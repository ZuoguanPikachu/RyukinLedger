namespace RyukinLedger.App.Model;

/// <summary>
/// Turns balances into "how many wishes is this worth".
/// </summary>
/// <remarks>
/// <para>
/// Four resources can be spent on a limited banner, and each buys wishes at a
/// different rate: 160 原石 or 160 创世结晶 per wish, one 纠缠之缘 per wish, and
/// five 无主的星辉 per wish.  相遇之缘 is deliberately absent -- it only works on
/// the standard banner, so counting it would overstate what can be spent on the
/// banner the calculator models.  摩拉 buys nothing.
/// </para>
/// <para>
/// The division is integer division, per resource: a balance of 159 原石 is
/// worth zero wishes, not 0.99 of one, because that is what the game will
/// actually let you do.
/// </para>
/// </remarks>
public static class PullBudget
{
    /// <summary>The resources that can be spent on wishes, in the order they are offered.</summary>
    public static readonly Currency[] Contributors =
    [
        Currency.Primogems,
        Currency.IntertwinedFate,
        Currency.MasterlessStarglitter,
        Currency.GenesisCrystals,
    ];

    /// <summary>Units of the resource that buy one wish; 0 when it buys none.</summary>
    public static int UnitsPerPull(Currency currency) => currency switch
    {
        Currency.Primogems => 160,
        Currency.GenesisCrystals => 160,
        Currency.IntertwinedFate => 1,
        Currency.MasterlessStarglitter => 5,
        _ => 0,
    };

    /// <summary>Whole wishes a balance is worth.  Negative balances count as zero.</summary>
    public static long Pulls(Currency currency, long balance)
    {
        int units = UnitsPerPull(currency);
        return units <= 0 || balance <= 0 ? 0 : balance / units;
    }

    /// <summary>
    /// Total wishes across the selected resources.  A resource that is left out
    /// of <paramref name="included"/> contributes nothing, which is the point:
    /// someone saving 无主的星辉 for a future character does not want it counted
    /// as spendable today.
    /// </summary>
    public static long Total(IReadOnlyDictionary<Currency, long> balances, IReadOnlySet<Currency> included)
    {
        long total = 0;
        foreach (Currency currency in Contributors)
        {
            if (included.Contains(currency) && balances.TryGetValue(currency, out long balance))
            {
                total += Pulls(currency, balance);
            }
        }

        return total;
    }
}
