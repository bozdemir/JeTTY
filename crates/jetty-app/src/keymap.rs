//! Configurable keybindings: a chord grammar (parse + serialize), a compiled
//! [`KeyMap`] (two small hashmaps — logical + physical), and
//! [`KeyMap::lookup`], which `decide_key` calls once at the top to resolve the
//! discrete app-command chords.
//!
//! PRIME DIRECTIVE: a user with no `[keys]` config gets byte-for-byte today's
//! behavior. The default keymap reproduces every hardcoded chord `decide_key`
//! (and the old macOS `Cmd` block) used, INCLUDING today's exact modifier
//! semantics — the Ctrl+Shift / tab-nav / font chords never tested Alt, so their
//! defaults are Alt-INSENSITIVE; the macOS `Cmd` chords never tested Shift, so
//! their defaults are Shift-INSENSITIVE. User-supplied chords use exact matching.

use std::collections::HashMap;

use winit::keyboard::{Key, KeyCode, PhysicalKey, SmolStr};

use crate::config::{ChordSpec, KeyBindings};
use crate::input::KeyAction;

/// Exact keyboard modifier state a chord matches. Order-insensitive when parsed;
/// compared exactly at lookup time (the default keymap seeds Alt/Shift variants
/// explicitly to reproduce today's looser matching).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
}

impl Mods {
    pub fn new(ctrl: bool, shift: bool, alt: bool, super_: bool) -> Self {
        Mods { ctrl, shift, alt, super_ }
    }
    fn is_empty(&self) -> bool {
        !self.ctrl && !self.shift && !self.alt && !self.super_
    }
    /// Ctrl is the only modifier held (used for the control-byte-shadow guard).
    fn ctrl_only(&self) -> bool {
        self.ctrl && !self.shift && !self.alt && !self.super_
    }
}

/// How a chord's key segment resolves against a key event.
#[derive(Clone, Debug, PartialEq)]
enum KeyMatch {
    /// Match `event.physical_key` (layout-invariant position). Used for letters,
    /// digits 1-9, and named keys.
    Phys(KeyCode),
    /// Match the produced logical character(s) (layout-following), with a
    /// US-position physical fallback. Used for font/opacity symbols, `0`, and
    /// `Super`-letters (macOS label convention).
    Logical {
        chars: Vec<SmolStr>,
        phys_fallback: Option<KeyCode>,
    },
}

/// A single parsed/compiled chord.
#[derive(Clone, Debug, PartialEq)]
struct Chord {
    mods: Mods,
    key: KeyMatch,
    /// Default chords reproduce today's Alt-don't-care matching (the Ctrl+Shift /
    /// tab-nav / font blocks never tested Alt). Seeds an Alt-flipped variant.
    alt_insensitive: bool,
    /// macOS `Cmd` defaults reproduce today's Shift-don't-care matching (the old
    /// Cmd block matched the folded char regardless of Shift). Seeds a
    /// Shift-flipped variant.
    shift_insensitive: bool,
}

impl Chord {
    fn exact(mods: Mods, key: KeyMatch) -> Self {
        Chord { mods, key, alt_insensitive: false, shift_insensitive: false }
    }
    fn alt_loose(mods: Mods, key: KeyMatch) -> Self {
        Chord { mods, key, alt_insensitive: true, shift_insensitive: false }
    }

    /// Canonical serialized form (word key names, fixed modifier order). Idempotent
    /// under re-parse: `parse(parse(s).canonical()) == parse(s)`. Used by the
    /// round-trip test (and available for a future `--dump-keys`).
    #[cfg(test)]
    fn canonical(&self) -> String {
        let mut s = String::new();
        if self.mods.ctrl {
            s.push_str("Ctrl+");
        }
        if self.mods.alt {
            s.push_str("Alt+");
        }
        if self.mods.shift {
            s.push_str("Shift+");
        }
        if self.mods.super_ {
            s.push_str("Super+");
        }
        s.push_str(&self.key_canonical());
        s
    }

    #[cfg(test)]
    fn key_canonical(&self) -> String {
        match &self.key {
            KeyMatch::Phys(code) => keycode_word(*code).to_string(),
            KeyMatch::Logical { chars, phys_fallback } => {
                if let Some(fb) = phys_fallback {
                    keycode_word(*fb).to_string()
                } else if let Some(c) = chars.first() {
                    // Super-letter: canonicalize as the uppercase letter.
                    c.to_uppercase()
                } else {
                    "None".to_string()
                }
            }
        }
    }

    /// Human-facing pretty form (symbols instead of words) for the help overlay.
    fn pretty(&self) -> String {
        let mut s = String::new();
        if self.mods.ctrl {
            s.push_str("Ctrl+");
        }
        if self.mods.alt {
            s.push_str("Alt+");
        }
        if self.mods.shift {
            s.push_str("Shift+");
        }
        if self.mods.super_ {
            s.push_str(if cfg!(target_os = "macos") { "Cmd+" } else { "Super+" });
        }
        match &self.key {
            KeyMatch::Phys(code) => s.push_str(&keycode_pretty(*code)),
            KeyMatch::Logical { chars, .. } => {
                if let Some(c) = chars.first() {
                    s.push_str(c);
                }
            }
        }
        s
    }
}

/// The set of remappable actions. Declaration order is the canonical conflict-
/// resolution priority (earlier wins a shared slot). Distinct from [`KeyAction`]
/// because the raw-encoding variants (Send / Scroll / ClosePanel / None) are not
/// remappable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindableAction {
    ToggleSettings,
    OpenPalette,
    NewTab,
    CloseTab,
    DetachTab,
    SearchToggle,
    PrevPrompt,
    NextPrompt,
    PrevTab,
    NextTab,
    SelectTab1,
    SelectTab2,
    SelectTab3,
    SelectTab4,
    SelectTab5,
    SelectTab6,
    SelectTab7,
    SelectTab8,
    SelectTab9,
    Copy,
    Paste,
    OpacityUp,
    OpacityDown,
    FontUp,
    FontDown,
    FontReset,
    SelectAll,
    Quit,
}

