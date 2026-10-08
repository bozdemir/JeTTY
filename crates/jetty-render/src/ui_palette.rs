//! `UiPalette` — every chrome color (menus, popups, pills, bars, overlays, the
//! Settings panel) derived from the active theme in ONE place, with WCAG contrast
//! floors enforced so the chrome reads on dark AND light themes.
//!
//! The legacy idiom was a fixed bg→fg blend per element (`lerp(0.40)` for a
//! hint, `lerp(0.85)` for a label, …). Fixed weights read on dark themes but
//! fall to 1.5–1.8:1 on light ones — luminance contrast is not linear in sRGB —
//! and a label laid over a colored fill (the accent, the cursor color) ignored
//! the fill entirely. Here every text role starts at its historical weight and
//! RISES only until its floor is met (a dark theme keeps its look, a light one
//! becomes readable), and text on a fill is chosen against that fill.
//!
//! Floors (WCAG contrast ratios, see [`crate::contrast_ratio`]):
//!
//! | role | floor | against |
//! |---|---|---|
//! | `text` | 4.5 | `surface` and `surface_hi` |
//! | `text_dim` | 4.5 | `surface` |
//! | `text_hint` | 3.0 | `surface` |
//! | `accent`, `danger`, `success`, `warn` | 3.0 | `surface` |
//! | `on_accent`, `on_danger`, `on_success`, `on_warn`, `on_cursor` | 4.5 | their fill |
//!
//! `surface`, `surface_hi` and `border` are decorative shades (no floor) and
//! keep today's exact blends (6 %, 18 %, 30 % of the way from bg to fg).
//!
//! Cost: [`UiPalette::from_theme`] is pure and takes a few µs. Per-frame
//! builders call [`UiPalette::cached`] instead — a one-entry memo keyed by the
//! theme's colors (a ~70-byte compare) — so a palette is computed once per theme
//! change on the render thread, never per frame, and the main window, detached
//! windows and jetty-shot all share it without any plumbing.

use std::cell::Cell;

use crate::colors::{contrast_ratio, relative_luminance};

/// Theme-derived chrome colors (all RGB unless noted). See the module docs for
/// the contrast floor every role meets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPalette {
    /// Card / menu / popup / status-strip fill: bg lifted 6 % toward fg.
    pub surface: [u8; 3],
    /// A raised fill on a surface — selected rows, secondary buttons (18 %).
    pub surface_hi: [u8; 3],
    /// Card halo, dividers (30 %). Decorative: no floor.
    pub border: [u8; 3],
    /// Primary text: the theme fg, pushed toward black/white only when it would
    /// fall under 4.5:1 on `surface` or `surface_hi`.
    pub text: [u8; 3],
    /// Secondary text (descriptions, unselected rows): ≥ 4.5:1 on `surface`.
    pub text_dim: [u8; 3],
    /// Tertiary text (shortcut hints, placeholders, counters, disabled rows) and
    /// thin UI marks such as scroll thumbs: ≥ 3:1 on `surface`.
    pub text_hint: [u8; 3],
    /// The accent: the theme's `accent`, else ANSI blue (`palette[4]`), else
    /// bright blue (`palette[12]`), else a shade of the first that reaches 3:1.
    pub accent: [u8; 3],
    /// Text on an `accent` fill.
    pub on_accent: [u8; 3],
    /// Error / destructive red (`palette[1]`, else `palette[9]`, else a shade).
    pub danger: [u8; 3],
    /// Text on a `danger` fill.
    pub on_danger: [u8; 3],
    /// Confirm / OK green (`palette[2]`, else `palette[10]`, else a shade).
    pub success: [u8; 3],
    /// Text on a `success` fill.
    pub on_success: [u8; 3],
    /// Highlight / attention yellow (`palette[3]`, else `palette[11]`, else a
    /// shade) — search matches, hint chips.
    pub warn: [u8; 3],
    /// Text on a `warn` fill.
    pub on_warn: [u8; 3],
    /// Modal backdrop dim, RGBA, drawn over the whole window under a popup.
    pub scrim: [u8; 4],
    /// The selection highlight (grid and lists): the theme's `selection_bg`, else
    /// the derived blend — always equal to [`crate::selection_bg`].
    pub selection_bg: [u8; 3],
    /// A light theme: the background is brighter than the foreground.
    pub is_light: bool,
    /// The theme background (RGB; the window's global opacity is NOT applied).
    pub bg: [u8; 3],
    /// The theme foreground.
    pub fg: [u8; 3],
    /// The theme cursor color (the fill of the toast / copy-mode pills).
    pub cursor: [u8; 3],
    /// Text on a `cursor` fill.
    pub on_cursor: [u8; 3],
}

