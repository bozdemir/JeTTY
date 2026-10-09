//! The vte [`Handler`] JeTTY parses into: alacritty_terminal 0.26's `Term`,
//! with the places where it departs from xterm (VTE and kitty agree with xterm
//! on all of them) corrected on the way through. Every other call is forwarded
//! untouched and `#[inline(always)]`, and [`Vt`] is `Term` itself (a
//! transparent newtype), so vte's performer still calls straight into `Term`
//! (whose `input` stays out of line, as in alacritty) — a wrapper HOLDING
//! `&mut Term` cost a pointer hop per character, 1–4% of parse throughput.
//!
//! The corrections:
//! * DCH (`CSI Ps P`) with a count past the end of the line erased the cells
//!   LEFT of the cursor too; the count is clamped to the cells that remain.
//! * DCH / ICH / ECH / EL / ED through half of a wide char erase all of it (as
//!   xterm, VTE and kitty do): an orphaned first half drew its glyph over the
//!   next cell, an orphaned second half put the drawn cursor a cell off.
//! * LF / VT / FF / IND / RI left a pending autowrap armed, so the next
//!   character wrapped a line too low instead of landing in the last column.
//! * CUU / CUD (and CNL / CPL / VPR) stop at the scroll margins when the
//!   cursor is inside them; under origin mode (DECOM) CUU / CUD / CNL / CPL /
//!   CHA / HPA / VPR no longer jump below the region (alacritty added the top
//!   margin to an absolute row), and CPR reports the row relative to it.
//! * DECSTBM with its top margin below the screen is ignored, as in xterm
//!   (alacritty kept an empty region, and output stopped scrolling).
//! * Modes 47 / 1047 (the alternate screen without 1049's cursor save) and
//!   1048 (save / restore the cursor) work.
//! * DECSCNM (`CSI ? 5 h`, the reverse-video screen vim's visual bell
//!   flashes) is tracked here and drawn by the snapshot; alacritty ignored it.
//! * OSC 4 / 10 / 11 / 12 queries answer with the color a program set (pywal,
//!   base16-shell) instead of the theme's, so the background a program
//!   detects (neovim, bat, delta) is the one on screen.
//! * DA1 reports sixel graphics, so lsix, chafa, notcurses and tmux use them.
//! * OSC 0 / 2 titles are clipped to [`TITLE_MAX_BYTES`]: alacritty kept the
//!   whole payload and cloned it on every title-stack push (`CSI 22 t`, 4096
//!   deep), so ~1 MB of output could pin ~4 GiB per tab.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, NamedPrivateMode, PrivateMode, Rgb,
    ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};
use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::mpsc::Sender;

/// Most bytes of a title `Term` keeps. JeTTY shows at most 256 chars (≤ 1 KiB
/// of UTF-8); alacritty stores the whole OSC 0 / 2 payload — up to the 1 MiB
/// OSC cap — and clones it on every title-stack push (`CSI 22 t`, 4096 deep).
pub(crate) const TITLE_MAX_BYTES: usize = 1024;

/// The primary device attributes JeTTY reports: a VT220-class terminal (62)
/// with sixel graphics (4) and ANSI color (22). alacritty answers a bare VT102
/// (`CSI ? 6 c`), which tells sixel-probing programs there are no images.
pub(crate) const DA1_REPLY: &[u8] = b"\x1b[?62;4;22c";

/// What the corrections remember between calls (owned by `Terminal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VtState {
    /// Mirror of alacritty's private scroll region: `[top, bottom)` in screen
    /// lines. Kept in step at every change: DECSTBM, RIS, DECCOLM and a resize
    /// (`Terminal::resize` resets it, as `Term::resize` does).
    pub(crate) region: (i32, i32),
    /// DECSCNM (`CSI ? 5 h`): the whole screen in reverse video — what vim's
    /// visual bell flashes (terminfo `flash`). alacritty ignores the mode; the
    /// snapshot swaps the colors.
    pub(crate) reverse: bool,
}

impl VtState {
    pub(crate) fn new(rows: usize) -> VtState {
        VtState { region: (0, rows as i32), reverse: false }
    }
}

/// alacritty's `Term` as JeTTY's vte handler: the same memory, so vte's
/// performer reaches `Term` through the one pointer it already holds.
#[repr(transparent)]
pub(crate) struct Vt<T>(Term<T>);

