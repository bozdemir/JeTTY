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
// MENU_W must accommodate the widest row: "Close Tab" (9 chars ≈ 88 px)
// plus left-pad (10) plus right-pad (10) plus the hint "⇧⌃W" (3 glyphs;
// unicode glyphs are wider than ASCII — measured ~20 px at chrome advance
// 9.8 px/char × 1.6 scale ≈ ~22 px) plus gap between label and hint (~20 px).
// Target ≈ 10 + 88 + 20 + 22 + 10 = 150 … add margin → 210 px.
const MENU_W: f32 = 210.0;
const ROW_H: f32 = 28.0;
/// Extra vertical gap inserted between "Select All" (idx 2) and "Clear"
/// (idx 3) to house the separator line.
const SEP_GAP: f32 = 10.0;
/// Total clickable items count (5).
const N: usize = MENU_ITEMS.len();
/// Menu content height: N rows × ROW_H + one separator gap.
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

/// Row-top Y for item `i` in content-space (relative to `cy`), accounting for
/// the separator gap that sits between items 2 and 3.
#[inline]
fn row_y(i: usize) -> f32 {
    let gap = if i >= 3 { SEP_GAP } else { 0.0 };
    i as f32 * ROW_H + gap
}

/// Build the right-click context menu anchored at `(x, y)` (physical pixels).
///
/// The menu is clamped so its right and bottom edges stay within the window.
/// `hovered` is the index (0-based) of the item under the cursor, if any.
pub fn build_context_menu(
    x: f32,
    y: f32,
    win_w: u32,
    win_h: u32,
    hovered: Option<usize>,
    theme: &jetty_core::Theme,
) -> ContextMenu {
    let sw = win_w as f32;
    let sh = win_h as f32;

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
    let total_w = MENU_W + BORDER * 2.0;
    let total_h = MENU_H + BORDER * 2.0;
    let mx = x.min(sw - total_w).max(0.0);
    let my = y.min(sh - total_h).max(0.0);

    // Content area (inside the border).
    let cx = mx + BORDER;
    let cy = my + BORDER;

    // Build item rects (also serve as hit-test rects).
    // Each rect sits at its visual row position; the separator gap between
    // items 2 and 3 is dead space — not a hit rect.
    let mut item_rects: Vec<Rect> = Vec::with_capacity(N);
    for i in 0..N {
        item_rects.push(Rect {
            x: cx,
            y: cy + row_y(i),
            w: MENU_W,
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
    quads.push(Rect::rounded(cx, cy, MENU_W, MENU_H, menu_bg, 6.0));

    // Hover highlight quad (drawn on top of background, under labels). The top
    // and bottom rows get the bg's corner radius so the highlight doesn't square
    // off the rounded card corners; interior rows stay sharp.
    if let Some(idx) = hovered {
        if idx < N {
            let radius = if idx == 0 || idx == N - 1 { 6.0 } else { 0.0 };
            quads.push(Rect {
                x: cx,
                y: cy + row_y(idx),
                w: MENU_W,
                h: ROW_H,
                color: hover_col,
                radius,
            });
        }
    }

    // Separator line: a thin (1px) dim quad in the middle of the SEP_GAP,
    // inset by 10px on each side so it doesn't butt against the rounded corners.
    {
        let sep_y = cy + row_y(2) + ROW_H + (SEP_GAP - 1.0) * 0.5;
        quads.push(Rect {
            x: cx + 10.0,
            y: sep_y,
            w: MENU_W - 20.0,
            h: 1.0,
            color: sep_col,
            radius: 0.0,
        });
    }

    // Labels: item name (left-aligned) + shortcut hint (right-aligned, dim).
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    for (i, &name) in MENU_ITEMS.iter().enumerate() {
        let label_y = cy + row_y(i) + 7.0; // 7px from row top — matches original
        labels.push((name.to_string(), cx + 10.0, label_y, text_col));

        let hint = MENU_HINTS[i];
        if !hint.is_empty() {
            // Right-align: measure hint width at chrome advance ≈ 9.8 px/char.
            // Unicode chars (⇧ ⌃) may be wider; using 12 px/glyph for safety.
            let hint_w = hint.chars().count() as f32 * 12.0;
            let hint_x = cx + MENU_W - 10.0 - hint_w;
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

    #[test]
    fn exactly_five_item_rects() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme());
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
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme());
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
    fn hints_present_in_labels() {
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme());
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
        let menu = build_context_menu(100.0, 100.0, 1280, 800, None, &theme());
        let texts: Vec<&str> = menu.labels.iter().map(|(t, ..)| t.as_str()).collect();
        for item in MENU_ITEMS.iter() {
            assert!(texts.contains(item), "item {:?} missing from labels", item);
        }
    }

    #[test]
    fn menu_stays_on_screen_when_clamped() {
        // Anchor near bottom-right corner — menu must clamp entirely on-screen.
        let (win_w, win_h) = (800u32, 600u32);
        let menu = build_context_menu(790.0, 590.0, win_w, win_h, None, &theme());
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
        let menu = build_context_menu(50.0, 50.0, 1280, 800, None, &theme());
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
    fn hover_highlight_aligns_with_item_rects() {
        // For each of the 5 items, the hover quad y must match the item rect y.
        // Quad order: [0] border, [1] bg, [2] hover, [3] separator.
        for hovered in 0..5 {
            let menu = build_context_menu(50.0, 50.0, 1280, 800, Some(hovered), &theme());
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
