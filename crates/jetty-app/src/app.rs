use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

#[derive(Default)]
pub struct App {
    window: Option<Arc<Window>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window = jetty_platform::build_window(event_loop, "Jetty", (1000, 640));
            self.window = Some(window);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }
}
