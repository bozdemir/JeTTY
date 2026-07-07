use crate::snapshot::{CellSnapshot, GridSnapshot};
use crate::theme::Theme;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Pack `cols`/`rows` into a single `u32` (cols in the high 16 bits) so the
/// `Terminal` and its moved-away `EventProxy` can share live geometry through
/// an `Arc<AtomicU32>` (alacritty exposes no public listener setter).
fn pack_geom(cols: usize, rows: usize) -> u32 {
    ((cols.min(u16::MAX as usize) as u32) << 16) | (rows.min(u16::MAX as usize) as u32)
}

/// EventListener that captures the terminal's write-back bytes (replies to
/// host queries such as DSR/DA, text-area size, and OSC color queries) and
/// forwards them over a channel so the app can write them back to the PTY.
/// Without this, queries from the shell (e.g. p10k/zsh capability probes) get
/// no response and time out, which is what produced the red "x" at the first
/// prompt. p10k/zsh issue several distinct query types and any unanswered one
/// can make a prompt-hook command fail, so we answer all of them, not just
/// `PtyWrite`.
#[derive(Clone)]
struct EventProxy {
    tx: std::sync::mpsc::Sender<Vec<u8>>,
    /// Live terminal geometry (cols<<16 | rows), needed to answer
    /// `TextAreaSizeRequest` (\e[14t/\e[18t). Shared with the owning `Terminal`
    /// so `resize()` keeps these replies current after the proxy is moved into
    /// `Term` (alacritty has no public listener setter).
    geom: Arc<AtomicU32>,
    /// Live theme used to answer OSC `ColorRequest` queries (OSC 10/11/12/4;n;?).
    /// Real apps (nvim/fzf/delta/tmux) probe OSC 11 to detect a dark/light
    /// background, so the reply must track runtime theme changes — not a copy
    /// frozen at construction. Shared with the owning `Terminal` and updated in
    /// place by `set_theme` (alacritty exposes no public listener setter).
    theme: Arc<Mutex<Theme>>,
    /// Set to `true` when the terminal reports the child process (the shell)
    /// has exited (`Event::ChildExit`) or requests shutdown (`Event::Exit`).
    /// Shared with the owning `Terminal` so the app can close the window.
    child_exited: Arc<AtomicBool>,
    /// Pending OSC 0/2 title update, shared with the owning `Terminal`:
    /// `Some(Some(t))` = new title, `Some(None)` = reset to default, `None` =
    /// nothing pending. Multiple OSCs within one drain coalesce last-wins.
    title_update: Arc<Mutex<Option<Option<String>>>>,
    /// Cheap "a title update is pending" flag so the drain path can skip the
    /// mutex entirely in the common no-title case.
    title_dirty: Arc<AtomicBool>,
    /// Set to `true` when the app rings the bell (BEL / ^G, `Event::Bell`).
    /// Shared with the owning `Terminal`; consumed via [`Terminal::take_bell`].
    bell: Arc<AtomicBool>,
}

impl EventProxy {
    /// Resolve a color-request index to an RGB reply.
    ///
    /// The index follows alacritty's `colors` table: `0..=255` are the
    /// palette / 6x6x6 cube / grayscale ramp, and the named-color slots use
    /// `NamedColor` discriminants (`Foreground = 256`, `Background = 257`,
    /// `Cursor = 258`). Anything else falls back to the default foreground.
    fn color_for_index(&self, index: usize) -> Rgb {
        let theme = self.theme.lock().unwrap();
        let [r, g, b] = match index {
            0..=255 => index_to_rgb(&theme, index as u8),
            256 => theme.fg,            // NamedColor::Foreground
            257 => [theme.bg[0], theme.bg[1], theme.bg[2]], // Background
            258 => theme.cursor,        // NamedColor::Cursor
            _ => theme.fg,
        };
        Rgb { r, g, b }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            // Replies to DSR/DA-style queries the terminal answers itself.
            Event::PtyWrite(s) => {
                let _ = self.tx.send(s.into_bytes());
            }
            // \e[14t (text area size in pixels) / \e[18t (size in cells).
            // The formatter turns a WindowSize into the proper escape reply.
            // We do not track the live font metrics here, but apps that use \e[14t
            // to derive a cell aspect ratio (chafa/timg/notcurses/viu for image
            // rendering) get vertically-squashed output from a 1x1 (square) cell.
            // Report a typical ~1:2 monospace cell so the derived aspect is sane;
            // the cell/col/line counts are what shells actually care about.
            Event::TextAreaSizeRequest(fmt) => {
                let g = self.geom.load(Ordering::Relaxed);
                let window_size = WindowSize {
                    num_lines: (g & 0xFFFF) as u16,
                    num_cols: (g >> 16) as u16,
                    cell_width: 8,
                    cell_height: 16,
                };
                let _ = self.tx.send(fmt(window_size).into_bytes());
            }
            // OSC 4/10/11/12 color queries. Reply with a reasonable color drawn
            // from the active theme so p10k's color-capability probes succeed.
            Event::ColorRequest(index, fmt) => {
                let rgb = self.color_for_index(index);
                let _ = self.tx.send(fmt(rgb).into_bytes());
            }
            // The shell process exited (`ChildExit`) or the terminal requested
            // shutdown (`Exit`). Flag it so the app can close the window.
            Event::ChildExit(_) | Event::Exit => {
                self.child_exited.store(true, Ordering::SeqCst);
            }
            // OSC 0/2 shell-set title (also XTWINOPS 22/23 title-stack pops).
            // Stored in a single slot so a flood of title OSCs coalesces
            // last-wins; the app applies it on its PTY-drain path.
            Event::Title(t) => {
                *self.title_update.lock().unwrap() = Some(Some(t));
                self.title_dirty.store(true, Ordering::Release);
            }
            Event::ResetTitle => {
                *self.title_update.lock().unwrap() = Some(None);
                self.title_dirty.store(true, Ordering::Release);
            }
            // BEL (^G): flag it so the app can show an activity indicator on
            // the tab that rang while inactive.
            Event::Bell => {
                self.bell.store(true, Ordering::Relaxed);
            }
            // Wakeup/ClipboardStore/MouseCursorDirty and the rest are
            // intentionally ignored for now.
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    lines: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Terminal {
    term: Term<EventProxy>,
    parser: Processor,
    cols: usize,
    rows: usize,
    theme: Theme,
    /// The active theme shared with the `EventProxy` so OSC color-query replies
    /// reflect runtime theme changes. Kept in lockstep with `theme` by
    /// `set_theme` (the proxy holds the other `Arc` clone).
    theme_shared: Arc<Mutex<Theme>>,
    /// Receives the terminal's write-back bytes (replies to host queries).
    pty_write_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Set to `true` once the shell child process exits; shared with the
    /// `EventProxy` listener that observes `Event::ChildExit`/`Event::Exit`.
    child_exited: Arc<AtomicBool>,
    /// Live geometry shared with the `EventProxy` so `\e[14t`/`\e[18t` replies
    /// stay correct after a resize.
    geom: Arc<AtomicU32>,
    /// Pending shell-set title slot shared with the `EventProxy`; consumed by
    /// [`Terminal::take_title_update`].
    title_update: Arc<Mutex<Option<Option<String>>>>,
    /// Fast pending-title flag shared with the `EventProxy` (see above).
    title_dirty: Arc<AtomicBool>,
    /// Pending-bell flag shared with the `EventProxy` (`Event::Bell`);
    /// consumed by [`Terminal::take_bell`].
    bell: Arc<AtomicBool>,
}

/// A link found under the pointer by [`Terminal::link_at`]: the target URI
/// plus where to underline it in the viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub uri: String,
    /// Viewport underline spans: `(row, col_start, col_end)` inclusive,
    /// clipped to the visible grid.
    pub spans: Vec<(usize, usize, usize)>,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Terminal {
        // alacritty's MIN_COLUMNS is 2 but it is not enforced at Term::new;
        // a 1-column grid panics when a wide (CJK) glyph wraps and indexes
        // row.inner[1] on a 1-element row. Clamp to 2 (and rows to 1).
        let cols = cols.max(2);
        let rows = rows.max(1);
        let size = Size { cols, lines: rows };
        let config = Config { scrolling_history: 10_000, ..Default::default() };
        let (tx, pty_write_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        // Load theme from JETTY_THEME env var; default to "catppuccin_mocha".
        let theme_name = std::env::var("JETTY_THEME").unwrap_or_else(|_| "catppuccin_mocha".to_string());
        let mut theme = Theme::by_name(&theme_name);

        // Apply opacity override from JETTY_OPACITY (float 0.0..1.0).
        // This multiplies into the theme bg alpha, enabling composited transparency.
        if let Ok(op_str) = std::env::var("JETTY_OPACITY") {
            // Reject NaN (which parses fine but survives clamp() and yields a fully
            // transparent, invisible window); mirrors the config.rs NaN guard.
            if let Some(opacity) = op_str.parse::<f32>().ok().filter(|v| v.is_finite()) {
                // Clamp to a VISIBLE floor (0.1), matching the app/settings path —
                // a literal JETTY_OPACITY=0 would otherwise load a fully transparent
                // (invisible) window that reads as a launch failure.
                let opacity = opacity.clamp(0.1, 1.0);
                theme.bg[3] = (opacity * 255.0) as u8;
            }
        }

        // The listener needs the geometry and theme so it can answer
        // TextAreaSizeRequest and ColorRequest queries. Clamp the usize
        // dimensions into the u16 that WindowSize expects.
        let child_exited = Arc::new(AtomicBool::new(false));
        let geom = Arc::new(AtomicU32::new(pack_geom(cols, rows)));
        let theme_shared = Arc::new(Mutex::new(theme.clone()));
        let title_update = Arc::new(Mutex::new(None));
        let title_dirty = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));
        let proxy = EventProxy {
            tx,
            geom: Arc::clone(&geom),
            theme: Arc::clone(&theme_shared),
            child_exited: Arc::clone(&child_exited),
            title_update: Arc::clone(&title_update),
            title_dirty: Arc::clone(&title_dirty),
            bell: Arc::clone(&bell),
        };
        let term = Term::new(config, &size, proxy);

        Terminal {
            term,
            parser: Processor::new(),
            cols,
            rows,
            theme,
            theme_shared,
            pty_write_rx,
            child_exited,
            geom,
            title_update,
            title_dirty,
            bell,
        }
    }

