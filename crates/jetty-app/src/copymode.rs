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
    /// Fixed selection anchor as an ABSOLUTE buffer line (captured when `v`/`V`
    /// was pressed). Content-pinned, NOT viewport-pinned: scrolling while
    /// selecting extends into scrollback instead of sliding the whole selection.
    pub anchor_line: i32,
    pub anchor_col: usize,
}

impl CopyMode {
    pub fn new(row: usize, col: usize) -> Self {
        // `anchor_line` is unused until `begin_select` captures a real one
        // (guarded by `selecting`), so 0 is a safe placeholder here.
        CopyMode { row, col, selecting: false, line_mode: false, anchor_line: 0, anchor_col: col }
    }

    /// Begin (or restart) a selection anchored at the current cursor cell.
    /// `anchor_line` is the cursor's ABSOLUTE buffer line right now — the app
    /// computes it from the terminal so the anchor is pinned to content.
    pub fn begin_select(&mut self, line_mode: bool, anchor_line: i32) {
        self.selecting = true;
        self.line_mode = line_mode;
        self.anchor_line = anchor_line;
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
/// rows-of-chars (`rows` × `cols`, a wide char's spacer cell =
/// [`jetty_core::WIDE_SPACER`]). Vertical motions past a viewport edge return
/// a `ScrollReq` and clamp the cursor to the edge row; word motions cross rows
/// WITHIN the viewport (no scroll — use `j`/Ctrl+d at the edge).
///
/// The cursor never rests on a spacer: `h`/`l` step over a wide char in one
/// press, and every other motion lands on the char's first cell (the app
/// re-snaps with [`snap_to_char`] after a scroll moved new content under it).
/// A spacer reads as part of its char, so a run of CJK text is one word.
pub fn apply_motion(
    cm: &CopyMode,
    motion: Motion,
    rows: usize,
    cols: usize,
    viewport: &[Vec<char>],
) -> MotionOut {
    let row = cm.row.min(rows.saturating_sub(1));
    let col = snap_to_char(viewport, row, cm.col.min(cols.saturating_sub(1)));
    // Every in-viewport motion lands on a char's first cell.
    let still = |r: usize, c: usize| MotionOut { row: r, col: snap_to_char(viewport, r, c), scroll: ScrollReq::None };
    match motion {
        Motion::Left => still(row, col.saturating_sub(1)),
        Motion::Right => {
            let next = next_char_col(viewport, row, col);
            still(row, if next < cols { next } else { col })
        }
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

/// `col` moved off a wide char's spacer onto the char itself (the cell its
/// glyph starts in); any other cell is returned unchanged.
pub fn snap_to_char(vp: &[Vec<char>], row: usize, col: usize) -> usize {
    if col > 0 && at(vp, row, col) == jetty_core::WIDE_SPACER {
        col - 1
    } else {
        col
    }
}

/// The column of the char after the one at `col`: one cell on, two past a wide
/// char. May be `cols` (no next char on this row).
fn next_char_col(vp: &[Vec<char>], row: usize, col: usize) -> usize {
    let next = col + 1;
    if at(vp, row, next) == jetty_core::WIDE_SPACER {
        next + 1
    } else {
        next
    }
}

/// Derive the two selection endpoints (in reading order, with sub-cell side
/// flags) from the fixed anchor and the current cursor. The START endpoint takes
/// `Side::Left` (`left_half=true`) and the END `Side::Right` (`left_half=false`),
/// so `selection_start` + `selection_update` cover BOTH endpoint cells
/// inclusively regardless of direction (BLOCKING 2). Returns
/// `((row, col, left_half), (row, col, left_half))` = (start, end).
pub fn selection_endpoints<L: Ord + Copy>(
    anchor: (L, usize),
    cursor: (L, usize),
) -> ((L, usize, bool), (L, usize, bool)) {
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
    // Within the current row, starting AFTER the cursor's char (past its
    // spacer, for a wide one): skip whitespace, then advance to this word's end.
    let mut c = next_char_col(vp, row, col);
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

    /// Like [`vp`], with each CJK ideograph followed by its wide-char spacer —
    /// the shape `Terminal::viewport_rows_chars` hands copy-mode.
    fn vpw(rows: &[&str], cols: usize) -> Vec<Vec<char>> {
        rows.iter()
            .map(|r| {
                let mut v: Vec<char> = Vec::new();
                for c in r.chars() {
                    v.push(c);
                    if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                        v.push(jetty_core::WIDE_SPACER);
                    }
                }
                v.resize(cols, ' ');
                v
            })
            .collect()
    }
    fn go(v: &[Vec<char>], from: (usize, usize), m: Motion) -> (usize, usize) {
        let o = apply_motion(&cm(from.0, from.1), m, v.len(), v[0].len(), v);
        (o.row, o.col)
    }

    #[test]
    fn h_and_l_step_over_a_wide_char_in_one_press() {
        // "a世界b": a=0, 世=1–2, 界=3–4, b=5. The spacer cells are the right
        // halves of the glyphs — the cursor must never stop on one.
        let v = vpw(&["a世界b"], 8);
        assert_eq!(go(&v, (0, 0), Motion::Right), (0, 1));
        assert_eq!(go(&v, (0, 1), Motion::Right), (0, 3), "l crosses 世 at once");
        assert_eq!(go(&v, (0, 3), Motion::Right), (0, 5));
        assert_eq!(go(&v, (0, 5), Motion::Left), (0, 3), "h lands on 界, not its spacer");
        assert_eq!(go(&v, (0, 3), Motion::Left), (0, 1));
        // A wide char in the last two columns: `l` has nowhere to go.
        let v = vpw(&["ab世"], 4);
        assert_eq!(go(&v, (0, 2), Motion::Right), (0, 2));
    }

    #[test]
    fn vertical_moves_never_land_on_a_spacer() {
        // Row 1 has 世 at cols 1–2: coming down from col 2 lands on 世 itself.
        let v = vpw(&["abcd", "a世d"], 4);
        assert_eq!(go(&v, (0, 2), Motion::Down), (1, 1));
        let v = vpw(&["a世d", "abcd"], 4);
        assert_eq!(go(&v, (1, 2), Motion::Up), (0, 1));
    }

    #[test]
    fn word_motions_treat_a_cjk_run_as_one_word() {
        // "世界 日本語 x": 世界=0–3, 日本語=5–10, x=12.
        let v = vpw(&["世界 日本語 x"], 16);
        assert_eq!(go(&v, (0, 0), Motion::WordFwd), (0, 5), "w skips the whole run");
        assert_eq!(go(&v, (0, 5), Motion::WordFwd), (0, 12));
        assert_eq!(go(&v, (0, 0), Motion::WordEnd), (0, 2), "e ends ON 界, not its spacer");
        assert_eq!(go(&v, (0, 5), Motion::WordEnd), (0, 9), "e ends on 語");
        assert_eq!(go(&v, (0, 2), Motion::WordEnd), (0, 9), "e from a word's last (wide) char → next word's end");
        assert_eq!(go(&v, (0, 12), Motion::WordBack), (0, 5));
        assert_eq!(go(&v, (0, 9), Motion::WordBack), (0, 5));
        // `$` on a row ending in a wide char lands on the char.
        assert_eq!(go(&v, (0, 0), Motion::LineEnd), (0, 12));
        let v = vpw(&["ab 世界"], 10);
        assert_eq!(go(&v, (0, 0), Motion::LineEnd), (0, 5));
        // The snap helper the app runs after a scroll moved new content under
        // the cursor.
        assert_eq!(snap_to_char(&v, 0, 6), 5);
        assert_eq!(snap_to_char(&v, 0, 5), 5);
        assert_eq!(snap_to_char(&v, 0, 9), 9);
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
        // Absolute buffer lines (i32) incl. NEGATIVE history lines: a cursor in
        // scrollback (line -3) before an anchor at line 1 orders correctly.
        assert_eq!(
            selection_endpoints((1i32, 4), (-3i32, 2)),
            ((-3i32, 2, true), (1i32, 4, false))
        );
    }
}
