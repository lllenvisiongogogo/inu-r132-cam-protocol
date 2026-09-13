using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;

namespace InuR132.Display.Services;

/// <summary>Runs the `inu-r132` CLI (serve) as a child process.</summary>
public sealed class CliRunner : IDisposable
{
    private Process? _process;

    /// <summary>Every stdout/stderr line the child prints.</summary>
    public event Action<string>? Output;

    public bool IsRunning => _process is { HasExited: false };

    /// <summary>
    /// Locate the inu-r132 binary: $INU_R132_BIN, next to this app, or in the
    /// build/ and target/ directories of the repository above us.
    /// </summary>
    public static string? FindBinary()
    {
        var name = OperatingSystem.IsWindows() ? "inu-r132.exe" : "inu-r132";
        var candidates = new List<string>();

        var fromEnv = Environment.GetEnvironmentVariable("INU_R132_BIN");
        if (!string.IsNullOrWhiteSpace(fromEnv))
        {
            candidates.Add(fromEnv);
        }

        var baseDir = AppContext.BaseDirectory;
        candidates.Add(Path.Combine(baseDir, name));

        var dir = new DirectoryInfo(baseDir);
        while (dir is not null)
        {
            candidates.Add(Path.Combine(dir.FullName, "build", name));
            candidates.Add(Path.Combine(dir.FullName, "target", "release", name));
            candidates.Add(Path.Combine(dir.FullName, "target", "debug", name));
            if (File.Exists(Path.Combine(dir.FullName, "Cargo.toml")))
            {
                break;
            }

            dir = dir.Parent;
        }

        return candidates.FirstOrDefault(File.Exists);
    }

    public void StartServe(int port, string stream, bool registered, string? binary)
    {
        Stop();

        var path = binary ?? FindBinary() ?? throw new FileNotFoundException(
            "找不到 inu-r132 可执行文件：先在项目根目录跑 `make build`，或设置 INU_R132_BIN。");

        var info = new ProcessStartInfo(path)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        info.ArgumentList.Add("serve");
        info.ArgumentList.Add("--port");
        info.ArgumentList.Add(port.ToString());
        info.ArgumentList.Add("--stream");
        info.ArgumentList.Add(stream);
        if (!registered)
        {
            info.ArgumentList.Add("--no-registered");
        }

        var process = new Process { StartInfo = info, EnableRaisingEvents = true };
        process.OutputDataReceived += (_, e) =>
        {
            if (e.Data is not null)
            {
                Output?.Invoke(e.Data);
            }
        };
        process.ErrorDataReceived += (_, e) =>
        {
            if (e.Data is not null)
            {
                Output?.Invoke(e.Data);
            }
        };
        process.Start();
        process.BeginOutputReadLine();
        process.BeginErrorReadLine();
        _process = process;
        Output?.Invoke($"$ {path} serve --port {port} --stream {stream}" +
                       (registered ? string.Empty : " --no-registered"));
    }

    public void Stop()
    {
        var process = _process;
        _process = null;
        if (process is null)
        {
            return;
        }

        try
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                process.WaitForExit(3000);
            }
        }
        catch
        {
            // already gone
        }
        finally
        {
            process.Dispose();
        }
    }

    public void Dispose() => Stop();
}
