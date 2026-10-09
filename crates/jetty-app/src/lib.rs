mod app;
/// System light/dark preference, reduced motion and accent (freedesktop
/// settings portal on Linux/BSD; winit's system theme elsewhere).
mod appearance;
mod backdrop;
/// Persisted settings. Public so the `jetty-shot` self-test can render the
/// Settings panel from a real config file.
pub mod config;
mod copymode;
mod detached;
/// Post-processing glue (CRT settings, effect presets, animation pacing, the
/// event glitch). Public so `jetty-shot` / `jetty-bench` build the CRT pass
/// through the SAME settings path as the app.
pub mod effects;
mod gridmouse;
/// What a launch and the running JeTTY say after the summon verb (version,
/// display) — see there.
mod ipc;
/// Keyboard navigation of the menus (pure). Public so the `jetty-shot`
/// self-test moves the highlight and anchors a menu exactly as the app does.
pub mod menunav;
pub mod motion;
mod notify;
/// Opening links with the platform opener (clean environment, fresh
/// activation token).
mod opener;
mod overlays;
mod runsel;
/// Command-palette action registry + fuzzy filter. Public so the `jetty-shot`
/// self-test binary can drive the SAME registry/filter path the app uses.
pub mod palette;
mod shell_integration;
mod tabmeta;
mod tabstrip;
/// Settings controls as data (the panel's descriptor table). Public so the
/// `jetty-shot` self-test builds the SAME panel content the app shows.
pub mod settings_ui;
mod watch;
/// The X11 summon-hotkey grab (Linux/BSD), event-driven — no polling thread.
#[cfg(all(unix, not(target_os = "macos")))]
mod x11_hotkey;
/// User theme loading + registry rebuild. Public so the `jetty-shot` self-test
/// binary can seed user themes before resolving `JETTY_THEME`.
pub mod themes;
pub mod clipboard;
pub mod input;
/// Configurable keybindings: chord grammar + compiled [`keymap::KeyMap`]. Public
/// so integration tests can build the default keymap the input path uses.
pub mod keymap;
/// Zero-cost-when-off real-window perf instrumentation (`JETTY_PERF_LOG=1`). Public
/// so `main.rs` can stamp process start and the `jetty-bench` bin can reuse the
/// shared percentile/env seams.
pub mod perf;

use app::AppEvent;
use winit::event_loop::{ControlFlow, EventLoop};

/// The detached window's context-menu `(label, hint)` rows under the DEFAULT
/// keymap — public so the `jetty-shot` self-test (JETTY_SHOT_DMENU) renders
/// EXACTLY the menu the app builds, driving the same
/// `DETACHED_MENU_ITEMS`/`menu_hint` pair.
pub fn detached_menu_items() -> Vec<(&'static str, String)> {
    let km = keymap::KeyMap::defaults();
    detached::DETACHED_MENU_ITEMS
        .iter()
        .map(|&l| (l, detached::menu_hint(&km, l)))
        .collect()
}

/// The main context menu's six shortcut hints under the DEFAULT keymap (for
/// `jetty-shot`; the app derives them from its live keymap the same way).
pub fn default_context_menu_hints() -> Vec<String> {
    detached::context_menu_hints(&keymap::KeyMap::defaults()).to_vec()
}

/// Copy-mode's block-selection sides `(anchor_left_half, cursor_left_half)`
/// for a rectangle between `anchor_col` and `cursor_col` (for `jetty-shot`'s
/// JETTY_SHOT_COPYMODE_BLOCK; the app's Ctrl+V uses the same rule).
pub fn copy_mode_block_sides(anchor_col: usize, cursor_col: usize) -> (bool, bool) {
    copymode::block_sides(anchor_col, cursor_col)
}

/// The welcome splash's tip line for keymap `km`: the command palette's
/// first chord as bound (the help overlay's form).
pub fn welcome_tip(km: &keymap::KeyMap) -> String {
    let chord = km.pretty_chords(keymap::BindableAction::OpenPalette).into_iter().next().unwrap_or_default();
    jetty_render::welcome_tip(&chord)
}

/// [`welcome_tip`] under the DEFAULT keymap (for `jetty-shot`).
pub fn default_welcome_tip() -> String {
    welcome_tip(&keymap::KeyMap::defaults())
}

/// The welcome splash's Summon row: the global key JeTTY grabs (`key`), or —
/// where it grabs none (Wayland, `summon_hotkey = "none"`) — the binding to
/// make.
pub fn welcome_summon(key: Option<&str>) -> String {
    key.map_or_else(|| "bind a key to jetty --toggle".to_string(), str::to_string)
}

/// The tab context menu's rows (for `jetty-shot`'s JETTY_SHOT_TAB_MENU).
pub fn shot_tab_menu_items(can_detach: bool) -> Vec<&'static str> {
    detached::tab_menu_items(can_detach)
}

/// The tab menu's "Color ▸" list rows (for `jetty-shot`).
pub fn shot_tab_color_menu_items() -> Vec<&'static str> {
    detached::tab_color_menu_items()
}

/// The color list's swatches, exactly as the app draws them (for `jetty-shot`).
pub fn shot_tab_color_swatches(
    item_rects: &[jetty_render::Rect],
    labels: &[&str],
    theme: &jetty_core::Theme,
    current: Option<u8>,
    cm: jetty_render::ChromeMetrics,
) -> Vec<jetty_render::Rect> {
    detached::tab_color_swatches(item_rects, labels, theme, current, cm)
}

/// The window ring's color for `window_border = mode` (a config string), as
/// the app picks it: the accent or the active tab's color while focused, the
/// muted border shade (or nothing) while not. For `jetty-shot`.
pub fn shot_ring_color(mode: &str, focused: bool, theme: &jetty_core::Theme, tab_color: Option<u8>) -> Option<[u8; 3]> {
    tabmeta::ring_rgb(tabmeta::WindowBorder::from_config(mode), focused, false, theme, tab_color)
}

