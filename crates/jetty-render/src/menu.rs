use crate::chrome::{ChromeMeasure, ChromeMetrics};
use crate::ui_palette::{ensure_contrast, mix, rgba, UiPalette};
use crate::Rect;

/// The six clickable items in the right-click context menu, in display order.
/// "Run in New Tab" sits right after the clipboard pair — the browser puts
/// "Open link in new tab" adjacent to the link actions.
///
/// The separator between "Select All" (idx 3) and "Clear" (idx 4) is purely
/// visual — a thin quad drawn in the gap — and does NOT appear here.
/// Click-index → action mapping is therefore 0..5 with no gaps.
pub const MENU_ITEMS: [&str; 6] =
    ["Copy", "Paste", "Run in New Tab", "Select All", "Clear", "Close Tab"];

/// The DEFAULT (Linux) keyboard-shortcut hints for [`MENU_ITEMS`], used by
/// tests and fixed-size harnesses. The app passes hints derived from the LIVE
/// keymap instead (a `[keys]` remap, or macOS's ⌘ chords, must show in the
/// menu) — "Clear" is the only fixed one (it is the raw Ctrl+L byte, not a
/// remappable action). Blank for items that have no shortcut. The symbols
/// (⇧ ⌃) render through MesloLGS NF (the chrome Nerd Font).
///
/// Default bindings:
///   ⇧⌃C = Ctrl+Shift+C     → KeyAction::Copy
///   ⇧⌃V = Ctrl+Shift+V     → KeyAction::Paste
///   ⇧⌃⏎ = Ctrl+Shift+Enter → KeyAction::RunSelection
///   ⌃L  = Ctrl+L           → ctrl_byte(L) = 0x0C (form-feed / clear)
///   ⇧⌃W = Ctrl+Shift+W     → KeyAction::CloseTab
pub const MENU_HINTS: [&str; 6] = ["⇧⌃C", "⇧⌃V", "⇧⌃⏎", "", "⌃L", "⇧⌃W"];

// --- Layout constants (DESIGN px: 16pt UI font, 1× — scaled by the chrome unit) ---
//
// Minimum card width (the historical value that fits the standard menu at the
// baseline). `build_menu` uses this only as a FLOOR: the real width is measured
// from the items, so with a wide UI font / long labels the card widens to keep
// the shortcut hint from overlapping the label.
const MENU_W: f32 = 210.0;
/// Inner left/right padding of the menu card (px, logical).
const MENU_HPAD: f32 = 10.0;
/// Minimum gap between a label's right edge and its right-aligned hint (px).
const MENU_LABEL_HINT_GAP: f32 = 20.0;
const ROW_H: f32 = 28.0;
/// Extra vertical gap inserted between "Select All" (idx 2) and "Clear"
/// (idx 3) to house the separator line.
const SEP_GAP: f32 = 10.0;
/// Total clickable items count of the STANDARD menu (6). The generic
/// `build_menu` derives its count from the item list; this and `MENU_H` remain
/// as the standard menu's reference values (asserted by the unit tests).
#[allow(dead_code)]
const N: usize = MENU_ITEMS.len();
/// Standard menu content height: N rows × ROW_H + one separator gap.
#[allow(dead_code)]
const MENU_H: f32 = ROW_H * N as f32 + SEP_GAP;
// 2px halo to match every other overlay (panel/help/confirm all use a 2px
// border with a radius delta of 2 over the bg).
const BORDER: f32 = 2.0;

/// Geometry and draw data for the right-click context menu.
pub struct ContextMenu {
    /// Quads in draw order: background panel, optional border, hover highlight,
    /// and the thin separator quad.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb).  Includes both item labels and the
    /// right-aligned shortcut hints.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Hit-test rects, one per item in MENU_ITEMS order (6 rects).
    /// The separator occupies dead space between rect[3] and rect[4].
    pub item_rects: Vec<Rect>,
}

