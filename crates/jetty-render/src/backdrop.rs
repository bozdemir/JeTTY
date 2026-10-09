//! Backdrop — the background layer under the terminal grid (visuals v2, slice E).
//!
//! `[backdrop] mode = "none"` (the default) builds NOTHING: the app keeps an
//! `Option<Backdrop>` that stays `None`, so the frame is exactly today's clear
//! (no pipeline, no buffer, no per-frame work beyond one mode check). Any other
//! mode draws ONE full-screen triangle as the first draw of the existing grid
//! pass — after the clear, before the cell-background quads — so there is no
//! extra render pass or submit. The draw REPLACEs every pixel with the
//! backdrop at alpha = the window opacity, premultiplied exactly like the clear
//! (`GpuContext::premultiply_clear`; macOS/Metal is PostMultiplied).
//!
//! Everything after the grid pass composes on top unchanged: the chrome strips
//! are opaque, the corner mask rounds the backdrop with the window, and with CRT
//! on the backdrop lands in the offscreen scene so the CRT pass warps it along
//! with the text. The dropdown slide and `parallax` shift it through `offset`.
//!
//! Color work happens in OKLab (the gradient stops and the theme bg are mixed
//! there, so blends stay perceptually even), output is dithered with a
//! triangular ±1 code-step noise in the sRGB-encoded domain — an 8-bit dark
//! gradient bands visibly without it (mandatory, always on) — and the optional
//! `grain` adds a monochrome film grain on top.
//!
//! Every look is BAKED by its own shader into a window-sized cache when an
//! input changes, and a frame only copies the cache (see [`Backdrop`]). The
//! bake variants, one pipeline each, built lazily for the mode in use only:
//! * gradient — the base look: 2–4 stops, linear (CSS angle) or radial;
//! * image — a decoded image (see `backdrop_image.rs`) over the base, with
//!   fit, dim (+ smart dim) and a frosted (blurred) variant;
//! * stars / grid / synthwave — procedural patterns over the base;
//! * baked — the aurora, whose noise first renders into a half-resolution
//!   layer that the bake samples.
//!
//! A readability guard in every bake but the stars keeps the backdrop at the
//! theme text's readable contrast (`readable_ratio`), whatever it shows.
//!
//! Self-contained: our own wgpu/WGSL; no desktop-environment / OS-specific code.

use std::sync::Arc;

use crate::backdrop_image::DecodedImage;

// ── Settings ─────────────────────────────────────────────────────────────────

/// `[backdrop] mode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackdropMode {
    /// No backdrop: nothing is built or drawn (today's look).
    #[default]
    None,
    /// The curated look for the active theme (`theme_look`).
    Theme,
    /// A gradient from `colors` (or colors derived from the theme).
    Gradient,
    /// An image file over the base gradient.
    Image,
    /// A procedural pattern over the base gradient.
    Pattern,
}

impl BackdropMode {
    /// Lenient parse: unknown values are `None` (one typo never fails a load).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "theme" => Self::Theme,
            "gradient" => Self::Gradient,
            "image" => Self::Image,
            "pattern" => Self::Pattern,
            _ => Self::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Theme => "theme",
            Self::Gradient => "gradient",
            Self::Image => "image",
            Self::Pattern => "pattern",
        }
    }
}

/// `[backdrop] shape` of a `gradient`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackdropShape {
    #[default]
    Linear,
    Radial,
}

impl BackdropShape {
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("radial") { Self::Radial } else { Self::Linear }
    }
}

/// `[backdrop] fit` of an `image`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackdropFit {
    /// Fill the window, cropping the overflow (aspect kept).
    #[default]
    Cover,
    /// The whole image inside the window, the rest shows the base gradient.
    Contain,
    /// Fill the window exactly (aspect NOT kept).
    Stretch,
    /// Native size, centered.
    Center,
    /// Native size, repeated from the top-left.
    Tile,
}

impl BackdropFit {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "contain" => Self::Contain,
            "stretch" => Self::Stretch,
            "center" => Self::Center,
            "tile" => Self::Tile,
            _ => Self::Cover,
        }
    }
}

/// `[backdrop] pattern`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackdropPattern {
    #[default]
    Stars,
    Aurora,
    Grid,
    Synthwave,
}

impl BackdropPattern {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "aurora" => Self::Aurora,
            "grid" => Self::Grid,
            "synthwave" => Self::Synthwave,
            _ => Self::Stars,
        }
    }
}

/// The parsed `[backdrop]` table (the app builds it from the config strings on
/// load / reload / a settings change — never per frame).
#[derive(Clone, Debug, PartialEq)]
pub struct BackdropSettings {
    pub mode: BackdropMode,
    /// Gradient stops (sRGB). Empty = derived from the theme. At most 4 are used.
    pub colors: Vec<[u8; 3]>,
    /// Linear gradient direction, CSS convention: 0 = toward the top, 90 =
    /// toward the right, 135 = top-left → bottom-right.
    pub angle: f32,
    pub shape: BackdropShape,
    /// 0 = the plain theme background … 1 = the colors as given.
    pub strength: f32,
    pub vignette: f32,
    pub grain: f32,
    pub fit: BackdropFit,
    /// Blend of the image toward the theme background (raised by smart dim).
    pub dim: f32,
    /// Frosted-glass blur of the image (0 = sharp).
    pub blur: f32,
    pub pattern: BackdropPattern,
    /// Opt-in motion (≤ 30 fps, never on a CPU adapter, paused while hidden).
    pub animate: bool,
    /// Shift with the scrollback position.
    pub parallax: bool,
}

impl Default for BackdropSettings {
    fn default() -> Self {
        BackdropSettings {
            mode: BackdropMode::None,
            colors: Vec::new(),
            angle: 135.0,
            shape: BackdropShape::Linear,
            strength: 0.5,
            vignette: 0.0,
            grain: 0.0,
            fit: BackdropFit::Cover,
            dim: 0.7,
            blur: 0.0,
            pattern: BackdropPattern::Stars,
            animate: false,
            parallax: false,
        }
    }
}

impl BackdropSettings {
    /// `mode = "none"`: nothing is built or drawn.
    pub fn is_off(&self) -> bool {
        self.mode == BackdropMode::None
    }

    /// Whether this look moves when `animate` is on (and so needs timed wakes).
    /// Images never animate.
    pub fn animates(&self) -> bool {
        self.animate && matches!(self.mode, BackdropMode::Theme | BackdropMode::Gradient | BackdropMode::Pattern)
    }
}

/// Parse `#rrggbb`, `rrggbb` or `#rgb` (case-insensitive) into sRGB.
pub fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let nib = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let b = h.as_bytes();
    match b.len() {
        6 => {
            let byte = |i: usize| Some(nib(b[i])? * 16 + nib(b[i + 1])?);
            Some([byte(0)?, byte(2)?, byte(4)?])
        }
        3 => Some([nib(b[0])? * 17, nib(b[1])? * 17, nib(b[2])? * 17]),
        _ => None,
    }
}

// ── OKLab ────────────────────────────────────────────────────────────────────

