//! Overlay draw data for HINT MODE (Ctrl+Shift+H) and keyboard COPY-MODE
//! (Ctrl+Shift+Space).
//!
//! Same surface language as `search_bar.rs` / `help.rs`: all colors derive from
//! the active theme (its `UiPalette`, no hardcoded RGB), metrics scale with the
//! chrome unit (DPI × UI font) and label widths are MEASURED as drawn, so both
//! overlays are HiDPI- and proportional-font-correct. Nothing here self-drives frames — the app draws
//! these only while a mode is active, once per event-driven redraw.

use crate::chrome::{ChromeMeasure, ChromeMetrics};
use crate::quad::SCROLLBAR_W;
use crate::ui_palette::{ensure_contrast, mix, rgba, UiPalette};
use crate::Rect;

/// Geometry + draw data for the hint-mode label chips.
pub struct HintOverlay {
    /// Rounded chip backgrounds (one per drawn token), draw order.
    pub quads: Vec<Rect>,
    /// Chip label text segments: (text, x, y, rgb).
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
}

/// Build the hint-mode chips. `labeled` is `(label, vp_row, col_start)` — the
/// label string and the FIRST visible cell of each token, in reading order.
/// `typed` is the already-typed prefix (drawn dimmer so the user sees narrowing);
/// callers pass only labels that still match. Chips are clamped inside the grid
/// (clear of the `SCROLLBAR_W` gutter) and a chip that would overlap an already-
/// placed chip on the same row is skipped (bounds visual clutter on a dense
/// screen — the token is still copyable once the overlapping one is resolved).
///
/// `m` / `cm` are the measurer and metrics of the font the labels are DRAWN
/// in. Chips are one grid row (`cell_h`) tall, so the app draws their labels in
/// the terminal grid font (its grid layer + `ChromeMetrics` of the terminal
/// font size): a large UI font would overflow the row.
#[allow(clippy::too_many_arguments)]
pub fn build_hint_overlay(
    labeled: &[(&str, usize, usize)],
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    typed: &str,
    win_w: u32,
) -> HintOverlay {
    // Chip fill: a strong bg→ANSI-yellow blend (bright, opaque — like a search
    // hit). Its label takes the fill's own readable color (the theme bg alone
    // fell to ~2.7:1 on light themes' darker yellows); the consumed prefix is a
    // dimmer shade of it, still ≥ 3:1, so narrowing stays visible.
    let ui = UiPalette::cached(theme);
    let chip_rgb = mix(ui.bg, theme.palette[3], 0.9);
    let chip_bg = rgba(chip_rgb, 255);
    let text_full = ui.on_fill(chip_rgb);
    let text_typed = ensure_contrast(mix(text_full, chip_rgb, 0.45), &[chip_rgb], UiPalette::HINT_FLOOR);

    let vscale = cm.overlay_u();
    let pad_x = (3.0 * vscale).max(2.0);
    let text_h = 16.0 * vscale;
    let radius = (cell_h * 0.25).min(6.0);
    let max_x = (win_w as f32 - cm.dpx(SCROLLBAR_W)).max(0.0);

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Per-row right edge of the last placed chip → skip an overlapping chip.
    let mut row_last_end: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();

    for (label, row, col) in labeled {
        let text_w = m.text_w(label);
        let chip_w = text_w + pad_x * 2.0;
        let mut x = *col as f32 * cell_w;
        if x + chip_w > max_x {
            x = (max_x - chip_w).max(0.0);
        }
        // Skip a chip that would overlap one already placed on this row.
        if let Some(&end) = row_last_end.get(row) {
            if x < end {
                continue;
            }
        }
        let y = y_offset + *row as f32 * cell_h;
        quads.push(Rect::rounded(x, y, chip_w, cell_h, chip_bg, radius));
        let ty = y + (cell_h - text_h) / 2.0;
        let tx = x + pad_x;
        if !typed.is_empty() && label.starts_with(typed) {
            labels.push((typed.to_string(), tx, ty, text_typed));
            let rest: String = label.chars().skip(typed.chars().count()).collect();
            if !rest.is_empty() {
                let rx = tx + m.text_w(typed);
                labels.push((rest, rx, ty, text_full));
            }
        } else {
            labels.push(((*label).to_string(), tx, ty, text_full));
        }
        row_last_end.insert(*row, x + chip_w);
    }

    HintOverlay { quads, labels }
}