/// Row-top Y for item `i` in content-space, accounting for the separator gaps
/// that sit before each index in `sep_before`.
#[inline]
fn row_y_in(i: usize, sep_before: &[usize]) -> f32 {
    let gaps = sep_before.iter().filter(|&&s| s <= i && s > 0).count() as f32;
    i as f32 * ROW_H + gaps * SEP_GAP
}

/// Build the standard right-click context menu (Copy / Paste / Run in New Tab /
/// Select All / Clear / Close Tab) anchored at `(x, y)` (physical pixels). A
/// thin wrapper over the generic `build_menu` with `MENU_ITEMS`/`MENU_HINTS`
/// and the visual separator before "Clear" (idx 4).
///
/// `disabled` lists item indices drawn dim (label + hint in the hint color)
/// with hover suppressed — native-menu grayed rows; a click on one is the
/// caller's no-op. Indices NEVER shift (the array is static), so cached
/// hit-rects stay valid.
///
/// `hints` are the shortcut hints for the six items in `MENU_ITEMS` order (the
/// app derives them from the live keymap; [`MENU_HINTS`] is the default set);
/// a missing entry reads as blank. `m` / `cm`: see [`build_menu`].
#[allow(clippy::too_many_arguments)]
pub fn build_context_menu(
    x: f32,
    y: f32,
    win_w: u32,
    win_h: u32,
    hovered: Option<usize>,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    hints: &[&str],
    disabled: &[usize],
) -> ContextMenu {
    let items: Vec<(&str, &str)> = MENU_ITEMS
        .iter()
        .enumerate()
        .map(|(i, &label)| (label, hints.get(i).copied().unwrap_or("")))
        .collect();
    build_menu(x, y, win_w, win_h, hovered, theme, m, cm, &items, &[4], disabled)
}

