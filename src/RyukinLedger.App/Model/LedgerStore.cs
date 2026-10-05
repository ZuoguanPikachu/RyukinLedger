using System.IO;
using System.Text;

namespace RyukinLedger.App.Model;

/// <summary>Income, expense and unattributed change for one currency.</summary>
public sealed class CurrencyTotals
{
    public long Income { get; set; }
    public long Expense { get; set; }

    /// <summary>
    /// Signed net change recorded as <c>gap</c>.  It happened over a window the
    /// core did not capture -- a disconnect/reconnect, or the core not running
    /// at all -- so it cannot be split into actions the way a live push can.
    /// Its amount is nevertheless part of <see cref="Income"/> / <see cref="Expense"/>
    /// (by sign); this field keeps the unattributed part visible so the
    /// interface can say how much of those totals came from such a window.
    /// </summary>
    public long Gap { get; set; }

    public long Net => Income - Expense;
}

/// <summary>Everything recorded on one calendar day.</summary>
public sealed class DayBucket
{
    public required DateOnly Day { get; init; }

    public Dictionary<Currency, CurrencyTotals> Totals { get; } = [];

    /// <summary>Last balance seen on this day, per currency.</summary>
    public Dictionary<Currency, long> EndBalance { get; } = [];

    public CurrencyTotals For(Currency currency) =>
        Totals.TryGetValue(currency, out CurrencyTotals? totals) ? totals : new CurrencyTotals();

    public CurrencyTotals For(Currency currency, bool create)
    {
        if (!Totals.TryGetValue(currency, out CurrencyTotals? totals))
        {
            totals = new CurrencyTotals();
            if (create)
            {
                Totals[currency] = totals;
            }
        }

        return totals;
    }
}

/// <summary>One row in the "recent activity" list.</summary>
public sealed class LedgerRow
{
    public required DateTimeOffset At { get; init; }
    public required Currency Currency { get; init; }
    public required long Delta { get; init; }
    public required long Balance { get; init; }
}

/// <summary>
/// Reads <c>ledger.jsonl</c> and keeps the aggregates the interface needs.
/// </summary>
/// <remarks>
/// The file is opened with <see cref="FileShare.ReadWrite"/> because the
/// elevated core holds it open for appending the whole time we are running.
///
/// Reading is incremental: each refresh resumes at the byte offset where the
/// previous one stopped, so a ledger that has been growing for a year costs the
/// same per tick as an empty one.  Bytes are buffered and only *complete* lines
/// are decoded, which keeps a half-written line (and a half-written multi-byte
/// character) from corrupting the parse.
/// </remarks>
public sealed class LedgerStore
{
    /// <summary>How many activity rows to keep for display.</summary>
    private const int MaxRecentRows = 500;

    private readonly string _path;
    private readonly Queue<LedgerRow> _recent = new();
    private byte[] _pending = new byte[64 * 1024];
    private int _pendingCount;
    private long _offset;

    public LedgerStore(string path) => _path = path;

    public string Path => _path;

    /// <summary>Last known balance per currency, from the newest record that carried one.</summary>
    public Dictionary<Currency, long> Balances { get; } = [];

    /// <summary>Per-day aggregates, oldest first.</summary>
    public SortedDictionary<DateOnly, DayBucket> Days { get; } = [];

    /// <summary>Most recent activity, newest first.</summary>
    public IReadOnlyList<LedgerRow> Recent
    {
        get
        {
            var rows = new List<LedgerRow>(_recent);
            rows.Reverse();
            return rows;
        }
    }

    /// <summary>Nickname reported by the core for the account, when it has seen it.</summary>
    public string? Nickname { get; private set; }

    public long TransactionCount { get; private set; }

    public int SessionCount { get; private set; }

    /// <summary>Set when the last refresh could not read the file at all.</summary>
    public string? LastError { get; private set; }

    /// <summary>True when the last refresh appended at least one record.</summary>
    public bool ChangedOnLastRefresh { get; private set; }

    /// <summary>
    /// Picks up everything appended since the previous call.  Safe to call from
    /// a background thread; never throws.
    /// </summary>
    public void Refresh()
    {
        ChangedOnLastRefresh = false;

        try
        {
            using var stream = new FileStream(
                _path,
                FileMode.Open,
                FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete);

            // A ledger that got shorter was replaced or truncated, so start over
            // rather than carrying aggregates from a file that no longer exists.
            if (stream.Length < _offset)
            {
                Reset();
            }

            stream.Seek(_offset, SeekOrigin.Begin);
            if (stream.Length > _offset)
            {
                ReadFrom(stream);
            }

            LastError = null;
        }
        catch (FileNotFoundException)
        {
            // Normal before the core has written anything.
            LastError = null;
        }
        catch (DirectoryNotFoundException)
        {
            LastError = null;
        }
        catch (Exception ex)
        {
            LastError = $"{ex.GetType().Name}: {ex.Message}";
        }
    }

