using System.Globalization;
using System.Text.Json;

namespace RyukinLedger.App.Model;

/// <summary>The kind of line found in <c>ledger.jsonl</c>.</summary>
public enum LedgerKind
{
    /// <summary>The core started, identified the account, or stopped.</summary>
    Session,

    /// <summary>First balance ever seen for a currency: a starting point only.</summary>
    Baseline,

    /// <summary>A change observed live: exactly one action, so the direction is real.</summary>
    Tx,

    /// <summary>A change that happened while the core was not capturing.</summary>
    Gap,
}

/// <summary>
/// One parsed line of <c>ledger.jsonl</c>.
/// </summary>
/// <remarks>
/// The file is append-only and is only ever *read* by this application; the
/// elevated core owns writing it.  Lines that do not parse are skipped rather
/// than throwing, because a crash mid-write can leave a torn final line and a
/// torn line must never cost the user their history.
/// </remarks>
public sealed class LedgerEntry
{
    public required LedgerKind Kind { get; init; }

    /// <summary>When the event happened, in local time.</summary>
    public required DateTimeOffset At { get; init; }

    /// <summary>Set for baseline / tx / gap.</summary>
    public Currency? Currency { get; init; }

    /// <summary>Signed change. Set for tx and gap.</summary>
    public long? Delta { get; init; }

    /// <summary>Balance after the change. Set for baseline, tx and gap.</summary>
    public long? Balance { get; init; }

    /// <summary><c>income</c> or <c>expense</c>; set for tx.</summary>
    public string? Direction { get; init; }

    /// <summary>Set for session records: <c>start</c>, <c>identified</c>, <c>end</c>.</summary>
    public string? Event { get; init; }

    public string? Nickname { get; init; }
    public string? Reason { get; init; }
    public string? Version { get; init; }

    public bool IsIncome => Kind == LedgerKind.Tx && Delta > 0;

    public bool IsExpense => Kind == LedgerKind.Tx && Delta < 0;

    /// <summary>
    /// Parses one line.  Returns <c>null</c> for anything that is not a record
    /// we understand -- an empty line, a torn line, a record written by a newer
    /// core version, or an <c>assumed</c> line from an older one (that kind is
    /// gone; the currency it named simply has no value, which is what the card
    /// shows as "unknown").
    /// </summary>
    public static LedgerEntry? TryParse(string line)
    {
        if (string.IsNullOrWhiteSpace(line))
        {
            return null;
        }

        try
        {
            using JsonDocument document = JsonDocument.Parse(line);
            JsonElement root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return null;
            }

            LedgerKind? kind = ParseKind(root);
            if (kind is null)
            {
                return null;
            }

            if (!TryGetString(root, "at", out string? at) ||
                !DateTimeOffset.TryParse(at, CultureInfo.InvariantCulture, DateTimeStyles.AssumeLocal, out DateTimeOffset timestamp))
            {
                return null;
            }

            Currency? currency = null;
            if (TryGetString(root, "currency", out string? currencyKey) &&
                Currencies.TryParse(currencyKey, out Currency parsed))
            {
                currency = parsed;
            }

            return new LedgerEntry
            {
                Kind = kind.Value,
                At = timestamp,
                Currency = currency,
                Delta = GetInt64(root, "delta"),
                Balance = GetInt64(root, "balance"),
                Direction = GetString(root, "direction"),
                Event = GetString(root, "event"),
                Nickname = GetString(root, "nickname"),
                Reason = GetString(root, "reason"),
                Version = GetString(root, "version"),
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static LedgerKind? ParseKind(JsonElement root) =>
        GetString(root, "kind") switch
        {
            "session" => LedgerKind.Session,
            "baseline" => LedgerKind.Baseline,
            "tx" => LedgerKind.Tx,
            "gap" => LedgerKind.Gap,
            _ => null,
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

    private static bool TryGetString(JsonElement root, string name, out string? value)
    {
        value = GetString(root, name);
        return value is not null;
    }
}
