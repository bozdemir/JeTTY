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
        false,
    );
    assert_eq!(action, KeyAction::TogglePanel);
}

#[test]
fn ctrl_c_sends_sigint() {
    // Ctrl+C must send 0x03 (SIGINT), not the literal letter "c".
    let a = decide_key(true, false, false, phys(KeyCode::KeyC), &Key::Character("c".into()), false, false, false);
    assert_eq!(a, KeyAction::Send(vec![3]));
}

#[test]
fn ctrl_letters_send_control_bytes() {
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyD), &Key::Character("d".into()), false, false, false),
        KeyAction::Send(vec![4]) // Ctrl+D = EOF
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyZ), &Key::Character("z".into()), false, false, false),
        KeyAction::Send(vec![26]) // Ctrl+Z = suspend
    );
    assert_eq!(
        decide_key(true, false, false, phys(KeyCode::KeyL), &Key::Character("l".into()), false, false, false),
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
        false,
    );
    assert_eq!(action, KeyAction::Send(b"\r".to_vec()));
}

#[test]
fn unknown_key_returns_none() {
    // F13 has no xterm mapping (we encode F1–F12); a genuinely unmapped key
    // must still produce no bytes. (F12 is now mapped — see function_keys test.)
    let action = decide_key(
        false,
        false,
        false,
        phys(KeyCode::F13),
        &Key::Named(NamedKey::F13),
        false,
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
        false,
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
        false,
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
            false,
        );
        assert_eq!(action, KeyAction::Send(expected.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// decide_mouse_press tests
// ---------------------------------------------------------------------------

/// Build a real PanelGeom for a 1000×640 window at 70% opacity, theme index 1.
fn make_panel_geom_tab(active_tab: usize) -> jetty_render::PanelGeom {
    make_panel_geom_tab_scroll(active_tab, 0.0)
}

/// Build a PanelGeom for `active_tab` with a specific `effects_scroll` offset.
fn make_panel_geom_tab_scroll(active_tab: usize, effects_scroll: f32) -> jetty_render::PanelGeom {
    make_panel_geom_full(active_tab, effects_scroll, false, 0)
}

/// Build a PanelGeom with explicit theme-dropdown state (open + scroll offset).
fn make_panel_geom_full(
    active_tab: usize,
    effects_scroll: f32,
    theme_open: bool,
    theme_scroll: usize,
) -> jetty_render::PanelGeom {
    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    // UI-font args: size 16, a single synthetic "System Sans" row, selected "".
    let ui_families = ["System Sans (default)".to_string()];
    jetty_render::build_panel(
        1000, 640, 0.7, 1, 16.0, &[], "", 0, 10.0, "Bayer",
        "Center", "Top", "10k", 0.50, 1.0, false, true,
        false, // launch_at_login
        16.0, &ui_families, "", 0,
        0.0, 0.0, &theme, 9.8,
        "System default", // shell_display
        &jetty_render::NotifyParams::default(), // Run & Notify (v0.15)
        active_tab,
        &jetty_render::EffectsParams::default(),
        effects_scroll,
        theme_open,
        theme_scroll,
    )
    .geom
}

/// Tab-0 ("Look") panel geometry: opacity slider, theme combo, etc.
fn make_panel_geom() -> jetty_render::PanelGeom {
    make_panel_geom_tab(0)
}

/// Build a scrollbar rect that is non-None (requires scroll_max > 0).
fn make_scrollbar_rect() -> jetty_render::Rect {
    // 30 rows visible, 10 lines of history, scroll_offset=5, 1000×640.
    jetty_render::scrollbar_rect_geom(30, 5, 10, 1000, 640, 0.0, 0.0, [150, 150, 165, 220])
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
fn click_dropdown_height_track_starts_drag() {
    let geom = make_panel_geom_tab(2); // Window tab
    let t = &geom.dropdown_track;
    let cx = t.x + t.w / 2.0;
    let cy = t.y + t.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartDropdownDrag);
}

#[test]
fn click_dropdown_width_track_starts_width_drag() {
    let geom = make_panel_geom_tab(2); // Window tab
    let t = &geom.dropdown_width_track;
    let cx = t.x + t.w / 2.0;
    let cy = t.y + t.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartDropdownWidthDrag);
}

#[test]
fn click_dropdown_width_handle_starts_width_drag() {
    let geom = make_panel_geom_tab(2); // Window tab
    let h = &geom.dropdown_width_handle;
    let cx = h.x + h.w / 2.0;
    let cy = h.y + h.h / 2.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::StartDropdownWidthDrag);
}

#[test]
fn click_theme_combo_toggles_dropdown() {
    // Closed combo: clicking the header asks to open the dropdown.
    let geom = make_panel_geom();
    let c = &geom.theme_combo;
    let action = decide_mouse_press(Some(&geom), None, c.x + c.w / 2.0, c.y + c.h / 2.0);
    assert_eq!(action, MouseAction::ToggleThemeDropdown);
}

#[test]
fn click_open_dropdown_row_sets_theme_with_offset() {
    // Open at scroll offset 2: clicking visible row 0 selects preset index 2.
    let geom = make_panel_geom_full(0, 0.0, true, 2);
    assert!(geom.theme_open);
    let row = &geom.theme_rows[0];
    let action = decide_mouse_press(Some(&geom), None, row.x + row.w / 2.0, row.y + row.h / 2.0);
    assert_eq!(action, MouseAction::SetTheme(2));
    // Row 3 maps to preset 2 + 3 = 5.
    let row3 = &geom.theme_rows[3];
    let action3 = decide_mouse_press(Some(&geom), None, row3.x + row3.w / 2.0, row3.y + row3.h / 2.0);
    assert_eq!(action3, MouseAction::SetTheme(5));
}

#[test]
fn click_theme_scroll_arrows_page_list() {
    let geom = make_panel_geom_full(0, 0.0, true, 0);
    let up = &geom.theme_scroll_up;
    let dn = &geom.theme_scroll_down;
    // Only meaningful when the preset list overflows MAX_THEME_ROWS.
    if jetty_core::theme::PRESETS.len() > geom.theme_rows.len() {
        assert_eq!(
            decide_mouse_press(Some(&geom), None, dn.x + dn.w / 2.0, dn.y + dn.h / 2.0),
            MouseAction::ThemeScrollDown
        );
        assert_eq!(
            decide_mouse_press(Some(&geom), None, up.x + up.w / 2.0, up.y + up.h / 2.0),
            MouseAction::ThemeScrollUp
        );
    }
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

// ---------------------------------------------------------------------------
// Navigation / editing / function keys + modified arrows (campaign fixes)
// ---------------------------------------------------------------------------

fn named(k: NamedKey) -> Key {
    Key::Named(k)
}
fn send(bytes: &[u8]) -> KeyAction {
    KeyAction::Send(bytes.to_vec())
}

#[test]
fn nav_editing_keys_send_xterm_sequences() {
    let cases: &[(NamedKey, &[u8])] = &[
        (NamedKey::Home, b"\x1b[H"),
        (NamedKey::End, b"\x1b[F"),
        (NamedKey::Delete, b"\x1b[3~"),
        (NamedKey::Insert, b"\x1b[2~"),
    ];
    for (k, want) in cases {
        let a = decide_key(false, false, false, phys(KeyCode::Home), &named(*k), false, false, false);
        assert_eq!(a, send(want), "key {:?}", k);
    }
}

#[test]
fn function_keys_send_xterm_sequences() {
    let cases: &[(NamedKey, &[u8])] = &[
        (NamedKey::F1, b"\x1bOP"),
        (NamedKey::F4, b"\x1bOS"),
        (NamedKey::F5, b"\x1b[15~"),
        (NamedKey::F12, b"\x1b[24~"),
    ];
    for (k, want) in cases {
        let a = decide_key(false, false, false, phys(KeyCode::F1), &named(*k), false, false, false);
        assert_eq!(a, send(want), "key {:?}", k);
    }
}

#[test]
fn modified_arrows_use_csi_1_mod_form() {
    // Ctrl+Left = mod 5, Shift+Right = mod 2, Alt+Up = mod 3, Ctrl+Shift+Down = mod 6.
    let ctrl_left = decide_key(true, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, false, false);
    assert_eq!(ctrl_left, send(b"\x1b[1;5D"));
    let shift_right = decide_key(false, true, false, phys(KeyCode::ArrowRight), &named(NamedKey::ArrowRight), false, false, false);
    assert_eq!(shift_right, send(b"\x1b[1;2C"));
    let alt_up = decide_key(false, false, true, phys(KeyCode::ArrowUp), &named(NamedKey::ArrowUp), false, false, false);
    assert_eq!(alt_up, send(b"\x1b[1;3A"));
    let ctrl_shift_down = decide_key(true, true, false, phys(KeyCode::ArrowDown), &named(NamedKey::ArrowDown), false, false, false);
    assert_eq!(ctrl_shift_down, send(b"\x1b[1;6B"));
}

#[test]
fn plain_arrows_unchanged_in_both_decckm_modes() {
    // No modifier → DECCKM-aware bare arrows (regression guard for the modified branch).
    let normal = decide_key(false, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, false, false);
    assert_eq!(normal, send(b"\x1b[D"));
    let app = decide_key(false, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, true, false);
    assert_eq!(app, send(b"\x1bOD"));
}

#[test]
fn shift_tab_sends_back_tab() {
    let a = decide_key(false, true, false, phys(KeyCode::Tab), &named(NamedKey::Tab), false, false, false);
    assert_eq!(a, send(b"\x1b[Z"));
    // Plain Tab still sends a literal TAB.
    let plain = decide_key(false, false, false, phys(KeyCode::Tab), &named(NamedKey::Tab), false, false, false);
    assert_eq!(plain, send(b"\t"));
}

// ---------------------------------------------------------------------------
// Effects tab (tab index 4) hit-tests
// ---------------------------------------------------------------------------

/// Build PanelGeom for the Effects tab (index 4) at scroll=0 (top).
/// The CRT section (bands 0–9) is fully within the [content_top, content_bottom]
/// viewport at this offset. The Caret section (bands 11–14) is below the
/// viewport and correctly rejected by the input guard until scrolled into view.
fn effects_panel_geom() -> jetty_render::PanelGeom {
    make_panel_geom_tab(4)
}

/// Build PanelGeom for the Effects tab scrolled to maximum offset.
/// This brings the Caret section (bands 11–14) fully into the content viewport.
fn effects_panel_geom_scrolled() -> jetty_render::PanelGeom {
    // max_scroll = EFFECTS_CONTENT_H - EFFECTS_VISIBLE_H (derive, don't hardcode).
    make_panel_geom_tab_scroll(
        4,
        (jetty_render::EFFECTS_CONTENT_H - jetty_render::EFFECTS_VISIBLE_H).max(0.0),
    )
}

/// Click the center of `rect` against the Effects panel and return the action.
fn click(g: &jetty_render::PanelGeom, r: &jetty_render::Rect) -> MouseAction {
    decide_mouse_press(Some(g), None, r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn effects_crt_enabled_toggle_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_enabled_toggle), MouseAction::ToggleCrt);
}

#[test]
fn effects_crt_curvature_track_hit_test() {
    let g = effects_panel_geom();
    // Click the slider TRACK center.
    assert_eq!(click(&g, &g.crt_curvature_track), MouseAction::StartCrtCurvatureDrag);
}

#[test]
fn effects_crt_curvature_handle_hit_test() {
    let g = effects_panel_geom();
    // Click the slider HANDLE — should also start a drag.
    assert_eq!(click(&g, &g.crt_curvature_handle), MouseAction::StartCrtCurvatureDrag);
}

#[test]
fn effects_scanline_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_scanline_track), MouseAction::StartScanlineDrag);
}

