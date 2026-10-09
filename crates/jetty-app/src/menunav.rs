//! Keyboard navigation for the custom-drawn menus — the terminal context menu,
//! the tab menu (and its "Color ▸" list) and a detached window's menu.
//!
//! The logic lives here — PURE and testable off `App`. An open menu is
//! keyboard-modal, like a native one: [`classify`] sorts every key press into
//! the menu's own keys ([`MenuKey`], Escape), a modifier alone, and any other
//! key — which closes the menu before it takes its usual path. [`step`] says
//! what a menu key does to an open menu, moving the highlight over the ENABLED
//! rows ([`next_enabled`], [`prev_enabled`], [`first_enabled`],
//! [`last_enabled`]: wrapping, grayed rows skipped). The app owns the menus: it
//! writes the highlight into the menu's hover index — the one the pointer
//! drives too, whenever it crosses rows ([`pointer_hover`]) — and runs a row
//! through the very method a click on that row runs.

use winit::keyboard::{Key, NamedKey};

use crate::keymap::Mods;

/// A key an open menu takes, decoded from the key press by [`classify`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKey {
    /// Up: the previous enabled row (wrapping); the last one from no highlight.
    Prev,
    /// Down: the next enabled row (wrapping); the first one from no highlight.
    Next,
    /// Home: the first enabled row.
    First,
    /// End: the last enabled row.
    Last,
    /// Enter, keypad Enter, Space: run the highlighted row.
    Activate,
    /// Right: open the highlighted row's submenu (the tab menu's "Color ▸").
    Open,
    /// Left: back from a submenu to the menu that opened it.
    Back,
}

/// What an open menu does with a [`MenuKey`] (see [`step`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuStep {
    /// Highlight this row — `None` when the menu has no enabled row.
    Highlight(Option<usize>),
    /// Run this (highlighted, enabled) row: exactly what a click on it does.
    Run(usize),
    /// Open this highlighted row's submenu — when the row has one.
    Open(usize),
    /// Go back to the parent menu — when this menu is a submenu.
    Back,
}

/// What a key PRESS means to an open menu (see [`classify`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuPress {
    /// One of the menu's keys: it drives the menu and goes no further.
    Key(MenuKey),
    /// Escape: it closes the menu and goes no further.
    Close,
    /// A modifier alone (Shift, Ctrl, Alt, AltGr, Super, a lock key): the menu
    /// stays open — it may start Shift+Down, or a chord — and the key takes
    /// its usual path.
    Modifier,
    /// Any other key, a chord included: it closes the menu, then takes its
    /// usual path — a letter reaches the shell, Ctrl+Tab switches tabs.
    Other,
}

/// Classify a key PRESS for an open menu. A menu is keyboard-modal, like a
/// native one: its keys (the arrows, Home / End, Enter / Space, Escape) are
/// its own, and every other key closes it on its way, so typing after an
/// accidental Menu press loses nothing and runs nothing — the next Space is
/// the shell's, not a press on the highlighted row. A key with Ctrl, Alt or
/// Super held is a chord (`Other`) — save Escape, which closes the menu
/// whatever is held; Shift changes nothing. Keypad Enter and the keypad's
/// arrows / Home / End (NumLock off) arrive as the same named keys as the
/// main block's.
pub fn classify(logical: &Key, mods: Mods) -> MenuPress {
    let Key::Named(named) = logical else { return MenuPress::Other };
    if crate::input::is_modifier_key(logical) {
        return MenuPress::Modifier;
    }
    if *named == NamedKey::Escape {
        return MenuPress::Close;
    }
    if mods.ctrl || mods.alt || mods.super_ {
        return MenuPress::Other;
    }
    MenuPress::Key(match named {
        NamedKey::ArrowUp => MenuKey::Prev,
        NamedKey::ArrowDown => MenuKey::Next,
        NamedKey::Home => MenuKey::First,
        NamedKey::End => MenuKey::Last,
        NamedKey::Enter | NamedKey::Space => MenuKey::Activate,
        NamedKey::ArrowRight => MenuKey::Open,
        NamedKey::ArrowLeft => MenuKey::Back,
        _ => return MenuPress::Other,
    })
}

