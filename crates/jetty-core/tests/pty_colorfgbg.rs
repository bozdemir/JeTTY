//! `COLORFGBG` reaches the shell describing JeTTY's theme — never the value of
//! the terminal JeTTY itself was started from.
#![cfg(unix)]

use std::time::{Duration, Instant};

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

fn shell_colorfgbg(pty: &PtySession) -> String {
    use std::io::Write;
    let mut w = pty.writer();
    w.write_all(b"echo \"CFB=[${COLORFGBG-unset}]\"; echo DONE-CFB\n").unwrap();
    let out = read_until(pty, "\nDONE-CFB");
    // A prompt may precede the line (`$ CFB=[…]`); the echoed command itself
    // carries the unexpanded `${…}`.
    out.lines()
        .filter_map(|l| l.split_once("CFB=[").map(|(_, v)| v))
        .filter_map(|v| v.split_once(']').map(|(v, _)| v))
        .find(|v| !v.contains('$'))
        .unwrap_or_else(|| panic!("no CFB line in:\n{out}"))
        .to_string()
}

#[test]
fn colorfgbg_is_jettys_own() {
    // This test binary's environment is its own (one file = one process).
    std::env::set_var("COLORFGBG", "7;0");
    let plain = PtySession::spawn(80, 24, 0, 0, Some("/bin/sh".into()), None, || {}).expect("spawn");
    assert_eq!(shell_colorfgbg(&plain), "unset", "the launching terminal's value must not leak");

    let light = jetty_core::contrast::colorfgbg([0xfd, 0xf6, 0xe3]);
    let pty = PtySession::spawn_with_env(
        80,
        24,
        0,
        0,
        Some("/bin/sh".into()),
        None,
        vec![("COLORFGBG".into(), light.into())],
        || {},
    )
    .expect("spawn");
    assert_eq!(shell_colorfgbg(&pty), "0;15");
}
