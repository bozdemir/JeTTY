//! The vte [`Handler`] JeTTY parses into: alacritty_terminal 0.26's `Term`,
//! with the places where it departs from xterm (VTE and kitty agree with xterm
//! on all of them) corrected on the way through. Every other call is forwarded
//! untouched and `#[inline(always)]`, so vte's performer still calls straight
//! into `Term` (whose `input` stays out of line, as in alacritty).
//!
//! The corrections:
//! * DCH (`CSI Ps P`) with a count past the end of the line erased the cells
//!   LEFT of the cursor too; the count is clamped to the cells that remain.
//! * DCH / ICH / ECH / EL / ED through half of a wide char erase all of it (as
//!   xterm, VTE and kitty do): an orphaned first half drew its glyph over the
//!   next cell, an orphaned second half put the drawn cursor a cell off.
//! * OSC 4 / 10 / 11 / 12 queries answer with the color a program set (pywal,
//!   base16-shell) instead of the theme's, so the background a program
//!   detects (neovim, bat, delta) is the one on screen.
//! * DA1 reports sixel graphics, so lsix, chafa, notcurses and tmux use them.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb, ScpCharPath, ScpUpdateMode,
    StandardCharset, TabulationClearMode,
};
use std::sync::mpsc::Sender;

/// The primary device attributes JeTTY reports: a VT220-class terminal (62)
/// with sixel graphics (4) and ANSI color (22). alacritty answers a bare VT102
/// (`CSI ? 6 c`), which tells sixel-probing programs there are no images.
pub(crate) const DA1_REPLY: &[u8] = b"\x1b[?62;4;22c";

/// `Term` plus what its corrections need, for one `advance` call.
pub(crate) struct Vt<'a, T> {
    pub(crate) term: &'a mut Term<T>,
    /// The PTY reply channel `Term`'s own answers go through, so an answer
    /// written here keeps its place among them.
    pub(crate) reply: &'a Sender<Vec<u8>>,
}

impl<T: EventListener> Vt<'_, T> {
    fn send(&self, bytes: Vec<u8>) {
        let _ = self.reply.send(bytes);
    }

    fn column(&self) -> usize {
        self.term.grid().cursor.point.column.0
    }

    /// Erase the wide char straddling the boundary just left of column `col`
    /// on the cursor row (`col` = its second half), both halves, before an edit
    /// that splits it: an orphaned first half drew its glyph over the next
    /// cell, an orphaned second half put the drawn cursor a cell off.
    fn split_wide_at(&mut self, col: usize) {
        if col == 0 || col >= self.term.columns() {
            return;
        }
        let line = self.term.grid().cursor.point.line;
        let row = &mut self.term.grid_mut()[line];
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
                Handler::$name(&mut *self.term $(, $arg)*)
            }
        )*
    };
}

impl<T: EventListener> Handler for Vt<'_, T> {
    forward! {
        fn set_title(&mut self, title: Option<String>);
        fn set_cursor_style(&mut self, style: Option<CursorStyle>);
        fn set_cursor_shape(&mut self, shape: CursorShape);
        fn input(&mut self, c: char);
        fn goto(&mut self, line: i32, col: usize);
        fn goto_line(&mut self, line: i32);
        fn goto_col(&mut self, col: usize);
        fn move_up(&mut self, lines: usize);
        fn move_down(&mut self, lines: usize);
        fn device_status(&mut self, arg: usize);
        fn move_forward(&mut self, cols: usize);
        fn move_backward(&mut self, cols: usize);
        fn move_down_and_cr(&mut self, lines: usize);
        fn move_up_and_cr(&mut self, lines: usize);
        fn put_tab(&mut self, count: u16);
        fn backspace(&mut self);
        fn carriage_return(&mut self);
        fn linefeed(&mut self);
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
        fn reset_state(&mut self);
        fn reverse_index(&mut self);
        fn terminal_attribute(&mut self, attr: Attr);
        fn set_mode(&mut self, mode: Mode);
        fn unset_mode(&mut self, mode: Mode);
        fn report_mode(&mut self, mode: Mode);
        fn set_private_mode(&mut self, mode: PrivateMode);
        fn unset_private_mode(&mut self, mode: PrivateMode);
        fn report_private_mode(&mut self, mode: PrivateMode);
        fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>);
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

    fn delete_chars(&mut self, count: usize) {
        let x = self.column();
        let count = count.min(self.term.columns() - x);
        self.split_wide_at(x);
        self.split_wide_at(x + count);
        Handler::delete_chars(&mut *self.term, count);
    }

    fn insert_blank(&mut self, count: usize) {
        let (x, cols) = (self.column(), self.term.columns());
        let count = count.min(cols - x);
        self.split_wide_at(x);
        // The cells from `cols - count` on are pushed off the end of the line.
        if cols - count > x {
            self.split_wide_at(cols - count);
        }
        Handler::insert_blank(&mut *self.term, count);
    }

    fn erase_chars(&mut self, count: usize) {
        let x = self.column();
        self.split_wide_at(x);
        self.split_wide_at(x.saturating_add(count));
        Handler::erase_chars(&mut *self.term, count);
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        let x = self.column();
        match mode {
            // `Term` clears nothing to the right while a wrap is pending.
            LineClearMode::Right if !self.term.grid().cursor.input_needs_wrap => self.split_wide_at(x),
            LineClearMode::Left => self.split_wide_at(x + 1),
            _ => {}
        }
        Handler::clear_line(&mut *self.term, mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        let x = self.column();
        match mode {
            ClearMode::Below => self.split_wide_at(x),
            ClearMode::Above => self.split_wide_at(x + 1),
            _ => {}
        }
        Handler::clear_screen(&mut *self.term, mode);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => self.send(DA1_REPLY.to_vec()),
            _ => Handler::identify_terminal(&mut *self.term, intermediate),
        }
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        // A color a program set (OSC 4 / 10 / 11 / 12) wins over the theme's,
        // which `Term` would ask the event listener for. Same reply format.
        match self.term.colors()[index] {
            Some(Rgb { r, g, b }) => self.send(
                format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}").into_bytes(),
            ),
            None => Handler::dynamic_color_sequence(&mut *self.term, prefix, index, terminator),
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

    fn state(term: &Term<Recorder>) -> State {
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
        let (tx, _rx) = std::sync::mpsc::channel();
        for piece in pieces {
            pa.advance(&mut a, piece.as_bytes());
            pb.advance(&mut Vt { term: &mut b, reply: &tx }, piece.as_bytes());
            assert_eq!(state(&a), state(&b), "after {piece:?}");
            assert_eq!(*rec_a.0.borrow(), *rec_b.0.borrow(), "events after {piece:?}");
        }
        assert!(rec_a.0.borrow().len() > 20, "the corpus produced events");
    }
}