/// The default grid padding `(padding_x, padding_y)` in logical px — the
/// config defaults, so `jetty-shot` renders the grid where the app does.
pub fn default_grid_padding() -> (f32, f32) {
    let d = config::Config::default();
    (d.padding_x, d.padding_y)
}

/// The `scrollbar` config key's modes and visibility rule (public for
/// `jetty-shot`'s JETTY_SHOT_SCROLLBAR).
pub use config::ScrollbarMode;

/// The `[backdrop]` table of the config JeTTY would load (`$JETTY_CONFIG_DIR`
/// honored), parsed, with its image file resolved — for `jetty-shot`'s
/// `JETTY_SHOT_BACKDROP=config`, so a shot shows exactly the configured look.
pub fn configured_backdrop() -> (jetty_render::BackdropSettings, Option<std::path::PathBuf>) {
    let b = config::Config::load().cfg.backdrop;
    (b.settings(), b.image_path(&config::Config::dir()))
}

/// Unix-socket path used for single-instance IPC. Any running primary Jetty
/// instance listens here; secondary invocations (including `jetty --toggle`)
/// connect and send a summon message, then exit immediately.
///
/// The socket lives inside a per-user, private directory (see
/// [`ipc_runtime_dir`]) so no other local user can pre-bind our path — which
/// would silently swallow every summon (a DoS) and leak our commands — or squat
/// the lock. We never place the socket directly in world-writable `/tmp`.
///
/// `display`: the socket of this display's own JeTTY, for a launch the one at
/// the usual path turned away as running elsewhere (see `ipc`).
fn ipc_socket_path(display: Option<&ipc::Display>) -> String {
    let override_dir = std::env::var_os("JETTY_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from);
    ipc_runtime_dir()
        .0
        .join(ipc_socket_name(override_dir.as_deref(), display))
        .to_string_lossy()
        .into_owned()
}

/// The instance lock for the socket at `sock_path`: beside it in the session's
/// `$XDG_RUNTIME_DIR` (a tmpfs nothing cleans while the session runs), else in
/// JeTTY's state directory ([`ipc_lock_dir`]). The cache directory the socket
/// then uses is fair game for cleaners — and for macOS freeing disk space —
/// and with the lock purged alongside, the next launch took a fresh lock and
/// started a second primary while the first still ran (F23's split brain).
fn ipc_lock_path(sock_path: &str) -> String {
    let name = std::path::Path::new(sock_path).file_name().map(|n| n.to_string_lossy().into_owned());
    match (ipc_runtime_dir().1, name) {
        (false, Some(name)) => match ipc_lock_dir() {
            Some(stable) => stable.join(format!("{name}.lock")).to_string_lossy().into_owned(),
            None => format!("{sock_path}.lock"),
        },
        _ => format!("{sock_path}.lock"),
    }
}

/// The IPC socket's file name: `jetty.sock` — or, with `$JETTY_CONFIG_DIR` set
/// (an alternate config tree), `jetty-<hash of that dir>.sock`. One primary per
/// config dir: `JETTY_CONFIG_DIR=/x jetty` used to find the user's running
/// instance and merely summon it, so the variable had no effect while JeTTY ran.
/// The hash is FNV-1a over the absolute, component-normalized path (`/x/` and
/// `/x` agree) — stable across builds, unlike std's hasher. A JeTTY on another
/// display than the one at that name (`display`) hashes its display in too.
fn ipc_socket_name(config_dir_override: Option<&std::path::Path>, display: Option<&ipc::Display>) -> String {
    let mut key: Vec<u8> = Vec::new();
    if let Some(dir) = config_dir_override {
        let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let norm: std::path::PathBuf = abs.components().collect();
        key.extend_from_slice(norm.as_os_str().as_encoded_bytes());
    }
    if let Some(d) = display {
        for part in ["\0display", &d.wayland, &d.x11] {
            key.extend_from_slice(part.as_bytes());
            key.push(0);
        }
    }
    if key.is_empty() {
        return "jetty.sock".to_string();
    }
    let hash = key
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("jetty-{hash:016x}.sock")
}

/// A private directory to hold the IPC socket, and whether it is the session's
/// own `$XDG_RUNTIME_DIR` (the instance lock then sits beside the socket — see
/// [`ipc_lock_path`]). Decided once per process.
///
/// `$XDG_RUNTIME_DIR` is a per-user 0700 tmpfs on logind systems and the ideal
/// home for a Unix socket — when it is what the XDG spec promises: absolute,
/// ours, 0700. A misconfigured one (`/tmp` in containers, `su` and WSL set-ups,
/// a relative path) is skipped, and so is bare `/tmp` always: there another
/// local user could pre-bind our (otherwise predictable) socket path or squat
/// the lock. Instead we use a private `jetty` subdir of the user's cache dir
/// (under `$HOME`, already per-user), and only as a last resort a 0700 subdir of
/// the system temp dir named by our uid. Because the socket then lives in a
/// directory only we can traverse ([`private_dir`]), any socket found there is
/// ours by construction — that directory permission authenticates the peer.
/// With none of them private, the "directory" is `/dev/null`, under which
/// nothing can be connected to, locked or bound: JeTTY runs, without
/// single-instance IPC.
fn ipc_runtime_dir() -> &'static (std::path::PathBuf, bool) {
    static DIR: std::sync::OnceLock<(std::path::PathBuf, bool)> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        if let Some(dir) = session_runtime_dir(std::env::var_os("XDG_RUNTIME_DIR")) {
            return (dir, true);
        }

        // Private per-user dir under the cache directory (~/.cache on Linux,
        // ~/Library/Caches on macOS): inside $HOME, so not world-writable.
        if let Some(cache) = dirs::cache_dir() {
            let dir = cache.join("jetty");
            if std::fs::create_dir_all(&dir).is_ok() {
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                if private_dir(&dir) {
                    return (dir, false);
                }
            }
        }

        // Last resort (no runtime dir and no cache/home): a 0700 subdir of the
        // system temp dir, named by our uid ($USER is anyone's to set). Created
        // 0700 in one step, and used only when it really is ours — another user
        // may have made it first.
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("jetty-{uid}"));
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
        if private_dir(&dir) {
            return (dir, false);
        }
        eprintln!(
            "jetty: no private directory for the IPC socket ({} is not ours); \
             single-instance IPC disabled",
            dir.display()
        );
        (std::path::PathBuf::from("/dev/null"), true)
    })
}

