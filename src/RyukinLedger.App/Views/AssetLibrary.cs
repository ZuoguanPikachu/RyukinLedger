using System.IO;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using RyukinLedger.App.Model;

namespace RyukinLedger.App.Views;

/// <summary>
/// Loads the optional artwork that lives in the <c>assets</c> folder next to the
/// executable.
/// </summary>
/// <remarks>
/// <para>
/// The artwork is deliberately not embedded in the assembly.  The background
/// illustration is a 1254x1254 PNG of a bit over a megabyte, and turning that
/// into an embedded resource would put all of it into the DLL for no benefit;
/// shipping it loose also means the picture can be replaced without a rebuild.
/// </para>
/// <para>
/// Everything here is optional.  A missing folder, a missing file, or a file
/// that fails to decode all end in <c>null</c>, and the interface falls back to
/// a plain look rather than refusing to start over a decoration.
/// </para>
/// </remarks>
internal static class AssetLibrary
{
    /// <summary>
    /// Currency icons are drawn at about 28 px.  They are decoded well above
    /// that and then reduced by the renderer with
    /// <c>BitmapScalingMode.HighQuality</c>: decoding straight to the display
    /// size makes the 192 px original's fine detail alias, which on these thin,
    /// high-contrast icons shows up as a jagged outline.
    /// </summary>
    private const int IconDecodeWidth = 128;

    /// <summary>
    /// The illustration is drawn into a 520 px box in the corner, so this cap
    /// costs it nothing: 1200 covers a 200% display of that box twice over.  It
    /// stays generous rather than tight because both the box and the picture are
    /// replaceable, and the picture before this one was stretched across the
    /// whole window, where 1200 was the least that still looked sharp (that file
    /// was 2480x3508, a 35 MB bitmap decoded whole).
    /// </summary>
    private const int BackgroundDecodeWidth = 1200;

    /// <summary>
    /// How much of the canvas the picture itself covers.  The rest is padding,
    /// painted with the picture's own edge colour.
    /// </summary>
    /// <remarks>
    /// <para>
    /// The window is wider than it is tall (1180x930 by default) and the artwork
    /// is square, so stretching it to fill the window at 1:1 magnifies the
    /// character until its face covers half the window and its eyes sit right
    /// behind the chart.  0.75 draws it at about 885 px, which reads as a
    /// background rather than a poster.
    /// </para>
    /// <para>
    /// The padding is what makes this work without a visible frame.  The window
    /// keeps a light theme precisely so the artwork can bleed to all four edges;
    /// a picture shrunk inside the window would draw a rectangle instead, and
    /// the join has to be invisible for the illusion to hold.
    /// </para>
    /// </remarks>
    private const double BackgroundArtScale = 0.75;

    private static readonly object Gate = new();
    private static readonly Dictionary<Currency, BitmapImage?> Icons = [];
    private static bool _backgroundProbed;
    private static BitmapSource? _background;
    private static bool _appIconProbed;
    private static BitmapSource? _appIcon;

    /// <summary>Where the artwork is expected to be.</summary>
    public static string Root { get; } = Path.Combine(AppContext.BaseDirectory, "assets");

    /// <summary>
    /// The application icon, for the title bar and the task bar.
    /// </summary>
    /// <remarks>
    /// <para>
    /// This reads <c>assets\app.ico</c>, which holds a frame drawn for every
    /// size Windows asks for.  Handing the renderer the 256 px frame and letting
    /// it scale would throw all of that away, and at 16 px that difference is
    /// the entire legibility of the icon, so the nearest frame is chosen here.
    /// </para>
    /// <para>
    /// 32 px is the default because it is the size the shell asks for when it
    /// draws a task bar button, and the title bar's 16 px is then a clean
    /// halving of it.
    /// </para>
    /// </remarks>
    public static BitmapSource? AppIcon(int preferredSize = 32)
    {
        lock (Gate)
        {
            if (_appIconProbed)
            {
                return _appIcon;
            }

            _appIconProbed = true;
            string path = Path.Combine(Root, "app.ico");
            if (!File.Exists(path))
            {
                return null;
            }

            try
            {
                var decoder = new IconBitmapDecoder(
                    new Uri(path),
                    BitmapCreateOptions.None,
                    // OnLoad reads the frames now, so the file is not left open.
                    BitmapCacheOption.OnLoad);

                BitmapFrame? best = null;
                foreach (BitmapFrame frame in decoder.Frames)
                {
                    if (best is null)
                    {
                        best = frame;
                        continue;
                    }

                    // Prefer the smallest frame that is still at least the size
                    // asked for; downscaling looks better than upscaling, and if
                    // nothing is big enough the largest frame wins.
                    bool frameIsBigEnough = frame.PixelWidth >= preferredSize;
                    bool bestIsBigEnough = best.PixelWidth >= preferredSize;
                    if (frameIsBigEnough != bestIsBigEnough)
                    {
                        if (frameIsBigEnough)
                        {
                            best = frame;
                        }
                    }
                    else if (frameIsBigEnough
                        ? frame.PixelWidth < best.PixelWidth
                        : frame.PixelWidth > best.PixelWidth)
                    {
                        best = frame;
                    }
                }

                _appIcon = best;
                return _appIcon;
            }
            catch (Exception)
            {
                return null;
            }
        }
    }

