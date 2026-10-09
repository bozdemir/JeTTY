//! The summon hotkey on X11 (Linux/BSD): `global-hotkey`'s parsed `HotKey` (its
//! `from_str` stays the config syntax) grabbed through `jetty_platform::hotkey`,
//! whose listener thread blocks on its X connection instead of polling it every
//! 50 ms — no idle wakeups, and no 0–50 ms delay on every summon (see there).
//!
//! The key table is `global-hotkey` 0.8's own (`platform_impl/x11`, Apache-2.0
//! OR MIT, © Tauri Programme within The Commons Conservancy), so a configured
//! hotkey grabs the key it did before — with two upstream slips fixed:
//! `NumLock` named F1's keysym, and `Quote` a keysym no layout carries. The
//! digit row and the symbol keys go by position ([`position`]), as their names
//! say and as on macOS: their keysyms move between keys and levels from layout
//! to layout.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use jetty_platform::hotkey::{HotkeyKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_SUPER};
use xkeysym::key;

/// The X11 keysym bound for `code` (`None`: the key has no X11 binding).
pub(crate) fn keysym(code: Code) -> Option<u32> {
    Some(match code {
        Code::KeyA => key::A,
        Code::KeyB => key::B,
        Code::KeyC => key::C,
        Code::KeyD => key::D,
        Code::KeyE => key::E,
        Code::KeyF => key::F,
        Code::KeyG => key::G,
        Code::KeyH => key::H,
        Code::KeyI => key::I,
        Code::KeyJ => key::J,
        Code::KeyK => key::K,
        Code::KeyL => key::L,
        Code::KeyM => key::M,
        Code::KeyN => key::N,
        Code::KeyO => key::O,
        Code::KeyP => key::P,
        Code::KeyQ => key::Q,
        Code::KeyR => key::R,
        Code::KeyS => key::S,
        Code::KeyT => key::T,
        Code::KeyU => key::U,
        Code::KeyV => key::V,
        Code::KeyW => key::W,
        Code::KeyX => key::X,
        Code::KeyY => key::Y,
        Code::KeyZ => key::Z,
        Code::Backslash => key::backslash,
        Code::BracketLeft => key::bracketleft,
        Code::BracketRight => key::bracketright,
        Code::Backquote => key::quoteleft,
        Code::Comma => key::comma,
        Code::Digit0 => key::_0,
        Code::Digit1 => key::_1,
        Code::Digit2 => key::_2,
        Code::Digit3 => key::_3,
        Code::Digit4 => key::_4,
        Code::Digit5 => key::_5,
        Code::Digit6 => key::_6,
        Code::Digit7 => key::_7,
        Code::Digit8 => key::_8,
        Code::Digit9 => key::_9,
        Code::Equal => key::equal,
        Code::Minus => key::minus,
        Code::Period => key::period,
        Code::Quote => key::apostrophe,
        Code::Semicolon => key::semicolon,
        Code::Slash => key::slash,
        Code::Backspace => key::BackSpace,
        Code::CapsLock => key::Caps_Lock,
        Code::Enter => key::Return,
        Code::Space => key::space,
        Code::Tab => key::Tab,
        Code::Delete => key::Delete,
        Code::End => key::End,
        Code::Home => key::Home,
        Code::Insert => key::Insert,
        Code::PageDown => key::Page_Down,
        Code::PageUp => key::Page_Up,
        Code::ArrowDown => key::Down,
        Code::ArrowLeft => key::Left,
        Code::ArrowRight => key::Right,
        Code::ArrowUp => key::Up,
        Code::Numpad0 => key::KP_0,
        Code::Numpad1 => key::KP_1,
        Code::Numpad2 => key::KP_2,
        Code::Numpad3 => key::KP_3,
        Code::Numpad4 => key::KP_4,
        Code::Numpad5 => key::KP_5,
        Code::Numpad6 => key::KP_6,
        Code::Numpad7 => key::KP_7,
        Code::Numpad8 => key::KP_8,
        Code::Numpad9 => key::KP_9,
        Code::NumpadAdd => key::KP_Add,
        Code::NumpadDecimal => key::KP_Decimal,
        Code::NumpadDivide => key::KP_Divide,
        Code::NumpadMultiply => key::KP_Multiply,
        Code::NumpadSubtract => key::KP_Subtract,
        Code::Escape => key::Escape,
        Code::PrintScreen => key::Print,
        Code::ScrollLock => key::Scroll_Lock,
        Code::NumLock => key::Num_Lock,
        Code::F1 => key::F1,
        Code::F2 => key::F2,
        Code::F3 => key::F3,
        Code::F4 => key::F4,
        Code::F5 => key::F5,
        Code::F6 => key::F6,
        Code::F7 => key::F7,
        Code::F8 => key::F8,
        Code::F9 => key::F9,
        Code::F10 => key::F10,
        Code::F11 => key::F11,
        Code::F12 => key::F12,
        Code::F13 => key::F13,
        Code::F14 => key::F14,
        Code::F15 => key::F15,
        Code::F16 => key::F16,
        Code::F17 => key::F17,
        Code::F18 => key::F18,
        Code::F19 => key::F19,
        Code::F20 => key::F20,
        Code::F21 => key::F21,
        Code::F22 => key::F22,
        Code::F23 => key::F23,
        Code::F24 => key::F24,
        Code::AudioVolumeDown => key::XF86_AudioLowerVolume,
        Code::AudioVolumeMute => key::XF86_AudioMute,
        Code::AudioVolumeUp => key::XF86_AudioRaiseVolume,
        Code::MediaPlay => key::XF86_AudioPlay,
        Code::MediaPause => key::XF86_AudioPause,
        Code::MediaStop => key::XF86_AudioStop,
        Code::MediaTrackNext => key::XF86_AudioNext,
        Code::MediaTrackPrevious => key::XF86_AudioPrev,
        Code::Pause => key::Pause,
        _ => return None,
    })
}

