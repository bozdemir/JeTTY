/// The system clipboard: CLIPBOARD (explicit Copy/Paste) and, on X11/Wayland,
/// PRIMARY (the select-to-copy, middle-click-to-paste convention).
///
/// * A native Wayland session (JeTTY's windows are Wayland clients): a
///   data-device client on winit's own `wl_display` — smithay-clipboard, the
///   core `wl_data_device` and `zwp_primary_selection` protocols on a worker
///   thread with its own event queue (see [`init`]). The XWayland route arboard
///   takes sees nothing a native Wayland app copied: KWin and wlroots hand a
///   Wayland-owned selection to X11 clients only while an X11 window has the
///   focus, so a paste got nothing, or JeTTY's own stale copy. arboard's
///   `wayland-data-control` backend is deliberately NOT enabled either — it
///   serves a copy by forking the process, and a fork of JeTTY would inherit
///   every PTY master fd (shells would not see their tab close).
/// * X11, macOS: `arboard::Clipboard`.
///
/// IMPORTANT: the clipboard contents are served by the *owning process* for as
/// long as its clipboard object stays alive. A fresh one created per call and
/// dropped at the end of `set()` would relinquish the selection immediately, so
/// pasting into another app yields nothing. We therefore keep ONE long-lived
/// clipboard for the whole process (a `thread_local`, since all clipboard
/// access happens on the UI thread) so the copied text keeps being served
/// while JeTTY runs.
use std::cell::RefCell;

use arboard::Clipboard;

/// Which selection an operation is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    /// Explicit Copy / Paste.
    Clipboard,
    /// What a middle click pastes. Platforms without one (macOS, Windows) use
    /// the clipboard instead, which is their own copy-on-select convention.
    Primary,
}

/// The UI thread's clipboard connections.
#[derive(Default)]
struct Selections {
    /// A native Wayland session's clipboard ([`init`]); `None` elsewhere.
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
    wayland: Option<wayland::Clipboard>,
    /// Opened on first use. `None` while no clipboard is available (e.g. a
    /// headless session) — every operation then degrades to a silent no-op,
    /// and the next one tries to open it again: one failed open (a display
    /// that was not up yet) never disables copy and paste for the session.
    arboard: Option<Clipboard>,
}

thread_local! {
    static SELECTIONS: RefCell<Selections> = RefCell::default();
}

