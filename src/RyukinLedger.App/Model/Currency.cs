namespace RyukinLedger.App.Model;

/// <summary>
/// The six resources this ledger tracks.
/// </summary>
/// <remarks>
/// The names are the contract with the capture core: <c>primogems</c>,
/// <c>mora</c>, <c>genesis_crystals</c>, <c>intertwined_fate</c>,
/// <c>acquaint_fate</c> and <c>masterless_starglitter</c> are exactly the keys
/// <c>irminsul</c> writes into <c>ledger.jsonl</c> and <c>status.json</c>.
/// </remarks>
public enum Currency
{
    Primogems,
    Mora,
    GenesisCrystals,
    IntertwinedFate,
    AcquaintFate,
    MasterlessStarglitter,
}

public static class Currencies
{
    /// <summary>
    /// Display order.  This is the order the balance cards appear in, and it is
    /// not the enum order: 摩拉 first because it is the number people watch for
    /// day-to-day spending, then the wishing resources in the order they get
    /// consumed, and 创世结晶 last because it is the one most people keep for
    /// skins rather than spend.
    /// </summary>
    public static readonly Currency[] All =
    [
        Currency.Mora,
        Currency.Primogems,
        Currency.IntertwinedFate,
        Currency.AcquaintFate,
        Currency.MasterlessStarglitter,
        Currency.GenesisCrystals,
    ];

    /// <summary>The stable key used in the ledger, in status.json, and on the wire.</summary>
    public static string Key(this Currency currency) => currency switch
    {
        Currency.Primogems => "primogems",
        Currency.Mora => "mora",
        Currency.GenesisCrystals => "genesis_crystals",
        Currency.IntertwinedFate => "intertwined_fate",
        Currency.AcquaintFate => "acquaint_fate",
        Currency.MasterlessStarglitter => "masterless_starglitter",
        _ => throw new ArgumentOutOfRangeException(nameof(currency)),
    };

    public static string DisplayName(this Currency currency) => currency switch
    {
        Currency.Primogems => "原石",
        Currency.Mora => "摩拉",
        Currency.GenesisCrystals => "创世结晶",
        Currency.IntertwinedFate => "纠缠之缘",
        Currency.AcquaintFate => "相遇之缘",
        Currency.MasterlessStarglitter => "无主的星辉",
        _ => throw new ArgumentOutOfRangeException(nameof(currency)),
    };

    /// <summary>
    /// Accent colour used by the cards and the chart legend, so a currency keeps
    /// the same colour everywhere.
    /// </summary>
    public static string Accent(this Currency currency) => currency switch
    {
        Currency.Primogems => "#4A90D9",
        Currency.Mora => "#D9A441",
        Currency.GenesisCrystals => "#8E6FD0",
        Currency.IntertwinedFate => "#C65B8E",
        Currency.AcquaintFate => "#4FB08A",
        Currency.MasterlessStarglitter => "#C98A4B",
        _ => "#888888",
    };

    public static bool TryParse(string? key, out Currency currency)
    {
        foreach (Currency candidate in All)
        {
            if (string.Equals(candidate.Key(), key, StringComparison.Ordinal))
            {
                currency = candidate;
                return true;
            }
        }

        currency = default;
        return false;
    }
}
