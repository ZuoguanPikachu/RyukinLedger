using System.IO;
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
/// illustration is a 2480x3508 JPEG of several megabytes, and turning that into
/// an embedded resource would put all of it into the DLL for no benefit;
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
    /// The illustration is drawn into a column roughly 540 px wide.  Decoding at
    /// 1200 covers a 200% display without decoding the full 2480 px original
    /// (which would be a 35 MB bitmap for a background).
    /// </summary>
    private const int BackgroundDecodeWidth = 1200;

    private static readonly object Gate = new();
    private static readonly Dictionary<Currency, BitmapImage?> Icons = [];
    private static bool _backgroundProbed;
    private static BitmapImage? _background;
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
    /// size Windows asks for -- including a 16 px frame whose shapes were
    /// simplified for it.  Handing the renderer the 256 px frame and letting it
    /// scale would throw all of that away, and at 16 px that difference is the
    /// entire legibility of the icon, so the nearest frame is chosen here.
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
    public static BitmapImage? Background()
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
            foreach (string name in new[] { "yoimiya.jpg", "background.jpg", "background.png" })
            {
                string candidate = Path.Combine(Root, name);
                if (File.Exists(candidate))
                {
                    _background = Decode(candidate, BackgroundDecodeWidth);
                    break;
                }
            }

            return _background;
        }
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
