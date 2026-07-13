use crate::hints::HintToken;
use crate::kitty::KittyCmd;
use crate::snapshot::{attr, CellSnapshot, CursorShapeSnap, GridSnapshot, SearchHit};
use crate::theme::Theme;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    Config, Osc52, Term, TermMode, point_to_viewport, viewport_to_point,
};
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

/// Turn a Kitty command's RAW payload (already base64-decoded and accumulated)
/// into a decoded `InlineImage`: apply `o=z` zlib inflate if requested, then
/// dispatch by `f=` format. `None` on any failure (correct-or-absent). Cold path.
fn decode_kitty_image(cmd: &KittyCmd, raw: Vec<u8>) -> Option<crate::sixel::InlineImage> {
    let data = if cmd.compressed {
        crate::kitty::inflate_zlib(&raw, KITTY_RAW_BUDGET)?
    } else {
        raw
    };
    match cmd.format {
        24 => crate::kitty::decode_rgb(cmd.width, cmd.height, &data, crate::sixel::SIXEL_CAPS),
        32 => crate::kitty::decode_rgba(cmd.width, cmd.height, &data, crate::sixel::SIXEL_CAPS),
        100 => crate::kitty::decode_png(&data, crate::sixel::SIXEL_CAPS),
        _ => None,
    }
}

/// Maximum decoded OSC 52 clipboard-copy payload (bytes) that we COMMIT to the
/// system clipboard. This is NOT a memory guard: alacritty/vte base64-decode and
/// UTF-8-validate the whole payload into a `String` BEFORE `Event::ClipboardStore`
/// reaches us, so the transient allocation is bounded by alacritty's own OSC string
/// buffer (~2 MiB), not by this cap. The cap only gates the COMMIT — a hostile
/// remote / stray `cat` cannot flood the real clipboard with megabytes — while 100
/// KiB comfortably covers real "yank a file" use. Also caps the clipboard→PTY reply
/// when `osc52_allow_paste` is enabled.
pub const OSC52_MAX_BYTES: usize = 100 * 1024;

/// Formatter supplied by alacritty with an OSC 52 PASTE (load) request: given the
/// clipboard text it returns the full `\e]52;…\a` reply to write back to the PTY.
/// Matches alacritty's `Event::ClipboardLoad` payload type exactly.
type ClipboardLoadFmt = Arc<dyn Fn(&str) -> String + Send + Sync + 'static>;

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
    /// Live cell pixel size (cell_w<<16 | cell_h, each rounded to u16), shared
    /// with the owning `Terminal` so the `\e[14t` text-area-size reply reports the
    /// REAL cell metrics (amendment A5). Image tools (chafa/timg/kitty) scale to
    /// this; a wrong (hardcoded 8×16) value makes HiDPI images render undersized.
    cell_px: Arc<AtomicU32>,
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
    /// Pending OSC 52 clipboard-COPY text (remote/tmux/nvim asked to set the system
    /// clipboard). `Some` = text to commit; last-wins coalesce. Committed by the app
    /// on the drain pass via [`Terminal::take_clipboard_store`]. Only ever set when
    /// alacritty's `osc52` mode permits copy (OnlyCopy/CopyPaste — the default).
    clipboard_store: Arc<Mutex<Option<String>>>,
    /// Cheap "a clipboard-copy is pending" flag so the drain path skips the mutex in
    /// the common no-copy case (lock-free — zero idle cost).
    clipboard_dirty: Arc<AtomicBool>,
    /// Pending OSC 52 clipboard-PASTE (load) request: the reply formatter alacritty
    /// supplied. Only ever set when `osc52` mode permits paste (OnlyPaste/CopyPaste),
    /// i.e. only when the user opted into `osc52_allow_paste`. Drained by the app via
    /// [`Terminal::take_clipboard_load`], which reads the clipboard, formats, and
    /// writes the reply to the PTY. Off by default (the secure default).
    clipboard_load: Arc<Mutex<Option<ClipboardLoadFmt>>>,
    /// Cheap "a clipboard-paste is pending" flag (mirrors `clipboard_dirty`).
    clipboard_load_dirty: Arc<AtomicBool>,
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
                // Real cell px shared from the owning Terminal (A5); fall back to
                // 8×16 only before the first `set_cell_px`. `\e[14t` reports the
                // text area in PIXELS, so the reply size is cols*cell_w × rows*cell_h.
                let cp = self.cell_px.load(Ordering::Relaxed);
                let cell_width = (cp >> 16) as u16;
                let cell_height = (cp & 0xFFFF) as u16;
                let window_size = WindowSize {
                    num_lines: (g & 0xFFFF) as u16,
                    num_cols: (g >> 16) as u16,
                    cell_width: if cell_width == 0 { 8 } else { cell_width },
                    cell_height: if cell_height == 0 { 16 } else { cell_height },
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
            // OSC 52 COPY: a remote host / tmux / nvim (`"+y`) asked to set the
            // system clipboard. alacritty already base64-decoded + UTF-8-validated
            // the payload and only emits this when its `osc52` mode permits copy
            // (OnlyCopy is JeTTY's default). Coalesce last-wins into a shared slot;
            // the app commits it on the drain pass. `Selection` (`p`/`s`) is merged
            // into the system clipboard for v1 (both are permitted remote writes).
            Event::ClipboardStore(_ty, text) => {
                // Cap the COMMITTED text (see OSC52_MAX_BYTES): reject an abusive
                // payload rather than flooding the real clipboard. The transient
                // decode already happened inside alacritty (bounded by its OSC
                // buffer), so this is a commit gate, not a memory guard.
                if text.len() <= OSC52_MAX_BYTES {
                    *self.clipboard_store.lock().unwrap() = Some(text);
                    self.clipboard_dirty.store(true, Ordering::Release);
                }
            }
            // OSC 52 PASTE (load): the app running in the PTY asked to READ the
            // system clipboard. alacritty only emits this when `osc52` permits paste
            // (OnlyPaste/CopyPaste) — never under the default OnlyCopy — so it is
            // inert unless the user set `osc52_allow_paste = true`. Stash the reply
            // formatter; the app reads the clipboard, caps + formats, writes to PTY.
            Event::ClipboardLoad(_ty, formatter) => {
                *self.clipboard_load.lock().unwrap() = Some(formatter);
                self.clipboard_load_dirty.store(true, Ordering::Release);
            }
            // Wakeup / MouseCursorDirty and the rest are intentionally ignored.
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

/// Cap on the sixel carry buffer (`sixel_buf`). A never-terminated or hostile
/// sixel cannot grow memory without bound: past this the scanner latches
/// `sixel_overflow`, keeps scanning for the terminator to resync, then DROPS the
/// image (correct-or-absent). 4 MiB comfortably holds any real terminal image.
const SIXEL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Clamp on the reserved cell-rows for one image (the injected line-feeds). Even
/// a legitimately tall image cannot scroll the grid without bound.
const MAX_IMAGE_ROWS: usize = 1024;

/// Cap on ONE Kitty APC's accumulated control+base64 payload (`apc_buf`), mirroring
/// [`SIXEL_MAX_BYTES`]. A single APC chunk is ≤ 4096 base64 bytes in practice, so
/// 4 MiB is generous; a never-terminated / hostile APC latches `apc_overflow`,
/// keeps scanning to resync, then DROPS (correct-or-absent).
const APC_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The RAW (post-base64, post-inflate) decode budget for a Kitty image, shared by
/// the cross-chunk accumulator and the zlib inflate limit. Reconciles the
/// transport ceiling with the decoder ceiling (amendment BLOCKING 2): a full HiDPI
/// window's uncompressed `f=32` RGBA (up to 16 Mpx) can actually reach the decoder.
/// = `SIXEL_CAPS.max_pixels * 4` = 64 MiB.
const KITTY_RAW_BUDGET: usize = crate::sixel::SIXEL_CAPS.max_pixels as usize * 4;

/// Hard cap on the number of `m=1` continuation chunks for one image — bounds a
/// pathological endless-`m=1` stream even below the byte budget.
const MAX_KITTY_CHUNKS: u32 = 4096;

/// Bounds on the transmit-then-put image registry (`kitty_images`): at most this
/// many stored images AND this many live decoded bytes; oldest evicted first.
const MAX_KITTY_STORED: usize = 64;
const MAX_KITTY_STORED_BYTES: u64 = 64 * 1024 * 1024;

/// Cap on retained inline-image placements per tab, plus a live-bytes budget on
/// their decoded RGBA (`Arc<SixelImage>`). Oldest are dropped first.
const MAX_PLACEMENTS: usize = 256;
const MAX_PLACEMENT_BYTES: u64 = 128 * 1024 * 1024;

/// Upper clamp for a parsed OSC 133 D exit code. Shell exit statuses are 8-bit
/// (0..=255; a signal death reports 128+signum), so anything larger is
/// non-conformant. Clamping here keeps the running parse from overflowing `u32`
/// on a crafted `D;<many digits>` and guarantees the later `as i32` cast stays
/// non-negative (a wrong-sign code could otherwise flip the failed/ok verdict).
const EXIT_CODE_MAX: u32 = 255;

/// State of the tiny escape scanner, carried across [`Terminal::feed`] calls so
/// a sequence split across PTY chunks resumes mid-parse. It recognizes BOTH
/// OSC 133 prompt marks (`ESC ]`) and sixel DCS images (`ESC P … q … ST`) in ONE
/// single-ESC state machine — Ground uses a `memchr(ESC)` fast path, so a stream
/// with no escapes costs one SIMD scan per feed and nothing per byte.
///
/// The OSC terminator set `{0x07 BEL, 0x18 CAN, 0x1A SUB, 0x1B ESC}` and the `;`
/// separator match vte 0.15's OSC framing (advance_osc_string). The DCS
/// terminators are DIFFERENT — `{0x18 CAN, 0x1A SUB, 0x1B ESC, 0x9C ST}`, and
/// **0x07 BEL is a DATA byte inside a DCS**, never a terminator (vte's
/// advance_dcs_passthrough). The two terminator sets are kept strictly separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scan {
    /// Not inside an escape; scan forward to the next ESC via memchr.
    Ground,
    /// Saw ESC; a following `]` (0x5d) opens an OSC, `P` (0x50) opens a DCS.
    Esc,
    /// Inside `ESC ]`, matching the `133;` prefix byte by byte (`n` matched).
    Prefix { n: u8 },
    /// Matched `133;`; collecting the letter (A/B/C/D) and the first `;code`.
    /// `code_done` is set by a SECOND `;` so `aid=<n>` params never corrupt the
    /// exit code (only the first param after the letter is the exit status).
    Payload { letter: u8, code: Option<u32>, in_code: bool, code_done: bool },
    /// Inside some OTHER OSC (title/hyperlink/color); skip to its terminator.
    Skip,
    /// Inside `ESC P`, collecting the `P1;P2;P3` params up to the final byte
    /// (`0x40..=0x7E`). `field` tracks which param digit run we're in; `p2` (the
    /// background-select param) is stashed for the decoder. `inter` latches an
    /// intermediate byte (`0x20..=0x2F`) — a DCS with intermediates is NOT a
    /// sixel (DECRQSS `$q`, XTGETTCAP `+q`), so it routes to `DcsOther`.
    DcsParams { p2: u32, field: u8, inter: bool },
    /// After a sixel `q` (final `0x71`, no intermediates): accumulating raw sixel
    /// data bytes into `sixel_buf` until a DCS terminator. BEL is data here.
    Sixel,
    /// A DCS that is NOT a bare-`q` sixel: skip to the DCS terminator, touch
    /// nothing (no accumulation, no placement).
    DcsOther,
    /// Saw `ESC _` (APC introducer): expecting the graphics identifier `G` (0x47).
    ApcIntro,
    /// Inside `ESC _ G`: accumulating the control+base64 payload into `apc_buf`
    /// until an APC terminator (ST / CAN / SUB / 8-bit ST). BEL is DATA here.
    Apc,
    /// An APC that is NOT `_G…` (some other APC use): skip to the terminator,
    /// accumulate nothing, emit nothing.
    ApcOther,
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
    /// Monotonic instant stamped at the C mark (command start), so a duration can
    /// be computed at D. `None` when no C was seen this block (e.g. plain bash,
    /// which emits only A+D) — the completion then carries an unknown duration.
    started_at: Option<std::time::Instant>,
}

/// One live inline-image placement (a decoded sixel anchored in the grid).
///
/// `abs_line` is the ABSOLUTE grid line of the image's top-left cell — the exact
/// analogue of `CmdBlock::prompt` (`abs_top`-relative, survives scrolling). The
/// decoded RGBA lives behind an `Arc` so the render layer can clone it cheaply to
/// upload to (each window's) GPU without copying, and so a texture evicted then
/// re-scrolled-into-view can re-upload. `cols`/`rows` are the reserved cell
/// footprint; `px_w`/`px_h` are the native pixel size the image draws at.
#[derive(Clone, Debug)]
struct ImagePlacement {
    id: u64,
    abs_line: i64,
    col: u16,
    cols: u16,
    rows: u16,
    px_w: u16,
    px_h: u16,
    image: Arc<crate::sixel::SixelImage>,
    /// The Kitty protocol image id (`i=`) or number (`I=`) this placement was
    /// created from, so `a=d,d=i,i=N` can target it (amendment A6). `None` for
    /// sixel placements and anonymous Kitty transmits.
    kitty_id: Option<u32>,
}

/// One finished shell command, surfaced from an OSC 133 `D` mark. The tab index /
/// window is attributed by `jetty-app` (it owns the tab→window mapping); this
/// struct is per-terminal. Drained on the existing PTY-drain pass via
/// [`Terminal::take_completions`] — no new event, no poll, no idle cost.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandCompletion {
    /// `D;<code>`. `None` = the shell sent no / an empty / a non-numeric code
    /// (unknown). A clamped byte (0..=255); nonzero ⇒ the command FAILED.
    pub exit_code: Option<i32>,
    /// `D − C` wall time. `None` = no C mark this block (bash without preexec),
    /// so the duration is unknown and the notifier degrades to failure-only.
    pub duration: Option<std::time::Duration>,
    /// Last non-empty line of the command's output region, trimmed + capped
    /// (the notification body). Empty string when the region was blank.
    pub last_line: String,
}

