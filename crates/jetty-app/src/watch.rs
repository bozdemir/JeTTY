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
//! * the config dir itself (`config.toml`) and its `themes/` subdir (a symlinked
//!   dir is followed);
//! * the config dir's PARENT (`~/.config`) only while the config dir is missing —
//!   to see it created — or, on inotify, is a symlink — to see it re-linked
//!   (stow / home-manager), which its own watch, on the link's target, cannot.
//!   Never while it is a plain dir: inotify reports every open() of every file in
//!   `~/.config` and FSEvents every write under `~/Library/Application Support`,
//!   each a wake of the watcher thread for nothing. A deleted dir is reported by
//!   its own watch, and the reload's rearm then watches the parent for its
//!   return. (FSEvents never watches that busy parent for a symlinked dir
//!   either: a re-pointed config FOLDER link is seen at the next reload, or
//!   restart.);
//! * when `config.toml` — or a `themes/*.toml` — is a symlink to a file
//!   elsewhere (a dotfiles repo), the real file's directory, matched by the real
//!   file's exact path, so its own name (`jetty.toml`) works and the repo's other
//!   files are ignored. A dangling link's target dir is watched too, so the file
//!   appearing there is noticed.
//!
//! FSEvents reports a change by its REAL path: `/private/tmp/jt/config.toml` for
//! a `JETTY_CONFIG_DIR=/tmp/jt`, the dotfiles folder a symlinked config dir points
//! into. Paths are matched in either spelling.
//!
//! Naming gotcha: jetty-app already has a local `mod notify` (desktop toasts), so
//! the file-watcher crate is referenced as `::notify` throughout.

use std::borrow::Cow;
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
    /// `(real, configured)`: the themes dir and the config dir as the OS spells
    /// them where that differs (see [`real_spelling`]) — FSEvents reports
    /// changes under the real one.
    real_dirs: Vec<(PathBuf, PathBuf)>,
    /// The real file `config.toml` points at, when it is a symlink.
    real_config: Option<PathBuf>,
    /// The real files symlinked `themes/*.toml` point at: an edit lands THERE,
    /// and the themes dir itself sees nothing.
    real_themes: Vec<PathBuf>,
}

/// `p` with every symlink resolved, as far as it exists — `/tmp/jt/themes`
/// with only `/tmp` there is `/private/tmp/jt/themes` on macOS.
fn real_spelling(p: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(p) {
        Ok(real) => Some(real),
        Err(_) => Some(real_spelling(p.parent()?)?.join(p.file_name()?)),
    }
}

