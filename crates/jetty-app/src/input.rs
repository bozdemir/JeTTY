use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// High-level action decoded from a key press event.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    TogglePanel,
    ClosePanel,
    /// Open a new terminal tab (Ctrl+Shift+T).
    NewTab,
    /// Close the active tab (Ctrl+Shift+W).
    CloseTab,
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
///
/// The rules mirror `app.rs` exactly:
/// 1. Ctrl+, (no shift)         → TogglePanel
/// 2. Escape                    → ClosePanel if panel open, else Send(ESC)
/// 3. Ctrl+Shift+O              → TogglePanel
/// 4. Ctrl+Shift+T              → CycleTheme
/// 5. Ctrl+Shift+Equal          → OpacityUp
/// 6. Ctrl+Shift+Minus          → OpacityDown
/// 7. PageUp                    → ScrollPageUp
/// 8. PageDown                  → ScrollPageDown
/// 9. Ctrl+<letter/symbol> → control byte, regardless of shift (the explicit
///    Ctrl+Shift shortcuts in rules 3-6 are intercepted first). When Alt is also
///    held, the control byte is ESC-prefixed (Ctrl+Alt+b → ESC + 0x02).
/// 10. Alt+<key> that yields bytes → ESC-prefixed Send(esc + bytes)
/// 11. Otherwise: key_to_bytes  → Send(bytes) or None
pub fn decide_key(
    ctrl: bool,
    shift: bool,
    alt: bool,
    physical: PhysicalKey,
    logical: &Key,
    panel_open: bool,
    app_cursor: bool,
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
    if matches!(logical, Key::Named(NamedKey::Escape)) {
        if panel_open {
            return KeyAction::ClosePanel;
        }
        // Fall through: key_to_bytes(Escape) will produce Send(vec![0x1b]).
    }

    // Rules 3-5 + panel toggle: Ctrl+Shift hotkeys keyed by PHYSICAL key, which
    // is layout-independent. Ctrl+Shift+O toggles the Settings panel and works on
    // every layout — unlike Ctrl+, which on a Turkish layout reports as Backslash
    // (not Comma), so it never matched.
    if ctrl && shift {
        match physical {
            // KeyP is the dedicated "open Settings dialog" hotkey.
            // KeyO is kept as an alias for backwards compatibility.
            PhysicalKey::Code(KeyCode::KeyP) => return KeyAction::TogglePanel,
            PhysicalKey::Code(KeyCode::KeyO) => return KeyAction::TogglePanel,
            // Tabs: Ctrl+Shift+T opens a new tab; Ctrl+Shift+W closes the active
            // one. Theme switching moved to the Settings window. Ctrl+Shift+Tab
            // cycles to the previous tab (must be intercepted before ctrl_byte).
            PhysicalKey::Code(KeyCode::KeyT) => return KeyAction::NewTab,
            PhysicalKey::Code(KeyCode::KeyW) => return KeyAction::CloseTab,
            PhysicalKey::Code(KeyCode::Tab) => return KeyAction::PrevTab,
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

    // Rules 6-7: PageUp / PageDown.
    match logical {
        Key::Named(NamedKey::PageUp) => return KeyAction::ScrollPageUp,
        Key::Named(NamedKey::PageDown) => return KeyAction::ScrollPageDown,
        _ => {}
    }

    // Font-size bindings: Ctrl (no shift) + Equal/Minus/Digit0.
    // These must be checked BEFORE the ctrl_byte fallback so they are never
    // swallowed as a raw control code. Ctrl+Shift+Equal/Minus are already
    // handled above as OpacityUp/Down and never reach here.
    if ctrl && !shift {
        match physical {
            PhysicalKey::Code(KeyCode::Equal) => return KeyAction::FontUp,
            PhysicalKey::Code(KeyCode::Minus) => return KeyAction::FontDown,
            PhysicalKey::Code(KeyCode::Digit0) => return KeyAction::FontReset,
            _ => {}
        }
    }

    // Ctrl+<letter> → control byte (Ctrl+C = 0x03 SIGINT, Ctrl+D = EOF, Ctrl+Z,
    // Ctrl+L clear, ...). Also the remaining "C0" symbol combos: Ctrl+Space = NUL
    // (0x00), Ctrl+[ = ESC (0x1b), Ctrl+\ = FS (0x1c), Ctrl+] = GS (0x1d). Keyed by
    // PHYSICAL position so it is layout-independent. Must come before the plain
    // key_to_bytes fallback, which would otherwise send the literal character
    // instead of the control code.
    //
    // Applies REGARDLESS of shift: Ctrl+Shift+C == Ctrl+C for control purposes
    // (both → 0x03). The explicit Ctrl+Shift app shortcuts (O/T/Equal/Minus) are
    // intercepted above and never reach here, so they keep their special meaning.
    //
    // When Alt/Meta is also held, the control byte is ESC-prefixed (the classic
    // "Meta sends Escape" convention), e.g. Ctrl+Alt+b → ESC + 0x02.
    if ctrl {
        if let PhysicalKey::Code(code) = physical {
            if let Some(b) = ctrl_byte(code) {
                if alt {
                    return KeyAction::Send(vec![0x1b, b]);
                }
                return KeyAction::Send(vec![b]);
            }
        }
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
    /// User clicked a theme chip. The index is into `jetty_core::theme::PRESETS`.
    SetTheme(usize),
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
    /// User pressed on the Dropdown-height slider handle or track — start drag.
    StartDropdownDrag,
    /// User pressed on the Dropdown-width slider handle or track — start drag.
    StartDropdownWidthDrag,
    /// User clicked the "Auto-hide on focus loss" toggle pill.
    ToggleFocusAutoHide,
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
/// 2. If panel open: theme chip i        → SetTheme(i)
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
        // Opacity slider handle or track → start drag.
        if point_in(&g.slider_handle, cx, cy) || point_in(&g.slider_track, cx, cy) {
            return MouseAction::StartSliderDrag;
        }
        // Corner-radius slider handle or track → start drag.
        if point_in(&g.radius_handle, cx, cy) || point_in(&g.radius_track, cx, cy) {
            return MouseAction::StartRadiusDrag;
        }
        // Theme chips.
        for (i, chip) in g.chips.iter().enumerate() {
            if point_in(chip, cx, cy) {
                return MouseAction::SetTheme(i);
            }
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
        // Font-family list rows.
        for (i, row) in g.font_rows.iter().enumerate() {
            if point_in(row, cx, cy) {
                return MouseAction::SetFont(g.font_scroll_offset + i);
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
            false, false,
        );
        assert_eq!(action, KeyAction::FontUp);
    }

    #[test]
    fn ctrl_minus_maps_to_font_down() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Minus),
            &make_logical_char("-"),
            false, false,
        );
        assert_eq!(action, KeyAction::FontDown);
    }

    #[test]
    fn ctrl_digit0_maps_to_font_reset() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Digit0),
            &make_logical_char("0"),
            false, false,
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
            false, false,
        );
        assert_eq!(action, KeyAction::OpacityUp);
    }

    #[test]
    fn ctrl_shift_t_maps_to_new_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::KeyT),
            &make_logical_char("T"),
            false, false,
        );
        assert_eq!(action, KeyAction::NewTab);
    }

    #[test]
    fn ctrl_shift_w_maps_to_close_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::KeyW),
            &make_logical_char("W"),
            false, false,
        );
        assert_eq!(action, KeyAction::CloseTab);
    }

    #[test]
    fn ctrl_tab_maps_to_next_tab() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false,
        );
        assert_eq!(action, KeyAction::NextTab);
    }

    #[test]
    fn ctrl_shift_tab_maps_to_prev_tab() {
        let action = decide_key(
            true, true, false,
            make_physical(KeyCode::Tab),
            &Key::Named(NamedKey::Tab),
            false, false,
        );
        assert_eq!(action, KeyAction::PrevTab);
    }

    #[test]
    fn ctrl_digit_maps_to_select_tab() {
        let action = decide_key(
            true, false, false,
            make_physical(KeyCode::Digit3),
            &make_logical_char("3"),
            false, false,
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
            false, false,
        );
        assert_eq!(action, KeyAction::FontReset);
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
}
