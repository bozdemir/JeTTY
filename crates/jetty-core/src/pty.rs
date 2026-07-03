use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

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

pub struct PtySession {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    rx: Receiver<Vec<u8>>,
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
        // `<defunct>` zombie, a blocked waiter/reader thread, or the master fd —
        // previously the child's Drop neither killed nor waited, leaking a PID
        // slot per closed tab. Stage 1: SIGHUP now (via `killer`); a well-behaved
        // shell exits, which makes the waiter thread's `child.wait()` return and
        // reap it without blocking the event loop. Killing the shell closes the
        // slave, and dropping `self.master` right after closes the master, so the
        // kernel also SIGHUPs any lingering foreground job (vim/top/build) on the
        // controlling terminal. `kill()` on an already-exited child is ignored.
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

impl PtySession {
    /// Spawn a PTY running the user's shell.
    ///
    /// `on_data` is called from the reader thread every time a chunk of bytes
    /// arrives from the PTY (and once more on EOF/error). Use it to wake the
    /// application's event loop immediately so query replies (DSR/DA/etc.) are
    /// sent back to the shell within ~1ms instead of waiting for a polling tick.
    ///
    /// `shell_override` is the `shell` config key: when non-empty it wins over
    /// every auto-detection, so a user whose login shell (`$SHELL`/passwd) is
    /// bash but who lives in zsh can set `shell = "/usr/bin/zsh"`.
    pub fn spawn(
        cols: u16,
        rows: u16,
        shell_override: Option<String>,
        on_data: impl Fn() + Send + 'static,
    ) -> std::io::Result<PtySession> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // The shell the caller explicitly requested (config `shell` override),
        // remembered so we can tell whether the launch fell back to another one.
        let requested = shell_override.clone().filter(|s| !s.is_empty());
        let candidates = shell_candidates(shell_override);

        // Build a fully-configured CommandBuilder for a given shell path. Kept as
        // a closure so every fallback candidate gets the identical environment.
        let make_cmd = |shell: &str| {
            let mut cmd = CommandBuilder::new(shell);
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
            // GUI launches (Finder/Dock/.desktop) start the app with cwd `/`;
            // unlike Terminal.app/iTerm2/kitty we don't want new shells opening
            // in the filesystem root. Only override in that case — a shell
            // launched from a terminal in a project dir keeps that directory.
            if std::env::current_dir().map(|p| p == std::path::Path::new("/")).unwrap_or(false)
            {
                if let Some(home) = home_dir() {
                    cmd.cwd(home);
                }
            }
            cmd
        };

        // Try each candidate until one actually spawns. A persisted override
        // that no longer exists on disk (uninstalled/moved) must NOT prevent a
        // usable window — fall through to $SHELL/passwd/bash/sh instead (F2).
        let mut child = None;
        let mut launched_shell = String::new();
        let mut last_err = None;
        for shell in &candidates {
            match pair.slave.spawn_command(make_cmd(shell)) {
                Ok(c) => {
                    child = Some(c);
                    launched_shell = shell.clone();
                    break;
                }
                Err(e) => last_err = Some(e.to_string()),
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
        // one, surface a one-line notice so the fallback is not silent.
        let startup_notice = match requested {
            Some(req) if req != launched_shell => Some(format!(
                "jetty: shell \"{req}\" could not be started — using \"{launched_shell}\" instead.",
            )),
            _ => None,
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

        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(e.to_string()));
            }
        };
        let (tx, rx) = channel::<Vec<u8>>();
        let exited = Arc::new(AtomicBool::new(false));
        // Shared so BOTH the reader thread (per-chunk / EOF wakes) and the waiter
        // thread (exit wake) can drive it. `spawn`'s `on_data` is only `Send`, so
        // it can't be cloned across threads directly; the `Mutex` makes it
        // shareable. Contention is nil — the waiter locks exactly once (at exit).
        let on_data = Arc::new(Mutex::new(on_data));

        let on_data_reader = Arc::clone(&on_data);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // EOF/error on the master: the slave's last fd closed.
                        // This is NOT by itself proof the shell exited — a job
                        // that redirects all its std fds away
                        // (`exec >/dev/null 2>&1 </dev/null`) triggers EIO here
                        // while the shell is still alive — so we do NOT flag
                        // `exited`; the waiter thread's `child.wait()` is the
                        // authoritative exit signal. Wake the app once and stop.
                        (*on_data_reader.lock().unwrap())();
                        break;
                    }
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        // Wake the app IMMEDIATELY so drain_pty runs and any
                        // query replies (\\e[6n CPR etc.) are written back to
                        // the shell within ~1ms, well inside p10k's timeout.
                        (*on_data_reader.lock().unwrap())();
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
        let on_data_waiter = Arc::clone(&on_data);
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            exited_waiter.store(true, Ordering::SeqCst);
            (*on_data_waiter.lock().unwrap())();
        });

        Ok(PtySession {
            master: Arc::new(Mutex::new(pair.master)),
            rx,
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

    pub fn output(&self) -> &Receiver<Vec<u8>> {
        &self.rx
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

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_writer(tx: Sender<Vec<u8>>) -> ChannelWriter {
        ChannelWriter { tx, queued: Arc::new(AtomicUsize::new(0)) }
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
