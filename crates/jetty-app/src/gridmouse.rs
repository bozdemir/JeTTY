//! Terminal-grid mouse handling shared by the main window and every detached
//! window.
//!
//! Each window's handler still owns its CHROME — tab bar or title bar,
//! scrollbar, menus, overlays, resize edges. Once a press, release, motion or
//! wheel event belongs to the terminal grid, both windows call the functions
//! here, so mouse reporting, selection, the right/middle-button routing and the
//! wheel behave the same in every window, and a fix lands everywhere at once.
//!
//! Nothing here touches the clipboard, a PTY writer or a window: bytes for the
//! program are appended to [`Grid::out`], and every effect the caller must
//! perform (copy, paste, open the menu or a link, show a hint, repaint) comes
//! back in the returned outcome. That keeps the routing testable without a
//! window, a PTY or a clipboard.

use std::time::{Duration, Instant};

use jetty_core::Terminal;
use winit::event::{MouseButton, MouseScrollDelta};
use winit::keyboard::ModifiersState;

use crate::input::{
    self, ClickTracker, MouseAct, MouseBtn, MouseEncoding, MouseReport, MouseTracking,
    ScrollAccumulator,
};

/// Where a window's terminal grid sits (physical px) — the pixel → cell half
/// of `jetty_render::grid_geom`'s convention (cell (0, 0) at `(left, top)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridGeom {
    /// x of the grid's first column (the left padding).
    pub left: f32,
    /// y of the grid's first row: the band top plus the top padding.
    pub top: f32,
    /// y where the grid BAND starts: the bottom edge of a top bar, else 0. The
    /// padding between it and `top` belongs to the grid — a pointer there is on
    /// the grid and maps to row 0.
    pub band_top: f32,
    /// y just past the grid band — above a bottom tab bar or the status strip.
    /// A pointer in `band_top..bottom` is ON the grid.
    pub bottom: f32,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl GridGeom {
    fn usable(&self) -> bool {
        self.cell_w > 0.0 && self.cell_h > 0.0
    }

    /// Whether window y `y` lies on the grid band (padding included).
    pub fn contains_y(&self, y: f32) -> bool {
        y >= self.band_top && y < self.bottom
    }

    /// The 1-based cell under window point `(x, y)`, clamped to the
    /// `cols` × `rows` grid — mouse-report coordinates. The padding and the
    /// scrollbar gutter map to the nearest edge cell, never outside the grid.
    pub fn report_cell(&self, x: f32, y: f32, cols: usize, rows: usize) -> (usize, usize) {
        let (gx, gy) = ((x - self.left).max(0.0), (y - self.top).max(0.0));
        input::cell_at_clamped(gx, gy, self.cell_w, self.cell_h, cols, rows)
    }

    /// The 0-based viewport cell under window point `(x, y)`, clamped to the
    /// grid, and whether the pointer is in its left half (selection endpoints,
    /// link hover). Left of column 0 counts as its left half, right of the
    /// last column as that column's right half.
    pub fn select_cell(&self, x: f32, y: f32, cols: usize, rows: usize) -> (usize, usize, bool) {
        input::cell_at_0_side(x - self.left, (y - self.top).max(0.0), self.cell_w, self.cell_h, cols, rows)
    }
}

/// The mouse-tracking mode the terminal's program asked for.
pub(crate) fn tracking(term: &Terminal) -> MouseTracking {
    MouseTracking::from_modes(term.mouse_x10(), term.mouse_mode(), term.mouse_drag(), term.mouse_motion())
}

/// The tracking mode a NEW gesture — a press, the wheel, a hover — is routed
/// by: `Off` while the view is scrolled back into history, which the program
/// can't see. A click there reached fzf's Ctrl+R (a primary-screen tracker) as
/// a click on whatever live row sat under the pointer, and the wheel went to
/// fzf instead of bringing the view back down. JeTTY keeps the mouse until the
/// view is live again; a press the program already holds still ends there.
pub(crate) fn gesture_tracking(term: &Terminal) -> MouseTracking {
    if term.scroll_offset() > 0 {
        MouseTracking::Off
    } else {
        tracking(term)
    }
}

/// The report encoding the terminal's program asked for.
pub(crate) fn encoding(term: &Terminal) -> MouseEncoding {
    MouseEncoding::from_modes(term.mouse_utf8(), term.mouse_sgr(), term.mouse_urxvt())
}

/// A button's bit in [`GridMouse::held`] (0 for the wheel, which has no
/// release).
fn held_bit(b: MouseBtn) -> u8 {
    match b {
        MouseBtn::Left => 1,
        MouseBtn::Middle => 2,
        MouseBtn::Right => 4,
        MouseBtn::Back => 8,
        MouseBtn::Forward => 16,
        _ => 0,
    }
}

/// The button a motion report names while buttons are held — xterm names the
/// first of left, middle, right.
fn motion_button(held: u8) -> Option<MouseBtn> {
    if held & 1 != 0 {
        Some(MouseBtn::Left)
    } else if held & 2 != 0 {
        Some(MouseBtn::Middle)
    } else if held & 4 != 0 {
        Some(MouseBtn::Right)
    } else {
        None
    }
}

/// One edge auto-scroll step per this interval while a selection drag sits
/// above or below the grid's rows.
pub(crate) const AUTOSCROLL_TICK: Duration = Duration::from_millis(50);
/// Lines per auto-scroll step at the farthest pointer distance.
const AUTOSCROLL_MAX_LINES: i32 = 8;
/// Pointer travel (px) after which a left press that went to the program counts
/// as a drag — an attempt to select.
const DRAG_HINT_PX: f64 = 8.0;
/// Wheel reports per event at most (a touchpad fling must not flood the PTY).
const WHEEL_MAX_REPORTS: i32 = 8;
/// Arrow keys per wheel event at most (alternate scroll).
const WHEEL_MAX_ARROWS: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq)]
struct AutoScroll {
    /// Lines per step: positive scrolls up into history (pointer above the grid).
    lines: i32,
    next: Instant,
}

