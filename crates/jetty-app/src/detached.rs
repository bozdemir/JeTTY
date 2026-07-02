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
/// client area minus the scrollbar gutter (width), the top bar (`top_bar_h`,
/// normally TABBAR_H) and the bottom status strip (`status_h`, 0 when the perf
/// HUD is off). `width_px`/`height_px` are physical pixels; `cell_w`/`cell_h`
/// the glyph cell size. Mirrors the main window's `grid_dims`/`reflow` math.
pub(crate) fn grid_dims(
    width_px: f32,
    height_px: f32,
    cell_w: f32,
    cell_h: f32,
    scrollbar_gutter: f32,
    top_bar_h: f32,
    status_h: f32,
) -> (usize, usize) {
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return (80, 24); // fallback, matches app.rs FALLBACK_COLS/ROWS
    }
    let cols = ((width_px - scrollbar_gutter) / cell_w).floor().max(2.0) as usize;
    let rows = ((height_px - top_bar_h - status_h) / cell_h).floor().max(1.0) as usize;
    (cols, rows)
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

/// True when the GLOBAL cursor `(gx, gy)` lands inside the MAIN window's
/// tab-bar strip: the band `tabbar_h` tall at the top of the main window, or —
/// when `tab_bar_bottom` — just above the status strip at the bottom (the same
/// band `App::tabbar_y` computes). `main_x/main_y` is the main window's outer
/// position; `main_w/main_h` its physical surface size.
#[allow(clippy::too_many_arguments)]
pub fn main_tabbar_contains(
    gx: f64,
    gy: f64,
    main_x: i32,
    main_y: i32,
    main_w: u32,
    main_h: u32,
    tabbar_h: f32,
    status_h: f32,
    tab_bar_bottom: bool,
) -> bool {
    let lx = gx - main_x as f64;
    let ly = gy - main_y as f64;
    if lx < 0.0 || lx > main_w as f64 {
        return false;
    }
    let bar_y = if tab_bar_bottom {
        (main_h as f32 - tabbar_h - status_h).max(0.0) as f64
    } else {
        0.0
    };
    ly >= bar_y && ly < bar_y + tabbar_h as f64
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
        vec!["Detach", "Rename", "Close Tab"]
    } else {
        vec!["Rename", "Close Tab"]
    }
}

/// Items of a DETACHED window's context menu (right-click anywhere).
pub const DETACHED_MENU_ITEMS: [&str; 3] = ["Reattach", "Copy", "Paste"];

/// Per-corner radii (tl, tr, bl, br) for a detached window's corner mask.
/// A detached window is a free-floating window — it is never docked top-flush
/// like the main window's Dropdown mode — so ALL FOUR corners round with the
/// same configured radius (the main window's top-square nuance never applies).
pub fn corner_radii(radius_px: f32) -> (f32, f32, f32, f32) {
    (radius_px, radius_px, radius_px, radius_px)
}