/// Defensive cap on buffered, undrained completions (a misbehaving flood can't
/// grow `Terminal.completed` without bound). Far above the ~1 completion/drain
/// steady state.
const MAX_PENDING_COMPLETIONS: usize = 32;

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
    /// Pending OSC 52 clipboard-copy text + flag, shared with the `EventProxy`;
    /// consumed by [`Terminal::take_clipboard_store`].
    clipboard_store: Arc<Mutex<Option<String>>>,
    clipboard_dirty: Arc<AtomicBool>,
    /// Pending OSC 52 clipboard-paste reply formatter + flag, shared with the
    /// `EventProxy`; consumed by [`Terminal::take_clipboard_load`]. Inert unless
    /// `osc52_mode` permits paste.
    clipboard_load: Arc<Mutex<Option<ClipboardLoadFmt>>>,
    clipboard_load_dirty: Arc<AtomicBool>,
    /// The OSC 52 mode this terminal was built with. Stored so `set_scrollback_lines`
    /// (which rebuilds the alacritty `Config`) preserves it instead of silently
    /// reverting an enabled paste back to the default `OnlyCopy`. Toggled by
    /// [`Terminal::set_osc52_allow_paste`].
    osc52_mode: Osc52,
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
    /// survive scrolling. Only its DIFFERENCES with a mark's absolute line matter,
    /// so its absolute offset is arbitrary — what has to hold is that it advances
    /// by exactly the number of lines scrolled off the top between a mark's bind
    /// and every later read.
    ///
    /// That holds EXACTLY for the whole unsaturated-scrollback lifetime (the
    /// common case). It CANNOT hold once the primary scrollback ring saturates:
    /// `history_size()` pins at [`Terminal::scrollback_limit`] while real scroll
    /// continues, so the delta becomes unobservable (alacritty 0.26 exposes no
    /// saturation-proof scroll counter). Rather than drift and paint markers on
    /// wrong rows, saturation latches [`Terminal::saturated`] and drops the marks
    /// (correct-or-absent). FROZEN while the alt screen is active and across an
    /// alt-screen toggle (that history change is not a scroll).
    abs_top: i64,
    /// The configured scrollback cap (mirrors the alacritty `Config`'s
    /// `scrolling_history`; kept in lockstep by `new`/`set_scrollback_lines`).
    /// `history_size()` saturates at this value, which is exactly when `abs_top`
    /// can no longer track lines scrolled off the top.
    scrollback_limit: usize,
    /// Latched once the primary scrollback fills to `scrollback_limit`: past that
    /// point `abs_top` can no longer count scrolled-off lines, so mark positions
    /// are untrustworthy. While set, marks are dropped and never (re)bound or
    /// rendered. Cleared again if history later falls below the cap (a scrollback
    /// clear / shrink), where fresh marks resume tracking exactly.
    saturated: bool,
    /// Escape scanner state (OSC 133 + sixel DCS), persisted across `feed` calls
    /// (chunk boundaries).
    scan: Scan,
    /// Per-tab semantic prompt marks (OSC 133 A/B/C/D), append order == ascending
    /// `abs_top`-relative line, pruned to the live scrollback window on each bind.
    marks: VecDeque<CmdBlock>,
    /// Command completions discovered during `feed()` (OSC 133 `D`). Drained by
    /// the app on the PTY-drain pass via [`Terminal::take_completions`]. Empty in
    /// the common case; bounded by [`MAX_PENDING_COMPLETIONS`]. A plain `Vec` on
    /// `&mut self` (not an `Arc`/atomic like `bell`) is correct: `D` is handled by
    /// our own scanner inside `feed(&mut self)`, never the async `EventProxy`.
    completed: Vec<CommandCompletion>,
    /// Raw sixel data bytes accumulated while in `Scan::Sixel`, capped at
    /// [`SIXEL_MAX_BYTES`]. Persists across `feed` chunk boundaries.
    sixel_buf: Vec<u8>,
    /// Latched when `sixel_buf` would exceed the cap: the image is dropped on
    /// finish (correct-or-absent), the scanner keeps running to resync.
    sixel_overflow: bool,
    /// The DCS `P2` (background-select) param captured at the sixel `q`, forwarded
    /// to the decoder.
    pending_sixel_p2: u32,
    /// Physical cell size in px, pushed by the app on font-size / DPI change so
    /// `finish_sixel` can map a decoded image's WxH to a cell footprint. Defaults
    /// to the 8×16 the `EventProxy` reports for `\e[14t`.
    cell_px_w: f32,
    cell_px_h: f32,
    /// Live inline-image placements (decoded sixels), append order ≈ ascending
    /// `abs_line`. Pruned to the scrollback window (span-intersection), dropped on
    /// saturation / reflow (correct-or-absent). Bounded by [`MAX_PLACEMENTS`] and
    /// [`MAX_PLACEMENT_BYTES`].
    placements: VecDeque<ImagePlacement>,
    /// Running sum of `image.rgba.len()` across `placements` (the live-bytes
    /// budget), maintained incrementally so pruning never re-sums the deque.
    placement_bytes: u64,
    /// Live cell pixel size shared with the `EventProxy` (cell_w<<16 | cell_h) so
    /// the `\e[14t` reply reports real metrics (A5). Updated by `set_cell_px`.
    cell_px: Arc<AtomicU32>,
    /// A clone of the PTY write-back sender so the scanner (`&mut self`) can enqueue
    /// Kitty graphics OK/error replies onto the same `pty_write_rx` the app drains.
    reply_tx: std::sync::mpsc::Sender<Vec<u8>>,
    /// Raw control+base64 bytes of the CURRENT Kitty APC, accumulated while in
    /// `Scan::Apc`, capped at [`APC_MAX_BYTES`]. Persists across `feed` chunk
    /// boundaries (like `sixel_buf`).
    apc_buf: Vec<u8>,
    /// Latched when `apc_buf` would exceed the cap (or a CAN/SUB abort): the APC is
    /// dropped on finish (correct-or-absent).
    apc_overflow: bool,
    /// Accumulated RAW (post-base64, post-inflate-input) bytes across `m=1`
    /// continuation chunks, bounded by [`KITTY_RAW_BUDGET`]. Empty when no
    /// multi-chunk transmit is in progress.
    chunk_buf: Vec<u8>,
    /// Control keys captured from the FIRST chunk of a multi-chunk transmit; drives
    /// the final decode/dispatch. `None` when no accumulation is in progress.
    chunk_meta: Option<KittyCmd>,
    /// Count of chunks accumulated so far (bounds an endless-`m=1` stream).
    chunk_count: u32,
    /// Bounded transmit-then-put image registry: `(i=/I= key, decoded image)`.
    /// A later `a=p,i=N` displays without re-transmitting. LRU-evicted by count
    /// and bytes ([`MAX_KITTY_STORED`] / [`MAX_KITTY_STORED_BYTES`]).
    kitty_images: VecDeque<(u32, Arc<crate::sixel::InlineImage>)>,
    /// Running sum of `rgba.len()` across `kitty_images` (the registry byte budget).
    kitty_stored_bytes: u64,
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
        let scrollback_limit = 10_000;
        // Default to write-only OSC 52 (alacritty's secure default): remote copy is
        // accepted, remote paste is denied. `set_osc52_allow_paste` flips this to
        // CopyPaste when the user opts in. Both `new` and `set_scrollback_lines`
        // build the Config with THIS value so a scrollback change never reverts it.
        let osc52_mode = Osc52::OnlyCopy;
        let config =
            Config { scrolling_history: scrollback_limit, osc52: osc52_mode, ..Default::default() };
        let (tx, pty_write_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // Clone the sender for the synchronous scanner path (Kitty graphics
        // OK/error replies flow out through the same drain as async proxy replies).
        let reply_tx = tx.clone();

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
        // Default cell px = 8×16 (matches the pre-set_cell_px \e[14t fallback).
        let cell_px = Arc::new(AtomicU32::new((8u32 << 16) | 16));
        let theme_shared = Arc::new(Mutex::new(theme.clone()));
        let title_update = Arc::new(Mutex::new(None));
        let title_dirty = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));
        let clipboard_store = Arc::new(Mutex::new(None));
        let clipboard_dirty = Arc::new(AtomicBool::new(false));
        let clipboard_load = Arc::new(Mutex::new(None));
        let clipboard_load_dirty = Arc::new(AtomicBool::new(false));
        let proxy = EventProxy {
            tx,
            geom: Arc::clone(&geom),
            cell_px: Arc::clone(&cell_px),
            theme: Arc::clone(&theme_shared),
            child_exited: Arc::clone(&child_exited),
            title_update: Arc::clone(&title_update),
            title_dirty: Arc::clone(&title_dirty),
            bell: Arc::clone(&bell),
            clipboard_store: Arc::clone(&clipboard_store),
            clipboard_dirty: Arc::clone(&clipboard_dirty),
            clipboard_load: Arc::clone(&clipboard_load),
            clipboard_load_dirty: Arc::clone(&clipboard_load_dirty),
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
            clipboard_store,
            clipboard_dirty,
            clipboard_load,
            clipboard_load_dirty,
            osc52_mode,
            search_query: String::new(),
            search_regex: None,
            search_matches: Vec::new(),
            search_current: 0,
            abs_top: 0,
            scrollback_limit,
            saturated: false,
            scan: Scan::Ground,
            marks: VecDeque::new(),
            completed: Vec::new(),
            sixel_buf: Vec::new(),
            sixel_overflow: false,
            pending_sixel_p2: 0,
            // Matches the EventProxy's default \e[14t reply (8×16) until the app
            // pushes real metrics via `set_cell_px` on the first reflow.
            cell_px_w: 8.0,
            cell_px_h: 16.0,
            placements: VecDeque::new(),
            placement_bytes: 0,
            cell_px,
            reply_tx,
            apc_buf: Vec::new(),
            apc_overflow: false,
            chunk_buf: Vec::new(),
            chunk_meta: None,
            chunk_count: 0,
            kitty_images: VecDeque::new(),
            kitty_stored_bytes: 0,
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

    /// Take the pending OSC 52 clipboard-COPY text, if any. `None` in the common
    /// case (a lock-free flag check — zero idle cost). Consuming; multiple copies
    /// between calls coalesce last-wins. The app writes the returned text to the
    /// system clipboard (jetty-core does not depend on the clipboard backend).
    pub fn take_clipboard_store(&mut self) -> Option<String> {
        if !self.clipboard_dirty.swap(false, Ordering::Acquire) {
            return None;
        }
        self.clipboard_store.lock().unwrap().take()
    }

    /// Take the pending OSC 52 clipboard-PASTE reply formatter, if any. `None` in the
    /// common case (a lock-free flag check). Only ever `Some` when the terminal was
    /// built/toggled to permit paste (`osc52_allow_paste`), so the default (write-
    /// only) build never yields one. The app reads the system clipboard, caps it,
    /// calls the formatter, and writes the reply to the PTY.
    pub fn take_clipboard_load(&mut self) -> Option<ClipboardLoadFmt> {
        if !self.clipboard_load_dirty.swap(false, Ordering::Acquire) {
            return None;
        }
        self.clipboard_load.lock().unwrap().take()
    }

    /// Enable or disable OSC 52 clipboard PASTE (remote READ of the local clipboard).
    /// Copy (write) is always permitted. Paste is a SECURITY trade-off (a remote host
    /// / stray output can exfiltrate the clipboard), so it is OFF by default; the
    /// `osc52_allow_paste` config key opts in. Rebuilds the alacritty `Config`
    /// preserving the current scrollback limit. Idempotent-ish (re-applies set_options
    /// even when unchanged), so callers may invoke it unconditionally at tab spawn.
    pub fn set_osc52_allow_paste(&mut self, allow: bool) {
        self.osc52_mode = if allow { Osc52::CopyPaste } else { Osc52::OnlyCopy };
        self.term.set_options(Config {
            scrolling_history: self.scrollback_limit,
            osc52: self.osc52_mode,
            ..Default::default()
        });
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
        // Preserve the OSC 52 mode: `..Default::default()` would reset `osc52` to
        // OnlyCopy, silently reverting an enabled `osc52_allow_paste` on every
        // scrollback change (amendment O2). Carry the stored mode through.
        self.term.set_options(Config {
            scrolling_history: lines,
            osc52: self.osc52_mode,
            ..Default::default()
        });
        // Keep the saturation model in lockstep with the live cap. A shrink can
        // pull us to/over the (smaller) cap; a grow can lift us back under it and
        // re-enable exact tracking for future marks.
        self.scrollback_limit = lines;
        self.refresh_saturation();
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
        self.prune_placements(history);
        // Drop any in-progress Kitty chunk accumulation across a scrollback change.
        self.reset_kitty_chunks();
    }

    /// Feed PTY bytes to the terminal, intercepting OSC 133 semantic-prompt
    /// marks AND sixel DCS images on the way through (alacritty_terminal 0.26 /
    /// vte 0.15 drop both — the DCS never moves the cursor, so JeTTY reserves the
    /// image's cell rows itself; see `finish_sixel`).
    ///
    /// SPEED (#1): in `Ground` this is one `memchr(ESC)` per feed with zero
    /// per-byte work; a stream carrying no escapes reaches `advance_slice` exactly
    /// once (the whole buffer). Only inside an escape does the per-byte state
    /// machine run. Each input byte reaches alacritty exactly once (`start` is
    /// the first un-flushed byte); the scanner sub-advances alacritty up to AND
    /// INCLUDING a sequence's terminator so the grid is caught up before the
    /// cursor line is read, then decodes/places the sixel (or drops the 133).
    /// The sixel payload IS still fed to alacritty (which ignores it) so its own
    /// parser walks the DCS in lockstep and stays consistent.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut i = 0;
        let mut start = 0; // first byte not yet handed to alacritty
        while i < bytes.len() {
            if matches!(self.scan, Scan::Ground) {
                match memchr::memchr(0x1b, &bytes[i..]) {
                    None => break, // no more escapes: flush the tail after the loop
                    Some(off) => {
                        i += off + 1; // step past the ESC
                        self.scan = Scan::Esc;
                        continue;
                    }
                }
            }
            let b = bytes[i];
            match self.scan {
                // Ground is handled by the memchr fast path above.
                Scan::Ground => unreachable!(),
                Scan::Esc => {
                    self.scan = match b {
                        0x5d => Scan::Prefix { n: 0 }, // ']' opens an OSC
                        0x50 => Scan::DcsParams { p2: 0, field: 0, inter: false }, // 'P' opens a DCS
                        0x5f => Scan::ApcIntro,        // '_' opens an APC (Kitty graphics)
                        0x1b => Scan::Esc,             // ESC ESC: restart escape scan
                        _ => Scan::Ground,             // some other escape; resync
                    };
                    i += 1;
                }
                Scan::Prefix { n } => {
                    match b {
                        // A bare ESC aborts this OSC AND begins a new escape (vte
                        // parity) — so `ESC]133; <ESC> ]133;A BEL` still binds A.
                        0x1b => self.scan = Scan::Esc,
                        // OSC ended before matching `133;` (e.g. `ESC]133 BEL`).
                        0x07 | 0x18 | 0x1a => self.scan = Scan::Ground,
                        _ if b == OSC133_PREFIX[n as usize] => {
                            let n2 = n + 1;
                            self.scan = if n2 as usize == OSC133_PREFIX.len() {
                                Scan::Payload {
                                    letter: 0,
                                    code: None,
                                    in_code: false,
                                    code_done: false,
                                }
                            } else {
                                Scan::Prefix { n: n2 }
                            };
                        }
                        // Some other OSC (title/hyperlink/color): skip to its end.
                        _ => self.scan = Scan::Skip,
                    }
                    i += 1;
                }
                Scan::Payload { letter, code, in_code, code_done } => match b {
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
                        self.scan = if is_esc { Scan::Esc } else { Scan::Ground };
                        start = k;
                        i = k;
                    }
                    b';' => {
                        // First `;` opens the code field; a SECOND `;` closes it so
                        // `aid=<n>` (p10k) never bleeds into the exit code.
                        self.scan = Scan::Payload {
                            letter,
                            code,
                            in_code: true,
                            code_done: in_code || code_done,
                        };
                        i += 1;
                    }
                    b'0'..=b'9' if in_code && !code_done => {
                        // FULLY saturating, then clamp to the 0..=255 byte range a
                        // shell exit status actually occupies (POSIX wait status is
                        // 8-bit; signals show as 128+signum, still < 256). A crafted
                        // `\e]133;D;9999999999\a` must neither panic (overflow-checks
                        // on in dev) nor wrap to a garbage/negative code in release —
                        // it clamps to 255 (nonzero → still classified "failed").
                        let next = code
                            .unwrap_or(0)
                            .saturating_mul(10)
                            .saturating_add((b - b'0') as u32)
                            .min(EXIT_CODE_MAX);
                        self.scan = Scan::Payload { letter, code: Some(next), in_code, code_done };
                        i += 1;
                    }
                    _ => {
                        // First byte after `133;` is the A/B/C/D letter. A
                        // non-digit inside the code field (e.g. `k=v`) makes the
                        // exit code unknown (None), closed so trailing digits do
                        // not resurrect it.
                        self.scan = if letter == 0 && !in_code {
                            Scan::Payload { letter: b, code, in_code, code_done }
                        } else if in_code && !code_done {
                            Scan::Payload { letter, code: None, in_code, code_done: true }
                        } else {
                            Scan::Payload { letter, code, in_code, code_done }
                        };
                        i += 1;
                    }
                },
                Scan::Skip => {
                    self.scan = match b {
                        0x1b => Scan::Esc,             // ST: ends this OSC, new escape
                        0x07 | 0x18 | 0x1a => Scan::Ground,
                        _ => Scan::Skip,
                    };
                    i += 1;
                }
                // Inside `ESC P`, collecting P1;P2;P3 up to the final byte. Mirrors
                // vte's DcsEntry/DcsParam/DcsIntermediate tables: digits fold into
                // the current param, `;`/`:` advance/subdivide it, `0x20..=0x2F` is
                // an intermediate (⇒ not sixel), `0x40..=0x7E` is the final byte.
                Scan::DcsParams { p2, field, inter } => {
                    match b {
                        b'0'..=b'9' => {
                            // Fold digits into the current field; only P2 is kept
                            // (the background-select param the decoder wants).
                            let p2 = if field == 1 {
                                p2.saturating_mul(10).saturating_add((b - b'0') as u32)
                            } else {
                                p2
                            };
                            self.scan = Scan::DcsParams { p2, field, inter };
                            i += 1;
                        }
                        b';' => {
                            self.scan = Scan::DcsParams { p2, field: field.saturating_add(1), inter };
                            i += 1;
                        }
                        // `:` subparam — advance no field, keep scanning (vte parity).
                        b':' => {
                            i += 1;
                        }
                        // Intermediate byte ⇒ DECRQSS (`$q`) / XTGETTCAP (`+q`) etc.,
                        // never a bare sixel.
                        0x20..=0x2f => {
                            self.scan = Scan::DcsParams { p2, field, inter: true };
                            i += 1;
                        }
                        // Final byte: a bare `q` (0x71) with NO intermediates is a
                        // sixel; anything else is some other DCS we skip.
                        0x40..=0x7e => {
                            if b == b'q' && !inter {
                                self.sixel_buf.clear();
                                self.sixel_overflow = false;
                                self.pending_sixel_p2 = p2;
                                self.scan = Scan::Sixel;
                            } else {
                                self.scan = Scan::DcsOther;
                            }
                            i += 1;
                        }
                        // CAN/SUB abort to Ground; ESC begins a new escape (vte
                        // `anywhere`). Other C0 bytes are ignored (stay).
                        0x18 | 0x1a => {
                            self.scan = Scan::Ground;
                            i += 1;
                        }
                        0x1b => {
                            self.scan = Scan::Esc;
                            i += 1;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
                // Accumulating raw sixel data until a DCS terminator. BEL is DATA
                // here (unlike OSC) — only CAN/SUB/ESC/8-bit-ST terminate.
                Scan::Sixel => match b {
                    0x18 | 0x1a | 0x9c => {
                        // CAN/SUB/8-bit-ST: flush the DCS to alacritty (it ignores
                        // it), decode + place, return to Ground.
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.finish_sixel();
                        self.scan = Scan::Ground;
                        start = k;
                        i = k;
                    }
                    0x1b => {
                        // 7-bit ST is `ESC \`: ESC ends the DCS and begins a new
                        // escape (the trailing `\` is consumed in Esc → Ground).
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.finish_sixel();
                        self.scan = Scan::Esc;
                        start = k;
                        i = k;
                    }
                    _ => {
                        // Data byte: accumulate up to the cap, then latch overflow
                        // and stop pushing (keep scanning to resync at the terminator).
                        if self.sixel_buf.len() < SIXEL_MAX_BYTES {
                            self.sixel_buf.push(b);
                        } else {
                            self.sixel_overflow = true;
                        }
                        i += 1;
                    }
                },
                // A non-sixel DCS (DECRQSS/XTGETTCAP/…): skip to the terminator,
                // accumulate nothing, emit nothing. Same terminators as `Sixel`.
                Scan::DcsOther => match b {
                    0x18 | 0x1a | 0x9c => {
                        self.scan = Scan::Ground;
                        i += 1;
                    }
                    0x1b => {
                        self.scan = Scan::Esc;
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                },
                // Saw `ESC _`: only `G` (0x47) is a Kitty graphics command; any
                // other APC use is skipped (touch nothing).
                Scan::ApcIntro => {
                    match b {
                        0x47 => {
                            self.apc_buf.clear();
                            self.apc_overflow = false;
                            self.scan = Scan::Apc;
                        }
                        0x1b => self.scan = Scan::Esc, // ESC aborts, new escape
                        0x18 | 0x1a => self.scan = Scan::Ground, // CAN/SUB abort
                        _ => self.scan = Scan::ApcOther,
                    }
                    i += 1;
                }
                // Accumulating a Kitty APC's control+payload until a terminator.
                // BEL is DATA here (like a DCS). ST/8-bit-ST finish; CAN/SUB abort.
                Scan::Apc => match b {
                    0x9c => {
                        // 8-bit ST: real terminator — flush the APC to vte (which
                        // swallows it), then decode/place.
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.finish_kitty_apc();
                        self.scan = Scan::Ground;
                        start = k;
                        i = k;
                    }
                    0x18 | 0x1a => {
                        // CAN/SUB: abort — force the drop path in finish.
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.apc_overflow = true;
                        self.finish_kitty_apc();
                        self.scan = Scan::Ground;
                        start = k;
                        i = k;
                    }
                    0x1b => {
                        // 7-bit ST is `ESC \`: ESC ends the APC (the trailing `\`
                        // is consumed in Esc → Ground).
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.finish_kitty_apc();
                        self.scan = Scan::Esc;
                        start = k;
                        i = k;
                    }
                    _ => {
                        if self.apc_buf.len() < APC_MAX_BYTES {
                            self.apc_buf.push(b);
                        } else {
                            self.apc_overflow = true;
                        }
                        i += 1;
                    }
                },
                // A non-`_G` APC: skip to the terminator, touch nothing.
                Scan::ApcOther => match b {
                    0x18 | 0x1a | 0x9c => {
                        self.scan = Scan::Ground;
                        i += 1;
                    }
                    0x1b => {
                        self.scan = Scan::Esc;
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                },
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
        // Once the ring saturates, `history_size()` pins at the cap while real
        // scroll keeps happening, so the abs_top delta above under-counts (this
        // very feed can already have scrolled a mark off the counted range). We
        // cannot recover the lost count — so latch and drop marks rather than
        // render them on drifting rows. See `refresh_saturation`.
        self.refresh_saturation();
    }

    /// Recompute the saturation latch from the live history depth. On the
    /// transition into saturation, purge marks (any of them may already have
    /// drifted). No-op while on the alt screen (its grid has no primary
    /// scrollback; saturation of the primary is re-evaluated on return).
    fn refresh_saturation(&mut self) {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        // `history_size()` is capped at `scrollback_limit`, so `>=` fires exactly
        // when the ring is full. A `scrollback_limit` of 0 (no scrollback) is
        // "always saturated": scrolled-off lines are lost, so marks cannot be
        // tracked — correctly disabling the feature.
        let now_saturated = self.term.grid().history_size() >= self.scrollback_limit;
        if now_saturated && !self.saturated {
            self.marks.clear();
            // Image anchors become untrustworthy for the same reason marks do
            // (abs_top can no longer count scrolled-off lines) — drop them.
            self.clear_placements();
        }
        self.saturated = now_saturated;
    }

    /// Handle a non-scroll history shrink on the PRIMARY screen (a destructive
    /// reset `RIS`/`\ec`, or a scrollback clear `\e[3J`): `Line(0)` does not move,
    /// so `abs_top` stays monotonic; drop marks whose line no longer exists. The
    /// next prompt re-marks.
    fn on_history_shrunk(&mut self, history_size: usize) {
        self.prune_marks(history_size);
        self.prune_placements(history_size);
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

    /// Empty the placement list and reset the live-bytes counter. Used on
    /// saturation / reflow (correct-or-absent).
    fn clear_placements(&mut self) {
        self.placements.clear();
        self.placement_bytes = 0;
    }

    /// Drop placements whose entire row SPAN lies outside the live window
    /// `[abs_top - history_size, abs_top + rows)`, then enforce the count / bytes
    /// caps (drop oldest). Unlike `prune_marks` (a single-line predicate) this is
    /// a SPAN intersection because an image occupies `rows` rows — an image is
    /// kept iff `abs_line + rows > min_abs && abs_line < max_abs`, the SAME test
    /// `visible_images` uses (kept consistent on purpose).
    fn prune_placements(&mut self, history_size: usize) {
        let min_abs = self.abs_top - history_size as i64;
        let max_abs = self.abs_top + self.rows as i64;
        let mut bytes = self.placement_bytes;
        self.placements.retain(|p| {
            let keep = p.abs_line + p.rows as i64 > min_abs && p.abs_line < max_abs;
            if !keep {
                bytes = bytes.saturating_sub(p.image.rgba.len() as u64);
            }
            keep
        });
        // Enforce the count + live-bytes budget, dropping the OLDEST first.
        while self.placements.len() > MAX_PLACEMENTS || bytes > MAX_PLACEMENT_BYTES {
            let Some(old) = self.placements.pop_front() else { break };
            bytes = bytes.saturating_sub(old.image.rgba.len() as u64);
        }
        self.placement_bytes = bytes;
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
        // Past scrollback saturation we can no longer place a mark's row reliably;
        // refuse to bind (absent) rather than record a mark that will drift.
        if self.saturated {
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
                    started_at: None,
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
                    // Command START: stamp the monotonic clock so `D` can compute a
                    // duration. One `Instant::now()`, once per command, on the
                    // already-off-hot-path OSC-133 scanner (never per byte).
                    last.started_at = Some(std::time::Instant::now());
                }
            }
            b'D' => {
                // Bind to the most-recent still-open block (shells emit strictly
                // A…B…C…D, so "most recent open" is correct even with gaps).
                let mut done = false;
                let mut duration = None;
                if let Some(block) = self.marks.iter_mut().rev().find(|m| !m.finished) {
                    block.exit = exit;
                    block.finished = true;
                    // `Some(elapsed)` iff a C was seen this block; `None` otherwise
                    // (bash without preexec) → the completion reports unknown time.
                    duration = block.started_at.map(|t| t.elapsed());
                    done = true;
                }
                // Emit a completion ONLY when a matching open block existed (a
                // spurious lone D produces nothing). The mutable `marks` borrow
                // above has ended, so reading the grid / pushing is conflict-free.
                if done {
                    let last_line = self.last_output_line();
                    self.completed.push(CommandCompletion { exit_code: exit, duration, last_line });
                    // Bound undrained completions; drop the oldest on overflow.
                    if self.completed.len() > MAX_PENDING_COMPLETIONS {
                        self.completed.remove(0);
                    }
                }
            }
            _ => {} // unknown 133 sub-command: ignore
        }
    }

    /// Push the physical cell size (px) so `finish_sixel` maps a decoded image's
    /// WxH to a cell footprint. Called by the app from its single reflow
    /// chokepoint on every font-size / DPI / window-size change.
    pub fn set_cell_px(&mut self, w: f32, h: f32) {
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            self.cell_px_w = w;
            self.cell_px_h = h;
            // Publish the rounded metric to the shared atomic so the EventProxy's
            // `\e[14t` reply reports real cell px (A5). Clamp into u16 each.
            let cw = (w.round() as u32).clamp(1, u16::MAX as u32);
            let ch = (h.round() as u32).clamp(1, u16::MAX as u32);
            self.cell_px.store((cw << 16) | ch, Ordering::Relaxed);
        }
    }

    /// Finish a sixel DCS at its terminator: decode the accumulated bytes, reserve
    /// its cell rows by injecting line-feeds (so alacritty scrolls + grows history
    /// and `abs_top` tracks the image for free), and record the placement.
    ///
    /// Correct-or-absent guards (drop, touch nothing): a buffer overflow, the alt
    /// screen, an already-saturated ring, an ACTIVE synchronized-update block
    /// (mode 2026 — the cursor is not yet at its post-flush position, so the
    /// anchor would be wrong), a zero cell metric, a decode failure, or a
    /// saturation flip caused by the reserve injection itself.
    fn finish_sixel(&mut self) {
        let buf = std::mem::take(&mut self.sixel_buf);
        let overflow = std::mem::take(&mut self.sixel_overflow);
        let p2 = self.pending_sixel_p2;

        if overflow
            || self.term.mode().contains(TermMode::ALT_SCREEN)
            || self.saturated
            // A sixel emitted inside a DECSET-2026 sync block anchors at the wrong
            // row (vte is buffering; the cursor hasn't advanced). Drop it (P2).
            || self.sync_deadline().is_some()
            || self.cell_px_w <= 0.0
            || self.cell_px_h <= 0.0
        {
            return;
        }

        let Some(img) = crate::sixel::decode_sixel(p2, &buf, crate::sixel::SIXEL_CAPS) else {
            return;
        };

        // Footprint in cells (ceil), clamped to the grid width and a row cap.
        let cols = ((img.width as f32 / self.cell_px_w).ceil() as usize).clamp(1, self.cols) as u16;
        let rows = ((img.height as f32 / self.cell_px_h).ceil() as usize)
            .clamp(1, MAX_IMAGE_ROWS) as u16;

        // Anchor at the CURRENT cursor ROW (alacritty ignored the DCS, so it is
        // still the image's top) — captured BEFORE injecting the reserve scroll,
        // so it reuses the FIXED abs_top model exactly like `bind_mark`. The image
        // starts at COLUMN 0 because the reserve injects a leading CR (below),
        // matching most sixel terminals; so the anchor column is 0.
        let cur = self.term.grid().cursor.point;
        let abs_line = self.abs_top + cur.line.0 as i64;
        let col = 0u16;

        // Reserve vertical space: a CR to start the image at column 0 (matching
        // most sixel terminals, and so a mid-line cursor doesn't overlap the
        // image), then `rows` line-feeds through the same `advance_slice` path so
        // alacritty scrolls, grows history, and `abs_top` tracks automatically.
        let mut reserve = Vec::with_capacity(1 + rows as usize * 2);
        reserve.push(b'\r');
        for _ in 0..rows {
            reserve.push(b'\r');
            reserve.push(b'\n');
        }
        self.advance_slice(&reserve);

        // The reserve injection can have pushed history to the cap and flipped the
        // saturation latch mid-call; if so, the anchor is no longer trustworthy —
        // treat the image as absent (do not record).
        if self.saturated {
            return;
        }

        let id = crate::sixel::content_id(&img);
        let bytes = img.rgba.len() as u64;
        self.placements.push_back(ImagePlacement {
            id,
            abs_line,
            col,
            cols,
            rows,
            px_w: img.width.min(u16::MAX as u32) as u16,
            px_h: img.height.min(u16::MAX as u32) as u16,
            image: Arc::new(img),
            kitty_id: None,
        });
        self.placement_bytes = self.placement_bytes.saturating_add(bytes);
        let history = self.term.grid().history_size();
        self.prune_placements(history);
    }

    // ─────────────────────────── Kitty graphics (APC ESC _ G) ────────────────

    /// Clear any in-progress cross-APC chunk accumulation (the highest-risk state).
    /// Called on abort/overflow/interrupt and on context changes (reflow /
    /// scrollback change) so a partial transmit can never splice or outlive its
    /// context. The bounded registry (`kitty_images`) survives — it is separately
    /// LRU-capped and a later `a=p` may legitimately reference it.
    fn reset_kitty_chunks(&mut self) {
        self.chunk_buf.clear();
        self.chunk_meta = None;
        self.chunk_count = 0;
    }

    /// Decode ONE chunk's base64 payload and append the RAW bytes to `chunk_buf`,
    /// bounded by [`KITTY_RAW_BUDGET`] (amendment BLOCKING 2 — accumulate raw, not
    /// base64, so a full-window `f=32` image fits). Returns `false` on a base64
    /// failure or budget overrun (caller aborts).
    fn accumulate_chunk(&mut self, payload: &[u8]) -> bool {
        let budget = KITTY_RAW_BUDGET.saturating_sub(self.chunk_buf.len());
        let Some(raw) = crate::base64::decode_base64(payload, budget) else {
            return false;
        };
        if self.chunk_buf.len().saturating_add(raw.len()) > KITTY_RAW_BUDGET {
            return false;
        }
        self.chunk_buf.extend_from_slice(&raw);
        true
    }

    /// Finish a Kitty APC at its terminator: run the cross-chunk state machine,
    /// then decode/place/store/delete/reply. `apc_overflow` (buffer cap OR a
    /// CAN/SUB abort) drops everything (correct-or-absent).
    ///
    /// Chunk state machine (amendment BLOCKING 1): continuation chunks OMIT the
    /// action key (`has_action == false`). While an accumulation is in progress,
    /// only a `!has_action` APC appends/finalizes; ANY `has_action` APC ABORTS the
    /// partial and is handled fresh — never spliced.
    fn finish_kitty_apc(&mut self) {
        let buf = std::mem::take(&mut self.apc_buf);
        let overflow = std::mem::take(&mut self.apc_overflow);

        if overflow {
            // A too-large or CAN/SUB-aborted APC drops itself AND any in-progress
            // accumulation (the abort could be mid-stream).
            self.reset_kitty_chunks();
            return;
        }

        // Split control | ';' payload at the FIRST ';'. No `;` ⇒ control-only
        // (a delete/query/put with no payload).
        let (control, payload): (&[u8], &[u8]) = match buf.iter().position(|&c| c == b';') {
            Some(p) => (&buf[..p], &buf[p + 1..]),
            None => (&buf[..], &[]),
        };
        let cmd = KittyCmd::parse(control);

        if self.chunk_meta.is_some() {
            if !cmd.has_action {
                // Continuation chunk: append this chunk's RAW payload.
                if !self.accumulate_chunk(payload) {
                    self.reset_kitty_chunks();
                    return;
                }
                self.chunk_count = self.chunk_count.saturating_add(1);
                if self.chunk_count > MAX_KITTY_CHUNKS {
                    self.reset_kitty_chunks();
                    return;
                }
                if cmd.more == 0 {
                    // Finalize under the STORED first-chunk meta.
                    let meta = self.chunk_meta.take().unwrap();
                    let raw = std::mem::take(&mut self.chunk_buf);
                    self.chunk_count = 0;
                    self.handle_kitty_command(meta, raw);
                }
                return;
            }
            // has_action while accumulating ⇒ ABORT the partial, then handle
            // `cmd` fresh below (never splice).
            self.reset_kitty_chunks();
        }

        // Fresh command. A first chunk (`m=1` WITH an action) starts accumulation.
        // An orphan continuation (`m=1`, no action, nothing in progress) is a
        // stray fragment — ignore it (this also caps an endless-`m=1` stream that
        // has already aborted its accumulation).
        if cmd.more == 1 {
            if !cmd.has_action {
                return;
            }
            self.reset_kitty_chunks();
            self.chunk_meta = Some(cmd);
            self.chunk_count = 1;
            if !self.accumulate_chunk(payload) {
                self.reset_kitty_chunks();
            }
            return;
        }

        // Single-shot: base64-decode the payload now (bounded), then dispatch.
        match crate::base64::decode_base64(payload, KITTY_RAW_BUDGET) {
            Some(raw) => self.handle_kitty_command(cmd, raw),
            None => self.kitty_reply(&cmd, "EBADF"),
        }
    }

    /// Dispatch a finalized Kitty command with its RAW (base64-decoded) payload.
    /// `raw` is meaningful only for transmit/query; delete/put ignore it.
    fn handle_kitty_command(&mut self, cmd: KittyCmd, raw: Vec<u8>) {
        match cmd.action {
            b'd' => self.kitty_delete(&cmd),
            b'p' => self.kitty_put(&cmd),
            b'q' => {
                // Validate/decode WITHOUT displaying or storing, then reply.
                if cmd.medium != b'd' {
                    self.kitty_reply(&cmd, "ENOTSUPP");
                } else if decode_kitty_image(&cmd, raw).is_some() {
                    self.kitty_reply(&cmd, "OK");
                } else {
                    self.kitty_reply(&cmd, "EBADF");
                }
            }
            b't' | b'T' => {
                // Only direct base64 transmission is supported (safety: an
                // untrusted PTY must not make us open files / shm).
                if cmd.medium != b'd' {
                    self.kitty_reply(&cmd, "ENOTSUPP");
                    return;
                }
                match decode_kitty_image(&cmd, raw) {
                    Some(img) => {
                        let img = Arc::new(img);
                        // Transmit: store in the registry if addressable.
                        if cmd.id != 0 || cmd.number != 0 {
                            self.kitty_store(&cmd, img.clone());
                        }
                        // Display on `a=T`.
                        if cmd.action == b'T' {
                            self.place_inline_image(&img, &cmd);
                        }
                        self.kitty_reply(&cmd, "OK");
                    }
                    None => self.kitty_reply(&cmd, "EBADF"),
                }
            }
            // Empty `a=` (action 0) is a malformed command.
            0 => self.kitty_reply(&cmd, "EINVAL"),
            // Animation (`a=a`/`a=f`) and any other action: documented non-goal.
            _ => self.kitty_reply(&cmd, "ENOTSUPP"),
        }
    }

    /// Store a decoded image in the transmit-then-put registry, replacing any
    /// existing entry with the same key and LRU-evicting to stay within both caps.
    fn kitty_store(&mut self, cmd: &KittyCmd, img: Arc<crate::sixel::InlineImage>) {
        let key = if cmd.id != 0 { cmd.id } else { cmd.number };
        if key == 0 {
            return;
        }
        // Replace an existing same-key entry (free its bytes).
        if let Some(pos) = self.kitty_images.iter().position(|(k, _)| *k == key) {
            if let Some((_, old)) = self.kitty_images.remove(pos) {
                self.kitty_stored_bytes =
                    self.kitty_stored_bytes.saturating_sub(old.rgba.len() as u64);
            }
        }
        self.kitty_stored_bytes = self.kitty_stored_bytes.saturating_add(img.rgba.len() as u64);
        self.kitty_images.push_back((key, img));
        while self.kitty_images.len() > MAX_KITTY_STORED
            || self.kitty_stored_bytes > MAX_KITTY_STORED_BYTES
        {
            let Some((_, old)) = self.kitty_images.pop_front() else { break };
            self.kitty_stored_bytes = self.kitty_stored_bytes.saturating_sub(old.rgba.len() as u64);
        }
    }

    /// Display a previously-transmitted image (`a=p,i=N`/`I=N`). Unknown id ⇒
    /// `ENOENT` and no placement.
    fn kitty_put(&mut self, cmd: &KittyCmd) {
        let key = if cmd.id != 0 { cmd.id } else { cmd.number };
        let found = self
            .kitty_images
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, i)| i.clone());
        match found {
            Some(img) => {
                self.place_inline_image(&img, cmd);
                self.kitty_reply(cmd, "OK");
            }
            None => self.kitty_reply(cmd, "ENOENT"),
        }
    }

    /// Honor the common Kitty delete requests; refuse the exotic ones as a safe
    /// no-op (amendment A6 / T9). Lowercase selectors delete PLACEMENTS only;
    /// uppercase also frees stored image data.
    fn kitty_delete(&mut self, cmd: &KittyCmd) {
        match cmd.delete {
            // `a=d` with no selector, or d=a / d=A: delete all placements. `A`
            // additionally frees stored images.
            0 | b'a' | b'A' => {
                self.clear_placements();
                if cmd.delete == b'A' {
                    self.kitty_images.clear();
                    self.kitty_stored_bytes = 0;
                }
            }
            // d=i / d=I: delete placements whose kitty id matches. `I` also frees
            // the stored image.
            b'i' | b'I' => {
                let key = if cmd.id != 0 { cmd.id } else { cmd.number };
                let mut freed = 0u64;
                self.placements.retain(|p| {
                    let del = p.kitty_id == Some(key);
                    if del {
                        freed += p.image.rgba.len() as u64;
                    }
                    !del
                });
                self.placement_bytes = self.placement_bytes.saturating_sub(freed);
                if cmd.delete == b'I' {
                    if let Some(pos) = self.kitty_images.iter().position(|(k, _)| *k == key) {
                        if let Some((_, old)) = self.kitty_images.remove(pos) {
                            self.kitty_stored_bytes =
                                self.kitty_stored_bytes.saturating_sub(old.rgba.len() as u64);
                        }
                    }
                }
            }
            // Any other selector (by row/column/z/cursor): documented no-op.
            _ => {}
        }
    }

    /// Enqueue a Kitty graphics OK/error reply on the PTY write-back channel,
    /// honoring the addressability + quiet rules (amendment A10):
    /// only reply when the command addresses an image (`i=`/`I=`), and never at
    /// `q>=2`; `q>=1` suppresses OK but still reports errors.
    fn kitty_reply(&mut self, cmd: &KittyCmd, msg: &str) {
        // Reply only for addressable commands — prevents a tiny-APC flood from
        // amplifying 1:1 writes back to a non-reading PTY.
        if !cmd.addressable() {
            return;
        }
        if cmd.quiet >= 2 {
            return;
        }
        if cmd.quiet >= 1 && msg == "OK" {
            return;
        }
        let mut out = Vec::with_capacity(16 + msg.len());
        out.extend_from_slice(b"\x1b_G");
        if cmd.id != 0 {
            out.extend_from_slice(format!("i={}", cmd.id).as_bytes());
        } else {
            out.extend_from_slice(format!("I={}", cmd.number).as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(msg.as_bytes());
        out.extend_from_slice(b"\x1b\\");
        let _ = self.reply_tx.send(out);
    }

    /// Place a decoded Kitty image at the cursor, reusing the sixel reserve-and-
    /// anchor machinery. Diverges from `finish_sixel` in two documented ways:
    /// the image anchors at the CURRENT cursor COLUMN (not forced col 0), and the
    /// reserve omits the sixel leading bare CR. Correct-or-absent guards are
    /// identical (alt screen / saturation / active sync / zero cell metric).
    fn place_inline_image(&mut self, img: &Arc<crate::sixel::InlineImage>, cmd: &KittyCmd) {
        if self.term.mode().contains(TermMode::ALT_SCREEN)
            || self.saturated
            || self.sync_deadline().is_some()
            || self.cell_px_w <= 0.0
            || self.cell_px_h <= 0.0
        {
            return;
        }

        // Cell footprint: explicit c=/r= override, else derive from pixels (ceil).
        let (mut cols, rows) = if cmd.cols > 0 && cmd.rows > 0 {
            (
                (cmd.cols as usize).clamp(1, self.cols) as u16,
                (cmd.rows as usize).clamp(1, MAX_IMAGE_ROWS) as u16,
            )
        } else {
            let c = ((img.width as f32 / self.cell_px_w).ceil() as usize).clamp(1, self.cols) as u16;
            let r = ((img.height as f32 / self.cell_px_h).ceil() as usize)
                .clamp(1, MAX_IMAGE_ROWS) as u16;
            (c, r)
        };
        // Clamp cols to the grid width unconditionally (A8 — avoid an underflow /
        // panic when cols > self.cols or self.cols == 0).
        cols = cols.min(self.cols.max(1) as u16);

        let cur = self.term.grid().cursor.point;
        let abs_line = self.abs_top + cur.line.0 as i64;
        // Anchor at the current cursor column, saturating so it can never exceed
        // the last column that still fits the image (A8).
        let max_col = (self.cols as u16).saturating_sub(cols);
        let col = (cur.column.0 as u16).min(max_col);

        // Reserve `rows` lines via CRLF injection (honoring the cursor column, so
        // no leading bare CR). Feeds the same `advance_slice` path so alacritty
        // scrolls, grows history, and `abs_top` tracks automatically.
        let mut reserve = Vec::with_capacity(rows as usize * 2);
        for _ in 0..rows {
            reserve.push(b'\r');
            reserve.push(b'\n');
        }
        self.advance_slice(&reserve);

        // A mid-call saturation flip makes the anchor untrustworthy — drop.
        if self.saturated {
            return;
        }

        let id = crate::sixel::content_id(img);
        let bytes = img.rgba.len() as u64;
        let key = if cmd.id != 0 {
            cmd.id
        } else {
            cmd.number
        };
        self.placements.push_back(ImagePlacement {
            id,
            abs_line,
            col,
            cols,
            rows,
            px_w: img.width.min(u16::MAX as u32) as u16,
            px_h: img.height.min(u16::MAX as u32) as u16,
            image: img.clone(),
            kitty_id: if key != 0 { Some(key) } else { None },
        });
        self.placement_bytes = self.placement_bytes.saturating_add(bytes);
        let history = self.term.grid().history_size();
        self.prune_placements(history);
    }

    /// Currently-visible inline images mapped to VIEWPORT rows, off the per-cell
    /// snapshot path (SPEED — mirrors `failed_prompt_rows`). Empty on the alt
    /// screen / past saturation. A placement is kept iff its row SPAN intersects
    /// the visible grid `[0, rows)` — the SAME span test as `prune_placements`.
    pub fn visible_images(&self) -> Vec<crate::snapshot::VisibleImage> {
        if self.saturated || self.term.mode().contains(TermMode::ALT_SCREEN) {
            return Vec::new();
        }
        let off = self.term.grid().display_offset() as i64;
        self.placements
            .iter()
            .filter_map(|p| {
                // Viewport row of the image's top-left cell (may be negative).
                let top = (p.abs_line - self.abs_top) + off;
                let bottom = top + p.rows as i64;
                if bottom <= 0 || top >= self.rows as i64 {
                    return None; // span does not intersect the visible grid
                }
                Some(crate::snapshot::VisibleImage {
                    id: p.id,
                    top_row: top as f32,
                    col: p.col,
                    cols: p.cols,
                    rows: p.rows,
                    px_w: p.px_w,
                    px_h: p.px_h,
                })
            })
            .collect()
    }

    /// The decoded RGBA image for a visible placement id (cheap `Arc` clone), so
    /// the render layer can upload it once per window. `None` if the placement was
    /// pruned since `visible_images` was called.
    pub fn image_rgba(&self, id: u64) -> Option<Arc<crate::sixel::SixelImage>> {
        self.placements.iter().find(|p| p.id == id).map(|p| p.image.clone())
    }

    /// Viewport rows (0-based) of currently-visible FAILED-command prompts
    /// (`D;<nonzero>`), for the themed left-edge marker. Empty in the common case
    /// and on the alt screen. Kept OFF the per-cell `GridSnapshot` so the render
    /// hot loop is untouched (SPEED). Uses the SAME `display_offset` mapping as
    /// `snapshot()`.
    pub fn failed_prompt_rows(&self) -> Vec<u16> {
        // On the alt screen, or once scrollback saturation has made mark rows
        // untrustworthy, render nothing (correct-or-absent). `saturated` implies
        // `marks` is already empty, but the guard states the intent.
        if self.saturated || self.term.mode().contains(TermMode::ALT_SCREEN) {
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
        // No target on the alt screen, past saturation (marks untrustworthy /
        // cleared), or with no marks at all.
        if self.saturated || self.term.mode().contains(TermMode::ALT_SCREEN) || self.marks.is_empty()
        {
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
        // A reflow REWRAPS logical lines: a mark's physical row genuinely moves by
        // an amount unrelated to any scroll, so its stored absolute line no longer
        // points at its prompt. `Term::resize` also changes `history_size()`
        // outside `track_abs_top`, breaking the abs_top⇄history relationship. Since
        // the anchors are now meaningless, DROP the marks (correct-or-absent — the
        // next prompt re-marks) and re-establish a clean anchor so future marks and
        // pruning are exact again. Skip the re-anchor on the alt screen: its grid
        // has ~no history, so `history_size()` there would corrupt the primary
        // `abs_top` (which is frozen for the alt session).
        self.marks.clear();
        // Image placements anchor on the same (now-meaningless) reflowed rows, so
        // drop them too (correct-or-absent; a re-emit re-marks). The reserved
        // blank rows remain — harmless — and the GPU textures simply stop being
        // drawn (the ImageLayer's LRU reclaims their VRAM).
        self.clear_placements();
        // A reflow also invalidates any in-progress Kitty chunk accumulation
        // (its anchor context changed) — drop it so it can't splice post-reflow.
        self.reset_kitty_chunks();
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.abs_top = self.term.grid().history_size() as i64;
            self.refresh_saturation();
        }
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

    /// Drain the command completions collected since the last call (OSC 133 `D`
    /// marks discovered during `feed()`). Empty in the common case — a fast
    /// `is_empty` check avoids allocating — so this rides the existing PTY-drain
    /// pass at zero idle cost. Consuming: a second call returns an empty `Vec`.
    pub fn take_completions(&mut self) -> Vec<CommandCompletion> {
        if self.completed.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut self.completed)
        }
    }

    /// Text of the last non-empty grid row at/above the cursor, trimmed and
    /// capped — the notification body. Called ONCE per command (at `D`), never on
    /// the per-byte path. Precmd emits `D` then `A`, so at `D` the cursor sits
    /// just below the command's final output; the bottom-most non-empty row
    /// at/above it is that command's last output line. A wrapped long line
    /// returns only its bottom physical row (acceptable). Only base `cell.c` is
    /// read (same combining-mark limit as `snapshot`).
    fn last_output_line(&self) -> String {
        const MAX_SCAN_ROWS: i32 = 64; // bound the upward walk
        const MAX_CHARS: usize = 200;
        let grid = self.term.grid();
        // Clamp every index into the LIVE grid range before indexing: alacritty's
        // `Grid` panics on an out-of-range `Line` (including negative below the
        // history top), so a near-empty grid or a top-of-history cursor must never
        // reach `grid[Line(l)]` with an invalid `l`.
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let cursor_line = grid.cursor.point.line.0.clamp(top, bottom);
        let lo = (cursor_line - MAX_SCAN_ROWS).max(top);
        for l in (lo..=cursor_line).rev() {
            let row = &grid[Line(l)];
            let mut s = String::new();
            for c in 0..self.cols {
                let cell = &row[Column(c)];
                // Skip the trailing half of a wide (CJK) glyph so the base char
                // isn't doubled; matches the snapshot/URL cell-walk convention.
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                s.push(cell.c);
            }
            let t = s.trim();
            if !t.is_empty() {
                return t.chars().take(MAX_CHARS).collect();
            }
        }
        String::new()
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

    /// Convert a viewport row (0 = top of the visible grid) to its ABSOLUTE
    /// buffer line at the current scroll offset. Copy-mode captures its anchor
    /// this way so the anchor stays pinned to CONTENT — not the viewport — and
    /// scrolling extends the selection into scrollback instead of sliding the
    /// whole selection with the viewport.
    pub fn viewport_line_to_buffer(&self, viewport_line: usize) -> i32 {
        let display_offset = self.term.grid().display_offset();
        viewport_to_point(display_offset, Point::new(viewport_line, Column(0))).line.0
    }

    /// Like [`Terminal::selection_start`] but anchored at an ABSOLUTE buffer
    /// line (independent of the scroll offset), so the anchor does not slide as
    /// the viewport scrolls. `buffer_line` comes from [`viewport_line_to_buffer`].
    pub fn selection_start_abs(&mut self, buffer_line: i32, col: usize, left_half: bool) {
        let pt = Point::new(Line(buffer_line), Column(col));
        let side = if left_half { Side::Left } else { Side::Right };
        self.term.selection = Some(Selection::new(SelectionType::Simple, pt, side));
    }

    /// Update the end of the current selection to an ABSOLUTE buffer cell.
    /// `left_half` is the sub-cell x side (see [`Terminal::selection_start`]).
    pub fn selection_update_abs(&mut self, buffer_line: i32, col: usize, left_half: bool) {
        let pt = Point::new(Line(buffer_line), Column(col));
        let side = if left_half { Side::Left } else { Side::Right };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(pt, side);
        }
    }

    /// Like [`Terminal::selection_start_lines`] but anchored at an ABSOLUTE
    /// buffer line, so a line-mode copy-mode anchor survives scrolling.
    pub fn selection_start_lines_abs(&mut self, buffer_line: i32) {
        let pt = Point::new(Line(buffer_line), Column(0));
        self.term.selection = Some(Selection::new(SelectionType::Lines, pt, Side::Left));
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

    /// Scan every visible URL / file-path / git-hash / IPv4 token for HINT MODE
    /// (Ctrl+Shift+H). Runs ONCE per key press (never per frame): it assembles
    /// each visible logical line (WRAPLINE-joined, wide spacers blanked, exactly
    /// like [`Terminal::link_at`]), [`crate::hints::scan_line`]s it, and maps each
    /// token back to VIEWPORT `(row, col_start, col_end)` spans (visible rows
    /// only). A wrapped token straddling the top (row 0) or bottom (row `rows-1`)
    /// edge extends its WRAPLINE walk BEYOND the viewport (capped at
    /// [`crate::url::MAX_WRAP_WALK`]) so its `text` is the COMPLETE token even
    /// though the label anchors on the visible portion. Identical on-screen
    /// tokens dedup to one entry; the total is capped so the label alphabet stays
    /// short. Like `link_at`, spans are recomputed from a fresh grid every call —
    /// never store the returned viewport coords across grid changes.
    pub fn hint_tokens(&self) -> Vec<HintToken> {
        const TOKEN_CAP: usize = 100;
        if self.cols == 0 || self.rows == 0 {
            return Vec::new();
        }
        let grid = self.term.grid();
        let display_offset = grid.display_offset();
        let last_col = Column(self.cols - 1);
        let wrapped = |l: i32| grid[Line(l)][last_col].flags.contains(Flags::WRAPLINE);
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;

        // Terminal-line range the viewport covers (contiguous).
        let first_vp = viewport_to_point(display_offset, Point::new(0, Column(0))).line.0;
        let last_vp =
            viewport_to_point(display_offset, Point::new(self.rows - 1, Column(0))).line.0;

        // Extend the WRAPLINE walk beyond the viewport at BOTH edges so a token
        // wrapped in from above / out below is assembled in full (BLOCKING 3).
        let mut scan_start = first_vp;
        for _ in 0..crate::url::MAX_WRAP_WALK {
            if scan_start > top && wrapped(scan_start - 1) {
                scan_start -= 1;
            } else {
                break;
            }
        }
        let mut scan_end = last_vp;
        for _ in 0..crate::url::MAX_WRAP_WALK {
            if scan_end < bottom && wrapped(scan_end) {
                scan_end += 1;
            } else {
                break;
            }
        }

        let mut tokens: Vec<HintToken> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut gs = scan_start;
        while gs <= scan_end {
            // Group consecutive WRAPLINE rows into one logical line.
            let mut ge = gs;
            while ge < scan_end && wrapped(ge) {
                ge += 1;
            }
            // Assemble the group's chars: exactly `cols` per row so char index i
            // maps back to cell (gs + i/cols, i % cols); wide spacers → ' '.
            let mut chars: Vec<char> =
                Vec::with_capacity(((ge - gs + 1) as usize) * self.cols);
            for l in gs..=ge {
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
            for (s, e, kind) in crate::hints::scan_line(&chars) {
                // Map to VISIBLE viewport spans first (a fully off-screen token
                // — wrapped entirely above/below — is dropped).
                let mut spans: Vec<(usize, usize, usize)> = Vec::new();
                for idx in s..e {
                    let term_line = gs + (idx / self.cols) as i32;
                    let c = idx % self.cols;
                    if let Some(vp) =
                        point_to_viewport(display_offset, Point::new(Line(term_line), Column(c)))
                    {
                        if vp.line < self.rows {
                            match spans.last_mut() {
                                Some(sp) if sp.0 == vp.line && sp.2 + 1 == c => sp.2 = c,
                                _ => spans.push((vp.line, c, c)),
                            }
                        }
                    }
                }
                if spans.is_empty() {
                    continue;
                }
                let text: String = chars[s..e].iter().collect();
                if !seen.insert(text.clone()) {
                    continue; // dedup identical on-screen tokens
                }
                tokens.push(HintToken { text, kind, spans });
                if tokens.len() >= TOKEN_CAP {
                    return tokens;
                }
            }
            gs = ge + 1;
        }
        tokens
    }

    /// The visible viewport as rows-of-chars (`rows` × `cols`, wide spacers
    /// blanked to `' '`, blank cells `' '`). Used by copy-mode word motions so
    /// `w`/`b`/`e` can see neighbouring rows (BLOCKING 4). Keystroke-rate only.
    pub fn viewport_rows_chars(&self) -> Vec<Vec<char>> {
        let mut rows = vec![vec![' '; self.cols]; self.rows];
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        for item in content.display_iter {
            if let Some(vp) = point_to_viewport(display_offset, item.point) {
                if vp.line < self.rows && vp.column.0 < self.cols {
                    let cell = item.cell;
                    let c = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        ' '
                    } else {
                        cell.c
                    };
                    rows[vp.line][vp.column.0] = c;
                }
            }
        }
        rows
    }

    /// Start a whole-LINE selection at the given viewport row (copy-mode `V`).
    /// Builds a `SelectionType::Lines` selection; `selection_update` then extends
    /// it to the cursor's row. Any prior selection is replaced.
    pub fn selection_start_lines(&mut self, viewport_line: usize) {
        let display_offset = self.term.grid().display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(0)));
        self.term.selection = Some(Selection::new(SelectionType::Lines, pt, Side::Left));
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
    fn abs_selection_anchor_survives_scroll_into_history() {
        // Regression (v0.21 copy-mode): with the anchor captured as an ABSOLUTE
        // buffer line, scrolling the viewport while selecting must EXTEND the
        // selection into scrollback — not slide the whole thing with the viewport
        // (which would cap it at one screen height). Feed 50 lines into a 5-row
        // screen so plenty of history exists.
        let mut t = Terminal::new(20, 5);
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        // Anchor on the last visible row at the live bottom (viewport row 4).
        let anchor_line = t.viewport_line_to_buffer(4);
        t.selection_start_abs(anchor_line, 0, true);
        // Scroll two screens up into history; the copy-mode cursor stays at the
        // top viewport row, whose buffer line is now ABOVE the anchor.
        t.scroll_lines(10);
        let cursor_line = t.viewport_line_to_buffer(0);
        assert!(
            cursor_line < anchor_line,
            "after scrolling, the cursor's buffer line ({cursor_line}) must be above the anchor ({anchor_line})"
        );
        t.selection_update_abs(cursor_line, 19, false);
        let text = t.selection_text().expect("selection should have text after scrolling");
        // The selection must span MORE than one 5-row screen — the whole point of
        // a content-pinned anchor is multi-screen scrollback selection.
        let n = text.lines().count();
        assert!(n > 5, "content-pinned selection must span >1 screen; got {n} lines: {text:?}");
        assert!(text.contains("line "), "selection should contain the fed content: {text:?}");
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
        // F1: a crafted overflowing code must not panic (overflow-checks on in
        // dev) or wrap (release); it clamps to the 8-bit exit-status range.
        assert_eq!(parse(b"\x1b]133;D;255\x07"), Some(255), "top of the byte range");
        assert_eq!(parse(b"\x1b]133;D;256\x07"), Some(255), "clamped to 255");
        assert_eq!(parse(b"\x1b]133;D;9999999999\x07"), Some(255), "no overflow, clamped");
    }

    #[test]
    fn osc133_overflowing_exit_code_no_panic_still_failed() {
        // F1 focused: the u32 accumulator would overflow-panic in dev without the
        // fully-saturating parse. The result must be sane AND classified failed.
        let mut t = Terminal::new(40, 5);
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;D;9999999999\x07"); // 10 digits: overflows u32 unclamped
        assert_eq!(t.marks.back().unwrap().exit, Some(255), "clamped, not wrapped/garbage");
        assert_eq!(t.failed_prompt_rows(), vec![0], "a huge (nonzero) code is still failed");
        // The `as i32` cast used downstream must stay non-negative (no sign flip).
        assert!(t.marks.back().unwrap().exit.unwrap() > 0);
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
        // Documented sync edge: a 133 inside a BSU binds at parse-arrival;
        // flush_sync must keep abs_top exactly tracking history (no drift). This
        // exercises the UNSATURATED regime (few lines, default 10k cap), where
        // `abs_top == history_size()` holds exactly and marks are placed precisely
        // — the common case the feature guarantees.
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
        assert!(!t.saturated, "12 lines under a 10k cap never saturates");
        assert_eq!(t.abs_top, t.scroll_max() as i64, "abs_top tracks history exactly after sync");
    }

    #[test]
    fn saturated_scrollback_invalidates_marks_never_wrong_row() {
        // F2: once the ring saturates, history_size() pins at the cap while output
        // keeps scrolling, so abs_top can no longer track the prompt. The mark must
        // be INVALIDATED (correct-or-absent) — NEVER painted on a drifting row.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(30); // small cap so saturation is reachable in-test
        t.feed(b"a\r\nb\r\n"); // prep so the prompt isn't on the top row
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        // Common case still exact while unsaturated: the failed prompt renders on
        // its real row and tracks scrolling into history.
        assert!(!t.saturated);
        assert_eq!(t.failed_prompt_rows(), vec![2], "exact placement pre-saturation");
        let prompt_abs = t.marks.back().unwrap().prompt;
        // Now blow well past the 30-line cap. history_size() saturates and freezes.
        for i in 0..80 {
            t.feed(format!("out {i}\r\n").as_bytes());
        }
        assert!(t.saturated, "80 lines over a 30 cap saturates the ring");
        // abs_top froze at the cap (30) while ~80 lines really scrolled — exactly
        // the drift that used to mis-place the marker. The mark's absolute line now
        // sits far BELOW the frozen abs_top, so any prompt−abs_top mapping is
        // meaningless: hence the mark must be gone rather than rendered.
        assert_eq!(t.abs_top, 30, "abs_top pinned at the cap once saturated");
        assert!(prompt_abs < t.abs_top, "the mark drifted below the frozen anchor");
        // NEVER a wrong row: marks are dropped, so nothing renders anywhere in the
        // buffer (bottom, mid-scroll, or top of history).
        assert!(t.marks.is_empty(), "possibly-drifted marks are purged at saturation");
        assert!(t.failed_prompt_rows().is_empty(), "no marker at the live bottom");
        t.scroll_lines(1000);
        assert!(t.failed_prompt_rows().is_empty(), "no marker anywhere in history either");
        t.scroll_to_bottom();
        assert!(!t.jump_prompt(false), "no jump target once saturated");
        // A fresh prompt while saturated must NOT bind a mark that would drift.
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert!(t.marks.is_empty(), "no new marks bound while saturated");
        assert!(t.failed_prompt_rows().is_empty());
    }

    #[test]
    fn saturation_clears_then_recovers_on_shrink() {
        // F2 continued: dropping the cap below the live depth relatches, and a
        // later grow (fresh, exact tracking) lets new marks work again.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(20);
        for i in 0..60 {
            t.feed(format!("x{i}\r\n").as_bytes());
        }
        assert!(t.saturated, "saturated at the 20 cap");
        // Raise the cap far above the live history: no longer saturated, so exact
        // tracking resumes and a new failed prompt renders on its true row.
        t.set_scrollback_lines(10_000);
        assert!(!t.saturated, "cap now far above live depth");
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1, "marks bind again once tracking is exact");
        assert_eq!(t.failed_prompt_rows().len(), 1, "and render on the real row");
    }

    #[test]
    fn resize_clears_marks_and_reanchors_never_wrong_row() {
        // F3: App::reflow() calls resize() on every window/font change. Reflow
        // rewraps logical lines (a prompt's physical row moves) and changes
        // history_size() outside track_abs_top. Pre-existing marks must be CLEARED
        // (their anchor is invalid) — never left to map to a continuation/unrelated
        // row — and abs_top re-anchored so future marks stay exact.
        let mut t = Terminal::new(20, 5);
        t.feed(b"prep\r\n");
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![1], "placed before resize");
        // A real reflow (both dims change, as a font/window resize does).
        t.resize(12, 8);
        assert!(t.marks.is_empty(), "reflow invalidates the anchor → marks cleared");
        assert!(t.failed_prompt_rows().is_empty(), "nothing painted on a wrong row");
        assert!(!t.jump_prompt(false), "no stale jump target after reflow");
        // abs_top re-anchored to the clean invariant on the primary screen.
        assert_eq!(t.abs_top, t.term.grid().history_size() as i64, "abs_top re-anchored");
        // Future marks are exact again: a fresh failed prompt lands on its row.
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1);
        let row = t.failed_prompt_rows();
        assert_eq!(row.len(), 1, "new mark renders on exactly one real row");
        // And that mark then survives scrolling correctly (tracking works post-resize).
        for i in 0..12 {
            t.feed(format!("p{i}\r\n").as_bytes());
        }
        assert!(t.failed_prompt_rows().is_empty(), "scrolled off the bottom");
        t.scroll_lines(1000);
        assert_eq!(t.failed_prompt_rows().len(), 1, "reappears in history at its true row");
    }

    #[test]
    fn same_size_resize_keeps_marks() {
        // The F15 same-dims no-op must NOT wipe marks: the common case is "no
        // resize since the mark was made", and App::reflow() resizes every tab on
        // any window event — a no-op resize has to stay a no-op for marks too.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1);
        t.resize(20, 5); // identical dimensions
        assert_eq!(t.marks.len(), 1, "a no-op resize preserves marks");
        assert_eq!(t.failed_prompt_rows(), vec![0], "still on its real row");
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

    // ── OSC 133 command-completion event (v0.15 Run & Notify) ─────────────────

    #[test]
    fn completion_success_with_c_has_duration_and_last_line() {
        // Full A…C…output…D;0: one completion, exit 0, a (Some) duration, and the
        // last non-empty output row as `last_line`.
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07"); // prompt
        t.feed(b"\x1b]133;C\x07"); // command start (stamps started_at)
        t.feed(b"building...\r\n");
        t.feed(b"done ok\r\n");
        t.feed(b"\x1b]133;D;0\x07"); // done, success
        let done = t.take_completions();
        assert_eq!(done.len(), 1, "exactly one completion");
        assert_eq!(done[0].exit_code, Some(0));
        assert!(done[0].duration.is_some(), "C→D duration present");
        assert_eq!(done[0].last_line, "done ok", "last non-empty output row");
        assert!(t.take_completions().is_empty(), "drains (second call empty)");
    }

    #[test]
    fn completion_failure_reports_nonzero_exit() {
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        t.feed(b"boom\r\n");
        t.feed(b"\x1b]133;D;1\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].exit_code, Some(1), "failure exit surfaced");
    }

    #[test]
    fn completion_bash_shape_a_then_d_has_no_duration() {
        // Plain bash emits A and D but no C — the completion must carry a KNOWN
        // exit but an UNKNOWN (None) duration (drives the notifier's failure-only
        // fallback).
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"oops\r\n");
        t.feed(b"\x1b]133;D;2\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].exit_code, Some(2));
        assert!(done[0].duration.is_none(), "no C ⇒ unknown duration");
    }

    #[test]
    fn completion_exit_code_stays_clamped_through_the_new_path() {
        // Regression: the D;<huge> clamp (0..=255) must still hold when the code
        // flows into a completion.
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        t.feed(b"\x1b]133;D;9999999999\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].exit_code, Some(255), "clamped to the byte ceiling");
    }

    #[test]
    fn completion_last_line_blank_region_is_empty_no_panic() {
        // A command that produced no output: last_line is "" and nothing panics
        // (bounds guard on a near-empty grid / low cursor).
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].last_line, "", "blank output region ⇒ empty body");
    }

    #[test]
    fn completion_last_line_caps_at_200_chars() {
        let mut t = Terminal::new(400, 4); // wide grid so the long line fits one row
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        let long = "x".repeat(250);
        t.feed(long.as_bytes());
        t.feed(b"\r\n\x1b]133;D;0\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].last_line.chars().count(), 200, "capped at 200 chars");
    }

    #[test]
    fn completion_top_of_history_cursor_never_panics() {
        // Bounds guard: a D with the cursor at the very top of a fresh grid must
        // not index an out-of-range Line (alacritty panics on that).
        let mut t = Terminal::new(20, 3);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        let _ = t.take_completions(); // reaching here (no panic) is the assertion
    }

    #[test]
    fn no_completion_on_alt_screen() {
        // OSC 133 inside a TUI (alt screen) is ignored — no completion emitted.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?1049h"); // enter alt screen
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        assert!(t.take_completions().is_empty(), "alt-screen D produces nothing");
    }

    #[test]
    fn completions_are_bounded() {
        // A flood of D marks the app never drains cannot grow `completed` without
        // bound.
        let mut t = Terminal::new(20, 5);
        for _ in 0..(MAX_PENDING_COMPLETIONS + 40) {
            t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07");
        }
        assert!(
            t.completed.len() <= MAX_PENDING_COMPLETIONS,
            "undrained completions stay bounded"
        );
    }

    // ── OSC 52 clipboard ──────────────────────────────────────────────────────

    #[test]
    fn osc52_copy_captures_and_coalesces() {
        // `\e]52;c;<base64("hi")>\a` → the decoded text is captured once, then
        // consumed (a second drain is None). base64("hi") == "aGk=".
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGk=\x07");
        assert_eq!(t.take_clipboard_store().as_deref(), Some("hi"));
        assert_eq!(t.take_clipboard_store(), None, "consuming: second drain is empty");
    }

    #[test]
    fn osc52_copy_coalesces_last_wins() {
        // Two copies before a drain coalesce to the LAST one.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGk=\x07"); // "hi"
        t.feed(b"\x1b]52;c;eWE=\x07"); // base64("ya") == "eWE="
        assert_eq!(t.take_clipboard_store().as_deref(), Some("ya"));
    }

    #[test]
    fn osc52_selection_type_routes_to_clipboard() {
        // The PRIMARY selection form (`p`) is merged into the system clipboard for
        // v1 (both are permitted remote writes under OnlyCopy).
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;p;aGk=\x07");
        assert_eq!(t.take_clipboard_store().as_deref(), Some("hi"));
    }

    #[test]
    fn osc52_copy_under_cap_is_accepted() {
        // A payload decoding to just UNDER the cap is committed. "AAAA" decodes to 3
        // zero bytes; 34133 reps → 102399 bytes ≤ OSC52_MAX_BYTES (102400).
        let reps = 34133;
        let decoded_len = reps * 3;
        assert!(decoded_len <= OSC52_MAX_BYTES, "premise: within the cap");
        let mut t = Terminal::new(20, 5);
        let seq = format!("\x1b]52;c;{}\x07", "AAAA".repeat(reps));
        t.feed(seq.as_bytes());
        let got = t.take_clipboard_store();
        assert_eq!(got.as_ref().map(|s| s.len()), Some(decoded_len));
    }

    #[test]
    fn osc52_copy_over_cap_is_rejected() {
        // A payload decoding to OVER the cap is NOT committed (no clipboard flood).
        // 34134 reps of "AAAA" → 102402 bytes > OSC52_MAX_BYTES (102400).
        let reps = 34134;
        let decoded_len = reps * 3;
        assert!(decoded_len > OSC52_MAX_BYTES, "premise: payload exceeds the cap");
        let mut t = Terminal::new(20, 5);
        let seq = format!("\x1b]52;c;{}\x07", "AAAA".repeat(reps));
        t.feed(seq.as_bytes());
        assert_eq!(t.take_clipboard_store(), None, "oversized copy is rejected");
    }

    #[test]
    fn osc52_paste_denied_by_default() {
        // Default build is write-only (OnlyCopy): a paste query `\e]52;c;?\a` is
        // denied at the alacritty layer, so no load request ever reaches us.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;?\x07");
        assert!(t.take_clipboard_load().is_none(), "paste is off by default");
    }

    #[test]
    fn osc52_paste_request_captured_when_enabled() {
        // With paste enabled, a query yields a reply formatter that produces a
        // well-formed `\e]52;` reply from the provided clipboard text.
        let mut t = Terminal::new(20, 5);
        t.set_osc52_allow_paste(true);
        t.feed(b"\x1b]52;c;?\x07");
        let fmt = t.take_clipboard_load().expect("paste request captured");
        let reply = fmt("hi");
        assert!(reply.starts_with("\x1b]52;"), "reply is an OSC 52 sequence");
        assert!(reply.contains("aGk="), "reply carries base64(\"hi\")");
        assert!(t.take_clipboard_load().is_none(), "consuming: second drain is empty");
    }

    #[test]
    fn osc52_scrollback_change_preserves_paste_mode() {
        // Regression (amendment O2): changing scrollback rebuilds the alacritty
        // Config and must NOT revert an enabled paste back to OnlyCopy.
        let mut t = Terminal::new(20, 5);
        t.set_osc52_allow_paste(true);
        t.set_scrollback_lines(500);
        t.feed(b"\x1b]52;c;?\x07");
        assert!(t.take_clipboard_load().is_some(), "paste survives a scrollback change");
    }

    // ─────────────────────────── SIXEL DCS scanner + placement ───────────────

    /// Wrap sixel `data` in a 7-bit DCS (`ESC P q … ESC \`). Empty params ⇒ P2=0.
    fn sixel(data: &str) -> Vec<u8> {
        let mut v = b"\x1bPq".to_vec();
        v.extend_from_slice(data.as_bytes());
        v.extend_from_slice(b"\x1b\\");
        v
    }
    // A red 1×6 column, and a red 1×12 (two bands).
    const RED_1X6: &str = "#0;2;100;0;0#0~";
    const RED_1X12: &str = "#0;2;100;0;0#0~-~";

    #[test]
    fn sixel_records_placement_and_reserves_rows() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X12)); // 1×12 px → rows = ceil(12/10) = 2
        assert_eq!(t.placements.len(), 1, "one placement recorded");
        let p = &t.placements[0];
        assert_eq!((p.px_w, p.px_h), (1, 12), "native size");
        assert_eq!((p.cols, p.rows), (1, 2), "cell footprint (ceil)");
        assert_eq!(p.abs_line, 0, "anchored at the starting row");
        assert_eq!(p.col, 0, "image starts at column 0");
        // Cursor moved down `rows` lines to column 0 (the reserved region).
        let snap = t.snapshot();
        assert_eq!(snap.cursor_row, 2, "cursor sits below the reserved image rows");
        assert_eq!(snap.cursor_col, 0);
    }

    #[test]
    fn sixel_visible_image_maps_to_viewport() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X6));
        let imgs = t.visible_images();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].top_row, 0.0, "top of image at viewport row 0");
        assert_eq!((imgs[0].px_w, imgs[0].px_h), (1, 6));
        assert!(t.image_rgba(imgs[0].id).is_some(), "rgba retrievable by id");
    }

    #[test]
    fn sixel_split_across_feeds_resumes() {
        let full = sixel(RED_1X12);
        // Split in the middle of the payload.
        let cut = full.len() / 2;
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&full[..cut]);
        t.feed(&full[cut..]);
        assert_eq!(t.placements.len(), 1, "one image across the two feeds");
        assert_eq!((t.placements[0].px_w, t.placements[0].px_h), (1, 12));
    }

    #[test]
    fn sixel_coexists_with_interleaved_osc133() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        // OSC133 A (bind a prompt mark), then a sixel, then a text line.
        t.feed(b"\x1b]133;A\x07");
        assert_eq!(t.marks.len(), 1, "133;A still bound with sixel scanning present");
        t.feed(&sixel(RED_1X6));
        t.feed(b"hello");
        assert_eq!(t.marks.len(), 1, "the mark survived the sixel");
        assert_eq!(t.placements.len(), 1, "and the sixel was recorded");
    }

    #[test]
    fn bel_is_data_inside_sixel() {
        // A BEL (0x07) between two data bytes must NOT terminate the DCS (unlike
        // OSC): both `~` belong to the image → width 2 (not 1 + stray text).
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel("#0;2;100;0;0#0~\x07~"));
        assert_eq!(t.placements.len(), 1);
        assert_eq!(t.placements[0].px_w, 2, "BEL was data; both columns drawn");
    }

    #[test]
    fn eight_bit_st_terminates_sixel() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        // `ESC P q <data> 0x9C` (8-bit ST).
        let mut bytes = b"\x1bPq".to_vec();
        bytes.extend_from_slice(RED_1X6.as_bytes());
        bytes.push(0x9c);
        t.feed(&bytes);
        assert_eq!(t.placements.len(), 1, "8-bit ST terminates the sixel");
    }

    #[test]
    fn dcs_other_records_nothing() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        // DECRQSS `ESC P $ q " p ST` — intermediate `$` ⇒ not a sixel.
        t.feed(b"\x1bP$q\"p\x1b\\");
        assert!(t.placements.is_empty(), "a DECRQSS DCS records no placement");
        // Parser is not desynced: a following sixel still works.
        t.feed(&sixel(RED_1X6));
        assert_eq!(t.placements.len(), 1);
    }

    #[test]
    fn overlong_sixel_overflows_and_drops() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        // Feed a valid opener + more than SIXEL_MAX_BYTES of data, WITHOUT a
        // terminator: the buffer must latch overflow and stay capped.
        let mut bytes = b"\x1bPq#0;2;100;0;0#0".to_vec();
        bytes.extend(std::iter::repeat_n(b'~', SIXEL_MAX_BYTES + 1024));
        t.feed(&bytes);
        assert!(t.sixel_overflow, "overflow latched");
        assert!(t.sixel_buf.len() <= SIXEL_MAX_BYTES, "buffer stays capped");
        // Now terminate: the overflowed image must be DROPPED (correct-or-absent).
        t.feed(b"\x1b\\");
        assert!(t.placements.is_empty(), "overflowed sixel produced no placement");
        assert!(t.sixel_buf.is_empty(), "buffer released on finish");
    }

    #[test]
    fn sixel_dropped_on_alt_screen() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h"); // enter alt screen
        t.feed(&sixel(RED_1X6));
        assert!(t.placements.is_empty(), "no inline images on the alt screen");
    }

    #[test]
    fn sixel_dropped_inside_sync_block() {
        // A sixel inside a DECSET-2026 synchronized-update block anchors at the
        // wrong row (vte is buffering) → it must be dropped (amendment P2).
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?2026h"); // BSU
        assert!(t.sync_deadline().is_some(), "sync armed");
        t.feed(&sixel(RED_1X6));
        assert!(t.placements.is_empty(), "sixel inside a sync block is dropped");
        t.flush_sync();
    }

    #[test]
    fn resize_clears_placements() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X6));
        assert_eq!(t.placements.len(), 1);
        t.resize(30, 8); // reflow invalidates anchors
        assert!(t.placements.is_empty(), "reflow drops placements (correct-or-absent)");
    }

    #[test]
    fn placement_anchor_tracks_scroll_into_history() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X6)); // 1 reserved row, anchored at abs_line 0
        assert_eq!(t.visible_images().len(), 1, "visible at the bottom initially");
        // Push it well into history.
        for _ in 0..30 {
            t.feed(b"\r\n");
        }
        assert!(
            t.visible_images().is_empty(),
            "scrolled into history: not visible while viewing the bottom"
        );
        // Scroll all the way up: the image reappears near the top of the viewport.
        t.scroll_to_offset(t.scroll_max());
        let imgs = t.visible_images();
        assert_eq!(imgs.len(), 1, "reappears when scrolled back to its row");
        assert_eq!(imgs[0].top_row, 0.0, "at viewport row 0 (its true row)");
    }

    #[test]
    fn multi_row_image_prunes_by_span_not_single_line() {
        // A tall image whose TOP has scrolled just above the live window but whose
        // BODY still intersects it must be RETAINED (span intersection, not the
        // single-line mark predicate). Use a tiny scrollback so pruning bites.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(3);
        t.set_cell_px(10.0, 10.0);
        // A 30px-tall image → 3 reserved rows, spanning abs 0..3.
        t.feed(&sixel("#0;2;100;0;0#0~-~-~-~-~")); // 5 bands = 30px
        assert_eq!(t.placements.len(), 1);
        assert!(t.placements[0].rows >= 3, "multi-row footprint");
    }

    // ── hint mode + copy-mode helpers ────────────────────────────────────────

    #[test]
    fn hint_tokens_finds_visible_tokens_with_spans() {
        let mut t = Terminal::new(60, 5);
        t.feed(b"go https://example.com/page and /etc/hosts done");
        let toks = t.hint_tokens();
        let url = toks.iter().find(|h| h.kind == crate::hints::TokenKind::Url).expect("url");
        assert_eq!(url.text, "https://example.com/page");
        assert_eq!(url.spans, vec![(0, 3, 26)]);
        let path = toks.iter().find(|h| h.kind == crate::hints::TokenKind::Path).expect("path");
        assert_eq!(path.text, "/etc/hosts");
    }

    #[test]
    fn hint_tokens_wrapped_url_is_one_full_token() {
        // 20 cols: a long URL wraps; hint_tokens must return the COMPLETE URL as
        // one token whose spans cover both visual rows.
        let mut t = Terminal::new(20, 5);
        t.feed(b"https://example.com/abcdef");
        let toks = t.hint_tokens();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].text, "https://example.com/abcdef");
        assert_eq!(toks[0].spans, vec![(0, 0, 19), (1, 0, 5)]);
    }

    #[test]
    fn hint_tokens_dedups_identical_tokens() {
        let mut t = Terminal::new(40, 5);
        t.feed(b"https://x.io/a\r\nhttps://x.io/a\r\n");
        let toks = t.hint_tokens();
        assert_eq!(toks.iter().filter(|h| h.text == "https://x.io/a").count(), 1);
    }

    #[test]
    fn viewport_rows_chars_matches_snapshot_row_text() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"alpha\r\nbeta\r\n");
        let rows = t.viewport_rows_chars();
        let snap = t.snapshot();
        assert_eq!(rows.len(), 4);
        for r in 0..4 {
            let s: String = rows[r].iter().collect();
            assert_eq!(s, snap.row_text(r), "row {r}");
        }
    }

    /// Copy-mode selection-side derivation (BLOCKING 2): the START endpoint takes
    /// Side::Left (left_half=true), the END endpoint Side::Right (left_half=false),
    /// ordered by cursor-vs-anchor reading order. This mirrors the app's per-
    /// keystroke rebuild.
    fn cm_select(t: &mut Terminal, anchor: (usize, usize), cursor: (usize, usize)) -> Option<String> {
        let forward = cursor >= anchor;
        let (s, e) = if forward { (anchor, cursor) } else { (cursor, anchor) };
        t.selection_start(s.0, s.1, true); // Left
        t.selection_update(e.0, e.1, false); // Right
        t.selection_text()
    }

    #[test]
    fn copy_mode_selection_is_inclusive_both_directions() {
        let mut t = Terminal::new(20, 3);
        t.feed(b"hello world");
        // Forward: anchor at 'h' (0,0), cursor at 'o' (0,4) → "hello" inclusive.
        assert_eq!(cm_select(&mut t, (0, 0), (0, 4)).as_deref(), Some("hello"));
        // Reverse: anchor at 'o' (0,4), cursor at 'h' (0,0) → same inclusive text.
        assert_eq!(cm_select(&mut t, (0, 4), (0, 0)).as_deref(), Some("hello"));
        // Single cell selects exactly that char.
        assert_eq!(cm_select(&mut t, (0, 6), (0, 6)).as_deref(), Some("w"));
    }

    #[test]
    fn selection_start_lines_yields_whole_lines() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"first line\r\nsecond\r\n");
        t.selection_start_lines(0);
        t.selection_update(1, 3, false);
        let txt = t.selection_text().expect("line selection");
        assert!(txt.contains("first line"), "got {txt:?}");
        assert!(txt.contains("second"), "got {txt:?}");
    }

    // ─────────────────────────── KITTY graphics (APC ESC _ G) ─────────────────

    /// Standard base64 encode (test helper; the decoder is tested in base64.rs).
    fn b64(data: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
            s.push(A[((n >> 18) & 63) as usize] as char);
            s.push(A[((n >> 12) & 63) as usize] as char);
            s.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
            s.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
        }
        s
    }

    /// Wrap a Kitty APC BODY (bytes AFTER `ESC _ G`, before ST) in a full APC.
    fn apc(body: &str) -> Vec<u8> {
        let mut v = b"\x1b_G".to_vec();
        v.extend_from_slice(body.as_bytes());
        v.extend_from_slice(b"\x1b\\");
        v
    }

    /// A 2×2 opaque-red f=32 RGBA transmit+display command.
    fn red_rgba_2x2(extra: &str) -> Vec<u8> {
        let px = [255u8, 0, 0, 255].repeat(4); // 2×2 RGBA
        let payload = b64(&px);
        apc(&format!("a=T,f=32,s=2,v=2{extra};{payload}"))
    }

    #[test]
    fn kitty_rgba_places_one_image() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(""));
        assert_eq!(t.placements.len(), 1, "one placement");
        let p = &t.placements[0];
        assert_eq!((p.px_w, p.px_h), (2, 2));
        assert_eq!(&p.image.rgba[0..4], &[255, 0, 0, 255], "opaque red premultiplied");
    }

    #[test]
    fn kitty_rgb_expands_and_places() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [0u8, 255, 0].repeat(4); // 2×2 green RGB
        let payload = b64(&px);
        t.feed(&apc(&format!("a=T,f=24,s=2,v=2;{payload}")));
        assert_eq!(t.placements.len(), 1);
        assert_eq!(&t.placements[0].image.rgba[0..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn kitty_explicit_cols_rows_override_footprint() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",c=4,r=2"));
        let p = &t.placements[0];
        assert_eq!((p.cols, p.rows), (4, 2), "explicit c/r footprint");
    }

    #[test]
    fn kitty_anchors_at_cursor_column() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"abc"); // cursor at column 3
        t.feed(&red_rgba_2x2(""));
        assert_eq!(t.placements[0].col, 3, "image anchors at the cursor column");
    }

    #[test]
    fn kitty_dropped_on_alt_screen() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h");
        t.feed(&red_rgba_2x2(""));
        assert!(t.placements.is_empty(), "no Kitty image on the alt screen");
    }

    #[test]
    fn kitty_dropped_inside_sync_block() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?2026h");
        assert!(t.sync_deadline().is_some());
        t.feed(&red_rgba_2x2(""));
        assert!(t.placements.is_empty(), "dropped inside a sync block");
        t.flush_sync();
    }

    #[test]
    fn kitty_split_across_feeds_resumes() {
        // One APC split mid-payload across two feed() calls still assembles.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let full = red_rgba_2x2("");
        let mid = full.len() / 2;
        t.feed(&full[..mid]);
        t.feed(&full[mid..]);
        assert_eq!(t.placements.len(), 1, "resumes across a feed boundary");
    }

    #[test]
    fn kitty_non_g_apc_is_ignored() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        // ESC _ q ... ESC \  (an APC that is not a graphics command).
        t.feed(b"\x1b_qsomething\x1b\\");
        assert!(t.placements.is_empty(), "non-G APC leaves no placement");
        // Parser not desynced: a real Kitty image after it still works.
        t.feed(&red_rgba_2x2(""));
        assert_eq!(t.placements.len(), 1);
    }

    #[test]
    fn kitty_apc_overflow_drops_and_caps() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let mut bytes = b"\x1b_Ga=T,f=32,s=2,v=2;".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', APC_MAX_BYTES + 1024));
        t.feed(&bytes);
        assert!(t.apc_overflow, "overflow latched");
        assert!(t.apc_buf.len() <= APC_MAX_BYTES, "buffer stays capped");
        t.feed(b"\x1b\\");
        assert!(t.placements.is_empty(), "overflowed APC produced no placement");
        assert!(t.apc_buf.is_empty(), "buffer released on finish");
    }

    #[test]
    fn kitty_two_chunk_rgba_assembles_one_image() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [10u8, 20, 30, 255].repeat(4); // 2×2 RGBA
        let payload = b64(&px);
        let (a, b) = payload.split_at(payload.len() / 2);
        // First chunk carries full control + m=1; last carries only m=0.
        t.feed(&apc(&format!("a=T,f=32,s=2,v=2,m=1;{a}")));
        assert_eq!(t.placements.len(), 0, "not placed until finalized");
        t.feed(&apc(&format!("m=0;{b}")));
        assert_eq!(t.placements.len(), 1, "two-chunk image assembles to one placement");
        assert_eq!((t.placements[0].px_w, t.placements[0].px_h), (2, 2));
    }

    #[test]
    fn kitty_interleaved_query_does_not_splice() {
        // BLOCKING 1: [first m=1] → [a=q,i=9 interleaved] → [continuation m=0]
        // must NOT splice the query into the accumulating image. The has_action
        // query aborts the partial; the continuation is then an orphan (dropped).
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [1u8, 2, 3, 255].repeat(4);
        let payload = b64(&px);
        let (a, b) = payload.split_at(payload.len() / 2);
        t.feed(&apc(&format!("a=T,f=32,s=2,v=2,m=1;{a}")));
        // Interleaved query (has_action=true) aborts the accumulation.
        t.feed(&apc("a=q,i=9,f=32,s=1,v=1;AAAA"));
        // The now-orphaned continuation must not produce a spliced image.
        t.feed(&apc(&format!("m=0;{b}")));
        assert!(t.placements.is_empty(), "no spliced image after an interleaved query");
        // The query still answered (addressable i=9).
        let replies = t.drain_pty_writes();
        assert!(!replies.is_empty(), "the interleaved query got a reply");
    }

    #[test]
    fn kitty_endless_more_is_bounded() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&apc("a=T,f=32,s=2,v=2,m=1;AAAA"));
        for _ in 0..(MAX_KITTY_CHUNKS + 10) {
            t.feed(&apc("m=1;AAAA"));
        }
        // Aborted once the chunk count cap was exceeded — no runaway growth.
        assert!(t.chunk_buf.len() <= KITTY_RAW_BUDGET, "chunk buffer bounded");
        assert!(t.chunk_meta.is_none(), "accumulation aborted at the cap");
    }

    #[test]
    fn kitty_transmit_then_put_round_trips() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [7u8, 7, 7, 255].repeat(4);
        let payload = b64(&px);
        // a=t stores without displaying.
        t.feed(&apc(&format!("a=t,f=32,s=2,v=2,i=7;{payload}")));
        assert_eq!(t.placements.len(), 0, "a=t does not display");
        assert_eq!(t.kitty_images.len(), 1, "stored in the registry");
        // a=p,i=7 displays it.
        t.feed(&apc("a=p,i=7"));
        assert_eq!(t.placements.len(), 1, "a=p displays the stored image");
    }

    #[test]
    fn kitty_put_unknown_id_replies_enoent() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&apc("a=p,i=999"));
        assert!(t.placements.is_empty());
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("ENOENT"), "got {reply:?}");
    }

    #[test]
    fn kitty_registry_evicts_over_cap() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [1u8, 1, 1, 255].repeat(4);
        let payload = b64(&px);
        for id in 1..=(MAX_KITTY_STORED as u32 + 5) {
            t.feed(&apc(&format!("a=t,f=32,s=2,v=2,i={id};{payload}")));
        }
        assert!(t.kitty_images.len() <= MAX_KITTY_STORED, "registry count bounded");
    }

    #[test]
    fn kitty_delete_all_clears_placements() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=3"));
        assert_eq!(t.placements.len(), 1);
        t.feed(&apc("a=d,d=a"));
        assert!(t.placements.is_empty(), "d=a clears all placements");
    }

    #[test]
    fn kitty_delete_by_id_targets_only_that_id() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=3"));
        t.feed(&red_rgba_2x2(",i=8"));
        assert_eq!(t.placements.len(), 2);
        t.feed(&apc("a=d,d=i,i=3"));
        assert_eq!(t.placements.len(), 1, "only id 3 removed");
        assert_eq!(t.placements[0].kitty_id, Some(8));
    }

    #[test]
    fn kitty_query_replies_ok_and_creates_no_placement() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [9u8, 9, 9, 255];
        let payload = b64(&px);
        t.feed(&apc(&format!("a=q,i=2,f=32,s=1,v=1;{payload}")));
        assert!(t.placements.is_empty(), "a=q never displays");
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("i=2;OK"), "got {reply:?}");
    }

    #[test]
    fn kitty_ok_reply_respects_quiet() {
        // q=1 suppresses OK but a later error still reports.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=1,q=1"));
        assert!(t.drain_pty_writes().is_empty(), "q=1 suppresses the OK");
        // q=1 still reports an ENOENT error.
        t.feed(&apc("a=p,i=555,q=1"));
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("ENOENT"), "q=1 still reports errors: {reply:?}");
        // q=2 suppresses everything, even errors.
        t.feed(&apc("a=p,i=556,q=2"));
        assert!(t.drain_pty_writes().is_empty(), "q=2 suppresses errors too");
    }

    #[test]
    fn kitty_refuses_file_transfer() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&apc("a=T,f=32,s=2,v=2,t=f,i=4;AAAA"));
        assert!(t.placements.is_empty(), "t=f is refused, no placement");
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("ENOTSUPP"), "got {reply:?}");
    }

    #[test]
    fn kitty_compressed_zlib_rgba_places() {
        // o=z: the payload is zlib-compressed RGBA (BLOCKING 3).
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [50u8, 60, 70, 255].repeat(4); // 2×2
        let comp = miniz_oxide::deflate::compress_to_vec_zlib(&px, 6);
        let payload = b64(&comp);
        t.feed(&apc(&format!("a=T,f=32,s=2,v=2,o=z;{payload}")));
        assert_eq!(t.placements.len(), 1, "o=z RGBA inflates and places");
    }

    #[test]
    fn kitty_anonymous_transmit_sends_no_reply() {
        // A10: an anonymous (no i=/I=) transmit must not amplify replies.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2("")); // no id
        assert!(t.drain_pty_writes().is_empty(), "no reply for anonymous transmit");
    }

    #[test]
    fn kitty_and_sixel_interleave_independently() {
        // A sixel DCS and a Kitty APC in ONE buffer both parse to placements
        // without cross-contaminating the two state machines.
        let mut t = Terminal::new(30, 8);
        t.set_cell_px(10.0, 10.0);
        let mut buf = sixel(RED_1X6);
        buf.extend_from_slice(&red_rgba_2x2(""));
        t.feed(&buf);
        assert_eq!(t.placements.len(), 2, "both a sixel and a Kitty image placed");
    }

    #[test]
    fn kitty_fuzz_random_apc_never_panics() {
        let mut t = Terminal::new(40, 10);
        t.set_cell_px(10.0, 10.0);
        let mut state: u64 = 0xabcd_1234_5678_9f01;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..300 {
            let n = (next() % 200) as usize;
            let mut buf = b"\x1b_G".to_vec();
            buf.extend((0..n).map(|_| (next() & 0xff) as u8));
            buf.extend_from_slice(b"\x1b\\");
            t.feed(&buf);
            // Invariant: every placement's rgba matches its native dims.
            for p in &t.placements {
                assert_eq!(
                    p.image.rgba.len(),
                    (p.image.width as usize) * (p.image.height as usize) * 4
                );
            }
        }
    }
}
