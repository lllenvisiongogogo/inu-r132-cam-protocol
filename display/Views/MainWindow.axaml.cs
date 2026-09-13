using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using Avalonia.Platform;
using Avalonia.Threading;
using InuR132.Display.Services;
using Ellipse = Avalonia.Controls.Shapes.Ellipse;
using Rectangle = Avalonia.Controls.Shapes.Rectangle;

namespace InuR132.Display.Views;

public partial class MainWindow : Window
{
    private readonly CliRunner _runner = new();
    private readonly FrameClient _client = new();
    private readonly DepthSettings _settings = new();
    private readonly FramePipeline _pipeline;
    private readonly DispatcherTimer _present;
    private readonly StringBuilder _log = new();
    private readonly Stopwatch _presentClock = Stopwatch.StartNew();

    private WriteableBitmap? _depthBitmap;
    private Bitmap? _jpegCurrent;
    private Bitmap? _jpegRetired;
    private long _rgbVersion;
    private long _depthVersion;
    private long _presentedFrames;
    private double _presentFps;
    private int _logLines;
    /// <summary>What the preview draws: rgb, depth or mix.</summary>
    private string _view = "rgb";
    /// <summary>Mode the server confirmed it is sending.</summary>
    private string? _subscription;
    private bool _subscribeRequested;
    private bool _rgbAvailable = true;
    private bool _depthAvailable;
    private bool _mixAvailable;
    /// <summary>Newest frames, kept so switching the view is instant and the probe always has depth.</summary>
    private JpegFrame? _lastRgb;
    private DepthFrame? _lastDepth;
    private string _stats = "-";
    /// <summary>Depth probe position in normalised depth image coordinates.</summary>
    private double? _probeU;
    private double? _probeV;
    /// <summary>Photos written by the 拍照 button since the app started.</summary>
    private int _shotsTaken;
    private bool _takingShots;
    /// <summary>The YOLO sidecar, started lazily the first time a colour frame arrives.</summary>
    private DetectorClient? _detector;
    private bool _detectorTried;
    /// <summary>A background discovery/start attempt is in flight.</summary>
    private bool _detectorStarting;
    /// <summary>Version of the detections currently drawn, and the size they were laid out for.</summary>
    private long _detectorVersion = -1;
    private Size _boxLayerSize;

    public MainWindow()
    {
        InitializeComponent();

        _pipeline = new FramePipeline(_settings);
        _present = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(15) };
        _present.Tick += (_, _) => Present();

        StartServeButton.Click += (_, _) => StartServe();
        StopServeButton.Click += (_, _) => StopServe();
        ConnectButton.Click += (_, _) => Connect();
        DisconnectButton.Click += (_, _) => Disconnect();
        StreamBox.SelectionChanged += (_, _) => OnStreamChanged();
        PreviewArea.PointerPressed += OnPreviewPressed;
        ShotButton.Click += async (_, _) => await TakeShotsAsync();
        OpenShotDirButton.Click += (_, _) => OpenShotDirectory();
        DetectBox.IsCheckedChanged += (_, _) => OnDetectToggled();
        ModelsDirBox.LostFocus += (_, _) => _detectorTried = false;
        Closed += (_, _) => Cleanup();

        _runner.Output += line => Dispatcher.UIThread.Post(() => Append(line));
        DetectorClient.Log = line => Dispatcher.UIThread.Post(() => Append(line));
        _client.TextReceived += text => Dispatcher.UIThread.Post(() => OnServerText(text));
        _client.FrameReceived += frame => _pipeline.Submit(frame);
        _client.Closed += reason => Dispatcher.UIThread.Post(() =>
        {
            Append($"连接结束: {reason}");
            StatusText.Text = $"连接结束: {reason}";
        });

