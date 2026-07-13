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
        if let Ok(chunk) = pty.output().recv_timeout(Duration::from_millis(200)) {
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
        while pty.output().try_recv().is_ok() {}
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
        if let Ok(chunk) = pty.output().recv_timeout(Duration::from_millis(200)) {
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
    while pty.output().try_recv().is_ok() {}
    assert!(!pty.child_exited(), "shell died after spawn with a vanished cwd");
    assert_ne!(pty.cwd(), Some(dir), "shell ended up in the deleted directory");
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
        while pty.output().try_recv().is_ok() {}
        if pty.child_exited() {
            // The exit guard must prevent reading a recycled PID's cwd.
            assert!(pty.cwd().is_none(), "cwd() returned Some for an exited shell");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("child_exited() never flipped true after the shell was told to exit");
}