impl BindableAction {
    pub const ALL: [BindableAction; 28] = [
        BindableAction::ToggleSettings,
        BindableAction::OpenPalette,
        BindableAction::NewTab,
        BindableAction::CloseTab,
        BindableAction::DetachTab,
        BindableAction::SearchToggle,
        BindableAction::PrevPrompt,
        BindableAction::NextPrompt,
        BindableAction::PrevTab,
        BindableAction::NextTab,
        BindableAction::SelectTab1,
        BindableAction::SelectTab2,
        BindableAction::SelectTab3,
        BindableAction::SelectTab4,
        BindableAction::SelectTab5,
        BindableAction::SelectTab6,
        BindableAction::SelectTab7,
        BindableAction::SelectTab8,
        BindableAction::SelectTab9,
        BindableAction::Copy,
        BindableAction::Paste,
        BindableAction::OpacityUp,
        BindableAction::OpacityDown,
        BindableAction::FontUp,
        BindableAction::FontDown,
        BindableAction::FontReset,
        BindableAction::SelectAll,
        BindableAction::Quit,
    ];

    /// Stable name for warnings / debugging.
    fn name(self) -> &'static str {
        use BindableAction::*;
        match self {
            ToggleSettings => "toggle_settings",
            OpenPalette => "open_palette",
            NewTab => "new_tab",
            CloseTab => "close_tab",
            DetachTab => "detach_tab",
            SearchToggle => "search_toggle",
            PrevPrompt => "prev_prompt",
            NextPrompt => "next_prompt",
            PrevTab => "prev_tab",
            NextTab => "next_tab",
            SelectTab1 => "select_tab_1",
            SelectTab2 => "select_tab_2",
            SelectTab3 => "select_tab_3",
            SelectTab4 => "select_tab_4",
            SelectTab5 => "select_tab_5",
            SelectTab6 => "select_tab_6",
            SelectTab7 => "select_tab_7",
            SelectTab8 => "select_tab_8",
            SelectTab9 => "select_tab_9",
            Copy => "copy",
            Paste => "paste",
            OpacityUp => "opacity_up",
            OpacityDown => "opacity_down",
            FontUp => "font_up",
            FontDown => "font_down",
            FontReset => "font_reset",
            SelectAll => "select_all",
            Quit => "quit",
        }
    }

    /// The [`KeyAction`] this binding dispatches.
    fn key_action(self) -> KeyAction {
        use BindableAction::*;
        match self {
            ToggleSettings => KeyAction::TogglePanel,
            OpenPalette => KeyAction::OpenPalette,
            NewTab => KeyAction::NewTab,
            CloseTab => KeyAction::CloseTab,
            DetachTab => KeyAction::DetachTab,
            SearchToggle => KeyAction::SearchToggle,
            PrevPrompt => KeyAction::PrevPrompt,
            NextPrompt => KeyAction::NextPrompt,
            PrevTab => KeyAction::PrevTab,
            NextTab => KeyAction::NextTab,
            SelectTab1 => KeyAction::SelectTab(0),
            SelectTab2 => KeyAction::SelectTab(1),
            SelectTab3 => KeyAction::SelectTab(2),
            SelectTab4 => KeyAction::SelectTab(3),
            SelectTab5 => KeyAction::SelectTab(4),
            SelectTab6 => KeyAction::SelectTab(5),
            SelectTab7 => KeyAction::SelectTab(6),
            SelectTab8 => KeyAction::SelectTab(7),
            SelectTab9 => KeyAction::SelectTab(8),
            Copy => KeyAction::Copy,
            Paste => KeyAction::Paste,
            OpacityUp => KeyAction::OpacityUp,
            OpacityDown => KeyAction::OpacityDown,
            FontUp => KeyAction::FontUp,
            FontDown => KeyAction::FontDown,
            FontReset => KeyAction::FontReset,
            SelectAll => KeyAction::SelectAll,
            Quit => KeyAction::Quit,
        }
    }

    /// This action's user override (if any) from the `[keys]` table.
    fn user_spec(self, b: &KeyBindings) -> &Option<ChordSpec> {
        use BindableAction::*;
        match self {
            ToggleSettings => &b.toggle_settings,
            OpenPalette => &b.open_palette,
            NewTab => &b.new_tab,
            CloseTab => &b.close_tab,
            DetachTab => &b.detach_tab,
            SearchToggle => &b.search_toggle,
            PrevPrompt => &b.prev_prompt,
            NextPrompt => &b.next_prompt,
            PrevTab => &b.prev_tab,
            NextTab => &b.next_tab,
            SelectTab1 => &b.select_tab_1,
            SelectTab2 => &b.select_tab_2,
            SelectTab3 => &b.select_tab_3,
            SelectTab4 => &b.select_tab_4,
            SelectTab5 => &b.select_tab_5,
            SelectTab6 => &b.select_tab_6,
            SelectTab7 => &b.select_tab_7,
            SelectTab8 => &b.select_tab_8,
            SelectTab9 => &b.select_tab_9,
            Copy => &b.copy,
            Paste => &b.paste,
            OpacityUp => &b.opacity_up,
            OpacityDown => &b.opacity_down,
            FontUp => &b.font_up,
            FontDown => &b.font_down,
            FontReset => &b.font_reset,
            SelectAll => &b.select_all,
            Quit => &b.quit,
        }
    }

    /// The built-in default chords reproducing today's exact behavior. macOS
    /// `Cmd` chords are seeded ONLY under `cfg!(target_os = "macos")`, so Linux
    /// leaves bare `Super` to the window manager, exactly as before.
    fn default_chords(self) -> Vec<Chord> {
        use BindableAction::*;
        match self {
            ToggleSettings => {
                let mut v = vec![
                    // Ctrl+, (logical + physical fallback) and Ctrl+Shift+O.
                    ctrl(sym(",", KeyCode::Comma)),
                    ctrl_shift(KeyMatch::Phys(KeyCode::KeyO)),
                ];
                push_cmd(&mut v, cmd_symbol(logical_only(",")));
                v
            }
            OpenPalette => {
                let mut v = vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyP))];
                push_cmd(&mut v, cmd_letter("p"));
                v
            }
            NewTab => {
                let mut v = vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyT))];
                push_cmd(&mut v, cmd_letter("t"));
                v
            }
            CloseTab => {
                let mut v = vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyW))];
                push_cmd(&mut v, cmd_letter("w"));
                v
            }
            DetachTab => vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyD))],
            SearchToggle => vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyF))],
            PrevPrompt => vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyZ))],
            NextPrompt => vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyX))],
            PrevTab => vec![ctrl_shift(KeyMatch::Phys(KeyCode::Tab))],
            NextTab => vec![ctrl(KeyMatch::Phys(KeyCode::Tab))],
            SelectTab1 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit1))],
            SelectTab2 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit2))],
            SelectTab3 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit3))],
            SelectTab4 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit4))],
            SelectTab5 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit5))],
            SelectTab6 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit6))],
            SelectTab7 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit7))],
            SelectTab8 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit8))],
            SelectTab9 => vec![ctrl(KeyMatch::Phys(KeyCode::Digit9))],
            Copy => {
                let mut v = vec![ctrl_shift(KeyMatch::Phys(KeyCode::KeyC))];
                push_cmd(&mut v, cmd_letter("c"));
                v
            }
            Paste => {
                let mut v = vec![
                    ctrl_shift(KeyMatch::Phys(KeyCode::KeyV)),
                    // Shift+Insert is EXACT (today: shift && !ctrl && !alt).
                    Chord::exact(Mods::new(false, true, false, false), KeyMatch::Phys(KeyCode::Insert)),
                ];
                push_cmd(&mut v, cmd_letter("v"));
                v
            }
            OpacityUp => vec![ctrl_shift(font_up_keymatch())],
            OpacityDown => vec![ctrl_shift(opacity_down_keymatch())],
            FontUp => {
                let mut v = vec![ctrl(font_up_keymatch())];
                push_cmd(&mut v, cmd_symbol(font_up_keymatch()));
                v
            }
            FontDown => {
                let mut v = vec![ctrl(font_down_keymatch())];
                push_cmd(&mut v, cmd_symbol(font_down_keymatch()));
                v
            }
            FontReset => {
                let mut v = vec![ctrl(font_reset_keymatch())];
                push_cmd(&mut v, cmd_symbol(font_reset_keymatch()));
                v
            }
            SelectAll => {
                // No Linux default (today: macOS Cmd+A only).
                let mut v = Vec::new();
                push_cmd(&mut v, cmd_letter("a"));
                v
            }
            Quit => {
                // No Linux default (today: macOS Cmd+Q only).
                let mut v = Vec::new();
                push_cmd(&mut v, cmd_letter("q"));
                v
            }
        }
    }
}

