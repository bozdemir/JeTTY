use crate::snapshot::{attr, CellSnapshot, CursorShapeSnap, GridSnapshot, SearchHit};
use crate::theme::Theme;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb};
use std::collections::VecDeque;
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

/// Maximum retained OSC 133 command blocks per tab (memory bound; ~40 B each,
/// so ≤ ~160 KB worst case). Pruned to the live scrollback window on every bind.
const MAX_MARKS: usize = 4096;

/// The exact OSC 133 introducer the scanner matches after `ESC ]`.
const OSC133_PREFIX: &[u8] = b"133;";

/// State of the tiny OSC-133-only scanner, carried across [`Terminal::feed`]
/// calls so a mark split across PTY chunks resumes mid-sequence. Every non-Ground
/// state is mid-escape; Ground uses a `memchr(ESC)` fast path, so a stream with
/// no escapes costs one SIMD scan per feed and nothing per byte. The terminator
/// set `{0x07 BEL, 0x18 CAN, 0x1A SUB, 0x1B ESC}` and the `;` separator match
/// vte 0.15's OSC framing exactly (advance_osc_string), so there are no false
/// splits with adjacent OSC 0/8/4 sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscScan {
    /// Not inside an escape; scan forward to the next ESC via memchr.
    Ground,
    /// Saw ESC; a following `]` (0x5d) opens an OSC.
    Esc,
    /// Inside `ESC ]`, matching the `133;` prefix byte by byte (`n` matched).
    Prefix { n: u8 },
    /// Matched `133;`; collecting the letter (A/B/C/D) and the first `;code`.
    /// `code_done` is set by a SECOND `;` so `aid=<n>` params never corrupt the
    /// exit code (only the first param after the letter is the exit status).
    Payload { letter: u8, code: Option<u32>, in_code: bool, code_done: bool },
    /// Inside some OTHER OSC (title/hyperlink/color); skip to its terminator.
    Skip,
}

/// One shell command's OSC 133 semantic marks. `prompt`/`input`/`output` are
/// ABSOLUTE grid-line indices (`abs_top`-relative; survive scrolling); `exit`
/// comes from `D;<code>` (None = unknown). FAILED iff `finished && exit == Some(n != 0)`.
#[derive(Clone, Copy, Debug)]
struct CmdBlock {
    /// OSC 133 A — the prompt line (where the failed marker renders).
    prompt: i64,
    /// OSC 133 B — input start (refinement; unused by the two shipped features).
    input: Option<i64>,
    /// OSC 133 C — command-output start.
    output: Option<i64>,
    /// OSC 133 D exit code (None = no/empty/non-numeric code → unknown).
    exit: Option<i32>,
    /// Set once a D arrives (or a later A closes an abandoned command, e.g. ^C).
    finished: bool,
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
    /// The active scrollback-search query (what the user typed, capped at
    /// [`SEARCH_MAX_QUERY`] chars). Empty = no active search.
    search_query: String,
    /// Compiled smart-case literal regex for `search_query` (None when the
    /// query is empty or failed to compile — both render as "0/0").
    search_regex: Option<RegexSearch>,
    /// All matches across history+viewport, topmost→bottommost, capped at
    /// [`SEARCH_MAX_MATCHES`]. Match `Point`s go stale as scrollback rotates;
    /// the app calls [`Terminal::search_refresh`] (throttled) on output.
    search_matches: Vec<Match>,
    /// Index into `search_matches` of the CURRENT match (the counter's "n").
    search_current: usize,
    /// Absolute grid-line index of the active-region top (grid `Line(0)`).
    /// Advanced by `history_size()` growth in [`Terminal::advance_slice`] /
    /// [`Terminal::flush_sync`]; the stable anchor that lets OSC 133 prompt marks
    /// survive scrolling. EXACT for the whole unsaturated-scrollback lifetime
    /// (marks past the cap age out anyway). FROZEN while the alt screen is active
    /// and across an alt-screen toggle (that history change is not a scroll).
    abs_top: i64,
    /// OSC 133 scanner state, persisted across `feed` calls (chunk boundaries).
    scan: OscScan,
    /// Per-tab semantic prompt marks (OSC 133 A/B/C/D), append order == ascending
    /// `abs_top`-relative line, pruned to the live scrollback window on each bind.
    marks: VecDeque<CmdBlock>,
}

