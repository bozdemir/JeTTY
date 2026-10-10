//! IME preedit — the in-progress composition of an input method (CJK, Korean,
//! compose sequences) — drawn at the terminal cursor until it commits.

use crate::quad::{Rect, UnderlineGeom};
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

/// The grid characters of composition `text` in a grid `cols` cells wide: one
/// `(cluster, cell offset)` per character (combining marks ride with their
/// base), and the cells they take — wide (CJK) characters count two; text wider
/// than the grid is cut at a character boundary, control characters dropped.
fn preedit_cells(text: &str, cols: usize) -> (Vec<(String, usize)>, usize) {
    let mut cells: Vec<(String, usize)> = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().take(MAX_PREEDIT_CHARS) {
        // Control characters have no width and no business in a composition.
        let Some(w) = ch.width() else { continue };
        if w == 0 {
            if let Some((cluster, _)) = cells.last_mut() {
                cluster.push(ch);
            }
            continue;
        }
        if width + w > cols {
            break;
        }
        cells.push((ch.to_string(), width));
        width += w;
    }
    (cells, width)
}

/// The grid column composition `text` starts at with the cursor in column
/// `col` of a grid `cols` cells wide: the cursor's, or further left when the
/// composition would run past the right edge (see [`build_preedit_overlay`]).
/// The IME's candidate window is anchored there.
pub fn preedit_start_col(text: &str, col: usize, cols: usize) -> usize {
    let (_, width) = preedit_cells(text, cols);
    if width == 0 {
        col
    } else {
        col.min(cols - width)
    }
}

/// Build the preedit overlay for `text` at grid cell `(row, col)` of a grid
/// `cols` cells wide whose origin is `(0, grid_top)`, in physical pixels. The
/// composition reads like typed text — theme fg on the theme bg — underlined the
/// way terminals mark uncommitted input, with the stroke the grid font's
/// underlines use (`ul`, `TextLayer::underline_geom`). It starts at the cursor
/// and shifts left when it would run past the right edge (`preedit_start_col`);
/// text wider than the whole grid is cut at a character boundary. Display width
/// counts wide (CJK) characters as two cells. `None` when there is nothing to
/// draw.
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
    ul: UnderlineGeom,
) -> Option<PreeditOverlay> {
    if cols == 0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return None;
    }
    // One label per grid character at its own cell column, so the composition
    // sits on the grid exactly like committed text — a run shaped as a whole
    // would put CJK at the fallback font's natural advance, off the 2-cell
    // rhythm.
    let (cells, width) = preedit_cells(text, cols);
    if width == 0 {
        return None;
    }
    let start = col.min(cols - width);
    let x = start as f32 * cell_w;
    let y = grid_top + row as f32 * cell_h;
    let w = width as f32 * cell_w;
    let [fr, fg, fb] = theme.fg;
    let [br, bg, bb, _] = theme.bg;
    Some(PreeditOverlay {
        quads: vec![
            Rect::new(x, y, w, cell_h, [br, bg, bb, 255]),
            Rect::new(x, y + ul.top, w, ul.thickness, [fr, fg, fb, 255]),
        ],
        labels: cells
            .into_iter()
            .map(|(cluster, off)| (cluster, x + off as f32 * cell_w, y, theme.fg))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// A 20 px row's underline geometry: the stroke at 17 px, 1 px thick.
    fn ul() -> UnderlineGeom {
        UnderlineGeom { top: 17.0, bottom: 20.0, thickness: 1.0 }
    }

    #[test]
    fn preedit_sits_on_the_cursor_cell_underlined() {
        let ov = build_preedit_overlay("nihao", 2, 3, 80, 10.0, 20.0, 36.0, &theme(), ul()).unwrap();
        let placed: Vec<(&str, f32, f32)> = ov.labels.iter().map(|(t, x, y, _)| (t.as_str(), *x, *y)).collect();
        assert_eq!(
            placed,
            [("n", 30.0, 76.0), ("i", 40.0, 76.0), ("h", 50.0, 76.0), ("a", 60.0, 76.0), ("o", 70.0, 76.0)],
            "one label per cell, on the grid"
        );
        // Backdrop covers exactly the composition's cells; the underline is the
        // grid font's (its position under the text, its thickness).
        assert_eq!((ov.quads[0].x, ov.quads[0].y, ov.quads[0].w, ov.quads[0].h), (30.0, 76.0, 50.0, 20.0));
        assert_eq!((ov.quads[1].y, ov.quads[1].h), (76.0 + 17.0, 1.0));
    }

    #[test]
    fn wide_characters_take_two_cells_and_shift_left_at_the_edge() {
        // 你好 = 4 cells; the cursor in column 78 of 80 shifts it to start at 76,
        // each glyph on its own 2-cell slot.
        let tall = UnderlineGeom { top: 26.0, bottom: 30.0, thickness: 2.0 };
        let ov = build_preedit_overlay("你好", 0, 78, 80, 10.0, 40.0, 0.0, &theme(), tall).unwrap();
        let xs: Vec<f32> = ov.labels.iter().map(|l| l.1).collect();
        assert_eq!(xs, [760.0, 780.0]);
        assert_eq!(ov.quads[0].w, 40.0);
        // A taller line (line_height 2.0): the underline stays with the text,
        // far above the cell bottom, as thick as the font's underlines.
        assert_eq!((ov.quads[1].y, ov.quads[1].h), (26.0, 2.0));
        // The IME's candidate window anchors where the composition starts.
        assert_eq!(preedit_start_col("你好", 78, 80), 76);
        assert_eq!(preedit_start_col("你好", 10, 80), 10);
        assert_eq!(preedit_start_col("", 79, 80), 79);
        assert_eq!(preedit_start_col("abcdef", 3, 4), 0, "wider than the grid: cut, from column 0");
    }

    #[test]
    fn text_wider_than_the_grid_is_cut_and_controls_are_dropped() {
        let text_of = |ov: &PreeditOverlay| ov.labels.iter().map(|l| l.0.as_str()).collect::<String>();
        let ov = build_preedit_overlay("a\u{1b}bcdef", 0, 0, 4, 10.0, 20.0, 0.0, &theme(), ul()).unwrap();
        assert_eq!(text_of(&ov), "abcd");
        // A wide char that would straddle the edge stops the cut before it.
        let ov = build_preedit_overlay("ab你", 0, 0, 3, 10.0, 20.0, 0.0, &theme(), ul()).unwrap();
        assert_eq!(text_of(&ov), "ab");
        // A combining mark rides with its base character's label.
        let ov = build_preedit_overlay("e\u{301}x", 0, 0, 80, 10.0, 20.0, 0.0, &theme(), ul()).unwrap();
        let labels: Vec<&str> = ov.labels.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(labels, ["e\u{301}", "x"]);
    }

    #[test]
    fn nothing_to_draw_is_none() {
        assert!(build_preedit_overlay("", 0, 0, 80, 10.0, 20.0, 0.0, &theme(), ul()).is_none());
        assert!(build_preedit_overlay("\u{7}", 0, 0, 80, 10.0, 20.0, 0.0, &theme(), ul()).is_none());
        assert!(build_preedit_overlay("x", 0, 0, 0, 10.0, 20.0, 0.0, &theme(), ul()).is_none());
    }
}