// ── default-chord keymatch/chord helpers ─────────────────────────────────────

/// Ctrl (no shift), Alt-don't-care (today's blocks never tested Alt).
fn ctrl(k: KeyMatch) -> Chord {
    Chord::alt_loose(Mods::new(true, false, false, false), k)
}

/// Ctrl+Shift, Alt-don't-care.
fn ctrl_shift(k: KeyMatch) -> Chord {
    Chord::alt_loose(Mods::new(true, true, false, false), k)
}

/// A logical symbol keymatch with a US physical fallback.
fn sym(ch: &str, phys: KeyCode) -> KeyMatch {
    KeyMatch::Logical { chars: vec![SmolStr::new(ch)], phys_fallback: Some(phys) }
}

/// A logical-char keymatch with NO physical fallback (macOS Cmd label convention).
fn logical_only(ch: &str) -> KeyMatch {
    KeyMatch::Logical { chars: vec![SmolStr::new(ch)], phys_fallback: None }
}

fn font_up_keymatch() -> KeyMatch {
    // FontUp / OpacityUp: '+' and '=' both engrave the Equal key.
    KeyMatch::Logical {
        chars: vec![SmolStr::new("="), SmolStr::new("+")],
        phys_fallback: Some(KeyCode::Equal),
    }
}

fn font_down_keymatch() -> KeyMatch {
    // FontDown is '-' ONLY — never '_' (Ctrl+_ must keep sending 0x1f).
    KeyMatch::Logical { chars: vec![SmolStr::new("-")], phys_fallback: Some(KeyCode::Minus) }
}

fn font_reset_keymatch() -> KeyMatch {
    KeyMatch::Logical { chars: vec![SmolStr::new("0")], phys_fallback: Some(KeyCode::Digit0) }
}

fn opacity_down_keymatch() -> KeyMatch {
    // OpacityDown matches '-' AND '_' (today's `"-" | "_"` arm).
    KeyMatch::Logical {
        chars: vec![SmolStr::new("-"), SmolStr::new("_")],
        phys_fallback: Some(KeyCode::Minus),
    }
}

/// A macOS Cmd chord matching the folded logical letter, Shift-insensitive.
fn cmd_letter(ch: &str) -> Chord {
    Chord {
        mods: Mods::new(false, false, false, true),
        key: logical_only(ch),
        alt_insensitive: false,
        shift_insensitive: true,
    }
}

/// A macOS Cmd chord over an arbitrary logical keymatch, Shift-insensitive.
fn cmd_symbol(key: KeyMatch) -> Chord {
    Chord {
        mods: Mods::new(false, false, false, true),
        key,
        alt_insensitive: false,
        shift_insensitive: true,
    }
}

/// Push a macOS `Cmd` default only under macOS (a no-op on other platforms, so
/// Linux never seeds a bare-Super chord). The `#[allow]`ed args keep the helper
/// signature identical across platforms.
#[cfg(target_os = "macos")]
fn push_cmd(v: &mut Vec<Chord>, chord: Chord) {
    v.push(chord);
}

#[cfg(not(target_os = "macos"))]
fn push_cmd(_v: &mut [Chord], _chord: Chord) {}

/// A compiled, ready-to-query keymap.
pub struct KeyMap {
    physical: HashMap<(Mods, KeyCode), KeyAction>,
    logical: HashMap<(Mods, SmolStr), KeyAction>,
    /// The compiled chords per action (unexpanded), for help/display.
    by_action: Vec<(BindableAction, Vec<Chord>)>,
    /// Human-readable compile warnings (conflicts / rejected binds / invalid
    /// chords). Surfaced by the caller (GUI users never see stderr).
    warnings: Vec<String>,
}

impl PartialEq for KeyMap {
    fn eq(&self, other: &Self) -> bool {
        // Compare only the resolved maps — by_action/warnings are derived from them.
        self.physical == other.physical && self.logical == other.logical
    }
}

impl KeyMap {
    /// The built-in default keymap (no user overrides).
    pub fn defaults() -> KeyMap {
        KeyMap::compile(&KeyBindings::default())
    }

