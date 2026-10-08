//! Where a window's terminal grid sits — the ONE definition of the inner
//! padding, the grid's size and the cell ↔ pixel convention, shared by the main
//! window, detached windows, jetty-shot and jetty-bench so drawing and
//! hit-testing can never disagree.
//!
//! # The convention (physical px)
//!
//! Viewport cell `(row, col)` covers
//!
//! ```text
//! x ∈ [left + col·cell_w, left + (col+1)·cell_w)
//! y ∈ [top  + row·cell_h, top  + (row+1)·cell_h)
//! ```
//!
//! where [`GridOrigin`] `{ left, top }` is the grid's padded origin: `left` is
//! the left padding and `top` the grid BAND's top (the bottom edge of a top tab
//! bar / title bar, else 0) plus the top padding. The band itself — what the
//! scrollbar track and chrome overlays anchor to — stays the un-padded area
//! between the bars.
//!
//! The per-cell quad builders (`cell_bg_rects`, `text_decoration_rects`,
//! `link_underline_rects`, `search_hit_rects`, `copy_cursor_rects`,
//! `cursor_guide_rect`, the hint-chip and IME-preedit overlays) take the
//! origin's `top` (plus any slide) as their `y_offset` and lay x out from the
//! grid's LEFT EDGE (`col·cell_w`). The caller moves what they return onto the
//! origin with [`shift_x`] / [`shift_labels_x`] right where it builds them, so
//! every list that reaches a draw call is in window coordinates. The shell
//! cursor (`cursor_draw`) and the cursor trail (`cursor_trail_rect`) take the
//! origin's `left` as an explicit `x_offset` instead and return window
//! coordinates directly.
//! `TextLayer::prepare_grid` takes the whole origin, and its
//! `decoration_rects()` come back already placed.
//!
//! Pixel → cell goes the other way: subtract the origin, then floor by the cell
//! size and clamp to the grid (jetty-app's `GridGeom` / `input::cell_at_*`), so
//! a pointer in the padding maps to the nearest edge cell.

use crate::Rect;

/// Largest inner padding (logical px) a config may ask for, per side.
pub const PADDING_MAX: f32 = 64.0;

/// The physical-px position of viewport cell (0, 0): see the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridOrigin {
    /// x of column 0's left edge (the left padding).
    pub left: f32,
    /// y of row 0's top edge (band top + top padding).
    pub top: f32,
}

impl GridOrigin {
    pub const fn new(left: f32, top: f32) -> Self {
        Self { left, top }
    }

    /// x of column `col`'s left edge.
    #[inline]
    pub fn col_x(self, col: usize, cell_w: f32) -> f32 {
        self.left + col as f32 * cell_w
    }

    /// y of row `row`'s top edge.
    #[inline]
    pub fn row_y(self, row: usize, cell_h: f32) -> f32 {
        self.top + row as f32 * cell_h
    }

    /// This origin moved down by `dy` (the main window's dropdown slide).
    #[inline]
    pub fn slid(self, dy: f32) -> Self {
        Self { left: self.left, top: self.top + dy }
    }
}

/// Physical inner padding for `logical` px at DPI `scale`: scaled, then
/// rounded to whole pixels so every column/row edge keeps exactly the subpixel
/// phase it has with no padding (and the grid never blurs between frames).
/// A non-finite or negative value counts as 0; the result never exceeds
/// [`PADDING_MAX`] logical px.
pub fn padding_px(logical: f32, scale: f32) -> f32 {
    let l = if logical.is_finite() { logical.clamp(0.0, PADDING_MAX) } else { 0.0 };
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (l * s).round()
}

/// Columns × rows that fit a window's grid band — `band_w` × `band_h` physical
/// px: the window minus its top/bottom bars and status strip — inside `pad_x`
/// / `pad_y` of inner padding on every side, with the scrollbar `gutter` at the
/// right. The gutter doubles as right padding (the scrollbar lives in it), so
/// the right inset is `max(pad_x, gutter)`; the bottom inset is `pad_y` like the
/// top. At least 2 × 1; the 80 × 24 fallback before cell metrics exist.
pub fn grid_dims(
    band_w: f32,
    band_h: f32,
    cell_w: f32,
    cell_h: f32,
    gutter: f32,
    pad_x: f32,
    pad_y: f32,
) -> (usize, usize) {
    if !(cell_w > 0.0 && cell_h > 0.0) {
        return (80, 24);
    }
    let usable_w = band_w - pad_x - pad_x.max(gutter);
    let usable_h = band_h - 2.0 * pad_y;
    let cols = (usable_w / cell_w).floor().max(2.0) as usize;
    let rows = (usable_h / cell_h).floor().max(1.0) as usize;
    (cols, rows)
}

/// Move grid-space quads (x measured from the grid's left edge, see the module
/// docs) onto the grid origin's `left`. Free at zero padding.
#[inline]
pub fn shift_x(rects: &mut [Rect], dx: f32) {
    if dx != 0.0 {
        for r in rects {
            r.x += dx;
        }
    }
}

/// [`shift_x`] for overlay labels `(text, x, y, rgb)` (hint chips, preedit).
#[inline]
pub fn shift_labels_x(labels: &mut [(String, f32, f32, [u8; 3])], dx: f32) {
    if dx != 0.0 {
        for l in labels {
            l.1 += dx;
        }
    }
}

