//! The shell cursor: its look (`[cursor]` config: shape variants, stroke
//! thickness, the unfocused look, where its color comes from), the
//! contrast-safe caret flash, and the faint row guide under the cursor line.
//!
//! Shared by the main window, detached windows and jetty-shot (one
//! [`cursor_draw`] call each), so the three can never drift.
//!
//! Layering (unchanged from before the style pack): a SOLID block is painted in
//! the background pass, UNDER the glyphs, with the glyph it covers recolored for
//! contrast (`CursorDraw::glyph` → `GridPaint::cursor_glyph`); every thin shape
//! (beam, underlines, the hollow outline) covers no glyph and draws over the
//! text. The glyph color never animates — only the block's own color and scale
//! ride the caret flash — so the per-row shaping cache is untouched.

use crate::colors::{caret_flash_target, contrast_ratio, cursor_text_color};
use crate::quad::Rect;
use jetty_core::{CursorShapeSnap, GridSnapshot, Theme};

/// Beam / underline stroke as a fraction of the cell (beam: of its width,
/// underline: of its height) — the look before `[cursor] thickness` existed.
pub const CURSOR_THICKNESS_DEFAULT: f32 = 0.12;
/// Clamp range for `[cursor] thickness`.
pub const CURSOR_THICKNESS_MIN: f32 = 0.04;
pub const CURSOR_THICKNESS_MAX: f32 = 0.5;
/// The hollow (unfocused) outline is this fraction of the stroke: 0.1 of the
/// cell width at the default 0.12, exactly the outline before the style pack.
const HOLLOW_OF_STROKE: f32 = 0.1 / CURSOR_THICKNESS_DEFAULT;
/// A thick ("vintage") underline is this many strokes tall (0.3 cell at the
/// default), capped at half the cell.
const THICK_UNDERLINE_STROKES: f32 = 2.5;
/// `color = "auto"`: below this contrast between the theme cursor and the cell
/// it sits on, the cursor switches to reverse video (the cell's own colors).
pub const CURSOR_AUTO_MIN_CONTRAST: f32 = 2.0;
/// How far the cursor-row guide band moves from the page color toward the
/// foreground — mixed in sRGB, so it reads equally faint on dark and light
/// themes (a low-alpha quad blends in LINEAR light: a glaring band on a dark
/// page, nearly nothing on a light one).
pub const CURSOR_GUIDE_MIX: f32 = 0.07;

/// How an UNFOCUSED window draws its cursor (`[cursor] unfocused`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnfocusedCursor {
    /// A solid block becomes its outline; beam/underline stay (the classic look).
    #[default]
    Hollow,
    /// Draw exactly as when focused.
    Unchanged,
    /// No cursor at all while unfocused.
    None,
}

/// How an underline cursor is drawn — DECSCUSR 3/4 and the `underline`,
/// `double_underline`, `thick_underline` shapes all use this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineCursor {
    #[default]
    Single,
    /// Two thin strokes with a stroke-wide gap.
    Double,
    /// A tall bar ("vintage" terminals): 2.5 strokes, at most half the cell.
    Thick,
}

/// Where the cursor takes its color (`[cursor] color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorColor {
    /// The theme's cursor color (or the program's OSC 12) — the classic look.
    #[default]
    Theme,
    /// Reverse video of the cell under it: the block takes the glyph's color and
    /// the glyph the cell background (follows syntax colors).
    Cell,
    /// The theme color, unless it is nearly invisible on the cell under it
    /// (under [`CURSOR_AUTO_MIN_CONTRAST`]) — then reverse video.
    Auto,
}

/// The render-side cursor look. `Default` is exactly the pre-`[cursor]` cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorStyle {
    /// Beam / underline stroke as a fraction of the cell (clamped to
    /// [`CURSOR_THICKNESS_MIN`]..=[`CURSOR_THICKNESS_MAX`] here).
    pub thickness: f32,
    pub unfocused: UnfocusedCursor,
    pub underline: UnderlineCursor,
    pub color: CursorColor,
}

