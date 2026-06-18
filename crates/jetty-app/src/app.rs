use std::io::Write;
use std::sync::Arc;
use jetty_core::{PtySession, Terminal};
use jetty_render::{GpuContext, TextLayer};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const COLS: usize = 100;
const ROWS: usize = 30;

pub struct App {
    proxy: EventLoopProxy<()>,
    window: Option<Arc<Window>>,
    gpu: Option<GpuContext>,
    text: Option<TextLayer>,
    terminal: Terminal,
    pty: Option<PtySession>,
    writer: Option<Box<dyn Write + Send>>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<()>) -> Self {
        App {
            proxy,
            window: None,
            gpu: None,
            text: None,
            terminal: Terminal::new(COLS, ROWS),
            pty: None,
            writer: None,
        }
    }

    fn drain_pty(&mut self) {
        if let Some(pty) = &self.pty {
            while let Ok(chunk) = pty.output().try_recv() {
                self.terminal.feed(&chunk);
            }
        }
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = jetty_platform::build_window(event_loop, "Jetty", (1000, 640));
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        let text = TextLayer::new(&gpu.device, &gpu.queue, gpu.format, 16.0);

        let pty = PtySession::spawn(COLS as u16, ROWS as u16).expect("pty");
        let writer = pty.writer();

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.text = Some(text);
        self.pty = Some(pty);
        self.writer = Some(writer);

        spawn_waker(self.proxy.clone());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _ev: ()) {
        self.drain_pty();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.gpu, &mut self.text) {
                    text.resize(gpu);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive y = wheel up = scroll into history (older output).
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y.round() as i32) * 3,
                    MouseScrollDelta::PixelDelta(p) => {
                        // Approximate cell height; use 20.0 as a reasonable default.
                        const CELL_H: f64 = 20.0;
                        (p.y / CELL_H).round() as i32
                    }
                };
                if lines != 0 {
                    self.terminal.scroll_lines(lines);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                // PageUp/Down scroll the terminal without sending to the PTY.
                match &event.logical_key {
                    Key::Named(NamedKey::PageUp) => {
                        self.terminal.scroll_page(true);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.terminal.scroll_page(false);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    _ => {}
                }
                if let Some(bytes) = key_to_bytes(&event.logical_key) {
                    // Any real keystroke jumps back to the bottom so the user sees their input.
                    self.terminal.scroll_to_bottom();
                    if let Some(w) = &mut self.writer {
                        let _ = w.write_all(&bytes);
                        let _ = w.flush();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.drain_pty();
                if let (Some(gpu), Some(text)) = (&mut self.gpu, &mut self.text) {
                    let snap = self.terminal.snapshot();
                    let _ = text.render(gpu, &snap);
                }
            }
            _ => {}
        }
    }
}

fn key_to_bytes(key: &Key) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Character(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

fn spawn_waker(proxy: EventLoopProxy<()>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(16));
        if proxy.send_event(()).is_err() {
            break;
        }
    });
}
