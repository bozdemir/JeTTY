//! IME preedit — the in-progress composition of an input method (CJK, Korean,
//! compose sequences) — drawn at the terminal cursor until it commits.

use crate::quad::Rect;
use unicode_width::UnicodeWidthChar;

/// Quads (an opaque backdrop over the covered cells + an underline) and the
/// text label of an IME preedit. The label renders in the TERMINAL font
/// (through the grid text layer), so it sits exactly on the cell grid.
pub struct PreeditOverlay {
    pub quads: Vec<Rect>,
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
}

/// Most preedit characters considered (an IME composes a word or a phrase; the
/// grid width clips long ones anyway).
pub const MAX_PREEDIT_CHARS: usize = 256;

/// Build the preedit overlay for `text` at grid cell `(row, col)` of a grid
/// `cols` cells wide whose origin is `(0, grid_top)`, in physical pixels. The
/// composition reads like typed text — theme fg on the theme bg — underlined
/// (`scale`-thick) the way terminals mark uncommitted input. It starts at the
/// cursor and shifts left when it would run past the right edge; text wider
/// than the whole grid is cut at a character boundary. Display width counts
/// wide (CJK) characters as two cells. `None` when there is nothing to draw.
#[allow(clippy::too_many_arguments)]
pub fn build_preedit_overlay(
    text: &str,
    row: usize,
    col: usize,
    cols: usize,
    cell_w: f32,
    cell_h: f32,
    grid_top: f32,
    theme: &jetty_core::Theme,
    scale: f32,
) -> Option<PreeditOverlay> {
    if cols == 0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return None;
    }
    let mut shown = String::new();
    let mut width = 0usize;
    for ch in text.chars().take(MAX_PREEDIT_CHARS) {
        // Control characters have no width and no business in a composition.
        let Some(w) = ch.width() else { continue };
        if width + w > cols {
            break;
        }
        width += w;
        shown.push(ch);
    }
    if width == 0 {
        return None;
    }
    let start = col.min(cols - width);
    let x = start as f32 * cell_w;
    let y = grid_top + row as f32 * cell_h;
    let w = width as f32 * cell_w;
    let [fr, fg, fb] = theme.fg;
    let [br, bg, bb, _] = theme.bg;
    let line = scale.round().max(1.0);
    Some(PreeditOverlay {
        quads: vec![
            Rect::new(x, y, w, cell_h, [br, bg, bb, 255]),
            Rect::new(x, y + cell_h - line, w, line, [fr, fg, fb, 255]),
        ],
        labels: vec![(shown, x, y, theme.fg)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    #[test]
    fn preedit_sits_on_the_cursor_cell_underlined() {
        let ov = build_preedit_overlay("nihao", 2, 3, 80, 10.0, 20.0, 36.0, &theme(), 1.0).unwrap();
        assert_eq!(ov.labels.len(), 1);
        let (t, x, y, _) = &ov.labels[0];
        assert_eq!((t.as_str(), *x, *y), ("nihao", 30.0, 76.0));
        // Backdrop covers exactly the composition's cells; underline on its bottom edge.
        assert_eq!((ov.quads[0].x, ov.quads[0].y, ov.quads[0].w, ov.quads[0].h), (30.0, 76.0, 50.0, 20.0));
        assert_eq!((ov.quads[1].y, ov.quads[1].h), (95.0, 1.0));
    }

    #[test]
    fn wide_characters_take_two_cells_and_shift_left_at_the_edge() {
        // 你好 = 4 cells; the cursor in column 78 of 80 shifts it to start at 76.
        let ov = build_preedit_overlay("你好", 0, 78, 80, 10.0, 20.0, 0.0, &theme(), 2.0).unwrap();
        assert_eq!(ov.labels[0].1, 760.0);
        assert_eq!(ov.quads[0].w, 40.0);
        assert_eq!(ov.quads[1].h, 2.0, "the underline follows the DPI scale");
    }

    #[test]
    fn text_wider_than_the_grid_is_cut_and_controls_are_dropped() {
        let ov = build_preedit_overlay("a\u{1b}bcdef", 0, 0, 4, 10.0, 20.0, 0.0, &theme(), 1.0).unwrap();
        assert_eq!(ov.labels[0].0, "abcd");
        // A wide char that would straddle the edge stops the cut before it.
        let ov = build_preedit_overlay("ab你", 0, 0, 3, 10.0, 20.0, 0.0, &theme(), 1.0).unwrap();
        assert_eq!(ov.labels[0].0, "ab");
    }

    #[test]
    fn nothing_to_draw_is_none() {
        assert!(build_preedit_overlay("", 0, 0, 80, 10.0, 20.0, 0.0, &theme(), 1.0).is_none());
        assert!(build_preedit_overlay("\u{7}", 0, 0, 80, 10.0, 20.0, 0.0, &theme(), 1.0).is_none());
        assert!(build_preedit_overlay("x", 0, 0, 0, 10.0, 20.0, 0.0, &theme(), 1.0).is_none());
    }
}
