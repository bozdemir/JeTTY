//! Headless unit tests for keyboard and mouse input decision logic.
//! No window, no GPU, no display required.

use jetty_app::input::{decide_key, decide_mouse_press, KeyAction, MouseAction};
use jetty_app::keymap::KeyMap;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

// ---------------------------------------------------------------------------
// Helper: wrap a KeyCode in PhysicalKey::Code
// ---------------------------------------------------------------------------
fn phys(code: KeyCode) -> PhysicalKey {
    PhysicalKey::Code(code)
}

/// Oracle harness: `decide_key` through the DEFAULT keymap with `super_ = false`.
/// The pre-refactor expectations below are thus a byte-identical regression oracle
/// for the default bindings — a user with no `[keys]` config gets today's behavior.
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
    let km = KeyMap::defaults();
    decide_key(&km, ctrl, shift, alt, false, physical, logical, panel_open, app_cursor, alt_screen)
}

// ---------------------------------------------------------------------------
// decide_key tests
// ---------------------------------------------------------------------------

#[test]
fn ctrl_comma_physical_toggles_panel_closed() {
    // THE Ctrl+, fix: physical Comma, no shift, panel closed.
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let a = dk(true, false, false, phys(KeyCode::KeyC), &Key::Character("c".into()), false, false, false);
    assert_eq!(a, KeyAction::Send(vec![3]));
}

#[test]
fn ctrl_letters_send_control_bytes() {
    assert_eq!(
        dk(true, false, false, phys(KeyCode::KeyD), &Key::Character("d".into()), false, false, false),
        KeyAction::Send(vec![4]) // Ctrl+D = EOF
    );
    assert_eq!(
        dk(true, false, false, phys(KeyCode::KeyZ), &Key::Character("z".into()), false, false, false),
        KeyAction::Send(vec![26]) // Ctrl+Z = suspend
    );
    assert_eq!(
        dk(true, false, false, phys(KeyCode::KeyL), &Key::Character("l".into()), false, false, false),
        KeyAction::Send(vec![12]) // Ctrl+L = clear
    );
}

