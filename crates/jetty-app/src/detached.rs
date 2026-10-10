//! Pure tab-transfer + eligibility logic for tab detach/reattach, plus the
//! `DetachedWindow` struct that wraps a single tab's render stack.
//!
//! The pure helpers (no GPU/winit) are at the top so they can be unit-tested
//! without an event loop. `DetachedWindow` and its constructor follow.

/// A tab may be detached only when the main window keeps at least one tab.
pub fn can_detach(main_tab_count: usize) -> bool {
    main_tab_count >= 2
}

/// Remove and return the element at `idx`, or `None` if out of range.
/// Generic so this module never needs visibility into `Tab`'s fields.
pub fn take_tab<T>(v: &mut Vec<T>, idx: usize) -> Option<T> {
    if idx < v.len() {
        Some(v.remove(idx))
    } else {
        None
    }
}

/// Active index after a reattached tab is appended to a vec whose length is now
/// `tabs_len_after_push`.
pub fn reattach_index(tabs_len_after_push: usize) -> usize {
    tabs_len_after_push.saturating_sub(1)
}

/// Cols/rows for a detached terminal window with chrome: the grid fills the
/// client area minus the top bar (`bars.0`, its scaled bar height) and the
/// bottom status strip (`bars.1`, 0 when the perf HUD is off), inside the
/// physical inner padding `pad` (x, y) and the scrollbar `gutter` on the
/// right (`jetty_render::grid_dims`). `width_px`/`height_px` are physical
/// pixels; `cell` the glyph cell size. Mirrors the main window's
/// `main_grid_dims_at`. The 80×24 fallback (app.rs FALLBACK_COLS/ROWS) before
/// cell metrics exist.
pub(crate) fn grid_dims(
    width_px: f32,
    height_px: f32,
    cell: (f32, f32),
    gutter: f32,
    bars: (f32, f32),
    pad: (f32, f32),
) -> (usize, usize) {
    let band_h = height_px - bars.0 - bars.1;
    jetty_render::grid_dims(width_px, band_h, cell.0, cell.1, gutter, pad.0, pad.1)
}

/// Vertical distance (px) the cursor must travel OUT of the tab-bar strip
/// (while the button is held) before a tab drag enters the "tearing" state.
pub const TEAR_THRESHOLD_PX: f32 = 24.0;

/// True when a held tab drag at `cursor_y` has moved more than `threshold` px
/// vertically OUT of the tab-bar strip (`bar_y .. bar_y + bar_h`) — the
/// "tearing" state. Returning INTO the strip (or its threshold margin) before
/// release cancels tearing, so a plain click still just selects the tab.
pub fn tearing(cursor_y: f32, bar_y: f32, bar_h: f32, threshold: f32) -> bool {
    cursor_y < bar_y - threshold || cursor_y > bar_y + bar_h + threshold
}

/// The slot a tab dragged ALONG the strip (not tearing) points at: the index
/// of the drawn tab rect spanning `cursor_x` — `None` before the first tab or
/// past the last (over the "+"). A rect parked offscreen for an overflowed
/// tab never matches.
pub fn reorder_target(cursor_x: f32, tab_rects: &[jetty_render::Rect]) -> Option<usize> {
    tab_rects.iter().position(|r| r.w > 0.0 && cursor_x >= r.x && cursor_x < r.x + r.w)
}

/// Move the element at `from` to index `to`, the ones between shifting over by
/// one. `false` (and nothing moved) when either index is out of range.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= v.len() || to >= v.len() {
        return false;
    }
    let item = v.remove(from);
    v.insert(to, item);
    true
}

/// Where index `i` ends up after [`move_item`]`(from, to)`.
pub fn index_after_move(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

/// True when the GLOBAL cursor `(gx, gy)` lands inside the MAIN window's
/// tab-bar strip: the band `tabbar_h` tall at the top of the main window, or —
/// when `tab_bar_bottom` — just above the status strip at the bottom (the same
/// band `App::tabbar_y` computes). `main_x/main_y` is the main window's outer
/// position; `main_w/main_h` its surface size.
///
/// All coordinates must be in ONE consistent unit space. The caller passes
/// the space the whole desktop shares ([`desktop_unit_scale`]) so a drop from a
/// detached window on a DIFFERENT-DPI monitor is tested against the main
/// window's band in the same units (mixing per-window scales made the band
/// test miss — F9).
#[allow(clippy::too_many_arguments)]
pub fn main_tabbar_contains(
    gx: f64,
    gy: f64,
    main_x: f64,
    main_y: f64,
    main_w: f64,
    main_h: f64,
    tabbar_h: f64,
    status_h: f64,
    tab_bar_bottom: bool,
) -> bool {
    let lx = gx - main_x;
    let ly = gy - main_y;
    if lx < 0.0 || lx > main_w {
        return false;
    }
    let bar_y = if tab_bar_bottom {
        (main_h - tabbar_h - status_h).max(0.0)
    } else {
        0.0
    };
    ly >= bar_y && ly < bar_y + tabbar_h
}

/// The scale that maps a window's or monitor's physical px into the ONE unit
/// space every desktop coordinate shares: macOS lays its displays out in
/// points (logical), so there it is that window's / monitor's own scale; X11's
/// root window and Windows' virtual screen are laid out in pixels, where a
/// monitor's own "logical" rect is not part of any shared space at mixed DPI —
/// 1 there. Only the OS decides this, never the desktop environment.
pub fn desktop_unit_scale(scale: f64, macos: bool) -> f64 {
    if macos && scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Where a torn-off tab's window goes: its top-left at the `drop` point, kept
/// on the monitor that contains it (else the nearest) — `win` its size and
/// `monitors` their `(x, y, w, h)` rects, all in the desktop's one unit space
/// ([`desktop_unit_scale`]). With no monitor known, the drop point itself.
pub fn drop_position(drop: (f64, f64), win: (f64, f64), monitors: &[(f64, f64, f64, f64)]) -> (f64, f64) {
    let (dx, dy) = drop;
    let contains = |m: &&(f64, f64, f64, f64)| dx >= m.0 && dx < m.0 + m.2 && dy >= m.1 && dy < m.1 + m.3;
    let dist = |m: &(f64, f64, f64, f64)| (dx - (m.0 + m.2 / 2.0)).powi(2) + (dy - (m.1 + m.3 / 2.0)).powi(2);
    let target = monitors.iter().find(contains).or_else(|| monitors.iter().min_by(|a, b| dist(a).total_cmp(&dist(b))));
    let Some(&(mx, my, mw, mh)) = target else { return drop };
    // Sub-pixel placement is irrelevant: whole units through `clamp_pos`.
    let (x, y) = clamp_pos(
        dx.round() as i32,
        dy.round() as i32,
        win.0.round() as u32,
        win.1.round() as u32,
        (mx.round() as i32, my.round() as i32, mw.round() as u32, mh.round() as u32),
    );
    (x as f64, y as f64)
}

/// Clamp a window top-left `(x, y)` so a `win_w`×`win_h` window stays inside
/// the monitor rect `(mon_x, mon_y, mon_w, mon_h)`. If the window is larger
/// than the monitor it pins to the monitor origin.
pub fn clamp_pos(
    x: i32,
    y: i32,
    win_w: u32,
    win_h: u32,
    (mon_x, mon_y, mon_w, mon_h): (i32, i32, u32, u32),
) -> (i32, i32) {
    let max_x = mon_x + mon_w as i32 - win_w as i32;
    let max_y = mon_y + mon_h as i32 - win_h as i32;
    (x.min(max_x).max(mon_x), y.min(max_y).max(mon_y))
}

/// Items of the MAIN window's tab context menu (right-click on a tab).
/// "Detach" is present only while detaching is allowed (≥ 2 tabs).
pub fn tab_menu_items(can_detach: bool) -> Vec<&'static str> {
    if can_detach {
        vec!["Detach", "Rename", TAB_MENU_COLOR, "Close Tab"]
    } else {
        vec!["Rename", TAB_MENU_COLOR, "Close Tab"]
    }
}

/// The tab menu row that opens the per-tab color list in place.
pub const TAB_MENU_COLOR: &str = "Color ▸";
/// The color list's "remove the color" row.
pub const TAB_MENU_NO_COLOR: &str = "No Color";

/// The tab menu's color list: "No Color", then palette colors 1–6.
pub fn tab_color_menu_items() -> Vec<&'static str> {
    std::iter::once(TAB_MENU_NO_COLOR).chain(jetty_render::TAB_COLORS.iter().map(|(_, n)| *n)).collect()
}