/// The terminal's [`VtState`] and its PTY reply channel, while a parse runs.
type Context = Option<(VtState, NonNull<Sender<Vec<u8>>>)>;

thread_local! {
    /// What the corrections need besides the `Term`, published by [`parse`]
    /// for the length of one parse: the terminal's [`VtState`] (copied in and
    /// back out) and its PTY reply channel. Only the rare corrected sequences
    /// read it — never the per-character path.
    static CTX: Cell<Context> = const { Cell::new(None) };
}

/// Run one parse — `f`, a `Processor::advance` or `stop_sync` — with `term`
/// as the handler, the corrections seeing `st` (updated in place) and sending
/// their answers through `reply`: the PTY reply channel `Term`'s own answers
/// go through, so one written here keeps its place among them.
pub(crate) fn parse<T, R>(
    term: &mut Term<T>,
    st: &mut VtState,
    reply: &Sender<Vec<u8>>,
    f: impl FnOnce(&mut Vt<T>) -> R,
) -> R {
    CTX.with(|c| c.set(Some((*st, NonNull::from(reply)))));
    // SAFETY: `Vt<T>` is `#[repr(transparent)]` over `Term<T>` (same layout);
    // the exclusive borrow of `term` carries over to the result.
    let vt = unsafe { &mut *(term as *mut Term<T>).cast::<Vt<T>>() };
    let r = f(vt);
    if let Some((after, _)) = CTX.with(|c| c.take()) {
        *st = after;
    }
    r
}

/// The corrections' state, as [`parse`] published it.
fn state() -> VtState {
    CTX.with(|c| c.get()).map(|(st, _)| st).expect("a handler call outside `parse`")
}

fn set_state(st: VtState) {
    CTX.with(|c| c.set(c.get().map(|(_, reply)| (st, reply))));
}

impl<T: EventListener> Vt<T> {
    fn send(&self, bytes: Vec<u8>) {
        if let Some((_, reply)) = CTX.with(|c| c.get()) {
            // SAFETY: `parse` published a borrow it holds for the whole parse,
            // and every handler call happens inside a parse.
            let _ = unsafe { reply.as_ref() }.send(bytes);
        }
    }

    fn column(&self) -> usize {
        self.0.grid().cursor.point.column.0
    }

    fn origin(&self) -> bool {
        self.0.mode().contains(TermMode::ORIGIN)
    }

    /// Move to the ABSOLUTE screen line `line` (`Term::goto` reads its line
    /// relative to the top margin under DECOM, and clamps it to the region).
    fn goto_abs(&mut self, line: i32, col: usize) {
        let top = if self.origin() { state().region.0 } else { 0 };
        Handler::goto(&mut self.0, line - top, col);
    }

    /// CUU's target row: `n` up, stopping at the top margin when the cursor is
    /// at or below it (xterm's `CursorUp`).
    fn up_target(&self, n: usize) -> i32 {
        let line = self.0.grid().cursor.point.line.0;
        let top = state().region.0;
        let floor = if line >= top { top } else { 0 };
        line.saturating_sub(n.min(u16::MAX as usize) as i32).max(floor)
    }

    /// CUD's target row: `n` down, stopping at the bottom margin when the
    /// cursor is at or above it (xterm's `CursorDown`).
    fn down_target(&self, n: usize) -> i32 {
        let line = self.0.grid().cursor.point.line.0;
        let bottom = state().region.1 - 1;
        let ceil = if line <= bottom { bottom } else { self.0.screen_lines() as i32 - 1 };
        line.saturating_add(n.min(u16::MAX as usize) as i32).min(ceil)
    }

    fn full_region(&mut self) {
        set_state(VtState { region: (0, self.0.screen_lines() as i32), ..state() });
    }

    /// Erase the wide char straddling the boundary just left of column `col`
    /// on the cursor row (`col` = its second half), both halves, before an edit
    /// that splits it: an orphaned first half drew its glyph over the next
    /// cell, an orphaned second half put the drawn cursor a cell off.
    fn split_wide_at(&mut self, col: usize) {
        if col == 0 || col >= self.0.columns() {
            return;
        }
        let line = self.0.grid().cursor.point.line;
        let row = &mut self.0.grid_mut()[line];
        if row[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            row[Column(col)].flags.remove(Flags::WIDE_CHAR_SPACER);
            if row[Column(col - 1)].flags.contains(Flags::WIDE_CHAR) {
                row[Column(col - 1)].clear_wide();
            }
        }
    }
}