#[test]
fn effects_mask_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_mask_track), MouseAction::StartMaskDrag);
}

#[test]
fn effects_bloom_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_bloom_track), MouseAction::StartBloomDrag);
}

#[test]
fn effects_chromatic_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_chromatic_track), MouseAction::StartChromaticDrag);
}

#[test]
fn effects_vignette_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_vignette_track), MouseAction::StartVignetteDrag);
}

#[test]
fn effects_tint_rgb_drag_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_tint_r_track), MouseAction::StartTintRDrag);
    assert_eq!(click(&g, &g.crt_tint_g_track), MouseAction::StartTintGDrag);
    assert_eq!(click(&g, &g.crt_tint_b_track), MouseAction::StartTintBDrag);
}

#[test]
fn effects_animation_toggles_hit_test() {
    let g = effects_panel_geom();
    assert_eq!(click(&g, &g.crt_roll_toggle),    MouseAction::ToggleCrtRoll);
    assert_eq!(click(&g, &g.crt_flicker_toggle), MouseAction::ToggleCrtFlicker);
    assert_eq!(click(&g, &g.crt_jitter_toggle),  MouseAction::ToggleCrtJitter);
}

#[test]
fn effects_caret_flash_toggle_hit_test() {
    // Caret section (band 11) is below the viewport at scroll=0.
    // Scroll to max to bring it into view.
    let g = effects_panel_geom_scrolled();
    assert_eq!(click(&g, &g.caret_flash_toggle), MouseAction::ToggleCaretFlash);
}

