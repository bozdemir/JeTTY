use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// High-level action decoded from a key press event.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    TogglePanel,
    ClosePanel,
    CycleTheme,
    OpacityUp,
    OpacityDown,
    ScrollPageUp,
    ScrollPageDown,
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
///
/// The rules mirror `app.rs` exactly:
/// 1. Ctrl+, (no shift)         → TogglePanel
/// 2. Escape                    → ClosePanel if panel open, else Send(ESC)
/// 3. Ctrl+Shift+T              → CycleTheme
/// 4. Ctrl+Shift+Equal          → OpacityUp
/// 5. Ctrl+Shift+Minus          → OpacityDown
/// 6. PageUp                    → ScrollPageUp
/// 7. PageDown                  → ScrollPageDown
/// 8. Ctrl+<symbol> (Space/[/\\/]) → control byte (0x00, 0x1b, 0x1c, 0x1d)
/// 9. Alt+<key> that yields bytes  → ESC-prefixed Send(esc + bytes)
/// 10. Otherwise: key_to_bytes  → Send(bytes) or None
pub fn decide_key(
    ctrl: bool,
    shift: bool,
    alt: bool,
    physical: PhysicalKey,
    logical: &Key,
    panel_open: bool,
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
            PhysicalKey::Code(KeyCode::KeyO) => return KeyAction::TogglePanel,
            PhysicalKey::Code(KeyCode::KeyT) => return KeyAction::CycleTheme,
            PhysicalKey::Code(KeyCode::Equal) => return KeyAction::OpacityUp,
            PhysicalKey::Code(KeyCode::Minus) => return KeyAction::OpacityDown,
            _ => {}
        }
    }

    // Rules 6-7: PageUp / PageDown.
    match logical {
        Key::Named(NamedKey::PageUp) => return KeyAction::ScrollPageUp,
        Key::Named(NamedKey::PageDown) => return KeyAction::ScrollPageDown,
        _ => {}
    }

    // Ctrl+<letter> → control byte (Ctrl+C = 0x03 SIGINT, Ctrl+D = EOF, Ctrl+Z,
    // Ctrl+L clear, ...). Also the remaining "C0" symbol combos: Ctrl+Space = NUL
    // (0x00), Ctrl+[ = ESC (0x1b), Ctrl+\ = FS (0x1c), Ctrl+] = GS (0x1d). Keyed by
    // PHYSICAL position so it is layout-independent. Must come before the plain
    // key_to_bytes fallback, which would otherwise send the literal character
    // instead of the control code. (Our own shortcuts use Ctrl+Shift, so they are
    // already handled above and never reach here.)
    if ctrl && !shift {
        if let PhysicalKey::Code(code) = physical {
            if let Some(b) = ctrl_byte(code) {
                return KeyAction::Send(vec![b]);
            }
        }
    }

    // Rule 9 + fallback: convert the key to its byte sequence. When Alt/Meta is
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
    /// User clicked a theme chip. The index is into `jetty_core::theme::PRESETS`.
    SetTheme(usize),
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
/// Priority matches `app.rs`:
/// 1. If panel open: slider handle/track → StartSliderDrag
/// 2. If panel open: theme chip i        → SetTheme(i)
/// 3. If panel open: inside panel rect   → ConsumePanel
/// 4. (Falls through to scrollbar when click is outside open panel)
/// 5. Inside scrollbar thumb             → StartScrollbarDrag
/// 6. Inside scrollbar track x-range     → ScrollbarTrackJump
/// 7. Anything else                      → None
pub fn decide_mouse_press(
    panel: Option<&jetty_render::PanelGeom>,
    scrollbar: Option<&jetty_render::Rect>,
    cx: f32,
    cy: f32,
) -> MouseAction {
    if let Some(g) = panel {
        // Slider handle or track → start drag.
        if point_in(&g.slider_handle, cx, cy) || point_in(&g.slider_track, cx, cy) {
            return MouseAction::StartSliderDrag;
        }
        // Theme chips.
        for (i, chip) in g.chips.iter().enumerate() {
            if point_in(chip, cx, cy) {
                return MouseAction::SetTheme(i);
            }
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

/// Translate a winit logical key into the byte sequence a terminal expects.
/// This is the single source of truth — both `app.rs` and tests use it.
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
        Key::Character(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}