/// Right-aligned keyboard-shortcut hint for a menu label (same symbols as
/// `menu::MENU_HINTS`; blank when the action has no binding).
pub fn menu_hint(label: &str) -> &'static str {
    match label {
        "Detach" | "Reattach" => "⇧⌃D",
        "Copy" => "⇧⌃C",
        "Paste" => "⇧⌃V",
        "Close Tab" => "⇧⌃W",
        _ => "",
    }
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
/// bar (title + close ✕, draggable to move) and — when the perf HUD is on — the
/// same bottom status strip as the main window.
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
    /// the same routing as the main window. Lazily re-allocated on size change
    /// by `App::render_detached_window` (mirrors the main stale check).
    pub offscreen: (wgpu::Texture, wgpu::TextureView),
    /// Per-window rounded-corner mask pass (same radius as the main window's;
    /// all four corners round — see `corner_radii`). Per-window instance because
    /// `CornerMask` caches its uniform/bind group; sharing across surfaces of
    /// different sizes would thrash it.
    pub corner_mask: jetty_render::CornerMask,
    /// Per-window CRT post-pass. Per-window instance because `Crt` caches its
    /// bind group keyed by the sampled src view — sharing the main window's
    /// instance would thrash the cache between windows.
    pub crt: jetty_render::Crt,
    /// Caret flash burst clock for keystrokes typed in THIS window (mirrors
    /// `App::caret_anim` for the main window). `None` = no burst live.
    pub caret_anim: Option<std::time::Instant>,
    /// When `Some`, a grid+PTY reflow is scheduled for this instant (mirrors
    /// `App::reflow_pending_at`): a border drag fires many Resized events, and
    /// reflow+SIGWINCH per event scatters p10k's prompt — the surface resizes
    /// live, ONE reflow fires ~250ms after the drag settles (`about_to_wait`).
    pub reflow_pending_at: Option<std::time::Instant>,
    /// The single terminal session owned by this detached window.
    pub tab: Tab,
    /// Last known cursor position inside THIS window (physical px).
    pub cursor: (f64, f64),
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
    /// Menu item currently under the cursor (hover highlight).
    pub menu_hover: Option<usize>,
    /// Whether the cursor is over the close ✕ (drives the red hover highlight).
    pub close_hover: bool,
    /// Time + position of the last left press on the top bar, for the
    /// double-click → maximize toggle (mirrors `App::last_strip_click`).
    pub last_bar_click: Option<(std::time::Instant, f32, f32)>,
}