    /// Compile a keymap from user `[keys]` bindings layered over the defaults.
    /// Non-panicking: an invalid chord string is dropped with a warning; a
    /// control-byte-shadowing or no-modifier-printable bind is rejected with a
    /// warning; `open_palette` is re-inserted if the user locked it out.
    pub fn compile(bindings: &KeyBindings) -> KeyMap {
        let mut km = KeyMap {
            physical: HashMap::new(),
            logical: HashMap::new(),
            by_action: Vec::new(),
            warnings: Vec::new(),
        };

        for action in BindableAction::ALL {
            let chords: Vec<Chord> = match action.user_spec(bindings) {
                Some(spec) => km.parse_user_chords(action, spec),
                None => action.default_chords(),
            };
            for ch in &chords {
                km.add_chord(action, ch, false);
            }
            km.by_action.push((action, chords));
        }

        // Reserved: `open_palette` must stay reachable (it reaches every command,
        // incl. "Reset keybindings"). If unbound or collided away, force-restore
        // its default.
        if !km.contains_action(&KeyAction::OpenPalette) {
            let defaults = BindableAction::OpenPalette.default_chords();
            for ch in &defaults {
                km.add_chord(BindableAction::OpenPalette, ch, true);
            }
            km.warnings.push(
                "open_palette is reserved and cannot be unbound — restored its default".to_string(),
            );
            // Reflect the restored chords in the display table.
            if let Some(entry) = km
                .by_action
                .iter_mut()
                .find(|(a, _)| *a == BindableAction::OpenPalette)
            {
                entry.1 = defaults;
            }
        }

        km.warnings.dedup();
        km
    }

    /// Resolve a key event to an app action, or `None` when unmapped (the caller
    /// falls through to raw PTY encoding). LOGICAL is checked before PHYSICAL,
    /// mirroring `decide_key`'s font-before-letter precedence.
    pub fn lookup(&self, m: Mods, physical: PhysicalKey, logical: &Key) -> Option<KeyAction> {
        if let Key::Character(s) = logical {
            let key = smol_lower(s);
            if let Some(a) = self.logical.get(&(m, key)) {
                return Some(a.clone());
            }
        }
        if let PhysicalKey::Code(code) = physical {
            if let Some(a) = self.physical.get(&(m, code)) {
                return Some(a.clone());
            }
        }
        None
    }

    /// Compile warnings for the caller to surface.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The current chord(s) for an action, pretty-formatted (symbols) for the
    /// help overlay. Empty when the action is unbound.
    pub fn pretty_chords(&self, action: BindableAction) -> Vec<String> {
        self.by_action
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, chords)| chords.iter().map(|c| c.pretty()).collect())
            .unwrap_or_default()
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn contains_action(&self, ka: &KeyAction) -> bool {
        self.physical.values().any(|v| v == ka) || self.logical.values().any(|v| v == ka)
    }

    /// Insert a chord's slots into the maps, expanding the Alt/Shift-insensitive
    /// variants for defaults. `force` overwrites on conflict (reserved restore).
    fn add_chord(&mut self, action: BindableAction, ch: &Chord, force: bool) {
        let ka = action.key_action();
        // Enumerate the modifier variants (Alt / Shift insensitivity for defaults).
        let mut variants: Vec<Mods> = vec![ch.mods];
        if ch.alt_insensitive {
            let extra: Vec<Mods> = variants
                .iter()
                .map(|m| Mods { alt: !m.alt, ..*m })
                .collect();
            variants.extend(extra);
        }
        if ch.shift_insensitive {
            let extra: Vec<Mods> = variants
                .iter()
                .map(|m| Mods { shift: !m.shift, ..*m })
                .collect();
            variants.extend(extra);
        }
        for m in variants {
            match &ch.key {
                KeyMatch::Phys(code) => self.put_phys(action, m, *code, &ka, force),
                KeyMatch::Logical { chars, phys_fallback } => {
                    for cc in chars {
                        self.put_logical(action, m, smol_lower(cc), &ka, force);
                    }
                    if let Some(fb) = phys_fallback {
                        self.put_phys(action, m, *fb, &ka, force);
                    }
                }
            }
        }
    }

    fn put_phys(&mut self, action: BindableAction, m: Mods, code: KeyCode, ka: &KeyAction, force: bool) {
        if let Some(existing) = self.physical.get(&(m, code)) {
            if existing == ka {
                return; // same action already occupies this slot
            }
            if !force {
                self.warnings.push(format!(
                    "keybinding conflict on {}: {} is ignored (already bound)",
                    pretty_slot_phys(m, code),
                    action.name(),
                ));
                return;
            }
        }
        // Cross-kind: a logical entry for this key's US char under the same mods.
        if let Some(ch) = us_char(code) {
            if let Some(other) = self.logical.get(&(m, ch)) {
                if other != ka {
                    self.warnings.push(format!(
                        "keybinding for {} shadows a logical binding on the same chord",
                        action.name()
                    ));
                }
            }
        }
        self.physical.insert((m, code), ka.clone());
    }

    fn put_logical(&mut self, action: BindableAction, m: Mods, ch: SmolStr, ka: &KeyAction, force: bool) {
        if let Some(existing) = self.logical.get(&(m, ch.clone())) {
            if existing == ka {
                return;
            }
            if !force {
                self.warnings.push(format!(
                    "keybinding conflict on {}+'{}': {} is ignored (already bound)",
                    pretty_mods(m),
                    ch,
                    action.name(),
                ));
                return;
            }
        }
        // Cross-kind: a physical entry for this char's US position under the same mods.
        if let Some(code) = us_phys(&ch) {
            if let Some(other) = self.physical.get(&(m, code)) {
                if other != ka {
                    self.warnings.push(format!(
                        "keybinding for {} shadows a physical binding on the same chord",
                        action.name()
                    ));
                }
            }
        }
        self.logical.insert((m, ch), ka.clone());
    }

    /// Parse a user `[keys]` value into accepted chords (invalid / unsafe chords
    /// are dropped with a warning; `""`/`[]` yields no chords → explicitly unbound).
    fn parse_user_chords(&mut self, action: BindableAction, spec: &ChordSpec) -> Vec<Chord> {
        let mut out = Vec::new();
        for raw in spec.chords() {
            let s = raw.trim();
            if s.is_empty() {
                continue; // "" = explicitly unbound
            }
            match parse_chord(s) {
                Ok(chord) => {
                    if let Some(reason) = chord_reject_reason(&chord) {
                        self.warnings.push(format!(
                            "keybinding '{}' for {} rejected: {}",
                            s,
                            action.name(),
                            reason
                        ));
                    } else {
                        out.push(chord);
                    }
                }
                Err(e) => self.warnings.push(format!(
                    "invalid keybinding '{}' for {}: {}",
                    s,
                    action.name(),
                    e
                )),
            }
        }
        out
    }
}