/// Connect to a native Wayland session's clipboard, on the connection of
/// winit's event loop (`display`). Elsewhere arboard opens on first use.
pub fn init(display: winit::event_loop::OwnedDisplayHandle) {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
    if let Some(native) = wayland::Native::connect(display) {
        let clipboard = wayland::Clipboard::spawn(native, wayland::READ_TIMEOUT);
        SELECTIONS.with(|cell| cell.borrow_mut().wayland = clipboard);
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
    let _ = display;
}

/// The value in `slot`, created by `open` while there is none: a failed open
/// is tried again on the next call, never remembered.
fn open_lazily<T>(slot: &mut Option<T>, open: impl FnOnce() -> Option<T>) -> Option<&mut T> {
    if slot.is_none() {
        *slot = open();
    }
    slot.as_mut()
}

/// Make `text` the `sel` selection. Errors are silently discarded.
fn store(sel: Selection, text: &str) {
    SELECTIONS.with(|cell| {
        let s = &mut *cell.borrow_mut();
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
        if let Some(w) = &s.wayland {
            w.store(sel, text.to_owned());
            return;
        }
        let Some(cb) = open_lazily(&mut s.arboard, || Clipboard::new().ok()) else { return };
        let _ = match sel {
            Selection::Clipboard => cb.set_text(text.to_owned()),
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
            Selection::Primary => {
                use arboard::{LinuxClipboardKind, SetExtLinux};
                cb.set().clipboard(LinuxClipboardKind::Primary).text(text.to_owned())
            }
            #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
            Selection::Primary => cb.set_text(text.to_owned()),
        };
    });
}

/// The text of the `sel` selection: `None` on error or when it holds no text.
fn load(sel: Selection) -> Option<String> {
    SELECTIONS.with(|cell| {
        let s = &mut *cell.borrow_mut();
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
        if let Some(w) = &mut s.wayland {
            return w.load(sel);
        }
        let cb = open_lazily(&mut s.arboard, || Clipboard::new().ok())?;
        match sel {
            Selection::Clipboard => cb.get_text(),
            #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
            Selection::Primary => {
                use arboard::{GetExtLinux, LinuxClipboardKind};
                cb.get().clipboard(LinuxClipboardKind::Primary).text()
            }
            #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
            Selection::Primary => cb.get_text(),
        }
        .ok()
    })
}

/// Write `text` to the system clipboard. Errors are silently discarded.
pub fn set(text: &str) {
    store(Selection::Clipboard, text);
}

/// Read a `String` from the system clipboard. Returns `None` on error or when
/// the clipboard contains no text.
pub fn get() -> Option<String> {
    load(Selection::Clipboard)
}

/// Write `text` to the PRIMARY selection — the copy-on-select target, pasted
/// by a middle click — leaving the clipboard alone. Platforms without a
/// primary selection (macOS, Windows) use the clipboard instead, which is their
/// own copy-on-select convention.
pub fn set_primary(text: &str) {
    store(Selection::Primary, text);
}

/// Read the PRIMARY selection (what a middle click pastes): the text most
/// recently selected in any app. Falls back to the clipboard where there is no
/// primary selection (see [`set_primary`]).
pub fn get_primary() -> Option<String> {
    load(Selection::Primary)
}

/// The native Wayland clipboard: smithay-clipboard's data-device client on
/// winit's own `wl_display`, driven from a thread of its own so that a paste
/// waits at most [`READ_TIMEOUT`]. smithay-clipboard's read blocks until the app
/// that owns the selection has sent all of it, and a frozen app never does —
/// that froze every JeTTY window until the app came back or died.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
mod wayland {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::Selection;

    /// How long a paste waits for the owner of the selection to send it — the
    /// same 4 s arboard's X11 read gives the owner to start.
    pub const READ_TIMEOUT: Duration = Duration::from_secs(4);

    /// What the clipboard thread drives: smithay-clipboard ([`Native`]), or a
    /// stand-in in the tests.
    pub trait Backend {
        fn store(&self, sel: Selection, text: String);
        fn load(&self, sel: Selection) -> Option<String>;
    }

    enum Request {
        Store(Selection, String),
        Load(Selection),
    }

    /// The UI thread's end of the clipboard thread.
    pub struct Clipboard {
        requests: mpsc::Sender<Request>,
        replies: mpsc::Receiver<Option<String>>,
        timeout: Duration,
        /// A read gave up waiting and still runs on the clipboard thread: until
        /// its (late) reply arrives, reads give up at once instead of queueing
        /// behind it, and that reply is dropped — never pasted by a later read.
        stalled: bool,
    }

    impl Clipboard {
        /// Serve `backend` from a thread of its own (it ends with this end), a
        /// read waiting at most `timeout`. `None` when no thread can start.
        pub fn spawn<B: Backend + Send + 'static>(backend: B, timeout: Duration) -> Option<Self> {
            let (requests, rx) = mpsc::channel();
            let (tx, replies) = mpsc::channel();
            std::thread::Builder::new()
                .name("jetty-clipboard".into())
                .spawn(move || {
                    for request in rx {
                        match request {
                            Request::Store(sel, text) => backend.store(sel, text),
                            Request::Load(sel) => {
                                if tx.send(backend.load(sel)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                })
                .ok()?;
            Some(Clipboard { requests, replies, timeout, stalled: false })
        }

        /// Make `text` the `sel` selection. Never waits.
        pub fn store(&self, sel: Selection, text: String) {
            let _ = self.requests.send(Request::Store(sel, text));
        }

        /// The text of the `sel` selection: `None` when there is none, it is
        /// not text, or its owner did not send it in time.
        pub fn load(&mut self, sel: Selection) -> Option<String> {
            if self.stalled {
                self.replies.try_recv().ok()?;
                self.stalled = false;
            }
            self.requests.send(Request::Load(sel)).ok()?;
            match self.replies.recv_timeout(self.timeout) {
                Ok(text) => text,
                Err(_) => {
                    self.stalled = true;
                    None
                }
            }
        }
    }

    /// smithay-clipboard and the display its worker runs on, dropped in that
    /// order (fields drop in declaration order).
    pub struct Native {
        clipboard: smithay_clipboard::Clipboard,
        /// Keeps winit's `wl_display` connected while `clipboard` uses it.
        _display: winit::event_loop::OwnedDisplayHandle,
    }

    impl Native {
        /// The clipboard of `display` when it is a Wayland display; `None` on
        /// X11 (arboard serves that).
        pub fn connect(display: winit::event_loop::OwnedDisplayHandle) -> Option<Self> {
            use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            let ptr = match display.display_handle().ok()?.as_raw() {
                RawDisplayHandle::Wayland(h) => h.display.as_ptr(),
                _ => return None,
            };
            // SAFETY: `ptr` is the live `wl_display` of winit's connection, and
            // `_display` holds that connection for as long as `clipboard` (and
            // its worker thread, joined when it drops) lives.
            let clipboard = unsafe { smithay_clipboard::Clipboard::new(ptr) };
            Some(Native { clipboard, _display: display })
        }
    }

    impl Backend for Native {
        fn store(&self, sel: Selection, text: String) {
            match sel {
                Selection::Clipboard => self.clipboard.store(text),
                Selection::Primary => self.clipboard.store_primary(text),
            }
        }

        fn load(&self, sel: Selection) -> Option<String> {
            match sel {
                Selection::Clipboard => self.clipboard.load(),
                Selection::Primary => self.clipboard.load_primary(),
            }
            .ok()
        }
    }
}

/// Where a finished mouse selection is copied (config key `copy_on_select`):
/// `"primary"` (default — the X11/Wayland select-to-copy convention: a middle
/// click pastes it, the clipboard is left alone), `"clipboard"` (overwrite the
/// clipboard, as JeTTY did before v0.26 — and, as then, a middle click in JeTTY
/// pastes the clipboard), `"both"`, or `"off"`. Platforms without a primary
/// selection (macOS, Windows) treat `"primary"` as the clipboard, their own
/// copy-on-select convention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyOnSelect {
    #[default]
    Primary,
    Clipboard,
    Both,
    Off,
}

impl CopyOnSelect {
    /// The config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            CopyOnSelect::Primary => "primary",
            CopyOnSelect::Clipboard => "clipboard",
            CopyOnSelect::Both => "both",
            CopyOnSelect::Off => "off",
        }
    }

    /// Lenient parse (case-insensitive). Anything unknown is the default, so a
    /// typo never fails a config load.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "clipboard" => CopyOnSelect::Clipboard,
            "both" => CopyOnSelect::Both,
            "off" | "none" | "false" => CopyOnSelect::Off,
            _ => CopyOnSelect::Primary,
        }
    }

    /// Whether JeTTY's middle click pastes the CLIPBOARD rather than the PRIMARY
    /// selection: under `"clipboard"`, where a selection goes. PRIMARY there is
    /// never written by JeTTY, so it held whatever ANOTHER app selected last —
    /// a middle click pasted that stale text instead of the selection just
    /// made. Every other mode keeps the X11 convention.
    pub fn middle_click_reads_clipboard(self) -> bool {
        self == CopyOnSelect::Clipboard
    }

    /// `(primary, clipboard)`: which selections a copy-on-select writes on a
    /// platform that `has_primary` (X11/Wayland) or not (macOS, Windows — where
    /// `Primary` means the clipboard and `Both` writes it once).
    pub fn targets(self, has_primary: bool) -> (bool, bool) {
        match (self, has_primary) {
            (CopyOnSelect::Off, _) => (false, false),
            (CopyOnSelect::Primary, true) => (true, false),
            (CopyOnSelect::Clipboard, _) => (false, true),
            (CopyOnSelect::Both, true) => (true, true),
            (CopyOnSelect::Primary | CopyOnSelect::Both, false) => (false, true),
        }
    }
}

