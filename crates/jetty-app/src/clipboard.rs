/// Thin wrapper around `arboard::Clipboard`.
///
/// IMPORTANT (X11): the clipboard contents are served by the *owning process*
/// for as long as its `Clipboard` instance stays alive. A fresh `Clipboard`
/// created per call and dropped at the end of `set()` would relinquish the X11
/// selection immediately, so pasting into another app yields nothing. We
/// therefore keep ONE long-lived `Clipboard` for the whole process (a
/// `thread_local`, since all clipboard access happens on the UI thread) so the
/// copied text keeps being served while Jetty runs.
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
