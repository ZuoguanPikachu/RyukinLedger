using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using RyukinLedger.App.Model;
using RyukinLedger.App.Services;
using RyukinLedger.App.Views;

namespace RyukinLedger.App;

public partial class MainWindow : Window
{
    private readonly AppSettings _settings;
    private readonly LedgerStore _ledger;
    private readonly PullSourceSelection _pullSelection;
    private readonly DispatcherTimer _timer;
    private readonly ObservableCollection<ActivityRow> _activity = [];
    private readonly List<CurrencyCard> _cards = [];

    /// <summary>
    /// The balance each card is showing: the core's live value when it is
    /// running, the ledger's otherwise.  Kept per refresh so the wish total is
    /// computed from exactly the numbers on screen.
    /// </summary>
    private readonly Dictionary<Currency, long> _balances = [];

    private TrayIcon? _tray;
    private CoreSession? _session;
    private StatusSnapshot? _status;

    /// <summary>The core resolved at startup, together with where it came from.</summary>
    private CoreLocation? _core;

    /// <summary>Set while the wish-probability window is open, so it is not opened twice.</summary>
    private GachaWindow? _gachaWindow;

    private string? _writeProbeError;
    private string? _lastLaunchMessage;
    private CoreState? _lastNotifiedState;
    private DateOnly _chartDay;
    private Currency _selectedCurrency;
    private bool _chartDirty = true;
    private bool _ledgerReady;
    private bool _exitRequested;
    private bool _balloonShown;

    // The chart's income/expense hues, taken a step darker.  As 10px bars they
    // are fine; as 12px text on a card that lets the artwork through they land
    // near 3:1, and a total nobody can read at a glance is not a summary.
    private const string IncomeFigureColor = "#2F7F63";
    private const string ExpenseFigureColor = "#B8453A";

    public MainWindow()
    {
        InitializeComponent();

        // Cosmetic, so a missing or unreadable .ico leaves the stock title-bar
        // icon rather than stopping the window from opening.
        Icon = AssetLibrary.AppIcon();

        _settings = AppSettings.Load(AppPaths.SettingsFile);
        _ledger = new LedgerStore(AppPaths.LedgerFile);
        _pullSelection = new PullSourceSelection(_settings);
        _pullSelection.Changed += (_, _) => OnPullSourcesChanged();

        // A settings file is just JSON on disk, so it can name a currency that
        // no longer exists.  Falling back to the first card keeps the chart
        // pointing at something real.
        _selectedCurrency = Currencies.All.Contains(_settings.ChartCurrency)
            ? _settings.ChartCurrency
            : Currencies.All[0];

        _chartDay = DateOnly.FromDateTime(DateTime.Now);

        BuildCurrencyCards();
        PopulateRangeSelector();
        BuildPullCard();
        ApplyCardSelection();

        ActivityList.ItemsSource = _activity;

        _timer = new DispatcherTimer(DispatcherPriority.Background) { Interval = TimeSpan.FromSeconds(1) };
        _timer.Tick += (_, _) => Refresh();

        Loaded += OnLoaded;
        Closing += OnClosing;
        Closed += OnClosed;
    }

    // -----------------------------------------------------------------------
    // Startup

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        // The tree is created here, by this application, *before* the elevated
        // core is started.  See AppPaths for why that ordering is load-bearing.
        // Probe() does the creating and reports what went wrong in one step.
        _writeProbeError = AppPaths.Probe();

        // Resolve the core once and remember how it was found, so the footer can
        // say so.  There is no silent fallback into a build tree unless the
        // setting explicitly asks for one.
        _core = CoreLocator.Find(CommandLineValue("--core"), _settings.CorePath, _settings.AllowRepositorySearch);

        // Artwork is entirely optional: no assets folder simply means a plainer
        // window, never a failure to start.
        if (AssetLibrary.Background() is { } backdrop)
        {
            BackdropArtBrush.ImageSource = backdrop;
            BackdropArt.Visibility = Visibility.Visible;
        }

        _tray = new TrayIcon();
        _tray.ShowRequested += RestoreFromTray;
        _tray.StartRequested += () => StartCore(silent: false);
        _tray.StopRequested += StopCore;
        _tray.ExitRequested += ExitApplication;