/// The small COPY-MODE status pill (top-left of the grid). Reads "COPY" while
/// the cursor just moves, "COPY · SEL" / "COPY · LINE" / "COPY · BLOCK" while
/// selecting characters / lines / a block. Same rounded/themed idiom as the
/// shift-drag hint pill.
pub struct CopyPill {
    pub quads: Vec<Rect>,
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
}

/// What copy-mode is doing, as its pill shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopySelect {
    /// Moving the cursor, nothing selected.
    None,
    /// `v`: characters in reading order.
    Chars,
    /// `V`: whole lines.
    Lines,
    /// Ctrl+V: a rectangle.
    Block,
}

/// What the copy-mode pill must not cover: the keyboard cursor and the
/// selection, read from the frame's snapshot. Coordinates are the window's.
pub struct PillAvoid<'a> {
    pub snap: &'a jetty_core::GridSnapshot,
    /// Window position of grid cell (0, 0), any dropdown slide included.
    pub origin: crate::GridOrigin,
    pub cell_w: f32,
    pub cell_h: f32,
    /// The copy-mode cursor cell `(row, col)`.
    pub cursor: (usize, usize),
    /// y just past the grid band: the pill's bottom-corner spot sits above it.
    pub band_bottom: f32,
}

impl PillAvoid<'_> {
    /// Whether a pill at `(x, y, w, h)` would hide the cursor or a selected cell.
    fn blocks(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        use unicode_width::UnicodeWidthChar;
        let s = self.snap;
        if s.rows == 0 || s.cols == 0 || !(self.cell_w > 0.0 && self.cell_h > 0.0) {
            return false;
        }
        // The cells the rect touches, clamped to the grid.
        let first = |p: f32, o: f32, cell: f32| ((p - o) / cell).floor();
        let last = |p: f32, o: f32, cell: f32| ((p - o) / cell).ceil() - 1.0;
        let (r0, r1) = (first(y, self.origin.top, self.cell_h), last(y + h, self.origin.top, self.cell_h));
        let (c0, c1) = (first(x, self.origin.left, self.cell_w), last(x + w, self.origin.left, self.cell_w));
        if r1 < 0.0 || c1 < 0.0 || r0 >= s.rows as f32 || c0 >= s.cols as f32 {
            return false;
        }
        let rows = r0.max(0.0) as usize..=(r1 as usize).min(s.rows - 1);
        let cols = c0.max(0.0) as usize..=(c1 as usize).min(s.cols - 1);
        let (cr, cc) = self.cursor;
        // A cursor on a wide char frames its spacer too.
        let cursor_w = if cr < s.rows && cc + 1 < s.cols && s.cell(cr, cc).c.width() == Some(2) { 2 } else { 1 };
        if rows.contains(&cr) && (cc..cc + cursor_w).any(|c| cols.contains(&c)) {
            return true;
        }
        rows.clone().any(|r| cols.clone().any(|c| s.cell(r, c).selected))
    }
}

