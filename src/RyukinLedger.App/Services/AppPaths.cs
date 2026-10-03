using System.IO;

namespace RyukinLedger.App.Services;

/// <summary>
/// Where everything lives on disk.
/// </summary>
/// <remarks>
/// <para>
/// <b>The directories are created by this application, not by the capture
/// core.</b>  That ordering is the whole point and it is not cosmetic.
/// </para>
/// <para>
/// Windows enforces mandatory integrity control on top of the ACLs: a process
/// cannot create or modify an object at a higher integrity level than its own
/// ("no write up").  The core runs elevated, i.e. at High integrity.  If it
/// created <c>%LOCALAPPDATA%\RyukinLedger</c> itself, that directory would come
/// out labelled High, and this application -- an ordinary Medium process --
/// would then get <c>Access Denied</c> for every write inside it, even though
/// the ACL grants full control.  That failure looks exactly like a broken
/// permission setup and sends you chasing ACLs that were never wrong.
/// </para>
/// <para>
/// Creating the tree here first leaves it at Medium.  Writing *down* is always
/// allowed, so the elevated core can still write everything it owns, and this
/// application keeps full access afterwards.
/// </para>
/// <para>
/// The two directories are separate for the same reason in reverse: whoever
/// creates a directory can write in it.  The core writes the data directory and
/// only reads the control directory; this application does the opposite.
/// </para>
/// </remarks>
public static class AppPaths
{
    /// <summary>Directory name under %LOCALAPPDATA%.</summary>
    public const string FolderName = "RyukinLedger";

    /// <summary>
    /// The root, under Local (not Roaming) AppData: the ledger, the logs and the
    /// cache are machine-local, and roaming profiles would try to synchronise a
    /// file that two machines could then append to at once.
    /// </summary>
    public static string Root { get; } =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), FolderName);

    /// <summary>Written by the capture core; read by this application.</summary>
    public static string DataDirectory { get; } = Path.Combine(Root, "data");

    /// <summary>Written by this application; read by the capture core.</summary>
    public static string ControlDirectory { get; } = Path.Combine(Root, "control");

    /// <summary>Deployed binaries, when the app is installed rather than run from a build tree.</summary>
    public static string AppDirectory { get; } = Path.Combine(Root, "app");

    public static string LedgerFile { get; } = Path.Combine(DataDirectory, "ledger.jsonl");

    public static string StatusFile { get; } = Path.Combine(DataDirectory, "status.json");

    public static string LogDirectory { get; } = Path.Combine(DataDirectory, "log");

    public static string StopRequestFile { get; } = Path.Combine(ControlDirectory, "stop.request");

    public static string SettingsFile { get; } = Path.Combine(Root, "settings.json");

    /// <summary>
    /// Creates the whole tree if it is not there yet.  Call before starting the
    /// core.
    /// </summary>
    /// <returns><c>null</c> on success, otherwise the real failure text.</returns>
    /// <remarks>
    /// Deliberately does not throw.  Creating a directory under
    /// <c>%LOCALAPPDATA%</c> fails outright when this process is at a lower
    /// integrity level than the target -- which is exactly the situation the
    /// interface has to be able to *report* rather than die of.
    /// </remarks>
    public static string? EnsureCreated()
    {
        try
        {
            Directory.CreateDirectory(DataDirectory);
            Directory.CreateDirectory(ControlDirectory);
            return null;
        }
        catch (Exception ex)
        {
            return $"{ex.GetType().Name}: {ex.Message}";
        }
    }

    /// <summary>
    /// Writes and removes a probe file, to find out whether this process can
    /// actually write where the ledger goes.
    /// </summary>
    /// <returns><c>null</c> when writing works, otherwise the real failure text.</returns>
    public static string? Probe()
    {
        if (EnsureCreated() is { } directoryError)
        {
            return directoryError;
        }

        try
        {
            string probe = Path.Combine(DataDirectory, $"write-probe-{Environment.ProcessId}.tmp");
            File.WriteAllText(probe, DateTimeOffset.Now.ToString("O"));
            File.Delete(probe);
            return null;
        }
        catch (Exception ex)
        {
            return $"{ex.GetType().Name}: {ex.Message}";
        }
    }
}
