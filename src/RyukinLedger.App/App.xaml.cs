using System.Threading;
using System.Windows;
using System.Windows.Threading;

namespace RyukinLedger.App;

/// <summary>
/// Entry point of the RyukinLedger GUI.
/// </summary>
/// <remarks>
/// The window does not own the process lifetime: closing it hides it in the
/// notification area, because the useful thing this application does is keep
/// recording.  Shutdown is therefore explicit, which is why
/// <c>ShutdownMode</c> is <c>OnExplicitShutdown</c>.
/// </remarks>
public partial class App : Application
{
    private Mutex? _singleInstance;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        // Two instances would fight over the same status.json and stop.request.
        _singleInstance = new Mutex(initiallyOwned: true, @"Local\RyukinLedger.SingleInstance", out bool created);
        if (!created)
        {
            MessageBox.Show(
                "RyukinLedger 已经在运行了（看通知区域里的图标）。",
                "RyukinLedger",
                MessageBoxButton.OK,
                MessageBoxImage.Information);
            Shutdown();
            return;
        }

        // A crash should say what happened rather than just vanish; there is no
        // console attached to a GUI application.
        DispatcherUnhandledException += OnDispatcherUnhandledException;

        new MainWindow().Show();
    }

    private void OnDispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        MessageBox.Show(
            $"RyukinLedger 遇到未处理的错误：\n\n{e.Exception}",
            "RyukinLedger",
            MessageBoxButton.OK,
            MessageBoxImage.Error);

        // Keep running: losing the recording loop because a repaint threw would
        // be worse than the exception itself.
        e.Handled = true;
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _singleInstance?.Dispose();
        base.OnExit(e);
    }
}
