//! `jetty` as a launcher or a login item sees it: exit statuses and messages.
//!
//! Every run here has no display (DISPLAY / WAYLAND_DISPLAY removed, so no
//! window can open anywhere) and its own runtime, config and home directories,
//! so it can never reach the JeTTY you are using or its single-instance socket.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fresh, short directory tree for one run (a Unix socket path is capped at
/// 108 bytes, so it stays under the system temp dir).
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jetty-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["run", "home", "config", "cache"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    dir
}

fn jetty(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jetty"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("JETTY_CONFIG_DIR")
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .expect("run jetty")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn no_display_is_an_error_not_a_panic() {
    let dir = sandbox("nodisplay");
    let out = jetty(&dir, &["--toggle"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("can't reach a display"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    // The socket it bound is gone: the next launch finds nothing stale.
    assert!(!dir.join("run").join("jetty.sock").exists(), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refusing_a_second_instance_exits_non_zero() {
    // A live primary holds the lock, but its socket is gone: JeTTY refuses to
    // start a second instance — and a launcher must see that as a failure.
    let dir = sandbox("refuse");
    let lock = std::fs::File::create(dir.join("run").join("jetty.sock.lock")).unwrap();
    lock.lock().unwrap();
    let out = jetty(&dir, &["--show"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("not starting a second instance"), "{err}");
    drop(lock);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hide_with_nothing_running_does_nothing() {
    let dir = sandbox("hide");
    let out = jetty(&dir, &["--hide"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    assert!(!dir.join("run").join("jetty.sock").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
