// Own test binary on purpose: it inspects THIS process's ptmx fds by tty index
// and its thread count, which other pty tests running in parallel would perturb
// (a freed pty index is reused by the next openpty).
#![cfg(target_os = "linux")]

use jetty_core::PtySession;
use std::time::{Duration, Instant};

/// How many of this process's fds are a pty master for `/dev/pts/<index>`.
fn master_fds_for(index: u32) -> usize {
    let Ok(dir) = std::fs::read_dir("/proc/self/fd") else { return 0 };
    dir.filter_map(Result::ok)
        .filter(|e| std::fs::read_link(e.path()).is_ok_and(|p| p.ends_with("ptmx")))
        .filter(|e| {
            let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", e.file_name().to_string_lossy()))
                .unwrap_or_default();
            info.lines()
                .find_map(|l| l.strip_prefix("tty-index:"))
                .and_then(|v| v.trim().parse::<u32>().ok())
                == Some(index)
        })
        .count()
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map(|d| d.count()).unwrap_or(0)
}

#[test]
fn dropping_a_session_closes_every_master_fd_even_when_the_tty_stays_silent() {
    // The reader thread used to sit in read(2) on its own dup of the master; it
    // only left once some output arrived. If nothing is ever written after the
    // tab closes (no echo, a HUP-ignoring process that dies silently), the
    // thread and its master fd leaked for the life of the app — and the master
    // was never fully closed, so the pty was never hung up. The session's Drop
    // must now end the reader regardless.
    let before = threads();
    let pidfile = std::env::temp_dir().join(format!("jetty-pty-hangup-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);
    let pty = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    {
        use std::io::Write;
        let mut w = pty.writer();
        // Report the pty index; leave a silent process holding the slave in its
        // own session (a disowned job: never writes, never reads, no SIGHUP);
        // then turn the shell into a silent HUP-ignoring reader: no echo (the
        // "\n^D" portable-pty sends when the writer drops is not echoed) and
        // its output goes nowhere. After Drop the tty never produces a byte,
        // yet stays open — the case that pinned the reader in read(2).
        let cmd = format!(
            "echo \"IDX=$(tty)=END\"; stty -echo; (trap '' HUP; exec setsid sh -c 'echo $$ > \"{}\"; exec sleep 1000') & trap '' HUP; exec cat >/dev/null\n",
            pidfile.display()
        );
        w.write_all(cmd.as_bytes()).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = String::new();
    let index = loop {
        assert!(Instant::now() < deadline, "no tty name in shell output: {seen:?}");
        if let Some(chunk) = pty.recv_output_timeout(Duration::from_millis(100)) {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        // The echoed command line shows `$(tty)`; the expansion shows the path.
        if let Some(n) = seen
            .split("IDX=/dev/pts/")
            .nth(1)
            .and_then(|rest| rest.split_once("=END"))
            .and_then(|(digits, _)| digits.parse::<u32>().ok())
        {
            break n;
        }
    };
    assert!(master_fds_for(index) > 0, "sanity: the live session holds the master");
    // Let the holder start and `exec cat` take over before closing the tab.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pidfile.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    drop(pty);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (master_fds_for(index) > 0 || threads() > before) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let leaked_fds = master_fds_for(index);
    let leaked_threads = threads().saturating_sub(before);
    // Never leave the holder behind.
    if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
        // SAFETY: kill(2) on the `sleep` our own test started (its PID file).
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(&pidfile);
    assert_eq!(leaked_fds, 0, "a master fd for /dev/pts/{index} outlived its session");
    assert_eq!(leaked_threads, 0, "session threads outlived it");
}
