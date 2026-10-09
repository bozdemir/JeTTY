use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::error::OsError;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Icon, Window};

/// Decode the embedded JeTTY app icon into a winit `Icon` (shown in the
/// taskbar / Alt-Tab / when minimized). The 256px RGBA PNG is baked into the
/// binary so there is nothing to install. Returns `None` if decoding fails,
/// in which case the window simply has no custom icon.
fn app_icon() -> Option<Icon> {
    let bytes: &[u8] = include_bytes!("../../../assets/icons/jetty-256.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    buf.truncate(info.buffer_size());
    Icon::from_rgba(buf, info.width, info.height).ok()
}

/// The smallest a terminal window (main or detached) may be, in logical px.
pub const MIN_TERMINAL_SIZE: (f64, f64) = (200.0, 120.0);
/// The smallest the Settings window may be, in logical px.
pub const MIN_SETTINGS_SIZE: (f64, f64) = (200.0, 200.0);

/// Build the main window: a borderless (client-side decorations) window with
/// our custom titlebar + the JeTTY app icon.
///
/// Returns the OS error on failure instead of panicking: this builder is also
/// used at runtime (tab detach), where a transient window-creation failure (X
/// server fd/resource exhaustion, compositor restart, WM limits) must abort
/// only that action, not kill every shell in the app. The startup call site can
/// still treat an `Err` as fatal.
pub fn build_window(
    event_loop: &ActiveEventLoop,
    title: &str,
    size: (u32, u32),
) -> Result<Arc<Window>, OsError> {
    build_window_with_visibility(event_loop, title, size, true)
}

/// [`build_window`], optionally created UNMAPPED (`visible == false`): a hidden
/// start (`jetty --background`) must not flash a window at login; the first
/// summon maps it.
pub fn build_window_with_visibility(
    event_loop: &ActiveEventLoop,
    title: &str,
    size: (u32, u32),
    visible: bool,
) -> Result<Arc<Window>, OsError> {
    let attrs = Window::default_attributes()
        .with_visible(visible)
        .with_title(title)
        .with_window_icon(app_icon())
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_resizable(true)
        .with_min_inner_size(LogicalSize::new(MIN_TERMINAL_SIZE.0, MIN_TERMINAL_SIZE.1))
        // Client-side decorations: drop the OS title bar/frame and draw our own
        // custom titlebar (min/max/close + drag) in the tab strip. Transparency
        // keeps the runtime opacity working and the rounded corners.
        .with_decorations(false)
        .with_transparent(true);
    event_loop.create_window(attrs).map(Arc::new)
}

/// Build a decorated utility window (the settings dialog), sized by the caller
/// to fit its content. RESIZABLE: the caller resizes it live when the panel
/// grows (e.g. a larger/wider UI font) so nothing is clipped, and the user may
/// resize it too. A low min-inner-size keeps a programmatic shrink valid while
/// stopping the user from collapsing it to nothing. Also carries the app icon.
///
/// Returns the OS error on failure (used at runtime on every settings-window
/// open) so a transient failure aborts only that action rather than panicking.
pub fn build_fixed_window(
    event_loop: &ActiveEventLoop,
    title: &str,
    size: (u32, u32),
) -> Result<Arc<Window>, OsError> {
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_window_icon(app_icon())
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_min_inner_size(LogicalSize::new(MIN_SETTINGS_SIZE.0, MIN_SETTINGS_SIZE.1))
        .with_resizable(true);
    event_loop.create_window(attrs).map(Arc::new)
}

/// Whether `pos` (a window outer top-left, physical px) lies inside the monitor
/// rect described by `mon_pos` (its origin, physical px — nonzero on secondary
/// monitors, NEGATIVE for a monitor placed to the left of the primary) and
/// `mon_size`.
///
/// Half-open on the right/bottom edges (`x < mon_x + w`), so a position exactly
/// on the right/bottom boundary belongs to the NEXT monitor — byte-identical to
/// the containment test `jetty-app`'s `pos_on_some_monitor` has always used, and
/// shared with it so "which screen" is decided in exactly one place.
///
/// Pure integer arithmetic: unit-testable with synthetic rects, no window needed.
pub fn pos_in_monitor_rect(pos: (i32, i32), mon_pos: (i32, i32), mon_size: (u32, u32)) -> bool {
    pos.0 >= mon_pos.0
        && pos.0 < mon_pos.0 + mon_size.0 as i32
        && pos.1 >= mon_pos.1
        && pos.1 < mon_pos.1 + mon_size.1 as i32
}

/// The monitor a window should be placed on.
///
/// Resolution order:
/// 1. `current_monitor()` — the platform's own answer, when it has one AND it is
///    still connected (see below).
/// 2. the monitor CONTAINING the window's last outer position. A HIDDEN window
///    (the dropdown between summons) reports no `current_monitor` on some
///    platforms, so without this fallback it would be treated as being on the
///    PRIMARY monitor and re-appear on the wrong screen for multi-monitor users.
/// 3. the first available monitor (also the Wayland path, where `outer_position`
///    is `Err` — accepted degradation, same as the F9 hotkey).
///
/// `current_monitor()` is NOT trusted blindly: on X11 it returns a CACHED handle
/// (winit's `last_monitor`, refreshed only by the WM's ConfigureNotify), so for a
/// window hidden while its monitor was unplugged it still names the vanished
/// output — and centering/docking against that rect maps the window off-screen.
/// It is therefore matched against the LIVE `available_monitors()` list and the
/// live handle (fresh geometry) is used; an unmatched one falls through to 2/3.
///
/// Shared by `center_window`, `dock_window_top` and the fullscreen path so all
/// three agree on "which screen". Lives in `jetty-platform` (not `jetty-app`)
/// because the crate dependency only points one way: `jetty-app` →
/// `jetty-platform`, never the reverse.
pub fn monitor_for_window(win: &Window) -> Option<MonitorHandle> {
    let available: Vec<MonitorHandle> = win.available_monitors().collect();
    let last_pos = win.outer_position().ok().map(|a| (a.x, a.y));
    pick_monitor(win.current_monitor(), &available, |m| {
        last_pos.is_some_and(|a| {
            let p = m.position();
            let s = m.size();
            pos_in_monitor_rect(a, (p.x, p.y), (s.width, s.height))
        })
    })
}

/// Pure resolution core of [`monitor_for_window`], generic over the monitor
/// type so it is unit-testable without a display. `current` is honored only if
/// it is still among `available` (and the LIVE entry is returned, so a monitor
/// re-plugged with a new geometry is placed against its current rect); else the
/// first available monitor `contains_last_pos` accepts; else the first one.
pub fn pick_monitor<M: PartialEq + Clone>(
    current: Option<M>,
    available: &[M],
    contains_last_pos: impl Fn(&M) -> bool,
) -> Option<M> {
    current
        .and_then(|c| available.iter().find(|m| **m == c).cloned())
        .or_else(|| available.iter().find(|m| contains_last_pos(m)).cloned())
        .or_else(|| available.first().cloned())
}

/// Put `win` into (or out of) whole-monitor fullscreen, cross-platform, through
/// winit ONLY — no desktop-environment / compositor / window-manager specific
/// code anywhere.
///
/// PRECONDITION: `on == true` requires a MAPPED (visible) window; `on == false`
/// must be issued while the window is still mapped. The app enforces this by
/// only ever entering fullscreen after `set_visible(true)` and only ever exiting
/// before `set_visible(false)` (rule F0). Two reasons this matters:
///   * macOS `set_simple_fullscreen(true)` panics when the window has no
///     `screen()`, i.e. while it is ordered out;
///   * X11 resolves `Borderless(None)` from the window's frame, which an
///     unmapped window does not have.
///
/// Everywhere except macOS: `Fullscreen::Borderless(None)`.
///   * X11 — `_NET_WM_STATE_FULLSCREEN`; covers panels/struts. Works.
///   * Wayland — `xdg_toplevel.set_fullscreen`. Works (unlike
///     `set_outer_position` / `request_inner_size`, which are no-ops there).
///   * Windows — works (and disables the screen saver).
///
/// `None` (rather than an explicit handle) is deliberate: winit's X11 backend
/// resolves `Borderless(None)` to the same monitor `current_monitor()` reports,
/// while `Borderless(Some(handle))` silently no-ops if the handle cannot be
/// resolved. macOS ignores the handle entirely (simple fullscreen always uses
/// the window's current screen). Passing `None` therefore has no failure mode.
///
/// `Fullscreen::Exclusive` is deliberately NEVER used: it needs a
/// `VideoModeHandle`, changes the display mode, is a documented no-op on
/// Wayland, and on macOS disables task switching.
///
/// macOS uses `WindowExtMacOS::set_simple_fullscreen` instead of `Borderless`.
/// `Borderless` on macOS enters a NATIVE FULLSCREEN SPACE: an animated (~0.5 s)
/// transition onto its own desktop. That is wrong for a quick-summon terminal —
/// `set_visible(false)` (`orderOut`) on a window that owns a space leaves an
/// EMPTY space behind and bounces the user to another desktop. Simple
/// fullscreen is the pre-Lion behaviour: resize the window to the screen frame,
/// auto-hide the dock/menu bar, no new space — so show/hide keeps working
/// exactly as it does windowed.
///
/// The macOS branch tests the window's STATE, never
/// `set_simple_fullscreen`'s return value: that returns `false` for a redundant
/// enter AND a redundant exit as well as for "already in a native space", so a
/// `if !set_simple_fullscreen(on) { fall through }` shape would turn a redundant
/// enter into a NATIVE space — the exact catastrophe this branch exists to
/// avoid. We fall through to `set_fullscreen` only when the window genuinely is
/// in a native space already (the user hit the green title-bar button), so the
/// request is still honoured.
///
/// Two documented macOS warts, called out so a future change does not trip over
/// them:
///   * simple fullscreen restores the saved CONTENT rect as the FRAME rect. That
///     is harmless ONLY because JeTTY builds its windows `with_decorations(false)`
///     (content == frame); it becomes a bug the day decorations are enabled.
///   * it clears the window's Movable/Resizable style bits and restores them on
///     exit, so the app's "drag / resize edges are inert while fullscreen" rules
///     are belt-and-braces on macOS and load-bearing on X11.
///
/// Is `cfg(target_os = "macos")` a violation of the no-platform-specific-code
/// rule? No. That rule forbids DESKTOP-ENVIRONMENT specific code (KDE/GNOME/
/// compositor hacks). This is (1) per-OS, not per-DE, (2) a first-party
/// documented winit platform trait — still entirely inside the winit
/// abstraction, (3) the same category as the macOS `cfg` code that already ships
/// (`keymap::push_cmd` seeds Cmd chords only on macOS; `Chord::pretty` prints
/// "Cmd+" vs "Super+"), and (4) confined to this ONE function in the platform
/// crate — which is what `jetty-platform` is for.
pub fn set_window_fullscreen(win: &Window, on: bool) {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowExtMacOS;
        if win.fullscreen().is_none() {
            // Not in a native fullscreen space: use SIMPLE fullscreen, and only
            // when the state actually changes (a redundant call is a no-op that
            // reports failure, which must never be mistaken for "fall through").
            if win.simple_fullscreen() != on {
                win.set_simple_fullscreen(on);
            }
            return;
        }
        // Genuinely in a native space — fall through and honour the request
        // through the native API so the state can still be left.
    }
    if on {
        win.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
    } else {
        win.set_fullscreen(None);
    }
}

