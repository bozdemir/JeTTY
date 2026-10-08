use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The version JeTTY advertises to child shells via `TERM_PROGRAM_VERSION` and
/// `JETTY`. The binary sets this once at startup (its real release version); the
/// headless shot and tests fall back to the crate version.
static ADVERTISED_VERSION: OnceLock<String> = OnceLock::new();

/// Record the version JeTTY advertises to spawned shells (call once at startup
/// from the app binary so the shell's `$JETTY` / `$TERM_PROGRAM_VERSION` carry
/// the true release version rather than jetty-core's placeholder).
pub fn set_advertised_version(version: &str) {
    let _ = ADVERTISED_VERSION.set(version.to_string());
}

/// The advertised version, defaulting to the crate version when unset.
fn advertised_version() -> String {
    ADVERTISED_VERSION
        .get()
        .cloned()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Hard ceiling on bytes queued to a session's writer thread but not yet
/// written to the fd. Normal use never approaches this: the writer thread
/// drains the channel at fd speed, so `queued` sits near zero. It only fills
/// when the child stops reading its stdin AND something keeps producing output
/// — the classic case being a `yes $'\e[6n'` / hostile-content query flood
/// where the terminal auto-answers every CPR/DA into the queue while the
/// blocked child never drains the ~4 KiB kernel tty buffer. The old unbounded
/// channel grew by GBs/min until the OOM killer took the whole app (F13). We
/// bound it at 64 MiB — far above any realistic keystroke burst or single
/// paste, so legitimate writes are never dropped, yet low enough that a
/// pathological reply loop caps in a few seconds instead of exhausting RAM.
const PTY_WRITE_QUEUE_CAP: usize = 64 * 1024 * 1024;

/// Bytes per PTY read — the most one queued output chunk can hold.
const PTY_READ_CHUNK: usize = 8 * 1024;

/// Most chunks the reader thread may queue ahead of the UI (≤ 8 MiB at
/// [`PTY_READ_CHUNK`]). When the UI falls this far behind, the reader BLOCKS on
/// the full queue, stops reading, the kernel tty buffer fills, and the child
/// blocks in write(2) — the backpressure every terminal relies on. The old
/// unbounded queue let a `yes`/`cat` flood that outran the VT parser pile up
/// hundreds of MB (GBs over minutes), and Ctrl+C then kept the screen scrolling
/// for seconds while that backlog drained. Counted in chunks, not bytes: reads
/// that keep up with the child are small, but once the queue backs up the tty
/// buffer refills between reads and chunks grow to the full 8 KiB, so a drain
/// pass still finds its whole budget waiting. Measured with a winit-shaped
/// harness: `cat` throughput unchanged, a long-line `yes` flood is quiet ~50 ms
/// after Ctrl+C, peak RSS ~47 MB (was 250+ MB and seconds).
const PTY_READ_QUEUE_CHUNKS: usize = 1024;

/// The UI wake, shared by the reader (new output / EOF) and waiter (exit)
/// threads. Only `Send` is required of the app's callback, hence the `Mutex`;
/// it is never contended in practice (wakes are coalesced, see `ReadShared`).
type Wake = Arc<Mutex<Box<dyn Fn() + Send>>>;

/// State shared between a session's reader thread and its owner (the UI).
struct ReadShared {
    /// Bytes queued by the reader and not yet taken by the UI. The reader adds
    /// BEFORE it sends; every receive path subtracts, so `> 0` means "output is
    /// waiting" (see [`PtySession::rearm_wake`]).
    queued: AtomicUsize,
    /// Wake coalescing latch. The reader wakes the UI only on its false→true
    /// transition, so a flood costs one wake per event-loop iteration instead of
    /// one per chunk (~350K wakes/s measured for `cat` of a log). Cleared only by
    /// [`PtySession::rearm_wake`], at the END of an event-loop iteration — see
    /// there for why it must not be cleared during the drain.
    wake_pending: AtomicBool,
}

/// The UI's end of a session's output queue (every receive path keeps
/// `ReadShared::queued` exact). Separate from `PtySession` so the queue/wake
/// protocol is unit-testable without a shell.
struct OutputQueue {
    rx: Receiver<Vec<u8>>,
    read: Arc<ReadShared>,
}

impl OutputQueue {
    fn new(rx: Receiver<Vec<u8>>, read: Arc<ReadShared>) -> Self {
        OutputQueue { rx, read }
    }

    fn drain(&self, budget: usize, mut f: impl FnMut(&[u8])) -> usize {
        let mut fed = 0usize;
        while fed < budget {
            match self.rx.try_recv() {
                Ok(chunk) => {
                    fed += self.took(&chunk);
                    f(&chunk);
                }
                Err(_) => break,
            }
        }
        fed
    }

    fn try_recv(&self) -> Option<Vec<u8>> {
        let chunk = self.rx.try_recv().ok()?;
        self.took(&chunk);
        Some(chunk)
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        match self.rx.recv_timeout(timeout) {
            Ok(chunk) => {
                self.took(&chunk);
                Some(chunk)
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Account for a chunk taken off the queue; returns its length.
    fn took(&self, chunk: &[u8]) -> usize {
        self.read.queued.fetch_sub(chunk.len(), Ordering::SeqCst);
        chunk.len()
    }

    fn rearm(&self) -> bool {
        if self.read.queued.load(Ordering::SeqCst) > 0 {
            self.read.wake_pending.store(true, Ordering::SeqCst);
            return true;
        }
        self.read.wake_pending.store(false, Ordering::SeqCst);
        // Lost-wakeup guard: a chunk queued between the check above and the
        // store found the latch still set and did not wake anyone. If output
        // is queued now, take the latch back — unless the reader already did
        // (after our store), in which case it has woken the app itself.
        self.read.queued.load(Ordering::SeqCst) > 0
            && !self.read.wake_pending.swap(true, Ordering::SeqCst)
    }
}

pub struct PtySession {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    /// Output queued by the reader thread, plus the coalesced-wake state.
    output: OutputQueue,
    /// Write end of the reader's shutdown pipe (Linux): dropping it — i.e.
    /// dropping the session — makes the reader's `poll` return so the thread
    /// exits and closes its master fd even when a disowned process keeps the
    /// slave open without writing (which would otherwise park the reader in
    /// `read` forever, leaking the thread and the fd).
    #[cfg(target_os = "linux")]
    _reader_stop: std::os::fd::OwnedFd,
    exited: Arc<AtomicBool>,
    /// Kills the shell child on `Drop`. The child itself lives on the waiter
    /// thread (which owns `wait()` and reaps it); Drop only signals the kill so
    /// the event loop never blocks on the grace period. `killer.kill()` sends a
    /// single SIGHUP — see `Drop` for the SIGKILL escalation that backs it up.
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// The shell's PID, captured before the child moved to the waiter thread.
    /// `Drop` uses it (unix) to escalate to SIGKILL when the shell ignores the
    /// initial SIGHUP, so a HUP-ignoring shell can't leak its process plus the
    /// blocked waiter/reader threads and master fd forever.
    pid: Option<u32>,
    /// Feeds the dedicated WRITER thread (see `writer()`): the UI thread sends
    /// byte buffers here and the thread — which owns the blocking Write half —
    /// performs the actual fd writes. Kept on the session so `writer()` can be
    /// called any number of times; the thread exits (dropping the Write half)
    /// once every sender clone is gone or a write fails (child exited → EIO).
    write_tx: Sender<Vec<u8>>,
    /// Bytes currently queued to the writer thread but not yet written to the
    /// fd. Shared with every [`ChannelWriter`] so writes past
    /// [`PTY_WRITE_QUEUE_CAP`] are dropped instead of growing the queue to OOM
    /// (F13). The writer thread decrements it as it drains each chunk.
    write_queued: Arc<AtomicUsize>,
    /// One-line notice to surface in the terminal when the configured shell
    /// override could not be launched and `spawn` fell back to another shell
    /// (F2). `None` when the requested/auto-detected shell started normally.
    startup_notice: Option<String>,
}

/// `Write` adapter handed to the app: forwards buffers to the PTY writer
/// thread over an unbounded channel, so a caller on the UI thread NEVER blocks
/// on a full kernel PTY buffer (e.g. pasting into a program that doesn't read
/// stdin used to freeze the whole event loop inside `write_all`). Per-session
/// write ordering is preserved: one channel, one consumer thread. `flush()` is
/// a no-op — the writer thread flushes after every chunk.
struct ChannelWriter {
    tx: Sender<Vec<u8>>,
    /// Shared byte counter (see [`PtySession::write_queued`]).
    queued: Arc<AtomicUsize>,
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Bound the queue: once more than PTY_WRITE_QUEUE_CAP bytes are pending
        // (the child has stopped reading and something is flooding the queue),
        // drop this buffer rather than grow toward OOM. We report it as fully
        // written so a hostile query-reply loop can't turn into an error storm
        // either; normal writes never reach the cap. This keeps the never-block
        // guarantee for legitimate writes while making the flood self-limiting.
        let queued = self.queued.load(Ordering::Relaxed);
        if queued.saturating_add(buf.len()) > PTY_WRITE_QUEUE_CAP {
            return Ok(buf.len());
        }
        self.queued.fetch_add(buf.len(), Ordering::Relaxed);
        self.tx.send(buf.to_vec()).map_err(|_| {
            // Roll back the reservation; the consumer is gone so nothing will
            // decrement it otherwise.
            self.queued.fetch_sub(buf.len(), Ordering::Relaxed);
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pty writer thread closed",
            )
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Two-stage reap so closing a tab (or `exit` / Ctrl+D) never leaks a
        // `<defunct>` zombie or a blocked waiter thread — previously the child's
        // Drop neither killed nor waited, leaking a PID slot per closed tab.
        // Stage 1: SIGHUP now (via `killer`); a well-behaved shell exits, which
        // makes the waiter thread's `child.wait()` return and reap it without
        // blocking the event loop. The shell is the session leader, so its exit
        // makes the kernel SIGHUP the terminal's foreground job (vim/top/build).
        // The master side is only hung up once EVERY dup of the master fd is
        // closed: `self.master` drops with this struct, the writer thread drops
        // its dup when the last writer goes, and on Linux the reader thread is
        // told to exit (the `_reader_stop` pipe closes with this struct) so its
        // dup closes too — then any disowned process still holding the slave sees
        // the hangup instead of pinning the reader in `read` forever. (On other
        // platforms the reader leaves only when the slave's last holder exits.)
        // `kill()` on an already-exited child is ignored.
        let _ = self.killer.kill();
        // Stage 2: if the shell IGNORES SIGHUP (`trap '' HUP`) and is still
        // unreaped after a grace period, escalate to an uncatchable SIGKILL so it
        // can't leak forever (portable-pty's cloned killer only sends SIGHUP, so
        // we restore the escalation the owning `Child::kill` used to provide).
        // Guarded by `exited` — which the waiter sets only AFTER it reaps — so we
        // never signal a PID that was reaped and possibly recycled. Detached
        // thread so Drop never blocks.
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            let exited = Arc::clone(&self.exited);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if !exited.load(Ordering::SeqCst) {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
            });
        }
    }
}

/// Ordered list of shell candidates to try, most-preferred first:
/// 1. the explicit `shell` config override, when non-empty;
/// 2. `$SHELL` (the conventional source), when set & non-empty;
/// 3. the current user's login shell from the passwd database (so a user who
///    `chsh`'d to zsh works even when `$SHELL` is unset in a GUI launch);
/// 4. `/bin/bash`, then `/bin/sh` as last resorts.
///
/// `spawn` walks this list and launches the first candidate that actually
/// starts, so a persisted override that no longer exists (the shell was
/// uninstalled or moved) can no longer brick startup — it falls back to a
/// working shell instead of failing to open any window (F2). Duplicates and
/// empties are dropped so the fallback chain is tried at most once each.
fn shell_candidates(override_shell: Option<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(s) = override_shell {
        push(s);
    }
    if let Ok(s) = std::env::var("SHELL") {
        push(s);
    }
    if let Some(s) = passwd_shell() {
        push(s);
    }
    push("/bin/bash".to_string());
    push("/bin/sh".to_string());
    out
}

/// The current user's login shell (`pw_shell`) from the passwd database, or
/// `None` if it can't be resolved. One-shot at spawn; `getpwuid` returns a
/// pointer into a static buffer, copied out immediately.
#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let sh = (*pw).pw_shell;
        if sh.is_null() {
            return None;
        }
        CStr::from_ptr(sh).to_str().ok().map(str::to_string)
    }
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

/// The user's home directory: `$HOME`, else the passwd `pw_dir`. Used to start
/// GUI-launched shells in home instead of the filesystem root (see `spawn`).
fn home_dir() -> Option<String> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    passwd_home()
}