/// The step `key` takes on an open menu of `rows` rows, `disabled` of them
/// grayed and `hover` highlighted. The moves always land (on an enabled row,
/// or nowhere when there is none); Enter / Space / Right need a highlighted
/// row and are `None` without one — the menu keeps the key and nothing
/// happens, as in a native menu. Whether Right / Left mean anything in THIS
/// menu (a row with a submenu, a submenu to back out of) is the caller's to
/// decide.
pub fn step(key: MenuKey, hover: Option<usize>, rows: usize, disabled: &[usize]) -> Option<MenuStep> {
    let hover = hover.filter(|&i| enabled(i, rows, disabled));
    Some(match key {
        MenuKey::Prev => MenuStep::Highlight(prev_enabled(hover, rows, disabled)),
        MenuKey::Next => MenuStep::Highlight(next_enabled(hover, rows, disabled)),
        MenuKey::First => MenuStep::Highlight(first_enabled(rows, disabled)),
        MenuKey::Last => MenuStep::Highlight(last_enabled(rows, disabled)),
        MenuKey::Activate => MenuStep::Run(hover?),
        MenuKey::Open => MenuStep::Open(hover?),
        MenuKey::Back => MenuStep::Back,
    })
}

/// The pointer's say in the highlight it shares with the keyboard: `row` is
/// the row under the pointer at this move (`None`: off the rows), `last` the
/// row it was over at its previous move (updated here). The pointer moves the
/// highlight only when it CROSSES onto another row or off the rows — then the
/// highlight follows it (a grayed row highlights nothing), exactly as with the
/// mouse alone; a nudge that stays off the rows (or on one row) leaves a
/// highlight the arrows made alone, so a palm on the touchpad cannot clear it
/// and turn the next Enter into a keystroke for the shell. `None`: no change.
pub fn pointer_hover(row: Option<usize>, last: &mut Option<usize>, disabled: &[usize]) -> Option<Option<usize>> {
    if row == *last {
        return None;
    }
    *last = row;
    Some(row.filter(|i| !disabled.contains(i)))
}

/// Whether row `i` of a `rows`-row menu exists and is not grayed.
fn enabled(i: usize, rows: usize, disabled: &[usize]) -> bool {
    i < rows && !disabled.contains(&i)
}

/// The first enabled row (`None`: an empty or all-grayed menu).
pub fn first_enabled(rows: usize, disabled: &[usize]) -> Option<usize> {
    (0..rows).find(|&i| enabled(i, rows, disabled))
}

/// The last enabled row (`None`: an empty or all-grayed menu).
pub fn last_enabled(rows: usize, disabled: &[usize]) -> Option<usize> {
    (0..rows).rev().find(|&i| enabled(i, rows, disabled))
}

/// The enabled row after `from`, wrapping past the last row to the first — or
/// the first enabled row when nothing is highlighted. `from` itself is the
/// answer when it is the only enabled row.
pub fn next_enabled(from: Option<usize>, rows: usize, disabled: &[usize]) -> Option<usize> {
    let Some(from) = from.filter(|&i| i < rows) else { return first_enabled(rows, disabled) };
    (1..=rows).map(|d| (from + d) % rows).find(|&i| enabled(i, rows, disabled))
}

/// The enabled row before `from`, wrapping past the first row to the last —
/// or the last enabled row when nothing is highlighted.
pub fn prev_enabled(from: Option<usize>, rows: usize, disabled: &[usize]) -> Option<usize> {
    let Some(from) = from.filter(|&i| i < rows) else { return last_enabled(rows, disabled) };
    (1..=rows).map(|d| (from + rows - d) % rows).find(|&i| enabled(i, rows, disabled))
}