/// Edge auto-scroll speed for a selection drag with the pointer at window y
/// `y`: `None` while it is over the grid's `rows`; above them `+n` lines per
/// step (into history), below them `-n` — one more line per cell height of
/// distance, capped.
pub(crate) fn autoscroll_lines(geom: GridGeom, rows: usize, y: f32) -> Option<i32> {
    if !geom.usable() {
        return None;
    }
    let rows_bottom = (geom.top + rows as f32 * geom.cell_h).min(geom.bottom);
    let speed = |dist: f32| (1 + (dist / geom.cell_h) as i32).min(AUTOSCROLL_MAX_LINES);
    if y < geom.top {
        Some(speed(geom.top - y))
    } else if y >= rows_bottom {
        Some(-speed(y - rows_bottom))
    } else {
        None
    }
}

/// One window's grid mouse state (the main window has one, each detached
/// window its own).
#[derive(Debug, Default)]
pub(crate) struct GridMouse {
    /// Buttons whose press went to the program ([`held_bit`]s): their releases
    /// go too, and a 1002 motion report names the first one held.
    held: u8,
    /// Where a left press that went to the program started — a drag from there
    /// suggests the user wanted to select ([`Release::Program`]).
    grab_press: Option<(f64, f64)>,
    /// The cell of the last report: motion is reported once per cell change.
    last_cell: Option<(usize, usize)>,
    clicks: ClickTracker,
    autoscroll: Option<AutoScroll>,
    /// Horizontal wheel remainder (each window keeps its own vertical one).
    hwheel: ScrollAccumulator,
}

impl GridMouse {
    /// Forget every gesture in flight — its release can no longer arrive
    /// (window hidden, focus lost).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// When the next edge auto-scroll step is due, while a selection drag sits
    /// outside the grid.
    pub fn autoscroll_due(&self) -> Option<Instant> {
        self.autoscroll.map(|a| a.next)
    }
}

/// One window's grid, borrowed for one mouse event.
pub(crate) struct Grid<'a> {
    pub term: &'a mut Terminal,
    pub mouse: &'a mut GridMouse,
    /// The window's "local selection drag in progress" flag.
    pub selecting: &'a mut bool,
    pub geom: GridGeom,
    /// The pointer in window coordinates (physical px).
    pub pointer: (f64, f64),
    pub mods: ModifiersState,
    /// Bytes for the program; the caller writes them to the tab's PTY.
    pub out: &'a mut Vec<u8>,
}

impl Grid<'_> {
    fn pointer_f32(&self) -> (f32, f32) {
        (self.pointer.0 as f32, self.pointer.1 as f32)
    }

    /// The 1-based cell under the pointer, clamped to the grid, on the
    /// program's live screen (report coordinates): scrolled back `n` lines,
    /// viewport row `r` shows live row `r - n`, and history clamps to row 1.
    fn report_cell(&self) -> (usize, usize) {
        let (x, y) = self.pointer_f32();
        let (col, row) = self.geom.report_cell(x, y, self.term.cols(), self.term.rows());
        (col, row.saturating_sub(self.term.scroll_offset()).max(1))
    }

    /// The 0-based viewport cell under the pointer (clamped) and whether the
    /// pointer is in its left half (selection endpoints).
    fn select_cell(&self) -> (usize, usize, bool) {
        let (x, y) = self.pointer_f32();
        self.geom.select_cell(x, y, self.term.cols(), self.term.rows())
    }

    /// Report `button` / `act` at the pointer with the held modifiers, when the
    /// program's tracking mode carries it. Returns whether it was sent.
    fn report(&mut self, button: Option<MouseBtn>, act: MouseAct) -> bool {
        let tracking = tracking(self.term);
        let r = MouseReport {
            button,
            act,
            shift: self.mods.shift_key(),
            alt: self.mods.alt_key(),
            ctrl: self.mods.control_key(),
        };
        if !input::mouse_reportable(tracking, &r) {
            return false;
        }
        let (col, row) = self.report_cell();
        self.out.extend(input::encode_mouse_report(&r, col, row, encoding(self.term), tracking));
        self.mouse.last_cell = Some((col, row));
        true
    }
}

/// Who answers a press on the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The program: it turned mouse tracking on and Shift isn't overriding it.
    Program,
    /// JeTTY: left selects, middle pastes, right opens the menu.
    Local,
    /// Neither — a button the tracking mode can't carry, or one JeTTY has no
    /// use for.
    Ignore,
}

/// Route a press of `btn`. Shift always keeps the mouse for JeTTY — the
/// terminal convention for selecting inside mouse-grabbing programs. A right
/// click while JeTTY holds a selection is about that selection (Copy, Run in New
/// Tab): the program never sees JeTTY's selection, so the menu wins.
pub(crate) fn route_press(
    tracking: MouseTracking,
    shift: bool,
    btn: MouseBtn,
    has_selection: bool,
) -> Route {
    if tracking == MouseTracking::Off || shift || (btn == MouseBtn::Right && has_selection) {
        return match btn {
            MouseBtn::Left | MouseBtn::Middle | MouseBtn::Right => Route::Local,
            _ => Route::Ignore,
        };
    }
    if input::mouse_reportable(tracking, &MouseReport::new(Some(btn), MouseAct::Press)) {
        Route::Program
    } else {
        Route::Ignore
    }
}

/// What the caller does after [`press`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Reported to the program (bytes in `out`).
    Reported,
    /// A local selection started: repaint.
    Selecting,
    /// Link-modifier click on a link: open it.
    OpenLink(String),
    /// JeTTY's right click: open this window's context menu.
    Menu,
    /// JeTTY's middle click: paste the PRIMARY selection (the clipboard under
    /// `copy_on_select = "clipboard"` — see `CopyOnSelect::middle_click_reads_clipboard`).
    PastePrimary,
    /// Nothing to do.
    Ignored,
}

