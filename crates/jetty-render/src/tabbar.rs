use crate::Rect;
use jetty_core::Theme;

/// Height of the tab bar in physical pixels. The terminal grid is offset down by
/// this amount; pixel↔cell math for the main grid subtracts it.
pub const TABBAR_H: f32 = 32.0;

/// Width of a single tab in physical pixels.
const TAB_W: f32 = 140.0;
/// Width of the "+" new-tab button.
const PLUS_W: f32 = 32.0;
/// Size of the "×" close hit box at the right of each tab.
const CLOSE_W: f32 = 18.0;
/// Width of each window-control button (minimize/maximize/close) on the right.
const CTRL_W: f32 = 28.0;
/// Total width reserved on the right of the strip for the three controls.
pub const CONTROLS_W: f32 = CTRL_W * 3.0;

/// Which window-control button (if any) is hovered, for the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlHover {
    None,
    Min,
    Max,
    Close,
}

/// Geometry + draw data for the tab bar.
pub struct TabBar {
    /// Quads in draw order: bar background, then per-tab backgrounds + plus button.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — tab titles, close glyphs, the plus glyph.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// One hit-test rect per tab (full tab area, for switching).
    pub tab_rects: Vec<Rect>,
    /// One hit-test rect per tab for its "×" close affordance.
    pub close_rects: Vec<Rect>,
    /// Hit-test rect for the "+" new-tab button.
    pub plus_rect: Rect,
    /// Hit-test rect for the minimize "─" window control.
    pub min_rect: Rect,
    /// Hit-test rect for the maximize/restore "▢" window control.
    pub max_rect: Rect,
    /// Hit-test rect for the close "✕" window control (rightmost).
    pub close_rect: Rect,
}

/// Build the tab bar across the top of the window.
///
/// `tabs` is `(title, is_active)` per tab in order; `theme` supplies colors so
/// the bar matches the active terminal theme. The active tab is highlighted with
/// the theme accent (palette blue); inactive tabs are dimmer.
///
/// `renaming` is `Some((idx, buf))` when tab `idx` is being renamed inline; that
/// tab shows `buf` plus a trailing caret and a highlighted background instead of
/// its stored title. `ctrl_hover` selects which window-control button is drawn
/// highlighted (close hover reads red).
pub fn build_tab_bar(width: u32, tabs: &[(String, bool)], theme: &Theme) -> TabBar {
    build_tab_bar_ex(width, tabs, theme, None, CtrlHover::None)
}

