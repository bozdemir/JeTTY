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
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_window_icon(app_icon())
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_resizable(true)
        .with_min_inner_size(LogicalSize::new(200.0_f64, 120.0_f64))
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
        .with_min_inner_size(LogicalSize::new(200u32, 200u32))
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
/// 1. `current_monitor()` — the platform's own answer, when it has one.
/// 2. the monitor CONTAINING the window's last outer position. A HIDDEN window
///    (the dropdown between summons) reports no `current_monitor` on X11, so
///    without this fallback it would be treated as being on the PRIMARY monitor
///    and re-appear on the wrong screen for multi-monitor users.
/// 3. the first available monitor (also the Wayland path, where `outer_position`
///    is `Err` — accepted degradation, same as the F9 hotkey).
///
/// Shared by `center_window`, `dock_window_top` and the fullscreen path so all
/// three agree on "which screen". Lives in `jetty-platform` (not `jetty-app`)
/// because the crate dependency only points one way: `jetty-app` →
/// `jetty-platform`, never the reverse.
pub fn monitor_for_window(win: &Window) -> Option<MonitorHandle> {
    win.current_monitor()
        .or_else(|| {
            win.outer_position().ok().and_then(|a| {
                win.available_monitors().find(|m| {
                    let p = m.position();
                    let s = m.size();
                    pos_in_monitor_rect((a.x, a.y), (p.x, p.y), (s.width, s.height))
                })
            })
        })
        .or_else(|| win.available_monitors().next())
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

#[cfg(test)]
mod tests {
    use super::pos_in_monitor_rect;

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
