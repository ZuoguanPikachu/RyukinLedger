using System.Globalization;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;

namespace RyukinLedger.App.Views;

/// <summary>One day's numbers for the currency the chart is showing.</summary>
public sealed record DailyPoint(DateOnly Day, long Income, long Expense, long Balance, bool HasBalance);

/// <summary>
/// A day-by-day income/expense chart with a separate balance band underneath.
/// </summary>
/// <remarks>
/// <para>
/// Drawn directly with <see cref="DrawingContext"/> on purpose.  A charting
/// library would add DLLs to the shipped folder and a package reference to keep
/// current, in exchange for a chart this simple; the ledger only ever needs
/// daily bars plus a balance trend.
/// </para>
/// <para>
/// Income and expense are drawn as separate bars, not netted, because the core
/// records them as separate signed events: a day that earned and spent the same
/// amount is not a day with no activity.
/// </para>
/// <para>
/// The balance line lives in its own band with its own scale rather than
/// sharing an axis with the bars.  A dual axis is the usual way to make a chart
/// that looks informative and misleads; two bands with labelled extremes are
/// honest about the fact that the two series are different scales.
/// </para>
/// </remarks>
public sealed class DailyChart : FrameworkElement
{
    // Geometry.
    private const double PadLeft = 72;
    private const double PadRight = 16;
    private const double PadTop = 16;
    private const double PadBottom = 40;
    private const double BandGap = 16;
    private const double BarAreaShare = 0.64;

    // Colours, kept next to the drawing code so the chart is self-contained.
    // Warm-neutral, so the chart sits in the same palette as the artwork.
    //
    // The grid and axis colours are deliberately a step darker than they would
    // need to be on a flat white card: the plot is translucent now, so these
    // lines sit over whatever the illustration puts behind them.
    private static readonly Brush IncomeBrush = Frozen("#3E9E7E");
    private static readonly Brush ExpenseBrush = Frozen("#DE5B4B");
    private static readonly Brush GridBrush = Frozen("#DED2C6");
    private static readonly Brush AxisBrush = Frozen("#8C8074");
    private static readonly Brush ZeroBrush = Frozen("#C9BCAE");
    private static readonly Brush SurfaceBrush = Frozen("#A8FFFFFF");
    private static readonly Brush MutedTextBrush = Frozen("#8C8074");
    private static readonly Brush HoverBrush = Frozen("#2B2521");

    private IReadOnlyList<DailyPoint> _points = [];
    private Brush _accent = Frozen("#4A90D9");
    private string _seriesLabel = string.Empty;
    private int _hoverIndex = -1;

    public DailyChart()
    {
        SnapsToDevicePixels = true;
    }

    /// <summary>Oldest day first; the most recent day is drawn on the right.</summary>
    public IReadOnlyList<DailyPoint> Points
    {
        get => _points;
        set
        {
            _points = value ?? [];
            _hoverIndex = -1;
            InvalidateVisual();
        }
    }

    public Brush Accent
    {
        get => _accent;
        set
        {
            _accent = value ?? Frozen("#4A90D9");
            InvalidateVisual();
        }
    }

    /// <summary>Currency name, used in the hover readout.</summary>
    public string SeriesLabel
    {
        get => _seriesLabel;
        set
        {
            _seriesLabel = value ?? string.Empty;
            InvalidateVisual();
        }
    }

    protected override void OnRender(DrawingContext dc)
    {
        double width = ActualWidth;
        double height = ActualHeight;
        if (width <= PadLeft + PadRight + 20 || height <= PadTop + PadBottom + 60)
        {
            return;
        }

        // Hit-testable background so the hover readout works.
        dc.DrawRectangle(SurfaceBrush, null, new Rect(0, 0, width, height));

        Rect plot = new(PadLeft, PadTop, width - PadLeft - PadRight, height - PadTop - PadBottom);
        double stackedHeight = plot.Height - BandGap;
        Rect barArea = new(plot.X, plot.Y, plot.Width, stackedHeight * BarAreaShare);
        Rect balanceArea = new(plot.X, barArea.Bottom + BandGap, plot.Width, stackedHeight * (1 - BarAreaShare));

        if (_points.Count == 0)
        {
            DrawCentred(dc, "还没有数据。开始记录后，这里会按天显示收支。", plot, MutedTextBrush, 13);
            return;
        }

        DrawBars(dc, barArea);
        DrawBalance(dc, balanceArea);
        DrawDayLabels(dc, plot, barArea, balanceArea);
        DrawHover(dc, plot, barArea, balanceArea);
    }

