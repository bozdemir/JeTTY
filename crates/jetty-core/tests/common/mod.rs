//! Shared by the PTY integration tests: every shell they start is `/bin/sh` in
//! a scratch home — never the developer's `$SHELL`, rc files or history. The
//! tests type into their shell (`exit`, markers, commands); typed into the
//! developer's own zsh, that reached whatever the rc file started — with the
//! common `[ -z "$TMUX" ] && exec tmux new -A -s main` (a guard that passes,
//! since JeTTY strips `TMUX` from its shells) the developer's live tmux session,
//! where the typed `exit` closed the active pane.
#![allow(dead_code)] // each test binary uses its own subset

use std::path::PathBuf;

use jetty_core::PtySession;

/// The shell every PTY test runs.
pub const SH: &str = "/bin/sh";

/// The tests' own home: inside the target dir, with no rc files.
pub fn scratch_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pty-home");
    std::fs::create_dir_all(&home).expect("create the scratch home");
    home
}

/// Exported to every test shell on top of the inherited environment: the
/// scratch home (where a macOS login `sh` looks for `~/.profile`), and none of
/// the other ways a shell finds the developer's startup files — `$ENV` (read by
/// an interactive `sh`), `$BASH_ENV`, `$ZDOTDIR` — or their history file.
pub fn hermetic_env() -> Vec<(String, String)> {
    let home = scratch_home();
    vec![
        ("HOME".into(), home.display().to_string()),
        ("ZDOTDIR".into(), home.display().to_string()),
        ("ENV".into(), String::new()),
        ("BASH_ENV".into(), String::new()),
        ("HISTFILE".into(), home.join("history").display().to_string()),
    ]
}

/// `shell` on a fresh 80×24 PTY with [`hermetic_env`] plus `env`, started in
/// `cwd` (`None`: the scratch home).
pub fn spawn_shell(shell: &str, cwd: Option<PathBuf>, env: Vec<(String, String)>) -> std::io::Result<PtySession> {
    let mut all = hermetic_env();
    all.extend(env);
    PtySession::spawn_with_env(80, 24, 0, 0, Some(shell.into()), cwd, all, || {})
}

/// [`SH`] via [`spawn_shell`].
pub fn spawn_sh(cwd: Option<PathBuf>) -> PtySession {
    spawn_shell(SH, cwd, Vec::new()).expect("spawn /bin/sh")
}