/// The current user's home (`pw_dir`) from the passwd database, mirroring
/// `passwd_shell` — a GUI launch can have `$HOME` present but this is the same
/// authoritative source used for the login shell.
#[cfg(unix)]
fn passwd_home() -> Option<String> {
    use std::ffi::CStr;
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let dir = (*pw).pw_dir;
        if dir.is_null() {
            return None;
        }
        CStr::from_ptr(dir).to_str().ok().map(str::to_string)
    }
}

#[cfg(not(unix))]
fn passwd_home() -> Option<String> {
    None
}

/// Current working directory of a live process by PID, or `None` when it
/// can't be read (process gone, permission, unsupported OS) or the directory
/// no longer exists.
#[cfg(target_os = "linux")]
fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    // /proc resolves symlinks; a deleted cwd reads as "/path (deleted)" and
    // fails the is_dir filter.
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().filter(|p| p.is_dir())
}

#[cfg(target_os = "macos")]
fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is sized and aligned for proc_vnodepathinfo; the
    // kernel writes at most `size` bytes and returns the count written, so
    // ret == size proves the struct is fully initialized.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if ret != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    // SAFETY: vip_path is declared [[c_char; 32]; 32] (libc flattens the C
    // char[MAXPATHLEN] to dodge an old-rustc array limit) — 1024 contiguous
    // bytes, NUL-terminated by the kernel. Read it as one flat buffer.
    let bytes = unsafe {
        std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast::<u8>(), 1024)
    };
    let len = bytes.iter().position(|&b| b == 0)?;
    if len == 0 {
        return None;
    }
    let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..len]));
    if path.is_dir() { Some(path) } else { None }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    let _ = pid;
    None
}

