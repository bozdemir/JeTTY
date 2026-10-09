//! The global summon hotkey on X11 (Linux/BSD): a passive key grab on the root
//! window, serviced by a thread that BLOCKS on its own X connection — zero
//! wakeups while idle, and a press is handled the moment the server sends it.
//!
//! This replaces `global-hotkey`'s X11 event processor, which polls its
//! connection in a loop with a 50 ms `thread::sleep`: 20 wakeups a second for
//! the life of the process (shown, hidden or idle alike), and 0–50 ms (25 ms on
//! average) added to every summon and hide. The grab is the same: the key with
//! its modifiers plus every NumLock/CapsLock combination (X matches a grab's
//! modifier state exactly), XKB detectable auto-repeat so a held key fires once,
//! and a key some other client already grabs reported as taken.
//!
//! Standard X11 only — no window manager or desktop specifics. Under Wayland this
//! reaches XWayland (keys pressed while an X11 window has focus), exactly as
//! before.

use std::fmt;

/// The modifier bits a press is matched on (X11 `KeyButMask`/`ModMask`): Shift,
/// Control, Mod1 (Alt) and Mod4 (Super). Lock (CapsLock) and Mod2 (NumLock) are
/// ignored: the key is grabbed with every combination of those two.
pub const HOTKEY_MOD_MASK: u16 = MOD_SHIFT | MOD_CONTROL | MOD_ALT | MOD_SUPER;
/// X11 `ShiftMask`.
pub const MOD_SHIFT: u16 = 1 << 0;
/// X11 `ControlMask`.
pub const MOD_CONTROL: u16 = 1 << 2;
/// X11 `Mod1Mask` (Alt).
pub const MOD_ALT: u16 = 1 << 3;
/// X11 `Mod4Mask` (Super).
pub const MOD_SUPER: u16 = 1 << 6;
/// `LockMask` and `Mod2Mask` (CapsLock, NumLock): every combination is grabbed.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
const IGNORED_MODS: [u16; 4] = [0, 1 << 4, 1 << 1, (1 << 4) | (1 << 1)];

/// The key a hotkey grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyKey {
    /// The keysym the key types — how letters, F-keys and the other named keys
    /// are found: the key typing it unshifted, else with Shift; never one that
    /// types it only with AltGr, which the grab doesn't hold (a press of that
    /// key without AltGr types something else).
    pub keysym: u32,
    /// The XKB name of the physical key (`*b"TLDE"`, `*b"AE01"` …), for keys
    /// named by their position — the digit row and the symbol keys, whose
    /// characters move between keys and levels from layout to layout. Looked
    /// up first.
    pub position: Option<[u8; 4]>,
}

/// Why the hotkey could not be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// No X server to talk to (e.g. a Wayland session without XWayland).
    NoDisplay(String),
    /// No key on the current keyboard map produces the hotkey's keysym.
    NoKeycode,
    /// Another X client already grabs this key with these modifiers.
    Taken,
    /// Any other X error.
    Failed(String),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::NoDisplay(e) => write!(f, "no X11 connection ({e})"),
            HotkeyError::NoKeycode => f.write_str("no key on this keyboard layout produces it"),
            HotkeyError::Taken => f.write_str("already grabbed by another X client"),
            HotkeyError::Failed(e) => write!(f, "X11 error: {e}"),
        }
    }
}

/// One `true` per physical press of the grabbed key with exactly the hotkey's
/// modifiers. With XKB detectable auto-repeat a held key sends repeated presses
/// and a single release: only the first press counts. Mirrors `global-hotkey`'s
/// press/release bookkeeping, so a held F9 still toggles once.
#[derive(Debug, Clone)]
pub struct PressFilter {
    keycode: u8,
    mods: u16,
    pressed: bool,
}

impl PressFilter {
    pub fn new(keycode: u8, mods: u16) -> Self {
        PressFilter { keycode, mods: mods & HOTKEY_MOD_MASK, pressed: false }
    }

    /// A KeyPress of `keycode` with modifier `state` (the event's state field):
    /// whether it is a new press of the hotkey.
    pub fn press(&mut self, keycode: u8, state: u16) -> bool {
        if keycode != self.keycode || state & HOTKEY_MOD_MASK != self.mods || self.pressed {
            return false;
        }
        self.pressed = true;
        true
    }