fn srgb8_to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb8(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let s = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// sRGB → linear RGB.
pub fn srgb_to_linear(c: [u8; 3]) -> [f32; 3] {
    [srgb8_to_linear(c[0]), srgb8_to_linear(c[1]), srgb8_to_linear(c[2])]
}

/// Linear RGB → OKLab (Björn Ottosson).
pub fn linear_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * c[0] + 0.536_332_55 * c[1] + 0.051_445_995 * c[2];
    let m = 0.211_903_5 * c[0] + 0.680_699_5 * c[1] + 0.107_396_96 * c[2];
    let s = 0.088_302_46 * c[0] + 0.281_718_85 * c[1] + 0.629_978_7 * c[2];
    let (l, m, s) = (l.max(0.0).cbrt(), m.max(0.0).cbrt(), s.max(0.0).cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab → linear RGB (unclamped; the shader clamps).
pub fn oklab_to_linear(c: [f32; 3]) -> [f32; 3] {
    let l = c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2];
    let m = c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2];
    let s = c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// sRGB → OKLab.
pub fn srgb_to_oklab(c: [u8; 3]) -> [f32; 3] {
    linear_to_oklab(srgb_to_linear(c))
}

/// OKLab → sRGB (gamut-clamped).
pub fn oklab_to_srgb(c: [f32; 3]) -> [u8; 3] {
    let l = oklab_to_linear(c);
    [linear_to_srgb8(l[0]), linear_to_srgb8(l[1]), linear_to_srgb8(l[2])]
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Mix two sRGB colors in OKLab.
pub fn mix_oklab(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    oklab_to_srgb(mix3(srgb_to_oklab(a), srgb_to_oklab(b), t.clamp(0.0, 1.0)))
}

/// Scale an sRGB color's OKLab lightness (`k < 1` darker), hue kept.
pub fn scale_lightness(c: [u8; 3], k: f32) -> [u8; 3] {
    let mut lab = srgb_to_oklab(c);
    lab[0] = (lab[0] * k).clamp(0.0, 1.0);
    oklab_to_srgb(lab)
}

/// OKLab chroma of an sRGB color.
fn chroma(c: [u8; 3]) -> f32 {
    let lab = srgb_to_oklab(c);
    (lab[1] * lab[1] + lab[2] * lab[2]).sqrt()
}

/// A light theme (dark text on a light background)?
pub fn is_light_theme(theme: &jetty_core::Theme) -> bool {
    crate::colors::relative_luminance([theme.bg[0], theme.bg[1], theme.bg[2]]) > 0.35
}

// ── Curated per-theme looks (`mode = "theme"`) ───────────────────────────────

/// A curated theme backdrop: a glow leaning from the theme bg toward `accent`
/// that fades through the bg into a deeper shade of it. The stops are COMPUTED
/// from the live theme bg (so a user theme shadowing a built-in's name still
/// gets a coherent look on its own colors); the table holds the choices — the
/// hue, where the light comes from, how strong, vignette and grain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeLook {
    pub shape: BackdropShape,
    /// Linear: CSS angle (degrees). Unused for radial.
    pub angle: f32,
    /// Radial: the glow center as fractions of the window (0,0 = top-left).
    pub center: [f32; 2],
    /// Radial: radius as a multiple of the window's half diagonal.
    pub radius: f32,
    /// The glow hue (0xRRGGBB).
    pub accent: u32,
    /// How far the glow stop leans from the bg toward `accent` (OKLab mix).
    pub glow: f32,
    /// OKLab lightness multiplier of the far stop (< 1 = deeper than the bg).
    pub deep: f32,
    pub vignette: f32,
    pub grain: f32,
    /// Multiplies the user's `strength` (light themes are kept subtler).
    pub gain: f32,
}

const fn hex(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Glow from the top-left corner (the default dark look).
const TOP_LEFT: ThemeLook = ThemeLook {
    shape: BackdropShape::Radial,
    angle: 135.0,
    center: [0.12, 0.0],
    radius: 1.45,
    accent: 0,
    glow: 0.42,
    deep: 0.70,
    vignette: 0.0,
    grain: 0.0,
    gain: 1.0,
};
/// Glow from above the top edge, centered.
const TOP: ThemeLook = ThemeLook { center: [0.5, -0.12], radius: 1.25, ..TOP_LEFT };
/// Glow from the top-right corner.
const TOP_RIGHT: ThemeLook = ThemeLook { center: [0.88, 0.0], ..TOP_LEFT };
/// Warm light rising from below the bottom edge.
const BOTTOM: ThemeLook = ThemeLook { center: [0.5, 1.15], radius: 1.35, ..TOP_LEFT };
/// A CRT tube: light in the middle, dark rim.
const TUBE: ThemeLook = ThemeLook {
    center: [0.5, 0.45],
    radius: 1.05,
    glow: 0.16,
    deep: 0.45,
    vignette: 0.45,
    grain: 0.08,
    ..TOP_LEFT
};
/// Light themes: the same light, much subtler (glow and deepening both small).
const LIGHT: ThemeLook = ThemeLook { glow: 0.20, deep: 0.955, gain: 0.8, ..TOP_LEFT };
const LIGHT_TOP: ThemeLook = ThemeLook { center: [0.5, -0.12], radius: 1.25, ..LIGHT };
const LIGHT_BOTTOM: ThemeLook = ThemeLook { center: [0.5, 1.15], radius: 1.35, ..LIGHT };

/// The curated table, keyed by theme id. Covers the 22 shipped built-ins and
/// the visuals-v2 additions (an id that is not installed is simply unused);
/// any other theme gets [`derived_look`].
const THEME_LOOKS: &[(&str, ThemeLook)] = &[
    // ── shipped built-ins ──
    ("catppuccin_mocha", ThemeLook { accent: 0xcba6f7, ..TOP_LEFT }),
    ("tokyo_night", ThemeLook { accent: 0x7aa2f7, glow: 0.46, ..TOP }),
    ("gruvbox_dark", ThemeLook { accent: 0xd79921, glow: 0.34, deep: 0.62, vignette: 0.45, grain: 0.12, ..BOTTOM }),
    ("dracula", ThemeLook { accent: 0xbd93f9, glow: 0.46, ..TOP_LEFT }),
    ("onyx", ThemeLook { accent: 0x61afef, glow: 0.30, ..TOP_LEFT }),
    ("nord", ThemeLook { shape: BackdropShape::Linear, angle: 180.0, accent: 0x88c0d0, glow: 0.30, deep: 0.78, ..TOP }),
    ("solarized_dark", ThemeLook { accent: 0x2aa198, glow: 0.36, ..TOP_LEFT }),
    ("solarized_light", ThemeLook { accent: 0xb58900, glow: 0.16, ..LIGHT }),
    ("one_dark", ThemeLook { accent: 0x61afef, glow: 0.36, ..TOP_LEFT }),
    ("monokai", ThemeLook { accent: 0xf92672, glow: 0.30, ..TOP_RIGHT }),
    ("monokai_pro", ThemeLook { accent: 0xff6188, glow: 0.30, ..TOP_RIGHT }),
    ("everforest_dark", ThemeLook { accent: 0xa7c080, glow: 0.32, grain: 0.05, center: [0.0, 1.0], ..TOP_LEFT }),
    ("rose_pine", ThemeLook { accent: 0xc4a7e7, ..TOP_LEFT }),
    ("kanagawa", ThemeLook { accent: 0x7e9cd8, glow: 0.40, grain: 0.06, ..TOP }),
    ("material_dark", ThemeLook { accent: 0x16afca, glow: 0.36, ..TOP_LEFT }),
    ("ayu_dark", ThemeLook { accent: 0x39bae6, glow: 0.34, deep: 0.62, ..TOP_LEFT }),
    ("ayu_mirage", ThemeLook { accent: 0x73d0ff, glow: 0.36, ..TOP_LEFT }),
    ("tomorrow_night", ThemeLook { accent: 0x81a2be, glow: 0.36, ..TOP_LEFT }),
    ("oceanic_next", ThemeLook { accent: 0x5fb3b3, glow: 0.38, ..TOP_LEFT }),
    ("github_dark", ThemeLook { accent: 0x58a6ff, glow: 0.34, ..TOP }),
    ("palenight", ThemeLook { accent: 0xc792ea, ..TOP_LEFT }),
    ("catppuccin_macchiato", ThemeLook { accent: 0xc6a0f6, ..TOP_LEFT }),
    // ── visuals-v2 additions (slice A) ──
    ("catppuccin_latte", ThemeLook { accent: 0x7287fd, ..LIGHT }),
    ("tokyo_night_storm", ThemeLook { accent: 0x7aa2f7, glow: 0.46, ..TOP }),
    ("rose_pine_dawn", ThemeLook { accent: 0xd7827e, ..LIGHT }),
    ("gruvbox_light", ThemeLook { accent: 0xd79921, vignette: 0.20, grain: 0.08, ..LIGHT_BOTTOM }),
    ("synthwave_84", ThemeLook { accent: 0xff7edb, glow: 0.42, deep: 0.62, vignette: 0.20, ..BOTTOM }),
    ("phosphor_green", ThemeLook { accent: 0x33ff33, ..TUBE }),
    ("phosphor_amber", ThemeLook { accent: 0xffb000, ..TUBE }),
    ("green_screen", ThemeLook { accent: 0x33ff33, ..TUBE }),
    ("kanagawa_dragon", ThemeLook { accent: 0x8ba4b0, glow: 0.30, grain: 0.06, ..TOP }),
    ("github_light", ThemeLook { accent: 0x0969da, glow: 0.12, ..LIGHT_TOP }),
    ("night_owl", ThemeLook { accent: 0x82aaff, glow: 0.40, ..TOP_LEFT }),
    ("carbonfox", ThemeLook { accent: 0x78a9ff, glow: 0.28, ..TOP_LEFT }),
    ("catppuccin_frappe", ThemeLook { accent: 0xca9ee6, ..TOP_LEFT }),
    ("rose_pine_moon", ThemeLook { accent: 0xc4a7e7, ..TOP_LEFT }),
    ("alucard", ThemeLook { accent: 0x644ac9, ..LIGHT }),
    ("flexoki_light", ThemeLook { accent: 0x24837b, grain: 0.06, ..LIGHT }),
    ("everforest_light", ThemeLook { accent: 0x8da101, center: [0.0, 1.0], ..LIGHT }),
    ("poimandres", ThemeLook { accent: 0x5de4c7, glow: 0.32, ..TOP_LEFT }),
    ("melange_dark", ThemeLook { accent: 0xe49b5d, glow: 0.30, vignette: 0.30, grain: 0.06, ..BOTTOM }),
    ("tokyo_night_moon", ThemeLook { accent: 0x82aaff, glow: 0.46, ..TOP }),
    ("tokyo_night_day", ThemeLook { accent: 0x2e7de9, glow: 0.14, ..LIGHT_TOP }),
    ("kanagawa_lotus", ThemeLook { accent: 0x4d699b, grain: 0.05, ..LIGHT_TOP }),
    ("iceberg", ThemeLook { accent: 0x84a0c6, glow: 0.38, ..TOP }),
    ("flexoki_dark", ThemeLook { accent: 0x3aa99f, glow: 0.34, grain: 0.05, ..TOP_LEFT }),
    ("dayfox", ThemeLook { accent: 0x2848a9, glow: 0.14, ..LIGHT }),
    ("dawnfox", ThemeLook { accent: 0x907aa9, ..LIGHT }),
    ("one_half_light", ThemeLook { accent: 0x0184bc, glow: 0.14, ..LIGHT }),
];

/// The curated look for theme `name`, if the table has one.
pub fn theme_look(name: &str) -> Option<ThemeLook> {
    THEME_LOOKS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// The ids the curated table covers (tests / Settings).
pub fn curated_theme_ids() -> impl Iterator<Item = &'static str> {
    THEME_LOOKS.iter().map(|(n, _)| *n)
}

/// The most usable accent of a theme for a glow: the most chromatic of its
/// blue / magenta / bright blue / bright magenta / cyan (ties → blue).
fn pick_accent(theme: &jetty_core::Theme) -> [u8; 3] {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    let mut best = theme.palette[4];
    let mut score = -1.0f32;
    for i in [4usize, 5, 12, 13, 6] {
        let c = theme.palette[i];
        // Chroma, and distinct from the bg (a "blue" equal to the bg is useless).
        let s = chroma(c) * crate::colors::contrast_ratio(c, bg).min(3.0);
        if s > score + 1e-6 {
            best = c;
            score = s;
        }
    }
    best
}

/// The look for a theme the curated table does not know: a glow of its most
/// usable accent from the top-left corner (much subtler on light themes).
pub fn derived_look(theme: &jetty_core::Theme) -> ThemeLook {
    let a = pick_accent(theme);
    let accent = (a[0] as u32) << 16 | (a[1] as u32) << 8 | a[2] as u32;
    if is_light_theme(theme) {
        ThemeLook { accent, ..LIGHT }
    } else {
        ThemeLook { accent, glow: 0.38, ..TOP_LEFT }
    }
}

/// The contrast every backdrop keeps with the theme's text: 4.5:1 — or, for a
/// low-contrast theme (solarized is ~4.1–4.8:1 on its own), 92 % of the
/// theme's own fg/bg contrast, so a backdrop may cost such a theme at most 8 %
/// rather than having to vanish. One ratio for the curated stops, the shader's
/// readability guard and smart dim.
pub fn readable_ratio(theme: &jetty_core::Theme) -> f32 {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    (crate::colors::contrast_ratio(theme.fg, bg) * 0.92).min(4.5)
}

/// The largest `t ≤ amount` for which `color(t)` keeps `floor` contrast with
/// `fg` (`color(0)` is the bg, which always does). Bisection, 14 steps.
fn readable_amount(fg: [u8; 3], floor: f32, amount: f32, color: impl Fn(f32) -> [u8; 3]) -> f32 {
    if crate::colors::contrast_ratio(fg, color(amount)) >= floor {
        return amount;
    }
    let (mut lo, mut hi) = (0.0f32, amount);
    for _ in 0..14 {
        let mid = (lo + hi) * 0.5;
        if crate::colors::contrast_ratio(fg, color(mid)) >= floor {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The three stops of a theme look on `theme`: glow → bg → deep, for a
/// backdrop shown at `strength` (the stops are mixed over the bg by it). Each
/// outer stop leans only as far as keeps the theme's text readable where it is
/// SHOWN ([`readable_ratio`]) — a low-contrast theme (solarized) gets a
/// fainter glow, and no theme can be made unreadable by its backdrop.
pub fn look_stops(look: &ThemeLook, theme: &jetty_core::Theme, strength: f32) -> [[u8; 3]; 3] {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    let floor = readable_ratio(theme);
    let accent = hex(look.accent);
    let k = strength.clamp(0.0, 1.0);
    // Each candidate is judged exactly as shown: the (8-bit) stop, mixed over
    // the bg at the strength.
    let glow_at = |t: f32| mix_oklab(bg, accent, t);
    let glow = readable_amount(theme.fg, floor, look.glow, |t| mix_oklab(bg, glow_at(t), k));
    // `deep` is a lightness multiplier: walk it from 1 (the bg) toward the target.
    let deep_at = |t: f32| scale_lightness(bg, 1.0 + (look.deep - 1.0) * t);
    let deep = readable_amount(theme.fg, floor, 1.0, |t| mix_oklab(bg, deep_at(t), k));
    [glow_at(glow), bg, deep_at(deep)]
}

/// A theme's look: curated when the table has its id, else derived.
pub fn look_for_theme(theme: &jetty_core::Theme) -> ThemeLook {
    theme_look(&theme.name).unwrap_or_else(|| derived_look(theme))
}

// ── Resolved gradient ────────────────────────────────────────────────────────

/// The base gradient a frame draws, resolved from settings + theme (recomputed
/// only when either changes).
#[derive(Clone, Debug, PartialEq)]
pub struct GradientLook {
    pub shape: BackdropShape,
    pub angle: f32,
    pub center: [f32; 2],
    pub radius: f32,
    /// 1..=4 sRGB stops, evenly spaced.
    pub stops: Vec<[u8; 3]>,
    /// Effective mix of the stops over the theme bg (0..1).
    pub strength: f32,
    pub vignette: f32,
    pub grain: f32,
    /// The look's glow hue (patterns tint with it).
    pub accent: [u8; 3],
}

/// Resolve the base gradient for `settings` on `theme`.
pub fn resolve_look(s: &BackdropSettings, theme: &jetty_core::Theme) -> GradientLook {
    let tl = look_for_theme(theme);
    if s.mode == BackdropMode::Theme {
        let strength = (s.strength * tl.gain).clamp(0.0, 1.0);
        return GradientLook {
            shape: tl.shape,
            angle: tl.angle,
            center: tl.center,
            radius: tl.radius,
            stops: look_stops(&tl, theme, strength).to_vec(),
            strength,
            vignette: tl.vignette.max(s.vignette),
            grain: tl.grain.max(s.grain),
            accent: hex(tl.accent),
        };
    }
    let stops: Vec<[u8; 3]> = if s.colors.is_empty() {
        look_stops(&tl, theme, s.strength).to_vec()
    } else {
        s.colors.iter().copied().take(4).collect()
    };
    // A user radial gradient grows from the window center to its corners; the
    // derived one keeps its glow position (it was designed for it).
    let (center, radius) = if s.colors.is_empty() && tl.shape == BackdropShape::Radial {
        (tl.center, tl.radius)
    } else {
        ([0.5, 0.5], 1.0)
    };
    GradientLook {
        shape: s.shape,
        angle: s.angle,
        center,
        radius,
        stops,
        strength: s.strength.clamp(0.0, 1.0),
        vignette: s.vignette.clamp(0.0, 1.0),
        grain: s.grain.clamp(0.0, 1.0),
        accent: hex(tl.accent),
    }
}

// ── Pure geometry / readability helpers ──────────────────────────────────────

/// How a fitted image maps window pixels to UV: `uv = p * xf[0..2] + xf[2..4]`,
/// plus the mip level to sample (≥ 0; > 0 when the image is minified).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitXf {
    pub xf: [f32; 4],
    pub lod: f32,
}

/// The UV transform of `fit` for an image laid out at `img` px (stored as a
/// texture `tex_w` px wide) in a `win`-px window. `zoom` > 1 enlarges `cover` /
/// `stretch` around the center (parallax slack).
pub fn fit_transform(fit: BackdropFit, img: (f32, f32), tex_w: f32, win: (f32, f32), zoom: f32) -> FitXf {
    let (iw, ih) = (img.0.max(1.0), img.1.max(1.0));
    let (ww, wh) = (win.0.max(1.0), win.1.max(1.0));
    // Displayed image size (sx, sy scale per axis) and its top-left origin.
    let (sx, sy) = match fit {
        BackdropFit::Cover => {
            let s = (ww / iw).max(wh / ih) * zoom;
            (s, s)
        }
        BackdropFit::Contain => {
            let s = (ww / iw).min(wh / ih);
            (s, s)
        }
        BackdropFit::Stretch => (ww / iw * zoom, wh / ih * zoom),
        BackdropFit::Center | BackdropFit::Tile => (1.0, 1.0),
    };
    let (dw, dh) = (iw * sx, ih * sy);
    let (ox, oy) = match fit {
        BackdropFit::Tile => (0.0, 0.0),
        _ => ((ww - dw) * 0.5, (wh - dh) * 0.5),
    };
    // Texels per screen pixel along the more minified axis → mip level.
    let texel_ratio = (tex_w / iw) * (1.0 / sx).max(1.0 / sy);
    FitXf {
        xf: [1.0 / dw, 1.0 / dh, -ox / dw, -oy / dh],
        lod: texel_ratio.max(1e-6).log2().max(0.0),
    }
}

/// Smart dim: the smallest blend of the image toward the theme bg that keeps
/// the theme's text at ≥ `ratio`:1 (4.5 — see [`readable_ratio`]) against the
/// image's bright end (light text — its 95th-percentile luminance) or dark end
/// (dark text — 5th percentile), raised from the user's `dim`. Luminances are
/// WCAG relative (linear) and the dim mixes in linear light, so luminance
/// mixes linearly too.
pub fn smart_dim(dim: f32, lum_p5: f32, lum_p95: f32, fg_lum: f32, bg_lum: f32, ratio: f32) -> f32 {
    let dim = dim.clamp(0.0, 1.0);
    let need = if fg_lum >= bg_lum {
        // Light text: whatever is behind it must stay at or below `target`.
        let target = (fg_lum + 0.05) / ratio - 0.05;
        if lum_p95 <= target {
            0.0
        } else if bg_lum >= target {
            1.0
        } else {
            (lum_p95 - target) / (lum_p95 - bg_lum)
        }
    } else {
        // Dark text: whatever is behind it must stay at or above `target`.
        let target = ratio * (fg_lum + 0.05) - 0.05;
        if lum_p5 >= target {
            0.0
        } else if bg_lum <= target {
            1.0
        } else {
            (target - lum_p5) / (bg_lum - lum_p5)
        }
    };
    dim.max(need.clamp(0.0, 1.0))
}

/// The readability guard's bounds for `theme`, as the shader reads them:
/// `[cap, floor, bg luminance, 0]`. A dark theme (light text) caps the
/// backdrop's luminance at the text's [`readable_ratio`] point — never below
/// the theme bg's own luminance; a light theme floors it the same way. The
/// unused bound is off (cap 1, floor 0).
pub fn readability_bounds(theme: &jetty_core::Theme) -> [f32; 4] {
    let fg = crate::colors::relative_luminance(theme.fg);
    let bg = crate::colors::relative_luminance([theme.bg[0], theme.bg[1], theme.bg[2]]);
    let r = readable_ratio(theme);
    if fg >= bg {
        [((fg + 0.05) / r - 0.05).max(bg), 0.0, bg, 0.0]
    } else {
        [1.0, (r * (fg + 0.05) - 0.05).min(bg), bg, 0.0]
    }
}

/// Largest parallax shift, as a fraction of the window height.
pub const PARALLAX_MAX: f32 = 0.05;

/// The vertical backdrop shift (px) for `parallax`: the backdrop follows the
/// scrollback position at a quarter of the text's speed and eases out (tanh)
/// to at most [`PARALLAX_MAX`] of the window height — `cover` images are
/// zoomed by twice that, so the shift never reveals an image edge.
pub fn parallax_offset(scroll_px: f32, win_h: f32) -> f32 {
    let max = PARALLAX_MAX * win_h;
    if max <= 0.0 || scroll_px <= 0.0 {
        return 0.0;
    }
    max * (scroll_px * 0.25 / max).tanh()
}

// ── GPU uniform + shader ─────────────────────────────────────────────────────

/// Per-frame uniform (240 bytes, vec4-aligned; layout pinned by a test). Every
/// field is `[f32; N]` so the `#[repr(C)]` layout matches the WGSL `struct U`
/// byte for byte (offset table on `BACKDROP_SHADER`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BackdropUniform {
    /// Surface size, physical px.
    pub resolution: [f32; 2],
    /// Content offset in px (dropdown slide + parallax), subtracted from the
    /// fragment position.
    pub offset: [f32; 2],
    /// Theme bg in OKLab (xyz) + the window opacity (w).
    pub bg: [f32; 4],
    /// Gradient stops: OKLab (xyz) + position 0..1 (w).
    pub stops: [[f32; 4]; 4],
    /// Linear: direction (xy). Radial: center (xy, window fractions), radius
    /// (z, × half diagonal). w: 0 linear / 1 radial.
    pub geom: [f32; 4],
    /// strength, vignette, grain, stop count.
    pub look: [f32; 4],
    /// Image UV transform: `uv = p * xy + zw`.
    pub img_xf: [f32; 4],
    /// Image: dim, mip level, tile (0/1), frost amount.
    pub img_fx: [f32; 4],
    /// time (s), DPI scale, premultiply (0/1), light theme (0/1).
    pub misc: [f32; 4],
    /// Pattern / image colors (linear RGB in xyz).
    pub pc: [[f32; 4]; 3],
    /// Readability guard ([`readability_bounds`]): max luminance (1 = off),
    /// min luminance (0 = off), the theme bg's luminance, unused.
    pub guard: [f32; 4],
}

// Field byte offsets (Rust == WGSL):
//   resolution 0 · offset 8 · bg 16 · stops 32..96 · geom 96 · look 112 ·
//   img_xf 128 · img_fx 144 · misc 160 · pc 176..224 · guard 224
//   => 240 bytes, align 16.
pub(crate) const BACKDROP_SHADER: &str = r#"
struct U {
    resolution: vec2<f32>,
    offset: vec2<f32>,
    bg: vec4<f32>,
    stops: array<vec4<f32>, 4>,
    geom: vec4<f32>,
    look: vec4<f32>,
    img_xf: vec4<f32>,
    img_fx: vec4<f32>,
    misc: vec4<f32>,
    pc: array<vec4<f32>, 3>,
    guard: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// The aurora bake texture is this many window pixels per texel per axis.
const BAKE_DIV: f32 = 2.0;

struct VsOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VsOut;
    o.pos = vec4(verts[vi], 0.0, 1.0);
    return o;
}

// "Hash without Sine" (Dave Hoskins, MIT): float-only on purpose — 32-bit
// integer multiplies run at a fraction of the float rate on integrated GPUs,
// and a PCG hash per pixel cost this pass several times its budget.
fn hash13(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 = p3 + dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

fn hash33(p: vec3<f32>) -> vec3<f32> {
    var p3 = fract(p * vec3(0.1031, 0.1030, 0.0973));
    p3 = p3 + dot(p3, p3.yxz + 33.33);
    return fract((p3.xxy + p3.yxx) * p3.zyx);
}

// Interleaved gradient noise (Jimenez 2014): the standard cheap dither noise,
// well spread at every scale, for integer pixel coordinates.
fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))));
}

