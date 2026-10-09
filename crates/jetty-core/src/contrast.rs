//! WCAG contrast math for the grid: the opt-in `minimum_contrast` (text pushed
//! toward white or black until it reads against its cell background) and the
//! dark/light classification of a theme background that the DEC mode 2031
//! color-scheme reports (`CSI ? 997 ; 1|2 n`) and `COLORFGBG` are derived from.
//!
//! Pure functions on sRGB bytes; the per-channel sRGB → linear conversion is a
//! 256-entry table, so nothing here calls `powf` on the snapshot path.

use std::sync::OnceLock;

/// sRGB channel → linear light, as a 256-entry table.
fn srgb_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            *v = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
        }
        t
    })
}

/// WCAG relative luminance of an sRGB color (0.0 ..= 1.0).
#[inline]
pub fn relative_luminance(c: [u8; 3]) -> f32 {
    let l = srgb_lut();
    0.2126 * l[c[0] as usize] + 0.7152 * l[c[1] as usize] + 0.0722 * l[c[2] as usize]
}

/// WCAG contrast ratio of two relative luminances (1.0 ..= 21.0).
#[inline]
fn ratio_of(la: f32, lb: f32) -> f32 {
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG contrast ratio between two sRGB colors (1.0 ..= 21.0).
#[inline]
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    ratio_of(relative_luminance(a), relative_luminance(b))
}

/// Luminance below which a background counts as DARK: exactly where white text
/// starts to contrast more with it than black text does
/// (`(L + 0.05)² = 1.05 · 0.05`, i.e. L ≈ 0.179, about L* 49.5).
const DARK_BELOW: f32 = 0.179_129;

/// Whether `bg` is a dark background (white text reads better on it than black).
/// The single dark/light rule behind the DEC 2031 / `CSI ? 996 n` reports and
/// `COLORFGBG`, so a program sees one consistent answer.
pub fn is_dark(bg: [u8; 3]) -> bool {
    relative_luminance(bg) < DARK_BELOW
}

/// The `COLORFGBG` value advertising a theme with background `bg` to programs
/// that read it at startup (vim, mc, …): `"15;0"` (white on black) for a dark
/// background, `"0;15"` for a light one — the rxvt convention, where the last
/// field is the background's palette index.
pub fn colorfgbg(bg: [u8; 3]) -> &'static str {
    if is_dark(bg) { "15;0" } else { "0;15" }
}

/// Largest accepted `minimum_contrast` (WCAG's black-on-white maximum).
pub const MAX_RATIO: f32 = 21.0;

/// Sanitize a configured minimum contrast: non-finite → 1.0 (off), else clamped
/// to `1.0 ..= 21.0`.
pub fn clamp_ratio(r: f32) -> f32 {
    if r.is_finite() { r.clamp(1.0, MAX_RATIO) } else { 1.0 }
}

/// `fg` moved toward white or black until it contrasts at least `min` with `bg`.
///
/// The color is mixed in sRGB with the target, which keeps its hue (lightening
/// lowers the saturation, darkening the value). The preferred direction is away
/// from the background (lighten text that is lighter than its background,
/// darken text that is darker); when that side cannot reach `min` at all the
/// other side is used if it gets further. When neither can, the result is the
/// most contrasting color reachable (pure white or black). A color that already
/// meets `min` is returned unchanged.
pub fn ensure_contrast(fg: [u8; 3], bg: [u8; 3], min: f32) -> [u8; 3] {
    let lf = relative_luminance(fg);
    let lb = relative_luminance(bg);
    if ratio_of(lf, lb) >= min {
        return fg;
    }
    // The best each direction can do: pure white / pure black against `bg`.
    let up_max = ratio_of(1.0, lb);
    let down_max = ratio_of(0.0, lb);
    let lighten = if lf >= lb {
        up_max >= min || up_max >= down_max
    } else {
        !(down_max >= min || down_max >= up_max)
    };
    let target: [u8; 3] = if lighten { [255; 3] } else { [0; 3] };
    let mix = |t: f32| -> [u8; 3] {
        let ch = |c: u8, to: u8| (c as f32 + (to as f32 - c as f32) * t).round() as u8;
        [ch(fg[0], target[0]), ch(fg[1], target[1]), ch(fg[2], target[2])]
    };
    // Smallest mix that reaches `min` (bisection; 9 steps resolve the 8-bit
    // channel steps). `hi` always holds a color that meets it, or the extreme.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..9 {
        let mid = (lo + hi) * 0.5;
        if ratio_of(relative_luminance(mix(mid)), lb) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(hi)
}

/// Glyphs `minimum_contrast` never recolors, because their color IS the drawing
/// rather than text on a background: the powerline / Nerd Font separators and
/// shapes (U+E0B0–E0D7 — drawn in the NEXT segment's background color so the
/// arrow blends into it), block elements (U+2580–259F) and the sextant mosaics
/// (U+1FB00–1FB3B) that chafa / timg / notcurses paint pixel art with. The
/// powerline icons before them (U+E0A0–E0A3: branch, line number, lock) are
/// text in their segment's text color, and are adjusted with it.
#[inline]
pub fn min_contrast_exempt(c: char) -> bool {
    // Every exempt range lies above U+257F: one compare clears ordinary text.
    let u = c as u32;
    u >= 0x2580 && matches!(u, 0x2580..=0x259F | 0xE0B0..=0xE0D7 | 0x1FB00..=0x1FB3B)
}