    private void DrawBars(DrawingContext dc, Rect area)
    {
        long maxIncome = 0;
        long maxExpense = 0;
        foreach (DailyPoint point in _points)
        {
            maxIncome = Math.Max(maxIncome, point.Income);
            maxExpense = Math.Max(maxExpense, point.Expense);
        }

        long total = maxIncome + maxExpense;
        bool hasData = total > 0;
        if (!hasData)
        {
            total = 1;
            maxIncome = 1;
        }

        // Zero sits where the two halves meet, so neither direction wastes space.
        double zeroY = area.Y + (area.Height * maxIncome / total);
        double incomeScale = maxIncome > 0 ? (zeroY - area.Y) / maxIncome : 0;
        double expenseScale = maxExpense > 0 ? (area.Bottom - zeroY) / maxExpense : 0;

        // Horizontal rules at the three meaningful heights.
        foreach (double y in new[] { area.Y, zeroY, area.Bottom })
        {
            dc.DrawLine(y == zeroY ? new Pen(ZeroBrush, 1) : new Pen(GridBrush, 1),
                new Point(area.X, y), new Point(area.Right, y));
        }

        // An axis labelled 0 / 0 / 1 over an empty range is noise, so leave the
        // scale off until there is something to scale.
        if (hasData)
        {
            DrawRightAligned(dc, Format(maxIncome), area.X - 8, area.Y - 7, AxisBrush, 11);
            DrawRightAligned(dc, "0", area.X - 8, zeroY - 7, AxisBrush, 11);
            DrawRightAligned(dc, Format(maxExpense), area.X - 8, area.Bottom - 7, AxisBrush, 11);
        }

        double slot = area.Width / _points.Count;
        double barWidth = Math.Max(2, Math.Min(slot * 0.62, 30));

        for (int i = 0; i < _points.Count; i++)
        {
            DailyPoint point = _points[i];
            double centre = area.X + (slot * i) + (slot / 2);
            double left = centre - (barWidth / 2);

            if (point.Income > 0)
            {
                double h = point.Income * incomeScale;
                dc.DrawRectangle(IncomeBrush, null, new Rect(left, zeroY - h, barWidth, h));
            }

            if (point.Expense > 0)
            {
                double h = point.Expense * expenseScale;
                dc.DrawRectangle(ExpenseBrush, null, new Rect(left, zeroY, barWidth, h));
            }
        }

        // Legend, top-left of the bar area, once there is something to name.
        if (hasData)
        {
            DrawLegend(dc, area);
        }
    }

    private void DrawLegend(DrawingContext dc, Rect area)
    {
        double x = area.X + 4;
        double y = area.Y + 2;

        dc.DrawRectangle(IncomeBrush, null, new Rect(x, y + 3, 9, 9));
        FormattedText income = Label("收入", 11, MutedTextBrush);
        dc.DrawText(income, new Point(x + 14, y));
        x += 14 + income.Width + 16;

        dc.DrawRectangle(ExpenseBrush, null, new Rect(x, y + 3, 9, 9));
        dc.DrawText(Label("支出", 11, MutedTextBrush), new Point(x + 14, y));
    }

    private void DrawBalance(DrawingContext dc, Rect area)
    {
        var withBalance = new List<(int Index, long Value)>();
        for (int i = 0; i < _points.Count; i++)
        {
            if (_points[i].HasBalance)
            {
                withBalance.Add((i, _points[i].Balance));
            }
        }

        dc.DrawLine(new Pen(GridBrush, 1), new Point(area.X, area.Y), new Point(area.Right, area.Y));
        dc.DrawLine(new Pen(GridBrush, 1), new Point(area.X, area.Bottom), new Point(area.Right, area.Bottom));

        if (withBalance.Count == 0)
        {
            DrawCentred(dc, "余额还没有记录", area, AxisBrush, 11);
            return;
        }

        long min = withBalance.Min(entry => entry.Value);
        long max = withBalance.Max(entry => entry.Value);

        // A balance that never moved still needs a band to draw in, so the range
        // is widened around it -- but the labels must not then claim the balance
        // ranged over that widened span.  One value reads as that value at both
        // ends, which is what a flat line means.
        long flat = min;
        bool isFlat = min == max;
        if (isFlat)
        {
            min -= 1;
            max += 1;
        }

        double slot = area.Width / _points.Count;
        double innerPad = 10;
        double top = area.Y + innerPad;
        double usable = Math.Max(1, area.Height - (innerPad * 2));

        Point Position(int index, long value)
        {
            double x = area.X + (slot * index) + (slot / 2);
            double y = top + (usable * (max - value) / (double)(max - min));
            return new Point(x, y);
        }

        var geometry = new StreamGeometry();
        using (StreamGeometryContext ctx = geometry.Open())
        {
            ctx.BeginFigure(Position(withBalance[0].Index, withBalance[0].Value), isFilled: false, isClosed: false);
            for (int i = 1; i < withBalance.Count; i++)
            {
                ctx.LineTo(Position(withBalance[i].Index, withBalance[i].Value), isStroked: true, isSmoothJoin: true);
            }
        }

        geometry.Freeze();
        dc.DrawGeometry(null, new Pen(_accent, 1.8), geometry);

        if (withBalance.Count == 1)
        {
            Point only = Position(withBalance[0].Index, withBalance[0].Value);
            dc.DrawEllipse(_accent, null, only, 3, 3);
        }

        DrawRightAligned(dc, Format(isFlat ? flat : max), area.X - 8, area.Y + 2, AxisBrush, 11);
        DrawRightAligned(dc, Format(isFlat ? flat : min), area.X - 8, area.Bottom - 14, AxisBrush, 11);

        FormattedText caption = Label("余额", 11, MutedTextBrush);
        dc.DrawText(caption, new Point(area.X + 4, area.Y + 2));
    }

