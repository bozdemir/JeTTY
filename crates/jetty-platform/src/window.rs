use std::sync::Arc;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::error::OsError;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Icon, Window, WindowAttributes};

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

/// The name JeTTY's desktop entry is installed under (`assets/jetty.desktop`,
/// whose `StartupWMClass` is the same word). Every window carries it as its
/// Wayland app id and its X11 `WM_CLASS`, which is how a compositor or taskbar
/// finds the entry — the icon, the pinned launcher the window belongs to.
/// Without it Wayland windows had no app id at all, and X11 took the class
/// from the binary's file name: `JeTTY-0.29.0-x86_64.AppImage` for the
/// AppImage.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub const APP_ID: &str = "jetty";

/// Give `attrs` JeTTY's [`APP_ID`]. winit keeps ONE name for both backends, so
/// the Wayland call sets the X11 class too (`WM_CLASS` "jetty", "jetty").
fn with_app_id(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use winit::platform::wayland::WindowAttributesExtWayland;
        attrs.with_name(APP_ID, APP_ID)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    attrs
}

/// How [`build_window_with`] starts a terminal window besides its title and
/// size. The default is a plain visible window.
#[derive(Debug, Clone, Default)]
pub struct WindowStart {
    /// Created UNMAPPED: a hidden start (`jetty --background`) must not flash
    /// a window at login; the first summon maps it.
    pub hidden: bool,
    /// Created maximized: a Wayland summon builds the main window again
    /// ([`HideKind::Close`]) the way the user left it.
    pub maximized: bool,
    /// The xdg-activation token of the launch this window answers (the
    /// launcher's `XDG_ACTIVATION_TOKEN`). The compositor gets it with the new
    /// window, so a compositor that focuses new windows only for a token
    /// focuses this one. Wayland only: an X11 window ignores it.
    pub activation_token: Option<String>,
}

/// Build a terminal window (the main one, a detached tab's): a borderless
/// (client-side decorations) window with our custom titlebar + the JeTTY app
/// icon.
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
    build_window_with(event_loop, title, size, WindowStart::default())
}

/// [`build_window`], started per `start`: hidden, maximized, or handed the
/// launcher's activation token.
pub fn build_window_with(
    event_loop: &ActiveEventLoop,
    title: &str,
    size: (u32, u32),
    start: WindowStart,
) -> Result<Arc<Window>, OsError> {
    let attrs = with_app_id(Window::default_attributes())
        .with_visible(!start.hidden)
        .with_maximized(start.maximized)
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
    let attrs = with_activation_token(attrs, event_loop, start.activation_token);
    event_loop.create_window(attrs).map(Arc::new)
}

/// Hand `token` to the new window's compositor with `attrs`, where it can be
/// used ([`wayland_token`]).
fn with_activation_token(attrs: WindowAttributes, event_loop: &ActiveEventLoop, token: Option<String>) -> WindowAttributes {
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(token) = wayland_token(display_kind(event_loop).as_ref(), token) {
        use winit::platform::startup_notify::WindowAttributesExtStartupNotify;
        return attrs.with_activation_token(winit::window::ActivationToken::from_raw(token));
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = (event_loop, token);
    attrs
}

/// The activation token JeTTY itself was launched with, for its first window:
/// the launcher's `XDG_ACTIVATION_TOKEN` (an app menu, KRunner, a compositor's
/// `exec`). Wayland only — `None` on X11 and macOS, whose launch protocols
/// JeTTY leaves alone. The variable stays set: shells never inherit it
/// (`jetty-core`'s environment denylist).
pub fn launch_activation_token(event_loop: &ActiveEventLoop) -> Option<String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use winit::platform::startup_notify::EventLoopExtStartupNotify;
        let token = event_loop.read_token_from_env().map(|t| t.into_raw());
        wayland_token(display_kind(event_loop).as_ref(), token)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = event_loop;
        None
    }
}

/// The raw kind of display `event_loop` runs on (`None` if winit can't say).
#[cfg(all(unix, not(target_os = "macos")))]
fn display_kind(event_loop: &ActiveEventLoop) -> Option<raw_window_handle::RawDisplayHandle> {
    use raw_window_handle::HasDisplayHandle;
    event_loop.display_handle().ok().map(|h| h.as_raw())
}

/// `token`, when it can be used: a non-empty token on a Wayland display. On
/// X11 winit would treat it as a startup-notification id — not JeTTY's to
/// change there. Pure (unit-tested without a display).
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn wayland_token(display: Option<&raw_window_handle::RawDisplayHandle>, token: Option<String>) -> Option<String> {
    match display {
        Some(raw_window_handle::RawDisplayHandle::Wayland(_)) => token.filter(|t| !t.is_empty()),
        _ => None,
    }
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
    let attrs = with_app_id(Window::default_attributes())
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

/// What the desktop's own bars take from the edges of a monitor, physical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Insets {
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
}

