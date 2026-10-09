// Test PTY echoing. Run with SHELL=/bin/cat if the default shell does not echo:
//   SHELL=/bin/cat cargo test -p jetty-core --test pty
use jetty_core::PtySession;
use std::time::{Duration, Instant};

#[test]
fn pty_echoes_written_bytes() {
    let pty = PtySession::spawn(80, 24, 0, 0, None, None, || {}).expect("spawn");
    {
        let mut w = pty.writer();
        // cooked PTY echoes typed input back; send a line.
        use std::io::Write;
        w.write_all(b"jetty-marker\n").unwrap();
        w.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(chunk) = pty.recv_output_timeout(Duration::from_millis(200)) {
            seen.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&seen).contains("jetty-marker") {
                return; // success
            }
        }
    }
    panic!("did not observe echoed marker; got: {:?}", String::from_utf8_lossy(&seen));
}

#[test]
fn child_exit_is_detected() {
    // When the shell exits (Ctrl+D / `exit`), the reader thread sees EOF on the
    // PTY master and must flag it so the app can close the window instead of
    // freezing on a dead shell. Drive that path by telling the shell to exit.
    let pty = PtySession::spawn(80, 24, 0, 0, None, None, || {}).expect("spawn");
    {
        let mut w = pty.writer();
        use std::io::Write;
        w.write_all(b"exit\n").unwrap();
        w.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // Drain output so the shell can make progress toward exiting.
        while pty.try_recv_output().is_some() {}
        if pty.child_exited() {
            return; // success: EOF observed, flag set
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("child_exited() never flipped true after the shell was told to exit");
}

/// A unique, freshly created directory under the OS temp dir (no tempfile dep).
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir()
        .join(format!("jetty-pty-test-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn spawn_inherits_cwd() {
    let dir = unique_temp_dir("inherit");
    // macOS /tmp is a symlink to /private/tmp; compare canonicalized paths.
    let canon = std::fs::canonicalize(&dir).expect("canonicalize temp dir");
    let pty =
        PtySession::spawn(80, 24, 0, 0, None, Some(dir.clone()), || {}).expect("spawn with cwd");

    // The spawned shell must report the requested cwd via PtySession::cwd().
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut matched = false;
    while Instant::now() < deadline {
        if pty.cwd().map(|p| std::fs::canonicalize(p).ok() == Some(canon.clone()))
            == Some(true)
        {
            matched = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(matched, "pty.cwd() never reported the requested directory {canon:?}");

    // And `pwd` in the shell must echo it — proves CommandBuilder::cwd took
    // effect, not just the readback path.
    {
        let mut w = pty.writer();
        use std::io::Write;
        w.write_all(b"pwd\n").unwrap();
        w.flush().unwrap();
    }
    let leaf = dir.file_name().unwrap().to_string_lossy().into_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(chunk) = pty.recv_output_timeout(Duration::from_millis(200)) {
            seen.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&seen).contains(&leaf) {
                let _ = std::fs::remove_dir(&dir);
                return; // success
            }
        }
    }
    panic!("pwd output never contained {leaf:?}; got: {:?}", String::from_utf8_lossy(&seen));
}

#[test]
fn spawn_with_vanished_cwd_falls_back() {
    let dir = unique_temp_dir("vanished");
    std::fs::remove_dir(&dir).expect("remove temp dir");
    // A vanished cwd must degrade to the default spawn dir, not fail the tab.
    let pty =
        PtySession::spawn(80, 24, 0, 0, None, Some(dir.clone()), || {}).expect("spawn must succeed");
    std::thread::sleep(Duration::from_millis(300));
    while pty.try_recv_output().is_some() {}
    assert!(!pty.child_exited(), "shell died after spawn with a vanished cwd");
    assert_ne!(pty.cwd(), Some(dir), "shell ended up in the deleted directory");
}

/// Read output until `needle` appears (or 5 s pass); returns everything seen.
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

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn spawn_without_cwd_starts_in_home() {
    // No requested cwd → home (portable-pty's default), whatever directory the
    // app itself was started from.
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return };
    let Ok(home) = std::fs::canonicalize(home) else { return };
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut cwd = None;
    while Instant::now() < deadline {
        cwd = pty.cwd().and_then(|p| std::fs::canonicalize(p).ok());
        if cwd.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(cwd, Some(home));
}

#[cfg(unix)]
#[test]
fn unenterable_inherited_cwd_falls_back_with_a_notice() {
    // A directory can still be `is_dir()` yet refuse chdir(2) (mode 000): every
    // shell candidate then failed for the same reason and the new tab silently
    // never opened. The spawn must retry the SAME shell in the default dir and
    // say so.
    if is_root() {
        return; // root ignores the permission bits — nothing to provoke
    }
    use std::os::unix::fs::PermissionsExt;
    // The directory name is attacker-controlled (any cloned repo can carry
    // one): ESC/BEL/C1 that would write the clipboard and query the terminal.
    let dir = unique_temp_dir("locked\x1b]52;c;aGk=\x07\u{9b}6n\x1b[c");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let res = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), Some(dir.clone()), || {});
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _ = std::fs::remove_dir(&dir);
    let pty = res.expect("spawn must fall back instead of failing the tab");
    let notices = pty.startup_notices();
    assert_eq!(notices.len(), 1, "exactly the start-directory notice: {notices:?}");
    let notice = &notices[0];
    assert!(notice.contains("could not open the shell in"), "notice: {notice}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pty.child_exited(), "the fallback shell must be alive");

    // Shown the way the app shows it, the name stays text: only our own SGR
    // wrapper is a sequence, and nothing in it reaches the clipboard, the
    // title or the shell (as a query reply).
    let line = jetty_core::Terminal::notice_line(notice);
    let inner = line.strip_prefix("\x1b[33m").and_then(|s| s.strip_suffix("\x1b[0m\r\n")).unwrap();
    assert!(!inner.chars().any(char::is_control), "control bytes in the shown notice: {inner:?}");
    let mut term = jetty_core::Terminal::new(200, 4);
    term.feed_notice(notice);
    assert!(term.take_clipboard_stores().is_empty());
    assert_eq!(term.take_title_update(), None);
    assert!(term.drain_pty_writes().is_empty(), "a query in the name was answered");
}

#[cfg(unix)]
#[test]
fn launch_environment_identity_does_not_leak_into_shells() {
    // Activation tokens, the AppImage runtime and another terminal's identity in
    // JeTTY's own environment must not reach its shells. (Process env is shared
    // by the test binary's threads; these names are inert for other tests.)
    for (k, v) in [
        ("XDG_ACTIVATION_TOKEN", "stale-token"),
        ("DESKTOP_STARTUP_ID", "stale-id"),
        ("KITTY_WINDOW_ID", "7"),
        ("TMUX", "/tmp/tmux-1/default,1,0"),
        ("WINDOWID", "12345"),
        ("OWD", "/somewhere"),
    ] {
        std::env::set_var(k, v);
    }
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    {
        use std::io::Write;
        let mut w = pty.writer();
        w.write_all(b"env; echo ENV-DONE\n").unwrap();
    }
    let out = read_until(&pty, "\nENV-DONE");
    for k in ["XDG_ACTIVATION_TOKEN=", "DESKTOP_STARTUP_ID=", "KITTY_WINDOW_ID=", "TMUX=", "WINDOWID=", "OWD="] {
        assert!(!out.lines().any(|l| l.starts_with(k)), "{k} leaked into the shell:\n{out}");
    }
    assert!(out.lines().any(|l| l.starts_with("JETTY_BIN=")), "JETTY_BIN missing:\n{out}");
    assert!(out.lines().any(|l| l.starts_with("TERM_PROGRAM=jetty")), "TERM_PROGRAM:\n{out}");
}

#[test]
fn cwd_none_after_exit() {
    let pty = PtySession::spawn(80, 24, 0, 0, None, None, || {}).expect("spawn");
    {
        let mut w = pty.writer();
        use std::io::Write;
        w.write_all(b"exit\n").unwrap();
        w.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        while pty.try_recv_output().is_some() {}
        if pty.child_exited() {
            // The exit guard must prevent reading a recycled PID's cwd.
            assert!(pty.cwd().is_none(), "cwd() returned Some for an exited shell");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("child_exited() never flipped true after the shell was told to exit");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn title_cwd_and_foreground_name_follow_the_shell() {
    // Smart tab titles read the shell's cwd WITHOUT stat'ing it and the name of
    // the foreground job. Spawn `sh` in a temp dir and check the cwd; with the
    // shell idle in the foreground there is "no command"; start a `sleep` and
    // its name is reported.
    let dir = unique_temp_dir("title");
    let canon = std::fs::canonicalize(&dir).expect("canonicalize temp dir");
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".to_string()), Some(dir.clone()), || {})
        .expect("spawn sh");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut cwd_ok = false;
    while Instant::now() < deadline {
        while pty.try_recv_output().is_some() {}
        if pty.title_cwd().and_then(|p| std::fs::canonicalize(p).ok()) == Some(canon.clone()) {
            cwd_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cwd_ok, "title_cwd never reported {canon:?}");
    assert_eq!(pty.foreground_name(), None, "the idle shell is not a command");
    {
        let mut w = pty.writer();
        use std::io::Write;
        w.write_all(b"sleep 3\n").unwrap();
        w.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut name = None;
    while Instant::now() < deadline {
        while pty.try_recv_output().is_some() {}
        name = pty.foreground_name();
        if name.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(name.as_deref(), Some("sleep"));
    drop(pty);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Wait (≤ 5 s) for the session's shell to exit, draining its output.
fn wait_exited(pty: &PtySession) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        while pty.try_recv_output().is_some() {}
        if pty.child_exited() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
#[test]
fn a_shell_that_dies_right_after_starting_hands_over_to_the_next_one() {
    // `shell = "/bin/false"` — or a zsh whose rc file exits 1 — spawns fine,
    // then dies at once, and the app used to vanish with its last tab. The
    // next candidate takes over in the same terminal, saying why.
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/false".into()), None, || {}).expect("spawn");
    assert!(wait_exited(&pty), "premise: /bin/false exits");
    let next = pty.respawn_after_failed_start().expect("a failed start").expect("the next shell spawns");
    let notices = next.startup_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("\"/bin/false\"") && notices[0].contains("status 1"), "{notices:?}");
    assert!(!next.child_exited(), "the fallback shell is alive");
    {
        use std::io::Write;
        next.writer().write_all(b"echo REVIVED-$((40+2))\n").unwrap();
    }
    let out = read_until(&next, "REVIVED-42");
    assert!(out.contains("REVIVED-42"), "the fallback shell runs commands: {out:?}");
}

#[cfg(unix)]
#[test]
fn a_clean_exit_is_not_a_failed_start() {
    // `exit` / Ctrl+D at once is the user closing the tab, not a broken shell.
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    {
        use std::io::Write;
        pty.writer().write_all(b"exit\n").unwrap();
    }
    assert!(wait_exited(&pty), "premise: the shell exits");
    assert!(pty.respawn_after_failed_start().is_none(), "status 0: the tab just closes");
}

#[cfg(unix)]
#[test]
fn a_paste_past_the_reply_cap_still_arrives_whole() {
    // The 64 MiB queue cap exists for query-REPLY floods (a program that keeps
    // asking without reading its input); it also swallowed any paste past it
    // — nothing reached the program and nothing said so.
    const N: usize = 64 * 1024 * 1024 + 4096;
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    {
        use std::io::Write;
        // The echoed command line shows `READ''Y`; only the output says READY.
        pty.writer().write_all(format!("stty raw -echo; echo READ''Y; head -c {N} | wc -c\n").as_bytes()).unwrap();
    }
    assert!(read_until(&pty, "READY").contains("READY"), "premise: raw reader ready");
    {
        use std::io::Write;
        pty.writer().write_all(&vec![b'x'; N]).unwrap();
    }
    let want = N.to_string();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    while Instant::now() < deadline && !seen.contains(&want) {
        if let Some(chunk) = pty.recv_output_timeout(Duration::from_millis(200)) {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    assert!(seen.contains(&want), "the program must receive all {N} bytes; it printed {seen:?}");
}
