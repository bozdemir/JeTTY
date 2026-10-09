/// Thin wrapper around `arboard::Clipboard`.
///
/// IMPORTANT (X11): the clipboard contents are served by the *owning process*
/// for as long as its `Clipboard` instance stays alive. A fresh `Clipboard`
/// created per call and dropped at the end of `set()` would relinquish the X11
/// selection immediately, so pasting into another app yields nothing. We
/// therefore keep ONE long-lived `Clipboard` for the whole process (a
/// `thread_local`, since all clipboard access happens on the UI thread) so the
/// copied text keeps being served while Jetty runs.
///
/// Two selections: CLIPBOARD (explicit Copy/Paste) and, on X11/Wayland, PRIMARY
/// (the select-to-copy, middle-click-to-paste convention). On Wayland the
/// X11 path is used through XWayland, which bridges both selections to native
/// apps; arboard's `wayland-data-control` backend is deliberately NOT enabled —
/// it serves a copy by forking the process, and a fork of JeTTY would inherit
/// every PTY master fd (shells would not see their tab close).
use std::cell::RefCell;

use arboard::Clipboard;

thread_local! {
    /// Opened on first use. `None` while no clipboard is available (e.g. a
    /// headless session) — every operation then degrades to a silent no-op,
    /// and the next one tries to open it again: one failed open (a display
    /// that was not up yet) never disables copy and paste for the session.
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

/// Run `f` on the clipboard, opening it first when it is not open yet.
fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> Option<R>) -> Option<R> {
    CLIPBOARD.with(|cell| f(open_lazily(&mut cell.borrow_mut(), || Clipboard::new().ok())?))
}

/// The value in `slot`, created by `open` while there is none: a failed open
/// is tried again on the next call, never remembered.
fn open_lazily<T>(slot: &mut Option<T>, open: impl FnOnce() -> Option<T>) -> Option<&mut T> {
    if slot.is_none() {
        *slot = open();
    }
    slot.as_mut()
}

/// Write `text` to the system clipboard. Errors are silently discarded.
pub fn set(text: &str) {
    with_clipboard(|cb| cb.set_text(text.to_owned()).ok());
}

/// Read a `String` from the system clipboard. Returns `None` on error or when
/// the clipboard contains no text.
pub fn get() -> Option<String> {
    with_clipboard(|cb| cb.get_text().ok())
}

/// Write `text` to the PRIMARY selection — the copy-on-select target, pasted
/// by a middle click — leaving the clipboard alone. Platforms without a
/// primary selection (macOS, Windows) use the clipboard instead, which is their
/// own copy-on-select convention.
pub fn set_primary(text: &str) {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
    {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        with_clipboard(|cb| cb.set().clipboard(LinuxClipboardKind::Primary).text(text.to_owned()).ok());
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
    set(text);
}

/// Read the PRIMARY selection (what a middle click pastes): the text most
/// recently selected in any app. Falls back to the clipboard where there is no
/// primary selection (see [`set_primary`]).
pub fn get_primary() -> Option<String> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
    {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        with_clipboard(|cb| cb.get().clipboard(LinuxClipboardKind::Primary).text().ok())
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
    get()
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
