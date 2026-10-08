namespace RyukinLedger.App.Model;

/// <summary>
/// The 原粹树脂 clock, as the window needs it.
/// </summary>
/// <remarks>
/// <para>
/// The core does the hard part: deciding, from the packets, what the value was
/// and when the game was last seen adding a point (see the core's <c>resin</c>
/// module).  What is left here is the arithmetic on top of it.  The game sends
/// nothing while resin regenerates, so the number in <c>status.json</c> is only
/// right at the instant it was written and everything after that is this
/// function -- which is also what keeps the number moving while no core is
/// running at all.
/// </para>
/// <para>
/// The cap is the one fact about the current game this file states: resin stops
/// regenerating there, and the cap has moved before (160 until it was raised), so
/// a stale number would show up as a value that keeps climbing past what the
/// game shows.
/// </para>
/// </remarks>
public static class ResinClock
{
    /// <summary>Seconds per point of resin.</summary>
    public const int TickSeconds = 8 * 60;

    /// <summary>Where regeneration stops: the cap the game applies.</summary>
    public const int Cap = 200;

    /// <summary>
    /// The value at <paramref name="now"/>, given one that was right at
    /// <paramref name="from"/>, and -- once the game has been seen adding a
    /// point -- the instant of that point.
    /// </summary>
    /// <remarks>
    /// With <paramref name="increasedAt"/> the points that have landed since the
    /// report are the eight-minute boundaries between the two instants: a
    /// subtraction, so the answer is exact and stays exact however long ago the
    /// report was.  Without it the phase of the game's window is unknown -- the
    /// next point is somewhere in the eight minutes after the report -- so
    /// counting from the report is at most one point low.
    /// </remarks>
    public static int At(int value, DateTimeOffset from, DateTimeOffset? increasedAt, DateTimeOffset now)
    {
        // Already at or past the cap: nothing regenerates, and an overshoot --
        // a transient resin can put it there -- is left alone rather than grown.
        if (value >= Cap)
        {
            return value;
        }

        long points = increasedAt is { } anchor
            ? Math.Max(0, Points(anchor, now) - Points(anchor, from))
            : Points(from, now);

        return (int)Math.Min(value + points, Cap);
    }

    /// <summary>How many eight-minute boundaries fall between two instants.</summary>
    private static long Points(DateTimeOffset from, DateTimeOffset to) =>
        Math.Max(0, (long)(to - from).TotalSeconds) / TickSeconds;
}
