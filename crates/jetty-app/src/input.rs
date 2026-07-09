use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// High-level action decoded from a key press event.
#[derive(Debug, PartialEq, Eq)]
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
    /// Raw bytes to write to the PTY.
    Send(Vec<u8>),
    None,
}

/// Decide what a key press means.
///
/// * `ctrl`       – whether the Control modifier is held
/// * `shift`      – whether the Shift modifier is held
/// * `alt`        – whether the Alt/Meta modifier is held
/// * `physical`   – layout-independent [`PhysicalKey`] from the event
/// * `logical`    – the produced [`Key`] from the event
/// * `panel_open` – whether the Settings panel is currently visible
/// * `app_cursor` – whether DECCKM application cursor keys are enabled
///   (`\e[?1h`); when true, arrow keys are encoded with the SS3 (`\eO`) prefix
///   instead of CSI (`\e[`).
/// * `alt_screen` – whether the terminal is on the alternate screen (a
///   full-screen app like less/vim/htop owns the display). Plain PageUp/PageDown
///   are forwarded to the PTY there (`\e[5~`/`\e[6~`) — the alt screen has no
///   scrollback to page. Shift+PageUp/Down is ALWAYS host scrollback (the
///   standard escape hatch).
///
/// The rules mirror `app.rs` exactly:
/// 1. Ctrl+, (no shift)         → TogglePanel
/// 2. Escape                    → ClosePanel if panel open, else Send(ESC)
/// 3. Ctrl+Shift+P              → OpenPalette (command palette)
///    3b. Ctrl+Shift+O          → TogglePanel (Settings — kept as an alias)
/// 4. Ctrl+Shift+T              → CycleTheme
///    4b. Ctrl+Shift+F         → SearchToggle (scrollback-search bar)
/// 5. Ctrl+Shift+Equal          → OpacityUp
/// 6. Ctrl+Shift+Minus          → OpacityDown
/// 7. PageUp                    → ScrollPageUp (primary screen) / `\e[5~` (alt screen)
/// 8. PageDown                  → ScrollPageDown (primary screen) / `\e[6~` (alt screen)
/// 9. Ctrl+<letter/symbol> → control byte, regardless of shift (the explicit
///    Ctrl+Shift shortcuts in rules 3-6 are intercepted first). When Alt is also
///    held, the control byte is ESC-prefixed (Ctrl+Alt+b → ESC + 0x02).
/// 10. Alt+<key> that yields bytes → ESC-prefixed Send(esc + bytes)
/// 11. Otherwise: key_to_bytes  → Send(bytes) or None
#[allow(clippy::too_many_arguments)]
pub fn decide_key(
    ctrl: bool,
    shift: bool,
    alt: bool,
    physical: PhysicalKey,
    logical: &Key,
    panel_open: bool,
    app_cursor: bool,
    alt_screen: bool,
) -> KeyAction {
    // Rule 1: Ctrl+, → toggle panel.
    // Match the PHYSICAL key; keep a logical fallback for platforms where
    // physical_key is unreliable.
    let is_comma = matches!(physical, PhysicalKey::Code(KeyCode::Comma))
        || matches!(logical, Key::Character(s) if s.as_str() == ",");
    if ctrl && !shift && is_comma {
        return KeyAction::TogglePanel;
    }

    // Rule 2: Escape → close panel if open, otherwise forward ESC byte.
    if matches!(logical, Key::Named(NamedKey::Escape))
        && panel_open {
            return KeyAction::ClosePanel;
        }
        // Fall through: key_to_bytes(Escape) will produce Send(vec![0x1b]).

    // Rules 3-5 + panel toggle: Ctrl+Shift hotkeys keyed by PHYSICAL key, which
    // is layout-independent. Ctrl+Shift+O toggles the Settings panel and works on
    // every layout — unlike Ctrl+, which on a Turkish layout reports as Backslash
    // (not Comma), so it never matched.
    if ctrl && shift {
        // Opacity: prefer the LOGICAL character so the binding follows the
        // engraved key on every layout (Turkish-Q, QWERTZ), mirroring the font
        // bindings below. On Turkish-Q the '-'-engraved key is physical Equal, so
        // a pure physical match inverted the direction (Ctrl+Shift+'-' → up).
        if let Key::Character(s) = logical {
            match s.as_str() {
                "+" | "=" => return KeyAction::OpacityUp,
                "-" | "_" => return KeyAction::OpacityDown,
                _ => {}
            }
        }
        match physical {
            // KeyP opens the fuzzy command palette (VSCode/Sublime muscle memory).
            // Settings stays reachable via Ctrl+Shift+O (kept below), Ctrl+, ,
            // macOS Cmd+, , and an "Open Settings…" palette entry.
            PhysicalKey::Code(KeyCode::KeyP) => return KeyAction::OpenPalette,
            PhysicalKey::Code(KeyCode::KeyO) => return KeyAction::TogglePanel,
            // Tabs: Ctrl+Shift+T opens a new tab; Ctrl+Shift+W closes the active
            // one. Ctrl+Shift+D detaches the active tab or reattaches a detached one.
            // Theme switching moved to the Settings window. Ctrl+Shift+Tab cycles to
            // the previous tab (must be intercepted before ctrl_byte).
            PhysicalKey::Code(KeyCode::KeyT) => return KeyAction::NewTab,
            PhysicalKey::Code(KeyCode::KeyW) => return KeyAction::CloseTab,
            PhysicalKey::Code(KeyCode::KeyD) => return KeyAction::DetachTab,
            // Scrollback search — must be intercepted before ctrl_byte, which
            // would otherwise send 0x06 (ACK) to the PTY.
            PhysicalKey::Code(KeyCode::KeyF) => return KeyAction::SearchToggle,
            // Prompt jump (OSC 133): Ctrl+Shift+Z prev / Ctrl+Shift+X next.
            // Intercepted here so KeyZ/KeyX never fall through to the ctrl-byte
            // path (which would send 0x1a / 0x18 to the PTY). Plain Ctrl+Z /
            // Ctrl+X are unaffected — they lack Shift and are handled below.
            PhysicalKey::Code(KeyCode::KeyZ) => return KeyAction::PrevPrompt,
            PhysicalKey::Code(KeyCode::KeyX) => return KeyAction::NextPrompt,
            PhysicalKey::Code(KeyCode::Tab) => return KeyAction::PrevTab,
            // Physical fallback for layouts where the logical char above is
            // unreliable (US: Shift+Equal → '+', Shift+Minus → '_').
            PhysicalKey::Code(KeyCode::Equal) => return KeyAction::OpacityUp,
            PhysicalKey::Code(KeyCode::Minus) => return KeyAction::OpacityDown,
            PhysicalKey::Code(KeyCode::KeyC) => return KeyAction::Copy,
            PhysicalKey::Code(KeyCode::KeyV) => return KeyAction::Paste,
            _ => {}
        }
    }

    // Tab navigation with Ctrl (no shift): Ctrl+Tab → next tab, Ctrl+1..9 → jump
    // to that tab. These MUST be intercepted before the ctrl_byte fallback so
    // they never send a control byte (Ctrl+I / Ctrl+digit) to the PTY.
    if ctrl && !shift {
        match physical {
            PhysicalKey::Code(KeyCode::Tab) => return KeyAction::NextTab,
            PhysicalKey::Code(KeyCode::Digit1) => return KeyAction::SelectTab(0),
            PhysicalKey::Code(KeyCode::Digit2) => return KeyAction::SelectTab(1),
            PhysicalKey::Code(KeyCode::Digit3) => return KeyAction::SelectTab(2),
            PhysicalKey::Code(KeyCode::Digit4) => return KeyAction::SelectTab(3),
            PhysicalKey::Code(KeyCode::Digit5) => return KeyAction::SelectTab(4),
            PhysicalKey::Code(KeyCode::Digit6) => return KeyAction::SelectTab(5),
            PhysicalKey::Code(KeyCode::Digit7) => return KeyAction::SelectTab(6),
            PhysicalKey::Code(KeyCode::Digit8) => return KeyAction::SelectTab(7),
            PhysicalKey::Code(KeyCode::Digit9) => return KeyAction::SelectTab(8),
            _ => {}
        }
    }

    // Rules 6-7: PageUp / PageDown — alt-screen aware.
    // * Shift+PageUp/Down → ALWAYS host scrollback (the standard escape hatch).
    // * Plain PageUp/Down on the ALT screen → forward `\e[5~`/`\e[6~` to the PTY
    //   so pagers/editors (less/man/vim/htop) actually page; the alt screen has
    //   no scrollback, so the old unconditional ScrollPage* was a dead key there.
    // * Plain PageUp/Down on the PRIMARY screen → host scrollback, as before.
    if let Key::Named(page @ (NamedKey::PageUp | NamedKey::PageDown)) = logical {
        let up = *page == NamedKey::PageUp;
        // Ctrl/Alt+PageUp/Down → xterm modified tilde form (`\e[5;m~` / `\e[6;m~`)
        // so apps see the modifier (vim :tabnext on Ctrl+PageDown, tmux
        // `bind -n C-PgUp`, less/htop) — on BOTH screens. Plain and Shift+Page keep
        // host scrollback (Shift being the standard escape hatch).
        if ctrl || alt {
            let n = if up { 5 } else { 6 };
            let m = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
            return KeyAction::Send(format!("\x1b[{n};{m}~").into_bytes());
        }
        if !shift && alt_screen {
            return KeyAction::Send(if up {
                b"\x1b[5~".to_vec()
            } else {
                b"\x1b[6~".to_vec()
            });
        }
        return if up {
            KeyAction::ScrollPageUp
        } else {
            KeyAction::ScrollPageDown
        };
    }

    // Font-size bindings: Ctrl (no shift) + '+'/'='/'-'/'0'.
    // Keyed on the LOGICAL character first so the binding follows the engraved
    // key on every layout — on Turkish-Q the '-'-engraved key is physical Equal,
    // so a pure physical match made Ctrl+'-' GROW the font (the inverse of what
    // README/help promise); on QWERTZ '-' is physical Slash, so Ctrl+'-' typed a
    // literal '-'. The physical positions remain as a fallback for layouts where
    // the logical char is unreliable. Checked BEFORE the ctrl_byte fallback so
    // they are never swallowed as a raw control code; Ctrl+Shift+'='/'-' are
    // handled above as OpacityUp/Down and never reach here.
    if ctrl && !shift {
        if let Key::Character(s) = logical {
            match s.as_str() {
                "+" | "=" => return KeyAction::FontUp,
                "-" => return KeyAction::FontDown,
                "0" => return KeyAction::FontReset,
                _ => {}
            }
        }
        match physical {
            PhysicalKey::Code(KeyCode::Equal) => return KeyAction::FontUp,
            PhysicalKey::Code(KeyCode::Minus) => return KeyAction::FontDown,
            PhysicalKey::Code(KeyCode::Digit0) => return KeyAction::FontReset,
            _ => {}
        }
    }

    // Ctrl+<letter/symbol> → control byte (Ctrl+C = 0x03 SIGINT, Ctrl+D = EOF,
    // Ctrl+Z, Ctrl+L clear, ...). Must come before the plain key_to_bytes
    // fallback, which would otherwise send the literal character.
    //
    // Keyed on the LOGICAL character, not the physical position: physical codes
    // are hardware QWERTY positions, so keying letters on them sends the wrong
    // control code on Dvorak/AZERTY/QWERTZ — Ctrl at the key LABELED C must be
    // 0x03, not TAB. Fall back to the physical position (ctrl_byte) only when the
    // logical key is non-ASCII (Cyrillic/Greek/CJK) or Unidentified; ctrl_byte
    // also owns the C0 symbol combos Ctrl+Space/[/\/] (NUL/ESC/FS/GS).
    //
    // Applies REGARDLESS of shift: Ctrl+Shift+C == Ctrl+C for control purposes
    // (both → 0x03). The explicit Ctrl+Shift app shortcuts (O/T/Equal/Minus) are
    // intercepted above and never reach here, so they keep their special meaning.
    //
    // When Alt/Meta is also held, the control byte is ESC-prefixed (the classic
    // "Meta sends Escape" convention), e.g. Ctrl+Alt+b → ESC + 0x02.
    //
    // Symbol control bytes are keyed on the logical character too: "/"|"_" → 0x1f
    // (US unit separator — readline/emacs undo), "@" → 0x00 (NUL), "^" → 0x1e (RS),
    // "?" → 0x7f (DEL). Physical Slash produces "-" on German layouts (which must
    // keep falling through to the literal "-"), and "_" reaches here only when it
    // is not Ctrl+Shift+Minus (that chord is intercepted above as OpacityDown).
    if ctrl {
        let logical_byte = match logical {
            Key::Character(s) if s.chars().count() == 1 => {
                let c = s.chars().next().unwrap();
                if c.is_ascii_alphabetic() {
                    Some(c.to_ascii_uppercase() as u8 - b'A' + 1)
                } else {
                    match c {
                        '/' | '_' => Some(0x1f),
                        '@' => Some(0x00),
                        '^' => Some(0x1e),
                        '?' => Some(0x7f),
                        _ => None,
                    }
                }
            }
            _ => None,
        };
        if let Some(b) = logical_byte {
            if alt {
                return KeyAction::Send(vec![0x1b, b]);
            }
            return KeyAction::Send(vec![b]);
        }
        if let PhysicalKey::Code(code) = physical {
            if let Some(b) = ctrl_byte(code) {
                if alt {
                    return KeyAction::Send(vec![0x1b, b]);
                }
                return KeyAction::Send(vec![b]);
            }
        }
    }

    // Ctrl+Backspace → 0x08 (BS), distinct from plain / Alt Backspace (0x7f) so
    // editors can bind delete-word-backward. ESC-prefixed when Alt is also held.
    if ctrl && matches!(logical, Key::Named(NamedKey::Backspace)) {
        if alt {
            return KeyAction::Send(vec![0x1b, 0x08]);
        }
        return KeyAction::Send(vec![0x08]);
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
            return KeyAction::Send(format!("\x1b[1;{}{}", m, fin as char).into_bytes());
        }
        // Modified Home/End/Delete/Insert: the xterm modified forms. Home/End
        // use the CSI-1 letter form (`\e[1;<mod>H` / `\e[1;<mod>F`); Insert and
        // Delete keep their tilde form with the modifier param (`\e[2;<mod>~` /
        // `\e[3;<mod>~`). Without these, Shift+Home (select-to-line-start) and
        // Ctrl+Delete (delete-word-forward) collapsed to the plain sequences and
        // apps could not see the modifiers at all.
        // Shift+Insert (no Ctrl/Alt) is the universal terminal paste chord,
        // handled by the host — never forwarded as `\e[2;2~`. Ctrl/Alt+Insert keep
        // the modified tilde form below.
        if shift && !ctrl && !alt && matches!(logical, Key::Named(NamedKey::Insert)) {
            return KeyAction::Paste;
        }
        let m = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
        match logical {
            Key::Named(NamedKey::Home) => {
                return KeyAction::Send(format!("\x1b[1;{m}H").into_bytes());
            }
            Key::Named(NamedKey::End) => {
                return KeyAction::Send(format!("\x1b[1;{m}F").into_bytes());
            }
            Key::Named(NamedKey::Insert) => {
                return KeyAction::Send(format!("\x1b[2;{m}~").into_bytes());
            }
            Key::Named(NamedKey::Delete) => {
                return KeyAction::Send(format!("\x1b[3;{m}~").into_bytes());
            }
            _ => {}
        }
        // Modified function keys: F1–F4 use the CSI-1 letter form (`\e[1;{m}P..S`),
        // F5–F12 the CSI tilde form (`\e[{n};{m}~`). Without these any modified
        // F-key collapses to the unmodified sequence (Shift+F5 → plain `\e[15~`),
        // and Alt+F-key would get a double-ESC from the Meta fallback below.
        let fkey_final = match logical {
            Key::Named(NamedKey::F1) => Some('P'),
            Key::Named(NamedKey::F2) => Some('Q'),
            Key::Named(NamedKey::F3) => Some('R'),
            Key::Named(NamedKey::F4) => Some('S'),
            _ => None,
        };
        if let Some(fin) = fkey_final {
            return KeyAction::Send(format!("\x1b[1;{m}{fin}").into_bytes());
        }
        let fkey_num = match logical {
            Key::Named(NamedKey::F5) => Some(15),
            Key::Named(NamedKey::F6) => Some(17),
            Key::Named(NamedKey::F7) => Some(18),
            Key::Named(NamedKey::F8) => Some(19),
            Key::Named(NamedKey::F9) => Some(20),
            Key::Named(NamedKey::F10) => Some(21),
            Key::Named(NamedKey::F11) => Some(23),
            Key::Named(NamedKey::F12) => Some(24),
            _ => None,
        };
        if let Some(n) = fkey_num {
            return KeyAction::Send(format!("\x1b[{n};{m}~").into_bytes());
        }
        if shift && !ctrl && !alt && matches!(logical, Key::Named(NamedKey::Tab)) {
            return KeyAction::Send(b"\x1b[Z".to_vec());
        }
    }

    // Arrow keys honor DECCKM (application cursor mode). When `app_cursor` is set
    // (`\e[?1h`), arrows are encoded with the SS3 prefix (`\eO A/B/C/D`) instead of
    // the default CSI prefix (`\e[ A/B/C/D`) so apps like vim/readline see the
    // sequences they expect.
    if let Some(bytes) = cursor_key_bytes(logical, app_cursor) {
        if alt {
            let mut out = Vec::with_capacity(bytes.len() + 1);
            out.push(0x1b);
            out.extend_from_slice(&bytes);
            return KeyAction::Send(out);
        }
        return KeyAction::Send(bytes);
    }

    // Fallback: convert the key to its byte sequence. When Alt/Meta is
    // held and the key produces bytes, send them ESC-prefixed (the classic
    // "Meta sends Escape" convention), e.g. Alt+b → ESC b, Alt+Enter → ESC CR.
    match key_to_bytes(logical) {
        Some(bytes) => {
            if alt {
                let mut out = Vec::with_capacity(bytes.len() + 1);
                out.push(0x1b);
                out.extend_from_slice(&bytes);
                KeyAction::Send(out)
            } else {
                KeyAction::Send(bytes)
            }
        }
        None => KeyAction::None,
    }
}