/// A rectangle `(x, y, width, height)`, physical px.
type Rect = (i32, i32, u32, u32);

/// The part of `monitor` a window may cover without going under the
/// desktop's own bars, as `(origin, size)` in physical px: the macOS menu bar
/// and Dock (`NSScreen.visibleFrame`); on X11 the panels' EWMH struts, or the
/// window manager's `_NET_WORKAREA` where no client reserves space itself (a
/// top bar the compositor draws). The whole monitor elsewhere — on Wayland the
/// compositor places windows itself. Standard protocols only, no desktop
/// named; a few round trips on X11, so ask once per placement, never per
/// frame.
pub fn work_area(win: &Window, monitor: &MonitorHandle) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let (p, s) = (monitor.position(), monitor.size());
    let mon = (p.x, p.y, s.width, s.height);
    #[cfg(all(unix, not(target_os = "macos")))]
    let insets = x11::insets(win, mon);
    #[cfg(target_os = "macos")]
    let insets = macos::insets(monitor);
    #[cfg(not(unix))]
    let insets: Option<Insets> = None;
    let _ = win;
    let (x, y, w, h) = inset_rect(mon, insets.unwrap_or_default());
    (PhysicalPosition::new(x, y), PhysicalSize::new(w, h))
}

/// `mon` less `insets`, never smaller than 1×1.
fn inset_rect(mon: Rect, insets: Insets) -> Rect {
    let w = mon.2.saturating_sub(insets.left).saturating_sub(insets.right).max(1);
    let h = mon.3.saturating_sub(insets.top).saturating_sub(insets.bottom).max(1);
    (mon.0.saturating_add_unsigned(insets.left), mon.1.saturating_add_unsigned(insets.top), w, h)
}

/// What EWMH struts take from `mon`. Each strut is `_NET_WM_STRUT_PARTIAL`'s
/// twelve values — left, right, top and bottom, then where each runs along
/// its edge (left_start_y, left_end_y, right_start_y, right_end_y,
/// top_start_x, top_end_x, bottom_start_x, bottom_end_x) — in the coordinates
/// of the root window, `root` px wide and high: a top strut reserves the
/// root's top rows, not the monitor's. A strut counts on a monitor its range
/// overlaps and whose edge it reaches past; one that would leave nothing of
/// the monitor is ignored (a panel on an edge between two monitors, which
/// EWMH cannot express), as window managers do.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn strut_insets(mon: Rect, root: (u32, u32), struts: &[[u32; 12]]) -> Insets {
    let (mx, my) = (i64::from(mon.0), i64::from(mon.1));
    let (mw, mh) = (i64::from(mon.2), i64::from(mon.3));
    let (rw, rh) = (i64::from(root.0), i64::from(root.1));
    let overlaps = |start: u32, end: u32, from: i64, len: i64| i64::from(start) < from + len && i64::from(end) >= from;
    let depth = |d: i64, extent: i64| if d > 0 && d < extent { d as u32 } else { 0 };
    let mut ins = Insets::default();
    for &[left, right, top, bottom, ly0, ly1, ry0, ry1, tx0, tx1, bx0, bx1] in struts {
        if top > 0 && overlaps(tx0, tx1, mx, mw) {
            ins.top = ins.top.max(depth(i64::from(top) - my, mh));
        }
        if bottom > 0 && overlaps(bx0, bx1, mx, mw) {
            ins.bottom = ins.bottom.max(depth(my + mh - (rh - i64::from(bottom)), mh));
        }
        if left > 0 && overlaps(ly0, ly1, my, mh) {
            ins.left = ins.left.max(depth(i64::from(left) - mx, mw));
        }
        if right > 0 && overlaps(ry0, ry1, my, mh) {
            ins.right = ins.right.max(depth(mx + mw - (rw - i64::from(right)), mw));
        }
    }
    ins
}

/// `_NET_WM_STRUT`'s four values as a `_NET_WM_STRUT_PARTIAL` running along
/// the whole of each edge of a `root`-sized root window.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn full_strut(s: [u32; 4], root: (u32, u32)) -> [u32; 12] {
    let (w, h) = (root.0.saturating_sub(1), root.1.saturating_sub(1));
    [s[0], s[1], s[2], s[3], 0, h, 0, h, 0, w, 0, w]
}