/// The memo key: every theme input [`UiPalette::from_theme`] reads. The bg
/// alpha (the live opacity) is deliberately absent — it never changes a role.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    bg: [u8; 3],
    fg: [u8; 3],
    cursor: [u8; 3],
    palette: [[u8; 3]; 16],
    accent: Option<[u8; 3]>,
    selection_bg: Option<[u8; 3]>,
}

impl Key {
    fn of(theme: &jetty_core::Theme) -> Key {
        Key {
            bg: [theme.bg[0], theme.bg[1], theme.bg[2]],
            fg: theme.fg,
            cursor: theme.cursor,
            palette: theme.palette,
            accent: theme.accent,
            selection_bg: theme.selection_bg,
        }
    }
}

thread_local! {
    /// One-entry memo for [`UiPalette::cached`]. Rendering is single-threaded, so
    /// a thread-local needs no lock; another thread simply has its own entry.
    static MEMO: Cell<Option<(Key, UiPalette)>> = const { Cell::new(None) };
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

impl UiPalette {
    /// Floor of `text` (on `surface` and `surface_hi`) and `text_dim` (on `surface`).
    pub const TEXT_FLOOR: f32 = 4.5;
    /// Floor of `text_hint` on `surface`.
    pub const HINT_FLOOR: f32 = 3.0;
    /// Floor of `accent` / `danger` / `success` / `warn` on `surface`.
    pub const ACCENT_FLOOR: f32 = 3.0;
    /// Floor of every `on_*` color on its fill.
    pub const ON_FILL_FLOOR: f32 = 4.5;

    /// Historical blend weights (bg→fg) of the decorative shades and the start
    /// weights of the rising text roles.
    const SURFACE_T: f32 = 0.06;
    const SURFACE_HI_T: f32 = 0.18;
    const BORDER_T: f32 = 0.30;
    const TEXT_DIM_T: f32 = 0.60;
    const TEXT_HINT_T: f32 = 0.40;

    /// Derive the palette from `theme` (pure; a few µs). Per-frame callers use
    /// [`Self::cached`].
    pub fn from_theme(theme: &jetty_core::Theme) -> UiPalette {
        let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
        let fg = theme.fg;
        let p = &theme.palette;
        let surface = mix(bg, fg, Self::SURFACE_T);
        let surface_hi = mix(bg, fg, Self::SURFACE_HI_T);
        let border = mix(bg, fg, Self::BORDER_T);

        let text = ensure_contrast(fg, &[surface, surface_hi], Self::TEXT_FLOOR);
        // Blend toward the FINAL text color (not the raw fg): on a low-contrast
        // theme whose fg had to be pushed, dim/hint keep their hierarchy below it.
        let text_dim = rise(bg, text, Self::TEXT_DIM_T, surface, Self::TEXT_FLOOR);
        let text_hint = rise(bg, text, Self::TEXT_HINT_T, surface, Self::HINT_FLOOR);

        let accent = match theme.accent {
            Some(a) => first_readable(&[a], surface),
            None => first_readable(&[p[4], p[12]], surface),
        };
        let danger = first_readable(&[p[1], p[9]], surface);
        let success = first_readable(&[p[2], p[10]], surface);
        let warn = first_readable(&[p[3], p[11]], surface);

        let is_light = relative_luminance(bg) > relative_luminance(fg);
        // Dark themes keep the classic 59 % black. On a light theme that turned
        // the page a muddy mid-gray; a lighter dim still sets the popup apart.
        let scrim = if is_light { [0, 0, 0, 96] } else { [0, 0, 0, 150] };

        let on = |fill: [u8; 3]| on_fill_of(fill, bg, fg);
        UiPalette {
            surface,
            surface_hi,
            border,
            text,
            text_dim,
            text_hint,
            accent,
            on_accent: on(accent),
            danger,
            on_danger: on(danger),
            success,
            on_success: on(success),
            warn,
            on_warn: on(warn),
            scrim,
            selection_bg: crate::colors::selection_bg(theme),
            is_light,
            bg,
            fg,
            cursor: theme.cursor,
            on_cursor: on(theme.cursor),
        }
    }

    /// [`Self::from_theme`] through a one-entry memo keyed by the theme's colors:
    /// recomputed only when the theme (not the opacity) changes. Cheap enough to
    /// call from every per-frame builder.
    pub fn cached(theme: &jetty_core::Theme) -> UiPalette {
        let key = Key::of(theme);
        MEMO.with(|memo| {
            if let Some((k, ui)) = memo.get() {
                if k == key {
                    return ui;
                }
            }
            let ui = UiPalette::from_theme(theme);
            memo.set(Some((key, ui)));
            ui
        })
    }

    /// The legacy bg→fg blend at `t` (0 = bg, 1 = fg), for decorative shades
    /// that have no contrast floor (hairlines, slider tracks, control fills).
    pub fn shade(&self, t: f32) -> [u8; 3] {
        mix(self.bg, self.fg, t)
    }

    /// The text color for an arbitrary `fill` (a per-tab color, a badge): the
    /// theme bg or fg when one reaches 4.5:1 on it, else black or white.
    pub fn on_fill(&self, fill: [u8; 3]) -> [u8; 3] {
        on_fill_of(fill, self.bg, self.fg)
    }

    /// `color` as a label on `surface`: unchanged when it already contrasts
    /// `floor`:1, else nudged toward black/white just far enough (theme colors
    /// used as text — a header in the cursor hue, a per-tab color).
    pub fn readable(&self, color: [u8; 3], floor: f32) -> [u8; 3] {
        ensure_contrast(color, &[self.surface], floor)
    }
}

/// Linear sRGB-space blend `a → b` at `t` (0 = `a`, 1 = `b`), rounded — the same
/// arithmetic as the chrome's historical `lerp` closures.
pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [ch(a[0], b[0]), ch(a[1], b[1]), ch(a[2], b[2])]
}