/// High-level action decoded from a left mouse button press.
#[derive(Debug, PartialEq)]
pub enum MouseAction {
    /// User pressed on the opacity slider handle or track.
    StartSliderDrag,
    /// User pressed on the corner-radius slider handle or track.
    StartRadiusDrag,
    /// User clicked a theme row in the OPEN dropdown. The index is into
    /// `jetty_core::theme::PRESETS` (already offset by `geom.theme_scroll_offset`).
    SetTheme(usize),
    /// User clicked the collapsed theme combo header — toggle the dropdown open/closed.
    ToggleThemeDropdown,
    /// User clicked the ▲ theme-list scroll button — scroll up (offset−1).
    ThemeScrollUp,
    /// User clicked the ▼ theme-list scroll button — scroll down (offset+1).
    ThemeScrollDown,
    /// User clicked the font-size decrement button ("−").
    FontMinus,
    /// User clicked the font-size increment button ("+").
    FontPlus,
    /// User clicked the font-size reset button ("reset").
    FontReset,
    /// User clicked a font-family row. The index is
    /// `geom.font_scroll_offset + row_index` into the families list.
    SetFont(usize),
    /// User clicked the ▲ font-list scroll button — scroll up (offset−1).
    FontScrollUp,
    /// User clicked the ▼ font-list scroll button — scroll down (offset+1).
    FontScrollDown,
    /// User clicked the UI (chrome) font-size decrement button ("−").
    UiFontMinus,
    /// User clicked the UI (chrome) font-size increment button ("+").
    UiFontPlus,
    /// User clicked the UI (chrome) font-size reset button ("rst").
    UiFontReset,
    /// User clicked a UI-font-family row. The index is
    /// `geom.ui_font_scroll_offset + row_index` into the UI families list
    /// (index 0 = the synthetic "System Sans (default)" row → "").
    SetUiFont(usize),
    /// User clicked the ▲ UI-font-list scroll button — scroll up (offset−1).
    UiFontScrollUp,
    /// User clicked the ▼ UI-font-list scroll button — scroll down (offset+1).
    UiFontScrollDown,
    /// User clicked the summon-effect "‹" button — cycle to the previous effect.
    SummonPrev,
    /// User clicked the summon-effect "›" button — cycle to the next effect.
    SummonNext,
    /// User clicked the window-mode "‹" button — cycle to the previous mode.
    WinModePrev,
    /// User clicked the window-mode "›" button — cycle to the next mode.
    WinModeNext,
    /// User clicked the tab-bar "‹" button — cycle the tab-bar position.
    TabBarPrev,
    /// User clicked the tab-bar "›" button — cycle the tab-bar position.
    TabBarNext,
    /// User clicked the scrollback "‹" button — previous scrollback-lines step.
    ScrollbackPrev,
    /// User clicked the scrollback "›" button — next scrollback-lines step.
    ScrollbackNext,
    /// User pressed on the Dropdown-height slider handle or track — start drag.
    StartDropdownDrag,
    /// User pressed on the Dropdown-width slider handle or track — start drag.
    StartDropdownWidthDrag,
    /// User clicked the "Auto-hide on focus loss" toggle pill.
    ToggleFocusAutoHide,
    /// User clicked the "Launch at login" toggle pill (bottom band).
    ToggleLaunchAtLogin,
    /// User clicked the shell-picker "‹" button — cycle to the previous shell.
    CycleShellPrev,
    /// User clicked the shell-picker "›" button — cycle to the next shell.
    CycleShellNext,
    // ── Shell tab: RUN & NOTIFY (v0.15) ────────────────────────────────────────
    /// User clicked the "Notify on command finish" master toggle.
    ToggleNotifyOnFinish,
    /// User clicked the "Only on failure" toggle.
    ToggleNotifyOnlyFailure,
    /// User clicked the minimum-duration cycler "‹" — previous step.
    NotifyDurPrev,
    /// User clicked the minimum-duration cycler "›" — next step.
    NotifyDurNext,
    /// User clicked the "Auto-summon when hidden" toggle.
    ToggleAutoSummon,
    // ── Effects tab widgets ────────────────────────────────────────────────────
    /// User clicked the "CRT enabled" master toggle pill.
    ToggleCrt,
    /// User clicked the "CRT roll" animation toggle pill.
    ToggleCrtRoll,
    /// User clicked the "CRT flicker" animation toggle pill.
    ToggleCrtFlicker,
    /// User clicked the "CRT jitter" animation toggle pill.
    ToggleCrtJitter,
    /// User clicked the "caret flash" enabled toggle pill.
    ToggleCaretFlash,
    /// User clicked the "caret glow" enabled toggle pill.
    ToggleCaretGlow,
    /// User pressed on the CRT curvature slider handle or track — start drag.
    StartCrtCurvatureDrag,
    /// User pressed on the CRT scanline-intensity slider handle or track — start drag.
    StartScanlineDrag,
    /// User pressed on the CRT shadow-mask slider handle or track — start drag.
    StartMaskDrag,
    /// User pressed on the CRT bloom slider handle or track — start drag.
    StartBloomDrag,
    /// User pressed on the CRT chromatic-aberration slider handle or track — start drag.
    StartChromaticDrag,
    /// User pressed on the CRT vignette slider handle or track — start drag.
    StartVignetteDrag,
    /// User pressed on the caret flash-duration slider handle or track — start drag.
    StartCaretDurDrag,
    /// User pressed on the CRT scanline-tint R-channel mini-slider — start drag.
    StartTintRDrag,
    /// User pressed on the CRT scanline-tint G-channel mini-slider — start drag.
    StartTintGDrag,
    /// User pressed on the CRT scanline-tint B-channel mini-slider — start drag.
    StartTintBDrag,
    /// User pressed on the caret color R-channel mini-slider — start drag.
    StartCaretColorRDrag,
    /// User pressed on the caret color G-channel mini-slider — start drag.
    StartCaretColorGDrag,
    /// User pressed on the caret color B-channel mini-slider — start drag.
    StartCaretColorBDrag,
    /// User clicked one of the 5 settings tab labels — switch the active tab.
    /// The index is 0..=4 (Look / Fonts / Window / Shell / Effects).
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
/// Priority:
/// 1. If panel open: slider handle/track → StartSliderDrag
/// 2. If panel open: theme combo / open rows → ToggleThemeDropdown / SetTheme(i)
/// 3. If panel open: font-size buttons   → FontMinus/Plus/Reset
/// 4. If panel open: font-scroll buttons → FontScrollUp/FontScrollDown
/// 5. If panel open: font-family row     → SetFont(idx)
/// 6. If panel open: title bar (top ~36px, no widget hit) → StartDialogDrag
/// 7. If panel open: inside panel rect   → ConsumePanel
/// 8. (Falls through to scrollbar when click is outside open panel)
/// 9. Inside scrollbar thumb             → StartScrollbarDrag
/// 10. Inside scrollbar track x-range    → ScrollbarTrackJump
/// 11. Anything else                     → None
pub fn decide_mouse_press(
    panel: Option<&jetty_render::PanelGeom>,
    scrollbar: Option<&jetty_render::Rect>,
    cx: f32,
    cy: f32,
) -> MouseAction {
    if let Some(g) = panel {
        // Tab strip — tested FIRST so a tab switch always wins over any band
        // widget (the inactive tabs' widgets are offscreen anyway, but the active
        // tab's first band sits just below the strip, so order still matters).
        for (i, tab) in g.tab_rects.iter().enumerate() {
            if point_in(tab, cx, cy) {
                return MouseAction::SetSettingsTab(i);
            }
        }
        // Opacity slider handle or track → start drag.
        if point_in(&g.slider_handle, cx, cy) || point_in(&g.slider_track, cx, cy) {
            return MouseAction::StartSliderDrag;
        }
        // Corner-radius slider handle or track → start drag.
        if point_in(&g.radius_handle, cx, cy) || point_in(&g.radius_track, cx, cy) {
            return MouseAction::StartRadiusDrag;
        }
        // Theme picker. When the dropdown is OPEN its rows + scroll arrows take
        // priority (they overlay the area below the combo). A row click selects a
        // preset; the scroll arrows page the list. Clicking the combo header itself
        // toggles the dropdown.
        if g.theme_open {
            for (i, row) in g.theme_rows.iter().enumerate() {
                if point_in(row, cx, cy) {
                    return MouseAction::SetTheme(g.theme_scroll_offset + i);
                }
            }
            if point_in(&g.theme_scroll_up, cx, cy) {
                return MouseAction::ThemeScrollUp;
            }
            if point_in(&g.theme_scroll_down, cx, cy) {
                return MouseAction::ThemeScrollDown;
            }
        }
        if point_in(&g.theme_combo, cx, cy) {
            return MouseAction::ToggleThemeDropdown;
        }
        // Font-size buttons (checked before generic ConsumePanel).
        if point_in(&g.font_minus, cx, cy) {
            return MouseAction::FontMinus;
        }
        if point_in(&g.font_plus, cx, cy) {
            return MouseAction::FontPlus;
        }
        if point_in(&g.font_reset, cx, cy) {
            return MouseAction::FontReset;
        }
        // Font-list scroll buttons.
        if point_in(&g.font_scroll_up, cx, cy) {
            return MouseAction::FontScrollUp;
        }
        if point_in(&g.font_scroll_down, cx, cy) {
            return MouseAction::FontScrollDown;
        }
        // Summon-effect cycle buttons.
        if point_in(&g.summon_prev, cx, cy) {
            return MouseAction::SummonPrev;
        }
        if point_in(&g.summon_next, cx, cy) {
            return MouseAction::SummonNext;
        }
        // Window-mode cycle buttons.
        if point_in(&g.win_mode_prev, cx, cy) {
            return MouseAction::WinModePrev;
        }
        if point_in(&g.win_mode_next, cx, cy) {
            return MouseAction::WinModeNext;
        }
        // Tab-bar position cycle buttons.
        if point_in(&g.tab_bar_prev, cx, cy) {
            return MouseAction::TabBarPrev;
        }
        if point_in(&g.tab_bar_next, cx, cy) {
            return MouseAction::TabBarNext;
        }
        // Scrollback-lines cycle buttons.
        if point_in(&g.scrollback_prev, cx, cy) {
            return MouseAction::ScrollbackPrev;
        }
        if point_in(&g.scrollback_next, cx, cy) {
            return MouseAction::ScrollbackNext;
        }
        // Dropdown-height slider handle or track.
        if point_in(&g.dropdown_handle, cx, cy) || point_in(&g.dropdown_track, cx, cy) {
            return MouseAction::StartDropdownDrag;
        }
        // Dropdown-width slider handle or track.
        if point_in(&g.dropdown_width_handle, cx, cy) || point_in(&g.dropdown_width_track, cx, cy) {
            return MouseAction::StartDropdownWidthDrag;
        }
        // Auto-hide toggle pill.
        if point_in(&g.autohide_toggle, cx, cy) {
            return MouseAction::ToggleFocusAutoHide;
        }
        // Launch-at-login toggle pill (bottom band).
        if point_in(&g.launch_login_toggle, cx, cy) {
            return MouseAction::ToggleLaunchAtLogin;
        }
        // Shell-picker cycle buttons.
        if point_in(&g.shell_prev, cx, cy) {
            return MouseAction::CycleShellPrev;
        }
        if point_in(&g.shell_next, cx, cy) {
            return MouseAction::CycleShellNext;
        }
        // RUN & NOTIFY section (Shell tab, v0.15).
        if point_in(&g.notify_toggle, cx, cy) {
            return MouseAction::ToggleNotifyOnFinish;
        }
        if point_in(&g.notify_failure_toggle, cx, cy) {
            return MouseAction::ToggleNotifyOnlyFailure;
        }
        if point_in(&g.notify_dur_prev, cx, cy) {
            return MouseAction::NotifyDurPrev;
        }
        if point_in(&g.notify_dur_next, cx, cy) {
            return MouseAction::NotifyDurNext;
        }
        if point_in(&g.auto_summon_toggle, cx, cy) {
            return MouseAction::ToggleAutoSummon;
        }
        // Font-family list rows.
        for (i, row) in g.font_rows.iter().enumerate() {
            if point_in(row, cx, cy) {
                return MouseAction::SetFont(g.font_scroll_offset + i);
            }
        }
        // UI (chrome) font-size buttons.
        if point_in(&g.ui_font_minus, cx, cy) {
            return MouseAction::UiFontMinus;
        }
        if point_in(&g.ui_font_plus, cx, cy) {
            return MouseAction::UiFontPlus;
        }
        if point_in(&g.ui_font_reset, cx, cy) {
            return MouseAction::UiFontReset;
        }
        // UI-font-list scroll buttons.
        if point_in(&g.ui_font_scroll_up, cx, cy) {
            return MouseAction::UiFontScrollUp;
        }
        if point_in(&g.ui_font_scroll_down, cx, cy) {
            return MouseAction::UiFontScrollDown;
        }
        // UI (chrome) font-family list rows. Row index 0 maps to the synthetic
        // "System Sans (default)" entry (handled app-side as "").
        for (i, row) in g.ui_font_rows.iter().enumerate() {
            if point_in(row, cx, cy) {
                return MouseAction::SetUiFont(g.ui_font_scroll_offset + i);
            }
        }
        // ── Effects-tab widgets ───────────────────────────────────────────────
        // Rects are at 1e6 when the Effects tab is not active, so these tests are
        // effectively no-ops on every other tab.
        //
        // Viewport guard: clicks in the title/tab-strip chrome area (above
        // content_top) or below the panel bottom (content_bottom) must NOT fire
        // Effects widget actions even if a widget rect has scrolled into that zone.
        // We only apply this guard when the panel is on-screen
        // (content_top < 1e5 — Y-viewport bound only; cross-tab isolation
        // relies on the OFF-sentinel rects, not this check).
        let in_effects_viewport = g.content_top < 1.0e5
            && cy >= g.content_top
            && cy < g.content_bottom
            && cx >= g.panel.x
            && cx < g.panel.x + g.panel.w;

        if in_effects_viewport {
            // Toggle pills.
            if point_in(&g.crt_enabled_toggle, cx, cy) {
                return MouseAction::ToggleCrt;
            }
            if point_in(&g.crt_roll_toggle, cx, cy) {
                return MouseAction::ToggleCrtRoll;
            }
            if point_in(&g.crt_flicker_toggle, cx, cy) {
                return MouseAction::ToggleCrtFlicker;
            }
            if point_in(&g.crt_jitter_toggle, cx, cy) {
                return MouseAction::ToggleCrtJitter;
            }
            if point_in(&g.caret_flash_toggle, cx, cy) {
                return MouseAction::ToggleCaretFlash;
            }
            if point_in(&g.caret_glow_toggle, cx, cy) {
                return MouseAction::ToggleCaretGlow;
            }
            // Effects tab sliders: handle OR track → start drag.
            if point_in(&g.crt_curvature_handle, cx, cy) || point_in(&g.crt_curvature_track, cx, cy) {
                return MouseAction::StartCrtCurvatureDrag;
            }
            if point_in(&g.crt_scanline_handle, cx, cy) || point_in(&g.crt_scanline_track, cx, cy) {
                return MouseAction::StartScanlineDrag;
            }
            if point_in(&g.crt_mask_handle, cx, cy) || point_in(&g.crt_mask_track, cx, cy) {
                return MouseAction::StartMaskDrag;
            }
            if point_in(&g.crt_bloom_handle, cx, cy) || point_in(&g.crt_bloom_track, cx, cy) {
                return MouseAction::StartBloomDrag;
            }
            if point_in(&g.crt_chromatic_handle, cx, cy) || point_in(&g.crt_chromatic_track, cx, cy) {
                return MouseAction::StartChromaticDrag;
            }
            if point_in(&g.crt_vignette_handle, cx, cy) || point_in(&g.crt_vignette_track, cx, cy) {
                return MouseAction::StartVignetteDrag;
            }
            // CRT scanline-tint RGB mini-sliders.
            if point_in(&g.crt_tint_r_handle, cx, cy) || point_in(&g.crt_tint_r_track, cx, cy) {
                return MouseAction::StartTintRDrag;
            }
            if point_in(&g.crt_tint_g_handle, cx, cy) || point_in(&g.crt_tint_g_track, cx, cy) {
                return MouseAction::StartTintGDrag;
            }
            if point_in(&g.crt_tint_b_handle, cx, cy) || point_in(&g.crt_tint_b_track, cx, cy) {
                return MouseAction::StartTintBDrag;
            }
            // Caret flash-duration slider.
            if point_in(&g.caret_dur_handle, cx, cy) || point_in(&g.caret_dur_track, cx, cy) {
                return MouseAction::StartCaretDurDrag;
            }
            // Caret flash-color RGB mini-sliders.
            if point_in(&g.caret_color_r_handle, cx, cy) || point_in(&g.caret_color_r_track, cx, cy) {
                return MouseAction::StartCaretColorRDrag;
            }
            if point_in(&g.caret_color_g_handle, cx, cy) || point_in(&g.caret_color_g_track, cx, cy) {
                return MouseAction::StartCaretColorGDrag;
            }
            if point_in(&g.caret_color_b_handle, cx, cy) || point_in(&g.caret_color_b_track, cx, cy) {
                return MouseAction::StartCaretColorBDrag;
            }
        }
        // Title bar (top ~36px) — drag handle; must come before generic consume.
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

/// Map a physical key to its Ctrl control byte: Ctrl+A=1 .. Ctrl+Z=26 (so
/// Ctrl+C=3=SIGINT, Ctrl+D=4=EOF, Ctrl+Z=26, Ctrl+L=12=clear), plus the remaining
/// C0 symbol combos: Ctrl+Space=0x00 (NUL), Ctrl+[=0x1b (ESC), Ctrl+\=0x1c (FS),
/// Ctrl+]=0x1d (GS). Uses the physical key position, so it is independent of the
/// keyboard layout.
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

/// Encode the four arrow keys honoring DECCKM (application cursor mode).
///
/// Returns `None` for any non-arrow key. When `app_cursor` is true the SS3
/// prefix (`\eO`) is used; otherwise the default CSI prefix (`\e[`) is used:
///
/// | key        | normal (CSI) | app_cursor (SS3) |
/// |------------|--------------|------------------|
/// | ArrowUp    | `\e[A`       | `\eOA`           |
/// | ArrowDown  | `\e[B`       | `\eOB`           |
/// | ArrowRight | `\e[C`       | `\eOC`           |
/// | ArrowLeft  | `\e[D`       | `\eOD`           |
pub fn cursor_key_bytes(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let final_byte = match key {
        Key::Named(NamedKey::ArrowUp) => b'A',
        Key::Named(NamedKey::ArrowDown) => b'B',
        Key::Named(NamedKey::ArrowRight) => b'C',
        Key::Named(NamedKey::ArrowLeft) => b'D',
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
/// Arrow keys here always use the default CSI (`\e[`) encoding. Callers that
/// need DECCKM-aware arrows should use [`cursor_key_bytes`] (or [`decide_key`],
/// which routes arrows through it).
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
        // and every ncurses TUI. Home/End use the CSI (`\e[H`/`\e[F`) form.
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
        // Function row. F1–F4 use the SS3 (`\eOP`..`\eOS`) form; F5–F12 the CSI
        // tilde form. (F9 is normally consumed by the global summon hotkey before
        // it reaches here; this is the fallback when no global grab is active.)
        Key::Named(NamedKey::F1) => Some(b"\x1bOP".to_vec()),
        Key::Named(NamedKey::F2) => Some(b"\x1bOQ".to_vec()),
        Key::Named(NamedKey::F3) => Some(b"\x1bOR".to_vec()),
        Key::Named(NamedKey::F4) => Some(b"\x1bOS".to_vec()),
        Key::Named(NamedKey::F5) => Some(b"\x1b[15~".to_vec()),
        Key::Named(NamedKey::F6) => Some(b"\x1b[17~".to_vec()),
        Key::Named(NamedKey::F7) => Some(b"\x1b[18~".to_vec()),
        Key::Named(NamedKey::F8) => Some(b"\x1b[19~".to_vec()),
        Key::Named(NamedKey::F9) => Some(b"\x1b[20~".to_vec()),
        Key::Named(NamedKey::F10) => Some(b"\x1b[21~".to_vec()),
        Key::Named(NamedKey::F11) => Some(b"\x1b[23~".to_vec()),
        Key::Named(NamedKey::F12) => Some(b"\x1b[24~".to_vec()),
        Key::Character(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
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
/// `(line, col, left_half)` CLAMPED to the grid. `y_grid` is grid-relative
/// (the caller subtracts the bar origin and floors at 0). `left_half` is
/// whether the pointer sits in the LEFT half of its cell — selection
/// start/update derive the endpoint `Side` from it (F4): hardcoding
/// Left-at-press / Right-at-update dropped the endpoint cells on a reverse
/// drag. Callers guarantee `cell_w`/`cell_h` > 0.
pub fn cell_at_0_side(
    x: f32,
    y_grid: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize, bool) {
    let col_f = (x / cell_w).floor();
    let col = (col_f as i64).clamp(0, cols.saturating_sub(1) as i64) as usize;
    let line = ((y_grid / cell_h).floor() as i64).clamp(0, rows.saturating_sub(1) as i64) as usize;
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
/// * the logical key is a plain `Character` (Named keys like Enter/Tab produce
///   control text that must keep going through `key_to_bytes`),
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
    let Key::Character(s) = logical else {
        return None;
    };
    let t = text?;
    if t.is_empty() || t == s.as_str() || t.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(t.as_bytes().to_vec())
}

/// A mouse event to encode for an application that enabled mouse reporting.
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

/// The button-code half of a mouse event, shared by both the SGR and X10
/// encoders so the two formats can never disagree about which button a given
/// `MouseEvent` maps to. Returns `(button, is_release)`.
///
/// Left press/release use button 0; the wheel uses 64 (up) / 65 (down). Only a
/// left release is a "release" event (terminator `m` in SGR); wheel events are
/// always reported as a press (`M`).
fn mouse_button_code(event: MouseEvent) -> (u8, bool) {
    match event {
        MouseEvent::LeftPress => (0, false),
        MouseEvent::LeftRelease => (0, true),
        MouseEvent::WheelUp => (64, false),
        MouseEvent::WheelDown => (65, false),
        // Motion adds the 0x20 (32) "motion" bit to the held-button base code:
        // left-drag → 32, no-button any-motion → 35. Reported as a press (`M`).
        MouseEvent::Motion { button } => (button.wrapping_add(32), false),
    }
}

/// Encode a mouse event in the format the running application requested.
///
/// When `sgr` is true the application enabled SGR (1006) reporting
/// (`\e[?1006h`); use [`encode_sgr_mouse`]. Otherwise fall back to the legacy
/// X10 encoding via [`encode_x10_mouse`]. This is the single dispatch point so
/// callers never have to branch on the mode themselves.
pub fn encode_mouse(event: MouseEvent, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    if sgr {
        encode_sgr_mouse(event, col, row)
    } else {
        encode_x10_mouse(event, col, row)
    }
}

/// Encode a mouse event as an SGR (1006) mouse report.
///
/// Format: `\e[<Cb;Cx;CyM` for a press/motion and `\e[<Cb;Cx;Cym` for a
/// release. `Cb` is the button code, `Cx`/`Cy` are 1-based cell coordinates.
/// Wheel events always use the press terminator (`M`) per the xterm protocol.
///
/// `col`/`row` are 1-based cell coordinates. They are clamped to a minimum of 1
/// so a click at the very edge never produces a 0 coordinate.
pub fn encode_sgr_mouse(event: MouseEvent, col: usize, row: usize) -> Vec<u8> {
    let col = col.max(1);
    let row = row.max(1);
    let (button, is_release) = mouse_button_code(event);
    let terminator = if is_release { 'm' } else { 'M' };
    format!("\x1b[<{button};{col};{row}{terminator}").into_bytes()
}

/// Encode a mouse event as a legacy X10 mouse report.
///
/// Format: `\e[M` followed by exactly three bytes: `32 + button`, `32 + col`,
/// `32 + row`, where `col`/`row` are 1-based cell coordinates. Each coordinate
/// is clamped to 223 so `32 + coord` never exceeds 255 (the maximum a single
/// byte can hold); coordinates beyond that are simply not representable in X10.
///
/// X10 has no separate release encoding per button: a release is reported as
/// button 3 (`32 + 3`). Wheel events carry the 0x40 (64) "extra button" bit, so
/// `32 + 64` for wheel up and `32 + 65` for wheel down, matching the SGR button
/// numbers.
pub fn encode_x10_mouse(event: MouseEvent, col: usize, row: usize) -> Vec<u8> {
    // Clamp to 1..=223 so the +32 offset stays within a single byte (<=255).
    let col = col.clamp(1, 223) as u8;
    let row = row.clamp(1, 223) as u8;
    let (button, is_release) = mouse_button_code(event);
    // Legacy X10 reports every button release as button 3; only the position
    // and the release indicator survive (no per-button release encoding).
    let button = if is_release { 3 } else { button };
    vec![
        0x1b,
        b'[',
        b'M',
        32u8.wrapping_add(button),
        32 + col,
        32 + row,
    ]
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

    #[test]
    fn ctrl_equal_maps_to_font_up() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Equal),
            &make_logical_char("="),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontUp);
    }

    #[test]
    fn ctrl_minus_maps_to_font_down() {
        let action = decide_key(
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
        let action = decide_key(
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
        let action = decide_key(
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
        let action = decide_key(
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
        let z = decide_key(
            true, false, false,
            make_physical(KeyCode::KeyZ),
            &make_logical_char("z"),
            false, false, false,
        );
        assert_eq!(z, KeyAction::Send(vec![0x1a]));
        let x = decide_key(
            true, false, false,
            make_physical(KeyCode::KeyX),
            &make_logical_char("x"),
            false, false, false,
        );
        assert_eq!(x, KeyAction::Send(vec![0x18]));
    }

    #[test]
    fn turkish_q_ctrl_shift_minus_is_opacity_down_not_up() {
        // Regression (F21): the opacity chord must follow the engraved key too.
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::Equal),
            &make_logical_char("-"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::OpacityDown);
    }

    #[test]
    fn qwertz_ctrl_minus_is_font_down_not_literal() {
        // Regression (F21): on German QWERTZ '-' sits at physical Slash; keying
        // on the logical char makes Ctrl+'-' shrink the font instead of typing
        // a literal '-' into the shell.
        let action = decide_key(
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
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Digit0),
            &make_logical_char("0"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontReset);
    }

    #[test]
    fn ctrl_shift_equal_still_opacity_up() {
        // Ctrl+Shift+Equal must remain OpacityUp even after adding FontUp.
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::Equal),
            &make_logical_char("="),
            false, false, false,
        );
        assert_eq!(action, KeyAction::OpacityUp);
    }

    #[test]
    fn ctrl_shift_p_opens_the_command_palette() {
        // Ctrl+Shift+P is repurposed from Settings to the command palette.
        let action = decide_key(
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
        let action = decide_key(
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
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::KeyP),
            &make_logical_char("p"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x10]));
    }

    #[test]
    fn ctrl_shift_t_maps_to_new_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::KeyT),
            &make_logical_char("T"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::NewTab);
    }

    #[test]
    fn ctrl_shift_w_maps_to_close_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::KeyW),
            &make_logical_char("W"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::CloseTab);
    }

    #[test]
    fn ctrl_shift_d_maps_to_detach_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::KeyD),
            &make_logical_char("D"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::DetachTab);
    }

    #[test]
    fn ctrl_shift_f_is_search_toggle() {
        let action = decide_key(
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
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::KeyF),
            &make_logical_char("f"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x06]));
    }

    #[test]
    fn ctrl_tab_maps_to_next_tab() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false, false,
        );
        assert_eq!(action, KeyAction::NextTab);
    }

    #[test]
    fn ctrl_shift_tab_maps_to_prev_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false, false,
        );
        assert_eq!(action, KeyAction::PrevTab);
    }

    #[test]
    fn ctrl_digit_maps_to_select_tab() {
        let action = decide_key(
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
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Digit0),
            &make_logical_char("0"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::FontReset);
    }

    // ── PageUp/PageDown: alt-screen aware (C1) ──────────────────────────────

    #[test]
    fn page_keys_scroll_on_primary_screen() {
        let up = decide_key(
            false, false, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, false,
        );
        assert_eq!(up, KeyAction::ScrollPageUp);
        let down = decide_key(
            false, false, false,
            make_physical(KeyCode::PageDown),
            &Key::Named(NamedKey::PageDown),
            false, false, false,
        );
        assert_eq!(down, KeyAction::ScrollPageDown);
    }

    #[test]
    fn page_keys_forward_to_pty_on_alt_screen() {
        // less/vim/htop (alt screen): plain PageUp/Down must reach the app.
        let up = decide_key(
            false, false, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, true,
        );
        assert_eq!(up, KeyAction::Send(b"\x1b[5~".to_vec()));
        let down = decide_key(
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
        let up = decide_key(
            false, true, false,
            make_physical(KeyCode::PageUp),
            &Key::Named(NamedKey::PageUp),
            false, false, true,
        );
        assert_eq!(up, KeyAction::ScrollPageUp);
        let down = decide_key(
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
        let action = decide_key(
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
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::IntlRo),
            &make_logical_char("_"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(vec![0x1f]));
    }

    #[test]
    fn ctrl_shift_minus_still_opacity_down() {
        // US: Ctrl+Shift+Minus stays the OpacityDown app shortcut even though
        // its logical character is "_" (the explicit chord wins).
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::Minus),
            &make_logical_char("_"),
            false, false, false,
        );
        assert_eq!(action, KeyAction::OpacityDown);
    }

    #[test]
    fn ctrl_minus_on_physical_slash_is_not_unit_separator() {
        // German layouts: physical Slash produces "-". Ctrl+- there must NOT be
        // hijacked as 0x1f (it is keyed on the logical character, not position).
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Slash),
            &make_logical_char("-"),
            false, false, false,
        );
        assert_ne!(action, KeyAction::Send(vec![0x1f]));
    }

    // ── Modified Home/End/Delete/Insert (C5) ────────────────────────────────

    #[test]
    fn shift_home_end_send_modified_csi() {
        let home = decide_key(
            false, true, false,
            make_physical(KeyCode::Home),
            &Key::Named(NamedKey::Home),
            false, false, false,
        );
        assert_eq!(home, KeyAction::Send(b"\x1b[1;2H".to_vec()));
        let end = decide_key(
            false, true, false,
            make_physical(KeyCode::End),
            &Key::Named(NamedKey::End),
            false, false, false,
        );
        assert_eq!(end, KeyAction::Send(b"\x1b[1;2F".to_vec()));
    }

    #[test]
    fn ctrl_delete_sends_modified_tilde_form() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Delete),
            &Key::Named(NamedKey::Delete),
            false, false, false,
        );
        assert_eq!(action, KeyAction::Send(b"\x1b[3;5~".to_vec()));
    }

    #[test]
    fn alt_insert_sends_modified_tilde_form() {
        let action = decide_key(
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
            let action = decide_key(
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

    // ── UI-font panel hit-tests ──────────────────────────────────────────────
    // Build a real panel (large screen so it isn't clamped) and verify clicks on
    // the new UI-font widgets decode to the right MouseAction. Using build_panel
    // keeps the geometry in lockstep with the renderer (no hand-rolled rects).

    /// A representative panel with 5 UI-font families (incl. the synthetic
    /// "System Sans" row) so the family list and its scroll math are exercised.
    fn panel_for_tab(active_tab: usize) -> jetty_render::PanelView {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let mono: Vec<String> = vec!["JetBrains Mono".into(), "Fira Code".into()];
        let ui: Vec<String> = vec![
            "System Sans (default)".into(),
            "Inter".into(),
            "Noto Sans".into(),
            "DejaVu Sans".into(),
            "Cantarell".into(),
        ];
        jetty_render::build_panel(
            1920, 1280, 0.97, 0, 15.0, &mono, "JetBrains Mono", 0,
            8.0, "Phosphor", "Center", "Top", "10k", 0.5, 1.0, false, true,
            false, // launch_at_login
            18.0, &ui, "", 0,
            0.0, 0.0, &theme, 9.8, // char_w scale-1 fallback
            "System default", // shell_display
            &jetty_render::NotifyParams::default(), // Run & Notify (v0.15)
            active_tab,
            &jetty_render::EffectsParams::default(),
            0.0, // effects_scroll (default: top)
            false, // theme_dropdown_open
            0, // theme_scroll_offset
        )
    }

    /// The Fonts tab (1): font + UI-font widgets are laid out here.
    fn ui_panel() -> jetty_render::PanelView {
        panel_for_tab(1)
    }

    /// Decode a click at the center of `rect` against the panel geometry.
    fn click_rect(g: &jetty_render::PanelGeom, rect: &jetty_render::Rect) -> MouseAction {
        decide_mouse_press(Some(g), None, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
    }

    #[test]
    fn ui_font_size_buttons_hit_test() {
        let pv = ui_panel();
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.ui_font_minus), MouseAction::UiFontMinus);
        assert_eq!(click_rect(g, &g.ui_font_plus), MouseAction::UiFontPlus);
        assert_eq!(click_rect(g, &g.ui_font_reset), MouseAction::UiFontReset);
    }

    #[test]
    fn ui_font_scroll_buttons_hit_test() {
        let pv = ui_panel();
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.ui_font_scroll_up), MouseAction::UiFontScrollUp);
        assert_eq!(click_rect(g, &g.ui_font_scroll_down), MouseAction::UiFontScrollDown);
    }

    #[test]
    fn ui_font_rows_hit_test_to_offset_plus_index() {
        let pv = ui_panel();
        let g = &pv.geom;
        // Row 0 maps to SetUiFont(offset+0) = SetUiFont(0) (the System Sans row).
        let r0 = g.ui_font_rows[0];
        assert_eq!(
            decide_mouse_press(Some(g), None, r0.x + 4.0, r0.y + r0.h / 2.0),
            MouseAction::SetUiFont(g.ui_font_scroll_offset),
        );
        // Row 2 maps to SetUiFont(offset+2).
        let r2 = g.ui_font_rows[2];
        assert_eq!(
            decide_mouse_press(Some(g), None, r2.x + 4.0, r2.y + r2.h / 2.0),
            MouseAction::SetUiFont(g.ui_font_scroll_offset + 2),
        );
    }

    #[test]
    fn shell_cycle_buttons_hit_test() {
        // The shell band lives on the Shell tab (3).
        let pv = panel_for_tab(3);
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.shell_prev), MouseAction::CycleShellPrev);
        assert_eq!(click_rect(g, &g.shell_next), MouseAction::CycleShellNext);
    }

    #[test]
    fn notify_controls_hit_test() {
        // RUN & NOTIFY controls also live on the Shell tab (3, v0.15).
        let pv = panel_for_tab(3);
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.notify_toggle), MouseAction::ToggleNotifyOnFinish);
        assert_eq!(
            click_rect(g, &g.notify_failure_toggle),
            MouseAction::ToggleNotifyOnlyFailure
        );
        assert_eq!(click_rect(g, &g.notify_dur_prev), MouseAction::NotifyDurPrev);
        assert_eq!(click_rect(g, &g.notify_dur_next), MouseAction::NotifyDurNext);
        assert_eq!(click_rect(g, &g.auto_summon_toggle), MouseAction::ToggleAutoSummon);
    }

    #[test]
    fn scrollback_cycler_clicks_decode() {
        // The scrollback band lives on the Window tab (2).
        let pv = panel_for_tab(2);
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.scrollback_prev), MouseAction::ScrollbackPrev);
        assert_eq!(click_rect(g, &g.scrollback_next), MouseAction::ScrollbackNext);
        // A click on the cycler BODY between the chevrons is not a scrollback
        // action (the value area is inert).
        let mid_x = (g.scrollback_prev.x + g.scrollback_prev.w
            + g.scrollback_next.x) / 2.0;
        let mid_y = g.scrollback_prev.y + g.scrollback_prev.h / 2.0;
        let act = decide_mouse_press(Some(g), None, mid_x, mid_y);
        assert!(
            act != MouseAction::ScrollbackPrev && act != MouseAction::ScrollbackNext,
            "cycler body must not decode to a scrollback action, got {act:?}"
        );
    }

    #[test]
    fn tab_strip_clicks_select_tab() {
        // A click on tab label i decodes to SetSettingsTab(i), regardless of which
        // tab is currently active.
        let pv = panel_for_tab(0);
        let g = &pv.geom;
        for i in 0..4 {
            assert_eq!(click_rect(g, &g.tab_rects[i]), MouseAction::SetSettingsTab(i));
        }
    }

    #[test]
    fn terminal_font_widgets_still_hit_test_after_ui_insert() {
        // Regression: the UI-font hit-tests are inserted AFTER the terminal-font
        // ones, so the terminal-font widgets must still decode correctly.
        let pv = ui_panel();
        let g = &pv.geom;
        assert_eq!(click_rect(g, &g.font_minus), MouseAction::FontMinus);
        assert_eq!(click_rect(g, &g.font_plus), MouseAction::FontPlus);
        let r0 = g.font_rows[0];
        assert_eq!(
            decide_mouse_press(Some(g), None, r0.x + 4.0, r0.y + r0.h / 2.0),
            MouseAction::SetFont(g.font_scroll_offset),
        );
    }
}
