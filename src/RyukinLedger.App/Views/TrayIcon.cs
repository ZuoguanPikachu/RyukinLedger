using System.Drawing;
using System.IO;
using RyukinLedger.App.Model;
using WinForms = System.Windows.Forms;

namespace RyukinLedger.App.Views;

/// <summary>
/// The notification-area presence, so the application can keep recording while
/// its window is closed.
/// </summary>
/// <remarks>
/// This uses <see cref="WinForms.NotifyIcon"/> rather than a WPF tray library:
/// it ships as part of the Windows Desktop runtime, so the application stays a
/// single assembly next to the runtime with no third-party DLLs -- which also
/// keeps the shipped file set, and therefore the SmartScreen story, boring.
///
/// The picture is the same <c>assets\app.ico</c> the executable carries, loaded
/// from disk rather than embedded a second time.
/// </remarks>
public sealed class TrayIcon : IDisposable
{
    private readonly WinForms.NotifyIcon _icon;
    private readonly Icon _appIcon;
    private bool _disposed;

    public TrayIcon()
    {
        _appIcon = LoadAppIcon();
        var menu = new WinForms.ContextMenuStrip();

        var showItem = new WinForms.ToolStripMenuItem("显示主界面(&O)");
        showItem.Click += (_, _) => ShowRequested?.Invoke();
        menu.Items.Add(showItem);

        menu.Items.Add(new WinForms.ToolStripSeparator());

        var startItem = new WinForms.ToolStripMenuItem("开始记录(&S)");
        startItem.Click += (_, _) => StartRequested?.Invoke();
        menu.Items.Add(startItem);

        var stopItem = new WinForms.ToolStripMenuItem("停止记录(&T)");
        stopItem.Click += (_, _) => StopRequested?.Invoke();
        menu.Items.Add(stopItem);

        menu.Items.Add(new WinForms.ToolStripSeparator());

        var exitItem = new WinForms.ToolStripMenuItem("退出(&X)");
        exitItem.Click += (_, _) => ExitRequested?.Invoke();
        menu.Items.Add(exitItem);

        _icon = new WinForms.NotifyIcon
        {
            Icon = _appIcon,
            Text = "RyukinLedger",
            ContextMenuStrip = menu,
            Visible = true,
        };

        _icon.DoubleClick += (_, _) => ShowRequested?.Invoke();
    }

    public event Action? ShowRequested;

    public event Action? StartRequested;

    public event Action? StopRequested;

    public event Action? ExitRequested;

    /// <summary>
    /// Updates the hover text.  Windows caps this at 63 characters, so the
    /// caller's text is truncated rather than silently rejected.
    /// </summary>
    public void SetTooltip(string text)
    {
        if (_disposed)
        {
            return;
        }

        _icon.Text = text.Length > 62 ? text[..59] + "..." : text;
    }

    public void Notify(string title, string text, bool isError = false)
    {
        if (_disposed)
        {
            return;
        }

        try
        {
            _icon.BalloonTipTitle = title;
            _icon.BalloonTipText = text;
            _icon.BalloonTipIcon = isError ? WinForms.ToolTipIcon.Warning : WinForms.ToolTipIcon.Info;
            _icon.ShowBalloonTip(5000);
        }
        catch (Exception)
        {
            // Balloon tips are a nicety; never let one break the app.
        }
    }

    /// <summary>Short label for the tray tooltip, derived from the core's state.</summary>
    public static string DescribeState(CoreState? state) => state switch
    {
        CoreState.Tracking => "记录中",
        CoreState.Receiving => "已连上，等待数据",
        CoreState.WaitingForHandshake => "等待游戏登录",
        CoreState.Error => "抓包出错",
        CoreState.Stopped => "已停止",
        _ => "未运行",
    };

    /// <summary>
    /// The picture the notification area draws, taken from the same .ico the
    /// executable carries.
    /// </summary>
    /// <remarks>
    /// The size is asked for rather than assumed: the notification area is 16 px
    /// at 100% scaling and 24 or 32 px above it, and the .ico ships a frame for
    /// each of those, so letting System.Drawing pick beats scaling one frame to
    /// fit.  A missing or unreadable file costs the application nothing but its
    /// own icon -- the generic Windows one is still better than refusing to
    /// start.
    /// </remarks>
    private static Icon LoadAppIcon()
    {
        try
        {
            string path = Path.Combine(AssetLibrary.Root, "app.ico");
            if (File.Exists(path))
            {
                return new Icon(path, WinForms.SystemInformation.SmallIconSize);
            }
        }
        catch (Exception)
        {
            // Fall through to the stock icon.
        }

        // Cloned because SystemIcons hands out a shared instance, and Dispose
        // below must not reach into it.
        return (Icon)SystemIcons.Application.Clone();
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _icon.Visible = false;
        _icon.Dispose();
        _appIcon.Dispose();
    }
}
