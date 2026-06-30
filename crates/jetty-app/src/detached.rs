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

// ── DetachedWindow ────────────────────────────────────────────────────────────

use std::sync::Arc;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;
use jetty_render::{GpuContext, QuadLayer, TextLayer};

use crate::app::Tab;

/// Default terminal font size (logical pixels) used when constructing a
/// detached window. Mirrors `FONT_LOGICAL_DEFAULT` in `app.rs` (16.0).
const DETACHED_FONT_DEFAULT: f32 = 16.0;

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
    pub chrome_text: TextLayer,
    pub quad: QuadLayer,
    /// Surface-sized offscreen render target (same descriptor as
    /// `App::make_offscreen`). Required for CRT and Tier-B summon effects.
    pub offscreen: (wgpu::Texture, wgpu::TextureView),
    /// The single terminal session owned by this detached window.
    pub tab: Tab,
}

impl DetachedWindow {
    /// Construct a detached window sized `w × h` (physical pixels) that owns
    /// `tab`. Mirrors the construction in `App::toggle_settings_window` and
    /// `App::resumed` — same `GpuContext::new`, same `TextLayer`/`QuadLayer`
    /// descriptors, same offscreen-texture descriptor.
    ///
    /// `DetachedWindow::new` may produce an `#[allow(dead_code)]` warning until
    /// Task 4 wires the detach keybinding.
    pub(crate) fn new(event_loop: &ActiveEventLoop, tab: Tab, w: u32, h: u32) -> Self {
        // Title the OS window from the tab (mirrors how the tab bar displays it).
        let window = jetty_platform::build_window(
            event_loop,
            &tab.title,
            (w, h),
        );
        let size = window.inner_size();
        // HiDPI: same scale-factor handling as the main window in `resumed`.
        let scale = window.scale_factor() as f32;

        // GPU context — identical call to App::resumed (`app.rs` ~2722) and
        // `toggle_settings_window` (`app.rs` ~1800).
        let gpu = GpuContext::new(window.clone(), size.width, size.height)
            .expect("DetachedWindow: GPU init failed — no suitable adapter");

        let font_px = DETACHED_FONT_DEFAULT * scale;

        // Terminal content layer — mirrors `TextLayer::new_with_family` used in
        // `toggle_settings_window` (`app.rs` ~1808). Using `new_with_family`
        // (not `new_with_family_and_fonts`) keeps the constructor synchronous;
        // Task 4 can propagate the live font family from `App` if needed.
        let text = TextLayer::new_with_family(
            &gpu.device, &gpu.queue, gpu.format, font_px, "",
        );
        // Chrome layer — mirrors the chrome TextLayer built in `resumed`
        // (`app.rs` ~2800): same device/queue/format, UI-sized font.
        let chrome_text = TextLayer::new_with_family(
            &gpu.device, &gpu.queue, gpu.format, font_px, "",
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
}
