//! Configurable keybindings: a chord grammar (parse + serialize), a compiled
//! [`KeyMap`] (small hashmaps — logical, physical, physical-fallback), and
//! [`KeyMap::lookup`], which `input::chord_action` calls — for the key path
//! (`decide_key_event`) and the overlays and menus alike — to resolve the
//! discrete app-command chords.
//!
//! Letter and symbol chords follow the key LABEL: they match the produced
//! (case-folded) character, so Ctrl+Shift+C copies on Dvorak / AZERTY /
//! Turkish-F exactly where the control-byte path already sends Ctrl+C = 0x03.
//! Their US physical position is only a fallback for keys that produce no ASCII
//! character (Cyrillic / Greek layouts, Turkish `ı`, dead / unidentified keys).
//! Named keys and the digit row (Ctrl+1…9 tab jumps) match by position — except
//! the Menu key, which matches the key the layout calls Menu (see
//! [`KeyMatch::Named`]).
//!
//! The Ctrl+Shift / tab-nav chords never tested Alt, so their defaults are
//! Alt-INSENSITIVE; the macOS `Cmd` chords never tested Shift, so their
//! defaults are Shift-INSENSITIVE. User-supplied chords use exact matching.

use std::collections::HashMap;

use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey, SmolStr};

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
    /// Match `event.physical_key` (layout-invariant position). Used for named
    /// keys and digits 1-9.
    Phys(KeyCode),
    /// Match the produced logical character(s) (layout-following). The US
    /// physical position is consulted only when the event's logical key is not
    /// an ASCII character. Used for letters, symbols and `0`.
    Logical {
        chars: Vec<SmolStr>,
        phys_fallback: Option<KeyCode>,
    },
    /// Match the event's logical NAMED key, wherever it sits. Only the Menu
    /// key: xkb options commonly give its position another job — Compose
    /// (`compose:menu`), Right Ctrl (`ctrl:menu_rctrl`) — and that key must
    /// keep doing what the layout says, while a key the layout made Menu
    /// opens the menu.
    Named(NamedKey),
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
            KeyMatch::Named(named) => named_word(*named).to_string(),
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

    /// Compact glyph form for a menu hint: Shift ⇧, Ctrl ⌃, Alt ⌥, Cmd ⌘ (Super
    /// ❖ off macOS), then the key — the order the menus have always used.
    fn menu_glyphs(&self) -> String {
        let mut s = String::new();
        if self.mods.shift {
            s.push('⇧');
        }
        if self.mods.ctrl {
            s.push('⌃');
        }
        if self.mods.alt {
            s.push('⌥');
        }
        if self.mods.super_ {
            s.push(if cfg!(target_os = "macos") { '⌘' } else { '❖' });
        }
        match &self.key {
            KeyMatch::Phys(code) => s.push_str(&keycode_menu_glyph(*code)),
            KeyMatch::Named(named) => s.push_str(named_word(*named)),
            KeyMatch::Logical { chars, phys_fallback } => match (chars.first(), phys_fallback) {
                (Some(c), _) => s.push_str(&c.to_uppercase()),
                (None, Some(code)) => s.push_str(&keycode_menu_glyph(*code)),
                (None, None) => {}
            },
        }
        s
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
            KeyMatch::Named(named) => s.push_str(named_word(*named)),
            KeyMatch::Logical { chars, .. } => {
                if let Some(c) = chars.first() {
                    // Letters are stored case-folded; show them as engraved.
                    if c.len() == 1 && c.as_bytes()[0].is_ascii_alphabetic() {
                        s.push_str(&c.to_ascii_uppercase());
                    } else {
                        s.push_str(c);
                    }
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
    HintMode,
    CopyMode,
    RunSelection,
    /// Declared after every older action so it can never win a slot from one.
    ToggleFullscreen,
    /// Host scrollback paging (v0.26: Shift+PageUp/PageDown; plain Page keys
    /// now reach the program). Appended LAST, like every new action.
    ScrollPageUp,
    ScrollPageDown,
    /// Step to the next / previous theme (picked and saved, named in a pill).
    /// NO default chord — bind one in `[keys]` (`next_theme = "Ctrl+Alt+T"`);
    /// the command palette has "Next theme" / "Previous theme" either way.
    NextTheme,
    PrevTheme,
    /// Open the window's context menu at the text cursor (the Menu key; the
    /// menus then take arrows / Enter). Appended LAST, like every new action.
    ContextMenu,
}

impl BindableAction {
    pub const ALL: [BindableAction; 37] = [
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
        BindableAction::HintMode,
        BindableAction::CopyMode,
        BindableAction::RunSelection,
        BindableAction::ToggleFullscreen,
        BindableAction::ScrollPageUp,
        BindableAction::ScrollPageDown,
        BindableAction::NextTheme,
        BindableAction::PrevTheme,
        BindableAction::ContextMenu,
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
            HintMode => "hint_mode",
            CopyMode => "copy_mode",
            RunSelection => "run_selection",
            ToggleFullscreen => "toggle_fullscreen",
            ScrollPageUp => "scroll_page_up",
            ScrollPageDown => "scroll_page_down",
            NextTheme => "next_theme",
            PrevTheme => "prev_theme",
            ContextMenu => "context_menu",
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
            HintMode => KeyAction::HintMode,
            CopyMode => KeyAction::CopyMode,
            RunSelection => KeyAction::RunSelection,
            ToggleFullscreen => KeyAction::ToggleFullscreen,
            ScrollPageUp => KeyAction::ScrollPageUp,
            ScrollPageDown => KeyAction::ScrollPageDown,
            NextTheme => KeyAction::NextTheme,
            PrevTheme => KeyAction::PrevTheme,
            ContextMenu => KeyAction::ContextMenu,
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
            HintMode => &b.hint_mode,
            CopyMode => &b.copy_mode,
            RunSelection => &b.run_selection,
            ToggleFullscreen => &b.toggle_fullscreen,
            ScrollPageUp => &b.scroll_page_up,
            ScrollPageDown => &b.scroll_page_down,
            NextTheme => &b.next_theme,
            PrevTheme => &b.prev_theme,
            ContextMenu => &b.context_menu,
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
                    ctrl_shift(letter('o')),
                ];
                push_cmd(&mut v, cmd_symbol(logical_only(",")));
                v
            }
            OpenPalette => {
                let mut v = vec![ctrl_shift(letter('p'))];
                push_cmd(&mut v, cmd_letter("p"));
                v
            }
            NewTab => {
                let mut v = vec![ctrl_shift(letter('t'))];
                push_cmd(&mut v, cmd_letter("t"));
                v
            }
            CloseTab => {
                let mut v = vec![ctrl_shift(letter('w'))];
                push_cmd(&mut v, cmd_letter("w"));
                v
            }
            DetachTab => vec![ctrl_shift(letter('d'))],
            SearchToggle => vec![ctrl_shift(letter('f'))],
            PrevPrompt => vec![ctrl_shift(letter('z'))],
            NextPrompt => vec![ctrl_shift(letter('x'))],
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
                let mut v = vec![ctrl_shift(letter('c'))];
                push_cmd(&mut v, cmd_letter("c"));
                v
            }
            Paste => {
                let mut v = vec![
                    ctrl_shift(letter('v')),
                    // Shift+Insert is EXACT (today: shift && !ctrl && !alt).
                    Chord::exact(Mods::new(false, true, false, false), KeyMatch::Phys(KeyCode::Insert)),
                ];
                push_cmd(&mut v, cmd_letter("v"));
                v
            }
            // Transparency lives on Ctrl+Alt+'±' (v0.26; was Ctrl+Shift): that
            // freed Ctrl+Shift+'+' for font zoom — on Turkish-Q / Swiss /
            // Hungarian layouts '+' only exists WITH Shift — and gave Ctrl+_
            // (readline / emacs undo, 0x1f) back to the shell. AltGr is not Alt
            // (XKB reports it as ISO_Level3), so no AltGr character is shadowed.
            OpacityUp => vec![shift_loose(Mods::new(true, false, true, false), font_up_keymatch())],
            OpacityDown => {
                vec![shift_loose(Mods::new(true, false, true, false), opacity_down_keymatch())]
            }
            // Ctrl + whatever key types '+' or '=' zooms in, Shift or not.
            // Alt-SENSITIVE: Ctrl+Alt+'±' is transparency.
            FontUp => {
                let mut v = vec![shift_loose(Mods::new(true, false, false, false), font_up_keymatch())];
                push_cmd(&mut v, cmd_symbol(font_up_keymatch()));
                v
            }
            // Exact Ctrl+'-': Ctrl+Shift+'-' types '_' and must reach the shell
            // as 0x1f; Ctrl+Alt+'-' is transparency.
            FontDown => {
                let mut v = vec![Chord::exact(Mods::new(true, false, false, false), font_down_keymatch())];
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
            // Hint mode (Ctrl+Shift+H) and copy-mode (Ctrl+Shift+Space): both
            // carry Shift (so the control-byte-shadow guard accepts them) and,
            // like SearchToggle/DetachTab, have NO macOS Cmd variant — the
            // Ctrl+Shift chord works on every platform. Both were verified free
            // in the default set (H and Space are unbound Ctrl+Shift slots).
            HintMode => vec![ctrl_shift(letter('h'))],
            CopyMode => vec![ctrl_shift(KeyMatch::Phys(KeyCode::Space))],
            // Run-selection-in-a-new-tab: Ctrl+Shift+Enter — the browser/IDE
            // "run it" chord. Verified free across the whole default set (no
            // other default binds Enter under any modifiers). Standard
            // alt-loose Ctrl+Shift form, so plain Enter, Shift+Enter and
            // Ctrl+Enter still reach the PTY (all collapse to \r today — no
            // kitty keyboard protocol is implemented). Like SearchToggle/
            // DetachTab/HintMode/CopyMode there is NO macOS Cmd companion:
            // the Ctrl+Shift chord works there as-is, and Cmd+Enter is
            // iTerm2's fullscreen muscle memory — deliberately not taken.
            RunSelection => vec![ctrl_shift(KeyMatch::Phys(KeyCode::Enter))],
            // Bare F11 — the universal fullscreen chord, and the ONLY default
            // chord that binds an F-key. Legal bare because
            // `chord_reject_reason` permits F-keys without a modifier
            // (`is_fkey`), and it shadows no control BYTE. It DOES shadow the
            // F11 escape SEQUENCE `\e[23~` (input.rs's F-key encoder), which the
            // keymap lookup runs before — the documented opt-out is
            // `[keys] toggle_fullscreen = ""`.
            //
            // The chord is `exact` (NOT Alt/Shift-loose), so Shift/Ctrl/Alt+F11
            // still reach the PTY as the xterm modified form `\e[23;{m}~` — a
            // second escape hatch for TUIs that want the key.
            //
            // macOS needs a COMPANION chord: there bare F11 is Mission Control's
            // "Show Desktop", and on Apple keyboards without "Use F1, F2… as
            // standard function keys" the physical F11 is Volume Down — the app
            // never sees the key at all. Cmd+Ctrl+F is the macOS fullscreen
            // convention; seeded only under macOS via the `push_cmd` idiom, and
            // the help row uses `all(A::ToggleFullscreen)` so BOTH appear.
            ToggleFullscreen => {
                let mut v = vec![Chord::exact(Mods::default(), KeyMatch::Phys(KeyCode::F11))];
                push_cmd(&mut v, cmd_ctrl_letter("f"));
                v
            }
            // Shift+PageUp/Down: every terminal's host-scrollback chord. Plain
            // PageUp/Down reach the program (fzf's inline Ctrl+R / Ctrl+T, zsh
            // history paging, inline prompt_toolkit apps). Old behaviour:
            // `[keys] scroll_page_up = ["Shift+PageUp", "PageUp"]` (+ down);
            // the bare keys then still go to programs on the alternate screen.
            ScrollPageUp => {
                vec![Chord::exact(Mods::new(false, true, false, false), KeyMatch::Phys(KeyCode::PageUp))]
            }
            ScrollPageDown => {
                vec![Chord::exact(Mods::new(false, true, false, false), KeyMatch::Phys(KeyCode::PageDown))]
            }
            // No default chord (no new default chords this release): theme
            // cycling is a palette command and a `[keys]` opt-in.
            NextTheme | PrevTheme => Vec::new(),
            // The bare Menu key — the PC keyboard's own "context menu" key,
            // and the second default (after F11) bound without a modifier,
            // which `chord_reject_reason` permits for it. The legacy encoders
            // send nothing for it, so the shell loses nothing; a program on
            // the kitty keyboard protocol loses its `CSI 57363 u` (the opt-out
            // is `[keys] context_menu = ""`, as with F11). EXACT, like F11.
            // Matched by the key the LAYOUT calls Menu (`KeyMatch::Named`), so
            // `compose:menu` keeps that key composing. Apple keyboards have no
            // Menu key (winit maps none on macOS): the palette's "Open context
            // menu" or a `[keys]` chord serves there.
            ContextMenu => vec![Chord::exact(Mods::default(), KeyMatch::Named(NamedKey::ContextMenu))],
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

/// `mods` with Shift-don't-care (Alt still exact): for symbols that need Shift
/// on some layouts and not on others ('+' is Shift+4 on Turkish-Q).
fn shift_loose(mods: Mods, k: KeyMatch) -> Chord {
    Chord { mods, key: k, alt_insensitive: false, shift_insensitive: true }
}

/// A letter chord: matches the key LABELED `ch` (any case) on any layout; the
/// US position is only a fallback for non-Latin layouts (see [`KeyMap::lookup`]).
fn letter(ch: char) -> KeyMatch {
    let lc = ch.to_ascii_lowercase();
    KeyMatch::Logical {
        chars: vec![SmolStr::new(lc.to_string())],
        phys_fallback: Some(letter_keycode(lc)),
    }
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

/// A macOS Cmd+Ctrl chord matching the folded logical letter, Shift-insensitive
/// (same convention as [`cmd_letter`]). Only Cmd+Ctrl+F uses it today.
fn cmd_ctrl_letter(ch: &str) -> Chord {
    Chord {
        mods: Mods::new(true, false, false, true),
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

/// How [`KeyMap::add_chord`] treats a slot another action already holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Claim {
    /// A user `[keys]` chord: user chords go in first, so the holder is an
    /// earlier action's user chord — a conflict, reported; this one is dropped.
    User,
    /// A built-in default: it quietly yields the slot (a user chord wins over
    /// every default, whichever action was declared first).
    Default,
    /// The reserved `open_palette` restore: it overwrites.
    Force,
}

/// A compiled, ready-to-query keymap.
pub struct KeyMap {
    /// Position chords (named keys, digits 1-9): always consulted.
    physical: HashMap<(Mods, KeyCode), KeyAction>,
    /// Label chords (letters, symbols, `0`), keyed by the case-folded character.
    logical: HashMap<(Mods, SmolStr), KeyAction>,
    /// The US positions of the label chords, consulted ONLY when the event's
    /// logical key is not an ASCII character (non-Latin layouts, Turkish `ı`,
    /// dead / unidentified keys) — never on a layout that types another ASCII
    /// character there (Dvorak's KeyC is 'J', and Ctrl+Shift+J is not Copy).
    phys_fallback: HashMap<(Mods, KeyCode), KeyAction>,
    /// Logical named-key chords (the Menu key), keyed by the named key the
    /// layout reports — consulted only for a `Key::Named` event.
    named: HashMap<(Mods, NamedKey), KeyAction>,
    /// The compiled chords per action (unexpanded), for help/display.
    by_action: Vec<(BindableAction, Vec<Chord>)>,
    /// Human-readable compile warnings (conflicts / rejected binds / invalid
    /// chords). Surfaced by the caller (GUI users never see stderr).
    warnings: Vec<String>,
}

impl PartialEq for KeyMap {
    fn eq(&self, other: &Self) -> bool {
        // Compare only the resolved maps — by_action/warnings are derived from them.
        self.physical == other.physical
            && self.logical == other.logical
            && self.phys_fallback == other.phys_fallback
            && self.named == other.named
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
    /// warning (an action left with none of its chords keeps its default);
    /// `open_palette` is re-inserted if the user locked it out.
    ///
    /// The user's chords go in first, then the defaults of the actions left
    /// alone — so a remap onto ANOTHER action's default chord takes it whichever
    /// of the two was declared first, and that default quietly yields the slot
    /// (declaration order used to decide: `search_toggle = "Ctrl+Shift+T"` was
    /// dropped as "already bound" while `new_tab = "Ctrl+Shift+F"` worked). Two
    /// USER chords on one slot still conflict: the earlier action wins, the
    /// later one is reported.
    pub fn compile(bindings: &KeyBindings) -> KeyMap {
        let mut km = KeyMap {
            physical: HashMap::new(),
            logical: HashMap::new(),
            phys_fallback: HashMap::new(),
            named: HashMap::new(),
            by_action: Vec::new(),
            warnings: Vec::new(),
        };

        // `None`: the action keeps its defaults (no `[keys]` entry, or one
        // naming no chord that can be used) — they go in after every user chord.
        let mut user: Vec<(BindableAction, Option<Vec<Chord>>)> = Vec::with_capacity(BindableAction::ALL.len());
        for action in BindableAction::ALL {
            let chords = action.user_spec(bindings).as_ref().and_then(|spec| km.parse_user_chords(action, spec));
            for ch in chords.iter().flatten() {
                km.add_chord(action, ch, Claim::User);
            }
            user.push((action, chords));
        }
        let mut by_action: Vec<(BindableAction, Vec<Chord>)> = Vec::with_capacity(user.len());
        for (action, chords) in user {
            let chords = chords.unwrap_or_else(|| {
                let defaults = action.default_chords();
                for ch in &defaults {
                    km.add_chord(action, ch, Claim::Default);
                }
                defaults
            });
            by_action.push((action, chords));
        }

        // Reserved: `open_palette` must stay reachable (it reaches every command,
        // incl. "Reset keybindings"). If unbound or collided away, force-restore
        // its default.
        if !km.contains_action(&KeyAction::OpenPalette) {
            let defaults = BindableAction::OpenPalette.default_chords();
            for ch in &defaults {
                km.add_chord(BindableAction::OpenPalette, ch, Claim::Force);
            }
            km.warnings.push(
                "open_palette is reserved and can't be unbound or taken — restored its default".to_string(),
            );
            // Reflect the restored chords in the display table.
            if let Some(entry) = by_action.iter_mut().find(|(a, _)| *a == BindableAction::OpenPalette) {
                entry.1 = defaults;
            }
        }

        // The help and the menus list a chord only while it still holds its
        // own slot — not a default that yielded it, nor a user chord that lost
        // a conflict (pressing it does something else).
        for (action, chords) in &mut by_action {
            let ka = action.key_action();
            chords.retain(|ch| km.primary_owner(ch) == Some(&ka));
        }
        km.by_action = by_action;

        km.warnings.dedup();
        km
    }

    /// Resolve a key event to an app action, or `None` when unmapped (the caller
    /// falls through to raw PTY encoding): the produced character (label) — or
    /// named key (Menu) — first, then the position chords, then — only for a
    /// key that produced no ASCII character — the label chords' US positions.
    pub fn lookup(&self, m: Mods, physical: PhysicalKey, logical: &Key) -> Option<KeyAction> {
        match logical {
            Key::Character(s) => {
                let key = smol_lower(s);
                if let Some(a) = self.logical.get(&(m, key)) {
                    return Some(a.clone());
                }
            }
            Key::Named(named) => {
                if let Some(a) = self.named.get(&(m, *named)) {
                    return Some(a.clone());
                }
            }
            _ => {}
        }
        if let PhysicalKey::Code(code) = physical {
            if let Some(a) = self.physical.get(&(m, code)) {
                return Some(a.clone());
            }
            let ascii_char = matches!(logical, Key::Character(s) if s.is_ascii());
            if !ascii_char {
                if let Some(a) = self.phys_fallback.get(&(m, code)) {
                    return Some(a.clone());
                }
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

    /// Compact glyph form ("⇧⌃C", "⌘V", "⇧⌃⏎") of an action's PRIMARY chord for
    /// a context-menu hint — "" when the action is unbound, so a `[keys]` remap
    /// or unbind shows up in the menus too. On macOS a Cmd chord (the platform's
    /// menu convention) is preferred over the Ctrl+Shift one when both exist.
    pub fn menu_hint(&self, action: BindableAction) -> String {
        let Some((_, chords)) = self.by_action.iter().find(|(a, _)| *a == action) else {
            return String::new();
        };
        let pick = if cfg!(target_os = "macos") {
            chords.iter().find(|c| c.mods.super_).or(chords.first())
        } else {
            chords.first()
        };
        pick.map(Chord::menu_glyphs).unwrap_or_default()
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn contains_action(&self, ka: &KeyAction) -> bool {
        self.physical.values().any(|v| v == ka)
            || self.logical.values().any(|v| v == ka)
            || self.phys_fallback.values().any(|v| v == ka)
            || self.named.values().any(|v| v == ka)
    }

    /// The action holding a chord's PRIMARY slot: its exact modifiers on its
    /// first label (or its position).
    fn primary_owner(&self, ch: &Chord) -> Option<&KeyAction> {
        match &ch.key {
            KeyMatch::Phys(code) => self.physical.get(&(ch.mods, *code)),
            KeyMatch::Named(named) => self.named.get(&(ch.mods, *named)),
            KeyMatch::Logical { chars, phys_fallback } => match chars.first() {
                Some(c) => self.logical.get(&(ch.mods, smol_lower(c))),
                None => phys_fallback.and_then(|fb| self.phys_fallback.get(&(ch.mods, fb))),
            },
        }
    }

    /// Whether `claim` may take a slot `existing` (another action) holds; a
    /// user chord that may not is reported as a conflict.
    fn may_take(&mut self, claim: Claim, slot: impl FnOnce() -> String, action: BindableAction) -> bool {
        match claim {
            Claim::Force => true,
            Claim::Default => false,
            Claim::User => {
                self.warnings.push(format!(
                    "keybinding conflict on {}: {} is ignored (already bound)",
                    slot(),
                    action.name(),
                ));
                false
            }
        }
    }

    /// Insert a chord's slots into the maps, expanding the Alt/Shift-insensitive
    /// variants for defaults; `claim` decides a slot another action holds.
    fn add_chord(&mut self, action: BindableAction, ch: &Chord, claim: Claim) {
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
                KeyMatch::Phys(code) => self.put_phys(action, m, *code, &ka, claim),
                KeyMatch::Named(named) => self.put_named(action, m, *named, &ka, claim),
                KeyMatch::Logical { chars, phys_fallback } => {
                    for cc in chars {
                        self.put_logical(action, m, smol_lower(cc), &ka, claim);
                    }
                    if let Some(fb) = phys_fallback {
                        self.put_phys_fallback(action, m, *fb, &ka, claim);
                    }
                }
            }
        }
    }

    fn put_named(&mut self, action: BindableAction, m: Mods, named: NamedKey, ka: &KeyAction, claim: Claim) {
        if let Some(existing) = self.named.get(&(m, named)) {
            if existing == ka || !self.may_take(claim, || pretty_slot_named(m, named), action) {
                return;
            }
        }
        self.named.insert((m, named), ka.clone());
    }

    fn put_phys_fallback(&mut self, action: BindableAction, m: Mods, code: KeyCode, ka: &KeyAction, claim: Claim) {
        if let Some(existing) = self.phys_fallback.get(&(m, code)) {
            if existing == ka || !self.may_take(claim, || pretty_slot_phys(m, code), action) {
                return;
            }
        }
        self.phys_fallback.insert((m, code), ka.clone());
    }

    fn put_phys(&mut self, action: BindableAction, m: Mods, code: KeyCode, ka: &KeyAction, claim: Claim) {
        if let Some(existing) = self.physical.get(&(m, code)) {
            // Same action already there, or another one that keeps the slot.
            if existing == ka || !self.may_take(claim, || pretty_slot_phys(m, code), action) {
                return;
            }
        }
        // Cross-kind: a logical entry for this key's US char under the same mods
        // (it is consulted first, so it wins on a US layout). A default yields.
        if let Some(ch) = us_char(code) {
            if let Some(other) = self.logical.get(&(m, ch)) {
                if other != ka {
                    if claim == Claim::Default {
                        return;
                    }
                    self.warnings.push(format!(
                        "keybinding for {} shadows a logical binding on the same chord",
                        action.name()
                    ));
                }
            }
        }
        self.physical.insert((m, code), ka.clone());
    }

    fn put_logical(&mut self, action: BindableAction, m: Mods, ch: SmolStr, ka: &KeyAction, claim: Claim) {
        if let Some(existing) = self.logical.get(&(m, ch.clone())) {
            if existing == ka || !self.may_take(claim, || format!("{}+'{}'", pretty_mods(m), ch), action) {
                return;
            }
        }
        // Cross-kind: a physical entry for this char's US position under the
        // same mods. A default yields to it.
        if let Some(code) = us_phys(&ch) {
            if let Some(other) = self.physical.get(&(m, code)) {
                if other != ka {
                    if claim == Claim::Default {
                        return;
                    }
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
    /// `None` when it names chords but none of them can be used: the action
    /// keeps its default, and the last warning says so — `new_tab = "Ctrl+T"`
    /// (a control byte) used to leave New tab with no shortcut at all.
    fn parse_user_chords(&mut self, action: BindableAction, spec: &ChordSpec) -> Option<Vec<Chord>> {
        let mut out = Vec::new();
        let mut named = false;
        for raw in spec.chords() {
            let s = raw.trim();
            if s.is_empty() {
                continue; // "" = explicitly unbound
            }
            named = true;
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
        if named && out.is_empty() {
            // Every chord it named was reported, the last one just now.
            if let Some(last) = self.warnings.last_mut() {
                last.push_str(&format!(" — {} keeps its default", action.name()));
            }
            return None;
        }
        Some(out)
    }
}

/// Lowercase a logical character key for case-folded matching. Runs on every
/// keystroke, so the common already-lowercase / non-letter case returns the
/// (inline, heap-free) SmolStr without building a String.
fn smol_lower(s: &SmolStr) -> SmolStr {
    if s.chars().all(|c| !c.is_alphabetic() || c.is_lowercase()) {
        return s.clone();
    }
    SmolStr::new(s.to_lowercase())
}

/// Would this user chord shadow a needed terminal control byte or lock out
/// typing? Returns the rejection reason, or `None` when safe.
fn chord_reject_reason(ch: &Chord) -> Option<String> {
    // A no-modifier bind shadows whatever the key normally sends — printable
    // chars, but ALSO Enter/Tab/Space/Backspace/Escape and the arrow/nav keys a
    // TUI needs. Only F-keys, PageUp/PageDown (the pre-v0.26 scroll keys —
    // `scroll_page_up = ["Shift+PageUp", "PageUp"]`; programs on the alternate
    // screen still get them) and the Menu key (it types nothing) may be bound
    // bare; anything else would lock that key out of the shell.
    if ch.mods.is_empty() {
        let ok_bare = match &ch.key {
            KeyMatch::Phys(code) => is_fkey(*code) || matches!(code, KeyCode::PageUp | KeyCode::PageDown),
            KeyMatch::Named(named) => *named == NamedKey::ContextMenu,
            KeyMatch::Logical { .. } => false,
        };
        if !ok_bare {
            return Some(
                "bindings need a modifier (only F-keys, PageUp/PageDown and Menu may be bound bare)"
                    .to_string(),
            );
        }
    }
    // Ctrl-only chord on a C0 control-byte producer → would kill SIGINT/EOF/ESC/…
    if ch.mods.ctrl_only() {
        let shadows = match &ch.key {
            KeyMatch::Phys(code) => is_ctrl_byte_key(*code),
            KeyMatch::Named(_) => false,
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

    let key = parse_key(&key_tok)?;
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

fn parse_key(tok: &str) -> Result<KeyMatch, String> {
    let t = tok.trim();
    if t.is_empty() {
        return Err("missing key".to_string());
    }

    // Single ASCII letter: matched by LABEL (the produced character), with the
    // US position as the non-Latin-layout fallback — like the defaults.
    if t.len() == 1 && t.chars().next().unwrap().is_ascii_alphabetic() {
        return Ok(letter(t.chars().next().unwrap()));
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

    // The Menu key, matched by name (see `KeyMatch::Named`).
    if matches!(t.to_ascii_lowercase().as_str(), "menu" | "contextmenu" | "apps") {
        return Ok(KeyMatch::Named(NamedKey::ContextMenu));
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
        F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12", F13 => "F13",
        F14 => "F14", F15 => "F15", F16 => "F16", F17 => "F17", F18 => "F18", F19 => "F19",
        F20 => "F20", F21 => "F21", F22 => "F22", F23 => "F23", F24 => "F24",
        _ => "Unknown",
    }
}

/// The name of a logical named-key chord's key (serialization, the help
/// overlay and the menu hints alike). Only the Menu key is bindable by name.
fn named_word(named: NamedKey) -> &'static str {
    match named {
        NamedKey::ContextMenu => "Menu",
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

/// Menu-hint glyph for a keycode: the conventional key symbols (⏎ ⇥ ␣ ⌫ ⌦ ⎋
/// arrows) where one exists, the pretty name otherwise.
fn keycode_menu_glyph(code: KeyCode) -> String {
    use KeyCode::*;
    let s = match code {
        Enter => "⏎",
        Tab => "⇥",
        Space => "␣",
        Backspace => "⌫",
        Delete => "⌦",
        Escape => "⎋",
        ArrowUp => "↑",
        ArrowDown => "↓",
        ArrowLeft => "←",
        ArrowRight => "→",
        _ => return keycode_pretty(code),
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

fn pretty_slot_named(m: Mods, named: NamedKey) -> String {
    format!("{}+{}", pretty_mods(m), named_word(named))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    // ── parse / serialize ─────────────────────────────────────────────────────

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn menu_hints_follow_the_live_keymap() {
        // Defaults reproduce the menus' historical glyphs exactly …
        let km = KeyMap::defaults();
        assert_eq!(km.menu_hint(BindableAction::Copy), "⇧⌃C");
        assert_eq!(km.menu_hint(BindableAction::Paste), "⇧⌃V");
        assert_eq!(km.menu_hint(BindableAction::RunSelection), "⇧⌃⏎");
        assert_eq!(km.menu_hint(BindableAction::CloseTab), "⇧⌃W");
        assert_eq!(km.menu_hint(BindableAction::DetachTab), "⇧⌃D");
        assert_eq!(km.menu_hint(BindableAction::SelectAll), "", "no Linux default");
        // … and a [keys] remap / unbind shows up in the menu.
        let b = crate::config::KeyBindings {
            copy: Some(crate::config::ChordSpec::One("Ctrl+Shift+K".to_string())),
            close_tab: Some(crate::config::ChordSpec::Many(vec![])),
            ..Default::default()
        };
        let km = KeyMap::compile(&b);
        assert_eq!(km.menu_hint(BindableAction::Copy), "⇧⌃K");
        assert_eq!(km.menu_hint(BindableAction::CloseTab), "");
    }

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
    fn a_binding_none_of_whose_chords_can_be_used_keeps_the_default() {
        // README's own example, `new_tab = "Ctrl+T"`, was rejected (a control
        // byte) and ALSO left New tab without its default: no shortcut at all.
        let defaults = KeyMap::defaults().pretty_chords(BindableAction::NewTab);
        let one = |s: &str| Some(ChordSpec::One(s.to_string()));
        let many = |v: &[&str]| Some(ChordSpec::Many(v.iter().map(|s| s.to_string()).collect()));
        for spec in [one("Ctrl+T"), one("Ctrl+Bogus"), many(&["Ctrl+T", "Hyper+N", "T"])] {
            let km = km_with(|b| b.new_tab = spec.clone());
            assert_eq!(km.pretty_chords(BindableAction::NewTab), defaults, "{spec:?}");
            let kept: Vec<_> = km.warnings().iter().filter(|w| w.ends_with("new_tab keeps its default")).collect();
            assert_eq!(kept.len(), 1, "{spec:?}: said once, on the last problem: {:?}", km.warnings());
            let cs = Mods::new(true, true, false, false);
            assert_eq!(km.lookup(cs, PhysicalKey::Code(KeyCode::KeyT), &ch("T")), Some(KeyAction::NewTab));
        }
        let km = km_with(|b| b.new_tab = one("Ctrl+T"));
        let ctrl = Mods::new(true, false, false, false);
        assert_eq!(km.lookup(ctrl, PhysicalKey::Code(KeyCode::KeyT), &ch("t")), None, "Ctrl+T stays the shell's");
        // One usable chord replaces the default; the rejected one is reported.
        let km = km_with(|b| b.new_tab = many(&["Ctrl+T", "Ctrl+Shift+N"]));
        assert_eq!(km.pretty_chords(BindableAction::NewTab), ["Ctrl+Shift+N"]);
        assert!(km.warnings().iter().all(|w| !w.contains("keeps its default")), "{:?}", km.warnings());
        // `""` / `[]` still unbind, silently.
        for spec in [one(""), many(&[]), many(&["", " "])] {
            let km = km_with(|b| b.new_tab = spec.clone());
            assert!(km.pretty_chords(BindableAction::NewTab).is_empty(), "{spec:?}");
            assert!(km.warnings().is_empty(), "{spec:?}: {:?}", km.warnings());
        }
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
    fn every_bindable_fkey_is_named_in_the_help_and_the_menus() {
        // F13–F24 parse and bind like F1–F12 (Apple keyboards have F13–F19;
        // remappers emit them), so the help overlay, the menu hints and the
        // conflict warnings must name them — they printed "Unknown".
        for n in 1..=24 {
            let chord = format!("Ctrl+F{n}");
            let km = km_with(|b| b.next_theme = Some(ChordSpec::One(chord.clone())));
            assert!(km.warnings().is_empty(), "{chord}: {:?}", km.warnings());
            assert_eq!(km.pretty_chords(BindableAction::NextTheme), vec![chord.clone()]);
            assert_eq!(km.menu_hint(BindableAction::NextTheme), format!("⌃F{n}"));
            assert_eq!(parse_chord(&chord).unwrap().canonical(), chord);
        }
        let km = km_with(|b| {
            b.next_theme = Some(ChordSpec::One("F13".into()));
            b.prev_theme = Some(ChordSpec::One("F13".into()));
        });
        assert!(
            km.warnings().iter().any(|w| w.contains("(none)+F13")),
            "the conflict names the key: {:?}",
            km.warnings()
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
    fn a_user_chord_takes_another_actions_default_in_either_direction() {
        let ct = Mods::new(true, true, false, false);
        let t = |km: &KeyMap| km.lookup(ct, PhysicalKey::Code(KeyCode::KeyT), &ch("T"));
        let f = |km: &KeyMap| km.lookup(ct, PhysicalKey::Code(KeyCode::KeyF), &ch("F"));
        // search_toggle is declared AFTER new_tab: its remap onto new_tab's
        // default was dropped as "already bound" — while the mirror image below
        // always worked. The user's chord wins either way, quietly (the yielded
        // default is no longer listed in the help), and the other action keeps
        // the rest of its chords.
        let km = km_with(|b| b.search_toggle = Some(ChordSpec::One("Ctrl+Shift+T".into())));
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        assert_eq!(t(&km), Some(KeyAction::SearchToggle));
        assert_eq!(f(&km), None, "search's own default moved with the remap");
        assert!(km.pretty_chords(BindableAction::NewTab).is_empty(), "the help says new_tab lost its chord");
        assert_eq!(km.menu_hint(BindableAction::NewTab), "");
        let km = km_with(|b| b.new_tab = Some(ChordSpec::One("Ctrl+Shift+F".into())));
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        assert_eq!(f(&km), Some(KeyAction::NewTab));
        assert!(km.pretty_chords(BindableAction::SearchToggle).is_empty());
        // A default only yields the slot it lost: paste keeps Shift+Insert.
        let km = km_with(|b| b.copy = Some(ChordSpec::One("Ctrl+Shift+V".into())));
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        assert_eq!(km.lookup(ct, PhysicalKey::Code(KeyCode::KeyV), &ch("V")), Some(KeyAction::Copy));
        assert_eq!(km.pretty_chords(BindableAction::Paste), vec!["Shift+Insert".to_string()]);
        let shift = Mods::new(false, true, false, false);
        assert_eq!(
            km.lookup(shift, PhysicalKey::Code(KeyCode::Insert), &Key::Named(winit::keyboard::NamedKey::Insert)),
            Some(KeyAction::Paste)
        );
        // Two USER chords on one slot still conflict: the earlier action wins.
        let km = km_with(|b| {
            b.new_tab = Some(ChordSpec::One("Ctrl+Shift+G".into()));
            b.search_toggle = Some(ChordSpec::One("Ctrl+Shift+G".into()));
        });
        assert!(km.warnings().iter().any(|w| w.contains("search_toggle is ignored")), "{:?}", km.warnings());
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
    fn hint_and_copy_mode_default_chords() {
        // Ctrl+Shift+H → HintMode, Ctrl+Shift+Space → CopyMode, resolved via the
        // default keymap (H by physical position, Space by physical position).
        let km = KeyMap::defaults();
        let cs = Mods::new(true, true, false, false);
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::KeyH), &ch("H")),
            Some(KeyAction::HintMode)
        );
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::Space), &Key::Named(winit::keyboard::NamedKey::Space)),
            Some(KeyAction::CopyMode)
        );
        // Alt-insensitive, like the other Ctrl+Shift defaults.
        let csa = Mods::new(true, true, true, false);
        assert_eq!(
            km.lookup(csa, PhysicalKey::Code(KeyCode::KeyH), &ch("H")),
            Some(KeyAction::HintMode)
        );
    }

    #[test]
    fn hint_copy_mode_remap_and_reject() {
        // Remap hint_mode to Ctrl+Shift+G: the new chord fires and the default
        // Ctrl+Shift+H is freed.
        let km = km_with(|b| b.hint_mode = Some(ChordSpec::One("Ctrl+Shift+G".to_string())));
        let cs = Mods::new(true, true, false, false);
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::KeyG), &ch("G")),
            Some(KeyAction::HintMode)
        );
        assert_eq!(km.lookup(cs, PhysicalKey::Code(KeyCode::KeyH), &ch("H")), None);
        // A Ctrl-only remap that drops Shift is rejected as a control-byte shadow
        // (Ctrl+H = 0x08 BS, Ctrl+Space = NUL must keep reaching the PTY).
        let km = km_with(|b| {
            b.hint_mode = Some(ChordSpec::One("Ctrl+H".to_string()));
            b.copy_mode = Some(ChordSpec::One("Ctrl+Space".to_string()));
        });
        assert!(km.warnings().iter().any(|w| w.contains("control byte")));
        assert_eq!(
            km.lookup(Mods::new(true, false, false, false), PhysicalKey::Code(KeyCode::KeyH), &ch("h")),
            None,
            "Ctrl+H must remain unmapped → BS byte to PTY"
        );
    }

    // ── run selection (Ctrl+Shift+Enter) ──────────────────────────────────────

    #[test]
    fn run_selection_default_chord_resolves() {
        use winit::keyboard::NamedKey;
        let km = KeyMap::defaults();
        let cs = Mods::new(true, true, false, false);
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::Enter), &Key::Named(NamedKey::Enter)),
            Some(KeyAction::RunSelection)
        );
        // Alt-insensitive like the other Ctrl+Shift defaults.
        let csa = Mods::new(true, true, true, false);
        assert_eq!(
            km.lookup(csa, PhysicalKey::Code(KeyCode::Enter), &Key::Named(NamedKey::Enter)),
            Some(KeyAction::RunSelection)
        );
        // Plain / Shift / Ctrl Enter stay UNMAPPED → reach the PTY as \r.
        for m in [
            Mods::default(),
            Mods::new(false, true, false, false),
            Mods::new(true, false, false, false),
        ] {
            assert_eq!(
                km.lookup(m, PhysicalKey::Code(KeyCode::Enter), &Key::Named(NamedKey::Enter)),
                None,
                "{m:?}+Enter must pass through to the PTY"
            );
        }
    }

    #[test]
    fn run_selection_chord_passes_guard_and_collides_with_nothing() {
        // The default chord parses, passes chord_reject_reason, and the FULL
        // default map still compiles without a single conflict warning.
        let c = parse_chord("Ctrl+Shift+Enter").unwrap();
        assert!(chord_reject_reason(&c).is_none());
        assert!(!c.mods.ctrl_only());
        assert!(
            KeyMap::defaults().warnings().is_empty(),
            "default keymap must stay conflict-free: {:?}",
            KeyMap::defaults().warnings()
        );
        // No OTHER default action binds Enter under ANY modifier set.
        for a in BindableAction::ALL {
            if a == BindableAction::RunSelection {
                continue;
            }
            for ch in a.default_chords() {
                assert_ne!(
                    ch.key,
                    KeyMatch::Phys(KeyCode::Enter),
                    "{} also binds Enter",
                    a.name()
                );
            }
        }
    }

    #[test]
    fn run_selection_remap_and_unbind() {
        use winit::keyboard::NamedKey;
        // Remap: the new chord fires and the default is freed.
        let km = km_with(|b| b.run_selection = Some(ChordSpec::One("Ctrl+Shift+R".to_string())));
        let cs = Mods::new(true, true, false, false);
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::KeyR), &ch("R")),
            Some(KeyAction::RunSelection)
        );
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::Enter), &Key::Named(NamedKey::Enter)),
            None
        );
        // `""` unbinds entirely.
        let km = km_with(|b| b.run_selection = Some(ChordSpec::One(String::new())));
        assert_eq!(
            km.lookup(cs, PhysicalKey::Code(KeyCode::Enter), &Key::Named(NamedKey::Enter)),
            None
        );
        assert!(km.pretty_chords(BindableAction::RunSelection).is_empty());
    }

    #[test]
    fn run_selection_ordering_in_all() {
        // New actions are APPENDED so they never win a slot from an older one:
        // … CopyMode, RunSelection, ToggleFullscreen, then the v0.26 scroll
        // actions, theme cycling, and the context-menu key.
        let all = BindableAction::ALL;
        assert_eq!(all[all.len() - 1], BindableAction::ContextMenu);
        assert_eq!(all[all.len() - 2], BindableAction::PrevTheme);
        assert_eq!(all[all.len() - 3], BindableAction::NextTheme);
        assert_eq!(all[all.len() - 4], BindableAction::ScrollPageDown);
        assert_eq!(all[all.len() - 5], BindableAction::ScrollPageUp);
        assert_eq!(all[all.len() - 6], BindableAction::ToggleFullscreen);
        assert_eq!(all[all.len() - 7], BindableAction::RunSelection);
        assert_eq!(all[all.len() - 8], BindableAction::CopyMode);
    }

    #[test]
    fn theme_cycling_has_no_default_chord_and_binds_from_keys() {
        let km = KeyMap::defaults();
        assert!(km.pretty_chords(BindableAction::NextTheme).is_empty());
        assert!(km.pretty_chords(BindableAction::PrevTheme).is_empty());
        let km = km_with(|b| {
            b.next_theme = Some(ChordSpec::One("Ctrl+Alt+T".into()));
            b.prev_theme = Some(ChordSpec::One("Ctrl+Alt+R".into()));
        });
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        let mods = Mods::new(true, false, true, false);
        assert_eq!(
            km.lookup(mods, PhysicalKey::Code(KeyCode::KeyT), &Key::Character("t".into())),
            Some(KeyAction::NextTheme)
        );
        assert_eq!(
            km.lookup(mods, PhysicalKey::Code(KeyCode::KeyR), &Key::Character("r".into())),
            Some(KeyAction::PrevTheme)
        );
    }

    // ── fullscreen (F11) ──────────────────────────────────────────────────────

    #[test]
    fn default_keymap_binds_bare_f11_to_toggle_fullscreen() {
        use winit::keyboard::NamedKey;
        let km = KeyMap::defaults();
        assert_eq!(
            km.lookup(
                Mods::default(),
                PhysicalKey::Code(KeyCode::F11),
                &Key::Named(NamedKey::F11)
            ),
            Some(KeyAction::ToggleFullscreen)
        );
    }

    #[test]
    fn f11_was_free_and_default_map_has_no_warnings() {
        // No OTHER default action binds any F-key at all, so bare F11 took a free
        // slot (and the macOS companion Cmd+Ctrl+F took another).
        for a in BindableAction::ALL {
            if a == BindableAction::ToggleFullscreen {
                continue;
            }
            for c in a.default_chords() {
                if let KeyMatch::Phys(code) = &c.key {
                    assert!(!is_fkey(*code), "{} binds an F-key by default", a.name());
                }
            }
        }
        assert!(
            KeyMap::defaults().warnings().is_empty(),
            "default keymap must compile without conflicts: {:?}",
            KeyMap::defaults().warnings()
        );
    }

    #[test]
    fn no_default_chord_yields_a_slot_to_another_default() {
        // Defaults yield quietly to USER chords, so a default colliding with
        // another default would vanish without a warning: check every slot of
        // every default chord (Alt / Shift variants included) is its own.
        let km = KeyMap::defaults();
        for a in BindableAction::ALL {
            let ka = a.key_action();
            for c in a.default_chords() {
                let mut variants = vec![c.mods];
                if c.alt_insensitive {
                    variants.push(Mods { alt: !c.mods.alt, ..c.mods });
                }
                if c.shift_insensitive {
                    let flipped: Vec<Mods> = variants.iter().map(|m| Mods { shift: !m.shift, ..*m }).collect();
                    variants.extend(flipped);
                }
                for m in variants {
                    let owners: Vec<Option<&KeyAction>> = match &c.key {
                        KeyMatch::Phys(code) => vec![km.physical.get(&(m, *code))],
                        KeyMatch::Named(named) => vec![km.named.get(&(m, *named))],
                        KeyMatch::Logical { chars, phys_fallback } => chars
                            .iter()
                            .map(|ch| km.logical.get(&(m, smol_lower(ch))))
                            .chain(phys_fallback.map(|fb| km.phys_fallback.get(&(m, fb))))
                            .collect(),
                    };
                    for owner in owners {
                        assert_eq!(owner, Some(&ka), "{}: {} ({m:?})", a.name(), c.pretty());
                    }
                }
            }
            assert_eq!(km.pretty_chords(a).len(), a.default_chords().len(), "{}", a.name());
        }
    }

    #[test]
    fn bare_f11_passes_the_reject_guard() {
        let c = parse_chord("F11").unwrap();
        assert!(chord_reject_reason(&c).is_none(), "bare F-keys are bindable");
        assert_eq!(c.canonical(), "F11");
        // It is not a Ctrl-only chord, so the control-byte shadow guard is moot.
        assert!(!c.mods.ctrl_only());
    }

    #[test]
    fn macos_companion_chord_is_seeded_only_on_macos() {
        // Cmd+Ctrl+F: bare F11 is DEAD on macOS (Mission Control "Show Desktop",
        // and Volume Down on Apple keyboards without standard-function-keys).
        let chords = BindableAction::ToggleFullscreen.default_chords();
        let want = if cfg!(target_os = "macos") { 2 } else { 1 };
        assert_eq!(chords.len(), want);
        // The primary chord is bare F11 on every platform, and EXACT — so
        // Shift/Ctrl/Alt+F11 still reach the PTY (see the input.rs test).
        assert_eq!(chords[0].key, KeyMatch::Phys(KeyCode::F11));
        assert!(chords[0].mods.is_empty());
        assert!(!chords[0].alt_insensitive && !chords[0].shift_insensitive);
        if cfg!(target_os = "macos") {
            assert!(chords[1].mods.super_ && chords[1].mods.ctrl);
            assert!(!chords[1].mods.shift && !chords[1].mods.alt);
        }
    }

    #[test]
    fn toggle_fullscreen_is_remappable_and_unbindable() {
        use winit::keyboard::NamedKey;
        // Remap: the new chord fires and bare F11 is freed (→ PTY passthrough).
        // (Ctrl+Shift+U — Enter is no longer free: Ctrl+Shift+Enter became
        // RunSelection's default in v0.25.)
        let km = km_with(|b| {
            b.toggle_fullscreen = Some(ChordSpec::One("Ctrl+Shift+U".to_string()))
        });
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        assert_eq!(
            km.lookup(
                Mods::new(true, true, false, false),
                PhysicalKey::Code(KeyCode::KeyU),
                &ch("U")
            ),
            Some(KeyAction::ToggleFullscreen)
        );
        assert_eq!(
            km.lookup(
                Mods::default(),
                PhysicalKey::Code(KeyCode::F11),
                &Key::Named(NamedKey::F11)
            ),
            None
        );
        // `toggle_fullscreen = ""` unbinds the action entirely — the documented
        // escape hatch that gives bare F11 back to the shell.
        let km = km_with(|b| b.toggle_fullscreen = Some(ChordSpec::One(String::new())));
        assert_eq!(
            km.lookup(
                Mods::default(),
                PhysicalKey::Code(KeyCode::F11),
                &Key::Named(NamedKey::F11)
            ),
            None
        );
        assert!(km.pretty_chords(BindableAction::ToggleFullscreen).is_empty());
    }

    // ── context menu (the Menu key) ───────────────────────────────────────────

    fn menu() -> Key {
        Key::Named(NamedKey::ContextMenu)
    }

    const MENU_POS: PhysicalKey = PhysicalKey::Code(KeyCode::ContextMenu);

    #[test]
    fn the_bare_menu_key_opens_the_context_menu() {
        let km = KeyMap::defaults();
        assert_eq!(km.lookup(Mods::default(), MENU_POS, &menu()), Some(KeyAction::ContextMenu));
        // EXACT, like F11: a modified Menu key is not the chord.
        for m in [
            Mods::new(false, true, false, false),
            Mods::new(true, false, false, false),
            Mods::new(false, false, true, false),
        ] {
            assert_eq!(km.lookup(m, MENU_POS, &menu()), None, "{m:?}");
        }
        assert_eq!(km.pretty_chords(BindableAction::ContextMenu), vec!["Menu".to_string()]);
        assert_eq!(km.menu_hint(BindableAction::ContextMenu), "Menu");
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
    }

    #[test]
    fn the_menu_key_follows_the_layout_not_the_position() {
        let km = KeyMap::defaults();
        // `compose:menu` / `ctrl:menu_rctrl`: the Menu key's position types
        // Compose / is Right Ctrl — it must keep doing that.
        for other in [NamedKey::Compose, NamedKey::Control] {
            assert_eq!(km.lookup(Mods::default(), MENU_POS, &Key::Named(other)), None, "{other:?}");
        }
        // A key the layout made Menu opens the menu wherever it sits.
        assert_eq!(
            km.lookup(Mods::default(), PhysicalKey::Code(KeyCode::SuperRight), &menu()),
            Some(KeyAction::ContextMenu)
        );
    }

    #[test]
    fn menu_parses_binds_bare_remaps_and_unbinds() {
        for s in ["Menu", "menu", "ContextMenu", "Apps"] {
            let c = parse_chord(s).unwrap();
            assert_eq!(c.key, KeyMatch::Named(NamedKey::ContextMenu), "{s}");
            assert!(chord_reject_reason(&c).is_none(), "bare {s} is bindable: it types nothing");
            assert_eq!(c.canonical(), "Menu");
        }
        assert_eq!(parse_chord("Shift+Menu").unwrap().canonical(), "Shift+Menu");
        assert!(chord_reject_reason(&parse_chord("Ctrl+Menu").unwrap()).is_none(), "no control byte");
        // Remapped (Shift+F10 is the other keyboards' menu chord): the new
        // chords fire, the bare Menu key is free.
        let km = km_with(|b| {
            b.context_menu = Some(ChordSpec::Many(vec!["Shift+F10".into(), "Ctrl+Menu".into()]))
        });
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        let f10 = Key::Named(NamedKey::F10);
        assert_eq!(
            km.lookup(Mods::new(false, true, false, false), PhysicalKey::Code(KeyCode::F10), &f10),
            Some(KeyAction::ContextMenu)
        );
        assert_eq!(km.lookup(Mods::new(true, false, false, false), MENU_POS, &menu()), Some(KeyAction::ContextMenu));
        assert_eq!(km.lookup(Mods::default(), MENU_POS, &menu()), None);
        assert_eq!(
            km.pretty_chords(BindableAction::ContextMenu),
            vec!["Shift+F10".to_string(), "Ctrl+Menu".to_string()]
        );
        // `context_menu = ""` unbinds it (the key then reaches the program).
        let km = km_with(|b| b.context_menu = Some(ChordSpec::One(String::new())));
        assert_eq!(km.lookup(Mods::default(), MENU_POS, &menu()), None);
        assert!(km.pretty_chords(BindableAction::ContextMenu).is_empty());
        // Another action may take the Menu key (the default yields quietly) …
        let km = km_with(|b| b.search_toggle = Some(ChordSpec::One("Menu".into())));
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
        assert_eq!(km.lookup(Mods::default(), MENU_POS, &menu()), Some(KeyAction::SearchToggle));
        assert!(km.pretty_chords(BindableAction::ContextMenu).is_empty());
        // … but two user chords on it conflict, the earlier action winning.
        let km = km_with(|b| {
            b.new_tab = Some(ChordSpec::One("Menu".into()));
            b.context_menu = Some(ChordSpec::One("Menu".into()));
        });
        assert!(
            km.warnings().iter().any(|w| w.contains("(none)+Menu") && w.contains("context_menu")),
            "{:?}",
            km.warnings()
        );
        assert_eq!(km.lookup(Mods::default(), MENU_POS, &menu()), Some(KeyAction::NewTab));
    }

    #[test]
    fn bindable_action_all_is_exhaustive() {
        assert_eq!(BindableAction::ALL.len(), 37);
        for a in BindableAction::ALL {
            assert_eq!(
                BindableAction::ALL.iter().filter(|x| **x == a).count(),
                1,
                "{} appears more than once in ALL",
                a.name()
            );
        }
    }

    #[test]
    fn array_binds_multiple_chords() {
        let km = km_with(|b| {
            b.new_tab = Some(ChordSpec::Many(vec![
                "Ctrl+Shift+G".to_string(),
                "Ctrl+Shift+N".to_string(),
            ]));
        });
        // (Letters match the produced character, so each event carries its own.)
        for (code, label) in [(KeyCode::KeyG, "G"), (KeyCode::KeyN, "N")] {
            assert_eq!(
                km.lookup(Mods::new(true, true, false, false), PhysicalKey::Code(code), &ch(label)),
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
    fn letter_chords_match_labels_and_pretty_print_uppercase() {
        let km = KeyMap::defaults();
        let cs = Mods::new(true, true, false, false);
        // Label match wherever the key sits…
        assert_eq!(km.lookup(cs, PhysicalKey::Code(KeyCode::KeyI), &ch("C")), Some(KeyAction::Copy));
        // …an ASCII label elsewhere never falls back to the US position…
        assert_eq!(km.lookup(cs, PhysicalKey::Code(KeyCode::KeyC), &ch("J")), None);
        // …while a non-ASCII one does (Cyrillic С at KeyC).
        assert_eq!(km.lookup(cs, PhysicalKey::Code(KeyCode::KeyC), &ch("С")), Some(KeyAction::Copy));
        // Named keys and the digit row stay positional.
        assert_eq!(
            km.lookup(Mods::new(true, false, false, false), PhysicalKey::Code(KeyCode::Digit2), &ch("é")),
            Some(KeyAction::SelectTab(1)),
            "AZERTY Ctrl+[2/é] still jumps to tab 2"
        );
        // Help shows letters as engraved.
        assert_eq!(km.pretty_chords(BindableAction::NewTab), vec!["Ctrl+Shift+T".to_string()]);
        // A user letter chord is a label chord too.
        let c = parse_chord("Ctrl+Shift+n").unwrap();
        assert_eq!(c.canonical(), "Ctrl+Shift+N");
        assert_eq!(c.key, letter('N'));
    }

    #[test]
    fn page_keys_may_be_bound_bare_but_letters_may_not() {
        assert!(chord_reject_reason(&parse_chord("PageUp").unwrap()).is_none());
        assert!(chord_reject_reason(&parse_chord("PageDown").unwrap()).is_none());
        assert!(chord_reject_reason(&parse_chord("Home").unwrap()).is_some());
        assert!(chord_reject_reason(&parse_chord("T").unwrap()).is_some());
        // Default scroll chords: Shift+PageUp / Shift+PageDown, conflict-free.
        let km = KeyMap::defaults();
        let sh = Mods::new(false, true, false, false);
        let pgup = Key::Named(winit::keyboard::NamedKey::PageUp);
        assert_eq!(km.lookup(sh, PhysicalKey::Code(KeyCode::PageUp), &pgup), Some(KeyAction::ScrollPageUp));
        assert_eq!(km.lookup(Mods::default(), PhysicalKey::Code(KeyCode::PageUp), &pgup), None);
        assert!(km.warnings().is_empty(), "{:?}", km.warnings());
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
