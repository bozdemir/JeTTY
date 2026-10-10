//! Desktop notifications a program asks for: iTerm2's `OSC 9 ; text`, the
//! `OSC 777 ; notify ; title ; body` of urxvt, foot and VTE, and the basic
//! subset of kitty's OSC 99 protocol — `p=title` / `p=body` payloads, `d=0`
//! chunks joined by their `i=` id, `e=1` base64. Every other OSC 99 key is
//! ignored, and nothing is ever answered (JeTTY sends no reports). The
//! scanner in `terminal.rs` hands each such OSC's payload here at its
//! terminator; the app decides whether to show the result (who is watching
//! the tab, a per-tab budget, the `program_notifications` switch).
//!
//! The text is UNTRUSTED — any `cat`ed file can carry these sequences — so it
//! leaves here only through [`crate::untrusted::display`]: one line, no
//! control or reordering character, a short title and body.

use crate::untrusted::display;

/// Most chars a program notification's title keeps.
pub const NOTIFICATION_TITLE_MAX_CHARS: usize = 64;

/// Most chars a program notification's body keeps.
pub const NOTIFICATION_BODY_MAX_CHARS: usize = 256;

/// Most payload bytes one notification OSC keeps — its head; the rest is
/// skipped. A kitty chunk is at most 4096 encoded bytes plus its metadata.
const CAPTURE_MAX_BYTES: usize = 8 * 1024;

/// Most bytes a kitty notification's title or body collects over its chunks:
/// far more than [`NOTIFICATION_BODY_MAX_CHARS`] chars take.
const PART_MAX_BYTES: usize = 4 * 1024;

/// Notifications held for the app between two drains. A flood keeps its
/// first ones (the app shows only a few of them anyway).
pub(crate) const MAX_PENDING: usize = 8;

/// Which OSC carries a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotifyOsc {
    /// `OSC 9 ; text` (iTerm2) — unless the text is a ConEmu command.
    Osc9,
    /// `OSC 777 ; notify ; title ; body` (urxvt, foot, VTE).
    Osc777,
    /// `OSC 99 ; metadata ; payload` (kitty).
    Osc99,
}

impl NotifyOsc {
    /// What the scanner matches after `ESC ]` before the payload.
    pub(crate) fn prefix(self) -> &'static [u8] {
        match self {
            NotifyOsc::Osc9 => b"9;",
            NotifyOsc::Osc777 => b"777;notify;",
            NotifyOsc::Osc99 => b"99;",
        }
    }
}

/// A desktop notification a program asked for, safe to show: one line each,
/// at most [`NOTIFICATION_TITLE_MAX_CHARS`] / [`NOTIFICATION_BODY_MAX_CHARS`]
/// chars, never both empty. Which tab sent it is the app's to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramNotification {
    /// Empty when the program gave none (OSC 9 never does).
    pub title: String,
    /// May be empty.
    pub body: String,
}

/// The program notifications of one terminal: the OSC being read, a kitty
/// notification still arriving in chunks, and those waiting for the app.
#[derive(Debug, Default)]
pub(crate) struct Notifications {
    /// The head of the notification OSC being scanned (up to
    /// [`CAPTURE_MAX_BYTES`]), without the C0 controls vte ignores in an OSC.
    capture: Vec<u8>,
    /// A kitty notification sent with `d=0`: more chunks follow.
    draft: Option<Draft>,
    pending: Vec<ProgramNotification>,
}

impl Notifications {
    /// A notification OSC begins; `head` is payload the scanner already read.
    pub(crate) fn begin(&mut self, head: &[u8]) {
        self.capture.clear();
        self.capture(head);
    }

    /// More of its payload (a run up to its terminator or the end of a read).
    pub(crate) fn capture(&mut self, run: &[u8]) {
        let room = CAPTURE_MAX_BYTES.saturating_sub(self.capture.len());
        self.capture.extend(run.iter().copied().filter(|&b| b >= 0x20).take(room));
    }

    /// Its terminator arrived: act on it.
    #[cold]
    #[inline(never)]
    pub(crate) fn finish(&mut self, osc: NotifyOsc) {
        let Notifications { capture, draft, pending } = self;
        match osc {
            NotifyOsc::Osc9 if !conemu_command(capture) => push(pending, b"", capture),
            NotifyOsc::Osc9 => {}
            NotifyOsc::Osc777 => {
                // The title ends at the first `;`; the body keeps any later one.
                let (title, body) = match capture.iter().position(|&b| b == b';') {
                    Some(n) => (&capture[..n], &capture[n + 1..]),
                    None => (&capture[..], &b""[..]),
                };
                push(pending, title, body);
            }
            NotifyOsc::Osc99 => kitty_chunk(draft, pending, capture),
        }
        capture.clear();
    }