impl Targets {
    fn compute(config_dir: &Path) -> Targets {
        let linked = |p: &Path| match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => Some(crate::config::real_path(p)),
            _ => None,
        };
        let themes_dir = config_dir.join("themes");
        let mut real_themes: Vec<PathBuf> = std::fs::read_dir(&themes_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| crate::themes::is_theme_file(p))
            .filter_map(|p| linked(&p))
            .collect();
        real_themes.sort();
        real_themes.dedup();
        // The themes dir first: when it is itself a symlink, its real path is
        // not under the config dir's.
        let real_dirs = [themes_dir.as_path(), config_dir]
            .into_iter()
            .filter_map(|d| real_spelling(d).filter(|r| r != d).map(|r| (r, d.to_path_buf())))
            .collect();
        Targets {
            config_dir: config_dir.to_path_buf(),
            themes_dir,
            real_dirs,
            real_config: linked(&config_dir.join("config.toml")),
            real_themes,
        }
    }

    /// The directories to watch (each non-recursively), existing ones only.
    fn watch_paths(&self) -> Vec<PathBuf> {
        self.watch_paths_for(RECURSIVE_BACKEND)
    }

    /// [`Targets::watch_paths`] for a backend that is (`recursive`) or is not
    /// recursive whatever is asked. The config dir's parent only while the
    /// config dir is missing (to see it appear) or — not recursive — a symlink
    /// (to see it re-pointed); see the module docs.
    fn watch_paths_for(&self, recursive: bool) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut add = |p: &Path| {
            if p.is_dir() && !out.iter().any(|q| q == p) {
                out.push(p.to_path_buf());
            }
        };
        if let Some(parent) = self.config_dir.parent() {
            let linked = std::fs::symlink_metadata(&self.config_dir).is_ok_and(|m| m.file_type().is_symlink());
            if !self.config_dir.is_dir() || (linked && !recursive) {
                add(parent);
            }
        }
        add(&self.config_dir);
        add(&self.themes_dir);
        for real in self.real_config.iter().chain(&self.real_themes) {
            if let Some(dir) = real.parent() {
                add(dir);
            }
        }
        out
    }

    /// `p` spelled under the dirs as configured, when FSEvents reported it under
    /// their real path.
    fn as_configured<'a>(&self, p: &'a Path) -> Cow<'a, Path> {
        for (real, configured) in &self.real_dirs {
            if let Ok(rest) = p.strip_prefix(real) {
                return Cow::Owned(configured.join(rest));
            }
        }
        Cow::Borrowed(p)
    }

    /// Does a change to `p` warrant a reload? The real `config.toml` (or the file
    /// it links to), the theme files (`themes/*.toml`, see
    /// [`crate::themes::is_theme_file`]), and the config / themes dirs themselves
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
        if self.real_config.as_deref() == Some(p) || self.real_themes.iter().any(|t| t == p) {
            return true;
        }
        let p = self.as_configured(p);
        if *p == self.config_dir || *p == self.themes_dir || *p == self.config_dir.join("config.toml") {
            return true;
        }
        p.parent() == Some(self.themes_dir.as_path()) && crate::themes::is_theme_file(&p)
    }
}

/// A watched dir's identity — device and inode, through symlinks. A dir deleted
/// and made again, or a link re-pointed, under the same path is another dir:
/// inotify's watch died with the old one.
fn dir_id(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

/// A watched dir and its [`dir_id`] when it was (to be) watched.
type Watched = (PathBuf, Option<(u64, u64)>);

/// What to unwatch and what to watch to get from `watched` to `desired`: only
/// what changed. A dir whose identity changed is in both.
fn rewatch_plan<'a>(watched: &'a [Watched], desired: &'a [Watched]) -> (Vec<&'a Path>, Vec<&'a Path>) {
    let stale = watched.iter().filter(|w| !desired.contains(w)).map(|(p, _)| p.as_path()).collect();
    let fresh = desired.iter().filter(|d| !watched.contains(d)).map(|(p, _)| p.as_path()).collect();
    (stale, fresh)
}

/// Why `dir` could not be watched, for the user — `None` for a dir that went
/// away meanwhile (the reload that follows sees what is there).
fn watch_problem(e: &::notify::Error, dir: &Path) -> Option<String> {
    use ::notify::ErrorKind;
    let why = match &e.kind {
        ErrorKind::PathNotFound | ErrorKind::WatchNotFound => return None,
        ErrorKind::MaxFilesWatch => "the inotify watch limit is reached — raise fs.inotify.max_user_watches".to_string(),
        ErrorKind::Io(io) => io.to_string(),
        ErrorKind::Generic(s) => s.clone(),
        ErrorKind::InvalidConfig(c) => format!("{c:?}"),
    };
    Some(format!("hot reload misses changes in {}: {why}", dir.display()))
}

/// A live config/themes watcher. Keep it alive for as long as watching should
/// continue (dropping it stops watching); call [`ConfigWatcher::rearm`] after each
/// reload.
pub struct ConfigWatcher {
    watcher: RecommendedWatcher,
    targets: Arc<Mutex<Targets>>,
    config_dir: PathBuf,
    watched: Vec<Watched>,
}

