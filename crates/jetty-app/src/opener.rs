//! Opening a link (a Ctrl+click, a hint's Alt) with the platform opener:
//! `xdg-open` (OS-level, never DE-specific) or macOS's `open`.
//!
//! The opener starts with a clean environment — none of the variables of
//! JeTTY's own launch the shells never see either (a launch token addressed
//! to JeTTY's first window, the AppImage runtime's) — from `/`, and on X11 /
//! Wayland with a FRESH activation token for the click: the compositor's
//! focus-stealing prevention then lets the browser come to the front instead
//! of opening it behind JeTTY (Wayland gives a program no other way to).

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

/// How long a link waits for its activation token before it opens without
/// one: the compositor answers within a round trip, and a lost answer must
/// not lose the click.
const TOKEN_WAIT: Duration = Duration::from_millis(500);

/// Links waiting for the activation token winit was asked for.
#[derive(Default)]
pub struct Opener {
    waiting: Vec<Waiting>,
}

struct Waiting {
    serial: winit::event_loop::AsyncRequestSerial,
    token: mpsc::Sender<String>,
    since: Instant,
}

impl Opener {
    /// Open `url` for a click in `window`, off the UI thread: the opener
    /// starts once its token is in (or after [`TOKEN_WAIT`]) and is reaped
    /// there, so it never zombies.
    pub fn open(&mut self, window: &winit::window::Window, url: &str) {
        self.waiting.retain(|w| w.since.elapsed() < TOKEN_WAIT);
        let token = self.request_token(window);
        let url = url.to_owned();
        let started = std::thread::Builder::new().name("jetty-open".into()).spawn(move || {
            let token = token.and_then(|t| t.recv_timeout(TOKEN_WAIT).ok());
            match command(&url, token.as_deref()).spawn() {
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(e) => eprintln!("jetty: failed to spawn {OPENER} for URL: {e}"),
            }
        });
        if let Err(e) = started {
            eprintln!("jetty: failed to spawn {OPENER} for URL: {e}");
        }
    }

    /// The activation token asked for as `serial` arrived
    /// (`WindowEvent::ActivationTokenDone`, for whichever window asked).
    pub fn token_done(&mut self, serial: winit::event_loop::AsyncRequestSerial, token: winit::window::ActivationToken) {
        if let Some(i) = self.waiting.iter().position(|w| w.serial == serial) {
            let _ = self.waiting.swap_remove(i).token.send(token.into_raw());
        }
    }

    /// Ask winit for a token for a launch from `window` (X11 startup
    /// notification, Wayland xdg-activation); `None` where there is none to
    /// ask for (macOS: `open` brings the app forward itself; a compositor
    /// without xdg-activation).
    fn request_token(&mut self, window: &winit::window::Window) -> Option<mpsc::Receiver<String>> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use winit::platform::startup_notify::WindowExtStartupNotify;
            let serial = window.request_activation_token().ok()?;
            let (token, rx) = mpsc::channel();
            self.waiting.push(Waiting { serial, token, since: Instant::now() });
            Some(rx)
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = window;
            None
        }
    }
}

/// The opener's command for `url`, with the activation `token` of the click
/// when there is one — under both names, as winit's `set_activation_token_env`
/// sets it: `XDG_ACTIVATION_TOKEN` (Wayland), `DESKTOP_STARTUP_ID` (X11).
fn command(url: &str, token: Option<&str>) -> Command {
    let mut cmd = Command::new(OPENER);
    cmd.arg(url).current_dir("/").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for key in jetty_core::uninherited_env() {
        cmd.env_remove(key);
    }
    if let Some(token) = token {
        cmd.env("XDG_ACTIVATION_TOKEN", token).env("DESKTOP_STARTUP_ID", token);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::command;
    use std::ffi::OsStr;

    /// What `cmd` does to `var`: `None` = inherited as is, `Some(None)` =
    /// removed, `Some(Some(v))` = set to `v`.
    fn env_of<'a>(cmd: &'a std::process::Command, var: &str) -> Option<Option<&'a OsStr>> {
        cmd.get_envs().find(|(k, _)| *k == OsStr::new(var)).map(|(_, v)| v)
    }

    #[test]
    fn the_opener_never_inherits_jettys_launch_variables() {
        let cmd = command("https://example.com", None);
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), [OsStr::new("https://example.com")]);
        // A token addressed to JeTTY's first window, the AppImage runtime's own
        // variables: removed, like from the shells.
        for var in ["DESKTOP_STARTUP_ID", "XDG_ACTIVATION_TOKEN", "APPIMAGE", "APPDIR", "ARGV0", "OWD"] {
            assert_eq!(env_of(&cmd, var), Some(None), "{var}");
        }
        assert_eq!(env_of(&cmd, "PATH"), None, "the rest is inherited");
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
    }

    #[test]
    fn the_opener_gets_the_fresh_token_of_the_click() {
        let cmd = command("https://example.com", Some("tok-123"));
        assert_eq!(env_of(&cmd, "XDG_ACTIVATION_TOKEN"), Some(Some(OsStr::new("tok-123"))));
        assert_eq!(env_of(&cmd, "DESKTOP_STARTUP_ID"), Some(Some(OsStr::new("tok-123"))));
        assert_eq!(env_of(&cmd, "APPIMAGE"), Some(None));
    }
}
