use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
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
pub fn build_window(event_loop: &ActiveEventLoop, title: &str, size: (u32, u32)) -> Arc<Window> {
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
    Arc::new(event_loop.create_window(attrs).expect("create_window failed"))
}

/// Build a fixed-size, non-resizable utility window (e.g. the settings dialog).
/// A normal decorated OS window the user can move anywhere; also carries the icon.
pub fn build_fixed_window(
    event_loop: &ActiveEventLoop,
    title: &str,
    size: (u32, u32),
) -> Arc<Window> {
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_window_icon(app_icon())
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_resizable(false);
    Arc::new(event_loop.create_window(attrs).expect("create_window failed"))
}