    /// Drain all currently-pending write-back byte chunks emitted by the
    /// terminal (replies to host queries such as DSR/DA) into one `Vec<u8>`.
    /// Returns an empty vec if there is nothing pending. The caller is
    /// expected to write these bytes back to the PTY.
    pub fn drain_pty_writes(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.pty_write_rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// Take the pending shell-set title update, if any (OSC 0/2, or an
    /// XTWINOPS title-stack pop). Returns:
    /// * `None` — nothing pending (the common case; a lock-free flag check).
    /// * `Some(Some(title))` — the shell set a new (sanitized) title.
    /// * `Some(None)` — reset to the default title (explicit reset, or a title
    ///   that sanitized to empty, e.g. `\e]0;\a`).
    ///
    /// Consuming: a second call returns `None` until the next OSC arrives.
    /// Multiple OSCs between calls coalesce last-wins. NOTE: RIS (`\ec`) clears
    /// alacritty's internal title WITHOUT emitting an event, so a stale title
    /// survives a `reset` until the next OSC (upstream behavior).
    pub fn take_title_update(&mut self) -> Option<Option<String>> {
        if !self.title_dirty.swap(false, Ordering::Acquire) {
            return None;
        }
        self.title_update
            .lock()
            .unwrap()
            .take()
            .map(|u| u.and_then(|s| sanitize_title(&s)))
    }

    /// Replace the active theme at runtime. Also refreshes the copy shared with
    /// the `EventProxy` so subsequent OSC 10/11/12/4 color-query replies reflect
    /// the new theme (e.g. so nvim/fzf detect the right background).
    pub fn set_theme(&mut self, theme: Theme) {
        *self.theme_shared.lock().unwrap() = theme.clone();
        self.theme = theme;
    }

    /// Return a reference to the active theme.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Change the scrollback history limit LIVE. Shrinking frees the trimmed
    /// history rows and clamps the scroll offset; growing only raises the cap —
    /// already-trimmed lines cannot be restored (new output accumulates up to
    /// the new limit).
    ///
    /// Constraints (both must hold if either construction site changes):
    /// * `set_options` replaces the ENTIRE alacritty `Config`, so this must use
    ///   the exact same `..Default::default()` construction as `Terminal::new`;
    ///   a future non-default field there must be mirrored here or it would be
    ///   silently reverted.
    /// * `set_options` also re-emits the CURRENT title (`Event::Title`/
    ///   `ResetTitle`) via the `EventProxy`. That is benign: the re-emitted
    ///   value equals what's already displayed (the app's apply path is a
    ///   no-op on unchanged titles, and manual renames are flagged app-side).
    pub fn set_scrollback_lines(&mut self, lines: usize) {
        self.term.set_options(Config { scrolling_history: lines, ..Default::default() });
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Deadline of a pending synchronized update (DEC mode 2026, `CSI ?2026h`),
    /// or `None` when no sync is active.
    ///
    /// vte 0.15's `Processor` buffers all bytes received during a synchronized
    /// update and only flushes them on the matching ESU (`CSI ?2026l`) or when
    /// the embedder polls this deadline and calls [`Terminal::flush_sync`]. An
    /// app that sends a BSU and then crashes/pauses mid-redraw (nvim, zellij)
    /// would otherwise freeze the display until 2 MiB accumulate; the app must
    /// schedule a wakeup at this instant and force-flush on expiry.
    pub fn sync_deadline(&self) -> Option<std::time::Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Force-terminate a pending synchronized update, flushing every byte that
    /// was buffered since the BSU back through the parser so the screen updates.
    /// A no-op when no sync is active. Call this once [`Terminal::sync_deadline`]
    /// has elapsed.
    pub fn flush_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
    }

    pub fn snapshot(&self) -> GridSnapshot {
        let mut cells = vec![CellSnapshot::default(); self.cols * self.rows];
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        // Dynamic OSC 4/10/11/12 palette overrides (pywal, base16 hooks, etc.)
        // are stored in the Term's color table; consult it so redefined colors
        // actually change on screen, falling back to the static theme.
        let colors = self.term.colors();

        // Iterate over all visible cells. Each item has point in terminal coordinates
        // (line 0 = top of current viewport when display_offset=0; negative = history).
        // point_to_viewport converts to display row: viewport_line = point.line.0 + display_offset.
        for item in content.display_iter {
            if let Some(vp) = point_to_viewport(display_offset, item.point) {
                let row = vp.line;
                let col = vp.column.0;
                if row < self.rows && col < self.cols {
                    let cell = item.cell;
                    let mut fg = resolve_rgb(&self.theme, colors, cell.fg);
                    let mut bg = resolve_rgb(&self.theme, colors, cell.bg);
                    // Reverse video (`\e[7m`, also used by selections and `ls`
                    // highlights): swap fg/bg after resolving to RGB so the cell
                    // renders inverted once backgrounds are painted.
                    if cell.flags.contains(Flags::INVERSE) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    // SGR 2 (dim): alacritty sets Flags::DIM but leaves fg as a
                    // named color resolving to full brightness, so dim text would
                    // be indistinguishable from normal. Apply the conventional
                    // ~0.66 brightness multiplier to the foreground here. Done
                    // after INVERSE so the dimmed channel is whichever ends up fg.
                    if cell.flags.contains(Flags::DIM) {
                        fg = [
                            (fg[0] as f32 * 0.66) as u8,
                            (fg[1] as f32 * 0.66) as u8,
                            (fg[2] as f32 * 0.66) as u8,
                        ];
                    }
                    // SGR 8 (conceal): the glyph must not be readable (password
                    // echoes, secret-masking TUIs). Paint the foreground with the
                    // cell's background so the character is invisible while its
                    // background/layout are preserved. Done after INVERSE/DIM so it
                    // wins over whatever ended up as fg.
                    if cell.flags.contains(Flags::HIDDEN) {
                        fg = bg;
                    }
                    // A double-width glyph occupies two grid cells: the WIDE_CHAR
                    // cell holds the actual char, and the following
                    // WIDE_CHAR_SPACER cell is a placeholder. alacritty stores a
                    // space (or stale char) in the spacer; the wide glyph from the
                    // preceding cell already visually spans both columns via the
                    // font, so we force the spacer to a blank to keep columns
                    // aligned (preserving the spacer's own bg).
                    // KNOWN LIMITATION (F24): alacritty stores combining marks /
                    // zero-width chars (e.g. NFD accents, U+0301) in the cell's
                    // `zerowidth()` extra storage, separate from `cell.c`. We copy
                    // only the base `cell.c` here, so a decomposed "é" renders as a
                    // bare "e" (visible e.g. on macOS NFD `ls` output). Carrying the
                    // marks would require making the per-cell `CellSnapshot` (which is
                    // `Copy` and allocated cols×rows every frame) hold a variable-
                    // length char list AND reshaping base+marks in the render hot
                    // path — a cost we deliberately avoid to protect idle/throughput.
                    // `selection_to_string` DOES preserve them, so copied text is
                    // correct even though the on-screen base glyph is not composed.
                    let c = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        ' '
                    } else {
                        cell.c
                    };
                    cells[row * self.cols + col] = CellSnapshot { c, fg, bg, selected: false };
                }
            }
        }

        // Mark selected cells. Compute the selection range once (in terminal
        // coordinates) and iterate over viewport rows to mark covered cells.
        let sel_range = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
        if let Some(range) = sel_range {
            let display_offset = self.term.grid().display_offset();
            for vp_row in 0..self.rows {
                let term_point = viewport_to_point(display_offset, Point::new(vp_row, Column(0)));
                let term_line = term_point.line;
                // Skip rows outside the selection's line range.
                if term_line < range.start.line || term_line > range.end.line {
                    continue;
                }
                for col in 0..self.cols {
                    let pt = Point::new(term_line, Column(col));
                    if range.contains(pt) {
                        cells[vp_row * self.cols + col].selected = true;
                    }
                }
            }
        }

        // Cursor point is in terminal coordinates; convert to viewport (display)
        // row using the SAME display-offset mapping as the cells above. When the
        // user scrolls up into history the cursor's grid point maps OUTSIDE the
        // visible viewport (point_to_viewport → None, or a row past the last
        // visible line); in that case the cursor has scrolled off-screen and must
        // be hidden so it does not paint over scrollback content.
        let cursor_vp = point_to_viewport(display_offset, content.cursor.point);
        let cursor_in_view = cursor_vp.map(|p| p.line < self.rows).unwrap_or(false);
        let (cursor_row, cursor_col) = cursor_vp
            .map(|p| (p.line.min(self.rows.saturating_sub(1)), p.column.0.min(self.cols.saturating_sub(1))))
            .unwrap_or((0, 0));

        // Apps hide the cursor with DECTCEM (`\e[?25l`); alacritty then reports
        // the renderable cursor shape as `CursorShape::Hidden`. Treat that as not
        // visible. Also hide the cursor when it has scrolled out of the viewport.
        let cursor_visible = content.cursor.shape != CursorShape::Hidden && cursor_in_view;

        // Scrollbar data: display_offset is how many lines we're scrolled up
        // (0 = at bottom). history_size() is the number of lines in the scrollback
        // buffer (total_lines - screen_lines), which is the maximum scroll offset.
        let grid = self.term.grid();
        let scroll_offset = grid.display_offset();
        let scroll_max = grid.history_size();

        // Honor OSC 11 (background) / OSC 12 (cursor) dynamic overrides, keeping
        // the theme's background alpha; fall back to the theme when unset.
        let bg_rgba = match colors[257] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b, self.theme.bg[3]],
            None => self.theme.bg,
        };
        let cursor_rgb = match colors[258] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => self.theme.cursor,
        };

        GridSnapshot {
            cols: self.cols,
            rows: self.rows,
            cells,
            cursor_row,
            cursor_col,
            cursor_visible,
            bg_rgba,
            cursor_rgb,
            scroll_offset,
            scroll_max,
        }
    }

    /// Scroll the terminal display by `delta` lines.
    /// Positive delta scrolls UP into history (shows older output).
    /// Negative delta scrolls DOWN toward the bottom.
    pub fn scroll_lines(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    /// Scroll to the very bottom (live view, most recent output).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Scroll one page up (true) or down (false).
    pub fn scroll_page(&mut self, up: bool) {
        let delta = (self.rows as i32).saturating_sub(1);
        if up {
            self.scroll_lines(delta);
        } else {
            self.scroll_lines(-delta);
        }
    }

    /// Return the current display offset (how many lines scrolled up from bottom).
    /// 0 = at the live bottom; positive = scrolled into history.
    pub fn scroll_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Return the maximum scroll offset (== history_size, same value used in snapshot()).
    pub fn scroll_max(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Scroll to an absolute offset (0 = bottom, scroll_max = top of history).
    /// The offset is clamped to `0..=scroll_max()`.
    pub fn scroll_to_offset(&mut self, offset: usize) {
        let max = self.scroll_max();
        let offset = offset.min(max);
        let current = self.scroll_offset();
        // Delta: positive = scroll up into history, negative = scroll toward bottom.
        let delta = offset as i32 - current as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Return the number of rows (screen lines) in this terminal.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Return the number of columns in this terminal.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Whether the running application has enabled mouse reporting (any of the
    /// X10/normal/button-event/any-event mouse modes). When true, the app wants
    /// to receive mouse events (clicks, wheel) over the PTY instead of the host
    /// handling them locally (scroll/panel).
    pub fn mouse_mode(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Whether the app enabled button-event (drag) mouse tracking (`\e[?1002h`,
    /// `TermMode::MOUSE_DRAG`) — motion is reported only while a button is held.
    pub fn mouse_drag(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::MOUSE_DRAG)
    }

    /// Whether the app enabled any-event motion tracking (`\e[?1003h`,
    /// `TermMode::MOUSE_MOTION`) — every pointer move is reported.
    pub fn mouse_motion(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::MOUSE_MOTION)
    }

    /// Whether alternate-scroll is enabled (`TermMode::ALTERNATE_SCROLL`, on by
    /// default; togglable via `\e[?1007h/l`). When set and the terminal is on the
    /// alternate screen with mouse reporting off, the host must translate wheel
    /// ticks into cursor-key (Up/Down) sequences so pagers/editors scroll.
    pub fn alternate_scroll(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::ALTERNATE_SCROLL)
    }

    /// Whether the running application requested SGR-encoded mouse reports
    /// (`\e[?1006h`). We only emit SGR-format reports, so this gates whether
    /// mouse events should be forwarded at all.
    pub fn sgr_mouse(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    /// Whether the running application requested SGR-encoded mouse reports
    /// (`\e[?1006h`). Spec-named alias of [`Terminal::sgr_mouse`] for the
    /// input/app layers.
    pub fn mouse_sgr(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    /// Whether the application has enabled DECCKM application cursor keys
    /// (`\e[?1h`). When true, the arrow keys should be encoded with the `SS3`
    /// (`\eO`) prefix instead of `CSI` (`\e[`) so apps like vim/readline see the
    /// expected sequences.
    pub fn app_cursor_keys(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether the terminal is on the alternate screen (`\e[?1049h` etc.) —
    /// i.e. a full-screen app (less/vim/htop) owns the display. Alt-screen apps
    /// have no scrollback, so PageUp/PageDown should be forwarded to the PTY
    /// (`\e[5~`/`\e[6~`) instead of paging the (empty) host scrollback.
    pub fn alt_screen(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Resize the terminal grid to the given `cols` × `rows`, preserving
    /// existing content and scrollback via alacritty's `Term::resize`.
    ///
    /// This reflowing resize is preferred over replacing the `Term` because it
    /// preserves on-screen text and scrollback history. After resizing, the
    /// `EventProxy`'s geometry fields are updated so subsequent
    /// `TextAreaSizeRequest` replies report the correct dimensions.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        // Clamp cols to alacritty's MIN_COLUMNS (2); a 1-column grid panics on
        // wide-glyph wrap. See Terminal::new.
        let cols = cols.max(2);
        let rows = rows.max(1);
        self.cols = cols;
        self.rows = rows;
        // Publish the new geometry to the shared atomic BEFORE Term::resize so
        // the EventProxy answers any subsequent \e[14t/\e[18t with the new size.
        // This is the only mutation path into the proxy (alacritty exposes no
        // public listener setter).
        self.geom.store(pack_geom(cols, rows), Ordering::Relaxed);
        // Build a Size with the new dimensions and pass it to Term::resize.
        // Term::resize implements the xterm/VTE resize algorithm: it reflows
        // existing lines, preserves scrollback, and adjusts the cursor position.
        let new_size = Size { cols, lines: rows };
        self.term.resize(new_size);
    }

    /// Whether the shell child process has exited (or the terminal requested
    /// shutdown). Set asynchronously by the `EventProxy` listener; the app
    /// polls this to close the window when the shell exits.
    pub fn child_exited(&self) -> bool {
        self.child_exited.load(Ordering::SeqCst)
    }

    /// True once since the last call if the app rang the bell (BEL / ^G).
    /// Consuming read: a second call returns `false` until the next bell.
    pub fn take_bell(&self) -> bool {
        self.bell.swap(false, Ordering::Relaxed)
    }

    /// Start a Simple text selection at the given viewport cell (0-based).
    ///
    /// `left_half` is whether the pointer is in the LEFT half of the cell; it
    /// picks the cell `Side` (Left/Right) exactly as alacritty does from the
    /// sub-cell x position. Deriving the side from the pointer (rather than
    /// hardcoding Left at press / Right at update) is what makes reverse
    /// (right-to-left / bottom-to-top) drags keep both endpoint cells — a
    /// hardcoded Left/Right pair makes `to_range` swap the anchors on a backward
    /// drag and then trim one cell off each end.
    ///
    /// The viewport row is converted to a terminal `Point` accounting for the
    /// current display offset, mirroring `snapshot()`'s mapping. Any prior
    /// selection is cleared.
    pub fn selection_start(&mut self, viewport_line: usize, col: usize, left_half: bool) {
        let display_offset = self.term.grid().display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(col)));
        let side = if left_half { Side::Left } else { Side::Right };
        self.term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
    }

    /// Update the end of the current selection to the given viewport cell.
    /// `left_half` is the sub-cell x side (see [`Terminal::selection_start`]).
    /// Does nothing if no selection is active.
    pub fn selection_update(&mut self, viewport_line: usize, col: usize, left_half: bool) {
        let display_offset = self.term.grid().display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(col)));
        let side = if left_half { Side::Left } else { Side::Right };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(pt, side);
        }
    }

    /// Clear the active selection.
    pub fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    /// Return the currently-selected text, or `None` if no selection is active
    /// or the selection is empty.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Find a link at the given 0-based viewport cell, or `None`.
    ///
    /// Checks the cell's OSC 8 hyperlink first (fully wired by
    /// alacritty_terminal via `Cell::hyperlink()`), then falls back to
    /// plain-text detection: the WRAPLINE-assembled logical line around the
    /// hovered row (capped at [`crate::url::MAX_WRAP_WALK`] rows each way) is
    /// scanned by [`crate::url::find_url_at`]. Spans are recomputed from a
    /// fresh grid on every call — callers must never store terminal `Point`s
    /// across grid changes (history can shrink between hover and recompute).
    pub fn link_at(&self, viewport_line: usize, col: usize) -> Option<LinkHit> {
        let viewport_line = viewport_line.min(self.rows.saturating_sub(1));
        let col = col.min(self.cols.saturating_sub(1));
        let grid = self.term.grid();
        let display_offset = grid.display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(col)));

        // OSC 8 branch: underline the visible cells carrying the same link id
        // (id equality groups multi-segment links exactly as the app emitted
        // them; id-less links share one generated id per OSC run).
        if let Some(link) = grid[pt].hyperlink() {
            let mut spans: Vec<(usize, usize, usize)> = Vec::new();
            for vp_row in 0..self.rows {
                let line = viewport_to_point(display_offset, Point::new(vp_row, Column(0))).line;
                for c in 0..self.cols {
                    let same = grid[Point::new(line, Column(c))]
                        .hyperlink()
                        .is_some_and(|h| h.id() == link.id());
                    if same {
                        match spans.last_mut() {
                            Some(s) if s.0 == vp_row && s.2 + 1 == c => s.2 = c,
                            _ => spans.push((vp_row, c, c)),
                        }
                    }
                }
            }
            return Some(LinkHit { uri: link.uri().to_string(), spans });
        }

        // Plain-text branch: assemble the logical (unwrapped) line. A row
        // continues onto the next when ITS last cell carries WRAPLINE.
        let last_col = Column(self.cols - 1);
        let wrapped = |l: i32| grid[Line(l)][last_col].flags.contains(Flags::WRAPLINE);
        let mut start_line = pt.line.0;
        let top = grid.topmost_line().0;
        for _ in 0..crate::url::MAX_WRAP_WALK {
            if start_line > top && wrapped(start_line - 1) {
                start_line -= 1;
            } else {
                break;
            }
        }
        let mut end_line = pt.line.0;
        let bottom = grid.bottommost_line().0;
        for _ in 0..crate::url::MAX_WRAP_WALK {
            if end_line < bottom && wrapped(end_line) {
                end_line += 1;
            } else {
                break;
            }
        }
        // Exactly `cols` chars per row so char index i maps back to cell
        // (start_line + i/cols, i % cols); wide-char spacers blank to ' '
        // (same rule as `snapshot`) to keep cell/char indices aligned.
        let mut chars: Vec<char> =
            Vec::with_capacity((end_line - start_line + 1) as usize * self.cols);
        for l in start_line..=end_line {
            let row = &grid[Line(l)];
            for c in 0..self.cols {
                let cell = &row[Column(c)];
                chars.push(if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    ' '
                } else {
                    cell.c
                });
            }
        }
        let idx = (pt.line.0 - start_line) as usize * self.cols + col;
        let (s, e) = crate::url::find_url_at(&chars, idx)?;
        let uri: String = chars[s..e].iter().collect();
        // Map the char range back to viewport spans, keeping only visible rows.
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        for i in s..e {
            let term_line = start_line + (i / self.cols) as i32;
            let c = i % self.cols;
            if let Some(vp) = point_to_viewport(display_offset, Point::new(Line(term_line), Column(c))) {
                if vp.line < self.rows {
                    match spans.last_mut() {
                        Some(sp) if sp.0 == vp.line && sp.2 + 1 == c => sp.2 = c,
                        _ => spans.push((vp.line, c, c)),
                    }
                }
            }
        }
        Some(LinkHit { uri, spans })
    }

    /// Whether the terminal has bracketed paste mode enabled (`\e[?2004h`).
    pub fn bracketed_paste(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Select all text — the entire scrollback history plus the visible screen.
    ///
    /// Creates a Simple selection from the oldest history line (top-left) to the
    /// last visible row (bottom-right), so a subsequent `selection_text()` call
    /// returns the full terminal contents. Any prior selection is replaced.
    pub fn select_all(&mut self) {
        let grid = self.term.grid();
        let history = grid.history_size();
        let cols = self.cols;
        let rows = self.rows;
        // The grid uses negative line indices for history in alacritty's model.
        // `history_size()` lines of scrollback live above line 0.
        // We want to start at the very top of history and end at the last row.
        // alacritty's Line type is a newtype over i32 (via index::Line).
        let top = Point::new(Line(-(history as i32)), Column(0));
        let bottom = Point::new(Line(rows as i32 - 1), Column(cols.saturating_sub(1)));
        let mut sel = Selection::new(SelectionType::Simple, top, Side::Left);
        sel.update(bottom, Side::Right);
        self.term.selection = Some(sel);
    }
}

