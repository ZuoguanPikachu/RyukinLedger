using System.Globalization;
using System.Windows;
using System.Windows.Media;
using RyukinLedger.App.Gacha;

namespace RyukinLedger.App.Views;

/// <summary>
/// Draws a <see cref="GachaResult"/>: the probability mass as an area, the
/// cumulative distribution as a line, and the three quantiles as markers.
/// </summary>
/// <remarks>
/// Hand-drawn like <see cref="DailyChart"/>, and for the same reason: one more
/// chart does not justify another dependency in the shipped folder.
///
/// Both series are drawn because they answer different questions.  The mass
/// curve shows where a single outcome is most likely; the cumulative curve
/// answers "how many pulls do I need to be 90% sure", which is the question
/// people actually have.  Reading only the mass curve is how people conclude
/// that 90 pulls is the typical cost.
///
/// The wish count the user already has is drawn as a fourth, solid marker.  It
/// is the only element on the chart that is a fact rather than a probability,
/// so it is the only one drawn solid and in a colour of its own.
/// </remarks>
public sealed class ProbabilityChart : FrameworkElement
{
    private const double PadLeft = 58;
    private const double PadRight = 18;
    private const double PadTop = 16;
    private const double PadBottom = 38;

    private static readonly Brush MassBrush = Frozen("#3E9E7E");
    private static readonly Brush MassFillBrush = Frozen("#283E9E7E");
    private static readonly Brush LineBrush = Frozen("#D9573F");
    private static readonly Brush GridBrush = Frozen("#E6DACE");
    private static readonly Brush AxisBrush = Frozen("#8C8074");
    private static readonly Brush MarkerBrush = Frozen("#B08040");
    private static readonly Brush CurrentBrush = Frozen("#4A90D9");
    private static readonly Brush TextBrush = Frozen("#2B2521");
    private static readonly Brush SurfaceBrush = Frozen("#40FFFFFF");

    private GachaResult? _result;
    private int? _currentPulls;

    public GachaResult? Result
    {
        get => _result;
        set
        {
            _result = value;
            InvalidateVisual();
        }
    }

    /// <summary>
    /// How many wishes are already affordable, marked on the chart.
    /// </summary>
    /// <remarks>
    /// This is the number the whole window exists to answer: the curves say what
    /// a target costs, and this says where the user already stands against it.
    /// It is not clamped by the caller -- a count beyond the longest possible
    /// run is drawn at the right edge, where the cumulative curve has already
    /// reached 100%, which is the honest reading.
    /// </remarks>
    public int? CurrentPulls
    {
        get => _currentPulls;
        set
        {
            _currentPulls = value;
            InvalidateVisual();
        }
    }

    protected override void OnRender(DrawingContext dc)
    {
        double width = ActualWidth;
        double height = ActualHeight;
        if (width <= PadLeft + PadRight + 40 || height <= PadTop + PadBottom + 40)
        {
            return;
        }

        dc.DrawRectangle(SurfaceBrush, null, new Rect(0, 0, width, height));

        Rect plot = new(PadLeft, PadTop, width - PadLeft - PadRight, height - PadTop - PadBottom);

        if (_result is null || _result.Pmf.Length < 2)
        {
            DrawCentred(dc, "设置参数后这里会显示概率分布。", plot, AxisBrush, 12);
            return;
        }

        GachaResult result = _result;
        int maxPulls = result.MaxPulls;

        // Horizontal grid at 0 / 25 / 50 / 75 / 100%.
        for (int step = 0; step <= 4; step++)
        {
            double fraction = step / 4.0;
            double y = plot.Bottom - (plot.Height * fraction);
            dc.DrawLine(new Pen(GridBrush, 1), new Point(plot.X, y), new Point(plot.Right, y));
            DrawRightAligned(dc, $"{fraction * 100:0}%", plot.X - 8, y - 7, AxisBrush, 11);
        }

        double X(int pulls) => plot.X + (plot.Width * pulls / maxPulls);
        double Y(double probability) => plot.Bottom - (plot.Height * Math.Clamp(probability, 0, 1));

        // Probability mass as a filled area.  Scaled so the tallest bar reaches
        // the top, otherwise a long tail flattens the interesting part.
        double peak = result.Pmf.Max();        if (peak > 0)
        {
            var mass = new StreamGeometry();
            using (StreamGeometryContext ctx = mass.Open())
            {
                ctx.BeginFigure(new Point(X(0), plot.Bottom), isFilled: true, isClosed: true);
                for (int pulls = 0; pulls <= maxPulls; pulls++)
                {
                    ctx.LineTo(new Point(X(pulls), Y(result.Pmf[pulls] / peak)), isStroked: false, isSmoothJoin: false);
                }

                ctx.LineTo(new Point(X(maxPulls), plot.Bottom), isStroked: false, isSmoothJoin: false);
            }

            mass.Freeze();
            dc.DrawGeometry(MassFillBrush, new Pen(MassBrush, 1), mass);
        }

        // Cumulative distribution.
        var cdf = new StreamGeometry();
        using (StreamGeometryContext ctx = cdf.Open())
        {
            ctx.BeginFigure(new Point(X(0), Y(result.Cdf[0])), isFilled: false, isClosed: false);
            for (int pulls = 1; pulls <= maxPulls; pulls++)
            {
                ctx.LineTo(new Point(X(pulls), Y(result.Cdf[pulls])), isStroked: true, isSmoothJoin: true);
            }
        }

        cdf.Freeze();
        dc.DrawGeometry(null, new Pen(LineBrush, 2), cdf);

        // Labels are placed in order and pushed up when they would collide: the
        // 90% and 99% markers often land only a few pulls apart, and their labels
        // then overlap into one unreadable string.
        var taken = new List<Rect>();

        DrawQuantile(dc, plot, "50%", result.P50, result.Cdf[result.P50], X, Y, taken);
        DrawQuantile(dc, plot, "90%", result.P90, result.Cdf[result.P90], X, Y, taken);
        DrawQuantile(dc, plot, "99%", result.P99, result.Cdf[result.P99], X, Y, taken);

        if (_currentPulls is { } current)
        {
            DrawCurrent(dc, plot, result, current, X, Y, taken);
        }

        // X axis: a handful of tick labels.
        int tickStep = Math.Max(1, maxPulls / 10);
        for (int pulls = 0; pulls <= maxPulls; pulls += tickStep)
        {
            FormattedText label = Label(pulls.ToString(CultureInfo.InvariantCulture), 11, AxisBrush);
            dc.DrawText(label, new Point(X(pulls) - (label.Width / 2), plot.Bottom + 8));
        }

        FormattedText caption = Label("累计概率 / 单点概率密度", 11, AxisBrush);
        dc.DrawText(caption, new Point(plot.X + 4, plot.Y + 2));
    }