    /// A KeyRelease of `keycode` (whatever the modifiers): the next press counts.
    pub fn release(&mut self, keycode: u8) {
        if keycode == self.keycode {
            self.pressed = false;
        }
    }
}

/// Grab `key` with `mods` (`MOD_*` bits) on the root window of `$DISPLAY`,
/// report the outcome through `ready`, then call `on_press` for every press until
/// it returns `false` or the X connection closes. Blocks the calling thread for
/// that whole time (run it on a thread of its own); while no key is pressed the
/// thread sleeps in the kernel.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn run_x11_hotkey(
    key: HotkeyKey,
    mods: u16,
    ready: impl FnOnce(Result<(), HotkeyError>),
    mut on_press: impl FnMut() -> bool,
) {
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;

    let (conn, screen) = match x11rb::connect(None) {
        Ok(c) => c,
        Err(e) => return ready(Err(HotkeyError::NoDisplay(e.to_string()))),
    };
    let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else {
        return ready(Err(HotkeyError::NoDisplay("no X screen".into())));
    };
    detectable_auto_repeat(&conn);
    let found = match key.position.and_then(|name| keycode_named(&conn, name)) {
        Some(k) => Ok(Some(k)),
        None => keycode_for(&conn, key.keysym),
    };
    let keycode = match found {
        Ok(Some(k)) => k,
        Ok(None) => return ready(Err(HotkeyError::NoKeycode)),
        Err(e) => return ready(Err(HotkeyError::Failed(e))),
    };
    let mods = mods & HOTKEY_MOD_MASK;
    if let Err(e) = grab(&conn, root, keycode, mods) {
        return ready(Err(e));
    }
    ready(Ok(()));
    let mut filter = PressFilter::new(keycode, mods);
    loop {
        match conn.wait_for_event() {
            Ok(Event::KeyPress(e)) => {
                if filter.press(e.detail, u16::from(e.state)) && !on_press() {
                    break;
                }
            }
            Ok(Event::KeyRelease(e)) => filter.release(e.detail),
            Ok(_) => {}
            // The connection is gone (X server exit): nothing more will arrive.
            Err(_) => break,
        }
    }
}

/// XKB "detectable auto-repeat" for this client: a held key sends repeated
/// presses and ONE release instead of a release/press pair per repeat, which is
/// what lets `PressFilter` fire once per hold. Best effort — without XKB (no X
/// server in decades lacks it) a held key would just toggle repeatedly.
#[cfg(all(unix, not(target_os = "macos")))]
fn detectable_auto_repeat(conn: &impl x11rb::connection::Connection) {
    use x11rb::protocol::xkb::{self, ConnectionExt as _};
    let ok = conn.xkb_use_extension(1, 0).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.supported);
    if ok {
        let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
        let _ = conn
            .xkb_per_client_flags(
                xkb::ID::USE_CORE_KBD.into(),
                flag,
                flag,
                Default::default(),
                Default::default(),
                Default::default(),
            )
            .ok()
            .and_then(|c| c.reply().ok());
    }
}

/// The keycode that types `keysym` on the current keyboard mapping
/// ([`keycode_typing`]).
#[cfg(all(unix, not(target_os = "macos")))]
fn keycode_for(conn: &impl x11rb::connection::Connection, keysym: u32) -> Result<Option<u8>, String> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let count = max.saturating_sub(min).saturating_add(1);
    let map = conn
        .get_keyboard_mapping(min, count)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    Ok(keycode_typing(&map.keysyms, usize::from(map.keysyms_per_keycode), min, keysym))
}

/// The first keycode a core keyboard mapping (`per` keysyms for each keycode
/// from `min`) gives `keysym` at level 1 — unshifted — of group 1 or 2, else
/// at level 2 (Shift). A core mapping lists group 1's levels 1–2, then group
/// 2's, then the higher levels: a keysym only there needs AltGr, which the
/// grab doesn't hold — the old "anywhere" search made `Ctrl+Backquote` on
/// Turkish-Q grab Ctrl+the comma key (`grave` is its AltGr level), taking
/// JeTTY's own Ctrl+, from it.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn keycode_typing(syms: &[u32], per: usize, min: u8, keysym: u32) -> Option<u8> {
    let keys = || syms.chunks(per.max(1));
    [[0, 2], [1, 3]]
        .into_iter()
        .find_map(|levels| keys().position(|k| levels.iter().any(|&l| k.get(l) == Some(&keysym))))
        .and_then(|i| u8::try_from(i).ok())
        .map(|i| min.saturating_add(i))
}

