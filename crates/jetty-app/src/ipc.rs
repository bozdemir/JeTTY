//! What a launch and the running JeTTY say after the summon verb.
//!
//! A launch forwards its verb (`toggle`, `show`, `hide`, `background`) to the
//! primary byte for byte as it always has — so a JeTTY older than this exchange
//! still acts on it — and then the two introduce themselves:
//!
//! 1. launch → the verb alone (an older primary acts on it and hangs up);
//! 2. primary → `jetty <version>\n`;
//! 3. launch → who it is ([`Caller`]: version, display, AppImage), then EOF;
//! 4. primary → `ok\n`, and acts on the verb — or `elsewhere\n` when the launch
//!    runs on another display (`ssh -X`, a second X session), which then starts
//!    or reaches that display's own JeTTY instead of toggling this one.
//!
//! An older launch hangs up after step 1 and the primary serves the verb as
//! before; `echo toggle | nc -U` says nothing at step 3 and is served once the
//! read times out. A launch that hears nothing back knows the primary predates
//! this exchange — older than itself.

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Where a JeTTY draws: `$WAYLAND_DISPLAY` and `$DISPLAY` (its screen number
/// dropped, `:0.0` → `:0`) as the process got them; empty when unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Display {
    pub wayland: String,
    pub x11: String,
}

impl Display {
    pub fn current() -> Display {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Display::new(&var("WAYLAND_DISPLAY"), &var("DISPLAY"))
    }

    fn new(wayland: &str, x11: &str) -> Display {
        // `host:display.screen`: every screen of one X server is one desktop.
        let x11 = match x11.rfind(':') {
            Some(colon) => match x11[colon..].find('.') {
                Some(dot) => &x11[..colon + dot],
                None => x11,
            },
            None => x11,
        };
        Display { wayland: wayland.to_string(), x11: x11.to_string() }
    }

    /// Is a launch on `self` on the desktop of a JeTTY on `primary`? The same
    /// Wayland or the same X display either way (a JeTTY started with
    /// `WAYLAND_DISPLAY` unset still answers its session's `jetty --toggle`);
    /// a launch with no display at all (a console, plain `ssh`) controls the
    /// running JeTTY as it always did.
    pub fn same_desktop(&self, primary: &Display) -> bool {
        (self.wayland.is_empty() && self.x11.is_empty())
            || (!self.wayland.is_empty() && self.wayland == primary.wayland)
            || (!self.x11.is_empty() && self.x11 == primary.x11)
    }
}

/// Who forwarded a verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub version: String,
    pub display: Display,
    /// The AppImage file it runs from, if it is one.
    pub appimage: Option<PathBuf>,
}

impl Caller {
    /// This process.
    pub fn current() -> Caller {
        Caller {
            version: env!("CARGO_PKG_VERSION").to_string(),
            display: Display::current(),
            appimage: jetty_core::self_exe().filter(|e| e.appimage).map(|e| e.path),
        }
    }

    /// The wire form: NUL-separated fields (a path holds anything but NUL).
    fn encode(&self) -> Vec<u8> {
        let mut out = b"v1".to_vec();
        for field in [self.version.as_bytes(), self.display.wayland.as_bytes(), self.display.x11.as_bytes()] {
            out.push(0);
            out.extend_from_slice(field);
        }
        out.push(0);
        if let Some(p) = &self.appimage {
            out.extend_from_slice(p.as_os_str().as_bytes());
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Caller> {
        let mut fields = bytes.split(|&b| b == 0);
        if fields.next()? != b"v1" {
            return None;
        }
        let mut text = || fields.next().map(|f| String::from_utf8_lossy(f).into_owned());
        let (version, wayland, x11) = (text()?, text()?, text()?);
        let appimage = fields
            .next()
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)));
        Some(Caller { version, display: Display { wayland, x11 }, appimage })
    }

    /// Is this caller a newer JeTTY than `version`?
    pub fn newer_than(&self, version: &str) -> bool {
        match (release(&self.version), release(version)) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        }
    }
}

