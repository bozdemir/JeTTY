//! Headless unit tests for keyboard and mouse input decision logic.
//! No window, no GPU, no display required.

use jetty_app::input::{decide_key, decide_mouse_press, KeyAction, MouseAction};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

// ---------------------------------------------------------------------------
// Helper: wrap a KeyCode in PhysicalKey::Code
// ---------------------------------------------------------------------------
fn phys(code: KeyCode) -> PhysicalKey {
    PhysicalKey::Code(code)
}

// ---------------------------------------------------------------------------
// decide_key tests
// ---------------------------------------------------------------------------

#[test]
fn ctrl_comma_physical_toggles_panel_closed() {
    // THE Ctrl+, fix: physical Comma, no shift, panel closed.
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::Comma),
        &Key::Character(",".into()),
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_comma_physical_toggles_panel_open() {
    // Ctrl+, also toggles when panel is already open.
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::Comma),
        &Key::Character(",".into()),
        true,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_comma_logical_fallback_toggles_panel() {
    // Fallback: physical key unknown but logical produces ",".
    let action = decide_key(
        true,
        false,
        false,
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
        &Key::Character(",".into()),
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_shift_o_toggles_panel() {
    // Layout-independent panel toggle. Works on the Turkish layout, where the
    // comma key reports to winit as Backslash (not Comma) so Ctrl+, never matched.
    let action = decide_key(
        true,
        true,
        false,
        phys(KeyCode::KeyO),
        &Key::Character("O".into()),
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_c_sends_sigint() {
    // Ctrl+C must send 0x03 (SIGINT), not the literal letter "c".
    let a = decide_key(true, false, false, phys(KeyCode::KeyC), &Key::Character("c".into()), false);
    assert_eq!(a, KeyAction::Send(vec![3]));
}

#[test]
fn ctrl_letters_send_control_bytes() {
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyD), &Key::Character("d".into()), false),
        KeyAction::Send(vec![4]) // Ctrl+D = EOF
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyZ), &Key::Character("z".into()), false),
        KeyAction::Send(vec![26]) // Ctrl+Z = suspend
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyL), &Key::Character("l".into()), false),
        KeyAction::Send(vec![12]) // Ctrl+L = clear
    );
}

#[test]
fn escape_closes_open_panel() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::Escape),
        &Key::Named(NamedKey::Escape),
        true,
    );
    assert_eq!(action, KeyAction::ClosePanel);
}

#[test]
fn escape_sends_esc_byte_when_panel_closed() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::Escape),
        &Key::Named(NamedKey::Escape),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b]));
}

#[test]
fn ctrl_shift_t_cycles_theme() {
    let action = decide_key(
        true,
        true,
        false,
        phys(KeyCode::KeyT),
        &Key::Character("T".into()),
        false,
    );
    assert_eq!(action, KeyAction::CycleTheme);
}

#[test]
fn ctrl_shift_equal_increases_opacity() {
    let action = decide_key(
        true,
        true,
        false,
        phys(KeyCode::Equal),
        &Key::Character("+".into()),
        false,
    );
    assert_eq!(action, KeyAction::OpacityUp);
}

#[test]
fn ctrl_shift_minus_decreases_opacity() {
    let action = decide_key(
        true,
        true,
        false,
        phys(KeyCode::Minus),
        &Key::Character("_".into()),
        false,
    );
    assert_eq!(action, KeyAction::OpacityDown);
}

#[test]
fn page_up_scrolls_up() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::PageUp),
        &Key::Named(NamedKey::PageUp),
        false,
    );
    assert_eq!(action, KeyAction::ScrollPageUp);
}

#[test]
fn page_down_scrolls_down() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::PageDown),
        &Key::Named(NamedKey::PageDown),
        false,
    );
    assert_eq!(action, KeyAction::ScrollPageDown);
}

#[test]
fn plain_s_sends_byte() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::KeyS),
        &Key::Character("s".into()),
        false,
    );
    assert_eq!(action, KeyAction::Send(b"s".to_vec()));
}

#[test]
fn enter_sends_cr() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::Enter),
        &Key::Named(NamedKey::Enter),
        false,
    );
    assert_eq!(action, KeyAction::Send(b"\r".to_vec()));
}

#[test]
fn unknown_key_returns_none() {
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::F12),
        &Key::Named(NamedKey::F12),
        false,
    );
    assert_eq!(action, KeyAction::None);
}

// ---------------------------------------------------------------------------
// Alt/Meta + key → ESC-prefixed bytes
// ---------------------------------------------------------------------------

#[test]
fn alt_b_sends_esc_prefixed_b() {
    // Alt+b → ESC b (meta sends escape). alt = true.
    let action = decide_key(
        false,
        false,
        true,
        phys(KeyCode::KeyB),
        &Key::Character("b".into()),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b, b'b']));
}

