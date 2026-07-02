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
    /// `App::make_offscreen`). Reserved for future CRT and Tier-B summon effects
    /// in detached windows (built now for parity; not yet read by the renderer).
    #[allow(dead_code)]
    pub offscreen: (wgpu::Texture, wgpu::TextureView),
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
    /// Cached resize-edge zone so `set_cursor` fires only on zone changes
    /// (mirrors `App::resize_cursor` for the borderless main window).
    pub resize_zone: crate::app::ResizeZone,
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
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        tab: Tab,
        w_logical: u32,
        h_logical: u32,
        font_logical: f32,
        ui_font_logical: f32,
        font_family: &str,
        ui_font_family: &str,
    ) -> Self {
        // Title the OS window from the tab (mirrors how the tab bar displays it).
        let window = jetty_platform::build_window(
            event_loop,
            &tab.title,
            (w_logical, h_logical),
        );
        let size = window.inner_size();
        // HiDPI: same scale-factor handling as the main window in `resumed`.
        let scale = window.scale_factor() as f32;

        // GPU context — identical call to App::resumed (`app.rs` ~2722) and
        // `toggle_settings_window` (`app.rs` ~1800).
        let gpu = GpuContext::new(window.clone(), size.width, size.height)
            .expect("DetachedWindow: GPU init failed — no suitable adapter");

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

        // Focus the new window so it receives keyboard events immediately.
        window.focus_window();
        window.request_redraw();

        Self {
            window,
            gpu,
            text,
            chrome_text,
            quad,
            offscreen,
            tab,
            cursor: (0.0, 0.0),
            bar_drag: None,
            resize_zone: crate::app::ResizeZone::None,
            close_hover: false,
            last_bar_click: None,
        }
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