/// `$XDG_RUNTIME_DIR` (`var`) when it can hold the socket: absolute and private.
fn session_runtime_dir(var: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    var.map(std::path::PathBuf::from).filter(|d| d.is_absolute() && private_dir(d))
}

/// Whether `dir` is a directory only this user can reach: a real directory
/// (not a symlink someone could swap), owned by us, no group / other access.
fn private_dir(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    std::fs::symlink_metadata(dir)
        .is_ok_and(|m| m.file_type().is_dir() && m.uid() == uid && m.permissions().mode() & 0o077 == 0)
}

/// The instance lock's directory outside `$XDG_RUNTIME_DIR` (see
/// [`ipc_lock_path`]): JeTTY's state directory, which no cleaner purges —
/// `$XDG_STATE_HOME/jetty` (`~/.local/state/jetty`), or `~/Library/Application
/// Support/jetty` on macOS (no state dir there).
fn ipc_lock_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::state_dir().or_else(dirs::data_local_dir)?.join("jetty");
    std::fs::create_dir_all(&dir).ok().map(|_| dir)
}

/// Outcome of an IPC connect attempt.
enum ConnectResult {
    /// Connected to a live primary; message was sent, and this is how it took
    /// it. This process should exit.
    Forwarded(ipc::Answer),
    /// A live primary on another display: it left the command alone.
    Elsewhere,
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
/// `show`, or `hide`), then introduce ourselves as `me` (see `ipc`). Returns the
/// outcome so the caller can decide whether to unlink a stale socket or become
/// the primary.
fn forward_command(sock_path: &str, cmd: &str, me: &ipc::Caller) -> ConnectResult {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    match UnixStream::connect(sock_path) {
        Ok(mut stream) => {
            let _ = stream.write_all(cmd.as_bytes());
            let _ = stream.flush();
            // The verb is delivered; this bounds only how long we wait to hear
            // how it was taken (the summon itself never waits on it).
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            match ipc::introduce(&mut stream, me) {
                ipc::Answer::Elsewhere => ConnectResult::Elsewhere,
                answer => ConnectResult::Forwarded(answer),
            }
        }
        Err(e) => match e.kind() {
            // ECONNREFUSED: socket file exists but nobody is listening — stale.
            // std maps it on every unix (a hard-coded errno was 0 on the BSDs,
            // where a crash then left IPC dead for good).
            std::io::ErrorKind::ConnectionRefused => ConnectResult::Stale,
            std::io::ErrorKind::NotFound => ConnectResult::NoSocket,
            _ => ConnectResult::Other,
        },
    }
}

/// [`forward_command`] to the JeTTY of this launch's display: the one at
/// `sock_path` — or, when that one runs on another display (`ssh -X`, a second
/// X session), this display's own, which `sock_path` then names. Never returns
/// `Elsewhere`: anything but `Forwarded` means no live primary at `sock_path`.
fn forward_here(sock_path: &mut String, cmd: &str, me: &ipc::Caller) -> ConnectResult {
    let r = forward_command(sock_path, cmd, me);
    if !matches!(r, ConnectResult::Elsewhere) {
        return r;
    }
    let own = ipc_socket_path(Some(&me.display));
    if *sock_path == own {
        // This display's own socket turned us away too (its display changed
        // under it?): nothing sensible to start — leave it there.
        return ConnectResult::Forwarded(ipc::Answer::Elsewhere);
    }
    *sock_path = own;
    match forward_command(sock_path, cmd, me) {
        ConnectResult::Elsewhere => ConnectResult::Forwarded(ipc::Answer::Elsewhere),
        r => r,
    }
}

/// A primary took the command. One older than the introductions (`Older`)
/// can't know a newer JeTTY was launched, so a newer AppImage moves the login
/// item to itself here (`app::follow_newer_appimage`) — or every login would
/// keep starting the old file.
fn forwarded(answer: ipc::Answer, me: &ipc::Caller) {
    if answer != ipc::Answer::Older {
        return;
    }
    if let Some(exe) = &me.appimage {
        if app::follow_newer_appimage(exe) {
            eprintln!(
                "jetty: an older JeTTY is running — Launch at login now starts {}; quit the \
                 running one to switch now",
                exe.display()
            );
        }
    }
}

/// A summon verb a connection to the IPC socket sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IpcCommand {
    Show,
    Hide,
    Toggle,
    /// `--background`: never pops up an instance that is already running (a
    /// login autostart), but the launch is still greeted (`ipc`).
    Background,
}

/// The verb in what a connection sent first — `jetty --show` / `--hide` /
/// `--toggle` / `--background` send the bare word. ASCII whitespace around it
/// is ignored, so `echo toggle | nc -U <socket>` works too. Anything else is
/// not one of ours.
fn ipc_command(bytes: &[u8]) -> Option<IpcCommand> {
    match bytes.trim_ascii() {
        b"show" => Some(IpcCommand::Show),
        b"hide" => Some(IpcCommand::Hide),
        b"toggle" => Some(IpcCommand::Toggle),
        b"background" => Some(IpcCommand::Background),
        _ => None,
    }
}