#[test]
fn escape_closes_open_panel() {
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
fn ctrl_alt_plus_increases_opacity() {
    // v0.26: transparency is Ctrl+Alt+'±' (Ctrl+Shift+'+' is font zoom now).
    let action = dk(
        true,
        false,
        true,
        phys(KeyCode::Equal),
        &Key::Character("=".into()),
        false,
        false,
        false,
    );
    assert_eq!(action, KeyAction::OpacityUp);
    let shifted = dk(true, true, true, phys(KeyCode::Equal), &Key::Character("+".into()), false, false, false);
    assert_eq!(shifted, KeyAction::OpacityUp);
    let zoom = dk(true, true, false, phys(KeyCode::Equal), &Key::Character("+".into()), false, false, false);
    assert_eq!(zoom, KeyAction::FontUp);
}

#[test]
fn ctrl_alt_minus_decreases_opacity() {
    let action = dk(
        true,
        false,
        true,
        phys(KeyCode::Minus),
        &Key::Character("-".into()),
        false,
        false,
        false,
    );
    assert_eq!(action, KeyAction::OpacityDown);
    // Ctrl+Shift+Minus types '_' → 0x1f (readline undo) again.
    let undo = dk(true, true, false, phys(KeyCode::Minus), &Key::Character("_".into()), false, false, false);
    assert_eq!(undo, KeyAction::Send(vec![0x1f]));
}

#[test]
fn shift_page_up_scrolls_up() {
    let action = dk(
        false,
        true,
        false,
        phys(KeyCode::PageUp),
        &Key::Named(NamedKey::PageUp),
        false,
        false,
        false,
    );
    assert_eq!(action, KeyAction::ScrollPageUp);
    // v0.26: plain PageUp reaches the program (fzf, zsh history paging).
    let plain = dk(false, false, false, phys(KeyCode::PageUp), &Key::Named(NamedKey::PageUp), false, false, false);
    assert_eq!(plain, KeyAction::Send(b"\x1b[5~".to_vec()));
}

#[test]
fn shift_page_down_scrolls_down() {
    let action = dk(
        false,
        true,
        false,
        phys(KeyCode::PageDown),
        &Key::Named(NamedKey::PageDown),
        false,
        false,
        false,
    );
    assert_eq!(action, KeyAction::ScrollPageDown);
    let plain = dk(false, false, false, phys(KeyCode::PageDown), &Key::Named(NamedKey::PageDown), false, false, false);
    assert_eq!(plain, KeyAction::Send(b"\x1b[6~".to_vec()));
}

#[test]
fn plain_s_sends_byte() {
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
    let action = dk(
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
        let action = dk(
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

/// A real Settings panel for a 1000×640 window, built from the real control
/// table (`settings_ui`) by the real builder: 70% opacity, Dropdown mode (so
/// the dropdown sliders are live), `active_tab` scrolled `scroll` px.
fn make_panel_geom_cfg(cfg: &jetty_app::config::Config, active_tab: usize, scroll: f32) -> jetty_render::PanelGeom {
    use jetty_app::settings_ui::{tab_items, Ctx};
    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    let ui = ["System Sans (default)".to_string()];
    let ctx = Ctx { ui_families: &ui, ..Ctx::empty() };
    let items = tab_items(active_tab, cfg, &ctx);
    let mut inp = jetty_render::PanelInput::new(1000, 640, &theme, jetty_render::ChromeMetrics::DEFAULT, &items);
    inp.active_tab = active_tab;
    inp.scroll = scroll;
    inp.theme_idx = 1;
    jetty_render::build_panel(&inp, &mut jetty_render::MonoMeasure(9.8)).geom
}

fn test_cfg() -> jetty_app::config::Config {
    jetty_app::config::Config { opacity: 0.7, window_mode: "dropdown".into(), ..Default::default() }
}

fn make_panel_geom_tab_scroll(active_tab: usize, scroll: f32) -> jetty_render::PanelGeom {
    make_panel_geom_cfg(&test_cfg(), active_tab, scroll)
}

fn make_panel_geom_tab(active_tab: usize) -> jetty_render::PanelGeom {
    make_panel_geom_tab_scroll(active_tab, 0.0)
}

/// Tab-0 ("Look") panel geometry: opacity / radius sliders, the theme gallery.
fn make_panel_geom() -> jetty_render::PanelGeom {
    make_panel_geom_tab(0)
}

/// The rect of part `part` of control `id`.
fn ctl_rect(g: &jetty_render::PanelGeom, id: &'static str, part: jetty_render::CtlPart) -> jetty_render::Rect {
    g.rect_of(jetty_render::PanelHit::Ctl { id, part }).unwrap_or_else(|| panic!("no {id} {part:?}"))
}

/// Click the center of `r` and return the action.
fn click(g: &jetty_render::PanelGeom, r: &jetty_render::Rect) -> MouseAction {
    decide_mouse_press(Some(g), None, r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// Build a scrollbar rect that is non-None (requires scroll_max > 0).
fn make_scrollbar_rect() -> jetty_render::Rect {
    // 30 rows visible, 10 lines of history, scroll_offset=5, 1000×640 (the
    // grid band is the whole window: no bars), 1×.
    let track = jetty_render::ScrollbarTrack::new(1000.0, 0.0, 640.0, 1.0);
    jetty_render::scrollbar_rect_geom(30, 5, 10, &track, [150, 150, 165, 220])
        .expect("scrollbar should be Some when scroll_max > 0")
}

#[test]
fn click_slider_track_starts_drag() {
    use jetty_render::CtlPart;
    let geom = make_panel_geom();
    let t = ctl_rect(&geom, "opacity", CtlPart::Track);
    assert_eq!(click(&geom, &t), MouseAction::Ctl { id: "opacity", part: CtlPart::Track });
    // Anywhere along the track (its ends included) grabs the knob.
    let left = decide_mouse_press(Some(&geom), None, t.x + 1.0, t.y + t.h / 2.0);
    assert_eq!(left, MouseAction::Ctl { id: "opacity", part: CtlPart::Track });
    let r = ctl_rect(&geom, "corner_radius", CtlPart::Track);
    assert_eq!(click(&geom, &r), MouseAction::Ctl { id: "corner_radius", part: CtlPart::Track });
}

#[test]
fn click_dropdown_size_tracks_start_drags() {
    use jetty_render::CtlPart;
    let geom = make_panel_geom_tab(2); // Window tab
    for id in ["dropdown_height_pct", "dropdown_width_pct"] {
        let t = ctl_rect(&geom, id, CtlPart::Track);
        assert_eq!(click(&geom, &t), MouseAction::Ctl { id, part: CtlPart::Track });
    }
}

#[test]
fn dropdown_size_sliders_are_inert_outside_dropdown_mode() {
    let mut cfg = test_cfg();
    cfg.window_mode = "center".into();
    let geom = make_panel_geom_cfg(&cfg, 2, 0.0);
    assert!(
        !geom.hits.iter().any(|(_, h)| matches!(
            h,
            jetty_render::PanelHit::Ctl { id: "dropdown_height_pct" | "dropdown_width_pct", .. }
        )),
        "the dropdown sliders draw dimmed but take no clicks in Center mode"
    );
}

#[test]
fn click_gallery_card_and_filter_chip() {
    let geom = make_panel_geom();
    let card = geom
        .hits
        .iter()
        .find_map(|(r, h)| match h {
            jetty_render::PanelHit::GalleryCard(i) if r.y + r.h < geom.content_bottom => Some((*r, *i)),
            _ => None,
        })
        .expect("a fully visible theme card");
    assert_eq!(click(&geom, &card.0), MouseAction::GalleryCard(card.1));
    let chip = geom
        .rect_of(jetty_render::PanelHit::GalleryFilter(jetty_render::ThemeFilter::Light))
        .expect("the Light chip");
    assert_eq!(click(&geom, &chip), MouseAction::GalleryFilter(jetty_render::ThemeFilter::Light));
}

#[test]
fn click_section_header_toggles_it() {
    let geom = make_panel_geom();
    let hdr = geom.rect_of(jetty_render::PanelHit::Section("look.window")).expect("first section header");
    assert_eq!(
        decide_mouse_press(Some(&geom), None, hdr.x + 20.0, hdr.y + hdr.h / 2.0),
        MouseAction::SettingsSection("look.window")
    );
}

#[test]
fn click_inside_panel_not_widget_consumes() {
    let geom = make_panel_geom();
    // Left of the content column, below the tab strip: panel, but no widget.
    let cx = geom.panel.x + 5.0;
    let cy = geom.panel.y + 100.0;
    let action = decide_mouse_press(Some(&geom), None, cx, cy);
    assert_eq!(action, MouseAction::ConsumePanel);
}

#[test]
fn click_title_bar_starts_dialog_drag() {
    let geom = make_panel_geom();
    // Click within the title row — not on a widget.
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
    // At (0,0) — outside the centered content column, above every widget.
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
        let a = dk(false, false, false, phys(KeyCode::Home), &named(*k), false, false, false);
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
        let a = dk(false, false, false, phys(KeyCode::F1), &named(*k), false, false, false);
        assert_eq!(a, send(want), "key {:?}", k);
    }
}

#[test]
fn modified_arrows_use_csi_1_mod_form() {
    // Ctrl+Left = mod 5, Shift+Right = mod 2, Alt+Up = mod 3, Ctrl+Shift+Down = mod 6.
    let ctrl_left = dk(true, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, false, false);
    assert_eq!(ctrl_left, send(b"\x1b[1;5D"));
    let shift_right = dk(false, true, false, phys(KeyCode::ArrowRight), &named(NamedKey::ArrowRight), false, false, false);
    assert_eq!(shift_right, send(b"\x1b[1;2C"));
    let alt_up = dk(false, false, true, phys(KeyCode::ArrowUp), &named(NamedKey::ArrowUp), false, false, false);
    assert_eq!(alt_up, send(b"\x1b[1;3A"));
    let ctrl_shift_down = dk(true, true, false, phys(KeyCode::ArrowDown), &named(NamedKey::ArrowDown), false, false, false);
    assert_eq!(ctrl_shift_down, send(b"\x1b[1;6B"));
}

#[test]
fn plain_arrows_unchanged_in_both_decckm_modes() {
    // No modifier → DECCKM-aware bare arrows (regression guard for the modified branch).
    let normal = dk(false, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, false, false);
    assert_eq!(normal, send(b"\x1b[D"));
    let app = dk(false, false, false, phys(KeyCode::ArrowLeft), &named(NamedKey::ArrowLeft), false, true, false);
    assert_eq!(app, send(b"\x1bOD"));
}

#[test]
fn shift_tab_sends_back_tab() {
    let a = dk(false, true, false, phys(KeyCode::Tab), &named(NamedKey::Tab), false, false, false);
    assert_eq!(a, send(b"\x1b[Z"));
    // Plain Tab still sends a literal TAB.
    let plain = dk(false, false, false, phys(KeyCode::Tab), &named(NamedKey::Tab), false, false, false);
    assert_eq!(plain, send(b"\t"));
}

// ---------------------------------------------------------------------------
// Effects tab (tab index 4) hit-tests
// ---------------------------------------------------------------------------

/// The Effects tab scrolled to the top.
fn effects_panel_geom() -> jetty_render::PanelGeom {
    make_panel_geom_tab(4)
}

/// The Effects tab scrolled to the end (the builder clamps the offset).
fn effects_panel_geom_scrolled() -> jetty_render::PanelGeom {
    make_panel_geom_tab_scroll(4, 1.0e9)
}

/// Click control part `(id, part)` on whichever Effects view shows it fully.
fn click_fx(id: &'static str, part: jetty_render::CtlPart) -> MouseAction {
    for g in [effects_panel_geom(), effects_panel_geom_scrolled()] {
        let r = ctl_rect(&g, id, part);
        if r.y >= g.content_top && r.y + r.h <= g.content_bottom {
            return click(&g, &r);
        }
    }
    panic!("{id} {part:?} is never fully in view");
}

#[test]
fn effects_crt_master_switch_lives_in_its_section_header() {
    use jetty_render::CtlPart;
    let want = MouseAction::Ctl { id: "effects.crt_enabled", part: CtlPart::Switch };
    assert_eq!(click_fx("effects.crt_enabled", CtlPart::Switch), want);
    // It sits on the "CRT" header row.
    let g = effects_panel_geom();
    let sw = ctl_rect(&g, "effects.crt_enabled", CtlPart::Switch);
    let hdr = g.rect_of(jetty_render::PanelHit::Section("fx.crt")).unwrap();
    assert!(sw.y >= hdr.y && sw.y + sw.h <= hdr.y + hdr.h + 0.5, "switch on the header row");
}

#[test]
fn effects_sliders_start_drags() {
    use jetty_render::CtlPart;
    for id in [
        "effects.crt_curvature",
        "effects.crt_scanline",
        "effects.crt_mask",
        "effects.crt_bloom",
        "effects.crt_chromatic",
        "effects.crt_vignette",
        "effects.caret_flash_ms",
    ] {
        assert_eq!(click_fx(id, CtlPart::Track), MouseAction::Ctl { id, part: CtlPart::Track });
    }
}

#[test]
fn effects_rgb_channels_start_drags() {
    use jetty_render::CtlPart;
    for id in ["effects.crt_scanline_tint", "effects.caret_flash_color"] {
        for ch in 0..3u8 {
            let part = CtlPart::Channel(ch);
            assert_eq!(click_fx(id, part), MouseAction::Ctl { id, part });
        }
    }
}

#[test]
fn effects_toggles_and_animation_chips() {
    use jetty_render::CtlPart;
    for i in 0..3u8 {
        let part = CtlPart::Chip(i);
        assert_eq!(click_fx("effects.crt_animate", part), MouseAction::Ctl { id: "effects.crt_animate", part });
    }
    for id in ["effects.caret_flash_enabled", "effects.caret_glow_enabled"] {
        assert_eq!(click_fx(id, CtlPart::Switch), MouseAction::Ctl { id, part: CtlPart::Switch });
    }
}

/// Scroll-aware hit-test: a control below the viewport at scroll 0 takes no
/// click there, and does once scrolled into view.
#[test]
fn effects_scroll_aware_hit_test() {
    use jetty_render::CtlPart;
    let id = "effects.caret_flash_color";
    let want = MouseAction::Ctl { id, part: CtlPart::Channel(1) };
    let top = effects_panel_geom();
    let r = ctl_rect(&top, id, CtlPart::Channel(1));
    assert!(r.y >= top.content_bottom, "below the fold at scroll 0");
    assert_ne!(click(&top, &r), want, "not hittable while scrolled out");
    let end = effects_panel_geom_scrolled();
    assert!(end.scroll > 0.0 && end.scroll == end.max_scroll);
    assert_eq!(click(&end, &ctl_rect(&end, id, CtlPart::Channel(1))), want);
}

/// Effects controls exist only on the Effects tab.
#[test]
fn effects_widgets_inactive_on_look_tab() {
    let g = make_panel_geom_tab(0);
    assert!(!g.hits.iter().any(|(_, h)| matches!(h, jetty_render::PanelHit::Ctl { id, .. } if id.starts_with("effects."))));
    let action = decide_mouse_press(Some(&g), None, 400.0, 400.0);
    assert!(!matches!(action, MouseAction::Ctl { id, .. } if id.starts_with("effects.")));
}

// ---------------------------------------------------------------------------
// Keyboard/VT encoding fixes (input group)
// ---------------------------------------------------------------------------

#[test]
fn ctrl_letter_keyed_on_logical_char_not_physical() {
    // Dvorak: the key that TYPES 'c' sits at physical KeyI. Ctrl+that must send
    // 0x03 (SIGINT), keyed on the LOGICAL char, not the physical QWERTY position
    // (which would wrongly send 0x09 TAB).
    let a = dk(true, false, false, phys(KeyCode::KeyI), &Key::Character("c".into()), false, false, false);
    assert_eq!(a, send(&[3]));
    // Unidentified physical + logical letter still yields the control byte.
    let b = dk(
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
    let caret = dk(true, true, false, phys(KeyCode::Digit6), &Key::Character("^".into()), false, false, false);
    assert_eq!(caret, send(&[0x1e]));
    // Ctrl+Shift+2 (logical "@") → 0x00 NUL.
    let at = dk(true, true, false, phys(KeyCode::Digit2), &Key::Character("@".into()), false, false, false);
    assert_eq!(at, send(&[0x00]));
    // Ctrl+Shift+/ (logical "?") → 0x7f DEL.
    let q = dk(true, true, false, phys(KeyCode::Slash), &Key::Character("?".into()), false, false, false);
    assert_eq!(q, send(&[0x7f]));
}

#[test]
fn modified_page_keys_send_xterm_tilde_form() {
    // Ctrl+PageDown → `\e[6;5~` (vim :tabnext, tmux C-PgDn) — even on the primary
    // screen (no longer hijacked into host scroll).
    let ctrl_pgdn = dk(true, false, false, phys(KeyCode::PageDown), &named(NamedKey::PageDown), false, false, false);
    assert_eq!(ctrl_pgdn, send(b"\x1b[6;5~"));
    // Alt+PageUp → `\e[5;3~`.
    let alt_pgup = dk(false, false, true, phys(KeyCode::PageUp), &named(NamedKey::PageUp), false, false, false);
    assert_eq!(alt_pgup, send(b"\x1b[5;3~"));
    // On the alt screen too: Ctrl+PageUp → `\e[5;5~`, not the plain `\e[5~`.
    let ctrl_pgup_alt = dk(true, false, false, phys(KeyCode::PageUp), &named(NamedKey::PageUp), false, false, true);
    assert_eq!(ctrl_pgup_alt, send(b"\x1b[5;5~"));
}

#[test]
fn shift_insert_pastes_ctrl_alt_insert_keep_tilde() {
    // Shift+Insert is the universal terminal paste chord — handled by the host.
    let a = dk(false, true, false, phys(KeyCode::Insert), &named(NamedKey::Insert), false, false, false);
    assert_eq!(a, KeyAction::Paste);
    // Ctrl+Insert keeps the modified tilde form (mod = 5).
    let ctrl_ins = dk(true, false, false, phys(KeyCode::Insert), &named(NamedKey::Insert), false, false, false);
    assert_eq!(ctrl_ins, send(b"\x1b[2;5~"));
}

#[test]
fn ctrl_backspace_sends_bs_not_del() {
    let ctrl_bs = dk(true, false, false, phys(KeyCode::Backspace), &named(NamedKey::Backspace), false, false, false);
    assert_eq!(ctrl_bs, send(&[0x08]));
    // Plain Backspace stays 0x7f (unchanged).
    let plain = dk(false, false, false, phys(KeyCode::Backspace), &named(NamedKey::Backspace), false, false, false);
    assert_eq!(plain, send(&[0x7f]));
}

#[test]
fn modified_function_keys_carry_modifier() {
    // Shift+F5 → `\e[15;2~`, Ctrl+F1 → `\e[1;5P`, Alt+F4 → `\e[1;3S`.
    let shift_f5 = dk(false, true, false, phys(KeyCode::F5), &named(NamedKey::F5), false, false, false);
    assert_eq!(shift_f5, send(b"\x1b[15;2~"));
    let ctrl_f1 = dk(true, false, false, phys(KeyCode::F1), &named(NamedKey::F1), false, false, false);
    assert_eq!(ctrl_f1, send(b"\x1b[1;5P"));
    let alt_f4 = dk(false, false, true, phys(KeyCode::F4), &named(NamedKey::F4), false, false, false);
    assert_eq!(alt_f4, send(b"\x1b[1;3S"));
    // Plain (unmodified) F-keys keep their original sequences.
    let plain_f5 = dk(false, false, false, phys(KeyCode::F5), &named(NamedKey::F5), false, false, false);
    assert_eq!(plain_f5, send(b"\x1b[15~"));
}