/// What a window manager's work area (`_NET_WORKAREA`: one rectangle for the
/// whole desktop) takes from `mon`. One that misses the monitor says nothing
/// about it.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn workarea_insets(mon: Rect, area: Rect) -> Insets {
    let edges = |r: Rect| (i64::from(r.0), i64::from(r.1), i64::from(r.0) + i64::from(r.2), i64::from(r.1) + i64::from(r.3));
    let (mx0, my0, mx1, my1) = edges(mon);
    let (ax0, ay0, ax1, ay1) = edges(area);
    if ax0 >= mx1 || ax1 <= mx0 || ay0 >= my1 || ay1 <= my0 {
        return Insets::default();
    }
    let depth = |d: i64, extent: u32| if d > 0 && d < i64::from(extent) { d as u32 } else { 0 };
    Insets {
        top: depth(ay0 - my0, mon.3),
        bottom: depth(my1 - ay1, mon.3),
        left: depth(ax0 - mx0, mon.2),
        right: depth(mx1 - ax1, mon.2),
    }
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
/// macOS (a minimized window un-minimized first) and an unreachable X server
/// keep winit's `focus_window()`.
pub fn activate_window(win: &Window) {
    #[cfg(all(unix, not(target_os = "macos")))]
    if x11::request_activation(win) {
        return;
    }
    if hide_kind(win) == HideKind::Close {
        // Wayland: a client cannot raise or un-minimize itself without an
        // activation token the compositor handed out for a user action, and
        // winit's focus_window() is a no-op there. Ask for attention instead —
        // the compositor flags the window (its taskbar entry) — so a summon is
        // never silently lost. (The main window's summon builds the window
        // anew instead, which the compositor focuses: see `HideKind::Close`.)
        win.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        return;
    }
    // macOS: winit's focus_window() does nothing for a minimized window — it
    // stays in the Dock, as `jetty --show` left it. Bring it back first, as a
    // click on its Dock tile would (the X11 activation above does both).
    if win.is_minimized() == Some(true) {
        win.set_minimized(false);
    }
    win.focus_window();
}

/// Whether the X server still has its keyboard focus on one of `windows`.
///
/// After a `Focused(false)` this tells another client's keyboard GRAB from a
/// real focus change. A grab — a held global shortcut, a window manager's
/// keyboard-grabbing move or resize (Xfwm4, Openbox), Alt+Tab while the user
/// picks — makes the server send the focused window a FocusOut with mode
/// NotifyGrab while the input focus stays where it was, and winit reports that
/// as `Focused(false)` like any other (it does not filter by mode). Asking the
/// server is standard X11 (core GetInputFocus), no window manager is named.
///
/// One round trip on a connection opened on first use and kept: ask only when
/// a decision is due, never per event or per frame. `false` where there is no
/// such grab (Wayland, macOS), for windows that are not X11 windows, and when
/// the server can't be reached — the caller then takes the loss as real.
pub fn holds_input_focus(windows: &[&Window]) -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    return x11::focus_on(windows);
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = windows;
        false
    }
}

/// Tells JeTTY when the window manager minimizes one of its windows, or shows
/// it again — which winit does not on X11. A minimize unmaps the window, and X
/// sends no VisibilityNotify for a window that stops being viewable, so a
/// terminal minimized from its taskbar entry, a shortcut or "show desktop"
/// went on rendering its shell's output, and its paced animations, into a
/// window nobody could see. The window manager's ICCCM `WM_STATE` says it:
/// standard X11, no window manager is named. Zero cost while nothing changes —
/// a thread blocked on its own connection, never a poll.
///
/// Nothing to watch elsewhere: macOS reports a minimized window as occluded
/// itself, and a Wayland client is never told.
pub struct MinimizeWatch {
    #[cfg(all(unix, not(target_os = "macos")))]
    watch: x11::Watch,
}

impl MinimizeWatch {
    /// Watch `win`, calling `on_change(window, minimized)` from the watch's
    /// own thread whenever the window manager changes the state of a watched
    /// window. `None` where there is nothing to watch (not an X11 window), or
    /// when the X server can't be reached.
    pub fn start(win: &Window, on_change: impl Fn(winit::window::WindowId, bool) + Send + 'static) -> Option<Self> {
        #[cfg(all(unix, not(target_os = "macos")))]
        return x11::Watch::start(win, on_change).map(|watch| MinimizeWatch { watch });
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = (win, on_change);
            None
        }
    }

    /// Watch `win` too (a detached window), reporting through the same
    /// `on_change`.
    pub fn watch(&self, win: &Window) {
        #[cfg(all(unix, not(target_os = "macos")))]
        self.watch.add(win);
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        let _ = win;
    }
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

