using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading;

namespace InuR132.Display.Services;

/// <summary>One detected box, in the pixel coordinates of the frame it was found in.</summary>
public sealed record Detection(
    string Model, string Label, int ClassId, double Conf,
    double X1, double Y1, double X2, double Y2);

/// <summary>What the detector most recently produced.</summary>
public sealed record DetectionSnapshot(
    IReadOnlyList<Detection> Detections, int Width, int Height, double Millis, string? Error)
{
    public static readonly DetectionSnapshot Empty =
        new(Array.Empty<Detection>(), 0, 0, 0, null);
}

/// <summary>A model the sidecar loaded.</summary>
public sealed record DetectorModel(string Name, string Path, IReadOnlyList<KeyValuePair<int, string>> Classes);

/// <summary>
/// Runs the YOLO detector for the GUI.
///
/// A `.pt` is a Python pickle, so this process cannot load one either: it starts
/// the same sidecar the CLI uses (`inu_yolo_detector.py`, which has torch and
/// ultralytics) and talks to it over pipes — one length-prefixed JPEG per frame
/// in, one length-prefixed JSON message with the boxes out.
///
/// Inference runs on its own thread with latest-frame semantics, exactly like
/// the Rust side: a frame that arrives while the detector is busy replaces the
/// one waiting, so a slow model drops frames instead of stalling the preview.
/// </summary>
public sealed class DetectorClient : IDisposable
{
    private readonly object _lock = new();
    private readonly List<string> _stderrTail = new();
    private readonly Process _process;
    private readonly Stream _input;
    private readonly Stream _output;
    private readonly Thread _worker;

    private byte[]? _pending;
    private DetectionSnapshot _snapshot = DetectionSnapshot.Empty;
    private List<DetectorModel> _models = new();
    private bool _ready;
    private bool _stop;
    private long _submitted;
    private long _completed;
    private long _dropped;
    private long _version;

    private DetectorClient(Process process, Stream input, Stream output, Stream error)
    {
        _process = process;
        _input = input;
        _output = output;

        var errorThread = new Thread(() => ReadStderr(error))
        {
            IsBackground = true,
            Name = "inu-r132-detector-stderr",
        };
        errorThread.Start();

        _worker = new Thread(WorkerLoop) { IsBackground = true, Name = "inu-r132-detector" };
        _worker.Start();
    }

    /// <summary>True once the sidecar has reported that its models loaded.</summary>
    public bool IsReady { get { lock (_lock) { return _ready; } } }

    /// <summary>Changes every time a new answer arrives, so the UI can skip redrawing.</summary>
    public long Version { get { lock (_lock) { return _version; } } }

    public IReadOnlyList<DetectorModel> Models { get { lock (_lock) { return _models; } } }

    public DetectionSnapshot Snapshot { get { lock (_lock) { return _snapshot; } } }

    /// <summary>submitted / completed / dropped frames.</summary>
    public (long Submitted, long Completed, long Dropped) Counters
    {
        get { lock (_lock) { return (_submitted, _completed, _dropped); } }
    }

    public string StderrTail { get { lock (_lock) { return string.Join("\n", _stderrTail); } } }