/// Bring an already-shown window to the front and give it keyboard focus, as a
/// DIRECT USER REQUEST: the summon hotkey, `jetty --show`, a command typed in a
/// detached window that acts on the main one. Never for something the user did
/// not just ask for — that would be focus stealing.
///
/// winit's `focus_window()` asks with the EWMH `_NET_ACTIVE_WINDOW` message,
/// source indication 1 ("an application asks") and no timestamp. A window
/// manager with focus-stealing prevention judges that against the window's last
/// user interaction: KWin's default level refused it whenever another window had
/// been used since, so F9 on a JeTTY left behind another window did nothing
/// (seen under KWin 6.6 on a nested X server — source 1 refused, source 2
/// raised and focused). On X11 this sends the same standard message with source
/// indication 2, the one pagers and taskbars send for a user's click, which
/// every EWMH window manager honors (KWin, Mutter, Xfwm4, Openbox, i3). Not a
/// desktop-specific hack: one freedesktop message, no WM is named. Wayland,
/// macOS and an unreachable X server keep winit's `focus_window()`.
pub fn activate_window(win: &Window) {
    #[cfg(all(unix, not(target_os = "macos")))]
    if x11::request_activation(win) {
        return;
    }
    if hide_kind(win) == HideKind::Minimize {
        // Wayland: a client cannot raise or un-minimize itself without an
        // activation token the compositor handed out for a user action, and
        // winit's focus_window() is a no-op there. Ask for attention instead —
        // the compositor flags the window (its taskbar entry) — so a summon is
        // never silently lost.
        win.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        return;
    }
    win.focus_window();
}