/// Variables JeTTY's OWN launch environment may carry that must NOT reach the
/// shells it spawns (alacritty strips the first two for the same reason).
const INHERITED_ENV_DENYLIST: &[&str] = &[
    // One-shot window-activation tokens addressed to JeTTY's first window; a
    // GUI app started from a JeTTY shell would replay a stale token and the
    // compositor's focus-stealing prevention may then open it unfocused.
    "DESKTOP_STARTUP_ID",
    "XDG_ACTIVATION_TOKEN",
    // The AppImage runtime describing JeTTY's own mount: tools inside the shell
    // would think THEY are the JeTTY AppImage (JETTY_BIN carries the path).
    "APPIMAGE",
    "APPDIR",
    "ARGV0",
    "OWD",
    // Another terminal's identity, inherited when JeTTY was started from inside
    // it: programs would talk that terminal's private protocols (or its
    // window/socket) from within JeTTY.
    "WINDOWID",
    "TERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "VTE_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_LOG",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_PROFILE_NAME",
    "KONSOLE_VERSION",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "TERMINATOR_UUID",
    "TERMINATOR_DBUS_NAME",
    "TERMINATOR_DBUS_PATH",
    "TILIX_ID",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    // A multiplexer JeTTY was started under: JeTTY's shells are NOT inside it,
    // so `tmux`/`screen` commands must not target the outer session.
    "TMUX",
    "TMUX_PANE",
    "STY",
    // The launching terminal's size: stale for JeTTY's grid, and some programs
    // (e.g. Python's shutil.get_terminal_size) prefer these over TIOCGWINSZ.
    "COLUMNS",
    "LINES",
];

/// The path a shell should use to re-invoke JeTTY (`$JETTY_BIN`). Under an
/// AppImage `current_exe()` is the transient `/tmp/.mount_*` path that vanishes
/// with the app, so the stable `$APPIMAGE` file wins when set.
fn jetty_bin_path() -> Option<std::ffi::OsString> {
    std::env::var_os("APPIMAGE")
        .filter(|p| !p.is_empty())
        .or_else(|| std::env::current_exe().ok().map(std::path::PathBuf::into_os_string))
}

/// Whether `path` is an executable file (macOS login-shell pre-check: the
/// default-program builder silently substitutes the passwd shell for a
/// non-executable `$SHELL`, which would defeat the candidate fallback chain).
#[cfg(target_os = "macos")]
fn is_executable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path; access(2) only reads it.
    std::path::Path::new(path).is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
}

/// Block until the reader's master fd is readable (or hung up / in error —
/// `read` then reports which) — returns `false` when the session's shutdown
/// pipe fired instead. Linux only: macOS `poll(2)` does not support devices.
#[cfg(target_os = "linux")]
fn wait_readable(fd: std::os::fd::RawFd, stop: std::os::fd::RawFd) -> bool {
    loop {
        let mut fds = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stop, events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two initialized pollfd entries; nfds matches the array length.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            // Unexpected poll failure: let the read surface the real state.
            return true;
        }
        // Any event on the stop pipe (its write end closed) means shutdown.
        return fds[1].revents == 0;
    }
}