/// x of the OSC 133 failed-command bar (`width_px` wide): centred in the left
/// padding when it fits there, else flush with the window's left edge (x = 0 —
/// the look at zero padding, where the bar overlaps column 0).
pub fn failed_marker_x(grid_left: f32, width_px: f32) -> f32 {
    ((grid_left - width_px) * 0.5).floor().max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_scales_with_dpi_and_lands_on_whole_pixels() {
        assert_eq!(padding_px(8.0, 1.0), 8.0);
        assert_eq!(padding_px(8.0, 2.0), 16.0);
        assert_eq!(padding_px(4.0, 1.25), 5.0);
        assert_eq!(padding_px(5.0, 1.25), 6.0, "6.25 rounds to a whole pixel");
        assert_eq!(padding_px(3.0, 1.5), 5.0, "4.5 rounds half away from zero");
        assert_eq!(padding_px(0.0, 2.0), 0.0, "0 = the unpadded look");
    }

    #[test]
    fn padding_rejects_garbage() {
        assert_eq!(padding_px(-3.0, 2.0), 0.0);
        assert_eq!(padding_px(f32::NAN, 2.0), 0.0);
        assert_eq!(padding_px(f32::INFINITY, 1.0), 0.0);
        assert_eq!(padding_px(500.0, 1.0), PADDING_MAX);
        assert_eq!(padding_px(8.0, f32::NAN), 8.0, "a bogus scale falls back to 1×");
        assert_eq!(padding_px(8.0, 0.0), 8.0);
    }

    #[test]
    fn dims_without_padding_match_the_old_formula() {
        // The pre-padding main/detached formula: (w - gutter)/cw, (h - bars)/ch.
        let (w, h, cw, ch, gutter) = (1000.0f32, 600.0f32, 9.6f32, 21.0f32, 18.0f32);
        let old = (((w - gutter) / cw).floor() as usize, (h / ch).floor() as usize);
        assert_eq!(grid_dims(w, h, cw, ch, gutter, 0.0, 0.0), old);
    }

    #[test]
    fn dims_with_padding_at_1x_and_2x() {
        // 1×: 1000 wide, pad 8 left, right inset max(8, 18) = 18 → 974 / 10 = 97.
        // 600 tall, pad 4 top + 4 bottom → 592 / 20 = 29.
        assert_eq!(grid_dims(1000.0, 600.0, 10.0, 20.0, 18.0, 8.0, 4.0), (97, 29));
        // 2× (everything doubled): the same grid.
        assert_eq!(grid_dims(2000.0, 1200.0, 20.0, 40.0, 36.0, 16.0, 8.0), (97, 29));
        // A padding wider than the gutter becomes the right inset.
        assert_eq!(grid_dims(1000.0, 600.0, 10.0, 20.0, 18.0, 30.0, 0.0), (94, 30));
        // No gutter (scrollbar off): symmetric padding.
        assert_eq!(grid_dims(1000.0, 600.0, 10.0, 20.0, 0.0, 8.0, 4.0), (98, 29));
    }

    #[test]
    fn dims_never_collapse_and_fall_back_without_metrics() {
        assert_eq!(grid_dims(10.0, 10.0, 10.0, 20.0, 18.0, 64.0, 64.0), (2, 1));
        assert_eq!(grid_dims(1000.0, 600.0, 0.0, 20.0, 18.0, 8.0, 4.0), (80, 24));
        assert_eq!(grid_dims(1000.0, 600.0, 10.0, f32::NAN, 18.0, 8.0, 4.0), (80, 24));
    }

    #[test]
    fn origin_maps_cells_to_pixels() {
        let o = GridOrigin::new(8.0, 40.0);
        assert_eq!(o.col_x(0, 9.5), 8.0);
        assert_eq!(o.col_x(10, 9.5), 103.0);
        assert_eq!(o.row_y(0, 20.0), 40.0);
        assert_eq!(o.row_y(3, 20.0), 100.0);
        assert_eq!(o.slid(-15.0), GridOrigin::new(8.0, 25.0));
    }

    #[test]
    fn shifts_move_x_only() {
        let mut rects = vec![Rect::new(0.0, 5.0, 10.0, 20.0, [1, 2, 3, 255])];
        shift_x(&mut rects, 8.0);
        assert_eq!((rects[0].x, rects[0].y, rects[0].w), (8.0, 5.0, 10.0));
        let mut labels = vec![("a".to_string(), 0.0, 5.0, [1, 2, 3])];
        shift_labels_x(&mut labels, 16.0);
        assert_eq!((labels[0].1, labels[0].2), (16.0, 5.0));
    }

    #[test]
    fn failed_marker_sits_in_the_left_padding() {
        // 1×: 3 px bar in 8 px of padding → x 2, a 3 px gap to column 0.
        assert_eq!(failed_marker_x(8.0, 3.0), 2.0);
        // 2×: 6 px bar in 16 px.
        assert_eq!(failed_marker_x(16.0, 6.0), 5.0);
        // No room: flush with the window edge, like before the padding existed.
        assert_eq!(failed_marker_x(0.0, 3.0), 0.0);
        assert_eq!(failed_marker_x(2.0, 3.0), 0.0);
    }
}