#[test]
fn effects_caret_glow_toggle_hit_test() {
    let g = effects_panel_geom_scrolled();
    assert_eq!(click(&g, &g.caret_glow_toggle), MouseAction::ToggleCaretGlow);
}

#[test]
fn effects_caret_dur_drag_hit_test() {
    let g = effects_panel_geom_scrolled();
    assert_eq!(click(&g, &g.caret_dur_track), MouseAction::StartCaretDurDrag);
}

#[test]
fn effects_caret_color_rgb_drag_hit_test() {
    let g = effects_panel_geom_scrolled();
    assert_eq!(click(&g, &g.caret_color_r_track), MouseAction::StartCaretColorRDrag);
    assert_eq!(click(&g, &g.caret_color_g_track), MouseAction::StartCaretColorGDrag);
    assert_eq!(click(&g, &g.caret_color_b_track), MouseAction::StartCaretColorBDrag);
}

/// Scroll-aware hit-test: a widget that is below the viewport at scroll=0 becomes
/// hittable once scrolled into view, and is correctly rejected when outside.
#[test]
fn effects_scroll_aware_hit_test() {
    // caret_flash_toggle is in band 11. At scroll=0 its draw-Y is outside
    // [content_top, content_bottom] for our 1000×640 test screen, so clicking
    // at the rect center must not fire the action.
    let g_top = effects_panel_geom(); // scroll=0
    let action_outside = click(&g_top, &g_top.caret_flash_toggle);
    assert_eq!(
        action_outside,
        MouseAction::None,
        "caret_flash_toggle should be invisible (and non-hittable) at scroll=0"
    );

    // At scroll=192 (max) the same widget is inside the viewport: must fire.
    let g_scrolled = effects_panel_geom_scrolled(); // scroll=192
    let action_inside = click(&g_scrolled, &g_scrolled.caret_flash_toggle);
    assert_eq!(
        action_inside,
        MouseAction::ToggleCaretFlash,
        "caret_flash_toggle should be hittable at max scroll"
    );
}

