/// Packed per-cell text-attribute bitfield (`CellSnapshot::attrs`).
///
/// Layout (u8):
///   bit0    BOLD          (0x01)
///   bit1    ITALIC        (0x02)
///   bit2    STRIKE        (0x04)
///   bits3-5 UNDERLINE style (0=None 1=Single 2=Double 3=Undercurl 4=Dotted 5=Dashed)
///   bit6-7  reserved (future: overline / link-underline marker)
///
/// BLINK (SGR 5/6) is deliberately OUT OF SCOPE — alacritty_terminal 0.26 drops
/// the blink bit at the VT engine and a blink timer would fight the ~0%-idle
/// goal (same non-goal as ligatures). See v0.13 amendments.
pub mod attr {
    pub const BOLD: u8 = 0x01;
    pub const ITALIC: u8 = 0x02;
    pub const STRIKE: u8 = 0x04;
    /// Mask for the 3-bit underline-style field (bits 3-5).
    pub const UL_MASK: u8 = 0x38;
    /// Right-shift to bring the underline style into the low bits.
    pub const UL_SHIFT: u8 = 3;

    // Underline style values (stored pre-shift in bits 3-5 via `<< UL_SHIFT`).
    pub const UL_NONE: u8 = 0;
    pub const UL_SINGLE: u8 = 1;
    pub const UL_DOUBLE: u8 = 2;
    pub const UL_UNDERCURL: u8 = 3;
    pub const UL_DOTTED: u8 = 4;
    pub const UL_DASHED: u8 = 5;
}

/// The only attribute bits that change SHAPING (and therefore must force a grid
/// re-shape / enter the grid-hash): BOLD + ITALIC select a different font face.
/// STRIKE / underline style / underline color are drawn as quads on top and
/// never re-shape, so they are excluded here (SPEED: an underline toggle must
/// not invalidate cosmic-text's per-line shape cache).
pub const SHAPE_MASK: u8 = attr::BOLD | attr::ITALIC;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CellSnapshot {
    pub c: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    /// Resolved underline color (RGB). Set from `Cell::underline_color()` (SGR 58)
    /// when present, else the cell's FINAL resolved `fg` (post INVERSE/DIM/HIDDEN),
    /// so an underline with no explicit color tracks the glyph color.
    pub uline: [u8; 3],
    /// Packed text-attribute bitfield — see the [`attr`] module.
    pub attrs: u8,
    /// Whether this cell is part of the current text selection.
    pub selected: bool,
}

impl Default for CellSnapshot {
    fn default() -> Self {
        CellSnapshot {
            c: ' ',
            fg: [220, 220, 220],
            bg: [18, 18, 23],
            uline: [220, 220, 220],
            attrs: 0,
            selected: false,
        }
    }
}

impl CellSnapshot {
    #[inline]
    pub const fn is_bold(&self) -> bool {
        self.attrs & attr::BOLD != 0
    }
    #[inline]
    pub const fn is_italic(&self) -> bool {
        self.attrs & attr::ITALIC != 0
    }
    #[inline]
    pub const fn is_strike(&self) -> bool {
        self.attrs & attr::STRIKE != 0
    }
    /// Underline style value 0..=5 (0 = none). See the [`attr`] module.
    #[inline]
    pub const fn underline_style(&self) -> u8 {
        (self.attrs & attr::UL_MASK) >> attr::UL_SHIFT
    }
    /// The shaping-affecting attribute bits only (BOLD|ITALIC). Folded into the
    /// grid re-shape fingerprint; STRIKE/underline are excluded.
    #[inline]
    pub const fn shape_bits(&self) -> u8 {
        self.attrs & SHAPE_MASK
    }
}

/// Renderable cursor SHAPE (independent of visibility). `Hidden` is represented
/// by `GridSnapshot::cursor_visible == false`, so it is not a variant here.
/// Set from `content.cursor.shape` (DECSCUSR `CSI Ps SP q`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum CursorShapeSnap {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
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
    /// Renderable cursor shape (block / underline / beam / hollow). Only meaningful
    /// when `cursor_visible`. One byte on the per-FRAME header (not per cell).
    pub cursor_shape: CursorShapeSnap,
}

impl GridSnapshot {
    pub fn cell(&self, row: usize, col: usize) -> &CellSnapshot {
        &self.cells[row * self.cols + col]
    }
    pub fn row_text(&self, row: usize) -> String {
        (0..self.cols).map(|c| self.cell(row, c).c).collect::<String>()
    }
}