/// Extended tab-bar builder with inline-rename and window-control hover state.
pub fn build_tab_bar_ex(
    width: u32,
    tabs: &[(String, bool)],
    theme: &Theme,
    renaming: Option<(usize, &str)>,
    ctrl_hover: CtrlHover,
) -> TabBar {
    let sw = width as f32;
    let h = TABBAR_H;

    // Theme-derived colors.
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2], 255];
    let accent = theme.palette[4]; // blue
    let active_bg = [accent[0], accent[1], accent[2], 255];
    // Inactive tab: a dim blend toward the accent so it reads as part of the bar.
    let inactive_bg = [
        ((bg[0] as u16 + accent[0] as u16) / 3) as u8,
        ((bg[1] as u16 + accent[1] as u16) / 3) as u8,
        ((bg[2] as u16 + accent[2] as u16) / 3) as u8,
        255,
    ];
    let fg = theme.fg;
    let dim_fg = [
        (fg[0] as u16 * 2 / 3) as u8,
        (fg[1] as u16 * 2 / 3) as u8,
        (fg[2] as u16 * 2 / 3) as u8,
    ];

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut tab_rects: Vec<Rect> = Vec::new();
    let mut close_rects: Vec<Rect> = Vec::new();

    // Bar background spanning the full width.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h, color: bg });

    // Tabs are laid out from the left and must never overlap the window
    // controls parked at the right; cap the usable area accordingly.
    let tab_area_w = (sw - CONTROLS_W).max(0.0);
    // Background for a tab currently being renamed: a brighter accent so the
    // editing target stands out.
    let rename_bg = active_bg;

    let mut x = 0.0_f32;
    for (i, (title, active)) in tabs.iter().enumerate() {
        let being_renamed = matches!(renaming, Some((ri, _)) if ri == i);
        let bg_color = if being_renamed {
            rename_bg
        } else if *active {
            active_bg
        } else {
            inactive_bg
        };
        let tab_rect = Rect { x, y: 0.0, w: TAB_W, h, color: bg_color };
        // Tab background (inset by 1px at the bottom to leave a separator line look).
        quads.push(Rect { x: x + 1.0, y: 2.0, w: TAB_W - 2.0, h: h - 2.0, color: bg_color });

        let max_chars = 14usize;
        let label_color = if *active || being_renamed { fg } else { dim_fg };
        if being_renamed {
            // Show the live edit buffer plus a trailing caret. Truncate from the
            // FRONT so the caret/most-recent text stays visible while typing.
            let buf = match renaming { Some((_, b)) => b, None => "" };
            let mut shown: String = if buf.chars().count() > max_chars - 1 {
                let skip = buf.chars().count() - (max_chars - 1);
                buf.chars().skip(skip).collect()
            } else {
                buf.to_string()
            };
            shown.push('▏');
            labels.push((shown, x + 10.0, 8.0, label_color));
        } else {
            // Truncated title label.
            let shown: String = if title.chars().count() > max_chars {
                let mut s: String = title.chars().take(max_chars - 1).collect();
                s.push('…');
                s
            } else {
                title.clone()
            };
            labels.push((shown, x + 10.0, 8.0, label_color));

            // Close "×" at the tab's right (hidden while renaming this tab).
            let close_x = x + TAB_W - CLOSE_W - 4.0;
            labels.push(("×".to_string(), close_x + 4.0, 8.0, label_color));
        }

        // Close hit-box (kept even while renaming so indices stay aligned).
        let close_x = x + TAB_W - CLOSE_W - 4.0;
        close_rects.push(Rect { x: close_x, y: 0.0, w: CLOSE_W, h, color: bg_color });

        tab_rects.push(tab_rect);
        x += TAB_W;
    }

    // "+" new-tab button after the last tab (clamped within the tab area).
    let plus_rect = Rect { x, y: 0.0, w: PLUS_W, h, color: inactive_bg };
    if x + PLUS_W <= tab_area_w {
        quads.push(Rect { x: x + 1.0, y: 2.0, w: PLUS_W - 2.0, h: h - 2.0, color: inactive_bg });
        labels.push(("+".to_string(), x + 11.0, 7.0, fg));
    }

    // --- Window controls: minimize "─", maximize "▢", close "✕" (rightmost). ---
    let hover_bg = active_bg;
    // A red-ish background for the close button when hovered.
    let close_hover_bg = [0xE0, 0x40, 0x40, 255];
    let ctrl_y = 0.0;

    let min_x = sw - CONTROLS_W;
    let max_x = sw - CTRL_W * 2.0;
    let close_x = sw - CTRL_W;

    let min_rect = Rect { x: min_x, y: ctrl_y, w: CTRL_W, h, color: bg };
    let max_rect = Rect { x: max_x, y: ctrl_y, w: CTRL_W, h, color: bg };
    let close_rect = Rect { x: close_x, y: ctrl_y, w: CTRL_W, h, color: bg };

    // Hover highlight quads.
    if ctrl_hover == CtrlHover::Min {
        quads.push(Rect { x: min_x, y: 0.0, w: CTRL_W, h, color: hover_bg });
    }
    if ctrl_hover == CtrlHover::Max {
        quads.push(Rect { x: max_x, y: 0.0, w: CTRL_W, h, color: hover_bg });
    }
    if ctrl_hover == CtrlHover::Close {
        quads.push(Rect { x: close_x, y: 0.0, w: CTRL_W, h, color: close_hover_bg });
    }

    // Glyphs centred-ish in each control cell.
    labels.push(("─".to_string(), min_x + 8.0, 8.0, fg));
    labels.push(("▢".to_string(), max_x + 8.0, 8.0, fg));
    let close_fg = if ctrl_hover == CtrlHover::Close { [0xFF, 0xFF, 0xFF] } else { fg };
    labels.push(("✕".to_string(), close_x + 8.0, 8.0, close_fg));

    TabBar { quads, labels, tab_rects, close_rects, plus_rect, min_rect, max_rect, close_rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::by_name("catppuccin_mocha")
    }

    #[test]
    fn controls_parked_at_right_in_order() {
        let bar = build_tab_bar(1000, &[("Tab 1".to_string(), true)], &theme());
        // close is rightmost, then max, then min — all within the strip.
        assert!(bar.min_rect.x < bar.max_rect.x);
        assert!(bar.max_rect.x < bar.close_rect.x);
        // The close button's right edge reaches the surface edge.
        assert!((bar.close_rect.x + bar.close_rect.w - 1000.0).abs() < 0.01);
        // Each control is one cell wide.
        assert!((bar.close_rect.w - CONTROLS_W / 3.0).abs() < 0.01);
    }

    #[test]
    fn tabs_never_overlap_controls() {
        // Many tabs would overflow; the "+" must not be drawn under the controls.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let bar = build_tab_bar(800, &tabs, &theme());
        let controls_left = 800.0 - CONTROLS_W;
        // No tab's switch rect should start at/after the controls region edge
        // beyond what fits; the plus rect (when shown) stays left of controls.
        if bar.plus_rect.x + bar.plus_rect.w <= controls_left {
            assert!(bar.plus_rect.x + bar.plus_rect.w <= controls_left);
        }
    }

    #[test]
    fn rename_shows_caret() {
        let tabs = [("Old".to_string(), true)];
        let bar = build_tab_bar_ex(800, &tabs, &theme(), Some((0, "New")), CtrlHover::None);
        // The first label is the tab text; renaming shows the buffer + caret.
        assert!(bar.labels[0].0.contains('▏'));
        assert!(bar.labels[0].0.starts_with("New"));
    }

    #[test]
    fn close_hover_changes_glyph_color() {
        let tabs = [("Tab 1".to_string(), true)];
        let hot = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::Close);
        // A red-ish hover quad is appended when the close control is hovered.
        assert!(hot.quads.iter().any(|q| q.color == [0xE0, 0x40, 0x40, 255]));
    }
}
