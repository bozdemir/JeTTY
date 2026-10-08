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

/// Built-ins are only ever APPENDED: the historical 22 keep their indices (an
/// ordered `theme_idx`, the registry tests' dracula == 3) and the v2 additions
/// follow in a fixed order.
#[test]
fn presets_only_ever_append() {
    let p = jetty_core::theme::PRESETS;
    assert_eq!(p.len(), 46);
    assert_eq!(&p[..4], &["catppuccin_mocha", "tokyo_night", "gruvbox_dark", "dracula"]);
    assert_eq!(p[21], "catppuccin_macchiato", "the last of the original 22");
    assert_eq!(p[22], "catppuccin_latte", "the first v2 addition");
    assert_eq!(p[45], "dayfox");
}

/// tokyo_night's brights follow the current tokyonight.nvim extras (they used to
/// repeat the normals); everforest_dark's bright accents are the dark palette's
/// (they were the LIGHT variant's), equal to its normals as everforest.vim maps them.
#[test]
fn tokyo_night_and_everforest_brights_follow_upstream() {
    let t = Theme::by_name("tokyo_night");
    let brights: Vec<[u8; 3]> = t.palette[9..15].to_vec();
    assert_eq!(
        brights,
        vec![[0xff, 0x89, 0x9d], [0x9f, 0xe0, 0x44], [0xfa, 0xba, 0x4a], [0x8d, 0xb0, 0xff], [0xc7, 0xa9, 0xff], [0xa4, 0xda, 0xff]]
    );
    let e = Theme::by_name("everforest_dark");
    for i in 1..=6 {
        assert_eq!(e.palette[8 + i], e.palette[i], "everforest_dark bright {i} == its normal");
    }
}

/// The Rose Pine family draws its cursor in the foreground (the upstream Moon /
/// Dawn cursors are 2.1 and 1.5:1 on their backgrounds).
#[test]
fn rose_pine_variants_draw_the_cursor_in_the_foreground() {
    for name in ["rose_pine", "rose_pine_moon", "rose_pine_dawn"] {
        let t = Theme::by_name(name);
        assert_eq!(t.cursor, t.fg, "{name}");
        assert_eq!(t.cursor_text, None, "{name}");
    }
}