/// Build the copy-mode pill for a window `win_w` px wide, anchored at
/// `grid_top`. With `avoid`, the pill never hides the copy cursor or the
/// selection: when its top-left spot would, it moves to the bottom-left of the
/// grid (and stays put when that spot is taken too — a grid a few rows tall).
#[allow(clippy::too_many_arguments)]
pub fn build_copy_pill(
    win_w: u32,
    grid_top: f32,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    select: CopySelect,
    avoid: Option<&PillAvoid>,
) -> CopyPill {
    let text = match select {
        CopySelect::None => "COPY",
        CopySelect::Chars => "COPY · SEL",
        CopySelect::Lines => "COPY · LINE",
        CopySelect::Block => "COPY · BLOCK",
    }
    .to_string();
    let vscale = cm.overlay_u();
    let pad = 10.0 * vscale;
    let pill_h = 24.0 * vscale;
    let text_h = 16.0 * vscale;
    let text_w = m.text_w(&text);
    let pill_w = (text_w + pad * 2.0).min((win_w as f32 - 16.0).max(0.0));
    let x = 8.0f32.min((win_w as f32 - pill_w - 8.0).max(0.0));
    let top_y = grid_top + 8.0;
    let y = match avoid {
        Some(a) if a.blocks(x, top_y, pill_w, pill_h) => {
            let bottom_y = a.band_bottom - 8.0 - pill_h;
            if bottom_y > top_y + pill_h && !a.blocks(x, bottom_y, pill_w, pill_h) {
                bottom_y
            } else {
                top_y
            }
        }
        _ => top_y,
    };

    // The theme's cursor color with that fill's own readable text (the theme bg
    // read 2.6:1 on Palenight's purple cursor).
    let ui = UiPalette::cached(theme);
    let pill = Rect::rounded(x, y, pill_w, pill_h, rgba(ui.cursor, 235), pill_h / 2.0);
    let ty = y + (pill_h - text_h) / 2.0;
    CopyPill {
        quads: vec![pill],
        labels: vec![(text, x + pad, ty, ui.on_cursor)],
    }
}