/// `major.minor.patch` of a version (anything after a `-` or `+` ignored).
fn release(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((n.next()??, n.next()??, n.next().unwrap_or(Some(0))?))
}

/// The most a caller's introduction may take (a path is at most 4 KiB).
const CALLER_MAX: u64 = 8 * 1024;

/// How the primary took a forwarded verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// It acted on it (or said nothing we understood after its hello).
    Served,
    /// It runs on another display and left the verb alone.
    Elsewhere,
    /// It hung up without a hello: a JeTTY older than this exchange, which
    /// acted on the verb.
    Older,
    /// No answer in time — whatever it did is unknown.
    Silent,
}

/// The launch side, after the verb was written to `stream` (which has a read
/// timeout): read the primary's hello, introduce `me`, read its answer.
pub(crate) fn introduce(stream: &mut UnixStream, me: &Caller) -> Answer {
    let hello = match read_line(stream) {
        Ok(line) if line.is_empty() => return Answer::Older,
        Ok(line) => line,
        Err(_) => return Answer::Silent,
    };
    if !hello.starts_with("jetty ") {
        return Answer::Served;
    }
    if stream.write_all(&me.encode()).is_err() || stream.shutdown(std::net::Shutdown::Write).is_err() {
        return Answer::Silent;
    }
    match read_line(stream) {
        Ok(answer) if answer == "elsewhere" => Answer::Elsewhere,
        Ok(_) => Answer::Served,
        Err(_) => Answer::Silent,
    }
}

/// The primary side, after reading a verb from `stream` (which has a read
/// timeout): say hello as `version`, hear who called, and answer whether this
/// JeTTY (on `here`) serves the verb. Returns the caller — `None` for an older
/// launch or a bare `nc` — and that decision.
pub(crate) fn greet(stream: &mut UnixStream, version: &str, here: &Display) -> (Option<Caller>, bool) {
    // An older launch has hung up already: the write may fail, and the read
    // below then sees EOF. Either way the verb is served as before.
    let _ = stream.write_all(format!("jetty {version}\n").as_bytes());
    let mut intro = Vec::new();
    let _ = Read::take(&mut *stream, CALLER_MAX).read_to_end(&mut intro);
    let caller = Caller::decode(&intro);
    let serve = caller.as_ref().is_none_or(|c| c.display.same_desktop(here));
    let _ = stream.write_all(if serve { b"ok\n" } else { b"elsewhere\n" });
    (caller, serve)
}