#[test]
fn alt_enter_sends_esc_prefixed_cr() {
    // Alt+Enter → ESC CR (esc + the Enter key bytes).
    let action = decide_key(
        false,
        false,
        true,
        phys(KeyCode::Enter),
        &Key::Named(NamedKey::Enter),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b, b'\r']));
}

// ---------------------------------------------------------------------------
// Remaining Ctrl + symbol combos (physical, no shift) → C0 control bytes
// ---------------------------------------------------------------------------

#[test]
fn ctrl_space_sends_nul() {
    // Ctrl+Space → 0x00 (NUL).
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::Space),
        &Key::Named(NamedKey::Space),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x00]));
}

#[test]
fn ctrl_bracket_left_sends_esc() {
    // Ctrl+[ → 0x1b (ESC).
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::BracketLeft),
        &Key::Character("[".into()),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b]));
}

#[test]
fn ctrl_backslash_sends_fs() {
    // Ctrl+\ → 0x1c (FS).
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::Backslash),
        &Key::Character("\\".into()),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1c]));
}

#[test]
fn ctrl_bracket_right_sends_gs() {
    // Ctrl+] → 0x1d (GS).
    let action = decide_key(
        true,
        false,
        false,
        phys(KeyCode::BracketRight),
        &Key::Character("]".into()),
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1d]));
}

// ---------------------------------------------------------------------------
// decide_mouse_press tests
// ---------------------------------------------------------------------------

/// Build a real PanelGeom for a 1000×640 window at 70% opacity, theme index 1.
fn make_panel_geom() -> jetty_render::PanelGeom {
    jetty_render::build_panel(1000, 640, 0.7, 1).geom
}

/// Build a scrollbar rect that is non-None (requires scroll_max > 0).
fn make_scrollbar_rect() -> jetty_render::Rect {
    // 30 rows visible, 10 lines of history, scroll_offset=5, 1000×640.
    jetty_render::scrollbar_rect_geom(30, 5, 10, 1000, 640)
        .expect("scrollbar should be Some when scroll_max > 0")
}

#[test]
fn click_slider_track_starts_drag() {
    let geom = make_panel_geom();
    let t = &geom.slider_track;
    // Click the center of the track.
    let cx = t.x + t.w / 2.0;
    let cy = t.y + t.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartSliderDrag);
}

#[test]
fn click_slider_handle_starts_drag() {
    let geom = make_panel_geom();
    let h = &geom.slider_handle;
    let cx = h.x + h.w / 2.0;
    let cy = h.y + h.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartSliderDrag);
}

#[test]
fn click_chip_2_sets_theme_2() {
    let geom = make_panel_geom();
    let chip = &geom.chips[2];
    let cx = chip.x + chip.w / 2.0;
    let cy = chip.y + chip.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::SetTheme(2));
}

#[test]
fn click_inside_panel_not_widget_consumes() {
    let geom = make_panel_geom();
    // Click somewhere in the panel background: near top-left of panel,
    // but not over any widget (slider or chip).
    let cx = geom.panel.x + 5.0;
    let cy = geom.panel.y + 5.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::ConsumePanel);
}

#[test]
fn click_scrollbar_thumb_starts_scrollbar_drag() {
    let rect = make_scrollbar_rect();
    // Click the vertical center of the thumb.
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let expected_grab_dy = cy - rect.y;
    let action = decide_mouse_press(None, Some(&rect), cx, cy);
    assert_eq!(
        action,
        MouseAction::StartScrollbarDrag { grab_dy: expected_grab_dy }
    );
}

#[test]
fn click_scrollbar_track_outside_thumb_jumps() {
    let rect = make_scrollbar_rect();
    // Click in the track x-range but above the thumb (y = 0.0).
    let cx = rect.x + rect.w / 2.0;
    let cy = 0.0; // above the thumb
    // Only applies if the thumb doesn't actually start at y=0.
    if rect.y > 0.0 {
        let action = decide_mouse_press(None, Some(&rect), cx, cy);
        assert_eq!(action, MouseAction::ScrollbarTrackJump);
    }
}

#[test]
fn click_outside_everything_is_none() {
    let action = decide_mouse_press(None, None, 100.0, 100.0);
    assert_eq!(action, MouseAction::None);
}

#[test]
fn click_outside_panel_and_scrollbar_with_panel_open_is_none() {
    let geom = make_panel_geom();
    // Click at (0,0) — well outside any widget.
    let action = decide_mouse_press(Some(&geom), None, 0.0, 0.0);
    assert_eq!(action, MouseAction::None);
}
