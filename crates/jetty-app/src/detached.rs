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

/// Cols/rows for a BARE detached terminal window (no tab bar, no status bar):
/// the grid fills the whole client area minus the scrollbar gutter. `width_px`/
/// `height_px` are physical pixels; `cell_w`/`cell_h` the glyph cell size.
pub(crate) fn grid_dims(width_px: f32, height_px: f32, cell_w: f32, cell_h: f32, scrollbar_gutter: f32) -> (usize, usize) {
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return (80, 24); // fallback, matches app.rs FALLBACK_COLS/ROWS
    }
    let cols = ((width_px - scrollbar_gutter) / cell_w).floor().max(2.0) as usize;
    let rows = (height_px / cell_h).floor().max(1.0) as usize;
    (cols, rows)
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
/// No tab bar is present — a detached window always contains exactly one tab.
pub(crate) struct DetachedWindow {
    pub window: Arc<Window>,
    pub gpu: GpuContext,
    /// Terminal-font TextLayer for the tab's grid content.
    pub text: TextLayer,
    /// UI-font TextLayer for window chrome (title, status bar, overlays).
    /// Reserved for future chrome parity with the main window (a detached window
    /// currently renders a bare terminal, so this is built but not yet read).
    #[allow(dead_code)]
    pub chrome_text: TextLayer,
    pub quad: QuadLayer,
    /// Surface-sized offscreen render target (same descriptor as
    /// `App::make_offscreen`). Reserved for future CRT and Tier-B summon effects
    /// in detached windows (built now for parity; not yet read by the bare renderer).
    #[allow(dead_code)]
    pub offscreen: (wgpu::Texture, wgpu::TextureView),
    /// The single terminal session owned by this detached window.
    pub tab: Tab,
}

impl DetachedWindow {
    /// Construct a detached window sized `w_logical × h_logical` (logical /
    /// device-independent pixels) that owns `tab`. Mirrors the construction in
    /// `App::toggle_settings_window` and `App::resumed` — same `GpuContext::new`,
    /// same `TextLayer`/`QuadLayer` descriptors, same offscreen-texture descriptor.
    ///
    /// `font_logical` and `ui_font_logical` are the caller's current logical font
    /// sizes (same values stored in `App::font_logical` and `App::ui_font_logical`).
    /// `font_family` is the terminal font family (same as `App::font_family`).
    /// Both are scaled by the new window's `scale_factor` before being passed to
    /// `TextLayer`, matching how `App::resumed` builds the main-window layers.
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        tab: Tab,
        w_logical: u32,
        h_logical: u32,
        font_logical: f32,
        ui_font_logical: f32,
        font_family: &str,
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
        // (`app.rs` ~2801): UI font at ui_font_logical × scale_factor.
        let chrome_text = TextLayer::new_with_family(
            &gpu.device, &gpu.queue, gpu.format, ui_font_logical * scale, font_family,
        );
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

        Self { window, gpu, text, chrome_text, quad, offscreen, tab }
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
    fn detached_grid_dims_fills_client_area_minus_gutter() {
        // No tab bar, no status bar: only the scrollbar gutter is subtracted
        // from width; height is used in full.
        assert_eq!(grid_dims(800.0, 600.0, 10.0, 20.0, 14.0), (78, 30));
    }

    #[test]
    fn detached_grid_dims_zero_cell_falls_back_to_default() {
        assert_eq!(grid_dims(800.0, 600.0, 0.0, 0.0, 14.0), (80, 24));
    }
}
