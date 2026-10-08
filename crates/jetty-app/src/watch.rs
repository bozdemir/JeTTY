//! Config + themes hot-reload file watcher.
//!
//! One `notify` `RecommendedWatcher` (INotifyWatcher on Linux, FsEventWatcher on
//! macOS) watches a handful of directories NON-recursively. It is OS-event-driven —
//! the backing thread blocks in the kernel and only wakes on a real filesystem
//! change, so it adds ZERO idle CPU (no polling). On a relevant change it calls the
//! `on_change` callback (the app forwards a single `AppEvent::ConfigChanged`; it
//! debounces and applies the reload from `about_to_wait`).
//!
//! What is watched (recomputed by [`ConfigWatcher::rearm`] after every reload):
//! * the config dir's PARENT (`~/.config`) — so the config dir being created,
//!   deleted-and-recreated or re-linked (stow / home-manager) is noticed. Not on
//!   macOS while the config dir exists: FSEvents watches recursively whatever is
//!   asked, and that parent is `~/Library/Application Support`, where every app
//!   writes all the time — each write would wake the watcher;
//! * the config dir itself (`config.toml`) and its `themes/` subdir (a symlinked
//!   dir is followed);
//! * when `config.toml` is a symlink to a file elsewhere (a dotfiles repo), the
//!   real file's directory — matched by the real file's exact path, so its own
//!   name (`jetty.toml`) works and the repo's other files are ignored.
//!
//! Naming gotcha: jetty-app already has a local `mod notify` (desktop toasts), so
//! the file-watcher crate is referenced as `::notify` throughout.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ::notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Whether the OS watcher backend watches directories RECURSIVELY regardless of
/// the mode asked for (macOS FSEvents) — then a busy parent dir is not watched.
const RECURSIVE_BACKEND: bool = cfg!(target_os = "macos");

/// The paths that matter right now. Recomputed on every `rearm`, read by the
/// event callback.
#[derive(Debug, Clone, PartialEq)]
struct Targets {
    /// The config dir as configured (`~/.config/jetty`, not canonicalized).
    config_dir: PathBuf,
    /// `<config_dir>/themes`.
    themes_dir: PathBuf,
    /// The real file `config.toml` points at, when it is a symlink.
    real_config: Option<PathBuf>,
}

impl Targets {
    fn compute(config_dir: &Path) -> Targets {
        let plain = config_dir.join("config.toml");
        let real_config = match std::fs::symlink_metadata(&plain) {
            Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(&plain).ok(),
            _ => None,
        };
        Targets {
            config_dir: config_dir.to_path_buf(),
            themes_dir: config_dir.join("themes"),
            real_config,
        }
    }

    /// The directories to watch (each non-recursively), existing ones only.
    fn watch_paths(&self) -> Vec<PathBuf> {
        self.watch_paths_for(RECURSIVE_BACKEND)
    }

    /// [`Targets::watch_paths`] for a backend that is (`recursive`) or is not
    /// recursive whatever is asked: a recursive one watches the config dir's
    /// parent only while the config dir is missing (to see it appear) — once it
    /// exists, the dir's own watch reports its removal.
    fn watch_paths_for(&self, recursive: bool) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut add = |p: &Path| {
            if p.is_dir() && !out.iter().any(|q| q == p) {
                out.push(p.to_path_buf());
            }
        };
        if let Some(parent) = self.config_dir.parent() {
            if !recursive || !self.config_dir.is_dir() {
                add(parent);
            }
        }
        add(&self.config_dir);
        add(&self.themes_dir);
        if let Some(dir) = self.real_config.as_deref().and_then(Path::parent) {
            add(dir);
        }
        out
    }

    /// Does a change to `p` warrant a reload? The real `config.toml` (or the file
    /// it links to), `themes/*.toml`, and the config / themes dirs themselves
    /// (created, removed, renamed). Never JeTTY's own atomic-save temp file
    /// (`.config.toml.tmp.<pid>`), its backups (`config.toml.bad-*`, `.bak-*`) or
    /// anything else in the watched dirs.
    fn is_relevant(&self, p: &Path) -> bool {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        if name.contains(".tmp.") {
            return false;
        }
        if p == self.config_dir || p == self.themes_dir || p == self.config_dir.join("config.toml") {
            return true;
        }
        if self.real_config.as_deref() == Some(p) {
            return true;
        }
        p.parent() == Some(self.themes_dir.as_path())
            && p.extension().and_then(|x| x.to_str()) == Some("toml")
    }
}