/// The reader's own fd for the master plus the session's shutdown pipe
/// `(reader, pipe read end, pipe write end)` (Linux). The reader polls both, so
/// dropping the write end (the session) ends the thread without any output.
#[cfg(target_os = "linux")]
fn linux_reader_fds(
    master: &dyn portable_pty::MasterPty,
) -> std::io::Result<(std::fs::File, std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let master_fd = master
        .as_raw_fd()
        .ok_or_else(|| std::io::Error::other("pty master has no fd"))?;
    // SAFETY: duplicating a live fd we don't own; the result is a fresh fd that
    // `File` takes ownership of (or -1, checked).
    let dup = unsafe { libc::fcntl(master_fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `dup` is a valid fd owned by nobody else.
    let reader = unsafe { std::fs::File::from_raw_fd(dup) };
    let mut pipe = [0; 2];
    // SAFETY: `pipe` has room for the two fds pipe2 writes on success.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both fds are valid and owned by us alone.
    let (stop_rd, stop_wr) = unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    Ok((reader, stop_rd, stop_wr))
}

/// Queue one chunk for the UI and wake it on the latch's false→true edge.
/// Returns `false` when the session is gone (the receiver dropped).
fn push_output(
    tx: &SyncSender<Vec<u8>>,
    shared: &ReadShared,
    wake: &Wake,
    chunk: Vec<u8>,
) -> bool {
    let n = chunk.len();
    // Count BEFORE sending so a receiver can never subtract first (underflow),
    // and so `rearm_wake` sees the bytes no later than the latch check below.
    shared.queued.fetch_add(n, Ordering::SeqCst);
    // Blocks while the queue is full: that is the backpressure (see
    // PTY_READ_QUEUE_CHUNKS). Errs only once the receiver is gone.
    if tx.send(chunk).is_err() {
        shared.queued.fetch_sub(n, Ordering::SeqCst);
        return false;
    }
    // Wake the app IMMEDIATELY on the first chunk since its last re-arm, so
    // query replies (\e[6n CPR etc.) go back to the shell within ~1ms, well
    // inside p10k's timeout; later chunks ride the wake already pending.
    if !shared.wake_pending.swap(true, Ordering::SeqCst) {
        (*wake.lock().unwrap())();
    }
    true
}

impl PtySession {
    /// Spawn a PTY running the user's shell.
    ///
    /// `on_data` wakes the application's event loop: the reader thread calls it
    /// on the FIRST chunk after each [`PtySession::rearm_wake`] (later chunks
    /// coalesce into that pending wake), once more on EOF/error, and the waiter
    /// thread calls it when the shell exits. Use it to schedule a drain
    /// immediately so query replies (DSR/DA/etc.) go back to the shell within
    /// ~1ms instead of waiting for a polling tick.
    ///
    /// `shell_override` is the `shell` config key: when non-empty it wins over
    /// every auto-detection, so a user whose login shell (`$SHELL`/passwd) is
    /// bash but who lives in zsh can set `shell = "/usr/bin/zsh"`.
    ///
    /// `cwd` is the directory the new shell should start in — typically the
    /// requesting tab's shell cwd sampled at action time. `None` (or a
    /// directory that has since vanished) starts the shell in the home
    /// directory; one that exists but can't be entered falls back to home too,
    /// with a [`PtySession::startup_notice`] saying so.
    pub fn spawn(
        cols: u16,
        rows: u16,
        px_w: u16,
        px_h: u16,
        shell_override: Option<String>,
        cwd: Option<std::path::PathBuf>,
        on_data: impl Fn() + Send + 'static,
    ) -> std::io::Result<PtySession> {
        let pty_system = native_pty_system();
        // Report the text-area pixel size (TIOCGWINSZ ws_xpixel/ws_ypixel) so
        // image tools that read it (as a fallback to the \e[14t reply) scale to
        // the real cell metrics. 0 = unknown (provisional spawn; a resize with the
        // real cell size follows).
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: px_w, pixel_height: px_h })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // A vanished directory silently degrades to existing behavior;
        // portable-pty re-guards (non-dir → home) at exec time.
        let cwd = cwd.filter(|d| d.is_dir());

        // The shell the caller explicitly requested (config `shell` override),
        // remembered so we can tell whether the launch fell back to another one.
        let requested = shell_override.clone().filter(|s| !s.is_empty());
        let candidates = shell_candidates(shell_override);
        let jetty_bin = jetty_bin_path();

        // Build a fully-configured CommandBuilder for a given shell path and start
        // directory. Kept as a closure so every fallback candidate gets the
        // identical environment.
        let make_cmd = |shell: &str, cwd: Option<&std::path::Path>| {
            // macOS terminals start LOGIN shells (argv0 "-zsh") so /etc/zprofile
            // runs path_helper and ~/.zprofile sets up Homebrew/rustup — without
            // it a Dock/Finder-launched JeTTY has a bare PATH. portable-pty's
            // default-program builder does exactly that for the shell named by
            // `SHELL`. Linux keeps the conventional non-login interactive shell.
            #[cfg(target_os = "macos")]
            let mut cmd = {
                let mut cmd = CommandBuilder::new_default_prog();
                cmd.env("SHELL", shell);
                cmd
            };
            #[cfg(not(target_os = "macos"))]
            let mut cmd = CommandBuilder::new(shell);
            for key in INHERITED_ENV_DENYLIST {
                cmd.env_remove(key);
            }
            // Advertise a capable terminal so shells (and prompts like p10k) run
            // their capability probes and emit truecolor; without TERM set, those
            // capability checks fail and the prompt renders the red "x".
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            // Disable macOS's shell-session save/restore (/etc/zshrc writes
            // ~/.zsh_sessions/<id>.session and sources it on the next launch). A
            // window-close can interrupt the save, leaving a malformed file that
            // the next shell tries to run — e.g. `command not found: Saving`.
            // JeTTY is a quick-summon terminal; session restore isn't wanted.
            // Harmless/ignored on Linux, so set it unconditionally.
            cmd.env("SHELL_SESSIONS_DISABLE", "1");
            // Shell-integration handshake (OSC 133). Advertise JeTTY so an opt-in
            // rc snippet can activate ONLY under JeTTY and feature-detect it, and
            // hand the shell an absolute path to this exe so the snippet needs no
            // PATH lookup: `source <($JETTY_BIN --print-shell-integration zsh)`.
            let ver = advertised_version();
            cmd.env("TERM_PROGRAM", "jetty");
            cmd.env("TERM_PROGRAM_VERSION", &ver);
            cmd.env("JETTY", &ver);
            if let Some(exe) = &jetty_bin {
                cmd.env("JETTY_BIN", exe);
            }
            // An explicit cwd (inherited from the requesting tab) wins.
            // Otherwise the shell starts in home: portable-pty's own default for
            // a builder without a cwd. The explicit `/` check below predates that
            // and keeps GUI launches (Finder/Dock/.desktop start the app in `/`)
            // out of the filesystem root even if that default ever changes.
            if let Some(dir) = cwd {
                cmd.cwd(dir);
            } else if std::env::current_dir()
                .map(|p| p == std::path::Path::new("/"))
                .unwrap_or(false)
            {
                if let Some(home) = home_dir() {
                    cmd.cwd(home);
                }
            }
            cmd
        };

        let spawn_one = |shell: &str, cwd: Option<&std::path::Path>| {
            #[cfg(target_os = "macos")]
            if !is_executable(shell) {
                return Err(format!("{shell} is not an executable file"));
            }
            pair.slave.spawn_command(make_cmd(shell, cwd)).map_err(|e| e.to_string())
        };

        // Try each candidate until one actually spawns. A persisted override
        // that no longer exists on disk (uninstalled/moved) must NOT prevent a
        // usable window — fall through to $SHELL/passwd/bash/sh instead (F2).
        let mut child = None;
        let mut launched_shell = String::new();
        let mut last_err = None;
        let mut cwd_notice = None;
        for shell in &candidates {
            match spawn_one(shell, cwd.as_deref()) {
                Ok(c) => {
                    child = Some(c);
                    launched_shell = shell.clone();
                    break;
                }
                Err(e) => {
                    // An inherited directory can pass `is_dir` yet refuse chdir(2)
                    // (permissions changed, e.g. mode 000): then EVERY candidate
                    // fails for the same reason and the new tab silently never
                    // opened. Retry this same shell in the default start dir
                    // before blaming the shell, and say where it landed.
                    if let Some(dir) = cwd.as_deref() {
                        if let Ok(c) = spawn_one(shell, None) {
                            cwd_notice = Some(format!(
                                "jetty: could not open the shell in \"{}\" ({e}) — started in your home directory instead.",
                                dir.display()
                            ));
                            child = Some(c);
                            launched_shell = shell.clone();
                            break;
                        }
                    }
                    last_err = Some(e);
                }
            }
        }
        let mut child = match child {
            Some(c) => c,
            None => {
                return Err(std::io::Error::other(
                    last_err.unwrap_or_else(|| "no shell could be spawned".to_string()),
                ));
            }
        };
        drop(pair.slave);

        // If the user asked for a specific shell and we ended up on a different
        // one, surface a one-line notice so the fallback is not silent; same for
        // a start directory that could not be entered.
        let shell_notice = match requested {
            Some(req) if req != launched_shell => Some(format!(
                "jetty: shell \"{req}\" could not be started — using \"{launched_shell}\" instead.",
            )),
            _ => None,
        };
        let startup_notice = match (shell_notice, cwd_notice) {
            (Some(a), Some(b)) => Some(format!("{a}\r\n{b}")),
            (a, b) => a.or(b),
        };

        // Dedicated WRITER thread (mirrors the reader thread below): it owns
        // the blocking Write half of the master; the UI thread only ever sends
        // buffers over the unbounded channel, so a full kernel PTY input buffer
        // (a big paste into `sleep 300`) can no longer freeze the winit event
        // loop — the blocking write_all happens here instead. Ordering is
        // preserved (single channel → single consumer). The loop ends when all
        // senders drop (session + writers gone) or a write errors (child
        // exited → EIO); either way the Write half drops and closes cleanly.
        let mut pty_writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                // Reap the child we just spawned before bailing (realistic under
                // fd exhaustion), or its Drop — which neither kills nor waits —
                // leaves a `<defunct>` zombie for the life of the process.
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(e.to_string()));
            }
        };
        let (write_tx, write_rx) = channel::<Vec<u8>>();
        let write_queued = Arc::new(AtomicUsize::new(0));
        let write_queued_thread = Arc::clone(&write_queued);
        std::thread::spawn(move || {
            while let Ok(chunk) = write_rx.recv() {
                let n = chunk.len();
                let write_ok = pty_writer.write_all(&chunk).is_ok();
                // Release the reservation as soon as the bytes leave the queue,
                // whether or not the fd write succeeded (a failure ends the loop).
                write_queued_thread.fetch_sub(n, Ordering::Relaxed);
                if !write_ok {
                    break;
                }
                let _ = pty_writer.flush();
            }
        });

        // The reader's own handle on the master. Linux: a dup we poll together
        // with a shutdown pipe (see `_reader_stop`); elsewhere portable-pty's
        // blocking reader.
        #[cfg(target_os = "linux")]
        let reader_fds = linux_reader_fds(pair.master.as_ref());
        #[cfg(not(target_os = "linux"))]
        let reader_fds = pair.master.try_clone_reader().map_err(|e| std::io::Error::other(e.to_string()));
        #[cfg(target_os = "linux")]
        let (mut reader, stop_rd, reader_stop) = match reader_fds {
            Ok(fds) => fds,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        #[cfg(not(target_os = "linux"))]
        let mut reader = match reader_fds {
            Ok(r) => r,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let (tx, rx) = sync_channel::<Vec<u8>>(PTY_READ_QUEUE_CHUNKS);
        let exited = Arc::new(AtomicBool::new(false));
        let read = Arc::new(ReadShared {
            queued: AtomicUsize::new(0),
            wake_pending: AtomicBool::new(false),
        });
        // Shared so the reader thread (coalesced output / EOF wakes), the waiter
        // thread (exit wake) and `rearm_wake` can all drive it. `spawn`'s
        // `on_data` is only `Send`, so it can't be cloned across threads
        // directly; the `Mutex` makes it shareable.
        let wake: Wake = Arc::new(Mutex::new(Box::new(on_data)));

        let wake_reader = Arc::clone(&wake);
        let read_reader = Arc::clone(&read);
        std::thread::spawn(move || {
            #[cfg(target_os = "linux")]
            let (reader_fd, stop_fd) = {
                use std::os::fd::AsRawFd;
                (reader.as_raw_fd(), stop_rd.as_raw_fd())
            };
            let mut buf = vec![0u8; PTY_READ_CHUNK];
            loop {
                // Session dropped (shutdown pipe closed): stop without waiting
                // for output that may never come.
                #[cfg(target_os = "linux")]
                if !wait_readable(reader_fd, stop_fd) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if !push_output(&tx, &read_reader, &wake_reader, buf[..n].to_vec()) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Ok(_) | Err(_) => {
                        // EOF/error on the master: the slave's last fd closed.
                        // This is NOT by itself proof the shell exited — a job
                        // that redirects all its std fds away
                        // (`exec >/dev/null 2>&1 </dev/null`) triggers EIO here
                        // while the shell is still alive — so we do NOT flag
                        // `exited`; the waiter thread's `child.wait()` is the
                        // authoritative exit signal. Wake the app once (not
                        // coalesced: it must see the EOF) and stop.
                        (*wake_reader.lock().unwrap())();
                        break;
                    }
                }
            }
        });

        // Authoritative exit detection, wait-based like xterm/kitty: block on the
        // child until the shell process actually dies — even when a background
        // job that inherited the slave keeps the master open, so the reader never
        // sees EOF — then reap it, flag `exited`, and wake the app so it closes
        // the tab. `Drop` kills via `killer`, which makes this `wait()` return.
        let killer = child.clone_killer();
        let pid = child.process_id();
        let exited_waiter = Arc::clone(&exited);
        let wake_waiter = Arc::clone(&wake);
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            exited_waiter.store(true, Ordering::SeqCst);
            (*wake_waiter.lock().unwrap())();
        });

        Ok(PtySession {
            master: Arc::new(Mutex::new(pair.master)),
            output: OutputQueue::new(rx, read),
            #[cfg(target_os = "linux")]
            _reader_stop: reader_stop,
            exited,
            killer,
            pid,
            write_tx,
            write_queued,
            startup_notice,
        })
    }

    /// A one-line notice describing a shell fallback (the configured `shell`
    /// override could not be launched), or `None` when the intended shell
    /// started normally. The app surfaces this in the fresh terminal (F2).
    pub fn startup_notice(&self) -> Option<&str> {
        self.startup_notice.as_deref()
    }

    /// Feed queued output to `f`, oldest chunk first, until the queue is empty or
    /// at least `budget` bytes were delivered; returns the bytes delivered. The
    /// budget keeps one flooding tab from monopolizing an event-loop pass —
    /// whatever it leaves behind is scheduled by [`PtySession::rearm_wake`].
    pub fn drain_output(&self, budget: usize, f: impl FnMut(&[u8])) -> usize {
        self.output.drain(budget, f)
    }

    /// Take the next queued output chunk, if any (polling callers: tests, the
    /// headless shot). Event-driven callers use [`PtySession::drain_output`].
    pub fn try_recv_output(&self) -> Option<Vec<u8>> {
        self.output.try_recv()
    }

    /// Wait up to `timeout` for the next output chunk (`None` on timeout, or
    /// once the reader is gone and the queue is empty).
    pub fn recv_output_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.output.recv_timeout(timeout)
    }

    /// Re-arm the output wake. The event loop MUST call this for every session
    /// once per iteration, AFTER that iteration's drains (`about_to_wait`);
    /// returns `true` when output is still queued and the caller must schedule
    /// one more drain (one wake for the whole app — the latch stays set, so the
    /// reader won't send its own).
    ///
    /// Why not clear the latch at the start of each drain: winit delivers every
    /// user event sent while it is dispatching them within the SAME iteration,
    /// so under a flood each drain would trigger the next one in turn and the
    /// iteration would never reach keyboard input or a redraw (a `yes` flood you
    /// could not Ctrl+C). Re-arming here instead turns the continuation into a
    /// wake that is delivered in the next iteration, after pending input — and
    /// while idle, the cleared latch makes the next chunk wake us immediately.
    pub fn rearm_wake(&self) -> bool {
        self.output.rearm()
    }

    /// Whether the shell child has exited. A dedicated waiter thread blocks on
    /// `child.wait()` and sets this the moment the shell process dies — even if a
    /// background job that inherited the slave keeps the PTY master open (so the
    /// reader never sees EOF), and NOT prematurely when a live shell merely
    /// redirects its std fds away (which EOFs the master). The app polls this
    /// after draining the output to close the window instead of freezing on a
    /// dead shell.
    pub fn child_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// CWD of the shell process (not its foreground child), or `None` when it
    /// exited or can't be read; callers use it to spawn sibling shells in the
    /// same directory. The exit guard (set only after the waiter reaps)
    /// ensures we never read a recycled PID's cwd.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        if self.child_exited() {
            return None;
        }
        self.pid.and_then(pid_cwd)
    }

    /// Returns a writer for the PTY (send keystrokes to the shell).
    ///
    /// The returned writer NEVER blocks the caller: bytes are queued to the
    /// session's dedicated writer thread (which owns the blocking fd), so the
    /// UI/event-loop thread can't be frozen by a full kernel PTY buffer.
    /// Ordering across all writers of one session is preserved. `flush()` is a
    /// no-op (the writer thread flushes each chunk). May be called any number
    /// of times.
    pub fn writer(&self) -> Box<dyn Write + Send> {
        Box::new(ChannelWriter {
            tx: self.write_tx.clone(),
            queued: Arc::clone(&self.write_queued),
        })
    }

    pub fn resize(&self, cols: u16, rows: u16, px_w: u16, px_h: u16) {
        let _ = self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: px_w,
            pixel_height: px_h,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_writer(tx: Sender<Vec<u8>>) -> ChannelWriter {
        ChannelWriter { tx, queued: Arc::new(AtomicUsize::new(0)) }
    }

    /// A reader-side half (sender + shared state + wake that counts into a
    /// channel) and the UI-side queue, wired exactly like `PtySession::spawn`.
    fn output_pair(
        cap: usize,
    ) -> (SyncSender<Vec<u8>>, Arc<ReadShared>, Wake, Receiver<()>, OutputQueue) {
        let (tx, rx) = sync_channel::<Vec<u8>>(cap);
        let shared = Arc::new(ReadShared {
            queued: AtomicUsize::new(0),
            wake_pending: AtomicBool::new(false),
        });
        let (wtx, wrx) = channel::<()>();
        let wake: Wake = Arc::new(Mutex::new(Box::new(move || {
            let _ = wtx.send(());
        })));
        let q = OutputQueue::new(rx, Arc::clone(&shared));
        (tx, shared, wake, wrx, q)
    }

    #[test]
    fn first_chunk_wakes_and_the_rest_coalesce_until_rearm() {
        let (tx, shared, wake, wakes, q) = output_pair(64);
        for _ in 0..10 {
            assert!(push_output(&tx, &shared, &wake, vec![b'x'; 100]));
        }
        assert_eq!(wakes.try_iter().count(), 1, "10 chunks, ONE wake while it is pending");
        assert_eq!(q.drain(usize::MAX, |_| {}), 1000);
        assert!(!q.rearm(), "fully drained: nothing left to schedule");
        assert!(push_output(&tx, &shared, &wake, b"next".to_vec()));
        assert_eq!(wakes.try_iter().count(), 1, "after a re-arm the next chunk wakes at once");
    }

    #[test]
    fn budget_hit_with_backlog_guarantees_another_wake() {
        // A drain that stops at its budget leaves output queued; the reader may
        // push nothing more (or be parked on a full queue), so the re-arm at the
        // end of the iteration MUST ask for another drain — every iteration —
        // until the backlog is gone.
        let (tx, shared, wake, wakes, q) = output_pair(64);
        for _ in 0..8 {
            assert!(push_output(&tx, &shared, &wake, vec![b'y'; 1000]));
        }
        assert_eq!(wakes.try_iter().count(), 1);
        let mut drained = q.drain(2500, |_| {});
        assert!((2500..8000).contains(&drained), "the budget stopped the drain");
        assert!(q.rearm(), "backlog after a budget hit → caller must schedule a drain");
        assert!(q.rearm(), "and keeps asking while nothing drained it");
        while q.rearm() {
            drained += q.drain(2500, |_| {});
        }
        assert_eq!(drained, 8000, "every byte arrives through the scheduled drains");
        assert_eq!(wakes.try_iter().count(), 0, "the reader stayed quiet: the latch was held");
    }

    #[test]
    fn rearm_never_loses_a_wakeup_under_concurrent_output() {
        // Stress the reader/UI race: a producer pushes many small chunks with
        // irregular gaps while a consumer that ONLY drains when woken (no
        // polling, small budget) re-arms after each pass. A lost wakeup would
        // strand bytes with nobody scheduled to drain them → the wait times out.
        const CHUNKS: usize = 50_000;
        let (tx, shared, wake, wakes, q) = output_pair(256);
        let (self_tx, all_wakes) = channel::<()>();
        // Funnel reader wakes and self-scheduled wakes into one "event loop".
        let fwd = self_tx.clone();
        std::thread::spawn(move || {
            for _ in wakes {
                if fwd.send(()).is_err() {
                    break;
                }
            }
        });
        let producer = std::thread::spawn(move || {
            let mut seed: u32 = 0x9e37_79b9;
            for i in 0..CHUNKS {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let len = 1 + (seed % 64) as usize;
                assert!(push_output(&tx, &shared, &wake, vec![(i % 251) as u8; len]));
                if seed.is_multiple_of(97) {
                    std::thread::sleep(Duration::from_micros(50));
                } else if seed.is_multiple_of(7) {
                    std::thread::yield_now();
                }
            }
        });
        let mut total = 0usize;
        let mut chunks = 0usize;
        loop {
            match all_wakes.recv_timeout(Duration::from_secs(5)) {
                Ok(()) => {
                    // winit-style: one drain per delivered wake, budget-limited.
                    total += q.drain(4096, |c| {
                        chunks += 1;
                        let _ = c;
                    });
                    // End of iteration (about_to_wait).
                    if q.rearm() {
                        let _ = self_tx.send(());
                    }
                }
                Err(_) => {
                    assert!(
                        producer.is_finished() && q.try_recv().is_none(),
                        "stalled with output pending: a wakeup was lost"
                    );
                    break;
                }
            }
            if producer.is_finished() && chunks == CHUNKS {
                break;
            }
        }
        producer.join().unwrap();
        assert_eq!(chunks, CHUNKS, "every chunk delivered exactly once");
        assert!(total > 0);
    }

    #[test]
    fn full_queue_blocks_the_reader_until_the_ui_drains() {
        // Backpressure: with nobody draining, the reader parks after `cap`
        // chunks (bounded memory) instead of queueing without limit, and resumes
        // as soon as the UI takes something.
        let cap = 16;
        let (tx, shared, wake, _wakes, q) = output_pair(cap);
        let pushed = Arc::new(AtomicUsize::new(0));
        let pushed_p = Arc::clone(&pushed);
        let producer = std::thread::spawn(move || {
            for _ in 0..cap + 4 {
                if !push_output(&tx, &shared, &wake, vec![0u8; 8]) {
                    break;
                }
                pushed_p.fetch_add(1, Ordering::SeqCst);
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(pushed.load(Ordering::SeqCst), cap, "reader must block on the full queue");
        // Exactly the queued bytes (the budget stops before chunks the freshly
        // unblocked reader adds meanwhile).
        assert_eq!(q.drain(cap * 8, |_| {}), cap * 8);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while pushed.load(Ordering::SeqCst) < cap + 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pushed.load(Ordering::SeqCst), cap + 4, "reader resumes after a drain");
        producer.join().unwrap();
    }

    #[test]
    fn reader_stops_when_the_session_is_gone() {
        // Dropping the UI end (the session) unblocks a reader parked on the full
        // queue: push_output reports the receiver gone instead of hanging.
        let (tx, shared, wake, _wakes, q) = output_pair(1);
        assert!(push_output(&tx, &shared, &wake, vec![1]));
        let producer = std::thread::spawn(move || push_output(&tx, &shared, &wake, vec![2]));
        std::thread::sleep(Duration::from_millis(50));
        drop(q);
        assert!(!producer.join().unwrap(), "send into a dropped queue must fail, not hang");
    }

    #[test]
    fn shell_candidates_always_end_with_a_working_fallback() {
        // Regression (F2): a dead override must not be the only candidate — the
        // auto-detect chain (bash/sh) is always appended so a usable shell can
        // still launch.
        let list = shell_candidates(Some("/nonexistent/fish".to_string()));
        assert_eq!(list.first().map(String::as_str), Some("/nonexistent/fish"),
            "override tried first");
        assert!(list.iter().any(|s| s == "/bin/bash" || s == "/bin/sh"),
            "fallback chain must always be present; got {list:?}");
    }

    #[test]
    fn shell_candidates_dedupe_and_drop_empties() {
        // An empty override is ignored; duplicates (e.g. $SHELL == /bin/bash)
        // are not tried twice.
        let list = shell_candidates(Some(String::new()));
        assert!(!list.iter().any(|s| s.is_empty()), "no empty candidates");
        let mut seen = std::collections::HashSet::new();
        for s in &list {
            assert!(seen.insert(s), "no duplicate candidate {s:?}");
        }
    }

    #[test]
    fn channel_writer_preserves_order() {
        // The bracketed-paste triple (prefix, payload, suffix) must arrive at
        // the writer thread in exactly the order it was written.
        let (tx, rx) = channel::<Vec<u8>>();
        let mut w = mk_writer(tx);
        w.write_all(b"\x1b[200~").unwrap();
        w.write_all(b"hello").unwrap();
        w.write_all(b"\x1b[201~").unwrap();
        w.flush().unwrap();
        let got: Vec<Vec<u8>> = rx.try_iter().collect();
        assert_eq!(
            got,
            vec![b"\x1b[200~".to_vec(), b"hello".to_vec(), b"\x1b[201~".to_vec()],
        );
    }

    #[test]
    fn channel_writer_accepts_large_writes_without_blocking() {
        // The unbounded channel queues arbitrarily large pastes even when
        // nothing consumes them yet (the C14 freeze scenario): write returns
        // immediately with the full length.
        let (tx, rx) = channel::<Vec<u8>>();
        let mut w = mk_writer(tx);
        let big = vec![b'x'; 1 << 20]; // 1 MiB, far beyond the ~64KB kernel buffer
        assert_eq!(w.write(&big).unwrap(), big.len());
        assert_eq!(rx.try_recv().unwrap().len(), 1 << 20);
    }

    #[test]
    fn channel_writer_drops_past_cap_without_blocking_or_erroring() {
        // Regression (F13): once the queue exceeds PTY_WRITE_QUEUE_CAP (the
        // child stopped reading and a reply flood keeps producing), further
        // writes are DROPPED — reported as written, never blocking, never
        // erroring — so memory stays bounded instead of growing to OOM.
        let (tx, rx) = channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut w = ChannelWriter { tx, queued: Arc::clone(&queued) };
        // Nothing consumes `rx`, so `queued` only ever grows here.
        let chunk = vec![b'q'; 1 << 20]; // 1 MiB per write
        let mut sent = 0usize;
        for _ in 0..200 {
            // Each call must return Ok(len) — never Err, never block.
            assert_eq!(w.write(&chunk).unwrap(), chunk.len());
            if queued.load(Ordering::Relaxed) >= PTY_WRITE_QUEUE_CAP - chunk.len() {
                sent += 1;
                // A few more writes past the cap must still succeed as no-ops.
                assert_eq!(w.write(&chunk).unwrap(), chunk.len());
            } else {
                sent += 1;
            }
        }
        assert!(sent > 0);
        // The actually-queued bytes never exceeded the cap.
        assert!(
            queued.load(Ordering::Relaxed) <= PTY_WRITE_QUEUE_CAP,
            "queued bytes must stay under the cap; got {}",
            queued.load(Ordering::Relaxed)
        );
        // And the messages that were enqueued sum to <= the cap.
        let total: usize = rx.try_iter().map(|v| v.len()).sum();
        assert!(total <= PTY_WRITE_QUEUE_CAP, "enqueued bytes exceeded cap");
    }

    #[test]
    fn channel_writer_errors_after_writer_thread_exit() {
        // Once the consuming side is gone (writer thread exited), writes fail
        // with BrokenPipe instead of panicking or silently vanishing.
        let (tx, rx) = channel::<Vec<u8>>();
        drop(rx);
        let mut w = mk_writer(tx);
        let err = w.write_all(b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn channel_writer_empty_write_sends_nothing() {
        let (tx, rx) = channel::<Vec<u8>>();
        let mut w = mk_writer(tx);
        assert_eq!(w.write(b"").unwrap(), 0);
        assert!(rx.try_recv().is_err(), "no message for a zero-length write");
    }

    #[test]
    fn multiple_writers_share_one_queue() {
        // writer() may now be called more than once; all clones feed the same
        // ordered queue (per-session ordering is what the terminal relies on).
        let (tx, rx) = channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut a = ChannelWriter { tx: tx.clone(), queued: Arc::clone(&queued) };
        let mut b = ChannelWriter { tx, queued };
        a.write_all(b"1").unwrap();
        b.write_all(b"2").unwrap();
        a.write_all(b"3").unwrap();
        let got: Vec<Vec<u8>> = rx.try_iter().collect();
        assert_eq!(got, vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]);
    }
}
