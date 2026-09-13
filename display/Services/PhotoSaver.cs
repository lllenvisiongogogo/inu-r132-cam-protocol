using System;
using System.Globalization;
using System.IO;

namespace InuR132.Display.Services;

/// <summary>
/// Writes photos into `shot/` next to the program, named after the capture time
/// (`2026-02-11_19-36-09.248.jpg`), exactly like `inu-r132 shot` does.
///
/// The app does not shell out to that command: it already receives the camera
/// frames over the protocol, and a second process opening the device would fight
/// the `serve` that is feeding the preview for it.
/// </summary>
public static class PhotoSaver
{
    /// <summary>`shot/` next to the running program.</summary>
    public static string DefaultDirectory()
    {
        var baseDirectory = AppContext.BaseDirectory;
        if (string.IsNullOrWhiteSpace(baseDirectory))
        {
            baseDirectory = Directory.GetCurrentDirectory();
        }

        return Path.Combine(baseDirectory, "shot");
    }

    /// <summary>
    /// A path in <paramref name="directory"/> that does not exist yet. The name
    /// is the capture time in local time, which sorts the way it reads; a
    /// counter is appended if the same millisecond comes around twice.
    /// </summary>
    public static string UniquePath(string directory, DateTime taken)
    {
        var stamp = taken.ToString("yyyy-MM-dd_HH-mm-ss.fff", CultureInfo.InvariantCulture);

        var candidate = Path.Combine(directory, stamp + ".jpg");
        if (!File.Exists(candidate))
        {
            return candidate;
        }

        for (var counter = 2; counter < 1000; counter++)
        {
            candidate = Path.Combine(directory, $"{stamp}_{counter}.jpg");
            if (!File.Exists(candidate))
            {
                return candidate;
            }
        }

        throw new IOException($"{stamp} 在 {directory} 下已经有 1000 张照片了");
    }

    /// <summary>Write one JPEG, creating the directory when needed.</summary>
    public static string Save(byte[] jpeg, string directory)
    {
        Directory.CreateDirectory(directory);
        var path = UniquePath(directory, DateTime.Now);
        File.WriteAllBytes(path, jpeg);
        return path;
    }
}
