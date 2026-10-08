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
    /// Built lazily on first use. `None` if no clipboard is available (e.g. a
    /// headless session) — every operation then degrades to a silent no-op.
    static CLIPBOARD: RefCell<Option<Clipboard>> = RefCell::new(Clipboard::new().ok());
}

/// Write `text` to the system clipboard. Errors are silently discarded.
pub fn set(text: &str) {
    CLIPBOARD.with(|cell| {
        if let Some(cb) = cell.borrow_mut().as_mut() {
            let _ = cb.set_text(text.to_owned());
        }
    });
}

/// Read a `String` from the system clipboard. Returns `None` on error or when
/// the clipboard contains no text.
pub fn get() -> Option<String> {
    CLIPBOARD.with(|cell| cell.borrow_mut().as_mut()?.get_text().ok())
}

/// Write `text` to the PRIMARY selection — the copy-on-select target, pasted
/// by a middle click — leaving the clipboard alone. Platforms without a
/// primary selection (macOS, Windows) use the clipboard instead, which is their
/// own copy-on-select convention.
pub fn set_primary(text: &str) {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))))]
    {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        CLIPBOARD.with(|cell| {
            if let Some(cb) = cell.borrow_mut().as_mut() {
                let _ = cb.set().clipboard(LinuxClipboardKind::Primary).text(text.to_owned());
            }
        });
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
        CLIPBOARD
            .with(|cell| cell.borrow_mut().as_mut()?.get().clipboard(LinuxClipboardKind::Primary).text().ok())
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "emscripten")))))]
    get()
}