/// Slots in [`ContrastMemo`]'s direct-mapped table.
const MEMO_SLOTS: usize = 16;

/// One memoized pair: `(fg, bg, readable fg)`.
type MemoEntry = ([u8; 3], [u8; 3], [u8; 3]);

/// Memo for [`ensure_contrast`] over one snapshot. Consecutive cells mostly
/// share one (fg, bg) pair, so a run costs one compare per cell (the `last`
/// entry, seeded with the theme's default pair); a change of colors looks in a
/// small direct-mapped table, so each distinct pair on screen is computed about
/// once per snapshot instead of once per color run.
pub(crate) struct ContrastMemo {
    min: f32,
    last: MemoEntry,
    /// The theme's default pair: colored runs mostly return to it.
    dflt: MemoEntry,
    table: [Option<MemoEntry>; MEMO_SLOTS],
}

impl ContrastMemo {
    pub(crate) fn new(min: f32, fg: [u8; 3], bg: [u8; 3]) -> Self {
        let dflt = (fg, bg, ensure_contrast(fg, bg, min));
        ContrastMemo { min, last: dflt, dflt, table: [None; MEMO_SLOTS] }
    }

    /// `fg` readable on `bg` (see [`ensure_contrast`]).
    #[inline]
    pub(crate) fn get(&mut self, fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
        if fg == self.last.0 && bg == self.last.1 {
            return self.last.2;
        }
        self.miss(fg, bg)
    }