/// The keycode XKB names `name` (`*b"TLDE"`) — a key's position, whatever the
/// layout types there. `None` without XKB or when no key has the name.
#[cfg(all(unix, not(target_os = "macos")))]
fn keycode_named(conn: &impl x11rb::connection::Connection, name: [u8; 4]) -> Option<u8> {
    use x11rb::protocol::xkb::{self, ConnectionExt as _};
    let names = conn
        .xkb_get_names(xkb::ID::USE_CORE_KBD.into(), xkb::NameDetail::KEY_NAMES)
        .ok()?
        .reply()
        .ok()?;
    let keys: Vec<[u8; 4]> = names.value_list.key_names?.iter().map(|k| k.name).collect();
    keycode_with_name(&keys, names.first_key, name)
}

/// The keycode of the key named `name` among XKB's key names for the keycodes
/// from `first`.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn keycode_with_name(names: &[[u8; 4]], first: u8, name: [u8; 4]) -> Option<u8> {
    let i = names.iter().position(|n| *n == name)?;
    first.checked_add(u8::try_from(i).ok()?)
}

/// Grab `keycode` + `mods` on `root` with every NumLock/CapsLock combination. A
/// grab another client holds (BadAccess) undoes the ones made and is `Taken`.
#[cfg(all(unix, not(target_os = "macos")))]
fn grab(conn: &impl x11rb::connection::Connection, root: u32, keycode: u8, mods: u16) -> Result<(), HotkeyError> {
    use x11rb::errors::ReplyError;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
    use x11rb::protocol::ErrorKind;
    let ungrab_all = || {
        for extra in IGNORED_MODS {
            if let Ok(c) = conn.ungrab_key(keycode, root, ModMask::from(mods | extra)) {
                c.ignore_error();
            }
        }
    };
    for extra in IGNORED_MODS {
        let checked = conn
            .grab_key(false, root, ModMask::from(mods | extra), keycode, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(|e| HotkeyError::Failed(e.to_string()))?
            .check();
        match checked {
            Ok(()) => {}
            Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Access => {
                ungrab_all();
                return Err(HotkeyError::Taken);
            }
            Err(e) => {
                ungrab_all();
                return Err(HotkeyError::Failed(e.to_string()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const F9: u8 = 75;

    #[test]
    fn a_press_fires_once_until_released() {
        let mut f = PressFilter::new(F9, 0);
        assert!(f.press(F9, 0), "the first press fires");
        // Detectable auto-repeat: a held key sends more presses, no release.
        assert!(!f.press(F9, 0), "auto-repeat does not fire again");
        assert!(!f.press(F9, 0));
        f.release(F9);
        assert!(f.press(F9, 0), "the next physical press fires");
    }

    #[test]
    fn locks_are_ignored_and_other_modifiers_are_not() {
        let mut f = PressFilter::new(F9, 0);
        // NumLock (Mod2) and CapsLock (Lock) on: still the hotkey.
        assert!(f.press(F9, (1 << 4) | (1 << 1)));
        f.release(F9);
        // Ctrl+F9 is a different chord.
        assert!(!f.press(F9, MOD_CONTROL));
        // Mouse-button bits in the state field (Button1 = 1 << 8) don't matter.
        assert!(f.press(F9, 1 << 8));
    }

    #[test]
    fn a_chord_needs_exactly_its_modifiers() {
        let mut f = PressFilter::new(F9, MOD_CONTROL | MOD_SHIFT);
        assert!(!f.press(F9, MOD_CONTROL), "Shift missing");
        assert!(!f.press(F9, MOD_CONTROL | MOD_SHIFT | MOD_ALT), "Alt extra");
        assert!(f.press(F9, MOD_CONTROL | MOD_SHIFT | (1 << 4)), "NumLock is ignored");
    }

    #[test]
    fn other_keys_never_fire_or_release() {
        let mut f = PressFilter::new(F9, 0);
        assert!(!f.press(F9 + 1, 0));
        assert!(f.press(F9, 0));
        f.release(F9 + 1);
        assert!(!f.press(F9, 0), "another key's release does not re-arm");
    }

    // Keysyms (X11 keysymdef.h).
    const GRAVE: u32 = 0x60;
    const COMMA: u32 = 0x2c;
    const SEMICOLON: u32 = 0x3b;
    const QUOTEDBL: u32 = 0x22;
    const LESS: u32 = 0x3c;
    const NOSYMBOL: u32 = 0;

    /// keycodes 49–51 of `setxkbmap tr` (xmodmap -pke on Xvfb): TLDE, Shift_L,
    /// BKSL — 7 keysyms each: group 1 levels 1–2, group 2's, then levels 3–4.
    fn turkish_q() -> Vec<u32> {
        vec![
            QUOTEDBL, 0xe9, QUOTEDBL, 0xe9, LESS, 0xb0, LESS, // 49 TLDE: " é, AltGr <
            0xffe1, NOSYMBOL, 0xffe1, NOSYMBOL, NOSYMBOL, NOSYMBOL, NOSYMBOL, // 50 Shift_L
            COMMA, SEMICOLON, COMMA, SEMICOLON, GRAVE, 0xfe50, GRAVE, // 51 BKSL: , ; AltGr `
        ]
    }

    #[test]
    fn a_keysym_only_altgr_types_grabs_no_key() {
        // Ctrl+Backquote on Turkish-Q: `grave` is the comma key's AltGr level —
        // grabbing that key took Ctrl+, (JeTTY's Settings chord) and never fired
        // on a press that types a backquote.
        let tr = turkish_q();
        assert_eq!(keycode_typing(&tr, 7, 49, GRAVE), None);
        assert_eq!(keycode_typing(&tr, 7, 49, LESS), None);
        // Levels 1 and 2 are the key's own.
        assert_eq!(keycode_typing(&tr, 7, 49, COMMA), Some(51));
        assert_eq!(keycode_typing(&tr, 7, 49, SEMICOLON), Some(51));
        assert_eq!(keycode_typing(&tr, 7, 49, QUOTEDBL), Some(49));
        // US: `grave` is TLDE's level 1.
        let us = [GRAVE, 0x7e, GRAVE, 0x7e];
        assert_eq!(keycode_typing(&us, 4, 49, GRAVE), Some(49));
    }

    #[test]
    fn level_1_wins_and_a_second_group_counts() {
        // `T` (Shift) on keycode 10, `T` unshifted on keycode 11: the unshifted one.
        let syms = [0x74, 0x54, 0x74, 0x54, 0x54, 0x74, 0x54, 0x74];
        assert_eq!(keycode_typing(&syms, 4, 10, 0x54), Some(11));
        // ru,us: Cyrillic in group 1, the T key's `t` / `T` in group 2.
        let ru_us = [0x6c5, 0x6e5, 0x74, 0x54];
        assert_eq!(keycode_typing(&ru_us, 4, 28, 0x54), Some(28));
        assert_eq!(keycode_typing(&ru_us, 4, 28, 0x74), Some(28));
        // A narrow mapping (2 keysyms per keycode) is read the same way.
        assert_eq!(keycode_typing(&[COMMA, SEMICOLON, GRAVE, 0x7e], 2, 8, GRAVE), Some(9));
    }

    #[test]
    fn a_position_is_found_by_its_xkb_name() {
        let names = [*b"TLDE", *b"LFSH", *b"BKSL"];
        assert_eq!(keycode_with_name(&names, 49, *b"TLDE"), Some(49));
        assert_eq!(keycode_with_name(&names, 49, *b"BKSL"), Some(51));
        assert_eq!(keycode_with_name(&names, 49, *b"AE01"), None);
        assert_eq!(keycode_with_name(&names, 254, *b"BKSL"), None, "past keycode 255");
    }

    #[test]
    fn the_ignored_combinations_cover_both_locks() {
        let lock = 1u16 << 1;
        let numlock = 1u16 << 4;
        assert_eq!(IGNORED_MODS, [0, numlock, lock, numlock | lock]);
        assert_eq!(HOTKEY_MOD_MASK & (lock | numlock), 0, "locks are never matched on");
    }
}
