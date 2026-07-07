#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CellSnapshot {
    pub c: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    /// Whether this cell is part of the current text selection.
    pub selected: bool,
}

impl Default for CellSnapshot {
    fn default() -> Self {
        CellSnapshot { c: ' ', fg: [220, 220, 220], bg: [18, 18, 23], selected: false }
    }
}

/// One visible scrollback-search match segment in VIEWPORT coordinates
/// (`row` 0 = top visible line; `col_end` inclusive). A match spanning
/// multiple wrapped rows yields one `SearchHit` per row. Produced by
/// `Terminal::search_viewport_hits`; consumed by the highlight-rect builder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchHit {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
    /// Whether this segment belongs to the CURRENT match (the one the
    /// counter points at) — rendered with a stronger tint.
    pub is_current: bool,
}

#[derive(Clone, Debug)]
pub struct GridSnapshot {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<CellSnapshot>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// Whether the cursor should be drawn. Apps hide the cursor via DECTCEM
    /// (`\e[?25l`); when hidden, alacritty reports `CursorShape::Hidden` and this
    /// is set to `false` so the renderer skips the block cursor.
    pub cursor_visible: bool,
    /// Terminal background color as RGBA — set from the theme.
    pub bg_rgba: [u8; 4],
    /// Cursor block color — set from the theme.
    pub cursor_rgb: [u8; 3],
    /// How many lines the view is currently scrolled up (0 = at the bottom).
    pub scroll_offset: usize,
    /// Maximum scroll offset = number of lines in scrollback history.
    /// 0 means no scrollback (no scrollbar should be drawn).
    pub scroll_max: usize,
}

impl GridSnapshot {
    pub fn cell(&self, row: usize, col: usize) -> &CellSnapshot {
        &self.cells[row * self.cols + col]
    }
    pub fn row_text(&self, row: usize) -> String {
        (0..self.cols).map(|c| self.cell(row, c).c).collect::<String>()
    }
}