/// A live config/themes watcher. Keep it alive for as long as watching should
/// continue (dropping it stops watching); call [`ConfigWatcher::rearm`] after each
/// reload.
pub struct ConfigWatcher {
    watcher: RecommendedWatcher,
    targets: Arc<Mutex<Targets>>,
    config_dir: PathBuf,
    watched: Vec<PathBuf>,
}

impl ConfigWatcher {
    /// Watch `config_dir` (see the module docs); `on_change` runs on the watcher's
    /// thread for every relevant change. `None` if the OS watcher can't be created.
    pub fn spawn(config_dir: PathBuf, on_change: impl Fn() + Send + 'static) -> Option<ConfigWatcher> {
        let targets = Arc::new(Mutex::new(Targets::compute(&config_dir)));
        let seen = Arc::clone(&targets);
        let watcher = RecommendedWatcher::new(
            move |res: ::notify::Result<::notify::Event>| {
                let Ok(ev) = res else { return };
                // Content/rename/create/remove only — ignore Access (open/close/read).
                if !matches!(
                    ev.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                let relevant = {
                    let t = seen.lock().unwrap_or_else(|p| p.into_inner());
                    ev.paths.iter().any(|p| t.is_relevant(p))
                };
                if relevant {
                    on_change();
                }
            },
            NotifyConfig::default(),
        )
        .ok()?;
        let mut w = ConfigWatcher { watcher, targets, config_dir, watched: Vec::new() };
        w.rearm();
        Some(w)
    }

    /// Recompute what to watch and (re)attach: picks up a config dir that was
    /// created or recreated, a `config.toml` symlink that was retargeted and a
    /// `themes/` dir created later. A watch on a deleted dir dies silently in the
    /// kernel, so every desired path is re-added (cheap: a few directories).
    pub fn rearm(&mut self) {
        let t = Targets::compute(&self.config_dir);
        let desired = t.watch_paths();
        *self.targets.lock().unwrap_or_else(|p| p.into_inner()) = t;
        for p in self.watched.drain(..) {
            let _ = self.watcher.unwatch(&p);
        }
        for p in desired {
            if self.watcher.watch(&p, RecursiveMode::NonRecursive).is_ok() {
                self.watched.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jetty-watch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn targets(dir: &str) -> Targets {
        Targets::compute(Path::new(dir))
    }

    #[test]
    fn matches_config_and_theme_files() {
        let t = targets("/home/u/.config/jetty");
        assert!(t.is_relevant(Path::new("/home/u/.config/jetty/config.toml")));
        assert!(t.is_relevant(Path::new("/home/u/.config/jetty/themes/mine.toml")));
        // The dirs themselves appearing/disappearing.
        assert!(t.is_relevant(Path::new("/home/u/.config/jetty")));
        assert!(t.is_relevant(Path::new("/home/u/.config/jetty/themes")));
    }

    #[test]
    fn ignores_temp_backups_and_unrelated() {
        let t = targets("/home/u/.config/jetty");
        // write_atomic's PID temp file.
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/.config.toml.tmp.1234")));
        // preserved copies.
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/config.toml.bad-1700000000")));
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/config.toml.bak-1700000000")));
        // a stray toml NOT under themes/, other apps' configs next door.
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/other.toml")));
        assert!(!t.is_relevant(Path::new("/home/u/.config/kwinrc")));
        assert!(!t.is_relevant(Path::new("/home/u/.config/foo/config.toml")));
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/notes.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_watches_and_matches_the_real_file_only() {
        use std::os::unix::fs::symlink;
        let base = tmp("link");
        let dotfiles = base.join("dotfiles");
        let cfg_dir = base.join("config").join("jetty");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(dotfiles.join("jetty.toml"), "theme = \"nord\"\n").unwrap();
        symlink(dotfiles.join("jetty.toml"), cfg_dir.join("config.toml")).unwrap();
        let t = Targets::compute(&cfg_dir);
        let real_dir = std::fs::canonicalize(&dotfiles).unwrap();
        assert!(t.watch_paths().contains(&real_dir), "{:?}", t.watch_paths());
        assert!(t.is_relevant(&real_dir.join("jetty.toml")));
        // The dotfiles repo's OTHER files never trigger a reload.
        assert!(!t.is_relevant(&real_dir.join("config.toml")));
        assert!(!t.is_relevant(&real_dir.join("nvim.toml")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_config_dir_still_watches_its_parent() {
        let base = tmp("missing");
        let t = Targets::compute(&base.join("jetty"));
        assert_eq!(t.watch_paths(), vec![base.clone()]);
        assert_eq!(t.watch_paths_for(true), vec![base.clone()], "recursive backend too");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_recursive_backend_skips_the_busy_parent_once_the_dir_exists() {
        // macOS FSEvents is recursive whatever is asked: watching the parent
        // (`~/Library/Application Support`) woke the watcher on every write of
        // every app. Only the config dir itself (and themes/) is watched then.
        let base = tmp("recursive");
        let cfg_dir = base.join("jetty");
        std::fs::create_dir_all(cfg_dir.join("themes")).unwrap();
        let t = Targets::compute(&cfg_dir);
        assert_eq!(t.watch_paths_for(true), vec![cfg_dir.clone(), cfg_dir.join("themes")]);
        // inotify (non-recursive) keeps the cheap parent watch: it also sees the
        // config dir re-linked (stow), which its own watch cannot.
        assert_eq!(
            t.watch_paths_for(false),
            vec![base.clone(), cfg_dir.clone(), cfg_dir.join("themes")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Waits for at least one change notification (draining any burst).
    fn changed(rx: &mpsc::Receiver<()>) -> bool {
        let got = rx.recv_timeout(Duration::from_secs(5)).is_ok();
        while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
        got
    }

    /// End-to-end with the real OS watcher: an edit, a themes/ dir created after
    /// startup, and the whole config dir deleted and recreated all keep working.
    #[test]
    fn live_watch_survives_new_themes_dir_and_recreated_config_dir() {
        let base = tmp("live");
        let cfg_dir = base.join("jetty");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"nord\"\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let mut w = ConfigWatcher::spawn(cfg_dir.clone(), move || {
            let _ = tx.lock().unwrap().send(());
        })
        .expect("watcher");
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"dracula\"\n").unwrap();
        assert!(changed(&rx), "edit of config.toml");

        // themes/ created later: the creation itself is a change; after the reload's
        // rearm, a theme file inside it is watched too.
        std::fs::create_dir_all(cfg_dir.join("themes")).unwrap();
        assert!(changed(&rx), "themes/ created");
        w.rearm();
        std::fs::write(cfg_dir.join("themes").join("mine.toml"), "x = 1\n").unwrap();
        assert!(changed(&rx), "theme file in a late themes/");

        // The config dir deleted and recreated (seen through the parent watch).
        std::fs::remove_dir_all(&cfg_dir).unwrap();
        assert!(changed(&rx), "config dir removed");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        assert!(changed(&rx), "config dir recreated");
        w.rearm();
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"nord\"\n").unwrap();
        assert!(changed(&rx), "edit after the dir was recreated");

        // Unrelated churn next door does not wake the app.
        std::fs::write(base.join("kwinrc"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "unrelated file");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }
}
