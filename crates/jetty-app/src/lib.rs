mod app;
mod config;
pub mod clipboard;
pub mod input;

use app::AppEvent;
use winit::event_loop::{ControlFlow, EventLoop};

/// Unix-socket path used for single-instance IPC. Any running primary Jetty
/// instance listens here; secondary invocations (including `jetty --toggle`)
/// connect and send a toggle message, then exit immediately.
///
/// When `XDG_RUNTIME_DIR` is set (the normal case on systemd/logind systems),
/// that directory is already per-user, so a plain `jetty.sock` name is fine.
/// When it is unset, we fall back to `/tmp` but UID-namespace the filename via
/// `$USER` so two users can't collide or hijack each other's socket.
fn ipc_socket_path() -> String {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return format!("{runtime_dir}/jetty.sock");
    }
    // XDG_RUNTIME_DIR unset: namespace by $USER (or fall back to the raw UID
    // read from /proc/self/loginuid so the name is still unique per user even
    // without a $USER env var, and without requiring any libc dependency).
    let user_tag = std::env::var("USER").unwrap_or_else(|_| {
        std::fs::read_to_string("/proc/self/loginuid")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    });
    format!("/tmp/jetty-{user_tag}.sock")
}

/// Outcome of an IPC connect attempt.
enum ConnectResult {
    /// Connected to a live primary; message was sent. This process should exit.
    Forwarded,
    /// No socket file exists (first launch).
    NoSocket,
    /// A socket file exists but `connect` returned `ECONNREFUSED` — it is a
    /// stale leftover from a previous crash. Safe to unlink and rebind.
    Stale,
    /// Some other error (e.g. permission denied). Treated as "no live instance"
    /// so we attempt to become the primary without removing anything.
    Other,
}

/// Connect to a live Jetty instance and forward a summon command (`toggle`,
/// `show`, or `hide`). Returns the outcome so the caller can decide whether to
/// unlink a stale socket or become the primary.
fn forward_command(sock_path: &str, cmd: &str) -> ConnectResult {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    match UnixStream::connect(sock_path) {
        Ok(mut stream) => {
            let _ = stream.write_all(cmd.as_bytes());
            let _ = stream.flush();
            ConnectResult::Forwarded
        }
        Err(e) => {
            // ECONNREFUSED: socket file exists but nobody is listening — stale.
            if e.raw_os_error() == Some(libc_econnrefused()) {
                ConnectResult::Stale
            } else if e.kind() == std::io::ErrorKind::NotFound {
                ConnectResult::NoSocket
            } else {
                ConnectResult::Other
            }
        }
    }
}

/// Returns the `ECONNREFUSED` errno value portably without a libc dependency.
/// On Linux/macOS/BSDs this is always 111 (Linux) or 61 (macOS). We read it
/// from a refused loopback connect at start-up … but that adds latency and a
/// syscall. Instead, rely on the OS constant directly: POSIX guarantees the
/// value is defined; we hard-code the Linux and macOS values and fall back to
/// 0 (which means the stale-socket heuristic is conservatively disabled) for
/// any other host OS.
#[inline]
fn libc_econnrefused() -> i32 {
    #[cfg(target_os = "linux")]   { 111 }
    #[cfg(target_os = "macos")]   { 61  }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))] { 0 }
}

pub fn run() {
    // CLI: --version/--help print and exit; --toggle/--show/--hide select the
    // summon command forwarded to a running instance. The compositor-bound
    // `jetty --toggle` is the cross-platform summon path — every X11/Wayland
    // compositor, no portal or DE-specific code.
    let version = env!("CARGO_PKG_VERSION");
    let build = option_env!("JETTY_BUILD").unwrap_or("dev");
    let mut cmd = "toggle";
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-version" | "-V" | "version" => {
                println!("jetty {version} ({build})");
                std::process::exit(0);
            }
            "--help" | "-help" | "-h" | "help" => {
                println!(
                    "JeTTY {version} — a blazing-fast GPU terminal with a global summon hotkey.\n\n\
                     USAGE:\n    jetty [FLAGS]\n\n\
                     FLAGS:\n\
                     \x20   --toggle     Show/hide a running instance (or launch one); same as plain `jetty`.\n\
                     \x20   --show       Summon a running instance (or launch one).\n\
                     \x20   --hide       Hide a running instance.\n\
                     \x20   --version    Print version and exit.\n\
                     \x20   --help       Print this help and exit.\n\n\
                     Bind `jetty --toggle` to a key in your compositor to summon from anywhere.\n\
                     Settings: Ctrl+Shift+P. Config: ~/.config/jetty/config.toml"
                );
                std::process::exit(0);
            }
            "--toggle" => cmd = "toggle",
            "--show" => cmd = "show",
            "--hide" => cmd = "hide",
            _ => {}
        }
    }

    let sock_path = ipc_socket_path();

    // Secondary invocation: forward the command to the running primary and exit.
    // No banner, no GUI setup — a compositor-bound keypress stays instant.
    // TOCTOU-safe: only unlink the socket on ECONNREFUSED (provably stale), never
    // when a live primary owns it (which would race two concurrent cold starts).
    match forward_command(&sock_path, cmd) {
        ConnectResult::Forwarded => return,
        ConnectResult::Stale => {
            std::fs::remove_file(&sock_path).ok();
        }
        ConnectResult::NoSocket | ConnectResult::Other => {}
    }
    // No live instance: `--hide` has nothing to hide; toggle/show launch.
    if cmd == "hide" {
        return;
    }

    eprintln!("jetty {version} ({build})");

    // No live instance found — become the primary. Bind directly (stale file
    // was already removed above when detected; NoSocket/Other means no file or
    // we conservatively skip the removal and let bind fail gracefully).
    let listener: Option<std::os::unix::net::UnixListener> =
        match std::os::unix::net::UnixListener::bind(&sock_path) {
            Ok(l) => {
                eprintln!("jetty: IPC socket bound at {sock_path}");
                Some(l)
            }
            Err(e) => {
                eprintln!("jetty: could not bind IPC socket at {sock_path}: {e} — single-instance IPC disabled");
                None
            }
        };

    let event_loop = EventLoop::<AppEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    // IPC accept thread (primary only): map each forwarded command to an event.
    // `show`/`hide` set visibility explicitly; anything else toggles. Shares the
    // summon code path with the X11 global-hotkey grab.
    if let Some(listener) = listener {
        let proxy_ipc = proxy.clone();
        let sock_cleanup = sock_path.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut s) = stream {
                    let mut buf = [0u8; 16];
                    let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
                    let event = match &buf[..n] {
                        b"show" => AppEvent::SetVisible(true),
                        b"hide" => AppEvent::SetVisible(false),
                        _ => AppEvent::ToggleVisibility,
                    };
                    if proxy_ipc.send_event(event).is_err() {
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&sock_cleanup);
        });
    }

    let mut app = app::App::new(proxy);
    event_loop.run_app(&mut app).expect("run_app");

    // Best-effort cleanup on normal exit. Crashes are handled by the
    // remove-stale-on-bind logic at the start of the next launch.
    let _ = std::fs::remove_file(&sock_path);
}
