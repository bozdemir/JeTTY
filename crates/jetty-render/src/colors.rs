//! Theme-derived colors for the grid's selection highlight and block cursor,
//! shared by the live app and `jetty-shot` so the two can never drift.

/// The WCAG math is jetty-core's — the same that `minimum_contrast` pushes
/// text with — so a selected glyph, the cursor and the grid can never disagree
/// about what reads.
pub use jetty_core::contrast::{contrast_ratio, relative_luminance};

/// A selected glyph whose own color contrasts LESS than this with the selection
/// highlight is redrawn in the readable fallback color. Deliberately below WCAG's
/// 3:1 so only the genuinely unreadable cases change (blue/red text on the blue
/// highlight) while every readable color keeps its hue.
pub const SELECTION_MIN_CONTRAST: f32 = 2.0;

/// How selected glyphs are colored (see [`crate::GridPaint::selection`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionPaint {
    /// The selection highlight painted under selected cells.
    pub bg: [u8; 3],
    /// The theme's explicit `selection_fg`: every selected glyph takes it. `None`
    /// keeps each glyph's own color unless it would be unreadable on `bg`.
    pub fg: Option<[u8; 3]>,
    /// Replacement for a glyph below [`SELECTION_MIN_CONTRAST`] on `bg`: the theme
    /// fg or bg, whichever contrasts more with the highlight.
    pub fallback_fg: [u8; 3],
}

/// The selection highlight: the theme's explicit `selection_bg` when it sets one,
/// else a dim accent blend (1/3 theme bg + 2/3 palette blue), mirroring the
/// Settings panel's selected-row color so it reads on any theme.
pub fn selection_bg(theme: &jetty_core::Theme) -> [u8; 3] {
    if let Some(c) = theme.selection_bg {
        return c;
    }
    let bg = theme.bg;
    let accent = theme.palette[4];
    [
        ((bg[0] as u16 + accent[0] as u16 * 2) / 3) as u8,
        ((bg[1] as u16 + accent[1] as u16 * 2) / 3) as u8,
        ((bg[2] as u16 + accent[2] as u16 * 2) / 3) as u8,
    ]
}

/// The complete selection paint for `theme`.
pub fn selection_paint(theme: &jetty_core::Theme) -> SelectionPaint {
    let bg = selection_bg(theme);
    let tbg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    SelectionPaint { bg, fg: theme.selection_fg, fallback_fg: more_contrasting(bg, theme.fg, tbg) }
}

/// The color of the glyph under a SOLID block cursor (`cursor_rgb` = the live
/// cursor color, which OSC 12 may have changed). The theme's `cursor_text` when
/// set; otherwise the theme background — the classic inverted cell — unless that
/// is too close to the cursor color to read, in which case the theme foreground.
pub fn cursor_text_color(theme: &jetty_core::Theme, cursor_rgb: [u8; 3]) -> [u8; 3] {
    if let Some(c) = theme.cursor_text {
        return c;
    }
    let tbg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    if contrast_ratio(tbg, cursor_rgb) >= 3.0 {
        tbg
    } else {
        more_contrasting(cursor_rgb, tbg, theme.fg)
    }
}

/// The contrast the caret flash keeps at its peak — against the cell it sits on
/// (so the cursor never melts into the page) and against the glyph drawn on a
/// solid block (so the character under it stays readable). WCAG's 3:1 for UI
/// components.
pub const CARET_FLASH_MIN_CONTRAST: f32 = 3.0;