fn oklab_to_linear(c: vec3<f32>) -> vec3<f32> {
    let l_ = c.x + 0.3963377774 * c.y + 0.2158037573 * c.z;
    let m_ = c.x - 0.1055613458 * c.y - 0.0638541728 * c.z;
    let s_ = c.x - 0.0894841775 * c.y - 1.2914855480 * c.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    );
}

// Gradient coordinate 0..1 at content position p (px). An animated look's
// sway / drift is already folded into `geom` on the CPU (no trig per pixel).
fn grad_t(p: vec2<f32>) -> f32 {
    let res = u.resolution;
    if (u.geom.w < 0.5) {
        // Linear: project on the direction; the two extreme corners map to 0 and 1.
        let d = u.geom.xy;
        let ext = abs(d.x) * res.x + abs(d.y) * res.y;
        return dot(p - res * 0.5, d) / max(ext, 1.0) + 0.5;
    }
    // Radial: distance from the center over the radius.
    let r = u.geom.z * 0.5 * length(res);
    return length(p - u.geom.xy * res) / max(r, 1.0);
}

// The base gradient in OKLab, mixed over the theme bg by strength. Stops are
// joined with a smoothstep so no Mach band marks a stop.
fn base_lab(p: vec2<f32>) -> vec3<f32> {
    let t = clamp(grad_t(p), 0.0, 1.0);
    let n = u32(u.look.w);
    var c = u.stops[0].xyz;
    for (var i = 1u; i < 4u; i = i + 1u) {
        if (i < n) {
            let a = u.stops[i - 1u];
            let b = u.stops[i];
            let k = smoothstep(a.w, b.w, t);
            c = select(c, mix(a.xyz, b.xyz, k), t >= a.w);
        }
    }
    return mix(u.bg.xyz, c, u.look.x);
}

fn base_rgb(p: vec2<f32>) -> vec3<f32> {
    return max(oklab_to_linear(base_lab(p)), vec3(0.0));
}

// Radial darkening toward the corners of the backdrop (it is baked with the
// rest, so it rides the dropdown slide like the strip itself).
fn vignette(rgb: vec3<f32>, p: vec2<f32>) -> vec3<f32> {
    if (u.look.y <= 0.0) {
        return rgb;
    }
    let c = (p / u.resolution - vec2(0.5)) * 2.0;
    let d = length(c * vec2(1.0, 0.9)) * 0.7071;
    let f = 1.0 - u.look.y * 0.6 * smoothstep(0.3, 1.0, d);
    return rgb * (f * f * f);
}

// Readability guard: whatever the backdrop shows, keep it at the theme text's
// 4.5:1 point — cap its luminance on dark themes (scaled, so the hue stays),
// lift it toward the bg on light themes. Exact identity inside the bounds.
fn guard(rgb: vec3<f32>) -> vec3<f32> {
    let y = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
    var c = rgb;
    if (y > u.guard.x) {
        c = c * (u.guard.x / y);
    }
    if (y < u.guard.y) {
        let bg = max(oklab_to_linear(u.bg.xyz), vec3(0.0));
        c = mix(c, bg, clamp((u.guard.y - y) / max(u.guard.z - y, 1e-4), 0.0, 1.0));
    }
    return c;
}

// Premultiply (when the surface wants it), then dither: a triangular ±1 code
// step of noise in the sRGB-encoded output domain (the slope term converts a
// code step to linear light, ~2·sqrt(x)), monochrome so it never speckles
// color, plus the optional film grain (its hash runs only when grain is on).
// Static per pixel: no shimmer, no per-frame cost difference.
fn finish(rgb_in: vec3<f32>, frag: vec2<f32>) -> vec4<f32> {
    let a = u.bg.w;
    let cap = select(1.0, a, u.misc.z > 0.5);
    var rgb = clamp(rgb_in, vec3(0.0), vec3(1.0)) * cap;
    let px = floor(frag);
    // One uniform sample remapped to a triangular distribution on [-1, 1].
    let v = ign(px) * 2.0 - 1.0;
    var n = sign(v) * (1.0 - sqrt(max(1.0 - abs(v), 0.0))) * (1.0 / 255.0);
    if (u.look.z > 0.0) {
        n = n + (hash13(vec3(px, 7.0)) - 0.5) * 2.0 * u.look.z * 0.06;
    }
    let slope = max(2.0 * sqrt(rgb), vec3(0.0775));
    rgb = rgb + slope * n;
    return vec4(clamp(rgb, vec3(0.0), vec3(cap)), a);
}

@fragment
fn fs_gradient(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    return finish(guard(vignette(base_rgb(p), p)), in.pos.xy);
}

// ── image ──
@fragment
fn fs_image(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    let base = base_rgb(p);
    let uv = p * u.img_xf.xy + u.img_xf.zw;
    let s = textureSampleLevel(tex, samp, uv, u.img_fx.y);
    // Coverage: 1 inside the image, fading over 1 px outside it (the contain /
    // center letterbox shows the base); tiles cover everything.
    let disp = 1.0 / max(abs(u.img_xf.xy), vec2(1e-9));
    let out_px = max(max(-uv.x, uv.x - 1.0) * disp.x, max(-uv.y, uv.y - 1.0) * disp.y);
    let cov = select(clamp(0.5 - out_px, 0.0, 1.0), 1.0, u.img_fx.z > 0.5);
    var c = s.rgb + base * (1.0 - s.a);          // premultiplied image over the base
    c = mix(c, u.pc[0].xyz, u.img_fx.w);         // frosted tint (blurred images)
    c = mix(c, u.pc[1].xyz, u.img_fx.x);         // dim toward the theme bg
    let rgb = mix(base, c, cov);
    return finish(guard(vignette(rgb, p)), in.pos.xy);
}