/// Lowercase a logical character key for case-folded matching.
fn smol_lower(s: &SmolStr) -> SmolStr {
    let lowered = s.to_lowercase();
    if lowered == s.as_str() {
        s.clone()
    } else {
        SmolStr::new(lowered)
    }
}

/// Would this user chord shadow a needed terminal control byte or lock out
/// typing? Returns the rejection reason, or `None` when safe.
fn chord_reject_reason(ch: &Chord) -> Option<String> {
    // A no-modifier bind shadows whatever the key normally sends — printable
    // chars, but ALSO Enter/Tab/Space/Backspace/Escape and the arrow/nav keys a
    // TUI needs. Only F-keys are safe to bind bare; everything else would lock
    // that key out of the shell.
    if ch.mods.is_empty() {
        let ok_bare = matches!(&ch.key, KeyMatch::Phys(code) if is_fkey(*code));
        if !ok_bare {
            return Some("bindings need a modifier (only F-keys may be bound bare)".to_string());
        }
    }
    // Ctrl-only chord on a C0 control-byte producer → would kill SIGINT/EOF/ESC/…
    if ch.mods.ctrl_only() {
        let shadows = match &ch.key {
            KeyMatch::Phys(code) => is_ctrl_byte_key(*code),
            KeyMatch::Logical { chars, .. } => chars.iter().any(|c| is_ctrl_byte_char(c)),
        };
        if shadows {
            return Some(
                "would shadow a terminal control byte (Ctrl+letter / Ctrl+Space/[/\\/]//)"
                    .to_string(),
            );
        }
    }
    None
}

/// F1..F24 — the only keys safe to bind WITHOUT a modifier (every other bare key
/// shadows something the shell/TUI needs: text, Enter/Tab/Space, arrows, nav).
fn is_fkey(code: KeyCode) -> bool {
    use KeyCode::*;
    matches!(
        code,
        F1 | F2 | F3 | F4 | F5 | F6 | F7 | F8 | F9 | F10 | F11 | F12 | F13 | F14 | F15 | F16 | F17
            | F18 | F19 | F20 | F21 | F22 | F23 | F24
    )
}

fn is_ctrl_byte_key(code: KeyCode) -> bool {
    use KeyCode::*;
    matches!(
        code,
        KeyA | KeyB | KeyC | KeyD | KeyE | KeyF | KeyG | KeyH | KeyI | KeyJ | KeyK | KeyL | KeyM
            | KeyN | KeyO | KeyP | KeyQ | KeyR | KeyS | KeyT | KeyU | KeyV | KeyW | KeyX | KeyY
            | KeyZ
            | Space
            | BracketLeft
            | Backslash
            | BracketRight
            | Slash
    )
}

fn is_ctrl_byte_char(c: &str) -> bool {
    matches!(c, "/" | "[" | "\\" | "]") || c.chars().all(|ch| ch.is_ascii_alphabetic())
}

// ── chord parsing (user input) ───────────────────────────────────────────────

/// Parse a user chord string like `"Ctrl+Shift+T"` or `"Cmd+,"`.
fn parse_chord(s: &str) -> Result<Chord, String> {
    let (mod_toks, key_tok) = split_chord(s).ok_or_else(|| "empty chord".to_string())?;

    let mut mods = Mods::default();
    for t in mod_toks {
        let t = t.trim();
        if t.is_empty() {
            return Err("empty modifier".to_string());
        }
        match t.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => set_once(&mut mods.ctrl, "Ctrl")?,
            "shift" => set_once(&mut mods.shift, "Shift")?,
            "alt" | "option" | "opt" => set_once(&mut mods.alt, "Alt")?,
            "super" | "cmd" | "command" | "win" | "meta" => set_once(&mut mods.super_, "Super")?,
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }

    let key = parse_key(&key_tok, mods)?;
    Ok(Chord::exact(mods, key))
}

fn set_once(flag: &mut bool, name: &str) -> Result<(), String> {
    if *flag {
        return Err(format!("duplicate modifier '{name}'"));
    }
    *flag = true;
    Ok(())
}

/// Split `"Ctrl+Shift+T"` → (["Ctrl","Shift"], "T"), handling a literal trailing
/// `+` key (`"Ctrl++"` → (["Ctrl"], "+")).
fn split_chord(s: &str) -> Option<(Vec<&str>, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(head) = s.strip_suffix('+') {
        // Trailing '+' is the KEY; strip the separator '+' that precedes it.
        let head = head.strip_suffix('+').unwrap_or(head);
        let mods: Vec<&str> = if head.is_empty() { Vec::new() } else { head.split('+').collect() };
        return Some((mods, "+".to_string()));
    }
    let mut parts: Vec<&str> = s.split('+').collect();
    let key = parts.pop().unwrap().to_string();
    Some((parts, key))
}

fn parse_key(tok: &str, mods: Mods) -> Result<KeyMatch, String> {
    let t = tok.trim();
    if t.is_empty() {
        return Err("missing key".to_string());
    }

    // Single ASCII letter.
    if t.len() == 1 && t.chars().next().unwrap().is_ascii_alphabetic() {
        let ch = t.chars().next().unwrap().to_ascii_lowercase();
        let code = letter_keycode(ch);
        // Super-letter resolves by logical char (macOS label convention).
        if mods.super_ {
            return Ok(KeyMatch::Logical {
                chars: vec![SmolStr::new(ch.to_string())],
                phys_fallback: Some(code),
            });
        }
        return Ok(KeyMatch::Phys(code));
    }

    // Single ASCII digit.
    if t.len() == 1 && t.chars().next().unwrap().is_ascii_digit() {
        let ch = t.chars().next().unwrap();
        if ch == '0' {
            return Ok(KeyMatch::Logical {
                chars: vec![SmolStr::new("0")],
                phys_fallback: Some(KeyCode::Digit0),
            });
        }
        return Ok(KeyMatch::Phys(digit_keycode(ch)));
    }

    // Symbol (word or single char).
    if let Some(km) = symbol_keymatch(t) {
        return Ok(km);
    }

    // Named key (word).
    if let Some(code) = named_keycode(t) {
        return Ok(KeyMatch::Phys(code));
    }

    Err(format!("unknown key '{t}'"))
}

