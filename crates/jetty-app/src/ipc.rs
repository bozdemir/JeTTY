//! What a launch and the running JeTTY say after the summon verb.
//!
//! A launch forwards its verb (`toggle`, `show`, `hide`, `background`,
//! `new-tab`) to the primary byte for byte as it always has — so a JeTTY older
//! than this exchange still acts on it — and then the two introduce themselves:
//!
//! 1. launch → the verb alone (an older primary acts on it and hangs up);
//! 2. primary → `jetty <version>\n`;
//! 3. launch → who it is ([`Caller`]: version, display, AppImage, the
//!    activation token it was launched with) — and for `new-tab` the tab it
//!    asks for ([`TabRequest`]) — then EOF;
//! 4. primary → `ok\n`, and acts on the verb — or `elsewhere\n` when the launch
//!    runs on another display (`ssh -X`, a second X session), which then starts
//!    or reaches that display's own JeTTY instead of toggling this one; a tab
//!    it can't open is `refused <why>\n`.
//!
//! An older launch hangs up after step 1 and the primary serves the verb as
//! before; `echo toggle | nc -U` says nothing at step 3 and is served once the
//! read times out. A launch that hears nothing back knows the primary predates
//! this exchange — older than itself. A primary older than `new-tab` hangs up
//! on it unanswered, having done nothing (unknown verbs are no-ops).

use std::ffi::{OsStr, OsString};
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
    /// The xdg-activation token it was launched with (`XDG_ACTIVATION_TOKEN`:
    /// a compositor shortcut, an app menu on Wayland). A Wayland summon
    /// builds the window with it, so a compositor that focuses a new window
    /// only for a token focuses this one (`AppEvent::ActivationToken`).
    pub activation_token: Option<String>,
    /// The tab a `new-tab` launch asks for.
    pub tab: Option<TabRequest>,
}

impl Caller {
    /// This process.
    pub fn current() -> Caller {
        Caller {
            version: env!("CARGO_PKG_VERSION").to_string(),
            display: Display::current(),
            appimage: jetty_core::self_exe().filter(|e| e.appimage).map(|e| e.path),
            activation_token: std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| activation_token_ok(t)),
            tab: None,
        }
    }

    /// The wire form: NUL-separated fields (a path holds anything but NUL) —
    /// `v1`, version, Wayland and X11 display, AppImage, activation token —
    /// then, for `new-tab`, [`TAB_MARK`], the tab's directory and each of
    /// its command's arguments. A field a later JeTTY adds goes before the
    /// tab, which is found by its marker.
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
        out.push(0);
        if let Some(t) = &self.activation_token {
            out.extend_from_slice(t.as_bytes());
        }
        if let Some(tab) = &self.tab {
            out.push(0);
            out.extend_from_slice(TAB_MARK);
            out.push(0);
            if let Some(dir) = &tab.cwd {
                out.extend_from_slice(dir.as_os_str().as_bytes());
            }
            for arg in &tab.argv {
                out.push(0);
                out.extend_from_slice(arg.as_bytes());
            }
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
            .map(|p| PathBuf::from(OsStr::from_bytes(p)));
        let activation_token = fields
            .next()
            .and_then(|t| std::str::from_utf8(t).ok())
            .filter(|t| activation_token_ok(t))
            .map(str::to_owned);
        let tab = fields.find(|f| *f == TAB_MARK).map(|_| TabRequest {
            cwd: fields.next().filter(|d| !d.is_empty()).map(|d| PathBuf::from(OsStr::from_bytes(d))),
            argv: fields.map(|a| OsStr::from_bytes(a).to_os_string()).collect(),
        });
        Some(Caller { version, display: Display { wayland, x11 }, appimage, activation_token, tab })
    }

    /// Is this caller a newer JeTTY than `version`?
    pub fn newer_than(&self, version: &str) -> bool {
        match (release(&self.version), release(version)) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        }
    }
}

/// Whether `t` can be an activation token: printable ASCII, no spaces, short
/// (KWin's are UUIDs). Anything else is dropped, never handed on.
fn activation_token_ok(t: &str) -> bool {
    !t.is_empty() && t.len() <= 512 && t.bytes().all(|b| b.is_ascii_graphic())
}