/// What a window keeps across scale-factor (DPI) changes: its size in LOGICAL
/// px — the size the user chose — and the PHYSICAL size last requested for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpiSize {
    pub logical: (f64, f64),
    pub requested: (u32, u32),
}

/// Most of the monitor a scale change may size a window to: the rest leaves
/// room for panels, so the window manager has no reason to clamp the request
/// (a clamp would lose the size the window comes back to).
const DPI_MAX_MONITOR_FRACTION: f64 = 0.9;

/// The size a window takes when its scale factor changes to `new_scale`,
/// given its `current` physical size, the scale that size is at
/// (`current_scale`: the window's own `scale_factor()` when the event arrives —
/// on X11 still the old scale for the first event and the new one for the
/// repeat, on Wayland and macOS already the new one) and what the previous
/// change requested (`prev`).
///
/// The window keeps its LOGICAL size. That is `prev.logical` while the window
/// still has the size `prev` requested — so neither a clamp nor a repeated
/// event can erode it — and otherwise `current / current_scale` (the user
/// resized it since). Why not winit's own suggestion (`current ×
/// new/old`): its X11 backend delivers every Xft.dpi change twice — the second
/// time from the next ConfigureNotify, with its cached monitor scale still the
/// old one — so following it applied the factor twice; and a window manager
/// clamping the scaled-up size to the screen lost the size on the way back.
/// Live (Xvfb + KWin, 1× → 2.25× → 1×) a 1000×640 window ended at 379×213.
///
/// The physical size is [`dpi_physical`]: clamped inside `monitor` (physical
/// px) and above `min_logical` at the new scale. Pure, so the whole sequence is
/// a unit test.
pub fn dpi_change_size(
    current: (u32, u32),
    current_scale: f64,
    new_scale: f64,
    prev: Option<DpiSize>,
    monitor: Option<(u32, u32)>,
    min_logical: (f64, f64),
) -> DpiSize {
    let old = sane_scale(current_scale);
    let logical = match prev {
        Some(p) if p.requested == current => p.logical,
        _ => (f64::from(current.0) / old, f64::from(current.1) / old),
    };
    DpiSize { logical, requested: dpi_physical(logical, new_scale, monitor, min_logical) }
}