/// Map a symbol token (word form like `Plus`/`Comma`, or the literal char) to a
/// logical keymatch with a US physical fallback.
fn symbol_keymatch(t: &str) -> Option<KeyMatch> {
    let lower = t.to_ascii_lowercase();
    let (chars, phys): (Vec<&str>, KeyCode) = match lower.as_str() {
        "plus" | "equal" | "equals" | "=" | "+" => (vec!["=", "+"], KeyCode::Equal),
        "minus" | "dash" | "-" => (vec!["-"], KeyCode::Minus),
        "underscore" | "_" => (vec!["_"], KeyCode::Minus),
        "comma" | "," => (vec![","], KeyCode::Comma),
        "period" | "dot" | "." => (vec!["."], KeyCode::Period),
        "slash" | "/" => (vec!["/"], KeyCode::Slash),
        "backslash" | "\\" => (vec!["\\"], KeyCode::Backslash),
        "semicolon" | ";" => (vec![";"], KeyCode::Semicolon),
        "quote" | "apostrophe" | "'" => (vec!["'"], KeyCode::Quote),
        "backquote" | "grave" | "backtick" | "`" => (vec!["`"], KeyCode::Backquote),
        "bracketleft" | "leftbracket" | "[" => (vec!["["], KeyCode::BracketLeft),
        "bracketright" | "rightbracket" | "]" => (vec!["]"], KeyCode::BracketRight),
        _ => return None,
    };
    Some(KeyMatch::Logical {
        chars: chars.into_iter().map(SmolStr::new).collect(),
        phys_fallback: Some(phys),
    })
}

fn named_keycode(t: &str) -> Option<KeyCode> {
    use KeyCode::*;
    let code = match t.to_ascii_lowercase().as_str() {
        "tab" => Tab,
        "enter" | "return" => Enter,
        "escape" | "esc" => Escape,
        "space" => Space,
        "backspace" => Backspace,
        "delete" | "del" => Delete,
        "insert" | "ins" => Insert,
        "home" => Home,
        "end" => End,
        "pageup" | "pgup" => PageUp,
        "pagedown" | "pgdn" => PageDown,
        "up" | "arrowup" => ArrowUp,
        "down" | "arrowdown" => ArrowDown,
        "left" | "arrowleft" => ArrowLeft,
        "right" | "arrowright" => ArrowRight,
        other => return fkey_keycode(other),
    };
    Some(code)
}

fn fkey_keycode(t: &str) -> Option<KeyCode> {
    use KeyCode::*;
    // F1..F24 (all exist in winit 0.30).
    let n: u32 = t.strip_prefix('f')?.parse().ok()?;
    Some(match n {
        1 => F1, 2 => F2, 3 => F3, 4 => F4, 5 => F5, 6 => F6, 7 => F7, 8 => F8,
        9 => F9, 10 => F10, 11 => F11, 12 => F12, 13 => F13, 14 => F14, 15 => F15,
        16 => F16, 17 => F17, 18 => F18, 19 => F19, 20 => F20, 21 => F21, 22 => F22,
        23 => F23, 24 => F24,
        _ => return None,
    })
}

// ── keycode <-> token tables ─────────────────────────────────────────────────

fn letter_keycode(ch: char) -> KeyCode {
    use KeyCode::*;
    match ch {
        'a' => KeyA, 'b' => KeyB, 'c' => KeyC, 'd' => KeyD, 'e' => KeyE, 'f' => KeyF,
        'g' => KeyG, 'h' => KeyH, 'i' => KeyI, 'j' => KeyJ, 'k' => KeyK, 'l' => KeyL,
        'm' => KeyM, 'n' => KeyN, 'o' => KeyO, 'p' => KeyP, 'q' => KeyQ, 'r' => KeyR,
        's' => KeyS, 't' => KeyT, 'u' => KeyU, 'v' => KeyV, 'w' => KeyW, 'x' => KeyX,
        'y' => KeyY, _ => KeyZ,
    }
}

fn digit_keycode(ch: char) -> KeyCode {
    use KeyCode::*;
    match ch {
        '1' => Digit1, '2' => Digit2, '3' => Digit3, '4' => Digit4, '5' => Digit5,
        '6' => Digit6, '7' => Digit7, '8' => Digit8, '9' => Digit9, _ => Digit0,
    }
}

/// Canonical word name for a keycode (serialization).
fn keycode_word(code: KeyCode) -> &'static str {
    use KeyCode::*;
    match code {
        KeyA => "A", KeyB => "B", KeyC => "C", KeyD => "D", KeyE => "E", KeyF => "F",
        KeyG => "G", KeyH => "H", KeyI => "I", KeyJ => "J", KeyK => "K", KeyL => "L",
        KeyM => "M", KeyN => "N", KeyO => "O", KeyP => "P", KeyQ => "Q", KeyR => "R",
        KeyS => "S", KeyT => "T", KeyU => "U", KeyV => "V", KeyW => "W", KeyX => "X",
        KeyY => "Y", KeyZ => "Z",
        Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
        Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
        Comma => "Comma", Period => "Period", Minus => "Minus", Equal => "Equal",
        Slash => "Slash", Backslash => "Backslash", Semicolon => "Semicolon",
        Quote => "Quote", Backquote => "Backquote", BracketLeft => "BracketLeft",
        BracketRight => "BracketRight",
        Tab => "Tab", Enter => "Enter", Escape => "Escape", Space => "Space",
        Backspace => "Backspace", Delete => "Delete", Insert => "Insert", Home => "Home",
        End => "End", PageUp => "PageUp", PageDown => "PageDown",
        ArrowUp => "Up", ArrowDown => "Down", ArrowLeft => "Left", ArrowRight => "Right",
        F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5", F6 => "F6", F7 => "F7",
        F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
        _ => "Unknown",
    }
}