/// `major.minor.patch` of a version (anything after a `-` or `+` ignored).
fn release(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((n.next()??, n.next()??, n.next().unwrap_or(Some(0))?))
}

/// A tab a launch asks the primary for (`jetty --new-tab`, `-e`): where it
/// starts and what runs in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TabRequest {
    /// The directory it starts in (absolute); `None`: where a new tab starts
    /// by default.
    pub cwd: Option<PathBuf>,
    /// The program and its arguments, exec'd as they are — no shell ever
    /// parses them; empty: the configured shell.
    pub argv: Vec<OsString>,
}

impl TabRequest {
    /// The bytes it takes on the wire after [`TAB_MARK`].
    fn wire_len(&self) -> usize {
        let dir = self.cwd.as_ref().map_or(0, |d| d.as_os_str().len());
        1 + dir + self.argv.iter().map(|a| 1 + a.len()).sum::<usize>()
    }

    /// Whether this tab can be opened: in an absolute directory that is
    /// there, a command with a name, short enough for the wire ([`TAB_MAX`]).
    /// The launch checks before it asks; the primary again, as it sees the
    /// filesystem. The error says why not.
    pub fn check(&self) -> Result<(), String> {
        if let Some(dir) = self.cwd.as_ref().filter(|d| !d.is_absolute() || !d.is_dir()) {
            return Err(format!("{}: not a directory", dir.display()));
        }
        if self.argv.first().is_some_and(|p| p.is_empty()) {
            return Err("the command to run is empty".to_string());
        }
        if self.wire_len() > TAB_MAX {
            let kib = |n: usize| n.div_ceil(1024);
            return Err(format!("the command is too long ({} KiB; at most {} KiB)", kib(self.wire_len()), kib(TAB_MAX)));
        }
        Ok(())
    }
}

/// Where a tab request starts in a launch's introduction ([`Caller::encode`]).
const TAB_MARK: &[u8] = b"tab1";

/// The most a tab request may take on the wire ([`TabRequest::check`]).
pub(crate) const TAB_MAX: usize = 256 * 1024;

/// The most a caller's introduction may take: its own fields (a path is at
/// most 4 KiB) and a tab request.
const INTRO_MAX: u64 = 8 * 1024 + TAB_MAX as u64;

/// The longest line either side reads (a hello, an answer).
const LINE_MAX: usize = 256;

/// How the primary took a forwarded verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    /// It acted on it (or said nothing we understood after its hello).
    Served,
    /// It runs on another display and left the verb alone.
    Elsewhere,
    /// It could not open the tab asked for, for this reason.
    Refused(String),
    /// It hung up without a hello: a JeTTY older than this exchange, which
    /// acted on the verb — or one older than `new-tab`, which did nothing.
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
        Ok(answer) => match answer.strip_prefix("refused") {
            Some(why) => Answer::Refused(why.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()),
            None => Answer::Served,
        },
        Err(_) => Answer::Silent,
    }
}

/// The primary side, after reading a verb from `stream` (which has a read
/// timeout): say hello as `version`, hear who called, and answer whether this
/// JeTTY (on `here`) serves the verb. Returns the caller — `None` for an older
/// launch or a bare `nc` — and that decision.
pub(crate) fn greet(stream: &mut UnixStream, version: &str, here: &Display) -> (Option<Caller>, bool) {
    let caller = hear(stream, version);
    let serve = caller.as_ref().is_none_or(|c| c.display.same_desktop(here));
    let _ = stream.write_all(if serve { b"ok\n" } else { b"elsewhere\n" });
    (caller, serve)
}

/// [`greet`] for `new-tab`, up to the answer: the caller and the tab it asks
/// for, checked here ([`TabRequest::check`]) — answered with [`answer_tab`]
/// once it opened or not. `None` for the tab: answered already — elsewhere,
/// or refused (cut short, unreadable, no such directory here). A bare
/// `echo new-tab | nc -U` asks for a new tab where one starts by default.
pub(crate) fn hear_tab(stream: &mut UnixStream, version: &str, here: &Display) -> (Option<Caller>, Option<TabRequest>) {
    let mut caller = hear(stream, version);
    let tab = match caller.as_mut() {
        None => Ok(TabRequest::default()),
        Some(c) if !c.display.same_desktop(here) => {
            let _ = stream.write_all(b"elsewhere\n");
            return (caller, None);
        }
        Some(c) => c.tab.take().ok_or_else(|| "the tab request did not arrive whole".to_string()),
    };
    match tab.and_then(|t| t.check().map(|()| t)) {
        Ok(t) => (caller, Some(t)),
        Err(why) => {
            refuse(stream, &why);
            (caller, None)
        }
    }
}

