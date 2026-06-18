// Diagnostic: a winit-only window (NO GPU) that logs exactly what winit reports
// for each key event. Runs fine in Xephyr (no presentation needed) so we can see
// how this system delivers Ctrl+, / Ctrl+Shift+T etc. — independent of layout.
//
// Run:  DISPLAY=:99 cargo run -p jetty-app --bin keyprobe
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

struct Probe {
    window: Option<Arc<Window>>,
    mods: ModifiersState,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            let w = el
                .create_window(Window::default_attributes().with_title("Jetty"))
                .unwrap();
            self.window = Some(Arc::new(w));
            eprintln!("PROBE: window created, ready for input");
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, e: WindowEvent) {
        match e {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                eprintln!(
                    "MODS  ctrl={} shift={} alt={} super={}",
                    self.mods.control_key(),
                    self.mods.shift_key(),
                    self.mods.alt_key(),
                    self.mods.super_key()
                );
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                eprintln!(
                    "KEY   physical={:?}  logical={:?}  key_no_mods={:?}  text={:?}  | mods ctrl={} shift={}",
                    event.physical_key,
                    event.logical_key,
                    event.key_without_modifiers(),
                    event.text,
                    self.mods.control_key(),
                    self.mods.shift_key()
                );
            }
            _ => {}
        }
    }
}

fn main() {
    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Wait);
    let mut p = Probe { window: None, mods: ModifiersState::empty() };
    el.run_app(&mut p).unwrap();
}
