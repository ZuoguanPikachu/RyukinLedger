using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using RyukinLedger.App.Model;

namespace RyukinLedger.App.Views;

/// <summary>
/// A row of filter chips, one per resource that can be spent on wishes, showing
/// what each one is worth.
/// </summary>
/// <remarks>
/// <para>
/// Built in code like the charts, and for the same reason: it is a handful of
/// chips over a fixed list, and a XAML file plus a code-behind would be more
/// moving parts than the thing it draws.  The chip's own look lives in
/// <c>Themes/Chip.xaml</c>, shared with the calculator window.
/// </para>
/// <para>
/// A chip rather than a checkbox because the control *is* the label: name, icon
/// and count all sit on the thing you press, so there is no separate caption to
/// keep in step with it.  Inside it is two labels -- the name on the left, the
/// count on the right -- and the chips share their row evenly, so the counts line
/// up in a column and can be compared at a glance.
/// </para>
/// </remarks>
public sealed class PullSourcePicker : UserControl
{
    /// <summary>
    /// How many chips share a row.  Two in the main window's narrow card, four in
    /// the calculator, which is wide enough for all of them at once.
    /// </summary>
    public static readonly DependencyProperty ColumnsProperty =
        DependencyProperty.Register(
            nameof(Columns),
            typeof(int),
            typeof(PullSourcePicker),
            new PropertyMetadata(2, OnColumnsChanged));

    private static readonly Brush UnselectedBorder = Frozen("#DCCDBC");
    private static readonly Brush UnselectedText = Frozen("#6E6259");
    private static readonly Brush SelectedText = Frozen("#2B2521");
    private static readonly Brush Transparent = Frozen("#00FFFFFF");

    private readonly UniformGrid _chips = new();
    private readonly Dictionary<Currency, Chip> _rows = [];
    private readonly HashSet<Currency> _included = [];

    private IReadOnlyDictionary<Currency, long> _balances = new Dictionary<Currency, long>();
    private bool _suppress;

    public PullSourcePicker()
    {
        _chips.Columns = Columns;
        Content = _chips;

        foreach (Currency currency in PullBudget.Contributors)
        {
            _chips.Children.Add(BuildChip(currency).Toggle);
        }

        ApplyIncluded();
    }

    /// <summary>Raised when the user toggles a chip; never raised by <see cref="SetIncluded"/>.</summary>
    public event EventHandler? SelectionChanged;

    public int Columns
    {
        get => (int)GetValue(ColumnsProperty);
        set => SetValue(ColumnsProperty, value);
    }

    public IReadOnlySet<Currency> Included => _included;

    /// <summary>Shows a selection that came from somewhere else, without echoing it back.</summary>
    public void SetIncluded(IEnumerable<Currency> included)
    {
        _suppress = true;
        try
        {
            var wanted = new HashSet<Currency>(included);
            foreach ((Currency currency, Chip chip) in _rows)
            {
                chip.Toggle.IsChecked = wanted.Contains(currency);
            }
        }
        finally
        {
            _suppress = false;
        }

        ApplyIncluded();
    }

    /// <summary>Re-reads the balances shown on each chip.</summary>
    public void UpdateBalances(IReadOnlyDictionary<Currency, long> balances)
    {
        _balances = new Dictionary<Currency, long>(balances);
        RefreshLabels();
    }

    private static void OnColumnsChanged(DependencyObject target, DependencyPropertyChangedEventArgs e) =>
        ((PullSourcePicker)target)._chips.Columns = Math.Max(1, (int)e.NewValue);

    private Chip BuildChip(Currency currency)
    {
        // Two labels: the name on the left, the count pushed to the right edge.
        // The star column between them takes the slack, so every chip lines its
        // count up with the others.
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var leading = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
        };

        // The real in-game icon when the artwork is present, nothing when it is
        // not: the name is right there, so a coloured dot would only be noise.
        Image? icon = null;
        if (AssetLibrary.CurrencyIcon(currency) is { } source)
        {
            icon = new Image
            {
                Source = source,
                Width = 18,
                Height = 18,
                Stretch = Stretch.Uniform,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 6, 0),
            };

