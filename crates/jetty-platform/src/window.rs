use std::sync::Arc;
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

/// Build the main window. M0: a normal decorated window; overlay flags arrive in M2.
pub fn build_window(event_loop: &ActiveEventLoop, title: &str, size: (u32, u32)) -> Arc<Window> {
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_resizable(true)
        .with_min_inner_size(LogicalSize::new(200.0_f64, 120.0_f64))
        .with_transparent(true);
    Arc::new(event_loop.create_window(attrs).expect("create_window failed"))
}
