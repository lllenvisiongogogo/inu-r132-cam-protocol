using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using InuR132.Display.Services;
using InuR132.Display.Views;

namespace InuR132.Display;

public partial class App : Application
{
    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            desktop.ShutdownMode = ShutdownMode.OnMainWindowClose;
            if (Program.LabelMode)
            {
                // Labelling is offline: no camera, so no InuService gate.
                desktop.MainWindow = new LabelWindow();
            }
            else
            {
                ShowGate(desktop);
            }
        }

        base.OnFrameworkInitializationCompleted();
    }

    /// <summary>
    /// The camera is only reachable through InuService, so the main window is
    /// only opened once the daemon is running. Otherwise an error window with a
    /// retry button is shown.
    /// </summary>
    private static void ShowGate(IClassicDesktopStyleApplicationLifetime desktop)
    {
        if (InuServiceMonitor.IsRunning)
        {
            desktop.MainWindow = new MainWindow();
            return;
        }

        var error = new ServiceErrorWindow();
        error.SetMessage(InuServiceMonitor.MissingMessage);
        error.RetryRequested += () =>
        {
            if (!InuServiceMonitor.IsRunning)
            {
                error.SetMessage(InuServiceMonitor.MissingMessage);
                return;
            }

            var main = new MainWindow();
            desktop.MainWindow = main;
            main.Show();
            error.Close();
        };
        desktop.MainWindow = error;
    }
}