/// A press of `button` that the window's chrome did not take. `link_mod` is
/// whether the link modifier (Ctrl; Cmd too on macOS) is held. Only a press on
/// the grid band counts: one on the status strip below it (the perf HUD) is
/// chrome, and clamping it onto the last row clicked htop's F9 Kill / F10 Quit
/// bar or tmux's status line. Scrolled back, the press is JeTTY's
/// ([`gesture_tracking`]).
pub(crate) fn press(g: &mut Grid, button: MouseButton, link_mod: bool, now: Instant) -> Press {
    let Some(btn) = MouseBtn::from_winit(button) else { return Press::Ignored };
    if !g.geom.usable() || !g.geom.contains_y(g.pointer.1 as f32) {
        return Press::Ignored;
    }
    let shift = g.mods.shift_key();
    // A link-modifier click on a link opens it (Shift still forces a selection).
    if btn == MouseBtn::Left && link_mod && !shift {
        let (line, col, _) = g.select_cell();
        if let Some(hit) = g.term.link_at(line, col) {
            return Press::OpenLink(hit.uri);
        }
    }
    let has_selection =
        btn == MouseBtn::Right && g.term.selection_text().is_some_and(|t| !t.is_empty());
    match route_press(gesture_tracking(g.term), shift, btn, has_selection) {
        Route::Program => {
            if !g.report(Some(btn), MouseAct::Press) {
                return Press::Ignored;
            }
            g.mouse.held |= held_bit(btn);
            if btn == MouseBtn::Left {
                g.mouse.grab_press = Some(g.pointer);
            }
            g.mouse.clicks.reset();
            Press::Reported
        }
        Route::Local => match btn {
            MouseBtn::Left => {
                start_selection(g, now);
                Press::Selecting
            }
            MouseBtn::Middle => Press::PastePrimary,
            MouseBtn::Right => Press::Menu,
            _ => Press::Ignored,
        },
        Route::Ignore => Press::Ignored,
    }
}

/// Start a local selection at the pointer: a single click selects by cell, a
/// double click by word, a triple click by line — and the drag that follows
/// extends in that unit.
fn start_selection(g: &mut Grid, now: Instant) {
    let (x, y) = g.pointer_f32();
    let count = g.mouse.clicks.press(MouseButton::Left, now, x, y);
    let (line, col, left_half) = g.select_cell();
    g.term.selection_clear();
    match count {
        2 => g.term.selection_start_semantic(line, col),
        3 => g.term.selection_start_lines(line),
        _ => g.term.selection_start(line, col, left_half),
    }
    *g.selecting = true;
    g.mouse.autoscroll = None;
}

/// What the caller does after [`release`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Release {
    /// A selection drag ended with text: copy it to the PRIMARY selection
    /// (copy-on-select) and repaint.
    Copy(String),
    /// A selection drag ended empty (a plain click): the highlight is cleared —
    /// repaint.
    Cleared,
    /// The release of a press that went to the program (reported if its
    /// tracking mode still wants it). `dragged`: a left press moved before
    /// release — the user was probably trying to select (teach Shift+drag).
    Program { dragged: bool },
    /// Nothing to do.
    Ignored,
}

/// A button release in the window.
pub(crate) fn release(g: &mut Grid, button: MouseButton) -> Release {
    let Some(btn) = MouseBtn::from_winit(button) else { return Release::Ignored };
    if btn == MouseBtn::Left && *g.selecting {
        *g.selecting = false;
        g.mouse.autoscroll = None;
        return match g.term.selection_text() {
            Some(t) if !t.is_empty() => Release::Copy(t),
            _ => {
                g.term.selection_clear();
                Release::Cleared
            }
        };
    }
    let bit = held_bit(btn);
    if bit == 0 || g.mouse.held & bit == 0 {
        return Release::Ignored;
    }
    g.mouse.held &= !bit;
    let dragged = btn == MouseBtn::Left
        && g.mouse
            .grab_press
            .take()
            .is_some_and(|(px, py)| (g.pointer.0 - px).hypot(g.pointer.1 - py) > DRAG_HINT_PX);
    if g.geom.usable() {
        g.report(Some(btn), MouseAct::Release);
    }
    Release::Program { dragged }
}

/// What the caller does after [`motion`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Motion {
    /// The selection changed: repaint.
    pub paint: bool,
}

/// The pointer moved: extend a local selection drag (arming the edge
/// auto-scroll while the pointer is past the grid's top or bottom), or report
/// the motion to a program that tracks it — 1002 while one of its buttons is
/// held, 1003 always (but not over history, [`gesture_tracking`]) — once per
/// cell.
pub(crate) fn motion(g: &mut Grid, now: Instant) -> Motion {
    if !g.geom.usable() {
        return Motion::default();
    }
    if *g.selecting {
        let (line, col, left_half) = g.select_cell();
        g.term.selection_update(line, col, left_half);
        let rows = g.term.rows();
        let next = g.mouse.autoscroll.map_or(now + AUTOSCROLL_TICK, |a| a.next);
        g.mouse.autoscroll = autoscroll_lines(g.geom, rows, g.pointer.1 as f32)
            .map(|lines| AutoScroll { lines, next });
        return Motion { paint: true };
    }
    if g.mouse.held == 0 && gesture_tracking(g.term) == MouseTracking::Off {
        return Motion::default();
    }
    let cell = g.report_cell();
    if g.mouse.last_cell != Some(cell) {
        g.report(motion_button(g.mouse.held), MouseAct::Motion);
        g.mouse.last_cell = Some(cell);
    }
    Motion::default()
}

/// Run a due edge auto-scroll step: scroll one step toward the pointer and
/// extend the selection to the grid edge under it. Returns whether the view
/// moved (repaint). Further steps stay scheduled while the drag remains
/// outside the grid ([`GridMouse::autoscroll_due`]).
pub(crate) fn autoscroll_step(g: &mut Grid, now: Instant) -> bool {
    let Some(a) = g.mouse.autoscroll else { return false };
    if !*g.selecting || !g.geom.usable() {
        g.mouse.autoscroll = None;
        return false;
    }
    if now < a.next {
        return false;
    }
    g.mouse.autoscroll = Some(AutoScroll { next: now + AUTOSCROLL_TICK, ..a });
    let before = g.term.scroll_offset();
    g.term.scroll_lines(a.lines);
    // The pointer is outside the rows, so its clamped cell IS the edge row.
    let (line, col, left_half) = g.select_cell();
    g.term.selection_update(line, col, left_half);
    g.term.scroll_offset() != before
}