/// How long the IPC thread waits for a connection's verb (and introduction):
/// an idle or half-open client (`nc -U`) can't wedge the serial accept loop.
const IPC_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// The pause after a failed `accept`, doubled per failure in a row up to
/// [`IPC_ACCEPT_BACKOFF_MAX`].
const IPC_ACCEPT_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_millis(10);
const IPC_ACCEPT_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(1);

/// The primary's IPC loop: read each connection's verb ([`ipc_command`]) and
/// hand it, with the connection (for the `ipc` exchange), to `serve` until
/// that reports the event loop gone. A failed `accept` is retried after a
/// growing `pause`: out of file descriptors (EMFILE — macOS gives a GUI app
/// 256) it fails before taking the pending connection, so retrying at once
/// spun a core until a tab closed.
fn serve_ipc(
    conns: impl Iterator<Item = std::io::Result<std::os::unix::net::UnixStream>>,
    mut serve: impl FnMut(IpcCommand, &mut std::os::unix::net::UnixStream) -> bool,
    mut pause: impl FnMut(std::time::Duration),
) {
    let mut backoff = IPC_ACCEPT_BACKOFF_MIN;
    for conn in conns {
        let mut s = match conn {
            Ok(s) => s,
            Err(_) => {
                pause(backoff);
                backoff = (backoff * 2).min(IPC_ACCEPT_BACKOFF_MAX);
                continue;
            }
        };
        backoff = IPC_ACCEPT_BACKOFF_MIN;
        let _ = s.set_read_timeout(Some(IPC_READ_TIMEOUT));
        let mut buf = [0u8; 16];
        // Zero bytes (a bare connect) or a read timeout: no command.
        let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
        if let Some(cmd) = ipc_command(&buf[..n]) {
            if !serve(cmd, &mut s) {
                break;
            }
        }
    }
}

/// Try to acquire the primary-instance lock: an `flock`-style exclusive lock
/// (std `File::try_lock`) on `lock_path`, created if missing. Returns the
/// locked `File` — hold it for the process lifetime — or `None` when another
/// process currently holds the lock (or the file can't be created).
///
/// Why a kernel lock and not an O_EXCL sentinel: the kernel releases the lock
/// automatically when the holder exits — INCLUDING crashes/SIGKILL — so there
/// is no stale-lock state to detect or clean up (the on-disk file may linger,
/// but an unlocked file is trivially re-lockable).
/// Outcome of a single primary-lock attempt. Distinguishing "held by a live
/// peer" from "the lock file can never be created" matters: the former means a
/// primary exists (retry/forward), the latter is a permanently degraded
/// environment where retrying just burns the whole 2 s deadline (F34).
enum LockAttempt {
    /// We now hold the exclusive lock; keep the `File` for the process lifetime.
    Acquired(std::fs::File),
    /// The lock file exists but another (live) process holds the lock.
    Held,
    /// The lock file could not even be opened/created (stale `XDG_RUNTIME_DIR`
    /// pointing at a removed path, a read-only or foreign-owned dir), or the
    /// filesystem can't lock it (ENOLCK). This is a permanent error for this
    /// launch — there is nothing to wait for.
    Unavailable,
}

fn try_acquire_primary_lock(lock_path: &str) -> LockAttempt {
    let f = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
    {
        Ok(f) => f,
        // open() failed permanently (ENOENT: parent gone, EACCES: not ours).
        // Do NOT collapse this into "lock held" — that made every cold start
        // spin the full 2 s retry loop before the first window appeared (F34).
        Err(_) => return LockAttempt::Unavailable,
    };
    let locked = f.try_lock();
    lock_outcome(f, locked)
}

/// What `try_lock` on the lock file `f` came to. Only contention means a live
/// peer holds it: a real error (ENOLCK on an NFS home without lockd, say — the
/// lock lives under ~/.cache/jetty when `XDG_RUNTIME_DIR` is unset) never
/// clears, so it degrades to the lockless bind at once like an open() failure,
/// instead of spinning the 2 s loop and refusing to start with no instance.
fn lock_outcome(f: std::fs::File, locked: Result<(), std::fs::TryLockError>) -> LockAttempt {
    match locked {
        Ok(()) => LockAttempt::Acquired(f),
        Err(std::fs::TryLockError::WouldBlock) => LockAttempt::Held,
        Err(std::fs::TryLockError::Error(_)) => LockAttempt::Unavailable,
    }
}

