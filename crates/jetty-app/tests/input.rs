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
        false,
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
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_c_sends_sigint() {
    // Ctrl+C must send 0x03 (SIGINT), not the literal letter "c".
    let a = decide_key(true, false, false, phys(KeyCode::KeyC), &Key::Character("c".into()), false, false);
    assert_eq!(a, KeyAction::Send(vec![3]));
}

#[test]
fn ctrl_letters_send_control_bytes() {
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyD), &Key::Character("d".into()), false, false),
        KeyAction::Send(vec![4]) // Ctrl+D = EOF
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyZ), &Key::Character("z".into()), false, false),
        KeyAction::Send(vec![26]) // Ctrl+Z = suspend
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyL), &Key::Character("l".into()), false, false),
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
        false,
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
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b]));
}

#[test]
fn ctrl_shift_t_opens_new_tab() {
    // Ctrl+Shift+T now opens a new tab (theme switching moved to Settings).
    let action = decide_key(
        true,
        true,
        false,
        phys(KeyCode::KeyT),
        &Key::Character("T".into()),
        false,
        false,
    );
    assert_eq!(action, KeyAction::NewTab);
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
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1d]));
}

// ---------------------------------------------------------------------------
// Ctrl+Alt+<letter> → ESC-prefixed control byte (fix #1)
// ---------------------------------------------------------------------------

#[test]
fn ctrl_alt_b_sends_esc_prefixed_control_byte() {
    // Ctrl+Alt+b must send ESC + 0x02, NOT a bare 0x02. The ESC prefix is the
    // classic "Meta sends Escape" convention applied to the control byte.
    let action = decide_key(
        true,  // ctrl
        false, // shift
        true,  // alt
        phys(KeyCode::KeyB),
        &Key::Character("b".into()),
        false,
        false,
    );
    assert_eq!(action, KeyAction::Send(vec![0x1b, 0x02]));
}

// ---------------------------------------------------------------------------
// Ctrl+Shift+C / Ctrl+Shift+V → clipboard Copy / Paste
// ---------------------------------------------------------------------------

#[test]
fn ctrl_shift_c_sends_sigint() {
    // Ctrl+Shift+C is now the "Copy selection" shortcut, not a SIGINT.
    // (Previously it sent 0x03; the new clipboard feature takes priority.)
    let action = decide_key(
        true, // ctrl
        true, // shift
        false,
        phys(KeyCode::KeyC),
        &Key::Character("C".into()),
        false,
        false,
    );
    assert_eq!(action, KeyAction::Copy);
}

#[test]
fn ctrl_shift_v_pastes() {
    // Ctrl+Shift+V is the "Paste" shortcut.
    let action = decide_key(
        true, // ctrl
        true, // shift
        false,
        phys(KeyCode::KeyV),
        &Key::Character("V".into()),
        false,
        false,
    );
    assert_eq!(action, KeyAction::Paste);
}

#[test]
fn ctrl_shift_o_still_toggles_panel() {
    // The explicit Ctrl+Shift+O shortcut must still be intercepted before the
    // ctrl-byte rule, so it toggles the panel rather than sending 0x0f.
    let action = decide_key(
        true, // ctrl
        true, // shift
        false,
        phys(KeyCode::KeyO),
        &Key::Character("O".into()),
        false,
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

// ---------------------------------------------------------------------------
// Arrow keys honor DECCKM application cursor mode (fix #3)
// ---------------------------------------------------------------------------

#[test]
fn arrow_up_normal_mode_sends_csi() {
    // app_cursor = false → CSI: ESC [ A.
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::ArrowUp),
        &Key::Named(NamedKey::ArrowUp),
        false,
        false, // app_cursor off
    );
    assert_eq!(action, KeyAction::Send(b"\x1b[A".to_vec()));
}

#[test]
fn arrow_up_app_cursor_mode_sends_ss3() {
    // app_cursor = true → SS3: ESC O A.
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::ArrowUp),
        &Key::Named(NamedKey::ArrowUp),
        false,
        true, // app_cursor on
    );
    assert_eq!(action, KeyAction::Send(b"\x1bOA".to_vec()));
}

#[test]
fn arrow_keys_app_cursor_mode_all_directions() {
    // Sanity-check all four arrows under DECCKM.
    let cases = [
        (NamedKey::ArrowUp, KeyCode::ArrowUp, b"\x1bOA"),
        (NamedKey::ArrowDown, KeyCode::ArrowDown, b"\x1bOB"),
        (NamedKey::ArrowRight, KeyCode::ArrowRight, b"\x1bOC"),
        (NamedKey::ArrowLeft, KeyCode::ArrowLeft, b"\x1bOD"),
    ];
    for (named, code, expected) in cases {
        let action = decide_key(
            false,
            false,
            false,
            phys(code),
            &Key::Named(named),
            false,
            true,
        );
        assert_eq!(action, KeyAction::Send(expected.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// decide_mouse_press tests
// ---------------------------------------------------------------------------

/// Build a real PanelGeom for a 1000×640 window at 70% opacity, theme index 1.
fn make_panel_geom() -> jetty_render::PanelGeom {
    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    jetty_render::build_panel(1000, 640, 0.7, 1, 16.0, &[], "", 0, 10.0, 0.0, 0.0, &theme).geom
}

/// Build a scrollbar rect that is non-None (requires scroll_max > 0).
fn make_scrollbar_rect() -> jetty_render::Rect {
    // 30 rows visible, 10 lines of history, scroll_offset=5, 1000×640.
    jetty_render::scrollbar_rect_geom(30, 5, 10, 1000, 640, 0.0, [150, 150, 165, 220])
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
    // Click somewhere in the panel background below the title bar (y+40) and
    // below the opacity slider (y+96), but not over any widget.
    // This should consume the click without triggering a drag or action.
    let cx = geom.panel.x + 5.0;
    let cy = geom.panel.y + 100.0; // below title bar (36px) and slider (96px)
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::ConsumePanel);
}

#[test]
fn click_title_bar_starts_dialog_drag() {
    let geom = make_panel_geom();
    // Click within the title bar strip (top 36px) — not on a widget.
    let cx = geom.panel.x + 10.0;
    let cy = geom.panel.y + 10.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartDialogDrag);
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