/// The color the caret flash animates toward. The configured `flash` color (0..1
/// RGB) when it keeps [`CARET_FLASH_MIN_CONTRAST`] against both `cell_bg` and
/// `glyph` (the character drawn on a solid block; pass `cell_bg` for the thin
/// shapes, which cover no glyph); otherwise black or white — whichever keeps the
/// larger of those two contrasts — unless the configured color still does better.
///
/// A white flash on a dark theme is therefore exactly what was configured, while
/// on a light theme it turns into a black one: flashing to white there drew a
/// near-white block on a near-white page with the glyph in the page color
/// (1.08:1 on solarized_light), so the cursor vanished for most of the burst.
pub fn caret_flash_target(flash: [f32; 3], cell_bg: [u8; 3], glyph: [u8; 3]) -> [f32; 3] {
    let to_u8 = |c: [f32; 3]| c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    let score = |c: [u8; 3]| contrast_ratio(c, cell_bg).min(contrast_ratio(c, glyph));
    let configured = score(to_u8(flash));
    if configured >= CARET_FLASH_MIN_CONTRAST {
        return flash;
    }
    let (white, black) = (score([255; 3]), score([0; 3]));
    let (best, best_score) = if black > white { ([0.0; 3], black) } else { ([1.0; 3], white) };
    if configured >= best_score {
        flash
    } else {
        best
    }
}

/// Whether `bg` reads as a LIGHT background: black text contrasts more with it
/// than white text does (relative luminance above ~0.18). Effects that brighten
/// (an additive glow) are invisible there and must darken instead.
pub fn is_light_bg(bg: [u8; 3]) -> bool {
    contrast_ratio(bg, [0, 0, 0]) > contrast_ratio(bg, [255, 255, 255])
}

/// Whichever of `a` / `b` contrasts more with `against` (ties → `a`).
fn more_contrasting(against: [u8; 3], a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    if contrast_ratio(b, against) > contrast_ratio(a, against) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_matches_wcag_endpoints() {
        assert!((contrast_ratio([0, 0, 0], [255, 255, 255]) - 21.0).abs() < 0.01);
        assert!((contrast_ratio([90, 90, 90], [90, 90, 90]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn catppuccin_blue_on_selection_is_unreadable_and_gets_a_fallback() {
        // The audit's case: palette blue text on the (blue-accent) highlight.
        let t = jetty_core::Theme::by_name("catppuccin_mocha");
        let p = selection_paint(&t);
        assert!(contrast_ratio(t.palette[4], p.bg) < SELECTION_MIN_CONTRAST);
        assert!(contrast_ratio(p.fallback_fg, p.bg) >= 3.0, "fallback must be readable");
        // Ordinary text on the highlight stays above the bar (keeps its color).
        assert!(contrast_ratio(t.fg, p.bg) >= SELECTION_MIN_CONTRAST);
    }

    #[test]
    fn every_builtin_theme_has_a_readable_selection_fallback_and_cursor_text() {
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let p = selection_paint(&t);
            assert!(
                contrast_ratio(p.fallback_fg, p.bg) >= SELECTION_MIN_CONTRAST,
                "{}: selection fallback unreadable",
                t.name
            );
            let c = cursor_text_color(&t, t.cursor);
            assert!(contrast_ratio(c, t.cursor) >= 2.0, "{}: cursor glyph unreadable", t.name);
        }
    }

    #[test]
    fn explicit_theme_colors_win() {
        let mut t = jetty_core::Theme::by_name("catppuccin_mocha");
        t.cursor_text = Some([1, 2, 3]);
        t.selection_fg = Some([4, 5, 6]);
        t.selection_bg = Some([7, 8, 9]);
        assert_eq!(cursor_text_color(&t, t.cursor), [1, 2, 3]);
        assert_eq!(selection_paint(&t).fg, Some([4, 5, 6]));
        assert_eq!(selection_bg(&t), [7, 8, 9]);
        assert_eq!(selection_paint(&t).bg, [7, 8, 9], "the grid highlight follows the theme too");
    }

    #[test]
    fn cursor_text_defaults_to_theme_bg() {
        let t = jetty_core::Theme::by_name("catppuccin_mocha");
        assert_eq!(cursor_text_color(&t, t.cursor), [t.bg[0], t.bg[1], t.bg[2]]);
    }
}
