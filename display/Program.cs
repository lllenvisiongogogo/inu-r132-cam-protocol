using System;
using System.Linq;
using Avalonia;

namespace InuR132.Display;

internal static class Program
{
    /// <summary>
    /// `--label` opens the hand-labelling window instead of the camera viewer.
    /// It is set before Avalonia starts so <see cref="App"/> can branch, and it
    /// needs no InuService: labelling is offline work.
    /// </summary>
    public static bool LabelMode { get; private set; }

    [STAThread]
    public static void Main(string[] args)
    {
        LabelMode = args.Any(argument => argument is "--label" or "-l");
        BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
    }

    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .WithInterFont()
            .LogToTrace();
}