// ── stars ──
// One star (or none) per cell, kept away from the cell edges so its glow is
// never clipped; brightness and size vary per star; animate twinkles them.
fn star_layer(p: vec2<f32>, cell: f32, density: f32, size: f32, seed: f32, t: f32) -> f32 {
    let q = p / cell;
    let g = floor(q);
    let h = hash33(vec3(g, seed));
    if (h.x > density) {
        return 0.0;
    }
    let f = (q - g) * cell;
    let c = (vec2(h.y, h.z) * 0.7 + vec2(0.15)) * cell;
    let d = length(f - c);
    let k = h.x / density;
    let r = size * (0.6 + 0.8 * k);
    let tw = 0.75 + 0.25 * sin(t * (0.8 + 2.2 * h.z) + h.y * 40.0);
    return (0.35 + 0.65 * k) * tw * exp(-(d * d) / (r * r));
}

@fragment
fn fs_stars(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    var rgb = base_rgb(p);
    let dpi = u.misc.y;
    let t = u.misc.x;
    let s = star_layer(p, 34.0 * dpi, 0.5, 0.7 * dpi, 11.0, t) * 0.8
        + star_layer(p + vec2(13.0, 29.0) * dpi, 91.0 * dpi, 0.35, 1.25 * dpi, 23.0, t);
    // The sky is guarded; the stars are not (a 1-2 px point never hides a
    // glyph, and capping them would put them out).
    let sky = guard(vignette(rgb, p));
    rgb = mix(sky, u.pc[0].xyz, clamp(s * u.look.x * 1.6, 0.0, 1.0));
    return finish(rgb, in.pos.xy);
}

// ── grid ──
// Coverage of an axis-aligned line lattice (antialiased over one pixel).
fn grid_cov(p: vec2<f32>, spacing: f32, half_w: f32) -> f32 {
    let g = abs(fract(p / spacing + vec2(0.5)) - vec2(0.5)) * spacing;
    let d = min(g.x, g.y);
    return 1.0 - smoothstep(half_w - 0.5, half_w + 0.5, d);
}

@fragment
fn fs_grid(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    var rgb = base_rgb(p);
    let dpi = u.misc.y;
    let q = p + vec2(0.0, u.misc.x * 8.0 * dpi);
    let minor = grid_cov(q, 28.0 * dpi, 0.5 * dpi);
    let major = grid_cov(q, 112.0 * dpi, 0.6 * dpi);
    let w = max(minor * 0.08, major * 0.18) * u.look.x * 2.0;
    rgb = mix(rgb, u.pc[0].xyz, clamp(w, 0.0, 1.0));
    return finish(guard(vignette(rgb, p)), in.pos.xy);
}

// ── synthwave ──
// A striped sun sinking into the horizon over a perspective floor grid that
// scrolls toward the viewer when animated. Colors are the theme's own yellow /
// magenta / cyan, so it fits any palette.
@fragment
fn fs_synthwave(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    let res = u.resolution;
    var rgb = base_rgb(p);
    let k = clamp(u.look.x * 2.0, 0.0, 2.0);
    let dpi = u.misc.y;
    let t = u.misc.x;
    let hz = res.y * 0.66;
    let r = min(res.x, res.y) * 0.2;
    let sc = vec2(res.x * 0.5, hz - r * 0.3);
    if (p.y < hz) {
        // Sky: a warm glow rising from the horizon.
        rgb = rgb + u.pc[1].xyz * exp(-(hz - p.y) / (res.y * 0.18)) * 0.10 * k;
        // Sun: a disc, yellow → magenta, cut by stripes that widen downward.
        let d = length(p - sc);
        var sun = 1.0 - smoothstep(r - 1.0, r + 1.0, d);
        let cut0 = sc.y - r * 0.05;
        if (p.y > cut0) {
            let period = r * 0.17;
            let into = fract((p.y - cut0) / period) * period;
            let gap = mix(0.12, 0.6, clamp((p.y - cut0) / (r * 1.1), 0.0, 1.0)) * period;
            sun = sun * smoothstep(gap - 0.5, gap + 0.5, into);
        }
        let v = clamp((p.y - (sc.y - r)) / (2.0 * r), 0.0, 1.0);
        rgb = mix(rgb, mix(u.pc[0].xyz, u.pc[1].xyz, v), sun * clamp(0.55 * k, 0.0, 1.0));
    } else {
        // Floor: a perspective plane. s = px below the horizon; depth z = 1 at
        // the bottom edge → ∞ at the horizon. Grid cells are `cell` px square
        // at the bottom edge; lines are 1 world unit apart on both axes.
        let s = max(p.y - hz, 0.25);
        let smax = max(res.y - hz, 1.0);
        let z = smax / s;
        let cell = 64.0 * dpi;
        let wx = (p.x - res.x * 0.5) * z / cell;
        let wz = z * smax / cell + t * 1.5;
        let gx = abs(fract(wx + 0.5) - 0.5);
        let gz = abs(fract(wz + 0.5) - 0.5);
        // World units per pixel along each axis: the AA width, and — once lines
        // crowd closer than a few pixels — the fade that keeps the horizon free
        // of moiré.
        let ux = z / cell;
        let uz = smax * smax / (s * s * cell);
        let lw = 0.8 * dpi;
        let lx = (1.0 - smoothstep(lw * ux * 0.5, lw * ux * 1.5, gx)) * (1.0 - smoothstep(0.12, 0.35, ux));
        let lz = (1.0 - smoothstep(lw * uz * 0.5, lw * uz * 1.5, gz)) * (1.0 - smoothstep(0.12, 0.35, uz));
        let line = max(lx, lz);
        rgb = mix(rgb, u.pc[2].xyz, clamp(line * 0.7 * k, 0.0, 1.0));
        rgb = rgb + u.pc[1].xyz * exp(-(s / res.y) / 0.03) * 0.12 * k;
    }
    return finish(guard(vignette(rgb, p)), in.pos.xy);
}

// ── aurora (baked) ──
fn vnoise(x: vec2<f32>, seed: f32) -> f32 {
    let i = floor(x);
    let f = x - i;
    let a = hash13(vec3(i, seed));
    let b = hash13(vec3(i + vec2(1.0, 0.0), seed));
    let c = hash13(vec3(i + vec2(0.0, 1.0), seed));
    let d = hash13(vec3(i + vec2(1.0, 1.0), seed));
    let w = f * f * (vec2(3.0) - 2.0 * f);
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

// Three octaves (≈ 0..0.875): plenty for a soft curtain, and every octave
// costs four hashes per texel.
fn fbm(x0: vec2<f32>, seed: f32) -> f32 {
    var x = x0;
    var s = 0.0;
    var amp = 0.5;
    for (var i = 0; i < 3; i = i + 1) {
        s = s + amp * vnoise(x, seed);
        x = x * 2.03 + vec2(13.7, 7.1);
        amp = amp * 0.5;
    }
    return s;
}

// Rendered into the half-res bake texture: three curtains (the theme's green,
// cyan and magenta) with a sharp lower edge, long rays upward and patchy
// intensity, fading out by mid-window. Output: emission (rgb) + coverage (a).
@fragment
fn fs_aurora_bake(in: VsOut) -> @location(0) vec4<f32> {
    let res = u.resolution;
    // Noise texel → cache pixel → backdrop position (the bake offset carries
    // the parallax margin).
    let p = in.pos.xy * BAKE_DIV - u.offset;
    let uv = p / res;
    // The curtains fade out by 70 % of the height: nothing to compute below.
    if (uv.y > 0.7) {
        return vec4(0.0);
    }
    let t = u.misc.x;
    let ax = uv.x * res.x / res.y;
    var col = vec3(0.0);
    var cov = 0.0;
    for (var k = 0; k < 3; k = k + 1) {
        let fk = f32(k);
        let x = ax * (0.9 + 0.3 * fk) + fk * 5.3;
        let yc = 0.14 + 0.10 * fk + (fbm(vec2(x * 1.1 + t * 0.04, fk * 2.7), 3.0) - 0.41) * 0.32;
        let d = uv.y - yc;
        let lower = exp(-(d * d) / (0.0016 + 0.0008 * fk));
        let upper = exp(-max(-d, 0.0) / (0.07 + 0.03 * fk));
        var band = select(upper, lower, d > 0.0);
        let rays = 0.45 + 0.55 * fbm(vec2(x * 7.0 + t * 0.12, uv.y * 0.8 + fk), 5.0);
        let patchy = smoothstep(0.22, 0.66, fbm(vec2(x * 0.8 - t * 0.02, fk * 1.9 + 4.0), 9.0));
        band = band * rays * patchy;
        col = col + u.pc[k].xyz * band;
        cov = max(cov, band);
    }
    let fade = 1.0 - smoothstep(0.2, 0.7, uv.y);
    return vec4(col * fade * 0.32, clamp(cov * fade, 0.0, 1.0));
}

// The base with the baked layer composited: emissive light on dark themes, a
// soft tint of its average hue on light themes.
@fragment
fn fs_baked(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy - u.offset;
    var rgb = base_rgb(p);
    // The noise layer covers the cache (window + parallax margin) at half size.
    let s = textureSampleLevel(tex, samp, in.pos.xy / (u.resolution + vec2(0.0, u.offset.y)), 0.0);
    let k = u.look.x * 2.0;
    let lit = rgb * (1.0 - 0.3 * clamp(s.a * k, 0.0, 1.0)) + s.rgb * k;
    // Light themes: a pastel of the curtain's hue (not a gray smudge).
    let hue = s.rgb / max(max(s.r, max(s.g, s.b)), 1e-4);
    let tinted = mix(rgb, mix(hue, vec3(1.0), 0.45), clamp(s.a * k * 0.45, 0.0, 1.0));
    rgb = select(lit, tinted, u.misc.w > 0.5);
    return finish(guard(vignette(rgb, p)), in.pos.xy);
}

// ── per frame ──
// Copy the baked cache (group 1) under the grid at the content offset — the
// slide and parallax shift it, rows past its edge repeat the edge — with the
// window's opacity, premultiplied when the surface wants it. An exact texel
// copy: the dither baked into the cache survives untouched at opacity 1.
@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(tex));
    let q = clamp(vec2<i32>(floor(in.pos.xy - u.offset)), vec2(0), dims - vec2(1));
    let c = textureLoad(tex, q, 0).rgb;
    let a = u.bg.w;
    return vec4(c * select(1.0, a, u.misc.z > 0.5), a);
}
"#;

/// Which fragment entry point a frame draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Gradient,
    Image,
    Stars,
    Grid,
    Synthwave,
    /// The aurora, sampled from its bake texture.
    Baked,
}

impl Variant {
    const ALL: [Variant; 6] =
        [Variant::Gradient, Variant::Image, Variant::Stars, Variant::Grid, Variant::Synthwave, Variant::Baked];

    fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }

    fn entry(self) -> &'static str {
        match self {
            Variant::Gradient => "fs_gradient",
            Variant::Image => "fs_image",
            Variant::Stars => "fs_stars",
            Variant::Grid => "fs_grid",
            Variant::Synthwave => "fs_synthwave",
            Variant::Baked => "fs_baked",
        }
    }

    /// Whether the entry point samples group 1 (texture + sampler).
    fn textured(self) -> bool {
        matches!(self, Variant::Image | Variant::Baked)
    }
}

/// The variant a frame of `s` draws (`has_image`: a decoded image is ready).
/// `None` for `mode = "none"`. An image that is still loading or failed to
/// load shows the base gradient.
pub fn variant_for(s: &BackdropSettings, has_image: bool) -> Option<Variant> {
    Some(match s.mode {
        BackdropMode::None => return None,
        BackdropMode::Theme | BackdropMode::Gradient => Variant::Gradient,
        BackdropMode::Image if has_image => Variant::Image,
        BackdropMode::Image => Variant::Gradient,
        BackdropMode::Pattern => match s.pattern {
            BackdropPattern::Stars => Variant::Stars,
            BackdropPattern::Grid => Variant::Grid,
            BackdropPattern::Synthwave => Variant::Synthwave,
            BackdropPattern::Aurora => Variant::Baked,
        },
    })
}

// ── GPU image ────────────────────────────────────────────────────────────────

/// A backdrop image uploaded to ONE device (mipmapped `Rgba8UnormSrgb`). The
/// app shares one `Arc<GpuImage>` across every window on that device.
pub struct GpuImage {
    device: wgpu::Device,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    pub layout_w: u32,
    pub layout_h: u32,
    pub tex_w: u32,
    pub tex_h: u32,
    pub lum_p5: f32,
    pub lum_p95: f32,
    pub blurred: bool,
}