/// The answer to a `new-tab` ([`hear_tab`]): `ok`, or why the tab could not
/// be opened.
pub(crate) fn answer_tab(stream: &mut UnixStream, opened: Result<(), String>) {
    match opened {
        Ok(()) => {
            let _ = stream.write_all(b"ok\n");
        }
        Err(why) => refuse(stream, &why),
    }
}

/// Say hello as `version` and hear who called (steps 2 and 3). A tab request
/// is taken only from an introduction that arrived whole — EOF within
/// [`INTRO_MAX`]: a command cut short is never run.
fn hear(stream: &mut UnixStream, version: &str) -> Option<Caller> {
    // An older launch has hung up already: the write may fail, and the read
    // below then sees EOF. Either way the verb is served as before.
    let _ = stream.write_all(format!("jetty {version}\n").as_bytes());
    let mut intro = Vec::new();
    let read = Read::take(&mut *stream, INTRO_MAX + 1).read_to_end(&mut intro);
    let whole = read.is_ok() && intro.len() as u64 <= INTRO_MAX;
    let mut caller = Caller::decode(&intro);
    if let Some(c) = caller.as_mut().filter(|_| !whole) {
        c.tab = None;
    }
    caller
}

/// `refused <why>\n`: one line of text, at most [`LINE_MAX`] bytes.
fn refuse(stream: &mut UnixStream, why: &str) {
    let mut line = String::from("refused ");
    for c in why.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if line.len() + c.len_utf8() >= LINE_MAX {
            break;
        }
        line.push(c);
    }
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

