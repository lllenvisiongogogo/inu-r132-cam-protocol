using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Avalonia.Media.Imaging;

namespace InuR132.Display.Services;

/// <summary>A frame converted off the UI thread and ready to present.</summary>
public abstract record PresentFrame;

/// <summary>
/// A decoded JPEG plus the bytes it came from, so the photo button can save
/// exactly the frame that is on screen without re-encoding it.
/// </summary>
public sealed record JpegFrame(Bitmap Bitmap, byte[] Jpeg) : PresentFrame;

/// <summary>
/// BGRA8888 pixels plus the statistics of the frame they came from. `Raw` keeps
/// the original little endian Z16 bytes so the UI can sample real millimetres.
/// </summary>
public sealed record DepthFrame(byte[] Bgra, byte[] Raw, int Width, int Height,
    Z16Summary Summary, double Near, double Far, double Gamma) : PresentFrame;

/// <summary>
/// Decouples frame arrival from UI updates. Frames arrive on the socket reader
/// thread; only the newest of each stream is kept, so a slow UI cannot build a
/// backlog and drift behind real time. A worker thread does the JPEG decode and
/// the per pixel depth conversion; the UI only copies the finished buffers.
///
/// `mix` delivers both streams on one connection, so rgb and depth have their
/// own pending slot and their own ready frame. Depth uses two buffers: the
/// worker always fills the one that is not current, and both the fill and the
/// UI copy hold the same lock.
/// </summary>
public sealed class FramePipeline : IDisposable
{
    private readonly DepthSettings _settings;
    private readonly object _lock = new();
    private readonly object _depthLock = new();
    private readonly SemaphoreSlim _signal = new(0, 1);
    private readonly Thread _worker;
    private readonly byte[]?[] _buffers = new byte[]?[2];
    private readonly FrameMessage?[] _pending = new FrameMessage?[2];

    private PresentFrame? _readyRgb;
    private PresentFrame? _readyDepth;
    private byte[]? _readyBuffer;
    private long _rgbVersion;
    private long _depthVersion;
    private volatile bool _stopped;
    private long _dropped;

    public FramePipeline(DepthSettings settings)
    {
        _settings = settings;
        _worker = new Thread(WorkerLoop)
        {
            IsBackground = true,
            Name = "inu-r132-convert",
        };
        _worker.Start();
    }

    /// <summary>Frames that were replaced before the converter picked them up.</summary>
    public long Dropped => Interlocked.Read(ref _dropped);

    /// <summary>Called from the socket reader thread; never blocks.</summary>
    public void Submit(FrameMessage frame)
    {
        var slot = frame.Codec == FrameClient.CodecJpeg ? 0 : 1;
        lock (_lock)
        {
            if (_pending[slot] is not null)
            {
                _dropped++;
            }

            _pending[slot] = frame;
        }

        if (_signal.CurrentCount == 0)
        {
            try
            {
                _signal.Release();
            }
            catch (SemaphoreFullException)
            {
                // another release is already pending
            }
        }
    }

    /// <summary>Latest converted rgb frame, with a version that changes per frame.</summary>
    public PresentFrame? TakeRgb(out long version)
    {
        lock (_lock)
        {
            version = _rgbVersion;
            return _readyRgb;
        }
    }

    /// <summary>Latest converted depth frame, with a version that changes per frame.</summary>
    public PresentFrame? TakeDepth(out long version)
    {
        lock (_lock)
        {
            version = _depthVersion;
            return _readyDepth;
        }
    }

    /// <summary>Copy a depth frame into the locked writeable bitmap surface.</summary>
    public void CopyDepth(DepthFrame frame, IntPtr destination, int rowBytes)
    {
        var widthBytes = frame.Width * 4;
        lock (_depthLock)
        {
            var source = frame.Bgra;
            for (var y = 0; y < frame.Height; y++)
            {
                Marshal.Copy(source, y * widthBytes, IntPtr.Add(destination, y * rowBytes), widthBytes);
            }
        }
    }

    private void WorkerLoop()
    {
        while (!_stopped)
        {
            _signal.Wait(200);

            FrameMessage?[] frames;
            lock (_lock)
            {
                frames = [(FrameMessage?)_pending[0], (FrameMessage?)_pending[1]];
                _pending[0] = null;
                _pending[1] = null;
            }

            for (var slot = 0; slot < frames.Length; slot++)
            {
                var frame = frames[slot];
                if (frame is null)
                {
                    continue;
                }

                var present = Convert(frame);
                if (present is null)
                {
                    continue;
                }

                lock (_lock)
                {
                    if (present is JpegFrame)
                    {
                        _readyRgb = present;
                        _rgbVersion++;
                    }
                    else
                    {
                        _readyDepth = present;
                        _depthVersion++;
                    }
                }
            }
        }
    }

    private PresentFrame? Convert(FrameMessage frame)
    {
        try
        {
            if (frame.Codec == FrameClient.CodecJpeg)
            {
                using var memory = new MemoryStream(frame.Payload, writable: false);
                return new JpegFrame(new Bitmap(memory), frame.Payload);
            }

            if (frame.Codec != FrameClient.CodecZ16 || frame.Width <= 0 || frame.Height <= 0)
            {
                return null;
            }

            var width = frame.Width;
            var height = frame.Height;
            var needed = width * height * 4;

            // Fill the buffer that is not the one the UI may be reading.
            var slot = ReferenceEquals(_readyBuffer, _buffers[0]) ? 1 : 0;
            if (_buffers[slot] is null || _buffers[slot]!.Length != needed)
            {
                _buffers[slot] = new byte[needed];
            }

            var buffer = _buffers[slot]!;
            var summary = Z16.Summarize(frame.Payload);
            var (near, far, gamma, auto) = _settings.Snapshot();
            if (auto)
            {
                (near, far) = _settings.ApplyAuto(summary.P2, summary.P98);
            }

            var payload = frame.Payload;
            lock (_depthLock)
            {
                var index = 0;
                for (var y = 0; y < height; y++)
                {
                    var sourceRow = y * width * 2;
                    for (var x = 0; x < width; x++)
                    {
                        var offset = sourceRow + x * 2;
                        var value = (ushort)(payload[offset] | (payload[offset + 1] << 8));
                        var level = Z16.Level(value, near, far, gamma);
                        buffer[index++] = level;
                        buffer[index++] = level;
                        buffer[index++] = level;
                        // Invalid pixels stay transparent so `mix` shows the rgb
                        // underneath instead of black holes.
                        buffer[index++] = value == 0 ? (byte)0 : (byte)255;
                    }
                }
            }

            _readyBuffer = buffer;
            return new DepthFrame(buffer, payload, width, height, summary, near, far, gamma);
        }
        catch
        {
            return null;
        }
    }

    public void Dispose()
    {
        _stopped = true;
        try
        {
            _signal.Release();
        }
        catch
        {
            // ignore
        }

        _worker.Join(500);
        _signal.Dispose();
    }
}
