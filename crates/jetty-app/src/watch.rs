//! Config + themes hot-reload file watcher.
//!
//! One `notify` `RecommendedWatcher` (INotifyWatcher on Linux, FsEventWatcher on
//! macOS) watches the JeTTY config directory. It is OS-event-driven — the backing
//! thread blocks in the kernel and only wakes on a real filesystem change, so it
//! adds ZERO idle CPU (no polling). On a relevant change it forwards a single
//! `AppEvent::ConfigChanged` through the winit `EventLoopProxy`; the app debounces
//! and applies the reload from `about_to_wait`.
//!
//! Naming gotcha: jetty-app already has a local `mod notify` (desktop toasts), so
//! the file-watcher crate is referenced as `::notify` throughout.

use winit::event_loop::EventLoopProxy;

use crate::app::AppEvent;

/// Does this changed path warrant a reload? Matches ONLY the real `config.toml` and
/// `themes/*.toml`, and EXCLUDES `write_atomic`'s PID-suffixed temp file
/// (`.config.toml.tmp.<pid>`, which contains `.tmp.`) and the startup-only
/// `config.toml.bad` — both of which would otherwise fire spurious CREATE/REMOVE
/// events (amendment H4).
fn is_watched_path(p: &std::path::Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // Exclude the atomic-write temp file and the preserved-bad file.
    if name.contains(".tmp.") || name.ends_with(".bad") {
        return false;
    }
    if name == "config.toml" {
        return true;
    }
    // A theme file: `*.toml` directly inside a `themes` directory.
    p.extension().and_then(|x| x.to_str()) == Some("toml")
        && p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) == Some("themes")
}

/// Spawn the config/themes watcher. Returns the `RecommendedWatcher`, which the
/// caller MUST keep alive for the process lifetime (dropping it stops watching);
/// `None` if the watcher could not be created or the config dir could not be watched.
///
/// Watches the CANONICAL config directory (following a symlinked `config.toml`, the
/// common dotfiles/stow/chezmoi setup — amendment H3) recursively, plus a distinct
/// canonical `themes/` directory when it resolves outside the config dir.
pub fn spawn_config_watcher(
    proxy: EventLoopProxy<AppEvent>,
) -> Option<::notify::RecommendedWatcher> {
    use ::notify::{Config as NotifyConfig, EventKind, RecursiveMode, Watcher};

    // Canonical parent of config.toml (resolves a symlinked config file to its real
    // dir); fall back to the plain ~/.config/jetty when the file doesn't exist yet.
    let cfg_path = crate::config::Config::config_path();
    let cfg_dir = std::fs::canonicalize(&cfg_path)
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(crate::config::Config::dir);

    // Canonical themes dir (may be symlinked to a different real location).
    let themes_raw = crate::config::Config::dir().join("themes");
    let themes_dir = std::fs::canonicalize(&themes_raw).ok();

    let mut watcher = ::notify::RecommendedWatcher::new(
        move |res: ::notify::Result<::notify::Event>| {
            let Ok(ev) = res else { return };
            // Content/rename/create/remove only — ignore Access (open/close/read).
            if !matches!(
                ev.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            if ev.paths.iter().any(|p| is_watched_path(p)) {
                // Coalesced app-side (debounced in about_to_wait); a send error just
                // means the loop is gone (shutting down).
                let _ = proxy.send_event(AppEvent::ConfigChanged);
            }
        },
        NotifyConfig::default(),
    )
    .ok()?;

    // Watch the DIRECTORY (not the file): write_atomic swaps the inode via rename,
    // which would drop a file-level watch. Recursive so a `themes/` created later is
    // covered too.
    watcher.watch(&cfg_dir, RecursiveMode::Recursive).ok()?;

    // If themes/ resolves OUTSIDE the config dir (symlinked away), watch it too.
    // Errors (e.g. it doesn't exist yet) are non-fatal — the config watch still works.
    if let Some(td) = themes_dir {
        if td != cfg_dir.join("themes") {
            let _ = watcher.watch(&td, RecursiveMode::Recursive);
        }
    }

    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_config_and_theme_files() {
        assert!(is_watched_path(Path::new("/home/u/.config/jetty/config.toml")));
        assert!(is_watched_path(Path::new("/home/u/.config/jetty/themes/mine.toml")));
    }

    #[test]
    fn ignores_temp_and_bad_and_unrelated() {
        // write_atomic's PID temp file.
        assert!(!is_watched_path(Path::new("/home/u/.config/jetty/.config.toml.tmp.1234")));
        // preserved-bad file (startup-only).
        assert!(!is_watched_path(Path::new("/home/u/.config/jetty/config.toml.bad")));
        // a stray toml NOT under themes/.
        assert!(!is_watched_path(Path::new("/home/u/.config/jetty/other.toml")));
        // unrelated file.
        assert!(!is_watched_path(Path::new("/home/u/.config/jetty/notes.txt")));
    }
}
