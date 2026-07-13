//! Keyboard COPY-MODE (Ctrl+Shift+Space): a modal vi-cursor over the viewport +
//! scrollback for mouse-free text selection.
//!
//! The motion logic lives here — PURE and testable off `App`. [`apply_motion`]
//! takes the current cursor, the grid dims, and the WHOLE viewport as
//! rows-of-chars (so `w`/`b`/`e` word motions see neighbouring rows — BLOCKING 4)
//! and returns the new cursor + a scroll request. The app owns the alacritty
//! `Selection` (started/updated per keystroke via the DERIVED sub-cell sides from
//! [`selection_endpoints`] — BLOCKING 2), the clipboard yank, and the render.

/// A copy-mode motion, decoded from the key press by the app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    WordFwd,
    WordBack,
    WordEnd,
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
}

/// What the app should do to the terminal scroll after a motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollReq {
    None,
    /// `scroll_lines(n)`: +n scrolls UP into history, -n toward the bottom.
    Lines(i32),
    /// Jump to the top of history (`g`).
    Top,
    /// Jump to the live bottom (`G`).
    Bottom,
}

/// The modal copy-mode state: a keyboard cursor over the viewport plus, once
/// `v`/`V` is pressed, a selection anchored at the cursor's position at that
/// moment.
#[derive(Clone, Copy, Debug)]
pub struct CopyMode {
    pub row: usize,
    pub col: usize,
    pub selecting: bool,
    pub line_mode: bool,
    /// Fixed selection anchor (cursor position when `v`/`V` was pressed).
    pub anchor_row: usize,
    pub anchor_col: usize,
}

impl CopyMode {
    pub fn new(row: usize, col: usize) -> Self {
        CopyMode { row, col, selecting: false, line_mode: false, anchor_row: row, anchor_col: col }
    }

    /// Begin (or restart) a selection anchored at the current cursor cell.
    pub fn begin_select(&mut self, line_mode: bool) {
        self.selecting = true;
        self.line_mode = line_mode;
        self.anchor_row = self.row;
        self.anchor_col = self.col;
    }
}

/// The result of a motion: the new cursor cell + a scroll request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MotionOut {
    pub row: usize,
    pub col: usize,
    pub scroll: ScrollReq,
}

/// Apply a motion to the cursor. Pure: `viewport` is the visible grid as
/// rows-of-chars (`rows` × `cols`). Vertical motions past a viewport edge return
/// a `ScrollReq` and clamp the cursor to the edge row; word motions cross rows
/// WITHIN the viewport (no scroll — use `j`/Ctrl+d at the edge).
pub fn apply_motion(
    cm: &CopyMode,
    motion: Motion,
    rows: usize,
    cols: usize,
    viewport: &[Vec<char>],
) -> MotionOut {
    let row = cm.row.min(rows.saturating_sub(1));
    let col = cm.col.min(cols.saturating_sub(1));
    let still = |r: usize, c: usize| MotionOut { row: r, col: c, scroll: ScrollReq::None };
    match motion {
        Motion::Left => still(row, col.saturating_sub(1)),
        Motion::Right => still(row, (col + 1).min(cols.saturating_sub(1))),
        Motion::Up => {
            if row == 0 {
                MotionOut { row: 0, col, scroll: ScrollReq::Lines(1) }
            } else {
                still(row - 1, col)
            }
        }
        Motion::Down => {
            if row + 1 >= rows {
                MotionOut { row: rows.saturating_sub(1), col, scroll: ScrollReq::Lines(-1) }
            } else {
                still(row + 1, col)
            }
        }
        Motion::LineStart => still(row, 0),
        Motion::LineEnd => {
            let last = viewport
                .get(row)
                .and_then(|r| r.iter().rposition(|c| !c.is_whitespace()))
                .unwrap_or(0);
            still(row, last.min(cols.saturating_sub(1)))
        }
        Motion::WordFwd => {
            let (r, c) = word_forward(viewport, row, col, rows, cols);
            still(r, c)
        }
        Motion::WordBack => {
            let (r, c) = word_back(viewport, row, col, cols);
            still(r, c)
        }
        Motion::WordEnd => {
            let (r, c) = word_end(viewport, row, col, rows, cols);
            still(r, c)
        }
        Motion::HalfPageUp => {
            MotionOut { row, col, scroll: ScrollReq::Lines((rows / 2).max(1) as i32) }
        }
        Motion::HalfPageDown => {
            MotionOut { row, col, scroll: ScrollReq::Lines(-((rows / 2).max(1) as i32)) }
        }
        Motion::Top => MotionOut { row: 0, col: 0, scroll: ScrollReq::Top },
        Motion::Bottom => MotionOut { row: rows.saturating_sub(1), col: 0, scroll: ScrollReq::Bottom },
    }
}

