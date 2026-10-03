namespace RyukinLedger.App.Gacha;

/// <summary>Inputs for one wish-probability calculation.</summary>
/// <param name="TargetCount">How many copies of the rate-up 5★ are wanted.</param>
/// <param name="PityCount">Pulls already made since the last 5★ (0-89).</param>
/// <param name="Guaranteed">True when the next 5★ is certain to be the rate-up one.</param>
/// <param name="Fate">Epitomized Path fate points (0-3).</param>
public sealed record GachaParameters(int TargetCount, int PityCount, bool Guaranteed, int Fate);

/// <summary>The distribution of the total number of pulls, and its summaries.</summary>
/// <remarks>
/// <see cref="Pmf"/> and <see cref="Cdf"/> are both indexed by the total number
/// of pulls, so <c>Pmf[n]</c> is the probability of finishing in exactly
/// <c>n</c> pulls and <c>Cdf[n]</c> the probability of finishing within
/// <c>n</c>.  Index 0 is always zero: at least one pull is always needed.
/// </remarks>
public sealed record GachaResult(
    double[] Pmf,
    double[] Cdf,
    double Expectation,
    int P50,
    int P90,
    int P99)
{
    /// <summary>Longest pull count with a non-zero probability.</summary>
    public int MaxPulls => Pmf.Length - 1;

    /// <summary>Probability of finishing within <paramref name="pulls"/>, clamped to the table.</summary>
    public double ProbabilityWithin(int pulls) =>
        pulls < 0 ? 0 : pulls >= Cdf.Length ? 1 : Cdf[pulls];
}

/// <summary>
/// Wish probability for a limited 5★ banner, with 50/50 and Epitomized Path.
/// </summary>
/// <remarks>
/// <para>
/// This is a direct port of a Python implementation that was already in use, so
/// the structure is deliberately kept close to the original: a base distribution
/// for "pulls until the next 5★", a small state machine for the 50/50 and the
/// fate points, and a dynamic program that convolves them together.
/// </para>
/// <para>
/// <b>Verified against the original.</b>  The port was checked element by
/// element against a pure-Python oracle mirroring that implementation, across a
/// grid of parameter sets, with the harness compiling this very file rather
/// than a copy of it.  Porting probability code without checking it against the
/// thing it came from is how a calculator ends up confidently wrong.  The
/// harness itself was a one-off and is no longer in the tree; the algorithm has
/// not changed since.
/// </para>
/// <para>
/// Nothing here touches WPF, so the verification harness can compile this one
/// file on its own.
/// </para>
/// </remarks>
public static class GachaCalculator
{
    /// <summary>Pulls after which a 5★ is certain.</summary>
    public const int HardPity = 90;

    /// <summary>Base 5★ rate before the soft pity ramp.</summary>
    public const double BaseRate = 0.006;

    /// <summary>Added to the rate for every pull past <see cref="SoftPityStart"/>.</summary>
    public const double SoftPityStep = 0.06;

    /// <summary>Last pull that still uses the base rate.</summary>
    public const int SoftPityStart = 73;

    /// <summary>One 5★ per 160 primogems, once fates run out.</summary>
    public const int PrimogemsPerPull = 160;

    /// <summary>State count: guaranteed (2) times fate points 0-3 (4).</summary>
    private const int NumStates = 8;

    /// <summary>Below this the remaining tail is dropped.</summary>
    private const double TailCutoff = 1e-15;

    /// <summary>Probability that the <paramref name="pity"/>-th pull since the last 5★ yields one.</summary>
    public static double FiveStarProbability(int pity)
    {
        if (pity <= SoftPityStart)
        {
            return BaseRate;
        }

        if (pity < HardPity)
        {
            return BaseRate + ((pity - SoftPityStart) * SoftPityStep);
        }

        return 1.0;
    }

    /// <summary>
    /// Distribution of the number of pulls until the next 5★, given the pity
    /// count already accumulated.
    /// </summary>
    public static double[] BasicDistribution(int startPity)
    {
        var probabilities = new List<double> { 0.0 };
        double survive = 1.0;
        int pity = startPity;

        while (survive > TailCutoff)
        {
            pity++;
            double p = FiveStarProbability(pity);
            probabilities.Add(survive * p);
            survive *= 1 - p;

            if (pity >= HardPity)
            {
                break;
            }
        }

        return [.. probabilities];
    }

    /// <summary>Applies one calculation: the PMF, its CDF and the usual quantiles.</summary>
    public static GachaResult Calculate(GachaParameters parameters)
    {
        double[] pmf = LimitedDistribution(parameters);

        var cdf = new double[pmf.Length];
        double running = 0;
        double expectation = 0;

        for (int i = 0; i < pmf.Length; i++)
        {
            running += pmf[i];
            cdf[i] = running;
            expectation += i * pmf[i];
        }

        return new GachaResult(
            pmf,
            cdf,
            expectation,
            Percentile(cdf, 0.50),
            Percentile(cdf, 0.90),
            Percentile(cdf, 0.99));
    }