/// `logical` at `scale` in physical px, at most [`DPI_MAX_MONITOR_FRACTION`] of
/// `monitor` and at least `min_logical` (also scaled).
pub fn dpi_physical(logical: (f64, f64), scale: f64, monitor: Option<(u32, u32)>, min_logical: (f64, f64)) -> (u32, u32) {
    let s = sane_scale(scale);
    let fit = |len: f64, mon: Option<u32>, min: f64| {
        let max = mon.map_or(f64::INFINITY, |m| (f64::from(m) * DPI_MAX_MONITOR_FRACTION).floor());
        (len * s).min(max).max(min * s).round() as u32
    };
    (fit(logical.0, monitor.map(|m| m.0), min_logical.0), fit(logical.1, monitor.map(|m| m.1), min_logical.1))
}

fn sane_scale(s: f64) -> f64 {
    if s.is_finite() && s > 0.0 {
        s
    } else {
        1.0
    }
}

/// What hiding a window does on its display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideKind {
    /// The window is unmapped (X11, macOS): off screen until JeTTY shows it.
    Unmap,
    /// The window is MINIMIZED (Wayland). A Wayland client cannot unmap its
    /// own toplevel — winit's `set_visible` is a no-op there — and the
    /// compositor may bring a minimized window back without JeTTY asking (its
    /// taskbar entry, Alt+Tab): the app learns of that from the focus the
    /// window then receives.
    Minimize,
}

/// How [`hide_window`] hides `win`.
pub fn hide_kind(win: &Window) -> HideKind {
    use raw_window_handle::HasWindowHandle;
    let handle = win.window_handle().ok().map(|h| h.as_raw());
    hide_kind_for(handle.as_ref())
}