        if (_writeProbeError is not null)
        {
            _lastLaunchMessage = $"无法写入数据目录，抓包内核会失败：{_writeProbeError}";
        }

        Refresh();

        if (_settings.AutoStartCore && !HasCommandLineFlag("--no-core"))
        {
            StartCore(silent: true);
        }

        _timer.Start();
    }

    /// <summary>True when the switch is present, in any position.</summary>
    private static bool HasCommandLineFlag(string name) =>
        Environment.GetCommandLineArgs().Any(argument => string.Equals(argument, name, StringComparison.OrdinalIgnoreCase));

    /// <summary>
    /// Reads <c>--name value</c> or <c>--name=value</c> from the command line.
    /// </summary>
    private static string? CommandLineValue(string name)
    {
        string[] arguments = Environment.GetCommandLineArgs();
        for (int i = 0; i < arguments.Length; i++)
        {
            if (string.Equals(arguments[i], name, StringComparison.OrdinalIgnoreCase) && i + 1 < arguments.Length)
            {
                return arguments[i + 1];
            }

            string prefix = name + "=";
            if (arguments[i].StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            {
                return arguments[i][prefix.Length..];
            }
        }

        return null;
    }

    private void PopulateRangeSelector()
    {
        var ranges = new List<RangeChoice>
        {
            new("近 7 天", 7),
            new("近 14 天", 14),
            new("近 30 天", 30),
            new("近 90 天", 90),
        };

        RangeSelector.ItemsSource = ranges;
        int savedRange = ranges.FindIndex(range => range.Days == _settings.ChartDays);
        RangeSelector.SelectedIndex = savedRange >= 0 ? savedRange : 1;

        // Deliberately does not set _ledgerReady: the first Refresh() has to run
        // the activity and summary update even when the ledger is empty, or the
        // "no records yet" hint never appears.
    }

    /// <summary>
    /// Wires the wish-total card to the shared selection.  The card does not own
    /// the answer: it reports a change, and comes back through
    /// <see cref="OnPullSourcesChanged"/> once it has been accepted, so the
    /// calculator's copy of the same checkboxes can never drift out of step.
    /// </summary>
    private void BuildPullCard()
    {
        PullPicker.SetIncluded(_pullSelection.Included);
        PullPicker.SelectionChanged += (_, _) => _pullSelection.SetAll(PullPicker.Included);

        // // One line, and the rule stated once: the per-row "→ N 抽" already shows
        // // the arithmetic, so this only has to say what the divisors are.
        // PullHintText.Text = "折算：原石、创世结晶 ÷160　纠缠之缘 ÷1　无主的星辉 ÷5";
        // PullHintText.ToolTip = "相遇之缘只能抽常驻，不计入可抽次数。";
    }

    private void OnPullSourcesChanged()
    {
        PullPicker.SetIncluded(_pullSelection.Included);
        _settings.Save(AppPaths.SettingsFile);
        _gachaWindow?.SyncPullSources(_pullSelection.Included);
        UpdatePullCard();
    }

    private void BuildCurrencyCards()
    {
        foreach (Currency currency in Currencies.All)
        {
            // The vertical rhythm of this card is deliberately tight: six of them
            // in three rows have to fit the resource column at the default window
            // height without a scroll bar.  Every number here is multiplied by
            // three -- 2px per card is 6px per row is 18px per column -- so the
            // amounts that look like rounding are the ones holding the fit.
            var header = new StackPanel { Orientation = Orientation.Horizontal };

            // The real in-game icon when the artwork is present, a plain coloured
            // dot when it is not: the card has to look deliberate either way,
            // not broken.
            if (AssetLibrary.CurrencyIcon(currency) is { } icon)
            {
                var iconImage = new Image
                {
                    Source = icon,
                    Width = 24,
                    Height = 24,
                    Stretch = Stretch.Uniform,
                    VerticalAlignment = VerticalAlignment.Center,
                    Margin = new Thickness(0, 0, 9, 0),
                };

                // Linear (the default) aliases badly once a bitmap is reduced by
                // more than about 2x.  These icons are thin, high-contrast line
                // art, which is exactly the case where that shows as jaggies.
                RenderOptions.SetBitmapScalingMode(iconImage, BitmapScalingMode.HighQuality);

                header.Children.Add(iconImage);
            }
            else
            {
                header.Children.Add(new Border
                {
                    Width = 8,
                    Height = 8,
                    CornerRadius = new CornerRadius(4),
                    Background = Brush(currency.Accent()),
                    VerticalAlignment = VerticalAlignment.Center,
                    Margin = new Thickness(0, 0, 6, 0),
                });
            }

            header.Children.Add(new TextBlock
            {
                Text = currency.DisplayName(),
                FontSize = 12.5,
                Foreground = Brush("#8C8074"),
                VerticalAlignment = VerticalAlignment.Center,
            });

            var balance = new TextBlock
            {
                Text = "—",
                FontSize = 19,
                FontWeight = FontWeights.SemiBold,
                Foreground = Brush("#2B2521"),
                Margin = new Thickness(0, 3, 0, 0),
            };

            // Two rows, each naming the day it covers.
            //
            // The shared "今日" on the left (the previous shape) saved two glyphs
            // and cost the second row its subject: 支 dangled under a label that
            // belonged to the row above it, and the eye had to travel back left
            // to find out what it was subtracting from.  Repeating the day is
            // how a table does it -- every row says what it is.
            //
            // The three columns do the alignment work: the day column, the
            // direction column, and a value column sized to the widest of the
            // two numbers and right-aligned inside it, so 174,475 and 1,041,040
            // end on the same edge and their digits line up by magnitude.
            var todayGrid = new Grid { Margin = new Thickness(0, 3, 0, 0) };
            todayGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            todayGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            todayGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            todayGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            todayGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

            var todayIncome = TodayFigureValue(IncomeFigureColor, row: 0);
            var todayExpense = TodayFigureValue(ExpenseFigureColor, row: 1);

            todayGrid.Children.Add(TodayCell("今日", column: 0, row: 0, gap: 8));
            todayGrid.Children.Add(TodayCell("今日", column: 0, row: 1, gap: 8));
            todayGrid.Children.Add(TodayCell("收", column: 1, row: 0, gap: 5));
            todayGrid.Children.Add(TodayCell("支", column: 1, row: 1, gap: 5));
            todayGrid.Children.Add(todayIncome);
            todayGrid.Children.Add(todayExpense);

            var stack = new StackPanel();
            stack.Children.Add(header);
            stack.Children.Add(balance);
            stack.Children.Add(todayGrid);

            // The card *is* the chart selector: clicking it switches the chart on
            // the right, so there is no dropdown repeating the same six names.
            var card = new Border
            {
                Style = (Style)FindResource("Card"),
                Margin = new Thickness(0, 0, 6, 4),
                Padding = new Thickness(11, 7, 11, 7),
                Child = stack,
                Cursor = Cursors.Hand,
                Tag = currency,
                ToolTip = $"点击查看{currency.DisplayName()}的每日收支",
            };

            card.MouseLeftButtonUp += OnCurrencyCardClick;

            CurrencyGrid.Children.Add(card);
            _cards.Add(new CurrencyCard(currency, card, balance, new TodayFigures(todayIncome, todayExpense)));
        }
    }

    /// <summary>
    /// A muted label cell of a resource card's today block -- the day ("今日")
    /// or the direction ("收" / "支").  The columns are what align them: every
    /// day lands on one x, every direction on another.
    /// </summary>
    private static TextBlock TodayCell(string text, int column, int row, double gap)
    {
        var cell = new TextBlock
        {
            Text = text,
            FontSize = 11,
            Foreground = Brush("#A2968A"),
            Margin = new Thickness(0, 0, gap, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };

        Grid.SetColumn(cell, column);
        Grid.SetRow(cell, row);
        return cell;
    }

    /// <summary>
    /// One value line of a card's today block, in the given colour.  Right
    /// aligned inside its column: a column of numbers is read by its right edge,
    /// where the units are, not by its first digit.
    /// </summary>
    private static TextBlock TodayFigureValue(string color, int row)
    {
        var value = new TextBlock
        {
            Text = "—",
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            Foreground = Brush(color),
            HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0),
        };

        Grid.SetColumn(value, 2);
        Grid.SetRow(value, row);
        return value;
    }

    /// <summary>Marks the selected card and names the currency in the chart header.</summary>
    private void ApplyCardSelection()
    {
        foreach (CurrencyCard card in _cards)
        {
            bool selected = card.Currency == _selectedCurrency;

            // Only two properties change, and the border thickness deliberately
            // does not: growing it would nudge every card in the grid sideways
            // each time the selection moved.
            card.Container.BorderBrush = Brush(selected ? card.Currency.Accent() : "#E4D6C8");
            card.Container.Background = Brush(selected ? "#EDFFFFFF" : "#B3FFFFFF");
        }

        ChartTitle.Text = $"每日收支 · {_selectedCurrency.DisplayName()}";
    }

    // -----------------------------------------------------------------------
    // The one-second loop

    private void Refresh()
    {
        _ledger.Refresh();
        _status = StatusSnapshot.TryRead(AppPaths.StatusFile);

        // A status file only counts when it names the core this app started: a
        // leftover from an earlier run would otherwise be presented as the
        // current state.  Before any core has been started there is nothing to
        // compare against, so whatever is there is taken at face value --
        // staleness (see StatusSnapshot.IsAlive) still rules out a dead one.
        if (_status is not null && _session is not null && _status.Session != _session.SessionId)
        {
            _status = null;
        }

        DateOnly today = DateOnly.FromDateTime(DateTime.Now);
        if (today != _chartDay)
        {
            // A new day started while we were running: the rolling window moved.
            _chartDay = today;
            _chartDirty = true;
        }

        if (_ledger.ChangedOnLastRefresh || !_ledgerReady)
        {
            _ledgerReady = true;
            _chartDirty = true;
            UpdateActivity();
        }

        UpdateStatusUi();
        UpdateCurrencyCards();

        // The calculator's "current wishes" line comes from the same numbers the
        // cards show, so it is pushed rather than re-derived over there.
        _gachaWindow?.SyncBalances(_balances);

        if (_chartDirty)
        {
            _chartDirty = false;
            UpdateChart();
        }

        UpdateDiagnostics();
    }

    private void UpdateStatusUi()
    {
        bool alive = _status is { IsAlive: true };
        CoreState? state = alive ? _status!.State : null;

        StatusDot.Fill = state switch
        {
            CoreState.Tracking => Brush("#3E9E7E"),
            CoreState.Receiving => Brush("#4A90D9"),
            CoreState.WaitingForHandshake => Brush("#E8B15C"),
            CoreState.Error => Brush("#DE5B4B"),
            CoreState.Stopped => Brush("#C3B8AC"),
            _ => Brush("#C3B8AC"),
        };

        if (state is null)
        {
            StatusText.Text = "内核未运行";
            StatusDetail.Text = _lastLaunchMessage ?? "点「开始记录」以管理员权限启动抓包内核。";
        }
        else
        {
            StatusText.Text = state switch
            {
                CoreState.Tracking => "记录中",
                CoreState.Receiving => "已握手，等待游戏数据",
                CoreState.WaitingForHandshake => "等待游戏登录",
                CoreState.Error => "抓包出错",
                CoreState.Stopped => "已停止",
                _ => "未知状态",
            };

            var parts = new List<string>();

            if (state == CoreState.WaitingForHandshake)
            {
                // Two very different reasons land here.  Either the core was
                // started after the login, so it never saw the handshake it needs
                // -- or a connection was replaced while it *was* recording, which
                // is an in-game disconnect the player may not even have noticed.
                // Telling the second user to "log in again" without saying why
                // reads as though the core forgot a login they just did.
                parts.Add(_status!.Reconnects > 0
                    ? "游戏内发生过断网重连，可能存在未被记录的数据；重进游戏（退回登录界面重新登录）会补齐差值。"
                    : "密钥要在登录握手时才能推导——如果是在进入游戏之后才启动的，请退到登录界面重新登录。");
            }

            if (_status!.Nickname is { Length: > 0 } nickname)
            {
                parts.Add($"账号 {nickname}");
            }

            if (_status.GameRunning)
            {
                parts.Add("游戏在运行");
            }

            parts.Add($"本次记录 {_status.Transactions:N0} 笔");

            if (_status.Error is { Length: > 0 } error)
            {
                parts.Add($"错误：{error}");
            }

            StatusDetail.Text = string.Join("　·　", parts);
        }

        StartButton.IsEnabled = !alive;
        StopButton.IsEnabled = alive;

        SubtitleText.Text = _ledger.Nickname is { Length: > 0 } name
            ? $"原神资源账本　·　{name}"
            : "原神资源账本";

        if (_tray is not null)
        {
            _tray.SetTooltip($"RyukinLedger — {TrayIcon.DescribeState(state)}");
        }

        // One notification per state change, and never a stream of them.
        if (alive && state != _lastNotifiedState)
        {
            _lastNotifiedState = state;
            switch (state)
            {
                case CoreState.Tracking when _balloonShown:
                    _tray?.Notify("RyukinLedger", "已经抓到游戏数据，正在记录收支。");
                    break;
                case CoreState.Error:
                    _tray?.Notify("RyukinLedger", _status?.Error ?? "抓包内核报错。", isError: true);
                    break;
            }
        }
    }

    private void UpdateCurrencyCards()
    {
        DateOnly today = DateOnly.FromDateTime(DateTime.Now);
        _ledger.Days.TryGetValue(today, out DayBucket? day);

        // Resolve every balance once.  The core's live value wins: it is what the
        // game last pushed.  The ledger's value is the fallback for when the core
        // is not running.
        _balances.Clear();
        foreach (Currency currency in Currencies.All)
        {
            if (_status is { IsAlive: true } live && live.Balances.TryGetValue(currency, out long fromCore))
            {
                _balances[currency] = fromCore;
            }
            else if (_ledger.Balances.TryGetValue(currency, out long fromLedger))
            {
                _balances[currency] = fromLedger;
            }
        }

        foreach (CurrencyCard card in _cards)
        {
            card.Balance.Text = _balances.TryGetValue(card.Currency, out long balance)
                ? DailyChart.Format(balance)
                : "—";

            CurrencyTotals totals = day is null ? new CurrencyTotals() : day.For(card.Currency);
            card.Today.Income.Text = DailyChart.Format(totals.Income);
            card.Today.Expense.Text = DailyChart.Format(totals.Expense);
        }

        UpdatePullCard();
    }

    /// <summary>Shows what the selected resources are worth, and keeps the checkboxes current.</summary>
    private void UpdatePullCard()
    {
        PullPicker.UpdateBalances(_balances);

        // A total is only meaningful once every resource it counts has actually
        // been seen.  "0 抽" for a balance nobody has observed is the same lie as
        // "0 摩拉", and worse, it reads like an answer.  An empty selection is a
        // real answer though: it counts nothing, and nothing comes to zero.
        bool known = _pullSelection.Included.All(currency => _balances.ContainsKey(currency));
        PullTotalText.Text = known
            ? $"{DailyChart.Format(PullBudget.Total(_balances, _pullSelection.Included))} 抽"
            : "—";
    }

    private void UpdateChart()
    {
        Currency currency = _selectedCurrency;
        int days = SelectedRangeDays();

        DateOnly today = DateOnly.FromDateTime(DateTime.Now);
        var points = new List<DailyPoint>(days);

        long carried = 0;
        bool hasCarried = false;

        for (int offset = days - 1; offset >= 0; offset--)
        {
            DateOnly day = today.AddDays(-offset);
            _ledger.Days.TryGetValue(day, out DayBucket? bucket);

            if (bucket is not null && bucket.EndBalance.TryGetValue(currency, out long endBalance))
            {
                carried = endBalance;
                hasCarried = true;
            }

            CurrencyTotals totals = bucket is null ? new CurrencyTotals() : bucket.For(currency);
            points.Add(new DailyPoint(day, totals.Income, totals.Expense, carried, hasCarried));
        }

        Chart.SeriesLabel = currency.DisplayName();
        Chart.Accent = Brush(currency.Accent());
        Chart.Points = points;
    }

    private void UpdateActivity()
    {
        _activity.Clear();
        foreach (LedgerRow row in _ledger.Recent)
        {
            if (_activity.Count >= 200)
            {
                break;
            }

            string sign = row.Delta > 0 ? "+" : string.Empty;
            _activity.Add(new ActivityRow
            {
                TimeText = row.At.LocalDateTime.ToString("MM-dd HH:mm:ss"),
                CurrencyText = row.Currency.DisplayName(),
                DeltaText = string.Concat(sign, DailyChart.Format(row.Delta)),
                BalanceText = DailyChart.Format(row.Balance),
            });
        }

        ActivityEmptyHint.Visibility = _activity.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        UpdateLedgerSummary();
    }

    /// <summary>
    /// Fills the "账本概况" card with the shape of the history, rather than
    /// repeating the balances that are already on the cards above it.
    /// </summary>
    private void UpdateLedgerSummary()
    {
        if (_ledger.Days.Count == 0)
        {
            LedgerSummaryText.Text = _ledger.LastError is { Length: > 0 } error
                ? $"读不到账本：{error}"
                : "账本还是空的。";
            return;
        }

        DateOnly first = _ledger.Days.Keys.First();
        DateOnly last = _ledger.Days.Keys.Last();
        DateTimeOffset? latest = _ledger.Recent.Count > 0 ? _ledger.Recent[0].At : null;

        // Two lines, not three: the card sits at the bottom of a column that now
        // also carries the wish total, and a third line was the difference
        // between fitting and needing to scroll.
        var lines = new List<string>
        {
            $"{_ledger.Days.Count} 天有记录（{first:yyyy-MM-dd} ~ {last:yyyy-MM-dd}）",
            latest is { } time
                ? $"{_ledger.TransactionCount:N0} 笔收支　·　{_ledger.SessionCount} 次会话　·　最近 {time.LocalDateTime:MM-dd HH:mm:ss}"
                : $"{_ledger.TransactionCount:N0} 笔收支　·　{_ledger.SessionCount} 次会话",
        };

        LedgerSummaryText.Text = string.Join(Environment.NewLine, lines);
    }

    private void UpdateDiagnostics()
    {
        var lines = new List<string>
        {
            $"数据目录 {AppPaths.DataDirectory}",
            _core is null
                ? "内核 未找到 irminsul.exe"
                : $"内核 {_core.Path}（{_core.OriginLabel}）",
        };

        if (_ledger.LastError is { Length: > 0 } ledgerError)
        {
            lines.Add($"账本读取失败 {ledgerError}");
        }

        if (_writeProbeError is { Length: > 0 })
        {
            lines.Add($"数据目录不可写 {_writeProbeError}");
        }

        DiagnosticsText.Text = string.Join("　|　", lines);
    }

    // -----------------------------------------------------------------------
    // Core control

    private void StartCore(bool silent)
    {
        if (_status is { IsAlive: true })
        {
            _lastLaunchMessage = "内核已经在运行了。";
            return;
        }

        if (_core is null)
        {
            _lastLaunchMessage = "找不到 irminsul.exe。请先跑 tools\\build.ps1 把界面和内核放到一起，" +
                                 "或用 --core <路径> 指定它的位置。";
            if (!silent)
            {
                MessageBox.Show(this, _lastLaunchMessage, "RyukinLedger",
                    MessageBoxButton.OK, MessageBoxImage.Warning);
            }

            return;
        }

        _session = new CoreSession(_core.Path);
        CoreLaunchResult result = _session.Launch();

        _lastLaunchMessage = result switch
        {
            CoreLaunchResult.Started => "已请求启动内核（需要管理员权限）。",
            CoreLaunchResult.Cancelled => "已取消提权，内核没有启动。",
            CoreLaunchResult.NotFound => "找不到 irminsul.exe。",
            _ => $"启动内核失败：{_session.LastError}",
        };

        if (result is CoreLaunchResult.Failed or CoreLaunchResult.NotFound && !silent)
        {
            MessageBox.Show(this, _lastLaunchMessage, "RyukinLedger",
                MessageBoxButton.OK, MessageBoxImage.Warning);
        }
    }

    private void StopCore()
    {
        if (_session is null)
        {
            _lastLaunchMessage = "本进程没有启动过内核。";
            return;
        }

        string? error = _session.RequestStop();
        _lastLaunchMessage = error is null
            ? "已请求内核停止。"
            : $"写入停止请求失败：{error}";
    }

    private void OnStartCoreClick(object sender, RoutedEventArgs e) => StartCore(silent: false);

    private void OnStopCoreClick(object sender, RoutedEventArgs e) => StopCore();

    private void OnOpenDataFolderClick(object sender, RoutedEventArgs e)
    {
        try
        {
            AppPaths.EnsureCreated();
            Process.Start(new ProcessStartInfo
            {
                FileName = AppPaths.Root,
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            MessageBox.Show(this, ex.Message, "RyukinLedger", MessageBoxButton.OK, MessageBoxImage.Warning);
        }
    }

    /// <summary>
    /// Opens the wish-probability calculator, reusing the window if it is
    /// already up rather than stacking copies of it.
    /// </summary>
    private void OnOpenGachaClick(object sender, RoutedEventArgs e)
    {
        if (_gachaWindow is null)
        {
            _gachaWindow = new GachaWindow(_pullSelection) { Owner = this };
            _gachaWindow.Closed += (_, _) => _gachaWindow = null;

            // Seed it before showing: a window that opens on "—" and fills in a
            // second later looks broken.
            _gachaWindow.SyncBalances(_balances);
            _gachaWindow.Show();
            return;
        }

        _gachaWindow.Activate();
    }

    /// <summary>Switches the chart to the currency on the card that was clicked.</summary>
    private void OnCurrencyCardClick(object sender, MouseButtonEventArgs e)
    {
        if (sender is not Border { Tag: Currency currency } || currency == _selectedCurrency)
        {
            return;
        }

        _selectedCurrency = currency;
        _settings.ChartCurrency = currency;
        ApplyCardSelection();

        _chartDirty = true;
        UpdateChart();
    }

    private void OnChartOptionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!IsLoaded)
        {
            return;
        }

        _settings.ChartDays = SelectedRangeDays();
        _chartDirty = true;
        UpdateChart();
    }

    private int SelectedRangeDays() =>
        RangeSelector.SelectedItem is RangeChoice choice ? choice.Days : 14;

    // -----------------------------------------------------------------------
    // Tray and shutdown

    private void OnClosing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        if (!_exitRequested && _settings.CloseToTray)
        {
            e.Cancel = true;
            Hide();

            if (!_balloonShown)
            {
                _balloonShown = true;
                _tray?.Notify("RyukinLedger", "还在后台记录。双击托盘图标可以打开窗口。");
            }

            return;
        }

        _settings.Save(AppPaths.SettingsFile);

        // Ask nicely first; the core also watches our process id, so it will not
        // outlive us even if this request is lost.
        _session?.RequestStop();
    }

    private void OnClosed(object? sender, EventArgs e)
    {
        _timer.Stop();
        _tray?.Dispose();

        // ShutdownMode is OnExplicitShutdown, so closing the last window does not
        // end the process by itself.  Without this the "exit" command would hide
        // the window, dispose the tray icon, and leave a windowless process
        // running -- which would then also block the next start, because the
        // single-instance mutex is still held.
        Application.Current.Shutdown();
    }

    private void RestoreFromTray()
    {
        Show();
        WindowState = WindowState.Normal;
        Activate();
    }

    private void ExitApplication()
    {
        _exitRequested = true;
        Close();
    }

    // -----------------------------------------------------------------------

    private static Brush Brush(string hex)
    {
        var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex));
        brush.Freeze();
        return brush;
    }

    private sealed record CurrencyCard(Currency Currency, Border Container, TextBlock Balance, TodayFigures Today);

    /// <summary>The two value lines under a card's "今日" heading, always written together.</summary>
    private sealed record TodayFigures(TextBlock Income, TextBlock Expense);

    private sealed record RangeChoice(string Label, int Days)
    {
        public override string ToString() => Label;
    }
}

/// <summary>One row of the activity list, shaped for the GridView bindings.</summary>
public sealed class ActivityRow
{
    public required string TimeText { get; init; }

    public required string CurrencyText { get; init; }

    public required string DeltaText { get; init; }

    public required string BalanceText { get; init; }
}
