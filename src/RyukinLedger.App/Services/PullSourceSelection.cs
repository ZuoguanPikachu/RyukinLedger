using RyukinLedger.App.Model;

namespace RyukinLedger.App.Services;

/// <summary>
/// The one answer to "which resources count towards my wishes", shared by every
/// window that shows a wish count.
/// </summary>
/// <remarks>
/// <para>
/// Two windows offer the same checkboxes -- the main window's 可抽次数 card and
/// the calculator's 参数 panel -- and they must not be able to disagree.  Rather
/// than have one window read the other's controls, both point at this object:
/// a change anywhere is persisted and announced once, and every listener
/// redraws from the same state.
/// </para>
/// <para>
/// <see cref="SetAll"/> is idempotent.  A listener that writes back what it was
/// just told therefore stops there instead of bouncing the change back and
/// forth.
/// </para>
/// </remarks>
public sealed class PullSourceSelection
{
    private readonly AppSettings _settings;
    private readonly HashSet<Currency> _included;

    public PullSourceSelection(AppSettings settings)
    {
        _settings = settings;
        _included = settings.ResolvePullSources();
    }

    public IReadOnlySet<Currency> Included => _included;

    public bool Contains(Currency currency) => _included.Contains(currency);

    /// <summary>Raised after a change that was actually persisted.</summary>
    public event EventHandler? Changed;

    /// <summary>
    /// Replaces the selection.  Anything that cannot buy a wish is dropped, so
    /// a stray key from a hand-edited settings file cannot inflate the total.
    /// </summary>
    public void SetAll(IEnumerable<Currency> included)
    {
        var wanted = new HashSet<Currency>();
        foreach (Currency currency in included)
        {
            if (PullBudget.UnitsPerPull(currency) > 0)
            {
                wanted.Add(currency);
            }
        }

        if (wanted.SetEquals(_included))
        {
            return;
        }

        _included.Clear();
        foreach (Currency currency in wanted)
        {
            _included.Add(currency);
        }

        _settings.StorePullSources(_included);
        Changed?.Invoke(this, EventArgs.Empty);
    }
}