/// Forward `Handler` methods to `Term` unchanged.
macro_rules! forward {
    ($( fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*); )*) => {
        $(
            #[inline(always)]
            fn $name(&mut self $(, $arg: $ty)*) {
                Handler::$name(&mut self.0 $(, $arg)*)
            }
        )*
    };
}

impl<T: EventListener> Handler for Vt<T> {
    forward! {
        fn set_cursor_style(&mut self, style: Option<CursorStyle>);
        fn set_cursor_shape(&mut self, shape: CursorShape);
        fn input(&mut self, c: char);
        fn goto(&mut self, line: i32, col: usize);
        fn goto_line(&mut self, line: i32);
        fn move_forward(&mut self, cols: usize);
        fn move_backward(&mut self, cols: usize);
        fn put_tab(&mut self, count: u16);
        fn backspace(&mut self);
        fn carriage_return(&mut self);
        fn bell(&mut self);
        fn substitute(&mut self);
        fn newline(&mut self);
        fn set_horizontal_tabstop(&mut self);
        fn scroll_up(&mut self, lines: usize);
        fn scroll_down(&mut self, lines: usize);
        fn insert_blank_lines(&mut self, lines: usize);
        fn delete_lines(&mut self, lines: usize);
        fn move_backward_tabs(&mut self, count: u16);
        fn move_forward_tabs(&mut self, count: u16);
        fn save_cursor_position(&mut self);
        fn restore_cursor_position(&mut self);
        fn clear_tabs(&mut self, mode: TabulationClearMode);
        fn set_tabs(&mut self, interval: u16);
        fn terminal_attribute(&mut self, attr: Attr);
        fn set_mode(&mut self, mode: Mode);
        fn unset_mode(&mut self, mode: Mode);
        fn report_mode(&mut self, mode: Mode);
        fn set_keypad_application_mode(&mut self);
        fn unset_keypad_application_mode(&mut self);
        fn set_active_charset(&mut self, index: CharsetIndex);
        fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset);
        fn set_color(&mut self, index: usize, color: Rgb);
        fn reset_color(&mut self, index: usize);
        fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]);
        fn clipboard_load(&mut self, clipboard: u8, terminator: &str);
        fn decaln(&mut self);
        fn push_title(&mut self);
        fn pop_title(&mut self);
        fn text_area_size_pixels(&mut self);
        fn text_area_size_chars(&mut self);
        fn set_hyperlink(&mut self, link: Option<Hyperlink>);
        fn set_mouse_cursor_icon(&mut self, icon: CursorIcon);
        fn report_keyboard_mode(&mut self);
        fn push_keyboard_mode(&mut self, mode: KeyboardModes);
        fn pop_keyboard_modes(&mut self, to_pop: u16);
        fn set_keyboard_mode(&mut self, mode: KeyboardModes, behavior: KeyboardModesApplyBehavior);
        fn set_modify_other_keys(&mut self, mode: ModifyOtherKeys);
        fn report_modify_other_keys(&mut self);
        fn set_scp(&mut self, char_path: ScpCharPath, update_mode: ScpUpdateMode);
    }

    #[inline(always)]
    fn linefeed(&mut self) {
        Handler::linefeed(&mut self.0);
        self.0.grid_mut().cursor.input_needs_wrap = false;
    }

    fn set_title(&mut self, title: Option<String>) {
        let title = title.map(|mut t| {
            if t.len() > TITLE_MAX_BYTES {
                let mut end = TITLE_MAX_BYTES;
                while !t.is_char_boundary(end) {
                    end -= 1;
                }
                t.truncate(end);
            }
            t
        });
        Handler::set_title(&mut self.0, title);
    }

    fn reverse_index(&mut self) {
        Handler::reverse_index(&mut self.0);
        self.0.grid_mut().cursor.input_needs_wrap = false;
    }

    fn move_up(&mut self, lines: usize) {
        let (line, col) = (self.up_target(lines), self.column());
        self.goto_abs(line, col);
    }

    fn move_down(&mut self, lines: usize) {
        let (line, col) = (self.down_target(lines), self.column());
        self.goto_abs(line, col);
    }

    fn move_up_and_cr(&mut self, lines: usize) {
        let line = self.up_target(lines);
        self.goto_abs(line, 0);
    }

    fn move_down_and_cr(&mut self, lines: usize) {
        let line = self.down_target(lines);
        self.goto_abs(line, 0);
    }

    fn goto_col(&mut self, col: usize) {
        let line = self.0.grid().cursor.point.line.0;
        self.goto_abs(line, col);
    }

    fn device_status(&mut self, arg: usize) {
        if arg == 6 && self.origin() {
            let p = self.0.grid().cursor.point;
            let line = p.line.0 - state().region.0 + 1;
            self.send(format!("\x1b[{line};{}R", p.column.0 + 1).into_bytes());
        } else {
            Handler::device_status(&mut self.0, arg);
        }
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let lines = self.0.screen_lines();
        let top = top.max(1);
        let bottom = bottom.map_or(lines, |b| b.min(lines));
        if top >= bottom {
            return; // xterm ignores a region that is empty once clamped to the screen
        }
        Handler::set_scrolling_region(&mut self.0, top, Some(bottom));
        set_state(VtState { region: (top as i32 - 1, bottom as i32), ..state() });
    }

    fn reset_state(&mut self) {
        Handler::reset_state(&mut self.0);
        set_state(VtState::new(self.0.screen_lines()));
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        match mode {
            PrivateMode::Unknown(5) => set_state(VtState { reverse: true, ..state() }),
            PrivateMode::Unknown(1048) => Handler::save_cursor_position(&mut self.0),
            PrivateMode::Unknown(47 | 1047) => {
                if !self.0.mode().contains(TermMode::ALT_SCREEN) {
                    self.0.swap_alt();
                }
            }
            _ => {
                Handler::set_private_mode(&mut self.0, mode);
                if mode == PrivateMode::Named(NamedPrivateMode::ColumnMode) {
                    self.full_region(); // DECCOLM resets the margins
                }
            }
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        match mode {
            PrivateMode::Unknown(5) => set_state(VtState { reverse: false, ..state() }),
            PrivateMode::Unknown(1048) => Handler::restore_cursor_position(&mut self.0),
            PrivateMode::Unknown(47 | 1047) => {
                if self.0.mode().contains(TermMode::ALT_SCREEN) {
                    self.0.swap_alt();
                }
            }
            _ => {
                Handler::unset_private_mode(&mut self.0, mode);
                if mode == PrivateMode::Named(NamedPrivateMode::ColumnMode) {
                    self.full_region();
                }
            }
        }
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        let on = match mode {
            PrivateMode::Unknown(5) => Some(state().reverse),
            PrivateMode::Unknown(47 | 1047) => Some(self.0.mode().contains(TermMode::ALT_SCREEN)),
            _ => None,
        };
        match on {
            Some(on) => {
                let state = if on { 1 } else { 2 };
                self.send(format!("\x1b[?{};{state}$y", mode.raw()).into_bytes());
            }
            None => Handler::report_private_mode(&mut self.0, mode),
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let x = self.column();
        let count = count.min(self.0.columns() - x);
        self.split_wide_at(x);
        self.split_wide_at(x + count);
        Handler::delete_chars(&mut self.0, count);
    }

    fn insert_blank(&mut self, count: usize) {
        let (x, cols) = (self.column(), self.0.columns());
        let count = count.min(cols - x);
        self.split_wide_at(x);
        // The cells from `cols - count` on are pushed off the end of the line.
        if cols - count > x {
            self.split_wide_at(cols - count);
        }
        Handler::insert_blank(&mut self.0, count);
    }

    fn erase_chars(&mut self, count: usize) {
        let x = self.column();
        self.split_wide_at(x);
        self.split_wide_at(x.saturating_add(count));
        Handler::erase_chars(&mut self.0, count);
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        let x = self.column();
        match mode {
            // `Term` clears nothing to the right while a wrap is pending.
            LineClearMode::Right if !self.0.grid().cursor.input_needs_wrap => self.split_wide_at(x),
            LineClearMode::Left => self.split_wide_at(x + 1),
            _ => {}
        }
        Handler::clear_line(&mut self.0, mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        let x = self.column();
        match mode {
            ClearMode::Below => self.split_wide_at(x),
            ClearMode::Above => self.split_wide_at(x + 1),
            _ => {}
        }
        Handler::clear_screen(&mut self.0, mode);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => self.send(DA1_REPLY.to_vec()),
            _ => Handler::identify_terminal(&mut self.0, intermediate),
        }
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        // A color a program set (OSC 4 / 10 / 11 / 12) wins over the theme's,
        // which `Term` would ask the event listener for. Same reply format.
        match self.0.colors()[index] {
            Some(Rgb { r, g, b }) => self.send(
                format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}").into_bytes(),
            ),
            None => Handler::dynamic_color_sequence(&mut self.0, prefix, index, terminator),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Terminal;
    use alacritty_terminal::event::Event;
    use alacritty_terminal::index::Line;
    use alacritty_terminal::term::cell::Cell;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, TermMode};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Rows (as text), cursor (row, col) and replies after feeding `seq`.
    fn run(cols: usize, rows: usize, seq: &str) -> (Vec<String>, (usize, usize), String) {
        let mut t = Terminal::new(cols, rows);
        t.feed(seq.as_bytes());
        let s = t.snapshot();
        let text = (0..rows).map(|r| s.row_text(r)).collect();
        (text, (s.cursor_row, s.cursor_col), String::from_utf8_lossy(&t.drain_pty_writes()).into_owned())
    }

    #[test]
    fn dch_past_the_end_of_the_line_keeps_the_cells_left_of_the_cursor() {
        for n in [3, 5, 99, 65535] {
            let (rows, cursor, _) = run(10, 1, &format!("0123456789\x1b[1;9H\x1b[{n}P"));
            assert_eq!((rows[0].as_str(), cursor), ("01234567  ", (0, 8)), "DCH {n}");
        }
        let (rows, _, _) = run(10, 1, "0123456789\x1b[1;4H\x1b[3P");
        assert_eq!(rows[0], "0126789   ", "a DCH inside the line is unchanged");
    }

    #[test]
    fn editing_through_half_a_wide_char_erases_all_of_it() {
        // xterm, VTE and kitty never leave half a wide char behind. An orphaned
        // first half drew its glyph over the next cell; an orphaned second half
        // put the drawn cursor a cell off (in column 0: a debug panic, and the
        // cursor drawn in the last column in release).
        let cases = [
            ("ab中cd\x1b[1;4H\x1b[P", "ab cd ", (0, 3)),
            ("中x\x1b[1;1H\x1b[P", " x    ", (0, 0)),
            ("ab中cd\x1b[1;4H\x1b[@", "ab   c", (0, 3)),
            ("abcd中\x1b[1;1H\x1b[@", " abcd ", (0, 0)),
            ("中x\x1b[1;1H\x1b[X\x1b[1;2H", "  x   ", (0, 1)),
            ("a中x\x1b[1;3H\x1b[X", "a  x  ", (0, 2)),
            ("a中x\x1b[1;3H\x1b[K", "a     ", (0, 2)),
            ("中x\x1b[1;1H\x1b[1K\x1b[1;2H", "  x   ", (0, 1)),
            ("a中x\x1b[1;3H\x1b[J", "a     ", (0, 2)),
            ("中x\x1b[1;1H\x1b[1J\x1b[1;2H", "  x   ", (0, 1)),
        ];
        for (seq, row, cursor) in cases {
            let (rows, at, _) = run(6, 2, seq);
            assert_eq!((rows[0].as_str(), at), (row, cursor), "{seq:?}");
        }
        // Whole wide chars are untouched by edits that do not split them.
        let (rows, _, _) = run(6, 1, "中文x\x1b[1;3H\x1b[2P");
        assert_eq!(rows[0], "中 x   ");
    }

    #[test]
    fn line_feeds_and_reverse_index_cancel_a_pending_wrap() {
        // After a full line the cursor waits in the last column for a wrap. LF,
        // VT, FF and IND (and RI, upward) move it a line and cancel the wrap: the
        // next character lands in the last column of that line (xterm, VTE,
        // kitty) — it used to wrap and land a line further down, in column 0.
        for seq in ["abcde\n*", "abcde\x0b*", "abcde\x0c*", "abcde\x1bD*"] {
            let (rows, cursor, _) = run(5, 3, seq);
            assert_eq!(rows, ["abcde", "    *", "     "], "{seq:?}");
            assert_eq!(cursor, (1, 4), "{seq:?}: wrap pending again after the `*`");
        }
        let (rows, _, _) = run(5, 3, "\x1b[2;1Habcde\x1bM*");
        assert_eq!(rows, ["    *", "abcde", "     "], "RI");
        // The usual CR LF, and NEL, still start the next line in column 0.
        for seq in ["abcde\r\n*", "abcde\x1bE*"] {
            assert_eq!(run(5, 3, seq).0, ["abcde", "*    ", "     "], "{seq:?}");
        }
    }

    #[test]
    fn cursor_up_and_down_stop_at_the_scroll_margins() {
        // Region rows 5–10 (1-based). From inside it CUU / CUD / CPL / CNL / VPR
        // stop at its edges; from outside they run to the screen's.
        let cases = [
            ("\x1b[7;3H\x1b[20A", (4, 2)),
            ("\x1b[7;3H\x1b[20B", (9, 2)),
            ("\x1b[7;3H\x1b[20F", (4, 0)),
            ("\x1b[7;3H\x1b[20E", (9, 0)),
            ("\x1b[7;3H\x1b[20e", (9, 2)),
            ("\x1b[3;3H\x1b[20A", (0, 2)),
            ("\x1b[3;3H\x1b[20B", (9, 2)),
            ("\x1b[11;3H\x1b[20B", (11, 2)),
            ("\x1b[11;3H\x1b[20A", (4, 2)),
            ("\x1b[7;3H\x1b[A", (5, 2)),
        ];
        for (moves, want) in cases {
            let (_, cursor, _) = run(10, 12, &format!("\x1b[5;10r{moves}"));
            assert_eq!(cursor, want, "{moves:?}");
        }
    }

    #[test]
    fn relative_moves_under_origin_mode_stay_in_the_region() {
        // DECOM with region rows 5–10: CUP is relative to row 5. alacritty added
        // the top margin AGAIN to the absolute row for CUU / CUD / CNL / CPL /
        // CHA / HPA / VPR, sending the cursor down to the bottom margin.
        let cases = [
            ("\x1b[3;1H\x1b[A", (5, 0)),
            ("\x1b[3;1H\x1b[B", (7, 0)),
            ("\x1b[3;3H\x1b[E", (7, 0)),
            ("\x1b[3;3H\x1b[F", (5, 0)),
            ("\x1b[3;1H\x1b[5G", (6, 4)),
            ("\x1b[3;1H\x1b[5`", (6, 4)),
            ("\x1b[3;1H\x1b[e", (7, 0)),
            ("\x1b[3;1H\x1b[9A", (4, 0)),
            ("\x1b[3;1H\x1b[9B", (9, 0)),
            ("\x1b[2;3H\x1b[3d", (6, 2)),
        ];
        for (moves, want) in cases {
            let (_, cursor, _) = run(10, 12, &format!("\x1b[5;10r\x1b[?6h{moves}"));
            assert_eq!(cursor, want, "{moves:?}");
        }
    }

    #[test]
    fn cursor_position_report_is_relative_to_the_origin() {
        let (_, _, reply) = run(10, 12, "\x1b[5;10r\x1b[?6h\x1b[3;4H\x1b[6n");
        assert_eq!(reply, "\x1b[3;4R");
        let (_, _, reply) = run(10, 12, "\x1b[5;10r\x1b[7;4H\x1b[6n");
        assert_eq!(reply, "\x1b[7;4R", "without DECOM: absolute");
    }

    #[test]
    fn a_scroll_region_starting_below_the_screen_is_ignored() {
        // xterm ignores DECSTBM unless bottom > top once clamped to the screen.
        // alacritty set an EMPTY region past the last row: from then on output
        // at the bottom stopped scrolling and overwrote the last line.
        let mut seq = String::from("\x1b[30;40r");
        for i in 0..10 {
            seq.push_str(&format!("line{i}\r\n"));
        }
        let (rows, cursor, _) = run(8, 5, &seq);
        assert_eq!(rows, ["line6   ", "line7   ", "line8   ", "line9   ", "        "]);
        assert_eq!(cursor, (4, 0));
        // A valid region still homes the cursor; one clamped to the screen works.
        let (_, cursor, _) = run(8, 5, "\x1b[3;3Hx\x1b[2;99r");
        assert_eq!(cursor, (0, 0));
    }

    #[test]
    fn the_region_mirror_follows_resets_and_resizes() {
        // RIS, DECCOLM and a resize put the region back to the whole screen.
        for reset in ["\x1bc", "\x1b[?3l", "\x1b[?3h"] {
            let (_, cursor, _) = run(10, 12, &format!("\x1b[5;10r{reset}\x1b[7;3H\x1b[20A"));
            assert_eq!(cursor, (0, 2), "{reset:?}");
        }
        let mut t = Terminal::new(10, 12);
        t.feed(b"\x1b[5;10r");
        t.resize(10, 14);
        t.feed(b"\x1b[7;3H\x1b[20B");
        let s = t.snapshot();
        assert_eq!((s.cursor_row, s.cursor_col), (13, 2));
    }

    #[test]
    fn modes_47_and_1047_switch_screens_and_1048_saves_the_cursor() {
        for mode in ["47", "1047"] {
            let (rows, _, reply) =
                run(10, 3, &format!("main\x1b[?{mode}halt\x1b[?{mode}$p\x1b[?{mode}l*\x1b[?{mode}$p"));
            assert_eq!(rows[0], "main*     ", "{mode}: the main screen is back");
            assert_eq!(reply, format!("\x1b[?{mode};1$y\x1b[?{mode};2$y"));
            let (rows, _, _) = run(10, 3, &format!("main\x1b[?{mode}hALT"));
            assert_eq!(rows[0], "    ALT   ", "{mode}: on the alternate screen");
        }
        let (rows, cursor, _) = run(10, 1, "ab\x1b[?1048hxyz\x1b[?1048l*");
        assert_eq!((rows[0].as_str(), cursor), ("ab*yz     ", (0, 3)));
    }

    #[test]
    fn color_queries_report_colors_a_program_set() {
        let (_, _, reply) = run(10, 1, "\x1b]11;#102030\x07\x1b]11;?\x07");
        assert_eq!(reply, "\x1b]11;rgb:1010/2020/3030\x07");
        let (_, _, reply) = run(10, 1, "\x1b]10;rgb:aa/bb/cc\x1b\\\x1b]10;?\x1b\\");
        assert_eq!(reply, "\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\");
        let (_, _, reply) = run(10, 1, "\x1b]4;1;#123456\x07\x1b]4;1;?;2;?\x07");
        assert!(reply.starts_with("\x1b]4;1;rgb:1212/3434/5656\x07\x1b]4;2;rgb:"), "{reply:?}");
        let (_, _, reply) = run(10, 1, "\x1b]12;#654321\x07\x1b]12;?\x07");
        assert_eq!(reply, "\x1b]12;rgb:6565/4343/2121\x07");
        // Reset to the theme: the theme's color again (same as never set).
        let (_, _, reset) = run(10, 1, "\x1b]11;#102030\x07\x1b]111\x07\x1b]11;?\x07");
        let (_, _, theme) = run(10, 1, "\x1b]11;?\x07");
        assert_eq!(reset, theme);
        assert_ne!(theme, "\x1b]11;rgb:1010/2020/3030\x07");
    }

    #[test]
    fn primary_device_attributes_report_sixel() {
        for seq in ["\x1b[c", "\x1b[0c", "\x1bZ"] {
            assert_eq!(run(10, 1, seq).2, "\x1b[?62;4;22c", "{seq:?}");
        }
        assert_eq!(run(10, 1, "\x1b[>c").2, "\x1b[>0;2600;1c", "DA2 unchanged");
    }

    // ── forwarding is transparent ────────────────────────────────────────────

    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<String>>>);

    impl EventListener for Recorder {
        fn send_event(&self, event: Event) {
            self.0.borrow_mut().push(format!("{event:?}"));
        }
    }

    type State = (Vec<Cell>, String, TermMode, Vec<Option<Rgb>>, CursorStyle);

    fn observe(term: &Term<Recorder>) -> State {
        let grid = term.grid();
        let mut cells = Vec::new();
        for l in grid.topmost_line().0..=grid.bottommost_line().0 {
            for c in 0..grid.columns() {
                cells.push(grid[Line(l)][Column(c)].clone());
            }
        }
        let cursors = format!("{:?} {:?}", grid.cursor, grid.saved_cursor);
        let colors = (0..269).map(|i| term.colors()[i]).collect();
        (cells, cursors, *term.mode(), colors, term.cursor_style())
    }

    #[test]
    fn everything_else_is_forwarded_unchanged() {
        // Every Handler call the corrections leave alone must reach `Term`
        // exactly as before: the same sequences through a bare `Term` and
        // through `Vt` give the same grid, cursors, modes, colors, cursor style
        // and events, piece by piece. A forward missing from the list would
        // silently become a no-op and show up here.
        let pieces: &[&str] = &[
            "\x1b]0;title\x07\x1b]2;two\x1b\\\x1b[22t\x1b]2;three\x07\x1b[23t",
            "\x1b[3 q\x1b]50;CursorShape=1\x07",
            "hello wörld 中文 e\u{301}\r\n",
            "\x1b[2;3Hx\x1b[4dy\x1b[2C\x1b[1Dz\x1b[3a",
            "\tA\x08B\rC\x07\x1a",
            "\x1b[1;5H\x1bH\x1b[1;1H\tT\x1b[g\x1b[3g\x1b[?5W\t\x1b[2I\x1b[Z",
            "\x1b7\x1b[5;5H\x1b[1;31mS\x1b8R\x1b[s\x1b[2;2H\x1b[u",
            "\x1b[1;2;3;4;5;7;8;9m\x1b[4:3m\x1b[58:2::1:2:3m\x1b[38;2;10;20;30m\x1b[48;5;100mX\x1b[m",
            "\x1b[4hins\x1b[4l\x1b[4$p\x1b[20$p\x1b=\x1b>",
            "\x1b)0\x0eqqq\x0f\x1b(0x\x1b(B\x1b*0\x1b+B",
            "\x1b]4;1;#123456\x07\x1b]104;1\x07\x1b]4;2;rgb:11/22/33\x07\x1b]10;#abcdef\x07\x1b]110\x07",
            "\x1b]52;c;aGk=\x07\x1b]52;c;?\x07\x1b]52;p;?\x07",
            "\x1b#8\x1b[2J\x1b[H",
            "\x1b[14t\x1b[18t\x1b[5n\x1b[6n\x1b[>c\x1b[?1$p\x1b[?2026$p\x1b]10;?\x07\x1b]4;5;?\x07",
            "\x1b]8;id=1;http://x.test\x07link\x1b]8;;\x07\x1b]22;pointer\x07",
            "\x1b[?u\x1b[>1u\x1b[>5u\x1b[<u\x1b[=3;2u\x1b[?u\x1b[<9u",
            "\x1b[>4;1m\x1b[?4m\x1b[>4;0m\x1b[1;1 k",
            "\x1b[?1h\x1b[?7l\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?1l\x1b[?7h\x1b[?25h",
            "\x1b[?12h\x1b[?12l\x1b[?1042h\x1b[?1007l",
            "line\r\nline\r\nline\r\n\x1b[S\x1b[2T\x1b[3;1H\x1b[L\x1b[2M",
            "\x1b[2;4r\x1b[4;1H\r\n\r\n\x1bE\x1b[2;1H\x1bM\x1bM\x1b[r",
            "abc\x1b[1;2H\x1b[J\x1b[1J\x1b[K\x1b[1K\x1b[2K\x1b[3X\x1b[2@\x1b[P\x1b[3J",
            "\x1b[H\x1b[A\x1b[3B\x1b[2E\x1b[F\x1b[7G\x1b[3e",
            "\x1b[?1049hALT\x1b[?1049l\x1b[?1049h\x1b[2J\x1b[?1049l",
            "\x1b[3;3Hq\x1b[3b\x1bc",
        ];
        let size = TermSize::new(12, 6);
        let config = || Config { kitty_keyboard: true, ..Config::default() };
        let (rec_a, rec_b) = (Recorder::default(), Recorder::default());
        let mut a = Term::new(config(), &size, rec_a.clone());
        let mut b = Term::new(config(), &size, rec_b.clone());
        let (mut pa, mut pb) = (Processor::<StdSyncHandler>::new(), Processor::<StdSyncHandler>::new());
        let mut st = VtState::new(6);
        let (tx, _rx) = std::sync::mpsc::channel();
        for piece in pieces {
            pa.advance(&mut a, piece.as_bytes());
            parse(&mut b, &mut st, &tx, |vt| pb.advance(vt, piece.as_bytes()));
            assert_eq!(observe(&a), observe(&b), "after {piece:?}");
            assert_eq!(*rec_a.0.borrow(), *rec_b.0.borrow(), "events after {piece:?}");
        }
        assert!(rec_a.0.borrow().len() > 20, "the corpus produced events");
    }
}