/// What a color-list row sets: `Some(None)` = "No Color", `Some(Some(i))` =
/// palette color `i`, `None` = not a color row.
pub fn tab_color_from_label(label: &str) -> Option<Option<u8>> {
    if label == TAB_MENU_NO_COLOR {
        return Some(None);
    }
    jetty_render::TAB_COLORS.iter().find(|(_, n)| *n == label).map(|(i, _)| Some(*i))
}

/// Whether an open tab menu's rows are the "Color ▸" list (Left goes back
/// from it to the tab menu proper) rather than the tab menu itself.
pub fn is_tab_color_list(labels: &[&str]) -> bool {
    !labels.is_empty() && labels.iter().all(|l| tab_color_from_label(l).is_some())
}

/// Swatches for the color list's rows: a small rounded square of each color
/// at the right end of its row (where a shortcut hint would sit), the tab's
/// `current` color ringed. Rows that are not colors get none.
pub fn tab_color_swatches(
    item_rects: &[jetty_render::Rect],
    labels: &[&str],
    theme: &jetty_core::Theme,
    current: Option<u8>,
    cm: jetty_render::ChromeMetrics,
) -> Vec<jetty_render::Rect> {
    let ui = jetty_render::UiPalette::cached(theme);
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let d = cm.px(12.0).round();
    let mut out = Vec::new();
    for (r, &label) in item_rects.iter().zip(labels) {
        let Some(choice) = tab_color_from_label(label) else { continue };
        let x = r.x + r.w - cm.px(14.0) - d;
        let y = r.y + (r.h - d) * 0.5;
        if choice == current {
            let ring = cm.px(2.0).round().max(1.0);
            out.push(jetty_render::Rect::rounded(
                x - ring,
                y - ring,
                d + ring * 2.0,
                d + ring * 2.0,
                rgba(ui.text),
                cm.px(4.0) + ring,
            ));
        }
        let fill = match choice.and_then(|i| jetty_render::tab_color_rgb(theme, i)) {
            Some(c) => c,
            // "No Color": the bar's own background, outlined by the hint shade.
            None => {
                out.push(jetty_render::Rect::rounded(x, y, d, d, rgba(ui.text_hint), cm.px(4.0)));
                let inset = cm.px(1.5);
                out.push(jetty_render::Rect::rounded(
                    x + inset,
                    y + inset,
                    d - inset * 2.0,
                    d - inset * 2.0,
                    rgba(ui.bg),
                    cm.px(3.0),
                ));
                continue;
            }
        };
        out.push(jetty_render::Rect::rounded(x, y, d, d, rgba(fill), cm.px(4.0)));
    }
    out
}

/// Items of a DETACHED window's context menu (right-click anywhere).
/// "Run in New Tab" runs THIS window's selection in a new MAIN-window tab at
/// this tab's cwd (the main window is the only tabbed one; it is not summoned).
/// "Select All" and "Clear" are the main menu's rows (Select All had no other
/// way in on Linux: no default chord, no palette row).
pub const DETACHED_MENU_ITEMS: [&str; 6] = ["Reattach", "Copy", "Paste", "Run in New Tab", "Select All", "Clear"];

/// Per-corner radii (tl, tr, bl, br) for a detached window's corner mask.
/// A detached window is a free-floating window — it is never docked top-flush
/// like the main window's Dropdown mode — so ALL FOUR corners round with the
/// same configured radius (the main window's top-square nuance never applies).
pub fn corner_radii(radius_px: f32) -> (f32, f32, f32, f32) {
    (radius_px, radius_px, radius_px, radius_px)
}

/// The remappable action a tab / detached-window menu label triggers (`None`
/// for labels with no keyboard binding, e.g. "Rename").
pub fn menu_action(label: &str) -> Option<crate::keymap::BindableAction> {
    use crate::keymap::BindableAction as A;
    Some(match label {
        "Detach" | "Reattach" => A::DetachTab,
        "Copy" => A::Copy,
        "Paste" => A::Paste,
        "Run in New Tab" => A::RunSelection,
        "Select All" => A::SelectAll,
        "Close Tab" => A::CloseTab,
        _ => return None,
    })
}

/// The six shortcut hints of the main right-click menu (`jetty_render::
/// MENU_ITEMS` order: Copy, Paste, Run in New Tab, Select All, Clear, Close
/// Tab) from the LIVE keymap. "Clear" is the raw Ctrl+L byte, not a remappable
/// action, so its hint is fixed.
pub fn context_menu_hints(km: &crate::keymap::KeyMap) -> [String; 6] {
    use crate::keymap::BindableAction as A;
    [
        km.menu_hint(A::Copy),
        km.menu_hint(A::Paste),
        km.menu_hint(A::RunSelection),
        km.menu_hint(A::SelectAll),
        "⌃L".to_string(),
        km.menu_hint(A::CloseTab),
    ]
}

/// Right-aligned keyboard-shortcut hint for a menu label, from the LIVE keymap
/// (a `[keys]` remap — or macOS's ⌘ chords — shows in the menu); blank when
/// the action has no binding. "Clear" is the raw Ctrl+L byte (as in
/// [`context_menu_hints`]).
pub fn menu_hint(km: &crate::keymap::KeyMap, label: &str) -> String {
    if label == "Clear" {
        return "⌃L".to_string();
    }
    menu_action(label).map(|a| km.menu_hint(a)).unwrap_or_default()
}

/// The OS title (taskbar, Alt+Tab) of a JeTTY window showing `tab_title` —
/// the main window's active tab and a detached window's tab alike.
pub(crate) fn os_window_title(tab_title: &str) -> String {
    format!("{tab_title} — JeTTY")
}

/// True if `last_focused` is one of the live detached-window ids, i.e. focus
/// moved from the main window into one of the app's OWN detached windows. The
/// main window's Yakuake-style auto-hide must be suppressed in that case (the
/// user has not left Jetty), exactly as it already is for the Settings window.
/// Generic over the id type so it is unit-testable without real `WindowId`s.
pub fn focus_in_detached<I: PartialEq>(last_focused: Option<I>, detached_ids: &[I]) -> bool {
    match last_focused {
        Some(id) => detached_ids.contains(&id),
        None => false,
    }
}

// ── DetachedWindow ────────────────────────────────────────────────────────────

use std::sync::Arc;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;
use jetty_render::{GpuContext, QuadLayer, TextLayer};