impl DetachedWindow {
    /// Construct a detached window sized `w_logical × h_logical` (logical /
    /// device-independent pixels) that owns `tab`. Mirrors the construction in
    /// `App::toggle_settings_window` and `App::resumed` — same `GpuContext::new`,
    /// same `TextLayer`/`QuadLayer` descriptors, same offscreen-texture descriptor.
    ///
    /// `font_logical` and `ui_font_logical` are the caller's current logical font
    /// sizes (same values stored in `App::font_logical` and `App::ui_font_logical`).
    /// `font_family` is the terminal font family (same as `App::font_family`);
    /// `ui_font_family` the chrome family (`""` = platform sans, same as
    /// `App::ui_font_family`). Both sizes are scaled by the new window's
    /// `scale_factor` before being passed to `TextLayer`, matching `App::resumed`.
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
    ) -> Result<Self, Tab> {
        // Title the OS window from the tab (mirrors how the tab bar displays it).
        let window = match jetty_platform::build_window(
            event_loop,
            &tab.title,
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

        // GPU context — identical call to App::resumed (`app.rs` ~2722) and
        // `toggle_settings_window` (`app.rs` ~1800). Failure hands the tab back
        // (the main + settings windows handle the same None gracefully).
        let gpu = match GpuContext::new(window.clone(), size.width, size.height) {
            Some(g) => g,
            None => {
                eprintln!("jetty: detached window GPU init failed — no suitable adapter");
                return Err(tab);
            }
        };

        // Terminal content layer — mirrors `TextLayer::new_with_family` used in
        // `App::resumed` (`app.rs` ~2728): terminal font at logical × scale_factor.
        let text = TextLayer::new_with_family(
            &gpu.device, &gpu.queue, gpu.format, font_logical * scale, font_family,
        );
        // Chrome layer — mirrors the chrome TextLayer built in `App::resumed`
        // (`app.rs` ~2801): UI font at ui_font_logical × scale_factor, with the
        // chrome family applied via `set_ui_family` (no fontconfig rescan).
        let mut chrome_text = TextLayer::new_with_family(
            &gpu.device, &gpu.queue, gpu.format, ui_font_logical * scale, font_family,
        );
        chrome_text.set_ui_family(if ui_font_family.is_empty() {
            None
        } else {
            Some(ui_font_family)
        });
        // Quad layer — same call as both sites in `app.rs` (~1823, ~2735).
        let quad = QuadLayer::new(&gpu.device, gpu.format);

        // Offscreen texture — verbatim copy of `App::make_offscreen` (~939).
        let offscreen = {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("detached-offscreen"),
                size: wgpu::Extent3d {
                    width: gpu.config.width.max(1),
                    height: gpu.config.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: gpu.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (tex, view)
        };

        // Rounded-corner mask + CRT post-pass — same unconditional construction
        // as the main window's in `App::resumed` (app.rs ~3523/~3537), but as
        // PER-WINDOW instances (both cache uniforms/bind groups; see field docs).
        let corner_mask = jetty_render::CornerMask::new(&gpu.device, gpu.format);
        let crt = jetty_render::Crt::new(&gpu.device, gpu.format);

        // Focus the new window so it receives keyboard events immediately.
        window.focus_window();
        window.request_redraw();

        Ok(Self {
            window,
            gpu,
            text,
            chrome_text,
            quad,
            offscreen,
            corner_mask,
            crt,
            caret_anim: None,
            reflow_pending_at: None,
            tab,
            cursor: (0.0, 0.0),
            bar_drag: None,
            bar_drag_start: None,
            resize_zone: crate::app::ResizeZone::None,
            menu_open: None,
            menu_rects: Vec::new(),
            menu_hover: None,
            close_hover: false,
            last_bar_click: None,
        })
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
        assert_eq!(grid_dims(800.0, 600.0, 10.0, 20.0, 14.0, 36.0, 22.0), (78, 27));
    }

    #[test]
    fn detached_grid_dims_no_status_strip_when_hud_off() {
        // status_h = 0 (perf HUD off): only the top bar is reserved.
        // rows = floor((600-36)/20) = 28.
        assert_eq!(grid_dims(800.0, 600.0, 10.0, 20.0, 14.0, 36.0, 0.0), (78, 28));
    }

    #[test]
    fn detached_grid_dims_zero_cell_falls_back_to_default() {
        assert_eq!(grid_dims(800.0, 600.0, 0.0, 0.0, 14.0, 36.0, 22.0), (80, 24));
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
        assert!(main_tabbar_contains(500.0, 60.0, 100, 50, 1000, 640, 36.0, 22.0, false));
        assert!(!main_tabbar_contains(500.0, 90.0, 100, 50, 1000, 640, 36.0, 22.0, false), "below the band");
        assert!(!main_tabbar_contains(50.0, 60.0, 100, 50, 1000, 640, 36.0, 22.0, false), "left of the window");
        assert!(!main_tabbar_contains(1150.0, 60.0, 100, 50, 1000, 640, 36.0, 22.0, false), "right of the window");
    }

    #[test]
    fn main_tabbar_hit_bottom_mode_respects_status_strip() {
        // Bottom mode: band sits at h - 36 - 22 = 582..618 local → 632..668 global.
        assert!(main_tabbar_contains(500.0, 640.0, 100, 50, 1000, 640, 36.0, 22.0, true));
        assert!(!main_tabbar_contains(500.0, 60.0, 100, 50, 1000, 640, 36.0, 22.0, true), "top band is not a target in bottom mode");
        assert!(!main_tabbar_contains(500.0, 680.0, 100, 50, 1000, 640, 36.0, 22.0, true), "the status strip below the band is not a target");
    }

    // ── on-screen clamp for the drop-placed window ──────────────────────────

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
        assert_eq!(tab_menu_items(can_detach(2)), vec!["Detach", "Rename", "Close Tab"]);
        assert_eq!(tab_menu_items(can_detach(1)), vec!["Rename", "Close Tab"]);
    }

    #[test]
    fn detached_menu_is_reattach_copy_paste() {
        assert_eq!(DETACHED_MENU_ITEMS, ["Reattach", "Copy", "Paste"]);
    }

    #[test]
    fn menu_hints_match_key_bindings() {
        assert_eq!(menu_hint("Detach"), "⇧⌃D");
        assert_eq!(menu_hint("Reattach"), "⇧⌃D");
        assert_eq!(menu_hint("Copy"), "⇧⌃C");
        assert_eq!(menu_hint("Paste"), "⇧⌃V");
        assert_eq!(menu_hint("Close Tab"), "⇧⌃W");
        assert_eq!(menu_hint("Rename"), "");
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