impl std::fmt::Debug for GpuImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuImage")
            .field("layout", &(self.layout_w, self.layout_h))
            .field("texture", &(self.tex_w, self.tex_h))
            .finish()
    }
}

impl GpuImage {
    /// Upload `img` (every mip level) to `device`. Levels larger than the
    /// device's texture limit are skipped (it starts from the first that fits);
    /// `None` when nothing fits.
    pub fn upload(device: &wgpu::Device, queue: &wgpu::Queue, img: &DecodedImage) -> Option<GpuImage> {
        let max = device.limits().max_texture_dimension_2d;
        let first = img.mips.iter().position(|m| m.w <= max && m.h <= max)?;
        let l0 = img.mips.get(first)?;
        // Only the prefix that follows the GPU's mip size rule (level k is
        // max(1, size >> k)) — wgpu rejects anything else.
        let valid = img.mips[first..]
            .iter()
            .enumerate()
            .take_while(|(k, m)| {
                let k = *k as u32;
                k < 32 && (m.w, m.h) == ((l0.w >> k).max(1), (l0.h >> k).max(1))
            })
            .count();
        let levels = &img.mips[first..first + valid];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop-image"),
            size: wgpu::Extent3d { width: l0.w, height: l0.h, depth_or_array_layers: 1 },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, m) in levels.iter().enumerate() {
            if m.rgba.len() != (m.w as usize) * (m.h as usize) * 4 {
                return None; // defensive: never hand wgpu a short buffer
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: i as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &m.rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(m.w * 4), rows_per_image: Some(m.h) },
                wgpu::Extent3d { width: m.w, height: m.h, depth_or_array_layers: 1 },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(GpuImage {
            device: device.clone(),
            _texture: texture,
            view,
            layout_w: img.layout_w,
            layout_h: img.layout_h,
            tex_w: l0.w,
            tex_h: l0.h,
            lum_p5: img.lum_p5,
            lum_p95: img.lum_p95,
            blurred: img.blurred,
        })
    }

    /// The device this texture lives on (a window on another device cannot use it).
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
}

// ── Per-frame inputs + uniform builder ───────────────────────────────────────

/// What a frame feeds the backdrop besides the settings and the theme.
#[derive(Clone, Copy, Debug)]
pub struct BackdropFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// Dropdown slide offset (px; 0 for detached windows and jetty-shot).
    pub slide_y: f32,
    /// Scrollback position in px (`scroll_offset × cell_h`), read by `parallax`.
    pub scroll_px: f32,
    /// DPI scale (pattern sizes).
    pub dpi: f32,
    /// `GpuContext::premultiply_clear`.
    pub premultiply: bool,
    /// Animation clock in seconds (only read while the look animates).
    pub time: f32,
    /// The decoded image on THIS window's device, when ready.
    pub image: Option<&'a Arc<GpuImage>>,
}

fn lab4(c: [u8; 3], w: f32) -> [f32; 4] {
    let l = srgb_to_oklab(c);
    [l[0], l[1], l[2], w]
}

fn lin4(c: [u8; 3]) -> [f32; 4] {
    let l = srgb_to_linear(c);
    [l[0], l[1], l[2], 0.0]
}

/// The frosted tint of a theme: its bg nudged lighter (dark themes) or darker
/// (light themes) in OKLab.
fn frost_tint(theme: &jetty_core::Theme) -> [u8; 3] {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    let mut lab = srgb_to_oklab(bg);
    lab[0] = if is_light_theme(theme) { lab[0] - 0.03 } else { lab[0] + 0.06 }.clamp(0.0, 1.0);
    oklab_to_srgb(lab)
}

/// The frosted-tint mix for a blurred image.
fn frost_amount(blurred: bool, blur: f32) -> f32 {
    if blurred { 0.22 * blur.clamp(0.0, 1.0) } else { 0.0 }
}

/// Build the frame's uniform (pure: unit-tested without a GPU).
pub fn build_uniform(
    look: &GradientLook,
    variant: Variant,
    s: &BackdropSettings,
    theme: &jetty_core::Theme,
    f: &BackdropFrame,
) -> BackdropUniform {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    let opacity = theme.bg[3] as f32 / 255.0;
    let light = is_light_theme(theme);
    let (w, h) = (f.width.max(1) as f32, f.height.max(1) as f32);
    let parallax = if s.parallax { parallax_offset(f.scroll_px, h) } else { 0.0 };

    // Stops: OKLab + evenly spaced positions (a single color is a flat fill).
    let n = look.stops.len().clamp(1, 4);
    let mut stops = [[0.0f32; 4]; 4];
    for (i, slot) in stops.iter_mut().enumerate() {
        let c = look.stops[i.min(n - 1)];
        let pos = if n == 1 { i as f32 } else { (i.min(n - 1)) as f32 / (n - 1) as f32 };
        *slot = lab4(c, pos);
    }
    let time = if s.animates() { f.time } else { 0.0 };
    // An animated look sways (linear: ±7°) or drifts (radial) slowly; folded in
    // here so the shader does no trig per pixel. time 0 = exactly static.
    let geom = match look.shape {
        BackdropShape::Linear => {
            let a = look.angle.to_radians() + 0.12 * (0.07 * time).sin();
            [a.sin(), -a.cos(), 0.0, 0.0]
        }
        BackdropShape::Radial => [
            look.center[0] + 0.05 * (0.13 * time).sin(),
            look.center[1] + 0.05 * (0.091 * time).sin(),
            look.radius.max(0.05),
            1.0,
        ],
    };

    let mut u = BackdropUniform {
        resolution: [w, h],
        offset: [0.0, f.slide_y + parallax],
        bg: lab4(bg, opacity),
        stops,
        geom,
        look: [look.strength, look.vignette, look.grain, n.max(2) as f32],
        img_xf: [1.0 / w, 1.0 / h, 0.0, 0.0],
        img_fx: [0.0; 4],
        misc: [time, f.dpi.max(0.5), if f.premultiply { 1.0 } else { 0.0 }, if light { 1.0 } else { 0.0 }],
        pc: [[0.0; 4]; 3],
        guard: readability_bounds(theme),
    };
    // A one-color "gradient": both used stops are that color.
    if n == 1 {
        u.stops[1] = lab4(look.stops[0], 1.0);
    }

    let pal = |i: usize| theme.palette[i];
    let fg = theme.fg;
    match variant {
        Variant::Image => {
            if let Some(img) = f.image {
                let zoom = if s.parallax && matches!(s.fit, BackdropFit::Cover | BackdropFit::Stretch) {
                    1.0 + 2.0 * PARALLAX_MAX
                } else {
                    1.0
                };
                let fit = fit_transform(
                    s.fit,
                    (img.layout_w as f32, img.layout_h as f32),
                    img.tex_w as f32,
                    (w, h),
                    zoom,
                );
                let frost = frost_amount(img.blurred, s.blur);
                let tint = frost_tint(theme);
                let tint_lum = crate::colors::relative_luminance(tint);
                let (p5, p95) = (
                    img.lum_p5 * (1.0 - frost) + tint_lum * frost,
                    img.lum_p95 * (1.0 - frost) + tint_lum * frost,
                );
                let dim = smart_dim(
                    s.dim,
                    p5,
                    p95,
                    crate::colors::relative_luminance(fg),
                    crate::colors::relative_luminance(bg),
                    readable_ratio(theme),
                );
                u.img_xf = fit.xf;
                u.img_fx = [dim, fit.lod, if s.fit == BackdropFit::Tile { 1.0 } else { 0.0 }, frost];
                u.pc[0] = lin4(tint);
                u.pc[1] = lin4(bg);
                // Frosted glass carries a little grain of its own.
                u.look[2] = u.look[2].max(0.12 * frost / 0.22);
            }
        }
        Variant::Stars => {
            // Light stars on dark themes; on light themes, ink specks.
            u.pc[0] = if light { lin4(mix_oklab(fg, bg, 0.25)) } else { lin4(mix_oklab(fg, [255, 255, 255], 0.35)) };
        }
        Variant::Grid => {
            u.pc[0] = lin4(mix_oklab(look.accent, fg, 0.25));
        }
        Variant::Synthwave => {
            u.pc[0] = lin4(pal(11));
            u.pc[1] = lin4(pal(13));
            u.pc[2] = lin4(pal(14));
        }
        Variant::Baked => {
            u.pc[0] = lin4(pal(10));
            u.pc[1] = lin4(pal(14));
            u.pc[2] = lin4(pal(13));
        }
        Variant::Gradient => {}
    }
    u
}

// ── The GPU layer ────────────────────────────────────────────────────────────

/// The theme fields a resolved look depends on (compared each frame without
/// allocating).
#[derive(Clone, Debug, PartialEq)]
struct ThemeKey {
    name: String,
    bg: [u8; 3],
    fg: [u8; 3],
    palette: [[u8; 3]; 16],
}

impl ThemeKey {
    fn of(t: &jetty_core::Theme) -> Self {
        ThemeKey { name: t.name.to_string(), bg: [t.bg[0], t.bg[1], t.bg[2]], fg: t.fg, palette: t.palette }
    }

    fn matches(&self, t: &jetty_core::Theme) -> bool {
        self.name == *t.name && self.bg == [t.bg[0], t.bg[1], t.bg[2]] && self.fg == t.fg && self.palette == t.palette
    }
}

/// A texture the backdrop bakes into, with the bind group that reads it.
struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

/// The per-window cache: the finished backdrop — straight color, guarded,
/// vignetted and dithered — sRGB-encoded 8-bit, exactly what the surface
/// stores. A frame only copies it (`fs_composite`).
const CACHE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// An animated look re-bakes at most this often (≤ 30 fps).
const BAKE_MIN_INTERVAL: f32 = 1.0 / 30.0;

/// Rows baked above the window for `parallax`: the cache covers backdrop rows
/// `-margin .. height`, so the shift never samples past its top edge.
pub fn parallax_margin(win_h: f32) -> u32 {
    (PARALLAX_MAX * win_h.max(0.0)).ceil() as u32
}

/// What a baked cache shows: the bake inputs with the clock stopped at 0 —
/// an animated look's sway / drift is folded into `geom` on the CPU, so the
/// clock changes more than `misc[0]` — plus the clock it was baked at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BakeStamp {
    pub still: BackdropUniform,
    pub time: f32,
}

/// The bake pass's uniform for a frame's `full` uniform: the window's layout in
/// straight color at full alpha, cache row r holding backdrop row r - `margin`.
fn bake_of(full: &BackdropUniform, margin: u32) -> BackdropUniform {
    let mut bake = *full;
    bake.offset = [0.0, margin as f32];
    bake.bg[3] = 1.0;
    bake.misc[2] = 0.0;
    bake
}

/// The bake uniform of frame `f` (whose full uniform is `full`) and the cache
/// stamp it would leave (see [`BakeStamp`]). A still look is its own stamp; an
/// animated one is rebuilt with the clock at 0 (pure CPU math, only while it
/// animates).
#[allow(clippy::too_many_arguments)]
pub fn bake_stamp(
    full: &BackdropUniform,
    look: &GradientLook,
    variant: Variant,
    s: &BackdropSettings,
    theme: &jetty_core::Theme,
    f: &BackdropFrame,
    margin: u32,
) -> (BackdropUniform, BakeStamp) {
    let bake = bake_of(full, margin);
    let still = if s.animates() {
        bake_of(&build_uniform(look, variant, s, theme, &BackdropFrame { time: 0.0, ..*f }), margin)
    } else {
        bake
    };
    (bake, BakeStamp { still, time: bake.misc[0] })
}

/// Whether a cache stamped `from` still shows `now`: identical inputs at the
/// same clock, or — while the look animates — a clock that moved less than a
/// 30 fps frame (so typing or an output flood never re-bakes faster).
pub fn bake_is_fresh(from: Option<&BakeStamp>, now: &BakeStamp, animates: bool) -> bool {
    let Some(from) = from else { return false };
    from.still == now.still
        && (from.time == now.time || (animates && (now.time - from.time).abs() < BAKE_MIN_INTERVAL))
}

