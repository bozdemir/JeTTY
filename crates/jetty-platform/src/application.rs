//! The application as a whole, apart from its windows. Only macOS has such a
//! thing: AppKit keeps an ACTIVE application separate from its key window, and
//! hides and un-hides applications. Everywhere else these are no-ops.
//!
//! Is this a violation of the no-platform-specific-code rule? No, for the same
//! reasons as `set_window_fullscreen`'s macOS branch: per-OS, not per-desktop,
//! and only documented APIs — winit's own `ActiveEventLoopExtMacOS` and AppKit's
//! `NSApplication`.

use winit::event_loop::ActiveEventLoop;

/// After the user hid JeTTY's last window (F9, `jetty --hide`): give the
/// keyboard back to the app the user came from.
///
/// macOS keeps an application ACTIVE after its last window is ordered out, so
/// JeTTY stayed frontmost — its menu bar — with no window, and typed keys went
/// nowhere until another app was clicked. Hiding the application (Cmd+H's
/// `[NSApp hide:]`, through winit's `ActiveEventLoopExtMacOS::hide_application`)
/// makes AppKit activate the next app in line. Only while JeTTY IS the active
/// app: a `jetty --hide` typed in another app leaves that one alone. The caller
/// skips it while another JeTTY window (Settings, a detached tab) is open —
/// hiding the application would take that one away too.
pub fn hide_application(event_loop: &ActiveEventLoop) {
    #[cfg(target_os = "macos")]
    macos::hide_application(event_loop);
    #[cfg(not(target_os = "macos"))]
    let _ = event_loop;
}

/// Before a window is shown: undo [`hide_application`] (or the user's Cmd+H).
/// AppKit keeps a hidden application's windows off screen, and winit's
/// `focus_window()` does nothing for a window that is not visible, so a summon
/// after a hide would show nothing. Un-hides WITHOUT activating: the summon's
/// own focus request activates, as it always did. A no-op when JeTTY is not
/// hidden, and off macOS.
pub fn unhide_application() {
    #[cfg(target_os = "macos")]
    macos::unhide_application();
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use winit::event_loop::ActiveEventLoop;
    use winit::platform::macos::ActiveEventLoopExtMacOS;

    pub(super) fn hide_application(event_loop: &ActiveEventLoop) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if NSApplication::sharedApplication(mtm).isActive() {
            event_loop.hide_application();
        }
    }

    pub(super) fn unhide_application() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        if app.isHidden() {
            app.unhideWithoutActivation();
        }
    }
}