    /// <summary>Icon for a currency, or <c>null</c> when there is no artwork for it.</summary>
    public static BitmapImage? CurrencyIcon(Currency currency)
    {
        lock (Gate)
        {
            if (Icons.TryGetValue(currency, out BitmapImage? cached))
            {
                return cached;
            }

            BitmapImage? loaded = Decode(FindCurrencyIcon(currency), IconDecodeWidth);
            Icons[currency] = loaded;
            return loaded;
        }
    }

    /// <summary>The background illustration, or <c>null</c> when there is none.</summary>
    public static BitmapSource? Background()
    {
        lock (Gate)
        {
            if (_backgroundProbed)
            {
                return _background;
            }

            _backgroundProbed = true;

            // Named after the artwork it currently is, with generic names accepted
            // so swapping the picture does not require a code change.
            foreach (string name in new[] { "yoimiya.png", "yoimiya.jpg", "background.jpg", "background.png" })
            {
                string candidate = Path.Combine(Root, name);
                if (File.Exists(candidate))
                {
                    BitmapImage? decoded = Decode(candidate, BackgroundDecodeWidth);
                    _background = decoded is null ? null : Frame(decoded);
                    break;
                }
            }

            return _background;
        }
    }

    /// <summary>
    /// Draws the picture centred on a larger canvas, so that stretching the
    /// result to fill the window shows the picture smaller than life size.
    /// </summary>
    private static BitmapSource Frame(BitmapSource art)
    {
        int side = (int)Math.Round(Math.Max(art.PixelWidth, art.PixelHeight) / BackgroundArtScale);
        var brush = new SolidColorBrush(EdgeColour(art));

        var visual = new DrawingVisual();
        using (DrawingContext context = visual.RenderOpen())
        {
            context.DrawRectangle(brush, null, new Rect(0, 0, side, side));
            context.DrawImage(
                art,
                new Rect(
                    (side - art.Width) / 2.0,
                    (side - art.Height) / 2.0,
                    art.Width,
                    art.Height));
        }

        var canvas = new RenderTargetBitmap(side, side, 96, 96, PixelFormats.Pbgra32);
        canvas.Render(visual);
        canvas.Freeze();
        return canvas;
    }

    /// <summary>
    /// The average colour along the picture's four edges -- what the padding has
    /// to match for the join to disappear.
    /// </summary>
    private static Color EdgeColour(BitmapSource art)
    {
        var pixels = new FormatConvertedBitmap(art, PixelFormats.Bgra32, null, 0);
        int width = pixels.PixelWidth;
        int height = pixels.PixelHeight;

        long red = 0;
        long green = 0;
        long blue = 0;
        int count = 0;

        void Accumulate(byte[] block)
        {
            for (int i = 0; i + 3 < block.Length; i += 4)
            {
                blue += block[i];
                green += block[i + 1];
                red += block[i + 2];
                count++;
            }
        }

        var row = new byte[width * 4];
        pixels.CopyPixels(new Int32Rect(0, 0, width, 1), row, width * 4, 0);
        Accumulate(row);
        pixels.CopyPixels(new Int32Rect(0, height - 1, width, 1), row, width * 4, 0);
        Accumulate(row);

        var column = new byte[height * 4];
        pixels.CopyPixels(new Int32Rect(0, 0, 1, height), column, 4, 0);
        Accumulate(column);
        pixels.CopyPixels(new Int32Rect(width - 1, 0, 1, height), column, 4, 0);
        Accumulate(column);

        return Color.FromRgb(
            (byte)(red / count),
            (byte)(green / count),
            (byte)(blue / count));
    }

    private static string? FindCurrencyIcon(Currency currency)
    {
        // The files are named after the in-game names, which is what a person
        // dropping artwork in will use; the ledger key is accepted as well so
        // either naming works.
        foreach (string stem in new[] { currency.DisplayName(), currency.Key() })
        {
            foreach (string extension in new[] { ".png", ".jpg" })
            {
                string candidate = Path.Combine(Root, stem + extension);
                if (File.Exists(candidate))
                {
                    return candidate;
                }
            }
        }

        return null;
    }

    private static BitmapImage? Decode(string? path, int decodeWidth)
    {
        if (path is null)
        {
            return null;
        }

        try
        {
            var image = new BitmapImage();
            image.BeginInit();
            image.UriSource = new Uri(path);
            // OnLoad reads the bytes now, so the file is not left open or locked.
            image.CacheOption = BitmapCacheOption.OnLoad;
            image.DecodePixelWidth = decodeWidth;
            image.EndInit();
            image.Freeze();
            return image;
        }
        catch (Exception)
        {
            return null;
        }
    }
}