/// One window's backdrop GPU state. Built only when the mode is not `none`
/// (the app keeps `Option<Backdrop>`).
///
/// The look is BAKED into a window-sized cache whenever an input changes —
/// size, theme, settings, the image, DPI, or (animated looks, ≤ 30 fps) the
/// clock — by the variant's own shader, which may be as rich as it likes. A
/// frame then only COPIES the cache under the grid (`fs_composite`: one
/// texel load + opacity/premultiply), whatever the variant: measured on the
/// reference iGPU this is a small step over the floor every full-screen pass
/// pays, where evaluating the gradient per frame cost more than twice that.
/// The dropdown slide and the parallax shift are offsets of that copy, so
/// neither re-bakes. Pipelines are built lazily per variant.
pub struct Backdrop {
    format: wgpu::TextureFormat,
    shader: wgpu::ShaderModule,
    /// Uniform of the bake passes (window layout, straight color).
    bake_buf: wgpu::Buffer,
    bake_bg: wgpu::BindGroup,
    /// Uniform of the per-frame composite (offset, opacity, premultiply).
    comp_buf: wgpu::Buffer,
    comp_bg: wgpu::BindGroup,
    layout0: wgpu::PipelineLayout,
    layout01: wgpu::PipelineLayout,
    tex_bgl: wgpu::BindGroupLayout,
    clamp_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
    /// Bake pipelines per variant (into the cache).
    pipelines: [Option<wgpu::RenderPipeline>; 6],
    noise_pipeline: Option<wgpu::RenderPipeline>,
    composite: Option<wgpu::RenderPipeline>,
    /// Settings + theme the cached look was resolved from.
    resolved: Option<(BackdropSettings, ThemeKey)>,
    look: Option<GradientLook>,
    variant: Variant,
    /// The image bind group: (image identity, tile sampler?, bind group).
    image_bind: Option<(Arc<GpuImage>, bool, wgpu::BindGroup)>,
    cache: Option<Target>,
    noise: Option<Target>,
    /// The aurora noise layer's format, chosen on its first use: half-float, or
    /// 8-bit sRGB where the GPU cannot render half-float
    /// (`gpu::effect_target_format`).
    noise_format: Option<wgpu::TextureFormat>,
    /// What the cache holds (see [`BakeStamp`]).
    baked: Option<BakeStamp>,
    last_comp: Option<BackdropUniform>,
    /// Cache bakes so far (tests and the bench count them).
    bakes: u64,
}