impl serde::Serialize for CopyOnSelect {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for CopyOnSelect {
    /// Accepts the string spellings and, leniently, a bool (`true` = primary,
    /// `false` = off).
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Bool(bool),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Str(s) => CopyOnSelect::parse(&s),
            Raw::Bool(true) => CopyOnSelect::Primary,
            Raw::Bool(false) => CopyOnSelect::Off,
        })
    }
}

/// Whether this platform has a separate PRIMARY selection.
pub const HAS_PRIMARY: bool =
    cfg!(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))));

/// What JeTTY's middle click pastes under `copy_on_select` `mode` (see
/// [`CopyOnSelect::middle_click_reads_clipboard`]).
pub fn get_for_middle_click(mode: CopyOnSelect) -> Option<String> {
    if mode.middle_click_reads_clipboard() {
        get()
    } else {
        get_primary()
    }
}

/// Copy a finished mouse selection per the user's `copy_on_select` policy.
pub fn copy_on_select(text: &str, mode: CopyOnSelect) {
    let (primary, clipboard) = mode.targets(HAS_PRIMARY);
    if primary {
        set_primary(text);
    }
    if clipboard {
        set(text);
    }
}

#[cfg(test)]
mod open_tests {
    use super::open_lazily;

    #[test]
    fn a_failed_open_is_retried_on_the_next_use() {
        let mut slot: Option<u32> = None;
        let mut tries = 0;
        assert_eq!(open_lazily(&mut slot, || { tries += 1; None }), None, "no clipboard yet");
        assert_eq!(open_lazily(&mut slot, || { tries += 1; Some(7) }).copied(), Some(7), "the next use opens it");
        assert_eq!(open_lazily(&mut slot, || { tries += 1; Some(8) }).copied(), Some(7), "and keeps it");
        assert_eq!(tries, 2, "an open clipboard is never reopened");
    }
}

