using System;
using System.Buffers.Binary;
using System.IO;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace InuR132.Display.Services;

/// <summary>One frame envelope from `inu-r132 serve`.</summary>
public sealed record FrameMessage(byte Codec, int Width, int Height, byte[] Payload);

/// <summary>
/// Consumes the inu-r132 protocol: reads length prefixed envelopes
/// (u32 length | u8 type | payload) and raises one event per message.
/// Client to server traffic is plain newline terminated UTF-8.
/// </summary>
public sealed class FrameClient : IDisposable
{
    public const byte TypeText = 0;
    public const byte TypeFrame = 1;
    public const byte TypeBye = 2;
    public const byte CodecJpeg = 1;
    public const byte CodecZ16 = 2;

    private TcpClient? _client;
    private NetworkStream? _stream;
    private CancellationTokenSource? _cts;

    public event Action<string>? TextReceived;
    public event Action<FrameMessage>? FrameReceived;
    public event Action<string>? Closed;

    public bool IsConnected => _client?.Connected == true;

    /// <summary>
    /// Open the connection. The caller sends `status` / `subscribe` itself, so it
    /// can react to a server that does not have the requested stream.
    /// </summary>
    public void Connect(string host, int port)
    {
        Disconnect();

        var client = new TcpClient { NoDelay = true };
        client.Connect(host, port);
        _client = client;
        _stream = client.GetStream();
        _cts = new CancellationTokenSource();

        _ = Task.Run(() => ReadLoop(_cts.Token));
    }

    public void Send(string line)
    {
        var stream = _stream;
        if (stream is null)
        {
            return;
        }

        var bytes = Encoding.UTF8.GetBytes(line + "\n");
        lock (stream)
        {
            stream.Write(bytes, 0, bytes.Length);
        }
    }

    private void ReadLoop(CancellationToken token)
    {
        var stream = _stream;
        if (stream is null)
        {
            return;
        }

        try
        {
            while (!token.IsCancellationRequested)
            {
                var (type, payload) = ReadEnvelope(stream);
                switch (type)
                {
                    case TypeText:
                        TextReceived?.Invoke(Encoding.UTF8.GetString(payload));
                        break;
                    case TypeFrame:
                        if (payload.Length < 10)
                        {
                            break;
                        }

                        var width = (int)BinaryPrimitives.ReadUInt32BigEndian(payload.AsSpan(0, 4));
                        var height = (int)BinaryPrimitives.ReadUInt32BigEndian(payload.AsSpan(4, 4));
                        var codec = payload[8];
                        var body = payload[10..];
                        FrameReceived?.Invoke(new FrameMessage(codec, width, height, body));
                        break;
                    case TypeBye:
                        Closed?.Invoke(Encoding.UTF8.GetString(payload));
                        return;
                }
            }
        }
        catch (Exception e)
        {
            if (!token.IsCancellationRequested)
            {
                Closed?.Invoke(e.Message);
            }
        }
    }

    private static (byte Type, byte[] Payload) ReadEnvelope(NetworkStream stream)
    {
        var header = ReadExactly(stream, 4);
        var length = (int)BinaryPrimitives.ReadUInt32BigEndian(header);
        if (length <= 0 || length > 128 * 1024 * 1024)
        {
            throw new InvalidDataException($"invalid envelope length {length}");
        }

        var body = ReadExactly(stream, length);
        var type = body[0];
        var payload = body[1..];
        return (type, payload);
    }

    private static byte[] ReadExactly(NetworkStream stream, int count)
    {
        var buffer = new byte[count];
        var offset = 0;
        while (offset < count)
        {
            var read = stream.Read(buffer, offset, count - offset);
            if (read <= 0)
            {
                throw new EndOfStreamException();
            }

            offset += read;
        }

        return buffer;
    }

    public void Disconnect()
    {
        try
        {
            _cts?.Cancel();
        }
        catch
        {
            // ignore
        }

        try
        {
            _stream?.Dispose();
        }
        catch
        {
            // ignore
        }

        try
        {
            _client?.Dispose();
        }
        catch
        {
            // ignore
        }

        _stream = null;
        _client = null;
        _cts = null;
    }

    public void Dispose() => Disconnect();
}
