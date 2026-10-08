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

    /// <summary>
    /// 原粹树脂 as the core's model says the game is showing it, or <c>null</c>
    /// while the game has reported no value.
    /// </summary>
    /// <remarks>
    /// Deliberately outside <see cref="Balances"/>: it is not a ledger currency,
    /// so it takes no part in the resource cards, the chart, the wish total or
    /// the "every currency is known" test.  It is a value *at
    /// <see cref="UpdatedAt"/>* -- resin regenerates on its own and the game
    /// sends nothing for that -- and <see cref="ResinClock.At"/> is what walks
    /// it forward from there.
    /// </remarks>
    public int? OriginalResin { get; init; }

    /// <summary>
    /// When 原粹树脂 last went up on its own, or <c>null</c> while that has not
    /// been seen.
    /// </summary>
    /// <remarks>
    /// The anchor <see cref="ResinClock.At"/> extrapolates along.  With it, the
    /// value keeps being exact however long ago the core last wrote; without it
    /// -- when nothing but a sync has arrived -- the extrapolation can be one
    /// point low.
    /// </remarks>
    public DateTimeOffset? OriginalResinLastIncreaseAt { get; init; }

    /// <summary>Every recorded currency has a known balance (possibly from the ledger).</summary>
    public bool Complete { get; init; }

    /// <summary>Whether *this* run has received game data -- not whether the ledger has any.</summary>
    public bool SessionData { get; init; }

    /// <summary>
    /// How often the core had to give up on a connection because it could no
    /// longer read it.  Data from those windows cannot be recorded, so the
    /// interface says so rather than leaving the player to guess why the core is
    /// waiting for a handshake they already did.  A connection the game merely
    /// replaced -- leaving co-op, say -- is not counted, because nothing is lost
    /// there.
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
                OriginalResin = (int?)GetInt64(root, "original_resin"),
                OriginalResinLastIncreaseAt = GetTimeOrNull(root, "original_resin_last_increase_at"),
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

    /// <summary>An instant that may simply not be there, unlike <see cref="GetTime"/>.</summary>
    private static DateTimeOffset? GetTimeOrNull(JsonElement root, string name) =>
        DateTimeOffset.TryParse(GetString(root, name), CultureInfo.InvariantCulture, DateTimeStyles.AssumeLocal, out DateTimeOffset parsed)
            ? parsed
            : null;
}