    /// The notifications collected since the last call — empty (no
    /// allocation) in the common case.
    pub(crate) fn take(&mut self) -> Vec<ProgramNotification> {
        if self.pending.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

/// Whether an OSC 9 payload is one of ConEmu's commands — `n` or `n ; …` with
/// `n` 1–12 (sleep, message box, tab title, progress, wait, macro, run,
/// environment, working directory …) — not text to show: some prompt themes
/// send `9 ; 9 ; <cwd>` with every prompt.
fn conemu_command(payload: &[u8]) -> bool {
    let digits = payload.iter().take_while(|b| b.is_ascii_digit()).count();
    let n = std::str::from_utf8(&payload[..digits.min(2)]).ok().and_then(|s| s.parse::<u8>().ok());
    digits <= 2 && matches!(n, Some(1..=12)) && matches!(payload.get(digits), None | Some(b';'))
}

/// Queue a notification of `title` and `body` (raw payload bytes), made safe
/// to show; one with nothing left to show is dropped, and so is every one
/// past [`MAX_PENDING`].
fn push(pending: &mut Vec<ProgramNotification>, title: &[u8], body: &[u8]) {
    if pending.len() >= MAX_PENDING {
        return;
    }
    let title = text(title, NOTIFICATION_TITLE_MAX_CHARS);
    let body = text(body, NOTIFICATION_BODY_MAX_CHARS);
    if !title.is_empty() || !body.is_empty() {
        pending.push(ProgramNotification { title, body });
    }
}

/// Payload bytes as notification text: UTF-8 (a bad byte shows as U+FFFD),
/// line breaks and tabs as spaces, then [`display`]'s rules — at most
/// `max_chars` chars of one line with no control or hiding character.
fn text(raw: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(raw).replace(['\n', '\r', '\t'], " ");
    display(&s, max_chars).unwrap_or_default()
}

/// A kitty notification still arriving in chunks (`d=0`).
#[derive(Debug, Default)]
struct Draft {
    /// Its `i=`, as sent: only compared, never echoed.
    id: Vec<u8>,
    title: Part,
    body: Part,
}

/// The title or body of a [`Draft`] so far.
#[derive(Debug, Default)]
struct Part {
    bytes: Vec<u8>,
    /// Base64 symbols of a group a chunk cut short (fewer than four): the
    /// next chunk completes it, as kitty allows a split anywhere.
    carry: Vec<u8>,
}

impl Part {
    /// Append one chunk's payload (`base64`: sent with `e=1`).
    fn add(&mut self, payload: &[u8], base64: bool) {
        if !base64 {
            self.flush();
            self.append(payload);
            return;
        }
        self.carry.extend_from_slice(payload);
        let whole = self.carry.len() / 4 * 4;
        // Undecodable base64 adds nothing.
        if let Some(raw) = crate::base64::decode_base64(&self.carry[..whole], whole / 4 * 3) {
            self.append(&raw);
        }
        self.carry.drain(..whole);
    }

    /// Decode a carried short group as the last one (its padding may be
    /// missing).
    fn flush(&mut self) {
        if let Some(raw) = crate::base64::decode_base64(&self.carry, 3) {
            self.append(&raw);
        }
        self.carry.clear();
    }

    fn append(&mut self, bytes: &[u8]) {
        let room = PART_MAX_BYTES.saturating_sub(self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }
}

/// Which part of a notification an OSC 99 chunk carries.
enum Payload {
    Title,
    Body,
    /// An icon or buttons: not shown, but the chunk still joins and can end
    /// its notification.
    Unshown,
}

/// One OSC 99 chunk — `metadata ; payload`, the metadata `key=value` pairs
/// joined by `:`. A chunk with the `i=` of the notification being assembled
/// adds to it; any other id starts a new one (the unfinished one is dropped,
/// unshown). `d=0` holds it for more chunks; otherwise it is complete.
fn kitty_chunk(draft: &mut Option<Draft>, pending: &mut Vec<ProgramNotification>, raw: &[u8]) {
    // Both semicolons are required: without the second there is no payload.
    let Some(semi) = raw.iter().position(|&b| b == b';') else { return };
    let (meta, payload) = (&raw[..semi], &raw[semi + 1..]);
    let (mut id, mut done, mut base64, mut part) = (&b""[..], true, false, Payload::Title);
    for kv in meta.split(|&b| b == b':') {
        let (key, value) = match kv.iter().position(|&b| b == b'=') {
            Some(n) => (&kv[..n], &kv[n + 1..]),
            None => (kv, &b""[..]),
        };
        match key {
            b"i" => id = value,
            b"d" => done = value != b"0",
            b"e" => base64 = value == b"1",
            b"p" => {
                part = match value {
                    b"title" => Payload::Title,
                    b"body" => Payload::Body,
                    b"icon" | b"buttons" => Payload::Unshown,
                    // `close`, `alive`, a `?` query, an unknown type: not part
                    // of a notification — ignored whole, never answered.
                    _ => return,
                }
            }
            // Keys JeTTY does not support (actions, urgency, sound, …).
            _ => {}
        }
    }
    let mut d = match draft.take() {
        Some(d) if d.id == id => d,
        _ => Draft { id: id.to_vec(), ..Draft::default() },
    };
    match part {
        Payload::Title => d.title.add(payload, base64),
        Payload::Body => d.body.add(payload, base64),
        Payload::Unshown => {}
    }
    if done {
        d.title.flush();
        d.body.flush();
        push(pending, &d.title.bytes, &d.body.bytes);
    } else {
        *draft = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run whole OSC payloads (prefix stripped) through the assembler.
    fn run(chunks: &[(NotifyOsc, &[u8])]) -> Vec<ProgramNotification> {
        let mut n = Notifications::default();
        for &(osc, payload) in chunks {
            n.begin(b"");
            n.capture(payload);
            n.finish(osc);
        }
        n.take()
    }

    fn note(title: &str, body: &str) -> ProgramNotification {
        ProgramNotification { title: title.to_string(), body: body.to_string() }
    }

    #[test]
    fn the_three_forms() {
        assert_eq!(run(&[(NotifyOsc::Osc9, b"Build finished")]), [note("", "Build finished")]);
        assert_eq!(run(&[(NotifyOsc::Osc777, b"Claude Code;Needs your input")]), [note("Claude Code", "Needs your input")]);
        assert_eq!(run(&[(NotifyOsc::Osc777, b"a;b;c")]), [note("a", "b;c")], "the body keeps later semicolons");
        assert_eq!(run(&[(NotifyOsc::Osc777, b"Only a title")]), [note("Only a title", "")]);
        assert_eq!(run(&[(NotifyOsc::Osc99, b";Hello")]), [note("Hello", "")], "p=title is the default");
        assert_eq!(run(&[(NotifyOsc::Osc99, b"p=body;Hi there")]), [note("", "Hi there")]);
        // Nothing to show: dropped.
        for (osc, payload) in [(NotifyOsc::Osc9, &b""[..]), (NotifyOsc::Osc777, b";"), (NotifyOsc::Osc99, b";")] {
            assert!(run(&[(osc, payload)]).is_empty(), "{osc:?} {payload:?}");
        }
    }

    #[test]
    fn conemu_commands_are_not_notifications() {
        for payload in [&b"9;\"/home/u\""[..], b"1;100", b"2;\"txt\"", b"3;tab", b"4", b"5", b"10", b"12", b"10;1"] {
            assert!(run(&[(NotifyOsc::Osc9, payload)]).is_empty(), "{}", String::from_utf8_lossy(payload));
        }
        // Text that merely starts with a number is text.
        for payload in [&b"13;x"[..], b"0;x", b"42 files", b"1 new message", b"123;x", b"4x"] {
            let got = run(&[(NotifyOsc::Osc9, payload)]);
            assert_eq!(got, [note("", &String::from_utf8_lossy(payload))]);
        }
    }

    #[test]
    fn kitty_chunks_join_by_id_until_done() {
        let got = run(&[
            (NotifyOsc::Osc99, b"i=1:d=0;Hello "),
            (NotifyOsc::Osc99, b"i=1:d=0;world"),
            (NotifyOsc::Osc99, b"i=1:d=0:p=icon:e=1;iVBORw0KGgo="),
            (NotifyOsc::Osc99, b"i=1:p=body;It works"),
        ]);
        assert_eq!(got, [note("Hello world", "It works")]);
        // Another id's chunk drops the unfinished notification.
        let got = run(&[(NotifyOsc::Osc99, b"i=a:d=0;Lost"), (NotifyOsc::Osc99, b"i=b;Kept")]);
        assert_eq!(got, [note("Kept", "")]);
        // An unfinished one shows nothing.
        assert!(run(&[(NotifyOsc::Osc99, b"i=1:d=0;Pending")]).is_empty());
        // A chunk's buttons can end it.
        let got = run(&[(NotifyOsc::Osc99, b"i=x:d=0;Title"), (NotifyOsc::Osc99, b"i=x:p=buttons;Yes\xe2\x80\xa8No")]);
        assert_eq!(got, [note("Title", "")]);
    }

    #[test]
    fn kitty_base64_decodes_across_any_split() {
        // "Héllo wörld" encoded, then cut at every point: chunked after
        // encoding, a group may be split between two chunks.
        let enc = b"SMOpbGxvIHfDtnJsZA==";
        for cut in 0..=enc.len() {
            let first = [&b"i=7:d=0:e=1;"[..], &enc[..cut]].concat();
            let second = [&b"i=7:e=1;"[..], &enc[cut..]].concat();
            let got = run(&[(NotifyOsc::Osc99, &first), (NotifyOsc::Osc99, &second)]);
            assert_eq!(got, [note("Héllo wörld", "")], "cut at {cut}");
        }
        // Chunked before encoding (each chunk padded), and without padding.
        let got = run(&[(NotifyOsc::Osc99, b"d=0:e=1;SGk="), (NotifyOsc::Osc99, b"e=1;IHRoZXJl")]);
        assert_eq!(got, [note("Hi there", "")]);
        assert_eq!(run(&[(NotifyOsc::Osc99, b"e=1:p=body;SGk")]), [note("", "Hi")]);
        // A decoded line break is a space; undecodable base64 adds nothing.
        assert_eq!(run(&[(NotifyOsc::Osc99, b"e=1;b25lCnR3bw==")]), [note("one two", "")]);
        assert!(run(&[(NotifyOsc::Osc99, b"e=1;!!!!")]).is_empty());
    }

    #[test]
    fn kitty_requests_that_are_not_notifications_are_ignored() {
        for payload in [
            &b"i=1:p=close;"[..],
            b"i=1:p=alive;",
            b"i=1:p=?;",
            b"p=whatever;text",
            b"no second semicolon",
        ] {
            assert!(run(&[(NotifyOsc::Osc99, payload)]).is_empty(), "{}", String::from_utf8_lossy(payload));
        }
        // Unsupported keys are ignored, the rest still works.
        let got = run(&[(NotifyOsc::Osc99, b"a=report:o=unfocused:u=2:w=5000:zz:i=9;Done")]);
        assert_eq!(got, [note("Done", "")]);
        // A query in the middle leaves the notification being assembled be.
        let got = run(&[
            (NotifyOsc::Osc99, b"i=1:d=0;Part one"),
            (NotifyOsc::Osc99, b"i=2:p=?;"),
            (NotifyOsc::Osc99, b"i=1;, two"),
        ]);
        assert_eq!(got, [note("Part one, two", "")]);
    }

    #[test]
    fn untrusted_text_is_made_safe_and_short() {
        // Controls the scanner kept out (C0) never matter; what is left of the
        // hiding / reordering characters is dropped, and the text capped.
        let got = run(&[(NotifyOsc::Osc777, "Up\u{202E}date\u{200B};pay\u{2066}now\u{7f}\u{85}".as_bytes())]);
        assert_eq!(got, [note("Update", "paynow")]);
        let long = "x".repeat(1000);
        let got = run(&[(NotifyOsc::Osc777, format!("{long};{long}").as_bytes())]);
        assert_eq!(got[0].title.chars().count(), NOTIFICATION_TITLE_MAX_CHARS);
        assert_eq!(got[0].body.chars().count(), NOTIFICATION_BODY_MAX_CHARS);
        // Invalid UTF-8 shows as a replacement char, never as raw bytes.
        assert_eq!(run(&[(NotifyOsc::Osc9, b"bad \xff byte")]), [note("", "bad \u{fffd} byte")]);
        // Only invisible chars: nothing to show.
        assert!(run(&[(NotifyOsc::Osc9, "\u{200B}\u{FEFF} ".as_bytes())]).is_empty());
    }

    #[test]
    fn capture_keeps_a_bounded_head_without_c0() {
        let mut n = Notifications::default();
        n.begin(b"a\x01b");
        n.capture(b"\nc\r");
        n.capture(&vec![b'z'; 3 * CAPTURE_MAX_BYTES]);
        assert_eq!(&n.capture[..3], b"abc", "vte ignores C0 controls in an OSC, and so here");
        assert_eq!(n.capture.len(), CAPTURE_MAX_BYTES);
        n.finish(NotifyOsc::Osc9);
        assert!(n.capture.is_empty());
        let got = n.take();
        assert_eq!(got.len(), 1);
        assert!(got[0].body.starts_with("abczz"));
        assert!(n.take().is_empty(), "consumed");
    }

    #[test]
    fn a_flood_keeps_a_few_and_parts_stay_bounded() {
        let mut n = Notifications::default();
        for i in 0..100 {
            n.begin(b"");
            n.capture(format!("note {i}").as_bytes());
            n.finish(NotifyOsc::Osc9);
        }
        let got = n.take();
        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got[0].body, "note 0", "the first ones are kept");
        // An endless `d=0` stream grows nothing past the part cap.
        for _ in 0..100 {
            n.begin(b"");
            n.capture(&[&b"i=1:d=0;"[..], &[b'y'; 4000]].concat());
            n.finish(NotifyOsc::Osc99);
        }
        assert!(n.draft.as_ref().is_some_and(|d| d.title.bytes.len() == PART_MAX_BYTES));
    }
}
