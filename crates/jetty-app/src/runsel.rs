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
        if ch == '\n' || ch == '\t' || !ch.is_control() {
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