/// One `\n`-terminated line (at most [`LINE_MAX`] bytes), without the
/// newline; empty at EOF.
fn read_line(stream: &mut UnixStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < LINE_MAX {
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
        Caller {
            version: version.to_string(),
            display,
            appimage: appimage.map(PathBuf::from),
            activation_token: None,
            tab: None,
        }
    }

    fn tab(cwd: Option<&str>, argv: &[&str]) -> TabRequest {
        TabRequest { cwd: cwd.map(PathBuf::from), argv: argv.iter().map(OsString::from).collect() }
    }

    /// A `new-tab` launch's side of the exchange on `launch`: the verb, then
    /// [`introduce`] as `me`.
    fn ask_tab(mut launch: UnixStream, me: Caller) -> std::thread::JoinHandle<Answer> {
        std::thread::spawn(move || {
            launch.write_all(b"new-tab").unwrap();
            introduce(&mut launch, &me)
        })
    }

    /// The primary's side up to the answer: the verb, then [`hear_tab`].
    fn hear_tab_on(primary: &mut UnixStream, here: &Display) -> Option<TabRequest> {
        let mut verb = [0u8; 16];
        let n = primary.read(&mut verb).unwrap();
        assert_eq!(&verb[..n], b"new-tab", "the verb comes alone");
        hear_tab(primary, "0.31.0", here).1
    }

    #[test]
    fn a_tab_request_survives_the_wire() {
        let tmp = std::env::temp_dir();
        let tmp = tmp.to_str().unwrap();
        for t in [
            tab(Some(tmp), &["htop"]),
            tab(Some(tmp), &[]),
            tab(None, &[]),
            tab(Some("/odd dir/\n"), &["vim", "a b", "", "$(rm -rf ~);x", "-e", "--", "\u{e9}"]),
        ] {
            let with = Caller { tab: Some(t.clone()), ..caller("0.31.0", Display::new("", ":0"), Some("/a/J.AppImage")) };
            assert_eq!(Caller::decode(&with.encode()), Some(with.clone()), "{t:?}");
            let token = Caller { activation_token: Some("tok-1".into()), ..with };
            assert_eq!(Caller::decode(&token.encode()), Some(token), "{t:?}");
        }
        // Arguments and paths are bytes, not UTF-8.
        let raw = TabRequest {
            cwd: Some(PathBuf::from(OsStr::from_bytes(b"/tmp/\xff"))),
            argv: vec![OsStr::from_bytes(b"\xfe").to_os_string()],
        };
        let c = Caller { tab: Some(raw), ..caller("0.31.0", Display::default(), None) };
        assert_eq!(Caller::decode(&c.encode()), Some(c.clone()));
        // A field a later JeTTY adds before the tab is skipped: the tab is
        // found by its marker.
        let plain = Caller { tab: None, ..c.clone() }.encode();
        let mut later = plain.clone();
        later.extend_from_slice(b"\0some-new-field\0");
        later.extend_from_slice(&c.encode()[plain.len() + 1..]);
        assert_eq!(Caller::decode(&later).and_then(|d| d.tab), c.tab);
        // A summon's introduction carries none.
        assert_eq!(Caller::decode(&plain).unwrap().tab, None);
    }

    #[test]
    fn a_tab_request_is_checked_before_it_is_opened() {
        let tmp = std::env::temp_dir();
        let dir = tmp.to_str().unwrap();
        assert_eq!(tab(Some(dir), &["htop", "-d", "10"]).check(), Ok(()));
        assert_eq!(tab(None, &[]).check(), Ok(()), "the shell, where a tab starts by default");
        let file = tmp.join(format!("jetty-ipc-file-{}", std::process::id()));
        std::fs::write(&file, b"").unwrap();
        for bad in [file.to_str().unwrap(), "/nonexistent/jetty-dir", "relative/dir", "."] {
            assert_eq!(tab(Some(bad), &[]).check(), Err(format!("{bad}: not a directory")));
        }
        let _ = std::fs::remove_file(&file);
        assert_eq!(tab(Some(dir), &["", "x"]).check(), Err("the command to run is empty".into()));
        let long = "x".repeat(TAB_MAX);
        assert!(tab(Some(dir), &[&long]).check().unwrap_err().contains("too long"));
        let fits = "x".repeat(TAB_MAX - 2 - dir.len());
        assert_eq!(tab(Some(dir), &[&fits]).check(), Ok(()), "the limit is the wire's");
    }

    #[test]
    fn a_new_tab_is_opened_or_the_launch_hears_why_not() {
        let here = Display::new("", ":0");
        let tmp = std::env::temp_dir();
        let want = tab(Some(tmp.to_str().unwrap()), &["htop", "-d", "10"]);
        let me = Caller { tab: Some(want.clone()), ..caller("0.31.0", Display::new("", ":0"), None) };
        // Opened.
        let (launch, mut primary) = pair();
        let t = ask_tab(launch, me.clone());
        assert_eq!(hear_tab_on(&mut primary, &here), Some(want.clone()));
        answer_tab(&mut primary, Ok(()));
        assert_eq!(t.join().unwrap(), Answer::Served);
        // The event loop could not start it.
        let (launch, mut primary) = pair();
        let t = ask_tab(launch, me.clone());
        assert!(hear_tab_on(&mut primary, &here).is_some());
        answer_tab(&mut primary, Err("\"htpo\": not found in PATH\nmore".into()));
        assert_eq!(t.join().unwrap(), Answer::Refused("\"htpo\": not found in PATH more".into()));
        // A directory the primary can't see is refused there.
        let (launch, mut primary) = pair();
        let gone = tab(Some("/nonexistent/jetty-dir"), &[]);
        let t = ask_tab(launch, Caller { tab: Some(gone), ..me.clone() });
        assert_eq!(hear_tab_on(&mut primary, &here), None);
        drop(primary);
        assert_eq!(t.join().unwrap(), Answer::Refused("/nonexistent/jetty-dir: not a directory".into()));
        // A launch on another display goes to its own JeTTY.
        let (launch, mut primary) = pair();
        let t = ask_tab(launch, Caller { display: Display::new("", ":5"), ..me.clone() });
        assert_eq!(hear_tab_on(&mut primary, &here), None);
        drop(primary);
        assert_eq!(t.join().unwrap(), Answer::Elsewhere);
        // `echo new-tab | nc -U`: no introduction, a tab with the defaults.
        let (mut launch, mut primary) = pair();
        launch.write_all(b"new-tab").unwrap();
        launch.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(hear_tab_on(&mut primary, &here), Some(TabRequest::default()));
        // A JeTTY older than `new-tab` hangs up on it, having done nothing.
        let (launch, mut primary) = pair();
        let t = ask_tab(launch, me);
        let mut verb = [0u8; 16];
        let _ = primary.read(&mut verb).unwrap();
        drop(primary);
        assert_eq!(t.join().unwrap(), Answer::Older);
    }

    #[test]
    fn a_tab_request_cut_short_is_never_opened() {
        let here = Display::new("", ":0");
        let me = Caller { tab: Some(tab(None, &["rm", "-r", "/tmp/jetty-x/build"])), ..caller("0.31.0", here.clone(), None) };
        let whole = me.encode();
        // The launch stalls mid-request (the read times out): refused.
        let (mut launch, mut primary) = pair();
        primary.set_read_timeout(Some(std::time::Duration::from_millis(100))).unwrap();
        launch.write_all(b"new-tab").unwrap();
        let mut verb = [0u8; 16];
        let _ = primary.read(&mut verb).unwrap();
        launch.write_all(&whole[..whole.len() - 6]).unwrap();
        assert_eq!(hear_tab(&mut primary, "0.31.0", &here).1, None);
        drop(launch);
        // More than an introduction may hold: refused, never truncated.
        let (mut launch, mut primary) = pair();
        launch.write_all(b"new-tab").unwrap();
        let _ = primary.read(&mut verb).unwrap();
        let big = Caller { tab: Some(tab(None, &["echo", &"x".repeat(INTRO_MAX as usize)])), ..me };
        let t = std::thread::spawn(move || {
            assert_eq!(read_line(&mut launch).unwrap(), "jetty 0.31.0");
            let _ = launch.write_all(&big.encode());
            let _ = launch.shutdown(std::net::Shutdown::Write);
            read_line(&mut launch).unwrap()
        });
        assert_eq!(hear_tab(&mut primary, "0.31.0", &here).1, None);
        drop(primary);
        assert_eq!(t.join().unwrap(), "refused the tab request did not arrive whole");
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
    fn a_launch_hands_on_its_activation_token() {
        let tok = "{5f1c3a52-9d0e-4a7b-8c61-2e3d4f5a6b7c}";
        let with = |t: &str| Caller { activation_token: Some(t.to_string()), ..caller("0.30.0", Display::new("wayland-0", ""), None) };
        assert_eq!(Caller::decode(&with(tok).encode()), Some(with(tok)));
        // Next to an AppImage path.
        let c = Caller { appimage: Some(PathBuf::from("/a/JeTTY.AppImage")), ..with(tok) };
        assert_eq!(Caller::decode(&c.encode()), Some(c));
        // An introduction without the field (or an empty one) carries none.
        let plain = caller("0.30.0", Display::default(), None);
        assert_eq!(Caller::decode(b"v1\x000.30.0\0\0\0"), Some(plain.clone()));
        assert_eq!(Caller::decode(&plain.encode()), Some(plain));
        // Something that can't be a token is never handed on.
        for bad in ["bad token", "\x1b[2J", &"x".repeat(513)] {
            let decoded = Caller::decode(&with(bad).encode()).unwrap();
            assert_eq!(decoded.activation_token, None, "{bad:?}");
        }
        // Through the exchange: the primary hears it with the caller.
        let (mut launch, mut primary) = pair();
        let me = with(tok);
        let t = std::thread::spawn(move || {
            launch.write_all(b"toggle").unwrap();
            introduce(&mut launch, &me)
        });
        let mut verb = [0u8; 16];
        let n = primary.read(&mut verb).unwrap();
        assert_eq!(&verb[..n], b"toggle", "the verb still comes alone");
        let (got, serve) = greet(&mut primary, "0.30.0", &Display::new("wayland-0", ""));
        assert!(serve);
        assert_eq!(got.and_then(|c| c.activation_token).as_deref(), Some(tok));
        drop(primary);
        assert_eq!(t.join().unwrap(), Answer::Served);
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