/// Hollow-box cursor rects for the copy-mode keyboard cursor at viewport cell
/// `(row, col)` of `snap`. The four-edge idiom from `cursor_rects`'
/// HollowBlock, colored `color`, so it reads distinctly from both the shell
/// cursor (suppressed while copy-mode is active) and the block selection tint.
/// On a double-width glyph the box spans both of its cells (the shell cursor's
/// rule), so it frames the whole char rather than its left half.
pub fn copy_cursor_rects(
    snap: &jetty_core::GridSnapshot,
    row: usize,
    col: usize,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    color: [u8; 3],
) -> Vec<Rect> {
    use unicode_width::UnicodeWidthChar;
    let wide = row < snap.rows && col + 1 < snap.cols && snap.cell(row, col).c.width() == Some(2);
    let w = if wide { cell_w * 2.0 } else { cell_w };
    let x = col as f32 * cell_w;
    let y = y_offset + row as f32 * cell_h;
    let b = (cell_w * 0.12).max(1.5);
    let col4 = [color[0], color[1], color[2], 255];
    vec![
        Rect::new(x, y, w, b, col4),                // top
        Rect::new(x, y + cell_h - b, w, b, col4),   // bottom
        Rect::new(x, y, b, cell_h, col4),           // left
        Rect::new(x + w - b, y, b, cell_h, col4),   // right
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }
    const TEST_CHAR_W: f32 = 9.8;

    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    #[test]
    fn chips_stay_in_bounds_across_widths() {
        // A chip near the right edge must be clamped clear of the scrollbar gutter.
        for w in [320u32, 500, 1000, 1600] {
            let cell_w = 9.0;
            let last_col = (w as f32 / cell_w) as usize;
            let labeled: Vec<(&str, usize, usize)> =
                vec![("a", 0, 0), ("sd", 1, last_col.saturating_sub(1)), ("qw", 2, last_col + 10)];
            let ov = build_hint_overlay(&labeled, cell_w, 18.0, 36.0, &theme(), &mut mono(), CM, "", w);
            for q in &ov.quads {
                assert!(q.x >= 0.0, "chip off-screen left at width {w}");
                assert!(
                    q.x + q.w <= w as f32 - SCROLLBAR_W + 0.5,
                    "chip overlaps scrollbar gutter at width {w}: {} > {}",
                    q.x + q.w,
                    w as f32 - SCROLLBAR_W
                );
            }
        }
    }

    #[test]
    fn chips_clear_the_dpi_scaled_scrollbar_column_at_2x() {
        // At 2× the thumb column is 28 px wide: a chip at the last column must
        // clear THAT, not the 1× 14 px.
        let cm2 = ChromeMetrics::new(2.0, 16.0);
        let (w, cell_w) = (2000u32, 19.0);
        let last_col = (w as f32 / cell_w) as usize;
        let labeled = vec![("sd", 0, last_col.saturating_sub(1)), ("qw", 1, last_col + 10)];
        let ov = build_hint_overlay(&labeled, cell_w, 36.0, 72.0, &theme(), &mut MonoMeasure(19.6), cm2, "", w);
        for q in &ov.quads {
            assert!(q.x + q.w <= w as f32 - 2.0 * SCROLLBAR_W + 0.5, "chip right {} at 2×", q.x + q.w);
        }
    }

    #[test]
    fn chip_color_differs_from_bg() {
        let labeled = vec![("a", 0, 0)];
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), &mut mono(), CM, "", 1000);
        assert_eq!(ov.quads.len(), 1);
        let bg = theme().bg;
        let c = ov.quads[0].color;
        assert_ne!([c[0], c[1], c[2]], [bg[0], bg[1], bg[2]], "chip must be visible against the bg");
    }

    #[test]
    fn overlapping_chips_on_a_row_are_skipped() {
        // Two tokens one cell apart on the same row: the second chip would overlap,
        // so it is dropped (bounds visual clutter).
        let labeled = vec![("as", 0, 0), ("df", 0, 1)];
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), &mut mono(), CM, "", 1000);
        assert_eq!(ov.quads.len(), 1, "the overlapping second chip is skipped");
    }

    #[test]
    fn typed_prefix_renders_as_two_segments() {
        let labeled = vec![("sd", 0, 5)];
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), &mut mono(), CM, "s", 1000);
        // "s" (typed) + "d" (remainder) → two label segments.
        assert_eq!(ov.labels.len(), 2);
        assert_eq!(ov.labels[0].0, "s");
        assert_eq!(ov.labels[1].0, "d");
    }

    #[test]
    fn pill_fits_and_scales() {
        let p1 = build_copy_pill(1000, 36.0, &theme(), &mut mono(), CM, CopySelect::None, None);
        let p2 = build_copy_pill(1000, 36.0, &theme(), &mut MonoMeasure(19.6), ChromeMetrics::new(2.0, 16.0), CopySelect::Lines, None);
        assert_eq!(p1.labels[0].0, "COPY");
        assert_eq!(p2.labels[0].0, "COPY · LINE");
        // Pill scales with the chrome unit (2× → ~2× height).
        assert!((p2.quads[0].h - p1.quads[0].h * 2.0).abs() < 0.5, "pill must scale with the chrome unit");
        // Pill fits a narrow window.
        let pn = build_copy_pill(200, 10.0, &theme(), &mut mono(), CM, CopySelect::None, None);
        assert!(pn.quads[0].x + pn.quads[0].w <= 200.0 + 0.5, "pill overflows narrow window");
    }

    #[test]
    fn chips_and_pill_read_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let ov = build_hint_overlay(&[("sd", 0, 5)], 9.0, 18.0, 0.0, &t, &mut mono(), CM, "s", 1000);
            let chip = rgb(ov.quads[0].color);
            let (typed, rest) = (ov.labels[0].3, ov.labels[1].3);
            assert!(cr(rest, chip) >= 4.5, "{}: chip label {}", t.name, cr(rest, chip));
            assert!(cr(typed, chip) >= 3.0, "{}: typed prefix {}", t.name, cr(typed, chip));
            assert_ne!(typed, rest, "{}: the typed prefix must look consumed", t.name);
            let p = build_copy_pill(1000, 36.0, &t, &mut mono(), CM, CopySelect::None, None);
            let c = cr(p.labels[0].3, rgb(p.quads[0].color));
            assert!(c >= 4.5, "{}: COPY pill {c}", t.name);
        }
    }

    fn snap_with(text: &str, cols: usize, rows: usize) -> jetty_core::GridSnapshot {
        let mut t = jetty_core::Terminal::new(cols, rows);
        t.feed(text.as_bytes());
        t.snapshot()
    }

    #[test]
    fn copy_cursor_is_a_hollow_box() {
        let snap = snap_with("", 10, 4);
        let r = copy_cursor_rects(&snap, 2, 3, 9.0, 18.0, 36.0, [255, 255, 255]);
        assert_eq!(r.len(), 4, "hollow box = 4 edges");
        // Anchored at cell (row 2, col 3) with the y offset.
        assert_eq!(r[0].x, 27.0);
        assert_eq!(r[0].y, 36.0 + 2.0 * 18.0);
        assert_eq!(r[0].w, 9.0, "one cell wide");
    }

    /// Where the pill sits ON TOP: `grid_top` 36 + its 8 px margin.
    const PILL_TOP: f32 = 36.0 + 8.0;
    /// Where it sits at the BOTTOM: 8 px above the band bottom (400).
    fn pill_bottom() -> f32 {
        400.0 - 8.0 - 24.0 * CM.overlay_u()
    }

    /// The pill's y for a copy cursor at `cursor` on an 80×20 grid (cells
    /// 9×18 px, origin (8, 40), band bottom 400), after `select` ran.
    fn pill_y(cursor: (usize, usize), select: impl Fn(&mut jetty_core::Terminal)) -> f32 {
        let mut t = jetty_core::Terminal::new(80, 20);
        t.feed(b"first line of output\r\nsecond line\r\nthird line");
        select(&mut t);
        let snap = t.snapshot();
        let avoid = PillAvoid {
            snap: &snap,
            origin: crate::GridOrigin::new(8.0, 40.0),
            cell_w: 9.0,
            cell_h: 18.0,
            cursor,
            band_bottom: 400.0,
        };
        build_copy_pill(1000, 36.0, &theme(), &mut mono(), CM, CopySelect::None, Some(&avoid)).quads[0].y
    }

    #[test]
    fn pill_steps_aside_for_the_copy_cursor() {
        // The pill covers rows 0–1 at the left: a cursor there (copy-mode `g`,
        // `k` scrolling history) hid under it. It moves to the bottom-left.
        assert_eq!(pill_y((0, 0), |_| {}), pill_bottom(), "cursor at (0, 0)");
        assert_eq!(pill_y((1, 2), |_| {}), pill_bottom(), "cursor on row 1, under the pill");
        // Clear of the pill: it stays at the top-left.
        assert_eq!(pill_y((0, 40), |_| {}), PILL_TOP, "cursor right of the pill");
        assert_eq!(pill_y((10, 0), |_| {}), PILL_TOP, "cursor below the pill");
    }

    #[test]
    fn pill_steps_aside_for_the_selection() {
        // A selection under the top-left spot (the anchor of a `g` + `v`) moves
        // the pill even with the cursor far below.
        let sel = |t: &mut jetty_core::Terminal| {
            t.selection_start(0, 0, true);
            t.selection_update(2, 5, false);
        };
        assert_eq!(pill_y((10, 0), sel), pill_bottom());
        // A selection elsewhere leaves it alone.
        let elsewhere = |t: &mut jetty_core::Terminal| {
            t.selection_start(2, 0, true);
            t.selection_update(2, 5, false);
        };
        assert_eq!(pill_y((10, 0), elsewhere), PILL_TOP);
    }

    #[test]
    fn pill_keeps_its_spot_when_both_corners_are_taken() {
        // A 2-row grid: the bottom spot overlaps the top one — stay on top.
        let mut t = jetty_core::Terminal::new(80, 2);
        t.feed(b"x");
        let snap = t.snapshot();
        let avoid = PillAvoid {
            snap: &snap,
            origin: crate::GridOrigin::new(8.0, 40.0),
            cell_w: 9.0,
            cell_h: 18.0,
            cursor: (0, 0),
            band_bottom: 40.0 + 36.0,
        };
        let p = build_copy_pill(1000, 36.0, &theme(), &mut mono(), CM, CopySelect::None, Some(&avoid));
        assert_eq!(p.quads[0].y, PILL_TOP);
    }

    #[test]
    fn copy_cursor_frames_the_whole_wide_char() {
        // "a世b": 世 spans cols 1–2. The box on it covers both cells — it
        // framed only the left half of the glyph.
        let snap = snap_with("a世b", 10, 2);
        let r = copy_cursor_rects(&snap, 0, 1, 9.0, 18.0, 0.0, [255, 255, 255]);
        assert_eq!((r[0].x, r[0].w), (9.0, 18.0), "top edge spans two cells");
        assert_eq!(r[3].x + r[3].w, 27.0, "right edge at the end of the spacer");
        // A narrow neighbour stays one cell.
        let r = copy_cursor_rects(&snap, 0, 3, 9.0, 18.0, 0.0, [255, 255, 255]);
        assert_eq!(r[0].w, 9.0);
    }
}