impl Backdrop {
    /// Build the layer: shader module, uniform buffers, layouts and samplers —
    /// NO pipeline and no texture yet (each is created on first use).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop-shader"),
            source: wgpu::ShaderSource::Wgsl(BACKDROP_SHADER.into()),
        });
        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop-uniform-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform = |label: &'static str| {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<BackdropUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &uniform_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            (buf, bg)
        };
        let (bake_buf, bake_bg) = uniform("backdrop-bake-uniform");
        let (comp_buf, comp_bg) = uniform("backdrop-composite-uniform");
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout0 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop-layout0"),
            bind_group_layouts: &[Some(&uniform_bgl)],
            ..Default::default()
        });
        let layout01 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop-layout01"),
            bind_group_layouts: &[Some(&uniform_bgl), Some(&tex_bgl)],
            ..Default::default()
        });
        let sampler = |mode: wgpu::AddressMode, label: &'static str| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: mode,
                address_mode_v: mode,
                address_mode_w: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            })
        };
        Backdrop {
            format,
            clamp_sampler: sampler(wgpu::AddressMode::ClampToEdge, "backdrop-clamp"),
            repeat_sampler: sampler(wgpu::AddressMode::Repeat, "backdrop-repeat"),
            shader,
            bake_buf,
            bake_bg,
            comp_buf,
            comp_bg,
            layout0,
            layout01,
            tex_bgl,
            pipelines: Default::default(),
            noise_pipeline: None,
            composite: None,
            resolved: None,
            look: None,
            variant: Variant::Gradient,
            image_bind: None,
            cache: None,
            noise: None,
            noise_format: None,
            baked: None,
            last_comp: None,
            bakes: 0,
        }
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        entry: &str,
        textured: bool,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop-pipeline"),
            layout: Some(if textured { &self.layout01 } else { &self.layout0 }),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some(entry),
                // REPLACE: every pass owns every pixel of its target.
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    fn texture_bind_group(&self, device: &wgpu::Device, view: &wgpu::TextureView, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop-tex-bg"),
            layout: &self.tex_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    fn target(&self, device: &wgpu::Device, label: &'static str, size: (u32, u32), format: wgpu::TextureFormat) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.texture_bind_group(device, &view, &self.clamp_sampler);
        Target { _texture: texture, view, bind_group, size }
    }

    /// Get ready to draw one frame: re-resolve the look when the settings or
    /// the theme changed, (re)bind the image, RE-BAKE the cache when any baked
    /// input changed, and write the composite uniform when it changed (opacity,
    /// slide, parallax). Returns `false` (draw nothing) for `mode = "none"`.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: &BackdropSettings,
        theme: &jetty_core::Theme,
        f: &BackdropFrame,
    ) -> bool {
        let image = f.image.filter(|img| img.device() == device);
        let Some(variant) = variant_for(settings, image.is_some()) else {
            return false;
        };
        let fresh = self.resolved.as_ref().is_some_and(|(s, k)| s == settings && k.matches(theme));
        if !fresh || self.look.is_none() {
            self.look = Some(resolve_look(settings, theme));
            self.resolved = Some((settings.clone(), ThemeKey::of(theme)));
        }
        self.variant = variant;
        // The image bind group follows the image identity and the tile flag.
        if variant == Variant::Image {
            let img = image.expect("Image variant implies an image");
            let tile = settings.fit == BackdropFit::Tile;
            let stale = self.image_bind.as_ref().is_none_or(|(i, t, _)| !Arc::ptr_eq(i, img) || *t != tile);
            if stale {
                let sampler = if tile { &self.repeat_sampler } else { &self.clamp_sampler };
                let bg = self.texture_bind_group(device, &img.view, sampler);
                self.image_bind = Some((Arc::clone(img), tile, bg));
                self.baked = None;
            }
        } else if self.image_bind.is_some() && f.image.is_none() {
            // The image went away (mode change / reload): release the texture.
            self.image_bind = None;
        }
        let frame = BackdropFrame { image, ..*f };
        let look = self.look.as_ref().expect("resolved above");
        let full = build_uniform(look, variant, settings, theme, &frame);
        let (w, h) = (full.resolution[0] as u32, full.resolution[1] as u32);
        let margin = if settings.parallax { parallax_margin(h as f32) } else { 0 };
        // The bake: the window's layout in straight color at full alpha, cache
        // row r holding backdrop row r - margin; and what it would leave cached.
        let (bake, stamp) = bake_stamp(&full, look, variant, settings, theme, &frame, margin);
        // The composite: the cache read at the content offset.
        let mut comp = full;
        comp.offset = [full.offset[0], full.offset[1] - margin as f32];

        let size = (w.max(1), (h + margin).max(1));
        if self.cache.as_ref().is_none_or(|c| c.size != size) {
            self.cache = Some(self.target(device, "backdrop-cache", size, CACHE_FORMAT));
            self.baked = None;
        }
        if !bake_is_fresh(self.baked.as_ref(), &stamp, settings.animates())
            && self.bake(device, queue, variant, &bake, size)
        {
            self.baked = Some(stamp);
        }
        if variant != Variant::Baked {
            self.noise = None; // the aurora's layer is only kept while it shows
        }
        if self.composite.is_none() {
            self.composite = Some(self.pipeline(device, "fs_composite", true, self.format));
        }
        if self.last_comp != Some(comp) {
            queue.write_buffer(&self.comp_buf, 0, bytemuck::bytes_of(&comp));
            self.last_comp = Some(comp);
        }
        true
    }

    /// Render the look into the cache (the aurora first renders its noise
    /// layer at half resolution): one encoder, one submit, off the frame's pass.
    /// Returns whether it baked (a textured variant without its texture waits).
    fn bake(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, variant: Variant, u: &BackdropUniform, size: (u32, u32)) -> bool {
        queue.write_buffer(&self.bake_buf, 0, bytemuck::bytes_of(u));
        let i = variant.index();
        if self.pipelines[i].is_none() {
            self.pipelines[i] = Some(self.pipeline(device, variant.entry(), variant.textured(), CACHE_FORMAT));
        }
        if variant == Variant::Baked {
            let nsize = (size.0.div_ceil(2), size.1.div_ceil(2));
            let format = *self.noise_format.get_or_insert_with(|| crate::gpu::effect_target_format(device));
            if self.noise.as_ref().is_none_or(|n| n.size != nsize) {
                self.noise = Some(self.target(device, "backdrop-noise", nsize, format));
            }
            if self.noise_pipeline.is_none() {
                self.noise_pipeline = Some(self.pipeline(device, "fs_aurora_bake", false, format));
            }
        }
        let group1 = match variant {
            Variant::Image => self.image_bind.as_ref().map(|(_, _, bg)| bg),
            Variant::Baked => self.noise.as_ref().map(|n| &n.bind_group),
            _ => None,
        };
        let (Some(cache), Some(pipeline)) = (self.cache.as_ref(), self.pipelines[i].as_ref()) else { return false };
        if variant.textured() && group1.is_none() {
            return false;
        }
        fn pass_desc(view: &wgpu::TextureView) -> wgpu::RenderPassColorAttachment<'_> {
            wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                depth_slice: None,
            }
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("backdrop-bake") });
        if let (Variant::Baked, Some(noise), Some(np)) = (variant, self.noise.as_ref(), self.noise_pipeline.as_ref()) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop-noise-pass"),
                color_attachments: &[Some(pass_desc(&noise.view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(np);
            pass.set_bind_group(0, &self.bake_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop-bake-pass"),
                color_attachments: &[Some(pass_desc(&cache.view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bake_bg, &[]);
            if let Some(bg) = group1 {
                pass.set_bind_group(1, bg, &[]);
            }
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        self.bakes += 1;
        true
    }

    /// Record the backdrop into the frame's grid pass (after its clear, before
    /// the cell backgrounds): a copy of the cache. [`Self::prepare`] must have
    /// returned `true` this frame.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let (Some(pipeline), Some(cache)) = (self.composite.as_ref(), self.cache.as_ref()) else { return };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.comp_bg, &[]);
        pass.set_bind_group(1, &cache.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The variant [`Self::prepare`] chose for this frame.
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// How many times the cache has been baked (an unchanged frame never bakes).
    pub fn bake_count(&self) -> u64 {
        self.bakes
    }

    /// Force a re-bake on the next [`Self::prepare`] (benchmarks).
    pub fn invalidate(&mut self) {
        self.baked = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str) -> jetty_core::Theme {
        let i = jetty_core::theme::PRESETS.iter().position(|&n| n == name).expect("built-in");
        jetty_core::theme_at(i)
    }

    /// The backdrop WGSL must parse and pass naga validation (always-run gate,
    /// like `crt_shader_compiles`).
    #[test]
    fn backdrop_shader_compiles() {
        let module = naga::front::wgsl::parse_str(BACKDROP_SHADER).expect("BACKDROP_SHADER must parse");
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("BACKDROP_SHADER must pass naga validation");
        // Every entry point the Rust side names exists.
        let names: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        for v in Variant::ALL {
            assert!(names.contains(&v.entry()), "missing {}", v.entry());
        }
        assert!(names.contains(&"fs_aurora_bake") && names.contains(&"vs") && names.contains(&"fs_composite"));
    }

    #[test]
    fn bake_freshness() {
        let t = theme("tokyo_night");
        let s = BackdropSettings { mode: BackdropMode::Theme, ..Default::default() };
        let look = resolve_look(&s, &t);
        let f = BackdropFrame {
            width: 640,
            height: 400,
            slide_y: 0.0,
            scroll_px: 0.0,
            dpi: 1.0,
            premultiply: true,
            time: 0.0,
            image: None,
        };
        let full = build_uniform(&look, Variant::Gradient, &s, &t, &f);
        let (_, a) = bake_stamp(&full, &look, Variant::Gradient, &s, &t, &f, 0);
        assert!(!bake_is_fresh(None, &a, false), "never baked");
        assert!(bake_is_fresh(Some(&a), &a, false));
        let mut b = a;
        b.still.resolution = [641.0, 400.0];
        assert!(!bake_is_fresh(Some(&a), &b, false), "a resize re-bakes");
        // Animated: the clock may drift by less than a 30 fps frame.
        let t1 = BakeStamp { time: 1.0, ..a };
        let t2 = BakeStamp { time: 1.0 + BAKE_MIN_INTERVAL * 0.5, ..a };
        assert!(bake_is_fresh(Some(&t1), &t2, true));
        assert!(!bake_is_fresh(Some(&t1), &t2, false), "a static look re-bakes on any change");
        let t3 = BakeStamp { time: 1.0 + BAKE_MIN_INTERVAL * 1.5, ..a };
        assert!(!bake_is_fresh(Some(&t1), &t3, true), "≤ 30 fps, not slower");
    }

    /// An animated look re-bakes at most once per 30 fps frame however often
    /// the window repaints (typing, a flood): two frames inside one bake
    /// interval share the cache. The clock also moves the look's geometry (the
    /// sway / drift folded into `geom`), which must not count as a new look.
    #[test]
    fn an_animated_look_rebakes_at_most_at_30_fps() {
        let t = theme("tokyo_night");
        let looks = [
            BackdropSettings { mode: BackdropMode::Theme, animate: true, ..Default::default() },
            BackdropSettings { mode: BackdropMode::Gradient, animate: true, ..Default::default() },
            BackdropSettings {
                mode: BackdropMode::Gradient,
                shape: BackdropShape::Radial,
                colors: vec![[10, 20, 30], [200, 100, 50]],
                animate: true,
                ..Default::default()
            },
            BackdropSettings { mode: BackdropMode::Pattern, pattern: BackdropPattern::Stars, animate: true, ..Default::default() },
            BackdropSettings { mode: BackdropMode::Pattern, pattern: BackdropPattern::Aurora, animate: true, ..Default::default() },
        ];
        for s in looks {
            let look = resolve_look(&s, &t);
            let variant = variant_for(&s, false).unwrap();
            // The stamp `prepare` leaves for a frame at `time` in a `w`-wide window.
            let at = |time: f32, width: u32| {
                let f = BackdropFrame {
                    width,
                    height: 500,
                    slide_y: 0.0,
                    scroll_px: 0.0,
                    dpi: 1.0,
                    premultiply: true,
                    time,
                    image: None,
                };
                let full = build_uniform(&look, variant, &s, &t, &f);
                let (bake, stamp) = bake_stamp(&full, &look, variant, &s, &t, &f, 0);
                assert_eq!(bake.misc[0], time, "the bake shows the live clock");
                stamp
            };
            let baked = at(100.0, 800);
            let soon = at(100.0 + BAKE_MIN_INTERVAL * 0.5, 800);
            assert!(bake_is_fresh(Some(&baked), &soon, true), "{:?}/{:?}: re-baked inside one 30 fps frame", s.mode, s.pattern);
            assert!(!bake_is_fresh(Some(&baked), &at(100.0 + BAKE_MIN_INTERVAL * 1.5, 800), true), "{:?}: frozen", s.mode);
            // A real change inside the interval still re-bakes (a resize, an angle).
            let resized = at(100.0 + BAKE_MIN_INTERVAL * 0.5, 801);
            assert!(!bake_is_fresh(Some(&baked), &resized, true), "{:?}: a resize re-bakes", s.mode);
        }
        // The angle of an animated gradient lives in `geom` with the sway: a new
        // angle is a new look even inside the interval.
        let s = BackdropSettings { mode: BackdropMode::Gradient, animate: true, ..Default::default() };
        let turned = BackdropSettings { angle: s.angle + 30.0, ..s.clone() };
        let stamp = |s: &BackdropSettings| {
            let look = resolve_look(s, &t);
            let f = BackdropFrame {
                width: 800,
                height: 500,
                slide_y: 0.0,
                scroll_px: 0.0,
                dpi: 1.0,
                premultiply: true,
                time: 100.0,
                image: None,
            };
            let full = build_uniform(&look, Variant::Gradient, s, &t, &f);
            bake_stamp(&full, &look, Variant::Gradient, s, &t, &f, 0).1
        };
        assert!(!bake_is_fresh(Some(&stamp(&s)), &stamp(&turned), true));
    }

    #[test]
    fn parallax_margin_covers_the_shift() {
        for h in [1.0f32, 480.0, 1199.0, 2160.0] {
            let m = parallax_margin(h) as f32;
            assert!(m >= parallax_offset(1e9, h), "{h}: the shift stays inside the margin");
            assert!(m <= PARALLAX_MAX * h + 1.0);
        }
        assert_eq!(parallax_margin(0.0), 0);
    }

    /// `BackdropUniform` matches the WGSL `struct U` byte for byte.
    #[test]
    fn backdrop_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<BackdropUniform>(), 240);
        assert_eq!(offset_of!(BackdropUniform, resolution), 0);
        assert_eq!(offset_of!(BackdropUniform, offset), 8);
        assert_eq!(offset_of!(BackdropUniform, bg), 16);
        assert_eq!(offset_of!(BackdropUniform, stops), 32);
        assert_eq!(offset_of!(BackdropUniform, geom), 96);
        assert_eq!(offset_of!(BackdropUniform, look), 112);
        assert_eq!(offset_of!(BackdropUniform, img_xf), 128);
        assert_eq!(offset_of!(BackdropUniform, img_fx), 144);
        assert_eq!(offset_of!(BackdropUniform, misc), 160);
        assert_eq!(offset_of!(BackdropUniform, pc), 176);
        assert_eq!(offset_of!(BackdropUniform, guard), 224);
        assert_eq!(size_of::<BackdropUniform>() % 16, 0);
        assert_eq!(align_of::<BackdropUniform>(), 4);
        // naga agrees on the WGSL side: the struct is 240 bytes with the same
        // member offsets.
        let module = naga::front::wgsl::parse_str(BACKDROP_SHADER).unwrap();
        let ty = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("U"))
            .map(|(_, t)| t)
            .expect("struct U");
        let naga::TypeInner::Struct { members, span } = &ty.inner else { panic!("U is a struct") };
        assert_eq!(*span, 240);
        let offs: Vec<(String, u32)> =
            members.iter().map(|m| (m.name.clone().unwrap_or_default(), m.offset)).collect();
        let want = [
            ("resolution", 0),
            ("offset", 8),
            ("bg", 16),
            ("stops", 32),
            ("geom", 96),
            ("look", 112),
            ("img_xf", 128),
            ("img_fx", 144),
            ("misc", 160),
            ("pc", 176),
            ("guard", 224),
        ];
        for (name, off) in want {
            assert!(offs.contains(&(name.to_string(), off)), "{name} @ {off}: {offs:?}");
        }
    }

    #[test]
    fn parse_is_lenient() {
        assert_eq!(BackdropMode::parse("Theme"), BackdropMode::Theme);
        assert_eq!(BackdropMode::parse(" image "), BackdropMode::Image);
        assert_eq!(BackdropMode::parse("bogus"), BackdropMode::None);
        assert_eq!(BackdropFit::parse("TILE"), BackdropFit::Tile);
        assert_eq!(BackdropFit::parse(""), BackdropFit::Cover);
        assert_eq!(BackdropPattern::parse("synthwave"), BackdropPattern::Synthwave);
        assert_eq!(BackdropPattern::parse("nope"), BackdropPattern::Stars);
        assert_eq!(BackdropShape::parse("radial"), BackdropShape::Radial);
        assert_eq!(BackdropShape::parse("x"), BackdropShape::Linear);
        for m in [BackdropMode::None, BackdropMode::Theme, BackdropMode::Gradient, BackdropMode::Image, BackdropMode::Pattern] {
            assert_eq!(BackdropMode::parse(m.as_str()), m);
        }
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#1e1e2e"), Some([0x1e, 0x1e, 0x2e]));
        assert_eq!(parse_hex_color("FFB000"), Some([255, 176, 0]));
        assert_eq!(parse_hex_color("#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#gg0000"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn mode_none_builds_nothing() {
        let s = BackdropSettings::default();
        assert!(s.is_off());
        assert_eq!(variant_for(&s, false), None);
        assert_eq!(variant_for(&s, true), None);
        assert!(!s.animates());
    }

    #[test]
    fn variants_per_mode() {
        let mut s = BackdropSettings { mode: BackdropMode::Image, ..Default::default() };
        // A loading / broken image shows the base gradient.
        assert_eq!(variant_for(&s, false), Some(Variant::Gradient));
        assert_eq!(variant_for(&s, true), Some(Variant::Image));
        s.mode = BackdropMode::Pattern;
        s.pattern = BackdropPattern::Aurora;
        assert_eq!(variant_for(&s, false), Some(Variant::Baked));
        s.pattern = BackdropPattern::Grid;
        assert_eq!(variant_for(&s, false), Some(Variant::Grid));
        s.mode = BackdropMode::Theme;
        assert_eq!(variant_for(&s, false), Some(Variant::Gradient));
        // Images never animate; gradients / patterns do when asked.
        let img = BackdropSettings { mode: BackdropMode::Image, animate: true, ..Default::default() };
        assert!(!img.animates());
        assert!(BackdropSettings { mode: BackdropMode::Pattern, animate: true, ..Default::default() }.animates());
    }

    #[test]
    fn every_shipped_theme_has_a_curated_look() {
        for name in jetty_core::theme::PRESETS {
            assert!(theme_look(name).is_some(), "no curated backdrop for {name}");
        }
        // Ids are unique.
        let ids: Vec<&str> = curated_theme_ids().collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate ids in THEME_LOOKS");
        assert_eq!(theme_look("no_such_theme"), None);
    }

    #[test]
    fn unknown_themes_get_a_derived_look() {
        let mut t = theme("dracula");
        t.name = "my_custom_theme".into();
        let look = look_for_theme(&t);
        assert_eq!(look, derived_look(&t));
        // The derived accent is one of the theme's own colors.
        let a = hex(look.accent);
        assert!([4usize, 5, 12, 13, 6].iter().any(|&i| t.palette[i] == a), "{a:?}");
        // A light theme gets the subtle variant.
        let mut light = theme("solarized_light");
        light.name = "my_light".into();
        let l = derived_look(&light);
        assert!(l.glow < 0.3 && l.deep > 0.9, "{l:?}");
    }

    /// The curated glow/bg/deep stops keep the theme's text readable at low,
    /// default and full strength: at least 4.5:1, or 92 % of the theme's own
    /// fg/bg contrast when that is already lower.
    #[test]
    fn theme_looks_keep_text_readable() {
        for name in jetty_core::theme::PRESETS {
            let t = theme(name);
            let bg = [t.bg[0], t.bg[1], t.bg[2]];
            let floor = (crate::colors::contrast_ratio(t.fg, bg) * 0.92).min(4.5);
            assert_eq!(floor, readable_ratio(&t));
            let look = look_for_theme(&t);
            for strength in [0.25f32, 0.5, 1.0] {
                let k = (strength * look.gain).min(1.0);
                for stop in look_stops(&look, &t, k) {
                    let shown = mix_oklab(bg, stop, k);
                    let cr = crate::colors::contrast_ratio(t.fg, shown);
                    assert!(cr >= floor, "{name} at {strength}: {cr:.2} < {floor:.2} ({shown:?})");
                }
            }
        }
    }

    #[test]
    fn theme_mode_uses_the_curated_table_and_user_strength() {
        let t = theme("tokyo_night");
        let s = BackdropSettings { mode: BackdropMode::Theme, strength: 0.4, vignette: 0.3, ..Default::default() };
        let look = resolve_look(&s, &t);
        let tl = theme_look("tokyo_night").unwrap();
        assert_eq!(look.shape, tl.shape);
        assert_eq!(look.center, tl.center);
        assert_eq!(look.stops.len(), 3);
        assert_eq!(look.stops[1], [t.bg[0], t.bg[1], t.bg[2]], "the middle stop is the theme bg");
        assert!((look.strength - 0.4 * tl.gain).abs() < 1e-6);
        assert_eq!(look.vignette, 0.3_f32.max(tl.vignette));
    }

    #[test]
    fn gradient_mode_uses_user_colors() {
        let t = theme("nord");
        let s = BackdropSettings {
            mode: BackdropMode::Gradient,
            colors: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [9, 9, 9], [1, 1, 1]],
            shape: BackdropShape::Radial,
            ..Default::default()
        };
        let look = resolve_look(&s, &t);
        assert_eq!(look.stops.len(), 4, "at most 4 stops");
        assert_eq!(look.shape, BackdropShape::Radial);
        assert_eq!(look.center, [0.5, 0.5]);
        // Empty colors → the theme-derived stops.
        let derived = resolve_look(&BackdropSettings { mode: BackdropMode::Gradient, ..Default::default() }, &t);
        assert_eq!(derived.stops, look_stops(&look_for_theme(&t), &t, 0.5).to_vec());
    }

    #[test]
    fn fit_math() {
        let win = (1000.0, 500.0);
        // cover: a square image scales to the window width, centered vertically.
        let c = fit_transform(BackdropFit::Cover, (100.0, 100.0), 100.0, win, 1.0);
        assert!((1.0 / c.xf[0] - 1000.0).abs() < 1e-3 && (1.0 / c.xf[1] - 1000.0).abs() < 1e-3);
        let uv_top = 0.0 * c.xf[1] + c.xf[3];
        assert!((uv_top - 0.25).abs() < 1e-6, "cropped a quarter at the top: {uv_top}");
        // contain: scales to the window height, letterboxed left/right.
        let k = fit_transform(BackdropFit::Contain, (100.0, 100.0), 100.0, win, 1.0);
        assert!((1.0 / k.xf[0] - 500.0).abs() < 1e-3);
        let uv_left = 0.0 * k.xf[0] + k.xf[2];
        assert!((uv_left + 0.5).abs() < 1e-6, "250 px letterbox: {uv_left}");
        // stretch: each axis to the window.
        let s = fit_transform(BackdropFit::Stretch, (100.0, 100.0), 100.0, win, 1.0);
        assert_eq!(s.xf, [1.0 / 1000.0, 1.0 / 500.0, 0.0, 0.0]);
        // center: native size in the middle.
        let n = fit_transform(BackdropFit::Center, (200.0, 100.0), 200.0, win, 1.0);
        assert_eq!(n.xf, [1.0 / 200.0, 1.0 / 100.0, -400.0 / 200.0, -200.0 / 100.0]);
        // tile: native size from the top-left.
        let t = fit_transform(BackdropFit::Tile, (64.0, 32.0), 64.0, win, 1.0);
        assert_eq!(t.xf, [1.0 / 64.0, 1.0 / 32.0, 0.0, 0.0]);
        // Magnified images sample mip 0; a 4× minified one mip 2.
        assert_eq!(c.lod, 0.0);
        let m = fit_transform(BackdropFit::Cover, (4000.0, 2000.0), 4000.0, win, 1.0);
        assert!((m.lod - 2.0).abs() < 1e-4, "{}", m.lod);
        // A ¼-res frosted texture laid out at full size is never minified.
        let f = fit_transform(BackdropFit::Cover, (4000.0, 2000.0), 1000.0, win, 1.0);
        assert_eq!(f.lod, 0.0);
        // zoom enlarges cover around the center.
        let z = fit_transform(BackdropFit::Cover, (100.0, 100.0), 100.0, win, 1.1);
        assert!((1.0 / z.xf[0] - 1100.0).abs() < 1e-2);
    }

    #[test]
    fn smart_dim_math() {
        let white = 1.0;
        let near_black = 0.01;
        // Dark theme (light text): a bright image needs dimming.
        let need = smart_dim(0.0, 0.0, 0.8, white, near_black, 4.5);
        let target = (white + 0.05) / 4.5 - 0.05;
        let shown = 0.8 * (1.0 - need) + near_black * need;
        assert!((shown - target).abs() < 1e-4, "dims exactly to the 4.5:1 point");
        // Already dark enough: the user's dim is kept.
        assert_eq!(smart_dim(0.3, 0.0, 0.05, white, near_black, 4.5), 0.3);
        // A larger user dim always wins.
        assert_eq!(smart_dim(0.95, 0.0, 0.8, white, near_black, 4.5), 0.95);
        // Light theme (dark text): a dark image needs lifting toward the bg.
        let need = smart_dim(0.0, 0.02, 0.9, 0.01, 0.9, 4.5);
        let target = 4.5 * (0.01 + 0.05) - 0.05;
        let shown = 0.02 * (1.0 - need) + 0.9 * need;
        assert!((shown - target).abs() < 1e-4);
        // Impossible (bg itself too close to the text): full dim.
        assert_eq!(smart_dim(0.0, 0.0, 0.9, 0.2, 0.3, 4.5), 1.0);
        // A lower ratio (a low-contrast theme) needs less dim.
        assert!(smart_dim(0.0, 0.0, 0.8, white, near_black, 3.5) < smart_dim(0.0, 0.0, 0.8, white, near_black, 4.5));
        // On a real theme the ratio is the theme's readable ratio: 4.5 for a
        // high-contrast one, less for solarized (it may lose at most 8 %).
        assert_eq!(readable_ratio(&theme("ayu_dark")), 4.5);
        let sol = theme("solarized_light");
        let own = crate::colors::contrast_ratio(sol.fg, [sol.bg[0], sol.bg[1], sol.bg[2]]);
        assert!(own < 4.5 && (readable_ratio(&sol) - own * 0.92).abs() < 1e-5);
    }

    #[test]
    fn parallax_is_bounded_and_monotonic() {
        assert_eq!(parallax_offset(0.0, 800.0), 0.0);
        let mut prev = 0.0;
        for px in [10.0, 100.0, 1000.0, 1e6] {
            let o = parallax_offset(px, 800.0);
            assert!(o > prev && o <= PARALLAX_MAX * 800.0 + 1e-3, "{px}: {o}");
            prev = o;
        }
        // Starts at a quarter of the text's speed.
        assert!((parallax_offset(4.0, 800.0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn uniform_builder_basics() {
        let mut t = theme("catppuccin_mocha");
        t.bg[3] = 217; // opacity 0.85
        let s = BackdropSettings { mode: BackdropMode::Theme, ..Default::default() };
        let look = resolve_look(&s, &t);
        let f = BackdropFrame {
            width: 800,
            height: 600,
            slide_y: -120.0,
            scroll_px: 0.0,
            dpi: 1.0,
            premultiply: true,
            time: 5.0,
            image: None,
        };
        let u = build_uniform(&look, Variant::Gradient, &s, &t, &f);
        assert_eq!(u.resolution, [800.0, 600.0]);
        assert_eq!(u.offset, [0.0, -120.0], "the dropdown slide moves the backdrop");
        assert!((u.bg[3] - 217.0 / 255.0).abs() < 1e-6, "alpha = opacity");
        assert_eq!(u.misc[2], 1.0, "premultiply flag");
        assert_eq!(u.misc[0], 0.0, "time is 0 unless animating");
        assert_eq!(u.look[3], 3.0);
        assert_eq!(u.stops[0][3], 0.0);
        assert_eq!(u.stops[2][3], 1.0);
        // PostMultiplied surfaces get straight color.
        let u2 = build_uniform(&look, Variant::Gradient, &s, &t, &BackdropFrame { premultiply: false, ..f });
        assert_eq!(u2.misc[2], 0.0);
        // Animate passes the clock through.
        let sa = BackdropSettings { animate: true, ..s.clone() };
        assert_eq!(build_uniform(&look, Variant::Gradient, &sa, &t, &f).misc[0], 5.0);
        // Parallax shifts with the scrollback; without it nothing moves.
        let sp = BackdropSettings { parallax: true, ..s.clone() };
        let scrolled = BackdropFrame { scroll_px: 400.0, slide_y: 0.0, ..f };
        assert!(build_uniform(&look, Variant::Gradient, &sp, &t, &scrolled).offset[1] > 0.0);
        assert_eq!(build_uniform(&look, Variant::Gradient, &s, &t, &scrolled).offset[1], 0.0);
    }

    #[test]
    fn readability_bounds_keep_text_at_4_5() {
        // Dark theme: a cap at the text's 4.5:1 point, no floor.
        let dark = theme("ayu_dark");
        let [cap, floor, bg_y, _] = readability_bounds(&dark);
        assert_eq!(floor, 0.0);
        let fg_y = crate::colors::relative_luminance(dark.fg);
        assert!(((fg_y + 0.05) / (cap + 0.05) - 4.5).abs() < 1e-3, "cap is the 4.5:1 point");
        assert!(cap > bg_y, "the theme bg itself is never capped");
        // Light theme: a floor at the 4.5:1 point, no cap.
        let light = theme("solarized_light");
        let [cap, floor, bg_y, _] = readability_bounds(&light);
        assert_eq!(cap, 1.0);
        assert!(floor <= bg_y, "never above the theme bg");
        let fg_y = crate::colors::relative_luminance(light.fg);
        assert!(floor <= 4.5 * (fg_y + 0.05) - 0.05 + 1e-6);
        // A low-contrast dark theme (solarized: fg/bg ≈ 4.8:1 — the 4.5:1 point
        // sits close to its bg): the cap never drops below the bg luminance.
        for name in jetty_core::theme::PRESETS {
            let t = theme(name);
            let [cap, floor, bg_y, _] = readability_bounds(&t);
            assert!(cap >= bg_y - 1e-6 && floor <= bg_y + 1e-6, "{name}");
            assert!((0.0..=1.0).contains(&cap) && (0.0..=1.0).contains(&floor), "{name}");
        }
    }

    #[test]
    fn strength_zero_is_the_theme_bg() {
        // At strength 0 the base mixes 0 % of the stops: the shader shows the
        // bg (the stops' OKLab is irrelevant). Check the CPU mirror of the mix.
        let t = theme("dracula");
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        for stop in look_stops(&look_for_theme(&t), &t, 0.5) {
            assert_eq!(mix_oklab(bg, stop, 0.0), bg);
        }
    }

    #[test]
    fn oklab_round_trips() {
        for c in [[0u8, 0, 0], [255, 255, 255], [30, 30, 46], [255, 176, 0], [12, 200, 90]] {
            let back = oklab_to_srgb(srgb_to_oklab(c));
            for i in 0..3 {
                assert!((back[i] as i32 - c[i] as i32).abs() <= 1, "{c:?} → {back:?}");
            }
        }
    }

    /// GPU smoke test: build the layer and prepare every variant on a real
    /// device. `#[ignore]` (a GPU adapter may be unavailable in CI). Run:
    /// `cargo test -p jetty-render backdrop_gpu -- --ignored`.
    #[test]
    #[ignore]
    fn backdrop_gpu_prepares_every_variant() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))
        .expect("adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let mut bd = Backdrop::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let t = theme("tokyo_night");
        let img = crate::backdrop_image::prepare(
            crate::backdrop_image::RawImage { w: 8, h: 8, rgba: vec![200; 256] },
            8,
            8,
            0.0,
        );
        let gpu_img = Arc::new(GpuImage::upload(&device, &queue, &img).expect("upload"));
        let f = BackdropFrame {
            width: 64,
            height: 48,
            slide_y: 0.0,
            scroll_px: 0.0,
            dpi: 1.0,
            premultiply: true,
            time: 0.0,
            image: Some(&gpu_img),
        };
        for (mode, pattern) in [
            (BackdropMode::Theme, BackdropPattern::Stars),
            (BackdropMode::Gradient, BackdropPattern::Stars),
            (BackdropMode::Image, BackdropPattern::Stars),
            (BackdropMode::Pattern, BackdropPattern::Stars),
            (BackdropMode::Pattern, BackdropPattern::Aurora),
            (BackdropMode::Pattern, BackdropPattern::Grid),
            (BackdropMode::Pattern, BackdropPattern::Synthwave),
        ] {
            let s = BackdropSettings { mode, pattern, ..Default::default() };
            assert!(bd.prepare(&device, &queue, &s, &t, &f));
        }
        assert!(!bd.prepare(&device, &queue, &BackdropSettings::default(), &t, &f));

        // The cache bakes on change only: an unchanged frame, a dropdown slide
        // or an opacity change re-composite without re-baking; a theme change
        // or a resize bakes again.
        let s = BackdropSettings { mode: BackdropMode::Theme, ..Default::default() };
        let mut bd = Backdrop::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert!(bd.prepare(&device, &queue, &s, &t, &f));
        assert_eq!(bd.bake_count(), 1);
        assert!(bd.prepare(&device, &queue, &s, &t, &f));
        assert!(bd.prepare(&device, &queue, &s, &t, &BackdropFrame { slide_y: -20.0, ..f }));
        let mut translucent = t.clone();
        translucent.bg[3] = 200;
        assert!(bd.prepare(&device, &queue, &s, &translucent, &f));
        assert_eq!(bd.bake_count(), 1, "no re-bake for an unchanged look");
        assert!(bd.prepare(&device, &queue, &s, &theme("dracula"), &f));
        assert_eq!(bd.bake_count(), 2, "a theme change re-bakes");
        assert!(bd.prepare(&device, &queue, &s, &theme("dracula"), &BackdropFrame { width: 70, ..f }));
        assert_eq!(bd.bake_count(), 3, "a resize re-bakes");
        // Parallax: scrolling shifts the composite, never re-bakes.
        let sp = BackdropSettings { parallax: true, ..s };
        assert!(bd.prepare(&device, &queue, &sp, &t, &f));
        let n = bd.bake_count();
        assert!(bd.prepare(&device, &queue, &sp, &t, &BackdropFrame { scroll_px: 300.0, ..f }));
        assert_eq!(bd.bake_count(), n);
        // An animated look: repaints 10 ms apart share one bake; the next
        // 30 fps step bakes again.
        let sa = BackdropSettings { mode: BackdropMode::Pattern, animate: true, ..Default::default() };
        assert!(bd.prepare(&device, &queue, &sa, &t, &BackdropFrame { time: 5.0, ..f }));
        let n = bd.bake_count();
        assert!(bd.prepare(&device, &queue, &sa, &t, &BackdropFrame { time: 5.01, ..f }));
        assert!(bd.prepare(&device, &queue, &sa, &t, &BackdropFrame { time: 5.02, ..f }));
        assert_eq!(bd.bake_count(), n, "no re-bake inside one 30 fps frame");
        assert!(bd.prepare(&device, &queue, &sa, &t, &BackdropFrame { time: 5.05, ..f }));
        assert_eq!(bd.bake_count(), n + 1);
    }
}
