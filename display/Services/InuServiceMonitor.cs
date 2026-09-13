using System;
using System.Diagnostics;

namespace InuR132.Display.Services;

/// <summary>
/// Detects Inuitive's InuService daemon. It owns the USB device, so nothing
/// can open the camera until it runs. InuService daemonizes itself, so a
/// successful start keeps it around until it is killed.
/// </summary>
public static class InuServiceMonitor
{
    public const string MissingMessage =
        "未检测到 InuService 进程，NU4000 无法访问，因此不能进入主窗口。";

    public static bool IsRunning
    {
        get
        {
            try
            {
                if (OperatingSystem.IsWindows())
                {
                    using var process = Process.Start(new ProcessStartInfo("tasklist")
                    {
                        Arguments = "/FI \"IMAGENAME eq InuService.exe\" /NH",
                        RedirectStandardOutput = true,
                        UseShellExecute = false,
                        CreateNoWindow = true,
                    });
                    if (process is null)
                    {
                        return false;
                    }

                    var text = process.StandardOutput.ReadToEnd();
                    process.WaitForExit(3000);
                    return text.Contains("InuService.exe", StringComparison.OrdinalIgnoreCase);
                }

                using var pgrep = Process.Start(new ProcessStartInfo("pgrep")
                {
                    Arguments = "-x InuService",
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    UseShellExecute = false,
                    CreateNoWindow = true,
                });
                if (pgrep is null)
                {
                    return false;
                }

                pgrep.WaitForExit(3000);
                return pgrep.HasExited && pgrep.ExitCode == 0;
            }
            catch
            {
                return false;
            }
        }
    }
}