/// Pure core of [`hide_kind`] (unit-tested without a display).
fn hide_kind_for(handle: Option<&raw_window_handle::RawWindowHandle>) -> HideKind {
    match handle {
        Some(raw_window_handle::RawWindowHandle::Wayland(_)) => HideKind::Minimize,
        _ => HideKind::Unmap,
    }
}

/// Take `win` off screen (the summon hide, the focus-loss auto-hide): unmapped
/// where the platform can ([`HideKind::Unmap`]), minimized on Wayland, where
/// `set_visible(false)` alone left the window on screen.
pub fn hide_window(win: &Window) {
    win.set_visible(false);
    if hide_kind(win) == HideKind::Minimize {
        win.set_minimized(true);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask};

    /// EWMH `_NET_ACTIVE_WINDOW` source indication for a request made on the
    /// user's behalf (pagers, taskbars — and a global hotkey).
    const SOURCE_USER: u32 = 2;

    /// Send `_NET_ACTIVE_WINDOW` (source 2) for `win` over a short-lived
    /// connection of its own (a raise is rare: one hotkey press). `false` when
    /// `win` is not an X11 window (Wayland) or the server can't be reached, so
    /// the caller falls back to winit.
    pub(super) fn request_activation(win: &Window) -> bool {
        let Ok(handle) = win.window_handle() else {
            return false;
        };
        let xid = match handle.as_raw() {
            RawWindowHandle::Xlib(h) => h.window as u32,
            RawWindowHandle::Xcb(h) => h.window.get(),
            _ => return false,
        };
        send(xid).is_ok()
    }

    fn send(xid: u32) -> Result<(), Box<dyn std::error::Error>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots.get(screen).ok_or("no X screen")?.root;
        let atom = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;
        // [source, timestamp (CurrentTime), requestor's active window (none), 0, 0]
        let event =
            ClientMessageEvent::new(32, xid, atom, [SOURCE_USER, x11rb::CURRENT_TIME, 0, 0, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?
        // Round-trip before the connection closes: a request merely flushed and
        // then followed by the close was dropped unprocessed by the server (on
        // Xvfb no message reached the WM until the request was checked or the
        // connection lingered). `check` also surfaces an X error (BadWindow).
        .check()?;
        Ok(())
    }
}

#[cfg(test)]
mod dpi_tests {
    use super::{dpi_change_size, dpi_physical, DpiSize};

    const MON: Option<(u32, u32)> = Some((1920, 1080));
    const MIN: (f64, f64) = (200.0, 120.0);

    /// One `ScaleFactorChanged`, then the window manager answering with the
    /// size it really gives the window (`wm` clamps a request to the screen,
    /// like KWin does).
    fn change(
        current_scale: f64,
        new: f64,
        current: (u32, u32),
        prev: Option<DpiSize>,
        wm: impl Fn((u32, u32)) -> (u32, u32),
    ) -> (DpiSize, (u32, u32)) {
        let d = dpi_change_size(current, current_scale, new, prev, MON, MIN);
        (d, wm(d.requested))
    }

    #[test]
    fn a_scale_change_and_back_restores_the_window_size() {
        // The live repro (Xvfb + KWin, Xft.dpi 96 → 216 → 96): winit's X11
        // backend sends every change TWICE (the second from the next
        // ConfigureNotify, its cached monitor scale still the old one), each
        // suggesting `current × new/old`. Followed blindly, with KWin clamping
        // the 2.25× size to the screen in between, the 1000×640 window ended at
        // 379×213 in the corner.
        let screen = |(w, h): (u32, u32)| (w.min(1920), h.min(1080));
        let (d1, size) = change(1.0, 2.25, (1000, 640), None, screen);
        assert_eq!(d1.logical, (1000.0, 640.0));
        assert!(size.0 <= 1920 && size.1 <= 1080, "stays on the screen: {size:?}");
        // winit's duplicate (the window's scale already 2.25): nothing more.
        let (d2, size2) = change(2.25, 2.25, size, Some(d1), screen);
        assert_eq!((d2.requested, size2), (size, size), "no second scaling");
        // Back to 1×: the size the user had, not the clamped one shrunk.
        let (d3, size3) = change(2.25, 1.0, size2, Some(d2), screen);
        assert_eq!(size3, (1000, 640));
        let (_, size4) = change(1.0, 1.0, size3, Some(d3), screen);
        assert_eq!(size4, (1000, 640), "the duplicate on the way back too");
    }

    #[test]
    fn a_size_that_fits_keeps_its_logical_size_exactly() {
        let big = Some((3840, 2160));
        let d = dpi_change_size((800, 500), 1.0, 2.0, None, big, MIN);
        assert_eq!(d.requested, (1600, 1000));
        let back = dpi_change_size((1600, 1000), 2.0, 1.0, Some(d), big, MIN);
        assert_eq!(back.requested, (800, 500));
        // Wayland / macOS: the size is already at the new scale when the event
        // comes — the logical size is the same, nothing is scaled twice.
        let wl = dpi_change_size((1600, 1000), 2.0, 2.0, None, big, MIN);
        assert_eq!((wl.logical, wl.requested), ((800.0, 500.0), (1600, 1000)));
    }

    #[test]
    fn a_resize_by_the_user_between_changes_is_the_new_intent() {
        let big = Some((3840, 2160));
        let d = dpi_change_size((800, 500), 1.0, 2.0, None, big, MIN);
        // The user dragged the window to 1400×900 at 2×: that is the size to keep.
        let back = dpi_change_size((1400, 900), 2.0, 1.0, Some(d), big, MIN);
        assert_eq!((back.logical, back.requested), ((700.0, 450.0), (700, 450)));
    }

    #[test]
    fn the_physical_size_is_clamped_inside_the_monitor_and_above_the_minimum() {
        // 90% of the monitor at most: room for panels, so the WM need not clamp.
        assert_eq!(dpi_physical((1000.0, 640.0), 2.25, MON, MIN), (1728, 972));
        assert_eq!(dpi_physical((1000.0, 640.0), 2.0, None, MIN), (2000, 1280), "no monitor: no clamp");
        assert_eq!(dpi_physical((50.0, 40.0), 2.0, MON, MIN), (400, 240), "the min size scales too");
        // Bogus scales fall back to 1.
        assert_eq!(dpi_physical((1000.0, 640.0), f64::NAN, MON, MIN), (1000, 640));
        assert_eq!(dpi_physical((1000.0, 640.0), 0.0, MON, MIN), (1000, 640));
    }
}

#[cfg(test)]
mod hide_tests {
    use super::{hide_kind_for, HideKind};
    use raw_window_handle::{RawWindowHandle, WaylandWindowHandle, XcbWindowHandle, XlibWindowHandle};
    use std::num::NonZeroU32;
    use std::ptr::NonNull;

    #[test]
    fn wayland_windows_hide_by_minimizing_everything_else_unmaps() {
        // winit's set_visible is a no-op on Wayland: a hide that only asked for
        // it left the window on screen (frozen — JeTTY stops painting a window
        // it believes hidden). There the hide minimizes.
        let wl = RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::dangling()));
        assert_eq!(hide_kind_for(Some(&wl)), HideKind::Minimize);
        let xlib = RawWindowHandle::Xlib(XlibWindowHandle::new(7));
        let xcb = RawWindowHandle::Xcb(XcbWindowHandle::new(NonZeroU32::new(7).unwrap()));
        assert_eq!(hide_kind_for(Some(&xlib)), HideKind::Unmap);
        assert_eq!(hide_kind_for(Some(&xcb)), HideKind::Unmap);
        // No handle (macOS before creation, an error): today's unmap.
        assert_eq!(hide_kind_for(None), HideKind::Unmap);
    }
}