/// Build a context menu from an arbitrary `(label, hint)` item list anchored at
/// `(x, y)` (physical pixels) — the generic builder behind `build_context_menu`,
/// also used for the tab context menu (Detach / Rename / Close Tab) and the
/// detached-window menu (Reattach / Copy / Paste).
///
/// The menu is clamped so its right and bottom edges stay within the window.
/// `hovered` is the index (0-based) of the item under the cursor, if any.
/// `sep_before` lists item indices that get a thin separator line (in a
/// `SEP_GAP` dead zone) drawn ABOVE them; pass `&[]` for no separators.
/// `disabled` lists item indices drawn dim with the hover highlight
/// suppressed (grayed rows); pass `&[]` for none.
///
/// `m` measures the labels/hints exactly as the chrome text pass renders them
/// (so the card fits a proportional UI font and the hints right-align flush);
/// `cm` scales rows, paddings and radii with the DPI × UI-font chrome unit, so
/// a large UI font gets taller rows instead of glyphs overflowing a fixed 28px
/// row.
#[allow(clippy::too_many_arguments)]
pub fn build_menu(
    x: f32,
    y: f32,
    win_w: u32,
    win_h: u32,
    hovered: Option<usize>,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    items: &[(&str, &str)],
    sep_before: &[usize],
    disabled: &[usize],
) -> ContextMenu {
    let sw = win_w as f32;
    let sh = win_h as f32;
    let n_items = items.len();
    let n_seps = sep_before.iter().filter(|&&s| s > 0 && s < n_items).count();
    let row_h = cm.px(ROW_H);
    let sep_gap = cm.px(SEP_GAP);
    let hpad = cm.px(MENU_HPAD);
    let border = cm.px(BORDER);
    let menu_h = row_h * n_items as f32 + sep_gap * n_seps as f32;

    // Card width measured from the widest row so the hint never overlaps the
    // label. Each row needs left-pad + label + [gap + hint] + right-pad. Floored
    // at MENU_W so the standard menu at the baseline keeps its classic width.
    let measured: Vec<(f32, f32)> =
        items.iter().map(|(label, hint)| (m.text_w(label), m.text_w(hint))).collect();
    let menu_w = measured
        .iter()
        .zip(items)
        .map(|(&(label_w, hint_w), (_, hint))| {
            let hint_extra = if hint.is_empty() { 0.0 } else { cm.px(MENU_LABEL_HINT_GAP) + hint_w };
            2.0 * hpad + label_w + hint_extra
        })
        .fold(cm.px(MENU_W), f32::max);

    // --- Theme-derived menu colors (the shared UiPalette) ---
    let ui = UiPalette::cached(theme);
    let menu_bg = rgba(ui.surface, 242);
    let row_bg = rgba(ui.shade(0.10), 255);
    let border_col = rgba(ui.border, 255);
    let hover_col = rgba(ui.accent, 255);
    let text_col = ui.text;
    // Dim color for shortcut hints and disabled rows.
    let hint_col = ui.text_hint;
    let sep_col = rgba(ui.shade(0.20), 200);
    // The hovered row sits on the accent: its label takes the accent's own text
    // color (it used to keep the surface's label color — 1.0–1.7:1 on 20 of 22
    // themes) and its hint a dimmer shade of that, still ≥ 3:1.
    let hover_text = ui.on_accent;
    let hover_hint = ensure_contrast(mix(ui.on_accent, ui.accent, 0.3), &[ui.accent], UiPalette::HINT_FLOOR);

    // Clamp so the full menu (plus border) stays on-screen.
    let total_w = menu_w + border * 2.0;
    let total_h = menu_h + border * 2.0;
    let mx = x.min(sw - total_w).max(0.0);
    let my = y.min(sh - total_h).max(0.0);

    // Content area (inside the border).
    let cx = mx + border;
    let cy = my + border;
    // Row-top Y for item `i` in content space (separator gaps included).
    let row_y = |i: usize| row_y_in(i, sep_before) * cm.u;

    // Build item rects (also serve as hit-test rects).
    // Each rect sits at its visual row position; separator gaps are dead
    // space — not hit rects.
    let mut item_rects: Vec<Rect> = Vec::with_capacity(n_items);
    for i in 0..n_items {
        item_rects.push(Rect {
            x: cx,
            y: cy + row_y(i),
            w: menu_w,
            h: row_h,
            color: row_bg,
            ..Default::default()
        });
    }

    let mut quads: Vec<Rect> = Vec::new();

    // Outer border quad (a 2px halo around the content), rounded to bg+2 so the
    // halo width is uniform — matches panel/help/confirm.
    quads.push(Rect::rounded(mx, my, total_w, total_h, border_col, cm.px(8.0)));

    // Background panel (rounded; hover rows stay sharp inside).
    quads.push(Rect::rounded(cx, cy, menu_w, menu_h, menu_bg, cm.px(6.0)));

    // Hover highlight quad (drawn on top of background, under labels). The top
    // and bottom rows get the bg's corner radius so the highlight doesn't square
    // off the rounded card corners; interior rows stay sharp. Disabled rows
    // never highlight (grayed rows are inert).
    if let Some(idx) = hovered {
        if idx < n_items && !disabled.contains(&idx) {
            let radius = if idx == 0 || idx == n_items - 1 { cm.px(6.0) } else { 0.0 };
            quads.push(Rect {
                x: cx,
                y: cy + row_y(idx),
                w: menu_w,
                h: row_h,
                color: hover_col,
                radius,
            });
        }
    }

    // Separator lines: a thin (1px) dim quad in the middle of each SEP_GAP,
    // inset by 10px on each side so it doesn't butt against the rounded corners.
    for &s in sep_before.iter().filter(|&&s| s > 0 && s < n_items) {
        let sep_y = cy + row_y(s) - sep_gap + (sep_gap - 1.0) * 0.5;
        quads.push(Rect {
            x: cx + hpad,
            y: sep_y,
            w: menu_w - 2.0 * hpad,
            h: 1.0,
            color: sep_col,
            radius: 0.0,
        });
    }

    // Labels: item name (left-aligned) + shortcut hint (right-aligned, dim).
    // Disabled rows draw the LABEL in the dim hint color too (grayed).
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let hovered_row = hovered.filter(|&h| h < n_items && !disabled.contains(&h));
    for (i, &(name, hint)) in items.iter().enumerate() {
        let label_y = cy + row_y(i) + cm.px(7.0); // 7 design px from the row top
        let on_hover = hovered_row == Some(i);
        let label_col = if disabled.contains(&i) {
            hint_col
        } else if on_hover {
            hover_text
        } else {
            text_col
        };
        labels.push((name.to_string(), cx + hpad, label_y, label_col));

        if !hint.is_empty() {
            // Right-align the shortcut hint flush with the right padding, by its
            // MEASURED width (the ⇧ ⌃ ⌘ glyphs are not one ASCII cell each).
            let hint_x = cx + menu_w - hpad - measured[i].1;
            labels.push((hint.to_string(), hint_x, label_y, if on_hover { hover_hint } else { hint_col }));
        }
    }

    ContextMenu { quads, labels, item_rects }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    /// Use a real theme preset so the struct fields never need manual updating.
    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests (matches the historical 9.8 estimate).
    const TEST_CHAR_W: f32 = 9.8;
    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    #[test]
    fn exactly_six_item_rects() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        assert_eq!(
            menu.item_rects.len(),
            6,
            "must have exactly 6 hit-rects (one per clickable item)"
        );
    }

    #[test]
    fn separator_not_a_hit_rect() {
        // The separator is drawn as a quad in `quads`, NOT as an item_rect.
        // Verify that six item_rects exist and the gap between rect[3]
        // (Select All) and rect[4] (Clear) is positive (SEP_GAP wide).
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        assert_eq!(menu.item_rects.len(), 6);
        let bottom_of_3 = menu.item_rects[3].y + menu.item_rects[3].h;
        let top_of_4 = menu.item_rects[4].y;
        assert!(
            top_of_4 > bottom_of_3,
            "rect[4] must start below rect[3] bottom (gap = {})",
            top_of_4 - bottom_of_3
        );
    }

    #[test]
    fn hint_does_not_overlap_label_on_hidpi() {
        // At a 2× chrome advance the fixed 210px card is too narrow ("Close Tab"
        // plus its hint would collide); the card must widen so the right-aligned
        // hint stays clear of the label.
        let big = 2.0 * TEST_CHAR_W;
        let menu = build_context_menu(
            100.0, 100.0, 3000, 2000, None, &theme(), &mut MonoMeasure(big),
            ChromeMetrics::new(2.0, 16.0), &MENU_HINTS, &[],
        );
        let label = menu.labels.iter().find(|l| l.0 == "Close Tab").expect("label");
        let hint = menu.labels.iter().find(|l| l.0 == "⇧⌃W").expect("hint");
        let label_right = label.1 + "Close Tab".chars().count() as f32 * big;
        assert!(
            hint.1 >= label_right,
            "hint overlaps label on HiDPI: hint_x={} label_right={}",
            hint.1,
            label_right
        );
    }

    #[test]
    fn hints_present_in_labels() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        let texts: Vec<&str> = menu.labels.iter().map(|(t, ..)| t.as_str()).collect();
        // Each non-empty hint must appear as a label.
        for hint in MENU_HINTS.iter() {
            if !hint.is_empty() {
                assert!(
                    texts.contains(hint),
                    "hint {:?} missing from labels",
                    hint
                );
            }
        }
    }

    #[test]
    fn all_items_present_in_labels() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        let texts: Vec<&str> = menu.labels.iter().map(|(t, ..)| t.as_str()).collect();
        for item in MENU_ITEMS.iter() {
            assert!(texts.contains(item), "item {:?} missing from labels", item);
        }
    }

    #[test]
    fn menu_stays_on_screen_when_clamped() {
        // Anchor near bottom-right corner — menu must clamp entirely on-screen.
        // Width is taken from the MEASURED outer quad (with "Run in New Tab"
        // the real card is wider than the MENU_W floor, so asserting against
        // the floor would silently stop describing the right edge).
        let (win_w, win_h) = (800u32, 600u32);
        let menu = build_context_menu(790.0, 590.0, win_w, win_h, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        // The outer border rect is the first quad; its size IS total_w/total_h.
        let outer = &menu.quads[0];
        assert!(
            outer.w >= MENU_W + BORDER * 2.0,
            "6-item card must be at least the historical floor wide"
        );
        assert_eq!(outer.h, MENU_H + BORDER * 2.0, "6 rows + one separator gap");
        assert!(outer.x >= 0.0);
        assert!(outer.y >= 0.0);
        assert!(
            outer.x + outer.w <= win_w as f32 + 1.0,
            "right edge overflows: {} > {}",
            outer.x + outer.w,
            win_w
        );
        assert!(
            outer.y + outer.h <= win_h as f32 + 1.0,
            "bottom edge overflows: {} > {}",
            outer.y + outer.h,
            win_h
        );
    }

    #[test]
    fn menu_items_order_is_copy_paste_run_selectall_clear_closetab() {
        // Pin the exact MENU_ITEMS order so accidental reordering is caught —
        // app.rs's click dispatch matches on these hard indices.
        assert_eq!(MENU_ITEMS[0], "Copy",           "item[0] must be Copy");
        assert_eq!(MENU_ITEMS[1], "Paste",          "item[1] must be Paste");
        assert_eq!(MENU_ITEMS[2], "Run in New Tab", "item[2] must be Run in New Tab");
        assert_eq!(MENU_ITEMS[3], "Select All",     "item[3] must be Select All");
        assert_eq!(MENU_ITEMS[4], "Clear",          "item[4] must be Clear");
        assert_eq!(MENU_ITEMS[5], "Close Tab",      "item[5] must be Close Tab");
        assert_eq!(MENU_HINTS[2], "⇧⌃⏎", "Run's hint is the Ctrl+Shift+Enter glyph");
    }

    #[test]
    fn click_in_separator_gap_hits_no_item_rect() {
        // The separator gap between item[3] (Select All) and item[4] (Clear) must
        // be a dead zone — a click coordinate inside it should not fall within any
        // item_rect.
        let menu = build_context_menu(50.0, 50.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        assert_eq!(menu.item_rects.len(), 6);

        // The dead zone is the pixel band between bottom of rect[3] and top of rect[4].
        let gap_top    = menu.item_rects[3].y + menu.item_rects[3].h;
        let gap_bottom = menu.item_rects[4].y;
        assert!(gap_bottom > gap_top, "expected a separator gap but rects are adjacent");

        // A click in the middle of the gap.
        let click_y = (gap_top + gap_bottom) * 0.5;
        let click_x = menu.item_rects[0].x + menu.item_rects[0].w * 0.5;

        let hit = menu.item_rects.iter().any(|r| {
            click_x >= r.x && click_x < r.x + r.w && click_y >= r.y && click_y < r.y + r.h
        });
        assert!(!hit, "click in separator gap (y={click_y}) should hit no item_rect");
    }

    #[test]
    fn generic_menu_one_rect_per_item_no_separator() {
        // The generic builder (used by the tab / detached context menus) emits
        // exactly one hit-rect per item, adjacent when no separator is passed.
        let items = [("Detach", "⇧⌃D"), ("Rename", ""), ("Close Tab", "⇧⌃W")];
        let menu = build_menu(50.0, 50.0, 1280, 800, None, &theme(), &mut mono(), CM, &items, &[], &[]);
        assert_eq!(menu.item_rects.len(), 3);
        for pair in menu.item_rects.windows(2) {
            assert_eq!(
                pair[0].y + pair[0].h,
                pair[1].y,
                "rows must be adjacent without a separator"
            );
        }
        let texts: Vec<&str> = menu.labels.iter().map(|(t, ..)| t.as_str()).collect();
        for (label, _) in items.iter() {
            assert!(texts.contains(label), "item {label:?} missing from labels");
        }
        assert!(texts.contains(&"⇧⌃D"), "hint missing from labels");
    }

    #[test]
    fn generic_menu_hover_aligns_and_stays_on_screen() {
        let items = [("Reattach", "⇧⌃D"), ("Copy", "⇧⌃C"), ("Paste", "⇧⌃V")];
        for hovered in 0..items.len() {
            let menu = build_menu(
                790.0, 590.0, 800, 600, Some(hovered), &theme(), &mut mono(), CM, &items, &[], &[],
            );
            // Quad order without separators: [0] border, [1] bg, [2] hover.
            let hover_quad = &menu.quads[2];
            assert_eq!(hover_quad.y, menu.item_rects[hovered].y);
            // Clamped fully on-screen even when anchored at the corner.
            let outer = &menu.quads[0];
            assert!(outer.x >= 0.0 && outer.y >= 0.0);
            assert!(outer.x + MENU_W + BORDER * 2.0 <= 800.0 + 1.0);
        }
    }

    #[test]
    fn legacy_menu_matches_generic_with_separator_at_4() {
        // build_context_menu is now a wrapper over build_menu; pin that the
        // separator layout (gap before item 4, "Clear") is preserved exactly.
        let legacy = build_context_menu(100.0, 100.0, 1280, 800, Some(5), &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        let items: Vec<(&str, &str)> = MENU_ITEMS
            .iter()
            .copied()
            .zip(MENU_HINTS.iter().copied())
            .collect();
        let generic = build_menu(100.0, 100.0, 1280, 800, Some(5), &theme(), &mut mono(), CM, &items, &[4], &[]);
        assert_eq!(legacy.item_rects.len(), generic.item_rects.len());
        for (a, b) in legacy.item_rects.iter().zip(&generic.item_rects) {
            assert_eq!(a.y, b.y);
        }
        assert_eq!(legacy.quads.len(), generic.quads.len());
    }

    #[test]
    fn hover_highlight_aligns_with_item_rects() {
        // For each of the 6 items, the hover quad y must match the item rect y.
        // Quad order: [0] border, [1] bg, [2] hover, [3] separator.
        for hovered in 0..6 {
            let menu = build_context_menu(50.0, 50.0, 1280, 800, Some(hovered), &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
            let hover_quad = &menu.quads[2];
            let item = &menu.item_rects[hovered];
            assert_eq!(
                hover_quad.y, item.y,
                "hover y mismatch at idx {}: quad={} item={}",
                hovered, hover_quad.y, item.y
            );
        }
    }

    #[test]
    fn disabled_rows_draw_dim_and_suppress_hover() {
        // Disabled Copy (0) + Run (2): labels render in the dim hint color and
        // a hover over a disabled row produces NO hover quad — the quad list
        // is then [0] border, [1] bg, [2] separator (pinned, so tests that
        // index quads by position stay honest).
        let dis = [0usize, 2];
        let menu =
            build_context_menu(50.0, 50.0, 1280, 800, Some(2), &theme(), &mut mono(), CM, &MENU_HINTS, &dis);
        assert_eq!(
            menu.quads.len(),
            3,
            "hovered disabled row must not emit a hover quad (border+bg+separator only)"
        );
        // Compare label colors: disabled labels match the hint color of an
        // enabled row's hint (the dim hint role), enabled labels do not.
        let enabled = build_context_menu(50.0, 50.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        let color_of = |m: &ContextMenu, text: &str| {
            m.labels.iter().find(|l| l.0 == text).map(|l| l.3).unwrap()
        };
        let dim_hint = color_of(&enabled, "⇧⌃C"); // hint color reference
        assert_eq!(color_of(&menu, "Copy"), dim_hint, "disabled Copy label is dim");
        assert_eq!(color_of(&menu, "Run in New Tab"), dim_hint, "disabled Run label is dim");
        assert_ne!(color_of(&menu, "Paste"), dim_hint, "enabled Paste label stays bright");
        // An ENABLED row still highlights with the same disabled set present.
        let menu2 =
            build_context_menu(50.0, 50.0, 1280, 800, Some(1), &theme(), &mut mono(), CM, &MENU_HINTS, &dis);
        assert_eq!(menu2.quads.len(), 4, "enabled hover keeps its quad");
        assert_eq!(menu2.quads[2].y, menu2.item_rects[1].y);
    }

    #[test]
    fn every_label_reads_on_its_row_on_every_theme() {
        // The hovered label sits on the accent; the others on the menu surface.
        // Disabled rows and hints are hint-grade (3:1), labels text-grade (4.5:1).
        use crate::colors::contrast_ratio as cr;
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let menu = build_context_menu(50.0, 50.0, 1280, 800, Some(1), &t, &mut mono(), CM, &MENU_HINTS, &[0]);
            let surface = [menu.quads[1].color[0], menu.quads[1].color[1], menu.quads[1].color[2]];
            let hover = [menu.quads[2].color[0], menu.quads[2].color[1], menu.quads[2].color[2]];
            let col = |text: &str| menu.labels.iter().find(|l| l.0 == text).unwrap().3;
            let n = &t.name;
            assert!(cr(col("Paste"), hover) >= 4.5, "{n}: hovered label {}", cr(col("Paste"), hover));
            assert!(cr(col("⇧⌃V"), hover) >= 3.0, "{n}: hovered hint {}", cr(col("⇧⌃V"), hover));
            assert!(cr(col("Select All"), surface) >= 4.5, "{n}: label {}", cr(col("Select All"), surface));
            assert!(cr(col("⇧⌃W"), surface) >= 3.0, "{n}: hint {}", cr(col("⇧⌃W"), surface));
            assert!(cr(col("Copy"), surface) >= 3.0, "{n}: disabled label {}", cr(col("Copy"), surface));
        }
    }

    #[test]
    fn rows_scale_with_the_chrome_unit() {
        // A 28pt UI font (u = 1.75) gets 49px rows, and every label's line box
        // (ceil(font_px * 1.3)) stays inside its row instead of spilling into
        // the next one (the fixed 28px rows did at >= ~21pt).
        let cm = ChromeMetrics::new(1.0, 28.0);
        let menu = build_context_menu(
            10.0, 10.0, 2000, 2000, None, &theme(), &mut MonoMeasure(9.6 * cm.u), cm, &MENU_HINTS, &[],
        );
        let line_h = (16.0 * cm.u * 1.3).ceil();
        for (i, r) in menu.item_rects.iter().enumerate() {
            assert!((r.h - ROW_H * cm.u).abs() < 0.01, "row {i} not scaled");
            let label = menu.labels.iter().find(|l| l.0 == MENU_ITEMS[i]).unwrap();
            assert!(label.2 >= r.y && label.2 + line_h <= r.y + r.h + 0.5, "row {i} label spills");
        }
    }

    #[test]
    fn hints_right_align_flush_by_measured_width() {
        // The hint's measured right edge sits exactly on the right padding — no
        // more `chars * char_w * 1.25` guess leaving a ragged gap.
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &MENU_HINTS, &[]);
        let card = menu.item_rects[0];
        for hint in MENU_HINTS.iter().filter(|h| !h.is_empty()) {
            let l = menu.labels.iter().find(|l| l.0 == *hint).unwrap();
            let right = l.1 + mono().text_w(hint);
            assert!((right - (card.x + card.w - MENU_HPAD)).abs() < 0.01, "{hint} not flush: {right}");
        }
    }

    #[test]
    fn caller_hints_replace_the_defaults() {
        // The app feeds hints from the LIVE keymap (remaps, macOS ⌘ chords).
        let hints = ["⌘C", "⌘V", "", "⌘A", "⌃L", "⌘W"];
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &hints, &[]);
        let texts: Vec<&str> = menu.labels.iter().map(|l| l.0.as_str()).collect();
        for h in hints.iter().filter(|h| !h.is_empty()) {
            assert!(texts.contains(h), "{h} missing");
        }
        assert!(!texts.contains(&"⇧⌃C"), "default hint must not leak through");
        // A short hint list reads as blank for the missing items.
        let short = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), &mut mono(), CM, &["⇧⌃C"], &[]);
        assert_eq!(short.labels.len(), 7, "6 labels + the one hint");
    }
}