/// What the caller does after [`wheel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wheel {
    /// Reported to the program (bytes in `out`).
    Reported,
    /// Arrow keys for an alternate-screen pager (bytes in `out`) — keyboard
    /// input, so the caller cancels a staged run-selection inject.
    Arrows,
    /// The host scrollback moved: repaint and revalidate the link hover.
    Scrolled,
    /// Nothing (a fraction of a line so far).
    None,
}

/// One wheel event. `vertical` is the window's line accumulator;
/// `over_scrollbar` keeps the wheel on the host scrollback (the scrollbar is
/// JeTTY's). Shift also always scrolls the host scrollback — the escape hatch
/// out of a mouse-grabbing program — and so does a view scrolled back
/// ([`gesture_tracking`]): the wheel brings it back down first.
pub(crate) fn wheel(
    g: &mut Grid,
    delta: MouseScrollDelta,
    over_scrollbar: bool,
    vertical: &mut ScrollAccumulator,
) -> Wheel {
    let lines = vertical.add(input::wheel_lines(delta, g.geom.cell_h));
    let cols = g.mouse.hwheel.add(input::wheel_columns(delta, g.geom.cell_w));
    let tracking = gesture_tracking(g.term);
    let shift = g.mods.shift_key();
    let wheel_report = MouseReport::new(Some(MouseBtn::WheelUp), MouseAct::Press);
    if !shift && !over_scrollbar && input::mouse_reportable(tracking, &wheel_report) {
        if lines == 0 && cols == 0 {
            return Wheel::None;
        }
        // One report per ~3 lines (a notch), bounded.
        let notches = |n: i32| ((n.abs() + 2) / 3).clamp(1, WHEEL_MAX_REPORTS);
        if lines != 0 {
            let b = if lines > 0 { MouseBtn::WheelUp } else { MouseBtn::WheelDown };
            for _ in 0..notches(lines) {
                g.report(Some(b), MouseAct::Press);
            }
        }
        if cols != 0 {
            let b = if cols > 0 { MouseBtn::WheelLeft } else { MouseBtn::WheelRight };
            for _ in 0..notches(cols) {
                g.report(Some(b), MouseAct::Press);
            }
        }
        return Wheel::Reported;
    }
    // Nothing horizontal to scroll on the host: never let a remainder carry
    // into a later program report.
    g.mouse.hwheel.reset();
    if lines == 0 {
        return Wheel::None;
    }
    // A mouse-tracking program + Shift goes straight to the host scrollback.
    let shift_escape = shift && tracking != MouseTracking::Off;
    if !shift_escape && !over_scrollbar && g.term.alt_screen() && g.term.alternate_scroll() {
        let seq = input::arrow_scroll_bytes(lines > 0, g.term.app_cursor_keys());
        for _ in 0..lines.unsigned_abs().clamp(1, WHEEL_MAX_ARROWS) {
            g.out.extend_from_slice(&seq);
        }
        return Wheel::Arrows;
    }
    g.term.scroll_lines(lines);
    Wheel::Scrolled
}

