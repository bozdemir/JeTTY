//! Tests for the runtime theme registry (built-ins + user-imported themes).
//!
//! The registry is a process-global `RwLock<Vec<Theme>>`, so these tests mutate
//! shared state. They live in their OWN test binary (separate from `theme.rs`,
//! which relies on the EMPTY-registry built-in fallback) and serialize registry
//! mutations through a mutex so parallel test threads never observe a half-set
//! registry.

use std::borrow::Cow;
use std::sync::Mutex;

use jetty_core::Theme;

static SERIAL: Mutex<()> = Mutex::new(());

fn user_theme(name: &str, display: &str, bg: [u8; 4]) -> Theme {
    Theme {
        name: Cow::Owned(name.to_string()),
        display_name: Cow::Owned(display.to_string()),
        bg,
        fg: [200, 200, 200],
        cursor: [255, 255, 255],
        palette: [[0, 0, 0]; 16],
    }
}

#[test]
fn empty_registry_falls_back_to_builtins() {
    let _g = SERIAL.lock().unwrap();
    jetty_core::set_registry(Vec::new());
    // With no registry, every accessor sees the 22 built-ins.
    assert_eq!(jetty_core::theme_count(), jetty_core::theme::PRESETS.len());
    assert_eq!(jetty_core::theme_index("dracula"), Some(3));
    assert_eq!(jetty_core::theme_at(0).name.as_ref(), "catppuccin_mocha");
    // Out-of-range index never panics — it falls back to catppuccin_mocha.
    assert_eq!(jetty_core::theme_at(9999).name.as_ref(), "catppuccin_mocha");
    jetty_core::set_registry(Vec::new());
}

#[test]
fn registry_orders_builtins_then_user_and_shadows() {
    let _g = SERIAL.lock().unwrap();
    // Merge = built-ins (PRESETS order), a user theme shadowing `dracula` in place,
    // then a brand-new user theme appended.
    let mut entries = jetty_core::builtins();
    let dracula_idx =
        entries.iter().position(|t| t.name.as_ref() == "dracula").expect("dracula built-in");
    entries[dracula_idx] = user_theme("dracula", "My Dracula", [1, 2, 3, 255]);
    entries.push(user_theme("mine", "My Theme", [9, 9, 9, 255]));
    let appended = entries.len() - 1;
    jetty_core::set_registry(entries);

    // theme_count reflects the merged length (built-ins + 1 appended).
    assert_eq!(jetty_core::theme_count(), jetty_core::theme::PRESETS.len() + 1);

    // theme_list is ordered: built-ins first (index 0 still catppuccin), user last.
    let list = jetty_core::theme_list();
    assert_eq!(list[0].0, "catppuccin_mocha");
    assert_eq!(list[appended], ("mine".to_string(), "My Theme".to_string()));

    // A user theme named `dracula` SHADOWS the built-in in `by_name` and keeps its
    // ordered position.
    let d = Theme::by_name("dracula");
    assert_eq!(d.display_name.as_ref(), "My Dracula");
    assert_eq!(d.bg, [1, 2, 3, 255]);
    assert_eq!(jetty_core::theme_index("dracula"), Some(dracula_idx));

    // by_name for a name absent from the registry still falls back to a built-in.
    assert_eq!(Theme::by_name("nord").name.as_ref(), "nord");
    // An unknown name falls back to catppuccin_mocha.
    assert_eq!(Theme::by_name("nope_xyz").name.as_ref(), "catppuccin_mocha");

    jetty_core::set_registry(Vec::new());
}

#[test]
fn theme_at_reclamps_when_registry_shrinks() {
    let _g = SERIAL.lock().unwrap();
    // A larger list, then a shrink: an index valid before the shrink must NOT panic
    // afterwards — it falls back to catppuccin_mocha (the app also re-clamps
    // theme_idx on reload).
    let big = jetty_core::builtins();
    let last = big.len() - 1;
    jetty_core::set_registry(big);
    assert!(!jetty_core::theme_at(last).name.as_ref().is_empty());

    jetty_core::set_registry(vec![user_theme("solo", "Solo", [0, 0, 0, 255])]);
    assert_eq!(jetty_core::theme_count(), 1);
    // The old `last` index is now stale → safe fallback, no panic.
    assert_eq!(jetty_core::theme_at(last).name.as_ref(), "catppuccin_mocha");
    assert_eq!(jetty_core::theme_at(0).name.as_ref(), "solo");

    jetty_core::set_registry(Vec::new());
}
