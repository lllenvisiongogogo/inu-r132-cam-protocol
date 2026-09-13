using System;
using System.Diagnostics;
using System.IO;

namespace InuR132.Display.Services;

/// <summary>
/// Runs `train/scripts/pipeline.sh` - the same thing `make yolo-train` runs -
/// and streams its output. Assembles the dataset, trains, then publishes the
/// best weights into `models/`, which is where the camera looks for them.
/// </summary>
public sealed class TrainingRunner : IDisposable
{
    private Process? _process;

    public event Action<string>? Output;
    public event Action<int>? Finished;

    public bool IsRunning => _process is { HasExited: false };

    public void Start()
    {
        if (IsRunning)
        {
            return;
        }

        var script = LabelStore.PipelineScript;
        if (!File.Exists(script))
        {
            throw new FileNotFoundException($"找不到训练脚本 {script}");
        }

        var info = new ProcessStartInfo("bash")
        {
            WorkingDirectory = LabelStore.RepoRoot,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        info.ArgumentList.Add(script);

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
        process.Exited += (_, _) =>
        {
            try
            {
                Finished?.Invoke(process.ExitCode);
            }
            catch
            {
                // the window is gone
            }
        };

        process.Start();
        process.BeginOutputReadLine();
        process.BeginErrorReadLine();
        _process = process;
    }

    public void Dispose()
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
}
