use crate::handler::VtState;
use crate::hints::HintToken;
use crate::kitty::KittyCmd;
use crate::snapshot::{
    attr, CellGrapheme, CellSnapshot, CursorShapeSnap, GridSnapshot, SearchHit, GRAPHEME_MAX_BYTES,
    GRAPHEME_MAX_MARKS,
};
use crate::theme::Theme;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    Config, Osc52, Term, TermMode, point_to_viewport, viewport_to_point,
};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor, Rgb};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Pack `cols`/`rows` into a single `u32` (cols in the high 16 bits) so the
/// `Terminal` and its moved-away `EventProxy` can share live geometry through
/// an `Arc<AtomicU32>` (alacritty exposes no public listener setter).
fn pack_geom(cols: usize, rows: usize) -> u32 {
    ((cols.min(u16::MAX as usize) as u32) << 16) | (rows.min(u16::MAX as usize) as u32)
}

/// Pack the exact cell size (px) for the `EventProxy`, as `f32` bits.
fn pack_cell_px(w: f32, h: f32) -> u64 {
    (u64::from(w.to_bits()) << 32) | u64::from(h.to_bits())
}

/// The pixel length of `cells` cells of `cell` px, as the PTY is told it
/// (TIOCGWINSZ — the app's `pty.resize` computes it the same way).
fn text_area_px(cells: u32, cell: f32) -> u16 {
    (cells as f32 * cell).min(65535.0) as u16
}

/// The ONE place the alacritty `Config` is built. `Term::set_options` replaces the
/// whole config, so `new` and every runtime rebuild (scrollback, OSC 52, kitty
/// keyboard, default cursor shape) must go through here or a non-default field
/// would silently revert.
fn term_config(scrollback: usize, osc52: Osc52, kitty_keyboard: bool, cursor: CursorShape) -> Config {
    Config {
        scrolling_history: scrollback,
        osc52,
        kitty_keyboard,
        default_cursor_style: CursorStyle { shape: cursor, blinking: false },
        ..Default::default()
    }
}

/// A sequence the `feed` scanner advances in a sub-slice of its OWN (only while
/// anchors exist) so `abs_top` bookkeeping sees its history effect in isolation.
/// ED 3 and RIS SHRINK history — a same-write `\e[2J\e[3J` (`clear`) otherwise
/// nets out to "no change" and leaves anchors on the wrong rows. ED 2 erases the
/// screen's anchors that `\e[2J` did not push into scrollback. ED 2, SU and DL
/// push up to a whole screen into scrollback from a few bytes, so their
/// sub-slice is bounded by the lines they actually push, not by their byte
/// count (primary screen only: on the alt screen they cannot touch the primary
/// scrollback and are not split out). An alt-screen toggle freezes `abs_top`, so
/// primary output sharing its slice would go uncounted. A synchronized update
/// in flight is applied first — vte would otherwise only buffer the sequence
/// and replay it together with the rest of the update — except before SU / DL
/// (the replay counts their scroll) and RIS (it drops every anchor anyway).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsolatedSeq {
    /// `ESC [ 2 J` — erase the whole screen.
    EraseScreen,
    /// `ESC [ 3 J` — erase the saved lines (scrollback).
    EraseSaved,
    /// `ESC [ Ps S` (SU) / `ESC [ Ps M` (DL, `delete`) — scroll (part of) the
    /// region up by `count` lines (vte: 0 = 1); on the primary screen with the
    /// region at the top they go into scrollback — for DL only when the cursor
    /// is on the top row.
    ScrollUp { count: u16, delete: bool },
    /// `ESC c` — RIS, full reset (clears screen + scrollback, leaves the alt screen).
    Reset,
    /// `ESC [ ? 47 | 1047 | 1049 h|l` — alternate-screen toggle.
    AltToggle,
}

impl IsolatedSeq {
    /// Whether one such sequence can push up to a whole screen of lines into
    /// scrollback (vte clamps the count to the scroll region).
    fn scrolls_a_screen(self) -> bool {
        matches!(self, IsolatedSeq::EraseScreen | IsolatedSeq::ScrollUp { .. })
    }
}

/// Index of the first OSC terminator in `s` — BEL, CAN, SUB or ESC (vte 0.15's
/// `advance_osc_string` set) — via SIMD scans.
fn osc_terminator(s: &[u8]) -> Option<usize> {
    let a = memchr::memchr3(0x07, 0x18, 0x1b, s);
    let b = memchr::memchr(0x1a, &s[..a.unwrap_or(s.len())]);
    b.or(a)
}

/// Index of the first DCS terminator in `s` — CAN, SUB, ESC or 8-bit ST (vte
/// 0.15's `advance_dcs_passthrough` set; BEL is data in a DCS) — via SIMD scans.
fn dcs_terminator(s: &[u8]) -> Option<usize> {
    let a = memchr::memchr3(0x18, 0x1a, 0x1b, s);
    let b = memchr::memchr(0x9c, &s[..a.unwrap_or(s.len())]);
    b.or(a)
}

/// Index of the first Kitty APC terminator in `s`: a DCS terminator, or BEL.
fn apc_terminator(s: &[u8]) -> Option<usize> {
    let a = dcs_terminator(s);
    let b = memchr::memchr(0x07, &s[..a.unwrap_or(s.len())]);
    b.or(a)
}

/// Where the DCS / APC body starting at `bytes[i]` ends: at its terminator, or
/// at the end of `bytes`. Out of line: no image arrives in the common case.
#[cold]
#[inline(never)]
fn dcs_end(bytes: &[u8], i: usize) -> usize {
    dcs_terminator(&bytes[i..]).map_or(bytes.len(), |n| i + n)
}

/// Drop the placements `keep` rejects, keeping the live-bytes counter exact.
fn retain_placements(
    list: &mut VecDeque<ImagePlacement>,
    bytes: &mut u64,
    mut keep: impl FnMut(&ImagePlacement) -> bool,
) {
    list.retain(|p| {
        let k = keep(p);
        if !k {
            *bytes = bytes.saturating_sub(p.image.rgba.len() as u64);
        }
        k
    });
}

/// Record `p`, first evicting every placement it fully covers (an in-place
/// redraw or animation frame — chafa, timg, a re-sent preview — would otherwise
/// stack up to the cap, every layer drawn each frame) and, for Kitty, any
/// placement with the same image id AND placement id (the spec's replace). Then
/// enforce the count / live-bytes budget, oldest first.
fn insert_placement(list: &mut VecDeque<ImagePlacement>, bytes: &mut u64, p: ImagePlacement) {
    retain_placements(list, bytes, |o| {
        !(o.covered_by(&p)
            || (p.kitty_placement.is_some()
                && o.kitty_id == p.kitty_id
                && o.kitty_placement == p.kitty_placement))
    });
    *bytes = bytes.saturating_add(p.image.rgba.len() as u64);
    list.push_back(p);
    while list.len() > MAX_PLACEMENTS || *bytes > MAX_PLACEMENT_BYTES {
        let Some(old) = list.pop_front() else { break };
        *bytes = bytes.saturating_sub(old.image.rgba.len() as u64);
    }
}

/// A cell flag bit alacritty does not define, set on the ALT-screen cells a
/// sixel covers. Writing, erasing or resetting a cell rebuilds its flags (from
/// the cursor template / defaults), so a covered cell without it was written —
/// even with identical content: TUIs erase a sixel by printing spaces over it.
const SIXEL_CELL: Flags = Flags::from_bits_retain(1 << 15);
const _: () = assert!(Flags::all().bits() & SIXEL_CELL.bits() == 0, "alacritty took bit 15");

/// The screen cells an ALT-screen placement covers, clipped to the screen.
fn alt_cells(p: &ImagePlacement, rows: usize, cols: usize) -> impl Iterator<Item = (Line, Column)> {
    let r0 = p.abs_line.clamp(0, rows as i64) as i32;
    let r1 = (p.abs_line + p.rows as i64).clamp(0, rows as i64) as i32;
    let c0 = (p.col as usize).min(cols);
    let c1 = (p.col as usize + p.cols as usize).min(cols);
    (r0..r1).flat_map(move |r| (c0..c1).map(move |c| (Line(r), Column(c))))
}

/// The composed text of a cell carrying zero-width chars: the base char, then at
/// most [`GRAPHEME_MAX_MARKS`] marks within [`GRAPHEME_MAX_BYTES`] — so a Zalgo
/// stack costs the snapshot (built every frame) a bounded amount.
fn grapheme_text(base: char, marks: &[char]) -> String {
    let mut text = String::with_capacity(base.len_utf8() + marks.len().min(GRAPHEME_MAX_MARKS) * 2);
    text.push(base);
    for &m in marks.iter().take(GRAPHEME_MAX_MARKS) {
        if text.len() + m.len_utf8() > GRAPHEME_MAX_BYTES {
            break;
        }
        text.push(m);
    }
    text
}

/// Whether the marks are in ascending `prompt` order (see `Terminal::marks_sorted`).
fn marks_ascending(marks: &VecDeque<CmdBlock>) -> bool {
    marks.iter().zip(marks.iter().skip(1)).all(|(a, b)| a.prompt <= b.prompt)
}

/// Trim the zero-width chars piled on the cells next to the cursor — the only
/// cells `Term::input` attaches marks to (the previous cell, or the one before a
/// wide glyph's spacer) — to [`GRAPHEME_MAX_MARKS`]. alacritty appends them
/// unbounded (`Cell::push_zerowidth`), so a Zalgo stream on ONE cell would grow
/// without limit; run after every sub-slice, this stops that. RESIDUAL
/// (documented): what one sub-slice piles on a cell the cursor then leaves stays
/// (bounded by that sub-slice — a PTY read, or a sync-update flush ≤ 2 MiB), and
/// marks spread one-per-base-char across many cells are bounded only by the
/// input still in the scrollback (each mark is a 4-byte `char` in its cell). The
/// snapshot reads at most [`GRAPHEME_MAX_MARKS`] of any cell's marks either way.
fn cap_cursor_zerowidth(term: &mut Term<EventProxy>) {
    let point = term.grid().cursor.point;
    let grid = term.grid_mut();
    let last = grid.columns().saturating_sub(1);
    for col in point.column.0.saturating_sub(2)..=point.column.0.min(last) {
        let cell = &mut grid[point.line][Column(col)];
        let Some(marks) = cell.zerowidth().filter(|m| m.len() > GRAPHEME_MAX_MARKS) else {
            continue;
        };
        let keep = marks[..GRAPHEME_MAX_MARKS].to_vec();
        let underline = cell.underline_color();
        let link = cell.hyperlink();
        cell.extra = None;
        for m in keep {
            cell.push_zerowidth(m);
        }
        cell.set_underline_color(underline);
        cell.set_hyperlink(link);
    }
}

/// Classify a completed CSI (`ESC [` + `params` + `fin`) the scanner isolates.
fn isolated_csi(params: &[u8], fin: u8) -> Option<IsolatedSeq> {
    match params.split_first() {
        Some((b'?', mode)) => (matches!(mode, b"47" | b"1047" | b"1049") && matches!(fin, b'h' | b'l'))
            .then_some(IsolatedSeq::AltToggle),
        _ if params.iter().all(u8::is_ascii_digit) => scroll_csi(params, fin),
        _ => None,
    }
}

/// The history-rewriting CSIs with one decimal parameter (`digits`, maybe
/// empty): ED 2 / ED 3 (vte reads the number, so `\e[02J` is ED 2 too), and SU /
/// DL with any count.
fn scroll_csi(digits: &[u8], fin: u8) -> Option<IsolatedSeq> {
    match fin {
        b'J' => match decset_param(digits) {
            2 => Some(IsolatedSeq::EraseScreen),
            3 => Some(IsolatedSeq::EraseSaved),
            _ => None,
        },
        b'S' | b'M' => Some(IsolatedSeq::ScrollUp { count: decset_param(digits), delete: fin == b'M' }),
        _ => None,
    }
}

/// [`Scan::Decset`] bit: the sequence named mode 9 (X10 mouse reporting).
const DECSET_X10: u8 = 1;
/// [`Scan::Decset`] bit: the sequence named mode 1015 (urxvt mouse encoding).
const DECSET_URXVT: u8 = 2;
/// [`Scan::Decset`] bit: the sequence named mode 2031 (color-scheme reports).
const DECSET_2031: u8 = 4;
/// [`Scan::Decset`] bit: the sequence has more than one parameter (so it is not
/// the single-parameter `CSI ? 996 n` query).
const DECSET_MULTI: u8 = 0x80;

/// The [`Scan::Decset`] bit for private mode `n` (0 for every other mode).
fn decset_bit(n: u16) -> u8 {
    match n {
        9 => DECSET_X10,
        1015 => DECSET_URXVT,
        2031 => DECSET_2031,
        _ => 0,
    }
}

/// The private DSR `CSI ? 996 n`: "which color scheme (dark/light) is on?".
const DSR_COLOR_SCHEME: u16 = 996;

/// Parse the decimal digits of one CSI parameter (a private mode's without its
/// `?`), saturating — an over-long number can never alias a real one (the mouse
/// modes 9 / 1015, ED 2 / 3).
fn decset_param(digits: &[u8]) -> u16 {
    digits.iter().fold(0u16, |n, &d| n.saturating_mul(10).saturating_add(u16::from(d.wrapping_sub(b'0'))))
}

/// Result of peeking at the bytes after `ESC [` (see [`peek_isolated_csi`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CsiPeek {
    /// An isolated sequence whose remaining `len` bytes are all present.
    Isolate(usize, IsolatedSeq),
    /// The buffer ends inside what could still become one.
    Incomplete,
    /// Some other CSI (SGR, cursor motion, …).
    Other,
}

/// Parameter bytes [`Scan::Csi`] collects when a CSI is cut by a feed boundary.
const CSI_PARAMS_MAX: usize = 5;

/// Bytes vte executes (C0 controls but CAN / SUB / ESC) or ignores (DEL, 0x80..)
/// inside a CSI WITHOUT leaving it — so `ESC [ <LF> > 1 u` is still a push.
fn csi_transparent(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff)
}

/// Most kitty keyboard flag-stack entries JeTTY lets a screen hold. A push past
/// it is dropped before it reaches alacritty: alacritty 0.26 caps its stack at
/// 4096 by evicting from the TITLE stack — a panic (the whole terminal gone)
/// when that is empty, so `printf '\e[>1u%.0s' {1..4097}` crashed JeTTY. Real
/// programs push one or two levels; the margin to 4096 absorbs any mirror drift.
const KBD_STACK_MAX: u16 = 128;

/// The PRIMARY-screen kitty keyboard state a command started from (see
/// [`Terminal::restore_kbd`]): the lowest stack depth seen since — what was on
/// the stack below it belongs to the shell — and whether the active flags were
/// replaced in place (`CSI = … u`, no push). `provisional` = opened at a prompt
/// (`A`) because the shell may never send a command-start `C`: it is only
/// undone by a `D` (a command did run), never by the next `A` alone.
#[derive(Clone, Copy, Debug)]
struct KbdWindow {
    floor: u16,
    set: bool,
    provisional: bool,
}

/// Peek at `rest` (the bytes right after `ESC [`) for an [`IsolatedSeq`]. An SGR
/// costs its first parameter's digits and one compare of the byte after them
/// (`;` or `m`); a letter-led CSI other than `J`/`S`/`M` costs one compare.
fn peek_isolated_csi(rest: &[u8]) -> CsiPeek {
    match rest.first() {
        None => CsiPeek::Incomplete,
        Some(b'?') => {
            const ALT: [&[u8]; 6] = [b"?47h", b"?47l", b"?1047h", b"?1047l", b"?1049h", b"?1049l"];
            let mut incomplete = false;
            for seq in ALT {
                if rest.starts_with(seq) {
                    return CsiPeek::Isolate(seq.len(), IsolatedSeq::AltToggle);
                }
                incomplete |= seq.starts_with(rest);
            }
            if incomplete { CsiPeek::Incomplete } else { CsiPeek::Other }
        }
        Some(b'0'..=b'9' | b'J' | b'S' | b'M') => match rest.iter().position(|b| !b.is_ascii_digit()) {
            Some(n) => match scroll_csi(&rest[..n], rest[n]) {
                Some(kind) => CsiPeek::Isolate(n + 1, kind),
                None => CsiPeek::Other,
            },
            // The feed ends inside the number: finish it byte-wise (a number too
            // long for that is not tracked — it can only be absurd).
            None if rest.len() < CSI_PARAMS_MAX => CsiPeek::Incomplete,
            None => CsiPeek::Other,
        },
        Some(_) => CsiPeek::Other,
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

/// The selection an OSC 52 request names: `c` the clipboard, `p` / `s` the
/// PRIMARY selection (what a middle click pastes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Osc52Target {
    Clipboard,
    Primary,
}

impl Osc52Target {
    fn of(ty: alacritty_terminal::term::ClipboardType) -> Osc52Target {
        match ty {
            alacritty_terminal::term::ClipboardType::Clipboard => Osc52Target::Clipboard,
            alacritty_terminal::term::ClipboardType::Selection => Osc52Target::Primary,
        }
    }
}

/// Pending OSC 52 requests, at most ONE per selection: a newer request for the
/// same selection replaces the older one (last wins) and queues behind the other
/// selection's, so arrival order is kept. nvim with `clipboard=unnamed,unnamedplus`
/// copies every yank to BOTH selections back to back, and asks for both on a
/// paste — one shared slot used to let the second overwrite the first.
type Osc52Pending<T> = Arc<Mutex<Vec<(Osc52Target, T)>>>;

fn push_osc52<T>(pending: &Osc52Pending<T>, target: Osc52Target, item: T) {
    let mut list = pending.lock().unwrap();
    list.retain(|(t, _)| *t != target);
    list.push((target, item));
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
    /// Live cell pixel size (exact, [`pack_cell_px`]), shared with the owning
    /// `Terminal` so the `\e[14t` text-area-size reply reports the REAL cell
    /// metrics (amendment A5). Image tools (chafa/timg/kitty) scale to this; a
    /// wrong (hardcoded 8×16) value makes HiDPI images render undersized.
    cell_px: Arc<AtomicU64>,
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
    /// Pending OSC 52 clipboard-COPY texts (remote/tmux/nvim asked to set a system
    /// selection), one per selection, last-wins. Committed by the app on the drain
    /// pass via [`Terminal::take_clipboard_stores`]. Only ever set when alacritty's
    /// `osc52` mode permits copy (OnlyCopy/CopyPaste — the default).
    clipboard_store: Osc52Pending<String>,
    /// Cheap "a clipboard-copy is pending" flag so the drain path skips the mutex in
    /// the common no-copy case (lock-free — zero idle cost).
    clipboard_dirty: Arc<AtomicBool>,
    /// Pending OSC 52 clipboard-PASTE (load) requests: the reply formatter alacritty
    /// supplied, one per selection. Only ever set when `osc52` mode permits paste
    /// (OnlyPaste/CopyPaste), i.e. only when the user opted into
    /// `osc52_allow_paste`. Drained by the app via [`Terminal::take_clipboard_loads`],
    /// which reads each selection, formats, and writes the replies to the PTY. Off
    /// by default (the secure default).
    clipboard_load: Osc52Pending<ClipboardLoadFmt>,
    /// Cheap "a clipboard-paste is pending" flag (mirrors `clipboard_dirty`).
    clipboard_load_dirty: Arc<AtomicBool>,
    /// DEC mode 2031 state, shared with the owning `Terminal`'s scanner (see
    /// `Terminal::color_reports`): alacritty answers `CSI ? 2031 $ p` as "not
    /// recognized", and neovim only enables the mode after a set/reset answer,
    /// so that reply is rewritten here from the real state.
    color_reports: Arc<AtomicBool>,
}

/// alacritty's DECRQM answer for a private mode it doesn't know, for mode 2031.
const DECRQM_2031_UNKNOWN: &str = "\x1b[?2031;0$y";

/// The DECRQM answer for mode 2031: `1` = set, `2` = reset.
fn decrqm_2031(on: bool) -> &'static str {
    if on { "\x1b[?2031;1$y" } else { "\x1b[?2031;2$y" }
}

/// The color-scheme report (`CSI ? 997 ; 1 n` dark, `CSI ? 997 ; 2 n` light)
/// for a theme with background `bg` — the reply to `CSI ? 996 n` and the
/// unsolicited DEC 2031 notification.
pub(crate) fn color_scheme_report(bg: [u8; 3]) -> &'static [u8] {
    if crate::contrast::is_dark(bg) { b"\x1b[?997;1n" } else { b"\x1b[?997;2n" }
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
            // Replies to DSR/DA-style queries the terminal answers itself. The
            // DECRQM answer for mode 2031 (which alacritty doesn't know) is
            // rewritten from the scanner-tracked state, so a program that probes
            // before enabling it (neovim) sees the mode as supported.
            Event::PtyWrite(s) => {
                let bytes = if s == DECRQM_2031_UNKNOWN {
                    decrqm_2031(self.color_reports.load(Ordering::Relaxed)).as_bytes().to_vec()
                } else {
                    s.into_bytes()
                };
                let _ = self.tx.send(bytes);
            }
            // \e[14t (text area size in pixels): the area the PTY is told
            // (TIOCGWINSZ), from the REAL cell metrics the app pushed via
            // `set_cell_px` (image tools — chafa/timg/notcurses/viu/lsix — scale
            // to them); 8×16 is only the fallback until the first push. The
            // formatter multiplies cells by a whole-pixel cell size, which
            // over-reported the width by up to half a pixel per column (1800
            // for 1720 px at 8.6 px cells: lsix's montage spilled past the
            // grid), so it is handed the exact area as one cell.
            Event::TextAreaSizeRequest(fmt) => {
                let g = self.geom.load(Ordering::Relaxed);
                let cp = self.cell_px.load(Ordering::Relaxed);
                let (cw, ch) = (f32::from_bits((cp >> 32) as u32), f32::from_bits(cp as u32));
                let window_size = WindowSize {
                    num_lines: 1,
                    num_cols: 1,
                    cell_width: text_area_px(g >> 16, cw),
                    cell_height: text_area_px(g & 0xFFFF, ch),
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
            // OSC 52 COPY: a remote host / tmux / nvim (`"+y`) asked to set a
            // system selection. alacritty already base64-decoded + UTF-8-validated
            // the payload and only emits this when its `osc52` mode permits copy
            // (OnlyCopy is JeTTY's default). Coalesce last-wins PER SELECTION; the
            // app commits each on the drain pass — the PRIMARY selection when the
            // request named it (`p`/`s` → `Selection`), else the clipboard.
            Event::ClipboardStore(ty, text) => {
                // Cap the COMMITTED text (see OSC52_MAX_BYTES): reject an abusive
                // payload rather than flooding the real clipboard. The transient
                // decode already happened inside alacritty (bounded by its OSC
                // buffer), so this is a commit gate, not a memory guard.
                if text.len() <= OSC52_MAX_BYTES {
                    push_osc52(&self.clipboard_store, Osc52Target::of(ty), text);
                    self.clipboard_dirty.store(true, Ordering::Release);
                }
            }
            // OSC 52 PASTE (load): the app running in the PTY asked to READ the
            // system clipboard. alacritty only emits this when `osc52` permits paste
            // (OnlyPaste/CopyPaste) — never under the default OnlyCopy — so it is
            // inert unless the user set `osc52_allow_paste = true`. Stash the reply
            // formatter; the app reads the clipboard, caps + formats, writes to PTY.
            Event::ClipboardLoad(ty, formatter) => {
                push_osc52(&self.clipboard_load, Osc52Target::of(ty), formatter);
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

/// [`Terminal::at_clean_prompt`] bounds: the cursor may sit at most this many rows
/// below the open prompt's input line (typed input soft-wrapping that far), and
/// at most this many (blank) lines may lie above the prompt.
const CLEAN_PROMPT_MAX_ROWS: i64 = 8;
const CLEAN_PROMPT_MAX_ABOVE: i64 = 256;

/// Blank cells right after a prompt's `B` that prove nothing was typed on its
/// row (see [`Terminal::command_line_blank`]): a command line never starts with
/// this many spaces, while the right prompt zsh leaves on an accepted line
/// (RPROMPT without `TRANSIENT_RPROMPT`) sits further along that row.
const EMPTY_INPUT_CELLS: usize = 3;

/// Upper bound on the lines one `parser.advance` call may scroll (see
/// [`Terminal::advance_slice`]). Kept below alacritty's 1000-row `Storage` row
/// cache so the room made before it never frees rows the next scroll would have
/// to reallocate (an 8 KiB `yes` chunk as one sub-slice measured 5 MB/s from
/// that churn).
const SLICE_MAX_LINES: usize = 768;

/// Block size of the line-feed count when a dense flood must be split (see
/// [`Terminal::advance_slice`]).
const SLICE_BLOCK: usize = 256;

/// Line-feed bytes in `s` (LF 0x0A, VT 0x0B and FF 0x0C all line-feed in vte).
/// Counted in u8 lanes over ≤ 255-byte chunks so it auto-vectorizes: no
/// per-match cost even on `yes`-dense data (a `memchr` iterator is far slower
/// there).
fn count_line_feeds(s: &[u8]) -> usize {
    s.chunks(255)
        .map(|c| c.iter().fold(0u8, |n, &b| n + (b.wrapping_sub(0x0a) < 3) as u8) as usize)
        .sum()
}

/// The exact OSC 133 introducer the scanner matches after `ESC ]`.
const OSC133_PREFIX: &[u8] = b"133;";

/// The `redraw=0` parameter of an OSC 133 `A` (kitty's extension): the shell
/// does NOT repaint its whole prompt after a resize — readline repaints only the
/// LAST line of a multi-line PS1 — so the resize wipe must not erase the rest.
const REDRAW_OFF: &[u8] = b"redraw=0";

/// [`Scan::Payload`] `kv`: the current parameter can no longer be `redraw=0`.
const KV_MISMATCH: u8 = u8::MAX;

/// Advance the `redraw=0` match of the current OSC 133 parameter by one byte.
fn redraw_step(kv: u8, b: u8) -> u8 {
    match REDRAW_OFF.get(usize::from(kv)) {
        Some(&want) if want == b => kv + 1,
        _ => KV_MISMATCH,
    }
}

/// Cap on the sixel carry buffer (`sixel_buf`). A never-terminated or hostile
/// sixel cannot grow memory without bound: past this the scanner latches
/// `sixel_overflow`, keeps scanning for the terminator to resync, then DROPS the
/// image (correct-or-absent). Two bytes per pixel of [`crate::sixel::SIXEL_CAPS`]
/// (32 MB): a dithered full-window frame runs about one byte per pixel, so a
/// 4K one (8.3 Mpx) fits — a 4 MiB cap dropped it though its pixels did.
const SIXEL_MAX_BYTES: usize = crate::sixel::SIXEL_CAPS.max_pixels as usize * 2;

/// Clamp on the reserved cell-rows for one image (the injected line-feeds). Even
/// a legitimately tall image cannot scroll the grid without bound.
const MAX_IMAGE_ROWS: usize = 1024;

/// Cap on ONE Kitty APC's accumulated control+base64 payload (`apc_buf`), mirroring
/// [`SIXEL_MAX_BYTES`]. A single APC chunk is ≤ 4096 base64 bytes in practice, so
/// 4 MiB is generous; a never-terminated / hostile APC latches `apc_overflow`,
/// keeps scanning to resync, then DROPS (correct-or-absent).
const APC_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Cap on ONE OSC's payload. vte (built with `std`) buffers an OSC in an
/// unbounded `Vec` until its terminator, so `printf '\e]0;'; base64 /dev/urandom`
/// would grow memory without bound. No OSC JeTTY honors needs more: OSC 52's
/// commit cap is [`OSC52_MAX_BYTES`] decoded (~137 KiB of base64), titles are
/// sanitized to 256 chars, hyperlink URIs are short. Past the cap the scanner
/// ends the OSC for vte (CAN — dispatching the truncated head, which every
/// handler above caps or rejects) and DISCARDS the rest up to its terminator,
/// so none of it leaks onto the grid as text.
const OSC_MAX_BYTES: u32 = 1024 * 1024;

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

/// Image work (bytes an image writes: its inflated `o=z` payload and decoded
/// RGBA) one byte of output earns, and the most [`Terminal::image_work`] banks —
/// four full-size (16 Mpx) images. Decoding runs on the UI thread and a few KB
/// of sixel `!` repeats, PNG or zlib can unpack to 64 MB, so every image is paid
/// for from the bank before it allocates: photos and screenshots earn more than
/// they cost, while a flood of such bombs empties the bank and is then dropped
/// (correct-or-absent) — the work stays a bounded multiple of the output.
const IMAGE_WORK_PER_BYTE: u64 = 64;
const IMAGE_WORK_MAX: u64 = 256 * 1024 * 1024;

/// The Kitty reply to an image the image-work bank cannot pay for right now.
const IMAGE_BUSY: &str = "EBUSY";

/// [`crate::sixel::content_id`] protocol tags: the same bytes are a different
/// image to the sixel and the Kitty decoder.
const SIXEL_TAG: u32 = 0;
const KITTY_TAG: u32 = 1;

/// Upper clamp for a parsed OSC 133 D exit code. Shell exit statuses are 8-bit
/// (0..=255; a signal death reports 128+signum), so anything larger is
/// non-conformant. Clamping here keeps the running parse from overflowing `u32`
/// on a crafted `D;<many digits>` and guarantees the later `as i32` cast stays
/// non-negative (a wrong-sign code could otherwise flip the failed/ok verdict).
const EXIT_CODE_MAX: u32 = 255;

/// State of the tiny escape scanner, carried across [`Terminal::feed`] calls so
/// a sequence split across PTY chunks resumes mid-parse. It recognizes OSC 133
/// prompt marks (`ESC ]`), sixel DCS images (`ESC P … q … ST`), Kitty APC images
/// (`ESC _ G`) and — only while anchors exist — the history-rewriting CSIs and
/// RIS ([`IsolatedSeq`]) in ONE single-ESC state machine. Ground uses a
/// `memchr(ESC)` fast path, so a stream with no escapes costs one SIMD scan per
/// feed and nothing per byte.
///
/// The OSC terminator set `{0x07 BEL, 0x18 CAN, 0x1A SUB, 0x1B ESC}` and the `;`
/// separator match vte 0.15's OSC framing (advance_osc_string). The DCS
/// terminators are DIFFERENT — `{0x18 CAN, 0x1A SUB, 0x1B ESC, 0x9C ST}`, and
/// **0x07 BEL is a DATA byte inside a DCS**, never a terminator (vte's
/// advance_dcs_passthrough). The two terminator sets are kept strictly separate.
/// A Kitty APC ends at BEL as well as at ST, as kitty ends it (vte, whose APC
/// string ends only at ESC / CAN / SUB, is handed an ST of its own there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scan {
    /// Not inside an escape; scan forward to the next ESC via memchr.
    Ground,
    /// Saw ESC; a following `]` (0x5d) opens an OSC, `P` (0x50) opens a DCS.
    Esc,
    /// Saw `ESC [` at the end of a feed: collecting up to [`CSI_PARAMS_MAX`]
    /// parameter bytes (`0-9`, `?`) to recognize the few CSIs that rewrite
    /// history ([`IsolatedSeq`]). Any other byte bails to Ground at once (an SGR
    /// costs one extra step); vte parses every CSI itself regardless — this only
    /// decides where `feed` splits its slices.
    Csi { params: [u8; CSI_PARAMS_MAX], len: u8 },
    /// Inside a private-mode CSI (`ESC [ ?`), watching for DECSET/DECRST of the
    /// modes alacritty does not track — the X10 (`9`) and urxvt (`1015`) mouse
    /// modes and the color-scheme reports (`2031`) — and for the color-scheme
    /// query `CSI ? 996 n`. `cur` is the parameter being read (saturating), `hit`
    /// the modes named so far ([`DECSET_X10`] / [`DECSET_URXVT`] /
    /// [`DECSET_2031`], plus [`DECSET_MULTI`] past a separator). Mirrors vte's
    /// CSI states: C0, DEL and high bytes stay, ESC restarts, CAN/SUB and
    /// anything that is not a plain `Pm h/l` / `Ps n` sequence end it.
    Decset { cur: u16, hit: u8 },
    /// Inside `ESC ]`, matching the `133;` prefix byte by byte (`n` matched).
    Prefix { n: u8 },
    /// Inside `ESC ]`, matching the `9;4;` progress prefix (`n` matched, ≥ 1:
    /// the `9` was read in [`Scan::Prefix`]).
    Prefix94 { n: u8 },
    /// Matched `9;4;`: collecting `st ; pr` up to the terminator. `field` 0 is
    /// `st`, 1 is `pr`, 2 means "past the two known fields" (extra parameters are
    /// ignored); `digits` counts the current field's digits. Any other byte in
    /// the first two fields is malformed → [`Scan::Skip`] (no update).
    Progress { st: Option<u8>, pr: Option<u16>, field: u8, digits: u8 },
    /// Matched `133;`; collecting the letter (A/B/C/D) and the first `;code`.
    /// `code_done` is set by a SECOND `;` so `aid=<n>` params never corrupt the
    /// exit code (only the first param after the letter is the exit status).
    /// `kv` matches the current parameter against [`REDRAW_OFF`];
    /// `no_redraw` latches once one matched (an `A;redraw=0`).
    Payload { letter: u8, code: Option<u32>, in_code: bool, code_done: bool, kv: u8, no_redraw: bool },
    /// Inside some OTHER OSC (title/hyperlink/color); skip to its terminator.
    Skip,
    /// An OSC overran [`OSC_MAX_BYTES`]: vte was handed CAN to end it, and every
    /// byte up to the OSC's own terminator is DROPPED (never reaches alacritty).
    OscDiscard,
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
    /// until an APC terminator (ST / BEL / 8-bit ST; CAN / SUB abort).
    Apc,
    /// An APC that is NOT `_G…` (some other APC use): skip to the terminator,
    /// accumulate nothing, emit nothing.
    ApcOther,
    /// Inside `ESC [ >`, `ESC [ <` or `ESC [ =` — a kitty keyboard-protocol
    /// candidate (`CSI > Ps u` push, `CSI < Ps u` pop, `CSI = Ps ; Pm u` set),
    /// read so JeTTY can mirror each screen's flag-stack DEPTH (`kbd_depth`;
    /// alacritty keeps the stacks private). `n` = the first parameter
    /// (saturating, like vte), `seps` = parameter separators so far, `odd` =
    /// something vte would not dispatch as a plain push/pop/set (an
    /// intermediate, a second marker, too many parameters).
    KbdCsi { marker: u8, n: u16, seps: u8, odd: bool },
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
    /// Column of that B: where the command line starts on the `input` row
    /// (the leftmost B on that row — see the `B` arm of `bind_mark`).
    input_col: usize,
    /// OSC 133 C — command-output start.
    output: Option<i64>,
    /// OSC 133 D exit code (None = no/empty/non-numeric code → unknown).
    exit: Option<i32>,
    /// Set once a D arrives (or a later A closes an abandoned command, e.g. ^C).
    finished: bool,
    /// The shell repaints this whole prompt after a resize (false for an
    /// `A;redraw=0`, e.g. bash: readline repaints only a prompt's last line).
    redraws: bool,
}

/// The shell command between its prompt mark (`A`) and its completion (`D`) —
/// the Run & Notify state. Deliberately NOT row-anchored: nothing that drops the
/// `marks` (reflow, scroll overflow, RIS) can lose a completion.
#[derive(Clone, Copy, Debug)]
struct OpenCmd {
    /// Monotonic instant stamped at the C mark (command start), so a duration can
    /// be computed at D. `None` when no C was seen (a shell integration that
    /// emits only A+D) — the completion then carries an unknown duration.
    started_at: Option<std::time::Instant>,
}

/// One live inline-image placement (a decoded sixel / Kitty image in the grid).
///
/// On the PRIMARY screen `abs_line` is the ABSOLUTE grid line of the image's
/// top-left cell — the exact analogue of `CmdBlock::prompt` (`abs_top`-relative,
/// survives scrolling). On the ALT screen (`Terminal::alt_placements`) it is the
/// plain screen row: the alt screen has no scrollback. The decoded RGBA lives
/// behind an `Arc` so the render layer can clone it cheaply to upload to (each
/// window's) GPU without copying, and so a texture evicted then
/// re-scrolled-into-view can re-upload. `cols`/`rows` are the cell footprint.
#[derive(Clone, Debug)]
struct ImagePlacement {
    id: u64,
    abs_line: i64,
    col: u16,
    cols: u16,
    rows: u16,
    /// The rectangle the image draws in, in CELLS from the footprint's
    /// top-left: `[x, y, w, h]` (see [`Terminal::image_geometry`]). Cells, not
    /// pixels, so it scales with the cell size: a font or DPI change that
    /// keeps the grid's dimensions keeps the image in its rows.
    draw: [f32; 4],
    image: Arc<crate::sixel::SixelImage>,
    /// The Kitty protocol image id (`i=`) or number (`I=`) this placement was
    /// created from, so `a=d,d=i,i=N` can target it (amendment A6). `None` for
    /// sixel placements and anonymous Kitty transmits.
    kitty_id: Option<u32>,
    /// The Kitty placement id (`p=`, nonzero) — a display reusing the same image
    /// id AND placement id REPLACES this placement (spec).
    kitty_placement: Option<u32>,
    /// Which inline-image protocol created this placement. Lets a Kitty
    /// delete-all (`d=a`/`d=A`) clear Kitty images — INCLUDING anonymous ones,
    /// which carry no `kitty_id` — without wiping a coexisting sixel image (M2).
    is_kitty: bool,
    /// ALT-screen sixels only: the covered cells carry [`SIXEL_CELL`]. A sixel's
    /// pixels ARE those cells, so once a TUI writes any of them the image is gone
    /// (checked after each sub-slice — see `check_alt_placements`).
    marks_cells: bool,
}

impl ImagePlacement {
    /// Whether `self`'s cell rectangle lies entirely inside `other`'s (same
    /// screen, same anchor space).
    fn covered_by(&self, other: &ImagePlacement) -> bool {
        self.abs_line >= other.abs_line
            && self.abs_line + self.rows as i64 <= other.abs_line + other.rows as i64
            && self.col >= other.col
            && self.col + self.cols <= other.col + other.cols
    }

    /// Put the image's left edge at cursor column `col` of a `cols`-wide grid:
    /// a sixel starts there and is cut at the grid's edge, as in xterm; a Kitty
    /// image moves left to fit (A8).
    fn anchor_at(&mut self, col: usize, cols: usize) {
        let (col, cols) = (col.min(cols - 1) as u16, cols as u16);
        if self.is_kitty {
            self.cols = self.cols.min(cols);
            self.col = col.min(cols - self.cols);
        } else {
            self.col = col;
            self.cols = self.cols.min(cols - col);
        }
    }
}

/// One image in the Kitty registry ([`Terminal::kitty_images`]).
#[derive(Debug)]
struct StoredImage {
    /// Its id: the `i=` it was sent with, or the one an `I=` transmit is given.
    id: u32,
    /// Its `I=` image number (0 = none).
    number: u32,
    /// Sent on the alternate screen. Each screen has images of its own, as in
    /// kitty: a full-screen program's ids never meet the shell's.
    alt: bool,
    /// Its place among the transmits (`I=` names the newest with a number).
    seq: u64,
    /// Content id (the texture key) and pixels.
    content: u64,
    image: Arc<crate::sixel::InlineImage>,
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

/// The exact OSC 9;4 introducer (ConEmu / Windows Terminal progress) the scanner
/// matches after `ESC ]`. `OSC 9 ; <text>` (an iTerm2 notification) shares the
/// `9;` head, so only a full `9;4;` enters [`Scan::Progress`].
const OSC94_PREFIX: &[u8] = b"9;4;";

/// Most digits one OSC 9;4 field may carry (`100` is the largest meaningful
/// value). A longer run is malformed: the OSC is skipped, nothing changes.
const PROGRESS_MAX_DIGITS: u8 = 3;

/// What a program reports through OSC 9;4 (`ESC ] 9 ; 4 ; st ; pr ST`): cargo
/// (`CARGO_TERM_PROGRESS_TERM_INTEGRATION=true`), Claude Code, winget, … The
/// state names follow ConEmu's spec (`st` 1–4); `st = 0` clears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    /// `st = 1`: a determinate percentage.
    Normal,
    /// `st = 2`: something failed (the value is optional).
    Error,
    /// `st = 3`: busy, no percentage.
    Indeterminate,
    /// `st = 4`: paused / waiting (the value is optional).
    Paused,
}

/// A tab's current OSC 9;4 progress. `value` is a percentage `0..=100`; `None`
/// for [`ProgressState::Indeterminate`], and for an error/paused report that
/// carried no value with none before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    pub value: Option<u8>,
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
    /// Pending OSC 52 clipboard-copy texts + flag, shared with the `EventProxy`;
    /// consumed by [`Terminal::take_clipboard_stores`].
    clipboard_store: Osc52Pending<String>,
    clipboard_dirty: Arc<AtomicBool>,
    /// Pending OSC 52 clipboard-paste reply formatters + flag, shared with the
    /// `EventProxy`; consumed by [`Terminal::take_clipboard_loads`]. Inert unless
    /// `osc52_mode` permits paste.
    clipboard_load: Osc52Pending<ClipboardLoadFmt>,
    clipboard_load_dirty: Arc<AtomicBool>,
    /// The OSC 52 mode this terminal was built with. Stored so `set_scrollback_lines`
    /// (which rebuilds the alacritty `Config`) preserves it instead of silently
    /// reverting an enabled paste back to the default `OnlyCopy`. Toggled by
    /// [`Terminal::set_osc52_allow_paste`].
    osc52_mode: Osc52,
    /// Whether alacritty's kitty keyboard protocol support (`CSI ? u` query,
    /// `CSI > u` push / `CSI < u` pop) is enabled. Off by default: an app that
    /// pushes kitty flags expects kitty-encoded keys, so this must only be turned
    /// on (via [`Terminal::set_kitty_keyboard`]) once the app's key encoder honors
    /// [`Terminal::kitty_keyboard_flags`]. Carried through every `Config` rebuild.
    kitty_keyboard: bool,
    /// Bold text in one of the 8 normal ANSI colors renders in its bright twin
    /// (config `bold_is_bright`, default off). See [`Terminal::set_bold_is_bright`].
    bold_is_bright: bool,
    /// The cursor shape a program's `CSI 0 SP q` (and a fresh terminal) falls
    /// back to — the user's `[cursor] shape`. Block unless set via
    /// [`Terminal::set_default_cursor_shape`]; carried through every `Config`
    /// rebuild.
    default_cursor: CursorShape,
    /// The active scrollback-search query (what the user typed, capped at
    /// [`SEARCH_MAX_QUERY`] chars). Empty = no active search.
    search_query: String,
    /// Compiled smart-case literal regex for `search_query` (None when the
    /// query is empty or failed to compile — both render as "0/0").
    search_regex: Option<RegexSearch>,
    /// All matches across history+viewport, topmost→bottommost, capped at
    /// [`SEARCH_MAX_MATCHES`], on the absolute line scale: they stay on their
    /// text while output scrolls. New output is matched by
    /// [`Terminal::search_refresh`], which the app calls (throttled) on output.
    search_matches: Vec<SearchMatch>,
    /// Index into `search_matches` of the CURRENT match (the counter's "n").
    search_current: usize,
    /// What `search_matches` was collected from — `None` until a query
    /// collects. What lets [`Terminal::search_refresh`] re-read only the lines
    /// that can have changed since.
    search_scan: Option<SearchScan>,
    /// Bumped on every switch between the primary and the alternate screen:
    /// stored search matches belong to the screen they were collected on.
    screen_switches: u64,
    /// While set (keyboard copy-mode, hint mode — modes that point at text on
    /// the screen), the view stays on the lines it shows even at the live
    /// bottom: output scrolls in below them instead of moving them, as tmux's
    /// copy mode does. An anchor, so the count of scrolled lines stays exact.
    /// See [`Terminal::set_view_pinned`].
    view_pinned: bool,
    /// While a double-click selection that began on a plain-text URL is live:
    /// `true` when the selection's START is that URL's first cell, `false` when
    /// its END is the URL's last cell (the drag went left of it). The URL is
    /// re-detected from that endpoint on every update — alacritty moves the
    /// selection with scrolling output, so no stored point goes stale. Reset
    /// by every new selection ([`Terminal::set_selection`]).
    url_select: Option<bool>,
    /// While a double-click (word / bracket-pair) selection is live: where the
    /// double-clicked cell is, as `(lines below the selection's first cell,
    /// column)` — alacritty moves the selection with scrolling output, so the
    /// offset stays true. The selection is stored as the cells it resolved to
    /// (see [`Terminal::select_words`]); every drag re-derives it from that
    /// cell. Reset by every new selection.
    word_select: Option<(i32, usize)>,
    /// Absolute grid-line index of the active-region top (grid `Line(0)`).
    /// Advanced by `history_size()` growth in [`Terminal::advance_piece`] /
    /// [`Terminal::flush_sync`]; the stable anchor that lets OSC 133 prompt marks
    /// and inline images survive scrolling. Only its DIFFERENCES with an anchor's
    /// absolute line matter, so its absolute offset is arbitrary — what has to
    /// hold is that it advances by exactly the number of lines scrolled off the
    /// top between an anchor's bind and every later read.
    ///
    /// alacritty 0.26 has no saturation-proof scroll counter: once
    /// `history_size()` reaches the grid's max it pins while scrolling continues.
    /// So while anything is anchored (marks, primary-screen images), before every
    /// sub-slice [`Terminal::make_room`] trims the oldest history until the most
    /// lines that sub-slice can scroll fit strictly below the max;
    /// `history_size()` then never pins and its growth stays an EXACT scroll
    /// count (a full scrollback no longer disables marks, Run & Notify or
    /// images). With nothing anchored nothing reads it: slices go through whole
    /// and it may under-count, harmlessly. Only a sub-slice that scrolls more
    /// than it could be bounded (a sync-update replay, a long `CSI Ps b` repeat,
    /// a screen taller than the scrollback) pins it — that drops every anchor
    /// once (correct-or-absent) and tracking resumes exactly on the next
    /// sub-slice. FROZEN on the alt screen and across an alt-screen toggle (that
    /// history change is not a scroll); toggles are isolated into their own
    /// sub-slice — inside a synchronized update too — so no primary output is
    /// lost with them.
    abs_top: i64,
    /// The scrollback cap (the alacritty grid's max). While anchors exist, room
    /// is made BEFORE each sub-slice, so the retained history sits between this
    /// minus the last sub-slice's bound (≤ [`SLICE_MAX_LINES`] + 1) and this
    /// minus one; with none, it fills to this.
    scrollback_limit: usize,
    /// Bumped whenever every anchor is dropped and/or `abs_top` re-anchored (a
    /// reflow, a scroll overflow, RIS). An image placement captures it before
    /// its reserve injection and is discarded if it changed (the anchor it
    /// computed is no longer meaningful).
    anchor_epoch: u64,
    /// The command between its prompt (`A`) and completion (`D`), tracked apart
    /// from the row-anchored `marks` so Run & Notify completions survive anything
    /// that drops anchors (a reflow during a long build, a scroll overflow, RIS).
    cur_cmd: Option<OpenCmd>,
    /// Escape scanner state (OSC 133 + sixel DCS), persisted across `feed` calls
    /// (chunk boundaries).
    scan: Scan,
    /// Per-tab semantic prompt marks (OSC 133 A/B/C/D) in append order, pruned to
    /// the live scrollback window on each bind.
    marks: VecDeque<CmdBlock>,
    /// Whether `marks` is in ascending `prompt` order — true unless a prompt was
    /// bound ABOVE an older one (a shell redrawing its prompt higher up). While it
    /// holds, the out-of-window marks are a prefix and a suffix, so `prune_marks`
    /// trims both ends in O(pruned) instead of rescanning every mark per prompt.
    marks_sorted: bool,
    /// Lifetime count of DISTINCT OSC 133 `A` prompt marks this terminal has
    /// seen (post-dedup; never decremented, survives mark pruning). The
    /// run-selection-in-new-tab readiness signal: `> 0` means the shell emits
    /// prompt marks. One u64, incremented only inside the existing `A` arm.
    prompts_seen: u64,
    /// Latched TRUE the first time this tab runs a command (an OSC-133 `C`, or a
    /// `D` from integrations that never emit `C`). One of the gates on the resize
    /// "clean-prompt" clear (p10k-scatter fix) — see [`Terminal::at_clean_prompt`]
    /// for the full content-safety argument. Never reset.
    saw_command_output: bool,
    /// Command completions discovered during `feed()` (OSC 133 `D`). Drained by
    /// the app on the PTY-drain pass via [`Terminal::take_completions`]. Empty in
    /// the common case; bounded by [`MAX_PENDING_COMPLETIONS`]. A plain `Vec` on
    /// `&mut self` (not an `Arc`/atomic like `bell`) is correct: `D` is handled by
    /// our own scanner inside `feed(&mut self)`, never the async `EventProxy`.
    completed: Vec<CommandCompletion>,
    /// The program-reported OSC 9;4 progress (`None` = no progress shown).
    /// Cleared by `9;4;0`, an OSC 133 `A` or `D` (the program that reported it
    /// is over) and RIS.
    progress: Option<Progress>,
    /// `progress` changed since the last [`Terminal::take_progress_update`].
    progress_dirty: bool,
    /// An OSC 133 `A`, `C` or `D` bound since the last
    /// [`Terminal::take_command_marks`] — the shell started or finished a
    /// command (smart tab titles re-derive "what runs here" on it).
    cmd_marks_dirty: bool,
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
    /// Live PRIMARY-screen inline-image placements (sixel / Kitty), append order ≈
    /// ascending `abs_line`. Pruned to the scrollback window (span-intersection),
    /// dropped on reflow / anchor loss (correct-or-absent). Bounded by
    /// [`MAX_PLACEMENTS`] and [`MAX_PLACEMENT_BYTES`].
    placements: VecDeque<ImagePlacement>,
    /// Running sum of `image.rgba.len()` across `placements` (the live-bytes
    /// budget), maintained incrementally so pruning never re-sums the deque.
    placement_bytes: u64,
    /// Live ALT-screen placements (TUI image previews: yazi, ranger, image.nvim),
    /// anchored at screen rows. Kitty ones last until deleted (`a=d`) or until the
    /// alt grid is cleared / scrolled / reset (alacritty FULL damage); sixel ones
    /// also vanish once a covered cell is written. All go on alt-screen enter /
    /// exit and on resize. Bounded like `placements` (own budget).
    alt_placements: VecDeque<ImagePlacement>,
    alt_placement_bytes: u64,
    /// Live cell pixel size shared with the `EventProxy` ([`pack_cell_px`]) so
    /// the `\e[14t` reply reports real metrics (A5). Updated by `set_cell_px`.
    cell_px: Arc<AtomicU64>,
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
    /// Bounded transmit-then-put image registry, least recently used first. A
    /// later `a=p,i=N` displays without re-transmitting — or re-hashing: the
    /// content id is computed once, at transmit. LRU-evicted by count and
    /// bytes ([`MAX_KITTY_STORED`] / [`MAX_KITTY_STORED_BYTES`]).
    kitty_images: VecDeque<StoredImage>,
    /// Running sum of `rgba.len()` across `kitty_images` (the registry byte budget).
    kitty_stored_bytes: u64,
    /// Kitty transmits stored so far ([`StoredImage::seq`]).
    kitty_seq: u64,
    /// The image-work bank: bytes images may still write (decoded RGBA,
    /// inflated payloads). Every byte fed earns [`IMAGE_WORK_PER_BYTE`], up to
    /// [`IMAGE_WORK_MAX`]; an image is paid for before it allocates, or dropped.
    image_work: u64,
    /// Payload bytes of the OSC being scanned (bounded by `osc_cap`). Persists
    /// across `feed` calls like `scan`.
    osc_len: u32,
    /// DECSET 9 (X10 mouse reporting: button presses only) is on. alacritty
    /// ignores this mode, so the scanner tracks it ([`Scan::Decset`]).
    mouse_x10: bool,
    /// DECSET 1015 (urxvt decimal mouse encoding) is on — tracked here for the
    /// same reason.
    mouse_urxvt: bool,
    /// The OSC payload cap in force: [`OSC_MAX_BYTES`], lowered only by tests so
    /// the vte differential fuzz reaches it cheaply.
    osc_cap: u32,
    /// `minimum_contrast` (WCAG ratio, `1.0` = off): text whose final color
    /// contrasts less than this with its cell background is pushed toward
    /// white/black in [`Terminal::snapshot`]. Set by the app
    /// ([`Terminal::set_minimum_contrast`]).
    min_contrast: f32,
    /// DEC private mode 2031 (color-scheme change reports, `CSI ? 2031 h`) is
    /// on: a palette change sends `CSI ? 997 ; 1|2 n`. Tracked by the scanner
    /// (alacritty doesn't know the mode) and shared with the `EventProxy`, which
    /// answers the mode's DECRQM query with it.
    color_reports: Arc<AtomicBool>,
    /// JeTTY's mirror of alacritty's kitty keyboard flag stacks — their DEPTHS
    /// only — `[primary, alternate]` (alacritty swaps the two stacks with the
    /// screens). Fed by the scanner (`Scan::KbdCsi`); zeroed whenever alacritty
    /// clears the stacks (RIS, a protocol toggle). It keeps a push flood below
    /// alacritty's panicking limit ([`KBD_STACK_MAX`]) and lets a prompt undo
    /// what a dead program left pushed ([`Terminal::restore_kbd`]).
    kbd_depth: [u16; 2],
    /// The keyboard state the running command started from; `None` outside one.
    kbd_window: Option<KbdWindow>,
    /// What the xterm-conformance corrections over alacritty's `Handler`
    /// remember (the scroll region mirror, the DECSCNM reverse-video screen) —
    /// see `handler.rs`.
    vt: VtState,
    /// Test-only: every byte handed to vte, in order (the differential fuzz
    /// replays it through a model of vte's state machine).
    #[cfg(test)]
    vte_log: Option<Vec<u8>>,
    /// The search match cap ([`Terminal::search_cap`]), lowered by tests to
    /// reach it.
    #[cfg(test)]
    search_cap: usize,
}

/// What [`Terminal::viewport_rows_chars`] puts in a wide char's spacer cell
/// (the right half of its glyph). NUL: a C0 control, so never a cell's char.
pub const WIDE_SPACER: char = '\0';

/// Maximum scrollback-search query length in chars (bounds per-keystroke DFA
/// builds and the search-bar layout).
pub const SEARCH_MAX_QUERY: usize = 256;
/// Maximum number of collected search matches — the most recent ones are
/// kept (see `Terminal::search_collect`); the counter shows "5000+" when this
/// cap is hit.
pub const SEARCH_MAX_MATCHES: usize = 5000;

/// A cell on the absolute line scale `abs_top` keeps for marks and images:
/// unlike a grid `Point` it names the same text while output scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AbsPoint {
    line: i64,
    col: usize,
}

/// A scrollback-search match: its first and last cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SearchMatch {
    start: AbsPoint,
    end: AbsPoint,
}

/// What the stored search matches were collected from (see
/// [`Terminal::search_collect`]).
#[derive(Clone, Copy, Debug)]
struct SearchScan {
    /// `abs_top` then: the lines from there down were on the screen and may
    /// have been rewritten since; the ones above were history, which never
    /// changes.
    abs_top: i64,
    /// The absolute line of the topmost row in the buffer then.
    top: i64,
    /// `anchor_epoch` then: a change means `abs_top` lost count (or a reflow
    /// re-anchored it), so the stored lines name other text.
    epoch: u64,
    /// `screen_switches` then: the matches belong to that screen.
    screens: u64,
    /// The collect stopped at [`SEARCH_MAX_MATCHES`]: the lines above its
    /// topmost match were never read.
    capped: bool,
}

/// Where the view was in the scrollback — the lines it showed — so a mode
/// that moved it can put it back ([`Terminal::view_spot`]). The default is the
/// live bottom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewSpot {
    /// The scroll offset then (0: the live bottom).
    offset: usize,
    /// The absolute line at the view's top then, and the anchor epoch it is on.
    top: i64,
    epoch: u64,
}

/// A link found under the pointer by [`Terminal::link_at`]: the target URI
/// plus where to underline it in the viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub uri: String,
    /// Viewport underline spans: `(row, col_start, col_end)` inclusive,
    /// clipped to the visible grid.
    pub spans: Vec<(usize, usize, usize)>,
    /// An OSC 8 hyperlink whose visible text is not its target: the text can
    /// name any address (or none), so the app shows `uri` before a click
    /// opens it. Never set for a plain-text URL — that text IS the target.
    pub hidden_target: bool,
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
        let kitty_keyboard = false;
        let default_cursor = CursorShape::Block;
        let config = term_config(scrollback_limit, osc52_mode, kitty_keyboard, default_cursor);
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
        let cell_px = Arc::new(AtomicU64::new(pack_cell_px(8.0, 16.0)));
        let theme_shared = Arc::new(Mutex::new(theme.clone()));
        let title_update = Arc::new(Mutex::new(None));
        let title_dirty = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));
        let clipboard_store = Arc::new(Mutex::new(Vec::new()));
        let clipboard_dirty = Arc::new(AtomicBool::new(false));
        let clipboard_load = Arc::new(Mutex::new(Vec::new()));
        let clipboard_load_dirty = Arc::new(AtomicBool::new(false));
        let color_reports = Arc::new(AtomicBool::new(false));
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
            color_reports: Arc::clone(&color_reports),
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
            kitty_keyboard,
            bold_is_bright: false,
            default_cursor,
            search_query: String::new(),
            search_regex: None,
            search_matches: Vec::new(),
            search_current: 0,
            search_scan: None,
            screen_switches: 0,
            view_pinned: false,
            url_select: None,
            word_select: None,
            abs_top: 0,
            scrollback_limit,
            anchor_epoch: 0,
            cur_cmd: None,
            scan: Scan::Ground,
            marks: VecDeque::new(),
            marks_sorted: true,
            prompts_seen: 0,
            saw_command_output: false,
            completed: Vec::new(),
            progress: None,
            progress_dirty: false,
            cmd_marks_dirty: false,
            sixel_buf: Vec::new(),
            sixel_overflow: false,
            pending_sixel_p2: 0,
            // Matches the EventProxy's default \e[14t reply (8×16) until the app
            // pushes real metrics via `set_cell_px` on the first reflow.
            cell_px_w: 8.0,
            cell_px_h: 16.0,
            placements: VecDeque::new(),
            placement_bytes: 0,
            alt_placements: VecDeque::new(),
            alt_placement_bytes: 0,
            cell_px,
            reply_tx,
            apc_buf: Vec::new(),
            apc_overflow: false,
            chunk_buf: Vec::new(),
            chunk_meta: None,
            chunk_count: 0,
            kitty_images: VecDeque::new(),
            kitty_stored_bytes: 0,
            kitty_seq: 0,
            image_work: IMAGE_WORK_MAX,
            osc_len: 0,
            mouse_x10: false,
            mouse_urxvt: false,
            osc_cap: OSC_MAX_BYTES,
            min_contrast: 1.0,
            color_reports,
            kbd_depth: [0, 0],
            kbd_window: None,
            vt: VtState::new(rows),
            #[cfg(test)]
            vte_log: None,
            #[cfg(test)]
            search_cap: SEARCH_MAX_MATCHES,
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

    /// Take the pending OSC 52 clipboard-COPY texts: at most one per selection,
    /// in arrival order. Empty in the common case (a lock-free flag check — zero
    /// idle cost, no allocation). Consuming; copies to the same selection between
    /// calls coalesce last-wins. The app writes each text to the selection it
    /// names (jetty-core does not depend on the clipboard backend).
    pub fn take_clipboard_stores(&mut self) -> Vec<(Osc52Target, String)> {
        if !self.clipboard_dirty.swap(false, Ordering::Acquire) {
            return Vec::new();
        }
        std::mem::take(&mut *self.clipboard_store.lock().unwrap())
    }

    /// Take the pending OSC 52 clipboard-PASTE reply formatters: at most one per
    /// selection, in arrival order. Empty in the common case (a lock-free flag
    /// check). Only ever non-empty when the terminal was built/toggled to permit
    /// paste (`osc52_allow_paste`), so the default (write-only) build never yields
    /// one. The app reads the named selection, caps it, calls the formatter, and
    /// writes the reply to the PTY — one reply per request.
    pub fn take_clipboard_loads(&mut self) -> Vec<(Osc52Target, ClipboardLoadFmt)> {
        if !self.clipboard_load_dirty.swap(false, Ordering::Acquire) {
            return Vec::new();
        }
        std::mem::take(&mut *self.clipboard_load.lock().unwrap())
    }

    /// The exact bytes [`Terminal::feed_notice`] feeds for `text`: the text in
    /// yellow on its own line, with every control character in it — ESC, BEL,
    /// all other C0 (TAB/CR/LF too), DEL, C1 — replaced by a visible U+FFFD.
    /// Only the SGR wrapper JeTTY adds itself reaches the parser as a sequence.
    pub fn notice_line(text: &str) -> String {
        let inert: String = text.chars().map(|c| if c.is_control() { '\u{fffd}' } else { c }).collect();
        format!("\x1b[33m{inert}\x1b[0m\r\n")
    }

    /// Show a one-line JeTTY notice (a shell or start-directory fallback…) in
    /// this terminal. Notices interpolate outside data — a directory name, a
    /// shell path, an OS error — so the text is made inert first
    /// ([`Terminal::notice_line`]): a name carrying `\e]52;…` (clipboard
    /// write), `\e]0;…` (title), a kitty APC or a query (whose reply would be
    /// typed into the shell) must show as text, never run through our parser.
    pub fn feed_notice(&mut self, text: &str) {
        self.feed(Self::notice_line(text).as_bytes());
    }

    /// Enable or disable OSC 52 clipboard PASTE (remote READ of the local clipboard).
    /// Copy (write) is always permitted. Paste is a SECURITY trade-off (a remote host
    /// / stray output can exfiltrate the clipboard), so it is OFF by default; the
    /// `osc52_allow_paste` config key opts in. Rebuilds the alacritty `Config`
    /// preserving the current scrollback limit. Idempotent-ish (re-applies set_options
    /// even when unchanged), so callers may invoke it unconditionally at tab spawn.
    pub fn set_osc52_allow_paste(&mut self, allow: bool) {
        self.osc52_mode = if allow { Osc52::CopyPaste } else { Osc52::OnlyCopy };
        self.term.set_options(self.config());
    }

    /// The alacritty `Config` for this terminal's current settings.
    fn config(&self) -> Config {
        term_config(self.scrollback_limit, self.osc52_mode, self.kitty_keyboard, self.default_cursor)
    }

    /// The cursor shape programs reset to (`CSI 0 SP q`) and a fresh screen
    /// shows — the user's `[cursor] shape`. A program's own DECSCUSR still wins
    /// until it resets. `HollowBlock` is accepted for completeness. No-op when
    /// unchanged (`set_options` repaints everything).
    pub fn set_default_cursor_shape(&mut self, shape: CursorShapeSnap) {
        let shape = match shape {
            CursorShapeSnap::Block => CursorShape::Block,
            CursorShapeSnap::Underline => CursorShape::Underline,
            CursorShapeSnap::Beam => CursorShape::Beam,
            CursorShapeSnap::HollowBlock => CursorShape::HollowBlock,
        };
        if self.default_cursor == shape {
            return;
        }
        self.default_cursor = shape;
        self.term.set_options(self.config());
    }

    /// Enable or disable kitty keyboard protocol support (progressive enhancement:
    /// `CSI ? u` query replies and the `CSI > u` / `CSI < u` flag stack). Turn it on
    /// only when the key encoder consults [`Terminal::kitty_keyboard_flags`] — an
    /// app that pushed flags expects kitty-encoded keys. A change clears both
    /// screens' flag stacks (alacritty `set_options`). No-op when unchanged.
    pub fn set_kitty_keyboard(&mut self, enabled: bool) {
        if self.kitty_keyboard == enabled {
            return;
        }
        self.kitty_keyboard = enabled;
        self.term.set_options(self.config());
        self.kbd_cleared();
    }

    /// Drop every keyboard / mouse mode a program may have left behind — the
    /// kitty keyboard flag stacks of BOTH screens, mouse reporting and its
    /// encodings, focus reporting and bracketed paste — without touching the
    /// screen, the scrollback or the cursor. The way out when a program died
    /// with them on (Ctrl+C sent as `\e[99;5u`, clicks typed as escape codes),
    /// e.g. a TUI killed on the alternate screen, where no prompt mark can
    /// restore anything.
    pub fn reset_input_modes(&mut self) {
        use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
        if self.kitty_keyboard {
            // A protocol toggle is alacritty's only way to clear BOTH screens'
            // stacks (`set_options`; the re-emitted title is a no-op app-side).
            let cursor = self.default_cursor;
            self.term.set_options(term_config(self.scrollback_limit, self.osc52_mode, false, cursor));
            self.term.set_options(term_config(self.scrollback_limit, self.osc52_mode, true, cursor));
        }
        self.kbd_cleared();
        for mode in [
            NamedPrivateMode::ReportMouseClicks,
            NamedPrivateMode::ReportCellMouseMotion,
            NamedPrivateMode::ReportAllMouseMotion,
            NamedPrivateMode::Utf8Mouse,
            NamedPrivateMode::SgrMouse,
            NamedPrivateMode::ReportFocusInOut,
            NamedPrivateMode::BracketedPaste,
        ] {
            self.term.unset_private_mode(PrivateMode::Named(mode));
        }
        self.mouse_x10 = false;
        self.mouse_urxvt = false;
    }

    /// Drop what a program that EXITED left on, before another one starts in
    /// this terminal (a shell that died right after starting hands over to the
    /// next): its unfinished synchronized update (flushed: its last words show),
    /// the alternate screen, and the keyboard / mouse / paste modes
    /// ([`Terminal::reset_input_modes`]) — a tmux that an rc file exec'd leaves
    /// all of them. The next program may never reset them itself.
    pub fn reset_after_exit(&mut self) {
        self.flush_sync();
        if self.alt_screen() {
            self.feed(b"\x1b[?1049l");
        }
        self.reset_input_modes();
    }

    /// The kitty keyboard protocol flags the running app has currently pushed,
    /// as the protocol's bit values: 1 disambiguate escape codes, 2 report event
    /// types, 4 report alternate keys, 8 report all keys as escape codes, 16
    /// report associated text. `0` when support is off or nothing is pushed (the
    /// legacy encoding applies). The alt screen keeps its own stack (alacritty).
    pub fn kitty_keyboard_flags(&self) -> u8 {
        let m = self.term.mode();
        let mut flags = 0u8;
        if m.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
            flags |= 1;
        }
        if m.contains(TermMode::REPORT_EVENT_TYPES) {
            flags |= 2;
        }
        if m.contains(TermMode::REPORT_ALTERNATE_KEYS) {
            flags |= 4;
        }
        if m.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
            flags |= 8;
        }
        if m.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
            flags |= 16;
        }
        flags
    }

    /// Whether the app enabled focus reporting (`\e[?1004h`): the host must then
    /// write `\e[I` on focus-in and `\e[O` on focus-out.
    pub fn focus_reporting(&self) -> bool {
        self.term.mode().contains(TermMode::FOCUS_IN_OUT)
    }

    /// Whether the app requested UTF-8 extended mouse coordinates (`\e[?1005h`).
    pub fn mouse_utf8(&self) -> bool {
        self.term.mode().contains(TermMode::UTF8_MOUSE)
    }

    /// Whether the app requested X10 mouse reporting (`\e[?9h`: button presses
    /// only). alacritty does not track this mode; the feed scanner does.
    pub fn mouse_x10(&self) -> bool {
        self.mouse_x10
    }

    /// Whether the app requested urxvt-style decimal mouse reports
    /// (`\e[?1015h`). Tracked by the feed scanner, like [`Terminal::mouse_x10`].
    pub fn mouse_urxvt(&self) -> bool {
        self.mouse_urxvt
    }

    /// A private-mode CSI ending in `fin` at byte `i` (`h` set / `l` reset /
    /// `n` DSR) named the modes in `hit`; `last` is its last parameter. Applies
    /// what alacritty doesn't track and returns the new flush `start`:
    ///
    /// * the X10 / urxvt mouse modes flip (no flush needed);
    /// * mode 2031 flips AFTER alacritty has caught up through this sequence, so
    ///   a DECRQM earlier in the same read is answered with the state it had
    ///   (the answer is rewritten in `EventProxy` from the shared flag);
    /// * `CSI ? 996 n` is answered with the color scheme — likewise after the
    ///   catch-up, so the reply keeps its place among alacritty's own replies.
    fn private_mode_csi(&mut self, bytes: &[u8], start: usize, i: usize, fin: u8, hit: u8, last: u16) -> usize {
        if fin == b'n' {
            if hit & DECSET_MULTI != 0 || last != DSR_COLOR_SCHEME {
                return start;
            }
            self.advance_slice(&bytes[start..=i]);
            self.apply_pending_sync();
            let _ = self.reply_tx.send(color_scheme_report(self.theme_rgb_bg()).to_vec());
            return i + 1;
        }
        let on = fin == b'h';
        if hit & DECSET_X10 != 0 {
            self.mouse_x10 = on;
        }
        if hit & DECSET_URXVT != 0 {
            self.mouse_urxvt = on;
        }
        if hit & DECSET_2031 != 0 {
            self.advance_slice(&bytes[start..=i]);
            self.apply_pending_sync();
            self.color_reports.store(on, Ordering::Relaxed);
            return i + 1;
        }
        start
    }

    /// vte BUFFERS a synchronized update (DEC 2026) until it ends or times out,
    /// answering the queries in it only then. Before the scanner itself answers
    /// one (or changes state a buffered query reads), apply the update, so that
    /// answer keeps its place behind the earlier ones (a program using DA1 as
    /// its end-of-probe sentinel would otherwise misread). The frame tears at
    /// most once, as for a prompt mark inside an update.
    fn apply_pending_sync(&mut self) {
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
    }

    /// The theme background without its alpha (what dark/light is judged on).
    fn theme_rgb_bg(&self) -> [u8; 3] {
        [self.theme.bg[0], self.theme.bg[1], self.theme.bg[2]]
    }

    /// Whether a program enabled the DEC 2031 color-scheme reports
    /// (`CSI ? 2031 h`).
    pub fn color_reports(&self) -> bool {
        self.color_reports.load(Ordering::Relaxed)
    }

    /// Bold text whose foreground is one of the 8 normal ANSI colors (SGR 30–37,
    /// or 256-color 0–7) renders in its bright twin (8–15) — the classic xterm
    /// `boldColors` look some color schemes are designed around. Default off (the
    /// bold face alone marks bold). Takes effect on the next snapshot.
    pub fn set_bold_is_bright(&mut self, on: bool) {
        self.bold_is_bright = on;
    }

    /// Replace the active theme at runtime. Also refreshes the copy shared with
    /// the `EventProxy` so subsequent OSC 10/11/12/4 color-query replies reflect
    /// the new theme (e.g. so nvim/fzf detect the right background).
    ///
    /// When a program enabled the DEC 2031 reports and the colors changed (not
    /// just the opacity), `CSI ? 997 ; 1|2 n` (dark / light) is queued on the
    /// reply channel — the app writes it to the PTY with the other replies
    /// ([`Terminal::drain_pty_writes`]).
    pub fn set_theme(&mut self, theme: Theme) {
        let recolored = self.theme.fg != theme.fg
            || self.theme.palette != theme.palette
            || self.theme.cursor != theme.cursor
            || self.theme.bg[..3] != theme.bg[..3];
        *self.theme_shared.lock().unwrap() = theme.clone();
        self.theme = theme;
        if recolored && self.color_reports() {
            let _ = self.reply_tx.send(color_scheme_report(self.theme_rgb_bg()).to_vec());
        }
    }

    /// `minimum_contrast`: the WCAG ratio every glyph's final color must reach
    /// against its cell background in [`Terminal::snapshot`] (1.0 = off — the
    /// default — and anything non-finite or below 1 is off; capped at 21).
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.min_contrast = crate::contrast::clamp_ratio(ratio);
    }

    /// The `minimum_contrast` in force (1.0 = off).
    pub fn minimum_contrast(&self) -> f32 {
        self.min_contrast
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
    /// Constraints:
    /// * `set_options` replaces the ENTIRE alacritty `Config`; it is built by
    ///   [`term_config`] (shared with `Terminal::new`) so no field reverts.
    /// * `set_options` also re-emits the CURRENT title (`Event::Title`/
    ///   `ResetTitle`) via the `EventProxy`. That is benign: the re-emitted
    ///   value equals what's already displayed (the app's apply path is a
    ///   no-op on unchanged titles, and manual renames are flagged app-side).
    pub fn set_scrollback_lines(&mut self, lines: usize) {
        // Preserve the OSC 52 mode: `..Default::default()` would reset `osc52` to
        // OnlyCopy, silently reverting an enabled `osc52_allow_paste` on every
        // scrollback change (amendment O2). Carry the stored mode through.
        self.term.set_options(term_config(lines, self.osc52_mode, self.kitty_keyboard, self.default_cursor));
        self.scrollback_limit = lines;
        // A shrink freed trimmed history rows, so stored search-match Points
        // can reference lines that no longer exist (wrong counter, Enter/F3
        // jumping to a clamped top-of-history). Re-collect, exactly like
        // `resize` does for reflow; cheap no-op when no search is active (F11).
        self.search_refresh();
        // A shrink also removes OLD history above `Line(0)` (which does NOT move,
        // so `abs_top` is unchanged): drop marks whose absolute line no longer
        // exists in the smaller live window. On the alt screen `grid()` is the
        // alt grid; the (inactive) primary now holds at most `lines`.
        let history = if self.term.mode().contains(TermMode::ALT_SCREEN) {
            lines
        } else {
            self.term.grid().history_size()
        };
        self.prune_marks(history);
        self.prune_placements(history);
        // Drop any in-progress Kitty chunk accumulation across a scrollback change.
        self.reset_kitty_chunks();
    }

    /// Feed PTY bytes to the terminal, intercepting OSC 133 semantic-prompt
    /// marks and sixel / Kitty images on the way through (alacritty_terminal 0.26
    /// / vte 0.15 drop them — an image never moves the cursor, so JeTTY reserves
    /// its cell rows itself; see `finish_sixel`).
    ///
    /// SPEED (#1): in `Ground` this is one `memchr(ESC)` per feed with zero
    /// per-byte work; a stream carrying no escapes reaches `advance_slice` exactly
    /// once (the whole buffer). Only inside an escape does the per-byte state
    /// machine run. Each input byte reaches alacritty exactly once (`start` is
    /// the first un-flushed byte); the scanner sub-advances alacritty up to AND
    /// INCLUDING a sequence's terminator so the grid is caught up before the
    /// cursor line is read, then decodes/places the image (or binds the 133).
    /// Image payloads are copied in runs and NOT handed to alacritty, which would
    /// only ignore them byte by byte: it gets each image's introducer and
    /// terminator, so its own parser enters and leaves the DCS/APC in lockstep
    /// (its DCS / APC states ignore every byte a payload holds). While anchors
    /// exist, ED 2/3, SU/DL, RIS and alt-screen toggles are advanced in
    /// sub-slices of their own so `abs_top` sees each one's history effect in
    /// isolation.
    /// Measured with `examples/feed_bench.rs`: within ±1.5% of the pre-scan code
    /// on every workload, including one with live prompt marks.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Output earns image work (see `image_work`): one add per feed.
        let earned = (bytes.len() as u64).saturating_mul(IMAGE_WORK_PER_BYTE);
        self.image_work = self.image_work.saturating_add(earned).min(IMAGE_WORK_MAX);
        let mut i = 0;
        let mut start = 0; // first byte not yet handed to alacritty
        // Index of the ESC that opened the CSI being scanned (0 when it arrived in
        // an earlier feed); where an isolated sequence's own sub-slice begins.
        let mut seq_start = 0;
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
                // Mirrors vte 0.15's Escape state exactly (`advance_esc`): C0
                // controls other than CAN/SUB, DEL and 0x80..=0xFF are executed or
                // ignored WITHOUT leaving Escape (so `ESC LF ]` still opens an OSC
                // there — leaving Esc here would let an OSC run uncapped), CAN/SUB
                // abort to Ground, ESC restarts. An isolated sequence's sub-slice
                // starts at THIS byte: vte's Escape state carries across advance
                // calls, and anything it executed since the ESC stays outside.
                Scan::Esc => {
                    self.scan = match b {
                        0x5d => {
                            // ']' opens an OSC.
                            self.osc_len = 0;
                            Scan::Prefix { n: 0 }
                        }
                        0x50 => Scan::DcsParams { p2: 0, field: 0, inter: false }, // 'P' opens a DCS
                        0x5f => Scan::ApcIntro,        // '_' opens an APC (Kitty graphics)
                        // '[' opens a CSI. A kitty keyboard push/pop/set is always
                        // followed (the flag-stack mirror, `kbd_depth`).
                        0x5b if matches!(bytes.get(i + 1), Some(b'<' | b'=' | b'>')) => {
                            self.scan = Scan::KbdCsi { marker: bytes[i + 1], n: 0, seps: 0, odd: false };
                            i += 2;
                            continue;
                        }
                        // Only while anchors exist is it checked for the few
                        // history-rewriting sequences ([`IsolatedSeq`]), by peeking
                        // ahead in this buffer — an SGR costs a byte compare or
                        // two, no extra scanner steps.
                        0x5b if self.has_anchors() => match peek_isolated_csi(&bytes[i + 1..]) {
                            CsiPeek::Isolate(len, kind) => {
                                if self.isolates(kind) {
                                    let k = i + 1 + len;
                                    start = self.isolate(bytes, start, i, k, kind);
                                    i = k;
                                    self.scan = Scan::Ground;
                                    continue;
                                }
                                Scan::Ground
                            }
                            // Cut off by the end of this feed: finish it byte-wise.
                            CsiPeek::Incomplete => {
                                seq_start = i;
                                Scan::Csi { params: [0; CSI_PARAMS_MAX], len: 0 }
                            }
                            // A private-mode CSI: watch it for the mouse modes
                            // alacritty ignores (the `?` is consumed here).
                            CsiPeek::Other if bytes.get(i + 1) == Some(&b'?') => {
                                self.scan = Scan::Decset { cur: 0, hit: 0 };
                                i += 2;
                                continue;
                            }
                            // A control vte executes inside the CSI: read on.
                            CsiPeek::Other if bytes.get(i + 1).is_some_and(|&c| csi_transparent(c)) => {
                                seq_start = i;
                                Scan::Csi { params: [0; CSI_PARAMS_MAX], len: 0 }
                            }
                            CsiPeek::Other => Scan::Ground,
                        },
                        // Without anchors only private-mode CSIs are followed (a
                        // one-byte peek; an SGR still costs no extra step). At the
                        // end of the feed — or past a control vte executes inside
                        // the CSI — finish it byte-wise.
                        0x5b => match bytes.get(i + 1) {
                            Some(b'?') => {
                                self.scan = Scan::Decset { cur: 0, hit: 0 };
                                i += 2;
                                continue;
                            }
                            Some(&c) if !csi_transparent(c) => Scan::Ground,
                            _ => {
                                seq_start = i;
                                Scan::Csi { params: [0; CSI_PARAMS_MAX], len: 0 }
                            }
                        },
                        // `ESC c` (RIS) resets the screen AND scrollback — and
                        // every terminal mode, including the two mouse modes,
                        // the color-scheme reports and the keyboard flag stacks
                        // mirrored here.
                        b'c' => {
                            let k = i + 1;
                            if self.has_anchors() {
                                start = self.isolate(bytes, start, i, k, IsolatedSeq::Reset);
                            }
                            self.mouse_x10 = false;
                            self.mouse_urxvt = false;
                            self.color_reports.store(false, Ordering::Relaxed);
                            self.kbd_cleared();
                            // A reset terminal shows no program's progress.
                            self.set_progress(None);
                            Scan::Ground
                        }
                        // ESC ESC restarts; C0 controls (vte executes them), DEL and
                        // 0x80..=0xFF (vte ignores them) all stay in Escape.
                        0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f..=0xff => Scan::Esc,
                        // CAN / SUB abort the escape; any other byte completes an
                        // ESC sequence, enters EscapeIntermediate (which can only
                        // reach a string state through another ESC), or opens a
                        // CSI / SOS / PM vte never buffers.
                        _ => Scan::Ground,
                    };
                    i += 1;
                }
                Scan::Csi { mut params, len } => {
                    let private = len > 0 && params[0] == b'?';
                    match b {
                        b'0'..=b'9' | b'?' if (len as usize) < params.len() => {
                            params[len as usize] = b;
                            self.scan = Scan::Csi { params, len: len + 1 };
                        }
                        // A private-mode CSI that outgrows the 5-byte window (a
                        // second parameter, a long number): keep reading it for
                        // the mouse modes (an over-long first number names none).
                        b';' if private => {
                            let hit = decset_bit(decset_param(&params[1..len as usize])) | DECSET_MULTI;
                            self.scan = Scan::Decset { cur: 0, hit };
                        }
                        b'0'..=b'9' if private => self.scan = Scan::Decset { cur: u16::MAX, hit: 0 },
                        // A kitty keyboard push/pop/set (its marker comes first).
                        b'<' | b'=' | b'>' if len == 0 => {
                            self.scan = Scan::KbdCsi { marker: b, n: 0, seps: 0, odd: false };
                        }
                        // vte executes C0 controls and ignores DEL / high bytes
                        // inside a CSI without leaving it: keep reading.
                        _ if csi_transparent(b) => {}
                        // Final byte: advance an isolated sequence in a sub-slice of
                        // its own (only while anchors exist — otherwise splitting
                        // buys nothing).
                        0x40..=0x7e => {
                            let k = i + 1;
                            if let Some(kind) = isolated_csi(&params[..len as usize], b) {
                                if self.isolates(kind) {
                                    start = self.isolate(bytes, start, seq_start, k, kind);
                                }
                            }
                            if private && matches!(b, b'h' | b'l' | b'n') {
                                let n = decset_param(&params[1..len as usize]);
                                start = self.private_mode_csi(bytes, start, i, b, decset_bit(n), n);
                            }
                            self.scan = Scan::Ground;
                        }
                        // ESC aborts the CSI and begins a new escape (vte parity).
                        0x1b => self.scan = Scan::Esc,
                        // Anything else (`;`, intermediates, C0): not a sequence we
                        // isolate — stop tracking it.
                        _ => self.scan = Scan::Ground,
                    }
                    i += 1;
                }
                Scan::KbdCsi { marker, n, seps, odd } => {
                    match b {
                        b'0'..=b'9' if seps == 0 => {
                            let n = n.saturating_mul(10).saturating_add(u16::from(b - b'0'));
                            self.scan = Scan::KbdCsi { marker, n, seps, odd };
                        }
                        b'0'..=b'9' => {}
                        // A parameter / sub-parameter separator (vte dispatches
                        // nothing past 32 of them; stay well clear of the edge).
                        b';' | b':' => {
                            let seps = seps.saturating_add(1);
                            self.scan = Scan::KbdCsi { marker, n, seps, odd: odd || seps >= 30 };
                        }
                        b'u' => {
                            start = self.kitty_kbd_csi(bytes, start, i, marker, n, odd);
                            self.scan = Scan::Ground;
                        }
                        // XTVERSION (`CSI > q`, `CSI > 0 q`), which vte drops.
                        b'q' if marker == b'>' && n == 0 && seps == 0 && !odd => {
                            start = self.xtversion(bytes, start, i);
                            self.scan = Scan::Ground;
                        }
                        // Any other final byte (`CSI > 4 ; 2 m`, `CSI > c`, …).
                        0x40..=0x7e => self.scan = Scan::Ground,
                        // ESC restarts, CAN/SUB abort (vte's "anywhere" rules).
                        0x1b => self.scan = Scan::Esc,
                        0x18 | 0x1a => self.scan = Scan::Ground,
                        _ if csi_transparent(b) => {}
                        // An intermediate or a second private marker: vte does not
                        // dispatch it as a push/pop/set (read on to the final byte).
                        _ => self.scan = Scan::KbdCsi { marker, n, seps, odd: true },
                    }
                    i += 1;
                }
                Scan::Decset { cur, hit } => {
                    match b {
                        b'0'..=b'9' => {
                            let cur = cur.saturating_mul(10).saturating_add(u16::from(b - b'0'));
                            self.scan = Scan::Decset { cur, hit };
                        }
                        b';' => self.scan = Scan::Decset { cur: 0, hit: hit | decset_bit(cur) | DECSET_MULTI },
                        b'h' | b'l' | b'n' => {
                            start = self.private_mode_csi(bytes, start, i, b, hit | decset_bit(cur), cur);
                            self.scan = Scan::Ground;
                        }
                        // ESC restarts, CAN/SUB abort (vte's "anywhere" rules).
                        0x1b => self.scan = Scan::Esc,
                        0x18 | 0x1a => self.scan = Scan::Ground,
                        // vte executes C0 controls and ignores DEL / high bytes
                        // without leaving the CSI.
                        0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff => {}
                        // Any other final byte, an intermediate, a sub-parameter
                        // colon or a second private marker: not a DECSET/DECRST.
                        _ => self.scan = Scan::Ground,
                    }
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
                                    kv: KV_MISMATCH,
                                    no_redraw: false,
                                }
                            } else {
                                Scan::Prefix { n: n2 }
                            };
                        }
                        // `9` opens the OSC 9;4 progress candidate.
                        b'9' if n == 0 => self.scan = Scan::Prefix94 { n: 1 },
                        // Some other OSC (title/hyperlink/color): skip to its end.
                        _ => self.scan = Scan::Skip,
                    }
                    if !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b) {
                        self.osc_len += 1; // OSC payload as vte buffers it
                    }
                    i += 1;
                }
                Scan::Prefix94 { n } => {
                    match b {
                        // Same framing as `Prefix`: ESC restarts, BEL/CAN/SUB end
                        // an OSC that never became `9;4;` (`ESC]9;4 BEL`).
                        0x1b => self.scan = Scan::Esc,
                        0x07 | 0x18 | 0x1a => self.scan = Scan::Ground,
                        _ if b == OSC94_PREFIX[n as usize] => {
                            let n2 = n + 1;
                            self.scan = if n2 as usize == OSC94_PREFIX.len() {
                                Scan::Progress { st: None, pr: None, field: 0, digits: 0 }
                            } else {
                                Scan::Prefix94 { n: n2 }
                            };
                        }
                        // `OSC 9 ; text` (a notification) or any other OSC 9.
                        _ => self.scan = Scan::Skip,
                    }
                    if !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b) {
                        self.osc_len += 1;
                    }
                    i += 1;
                }
                // A progress payload overrunning the OSC cap (a C0 flood between
                // the fields, or endless extra parameters): end it, no update.
                Scan::Progress { .. }
                    if self.osc_len >= self.osc_cap && !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b) =>
                {
                    start = self.abort_osc(bytes, start, i);
                }
                Scan::Progress { st, pr, field, digits } => {
                    match b {
                        // BEL / CAN / SUB / ESC(=ST) end the OSC (vte parity, as
                        // for OSC 133). Nothing here reads the grid, so the bytes
                        // need no sub-slice of their own.
                        0x07 | 0x18 | 0x1a | 0x1b => {
                            self.apply_progress(st, pr);
                            self.scan = if b == 0x1b { Scan::Esc } else { Scan::Ground };
                        }
                        b'0'..=b'9' if field < 2 => {
                            if digits >= PROGRESS_MAX_DIGITS {
                                self.scan = Scan::Skip; // malformed: no update
                            } else {
                                let d = u16::from(b - b'0');
                                self.scan = if field == 0 {
                                    let v = u16::from(st.unwrap_or(0)) * 10 + d;
                                    Scan::Progress { st: Some(v.min(255) as u8), pr, field, digits: digits + 1 }
                                } else {
                                    Scan::Progress { st, pr: Some(pr.unwrap_or(0) * 10 + d), field, digits: digits + 1 }
                                };
                            }
                        }
                        b';' => {
                            self.scan = Scan::Progress { st, pr, field: (field + 1).min(2), digits: 0 };
                        }
                        // vte ignores C0 controls inside an OSC (never buffered).
                        0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                        // Extra parameters past `pr` are ignored, not an error.
                        _ if field >= 2 => {}
                        // A non-digit in `st` / `pr`: malformed, skip the rest.
                        _ => self.scan = Scan::Skip,
                    }
                    if !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b) {
                        self.osc_len += 1;
                    }
                    i += 1;
                }
                // An OSC 133 payload overrunning the OSC cap: end it for vte and
                // discard the rest (never binds a mark).
                Scan::Payload { .. }
                    if self.osc_len >= self.osc_cap && !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b) =>
                {
                    start = self.abort_osc(bytes, start, i);
                }
                Scan::Payload { letter, code, in_code, code_done, kv, no_redraw } => match b {
                    // BEL / CAN / SUB / ESC(=ST) all end the OSC (vte parity).
                    0x07 | 0x18 | 0x1a | 0x1b => {
                        let is_esc = b == 0x1b;
                        let k = i + 1;
                        // Catch alacritty up to & including the terminator, then
                        // read the cursor NOW (OSC 133 never moves it, so the line
                        // is identical whether read in this feed or a later split).
                        self.advance_slice(&bytes[start..k]);
                        let redraws = !(no_redraw || usize::from(kv) == REDRAW_OFF.len());
                        self.bind_mark(letter, code.map(|c| c as i32), redraws);
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
                            kv: 0,
                            no_redraw: no_redraw || usize::from(kv) == REDRAW_OFF.len(),
                        };
                        self.osc_len += 1;
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
                        let kv = redraw_step(kv, b);
                        self.scan = Scan::Payload { letter, code: Some(next), in_code, code_done, kv, no_redraw };
                        self.osc_len += 1;
                        i += 1;
                    }
                    _ => {
                        // First byte after `133;` is the A/B/C/D letter. A
                        // non-digit inside the code field (e.g. `k=v`) makes the
                        // exit code unknown (None), closed so trailing digits do
                        // not resurrect it.
                        let kv = if in_code { redraw_step(kv, b) } else { kv };
                        self.scan = if letter == 0 && !in_code {
                            Scan::Payload { letter: b, code, in_code, code_done, kv, no_redraw }
                        } else if in_code && !code_done {
                            Scan::Payload { letter, code: None, in_code, code_done: true, kv, no_redraw }
                        } else {
                            Scan::Payload { letter, code, in_code, code_done, kv, no_redraw }
                        };
                        self.osc_len += 1;
                        i += 1;
                    }
                },
                // Some other OSC: jump straight to its terminator (one SIMD scan,
                // not a step per byte), counting the payload toward the cap.
                Scan::Skip => {
                    let term = osc_terminator(&bytes[i..]);
                    let run = term.unwrap_or(bytes.len() - i);
                    let room = self.osc_cap.saturating_sub(self.osc_len) as usize;
                    if run > room {
                        // Hand vte exactly up to the cap, end the OSC, drop the rest.
                        start = self.abort_osc(bytes, start, i + room);
                        i += room;
                    } else {
                        self.osc_len += run as u32;
                        i += run;
                        if term.is_some() {
                            // ESC (= ST) also begins a new escape; BEL/CAN/SUB end it.
                            self.scan = if bytes[i] == 0x1b { Scan::Esc } else { Scan::Ground };
                            i += 1;
                        }
                    }
                }
                // Past an OSC overrun: drop everything up to the OSC's terminator.
                Scan::OscDiscard => match osc_terminator(&bytes[i..]) {
                    None => {
                        i = bytes.len();
                        start = i;
                    }
                    Some(off) => {
                        let j = i + off;
                        if bytes[j] == 0x1b {
                            // Forward the ESC: vte (back in Ground after our CAN)
                            // parses what follows — ST's `\`, or a new escape.
                            start = j;
                            self.scan = Scan::Esc;
                        } else {
                            // BEL/CAN/SUB: the OSC already ended for vte; drop it too.
                            start = j + 1;
                            self.scan = Scan::Ground;
                        }
                        i = j + 1;
                    }
                },
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
                        // 8-bit ST ends the DCS; CAN / SUB CANCEL it (vte aborts the
                        // DCS) — nothing may be drawn. Flush the DCS to alacritty
                        // (it ignores it), then decode + place, or drop.
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        if b != 0x9c {
                            self.sixel_overflow = true;
                        }
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
                    // Data, up to the terminator, in one run.
                    _ => (start, i) = self.sixel_run(bytes, start, i),
                },
                // A non-sixel DCS (DECRQSS/XTGETTCAP/…): skip to the terminator
                // (one SIMD scan), accumulate nothing, emit nothing. Same
                // terminators as `Sixel`.
                Scan::DcsOther => match b {
                    0x18 | 0x1a | 0x9c => {
                        self.scan = Scan::Ground;
                        i += 1;
                    }
                    0x1b => {
                        self.scan = Scan::Esc;
                        i += 1;
                    }
                    _ => i = dcs_end(bytes, i),
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
                // ST, BEL (kitty ends every APC there) and 8-bit ST finish;
                // CAN/SUB abort.
                Scan::Apc => match b {
                    0x07 | 0x9c => {
                        // BEL / 8-bit ST: flush the APC to vte and end it there
                        // with an ST of its own — vte's APC string ends only at
                        // ESC / CAN / SUB, so it would swallow the output after
                        // the image up to the next ESC. Then decode/place.
                        let k = i + 1;
                        self.advance_slice(&bytes[start..k]);
                        self.advance_slice(b"\x1b\\");
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
                    // Control and payload, up to the terminator, in one run.
                    _ => (start, i) = self.apc_run(bytes, start, i),
                },
                // A non-`_G` APC: skip to the terminator (one SIMD scan), touch
                // nothing.
                Scan::ApcOther => match b {
                    0x18 | 0x1a | 0x9c => {
                        self.scan = Scan::Ground;
                        i += 1;
                    }
                    0x1b => {
                        self.scan = Scan::Esc;
                        i += 1;
                    }
                    _ => i = dcs_end(bytes, i),
                },
            }
        }
        if start < bytes.len() {
            self.advance_slice(&bytes[start..]);
        }
    }

    /// The ONLY entry for bytes into alacritty. Each sub-slice goes through
    /// [`Terminal::advance_piece`] together with an upper bound on the lines it
    /// can scroll — one per line-feed byte (LF/VT/FF) plus the autowraps its
    /// bytes can cause — so room for exactly that much is made below the cap
    /// first. Normal output (an 8 KiB read ≈ 100 lines) is one sub-slice costing
    /// one vectorized count; only dense floods (`yes`) split, so that no sub-slice
    /// exceeds [`Terminal::piece_budget`] lines.
    fn advance_slice(&mut self, s: &[u8]) {
        // Exact scroll accounting only matters while something is anchored to a
        // row, or while the view is scrolled back (its offset is re-derived from
        // the lines that really entered history, `keep_view_on_content`).
        // Otherwise the slice goes through whole, exactly as before (history may
        // pin at the cap; `abs_top` then under-counts, which nothing reads — a
        // mark bound later is relative to whatever `abs_top` is then). Keeping
        // history below the cap costs alacritty ~1 ns per scrolled line, so
        // floods past the last anchor run at full speed.
        if !self.has_anchors() && self.term.grid().display_offset() == 0 {
            self.advance_piece(s, None);
            return;
        }
        let cols = self.cols.max(1);
        let budget = self.piece_budget();
        // Tiny slices (between escapes) are bounded by their length alone.
        if s.len() + s.len() / cols < budget {
            self.advance_piece(s, Some(s.len() + s.len() / cols + 1));
            return;
        }
        let lines = count_line_feeds(s) + s.len() / cols + 1;
        if lines <= budget {
            self.advance_piece(s, Some(lines));
            return;
        }
        // A dense flood: cut at block granularity so each piece fits the budget.
        let wraps_per_block = SLICE_BLOCK / cols + 1;
        let mut piece_start = 0;
        let mut piece_lines = 0;
        for (n, block) in s.chunks(SLICE_BLOCK).enumerate() {
            let block_lines = count_line_feeds(block) + wraps_per_block;
            let at = n * SLICE_BLOCK;
            if piece_lines + block_lines > budget && at > piece_start {
                self.advance_piece(&s[piece_start..at], Some(piece_lines));
                piece_start = at;
                piece_lines = 0;
            }
            piece_lines += block_lines;
        }
        self.advance_piece(&s[piece_start..], Some(piece_lines));
    }

    /// Most lines one sub-slice may scroll: below alacritty's 1000-row row
    /// cache (so a trim never frees rows the next scroll would reallocate) and
    /// below the user cap (so room for it can always be made under the cap).
    fn piece_budget(&self) -> usize {
        SLICE_MAX_LINES.min(self.scrollback_limit / 2).max(1)
    }

    /// An OSC overran [`OSC_MAX_BYTES`] at `bytes[cut]`: hand alacritty the bytes
    /// up to the cap, end the OSC with CAN (vte dispatches the truncated head —
    /// every handler caps or rejects it — and CAN itself is a no-op), and switch
    /// to discarding up to the OSC's terminator. Returns the new `start`.
    #[cold]
    #[inline(never)]
    fn abort_osc(&mut self, bytes: &[u8], start: usize, cut: usize) -> usize {
        self.advance_slice(&bytes[start..cut]);
        self.advance_slice(b"\x18");
        self.scan = Scan::OscDiscard;
        cut
    }

    /// Copy the sixel data from `bytes[i]` up to its terminator (or the end of
    /// `bytes`) in one run — never past [`SIXEL_MAX_BYTES`]: past it, latch the
    /// overflow and keep scanning to resync at the terminator. vte steps over
    /// it ([`Terminal::skip_payload`]). Returns the new `start` and `i`.
    #[cold]
    #[inline(never)]
    fn sixel_run(&mut self, bytes: &[u8], start: usize, i: usize) -> (usize, usize) {
        let end = dcs_end(bytes, i);
        let room = SIXEL_MAX_BYTES.saturating_sub(self.sixel_buf.len());
        let run = &bytes[i..end];
        self.sixel_buf.extend_from_slice(&run[..run.len().min(room)]);
        self.sixel_overflow |= run.len() > room;
        (self.skip_payload(bytes, start, i, end), end)
    }

    /// [`Terminal::sixel_run`] for a Kitty APC's control and payload, up to
    /// [`APC_MAX_BYTES`].
    #[cold]
    #[inline(never)]
    fn apc_run(&mut self, bytes: &[u8], start: usize, i: usize) -> (usize, usize) {
        let end = apc_terminator(&bytes[i..]).map_or(bytes.len(), |n| i + n);
        let room = APC_MAX_BYTES.saturating_sub(self.apc_buf.len());
        let run = &bytes[i..end];
        self.apc_buf.extend_from_slice(&run[..run.len().min(room)]);
        self.apc_overflow |= run.len() > room;
        (self.skip_payload(bytes, start, i, end), end)
    }

    /// `bytes[i..end]` is a sixel's data or a Kitty APC's payload, which vte
    /// would only ignore byte by byte (alacritty's DCS `put` is a no-op; an APC
    /// string is discarded) — and which can run to megabytes. Hand vte what
    /// precedes it and step over it: vte stays in its DCS / APC state and is
    /// handed the terminator. Returns the new `start`.
    fn skip_payload(&mut self, bytes: &[u8], start: usize, i: usize, end: usize) -> usize {
        if start < i {
            self.advance_slice(&bytes[start..i]);
        }
        end
    }

    /// Advance `bytes[start..k]` with the isolated sequence `bytes[seq_start..k]`
    /// in a sub-slice of its own, then apply its anchor effects. Returns the new
    /// `start` (`k`). `seq_start` is clamped to `start` (its ESC may already have
    /// been handed to alacritty, e.g. as an OSC's ST).
    #[cold]
    #[inline(never)]
    fn isolate(&mut self, bytes: &[u8], start: usize, seq_start: usize, k: usize, kind: IsolatedSeq) -> usize {
        let s0 = seq_start.max(start);
        self.advance_slice(&bytes[start..s0]);
        // Inside a DEC 2026 synchronized update vte only BUFFERS, then replays
        // the whole update in one piece, where these would not run alone: a
        // toggle would freeze `abs_top` over the primary lines after it, ED 3's
        // shrink would hide the scroll before it, ED 2's prune would run before
        // the erase, RIS would drop the anchors (bump the epoch) before the
        // reset it stands for — a search collected in between would trust
        // `abs_top` across it. Apply the update first (the frame tears at most
        // once, as for a mark or image inside a sync). SU / DL only scroll,
        // which the replay counts like any line feed.
        if !matches!(kind, IsolatedSeq::ScrollUp { .. }) {
            self.apply_pending_sync();
        }
        let seq = &bytes[s0..k];
        // A handful of bytes that may push a whole screen into scrollback: make
        // room for the lines it really pushes (+1 per byte), or a nearly full
        // scrollback pins at the cap and every anchor is lost (Ctrl+L in a
        // long-lived tab). Room beyond the piece budget is not made: that would
        // trim — or wipe — history for a push that overflows the cap anyway, so
        // such a piece goes through as before (anchors dropped once). Inside a
        // synchronized update (SU / DL) the bytes are only buffered: nothing to
        // bound.
        let room = (kind.scrolls_a_screen() && self.has_anchors() && self.sync_deadline().is_none())
            .then(|| self.lines_pushed_by(kind) + seq.len() + 1)
            .filter(|&lines| lines <= self.piece_budget());
        match room {
            Some(lines) => self.advance_piece(seq, Some(lines)),
            None => self.advance_slice(seq),
        }
        self.after_isolated(kind);
        k
    }

    /// Whether the scanner splits `kind` out into a sub-slice of its own: only
    /// while anchors exist, and — for ED 2 / SU / DL, which can only move the
    /// PRIMARY scrollback — not on the alt screen, where TUIs scroll constantly
    /// (`abs_top` is frozen there; splitting would only cost a piece each).
    fn isolates(&self, kind: IsolatedSeq) -> bool {
        self.has_anchors() && !(kind.scrolls_a_screen() && self.term.mode().contains(TermMode::ALT_SCREEN))
    }

    /// At most how many lines `kind` (ED 2 / SU / DL, about to run on the primary
    /// screen with alacritty caught up) pushes into scrollback. ED 2 pushes the
    /// screen down to its last non-empty row (alacritty's `clear_viewport`); SU
    /// its count, clamped to the screen (exact for a region at the top, an upper
    /// bound otherwise — the region is not readable); DL pushes only from the
    /// top row.
    fn lines_pushed_by(&self, kind: IsolatedSeq) -> usize {
        use alacritty_terminal::grid::GridCell;
        let grid = self.term.grid();
        match kind {
            IsolatedSeq::EraseScreen => (0..self.rows)
                .rev()
                .find(|&r| {
                    let row = &grid[Line(r as i32)];
                    (0..self.cols).any(|c| !row[Column(c)].is_empty())
                })
                .map_or(0, |r| r + 1),
            IsolatedSeq::ScrollUp { delete: true, .. } if grid.cursor.point.line.0 != 0 => 0,
            IsolatedSeq::ScrollUp { count, .. } => usize::from(count.max(1)).min(self.rows),
            _ => 0,
        }
    }

    /// A complete kitty keyboard CSI whose final `u` is `bytes[i]` (`marker` `>`
    /// push, `<` pop, `=` set; `n` its first parameter; `odd` = vte would not
    /// dispatch it as one). Catches alacritty up to the `u` — so the screen it
    /// acts on is known, and a synchronized update applied first, as for a mark
    /// — then mirrors it in `kbd_depth` and returns the new `start`. A push that
    /// would exceed [`KBD_STACK_MAX`] is DROPPED: vte gets CAN instead of the `u`.
    /// Errs safe where it must guess: an odd push still counts, an odd pop does
    /// not, so the mirror never under-counts what alacritty holds.
    #[cold]
    #[inline(never)]
    fn kitty_kbd_csi(&mut self, bytes: &[u8], start: usize, i: usize, marker: u8, n: u16, odd: bool) -> usize {
        self.advance_slice(&bytes[start..i]);
        if !self.kitty_keyboard {
            return i; // alacritty ignores the protocol: nothing to mirror
        }
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
        let screen = usize::from(self.term.mode().contains(TermMode::ALT_SCREEN));
        let primary = screen == 0;
        let depth = self.kbd_depth[screen];
        match marker {
            b'>' if depth >= KBD_STACK_MAX => {
                self.advance_slice(b"\x18");
                return i + 1;
            }
            b'>' => self.kbd_depth[screen] = depth + 1,
            b'<' if !odd => {
                // vte: a missing or zero count pops one.
                let depth = depth.saturating_sub(n.max(1));
                self.kbd_depth[screen] = depth;
                if let Some(w) = self.kbd_window.as_mut().filter(|_| primary) {
                    w.floor = w.floor.min(depth);
                }
            }
            b'=' => {
                if let Some(w) = self.kbd_window.as_mut().filter(|_| primary) {
                    w.set = true;
                }
            }
            _ => {}
        }
        i
    }

    /// Answer XTVERSION (`CSI > q`, final byte at `bytes[i]`) with the
    /// terminal's name and version — `DCS > | JeTTY(0.27.0) ST` — so programs
    /// (tmux, notcurses, yazi…) can tell which terminal they run in; vte drops
    /// the query. Caught up first (a synchronized update applied) so the reply
    /// keeps its place among alacritty's own. Returns the new `start`.
    #[cold]
    #[inline(never)]
    fn xtversion(&mut self, bytes: &[u8], start: usize, i: usize) -> usize {
        self.advance_slice(&bytes[start..=i]);
        self.apply_pending_sync();
        let reply = format!("\x1bP>|JeTTY({})\x1b\\", crate::pty::advertised_version());
        let _ = self.reply_tx.send(reply.into_bytes());
        i + 1
    }

    /// alacritty just cleared BOTH kitty keyboard stacks (RIS, a protocol toggle).
    fn kbd_cleared(&mut self) {
        self.kbd_depth = [0, 0];
        if let Some(w) = self.kbd_window.as_mut() {
            w.floor = 0;
        }
    }

    /// A prompt arrived: whatever ran since the save point is over. Undo what it
    /// left on the PRIMARY kitty keyboard stack — entries it pushed and never
    /// popped (a SIGKILLed or crashed program, a dropped ssh) and flags it set in
    /// place — so the shell gets legacy keys again (Ctrl+C as `^C`, not
    /// `\e[99;5u`; with "report all keys" even `reset` could not be typed). What
    /// the shell had on the stack before the command stays. A no-op while
    /// nothing changed — and for a `provisional` prompt-to-prompt window closed by
    /// a bare `A` (Ctrl+C or an empty line at the prompt): that window spans only
    /// the line editor reading input, so what is on the stack then is the
    /// editor's own (reedline pushes while it reads), not a dead program's; a
    /// command run without a `C` mark is cleaned up at its `D`.
    fn restore_kbd(&mut self, end_of_command: bool) {
        let Some(w) = self.kbd_window.take() else { return };
        if w.provisional && !end_of_command {
            return;
        }
        let depth = self.kbd_depth[0];
        let extra = depth.saturating_sub(w.floor);
        if self.kitty_keyboard && (extra > 0 || w.set) {
            use alacritty_terminal::vte::ansi::Handler;
            // Pops `extra` entries and reloads the active flags from the new
            // stack top — with 0 it only drops flags a `CSI = u` set in place.
            self.term.pop_keyboard_modes(extra);
            self.kbd_depth[0] = depth - extra;
        }
    }

    /// Start a keyboard save point at the current primary stack depth.
    fn open_kbd_window(&mut self, provisional: bool) {
        self.kbd_window = Some(KbdWindow { floor: self.kbd_depth[0], set: false, provisional });
    }

    /// Whether any row-anchored state exists — the only time the scanner pays to
    /// isolate history-rewriting sequences (see [`IsolatedSeq`]). An active
    /// search and a pinned view count: they sit on the absolute line scale.
    #[inline(always)]
    fn has_anchors(&self) -> bool {
        !self.marks.is_empty() || !self.placements.is_empty() || self.search_regex.is_some() || self.view_pinned
    }

    /// Apply what an isolated sequence (just advanced in its own sub-slice) means
    /// for the anchors beyond the history delta `track_abs_top` already folded in.
    #[cold]
    #[inline(never)]
    fn after_isolated(&mut self, kind: IsolatedSeq) {
        match kind {
            // RIS wiped the screen AND scrollback (and left the alt screen): no
            // anchor can still point at its content.
            IsolatedSeq::Reset => self.drop_anchors(),
            // `\e[2J` on the primary pushed every line up to the last non-empty
            // one into scrollback (`abs_top` followed); anything still anchored
            // ON the screen was erased with it.
            IsolatedSeq::EraseScreen if !self.term.mode().contains(TermMode::ALT_SCREEN) => {
                let top = self.abs_top;
                self.marks.retain(|m| m.prompt < top);
                let mut freed = 0u64;
                self.placements.retain(|p| {
                    let keep = p.abs_line + p.rows as i64 <= top;
                    if !keep {
                        freed += p.image.rgba.len() as u64;
                    }
                    keep
                });
                self.placement_bytes = self.placement_bytes.saturating_sub(freed);
            }
            // ED 3's shrink, SU/DL's scroll and a toggle's freeze are fully
            // handled by the isolated `track_abs_top` call itself.
            _ => {}
        }
    }

    /// Advance alacritty by ONE sub-slice and fold its history change into
    /// `abs_top`. `Some(max_lines)` (anchors exist) first makes room below the cap
    /// for at most that many scrolled lines, so the change is exact.
    fn advance_piece(&mut self, s: &[u8], max_lines: Option<usize>) {
        let alt_before = self.term.mode().contains(TermMode::ALT_SCREEN);
        if let (Some(lines), false) = (max_lines, alt_before) {
            self.make_room(lines);
        }
        let h0 = self.term.grid().history_size();
        let d0 = self.term.grid().display_offset();
        #[cfg(test)]
        if let Some(log) = self.vte_log.as_mut() {
            log.extend_from_slice(s);
        }
        let parser = &mut self.parser;
        crate::handler::parse(&mut self.term, &mut self.vt, &self.reply_tx, |vt| parser.advance(vt, s));
        let alt_after = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h1 = self.term.grid().history_size();
        self.after_vte(alt_before, alt_after, h0, h1, d0);
    }

    /// Fold a `history_size` delta into `abs_top`, honoring the alt screen.
    /// Entering/leaving the alt screen (vim/less/htop) changes `history_size`
    /// WITHOUT scrolling, and while on the alt screen its history churn is not
    /// the primary scrollback — so `abs_top` (and every mark) is FROZEN across an
    /// alt-screen toggle and for its whole duration, resuming cleanly on return.
    fn track_abs_top(&mut self, alt_before: bool, alt_after: bool, h0: usize, h1: usize) {
        if alt_before || alt_after {
            return;
        }
        if h1 >= h0 {
            self.abs_top += (h1 - h0) as i64;
            // `make_room` keeps history strictly below the cap for any sub-slice
            // within its bound, so reaching the cap means this one scrolled more
            // than counted (a sync-update replay, a long `CSI Ps b` repeat, a
            // screen taller than the scrollback) and `history_size()` pinned:
            // the count is lost.
            // Drop every anchor once (correct-or-absent); the next sub-slice is
            // exact again.
            if h1 >= self.scrollback_limit && (h1 > h0 || self.scrollback_limit == 0) {
                self.drop_anchors();
            }
        } else {
            self.on_history_shrunk(h1);
        }
    }

    /// Keep a scrolled-back view on the lines it showed. alacritty 0.26 bumps the
    /// display offset on EVERY scroll while the view is scrolled back — also when
    /// a region below a fixed top row scrolls (DECSTBM top margin > 1) or lines
    /// below the top row are deleted, which push nothing into history: the view
    /// drifted up a line per scroll, and once past the top of the history the
    /// next snapshot indexed outside the grid (a panic). Re-derive the offset
    /// from the lines that really entered history since `before` = (offset,
    /// history size) — exact, as sub-slices are bounded while the view is
    /// scrolled back. Where that count is lost (`None`: the primary screen came
    /// back from the alt screen mid-slice; a history pinned at the cap by an
    /// unbounded sync replay) the offset is only kept inside the history.
    #[cold]
    #[inline(never)]
    fn keep_view_on_content(&mut self, before: Option<(usize, usize)>, h1: usize) {
        let d1 = self.term.grid().display_offset();
        // Back at the bottom — put there by ED 3 or RIS: nothing to keep. A
        // pinned view that WAS at the bottom is kept on its lines too.
        if d1 == 0 && !(self.view_pinned && before.is_some_and(|(d0, _)| d0 == 0)) {
            return;
        }
        let want = match before {
            Some((d0, h0)) if h1 < self.scrollback_limit => d0 + h1.saturating_sub(h0),
            _ => d1,
        }
        .min(h1);
        if want != d1 {
            self.term.scroll_display(Scroll::Delta(want as i32 - d1 as i32));
        }
    }

    /// Make room for a sub-slice that may scroll up to `lines` lines: trim the
    /// OLDEST history so `history + lines < scrollback_limit` (the grid's max),
    /// so `history_size()` cannot pin and its growth is an exact scroll count.
    /// The trim is O(1) in alacritty (`Storage::shrink_lines`); the trimmed rows
    /// stay in its row cache for the next scrolls. `Line(0)` does not move, so
    /// `abs_top` is unchanged; anchors that fell off the top are dropped. Only
    /// called on the primary screen (`grid_mut` would be the alt grid).
    #[inline(never)]
    fn make_room(&mut self, lines: usize) {
        let limit = self.scrollback_limit;
        let target = limit.saturating_sub(lines + 1);
        let history = self.term.grid().history_size();
        if history <= target {
            return;
        }
        let grid = self.term.grid_mut();
        grid.update_history(target);
        grid.update_history(limit);
        // Anchors that fell off the top. Marks/placements are appended in
        // (nearly) ascending order, so popping the front is O(dropped); any
        // out-of-order straggler maps outside the viewport and goes at the next
        // full prune.
        let min_abs = self.abs_top - target as i64;
        while self.marks.front().is_some_and(|m| m.prompt < min_abs) {
            self.marks.pop_front();
        }
        while let Some(p) = self.placements.front() {
            if p.abs_line + p.rows as i64 > min_abs {
                break;
            }
            self.placement_bytes = self.placement_bytes.saturating_sub(p.image.rgba.len() as u64);
            self.placements.pop_front();
        }
    }

    /// Drop every row-anchored state (marks + primary image placements) after an
    /// event that makes their absolute lines meaningless, and bump
    /// `anchor_epoch` so an in-flight placement notices. Run & Notify state
    /// (`cur_cmd`) is NOT anchored and survives.
    #[cold]
    #[inline(never)]
    fn drop_anchors(&mut self) {
        self.marks.clear();
        self.marks_sorted = true;
        self.clear_placements();
        self.anchor_epoch = self.anchor_epoch.wrapping_add(1);
    }

    /// Handle a non-scroll history shrink on the PRIMARY screen (a destructive
    /// reset `RIS`/`\ec`, or a scrollback clear `\e[3J`): `Line(0)` does not move,
    /// so `abs_top` stays monotonic; drop marks whose line no longer exists. The
    /// next prompt re-marks.
    #[cold]
    #[inline(never)]
    fn on_history_shrunk(&mut self, history_size: usize) {
        self.prune_marks(history_size);
        self.prune_placements(history_size);
    }

    /// Append a prompt mark, keeping `marks_sorted` honest.
    fn push_mark(&mut self, block: CmdBlock) {
        if self.marks.back().is_some_and(|b| block.prompt < b.prompt) {
            self.marks_sorted = false;
        }
        self.marks.push_back(block);
    }

    /// Drop marks outside the live window `[abs_top - history_size, abs_top + rows)`
    /// and cap the total (defensive). Called on every A-bind and on a shrink.
    /// O(pruned) while `marks_sorted`: the out-of-window marks are then a prefix
    /// (scrolled off) and a suffix (below the screen). Only after an out-of-order
    /// bind does it rescan, and re-derives the order flag from the survivors.
    #[cold]
    #[inline(never)]
    fn prune_marks(&mut self, history_size: usize) {
        let min_abs = self.abs_top - history_size as i64;
        let max_abs = self.abs_top + self.rows as i64;
        if self.marks_sorted {
            while self.marks.front().is_some_and(|m| m.prompt < min_abs) {
                self.marks.pop_front();
            }
            while self.marks.back().is_some_and(|m| m.prompt >= max_abs) {
                self.marks.pop_back();
            }
        } else {
            self.marks.retain(|m| m.prompt >= min_abs && m.prompt < max_abs);
            self.marks_sorted = marks_ascending(&self.marks);
        }
        while self.marks.len() > MAX_MARKS {
            self.marks.pop_front();
        }
    }

    /// Empty the PRIMARY placement list and reset the live-bytes counter (part
    /// of [`Terminal::drop_anchors`]: correct-or-absent).
    fn clear_placements(&mut self) {
        self.placements.clear();
        self.placement_bytes = 0;
    }

    /// Empty the ALT-screen placement list.
    fn clear_alt_placements(&mut self) {
        self.alt_placements.clear();
        self.alt_placement_bytes = 0;
    }

    /// Drop the Kitty images stored on the ALT screen: they go with what it
    /// showed, as kitty clears that screen's images on entering it.
    fn forget_alt_images(&mut self) {
        let mut freed = 0;
        self.kitty_images.retain(|e| {
            freed += if e.alt { e.image.rgba.len() as u64 } else { 0 };
            !e.alt
        });
        self.kitty_stored_bytes = self.kitty_stored_bytes.saturating_sub(freed);
    }

    /// Bookkeeping after vte consumed bytes (a sub-slice or a sync flush); `h0` /
    /// `d0` are the history size and display offset before it.
    fn after_vte(&mut self, alt_before: bool, alt_after: bool, h0: usize, h1: usize, d0: usize) {
        if alt_before != alt_after {
            // A full-screen TUI took over (or left) mid-transfer: a partial Kitty
            // chunk accumulation lost its context (M5), and alt-screen images
            // belong to the screen that just went away.
            self.reset_kitty_chunks();
            self.clear_alt_placements();
            self.forget_alt_images();
            self.screen_switches = self.screen_switches.wrapping_add(1);
        }
        // Back on (or still on) the primary screen with its view scrolled back
        // — or pinned.
        if !alt_after && (d0 != 0 || alt_before || self.view_pinned) {
            self.keep_view_on_content((!alt_before).then_some((d0, h0)), h1);
        }
        self.track_abs_top(alt_before, alt_after, h0, h1);
        if alt_after && !self.alt_placements.is_empty() {
            self.check_alt_placements();
        }
        cap_cursor_zerowidth(&mut self.term);
    }

    /// Re-validate ALT-screen images after vte moved on. FULL damage (alacritty
    /// marks it on any scroll, any ED — even one that misses the image — RIS,
    /// insert mode, palette change) may mean the grid under them moved: drop
    /// them all (correct-or-absent; TUIs re-send previews they still want). A
    /// sixel additionally goes once any covered cell was written (its pixels ARE
    /// those cells). Damage is consumed only here and reset at an alt placement
    /// (its baseline) — JeTTY renders from snapshots, not alacritty's damage.
    #[cold]
    #[inline(never)]
    fn check_alt_placements(&mut self) {
        use alacritty_terminal::term::TermDamage;
        let full = matches!(self.term.damage(), TermDamage::Full);
        self.term.reset_damage();
        if full {
            self.clear_alt_placements();
            return;
        }
        let (rows, cols) = (self.rows, self.cols);
        let grid = self.term.grid();
        let mut freed = 0u64;
        self.alt_placements.retain(|p| {
            let keep = !p.marks_cells
                || alt_cells(p, rows, cols).all(|(l, c)| grid[l][c].flags.contains(SIXEL_CELL));
            if !keep {
                freed += p.image.rgba.len() as u64;
            }
            keep
        });
        self.alt_placement_bytes = self.alt_placement_bytes.saturating_sub(freed);
    }

    /// Anchor `p` at the cursor on the PRIMARY screen. With `reserve`, the cursor
    /// then moves past it, scrolling as needed: below a sixel, under its left
    /// edge (xterm); onto a Kitty image's last row, right of it (kitty — a wrap
    /// pending at the edge). Through alacritty's `Handler`, never by injecting
    /// bytes into a parser that may sit mid-sequence (e.g. in the Escape state
    /// an ESC-ended DCS or APC leaves it in). The lines it feeds are paid for
    /// first (`Err(IMAGE_BUSY)`: nothing happens), and room for their scroll is
    /// made so it is counted exactly; an anchor reset on the way drops `p`.
    fn place_primary_at_cursor(&mut self, mut p: ImagePlacement, reserve: bool, sixel: bool) -> Result<(), &'static str> {
        use alacritty_terminal::vte::ansi::Handler;
        let lines = match (reserve, sixel) {
            (false, _) => 0,
            (true, true) => p.rows,
            (true, false) => p.rows.saturating_sub(1),
        };
        // Each line fed writes a row of cells, paid for from the image-work
        // bank like the image's pixels: a 24-byte `a=p` with `r=1024` feeds
        // 1,024 of them.
        let row = self.cols * std::mem::size_of::<alacritty_terminal::term::cell::Cell>();
        self.pay_image_work(u64::from(lines) * row as u64)?;
        self.make_room(p.rows as usize + 1);
        let cur = self.term.grid().cursor.point;
        p.abs_line = self.abs_top + cur.line.0 as i64;
        p.anchor_at(cur.column.0, self.cols);
        if reserve {
            let epoch = self.anchor_epoch;
            let h0 = self.term.grid().history_size();
            let d0 = self.term.grid().display_offset();
            for _ in 0..lines {
                self.term.linefeed();
            }
            let h1 = self.term.grid().history_size();
            if d0 != 0 || self.view_pinned {
                self.keep_view_on_content(Some((d0, h0)), h1);
            }
            self.track_abs_top(false, false, h0, h1);
            if self.anchor_epoch != epoch {
                return Ok(());
            }
            let col = usize::from(if sixel { p.col } else { p.col + p.cols });
            let cursor = &mut self.term.grid_mut().cursor;
            cursor.point.column = Column(col.min(self.cols - 1));
            cursor.input_needs_wrap = col >= self.cols;
        }
        insert_placement(&mut self.placements, &mut self.placement_bytes, p);
        let history = self.term.grid().history_size();
        self.prune_placements(history);
        Ok(())
    }

    /// Anchor `p` at the cursor on the ALT screen (a TUI preview: yazi, ranger,
    /// image.nvim). Never scrolls the TUI. A sixel moves the cursor to the line
    /// below it (clamped to the screen) and marks its cells so it vanishes once
    /// any is written; a Kitty image moves the cursor past its last column on its
    /// last row (a wrap pending at the edge) unless `C=1`. The cursor is set
    /// directly: `Handler::goto` would offset it by the scroll region (DECOM).
    fn place_alt_at_cursor(&mut self, mut p: ImagePlacement, sixel: bool, move_cursor: bool) {
        let (rows, cols) = (self.rows, self.cols);
        let cur = self.term.grid().cursor.point;
        p.abs_line = cur.line.0 as i64;
        p.anchor_at(cur.column.0, cols);
        let last_row = rows as i32 - 1;
        let grid = self.term.grid_mut();
        let target = if sixel {
            p.marks_cells = true;
            for (l, c) in alt_cells(&p, rows, cols) {
                grid[l][c].flags.insert(SIXEL_CELL);
            }
            Some(((cur.line.0 + p.rows as i32).min(last_row), cur.column.0))
        } else if move_cursor {
            Some(((cur.line.0 + p.rows as i32 - 1).min(last_row), (p.col + p.cols) as usize))
        } else {
            None
        };
        if let Some((line, col)) = target {
            let cursor = &mut grid.cursor;
            cursor.point = Point::new(Line(line), Column(col.min(cols.saturating_sub(1))));
            cursor.input_needs_wrap = col >= cols;
        }
        insert_placement(&mut self.alt_placements, &mut self.alt_placement_bytes, p);
        // Damage baseline for `check_alt_placements`: whatever happened before this
        // image was placed must not count against it.
        self.term.reset_damage();
    }

    /// Drop placements whose entire row SPAN lies outside the live window
    /// `[abs_top - history_size, abs_top + rows)`, then enforce the count / bytes
    /// caps (drop oldest). Unlike `prune_marks` (a single-line predicate) this is
    /// a SPAN intersection because an image occupies `rows` rows — an image is
    /// kept iff `abs_line + rows > min_abs && abs_line < max_abs`, the SAME test
    /// `visible_images` uses (kept consistent on purpose).
    #[cold]
    #[inline(never)]
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

    /// True when a resize may WIPE the grid + scrollback instead of reflowing
    /// them (the p10k/starship prompt-scatter fix in [`Terminal::resize`]: the
    /// shell repaints one clean prompt on SIGWINCH). Content safety needs ALL of:
    /// * no command has run in this tab yet (`saw_command_output`, latched by a
    ///   `C`, or by a `D` from integrations that never send `C`);
    /// * the newest prompt block is open and has not started a command;
    /// * the shell repaints that whole prompt after a resize (no `A;redraw=0`:
    ///   bash's readline repaints only the last line of a multi-line PS1, so the
    ///   wipe would erase the rest for good);
    /// * nothing but blank lines exists ABOVE that prompt's line — output printed
    ///   before the first prompt (login banner, motd, fastfetch) must survive;
    /// * the cursor is still on the prompt's INPUT line — its `B` mark when the
    ///   integration sends one, else the `A` line — or on rows soft-wrapped from
    ///   it (typed input longer than a row), within [`CLEAN_PROMPT_MAX_ROWS`].
    ///   Below a hard line break sits either a multi-line prompt drawn after an
    ///   `A` without a `B`, or the first command's output from a shell that sends
    ///   no `C`; the two look alike, and the output must survive;
    /// * the shell is not mid-write: no escape sequence cut by a read boundary,
    ///   no synchronized update still buffered in vte (its output isn't settled,
    ///   so a wipe now could erase or reorder part of it).
    ///
    /// False without shell integration (no marks) and on the alt screen. Only
    /// called on a real resize, so the bounded blank-line walk is off the hot path.
    fn at_clean_prompt(&self) -> bool {
        if self.saw_command_output
            || self.scan != Scan::Ground
            || self.sync_deadline().is_some()
            || self.term.mode().contains(TermMode::ALT_SCREEN)
        {
            return false;
        }
        let Some(m) = self.marks.back() else {
            return false;
        };
        if m.finished || m.output.is_some() || !m.redraws {
            return false;
        }
        let grid = self.term.grid();
        // Grid lines of the prompt mark and of where input starts (negative = in
        // scrollback).
        let prompt_line = m.prompt - self.abs_top;
        let input_line = m.input.map_or(prompt_line, |b| (b - self.abs_top).max(prompt_line));
        let cursor_line = grid.cursor.point.line.0 as i64;
        if cursor_line < input_line || cursor_line - input_line > CLEAN_PROMPT_MAX_ROWS {
            return false;
        }
        let last_col = Column(self.cols - 1);
        if (input_line..cursor_line).any(|l| !grid[Line(l as i32)][last_col].flags.contains(Flags::WRAPLINE)) {
            return false;
        }
        let top = grid.topmost_line().0 as i64;
        if prompt_line < top || prompt_line - top > CLEAN_PROMPT_MAX_ABOVE {
            return false;
        }
        (top..prompt_line).all(|l| {
            let row = &grid[Line(l as i32)];
            (0..self.cols).all(|c| matches!(row[Column(c)].c, ' ' | '\0'))
        })
    }

    /// Whether NOTHING was typed on block `m`'s command line before a `C` at
    /// absolute line `c_line`, column `c_col`: an Enter (or ^C) on an empty
    /// prompt. p10k and iTerm2-style integrations still report that as a
    /// command — `C;` + `D;<$?>`, with `$?` the PREVIOUS command's status.
    /// Only claimed when provable: `m` is the open block and has a `B`, the `C`
    /// sits on a LATER row (as after every real Enter) at most
    /// [`CLEAN_PROMPT_MAX_ROWS`] below it, the [`EMPTY_INPUT_CELLS`] cells from
    /// the `B` are blank — whatever lies further along the `B` row is a right
    /// prompt — and so is every cell on the rows after it up to the `C`.
    /// Anything else is a command, as before.
    fn command_line_blank(&self, m: &CmdBlock, c_line: i64, c_col: usize) -> bool {
        let Some(b_line) = m.input else { return false };
        if m.finished || m.output.is_some() || c_line <= b_line || c_line - b_line > CLEAN_PROMPT_MAX_ROWS {
            return false;
        }
        let grid = self.term.grid();
        let top = grid.topmost_line().0 as i64;
        (b_line..=c_line).all(|abs| {
            let l = abs - self.abs_top;
            if l < top || l >= self.rows as i64 {
                return false;
            }
            let row = &grid[Line(l as i32)];
            let (from, to) = if abs == b_line {
                let from = m.input_col.min(self.cols);
                (from, (from + EMPTY_INPUT_CELLS).min(self.cols))
            } else if abs == c_line {
                (0, c_col.min(self.cols))
            } else {
                (0, self.cols)
            };
            (from..to).all(|c| matches!(row[Column(c)].c, ' ' | '\0'))
        })
    }

    /// Record an OSC 133 mark for the given sub-command letter (and D's exit
    /// code). Reads the cursor's absolute line immediately after the terminator
    /// has been advanced. No-op on the alt screen (OSC 133 inside a TUI is
    /// meaningless). Coalesces a duplicate A on the same line so p10k + our own
    /// snippet both emitting A cannot create two blocks.
    fn bind_mark(&mut self, letter: u8, exit: Option<i32>, redraws: bool) {
        // Inside a DEC 2026 synchronized update vte is still BUFFERING the bytes
        // before this mark, so the cursor has not reached the mark's row yet —
        // nor has a buffered alt-screen exit run. Flush the update now (the
        // frame tears at most once) so the mark binds to its real row, on the
        // screen it really is on.
        self.apply_pending_sync();
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let abs = self.abs_top + self.term.grid().cursor.point.line.0 as i64;
        match letter {
            b'A' => {
                // Dedup an A re-sent for the newest prompt on its own line, no
                // command started in between: a double emission (p10k's own
                // integration + ours) or a redraw (the transient prompt). Keyed
                // on that prompt's MARK: once a resize dropped the marks, the
                // shell's repaint of the same line must mark it again.
                if self.marks.back().is_some_and(|m| m.prompt == abs && !m.finished && m.output.is_none()) {
                    return;
                }
                // Run-selection readiness signal: count each DISTINCT prompt
                // (after the dedup above, so a p10k double-emission counts
                // once). One u64 add on the already-off-hot-path OSC-133
                // scanner. Never per byte.
                self.prompts_seen += 1;
                // A new prompt closes any previous still-open block (a command
                // that never emitted D, e.g. ^C at the prompt) as unknown, and
                // abandons its Run & Notify state (no completion for it).
                if let Some(last) = self.marks.back_mut() {
                    last.finished = true;
                }
                self.cur_cmd = Some(OpenCmd { started_at: None });
                // Back at a prompt: whatever reported progress has ended (a
                // program killed mid-build never sends its `9;4;0`).
                self.set_progress(None);
                self.cmd_marks_dirty = true;
                // Keyboard: what ran since the last save point is over; save
                // again here in case this shell never sends a `C`.
                self.restore_kbd(false);
                self.open_kbd_window(true);
                self.push_mark(CmdBlock {
                    prompt: abs,
                    input: None,
                    input_col: 0,
                    output: None,
                    exit: None,
                    finished: false,
                    redraws,
                });
                let history = self.term.grid().history_size();
                self.prune_marks(history);
            }
            b'B' => {
                let col = self.term.grid().cursor.point.column.0;
                if let Some(last) = self.marks.back_mut() {
                    // The leftmost B on a row wins: an integration that also ends
                    // a RIGHT prompt with B (p10k under Warp) must not move the
                    // command line's start past the typed command.
                    if last.input != Some(abs) || col < last.input_col {
                        last.input_col = col;
                    }
                    last.input = Some(abs);
                }
            }
            b'C' => {
                let col = self.term.grid().cursor.point.column.0;
                let blank = self.marks.back().is_some_and(|m| self.command_line_blank(m, abs, col));
                if let Some(last) = self.marks.back_mut() {
                    last.output = Some(abs);
                    // Nothing typed, nothing ran: close the block WITHOUT an exit
                    // code, so the `D` that follows marks nothing failed.
                    last.finished |= blank;
                }
                if blank {
                    // …and completes nothing (notification, pulse, tab badge).
                    self.cur_cmd = None;
                } else {
                    // Command START: stamp the monotonic clock so `D` can compute
                    // a duration. One `Instant::now()`, once per command, on the
                    // already-off-hot-path OSC-133 scanner (never per byte).
                    let now = std::time::Instant::now();
                    match self.cur_cmd.as_mut() {
                        Some(c) => c.started_at = Some(now),
                        None => self.cur_cmd = Some(OpenCmd { started_at: Some(now) }),
                    }
                }
                // A command ran: the resize clean-prompt wipe must never fire again.
                self.saw_command_output = true;
                self.cmd_marks_dirty = true;
                // Keyboard: the command starts from the shell's current flags.
                self.open_kbd_window(false);
            }
            b'D' => {
                // Failed-command marker: bind to the most-recent still-open block
                // (shells emit strictly A…B…C…D, so "most recent open" is correct
                // even with gaps). Absent if anchors were dropped meanwhile.
                let mut output_top = None;
                if let Some(block) = self.marks.iter_mut().rev().find(|m| !m.finished) {
                    block.exit = exit;
                    block.finished = true;
                    // Where its output starts: the C row, else the row after the
                    // command line (an integration without C).
                    output_top = Some(block.output.unwrap_or(block.input.unwrap_or(block.prompt) + 1));
                }
                // Run & Notify: emit a completion iff a command was open (a
                // spurious lone D produces nothing). Independent of `marks`, so a
                // reflow / scroll overflow during a long build cannot lose it.
                if let Some(cmd) = self.cur_cmd.take() {
                    // `Some(elapsed)` iff a C was seen; `None` otherwise (an
                    // integration without C) → the completion reports unknown time.
                    let duration = cmd.started_at.map(|t| t.elapsed());
                    let last_line = self.last_output_line(output_top);
                    self.completed.push(CommandCompletion { exit_code: exit, duration, last_line });
                    // Bound undrained completions; drop the oldest on overflow.
                    if self.completed.len() > MAX_PENDING_COMPLETIONS {
                        self.completed.remove(0);
                    }
                }
                // Also covers integrations that never send C (old bash): once a
                // command has completed, the clean-prompt wipe is off for good.
                self.saw_command_output = true;
                // The command is over, and so is any progress it reported.
                self.set_progress(None);
                self.cmd_marks_dirty = true;
                // Keyboard: drop what the finished command left behind.
                self.restore_kbd(true);
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
            // Publish the metric to the shared atomic so the EventProxy's
            // `\e[14t` reply reports real cell px (A5).
            self.cell_px.store(pack_cell_px(w, h), Ordering::Relaxed);
        }
    }

    /// Finish a sixel DCS at its terminator: decode the accumulated bytes and
    /// place the image at the cursor, cut at the grid's edge, as xterm does.
    /// PRIMARY screen: reserve its cell rows (the cursor moves below it,
    /// scrolling as needed, so `abs_top` tracks the image). ALT screen (TUI
    /// previews): no scrolling; it lives until a covered cell is written (see
    /// `check_alt_placements`).
    ///
    /// Correct-or-absent guards (drop, touch nothing): a buffer overflow or a
    /// CAN/SUB-cancelled DCS (`sixel_overflow`), a zero cell metric, a decode
    /// failure, an image the image-work bank cannot pay for, or an anchor reset
    /// caused by the reserve itself. A synchronized update in flight is flushed
    /// first, so the cursor is where the app put it.
    fn finish_sixel(&mut self) {
        let buf = std::mem::take(&mut self.sixel_buf);
        let overflow = std::mem::take(&mut self.sixel_overflow);
        let p2 = self.pending_sixel_p2;
        if overflow || self.cell_px_w <= 0.0 || self.cell_px_h <= 0.0 {
            return;
        }
        // The decoder's measuring pass (one walk over the bytes) refuses an
        // image larger than the bank can pay for before anything is allocated;
        // the one it draws is paid for here.
        let caps = crate::sixel::SixelCaps {
            max_pixels: (self.image_work / 4).min(u64::from(crate::sixel::SIXEL_CAPS.max_pixels)) as u32,
            ..crate::sixel::SIXEL_CAPS
        };
        let Some(img) = crate::sixel::decode_sixel(p2, &buf, caps) else {
            return;
        };
        self.image_work = self.image_work.saturating_sub(img.rgba.len() as u64);
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
        let (cols, rows, draw) = self.image_geometry(img.width, img.height, 0, 0);
        let p = ImagePlacement {
            id: crate::sixel::content_id(&[SIXEL_TAG, p2], &buf),
            abs_line: 0,
            col: 0,
            cols,
            rows,
            draw,
            image: Arc::new(img),
            kitty_id: None,
            kitty_placement: None,
            is_kitty: false,
            marks_cells: false,
        };
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.place_alt_at_cursor(p, true, true);
        } else {
            // Rows the bank cannot pay for drop the sixel (correct-or-absent).
            let _ = self.place_primary_at_cursor(p, true, true);
        }
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
    /// Chunk state machine: a continuation chunk carries ONLY `m=` (and maybe
    /// `q=`) — `!has_control`. A first chunk carries the image's keys, with or
    /// without `a=` (the spec's own chunked example omits it). While an
    /// accumulation is in progress, only a continuation appends/finalizes; any
    /// other APC ABORTS the partial and is handled fresh — never spliced.
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
            if !cmd.has_control {
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
            // A new command while accumulating ⇒ ABORT the partial, then handle
            // `cmd` fresh below (never splice).
            self.reset_kitty_chunks();
        }

        // Fresh command. A first chunk (`m=1` with the image's keys) starts an
        // accumulation. An orphan continuation (`m=1`, only `m`/`q`, nothing in
        // progress) is a stray fragment — ignore it (this also caps an
        // endless-`m=1` stream that has already aborted its accumulation).
        if cmd.more == 1 {
            if !cmd.has_control {
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
            None => self.kitty_reply(&cmd, cmd.id, "EBADF"),
        }
    }

    /// Dispatch a finalized Kitty command with its RAW (base64-decoded) payload.
    /// `raw` is meaningful only for transmit/query; delete/put ignore it.
    fn handle_kitty_command(&mut self, cmd: KittyCmd, raw: Vec<u8>) {
        if cmd.id != 0 && cmd.number != 0 {
            // Naming an image both ways is an error (the spec).
            self.kitty_reply(&cmd, cmd.id, "EINVAL");
            return;
        }
        if cmd.virtual_placement && matches!(cmd.action, b'T' | b'p') {
            // Unicode-placeholder (virtual) placements are not rendered: refuse,
            // so the client falls back instead of printing placeholder cells.
            self.kitty_reply(&cmd, cmd.id, "ENOTSUPP");
            return;
        }
        match cmd.action {
            b'd' => self.kitty_delete(&cmd),
            b'p' => self.kitty_put(&cmd),
            b'q' => {
                // Validate/decode WITHOUT displaying or storing, then reply. A
                // reply nobody receives (no `i=`/`I=`, or `q=2`) is worth no
                // work: kitty drops an unaddressed query before loading it too.
                if !cmd.addressable() || cmd.quiet >= 2 {
                    return;
                }
                let reply = if cmd.medium != b'd' {
                    "ENOTSUPP"
                } else {
                    self.decode_kitty_image(&cmd, raw).map_or_else(|code| code, |_| "OK")
                };
                self.kitty_reply(&cmd, cmd.id, reply);
            }
            b't' | b'T' => {
                // Only direct base64 transmission is supported (safety: an
                // untrusted PTY must not make us open files / shm).
                if cmd.medium != b'd' {
                    self.kitty_reply(&cmd, cmd.id, "ENOTSUPP");
                    return;
                }
                // Transmit-only without an `i=`/`I=`: nothing could ever put or
                // answer it, so there is nothing to decode.
                if cmd.action == b't' && !cmd.addressable() {
                    return;
                }
                let params = [KITTY_TAG, cmd.format.into(), cmd.width, cmd.height, cmd.compressed.into()];
                let content = crate::sixel::content_id(&params, &raw);
                match self.decode_kitty_image(&cmd, raw) {
                    Ok(img) => {
                        let img = Arc::new(img);
                        // Transmit: store in the registry if addressable (an
                        // `I=` image is given an id there).
                        let id = if cmd.addressable() { self.kitty_store(&cmd, content, img.clone()) } else { 0 };
                        // Display on `a=T`.
                        let shown = match cmd.action {
                            b'T' => self.place_inline_image(&img, content, id, &cmd),
                            _ => Ok(()),
                        };
                        self.kitty_reply(&cmd, id, shown.err().unwrap_or("OK"));
                    }
                    Err(code) => self.kitty_reply(&cmd, cmd.id, code),
                }
            }
            // Empty `a=` (action 0) is a malformed command.
            0 => self.kitty_reply(&cmd, cmd.id, "EINVAL"),
            // Animation (`a=a`/`a=f`) and any other action: documented non-goal.
            _ => self.kitty_reply(&cmd, cmd.id, "ENOTSUPP"),
        }
    }

    /// Turn a Kitty command's RAW payload (already base64-decoded and
    /// accumulated) into a decoded `InlineImage`: apply `o=z` zlib inflate if
    /// requested, then dispatch by `f=` format. Each step is paid from the
    /// image-work bank BEFORE it runs — the inflate's output, then the RGBA
    /// sized from the header (`s`×`v`, or the PNG's `IHDR`) — and the decoder is
    /// capped at what was paid for. `Err` is the reply: `EBADF` for a bad
    /// payload (correct-or-absent), [`IMAGE_BUSY`] when the bank cannot pay.
    fn decode_kitty_image(&mut self, cmd: &KittyCmd, raw: Vec<u8>) -> Result<crate::sixel::InlineImage, &'static str> {
        use crate::kitty::{checked_pixels, decode_png, decode_rgb, decode_rgba, inflate_zlib, png_size};
        use crate::sixel::{SixelCaps, SIXEL_CAPS};
        // Bytes per pixel of a raw format, which declares its size (`s`×`v`);
        // 0 for a PNG, whose header does.
        let bpp = match cmd.format {
            24 => 3,
            32 => 4,
            100 => 0,
            _ => return Err("EBADF"),
        };
        let data = if cmd.compressed {
            // A raw format inflates to exactly its pixels' bytes, a PNG to at
            // most the raw budget: pay that up front; a stream that inflates
            // gets back what it did not use.
            let max = match bpp {
                0 => KITTY_RAW_BUDGET,
                _ => checked_pixels(cmd.width, cmd.height, SIXEL_CAPS).ok_or("EBADF")? * bpp,
            };
            self.pay_image_work(max as u64)?;
            let data = inflate_zlib(&raw, max).ok_or("EBADF")?;
            self.image_work += (max - data.len()) as u64;
            data
        } else {
            raw
        };
        let (w, h) = match bpp {
            0 => png_size(&data).ok_or("EBADF")?,
            _ => (cmd.width, cmd.height),
        };
        let px = checked_pixels(w, h, SIXEL_CAPS).ok_or("EBADF")?;
        if bpp != 0 && data.len() != px * bpp {
            return Err("EBADF"); // the wrong amount of pixel data decodes to nothing
        }
        self.pay_image_work(px as u64 * 4)?;
        let caps = SixelCaps { max_w: w, max_h: h, max_pixels: px as u32 };
        match bpp {
            3 => decode_rgb(w, h, &data, caps),
            4 => decode_rgba(w, h, &data, caps),
            _ => decode_png(&data, caps),
        }
        .ok_or("EBADF")
    }

    /// Take `bytes` from the image-work bank — or nothing, and `Err(IMAGE_BUSY)`,
    /// when it holds less: the image is then dropped.
    fn pay_image_work(&mut self, bytes: u64) -> Result<(), &'static str> {
        if bytes > self.image_work {
            return Err(IMAGE_BUSY);
        }
        self.image_work -= bytes;
        Ok(())
    }

    /// Store a decoded image (and its `content` id) in the transmit-then-put
    /// registry of the active screen and return its id: the `i=` it was sent
    /// with, or a free one for an `I=` transmit — every such transmit is a new
    /// image, its number naming the newest. Re-sending an `i=` replaces that
    /// image and deletes its placements (the spec). LRU-evicts to stay within
    /// both caps, images no placement shows first (as kitty frees them).
    fn kitty_store(&mut self, cmd: &KittyCmd, content: u64, image: Arc<crate::sixel::InlineImage>) -> u32 {
        let alt = self.alt_screen();
        let id = if cmd.id != 0 {
            self.drop_stored_image(alt, cmd.id);
            self.delete_placements(|p| p.kitty_id == Some(cmd.id));
            cmd.id
        } else {
            self.free_image_id(alt)
        };
        self.kitty_seq += 1;
        self.kitty_stored_bytes += image.rgba.len() as u64;
        self.kitty_images.push_back(StoredImage { id, number: cmd.number, alt, seq: self.kitty_seq, content, image });
        while self.kitty_images.len() > MAX_KITTY_STORED || self.kitty_stored_bytes > MAX_KITTY_STORED_BYTES {
            // Never the image just stored.
            let older = self.kitty_images.len() - 1;
            let unshown = self.kitty_images.iter().take(older).position(|e| !self.shows(e));
            let Some(old) = self.kitty_images.remove(unshown.unwrap_or(0)) else { break };
            self.kitty_stored_bytes = self.kitty_stored_bytes.saturating_sub(old.image.rgba.len() as u64);
        }
        id
    }

    /// Whether a placement on its screen shows stored image `e`.
    fn shows(&self, e: &StoredImage) -> bool {
        let list = if e.alt { &self.alt_placements } else { &self.placements };
        list.iter().any(|p| p.kitty_id == Some(e.id))
    }

    /// The smallest image id nothing on that screen uses (kitty gives an `I=`
    /// transmit the same), so a later `i=` names this image alone.
    fn free_image_id(&self, alt: bool) -> u32 {
        let list = if alt { &self.alt_placements } else { &self.placements };
        let mut used: Vec<u32> = self.kitty_images.iter().filter(|e| e.alt == alt).map(|e| e.id).collect();
        used.extend(list.iter().filter_map(|p| p.kitty_id));
        used.sort_unstable();
        let mut id = 1;
        for u in used {
            if u == id {
                id += 1;
            } else if u > id {
                break;
            }
        }
        id
    }

    /// The registry index of the active screen's image `cmd` names: by `i=`,
    /// or the newest with its `I=` number.
    fn find_image(&self, cmd: &KittyCmd) -> Option<usize> {
        let alt = self.alt_screen();
        let mine = self.kitty_images.iter().enumerate().filter(|(_, e)| e.alt == alt);
        if cmd.id != 0 {
            mine.filter(|(_, e)| e.id == cmd.id).map(|(k, _)| k).next()
        } else {
            mine.filter(|(_, e)| cmd.number != 0 && e.number == cmd.number).max_by_key(|(_, e)| e.seq).map(|(k, _)| k)
        }
    }

    /// Drop image `id` of that screen from the registry (its placements stay).
    fn drop_stored_image(&mut self, alt: bool, id: u32) {
        if let Some(pos) = self.kitty_images.iter().position(|e| e.alt == alt && e.id == id) {
            if let Some(old) = self.kitty_images.remove(pos) {
                self.kitty_stored_bytes = self.kitty_stored_bytes.saturating_sub(old.image.rgba.len() as u64);
            }
        }
    }

    /// Drop the active screen's placements `hit` matches.
    fn delete_placements(&mut self, hit: impl Fn(&ImagePlacement) -> bool) {
        if self.alt_screen() {
            retain_placements(&mut self.alt_placements, &mut self.alt_placement_bytes, |p| !hit(p));
        } else {
            retain_placements(&mut self.placements, &mut self.placement_bytes, |p| !hit(p));
        }
    }

    /// Display a previously-transmitted image of the active screen (`a=p,i=N`,
    /// or `I=N`: the newest with that number) under the content id it was
    /// stored with — a put costs nothing per pixel — and mark it recently used.
    /// Unknown ⇒ `ENOENT` and no placement.
    fn kitty_put(&mut self, cmd: &KittyCmd) {
        let Some(e) = self.find_image(cmd).and_then(|pos| self.kitty_images.remove(pos)) else {
            self.kitty_reply(cmd, cmd.id, "ENOENT");
            return;
        };
        let (id, content, img) = (e.id, e.content, e.image.clone());
        self.kitty_images.push_back(e);
        let shown = self.place_inline_image(&img, content, id, cmd);
        self.kitty_reply(cmd, id, shown.err().unwrap_or("OK"));
    }

    /// `a=d`: delete the active screen's Kitty placements the `d=` selector
    /// picks — each screen has its own, as in kitty, so a full-screen program
    /// clearing its images never touches the shell's. `a` (the default): those
    /// visible on screen, not the scrollback's; `i`: those of image `i=`; `n`:
    /// of the newest image numbered `I=` — narrowed to one placement by `p=`.
    /// An uppercase selector also frees the stored images it names that no
    /// placement on the screen still shows. The other selectors (by cell, row,
    /// column, z-index): documented no-op (amendment A6 / T9).
    fn kitty_delete(&mut self, cmd: &KittyCmd) {
        let alt = self.alt_screen();
        let top = if alt { 0 } else { self.abs_top };
        let selector = cmd.delete.to_ascii_lowercase();
        // The image `i` / `n` names (0: none).
        let id = match selector {
            b'i' => cmd.id,
            b'n' => self.find_image(&KittyCmd { id: 0, ..*cmd }).map_or(0, |pos| self.kitty_images[pos].id),
            _ => 0,
        };
        self.delete_placements(|p| match selector {
            // Never a sixel: a Kitty clear must not wipe another protocol's
            // images (M2).
            0 | b'a' => p.is_kitty && p.abs_line + p.rows as i64 > top,
            b'i' | b'n' => {
                id != 0 && p.kitty_id == Some(id) && (cmd.placement == 0 || p.kitty_placement == Some(cmd.placement))
            }
            _ => false,
        });
        if cmd.delete.is_ascii_uppercase() {
            let freed: Vec<u32> = self
                .kitty_images
                .iter()
                .filter(|e| e.alt == alt && (selector == b'a' || e.id == id) && !self.shows(e))
                .map(|e| e.id)
                .collect();
            for id in freed {
                self.drop_stored_image(alt, id);
            }
        }
    }

    /// Enqueue a Kitty graphics OK/error reply on the PTY write-back channel,
    /// honoring the addressability + quiet rules (amendment A10):
    /// only reply when the command addresses an image (`i=`/`I=`), and never at
    /// `q>=2`; `q>=1` suppresses OK but still reports errors. Like kitty's, it
    /// names the image — its `id` (the one an `I=` transmit was given), number
    /// and placement. A synchronized update in flight is applied first, so the
    /// reply keeps its place behind the answers vte holds in it.
    fn kitty_reply(&mut self, cmd: &KittyCmd, id: u32, msg: &str) {
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
        self.apply_pending_sync();
        let keys: Vec<String> = [("i", id), ("I", cmd.number), ("p", cmd.placement)]
            .into_iter()
            .filter(|&(_, v)| v != 0)
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let _ = self.reply_tx.send(format!("\x1b_G{};{msg}\x1b\\", keys.join(",")).into_bytes());
    }

    /// The cell footprint (`cols`, `rows`) of a `width`×`height` px image and
    /// the rectangle it draws in ([`ImagePlacement::draw`]), for a Kitty `c=` /
    /// `r=` request (0 = not given; a sixel gives neither): its native size, or
    /// scaled up or down to fill the columns / rows given — one given, the other
    /// follows the aspect ratio; both, it is as large as fits that box and
    /// centred in it (the spec's letterbox). At most [`MAX_IMAGE_ROWS`] rows (a
    /// taller image shrinks to fit them, never painting over the rows below its
    /// reservation) and the grid's width: a wider image is cut at the grid's
    /// edge where it is drawn, not squashed.
    fn image_geometry(&self, width: u32, height: u32, c: u16, r: u16) -> (u16, u16, [f32; 4]) {
        let (cw, ch) = (self.cell_px_w, self.cell_px_h);
        let (iw, ih) = (width as f32, height as f32);
        let (c, r) = (f32::from(c), f32::from(r));
        let scale = match (c > 0.0, r > 0.0) {
            (false, false) => 1.0,
            (true, false) => c * cw / iw,
            (false, true) => r * ch / ih,
            (true, true) => (c * cw / iw).min(r * ch / ih),
        };
        let rows = if r > 0.0 { r } else { (ih * scale / ch).ceil() }.clamp(1.0, MAX_IMAGE_ROWS as f32);
        let scale = scale.min(rows * ch / ih);
        let (w, h) = (iw * scale / cw, ih * scale / ch);
        let cols = if c > 0.0 { c } else { w.ceil() }.max(1.0);
        let (x, y) = if c > 0.0 && r > 0.0 { ((cols - w) / 2.0, (rows - h) / 2.0) } else { (0.0, 0.0) };
        (cols.min(self.cols as f32) as u16, rows as u16, [x, y, w, h])
    }

    /// Place a decoded Kitty image (content id `content`; registry id `id`, 0
    /// for an anonymous one) at the cursor, moved left to fit; `C=1` leaves the
    /// cursor (and the grid) untouched. Guards: a zero cell metric drops it; an
    /// in-flight sync update is flushed first so the cursor is where the app
    /// put it. `Err` is the reply: [`IMAGE_BUSY`] when the image-work bank
    /// cannot pay for the rows it reserves.
    fn place_inline_image(
        &mut self,
        img: &Arc<crate::sixel::InlineImage>,
        content: u64,
        id: u32,
        cmd: &KittyCmd,
    ) -> Result<(), &'static str> {
        if self.cell_px_w <= 0.0 || self.cell_px_h <= 0.0 {
            return Ok(());
        }
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
        let (cols, rows, draw) = self.image_geometry(img.width, img.height, cmd.cols, cmd.rows);
        let p = ImagePlacement {
            id: content,
            abs_line: 0,
            col: 0,
            cols,
            rows,
            draw,
            image: img.clone(),
            kitty_id: (id != 0).then_some(id),
            kitty_placement: (cmd.placement != 0).then_some(cmd.placement),
            is_kitty: true,
            marks_cells: false,
        };
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.place_alt_at_cursor(p, false, !cmd.no_cursor_move);
            Ok(())
        } else {
            self.place_primary_at_cursor(p, !cmd.no_cursor_move, false)
        }
    }

    /// Currently-visible inline images mapped to VIEWPORT rows, off the per-cell
    /// snapshot path (SPEED — mirrors `failed_prompt_rows`). On the alt screen,
    /// its own placements (anchored at screen rows); on the primary, the
    /// scrollback-anchored ones. A placement is kept iff its row SPAN intersects
    /// the visible grid `[0, rows)` — the SAME span test as `prune_placements`.
    pub fn visible_images(&self) -> Vec<crate::snapshot::VisibleImage> {
        let (list, shift) = if self.term.mode().contains(TermMode::ALT_SCREEN) {
            (&self.alt_placements, 0)
        } else {
            (&self.placements, self.term.grid().display_offset() as i64 - self.abs_top)
        };
        let (cw, ch) = (self.cell_px_w, self.cell_px_h);
        list.iter()
            .filter_map(|p| {
                // Viewport row of the image's top-left cell (may be negative).
                let top = p.abs_line + shift;
                let bottom = top + p.rows as i64;
                if bottom <= 0 || top >= self.rows as i64 {
                    return None; // span does not intersect the visible grid
                }
                let [x, y, w, h] = p.draw;
                Some(crate::snapshot::VisibleImage {
                    id: p.id,
                    top_row: top as f32,
                    col: p.col,
                    cols: p.cols,
                    rows: p.rows,
                    px_x: x * cw,
                    px_y: y * ch,
                    // Whole pixels: at the cell size it was placed at, a
                    // native-size image is exactly its own size again.
                    px_w: (w * cw).round(),
                    px_h: (h * ch).round(),
                })
            })
            .collect()
    }

    /// The decoded RGBA image for a visible placement id (cheap `Arc` clone), so
    /// the render layer can upload it once per window. `None` if the placement was
    /// pruned since `visible_images` was called.
    pub fn image_rgba(&self, id: u64) -> Option<Arc<crate::sixel::SixelImage>> {
        self.placements
            .iter()
            .chain(self.alt_placements.iter())
            .find(|p| p.id == id)
            .map(|p| p.image.clone())
    }

    /// Viewport rows (0-based) of currently-visible FAILED-command prompts
    /// (`D;<nonzero>`), for the themed left-edge marker. Empty in the common case
    /// and on the alt screen. Kept OFF the per-cell `GridSnapshot` so the render
    /// hot loop is untouched (SPEED). Uses the SAME `display_offset` mapping as
    /// `snapshot()`.
    pub fn failed_prompt_rows(&self) -> Vec<u16> {
        // On the alt screen a TUI owns the display: render nothing.
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
        // No target on the alt screen or with no marks at all.
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
        let d0 = self.term.grid().display_offset();
        let parser = &mut self.parser;
        crate::handler::parse(&mut self.term, &mut self.vt, &self.reply_tx, |vt| parser.stop_sync(vt));
        let alt_after = self.term.mode().contains(TermMode::ALT_SCREEN);
        let h1 = self.term.grid().history_size();
        self.after_vte(alt_before, alt_after, h0, h1, d0);
    }

    /// The grid cell the cursor is drawn on: its own — or, on the right half of
    /// a wide glyph, the glyph's first cell. Only a REAL glyph counts:
    /// alacritty's renderable cursor steps left off any right half, which put
    /// the cursor a cell off on an orphaned half (DCH / ECH / EL through a wide
    /// char leave one) and underflowed in column 0 (a debug panic; release drew
    /// the cursor in the last column).
    fn cursor_cell(&self) -> Point {
        let grid = self.term.grid();
        let mut p = grid.cursor.point;
        if p.column.0 > 0
            && grid[p].flags.contains(Flags::WIDE_CHAR_SPACER)
            && grid[p.line][p.column - 1].flags.contains(Flags::WIDE_CHAR)
        {
            p.column -= 1;
        }
        p
    }

    /// The viewport cell `(row, col)` of the cursor — the snapshot's
    /// `cursor_row` / `cursor_col`, without building a snapshot (for UI placed
    /// at the cursor on demand, like the keyboard-opened context menu).
    /// Clamped into the view the same way: scrolled back past the cursor, it
    /// is on the bottom row. Hidden (DECTCEM) or not, it is where the cursor is.
    pub fn cursor_viewport_cell(&self) -> (usize, usize) {
        point_to_viewport(self.term.grid().display_offset(), self.cursor_cell())
            .map(|p| (p.line.min(self.rows.saturating_sub(1)), p.column.0.min(self.cols.saturating_sub(1))))
            .unwrap_or((0, 0))
    }

    pub fn snapshot(&self) -> GridSnapshot {
        let (rows, cols) = (self.rows, self.cols);
        // Filled in row-major order below (every cell is pushed exactly once), so
        // there is no blank pre-fill for the loop to overwrite.
        let mut cells: Vec<CellSnapshot> = Vec::with_capacity(rows * cols);
        let mut graphemes = Vec::new();
        // The grid is read directly, not through `Term::renderable_content`:
        // that also computes the selection range (done once, below) and a
        // cursor that underflows on an orphaned wide-char half (`cursor_cell`).
        let grid = self.term.grid();
        let display_offset = grid.display_offset();
        // Dynamic OSC 4/10/11/12 palette overrides (pywal, base16 hooks, etc.)
        // are stored in the Term's color table; consult it so redefined colors
        // actually change on screen, falling back to the static theme.
        let colors = self.term.colors();
        // `minimum_contrast` memo, seeded with the default text/background pair
        // (most cells); `None` while the feature is off.
        let mut mc_memo = (self.min_contrast > 1.0).then(|| {
            let bg = [self.theme.bg[0], self.theme.bg[1], self.theme.bg[2]];
            crate::contrast::ContrastMemo::new(self.min_contrast, self.theme.fg, bg)
        });
        // DECSCNM (`CSI ? 5 h` — vim's visual bell): the whole screen in
        // reverse video. Every cell swaps its colors, as SGR 7 swaps one (VTE's
        // reading: a reversed cell shows normal), and so does the frame.
        let reverse_screen = self.vt.reverse;
        // The plain-cell shortcut below holds only while nothing recolors such a
        // cell: one flag for the loop to test.
        let plain_as_is = mc_memo.is_none() && !reverse_screen;
        // Color resolution memo: neighbouring cells overwhelmingly share their
        // colors (a run of text, a background band), so the last palette color →
        // RGB pair is remembered for fg and for bg. `resolve_rgb` is pure for one
        // snapshot (theme and override table are fixed while it runs).
        let mut fg_memo = ColorMemo::new(&self.theme, colors);
        let mut bg_memo = ColorMemo::new(&self.theme, colors);
        // The visible rows, one slice per row: viewport row `r` is grid line
        // `r - display_offset` (negative = history) — the mapping
        // `point_to_viewport` inverts. Walking each row's cells as a slice costs
        // one ring-buffer lookup per ROW (a per-cell `display_iter` paid it per
        // cell) and lets the cell loop run without per-cell index math.
        let visible = rows.min(grid.screen_lines());
        for row in 0..visible {
            let line = Line(row as i32 - display_offset as i32);
            let row_cells = &grid[line][..];
            let row_start = cells.len();
            for (col, cell) in row_cells.iter().take(cols).enumerate() {
                // The common cell — no SGR attribute, no marks / underline color /
                // link (`extra`), `minimum_contrast` off, screen not reversed — is
                // exactly its char and two colors: everything below would leave
                // it unchanged.
                if cell.flags.is_empty() && cell.extra.is_none() && plain_as_is {
                    let fg = fg_memo.get(cell.fg);
                    let bg = bg_memo.get(cell.bg);
                    cells.push(CellSnapshot { c: cell.c, fg, bg, uline: fg, attrs: 0, selected: false });
                    continue;
                }
                // `bold_is_bright`: a bold cell's normal ANSI foreground (0–7)
                // takes its bright twin (8–15) before it is resolved.
                let fg_color = if self.bold_is_bright && cell.flags.contains(Flags::BOLD) {
                    bright_for_bold(cell.fg)
                } else {
                    cell.fg
                };
                let mut fg = fg_memo.get(fg_color);
                let mut bg = bg_memo.get(cell.bg);
                // Reverse video (`\e[7m`, also used by selections and `ls`
                // highlights): swap fg/bg after resolving to RGB so the cell
                // renders inverted once backgrounds are painted. A reversed
                // screen (DECSCNM) flips it again.
                if cell.flags.contains(Flags::INVERSE) != reverse_screen {
                    std::mem::swap(&mut fg, &mut bg);
                }
                // SGR 2 (dim): alacritty sets Flags::DIM but leaves fg as a
                // named color resolving to full brightness, so dim text would
                // be indistinguishable from normal. Pull the foreground a third
                // of the way toward the cell's background (`faint`) — less
                // contrast on dark AND light themes. Done after INVERSE so the
                // dimmed channel is whichever ends up fg.
                if cell.flags.contains(Flags::DIM) {
                    fg = faint(fg, bg);
                }
                // SGR 8 (conceal): the glyph must not be readable (password
                // echoes, secret-masking TUIs). Paint the foreground with the
                // cell's background so the character is invisible while its
                // background/layout are preserved. Done after INVERSE/DIM so it
                // wins over whatever ended up as fg.
                if cell.flags.contains(Flags::HIDDEN) {
                    fg = bg;
                } else if let Some(memo) = mc_memo.as_mut() {
                    // `minimum_contrast` (off = one predictable branch): the
                    // FINAL fg is pushed to the ratio against the final bg —
                    // even a palette color that equals the background
                    // (solarized_dark's 8: invisible zsh autosuggestions).
                    // Concealed text (above) stays invisible; powerline, block
                    // and sextant glyphs keep their colors (they draw shapes).
                    // A blank draws no glyph: skipped (the common cell).
                    if cell.c != ' ' && !crate::contrast::min_contrast_exempt(cell.c) {
                        fg = memo.get(fg, bg);
                    }
                }
                // A double-width glyph occupies two grid cells: the WIDE_CHAR
                // cell holds the actual char, and the following
                // WIDE_CHAR_SPACER cell is a placeholder. alacritty stores a
                // space (or stale char) in the spacer; the wide glyph from the
                // preceding cell already visually spans both columns via the
                // font, so we force the spacer to a blank to keep columns
                // aligned (preserving the spacer's own bg).
                // Combining marks / zero-width chars (NFD accents, VS16, ZWJ)
                // live in the cell's `zerowidth()` extra storage, separate from
                // `cell.c`. The per-cell `CellSnapshot` stays `Copy` (base char
                // only); cells that carry marks are listed SPARSELY in
                // `graphemes` (base + marks, capped) for the renderer to compose
                // — an empty `Vec`, no allocation, on the common path.
                let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
                let c = if spacer { ' ' } else { cell.c };
                // (`extra` also holds hyperlinks / underline colors: skip empty.)
                if let Some(marks) = cell.zerowidth().filter(|m| !spacer && !m.is_empty()) {
                    graphemes.push(CellGrapheme { row, col, text: grapheme_text(cell.c, marks) });
                }
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
                // rather than only its left half. The base cell (col-1) was pushed
                // just before the spacer. The spacer keeps its own bg (painted
                // above) and blank char.
                if spacer && col > 0 {
                    if let Some(base) = cells.last() {
                        attrs = base.attrs;
                        uline = base.uline;
                    }
                }
                cells.push(CellSnapshot { c, fg, bg, uline, attrs, selected: false });
            }
            // A row shorter than the grid (never expected): blank the rest.
            cells.resize(row_start + cols, CellSnapshot::default());
        }
        // Viewport rows the grid doesn't have (never expected): blank.
        cells.resize(rows * cols, CellSnapshot::default());

        // Mark selected cells: the selection range (terminal coordinates),
        // resolved once, over the viewport rows.
        if let Some(range) = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term)) {
            for vp_row in 0..self.rows {
                let term_point = viewport_to_point(display_offset, Point::new(vp_row, Column(0)));
                let term_line = term_point.line;
                // Skip rows outside the selection's line range.
                if term_line < range.start.line || term_line > range.end.line {
                    continue;
                }
                let row = &grid[term_line];
                let base = vp_row * self.cols;
                for col in 0..self.cols {
                    let pt = Point::new(term_line, Column(col));
                    if range.contains(pt) {
                        cells[base + col].selected = true;
                        // A wide glyph spans its cell and the spacer after it,
                        // and the copied text takes the whole char when either
                        // is selected: highlight both halves.
                        let flags = row[Column(col)].flags;
                        if flags.contains(Flags::WIDE_CHAR) && col + 1 < self.cols {
                            cells[base + col + 1].selected = true;
                        } else if flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 {
                            cells[base + col - 1].selected = true;
                        }
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
        let cursor_vp = point_to_viewport(display_offset, self.cursor_cell());
        let cursor_in_view = cursor_vp.map(|p| p.line < self.rows).unwrap_or(false);
        let (cursor_row, cursor_col) = cursor_vp
            .map(|p| (p.line.min(self.rows.saturating_sub(1)), p.column.0.min(self.cols.saturating_sub(1))))
            .unwrap_or((0, 0));

        // Apps hide the cursor with DECTCEM (`\e[?25l`). Also hide the cursor
        // when it has scrolled out of the viewport.
        let shown = self.term.mode().contains(TermMode::SHOW_CURSOR);
        let cursor_visible = shown && cursor_in_view;

        // Renderable cursor SHAPE (DECSCUSR `CSI Ps SP q`): 1/2 block, 3/4
        // underline, 5/6 beam (the user's default until a program sets one). A
        // hidden cursor reports the Block default (never drawn while invisible).
        let cursor_shape = match self.term.cursor_style().shape {
            _ if !shown => CursorShapeSnap::Block,
            CursorShape::Underline => CursorShapeSnap::Underline,
            CursorShape::Beam => CursorShapeSnap::Beam,
            CursorShape::HollowBlock => CursorShapeSnap::HollowBlock,
            CursorShape::Block | CursorShape::Hidden => CursorShapeSnap::Block,
        };

        // Scrollbar data: display_offset is how many lines we're scrolled up
        // (0 = at bottom). history_size() is the number of lines in the scrollback
        // buffer (total_lines - screen_lines), which is the maximum scroll offset.
        let scroll_offset = grid.display_offset();
        let scroll_max = grid.history_size();

        // Honor OSC 11 (background) / OSC 12 (cursor) dynamic overrides, keeping
        // the theme's background alpha; fall back to the theme when unset. A
        // reversed screen (DECSCNM) is cleared with the default foreground.
        let bg_rgba = if reverse_screen {
            use alacritty_terminal::vte::ansi::{Color, NamedColor};
            let [r, g, b] = resolve_rgb(&self.theme, colors, Color::Named(NamedColor::Foreground));
            [r, g, b, self.theme.bg[3]]
        } else {
            match colors[257] {
                Some(rgb) => [rgb.r, rgb.g, rgb.b, self.theme.bg[3]],
                None => self.theme.bg,
            }
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
            graphemes,
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
        // p10k / starship prompt-scatter fix (see the clear after `term.resize`
        // below). Capture the idle-clean-prompt state NOW, before `term.resize`
        // clears the marks it depends on.
        let clean_prompt = self.at_clean_prompt();
        // Primary-screen images ride the reflow on their rows.
        let image_rows = self.tag_image_rows();
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
        // `Term::resize` resets the scroll region to the whole screen.
        self.vt.region = (0, rows as i32);
        // p10k / starship prompt-scatter fix. alacritty's reflow rewraps a
        // full-width, absolute-positioned prompt into stray fragments (its
        // right-aligned segment lands on a wrapped row), and on GROW pulls
        // prompts that a prior SHRINK pushed into scrollback back into view —
        // stacking copies, worst on an empty tab. When we were idle at a clean
        // prompt with nothing else in the buffer, WIPE what the reflow just
        // produced — the rewrapped fragments AND the rows the shrink pushed into
        // scrollback — so a later grow reveals nothing. In alacritty `\e[2J`
        // scrolls the screen INTO scrollback, so `\e[3J` (clear scrollback) MUST
        // come LAST. The SIGWINCH `pty.resize` already sends makes the shell
        // repaint exactly ONE clean prompt. See `at_clean_prompt` for the gates
        // that keep this from ever erasing real output.
        if clean_prompt {
            // Straight through alacritty's `Handler` (= `\e[H\e[2J\e[3J`), never
            // injected into the parser; the anchors are re-established below.
            use alacritty_terminal::vte::ansi::{ClearMode, Handler};
            self.term.goto(0, 0);
            self.term.clear_screen(ClearMode::All);
            self.term.clear_screen(ClearMode::Saved);
        }
        // A reflow REWRAPS logical lines: a mark's physical row genuinely moves by
        // an amount unrelated to any scroll, so its stored absolute line no longer
        // points at its prompt. `Term::resize` also changes `history_size()`
        // outside `track_abs_top`, breaking the abs_top⇄history relationship. Since
        // the anchors are now meaningless, DROP the marks (correct-or-absent — the
        // next prompt re-marks) and re-establish a clean anchor so future marks and
        // pruning are exact again. Skip the re-anchor on the alt screen: its grid
        // has ~no history, so `history_size()` there would corrupt the primary
        // `abs_top` (which is frozen for the alt session). Primary-screen images
        // are carried over to the rows their anchor rows reflowed to (found by
        // the tags set above) — or, where that cannot be told, go too
        // (correct-or-absent). Alt-screen ones go: a TUI redraws on SIGWINCH.
        // Run & Notify state survives: a command running across the resize
        // still yields its completion.
        let images = std::mem::take(&mut self.placements);
        let moved = self.find_image_rows(image_rows.len());
        self.drop_anchors();
        self.clear_alt_placements();
        // A reflow also invalidates any in-progress Kitty chunk accumulation
        // (its anchor context changed) — drop it so it can't splice post-reflow.
        self.reset_kitty_chunks();
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.abs_top = self.term.grid().history_size() as i64;
            if let Some(moved) = moved {
                for mut p in images {
                    let Ok(k) = image_rows.binary_search(&p.abs_line) else { continue };
                    p.abs_line = self.abs_top + i64::from(moved[k]);
                    p.anchor_at(usize::from(p.col), cols);
                    self.placement_bytes += p.image.rgba.len() as u64;
                    self.placements.push_back(p);
                }
                self.prune_placements(self.term.grid().history_size());
            }
        }
        // Reflow moved every line (the epoch bump above tells the search):
        // re-collect the whole grid. Cheap no-op when no search is active.
        self.search_refresh();
    }

    /// Before a reflow: tag column 0 of every primary-screen image's anchor
    /// row with [`SIXEL_CELL`] (unused on the primary screen), so the rows can
    /// be found where alacritty's reflow puts them — it never splits column 0
    /// off a row, nor merges the line-fed rows an image reserves. Returns the
    /// tagged rows' absolute lines, ascending; none on the alt screen, where
    /// the primary grid is out of reach (its images go, correct-or-absent).
    #[cold]
    #[inline(never)]
    fn tag_image_rows(&mut self) -> Vec<i64> {
        if self.placements.is_empty() || self.term.mode().contains(TermMode::ALT_SCREEN) {
            return Vec::new();
        }
        let top = self.abs_top;
        let grid = self.term.grid_mut();
        let lines = i64::from(grid.topmost_line().0)..=i64::from(grid.bottommost_line().0);
        let mut rows: Vec<i64> = self.placements.iter().map(|p| p.abs_line).filter(|a| lines.contains(&(a - top))).collect();
        rows.sort_unstable();
        rows.dedup();
        for &abs in &rows {
            grid[Line((abs - top) as i32)][Column(0)].flags.insert(SIXEL_CELL);
        }
        rows
    }

    /// After the reflow: the grid lines of the `n` rows [`Terminal::tag_image_rows`]
    /// tagged, top to bottom, untagging them. `None` unless all `n` are found:
    /// rows the reflow dropped (history overflow, empty rows below the cursor,
    /// a wipe) leave no way to tell which image went with them.
    #[cold]
    #[inline(never)]
    fn find_image_rows(&mut self, n: usize) -> Option<Vec<i32>> {
        if n == 0 {
            return None;
        }
        let grid = self.term.grid_mut();
        let mut found = Vec::with_capacity(n);
        for l in grid.topmost_line().0..=grid.bottommost_line().0 {
            let flags = &mut grid[Line(l)][Column(0)].flags;
            if flags.contains(SIXEL_CELL) {
                flags.remove(SIXEL_CELL);
                found.push(l);
            }
        }
        (found.len() == n).then_some(found)
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

    /// The program-reported OSC 9;4 progress, if any is showing.
    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }

    /// Take a pending OSC 9;4 progress change: `None` = unchanged since the last
    /// call (the common case — one bool test on the drain pass), `Some(p)` = the
    /// progress is now `p` (`Some(None)` = cleared). Consuming.
    pub fn take_progress_update(&mut self) -> Option<Option<Progress>> {
        if !self.progress_dirty {
            return None;
        }
        self.progress_dirty = false;
        Some(self.progress)
    }

    /// Whether the shell is running a command right now: an OSC 133 `C` arrived
    /// and its `D` (or the next prompt's `A`) has not. Always `false` without
    /// shell integration.
    pub fn command_running(&self) -> bool {
        self.cur_cmd.is_some_and(|c| c.started_at.is_some())
    }

    /// True once if an OSC 133 `A`, `C` or `D` was bound since the last call —
    /// the shell started or finished a command. Consuming (one bool test).
    pub fn take_command_marks(&mut self) -> bool {
        std::mem::take(&mut self.cmd_marks_dirty)
    }

    /// Apply a complete OSC 9;4 report (`st`, `pr` as parsed; `None` = empty
    /// field). `st` 0 (or empty) clears; 1 sets a percentage (empty = 0); 2
    /// (error) and 4 (paused) keep the previous percentage when they carry none;
    /// 3 is indeterminate. An unknown `st` changes nothing.
    fn apply_progress(&mut self, st: Option<u8>, pr: Option<u16>) {
        let value = pr.map(|v| v.min(100) as u8);
        let prev = self.progress.and_then(|p| p.value);
        let next = match st.unwrap_or(0) {
            0 => None,
            1 => Some(Progress { state: ProgressState::Normal, value: Some(value.unwrap_or(0)) }),
            2 => Some(Progress { state: ProgressState::Error, value: value.or(prev) }),
            3 => Some(Progress { state: ProgressState::Indeterminate, value: None }),
            4 => Some(Progress { state: ProgressState::Paused, value: value.or(prev) }),
            _ => return,
        };
        self.set_progress(next);
    }

    /// Set the progress, flagging a change only when it really changed.
    fn set_progress(&mut self, p: Option<Progress>) {
        if self.progress != p {
            self.progress = p;
            self.progress_dirty = true;
        }
    }

    /// Text of the last non-empty grid row at/above the cursor, trimmed and
    /// capped — the notification body. Called ONCE per command (at `D`), never on
    /// the per-byte path. Precmd emits `D` then `A`, so at `D` the cursor sits
    /// just below the command's final output; the bottom-most non-empty row
    /// at/above it is that command's last output line. `output_top` (an absolute
    /// row) is where that output starts: the prompt and command line above it
    /// are not output, so a command that printed nothing (`sleep 30`) gets an
    /// empty body, not the prompt. `None` (its marks were dropped) scans up
    /// regardless. A wrapped long line returns only its bottom physical row
    /// (acceptable). Only base `cell.c` is read (same combining-mark limit as
    /// `snapshot`).
    fn last_output_line(&self, output_top: Option<i64>) -> String {
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
        let mut lo = (cursor_line - MAX_SCAN_ROWS).max(top);
        if let Some(abs) = output_top {
            lo = lo.max((abs - self.abs_top).clamp(i32::MIN.into(), i32::MAX.into()) as i32);
        }
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
        self.set_selection(Some(Selection::new(SelectionType::Simple, pt, side)));
    }

    /// Replace the selection. Every `selection_start*`, the clear and select-all
    /// go through here, which ends a URL double-click drag.
    fn set_selection(&mut self, sel: Option<Selection>) {
        self.url_select = None;
        self.word_select = None;
        self.term.selection = sel;
    }

    /// Start a SEMANTIC (word) selection at the given viewport cell — the
    /// double-click gesture. alacritty expands it to the surrounding word, bounded
    /// by its default semantic escape chars (whitespace, ``,│`|:"'()[]{}<>``);
    /// [`Terminal::selection_update`] then extends it word by word. Replaces any
    /// prior selection.
    ///
    /// On a plain-text URL the "word" is the whole URL (`:` would otherwise
    /// cut `https` off it): exactly the URL is selected, it stays whole while
    /// the pointer moves inside it, and a drag past it extends word by word.
    pub fn selection_start_semantic(&mut self, viewport_line: usize, col: usize) {
        let display_offset = self.term.grid().display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(col)));
        if let Some((first, last)) = self.plain_url_range(pt) {
            self.set_selection(Some(cell_range_selection(first, last)));
            self.url_select = Some(true);
            return;
        }
        self.select_words(pt, pt, Side::Left);
    }

    /// Select from double-clicked cell `origin` to `pt` as alacritty's word
    /// (Semantic) selection does — the bracket pair when `pt` is `origin` on a
    /// bracket, else whole words — resolved ONCE and stored as those cells,
    /// with `word_select` noting where `origin` is. Left Semantic, every
    /// `to_range` re-derived it — each frame, each selection_text — and on an
    /// unmatched bracket that is a bracket search through the whole
    /// scrollback (or the rest of the screen).
    fn select_words(&mut self, origin: Point, pt: Point, side: Side) {
        let mut sel = Selection::new(SelectionType::Semantic, origin, Side::Left);
        sel.update(pt, side);
        let Some(range) = sel.to_range(&self.term) else {
            self.set_selection(None);
            return;
        };
        self.set_selection(Some(cell_range_selection(range.start, range.end)));
        self.word_select = Some((origin.line.0 - range.start.line.0, origin.column.0));
    }

    /// Update the end of the current selection to the given viewport cell.
    /// `left_half` is the sub-cell x side (see [`Terminal::selection_start`]).
    /// Does nothing if no selection is active.
    pub fn selection_update(&mut self, viewport_line: usize, col: usize, left_half: bool) {
        let display_offset = self.term.grid().display_offset();
        let pt = viewport_to_point(display_offset, Point::new(viewport_line, Column(col)));
        if self.url_select.is_some() && self.extend_url_selection(pt) {
            return;
        }
        let side = if left_half { Side::Left } else { Side::Right };
        if let Some((lines, column)) = self.word_select {
            // The double-clicked cell, below the stored cells' first one.
            let range = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
            if let Some(range) = range {
                self.select_words(Point::new(range.start.line + lines, Column(column)), pt, side);
                return;
            }
        }
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(pt, side);
        }
    }

    /// Drag a URL double-click selection to buffer point `pt`: inside the URL
    /// it stays exactly the URL; past either end it grows from the URL by
    /// words (alacritty's semantic boundaries). `false` when the URL is gone
    /// (overwritten, scrolled off) — the caller then extends plainly.
    fn extend_url_selection(&mut self, pt: Point) -> bool {
        let Some(pin_start) = self.url_select else { return false };
        let Some(range) = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term)) else {
            return false;
        };
        let pinned = if pin_start { range.start } else { range.end };
        let Some((first, last)) = self.plain_url_range(pinned) else {
            self.url_select = None;
            return false;
        };
        let (from, to, pin_start) = if pt < first {
            (self.term.semantic_search_left(pt), last, false)
        } else if pt > last {
            (first, self.term.semantic_search_right(pt), true)
        } else {
            (first, last, true)
        };
        self.term.selection = Some(cell_range_selection(from, to));
        self.url_select = Some(pin_start);
        true
    }

    /// Convert a viewport row (0 = top of the visible grid) to its buffer line
    /// at the current scroll offset — independent of the scroll offset, but
    /// moved by every line output scrolls into history; keep a line across
    /// output as [`Terminal::viewport_row_abs`].
    pub fn viewport_line_to_buffer(&self, viewport_line: usize) -> i32 {
        let display_offset = self.term.grid().display_offset();
        viewport_to_point(display_offset, Point::new(viewport_line, Column(0))).line.0
    }

    /// The line viewport row `row` shows on the absolute scale marks and
    /// images use: it keeps naming the same text while output scrolls (a
    /// copy-mode selection anchor). Exact while something is anchored — a
    /// pinned view is — and until [`Terminal::anchor_epoch`] changes.
    pub fn viewport_row_abs(&self, row: usize) -> i64 {
        self.abs_top - self.term.grid().display_offset() as i64 + row as i64
    }

    /// The buffer line (as [`Terminal::viewport_line_to_buffer`] gives) of
    /// absolute line `abs`, clamped into the buffer: a line that scrolled out
    /// of its top is the topmost one left.
    pub fn abs_to_buffer_line(&self, abs: i64) -> i32 {
        let grid = self.term.grid();
        let (top, bottom) = (grid.topmost_line().0 as i64, grid.bottommost_line().0 as i64);
        (abs - self.abs_top).clamp(top, bottom) as i32
    }

    /// Bumped whenever the absolute lines are re-anchored — a reflow, a lost
    /// scroll count, RIS: an absolute line kept from before names other text.
    pub fn anchor_epoch(&self) -> u64 {
        self.anchor_epoch
    }

    /// Pin the view to the lines it shows (`true`) — even at the live bottom,
    /// output then scrolls in below them — for a mode that points at text on
    /// the screen (copy-mode's cursor, hint chips); `false` lets it follow
    /// the output again (put it back with [`Terminal::scroll_to_spot`]).
    pub fn set_view_pinned(&mut self, on: bool) {
        // The pin makes an anchor: a synchronized update still buffering bytes
        // the scanner saw without one is applied first (as for a search).
        if on && !self.view_pinned {
            self.apply_pending_sync();
        }
        self.view_pinned = on;
    }

    /// Where the view is now, to put it back with [`Terminal::scroll_to_spot`].
    pub fn view_spot(&self) -> ViewSpot {
        let offset = self.term.grid().display_offset();
        ViewSpot { offset, top: self.abs_top - offset as i64, epoch: self.anchor_epoch }
    }

    /// Put the view back on `spot`: at the live bottom when it was there, else
    /// on the lines it showed — output that scrolled in since does not move
    /// them — or, once a reflow re-anchored the lines, at its old offset.
    pub fn scroll_to_spot(&mut self, spot: ViewSpot) {
        let offset = if spot.offset == 0 || spot.epoch != self.anchor_epoch {
            spot.offset
        } else {
            (self.abs_top - spot.top).max(0) as usize
        };
        self.scroll_to_offset(offset);
    }

    /// Like [`Terminal::selection_start`] but anchored at an ABSOLUTE buffer
    /// line (independent of the scroll offset), so the anchor does not slide as
    /// the viewport scrolls. `buffer_line` comes from [`viewport_line_to_buffer`].
    pub fn selection_start_abs(&mut self, buffer_line: i32, col: usize, left_half: bool) {
        let pt = Point::new(Line(buffer_line), Column(col));
        let side = if left_half { Side::Left } else { Side::Right };
        self.set_selection(Some(Selection::new(SelectionType::Simple, pt, side)));
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

    /// Start a BLOCK (rectangular) selection at an ABSOLUTE buffer cell —
    /// copy-mode's Ctrl+V. [`Terminal::selection_update_abs`] moves the
    /// opposite corner; the copied text is each row's slice of the rectangle,
    /// trailing blanks trimmed, one line per row.
    pub fn selection_start_block_abs(&mut self, buffer_line: i32, col: usize, left_half: bool) {
        let pt = Point::new(Line(buffer_line), Column(col));
        let side = if left_half { Side::Left } else { Side::Right };
        self.set_selection(Some(Selection::new(SelectionType::Block, pt, side)));
    }

    /// Like [`Terminal::selection_start_lines`] but anchored at an ABSOLUTE
    /// buffer line, so a line-mode copy-mode anchor survives scrolling.
    pub fn selection_start_lines_abs(&mut self, buffer_line: i32) {
        let pt = Point::new(Line(buffer_line), Column(0));
        self.set_selection(Some(Selection::new(SelectionType::Lines, pt, Side::Left)));
    }

    /// Clear the active selection.
    pub fn selection_clear(&mut self) {
        self.set_selection(None);
    }

    /// The selected cells' bounds — `(start, end)` as (buffer line, column),
    /// inclusive, in reading order — or `None` when nothing is selected. No
    /// text is built: a mouse drag compares it to tell whether a step changed
    /// what is drawn.
    pub fn selection_bounds(&self) -> Option<((i32, usize), (i32, usize))> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        Some(((range.start.line.0, range.start.column.0), (range.end.line.0, range.end.column.0)))
    }

    /// Return the currently-selected text, or `None` if no selection is active
    /// or the selection is empty.
    pub fn selection_text(&self) -> Option<String> {
        // Built from the selection's range rather than `selection_to_string`,
        // whose rows ending in a wrapped wide char's placeholder copied a stray
        // character (see `text_between`); otherwise the same text.
        let sel = self.term.selection.as_ref()?;
        let range = sel.to_range(&self.term)?;
        if sel.ty != SelectionType::Block {
            let mut text = self.text_between(range.start, range.end, true);
            if sel.ty == SelectionType::Lines {
                text.push('\n');
            }
            return Some(text);
        }
        // A block: each row's slice of the rectangle, trailing blanks trimmed,
        // one line per row. As in alacritty, a row ending in a placeholder
        // takes its wrapped wide char along on the last row, and on the others
        // when the block does not start in column 0.
        // A corner on the right half of the last column moves the rectangle's
        // left edge PAST it (alacritty's range_block does not wrap it): no
        // column is selected then — and indexing one panicked.
        let empty = range.start.column.0 >= self.cols || range.start.column > range.end.column;
        let mut text = String::new();
        for line in range.start.line.0..=range.end.line.0 {
            let line = Line(line);
            let wrapped = line == range.end.line || range.start.column.0 != 0;
            let row = if empty {
                String::new()
            } else {
                self.text_between(Point::new(line, range.start.column), Point::new(line, range.end.column), wrapped)
            };
            text += row.trim_end();
            if line != range.end.line {
                text.push('\n');
            }
        }
        Some(text)
    }

    /// Whether the selection holds any text — a cell other than a blank —
    /// without building it: what the menus gray Copy / Run in New Tab on and
    /// the right-click routing asks, on every press. `selection_text` built
    /// the whole string for that bool — after Select All over a 100k-line
    /// scrollback, tens of MB, twice per right-click. Stops at the first such
    /// cell.
    pub fn has_selection(&self) -> bool {
        let Some(range) = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term)) else {
            return false;
        };
        let grid = self.term.grid();
        let last = self.cols - 1;
        (range.start.line.0..=range.end.line.0).any(|line| {
            let (first, end) = if range.is_block {
                (range.start.column.0, range.end.column.0)
            } else {
                let first = if line == range.start.line.0 { range.start.column.0 } else { 0 };
                (first, if line == range.end.line.0 { range.end.column.0 } else { last })
            };
            let row = &grid[Line(line)];
            (first..=end.min(last)).any(|c| row[Column(c)].c != ' ')
        })
    }

    /// `Term::bounds_to_string(start, end)` without alacritty 0.26's bug: when
    /// the text ENDS on a row whose last cell is the placeholder of a wide char
    /// that did not fit and wrapped to the next row, it appended the char at the
    /// start of the row ABOVE (`line - 1`) — a stray character in the copy, and
    /// out of range on the top row of the history (a debug panic, garbage in
    /// release). With `wrapped`, the wrapped wide char itself is taken along,
    /// as intended.
    fn text_between(&self, start: Point, end: Point, wrapped: bool) -> String {
        let grid = self.term.grid();
        let last = Column(self.cols - 1);
        if end.column != last || !grid[end.line][last].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            return self.term.bounds_to_string(start, end);
        }
        // Stop short of the placeholder: it holds no text.
        let mut text = self.term.bounds_to_string(start, Point::new(end.line, last - 1));
        if wrapped && end.line < grid.bottommost_line() {
            let cell = &grid[end.line + 1i32][Column(0)];
            if cell.flags.contains(Flags::WIDE_CHAR) {
                text.push(cell.c);
                text.extend(cell.zerowidth().into_iter().flatten());
            }
        }
        text
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

        // OSC 8 branch: underline the visible cells of the same link — the same
        // id AND URI, as the OSC 8 spec joins them (a multi-segment link the
        // app emitted under one id; id-less links get one generated id per OSC
        // run). Comparing `Hyperlink` values is one pointer compare for the
        // cells of one OSC run; both strings are bounded (see `handler.rs`).
        if let Some(link) = grid[pt].hyperlink() {
            let mut spans: Vec<(usize, usize, usize)> = Vec::new();
            // The text the link wears on screen, to tell whether it shows
            // its target.
            let mut text = String::new();
            for vp_row in 0..self.rows {
                let line = viewport_to_point(display_offset, Point::new(vp_row, Column(0))).line;
                for c in 0..self.cols {
                    let cell = &grid[Point::new(line, Column(c))];
                    let same = cell.hyperlink().as_ref() == Some(&link);
                    if same {
                        if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                            text.push(cell.c);
                        }
                        match spans.last_mut() {
                            Some(s) if s.0 == vp_row && s.2 + 1 == c => s.2 = c,
                            _ => spans.push((vp_row, c, c)),
                        }
                    }
                }
            }
            let uri = link.uri().to_string();
            let hidden_target = text.trim() != uri;
            return Some(LinkHit { uri, spans, hidden_target });
        }

        // Plain-text branch: the URL in the logical line around the cell.
        let (start_line, chars, s, e) = self.plain_url_chars(pt)?;
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
        Some(LinkHit { uri, spans, hidden_target: false })
    }

    /// The plain-text URL covering buffer point `pt`, as `(first row of its
    /// logical line, that line's chars, start, end)`: chars `start..end` are the
    /// URL, char `i` is the cell `(first + i / cols, i % cols)`. The logical
    /// (unwrapped) line is assembled around `pt` — a row continues onto the
    /// next when ITS last cell carries WRAPLINE — at most
    /// [`crate::url::MAX_WRAP_WALK`] rows each way, exactly `cols` chars per row
    /// with wide-char spacers blanked to ' ' (same rule as `snapshot`) so cell
    /// and char indices stay aligned.
    fn plain_url_chars(&self, pt: Point) -> Option<(i32, Vec<char>, usize, usize)> {
        let grid = self.term.grid();
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
        let idx = (pt.line.0 - start_line) as usize * self.cols + pt.column.0;
        let (s, e) = crate::url::find_url_at(&chars, idx)?;
        Some((start_line, chars, s, e))
    }

    /// The plain-text URL covering buffer point `pt`, as its first and last
    /// cells (inclusive).
    fn plain_url_range(&self, pt: Point) -> Option<(Point, Point)> {
        let (first, _, s, e) = self.plain_url_chars(pt)?;
        let cell = |i: usize| Point::new(Line(first + (i / self.cols) as i32), Column(i % self.cols));
        Some((cell(s), cell(e - 1)))
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
    /// tokens dedup to one entry (the bottom-most); the total is capped — at
    /// the NEWEST tokens — so the label alphabet stays short. Like `link_at`, spans are recomputed from a fresh grid every call —
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

        // Logical lines BOTTOM-UP, each one's tokens right to left: the cap keeps
        // the NEWEST tokens — the rows next to the prompt, the usual target —
        // and a duplicate is labelled where it last appears. Handed back in
        // reading order, so labels still go top-down.
        let mut tokens: Vec<HintToken> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ge = scan_end;
        'lines: while ge >= scan_start {
            // Group consecutive WRAPLINE rows into one logical line.
            let mut gs = ge;
            while gs > scan_start && wrapped(gs - 1) {
                gs -= 1;
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
            for (s, e, kind) in crate::hints::scan_line(&chars).into_iter().rev() {
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
                let at = (self.abs_top + (gs + (s / self.cols) as i32) as i64, s % self.cols);
                // The cell after it: in its logical line, else the next row's first.
                let next = chars.get(e).copied().unwrap_or_else(|| {
                    if ge < bottom {
                        let cell = &grid[Line(ge + 1)][Column(0)];
                        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c }
                    } else {
                        ' '
                    }
                });
                tokens.push(HintToken { text, kind, spans, at, epoch: self.anchor_epoch, next });
                if tokens.len() >= TOKEN_CAP {
                    break 'lines;
                }
            }
            ge = gs - 1;
        }
        tokens.reverse();
        tokens
    }

    /// Where hint token `tok`'s chip goes now: the viewport cell of the first
    /// of its cells on screen — the scan's first span while nothing moved, the
    /// text's new place after the view or the lines did. `None` once the text
    /// is no longer where it was scanned (rewritten, scrolled out of the
    /// buffer, the lines re-anchored by a reflow) or none of it is on screen.
    /// O(token length); per frame only while hint mode is up.
    pub fn hint_chip_cell(&self, tok: &HintToken) -> Option<(usize, usize)> {
        if tok.epoch != self.anchor_epoch || self.cols == 0 {
            return None;
        }
        let grid = self.term.grid();
        let top = self.abs_top - grid.history_size() as i64;
        let bottom = self.abs_top + self.rows as i64 - 1;
        let view_top = self.abs_top - grid.display_offset() as i64;
        let (line, col) = tok.at;
        let mut chip = None;
        // One char per cell, a wide char's spacer read as ' ' — as scanned —
        // and the cell after it unchanged too: a line still being printed at
        // the scan has not grown the token since.
        let len = tok.text.chars().count();
        for (i, ch) in tok.text.chars().chain([tok.next]).enumerate() {
            let abs = line + ((col + i) / self.cols) as i64;
            let c = (col + i) % self.cols;
            let after = i == len;
            if abs < top || abs > bottom {
                if after {
                    break;
                }
                return None;
            }
            let cell = &grid[Line((abs - self.abs_top) as i32)][Column(c)];
            let got = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c };
            if got != ch {
                return None;
            }
            let row = abs - view_top;
            if !after && chip.is_none() && (0..self.rows as i64).contains(&row) {
                chip = Some((row as usize, c));
            }
        }
        chip
    }

    /// The visible viewport as rows-of-chars (`rows` × `cols`, blank cells
    /// `' '`). A wide char's spacer — the right half of its glyph — reads as
    /// [`WIDE_SPACER`], so copy-mode can step over a wide char in one move and
    /// keep a run of them one word. Used by copy-mode motions so `w`/`b`/`e`
    /// can see neighbouring rows (BLOCKING 4). Keystroke-rate only.
    pub fn viewport_rows_chars(&self) -> Vec<Vec<char>> {
        let mut rows = vec![vec![' '; self.cols]; self.rows];
        let grid = self.term.grid();
        let display_offset = grid.display_offset();
        for item in grid.display_iter() {
            if let Some(vp) = point_to_viewport(display_offset, item.point) {
                if vp.line < self.rows && vp.column.0 < self.cols {
                    let cell = item.cell;
                    let c = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        WIDE_SPACER
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
        self.set_selection(Some(Selection::new(SelectionType::Lines, pt, Side::Left)));
    }

    /// Whether the terminal has bracketed paste mode enabled (`\e[?2004h`).
    pub fn bracketed_paste(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Lifetime count of distinct OSC 133 `A` prompt marks (post-dedup). The
    /// run-selection readiness signal: a fresh tab starts at 0; the first
    /// prompt makes it 1. Never reset (mark pruning does not affect it).
    pub fn prompt_count(&self) -> u64 {
        self.prompts_seen
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
        self.set_selection(Some(sel));
    }

    /// Set (or replace) the scrollback-search query and recompute all matches.
    ///
    /// The query is a LITERAL string (regex metachars are escaped) compiled
    /// with alacritty's built-in smart-case: an all-lowercase query matches
    /// case-insensitively, any uppercase char makes it case-sensitive. The
    /// query is truncated to [`SEARCH_MAX_QUERY`] chars and matches are capped
    /// at the [`SEARCH_MAX_MATCHES`] most recent. The current match becomes the bottom-most
    /// match at or above the viewport bottom (nearest as the user reads up)
    /// and the view scrolls to it if off-screen.
    ///
    /// Returns `(current 1-based, total)`, `(0, 0)` when there is no match
    /// (empty query, failed compile, or genuinely nothing found).
    pub fn search_set_query(&mut self, query: &str) -> (usize, usize) {
        // A search starting now makes the matches an anchor: a synchronized
        // update still buffering bytes the scanner saw without one (an ED 3 or
        // RIS not isolated, a shrink its replay would hide from `abs_top`)
        // is applied first.
        if self.search_regex.is_none() && !query.is_empty() {
            self.apply_pending_sync();
        }
        self.search_query = query.chars().take(SEARCH_MAX_QUERY).collect();
        self.search_regex = None;
        self.search_matches.clear();
        self.search_scan = None;
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
        let bottom_line = self.abs_top + self.rows as i64 - 1 - display_offset as i64;
        self.search_current = self
            .search_matches
            .iter()
            .rposition(|m| m.start.line <= bottom_line)
            .unwrap_or(self.search_matches.len() - 1);
        self.search_scroll_to_current();
        (self.search_current + 1, self.search_matches.len())
    }

    /// Clear all scrollback-search state (query, matches, highlights).
    pub fn search_clear(&mut self) {
        self.search_query.clear();
        self.search_regex = None;
        self.search_matches.clear();
        self.search_scan = None;
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
    #[cfg(test)]
    fn search_is_active(&self) -> bool {
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

    /// Bring the matches up to date with the existing query — called
    /// (throttled) after new PTY output, and after a resize or a scrollback
    /// change. Only what can have changed is re-read (see
    /// [`Terminal::search_collect`]). The current match stays the one it was
    /// while it survives (else the nearest newer one; after a reflow, the one
    /// as far from the bottom); never scrolls (streaming output must not fight
    /// the user's viewport).
    pub fn search_refresh(&mut self) {
        if self.search_regex.is_none() {
            return;
        }
        let current = self.search_matches.get(self.search_current).copied();
        let from_bottom = self.search_matches.len().saturating_sub(self.search_current + 1);
        // The stored lines still name the same text unless a reflow or a lost
        // scroll count re-anchored them.
        let same_lines = self.search_scan.is_some_and(|s| s.epoch == self.anchor_epoch);
        self.search_collect();
        let len = self.search_matches.len();
        self.search_current = match current {
            _ if len == 0 => 0,
            None => len - 1,
            Some(m) if same_lines => self.search_matches.partition_point(|n| n.start < m.start).min(len - 1),
            // A reflow keeps the matches in order, it only moves them.
            Some(_) => len.saturating_sub(from_bottom + 1),
        };
    }

    /// Visible match segments in viewport coordinates, split per row for
    /// wrapped matches, with the current match flagged. O(visible hits) via a
    /// binary search over the (ordered) match list — cheap per redraw.
    pub fn search_viewport_hits(&self) -> Vec<SearchHit> {
        // Lines a reflow or a lost scroll count re-anchored, or the other
        // screen's, name other text: nothing is lit until the re-collect.
        let stale = self
            .search_scan
            .is_some_and(|s| s.epoch != self.anchor_epoch || s.screens != self.screen_switches);
        if self.search_matches.is_empty() || stale {
            return Vec::new();
        }
        // The absolute lines the viewport shows.
        let top = self.abs_top - self.term.grid().display_offset() as i64;
        let bottom = top + self.rows as i64 - 1;
        // Matches are disjoint and ordered, so end-lines are monotonic too.
        let first = self.search_matches.partition_point(|m| m.end.line < top);
        let mut hits = Vec::new();
        for (i, m) in self.search_matches.iter().enumerate().skip(first) {
            if m.start.line > bottom {
                break;
            }
            for line in m.start.line.max(top)..=m.end.line.min(bottom) {
                let col_start = if line == m.start.line { m.start.col } else { 0 };
                let col_end = if line == m.end.line { m.end.col } else { self.cols.saturating_sub(1) };
                hits.push(SearchHit {
                    row: (line - top) as usize,
                    col_start,
                    col_end,
                    is_current: i == self.search_current,
                });
            }
        }
        hits
    }

    /// Bring `search_matches` up to date with the grid and the compiled regex
    /// (stored topmost→bottommost, capped at [`SEARCH_MAX_MATCHES`]).
    ///
    /// Collected BOTTOM-UP, so a query with more matches than the cap keeps
    /// the most RECENT ones — the screen the search starts from — and drops
    /// the oldest history. (Top-down, the cap kept the oldest: the visible
    /// output had no matches and the view jumped far up into history.) The
    /// early stop at the cap keeps a one-letter query in a huge scrollback as
    /// cheap as before. Each logical line is read on its own (a match spans
    /// soft-wrapped rows, never a hard line break — see [`collect_lines`]).
    ///
    /// After the first collect only what can have changed is read again.
    /// History never changes, so that is the lines that were on the screen
    /// then or arrived since (the zone), from the start of the logical line
    /// the old screen top belongs to — and, when rows scrolled out of the top
    /// since, the buffer's first logical line (the head): a wide char that
    /// wrapped onto it matched from the spacer the lost row ended in. The
    /// matches in between are kept. A capped collect reads on above its
    /// topmost match when the re-read lines came up short. That is exactly
    /// the list a full re-collect finds (`search_incremental_equals_a_full_scan`),
    /// at the cost of the new output instead of the whole history. The whole
    /// grid is read again after a reflow or a lost scroll count (the anchor
    /// epoch), after a screen switch, and on the alt screen (no history).
    fn search_collect(&mut self) {
        let max = self.search_cap();
        let Some(regex) = self.search_regex.as_mut() else {
            self.search_matches.clear();
            self.search_scan = None;
            return;
        };
        let term = &self.term;
        let grid = term.grid();
        let abs_top = self.abs_top;
        // The absolute lines of the buffer's first and last row.
        let top = abs_top - grid.history_size() as i64;
        let bottom = abs_top + grid.bottommost_line().0 as i64;
        let last_col = grid.last_column();
        let grid_line = |abs: i64| Line((abs - abs_top) as i32);
        let wrapped = |abs: i64| grid[grid_line(abs)][last_col].flags.contains(Flags::WRAPLINE);
        let to_point = |p: AbsPoint| Point::new(grid_line(p.line), Column(p.col));
        let to_abs = |p: Point| AbsPoint { line: abs_top + p.line.0 as i64, col: p.column.0 };
        let to_match = |m: &Match| SearchMatch { start: to_abs(*m.start()), end: to_abs(*m.end()) };
        // The first row of the logical line through row `abs`.
        let line_start = |mut abs: i64| {
            while abs > top && wrapped(abs - 1) {
                abs -= 1;
            }
            abs
        };
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let prev = self
            .search_scan
            .filter(|p| !alt && p.epoch == self.anchor_epoch && p.screens == self.screen_switches);
        // The zone starts at the old screen top's logical line — for a full
        // collect, at the top of the buffer.
        let seam = prev.map_or(top, |p| line_start(p.abs_top.clamp(top, abs_top)));
        let mut found = Vec::new();
        collect_lines(term, regex, grid_line(seam), grid_line(bottom), &mut found, max);
        let old = std::mem::take(&mut self.search_matches);
        let mut matches = Vec::with_capacity(old.len().max(found.len()));
        if let Some(p) = prev.filter(|_| found.len() < max) {
            // The head, `top..=head_end` — none (`top - 1`) unless rows
            // scrolled out of the top since and it lies above the zone.
            let mut head_end = top - 1;
            if top > p.top && seam > top {
                head_end = top;
                while head_end + 1 < seam && wrapped(head_end) {
                    head_end += 1;
                }
            }
            // Kept: the old matches between the head and the zone.
            let lo = old.partition_point(|m| m.start.line <= head_end);
            let hi = old.partition_point(|m| m.end.line < seam).max(lo);
            let kept = &old[lo..hi];
            let room = max - found.len();
            if kept.len() < room {
                // What a full collect reads above the kept matches.
                let cap = room - kept.len();
                let mut more = Vec::new();
                match old.first() {
                    // That collect stopped at the cap on this match: the rest
                    // of its line from the cell after it, then the lines above.
                    Some(m) if p.capped && m.start.line > head_end && m.end.line < seam => {
                        let start = line_start(m.start.line);
                        let first = Point::new(grid_line(start), Column(0));
                        let end = term.expand_wide(to_point(m.end), Direction::Left);
                        if end > first {
                            let from = end.sub(term, Boundary::None, 1);
                            collect_left(term, regex, from, first, Some(to_point(m.start)), &mut more, cap);
                        }
                        if start > top {
                            collect_lines(term, regex, grid_line(top), grid_line(start - 1), &mut more, cap);
                        }
                    }
                    // It read nothing above the zone.
                    Some(m) if p.capped && m.start.line >= seam => {
                        if seam > top {
                            collect_lines(term, regex, grid_line(top), grid_line(seam - 1), &mut more, cap);
                        }
                    }
                    // It read every line below the head: the head.
                    _ => {
                        if head_end >= top {
                            collect_lines(term, regex, grid_line(top), grid_line(head_end), &mut more, cap);
                        }
                    }
                }
                matches.extend(more.iter().rev().map(to_match));
            }
            matches.extend_from_slice(&kept[kept.len().saturating_sub(room)..]);
        }
        matches.extend(found.iter().rev().map(to_match));
        let capped = matches.len() >= max;
        self.search_matches = matches;
        let (epoch, screens) = (self.anchor_epoch, self.screen_switches);
        self.search_scan = Some(SearchScan { abs_top, top, epoch, screens, capped });
    }

    /// The most matches a search keeps: [`SEARCH_MAX_MATCHES`] (tests lower
    /// it to reach it).
    fn search_cap(&self) -> usize {
        #[cfg(test)]
        return self.search_cap;
        #[cfg(not(test))]
        SEARCH_MAX_MATCHES
    }

    /// Scroll so the current match is visible: no-op when it already is,
    /// otherwise center it (clamped to the valid scroll range).
    fn search_scroll_to_current(&mut self) {
        let Some(m) = self.search_matches.get(self.search_current) else {
            return;
        };
        // Its grid line, and the viewport row that shows it now.
        let line = m.start.line - self.abs_top;
        let row = line + self.term.grid().display_offset() as i64;
        if (0..self.rows as i64).contains(&row) {
            return;
        }
        // Desired offset centers the match: viewport row rows/2 shows term
        // line (rows/2 - offset), so offset = rows/2 - match_line.
        let max = self.scroll_max() as i64;
        let target = (self.rows as i64 / 2 - line).clamp(0, max);
        self.scroll_to_offset(target as usize);
    }
}

/// Collect `regex`'s matches in the logical lines of grid rows `first..=last`
/// (`first` starts one, `last` ends one) into `out`, bottom-up, until it
/// holds `cap`. Each line is read on its own: alacritty's one scan of the
/// whole buffer missed a hard line break right after a wide char in the last
/// columns (a match then ran across it), and ended for good at a match it
/// failed to confirm there, leaving every older line unsearched.
fn collect_lines<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    first: Line,
    last: Line,
    out: &mut Vec<Match>,
    cap: usize,
) {
    let last_col = term.last_column();
    let mut end = last;
    while end >= first && out.len() < cap {
        let mut start = end;
        while start > first && term.grid()[start - 1i32][last_col].flags.contains(Flags::WRAPLINE) {
            start -= 1i32;
        }
        collect_left(term, regex, Point::new(end, last_col), Point::new(start, Column(0)), None, out, cap);
        end = start - 1i32;
    }
}

/// Collect `regex`'s matches leftward from `from` down to `to` (inclusive)
/// into `out`, bottom-up, until it holds `cap`. `last` is the start of the
/// match the scan resumes after, if any. A leftward scan resumes inside the
/// match it just found, so a self-overlapping literal (`==` in `====`) also
/// yields overlapping matches: only those that end before the last kept one
/// starts are kept.
fn collect_left<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    from: Point,
    to: Point,
    last: Option<Point>,
    out: &mut Vec<Match>,
    cap: usize,
) {
    let mut floor = last;
    let mut prev = last;
    for m in RegexIter::new(from, to, Direction::Left, term, regex) {
        // A match that does not start further left than the one before it
        // means the scan wrapped around the buffer.
        if prev.is_some_and(|p| *m.start() >= p) {
            break;
        }
        prev = Some(*m.start());
        if floor.is_none_or(|f| *m.end() < f) {
            floor = Some(*m.start());
            out.push(m.clone());
            if out.len() >= cap {
                break;
            }
        }
        // The next step resumes one cell left of this match's (wide) end:
        // from `to` that is above it — past a wide char in the buffer's
        // top-left corner, around to its bottom, finding every match again.
        if term.expand_wide(*m.end(), Direction::Left) <= to {
            break;
        }
    }
}

/// A Simple selection covering the cells `first..=last`.
fn cell_range_selection(first: Point, last: Point) -> Selection {
    let mut sel = Selection::new(SelectionType::Simple, first, Side::Left);
    sel.update(last, Side::Right);
    sel
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
/// SGR 2 (faint): `fg` mixed 66% of the way from `bg` — always toward the
/// background, so faint text loses contrast on light themes too (the old plain
/// darkening pushed dark-on-light text toward black: MORE contrast).
pub(crate) fn faint(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    let mix = |f: u8, b: u8| (b as f32 + (f as f32 - b as f32) * 0.66).round().clamp(0.0, 255.0) as u8;
    [mix(fg[0], bg[0]), mix(fg[1], bg[1]), mix(fg[2], bg[2])]
}

/// `bold_is_bright`: the 8 normal ANSI colors (named, or indexed 0–7) of a bold
/// cell's foreground become their bright twins (8–15), like xterm's
/// `boldColors`; every other color is unchanged.
fn bright_for_bold(color: alacritty_terminal::vte::ansi::Color) -> alacritty_terminal::vte::ansi::Color {
    use alacritty_terminal::vte::ansi::{Color, NamedColor as N};
    match color {
        Color::Indexed(i) if i < 8 => Color::Indexed(i + 8),
        Color::Named(n) => Color::Named(match n {
            N::Black => N::BrightBlack,
            N::Red => N::BrightRed,
            N::Green => N::BrightGreen,
            N::Yellow => N::BrightYellow,
            N::Blue => N::BrightBlue,
            N::Magenta => N::BrightMagenta,
            N::Cyan => N::BrightCyan,
            N::White => N::BrightWhite,
            other => other,
        }),
        other => other,
    }
}

/// A one-entry [`resolve_rgb`] cache for one snapshot pass: neighbouring cells
/// overwhelmingly repeat the previous cell's fg (and bg), so the last color →
/// RGB pair answers most lookups with one compare.
struct ColorMemo<'a> {
    theme: &'a Theme,
    colors: &'a Colors,
    last: Option<(alacritty_terminal::vte::ansi::Color, [u8; 3])>,
}

impl<'a> ColorMemo<'a> {
    fn new(theme: &'a Theme, colors: &'a Colors) -> Self {
        ColorMemo { theme, colors, last: None }
    }

    #[inline]
    fn get(&mut self, color: alacritty_terminal::vte::ansi::Color) -> [u8; 3] {
        if let Some((c, rgb)) = self.last {
            if c == color {
                return rgb;
            }
        }
        let rgb = resolve_rgb(self.theme, self.colors, color);
        self.last = Some((color, rgb));
        rgb
    }
}

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

    /// The snapshot as it was built before the row-slice walk: every cell through
    /// `display_iter`, colors resolved per cell, a blank pre-fill. Kept as the
    /// reference `Terminal::snapshot` must reproduce exactly.
    fn reference_snapshot(t: &Terminal) -> GridSnapshot {
        let mut cells = vec![CellSnapshot::default(); t.cols * t.rows];
        let mut graphemes = Vec::new();
        let content = t.term.renderable_content();
        let display_offset = content.display_offset;
        // Dynamic OSC 4/10/11/12 palette overrides (pywal, base16 hooks, etc.)
        // are stored in the Term's color table; consult it so redefined colors
        // actually change on screen, falling back to the static theme.
        let colors = t.term.colors();
        // `minimum_contrast` memo, seeded with the default text/background pair
        // (most cells); `None` while the feature is off.
        let mut mc_memo = (t.min_contrast > 1.0).then(|| {
            let bg = [t.theme.bg[0], t.theme.bg[1], t.theme.bg[2]];
            crate::contrast::ContrastMemo::new(t.min_contrast, t.theme.fg, bg)
        });

        // Iterate over all visible cells. Each item has point in terminal coordinates
        // (line 0 = top of current viewport when display_offset=0; negative = history).
        // point_to_viewport converts to display row: viewport_line = point.line.0 + display_offset.
        for item in content.display_iter {
            if let Some(vp) = point_to_viewport(display_offset, item.point) {
                let row = vp.line;
                let col = vp.column.0;
                if row < t.rows && col < t.cols {
                    let cell = item.cell;
                    // `bold_is_bright`: a bold cell's normal ANSI foreground (0–7)
                    // takes its bright twin (8–15) before it is resolved.
                    let fg_color = if t.bold_is_bright && cell.flags.contains(Flags::BOLD) {
                        bright_for_bold(cell.fg)
                    } else {
                        cell.fg
                    };
                    let mut fg = resolve_rgb(&t.theme, colors, fg_color);
                    let mut bg = resolve_rgb(&t.theme, colors, cell.bg);
                    // Reverse video (`\e[7m`, also used by selections and `ls`
                    // highlights): swap fg/bg after resolving to RGB so the cell
                    // renders inverted once backgrounds are painted.
                    if cell.flags.contains(Flags::INVERSE) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    // SGR 2 (dim): alacritty sets Flags::DIM but leaves fg as a
                    // named color resolving to full brightness, so dim text would
                    // be indistinguishable from normal. Pull the foreground a third
                    // of the way toward the cell's background (`faint`) — less
                    // contrast on dark AND light themes. Done after INVERSE so the
                    // dimmed channel is whichever ends up fg.
                    if cell.flags.contains(Flags::DIM) {
                        fg = faint(fg, bg);
                    }
                    // SGR 8 (conceal): the glyph must not be readable (password
                    // echoes, secret-masking TUIs). Paint the foreground with the
                    // cell's background so the character is invisible while its
                    // background/layout are preserved. Done after INVERSE/DIM so it
                    // wins over whatever ended up as fg.
                    if cell.flags.contains(Flags::HIDDEN) {
                        fg = bg;
                    } else if let Some(memo) = mc_memo.as_mut() {
                        // `minimum_contrast` (off = one predictable branch): the
                        // FINAL fg is pushed to the ratio against the final bg —
                        // even a palette color that equals the background
                        // (solarized_dark's 8: invisible zsh autosuggestions).
                        // Concealed text (above) stays invisible; powerline, block
                        // and sextant glyphs keep their colors (they draw shapes).
                        // A blank draws no glyph: skipped (the common cell).
                        if cell.c != ' ' && !crate::contrast::min_contrast_exempt(cell.c) {
                            fg = memo.get(fg, bg);
                        }
                    }
                    // A double-width glyph occupies two grid cells: the WIDE_CHAR
                    // cell holds the actual char, and the following
                    // WIDE_CHAR_SPACER cell is a placeholder. alacritty stores a
                    // space (or stale char) in the spacer; the wide glyph from the
                    // preceding cell already visually spans both columns via the
                    // font, so we force the spacer to a blank to keep columns
                    // aligned (preserving the spacer's own bg).
                    // Combining marks / zero-width chars (NFD accents, VS16, ZWJ)
                    // live in the cell's `zerowidth()` extra storage, separate from
                    // `cell.c`. The per-cell `CellSnapshot` stays `Copy` (base char
                    // only); cells that carry marks are listed SPARSELY in
                    // `graphemes` (base + marks, capped) for the renderer to compose
                    // — an empty `Vec`, no allocation, on the common path.
                    let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
                    let c = if spacer { ' ' } else { cell.c };
                    // (`extra` also holds hyperlinks / underline colors: skip empty.)
                    if let Some(marks) = cell.zerowidth().filter(|m| !spacer && !m.is_empty()) {
                        graphemes.push(CellGrapheme { row, col, text: grapheme_text(cell.c, marks) });
                    }
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
                            uline = resolve_rgb(&t.theme, colors, c);
                        }
                    }
                    // WIDE_CHAR_SPACER: inherit the preceding base cell's attrs+uline
                    // so an underline/strike/bold spans the FULL width of a CJK glyph
                    // rather than only its left half. display_iter yields the base
                    // cell (col-1) before the spacer, so it is already stored. The
                    // spacer keeps its own bg (painted above) and blank char.
                    if flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 {
                        let base = &cells[row * t.cols + col - 1];
                        attrs = base.attrs;
                        uline = base.uline;
                    }
                    cells[row * t.cols + col] =
                        CellSnapshot { c, fg, bg, uline, attrs, selected: false };
                }
            }
        }

        // Mark selected cells. Compute the selection range once (in terminal
        // coordinates) and iterate over viewport rows to mark covered cells.
        let sel_range = t.term.selection.as_ref().and_then(|s| s.to_range(&t.term));
        if let Some(range) = sel_range {
            let grid = t.term.grid();
            let display_offset = grid.display_offset();
            for vp_row in 0..t.rows {
                let term_point = viewport_to_point(display_offset, Point::new(vp_row, Column(0)));
                let term_line = term_point.line;
                // Skip rows outside the selection's line range.
                if term_line < range.start.line || term_line > range.end.line {
                    continue;
                }
                let row = &grid[term_line];
                let base = vp_row * t.cols;
                for col in 0..t.cols {
                    let pt = Point::new(term_line, Column(col));
                    if range.contains(pt) {
                        cells[base + col].selected = true;
                        // A wide glyph spans its cell and the spacer after it:
                        // highlight both halves.
                        let flags = row[Column(col)].flags;
                        if flags.contains(Flags::WIDE_CHAR) && col + 1 < t.cols {
                            cells[base + col + 1].selected = true;
                        } else if flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 {
                            cells[base + col - 1].selected = true;
                        }
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
        let cursor_in_view = cursor_vp.map(|p| p.line < t.rows).unwrap_or(false);
        let (cursor_row, cursor_col) = cursor_vp
            .map(|p| (p.line.min(t.rows.saturating_sub(1)), p.column.0.min(t.cols.saturating_sub(1))))
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
        let grid = t.term.grid();
        let scroll_offset = grid.display_offset();
        let scroll_max = grid.history_size();

        // Honor OSC 11 (background) / OSC 12 (cursor) dynamic overrides, keeping
        // the theme's background alpha; fall back to the theme when unset.
        let bg_rgba = match colors[257] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b, t.theme.bg[3]],
            None => t.theme.bg,
        };
        let cursor_rgb = match colors[258] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => t.theme.cursor,
        };

        GridSnapshot {
            cols: t.cols,
            rows: t.rows,
            cells,
            cursor_row,
            cursor_col,
            cursor_visible,
            bg_rgba,
            cursor_rgb,
            scroll_offset,
            scroll_max,
            cursor_shape,
            graphemes,
        }
    }

    /// `snapshot` (row slices, memoized colors, the plain-cell fast path) must be
    /// field-for-field the reference snapshot on every screen: random text (wide,
    /// combining, emoji, box/braille), every SGR attribute and color form,
    /// underline colors, cursor moves and erases, the alternate screen, palette
    /// and OSC 10/11/12 overrides, resizes, scrollback views, selections, themes,
    /// `bold_is_bright` and `minimum_contrast`.
    #[test]
    fn snapshot_matches_the_display_iter_reference_on_random_screens() {
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut rnd = move |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        const TEXT: &[&str] = &[
            "a", "Z", "0", " ", " ", "-", "~", "漢", "字", "テ", "👍", "😀", "e\u{301}", "a\u{308}\u{301}",
            "❤\u{fe0f}", "👩\u{200d}💻", "─", "│", "╭", "⠿", "█", "▄", "\t", "\u{e0b0}",
        ];
        const SGR: &[&str] = &[
            "0", "1", "2", "3", "4", "4:2", "4:3", "4:4", "4:5", "7", "8", "9", "21", "22", "23", "24", "27", "28",
            "29", "31", "37", "39", "42", "47", "49", "92", "105", "38;5;208", "48;5;17", "38;2;10;200;30",
            "48;2;250;250;250", "58;5;196", "58;2;0;255;255", "59", "1;4;7", "2;8",
        ];
        let mut checked = 0;
        for case in 0..160 {
            let (mut cols, mut rows) = (2 + rnd(70) as usize, 1 + rnd(24) as usize);
            let mut t = Terminal::new(cols, rows);
            if case % 3 == 0 {
                t.set_theme(crate::theme::theme_at(rnd(crate::theme::theme_count() as u64) as usize));
            }
            for _ in 0..40 {
                let mut out = String::new();
                for _ in 0..rnd(40) {
                    match rnd(12) {
                        0..=4 => out.push_str(TEXT[rnd(TEXT.len() as u64) as usize]),
                        5 | 6 => out.push_str(&format!("\x1b[{}m", SGR[rnd(SGR.len() as u64) as usize])),
                        7 => out.push_str("\r\n"),
                        8 => out.push_str(&format!("\x1b[{};{}H", 1 + rnd(rows as u64 + 2), 1 + rnd(cols as u64 + 2))),
                        9 => out.push_str(["\x1b[K", "\x1b[1K", "\x1b[J", "\x1b[2L", "\x1b[3P", "\x1b[2@"][rnd(6) as usize]),
                        10 => out.push_str(
                            [
                                "\x1b[?1049h", "\x1b[?1049l", "\x1b[?25l", "\x1b[?25h", "\x1b[5 q", "\x1b[3 q",
                                "\x1b]4;1;rgb:ff/00/80\x07", "\x1b]10;rgb:10/20/30\x07", "\x1b]11;rgb:f0/f0/e0\x07",
                                "\x1b]12;rgb:00/ff/00\x07", "\x1b]104\x07", "\x1b]110\x07", "\x1b]111\x07",
                            ][rnd(13) as usize],
                        ),
                        _ => out.push_str(&"x".repeat(rnd(cols as u64 * 2) as usize)),
                    }
                }
                t.feed(out.as_bytes());
                match rnd(10) {
                    0 => {
                        cols = 2 + rnd(70) as usize;
                        rows = 1 + rnd(24) as usize;
                        t.resize(cols, rows);
                    }
                    1 => t.scroll_lines(rnd(40) as i32 - 10),
                    2 => {
                        t.selection_start(rnd(rows as u64) as usize, rnd(cols as u64) as usize, rnd(2) == 0);
                        t.selection_update(rnd(rows as u64) as usize, rnd(cols as u64) as usize, rnd(2) == 0);
                    }
                    3 => t.selection_clear(),
                    4 => t.set_bold_is_bright(rnd(2) == 0),
                    5 => t.set_minimum_contrast(if rnd(2) == 0 { 1.0 } else { 4.5 }),
                    _ => {}
                }
                let (got, want) = (t.snapshot(), reference_snapshot(&t));
                let ctx = format!("case {case}, {cols}x{rows}");
                assert_eq!((got.cols, got.rows), (want.cols, want.rows), "{ctx}");
                for (i, (g, w)) in got.cells.iter().zip(&want.cells).enumerate() {
                    assert_eq!(g, w, "{ctx}: cell row {} col {}", i / got.cols, i % got.cols);
                }
                assert_eq!(got.cells.len(), want.cells.len(), "{ctx}");
                assert_eq!(got.graphemes, want.graphemes, "{ctx}");
                assert_eq!(
                    (got.cursor_row, got.cursor_col, got.cursor_visible, got.cursor_shape),
                    (want.cursor_row, want.cursor_col, want.cursor_visible, want.cursor_shape),
                    "{ctx}"
                );
                assert_eq!(t.cursor_viewport_cell(), (got.cursor_row, got.cursor_col), "{ctx}: cursor_viewport_cell");
                assert_eq!((got.bg_rgba, got.cursor_rgb), (want.bg_rgba, want.cursor_rgb), "{ctx}");
                assert_eq!((got.scroll_offset, got.scroll_max), (want.scroll_offset, want.scroll_max), "{ctx}");
                checked += 1;
            }
        }
        assert_eq!(checked, 160 * 40);
    }

    /// `prune_marks`' O(pruned) fast path must keep EXACTLY the marks a full
    /// rescan keeps, in the same order — under mostly-ascending binds with
    /// occasional upward (out-of-order) prompts, which force the rescan path and
    /// must hand back to the fast path once the order is restored.
    #[test]
    fn prune_marks_fast_path_matches_a_full_rescan() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut rnd = move |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        let block = |prompt: i64| CmdBlock {
            prompt,
            input: None,
            input_col: 0,
            output: None,
            exit: None,
            finished: true,
            redraws: true,
        };
        let mut fast_path_hits = 0;
        for _ in 0..300 {
            let mut t = Terminal::new(80, 24);
            let mut reference: Vec<i64> = Vec::new();
            let mut line = 0i64;
            for _ in 0..rnd(80) {
                line += if rnd(12) == 0 { -(rnd(40) as i64) } else { rnd(6) as i64 };
                t.push_mark(block(line));
                reference.push(line);
                // Prune under a random live window around the newest prompt.
                t.abs_top = line - rnd(24) as i64;
                let history = rnd(60) as usize;
                if t.marks_sorted {
                    fast_path_hits += 1;
                }
                t.prune_marks(history);
                let (min_abs, max_abs) = (t.abs_top - history as i64, t.abs_top + t.rows as i64);
                reference.retain(|&p| p >= min_abs && p < max_abs);
                let got: Vec<i64> = t.marks.iter().map(|m| m.prompt).collect();
                assert_eq!(got, reference);
                // The flag may be pessimistic, never optimistic.
                if t.marks_sorted {
                    assert!(got.windows(2).all(|w| w[0] <= w[1]), "marks_sorted lied: {got:?}");
                }
            }
        }
        assert!(fast_path_hits > 1000, "the fast path must be the common case ({fast_path_hits})");
    }

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
    fn faint_mixes_toward_the_background() {
        // The math: 66% of the way from bg to fg, per channel, rounded.
        assert_eq!(faint([255, 255, 255], [0, 0, 0]), [168, 168, 168]);
        assert_eq!(faint([0, 0, 0], [255, 255, 255]), [87, 87, 87]);
        assert_eq!(faint([200, 100, 50], [200, 100, 50]), [200, 100, 50], "fg == bg stays put");
        // On a LIGHT theme faint text gets lighter (less contrast), not darker.
        let mut t = Terminal::new(20, 5);
        t.set_theme(crate::theme::solarized_light());
        t.feed(b"A\x1b[2mB\x1b[0m");
        let snap = t.snapshot();
        let (normal, dim, bg) = (snap.cell(0, 0).fg, snap.cell(0, 1).fg, snap.cell(0, 1).bg);
        let dist = |a: [u8; 3], b: [u8; 3]| (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum::<i32>();
        assert!(dist(dim, bg) < dist(normal, bg), "faint {dim:?} is closer to bg {bg:?} than {normal:?}");
        // Faint after reverse video dims whichever color ended up in front.
        t.feed(b"\r\n\x1b[2;7mC\x1b[0m");
        let c = *t.snapshot().cell(1, 0);
        assert_eq!(c.fg, faint(normal_bg_of(&t), c.bg));
    }

    /// The default (theme) background as a cell resolves it.
    fn normal_bg_of(t: &Terminal) -> [u8; 3] {
        let bg = t.theme().bg;
        [bg[0], bg[1], bg[2]]
    }

    #[test]
    fn bold_is_bright_maps_the_normal_ansi_colors() {
        let mut t = Terminal::new(40, 5);
        let theme = t.theme().clone();
        // Bold red (SGR 31 named), bold 256-color 2, bold 256-color 9 (already
        // bright), bold truecolor, and a plain (non-bold) red.
        let line = b"\x1b[1;31mA\x1b[0m\x1b[1;38;5;2mB\x1b[0m\x1b[1;38;5;9mC\x1b[0m\x1b[1;38;2;1;2;3mD\x1b[0m\x1b[31mE\x1b[0m";
        t.feed(line);
        let off = t.snapshot();
        assert_eq!(off.cell(0, 0).fg, theme.palette[1], "off by default: bold red stays red");
        t.set_bold_is_bright(true);
        let on = t.snapshot();
        assert_eq!(on.cell(0, 0).fg, theme.palette[9], "bold red → bright red");
        assert_eq!(on.cell(0, 1).fg, theme.palette[10], "bold 256-color 2 → 10");
        assert_eq!(on.cell(0, 2).fg, theme.palette[9], "already bright: unchanged");
        assert_eq!(on.cell(0, 3).fg, [1, 2, 3], "truecolor: unchanged");
        assert_eq!(on.cell(0, 4).fg, theme.palette[1], "not bold: unchanged");
        // The default foreground is not one of the 8 colors.
        t.feed(b"\r\n\x1b[1mF\x1b[0m");
        assert_eq!(t.snapshot().cell(1, 0).fg, theme.fg);
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
    fn default_cursor_shape_is_what_programs_reset_to() {
        let mut t = Terminal::new(20, 5);
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Block);
        // The user's `[cursor] shape` shows on a fresh screen…
        t.set_default_cursor_shape(CursorShapeSnap::Beam);
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Beam);
        // …a program's DECSCUSR still wins…
        t.feed(b"\x1b[2 q");
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Block);
        // …until it resets (CSI 0 SP q), which lands on the user's shape.
        t.feed(b"\x1b[0 q");
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Beam);
        // Survives the other Config rebuilds.
        t.set_scrollback_lines(500);
        t.set_kitty_keyboard(true);
        t.set_osc52_allow_paste(true);
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Beam);
        t.set_default_cursor_shape(CursorShapeSnap::Underline);
        assert_eq!(t.snapshot().cursor_shape, CursorShapeSnap::Underline);
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
    fn resize_at_clean_prompt_wipes_instead_of_scattering() {
        // p10k-scatter fix: an EMPTY tab (a lone OSC-133 prompt, no command ever
        // run) must have its grid wiped on resize so alacritty's reflow leaves no
        // scattered/stacked prompt fragments. Shrink then grow — the classic
        // scatter gesture — must end with a CLEAN grid (the real shell would then
        // repaint one prompt via SIGWINCH; there is no shell here, so a blank grid
        // proves zero fragments survived the reflow).
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07"); // prompt start (idle at prompt, no command)
        // A full-width, right-segmented prompt like p10k's (what actually scatters).
        let mut prompt = String::from("\u{276f} home ~ ");
        prompt.push_str(&" ".repeat(60));
        prompt.push('\u{2713}'); // right-aligned check glyph near the edge
        t.feed(prompt.as_bytes());
        t.resize(40, 12); // shrink (rewraps / stuffs the prompt into history)
        t.resize(100, 30); // grow (would reveal the stray copies)
        let snap = t.snapshot();
        let prompts = (0..snap.rows).filter(|&r| snap.row_text(r).contains('\u{276f}')).count();
        assert_eq!(
            prompts, 0,
            "a clean-prompt resize must wipe the grid so no prompt fragment survives; found {prompts}"
        );
    }

    #[test]
    fn resize_preserves_real_command_output() {
        // The clean-prompt clear MUST be content-safe: once a command has produced
        // output (OSC-133 C), a resize must NOT wipe it — it takes the plain reflow.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07"); // prompt
        t.feed(b"\x1b]133;C\x07"); // command output starts -> saw_command_output latched
        t.feed(b"IMPORTANT_OUTPUT_XYZ\r\n"); // real output that must survive
        t.feed(b"\x1b]133;A\x07"); // next prompt (idle again, but output exists)
        t.resize(40, 12);
        t.resize(100, 30);
        let snap = t.snapshot();
        let found = (0..snap.rows).any(|r| snap.row_text(r).contains("IMPORTANT_OUTPUT_XYZ"));
        assert!(found, "a tab that produced output must never be cleared on resize");
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
    fn link_at_flags_an_osc8_target_its_text_does_not_show() {
        // An OSC 8 link's text can claim any address: `cat` of a crafted file
        // shows "https://github.com" and a Ctrl+click opens something else.
        let mut t = Terminal::new(60, 5);
        t.feed(
            b"\x1b]8;;https://evil.example/x\x1b\\https://github.com\x1b]8;;\x1b\\ \
              \x1b]8;;https://a.io/\x1b\\https://a.io/\x1b]8;;\x1b\\ https://plain.io/x",
        );
        let h = t.link_at(0, 2).expect("the disguised link");
        assert_eq!(h.uri, "https://evil.example/x");
        assert!(h.hidden_target, "its text names another address");
        let h = t.link_at(0, 20).expect("an honest OSC 8 link");
        assert_eq!(h.uri, "https://a.io/");
        assert!(!h.hidden_target, "its text IS its target");
        let h = t.link_at(0, 36).expect("a plain-text URL");
        assert!(!h.hidden_target, "plain text is always its own target");
        // `ls --hyperlink`: the file name links to a file:// URI.
        t.feed(b"\r\n\x1b]8;;file://host/home/u/Cargo.toml\x1b\\Cargo.toml\x1b]8;;\x1b\\");
        assert!(t.link_at(1, 3).expect("file link").hidden_target);
    }

    #[test]
    fn double_click_on_a_url_selects_the_whole_url() {
        // `:` is a word separator, so a double-click on a URL selected
        // `//example.com/a/b?c=1` (or just `https`) — never a usable URL.
        let url = "https://example.com/a/b?c=1"; // cols 4..=30
        let mut t = Terminal::new(60, 5);
        t.feed(format!("see {url} now, foo:bar").as_bytes());
        for col in [4, 6, 10, 30] {
            t.selection_start_semantic(0, col);
            assert_eq!(t.selection_text().as_deref(), Some(url), "double-click at col {col}");
        }
        // Pointer jitter between the clicks (motion inside the URL) keeps it.
        t.selection_start_semantic(0, 8);
        t.selection_update(0, 9, true);
        t.selection_update(0, 5, false);
        assert_eq!(t.selection_text().as_deref(), Some(url));
        // Dragging past it extends word by word, the URL kept whole.
        t.selection_update(0, 33, true); // in "now,"
        assert_eq!(t.selection_text().as_deref(), Some(&*format!("{url} now")));
        t.selection_update(0, 1, true); // back left, in "see"
        assert_eq!(t.selection_text().as_deref(), Some(&*format!("see {url}")));
        // Other words keep the usual separators: `:` still splits `foo:bar`.
        t.selection_start_semantic(0, 38);
        assert_eq!(t.selection_text().as_deref(), Some("foo"));
        t.selection_update(0, 39, false);
        assert_eq!(t.selection_text().as_deref(), Some("foo"));
    }

    #[test]
    fn double_click_on_a_wrapped_url_selects_all_of_it() {
        let url = "https://example.com/some/long/path/x";
        let mut t = Terminal::new(20, 5);
        t.feed(format!("go {url} ok").as_bytes());
        t.selection_start_semantic(1, 5); // the wrapped continuation row
        assert_eq!(t.selection_text().as_deref(), Some(url));
        // Output scrolls it into history mid-drag: the pinned URL moves with
        // its text, so the pointer over it (two rows up now) still means it.
        t.feed(b"\r\nmore\r\nlines\r\n\r\n");
        t.scroll_lines(2);
        assert!(t.snapshot().row_text(1).starts_with("om/some"));
        t.selection_update(1, 5, true);
        assert_eq!(t.selection_text().as_deref(), Some(url));
        // Past its end (onto "ok") it extends by words.
        t.selection_update(2, 1, false);
        assert_eq!(t.selection_text().as_deref(), Some(&*format!("{url} ok")));
    }

    #[test]
    fn selection_highlights_both_cells_of_a_wide_char() {
        // A wide glyph spans its cell and the spacer after it. The copied text
        // takes the whole char whenever either cell is selected, so the
        // highlight must cover both — it lit only the selected half.
        let mut t = Terminal::new(20, 3);
        t.feed("ab世界cd".as_bytes()); // 世 = cols 2–3, 界 = cols 4–5
        let selected = |t: &Terminal| -> Vec<usize> {
            let snap = t.snapshot();
            (0..8).filter(|&c| snap.cell(0, c).selected).collect()
        };
        // Ending ON 世 (a drag ending over its left half, copy-mode `e`).
        t.selection_start(0, 0, true);
        t.selection_update(0, 2, false);
        assert_eq!(t.selection_text().as_deref(), Some("ab世"));
        assert_eq!(selected(&t), vec![0, 1, 2, 3], "both halves of 世");
        // Starting on 世's spacer (a drag from its right half, leftward end).
        t.selection_start(0, 3, true);
        t.selection_update(0, 5, false);
        assert_eq!(t.selection_text().as_deref(), Some("世界"));
        assert_eq!(selected(&t), vec![2, 3, 4, 5], "both halves of 世 and 界");
        // A selection of only narrow cells is untouched.
        t.selection_start(0, 6, true);
        t.selection_update(0, 7, false);
        assert_eq!(selected(&t), vec![6, 7]);
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
    fn the_title_stack_holds_clipped_titles() {
        // alacritty kept an OSC 0/2 title whole (up to the 1 MiB OSC cap) and
        // cloned it on every `CSI 22 t`, 4096 deep: ~1 MB of output pinned
        // ~4 GiB per tab. Titles are clipped on the way in (JeTTY shows 256
        // chars), so the stack holds at most 4096 × 1 KiB.
        let mut t = Terminal::new(20, 5);
        t.feed(format!("\x1b]2;{}\x07", "中".repeat(5462)).as_bytes()); // 16 KiB
        t.feed(&b"\x1b[22t".repeat(4096));
        t.feed(b"\x1b]2;short\x07");
        for _ in 0..3 {
            t.feed(b"\x1b[23t");
            let popped = t.title_update.lock().unwrap().clone().flatten().unwrap();
            assert!(popped.len() <= crate::handler::TITLE_MAX_BYTES, "a {}-byte title", popped.len());
            assert_eq!(popped, "中".repeat(341), "clipped on a char boundary");
        }
        assert_eq!(t.take_title_update(), Some(Some("中".repeat(256))), "the title shown is the same");
    }

    // ── OSC 9;4 progress ──────────────────────────────────────────────────────

    fn prog(state: ProgressState, value: Option<u8>) -> Option<Progress> {
        Some(Progress { state, value })
    }

    /// Feed `seq` split at EVERY byte boundary (and whole) into a fresh terminal
    /// that already shows `before`, returning the progress each run ends with.
    fn progress_after(before: &[u8], seq: &[u8]) -> Vec<Option<Progress>> {
        (0..=seq.len())
            .map(|cut| {
                let mut t = Terminal::new(30, 6);
                t.feed(before);
                t.feed(&seq[..cut]);
                t.feed(&seq[cut..]);
                t.progress()
            })
            .collect()
    }

    #[test]
    fn osc94_every_state_parses_at_any_split() {
        use ProgressState::*;
        let cases: [(&[u8], &[u8], Option<Progress>); 12] = [
            (b"", b"\x1b]9;4;1;40\x07", prog(Normal, Some(40))),
            (b"", b"\x1b]9;4;1;40\x1b\\", prog(Normal, Some(40))), // ST terminator
            (b"", b"\x1b]9;4;1;250\x07", prog(Normal, Some(100))), // clamped
            (b"", b"\x1b]9;4;1\x07", prog(Normal, Some(0))),       // no value = 0
            (b"", b"\x1b]9;4;2;7\x07", prog(Error, Some(7))),
            (b"\x1b]9;4;1;55\x07", b"\x1b]9;4;2\x07", prog(Error, Some(55))), // keeps value
            (b"", b"\x1b]9;4;2\x07", prog(Error, None)),
            (b"", b"\x1b]9;4;3;\x07", prog(Indeterminate, None)),
            (b"", b"\x1b]9;4;3;80\x07", prog(Indeterminate, None)),
            (b"\x1b]9;4;1;30\x07", b"\x1b]9;4;4\x07", prog(Paused, Some(30))),
            (b"\x1b]9;4;1;30\x07", b"\x1b]9;4;0;0\x07", None), // clear
            (b"\x1b]9;4;1;30\x07", b"\x1b]9;4;\x07", None),    // empty state = clear
        ];
        for (before, seq, want) in cases {
            for got in progress_after(before, seq) {
                assert_eq!(got, want, "{:?} then {:?}", String::from_utf8_lossy(before), String::from_utf8_lossy(seq));
            }
        }
    }

    #[test]
    fn osc94_malformed_and_lookalikes_change_nothing() {
        let before: &[u8] = b"\x1b]9;4;1;20\x07";
        let kept = prog(ProgressState::Normal, Some(20));
        for seq in [
            &b"\x1b]9;hello\x07"[..],      // iTerm2 notification, not progress
            b"\x1b]9;4\x07",               // prefix never completed
            b"\x1b]9;4;x;5\x07",           // non-digit state
            b"\x1b]9;4;1;5a\x07",          // non-digit value
            b"\x1b]9;4;1;1000\x07",        // too many digits
            b"\x1b]9;4;9;50\x07",          // unknown state
            b"\x1b]99;4;1;50\x07",         // OSC 99, not 9
            b"\x1b]19;4;1;50\x07",
            b"\x1b]2;9;4;1;50\x07",        // a title that merely contains it
        ] {
            for got in progress_after(before, seq) {
                assert_eq!(got, kept, "{:?}", String::from_utf8_lossy(seq));
            }
        }
        // Extra parameters after `pr` are ignored, not malformed.
        for got in progress_after(b"", b"\x1b]9;4;1;60;junk;more\x07") {
            assert_eq!(got, prog(ProgressState::Normal, Some(60)));
        }
        // C0 controls inside the OSC are ignored by vte, and so here.
        for got in progress_after(b"", b"\x1b]9;4;1;\n6\r5\x07") {
            assert_eq!(got, prog(ProgressState::Normal, Some(65)));
        }
    }

    #[test]
    fn osc94_cleared_by_prompt_marks_and_ris() {
        let set: &[u8] = b"\x1b]9;4;1;40\x07";
        // A new prompt (A) and a finished command (D) both clear it…
        for clear in [&b"\x1b]133;A\x07"[..], b"\x1b]133;D;0\x07", b"\x1bc"] {
            let mut t = Terminal::new(30, 6);
            t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
            t.feed(set);
            assert!(t.progress().is_some());
            let _ = t.take_progress_update();
            t.feed(clear);
            assert_eq!(t.progress(), None, "{:?} must clear", String::from_utf8_lossy(clear));
            assert_eq!(t.take_progress_update(), Some(None), "the clear is reported");
        }
        // …but a command START (C) does not: cargo reports right after it.
        let mut t = Terminal::new(30, 6);
        t.feed(b"\x1b]133;A\x07");
        t.feed(set);
        t.feed(b"\x1b]133;C\x07");
        assert_eq!(t.progress(), prog(ProgressState::Normal, Some(40)));
    }

    #[test]
    fn osc94_update_is_reported_once_and_only_on_change() {
        let mut t = Terminal::new(30, 6);
        assert_eq!(t.take_progress_update(), None, "nothing pending initially");
        t.feed(b"\x1b]9;4;1;10\x07\x1b]9;4;1;20\x07");
        assert_eq!(t.take_progress_update(), Some(prog(ProgressState::Normal, Some(20))), "coalesced");
        assert_eq!(t.take_progress_update(), None, "consumed");
        t.feed(b"\x1b]9;4;1;20\x07");
        assert_eq!(t.take_progress_update(), None, "an identical report is no change");
        // Clearing an absent progress (a prompt with nothing showing) is silent.
        let mut u = Terminal::new(30, 6);
        u.feed(b"\x1b]133;A\x07\x1b]9;4;0\x07\x1bc");
        assert_eq!(u.take_progress_update(), None);
    }

    #[test]
    fn osc94_bytes_still_reach_vte_and_leave_the_grid_alone() {
        // The progress OSC is consumed by vte like any other OSC: nothing leaks
        // onto the grid, text around it lands where it should, and a following
        // OSC 133 still binds.
        let mut t = Terminal::new(30, 6);
        t.feed(b"ab\x1b]9;4;1;40\x07cd");
        let snap = t.snapshot();
        let row: String = (0..4).map(|c| snap.cell(0, c).c).collect();
        assert_eq!(row, "abcd");
        t.feed(b"\x1b]9;4;3;\x1b\\\x1b]133;A\x07");
        assert_eq!(t.prompt_count(), 1, "the ST-terminated progress left the scanner in sync");
    }

    #[test]
    fn osc94_flood_is_capped_like_every_osc() {
        // A never-terminated progress payload of ignored C0 bytes is ended at
        // the OSC cap (vte never buffers more), then discarded to its terminator.
        let mut t = Terminal::new(30, 6);
        t.osc_cap = 64;
        t.feed(b"\x1b]9;4;1;");
        t.feed(&[b'\n'; 200]);
        assert!(matches!(t.scan, Scan::OscDiscard), "scan = {:?}", t.scan);
        t.feed(b"5\x07xy");
        assert_eq!(t.progress(), None, "a capped OSC applies nothing");
        let snap = t.snapshot();
        assert_eq!(snap.cell(0, 0).c, 'x', "the stream resyncs after the terminator");
    }

    #[test]
    fn command_running_and_marks_track_osc133() {
        let mut t = Terminal::new(30, 6);
        assert!(!t.command_running() && !t.take_command_marks());
        t.feed(b"\x1b]133;A\x07");
        assert!(!t.command_running());
        assert!(t.take_command_marks(), "A is a mark");
        assert!(!t.take_command_marks(), "consumed");
        t.feed(b"\x1b]133;B\x07");
        assert!(!t.take_command_marks(), "B is not a start/finish");
        t.feed(b"\x1b]133;C\x07");
        assert!(t.command_running() && t.take_command_marks());
        t.feed(b"out\r\n\x1b]133;D;1\x07");
        assert!(!t.command_running() && t.take_command_marks());
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

    /// `text` inside an OSC 8 link with these params and URI.
    fn osc8(params: &str, uri: &str, text: &str) -> String {
        format!("\x1b]8;{params};{uri}\x1b\\{text}\x1b]8;;\x1b\\")
    }

    #[test]
    fn link_at_joins_cells_of_the_same_id_and_uri() {
        // The OSC 8 spec joins cells with the same id AND URI. JeTTY joined by
        // id alone, comparing the id string for every viewport cell: with a
        // ~1 MiB id each recompute of a Ctrl+hover (every mouse move, every
        // drain while hovered) cost ~60 ms. Links compare as values now — one
        // pointer compare for the cells of one OSC run.
        let mut t = Terminal::new(30, 3);
        let one = osc8("id=a", "https://one.test/", "one");
        let two = osc8("id=a", "https://two.test/", "two");
        t.feed(format!("{one} {two} {}", osc8("id=a", "https://one.test/", "uno")).as_bytes());
        let hit = t.link_at(0, 0).unwrap();
        assert_eq!(hit.spans, vec![(0, 0, 2), (0, 8, 10)], "both runs of one.test, not two.test");
        assert_eq!(t.link_at(0, 5).unwrap().spans, vec![(0, 4, 6)]);
    }

    #[test]
    fn overlong_osc8_ids_and_uris_are_dropped_as_vte_does() {
        // Past VTE's limits an `id` is ignored (each run is a link of its own)
        // and a URI makes no link, which also bounds every compare above.
        let mut t = Terminal::new(30, 3);
        let id = format!("id={}", "i".repeat(251));
        t.feed(format!("{} {}", osc8(&id, "https://x.test/", "aa"), osc8(&id, "https://x.test/", "bb")).as_bytes());
        assert_eq!(t.link_at(0, 0).unwrap().spans, vec![(0, 0, 1)], "an overlong id joins nothing");
        let uri = format!("https://x.test/{}", "u".repeat(8 * 1024));
        t.feed(format!("\r\n{}", osc8("", &uri, "cc")).as_bytes());
        assert!(t.link_at(1, 0).is_none(), "an overlong URI is no link");
        t.feed(format!("\r\n{}", osc8("id=ok", &uri[..8 * 1024], "dd")).as_bytes());
        assert_eq!(t.link_at(2, 0).unwrap().uri.len(), 8 * 1024, "8 KiB is still a link");
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
    fn search_for_a_wide_char_at_the_top_of_the_buffer_finishes() {
        // The match scan runs leftward and resumes one cell left of each match;
        // past a wide char in the buffer's top-left corner alacritty wrapped
        // around to the bottom and found every match again, forever: searching
        // 中 in a tab whose first line starts with it froze JeTTY. Run in a
        // thread so a regression fails here instead of hanging the suite.
        let cases: [(&str, usize, usize, &str, usize); 3] = [
            ("中文 notes\r\n$ ls 中\r\n", 80, 24, "中", 2),
            ("中文", 5, 1, "中", 1),
            ("😀 hi 😀 x\r\n", 20, 3, "😀", 2),
        ];
        for (text, cols, rows, query, want) in cases {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut t = Terminal::new(cols, rows);
                t.set_scrollback_lines(if rows == 1 { 0 } else { 100 });
                t.feed(text.as_bytes());
                let _ = tx.send(t.search_set_query(query).1);
            });
            let total = rx.recv_timeout(std::time::Duration::from_secs(20));
            assert_eq!(total, Ok(want), "{text:?} / {query:?}: the search never finished");
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
    fn search_cap_keeps_the_most_recent_matches() {
        // More matches than SEARCH_MAX_MATCHES: the cap must drop the OLDEST
        // ones. Collected top-down, it kept the 5000 oldest — the newest
        // output (the visible screen, where a search starts) had no matches,
        // and typing the query jumped the view ~1000 lines up into history.
        let mut t = Terminal::new(40, 10);
        let n = SEARCH_MAX_MATCHES + 1000;
        for i in 0..n {
            t.feed(format!("foo {i}\r\n").as_bytes());
        }
        let (cur, total) = t.search_set_query("foo");
        assert_eq!(total, SEARCH_MAX_MATCHES);
        assert_eq!(cur, total, "the current match is the newest one");
        assert_eq!(t.scroll_offset(), 0, "a match on screen: the view must not move");
        let snap = t.snapshot();
        let last_row = (0..t.rows()).rev().find(|&r| !snap.row_text(r).trim().is_empty()).unwrap();
        assert_eq!(snap.row_text(last_row).trim_end(), format!("foo {}", n - 1));
        let hits = t.search_viewport_hits();
        assert_eq!(hits.len(), 9, "every visible match is highlighted: {hits:?}");
        assert!(hits.iter().any(|h| h.is_current && h.row == last_row));
        // Navigating back from the oldest kept match wraps to the newest.
        let first_kept = format!("foo {}", n - SEARCH_MAX_MATCHES);
        t.search_nav(false);
        assert_eq!(t.search_counter(), (1, total));
        let snap = t.snapshot();
        assert!(
            (0..t.rows()).any(|r| snap.row_text(r).trim_end() == first_kept),
            "the oldest kept match is {first_kept:?}"
        );
    }

    #[test]
    fn search_matches_of_a_self_overlapping_query_stay_disjoint() {
        // `==` in `====` is two matches, never three overlapping ones.
        let mut t = Terminal::new(40, 5);
        t.feed(b"==== x aaa");
        assert_eq!(t.search_set_query("==").1, 2);
        let cols: Vec<(usize, usize)> =
            t.search_viewport_hits().iter().map(|h| (h.col_start, h.col_end)).collect();
        assert_eq!(cols, vec![(0, 1), (2, 3)]);
        assert_eq!(t.search_set_query("aa").1, 1);
    }

    #[test]
    fn search_reads_past_a_wide_char_ending_a_row() {
        // `a中` filling a 3-column row right before a hard line break: alacritty
        // finds it leftward but cannot confirm it, and its one scan of the whole
        // buffer ended right there — every older match went unfound.
        let mut t = Terminal::new(3, 4);
        t.feed("xa中b\r\n\r\na中\r\nz".as_bytes());
        assert_eq!(t.search_set_query("a中").1, 1, "the wrapped one, above the row it cannot confirm");
        let hits = t.search_viewport_hits();
        assert_eq!(hits.len(), 2, "one match over two rows: {hits:?}");
        assert_eq!((hits[0].row, hits[0].col_start), (0, 1));
    }

    #[test]
    fn search_current_match_stays_on_its_text_while_output_scrolls() {
        // 20×5: "err A" in history, "err B" on screen. Enter (older) makes A
        // current and scrolls to it; three lines of output must leave A
        // current. The refresh used to look it up by its pre-scroll grid
        // point and land on B — the next Enter then re-selected A without
        // moving, as if it did nothing.
        let mut t = Terminal::new(20, 5);
        t.feed(b"err A\r\n1\r\n2\r\n3\r\n4\r\n5\r\nerr B\r\n");
        assert_eq!(t.search_set_query("err"), (2, 2));
        assert_eq!(t.search_nav(true), (1, 2));
        let offset = t.scroll_offset();
        assert!(offset > 0, "A is in history: the view moved to it");
        t.feed(b"x\r\ny\r\nz\r\n");
        t.search_refresh();
        assert_eq!(t.search_counter(), (1, 2), "A is still the current match");
        assert_eq!(t.scroll_offset(), offset + 3, "the view stays on A");
        let hits = t.search_viewport_hits();
        assert!(hits.iter().any(|h| h.is_current), "A is lit as current where it is: {hits:?}");
        // New matches below keep the old one current, and the counter honest.
        t.feed(b"err C\r\nerr D\r\n");
        t.search_refresh();
        assert_eq!(t.search_counter(), (1, 4));
        assert_eq!(t.search_nav(false), (2, 4), "Shift+Enter steps to the next newer one, B");
    }

    #[test]
    fn search_current_match_survives_a_reflow() {
        let mut t = Terminal::new(30, 6);
        for i in 0..40 {
            t.feed(format!("line {i} needle\r\n").as_bytes());
        }
        let (_, total) = t.search_set_query("needle");
        for _ in 0..5 {
            t.search_nav(true);
        }
        let (cur, _) = t.search_counter();
        t.resize(20, 9);
        assert_eq!(t.search_counter(), (cur, total), "the same match, counted from the bottom");
    }

    /// The matches a full re-collect finds on `t`'s grid right now; the
    /// incremental state is left as it was.
    fn full_search_matches(t: &mut Terminal) -> Vec<SearchMatch> {
        let (kept, scan) = (t.search_matches.clone(), t.search_scan);
        t.search_scan = None;
        t.search_collect();
        t.search_scan = scan;
        std::mem::replace(&mut t.search_matches, kept)
    }

    /// Output that keeps matching the queries of [`search_equivalence`]: runs
    /// of `a` / `b` / `=`, wide `中`, and line breaks — or a sequence that
    /// rewrites what is on the screen or moves it into history.
    fn search_fuzz_token(r: &mut Rng, out: &mut Vec<u8>) {
        match r.below(12) {
            0..=6 => {
                for _ in 0..1 + r.below(30) {
                    let piece: &[u8] = r.pick(&[&b"a"[..], b"a", b"b", b"=", b" ", "中".as_bytes(), b"\r\n", b"ab"]);
                    out.extend_from_slice(piece);
                }
            }
            7 => out.extend_from_slice(format!("\x1b[{};{}H", 1 + r.below(14), 1 + r.below(34)).as_bytes()),
            8 => out.extend_from_slice(r.pick(&[
                &b"\x1b[K"[..], b"\x1b[1K", b"\x1b[J", b"\x1b[2J", b"\x1b[3J", b"\x1b[2L", b"\x1b[M",
                b"\x1b[3S", b"\x1b[T", b"\x1bM", b"\x1b[3@", b"\x1b[2P", b"\x1b#8",
            ])),
            9 => out.extend_from_slice(format!("\x1b[{};{}r", r.below(8), r.below(16)).as_bytes()),
            10 => out.extend_from_slice(r.pick(&[
                &b"\x1b[?1049h"[..], b"\x1b[?1049l", b"\x1b[?2026h", b"\x1b[?2026l", b"\x1bc",
            ])),
            _ => fuzz_token(r, out),
        }
    }

    /// Drive one terminal with a search open through `steps` random outputs
    /// and app calls, checking after every refresh that the incremental match
    /// list equals a full re-collect. Returns how many refreshes hit the cap.
    fn search_equivalence(seed: u64, steps: usize) -> usize {
        let mut r = Rng(seed | 1);
        let mut t = Terminal::new(2 + r.below(34), 1 + r.below(14));
        t.set_scrollback_lines(r.pick(&[0usize, 3, 40, 300, 3000]));
        t.search_cap = r.pick(&[1usize, 4, 30, 300, SEARCH_MAX_MATCHES]);
        if r.chance(3) {
            t.feed(b"\x1b]133;A\x07$ \x1b]133;C\x07"); // a live mark as well
        }
        let queries = ["a", "aa", "ab", "==", "中", "a中", "ba", "A"];
        t.search_set_query(r.pick(&queries));
        let mut capped = 0;
        let mut buf = Vec::new();
        for step in 0..steps {
            buf.clear();
            for _ in 0..1 + r.below(10) {
                search_fuzz_token(&mut r, &mut buf);
            }
            let mut i = 0;
            while i < buf.len() {
                let end = (i + 1 + r.below(200)).min(buf.len());
                t.feed(&buf[i..end]);
                i = end;
            }
            match r.below(30) {
                0 => t.resize(2 + r.below(34), 1 + r.below(14)),
                1 => t.set_scrollback_lines(r.pick(&[0usize, 3, 40, 300, 3000])),
                2 => t.flush_sync(),
                3 => t.scroll_lines(r.below(40) as i32 - 20),
                4 => {
                    t.search_set_query(r.pick(&queries));
                }
                _ => {}
            }
            if r.chance(4) {
                continue; // several outputs between two refreshes
            }
            t.search_refresh();
            capped += usize::from(t.search_matches.len() == t.search_cap);
            let full = full_search_matches(&mut t);
            assert!(
                t.search_matches == full,
                "seed {seed:#x} step {step} ({:?}): {} incremental vs {} full matches",
                t.search_query,
                t.search_matches.len(),
                full.len()
            );
            let (cur, total) = t.search_counter();
            assert!(cur <= total && (total == 0) == (cur == 0), "seed {seed:#x} step {step}: counter {cur}/{total}");
        }
        capped
    }

    /// An incremental refresh must find exactly what a full re-collect finds,
    /// whatever the output did since the last one: text dense in (wrapped,
    /// wide, self-overlapping) matches, screen rewrites, scroll regions, the
    /// alt screen, RIS, synchronized updates, resizes and scrollback changes,
    /// with the history below and at its cap, and the match cap hit.
    #[test]
    fn search_incremental_equals_a_full_scan() {
        let capped: usize = (1..=120u64).map(|s| search_equivalence(s.wrapping_mul(0x9e37_79b9_7f4a_7c15), 160)).sum();
        assert!(capped > 100, "the match cap must be exercised ({capped})");
    }

    /// The long run: `cargo test -p jetty-core --release -- --ignored
    /// search_incremental_long`. `JETTY_FUZZ_SEED=0x…` replays one seed.
    #[test]
    #[ignore]
    fn search_incremental_long() {
        if let Some(seed) = std::env::var("JETTY_FUZZ_SEED").ok().and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()) {
            search_equivalence(seed, 600);
            return;
        }
        for seed in 1..=5000u64 {
            search_equivalence(seed.wrapping_mul(0xd1b5_4a32_d192_ed03), 600);
        }
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
    fn prompt_count_increments_on_a_marks() {
        let mut t = Terminal::new(20, 5);
        assert_eq!(t.prompt_count(), 0, "fresh terminal has seen no prompts");
        t.feed(b"\x1b]133;A\x07");
        assert_eq!(t.prompt_count(), 1);
    }

    #[test]
    fn prompt_count_dedups_same_line_double_emission() {
        // p10k's own integration + ours both emit A for the same prompt line —
        // the dedup in bind_mark must keep the count at 1.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07\x1b]133;A\x07");
        assert_eq!(t.prompt_count(), 1, "same-line duplicate A counts once");
        assert_eq!(t.marks.len(), 1);
    }

    #[test]
    fn prompt_count_a_c_d_a_counts_two() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07");
        t.feed(b"\x1b]133;C\x07out\r\n");
        t.feed(b"\x1b]133;D;0\x07");
        t.feed(b"\x1b]133;A\x07");
        assert_eq!(t.prompt_count(), 2, "a finished block's next prompt counts");
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
        let add_prompt = |t: &mut Terminal, tag: char| {
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
    fn mark_inside_sync_block_binds_to_its_real_row() {
        // vte BUFFERS a DEC 2026 synchronized update, so when the OSC 133 inside
        // it reaches our scanner the cursor has not moved yet. The update is
        // flushed before binding, so the mark lands on the row the shell drew the
        // prompt on (row 4 here), not the pre-sync row (2).
        let mut t = Terminal::new(20, 6);
        t.feed(b"a\r\nb\r\n"); // cursor on row 2
        t.feed(b"\x1b[?2026h"); // BSU
        t.feed(b"c\r\nd\r\n\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![4], "bound after the buffered rows");
        assert!(t.sync_deadline().is_none(), "the sync was flushed to bind correctly");
        // abs_top keeps tracking history exactly afterwards.
        for i in 0..12 {
            t.feed(format!("z{i}\r\n").as_bytes());
        }
        t.scroll_to_bottom();
        assert_eq!(t.abs_top, t.scroll_max() as i64, "abs_top tracks history exactly after sync");
    }

    #[test]
    fn full_scrollback_keeps_marks_exact() {
        // A full scrollback used to PERMANENTLY disable marks (history_size()
        // pinned at the cap, so scrolls became unobservable). With the slack +
        // per-sub-slice trim, scrolling stays exactly countable for the tab's
        // whole life: marks keep binding, tracking, and aging out correctly.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(30);
        t.feed(b"a\r\nb\r\n"); // the prompt is not on the top row
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![2], "exact placement");
        for i in 0..80 {
            t.feed(format!("out {i}\r\n").as_bytes());
        }
        assert!(t.scroll_max() <= 30, "the cap bounds the scrollback");
        // The old mark scrolled out of the 30-line window and aged out.
        assert!(t.marks.is_empty(), "the out-of-window mark was pruned");
        // A fresh failed prompt binds and renders on its true row…
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![4], "binds on the real (bottom) row");
        // …and tracks scrolling into the (full) history at its true row.
        for i in 0..3 {
            t.feed(format!("more {i}\r\n").as_bytes());
        }
        assert_eq!(t.failed_prompt_rows(), vec![1], "moved up by exactly 3 rows");
        t.scroll_lines(2);
        assert_eq!(t.failed_prompt_rows(), vec![3], "maps through the scrolled viewport");
        t.scroll_to_bottom();
        // Push it into the (full) history: prompt-jump still lands on it.
        for i in 0..10 {
            t.feed(format!("tail {i}\r\n").as_bytes());
        }
        assert!(t.jump_prompt(false), "jump target exists in a full scrollback");
        assert_eq!(t.failed_prompt_rows(), vec![0], "jumped prompt sits at viewport row 0");
    }

    #[test]
    fn image_in_a_full_anchor_free_scrollback_tracks_exactly() {
        // No anchors ⇒ history is allowed to pin at the cap (full speed). An image
        // arriving then makes room for its own reserve, so it is exact from birth.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(30);
        t.set_cell_px(10.0, 10.0);
        for i in 0..200 {
            t.feed(format!("pre {i}\r\n").as_bytes());
        }
        t.feed(&sixel(RED_1X12)); // 2 reserved rows, anchored at the bottom row
        let top0 = t.visible_images()[0].top_row;
        assert_eq!(top0, 2.0, "image top sits 2 rows above the cursor row");
        t.feed(b"x\r\ny\r\n");
        assert_eq!(t.visible_images()[0].top_row, top0 - 2.0, "moved up exactly 2 rows");
        for i in 0..40 {
            t.feed(format!("post {i}\r\n").as_bytes());
        }
        assert!(t.visible_images().is_empty(), "scrolled out of the 30-line window");
    }

    #[test]
    fn huge_floods_stay_exact_and_bounded() {
        // Thousands of lines in ONE feed are split into bounded sub-slices while
        // an anchor exists, so the count stays exact and the scrollback capped.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(100);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        let abs0 = t.abs_top;
        let flood = "y\r\n".repeat(50_000);
        t.feed(flood.as_bytes());
        // From row 0 of a 5-row grid: 4 cursor moves, then 49_996 scrolls.
        assert_eq!(t.abs_top - abs0, 49_996, "every scroll counted across sub-slices");
        assert!(t.scroll_max() <= 100, "capped: {}", t.scroll_max());
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![4], "marks still exact after the flood");
    }

    /// A 20×40 tab whose 100-line scrollback filled up before the shell
    /// integration bound anything, then a failed prompt on the bottom row and 3
    /// more lines: the mark sits on row 36 and the history holds over 60 lines,
    /// so one more screen (40 lines) would overflow the cap.
    fn full_history_with_a_failed_prompt() -> Terminal {
        let mut t = Terminal::new(20, 40);
        t.set_scrollback_lines(100);
        for i in 0..150 {
            t.feed(format!("old {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b]133;A\x07$ false\x1b]133;D;1\x07");
        for i in 0..3 {
            t.feed(format!("\r\nnew {i}").as_bytes());
        }
        assert_eq!(t.failed_prompt_rows(), vec![36], "premise: the mark sits on row 36");
        assert!(t.scroll_max() + 40 > 100, "premise: a screen more overflows the cap");
        t
    }

    #[test]
    fn ctrl_l_in_a_full_scrollback_keeps_the_marks() {
        // Ctrl+L (`\e[H\e[2J`) pushes up to a whole screen into scrollback. Its
        // sub-slice used to be bounded by its 4 bytes, so a nearly full history
        // pinned at the cap and every mark/image in the tab was dropped.
        let mut t = full_history_with_a_failed_prompt();
        t.feed(b"\x1b[H\x1b[2J");
        assert_eq!(t.marks.len(), 1, "the mark survives the clear");
        assert!(t.failed_prompt_rows().is_empty(), "it was pushed into scrollback");
        assert!(t.jump_prompt(false), "and prompt-jump still finds it there");
        assert_eq!(t.failed_prompt_rows(), vec![0]);
        assert!(t.scroll_max() < 100, "history stayed below the cap: {}", t.scroll_max());
    }

    #[test]
    fn scroll_up_and_delete_lines_in_a_full_scrollback_keep_the_marks() {
        // `CSI Ps S` (SU) and `CSI Ps M` (DL on the top row) also push up to a
        // screen of lines into scrollback from a few bytes.
        for seq in [&b"\x1b[40S"[..], b"\x1b[H\x1b[40M", b"\x1b[0040S"] {
            let mut t = full_history_with_a_failed_prompt();
            t.feed(seq);
            assert_eq!(t.marks.len(), 1, "{seq:?}: the mark survives");
            t.feed(b"\x1b[40;1H");
            for i in 0..3 {
                t.feed(format!("\r\nafter {i}").as_bytes());
            }
            assert!(t.jump_prompt(false), "{seq:?}: prompt-jump finds the mark");
            assert_eq!(t.failed_prompt_rows(), vec![0], "{seq:?}: on its true row");
        }
    }

    #[test]
    fn ctrl_l_with_a_small_scrollback_and_a_tall_window_keeps_the_history() {
        // `scrollback_lines = 100` (the minimum) and 96 rows: making room for a
        // whole screen wiped the scrollback before a Ctrl+L that pushes two
        // lines. Room is made for what the clear really pushes.
        let mut t = Terminal::new(20, 96);
        t.set_scrollback_lines(100);
        for i in 0..300 {
            t.feed(format!("old {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b[H\x1b[2J\x1b[H"); // a clear screen above a full history
        t.feed(b"\x1b]133;A\x07$ false\x1b]133;D;1\x07\r\nout\r\n");
        let before = t.scroll_max();
        assert!(before >= 75, "premise: a nearly full history ({before})");
        t.feed(b"\x1b[H\x1b[2J"); // two non-empty rows to push
        assert!(t.scroll_max() >= before, "the history survives: {} < {before}", t.scroll_max());
        assert_eq!(t.marks.len(), 1, "and so does the mark");
        assert!(t.jump_prompt(false));
        assert_eq!(t.failed_prompt_rows(), vec![0]);
    }

    #[test]
    fn delete_lines_below_the_top_row_trims_no_history() {
        // DL off the top row pushes nothing into scrollback; it must not make
        // room (= trim the oldest history) for a screen it never pushes.
        let mut t = full_history_with_a_failed_prompt();
        let before = t.scroll_max();
        for _ in 0..20 {
            t.feed(b"\x1b[10;1H\x1b[5M");
        }
        assert_eq!(t.scroll_max(), before, "history untouched");
        assert_eq!(t.failed_prompt_rows(), vec![36], "the mark did not move");
    }

    #[test]
    fn scroll_up_split_across_feeds_keeps_the_marks() {
        // The same sequence arriving in pieces (a PTY read boundary inside it).
        let mut t = full_history_with_a_failed_prompt();
        t.feed(b"\x1b[");
        t.feed(b"40");
        t.feed(b"S");
        assert_eq!(t.marks.len(), 1, "the mark survives a split SU");
        assert!(t.jump_prompt(false));
        assert_eq!(t.failed_prompt_rows(), vec![0]);
    }

    #[test]
    fn pathological_scroll_overflow_resets_anchors_once_then_recovers() {
        // A sub-slice that scrolls more than the whole scrollback can hold (a
        // `CSI S` count beyond the 30-line cap) pins history: the count is lost,
        // so every anchor is dropped ONCE — and exact tracking resumes after.
        let mut t = Terminal::new(20, 200);
        t.set_scrollback_lines(30);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1);
        let burst = "\x1b[200S".repeat(20); // 20 × 200 lines in one 120-byte slice
        t.feed(burst.as_bytes());
        assert!(t.marks.is_empty(), "uncountable burst drops the anchors");
        assert!(t.scroll_max() <= 30, "and the cap holds");
        t.feed(b"\x1b[H\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![0], "fresh marks are exact again");
    }

    #[test]
    fn scrollback_shrink_and_grow_keep_tracking_exact() {
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(20);
        for i in 0..60 {
            t.feed(format!("x{i}\r\n").as_bytes());
        }
        assert!(t.scroll_max() <= 20);
        t.set_scrollback_lines(5); // live shrink trims the oldest lines
        assert!(t.scroll_max() <= 5);
        t.set_scrollback_lines(10_000);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.marks.len(), 1, "marks bind");
        assert_eq!(t.failed_prompt_rows(), vec![4], "and render on the real row");
    }

    #[test]
    fn scrolled_up_viewport_is_stable_during_a_full_scrollback_flood() {
        // The trim removes the OLDEST lines only; a viewport scrolled into the
        // middle of history keeps showing the same text while output streams.
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_lines(200);
        for i in 0..300 {
            t.feed(format!("old {i}\r\n").as_bytes());
        }
        t.scroll_lines(50);
        let before = t.snapshot().row_text(0);
        for i in 0..40 {
            t.feed(format!("new {i}\r\n").as_bytes());
        }
        assert_eq!(t.snapshot().row_text(0), before, "viewport content did not move");
    }

    #[test]
    fn reverse_screen_mode_swaps_every_cells_colors() {
        // DECSCNM (`CSI ? 5 h`) — what vim's `set visualbell` flashes (terminfo
        // `flash` = `\e[?5h`, 100 ms, `\e[?5l`); alacritty ignored the mode.
        let mut t = Terminal::new(10, 2);
        t.feed(b"a\x1b[7mb\x1b[m\x1b[31mc\x1b[m");
        let normal = t.snapshot();
        let swapped = |c: &CellSnapshot| (c.bg, c.fg);
        let colors = |c: &CellSnapshot| (c.fg, c.bg);
        t.feed(b"\x1b[?5h");
        let rev = t.snapshot();
        assert_eq!(rev.bg_rgba[..3], t.theme().fg, "the frame is cleared with the default fg");
        assert_eq!(rev.bg_rgba[3], normal.bg_rgba[3], "opacity kept");
        assert_eq!(colors(rev.cell(0, 0)), swapped(normal.cell(0, 0)), "plain text reversed");
        assert_eq!(colors(rev.cell(0, 1)), colors(normal.cell(0, 0)), "SGR 7 text shows normal");
        assert_eq!(colors(rev.cell(0, 2)), swapped(normal.cell(0, 2)), "colored text reversed");
        assert_eq!(colors(rev.cell(1, 5)), swapped(normal.cell(1, 5)), "blank cells too");
        t.feed(b"\x1b[?5$p");
        assert_eq!(t.drain_pty_writes(), b"\x1b[?5;1$y");
        t.feed(b"\x1b[?5l\x1b[?5$p");
        assert_eq!(t.drain_pty_writes(), b"\x1b[?5;2$y");
        let back = t.snapshot();
        assert_eq!((back.cells, back.bg_rgba), (normal.cells, normal.bg_rgba));
        // A resize keeps the mode; RIS ends it.
        t.feed(b"\x1b[?5h");
        t.resize(12, 3);
        assert_eq!(t.snapshot().bg_rgba[..3], t.theme().fg);
        t.feed(b"\x1bc");
        assert_eq!(t.snapshot().bg_rgba, normal.bg_rgba);
    }

    #[test]
    fn xtversion_names_the_terminal() {
        let name = format!("\x1bP>|JeTTY({})\x1b\\", crate::pty::advertised_version());
        let da1 = String::from_utf8_lossy(crate::handler::DA1_REPLY).into_owned();
        let reply = |seq: &[u8], cut: usize| {
            let mut t = Terminal::new(20, 3);
            t.feed(&seq[..cut]);
            t.feed(&seq[cut..]);
            String::from_utf8_lossy(&t.drain_pty_writes()).into_owned()
        };
        // Answered at any feed split, in order after a DA1 earlier in the read
        // (also when both sit in a synchronized update vte is still buffering).
        for seq in [&b"\x1b[>q"[..], b"\x1b[>0q", b"\x1b[c\x1b[>q", b"\x1b[?2026h\x1b[c\x1b[>q"] {
            let want = if seq.ends_with(b"[>q") && seq.len() > 4 { format!("{da1}{name}") } else { name.clone() };
            for cut in 0..=seq.len() {
                assert_eq!(reply(seq, cut), want, "{:?} cut at {cut}", String::from_utf8_lossy(seq));
            }
        }
        // Other `CSI > … q` forms are not XTVERSION.
        for seq in [&b"\x1b[>1q"[..], b"\x1b[>0;1q", b"\x1b[>$q", b"\x1b[>c"] {
            assert!(!reply(seq, 0).contains("JeTTY"), "{:?}", String::from_utf8_lossy(seq));
        }
    }

    #[test]
    fn copying_a_row_whose_wide_char_wrapped_copies_that_char() {
        // "abcd中" in 5 columns: 中 does not fit in the last column, so it wraps
        // and leaves a placeholder there. A selection ending on that row copied
        // the character at the start of the row ABOVE (alacritty's `line - 1`):
        // "abcdQ". On the top row of the history that index was out of range —
        // a debug panic, a garbage character in release.
        let mut t = Terminal::new(5, 3);
        t.feed("Qxxxx\r\nabcd中e".as_bytes());
        t.selection_start(1, 0, true);
        t.selection_update(1, 4, false);
        assert_eq!(t.selection_text().as_deref(), Some("abcd中"));
        t.selection_update(1, 3, false);
        assert_eq!(t.selection_text().as_deref(), Some("abcd"), "the placeholder not selected");
        t.selection_update(2, 2, false);
        assert_eq!(t.selection_text().as_deref(), Some("abcd中e"));
        t.selection_start_lines(1);
        assert_eq!(t.selection_text().as_deref(), Some("abcd中e\n"));
        let mut t = Terminal::new(5, 3);
        t.feed("abcd中e".as_bytes());
        t.selection_start(0, 0, true);
        t.selection_update(0, 4, false);
        assert_eq!(t.selection_text().as_deref(), Some("abcd中"));
        t.selection_start(0, 4, false);
        t.selection_update(0, 0, true);
        assert_eq!(t.selection_text().as_deref(), Some("abcd中"), "dragged backwards");
        // Block selections (copy mode's Ctrl+V): per row, the wrapped char comes
        // along on the last row, and on the others when the block does not
        // start in column 0 — alacritty's rule, minus the stray char.
        let mut t = Terminal::new(5, 3);
        t.feed("Qxxxx\r\nabcd中e".as_bytes());
        let block = |t: &mut Terminal, (l0, c0): (i32, usize), (l1, c1): (i32, usize)| {
            t.selection_start_block_abs(l0, c0, true);
            t.selection_update_abs(l1, c1, false);
            t.selection_text()
        };
        assert_eq!(block(&mut t, (1, 1), (1, 4)).as_deref(), Some("bcd中"));
        assert_eq!(block(&mut t, (0, 0), (1, 4)).as_deref(), Some("Qxxxx\nabcd中"));
        assert_eq!(block(&mut t, (1, 0), (2, 4)).as_deref(), Some("abcd\n中e"));
        assert_eq!(block(&mut t, (0, 1), (2, 4)).as_deref(), Some("xxxx\nbcd中\n中e"));
        let mut t = Terminal::new(5, 3);
        t.feed("abcd中e".as_bytes());
        assert_eq!(block(&mut t, (0, 1), (0, 4)).as_deref(), Some("bcd中"), "on the top row");
        // A corner on the right half of the last column selects no column (the
        // rectangle's left edge lands past it): empty rows, not a panic.
        t.selection_start_block_abs(0, 4, false);
        t.selection_update_abs(1, 4, true);
        assert_eq!(t.selection_text().as_deref(), Some("\n"));
    }

    #[test]
    fn cursor_viewport_cell_is_the_snapshots_cursor_without_a_snapshot() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"$ ls\r\nfoo bar\r\n$ ec");
        assert_eq!(t.cursor_viewport_cell(), (2, 4));
        // On a wide glyph's right half the cursor covers the glyph.
        t.feed("中\x1b[D".as_bytes());
        let s = t.snapshot();
        assert_eq!(t.cursor_viewport_cell(), (s.cursor_row, s.cursor_col));
        // Hidden by the program: still where it is.
        t.feed(b"\x1b[?25l");
        assert_eq!(t.cursor_viewport_cell(), (s.cursor_row, s.cursor_col));
        // Scrolled back past it: clamped onto the bottom row, like the snapshot.
        for i in 0..20 {
            t.feed(format!("\r\nline {i}").as_bytes());
        }
        t.scroll_lines(8);
        let s = t.snapshot();
        assert!(!s.cursor_visible, "premise: the cursor scrolled out of view");
        assert_eq!(t.cursor_viewport_cell(), (s.cursor_row, s.cursor_col));
        assert_eq!(t.cursor_viewport_cell().0, 4);
    }

    #[test]
    fn the_cursor_on_an_orphaned_wide_char_half_is_drawn_where_it_is() {
        // A wide char's second half with no first half before it (column 0, or
        // left by an edit through a wide char). alacritty's renderable cursor
        // stepped left off ANY second half: a cell off, and in column 0 an
        // underflow — a panic in debug builds, the cursor drawn in the LAST
        // column in release.
        let mut t = Terminal::new(6, 2);
        t.feed(b"ab");
        for col in [0, 1] {
            t.term.grid_mut()[Line(0)][Column(col)].flags.insert(Flags::WIDE_CHAR_SPACER);
            t.feed(format!("\x1b[1;{}H", col + 1).as_bytes());
            let s = t.snapshot();
            assert_eq!((s.cursor_row, s.cursor_col), (0, col));
            assert_eq!(t.viewport_rows_chars()[0][col], WIDE_SPACER);
        }
        // On a real wide char's second half the cursor still covers the glyph.
        t.feed("\x1b[2;1Hx中\x1b[2;3H".as_bytes());
        assert_eq!(t.snapshot().cursor_col, 1);
    }

    #[test]
    fn region_scrolls_while_scrolled_back_leave_the_view_alone() {
        // Scrolling a region below a fixed top row (DECSTBM top margin > 1) or
        // deleting lines below the top row pushes nothing into history, yet
        // alacritty 0.26 bumped the display offset on every such scroll while the
        // view was scrolled back: the view drifted up a line per scroll, and once
        // past the top of the history the next snapshot indexed outside the grid
        // (a panic, release builds too). LF / IND / SU / DL / an autowrap.
        let scrolls: [&[u8]; 5] = [
            b"\x1b[2;5r\x1b[5;1H\n",
            b"\x1b[2;5r\x1b[5;1H\x1bD",
            b"\x1b[2;5r\x1b[S",
            b"\x1b[r\x1b[3;1H\x1b[M",
            b"\x1b[2;5r\x1b[5;1Hxxxxxxxxxxx",
        ];
        for scroll in scrolls {
            let mut t = Terminal::new(10, 5);
            for i in 0..20 {
                t.feed(format!("line{i}\r\n").as_bytes());
            }
            t.scroll_lines(3);
            let view: Vec<String> = (0..2).map(|r| t.snapshot().row_text(r)).collect();
            for _ in 0..3000 {
                t.feed(scroll);
            }
            let what = String::from_utf8_lossy(scroll);
            assert_eq!(t.scroll_offset(), 3, "{what:?}: the view drifted");
            let now: Vec<String> = (0..2).map(|r| t.snapshot().row_text(r)).collect();
            assert_eq!(now, view, "{what:?}: the history rows on screen changed");
        }
        // Leaving the alt screen in the same write as the region scrolls: the
        // count is lost there, but the view stays inside the history.
        let mut t = Terminal::new(10, 5);
        for i in 0..20 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        t.scroll_lines(3);
        t.feed(b"\x1b[?1049h");
        let mut burst = b"\x1b[?1049l\x1b[2;5r\x1b[5;1H".to_vec();
        burst.extend_from_slice(&b"\n".repeat(3000));
        t.feed(&burst);
        assert!(t.scroll_offset() <= t.scroll_max(), "{} > {}", t.scroll_offset(), t.scroll_max());
        let _ = t.snapshot();
    }

    #[test]
    fn output_that_fills_history_while_scrolled_back_keeps_the_view_on_its_lines() {
        // A full-screen scroll DOES push lines into history: the view follows
        // its content up (also with a bottom status line, apt-style), in one
        // write or many, until that content falls off the top of the history.
        for (region, bottom) in [("", 5), ("\x1b[1;4r", 4)] {
            let mut t = Terminal::new(10, 5);
            t.set_scrollback_lines(100);
            for i in 0..50 {
                t.feed(format!("line{i}\r\n").as_bytes());
            }
            t.feed(region.as_bytes());
            t.scroll_lines(10);
            let view = t.snapshot().row_text(0);
            let mut burst = String::new();
            for i in 0..30 {
                burst.push_str(&format!("\x1b[{bottom};1Hnew{i}\n"));
            }
            t.feed(burst.as_bytes());
            for i in 0..20 {
                t.feed(format!("\x1b[{bottom};1Hmore{i}\n").as_bytes());
            }
            assert_eq!(t.scroll_offset(), 60, "{region:?}: the view follows its lines");
            assert_eq!(t.snapshot().row_text(0), view, "{region:?}");
            t.feed(format!("\x1b[{bottom};1H\n").repeat(500).as_bytes());
            assert!(t.scroll_offset() <= t.scroll_max(), "{region:?}: inside the history");
            let _ = t.snapshot();
        }
    }

    #[test]
    fn run_and_notify_survives_a_full_scrollback() {
        // The v0.15 headline feature used to die silently once 10k lines filled
        // the scrollback. Completions no longer depend on row anchors.
        let mut t = Terminal::new(40, 6);
        t.set_scrollback_lines(30);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        for i in 0..200 {
            t.feed(format!("build step {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b]133;D;2\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1, "completion emitted past the scrollback cap");
        assert_eq!(done[0].exit_code, Some(2));
        assert!(done[0].duration.is_some(), "C→D duration kept");
        assert_eq!(done[0].last_line, "build step 199");
    }

    #[test]
    fn run_and_notify_survives_a_resize_mid_command() {
        // A window/font resize during a long build drops the row anchors (reflow)
        // but must not lose the build's completion.
        let mut t = Terminal::new(40, 6);
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        t.feed(b"compiling...\r\n");
        t.resize(60, 10);
        t.feed(b"finished\r\n\x1b]133;D;0\x07");
        let done = t.take_completions();
        assert_eq!(done.len(), 1, "the in-flight command still completes");
        assert!(done[0].duration.is_some());
    }

    #[test]
    fn clear_in_one_write_drops_stale_failed_marker() {
        // `false; clear`: ncurses writes `\e[H\e[2J\e[3J` in ONE write. Isolating
        // ED 3 lets the bookkeeping see "+rows (2J) then shrink (3J)" instead of a
        // net-zero change that left the red marker on the NEW prompt's row.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]133;A\x07$ false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        t.feed(b"\x1b]133;A\x07$ clear\r\n\x1b]133;C\x07");
        assert_eq!(t.failed_prompt_rows(), vec![0]);
        t.feed(b"\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert!(t.failed_prompt_rows().is_empty(), "no stale marker after clear");
        assert_eq!(t.marks.len(), 1, "only the fresh prompt remains");
    }

    #[test]
    fn clear_inside_a_sync_update_drops_the_cleared_anchors() {
        // vte BUFFERS a DEC 2026 synchronized update and applies it at once, so a
        // `\e[2J\e[3J` + redraw inside one used to show up as a single NET history
        // change (2J's push − 3J's clear + the redraw's scroll) — old marks then
        // tracked that net change onto the NEW rows (a red marker beside the
        // redrawn content, a prompt-jump into it).
        let mut t = Terminal::new(20, 5);
        t.feed(b"a\r\nb\r\n\x1b]133;A\x07$ false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        for i in 0..4 {
            t.feed(format!("out {i}\r\n").as_bytes());
        }
        assert_eq!(t.marks.len(), 1, "premise: the failed mark is in scrollback");
        let mut block = b"\x1b[?2026h\x1b[H\x1b[2J\x1b[3J".to_vec();
        for i in 0..9 {
            block.extend_from_slice(format!("redraw {i}\r\n").as_bytes());
        }
        block.extend_from_slice(b"\x1b[?2026l");
        t.feed(&block);
        assert!(t.marks.is_empty(), "the cleared scrollback took its marks along");
        assert!(t.failed_prompt_rows().is_empty(), "no marker on the redrawn rows");
        assert!(!t.jump_prompt(false), "no stale prompt-jump target");
        assert_eq!(t.snapshot().row_text(0).trim_end(), "redraw 5", "the redraw itself landed");
        // Tracking stays exact for the next prompt.
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![4]);
    }

    #[test]
    fn clear_in_one_write_drops_the_image() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X6));
        assert_eq!(t.visible_images().len(), 1);
        t.feed(b"\x1b[H\x1b[2J\x1b[3J");
        assert!(t.visible_images().is_empty(), "the image went with the cleared screen");
        assert!(t.placements.is_empty(), "and was pruned with the scrollback");
    }

    #[test]
    fn ris_drops_every_anchor_but_not_the_running_command() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        let _ = t.take_completions(); // the first (failed) command
        t.feed(&sixel(RED_1X6));
        t.feed(b"\x1bc"); // `reset`
        assert!(t.marks.is_empty() && t.placements.is_empty(), "RIS clears anchors");
        assert!(t.failed_prompt_rows().is_empty());
        t.feed(b"\x1b]133;D;0\x07");
        assert_eq!(t.take_completions().len(), 1, "the `reset` command still completes");
    }

    #[test]
    fn primary_output_sharing_a_slice_with_alt_exit_is_counted() {
        // vim exit + shell prompt can arrive in one PTY read. The toggle is
        // isolated, so the primary lines after it still advance abs_top.
        let mut t = Terminal::new(20, 5);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n");
        t.feed(b"\x1b]133;A\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![4]);
        let mut slice = b"\x1b[?1049h".to_vec();
        slice.extend_from_slice(b"tui\r\ntui\r\n");
        slice.extend_from_slice(b"\x1b[?1049l");
        slice.extend_from_slice(b"x\r\ny\r\n");
        t.feed(&slice);
        assert_eq!(t.failed_prompt_rows(), vec![2], "marker followed the 2 primary scrolls");
    }

    #[test]
    fn a_synchronized_update_never_moves_the_marks() {
        // vte only BUFFERS a DEC 2026 update and replays it at its end in one
        // piece, so a sequence the scanner isolates did not run alone in it:
        // an alt-screen toggle froze `abs_top` over the primary lines sharing
        // its replay (nvim >= 0.10 wraps its exit, rmcup included, in BSU/ESU),
        // ED 2 pruned the screen's marks before it pushed them into scrollback,
        // ED 3 hid the scroll before it. In an update or not, the same bytes
        // must leave the marks on the same lines (a negative one: scrollback).
        let setup: &[u8] = b"1\r\n2\r\n\x1b]133;A\x07$ false\r\n\x1b]133;D;1\x07"; // prompt on row 2
        // Bytes before the update, its body, bytes after it, the prompt's line.
        type Case = (&'static [u8], &'static [u8], &'static [u8], i64);
        let cases: [Case; 5] = [
            // Leaving the alt screen, then the shell's lines, in one read.
            (b"\x1b[?1049htui\r\n", b"\x1b[?1049lx\r\ny\r\n", b"", 1),
            // Primary output, then entering the alt screen.
            (b"", b"a\r\nb\r\n\x1b[?1049htui", b"\x1b[?1049l", 1),
            // A clear that keeps the scrollback: the prompt is pushed into it.
            (b"", b"\x1b[H\x1b[2Jredraw", b"", -1),
            // Scrolling, then a scrollback clear.
            (b"", b"x\r\ny\r\nz\r\n\x1b[3J", b"", 0),
            (b"", b"\x1b[2S", b"", 0),
        ];
        for (before, body, after, line) in cases {
            for (sync, split) in [(false, false), (true, false), (true, true)] {
                let mut t = Terminal::new(20, 5);
                t.feed(setup);
                t.feed(before);
                let wrap = |s: &'static [u8]| if sync { s } else { b"" };
                let read = [wrap(b"\x1b[?2026h"), body, wrap(b"\x1b[?2026l")].concat();
                // One PTY read, or one per byte (sequences cut by read boundaries).
                for chunk in read.chunks(if split { 1 } else { read.len() }) {
                    t.feed(chunk);
                }
                t.feed(after);
                let lines: Vec<i64> = t.marks.iter().map(|m| m.prompt - t.abs_top).collect();
                assert_eq!(lines, vec![line], "sync={sync} split={split}: {:?}", String::from_utf8_lossy(body));
            }
        }
    }

    #[test]
    fn a_mark_after_an_alt_exit_inside_a_synchronized_update_binds() {
        // Nothing anchored yet, so the toggle is not split out and is still
        // buffered when the prompt's mark arrives in the same update: the mark
        // belongs to the primary screen the update returns to.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?1049htui");
        t.feed(b"\x1b[?2026h\x1b[?1049l\x1b]133;A\x07$ false\r\n\x1b]133;D;1\x07\x1b[?2026l");
        assert!(!t.alt_screen());
        assert_eq!(t.failed_prompt_rows(), vec![0], "the failed prompt is marked");
    }

    #[test]
    fn resize_wipe_spares_an_a_and_d_only_shell() {
        // Integrations that never send C (old bash) used to look "clean" forever,
        // so every resize erased the whole tab. A completed command (D) latches.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07$ ls\r\n");
        t.feed(b"IMPORTANT_OUTPUT_XYZ\r\n");
        t.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        t.resize(40, 12);
        t.resize(100, 30);
        let snap = t.snapshot();
        let found = (0..snap.rows).any(|r| snap.row_text(r).contains("IMPORTANT_OUTPUT_XYZ"));
        assert!(found, "an A/D-only shell's output survives a resize");
    }

    #[test]
    fn resize_wipe_spares_a_startup_banner() {
        // fastfetch / motd printed before the FIRST prompt: no command ever ran,
        // but the prompt is not the topmost content, so the tab is not "clean".
        let mut t = Terminal::new(80, 24);
        t.feed(b"Welcome to BANNER_HOST\r\n\r\n");
        t.feed(b"\x1b]133;A\x07$ ");
        t.resize(40, 12);
        let snap = t.snapshot();
        let found = (0..snap.rows).any(|r| snap.row_text(r).contains("BANNER_HOST"));
        assert!(found, "a banner above the first prompt survives a resize");
    }

    #[test]
    fn resize_wipe_spares_a_running_first_command_without_c() {
        // A C-less shell running its very first command: the cursor has moved
        // well below the prompt line, so the tab is not idle at a clean prompt.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07$ cat log\r\n");
        for i in 0..12 {
            t.feed(format!("LOGLINE {i}\r\n").as_bytes());
        }
        t.resize(40, 12);
        let snap = t.snapshot();
        let found = (0..snap.rows).any(|r| snap.row_text(r).contains("LOGLINE 11"));
        assert!(found, "output of a running first command survives a resize");
    }

    /// Whether any row of `t`'s screen contains `needle`.
    fn screen_has(t: &Terminal, needle: &str) -> bool {
        let snap = t.snapshot();
        (0..snap.rows).any(|r| snap.row_text(r).contains(needle))
    }

    #[test]
    fn resize_wipe_spares_a_short_first_command_without_c() {
        // A shell that sends no C, running its very first command: three output
        // lines keep the cursor well within the old 8-row allowance, but a hard
        // line break separates them from the prompt — never a clean prompt.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07$ cat notes\r\n");
        t.feed(b"NOTE_ONE\r\nNOTE_TWO\r\n");
        t.resize(40, 12);
        assert!(screen_has(&t, "NOTE_ONE") && screen_has(&t, "NOTE_TWO"), "the output survives");
    }

    #[test]
    fn resize_wipe_spares_a_prompt_that_will_not_redraw() {
        // bash: readline repaints only the LAST line of a multi-line PS1 after
        // SIGWINCH, and the snippet says so with `A;redraw=0`; wiping would lose
        // the info line for good.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A;redraw=0\x07[INFO_LINE]\r\n$ ");
        t.resize(40, 12);
        assert!(screen_has(&t, "INFO_LINE"), "the first prompt line survives");
        // Even a single-line prompt: the shell said it will not repaint it.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A;aid=7;redraw=0\x07BASH_PROMPT$ ");
        t.resize(40, 12);
        assert!(screen_has(&t, "BASH_PROMPT"));
    }

    #[test]
    fn redraw_param_is_read_from_any_a_parameter() {
        let parse = |payload: &[u8]| {
            let mut t = Terminal::new(20, 5);
            t.feed(payload);
            t.marks.back().map(|m| m.redraws)
        };
        assert_eq!(parse(b"\x1b]133;A\x07"), Some(true));
        assert_eq!(parse(b"\x1b]133;A;redraw=0\x07"), Some(false));
        assert_eq!(parse(b"\x1b]133;A;cl=m;redraw=0;aid=3\x1b\\"), Some(false));
        assert_eq!(parse(b"\x1b]133;A;redraw=1\x07"), Some(true));
        assert_eq!(parse(b"\x1b]133;A;redraw=00\x07"), Some(true));
        assert_eq!(parse(b"\x1b]133;A;xredraw=0\x07"), Some(true));
        // Split across feeds at every byte.
        let mut t = Terminal::new(20, 5);
        for b in b"\x1b]133;A;redraw=0\x07" {
            t.feed(&[*b]);
        }
        assert_eq!(t.marks.back().map(|m| m.redraws), Some(false));
    }

    #[test]
    fn resize_wipe_still_fires_with_wrapped_typed_input() {
        // Typed input longer than a row soft-wraps: still the input line.
        let mut t = Terminal::new(20, 10);
        t.feed(b"\x1b]133;A\x07\xe2\x9d\xaf ");
        t.feed("x".repeat(30).as_bytes()); // wraps onto a second row
        assert_eq!(t.snapshot().cursor_row, 1, "premise: the input wrapped");
        t.resize(12, 8);
        assert!(!screen_has(&t, "\u{276f}"), "the clean prompt was wiped (the shell repaints it)");
    }

    #[test]
    fn resize_wipe_uses_the_b_mark_for_a_multi_line_prompt() {
        // p10k / starship with B: the prompt's first line is above the input
        // line, joined by a HARD break — B says where input starts, so this is
        // still a clean prompt and its stray fragments are wiped.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07~/src/jetty  main\r\n\x1b]133;B\x07\xe2\x9d\xaf ");
        t.resize(40, 12);
        assert!(!screen_has(&t, "~/src/jetty"), "wiped: the shell repaints both lines");
        // Without B the same two lines are not provably a prompt: kept.
        let mut t = Terminal::new(80, 24);
        t.feed(b"\x1b]133;A\x07~/src/jetty  main\r\n\xe2\x9d\xaf ");
        t.resize(40, 12);
        assert!(screen_has(&t, "~/src/jetty"), "no B: never wiped");
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

    /// What powerlevel10k (`POWERLEVEL9K_TERM_SHELL_INTEGRATION=true`, two-line
    /// prompt, transient prompt) prints — recorded from a real zsh, SGR trimmed.
    const P10K_PROMPT: &[u8] =
        b"\x1b]133;A\x07\r\n\r\n\x1b[A\xe2\x95\xad\xe2\x94\x80 ~\r\n\xe2\x95\xb0\xe2\x94\x80 \x1b]133;B\x07";

    /// Enter on [`P10K_PROMPT`] with `cmd` typed: zsh climbs back to the row
    /// the prompt began on (its `A` row) and the transient prompt redraws it as
    /// `❯ <cmd>`, then the newline — recorded from a real zsh.
    fn p10k_accept(cmd: &str) -> Vec<u8> {
        format!("\r\r\x1b[A\x1b[A\x1b[J\x1b]133;A\x07\u{276f} \x1b]133;B\x07{cmd}\x1b[K\r\r\n").into_bytes()
    }

    /// zsh's SIGWINCH repaint of [`P10K_PROMPT`] (recorded): back up to the
    /// prompt's first row, clear below, print the whole prompt again — its `A`
    /// included.
    fn p10k_winch() -> Vec<u8> {
        [&b"\r\r\x1b[A\x1b[A\x1b[J"[..], P10K_PROMPT].concat()
    }

    /// zsh's PROMPT_SP at 40 columns: `%` + padding, then back to column 0.
    fn p10k_prompt_sp() -> Vec<u8> {
        format!("\x1b[7m%\x1b[0m{}\r \r", " ".repeat(39)).into_bytes()
    }

    /// p10k prompts at 40 columns with the transient prompt OFF whose input row
    /// carries right-aligned text, shaped like recorded zsh 5.9 streams (SGR
    /// trimmed): after the `B` zsh draws RPROMPT at the far right and returns
    /// to the `B`. A one-line prompt with right segments, and the two-line
    /// frame (`╰─ … ─╯`), whose right end is RPROMPT too.
    const P10K_RPROMPTS: [&[u8]; 2] = [
        b"\x1b]133;A\x07~/proj \xe2\x9d\xaf \x1b]133;B\x07\x1b[K\x1b[21C\xe2\x9c\x98 1 12:00\x1b[30D",
        b"\x1b]133;A\x07\r\n\r\n\x1b[A\xe2\x95\xad\xe2\x94\x80 ~ 12:00 \xe2\x94\x80\xe2\x95\xae\r\n\
          \xe2\x95\xb0\xe2\x94\x80 \x1b]133;B\x07\x1b[K\x1b[34C\xe2\x94\x80\xe2\x95\xaf\x1b[36D",
    ];

    #[test]
    fn an_empty_enter_is_not_a_command_even_when_the_shell_reports_one() {
        // p10k (like iTerm2's own script) answers an empty Enter with `C;` and
        // `D;<$?>` — `$?` still being the PREVIOUS command's status — so every
        // empty Enter after a failure drew another failed marker (and pulsed).
        // Nothing was typed between the prompt's B and the C: no command ran.
        let mut t = Terminal::new(40, 24);
        t.feed(P10K_PROMPT);
        t.feed(b"false");
        t.feed(&p10k_accept("false"));
        t.feed(b"\x1b]133;C;\x07"); // preexec
        t.feed(&p10k_prompt_sp());
        t.feed(b"\x1b]133;D;1\x07"); // precmd: false failed
        t.feed(P10K_PROMPT);
        let failed = t.failed_prompt_rows();
        assert_eq!(failed.len(), 1, "premise: `false` is marked failed");
        for _ in 0..2 {
            // Empty Enter: no preexec — precmd sends `C;` + the stale `D;1`.
            t.feed(&p10k_accept(""));
            t.feed(&p10k_prompt_sp());
            t.feed(b"\x1b]133;C;\x07\x1b]133;D;1\x07");
            t.feed(P10K_PROMPT);
        }
        assert_eq!(t.failed_prompt_rows(), failed, "only the `false` prompt is marked failed");
        let done = t.take_completions();
        assert_eq!(done.len(), 1, "only `false` completed: {done:?}");
        assert_eq!(done[0].exit_code, Some(1));
        assert!(!t.command_running());
        // A command typed on the same prompt still counts, whatever ran before.
        t.feed(b"true");
        t.feed(&p10k_accept("true"));
        t.feed(b"\x1b]133;C;\x07");
        t.feed(&p10k_prompt_sp());
        t.feed(b"\x1b]133;D;0\x07");
        t.feed(P10K_PROMPT);
        let done = t.take_completions();
        assert_eq!(done.len(), 1, "`true` completed: {done:?}");
        assert_eq!(done[0].exit_code, Some(0));
    }

    #[test]
    fn an_empty_enter_under_a_right_prompt_is_not_a_command() {
        // Without the transient prompt zsh keeps RPROMPT on the accepted line,
        // so the input row of an empty Enter still holds right-aligned text.
        // That is not a typed command: p10k's `C;` + stale `D;1` after a
        // failure must mark nothing failed and complete nothing.
        for prompt in P10K_RPROMPTS {
            let mut t = Terminal::new(40, 24);
            t.feed(prompt);
            t.feed(b"false\r\r\n\x1b]133;C;\x07"); // preexec
            t.feed(&p10k_prompt_sp());
            t.feed(b"\x1b]133;D;1\x07\x1b[K\r\n"); // precmd, add-newline
            t.feed(prompt);
            let failed = t.failed_prompt_rows();
            assert_eq!(failed.len(), 1, "premise: `false` is marked failed");
            assert_eq!(t.take_completions().len(), 1);
            for _ in 0..2 {
                // Empty Enter: no preexec — precmd sends `C;` + the stale `D;1`.
                t.feed(b"\r\r\n");
                t.feed(&p10k_prompt_sp());
                t.feed(b"\x1b]133;C;\x07\x1b]133;D;1\x07\x1b[K\r\n");
                t.feed(prompt);
            }
            assert_eq!(t.failed_prompt_rows(), failed, "only the `false` prompt is marked failed");
            assert!(t.take_completions().is_empty(), "an empty Enter completes nothing");
            // A command typed on such a prompt still counts.
            t.feed(b"false\r\r\n\x1b]133;C;\x07");
            t.feed(&p10k_prompt_sp());
            t.feed(b"\x1b]133;D;1\x07\x1b[K\r\n");
            t.feed(prompt);
            assert_eq!(t.failed_prompt_rows().len(), 2, "the second `false` is marked too");
            assert_eq!(t.take_completions().len(), 1);
        }
    }

    #[test]
    fn a_prompt_repainted_after_a_resize_is_marked_again() {
        // A resize drops every mark, and zsh repaints the whole prompt — p10k's
        // A included — on SIGWINCH. That A must mark the prompt anew, even on
        // the very line the dropped mark sat on (row 0 of a wiped fresh tab):
        // swallowed as a duplicate, the live prompt stayed unmarked, so the
        // NEXT resize no longer wiped and the prompt scattered.
        let mut t = Terminal::new(80, 24);
        t.feed(P10K_PROMPT);
        for (cols, rows) in [(60, 20), (80, 24), (60, 20)] {
            t.resize(cols, rows);
            assert!(!screen_has(&t, "\u{256d}"), "the clean prompt was wiped at {cols}x{rows}");
            t.feed(&p10k_winch());
            assert_eq!(t.marks.len(), 1, "the repainted prompt is marked at {cols}x{rows}");
        }
    }

    #[test]
    fn a_failed_command_after_a_resize_keeps_its_marker() {
        // A used tab (no wipe) resized without rewrapping the prompt — a height
        // change, a font zoom: the repainted prompt sits on the same absolute
        // line as before. It must still get a block, or the next command's D
        // finds none and the failure goes unmarked.
        let mut t = Terminal::new(40, 24);
        t.feed(P10K_PROMPT);
        t.feed(b"true");
        t.feed(&p10k_accept("true"));
        t.feed(b"\x1b]133;C;\x07");
        t.feed(&p10k_prompt_sp());
        t.feed(b"\x1b]133;D;0\x07");
        t.feed(P10K_PROMPT); // A on row 1
        t.resize(40, 30);
        t.feed(&p10k_winch());
        t.feed(b"false");
        t.feed(&p10k_accept("false"));
        t.feed(b"\x1b]133;C;\x07");
        t.feed(&p10k_prompt_sp());
        t.feed(b"\x1b]133;D;1\x07");
        t.feed(P10K_PROMPT);
        assert_eq!(t.failed_prompt_rows(), vec![1], "`false` is marked failed on its prompt");
        let done = t.take_completions();
        assert_eq!(done.last().map(|c| c.exit_code), Some(Some(1)));
    }

    #[test]
    fn a_right_prompt_b_mark_never_hides_the_typed_command() {
        // An integration that also ends a right prompt with B (p10k does so for
        // Warp) puts a second B at the far right of the input line: the command
        // typed LEFT of it must still count — the leftmost B on a row wins.
        let mut t = Terminal::new(40, 10);
        t.feed(b"\x1b]133;A\x07\xe2\x9d\xaf \x1b]133;B\x07"); // left prompt `❯ `
        t.feed(b"\x1b[36G12:00\x1b]133;B\x07\x1b[3G"); // right prompt, cursor back
        t.feed(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        assert_eq!(t.failed_prompt_rows(), vec![0], "the failed command keeps its marker");
        assert_eq!(t.take_completions().len(), 1);
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
    fn completion_body_is_never_the_prompt_or_the_command_line() {
        // A command that prints nothing (`sleep 30`, `cp -r`): the toast body
        // used to be the nearest text above the cursor — the command line and
        // the prompt, its private-use icons drawn as tofu. Its output region is
        // blank: the body is empty. zsh / p10k shape, two-line prompt:
        let mut t = Terminal::new(60, 8);
        let prompt = "\x1b]133;A\x07~/proj \u{e0a0} main\r\n\u{276f} \x1b]133;B\x07";
        t.feed(prompt.as_bytes());
        t.feed(b"sleep 30\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(t.take_completions()[0].last_line, "", "a silent command: empty body");
        // A command's own output still is the body.
        t.feed(prompt.as_bytes());
        t.feed(b"ls\r\n\x1b]133;C\x07Cargo.toml\r\n\x1b]133;D;0\x07");
        assert_eq!(t.take_completions()[0].last_line, "Cargo.toml");
        // bash without a C mark (3.2 has no PS0): output starts below the
        // command line.
        t.feed(b"\x1b]133;A;redraw=0\x07$ sleep 1\r\n\x1b]133;D;0\x07");
        assert_eq!(t.take_completions()[0].last_line, "");
        t.feed(b"\x1b]133;A;redraw=0\x07$ date\r\nThu Oct  9\r\n\x1b]133;D;0\x07");
        assert_eq!(t.take_completions()[0].last_line, "Thu Oct  9");
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

    /// The single pending OSC 52 copy, if exactly one is pending.
    fn one_copy(t: &mut Terminal) -> Option<(Osc52Target, String)> {
        let mut v = t.take_clipboard_stores();
        assert!(v.len() <= 1, "expected at most one pending copy: {v:?}");
        v.pop()
    }

    #[test]
    fn osc52_copy_captures_and_coalesces() {
        // `\e]52;c;<base64("hi")>\a` → the decoded text is captured once, then
        // consumed (a second drain is empty). base64("hi") == "aGk=".
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGk=\x07");
        assert_eq!(one_copy(&mut t), Some((Osc52Target::Clipboard, "hi".to_string())));
        assert!(t.take_clipboard_stores().is_empty(), "consuming: second drain is empty");
    }

    #[test]
    fn osc52_copy_reports_the_named_selection() {
        // `c` is the clipboard; `p` (and `s`) the PRIMARY selection, so a
        // remote nvim `"*y` lands where a middle click pastes, not over Ctrl+V.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;p;aGk=\x07");
        assert_eq!(one_copy(&mut t), Some((Osc52Target::Primary, "hi".to_string())));
        t.feed(b"\x1b]52;c;aGk=\x07");
        assert_eq!(one_copy(&mut t), Some((Osc52Target::Clipboard, "hi".to_string())));
    }

    #[test]
    fn osc52_copy_coalesces_last_wins() {
        // Two copies to the same selection before a drain coalesce to the LAST one.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGk=\x07"); // "hi"
        t.feed(b"\x1b]52;c;eWE=\x07"); // base64("ya") == "eWE="
        assert_eq!(one_copy(&mut t), Some((Osc52Target::Clipboard, "ya".to_string())));
    }

    #[test]
    fn osc52_copies_to_both_selections_both_land() {
        // nvim with `clipboard=unnamed,unnamedplus` sends `c` then `p` for every
        // yank; one shared slot let the `p` overwrite the `c`, so the CLIPBOARD
        // (Ctrl+V) never got the text.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGk=\x07\x1b]52;p;aGk=\x07");
        assert_eq!(
            t.take_clipboard_stores(),
            vec![(Osc52Target::Clipboard, "hi".to_string()), (Osc52Target::Primary, "hi".to_string())]
        );
        // Still one per selection, newest text, arrival order of the newest.
        t.feed(b"\x1b]52;p;aGk=\x07\x1b]52;c;aGk=\x07\x1b]52;p;eWE=\x07");
        assert_eq!(
            t.take_clipboard_stores(),
            vec![(Osc52Target::Clipboard, "hi".to_string()), (Osc52Target::Primary, "ya".to_string())]
        );
    }

    // ── JeTTY's own notices ───────────────────────────────────────────────────

    /// A directory / shell name an attacker controls, carrying an OSC 52
    /// clipboard write, an OSC 0 title, a DSR + DA query (replies are typed into
    /// the shell), a C1 CSI query, a kitty APC query and an OSC 133 mark.
    const HOSTILE: &str = "x\x1b]52;c;aGk=\x07 \x1b]0;pwned\x07 \x1b[6n \x1b[c \u{9b}6n \
        \x1b_Ga=q,i=31;AAAA\x1b\\ \x1b]133;A\x07 tab\tcr\rlf\ndel\x7fend";

    #[test]
    fn notice_line_keeps_only_our_own_sgr() {
        let line = Terminal::notice_line(HOSTILE);
        let inner = line
            .strip_prefix("\x1b[33m")
            .and_then(|s| s.strip_suffix("\x1b[0m\r\n"))
            .expect("our yellow wrapper, nothing else around it");
        assert!(!inner.chars().any(char::is_control), "a control survived: {inner:?}");
        assert!(inner.contains("]52;c;aGk=") && inner.contains('\u{fffd}'), "shown as text: {inner:?}");
    }

    #[test]
    fn feeding_a_hostile_notice_runs_none_of_its_sequences() {
        let mut t = Terminal::new(120, 5);
        assert!(t.drain_pty_writes().is_empty());
        t.feed_notice(HOSTILE);
        assert!(t.take_clipboard_stores().is_empty(), "no OSC 52 clipboard write");
        assert_eq!(t.take_title_update(), None, "no OSC 0 title change");
        assert!(t.drain_pty_writes().is_empty(), "no query reply may reach the shell");
        let snap = t.snapshot();
        let row0: String = snap.cells[..snap.cols].iter().map(|c| c.c).collect();
        assert!(row0.contains("]52;c;aGk="), "the payload is visible text: {row0:?}");
    }

    #[test]
    fn osc52_selection_type_routes_to_primary() {
        // `s` (the "selection" form) is the PRIMARY selection too, like `p`; both
        // are permitted remote writes under OnlyCopy.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;s;aGk=\x07");
        assert_eq!(one_copy(&mut t), Some((Osc52Target::Primary, "hi".to_string())));
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
        let got = one_copy(&mut t);
        assert_eq!(got.as_ref().map(|(_, s)| s.len()), Some(decoded_len));
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
        assert!(t.take_clipboard_stores().is_empty(), "oversized copy is rejected");
    }

    #[test]
    fn osc52_paste_denied_by_default() {
        // Default build is write-only (OnlyCopy): a paste query `\e]52;c;?\a` is
        // denied at the alacritty layer, so no load request ever reaches us.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;?\x07");
        assert!(t.take_clipboard_loads().is_empty(), "paste is off by default");
    }

    #[test]
    fn osc52_paste_request_captured_when_enabled() {
        // With paste enabled, a query yields a reply formatter that produces a
        // well-formed `\e]52;` reply from the provided clipboard text.
        let mut t = Terminal::new(20, 5);
        t.set_osc52_allow_paste(true);
        t.feed(b"\x1b]52;c;?\x07");
        let mut loads = t.take_clipboard_loads();
        assert_eq!(loads.len(), 1, "paste request captured");
        let (target, fmt) = loads.pop().unwrap();
        assert_eq!(target, Osc52Target::Clipboard);
        let reply = fmt("hi");
        assert!(reply.starts_with("\x1b]52;"), "reply is an OSC 52 sequence");
        assert!(reply.contains("aGk="), "reply carries base64(\"hi\")");
        assert!(t.take_clipboard_loads().is_empty(), "consuming: second drain is empty");
    }

    #[test]
    fn osc52_paste_requests_for_both_selections_each_get_a_reply() {
        // A `c;?` + `p;?` pair (nvim's paste provider asks for both) must yield TWO
        // replies, each naming its own selection, in request order.
        let mut t = Terminal::new(20, 5);
        t.set_osc52_allow_paste(true);
        t.feed(b"\x1b]52;c;?\x07\x1b]52;p;?\x07");
        let loads = t.take_clipboard_loads();
        let targets: Vec<Osc52Target> = loads.iter().map(|(t, _)| *t).collect();
        assert_eq!(targets, vec![Osc52Target::Clipboard, Osc52Target::Primary]);
        assert!(loads[0].1("a").starts_with("\x1b]52;c;"), "the clipboard reply names c");
        assert!(loads[1].1("b").starts_with("\x1b]52;p;"), "the primary reply names p");
    }

    #[test]
    fn osc52_scrollback_change_preserves_paste_mode() {
        // Regression (amendment O2): changing scrollback rebuilds the alacritty
        // Config and must NOT revert an enabled paste back to OnlyCopy.
        let mut t = Terminal::new(20, 5);
        t.set_osc52_allow_paste(true);
        t.set_scrollback_lines(500);
        t.feed(b"\x1b]52;c;?\x07");
        assert_eq!(t.take_clipboard_loads().len(), 1, "paste survives a scrollback change");
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
    fn csi_14t_reports_the_text_area_the_pty_is_told() {
        // With 8.6 px cells and 200 columns the PTY is told 1720 px wide, but
        // `CSI 14 t` answered 200 × 9 = 1800: lsix sizes its montage from it,
        // so a native-size montage spilled 80 px past the grid.
        let mut t = Terminal::new(200, 50);
        t.set_cell_px(8.6, 21.0);
        t.feed(b"\x1b[14t");
        assert_eq!(String::from_utf8(t.drain_pty_writes()).unwrap(), "\x1b[4;1050;1720t");
        t.resize(100, 40);
        t.feed(b"\x1b[14t");
        assert_eq!(String::from_utf8(t.drain_pty_writes()).unwrap(), "\x1b[4;840;860t");
    }

    #[test]
    fn sixel_records_placement_and_reserves_rows() {
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X12)); // 1×12 px → rows = ceil(12/10) = 2
        assert_eq!(t.placements.len(), 1, "one placement recorded");
        let p = &t.placements[0];
        assert_eq!((p.image.width, p.image.height), (1, 12), "native size");
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
        assert_eq!((imgs[0].px_w, imgs[0].px_h), (1.0, 6.0));
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
        assert_eq!((t.placements[0].image.width, t.placements[0].image.height), (1, 12));
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
        assert_eq!(t.placements[0].image.width, 2, "BEL was data; both columns drawn");
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
    fn a_sixel_reserves_the_rows_it_declares() {
        // chafa and yazi declare `"1;1;W;H` for a picture with a transparent
        // bottom margin; JeTTY reserved only the rows drawn on, so the text
        // after it landed higher than the encoder meant.
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel("\"1;1;20;40#0;2;100;0;0#0~"));
        assert_eq!((t.placements[0].cols, t.placements[0].rows), (2, 4));
        assert_eq!(t.snapshot().cursor_row, 4, "below the declared 40 px");
    }

    #[test]
    fn a_large_sixel_within_the_pixel_caps_draws() {
        // A dithered full-window frame runs ~1 byte per pixel: 4K (8.3 Mpx) is
        // ~8 MB of sixel, past the old 4 MiB byte cap that dropped it silently
        // though its pixels are within SIXEL_CAPS. Here: 4.2 MB for 3.6 Mpx.
        let mut data = String::from("#0;2;100;0;0#1;2;0;100;0");
        for _ in 0..600 {
            for c in 0..7 {
                data.push_str(&format!("#{}", c % 2));
                data.push_str(&"~".repeat(1000));
                data.push('$');
            }
            data.push('-');
        }
        let seq = sixel(&data);
        assert!(seq.len() > 4 * 1024 * 1024);
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(&seq);
        assert_eq!(t.placements.len(), 1, "drawn");
        assert_eq!((t.placements[0].image.width, t.placements[0].image.height), (1000, 3600));
    }

    /// The absolute line (anchor space) of the row — history or screen — whose
    /// text starts with `marker`.
    fn abs_line_of(t: &Terminal, marker: &str) -> Option<i64> {
        let grid = t.term.grid();
        (grid.topmost_line().0..=grid.bottommost_line().0).find_map(|l| {
            let row = &grid[Line(l)];
            let text: String = (0..marker.len().min(t.cols)).map(|c| row[Column(c)].c).collect();
            (text == marker).then_some(t.abs_top + l as i64)
        })
    }

    #[test]
    fn images_keep_their_rows_across_a_resize() {
        // A window resize, F11, a font zoom or a detach reflows the grid, and it
        // dropped every image, leaving its blank rows behind. kitty, WezTerm
        // and foot keep them; so does JeTTY now, each on its own row.
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"m0\r\n");
        t.feed(&sixel(RED_1X12));
        t.feed(b"m1 ");
        t.feed(&red_rgba_2x2(",c=3,r=2"));
        t.feed(b"\r\n");
        for (cols, rows) in [(30, 10), (12, 5), (40, 3), (20, 8)] {
            t.resize(cols, rows);
            assert_eq!(t.placements.len(), 2, "{cols}x{rows}");
            assert_eq!(t.placements[0].abs_line, abs_line_of(&t, "m0").unwrap() + 1, "{cols}x{rows}");
            assert_eq!(t.placements[1].abs_line, abs_line_of(&t, "m1").unwrap(), "{cols}x{rows}");
            assert_eq!(t.placements[1].col, 3);
        }
        // Wiped by the prompt-scatter fix (a resize at a clean prompt): gone.
        t.feed(b"\x1b[2J\x1b[3J\x1b[H");
        assert!(t.placements.is_empty());
    }

    #[test]
    fn images_never_land_on_another_row_after_a_resize() {
        // Random output — long lines that rewrap, images under marker lines, a
        // small scrollback — through random resizes: every image left is right
        // under its marker. One may be dropped (correct-or-absent), never moved.
        let mut r = Rng(0x1234_5678_9abc_def1);
        let (mut carried, mut dropped) = (0, 0);
        for _ in 0..80 {
            let mut t = Terminal::new(10 + r.below(30), 3 + r.below(10));
            t.set_cell_px(10.0, 10.0);
            t.set_scrollback_lines(r.pick(&[0usize, 4, 20, 1000]));
            let mut k = 0;
            for _ in 0..30 {
                match r.below(5) {
                    0 => {
                        k += 1;
                        t.feed(format!("m{k}:\r\n").as_bytes());
                        t.feed(&red_rgba_2x2(&format!(",i={k},c=2,r={}", 1 + r.below(3))));
                    }
                    1 => {
                        t.feed(&vec![b'x'; r.below(90)]);
                        t.feed(b"\r\n");
                    }
                    2 | 3 => {
                        let before = t.placements.len();
                        t.resize(10 + r.below(30), 3 + r.below(10));
                        carried += t.placements.len();
                        dropped += before - t.placements.len();
                    }
                    _ => t.feed(b"\r\n"),
                }
                for p in &t.placements {
                    let marker = format!("m{}:", p.kitty_id.unwrap());
                    if let Some(line) = abs_line_of(&t, &marker) {
                        assert_eq!(p.abs_line, line + 1, "image {marker} moved");
                    }
                }
            }
        }
        assert!(carried > 4 * dropped, "most images ride a resize: {carried} carried, {dropped} dropped");
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

    /// The viewport's rows as text.
    fn screen_rows(t: &Terminal) -> Vec<String> {
        let snap = t.snapshot();
        (0..t.rows()).map(|r| snap.row_text(r).trim_end().to_string()).collect()
    }

    #[test]
    fn a_pinned_view_keeps_its_lines_while_output_scrolls() {
        // Copy-mode / hint mode pin the view: at the live bottom, output used
        // to scroll the text out from under the copy cursor and the chips.
        // Also with a full scrollback and nothing else anchored, where the
        // count of scrolled lines is otherwise lost.
        for scrollback in [10_000usize, 50] {
            let mut t = Terminal::new(20, 4);
            t.set_scrollback_lines(scrollback);
            for i in 0..80 {
                t.feed(format!("line {i}\r\n").as_bytes());
            }
            let spot = t.view_spot();
            let before = screen_rows(&t);
            let anchor = t.viewport_row_abs(1);
            t.set_view_pinned(true);
            for i in 80..83 {
                t.feed(format!("line {i}\r\n").as_bytes());
            }
            // The cursor's (empty) row took `line 80` before it scrolled.
            assert_eq!(screen_rows(&t)[..3], before[..3], "scrollback {scrollback}: the view stays on its lines");
            assert_eq!(t.scroll_offset(), 3);
            assert_eq!(t.abs_to_buffer_line(anchor), t.viewport_line_to_buffer(1), "the absolute line follows its text");
            t.set_view_pinned(false);
            t.scroll_to_spot(spot);
            assert_eq!(t.scroll_offset(), 0, "back at the live bottom");
            assert_eq!(screen_rows(&t)[2], "line 82");
        }
    }

    #[test]
    fn a_spot_scrolled_back_is_found_again_after_output() {
        let mut t = Terminal::new(20, 4);
        for i in 0..80 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll_lines(30);
        let spot = t.view_spot();
        let before = screen_rows(&t);
        t.feed(b"more\r\nand more\r\n");
        t.scroll_lines(-10);
        t.scroll_to_spot(spot);
        assert_eq!(screen_rows(&t), before, "the same lines, not the same offset");
    }

    #[test]
    fn a_hint_chip_follows_its_text_and_goes_when_the_text_does() {
        let mut t = Terminal::new(30, 4);
        t.feed(b"see /etc/hosts\r\nand /tmp/x\r\n");
        let toks = t.hint_tokens();
        let hosts = toks.iter().find(|h| h.text == "/etc/hosts").unwrap();
        assert_eq!(t.hint_chip_cell(hosts), Some((0, 4)), "where the scan found it");
        // Output scrolls it up a row (the view is not pinned here).
        t.feed(b"x\r\ny\r\n");
        assert_eq!(t.hint_chip_cell(hosts), None, "scrolled off the screen");
        t.scroll_lines(1);
        assert_eq!(t.hint_chip_cell(hosts), Some((0, 4)), "on its text again");
        // Rewritten in place: the chip goes.
        let tmp = toks.iter().find(|h| h.text == "/tmp/x").unwrap();
        t.scroll_to_bottom();
        assert_eq!(t.hint_chip_cell(tmp), Some((0, 4)));
        t.feed(b"\x1b[1;5H/tmp/y");
        assert_eq!(t.hint_chip_cell(tmp), None, "its text was overwritten");
        // A reflow re-anchors the lines.
        let toks = t.hint_tokens();
        t.resize(25, 4);
        assert!(toks.iter().all(|h| t.hint_chip_cell(h).is_none()));
        // A line still being printed when hint mode scanned it: once it goes
        // on, the token under the chip is longer than the one its label copies.
        let mut t = Terminal::new(40, 4);
        t.feed(b"GET https://example.com/page/5");
        let toks = t.hint_tokens();
        assert_eq!(toks[0].text, "https://example.com/page/5");
        assert!(t.hint_chip_cell(&toks[0]).is_some());
        t.feed(b"8\r\n");
        assert_eq!(t.hint_chip_cell(&toks[0]), None, "the text under it is now page/58");
    }

    #[test]
    fn hint_tokens_keep_the_newest_when_capped() {
        // More tokens on screen than the cap (a tall window of `git log`
        // hashes): the cap kept the 100 top-most, so the newest rows next to
        // the prompt — the usual target — got no label. A duplicate is
        // labelled where it last appears, and labels still go in reading order.
        let mut t = Terminal::new(40, 130);
        for i in 0..120 {
            t.feed(format!("deadbeef{i:04}\r\n").as_bytes());
        }
        t.feed(b"/tmp/dup\r\n...\r\n/tmp/dup");
        let toks = t.hint_tokens();
        assert_eq!(toks.len(), 100);
        let texts: Vec<&str> = toks.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts[0], "deadbeef0021", "the oldest kept: {texts:?}");
        assert_eq!(texts[98], "deadbeef0119");
        assert_eq!(texts[99], "/tmp/dup");
        assert_eq!(toks[99].spans, vec![(122, 0, 7)], "the duplicate on the last row");
        assert!(toks.windows(2).all(|w| w[0].spans[0] < w[1].spans[0]), "reading order");
    }

    #[test]
    fn viewport_rows_chars_marks_wide_char_spacers() {
        let mut t = Terminal::new(8, 2);
        t.feed("a世b".as_bytes());
        assert_eq!(&t.viewport_rows_chars()[0][..4], &['a', '世', WIDE_SPACER, 'b']);
    }

    #[test]
    fn viewport_rows_chars_matches_snapshot_row_text() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"alpha\r\nbeta\r\n");
        let rows = t.viewport_rows_chars();
        let snap = t.snapshot();
        assert_eq!(rows.len(), 4);
        for (r, row) in rows.iter().enumerate() {
            let s: String = row.iter().collect();
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
        assert_eq!((p.image.width, p.image.height), (2, 2));
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

    /// Feed a `w`×`h` opaque Kitty image with the extra control `keys` to a
    /// fresh `cols`-wide terminal of 10×20 px cells; return its one placement
    /// as drawn (footprint, then `[x, y, w, h]` in px).
    fn kitty_drawn(cols: usize, w: u32, h: u32, keys: &str) -> ((u16, u16), [f32; 4]) {
        let mut t = Terminal::new(cols, 30);
        t.set_cell_px(10.0, 20.0);
        let px = b64(&[9u8, 9, 9, 255].repeat((w * h) as usize));
        t.feed(&apc(&format!("a=T,f=32,s={w},v={h},C=1{keys};{px}")));
        let v = t.visible_images()[0];
        ((v.cols, v.rows), [v.px_x, v.px_y, v.px_w, v.px_h])
    }

    #[test]
    fn kitty_c_and_r_scale_the_image_keeping_its_shape() {
        // A 400×100 image in a `c=10,r=10` box of 10×20 px cells (100×200 px)
        // drew at 100×100, squashed 4× sideways; a lone `c=` or `r=` was
        // ignored. The spec: the image is scaled to fill the area, one of the
        // two given makes the other follow its aspect ratio, and with both it
        // is letterboxed.
        assert_eq!(kitty_drawn(40, 400, 100, ",c=10,r=10"), ((10, 10), [0.0, 87.5, 100.0, 25.0]), "letterboxed");
        assert_eq!(kitty_drawn(40, 400, 100, ",c=20"), ((20, 3), [0.0, 0.0, 200.0, 50.0]), "rows follow");
        assert_eq!(kitty_drawn(40, 400, 100, ",r=2"), ((16, 2), [0.0, 0.0, 160.0, 40.0]), "columns follow");
        assert_eq!(kitty_drawn(40, 2, 2, ",c=4,r=2"), ((4, 2), [0.0, 0.0, 40.0, 40.0]), "scaled up to fit");
        // Wider than the grid: native size, cut at the grid's edge when drawn
        // (it was squashed into the grid's width at its full height).
        assert_eq!(kitty_drawn(30, 400, 100, ""), ((30, 5), [0.0, 0.0, 400.0, 100.0]), "not squashed");
        // Taller than the most rows an image reserves: shrunk to fit them.
        let ((_, rows), [.., w, h]) = kitty_drawn(40, 2, 2, ",r=4000");
        assert_eq!((rows, w, h), (MAX_IMAGE_ROWS as u16, 20480.0, 20480.0));
    }

    #[test]
    fn an_image_scales_with_the_cell_size_when_the_grid_keeps_its_size() {
        // A window moved to a monitor of another scale keeps its grid and gets
        // twice (or half) the cell size: its images kept their pixel size and
        // painted over twice the rows they reserve (or shrank to half).
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X12)); // 1×12 px: 1 column, 2 rows
        t.feed(&red_rgba_2x2(",c=2,r=1"));
        t.set_cell_px(20.0, 20.0);
        let imgs = t.visible_images();
        assert_eq!((imgs[0].px_w, imgs[0].px_h, imgs[0].rows), (2.0, 24.0, 2));
        assert_eq!((imgs[1].px_x, imgs[1].px_w, imgs[1].px_h), (10.0, 20.0, 20.0), "c=2,r=1 still fills its box");
    }

    #[test]
    fn a_sixel_starts_at_the_cursor_column_and_leaves_the_cursor_under_it() {
        // `printf 'Plot: '; gnuplot` drew the plot over "Plot: " at column 0,
        // and timg's grid (`CSI n C` before each image) stacked every image
        // in column 0. xterm and WezTerm anchor a sixel at the cursor and
        // put the cursor under the image's left edge.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"Plot: ");
        t.feed(&sixel(RED_1X12)); // 1 column, 2 rows
        assert_eq!((t.placements[0].abs_line, t.placements[0].col), (0, 6));
        let snap = t.snapshot();
        assert_eq!((snap.cursor_row, snap.cursor_col), (2, 6), "under the image's left edge");
        assert!(screen_has(&t, "Plot: "));
        // Wider than the columns left: it stays at the cursor, cut at the edge.
        t.feed(b"\x1b[5;18H");
        t.feed(&sixel("#0;2;100;0;0#0!50~")); // 5 columns from column 17
        let p = t.placements.back().unwrap();
        assert_eq!((p.col, p.cols), (17, 3), "anchored at the cursor, clipped to the grid");
        assert_eq!(t.snapshot().cursor_col, 17);
    }

    #[test]
    fn a_kitty_image_leaves_the_cursor_right_of_its_last_row() {
        // kitty moves the cursor right by the image's columns and down to its
        // last row. icat and timg print a newline after an image, so JeTTY
        // (column 0 BELOW the image) showed a blank row under every one, and
        // timg's grid (up by the image's rows, then right) stepped down a row
        // per column.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"ab");
        t.feed(&red_rgba_2x2(",c=3,r=2")); // 3×2 cells at (0, 2)
        let snap = t.snapshot();
        assert_eq!((snap.cursor_row, snap.cursor_col), (1, 5), "right of its last row");
        t.feed(b"\r\n\x1b[2A\x1b[6C");
        t.feed(&red_rgba_2x2(",c=3,r=2")); // timg's next grid column
        assert_eq!(t.placements[1].abs_line, 0, "level with the first one");
        // At the right edge the wrap is pending, as for text in the last column.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[1;18H");
        t.feed(&red_rgba_2x2(",c=3,r=1"));
        let snap = t.snapshot();
        assert_eq!((snap.cursor_row, snap.cursor_col), (0, 19));
        t.feed(b"x");
        assert_eq!(t.snapshot().row_text(1).trim_end(), "x", "the next character wraps");
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
    fn a_kitty_apc_ends_at_bel_and_8bit_st_for_vte_too() {
        // kitty ends an APC at BEL; vte's APC string ends only at ESC / CAN /
        // SUB. A BEL-terminated image was dropped and an 8-bit-ST-terminated
        // one drawn — and either way the output after it was swallowed up to
        // the next ESC.
        let px = b64(&[255u8, 0, 0, 255].repeat(4));
        for end in [0x07u8, 0x9c] {
            let mut seq = format!("\x1b_Ga=T,f=32,s=2,v=2;{px}").into_bytes();
            seq.push(end);
            seq.extend_from_slice(b"after");
            for cut in 0..=seq.len() {
                let mut t = Terminal::new(20, 5);
                t.set_cell_px(10.0, 10.0);
                t.feed(&seq[..cut]);
                t.feed(&seq[cut..]);
                assert_eq!(t.placements.len(), 1, "{end:#04x}, cut at {cut}: the image");
                assert!(screen_has(&t, "after"), "{end:#04x}, cut at {cut}: the output after it");
            }
        }
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
        assert_eq!((t.placements[0].image.width, t.placements[0].image.height), (2, 2));
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
    fn kitty_put_costs_nothing_per_pixel_of_the_stored_image() {
        // Every `a=p` re-hashed the whole stored RGBA for the placement id: a
        // 20-byte put of a 4000×4000 image cost ~50 ms on the UI thread, so a
        // few hundred KB of puts froze JeTTY for minutes (image.nvim re-puts on
        // every redraw paid it too). The id is the stored image's own.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let px = [9u8, 8, 7, 255].repeat(512 * 512); // 1 MB of RGBA
        t.feed(&apc(&format!("a=t,f=32,s=512,v=512,i=1,q=2;{}", b64(&px))));
        let puts = apc("a=p,i=1,C=1,q=2").repeat(1000);
        let started = std::time::Instant::now();
        t.feed(&puts);
        let took = started.elapsed();
        assert!(took.as_millis() < 500, "1000 puts took {took:?}");
        assert_eq!(t.placements.len(), 1, "each put replaced the one it covers");
        let stored = &t.kitty_images[0];
        assert!(Arc::ptr_eq(&t.placements[0].image, &stored.image), "the stored image, not a copy");
        assert_eq!(t.placements[0].id, stored.content, "under the id it was stored with");
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
    fn a_kitty_delete_touches_only_its_own_screen() {
        // mpv --vo=kitty sends `a=d` on the alternate screen, and again before
        // it leaves it: every Kitty image of the shell went with it, and a
        // `d=A` there freed the shell's stored images too. kitty keeps a set
        // of images and placements per screen.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=1"));
        t.feed(&apc(&format!("a=t,f=32,s=1,v=1,i=7;{}", b64(&[1, 2, 3, 255]))));
        t.feed(b"\x1b[?1049h");
        t.feed(&red_rgba_2x2(",i=1"));
        t.feed(&apc("a=d"));
        assert!(t.alt_placements.is_empty(), "the alternate screen's image is deleted");
        t.feed(&apc("a=d,d=A"));
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.placements.len(), 1, "the shell's image survives");
        t.feed(&apc("a=p,i=7,C=1"));
        assert_eq!(t.placements.len(), 2, "and so does the image it stored");
    }

    #[test]
    fn a_kitty_clear_keeps_the_scrollback_and_p_picks_one_placement() {
        // `kitten icat --clear` (`d=A`) deleted every Kitty image in the
        // scrollback too; the spec deletes those visible on screen. And
        // `d=i,i=N,p=P` deleted every placement of N.
        let mut t = Terminal::new(20, 4);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=1"));
        t.feed(&b"\r\n".repeat(8)); // into the scrollback
        t.feed(&red_rgba_2x2(",i=2"));
        t.feed(&apc("a=d,d=A"));
        assert_eq!(t.placements.len(), 1);
        assert_eq!(t.placements[0].kitty_id, Some(1), "the scrolled-off image stays");
        t.feed(&red_rgba_2x2(",i=3,p=1"));
        t.feed(b"\r\n");
        t.feed(&apc("a=p,i=3,p=2"));
        t.feed(&apc("a=d,d=i,i=3,p=1"));
        let left: Vec<_> = t.placements.iter().map(|p| (p.kitty_id, p.kitty_placement)).collect();
        assert_eq!(left, [(Some(1), None), (Some(3), Some(2))], "only placement 1 of image 3");
    }

    #[test]
    fn re_transmitting_an_image_deletes_its_placements() {
        // The spec: "When re-transmitting image data for a specific id, the
        // existing image and all its placements must be deleted." The old one
        // stayed on screen as a ghost.
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=1"));
        t.feed(b"\x1b[5;1H");
        t.feed(&red_rgba_2x2(",i=1"));
        assert_eq!(t.placements.len(), 1, "a=T again");
        assert_eq!(t.placements[0].abs_line, 4);
        t.feed(&apc(&format!("a=t,f=32,s=1,v=1,i=1;{}", b64(&[1, 2, 3, 255]))));
        assert!(t.placements.is_empty(), "a=t again");
    }

    #[test]
    fn kitty_replies_name_the_image_number_and_placement() {
        // `a=T,I=13` answered `I=13;OK`: the spec's answer names the id the
        // image got (`i=99,I=13;OK`), and kitty's adds the placement id. `i=`
        // and `I=` shared one key space: `I=5` replaced image `i=5`.
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        let reply = |t: &mut Terminal, keys: &str| {
            t.feed(&red_rgba_2x2(keys));
            String::from_utf8(t.drain_pty_writes()).unwrap()
        };
        assert_eq!(reply(&mut t, ",I=13"), "\x1b_Gi=1,I=13;OK\x1b\\", "the id it got");
        assert_eq!(reply(&mut t, ",i=5,p=7"), "\x1b_Gi=5,p=7;OK\x1b\\");
        assert_eq!(reply(&mut t, ",I=5"), "\x1b_Gi=2,I=5;OK\x1b\\", "a number is not an id");
        assert_eq!(reply(&mut t, ",I=13"), "\x1b_Gi=3,I=13;OK\x1b\\", "a new image per transmit");
        t.feed(&apc("a=p,I=13,p=4,C=1"));
        assert_eq!(String::from_utf8(t.drain_pty_writes()).unwrap(), "\x1b_Gi=3,I=13,p=4;OK\x1b\\", "the newest");
        assert_eq!(reply(&mut t, ",i=6,I=6"), "\x1b_Gi=6,I=6;EINVAL\x1b\\", "both is an error");
        assert_eq!(t.placements.iter().filter(|p| p.kitty_id == Some(5)).count(), 1);
        t.feed(&apc("a=d,d=n,I=13"));
        assert!(t.placements.iter().all(|p| p.kitty_id != Some(3)), "d=n: the newest image numbered 13");
        assert!(t.placements.iter().any(|p| p.kitty_id == Some(1)));
    }

    #[test]
    fn the_kitty_registry_keeps_the_images_in_use() {
        // A TUI re-puts an icon on every redraw while transmitting thumbnails:
        // the registry evicted in transmit order, so the icon went first and
        // its next (quiet) put showed nothing.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        let px = b64(&[1u8, 2, 3, 255]);
        let send = |t: &mut Terminal, id: usize| t.feed(&apc(&format!("a=t,f=32,s=1,v=1,i={id},q=2;{px}")));
        send(&mut t, 1);
        t.feed(&apc("a=p,i=1,C=1,q=2"));
        for id in 2..=MAX_KITTY_STORED + 10 {
            send(&mut t, id);
        }
        assert!(t.kitty_images.iter().any(|e| e.id == 1), "an image on screen is kept");
        // Unshown, the least recently used one goes first.
        t.feed(&apc("a=d,d=a"));
        t.feed(&apc("a=p,i=1,C=1,q=2"));
        t.feed(&apc("a=d,d=a"));
        for id in 100..110 {
            send(&mut t, id);
        }
        assert!(t.kitty_images.iter().any(|e| e.id == 1), "a recently put image is kept");
        assert_eq!(t.kitty_images.len(), MAX_KITTY_STORED);
    }

    #[test]
    fn kitty_replies_keep_their_place_in_a_synchronized_update() {
        // vte holds the DA1 reply until the update ends; the Kitty reply
        // jumped ahead of it.
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?2026h\x1b[c");
        t.feed(&apc("a=q,i=1,s=1,v=1,f=24;AAAA"));
        let replies = String::from_utf8(t.drain_pty_writes()).unwrap();
        assert!(replies.starts_with("\x1b[?62;"), "DA1 first: {replies:?}");
        assert!(replies.ends_with("\x1b_Gi=1;OK\x1b\\"), "{replies:?}");
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

    // ── image work: what images cost the UI thread ────────────────────────────

    /// A 1-bit grayscale PNG of `w`×`h` black pixels: a few hundred bytes that
    /// decode to `w * h * 4` bytes of RGBA.
    fn black_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&vec![0u8; w.div_ceil(8) as usize * h as usize]).unwrap();
        }
        out
    }

    /// A sixel of `bands` bands, each one `!1000~` run: ~10 bytes per 24,000
    /// bytes of RGBA.
    fn sixel_bomb(bands: usize) -> Vec<u8> {
        sixel(&format!("#0;2;100;0;0{}", "#0!1000~-".repeat(bands)))
    }

    #[test]
    fn work_nobody_can_receive_is_not_done() {
        // An unaddressed `a=q` (or one at `q=2`) has no reply to send, and an
        // anonymous `a=t` can never be put: they used to inflate and decode
        // anyway — a 62 KB zlib query unpacked 64 MB for nothing.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        let z = b64(&miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 256 * 256 * 4], 6));
        for control in ["a=q", "a=q,i=4,q=2", "a=t", "f=32"] {
            t.feed(&apc(&format!("{control},f=32,o=z,s=256,v=256;{z}")));
            assert_eq!(t.image_work, IMAGE_WORK_MAX, "{control}: nothing paid, nothing decoded");
        }
        assert!(t.drain_pty_writes().is_empty() && t.placements.is_empty() && t.kitty_images.is_empty());
        // A query someone receives is still answered from a real decode.
        t.feed(&apc(&format!("a=q,i=4,f=32,o=z,s=256,v=256;{z}")));
        assert_eq!(String::from_utf8_lossy(&t.drain_pty_writes()), "\x1b_Gi=4;OK\x1b\\");
        t.feed(&apc(&format!("a=q,i=4,f=32,o=z,s=256,v=255;{z}")));
        assert_eq!(String::from_utf8_lossy(&t.drain_pty_writes()), "\x1b_Gi=4;EBADF\x1b\\");
    }

    #[test]
    fn image_bombs_are_paid_for_or_dropped() {
        // A few hundred bytes of sixel `!` repeats, PNG or zlib unpack to
        // hundreds of KB here (64 MB at full size), on the UI thread: a 2 MiB
        // stream of them froze JeTTY for tens of seconds. Each image is paid
        // for from the image-work bank before it allocates; output refills it.
        let mut t = Terminal::new(40, 5);
        t.set_cell_px(10.0, 10.0);
        t.image_work = 0; // as after a flood
        t.feed(&sixel_bomb(20)); // 1000×120 px
        assert!(t.placements.is_empty(), "a sixel bomb the bank cannot pay for is dropped");
        t.feed(&sixel(RED_1X12));
        assert_eq!(t.placements.len(), 1, "an image its own bytes pay for still draws");
        let png = b64(&black_png(256, 256));
        t.feed(&apc(&format!("a=T,f=100,i=5;{png}")));
        let z = b64(&miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 256 * 256 * 4], 6));
        t.feed(&apc(&format!("a=T,f=32,o=z,s=256,v=256,i=6;{z}")));
        assert_eq!(
            String::from_utf8_lossy(&t.drain_pty_writes()),
            "\x1b_Gi=5;EBUSY\x1b\\\x1b_Gi=6;EBUSY\x1b\\",
            "Kitty bombs are refused, saying why"
        );
        assert!(t.kitty_images.is_empty() && t.placements.len() == 1);
        // Output earns the work back (64 bytes per byte): the same images fit.
        t.feed(&b"\r\n".repeat(16 * 1024));
        let before = t.placements.len();
        t.feed(&sixel_bomb(20));
        t.feed(&apc(&format!("a=T,f=100,i=5,q=1;{png}")));
        t.feed(&apc(&format!("a=T,f=32,o=z,s=256,v=256,i=6,q=1;{z}")));
        assert_eq!(t.placements.len(), before + 3, "all three placed once the bank holds enough");
        assert!(t.drain_pty_writes().is_empty(), "no errors");
        assert!(t.image_work < IMAGE_WORK_MAX, "and paid for");
    }

    #[test]
    fn the_rows_an_image_reserves_are_paid_for() {
        // On the primary screen an image line-feeds the rows it reserves: a
        // 24-byte `a=p` with `r=1024` fed 1,024 lines, and 15,000 of them
        // (360 KB) kept the UI thread busy for seconds.
        let mut t = Terminal::new(80, 24);
        t.set_cell_px(10.0, 10.0);
        t.feed(&apc(&format!("a=t,f=32,s=1,v=1,i=1,q=2;{}", b64(&[1, 2, 3, 255]))));
        let flood = apc("a=p,i=1,c=1,r=1024,q=2").repeat(15_000);
        let top = t.abs_top;
        t.feed(&flood);
        // Each line fed writes a row of cells, paid from the bank and what the
        // flood itself earned.
        let row = 80 * std::mem::size_of::<alacritty_terminal::term::cell::Cell>() as u64;
        let earned = IMAGE_WORK_MAX + flood.len() as u64 * IMAGE_WORK_PER_BYTE;
        let fed = (t.abs_top - top) as u64;
        assert!(fed <= earned / row, "fed {fed} lines");
        assert!(fed > 50_000, "puts it can pay for still scroll: {fed}");
        let reply = String::from_utf8_lossy(&{
            t.feed(&apc("a=p,i=1,c=1,r=1024"));
            t.drain_pty_writes()
        })
        .to_string();
        assert_eq!(reply, "\x1b_Gi=1;EBUSY\x1b\\", "a put the bank cannot pay for says so");
    }

    #[test]
    fn a_full_bank_bounds_a_bomb_flood() {
        // From a full bank, a stream of bombs decodes what the bank and the
        // stream's own bytes pay for, then drops the rest.
        let mut t = Terminal::new(40, 5);
        t.set_cell_px(10.0, 10.0);
        t.image_work = 3 * 1000 * 120 * 4; // three of them
        let bomb = sixel_bomb(20);
        let stream = bomb.repeat(10);
        t.feed(&stream);
        let decoded = t.placements.iter().filter(|p| p.image.width == 1000).count();
        assert_eq!(decoded, 3, "the bank, plus {} bytes earned", stream.len() as u64 * IMAGE_WORK_PER_BYTE);
    }

    #[test]
    fn image_ids_follow_the_input() {
        // The texture-cache key hashes what was received, so a re-sent frame
        // reuses its texture; the same bytes under another geometry or
        // protocol are another image.
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        t.feed(&sixel(RED_1X6));
        t.feed(&sixel(RED_1X6));
        t.feed(&sixel("#0;2;100;0;1#0~"));
        let ids: Vec<u64> = t.placements.iter().map(|p| p.id).collect();
        assert_eq!(ids[0], ids[1], "the same sixel twice");
        assert_ne!(ids[0], ids[2], "another color");
        let px = b64(&[255u8, 0, 0, 255].repeat(4));
        t.feed(&apc(&format!("a=T,f=32,s=2,v=2;{px}")));
        t.feed(&apc(&format!("a=T,f=32,s=4,v=1;{px}")));
        let n = t.placements.len();
        assert_ne!(t.placements[n - 1].id, t.placements[n - 2].id, "the same pixels at another size");
    }

    // ── OSC size cap ──────────────────────────────────────────────────────────

    /// Resident set size of this process in bytes (Linux; 0 elsewhere).
    fn rss_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()))
            .map_or(0, |pages| pages * 4096)
    }

    #[test]
    fn unterminated_osc_flood_is_bounded_and_leaks_no_text() {
        // `printf '\e]0;'; base64 /dev/urandom` used to grow vte's OSC buffer
        // without bound. 300 MiB streamed in PTY-sized chunks must stay bounded,
        // print none of it, and the terminal must recover at the terminator.
        let mut t = Terminal::new(40, 5);
        t.feed(b"\x1b]0;");
        let chunk = vec![b'A'; 64 * 1024];
        let before = rss_bytes();
        for _ in 0..(300 * 16) {
            t.feed(&chunk);
        }
        let grown = rss_bytes().saturating_sub(before);
        assert!(grown < 64 * 1024 * 1024, "RSS grew by {grown} bytes");
        assert_eq!(t.scan, Scan::OscDiscard, "the overrun is being discarded");
        t.feed(b"\x07ok");
        let snap = t.snapshot();
        assert!(snap.row_text(0).starts_with("ok"), "recovers: {:?}", snap.row_text(0));
        assert!((0..snap.rows).all(|r| !snap.row_text(r).contains('A')), "no payload leaked");
    }

    #[test]
    fn osc_overrun_split_across_feeds_resyncs_on_st() {
        let mut t = Terminal::new(40, 5);
        let mut payload = b"\x1b]2;".to_vec();
        payload.extend(std::iter::repeat_n(b'z', OSC_MAX_BYTES as usize + 10));
        let (a, b) = payload.split_at(payload.len() / 2);
        t.feed(a);
        t.feed(b);
        t.feed(b"zzz\x1b\\after"); // terminated by a 7-bit ST
        assert_eq!(t.snapshot().row_text(0).trim_end(), "after");
        // A normal OSC right after still works.
        t.feed(b"\x1b]0;fine\x07");
        assert_eq!(t.take_title_update(), Some(Some("fine".to_string())));
    }

    #[test]
    fn overrunning_osc133_binds_no_mark() {
        let mut t = Terminal::new(40, 5);
        let mut bytes = b"\x1b]133;A".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', OSC_MAX_BYTES as usize + 5));
        bytes.push(0x07);
        t.feed(&bytes);
        assert!(t.marks.is_empty(), "a garbage-sized 133 never binds");
        assert_eq!(t.prompt_count(), 0);
        assert!(t.snapshot().row_text(0).trim().is_empty(), "nothing printed");
    }

    // ── images: alt screen, sync, stacking, chunking, cancel ──────────────────

    #[test]
    fn sixel_on_alt_screen_anchors_at_cursor_and_goes_with_the_screen() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h\x1b[3;5H"); // alt screen, cursor row 2 col 4
        t.feed(&sixel(RED_1X12)); // 2 rows
        let imgs = t.visible_images();
        assert_eq!(imgs.len(), 1, "a TUI's sixel preview is shown");
        assert_eq!((imgs[0].top_row, imgs[0].col), (2.0, 4));
        assert_eq!(t.snapshot().cursor_row, 4, "cursor below the image, nothing scrolled");
        t.feed(b"\x1b[?1049l");
        assert!(t.visible_images().is_empty() && t.alt_placements.is_empty());
    }

    #[test]
    fn alt_sixel_vanishes_once_a_covered_cell_is_written() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h\x1b[3;5H");
        t.feed(&sixel(RED_1X12)); // rows 2-3, col 4
        t.feed(b"\x1b[6;1Hzz"); // text elsewhere
        t.feed(b"\x1b[4;5H\x1b[1;1H"); // the cursor passes over it, writes nothing
        assert_eq!(t.visible_images().len(), 1, "unrelated text / cursor moves keep it");
        t.feed(b"\x1b[4;5H "); // yazi's erase: a space over an identical blank cell
        assert!(t.visible_images().is_empty(), "overwritten sixel pixels are gone");
    }

    #[test]
    fn kitty_alt_cursor_moves_past_the_image_even_in_origin_mode() {
        let mut t = Terminal::new(20, 8);
        t.set_cell_px(10.0, 10.0);
        // Scroll region rows 3-6 + DECOM: `CUP 1;1` is screen row 2.
        t.feed(b"\x1b[?1049h\x1b[3;6r\x1b[?6h\x1b[1;1H");
        t.feed(&red_rgba_2x2(",i=4")); // one cell
        assert_eq!(t.visible_images()[0].top_row, 2.0);
        let snap = t.snapshot();
        assert_eq!((snap.cursor_row, snap.cursor_col), (2, 1), "past its last column, same row");
    }

    #[test]
    fn kitty_alt_c1_keeps_the_cursor_and_delete_clears() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h\x1b[2;3H");
        t.feed(&red_rgba_2x2(",i=4,C=1"));
        let imgs = t.visible_images();
        assert_eq!(imgs.len(), 1);
        assert_eq!((imgs[0].top_row, imgs[0].col), (1.0, 2));
        let snap = t.snapshot();
        assert_eq!((snap.cursor_row, snap.cursor_col), (1, 2), "C=1: cursor untouched");
        t.feed(&apc("a=d,d=i,i=4"));
        assert!(t.visible_images().is_empty(), "a=d deletes alt-screen placements too");
    }

    #[test]
    fn kitty_alt_placement_survives_text_but_not_clear_or_scroll() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?1049h\x1b[2;3H");
        t.feed(&red_rgba_2x2(",i=4,C=1"));
        t.feed(b"\x1b[5;1Hfile list redraw");
        assert_eq!(t.visible_images().len(), 1, "partial redraws keep the preview");
        t.feed(b"\x1b[2J");
        assert!(t.visible_images().is_empty(), "a cleared screen drops it");
        t.feed(b"\x1b[2;3H");
        t.feed(&red_rgba_2x2(",i=4,C=1"));
        t.feed(b"\x1b[S");
        assert!(t.visible_images().is_empty(), "a scroll drops it (correct-or-absent)");
    }

    #[test]
    fn images_inside_a_sync_block_flush_and_anchor_correctly() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"\x1b[?2026h"); // BSU: vte buffers the next bytes
        t.feed(b"a\r\nb\r\n");
        t.feed(&sixel(RED_1X6));
        assert_eq!(t.placements.len(), 1, "placed, not dropped");
        assert_eq!(t.visible_images()[0].top_row, 2.0, "at the row the app drew it");
        t.feed(b"\x1b[?2026h");
        t.feed(&red_rgba_2x2(""));
        assert_eq!(t.placements.len(), 2);
        assert!(t.sync_deadline().is_none());
    }

    #[test]
    fn an_image_follows_the_lines_after_an_alt_exit_inside_a_sync_block() {
        // An editor quit (rmcup inside BSU/ESU) and the shell's lines in one
        // read: the image scrolls with them instead of staying on its old row.
        let mut t = Terminal::new(20, 5);
        t.set_cell_px(10.0, 10.0);
        t.feed(b"1\r\n2\r\n");
        t.feed(&sixel(RED_1X6)); // row 2; the cursor moves below it
        t.feed(b"\x1b[?1049htui");
        t.feed(b"\x1b[?2026h\x1b[?1049l\x1b[?2026lx\r\ny\r\n");
        assert_eq!(t.visible_images()[0].top_row, 1.0, "moved up with the one scrolled line");
    }

    #[test]
    fn in_place_redraws_keep_one_placement() {
        // chafa / timg animate by re-emitting each frame at the same spot.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        for _ in 0..300 {
            t.feed(b"\x1b[H");
            t.feed(&sixel(RED_1X12));
        }
        assert_eq!(t.placements.len(), 1, "each frame evicts the one it covers");
        // Kitty: the same image id + placement id replaces even when it moves
        // (the 1-cell image at another spot covers nothing).
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        for k in 0..300 {
            t.feed(format!("\x1b[1;{}H", 1 + k % 10).as_bytes());
            t.feed(&red_rgba_2x2(",i=1,p=1"));
        }
        assert_eq!(t.placements.len(), 1, "same image id + placement id replaces");
        assert_eq!(t.visible_images()[0].col, 299 % 10);
        t.feed(b"\x1b[3;1H");
        t.feed(&apc("a=p,i=1,p=2"));
        assert_eq!(t.placements.len(), 2, "another placement id is a second placement");
    }

    #[test]
    fn kitty_spec_chunking_without_action_transmits_only() {
        // The spec's chunked example opens WITHOUT `a=`: the default is `t`.
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        let payload = b64(&[9u8, 9, 9, 255].repeat(4));
        let (a, b) = payload.split_at(payload.len() / 2);
        t.feed(&apc(&format!("f=32,s=2,v=2,i=5,m=1;{a}")));
        t.feed(&apc(&format!("m=0;{b}")));
        assert!(t.placements.is_empty(), "transmit only");
        assert_eq!(t.kitty_images.len(), 1, "stored under i=5");
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("i=5;OK"), "got {reply:?}");
        t.feed(&apc("a=p,i=5"));
        assert_eq!(t.placements.len(), 1, "a=p displays it");
    }

    #[test]
    fn kitty_virtual_placement_is_refused() {
        let mut t = Terminal::new(20, 6);
        t.set_cell_px(10.0, 10.0);
        t.feed(&red_rgba_2x2(",i=7,U=1"));
        assert!(t.placements.is_empty(), "no placeholder-cell rendering");
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert!(reply.contains("ENOTSUPP"), "got {reply:?}");
    }

    #[test]
    fn images_split_anywhere_draw_the_same() {
        // A PTY read can end at any byte of an image. Every split — and one
        // byte per read — must place the same image and leave vte where it
        // was: the text after it prints. A CAN / SUB anywhere in it draws
        // nothing and leaves the terminal ready for the next one.
        let px = b64(&[10u8, 20, 30, 255].repeat(6));
        let third = px.len() / 3 / 4 * 4; // chunks split base64 at a multiple of 4
        let mut kitty = apc(&format!("a=T,f=32,s=3,v=2,i=3,q=2,m=1;{}", &px[..third]));
        kitty.extend(apc(&format!("m=1;{}", &px[third..2 * third])));
        kitty.extend(apc(&format!("m=0;{}", &px[2 * third..])));
        let drawn = |t: &Terminal| -> Vec<(i64, u16, u16, u16, u64)> {
            t.placements.iter().map(|p| (p.abs_line, p.col, p.cols, p.rows, p.id)).collect()
        };
        for image in [sixel(RED_1X12), kitty] {
            let mut seq = b"ab".to_vec();
            seq.extend_from_slice(&image);
            seq.extend_from_slice(b"after");
            let mut whole = Terminal::new(20, 6);
            whole.set_cell_px(10.0, 10.0);
            whole.feed(&seq);
            let want = (drawn(&whole), whole.snapshot().row_text(2), whole.cursor_viewport_cell());
            assert_eq!(want.0.len(), 1);
            assert!(screen_has(&whole, "after"));
            let mut bytewise = Terminal::new(20, 6);
            bytewise.set_cell_px(10.0, 10.0);
            for b in &seq {
                bytewise.feed(std::slice::from_ref(b));
            }
            assert_eq!((drawn(&bytewise), bytewise.snapshot().row_text(2), bytewise.cursor_viewport_cell()), want);
            for cut in 0..=seq.len() {
                let mut t = Terminal::new(20, 6);
                t.set_cell_px(10.0, 10.0);
                t.feed(&seq[..cut]);
                t.feed(&seq[cut..]);
                assert_eq!((drawn(&t), t.snapshot().row_text(2), t.cursor_viewport_cell()), want, "cut at {cut}");
            }
            // Cancelled anywhere inside it (after its introducer).
            for at in 4..2 + image.len() - 2 {
                for cancel in [0x18u8, 0x1a] {
                    let mut t = Terminal::new(20, 6);
                    t.set_cell_px(10.0, 10.0);
                    let mut bytes = seq[..at].to_vec();
                    bytes.push(cancel);
                    bytes.extend_from_slice(b"ok");
                    t.feed(&bytes);
                    assert!(t.placements.is_empty(), "cancelled at {at}");
                    assert!(screen_has(&t, "ok"), "the terminal reads on after a cancel at {at}");
                    t.feed(&sixel(RED_1X6));
                    assert_eq!(t.placements.len(), 1, "and draws the next image");
                }
            }
        }
    }

    #[test]
    fn sixel_cancelled_by_can_or_sub_draws_nothing() {
        for cancel in [0x18u8, 0x1a] {
            let mut t = Terminal::new(20, 6);
            t.set_cell_px(10.0, 10.0);
            let mut bytes = b"\x1bPq".to_vec();
            bytes.extend_from_slice(RED_1X6.as_bytes());
            bytes.push(cancel);
            t.feed(&bytes);
            assert!(t.placements.is_empty(), "cancelled with {cancel:#04x}");
        }
    }

    // ── combining marks / zero-width chars ────────────────────────────────────

    #[test]
    fn graphemes_carry_combining_marks_sparsely() {
        let mut t = Terminal::new(20, 3);
        t.feed(b"\x1b]8;;https://x.test\x1b\\link\x1b]8;;\x1b\\\x1b[58:5:1m\x1b[4mu\x1b[m\r\n");
        assert!(t.snapshot().graphemes.is_empty(), "nothing composed on plain / linked text");
        t.feed("e\u{301}x \u{2764}\u{fe0f} \u{1f469}\u{200d}\u{1f4bb}".as_bytes());
        let g = t.snapshot().graphemes;
        assert!(g.iter().any(|c| (c.row, c.col) == (1, 0) && c.text == "e\u{301}"), "{g:?}");
        assert!(g.iter().any(|c| c.text == "\u{2764}\u{fe0f}"), "VS16: {g:?}");
        assert!(g.iter().any(|c| c.text == "\u{1f469}\u{200d}"), "ZWJ: {g:?}");
    }

    #[test]
    fn zalgo_flood_is_capped_on_the_grid_and_in_the_snapshot() {
        let mut t = Terminal::new(20, 3);
        t.feed(b"a");
        let marks = "\u{301}".repeat(100_000);
        for chunk in marks.as_bytes().chunks(8192) {
            t.feed(chunk);
        }
        let cell_marks = t.term.grid()[Line(0)][Column(0)].zerowidth().map_or(0, |z| z.len());
        assert!(cell_marks <= GRAPHEME_MAX_MARKS, "grid cell holds {cell_marks} marks");
        let g = t.snapshot().graphemes;
        assert_eq!(g.len(), 1);
        assert!(g[0].text.chars().count() <= 1 + GRAPHEME_MAX_MARKS);
        assert!(g[0].text.len() <= GRAPHEME_MAX_BYTES);
    }

    // ── differential fuzz: the pre-scanner vs vte 0.15's real state machine ───

    /// vte 0.15's parser states (`vte/src/lib.rs`, std build).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum VState {
        Ground,
        Escape,
        EscInter,
        CsiEntry,
        CsiParam,
        CsiInter,
        CsiIgnore,
        DcsEntry,
        DcsParam,
        DcsInter,
        DcsPass,
        DcsIgnore,
        Osc,
        SosPmApc,
    }

    /// A transcription of vte 0.15's `advance_*` transition functions, tracking
    /// the state and how many bytes its (std, unbounded) OSC buffer holds. In
    /// Ground vte only ever leaves on ESC (UTF-8 decoding never swallows one).
    struct VteModel {
        state: VState,
        osc_len: usize,
        max_osc: usize,
    }

    impl VteModel {
        fn anywhere(&self, b: u8) -> VState {
            match b {
                0x18 | 0x1a => VState::Ground,
                0x1b => VState::Escape,
                _ => self.state,
            }
        }

        fn step(&mut self, b: u8) {
            use VState::*;
            let c0 = matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f);
            self.state = match self.state {
                Ground => if b == 0x1b { Escape } else { Ground },
                Escape => match b {
                    _ if c0 => Escape,
                    0x20..=0x2f => EscInter,
                    0x50 => DcsEntry,
                    0x58 | 0x5e | 0x5f => SosPmApc,
                    0x5b => CsiEntry,
                    0x5d => {
                        self.osc_len = 0;
                        Osc
                    }
                    0x30..=0x7e | 0x18 | 0x1a => Ground,
                    _ => Escape, // ESC, DEL, 0x80..=0xFF
                },
                EscInter => match b {
                    _ if c0 => EscInter,
                    0x20..=0x2f | 0x7f => EscInter,
                    0x30..=0x7e => Ground,
                    _ => self.anywhere(b),
                },
                CsiEntry => match b {
                    _ if c0 => CsiEntry,
                    0x20..=0x2f => CsiInter,
                    0x30..=0x3f => CsiParam,
                    0x40..=0x7e => Ground,
                    _ => self.anywhere(b),
                },
                CsiParam => match b {
                    _ if c0 => CsiParam,
                    0x20..=0x2f => CsiInter,
                    0x30..=0x3b | 0x7f => CsiParam,
                    0x3c..=0x3f => CsiIgnore,
                    0x40..=0x7e => Ground,
                    _ => self.anywhere(b),
                },
                CsiInter => match b {
                    _ if c0 => CsiInter,
                    0x20..=0x2f => CsiInter,
                    0x30..=0x3f => CsiIgnore,
                    0x40..=0x7e => Ground,
                    _ => self.anywhere(b),
                },
                CsiIgnore => match b {
                    _ if c0 => CsiIgnore,
                    0x20..=0x3f | 0x7f => CsiIgnore,
                    0x40..=0x7e => Ground,
                    _ => self.anywhere(b),
                },
                DcsEntry => match b {
                    _ if c0 => DcsEntry,
                    0x20..=0x2f => DcsInter,
                    0x30..=0x3f => DcsParam,
                    0x40..=0x7e => DcsPass,
                    0x7f => DcsEntry,
                    _ => self.anywhere(b),
                },
                DcsParam => match b {
                    _ if c0 => DcsParam,
                    0x20..=0x2f => DcsInter,
                    0x30..=0x3b | 0x7f => DcsParam,
                    0x3c..=0x3f => DcsIgnore,
                    0x40..=0x7e => DcsPass,
                    _ => self.anywhere(b),
                },
                DcsInter => match b {
                    _ if c0 => DcsInter,
                    0x20..=0x2f | 0x7f => DcsInter,
                    0x30..=0x3f => DcsIgnore,
                    0x40..=0x7e => DcsPass,
                    _ => self.anywhere(b),
                },
                DcsIgnore | SosPmApc => self.anywhere(b),
                DcsPass => match b {
                    0x18 | 0x1a | 0x9c => Ground,
                    0x1b => Escape,
                    _ => DcsPass,
                },
                Osc => match b {
                    0x07 | 0x18 | 0x1a => Ground,
                    0x1b => Escape,
                    0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f | 0x3b => Osc,
                    _ => {
                        self.osc_len += 1;
                        self.max_osc = self.max_osc.max(self.osc_len);
                        Osc
                    }
                },
            };
        }
    }

    #[test]
    fn scanner_agrees_with_vte_on_escape_and_osc_states_fuzz() {
        // Whenever vte is collecting an OSC (its only unbounded buffer), the
        // scanner must be tracking it, and vice versa; whenever vte sits in
        // Escape, so must the scanner (that is where an OSC can begin); and vte's
        // OSC buffer must never exceed the cap. Random streams (ESC with C0 /
        // DEL / high bytes in between, CAN/SUB/BEL/0x9C, DCS/APC/SOS/PM, long
        // payload runs) are fed at random chunk boundaries; the model replays
        // exactly the bytes JeTTY handed to vte (incl. its injected CAN).
        let mut seed: u64 = 0x5eed_1234_abcd_0001;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let alphabet: &[u8] =
            b"\x1b\x1b\x1b\x1b]]][[P_X^\x07\x18\x1a\x9c\n\r\x01\x7f\x80\xc3\xa90123;;?qGJhlc\\Az=,";
        for case in 0..400 {
            let mut t = Terminal::new(30, 8);
            t.set_cell_px(10.0, 10.0);
            t.osc_cap = 48;
            if case % 3 == 0 {
                t.feed(b"\x1b]133;A\x07"); // live anchors: the isolation paths run too
            }
            t.vte_log = Some(Vec::new());
            let mut model = VteModel { state: VState::Ground, osc_len: 0, max_osc: 0 };
            let mut stream = Vec::new();
            while stream.len() < 1500 {
                match next() % 10 {
                    0 => stream.extend(std::iter::repeat_n(b'x', (next() % 120) as usize)),
                    1 => stream.extend_from_slice(b"\x1b]0;"),
                    2 => stream.extend_from_slice(b"\x1b]133;"),
                    3 => stream.extend_from_slice(b"\x1b]9;4;"),
                    _ => stream.push(alphabet[next() as usize % alphabet.len()]),
                }
            }
            let (mut i, mut seen) = (0, 0);
            while i < stream.len() {
                let end = (i + 1 + (next() % 40) as usize).min(stream.len());
                t.feed(&stream[i..end]);
                i = end;
                let log = t.vte_log.as_ref().unwrap();
                for &b in &log[seen..] {
                    model.step(b);
                }
                seen = log.len();
                let scanning_osc = matches!(
                    t.scan,
                    Scan::Prefix { .. } | Scan::Payload { .. } | Scan::Skip | Scan::Prefix94 { .. } | Scan::Progress { .. }
                );
                assert_eq!(scanning_osc, model.state == VState::Osc, "case {case} @{i}: scan {:?} vs vte {:?}", t.scan, model.state);
                assert_eq!(t.scan == Scan::Esc, model.state == VState::Escape, "case {case} @{i}: scan {:?} vs vte {:?}", t.scan, model.state);
                assert!(model.max_osc <= t.osc_cap as usize, "case {case}: vte buffered {} OSC bytes", model.max_osc);
            }
        }
    }

    // ── scanner-tracked mouse modes (X10 9 / urxvt 1015) ──────────────────────

    /// Feed `seq` split at EVERY byte boundary (and whole), with and without
    /// live anchors, and return the (x10, urxvt) flags each run ends with.
    fn mouse_modes_after(seq: &[u8]) -> Vec<(bool, bool)> {
        let mut out = Vec::new();
        for anchors in [false, true] {
            for cut in 0..=seq.len() {
                let mut t = Terminal::new(30, 8);
                if anchors {
                    t.feed(b"\x1b]133;A\x07");
                }
                t.feed(&seq[..cut]);
                t.feed(&seq[cut..]);
                out.push((t.mouse_x10(), t.mouse_urxvt()));
            }
        }
        out
    }

    #[test]
    fn urxvt_and_x10_mouse_modes_are_tracked_at_any_split() {
        let all = |seq: &[u8], want: (bool, bool)| {
            for got in mouse_modes_after(seq) {
                assert_eq!(got, want, "{:?}", String::from_utf8_lossy(seq));
            }
        };
        all(b"\x1b[?1015h", (false, true));
        all(b"\x1b[?9h", (true, false));
        all(b"\x1b[?1000;1006;1015h", (false, true));
        all(b"\x1b[?1015;9h", (true, true));
        all(b"\x1b[?12345;1015h", (false, true));
        all(b"\x1b[?1015h\x1b[?1015l", (false, false));
        all(b"\x1b[?9h\x1b[?1015h\x1bc", (false, false)); // RIS resets both
        // Not a DECSET of these modes: other modes, ANSI SM, other finals, an
        // intermediate, an over-long number, a sub-parameter, a CAN abort.
        all(b"\x1b[?25l\x1b[?2004h", (false, false));
        all(b"\x1b[1015h", (false, false));
        all(b"\x1b[?1015m", (false, false));
        all(b"\x1b[?1015$h", (false, false));
        all(b"\x1b[?10150h", (false, false));
        all(b"\x1b[?1015:1h", (false, false));
        all(b"\x1b[?10\x1815h", (false, false));
        // C0 controls inside the CSI are executed by vte without ending it.
        all(b"\x1b[?10\n15h", (false, true));
    }

    #[test]
    fn tracking_mouse_modes_leaves_the_screen_untouched() {
        let mut t = Terminal::new(30, 4);
        t.feed(b"ab\x1b[?1015hcd\x1b[?9h\x1b[1;31mef");
        assert!(t.mouse_urxvt() && t.mouse_x10());
        let snap = t.snapshot();
        let row: String = snap.cells.iter().take(6).map(|c| c.c).collect();
        assert_eq!(row, "abcdef", "the sequences still reach vte, nothing leaks as text");
    }

    // ── mode getters for the input layer (kitty keyboard / focus / mouse) ────

    #[test]
    fn kitty_keyboard_off_by_default_ignores_push() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[>1u"); // push "disambiguate"
        assert_eq!(t.kitty_keyboard_flags(), 0, "support off ⇒ push ignored");
        t.feed(b"\x1b[?u"); // query
        assert!(t.drain_pty_writes().is_empty(), "no kitty reply while disabled");
    }

    #[test]
    fn kitty_keyboard_push_pop_and_query_when_enabled() {
        let mut t = Terminal::new(20, 5);
        t.set_kitty_keyboard(true);
        t.feed(b"\x1b[>1u");
        assert_eq!(t.kitty_keyboard_flags(), 1, "disambiguate pushed");
        t.feed(b"\x1b[>31u");
        assert_eq!(t.kitty_keyboard_flags(), 31, "all five flags");
        t.feed(b"\x1b[?u");
        let reply = String::from_utf8_lossy(&t.drain_pty_writes()).to_string();
        assert_eq!(reply, "\x1b[?31u", "query reports the current flags");
        t.feed(b"\x1b[<u"); // pop one
        assert_eq!(t.kitty_keyboard_flags(), 1, "pop restores the previous entry");
        // A scrollback rebuild must not silently disable the protocol.
        t.set_scrollback_lines(500);
        t.feed(b"\x1b[?u");
        assert!(!t.drain_pty_writes().is_empty(), "still enabled after a Config rebuild");
    }

    /// A terminal with the kitty keyboard protocol on (as every app tab is).
    fn kitty_term() -> Terminal {
        let mut t = Terminal::new(40, 10);
        t.set_kitty_keyboard(true);
        t
    }

    #[test]
    fn flags_a_killed_program_pushed_are_dropped_at_the_next_prompt() {
        // A main-screen program pushes "report all keys" and dies (SIGKILL, a
        // crash, a dropped ssh) without popping. The shell's next prompt must not
        // inherit it: Ctrl+C would reach the shell as `\e[99;5u`.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07$ tui\r\n\x1b]133;C\x07");
        t.feed(b"\x1b[>15u");
        assert_eq!(t.kitty_keyboard_flags(), 15, "premise: the program's flags are live");
        t.feed(b"\x1b]133;D;137\x07\x1b]133;A\x07$ ");
        assert_eq!(t.kitty_keyboard_flags(), 0, "the prompt gets legacy keys back");
        t.feed(b"\x1b[?u");
        assert_eq!(t.drain_pty_writes(), b"\x1b[?0u", "the stack itself is empty again");
    }

    #[test]
    fn flags_the_shell_had_before_the_command_are_kept() {
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b[>1u$ tui\r\n\x1b]133;C\x07"); // the shell's own entry
        t.feed(b"\x1b[>8u\x1b[>31u"); // the program's two, never popped
        t.feed(b"\x1b]133;D;1\x07");
        assert_eq!(t.kitty_keyboard_flags(), 1, "only the program's entries are dropped");
        t.feed(b"\x1b[<u");
        assert_eq!(t.kitty_keyboard_flags(), 0, "the shell's entry is still the bottom one");
    }

    #[test]
    fn a_program_that_cleans_up_is_left_alone() {
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b[>15u\x1b[<u\x1b]133;D;0\x07");
        assert_eq!(t.kitty_keyboard_flags(), 0);
        assert_eq!(t.kbd_depth, [0, 0]);
    }

    #[test]
    fn flags_set_in_place_by_a_killed_program_are_dropped() {
        // `CSI = flags u` replaces the active flags without a push.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b[=8u");
        assert_eq!(t.kitty_keyboard_flags(), 8);
        t.feed(b"\x1b]133;D;137\x07");
        assert_eq!(t.kitty_keyboard_flags(), 0);
    }

    #[test]
    fn a_shell_popping_its_own_entry_after_c_is_not_undone() {
        // A line editor that pushes while reading (reedline) and pops just after
        // the shell emitted C: the floor follows the pop, so only the program's
        // push is undone — the shell's popped entry is never resurrected.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b[>1u$ cmd\r\n\x1b]133;C\x07\x1b[<u");
        t.feed(b"\x1b[>8u"); // the program, killed
        t.feed(b"\x1b]133;D;137\x07");
        assert_eq!(t.kitty_keyboard_flags(), 0);
        assert_eq!(t.kbd_depth[0], 0);
    }

    #[test]
    fn a_shell_without_c_marks_still_gets_its_keys_back() {
        // A/D-only integrations (bash < 4.4): the save point is the prompt itself,
        // and the command's D undoes what it left.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07$ tui\r\n\x1b[>31u");
        t.feed(b"\x1b]133;D;137\x07");
        assert_eq!(t.kitty_keyboard_flags(), 0);
    }

    #[test]
    fn a_new_prompt_without_a_command_keeps_the_line_editors_flags() {
        // A line editor that pushes its own flags while reading (reedline), then
        // Ctrl+C at the prompt: a fresh A with no command in between — those
        // flags are the editor's, still in use, never popped by JeTTY.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b[>1u^C\r\n\x1b]133;A\x07");
        assert_eq!(t.kitty_keyboard_flags(), 1);
        // Same for an editor that pushes BEFORE the prompt mark and pops and
        // re-pushes around the repaint (the window's floor dips to 0 meanwhile).
        let mut t = kitty_term();
        t.feed(b"\x1b[>1u\x1b]133;A\x07");
        t.feed(b"\x1b[<u\x1b[>1u\r\n\x1b]133;A\x07");
        assert_eq!(t.kitty_keyboard_flags(), 1);
    }

    #[test]
    fn a_prompt_repaint_keeps_flags_the_shell_set_while_reading() {
        // fish sets its flags in place (`CSI = 5 u`) while reading a line; a
        // Ctrl+C repaint emits a fresh A with no command in between — those
        // flags are the shell's, not a dead program's.
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b[=5u");
        t.feed(b"^C\r\n\x1b]133;A\x07");
        assert_eq!(t.kitty_keyboard_flags(), 5);
    }

    #[test]
    fn the_alternate_screen_stack_is_not_touched_by_prompts() {
        let mut t = kitty_term();
        t.feed(b"\x1b]133;A\x07\x1b]133;C\x07\x1b[?1049h\x1b[>15u");
        assert_eq!(t.kbd_depth, [0, 1]);
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.kitty_keyboard_flags(), 0, "the primary stack was never pushed");
        t.feed(b"\x1b]133;D;0\x07");
        assert_eq!(t.kbd_depth, [0, 1], "a prompt only restores the primary stack");
    }

    #[test]
    fn a_push_flood_cannot_crash_the_terminal() {
        // alacritty 0.26 caps its stack at 4096 by evicting from the TITLE stack,
        // which panics when that is empty: `printf '\e[>1u%.0s' {1..4097}` used
        // to kill the whole terminal. Controls vte executes inside the CSI are
        // seen through, so they cannot hide a push from the mirror.
        for unit in [&b"\x1b[>1u"[..], b"\x1b[\x00>1u", b"\x1b[\x07>\x7f1\x0au", b"\x1b[>;1u"] {
            let mut t = kitty_term();
            let flood = unit.repeat(5000);
            t.feed(&flood);
            assert!(t.kbd_depth[0] <= KBD_STACK_MAX, "{unit:?}: depth {}", t.kbd_depth[0]);
        }
        // Split across feeds at every byte.
        let mut t = kitty_term();
        for _ in 0..4200 {
            for b in b"\x1b[>1u" {
                t.feed(&[*b]);
            }
        }
        assert_eq!(t.kbd_depth[0], KBD_STACK_MAX);
        assert_eq!(t.kitty_keyboard_flags(), 1);
    }

    #[test]
    fn the_depth_mirror_follows_pops_resets_and_toggles() {
        let mut t = kitty_term();
        t.feed(b"\x1b[>1u\x1b[>2u\x1b[>4u");
        assert_eq!(t.kbd_depth[0], 3);
        t.feed(b"\x1b[<2u");
        assert_eq!((t.kbd_depth[0], t.kitty_keyboard_flags()), (1, 1));
        t.feed(b"\x1b[<0u"); // vte: a zero count pops one
        assert_eq!(t.kbd_depth[0], 0);
        t.feed(b"\x1b[>1u\x1bc"); // RIS clears both stacks
        assert_eq!((t.kbd_depth, t.kitty_keyboard_flags()), ([0, 0], 0));
        t.feed(b"\x1b[>1u");
        t.set_kitty_keyboard(false);
        assert_eq!((t.kbd_depth, t.kitty_keyboard_flags()), ([0, 0], 0));
        t.feed(b"\x1b[>1u");
        assert_eq!(t.kbd_depth, [0, 0], "ignored while the protocol is off");
    }

    #[test]
    fn reset_input_modes_clears_modes_but_keeps_the_screen() {
        let mut t = kitty_term();
        t.feed(b"keep me\r\n");
        t.feed(b"\x1b[>15u\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?9h\x1b[?1015h");
        t.feed(b"\x1b[?1049h\x1b[>8u\x1b[?1003h"); // a TUI that then died on the alt screen
        assert!(t.mouse_mode() && t.kitty_keyboard_flags() == 8);
        t.reset_input_modes();
        assert_eq!(t.kitty_keyboard_flags(), 0, "alt-screen flags gone");
        assert!(!t.mouse_mode() && !t.mouse_drag() && !t.mouse_motion());
        assert!(!t.sgr_mouse() && !t.focus_reporting() && !t.bracketed_paste());
        assert!(!t.mouse_x10() && !t.mouse_urxvt());
        assert!(t.alt_screen(), "the screen itself is left alone");
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.kitty_keyboard_flags(), 0, "the primary stack was cleared too");
        assert_eq!(t.snapshot().row_text(0).trim_end(), "keep me");
        t.feed(b"\x1b[?u");
        assert_eq!(t.drain_pty_writes(), b"\x1b[?0u", "the protocol is still on");
    }

    #[test]
    fn reset_after_exit_drops_what_a_dead_program_left_on() {
        // An rc file exec'd tmux, which died mid-frame: a synchronized update
        // still open, the alternate screen, kitty keys, mouse and paste modes.
        let mut t = kitty_term();
        t.feed(b"$ zsh\r\n");
        t.feed(b"\x1b[?1049h\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?2026hhalf a frame");
        t.reset_after_exit();
        assert!(t.sync_deadline().is_none(), "the update is over");
        assert!(!t.alt_screen(), "back on the primary screen");
        assert_eq!(t.kitty_keyboard_flags(), 0, "Ctrl+C is ^C again");
        assert!(!t.mouse_mode() && !t.sgr_mouse() && !t.bracketed_paste());
        assert_eq!(t.snapshot().row_text(0).trim_end(), "$ zsh", "the primary screen as it was");
        // Nothing to drop: the screen is left alone.
        let mut plain = kitty_term();
        plain.feed(b"keep\r\nme");
        plain.reset_after_exit();
        assert_eq!(plain.snapshot().row_text(1).trim_end(), "me");
        assert_eq!(plain.snapshot().cursor_col, 2, "the cursor stays");
    }

    #[test]
    fn focus_reporting_and_utf8_mouse_track_their_modes() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.focus_reporting());
        t.feed(b"\x1b[?1004h");
        assert!(t.focus_reporting());
        t.feed(b"\x1b[?1004l");
        assert!(!t.focus_reporting());
        assert!(!t.mouse_utf8());
        t.feed(b"\x1b[?1005h");
        assert!(t.mouse_utf8());
    }

    #[test]
    fn has_selection_tells_text_from_blanks_without_building_it() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"ab  cd\r\n\r\nxyz");
        assert!(!t.has_selection(), "none");
        t.selection_start(0, 0, true);
        t.selection_update(0, 4, false);
        assert!(t.has_selection());
        t.selection_start(0, 2, true);
        t.selection_update(0, 3, false);
        assert!(!t.has_selection(), "only the blanks between words");
        t.selection_start(0, 10, true);
        t.selection_update(1, 5, false);
        assert!(!t.has_selection(), "blank cells across lines");
        t.selection_start_block_abs(t.viewport_line_to_buffer(0), 3, true);
        t.selection_update_abs(t.viewport_line_to_buffer(2), 3, false);
        assert!(!t.has_selection(), "a blank column block");
        t.selection_start_block_abs(t.viewport_line_to_buffer(0), 2, true);
        t.selection_update_abs(t.viewport_line_to_buffer(2), 3, false);
        assert!(t.has_selection(), "the block reaches the z");
        t.select_all();
        assert!(t.has_selection());
        t.selection_clear();
        assert!(!t.has_selection());
    }

    #[test]
    fn a_double_click_on_a_bracket_is_resolved_once() {
        // A double-click selection was a Semantic one, re-derived by every
        // `to_range` — every frame, every selection_text: on an unmatched
        // bracket that is a bracket search through the whole scrollback.
        let mut t = Terminal::new(30, 4);
        for i in 0..200 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.feed(b">>> x) f(a, b) y");
        t.selection_start_semantic(3, 8); // the `(` of f(a, b)
        assert_eq!(t.selection_text().as_deref(), Some("(a, b)"));
        assert_eq!(t.term.selection.as_ref().map(|s| s.ty), Some(SelectionType::Simple), "stored as its cells");
        t.selection_start_semantic(3, 5); // the unmatched `)`
        assert_eq!(t.term.selection.as_ref().map(|s| s.ty), Some(SelectionType::Simple));
        // Output scrolls the double-clicked word up two rows; a drag still
        // grows it from that word.
        t.selection_start_semantic(3, 12); // `b`
        t.feed(b"\r\nmore\r\nlines");
        t.selection_update(1, 15, false); // `y`, two rows up now
        assert_eq!(t.selection_text().as_deref(), Some("b) y"));
    }

    #[test]
    fn a_double_click_selection_covers_what_alacritty_s_word_selection_does() {
        // Resolved once and re-derived from the double-clicked cell on every
        // drag, the range equals a Semantic selection's — words, bracket
        // pairs, separators, wide chars — back on the origin cell too.
        let mut r = Rng(0x5eed_d0b1);
        let pieces: [&[u8]; 13] = [
            b"ab", b" ", b"(", b")", b"[", b"]", b"<", b">", "\u{4e2d}".as_bytes(), b"\r\n", b"x:y", b"{", b"}",
        ];
        let at = |t: &Terminal, row: usize, col: usize| {
            viewport_to_point(t.term.grid().display_offset(), Point::new(row, Column(col)))
        };
        let range = |t: &Terminal, sel: &Selection| sel.to_range(&t.term).map(|r| (r.start, r.end));
        let mine = |t: &Terminal| t.term.selection.as_ref().and_then(|s| s.to_range(&t.term)).map(|r| (r.start, r.end));
        for case in 0..300 {
            let mut t = Terminal::new(2 + r.below(20), 1 + r.below(6));
            for _ in 0..r.below(60) {
                t.feed(r.pick(&pieces));
            }
            let (rows, cols) = (t.rows, t.cols);
            let (row, col) = (r.below(rows), r.below(cols));
            let mut reference = Selection::new(SelectionType::Semantic, at(&t, row, col), Side::Left);
            t.selection_start_semantic(row, col);
            assert_eq!(mine(&t), range(&t, &reference), "case {case}: double-click at {row},{col}");
            for step in 0..r.below(6) {
                let (row2, col2, left) =
                    if r.chance(4) { (row, col, true) } else { (r.below(rows), r.below(cols), r.chance(2)) };
                let side = if left { Side::Left } else { Side::Right };
                reference.update(at(&t, row2, col2), side);
                t.selection_update(row2, col2, left);
                assert_eq!(mine(&t), range(&t, &reference), "case {case} step {step}: drag to {row2},{col2}");
            }
        }
    }

    #[test]
    fn semantic_selection_selects_the_word_under_the_cell() {
        let mut t = Terminal::new(40, 3);
        t.feed(b"cargo build --release");
        t.selection_start_semantic(0, 7); // inside "build"
        assert_eq!(t.selection_text().as_deref(), Some("build"));
        // Extending to another word grows the selection word-wise.
        t.selection_update(0, 15, true);
        assert_eq!(t.selection_text().as_deref(), Some("build --release"));
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

    // ── robustness: seeded random streams of garbage AND valid sequences ─────

    /// xorshift64* — a tiny seeded generator (no new dependency).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }

        fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
            xs[self.below(xs.len())]
        }

        fn chance(&mut self, one_in: usize) -> bool {
            self.below(one_in) == 0
        }
    }

    /// A CSI / mode parameter: mostly small, sometimes 0, empty or huge.
    fn fuzz_param(r: &mut Rng, out: &mut Vec<u8>) {
        match r.below(8) {
            0 => {}
            1 => out.push(b'0'),
            2 => out.extend_from_slice(r.pick(&[&b"65535"[..], b"99999999999", b"4294967296"])),
            _ => out.extend_from_slice(r.below(140).to_string().as_bytes()),
        }
    }

    /// Append one random token: raw garbage, or a valid (sometimes mangled)
    /// sequence of the kinds real programs emit.
    fn fuzz_token(r: &mut Rng, out: &mut Vec<u8>) {
        const TEXT: [&str; 14] = [
            "hello", "a", " ", "x\u{301}", "中文", "😀", "👩\u{200d}💻", "❤\u{fe0f}", "\u{1f1e9}\u{1f1ea}",
            "\u{200b}", "\u{fe0f}", "\u{301}\u{302}\u{303}", "ﷺ", "e\u{20e3}",
        ];
        const CONTROLS: &[u8] = b"\r\n\t\x08\x0b\x0c\x0e\x0f\x07\x18\x1a\x00\x7f\x05";
        const CSI_FINALS: &[u8] = b"@ABCDEFGHIJKLMPSTXZ`abcdefghlmnpqrstuxy";
        const ESC_FINALS: &[u8] = b"78DEHMNOPVWZ\\c=>n|}~";
        const MODES: [&[u8]; 26] = [
            b"1", b"3", b"5", b"6", b"7", b"9", b"12", b"25", b"47", b"66", b"69", b"1000", b"1002",
            b"1003", b"1004", b"1005", b"1006", b"1007", b"1015", b"1047", b"1048", b"1049", b"2004",
            b"2026", b"2031", b"9999",
        ];
        match r.below(30) {
            // Raw garbage, any byte.
            0..=2 => {
                for _ in 0..1 + r.below(24) {
                    out.push(r.next() as u8);
                }
            }
            3..=6 => out.extend_from_slice(r.pick(&TEXT).as_bytes()),
            7..=8 => {
                for _ in 0..1 + r.below(4) {
                    out.push(r.pick(CONTROLS));
                }
            }
            // A run of printable text (line-filling, wrapping).
            9 => {
                let n = r.below(200);
                out.extend((0..n).map(|i| b'a' + (i % 26) as u8));
            }
            // CSI with random params / markers / intermediates / finals.
            10..=15 => {
                out.extend_from_slice(b"\x1b[");
                if r.chance(3) {
                    out.push(r.pick(b"?><=!"));
                }
                for k in 0..r.below(5) {
                    if k > 0 {
                        out.push(if r.chance(5) { b':' } else { b';' });
                    }
                    fuzz_param(r, out);
                }
                if r.chance(6) {
                    out.push(r.pick(b" $\"'!#"));
                }
                if r.chance(20) {
                    out.push(r.pick(CONTROLS)); // a C0 inside the CSI
                }
                out.push(if r.chance(10) { 0x40 + r.below(0x3f) as u8 } else { r.pick(CSI_FINALS) });
            }
            // DEC private modes: set, reset or query.
            16..=17 => {
                out.extend_from_slice(b"\x1b[?");
                out.extend_from_slice(r.pick(&MODES));
                if r.chance(4) {
                    out.push(b';');
                    out.extend_from_slice(r.pick(&MODES));
                }
                out.extend_from_slice(r.pick(&[&b"h"[..], b"l", b"h", b"l", b"$p"]));
            }
            // Scroll regions and absolute moves.
            18 => out.extend_from_slice(format!("\x1b[{};{}r", r.below(30), r.below(30)).as_bytes()),
            19 => out.extend_from_slice(format!("\x1b[{};{}H", r.below(40), r.below(140)).as_bytes()),
            // ESC + one byte (DECSC/DECRC/IND/NEL/RI/RIS/charsets…).
            20 => {
                out.push(0x1b);
                match r.below(4) {
                    0 => out.extend_from_slice(r.pick(&[&b"(0"[..], b"(B", b")0", b"*A", b"#8", b" G"])),
                    _ => out.push(r.pick(ESC_FINALS)),
                }
            }
            // OSC: titles, colors (set / query / reset), links, clipboard, marks.
            21..=23 => {
                out.extend_from_slice(b"\x1b]");
                let body: &[u8] = r.pick(&[
                    &b"0;title"[..], b"2;t\xc3\xa9", b"1;icon", b"4;1;#123456", b"4;300;?", b"4;7;?",
                    b"10;?", b"11;?", b"12;?", b"10;#abcdef", b"11;rgb:12/34/56", b"104", b"104;1",
                    b"110", b"111", b"112", b"8;;https://x.test", b"8;;", b"52;c;aGk=", b"52;p;?",
                    b"7;file:///tmp", b"133;A", b"133;B", b"133;C", b"133;D;1", b"133;D", b"9;4;1;50",
                    b"9;4;3", b"9;4;0", b"777;notify;a;b", b"", b"9;hi",
                ]);
                out.extend_from_slice(body);
                if r.chance(8) {
                    out.extend(std::iter::repeat_n(b'z', r.below(300)));
                }
                out.extend_from_slice(r.pick(&[&b"\x07"[..], b"\x1b\\", b"\x1b\\", b"\x18", b"", b"\x9c"]));
            }
            // DCS: sixel (valid and broken), DECRQSS, XTGETTCAP, tmux.
            24..=25 => {
                out.extend_from_slice(b"\x1bP");
                let body: &[u8] = r.pick(&[
                    &b"q#0;2;100;0;0#0~~-~~"[..], b"0;1;0q\"1;1;4;4#1~", b"q!5~$-!300~", b"$qm",
                    b"$q q", b"$qr", b"+q544e", b"tmux;\x1b\x1b[1m", b"1$t", b"q#9999;2;0;0;0",
                ]);
                out.extend_from_slice(body);
                out.extend_from_slice(r.pick(&[&b"\x1b\\"[..], b"\x9c", b"\x18", b""]));
            }
            // APC: kitty graphics (valid, chunked, garbage) and other APCs.
            26 => {
                out.extend_from_slice(b"\x1b_");
                let body: &[u8] = r.pick(&[
                    &b"Gf=32,s=1,v=1,a=T;/wAA/w=="[..], b"Gf=24,s=1,v=1,a=T,i=3;/wAA", b"Ga=q,i=9;",
                    b"Ga=d", b"Ga=d,d=i,i=3", b"Gm=1,f=32,s=2,v=1;AAAA", b"Gm=0;AAAAAAAAAAA=",
                    b"Ga=p,i=3", b"Gq=2,a=T,f=100;iVBORw0K", b"other apc", b"G",
                ]);
                out.extend_from_slice(body);
                out.extend_from_slice(r.pick(&[&b"\x1b\\"[..], b"\x9c", b"\x1a", b""]));
            }
            // C1 bytes and broken UTF-8.
            27 => {
                for _ in 0..1 + r.below(4) {
                    out.push(r.pick(&[0x84u8, 0x85, 0x88, 0x8d, 0x90, 0x9b, 0x9c, 0x9d, 0x9f, 0xc2, 0xe4, 0xf0, 0xff]));
                }
            }
            // Kitty keyboard stack, synchronized updates, queries.
            28 => out.extend_from_slice(r.pick(&[
                &b"\x1b[>1u"[..], b"\x1b[<u", b"\x1b[=5;1u", b"\x1b[?u", b"\x1b[<99u", b"\x1b[?2026h",
                b"\x1b[?2026l", b"\x1b[>q", b"\x1b[c", b"\x1b[>c", b"\x1b[6n", b"\x1b[?996n",
            ])),
            // Repeat, tabs, erase, insert/delete — the editing family, big counts.
            _ => {
                let f = r.pick(b"b@PXLMIZgJKST");
                out.extend_from_slice(format!("\x1b[{}{}", r.below(300), f as char).as_bytes());
            }
        }
    }

    /// Assert what must hold whatever the input: the grid's geometry, both
    /// cursors and every bounded buffer, and a snapshot consistent with them.
    fn assert_terminal_invariants(t: &Terminal, deep: bool, ctx: &str) {
        let grid = t.term.grid();
        assert_eq!(grid.columns(), t.cols, "{ctx}: grid columns");
        assert_eq!(grid.screen_lines(), t.rows, "{ctx}: grid lines");
        for (what, c) in [("cursor", &grid.cursor), ("saved cursor", &grid.saved_cursor)] {
            let p = c.point;
            assert!(p.line.0 >= 0 && (p.line.0 as usize) < t.rows, "{ctx}: {what} at {p:?}");
            assert!(p.column.0 < t.cols, "{ctx}: {what} at {p:?}");
        }
        assert!(grid.display_offset() <= grid.history_size(), "{ctx}: display offset");
        assert!(grid.history_size() <= t.scrollback_limit, "{ctx}: history over the cap");
        if deep {
            for l in grid.topmost_line().0..=grid.bottommost_line().0 {
                assert_eq!(grid[Line(l)].len(), t.cols, "{ctx}: row {l} length");
            }
        }
        assert!(t.osc_len <= t.osc_cap, "{ctx}: OSC buffer {}", t.osc_len);
        assert!(t.sixel_buf.len() <= SIXEL_MAX_BYTES, "{ctx}: sixel buffer");
        assert!(t.apc_buf.len() <= APC_MAX_BYTES, "{ctx}: APC buffer");
        assert!(t.chunk_buf.len() <= KITTY_RAW_BUDGET, "{ctx}: kitty chunks");
        assert!(t.image_work <= IMAGE_WORK_MAX, "{ctx}: image-work bank");
        assert!(t.kbd_depth.iter().all(|&d| d <= KBD_STACK_MAX), "{ctx}: kbd depth {:?}", t.kbd_depth);
        assert!(t.marks.len() <= MAX_MARKS, "{ctx}: marks");
        assert!(t.completed.len() <= MAX_PENDING_COMPLETIONS, "{ctx}: completions");
        assert!(t.placements.len() <= MAX_PLACEMENTS && t.alt_placements.len() <= MAX_PLACEMENTS, "{ctx}: placements");
        let live: u64 = t.placements.iter().map(|p| p.image.rgba.len() as u64).sum();
        assert_eq!(live, t.placement_bytes, "{ctx}: placement byte counter");
        let live: u64 = t.alt_placements.iter().map(|p| p.image.rgba.len() as u64).sum();
        assert_eq!(live, t.alt_placement_bytes, "{ctx}: alt placement byte counter");
        let stored: u64 = t.kitty_images.iter().map(|e| e.image.rgba.len() as u64).sum();
        assert_eq!(stored, t.kitty_stored_bytes, "{ctx}: registry byte counter");
        assert!(t.kitty_images.len() <= MAX_KITTY_STORED && stored <= MAX_KITTY_STORED_BYTES, "{ctx}: registry");
        assert!(t.alt_screen() || t.kitty_images.iter().all(|e| !e.alt), "{ctx}: alternate-screen images outlive it");
        let snap = t.snapshot();
        assert_eq!(snap.cells.len(), t.rows * t.cols, "{ctx}: snapshot size");
        assert!(snap.cursor_row < t.rows && snap.cursor_col < t.cols, "{ctx}: snapshot cursor");
        for g in &snap.graphemes {
            assert!(g.row < t.rows && g.col < t.cols, "{ctx}: grapheme at {},{}", g.row, g.col);
            assert!(g.text.len() <= GRAPHEME_MAX_BYTES, "{ctx}: grapheme bytes");
            assert!(g.text.chars().count() <= 1 + GRAPHEME_MAX_MARKS, "{ctx}: grapheme marks");
        }
        for img in t.visible_images() {
            assert!(img.cols >= 1 && usize::from(img.col) + usize::from(img.cols) <= t.cols, "{ctx}: image span");
        }
    }

    /// Drive one terminal through `steps` random writes (split at random feed
    /// boundaries) interleaved with the app's own calls — resizes, scrollback
    /// changes, scrolling, selections, search, links, hints, sync flushes, reply
    /// drains — checking the invariants after every step.
    fn fuzz_terminal(seed: u64, steps: usize) {
        let mut r = Rng(seed | 1);
        let size = |r: &mut Rng| {
            let tiny = r.chance(4);
            (2 + r.below(if tiny { 6 } else { 120 }), 1 + r.below(if tiny { 4 } else { 40 }))
        };
        let (cols, rows) = size(&mut r);
        let mut t = Terminal::new(cols, rows);
        t.set_cell_px(1.0 + r.below(20) as f32, 1.0 + r.below(40) as f32);
        t.set_kitty_keyboard(!r.chance(3));
        if r.chance(3) {
            t.set_scrollback_lines(r.pick(&[0usize, 1, 3, 50, 1000]));
        }
        if r.chance(2) {
            t.osc_cap = 64 + r.below(512) as u32;
        }
        if r.chance(2) {
            t.feed(b"\x1b]133;A\x07$ \x1b]133;C\x07"); // live anchors: the isolation paths run
        }
        let mut buf = Vec::new();
        for step in 0..steps {
            buf.clear();
            for _ in 0..1 + r.below(12) {
                fuzz_token(&mut r, &mut buf);
            }
            let mut i = 0;
            while i < buf.len() {
                let end = (i + 1 + r.below(64)).min(buf.len());
                t.feed(&buf[i..end]);
                i = end;
            }
            match r.below(40) {
                0 => {
                    let (c, l) = size(&mut r);
                    t.resize(c, l);
                }
                1 => t.set_scrollback_lines(r.pick(&[0usize, 2, 40, 10_000])),
                2 => t.scroll_lines(r.below(60) as i32 - 30),
                3 => {
                    t.selection_start(r.below(t.rows), r.below(t.cols), r.chance(2));
                    t.selection_update(r.below(t.rows), r.below(t.cols), r.chance(2));
                    let _ = t.selection_text();
                }
                4 => {
                    let _ = t.search_set_query(r.pick(&["a", "中", "x\u{301}", "(", "zz"]));
                    let _ = t.search_nav(r.chance(2));
                    let _ = t.search_viewport_hits();
                }
                5 => {
                    let _ = t.link_at(r.below(t.rows), r.below(t.cols));
                    let _ = t.hint_tokens();
                    let _ = t.viewport_rows_chars();
                }
                6 => t.flush_sync(),
                7 => {
                    let _ = t.jump_prompt(r.chance(2));
                    let _ = t.failed_prompt_rows();
                    let _ = t.take_completions();
                }
                8 => t.reset_input_modes(),
                9 => t.selection_start_semantic(r.below(t.rows), r.below(t.cols)),
                10 => {
                    // Copy mode's block (Ctrl+V) and line selections, anchored in
                    // the buffer.
                    let line = t.viewport_line_to_buffer(r.below(t.rows));
                    if r.chance(2) {
                        t.selection_start_block_abs(line, r.below(t.cols), r.chance(2));
                    } else {
                        t.selection_start_lines_abs(line);
                    }
                    let line = t.viewport_line_to_buffer(r.below(t.rows));
                    t.selection_update_abs(line, r.below(t.cols), r.chance(2));
                    let _ = t.selection_text();
                }
                _ => {}
            }
            let _ = t.drain_pty_writes();
            let _ = t.take_title_update();
            let _ = t.take_clipboard_stores();
            assert_terminal_invariants(&t, step % 16 == 0, &format!("seed {seed:#x} step {step}"));
        }
    }

    #[test]
    fn random_streams_never_panic_and_keep_the_grid_consistent() {
        for seed in 1..=24u64 {
            fuzz_terminal(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 150);
        }
    }

    /// The long run (minutes):
    /// `cargo test -p jetty-core --release -- --ignored random_streams_long`.
    /// `JETTY_FUZZ_SEEDS` overrides the number of seeds; `JETTY_FUZZ_SEED=0x…`
    /// replays the one seed a failure names.
    #[test]
    #[ignore]
    fn random_streams_long() {
        if let Some(seed) = std::env::var("JETTY_FUZZ_SEED").ok().and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()) {
            return fuzz_terminal(seed, 600);
        }
        let seeds: u64 = std::env::var("JETTY_FUZZ_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(3000);
        for seed in 1..=seeds {
            let seed = seed.wrapping_mul(0xd1b5_4a32_d192_ed03);
            // Name the seed even when the panic comes from inside alacritty.
            let run = std::panic::catch_unwind(|| fuzz_terminal(seed, 600));
            assert!(run.is_ok(), "fuzz seed {seed:#x} panicked (replay: JETTY_FUZZ_SEED={seed:#x})");
        }
    }
}