/// One `\n`-terminated line (at most 64 bytes), without the newline; empty at
/// EOF.
fn read_line(stream: &mut UnixStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < 64 {
        match stream.read(&mut byte)? {
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ => line.push(byte[0]),
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn caller(version: &str, display: Display, appimage: Option<&str>) -> Caller {
        Caller { version: version.to_string(), display, appimage: appimage.map(PathBuf::from) }
    }

    fn pair() -> (UnixStream, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        for s in [&a, &b] {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (a, b)
    }

    #[test]
    fn a_display_is_its_server_not_its_screen() {
        assert_eq!(Display::new("", ":0.0").x11, ":0");
        assert_eq!(Display::new("", "localhost:10.0").x11, "localhost:10");
        assert_eq!(Display::new("", ":1").x11, ":1");
        assert_eq!(Display::new("wayland-0", "").wayland, "wayland-0");
    }

    #[test]
    fn launches_on_another_display_are_elsewhere() {
        let desk = Display::new("wayland-0", ":0");
        // The session's own launches, by either variable.
        assert!(Display::new("wayland-0", ":0").same_desktop(&desk));
        assert!(Display::new("", ":0.0").same_desktop(&desk));
        assert!(Display::new("wayland-0", "").same_desktop(&desk));
        // A JeTTY started with WAYLAND_DISPLAY unset (X11 under XWayland).
        assert!(Display::new("wayland-0", ":0").same_desktop(&Display::new("", ":0")));
        // No display at all: a console or plain ssh still controls it.
        assert!(Display::new("", "").same_desktop(&desk));
        // ssh -X, a second X session, another Wayland session.
        assert!(!Display::new("", "localhost:10.0").same_desktop(&desk));
        assert!(!Display::new("", ":1").same_desktop(&desk));
        assert!(!Display::new("wayland-1", ":1").same_desktop(&desk));
    }

    #[test]
    fn a_caller_survives_the_wire() {
        for c in [
            caller("0.30.0", Display::new("wayland-0", ":0"), Some("/home/u/Apps/JeTTY-0.30.0-x86_64.AppImage")),
            caller("0.30.0", Display::default(), None),
            caller("1.2.3", Display::new("", ":1"), Some("/odd dir/\n\t\u{e9}.AppImage")),
        ] {
            assert_eq!(Caller::decode(&c.encode()), Some(c));
        }
        // A path is bytes, not UTF-8.
        let raw = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff.AppImage"));
        let c = Caller { appimage: Some(raw), ..caller("0.30.0", Display::default(), None) };
        assert_eq!(Caller::decode(&c.encode()), Some(c));
        assert_eq!(Caller::decode(b""), None);
        assert_eq!(Caller::decode(b"v9\0x"), None);
        assert_eq!(Caller::decode(b"v1\x000.30.0"), None, "fields missing");
    }

    #[test]
    fn only_a_newer_release_is_newer() {
        let c = |v: &str| caller(v, Display::default(), None);
        assert!(c("0.30.0").newer_than("0.29.1"));
        assert!(c("0.30.1").newer_than("0.30.0"));
        assert!(c("1.0.0").newer_than("0.99.9"));
        assert!(c("0.10.0").newer_than("0.9.0"), "numbers, not strings");
        assert!(!c("0.30.0").newer_than("0.30.0"));
        assert!(!c("0.29.1").newer_than("0.30.0"));
        assert!(!c("0.30.0-rc1").newer_than("0.30.0"));
        assert!(!c("dev").newer_than("0.30.0"));
        assert!(!c("0.31.0").newer_than("garbage"));
    }

    #[test]
    fn the_exchange_serves_this_desktop_and_turns_others_away() {
        let desk = Display::new("", ":0");
        for (display, want) in [(Display::new("", ":0"), Answer::Served), (Display::new("", ":5"), Answer::Elsewhere)] {
            let (mut launch, mut primary) = pair();
            let me = caller("0.31.0", display, Some("/a/JeTTY.AppImage"));
            let sent = me.clone();
            let t = std::thread::spawn(move || {
                launch.write_all(b"toggle").unwrap();
                introduce(&mut launch, &sent)
            });
            let mut verb = [0u8; 16];
            let n = primary.read(&mut verb).unwrap();
            assert_eq!(&verb[..n], b"toggle", "the verb comes alone, as an older primary expects");
            let (got, serve) = greet(&mut primary, "0.30.0", &desk);
            assert_eq!(got.as_ref(), Some(&me));
            assert_eq!(serve, want == Answer::Served);
            drop(primary);
            assert_eq!(t.join().unwrap(), want);
        }
    }

    #[test]
    fn an_older_primary_or_launch_still_works() {
        // An older primary reads the verb and hangs up: the launch knows.
        let (mut launch, mut primary) = pair();
        launch.write_all(b"show").unwrap();
        let mut verb = [0u8; 16];
        assert_eq!(primary.read(&mut verb).unwrap(), 4);
        drop(primary);
        assert_eq!(introduce(&mut launch, &Caller::current()), Answer::Older);
        // An older launch writes the verb and exits: served, no caller.
        let (mut launch, mut primary) = pair();
        launch.write_all(b"toggle").unwrap();
        drop(launch);
        let n = primary.read(&mut verb).unwrap();
        assert_eq!(&verb[..n], b"toggle");
        assert_eq!(greet(&mut primary, "0.30.0", &Display::new("", ":0")), (None, true));
    }
}