/// Where a menu opened from the keyboard is anchored, in physical pixels: at
/// the text cursor's cell `(row, col)`, through the same grid origin and cell
/// size every cell-anchored overlay uses (the IME area, the hint chips). The
/// card (`menu_h` tall, border included) opens just below the line being
/// typed — or, where it would not fit there in a `win_h`-tall window (a prompt
/// on the bottom row, the usual case), just above it, the way a native popup
/// flips, so it never covers that line when it can help it. Whole pixels; the
/// menu builder still clamps the card into the window.
pub fn cursor_anchor(
    row: usize,
    col: usize,
    cell: (f32, f32),
    origin: jetty_render::GridOrigin,
    menu_h: f32,
    win_h: f32,
) -> (f32, f32) {
    let below = origin.row_y(row + 1, cell.1);
    let above = origin.row_y(row, cell.1) - menu_h;
    let y = if below + menu_h <= win_h || above < 0.0 { below } else { above };
    (origin.col_x(col, cell.0).round(), y.round())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, super_: false };

    fn named(k: NamedKey) -> Key {
        Key::Named(k)
    }

    #[test]
    fn arrows_home_end_enter_space_and_left_right_are_the_menus() {
        let cases = [
            (NamedKey::ArrowUp, MenuKey::Prev),
            (NamedKey::ArrowDown, MenuKey::Next),
            (NamedKey::Home, MenuKey::First),
            (NamedKey::End, MenuKey::Last),
            (NamedKey::Enter, MenuKey::Activate),
            (NamedKey::Space, MenuKey::Activate),
            (NamedKey::ArrowRight, MenuKey::Open),
            (NamedKey::ArrowLeft, MenuKey::Back),
        ];
        for (k, want) in cases {
            assert_eq!(classify(&named(k), NONE), MenuPress::Key(want), "{k:?}");
            // Shift changes nothing (Shift+Space, Shift+Down still navigate).
            let shift = Mods { shift: true, ..NONE };
            assert_eq!(classify(&named(k), shift), MenuPress::Key(want), "Shift+{k:?}");
        }
    }

    #[test]
    fn chords_and_every_other_key_close_the_menu_on_their_way() {
        // Ctrl / Alt / Super make it a chord: the menu closes and the chord
        // takes its usual path (a keymap action, or the shell).
        for m in [
            Mods { ctrl: true, ..NONE },
            Mods { alt: true, ..NONE },
            Mods { super_: true, ..NONE },
            Mods { ctrl: true, shift: true, ..NONE },
        ] {
            for k in [NamedKey::ArrowDown, NamedKey::Enter, NamedKey::Space, NamedKey::Home, NamedKey::Tab] {
                assert_eq!(classify(&named(k), m), MenuPress::Other, "{m:?}+{k:?}");
            }
        }
        // Letters, digits, Tab, Page keys, Backspace, F-keys and the Menu key
        // itself (its toggle closes the menu) are not the menu's either.
        for k in [
            named(NamedKey::Tab),
            named(NamedKey::PageDown),
            named(NamedKey::Backspace),
            named(NamedKey::Delete),
            named(NamedKey::F5),
            named(NamedKey::ContextMenu),
            Key::Character("j".into()),
            Key::Character("1".into()),
            Key::Character(" ".into()),
            Key::Character("@".into()),
        ] {
            assert_eq!(classify(&k, NONE), MenuPress::Other, "{k:?}");
        }
    }

    #[test]
    fn escape_closes_the_menu_whatever_is_held() {
        for m in [NONE, Mods { shift: true, ..NONE }, Mods { ctrl: true, ..NONE }, Mods { alt: true, ..NONE }] {
            assert_eq!(classify(&named(NamedKey::Escape), m), MenuPress::Close, "{m:?}");
        }
    }

    #[test]
    fn a_modifier_alone_leaves_the_menu_open() {
        // Pressing Ctrl on its way to Ctrl+Tab, or Shift to Shift+Down, must
        // not close the menu — the held modifier is already in `mods` then.
        let cases = [
            (NamedKey::Shift, Mods { shift: true, ..NONE }),
            (NamedKey::Control, Mods { ctrl: true, ..NONE }),
            (NamedKey::Alt, Mods { alt: true, ..NONE }),
            (NamedKey::AltGraph, NONE),
            (NamedKey::Super, Mods { super_: true, ..NONE }),
            (NamedKey::Meta, NONE),
            (NamedKey::CapsLock, NONE),
            (NamedKey::NumLock, NONE),
        ];
        for (k, m) in cases {
            assert_eq!(classify(&named(k), m), MenuPress::Modifier, "{k:?}");
            assert_eq!(classify(&named(k), NONE), MenuPress::Modifier, "{k:?} (state not yet updated)");
        }
    }

    #[test]
    fn typing_after_an_accidental_menu_press_runs_nothing() {
        // The Menu key opened the terminal menu on its first enabled row:
        // Paste (Copy and Run in New Tab are grayed without a selection).
        let disabled = [0, 2];
        assert_eq!(first_enabled(6, &disabled), Some(1));
        // `git commit -m x`: the `g` is not the menu's — it closes the menu
        // and reaches the shell, so the Space after `git` never meets it.
        assert_eq!(classify(&Key::Character("g".into()), NONE), MenuPress::Other);
        // Only a Space or Enter pressed while the menu is still open runs the
        // highlighted row — the one the menu draws highlighted.
        assert_eq!(classify(&named(NamedKey::Space), NONE), MenuPress::Key(MenuKey::Activate));
        assert_eq!(step(MenuKey::Activate, Some(1), 6, &disabled), Some(MenuStep::Run(1)));
    }

    #[test]
    fn up_and_down_wrap_around() {
        // 6 rows, none grayed: Down from the last row is the first, Up from
        // the first is the last.
        assert_eq!(next_enabled(Some(0), 6, &[]), Some(1));
        assert_eq!(next_enabled(Some(5), 6, &[]), Some(0));
        assert_eq!(prev_enabled(Some(3), 6, &[]), Some(2));
        assert_eq!(prev_enabled(Some(0), 6, &[]), Some(5));
    }

    #[test]
    fn nothing_highlighted_down_takes_the_first_and_up_the_last() {
        assert_eq!(next_enabled(None, 6, &[]), Some(0));
        assert_eq!(prev_enabled(None, 6, &[]), Some(5));
        // … the first / last ENABLED ones (the main menu without a selection:
        // Copy=0 and Run in New Tab=2 grayed).
        assert_eq!(next_enabled(None, 6, &[0, 2]), Some(1));
        assert_eq!(prev_enabled(None, 6, &[5]), Some(4));
    }

    #[test]
    fn grayed_rows_are_skipped_both_ways_and_across_the_wrap() {
        let dis = [0, 2];
        assert_eq!(next_enabled(Some(1), 6, &dis), Some(3), "skips Run (2)");
        assert_eq!(prev_enabled(Some(3), 6, &dis), Some(1), "skips Run (2)");
        assert_eq!(next_enabled(Some(5), 6, &dis), Some(1), "wraps past Copy (0)");
        assert_eq!(prev_enabled(Some(1), 6, &dis), Some(5), "wraps past Copy (0)");
        // The detached menu without a selection: Copy=1, Run=3 grayed.
        assert_eq!(next_enabled(Some(0), 4, &[1, 3]), Some(2));
        assert_eq!(next_enabled(Some(2), 4, &[1, 3]), Some(0));
    }

    #[test]
    fn a_lone_enabled_row_stays_put() {
        assert_eq!(next_enabled(Some(2), 4, &[0, 1, 3]), Some(2));
        assert_eq!(prev_enabled(Some(2), 4, &[0, 1, 3]), Some(2));
    }

    #[test]
    fn all_grayed_or_empty_menus_highlight_nothing() {
        let all = [0, 1, 2];
        assert_eq!(first_enabled(3, &all), None);
        assert_eq!(last_enabled(3, &all), None);
        assert_eq!(next_enabled(None, 3, &all), None);
        assert_eq!(prev_enabled(Some(1), 3, &all), None);
        for key in [MenuKey::Prev, MenuKey::Next, MenuKey::First, MenuKey::Last] {
            assert_eq!(step(key, None, 3, &all), Some(MenuStep::Highlight(None)), "{key:?}");
            assert_eq!(step(key, None, 0, &[]), Some(MenuStep::Highlight(None)), "empty {key:?}");
        }
        assert_eq!(first_enabled(0, &[]), None);
        assert_eq!(next_enabled(Some(0), 0, &[]), None, "a stale highlight in an empty menu");
    }

    #[test]
    fn home_and_end_jump_to_the_first_and_last_enabled_rows() {
        assert_eq!(step(MenuKey::First, Some(4), 6, &[]), Some(MenuStep::Highlight(Some(0))));
        assert_eq!(step(MenuKey::Last, Some(1), 6, &[]), Some(MenuStep::Highlight(Some(5))));
        assert_eq!(step(MenuKey::First, None, 6, &[0, 2]), Some(MenuStep::Highlight(Some(1))));
        assert_eq!(step(MenuKey::Last, None, 4, &[3]), Some(MenuStep::Highlight(Some(2))));
    }

    #[test]
    fn enter_space_and_right_need_a_highlighted_row() {
        // With a row highlighted they act on it …
        assert_eq!(step(MenuKey::Activate, Some(3), 6, &[]), Some(MenuStep::Run(3)));
        assert_eq!(step(MenuKey::Open, Some(2), 4, &[]), Some(MenuStep::Open(2)));
        // … with none they do nothing (the menu keeps the key).
        assert_eq!(step(MenuKey::Activate, None, 6, &[]), None);
        assert_eq!(step(MenuKey::Open, None, 4, &[]), None);
        // A highlight can never sit on a grayed row; if one did, it runs nothing.
        assert_eq!(step(MenuKey::Activate, Some(0), 6, &[0, 2]), None);
        // Left always reaches the menu (the caller knows whether it is a submenu).
        assert_eq!(step(MenuKey::Back, None, 7, &[]), Some(MenuStep::Back));
    }

    #[test]
    fn moves_from_a_highlight_go_through_step() {
        assert_eq!(step(MenuKey::Next, Some(1), 6, &[0, 2]), Some(MenuStep::Highlight(Some(3))));
        assert_eq!(step(MenuKey::Prev, Some(1), 6, &[0, 2]), Some(MenuStep::Highlight(Some(5))));
        // Down from nothing: the first enabled row.
        assert_eq!(step(MenuKey::Next, None, 6, &[0, 2]), Some(MenuStep::Highlight(Some(1))));
    }

    #[test]
    fn the_pointer_moves_the_highlight_only_when_it_crosses_rows() {
        let mut last = None;
        // Moves off the rows (a nudge after the menu opened): no change — a
        // keyboard highlight survives.
        assert_eq!(pointer_hover(None, &mut last, &[]), None);
        // Onto row 2: the highlight follows the pointer …
        assert_eq!(pointer_hover(Some(2), &mut last, &[]), Some(Some(2)));
        // … moves within row 2 change nothing (the arrows may have moved on) …
        assert_eq!(pointer_hover(Some(2), &mut last, &[]), None);
        // … onto a grayed row: nothing highlighted, like the mouse alone …
        assert_eq!(pointer_hover(Some(0), &mut last, &[0]), Some(None));
        // … and off the rows again: cleared once, then left alone.
        assert_eq!(pointer_hover(Some(3), &mut last, &[0]), Some(Some(3)));
        assert_eq!(pointer_hover(None, &mut last, &[0]), Some(None));
        assert_eq!(pointer_hover(None, &mut last, &[0]), None);
    }

    #[test]
    fn the_keyboard_menu_opens_below_the_cursor_line_when_it_fits() {
        // Cell (row 2, col 5) of a 10×20 grid whose origin is (8, 40): the
        // card's top-left is the cell's bottom-left corner.
        let origin = jetty_render::GridOrigin::new(8.0, 40.0);
        assert_eq!(cursor_anchor(2, 5, (10.0, 20.0), origin, 200.0, 800.0), (58.0, 100.0));
        // Exactly fitting still opens below.
        assert_eq!(cursor_anchor(2, 5, (10.0, 20.0), origin, 700.0, 800.0), (58.0, 100.0));
        // Fractional cells land on whole pixels.
        assert_eq!(cursor_anchor(0, 3, (9.6, 19.5), origin, 200.0, 800.0), (37.0, 60.0));
    }

    #[test]
    fn the_keyboard_menu_flips_above_a_prompt_on_the_bottom_rows() {
        // Row 30's line is 640..660; a 200px card below it would end at 860 >
        // 800: it opens above the line instead, its bottom on the line's top.
        let origin = jetty_render::GridOrigin::new(8.0, 40.0);
        assert_eq!(cursor_anchor(30, 0, (10.0, 20.0), origin, 200.0, 800.0), (8.0, 440.0));
        // Fitting neither way (a tiny window): below, and the builder clamps.
        assert_eq!(cursor_anchor(1, 0, (10.0, 20.0), origin, 300.0, 200.0), (8.0, 80.0));
    }
}
