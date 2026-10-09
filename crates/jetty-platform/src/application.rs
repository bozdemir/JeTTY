//! The application as a whole, apart from its windows. Only macOS has such a
//! thing: AppKit keeps an ACTIVE application separate from its key window,
//! hides and un-hides applications, and the Dock talks to the application, not
//! to a window. Everywhere else these are no-ops.
//!
//! Is this a violation of the no-platform-specific-code rule? No, for the same
//! reasons as `set_window_fullscreen`'s macOS branch: per-OS, not per-desktop,
//! and only documented APIs — winit's own `ActiveEventLoopExtMacOS`, AppKit's
//! `NSApplication`, Foundation's `NSAppleEventManager`.

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

/// Call `on_reopen` whenever macOS asks the running JeTTY to "reopen": a click
/// on its Dock icon, `open -a JeTTY`, a launch from Spotlight, Launchpad or
/// Finder. LaunchServices sends the running process the `kAEReopenApplication`
/// Apple event instead of starting a second one, so the single-instance socket
/// never hears of it, and winit (0.30) neither handles the event nor lets an
/// app set its own `NSApplicationDelegate` (its own must stay installed) — a
/// click on the Dock icon of a hidden JeTTY only activated it, with no window.
/// This installs a handler for that one event with `NSAppleEventManager`.
///
/// Call once the application has finished launching (from `resumed`): AppKit
/// installs its own handler while launching, and the later one wins. The
/// callback runs on the main thread, inside the event loop. A no-op off macOS.
pub fn on_reopen(on_reopen: impl Fn() + 'static) {
    #[cfg(target_os = "macos")]
    macos::on_reopen(Box::new(on_reopen));
    #[cfg(not(target_os = "macos"))]
    let _ = on_reopen;
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{class, define_class, msg_send, sel, DefinedClass, MainThreadMarker};
    use objc2_app_kit::NSApplication;
    use winit::event_loop::ActiveEventLoop;
    use winit::platform::macos::ActiveEventLoopExtMacOS;

    /// `kCoreEventClass` and `kAEReopenApplication`: the four-character codes
    /// ('aevt', 'rapp') of the reopen Apple event.
    const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
    const REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");

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

    struct Reopen {
        run: Box<dyn Fn()>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and
        // `ReopenHandler` does not implement Drop.
        #[unsafe(super(NSObject))]
        #[thread_kind = objc2::MainThreadOnly]
        #[ivars = Reopen]
        struct ReopenHandler;

        impl ReopenHandler {
            // SAFETY: an NSAppleEventManager handler's signature —
            // `-(void)handle:(NSAppleEventDescriptor *)event
            // withReplyEvent:(NSAppleEventDescriptor *)reply`, both taken as
            // nullable so a nil descriptor can never become a reference.
            #[unsafe(method(handleReopen:withReplyEvent:))]
            fn handle_reopen(&self, _event: Option<&AnyObject>, _reply: Option<&AnyObject>) {
                (self.ivars().run)();
            }
        }
    );

    pub(super) fn on_reopen(run: Box<dyn Fn()>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let handler = mtm.alloc::<ReopenHandler>().set_ivars(Reopen { run });
        // SAFETY: NSObject's designated initializer.
        let handler: Retained<ReopenHandler> = unsafe { msg_send![super(handler), init] };
        // SAFETY: the documented `+[NSAppleEventManager sharedAppleEventManager]`
        // and `-setEventHandler:andSelector:forEventClass:andEventID:` (the
        // class and id are 32-bit four-character codes); the selector is the
        // handler method defined above, with the signature it requires.
        unsafe {
            let manager: Retained<AnyObject> =
                msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            let _: () = msg_send![
                &*manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleReopen:withReplyEvent:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: REOPEN_APPLICATION,
            ];
        }
        // NSAppleEventManager does not retain its handlers: this one lives as
        // long as the process.
        std::mem::forget(handler);
    }
}