#[cfg(test)]
mod tests {
    use super::{pick_monitor, pos_in_monitor_rect};

    /// Synthetic monitor for `pick_monitor`: equality by `id` (like winit's X11
    /// CRTC-id equality), geometry carried so the LIVE entry can be told apart
    /// from a stale cached one.
    #[derive(Clone, Debug)]
    struct Mon {
        id: u32,
        pos: (i32, i32),
        size: (u32, u32),
    }
    impl PartialEq for Mon {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }
    fn mon(id: u32, x: i32, w: u32) -> Mon {
        Mon { id, pos: (x, 0), size: (w, 1080) }
    }
    fn contains(last: (i32, i32)) -> impl Fn(&Mon) -> bool {
        move |m: &Mon| pos_in_monitor_rect(last, m.pos, m.size)
    }

    #[test]
    fn pick_monitor_honors_a_still_connected_current_monitor() {
        let avail = [mon(1, 0, 1920), mon(2, 1920, 2560)];
        let got = pick_monitor(Some(mon(2, 1920, 2560)), &avail, contains((10, 10)));
        assert_eq!(got.map(|m| m.id), Some(2));
    }

    #[test]
    fn pick_monitor_rejects_an_unplugged_cached_current_monitor() {
        // X11's current_monitor() still names output 2 after it was unplugged
        // while hidden; only output 1 is live. The window's last position sits
        // on the vanished monitor, so the fallback is the first live one.
        let avail = [mon(1, 0, 1920)];
        let got = pick_monitor(Some(mon(2, 1920, 2560)), &avail, contains((2000, 10)));
        assert_eq!(got.map(|m| m.id), Some(1), "must never place on a vanished monitor");
    }