impl Default for CursorStyle {
    fn default() -> Self {
        CursorStyle {
            thickness: CURSOR_THICKNESS_DEFAULT,
            unfocused: UnfocusedCursor::Hollow,
            underline: UnderlineCursor::Single,
            color: CursorColor::Theme,
        }
    }
}

/// The cursor's resting colors: `block` paints the shape, `glyph` is the color of
/// the character drawn ON a solid block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorColors {
    pub block: [u8; 3],
    pub glyph: [u8; 3],
}

/// The cell under the cursor, if it is on the grid.
fn cursor_cell(snap: &GridSnapshot) -> Option<&jetty_core::CellSnapshot> {
    (snap.cursor_row < snap.rows && snap.cursor_col < snap.cols)
        .then(|| snap.cell(snap.cursor_row, snap.cursor_col))
}

/// The cursor's resting colors for `mode` (see [`CursorColor`]).
pub fn cursor_colors(snap: &GridSnapshot, theme: &Theme, mode: CursorColor) -> CursorColors {
    let themed = CursorColors {
        block: snap.cursor_rgb,
        glyph: cursor_text_color(theme, snap.cursor_rgb),
    };
    // Reverse video of the cell — unless the cell's own colors are (near) equal
    // (concealed text, a blank cell painted fg == bg), which would draw an
    // invisible cursor.
    let reverse = || {
        cursor_cell(snap)
            .filter(|c| contrast_ratio(c.fg, c.bg) >= 1.5)
            .map(|c| CursorColors { block: c.fg, glyph: c.bg })
    };
    match mode {
        CursorColor::Theme => themed,
        CursorColor::Cell => reverse().unwrap_or(themed),
        CursorColor::Auto => match cursor_cell(snap) {
            Some(c) if contrast_ratio(snap.cursor_rgb, c.bg) < CURSOR_AUTO_MIN_CONTRAST => {
                reverse().unwrap_or(themed)
            }
            _ => themed,
        },
    }
}

/// The caret flash envelope at progress `t` (0..=1): `bump = 4e(1−e)` with
/// `e = 1−(1−t)²` — 0 at both ends, 1 at t = 1 − 1/√2 ≈ 0.29 (a fast attack, a
/// slower release). Color and the block scale both ride it.
fn flash_bump(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - t) * (1.0 - t);
    4.0 * e * (1.0 - e)
}

