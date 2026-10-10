use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey};

use crate::keymap::{KeyMap, Mods};

/// High-level action decoded from a key press event.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum KeyAction {
    TogglePanel,
    ClosePanel,
    /// Open the fuzzy command palette (Ctrl+Shift+P / macOS Cmd+Shift+P).
    OpenPalette,
    /// Open a new terminal tab (Ctrl+Shift+T).
    NewTab,
    /// Close the active tab (Ctrl+Shift+W).
    CloseTab,
    /// Detach the active tab into its own window, or — when already in a detached
    /// window — reattach it to the main window (Ctrl+Shift+D).
    DetachTab,
    /// Switch to the next tab, wrapping (Ctrl+Tab).
    NextTab,
    /// Switch to the previous tab, wrapping (Ctrl+Shift+Tab).
    PrevTab,
    /// Jump to tab `n` (0-based; Ctrl+1..Ctrl+9 → 0..8), clamped to range.
    SelectTab(usize),
    OpacityUp,
    OpacityDown,
    ScrollPageUp,
    ScrollPageDown,
    /// Increase font size by one logical point.
    FontUp,
    /// Decrease font size by one logical point.
    FontDown,
    /// Reset font size to the default (16.0).
    FontReset,
    /// Copy the current selection to the clipboard (Ctrl+Shift+C).
    Copy,
    /// Paste from the clipboard into the PTY (Ctrl+Shift+V).
    Paste,
    /// Toggle the scrollback-search bar (Ctrl+Shift+F).
    SearchToggle,
    /// Jump the viewport to the previous (older) OSC 133 prompt (Ctrl+Shift+Z).
    PrevPrompt,
    /// Jump the viewport to the next (newer) OSC 133 prompt (Ctrl+Shift+X).
    NextPrompt,
    /// Select the entire scrollback + screen (macOS Cmd+A; remappable).
    SelectAll,
    /// Request application quit (macOS Cmd+Q; remappable). Opens the quit
    /// confirmation.
    Quit,
    /// Enter hint mode (Ctrl+Shift+H): overlay home-row labels on every visible
    /// URL / path / git-hash / IPv4 for mouse-free copy (or Alt → open a URL).
    HintMode,
    /// Enter keyboard copy-mode (Ctrl+Shift+Space): a modal vi-cursor over the
    /// viewport + scrollback for keyboard-only text selection and yank.
    CopyMode,
    /// Run the current selection in a NEW tab (Ctrl+Shift+Enter) — the
    /// browser's "open link in a new tab" gesture for commands. No-op without
    /// a selection. The chord is Ctrl+Shift-only; plain/Shift/Ctrl Enter still
    /// reach the PTY (`\r`, or `CSI 13;2u` / `CSI 13;5u` once a program enables
    /// the kitty keyboard protocol). It DOES shadow that protocol's
    /// Ctrl+Shift+Enter (`CSI 13;6u`); the opt-out is
    /// `[keys] run_selection = ""`. NumpadEnter deliberately unmatched.
    RunSelection,
    /// Toggle OS fullscreen on the window that has focus (F11; macOS also
    /// Cmd+Ctrl+F). Transient per-window view state — it never writes
    /// `window_mode`, so it costs no disk I/O on the key path.
    ToggleFullscreen,
    /// Step to the next / previous theme (no default chord; `[keys]
    /// next_theme` / `prev_theme`).
    NextTheme,
    PrevTheme,
    /// Open the window's context menu at the text cursor, its first enabled
    /// row highlighted — or close the menu that is open (the Menu key).
    ContextMenu,
    /// Copy / select the last command's output (shell integration; no
    /// default chord: `[keys] copy_last_output` / `select_last_output`).
    CopyLastOutput,
    SelectLastOutput,
    /// Raw bytes to write to the PTY.
    Send(Vec<u8>),
    None,
}

impl KeyAction {
    /// Whether auto-repeat — the chord held down — runs the action again. The
    /// actions that STEP repeat, like a held arrow key: scrolling, the prompt /
    /// tab / theme steppers, font size and opacity (and typed bytes, of
    /// course). Every other command opens, closes, toggles or sets something
    /// — Settings, the palette and the other overlays, the Menu key, F11,
    /// detach, a new tab, run-selection, copy and paste — and runs once per
    /// press: a held F11 flipped fullscreen at the repeat rate, a held
    /// Ctrl+Shift+D detached tab after tab.
    pub fn repeats(&self) -> bool {
        matches!(
            self,
            KeyAction::ScrollPageUp
                | KeyAction::ScrollPageDown
                | KeyAction::PrevPrompt
                | KeyAction::NextPrompt
                | KeyAction::NextTab
                | KeyAction::PrevTab
                | KeyAction::NextTheme
                | KeyAction::PrevTheme
                | KeyAction::FontUp
                | KeyAction::FontDown
                | KeyAction::OpacityUp
                | KeyAction::OpacityDown
                | KeyAction::Send(_)
        )
    }
}

// ── Key event model (one path for the main AND detached windows) ─────────────

/// Kitty keyboard protocol progressive-enhancement flags (`CSI = flags u`,
/// <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), carried in
/// [`KeyModes::kitty_flags`]. Bit values are the spec's.
pub const KITTY_DISAMBIGUATE: u8 = 0b1;
/// Report key repeat and release events (`CSI …;mods:2u` / `CSI …;mods:3u`).
pub const KITTY_REPORT_EVENT_TYPES: u8 = 0b10;
/// Report the shifted key and the base-layout (PC-101) key as sub-fields.
pub const KITTY_REPORT_ALTERNATE_KEYS: u8 = 0b100;
/// Report every key — text and modifier keys too — as an escape code.
pub const KITTY_REPORT_ALL_KEYS: u8 = 0b1000;
/// With [`KITTY_REPORT_ALL_KEYS`]: embed the key's text as code points.
pub const KITTY_REPORT_ASSOCIATED_TEXT: u8 = 0b1_0000;

/// Press / auto-repeat / release. winit reports auto-repeat as a press with
/// `KeyEvent::repeat == true`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyEventKind {
    #[default]
    Press,
    Repeat,
    Release,
}

impl KeyEventKind {
    /// From winit's `KeyEvent::state` + `KeyEvent::repeat`.
    pub fn from_winit(state: winit::event::ElementState, repeat: bool) -> Self {
        match (state, repeat) {
            (winit::event::ElementState::Released, _) => KeyEventKind::Release,
            (winit::event::ElementState::Pressed, true) => KeyEventKind::Repeat,
            (winit::event::ElementState::Pressed, false) => KeyEventKind::Press,
        }
    }
}

/// Modifier state of one key event. `lalt`/`ralt` say WHICH Alt/Option key is
/// held where the platform reports sides (winit does on macOS); elsewhere both
/// stay `false` and only `alt` matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
    pub lalt: bool,
    pub ralt: bool,
}

impl KeyMods {
    /// From winit's `Modifiers` (the `ModifiersChanged` payload). Keeps the
    /// Option side, which `macos_option_as_alt = "left" | "right"` needs.
    pub fn from_winit(m: &winit::event::Modifiers) -> Self {
        use winit::keyboard::ModifiersKeyState::Pressed;
        KeyMods {
            lalt: m.lalt_state() == Pressed,
            ralt: m.ralt_state() == Pressed,
            ..KeyMods::from_state(m.state())
        }
    }

    /// From a bare `ModifiersState` (no side information).
    pub fn from_state(s: winit::keyboard::ModifiersState) -> Self {
        KeyMods {
            ctrl: s.control_key(),
            shift: s.shift_key(),
            alt: s.alt_key(),
            super_: s.super_key(),
            lalt: false,
            ralt: false,
        }
    }
}

/// Keyboard modes the program in the active tab requested (sample them from
/// that tab's `Terminal` per event).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM (`CSI ? 1 h`): arrows and Home/End send SS3 (`\eOA`, `\eOH`, …),
    /// which is what terminfo's `kcuu1`/`khome`/`kend` promise after `smkx`.
    pub app_cursor: bool,
    /// DECKPAM (`ESC =`). xterm lets NumLock override application-keypad mode,
    /// and winit only reports keypad CHARACTERS while NumLock is on, so keypad
    /// digits / operators / Enter stay numeric (`1`, `+`, `\r`) in both modes
    /// (sending SS3 there broke numpad typing at oh-my-zsh prompts, which `smkx`
    /// both modes). NumLock-off keypad keys arrive as navigation keys and follow
    /// DECCKM like their main-block twins.
    pub app_keypad: bool,
    /// The alternate screen is active (a full-screen TUI owns the display).
    pub alt_screen: bool,
    /// Kitty keyboard protocol flags (`KITTY_*`); 0 = legacy xterm encoding.
    pub kitty_flags: u8,
}

/// macOS only: which Option key(s) act as Meta (ESC-prefix / the kitty `alt`
/// bit) instead of composing characters. Config key `macos_option_as_alt`:
/// `"none"` (default — Option types `@ { } [ ] | ~ …` on non-US layouts, and
/// `©`, `∫`, … on US), `"left"`, `"right"` or `"both"`. Ignored on other
/// platforms, where Alt is always Meta. The app must hand the SAME value to
/// winit (`OptionAsAlt::to_winit`) so winit stops composing on the Meta side(s)
/// and reports the plain key there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsAlt {
    #[default]
    None,
    Left,
    Right,
    Both,
}

impl OptionAsAlt {
    /// The config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            OptionAsAlt::None => "none",
            OptionAsAlt::Left => "left",
            OptionAsAlt::Right => "right",
            OptionAsAlt::Both => "both",
        }
    }

    /// Lenient parse (case-insensitive, winit's `only_left`/`only_right`
    /// spellings accepted). Anything unknown is `None`, so one typo can never
    /// fail a whole config load.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "only_left" | "onlyleft" => OptionAsAlt::Left,
            "right" | "only_right" | "onlyright" => OptionAsAlt::Right,
            "both" => OptionAsAlt::Both,
            _ => OptionAsAlt::None,
        }
    }

    /// The matching winit window setting (`WindowAttributesExtMacOS::
    /// with_option_as_alt` / `WindowExtMacOS::set_option_as_alt`).
    #[cfg(target_os = "macos")]
    pub fn to_winit(self) -> winit::platform::macos::OptionAsAlt {
        use winit::platform::macos::OptionAsAlt as W;
        match self {
            OptionAsAlt::None => W::None,
            OptionAsAlt::Left => W::OnlyLeft,
            OptionAsAlt::Right => W::OnlyRight,
            OptionAsAlt::Both => W::Both,
        }
    }
}

impl Serialize for OptionAsAlt {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OptionAsAlt {
    /// Accepts the string spellings and, leniently, a bool (`true` = both).
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Bool(bool),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Str(s) => OptionAsAlt::parse(&s),
            Raw::Bool(true) => OptionAsAlt::Both,
            Raw::Bool(false) => OptionAsAlt::None,
        })
    }
}

/// Platform behaviour knobs for key encoding. A plain struct (not `cfg!`) so the
/// macOS Option rules are testable on every platform; the app builds it with
/// [`KeyOptions::native`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOptions {
    /// Apply macOS Option semantics (compose vs Meta per `option_as_alt`).
    pub macos: bool,
    pub option_as_alt: OptionAsAlt,
}

impl KeyOptions {
    /// This platform's rules with the configured `macos_option_as_alt`.
    pub fn native(option_as_alt: OptionAsAlt) -> Self {
        KeyOptions { macos: cfg!(target_os = "macos"), option_as_alt }
    }
}

/// One key event, decoupled from winit's `KeyEvent` (which tests cannot
/// construct). Build it from the event's fields + `key_without_modifiers()`.
#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    pub physical: PhysicalKey,
    /// `KeyEvent::logical_key`: Shift / AltGr / (macOS) Option are applied,
    /// Ctrl is not.
    pub logical: &'a Key,
    /// `KeyEvent::key_without_modifiers()`: the unshifted key on the current
    /// layout (the kitty protocol's key code; the macOS binding-match key).
    pub key_without_modifiers: &'a Key,
    /// `KeyEvent::text`. winit leaves Ctrl out of it; Shift / AltGr / (macOS)
    /// Option are applied.
    pub text: Option<&'a str>,
    pub location: KeyLocation,
    pub kind: KeyEventKind,
    pub mods: KeyMods,
}

impl<'a> KeyInput<'a> {
    /// A press with no text and no layout extras — what the legacy
    /// [`decide_key`] wrapper (and simple tests) need.
    pub fn press(physical: PhysicalKey, logical: &'a Key, mods: KeyMods) -> Self {
        KeyInput {
            physical,
            logical,
            key_without_modifiers: logical,
            text: None,
            location: KeyLocation::Standard,
            kind: KeyEventKind::Press,
            mods,
        }
    }
}

/// Is Alt acting as Meta for this event (ESC-prefix in legacy encoding, the
/// `alt` bit in the kitty protocol)? Always on non-macOS platforms; on macOS
/// only for the Option side(s) `macos_option_as_alt` names — otherwise Option
/// composes characters.
pub fn alt_is_meta(mods: KeyMods, opts: &KeyOptions) -> bool {
    if !mods.alt {
        return false;
    }
    if !opts.macos {
        return true;
    }
    match opts.option_as_alt {
        OptionAsAlt::None => false,
        OptionAsAlt::Both => true,
        OptionAsAlt::Left => mods.lalt,
        OptionAsAlt::Right => mods.ralt,
    }
}

/// The app command a key event's chord names in `keymap`, if any — step 3 of
/// [`decide_key_event`], shared with the overlays and the menus so a chord
/// means the same wherever it is pressed: on macOS with Option held the
/// un-composed key is matched (Option+B is "Alt+B", not "∫"; Option+L on a
/// German layout is "Alt+L", not "@"), and a Shift chord on a symbol or `0`
/// matches the key's unshifted character too (`Ctrl+Shift+/` while `?` is
/// typed — [`KeyMap::lookup_event`]).
pub fn chord_action(keymap: &KeyMap, ev: &KeyInput<'_>, opts: &KeyOptions) -> Option<KeyAction> {
    let m = ev.mods;
    let mods = Mods::new(m.ctrl, m.shift, m.alt, m.super_);
    let lookup_key = if opts.macos && m.alt { ev.key_without_modifiers } else { ev.logical };
    keymap.lookup_event(mods, ev.physical, lookup_key, ev.key_without_modifiers)
}

/// Decide what one key event means — the single entry point for the main and
/// detached windows (the keymap, the overlays' Escape, macOS Option-compose,
/// dead keys, the kitty keyboard protocol and the legacy xterm encoders).
///
/// Resolution order:
/// 1. Releases → only the kitty protocol's event-type reporting wants them.
/// 2. Escape + panel open → ClosePanel (overlay logic, not a keybinding).
/// 3. [`chord_action`] → the discrete app-command chords. A chord's
///    auto-repeat runs only the actions that step ([`KeyAction::repeats`]);
///    any other's repeats go nowhere. Unmodified Page keys bound to host
///    scrolling yield to the program on the alternate screen (it has no
///    scrollback).
/// 4. macOS Cmd swallow: an unmapped bare Cmd chord is never sent to the PTY.
/// 5. Kitty keyboard protocol, when the program enabled it.
/// 6. Legacy: macOS Option-compose text, dead-key text, then the xterm
///    encoders (control bytes, Meta-ESC, modified/DECCKM cursor keys, …).
pub fn decide_key_event(
    keymap: &KeyMap,
    ev: &KeyInput<'_>,
    modes: &KeyModes,
    opts: &KeyOptions,
    panel_open: bool,
) -> KeyAction {
    let m = ev.mods;
    if ev.kind == KeyEventKind::Release {
        // No app command fires on release and legacy encoding has no release
        // events; only the kitty protocol's event types report them.
        if !kitty_active(modes) {
            return KeyAction::None;
        }
        return send_or_none(encode_kitty_key(ev, modes, opts));
    }

    if matches!(ev.logical, Key::Named(NamedKey::Escape)) && panel_open {
        return KeyAction::ClosePanel;
    }

    if let Some(action) = chord_action(keymap, ev, opts) {
        // The press ran it; a one-shot action's repeats reach nothing — not
        // the program either, which never saw the press.
        if ev.kind == KeyEventKind::Repeat && !action.repeats() {
            return KeyAction::None;
        }
        // `[keys] scroll_page_up = ["Shift+PageUp", "PageUp"]` restores the
        // pre-v0.26 plain-PageUp scrolling; like then, the bare key still
        // reaches pagers/editors on the alternate screen.
        let page_passthrough = modes.alt_screen
            && !(m.ctrl || m.shift || m.alt || m.super_)
            && matches!(action, KeyAction::ScrollPageUp | KeyAction::ScrollPageDown);
        if !page_passthrough {
            return action;
        }
    }

    // macOS Cmd "swallow" safety net: after a keymap MISS, a bare Cmd chord
    // (`super && !ctrl && !alt`) does nothing and is never injected to the PTY.
    // On Linux no Super default is seeded, so bare Super lands here too and is a
    // clean no-op (it was rarely delivered and always swallowed).
    if m.super_ && !m.ctrl && !m.alt {
        return KeyAction::None;
    }

    if kitty_active(modes) {
        return send_or_none(encode_kitty_key(ev, modes, opts));
    }

    // macOS Option-compose: when Option is not this side's Meta key and it
    // produced a character (Option+Q → "@" on Turkish-Q, Option+8 → "{" on
    // German, Option+G → "©"), type that character — no ESC prefix.
    if opts.macos && m.alt && !m.ctrl && !alt_is_meta(m, opts) {
        if let Some(text) = composed_option_text(ev) {
            return KeyAction::Send(text.as_bytes().to_vec());
        }
    }
    // Dead-key composition (' then e → "é"): prefer the composed text.
    if !m.super_ {
        if let Some(bytes) = dead_key_text_override(m.ctrl, m.alt, ev.logical, ev.text) {
            return KeyAction::Send(bytes);
        }
    }
    send_or_none(encode_legacy(m.ctrl, m.shift, m.alt, ev.physical, ev.logical, modes))
}

/// The pre-v0.26 entry point, kept as a thin wrapper over [`decide_key_event`]
/// for call sites that only know the modifier bits: a press with no text, no
/// kitty protocol, Linux Alt semantics.
///
/// * `app_cursor` – DECCKM (`\e[?1h`): arrows and Home/End use SS3 (`\eO`).
/// * `alt_screen` – a full-screen program owns the display; an unmodified Page
///   key bound to host scrolling is forwarded to it instead.
#[allow(clippy::too_many_arguments)]
pub fn decide_key(
    keymap: &KeyMap,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_: bool,
    physical: PhysicalKey,
    logical: &Key,
    panel_open: bool,
    app_cursor: bool,
    alt_screen: bool,
) -> KeyAction {
    let mods = KeyMods { ctrl, shift, alt, super_, ..KeyMods::default() };
    let modes = KeyModes { app_cursor, alt_screen, ..KeyModes::default() };
    decide_key_event(
        keymap,
        &KeyInput::press(physical, logical, mods),
        &modes,
        &KeyOptions::default(),
        panel_open,
    )
}

// ── Per-tab key/focus bookkeeping (main AND detached windows) ────────────────

/// Keyboard state one tab carries across events: which keys it was sent a
/// press for (so a kitty-protocol app gets exactly the matching releases, never
/// a release of a key it didn't see go down) and the focus state it last
/// observed (DECSET 1004 focus reporting).
#[derive(Debug, Default)]
pub struct TabInputState {
    /// Keys whose press reached this tab's PTY and are not released yet. A
    /// handful at most; bounded so a release lost to a window teardown can
    /// never grow it.
    keys_down: Vec<HeldKey>,
    /// Focus state at the last observation (`None` before the first): a report
    /// is due only on a CHANGE, never when the app merely enables the mode.
    focus_seen: Option<bool>,
}

/// A key whose press a tab was sent, kept with what its release is encoded
/// from — for the release JeTTY sends itself when the platform never does
/// (focus left the window with the key down; see [`TabInputState::take_held`]).
#[derive(Clone, Debug, PartialEq)]
pub struct HeldKey {
    pub physical: PhysicalKey,
    /// The press's [`KeyInput`] fields of the same names.
    pub logical: Key,
    pub key_without_modifiers: Key,
    pub location: KeyLocation,
}

impl HeldKey {
    /// This key's release, sent while `mods` are held.
    pub fn release(&self, mods: KeyMods) -> KeyInput<'_> {
        KeyInput {
            physical: self.physical,
            logical: &self.logical,
            key_without_modifiers: &self.key_without_modifiers,
            text: None,
            location: self.location,
            kind: KeyEventKind::Release,
            mods,
        }
    }
}

impl TabInputState {
    /// Most keys a tab tracks as held; the oldest is forgotten past this.
    pub const MAX_KEYS_DOWN: usize = 16;

    /// A press (or an auto-repeat) of `key` was written to this tab's PTY.
    pub fn note_press(&mut self, key: HeldKey) {
        if self.holds(key.physical) {
            return;
        }
        if self.keys_down.len() == Self::MAX_KEYS_DOWN {
            self.keys_down.remove(0);
        }
        self.keys_down.push(key);
    }

    /// `physical` was released. True when this tab saw its press — the release
    /// belongs to it — and forgets the key.
    pub fn take_release(&mut self, physical: PhysicalKey) -> bool {
        match self.keys_down.iter().position(|k| k.physical == physical) {
            Some(i) => {
                self.keys_down.remove(i);
                true
            }
            None => false,
        }
    }

    /// Whether this tab saw a press of `physical` that wasn't released yet.
    pub fn holds(&self, physical: PhysicalKey) -> bool {
        self.keys_down.iter().any(|k| k.physical == physical)
    }

    /// Every key still held, oldest first, now forgotten: the window lost the
    /// keyboard with them down, and the releases are owed now. X11 sends them
    /// itself before the focus loss (and `take_release` takes those first);
    /// Wayland and macOS never do.
    pub fn take_held(&mut self) -> Vec<HeldKey> {
        std::mem::take(&mut self.keys_down)
    }

    /// Observe whether this tab is the focused one, returning the report due:
    /// `CSI I` (focus in) / `CSI O` (focus out) when the app enabled focus
    /// reporting (`enabled`, `\e[?1004h`) and the state CHANGED since the last
    /// observation. The first observation and enabling the mode never report
    /// (xterm behavior); the state is tracked even while reporting is off.
    pub fn focus_report(&mut self, focused: bool, enabled: bool) -> Option<&'static [u8]> {
        let prev = self.focus_seen.replace(focused);
        if !enabled || prev.is_none_or(|p| p == focused) {
            return None;
        }
        Some(if focused { b"\x1b[I" } else { b"\x1b[O" })
    }
}

/// The IME candidate-window anchor for the cursor at grid cell `(row, col)`:
/// `(x, y, w, h)` in physical pixels — that cell's rect, cell (0, 0) at the
/// grid `origin` (padding + bar). winit wants it in window coordinates.
pub fn ime_cursor_area(
    row: usize,
    col: usize,
    cell_w: f32,
    cell_h: f32,
    origin: jetty_render::GridOrigin,
) -> (i32, i32, u32, u32) {
    let x = origin.col_x(col, cell_w).round() as i32;
    let y = origin.row_y(row, cell_h).round() as i32;
    (x, y, cell_w.round().max(1.0) as u32, cell_h.round().max(1.0) as u32)
}

#[cfg(test)]
mod tab_input_tests {
    use super::*;

    const A: PhysicalKey = PhysicalKey::Code(KeyCode::KeyA);
    const B: PhysicalKey = PhysicalKey::Code(KeyCode::KeyB);

    /// The press of `physical`, typing `text`.
    fn key(physical: PhysicalKey, text: &str) -> HeldKey {
        let logical = Key::Character(text.into());
        HeldKey { physical, key_without_modifiers: logical.clone(), logical, location: KeyLocation::Standard }
    }

    #[test]
    fn releases_go_only_to_the_tab_that_saw_the_press() {
        let mut s = TabInputState::default();
        assert!(!s.take_release(A), "a release with no press (e.g. the summon key) is not ours");
        s.note_press(key(A, "a"));
        s.note_press(key(A, "a")); // auto-repeat / duplicate: still one entry
        assert!(s.holds(A));
        assert!(s.take_release(A));
        assert!(!s.take_release(A), "exactly one release per press");
        assert!(!s.holds(A));
    }

    #[test]
    fn held_keys_stay_bounded() {
        let mut s = TabInputState::default();
        for c in [
            KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE,
            KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ,
            KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO,
            KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
        ] {
            s.note_press(key(PhysicalKey::Code(c), "x"));
        }
        assert_eq!(s.keys_down.len(), TabInputState::MAX_KEYS_DOWN);
        assert!(!s.holds(A), "the oldest is forgotten first");
        assert!(!s.holds(B));
        assert!(s.holds(PhysicalKey::Code(KeyCode::KeyR)));
    }

    #[test]
    fn keys_still_held_when_the_focus_leaves_are_released_once() {
        let mut s = TabInputState::default();
        s.note_press(key(A, "a"));
        s.note_press(key(B, "b"));
        // X11 sends its own releases before the focus loss: those come first…
        assert!(s.take_release(B));
        // …and only what is left is still owed, oldest first, exactly once.
        let held = s.take_held();
        assert_eq!(held, vec![key(A, "a")]);
        assert!(s.take_held().is_empty());
        assert!(!s.take_release(A), "the real release that comes later sends nothing more");
        // The release sent is the one the key would have had: `CSI 97;1:3u`
        // with kitty event types on, nothing without them.
        let decide = |flags| {
            let modes = KeyModes { kitty_flags: flags, ..KeyModes::default() };
            let ev = held[0].release(KeyMods::default());
            decide_key_event(&KeyMap::defaults(), &ev, &modes, &KeyOptions::default(), false)
        };
        assert_eq!(decide(KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES), KeyAction::Send(b"\x1b[97;1:3u".to_vec()));
        assert_eq!(decide(KITTY_DISAMBIGUATE), KeyAction::None);
        assert_eq!(decide(0), KeyAction::None);
    }