/// What hiding a window takes on its display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideKind {
    /// The window is unmapped (X11, macOS): off screen until JeTTY shows it.
    Unmap,
    /// The window is CLOSED (Wayland): the app drops it — tabs, shells and the
    /// GPU stay — and builds a new one on the next summon. A Wayland client
    /// can't take its toplevel off screen through winit (`set_visible` is a
    /// no-op there), a minimize is a request tiling compositors (sway, river,
    /// Hyprland, niri) ignore, and a compositor brings an existing window
    /// back to the front only for an activation token that winit can't hand
    /// it. A new window is placed and focused the way the compositor treats
    /// every new window — with the launcher's token (`jetty --toggle`
    /// forwards it, [`WindowStart::activation_token`]) where it wants one.
    Close,
}

/// How a hide takes `win` off screen.
pub fn hide_kind(win: &Window) -> HideKind {
    use raw_window_handle::HasWindowHandle;
    let handle = win.window_handle().ok().map(|h| h.as_raw());
    hide_kind_for(handle.as_ref())
}

/// Pure core of [`hide_kind`] (unit-tested without a display).
fn hide_kind_for(handle: Option<&raw_window_handle::RawWindowHandle>) -> HideKind {
    match handle {
        Some(raw_window_handle::RawWindowHandle::Wayland(_)) => HideKind::Close,
        _ => HideKind::Unmap,
    }
}