    /// <summary>
    /// Distribution of the total pulls needed for
    /// <see cref="GachaParameters.TargetCount"/> copies.
    /// </summary>
    /// <remarks>
    /// <c>dp[count][state]</c> holds the distribution of pulls spent so far,
    /// having won <c>count</c> rate-up 5★s and sitting in <c>state</c>.  Each
    /// round convolves in one more 5★.  The number of rounds is capped at
    /// <c>target * 2</c>, which is the worst case: every rate-up copy lost to a
    /// 50/50, plus the one that finally lands.
    /// </remarks>
    private static double[] LimitedDistribution(GachaParameters parameters)
    {
        int targetCount = Math.Max(1, parameters.TargetCount);
        int fate = Math.Clamp(parameters.Fate, 0, 3);

        double[] firstDistribution = BasicDistribution(Math.Clamp(parameters.PityCount, 0, HardPity - 1));
        double[] normalDistribution = BasicDistribution(0);
        List<Transition>[] transitions = BuildTransitions();

        var dp = new double[]?[targetCount][];
        for (int i = 0; i < targetCount; i++)
        {
            dp[i] = new double[]?[NumStates];
        }

        double[]? result = null;
        int initialState = StateId(parameters.Guaranteed, fate);

        foreach (Transition transition in transitions[initialState])
        {
            double[] distribution = Scale(firstDistribution, transition.Probability);

            if (transition.Gain >= targetCount)
            {
                result = AddTo(result, distribution);
            }
            else
            {
                dp[transition.Gain][transition.NextState] =
                    AddTo(dp[transition.Gain][transition.NextState], distribution);
            }
        }

        for (int round = 1; round < targetCount * 2; round++)
        {
            var next = new double[]?[targetCount][];
            for (int i = 0; i < targetCount; i++)
            {
                next[i] = new double[]?[NumStates];
            }

            for (int count = 0; count < targetCount; count++)
            {
                for (int state = 0; state < NumStates; state++)
                {
                    double[]? current = dp[count][state];
                    if (current is null)
                    {
                        continue;
                    }

                    foreach (Transition transition in transitions[state])
                    {
                        double[] convolved = Convolve(current, Scale(normalDistribution, transition.Probability));
                        int newCount = count + transition.Gain;

                        if (newCount >= targetCount)
                        {
                            result = AddTo(result, convolved);
                        }
                        else
                        {
                            next[newCount][transition.NextState] =
                                AddTo(next[newCount][transition.NextState], convolved);
                        }
                    }
                }
            }

            dp = next;
        }

        return result ?? [0.0];
    }

    private readonly record struct Transition(int Gain, int NextState, double Probability);

    private static int StateId(bool guaranteed, int fate) => (guaranteed ? 1 : 0) * 4 + fate;

    /// <summary>
    /// The 50/50 and fate-point state machine.
    /// </summary>
    /// <remarks>
    /// <c>Gain</c> is 1 when the 5★ obtained is the rate-up one and 0 when it is
    /// a standard-banner 5★.  Fate points fall on a win and rise on a loss, and
    /// at three the next 5★ is the rate-up one by rule.
    /// </remarks>
    private static List<Transition>[] BuildTransitions()
    {
        var transitions = new List<Transition>[NumStates];
        for (int i = 0; i < NumStates; i++)
        {
            transitions[i] = [];
        }

        foreach (bool guaranteed in (bool[])[false, true])
        {
            for (int fate = 0; fate < 4; fate++)
            {
                int current = StateId(guaranteed, fate);

                if (guaranteed)
                {
                    transitions[current].Add(new Transition(1, StateId(false, fate), 1.0));
                }
                else if (fate == 3)
                {
                    transitions[current].Add(new Transition(1, StateId(false, 1), 1.0));
                }
                else
                {
                    transitions[current].Add(new Transition(1, StateId(false, Math.Max(fate - 1, 0)), 0.5));
                    transitions[current].Add(new Transition(0, StateId(true, fate + 1), 0.5));
                }
            }
        }

        return transitions;
    }

    private static double[] Scale(double[] distribution, double factor)
    {
        var result = new double[distribution.Length];
        for (int i = 0; i < distribution.Length; i++)
        {
            result[i] = distribution[i] * factor;
        }

        return result;
    }

    /// <summary>Adds two distributions indexed by pull count, padding to the longer one.</summary>
    private static double[] AddTo(double[]? a, double[] b)
    {
        if (a is null)
        {
            return (double[])b.Clone();
        }

        var result = new double[Math.Max(a.Length, b.Length)];
        for (int i = 0; i < a.Length; i++)
        {
            result[i] += a[i];
        }

        for (int i = 0; i < b.Length; i++)
        {
            result[i] += b[i];
        }

        return result;
    }

    private static double[] Convolve(double[] a, double[] b)
    {
        var result = new double[a.Length + b.Length - 1];

        for (int i = 0; i < a.Length; i++)
        {
            if (a[i] == 0)
            {
                continue;
            }

            for (int j = 0; j < b.Length; j++)
            {
                result[i + j] += a[i] * b[j];
            }
        }

        return result;
    }

    /// <summary>
    /// First index whose CDF reaches <paramref name="probability"/> -- the same
    /// answer as <c>numpy.searchsorted(cdf, p)</c>.
    /// </summary>
    private static int Percentile(double[] cdf, double probability)
    {
        for (int i = 0; i < cdf.Length; i++)
        {
            if (cdf[i] >= probability)
            {
                return i;
            }
        }

        return cdf.Length - 1;
    }
}
