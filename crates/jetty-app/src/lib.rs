mod app;
pub mod input;

use winit::event_loop::{ControlFlow, EventLoop};

pub fn run() {
    // One-line build banner so a user can confirm which build they are running.
    // JETTY_BUILD is injected at compile time (e.g. the git short SHA via
    // `JETTY_BUILD=$(git rev-parse --short HEAD) cargo build`); falls back to
    // "dev" for plain local builds.
    eprintln!(
        "jetty {} ({})",
        env!("CARGO_PKG_VERSION"),
        option_env!("JETTY_BUILD").unwrap_or("dev")
    );

    let event_loop = EventLoop::<()>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = app::App::new(proxy);
    event_loop.run_app(&mut app).expect("run_app");
}
