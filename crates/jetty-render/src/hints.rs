//! Overlay draw data for HINT MODE (Ctrl+Shift+H) and keyboard COPY-MODE
//! (Ctrl+Shift+Space).
//!
//! Same surface language as `search_bar.rs` / `help.rs`: all colors derive from
//! the active theme (bg→accent lerp, no hardcoded RGB) and metrics scale with the
//! measured chrome-font advance `char_w` (the `char_w/9.8` vscale idiom), so both
//! overlays are HiDPI-correct. Nothing here self-drives frames — the app draws
//! these only while a mode is active, once per event-driven redraw.

use crate::quad::SCROLLBAR_W;
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
#[allow(clippy::too_many_arguments)]
pub fn build_hint_overlay(
    labeled: &[(&str, usize, usize)],
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    theme: &jetty_core::Theme,
    char_w: f32,
    typed: &str,
    win_w: u32,
) -> HintOverlay {
    let bg = theme.bg;
    let accent = theme.palette[3];
    // bg→accent blend for the chip fill (bright, opaque — like a search hit).
    let chip = |t: f32| -> [u8; 4] {
        [
            (bg[0] as f32 + (accent[0] as f32 - bg[0] as f32) * t).round() as u8,
            (bg[1] as f32 + (accent[1] as f32 - bg[1] as f32) * t).round() as u8,
            (bg[2] as f32 + (accent[2] as f32 - bg[2] as f32) * t).round() as u8,
            255,
        ]
    };
    let chip_bg = chip(0.9);
    // Dark text (theme bg, RGB only) reads clearly on the bright chip; the
    // consumed prefix is a mid blend so narrowing is visible.
    let text_full = [bg[0], bg[1], bg[2]];
    let text_typed = [
        (bg[0] as f32 + (accent[0] as f32 - bg[0] as f32) * 0.4).round() as u8,
        (bg[1] as f32 + (accent[1] as f32 - bg[1] as f32) * 0.4).round() as u8,
        (bg[2] as f32 + (accent[2] as f32 - bg[2] as f32) * 0.4).round() as u8,
    ];

    let vscale = (char_w / 9.8).max(0.1);
    let pad_x = (3.0 * vscale).max(2.0);
    let text_h = 16.0 * vscale;
    let radius = (cell_h * 0.25).min(6.0);
    let max_x = (win_w as f32 - SCROLLBAR_W).max(0.0);

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Per-row right edge of the last placed chip → skip an overlapping chip.
    let mut row_last_end: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();

    for (label, row, col) in labeled {
        let n = label.chars().count() as f32;
        let text_w = n * char_w;
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
                let rx = tx + typed.chars().count() as f32 * char_w;
                labels.push((rest, rx, ty, text_full));
            }
        } else {
            labels.push(((*label).to_string(), tx, ty, text_full));
        }
        row_last_end.insert(*row, x + chip_w);
    }

    HintOverlay { quads, labels }
}

/// The small COPY-MODE status pill (top-left of the grid). Reads "COPY" (char
/// select) or "COPY · LINE" (line select). Same rounded/themed idiom as the
/// shift-drag hint pill.
pub struct CopyPill {
    pub quads: Vec<Rect>,
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
}

/// Build the copy-mode pill for a window `win_w` px wide, anchored at `grid_top`.
pub fn build_copy_pill(
    win_w: u32,
    grid_top: f32,
    theme: &jetty_core::Theme,
    char_w: f32,
    line_mode: bool,
    selecting: bool,
) -> CopyPill {
    let text = if line_mode {
        "COPY · LINE".to_string()
    } else if selecting {
        "COPY · SEL".to_string()
    } else {
        "COPY".to_string()
    };
    let vscale = (char_w / 9.8).max(0.1);
    let pad = 10.0 * vscale;
    let pill_h = 24.0 * vscale;
    let text_h = 16.0 * vscale;
    let text_w = text.chars().count() as f32 * char_w;
    let pill_w = (text_w + pad * 2.0).min((win_w as f32 - 16.0).max(0.0));
    let x = 8.0f32.min((win_w as f32 - pill_w - 8.0).max(0.0));
    let y = grid_top + 8.0;

    let c = theme.cursor;
    let pill = Rect::rounded(x, y, pill_w, pill_h, [c[0], c[1], c[2], 235], pill_h / 2.0);
    let ty = y + (pill_h - text_h) / 2.0;
    let tb = theme.bg;
    CopyPill {
        quads: vec![pill],
        labels: vec![(text, x + pad, ty, [tb[0], tb[1], tb[2]])],
    }
}