            // Same reason as the balance cards: these are thin, high-contrast
            // line drawings, which is exactly what aliases when a bitmap is
            // reduced by more than about 2x.
            RenderOptions.SetBitmapScalingMode(icon, BitmapScalingMode.HighQuality);
            leading.Children.Add(icon);
        }

        var name = new TextBlock
        {
            Text = currency.DisplayName(),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = UnselectedText,
        };

        leading.Children.Add(name);
        Grid.SetColumn(leading, 0);
        grid.Children.Add(leading);

        var count = new TextBlock
        {
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            TextAlignment = TextAlignment.Right,
            Margin = new Thickness(10, 0, 0, 0),
            Foreground = UnselectedText,
        };

        Grid.SetColumn(count, 1);
        grid.Children.Add(count);

        var toggle = new ToggleButton
        {
            Content = grid,
            Style = (Style)Application.Current.FindResource("CurrencyChip"),
            Tag = currency,
        };

        // The content is a panel, so without this the chip has no name at all for
        // a screen reader (or for a UI test looking it up).
        AutomationProperties.SetName(toggle, currency.DisplayName());

        toggle.Checked += OnChipToggled;
        toggle.Unchecked += OnChipToggled;

        var chip = new Chip(currency, toggle, name, count, icon);
        _rows[currency] = chip;
        return chip;
    }

    private void OnChipToggled(object sender, RoutedEventArgs e)
    {
        if (_suppress)
        {
            return;
        }

        ApplyIncluded();
        SelectionChanged?.Invoke(this, EventArgs.Empty);
    }

    private void ApplyIncluded()
    {
        _included.Clear();
        foreach ((Currency currency, Chip chip) in _rows)
        {
            bool selected = chip.Toggle.IsChecked == true;
            if (selected)
            {
                _included.Add(currency);
            }

            Paint(chip, selected);
        }

        RefreshLabels();
    }

    /// <summary>
    /// The on/off colours, which are per-currency: a selected chip is filled and
    /// outlined with a tint of the same accent its balance card uses, so the two
    /// read as the same thing.
    /// </summary>
    private static void Paint(Chip chip, bool selected)
    {
        Color accent = (Color)ColorConverter.ConvertFromString(chip.Currency.Accent());

        chip.Toggle.Background = selected
            ? Frozen(Color.FromArgb(0x22, accent.R, accent.G, accent.B))
            : Transparent;
        chip.Toggle.BorderBrush = selected
            ? Frozen(Color.FromArgb(0x99, accent.R, accent.G, accent.B))
            : UnselectedBorder;
        chip.Name.Foreground = selected ? SelectedText : UnselectedText;
        chip.Name.FontWeight = selected ? FontWeights.SemiBold : FontWeights.Normal;

        if (chip.Icon is not null)
        {
            chip.Icon.Opacity = selected ? 1.0 : 0.7;
        }
    }

    private void RefreshLabels()
    {
        foreach ((Currency currency, Chip chip) in _rows)
        {
            if (!_balances.TryGetValue(currency, out long balance))
            {
                chip.Count.Text = "—";
                chip.Toggle.ToolTip = $"{currency.DisplayName()}：还没有观测到余额";
                continue;
            }

            long pulls = PullBudget.Pulls(currency, balance);
            chip.Count.Text = $"{DailyChart.Format(pulls)} 抽";

            int units = PullBudget.UnitsPerPull(currency);
            chip.Toggle.ToolTip = units == 1
                ? $"{currency.DisplayName()} {DailyChart.Format(balance)} = {DailyChart.Format(pulls)} 抽"
                : $"{currency.DisplayName()} {DailyChart.Format(balance)} ÷ {units} = {DailyChart.Format(pulls)} 抽";
        }
    }

    private static Brush Frozen(string hex) => Frozen((Color)ColorConverter.ConvertFromString(hex));

    private static Brush Frozen(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    private sealed record Chip(Currency Currency, ToggleButton Toggle, TextBlock Name, TextBlock Count, Image? Icon);
}