/// Unlink `path` only if it is still the exact socket inode we bound (`ident` =
/// its dev+ino at bind time). Prevents deleting a socket a DIFFERENT primary
/// rebound at the same path, and is a no-op when this process never bound
/// (`ident` is `None`, e.g. the lock-timeout degraded path).
fn remove_socket_if_ours(path: &str, ident: Option<(u64, u64)>) {
    use std::os::unix::fs::MetadataExt;
    let Some((dev, ino)) = ident else { return };
    if let Ok(m) = std::fs::metadata(path) {
        if (m.dev(), m.ino()) == (dev, ino) {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn run() {
    // CLI: --version/--help print and exit; --toggle/--show/--hide select the
    // summon command forwarded to a running instance. The compositor-bound
    // `jetty --toggle` is the cross-platform summon path — every X11/Wayland
    // compositor, no portal or DE-specific code.
    let version = env!("CARGO_PKG_VERSION");
    let build = option_env!("JETTY_BUILD").unwrap_or("dev");
    // Advertise the real release version to spawned shells (`$JETTY` /
    // `$TERM_PROGRAM_VERSION`); jetty-core alone only knows its placeholder.
    jetty_core::set_advertised_version(version);
    // A relative `JETTY_CONFIG_DIR` becomes the absolute folder it names here,
    // before the IPC socket is named after it and the shells inherit it.
    config::Config::pin_dir_env();

    // `--print-shell-integration <zsh|bash|fish>`: emit the OSC 133 opt-in
    // snippet and exit, BEFORE any IPC/GUI. Safe arg parsing — no panic on a
    // missing/non-UTF8 argument; a bad/absent shell prints usage to stderr and
    // exits 2. Handled here (not the loop below) so it can read the next token.
    {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if let Some(pos) = args.iter().position(|a| a.as_os_str() == "--print-shell-integration") {
            let shell = args.get(pos + 1).and_then(|a| a.to_str());
            match shell.and_then(shell_integration::snippet_for) {
                Some(snippet) => {
                    print!("{snippet}");
                    std::process::exit(0);
                }
                None => {
                    eprintln!("jetty: usage: jetty --print-shell-integration <zsh|bash|fish>");
                    std::process::exit(2);
                }
            }
        }
    }

    let mut cmd = "toggle";
    // args_os + to_str: std::env::args() panics on non-UTF8 argv; a bad byte in
    // an unknown arg should be ignored like any other unrecognized flag, not
    // abort the process before any window or IPC handling.
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--version") | Some("-version") | Some("-V") | Some("version") => {
                println!("jetty {version} ({build})");
                std::process::exit(0);
            }
            Some("--help") | Some("-help") | Some("-h") | Some("help") => {
                println!(
                    "JeTTY {version} — a blazing-fast GPU terminal with a global summon hotkey.\n\n\
                     USAGE:\n    jetty [FLAGS]\n\n\
                     FLAGS:\n\
                     \x20   --toggle       Show/hide a running instance (or launch one); same as plain `jetty`.\n\
                     \x20   --show         Summon a running instance (or launch one).\n\
                     \x20   --hide         Hide a running instance.\n\
                     \x20   --background   Launch hidden — no window until the first summon. Does nothing\n\
                     \x20                  if JeTTY is already running (\"Launch at login\" uses this).\n\
                     \x20   --check-config Check config.toml and the theme files: print every problem\n\
                     \x20                  (with the closest valid spelling) and exit, 1 if there are any.\n\
                     \x20   --version      Print version and exit.\n\
                     \x20   --help         Print this help and exit.\n\
                     \x20   --print-shell-integration <zsh|bash|fish>\n\
                     \x20                  Print the OSC 133 shell-integration snippet to stdout.\n\
                     Anything else is ignored: there is no `-e` — JeTTY always starts your shell.\n\n\
                     Bind `jetty --toggle` to a key in your compositor to summon from anywhere.\n\
                     Settings: Ctrl+, or Ctrl+Shift+O · Command palette: Ctrl+Shift+P\n\
                     Config: {config} (another dir: set JETTY_CONFIG_DIR)\n\
                     Shell integration (prompt marks, Ctrl+Shift+Z/X jump, Run & Notify). Add to your rc file:\n\
                     \x20 zsh:  {zsh}\n\
                     \x20 bash: {bash}\n\
                     \x20 fish: {fish}\n\
                     \x20       (fish 4+ marks its prompts itself — nothing to add)",
                    config = config::Config::config_path().display(),
                    zsh = shell_integration::ZSH_LINE,
                    bash = shell_integration::BASH_LINE,
                    fish = shell_integration::FISH_LINE,
                );
                std::process::exit(0);
            }
            Some("--check-config") => std::process::exit(config::check::check_cli()),
            Some("--toggle") => cmd = "toggle",
            Some("--show") => cmd = "show",
            Some("--hide") => cmd = "hide",
            Some("--background") => cmd = "background",
            _ => {}
        }
    }

    // Who this launch is (version, display, AppImage): said to a running
    // primary after the command (see `ipc`).
    let me = ipc::Caller::current();
    let mut sock_path = ipc_socket_path(None);

    // Secondary invocation: forward the command to the running primary and exit.
    // No banner, no GUI setup — a compositor-bound keypress stays instant.
    if let ConnectResult::Forwarded(answer) = forward_here(&mut sock_path, cmd, &me) {
        return forwarded(answer, &me);
    }
    // No live instance: `--hide` has nothing to hide; toggle/show launch.
    if cmd == "hide" {
        return;
    }

    // Become the primary. The stale-socket unlink+bind below is serialized by
    // an exclusive kernel lock (`ipc_lock_path`): the plain connect→unlink→bind
    // dance is only TOCTOU-safe against a LIVE primary — two concurrent COLD
    // starts racing over the same stale socket could both see ECONNREFUSED and
    // then unlink each other's freshly bound socket, yielding two primaries.
    // With the lock, exactly one process runs remove_file+bind; the loser
    // keeps retrying forward_command (the winner's socket appears within ms)
    // and exits once it gets through. The winning lock `File` is intentionally
    // leaked below (held for the process lifetime); the kernel releases it on
    // ANY exit, crash included, so no stale-lock handling is ever needed. A
    // primary on another display that binds the usual socket meanwhile turns us
    // to this display's own (`forward_here`): its lock is taken instead.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let lock_file: Option<std::fs::File> = loop {
        let lock_path = ipc_lock_path(&sock_path);
        let lock = match try_acquire_primary_lock(&lock_path) {
            LockAttempt::Acquired(f) => Some(f),
            LockAttempt::Unavailable => {
                // The lock can never be taken here. Retrying would just spin the
                // full 2 s for nothing — degrade to a lockless bind NOW so the
                // first frame is not delayed 2 s (F34). No primary answered on
                // the socket, so a lockless bind only risks two cold starts
                // racing at the same instant.
                eprintln!(
                    "jetty: single-instance lock at {lock_path} is unavailable; \
                     proceeding without it"
                );
                None
            }
            // Held: a live peer owns the lock (the kernel releases it on ANY exit).
            LockAttempt::Held => {
                // Another instance is mid-startup: give it a beat, then try forwarding.
                std::thread::sleep(std::time::Duration::from_millis(25));
                if let ConnectResult::Forwarded(answer) = forward_here(&mut sock_path, cmd, &me) {
                    return forwarded(answer, &me);
                }
                if std::time::Instant::now() >= deadline {
                    // Reaching the deadline means the lock was HELD on every
                    // iteration, i.e. a live primary exists but we could never
                    // reach its socket — its socket file was removed out from
                    // under it. Booting a second primary here would split-brain
                    // (two windows, two config writers, the hotkey and --toggle
                    // driving different instances) — the very bug the lock exists
                    // to prevent. Refuse to duplicate; the existing instance is
                    // alive (F23). Exit non-zero: a launcher or the login item
                    // must not see success.
                    eprintln!(
                        "jetty: another instance holds the lock but its IPC socket at \
                         {sock_path} is unreachable (deleted?); not starting a second \
                         instance — quit the running one first"
                    );
                    std::process::exit(1);
                }
                continue;
            }
        };
        // UNDER the lock, re-check for a live primary (one may have bound while
        // we waited) and only now unlink a provably stale socket (ECONNREFUSED).
        let locked = sock_path.clone();
        match forward_here(&mut sock_path, cmd, &me) {
            ConnectResult::Forwarded(answer) => return forwarded(answer, &me),
            ConnectResult::Stale if sock_path == locked => {
                std::fs::remove_file(&sock_path).ok();
            }
            _ => {}
        }
        if sock_path == locked {
            break lock;
        }
    };

    eprintln!("jetty {version} ({build})");

    let listener: Option<std::os::unix::net::UnixListener> =
        match std::os::unix::net::UnixListener::bind(&sock_path) {
            Ok(l) => {
                // Restrict the socket to the owner (defense in depth; the
                // enclosing directory is already 0700).
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &sock_path,
                    std::fs::Permissions::from_mode(0o600),
                );
                eprintln!("jetty: IPC socket bound at {sock_path}");
                Some(l)
            }
            Err(e) => {
                eprintln!("jetty: could not bind IPC socket at {sock_path}: {e} — single-instance IPC disabled");
                None
            }
        };
    // Identify the socket inode we just bound so cleanup only ever unlinks OUR
    // socket — never one a different primary rebound at the same path (e.g.
    // after a tmp cleaner aged ours out and we ran degraded without binding).
    let bound_ident: Option<(u64, u64)> = if listener.is_some() {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&sock_path).ok().map(|m| (m.dev(), m.ino()))
    } else {
        None
    };
    // Hold the primary lock for the process lifetime (released by the kernel
    // on exit). Leaking the File keeps the descriptor — and the lock — alive.
    std::mem::forget(lock_file);

    // Startup Vulkan driver filter (Linux): the first GPU instance skips the
    // drivers that cannot matter here (`jetty_render::vk_loader`). It is an
    // environment variable, so it is set HERE, while the process still has one
    // thread — and hidden from the shells (the first one starts before the GPU
    // block releases it).
    if jetty_render::vk_loader::install(jetty_render::vk_loader::wants_high_performance()).is_some() {
        jetty_core::hide_from_shells(jetty_render::vk_loader::FILTER_VAR);
    }

    let mut builder = EventLoop::<AppEvent>::with_user_event();
    // macOS: winit activates the application as it launches. A hidden start
    // (`--background`, the login item) has no window to take the keyboard, so
    // JeTTY sat frontmost with nothing to type into — the keys of the app in
    // front went nowhere. Activate only when no other app is active.
    #[cfg(target_os = "macos")]
    if cmd == "background" {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_activate_ignoring_other_apps(false);
    }
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            // No display to open a window on (an SSH or console session, a dead
            // compositor): say so and exit 1 — not a panic (status 101) — and
            // leave no socket behind for the next launch to clean up.
            eprintln!(
                "jetty: can't reach a display ({e}) — JeTTY needs an X11 or Wayland \
                 session (DISPLAY / WAYLAND_DISPLAY)"
            );
            remove_socket_if_ours(&sock_path, bound_ident);
            std::process::exit(1);
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    // IPC accept thread (primary only): map each forwarded command to an event
    // (`serve_ipc`). `show`/`hide` set visibility explicitly, `toggle` toggles.
    // Shares the summon code path with the X11 global-hotkey grab. Every launch
    // is greeted and says who it is (`ipc`): one on another display is turned
    // away, and a newer JeTTY is announced once — it would otherwise only ever
    // toggle this older one.
    if let Some(listener) = listener {
        let proxy_ipc = proxy.clone();
        let sock_cleanup = sock_path.clone();
        let here = me.display.clone();
        std::thread::spawn(move || {
            let mut announced: Option<String> = None;
            let serve = |cmd, s: &mut std::os::unix::net::UnixStream| {
                let event = match cmd {
                    IpcCommand::Show => Some(AppEvent::SetVisible(true)),
                    IpcCommand::Hide => Some(AppEvent::SetVisible(false)),
                    IpcCommand::Toggle => Some(AppEvent::ToggleVisibility),
                    // `--background`: no-op, don't toggle (a login autostart must
                    // never pop up an instance that's already running).
                    IpcCommand::Background => None,
                };
                let (caller, serve) = ipc::greet(s, version, &here);
                if !serve {
                    return true;
                }
                // The launcher's activation token goes just ahead of a summon:
                // a Wayland summon builds the window with it.
                let token = caller
                    .as_ref()
                    .and_then(|c| c.activation_token.clone())
                    .filter(|_| matches!(event, Some(AppEvent::SetVisible(true) | AppEvent::ToggleVisibility)));
                let mut events: Vec<AppEvent> =
                    token.map(AppEvent::ActivationToken).into_iter().chain(event).collect();
                let newer = caller.filter(|c| c.newer_than(version) && announced.as_ref() != Some(&c.version));
                if let Some(c) = newer {
                    let moved = c.appimage.as_deref().is_some_and(app::follow_newer_appimage);
                    events.push(AppEvent::Notice(app::newer_version_notice(&c.version, version, moved)));
                    announced = Some(c.version);
                }
                !events.into_iter().any(|e| proxy_ipc.send_event(e).is_err())
            };
            serve_ipc(listener.incoming(), serve, std::thread::sleep);
            remove_socket_if_ours(&sock_cleanup, bound_ident);
        });
    }

    let mut app = app::App::new(proxy);
    app.set_start_hidden(cmd == "background");
    event_loop.run_app(&mut app).expect("run_app");

    // Best-effort cleanup on normal exit. Crashes are handled by the
    // remove-stale-on-bind logic at the start of the next launch. Only unlink
    // the socket if it is still the one WE bound.
    remove_socket_if_ours(&sock_path, bound_ident);
    if app.startup_failed() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod ipc_socket_tests {
    use super::{ipc, ipc_socket_name};
    use std::path::Path;

    #[test]
    fn an_alternate_config_dir_gets_its_own_instance() {
        // The default socket is unchanged — a `jetty --toggle` bound in the
        // compositor keeps finding the user's instance.
        assert_eq!(ipc_socket_name(None, None), "jetty.sock");
        let a = ipc_socket_name(Some(Path::new("/home/u/try-config")), None);
        assert!(a.starts_with("jetty-") && a.ends_with(".sock") && a != "jetty.sock", "{a}");
        assert_eq!(a, ipc_socket_name(Some(Path::new("/home/u/try-config/")), None), "trailing slash");
        assert_eq!(a, ipc_socket_name(Some(Path::new("/home/u//try-config")), None), "doubled slash");
        assert_ne!(a, ipc_socket_name(Some(Path::new("/home/u/other-config")), None));
        // Stable across runs/builds — and versions, so an older launch finds a
        // newer primary: a fixed FNV-1a of the path, not std's hasher.
        assert_eq!(a, "jetty-9b075b403ed34830.sock");
        assert_eq!(a.len(), "jetty-".len() + 16 + ".sock".len());
    }

    #[test]
    fn another_display_gets_its_own_socket() {
        // A launch turned away by a JeTTY on another display (ssh -X, a second
        // X session) uses its display's own socket — per config dir as well.
        let d0 = ipc::Display { wayland: String::new(), x11: ":0".into() };
        let d1 = ipc::Display { wayland: String::new(), x11: "localhost:10".into() };
        let own = ipc_socket_name(None, Some(&d1));
        assert!(own.starts_with("jetty-") && own != "jetty.sock", "{own}");
        assert_ne!(own, ipc_socket_name(None, Some(&d0)));
        assert_eq!(own, ipc_socket_name(None, Some(&d1.clone())));
        let cfg = Some(Path::new("/home/u/try-config"));
        assert_ne!(ipc_socket_name(cfg, Some(&d1)), own);
        assert_ne!(ipc_socket_name(cfg, Some(&d1)), ipc_socket_name(cfg, None));
    }
}