/// Regression: Effects widgets must NOT fire on tab 0 (Look) — their rects are
/// parked at 1e6 when not on the Effects tab, so any real cursor position misses.
#[test]
fn effects_widgets_inactive_on_look_tab() {
    let g = make_panel_geom_tab(0);
    // Spot-check: the crt_enabled_toggle rect is at 1e6 on tab 0, so a center
    // click at (1e6 + w/2, 1e6 + h/2) is outside any real screen — but we can
    // verify it doesn't decode as ToggleCrt for any realistic cursor position.
    let action = decide_mouse_press(Some(&g), None, 400.0, 400.0);
    // A click at (400,400) on tab 0 (Look) should NOT be a ToggleCrt.
    assert_ne!(action, MouseAction::ToggleCrt);
}

// ---------------------------------------------------------------------------
// Keyboard/VT encoding fixes (input group)
// ---------------------------------------------------------------------------

#[test]
fn ctrl_letter_keyed_on_logical_char_not_physical() {
    // Dvorak: the key that TYPES 'c' sits at physical KeyI. Ctrl+that must send
    // 0x03 (SIGINT), keyed on the LOGICAL char, not the physical QWERTY position
    // (which would wrongly send 0x09 TAB).
    let a = decide_key(true, false, false, phys(KeyCode::KeyI), &Key::Character("c".into()), false, false, false);
    assert_eq!(a, send(&[3]));
    // Unidentified physical + logical letter still yields the control byte.
    let b = decide_key(
        true, false, false,
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
        &Key::Character("z".into()),
        false, false, false,
    );
    assert_eq!(b, send(&[26]));
}