/// Human-facing pretty name for a keycode (symbols where natural).
fn keycode_pretty(code: KeyCode) -> String {
    use KeyCode::*;
    let s = match code {
        Comma => ",", Period => ".", Minus => "-", Equal => "=", Slash => "/",
        Backslash => "\\", Semicolon => ";", Quote => "'", Backquote => "`",
        BracketLeft => "[", BracketRight => "]",
        _ => return keycode_word(code).to_string(),
    };
    s.to_string()
}

/// US-layout character produced by a physical key (for cross-kind conflict scan).
fn us_char(code: KeyCode) -> Option<SmolStr> {
    use KeyCode::*;
    let s = match code {
        KeyA => "a", KeyB => "b", KeyC => "c", KeyD => "d", KeyE => "e", KeyF => "f",
        KeyG => "g", KeyH => "h", KeyI => "i", KeyJ => "j", KeyK => "k", KeyL => "l",
        KeyM => "m", KeyN => "n", KeyO => "o", KeyP => "p", KeyQ => "q", KeyR => "r",
        KeyS => "s", KeyT => "t", KeyU => "u", KeyV => "v", KeyW => "w", KeyX => "x",
        KeyY => "y", KeyZ => "z",
        Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
        Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
        Comma => ",", Period => ".", Minus => "-", Equal => "=", Slash => "/",
        Backslash => "\\", Semicolon => ";", Quote => "'", Backquote => "`",
        BracketLeft => "[", BracketRight => "]",
        _ => return None,
    };
    Some(SmolStr::new(s))
}

/// US-layout physical key for a (possibly shifted) character (cross-kind scan).
fn us_phys(ch: &str) -> Option<KeyCode> {
    let c = ch.chars().next()?;
    if ch.chars().count() != 1 {
        return None;
    }
    let lc = c.to_ascii_lowercase();
    if lc.is_ascii_alphabetic() {
        return Some(letter_keycode(lc));
    }
    use KeyCode::*;
    Some(match c {
        '1' | '!' => Digit1, '2' | '@' => Digit2, '3' | '#' => Digit3, '4' | '$' => Digit4,
        '5' | '%' => Digit5, '6' | '^' => Digit6, '7' | '&' => Digit7, '8' | '*' => Digit8,
        '9' | '(' => Digit9, '0' | ')' => Digit0,
        '=' | '+' => Equal, '-' | '_' => Minus, ',' | '<' => Comma, '.' | '>' => Period,
        '/' | '?' => Slash, '\\' | '|' => Backslash, ';' | ':' => Semicolon,
        '\'' | '"' => Quote, '`' | '~' => Backquote, '[' | '{' => BracketLeft,
        ']' | '}' => BracketRight,
        _ => return None,
    })
}