    private void DrawDayLabels(DrawingContext dc, Rect plot, Rect barArea, Rect balanceArea)
    {
        double slot = plot.Width / _points.Count;
        int step = Math.Max(1, (int)Math.Ceiling(_points.Count / Math.Max(1, plot.Width / 56)));
        double y = balanceArea.Bottom + 8;

        for (int i = 0; i < _points.Count; i += step)
        {
            FormattedText text = Label(_points[i].Day.ToString("MM-dd", CultureInfo.InvariantCulture), 11, AxisBrush);
            double centre = plot.X + (slot * i) + (slot / 2);
            dc.DrawText(text, new Point(centre - (text.Width / 2), y));
        }
    }

    private void DrawHover(DrawingContext dc, Rect plot, Rect barArea, Rect balanceArea)
    {
        if (_hoverIndex < 0 || _hoverIndex >= _points.Count)
        {
            return;
        }

        DailyPoint point = _points[_hoverIndex];
        double slot = plot.Width / _points.Count;
        double centre = plot.X + (slot * _hoverIndex) + (slot / 2);

        dc.DrawLine(new Pen(ZeroBrush, 1) { DashStyle = DashStyles.Dash }, new Point(centre, barArea.Y), new Point(centre, balanceArea.Bottom));

        string line1 = $"{point.Day:MM-dd}  {_seriesLabel}";
        string line2 = $"收入 {Format(point.Income)}";
        string line3 = $"支出 {Format(point.Expense)}";
        string line4 = point.HasBalance ? $"余额 {Format(point.Balance)}" : "余额 未知";

        FormattedText[] texts =
        [
            Label(line1, 11.5, HoverBrush, FontWeights.SemiBold),
            Label(line2, 11, IncomeBrush),
            Label(line3, 11, ExpenseBrush),
            Label(line4, 11, MutedTextBrush),
        ];

        double boxWidth = texts.Max(t => t.Width) + 18;
        double boxHeight = texts.Sum(t => t.Height) + 14;
        double boxX = Math.Min(Math.Max(centre + 10, plot.X), plot.Right - boxWidth);
        double boxY = barArea.Y + 6;

        var box = new Rect(boxX, boxY, boxWidth, boxHeight);
        dc.DrawRoundedRectangle(Frozen("#FDFAF6"), new Pen(GridBrush, 1), box, 6, 6);

        double textY = boxY + 7;
        foreach (FormattedText text in texts)
        {
            dc.DrawText(text, new Point(boxX + 9, textY));
            textY += text.Height;
        }
    }

    protected override void OnMouseMove(MouseEventArgs e)
    {
        base.OnMouseMove(e);
        UpdateHover(e.GetPosition(this));
    }

    protected override void OnMouseLeave(MouseEventArgs e)
    {
        base.OnMouseLeave(e);
        if (_hoverIndex != -1)
        {
            _hoverIndex = -1;
            InvalidateVisual();
        }
    }

    private void UpdateHover(Point position)
    {
        int index = -1;
        double width = ActualWidth - PadLeft - PadRight;
        if (_points.Count > 0 && width > 0 && position.X >= PadLeft && position.X <= PadLeft + width)
        {
            double slot = width / _points.Count;
            index = Math.Clamp((int)((position.X - PadLeft) / slot), 0, _points.Count - 1);
        }

        if (index != _hoverIndex)
        {
            _hoverIndex = index;
            InvalidateVisual();
        }
    }

    // -----------------------------------------------------------------------
    // Helpers

    private FormattedText Label(string text, double size, Brush brush, FontWeight? weight = null) =>
        new(
            text,
            CultureInfo.CurrentUICulture,
            FlowDirection.LeftToRight,
            new Typeface(new FontFamily("Segoe UI"), FontStyles.Normal, weight ?? FontWeights.Normal, FontStretches.Normal),
            size,
            brush,
            VisualTreeHelper.GetDpi(this).PixelsPerDip);

    private void DrawRightAligned(DrawingContext dc, string text, double right, double top, Brush brush, double size)
    {
        FormattedText formatted = Label(text, size, brush);
        dc.DrawText(formatted, new Point(right - formatted.Width, top));
    }

    private void DrawCentred(DrawingContext dc, string text, Rect area, Brush brush, double size)
    {
        FormattedText formatted = Label(text, size, brush);
        dc.DrawText(formatted, new Point(area.X + ((area.Width - formatted.Width) / 2), area.Y + ((area.Height - formatted.Height) / 2)));
    }

    /// <summary>Groups digits so six-figure currencies stay readable.</summary>
    public static string Format(long value) => value.ToString("N0", CultureInfo.InvariantCulture);

    private static Brush Frozen(string hex)
    {
        var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex));
        brush.Freeze();
        return brush;
    }
}