#[test]
fn ctrl_caret_at_question_send_c0_bytes() {
    // Ctrl+Shift+6 (logical "^") → 0x1e RS (vim Ctrl+^ alternate-file toggle).
    let caret = decide_key(true, true, false, phys(KeyCode::Digit6), &Key::Character("^".into()), false, false, false);
    assert_eq!(caret, send(&[0x1e]));
    // Ctrl+Shift+2 (logical "@") → 0x00 NUL.
    let at = decide_key(true, true, false, phys(KeyCode::Digit2), &Key::Character("@".into()), false, false, false);
    assert_eq!(at, send(&[0x00]));
    // Ctrl+Shift+/ (logical "?") → 0x7f DEL.
    let q = decide_key(true, true, false, phys(KeyCode::Slash), &Key::Character("?".into()), false, false, false);
    assert_eq!(q, send(&[0x7f]));
}

#[test]
fn modified_page_keys_send_xterm_tilde_form() {
    // Ctrl+PageDown → `\e[6;5~` (vim :tabnext, tmux C-PgDn) — even on the primary
    // screen (no longer hijacked into host scroll).
    let ctrl_pgdn = decide_key(true, false, false, phys(KeyCode::PageDown), &named(NamedKey::PageDown), false, false, false);
    assert_eq!(ctrl_pgdn, send(b"\x1b[6;5~"));
    // Alt+PageUp → `\e[5;3~`.
    let alt_pgup = decide_key(false, false, true, phys(KeyCode::PageUp), &named(NamedKey::PageUp), false, false, false);
    assert_eq!(alt_pgup, send(b"\x1b[5;3~"));
    // On the alt screen too: Ctrl+PageUp → `\e[5;5~`, not the plain `\e[5~`.
    let ctrl_pgup_alt = decide_key(true, false, false, phys(KeyCode::PageUp), &named(NamedKey::PageUp), false, false, true);
    assert_eq!(ctrl_pgup_alt, send(b"\x1b[5;5~"));
}

#[test]
fn shift_insert_pastes_ctrl_alt_insert_keep_tilde() {
    // Shift+Insert is the universal terminal paste chord — handled by the host.
    let a = decide_key(false, true, false, phys(KeyCode::Insert), &named(NamedKey::Insert), false, false, false);
    assert_eq!(a, KeyAction::Paste);
    // Ctrl+Insert keeps the modified tilde form (mod = 5).
    let ctrl_ins = decide_key(true, false, false, phys(KeyCode::Insert), &named(NamedKey::Insert), false, false, false);
    assert_eq!(ctrl_ins, send(b"\x1b[2;5~"));
}

#[test]
fn ctrl_backspace_sends_bs_not_del() {
    let ctrl_bs = decide_key(true, false, false, phys(KeyCode::Backspace), &named(NamedKey::Backspace), false, false, false);
    assert_eq!(ctrl_bs, send(&[0x08]));
    // Plain Backspace stays 0x7f (unchanged).
    let plain = decide_key(false, false, false, phys(KeyCode::Backspace), &named(NamedKey::Backspace), false, false, false);
    assert_eq!(plain, send(&[0x7f]));
}

#[test]
fn modified_function_keys_carry_modifier() {
    // Shift+F5 → `\e[15;2~`, Ctrl+F1 → `\e[1;5P`, Alt+F4 → `\e[1;3S`.
    let shift_f5 = decide_key(false, true, false, phys(KeyCode::F5), &named(NamedKey::F5), false, false, false);
    assert_eq!(shift_f5, send(b"\x1b[15;2~"));
    let ctrl_f1 = decide_key(true, false, false, phys(KeyCode::F1), &named(NamedKey::F1), false, false, false);
    assert_eq!(ctrl_f1, send(b"\x1b[1;5P"));
    let alt_f4 = decide_key(false, false, true, phys(KeyCode::F4), &named(NamedKey::F4), false, false, false);
    assert_eq!(alt_f4, send(b"\x1b[1;3S"));
    // Plain (unmodified) F-keys keep their original sequences.
    let plain_f5 = decide_key(false, false, false, phys(KeyCode::F5), &named(NamedKey::F5), false, false, false);
    assert_eq!(plain_f5, send(b"\x1b[15~"));
}