    /// <summary>
    /// Start the sidecar. Returns immediately; readiness is reported through
    /// <see cref="IsReady"/> once the models are loaded, so the window never
    /// blocks on a model load.
    /// </summary>
    public static DetectorClient Start(string python, string script, IEnumerable<string> models,
        double conf, int imgsz)
    {
        var info = new ProcessStartInfo(python)
        {
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        info.ArgumentList.Add(script);
        info.ArgumentList.Add("--conf");
        info.ArgumentList.Add(conf.ToString("0.###", System.Globalization.CultureInfo.InvariantCulture));
        info.ArgumentList.Add("--imgsz");
        info.ArgumentList.Add(imgsz.ToString());
        foreach (var model in models)
        {
            info.ArgumentList.Add("--model");
            info.ArgumentList.Add(model);
        }

        var process = Process.Start(info) ?? throw new InvalidOperationException(
            $"启动检测进程失败: {python}");
        return new DetectorClient(process, process.StandardInput.BaseStream,
            process.StandardOutput.BaseStream, process.StandardError.BaseStream);
    }

    /// <summary>Hand the newest frame to the detector. Never blocks.</summary>
    public void Submit(byte[] jpeg)
    {
        lock (_lock)
        {
            if (_stop)
            {
                return;
            }

            _submitted++;
            if (_pending is not null)
            {
                _dropped++;
            }

            _pending = jpeg;
            Monitor.PulseAll(_lock);
        }
    }

    private void ReadStderr(Stream error)
    {
        try
        {
            using var reader = new StreamReader(error);
            while (reader.ReadLine() is { } line)
            {
                lock (_lock)
                {
                    if (_stderrTail.Count == 12)
                    {
                        _stderrTail.RemoveAt(0);
                    }

                    _stderrTail.Add(line);
                }
            }
        }
        catch
        {
            // the process went away; nothing useful to add
        }
    }

    private void WorkerLoop()
    {
        // The sidecar says whether the models loaded before it reads any frame.
        var first = ReadMessage(_output);
        if (first is null)
        {
            Fail("检测进程在就绪之前就退出了");
            return;
        }

        try
        {
            using var document = JsonDocument.Parse(first);
            var root = document.RootElement;
            if (root.TryGetProperty("ready", out var ready) && ready.GetBoolean())
            {
                var models = new List<DetectorModel>();
                if (root.TryGetProperty("models", out var list) && list.ValueKind == JsonValueKind.Array)
                {
                    foreach (var entry in list.EnumerateArray())
                    {
                        var classes = new List<KeyValuePair<int, string>>();
                        if (entry.TryGetProperty("classes", out var map) && map.ValueKind == JsonValueKind.Object)
                        {
                            foreach (var property in map.EnumerateObject())
                            {
                                if (int.TryParse(property.Name, out var id))
                                {
                                    classes.Add(new KeyValuePair<int, string>(id, property.Value.GetString() ?? "?"));
                                }
                            }
                            classes.Sort((a, b) => a.Key.CompareTo(b.Key));
                        }

                        models.Add(new DetectorModel(
                            entry.TryGetProperty("name", out var name) ? name.GetString() ?? "model" : "model",
                            entry.TryGetProperty("path", out var path) ? path.GetString() ?? string.Empty : string.Empty,
                            classes));
                    }
                }

                lock (_lock)
                {
                    _models = models;
                    _ready = true;
                }
            }
            else
            {
                Fail(root.TryGetProperty("error", out var error)
                    ? error.GetString() ?? "检测进程无法加载模型"
                    : "检测进程没有报告就绪");
                return;
            }
        }
        catch (Exception e)
        {
            Fail($"检测进程的应答无法解析: {e.Message}");
            return;
        }

        while (true)
        {
            byte[]? frame;
            lock (_lock)
            {
                while (_pending is null && !_stop)
                {
                    Monitor.Wait(_lock, 200);
                }

                if (_stop)
                {
                    return;
                }

                frame = _pending;
                _pending = null;
            }

            if (frame is null)
            {
                continue;
            }

            try
            {
                WriteMessage(_input, frame);
                var answer = ReadMessage(_output);
                if (answer is null)
                {
                    Fail("检测进程关闭了输出");
                    return;
                }

                var snapshot = ParseSnapshot(answer);
                lock (_lock)
                {
                    _snapshot = snapshot;
                    _completed++;
                    _version++;
                }
            }
            catch (Exception e)
            {
                Fail($"检测进程通信失败: {e.Message}");
                return;
            }
        }
    }

    private void Fail(string message)
    {
        lock (_lock)
        {
            var detail = _stderrTail.Count == 0 ? string.Empty : "\n" + string.Join("\n", _stderrTail);
            _snapshot = new DetectionSnapshot(Array.Empty<Detection>(), 0, 0, 0, message + detail);
            _stop = true;
            Monitor.PulseAll(_lock);
        }
    }

    private static DetectionSnapshot ParseSnapshot(byte[] json)
    {
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;

        if (root.TryGetProperty("error", out var error))
        {
            return new DetectionSnapshot(Array.Empty<Detection>(), 0, 0, 0, error.GetString());
        }

        var detections = new List<Detection>();
        if (root.TryGetProperty("detections", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var entry in list.EnumerateArray())
            {
                if (!TryNumber(entry, "x1", out var x1) || !TryNumber(entry, "y1", out var y1)
                    || !TryNumber(entry, "x2", out var x2) || !TryNumber(entry, "y2", out var y2))
                {
                    continue;
                }

                detections.Add(new Detection(
                    GetString(entry, "model"),
                    GetString(entry, "class") is { Length: > 0 } label ? label : "?",
                    entry.TryGetProperty("class_id", out var id) && id.TryGetInt32(out var classId) ? classId : -1,
                    TryNumber(entry, "conf", out var conf) ? conf : 0,
                    x1, y1, x2, y2));
            }
        }

        return new DetectionSnapshot(
            detections,
            root.TryGetProperty("width", out var width) ? width.GetInt32() : 0,
            root.TryGetProperty("height", out var height) ? height.GetInt32() : 0,
            TryNumber(root, "ms", out var millis) ? millis : 0,
            null);
    }

    private static string GetString(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) ? value.GetString() ?? string.Empty : string.Empty;

    private static bool TryNumber(JsonElement element, string name, out double value)
    {
        value = 0;
        if (!element.TryGetProperty(name, out var property))
        {
            return false;
        }

        return property.ValueKind switch
        {
            JsonValueKind.Number => property.TryGetDouble(out value),
            JsonValueKind.String => double.TryParse(property.GetString(),
                System.Globalization.NumberStyles.Float,
                System.Globalization.CultureInfo.InvariantCulture, out value),
            _ => false,
        };
    }

    /// <summary>One message: <c>u32 big endian length | payload</c>.</summary>
    private static byte[]? ReadMessage(Stream stream)
    {
        var header = new byte[4];
        if (!ReadExactly(stream, header, 4))
        {
            return null;
        }

        var length = (uint)((header[0] << 24) | (header[1] << 16) | (header[2] << 8) | header[3]);
        if (length == 0 || length > 64u * 1024 * 1024)
        {
            throw new InvalidDataException($"消息长度不合理: {length}");
        }

        var payload = new byte[length];
        if (!ReadExactly(stream, payload, (int)length))
        {
            return null;
        }

        return payload;
    }

    private static bool ReadExactly(Stream stream, byte[] buffer, int count)
    {
        var read = 0;
        while (read < count)
        {
            var chunk = stream.Read(buffer, read, count - read);
            if (chunk <= 0)
            {
                return false;
            }

            read += chunk;
        }

        return true;
    }

    private static void WriteMessage(Stream stream, byte[] payload)
    {
        var length = (uint)payload.Length;
        stream.Write(new[]
        {
            (byte)(length >> 24), (byte)(length >> 16), (byte)(length >> 8), (byte)length,
        });
        stream.Write(payload);
        stream.Flush();
    }

    // --------------------------------------------------------- discovery ----

    /// <summary>`*.pt` files in <paramref name="directory"/>, sorted.</summary>
    public static List<string> ScanModels(string directory)
    {
        try
        {
            if (!Directory.Exists(directory))
            {
                return new List<string>();
            }

            return Directory.EnumerateFiles(directory)
                .Where(path => string.Equals(Path.GetExtension(path), ".pt", StringComparison.OrdinalIgnoreCase))
                .OrderBy(path => path, StringComparer.Ordinal)
                .ToList();
        }
        catch
        {
            return new List<string>();
        }
    }

    /// <summary>The sidecar script: $INU_R132_DETECTOR, next to the app, under a
    /// yolo-cube-detect/ above it, or in the working directory.</summary>
    public static string? FindScript()
    {
        var fromEnv = Environment.GetEnvironmentVariable("INU_R132_DETECTOR");
        if (!string.IsNullOrWhiteSpace(fromEnv) && File.Exists(fromEnv))
        {
            return fromEnv;
        }

        const string name = "inu_yolo_detector.py";
        foreach (var directory in SearchDirectories())
        {
            var direct = Path.Combine(directory, name);
            if (File.Exists(direct))
            {
                return direct;
            }

            var nested = Path.Combine(directory, "yolo-cube-detect", name);
            if (File.Exists(nested))
            {
                return nested;
            }
        }

        return null;
    }

    /// <summary>Where discovery diagnostics go; the window points this at its log.</summary>
    public static Action<string>? Log { get; set; }

    /// <summary>The Python that runs the sidecar: $INU_R132_PYTHON, a .venv next
    /// to the app or in the working directory, then python3/python on PATH.
    ///
    /// A Python without ultralytics starts the sidecar perfectly happily and then
    /// dies on `import ultralytics`, which is a confusing way to fail, so each
    /// candidate is asked first and the first one that can import it wins.</summary>
    public static string? FindPython()
    {
        var fromEnv = Environment.GetEnvironmentVariable("INU_R132_PYTHON");
        if (!string.IsNullOrWhiteSpace(fromEnv))
        {
            if (File.Exists(fromEnv))
            {
                return fromEnv;
            }

            Log?.Invoke($"$INU_R132_PYTHON 指向的 {fromEnv} 不存在，继续找别的 Python");
        }

        // The same places the sidecar is looked for, so a `.venv` next to the
        // script's repository is found no matter which directory the app was
        // started from.
        var candidates = new List<string>();
        foreach (var directory in SearchDirectories())
        {
            candidates.Add(Path.Combine(directory, ".venv", "bin", "python"));
            candidates.Add(Path.Combine(directory, ".venv", "Scripts", "python.exe"));
        }

        foreach (var name in new[] { "python3", "python" })
        {
            candidates.AddRange(FindOnPath(name));
        }

        var existing = candidates.Where(File.Exists).Distinct().ToList();
        foreach (var candidate in existing)
        {
            if (CanImportUltralytics(candidate))
            {
                return candidate;
            }
        }

        if (existing.Count > 0)
        {
            Log?.Invoke("这些 Python 都 import 不了 ultralytics: " + string.Join(", ", existing));
        }

        return null;
    }

    /// <summary>Ask a Python whether ultralytics is importable, with a ceiling so a
    /// wedged interpreter cannot hang the start-up.</summary>
    private static bool CanImportUltralytics(string python)
    {
        try
        {
            var info = new ProcessStartInfo(python)
            {
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            info.ArgumentList.Add("-c");
            info.ArgumentList.Add("import ultralytics");

            using var process = Process.Start(info);
            if (process is null)
            {
                return false;
            }

            // Drain both pipes while waiting; leaving them unread is how this
            // kind of probe deadlocks.
            var stdout = process.StandardOutput.ReadToEndAsync();
            var stderr = process.StandardError.ReadToEndAsync();
            if (!process.WaitForExit(30000))
            {
                try
                {
                    process.Kill(entireProcessTree: true);
                }
                catch
                {
                    // already gone
                }

                return false;
            }

            System.Threading.Tasks.Task.WaitAll(stdout, stderr);
            return process.ExitCode == 0;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>Next to the program, then the directories above it (the program
    /// lives in build/, so the repository root is a couple of levels up), then
    /// the working directory. Used for both the sidecar and the interpreter.</summary>
    private static IEnumerable<string> SearchDirectories()
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);
        var baseDirectory = AppContext.BaseDirectory;
        if (seen.Add(baseDirectory))
        {
            yield return baseDirectory;
        }

        var info = new DirectoryInfo(baseDirectory);
        for (var level = 0; level < 6 && info?.Parent is not null; level++)
        {
            info = info.Parent;
            if (seen.Add(info.FullName))
            {
                yield return info.FullName;
            }
        }

        var cwd = Directory.GetCurrentDirectory();
        if (seen.Add(cwd))
        {
            yield return cwd;
        }
    }

    private static IEnumerable<string> FindOnPath(string name)
    {
        var path = Environment.GetEnvironmentVariable("PATH");
        if (string.IsNullOrEmpty(path))
        {
            yield break;
        }

        foreach (var directory in path.Split(Path.PathSeparator))
        {
            if (string.IsNullOrWhiteSpace(directory))
            {
                continue;
            }

            string candidate;
            try
            {
                candidate = Path.Combine(directory, name);
            }
            catch
            {
                continue;
            }

            if (File.Exists(candidate))
            {
                yield return candidate;
            }
        }
    }

    public void Dispose()
    {
        lock (_lock)
        {
            _stop = true;
            _pending = null;
            Monitor.PulseAll(_lock);
        }

        try
        {
            _worker.Join(1500);
        }
        catch
        {
            // ignore
        }

        try
        {
            if (!_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
                _process.WaitForExit(3000);
            }
        }
        catch
        {
            // already gone
        }
        finally
        {
            _process.Dispose();
        }
    }
}