/// Sanitize a shell-provided OSC 0/2 title: strip control characters
/// (ESC/BEL/C0/DEL — nothing a shell legitimately puts in a title), cap at 256
/// chars (char-boundary safe by construction; the cap also bounds the per-tab
/// title hashing in the app's tab-bar cache), and trim whitespace. Returns
/// `None` when the result is empty, which the caller must treat as "reset to
/// the default title" (vte delivers `\e]0;\a` as `Title("")`, not `ResetTitle`).
fn sanitize_title(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !c.is_control()).take(256).collect();
    let t = t.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Convert a 256-color palette index to RGB (standard xterm scheme):
/// 0..=15 from the theme palette, 16..=231 the 6x6x6 cube, 232..=255 the grayscale ramp.
fn index_to_rgb(theme: &Theme, i: u8) -> [u8; 3] {
    match i {
        0..=15 => theme.palette[i as usize],
        16..=231 => {
            let c = i - 16;
            let levels = [0u8, 95, 135, 175, 215, 255];
            [
                levels[(c / 36) as usize],
                levels[((c % 36) / 6) as usize],
                levels[(c % 6) as usize],
            ]
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

/// Map an alacritty cell color to RGB using the active theme.
/// True-color is exact; named and indexed colors resolve through the theme
/// palette, unless a dynamic OSC 4/10/11/12 override is present in `colors`
/// (indexed by the same slot numbering as alacritty's color table), which wins.
fn resolve_rgb(theme: &Theme, colors: &Colors, color: alacritty_terminal::vte::ansi::Color) -> [u8; 3] {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    // Indexed and named colors map onto slots in the override table (Indexed(i)
    // -> i, Named(n) -> n as usize); a Some entry is a runtime redefinition.
    let override_slot = match color {
        Color::Indexed(i) => Some(i as usize),
        Color::Named(n) => Some(n as usize),
        Color::Spec(_) => None,
    };
    if let Some(rgb) = override_slot.and_then(|slot| colors[slot]) {
        return [rgb.r, rgb.g, rgb.b];
    }
    match color {
        Color::Spec(rgb) => [rgb.r, rgb.g, rgb.b],
        Color::Indexed(i) => index_to_rgb(theme, i),
        Color::Named(n) => match n {
            NamedColor::Background => [theme.bg[0], theme.bg[1], theme.bg[2]],
            NamedColor::Foreground | NamedColor::BrightForeground => theme.fg,
            NamedColor::Black => index_to_rgb(theme, 0),
            NamedColor::Red => index_to_rgb(theme, 1),
            NamedColor::Green => index_to_rgb(theme, 2),
            NamedColor::Yellow => index_to_rgb(theme, 3),
            NamedColor::Blue => index_to_rgb(theme, 4),
            NamedColor::Magenta => index_to_rgb(theme, 5),
            NamedColor::Cyan => index_to_rgb(theme, 6),
            NamedColor::White => index_to_rgb(theme, 7),
            NamedColor::BrightBlack => index_to_rgb(theme, 8),
            NamedColor::BrightRed => index_to_rgb(theme, 9),
            NamedColor::BrightGreen => index_to_rgb(theme, 10),
            NamedColor::BrightYellow => index_to_rgb(theme, 11),
            NamedColor::BrightBlue => index_to_rgb(theme, 12),
            NamedColor::BrightMagenta => index_to_rgb(theme, 13),
            NamedColor::BrightCyan => index_to_rgb(theme, 14),
            NamedColor::BrightWhite => index_to_rgb(theme, 15),
            // Dim*/Cursor and any future variants: approximate with default fg.
            _ => theme.fg,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_visible_by_default() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello");
        let snap = t.snapshot();
        assert!(snap.cursor_visible, "cursor should be visible by default");
    }

    #[test]
    fn cursor_hidden_after_dectcem_off() {
        let mut t = Terminal::new(20, 5);
        // DECTCEM off: hide the cursor.
        t.feed(b"\x1b[?25l");
        let snap = t.snapshot();
        assert!(!snap.cursor_visible, "cursor should be hidden after \\e[?25l");
    }

    #[test]
    fn cursor_reshown_after_dectcem_on() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?25l");
        assert!(!t.snapshot().cursor_visible);
        // DECTCEM on: show the cursor again.
        t.feed(b"\x1b[?25h");
        assert!(t.snapshot().cursor_visible, "cursor should be visible after \\e[?25h");
    }

    #[test]
    fn narrow_terminal_survives_wide_char() {
        // Regression: a 1-column grid would panic when a wide (CJK) glyph wraps
        // and indexes row.inner[1] on a 1-element row. cols is clamped to 2.
        let mut t = Terminal::new(1, 5);
        t.feed("世界".as_bytes());
        let _ = t.snapshot();
        let mut t = Terminal::new(20, 5);
        t.resize(1, 5);
        t.feed("世界".as_bytes());
        let _ = t.snapshot();
    }

    #[test]
    fn dim_text_is_darker_than_normal() {
        let mut t = Terminal::new(20, 5);
        // Normal "A" then dim "B".
        t.feed(b"A\x1b[2mB\x1b[0m");
        let snap = t.snapshot();
        let normal = snap.cell(0, 0).fg;
        let dim = snap.cell(0, 1).fg;
        assert!(
            (dim[0] as u16 + dim[1] as u16 + dim[2] as u16)
                < (normal[0] as u16 + normal[1] as u16 + normal[2] as u16),
            "dim fg {dim:?} should be darker than normal fg {normal:?}"
        );
    }

    #[test]
    fn plain_text_is_unchanged() {
        // Regression: hiding-cursor / wide-char handling must not alter ASCII text.
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello world");
        let snap = t.snapshot();
        assert_eq!(&snap.row_text(0)[..11], "hello world");
    }

    #[test]
    fn mouse_mode_off_by_default() {
        let t = Terminal::new(20, 5);
        assert!(!t.mouse_mode(), "mouse mode should be off by default");
        assert!(!t.sgr_mouse(), "SGR mouse should be off by default");
    }

    #[test]
    fn mouse_mode_enabled_by_app() {
        let mut t = Terminal::new(20, 5);
        // \e[?1000h: enable normal (button) mouse tracking.
        t.feed(b"\x1b[?1000h");
        assert!(t.mouse_mode(), "mouse mode should be on after \\e[?1000h");
        // \e[?1006h: request SGR-encoded reports.
        t.feed(b"\x1b[?1006h");
        assert!(t.sgr_mouse(), "SGR mouse should be on after \\e[?1006h");
        // Disabling turns it back off.
        t.feed(b"\x1b[?1000l");
        assert!(!t.mouse_mode(), "mouse mode should be off after \\e[?1000l");
    }

    #[test]
    fn reverse_video_swaps_fg_and_bg() {
        // `\e[7m` (reverse video) must swap the resolved fg/bg RGB so the cell
        // renders inverted. Capture the cell's normal colors first, then the
        // inverted cell, and assert they are swapped.
        let mut plain = Terminal::new(20, 5);
        plain.feed(b"X");
        let normal = *plain.snapshot().cell(0, 0);

        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[7mX");
        let inverted = *t.snapshot().cell(0, 0);

        assert_eq!(inverted.fg, normal.bg, "reverse video: fg should be old bg");
        assert_eq!(inverted.bg, normal.fg, "reverse video: bg should be old fg");
    }

    #[test]
    fn alt_screen_toggles() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.alt_screen(), "primary screen by default");
        // \e[?1049h: enter the alternate screen (what less/vim/htop use).
        t.feed(b"\x1b[?1049h");
        assert!(t.alt_screen(), "alt screen after \\e[?1049h");
        // \e[?1049l: back to the primary screen.
        t.feed(b"\x1b[?1049l");
        assert!(!t.alt_screen(), "primary screen after \\e[?1049l");
    }

    #[test]
    fn app_cursor_keys_toggles() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.app_cursor_keys(), "DECCKM off by default");
        // \e[?1h: enable application cursor keys (DECCKM).
        t.feed(b"\x1b[?1h");
        assert!(t.app_cursor_keys(), "DECCKM on after \\e[?1h");
        // \e[?1l: disable.
        t.feed(b"\x1b[?1l");
        assert!(!t.app_cursor_keys(), "DECCKM off after \\e[?1l");
    }

    #[test]
    fn child_exited_false_by_default() {
        let t = Terminal::new(20, 5);
        assert!(!t.child_exited(), "child should not be flagged exited at start");
    }

    #[test]
    fn bell_flag_set_by_bel_and_consumed_by_take() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.take_bell(), "no bell should be pending at start");
        t.feed(b"\x07");
        assert!(t.take_bell(), "BEL should arm the bell flag");
        assert!(!t.take_bell(), "take_bell is a consuming read");
    }

    #[test]
    fn bell_not_set_by_plain_output() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello");
        assert!(!t.take_bell(), "plain output must not ring the bell");
    }

    #[test]
    fn cursor_hidden_when_scrolled_into_history() {
        // Build scrollback: feed more lines than the 5-row screen so history exists.
        let mut t = Terminal::new(20, 5);
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        // At the bottom (live view), the cursor is on-screen and visible.
        let snap = t.snapshot();
        assert!(snap.scroll_max > 0, "expected scrollback to have built up");
        assert!(snap.cursor_visible, "cursor should be visible at the bottom");

        // Scroll up into history; the cursor scrolls off the viewport and must hide.
        t.scroll_lines(10);
        let snap = t.snapshot();
        assert!(snap.scroll_offset > 0, "should be scrolled up into history");
        assert!(
            !snap.cursor_visible,
            "cursor must be hidden once scrolled out of the viewport"
        );

        // Scroll back to the bottom; the cursor becomes visible again.
        t.scroll_to_bottom();
        let snap = t.snapshot();
        assert_eq!(snap.scroll_offset, 0, "back at the live bottom");
        assert!(snap.cursor_visible, "cursor visible again at the bottom");
    }

    #[test]
    fn resize_preserves_content_and_updates_dims() {
        // Feed text, resize to a different grid, verify the text survives and
        // the reported dimensions match the new size.
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello");
        // Resize to a smaller grid.
        t.resize(10, 3);
        assert_eq!(t.cols, 10, "cols should update to 10");
        assert_eq!(t.rows, 3, "rows should update to 3");
        // The text 'hello' should still be visible in the snapshot after reflow.
        let snap = t.snapshot();
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.rows, 3);
        let row0 = snap.row_text(0);
        assert!(
            row0.contains("hello"),
            "text 'hello' should survive resize; got row0={row0:?}"
        );
    }

    #[test]
    fn selection_text_and_selected_flag() {
        // Feed "hello" at column 0 row 0, start a selection from col 0 to col 4
        // and verify selection_text() returns the expected substring, and that
        // the covered cells have `selected == true` while others are false.
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello");
        // Start at viewport (0, 0) left half, update to (0, 4) right half → "hello".
        t.selection_start(0, 0, true);
        t.selection_update(0, 4, false);
        assert_eq!(t.selection_text().as_deref(), Some("hello"),
            "selection_text should return 'hello'");
        let snap = t.snapshot();
        for col in 0..5 {
            assert!(snap.cell(0, col).selected,
                "cell (0, {col}) should be selected");
        }
        // Column 5 onward should not be selected.
        assert!(!snap.cell(0, 5).selected, "cell (0, 5) should not be selected");
        // After clearing, none should be selected.
        t.selection_clear();
        assert_eq!(t.selection_text(), None, "selection_text should be None after clear");
        let snap2 = t.snapshot();
        for col in 0..5 {
            assert!(!snap2.cell(0, col).selected,
                "cell (0, {col}) should not be selected after clear");
        }
    }

    #[test]
    fn reverse_drag_keeps_both_endpoints() {
        // Regression (F4): pressing on the last char and dragging left to the
        // first must keep BOTH endpoint cells. With the side derived from the
        // sub-cell x position (press in the right half, release in the left
        // half) a backward drag over "hello" selects all of "hello", not "ell".
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello");
        // Press in the RIGHT half of 'o' (col 4), drag to the LEFT half of 'h' (col 0).
        t.selection_start(0, 4, false);
        t.selection_update(0, 0, true);
        assert_eq!(t.selection_text().as_deref(), Some("hello"),
            "reverse drag must not drop the endpoint cells");
    }

    #[test]
    fn sync_update_buffers_until_flush() {
        // Regression (F1): after a BSU (CSI ?2026h) the parser buffers all
        // subsequent bytes; they must not appear until an ESU OR the embedder
        // force-flushes on the sync deadline.
        let mut t = Terminal::new(20, 5);
        assert!(t.sync_deadline().is_none(), "no sync pending initially");
        t.feed(b"\x1b[?2026h"); // BSU: begin synchronized update
        assert!(t.sync_deadline().is_some(), "BSU must arm a sync deadline");
        t.feed(b"hidden");
        // The buffered text is NOT yet on screen.
        assert!(!t.snapshot().row_text(0).starts_with("hidden"),
            "bytes after BSU stay buffered until flush");
        // Force-flush (what the app does when the deadline elapses).
        t.flush_sync();
        assert!(t.sync_deadline().is_none(), "flush clears the sync deadline");
        assert!(t.snapshot().row_text(0).starts_with("hidden"),
            "flush_sync must make buffered output visible");
    }

    #[test]
    fn alternate_scroll_on_by_default() {
        // Regression (F3): alacritty enables ALTERNATE_SCROLL by default, so a
        // host must translate wheel→arrows on the alt screen. Apps can disable
        // it with \e[?1007l.
        let mut t = Terminal::new(20, 5);
        assert!(t.alternate_scroll(), "alternate-scroll on by default");
        t.feed(b"\x1b[?1007l");
        assert!(!t.alternate_scroll(), "alternate-scroll off after \\e[?1007l");
    }

    #[test]
    fn mouse_drag_and_motion_modes() {
        // Regression (F5): 1002 (button-drag) and 1003 (any-motion) must be
        // distinguishable so the app knows when to emit motion reports.
        let mut t = Terminal::new(20, 5);
        assert!(!t.mouse_drag() && !t.mouse_motion());
        t.feed(b"\x1b[?1002h");
        assert!(t.mouse_drag(), "1002 → drag reporting");
        assert!(t.mouse_mode(), "drag mode counts as mouse mode");
        t.feed(b"\x1b[?1002l\x1b[?1003h");
        assert!(t.mouse_motion(), "1003 → any-motion reporting");
    }

    #[test]
    fn select_all_covers_full_content() {
        // Feed two lines; select_all should produce text containing both words.
        let mut t = Terminal::new(20, 5);
        // Write "hello", then a carriage-return+newline to move to row 1.
        t.feed(b"hello\r\nworld");
        t.select_all();
        let text = t.selection_text().unwrap_or_default();
        assert!(text.contains("hello"), "select_all text should contain 'hello'; got {text:?}");
        assert!(text.contains("world"), "select_all text should contain 'world'; got {text:?}");
    }

    #[test]
    fn wide_char_spacer_is_blanked() {
        // A double-width CJK glyph occupies its WIDE_CHAR cell plus a following
        // WIDE_CHAR_SPACER cell. The wide char lands in column 0; column 1 (the
        // spacer) must read as a blank so columns stay aligned, and the char after
        // it lands in column 2.
        let mut t = Terminal::new(20, 5);
        // U+4E16 (世) is a double-width character, followed by ASCII 'X'.
        t.feed("世X".as_bytes());
        let snap = t.snapshot();
        assert_eq!(snap.cell(0, 0).c, '世', "wide char in column 0");
        assert_eq!(snap.cell(0, 1).c, ' ', "spacer column blanked");
        assert_eq!(snap.cell(0, 2).c, 'X', "following char in column 2");
    }

    #[test]
    fn concealed_text_is_hidden() {
        // SGR 8 (conceal) must render the glyph invisibly by painting the
        // foreground with the cell's own background; the bg itself is unchanged.
        let mut plain = Terminal::new(20, 5);
        plain.feed(b"S");
        let normal = *plain.snapshot().cell(0, 0);

        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[8mS");
        let hidden = *t.snapshot().cell(0, 0);
        assert_eq!(hidden.fg, hidden.bg, "concealed fg must equal its bg (invisible)");
        assert_eq!(hidden.bg, normal.bg, "concealed cell bg should be unchanged");
    }

    #[test]
    fn osc_background_query_reflects_runtime_theme() {
        // After set_theme, an OSC 11 (background) query must reply with the
        // CURRENT theme, not the one captured at construction.
        let mut t = Terminal::new(20, 5);
        t.set_theme(crate::theme::gruvbox_dark());
        // OSC 11 ; ? BEL — report the background color.
        t.feed(b"\x1b]11;?\x07");
        let reply = String::from_utf8(t.drain_pty_writes()).unwrap();
        // gruvbox_dark bg is [40, 40, 40] = 0x28 → "rgb:2828/2828/2828".
        assert!(
            reply.contains("2828/2828/2828"),
            "OSC 11 reply should carry the new theme bg; got {reply:?}"
        );
    }

    #[test]
    fn osc_title_sets_pending_update() {
        let mut t = Terminal::new(20, 5);
        assert_eq!(t.take_title_update(), None, "nothing pending initially");
        t.feed(b"\x1b]2;hello\x07");
        assert_eq!(t.take_title_update(), Some(Some("hello".to_string())));
        assert_eq!(t.take_title_update(), None, "update is consumed by take");
    }

    #[test]
    fn osc_title_st_terminator() {
        // OSC 0 (icon+title) with an ST (\e\\) terminator instead of BEL.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]0;world\x1b\\");
        assert_eq!(t.take_title_update(), Some(Some("world".to_string())));
    }

    #[test]
    fn osc_empty_title_is_reset() {
        // vte delivers `\e]2;\a` as Title("") — an empty title must map to a
        // reset (Some(None)), not a literal empty string.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]2;\x07");
        assert_eq!(t.take_title_update(), Some(None));
    }

    #[test]
    fn osc_title_sanitized() {
        // Control chars are stripped; over-long titles are capped at 256 chars.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]2;a\x01b\x08c\x7fd\x07");
        assert_eq!(t.take_title_update(), Some(Some("abcd".to_string())));
        let long = "x".repeat(1000);
        t.feed(format!("\x1b]2;{long}\x07").as_bytes());
        let got = t.take_title_update().flatten().unwrap();
        assert!(got.chars().count() <= 256, "title capped at 256 chars");
    }

    #[test]
    fn osc_title_coalesces_last_wins() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]2;a\x07\x1b]2;b\x07");
        assert_eq!(t.take_title_update(), Some(Some("b".to_string())));
        assert_eq!(t.take_title_update(), None, "coalesced into one update");
    }

    #[test]
    fn link_at_plain_url_single_row() {
        let mut t = Terminal::new(60, 5);
        t.feed(b"see https://example.com/page now");
        // "https://example.com/page" occupies cols 4..=27.
        let hit = t.link_at(0, 10).expect("URL under cursor");
        assert_eq!(hit.uri, "https://example.com/page");
        assert_eq!(hit.spans, vec![(0, 4, 27)]);
        // Every column of the URL hits; the surrounding text misses.
        for c in 4..=27 {
            assert!(t.link_at(0, c).is_some(), "col {c} should hit");
        }
        assert!(t.link_at(0, 0).is_none(), "'see' is not a link");
        assert!(t.link_at(0, 30).is_none(), "'now' is not a link");
    }

    #[test]
    fn link_at_wrapped_url_spans_both_rows() {
        // 20 cols: the 26-char URL wraps onto a second visual row (WRAPLINE).
        let mut t = Terminal::new(20, 5);
        t.feed(b"https://example.com/abcdef");
        // Hover the SECOND visual row: the full unwrapped URL must come back.
        let hit = t.link_at(1, 2).expect("wrapped URL under cursor");
        assert_eq!(hit.uri, "https://example.com/abcdef");
        assert_eq!(hit.spans, vec![(0, 0, 19), (1, 0, 5)]);
        // Hovering the first row yields the same hit.
        assert_eq!(t.link_at(0, 5), Some(hit));
    }

    #[test]
    fn link_at_explicit_newline_does_not_join_rows() {
        // A real \r\n between two charset runs must NOT merge them (no
        // WRAPLINE), unlike the wrapped case above.
        let mut t = Terminal::new(20, 5);
        t.feed(b"foo/bar.baz\r\nhttps://x.io");
        let hit = t.link_at(1, 3).expect("URL on row 1");
        assert_eq!(hit.uri, "https://x.io");
        assert_eq!(hit.spans, vec![(1, 0, 11)]);
        assert!(t.link_at(0, 3).is_none(), "row 0 alone is not a URL");
    }

    #[test]
    fn link_at_osc8_hyperlink() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\ plain");
        let hit = t.link_at(0, 2).expect("OSC 8 link under 'click'");
        assert_eq!(hit.uri, "https://example.com");
        // Exactly the 8 label cells ("click me"), nothing after the OSC close.
        assert_eq!(hit.spans, vec![(0, 0, 7)]);
        assert!(t.link_at(0, 10).is_none(), "'plain' carries no link");
    }

    #[test]
    fn link_at_non_link_cell_is_none() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"hello world");
        assert!(t.link_at(0, 2).is_none());
        assert!(t.link_at(3, 0).is_none(), "empty row");
    }

    #[test]
    fn link_at_scrolled_viewport_maps_history() {
        let mut t = Terminal::new(30, 5);
        t.feed(b"https://early.example/x\r\n");
        for i in 0..30 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        // At the live bottom the URL is out of view.
        assert!(t.link_at(0, 3).is_none());
        // Scroll to the very top of history: the URL is viewport row 0 again.
        t.scroll_lines(1000);
        let hit = t.link_at(0, 3).expect("URL in scrollback");
        assert_eq!(hit.uri, "https://early.example/x");
        assert_eq!(hit.spans, vec![(0, 0, 22)]);
    }

    #[test]
    fn link_at_after_wide_chars_keeps_alignment() {
        // Two CJK cells (each WIDE_CHAR + spacer) precede the URL; the spacer
        // → ' ' rule keeps char indices == cell columns.
        let mut t = Terminal::new(30, 5);
        t.feed("世界 https://x.io/a".as_bytes());
        // 世(0)+spacer(1) 界(2)+spacer(3) space(4) URL cols 5..=18.
        let hit = t.link_at(0, 8).expect("URL after CJK text");
        assert_eq!(hit.uri, "https://x.io/a");
        assert_eq!(hit.spans, vec![(0, 5, 18)]);
        assert!(t.link_at(0, 0).is_none(), "the CJK cell is not a link");
    }

    #[test]
    fn dynamic_palette_override_changes_displayed_color() {
        // An OSC 4 redefinition of a palette color must change what is drawn,
        // not be silently stored and ignored.
        let mut t = Terminal::new(20, 5);
        // OSC 4 ; 1 ; #00ff00 BEL — redefine palette index 1 (red) to green.
        t.feed(b"\x1b]4;1;#00ff00\x07");
        // SGR 31 selects palette index 1 as the foreground.
        t.feed(b"\x1b[31mX");
        let snap = t.snapshot();
        assert_eq!(
            snap.cell(0, 0).fg,
            [0, 255, 0],
            "OSC 4 override should change the displayed fg"
        );
    }
}
