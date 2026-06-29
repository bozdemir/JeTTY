use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

pub struct PtySession {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    rx: Receiver<Vec<u8>>,
    exited: Arc<AtomicBool>,
    /// The shell child. `Option` so `Drop` can move it to a reaper thread.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Reap the shell child so closing a tab (or `exit` / Ctrl+D) doesn't leak a
        // `<defunct>` zombie for the life of the process — previously `_child`'s
        // Drop neither killed nor waited, so a long-lived summon terminal leaked a
        // PID slot per closed tab. We kill+wait on a DETACHED thread so the event
        // loop never blocks on the SIGKILL/wait grace period. Killing the shell
        // also closes the slave, so the reader thread hits EOF and drops its master
        // clone; with the master itself dropped, the kernel then SIGHUPs any
        // lingering foreground job (vim/top/build) on the controlling terminal.
        // `kill()` on an already-exited child errors harmlessly (ignored); `wait()`
        // still reaps it.
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// Decide which shell to launch, in priority order:
/// 1. the explicit `shell` config override, when non-empty;
/// 2. `$SHELL` (the conventional source), when set & non-empty;
/// 3. the current user's login shell from the passwd database (so a user who
///    `chsh`'d to zsh works even when `$SHELL` is unset in a GUI launch);
/// 4. `/bin/bash` as a last resort.
fn resolve_shell(override_shell: Option<String>) -> String {
    if let Some(s) = override_shell {
        if !s.is_empty() {
            return s;
        }
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }
    if let Some(s) = passwd_shell() {
        if !s.is_empty() {
            return s;
        }
    }
    "/bin/bash".to_string()
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

        let shell = resolve_shell(shell_override);
        let mut cmd = CommandBuilder::new(shell);
        // Advertise a capable terminal so shells (and prompts like p10k) run
        // their capability probes and emit truecolor; without TERM set, those
        // capability checks fail and the prompt renders the red "x".
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // Disable macOS's shell-session save/restore (/etc/zshrc writes
        // ~/.zsh_sessions/<id>.session and sources it on the next launch). A
        // window-close can interrupt the save, leaving a malformed file that the
        // next shell tries to run — e.g. `command not found: Saving`. JeTTY is a
        // quick-summon terminal; session restore isn't wanted. Harmless/ignored
        // on Linux, so set it unconditionally (no platform-specific code).
        cmd.env("SHELL_SESSIONS_DISABLE", "1");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let (tx, rx) = channel::<Vec<u8>>();
        let exited = Arc::new(AtomicBool::new(false));
        let exited_reader = Arc::clone(&exited);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // EOF or error means the shell closed the PTY (the user
                        // pressed Ctrl+D or ran `exit`). Flag it BEFORE waking the
                        // app so its post-drain child-exit check sees it and closes
                        // the window instead of hanging on a dead shell.
                        exited_reader.store(true, Ordering::SeqCst);
                        on_data();
                        break;
                    }
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        // Wake the app IMMEDIATELY so drain_pty runs and any
                        // query replies (\\e[6n CPR etc.) are written back to
                        // the shell within ~1ms, well inside p10k's timeout.
                        on_data();
                    }
                }
            }
        });

        Ok(PtySession {
            master: Arc::new(Mutex::new(pair.master)),
            rx,
            exited,
            child: Some(child),
        })
    }

    pub fn output(&self) -> &Receiver<Vec<u8>> {
        &self.rx
    }

    /// Whether the shell child has exited — the reader thread saw EOF/error on
    /// the PTY master (Ctrl+D / `exit`). The app polls this after draining the
    /// output to close the window instead of freezing on a dead shell.
    pub fn child_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Returns a writer for the PTY master (send keystrokes to the shell).
    ///
    /// # Panics
    /// `take_writer()` is one-shot: this may only be called ONCE per `PtySession`.
    /// A second call panics.
    pub fn writer(&self) -> Box<dyn Write + Send> {
        self.master.lock().unwrap().take_writer().expect("writer() can only be called once per PtySession")
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
