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
pub mod motion;
mod notify;
mod overlays;
mod runsel;
/// Command-palette action registry + fuzzy filter. Public so the `jetty-shot`
/// self-test binary can drive the SAME registry/filter path the app uses.
pub mod palette;
mod shell_integration;
mod tabmeta;
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
/// The socket lives inside a per-user, non-world-writable directory (see
/// [`ipc_runtime_dir`]) so no other local user can pre-bind our path — which
/// would silently swallow every summon (a DoS) and leak our commands — or squat
/// the lock. We never place the socket directly in world-writable `/tmp`.
fn ipc_socket_path() -> String {
    let override_dir = std::env::var_os("JETTY_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from);
    ipc_runtime_dir()
        .join(ipc_socket_name(override_dir.as_deref()))
        .to_string_lossy()
        .into_owned()
}

/// The IPC socket's file name: `jetty.sock` — or, with `$JETTY_CONFIG_DIR` set
/// (an alternate config tree), `jetty-<hash of that dir>.sock`. One primary per
/// config dir: `JETTY_CONFIG_DIR=/x jetty` used to find the user's running
/// instance and merely summon it, so the variable had no effect while JeTTY ran.
/// The hash is FNV-1a over the absolute, component-normalized path (`/x/` and
/// `/x` agree) — stable across builds, unlike std's hasher.
fn ipc_socket_name(config_dir_override: Option<&std::path::Path>) -> String {
    let Some(dir) = config_dir_override else {
        return "jetty.sock".to_string();
    };
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let norm: std::path::PathBuf = abs.components().collect();
    let hash = norm
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("jetty-{hash:016x}.sock")
}

/// A per-user, non-world-writable directory to hold the IPC socket + lock.
///
/// `$XDG_RUNTIME_DIR` is a per-user 0700 tmpfs on logind systems and the ideal
/// home for a Unix socket. When it is unset (always on macOS, common on minimal
/// Linux) we must NOT fall back to bare `/tmp`: it is world-writable, so another
/// local user could pre-bind our (otherwise predictable) socket path or squat
/// the lock. Instead we use a private `jetty` subdir of the user's cache dir
/// (under `$HOME`, already per-user), and only as a last resort a 0700 subdir of
/// the system temp dir. Because the socket then lives in a directory only we can
/// traverse, any socket found there is ours by construction — that directory
/// permission authenticates the peer, standing in for an explicit uid check
/// (which would need a libc dependency this crate does not carry).
fn ipc_runtime_dir() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }

    // Private per-user dir under the cache directory (~/.cache on Linux,
    // ~/Library/Caches on macOS): inside $HOME, so not world-writable.
    if let Some(cache) = dirs::cache_dir() {
        let dir = cache.join("jetty");
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            return dir;
        }
    }

    // Last resort (no runtime dir and no cache/home): a 0700 subdir of the
    // system temp dir. Tighten perms so it is at least not world-writable.
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let dir = std::env::temp_dir().join(format!("jetty-{user}"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
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
    /// pointing at a removed path, a read-only or foreign-owned dir). This is a
    /// permanent error for this launch — there is nothing to wait for.
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
    match f.try_lock() {
        Ok(()) => LockAttempt::Acquired(f),
        Err(_) => LockAttempt::Held,
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
                     \x20                  Print the OSC 133 shell-integration snippet to stdout.\n\n\
                     Bind `jetty --toggle` to a key in your compositor to summon from anywhere.\n\
                     Settings: Ctrl+, or Ctrl+Shift+O · Command palette: Ctrl+Shift+P\n\
                     Config: {config} (another dir: set JETTY_CONFIG_DIR)\n\
                     Shell integration (prompt marks, Ctrl+Shift+Z/X jump, Run & Notify). Add to your rc file:\n\
                     \x20 zsh:  [[ -n \"${{JETTY-}}\" ]] && source <(\"${{JETTY_BIN:-jetty}}\" --print-shell-integration zsh)\n\
                     \x20 bash: [[ -n \"${{JETTY-}}\" ]] && source <(\"${{JETTY_BIN:-jetty}}\" --print-shell-integration bash)\n\
                     \x20 fish: set -q JETTY; and \"$JETTY_BIN\" --print-shell-integration fish | source",
                    config = config::Config::config_path().display(),
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

    let sock_path = ipc_socket_path();

    // Secondary invocation: forward the command to the running primary and exit.
    // No banner, no GUI setup — a compositor-bound keypress stays instant.
    match forward_command(&sock_path, cmd) {
        ConnectResult::Forwarded => return,
        ConnectResult::Stale | ConnectResult::NoSocket | ConnectResult::Other => {}
    }
    // No live instance: `--hide` has nothing to hide; toggle/show launch.
    if cmd == "hide" {
        return;
    }

    // Become the primary. The stale-socket unlink+bind below is serialized by
    // an exclusive kernel lock (`<sock>.lock`): the plain connect→unlink→bind
    // dance is only TOCTOU-safe against a LIVE primary — two concurrent COLD
    // starts racing over the same stale socket could both see ECONNREFUSED and
    // then unlink each other's freshly bound socket, yielding two primaries.
    // With the lock, exactly one process runs remove_file+bind; the loser
    // keeps retrying forward_command (the winner's socket appears within ms)
    // and exits once it gets through. The winning lock `File` is intentionally
    // leaked below (held for the process lifetime); the kernel releases it on
    // ANY exit, crash included, so no stale-lock handling is ever needed.
    let lock_path = format!("{sock_path}.lock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let lock_file: Option<std::fs::File> = loop {
        match try_acquire_primary_lock(&lock_path) {
            LockAttempt::Acquired(f) => break Some(f),
            LockAttempt::Unavailable => {
                // The lock file can never be created here. Retrying would just
                // spin the full 2 s for nothing — degrade to a lockless bind NOW
                // so the first frame is not delayed 2 s (F34). No reachable
                // primary exists in this environment (the socket lives under the
                // same broken dir), so a lockless bind is safe.
                eprintln!(
                    "jetty: single-instance lock at {lock_path} is unavailable; \
                     proceeding without it"
                );
                break None;
            }
            // Held: a live peer owns the lock (the kernel releases it on ANY exit).
            // Fall through to retry-forward below.
            LockAttempt::Held => {}
        }
        // Another instance is mid-startup: give it a beat, then try forwarding.
        std::thread::sleep(std::time::Duration::from_millis(25));
        if matches!(forward_command(&sock_path, cmd), ConnectResult::Forwarded) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            // Reaching the deadline means the lock was HELD on every iteration
            // (Acquired/Unavailable both break out), i.e. a live primary exists
            // but we could never reach its socket — its socket file was removed
            // out from under it. Booting a second primary here would split-brain
            // (two windows, two config writers, the hotkey and --toggle driving
            // different instances) — the very bug the lock exists to prevent.
            // Refuse to duplicate; the existing instance is alive (F23).
            eprintln!(
                "jetty: another instance holds the lock but its IPC socket at \
                 {sock_path} is unreachable; not starting a second instance"
            );
            return;
        }
    };

    // UNDER the lock, re-check for a live primary (one may have bound while we
    // waited) and only now unlink a provably stale socket (ECONNREFUSED).
    match forward_command(&sock_path, cmd) {
        ConnectResult::Forwarded => return,
        ConnectResult::Stale => {
            std::fs::remove_file(&sock_path).ok();
        }
        ConnectResult::NoSocket | ConnectResult::Other => {}
    }

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
            for mut s in listener.incoming().flatten() {
                // Bound the read so an idle/half-open client (e.g. `nc -U`)
                // can't wedge this serial accept loop and silently kill
                // summon IPC.
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(250)));
                let mut buf = [0u8; 16];
                let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
                if n == 0 {
                    // Zero-byte connect or read timeout: never toggle.
                    continue;
                }
                let event = match &buf[..n] {
                    b"show" => AppEvent::SetVisible(true),
                    b"hide" => AppEvent::SetVisible(false),
                    b"toggle" => AppEvent::ToggleVisibility,
                    // `--background` / unknown command: no-op, don't toggle (a login
                    // autostart must never pop up an instance that's already running).
                    _ => continue,
                };
                if proxy_ipc.send_event(event).is_err() {
                    break;
                }
            }
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
    use super::ipc_socket_name;
    use std::path::Path;

    #[test]
    fn an_alternate_config_dir_gets_its_own_instance() {
        // The default socket is unchanged — a `jetty --toggle` bound in the
        // compositor keeps finding the user's instance.
        assert_eq!(ipc_socket_name(None), "jetty.sock");
        let a = ipc_socket_name(Some(Path::new("/home/u/try-config")));
        assert!(a.starts_with("jetty-") && a.ends_with(".sock") && a != "jetty.sock", "{a}");
        assert_eq!(a, ipc_socket_name(Some(Path::new("/home/u/try-config/"))), "trailing slash");
        assert_eq!(a, ipc_socket_name(Some(Path::new("/home/u//try-config"))), "doubled slash");
        assert_ne!(a, ipc_socket_name(Some(Path::new("/home/u/other-config"))));
        // Stable across runs/builds: a fixed FNV-1a, not std's hasher.
        assert_eq!(ipc_socket_name(Some(Path::new("/x"))), ipc_socket_name(Some(Path::new("/x"))));
        assert_eq!(a.len(), "jetty-".len() + 16 + ".sock".len());
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