/// The lowest contrast of `c` against any of `against`.
fn min_contrast(c: [u8; 3], against: &[[u8; 3]]) -> f32 {
    against.iter().map(|&s| contrast_ratio(c, s)).fold(f32::INFINITY, f32::min)
}

/// `c` itself when it contrasts at least `floor`:1 with every color in
/// `against`; otherwise `c` moved toward black or white — whichever can contrast
/// more — just far enough to pass (or all the way, when even that falls short:
/// some floor/background pairs above ~4.6:1 are unreachable).
pub fn ensure_contrast(c: [u8; 3], against: &[[u8; 3]], floor: f32) -> [u8; 3] {
    if min_contrast(c, against) >= floor {
        return c;
    }
    let target = if min_contrast(BLACK, against) >= min_contrast(WHITE, against) { BLACK } else { WHITE };
    const STEPS: u32 = 64;
    for i in 1..=STEPS {
        let m = mix(c, target, i as f32 / STEPS as f32);
        if min_contrast(m, against) >= floor {
            return m;
        }
    }
    target
}

/// The first bg→`to` blend, from weight `t0` upward in 1 % steps, that contrasts
/// at least `floor`:1 with `surface`; `to` itself (or `to` made to pass) when
/// none does.
fn rise(bg: [u8; 3], to: [u8; 3], t0: f32, surface: [u8; 3], floor: f32) -> [u8; 3] {
    let start = (t0.clamp(0.0, 1.0) * 100.0).round() as u32;
    for k in start..=100 {
        let c = mix(bg, to, k as f32 / 100.0);
        if contrast_ratio(c, surface) >= floor {
            return c;
        }
    }
    ensure_contrast(to, &[surface], floor)
}

/// The first of `candidates` that reaches [`UiPalette::ACCENT_FLOOR`] on
/// `surface`, else the first one shaded until it does. A later candidate (the
/// bright ANSI slot) only stands in for the first when it is the same color
/// family — Solarized's "bright" slots are its gray base tones, and a gray must
/// not replace its green or blue.
fn first_readable(candidates: &[[u8; 3]], surface: [u8; 3]) -> [u8; 3] {
    let first = candidates[0];
    candidates
        .iter()
        .copied()
        .enumerate()
        .find(|&(i, c)| {
            (i == 0 || same_family(first, c)) && contrast_ratio(c, surface) >= UiPalette::ACCENT_FLOOR
        })
        .map(|(_, c)| c)
        .unwrap_or_else(|| ensure_contrast(first, &[surface], UiPalette::ACCENT_FLOOR))
}