    #[test]
    fn pick_monitor_returns_the_live_geometry_for_a_replugged_monitor() {
        // Same output id, new geometry (re-plugged at another resolution): the
        // LIVE handle's rect is used, never the stale cached one.
        let avail = [mon(1, 0, 1920), mon(2, 1920, 3840)];
        let got = pick_monitor(Some(mon(2, 1920, 2560)), &avail, contains((0, 0))).unwrap();
        assert_eq!((got.id, got.size.0), (2, 3840));
    }

    #[test]
    fn pick_monitor_falls_back_to_the_monitor_containing_the_last_position() {
        let avail = [mon(1, 0, 1920), mon(2, 1920, 2560)];
        let got = pick_monitor(None, &avail, contains((2500, 100)));
        assert_eq!(got.map(|m| m.id), Some(2));
    }

    #[test]
    fn pick_monitor_falls_back_to_the_first_monitor_and_handles_none() {
        let avail = [mon(1, 0, 1920), mon(2, 1920, 2560)];
        assert_eq!(pick_monitor(None, &avail, |_: &Mon| false).map(|m| m.id), Some(1));
        assert!(pick_monitor(Some(mon(9, 0, 1)), &[], |_: &Mon| true).is_none());
    }

    #[test]
    fn pos_in_monitor_rect_contains_interior_and_origin() {
        // Primary monitor at the origin.
        assert!(pos_in_monitor_rect((0, 0), (0, 0), (1920, 1080)));
        assert!(pos_in_monitor_rect((100, 200), (0, 0), (1920, 1080)));
        assert!(pos_in_monitor_rect((1919, 1079), (0, 0), (1920, 1080)));
    }

    #[test]
    fn pos_in_monitor_rect_is_half_open_on_the_far_edges() {
        // The exact right/bottom boundary belongs to the NEXT monitor.
        assert!(!pos_in_monitor_rect((1920, 0), (0, 0), (1920, 1080)));
        assert!(!pos_in_monitor_rect((0, 1080), (0, 0), (1920, 1080)));
        // …and the next monitor claims it.
        assert!(pos_in_monitor_rect((1920, 0), (1920, 0), (2560, 1440)));
    }

    #[test]
    fn pos_in_monitor_rect_handles_a_negative_origin_left_monitor() {
        // A monitor placed to the LEFT of the primary has a negative origin.
        let mon = ((-1920, 0), (1920u32, 1080u32));
        assert!(pos_in_monitor_rect((-1920, 0), mon.0, mon.1));
        assert!(pos_in_monitor_rect((-1, 500), mon.0, mon.1));
        assert!(!pos_in_monitor_rect((0, 500), mon.0, mon.1), "x=0 is the primary");
        assert!(!pos_in_monitor_rect((-1921, 0), mon.0, mon.1));
        // Negative Y (monitor stacked above) too.
        assert!(pos_in_monitor_rect((10, -5), (0, -1080), (1920, 1080)));
        assert!(!pos_in_monitor_rect((10, 0), (0, -1080), (1920, 1080)));
    }

    #[test]
    fn pos_in_monitor_rect_rejects_everything_for_a_zero_sized_monitor() {
        assert!(!pos_in_monitor_rect((0, 0), (0, 0), (0, 0)));
    }
}