/// What dropping a file onto the terminal types: its path — single-quoted for
/// the shell unless it holds only plainly safe characters — plus a space, so
/// several dropped files (one event each) become separate arguments.
pub(crate) fn dropped_path_text(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let plain = !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+,:@%".contains(&b));
    let mut out = if plain { s.into_owned() } else { format!("'{}'", s.replace('\'', r"'\''")) };
    out.push(' ');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: f32 = 10.0;
    /// An 8×4 grid starting 30 px down (a tab bar above it), 4 rows tall.
    const GEOM: GridGeom =
        GridGeom { left: 0.0, top: 30.0, band_top: 30.0, bottom: 70.0, cell_w: CELL, cell_h: CELL };

    struct Win {
        term: Terminal,
        mouse: GridMouse,
        selecting: bool,
        out: Vec<u8>,
    }

    impl Win {
        fn new(setup: &[u8]) -> Self {
            let mut term = Terminal::new(8, 4);
            term.feed(setup);
            Win { term, mouse: GridMouse::default(), selecting: false, out: Vec::new() }
        }

        /// Run `f` with the pointer over 0-based cell (col, row) and `mods`;
        /// returns its result and the bytes it produced for the program.
        fn at<R>(
            &mut self,
            col: f32,
            row: f32,
            mods: ModifiersState,
            f: impl FnOnce(&mut Grid) -> R,
        ) -> (R, String) {
            self.out.clear();
            let pointer = ((col * CELL + 2.0) as f64, (GEOM.top + row * CELL + 2.0) as f64);
            let r = f(&mut Grid {
                term: &mut self.term,
                mouse: &mut self.mouse,
                selecting: &mut self.selecting,
                geom: GEOM,
                pointer,
                mods,
                out: &mut self.out,
            });
            (r, String::from_utf8_lossy(&self.out).into_owned())
        }
    }

    const NONE: ModifiersState = ModifiersState::empty();
    const SGR_CLICKS: &[u8] = b"\x1b[?1000h\x1b[?1006h";

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn routing_gives_the_program_its_buttons_unless_shift_or_a_selection() {
        use MouseBtn::*;
        use MouseTracking::*;
        assert_eq!(route_press(Off, false, Left, false), Route::Local);
        assert_eq!(route_press(Off, false, Middle, false), Route::Local);
        assert_eq!(route_press(Off, false, Right, false), Route::Local);
        assert_eq!(route_press(Off, false, Back, false), Route::Ignore);
        for t in [X10, Normal, ButtonEvent, AnyEvent] {
            assert_eq!(route_press(t, false, Left, false), Route::Program);
            assert_eq!(route_press(t, false, Middle, false), Route::Program);
            assert_eq!(route_press(t, false, Right, false), Route::Program);
            // Shift keeps every button for JeTTY.
            assert_eq!(route_press(t, true, Left, false), Route::Local);
            assert_eq!(route_press(t, true, Right, false), Route::Local);
            // A right click on JeTTY's own selection opens JeTTY's menu.
            assert_eq!(route_press(t, false, Right, true), Route::Local);
        }
        assert_eq!(route_press(Normal, false, Back, false), Route::Program);
        assert_eq!(route_press(X10, false, Back, false), Route::Ignore, "X10 carries L/M/R only");
    }

    #[test]
    fn left_middle_right_press_and_release_reach_a_tracking_program() {
        let mut w = Win::new(SGR_CLICKS);
        for (button, code) in [(MouseButton::Left, 0), (MouseButton::Middle, 1), (MouseButton::Right, 2)] {
            let (p, bytes) = w.at(2.0, 1.0, NONE, |g| press(g, button, false, t0()));
            assert_eq!(p, Press::Reported);
            assert_eq!(bytes, format!("\x1b[<{code};3;2M"));
            let (r, bytes) = w.at(2.0, 1.0, NONE, |g| release(g, button));
            assert_eq!(r, Release::Program { dragged: false });
            assert_eq!(bytes, format!("\x1b[<{code};3;2m"));
        }
        // A release whose press never went to the program is not reported.
        let (r, bytes) = w.at(2.0, 1.0, NONE, |g| release(g, MouseButton::Right));
        assert_eq!((r, bytes.as_str()), (Release::Ignored, ""));
    }

    #[test]
    fn presses_on_the_status_strip_below_the_grid_band_do_nothing() {
        // GEOM's band ends at y = 70 (4 rows); the status strip (perf HUD) sits
        // below it. A press there used to be clamped onto the LAST row: a click
        // on the HUD reached htop's function-key bar (F9 Kill / F10 Quit) or
        // tmux's status line, and started a selection on a plain prompt.
        for setup in [&b"\x1b[?1000h\x1b[?1006h"[..], b"\x1b[?1003h", b""] {
            let mut w = Win::new(setup);
            for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right, MouseButton::Back] {
                let (p, bytes) = w.at(3.0, 4.2, NONE, |g| press(g, button, false, t0()));
                assert_eq!((p, bytes.as_str()), (Press::Ignored, ""), "{button:?} after {setup:?}");
                assert!(!w.selecting);
                // No phantom release either: the press was never the program's.
                let (r, bytes) = w.at(3.0, 4.2, NONE, |g| release(g, button));
                assert_eq!(bytes, "", "{button:?} release after {setup:?}");
                assert_ne!(r, Release::Copy(String::new()));
            }
        }
        // The last row itself (just above the band's end) still reports.
        let mut w = Win::new(SGR_CLICKS);
        let (p, bytes) = w.at(3.0, 3.6, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!((p, bytes.as_str()), (Press::Reported, "\x1b[<0;4;4M"));
    }

    #[test]
    fn modifier_bits_and_every_encoding() {
        let ctrl = ModifiersState::CONTROL;
        let alt = ModifiersState::ALT;
        let mut w = Win::new(SGR_CLICKS);
        assert_eq!(w.at(0.0, 0.0, ctrl, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[<16;1;1M");
        let mut w = Win::new(SGR_CLICKS);
        assert_eq!(w.at(0.0, 0.0, alt, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[<8;1;1M");
        let mut w = Win::new(b"\x1b[?1000h\x1b[?1015h");
        assert_eq!(w.at(1.0, 2.0, NONE, |g| press(g, MouseButton::Right, false, t0())).1, "\x1b[34;2;3M");
        let mut w = Win::new(b"\x1b[?1000h");
        assert_eq!(w.at(1.0, 2.0, NONE, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[M\x20\x22\x23");
        assert_eq!(w.at(1.0, 2.0, NONE, |g| release(g, MouseButton::Left)).1, "\x1b[M\x23\x22\x23");
        // X10 (mode 9): presses only, no modifier bits, no release.
        let mut w = Win::new(b"\x1b[?9h");
        assert_eq!(w.at(0.0, 0.0, ctrl, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[M\x20\x21\x21");
        assert_eq!(w.at(0.0, 0.0, NONE, |g| release(g, MouseButton::Left)).1, "");
    }

    #[test]
    fn shift_keeps_the_mouse_for_jetty() {
        let shift = ModifiersState::SHIFT;
        let mut w = Win::new(SGR_CLICKS);
        let (p, bytes) = w.at(1.0, 1.0, shift, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!((p, bytes.as_str()), (Press::Selecting, ""));
        assert!(w.selecting);
        assert_eq!(w.at(1.0, 1.0, shift, |g| press(g, MouseButton::Middle, false, t0())).0, Press::PastePrimary);
        assert_eq!(w.at(1.0, 1.0, shift, |g| press(g, MouseButton::Right, false, t0())).0, Press::Menu);
    }

    #[test]
    fn right_click_on_a_jetty_selection_opens_the_menu_even_in_a_tracking_program() {
        let mut w = Win::new(b"hello\x1b[?1000h\x1b[?1006h");
        w.term.selection_start(0, 0, true);
        w.term.selection_update(0, 4, false);
        let (p, bytes) = w.at(2.0, 0.0, NONE, |g| press(g, MouseButton::Right, false, t0()));
        assert_eq!((p, bytes.as_str()), (Press::Menu, ""));
    }

    #[test]
    fn button_event_motion_only_while_held_any_event_always_once_per_cell() {
        let mut w = Win::new(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(w.at(1.0, 1.0, NONE, |g| motion(g, t0())).1, "", "1002: no button, no report");
        w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!(w.at(1.0, 1.0, NONE, |g| motion(g, t0())).1, "", "same cell as the press");
        assert_eq!(w.at(2.0, 1.0, NONE, |g| motion(g, t0())).1, "\x1b[<32;3;2M");
        assert_eq!(w.at(2.0, 1.0, NONE, |g| motion(g, t0())).1, "", "once per cell");
        w.at(2.0, 1.0, NONE, |g| release(g, MouseButton::Left));
        assert_eq!(w.at(3.0, 1.0, NONE, |g| motion(g, t0())).1, "", "released: 1002 stops");

        let mut w = Win::new(b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(w.at(1.0, 1.0, NONE, |g| motion(g, t0())).1, "\x1b[<35;2;2M", "1003: no button = 3+32");
        w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Right, false, t0()));
        assert_eq!(w.at(4.0, 2.0, NONE, |g| motion(g, t0())).1, "\x1b[<34;5;3M", "right held = 2+32");
    }

    #[test]
    fn double_and_triple_click_select_a_word_and_a_line() {
        let mut w = Win::new(b"ab cde fg");
        let t = Instant::now();
        let click = |w: &mut Win, at: Instant| {
            w.at(4.0, 0.0, NONE, |g| press(g, MouseButton::Left, false, at));
            w.at(4.0, 0.0, NONE, |g| release(g, MouseButton::Left)).0
        };
        assert_eq!(click(&mut w, t), Release::Cleared, "a single click selects nothing");
        assert_eq!(click(&mut w, t + Duration::from_millis(150)), Release::Copy("cde".into()));
        // The whole logical line — wrapped across both rows of this 8-column
        // grid — with its newline (alacritty's line selection, as copy-mode `V`).
        assert_eq!(click(&mut w, t + Duration::from_millis(300)), Release::Copy("ab cde fg\n".into()));
        // Too slow: a new single click.
        assert_eq!(click(&mut w, t + Duration::from_secs(2)), Release::Cleared);
    }

    #[test]
    fn a_drag_selects_and_release_hands_back_the_text_to_copy() {
        let mut w = Win::new(b"hello world");
        w.at(0.0, 0.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        let (m, _) = w.at(4.6, 0.0, NONE, |g| motion(g, t0()));
        assert!(m.paint);
        assert_eq!(w.at(4.6, 0.0, NONE, |g| release(g, MouseButton::Left)).0, Release::Copy("hello".into()));
        assert!(!w.selecting);
    }

    #[test]
    fn a_drag_that_went_to_the_program_asks_for_the_shift_hint() {
        let mut w = Win::new(SGR_CLICKS);
        w.at(0.0, 0.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!(w.at(3.0, 2.0, NONE, |g| release(g, MouseButton::Left)).0, Release::Program { dragged: true });
        w.at(0.0, 0.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!(w.at(0.0, 0.0, NONE, |g| release(g, MouseButton::Left)).0, Release::Program { dragged: false });
    }

    #[test]
    fn link_click_opens_only_on_the_grid_with_the_modifier() {
        let mut w = Win::new(b"https://example.com/x");
        let (p, _) = w.at(3.0, 0.0, NONE, |g| press(g, MouseButton::Left, true, t0()));
        assert_eq!(p, Press::OpenLink("https://example.com/x".into()));
        let (p, _) = w.at(3.0, 0.0, ModifiersState::SHIFT, |g| press(g, MouseButton::Left, true, t0()));
        assert_eq!(p, Press::Selecting, "Shift still selects");
        let (p, _) = w.at(3.0, 0.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!(p, Press::Selecting, "no modifier, no link");
    }

    #[test]
    fn wheel_reports_shift_and_the_scrollbar_keep_it_on_the_host() {
        let line = |n: f32| MouseScrollDelta::LineDelta(0.0, n);
        let mut acc = ScrollAccumulator::new();
        let mut w = Win::new(b"\x1b[?1000h\x1b[?1006h");
        let (r, bytes) = w.at(0.0, 0.0, NONE, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Reported, "\x1b[<64;1;1M"));
        let (_, bytes) = w.at(0.0, 0.0, ModifiersState::CONTROL, |g| wheel(g, line(-1.0), false, &mut acc));
        assert_eq!(bytes, "\x1b[<81;1;1M", "wheel down (65) + ctrl (16)");
        let (r, bytes) = w.at(0.0, 0.0, NONE, |g| wheel(g, MouseScrollDelta::LineDelta(1.0, 0.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Reported, "\x1b[<66;1;1M"), "horizontal: wheel-left");
        let (r, bytes) = w.at(0.0, 0.0, ModifiersState::SHIFT, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Scrolled, ""), "Shift: host scrollback");
        let (r, bytes) = w.at(0.0, 0.0, NONE, |g| wheel(g, line(1.0), true, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Scrolled, ""), "over the scrollbar: host scrollback");
    }

    #[test]
    fn wheel_on_an_alt_screen_pager_sends_arrows_unless_shift_escapes_tracking() {
        let line = |n: f32| MouseScrollDelta::LineDelta(0.0, n);
        let mut acc = ScrollAccumulator::new();
        let mut w = Win::new(b"\x1b[?1049h");
        let (r, bytes) = w.at(0.0, 0.0, NONE, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Arrows, "\x1b[A\x1b[A\x1b[A"));
        let (r, _) = w.at(0.0, 0.0, ModifiersState::SHIFT, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!(r, Wheel::Arrows, "no tracking: Shift changes nothing");
        let mut w = Win::new(b"\x1b[?1049h\x1b[?1000h");
        let (r, bytes) = w.at(0.0, 0.0, ModifiersState::SHIFT, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Scrolled, ""), "tracking + Shift: host scrollback");
    }

    /// A padded 80×24 grid below a 36-px tab bar at DPI `scale` (MesloLGS-like
    /// 9.6 × 21 px cells at 1×), padding 8 × 4 logical — the default config.
    fn padded_geom(scale: f32) -> (GridGeom, usize, usize) {
        let (cols, rows) = (80usize, 24usize);
        let (cell_w, cell_h) = (9.6 * scale, 21.0 * scale);
        let (pad_x, pad_y) = (jetty_render::padding_px(8.0, scale), jetty_render::padding_px(4.0, scale));
        let band_top = (36.0 * scale).round();
        let top = band_top + pad_y;
        // The band ends a little below the last row + the bottom padding (the
        // leftover of a non-integral fit), above the status strip.
        let bottom = top + rows as f32 * cell_h + pad_y + 3.0 * scale;
        (GridGeom { left: pad_x, top, band_top, bottom, cell_w, cell_h }, cols, rows)
    }

    #[test]
    fn padded_grid_maps_corners_and_paddings_to_edge_cells_at_1x_and_2x() {
        for scale in [1.0f32, 2.0] {
            let (g, cols, rows) = padded_geom(scale);
            let right = g.left + cols as f32 * g.cell_w; // just past the last column
            let below = g.top + rows as f32 * g.cell_h; // just past the last row
            // The four corner cells, a pixel inside each.
            assert_eq!(g.report_cell(g.left + 1.0, g.top + 1.0, cols, rows), (1, 1), "@{scale}× top-left");
            assert_eq!(g.report_cell(right - 1.0, g.top + 1.0, cols, rows), (cols, 1), "@{scale}× top-right");
            assert_eq!(g.report_cell(g.left + 1.0, below - 1.0, cols, rows), (1, rows), "@{scale}× bottom-left");
            assert_eq!(g.report_cell(right - 1.0, below - 1.0, cols, rows), (cols, rows), "@{scale}× bottom-right");
            assert_eq!(g.select_cell(g.left + 1.0, g.top + 1.0, cols, rows), (0, 0, true));
            assert_eq!(g.select_cell(right - 1.0, below - 1.0, cols, rows), (rows - 1, cols - 1, false));
            // Column/row edges land exactly on the cell boundaries.
            assert_eq!(g.report_cell(g.left + g.cell_w, g.top + g.cell_h, cols, rows), (2, 2), "@{scale}×");
            assert_eq!(g.report_cell(g.left + g.cell_w - 0.5, g.top + g.cell_h - 0.5, cols, rows), (1, 1));
            // The left padding is column 1 (its left half for a selection).
            for x in [0.0, 1.0, g.left - 0.5] {
                assert_eq!(g.report_cell(x, g.top + 1.0, cols, rows), (1, 1), "@{scale}× x={x}");
                assert_eq!(g.select_cell(x, g.top + 1.0, cols, rows), (0, 0, true), "@{scale}× x={x}");
            }
            // The top padding is ON the grid and is row 1; the bar above is not.
            for y in [g.band_top, g.band_top + 1.0, g.top - 0.5] {
                assert!(g.contains_y(y), "@{scale}× y={y} is grid padding");
                assert_eq!(g.report_cell(g.left + 1.0, y, cols, rows), (1, 1), "@{scale}× y={y}");
                assert_eq!(g.select_cell(g.left + 1.0, y, cols, rows), (0, 0, true));
            }
            assert!(!g.contains_y(g.band_top - 1.0), "@{scale}× the tab bar is chrome");
            // The right padding / scrollbar gutter is the last column (its right
            // half), the bottom padding the last row.
            for x in [right, right + 3.0, right + 18.0 * scale] {
                assert_eq!(g.report_cell(x, g.top + 1.0, cols, rows), (cols, 1), "@{scale}× x={x}");
                assert_eq!(g.select_cell(x, g.top + 1.0, cols, rows), (0, cols - 1, false), "@{scale}× x={x}");
            }
            for y in [below, below + 1.0, g.bottom - 0.5] {
                assert!(g.contains_y(y));
                assert_eq!(g.report_cell(g.left + 1.0, y, cols, rows), (1, rows), "@{scale}× y={y}");
            }
            // Never out of range, wherever the pointer is (even off-window).
            for x in [-1e6, -50.0, 0.0, 5000.0, 1e6] {
                for y in [-1e6, -50.0, 0.0, 5000.0, 1e6] {
                    let (c, r) = g.report_cell(x, y, cols, rows);
                    assert!((1..=cols).contains(&c) && (1..=rows).contains(&r), "@{scale}× ({x},{y}) → ({c},{r})");
                    let (l, c0, _) = g.select_cell(x, y, cols, rows);
                    assert!(l < rows && c0 < cols, "@{scale}× ({x},{y}) → ({l},{c0})");
                }
            }
        }
    }

    #[test]
    fn padded_grid_reports_the_cell_under_the_pointer_to_the_program() {
        // A click a pixel inside cell (col 3, row 2) of the padded 2× grid
        // reports exactly that cell (1-based 4;3); one in the left padding of
        // that row reports column 1.
        let (g, _, _) = padded_geom(2.0);
        let mut term = Terminal::new(80, 24);
        term.feed(SGR_CLICKS);
        let (mut mouse, mut selecting, mut out) = (GridMouse::default(), false, Vec::new());
        for (x, expect) in [(g.left + 3.0 * g.cell_w + 1.0, "\x1b[<0;4;3M"), (2.0, "\x1b[<0;1;3M")] {
            out.clear();
            let p = press(
                &mut Grid {
                    term: &mut term,
                    mouse: &mut mouse,
                    selecting: &mut selecting,
                    geom: g,
                    pointer: (x as f64, (g.top + 2.0 * g.cell_h + 1.0) as f64),
                    mods: NONE,
                    out: &mut out,
                },
                MouseButton::Left,
                false,
                t0(),
            );
            assert_eq!(p, Press::Reported);
            assert_eq!(String::from_utf8_lossy(&out), expect, "x={x}");
            let mut grid = Grid {
                term: &mut term,
                mouse: &mut mouse,
                selecting: &mut selecting,
                geom: g,
                pointer: (x as f64, (g.top + 2.0 * g.cell_h + 1.0) as f64),
                mods: NONE,
                out: &mut out,
            };
            release(&mut grid, MouseButton::Left);
        }
    }

    #[test]
    fn padded_grid_autoscrolls_from_the_top_padding_up() {
        // A selection drag in the top padding (above row 0's top edge) scrolls
        // history like one over the bar did; over the rows it does not.
        let (g, _, rows) = padded_geom(1.0);
        assert_eq!(autoscroll_lines(g, rows, g.top + 1.0), None);
        assert_eq!(autoscroll_lines(g, rows, g.top - 1.0), Some(1));
        assert_eq!(autoscroll_lines(g, rows, g.top + rows as f32 * g.cell_h), Some(-1));
    }

    #[test]
    fn edge_autoscroll_speed_grows_with_distance_and_stops_inside() {
        assert_eq!(autoscroll_lines(GEOM, 4, 50.0), None);
        assert_eq!(autoscroll_lines(GEOM, 4, 29.0), Some(1));
        assert_eq!(autoscroll_lines(GEOM, 4, 5.0), Some(3));
        assert_eq!(autoscroll_lines(GEOM, 4, -500.0), Some(AUTOSCROLL_MAX_LINES));
        assert_eq!(autoscroll_lines(GEOM, 4, 70.0), Some(-1));
        assert_eq!(autoscroll_lines(GEOM, 4, 95.0), Some(-3));
        // Fewer rows than the band: the gap below them already scrolls.
        assert_eq!(autoscroll_lines(GEOM, 2, 55.0), Some(-1));
    }

    #[test]
    fn dragging_above_the_grid_auto_scrolls_into_history_on_a_timer() {
        let mut feed = Vec::new();
        for i in 0..20 {
            feed.extend_from_slice(format!("line{i}\r\n").as_bytes());
        }
        let mut w = Win::new(&feed);
        let t = Instant::now();
        w.at(1.0, 2.0, NONE, |g| press(g, MouseButton::Left, false, t));
        // Above the grid (row -2 = 20 px above its top).
        w.at(1.0, -2.0, NONE, |g| motion(g, t));
        let due = w.mouse.autoscroll_due().expect("armed");
        assert!(due > t, "first step one tick later, never in the past");
        assert!(!w.at(1.0, -2.0, NONE, |g| autoscroll_step(g, t)).0, "not due yet");
        assert!(w.at(1.0, -2.0, NONE, |g| autoscroll_step(g, due)).0, "scrolled");
        assert!(w.term.scroll_offset() > 0);
        let next = w.mouse.autoscroll_due().expect("still armed");
        assert!(next > due);
        // Back over the grid: disarmed; release ends it for good.
        w.at(1.0, 1.0, NONE, |g| motion(g, next));
        assert_eq!(w.mouse.autoscroll_due(), None);
        w.at(1.0, -2.0, NONE, |g| motion(g, next));
        w.at(1.0, -2.0, NONE, |g| release(g, MouseButton::Left));
        assert_eq!(w.mouse.autoscroll_due(), None);
    }

    /// 20 lines of output and then `setup`: an 8×4 grid with history.
    fn with_history(setup: &[u8]) -> Win {
        let mut feed = Vec::new();
        for i in 0..20 {
            feed.extend_from_slice(format!("line{i}\r\n").as_bytes());
        }
        feed.extend_from_slice(setup);
        Win::new(&feed)
    }

    #[test]
    fn scrolled_back_new_gestures_are_jettys_until_the_view_is_live() {
        // fzf's Ctrl+R (`--height`) tracks the mouse on the PRIMARY screen. A
        // click in the history view reached it as a click on the live row under
        // the pointer — an unrelated entry, which a double click accepted — and
        // the wheel went to fzf too, so the view never came back down.
        let line = |n: f32| MouseScrollDelta::LineDelta(0.0, n);
        let mut acc = ScrollAccumulator::new();
        let mut w = with_history(b"\x1b[?1003h\x1b[?1006h");
        w.term.scroll_lines(3);
        let (p, bytes) = w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        assert_eq!((p, bytes.as_str()), (Press::Selecting, ""), "a press selects");
        assert_eq!(w.at(1.0, 1.0, NONE, |g| release(g, MouseButton::Left)).1, "");
        assert_eq!(w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Right, false, t0())), (Press::Menu, String::new()));
        assert_eq!(
            w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Middle, false, t0())),
            (Press::PastePrimary, String::new())
        );
        assert_eq!(w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Back, false, t0())), (Press::Ignored, String::new()));
        assert_eq!(w.at(3.0, 2.0, NONE, |g| motion(g, t0())).1, "", "no hover reports over history");
        // The wheel scrolls the view — up further, then down to the live screen.
        let (r, bytes) = w.at(0.0, 0.0, NONE, |g| wheel(g, line(1.0), false, &mut acc));
        assert_eq!((r, bytes.as_str()), (Wheel::Scrolled, ""));
        assert_eq!(w.term.scroll_offset(), 6);
        for _ in 0..2 {
            assert_eq!(w.at(0.0, 0.0, NONE, |g| wheel(g, line(-1.0), false, &mut acc)), (Wheel::Scrolled, String::new()));
        }
        assert_eq!(w.term.scroll_offset(), 0);
        // Live again: the program gets the wheel, the hover and the press.
        assert_eq!(w.at(0.0, 0.0, NONE, |g| wheel(g, line(-1.0), false, &mut acc)).1, "\x1b[<65;1;1M");
        assert_eq!(w.at(3.0, 2.0, NONE, |g| motion(g, t0())).1, "\x1b[<35;4;3M");
        assert_eq!(w.at(3.0, 2.0, NONE, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[<0;4;3M");
    }

    #[test]
    fn scrolled_back_a_held_press_ends_on_the_live_screens_rows() {
        // A press the program already holds still gets its drag and release
        // (it must never see a stuck button) — at the LIVE row under the
        // pointer: scrolled back one line, viewport row 3 shows live row 2, and
        // a pointer over history is row 1.
        let mut w = with_history(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(w.at(2.0, 1.0, NONE, |g| press(g, MouseButton::Left, false, t0())).1, "\x1b[<0;3;2M");
        w.term.scroll_lines(1);
        assert_eq!(w.at(2.0, 3.0, NONE, |g| motion(g, t0())).1, "\x1b[<32;3;3M");
        assert_eq!(w.at(2.0, 0.0, NONE, |g| motion(g, t0())).1, "\x1b[<32;3;1M");
        let (r, bytes) = w.at(2.0, 3.0, NONE, |g| release(g, MouseButton::Left));
        assert_eq!((r, bytes.as_str()), (Release::Program { dragged: true }, "\x1b[<0;3;3m"));
    }

    #[test]
    fn reset_forgets_held_buttons_and_the_autoscroll() {
        let mut w = Win::new(SGR_CLICKS);
        w.at(1.0, 1.0, NONE, |g| press(g, MouseButton::Left, false, t0()));
        w.mouse.reset();
        assert_eq!(w.at(1.0, 1.0, NONE, |g| release(g, MouseButton::Left)).1, "", "no orphan release report");
    }

    #[test]
    fn dropped_paths_are_quoted_for_the_shell() {
        let p = |s: &str| dropped_path_text(std::path::Path::new(s));
        assert_eq!(p("/home/me/notes.txt"), "/home/me/notes.txt ");
        assert_eq!(p("/home/me/My Docs/a.txt"), "'/home/me/My Docs/a.txt' ");
        assert_eq!(p("/tmp/it's"), r"'/tmp/it'\''s' ");
        assert_eq!(p("/tmp/çalışma"), "'/tmp/çalışma' ");
        assert_eq!(p("/tmp/$(rm -rf ~)"), "'/tmp/$(rm -rf ~)' ");
    }
}