#[cfg(all(test, unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
mod wayland_tests {
    use super::wayland::{Backend, Clipboard};
    use super::Selection;
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    /// A stand-in for the app that owns the selection (no real clipboard is
    /// touched): copies land in `stored`, and every read waits for its answer
    /// on `answers` — a frozen app never sends one.
    struct Owner {
        stored: mpsc::Sender<(Selection, String)>,
        asked: mpsc::Sender<Selection>,
        answers: Mutex<mpsc::Receiver<Option<String>>>,
    }

    impl Backend for Owner {
        fn store(&self, sel: Selection, text: String) {
            let _ = self.stored.send((sel, text));
        }

        fn load(&self, sel: Selection) -> Option<String> {
            let _ = self.asked.send(sel);
            self.answers.lock().unwrap().recv().ok().flatten()
        }
    }

    #[test]
    fn a_paste_never_waits_on_a_frozen_owner_past_the_timeout() {
        let (stored_tx, stored) = mpsc::channel();
        let (asked_tx, asked) = mpsc::channel();
        let (answer, answers) = mpsc::channel();
        let owner = Owner { stored: stored_tx, asked: asked_tx, answers: Mutex::new(answers) };
        let mut cb = Clipboard::spawn(owner, Duration::from_millis(500)).unwrap();
        let wait = Duration::from_secs(10);

        // An owner that answers: the paste gets its text.
        answer.send(Some("one".into())).unwrap();
        assert_eq!(cb.load(Selection::Clipboard).as_deref(), Some("one"));
        assert_eq!(asked.recv_timeout(wait), Ok(Selection::Clipboard));

        // A frozen one: the paste gives up after the timeout…
        assert_eq!(cb.load(Selection::Primary), None);
        assert_eq!(asked.recv_timeout(wait), Ok(Selection::Primary));
        // …and while that read still hangs, the next one gives up at once
        // instead of queueing behind it.
        assert_eq!(cb.load(Selection::Clipboard), None);
        // A copy still goes through, once the hung read is over.
        cb.store(Selection::Clipboard, "copied".into());

        // The owner comes back: its late answer is dropped, never pasted.
        answer.send(Some("late".into())).unwrap();
        assert_eq!(stored.recv_timeout(wait), Ok((Selection::Clipboard, "copied".to_string())));
        assert!(asked.try_recv().is_err(), "no read was queued behind the hung one");
        answer.send(Some("two".into())).unwrap();
        assert_eq!(cb.load(Selection::Clipboard).as_deref(), Some("two"));
    }
}

#[cfg(test)]
mod copy_on_select_tests {
    use super::CopyOnSelect;

    #[test]
    fn targets_follow_the_policy_on_x11_and_macos() {
        // X11 / Wayland: a real PRIMARY selection exists.
        assert_eq!(CopyOnSelect::Primary.targets(true), (true, false));
        assert_eq!(CopyOnSelect::Clipboard.targets(true), (false, true));
        assert_eq!(CopyOnSelect::Both.targets(true), (true, true));
        assert_eq!(CopyOnSelect::Off.targets(true), (false, false));
        // macOS / Windows: "primary" means the clipboard; "both" writes it once.
        assert_eq!(CopyOnSelect::Primary.targets(false), (false, true));
        assert_eq!(CopyOnSelect::Both.targets(false), (false, true));
        assert_eq!(CopyOnSelect::Clipboard.targets(false), (false, true));
        assert_eq!(CopyOnSelect::Off.targets(false), (false, false));
    }

    #[test]
    fn a_middle_click_pastes_where_the_selection_went() {
        // "clipboard" never writes PRIMARY, so reading it pasted another app's
        // stale selection instead of the one just made in JeTTY.
        assert!(CopyOnSelect::Clipboard.middle_click_reads_clipboard());
        for m in [CopyOnSelect::Primary, CopyOnSelect::Both, CopyOnSelect::Off] {
            assert!(!m.middle_click_reads_clipboard(), "{m:?} keeps the X11 convention");
        }
    }

    #[test]
    fn parses_leniently_and_round_trips() {
        for m in [CopyOnSelect::Primary, CopyOnSelect::Clipboard, CopyOnSelect::Both, CopyOnSelect::Off] {
            assert_eq!(CopyOnSelect::parse(m.as_str()), m);
        }
        assert_eq!(CopyOnSelect::parse(" Clipboard "), CopyOnSelect::Clipboard);
        assert_eq!(CopyOnSelect::parse("typo"), CopyOnSelect::Primary);
        #[derive(serde::Deserialize)]
        struct W {
            v: CopyOnSelect,
        }
        let b: W = toml::from_str("v = false").unwrap();
        assert_eq!(b.v, CopyOnSelect::Off);
        let s: W = toml::from_str("v = \"both\"").unwrap();
        assert_eq!(s.v, CopyOnSelect::Both);
    }
}