    private void ReadFrom(FileStream stream)
    {
        // Read the newly appended bytes, then decode only whole lines.
        byte[] chunk = new byte[64 * 1024];
        int read;
        while ((read = stream.Read(chunk, 0, chunk.Length)) > 0)
        {
            EnsureCapacity(_pendingCount + read);
            Buffer.BlockCopy(chunk, 0, _pending, _pendingCount, read);
            _pendingCount += read;
        }

        _offset = stream.Position;

        int lastNewline = -1;
        for (int i = _pendingCount - 1; i >= 0; i--)
        {
            if (_pending[i] == (byte)'\n')
            {
                lastNewline = i;
                break;
            }
        }

        if (lastNewline < 0)
        {
            return;
        }

        string text = Encoding.UTF8.GetString(_pending, 0, lastNewline);
        int consumed = lastNewline + 1;
        int remaining = _pendingCount - consumed;
        if (remaining > 0)
        {
            Buffer.BlockCopy(_pending, consumed, _pending, 0, remaining);
        }

        _pendingCount = remaining;

        foreach (string line in text.Split('\n'))
        {
            LedgerEntry? entry = LedgerEntry.TryParse(line);
            if (entry is not null)
            {
                Apply(entry);
                ChangedOnLastRefresh = true;
            }
        }
    }

    private void EnsureCapacity(int required)
    {
        if (_pending.Length >= required)
        {
            return;
        }

        int size = _pending.Length;
        while (size < required)
        {
            size *= 2;
        }

        Array.Resize(ref _pending, size);
    }

    private void Apply(LedgerEntry entry)
    {
        switch (entry.Kind)
        {
            case LedgerKind.Session:
                SessionCount++;
                if (entry.Nickname is { Length: > 0 } nickname)
                {
                    Nickname = nickname;
                }

                break;

            case LedgerKind.Baseline:
                if (entry.Currency is { } baselineCurrency && entry.Balance is { } baselineBalance)
                {
                    Balances[baselineCurrency] = baselineBalance;
                    DayBucket(entry.At).EndBalance[baselineCurrency] = baselineBalance;
                }

                break;

            case LedgerKind.Tx:
            case LedgerKind.Gap:
                if (entry.Currency is not { } currency ||
                    entry.Delta is not { } delta ||
                    entry.Balance is not { } balance)
                {
                    break;
                }

                Balances[currency] = balance;
                DayBucket day = DayBucket(entry.At);
                day.EndBalance[currency] = balance;
                CurrencyTotals totals = day.For(currency, create: true);

                if (entry.Kind == LedgerKind.Gap)
                {
                    // The net change of an uncaptured window.  It cannot be
                    // split into actions, but it does move the balance, so it
                    // counts towards the day's totals by its sign -- leaving it
                    // out would make a day that clearly gained 42,300 mora look
                    // as though nothing happened.  Gap keeps it identifiable.
                    totals.Gap += delta;
                }

                // Income and expense are stored as separate signed events by
                // the core; keep them separate here too, so a day that both
                // earned and spent 160 shows 160/160 rather than zero.
                if (delta > 0)
                {
                    totals.Income += delta;
                    TransactionCount++;
                }
                else if (delta < 0)
                {
                    totals.Expense += -delta;
                    TransactionCount++;
                }

                _recent.Enqueue(new LedgerRow
                {
                    At = entry.At,
                    Currency = currency,
                    Delta = delta,
                    Balance = balance,
                });

                while (_recent.Count > MaxRecentRows)
                {
                    _recent.Dequeue();
                }

                break;
        }
    }

    private DayBucket DayBucket(DateTimeOffset at)
    {
        DateOnly day = DateOnly.FromDateTime(at.LocalDateTime);
        if (!Days.TryGetValue(day, out DayBucket? bucket))
        {
            bucket = new DayBucket { Day = day };
            Days[day] = bucket;
        }

        return bucket;
    }

    private void Reset()
    {
        Days.Clear();
        Balances.Clear();
        _recent.Clear();
        Nickname = null;
        TransactionCount = 0;
        SessionCount = 0;
        _pendingCount = 0;
        _offset = 0;
    }

    /// <summary>Totals for a whole day across every currency.</summary>
    public static (long Income, long Expense) Sum(DayBucket? day)
    {
        if (day is null)
        {
            return (0, 0);
        }

        long income = 0;
        long expense = 0;
        foreach (CurrencyTotals totals in day.Totals.Values)
        {
            income += totals.Income;
            expense += totals.Expense;
        }

        return (income, expense);
    }
}