impl ConfigWatcher {
    /// Watch `config_dir` (see the module docs); `on_change` runs on the watcher's
    /// thread for every relevant change. Comes with the first rearm's problem,
    /// if any; `Err` says why there is no watcher at all (no hot reload), for
    /// the user.
    pub fn spawn(
        config_dir: PathBuf,
        on_change: impl Fn() + Send + 'static,
    ) -> Result<(ConfigWatcher, Option<String>), String> {
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
        .map_err(|e| match &e.kind {
            // inotify_init's EMFILE: the per-user instance limit (IDEs and
            // Electron apps use them up) — or the open-file limit.
            ::notify::ErrorKind::Io(io) if cfg!(target_os = "linux") && io.raw_os_error() == Some(24) => {
                "hot reload is off: no inotify instance is left — raise fs.inotify.max_user_instances".to_string()
            }
            _ => format!("hot reload is off: the file watcher did not start ({e}) — changes apply after a restart"),
        })?;
        let mut w = ConfigWatcher { watcher, targets, config_dir, watched: Vec::new() };
        let problem = w.rearm();
        Ok((w, problem))
    }

    /// Recompute what to watch and re-attach what changed: picks up a config dir
    /// that was created or recreated, a symlink that was retargeted and a
    /// `themes/` dir created later. Nothing else is touched: an echo reload
    /// costs a few `stat`s, where re-adding everything restarted the FSEvents
    /// stream (macOS) once per dir — on the UI thread, with changes made
    /// meanwhile lost. A dir that comes or goes while this runs is caught by
    /// looking again. Returns why a dir could not be watched, for the user.
    pub fn rearm(&mut self) -> Option<String> {
        let mut problem = None;
        for _ in 0..3 {
            let t = Targets::compute(&self.config_dir);
            let desired: Vec<Watched> = t
                .watch_paths()
                .into_iter()
                .map(|p| {
                    let id = dir_id(&p);
                    (p, id)
                })
                .collect();
            *self.targets.lock().unwrap_or_else(|p| p.into_inner()) = t;
            let (stale, fresh) = rewatch_plan(&self.watched, &desired);
            if stale.is_empty() && fresh.is_empty() {
                break;
            }
            // One batch: FSEvents restarts its stream once for all of it.
            let mut added = Vec::new();
            let mut paths = self.watcher.paths_mut();
            for p in &stale {
                let _ = paths.remove(p);
            }
            for p in &fresh {
                match paths.add(p, RecursiveMode::NonRecursive) {
                    Ok(()) => added.push(p.to_path_buf()),
                    Err(e) => problem = problem.or_else(|| watch_problem(&e, p)),
                }
            }
            let _ = paths.commit();
            let stale: Vec<PathBuf> = stale.into_iter().map(Path::to_path_buf).collect();
            self.watched.retain(|(p, _)| !stale.contains(p));
            self.watched.extend(desired.into_iter().filter(|(p, _)| added.contains(p)));
        }
        problem
    }
}