    #[cold]
    fn miss(&mut self, fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
        if fg == self.dflt.0 && bg == self.dflt.1 {
            self.last = self.dflt;
            return self.dflt.2;
        }
        let h =(fg[0] ^ fg[1].rotate_left(2) ^ fg[2].rotate_left(4) ^ bg[0].rotate_left(1) ^ bg[1].rotate_left(3)
            ^ bg[2].rotate_left(5)) as usize
            % MEMO_SLOTS;
        let entry = match self.table[h] {
            Some(e) if e.0 == fg && e.1 == bg => e,
            _ => {
                let e = (fg, bg, ensure_contrast(fg, bg, self.min));
                self.table[h] = Some(e);
                e
            }
        };
        self.last = entry;
        entry.2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hue angle in degrees (None for a gray).
    fn hue(c: [u8; 3]) -> Option<f32> {
        let [r, g, b] = c.map(|v| v as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        if d < 1e-6 {
            return None;
        }
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        Some(h * 60.0)
    }

    #[test]
    fn ratio_matches_the_wcag_endpoints() {
        assert!((contrast_ratio([0, 0, 0], [255, 255, 255]) - 21.0).abs() < 0.01);
        assert!((contrast_ratio([90, 90, 90], [90, 90, 90]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dark_and_light_backgrounds_are_classified_by_which_text_reads_better() {
        for bg in [[0, 0, 0], [0x1e, 0x1e, 0x2e], [0x28, 0x2a, 0x36], [0x00, 0x2b, 0x36]] {
            assert!(is_dark(bg), "{bg:?} is dark");
            assert_eq!(colorfgbg(bg), "15;0");
        }
        for bg in [[255, 255, 255], [0xfd, 0xf6, 0xe3], [0xef, 0xf1, 0xf5], [0xfb, 0xf1, 0xc7]] {
            assert!(!is_dark(bg), "{bg:?} is light");
            assert_eq!(colorfgbg(bg), "0;15");
        }
        // The boundary is where white and black text contrast equally.
        for v in 0..=255u8 {
            let g = [v, v, v];
            let white = contrast_ratio([255; 3], g);
            let black = contrast_ratio([0; 3], g);
            if (white - black).abs() > 0.05 {
                assert_eq!(is_dark(g), white > black, "gray {v}");
            }
        }
    }

    #[test]
    fn ensure_contrast_reaches_the_ratio_with_the_smallest_change() {
        let bg = [0x00, 0x2b, 0x36]; // solarized dark base03
        let fg = [0x07, 0x36, 0x42]; // base02-ish: nearly invisible
        for min in [3.0f32, 4.5, 7.0] {
            let out = ensure_contrast(fg, bg, min);
            let r = contrast_ratio(out, bg);
            assert!(r >= min, "min {min}: got {r}");
            // Minimal: one channel step less mixing would miss the target —
            // check the result is not wildly above it.
            assert!(r < min + 0.6, "min {min}: overshot to {r}");
        }
    }

    #[test]
    fn already_readable_colors_are_untouched() {
        let bg = [0x1e, 0x1e, 0x2e];
        for fg in [[0xcd, 0xd6, 0xf4], [0xf3, 0x8b, 0xa8], [255, 255, 255]] {
            assert_eq!(ensure_contrast(fg, bg, 4.5), fg);
        }
        // min 1.0 = off: nothing ever changes.
        assert_eq!(ensure_contrast([0x1f, 0x1e, 0x2e], bg, 1.0), [0x1f, 0x1e, 0x2e]);
    }

    #[test]
    fn hue_is_kept_and_the_direction_follows_the_background() {
        // Dark bg: a dim blue gets LIGHTER, still blue.
        let dark = [0x1a, 0x1b, 0x26];
        let blue = [0x20, 0x30, 0x70];
        let out = ensure_contrast(blue, dark, 4.5);
        assert!(relative_luminance(out) > relative_luminance(blue));
        let (h0, h1) = (hue(blue).unwrap(), hue(out).unwrap());
        assert!((h0 - h1).abs() < 4.0, "hue {h0} → {h1}");
        // Light bg: a pale yellow gets DARKER, still yellow.
        let light = [0xfd, 0xf6, 0xe3];
        let yellow = [0xf0, 0xe0, 0x90];
        let out = ensure_contrast(yellow, light, 4.5);
        assert!(relative_luminance(out) < relative_luminance(yellow));
        assert!(contrast_ratio(out, light) >= 4.5);
        let (h0, h1) = (hue(yellow).unwrap(), hue(out).unwrap());
        assert!((h0 - h1).abs() < 4.0, "hue {h0} → {h1}");
    }

    #[test]
    fn a_blocked_direction_flips_to_the_side_that_can_reach_the_ratio() {
        // Mid-light gray bg: text slightly LIGHTER than it can't reach 4.5 by
        // lightening (white only gets ~2.3:1), so it darkens instead.
        let bg = [0xb0, 0xb0, 0xb0];
        let fg = [0xc0, 0xc0, 0xc0];
        assert!(contrast_ratio([255; 3], bg) < 4.5);
        let out = ensure_contrast(fg, bg, 4.5);
        assert!(contrast_ratio(out, bg) >= 4.5);
        assert!(relative_luminance(out) < relative_luminance(bg));
    }

    #[test]
    fn an_unreachable_ratio_gives_the_extreme() {
        // Nothing reaches 21:1 against mid-gray: the best side's extreme.
        let bg = [0x80, 0x80, 0x80];
        let out = ensure_contrast([0x90, 0x90, 0x90], bg, 21.0);
        assert!(out == [0, 0, 0] || out == [255, 255, 255], "{out:?}");
        let best = contrast_ratio([0; 3], bg).max(contrast_ratio([255; 3], bg));
        assert!((contrast_ratio(out, bg) - best).abs() < 0.01);
    }

    #[test]
    fn exemptions_cover_powerline_separators_blocks_and_sextants_only() {
        for c in ['\u{E0B0}', '\u{E0B6}', '\u{E0D7}', '█', '▀', '▐', '░', '\u{1FB00}', '\u{1FB3B}'] {
            assert!(min_contrast_exempt(c), "{:X}", c as u32);
        }
        // The powerline icons (branch, line number, lock, column) are text.
        for c in ['a', ' ', '─', '│', '⠿', '\u{E0A0}', '\u{E0A3}', '\u{E0AF}', '\u{E0D8}', '\u{E09F}', '\u{1FB3C}', '✔'] {
            assert!(!min_contrast_exempt(c), "{:X}", c as u32);
        }
    }

    #[test]
    fn memo_recomputes_only_on_a_new_pair() {
        let bg = [0x1e, 0x1e, 0x2e];
        let mut m = ContrastMemo::new(4.5, [0xcd, 0xd6, 0xf4], bg);
        assert_eq!(m.get([0xcd, 0xd6, 0xf4], bg), [0xcd, 0xd6, 0xf4]);
        let dim = [0x45, 0x47, 0x5a];
        let a = m.get(dim, bg);
        assert!(contrast_ratio(a, bg) >= 4.5);
        assert_eq!(m.get(dim, bg), a);
        assert_eq!(m.get(dim, [0xff, 0xff, 0xff]), ensure_contrast(dim, [0xff; 3], 4.5));
    }

    #[test]
    fn memo_table_is_exact_under_alternation_and_slot_collisions() {
        let mut m = ContrastMemo::new(4.5, [200, 200, 200], [0, 0, 0]);
        // 64 distinct pairs through 16 slots: every lookup must still be exact.
        let pairs: Vec<([u8; 3], [u8; 3])> =
            (0..64u8).map(|i| ([i.wrapping_mul(3), i, 255 - i], [i, 0, i / 2])).collect();
        for _ in 0..3 {
            for &(f, b) in pairs.iter().chain(pairs.iter().rev()) {
                assert_eq!(m.get(f, b), ensure_contrast(f, b, 4.5), "{f:?} on {b:?}");
            }
        }
    }

    #[test]
    fn clamp_ratio_sanitizes() {
        assert_eq!(clamp_ratio(f32::NAN), 1.0);
        assert_eq!(clamp_ratio(f32::INFINITY), 1.0);
        assert_eq!(clamp_ratio(0.2), 1.0);
        assert_eq!(clamp_ratio(4.5), 4.5);
        assert_eq!(clamp_ratio(99.0), 21.0);
    }
}
