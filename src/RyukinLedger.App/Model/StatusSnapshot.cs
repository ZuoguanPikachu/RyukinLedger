using System.Globalization;
using System.IO;
using System.Text.Json;

namespace RyukinLedger.App.Model;

/// <summary>Lifecycle of the capture core, as published in <c>status.json</c>.</summary>
public enum CoreState
{
    /// <summary>Capturing, but the session key is not derived yet: the game has to log in.</summary>
    WaitingForHandshake,

    /// <summary>The session key works and packets are being decoded.</summary>
    Receiving,

    /// <summary>This run has received real game data and is tracking balances.</summary>
    Tracking,

    /// <summary>Capture could not start, or stopped unexpectedly.</summary>
    Error,

    /// <summary>Clean shutdown.</summary>
    Stopped,
}

/// <summary>
/// A snapshot of the capture core's <c>status.json</c>.
/// </summary>
/// <remarks>
/// The core rewrites this file at least once a second, which doubles as its
/// heartbeat: a snapshot whose <see cref="UpdatedAt"/> is older than a few
/// seconds means the feature is gone, not idle.
/// </remarks>
public sealed class StatusSnapshot
{
    /// <summary>How stale <c>updated_at</c> may be before the core counts as dead.</summary>
    public static readonly TimeSpan StaleAfter = TimeSpan.FromSeconds(5);

    public string? App { get; init; }
    public string? Version { get; init; }
    public int Pid { get; init; }
    public DateTimeOffset StartedAt { get; init; }
    public DateTimeOffset UpdatedAt { get; init; }
    public CoreState State { get; init; }

    /// <summary>Identity of the run that wrote this file.</summary>
    public string? Session { get; init; }

    public string? Error { get; init; }
    public string? Nickname { get; init; }
    public Dictionary<Currency, long> Balances { get; init; } = [];

    /// <summary>Every recorded currency has a known balance (possibly from the ledger).</summary>
    public bool Complete { get; init; }

    /// <summary>Whether *this* run has received game data -- not whether the ledger has any.</summary>
    public bool SessionData { get; init; }

    /// <summary>
    /// How often the connection was replaced after this run had already been
    /// recording: an in-game disconnect and reconnect (or the game being closed
    /// and started again).  Data from those windows cannot be recorded, so the
    /// interface says so rather than leaving the player to guess why the core is
    /// waiting for a handshake they already did.
    /// </summary>
    public long Reconnects { get; init; }

    public bool GameRunning { get; init; }
    public long Transactions { get; init; }

    /// <summary>True when the file is fresh enough to believe the core is alive.</summary>
    public bool IsAlive => DateTimeOffset.Now - UpdatedAt < StaleAfter;

    /// <summary>
    /// Reads and parses <paramref name="path"/>.  Returns <c>null</c> when the
    /// file is missing, empty, or being replaced at this instant -- the core
    /// writes it via write-then-rename, so a miss is expected occasionally and
    /// must not be treated as an error.
    /// </summary>
    public static StatusSnapshot? TryRead(string path)
    {
        string text;
        try
        {
            using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            using var reader = new StreamReader(stream);
            text = reader.ReadToEnd();
        }
        catch (Exception)
        {
            return null;
        }

        if (string.IsNullOrWhiteSpace(text))
        {
            return null;
        }

        try
        {
            using JsonDocument document = JsonDocument.Parse(text);
            JsonElement root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return null;
            }

            var balances = new Dictionary<Currency, long>();
            if (root.TryGetProperty("balances", out JsonElement balanceElement) &&
                balanceElement.ValueKind == JsonValueKind.Object)
            {
                foreach (JsonProperty property in balanceElement.EnumerateObject())
                {
                    if (Currencies.TryParse(property.Name, out Currency currency) &&
                        property.Value.ValueKind == JsonValueKind.Number &&
                        property.Value.TryGetInt64(out long value))
                    {
                        balances[currency] = value;
                    }
                }
            }

            return new StatusSnapshot
            {
                App = GetString(root, "app"),
                Version = GetString(root, "version"),
                Pid = (int)(GetInt64(root, "pid") ?? 0),
                StartedAt = GetTime(root, "started_at"),
                UpdatedAt = GetTime(root, "updated_at"),
                State = ParseState(GetString(root, "state")),
                Session = GetString(root, "session"),
                Error = GetString(root, "error"),
                Nickname = GetString(root, "nickname"),
                Balances = balances,
                Complete = GetBool(root, "complete"),
                SessionData = GetBool(root, "session_data"),
                Reconnects = GetInt64(root, "reconnects") ?? 0,
                GameRunning = GetBool(root, "game_running"),
                Transactions = GetInt64(root, "transactions") ?? 0,
            };
        }
        catch (JsonException)
        {
            // Caught mid-rename; the next tick will have a complete file.
            return null;
        }
    }

    private static CoreState ParseState(string? state) => state switch
    {
        "waiting_for_handshake" => CoreState.WaitingForHandshake,
        "receiving" => CoreState.Receiving,
        "tracking" => CoreState.Tracking,
        "stopped" => CoreState.Stopped,
        _ => CoreState.Error,
    };

    private static string? GetString(JsonElement root, string name) =>
        root.TryGetProperty(name, out JsonElement value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    private static long? GetInt64(JsonElement root, string name) =>
        root.TryGetProperty(name, out JsonElement value) && value.ValueKind == JsonValueKind.Number &&
        value.TryGetInt64(out long number)
            ? number
            : null;

    private static bool GetBool(JsonElement root, string name) =>
        root.TryGetProperty(name, out JsonElement value) && value.ValueKind == JsonValueKind.True;

    private static DateTimeOffset GetTime(JsonElement root, string name) =>
        DateTimeOffset.TryParse(GetString(root, name), CultureInfo.InvariantCulture, DateTimeStyles.AssumeLocal, out DateTimeOffset parsed)
            ? parsed
            : default;
}
