using System.IO;

namespace RyukinLedger.App.Services;

/// <summary>Where a resolved <c>irminsul.exe</c> came from.</summary>
public enum CoreOrigin
{
    /// <summary>Passed on the command line with <c>--core</c>.</summary>
    CommandLine,

    /// <summary>Configured in settings.json.</summary>
    Settings,

    /// <summary>Sitting next to this application.  The normal, deployed case.</summary>
    ApplicationFolder,

    /// <summary>In %LOCALAPPDATA%\RyukinLedger\app\.</summary>
    InstalledFolder,

    /// <summary>
    /// Found by walking up the directory tree into a repository build output.
    /// Off unless explicitly enabled: an application that silently runs a binary
    /// out of a source tree it happens to sit under is a surprise, not a feature.
    /// </summary>
    Repository,
}

/// <summary>A resolved capture core, and how it was found.</summary>
public sealed record CoreLocation(string Path, CoreOrigin Origin)
{
    /// <summary>Short label for the diagnostics line.</summary>
    public string OriginLabel => Origin switch
    {
        CoreOrigin.CommandLine => "命令行 --core",
        CoreOrigin.Settings => "设置",
        CoreOrigin.ApplicationFolder => "同级目录",
        CoreOrigin.InstalledFolder => "已安装目录",
        CoreOrigin.Repository => "构建树（不是部署产物）",
        _ => "未知",
    };

    /// <summary>True when the binary did not come from a deployed folder.</summary>
    public bool IsUndeployed => Origin == CoreOrigin.Repository;
}

/// <summary>
/// Finds <c>irminsul.exe</c>.
/// </summary>
/// <remarks>
/// <para>
/// The supported layout is one folder holding both executables, which is what
/// <c>tools\build.ps1</c> produces in <c>dist\app\</c> and what
/// <c>tools\deploy.ps1</c> produces under <c>%LOCALAPPDATA%</c>.  Reaching back
/// into a build tree from an application running out of that tree is a
/// development convenience, not a deployment strategy, so it is the last resort
/// and it stays switched off unless asked for.
/// </para>
/// <para>
/// Every result carries its origin, so the interface can state where the core
/// came from instead of quietly running whatever it happened to find.
/// </para>
/// </remarks>
public static class CoreLocator
{
    public const string ExecutableName = "irminsul.exe";

    /// <summary>Relative locations tried when walking up a repository tree.</summary>
    private static readonly string[] RepositoryCandidates =
    [
        @"src\irminsul\target\release\" + ExecutableName,
        @"src\irminsul\target\debug\" + ExecutableName,
        ExecutableName,
    ];

    /// <summary>
    /// Resolves the core, or returns <c>null</c> when there is none to run.
    /// </summary>
    /// <param name="commandLinePath">Value of <c>--core</c>, if given.</param>
    /// <param name="settingsPath">Value of the <c>CorePath</c> setting, if set.</param>
    /// <param name="allowRepositorySearch">
    /// Whether to fall back to walking up the tree looking for a build output.
    /// </param>
    public static CoreLocation? Find(string? commandLinePath, string? settingsPath, bool allowRepositorySearch)
    {
        if (TryAccept(commandLinePath) is { } fromCommandLine)
        {
            return new CoreLocation(fromCommandLine, CoreOrigin.CommandLine);
        }

        if (TryAccept(settingsPath) is { } fromSettings)
        {
            return new CoreLocation(fromSettings, CoreOrigin.Settings);
        }

        // Next to this application: the deployed layout, and the one to prefer.
        if (TryAccept(Path.Combine(AppContext.BaseDirectory, ExecutableName)) is { } fromApplication)
        {
            return new CoreLocation(fromApplication, CoreOrigin.ApplicationFolder);
        }

        if (TryAccept(Path.Combine(AppPaths.AppDirectory, ExecutableName)) is { } fromInstalled)
        {
            return new CoreLocation(fromInstalled, CoreOrigin.InstalledFolder);
        }

        if (allowRepositorySearch && FindInRepository() is { } fromRepository)
        {
            return new CoreLocation(fromRepository, CoreOrigin.Repository);
        }

        return null;
    }

    private static string? TryAccept(string? candidate) =>
        !string.IsNullOrWhiteSpace(candidate) && File.Exists(candidate)
            ? Path.GetFullPath(candidate)
            : null;

    private static string? FindInRepository()
    {
        string? directory = AppContext.BaseDirectory;
        while (!string.IsNullOrEmpty(directory))
        {
            foreach (string relative in RepositoryCandidates)
            {
                string candidate = Path.Combine(directory, relative);
                if (File.Exists(candidate))
                {
                    return Path.GetFullPath(candidate);
                }
            }

            DirectoryInfo? parent = Directory.GetParent(directory);
            if (parent is null)
            {
                break;
            }

            directory = parent.FullName;
        }

        return null;
    }
}
