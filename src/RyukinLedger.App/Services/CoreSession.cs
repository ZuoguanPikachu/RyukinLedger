using System.ComponentModel;
using System.Diagnostics;
using System.IO;

namespace RyukinLedger.App.Services;

/// <summary>Outcome of an attempt to start the capture core.</summary>
public enum CoreLaunchResult
{
    Started,

    /// <summary>The user dismissed the UAC prompt.</summary>
    Cancelled,

    /// <summary><c>irminsul.exe</c> could not be found.</summary>
    NotFound,

    Failed,
}

/// <summary>
/// Owns the relationship with one run of the capture core.
/// </summary>
/// <remarks>
/// <para>
/// The core captures packets, so it needs administrator rights and is started
/// with the <c>runas</c> verb.  An elevated process cannot inherit redirected
/// stdio, so there is no pipe between the two: they talk through files, and
/// this class is responsible for the two halves of that protocol that belong to
/// the interface.
/// </para>
/// <para>
/// <b>Session identity.</b>  Every app run gets a fresh id, which is passed to
/// the core and echoed in <c>status.json</c>.  It is how this app tells its own
/// core apart from a status file left behind by an earlier one -- and it is
/// what a stop request has to name, so a stale <c>stop.request</c> from a crash
/// cannot kill a fresh capture.
/// </para>
/// <para>
/// <b>Watchdog.</b>  The app passes its own process id as <c>--watch-pid</c>, so
/// the elevated core exits on its own if this process dies.  Without that, a
/// crash in the interface would leave an administrator-level process running
/// with no window to close it from.
/// </para>
/// </remarks>
public sealed class CoreSession
{
    public CoreSession(string corePath)
    {
        CorePath = corePath;
    }

    public string CorePath { get; }

    /// <summary>Identity of this app run; the core only obeys a stop request naming it.</summary>
    public string SessionId { get; } = Guid.NewGuid().ToString("N");

   /// <summary>Command line handed to the core, for display and support.</summary>
    public string LastCommandLine { get; private set; } = string.Empty;

    public string? LastError { get; private set; }

    public CoreLaunchResult Launch()
    {
        // Best effort: if this fails the core will fail too, and it reports that
        // in its own log and in status.json.  Blocking the attempt here would
        // hide the core's much more specific error message.
        _ = AppPaths.EnsureCreated();

        var arguments = new List<string>
        {
            "--data-dir", AppPaths.DataDirectory,
            "--control-dir", AppPaths.ControlDirectory,
            "--session", SessionId,
            "--watch-pid", Environment.ProcessId.ToString(),
        };

        LastCommandLine = $"{CorePath} {string.Join(' ', arguments.Select(Quote))}";

        var startInfo = new ProcessStartInfo
        {
            FileName = CorePath,
            Arguments = string.Join(' ', arguments.Select(Quote)),
            WorkingDirectory = AppPaths.Root,
            // runas means one UAC prompt here, and the process we get back is
            // the real one.  Letting the core re-launch itself instead would
            // make --watch-pid point at a process that is about to exit.
            UseShellExecute = true,
            Verb = "runas",
        };

        try
        {
            Process? process = Process.Start(startInfo);
            if (process is null)
            {
                LastError = "Process.Start returned null.";
                return CoreLaunchResult.Failed;
            }

            LastError = null;
            return CoreLaunchResult.Started;
        }
        catch (Win32Exception ex) when (ex.NativeErrorCode == 1223)
        {
            // ERROR_CANCELLED: the user said no to the UAC prompt.  Not an error,
            // and the interface should say so rather than show a failure.
            LastError = "用户取消了 UAC 提权。";
            return CoreLaunchResult.Cancelled;
        }
        catch (Win32Exception ex)
        {
            LastError = $"Win32 {ex.NativeErrorCode}: {ex.Message}";
            return CoreLaunchResult.Failed;
        }
        catch (Exception ex)
        {
            LastError = $"{ex.GetType().Name}: {ex.Message}";
            return CoreLaunchResult.Failed;
        }
    }

    /// <summary>
    /// Asks the running core to shut down by writing <c>stop.request</c>.
    /// </summary>
    /// <remarks>
    /// The file contains <see cref="SessionId"/>, and the core ignores any
    /// request that does not name its own session.  Deleting the file afterwards
    /// is best effort; the id comparison, not the deletion, is what makes this
    /// safe.
    /// </remarks>
    public string? RequestStop()
    {
        try
        {
            AppPaths.EnsureCreated();
            File.WriteAllText(AppPaths.StopRequestFile, SessionId);
            return null;
        }
        catch (Exception ex)
        {
            return $"{ex.GetType().Name}: {ex.Message}";
        }
    }

    /// <summary>
    /// Last resort for a core that ignored the stop request.
    /// </summary>
    /// <remarks>
    /// Expected to fail when called unelevated: terminating an administrator
    /// process needs administrator rights.  The return value says what happened
    /// so the interface can tell the user to end it from Task Manager instead of
    /// pretending it worked.
    /// </remarks>
    public string? TryKill(int processId)
    {
        if (processId <= 0)
        {
            return "没有可终止的进程 id。";
        }

        try
        {
            using Process process = Process.GetProcessById(processId);
            process.Kill(entireProcessTree: true);
            process.WaitForExit(3000);
            return null;
        }
        catch (ArgumentException)
        {
            return null; // Already gone.
        }
        catch (Exception ex)
        {
            return $"{ex.GetType().Name}: {ex.Message}";
        }
    }

    private static string Quote(string value) => $"\"{value.Replace("\"", "\\\"")}\"";
}