/// Hollow-box cursor rects for the copy-mode keyboard cursor at viewport cell
/// `(row, col)`. The four-edge idiom from `cursor_rects`' HollowBlock, colored
/// `color`, so it reads distinctly from both the shell cursor (suppressed while
/// copy-mode is active) and the block selection tint.
pub fn copy_cursor_rects(
    row: usize,
    col: usize,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    color: [u8; 3],
) -> Vec<Rect> {
    let x = col as f32 * cell_w;
    let y = y_offset + row as f32 * cell_h;
    let b = (cell_w * 0.12).max(1.5);
    let col4 = [color[0], color[1], color[2], 255];
    vec![
        Rect::new(x, y, cell_w, b, col4),                // top
        Rect::new(x, y + cell_h - b, cell_w, b, col4),   // bottom
        Rect::new(x, y, b, cell_h, col4),                // left
        Rect::new(x + cell_w - b, y, b, cell_h, col4),   // right
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }
    const TEST_CHAR_W: f32 = 9.8;

    #[test]
    fn chips_stay_in_bounds_across_widths() {
        // A chip near the right edge must be clamped clear of the scrollbar gutter.
        for w in [320u32, 500, 1000, 1600] {
            let cell_w = 9.0;
            let last_col = (w as f32 / cell_w) as usize;
            let labeled: Vec<(&str, usize, usize)> =
                vec![("a", 0, 0), ("sd", 1, last_col.saturating_sub(1)), ("qw", 2, last_col + 10)];
            let ov = build_hint_overlay(&labeled, cell_w, 18.0, 36.0, &theme(), TEST_CHAR_W, "", w);
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
    fn chip_color_differs_from_bg() {
        let labeled = vec![("a", 0, 0)];
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), TEST_CHAR_W, "", 1000);
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
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), TEST_CHAR_W, "", 1000);
        assert_eq!(ov.quads.len(), 1, "the overlapping second chip is skipped");
    }

    #[test]
    fn typed_prefix_renders_as_two_segments() {
        let labeled = vec![("sd", 0, 5)];
        let ov = build_hint_overlay(&labeled, 9.0, 18.0, 0.0, &theme(), TEST_CHAR_W, "s", 1000);
        // "s" (typed) + "d" (remainder) → two label segments.
        assert_eq!(ov.labels.len(), 2);
        assert_eq!(ov.labels[0].0, "s");
        assert_eq!(ov.labels[1].0, "d");
    }

    #[test]
    fn pill_fits_and_scales() {
        let p1 = build_copy_pill(1000, 36.0, &theme(), 9.8, false, false);
        let p2 = build_copy_pill(1000, 36.0, &theme(), 19.6, true, true);
        assert_eq!(p1.labels[0].0, "COPY");
        assert_eq!(p2.labels[0].0, "COPY · LINE");
        // Pill scales with char_w (2× advance → ~2× height).
        assert!((p2.quads[0].h - p1.quads[0].h * 2.0).abs() < 0.5, "pill must scale with char_w");
        // Pill fits a narrow window.
        let pn = build_copy_pill(200, 10.0, &theme(), 9.8, false, false);
        assert!(pn.quads[0].x + pn.quads[0].w <= 200.0 + 0.5, "pill overflows narrow window");
    }

    #[test]
    fn copy_cursor_is_a_hollow_box() {
        let r = copy_cursor_rects(2, 3, 9.0, 18.0, 36.0, [255, 255, 255]);
        assert_eq!(r.len(), 4, "hollow box = 4 edges");
        // Anchored at cell (row 2, col 3) with the y offset.
        assert_eq!(r[0].x, 27.0);
        assert_eq!(r[0].y, 36.0 + 2.0 * 18.0);
    }
}