    #[test]
    fn focus_reports_fire_on_changes_only_and_only_when_enabled() {
        let mut s = TabInputState::default();
        // First observation never reports, even with the mode on.
        assert_eq!(s.focus_report(true, true), None);
        assert_eq!(s.focus_report(true, true), None, "no change, no report");
        assert_eq!(s.focus_report(false, true), Some(&b"\x1b[O"[..]));
        assert_eq!(s.focus_report(false, true), None, "a hide after the focus-out sends nothing more");
        assert_eq!(s.focus_report(true, true), Some(&b"\x1b[I"[..]));
        // Mode off: the change is tracked but not reported…
        assert_eq!(s.focus_report(false, false), None);
        // …and enabling the mode later doesn't fire a stale report.
        assert_eq!(s.focus_report(false, true), None);
        assert_eq!(s.focus_report(true, true), Some(&b"\x1b[I"[..]));
    }

    #[test]
    fn grab_churn_around_a_hotkey_reports_out_then_in_once_each() {
        // X11: the summon hotkey's key grab sends FocusOut then FocusIn on a
        // focused window; each transition reports exactly once.
        let mut s = TabInputState::default();
        assert_eq!(s.focus_report(true, true), None);
        assert_eq!(s.focus_report(false, true), Some(&b"\x1b[O"[..]));
        assert_eq!(s.focus_report(true, true), Some(&b"\x1b[I"[..]));
    }

    #[test]
    fn ime_area_is_the_cursor_cell_below_the_grid_top() {
        use jetty_render::GridOrigin;
        assert_eq!(ime_cursor_area(0, 0, 9.6, 20.0, GridOrigin::new(0.0, 36.0)), (0, 36, 10, 20));
        assert_eq!(ime_cursor_area(3, 10, 9.6, 20.0, GridOrigin::new(0.0, 36.0)), (96, 96, 10, 20));
        // The padded origin moves the anchor with the grid (2×: 16 px / 8 px).
        assert_eq!(ime_cursor_area(3, 10, 19.2, 40.0, GridOrigin::new(16.0, 80.0)), (208, 200, 19, 40));
        // Degenerate metrics (before the first layout) still give a 1px area.
        assert_eq!(ime_cursor_area(0, 0, 0.0, 0.0, GridOrigin::default()), (0, 0, 1, 1));
    }
}

fn send_or_none(bytes: Option<Vec<u8>>) -> KeyAction {
    bytes.map_or(KeyAction::None, KeyAction::Send)
}

/// The kitty protocol changes key encoding only when one of these flags is set:
/// "report alternate keys" just decorates codes the others produce, and
/// "associated text" is undefined without "report all keys".
fn kitty_active(modes: &KeyModes) -> bool {
    modes.kitty_flags & (KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES | KITTY_REPORT_ALL_KEYS) != 0
}

/// macOS Option-compose: the text Option produced, when it differs from the bare
/// key. `None` for control text, or when Option changed nothing (then Alt falls
/// back to Meta, ESC-prefixing the key).
fn composed_option_text<'a>(ev: &KeyInput<'a>) -> Option<&'a str> {
    let text = ev.text?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    if let Key::Character(base) = ev.key_without_modifiers {
        if base.as_str() == text {
            return None;
        }
    }
    Some(text)
}

/// Legacy xterm encoding (`TERM=xterm-256color`, byte-checked against
/// `infocmp -x xterm-256color`): Page keys, Ctrl+<letter/symbol> control bytes
/// (ESC-prefixed with Alt), Ctrl+Backspace, modified arrows / Home / End /
/// Delete / Insert / F-keys, DECCKM cursor keys, and the Meta-ESC fallback.
/// `None` = the key produces nothing.
fn encode_legacy(
    ctrl: bool,
    shift: bool,
    alt: bool,
    physical: PhysicalKey,
    logical: &Key,
    modes: &KeyModes,
) -> Option<Vec<u8>> {
    // An F13–F18 key the layout left unnamed is that F-key (`unnamed_fkey`).
    let named;
    let logical = match unnamed_fkey(logical, physical) {
        Some(f) => {
            named = Key::Named(f);
            &named
        }
        None => logical,
    };

    // PageUp / PageDown always reach the program: `\e[5~`/`\e[6~`, with the
    // xterm modifier parameter when modified (`\e[5;m~`; vim :tabnext on
    // Ctrl+PageDown, tmux `bind -n C-PgUp`). Host scrolling is the keymap's
    // Shift+PageUp/Down (`scroll_page_up`/`scroll_page_down`), resolved before
    // this, like every terminal's standard escape hatch.
    if let Key::Named(page @ (NamedKey::PageUp | NamedKey::PageDown)) = logical {
        let n = if *page == NamedKey::PageUp { 5 } else { 6 };
        let m = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
        if m == 1 {
            return Some(format!("\x1b[{n}~").into_bytes());
        }
        return Some(format!("\x1b[{n};{m}~").into_bytes());
    }

    // (Font size Ctrl+'+'/'='/'-'/'0' and opacity Ctrl+Alt+'±' are keymap
    // chords, resolved before this by the logical character first — so they
    // follow the key LABEL on Turkish-Q / QWERTZ / AZERTY.)

    // Ctrl+<letter/symbol> → control byte (Ctrl+C = 0x03 SIGINT, Ctrl+D = EOF,
    // Ctrl+Z, Ctrl+L clear, ...). Must come before the plain key_to_bytes
    // fallback, which would otherwise send the literal character.
    //
    // Keyed on the LOGICAL character, not the physical position: physical codes
    // are hardware QWERTY positions, so keying letters on them sends the wrong
    // control code on Dvorak/AZERTY/QWERTZ — Ctrl at the key LABELED C must be
    // 0x03, not TAB. A typed ASCII character decides ALONE ([`ctrl_char_byte`]:
    // its C0 code, or nothing — then the character itself goes out below, as in
    // xterm). Only a key that typed no ASCII character (Cyrillic/Greek/CJK, the
    // Turkish ğ/ü/ı, a dead or Unidentified key, Space) falls back to its US
    // position (ctrl_byte). Falling back for ASCII punctuation too sent the US
    // key's byte at that spot: Ctrl+; on Turkish-Q and Ctrl+# on German were FS
    // (0x1c, SIGQUIT), Ctrl+; on Dvorak SUB (0x1a, SIGTSTP).
    //
    // Applies REGARDLESS of shift: Ctrl+Shift+C == Ctrl+C for control purposes
    // (both → 0x03). The explicit Ctrl+Shift app shortcuts (C/V/T/W/…) are
    // resolved by the keymap before this, so they keep their special meaning.
    //
    // When Alt/Meta is also held, the control byte is ESC-prefixed (the classic
    // "Meta sends Escape" convention), e.g. Ctrl+Alt+b → ESC + 0x02.
    if ctrl {
        let byte = match logical {
            Key::Character(s) if s.is_ascii() => single_char(s).and_then(ctrl_char_byte),
            _ => match physical {
                PhysicalKey::Code(code) => ctrl_byte(code),
                PhysicalKey::Unidentified(_) => None,
            },
        };
        if let Some(b) = byte {
            if alt {
                return Some(vec![0x1b, b]);
            }
            return Some(vec![b]);
        }
    }

    // Ctrl+Backspace → 0x08 (BS), distinct from plain / Alt Backspace (0x7f) so
    // editors can bind delete-word-backward. ESC-prefixed when Alt is also held.
    if ctrl && matches!(logical, Key::Named(NamedKey::Backspace)) {
        if alt {
            return Some(vec![0x1b, 0x08]);
        }
        return Some(vec![0x08]);
    }

    // Modified arrows + back-tab. When any of Ctrl/Shift/Alt is held with an arrow,
    // emit the xterm CSI form `\e[1;<mod><final>` (mod = 1 + shift + alt<<1 +
    // ctrl<<2) so apps see word-jump (Ctrl+Left/Right), selection (Shift+Arrow),
    // etc. instead of a bare arrow. This is used regardless of DECCKM; plain
    // arrows fall through to the DECCKM-aware cursor_key_bytes below. Plain
    // Shift+Tab (Ctrl+Shift+Tab is already intercepted as PrevTab above) sends
    // CSI Z (back-tab) for reverse menu-complete.
    if ctrl || shift || alt {
        let arrow_final = match logical {
            Key::Named(NamedKey::ArrowUp) => Some(b'A'),
            Key::Named(NamedKey::ArrowDown) => Some(b'B'),
            Key::Named(NamedKey::ArrowRight) => Some(b'C'),
            Key::Named(NamedKey::ArrowLeft) => Some(b'D'),
            _ => None,
        };
        if let Some(fin) = arrow_final {
            let m = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
            return Some(format!("\x1b[1;{}{}", m, fin as char).into_bytes());
        }
        // Modified Home/End/Delete/Insert: the xterm modified forms. Home/End
        // use the CSI-1 letter form (`\e[1;<mod>H` / `\e[1;<mod>F`); Insert and
        // Delete keep their tilde form with the modifier param (`\e[2;<mod>~` /
        // `\e[3;<mod>~`). Without these, Shift+Home (select-to-line-start) and
        // Ctrl+Delete (delete-word-forward) collapsed to the plain sequences and
        // apps could not see the modifiers at all.
        // Shift+Insert (no Ctrl/Alt) is the universal terminal paste chord — the
        // keymap resolves it to Paste above (default binding), so it reaches this
        // modified-nav block only when the user explicitly UNBOUND paste, in which
        // case the raw `\e[2;{m}~` form below is the correct passthrough.
        let m = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
        match logical {
            Key::Named(NamedKey::Home) => {
                return Some(format!("\x1b[1;{m}H").into_bytes());
            }
            Key::Named(NamedKey::End) => {
                return Some(format!("\x1b[1;{m}F").into_bytes());
            }
            Key::Named(NamedKey::Insert) => {
                return Some(format!("\x1b[2;{m}~").into_bytes());
            }
            Key::Named(NamedKey::Delete) => {
                return Some(format!("\x1b[3;{m}~").into_bytes());
            }
            _ => {}
        }
        if kp_begin(physical, logical) {
            return Some(format!("\x1b[1;{m}E").into_bytes());
        }
        // Modified function keys: F1–F4 use the CSI-1 letter form (`\e[1;{m}P..S`),
        // F5–F24 and Menu the CSI tilde form (`\e[{n};{m}~`). Without these any
        // modified F-key collapses to the unmodified sequence (Shift+F5 → plain
        // `\e[15~`), and Alt+F-key would get a double-ESC from the Meta fallback
        // below.
        let fkey_final = match logical {
            Key::Named(NamedKey::F1) => Some('P'),
            Key::Named(NamedKey::F2) => Some('Q'),
            Key::Named(NamedKey::F3) => Some('R'),
            Key::Named(NamedKey::F4) => Some('S'),
            _ => None,
        };
        if let Some(fin) = fkey_final {
            return Some(format!("\x1b[1;{m}{fin}").into_bytes());
        }
        if let Some(n) = tilde_key_number(logical) {
            return Some(format!("\x1b[{n};{m}~").into_bytes());
        }
        if shift && !ctrl && !alt && matches!(logical, Key::Named(NamedKey::Tab)) {
            return Some(b"\x1b[Z".to_vec());
        }
    }

    // Unmodified cursor keys honor DECCKM (application cursor mode): with `\e[?1h`
    // the arrows AND Home/End use SS3 (`\eOA`, `\eOH`, `\eOF`) — exactly
    // terminfo's kcuu1/khome/kend, which shells read after `smkx` (oh-my-zsh
    // binds Home/End to `$terminfo[khome]`/`[kend]` ONLY). Normal mode keeps CSI.
    // NumLock-off keypad 5 (`KP_Begin`) is one of xterm's cursor keys too:
    // `\e[E` / `\eOE` (terminfo's kb2).
    if kp_begin(physical, logical) {
        return Some(if modes.app_cursor { b"\x1bOE".to_vec() } else { b"\x1b[E".to_vec() });
    }
    if let Some(bytes) = cursor_key_bytes(logical, modes.app_cursor) {
        if alt {
            let mut out = Vec::with_capacity(bytes.len() + 1);
            out.push(0x1b);
            out.extend_from_slice(&bytes);
            return Some(out);
        }
        return Some(bytes);
    }

    // Fallback: convert the key to its byte sequence. When Alt/Meta is
    // held and the key produces bytes, send them ESC-prefixed (the classic
    // "Meta sends Escape" convention), e.g. Alt+b → ESC b, Alt+ş → ESC ş,
    // Alt+Enter → ESC CR.
    let bytes = key_to_bytes(logical)?;
    if alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.extend_from_slice(&bytes);
        return Some(out);
    }
    Some(bytes)
}

/// High-level action decoded from a left mouse button press.
#[derive(Debug, PartialEq)]
pub enum MouseAction {
    /// A part of a data-driven Settings control (see `settings_ui`): the
    /// control's id (its config key path) and which part was hit.
    Ctl { id: jetty_render::CtlId, part: jetty_render::CtlPart },
    /// A Settings section header — collapse / expand it.
    SettingsSection(&'static str),
    /// A theme card in the gallery (theme registry index).
    GalleryCard(usize),
    /// A gallery filter chip.
    GalleryFilter(jetty_render::ThemeFilter),
    /// The Settings scrollbar thumb. `grab_dy` is `cy - thumb.y`.
    PanelScrollThumb { grab_dy: f32 },
    /// The Settings scrollbar track outside the thumb — page toward the click.
    PanelScrollTrack,
    /// The footer's "Reset tab" button.
    ResetTab,
    /// User clicked one of the settings tab labels — switch the active tab.
    SetSettingsTab(usize),
    /// User pressed on the title bar (not on any widget) — start dialog drag.
    StartDialogDrag,
    /// User clicked inside the panel but not on any widget — swallow the event.
    ConsumePanel,
    /// User pressed inside the scrollbar thumb. `grab_dy` is `cy - rect.y`.
    StartScrollbarDrag { grab_dy: f32 },
    /// User pressed on the scrollbar track outside the thumb — jump to position.
    ScrollbarTrackJump,
    /// Click is not handled by any panel or scrollbar widget.
    None,
}

/// Decide what a left mouse button press means given current geometry.
///
/// * `panel`     – `Some(&PanelGeom)` when the Settings panel is open.
/// * `scrollbar` – The current scrollbar thumb [`Rect`], if any.
/// * `cx`, `cy`  – Cursor position in physical pixels.
///
/// Priority: the panel's parts (`PanelGeom::hit_at`: tabs, then chrome — the
/// footer button and the scrollbar — then the content, live only inside its
/// viewport), then its title bar, then anywhere else on the panel (consumed);
/// outside the panel, the terminal scrollbar thumb / track.
pub fn decide_mouse_press(
    panel: Option<&jetty_render::PanelGeom>,
    scrollbar: Option<&jetty_render::Rect>,
    cx: f32,
    cy: f32,
) -> MouseAction {
    use jetty_render::PanelHit;
    if let Some(g) = panel {
        if let Some(hit) = g.hit_at(cx, cy) {
            return match hit {
                PanelHit::Tab(i) => MouseAction::SetSettingsTab(i),
                PanelHit::Ctl { id, part } => MouseAction::Ctl { id, part },
                PanelHit::Section(id) => MouseAction::SettingsSection(id),
                PanelHit::GalleryCard(i) => MouseAction::GalleryCard(i),
                PanelHit::GalleryFilter(f) => MouseAction::GalleryFilter(f),
                PanelHit::ScrollThumb => MouseAction::PanelScrollThumb {
                    grab_dy: cy - g.scroll_thumb.map_or(cy, |t| t.y),
                },
                PanelHit::ScrollTrack => MouseAction::PanelScrollTrack,
                PanelHit::ResetTab => MouseAction::ResetTab,
            };
        }
        // Title bar — drag handle; must come before generic consume.
        if point_in(&g.title_bar, cx, cy) {
            return MouseAction::StartDialogDrag;
        }
        // Inside panel but not a widget → consume.
        if point_in(&g.panel, cx, cy) {
            return MouseAction::ConsumePanel;
        }
        // Click outside the panel while it is open: fall through to scrollbar.
    }

    if let Some(rect) = scrollbar {
        let in_thumb = cx >= rect.x && cx <= rect.x + rect.w
            && cy >= rect.y && cy <= rect.y + rect.h;
        let in_track = cx >= rect.x && cx <= rect.x + rect.w;

        if in_thumb {
            return MouseAction::StartScrollbarDrag { grab_dy: cy - rect.y };
        }
        if in_track {
            return MouseAction::ScrollbarTrackJump;
        }
    }

    MouseAction::None
}

/// Returns `true` when the point `(x, y)` lies within the rect (inclusive).
pub fn point_in(r: &jetty_render::Rect, x: f32, y: f32) -> bool {
    x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
}

/// The control byte Ctrl turns a typed ASCII character into: letters → 1..=26
/// (Ctrl+C = SIGINT, Ctrl+D = EOF, …), and the C0 symbols by xterm's Control
/// rule (Xlib / libxkbcommon: `@` `` ` `` NUL; `[` `{` ESC; `\` `|` FS; `]` `}`
/// GS; `^` `~` RS; `_` `/` US — readline / emacs undo) with its digits (`2`
/// NUL, `3`…`7` ESC…US, `8` DEL — the VT220's), plus `?` → DEL as kitty sends
/// it. `None`: Ctrl doesn't change this character (`;` `#` `'` `.` `1` `9` …)
/// — it is typed as is.
fn ctrl_char_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' | 'A'..='Z' => c.to_ascii_uppercase() as u8 - b'@',
        '@' | '`' | '2' => 0x00,
        '[' | '{' => 0x1b,
        '\\' | '|' => 0x1c,
        ']' | '}' => 0x1d,
        '^' | '~' => 0x1e,
        '_' | '/' => 0x1f,
        '3'..='7' => c as u8 - b'3' + 0x1b,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Map a physical key to its Ctrl control byte: Ctrl+A=1 .. Ctrl+Z=26 (so
/// Ctrl+C=3=SIGINT, Ctrl+D=4=EOF, Ctrl+Z=26, Ctrl+L=12=clear), plus the remaining
/// C0 symbol combos: Ctrl+Space=0x00 (NUL), Ctrl+[=0x1b (ESC), Ctrl+\=0x1c (FS),
/// Ctrl+]=0x1d (GS). Uses the US key at that position — only for keys that typed
/// no ASCII character (see `encode_legacy`).
fn ctrl_byte(code: KeyCode) -> Option<u8> {
    use KeyCode::*;
    let n: u8 = match code {
        KeyA => 1, KeyB => 2, KeyC => 3, KeyD => 4, KeyE => 5, KeyF => 6,
        KeyG => 7, KeyH => 8, KeyI => 9, KeyJ => 10, KeyK => 11, KeyL => 12,
        KeyM => 13, KeyN => 14, KeyO => 15, KeyP => 16, KeyQ => 17, KeyR => 18,
        KeyS => 19, KeyT => 20, KeyU => 21, KeyV => 22, KeyW => 23, KeyX => 24,
        KeyY => 25, KeyZ => 26,
        Space => 0x00,        // Ctrl+Space → NUL
        BracketLeft => 0x1b,  // Ctrl+[ → ESC
        Backslash => 0x1c,    // Ctrl+\ → FS
        BracketRight => 0x1d, // Ctrl+] → GS
        _ => return None,
    };
    Some(n)
}

/// Encode the unmodified cursor keys — the four arrows plus Home/End — honoring
/// DECCKM (application cursor mode).
///
/// Returns `None` for any other key. When `app_cursor` is true the SS3 prefix
/// (`\eO`) is used; otherwise the default CSI prefix (`\e[`). The SS3 column is
/// terminfo's kcuu1/kcud1/kcuf1/kcub1/khome/kend for `xterm-256color`:
///
/// | key        | normal (CSI) | app_cursor (SS3) |
/// |------------|--------------|------------------|
/// | ArrowUp    | `\e[A`       | `\eOA`           |
/// | ArrowDown  | `\e[B`       | `\eOB`           |
/// | ArrowRight | `\e[C`       | `\eOC`           |
/// | ArrowLeft  | `\e[D`       | `\eOD`           |
/// | Home       | `\e[H`       | `\eOH`           |
/// | End        | `\e[F`       | `\eOF`           |
pub fn cursor_key_bytes(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let final_byte = match key {
        Key::Named(NamedKey::ArrowUp) => b'A',
        Key::Named(NamedKey::ArrowDown) => b'B',
        Key::Named(NamedKey::ArrowRight) => b'C',
        Key::Named(NamedKey::ArrowLeft) => b'D',
        Key::Named(NamedKey::Home) => b'H',
        Key::Named(NamedKey::End) => b'F',
        _ => return None,
    };
    // CSI (`\e[`) by default; SS3 (`\eO`) under DECCKM.
    let prefix = if app_cursor { b'O' } else { b'[' };
    Some(vec![0x1b, prefix, final_byte])
}

/// Byte sequence for a single wheel-driven scroll step on the alternate screen
/// (ALTERNATE_SCROLL): an Up (`up == true`) or Down arrow, DECCKM-aware. A
/// compliant host translates wheel ticks to arrow keys when a pager/editor owns
/// the alt screen with mouse reporting off, so `less`/`man`/`git log` scroll
/// (F3). This is the arrow-key analogue of [`cursor_key_bytes`].
pub fn arrow_scroll_bytes(up: bool, app_cursor: bool) -> Vec<u8> {
    let final_byte = if up { b'A' } else { b'B' };
    let prefix = if app_cursor { b'O' } else { b'[' };
    vec![0x1b, prefix, final_byte]
}

/// Translate a winit logical key into the byte sequence a terminal expects.
/// This is the single source of truth — both `app.rs` and tests use it.
///
/// Arrow and Home/End keys here always use the default CSI (`\e[`) encoding.
/// Callers that need DECCKM-aware cursor keys should use [`cursor_key_bytes`]
/// (or [`decide_key_event`], which routes them through it).
pub fn key_to_bytes(key: &Key) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        // Navigation + editing keys (xterm encodings). Without these the keys are
        // silently dropped, breaking readline line-editing, vim, htop, less, fzf
        // and every ncurses TUI. Home/End use the normal-mode CSI (`\e[H`/`\e[F`)
        // form; the DECCKM SS3 form comes from `cursor_key_bytes`.
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
        // Function row. F1–F4 use the SS3 (`\eOP`..`\eOS`) form; F5–F24 (and
        // Menu) the CSI tilde form (`tilde_key_number`). (F9 is normally
        // consumed by the global summon hotkey before it reaches here; this is
        // the fallback when no global grab is active.)
        Key::Named(NamedKey::F1) => Some(b"\x1bOP".to_vec()),
        Key::Named(NamedKey::F2) => Some(b"\x1bOQ".to_vec()),
        Key::Named(NamedKey::F3) => Some(b"\x1bOR".to_vec()),
        Key::Named(NamedKey::F4) => Some(b"\x1bOS".to_vec()),
        Key::Character(s) => Some(s.as_bytes().to_vec()),
        _ => tilde_key_number(key).map(|n| format!("\x1b[{n}~").into_bytes()),
    }
}

/// The xterm `CSI n ~` number of F5–F24 and the Menu key (xterm's
/// `decfuncvalue`): F13–F20 are the VT220's 25, 26, 28, 29 and 31–34 — 28 is
/// its Help key and 29 its Do key, which xterm sends for Menu — and from F21
/// they run on from 42.
fn tilde_key_number(key: &Key) -> Option<u8> {
    use NamedKey as N;
    let Key::Named(named) = key else { return None };
    Some(match named {
        N::F5 => 15,
        N::F6 => 17,
        N::F7 => 18,
        N::F8 => 19,
        N::F9 => 20,
        N::F10 => 21,
        N::F11 => 23,
        N::F12 => 24,
        N::F13 => 25,
        N::F14 => 26,
        N::F15 => 28,
        N::F16 | N::ContextMenu => 29,
        N::F17 => 31,
        N::F18 => 32,
        N::F19 => 33,
        N::F20 => 34,
        N::F21 => 42,
        N::F22 => 43,
        N::F23 => 44,
        N::F24 => 45,
        _ => return None,
    })
}

/// F13–F18 for a key the layout left unnamed at those positions: xkb's evdev
/// rules call them `XF86Tools` and `XF86Launch5`…`9`, which winit doesn't
/// name, so Apple's F13–F18 and a keyd / QMK remap to them sent nothing. F19
/// and F24 arrive named; F20–F23 are laptop keys by udev convention (mic
/// mute, touchpad toggle / on / off) and stay silent.
fn unnamed_fkey(logical: &Key, physical: PhysicalKey) -> Option<NamedKey> {
    if !matches!(logical, Key::Unidentified(_)) {
        return None;
    }
    Some(match physical {
        PhysicalKey::Code(KeyCode::F13) => NamedKey::F13,
        PhysicalKey::Code(KeyCode::F14) => NamedKey::F14,
        PhysicalKey::Code(KeyCode::F15) => NamedKey::F15,
        PhysicalKey::Code(KeyCode::F16) => NamedKey::F16,
        PhysicalKey::Code(KeyCode::F17) => NamedKey::F17,
        PhysicalKey::Code(KeyCode::F18) => NamedKey::F18,
        _ => return None,
    })
}