#[cfg(test)]
mod primary_lock_tests {
    use super::{try_acquire_primary_lock, LockAttempt};

    fn tmp_lock_path(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("jetty-lock-test-{tag}-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn lock_is_exclusive_while_held() {
        let path = tmp_lock_path("excl");
        let first = try_acquire_primary_lock(&path);
        assert!(matches!(first, LockAttempt::Acquired(_)), "first acquire must succeed");
        // flock-style locks are per open-file-description, so a second open —
        // even in the same process — must be refused while the first is held.
        // This is exactly the two-concurrent-cold-starts race: only one may
        // enter the unlink+bind section. It must report Held (a live peer), NOT
        // Unavailable — the lock FILE opens fine (F34).
        assert!(
            matches!(try_acquire_primary_lock(&path), LockAttempt::Held),
            "second acquire must report Held while the lock is held"
        );
        drop(first);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lock_is_reacquirable_after_release() {
        let path = tmp_lock_path("realock");
        let first = try_acquire_primary_lock(&path);
        assert!(matches!(first, LockAttempt::Acquired(_)));
        drop(first); // holder exits → kernel releases the lock (no stale state)
        assert!(
            matches!(try_acquire_primary_lock(&path), LockAttempt::Acquired(_)),
            "lock must be free again once the holder is gone"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lock_survives_a_leftover_file() {
        // A lingering lock FILE from a previous run is not a stale lock: the
        // kernel lock died with its holder, so acquiring must succeed.
        let path = tmp_lock_path("leftover");
        std::fs::write(&path, b"").unwrap();
        assert!(matches!(try_acquire_primary_lock(&path), LockAttempt::Acquired(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn only_contention_counts_as_held() {
        use super::lock_outcome;
        use std::fs::TryLockError;
        let path = tmp_lock_path("outcome");
        let file = || std::fs::File::create(&path).unwrap();
        assert!(matches!(lock_outcome(file(), Ok(())), LockAttempt::Acquired(_)));
        assert!(matches!(lock_outcome(file(), Err(TryLockError::WouldBlock)), LockAttempt::Held));
        // ENOLCK (an NFS home without lockd) is no live peer: waiting can't
        // help, so it degrades at once instead of refusing to start 2 s later.
        let enolck = std::io::Error::from_raw_os_error(37);
        assert!(matches!(
            lock_outcome(file(), Err(TryLockError::Error(enolck))),
            LockAttempt::Unavailable
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unavailable_when_lock_file_cannot_be_created() {
        // Regression (F34): a lock path whose parent dir cannot exist must report
        // Unavailable (a permanent error → degrade immediately), NOT Held (which
        // spun the full 2 s retry loop before the first window appeared).
        let path = "/nonexistent-jetty-dir-xyz/jetty.sock.lock";
        assert!(matches!(
            try_acquire_primary_lock(path),
            LockAttempt::Unavailable
        ));
    }
}

#[cfg(test)]
mod ipc_dir_tests {
    use super::{private_dir, session_runtime_dir};
    use std::os::unix::fs::PermissionsExt;

    /// A fresh directory under the system temp dir with `mode`.
    fn dir(tag: &str, mode: u32) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("jetty-dir-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
        d
    }

    #[test]
    fn only_a_directory_of_our_own_holds_the_socket() {
        let private = dir("private", 0o700);
        let open = dir("open", 0o755);
        let shared = dir("shared", 0o1777); // what `/tmp` is
        assert!(private_dir(&private));
        assert!(!private_dir(&open), "group/other may traverse it");
        assert!(!private_dir(&shared), "anyone may create in it");
        // A symlink to a private directory could be swapped under us.
        let link = std::env::temp_dir().join(format!("jetty-dir-test-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&private, &link).unwrap();
        assert!(!private_dir(&link));
        // Not a directory, or not there at all.
        let file = private.join("f");
        std::fs::write(&file, b"").unwrap();
        assert!(!private_dir(&file));
        assert!(!private_dir(&private.join("missing")));
        // $XDG_RUNTIME_DIR: only an absolute, private one is used.
        assert_eq!(session_runtime_dir(Some(private.clone().into())), Some(private.clone()));
        assert_eq!(session_runtime_dir(Some(shared.clone().into())), None);
        assert_eq!(session_runtime_dir(Some("relative/run".into())), None);
        assert_eq!(session_runtime_dir(Some("".into())), None);
        assert_eq!(session_runtime_dir(None), None);
        let _ = std::fs::remove_file(&link);
        for d in [private, open, shared] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod ipc_serve_tests {
    use super::{
        forward_command, ipc, ipc_command, serve_ipc, ConnectResult, IpcCommand, IPC_ACCEPT_BACKOFF_MAX,
    };
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::Duration;

    /// The server end of a connection whose client sent `bytes` and hung up.
    fn sent(bytes: &[u8]) -> UnixStream {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(bytes).unwrap();
        server
    }

    #[test]
    fn a_command_may_carry_ascii_whitespace() {
        let cases: [(&[u8], Option<IpcCommand>); 11] = [
            (b"toggle", Some(IpcCommand::Toggle)),
            (b"toggle\n", Some(IpcCommand::Toggle)), // `echo toggle | nc -U <socket>`
            (b" show\r\n", Some(IpcCommand::Show)),
            (b"\thide ", Some(IpcCommand::Hide)),
            (b"background", Some(IpcCommand::Background)),
            (b"", None),
            (b"\n", None),
            (b"toggle!", None),
            (b"TOGGLE", None),
            (b"to ggle", None),
            (b"v1\0", None),
        ];
        for (bytes, want) in cases {
            assert_eq!(ipc_command(bytes), want, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn silent_or_unknown_connections_ask_for_nothing() {
        let (hung_up, client) = UnixStream::pair().unwrap();
        drop(client);
        // Still open and never writing: the read times out.
        let (idle, _open) = UnixStream::pair().unwrap();
        let conns = vec![Ok(hung_up), Ok(idle), Ok(sent(b"bogus")), Ok(sent(b"toggle\n"))];
        let mut got = Vec::new();
        serve_ipc(conns.into_iter(), |c, _| { got.push(c); true }, |_| {});
        assert_eq!(got, [IpcCommand::Toggle]);
    }

    #[test]
    fn failed_accepts_back_off_instead_of_spinning() {
        let emfile = || Err(std::io::Error::from_raw_os_error(24));
        let conns = vec![emfile(), emfile(), emfile(), Ok(sent(b"toggle")), emfile(), Ok(sent(b"show"))];
        let (mut got, mut pauses) = (Vec::new(), Vec::new());
        serve_ipc(conns.into_iter(), |c, _| { got.push(c); true }, |d| pauses.push(d));
        assert_eq!(got, [IpcCommand::Toggle, IpcCommand::Show], "the queued commands still arrive");
        let ms = Duration::from_millis;
        assert_eq!(pauses, [ms(10), ms(20), ms(40), ms(10)], "doubling, reset by an accept");
        let mut pauses = Vec::new();
        serve_ipc((0..20).map(|_| emfile()), |_, _| true, |d| pauses.push(d));
        assert_eq!(pauses.len(), 20);
        assert!(pauses.iter().all(|&d| d <= IPC_ACCEPT_BACKOFF_MAX));
        assert_eq!(pauses.last(), Some(&IPC_ACCEPT_BACKOFF_MAX));
    }

    #[test]
    fn the_loop_ends_with_the_event_loop() {
        let conns = vec![Ok(sent(b"show")), Ok(sent(b"hide"))];
        let mut got = Vec::new();
        serve_ipc(conns.into_iter(), |c, _| { got.push(c); false }, |_| {});
        assert_eq!(got, [IpcCommand::Show], "nothing is read once the send failed");
    }

    #[test]
    fn a_socket_nobody_listens_on_is_stale() {
        let path = std::env::temp_dir().join(format!("jetty-stale-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = path.to_str().unwrap();
        let me = ipc::Caller::current();
        assert!(matches!(forward_command(p, "toggle", &me), ConnectResult::NoSocket));
        // A live primary (older than the introductions: it reads the verb and
        // hangs up) takes it.
        let live = UnixListener::bind(&path).unwrap();
        let primary = std::thread::spawn(move || {
            let (mut s, _) = live.accept().unwrap();
            let mut verb = [0u8; 16];
            let n = s.read(&mut verb).unwrap();
            verb[..n].to_vec()
        });
        assert!(matches!(forward_command(p, "show", &me), ConnectResult::Forwarded(_)));
        assert_eq!(primary.join().unwrap(), b"show");
        // A crashed primary leaves its socket file behind: refused, so stale.
        assert!(matches!(forward_command(p, "toggle", &me), ConnectResult::Stale));
        let _ = std::fs::remove_file(&path);
    }
}
