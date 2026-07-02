use crate::Rect;

/// The five clickable items in the right-click context menu, in display order.
///
/// The separator between "Select All" (idx 2) and "Clear" (idx 3) is purely
/// visual — a thin quad drawn in the gap — and does NOT appear here.
/// Click-index → action mapping is therefore 0..4 with no gaps.
pub const MENU_ITEMS: [&str; 5] = ["Copy", "Paste", "Select All", "Clear", "Close Tab"];

/// Right-aligned keyboard-shortcut hints shown beside each item.
///
/// Blank for items that have no shortcut.  The symbols (⇧ ⌃) render
/// correctly through MesloLGS NF (the chrome Nerd Font).  If glyph
/// fallback is ever needed, replace with "Ctrl+Shift+C" style strings to
/// match the help.rs overlay.
///
/// Verified against input.rs bindings:
///   ⇧⌃C = Ctrl+Shift+C → KeyAction::Copy
///   ⇧⌃V = Ctrl+Shift+V → KeyAction::Paste
///   ⌃L  = Ctrl+L       → ctrl_byte(L) = 0x0C (form-feed / clear)
///   ⇧⌃W = Ctrl+Shift+W → KeyAction::CloseTab
pub const MENU_HINTS: [&str; 5] = ["⇧⌃C", "⇧⌃V", "", "⌃L", "⇧⌃W"];

// --- Layout constants ---
//
// Fixed minimum card width (the historical value that fits the standard menu at
// the scale-1 chrome advance). `build_menu` uses this only as a FLOOR: the real
// width is measured from the items with the passed `char_w` so that on HiDPI /
// large UI fonts (where `char_w` grows) the card widens to keep the shortcut hint
// from overlapping the label.
const MENU_W: f32 = 210.0;
/// Inner left/right padding of the menu card (px, logical).
const MENU_HPAD: f32 = 10.0;
/// Minimum gap between a label's right edge and its right-aligned hint (px).
const MENU_LABEL_HINT_GAP: f32 = 20.0;
const ROW_H: f32 = 28.0;
/// Extra vertical gap inserted between "Select All" (idx 2) and "Clear"
/// (idx 3) to house the separator line.
const SEP_GAP: f32 = 10.0;
/// Total clickable items count of the STANDARD menu (5). The generic
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
    /// Hit-test rects, one per item in MENU_ITEMS order (5 rects).
    /// The separator occupies dead space between rect[2] and rect[3].
    pub item_rects: Vec<Rect>,
}

/// Row-top Y for item `i` in content-space, accounting for the separator gaps
/// that sit before each index in `sep_before`.
#[inline]
fn row_y_in(i: usize, sep_before: &[usize]) -> f32 {
    let gaps = sep_before.iter().filter(|&&s| s <= i && s > 0).count() as f32;
    i as f32 * ROW_H + gaps * SEP_GAP
}

