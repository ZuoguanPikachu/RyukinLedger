using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using RyukinLedger.App.Model;

namespace RyukinLedger.App.Services;

/// <summary>
/// User settings, stored as one small JSON file next to the ledger.
/// </summary>
/// <remarks>
/// Loading never throws: a settings file that cannot be read or parsed falls
/// back to defaults rather than preventing the application from starting.
/// </remarks>
public sealed class AppSettings
{
    private static readonly JsonSerializerOptions WriteOptions = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    /// <summary>Explicit path to <c>irminsul.exe</c>; empty means "look next to the app".</summary>
    public string? CorePath { get; set; }

    /// <summary>
    /// Allow the repository-tree fallback: walking up from the application
    /// directory looking for <c>src\irminsul\target\release\irminsul.exe</c>.
    /// </summary>
    /// <remarks>
    /// Off by default.  It is handy while debugging straight out of
    /// <c>bin\Release\</c>, but an application that silently runs a binary from
    /// a source tree it happens to sit under is a surprise rather than a
    /// feature.  <c>tools\build.ps1</c> puts both executables in one folder, so
    /// the normal path never needs this.
    /// </remarks>
    public bool AllowRepositorySearch { get; set; }

    /// <summary>Start the capture core as soon as the application launches.</summary>
    public bool AutoStartCore { get; set; } = true;

    /// <summary>Keep running in the notification area when the window is closed.</summary>
    public bool CloseToTray { get; set; } = true;

    /// <summary>How many days the chart shows.</summary>
    public int ChartDays { get; set; } = 14;

    /// <summary>Currency selected in the chart.</summary>
    public Currency ChartCurrency { get; set; } = Currency.Primogems;

    /// <summary>
    /// Which resources the wish total counts, as ledger keys.
    /// </summary>
    /// <remarks>
    /// <c>null</c> means "never chosen", which counts all of
    /// <see cref="PullBudget.Contributors"/>.  An empty list is a real answer --
    /// "count none of them" -- so the two cases are kept apart rather than
    /// collapsed into one.
    /// </remarks>
    public List<string>? PullSources { get; set; }

    /// <summary>The saved selection, or every contributor when none was saved.</summary>
    public HashSet<Currency> ResolvePullSources()
    {
        if (PullSources is null)
        {
            return [.. PullBudget.Contributors];
        }

        var included = new HashSet<Currency>();
        foreach (string key in PullSources)
        {
            if (Currencies.TryParse(key, out Currency currency) && PullBudget.UnitsPerPull(currency) > 0)
            {
                included.Add(currency);
            }
        }

        return included;
    }

    public void StorePullSources(IEnumerable<Currency> included) =>
        PullSources = [.. included.Select(currency => currency.Key())];

    public static AppSettings Load(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return new AppSettings();
            }

            string text = File.ReadAllText(path);
            return JsonSerializer.Deserialize<AppSettings>(text) ?? new AppSettings();
        }
        catch (Exception)
        {
            return new AppSettings();
        }
    }

    /// <summary>Saves, returning the failure text when it could not be written.</summary>
    public string? Save(string path)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, JsonSerializer.Serialize(this, WriteOptions));
            return null;
        }
        catch (Exception ex)
        {
            return $"{ex.GetType().Name}: {ex.Message}";
        }
    }
}