/// NumLock-off keypad 5: xkb's `KP_Begin`, which winit leaves unnamed (with
/// NumLock on, the key types its digit).
fn kp_begin(physical: PhysicalKey, logical: &Key) -> bool {
    physical == PhysicalKey::Code(KeyCode::Numpad5) && matches!(logical, Key::Unidentified(_))
}

/// Whether `logical` is a modifier key on its own (Shift, Ctrl, Alt, AltGr,
/// Super, the lock keys, …): sent to the PTY only under the kitty "report all
/// keys" flag, and never a reason to close an open menu.
pub fn is_modifier_key(logical: &Key) -> bool {
    use NamedKey as N;
    matches!(
        logical,
        Key::Named(
            N::Shift
                | N::Control
                | N::Alt
                | N::AltGraph
                | N::Super
                | N::Meta
                | N::Hyper
                | N::CapsLock
                | N::NumLock
                | N::ScrollLock
                | N::Fn
                | N::FnLock
                | N::Symbol
                | N::SymbolLock
        )
    )
}

/// Accumulates fractional scroll deltas (in LINES) across wheel events.
///
/// Slow two-finger touchpad scrolling arrives as many sub-line deltas
/// (PixelDelta of a few px, or fractional LineDelta); rounding each event
/// independently discards them all and gentle scrolling moves nothing. This
/// accumulator carries the fraction across events: `add` returns the whole
/// lines to scroll NOW (truncated toward zero) and keeps the remainder.
#[derive(Debug, Default)]
pub struct ScrollAccumulator {
    acc: f32,
}

impl ScrollAccumulator {
    pub fn new() -> Self {
        Self { acc: 0.0 }
    }

    /// Feed a (possibly fractional) line delta; returns the whole lines to
    /// emit now. The fractional remainder is retained for the next event, so a
    /// stream of +0.3 deltas emits a line every ~4 events instead of never.
    /// Non-finite deltas are ignored defensively.
    pub fn add(&mut self, delta_lines: f32) -> i32 {
        if !delta_lines.is_finite() {
            return 0;
        }
        self.acc += delta_lines;
        let whole = self.acc.trunc();
        self.acc -= whole;
        whole as i32
    }

    /// Drop any accumulated remainder (called on tab switch so one tab's
    /// leftover fraction never bleeds into another tab's scroll).
    pub fn reset(&mut self) {
        self.acc = 0.0;
    }
}

/// Convert a pixel position to 1-based terminal cell coordinates, CLAMPED to
/// the grid (`1..=cols`, `1..=rows`). `y` is grid-relative (the caller
/// subtracts the tab-bar origin). Clicks in the scrollbar gutter right of the
/// last column or in the status strip below the grid previously produced
/// out-of-range coordinates (cols+1, rows+1) that xterm would clamp — mouse
/// reports must never carry coordinates outside the grid.
pub fn cell_at_clamped(
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let col = (x / cell_w).floor() as i64 + 1;
    let row = (y / cell_h).floor() as i64 + 1;
    (
        col.clamp(1, cols.max(1) as i64) as usize,
        row.clamp(1, rows.max(1) as i64) as usize,
    )
}

/// Convert a pixel position to 0-based viewport cell coordinates
/// `(line, col, left_half)` CLAMPED to the grid. `x` and `y_grid` are
/// grid-relative (the caller subtracts the grid origin — padding and bar — and
/// floors y at 0). `left_half` is whether the pointer sits in the LEFT half of
/// its cell — selection start/update derive the endpoint `Side` from it (F4):
/// hardcoding Left-at-press / Right-at-update dropped the endpoint cells on a
/// reverse drag. A pointer left of the grid (the left padding) is column 0's
/// left half and one past its right edge (the right padding / scrollbar
/// gutter) the last column's right half — so a drag into either padding
/// selects through the edge cell instead of depending on the sub-pixel spot.
/// Callers guarantee `cell_w`/`cell_h` > 0.
pub fn cell_at_0_side(
    x: f32,
    y_grid: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize, bool) {
    let line = ((y_grid / cell_h).floor() as i64).clamp(0, rows.saturating_sub(1) as i64) as usize;
    let last = cols.saturating_sub(1);
    if x < 0.0 {
        return (line, 0, true);
    }
    if x >= cols as f32 * cell_w {
        return (line, last, false);
    }
    let col_f = (x / cell_w).floor();
    let col = (col_f as i64).clamp(0, last as i64) as usize;
    // Sub-cell x fraction: the pointer is in the left half when it sits in
    // the first half-cell-width past the cell's left edge.
    let left_half = (x - col_f * cell_w) < cell_w * 0.5;
    (line, col, left_half)
}

/// Dead-key / compose fallback for the plain printable-send path.
///
/// When a dead-key sequence composes (e.g. `'` then `e` on US-International),
/// the composed glyph ("é") lives only in the event's `text`, while
/// `logical_key` still reports the base character ("e"). Returns
/// `Some(text bytes)` when the composed `text` should be sent INSTEAD of the
/// logical character:
/// * no Ctrl/Alt held (those paths — control bytes, Meta-ESC, macOS
///   Option-compose — must keep their existing behavior),
/// * the logical key is a plain `Character` — or Space, which winit keeps
///   reporting as the named key when a dead key or Compose sequence ends in it
///   (`'` then Space → "'" on US-International, `^` Space → "^" on German,
///   Compose Space Space → NBSP; xkb compose with no input method, i.e.
///   Wayland). Other named keys like Enter/Tab produce control text that must
///   keep going through `key_to_bytes`,
/// * `text` is non-empty, differs from the logical character, and contains no
///   control characters.
pub fn dead_key_text_override(
    ctrl: bool,
    alt: bool,
    logical: &Key,
    text: Option<&str>,
) -> Option<Vec<u8>> {
    if ctrl || alt {
        return None;
    }
    let base = match logical {
        Key::Character(s) => s.as_str(),
        Key::Named(NamedKey::Space) => " ",
        _ => return None,
    };
    let t = text?;
    if t.is_empty() || t == base || t.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(t.as_bytes().to_vec())
}

// ── Kitty keyboard protocol ──────────────────────────────────────────────────
//
// A port of kitty's reference encoder (`kitty/key_encoding.c`: `encode_key`,
// `encode_function_key`, `serialize`, `encode_printable_ascii_key_legacy`),
// driven by winit's key event fields. Differences forced by winit: there is no
// Hyper/Meta modifier and no Caps/Num Lock state, so those bits are never set.
// Deliberate deviation: with ONLY "report event types" (0b10) on, releasing Esc
// sends `CSI 27;1:3u` instead of kitty's second raw ESC byte.

/// Kitty modifier bits (the encoded field is `1 + bits`).
const KM_SHIFT: u8 = 1;
const KM_ALT: u8 = 2;
const KM_CTRL: u8 = 4;
const KM_SUPER: u8 = 8;

impl KeyEventKind {
    /// The protocol's event-type number (press 1, repeat 2, release 3).
    fn kitty_number(self) -> u8 {
        match self {
            KeyEventKind::Press => 1,
            KeyEventKind::Repeat => 2,
            KeyEventKind::Release => 3,
        }
    }
}

/// A key with a kitty "functional key" code (everything that isn't plain
/// text), from the spec's functional-key table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FKey {
    Escape,
    Enter,
    Tab,
    Backspace,
    Insert,
    Delete,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// F1..=F35.
    F(u8),
    /// KP_0..=KP_9 (NumLock on).
    KpDigit(u8),
    KpDecimal,
    KpDivide,
    KpMultiply,
    KpSubtract,
    KpAdd,
    KpEnter,
    KpEqual,
    KpSeparator,
    KpLeft,
    KpRight,
    KpUp,
    KpDown,
    KpPageUp,
    KpPageDown,
    KpHome,
    KpEnd,
    KpInsert,
    KpDelete,
    KpBegin,
    /// Lock keys, Print, Pause, Menu, media and volume keys: their PUA code.
    Pua(u32),
    /// A modifier key itself: its PUA code and the modifier bit it drives (0 for
    /// keys winit can't express as a modifier: Hyper, Meta, AltGr).
    Modifier(u32, u8),
}

// PUA codes from the spec's functional-key table.
const KP_CAPS_LOCK: u32 = 57358;
const KP_SCROLL_LOCK: u32 = 57359;
const KP_NUM_LOCK: u32 = 57360;

impl FKey {
    /// `(number, CSI final byte)` per the spec's table (`CSI number ; mods final`).
    fn number_and_final(self) -> (u32, u8) {
        match self {
            FKey::Escape => (27, b'u'),
            FKey::Enter => (13, b'u'),
            FKey::Tab => (9, b'u'),
            FKey::Backspace => (127, b'u'),
            FKey::Insert => (2, b'~'),
            FKey::Delete => (3, b'~'),
            FKey::Left => (1, b'D'),
            FKey::Right => (1, b'C'),
            FKey::Up => (1, b'A'),
            FKey::Down => (1, b'B'),
            FKey::PageUp => (5, b'~'),
            FKey::PageDown => (6, b'~'),
            FKey::Home => (1, b'H'),
            FKey::End => (1, b'F'),
            FKey::F(1) => (1, b'P'),
            FKey::F(2) => (1, b'Q'),
            // `CSI R` would collide with the cursor position report.
            FKey::F(3) => (13, b'~'),
            FKey::F(4) => (1, b'S'),
            FKey::F(n @ 5..=12) => {
                ([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)], b'~')
            }
            FKey::F(n) => (57376 + u32::from(n.saturating_sub(13)), b'u'),
            FKey::KpDigit(d) => (57399 + u32::from(d), b'u'),
            FKey::KpDecimal => (57409, b'u'),
            FKey::KpDivide => (57410, b'u'),
            FKey::KpMultiply => (57411, b'u'),
            FKey::KpSubtract => (57412, b'u'),
            FKey::KpAdd => (57413, b'u'),
            FKey::KpEnter => (57414, b'u'),
            FKey::KpEqual => (57415, b'u'),
            FKey::KpSeparator => (57416, b'u'),
            FKey::KpLeft => (57417, b'u'),
            FKey::KpRight => (57418, b'u'),
            FKey::KpUp => (57419, b'u'),
            FKey::KpDown => (57420, b'u'),
            FKey::KpPageUp => (57421, b'u'),
            FKey::KpPageDown => (57422, b'u'),
            FKey::KpHome => (57423, b'u'),
            FKey::KpEnd => (57424, b'u'),
            FKey::KpInsert => (57425, b'u'),
            FKey::KpDelete => (57426, b'u'),
            FKey::KpBegin => (1, b'E'),
            FKey::Pua(code) | FKey::Modifier(code, _) => (code, b'u'),
        }
    }

    /// Modifier and lock keys: reported only with "report all keys".
    fn is_modifier(self) -> bool {
        matches!(
            self,
            FKey::Modifier(..) | FKey::Pua(KP_CAPS_LOCK | KP_SCROLL_LOCK | KP_NUM_LOCK)
        )
    }

    /// Keys whose legacy form is text (`\r`, `\t`, DEL, ESC): on macOS a
    /// composing Option is not reported as `alt` for them (alacritty's rule).
    fn is_textual(self) -> bool {
        matches!(self, FKey::Enter | FKey::Tab | FKey::Backspace | FKey::Escape | FKey::KpEnter)
    }

    /// Without disambiguation, keypad keys are reported as their main-block
    /// twins: navigation keys stay functional, digits/operators become text.
    fn without_keypad(self) -> Result<FKey, char> {
        Ok(match self {
            FKey::KpDigit(d) => return Err(char::from(b'0' + d)),
            FKey::KpDecimal => return Err('.'),
            FKey::KpDivide => return Err('/'),
            FKey::KpMultiply => return Err('*'),
            FKey::KpSubtract => return Err('-'),
            FKey::KpAdd => return Err('+'),
            FKey::KpEqual => return Err('='),
            FKey::KpEnter => FKey::Enter,
            FKey::KpLeft => FKey::Left,
            FKey::KpRight => FKey::Right,
            FKey::KpUp => FKey::Up,
            FKey::KpDown => FKey::Down,
            FKey::KpPageUp => FKey::PageUp,
            FKey::KpPageDown => FKey::PageDown,
            FKey::KpHome => FKey::Home,
            FKey::KpEnd => FKey::End,
            FKey::KpInsert => FKey::Insert,
            FKey::KpDelete => FKey::Delete,
            other => other,
        })
    }
}

/// The functional key (if any) for this event. Keypad keys are identified by
/// their PHYSICAL position (winit's `key_without_modifiers` reports a NumLock-on
/// keypad "1" as `End`); the logical key says whether NumLock made it a digit.
fn functional_key(ev: &KeyInput<'_>) -> Option<FKey> {
    if let PhysicalKey::Code(code) = ev.physical {
        if let Some(k) = keypad_key(code, ev.logical) {
            return Some(k);
        }
    }
    let named = match ev.logical {
        Key::Named(named) => *named,
        _ => unnamed_fkey(ev.logical, ev.physical)?,
    };
    let right = ev.location == KeyLocation::Right;
    let side = |left: u32| if right { left + 6 } else { left };
    use NamedKey as N;
    Some(match named {
        N::Escape => FKey::Escape,
        N::Enter => FKey::Enter,
        N::Tab => FKey::Tab,
        N::Backspace => FKey::Backspace,
        N::Insert => FKey::Insert,
        N::Delete => FKey::Delete,
        N::ArrowLeft => FKey::Left,
        N::ArrowRight => FKey::Right,
        N::ArrowUp => FKey::Up,
        N::ArrowDown => FKey::Down,
        N::PageUp => FKey::PageUp,
        N::PageDown => FKey::PageDown,
        N::Home => FKey::Home,
        N::End => FKey::End,
        N::F1 => FKey::F(1),
        N::F2 => FKey::F(2),
        N::F3 => FKey::F(3),
        N::F4 => FKey::F(4),
        N::F5 => FKey::F(5),
        N::F6 => FKey::F(6),
        N::F7 => FKey::F(7),
        N::F8 => FKey::F(8),
        N::F9 => FKey::F(9),
        N::F10 => FKey::F(10),
        N::F11 => FKey::F(11),
        N::F12 => FKey::F(12),
        N::F13 => FKey::F(13),
        N::F14 => FKey::F(14),
        N::F15 => FKey::F(15),
        N::F16 => FKey::F(16),
        N::F17 => FKey::F(17),
        N::F18 => FKey::F(18),
        N::F19 => FKey::F(19),
        N::F20 => FKey::F(20),
        N::F21 => FKey::F(21),
        N::F22 => FKey::F(22),
        N::F23 => FKey::F(23),
        N::F24 => FKey::F(24),
        N::F25 => FKey::F(25),
        N::F26 => FKey::F(26),
        N::F27 => FKey::F(27),
        N::F28 => FKey::F(28),
        N::F29 => FKey::F(29),
        N::F30 => FKey::F(30),
        N::F31 => FKey::F(31),
        N::F32 => FKey::F(32),
        N::F33 => FKey::F(33),
        N::F34 => FKey::F(34),
        N::F35 => FKey::F(35),
        N::CapsLock => FKey::Pua(KP_CAPS_LOCK),
        N::ScrollLock => FKey::Pua(KP_SCROLL_LOCK),
        N::NumLock => FKey::Pua(KP_NUM_LOCK),
        N::PrintScreen => FKey::Pua(57361),
        N::Pause => FKey::Pua(57362),
        N::ContextMenu => FKey::Pua(57363),
        N::MediaPlay => FKey::Pua(57428),
        N::MediaPause => FKey::Pua(57429),
        N::MediaPlayPause => FKey::Pua(57430),
        N::MediaStop => FKey::Pua(57432),
        N::MediaFastForward => FKey::Pua(57433),
        N::MediaRewind => FKey::Pua(57434),
        N::MediaTrackNext => FKey::Pua(57435),
        N::MediaTrackPrevious => FKey::Pua(57436),
        N::MediaRecord => FKey::Pua(57437),
        N::AudioVolumeDown => FKey::Pua(57438),
        N::AudioVolumeUp => FKey::Pua(57439),
        N::AudioVolumeMute => FKey::Pua(57440),
        N::Shift => FKey::Modifier(side(57441), KM_SHIFT),
        N::Control => FKey::Modifier(side(57442), KM_CTRL),
        N::Alt => FKey::Modifier(side(57443), KM_ALT),
        N::Super => FKey::Modifier(side(57444), KM_SUPER),
        N::Hyper => FKey::Modifier(side(57445), 0),
        N::Meta => FKey::Modifier(side(57446), 0),
        N::AltGraph => FKey::Modifier(57453, 0),
        _ => return None,
    })
}

/// Keypad keys by physical position. `logical` is a Character when NumLock is
/// on (a digit / the decimal separator) and a navigation key when it is off.
fn keypad_key(code: KeyCode, logical: &Key) -> Option<FKey> {
    use KeyCode as K;
    let digit = matches!(logical, Key::Character(_));
    let pick = |d: u8, nav: FKey| if digit { FKey::KpDigit(d) } else { nav };
    Some(match code {
        K::Numpad0 => pick(0, FKey::KpInsert),
        K::Numpad1 => pick(1, FKey::KpEnd),
        K::Numpad2 => pick(2, FKey::KpDown),
        K::Numpad3 => pick(3, FKey::KpPageDown),
        K::Numpad4 => pick(4, FKey::KpLeft),
        K::Numpad5 => pick(5, FKey::KpBegin),
        K::Numpad6 => pick(6, FKey::KpRight),
        K::Numpad7 => pick(7, FKey::KpHome),
        K::Numpad8 => pick(8, FKey::KpUp),
        K::Numpad9 => pick(9, FKey::KpPageUp),
        K::NumpadDecimal => {
            if digit {
                FKey::KpDecimal
            } else {
                FKey::KpDelete
            }
        }
        K::NumpadDivide => FKey::KpDivide,
        K::NumpadMultiply | K::NumpadStar => FKey::KpMultiply,
        K::NumpadSubtract => FKey::KpSubtract,
        K::NumpadAdd => FKey::KpAdd,
        K::NumpadEnter => FKey::KpEnter,
        K::NumpadEqual => FKey::KpEqual,
        K::NumpadComma => FKey::KpSeparator,
        _ => return None,
    })
}

/// The single char of a one-char string.
fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// The kitty key code of a text key: the UNSHIFTED key on the current layout
/// (`ı` on the Turkish-Q I key, `с` on a Cyrillic layout), lower-cased.
fn text_key_code(ev: &KeyInput<'_>) -> Option<u32> {
    let as_char = |k: &Key| match k {
        Key::Character(s) => single_char(s),
        Key::Named(NamedKey::Space) => Some(' '),
        _ => None,
    };
    let c = as_char(ev.key_without_modifiers).or_else(|| as_char(ev.logical))?;
    let mut lower = c.to_lowercase();
    let c = match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    };
    Some(u32::from(c))
}

/// The PC-101 (US) key at this physical position — the protocol's "base layout
/// key", which lets Ctrl+С on a Cyrillic layout match a Ctrl+C shortcut.
fn base_layout_char(physical: PhysicalKey) -> Option<char> {
    use KeyCode as K;
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        K::KeyA => 'a', K::KeyB => 'b', K::KeyC => 'c', K::KeyD => 'd', K::KeyE => 'e',
        K::KeyF => 'f', K::KeyG => 'g', K::KeyH => 'h', K::KeyI => 'i', K::KeyJ => 'j',
        K::KeyK => 'k', K::KeyL => 'l', K::KeyM => 'm', K::KeyN => 'n', K::KeyO => 'o',
        K::KeyP => 'p', K::KeyQ => 'q', K::KeyR => 'r', K::KeyS => 's', K::KeyT => 't',
        K::KeyU => 'u', K::KeyV => 'v', K::KeyW => 'w', K::KeyX => 'x', K::KeyY => 'y',
        K::KeyZ => 'z',
        K::Digit0 => '0', K::Digit1 => '1', K::Digit2 => '2', K::Digit3 => '3',
        K::Digit4 => '4', K::Digit5 => '5', K::Digit6 => '6', K::Digit7 => '7',
        K::Digit8 => '8', K::Digit9 => '9',
        K::Minus => '-', K::Equal => '=', K::BracketLeft => '[', K::BracketRight => ']',
        K::Backslash => '\\', K::Semicolon => ';', K::Quote => '\'', K::Backquote => '`',
        K::Comma => ',', K::Period => '.', K::Slash => '/', K::Space => ' ',
        _ => return None,
    })
}

/// The text a key event inputs, for the protocol: winit leaves Ctrl and Alt out
/// of `KeyEvent::text`, but under Ctrl / Meta / Super the key produces no text
/// input. Control text (Enter's `\r`, …) is never text.
fn kitty_text<'a>(ev: &KeyInput<'a>, meta: bool) -> Option<&'a str> {
    if ev.mods.ctrl || meta || ev.mods.super_ {
        return None;
    }
    let text = ev.text?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(text)
}

/// Encode a key event with the kitty keyboard protocol for the given
/// `modes.kitty_flags`. `None` = nothing to send. Call only when one of
/// [`KITTY_DISAMBIGUATE`], [`KITTY_REPORT_EVENT_TYPES`] or
/// [`KITTY_REPORT_ALL_KEYS`] is set ([`decide_key_event`] does).
pub fn encode_kitty_key(ev: &KeyInput<'_>, modes: &KeyModes, opts: &KeyOptions) -> Option<Vec<u8>> {
    let flags = modes.kitty_flags;
    let disambiguate = flags & KITTY_DISAMBIGUATE != 0;
    let report_events = flags & KITTY_REPORT_EVENT_TYPES != 0;
    let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;

    // A dead key (or the middle of a Compose sequence) inputs nothing by itself:
    // the key that completes it carries the composed text. Same as legacy.
    if matches!(ev.logical, Key::Dead(_)) {
        return None;
    }
    let m = ev.mods;
    let meta = alt_is_meta(m, opts);
    let mut fkey = functional_key(ev);
    if !report_all && fkey.is_some_and(FKey::is_modifier) {
        return None;
    }
    let text = kitty_text(ev, meta);

    // Alt is reported when it acts as Meta, on non-text functional keys, and —
    // macOS with Option composing — whenever Option produced no printable
    // character (Option+Enter / Backspace / Tab / Esc keep their Meta meaning, as
    // in the legacy encoder).
    let alt_bit = m.alt
        && (meta
            || composed_option_text(ev).is_none()
            || fkey.is_some_and(|k| !k.is_textual()));
    let mut mods = (u8::from(m.shift) * KM_SHIFT)
        | (u8::from(alt_bit) * KM_ALT)
        | (u8::from(m.ctrl) * KM_CTRL)
        | (u8::from(m.super_) * KM_SUPER);
    // A modifier key's own bit reflects the state INCLUDING this event (winit
    // delivers ModifiersChanged after the key event).
    if let Some(FKey::Modifier(_, bit)) = fkey {
        if ev.kind == KeyEventKind::Release {
            mods &= !bit;
        } else {
            mods |= bit;
        }
    }

    let mut text_key = None;
    if !disambiguate && !report_all {
        if let Some(k) = fkey {
            match k.without_keypad() {
                Ok(f) => fkey = Some(f),
                Err(c) => {
                    fkey = None;
                    text_key = Some(u32::from(c));
                }
            }
        }
    }
    if fkey.is_none() && text_key.is_none() {
        text_key = text_key_code(ev);
    }
    if fkey.is_none() && text_key.is_none() && text.is_none() {
        return None;
    }

    // Text goes out as plain UTF-8 unless every key is to be an escape code.
    if !report_all && ev.kind != KeyEventKind::Release {
        if let Some(t) = text {
            return Some(t.as_bytes().to_vec());
        }
    }
    if !report_events && ev.kind == KeyEventKind::Release {
        return None;
    }
    let embedded = if flags & KITTY_REPORT_ASSOCIATED_TEXT != 0 && ev.kind != KeyEventKind::Release {
        text
    } else {
        None
    };

    if let Some(k) = fkey {
        return kitty_functional(k, mods, ev.kind, flags, embedded);
    }

    let key = text_key.unwrap_or(0);
    if key == 0 && embedded.is_none() {
        return None;
    }
    let shifted = if m.shift {
        match ev.logical {
            Key::Character(s) => single_char(s).map(u32::from).filter(|&c| c != key),
            _ => None,
        }
    } else {
        None
    };
    let base = base_layout_char(ev.physical).map(u32::from).filter(|&b| b != key);
    kitty_text_key(key, shifted, base, mods, ev.kind, flags, embedded)
}

/// `encode_function_key` for the flags-active case (never legacy mode: DECCKM
/// and the SS3 F1–F4 forms don't apply once a program enabled the protocol).
fn kitty_functional(
    k: FKey,
    mods: u8,
    kind: KeyEventKind,
    flags: u8,
    embedded: Option<&str>,
) -> Option<Vec<u8>> {
    let disambiguate = flags & KITTY_DISAMBIGUATE != 0;
    let report_events = flags & KITTY_REPORT_EVENT_TYPES != 0;
    let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
    if mods == 0 {
        if !disambiguate && !report_all && k == FKey::Escape && kind != KeyEventKind::Release {
            return Some(vec![0x1b]);
        }
        // Enter / Tab / Backspace keep their legacy bytes (and report no release)
        // so a user can still type `reset` after a program that set the mode dies.
        if !report_all {
            let legacy: &[u8] = match k {
                FKey::Enter => b"\r",
                FKey::Backspace => b"\x7f",
                FKey::Tab => b"\t",
                _ => b"",
            };
            if !legacy.is_empty() {
                return (kind != KeyEventKind::Release).then(|| legacy.to_vec());
            }
        }
    }
    let (number, fin) = k.number_and_final();
    let add_actions = report_events && kind != KeyEventKind::Press;
    Some(kitty_csi(number, None, None, mods, kind, add_actions, embedded, fin))
}