        var binary = CliRunner.FindBinary();
        Append(binary is null
            ? "找不到 inu-r132：先在项目根目录 `make build`，或设置 INU_R132_BIN。"
            : $"inu-r132: {binary}");
        ShotDirBox.Text = PhotoSaver.DefaultDirectory();
        Append($"拍照目录: {ShotDirBox.Text}");
        StatusText.Text = binary is null
            ? "找不到 inu-r132 可执行文件"
            : "就绪（点击画面可显示该点距离）";
        _present.Start();
    }

    private int Port => (int)(PortBox.Value ?? 5534);
    private string CurrentStream => (StreamBox.SelectedItem as ComboBoxItem)?.Content?.ToString() ?? "rgb";
    private double Near => (double)(NearBox.Value ?? 300);
    private double Far => (double)(FarBox.Value ?? 5000);
    private double Gamma => (double)(GammaBox.Value ?? 1);
    private double DepthOpacity => (double)(OpacityBox.Value ?? 0.55m);

    private void StartServe()
    {
        try
        {
            // Open every stream once so rgb, depth and mix all work on connect.
            var registered = RegisteredBox.IsChecked == true;
            _runner.StartServe(Port, "all", registered, null);
            Append($"serve 已启动（stream=all, port={Port}, registered={registered}）");
        }
        catch (Exception e)
        {
            Append($"启动 serve 失败: {e.Message}");
        }
    }

    private void StopServe()
    {
        _runner.Stop();
        _client.Disconnect();
        Append("serve 已停止");
    }

    private void Connect()
    {
        _rgbVersion = 0;
        _depthVersion = 0;
        _view = CurrentStream;
        _subscription = null;
        _subscribeRequested = false;
        try
        {
            _client.Connect("127.0.0.1", Port);
            // The hello carries the status JSON; the subscription is chosen once
            // it arrives so the widest stream the server has is used.
            _client.Send("status");
            Append($"已连接 127.0.0.1:{Port}");
        }
        catch (Exception e)
        {
            Append($"连接失败: {e.Message}");
        }
    }

    private void Disconnect() => _client.Disconnect();

    private double Conf => (double)(ConfBox.Value ?? 0.4m);

    // ----------------------------------------------------- object detection ----

    private void OnDetectToggled()
    {
        if (DetectBox.IsChecked == true)
        {
            _detectorTried = false;
            DetectStatusText.Text = "检测: 启动中…";
            Append("目标检测已打开");
            return;
        }

        _detector?.Dispose();
        _detector = null;
        _detectorTried = false;
        _detectorStarting = false;
        _detectorVersion = -1;
        ClearBoxes();
        DetectStatusText.Text = "检测: 关闭";
        Append("目标检测已关闭");
    }

    /// <summary>
    /// Start the sidecar the first time it is needed — off the UI thread, because
    /// finding the interpreter means asking each candidate whether it can import
    /// ultralytics, which takes about a second and must not freeze the preview.
    /// </summary>
    private DetectorClient? EnsureDetector()
    {
        if (_detector is not null || _detectorTried || _detectorStarting || DetectBox.IsChecked != true)
        {
            return _detector;
        }

        var directory = ModelsDirBox.Text?.Trim();
        if (string.IsNullOrEmpty(directory))
        {
            directory = "models";
        }

        _detectorStarting = true;
        DetectStatusText.Text = "检测: 启动中…";
        var conf = Conf;
        _ = Task.Run(() => StartDetector(directory, conf));
        return null;
    }

    /// <summary>The background half of <see cref="EnsureDetector"/>.</summary>
    private void StartDetector(string directory, double conf)
    {
        DetectorClient? client = null;
        string status;
        string? log = null;
        try
        {
            var models = DetectorClient.ScanModels(directory);
            var script = models.Count == 0 ? null : DetectorClient.FindScript();
            var python = script is null ? null : DetectorClient.FindPython();

            if (models.Count == 0)
            {
                status = $"检测: {directory} 下没有 .pt";
            }
            else if (script is null)
            {
                status = "检测: 找不到 inu_yolo_detector.py";
            }
            else if (python is null)
            {
                status = "检测: 没有能 import ultralytics 的 Python";
            }
            else
            {
                client = DetectorClient.Start(python, script, models, conf, 640);
                status = "检测: 加载模型…";
                log = $"检测: {models.Count} 个模型 " +
                      $"[{string.Join(", ", models.Select(Path.GetFileName))}] via {python}";
            }
        }
        catch (Exception e)
        {
            status = $"检测: 启动失败 - {e.Message}";
        }

        var started = client;
        Dispatcher.UIThread.Post(() =>
        {
            _detectorStarting = false;
            _detector = started;
            // Only a successful start sticks; a failure is retried when the
            // checkbox or the model directory changes.
            _detectorTried = started is null;
            DetectStatusText.Text = status;
            Append(log ?? status);
        });
    }

    /// <summary>Refresh the boxes and the status line from the detector.</summary>
    private void UpdateDetections()
    {
        var detector = _detector;
        if (detector is null)
        {
            return;
        }

        var snapshot = detector.Snapshot;
        if (snapshot.Error is { } error)
        {
            var first = error.Split('\n')[0];
            DetectStatusText.Text = first.Contains("ultralytics", StringComparison.OrdinalIgnoreCase)
                ? "检测: 失败 - 这个 Python 没装 ultralytics（设 INU_R132_PYTHON 指过去）"
                : $"检测: 失败 - {first}";
            ClearBoxes();
            return;
        }

        var (submitted, completed, _) = detector.Counters;
        if (!detector.IsReady)
        {
            DetectStatusText.Text = "检测: 加载模型…";
            return;
        }

        // Its boxes were found on the colour image, so they only line up there.
        if (_view == "depth")
        {
            DetectStatusText.Text = "检测: 只在彩色视图显示";
            ClearBoxes();
            return;
        }

        // The distance comes from the centre of each box; without a depth
        // stream there is nothing to measure, so it is left out entirely rather
        // than reporting 无测量 for every box.
        var hasDepth = _lastDepth is not null;
        DetectStatusText.Text = snapshot.Detections.Count == 0
            ? $"检测: 无目标 ({snapshot.Millis:0} ms, {completed}/{submitted} 帧)"
            : $"检测: {string.Join(", ", snapshot.Detections.Select(d =>
                hasDepth
                    ? $"{d.Label} {d.Conf:0.00} {SampleBoxCentre(d, snapshot).Text}"
                    : $"{d.Label} {d.Conf:0.00}"))}" +
              $" ({snapshot.Millis:0} ms, {completed}/{submitted} 帧)";

        // Redraw when the answer changed, or when the preview was resized.
        var size = PreviewArea.Bounds.Size;
        if (detector.Version == _detectorVersion && size == _boxLayerSize)
        {
            return;
        }

        _detectorVersion = detector.Version;
        _boxLayerSize = size;
        DrawDetections(snapshot);
    }

    private void DrawDetections(DetectionSnapshot snapshot)
    {
        ClearBoxes();
        if (snapshot.Detections.Count == 0 || snapshot.Width <= 0 || snapshot.Height <= 0)
        {
            return;
        }

        var rect = ColourImageRect();
        if (rect.Width <= 0 || rect.Height <= 0)
        {
            return;
        }

        var hasDepth = _lastDepth is not null;
        foreach (var hit in snapshot.Detections)
        {
            var colour = ClassColour(hit.ClassId);
            var brush = new SolidColorBrush(colour);
            // Detections are in the detector's frame; the preview may be scaled.
            var x = rect.X + hit.X1 / snapshot.Width * rect.Width;
            var y = rect.Y + hit.Y1 / snapshot.Height * rect.Height;
            var width = Math.Max(1.0, (hit.X2 - hit.X1) / snapshot.Width * rect.Width);
            var height = Math.Max(1.0, (hit.Y2 - hit.Y1) / snapshot.Height * rect.Height);

            var box = new Rectangle
            {
                Width = width,
                Height = height,
                Stroke = brush,
                StrokeThickness = 2,
                Fill = null,
            };
            Canvas.SetLeft(box, x);
            Canvas.SetTop(box, y);
            BoxLayer.Children.Add(box);

            // Where the distance below is measured: the box centre.
            var centreX = x + width / 2;
            var centreY = y + height / 2;
            var distance = hasDepth ? SampleBoxCentre(hit, snapshot) : DepthSample.None;
            if (distance.Millimetres is not null)
            {
                var dot = new Ellipse { Width = 7, Height = 7, Fill = brush };
                Canvas.SetLeft(dot, centreX - 3.5);
                Canvas.SetTop(dot, centreY - 3.5);
                BoxLayer.Children.Add(dot);
            }

            var label = new Border
            {
                Background = new SolidColorBrush(Color.FromArgb(0xCC, 0, 0, 0)),
                CornerRadius = new CornerRadius(3),
                Padding = new Thickness(5, 1),
                Child = new TextBlock
                {
                    Text = hasDepth
                        ? $"{hit.Label} {hit.Conf:0.00}  {distance.Text}"
                        : $"{hit.Label} {hit.Conf:0.00}",
                    Foreground = brush,
                    FontSize = 13,
                },
            };
            Canvas.SetLeft(label, Math.Clamp(x, 0, Math.Max(0, rect.Right - 160)));
            Canvas.SetTop(label, y >= 22 ? y - 22 : y + 3);
            BoxLayer.Children.Add(label);
        }
    }

    private void ClearBoxes() => BoxLayer.Children.Clear();

    /// <summary>Where the colour image is actually drawn inside the preview area.</summary>
    private Rect ColourImageRect()
    {
        var size = PreviewArea.Bounds.Size;
        if (size.Width <= 0 || size.Height <= 0)
        {
            return new Rect(size);
        }

        // mix stretches both images over the whole area; a single stream view
        // keeps the aspect ratio, so the image can be letterboxed.
        if (_view == "mix" || _lastRgb is null)
        {
            return new Rect(size);
        }

        var imageAspect = (double)_lastRgb.Bitmap.PixelSize.Width / _lastRgb.Bitmap.PixelSize.Height;
        var boxAspect = size.Width / size.Height;
        if (boxAspect > imageAspect)
        {
            var width = size.Height * imageAspect;
            return new Rect((size.Width - width) / 2, 0, width, size.Height);
        }

        var height = size.Width / imageAspect;
        return new Rect(0, (size.Height - height) / 2, size.Width, height);
    }

    /// <summary>Box colours by class id, matching the CLI viewer's palette.</summary>
    private static Color ClassColour(int classId)
    {
        var palette = new[]
        {
            Color.FromRgb(0, 255, 0),     // lime
            Color.FromRgb(255, 170, 0),   // orange
            Color.FromRgb(0, 200, 255),   // cyan
            Color.FromRgb(255, 0, 255),   // magenta
            Color.FromRgb(255, 60, 60),   // red
            Color.FromRgb(255, 255, 0),   // yellow
        };
        return palette[((classId % palette.Length) + palette.Length) % palette.Length];
    }

    private int ShotCount => Math.Clamp((int)(ShotCountBox.Value ?? 1), 1, 999);
    private int ShotInterval => Math.Clamp((int)(ShotIntervalBox.Value ?? 500), 0, 60000);

    private string ShotDirectory
    {
        get
        {
            var text = ShotDirBox.Text?.Trim();
            return string.IsNullOrEmpty(text) ? PhotoSaver.DefaultDirectory() : text;
        }
    }

    /// <summary>
    /// Save the picture that is on screen as a timestamped JPEG, `count` times.
    /// The frames come from the subscription the preview already runs, so this
    /// never opens the camera a second time and works against a remote `serve`
    /// too. The first photo is the frame being displayed right now; each further
    /// photo waits for a frame the pipeline has not shown yet, so a burst is
    /// never the same picture twice.
    /// </summary>
    private async Task TakeShotsAsync()
    {
        if (_takingShots)
        {
            Append("上一轮拍照还没结束");
            return;
        }

        if (_lastRgb is null)
        {
            Append("还没有画面可拍：先「启动 serve」并「连接」");
            StatusText.Text = "还没有画面可拍";
            return;
        }

        var directory = ShotDirectory;
        var count = ShotCount;
        var interval = ShotInterval;

        // Report a bad directory before the first shutter instead of mid burst.
        try
        {
            Directory.CreateDirectory(directory);
        }
        catch (Exception e)
        {
            Append($"拍照目录不可用: {e.Message}");
            return;
        }

        _takingShots = true;
        ShotButton.IsEnabled = false;
        var saved = 0;
        // Version of the frame the last photo used, so a burst waits for a new one.
        var lastVersion = _rgbVersion;
        try
        {
            for (var index = 0; index < count; index++)
            {
                if (index > 0)
                {
                    if (interval > 0)
                    {
                        await Task.Delay(interval);
                    }

                    if (!await WaitForNextRgbAsync(lastVersion, 2000))
                    {
                        Append($"第 {index + 1} 张放弃：2 秒内没有新画面（断开了？）");
                        break;
                    }
                }

                var frame = _lastRgb;
                if (frame is null)
                {
                    break;
                }

                try
                {
                    var path = PhotoSaver.Save(frame.Jpeg, directory);
                    saved++;
                    lastVersion = _rgbVersion;
                    Append($"已保存 {path}");
                }
                catch (Exception e)
                {
                    Append($"保存失败: {e.Message}");
                    break;
                }
            }
        }
        finally
        {
            _takingShots = false;
            ShotButton.IsEnabled = true;
        }

        _shotsTaken += saved;
        ShotCountText.Text = $"已拍 {_shotsTaken} 张";
        StatusText.Text = saved > 0
            ? $"已保存 {saved} 张到 {directory}"
            : $"没有保存照片（已拍 {_shotsTaken} 张）";
    }

    /// <summary>Wait until the preview has taken delivery of a newer rgb frame.</summary>
    private async Task<bool> WaitForNextRgbAsync(long version, int timeoutMs)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        while (_rgbVersion == version)
        {
            if (Environment.TickCount64 >= deadline)
            {
                return false;
            }

            // Yields back to the UI thread, where Present() bumps the version.
            await Task.Delay(10);
        }

        return _lastRgb is not null;
    }

    /// <summary>Show the photo directory in the platform file manager.</summary>
    private void OpenShotDirectory()
    {
        var directory = ShotDirectory;
        try
        {
            Directory.CreateDirectory(directory);
            if (OperatingSystem.IsWindows())
            {
                Process.Start(new ProcessStartInfo(directory) { UseShellExecute = true });
                return;
            }

            var info = new ProcessStartInfo(OperatingSystem.IsMacOS() ? "open" : "xdg-open")
            {
                UseShellExecute = false,
            };
            info.ArgumentList.Add(directory);
            Process.Start(info);
        }
        catch (Exception e)
        {
            Append($"打开目录失败: {e.Message}");
        }
    }

    /// <summary>
    /// The app subscribes to the widest mode once, and the selector only changes
    /// what is drawn. That way the colour view keeps receiving depth, so the
    /// distance probe works there too.
    /// </summary>
    private bool IsViewAvailable(string view)
    {
        if (_subscription is null)
        {
            return false;
        }

        return _subscription == "mix" || _subscription == view;
    }

    private void OnStreamChanged()
    {
        _view = CurrentStream;
        if (_client.IsConnected && !IsViewAvailable(_view))
        {
            Append($"{_view} 视图不可用：当前只订阅了 {_subscription ?? "无"}（serve 需要 --stream all）");
        }
    }

    private void OnServerText(string text)
    {
        Append($"server: {text}");

        if (TryParseStatus(text))
        {
            if (!_subscribeRequested && _client.IsConnected)
            {
                var mode = _mixAvailable ? "mix" : (_rgbAvailable ? "rgb" : "depth");
                _client.Send($"subscribe 30 {mode}");
                _subscribeRequested = true;
                Append($"订阅 {mode}");
            }

            return;
        }

        const string subscribed = "ok subscribed stream=";
        const string switched = "ok stream=";
        if (text.StartsWith(subscribed, StringComparison.Ordinal))
        {
            _subscription = FirstWord(text[subscribed.Length..]);
        }
        else if (text.StartsWith(switched, StringComparison.Ordinal))
        {
            _subscription = FirstWord(text[switched.Length..]);
        }
    }

    /// <summary>Pick `mix_available` and per stream availability out of a reply.</summary>
    private bool TryParseStatus(string text)
    {
        var start = text.IndexOf('{');
        if (start < 0)
        {
            return false;
        }

        try
        {
            using var document = System.Text.Json.JsonDocument.Parse(text[start..]);
            var root = document.RootElement;
            if (root.TryGetProperty("mix_available", out var mix))
            {
                _mixAvailable = mix.GetBoolean();
            }

            if (root.TryGetProperty("streams", out var streams))
            {
                if (streams.TryGetProperty("rgb", out var rgb)
                    && rgb.TryGetProperty("available", out var rgbAvailable))
                {
                    _rgbAvailable = rgbAvailable.GetBoolean();
                }

                if (streams.TryGetProperty("depth", out var depth)
                    && depth.TryGetProperty("available", out var depthAvailable))
                {
                    _depthAvailable = depthAvailable.GetBoolean();
                }
            }

            return true;
        }
        catch
        {
            return false;
        }
    }

    private static string FirstWord(string text)
    {
        var end = text.IndexOf(' ');
        return end < 0 ? text : text[..end];
    }

    /// <summary>
    /// Refresh the preview at the display rate, always with the newest converted
    /// frame of each stream. Frames that arrived in between are dropped by the
    /// pipeline instead of queueing up, which is what keeps the view real time.
    /// Depth is consumed even in the colour view so the probe stays live.
    /// </summary>
    private void Present()
    {
        if (AutoRangeBox.IsChecked == true)
        {
            _settings.SetAuto(true);
        }
        else
        {
            _settings.SetManual(Near, Far, Gamma);
        }

        var showRgb = _view != "depth";
        var showDepth = _view != "rgb";
        var presented = false;

        var rgbFrame = _pipeline.TakeRgb(out var rgbVersion);
        if (rgbFrame is JpegFrame jpeg && rgbVersion != _rgbVersion)
        {
            _rgbVersion = rgbVersion;
            _lastRgb = jpeg;
            presented = true;
            // The detector only ever sees colour frames, and it gets the very
            // bytes serve sent rather than a re-encode.
            EnsureDetector()?.Submit(jpeg.Jpeg);
        }

        var depthFrame = _pipeline.TakeDepth(out var depthVersion);
        if (depthFrame is DepthFrame depth && depthVersion != _depthVersion)
        {
            _depthVersion = depthVersion;
            _lastDepth = depth;
            // Copy into the bitmap even in the colour view, so the depth view
            // switches instantly and the probe always has fresh pixels.
            ShowDepth(depth);
            presented = true;
        }

        ApplyLayout(showRgb, showDepth);

        if (_lastRgb is not null)
        {
            _stats = $"rgb {_lastRgb.Bitmap.PixelSize.Width}x{_lastRgb.Bitmap.PixelSize.Height}";
        }

        if (_lastDepth is not null)
        {
            _stats = $"depth {_lastDepth.Width}x{_lastDepth.Height}  " +
                     $"min/p50/max {_lastDepth.Summary.Min}/{_lastDepth.Summary.P50}/{_lastDepth.Summary.Max} mm, " +
                     $"invalid {_lastDepth.Summary.InvalidRatio * 100:0}%";
        }

        UpdateProbe();
        UpdateDetections();

        if (presented)
        {
            _presentedFrames++;
        }

        var seconds = _presentClock.Elapsed.TotalSeconds;
        if (seconds >= 0.5)
        {
            _presentFps = _presentedFrames / seconds;
            _presentedFrames = 0;
            _presentClock.Restart();
        }

        var (near, far, gamma, auto) = _settings.Snapshot();
        var availability =
            $"rgb={(_rgbAvailable ? "y" : "n")} depth={(_depthAvailable ? "y" : "n")} " +
            $"mix={(_mixAvailable ? "y" : "n")}";
        var warning = IsViewAvailable(_view)
            ? string.Empty
            : $"  [{_view} 视图不可用：订阅 {_subscription ?? "无"}，serve 需要 --stream all]";
        StatusText.Text =
            $"{_view}  sub={_subscription ?? "-"}  {availability}{warning}  " +
            $"present {_presentFps:0.0} fps, dropped {_pipeline.Dropped}  " +
            $"near..far {near:0}..{far:0} g{gamma:0.00} auto={(auto ? "on" : "off")}  " +
            $"alpha {DepthOpacity:0.00}  {_stats}";
    }

    /// <summary>
    /// rgb shows the colour image, depth the greyscale one, mix overlays depth on
    /// rgb. With registered depth the two are already in the same camera frame,
    /// so no manual offset is needed.
    /// </summary>
    private void ApplyLayout(bool showRgb, bool showDepth)
    {
        var mix = _view == "mix";
        RgbImage.IsVisible = showRgb;
        DepthImage.IsVisible = showDepth;
        var stretch = mix ? Stretch.Fill : Stretch.Uniform;
        RgbImage.Stretch = stretch;
        DepthImage.Stretch = stretch;
        DepthImage.Opacity = mix ? DepthOpacity : 1.0;

        if (showRgb && _lastRgb is not null && !ReferenceEquals(RgbImage.Source, _lastRgb.Bitmap))
        {
            SwapJpeg(_lastRgb);
        }

        if (showDepth && _depthBitmap is not null && !ReferenceEquals(DepthImage.Source, _depthBitmap))
        {
            DepthImage.Source = _depthBitmap;
        }
    }

    /// <summary>Drop the probe marker on the clicked point of the depth image.</summary>
    private void OnPreviewPressed(object? sender, PointerPressedEventArgs e)
    {
        var point = e.GetPosition(PreviewArea);
        var rect = DepthImageRect();
        if (rect.Width <= 0 || rect.Height <= 0)
        {
            return;
        }

        var u = (point.X - rect.X) / rect.Width;
        var v = (point.Y - rect.Y) / rect.Height;
        if (u < 0 || u > 1 || v < 0 || v > 1)
        {
            return;
        }

        _probeU = u;
        _probeV = v;
        UpdateProbe();
    }

    /// <summary>Where the depth image is drawn inside the preview area.</summary>
    private Rect DepthImageRect()
    {
        var size = PreviewArea.Bounds.Size;
        if (size.Width <= 0 || size.Height <= 0)
        {
            return new Rect(size);
        }

        // mix stretches both images to the whole area; the single stream views
        // keep the aspect ratio, so the image can be letterboxed. The probe is
        // placed in the colour image, which shares the area in every view.
        if (_view == "mix" || _lastRgb is not null)
        {
            return new Rect(size);
        }

        if (_lastDepth is null)
        {
            return new Rect(size);
        }

        var imageAspect = (double)_lastDepth.Width / _lastDepth.Height;
        var boxAspect = size.Width / size.Height;
        if (boxAspect > imageAspect)
        {
            var width = size.Height * imageAspect;
            return new Rect((size.Width - width) / 2, 0, width, size.Height);
        }

        var height = size.Width / imageAspect;
        return new Rect(0, (size.Height - height) / 2, size.Width, height);
    }

    /// <summary>Sample the depth under the probe and move the green marker.</summary>
    private void UpdateProbe()
    {
        if (_probeU is null || _probeV is null || _lastDepth is null)
        {
            ProbeDot.IsVisible = false;
            ProbeLabel.IsVisible = false;
            return;
        }

        var rect = DepthImageRect();
        var x = rect.X + _probeU.Value * rect.Width;
        var y = rect.Y + _probeV.Value * rect.Height;
        Canvas.SetLeft(ProbeDot, x - ProbeDot.Width / 2);
        Canvas.SetTop(ProbeDot, y - ProbeDot.Height / 2);
        ProbeDot.IsVisible = true;
        Canvas.SetLeft(ProbeLabel, x + ProbeDot.Width / 2 + 4);
        Canvas.SetTop(ProbeLabel, y - 12);
        ProbeLabel.IsVisible = true;

        var (millimetres, valid) = SampleDepth();
        ProbeText.Text = millimetres is { } mm
            ? valid < 9 ? $"{mm} mm ({valid}/9)" : $"{mm} mm"
            : "无测量 (0/9)";
    }

    /// <summary>
    /// Average the valid samples of a 3x3 window under the probe.
    /// </summary>
    private DepthSample SampleDepth() =>
        _probeU is null || _probeV is null || _lastDepth is null
            ? DepthSample.None
            : DepthSampler.At(_lastDepth.Raw, _lastDepth.Width, _lastDepth.Height,
                _probeU.Value, _probeV.Value);

    /// <summary>Distance at the centre of a detected box, or `None` when there is
    /// no depth there.</summary>
    private DepthSample SampleBoxCentre(Detection hit, DetectionSnapshot snapshot) =>
        _lastDepth is null
            ? DepthSample.None
            : DepthSampler.BoxCentre(_lastDepth.Raw, _lastDepth.Width, _lastDepth.Height,
                hit.X1, hit.Y1, hit.X2, hit.Y2, snapshot.Width, snapshot.Height);


    private void ShowJpeg(JpegFrame frame) => SwapJpeg(frame);

    private void SwapJpeg(JpegFrame frame)
    {
        if (ReferenceEquals(_jpegCurrent, frame.Bitmap))
        {
            return;
        }

        _jpegRetired?.Dispose();
        _jpegRetired = _jpegCurrent;
        _jpegCurrent = frame.Bitmap;
        RgbImage.Source = frame.Bitmap;
    }

    private void ShowDepth(DepthFrame frame)
    {
        if (_depthBitmap is null
            || _depthBitmap.PixelSize.Width != frame.Width
            || _depthBitmap.PixelSize.Height != frame.Height)
        {
            _depthBitmap?.Dispose();
            _depthBitmap = new WriteableBitmap(
                new PixelSize(frame.Width, frame.Height),
                new Vector(96, 96),
                PixelFormat.Bgra8888,
                // Unpremultiplied so invalid pixels can be fully transparent.
                AlphaFormat.Unpremul);
        }

        using (var framebuffer = _depthBitmap.Lock())
        {
            _pipeline.CopyDepth(frame, framebuffer.Address, framebuffer.RowBytes);
        }

        DepthImage.Source = _depthBitmap;
        // The source object is unchanged, so force a repaint.
        DepthImage.InvalidateVisual();
    }

    private void Append(string line)
    {
        _log.Append(line).Append('\n');
        _logLines++;
        if (_logLines > 500)
        {
            var text = _log.ToString();
            var cut = text.IndexOf('\n', text.Length / 3);
            _log.Clear();
            _log.Append(cut >= 0 ? text[(cut + 1)..] : text);
            _logLines = _log.ToString().Count(c => c == '\n');
        }

        LogBox.Text = _log.ToString();
        LogBox.CaretIndex = LogBox.Text?.Length ?? 0;
    }

    private void Cleanup()
    {
        _present.Stop();
        _detector?.Dispose();
        _client.Dispose();
        _runner.Dispose();
        _pipeline.Dispose();
        _jpegCurrent?.Dispose();
        _jpegRetired?.Dispose();
        _depthBitmap?.Dispose();
    }
}