/// The XKB name of the key at `code`'s position, for the digit row and the
/// symbol keys: `Ctrl+Backquote` is the key below Esc on every layout, as on
/// macOS — by keysym it was a key typing a backquote anywhere, which on
/// Turkish-Q is the comma key's AltGr level. Letters and named keys go by
/// keysym (`None`).
pub(crate) fn position(code: Code) -> Option<[u8; 4]> {
    Some(*match code {
        Code::Backquote => b"TLDE",
        Code::Digit1 => b"AE01",
        Code::Digit2 => b"AE02",
        Code::Digit3 => b"AE03",
        Code::Digit4 => b"AE04",
        Code::Digit5 => b"AE05",
        Code::Digit6 => b"AE06",
        Code::Digit7 => b"AE07",
        Code::Digit8 => b"AE08",
        Code::Digit9 => b"AE09",
        Code::Digit0 => b"AE10",
        Code::Minus => b"AE11",
        Code::Equal => b"AE12",
        Code::BracketLeft => b"AD11",
        Code::BracketRight => b"AD12",
        Code::Backslash => b"BKSL",
        Code::Semicolon => b"AC10",
        Code::Quote => b"AC11",
        Code::Comma => b"AB08",
        Code::Period => b"AB09",
        Code::Slash => b"AB10",
        _ => return None,
    })
}

/// `mods` as X11 modifier bits (Super and Meta are both Mod4, as upstream).
pub(crate) fn mods(mods: Modifiers) -> u16 {
    let mut x = 0;
    if mods.contains(Modifiers::SHIFT) {
        x |= MOD_SHIFT;
    }
    if mods.intersects(Modifiers::SUPER | Modifiers::META) {
        x |= MOD_SUPER;
    }
    if mods.contains(Modifiers::ALT) {
        x |= MOD_ALT;
    }
    if mods.contains(Modifiers::CONTROL) {
        x |= MOD_CONTROL;
    }
    x
}

/// Grab `hotkey` and call `on_press` for each press, on the calling thread: it
/// blocks for the life of the X connection (spawn a thread). `ready` gets the
/// grab's outcome before any press — `Err` with a user-facing reason when the key
/// cannot be grabbed (nothing is listened for then).
pub(crate) fn run(hotkey: HotKey, ready: impl FnOnce(Result<(), String>), on_press: impl FnMut() -> bool) {
    let Some(sym) = keysym(hotkey.key) else {
        return ready(Err(format!("{} has no X11 key", hotkey.key)));
    };
    let key = HotkeyKey { keysym: sym, position: position(hotkey.key) };
    jetty_platform::hotkey::run_x11_hotkey(key, mods(hotkey.mods), |r| ready(r.map_err(|e| e.to_string())), on_press);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn the_default_and_common_chords_resolve() {
        let f9 = HotKey::from_str("F9").unwrap();
        assert_eq!(keysym(f9.key), Some(key::F9));
        assert_eq!(mods(f9.mods), 0);
        let chord = HotKey::from_str("Ctrl+Shift+Alt+Super+KeyT").unwrap();
        assert_eq!(keysym(chord.key), Some(key::T));
        assert_eq!(mods(chord.mods), MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_SUPER);
        let grave = HotKey::from_str("Ctrl+Backquote").unwrap();
        assert_eq!(keysym(grave.key), Some(key::quoteleft));
        assert_eq!(mods(grave.mods), MOD_CONTROL);
    }

    #[test]
    fn every_function_key_binds_its_own_keysym() {
        for n in 1..=24u32 {
            let hk = HotKey::from_str(&format!("F{n}")).unwrap();
            assert_eq!(keysym(hk.key), Some(key::F1 + n - 1), "F{n}");
        }
    }

    #[test]
    fn the_digit_row_and_the_symbol_keys_go_by_position() {
        let grave = HotKey::from_str("Ctrl+Backquote").unwrap();
        assert_eq!(position(grave.key), Some(*b"TLDE"));
        for (code, name) in [
            (Code::Digit1, b"AE01"),
            (Code::Digit0, b"AE10"),
            (Code::Minus, b"AE11"),
            (Code::BracketRight, b"AD12"),
            (Code::Backslash, b"BKSL"),
            (Code::Quote, b"AC11"),
            (Code::Slash, b"AB10"),
        ] {
            assert_eq!(position(code), Some(*name), "{code}");
            assert!(keysym(code).is_some(), "{code} keeps its keysym (the fallback)");
        }
        // Letters, F-keys and the other named keys go by keysym.
        for code in [Code::KeyT, Code::F9, Code::Space, Code::Enter, Code::Numpad5] {
            assert_eq!(position(code), None, "{code}");
        }
    }

    #[test]
    fn the_upstream_slips_are_fixed() {
        assert_eq!(keysym(Code::NumLock), Some(key::Num_Lock));
        assert_eq!(keysym(Code::Quote), Some(key::apostrophe));
    }

    #[test]
    fn a_key_without_an_x11_binding_is_refused_before_any_x_call() {
        // `Fn` has no keysym: refused up front, with the key named.
        let hk = HotKey::new(None, Code::Fn);
        let mut got = None;
        run(hk, |r| got = Some(r), || true);
        let err = got.expect("ready is always called").unwrap_err();
        assert!(err.contains("Fn"), "{err}");
    }
}