/// Take `win` off screen (the summon hide, the focus-loss auto-hide) where the
/// platform unmaps ([`HideKind::Unmap`]) — on X11 also withdrawn from the
/// window manager, which keeps no taskbar entry for it. A [`HideKind::Close`]
/// window goes when the app drops it.
pub fn hide_window(win: &Window) {
    win.set_visible(false);
    #[cfg(all(unix, not(target_os = "macos")))]
    x11::withdraw(win);
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    use winit::monitor::MonitorHandle;
    use winit::platform::macos::MonitorHandleExtMacOS;

    /// [`super::work_area`] on macOS: what the menu bar (and a notch) and the
    /// Dock take from `monitor` — the gap between its `NSScreen`'s frame and
    /// visible frame, in points, times the monitor's scale. AppKit's y axis
    /// points up: the menu bar is the gap at the top of the frame (its
    /// largest y). Borderless windows are never kept out of either by AppKit,
    /// so a Dropdown strip docked at the monitor's top sat under the menu bar.
    pub(super) fn insets(monitor: &MonitorHandle) -> Option<super::Insets> {
        MainThreadMarker::new()?;
        let screen = monitor.ns_screen()?;
        // SAFETY: winit hands out the NSScreen of a connected display, which
        // AppKit keeps alive in `+[NSScreen screens]`; it is read at once, on
        // the main thread (checked above).
        let screen: &NSScreen = unsafe { &*screen.cast::<NSScreen>() };
        let (frame, visible) = (screen.frame(), screen.visibleFrame());
        let scale = monitor.scale_factor();
        let px = |points: f64| (points.max(0.0) * scale).round() as u32;
        Some(super::Insets {
            top: px((frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height)),
            bottom: px(visible.origin.y - frame.origin.y),
            left: px(visible.origin.x - frame.origin.x),
            right: px((frame.origin.x + frame.size.width) - (visible.origin.x + visible.size.width)),
        })
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::Mutex;
    use winit::window::Window;
    use x11rb::connection::Connection;
    use x11rb::errors::ReplyError;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask, UnmapNotifyEvent, UNMAP_NOTIFY_EVENT};
    use x11rb::rust_connection::RustConnection;

    /// EWMH `_NET_ACTIVE_WINDOW` source indication for a request made on the
    /// user's behalf (pagers, taskbars — and a global hotkey).
    const SOURCE_USER: u32 = 2;

    /// The X id of `win`; `None` when it is not an X11 window (Wayland).
    fn xid(win: &Window) -> Option<u32> {
        match win.window_handle().ok()?.as_raw() {
            RawWindowHandle::Xlib(h) => Some(h.window as u32),
            RawWindowHandle::Xcb(h) => Some(h.window.get()),
            _ => None,
        }
    }

    x11rb::atom_manager! {
        /// The atoms the helper's requests name, interned in one batch.
        Atoms: AtomsCookie {
            _NET_ACTIVE_WINDOW,
            _NET_CLIENT_LIST,
            _NET_WM_STRUT_PARTIAL,
            _NET_WM_STRUT,
            _NET_WORKAREA,
            _NET_CURRENT_DESKTOP,
        }
    }

    /// JeTTY's own connection for the few requests winit has no API for — the
    /// activation, the withdraw, the focus question, the work area. Opened on
    /// the first one and kept: each costs a round trip or two, where a
    /// connection per request paid a socket, the Xauthority file and an atom
    /// lookup (0.5–3 ms) on every F9 summon. The UI thread is its only user.
    struct Helper {
        conn: RustConnection,
        root: u32,
        atoms: Atoms,
    }

    fn connect() -> Result<Helper, Box<dyn std::error::Error>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots.get(screen).ok_or("no X screen")?.root;
        let atoms = Atoms::new(&conn)?.reply()?;
        Ok(Helper { conn, root, atoms })
    }

    /// Run `request` on the kept connection, opening it first when there is
    /// none. A connection that broke is dropped, so the next request opens a
    /// new one. `None` when the server can't be reached or answered an error.
    fn with_helper<T>(request: impl FnOnce(&Helper) -> Result<T, ReplyError>) -> Option<T> {
        static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
        let mut slot = HELPER.lock().ok()?;
        if slot.is_none() {
            *slot = connect().ok();
        }
        let result = request(slot.as_ref()?);
        if let Err(ReplyError::ConnectionError(_)) = result {
            *slot = None;
        }
        result.ok()
    }

    /// A reply on winit's own connection: the server has then handled every
    /// request winit sent before it — the map of a summon, the unmap of a hide
    /// — so what JeTTY sends on its own connection next reaches the window
    /// manager after them. Both are flushed but need not be processed yet, and
    /// a window manager drops an activation for a window it has not been asked
    /// to manage. Any request with a reply does; this one asks for the
    /// window's position.
    fn after_winit(win: &Window) {
        let _ = win.inner_position();
    }

    /// Send `_NET_ACTIVE_WINDOW` (source 2) for `win`. `false` when `win` is
    /// not an X11 window (Wayland) or the server can't be reached, so the
    /// caller falls back to winit.
    pub(super) fn request_activation(win: &Window) -> bool {
        let Some(xid) = xid(win) else { return false };
        after_winit(win);
        with_helper(|h| {
            // [source, timestamp (CurrentTime), requestor's active window (none), 0, 0]
            let event = ClientMessageEvent::new(
                32,
                xid,
                h.atoms._NET_ACTIVE_WINDOW,
                [SOURCE_USER, x11rb::CURRENT_TIME, 0, 0, 0],
            );
            h.conn
                .send_event(false, h.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event)?
                // A round trip: it also surfaces an X error (BadWindow), and
                // then the caller falls back to winit.
                .check()
        })
        .is_some()
    }

    /// [`super::hide_window`]'s second half on X11 — ICCCM's withdraw: after
    /// its own unmap a client sends the root a synthetic UnmapNotify for the
    /// window, as Xlib's `XWithdrawWindow` does. It is the only word a window
    /// manager gets when the window was not mapped any more: a window it had
    /// MINIMIZED is unmapped already, so the client's unmap changes nothing.
    /// Without it the focus-loss auto-hide of a minimized terminal left a
    /// minimized taskbar entry behind, and a click on it showed the window for
    /// an instant before winit unmapped it again — the window manager then let
    /// go of it, and only the summon key brought it back. For a window that was
    /// on screen it repeats what the real UnmapNotify already said.
    pub(super) fn withdraw(win: &Window) {
        let Some(xid) = xid(win) else { return };
        after_winit(win);
        with_helper(|h| {
            let event = UnmapNotifyEvent {
                response_type: UNMAP_NOTIFY_EVENT,
                sequence: 0,
                event: h.root,
                window: xid,
                from_configure: false,
            };
            h.conn
                .send_event(false, h.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event)?
                .check()
        });
    }

    /// [`super::holds_input_focus`]: GetInputFocus names one of `windows`.
    pub(super) fn focus_on(windows: &[&Window]) -> bool {
        let xids: Vec<u32> = windows.iter().filter_map(|w| xid(w)).collect();
        if xids.is_empty() {
            return false;
        }
        with_helper(|h| Ok(h.conn.get_input_focus()?.reply()?.focus)).is_some_and(|focus| xids.contains(&focus))
    }

    /// [`super::work_area`] on X11: what the panels take from monitor `mon`
    /// (root px). The struts of every client the window manager lists, asked
    /// for in one batch — the requests go out together and the replies come
    /// back together; where none reserves space, the window manager's work
    /// area for the current desktop. `None` for a window that is not an X11
    /// one, or when the server can't be reached.
    pub(super) fn insets(win: &Window, mon: super::Rect) -> Option<super::Insets> {
        use x11rb::protocol::xproto::AtomEnum;
        xid(win)?;
        with_helper(|h| {
            let (c, a) = (&h.conn, &h.atoms);
            let clients = c.get_property(false, h.root, a._NET_CLIENT_LIST, AtomEnum::WINDOW, 0, u32::MAX)?;
            let geometry = c.get_geometry(h.root)?;
            let area = c.get_property(false, h.root, a._NET_WORKAREA, AtomEnum::CARDINAL, 0, u32::MAX)?;
            let desktop = c.get_property(false, h.root, a._NET_CURRENT_DESKTOP, AtomEnum::CARDINAL, 0, 1)?;
            let values = |reply: x11rb::protocol::xproto::GetPropertyReply| -> Vec<u32> {
                reply.value32().map(|v| v.collect()).unwrap_or_default()
            };
            let clients = values(clients.reply()?);
            let geometry = geometry.reply()?;
            let root = (u32::from(geometry.width), u32::from(geometry.height));
            let area = values(area.reply()?);
            let desktop = values(desktop.reply()?).first().copied().unwrap_or(0) as usize;
            let mut asked = Vec::with_capacity(clients.len());
            for &w in &clients {
                asked.push((
                    c.get_property(false, w, a._NET_WM_STRUT_PARTIAL, AtomEnum::CARDINAL, 0, 12)?,
                    c.get_property(false, w, a._NET_WM_STRUT, AtomEnum::CARDINAL, 0, 4)?,
                ));
            }
            let mut struts = Vec::new();
            for (partial, plain) in asked {
                // A client gone meanwhile answers BadWindow: it reserves nothing.
                let partial = partial.reply().ok().map(values).and_then(|v| <[u32; 12]>::try_from(v).ok());
                let plain = plain.reply().ok().map(values).and_then(|v| <[u32; 4]>::try_from(v).ok());
                if let Some(s) = partial.or_else(|| plain.map(|p| super::full_strut(p, root))) {
                    struts.push(s);
                }
            }
            Ok(if !struts.is_empty() {
                super::strut_insets(mon, root, &struts)
            } else if let Some(&[x, y, w, h]) = area.chunks_exact(4).nth(desktop) {
                super::workarea_insets(mon, (x as i32, y as i32, w, h))
            } else {
                super::Insets::default()
            })
        })
    }

    /// ICCCM `IconicState`, the first field of `WM_STATE`.
    const ICONIC_STATE: u32 = 3;

    /// Whether a window whose `WM_STATE` begins with `state` is minimized:
    /// IconicState. NormalState (1) is on screen, WithdrawnState (0) — or no
    /// `WM_STATE` at all — is a window the window manager does not manage: JeTTY
    /// hid it itself, or it was never shown.
    pub(super) fn iconic(state: Option<u32>) -> bool {
        state == Some(ICONIC_STATE)
    }

    /// [`super::MinimizeWatch`] on X11: a thread blocked on a connection of its
    /// own — no wakeup while nothing changes — told of every change of the
    /// ICCCM `WM_STATE` property of the windows it watches. A window manager
    /// sets it to IconicState when it minimizes a window (its taskbar entry, a
    /// shortcut, "show desktop") and back to NormalState when it shows it
    /// again, and deletes it when the window is withdrawn (JeTTY's own hide).
    pub(super) struct Watch {
        conn: std::sync::Arc<RustConnection>,
    }

    impl Watch {
        pub(super) fn start(win: &Window, on_change: impl Fn(winit::window::WindowId, bool) + Send + 'static) -> Option<Watch> {
            use x11rb::protocol::xproto::{AtomEnum, Property};
            use x11rb::protocol::Event;
            let xid = xid(win)?;
            let (conn, _) = x11rb::connect(None).ok()?;
            let wm_state = conn.intern_atom(false, b"WM_STATE").ok()?.reply().ok()?.atom;
            let watch = Watch { conn: std::sync::Arc::new(conn) };
            watch.select(xid);
            let conn = std::sync::Arc::clone(&watch.conn);
            std::thread::Builder::new()
                .name("jetty-minimize".into())
                .spawn(move || {
                    // Ends when the connection does (the X server is gone).
                    while let Ok(event) = conn.wait_for_event() {
                        let Event::PropertyNotify(e) = event else { continue };
                        if e.atom != wm_state {
                            continue;
                        }
                        let minimized = e.state == Property::NEW_VALUE
                            && iconic(
                                conn.get_property(false, e.window, wm_state, AtomEnum::ANY, 0, 1)
                                    .ok()
                                    .and_then(|cookie| cookie.reply().ok())
                                    .and_then(|r| r.value32().and_then(|mut v| v.next())),
                            );
                        // winit's X11 window ids are the windows' XIDs.
                        on_change(winit::window::WindowId::from(u64::from(e.window)), minimized);
                    }
                })
                .ok()?;
            Some(watch)
        }

        pub(super) fn add(&self, win: &Window) {
            if let Some(xid) = xid(win) {
                self.select(xid);
            }
        }

        /// Ask for `xid`'s property changes — on this connection only: event
        /// masks are kept per client, so winit's own selection is untouched.
        /// Sent from the UI thread while the watch thread waits for events on
        /// the same connection, which x11rb supports.
        fn select(&self, xid: u32) {
            use x11rb::protocol::xproto::ChangeWindowAttributesAux;
            let aux = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
            if let Ok(cookie) = self.conn.change_window_attributes(xid, &aux) {
                cookie.ignore_error();
            }
            let _ = self.conn.flush();
        }
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
mod work_area_tests {
    use super::{full_strut, inset_rect, strut_insets, workarea_insets, Insets};

    const ROOT: (u32, u32) = (3840, 1080);
    const LEFT: super::Rect = (0, 0, 1920, 1080);
    const RIGHT: super::Rect = (1920, 0, 1920, 1080);

    /// A panel's `_NET_WM_STRUT_PARTIAL`: `top` px along x `from..=to`.
    fn top_panel(top: u32, from: u32, to: u32) -> [u32; 12] {
        [0, 0, top, 0, 0, 0, 0, 0, from, to, 0, 0]
    }

    #[test]
    fn a_top_panel_pushes_the_strip_down_on_its_own_monitor_only() {
        // A 30 px panel along the top of the right monitor (KDE, Xfce): the
        // strip docks below it there, flush with the top on the left one.
        let struts = [top_panel(30, 1920, 3839)];
        assert_eq!(strut_insets(RIGHT, ROOT, &struts), Insets { top: 30, ..Insets::default() });
        assert_eq!(strut_insets(LEFT, ROOT, &struts), Insets::default());
        assert_eq!(inset_rect(RIGHT, strut_insets(RIGHT, ROOT, &struts)), (1920, 30, 1920, 1050));
    }

    #[test]
    fn bottom_and_side_struts_count_from_the_root_edges() {
        // A 44 px bottom panel on the left monitor and a 60 px dock on the
        // right edge of the right one.
        let struts = [
            [0, 0, 0, 44, 0, 0, 0, 0, 0, 0, 0, 1919],
            [0, 60, 0, 0, 0, 0, 0, 1079, 0, 0, 0, 0],
        ];
        assert_eq!(strut_insets(LEFT, ROOT, &struts), Insets { bottom: 44, ..Insets::default() });
        assert_eq!(strut_insets(RIGHT, ROOT, &struts), Insets { right: 60, ..Insets::default() });
        assert_eq!(inset_rect(LEFT, Insets { bottom: 44, ..Insets::default() }), (0, 0, 1920, 1036));
    }

    #[test]
    fn a_plain_strut_runs_along_the_whole_edge() {
        // `_NET_WM_STRUT` (no ranges) reserves its edge on every monitor that
        // touches it.
        let s = full_strut([0, 0, 25, 0], ROOT);
        assert_eq!(strut_insets(LEFT, ROOT, &[s]).top, 25);
        assert_eq!(strut_insets(RIGHT, ROOT, &[s]).top, 25);
    }

    #[test]
    fn a_strut_that_cannot_be_meant_for_a_monitor_is_ignored() {
        // Monitors stacked: a panel on the top edge of the LOWER one can only be
        // written as a strut covering the whole upper monitor — no monitor may
        // be swallowed by one, so it counts on neither.
        let root = (1920, 2160);
        let upper = (0, 0, 1920, 1080);
        let lower = (0, 1080, 1920, 1080);
        let struts = [top_panel(1110, 0, 1919)];
        assert_eq!(strut_insets(upper, root, &struts), Insets::default());
        assert_eq!(strut_insets(lower, root, &struts).top, 30);
        // The widest of several panels on one edge wins.
        let two = [top_panel(24, 0, 1919), top_panel(36, 0, 999)];
        assert_eq!(strut_insets(upper, root, &two).top, 36);
    }

    #[test]
    fn the_work_area_of_a_bar_the_compositor_draws() {
        // No client reserves space (a desktop that draws its own top bar):
        // the window manager's work area says where windows go.
        let area = (0, 32, 3840, 1048);
        assert_eq!(workarea_insets(LEFT, area), Insets { top: 32, ..Insets::default() });
        assert_eq!(workarea_insets(RIGHT, area), Insets { top: 32, ..Insets::default() });
        // A work area that misses the monitor says nothing about it.
        assert_eq!(workarea_insets(RIGHT, (0, 0, 1920, 1080)), Insets::default());
        // A whole-desktop work area takes nothing.
        assert_eq!(workarea_insets(LEFT, (0, 0, 3840, 1080)), Insets::default());
    }

    #[test]
    fn an_inset_never_leaves_less_than_a_pixel() {
        let all = Insets { top: 2000, bottom: 2000, left: 4000, right: 4000 };
        assert_eq!(inset_rect(LEFT, all).2, 1);
        assert_eq!(inset_rect(LEFT, all).3, 1);
        assert_eq!(inset_rect(LEFT, Insets::default()), LEFT);
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod minimize_tests {
    use super::x11::iconic;

    #[test]
    fn only_the_iconic_state_is_a_minimize() {
        // ICCCM WM_STATE: what a window manager writes when it minimizes a
        // window (3), shows it (1), or lets go of it (0, or deletes it — JeTTY's
        // own hide). Only the first is a minimize: a window JeTTY hid stops
        // painting through `visible`, not through this.
        assert!(iconic(Some(3)));
        assert!(!iconic(Some(1)));
        assert!(!iconic(Some(0)));
        assert!(!iconic(None));
        // A state outside ICCCM's three is not a minimize either.
        assert!(!iconic(Some(2)));
    }
}

#[cfg(test)]
mod app_id_tests {
    use super::APP_ID;

    #[test]
    fn the_app_id_is_the_desktop_entry_every_package_installs() {
        // A Wayland compositor looks the window up as `<app id>.desktop`; an X11
        // taskbar matches the entry's StartupWMClass against WM_CLASS.
        let entry = include_str!("../../../assets/jetty.desktop");
        assert!(entry.lines().any(|l| l == format!("StartupWMClass={APP_ID}")), "{entry}");
        let file = format!("assets/{APP_ID}.desktop");
        // The .deb, the AppImage and install.sh all install that file.
        assert!(include_str!("../../../Cargo.toml").contains(&format!("[\"{file}\"")), "deb assets");
        assert!(include_str!("../../../.github/workflows/release.yml").contains(&format!("--desktop-file {file}")), "AppImage");
        assert!(include_str!("../../../install.sh").contains(&format!("applications/{APP_ID}.desktop")), "install.sh");
    }
}

#[cfg(test)]
mod hide_tests {
    use super::{hide_kind_for, wayland_token, HideKind};
    use raw_window_handle::{
        RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, XcbWindowHandle,
        XlibDisplayHandle, XlibWindowHandle,
    };
    use std::num::NonZeroU32;
    use std::ptr::NonNull;

    #[test]
    fn wayland_windows_hide_by_closing_everything_else_unmaps() {
        // winit's set_visible is a no-op on Wayland, and the minimize that
        // replaced it is ignored by tiling compositors and can't be undone by a
        // summon (the compositor refuses to raise an existing window without a
        // token). There the hide closes the window.
        let wl = RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::dangling()));
        assert_eq!(hide_kind_for(Some(&wl)), HideKind::Close);
        let xlib = RawWindowHandle::Xlib(XlibWindowHandle::new(7));
        let xcb = RawWindowHandle::Xcb(XcbWindowHandle::new(NonZeroU32::new(7).unwrap()));
        assert_eq!(hide_kind_for(Some(&xlib)), HideKind::Unmap);
        assert_eq!(hide_kind_for(Some(&xcb)), HideKind::Unmap);
        // No handle (macOS before creation, an error): today's unmap.
        assert_eq!(hide_kind_for(None), HideKind::Unmap);
    }

    #[test]
    fn an_activation_token_goes_to_wayland_windows_only() {
        let wl = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull::dangling()));
        let x11 = RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0));
        let tok = || Some("kwin-123".to_string());
        assert_eq!(wayland_token(Some(&wl), tok()), tok());
        // X11 has its own launch protocol (DESKTOP_STARTUP_ID): left alone.
        assert_eq!(wayland_token(Some(&x11), tok()), None);
        assert_eq!(wayland_token(None, tok()), None);
        // An empty variable is no token.
        assert_eq!(wayland_token(Some(&wl), Some(String::new())), None);
        assert_eq!(wayland_token(Some(&wl), None), None);
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
