using System.Globalization;
using System.Windows;
using RyukinLedger.App.Gacha;
using RyukinLedger.App.Model;
using RyukinLedger.App.Services;
using RyukinLedger.App.Views;

namespace RyukinLedger.App;

/// <summary>
/// The wish-probability calculator.
/// </summary>
/// <remarks>
/// <para>
/// Every control recalculates on change; there is no "calculate" button, because
/// the whole value of a calculator like this is trying five parameter sets in
/// five seconds.  The distribution arrays are a few hundred entries at worst, so
/// recomputing on every keystroke costs nothing measurable.
/// </para>
/// <para>
/// Balances are pushed in by the main window rather than read here.  This window
/// does not know about the ledger, the capture core, or where either keeps its
/// files -- it is a calculator with a picture, and the number it is asked to
/// place on that picture arrives from the one place that already resolved it.
/// </para>
/// </remarks>
public partial class GachaWindow : Window
{
    private readonly PullSourceSelection _selection;
    private readonly Dictionary<Currency, long> _balances = [];
    private bool _ready;

    public GachaWindow(PullSourceSelection selection)
    {
        InitializeComponent();

        Icon = AssetLibrary.AppIcon();

        _selection = selection;
        _selection.Changed += OnSelectionChanged;

        TargetSelector.ItemsSource = Enumerable.Range(1, 7).Select(count => new CountChoice(count)).ToList();
        TargetSelector.SelectedIndex = 0;

        FateSelector.ItemsSource = Enumerable.Range(0, 4).Select(fate => new FateChoice(fate)).ToList();
        FateSelector.SelectedIndex = 1;

        PityBox.Text = "0";

        PullPicker.SetIncluded(_selection.Included);
        PullPicker.SelectionChanged += (_, _) => _selection.SetAll(PullPicker.Included);

        Closed += (_, _) => _selection.Changed -= OnSelectionChanged;

        _ready = true;
        Recalculate();
    }

    /// <summary>Replaces the balances the wish count is derived from.</summary>
    /// <remarks>
    /// Called once a second from the main window's refresh loop, so it returns
    /// early when nothing moved: a balance that has not changed must not cost a
    /// full dynamic program, and for a seven-copy target that program is
    /// thousands of convolutions.
    /// </remarks>
    public void SyncBalances(IReadOnlyDictionary<Currency, long> balances)
    {
        if (BalancesMatch(balances))
        {
            return;
        }

        _balances.Clear();
        foreach ((Currency currency, long balance) in balances)
        {
            _balances[currency] = balance;
        }

        PullPicker.UpdateBalances(_balances);
        Recalculate();
    }

    /// <summary>True when every balance we are holding already equals the incoming one.</summary>
    private bool BalancesMatch(IReadOnlyDictionary<Currency, long> balances)
    {
        if (balances.Count != _balances.Count)
        {
            return false;
        }

        foreach ((Currency currency, long balance) in _balances)
        {
            if (!balances.TryGetValue(currency, out long other) || other != balance)
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>Mirrors a selection made in the other window, without echoing it back.</summary>
    public void SyncPullSources(IReadOnlySet<Currency> included)
    {
        PullPicker.SetIncluded(included);
        Recalculate();
    }

    private void OnSelectionChanged(object? sender, EventArgs e)
    {
        PullPicker.SetIncluded(_selection.Included);
        Recalculate();
    }

    private void OnInputChanged(object sender, RoutedEventArgs e) => Recalculate();

    private void Recalculate()
    {
        // Setting the initial values above raises these events before the window
        // is in a consistent state.
        if (!_ready)
        {
            return;
        }

        long affordable = PullBudget.Total(_balances, _selection.Included);
        int currentPulls = (int)Math.Clamp(affordable, 0, int.MaxValue);

        if (!int.TryParse(PityBox.Text.Trim(), out int pity) || pity < 0 || pity >= GachaCalculator.HardPity)
        {
            HintText.Text = $"当前保底要填 0 到 {GachaCalculator.HardPity - 1} 之间的整数。";
            ExpectationText.Text = "—";
            P50Text.Text = "—";
            P90Text.Text = "—";
            P99Text.Text = "—";
            PrimogemText.Text = string.Empty;
            CurrentPullsText.Text = $"当前可抽 {affordable:N0} 抽";
            ProbabilityChartControl.Result = null;
            ProbabilityChartControl.CurrentPulls = null;
            return;
        }

        var parameters = new GachaParameters(
            TargetSelector.SelectedItem is CountChoice count ? count.Count : 1,
            pity,
            GuaranteedCheck.IsChecked == true,
            FateSelector.SelectedItem is FateChoice fate ? fate.Fate : 0);

        GachaResult result = GachaCalculator.Calculate(parameters);

        ExpectationText.Text = result.Expectation.ToString("0.0", CultureInfo.InvariantCulture);
        P50Text.Text = result.P50.ToString(CultureInfo.InvariantCulture);
        P90Text.Text = result.P90.ToString(CultureInfo.InvariantCulture);
        P99Text.Text = result.P99.ToString(CultureInfo.InvariantCulture);

        double primogems = result.Expectation * GachaCalculator.PrimogemsPerPull;
        PrimogemText.Text =
            $"期望消耗 {result.Expectation:0.0} 抽\r\n" +
            $"约 {primogems:N0} 原石";

        // What the balance is worth against this particular target: the same
        // number the chart marks, said in words.
        CurrentPullsText.Text =
            $"当前可抽 {affordable:N0} 抽　·　达成概率 {result.ProbabilityWithin(currentPulls):P0}";

        ProbabilityChartControl.Result = result;
        ProbabilityChartControl.CurrentPulls = currentPulls;

        HintText.Text =
            $"5★ 基础概率 {GachaCalculator.BaseRate:P1}，" +
            $"第 {GachaCalculator.SoftPityStart + 1} 抽起每抽 +{GachaCalculator.SoftPityStep:P0}，" +
            $"第 {GachaCalculator.HardPity} 抽必出。" +
            "　统计不含四星保底，也不考虑定轨之外的其他转换。";
    }

    private sealed record CountChoice(int Count)
    {
        public override string ToString() => $"{Count} 个";
    }

    private sealed record FateChoice(int Fate)
    {
        public override string ToString() => Fate.ToString(CultureInfo.InvariantCulture);
    }
}