/// Why hot reload could not watch the config tree at `config_dir` right now —
/// for `jetty --check-config` (a watcher is started and dropped); `None` when
/// it can.
pub(crate) fn probe(config_dir: &Path) -> Option<String> {
    match ConfigWatcher::spawn(config_dir.to_path_buf(), || {}) {
        Ok((_, problem)) => problem,
        Err(e) => Some(e),
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
        // Emacs's lock for a theme buffer being edited.
        assert!(!t.is_relevant(Path::new("/home/u/.config/jetty/themes/.#mine.toml")));
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

    #[cfg(unix)]
    #[test]
    fn symlinked_theme_files_are_watched_at_their_target() {
        // `themes/mine.toml -> ~/dotfiles/themes/mine.toml`: an editor saving the
        // real file changes nothing in themes/, so it never hot-reloaded.
        use std::os::unix::fs::symlink;
        let base = tmp("theme-link");
        let dotfiles = base.join("dotfiles").join("themes");
        let cfg_dir = base.join("config").join("jetty");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(cfg_dir.join("themes")).unwrap();
        std::fs::write(dotfiles.join("mine.toml"), "x = 1\n").unwrap();
        symlink(dotfiles.join("mine.toml"), cfg_dir.join("themes").join("mine.toml")).unwrap();
        let t = Targets::compute(&cfg_dir);
        let real_dir = std::fs::canonicalize(&dotfiles).unwrap();
        assert!(t.watch_paths().contains(&real_dir), "{:?}", t.watch_paths());
        assert!(t.is_relevant(&real_dir.join("mine.toml")));
        assert!(!t.is_relevant(&real_dir.join("other.toml")), "the repo's other files are not ours");

        // End to end: an edit of the REAL file reloads.
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (w, problem) = ConfigWatcher::spawn(cfg_dir.clone(), move || {
            let _ = tx.lock().unwrap().send(());
        })
        .expect("watcher");
        assert_eq!(problem, None);
        std::fs::write(dotfiles.join("mine.toml"), "x = 2\n").unwrap();
        assert!(changed(&rx), "edit of the symlinked theme's target");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_config_link_watches_where_its_target_will_appear() {
        use std::os::unix::fs::symlink;
        let base = tmp("dangling-link");
        let dotfiles = base.join("dotfiles");
        let cfg_dir = base.join("jetty");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&cfg_dir).unwrap();
        symlink(dotfiles.join("jetty.toml"), cfg_dir.join("config.toml")).unwrap();
        let t = Targets::compute(&cfg_dir);
        assert!(t.watch_paths().contains(&dotfiles), "{:?}", t.watch_paths());
        assert!(t.is_relevant(&dotfiles.join("jetty.toml")));
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
    fn a_config_dir_that_exists_never_has_its_busy_parent_watched() {
        // macOS FSEvents is recursive whatever is asked: watching the parent
        // (`~/Library/Application Support`) woke the watcher on every write of
        // every app. inotify reports every open() of a file in the parent
        // (`~/.config/kdeglobals`, any app's start) — a wake for nothing too.
        // Only the config dir itself (and themes/) is watched then.
        let base = tmp("busy-parent");
        let cfg_dir = base.join("jetty");
        std::fs::create_dir_all(cfg_dir.join("themes")).unwrap();
        let t = Targets::compute(&cfg_dir);
        assert_eq!(t.watch_paths_for(true), vec![cfg_dir.clone(), cfg_dir.join("themes")]);
        assert_eq!(t.watch_paths_for(false), vec![cfg_dir.clone(), cfg_dir.join("themes")]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_dir_has_its_parent_watched_on_inotify_only() {
        // Its own watch is on the link's target: only the parent sees the link
        // re-pointed (stow). FSEvents keeps the idle promise instead.
        let base = tmp("linked-parent");
        std::fs::create_dir_all(base.join("dotfiles")).unwrap();
        std::os::unix::fs::symlink(base.join("dotfiles"), base.join("jetty")).unwrap();
        let t = Targets::compute(&base.join("jetty"));
        assert_eq!(t.watch_paths_for(false), vec![base.clone(), base.join("jetty")]);
        assert_eq!(t.watch_paths_for(true), vec![base.join("jetty")]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn changes_reported_by_their_real_path_are_matched() {
        // FSEvents reports `/private/tmp/jt/config.toml` for JETTY_CONFIG_DIR
        // =/tmp/jt, and `~/dotfiles/jetty/…` for a config dir linked there:
        // nothing matched the dir as configured, so hot reload was dead.
        use std::os::unix::fs::symlink;
        let base = tmp("real-path");
        let real = std::fs::canonicalize(&base).unwrap().join("dotfiles").join("jetty");
        std::fs::create_dir_all(real.join("themes")).unwrap();
        symlink(&real, base.join("jetty")).unwrap();
        let t = Targets::compute(&base.join("jetty"));
        assert!(t.is_relevant(&real.join("config.toml")));
        assert!(t.is_relevant(&real.join("themes").join("mine.toml")));
        assert!(t.is_relevant(&real.join("themes")));
        assert!(t.is_relevant(&real));
        assert!(!t.is_relevant(&real.join("notes.toml")), "still only what matters");
        assert!(!t.is_relevant(&real.join("themes").join(".#mine.toml")));
        // A themes dir linked somewhere else, and a config dir that is not
        // there yet under a linked parent (it is watched to appear).
        let elsewhere = std::fs::canonicalize(&base).unwrap().join("theme-repo");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::remove_dir(real.join("themes")).unwrap();
        symlink(&elsewhere, real.join("themes")).unwrap();
        let t = Targets::compute(&base.join("jetty"));
        assert!(t.is_relevant(&elsewhere.join("mine.toml")));
        let t = Targets::compute(&base.join("jetty").join("not-yet"));
        assert!(t.is_relevant(&real.join("not-yet")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_dir_that_cannot_be_watched_says_why() {
        // `hot_reload = true` silently did nothing once inotify's limits were
        // used up (IDEs and Electron apps eat them).
        let dir = Path::new("/home/u/.config/jetty");
        let limit = ::notify::Error::new(::notify::ErrorKind::MaxFilesWatch);
        assert_eq!(
            watch_problem(&limit, dir).as_deref(),
            Some(
                "hot reload misses changes in /home/u/.config/jetty: the inotify watch limit is reached — \
                 raise fs.inotify.max_user_watches"
            )
        );
        // A dir gone meanwhile is no problem: the reload sees what is there.
        assert_eq!(watch_problem(&::notify::Error::path_not_found(), dir), None);
    }

    #[test]
    fn a_rearm_touches_only_the_watches_that_changed() {
        // Every reload — the echo of each Settings save included — unwatched
        // and re-watched every dir: on macOS a stream restart per dir, on the UI
        // thread.
        let (a, b, c) = (PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c"));
        let watched = vec![(a.clone(), Some((1, 10))), (b.clone(), Some((1, 20)))];
        assert_eq!(rewatch_plan(&watched, &watched), (vec![], vec![]), "nothing changed: nothing touched");
        // b deleted and made again (another inode), c new, a gone.
        let desired = vec![(b.clone(), Some((1, 21))), (c.clone(), Some((1, 30)))];
        let (stale, fresh) = rewatch_plan(&watched, &desired);
        assert_eq!(stale, vec![a.as_path(), b.as_path()]);
        assert_eq!(fresh, vec![b.as_path(), c.as_path()]);
    }

    /// Waits for at least one change notification (draining any burst).
    fn changed(rx: &mpsc::Receiver<()>) -> bool {
        let got = rx.recv_timeout(Duration::from_secs(5)).is_ok();
        while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
        got
    }

    /// Editors save in different ways; each must reach the app: an atomic save
    /// (a temp file renamed over config.toml — VS Code, most tools), vim's
    /// (config.toml renamed to a backup, then a new file written), and a delete
    /// followed by a fresh file. The leftovers never count on their own.
    #[test]
    fn every_way_editors_save_is_noticed() {
        let base = tmp("editors");
        let cfg_dir = base.join("jetty");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg = cfg_dir.join("config.toml");
        std::fs::write(&cfg, "theme = \"nord\"\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (w, problem) = ConfigWatcher::spawn(cfg_dir.clone(), move || {
            let _ = tx.lock().unwrap().send(());
        })
        .expect("watcher");
        assert_eq!(problem, None);
        // Atomic replace.
        std::fs::write(cfg_dir.join(".config.toml.swp-edit"), "theme = \"dracula\"\n").unwrap();
        assert!(!changed_quick(&rx), "an editor's scratch file alone is not a change");
        std::fs::rename(cfg_dir.join(".config.toml.swp-edit"), &cfg).unwrap();
        assert!(changed(&rx), "temp file renamed over config.toml");
        // vim: backup by rename, then a new file.
        std::fs::rename(&cfg, cfg_dir.join("config.toml~")).unwrap();
        std::fs::write(&cfg, "theme = \"gruvbox_dark\"\n").unwrap();
        assert!(changed(&rx), "rename away + write new");
        std::fs::remove_file(cfg_dir.join("config.toml~")).unwrap();
        assert!(!changed_quick(&rx), "the backup going away is not a change");
        // Delete, then a fresh file.
        std::fs::remove_file(&cfg).unwrap();
        assert!(changed(&rx), "deleted");
        std::fs::write(&cfg, "theme = \"nord\"\n").unwrap();
        assert!(changed(&rx), "recreated");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A config DIRECTORY symlinked from a dotfiles repo (stow, home-manager):
    /// edits through it are seen, and so is the link being pointed elsewhere.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_dir_is_followed_and_its_retarget_seen() {
        use std::os::unix::fs::symlink;
        let base = tmp("dir-link");
        let (repo_a, repo_b) = (base.join("dotfiles-a"), base.join("dotfiles-b"));
        for r in [&repo_a, &repo_b] {
            std::fs::create_dir_all(r).unwrap();
            std::fs::write(r.join("config.toml"), "theme = \"nord\"\n").unwrap();
        }
        let cfg_dir = base.join("jetty");
        symlink(&repo_a, &cfg_dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (mut w, problem) = ConfigWatcher::spawn(cfg_dir.clone(), move || {
            let _ = tx.lock().unwrap().send(());
        })
        .expect("watcher");
        assert_eq!(problem, None);
        std::fs::write(repo_a.join("config.toml"), "theme = \"dracula\"\n").unwrap();
        assert!(changed(&rx), "an edit in the linked folder");
        // Re-point the link (atomically, as stow / ln -sfn do).
        symlink(&repo_b, base.join("jetty.new")).unwrap();
        std::fs::rename(base.join("jetty.new"), &cfg_dir).unwrap();
        assert!(changed(&rx), "the link re-pointed");
        w.rearm();
        std::fs::write(repo_b.join("config.toml"), "theme = \"gruvbox_dark\"\n").unwrap();
        assert!(changed(&rx), "an edit in the new target");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Whether a change notification arrives soon (for "nothing happened").
    fn changed_quick(rx: &mpsc::Receiver<()>) -> bool {
        let got = rx.recv_timeout(Duration::from_millis(400)).is_ok();
        while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}
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
        let (mut w, problem) = ConfigWatcher::spawn(cfg_dir.clone(), move || {
            let _ = tx.lock().unwrap().send(());
        })
        .expect("watcher");
        assert_eq!(problem, None);
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"dracula\"\n").unwrap();
        assert!(changed(&rx), "edit of config.toml");

        // themes/ created later: the creation itself is a change; after the reload's
        // rearm, a theme file inside it is watched too.
        std::fs::create_dir_all(cfg_dir.join("themes")).unwrap();
        assert!(changed(&rx), "themes/ created");
        w.rearm();
        std::fs::write(cfg_dir.join("themes").join("mine.toml"), "x = 1\n").unwrap();
        assert!(changed(&rx), "theme file in a late themes/");

        // The config dir deleted (seen by its own watch) and — after the
        // reload's rearm watches the parent for it — recreated.
        std::fs::remove_dir_all(&cfg_dir).unwrap();
        assert!(changed(&rx), "config dir removed");
        assert_eq!(w.rearm(), None);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        assert!(changed(&rx), "config dir recreated");
        assert_eq!(w.rearm(), None);
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"nord\"\n").unwrap();
        assert!(changed(&rx), "edit after the dir was recreated");

        // Unrelated churn next door does not wake the app.
        std::fs::write(base.join("kwinrc"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "unrelated file");
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }
}