/// Derive the two selection endpoints (in reading order, with sub-cell side
/// flags) from the fixed anchor and the current cursor. The START endpoint takes
/// `Side::Left` (`left_half=true`) and the END `Side::Right` (`left_half=false`),
/// so `selection_start` + `selection_update` cover BOTH endpoint cells
/// inclusively regardless of direction (BLOCKING 2). Returns
/// `((row, col, left_half), (row, col, left_half))` = (start, end).
pub fn selection_endpoints(
    anchor: (usize, usize),
    cursor: (usize, usize),
) -> ((usize, usize, bool), (usize, usize, bool)) {
    if cursor >= anchor {
        ((anchor.0, anchor.1, true), (cursor.0, cursor.1, false))
    } else {
        ((cursor.0, cursor.1, true), (anchor.0, anchor.1, false))
    }
}

fn at(vp: &[Vec<char>], r: usize, c: usize) -> char {
    vp.get(r).and_then(|row| row.get(c)).copied().unwrap_or(' ')
}

/// The start column of the word ending at (or covering) column `c` in row `r`.
fn word_start_col(vp: &[Vec<char>], r: usize, c: usize) -> usize {
    let mut c = c;
    while c > 0 && !at(vp, r, c - 1).is_whitespace() {
        c -= 1;
    }
    c
}

/// Next word START (a word = a run of non-whitespace). Row-end is a word
/// boundary: at the end of a row, move to the first word of a subsequent row.
/// Clamps to the last cell when there is no further word.
fn word_forward(vp: &[Vec<char>], row: usize, col: usize, rows: usize, cols: usize) -> (usize, usize) {
    if rows == 0 || cols == 0 {
        return (row, col);
    }
    // Within the current row: skip the rest of the current word, then whitespace.
    let mut c = col;
    if c < cols && !at(vp, row, c).is_whitespace() {
        while c < cols && !at(vp, row, c).is_whitespace() {
            c += 1;
        }
    }
    while c < cols && at(vp, row, c).is_whitespace() {
        c += 1;
    }
    if c < cols {
        return (row, c);
    }
    // Otherwise the first word of a later row.
    for r in (row + 1)..rows {
        if let Some(fc) = (0..cols).find(|&c| !at(vp, r, c).is_whitespace()) {
            return (r, fc);
        }
    }
    (rows - 1, cols - 1)
}

/// Previous word START. Row-start is a word boundary: at the start of a row,
/// move to the last word of a preceding row.
fn word_back(vp: &[Vec<char>], row: usize, col: usize, cols: usize) -> (usize, usize) {
    if cols == 0 {
        return (row, col);
    }
    // Within the current row: step left over whitespace, then to the word start.
    if col > 0 {
        let mut c = col - 1;
        while c > 0 && at(vp, row, c).is_whitespace() {
            c -= 1;
        }
        if !at(vp, row, c).is_whitespace() {
            return (row, word_start_col(vp, row, c));
        }
    }
    // Otherwise the last word of an earlier row.
    for r in (0..row).rev() {
        if let Some(last) = (0..cols).rev().find(|&c| !at(vp, r, c).is_whitespace()) {
            return (r, word_start_col(vp, r, last));
        }
    }
    (0, 0)
}

