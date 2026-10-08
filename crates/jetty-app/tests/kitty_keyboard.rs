//! End-to-end kitty keyboard protocol: a real `Terminal` tracks the flag stack
//! a program pushes, and the key decision every window uses encodes per those
//! flags. No window, no GPU, no PTY.

use jetty_app::input::{
    decide_key_event, KeyAction, KeyEventKind, KeyInput, KeyMods, KeyModes, KeyOptions,
};
use jetty_app::keymap::KeyMap;
use jetty_core::Terminal;
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey};

/// The modes the app samples from the active tab's terminal per key event.
fn modes(t: &Terminal) -> KeyModes {
    KeyModes {
        app_cursor: t.app_cursor_keys(),
        app_keypad: false,
        alt_screen: t.alt_screen(),
        kitty_flags: t.kitty_keyboard_flags(),
    }
}

fn shift_enter(kind: KeyEventKind, enter: &Key) -> KeyInput<'_> {
    KeyInput {
        physical: PhysicalKey::Code(KeyCode::Enter),
        logical: enter,
        key_without_modifiers: enter,
        text: Some("\r"),
        location: KeyLocation::Standard,
        kind,
        mods: KeyMods { shift: true, ..KeyMods::default() },
    }
}

/// A terminal the way every tab is created: kitty keyboard support on.
fn terminal() -> Terminal {
    let mut t = Terminal::new(80, 24);
    t.set_kitty_keyboard(true);
    t
}

#[test]
fn shift_enter_reaches_a_kitty_program_as_csi_13_2u() {
    let km = KeyMap::defaults();
    let enter = Key::Named(NamedKey::Enter);
    let ev = shift_enter(KeyEventKind::Press, &enter);
    let mut t = terminal();
    // Before the program opts in: the legacy byte (a shell just sees Enter).
    assert_eq!(
        decide_key_event(&km, &ev, &modes(&t), &KeyOptions::default(), false),
        KeyAction::Send(b"\r".to_vec())
    );
    // The program pushes "disambiguate escape codes" (Claude Code, fish 4, nvim).
    t.feed(b"\x1b[>1u");
    assert_eq!(t.kitty_keyboard_flags(), 1);
    assert_eq!(
        decide_key_event(&km, &ev, &modes(&t), &KeyOptions::default(), false),
        KeyAction::Send(b"\x1b[13;2u".to_vec()),
        "Shift+Enter must be distinguishable from Enter (Claude Code's newline)"
    );
    // Popping the flags restores the legacy encoding.
    t.feed(b"\x1b[<u");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    assert_eq!(
        decide_key_event(&km, &ev, &modes(&t), &KeyOptions::default(), false),
        KeyAction::Send(b"\r".to_vec())
    );
}

#[test]
fn releases_reach_only_programs_that_asked_for_event_types() {
    let km = KeyMap::defaults();
    let enter = Key::Named(NamedKey::Enter);
    let release = shift_enter(KeyEventKind::Release, &enter);
    let mut t = terminal();
    // Legacy and plain "disambiguate": no release events at all.
    assert_eq!(decide_key_event(&km, &release, &modes(&t), &KeyOptions::default(), false), KeyAction::None);
    t.feed(b"\x1b[>1u");
    assert_eq!(decide_key_event(&km, &release, &modes(&t), &KeyOptions::default(), false), KeyAction::None);
    // "Report all keys" + "event types": the release is reported (`:3`).
    t.feed(b"\x1b[>11u");
    assert_eq!(t.kitty_keyboard_flags(), 11);
    assert_eq!(
        decide_key_event(&km, &release, &modes(&t), &KeyOptions::default(), false),
        KeyAction::Send(b"\x1b[13;2:3u".to_vec())
    );
}

#[test]
fn the_flag_query_is_answered_with_the_pushed_flags() {
    let mut t = terminal();
    t.feed(b"\x1b[>5u\x1b[?u");
    assert_eq!(t.drain_pty_writes(), b"\x1b[?5u");
}

#[test]
fn the_alt_screen_keeps_its_own_flag_stack() {
    let mut t = terminal();
    t.feed(b"\x1b[>1u");
    assert_eq!(t.kitty_keyboard_flags(), 1);
    // A full-screen TUI on the alt screen starts from a clean stack…
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    // …and leaving it restores the primary screen's flags.
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.kitty_keyboard_flags(), 1);
}
