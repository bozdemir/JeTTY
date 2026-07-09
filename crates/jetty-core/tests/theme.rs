// Tests for the theme system.
use jetty_core::{Terminal, Theme};

/// Every PRESETS entry must resolve to a theme whose `name` round-trips exactly,
/// carry a non-empty display name, and have a unique key + display name. This is
/// the lockstep guard for the Settings theme dropdown (which lists every preset).
#[test]
fn every_preset_resolves_and_has_unique_display_name() {
    use std::collections::HashSet;
    let mut keys = HashSet::new();
    let mut displays = HashSet::new();
    for &key in jetty_core::theme::PRESETS.iter() {
        let t = Theme::by_name(key);
        assert_eq!(t.name.as_ref(), key, "by_name({key:?}) must round-trip its key");
        assert!(!t.display_name.is_empty(), "{key} has an empty display_name");
        assert!(keys.insert(key), "duplicate preset key: {key}");
        let display = t.display_name.into_owned();
        assert!(
            displays.insert(display.clone()),
            "duplicate display_name: {display}"
        );
    }
}

/// Unknown JETTY_THEME name falls back to the default theme (catppuccin_mocha).
#[test]
fn unknown_theme_env_falls_back_to_default() {
    // We use set_theme so env ordering doesn't affect other tests.
    let mut term = Terminal::new(80, 24);
    term.set_theme(Theme::by_name("nonexistent_theme_xyz"));
    assert_eq!(term.theme().bg, [30, 30, 46, 255]); // Catppuccin Mocha base
}

/// Setting a non-default theme changes the snapshot bg_rgba.
#[test]
fn set_theme_changes_snapshot_bg_rgba() {
    use jetty_core::theme::gruvbox_dark;
    let mut term = Terminal::new(80, 24);
    term.set_theme(gruvbox_dark());
    term.feed(b"x");
    let snap = term.snapshot();
    // Gruvbox dark bg is [40, 40, 40, 255]
    assert_eq!(snap.bg_rgba, [40, 40, 40, 255]);
}
