// Test PTY echoing. Run with SHELL=/bin/cat if the default shell does not echo:
//   SHELL=/bin/cat cargo test -p jetty-core --test pty
use jetty_core::PtySession;
use std::time::{Duration, Instant};

#[test]
fn pty_echoes_written_bytes() {
    let pty = PtySession::spawn(80, 24, None, || {}).expect("spawn");
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
    let pty = PtySession::spawn(80, 24, None, || {}).expect("spawn");
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