fn pretty_mods(m: Mods) -> String {
    let mut s = String::new();
    if m.ctrl {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    if m.super_ {
        s.push_str("Super+");
    }
    s.pop(); // trailing '+'
    if s.is_empty() {
        "(none)".to_string()
    } else {
        s
    }
}

fn pretty_slot_phys(m: Mods, code: KeyCode) -> String {
    format!("{}+{}", pretty_mods(m), keycode_pretty(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    // ── parse / serialize ─────────────────────────────────────────────────────

    #[test]
    fn parse_basic_and_roundtrip() {
        let c = parse_chord("Ctrl+Shift+T").unwrap();
        assert_eq!(c.canonical(), "Ctrl+Shift+T");
        // Idempotent under re-parse.
        let c2 = parse_chord(&c.canonical()).unwrap();
        assert_eq!(c2.canonical(), c.canonical());
    }

    #[test]
    fn parse_mod_aliases() {
        assert!(parse_chord("Cmd+P").unwrap().mods.super_);
        assert!(parse_chord("Command+P").unwrap().mods.super_);
        assert!(parse_chord("Win+P").unwrap().mods.super_);
        assert!(parse_chord("Opt+P").unwrap().mods.alt);
        assert!(parse_chord("Option+P").unwrap().mods.alt);
        assert!(parse_chord("Control+P").unwrap().mods.ctrl);
    }

    #[test]
    fn parse_symbol_word_and_char_equivalence() {
        let a = parse_chord("Ctrl+Plus").unwrap();
        let b = parse_chord("Ctrl+Equal").unwrap();
        assert_eq!(a.key, b.key, "Plus == Equal (both engrave the Equal key)");
        let comma_word = parse_chord("Ctrl+Comma").unwrap();
        let comma_char = parse_chord("Ctrl+,").unwrap();
        assert_eq!(comma_word.key, comma_char.key);
    }

    #[test]
    fn parse_trailing_plus_key() {
        let c = parse_chord("Ctrl++").unwrap();
        assert!(c.mods.ctrl && !c.mods.shift);
        // '+' resolves to the Equal key.
        assert_eq!(c.key, parse_chord("Ctrl+Plus").unwrap().key);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_chord("Ctrl+Nonsense").is_err());
        assert!(parse_chord("Bogus+T").is_err());
        assert!(parse_chord("Ctrl+Ctrl+T").is_err(), "duplicate modifier");
        assert!(parse_chord("").is_err());
    }

    // ── conflict / rejection ──────────────────────────────────────────────────

    fn km_with(field: impl FnOnce(&mut KeyBindings)) -> KeyMap {
        let mut b = KeyBindings::default();
        field(&mut b);
        KeyMap::compile(&b)
    }

    #[test]
    fn reject_control_byte_shadow() {
        let km = km_with(|b| b.new_tab = Some(ChordSpec::One("Ctrl+C".to_string())));
        assert!(
            km.warnings().iter().any(|w| w.contains("control byte")),
            "Ctrl+C bind must be rejected: {:?}",
            km.warnings()
        );
        // Ctrl+C still produces the SIGINT byte (not NewTab).
        let a = km.lookup(Mods::new(true, false, false, false), PhysicalKey::Code(KeyCode::KeyC), &ch("c"));
        assert_eq!(a, None, "Ctrl+C must remain unmapped → passes to PTY");
    }

    #[test]
    fn reject_no_modifier_printable() {
        let km = km_with(|b| b.new_tab = Some(ChordSpec::One("T".to_string())));
        assert!(km.warnings().iter().any(|w| w.contains("modifier")));
    }

    #[test]
    fn reject_no_modifier_named_key() {
        // A bare named key (Enter/Tab/Space/…) shadows what the shell needs — reject.
        for k in ["Enter", "Tab", "Space", "Backspace", "Escape", "Up"] {
            let km = km_with(|b| b.new_tab = Some(ChordSpec::One(k.to_string())));
            assert!(
                km.warnings().iter().any(|w| w.contains("modifier")),
                "bare {k} should be rejected"
            );
        }
    }

    #[test]
    fn accept_no_modifier_fkey() {
        // F-keys are the one class safe to bind bare.
        let km = km_with(|b| b.new_tab = Some(ChordSpec::One("F5".to_string())));
        assert!(
            !km.warnings().iter().any(|w| w.contains("modifier")),
            "bare F5 should be accepted"
        );
    }

    #[test]
    fn conflict_two_actions_same_chord() {
        // Bind BOTH new_tab and close_tab to Ctrl+Shift+G.
        let km = km_with(|b| {
            b.new_tab = Some(ChordSpec::One("Ctrl+Shift+G".to_string()));
            b.close_tab = Some(ChordSpec::One("Ctrl+Shift+G".to_string()));
        });
        assert!(km.warnings().iter().any(|w| w.contains("conflict")));
        // new_tab is earlier in ALL → it wins the slot.
        let a = km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(KeyCode::KeyG), &ch("G"));
        assert_eq!(a, Some(KeyAction::NewTab));
    }

    #[test]
    fn conflict_cross_physical_logical() {
        // A physical-letter bind vs a logical-symbol bind that share a US slot.
        // Bind copy to physical KeyG and search to logical "g" won't share; use a
        // symbol: bind font_up to "Ctrl+Slash" (logical "/") while another binds a
        // physical Slash → cross-kind warning.
        let km = km_with(|b| {
            b.font_up = Some(ChordSpec::One("Ctrl+Shift+Slash".to_string()));
            b.font_down = Some(ChordSpec::One("Ctrl+Shift+Slash".to_string()));
        });
        assert!(
            km.warnings().iter().any(|w| w.contains("conflict") || w.contains("shadows")),
            "{:?}",
            km.warnings()
        );
    }

    #[test]
    fn reserved_palette_restored_when_unbound() {
        let km = km_with(|b| b.open_palette = Some(ChordSpec::One(String::new())));
        assert!(km.warnings().iter().any(|w| w.contains("reserved")));
        let a = km.lookup(
            Mods::new(true, true, false, false),
            PhysicalKey::Code(KeyCode::KeyP),
            &ch("P"),
        );
        assert_eq!(a, Some(KeyAction::OpenPalette), "palette default re-inserted");
    }

    #[test]
    fn custom_remap_changes_binding_and_frees_default() {
        let km = km_with(|b| b.new_tab = Some(ChordSpec::One("Ctrl+Shift+G".to_string())));
        // New chord fires.
        assert_eq!(
            km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(KeyCode::KeyG), &ch("G")),
            Some(KeyAction::NewTab)
        );
        // Old default chord is freed (unmapped → passes to PTY).
        assert_eq!(
            km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(KeyCode::KeyT), &ch("T")),
            None
        );
    }

    #[test]
    fn empty_string_unbinds() {
        let km = km_with(|b| b.detach_tab = Some(ChordSpec::One(String::new())));
        assert_eq!(
            km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(KeyCode::KeyD), &ch("D")),
            None
        );
    }

    #[test]
    fn array_binds_multiple_chords() {
        let km = km_with(|b| {
            b.new_tab = Some(ChordSpec::Many(vec![
                "Ctrl+Shift+G".to_string(),
                "Ctrl+Shift+N".to_string(),
            ]));
        });
        for code in [KeyCode::KeyG, KeyCode::KeyN] {
            assert_eq!(
                km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(code), &ch("x")),
                Some(KeyAction::NewTab)
            );
        }
    }

    // ── layout independence (remapped font still logical) ─────────────────────

    #[test]
    fn remapped_font_up_still_logical_on_turkish_q() {
        // font_up = "Ctrl+Plus" keeps the logical-char behavior: on Turkish-Q the
        // '+' engraved key is at a different physical position, but the logical
        // char resolves.
        let km = km_with(|b| b.font_up = Some(ChordSpec::One("Ctrl+Plus".to_string())));
        let a = km.lookup(
            Mods::new(true, false, false, false),
            PhysicalKey::Code(KeyCode::BracketRight), // some other physical position
            &ch("+"),
        );
        assert_eq!(a, Some(KeyAction::FontUp));
    }

    #[test]
    fn defaults_have_no_super_entries_on_linux() {
        let km = KeyMap::defaults();
        // On Linux, no Super chord is seeded (bare Super stays WM territory).
        #[cfg(not(target_os = "macos"))]
        {
            let a = km.lookup(Mods::new(false, false, false, true), PhysicalKey::Code(KeyCode::KeyC), &ch("c"));
            assert_eq!(a, None);
        }
        let _ = km;
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cmd_defaults_present_and_shift_agnostic() {
        let km = KeyMap::defaults();
        let sup = Mods::new(false, false, false, true);
        let sup_shift = Mods::new(false, true, false, true);
        // Cmd+C and Cmd+Shift+C both copy.
        assert_eq!(km.lookup(sup, PhysicalKey::Code(KeyCode::KeyC), &ch("c")), Some(KeyAction::Copy));
        assert_eq!(km.lookup(sup_shift, PhysicalKey::Code(KeyCode::KeyC), &ch("C")), Some(KeyAction::Copy));
        // Cmd+P and Cmd+Shift+P both open the palette.
        assert_eq!(km.lookup(sup, PhysicalKey::Code(KeyCode::KeyP), &ch("p")), Some(KeyAction::OpenPalette));
        assert_eq!(km.lookup(sup_shift, PhysicalKey::Code(KeyCode::KeyP), &ch("P")), Some(KeyAction::OpenPalette));
        // Cmd+A / Cmd+Q new variants.
        assert_eq!(km.lookup(sup, PhysicalKey::Code(KeyCode::KeyA), &ch("a")), Some(KeyAction::SelectAll));
        assert_eq!(km.lookup(sup, PhysicalKey::Code(KeyCode::KeyQ), &ch("q")), Some(KeyAction::Quit));
    }
}