/// `encode_key` for a text key (`key` = its kitty code, 0 = text with no key).
fn kitty_text_key(
    key: u32,
    shifted: Option<u32>,
    base: Option<u32>,
    mods: u8,
    kind: KeyEventKind,
    flags: u8,
    embedded: Option<&str>,
) -> Option<Vec<u8>> {
    let disambiguate = flags & KITTY_DISAMBIGUATE != 0;
    let report_events = flags & KITTY_REPORT_EVENT_TYPES != 0;
    let report_alternates = flags & KITTY_REPORT_ALTERNATE_KEYS != 0;
    let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
    let add_actions = report_events && kind != KeyEventKind::Press;
    let add_alternates = report_alternates && (shifted.is_some() || base.is_some());
    let simple = !add_actions && !add_alternates && embedded.is_none();
    if simple {
        if mods == 0 {
            if report_all {
                return Some(kitty_csi(key, None, None, 0, kind, false, None, b'u'));
            }
            // Text keys already went out as their text; reaching here means the
            // key produced none (an AltGr combo with no level-3 symbol, …), and
            // like the legacy encoder we send nothing rather than the key code.
            return None;
        }
        // Event-types-only mode keeps the legacy bytes where they exist.
        if !disambiguate && !report_all {
            if is_kitty_legacy_ascii(key) || shifted.is_some_and(is_kitty_legacy_ascii) {
                if let Some(bytes) = kitty_legacy_ascii(key, shifted, mods) {
                    return Some(bytes);
                }
            }
            if matches!(mods, KM_CTRL | KM_ALT) || mods == KM_CTRL | KM_ALT {
                if let Some(b) = base.filter(|&b| !is_kitty_legacy_ascii(key) && is_kitty_legacy_ascii(b)) {
                    if let Some(bytes) = kitty_legacy_ascii(b, None, mods) {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    let (shifted, base) = if add_alternates { (shifted, base) } else { (None, None) };
    Some(kitty_csi(key, shifted, base, mods, kind, add_actions, embedded, b'u'))
}

/// kitty's `serialize`: `CSI key[:shifted[:base]] [; mods[:event]] [; text] final`.
#[allow(clippy::too_many_arguments)]
fn kitty_csi(
    key: u32,
    shifted: Option<u32>,
    base: Option<u32>,
    mods: u8,
    kind: KeyEventKind,
    add_actions: bool,
    text: Option<&str>,
    fin: u8,
) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(24);
    s.push_str("\x1b[");
    let add_alternates = shifted.is_some() || base.is_some();
    let second = mods != 0 || add_actions;
    let text = text.filter(|t| !t.is_empty());
    if key != 1 || add_alternates || second || text.is_some() {
        let _ = write!(s, "{key}");
    }
    if add_alternates {
        s.push(':');
        if let Some(sh) = shifted {
            let _ = write!(s, "{sh}");
        }
        if let Some(b) = base {
            let _ = write!(s, ":{b}");
        }
    }
    if second || text.is_some() {
        s.push(';');
        if second {
            let _ = write!(s, "{}", u16::from(mods) + 1);
        }
        if add_actions {
            let _ = write!(s, ":{}", kind.kitty_number());
        }
    }
    if let Some(t) = text {
        for (i, c) in t.chars().enumerate() {
            let _ = write!(s, "{}{}", if i == 0 { ';' } else { ':' }, u32::from(c));
        }
    }
    s.push(char::from(fin));
    s.into_bytes()
}

/// kitty's `is_legacy_ascii_key`.
fn is_kitty_legacy_ascii(key: u32) -> bool {
    char::from_u32(key).is_some_and(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || "!@#$%^&*()`~-_=+[{]}\\|;:'\",<.>/? ".contains(c)
    })
}

/// kitty's `encode_printable_ascii_key_legacy` (event-types-only mode).
fn kitty_legacy_ascii(key: u32, shifted: Option<u32>, mods: u8) -> Option<Vec<u8>> {
    let mut k = u8::try_from(key).ok().filter(u8::is_ascii)?;
    if mods == 0 {
        return Some(vec![k]);
    }
    let mut rest = mods;
    if mods & KM_SHIFT != 0 {
        if let Some(sh) = shifted.and_then(|s| u8::try_from(s).ok()).filter(u8::is_ascii) {
            if sh != k && (mods & KM_CTRL == 0 || !k.is_ascii_lowercase()) {
                k = sh;
                rest &= !KM_SHIFT;
            }
        }
    }
    if mods == KM_SHIFT {
        return Some(vec![k]);
    }
    if rest == KM_ALT {
        return Some(vec![0x1b, k]);
    }
    if rest == KM_CTRL {
        return Some(vec![kitty_ctrled(k)]);
    }
    if rest == KM_CTRL | KM_ALT {
        return Some(vec![0x1b, kitty_ctrled(k)]);
    }
    if k == b' ' {
        if rest == KM_CTRL | KM_SHIFT {
            return Some(vec![kitty_ctrled(k)]);
        }
        if rest == KM_ALT | KM_SHIFT {
            return Some(vec![0x1b, k]);
        }
    }
    None
}

/// kitty's `ctrled_key` (the spec's "legacy ctrl mapping of ASCII keys").
fn kitty_ctrled(k: u8) -> u8 {
    match k {
        b' ' | b'2' | b'@' => 0,
        b'/' | b'7' | b'_' => 31,
        b'3' | b'[' => 27,
        b'4' | b'\\' => 28,
        b'5' | b']' => 29,
        b'6' | b'^' | b'~' => 30,
        b'8' | b'?' => 127,
        b'a'..=b'z' => k - b'a' + 1,
        other => other,
    }
}

// ── Mouse reporting (xterm ctlseqs "Mouse Tracking") ─────────────────────────

/// A mouse button as the xterm mouse protocols number it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Button 8 ("back").
    Back,
    /// Button 9 ("forward").
    Forward,
}

impl MouseBtn {
    /// xterm's base button code, before the modifier and motion bits: 0/1/2 for
    /// left/middle/right, 64–67 for the wheel, 128/129 for buttons 8/9.
    pub fn code(self) -> u8 {
        match self {
            MouseBtn::Left => 0,
            MouseBtn::Middle => 1,
            MouseBtn::Right => 2,
            MouseBtn::WheelUp => 64,
            MouseBtn::WheelDown => 65,
            MouseBtn::WheelLeft => 66,
            MouseBtn::WheelRight => 67,
            MouseBtn::Back => 128,
            MouseBtn::Forward => 129,
        }
    }

    pub fn is_wheel(self) -> bool {
        matches!(
            self,
            MouseBtn::WheelUp | MouseBtn::WheelDown | MouseBtn::WheelLeft | MouseBtn::WheelRight
        )
    }

    /// From winit's button (`None` for `Other(n)`, which xterm can't express).
    pub fn from_winit(b: winit::event::MouseButton) -> Option<Self> {
        use winit::event::MouseButton as W;
        Some(match b {
            W::Left => MouseBtn::Left,
            W::Middle => MouseBtn::Middle,
            W::Right => MouseBtn::Right,
            W::Back => MouseBtn::Back,
            W::Forward => MouseBtn::Forward,
            W::Other(_) => return None,
        })
    }
}

/// What happened to the button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAct {
    Press,
    Release,
    /// The pointer moved to a new cell.
    Motion,
}

/// One mouse event to report. `button` is the button pressed / released, or
/// — for [`MouseAct::Motion`] — the button held during the move (`None` = no
/// button held). `alt` is reported as xterm's "meta" bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub button: Option<MouseBtn>,
    pub act: MouseAct,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl MouseReport {
    /// A press / release / held-button motion with no modifiers.
    pub fn new(button: Option<MouseBtn>, act: MouseAct) -> Self {
        MouseReport { button, act, shift: false, alt: false, ctrl: false }
    }
}

/// Which events the program asked for: DECSET 9 (X10: presses only),
/// 1000 (presses + releases + wheel), 1002 (+ motion while a button is held),
/// 1003 (+ all motion). The highest mode set wins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseTracking {
    #[default]
    Off,
    X10,
    Normal,
    ButtonEvent,
    AnyEvent,
}

impl MouseTracking {
    /// From the DECSET 9 / 1000 / 1002 / 1003 bits.
    pub fn from_modes(x10: bool, normal: bool, button_event: bool, any_event: bool) -> Self {
        if any_event {
            MouseTracking::AnyEvent
        } else if button_event {
            MouseTracking::ButtonEvent
        } else if normal {
            MouseTracking::Normal
        } else if x10 {
            MouseTracking::X10
        } else {
            MouseTracking::Off
        }
    }
}

/// How reports are encoded: legacy `CSI M Cb Cx Cy` bytes (coordinates ≤ 223),
/// DECSET 1005 UTF-8 (≤ 2015), 1006 SGR (`CSI < Cb;Cx;Cy M/m`, unbounded,
/// distinct releases) or 1015 urxvt (`CSI Cb;Cx;Cy M`, decimal). When several
/// are set, SGR wins over urxvt over UTF-8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Legacy,
    Utf8,
    Sgr,
    Urxvt,
}

impl MouseEncoding {
    /// From the DECSET 1005 / 1006 / 1015 bits.
    pub fn from_modes(utf8: bool, sgr: bool, urxvt: bool) -> Self {
        if sgr {
            MouseEncoding::Sgr
        } else if urxvt {
            MouseEncoding::Urxvt
        } else if utf8 {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Legacy
        }
    }
}

/// Should this event be reported under `tracking`? X10 reports button presses
/// only; 1000 adds releases and the wheel (wheel "releases" never exist);
/// 1002 adds motion while a button is held; 1003 adds every motion.
pub fn mouse_reportable(tracking: MouseTracking, r: &MouseReport) -> bool {
    let wheel = r.button.is_some_and(MouseBtn::is_wheel);
    let click = match r.act {
        MouseAct::Press => r.button.is_some(),
        MouseAct::Release => r.button.is_some() && !wheel,
        MouseAct::Motion => false,
    };
    match tracking {
        MouseTracking::Off => false,
        MouseTracking::X10 => {
            r.act == MouseAct::Press
                && matches!(r.button, Some(MouseBtn::Left | MouseBtn::Middle | MouseBtn::Right))
        }
        MouseTracking::Normal => click,
        MouseTracking::ButtonEvent => {
            click || (r.act == MouseAct::Motion && r.button.is_some_and(|b| !b.is_wheel()))
        }
        MouseTracking::AnyEvent => click || r.act == MouseAct::Motion,
    }
}

/// Encode one report at 1-based cell `(col, row)` in `enc`. Coordinates are
/// clamped to `1..=` what the encoding can carry (legacy 223, UTF-8 2015). In
/// X10 `tracking` no modifier bits are sent (that mode predates them).
pub fn encode_mouse_report(
    r: &MouseReport,
    col: usize,
    row: usize,
    enc: MouseEncoding,
    tracking: MouseTracking,
) -> Vec<u8> {
    // Cb before the encoding's +32 offset: the base button (3 = "a button was
    // released" in every encoding but SGR, which keeps the button), plus shift
    // 4, meta 8, ctrl 16, motion 32. Motion with no button held is 3 + 32 = 35.
    let mut cb = match r.act {
        MouseAct::Press => u32::from(r.button.map_or(3, MouseBtn::code)),
        MouseAct::Release if enc == MouseEncoding::Sgr => u32::from(r.button.map_or(3, MouseBtn::code)),
        MouseAct::Release => 3,
        MouseAct::Motion => u32::from(r.button.map_or(3, MouseBtn::code)) + 32,
    };
    if tracking != MouseTracking::X10 {
        cb += u32::from(r.shift) * 4 + u32::from(r.alt) * 8 + u32::from(r.ctrl) * 16;
    }
    match enc {
        MouseEncoding::Sgr => {
            let fin = if r.act == MouseAct::Release { 'm' } else { 'M' };
            format!("\x1b[<{cb};{};{}{fin}", col.max(1), row.max(1)).into_bytes()
        }
        MouseEncoding::Urxvt => format!("\x1b[{};{};{}M", 32 + cb, col.max(1), row.max(1)).into_bytes(),
        MouseEncoding::Legacy => {
            // One byte each: 32 + value must fit in a byte, so positions stop
            // at 223 (and Cb tops out at 32 + 191).
            let byte = |v: u32| u8::try_from(32 + v).unwrap_or(u8::MAX);
            vec![
                0x1b,
                b'[',
                b'M',
                byte(cb),
                byte(col.clamp(1, 223) as u32),
                byte(row.clamp(1, 223) as u32),
            ]
        }
        MouseEncoding::Utf8 => {
            // Like legacy, but each value ≥ 128 travels as a two-byte UTF-8
            // sequence, so positions reach 2015 (= 2047 − 32).
            let mut out = vec![0x1b, b'[', b'M'];
            for v in [cb, col.clamp(1, 2015) as u32, row.clamp(1, 2015) as u32] {
                let c = char::from_u32(32 + v).unwrap_or(' ');
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            out
        }
    }
}

/// A mouse event to encode for an application that enabled mouse reporting —
/// the original left-button/vertical-wheel subset, kept for existing call
/// sites. New code builds a [`MouseReport`] (all buttons, modifiers, encodings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    /// Left button pressed.
    LeftPress,
    /// Left button released.
    LeftRelease,
    /// Wheel scrolled up (button 64).
    WheelUp,
    /// Wheel scrolled down (button 65).
    WheelDown,
    /// Pointer motion report (modes 1002 button-drag / 1003 any-motion). The
    /// `button` is the BASE button code held during the move (0 left, 1 middle,
    /// 2 right, 3 = no button); the encoder adds the 0x20 motion bit. Emitted
    /// once per cell change while the app requested motion reporting.
    Motion { button: u8 },
}

impl MouseEvent {
    /// The equivalent unmodified [`MouseReport`].
    pub fn to_report(self) -> MouseReport {
        match self {
            MouseEvent::LeftPress => MouseReport::new(Some(MouseBtn::Left), MouseAct::Press),
            MouseEvent::LeftRelease => MouseReport::new(Some(MouseBtn::Left), MouseAct::Release),
            MouseEvent::WheelUp => MouseReport::new(Some(MouseBtn::WheelUp), MouseAct::Press),
            MouseEvent::WheelDown => MouseReport::new(Some(MouseBtn::WheelDown), MouseAct::Press),
            MouseEvent::Motion { button } => {
                let held = match button {
                    0 => Some(MouseBtn::Left),
                    1 => Some(MouseBtn::Middle),
                    2 => Some(MouseBtn::Right),
                    _ => None,
                };
                MouseReport::new(held, MouseAct::Motion)
            }
        }
    }
}

/// Encode a mouse event in the format the running application requested:
/// SGR (1006) when `sgr`, else the legacy encoding.
pub fn encode_mouse(event: MouseEvent, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    let enc = if sgr { MouseEncoding::Sgr } else { MouseEncoding::Legacy };
    encode_mouse_report(&event.to_report(), col, row, enc, MouseTracking::Normal)
}

/// Encode a mouse event as an SGR (1006) mouse report: `\e[<Cb;Cx;CyM` for a
/// press/motion and `\e[<Cb;Cx;Cym` for a release (wheel events are always
/// presses). `col`/`row` are 1-based and clamped to a minimum of 1.
pub fn encode_sgr_mouse(event: MouseEvent, col: usize, row: usize) -> Vec<u8> {
    encode_mouse(event, col, row, true)
}

/// Encode a mouse event as a legacy report: `\e[M` then `32 + Cb`, `32 + col`,
/// `32 + row` (1-based, clamped to 223 so each fits a byte). A release is
/// button 3 — legacy encodings can't say which button was released.
pub fn encode_x10_mouse(event: MouseEvent, col: usize, row: usize) -> Vec<u8> {
    encode_mouse(event, col, row, false)
}

/// Wheel delta → fractional LINES (positive = up, into history). A wheel notch
/// (`LineDelta`) is 3 lines; a touchpad's `PixelDelta` (physical px) converts at
/// the real `cell_h_px`, so one cell of finger travel scrolls one line at any
/// font size and DPI (a non-positive / non-finite cell height falls back to
/// 20 px).
pub fn wheel_lines(delta: MouseScrollDelta, cell_h_px: f32) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y * 3.0,
        MouseScrollDelta::PixelDelta(p) => (p.y / f64::from(px_or_default(cell_h_px))) as f32,
    }
}

/// Horizontal twin of [`wheel_lines`] in COLUMNS (positive = the content moves
/// right, i.e. xterm's wheel-left button 66).
pub fn wheel_columns(delta: MouseScrollDelta, cell_w_px: f32) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(x, _) => x * 3.0,
        MouseScrollDelta::PixelDelta(p) => (p.x / f64::from(px_or_default(cell_w_px))) as f32,
    }
}

fn px_or_default(px: f32) -> f32 {
    if px.is_finite() && px > 0.0 {
        px
    } else {
        20.0
    }
}

/// Counts consecutive clicks for double-click (word) / triple-click (line)
/// selection: a press of the SAME button within [`ClickTracker::INTERVAL`] of
/// the previous one and within [`ClickTracker::SLOP_PX`] of it continues the
/// sequence 1 → 2 → 3, and a fourth starts over at 1.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<(winit::event::MouseButton, Instant, f32, f32, u8)>,
}

impl ClickTracker {
    /// Max gap between presses of one sequence (the common desktop default).
    pub const INTERVAL: Duration = Duration::from_millis(400);
    /// Max pointer travel between presses, in the caller's pixel units.
    pub const SLOP_PX: f32 = 5.0;

    pub fn new() -> Self {
        Self::default()
    }

    /// Register a press at `(x, y)` and return its count: 1 single, 2 double,
    /// 3 triple.
    pub fn press(&mut self, button: winit::event::MouseButton, at: Instant, x: f32, y: f32) -> u8 {
        let count = match self.last {
            Some((b, t, px, py, n))
                if b == button
                    && at.saturating_duration_since(t) <= Self::INTERVAL
                    && (x - px).hypot(y - py) <= Self::SLOP_PX =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.last = Some((button, at, x, y, count));
        count
    }

    /// Forget the sequence (focus loss, tab switch, a drag in between, …).
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_physical(code: KeyCode) -> PhysicalKey {
        PhysicalKey::Code(code)
    }
    fn make_logical_char(s: &'static str) -> Key {
        Key::Character(winit::keyboard::SmolStr::new(s))
    }

    /// Oracle harness: run `decide_key` through the DEFAULT keymap with `super_ =
    /// false`, so the existing (pre-refactor) expectations become a byte-identical
    /// regression oracle for the default bindings.
    #[allow(clippy::too_many_arguments)]
    fn dk(
        ctrl: bool,
        shift: bool,
        alt: bool,
        physical: PhysicalKey,
        logical: &Key,
        panel_open: bool,
        app_cursor: bool,
        alt_screen: bool,
    ) -> KeyAction {
        let km = crate::keymap::KeyMap::defaults();
        decide_key(&km, ctrl, shift, alt, false, physical, logical, panel_open, app_cursor, alt_screen)
    }

    /// Same as `dk` but with the `super_` (macOS Cmd) bit set — for the swallow +
    /// Cmd-default oracle.
    #[allow(clippy::too_many_arguments)]
    fn dk_super(
        ctrl: bool,
        shift: bool,
        alt: bool,
        physical: PhysicalKey,
        logical: &Key,
    ) -> KeyAction {
        let km = crate::keymap::KeyMap::defaults();
        decide_key(&km, ctrl, shift, alt, true, physical, logical, false, false, false)
    }

    // ── Oracle: default keymap reproduces today's EXACT mod semantics ─────────

    #[test]
    fn alt_is_dont_care_for_ctrl_shift_defaults() {
        // Today's Ctrl+Shift block never tested Alt, so Ctrl+Alt+Shift+T is still
        // NewTab (the default keymap seeds the Alt-flipped variant).
        assert_eq!(
            dk(true, true, true, make_physical(KeyCode::KeyT), &make_logical_char("T"), false, false, false),
            KeyAction::NewTab
        );
        // DELIBERATE v0.26 change: Ctrl+Alt+= is transparency now (it used to be
        // an Alt-don't-care FontUp) — moving opacity off Ctrl+Shift freed
        // Ctrl+'+' on Turkish-Q and Ctrl+_ (0x1f) everywhere.
        assert_eq!(
            dk(true, false, true, make_physical(KeyCode::Equal), &make_logical_char("="), false, false, false),
            KeyAction::OpacityUp
        );
        // Ctrl+Alt+Tab is still NextTab.
        assert_eq!(
            dk(true, false, true, make_physical(KeyCode::Tab), &Key::Named(NamedKey::Tab), false, false, false),
            KeyAction::NextTab
        );
    }

    #[test]
    fn hint_and_copy_mode_default_chords_resolve() {
        // Ctrl+Shift+H → HintMode; Ctrl+Shift+Space → CopyMode. Both flow out of
        // the keymap lookup in decide_key (never the ctrl_byte fallback).
        assert_eq!(
            dk(true, true, false, make_physical(KeyCode::KeyH), &make_logical_char("H"), false, false, false),
            KeyAction::HintMode
        );
        assert_eq!(
            dk(true, true, false, make_physical(KeyCode::Space), &Key::Named(NamedKey::Space), false, false, false),
            KeyAction::CopyMode
        );
        // Ctrl+Space (no Shift) must still reach the PTY as NUL (0x00), NOT CopyMode.
        assert_eq!(
            dk(true, false, false, make_physical(KeyCode::Space), &Key::Named(NamedKey::Space), false, false, false),
            KeyAction::Send(vec![0x00])
        );
        // Ctrl+H (no Shift) must still be BS (0x08), NOT HintMode.
        assert_eq!(
            dk(true, false, false, make_physical(KeyCode::KeyH), &make_logical_char("h"), false, false, false),
            KeyAction::Send(vec![0x08])
        );
    }

    #[test]
    fn bare_f11_is_toggle_fullscreen_modified_f11_still_reaches_the_pty() {
        // THE one deliberate behaviour change of the fullscreen feature: bare F11
        // no longer sends `\e[23~` — the keymap lookup runs before the F-key
        // encoder and claims it.
        assert_eq!(
            dk(false, false, false, make_physical(KeyCode::F11), &Key::Named(NamedKey::F11), false, false, false),
            KeyAction::ToggleFullscreen
        );
        // The default chord is EXACT, so every MODIFIED F11 still reaches the PTY
        // as the xterm modified form `\e[23;{m}~` (m = 1 + 1*shift + 2*alt +
        // 4*ctrl) — the second escape hatch for TUIs, alongside
        // `[keys] toggle_fullscreen = ""`.
        for (ctrl, shift, alt, m) in [
            (false, true, false, 2),
            (false, false, true, 3),
            (true, false, false, 5),
            (true, true, false, 6),
        ] {
            assert_eq!(
                dk(ctrl, shift, alt, make_physical(KeyCode::F11), &Key::Named(NamedKey::F11), false, false, false),
                KeyAction::Send(format!("\x1b[23;{m}~").into_bytes()),
                "ctrl={ctrl} shift={shift} alt={alt}"
            );
        }
    }

    #[test]
    fn f11_pty_passthrough_restored_when_unbound() {
        // `[keys] toggle_fullscreen = ""` gives bare F11 back to the shell: the
        // F-key encoder is untouched and still produces `\e[23~`.
        let b = crate::config::KeyBindings {
            toggle_fullscreen: Some(crate::config::ChordSpec::One(String::new())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        assert_eq!(
            decide_key(
                &km, false, false, false, false,
                make_physical(KeyCode::F11), &Key::Named(NamedKey::F11), false, false, false
            ),
            KeyAction::Send(b"\x1b[23~".to_vec())
        );
    }

    #[test]
    fn the_menu_key_opens_the_context_menu_before_any_encoder() {
        // Bare Menu is the context-menu action — for a kitty-protocol program
        // too (the keymap runs first, like F11).
        let menu = Key::Named(NamedKey::ContextMenu);
        let ev = kev(KeyCode::ContextMenu, &menu, None, KeyMods::default());
        let kitty = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        for modes in [KeyModes::default(), kitty] {
            assert_eq!(decide(&ev, modes, KeyOptions::default()), KeyAction::ContextMenu);
        }
        // `[keys] context_menu = ""` hands the key back: a kitty program gets
        // `CSI 57363 u`, a legacy one xterm's `CSI 29 ~` (Menu is the VT220's Do
        // key there) — it used to get nothing.
        let b = crate::config::KeyBindings {
            context_menu: Some(crate::config::ChordSpec::One(String::new())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        assert_eq!(
            decide_key_event(&km, &ev, &kitty, &KeyOptions::default(), false),
            KeyAction::Send(b"\x1b[57363u".to_vec())
        );
        assert_eq!(
            decide_key_event(&km, &ev, &KeyModes::default(), &KeyOptions::default(), false),
            KeyAction::Send(b"\x1b[29~".to_vec())
        );
        // The default chord is EXACT: Shift+Menu is not the menu (it reaches the
        // program as xterm's `CSI 29 ; 2 ~`).
        let shifted = kev(KeyCode::ContextMenu, &menu, None, KeyMods { shift: true, ..KeyMods::default() });
        assert_eq!(decide(&shifted, KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"\x1b[29;2~".to_vec()));
    }

    #[test]
    fn shift_insert_is_paste_via_keymap() {
        assert_eq!(
            dk(false, true, false, make_physical(KeyCode::Insert), &Key::Named(NamedKey::Insert), false, false, false),
            KeyAction::Paste
        );
    }

    #[test]
    fn ctrl_underscore_still_unit_separator_not_font_down() {
        // Amendment 5: FontDown is '-' only. Ctrl+_ (no shift) must send 0x1f.
        assert_eq!(
            dk(true, false, false, make_physical(KeyCode::IntlRo), &make_logical_char("_"), false, false, false),
            KeyAction::Send(vec![0x1f])
        );
    }

    #[test]
    fn bare_cmd_chord_is_swallowed_not_injected() {
        // macOS Cmd swallow safety net: an unmapped `super && !ctrl && !alt` chord
        // returns None (never reaches the PTY), reproducing the old Cmd block.
        assert_eq!(
            dk_super(false, false, false, make_physical(KeyCode::KeyB), &make_logical_char("b")),
            KeyAction::None
        );
        // A Named key under Cmd is swallowed too.
        assert_eq!(
            dk_super(false, false, false, make_physical(KeyCode::Escape), &Key::Named(NamedKey::Escape)),
            KeyAction::None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cmd_defaults_resolve_and_are_shift_agnostic() {
        assert_eq!(dk_super(false, false, false, make_physical(KeyCode::KeyC), &make_logical_char("c")), KeyAction::Copy);
        assert_eq!(dk_super(false, true, false, make_physical(KeyCode::KeyC), &make_logical_char("C")), KeyAction::Copy);
        assert_eq!(dk_super(false, false, false, make_physical(KeyCode::KeyP), &make_logical_char("p")), KeyAction::OpenPalette);
        assert_eq!(dk_super(false, true, false, make_physical(KeyCode::KeyP), &make_logical_char("P")), KeyAction::OpenPalette);
        assert_eq!(dk_super(false, false, false, make_physical(KeyCode::KeyA), &make_logical_char("a")), KeyAction::SelectAll);
        assert_eq!(dk_super(false, false, false, make_physical(KeyCode::KeyQ), &make_logical_char("q")), KeyAction::Quit);
        assert_eq!(dk_super(false, false, false, make_physical(KeyCode::Comma), &make_logical_char(",")), KeyAction::TogglePanel);
    }

    #[test]
    fn ctrl_equal_maps_to_font_up() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Equal),
            &make_logical_char("="),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontUp);
    }

    #[test]
    fn ctrl_minus_maps_to_font_down() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Minus),
            &make_logical_char("-"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontDown);
    }

    #[test]
    fn turkish_q_ctrl_minus_is_font_down_not_up() {
        // Regression (F21): on Turkish-Q the '-'-engraved key is physical Equal.
        // Ctrl at that key must SHRINK the font (logical '-'), not grow it.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Equal), // physical position of the '-' key
            &make_logical_char("-"),        // engraved/produced character
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontDown, "logical '-' must win over physical Equal");
    }

    #[test]
    fn ctrl_shift_z_is_prev_prompt_not_sub() {
        // Ctrl+Shift+Z must be PrevPrompt and never leak 0x1a (SUB) to the PTY.
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyZ),
            &make_logical_char("Z"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::PrevPrompt);
        assert_ne!(action, KeyAction::Send(vec![0x1a]));
    }