/// Hue (degrees) and chroma (0..=1, max − min channel) of an sRGB color.
fn hue_chroma(c: [u8; 3]) -> (f32, f32) {
    let [r, g, b] = c.map(|v| v as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    if chroma <= 0.0 {
        return (0.0, 0.0);
    }
    let h = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    (h * 60.0, chroma)
}

/// Whether `b` can stand in for `a`: both near-gray, or both colored with hues
/// within 40° (gruvbox's bright blue for its blue: yes; a gray for a green: no).
fn same_family(a: [u8; 3], b: [u8; 3]) -> bool {
    const GRAY: f32 = 0.08;
    let ((ha, ca), (hb, cb)) = (hue_chroma(a), hue_chroma(b));
    match (ca < GRAY, cb < GRAY) {
        (true, true) => true,
        (false, false) => {
            let d = (ha - hb).abs();
            d.min(360.0 - d) <= 40.0
        }
        _ => false,
    }
}

/// Text for `fill`: whichever of the theme `bg` / `fg` contrasts more, when it
/// reaches [`UiPalette::ON_FILL_FLOOR`] (keeps the theme's own tones); otherwise
/// black or white, whichever contrasts more (always ≥ 4.58:1).
fn on_fill_of(fill: [u8; 3], bg: [u8; 3], fg: [u8; 3]) -> [u8; 3] {
    let (cb, cf) = (contrast_ratio(bg, fill), contrast_ratio(fg, fill));
    let (theme_best, theme_c) = if cf > cb { (fg, cf) } else { (bg, cb) };
    if theme_c >= UiPalette::ON_FILL_FLOOR {
        return theme_best;
    }
    if contrast_ratio(BLACK, fill) >= contrast_ratio(WHITE, fill) {
        BLACK
    } else {
        WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetty_core::theme::{theme_at, PRESETS};

    fn builtins() -> Vec<jetty_core::Theme> {
        (0..PRESETS.len()).map(theme_at).collect()
    }

    /// Every built-in theme × every role meets its documented floor.
    #[test]
    fn every_builtin_meets_every_floor() {
        for t in builtins() {
            let ui = UiPalette::from_theme(&t);
            let n = &t.name;
            let c = contrast_ratio;
            assert!(c(ui.text, ui.surface) >= 4.5, "{n}: text on surface {}", c(ui.text, ui.surface));
            assert!(c(ui.text, ui.surface_hi) >= 4.5, "{n}: text on surface_hi {}", c(ui.text, ui.surface_hi));
            assert!(c(ui.text_dim, ui.surface) >= 4.5, "{n}: text_dim {}", c(ui.text_dim, ui.surface));
            assert!(c(ui.text_hint, ui.surface) >= 3.0, "{n}: text_hint {}", c(ui.text_hint, ui.surface));
            for (role, col) in [("accent", ui.accent), ("danger", ui.danger), ("success", ui.success), ("warn", ui.warn)] {
                assert!(c(col, ui.surface) >= 3.0, "{n}: {role} on surface {}", c(col, ui.surface));
            }
            for (role, fill, on) in [
                ("accent", ui.accent, ui.on_accent),
                ("danger", ui.danger, ui.on_danger),
                ("success", ui.success, ui.on_success),
                ("warn", ui.warn, ui.on_warn),
                ("cursor", ui.cursor, ui.on_cursor),
            ] {
                assert!(c(on, fill) >= 4.5, "{n}: on_{role} {}", c(on, fill));
            }
            assert!(ui.scrim[3] > 0, "{n}: the scrim must dim");
            assert_eq!(ui.selection_bg, crate::colors::selection_bg(&t), "{n}: selection_bg");
        }
    }

    /// The hierarchy holds on every theme: text ≥ text_dim ≥ text_hint (in
    /// contrast against the surface), so "dim" never out-shouts "primary".
    #[test]
    fn text_hierarchy_is_ordered_on_every_builtin() {
        for t in builtins() {
            let ui = UiPalette::from_theme(&t);
            let c = |x| contrast_ratio(x, ui.surface);
            assert!(c(ui.text) >= c(ui.text_dim) - 1e-4, "{}: dim brighter than text", t.name);
            assert!(c(ui.text_dim) >= c(ui.text_hint) - 1e-4, "{}: hint brighter than dim", t.name);
        }
    }

    /// Dark themes keep today's exact shades: the surface/raised/border blends,
    /// fg as the text, ANSI blue as the accent and the classic 59 % black scrim.
    #[test]
    fn a_dark_theme_keeps_todays_look() {
        let t = jetty_core::Theme::by_name("catppuccin_mocha");
        let ui = UiPalette::from_theme(&t);
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        assert_eq!(ui.surface, mix(bg, t.fg, 0.06));
        assert_eq!(ui.surface_hi, mix(bg, t.fg, 0.18));
        assert_eq!(ui.border, mix(bg, t.fg, 0.30));
        assert_eq!(ui.text, t.fg);
        assert_eq!(ui.accent, t.palette[4]);
        assert_eq!(ui.success, t.palette[2]);
        assert_eq!(ui.scrim, [0, 0, 0, 150]);
        assert!(!ui.is_light);
        // Text on the blue accent is the theme's own dark bg, not stark black.
        assert_eq!(ui.on_accent, bg);
    }

    #[test]
    fn light_themes_are_detected_and_get_a_lighter_scrim() {
        let light = UiPalette::from_theme(&jetty_core::Theme::by_name("solarized_light"));
        assert!(light.is_light);
        assert!(light.scrim[3] < 150, "a lighter dim than the dark themes'");
        assert!(!UiPalette::from_theme(&jetty_core::Theme::by_name("ayu_dark")).is_light);
    }

    /// The fixed 0.40 hint blend measured ~1.8:1 on Solarized Light; the rising
    /// weight lifts it over its floor, while a dark theme's hint barely moves.
    #[test]
    fn hints_rise_only_as_far_as_needed() {
        let light = jetty_core::Theme::by_name("solarized_light");
        let lbg = [light.bg[0], light.bg[1], light.bg[2]];
        let ui = UiPalette::from_theme(&light);
        let old = mix(lbg, light.fg, 0.40);
        assert!(contrast_ratio(old, ui.surface) < 3.0, "the legacy blend was unreadable");
        assert!(contrast_ratio(ui.text_hint, ui.surface) >= 3.0);
        // Minimal: one step less would miss the floor.
        let mocha = UiPalette::from_theme(&jetty_core::Theme::by_name("catppuccin_mocha"));
        assert!(contrast_ratio(mocha.text_hint, mocha.surface) < 3.2, "no overshoot on a dark theme");
    }

    #[test]
    fn a_weak_accent_falls_back_to_bright_blue_then_a_shade() {
        let mut t = jetty_core::Theme::by_name("catppuccin_mocha");
        // A navy palette[4] all but vanishes on the dark surface → the bright
        // blue (palette[12]) is used.
        t.palette[4] = [30, 40, 90];
        t.palette[12] = [120, 170, 255];
        assert_eq!(UiPalette::from_theme(&t).accent, [120, 170, 255]);
        // Both faint → a shade of palette[4] that reaches the floor.
        t.palette[12] = [35, 45, 95];
        let ui = UiPalette::from_theme(&t);
        assert!(contrast_ratio(ui.accent, ui.surface) >= 3.0);
        assert!(contrast_ratio(ui.on_accent, ui.accent) >= 4.5);
        assert!(hue_chroma(ui.accent).1 > 0.1, "still a blue, not a gray: {:?}", ui.accent);
    }

    #[test]
    fn a_bright_slot_of_another_family_never_replaces_the_hue() {
        // Solarized Light: green #859900 is too faint on the surface and bright
        // green is the gray base tone #586e75 — the role stays a (darker) GREEN.
        let t = jetty_core::Theme::by_name("solarized_light");
        let ui = UiPalette::from_theme(&t);
        assert_ne!(ui.success, t.palette[10], "a gray must not stand in for green");
        let (h, c) = hue_chroma(ui.success);
        let (h0, _) = hue_chroma(t.palette[2]);
        assert!(c > 0.2 && (h - h0).abs() < 15.0, "kept the green hue: {:?}", ui.success);
        // Gruvbox Dark: its bright blue IS a blue, so it is used as is.
        let g = jetty_core::Theme::by_name("gruvbox_dark");
        assert!(same_family(g.palette[4], g.palette[12]));
        assert!(!same_family(t.palette[2], t.palette[10]));
        assert!(same_family([90, 90, 90], [200, 200, 205]), "two grays are one family");
    }

    #[test]
    fn an_explicit_accent_wins_and_is_still_kept_readable() {
        let mut t = jetty_core::Theme::by_name("catppuccin_mocha");
        t.accent = Some([250, 100, 50]);
        assert_eq!(UiPalette::from_theme(&t).accent, [250, 100, 50]);
        // An accent the surface swallows is nudged until it reads.
        t.accent = Some([40, 40, 60]);
        let ui = UiPalette::from_theme(&t);
        assert!(contrast_ratio(ui.accent, ui.surface) >= 3.0);
    }

    #[test]
    fn an_explicit_selection_bg_is_the_selection_role() {
        let mut t = jetty_core::Theme::by_name("nord");
        t.selection_bg = Some([1, 2, 3]);
        assert_eq!(UiPalette::from_theme(&t).selection_bg, [1, 2, 3]);
    }

    /// A degenerate theme (fg == bg) still yields readable chrome.
    #[test]
    fn a_degenerate_theme_still_meets_the_floors() {
        for gray in [0u8, 60, 118, 128, 200, 255] {
            let mut t = jetty_core::Theme::by_name("catppuccin_mocha");
            t.bg = [gray, gray, gray, 255];
            t.fg = [gray, gray, gray];
            t.palette = [[gray, gray, gray]; 16];
            t.cursor = [gray, gray, gray];
            let ui = UiPalette::from_theme(&t);
            assert!(contrast_ratio(ui.text, ui.surface) >= 4.5, "gray {gray}");
            assert!(contrast_ratio(ui.text_hint, ui.surface) >= 3.0, "gray {gray}");
            assert!(contrast_ratio(ui.accent, ui.surface) >= 3.0, "gray {gray}");
            assert!(contrast_ratio(ui.on_cursor, ui.cursor) >= 4.5, "gray {gray}");
        }
    }

    #[test]
    fn on_fill_is_readable_on_any_fill_and_prefers_theme_tones() {
        let ui = UiPalette::from_theme(&jetty_core::Theme::by_name("catppuccin_mocha"));
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(15) {
                for b in (0..=255u16).step_by(51) {
                    let fill = [r as u8, g as u8, b as u8];
                    let on = ui.on_fill(fill);
                    assert!(contrast_ratio(on, fill) >= 4.5, "{fill:?} → {on:?}");
                }
            }
        }
        // A bright fill takes the theme's dark bg rather than pure black.
        assert_eq!(ui.on_fill([250, 230, 180]), ui.bg);
    }

    #[test]
    fn ensure_contrast_moves_only_as_needed() {
        // Already fine → unchanged.
        assert_eq!(ensure_contrast([255, 255, 255], &[[0, 0, 0]], 4.5), [255, 255, 255]);
        // Too close → pushed away, and it passes.
        let c = ensure_contrast([60, 60, 60], &[[50, 50, 50]], 4.5);
        assert!(contrast_ratio(c, [50, 50, 50]) >= 4.5, "{c:?}");
        // Against a light color the push goes dark.
        let c = ensure_contrast([200, 200, 200], &[[230, 230, 230]], 3.0);
        assert!(relative_luminance(c) < relative_luminance([200, 200, 200]), "{c:?}");
    }

    #[test]
    fn cached_matches_from_theme_and_follows_theme_changes() {
        let a = jetty_core::Theme::by_name("dracula");
        let b = jetty_core::Theme::by_name("solarized_light");
        assert_eq!(UiPalette::cached(&a), UiPalette::from_theme(&a));
        assert_eq!(UiPalette::cached(&b), UiPalette::from_theme(&b), "a theme switch recomputes");
        assert_eq!(UiPalette::cached(&a), UiPalette::from_theme(&a));
        // The live opacity (bg alpha) never changes a role → same palette.
        let mut faded = a.clone();
        faded.bg[3] = 90;
        assert_eq!(UiPalette::cached(&faded), UiPalette::from_theme(&a));
        // An accent edit (a theme-file hot reload) is a new key.
        let mut edited = a.clone();
        edited.accent = Some([255, 0, 128]);
        assert_eq!(UiPalette::cached(&edited).accent, [255, 0, 128]);
    }

    #[test]
    fn shade_is_the_legacy_blend() {
        let t = jetty_core::Theme::by_name("gruvbox_dark");
        let ui = UiPalette::from_theme(&t);
        assert_eq!(ui.shade(0.0), ui.bg);
        assert_eq!(ui.shade(1.0), ui.fg);
        assert_eq!(ui.shade(0.06), ui.surface);
    }
}