use crate::app::Tab;

/// A detached terminal window: owns one `Tab` plus its own wgpu render stack
/// (window, GPU context, text/quad layers, offscreen texture). Mirrors the
/// per-window resources that the main `App` holds for the main window.
///
/// A detached window always contains exactly one tab; its chrome is a slim top
/// bar (title + help "?" + close ✕, draggable to move) and — when the perf HUD
/// is on — the same bottom status strip as the main window. It has its own
/// overlays (search bar, help, command palette, hint mode, copy-mode).
pub(crate) struct DetachedWindow {
    pub window: Arc<Window>,
    pub gpu: GpuContext,
    /// Terminal-font TextLayer for the tab's grid content.
    pub text: TextLayer,
    /// UI-font TextLayer for window chrome (title, close ✕, status bar, menu).
    pub chrome_text: TextLayer,
    pub quad: QuadLayer,
    /// Surface-sized offscreen render target (same descriptor as
    /// `App::make_offscreen`). When CRT is enabled the whole detached scene is
    /// rendered into it and the CRT post-pass samples it onto the surface —
    /// the same routing as the main window. Allocated LAZILY by
    /// `App::render_detached_window` on the first CRT frame and re-allocated on
    /// size change (mirrors the main window); `None` while CRT has never been on,
    /// so a plain detached window holds no full-surface texture.
    pub offscreen: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// Per-window rounded-corner mask pass (same radius as the main window's;
    /// all four corners round — see `corner_radii`). Per-window instance because
    /// `CornerMask` caches its uniform/bind group; sharing across surfaces of
    /// different sizes would thrash it.
    pub corner_mask: jetty_render::CornerMask,
    /// Per-window focus-ring pass (`window_border`), built on this window's
    /// device the first time it draws a ring — `None` while the key is "none".
    pub focus_ring: Option<jetty_render::FocusRing>,
    /// Per-window CRT post-pass. Per-window instance because `Crt` caches its
    /// bind groups keyed by the sampled src view — sharing the main window's
    /// instance would thrash the cache between windows. Built LAZILY (with only
    /// the pipeline variant the settings need) by `App::sync_detached_post` the
    /// first time CRT or a glitch trigger is on; `None` until then.
    pub crt: Option<jetty_render::Crt>,
    /// The variant `crt` was last prepared for (see `App::crt_key`).
    pub crt_key: Option<jetty_render::CrtKey>,
    /// This window's event glitch (a failed command in its tab).
    pub glitch: crate::effects::Glitch,
    /// When its last PACED effect-animation frame was requested (see
    /// `App::anim_requested_at`).
    pub anim_requested_at: Option<std::time::Instant>,
    /// Per-window inline-image (sixel) layer on THIS window's device. Device-
    /// scoped GPU resources cannot be shared with the main window's layer
    /// (amendment R1), so each detached window owns one — same decoded RGBA from
    /// jetty-core, uploaded to this device on demand.
    pub image_layer: jetty_render::ImageLayer,
    /// This window's backdrop layer (visuals v2): `None` while `[backdrop]` is
    /// "none" (nothing built), created on the first frame that draws it. The
    /// settings and the image texture are the App's, shared on this device.
    pub backdrop: Option<jetty_render::Backdrop>,
    /// Caret flash burst clock for keystrokes typed in THIS window (mirrors
    /// `App::caret_anim` for the main window). `None` = no burst live.
    pub caret_anim: Option<std::time::Instant>,
    /// Fallback paint deadline for THIS window's latest PTY-bound keystroke
    /// (mirrors `App::key_paint_due`): the echo paints, not the key.
    pub key_paint_due: Option<std::time::Instant>,
    /// THIS window's overlays (search bar, help, command palette, hint mode,
    /// copy-mode) — the main window's twin is `App::ov`.
    pub ov: crate::overlays::Overlays,
    /// The IME's in-progress composition in this window, drawn at the cursor
    /// until it commits (mirrors `App::ime_preedit`).
    pub ime_preedit: Option<String>,
    /// Last IME candidate-window anchor handed to winit for this window
    /// (mirrors `App::ime_area`): re-sent only when the cursor cell moves.
    pub ime_area: Option<(i32, i32, u32, u32)>,
    /// Backoff after this window's `acquire_frame()` failed (mirrors
    /// `App::acquire_retry`); `None` while frames present normally.
    pub acquire_retry: Option<crate::app::AcquireRetry>,
    /// Perf HUD state for THIS window (mirrors `App::perf_ms`, `last_frame_at`,
    /// `perf_idle_at`, `perf_idle_shown`): its strip shows its own smoothed frame
    /// time and flips to "idle" by its own one-shot repaint.
    pub perf_ms: f32,
    pub last_frame_at: Option<std::time::Instant>,
    pub perf_idle_at: Option<std::time::Instant>,
    pub perf_idle_shown: bool,
    /// Flood frame pacing for THIS window (mirrors `App::last_present_at` /
    /// `App::paced_paint_at`; see `pace_paint`).
    pub last_present_at: Option<std::time::Instant>,
    pub paced_paint_at: Option<std::time::Instant>,
    /// This window's display refresh interval for that pacing — its OWN
    /// monitor's (`App::frame_interval` is the main window's, which may sit on
    /// another): `None` until [`Self::frame_interval`] reads it, and again
    /// after the window moved.
    pub frame_interval: Option<std::time::Duration>,
    /// Whether THIS detached window is in OS fullscreen (F11 pressed in it).
    /// Session-only and PER WINDOW — detached windows persist no geometry at all,
    /// have no `window_mode`, and are never hidden, so this is purely a live
    /// shape mirror (the render path must never call the syscall-backed
    /// `Window::fullscreen()`). Mirrors `App::main_fullscreen`; feeds this
    /// window's corner-radius suppression (`effective_corner_radius_px`) and its
    /// maximize / drag / resize-edge inertness.
    ///
    /// MUST be false before the struct is dropped — on macOS
    /// `set_simple_fullscreen(true)` overwrites app-scoped
    /// `NSApplication.presentationOptions` that only its own `false` call
    /// restores, so dropping a fullscreen window leaks an auto-hidden Dock and
    /// menu bar for the rest of the session. See `App::exit_detached_fullscreen_bare`.
    pub fullscreen: bool,
    /// Whether THIS detached window holds OS focus (from its Focused events).
    /// Drives the unfocused-hollow cursor, per-window like the main one.
    pub focused: bool,
    /// Whether JeTTY asked for attention on THIS window since it last had
    /// focus — cleared on its next Focused(true), and only then (see
    /// `App::main_attention`).
    pub attention: bool,
    /// When `Some`, a grid+PTY reflow is scheduled for this instant (mirrors
    /// `App::reflow_pending_at`): a border drag fires many Resized events, and
    /// reflow+SIGWINCH per event scatters p10k's prompt — the surface resizes
    /// live, ONE reflow fires ~250ms after the drag settles (`about_to_wait`).
    pub reflow_pending_at: Option<std::time::Instant>,
    /// The single terminal session owned by this detached window.
    pub tab: Tab,
    /// The tab title the OS title was last set from, so `sync_os_title` is a
    /// no-op string compare unless the tab's title really changed.
    pub applied_os_title: String,
    /// Last known cursor position inside THIS window (physical px).
    pub cursor: (f64, f64),
    /// Whether the pointer is over THIS window (`cursor` is its last position
    /// there): false from a CursorLeft until it moves over the window again —
    /// nothing it hovered (the "✕", a Ctrl+hover link) stays lit meanwhile.
    pub pointer_in: bool,
    /// Manual top-bar drag: `Some(local cursor at press)` while the bar is held.
    /// Each CursorMoved computes `global_cursor = outer_position + local` and
    /// moves the window to `global_cursor - offset`, so the RELEASE event is
    /// ours (needed for drop-to-reattach). `None` when not dragging (including
    /// the Wayland `drag_window()` fallback, where the compositor owns the drag).
    pub bar_drag: Option<(f64, f64)>,
    /// GLOBAL cursor position (physical px) at the top-bar press that armed
    /// `bar_drag`. Drop-to-reattach requires the release to have moved more than
    /// a few px from here — a sub-threshold press/release is a plain click (raise
    /// the window), NOT a tear-out. Without this, a plain click on a detached
    /// title bar that overlaps the main window's tab-bar band would reattach the
    /// tab. `None` when no bar drag is in progress.
    pub bar_drag_start: Option<(f64, f64)>,
    /// Cached resize-edge zone so `set_cursor` fires only on zone changes
    /// (mirrors `App::resize_cursor` for the borderless main window).
    pub resize_zone: crate::app::ResizeZone,
    /// When `Some`, this window's context menu (Reattach / Copy / Paste) is open
    /// at this physical-pixel anchor.
    pub menu_open: Option<(f32, f32)>,
    /// Cached hit-test rects for the open context menu (built once on open).
    pub menu_rects: Vec<jetty_render::Rect>,
    /// Disabled menu indices, computed once at open (Copy=1 and Run in New
    /// Tab=3 dim without a selection — same rule as the main menu's cache).
    pub menu_disabled: Vec<usize>,
    /// Highlighted menu item — the pointer's hover or the arrow keys'.
    pub menu_hover: Option<usize>,
    /// The menu row the pointer was over at its last move (`None`: off the
    /// rows), reset at every open: the pointer moves the highlight only when
    /// it crosses rows (`menunav::pointer_hover`).
    pub menu_pointer_row: Option<usize>,
    /// The bar control under the pointer — the close "✕" (its red hover) or
    /// the help "?" (an accent tint, as in the main bar).
    pub ctrl_hover: jetty_render::CtrlHover,
    /// Time + position of the last left press on the top bar, for the
    /// double-click → maximize toggle (mirrors `App::last_strip_click`).
    pub last_bar_click: Option<(std::time::Instant, f32, f32)>,
    /// Whether this detached window is occluded/minimized
    /// (`WindowEvent::Occluded(true)` or WM iconify). Every self-driven redraw
    /// (CRT/caret animation, PTY-output redraw) gates on `!occluded` so a hidden
    /// detached window returns to true idle instead of rendering forever (F8/F17).
    pub occluded: bool,
    /// Per-window fractional wheel-scroll accumulator. Separate from the main
    /// window's `App::scroll_accum` so a leftover fraction in one window never
    /// bleeds into another's scroll (F26).
    pub scroll_accum: crate::input::ScrollAccumulator,
    /// Whether a local text selection drag is in progress in this window (F37).
    /// Mirrors `App::selecting` for the main window.
    pub selecting: bool,
    /// This window's grid mouse state (buttons held by the program, click
    /// counting, edge auto-scroll) — the same `gridmouse` logic as the main
    /// window's `App::grid_mouse`.
    pub grid_mouse: crate::gridmouse::GridMouse,
    /// Whether the scrollbar thumb is being dragged in THIS window. Mirrors
    /// `App::dragging_scrollbar` for the main window.
    pub dragging_scrollbar: bool,
    /// Thumb-local y grab offset captured at drag start, so the thumb never
    /// jumps under the pointer. Mirrors `App::drag_grab_dy`.
    pub drag_grab_dy: f32,
    /// Whether the pointer is over THIS window's scrollbar gutter — shows the
    /// thumb under `scrollbar = "auto"`. Mirrors `App::scrollbar_hover`.
    pub scrollbar_hover: bool,
    /// The link under the pointer while the link modifier is held in THIS
    /// window (underlined; opened on click). Mirrors `App::link_hover`.
    pub link_hover: Option<jetty_core::LinkHit>,
    /// This window's caret glow pass (`App::caret_fx`'s twin on this window's
    /// device): `None` until the glow is enabled and this window paints.
    pub caret_fx: Option<jetty_render::CaretFx>,
    /// This window's cursor trail (`App::trail` and friends, per window).
    pub trail: jetty_render::TrailModel,
    pub trail_layer: Option<jetty_render::CursorTrailLayer>,
    pub trail_wake: Option<std::time::Instant>,
    /// When this window's tab last drained a flood (see `App::flood_at`).
    pub flood_at: Option<std::time::Instant>,
    /// This window's visual bell / command pulse (`App::bell_anim` and
    /// friends, per window; the rims are drawn by `focus_ring`).
    pub bell_anim: Option<(std::time::Instant, crate::motion::VisualBell)>,
    pub bell_limit: crate::motion::RateLimit,
    pub pulse_anim: Option<(std::time::Instant, crate::motion::PulseKind)>,
    /// The hovered 0-based grid cell the cache above was computed for.
    /// Mirrors `App::link_hover_cell`.
    pub link_hover_cell: Option<(usize, usize)>,
    /// This window's size across scale-factor changes (mirrors
    /// `App::main_dpi_size`; see `jetty_platform::dpi_change_size`).
    pub dpi_size: Option<jetty_platform::DpiSize>,
}