    #[test]
    fn ctrl_shift_x_is_next_prompt_not_can() {
        // Ctrl+Shift+X must be NextPrompt and never leak 0x18 (CAN) to the PTY.
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyX),
            &make_logical_char("X"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::NextPrompt);
        assert_ne!(action, KeyAction::Send(vec![0x18]));
    }

    #[test]
    fn plain_ctrl_z_and_x_still_send_control_bytes() {
        // Regression guard: WITHOUT shift, Ctrl+Z / Ctrl+X keep their control
        // bytes (SUB 0x1a / CAN 0x18) — prompt-jump only steals the Shift chord.
        let z = dk(
            true, false, false,
            make_physical(KeyCode::KeyZ),
            &make_logical_char("z"),
            false, false, false,
        );
        assert_eq!(z, KeyAction::Send(vec![0x1a]));
        let x = dk(
            true, false, false,
            make_physical(KeyCode::KeyX),
            &make_logical_char("x"),
            false, false, false,
        );
        assert_eq!(x, KeyAction::Send(vec![0x18]));
    }

    #[test]
    fn turkish_q_minus_key_shift_is_unit_separator_and_alt_is_opacity_down() {
        // Turkish-Q (xkb `tr`): the '-' / '_' key is physical Equal.
        // Ctrl+Shift there types '_' → 0x1f (readline undo), never a shortcut…
        assert_eq!(
            dk(true, true, false, make_physical(KeyCode::Equal), &make_logical_char("_"), false, false, false),
            KeyAction::Send(vec![0x1f])
        );
        // …and transparency follows the engraved '-' under Ctrl+Alt (F21).
        assert_eq!(
            dk(true, false, true, make_physical(KeyCode::Equal), &make_logical_char("-"), false, false, false),
            KeyAction::OpacityDown
        );
    }

    #[test]
    fn qwertz_ctrl_minus_is_font_down_not_literal() {
        // Regression (F21): on German QWERTZ '-' sits at physical Slash; keying
        // on the logical char makes Ctrl+'-' shrink the font instead of typing
        // a literal '-' into the shell.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Slash),
            &make_logical_char("-"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontDown);
    }

    #[test]
    fn motion_report_sets_the_motion_bit() {
        // Regression (F5): a left-drag motion report (SGR) carries button 32
        // (base 0 + 0x20 motion bit); a no-button any-motion report carries 35.
        let left = encode_sgr_mouse(MouseEvent::Motion { button: 0 }, 5, 7);
        assert_eq!(left, b"\x1b[<32;5;7M".to_vec());
        let none = encode_sgr_mouse(MouseEvent::Motion { button: 3 }, 1, 1);
        assert_eq!(none, b"\x1b[<35;1;1M".to_vec());
    }

    #[test]
    fn ctrl_digit0_maps_to_font_reset() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Digit0),
            &make_logical_char("0"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontReset);
    }

    #[test]
    fn ctrl_shift_plus_zooms_like_ctrl_plus() {
        // DELIBERATE v0.26 change: Ctrl+Shift+'=' / '+' is font zoom (was
        // opacity) — on Turkish-Q / Swiss / Hungarian '+' exists only with Shift.
        for logical in ["=", "+"] {
            assert_eq!(
                dk(true, true, false, make_physical(KeyCode::Equal), &make_logical_char(logical), false, false, false),
                KeyAction::FontUp,
                "Ctrl+Shift+{logical}"
            );
        }
    }

    #[test]
    fn ctrl_shift_p_opens_the_command_palette() {
        // Ctrl+Shift+P is repurposed from Settings to the command palette.
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyP),
            &make_logical_char("P"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::OpenPalette);
    }

    #[test]
    fn ctrl_shift_o_still_toggles_settings() {
        // Settings remains reachable via the Ctrl+Shift+O alias.
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyO),
            &make_logical_char("O"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::TogglePanel);
    }

    #[test]
    fn plain_ctrl_p_still_sends_control_byte() {
        // Regression guard: plain Ctrl+P (no shift) keeps sending 0x10 (DLE) to
        // the PTY (readline previous-history); only Ctrl+SHIFT+P opens the palette.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::KeyP),
            &make_logical_char("p"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x10]));
    }

    #[test]
    fn ctrl_shift_t_maps_to_new_tab() {
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyT),
            &make_logical_char("T"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::NewTab);
    }

    #[test]
    fn ctrl_shift_w_maps_to_close_tab() {
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyW),
            &make_logical_char("W"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::CloseTab);
    }

    #[test]
    fn ctrl_shift_d_maps_to_detach_tab() {
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyD),
            &make_logical_char("D"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::DetachTab);
    }

    #[test]
    fn ctrl_shift_f_is_search_toggle() {
        let action = dk(
            true, true, false,
            make_physical(KeyCode::KeyF),
            &make_logical_char("F"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::SearchToggle);
    }

    #[test]
    fn ctrl_f_without_shift_still_sends_ack() {
        // Regression guard: plain Ctrl+F must keep sending 0x06 (ACK) to the
        // PTY (readline forward-char); only Ctrl+SHIFT+F opens the search bar.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::KeyF),
            &make_logical_char("f"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x06]));
    }

    #[test]
    fn ctrl_tab_maps_to_next_tab() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false, false,
        );
        assert_eq!(action, KeyAction::NextTab);
    }

    #[test]
    fn ctrl_shift_tab_maps_to_prev_tab() {
        let action = dk(
            true, true, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false, false,
        );
        assert_eq!(action, KeyAction::PrevTab);
    }

    #[test]
    fn ctrl_digit_maps_to_select_tab() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Digit3),
            &make_logical_char("3"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::SelectTab(2));
    }

    #[test]
    fn ctrl_digit0_still_font_reset() {
        // Ctrl+0 must remain FontReset, not a tab jump.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Digit0),
            &make_logical_char("0"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontReset);
    }

    // ── PageUp/PageDown: alt-screen aware (C1) ──────────────────────────────

    #[test]
    fn plain_page_keys_reach_the_program_on_the_primary_screen_too() {
        // DELIBERATE v0.26 change (was: host scrollback on the primary screen):
        // fzf's inline Ctrl+R / Ctrl+T, zsh's history paging and inline
        // prompt_toolkit apps need the key. Shift+PageUp/Down scrolls (below).
        let up = dk(
            false, false, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, false,
        );
        assert_eq!(up, KeyAction::Send(b"\x1b[5~".to_vec()));
        let down = dk(
            false, false, false,
            make_physical(KeyCode::PageDown),
            &Key::Named(NamedKey::PageDown),
            false, false, false,
        );
        assert_eq!(down, KeyAction::Send(b"\x1b[6~".to_vec()));
        let shift_up = dk(
            false, true, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, false,
        );
        assert_eq!(shift_up, KeyAction::ScrollPageUp);
    }

    #[test]
    fn rebinding_plain_page_keys_restores_scrolling_but_not_on_the_alt_screen() {
        // The documented one-liner for the pre-v0.26 behaviour.
        let b = crate::config::KeyBindings {
            scroll_page_up: Some(crate::config::ChordSpec::Many(vec![
                "Shift+PageUp".into(),
                "PageUp".into(),
            ])),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        assert!(km.warnings().is_empty(), "bare PageUp must be bindable: {:?}", km.warnings());
        let pgup = Key::Named(NamedKey::PageUp);
        let phys = make_physical(KeyCode::PageUp);
        // Primary screen: plain PageUp scrolls again.
        assert_eq!(
            decide_key(&km, false, false, false, false, phys, &pgup, false, false, false),
            KeyAction::ScrollPageUp
        );
        // Alternate screen: the bare key still reaches less / vim / htop…
        assert_eq!(
            decide_key(&km, false, false, false, false, phys, &pgup, false, false, true),
            KeyAction::Send(b"\x1b[5~".to_vec())
        );
        // …while Shift+PageUp stays the host-scrollback escape hatch.
        assert_eq!(
            decide_key(&km, false, true, false, false, phys, &pgup, false, false, true),
            KeyAction::ScrollPageUp
        );
    }

    #[test]
    fn page_keys_forward_to_pty_on_alt_screen() {
        // less/vim/htop (alt screen): plain PageUp/Down must reach the app.
        let up = dk(
            false, false, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, true,
        );
        assert_eq!(up, KeyAction::Send(b"\x1b[5~".to_vec()));
        let down = dk(
            false, false, false,
            make_physical(KeyCode::PageDown),
            &Key::Named(NamedKey::PageDown),
            false, false, true,
        );
        assert_eq!(down, KeyAction::Send(b"\x1b[6~".to_vec()));
    }

    #[test]
    fn shift_page_keys_always_scroll_even_on_alt_screen() {
        // Shift+PageUp/Down is the standard host-scrollback escape hatch.
        let up = dk(
            false, true, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, true,
        );
        assert_eq!(up, KeyAction::ScrollPageUp);
        let down = dk(
            false, true, false,
            make_physical(KeyCode::PageDown),
            &Key::Named(NamedKey::PageDown),
            false, false, true,
        );
        assert_eq!(down, KeyAction::ScrollPageDown);
    }

    // ── Ctrl+/ and Ctrl+_ → 0x1f (C4) ───────────────────────────────────────

    #[test]
    fn ctrl_slash_sends_unit_separator() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Slash),
            &make_logical_char("/"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x1f]));
    }

    #[test]
    fn ctrl_underscore_sends_unit_separator() {
        // A layout where "_" is produced by some non-Minus physical key.
        let action = dk(
            true, false, false,
            make_physical(KeyCode::IntlRo),
            &make_logical_char("_"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x1f]));
    }

    #[test]
    fn ctrl_shift_minus_sends_unit_separator_again() {
        // DELIBERATE v0.26 change: US Ctrl+Shift+Minus types "_" → 0x1f
        // (readline / emacs undo) instead of being swallowed as OpacityDown;
        // transparency moved to Ctrl+Alt+'-'.
        let action = dk(
            true, true, false,
            make_physical(KeyCode::Minus),
            &make_logical_char("_"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x1f]));
        assert_eq!(
            dk(true, false, true, make_physical(KeyCode::Minus), &make_logical_char("-"), false, false, false),
            KeyAction::OpacityDown
        );
    }

    #[test]
    fn ctrl_minus_on_physical_slash_is_not_unit_separator() {
        // German layouts: physical Slash produces "-". Ctrl+- there must NOT be
        // hijacked as 0x1f (it is keyed on the logical character, not position).
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Slash),
            &make_logical_char("-"),
            false, false, false,
        );
        assert_ne!(action, KeyAction::Send(vec![0x1f]));
    }

    #[test]
    fn ctrl_symbols_follow_the_typed_character_not_the_us_key_position() {
        // xkb ground truth (live on Xvfb, `setxkbmap tr` / `de` / `fr` /
        // `us -variant dvorak`): the key's position and what it types there.
        // A typed ASCII character decides alone — its C0 code when it has one
        // (xterm's Control rule), else the character itself. The US key at that
        // POSITION used to answer instead: Ctrl+; on Turkish-Q and Ctrl+# on
        // German sent FS (0x1c, SIGQUIT), Ctrl+; on Dvorak SUB (0x1a, SIGTSTP).
        let k = |ctrl_shift: bool, at: KeyCode, typed: &'static str| {
            dk(true, ctrl_shift, false, make_physical(at), &make_logical_char(typed), false, false, false)
        };
        let send = |b: &[u8]| KeyAction::Send(b.to_vec());
        for (shift, at, typed, want, layout) in [
            // Turkish-Q: `,` / `;` at Backslash; AltGr symbols (AltGr isn't Alt).
            (true, KeyCode::Backslash, ";", &b";"[..], "tr Ctrl+;"),
            (false, KeyCode::BracketRight, "~", b"\x1e", "tr Ctrl+AltGr+ü (~)"),
            (false, KeyCode::Minus, "\\", b"\x1c", "tr Ctrl+AltGr+* (\\)"),
            (false, KeyCode::Equal, "|", b"\x1c", "tr Ctrl+AltGr+- (|)"),
            // German QWERTZ: `#` / `'` at Backslash.
            (false, KeyCode::Backslash, "#", b"#", "de Ctrl+#"),
            (true, KeyCode::Backslash, "'", b"'", "de Ctrl+'"),
            (false, KeyCode::Minus, "\\", b"\x1c", "de Ctrl+AltGr+ß (\\)"),
            (false, KeyCode::BracketRight, "~", b"\x1e", "de Ctrl+AltGr++ (~)"),
            // AZERTY: `*` at Backslash, `$` at BracketRight.
            (false, KeyCode::Backslash, "*", b"*", "fr Ctrl+*"),
            (false, KeyCode::BracketRight, "$", b"$", "fr Ctrl+$"),
            // Dvorak: punctuation on US letter / bracket positions.
            (false, KeyCode::KeyQ, "'", b"'", "dvorak Ctrl+'"),
            (false, KeyCode::KeyE, ".", b".", "dvorak Ctrl+."),
            (false, KeyCode::KeyZ, ";", b";", "dvorak Ctrl+;"),
            (false, KeyCode::Minus, "[", b"\x1b", "dvorak Ctrl+["),
            (false, KeyCode::Equal, "]", b"\x1d", "dvorak Ctrl+]"),
            (true, KeyCode::Minus, "{", b"\x1b", "dvorak Ctrl+{"),
            // US: the C0 symbols (and their shifted twins) keep their bytes.
            (false, KeyCode::BracketLeft, "[", b"\x1b", "us Ctrl+["),
            (false, KeyCode::Backslash, "\\", b"\x1c", "us Ctrl+\\"),
            (false, KeyCode::BracketRight, "]", b"\x1d", "us Ctrl+]"),
            (true, KeyCode::BracketLeft, "{", b"\x1b", "us Ctrl+{"),
            (true, KeyCode::Backslash, "|", b"\x1c", "us Ctrl+|"),
            (true, KeyCode::BracketRight, "}", b"\x1d", "us Ctrl+}"),
            (true, KeyCode::Backquote, "~", b"\x1e", "us Ctrl+~"),
            (false, KeyCode::Semicolon, ";", b";", "us Ctrl+;"),
            // xterm / libxkbcommon: '`' is NUL (tmux / emacs C-@ where Ctrl+Space
            // switches the input method), and so is '2'; '3'…'7' are ESC…US, '8'
            // DEL — the kitty protocol's own legacy table (`kitty_ctrled`) agrees.
            (false, KeyCode::Backquote, "`", b"\x00", "us Ctrl+`"),
            (true, KeyCode::Digit2, "2", b"\x00", "fr Ctrl+Shift+é (2)"),
            (true, KeyCode::Digit3, "3", b"\x1b", "fr Ctrl+Shift+\" (3)"),
            (true, KeyCode::Digit4, "4", b"\x1c", "fr Ctrl+Shift+' (4)"),
            (true, KeyCode::Digit5, "5", b"\x1d", "fr Ctrl+Shift+( (5)"),
            (true, KeyCode::Digit6, "6", b"\x1e", "fr Ctrl+Shift+- (6)"),
            (true, KeyCode::Digit7, "7", b"\x1f", "fr Ctrl+Shift+è (7)"),
            (true, KeyCode::Digit8, "8", b"\x7f", "fr Ctrl+Shift+_ (8)"),
            (true, KeyCode::Digit1, "1", b"1", "fr Ctrl+Shift+& (1)"),
            (true, KeyCode::Digit9, "9", b"9", "fr Ctrl+Shift+ç (9)"),
            (false, KeyCode::Backslash, "`", b"\x00", "tr Ctrl+AltGr+, (`)"),
            // Non-ASCII letters keep the US position (Turkish-Q ğ / ü / ı,
            // German ü): the documented fallback for keys with no ASCII label.
            (false, KeyCode::BracketLeft, "ğ", b"\x1b", "tr Ctrl+ğ"),
            (false, KeyCode::BracketRight, "ü", b"\x1d", "tr Ctrl+ü"),
            (false, KeyCode::KeyI, "ı", b"\x09", "tr Ctrl+ı"),
            (false, KeyCode::Semicolon, "ş", "ş".as_bytes(), "tr Ctrl+ş"),
            (false, KeyCode::BracketLeft, "ü", b"\x1b", "de Ctrl+ü"),
        ] {
            assert_eq!(k(shift, at, typed), send(want), "{layout}");
        }
        // With Alt the same byte is ESC-prefixed; a plain character too.
        let ctrl_alt = |at: KeyCode, typed: &'static str| {
            dk(true, false, true, make_physical(at), &make_logical_char(typed), false, false, false)
        };
        assert_eq!(ctrl_alt(KeyCode::KeyZ, ";"), send(b"\x1b;"), "dvorak Ctrl+Alt+;");
        assert_eq!(ctrl_alt(KeyCode::Minus, "["), send(b"\x1b\x1b"), "dvorak Ctrl+Alt+[");
        // Space has no character: its position still gives NUL.
        assert_eq!(
            dk(true, false, false, make_physical(KeyCode::Space), &Key::Named(NamedKey::Space), false, false, false),
            send(b"\x00")
        );
        // Ctrl+3 is a tab jump by default; unbound (`select_tab_3 = ""`) it is
        // ESC, as in xterm — it used to type a '3'.
        let b = crate::config::KeyBindings {
            select_tab_3: Some(crate::config::ChordSpec::One(String::new())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        let three = make_logical_char("3");
        let at = make_physical(KeyCode::Digit3);
        assert_eq!(decide_key(&km, true, false, false, false, at, &three, false, false, false), send(b"\x1b"));
    }

    // ── Modified Home/End/Delete/Insert (C5) ────────────────────────────────

    #[test]
    fn shift_home_end_send_modified_csi() {
        let home = dk(
            false, true, false,
            make_physical(KeyCode::Home),
            &Key::Named(NamedKey::Home),
            false, false, false,
        );
        assert_eq!(home, KeyAction::Send(b"\x1b[1;2H".to_vec()));
        let end = dk(
            false, true, false,
            make_physical(KeyCode::End),
            &Key::Named(NamedKey::End),
            false, false, false,
        );
        assert_eq!(end, KeyAction::Send(b"\x1b[1;2F".to_vec()));
    }

    #[test]
    fn ctrl_delete_sends_modified_tilde_form() {
        let action = dk(
            true, false, false,
            make_physical(KeyCode::Delete),
            &Key::Named(NamedKey::Delete),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(b"\x1b[3;5~".to_vec()));
    }

    #[test]
    fn alt_insert_sends_modified_tilde_form() {
        let action = dk(
            false, false, true,
            make_physical(KeyCode::Insert),
            &Key::Named(NamedKey::Insert),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(b"\x1b[2;3~".to_vec()));
    }

    #[test]
    fn plain_home_end_delete_insert_unchanged() {
        // No modifiers → the plain xterm forms, exactly as before.
        for (named, code, bytes) in [
            (NamedKey::Home, KeyCode::Home, &b"\x1b[H"[..]),
            (NamedKey::End, KeyCode::End, &b"\x1b[F"[..]),
            (NamedKey::Delete, KeyCode::Delete, &b"\x1b[3~"[..]),
            (NamedKey::Insert, KeyCode::Insert, &b"\x1b[2~"[..]),
        ] {
            let action = dk(
                false, false, false,
                make_physical(code),
                &Key::Named(named),
                false, false, false,
            );
            assert_eq!(action, KeyAction::Send(bytes.to_vec()));
        }
    }

    // ── Scroll accumulator (C7) ─────────────────────────────────────────────

    #[test]
    fn scroll_accumulator_carries_fractions() {
        // Four +0.3 deltas: nothing, nothing, nothing, then one line (1.2 total).
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.add(0.3), 0);
        assert_eq!(a.add(0.3), 0);
        assert_eq!(a.add(0.3), 0);
        assert_eq!(a.add(0.3), 1);
        // The remainder (0.2) is kept: +0.8 more completes the next line.
        assert_eq!(a.add(0.8), 1);
    }

    #[test]
    fn scroll_accumulator_is_sign_symmetric() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.add(-0.6), 0);
        assert_eq!(a.add(-0.6), -1);
        // Remainder is ~-0.2; a +0.2 delta cancels it (no phantom line).
        assert_eq!(a.add(0.2), 0);
        assert_eq!(a.add(1.5), 1);
    }

    #[test]
    fn scroll_accumulator_emits_whole_lines_immediately() {
        let mut a = ScrollAccumulator::new();
        // A fast flick (3 notches × 3 lines) is emitted at once, undamped.
        assert_eq!(a.add(9.0), 9);
        assert_eq!(a.add(-3.0), -3);
    }

    #[test]
    fn scroll_accumulator_reset_drops_remainder() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.add(0.9), 0);
        a.reset();
        assert_eq!(a.add(0.2), 0, "remainder must not survive a reset");
    }

    #[test]
    fn scroll_accumulator_ignores_non_finite() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.add(f32::NAN), 0);
        assert_eq!(a.add(f32::INFINITY), 0);
        assert_eq!(a.add(1.0), 1, "accumulator must stay usable after NaN");
    }

    // ── Clamped cell coordinates for mouse reports (C8) ─────────────────────

    #[test]
    fn cell_at_clamped_interior() {
        // 10px cells: (25, 35) → col 3, row 4 (1-based).
        assert_eq!(cell_at_clamped(25.0, 35.0, 10.0, 10.0, 80, 24), (3, 4));
    }

    #[test]
    fn cell_at_clamped_gutter_clamps_to_last_column() {
        // A click in the scrollbar gutter right of the last column must clamp
        // to the last column, never report cols+1.
        assert_eq!(cell_at_clamped(805.0, 35.0, 10.0, 10.0, 80, 24), (80, 4));
    }

    #[test]
    fn cell_at_clamped_status_strip_clamps_to_last_row() {
        assert_eq!(cell_at_clamped(25.0, 500.0, 10.0, 10.0, 80, 24), (3, 24));
    }

    #[test]
    fn cell_at_clamped_negative_clamps_to_one() {
        assert_eq!(cell_at_clamped(-5.0, -5.0, 10.0, 10.0, 80, 24), (1, 1));
    }

    // ── 0-based selection cell + sub-cell side (shared by all windows) ──────

    #[test]
    fn cell_at_0_side_interior_and_halves() {
        // 10×20 cells, 80×24 grid: y=30 → line 1, x≈25 → col 2 (0-based).
        // Exactly the half-width boundary counts as the RIGHT half.
        assert_eq!(cell_at_0_side(25.0, 30.0, 10.0, 20.0, 80, 24), (1, 2, false));
        assert_eq!(cell_at_0_side(24.9, 30.0, 10.0, 20.0, 80, 24), (1, 2, true));
        assert_eq!(cell_at_0_side(25.1, 30.0, 10.0, 20.0, 80, 24), (1, 2, false));
        assert_eq!(cell_at_0_side(21.0, 30.0, 10.0, 20.0, 80, 24), (1, 2, true));
    }

    #[test]
    fn cell_at_0_side_clamps_to_grid() {
        // Negative coordinates clamp to the first cell.
        let (line, col, _) = cell_at_0_side(-5.0, -5.0, 10.0, 20.0, 80, 24);
        assert_eq!((line, col), (0, 0));
        // Beyond the last cell clamps to the last line/column.
        let (line, col, _) = cell_at_0_side(9000.0, 9000.0, 10.0, 20.0, 80, 24);
        assert_eq!((line, col), (23, 79));
        // Degenerate 0×0 grid must not panic (saturating_sub path).
        let (line, col, _) = cell_at_0_side(25.0, 30.0, 10.0, 20.0, 0, 0);
        assert_eq!((line, col), (0, 0));
    }

    #[test]
    fn cell_at_0_side_paddings_are_the_edge_cells_outer_halves() {
        // The left padding (x < 0 grid-relative) is ALWAYS column 0's left half
        // — before, a pointer a few px left of the grid read as its RIGHT half.
        for x in [-0.5, -3.0, -6.0, -9.9, -500.0] {
            assert_eq!(cell_at_0_side(x, 30.0, 10.0, 20.0, 80, 24), (1, 0, true), "x={x}");
        }
        // Past the last column (right padding / scrollbar gutter) is ALWAYS
        // its right half — before, it depended on the sub-pixel spot.
        for x in [800.0, 801.0, 803.0, 806.0, 818.0, 5000.0] {
            assert_eq!(cell_at_0_side(x, 30.0, 10.0, 20.0, 80, 24), (1, 79, false), "x={x}");
        }
        // The edge cells' own halves are unchanged.
        assert_eq!(cell_at_0_side(0.0, 30.0, 10.0, 20.0, 80, 24), (1, 0, true));
        assert_eq!(cell_at_0_side(799.9, 30.0, 10.0, 20.0, 80, 24), (1, 79, false));
    }

    // ── Dead-key composed text override (C3) ────────────────────────────────

    #[test]
    fn dead_key_override_prefers_composed_text() {
        // ' then e on US-International: logical "e", text "é" → send "é".
        let out = dead_key_text_override(false, false, &make_logical_char("e"), Some("é"));
        assert_eq!(out, Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn dead_key_override_noop_when_text_matches_logical() {
        assert_eq!(
            dead_key_text_override(false, false, &make_logical_char("e"), Some("e")),
            None,
        );
    }

    #[test]
    fn dead_key_override_ignores_ctrl_alt_named_and_control_text() {
        // Ctrl/Alt held → None (control-byte / Meta-ESC paths must win).
        assert_eq!(dead_key_text_override(true, false, &make_logical_char("e"), Some("é")), None);
        assert_eq!(dead_key_text_override(false, true, &make_logical_char("e"), Some("é")), None);
        // Named keys (Enter yields text "\r") → None.
        assert_eq!(
            dead_key_text_override(false, false, &Key::Named(NamedKey::Enter), Some("\r")),
            None,
        );
        // Control chars in text → None.
        assert_eq!(dead_key_text_override(false, false, &make_logical_char("e"), Some("\x08")), None);
        // Empty / missing text → None.
        assert_eq!(dead_key_text_override(false, false, &make_logical_char("e"), Some("")), None);
        assert_eq!(dead_key_text_override(false, false, &make_logical_char("e"), None), None);
    }

    #[test]
    fn dead_key_then_space_sends_the_accent_itself() {
        // xkb compose without an input method (Wayland): the Space that ends a
        // dead-key sequence is still the named key, its text the composed
        // character — ' Space → "'" on US-International, ^ Space → "^" on
        // German, ~ Space → "~" on ABNT2. A plain " " went out instead.
        let space = Key::Named(NamedKey::Space);
        for accent in ["'", "^", "~", "`", "\"", "´", "¨"] {
            assert_eq!(
                dead_key_text_override(false, false, &space, Some(accent)),
                Some(accent.as_bytes().to_vec()),
                "{accent}"
            );
        }
        // Compose Space Space → NO-BREAK SPACE.
        assert_eq!(
            dead_key_text_override(false, false, &space, Some("\u{a0}")),
            Some("\u{a0}".as_bytes().to_vec())
        );
        // A plain Space keeps going through the key encoder.
        assert_eq!(dead_key_text_override(false, false, &space, Some(" ")), None);
        assert_eq!(dead_key_text_override(false, false, &space, None), None);
        // Ctrl+Space (NUL) and Alt+Space (Meta) keep their encodings.
        assert_eq!(dead_key_text_override(true, false, &space, Some("'")), None);
        assert_eq!(dead_key_text_override(false, true, &space, Some("'")), None);
    }

    #[test]
    fn dead_key_then_space_types_the_accent_at_a_shell() {
        let space = Key::Named(NamedKey::Space);
        let none = KeyMods::default();
        let ev = KeyInput::press(make_physical(KeyCode::Space), &space, none);
        // Legacy encoding (bash/zsh at a prompt): the accent, not a space.
        let accent = KeyInput { text: Some("~"), ..ev };
        let legacy = KeyModes::default();
        let opts = KeyOptions::default();
        let km = crate::keymap::KeyMap::defaults();
        assert_eq!(decide_key_event(&km, &accent, &legacy, &opts, false), KeyAction::Send(b"~".to_vec()));
        // The kitty protocol already sent the text; it still does.
        let kitty = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        assert_eq!(decide_key_event(&km, &accent, &kitty, &opts, false), KeyAction::Send(b"~".to_vec()));
        // A plain Space is a space in both.
        let plain = KeyInput { text: Some(" "), ..ev };
        assert_eq!(decide_key_event(&km, &plain, &legacy, &opts, false), KeyAction::Send(b" ".to_vec()));
        assert_eq!(decide_key_event(&km, &plain, &kitty, &opts, false), KeyAction::Send(b" ".to_vec()));
    }

    #[test]
    fn sgr_left_press_release() {
        assert_eq!(encode_sgr_mouse(MouseEvent::LeftPress, 5, 3), b"\x1b[<0;5;3M");
        assert_eq!(encode_sgr_mouse(MouseEvent::LeftRelease, 5, 3), b"\x1b[<0;5;3m");
    }

    #[test]
    fn sgr_wheel_buttons() {
        assert_eq!(encode_sgr_mouse(MouseEvent::WheelUp, 1, 1), b"\x1b[<64;1;1M");
        assert_eq!(encode_sgr_mouse(MouseEvent::WheelDown, 10, 20), b"\x1b[<65;10;20M");
    }

    #[test]
    fn sgr_coords_clamped_to_one() {
        // 0-based callers that forgot to add 1 still get a valid 1-based report.
        assert_eq!(encode_sgr_mouse(MouseEvent::LeftPress, 0, 0), b"\x1b[<0;1;1M");
    }

    #[test]
    fn x10_left_press_release() {
        // Press: \e[M then 32+button, 32+col, 32+row.
        assert_eq!(
            encode_x10_mouse(MouseEvent::LeftPress, 5, 3),
            vec![0x1b, b'[', b'M', 32, 32 + 5, 32 + 3],
        );
        // Release: legacy X10 encodes any release as button 3.
        assert_eq!(
            encode_x10_mouse(MouseEvent::LeftRelease, 5, 3),
            vec![0x1b, b'[', b'M', 32 + 3, 32 + 5, 32 + 3],
        );
    }

    #[test]
    fn x10_wheel_buttons() {
        assert_eq!(
            encode_x10_mouse(MouseEvent::WheelUp, 1, 1),
            vec![0x1b, b'[', b'M', 32u8.wrapping_add(64), 33, 33],
        );
        assert_eq!(
            encode_x10_mouse(MouseEvent::WheelDown, 1, 1),
            vec![0x1b, b'[', b'M', 32u8.wrapping_add(65), 33, 33],
        );
    }

    #[test]
    fn x10_coords_clamped_to_one_and_223() {
        // 0-based callers still get a valid 1-based report (min clamp to 1).
        assert_eq!(
            encode_x10_mouse(MouseEvent::LeftPress, 0, 0),
            vec![0x1b, b'[', b'M', 32, 33, 33],
        );
        // Coordinates above 223 saturate so 32+coord never exceeds 255.
        assert_eq!(
            encode_x10_mouse(MouseEvent::LeftPress, 500, 999),
            vec![0x1b, b'[', b'M', 32, 255, 255],
        );
    }

    #[test]
    fn encode_mouse_dispatches_on_sgr_flag() {
        // sgr=true → SGR encoding; sgr=false → X10 encoding.
        assert_eq!(
            encode_mouse(MouseEvent::LeftPress, 5, 3, true),
            encode_sgr_mouse(MouseEvent::LeftPress, 5, 3),
        );
        assert_eq!(
            encode_mouse(MouseEvent::LeftPress, 5, 3, false),
            encode_x10_mouse(MouseEvent::LeftPress, 5, 3),
        );
    }

    // ── Settings panel hit-tests ─────────────────────────────────────────────
    // Real panels built from the real control table (`settings_ui`) through
    // `build_panel`, so the geometry is exactly what the Settings window draws.

    fn settings_view(tab: usize, scroll: f32) -> jetty_render::PanelView {
        use crate::settings_ui::{tab_items, Ctx};
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let mono: Vec<String> = ["JetBrains Mono", "Fira Code", "Hack", "Cascadia Code", "Iosevka", "Monaspace"]
            .map(String::from)
            .to_vec();
        let ui: Vec<String> = ["System Sans (default)", "Inter", "Noto Sans", "DejaVu Sans", "Cantarell"]
            .map(String::from)
            .to_vec();
        // Dropdown mode: the dropdown sliders are live.
        let cfg = crate::config::Config { window_mode: "dropdown".into(), ..Default::default() };
        let ctx = Ctx { mono_families: &mono, ui_families: &ui, ..Ctx::empty() };
        let items = tab_items(tab, &cfg, &ctx);
        let mut inp = jetty_render::PanelInput::new(420, 592, &theme, jetty_render::ChromeMetrics::DEFAULT, &items);
        inp.active_tab = tab;
        inp.scroll = scroll;
        jetty_render::build_panel(&inp, &mut jetty_render::MonoMeasure(9.8))
    }

    /// Decode a click at the center of `rect` against the panel geometry.
    fn click_rect(g: &jetty_render::PanelGeom, rect: &jetty_render::Rect) -> MouseAction {
        decide_mouse_press(Some(g), None, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
    }

    #[test]
    fn every_visible_control_part_decodes_to_its_ctl_action() {
        use jetty_render::PanelHit;
        for tab in 0..jetty_render::N_TABS {
            let pv = settings_view(tab, 0.0);
            let g = &pv.geom;
            let mut seen = 0;
            for (r, hit) in &g.hits {
                // Only parts fully inside the viewport are clickable.
                if r.y < g.content_top || r.y + r.h > g.content_bottom {
                    continue;
                }
                let want = match *hit {
                    PanelHit::Ctl { id, part } => MouseAction::Ctl { id, part },
                    PanelHit::Section(id) => MouseAction::SettingsSection(id),
                    PanelHit::GalleryCard(i) => MouseAction::GalleryCard(i),
                    PanelHit::GalleryFilter(f) => MouseAction::GalleryFilter(f),
                    other => panic!("unexpected content hit {other:?}"),
                };
                // A section header's hit rect is overlapped by its master switch
                // on the right — click its left part.
                let act = if matches!(hit, PanelHit::Section(_)) {
                    decide_mouse_press(Some(g), None, r.x + 12.0, r.y + r.h / 2.0)
                } else {
                    click_rect(g, r)
                };
                assert_eq!(act, want, "tab {tab}");
                seen += 1;
            }
            assert!(seen > 0, "tab {tab} has visible parts");
        }
    }

    #[test]
    fn named_controls_decode_on_their_tabs() {
        use jetty_render::{CtlPart, PanelHit};
        for (tab, id, part) in [
            (0, "opacity", CtlPart::Track),
            (0, "corner_radius", CtlPart::Track),
            (1, "font_size", CtlPart::Minus),
            (1, "font_size", CtlPart::Reset),
            (1, "font_family", CtlPart::Row(1)),
            (1, "font_family", CtlPart::ScrollDown),
            (2, "summon_effect", CtlPart::Next),
            (2, "window_mode", CtlPart::Prev),
            (2, "focus_autohide", CtlPart::Switch),
            (2, "dropdown_height_pct", CtlPart::Track),
            (3, "shell", CtlPart::Next),
            (3, "launch_at_login", CtlPart::Switch),
            (3, "notify_on_command_finish", CtlPart::Switch),
            (3, "notify_min_seconds", CtlPart::Prev),
            (4, "effects.crt_enabled", CtlPart::Switch),
            (4, "effects.crt_curvature", CtlPart::Track),
            (4, "effects.crt_animate", CtlPart::Chip(2)),
            (4, "effects.preset", CtlPart::Chip(3)),
            (4, "effects.crt_phosphor", CtlPart::Next),
            (4, "effects.glitch", CtlPart::Chip(1)),
            (0, "tab_style", CtlPart::Next),
            (0, "follow_system_theme", CtlPart::Switch),
            (0, "minimum_contrast", CtlPart::Track),
            (1, "line_height", CtlPart::Track),
            (2, "scrollbar", CtlPart::Prev),
            (2, "padding_x", CtlPart::Track),
            (0, "look", CtlPart::Chip(2)),
            (2, "reduce_motion", CtlPart::Next),
            (4, "cursor.shape", CtlPart::Next),
            (4, "cursor.trail", CtlPart::Switch),
            (4, "visual_bell", CtlPart::Prev),
            (4, "command_pulse", CtlPart::Next),
        ] {
            // Scroll the control's row to the top of the viewport first (a
            // section's master switch sits in its header: no row of its own).
            let top = settings_view(tab, 0.0).geom.anchor(id).map_or(0.0, |a| a.0);
            let pv = settings_view(tab, top);
            let g = &pv.geom;
            let r = g
                .rect_of(PanelHit::Ctl { id, part })
                .unwrap_or_else(|| panic!("tab {tab}: no {id} {part:?}"));
            assert_eq!(click_rect(g, &r), MouseAction::Ctl { id, part }, "tab {tab} {id}");
        }
    }

    #[test]
    fn scrolled_out_controls_are_dead_and_scrolled_in_ones_live() {
        use jetty_render::{CtlPart, PanelHit};
        // The caret color sits below the fold on the Effects tab.
        let hit = PanelHit::Ctl { id: "effects.caret_flash_color", part: CtlPart::Channel(0) };
        let want = MouseAction::Ctl { id: "effects.caret_flash_color", part: CtlPart::Channel(0) };
        let top = settings_view(4, 0.0);
        let r = top.geom.rect_of(hit).expect("laid out even when below the fold");
        assert!(r.y >= top.geom.content_bottom, "below the fold at scroll 0");
        assert_ne!(click_rect(&top.geom, &r), want);
        let bottom = settings_view(4, 1.0e9);
        let r = bottom.geom.rect_of(hit).unwrap();
        assert_eq!(click_rect(&bottom.geom, &r), want);
        // Scrolled to the bottom, the first section header is under the chrome:
        // a click there must not reach it.
        let hdr = bottom.geom.rect_of(PanelHit::Section("fx.crt")).unwrap();
        assert!(hdr.y + hdr.h < bottom.geom.content_top);
    }

    #[test]
    fn tab_strip_clicks_select_tab() {
        let pv = settings_view(0, 0.0);
        let g = &pv.geom;
        for i in 0..jetty_render::N_TABS {
            assert_eq!(click_rect(g, &g.tab_rects[i]), MouseAction::SetSettingsTab(i));
        }
    }

    #[test]
    fn scrollbar_thumb_and_track_and_footer_decode() {
        let pv = settings_view(4, 0.0);
        let g = &pv.geom;
        let thumb = g.scroll_thumb.expect("the Effects tab overflows");
        assert_eq!(
            decide_mouse_press(Some(g), None, thumb.x + thumb.w / 2.0, thumb.y + 5.0),
            MouseAction::PanelScrollThumb { grab_dy: 5.0 }
        );
        assert_eq!(
            decide_mouse_press(Some(g), None, thumb.x + thumb.w / 2.0, g.content_bottom - 2.0),
            MouseAction::PanelScrollTrack
        );
        let reset = g.rect_of(jetty_render::PanelHit::ResetTab).expect("footer button");
        assert_eq!(click_rect(g, &reset), MouseAction::ResetTab);
        // The title row is the drag handle.
        assert_eq!(click_rect(g, &g.title_bar), MouseAction::StartDialogDrag);
    }

    // ── Golden table: legacy encoding vs terminfo `xterm-256color` ───────────

    /// `infocmp -x xterm-256color` (ncurses 6.5), the key capabilities only.
    /// The keypad-transmit (smkx) strings are what applications expect once
    /// DECCKM is on; `kent`/`ka1`/… (application keypad) are deliberately absent:
    /// keypad keys stay numeric (see `KeyModes::app_keypad`).
    const XTERM_256COLOR_KEYS: &str = r"kDC=\E[3;2~ kEND=\E[1;2F kHOM=\E[1;2H kIC=\E[2;2~ kLFT=\E[1;2D
        kNXT=\E[6;2~ kPRV=\E[5;2~ kRIT=\E[1;2C kbs=^? kcbt=\E[Z kcub1=\EOD kcud1=\EOB kcuf1=\EOC
        kcuu1=\EOA kdch1=\E[3~ kend=\EOF kf1=\EOP kf10=\E[21~ kf11=\E[23~ kf12=\E[24~
        kf13=\E[1;2P kf14=\E[1;2Q kf15=\E[1;2R kf16=\E[1;2S kf17=\E[15;2~ kf18=\E[17;2~
        kf19=\E[18;2~ kf2=\EOQ kf20=\E[19;2~ kf21=\E[20;2~ kf22=\E[21;2~ kf23=\E[23;2~
        kf24=\E[24;2~ kf25=\E[1;5P kf26=\E[1;5Q kf27=\E[1;5R kf28=\E[1;5S kf29=\E[15;5~
        kf3=\EOR kf30=\E[17;5~ kf31=\E[18;5~ kf32=\E[19;5~ kf33=\E[20;5~ kf34=\E[21;5~
        kf35=\E[23;5~ kf36=\E[24;5~ kf37=\E[1;6P kf38=\E[1;6Q kf39=\E[1;6R kf4=\EOS
        kf40=\E[1;6S kf41=\E[15;6~ kf42=\E[17;6~ kf43=\E[18;6~ kf44=\E[19;6~ kf45=\E[20;6~
        kf46=\E[21;6~ kf47=\E[23;6~ kf48=\E[24;6~ kf49=\E[1;3P kf5=\E[15~ kf50=\E[1;3Q
        kf51=\E[1;3R kf52=\E[1;3S kf53=\E[15;3~ kf54=\E[17;3~ kf55=\E[18;3~ kf56=\E[19;3~
        kf57=\E[20;3~ kf58=\E[21;3~ kf59=\E[23;3~ kf6=\E[17~ kf60=\E[24;3~ kf61=\E[1;4P
        kf62=\E[1;4Q kf63=\E[1;4R kf7=\E[18~ kf8=\E[19~ kf9=\E[20~ khome=\EOH kich1=\E[2~
        knp=\E[6~ kpp=\E[5~ kDC3=\E[3;3~ kDC4=\E[3;4~ kDC5=\E[3;5~ kDC6=\E[3;6~ kDC7=\E[3;7~
        kDN=\E[1;2B kDN3=\E[1;3B kDN4=\E[1;4B kDN5=\E[1;5B kDN6=\E[1;6B kDN7=\E[1;7B
        kEND3=\E[1;3F kEND4=\E[1;4F kEND5=\E[1;5F kEND6=\E[1;6F kEND7=\E[1;7F
        kHOM3=\E[1;3H kHOM4=\E[1;4H kHOM5=\E[1;5H kHOM6=\E[1;6H kHOM7=\E[1;7H
        kIC3=\E[2;3~ kIC4=\E[2;4~ kIC5=\E[2;5~ kIC6=\E[2;6~ kIC7=\E[2;7~
        kLFT3=\E[1;3D kLFT4=\E[1;4D kLFT5=\E[1;5D kLFT6=\E[1;6D kLFT7=\E[1;7D
        kNXT3=\E[6;3~ kNXT4=\E[6;4~ kNXT5=\E[6;5~ kNXT6=\E[6;6~ kNXT7=\E[6;7~
        kPRV3=\E[5;3~ kPRV4=\E[5;4~ kPRV5=\E[5;5~ kPRV6=\E[5;6~ kPRV7=\E[5;7~
        kRIT3=\E[1;3C kRIT4=\E[1;4C kRIT5=\E[1;5C kRIT6=\E[1;6C kRIT7=\E[1;7C
        kUP=\E[1;2A kUP3=\E[1;3A kUP4=\E[1;4A kUP5=\E[1;5A kUP6=\E[1;6A kUP7=\E[1;7A";

    /// Decode a terminfo string value (`\E`, `^?`, `^X`, `\\`, octal `\NNN`).
    fn terminfo_bytes(v: &str) -> Vec<u8> {
        let b = v.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' if i + 1 < b.len() => {
                    let c = b[i + 1];
                    if c == b'E' || c == b'e' {
                        out.push(0x1b);
                        i += 2;
                    } else if c.is_ascii_digit() && i + 4 <= b.len() {
                        out.push(u8::from_str_radix(&v[i + 1..i + 4], 8).unwrap_or(b'?'));
                        i += 4;
                    } else {
                        out.push(c);
                        i += 2;
                    }
                }
                b'^' if i + 1 < b.len() => {
                    let c = b[i + 1];
                    out.push(if c == b'?' { 0x7f } else { c & 0x1f });
                    i += 2;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    /// Map a terminfo key capability to the key event it describes:
    /// `(key, shift, alt, ctrl, decckm)`. `None` for capabilities we don't model.
    fn terminfo_key(cap: &str) -> Option<(Key, bool, bool, bool, bool)> {
        let named = |n: NamedKey| Key::Named(n);
        // Modifier number m (2..=8) → shift/alt/ctrl bits of m-1.
        let mods = |m: u8| {
            let b = m - 1;
            (b & 1 != 0, b & 2 != 0, b & 4 != 0)
        };
        let unmod = |k: Key, decckm: bool| Some((k, false, false, false, decckm));
        match cap {
            "kcuu1" => return unmod(named(NamedKey::ArrowUp), true),
            "kcud1" => return unmod(named(NamedKey::ArrowDown), true),
            "kcuf1" => return unmod(named(NamedKey::ArrowRight), true),
            "kcub1" => return unmod(named(NamedKey::ArrowLeft), true),
            "khome" => return unmod(named(NamedKey::Home), true),
            "kend" => return unmod(named(NamedKey::End), true),
            "kich1" => return unmod(named(NamedKey::Insert), false),
            "kdch1" => return unmod(named(NamedKey::Delete), false),
            "kpp" => return unmod(named(NamedKey::PageUp), false),
            "knp" => return unmod(named(NamedKey::PageDown), false),
            "kbs" => return unmod(named(NamedKey::Backspace), false),
            "kcbt" => return Some((named(NamedKey::Tab), true, false, false, false)),
            _ => {}
        }
        const F: [NamedKey; 12] = [
            NamedKey::F1, NamedKey::F2, NamedKey::F3, NamedKey::F4, NamedKey::F5, NamedKey::F6,
            NamedKey::F7, NamedKey::F8, NamedKey::F9, NamedKey::F10, NamedKey::F11, NamedKey::F12,
        ];
        if let Some(n) = cap.strip_prefix("kf").and_then(|s| s.parse::<usize>().ok()) {
            // kf1-12 plain; then blocks of 12: Shift, Ctrl, Ctrl+Shift, Alt, Shift+Alt.
            let (block, idx) = ((n - 1) / 12, (n - 1) % 12);
            let m = [1u8, 2, 5, 6, 3, 4].get(block).copied()?;
            let (s, a, c) = if m == 1 { (false, false, false) } else { mods(m) };
            return Some((named(F[idx]), s, a, c, false));
        }
        for (prefix, key) in [
            ("kUP", NamedKey::ArrowUp),
            ("kDN", NamedKey::ArrowDown),
            ("kRIT", NamedKey::ArrowRight),
            ("kLFT", NamedKey::ArrowLeft),
            ("kHOM", NamedKey::Home),
            ("kEND", NamedKey::End),
            ("kIC", NamedKey::Insert),
            ("kDC", NamedKey::Delete),
            ("kNXT", NamedKey::PageDown),
            ("kPRV", NamedKey::PageUp),
        ] {
            if let Some(rest) = cap.strip_prefix(prefix) {
                let m = if rest.is_empty() { 2 } else { rest.parse::<u8>().ok()? };
                let (s, a, c) = mods(m);
                return Some((named(key), s, a, c, false));
            }
        }
        None
    }

    fn check_terminfo_caps(caps: &[(String, Vec<u8>)]) -> usize {
        let mut checked = 0;
        for (cap, want) in caps {
            let Some((key, shift, alt, ctrl, decckm)) = terminfo_key(cap) else { continue };
            // Unmodified keys must match in the mode the cap describes; modified
            // forms are mode-independent, so check them in BOTH modes.
            let modes_to_check: &[bool] = if !shift && !alt && !ctrl && decckm { &[true] } else { &[false, true] };
            for &app_cursor in modes_to_check {
                let modes = KeyModes { app_cursor, ..KeyModes::default() };
                let got = encode_legacy(ctrl, shift, alt, make_physical(KeyCode::F35), &key, &modes);
                assert_eq!(
                    got.as_deref(),
                    Some(want.as_slice()),
                    "{cap} (shift={shift} alt={alt} ctrl={ctrl} DECCKM={app_cursor})"
                );
                checked += 1;
            }
        }
        checked
    }

    #[test]
    fn legacy_encoding_matches_xterm_256color_terminfo() {
        let caps: Vec<(String, Vec<u8>)> = XTERM_256COLOR_KEYS
            .split_whitespace()
            .filter_map(|e| e.split_once('='))
            .map(|(k, v)| (k.to_string(), terminfo_bytes(v)))
            .collect();
        assert!(caps.len() > 120, "snapshot parsed: {}", caps.len());
        assert!(check_terminfo_caps(&caps) > 200);
    }

    #[test]
    fn legacy_encoding_matches_the_live_terminfo_database_when_present() {
        // Cross-check against THIS machine's ncurses database (skipped when
        // `infocmp` or the entry is unavailable, e.g. a minimal CI image).
        let Ok(out) = std::process::Command::new("infocmp").args(["-x", "xterm-256color"]).output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let caps: Vec<(String, Vec<u8>)> = text
            .split(',')
            .map(str::trim)
            .filter_map(|e| e.split_once('='))
            .filter(|(k, _)| k.starts_with('k'))
            .map(|(k, v)| (k.to_string(), terminfo_bytes(v)))
            .collect();
        let checked = check_terminfo_caps(&caps);
        assert!(checked > 100, "only {checked} terminfo key capabilities were checked");
    }

    #[test]
    fn unmodified_cursor_keys_follow_decckm_in_both_modes() {
        // THE Home/End fix: under DECCKM (oh-my-zsh's `smkx` sets it at every
        // prompt) Home/End must be SS3 — the strings oh-my-zsh binds to
        // beginning/end-of-line ($terminfo[khome]/[kend]). Normal mode: CSI.
        for (named, normal, app) in [
            (NamedKey::Home, &b"\x1b[H"[..], &b"\x1bOH"[..]),
            (NamedKey::End, b"\x1b[F", b"\x1bOF"),
            (NamedKey::ArrowUp, b"\x1b[A", b"\x1bOA"),
            (NamedKey::ArrowLeft, b"\x1b[D", b"\x1bOD"),
        ] {
            let key = Key::Named(named);
            assert_eq!(dk(false, false, false, make_physical(KeyCode::Home), &key, false, false, false), KeyAction::Send(normal.to_vec()));
            assert_eq!(dk(false, false, false, make_physical(KeyCode::Home), &key, false, true, false), KeyAction::Send(app.to_vec()));
            // Modified forms are DECCKM-independent: Alt+Home = CSI 1;3H.
            let alt = format!("\x1b[1;3{}", char::from(app[2])).into_bytes();
            assert_eq!(dk(false, false, true, make_physical(KeyCode::Home), &key, false, true, false), KeyAction::Send(alt), "Alt+{named:?}");
        }
    }

    // ── keypad: numeric in both keypad modes (NumLock override) ──────────────

    fn kev<'a>(physical: KeyCode, logical: &'a Key, text: Option<&'a str>, mods: KeyMods) -> KeyInput<'a> {
        KeyInput {
            physical: PhysicalKey::Code(physical),
            logical,
            key_without_modifiers: logical,
            text,
            location: KeyLocation::Standard,
            kind: KeyEventKind::Press,
            mods,
        }
    }

    fn decide(ev: &KeyInput<'_>, modes: KeyModes, opts: KeyOptions) -> KeyAction {
        decide_key_event(&crate::keymap::KeyMap::defaults(), ev, &modes, &opts, false)
    }

    #[test]
    fn keypad_stays_numeric_in_application_keypad_mode() {
        let smkx = KeyModes { app_cursor: true, app_keypad: true, ..KeyModes::default() };
        let one = Key::Character("1".into());
        let plus = Key::Character("+".into());
        let enter = Key::Named(NamedKey::Enter);
        let home = Key::Named(NamedKey::Home);
        for modes in [KeyModes::default(), smkx] {
            assert_eq!(decide(&kev(KeyCode::Numpad1, &one, Some("1"), KeyMods::default()), modes, KeyOptions::default()), KeyAction::Send(b"1".to_vec()));
            assert_eq!(decide(&kev(KeyCode::NumpadAdd, &plus, Some("+"), KeyMods::default()), modes, KeyOptions::default()), KeyAction::Send(b"+".to_vec()));
            assert_eq!(decide(&kev(KeyCode::NumpadEnter, &enter, Some("\r"), KeyMods::default()), modes, KeyOptions::default()), KeyAction::Send(b"\r".to_vec()));
        }
        // NumLock off: keypad 7 is Home and follows DECCKM like the main key.
        assert_eq!(decide(&kev(KeyCode::Numpad7, &home, None, KeyMods::default()), smkx, KeyOptions::default()), KeyAction::Send(b"\x1bOH".to_vec()));
        assert_eq!(decide(&kev(KeyCode::Numpad7, &home, None, KeyMods::default()), KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"\x1b[H".to_vec()));
    }

    #[test]
    fn numlock_off_keypad_5_is_xterms_begin_key() {
        // xkb's KP_Begin, which winit leaves Unidentified: it sent nothing.
        let begin = Key::Unidentified(winit::keyboard::NativeKey::Xkb(0xff9d));
        let smkx = KeyModes { app_cursor: true, ..KeyModes::default() };
        let ev = kev(KeyCode::Numpad5, &begin, None, KeyMods::default());
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"\x1b[E".to_vec()));
        assert_eq!(decide(&ev, smkx, KeyOptions::default()), KeyAction::Send(b"\x1bOE".to_vec()));
        let shifted = kev(KeyCode::Numpad5, &begin, None, KeyMods { shift: true, ..KeyMods::default() });
        assert_eq!(decide(&shifted, smkx, KeyOptions::default()), KeyAction::Send(b"\x1b[1;2E".to_vec()));
        // NumLock on: the digit.
        let five = Key::Character("5".into());
        assert_eq!(decide(&kev(KeyCode::Numpad5, &five, Some("5"), KeyMods::default()), KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"5".to_vec()));
        // An unnamed key elsewhere still sends nothing.
        let other = kev(KeyCode::Numpad6, &begin, None, KeyMods::default());
        assert_eq!(decide(&other, KeyModes::default(), KeyOptions::default()), KeyAction::None);
    }

    #[test]
    fn f13_to_f18_the_layout_left_unnamed_are_those_f_keys() {
        // xkb's evdev rules call F13 `XF86Tools` and F14–F18 `XF86Launch5`…`9`,
        // which winit doesn't name: an Apple keyboard's F13–F18 or a keyd / QMK
        // remap to them sent nothing in either protocol.
        let tools = Key::Unidentified(winit::keyboard::NativeKey::Xkb(0x1008_ff81));
        let ev = kev(KeyCode::F13, &tools, None, KeyMods::default());
        let kitty = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"\x1b[25~".to_vec()));
        assert_eq!(decide(&ev, kitty, KeyOptions::default()), KeyAction::Send(b"\x1b[57376u".to_vec()));
        let ctrl = kev(KeyCode::F18, &tools, None, KeyMods { ctrl: true, ..KeyMods::default() });
        assert_eq!(decide(&ctrl, KeyModes::default(), KeyOptions::default()), KeyAction::Send(b"\x1b[32;5~".to_vec()));
        // F20–F23 are laptop keys by udev convention (mic mute, touchpad
        // toggle / on / off): left unnamed they stay silent.
        let touchpad = kev(KeyCode::F21, &tools, None, KeyMods::default());
        assert_eq!(decide(&touchpad, KeyModes::default(), KeyOptions::default()), KeyAction::None);
        assert_eq!(decide(&touchpad, kitty, KeyOptions::default()), KeyAction::None);
    }

    // ── layouts: chords follow the key LABEL ──────────────────────────────────

    fn ctrl_shift() -> KeyMods {
        KeyMods { ctrl: true, shift: true, ..KeyMods::default() }
    }

    #[test]
    fn chords_follow_labels_on_dvorak_azerty_and_turkish_f() {
        let c = |label: &str, at: KeyCode| {
            let key = Key::Character(label.into());
            decide(&kev(at, &key, None, ctrl_shift()), KeyModes::default(), KeyOptions::default())
        };
        // Dvorak: the key LABELED C sits at physical KeyI → Copy, never SIGINT…
        assert_eq!(c("C", KeyCode::KeyI), KeyAction::Copy);
        // …and physical KeyC (labeled J there) is Ctrl+J = LF, not Copy.
        assert_eq!(c("J", KeyCode::KeyC), KeyAction::Send(vec![0x0a]));
        // Turkish-F: C at KeyV, V at KeyC, T at KeyH, H at KeyO.
        assert_eq!(c("C", KeyCode::KeyV), KeyAction::Copy);
        assert_eq!(c("V", KeyCode::KeyC), KeyAction::Paste);
        assert_eq!(c("T", KeyCode::KeyH), KeyAction::NewTab);
        assert_eq!(c("H", KeyCode::KeyO), KeyAction::HintMode);
        // AZERTY: W at KeyZ, Z at KeyW.
        assert_eq!(c("W", KeyCode::KeyZ), KeyAction::CloseTab);
        assert_eq!(c("Z", KeyCode::KeyW), KeyAction::PrevPrompt);
        // Cyrillic (non-ASCII): the US position is the fallback → still Copy.
        assert_eq!(c("С", KeyCode::KeyC), KeyAction::Copy);
        // Unidentified physical + label → the label still decides.
        let key = Key::Character("T".into());
        let ev = KeyInput { physical: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified), ..kev(KeyCode::KeyT, &key, None, ctrl_shift()) };
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::NewTab);
    }

    #[test]
    fn a_shift_chord_on_a_symbol_fires_where_shift_changes_the_character() {
        // `copy_mode = "Ctrl+Shift+/"` on US: the event types '?', which used to
        // miss the chord and send DEL (Ctrl+?) — deleting a character.
        let b = crate::config::KeyBindings {
            copy_mode: Some(crate::config::ChordSpec::One("Ctrl+Shift+/".into())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        let (logical, base) = (Key::Character("?".into()), Key::Character("/".into()));
        let ev = KeyInput { key_without_modifiers: &base, ..kev(KeyCode::Slash, &logical, Some("?"), ctrl_shift()) };
        assert_eq!(decide_key_event(&km, &ev, &KeyModes::default(), &KeyOptions::default(), false), KeyAction::CopyMode);
        // Unbound (the default), Ctrl+Shift+/ still reaches the shell.
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::Send(vec![0x7f]));
    }

    #[test]
    fn turkish_q_dotless_i_falls_back_to_the_i_position() {
        // A user chord on I: Turkish-Q's I key types 'ı' unshifted (non-ASCII →
        // position fallback) and 'I' shifted (label match).
        let b = crate::config::KeyBindings {
            new_tab: Some(crate::config::ChordSpec::One("Ctrl+Alt+I".into())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        let dotless = Key::Character("ı".into());
        let mods = KeyMods { ctrl: true, alt: true, ..KeyMods::default() };
        let ev = kev(KeyCode::KeyI, &dotless, None, mods);
        assert_eq!(decide_key_event(&km, &ev, &KeyModes::default(), &KeyOptions::default(), false), KeyAction::NewTab);
    }

    // ── font / transparency chords on real layouts (xkb ground truth) ─────────

    #[test]
    fn font_and_transparency_chords_on_turkish_q_us_and_german() {
        let k = |ctrl: bool, shift: bool, alt: bool, at: KeyCode, label: &'static str| {
            dk(ctrl, shift, alt, make_physical(at), &make_logical_char(label), false, false, false)
        };
        // Turkish-Q (xkb `tr`: AE04 = 4 / plus, AE10 = 0 / equal, AE12 = minus /
        // underscore): Ctrl+'+' needs Shift there.
        assert_eq!(k(true, true, false, KeyCode::Digit4, "+"), KeyAction::FontUp);
        assert_eq!(k(true, true, false, KeyCode::Digit0, "="), KeyAction::FontUp);
        assert_eq!(k(true, false, false, KeyCode::Equal, "-"), KeyAction::FontDown);
        assert_eq!(k(true, true, false, KeyCode::Equal, "_"), KeyAction::Send(vec![0x1f]));
        assert_eq!(k(true, true, true, KeyCode::Digit4, "+"), KeyAction::OpacityUp);
        assert_eq!(k(true, false, true, KeyCode::Equal, "-"), KeyAction::OpacityDown);
        assert_eq!(k(true, true, true, KeyCode::Equal, "_"), KeyAction::OpacityDown);
        // Plain Ctrl+4 stays a tab jump; Ctrl+Shift+4 is not.
        assert_eq!(k(true, false, false, KeyCode::Digit4, "4"), KeyAction::SelectTab(3));
        // US.
        assert_eq!(k(true, false, false, KeyCode::Equal, "="), KeyAction::FontUp);
        assert_eq!(k(true, true, false, KeyCode::Equal, "+"), KeyAction::FontUp);
        assert_eq!(k(true, false, true, KeyCode::Equal, "="), KeyAction::OpacityUp);
        assert_eq!(k(true, false, true, KeyCode::Minus, "-"), KeyAction::OpacityDown);
        assert_eq!(k(true, true, false, KeyCode::Minus, "_"), KeyAction::Send(vec![0x1f]));
        // German QWERTZ: '+' unshifted at BracketRight, '=' is Shift+0, '-' at Slash.
        assert_eq!(k(true, false, false, KeyCode::BracketRight, "+"), KeyAction::FontUp);
        assert_eq!(k(true, true, false, KeyCode::Digit0, "="), KeyAction::FontUp);
        assert_eq!(k(true, false, false, KeyCode::Slash, "-"), KeyAction::FontDown);
        assert_eq!(k(true, true, false, KeyCode::Slash, "_"), KeyAction::Send(vec![0x1f]));
        // AltGr is not Alt (XKB Level3): AltGr+Q on Turkish-Q / German types '@'.
        assert_eq!(k(false, false, false, KeyCode::KeyQ, "@"), KeyAction::Send(b"@".to_vec()));
    }

    // ── macOS Option vs Meta; Linux Alt is always Meta ──────────────────────

    fn option(text: &'static str, logical: &'static str, base: &'static str, lalt: bool, ralt: bool) -> (Key, Key, &'static str, KeyMods) {
        (
            Key::Character(logical.into()),
            Key::Character(base.into()),
            text,
            KeyMods { alt: true, lalt, ralt, ..KeyMods::default() },
        )
    }

    fn mac(opt: OptionAsAlt) -> KeyOptions {
        KeyOptions { macos: true, option_as_alt: opt }
    }

    fn run(phys: KeyCode, parts: &(Key, Key, &str, KeyMods), opts: KeyOptions) -> KeyAction {
        let (logical, base, text, mods) = parts;
        let ev = KeyInput { key_without_modifiers: base, ..kev(phys, logical, Some(text), *mods) };
        decide(&ev, KeyModes::default(), opts)
    }

    #[test]
    fn macos_option_composes_characters_by_default() {
        // Turkish Mac: Option+Q = '@'; German Mac: Option+L = '@', Option+8 = '{',
        // Option+5/6 = '[' / ']', Option+7 = '|'. winit puts the composed char
        // in logical_key AND text; key_without_modifiers is the bare key.
        for (phys, out, base) in [
            (KeyCode::KeyQ, "@", "q"),
            (KeyCode::KeyL, "@", "l"),
            (KeyCode::Digit8, "{", "8"),
            (KeyCode::Digit5, "[", "5"),
            (KeyCode::Digit7, "|", "7"),
            (KeyCode::KeyG, "©", "g"),
        ] {
            let p = option(out, out, base, true, false);
            assert_eq!(run(phys, &p, mac(OptionAsAlt::None)), KeyAction::Send(out.as_bytes().to_vec()), "Option+{base}");
        }
    }

    #[test]
    fn macos_option_as_alt_sides() {
        // With winit's option_as_alt set to the same value, the Meta side
        // reports the plain key ('q'), which we ESC-prefix.
        let left_meta = option("q", "q", "q", true, false);
        assert_eq!(run(KeyCode::KeyQ, &left_meta, mac(OptionAsAlt::Left)), KeyAction::Send(b"\x1bq".to_vec()));
        assert_eq!(run(KeyCode::KeyQ, &left_meta, mac(OptionAsAlt::Both)), KeyAction::Send(b"\x1bq".to_vec()));
        // The other side keeps composing.
        let right_compose = option("@", "@", "q", false, true);
        assert_eq!(run(KeyCode::KeyQ, &right_compose, mac(OptionAsAlt::Left)), KeyAction::Send(b"@".to_vec()));
        let left_compose = option("@", "@", "q", true, false);
        assert_eq!(run(KeyCode::KeyQ, &left_compose, mac(OptionAsAlt::Right)), KeyAction::Send(b"@".to_vec()));
        // Option produced nothing new → Meta fallback (ESC + key).
        let same = option("b", "b", "b", true, false);
        assert_eq!(run(KeyCode::KeyB, &same, mac(OptionAsAlt::None)), KeyAction::Send(b"\x1bb".to_vec()));
        // Named keys without text: Option is a modifier (word-jump).
        let left = Key::Named(NamedKey::ArrowLeft);
        let ev = kev(KeyCode::ArrowLeft, &left, None, KeyMods { alt: true, lalt: true, ..KeyMods::default() });
        assert_eq!(decide(&ev, KeyModes::default(), mac(OptionAsAlt::None)), KeyAction::Send(b"\x1b[1;3D".to_vec()));
    }

    #[test]
    fn macos_alt_bindings_match_the_bare_key() {
        // A user `Alt+B` binding fires on Option+B even though Option composed '∫'.
        let b = crate::config::KeyBindings {
            new_tab: Some(crate::config::ChordSpec::One("Alt+B".into())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        let (logical, base) = (Key::Character("∫".into()), Key::Character("b".into()));
        let ev = KeyInput { key_without_modifiers: &base, ..kev(KeyCode::KeyB, &logical, Some("∫"), KeyMods { alt: true, lalt: true, ..KeyMods::default() }) };
        assert_eq!(decide_key_event(&km, &ev, &KeyModes::default(), &mac(OptionAsAlt::None), false), KeyAction::NewTab);
    }

    #[test]
    fn an_overlay_toggle_on_an_option_chord_closes_what_it_opened() {
        // German Mac: `open_palette = "Alt+L"`, and Option+L types '@' — ASCII,
        // so no US-position fallback either. The palette (like every overlay
        // and menu) resolves its own chord through `chord_action`, which
        // matches the bare key as the key path does; looking up the event's
        // '@' found nothing, and the second press typed '@' into the query.
        let b = crate::config::KeyBindings {
            open_palette: Some(crate::config::ChordSpec::One("Alt+L".into())),
            ..Default::default()
        };
        let km = crate::keymap::KeyMap::compile(&b);
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        let (logical, base) = (Key::Character("@".into()), Key::Character("l".into()));
        let mods = KeyMods { alt: true, lalt: true, ..KeyMods::default() };
        let ev = KeyInput { key_without_modifiers: &base, ..kev(KeyCode::KeyL, &logical, Some("@"), mods) };
        let alt = crate::keymap::Mods::new(false, false, true, false);
        assert_eq!(km.lookup(alt, ev.physical, &logical), None, "the composed '@' names no chord");
        assert_eq!(chord_action(&km, &ev, &mac(OptionAsAlt::None)), Some(KeyAction::OpenPalette));
        assert_eq!(decide_key_event(&km, &ev, &KeyModes::default(), &mac(OptionAsAlt::None), false), KeyAction::OpenPalette);
        // Off macOS, Alt never composes: the event's own key decides.
        let l = Key::Character("l".into());
        let ev = kev(KeyCode::KeyL, &l, Some("l"), KeyMods { alt: true, ..KeyMods::default() });
        assert_eq!(chord_action(&km, &ev, &KeyOptions::default()), Some(KeyAction::OpenPalette));
    }

    #[test]
    fn linux_alt_is_meta_even_for_non_ascii() {
        // Alt+ş (Turkish) / Alt+ф (Cyrillic) → ESC + the UTF-8 char; the old
        // "send non-ASCII text without ESC" heuristic dropped Meta here.
        for (phys, ch) in [(KeyCode::Semicolon, "ş"), (KeyCode::KeyA, "ф"), (KeyCode::KeyB, "b")] {
            let key = Key::Character(ch.into());
            let ev = kev(phys, &key, Some(ch), KeyMods { alt: true, ..KeyMods::default() });
            let mut want = vec![0x1b];
            want.extend_from_slice(ch.as_bytes());
            assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::Send(want), "Alt+{ch}");
        }
    }

    #[test]
    fn option_as_alt_parses_leniently_and_round_trips() {
        assert_eq!(OptionAsAlt::parse("left"), OptionAsAlt::Left);
        assert_eq!(OptionAsAlt::parse("Only_Right"), OptionAsAlt::Right);
        assert_eq!(OptionAsAlt::parse("BOTH"), OptionAsAlt::Both);
        assert_eq!(OptionAsAlt::parse("nonsense"), OptionAsAlt::None);
        #[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
        struct W {
            o: OptionAsAlt,
        }
        assert_eq!(toml::from_str::<W>("o = \"right\"").unwrap().o, OptionAsAlt::Right);
        assert_eq!(toml::from_str::<W>("o = true").unwrap().o, OptionAsAlt::Both);
        assert_eq!(toml::from_str::<W>("o = \"typo\"").unwrap().o, OptionAsAlt::None);
        let s = toml::to_string(&W { o: OptionAsAlt::Left }).unwrap();
        assert_eq!(s.trim(), "o = \"left\"");
    }

    #[test]
    fn releases_do_nothing_without_the_kitty_protocol() {
        let a = Key::Character("a".into());
        let ev = KeyInput { kind: KeyEventKind::Release, ..kev(KeyCode::KeyA, &a, None, KeyMods::default()) };
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::None);
        // …and no app command fires on a release.
        let t = Key::Character("T".into());
        let ev = KeyInput { kind: KeyEventKind::Release, ..kev(KeyCode::KeyT, &t, None, ctrl_shift()) };
        assert_eq!(decide(&ev, KeyModes::default(), KeyOptions::default()), KeyAction::None);
    }

    #[test]
    fn a_held_chord_repeats_only_the_actions_that_step() {
        let held = |code: KeyCode, key: &Key, mods: KeyMods, modes: KeyModes| {
            let press = decide(&kev(code, key, None, mods), modes, KeyOptions::default());
            let ev = KeyInput { kind: KeyEventKind::Repeat, ..kev(code, key, None, mods) };
            (press, decide(&ev, modes, KeyOptions::default()))
        };
        let legacy = KeyModes::default();
        let kitty = KeyModes { kitty_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES, ..legacy };
        let ctrl = KeyMods { ctrl: true, ..KeyMods::default() };
        let shift = KeyMods { shift: true, ..KeyMods::default() };
        let c = |s: &str| Key::Character(s.into());
        // One-shot: a held F11 flipped fullscreen at the repeat rate, a held
        // Ctrl+Shift+D detached tab after tab. The press acts; each repeat goes
        // nowhere — not to a kitty-protocol program either, which never saw
        // the press.
        for (code, key, mods, want) in [
            (KeyCode::F11, Key::Named(NamedKey::F11), KeyMods::default(), KeyAction::ToggleFullscreen),
            (KeyCode::KeyD, c("D"), ctrl_shift(), KeyAction::DetachTab),
            (KeyCode::Comma, c(","), ctrl, KeyAction::TogglePanel),
            (KeyCode::ContextMenu, Key::Named(NamedKey::ContextMenu), KeyMods::default(), KeyAction::ContextMenu),
            (KeyCode::KeyP, c("P"), ctrl_shift(), KeyAction::OpenPalette),
            (KeyCode::KeyF, c("F"), ctrl_shift(), KeyAction::SearchToggle),
            (KeyCode::KeyT, c("T"), ctrl_shift(), KeyAction::NewTab),
            (KeyCode::Enter, Key::Named(NamedKey::Enter), ctrl_shift(), KeyAction::RunSelection),
            (KeyCode::KeyV, c("V"), ctrl_shift(), KeyAction::Paste),
            (KeyCode::Digit2, c("2"), ctrl, KeyAction::SelectTab(1)),
        ] {
            for modes in [legacy, kitty] {
                assert_eq!(held(code, &key, mods, modes), (want.clone(), KeyAction::None), "{want:?}");
            }
        }
        // Steppers keep stepping while held.
        for (code, key, mods, want) in [
            (KeyCode::Equal, c("="), ctrl, KeyAction::FontUp),
            (KeyCode::Minus, c("-"), ctrl, KeyAction::FontDown),
            (KeyCode::PageUp, Key::Named(NamedKey::PageUp), shift, KeyAction::ScrollPageUp),
            (KeyCode::Tab, Key::Named(NamedKey::Tab), ctrl, KeyAction::NextTab),
            (KeyCode::KeyZ, c("Z"), ctrl_shift(), KeyAction::PrevPrompt),
            (KeyCode::Equal, c("="), KeyMods { alt: true, ..ctrl }, KeyAction::OpacityUp),
        ] {
            assert_eq!(held(code, &key, mods, legacy), (want.clone(), want.clone()), "{want:?}");
        }
        // Typing repeats, of course.
        assert_eq!(held(KeyCode::KeyA, &c("a"), KeyMods::default(), legacy).1, KeyAction::Send(b"a".to_vec()));
        assert_eq!(held(KeyCode::KeyC, &c("c"), ctrl, legacy).1, KeyAction::Send(vec![0x03]));
    }

    // ── kitty keyboard protocol (byte-exact vs kitty's key_encoding.c) ──────

    fn kitty(ev: &KeyInput<'_>, flags: u8) -> Option<Vec<u8>> {
        encode_kitty_key(ev, &KeyModes { kitty_flags: flags, ..KeyModes::default() }, &KeyOptions::default())
    }

    fn named_ev(n: NamedKey, phys: KeyCode, mods: KeyMods) -> (Key, PhysicalKey, KeyMods) {
        (Key::Named(n), PhysicalKey::Code(phys), mods)
    }

    fn kn(n: NamedKey, phys: KeyCode, mods: KeyMods, text: Option<&str>, flags: u8) -> Option<Vec<u8>> {
        let (key, physical, mods) = named_ev(n, phys, mods);
        let ev = KeyInput { physical, logical: &key, key_without_modifiers: &key, text, location: KeyLocation::Standard, kind: KeyEventKind::Press, mods };
        kitty(&ev, flags)
    }

    fn kc(ch: &str, base: &str, phys: KeyCode, mods: KeyMods, text: Option<&str>, flags: u8) -> Option<Vec<u8>> {
        let (logical, kwm) = (Key::Character(ch.into()), Key::Character(base.into()));
        let ev = KeyInput { physical: PhysicalKey::Code(phys), logical: &logical, key_without_modifiers: &kwm, text, location: KeyLocation::Standard, kind: KeyEventKind::Press, mods };
        kitty(&ev, flags)
    }

    fn b(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    const SHIFT: KeyMods = KeyMods { ctrl: false, shift: true, alt: false, super_: false, lalt: false, ralt: false };
    const CTRL: KeyMods = KeyMods { ctrl: true, shift: false, alt: false, super_: false, lalt: false, ralt: false };
    const ALT: KeyMods = KeyMods { ctrl: false, shift: false, alt: true, super_: false, lalt: false, ralt: false };
    const CTRL_SHIFT: KeyMods = KeyMods { ctrl: true, shift: true, alt: false, super_: false, lalt: false, ralt: false };
    const NONE: KeyMods = KeyMods { ctrl: false, shift: false, alt: false, super_: false, lalt: false, ralt: false };

    #[test]
    fn kitty_dead_and_textless_keys_send_nothing() {
        for f in [KITTY_DISAMBIGUATE, KITTY_DISAMBIGUATE | KITTY_REPORT_ALTERNATE_KEYS] {
            // Turkish-Q AltGr+. (dead_abovedot): no input until the key it composes with.
            let (dead, period) = (Key::Dead(Some('˙')), Key::Character(".".into()));
            let ev = KeyInput { physical: PhysicalKey::Code(KeyCode::Period), logical: &dead, key_without_modifiers: &period, text: None, location: KeyLocation::Standard, kind: KeyEventKind::Press, mods: NONE };
            assert_eq!(kitty(&ev, f), None, "dead key, flags {f}");
            // AltGr+W with no level-3 symbol: winit reports no text — never the plain `w`.
            let (unid, w) = (Key::Unidentified(winit::keyboard::NativeKey::Unidentified), Key::Character("w".into()));
            let ev = KeyInput { physical: PhysicalKey::Code(KeyCode::KeyW), logical: &unid, key_without_modifiers: &w, text: None, location: KeyLocation::Standard, kind: KeyEventKind::Press, mods: NONE };
            assert_eq!(kitty(&ev, f), None, "textless AltGr combo, flags {f}");
        }
    }

    #[test]
    fn kitty_macos_option_keeps_meta_on_keys_it_does_not_compose() {
        let opts = mac(OptionAsAlt::None);
        let modes = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        let opt = KeyMods { alt: true, lalt: true, ..KeyMods::default() };
        // Option+Enter / Option+Backspace keep Meta (legacy sent ESC CR / ESC DEL).
        let enter = Key::Named(NamedKey::Enter);
        let ev = KeyInput { physical: PhysicalKey::Code(KeyCode::Enter), logical: &enter, key_without_modifiers: &enter, text: Some("\r"), location: KeyLocation::Standard, kind: KeyEventKind::Press, mods: opt };
        assert_eq!(encode_kitty_key(&ev, &modes, &opts), b("\x1b[13;3u"));
        let bs = Key::Named(NamedKey::Backspace);
        let ev = KeyInput { physical: PhysicalKey::Code(KeyCode::Backspace), logical: &bs, key_without_modifiers: &bs, text: None, location: KeyLocation::Standard, kind: KeyEventKind::Press, mods: opt };
        assert_eq!(encode_kitty_key(&ev, &modes, &opts), b("\x1b[127;3u"));
        // A character Option composed still goes out as that character (Turkish Mac Option+Q).
        let (at, q) = (Key::Character("@".into()), Key::Character("q".into()));
        let ev = KeyInput { physical: PhysicalKey::Code(KeyCode::KeyQ), logical: &at, key_without_modifiers: &q, text: Some("@"), location: KeyLocation::Standard, kind: KeyEventKind::Press, mods: opt };
        assert_eq!(encode_kitty_key(&ev, &modes, &opts), b("@"));
    }

    #[test]
    fn kitty_disambiguate_flag() {
        let f = KITTY_DISAMBIGUATE;
        // Claude Code's newline: Shift+Enter → CSI 13;2u. Plain Enter stays \r.
        assert_eq!(kn(NamedKey::Enter, KeyCode::Enter, SHIFT, Some("\r"), f), b("\x1b[13;2u"));
        assert_eq!(kn(NamedKey::Enter, KeyCode::Enter, NONE, Some("\r"), f), b("\r"));
        assert_eq!(kn(NamedKey::Enter, KeyCode::Enter, CTRL, Some("\r"), f), b("\x1b[13;5u"));
        assert_eq!(kn(NamedKey::Enter, KeyCode::Enter, ALT, Some("\r"), f), b("\x1b[13;3u"));
        assert_eq!(kn(NamedKey::Escape, KeyCode::Escape, NONE, Some("\x1b"), f), b("\x1b[27u"));
        assert_eq!(kn(NamedKey::Tab, KeyCode::Tab, NONE, Some("\t"), f), b("\t"));
        assert_eq!(kn(NamedKey::Tab, KeyCode::Tab, SHIFT, None, f), b("\x1b[9;2u"));
        assert_eq!(kn(NamedKey::Backspace, KeyCode::Backspace, NONE, None, f), b("\x7f"));
        assert_eq!(kn(NamedKey::Backspace, KeyCode::Backspace, CTRL, None, f), b("\x1b[127;5u"));
        // Text keys: text as-is; Ctrl/Alt chords as CSI u (no SIGINT).
        assert_eq!(kc("a", "a", KeyCode::KeyA, NONE, Some("a"), f), b("a"));
        assert_eq!(kc("A", "a", KeyCode::KeyA, SHIFT, Some("A"), f), b("A"));
        assert_eq!(kc("c", "c", KeyCode::KeyC, CTRL, Some("c"), f), b("\x1b[99;5u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA, CTRL_SHIFT, Some("A"), f), b("\x1b[97;6u"));
        assert_eq!(kc("a", "a", KeyCode::KeyA, ALT, Some("a"), f), b("\x1b[97;3u"));
        // Ctrl+Space → CSI 32;5u.
        assert_eq!(kn(NamedKey::Space, KeyCode::Space, CTRL, Some(" "), f), b("\x1b[32;5u"));
        assert_eq!(kn(NamedKey::Space, KeyCode::Space, NONE, Some(" "), f), b(" "));
        // Functional keys: CSI forms, DECCKM no longer applies.
        assert_eq!(kn(NamedKey::ArrowLeft, KeyCode::ArrowLeft, NONE, None, f), b("\x1b[D"));
        assert_eq!(kn(NamedKey::ArrowLeft, KeyCode::ArrowLeft, CTRL, None, f), b("\x1b[1;5D"));
        assert_eq!(kn(NamedKey::Home, KeyCode::Home, NONE, None, f), b("\x1b[H"));
        assert_eq!(kn(NamedKey::F1, KeyCode::F1, NONE, None, f), b("\x1b[P"));
        assert_eq!(kn(NamedKey::F3, KeyCode::F3, NONE, None, f), b("\x1b[13~"));
        assert_eq!(kn(NamedKey::F5, KeyCode::F5, SHIFT, None, f), b("\x1b[15;2~"));
        assert_eq!(kn(NamedKey::F13, KeyCode::F13, NONE, None, f), b("\x1b[57376u"));
        assert_eq!(kn(NamedKey::PageUp, KeyCode::PageUp, NONE, None, f), b("\x1b[5~"));
        assert_eq!(kn(NamedKey::Insert, KeyCode::Insert, NONE, None, f), b("\x1b[2~"));
        // Keypad: text-producing keys stay text; non-text keypad keys get KP codes.
        assert_eq!(kc("1", "1", KeyCode::Numpad1, NONE, Some("1"), f), b("1"));
        assert_eq!(kn(NamedKey::Enter, KeyCode::NumpadEnter, NONE, Some("\r"), f), b("\x1b[57414u"));
        assert_eq!(kn(NamedKey::Home, KeyCode::Numpad7, NONE, None, f), b("\x1b[57423u"));
        // Modifier keys alone are not reported without "report all keys".
        assert_eq!(kn(NamedKey::Shift, KeyCode::ShiftLeft, SHIFT, None, f), None);
    }

    #[test]
    fn kitty_event_types_flag() {
        let f = KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES;
        let a = Key::Character("a".into());
        let base = |kind| KeyInput { physical: PhysicalKey::Code(KeyCode::KeyA), logical: &a, key_without_modifiers: &a, text: Some("a"), location: KeyLocation::Standard, kind, mods: NONE };
        assert_eq!(kitty(&base(KeyEventKind::Press), f), b("a"));
        assert_eq!(kitty(&base(KeyEventKind::Repeat), f), b("a"));
        assert_eq!(kitty(&KeyInput { text: None, ..base(KeyEventKind::Release) }, f), b("\x1b[97;1:3u"));
        let left = Key::Named(NamedKey::ArrowLeft);
        let ev = |kind| KeyInput { physical: PhysicalKey::Code(KeyCode::ArrowLeft), logical: &left, key_without_modifiers: &left, text: None, location: KeyLocation::Standard, kind, mods: NONE };
        assert_eq!(kitty(&ev(KeyEventKind::Repeat), f), b("\x1b[1;1:2D"));
        assert_eq!(kitty(&ev(KeyEventKind::Release), f), b("\x1b[1;1:3D"));
        // Enter/Tab/Backspace report no release (so `reset` stays typeable)…
        let enter = Key::Named(NamedKey::Enter);
        let e = |mods, kind| KeyInput { physical: PhysicalKey::Code(KeyCode::Enter), logical: &enter, key_without_modifiers: &enter, text: None, location: KeyLocation::Standard, kind, mods };
        assert_eq!(kitty(&e(NONE, KeyEventKind::Release), f), None);
        // …unless modified (kitty's encoder reports Shift+Enter's release).
        assert_eq!(kitty(&e(SHIFT, KeyEventKind::Release), f), b("\x1b[13;2:3u"));
        // Event types alone keep legacy bytes where they exist.
        let only = KITTY_REPORT_EVENT_TYPES;
        assert_eq!(kn(NamedKey::Escape, KeyCode::Escape, NONE, None, only), b("\x1b"));
        assert_eq!(kc("a", "a", KeyCode::KeyA, CTRL, None, only), b("\x01"));
        assert_eq!(kc("A", "a", KeyCode::KeyA, CTRL_SHIFT, None, only), b("\x1b[97;6u"));
        // (deviation from kitty: Esc's release is an escape code, not a 2nd ESC)
        let esc = Key::Named(NamedKey::Escape);
        let rel = KeyInput { physical: PhysicalKey::Code(KeyCode::Escape), logical: &esc, key_without_modifiers: &esc, text: None, location: KeyLocation::Standard, kind: KeyEventKind::Release, mods: NONE };
        assert_eq!(kitty(&rel, only), b("\x1b[27;1:3u"));
    }

    #[test]
    fn kitty_alternate_keys_flag() {
        let f = KITTY_DISAMBIGUATE | KITTY_REPORT_ALTERNATE_KEYS;
        assert_eq!(kc("A", "a", KeyCode::KeyA, CTRL_SHIFT, None, f), b("\x1b[97:65;6u"));
        // US layout: the base-layout key equals the key → not repeated.
        assert_eq!(kc("a", "a", KeyCode::KeyA, CTRL, None, f), b("\x1b[97;5u"));
        // Cyrillic Ctrl+С: key 'с' (1089) + base-layout 'c' (99).
        assert_eq!(kc("с", "с", KeyCode::KeyC, CTRL, None, f), b("\x1b[1089::99;5u"));
        // Turkish-Q Ctrl+ı: key 'ı' (305), base 'i' (105).
        assert_eq!(kc("ı", "ı", KeyCode::KeyI, CTRL, None, f), b("\x1b[305::105;5u"));
        // Shifted digit: key '1', shifted '!'.
        assert_eq!(kc("!", "1", KeyCode::Digit1, CTRL_SHIFT, None, f), b("\x1b[49:33;6u"));
        // Alternates never decorate functional keys.
        assert_eq!(kn(NamedKey::ArrowUp, KeyCode::ArrowUp, CTRL, None, f), b("\x1b[1;5A"));
    }

    #[test]
    fn kitty_report_all_keys_and_text() {
        let all = KITTY_REPORT_ALL_KEYS;
        assert_eq!(kc("a", "a", KeyCode::KeyA, NONE, Some("a"), all), b("\x1b[97u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA, SHIFT, Some("A"), all), b("\x1b[97;2u"));
        assert_eq!(kn(NamedKey::Enter, KeyCode::Enter, NONE, Some("\r"), all), b("\x1b[13u"));
        assert_eq!(kn(NamedKey::Tab, KeyCode::Tab, NONE, Some("\t"), all), b("\x1b[9u"));
        assert_eq!(kn(NamedKey::Escape, KeyCode::Escape, NONE, None, all), b("\x1b[27u"));
        // Modifier keys are reported, their own bit set on press, clear on release.
        let shift = Key::Named(NamedKey::Shift);
        let m = |kind, mods| KeyInput { physical: PhysicalKey::Code(KeyCode::ShiftLeft), logical: &shift, key_without_modifiers: &shift, text: None, location: KeyLocation::Left, kind, mods };
        assert_eq!(kitty(&m(KeyEventKind::Press, NONE), all), b("\x1b[57441;2u"));
        assert_eq!(kitty(&m(KeyEventKind::Release, SHIFT), all | KITTY_REPORT_EVENT_TYPES), b("\x1b[57441;1:3u"));
        let ctrl_r = Key::Named(NamedKey::Control);
        let rc = KeyInput { physical: PhysicalKey::Code(KeyCode::ControlRight), logical: &ctrl_r, key_without_modifiers: &ctrl_r, text: None, location: KeyLocation::Right, kind: KeyEventKind::Press, mods: NONE };
        assert_eq!(kitty(&rc, all), b("\x1b[57448;5u"));
        // Associated text (needs report-all).
        let txt = all | KITTY_REPORT_ASSOCIATED_TEXT;
        assert_eq!(kc("a", "a", KeyCode::KeyA, NONE, Some("a"), txt), b("\x1b[97;;97u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA, SHIFT, Some("A"), txt), b("\x1b[97;2;65u"));
        assert_eq!(kc("1", "1", KeyCode::Numpad1, NONE, Some("1"), txt), b("\x1b[57400;;49u"));
        // AltGr text: the composed char, no alt bit.
        assert_eq!(kc("@", "q", KeyCode::KeyQ, NONE, Some("@"), txt), b("\x1b[113;;64u"));
    }

    #[test]
    fn kitty_flags_reach_decide_key_event_after_the_keymap() {
        // JeTTY's own chords still win (Ctrl+Shift+C copies)…
        let c = Key::Character("C".into());
        let modes = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        let ev = kev(KeyCode::KeyC, &c, Some("C"), ctrl_shift());
        assert_eq!(decide_key_event(&crate::keymap::KeyMap::defaults(), &ev, &modes, &KeyOptions::default(), false), KeyAction::Copy);
        // …and everything else is encoded by the protocol.
        let enter = Key::Named(NamedKey::Enter);
        let ev = kev(KeyCode::Enter, &enter, Some("\r"), SHIFT);
        assert_eq!(decide_key_event(&crate::keymap::KeyMap::defaults(), &ev, &modes, &KeyOptions::default(), false), KeyAction::Send(b"\x1b[13;2u".to_vec()));
        // Only "alternate keys" set = legacy encoding.
        let modes = KeyModes { kitty_flags: KITTY_REPORT_ALTERNATE_KEYS, ..KeyModes::default() };
        assert_eq!(decide_key_event(&crate::keymap::KeyMap::defaults(), &ev, &modes, &KeyOptions::default(), false), KeyAction::Send(b"\r".to_vec()));
    }

    #[test]
    fn kitty_macos_option_compose_is_text_without_alt() {
        let opts = mac(OptionAsAlt::None);
        let (at, q) = (Key::Character("@".into()), Key::Character("q".into()));
        let ev = KeyInput { key_without_modifiers: &q, ..kev(KeyCode::KeyQ, &at, Some("@"), KeyMods { alt: true, lalt: true, ..KeyMods::default() }) };
        let modes = KeyModes { kitty_flags: KITTY_DISAMBIGUATE, ..KeyModes::default() };
        assert_eq!(encode_kitty_key(&ev, &modes, &opts), b("@"));
        // As Meta (option_as_alt = both; winit reports the plain key) → CSI q;3u.
        let ev = KeyInput { key_without_modifiers: &q, ..kev(KeyCode::KeyQ, &q, Some("q"), KeyMods { alt: true, lalt: true, ..KeyMods::default() }) };
        assert_eq!(encode_kitty_key(&ev, &modes, &mac(OptionAsAlt::Both)), b("\x1b[113;3u"));
    }

    // ── mouse reporting ─────────────────────────────────────────────────────

    fn rep(button: Option<MouseBtn>, act: MouseAct) -> MouseReport {
        MouseReport::new(button, act)
    }

    #[test]
    fn mouse_buttons_and_modifiers_in_every_encoding() {
        let n = MouseTracking::Normal;
        let right = rep(Some(MouseBtn::Right), MouseAct::Press);
        assert_eq!(encode_mouse_report(&right, 5, 3, MouseEncoding::Sgr, n), b"\x1b[<2;5;3M");
        assert_eq!(encode_mouse_report(&MouseReport { act: MouseAct::Release, ..right }, 5, 3, MouseEncoding::Sgr, n), b"\x1b[<2;5;3m");
        let middle = rep(Some(MouseBtn::Middle), MouseAct::Press);
        assert_eq!(encode_mouse_report(&middle, 1, 1, MouseEncoding::Legacy, n), vec![0x1b, b'[', b'M', 33, 33, 33]);
        // Legacy release = button 3 (+ modifiers).
        let rel = MouseReport { ctrl: true, ..rep(Some(MouseBtn::Right), MouseAct::Release) };
        assert_eq!(encode_mouse_report(&rel, 1, 1, MouseEncoding::Legacy, n), vec![0x1b, b'[', b'M', 32 + 3 + 16, 33, 33]);
        // shift 4 + meta 8 + ctrl 16 on a left press.
        let all = MouseReport { shift: true, alt: true, ctrl: true, ..rep(Some(MouseBtn::Left), MouseAct::Press) };
        assert_eq!(encode_mouse_report(&all, 2, 3, MouseEncoding::Sgr, n), b"\x1b[<28;2;3M");
        // Wheel left/right = 66/67; buttons 8/9 = 128/129.
        assert_eq!(encode_mouse_report(&rep(Some(MouseBtn::WheelLeft), MouseAct::Press), 1, 1, MouseEncoding::Sgr, n), b"\x1b[<66;1;1M");
        assert_eq!(encode_mouse_report(&rep(Some(MouseBtn::WheelRight), MouseAct::Press), 1, 1, MouseEncoding::Sgr, n), b"\x1b[<67;1;1M");
        assert_eq!(encode_mouse_report(&rep(Some(MouseBtn::Back), MouseAct::Press), 1, 1, MouseEncoding::Sgr, n), b"\x1b[<128;1;1M");
        // urxvt 1015: decimal, 32 + Cb, release = 3.
        assert_eq!(encode_mouse_report(&right, 300, 70, MouseEncoding::Urxvt, n), b"\x1b[34;300;70M");
        assert_eq!(encode_mouse_report(&MouseReport { act: MouseAct::Release, ..right }, 300, 70, MouseEncoding::Urxvt, n), b"\x1b[35;300;70M");
        // X10 mode: no modifier bits.
        assert_eq!(encode_mouse_report(&all, 1, 1, MouseEncoding::Legacy, MouseTracking::X10), vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn mouse_coordinate_limits_per_encoding() {
        let n = MouseTracking::Normal;
        let left = rep(Some(MouseBtn::Left), MouseAct::Press);
        // Legacy: 223 max (one byte).
        assert_eq!(encode_mouse_report(&left, 500, 999, MouseEncoding::Legacy, n), vec![0x1b, b'[', b'M', 32, 255, 255]);
        // UTF-8 1005: 32 + 300 = 332 → two-byte UTF-8 (0xC5 0x8C); 95 + 32 = 127
        // stays one byte; positions clamp at 2015.
        assert_eq!(encode_mouse_report(&left, 300, 95, MouseEncoding::Utf8, n), vec![0x1b, b'[', b'M', 32, 0xC5, 0x8C, 127]);
        let big = encode_mouse_report(&left, 9000, 1, MouseEncoding::Utf8, n);
        assert_eq!(&big[4..6], "\u{7ff}".as_bytes(), "2015 + 32 = 2047");
        // SGR: unbounded.
        assert_eq!(encode_mouse_report(&left, 9000, 4000, MouseEncoding::Sgr, n), b"\x1b[<0;9000;4000M");
    }

    #[test]
    fn mouse_tracking_modes_gate_events() {
        use MouseTracking as T;
        let press = rep(Some(MouseBtn::Left), MouseAct::Press);
        let release = rep(Some(MouseBtn::Left), MouseAct::Release);
        let drag = rep(Some(MouseBtn::Left), MouseAct::Motion);
        let hover = rep(None, MouseAct::Motion);
        let wheel = rep(Some(MouseBtn::WheelUp), MouseAct::Press);
        let wheel_rel = rep(Some(MouseBtn::WheelUp), MouseAct::Release);
        let r = |t, e: &MouseReport| mouse_reportable(t, e);
        assert!(!r(T::Off, &press));
        assert!(r(T::X10, &press) && !r(T::X10, &release) && !r(T::X10, &wheel));
        assert!(r(T::Normal, &press) && r(T::Normal, &release) && r(T::Normal, &wheel));
        assert!(!r(T::Normal, &drag) && !r(T::Normal, &wheel_rel));
        assert!(r(T::ButtonEvent, &drag) && !r(T::ButtonEvent, &hover));
        assert!(r(T::AnyEvent, &drag) && r(T::AnyEvent, &hover));
        // Motion codes: left-drag 32, hover 35.
        assert_eq!(encode_mouse_report(&drag, 1, 1, MouseEncoding::Sgr, T::ButtonEvent), b"\x1b[<32;1;1M");
        assert_eq!(encode_mouse_report(&hover, 1, 1, MouseEncoding::Sgr, T::AnyEvent), b"\x1b[<35;1;1M");
        // Mode-bit priority.
        assert_eq!(T::from_modes(true, true, true, true), T::AnyEvent);
        assert_eq!(T::from_modes(false, true, true, false), T::ButtonEvent);
        assert_eq!(MouseEncoding::from_modes(true, true, true), MouseEncoding::Sgr);
        assert_eq!(MouseEncoding::from_modes(true, false, true), MouseEncoding::Urxvt);
        assert_eq!(MouseEncoding::from_modes(true, false, false), MouseEncoding::Utf8);
    }

    #[test]
    fn wheel_converts_at_the_real_cell_size() {
        use winit::dpi::PhysicalPosition;
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 17.0), 3.0);
        let px = |y: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y));
        assert_eq!(wheel_lines(px(40.0), 20.0), 2.0);
        assert_eq!(wheel_lines(px(40.0), 40.0), 1.0);
        assert_eq!(wheel_lines(px(-34.0), 34.0), -1.0);
        // Unknown cell height → the old 20 px estimate.
        assert_eq!(wheel_lines(px(40.0), 0.0), 2.0);
        assert_eq!(wheel_lines(px(40.0), f32::NAN), 2.0);
        assert_eq!(wheel_columns(MouseScrollDelta::PixelDelta(PhysicalPosition::new(30.0, 0.0)), 10.0), 3.0);
    }

    // ── click counting ──────────────────────────────────────────────────────

    #[test]
    fn click_tracker_counts_double_and_triple_clicks() {
        use winit::event::MouseButton as B;
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut c = ClickTracker::new();
        assert_eq!(c.press(B::Left, ms(0), 10.0, 10.0), 1);
        assert_eq!(c.press(B::Left, ms(150), 12.0, 11.0), 2);
        assert_eq!(c.press(B::Left, ms(300), 10.0, 10.0), 3);
        assert_eq!(c.press(B::Left, ms(450), 10.0, 10.0), 1, "a fourth click starts over");
        // Too slow → single.
        assert_eq!(c.press(B::Left, ms(1000), 10.0, 10.0), 1);
        // Moved too far → single.
        assert_eq!(c.press(B::Left, ms(1100), 30.0, 10.0), 1);
        // Another button → single.
        assert_eq!(c.press(B::Right, ms(1200), 30.0, 10.0), 1);
        assert_eq!(c.press(B::Left, ms(1300), 30.0, 10.0), 1);
        // reset() forgets the sequence.
        c.reset();
        assert_eq!(c.press(B::Left, ms(1350), 30.0, 10.0), 1);
        assert_eq!(c.press(B::Left, ms(1360), 30.0, 10.0), 2);
    }
}