/// `base` moved toward `target` (0..1 RGB) by `k` (0..=1).
fn lerp_rgb(base: [u8; 3], target: [f32; 3], k: f32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let b = base[i] as f32 / 255.0;
        out[i] = ((b + (target[i] - b) * k) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The block color during a caret flash at progress `t` with the configured
/// `flash` color — exposed for the all-themes contrast test.
pub fn caret_flash_color(colors: CursorColors, cell_bg: [u8; 3], flash: [f32; 3], t: f32) -> [u8; 3] {
    let target = caret_flash_target(flash, cell_bg, colors.glyph);
    lerp_rgb(colors.block, target, flash_bump(t))
}

/// One frame's cursor: the solid block painted UNDER the glyphs (`under`), the
/// thin shapes drawn over them (`over`), and the recolored glyph on a solid
/// block (`glyph`, for `GridPaint::cursor_glyph`).
#[derive(Clone, Default)]
pub struct CursorDraw {
    pub under: Option<Rect>,
    pub over: Vec<Rect>,
    pub glyph: Option<(usize, usize, [u8; 3])>,
}

/// Build the cursor for the current frame — the ONE per-frame quad rebuild
/// (everything else is cached), because the caret flash animates.
///
/// `flash` = `(t, configured color)` while a caret flash burst runs. Its color
/// is made contrast-safe per cell ([`caret_flash_target`]); a solid block also
/// scales up to 1.15× about its cell (clamped so row 0 never grows into the tab
/// bar). A wide (CJK / emoji) char spans its spacer cell too. Returns nothing
/// when the cursor is hidden, off the grid, or unfocused with
/// [`UnfocusedCursor::None`].
#[allow(clippy::too_many_arguments)]
pub fn cursor_draw(
    snap: &GridSnapshot,
    theme: &Theme,
    cell_w: f32,
    cell_h: f32,
    x_offset: f32,
    y_offset: f32,
    focused: bool,
    flash: Option<(f32, [f32; 3])>,
    style: &CursorStyle,
) -> CursorDraw {
    use unicode_width::UnicodeWidthChar;
    let Some(cell) = cursor_cell(snap).filter(|_| snap.cursor_visible) else {
        return CursorDraw::default();
    };
    let shape = match (focused, style.unfocused, snap.cursor_shape) {
        (true, _, s) => s,
        (false, UnfocusedCursor::None, _) => return CursorDraw::default(),
        (false, UnfocusedCursor::Hollow, CursorShapeSnap::Block) => CursorShapeSnap::HollowBlock,
        (false, _, s) => s,
    };
    let wide = snap.cursor_col + 1 < snap.cols && cell.c.width() == Some(2);
    let span_w = if wide { cell_w * 2.0 } else { cell_w };
    let colors = cursor_colors(snap, theme, style.color);
    let solid = shape == CursorShapeSnap::Block;
    // Only a solid block carries a glyph; the thin shapes are judged against the
    // cell alone.
    let glyph_on = if solid { colors.glyph } else { cell.bg };
    let (rgb, scale) = match flash {
        Some((t, flash_rgb)) => {
            let bump = flash_bump(t);
            let target = caret_flash_target(flash_rgb, cell.bg, glyph_on);
            (lerp_rgb(colors.block, target, bump), 1.0 + 0.15 * bump)
        }
        None => (colors.block, 1.0),
    };
    let color = [rgb[0], rgb[1], rgb[2], 255];
    let base_x = x_offset + snap.cursor_col as f32 * cell_w;
    let base_y = y_offset + snap.cursor_row as f32 * cell_h;
    let stroke = style.thickness.clamp(CURSOR_THICKNESS_MIN, CURSOR_THICKNESS_MAX);
    let mut out = CursorDraw::default();
    match shape {
        CursorShapeSnap::Block => {
            // Center-scale bump about the cell(s); on grid row 0 the top edge is
            // clamped to the grid top so the bump only grows downward there.
            let w = span_w * scale;
            let h = cell_h * scale;
            let raw_top = base_y - (h - cell_h) * 0.5;
            let top = raw_top.max(y_offset);
            out.under = Some(Rect::new(base_x - (w - span_w) * 0.5, top, w, raw_top + h - top, color));
            out.glyph = Some((snap.cursor_row, snap.cursor_col, colors.glyph));
        }
        CursorShapeSnap::Beam => {
            let w = (cell_w * stroke).max(1.0);
            out.over.push(Rect::new(base_x, base_y, w, cell_h, color));
        }
        CursorShapeSnap::Underline => {
            let bottom = base_y + cell_h;
            match style.underline {
                UnderlineCursor::Single => {
                    let h = (cell_h * stroke).max(1.0);
                    out.over.push(Rect::new(base_x, bottom - h, span_w, h, color));
                }
                UnderlineCursor::Double => {
                    // Whole pixels so the gap between the strokes stays crisp.
                    let s = (cell_h * stroke * 0.75).round().max(1.0);
                    out.over.push(Rect::new(base_x, bottom - s, span_w, s, color));
                    out.over.push(Rect::new(base_x, bottom - 3.0 * s, span_w, s, color));
                }
                UnderlineCursor::Thick => {
                    let h = (cell_h * stroke * THICK_UNDERLINE_STROKES).min(cell_h * 0.5).max(2.0);
                    out.over.push(Rect::new(base_x, bottom - h, span_w, h, color));
                }
            }
        }
        CursorShapeSnap::HollowBlock => {
            let b = (cell_w * stroke * HOLLOW_OF_STROKE).max(1.0);
            out.over.push(Rect::new(base_x, base_y, span_w, b, color)); // top
            out.over.push(Rect::new(base_x, base_y + cell_h - b, span_w, b, color)); // bottom
            out.over.push(Rect::new(base_x, base_y, b, cell_h, color)); // left
            out.over.push(Rect::new(base_x + span_w - b, base_y, b, cell_h, color)); // right
        }
    }
    out
}

/// The cursor's resting rect `[x, y, w, h]` in window coordinates — the shape
/// the cursor trail chases (block and hollow: the cell(s); beam: its bar; any
/// underline: the strokes' extent). `None` when [`cursor_draw`] draws nothing.
#[allow(clippy::too_many_arguments)]
pub fn cursor_trail_rect(
    snap: &GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    x_offset: f32,
    y_offset: f32,
    focused: bool,
    style: &CursorStyle,
) -> Option<[f32; 4]> {
    use unicode_width::UnicodeWidthChar;
    let cell = cursor_cell(snap).filter(|_| snap.cursor_visible)?;
    let shape = match (focused, style.unfocused, snap.cursor_shape) {
        (false, UnfocusedCursor::None, _) => return None,
        (_, _, s) => s,
    };
    let wide = snap.cursor_col + 1 < snap.cols && cell.c.width() == Some(2);
    let span_w = if wide { cell_w * 2.0 } else { cell_w };
    let x = x_offset + snap.cursor_col as f32 * cell_w;
    let y = y_offset + snap.cursor_row as f32 * cell_h;
    let stroke = style.thickness.clamp(CURSOR_THICKNESS_MIN, CURSOR_THICKNESS_MAX);
    Some(match shape {
        CursorShapeSnap::Block | CursorShapeSnap::HollowBlock => [x, y, span_w, cell_h],
        CursorShapeSnap::Beam => [x, y, (cell_w * stroke).max(1.0), cell_h],
        CursorShapeSnap::Underline => {
            let h = match style.underline {
                UnderlineCursor::Single => (cell_h * stroke).max(1.0),
                UnderlineCursor::Double => 3.0 * (cell_h * stroke * 0.75).round().max(1.0),
                UnderlineCursor::Thick => (cell_h * stroke * THICK_UNDERLINE_STROKES).min(cell_h * 0.5).max(2.0),
            };
            [x, y + cell_h - h, span_w, h]
        }
    })
}

/// The faint band across the cursor's row (`[cursor] guide`): the page color
/// [`CURSOR_GUIDE_MIX`] of the way to the foreground, across the whole grid
/// row, opaque like every cell background. Painted FIRST in the background
/// pass, so cells with a background of their own (and the selection) cover it
/// and the glyphs draw over it. `None` while the cursor is hidden or scrolled
/// off the grid.
pub fn cursor_guide_rect(
    snap: &GridSnapshot,
    theme: &Theme,
    cell_w: f32,
    cell_h: f32,
    x_offset: f32,
    y_offset: f32,
) -> Option<Rect> {
    if !snap.cursor_visible || snap.cursor_row >= snap.rows {
        return None;
    }
    let bg = snap.bg_rgba;
    let mix = |i: usize| {
        (bg[i] as f32 + (theme.fg[i] as f32 - bg[i] as f32) * CURSOR_GUIDE_MIX).round().clamp(0.0, 255.0) as u8
    };
    Some(Rect::new(
        x_offset,
        y_offset + snap.cursor_row as f32 * cell_h,
        snap.cols as f32 * cell_w,
        cell_h,
        [mix(0), mix(1), mix(2), 255],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colors::{contrast_ratio, CARET_FLASH_MIN_CONTRAST};
    use jetty_core::{CellSnapshot, CursorShapeSnap, GridSnapshot};

    fn grid(cols: usize, rows: usize) -> GridSnapshot {
        GridSnapshot {
            cols,
            rows,
            cells: vec![CellSnapshot::default(); cols * rows],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            bg_rgba: [0, 0, 0, 255],
            cursor_rgb: [200, 200, 200],
            scroll_offset: 0,
            scroll_max: 0,
            cursor_shape: CursorShapeSnap::Block,
            graphemes: Vec::new(),
        }
    }

    fn theme() -> Theme {
        Theme::by_name("catppuccin_mocha")
    }

    /// Where the flash envelope peaks (bump = 1).
    const FLASH_PEAK_T: f32 = 1.0 - std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn the_flash_envelope_peaks_at_one_and_rests_at_both_ends() {
        assert!((flash_bump(FLASH_PEAK_T) - 1.0).abs() < 1e-5);
        assert_eq!(flash_bump(0.0), 0.0);
        assert!(flash_bump(1.0).abs() < 1e-6);
        for k in 0..=100 {
            let b = flash_bump(k as f32 / 100.0);
            assert!((0.0..=1.0 + 1e-6).contains(&b));
        }
    }

    fn draw(g: &GridSnapshot, focused: bool, flash: Option<f32>, style: &CursorStyle) -> CursorDraw {
        cursor_draw(g, &theme(), 10.0, 20.0, 0.0, 0.0, focused, flash.map(|t| (t, [1.0; 3])), style)
    }

    fn all(d: &CursorDraw) -> Vec<Rect> {
        d.under.iter().copied().chain(d.over.iter().copied()).collect()
    }

    #[test]
    fn block_is_one_full_cell_rect_under_the_text() {
        let mut g = grid(5, 3);
        g.cursor_col = 1;
        g.cursor_row = 2;
        let d = draw(&g, true, None, &CursorStyle::default());
        let r = d.under.expect("solid block goes under the glyphs");
        assert!(d.over.is_empty());
        assert_eq!((r.x, r.y, r.w, r.h), (10.0, 40.0, 10.0, 20.0));
        assert_eq!(d.glyph.map(|(r, c, _)| (r, c)), Some((2, 1)), "its glyph is recolored");
    }

    #[test]
    fn x_offset_moves_every_shape() {
        let mut g = grid(5, 3);
        g.cursor_col = 1;
        for shape in [CursorShapeSnap::Block, CursorShapeSnap::Beam, CursorShapeSnap::Underline] {
            g.cursor_shape = shape;
            let at0 = all(&cursor_draw(&g, &theme(), 10.0, 20.0, 0.0, 0.0, true, None, &CursorStyle::default()));
            let at8 = all(&cursor_draw(&g, &theme(), 10.0, 20.0, 8.0, 0.0, true, None, &CursorStyle::default()));
            for (a, b) in at0.iter().zip(&at8) {
                assert_eq!(b.x - a.x, 8.0, "{shape:?}");
            }
        }
    }

    #[test]
    fn thin_shapes_draw_over_the_text() {
        let mut g = grid(5, 3);
        for shape in [CursorShapeSnap::Beam, CursorShapeSnap::Underline] {
            g.cursor_shape = shape;
            let d = draw(&g, true, None, &CursorStyle::default());
            assert!(d.under.is_none() && d.over.len() == 1 && d.glyph.is_none(), "{shape:?}");
        }
        g.cursor_shape = CursorShapeSnap::HollowBlock;
        assert_eq!(draw(&g, true, None, &CursorStyle::default()).over.len(), 4);
    }

    #[test]
    fn default_thickness_matches_the_old_constants() {
        // Beam 0.12 × cell width, underline 0.12 × cell height, hollow 0.1 × width.
        let mut g = grid(5, 3);
        let style = CursorStyle::default();
        g.cursor_shape = CursorShapeSnap::Beam;
        assert!((draw(&g, true, None, &style).over[0].w - 1.2).abs() < 1e-5);
        g.cursor_shape = CursorShapeSnap::Underline;
        let ul = draw(&g, true, None, &style).over[0];
        assert!((ul.h - 2.4).abs() < 1e-5 && (ul.y - (20.0 - 2.4)).abs() < 1e-5);
        g.cursor_shape = CursorShapeSnap::HollowBlock;
        assert!((draw(&g, true, None, &style).over[0].h - 1.0).abs() < 1e-5);
    }

    #[test]
    fn thickness_scales_beam_and_underline_and_is_clamped() {
        let mut g = grid(5, 3);
        g.cursor_shape = CursorShapeSnap::Beam;
        let thick = CursorStyle { thickness: 0.3, ..Default::default() };
        assert!((draw(&g, true, None, &thick).over[0].w - 3.0).abs() < 1e-5);
        let silly = CursorStyle { thickness: 9.0, ..Default::default() };
        assert!((draw(&g, true, None, &silly).over[0].w - 5.0).abs() < 1e-5, "clamped to half a cell");
        let hair = CursorStyle { thickness: 0.0, ..Default::default() };
        assert!(draw(&g, true, None, &hair).over[0].w >= 1.0, "never thinner than a pixel");
    }

    #[test]
    fn double_and_thick_underlines() {
        let mut g = grid(5, 3);
        g.cursor_shape = CursorShapeSnap::Underline;
        let double = CursorStyle { underline: UnderlineCursor::Double, ..Default::default() };
        let d = draw(&g, true, None, &double);
        assert_eq!(d.over.len(), 2, "two strokes");
        let (lo, hi) = (d.over[0], d.over[1]);
        assert_eq!(lo.y + lo.h, 20.0, "the lower stroke sits on the cell bottom");
        assert!(hi.y + hi.h < lo.y, "a gap separates the strokes");
        assert_eq!(lo.h.fract(), 0.0, "whole-pixel strokes stay crisp");
        let thick = CursorStyle { underline: UnderlineCursor::Thick, ..Default::default() };
        let t = draw(&g, true, None, &thick).over[0];
        assert!((t.h - 6.0).abs() < 1e-4, "2.5 strokes = 0.3 cell, got {}", t.h);
        assert_eq!(t.y + t.h, 20.0);
        // The underline variant never changes a block or a beam.
        g.cursor_shape = CursorShapeSnap::Beam;
        assert_eq!(draw(&g, true, None, &thick).over.len(), 1);
    }

    #[test]
    fn unfocused_modes() {
        let mut g = grid(5, 3);
        let hollow = CursorStyle::default();
        assert_eq!(draw(&g, false, None, &hollow).over.len(), 4, "block hollows out");
        assert!(draw(&g, false, None, &hollow).under.is_none());
        let unchanged = CursorStyle { unfocused: UnfocusedCursor::Unchanged, ..Default::default() };
        assert!(draw(&g, false, None, &unchanged).under.is_some(), "solid block kept");
        let none = CursorStyle { unfocused: UnfocusedCursor::None, ..Default::default() };
        assert!(all(&draw(&g, false, None, &none)).is_empty(), "no cursor at all");
        assert!(draw(&g, true, None, &none).under.is_some(), "focused is unaffected");
        g.cursor_shape = CursorShapeSnap::Beam;
        assert_eq!(draw(&g, false, None, &hollow).over.len(), 1, "a beam stays a beam");
    }

    #[test]
    fn wide_char_spans_both_cells() {
        let mut g = grid(5, 3);
        g.cursor_col = 1;
        g.cells[1].c = '漢';
        assert_eq!(draw(&g, true, None, &CursorStyle::default()).under.unwrap().w, 20.0);
        assert_eq!(draw(&g, false, None, &CursorStyle::default()).over[0].w, 20.0);
        g.cursor_shape = CursorShapeSnap::Underline;
        let double = CursorStyle { underline: UnderlineCursor::Double, ..Default::default() };
        assert!(draw(&g, true, None, &double).over.iter().all(|r| r.w == 20.0));
        // A wide char in the LAST column can't span.
        g.cursor_shape = CursorShapeSnap::Block;
        g.cursor_col = 4;
        g.cells[4].c = '漢';
        assert_eq!(draw(&g, true, None, &CursorStyle::default()).under.unwrap().w, 10.0);
    }

    #[test]
    fn hidden_or_off_grid_cursor_draws_nothing() {
        let mut g = grid(5, 3);
        g.cursor_visible = false;
        assert!(all(&draw(&g, true, None, &CursorStyle::default())).is_empty());
        g.cursor_visible = true;
        g.cursor_row = 3;
        assert!(all(&draw(&g, true, None, &CursorStyle::default())).is_empty());
        assert!(cursor_guide_rect(&g, &theme(), 10.0, 20.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn flash_shifts_color_and_grows_the_block_centered() {
        let mut g = grid(5, 3);
        g.cursor_row = 1;
        g.cursor_rgb = [0, 0, 0];
        g.cells[5].bg = [0, 0, 0];
        let style = CursorStyle::default();
        let rest = draw(&g, true, None, &style).under.unwrap();
        let peak = draw(&g, true, Some(0.5), &style).under.unwrap();
        assert!(peak.color[0] > rest.color[0], "toward the flash color");
        assert!(peak.w > rest.w && peak.x < rest.x, "grows about its center");
        // The envelope returns to rest at both ends.
        let end = draw(&g, true, Some(1.0), &style).under.unwrap();
        assert_eq!((end.w, end.color), (rest.w, rest.color));
    }

    #[test]
    fn flash_never_bleeds_above_the_grid_top() {
        let g = grid(5, 3);
        let y_offset = 36.0;
        let d = cursor_draw(&g, &theme(), 10.0, 20.0, 0.0, y_offset, true, Some((0.5, [1.0; 3])), &CursorStyle::default());
        assert!(d.under.unwrap().y >= y_offset - 1e-3);
    }

    #[test]
    fn color_modes() {
        let mut g = grid(5, 3);
        g.cursor_rgb = [250, 180, 80];
        g.cells[0].fg = [120, 200, 255];
        g.cells[0].bg = [10, 10, 30];
        let t = theme();
        let themed = cursor_colors(&g, &t, CursorColor::Theme);
        assert_eq!(themed.block, [250, 180, 80]);
        let cell = cursor_colors(&g, &t, CursorColor::Cell);
        assert_eq!(cell, CursorColors { block: [120, 200, 255], glyph: [10, 10, 30] });
        // Auto keeps the theme color while it reads on the cell…
        assert_eq!(cursor_colors(&g, &t, CursorColor::Auto), themed);
        // …and goes reverse-video when the cell bg swallows it.
        g.cells[0].bg = [245, 175, 85];
        g.cells[0].fg = [20, 20, 20];
        assert_eq!(
            cursor_colors(&g, &t, CursorColor::Auto),
            CursorColors { block: [20, 20, 20], glyph: [245, 175, 85] }
        );
        // Concealed text (fg == bg) never makes the cursor invisible.
        g.cells[0].fg = g.cells[0].bg;
        assert_eq!(cursor_colors(&g, &t, CursorColor::Cell), themed);
    }

    #[test]
    fn trail_rect_follows_the_drawn_shape() {
        let mut g = grid(5, 3);
        g.cursor_row = 1;
        g.cursor_col = 2;
        let style = CursorStyle::default();
        let r = |g: &GridSnapshot, s: &CursorStyle| cursor_trail_rect(g, 10.0, 20.0, 4.0, 30.0, true, s);
        assert_eq!(r(&g, &style), Some([24.0, 50.0, 10.0, 20.0]), "block: the cell, at the origin");
        g.cursor_shape = CursorShapeSnap::Beam;
        let beam = r(&g, &style).unwrap();
        assert!((beam[2] - 1.2).abs() < 1e-5 && beam[3] == 20.0);
        g.cursor_shape = CursorShapeSnap::Underline;
        let ul = r(&g, &style).unwrap();
        assert!((ul[1] + ul[3] - 70.0).abs() < 1e-4, "sits on the cell bottom");
        // Hidden / unfocused-none → nothing to chase.
        let none = CursorStyle { unfocused: UnfocusedCursor::None, ..Default::default() };
        assert!(cursor_trail_rect(&g, 10.0, 20.0, 0.0, 0.0, false, &none).is_none());
        g.cursor_visible = false;
        assert!(r(&g, &style).is_none());
    }

    #[test]
    fn guide_spans_the_cursor_row() {
        let mut g = grid(7, 4);
        g.cursor_row = 2;
        let r = cursor_guide_rect(&g, &theme(), 10.0, 20.0, 4.0, 30.0).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (4.0, 70.0, 70.0, 20.0));
        // 7% of the way from the page (the snapshot's bg, OSC 11 included)
        // toward the theme fg, opaque.
        let (bg, fg) = (g.bg_rgba, theme().fg);
        for i in 0..3 {
            let want = bg[i] as f32 + (fg[i] as f32 - bg[i] as f32) * CURSOR_GUIDE_MIX;
            assert!((r.color[i] as f32 - want).abs() <= 0.5);
        }
        assert_eq!(r.color[3], 255);
    }

    /// The caret flash keeps the cursor visible on EVERY built-in theme, for the
    /// default white flash, a black one and a mid-tone accent:
    /// * at the peak the block contrasts ≥ 3:1 with the page;
    /// * the target is never a worse pick than the configured color, black or
    ///   white (scored by the smaller of its page / glyph contrasts);
    /// * where the theme draws the classic inverted cell (glyph = page color —
    ///   21 of the 22 built-ins) the glyph on the block never drops below its
    ///   resting contrast or 3:1, whichever is lower, at any point of the burst.
    ///   (palenight's cursor is too close to its page for that, so its glyph is
    ///   the light fg; no flash target can then contrast with both — it keeps
    ///   white, as before the fix, and the cursor itself stays plainly visible.)
    #[test]
    fn caret_flash_stays_readable_on_every_builtin_theme() {
        let score = |c: [u8; 3], bg: [u8; 3], glyph: [u8; 3]| {
            contrast_ratio(c, bg).min(contrast_ratio(c, glyph))
        };
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let bg = [t.bg[0], t.bg[1], t.bg[2]];
            let mut g = grid(3, 1);
            g.cursor_rgb = t.cursor;
            g.cells[0].bg = bg;
            g.cells[0].fg = t.fg;
            let colors = cursor_colors(&g, &t, CursorColor::Theme);
            let rest = contrast_ratio(colors.block, colors.glyph);
            for flash in [[1.0f32; 3], [0.0; 3], [0.5, 0.45, 0.6]] {
                let peak = caret_flash_color(colors, bg, flash, FLASH_PEAK_T);
                assert!(
                    contrast_ratio(peak, bg) >= CARET_FLASH_MIN_CONTRAST,
                    "{}: flash {flash:?} peaks at {:.2}:1 on the page",
                    t.name,
                    contrast_ratio(peak, bg)
                );
                let picked = score(peak, bg, colors.glyph);
                let flash8 = flash.map(|v| (v * 255.0).round() as u8);
                if score(flash8, bg, colors.glyph) >= CARET_FLASH_MIN_CONTRAST {
                    assert_eq!(peak, flash8, "{}: a readable configured color is kept", t.name);
                } else {
                    for cand in [flash8, [0; 3], [255; 3]] {
                        assert!(
                            picked >= score(cand, bg, colors.glyph) - 0.01,
                            "{}: flash {flash:?} picked a worse target than {cand:?}",
                            t.name
                        );
                    }
                }
                if colors.glyph != bg {
                    continue;
                }
                for k in 0..=20 {
                    let tt = k as f32 / 20.0;
                    let c = contrast_ratio(caret_flash_color(colors, bg, flash, tt), colors.glyph);
                    assert!(
                        c >= rest.min(CARET_FLASH_MIN_CONTRAST) - 0.05,
                        "{}: flash {flash:?} t={tt}: glyph {c:.2}:1 (rest {rest:.2}:1)",
                        t.name
                    );
                }
            }
        }
    }

    #[test]
    fn a_white_flash_on_a_dark_theme_is_untouched() {
        // The owner's setup (ayu_dark, white flash): exactly the configured color.
        let t = Theme::by_name("ayu_dark");
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        let glyph = cursor_text_color(&t, t.cursor);
        assert_eq!(caret_flash_target([1.0; 3], bg, glyph), [1.0; 3]);
    }

    #[test]
    fn a_white_flash_on_a_light_theme_turns_dark() {
        let t = Theme::by_name("solarized_light");
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        let glyph = cursor_text_color(&t, t.cursor);
        assert_eq!(caret_flash_target([1.0; 3], bg, glyph), [0.0; 3]);
    }
}