impl DetachedWindow {
    /// Construct a detached window sized `w_logical × h_logical` (logical /
    /// device-independent pixels) that owns `tab`. Mirrors the construction in
    /// `App::toggle_settings_window` and `App::resumed` — same `TextLayer` /
    /// `QuadLayer` descriptors — but on the main window's GPU: `gpu_shared` (the
    /// main `GpuContext::shared()`) means only a new surface is created, and
    /// `font_source` (the main grid `TextLayer`) lends its loaded font database,
    /// so a detach no longer blocks the UI thread on a new device plus two
    /// fontconfig scans. Either may be `None` (main GPU unavailable) → the window
    /// acquires its own, as before.
    ///
    /// `font_logical` and `ui_font_logical` are the caller's current logical font
    /// sizes (same values stored in `App::font_logical` and `App::ui_font_logical`).
    /// `font_family` is the terminal font family (same as `App::font_family`);
    /// `ui_font_family` the chrome family (`""` = platform sans, same as
    /// `App::ui_font_family`) and `chrome_family` the chrome's mono family at that
    /// default (`App::chrome_font_family`). Both sizes are scaled by the new
    /// window's `scale_factor` before being passed to `TextLayer`, matching
    /// `App::resumed`.
    ///
    /// Returns `Err(tab)` — handing the `Tab` back intact — when the OS window
    /// or the GPU context cannot be created at runtime (both fail for real,
    /// non-fatal reasons: X resource exhaustion, no adapter that can present
    /// this surface, a `request_device` hiccup). The caller re-inserts the tab
    /// and aborts the detach instead of the whole app aborting and SIGKILLing
    /// every shell.
    // The Err payload is the whole `Tab`, deliberately handed back for
    // re-insertion; this init runs once per detach, never on a hot path.
    #[allow(clippy::result_large_err)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        tab: Tab,
        w_logical: u32,
        h_logical: u32,
        font_logical: f32,
        ui_font_logical: f32,
        font_family: &str,
        ui_font_family: &str,
        chrome_family: &str,
        gpu_shared: Option<&Arc<jetty_render::GpuShared>>,
        font_source: Option<&TextLayer>,
    ) -> Result<Self, Tab> {
        // Title the OS window from the tab (mirrors how the tab bar displays it).
        let window = match jetty_platform::build_window(
            event_loop,
            &os_window_title(&tab.title),
            (w_logical, h_logical),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("jetty: failed to create detached window: {e}");
                return Err(tab);
            }
        };
        // Allow IME so CJK/dead-key commits reach this window's PTY too
        // (mirrors the main terminal window; commits are handled in
        // `handle_detached_event`'s `Ime::Commit` arm).
        window.set_ime_allowed(true);
        let size = window.inner_size();
        // HiDPI: same scale-factor handling as the main window in `resumed`.
        let scale = window.scale_factor() as f32;

        // GPU context — a surface on the shared main device (same call as
        // `toggle_settings_window`), else a device of its own. Failure hands the tab
        // back (the main + settings windows handle the same None gracefully).
        let gpu = match GpuContext::new_sharing(gpu_shared, window.clone(), size.width, size.height) {
            Some(g) => g,
            None => {
                eprintln!("jetty: detached window GPU init failed — no suitable adapter");
                return Err(tab);
            }
        };

        // Both layers build from the main layer's already-loaded font database
        // (a copy of the face index, not a fresh fontconfig scan).
        let fonts = || font_source.map_or_else(TextLayer::build_font_system, |t| t.clone_font_system());
        // Terminal content layer — mirrors the grid TextLayer built in
        // `App::resumed`: terminal font at logical × scale_factor.
        let text = TextLayer::for_gpu(&gpu, font_logical * scale, font_family, fonts());
        // Chrome layer — mirrors the chrome TextLayer built in `App::resumed`:
        // UI font at ui_font_logical × scale_factor, with the chrome family
        // applied via `set_ui_family` (no fontconfig rescan).
        let mut chrome_text = TextLayer::for_gpu(&gpu, ui_font_logical * scale, chrome_family, fonts());
        chrome_text.set_ui_family(if ui_font_family.is_empty() {
            None
        } else {
            Some(ui_font_family)
        });
        // Quad layer — same call as both sites in `app.rs` (~1823, ~2735).
        // Every layer here draws with the pipeline the main window's built
        // (`GpuContext::shared_pipeline`): a detach compiles no shader.
        let quad = QuadLayer::for_gpu(&gpu);

        // Rounded-corner mask — same unconditional construction as the main
        // window's in `App::resumed`, but a PER-WINDOW instance (it caches its
        // uniform/bind group). The CRT pass is built lazily (see the field doc).
        let corner_mask = jetty_render::CornerMask::for_gpu(&gpu);
        let image_layer = jetty_render::ImageLayer::for_gpu(&gpu);

        // Focus the new window so it receives keyboard events immediately.
        window.focus_window();
        window.request_redraw();
        // Focused only once the platform says so: its Focused(true) — the window
        // manager focusing it on map — sets it. Assumed `true`, a window the WM
        // never focused (focus-stealing prevention) counted as watched for good:
        // F9 hid instead of raising, its commands' notifications were skipped,
        // its program got a focus-in.
        let focused = window.has_focus();

        Ok(Self {
            window,
            gpu,
            text,
            chrome_text,
            quad,
            // Allocated on the first CRT frame (see the field doc).
            offscreen: None,
            corner_mask,
            focus_ring: None,
            crt: None,
            crt_key: None,
            glitch: crate::effects::Glitch::default(),
            anim_requested_at: None,
            image_layer,
            backdrop: None,
            caret_anim: None,
            key_paint_due: None,
            ov: crate::overlays::Overlays::default(),
            ime_preedit: None,
            ime_area: None,
            acquire_retry: None,
            perf_ms: 0.0,
            last_frame_at: None,
            perf_idle_at: None,
            perf_idle_shown: false,
            last_present_at: None,
            paced_paint_at: None,
            frame_interval: None,
            focused,
            attention: false,
            fullscreen: false,
            reflow_pending_at: None,
            // build_window above already titled the OS window from tab.title.
            applied_os_title: tab.title.clone(),
            tab,
            cursor: (0.0, 0.0),
            pointer_in: false,
            bar_drag: None,
            bar_drag_start: None,
            resize_zone: crate::app::ResizeZone::None,
            menu_open: None,
            menu_rects: Vec::new(),
            menu_disabled: Vec::new(),
            menu_hover: None,
            menu_pointer_row: None,
            ctrl_hover: jetty_render::CtrlHover::None,
            last_bar_click: None,
            occluded: false,
            scroll_accum: crate::input::ScrollAccumulator::new(),
            selecting: false,
            grid_mouse: crate::gridmouse::GridMouse::default(),
            dragging_scrollbar: false,
            drag_grab_dy: 0.0,
            scrollbar_hover: false,
            link_hover: None,
            link_hover_cell: None,
            caret_fx: None,
            trail: jetty_render::TrailModel::default(),
            trail_layer: None,
            trail_wake: None,
            flood_at: None,
            bell_anim: None,
            bell_limit: crate::motion::RateLimit::default(),
            pulse_anim: None,
            dpi_size: None,
        })
    }

    /// Rebuild this window's GPU stack after a device loss — a new surface on
    /// `gpu_shared` (or a device of its own when that cannot present here) and
    /// fresh layers built like `new` does, the fonts from this window's own
    /// already-loaded font database. The window, its tab and every piece of UI
    /// state are kept. Returns `false` (keeping the lost stack, for a later
    /// retry) when no GPU can be acquired.
    pub(crate) fn rebuild_gpu(
        &mut self,
        gpu_shared: Option<&Arc<jetty_render::GpuShared>>,
        font_logical: f32,
        ui_font_logical: f32,
        font_family: &str,
        ui_font_family: &str,
    ) -> bool {
        let size = self.window.inner_size();
        let scale = self.window.scale_factor() as f32;
        // The lost surface still holds the window's swapchain: release it before
        // the new one is made (`GpuContext::release_surface`).
        self.gpu.release_surface();
        let Some(gpu) = GpuContext::new_sharing(gpu_shared, self.window.clone(), size.width, size.height) else {
            return false;
        };
        let (grid_fonts, chrome_fonts) = (self.text.clone_font_system(), self.text.clone_font_system());
        // The rebuilt grid layer keeps the lost one's row spacing.
        let line_height = self.text.line_height();
        self.text = TextLayer::for_gpu(&gpu, font_logical * scale, font_family, grid_fonts);
        self.text.set_line_height(line_height);
        // The chrome keeps its mono family (`App::chrome_font_family`).
        let chrome_family = self.chrome_text.font_family().to_string();
        let mut chrome_text = TextLayer::for_gpu(&gpu, ui_font_logical * scale, &chrome_family, chrome_fonts);
        chrome_text.set_ui_family(if ui_font_family.is_empty() { None } else { Some(ui_font_family) });
        self.chrome_text = chrome_text;
        self.quad = QuadLayer::for_gpu(&gpu);
        self.corner_mask = jetty_render::CornerMask::for_gpu(&gpu);
        // Device-scoped: rebuilt lazily on the next ring frame.
        self.focus_ring = None;
        // Rebuilt lazily on the new device by the next CRT frame's sync.
        self.crt = None;
        self.crt_key = None;
        self.image_layer = jetty_render::ImageLayer::for_gpu(&gpu);
        // Lazily re-allocated on the next CRT frame, on the new device.
        self.offscreen = None;
        // Rebuilt on the next frame that draws a backdrop, on the new device.
        self.backdrop = None;
        // Rebuilt on the new device by the next glow / trail frame.
        self.caret_fx = None;
        self.trail_layer = None;
        self.acquire_retry = None;
        self.gpu = gpu;
        true
    }

    /// Keep the OS window title (title bar / taskbar) in sync with the tab's
    /// display title — "<title> — JeTTY", as the main window's. Called after
    /// each PTY drain — a no-op string compare (against the tab title last
    /// applied) unless the title actually changed, so it adds nothing to the
    /// idle path. Deliberately NOT gated on occlusion: a minimized window's
    /// taskbar entry must stay correct too.
    pub(crate) fn sync_os_title(&mut self) {
        if self.tab.title != self.applied_os_title {
            self.window.set_title(&os_window_title(&self.tab.title));
            self.applied_os_title = self.tab.title.clone();
        }
    }

    /// THE per-surface paint choke for this detached window (v0.23 central paint
    /// chokepoint). Every producer-category `request_redraw` for a detached window
    /// (PTY output, input, resize, overlay/chrome, sync-flush) routes through here
    /// instead of calling `self.window.request_redraw()` raw, so there is ONE
    /// auditable site per surface and a CI grep can assert no raw producer calls
    /// leak back in.
    ///
    /// NON-stateful by design (v0.23): winit already coalesces multiple
    /// `request_redraw` into a single `RedrawRequested`, so this is a thin, direct
    /// forward — NO `Cell` flag, NO deferred flush. The deliverable is auditability,
    /// not fewer syscalls, and a stateful flag would risk a dropped frame across the
    /// macOS `Wait`/`Poll` seam. `&self`: reads only `self.window`.
    ///
    /// This does NOT gate on `self.occluded`: the LOAD-BEARING occlusion gates that
    /// keep an occluded window at ~0% idle live at the PRODUCER call sites
    /// (`!dw.occluded` around the Wake-drain redraw, sync-flush, etc.) and MUST stay
    /// there verbatim — do not move them into this wrapper.
    pub(crate) fn request_paint(&self) {
        self.window.request_redraw();
    }

    /// This window's display refresh interval (flood pacing), read from its
    /// monitor on first use after a create or a move — never per frame.
    pub(crate) fn frame_interval(&mut self) -> std::time::Duration {
        *self.frame_interval.get_or_insert_with(|| {
            crate::app::refresh_interval(self.window.current_monitor().and_then(|m| m.refresh_rate_millihertz()))
        })
    }

    /// This window's chrome metrics: its OWN DPI (it may sit on a different
    /// monitor than the main window) × the app-wide UI font size. Drives the
    /// title-bar / status-strip / menu geometry and every hit-test against it.
    pub(crate) fn chrome_metrics(&self, ui_font_logical: f32) -> jetty_render::ChromeMetrics {
        jetty_render::ChromeMetrics::new(self.window.scale_factor() as f32, ui_font_logical)
    }

    /// `(top bar height, bottom status-strip height)` of this window's chrome
    /// in physical px — the strip is 0 when the perf HUD is off.
    pub(crate) fn chrome_bands(&self, ui_font_logical: f32, show_perf_hud: bool) -> (f32, f32) {
        let cm = self.chrome_metrics(ui_font_logical);
        (cm.bar_h(), if show_perf_hud { cm.status_h() } else { 0.0 })
    }

    /// THIS window's physical inner padding `(x, y)` for the logical
    /// `padding` — its own DPI, whole pixels (`jetty_render::padding_px`).
    pub(crate) fn pad_px(&self, padding: (f32, f32)) -> (f32, f32) {
        let s = self.window.scale_factor() as f32;
        (jetty_render::padding_px(padding.0, s), jetty_render::padding_px(padding.1, s))
    }

    /// Where THIS window's grid cell (0, 0) sits: below its top bar, inside
    /// the padding (see `jetty_render::grid_geom`).
    pub(crate) fn grid_origin(&self, ui_font_logical: f32, padding: (f32, f32)) -> jetty_render::GridOrigin {
        let (px, py) = self.pad_px(padding);
        jetty_render::GridOrigin::new(px, self.chrome_metrics(ui_font_logical).bar_h() + py)
    }

    /// THIS window's grid cols × rows right now: its surface minus its chrome
    /// bands, the scrollbar gutter (at its own DPI; none when `gutter` is off —
    /// `scrollbar = "never"`) and the padding ([`grid_dims`]).
    pub(crate) fn fit_grid_dims(
        &self,
        ui_font_logical: f32,
        show_perf_hud: bool,
        gutter: bool,
        padding: (f32, f32),
    ) -> (usize, usize) {
        let scale = self.window.scale_factor() as f32;
        grid_dims(
            self.gpu.config.width as f32,
            self.gpu.config.height as f32,
            self.text.cell_size(),
            if gutter { jetty_render::scrollbar_gutter_px(scale) } else { 0.0 },
            self.chrome_bands(ui_font_logical, show_perf_hud),
            self.pad_px(padding),
        )
    }

    /// Where THIS window's scrollbar runs: its grid band (below the title bar,
    /// above the status strip) at its right edge, at its own DPI.
    pub(crate) fn scrollbar_track(&self, ui_font_logical: f32, show_perf_hud: bool) -> jetty_render::ScrollbarTrack {
        let (bar_h, status_h) = self.chrome_bands(ui_font_logical, show_perf_hud);
        let (w, h) = (self.gpu.config.width as f32, self.gpu.config.height as f32);
        jetty_render::ScrollbarTrack::new(w, bar_h, (h - status_h).max(bar_h), self.window.scale_factor() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_requires_at_least_two_tabs() {
        assert!(!can_detach(0));
        assert!(!can_detach(1));
        assert!(can_detach(2));
        assert!(can_detach(5));
    }

    #[test]
    fn take_tab_removes_and_returns_in_range() {
        let mut v = vec!['a', 'b', 'c'];
        assert_eq!(take_tab(&mut v, 1), Some('b'));
        assert_eq!(v, vec!['a', 'c']);
    }

    #[test]
    fn take_tab_out_of_range_is_none_and_no_mutation() {
        let mut v = vec!['a'];
        assert_eq!(take_tab(&mut v, 5), None);
        assert_eq!(v, vec!['a']);
    }

    #[test]
    fn reattached_tab_becomes_active_last() {
        // after pushing onto a vec that now has length 3, active index is 2
        assert_eq!(reattach_index(3), 2);
    }

    #[test]
    fn detached_grid_dims_reserves_top_bar_and_status_strip() {
        // Chrome heights: 36px top bar + 22px status strip → rows shrink;
        // width still only loses the scrollbar gutter.
        // cols = floor((800-14)/10) = 78; rows = floor((600-36-22)/20) = 27.
        assert_eq!(grid_dims(800.0, 600.0, (10.0, 20.0), 14.0, (36.0, 22.0), (0.0, 0.0)), (78, 27));
    }

    #[test]
    fn detached_grid_dims_no_status_strip_when_hud_off() {
        // status_h = 0 (perf HUD off): only the top bar is reserved.
        // rows = floor((600-36)/20) = 28.
        assert_eq!(grid_dims(800.0, 600.0, (10.0, 20.0), 14.0, (36.0, 0.0), (0.0, 0.0)), (78, 28));
    }

    #[test]
    fn detached_grid_dims_zero_cell_falls_back_to_default() {
        assert_eq!(grid_dims(800.0, 600.0, (0.0, 0.0), 14.0, (36.0, 22.0), (0.0, 0.0)), (80, 24));
    }

    #[test]
    fn detached_grid_dims_reserve_the_padding() {
        // 1×, padding 8 × 4: cols = floor((800 - 8 - max(8, 18)) / 10) = 77;
        // rows = floor((600 - 36 - 22 - 2·4) / 20) = 26.
        assert_eq!(grid_dims(800.0, 600.0, (10.0, 20.0), 18.0, (36.0, 22.0), (8.0, 4.0)), (77, 26));
        // 2× (every length doubled): the same grid.
        assert_eq!(grid_dims(1600.0, 1200.0, (20.0, 40.0), 36.0, (72.0, 44.0), (16.0, 8.0)), (77, 26));
    }

    // ── tear-out threshold ───────────────────────────────────────────────────

    #[test]
    fn tearing_requires_leaving_the_strip_by_more_than_threshold() {
        // Top-mode bar at y 0..36, threshold 24.
        assert!(!tearing(18.0, 0.0, 36.0, 24.0), "inside the strip");
        assert!(!tearing(50.0, 0.0, 36.0, 24.0), "below strip but within threshold (36+24=60)");
        assert!(tearing(61.0, 0.0, 36.0, 24.0), "beyond the threshold below");
        // Above the strip: bar at y 0 means cursor_y can't go below -24 in
        // practice, but the math still holds for a bottom-mode bar.
        assert!(!tearing(580.0, 578.0, 36.0, 24.0), "inside a bottom-mode strip");
        assert!(tearing(553.0, 578.0, 36.0, 24.0), "torn upward out of a bottom strip");
        assert!(!tearing(560.0, 578.0, 36.0, 24.0), "above bottom strip but within threshold");
    }

    // ── drag along the strip: reorder ────────────────────────────────────────

    fn rect(x: f32, w: f32) -> jetty_render::Rect {
        jetty_render::Rect::new(x, 0.0, w, 36.0, [0; 4])
    }

    #[test]
    fn a_tab_dragged_along_the_strip_targets_the_slot_under_the_pointer() {
        // Three 140px tabs from x 8; an overflowed one parked offscreen.
        let parked = rect(-1.0e6, 0.0);
        let rects = [rect(8.0, 140.0), rect(148.0, 140.0), rect(288.0, 140.0), parked];
        assert_eq!(reorder_target(20.0, &rects), Some(0));
        assert_eq!(reorder_target(148.0, &rects), Some(1), "a slot starts at its left edge");
        assert_eq!(reorder_target(427.0, &rects), Some(2));
        assert_eq!(reorder_target(500.0, &rects), None, "past the last tab (the \"+\")");
        assert_eq!(reorder_target(2.0, &rects), None, "before the first");
        assert_eq!(reorder_target(-1.0e6, &rects), None, "a parked rect never matches");
    }

    #[test]
    fn moving_a_tab_shifts_the_others_and_keeps_every_index_on_its_tab() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert!(move_item(&mut v, 0, 2));
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
        assert!(move_item(&mut v, 3, 1));
        assert_eq!(v, vec!['b', 'd', 'c', 'a']);
        assert!(!move_item(&mut v, 4, 0), "out of range: untouched");
        assert!(!move_item(&mut v, 0, 9));
        assert_eq!(v, vec!['b', 'd', 'c', 'a']);
        // Every index follows its element.
        let before = ['a', 'b', 'c', 'd', 'e'];
        for (from, to) in [(0, 4), (4, 0), (1, 3), (3, 1), (2, 2)] {
            let mut v = before.to_vec();
            move_item(&mut v, from, to);
            for (i, &c) in before.iter().enumerate() {
                assert_eq!(v[index_after_move(i, from, to)], c, "{from}→{to}: {c}");
            }
        }
    }

    #[test]
    fn tearing_cancelled_when_returning_to_the_strip() {
        // A drag that tore out (y=100) then returned to the strip (y=20)
        // reads as not-tearing again — the release is a plain tab click.
        assert!(tearing(100.0, 0.0, 36.0, 24.0));
        assert!(!tearing(20.0, 0.0, 36.0, 24.0));
    }

    // ── drop-to-reattach target rect ────────────────────────────────────────

    #[test]
    fn main_tabbar_hit_top_mode() {
        // Main window at (100, 50), 1000×640, bar at top (y 50..86 global).
        assert!(main_tabbar_contains(500.0, 60.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, false));
        assert!(!main_tabbar_contains(500.0, 90.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, false), "below the band");
        assert!(!main_tabbar_contains(50.0, 60.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, false), "left of the window");
        assert!(!main_tabbar_contains(1150.0, 60.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, false), "right of the window");
    }

    #[test]
    fn main_tabbar_hit_bottom_mode_respects_status_strip() {
        // Bottom mode: band sits at h - 36 - 22 = 582..618 local → 632..668 global.
        assert!(main_tabbar_contains(500.0, 640.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, true));
        assert!(!main_tabbar_contains(500.0, 60.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, true), "top band is not a target in bottom mode");
        assert!(!main_tabbar_contains(500.0, 680.0, 100.0, 50.0, 1000.0, 640.0, 36.0, 22.0, true), "the status strip below the band is not a target");
    }

    // ── on-screen clamp for the drop-placed window ──────────────────────────

    #[test]
    fn a_torn_off_tab_lands_where_it_was_dropped_at_mixed_dpi() {
        // X11, monitor A 1920 px at 1×, monitor B at x = 1920, 3840 px at 2×
        // (per-monitor scales from RandR, no Xft.dpi). Root coordinates are
        // one PIXEL space: a drop at x = 3000 is on B. Each monitor's own
        // logical rect (B: 960..2880) is not part of any shared space — there
        // the drop matched no monitor and the window ended up on A.
        let x11 = |px: f64, sc: f64| px / desktop_unit_scale(sc, false);
        let mons = [(x11(0.0, 1.0), 0.0, x11(1920.0, 1.0), 1080.0), (x11(1920.0, 2.0), 0.0, x11(3840.0, 2.0), 2160.0)];
        assert_eq!(drop_position((3000.0, 500.0), (1600.0, 1000.0), &mons), (3000.0, 500.0));
        // Near B's right edge: kept on B, never pushed back onto A.
        assert_eq!(drop_position((5500.0, 500.0), (1600.0, 1000.0), &mons), (5760.0 - 1600.0, 500.0));
        // macOS lays displays out in points: each rect / its own scale.
        let mac = |pt: f64, sc: f64| pt / desktop_unit_scale(sc, true);
        let mons = [(0.0, 0.0, mac(2880.0, 2.0), mac(1800.0, 2.0)), (mac(2880.0, 2.0), 0.0, 1920.0, 1080.0)];
        assert_eq!(drop_position((2000.0, 300.0), (800.0, 500.0), &mons), (2000.0, 300.0));
        // Off every monitor: the nearest one; none known: the drop point.
        assert_eq!(drop_position((-300.0, 200.0), (800.0, 500.0), &mons), (0.0, 200.0));
        assert_eq!(drop_position((-300.0, 200.0), (800.0, 500.0), &[]), (-300.0, 200.0));
    }

    #[test]
    fn clamp_pos_keeps_window_on_the_monitor() {
        let mon = (0, 0, 1920, 1080);
        assert_eq!(clamp_pos(100, 100, 800, 600, mon), (100, 100), "already inside");
        assert_eq!(clamp_pos(1900, 1000, 800, 600, mon), (1120, 480), "clamped to bottom-right");
        assert_eq!(clamp_pos(-50, -50, 800, 600, mon), (0, 0), "clamped to origin");
        // Secondary monitor with a nonzero origin.
        let mon2 = (1920, 0, 1920, 1080);
        assert_eq!(clamp_pos(1000, 10, 800, 600, mon2), (1920, 10), "pinned to the monitor's left edge");
    }

    // ── corner-mask radii ────────────────────────────────────────────────────

    #[test]
    fn detached_rounds_all_four_corners() {
        // A detached window is free-floating: unlike Dropdown (top-flush) mode,
        // ALL FOUR corners get the same configured radius.
        assert_eq!(corner_radii(12.0), (12.0, 12.0, 12.0, 12.0));
        assert_eq!(corner_radii(0.0), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn detached_corner_mask_carves_every_corner() {
        // Feed the detached radii through the SAME coverage math the GPU mask
        // uses: with radius 12 every corner pixel of a 100×100 frame goes
        // transparent while the center stays opaque.
        let (tl, tr, bl, br) = corner_radii(12.0);
        for &(x, y) in &[(0.0, 0.0), (99.0, 0.0), (0.0, 99.0), (99.0, 99.0)] {
            let cov =
                jetty_render::rounded_rect_coverage_per(x, y, 100.0, 100.0, tl, tr, bl, br);
            assert!(cov < 0.01, "corner ({x},{y}) should round, got {cov}");
        }
        let center =
            jetty_render::rounded_rect_coverage_per(50.0, 50.0, 100.0, 100.0, tl, tr, bl, br);
        assert!((center - 1.0).abs() < 1e-4, "center should stay opaque, got {center}");
    }

    // ── context-menu item lists ─────────────────────────────────────────────

    #[test]
    fn tab_menu_hides_detach_at_one_tab() {
        assert_eq!(tab_menu_items(can_detach(2)), vec!["Detach", "Rename", "Color ▸", "Close Tab"]);
        assert_eq!(tab_menu_items(can_detach(1)), vec!["Rename", "Color ▸", "Close Tab"]);
    }

    #[test]
    fn tab_color_list_maps_rows_to_palette_indices() {
        let items = tab_color_menu_items();
        assert_eq!(items, vec!["No Color", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan"]);
        assert_eq!(tab_color_from_label("No Color"), Some(None));
        assert_eq!(tab_color_from_label("Red"), Some(Some(1)));
        assert_eq!(tab_color_from_label("Cyan"), Some(Some(6)));
        // The main menu's own rows are not colors (no accidental match).
        for l in tab_menu_items(true) {
            assert_eq!(tab_color_from_label(l), None, "{l}");
        }
        // No color row carries a shortcut hint (the swatch takes that column).
        for l in &items {
            assert!(menu_action(l).is_none(), "{l}");
        }
    }

    #[test]
    fn the_color_list_is_told_apart_from_the_tab_menu() {
        // Left goes back only from the color list.
        assert!(is_tab_color_list(&tab_color_menu_items()));
        for can in [true, false] {
            assert!(!is_tab_color_list(&tab_menu_items(can)));
        }
        assert!(!is_tab_color_list(&[]));
        // The tab menu's submenu row is in both of its variants.
        assert!(tab_menu_items(false).contains(&TAB_MENU_COLOR));
    }

    #[test]
    fn tab_color_swatches_sit_inside_their_rows_and_ring_the_current_one() {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let cm = jetty_render::ChromeMetrics::DEFAULT;
        let labels = tab_color_menu_items();
        let rects: Vec<jetty_render::Rect> = (0..labels.len())
            .map(|i| jetty_render::Rect::new(100.0, 50.0 + i as f32 * 28.0, 210.0, 28.0, [0; 4]))
            .collect();
        let sw = tab_color_swatches(&rects, &labels, &theme, Some(2), cm);
        // 6 color swatches + the "No Color" outline (2 quads) + one ring.
        assert_eq!(sw.len(), 6 + 2 + 1);
        for s in &sw {
            let row = rects.iter().find(|r| s.y >= r.y - 3.0 && s.y + s.h <= r.y + r.h + 3.0).expect("in a row");
            assert!(s.x >= row.x && s.x + s.w <= row.x + row.w);
        }
        let green = theme.palette[2];
        assert!(sw.iter().any(|q| q.color == [green[0], green[1], green[2], 255]));
        // The main menu's rows get no swatches.
        assert!(tab_color_swatches(&rects[..4], &tab_menu_items(true), &theme, None, cm).is_empty());
    }

    #[test]
    fn detached_menu_is_reattach_copy_paste_run_select_all_clear() {
        // Pinned order — app.rs's detached click dispatch matches on these
        // hard indices (0 Reattach, 1 Copy, 2 Paste, 3 Run in New Tab,
        // 4 Select All, 5 Clear).
        assert_eq!(DETACHED_MENU_ITEMS, ["Reattach", "Copy", "Paste", "Run in New Tab", "Select All", "Clear"]);
        // Their hints are the main menu's (Select All has none on Linux).
        let km = crate::keymap::KeyMap::defaults();
        let main = context_menu_hints(&km);
        assert_eq!(menu_hint(&km, "Select All"), main[3]);
        assert_eq!(menu_hint(&km, "Clear"), main[4]);
    }

    #[test]
    fn menu_hints_match_key_bindings() {
        // Hints come from the live keymap; on Linux the defaults reproduce the
        // menus' historical glyphs exactly (macOS shows its ⌘ chords instead).
        let km = crate::keymap::KeyMap::defaults();
        if !cfg!(target_os = "macos") {
            assert_eq!(menu_hint(&km, "Detach"), "⇧⌃D");
            assert_eq!(menu_hint(&km, "Reattach"), "⇧⌃D");
            assert_eq!(menu_hint(&km, "Copy"), "⇧⌃C");
            assert_eq!(menu_hint(&km, "Paste"), "⇧⌃V");
            assert_eq!(menu_hint(&km, "Run in New Tab"), "⇧⌃⏎");
            assert_eq!(menu_hint(&km, "Close Tab"), "⇧⌃W");
        }
        assert_eq!(menu_hint(&km, "Rename"), "");
        assert!(menu_action("Rename").is_none());
    }

    #[test]
    fn every_window_is_named_jetty_in_the_taskbar() {
        // The main window's OS title always ended in " — JeTTY"; a detached
        // window's was the bare tab title.
        assert_eq!(os_window_title("htop"), "htop — JeTTY");
    }

    #[test]
    fn focus_in_detached_matches_a_live_detached_id() {
        // Focus moved from the main window to one of our own detached windows:
        // the main window must NOT auto-hide.
        assert!(focus_in_detached(Some(7), &[3, 7, 9]));
    }

    #[test]
    fn focus_in_detached_false_for_third_party_or_none() {
        // Focus left to a third app (id not among ours) → main may auto-hide.
        assert!(!focus_in_detached(Some(42), &[3, 7, 9]));
        // No tracked focus target → not one of ours.
        assert!(!focus_in_detached(None, &[3, 7, 9]));
        // No detached windows at all → never a match.
        assert!(!focus_in_detached(Some(7), &[]));
    }
}