/// Maximum scrollback-search query length in chars (bounds per-keystroke DFA
/// builds and the search-bar layout).
pub const SEARCH_MAX_QUERY: usize = 256;
/// Maximum number of collected search matches; the counter shows "5000+"
/// when this cap is hit.
pub const SEARCH_MAX_MATCHES: usize = 5000;

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
            search_query: String::new(),
            search_regex: None,
            search_matches: Vec::new(),
            search_current: 0,
            abs_top: 0,
            scan: OscScan::Ground,
            marks: VecDeque::new(),
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
        // A shrink freed trimmed history rows, so stored search-match Points
        // can reference lines that no longer exist (wrong counter, Enter/F3
        // jumping to a clamped top-of-history). Re-collect, exactly like
        // `resize` does for reflow; cheap no-op when no search is active (F11).
        self.search_refresh();
        // A shrink also removes OLD history above `Line(0)` (which does NOT move,
        // so `abs_top` is unchanged): drop marks whose absolute line no longer
        // exists in the smaller live window.
        let history = self.term.grid().history_size();
        self.prune_marks(history);
    }

    /// Feed PTY bytes to the terminal, intercepting OSC 133 semantic-prompt
    /// marks on the way through (alacritty_terminal 0.26 / vte 0.15 drop 133).
    ///
    /// SPEED (#1): in `Ground` this is one `memchr(ESC)` per feed with zero
    /// per-byte work; a stream carrying no 133 reaches `advance_slice` exactly
    /// once (the whole buffer). Only inside an escape does the per-byte state
    /// machine run. Each input byte reaches alacritty exactly once (`start` is
    /// the first un-flushed byte); the scanner sub-advances alacritty up to AND
    /// INCLUDING a 133's terminator so the grid is caught up before the cursor
    /// line is read, then drops the (unhandled) 133 harmlessly.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut i = 0;
        let mut start = 0; // first byte not yet handed to alacritty
        while i < bytes.len() {
            if matches!(self.scan, OscScan::Ground) {
                match memchr::memchr(0x1b, &bytes[i..]) {
                    None => break, // no more escapes: flush the tail after the loop
                    Some(off) => {
                        i += off + 1; // step past the ESC
                        self.scan = OscScan::Esc;
                        continue;
                    }
                }
            }
            let b = bytes[i];
            match self.scan {
                // Ground is handled by the memchr fast path above.
                OscScan::Ground => unreachable!(),
                OscScan::Esc => {
                    self.scan = match b {
                        0x5d => OscScan::Prefix { n: 0 }, // ']' opens an OSC
                        0x1b => OscScan::Esc,             // ESC ESC: restart escape scan
                        _ => OscScan::Ground,             // some other escape; resync
                    };
                    i += 1;
                }
                OscScan::Prefix { n } => {
                    match b {
                        // A bare ESC aborts this OSC AND begins a new escape (vte
                        // parity) — so `ESC]133; <ESC> ]133;A BEL` still binds A.
                        0x1b => self.scan = OscScan::Esc,
                        // OSC ended before matching `133;` (e.g. `ESC]133 BEL`).
                        0x07 | 0x18 | 0x1a => self.scan = OscScan::Ground,
                        _ if b == OSC133_PREFIX[n as usize] => {
                            let n2 = n + 1;
                            self.scan = if n2 as usize == OSC133_PREFIX.len() {
                                OscScan::Payload {
                                    letter: 0,
                                    code: None,
                                    in_code: false,
                                    code_done: false,
                                }
                            } else {
                                OscScan::Prefix { n: n2 }
                            };
                        }
                        // Some other OSC (title/hyperlink/color): skip to its end.
                        _ => self.scan = OscScan::Skip,
                    }
                    i += 1;
                }
                OscScan::Payload { letter, code, in_code, code_done } => match b {
                    // BEL / CAN / SUB / ESC(=ST) all end the OSC (vte parity).
                    0x07 | 0x18 | 0x1a | 0x1b => {
                        let is_esc = b == 0x1b;
                        let k = i + 1;
                        // Catch alacritty up to & including the terminator, then
                        // read the cursor NOW (OSC 133 never moves it, so the line
                        // is identical whether read in this feed or a later split).
                        self.advance_slice(&bytes[start..k]);
                        self.bind_mark(letter, code.map(|c| c as i32));
                        // ESC leaves alacritty in Escape state and may begin a new
                        // sequence (the trailing `\` of an ST is consumed there).
                        self.scan = if is_esc { OscScan::Esc } else { OscScan::Ground };
                        start = k;
                        i = k;
                    }
                    b';' => {
                        // First `;` opens the code field; a SECOND `;` closes it so
                        // `aid=<n>` (p10k) never bleeds into the exit code.
                        self.scan = OscScan::Payload {
                            letter,
                            code,
                            in_code: true,
                            code_done: in_code || code_done,
                        };
                        i += 1;
                    }
                    b'0'..=b'9' if in_code && !code_done => {
                        self.scan = OscScan::Payload {
                            letter,
                            code: Some(code.unwrap_or(0).saturating_mul(10) + (b - b'0') as u32),
                            in_code,
                            code_done,
                        };
                        i += 1;
                    }
                    _ => {
                        // First byte after `133;` is the A/B/C/D letter. A
                        // non-digit inside the code field (e.g. `k=v`) makes the
                        // exit code unknown (None), closed so trailing digits do
                        // not resurrect it.
                        self.scan = if letter == 0 && !in_code {
                            OscScan::Payload { letter: b, code, in_code, code_done }
                        } else if in_code && !code_done {
                            OscScan::Payload { letter, code: None, in_code, code_done: true }
                        } else {
                            OscScan::Payload { letter, code, in_code, code_done }
                        };
                        i += 1;
                    }
                },
                OscScan::Skip => {
                    self.scan = match b {
                        0x1b => OscScan::Esc,             // ST: ends this OSC, new escape
                        0x07 | 0x18 | 0x1a => OscScan::Ground,
                        _ => OscScan::Skip,
                    };
                    i += 1;
                }
            }
        }
        if start < bytes.len() {
            self.advance_slice(&bytes[start..]);
        }
    }

    /// The ONLY place bytes reach alacritty; also where `abs_top` is maintained.
    /// `history_size()` grows by exactly the scrolled-line count until the buffer
    /// saturates, so `abs_top` is EXACT for the whole unsaturated lifetime.
    fn advance_slice(&mut self, s: &[u8]) {
        let alt_before = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h0 = self.term.grid().history_size();
        self.parser.advance(&mut self.term, s);
        let alt_after = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h1 = self.term.grid().history_size();
        self.track_abs_top(alt_before, alt_after, h0, h1);
    }

    /// Fold a `history_size` delta into `abs_top`, honoring the alt screen.
    /// Entering/leaving the alt screen (vim/less/htop) changes `history_size`
    /// WITHOUT scrolling, and while on the alt screen its history churn is not
    /// the primary scrollback — so `abs_top` (and every mark) is FROZEN across an
    /// alt-screen toggle and for its whole duration, resuming cleanly on return.
    fn track_abs_top(&mut self, alt_before: bool, alt_after: bool, h0: usize, h1: usize) {
        if alt_before != alt_after || alt_after {
            return;
        }
        if h1 >= h0 {
            self.abs_top += (h1 - h0) as i64;
        } else {
            self.on_history_shrunk(h1);
        }
    }

    /// Handle a non-scroll history shrink on the PRIMARY screen (a destructive
    /// reset `RIS`/`\ec`, or a scrollback clear `\e[3J`): `Line(0)` does not move,
    /// so `abs_top` stays monotonic; drop marks whose line no longer exists. The
    /// next prompt re-marks.
    fn on_history_shrunk(&mut self, history_size: usize) {
        self.prune_marks(history_size);
    }

    /// Drop marks outside the live window `[abs_top - history_size, abs_top + rows)`
    /// and cap the total (defensive). Called on every A-bind and on a shrink.
    fn prune_marks(&mut self, history_size: usize) {
        let min_abs = self.abs_top - history_size as i64;
        let max_abs = self.abs_top + self.rows as i64;
        self.marks.retain(|m| m.prompt >= min_abs && m.prompt < max_abs);
        while self.marks.len() > MAX_MARKS {
            self.marks.pop_front();
        }
    }

    /// Record an OSC 133 mark for the given sub-command letter (and D's exit
    /// code). Reads the cursor's absolute line immediately after the terminator
    /// has been advanced. No-op on the alt screen (OSC 133 inside a TUI is
    /// meaningless). Coalesces a duplicate A on the same line so p10k + our own
    /// snippet both emitting A cannot create two blocks.
    fn bind_mark(&mut self, letter: u8, exit: Option<i32>) {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let abs = self.abs_top + self.term.grid().cursor.point.line.0 as i64;
        match letter {
            b'A' => {
                // Dedup double-emission (p10k's own integration + ours).
                if let Some(last) = self.marks.back() {
                    if last.prompt == abs && !last.finished {
                        return;
                    }
                }
                // A new prompt closes any previous still-open block (a command
                // that never emitted D, e.g. ^C at the prompt) as unknown.
                if let Some(last) = self.marks.back_mut() {
                    last.finished = true;
                }
                self.marks.push_back(CmdBlock {
                    prompt: abs,
                    input: None,
                    output: None,
                    exit: None,
                    finished: false,
                });
                let history = self.term.grid().history_size();
                self.prune_marks(history);
            }
            b'B' => {
                if let Some(last) = self.marks.back_mut() {
                    last.input = Some(abs);
                }
            }
            b'C' => {
                if let Some(last) = self.marks.back_mut() {
                    last.output = Some(abs);
                }
            }
            b'D' => {
                // Bind to the most-recent still-open block (shells emit strictly
                // A…B…C…D, so "most recent open" is correct even with gaps).
                if let Some(block) = self.marks.iter_mut().rev().find(|m| !m.finished) {
                    block.exit = exit;
                    block.finished = true;
                }
            }
            _ => {} // unknown 133 sub-command: ignore
        }
    }

    /// Viewport rows (0-based) of currently-visible FAILED-command prompts
    /// (`D;<nonzero>`), for the themed left-edge marker. Empty in the common case
    /// and on the alt screen. Kept OFF the per-cell `GridSnapshot` so the render
    /// hot loop is untouched (SPEED). Uses the SAME `display_offset` mapping as
    /// `snapshot()`.
    pub fn failed_prompt_rows(&self) -> Vec<u16> {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return Vec::new();
        }
        let display_offset = self.term.grid().display_offset() as i64;
        let mut rows = Vec::new();
        for m in &self.marks {
            if !(m.finished && matches!(m.exit, Some(code) if code != 0)) {
                continue;
            }
            let grid_line = m.prompt - self.abs_top;
            let vp = grid_line + display_offset;
            if vp >= 0 && (vp as usize) < self.rows {
                rows.push(vp as u16);
            }
        }
        rows
    }

    /// Scroll the viewport to the previous (`forward == false`, older) or next
    /// (`forward == true`, newer) OSC 133 prompt, landing it at viewport row 0.
    /// Returns whether the viewport moved. PURE NO-OP when there are no marks
    /// (shell integration never enabled), on the alt screen, and at the ends
    /// (clamps, never wraps).
    pub fn jump_prompt(&mut self, forward: bool) -> bool {
        if self.term.mode().contains(TermMode::ALT_SCREEN) || self.marks.is_empty() {
            return false;
        }
        let display_offset = self.term.grid().display_offset() as i64;
        // Absolute line currently at viewport row 0 (top visible line).
        let viewport_top_abs = self.abs_top - display_offset;
        let target = if forward {
            self.marks.iter().map(|m| m.prompt).filter(|&p| p > viewport_top_abs).min()
        } else {
            self.marks.iter().map(|m| m.prompt).filter(|&p| p < viewport_top_abs).max()
        };
        let Some(target) = target else {
            return false; // clamp at the ends (no wrap)
        };
        let desired = (self.abs_top - target).clamp(0, self.scroll_max() as i64) as usize;
        let before = self.scroll_offset();
        self.scroll_to_offset(desired);
        self.scroll_offset() != before
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
        // The buffered lines become real here, so bracket `abs_top` the same way
        // `advance_slice` does (a sync block that scrolled must advance abs_top).
        let alt_before = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h0 = self.term.grid().history_size();
        self.parser.stop_sync(&mut self.term);
        let alt_after = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h1 = self.term.grid().history_size();
        self.track_abs_top(alt_before, alt_after, h0, h1);
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
                    // Pack the SGR text attributes we render (bold/italic/strike +
                    // underline style). BLINK (SGR 5/6) is intentionally NOT here:
                    // alacritty_terminal 0.26 drops the blink bit at the VT engine
                    // and a blink timer would fight ~0% idle (same non-goal as
                    // ligatures). DIM_BOLD contains the BOLD bit, so a dim+bold cell
                    // reads as bold via `contains(BOLD)`.
                    let flags = cell.flags;
                    let mut attrs = 0u8;
                    if flags.contains(Flags::BOLD) {
                        attrs |= attr::BOLD;
                    }
                    if flags.contains(Flags::ITALIC) {
                        attrs |= attr::ITALIC;
                    }
                    if flags.contains(Flags::STRIKEOUT) {
                        attrs |= attr::STRIKE;
                    }
                    // Underline style: most cells have none, so gate the five style
                    // tests behind a single ALL_UNDERLINES check. Priority ladder
                    // matches how the styles are mutually exclusive in the SGR model
                    // (the most specific colon-subparam form wins).
                    if flags.intersects(Flags::ALL_UNDERLINES) {
                        let ul = if flags.contains(Flags::UNDERCURL) {
                            attr::UL_UNDERCURL
                        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
                            attr::UL_DOTTED
                        } else if flags.contains(Flags::DASHED_UNDERLINE) {
                            attr::UL_DASHED
                        } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
                            attr::UL_DOUBLE
                        } else {
                            attr::UL_SINGLE
                        };
                        attrs |= ul << attr::UL_SHIFT;
                    }
                    // Underline color: SGR 58 (per-cell, stored in CellExtra) when
                    // set, otherwise the FINAL resolved fg (post INVERSE/DIM/HIDDEN)
                    // — so a reverse-video underline uses the swapped fg, and a
                    // HIDDEN (conceal) cell whose fg==bg draws an invisible underline.
                    // Deliberate: the underline tracks the visible glyph color. Gate
                    // the underline_color() lookup behind the underline flag: it is
                    // never read without an underline, so most cells skip it (SPEED).
                    let mut uline = fg;
                    if flags.intersects(Flags::ALL_UNDERLINES) {
                        if let Some(c) = cell.underline_color() {
                            uline = resolve_rgb(&self.theme, colors, c);
                        }
                    }
                    // WIDE_CHAR_SPACER: inherit the preceding base cell's attrs+uline
                    // so an underline/strike/bold spans the FULL width of a CJK glyph
                    // rather than only its left half. display_iter yields the base
                    // cell (col-1) before the spacer, so it is already stored. The
                    // spacer keeps its own bg (painted above) and blank char.
                    if flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 {
                        let base = &cells[row * self.cols + col - 1];
                        attrs = base.attrs;
                        uline = base.uline;
                    }
                    cells[row * self.cols + col] =
                        CellSnapshot { c, fg, bg, uline, attrs, selected: false };
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

        // Renderable cursor SHAPE (DECSCUSR `CSI Ps SP q`): 1/2 block, 3/4
        // underline, 5/6 beam. Hidden is folded into `cursor_visible` above, so
        // it maps to the Block default (never drawn while invisible).
        let cursor_shape = match content.cursor.shape {
            CursorShape::Underline => CursorShapeSnap::Underline,
            CursorShape::Beam => CursorShapeSnap::Beam,
            CursorShape::HollowBlock => CursorShapeSnap::HollowBlock,
            CursorShape::Block | CursorShape::Hidden => CursorShapeSnap::Block,
        };

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
            cursor_shape,
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
        // Unchanged dimensions: nothing reflows, so the grid, PTY geometry and
        // any stored search matches all stay valid — skip Term::resize AND the
        // full-history search re-collect below. App::reflow() resizes EVERY
        // tab per (debounced) window-resize event, so this guard keeps
        // same-size calls free on the interactive resize path (F15).
        if cols == self.cols && rows == self.rows {
            return;
        }
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
        // Reflow moved every line; stored search-match Points are stale now.
        // Cheap no-op when no search is active.
        self.search_refresh();
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

    /// Set (or replace) the scrollback-search query and recompute all matches.
    ///
    /// The query is a LITERAL string (regex metachars are escaped) compiled
    /// with alacritty's built-in smart-case: an all-lowercase query matches
    /// case-insensitively, any uppercase char makes it case-sensitive. The
    /// query is truncated to [`SEARCH_MAX_QUERY`] chars and matches are capped
    /// at [`SEARCH_MAX_MATCHES`]. The current match becomes the bottom-most
    /// match at or above the viewport bottom (nearest as the user reads up)
    /// and the view scrolls to it if off-screen.
    ///
    /// Returns `(current 1-based, total)`, `(0, 0)` when there is no match
    /// (empty query, failed compile, or genuinely nothing found).
    pub fn search_set_query(&mut self, query: &str) -> (usize, usize) {
        self.search_query = query.chars().take(SEARCH_MAX_QUERY).collect();
        self.search_regex = None;
        self.search_matches.clear();
        self.search_current = 0;
        if self.search_query.is_empty() {
            return (0, 0);
        }
        let pattern = escape_regex_literal(&self.search_query);
        // A failed compile (shouldn't happen for an escaped literal, but the
        // DFA has size limits) renders as "no matches" rather than an error.
        let Ok(regex) = RegexSearch::new(&pattern) else {
            return (0, 0);
        };
        self.search_regex = Some(regex);
        self.search_collect();
        if self.search_matches.is_empty() {
            return (0, 0);
        }
        // Current = the last match starting at or above the viewport bottom
        // (matches are topmost→bottommost); fall back to the last one.
        let display_offset = self.term.grid().display_offset();
        let bottom_line = self.rows as i32 - 1 - display_offset as i32;
        self.search_current = self
            .search_matches
            .iter()
            .rposition(|m| m.start().line.0 <= bottom_line)
            .unwrap_or(self.search_matches.len() - 1);
        self.search_scroll_to_current();
        (self.search_current + 1, self.search_matches.len())
    }

    /// Clear all scrollback-search state (query, matches, highlights).
    pub fn search_clear(&mut self) {
        self.search_query.clear();
        self.search_regex = None;
        self.search_matches.clear();
        self.search_current = 0;
    }

    /// Step the current match: `forward` (Enter/F3) moves UP through history
    /// (toward older output), `!forward` moves back down; both wrap. Scrolls
    /// the view so the new current match is visible. Returns the counter.
    pub fn search_nav(&mut self, forward: bool) -> (usize, usize) {
        let len = self.search_matches.len();
        if len == 0 {
            return (0, 0);
        }
        // Matches are ordered topmost→bottommost, so "older" = smaller index.
        self.search_current = if forward {
            (self.search_current + len - 1) % len
        } else {
            (self.search_current + 1) % len
        };
        self.search_scroll_to_current();
        (self.search_current + 1, len)
    }

    /// Whether a search query is currently set.
    pub fn search_is_active(&self) -> bool {
        !self.search_query.is_empty()
    }

    /// The active search query (empty when no search is set).
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// `(current 1-based, total)` — `(0, 0)` when there are no matches.
    pub fn search_counter(&self) -> (usize, usize) {
        if self.search_matches.is_empty() {
            (0, 0)
        } else {
            (self.search_current + 1, self.search_matches.len())
        }
    }

    /// Recompute matches with the existing query — called (throttled) after
    /// new PTY output and after a resize, because stored match `Point`s go
    /// stale when scrollback rotates or the grid reflows. Keeps the current
    /// index pointed at the nearest surviving match; never scrolls (streaming
    /// output must not fight the user's viewport).
    pub fn search_refresh(&mut self) {
        if self.search_regex.is_none() {
            return;
        }
        let old_start: Option<Point> =
            self.search_matches.get(self.search_current).map(|m| *m.start());
        self.search_collect();
        self.search_current = match (old_start, self.search_matches.len()) {
            (_, 0) => 0,
            (None, len) => len - 1,
            (Some(p), len) => self
                .search_matches
                .partition_point(|m| *m.start() < p)
                .min(len - 1),
        };
    }

    /// Visible match segments in viewport coordinates, split per row for
    /// wrapped matches, with the current match flagged. O(visible hits) via a
    /// binary search over the (ordered) match list — cheap per redraw.
    pub fn search_viewport_hits(&self) -> Vec<SearchHit> {
        if self.search_matches.is_empty() {
            return Vec::new();
        }
        let display_offset = self.term.grid().display_offset();
        let top_line = -(display_offset as i32);
        let bottom_line = self.rows as i32 - 1 - display_offset as i32;
        // Matches are disjoint and ordered, so end-lines are monotonic too.
        let first = self
            .search_matches
            .partition_point(|m| m.end().line.0 < top_line);
        let mut hits = Vec::new();
        for (i, m) in self.search_matches.iter().enumerate().skip(first) {
            let (s, e) = (*m.start(), *m.end());
            if s.line.0 > bottom_line {
                break;
            }
            for line in s.line.0..=e.line.0 {
                let col_start = if line == s.line.0 { s.column.0 } else { 0 };
                let col_end = if line == e.line.0 {
                    e.column.0
                } else {
                    self.cols.saturating_sub(1)
                };
                if let Some(vp) =
                    point_to_viewport(display_offset, Point::new(Line(line), Column(col_start)))
                {
                    if vp.line < self.rows {
                        hits.push(SearchHit {
                            row: vp.line,
                            col_start,
                            col_end,
                            is_current: i == self.search_current,
                        });
                    }
                }
            }
        }
        hits
    }

    /// Re-collect `search_matches` from the whole grid with the compiled
    /// regex (topmost→bottommost, capped at [`SEARCH_MAX_MATCHES`]).
    fn search_collect(&mut self) {
        self.search_matches.clear();
        let Some(regex) = self.search_regex.as_mut() else {
            return;
        };
        let grid = self.term.grid();
        let start = Point::new(grid.topmost_line(), Column(0));
        let end = Point::new(grid.bottommost_line(), grid.last_column());
        self.search_matches.extend(
            RegexIter::new(start, end, Direction::Right, &self.term, regex)
                .take(SEARCH_MAX_MATCHES),
        );
    }

    /// Scroll so the current match is visible: no-op when it already is,
    /// otherwise center it (clamped to the valid scroll range).
    fn search_scroll_to_current(&mut self) {
        let Some(m) = self.search_matches.get(self.search_current) else {
            return;
        };
        let start = *m.start();
        let display_offset = self.term.grid().display_offset();
        if let Some(vp) = point_to_viewport(display_offset, start) {
            if vp.line < self.rows {
                return;
            }
        }
        // Desired offset centers the match: viewport row rows/2 shows term
        // line (rows/2 - offset), so offset = rows/2 - match_line.
        let max = self.scroll_max() as i32;
        let target = (self.rows as i32 / 2 - start.line.0).clamp(0, max);
        self.scroll_to_offset(target as usize);
    }
}