/// Build the standard right-click context menu (Copy / Paste / Select All /
/// Clear / Close Tab) anchored at `(x, y)` (physical pixels). A thin wrapper
/// over the generic `build_menu` with `MENU_ITEMS`/`MENU_HINTS` and the visual
/// separator before "Clear" (idx 3).
///
/// `char_w` is the measured physical-pixel advance of one chrome-font character
/// (from `TextLayer::cell_size().0`). Pass `9.8` when a real measurement is not
/// available (scale-1 fallback used by tests).
pub fn build_context_menu(
    x: f32,
    y: f32,
    win_w: u32,
    win_h: u32,
    hovered: Option<usize>,
    theme: &jetty_core::Theme,
    char_w: f32,
) -> ContextMenu {
    let items: Vec<(&str, &str)> = MENU_ITEMS
        .iter()
        .copied()
        .zip(MENU_HINTS.iter().copied())
        .collect();
    build_menu(x, y, win_w, win_h, hovered, theme, char_w, &items, &[3])
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
///
/// `char_w` is the measured chrome-font advance (see `build_context_menu`).
/// The shortcut-hint glyphs (⇧ ⌃) are wider than ASCII; the hint width is
/// computed as `char_w * 1.25` to account for this — at scale 1 this gives
/// ≈ 12.25 px/glyph, matching the previous hardcoded 12.0 estimate.
#[allow(clippy::too_many_arguments)]
pub fn build_menu(
    x: f32,
    y: f32,
    win_w: u32,
    win_h: u32,
    hovered: Option<usize>,
    theme: &jetty_core::Theme,
    char_w: f32,
    items: &[(&str, &str)],
    sep_before: &[usize],
) -> ContextMenu {
    let sw = win_w as f32;
    let sh = win_h as f32;
    let n_items = items.len();
    let n_seps = sep_before.iter().filter(|&&s| s > 0 && s < n_items).count();
    let menu_h = ROW_H * n_items as f32 + SEP_GAP * n_seps as f32;

    // Card width measured from the widest row so the hint never overlaps the label
    // on HiDPI / large UI fonts (`char_w` scales with those). Each row needs
    // left-pad + label + [gap + hint] + right-pad; hint glyphs (⇧ ⌃) render ~1.25×
    // an ASCII cell, matching the hint layout below. Floored at MENU_W so the
    // standard menu at the scale-1 advance is unchanged.
    let menu_w = items
        .iter()
        .map(|(label, hint)| {
            let label_w = label.chars().count() as f32 * char_w;
            let hint_extra = if hint.is_empty() {
                0.0
            } else {
                MENU_LABEL_HINT_GAP + hint.chars().count() as f32 * (char_w * 1.25)
            };
            2.0 * MENU_HPAD + label_w + hint_extra
        })
        .fold(MENU_W, f32::max);

    // --- Theme-derived menu colors (mirrors panel.rs::build_panel) ---
    let tbg = theme.bg;
    let tfg = theme.fg;
    let accent = theme.palette[4]; // blue accent → hover highlight
    let lerp = |t: f32| -> [u8; 3] {
        [
            (tbg[0] as f32 + (tfg[0] as f32 - tbg[0] as f32) * t).round() as u8,
            (tbg[1] as f32 + (tfg[1] as f32 - tbg[1] as f32) * t).round() as u8,
            (tbg[2] as f32 + (tfg[2] as f32 - tbg[2] as f32) * t).round() as u8,
        ]
    };
    let bg3 = lerp(0.06);
    let menu_bg: [u8; 4] = [bg3[0], bg3[1], bg3[2], 242];
    let row3 = lerp(0.10);
    let row_bg: [u8; 4] = [row3[0], row3[1], row3[2], 255];
    let border3 = lerp(0.30);
    let border_col: [u8; 4] = [border3[0], border3[1], border3[2], 255];
    let hover_col: [u8; 4] = [accent[0], accent[1], accent[2], 255];
    let text_col = lerp(0.85);
    // Dim color for shortcut hints and the separator line.
    let hint_col = lerp(0.40);
    let sep_col: [u8; 4] = {
        let c = lerp(0.20);
        [c[0], c[1], c[2], 200]
    };

    // Clamp so the full menu (plus border) stays on-screen.
    let total_w = menu_w + BORDER * 2.0;
    let total_h = menu_h + BORDER * 2.0;
    let mx = x.min(sw - total_w).max(0.0);
    let my = y.min(sh - total_h).max(0.0);

    // Content area (inside the border).
    let cx = mx + BORDER;
    let cy = my + BORDER;

    // Build item rects (also serve as hit-test rects).
    // Each rect sits at its visual row position; separator gaps are dead
    // space — not hit rects.
    let mut item_rects: Vec<Rect> = Vec::with_capacity(n_items);
    for i in 0..n_items {
        item_rects.push(Rect {
            x: cx,
            y: cy + row_y_in(i, sep_before),
            w: menu_w,
            h: ROW_H,
            color: row_bg,
            ..Default::default()
        });
    }

    let mut quads: Vec<Rect> = Vec::new();

    // Outer border quad (a 2px halo around the content), rounded to bg+2 so the
    // halo width is uniform — matches panel/help/confirm.
    quads.push(Rect::rounded(mx, my, total_w, total_h, border_col, 8.0));

    // Background panel (rounded; hover rows stay sharp inside).
    quads.push(Rect::rounded(cx, cy, menu_w, menu_h, menu_bg, 6.0));

    // Hover highlight quad (drawn on top of background, under labels). The top
    // and bottom rows get the bg's corner radius so the highlight doesn't square
    // off the rounded card corners; interior rows stay sharp.
    if let Some(idx) = hovered {
        if idx < n_items {
            let radius = if idx == 0 || idx == n_items - 1 { 6.0 } else { 0.0 };
            quads.push(Rect {
                x: cx,
                y: cy + row_y_in(idx, sep_before),
                w: menu_w,
                h: ROW_H,
                color: hover_col,
                radius,
            });
        }
    }

    // Separator lines: a thin (1px) dim quad in the middle of each SEP_GAP,
    // inset by 10px on each side so it doesn't butt against the rounded corners.
    for &s in sep_before.iter().filter(|&&s| s > 0 && s < n_items) {
        let sep_y = cy + row_y_in(s, sep_before) - SEP_GAP + (SEP_GAP - 1.0) * 0.5;
        quads.push(Rect {
            x: cx + 10.0,
            y: sep_y,
            w: menu_w - 20.0,
            h: 1.0,
            color: sep_col,
            radius: 0.0,
        });
    }

    // Labels: item name (left-aligned) + shortcut hint (right-aligned, dim).
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    for (i, &(name, hint)) in items.iter().enumerate() {
        let label_y = cy + row_y_in(i, sep_before) + 7.0; // 7px from row top — matches original
        labels.push((name.to_string(), cx + 10.0, label_y, text_col));

        if !hint.is_empty() {
            // Right-align the shortcut hint. Unicode glyph hints (⇧ ⌃) render
            // wider than plain ASCII; use char_w * 1.25 so the reservation
            // scales correctly on HiDPI (at scale 1, 9.8 * 1.25 ≈ 12.25 px —
            // the same as the former hardcoded 12.0 estimate).
            let hint_w = hint.chars().count() as f32 * (char_w * 1.25);
            let hint_x = cx + menu_w - 10.0 - hint_w;
            labels.push((hint.to_string(), hint_x, label_y, hint_col));
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

    /// Use a real theme preset so the struct fields never need manual updating.
    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests (matches the historical 9.8 estimate).
    const TEST_CHAR_W: f32 = 9.8;

    #[test]
    fn exactly_five_item_rects() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), TEST_CHAR_W);
        assert_eq!(
            menu.item_rects.len(),
            5,
            "must have exactly 5 hit-rects (one per clickable item)"
        );
    }

    #[test]
    fn separator_not_a_hit_rect() {
        // The separator is drawn as a quad in `quads`, NOT as an item_rect.
        // Verify that five item_rects exist and the gap between rect[2] and
        // rect[3] is positive (SEP_GAP wide).
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), TEST_CHAR_W);
        assert_eq!(menu.item_rects.len(), 5);
        let bottom_of_2 = menu.item_rects[2].y + menu.item_rects[2].h;
        let top_of_3 = menu.item_rects[3].y;
        assert!(
            top_of_3 > bottom_of_2,
            "rect[3] must start below rect[2] bottom (gap = {})",
            top_of_3 - bottom_of_2
        );
    }

    #[test]
    fn hint_does_not_overlap_label_on_hidpi() {
        // At a 2× chrome advance the fixed 210px card is too narrow ("Close Tab"
        // plus its hint would collide); the card must widen so the right-aligned
        // hint stays clear of the label.
        let big = 2.0 * TEST_CHAR_W;
        let menu = build_context_menu(100.0, 100.0, 3000, 2000, None, &theme(), big);
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
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), TEST_CHAR_W);
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
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme(), TEST_CHAR_W);
        let texts: Vec<&str> = menu.labels.iter().map(|(t, ..)| t.as_str()).collect();
        for item in MENU_ITEMS.iter() {
            assert!(texts.contains(item), "item {:?} missing from labels", item);
        }
    }

    #[test]
    fn menu_stays_on_screen_when_clamped() {
        // Anchor near bottom-right corner — menu must clamp entirely on-screen.
        let (win_w, win_h) = (800u32, 600u32);
        let menu = build_context_menu(790.0, 590.0, win_w, win_h, None, &theme(), TEST_CHAR_W);
        let total_w = MENU_W + BORDER * 2.0;
        let total_h = MENU_H + BORDER * 2.0;
        // The outer border rect is the first quad.
        let outer = &menu.quads[0];
        assert!(outer.x >= 0.0);
        assert!(outer.y >= 0.0);
        assert!(
            outer.x + total_w <= win_w as f32 + 1.0,
            "right edge overflows: {} > {}",
            outer.x + total_w,
            win_w
        );
        assert!(
            outer.y + total_h <= win_h as f32 + 1.0,
            "bottom edge overflows: {} > {}",
            outer.y + total_h,
            win_h
        );
    }

    #[test]
    fn menu_items_order_is_copy_paste_selectall_clear_closetab() {
        // Pin the exact MENU_ITEMS order so accidental reordering is caught.
        assert_eq!(MENU_ITEMS[0], "Copy",       "item[0] must be Copy");
        assert_eq!(MENU_ITEMS[1], "Paste",      "item[1] must be Paste");
        assert_eq!(MENU_ITEMS[2], "Select All", "item[2] must be Select All");
        assert_eq!(MENU_ITEMS[3], "Clear",      "item[3] must be Clear");
        assert_eq!(MENU_ITEMS[4], "Close Tab",  "item[4] must be Close Tab");
    }

    #[test]
    fn click_in_separator_gap_hits_no_item_rect() {
        // The separator gap between item[2] (Select All) and item[3] (Clear) must
        // be a dead zone — a click coordinate inside it should not fall within any
        // item_rect.
        let menu = build_context_menu(50.0, 50.0, 1280, 800, None, &theme(), TEST_CHAR_W);
        assert_eq!(menu.item_rects.len(), 5);

        // The dead zone is the pixel band between bottom of rect[2] and top of rect[3].
        let gap_top    = menu.item_rects[2].y + menu.item_rects[2].h;
        let gap_bottom = menu.item_rects[3].y;
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
        let menu = build_menu(50.0, 50.0, 1280, 800, None, &theme(), TEST_CHAR_W, &items, &[]);
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
                790.0, 590.0, 800, 600, Some(hovered), &theme(), TEST_CHAR_W, &items, &[],
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
    fn legacy_menu_matches_generic_with_separator_at_3() {
        // build_context_menu is now a wrapper over build_menu; pin that the
        // separator layout (gap before item 3) is preserved exactly.
        let legacy = build_context_menu(100.0, 100.0, 1280, 800, Some(4), &theme(), TEST_CHAR_W);
        let items: Vec<(&str, &str)> = MENU_ITEMS
            .iter()
            .copied()
            .zip(MENU_HINTS.iter().copied())
            .collect();
        let generic = build_menu(100.0, 100.0, 1280, 800, Some(4), &theme(), TEST_CHAR_W, &items, &[3]);
        assert_eq!(legacy.item_rects.len(), generic.item_rects.len());
        for (a, b) in legacy.item_rects.iter().zip(&generic.item_rects) {
            assert_eq!(a.y, b.y);
        }
        assert_eq!(legacy.quads.len(), generic.quads.len());
    }

    #[test]
    fn hover_highlight_aligns_with_item_rects() {
        // For each of the 5 items, the hover quad y must match the item rect y.
        // Quad order: [0] border, [1] bg, [2] hover, [3] separator.
        for hovered in 0..5 {
            let menu = build_context_menu(50.0, 50.0, 1280, 800, Some(hovered), &theme(), TEST_CHAR_W);
            let hover_quad = &menu.quads[2];
            let item = &menu.item_rects[hovered];
            assert_eq!(
                hover_quad.y, item.y,
                "hover y mismatch at idx {}: quad={} item={}",
                hovered, hover_quad.y, item.y
            );
        }
    }
}
