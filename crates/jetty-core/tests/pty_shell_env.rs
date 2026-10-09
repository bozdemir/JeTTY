//! `$SHELL` inside JeTTY names the shell JeTTY runs (the `shell` override or
//! a fallback) — not the login shell JeTTY inherited — so tmux, `vim :sh`,
//! `sudo -s`, mc… open the same shell as the tab. Own test binary: it sets
//! `$SHELL` in this process (one file = one process).
#![cfg(target_os = "linux")]

// Every shell here is `/bin/sh` in a scratch home — see `common`.
mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::{spawn_sh, spawn_shell};
use jetty_core::PtySession;

fn read_until(pty: &PtySession, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(chunk) = pty.recv_output_timeout(Duration::from_millis(100)) {
            seen.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&seen).contains(needle) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// The `$SHELL` a shell started by `PtySession` sees.
fn shell_var(pty: &PtySession) -> String {
    pty.writer().write_all(b"echo \"SH=[${SHELL-unset}]\"; echo DONE-SH\n").unwrap();
    let out = read_until(pty, "\nDONE-SH");
    // The echoed command line carries the unexpanded `${…}`.
    out.lines()
        .filter_map(|l| l.split_once("SH=[").map(|(_, v)| v))
        .filter_map(|v| v.split_once(']').map(|(v, _)| v))
        .find(|v| !v.contains('$'))
        .unwrap_or_else(|| panic!("no SH line in:\n{out}"))
        .to_string()
}

#[test]
fn shell_names_the_shell_that_runs() {
    // The login shell JeTTY was started with.
    std::env::set_var("SHELL", "/bin/bash");
    let pty = spawn_sh(None);
    assert_eq!(shell_var(&pty), "/bin/sh", "`shell = \"/bin/sh\"`: $SHELL follows it");

    // Not a shell — e.g. a multiplexer, which would start itself in every
    // window: $SHELL stays the login shell.
    let dir = std::env::temp_dir().join(format!("jetty-shell-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let screen = dir.join("screen");
    std::fs::write(&screen, "#!/bin/sh\nexec /bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&screen, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pty = spawn_shell(&screen.display().to_string(), None, Vec::new()).expect("spawn");
    assert_eq!(shell_var(&pty), "/bin/bash", "a non-shell program leaves $SHELL alone");
    let _ = std::fs::remove_dir_all(&dir);
}