/// Escape ASCII regex metacharacters so a user query is matched literally
/// (alacritty's `RegexSearch` always treats the pattern as a regex). Escaping
/// never adds an uppercase char, so smart-case is preserved.
fn escape_regex_literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
    use crate::snapshot::{attr, CursorShapeSnap};

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
    fn plain_cell_has_no_attrs() {
        // A plain ASCII cell carries no attributes and its underline color falls
        // back to fg (so a later underline draws in the glyph color by default).
        let mut t = Terminal::new(20, 5);
        t.feed(b"A");
        let cell = *t.snapshot().cell(0, 0);
        assert_eq!(cell.attrs, 0);
        assert!(!cell.is_bold() && !cell.is_italic() && !cell.is_strike());
        assert_eq!(cell.underline_style(), attr::UL_NONE);
        assert_eq!(cell.uline, cell.fg, "plain underline color should equal fg");
    }

    #[test]
    fn flags_map_to_attr_bits() {
        // \e[1m bold, \e[3m italic, \e[1;3m bold+italic, \e[9m strike.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[1;3mX\x1b[0m\x1b[9mS\x1b[0m");
        let snap = t.snapshot();
        let b = snap.cell(0, 0);
        assert!(b.is_bold() && !b.is_italic() && !b.is_strike(), "cell 0 should be bold only");
        let i = snap.cell(0, 1);
        assert!(i.is_italic() && !i.is_bold() && !i.is_strike(), "cell 1 should be italic only");
        let x = snap.cell(0, 2);
        assert!(x.is_bold() && x.is_italic(), "cell 2 should be bold+italic");
        let s = snap.cell(0, 3);
        assert!(s.is_strike() && !s.is_bold() && !s.is_italic(), "cell 3 should be strike only");
    }

    #[test]
    fn underline_styles_decode() {
        // Single \e[4m, double \e[4:2m, undercurl \e[4:3m, dotted \e[4:4m,
        // dashed \e[4:5m. NOTE: \e[21m is CancelBold in vte, NOT double underline.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[4mU\x1b[0m\x1b[4:2mD\x1b[0m\x1b[4:3mC\x1b[0m\x1b[4:4mo\x1b[0m\x1b[4:5mh\x1b[0m");
        let snap = t.snapshot();
        assert_eq!(snap.cell(0, 0).underline_style(), attr::UL_SINGLE);
        assert_eq!(snap.cell(0, 1).underline_style(), attr::UL_DOUBLE);
        assert_eq!(snap.cell(0, 2).underline_style(), attr::UL_UNDERCURL);
        assert_eq!(snap.cell(0, 3).underline_style(), attr::UL_DOTTED);
        assert_eq!(snap.cell(0, 4).underline_style(), attr::UL_DASHED);
    }

    #[test]
    fn double_underline_needs_colon_form_not_sgr_21() {
        // Guard the amendment: \e[21m must NOT produce a double underline.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[21mX\x1b[0m");
        assert_eq!(t.snapshot().cell(0, 0).underline_style(), attr::UL_NONE);
    }

    #[test]
    fn colored_underline_uses_sgr_58() {
        // \e[58;2;255;0;0m sets the underline color; \e[4m turns underline on.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[58;2;255;0;0m\x1b[4mX\x1b[0m");
        let cell = *t.snapshot().cell(0, 0);
        assert_eq!(cell.underline_style(), attr::UL_SINGLE);
        assert_eq!(cell.uline, [255, 0, 0], "explicit SGR 58 underline color");
        // A plainly underlined cell (no SGR 58) falls back to fg.
        let mut t2 = Terminal::new(20, 5);
        t2.feed(b"\x1b[4mY\x1b[0m");
        let c2 = *t2.snapshot().cell(0, 0);
        assert_eq!(c2.uline, c2.fg);
    }

    #[test]
    fn inverse_underline_uses_swapped_fg() {
        // Reverse-video (\e[7m) swaps fg/bg BEFORE uline falls back to fg, so the
        // underline color is the swapped (visible) fg. A conceal cell (fg==bg)
        // therefore draws an invisible underline.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[7m\x1b[4mX\x1b[0m");
        let cell = *t.snapshot().cell(0, 0);
        assert_eq!(cell.uline, cell.fg, "inverse underline uses the swapped fg");
        assert_eq!(cell.fg, cell.uline);
    }

    #[test]
    fn cursor_shape_from_decscusr() {
        // DECSCUSR: \e[1 q block, \e[3 q underline, \e[5 q beam.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[1 q");
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Block);
        t.feed(b"\x1b[3 q");
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Underline);
        t.feed(b"\x1b[5 q");
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Beam);
        // Hiding the cursor still reports invisible regardless of shape.
        t.feed(b"\x1b[?25l");
        assert!(!t.snapshot().cursor_visible);
    }

    #[test]
    fn wide_char_spacer_inherits_attrs() {
        // A bold+underlined CJK glyph must carry its attrs onto the WIDE_CHAR_SPACER
        // so the decoration spans both columns, not just the left half.
        let mut t = Terminal::new(20, 5);
        t.feed("\x1b[1m\x1b[4m世\x1b[0m".as_bytes());
        let snap = t.snapshot();
        let base = snap.cell(0, 0);
        let spacer = snap.cell(0, 1);
        assert!(base.is_bold() && base.underline_style() == attr::UL_SINGLE);
        assert_eq!(spacer.attrs, base.attrs, "spacer inherits base attrs");
        assert_eq!(spacer.uline, base.uline, "spacer inherits base underline color");
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
    fn search_finds_literal_matches() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"error one\r\nok fine\r\nerror two\r\nnothing\r\nerror three\r\n");
        let (cur, total) = t.search_set_query("error");
        assert_eq!(total, 3, "three literal occurrences");
        assert!((1..=3).contains(&cur), "current is 1-based within range");
        let hits = t.search_viewport_hits();
        assert_eq!(hits.len(), 3, "all three matches visible");
        for h in &hits {
            assert_eq!(h.col_end - h.col_start + 1, 5, "each hit spans 'error'");
        }
    }

    #[test]
    fn search_escapes_regex_metachars() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"1x5 125 1.5");
        let (_, total) = t.search_set_query("1.5");
        assert_eq!(total, 1, "'.' must be literal: only '1.5' matches");
        let hits = t.search_viewport_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].col_start, hits[0].col_end), (8, 10));
    }

    #[test]
    fn search_smart_case() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"ERROR here");
        let (_, total) = t.search_set_query("error");
        assert_eq!(total, 1, "lowercase query is case-insensitive");

        let mut t = Terminal::new(40, 5);
        t.feed(b"error here");
        let (_, total) = t.search_set_query("Error");
        assert_eq!(total, 0, "uppercase in the query makes it case-sensitive");
    }

    #[test]
    fn search_nav_wraps() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"aaa\r\nbbb\r\naaa\r\nccc\r\naaa\r\n");
        let (start, total) = t.search_set_query("aaa");
        assert_eq!(total, 3);
        // Three forward steps over three matches wrap back to the start.
        t.search_nav(true);
        t.search_nav(true);
        let (cur, _) = t.search_nav(true);
        assert_eq!(cur, start, "3 forward navs over 3 matches wrap around");
        // And one forward + one backward is a no-op.
        t.search_nav(true);
        let (cur, _) = t.search_nav(false);
        assert_eq!(cur, start, "forward then backward returns to start");
    }

    #[test]
    fn search_scrolls_to_history_match() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"needle here\r\n");
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let (cur, total) = t.search_set_query("needle");
        assert_eq!((cur, total), (1, 1));
        assert!(t.scroll_offset() > 0, "view must scroll up to the history match");
        let hits = t.search_viewport_hits();
        assert!(
            hits.iter().any(|h| h.is_current && h.row < t.rows()),
            "current hit must be within the viewport after the scroll; got {hits:?}"
        );
    }

    #[test]
    fn search_empty_query_clears() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"error error");
        let (_, total) = t.search_set_query("error");
        assert_eq!(total, 2);
        assert_eq!(t.search_set_query(""), (0, 0));
        assert_eq!(t.search_counter(), (0, 0));
        assert!(t.search_viewport_hits().is_empty());
        assert!(!t.search_is_active());
        // search_clear likewise.
        t.search_set_query("error");
        t.search_clear();
        assert_eq!(t.search_counter(), (0, 0));
        assert!(t.search_viewport_hits().is_empty());
        assert_eq!(t.search_query(), "");
    }

    #[test]
    fn search_survives_resize() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"alpha beta\r\nalpha gamma\r\n");
        let (_, total) = t.search_set_query("alpha");
        assert_eq!(total, 2);
        t.resize(10, 3);
        // Matches were recomputed against the reflowed grid — no panic, and
        // the counter stays consistent.
        let (cur, total) = t.search_counter();
        assert_eq!(total, 2, "both occurrences survive the reflow");
        assert!(cur >= 1 && cur <= total);
    }

    #[test]
    fn search_survives_scrollback_shrink() {
        // F11: a live scrollback shrink frees trimmed history rows; stored
        // match Points into those rows must be re-collected, not kept.
        let mut t = Terminal::new(40, 5);
        for i in 0..200 {
            t.feed(format!("error {i}\r\n").as_bytes());
        }
        let (_, total) = t.search_set_query("error");
        assert!(total > 50, "expected matches across history; got {total}");
        t.set_scrollback_lines(10);
        let (cur, total_after) = t.search_counter();
        assert!(
            total_after < total,
            "match total must drop with the freed history ({total} -> {total_after})"
        );
        assert!(cur >= 1 && cur <= total_after, "current index stays in range");
        // The refreshed list must equal a from-scratch re-collect (i.e. no
        // stale Points into freed lines survive).
        let fresh = t.search_set_query("error").1;
        assert_eq!(total_after, fresh, "refresh must match a fresh re-collect");
        // Navigation over the shrunk history stays within the valid range.
        t.search_nav(true);
        assert!(t.scroll_offset() <= t.scroll_max());
    }

    #[test]
    fn same_size_resize_is_a_noop() {
        // F15 hardening: App::reflow() resizes every tab per debounced window
        // resize; a same-dims call must not reflow, move the viewport, or
        // re-collect search matches.
        let mut t = Terminal::new(20, 5);
        for i in 0..30 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let (_, total) = t.search_set_query("line");
        t.scroll_lines(5);
        let offset = t.scroll_offset();
        t.resize(20, 5);
        assert_eq!((t.cols, t.rows), (20, 5));
        assert_eq!(t.scroll_offset(), offset, "same-size resize must not move the viewport");
        assert_eq!(t.search_counter().1, total, "matches unchanged by a same-size resize");
    }

    #[test]
    fn search_refresh_after_feed() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"error one\r\n");
        let (_, total) = t.search_set_query("error");
        assert_eq!(total, 1);
        t.feed(b"error two\r\nerror three\r\n");
        t.search_refresh();
        let (_, total) = t.search_counter();
        assert_eq!(total, 3, "refresh picks up matches in new output");
    }

    #[test]
    fn search_current_hit_flagged() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"foo\r\nfoo\r\nfoo\r\n");
        let (_, total) = t.search_set_query("foo");
        assert_eq!(total, 3);
        let hits = t.search_viewport_hits();
        assert_eq!(
            hits.iter().filter(|h| h.is_current).count(),
            1,
            "exactly one visible hit carries is_current"
        );
    }

    #[test]
    fn search_query_capped() {
        let mut t = Terminal::new(40, 5);
        let long = "x".repeat(1000);
        t.search_set_query(&long);
        assert_eq!(t.search_query().chars().count(), SEARCH_MAX_QUERY);
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

    // ── OSC 133 semantic-prompt scanner + marks (v0.14.0) ──────────────────────

    #[test]
    fn osc133_a_records_prompt_mark_and_drops_the_osc() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07hello");
        assert_eq!(t.marks.len(), 1, "OSC 133 A records one prompt mark");
        assert_eq!(t.marks.back().unwrap().prompt, 0, "prompt at the cursor's line");
        // The 133 is dropped (not printed); the text after it renders normally.
        assert!(t.snapshot().row_text(0).starts_with("hello"),
            "the 133 must be consumed, leaving 'hello' at col 0");
    }

    #[test]
    fn osc133_d_nonzero_flags_failed() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;C\x07");
        t.feed(b"\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![0], "D;1 marks the prompt failed");
    }

    #[test]
    fn osc133_d_zero_and_absent_not_failed() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;0\x07");
        assert!(t.failed_prompt_rows().is_empty(), "exit 0 is not failed");
        let mut t2 = Terminal::new(20, 5);
        t2.feed(b"\x1b]133;A\x07\x1b]133;D\x07");
        assert!(t2.failed_prompt_rows().is_empty(), "a bare D (no code) is not failed");
    }

    #[test]
    fn osc133_exit_code_parses_only_first_param() {
        // BLOCKING 2: `aid=<n>` (p10k) must never corrupt the exit code.
        let parse = |seq: &[u8]| -> Option<i32> {
            let mut t = Terminal::new(40, 5);
            t.feed(b"\x1b]133;A\x07");
            t.feed(seq);
            t.marks.back().unwrap().exit
        };
        assert_eq!(parse(b"\x1b]133;D\x07"), None);
        assert_eq!(parse(b"\x1b]133;D;0\x07"), Some(0));
        assert_eq!(parse(b"\x1b]133;D;1\x07"), Some(1));
        assert_eq!(parse(b"\x1b]133;D;130\x07"), Some(130));
        assert_eq!(parse(b"\x1b]133;D;1;aid=7\x07"), Some(1), "aid must not become 17 or 1*10+7");
        assert_eq!(parse(b"\x1b]133;D;;aid=7\x07"), None, "empty code is unknown");
    }

    #[test]
    fn osc133_bel_and_st_terminators_are_equivalent() {
        let mut a = Terminal::new(20, 5);
        a.feed(b"\x1b]133;A\x07"); // BEL
        let mut b = Terminal::new(20, 5);
        b.feed(b"\x1b]133;A\x1b\\"); // ST (ESC \)
        assert_eq!(a.marks.len(), 1);
        assert_eq!(b.marks.len(), 1);
        assert_eq!(a.marks.back().unwrap().prompt, b.marks.back().unwrap().prompt);
        // Neither leaks the ST trailing backslash into the grid.
        assert!(a.snapshot().row_text(0).trim().is_empty());
        assert!(b.snapshot().row_text(0).trim().is_empty(), "ST '\\' must not print");
    }

    #[test]
    fn osc133_split_across_feeds_binds_once() {
        // The scanner state persists across feed() calls (chunk boundaries).
        let seq = b"\x1b]133;A\x07";
        let mut t = Terminal::new(20, 5);
        for &byte in seq {
            t.feed(&[byte]);
        }
        assert_eq!(t.marks.len(), 1, "byte-split A binds exactly one mark");
        // A failed D;1 split at EVERY boundary must still flag failed.
        let full = b"\x1b]133;D;1\x07";
        for cut in 1..full.len() {
            let mut t = Terminal::new(20, 5);
            t.feed(b"\x1b]133;A\x07");
            t.feed(&full[..cut]);
            t.feed(&full[cut..]);
            assert_eq!(t.failed_prompt_rows(), vec![0], "split at byte {cut} still flags failed");
        }
    }

    #[test]
    fn osc133_esc_abort_then_restart_binds() {
        // Amendment improvement 1: a bare ESC mid-133 aborts it AND restarts
        // escape scanning, so an immediately following ESC]133;A still binds.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;\x1b]133;A\x07");
        assert_eq!(t.marks.len(), 1, "the aborted 133; binds nothing; the restarted 133;A binds one");
    }

    #[test]
    fn osc133_not_confused_by_adjacent_oscs() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"\x1b]0;the title\x07"); // OSC 0 title
        t.feed(b"\x1b]133;A\x07"); // our prompt mark
        t.feed(b"\x1b]8;;https://ex.io\x1b\\link\x1b]8;;\x1b\\"); // OSC 8 hyperlink
        t.feed(b"\x1b]4;1;#00ff00\x07"); // OSC 4 palette override
        t.feed(b"\x1b]133;D;1\x07"); // failed
        assert_eq!(t.marks.len(), 1, "exactly one prompt mark among adjacent OSCs");
        assert_eq!(t.failed_prompt_rows(), vec![0]);
        // The title OSC still worked (no false split swallowed it).
        assert_eq!(t.take_title_update(), Some(Some("the title".to_string())));
        // The OSC 8 hyperlink is intact.
        let hit = t.link_at(0, 1).expect("hyperlink survived interleaving");
        assert_eq!(hit.uri, "https://ex.io");
        // The OSC 4 override applied.
        t.feed(b"\x1b[31mZ");
        assert_eq!(t.snapshot().cell(0, 4).fg, [0, 255, 0], "OSC 4 override still took effect");
    }

    #[test]
    fn osc133_malformed_letter_only_and_no_letter() {
        // `133;A;aid=7` binds A (extra params ignored).
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A;aid=7\x07");
        assert_eq!(t.marks.len(), 1);
        assert_eq!(t.marks.back().unwrap().prompt, 0);
        // `133;` with no letter is a harmless no-op.
        let mut t2 = Terminal::new(20, 5);
        t2.feed(b"\x1b]133;\x07");
        assert!(t2.marks.is_empty(), "no letter → no mark");
    }

    #[test]
    fn mark_survives_scroll_and_maps_to_viewport() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"prep\r\n"); // content so the prompt is not on the very top line
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows().len(), 1, "visible at the bottom");
        // Push output so the marked prompt scrolls up into history.
        for i in 0..10 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.failed_prompt_rows().is_empty(), "off-screen at the live bottom");
        // Scroll to the very top of history: the marker reappears.
        t.scroll_lines(1000);
        assert_eq!(t.failed_prompt_rows().len(), 1, "marker tracks the prompt into history");
        // Back to the bottom: hidden again.
        t.scroll_to_bottom();
        assert!(t.failed_prompt_rows().is_empty());
    }

    #[test]
    fn mark_ages_out_at_scrollback_shrink() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1);
        for i in 0..200 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let abs_top_before = t.abs_top;
        // Shrink hard: the old mark's line no longer exists in the window.
        t.set_scrollback_lines(10);
        assert!(t.marks.is_empty(), "aged-out mark is pruned");
        // A shrink removes OLD history above Line(0), which does not move, so
        // abs_top stays monotonic (it now exceeds the smaller history_size).
        assert_eq!(t.abs_top, abs_top_before, "abs_top is monotonic across a shrink");
        // No panic / negative index on the now-empty mark list.
        assert!(t.failed_prompt_rows().is_empty());
        assert!(!t.jump_prompt(false));
    }

    #[test]
    fn jump_prompt_prev_next_and_clamps() {
        let mut t = Terminal::new(20, 5);
        let mut add_prompt = |t: &mut Terminal, tag: char| {
            t.feed(b"\x1b]133;A\x07");
            t.feed(b"\x1b]133;D;0\x07");
            for i in 0..6 {
                t.feed(format!("{tag}{i}\r\n").as_bytes());
            }
        };
        add_prompt(&mut t, 'a');
        add_prompt(&mut t, 'b');
        add_prompt(&mut t, 'c');
        assert_eq!(t.marks.len(), 3);
        t.scroll_to_bottom();
        // Step to each older prompt; the offset must strictly increase.
        assert!(t.jump_prompt(false), "prev → 3rd prompt");
        let o1 = t.scroll_offset();
        assert!(t.jump_prompt(false), "prev → 2nd prompt");
        let o2 = t.scroll_offset();
        assert!(o2 > o1, "older prompt is further up ({o1} < {o2})");
        assert!(t.jump_prompt(false), "prev → 1st prompt");
        let o3 = t.scroll_offset();
        assert!(o3 > o2);
        // Past the oldest: clamp (no wrap, no move).
        assert!(!t.jump_prompt(false), "no prompt older than the first");
        assert_eq!(t.scroll_offset(), o3, "clamped at the top");
        // Forward steps back toward the bottom.
        assert!(t.jump_prompt(true), "next → a newer prompt");
        assert!(t.scroll_offset() < o3);
    }

    #[test]
    fn jump_prompt_zero_marks_is_pure_noop() {
        // BLOCKING/amendment 4: never scroll-to-bottom on an empty mark list.
        let mut t = Terminal::new(20, 5);
        for i in 0..10 {
            t.feed(format!("y{i}\r\n").as_bytes());
        }
        t.scroll_lines(3);
        let off = t.scroll_offset();
        assert!(!t.jump_prompt(true), "no marks → no-op");
        assert!(!t.jump_prompt(false), "no marks → no-op");
        assert_eq!(t.scroll_offset(), off, "viewport unchanged with zero marks");
    }

    #[test]
    fn jump_prompt_noop_on_alt_screen() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;0\x07");
        for i in 0..10 {
            t.feed(format!("x{i}\r\n").as_bytes());
        }
        t.feed(b"\x1b[?1049h"); // enter alt screen
        let off = t.scroll_offset();
        assert!(!t.jump_prompt(false), "no jump while a TUI owns the display");
        assert_eq!(t.scroll_offset(), off);
    }

    #[test]
    fn alt_screen_freezes_abs_top_and_marks() {
        // BLOCKING 1: entering/leaving the alt screen must NOT corrupt abs_top
        // or wipe marks; both are frozen for the alt screen's duration.
        let mut t = Terminal::new(20, 5);
        for i in 0..20 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;D;1\x07");
        let abs_top_before = t.abs_top;
        let marks_before: Vec<i64> = t.marks.iter().map(|m| m.prompt).collect();
        let rows_before = t.failed_prompt_rows();
        assert!(!marks_before.is_empty());
        // Enter, churn, emit an (ignored) 133, and leave the alt screen.
        t.feed(b"\x1b[?1049h");
        for i in 0..30 {
            t.feed(format!("tui {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b]133;A\x07"); // must be ignored on the alt screen
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.abs_top, abs_top_before, "abs_top frozen across the alt screen");
        let marks_after: Vec<i64> = t.marks.iter().map(|m| m.prompt).collect();
        assert_eq!(marks_after, marks_before, "marks unchanged across the alt screen");
        assert_eq!(t.failed_prompt_rows(), rows_before, "marker maps to the same rows");
    }

    #[test]
    fn sync_defers_mark_but_abs_top_stays_exact() {
        // Documented sync edge (F1): a 133 inside a BSU binds at parse-arrival;
        // flush_sync must keep abs_top exactly tracking history (no drift).
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?2026h"); // BSU
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        t.feed(b"buffered line\r\n");
        t.flush_sync();
        // Continue with a real scroll and confirm the invariant abs_top == history.
        for i in 0..12 {
            t.feed(format!("z{i}\r\n").as_bytes());
        }
        t.scroll_to_bottom();
        assert_eq!(t.abs_top, t.scroll_max() as i64, "abs_top tracks history exactly after sync");
    }

    #[test]
    fn double_a_emission_coalesces() {
        // p10k's own integration + our snippet both emitting A on one prompt line
        // must not create two blocks.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;A\x07"); // same line, duplicate
        assert_eq!(t.marks.len(), 1, "duplicate A on the same line is coalesced");
    }
}