    private void DrawQuantile(
        DrawingContext dc,
        Rect plot,
        string name,
        int pulls,
        double cumulative,
        Func<int, double> x,
        Func<double, double> y,
        List<Rect> taken)
    {
        double position = x(pulls);
        dc.DrawLine(
            new Pen(MarkerBrush, 1) { DashStyle = DashStyles.Dash },
            new Point(position, plot.Y),
            new Point(position, plot.Bottom));

        FormattedText label = Label($"{name} {pulls}抽", 11, MarkerBrush, FontWeights.SemiBold);
        double left = Math.Min(Math.Max(position - (label.Width / 2), plot.X), plot.Right - label.Width);

        var box = new Rect(left, plot.Bottom - 16, label.Width, label.Height);
        while (taken.Any(other => other.IntersectsWith(box)))
        {
            box.Y -= label.Height + 2;
        }

        taken.Add(box);
        dc.DrawText(label, new Point(box.X, box.Y));

        // The dot sits on the curve, not at a fixed height: the point of the
        // marker is to show where that quantile actually falls.
        dc.DrawEllipse(MarkerBrush, null, new Point(position, y(cumulative)), 3, 3);
    }

    /// <summary>
    /// Marks the wish count the user already has: a solid line, a dot on the
    /// cumulative curve, and the chance that count is already enough.
    /// </summary>
    /// <remarks>
    /// Drawn after the quantiles so it takes part in the same collision
    /// avoidance, and in a different colour and weight so it never reads as one
    /// more quantile.
    /// </remarks>
    private void DrawCurrent(
        DrawingContext dc,
        Rect plot,
        GachaResult result,
        int pulls,
        Func<int, double> x,
        Func<double, double> y,
        List<Rect> taken)
    {
        int position = Math.Clamp(pulls, 0, result.MaxPulls);
        double cumulative = result.ProbabilityWithin(pulls);
        double line = x(position);

        dc.DrawLine(new Pen(CurrentBrush, 1.6), new Point(line, plot.Y), new Point(line, plot.Bottom));

        FormattedText label = Label($"当前 {pulls} 抽 · {cumulative:P0}", 11, CurrentBrush, FontWeights.SemiBold);
        double left = Math.Min(Math.Max(line - (label.Width / 2), plot.X), plot.Right - label.Width);

        var box = new Rect(left, plot.Bottom - 16, label.Width, label.Height);
        while (taken.Any(other => other.IntersectsWith(box)))
        {
            box.Y -= label.Height + 2;
        }

        taken.Add(box);
        dc.DrawText(label, new Point(box.X, box.Y));

        dc.DrawEllipse(CurrentBrush, null, new Point(line, y(cumulative)), 3.5, 3.5);
    }

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
        dc.DrawText(formatted, new Point(
            area.X + ((area.Width - formatted.Width) / 2),
            area.Y + ((area.Height - formatted.Height) / 2)));
    }

    private static Brush Frozen(string hex)
    {
        var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex));
        brush.Freeze();
        return brush;
    }
}
