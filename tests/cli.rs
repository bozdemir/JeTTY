//! `jetty` as a launcher or a login item sees it: exit statuses and messages.
//!
//! Every run here has no display (DISPLAY / WAYLAND_DISPLAY removed, so no
//! window can open anywhere) and its own runtime, config and home directories,
//! so it can never reach the JeTTY you are using or its single-instance socket.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
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

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_jetty"));
    c.args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("JETTY_CONFIG_DIR")
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"));
    c
}

fn jetty(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().expect("run jetty")
}

/// A file that passes for an AppImage (ELF with the type-2 magic at byte 8).
fn fake_appimage(path: &Path) {
    let mut head = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
    head.resize(64, 0);
    std::fs::write(path, head).unwrap();
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
fn a_newer_appimage_moves_the_login_item_off_an_older_running_jetty() {
    // JeTTY ≤ 0.29 runs from its AppImage, which the login item starts.
    // AppImageUpdate wrote the new version beside it; launching that only
    // toggles the old instance — which reads the verb and hangs up, knowing
    // nothing newer. The launch moves the login item to itself.
    let dir = sandbox("follow");
    let old = dir.join("JeTTY-0.29.1-x86_64.AppImage");
    let new = dir.join("JeTTY-0.30.0-x86_64.AppImage");
    fake_appimage(&old);
    fake_appimage(&new);
    let item = dir.join("config").join("autostart").join("jetty.desktop");
    std::fs::create_dir_all(item.parent().unwrap()).unwrap();
    std::fs::write(
        &item,
        format!("[Desktop Entry]\nType=Application\nExec=\"{}\" --background\nX-JeTTY-Generated=true\n", old.display()),
    )
    .unwrap();
    let listener = UnixListener::bind(dir.join("run").join("jetty.sock")).unwrap();
    let older = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut verb = [0u8; 16];
        let n = s.read(&mut verb).unwrap();
        verb[..n].to_vec()
    });
    // Running from an AppImage: $APPIMAGE names the file, the binary lives
    // under $APPDIR (the AppImage runtime sets both).
    let exe = Path::new(env!("CARGO_BIN_EXE_jetty"));
    let out = command(&dir, &["--toggle"])
        .env("APPIMAGE", &new)
        .env("APPDIR", exe.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(older.join().unwrap(), b"toggle", "the verb arrives alone, as an older JeTTY reads it");
    let entry = std::fs::read_to_string(&item).unwrap();
    assert!(entry.contains(&*new.to_string_lossy()) && !entry.contains(&*old.to_string_lossy()), "{entry}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_on_another_display_does_not_toggle_this_one() {
    // `ssh -X` / a second X session: the running JeTTY is on another display.
    // It hears who calls and turns the launch away; the launch then starts
    // its own (here: there is no X server, so it says so and exits 1).
    let dir = sandbox("elsewhere");
    let listener = UnixListener::bind(dir.join("run").join("jetty.sock")).unwrap();
    let primary = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut verb = [0u8; 16];
        let n = s.read(&mut verb).unwrap();
        s.write_all(b"jetty 0.30.0\n").unwrap();
        let mut intro = Vec::new();
        s.read_to_end(&mut intro).unwrap();
        s.write_all(b"elsewhere\n").unwrap();
        (verb[..n].to_vec(), intro)
    });
    let out = command(&dir, &["--toggle"]).env("DISPLAY", ":4242.0").output().unwrap();
    let err = stderr(&out);
    let (verb, intro) = primary.join().unwrap();
    assert_eq!(verb, b"toggle");
    let fields: Vec<&[u8]> = intro.split(|&b| b == 0).collect();
    assert_eq!(fields[0], b"v1", "{intro:?}");
    assert_eq!(fields[3], b":4242", "the display it runs on: {intro:?}");
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("can't reach a display"), "{err}");
    // It took its display's own lock and socket (the socket cleaned up again),
    // never the other JeTTY's.
    let mut names: Vec<String> = std::fs::read_dir(dir.join("run"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].starts_with("jetty-") && names[0].ends_with(".sock.lock"), "{names:?}");
    assert_eq!(names[1], "jetty.sock");
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