/// Next word END. Row-end is a word boundary: at the end of a row, move to the
/// end of the first word of a subsequent row.
fn word_end(vp: &[Vec<char>], row: usize, col: usize, rows: usize, cols: usize) -> (usize, usize) {
    if rows == 0 || cols == 0 {
        return (row, col);
    }
    // Within the current row, starting AFTER the cursor: skip whitespace, then
    // advance to this word's end.
    let mut c = col + 1;
    if c < cols {
        while c < cols && at(vp, row, c).is_whitespace() {
            c += 1;
        }
        if c < cols {
            while c + 1 < cols && !at(vp, row, c + 1).is_whitespace() {
                c += 1;
            }
            return (row, c);
        }
    }
    // Otherwise the end of the first word of a later row.
    for r in (row + 1)..rows {
        if let Some(fc) = (0..cols).find(|&c| !at(vp, r, c).is_whitespace()) {
            let mut c = fc;
            while c + 1 < cols && !at(vp, r, c + 1).is_whitespace() {
                c += 1;
            }
            return (r, c);
        }
    }
    (rows - 1, cols - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(rows: &[&str], cols: usize) -> Vec<Vec<char>> {
        rows.iter()
            .map(|r| {
                let mut v: Vec<char> = r.chars().collect();
                v.resize(cols, ' ');
                v
            })
            .collect()
    }
    fn cm(row: usize, col: usize) -> CopyMode {
        CopyMode::new(row, col)
    }

    #[test]
    fn hjkl_clamps_and_scrolls_at_edges() {
        let v = vp(&["abc", "def", "ghi"], 3);
        // Left/Right clamp.
        assert_eq!(apply_motion(&cm(0, 0), Motion::Left, 3, 3, &v), MotionOut { row: 0, col: 0, scroll: ScrollReq::None });
        assert_eq!(apply_motion(&cm(0, 2), Motion::Right, 3, 3, &v).col, 2);
        // Up at row 0 → scroll up one line, stay on row 0.
        assert_eq!(apply_motion(&cm(0, 1), Motion::Up, 3, 3, &v), MotionOut { row: 0, col: 1, scroll: ScrollReq::Lines(1) });
        // Down at bottom → scroll down one line, stay on last row.
        assert_eq!(apply_motion(&cm(2, 1), Motion::Down, 3, 3, &v), MotionOut { row: 2, col: 1, scroll: ScrollReq::Lines(-1) });
        // Interior up/down move the row, no scroll.
        assert_eq!(apply_motion(&cm(1, 1), Motion::Up, 3, 3, &v).row, 0);
        assert_eq!(apply_motion(&cm(1, 1), Motion::Down, 3, 3, &v).row, 2);
    }

    #[test]
    fn line_start_end_on_trailing_blank_row() {
        let v = vp(&["hi there   ", "        ", ""], 11);
        // $ lands on the last non-blank ('e' of "there" at col 7).
        assert_eq!(apply_motion(&cm(0, 0), Motion::LineEnd, 3, 11, &v).col, 7);
        // 0 → col 0.
        assert_eq!(apply_motion(&cm(0, 5), Motion::LineStart, 3, 11, &v).col, 0);
        // An all-blank row → $ stays at col 0 (no non-blank).
        assert_eq!(apply_motion(&cm(1, 4), Motion::LineEnd, 3, 11, &v).col, 0);
    }

    #[test]
    fn word_motions_cross_rows() {
        // Row 0 ends with "foo", row 1 begins with "bar baz".
        let v = vp(&["one foo", "bar baz", "qux"], 7);
        // From col 0 ('o' of "one"), w → start of "foo" (col 4).
        assert_eq!((|| { let o = apply_motion(&cm(0, 0), Motion::WordFwd, 3, 7, &v); (o.row, o.col) })(), (0, 4));
        // From "foo" (col 4), w crosses the row boundary to "bar" (row 1, col 0).
        assert_eq!((|| { let o = apply_motion(&cm(0, 4), Motion::WordFwd, 3, 7, &v); (o.row, o.col) })(), (1, 0));
        // e from "bar" start → end of "bar" (row 1 col 2).
        assert_eq!((|| { let o = apply_motion(&cm(1, 0), Motion::WordEnd, 3, 7, &v); (o.row, o.col) })(), (1, 2));
        // b from "bar" start crosses back up to the start of "foo" (row 0 col 4).
        assert_eq!((|| { let o = apply_motion(&cm(1, 0), Motion::WordBack, 3, 7, &v); (o.row, o.col) })(), (0, 4));
    }

    #[test]
    fn half_page_and_top_bottom() {
        let v = vp(&["a", "b", "c", "d"], 1);
        assert_eq!(apply_motion(&cm(0, 0), Motion::HalfPageUp, 4, 1, &v).scroll, ScrollReq::Lines(2));
        assert_eq!(apply_motion(&cm(0, 0), Motion::HalfPageDown, 4, 1, &v).scroll, ScrollReq::Lines(-2));
        assert_eq!(apply_motion(&cm(2, 0), Motion::Top, 4, 1, &v), MotionOut { row: 0, col: 0, scroll: ScrollReq::Top });
        assert_eq!(apply_motion(&cm(0, 0), Motion::Bottom, 4, 1, &v), MotionOut { row: 3, col: 0, scroll: ScrollReq::Bottom });
    }

    #[test]
    fn selection_endpoints_derives_side_by_reading_order() {
        // Forward: anchor before cursor → anchor Left, cursor Right.
        assert_eq!(
            selection_endpoints((0, 0), (0, 4)),
            ((0, 0, true), (0, 4, false))
        );
        // Reverse: cursor before anchor → cursor Left, anchor Right.
        assert_eq!(
            selection_endpoints((0, 4), (0, 0)),
            ((0, 0, true), (0, 4, false))
        );
        // Cross-row forward.
        assert_eq!(
            selection_endpoints((1, 2), (3, 1)),
            ((1, 2, true), (3, 1, false))
        );
        // Same cell → single-cell inclusive.
        assert_eq!(
            selection_endpoints((2, 5), (2, 5)),
            ((2, 5, true), (2, 5, false))
        );
    }
}
