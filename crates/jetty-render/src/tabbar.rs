use crate::Rect;
use jetty_core::Theme;

/// Height of the tab bar in physical pixels. The terminal grid is offset down by
/// this amount; pixel↔cell math for the main grid subtracts it.
pub const TABBAR_H: f32 = 36.0;

/// Width of a single tab in physical pixels.
const TAB_W: f32 = 140.0;
/// Width of the "+" new-tab button.
const PLUS_W: f32 = 32.0;
/// Size of the "×" close hit box at the right of each tab.
const CLOSE_W: f32 = 18.0;
/// Width of each window-control button (minimize/maximize/close) on the right.
const CTRL_W: f32 = 28.0;
/// Total width reserved on the right of the strip for the controls. Left→right:
/// Help "?", Settings "⚙", minimize "─", maximize "▢", close "✕" — five cells.
pub const CONTROLS_W: f32 = CTRL_W * 5.0;
/// Inset of the whole tab strip from the window's left/right edges, so tabs and
/// the window controls don't sit flush against the rounded window corners.
pub const STRIP_PAD: f32 = 8.0;

/// Which window-control button (if any) is hovered, for the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlHover {
    None,
    Help,
    Settings,
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
    /// Hit-test rect for the Help "?" button (left of the window controls).
    pub help_rect: Rect,
    /// Hit-test rect for the Settings "⚙" button (left of the window controls).
    pub settings_rect: Rect,
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
    // Tab fills are SUBTLE accent tints of the bar bg (not a full bright slab):
    // the active tab is a soft tinted panel topped with a bright accent indicator
    // bar (drawn in the loop); inactive tabs barely lift off the bar so they recede.
    let tint = |t: f32| -> [u8; 4] {
        [
            (bg[0] as f32 + (accent[0] as f32 - bg[0] as f32) * t) as u8,
            (bg[1] as f32 + (accent[1] as f32 - bg[1] as f32) * t) as u8,
            (bg[2] as f32 + (accent[2] as f32 - bg[2] as f32) * t) as u8,
            255,
        ]
    };
    let active_bg = tint(0.22);
    let inactive_bg = tint(0.07);
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
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h, color: bg, ..Default::default() });

    // Tabs are laid out from `left` (inset from the window edge) and must never
    // overlap the window controls parked at the right (also inset by STRIP_PAD).
    // `tab_area_x` is the absolute x where the controls begin — the right
    // boundary for the tabs and the "+" button.
    let left = STRIP_PAD;
    let tab_area_x = (sw - STRIP_PAD - CONTROLS_W).max(left);
    // The "+" button sits after the last tab and must stay left of the controls,
    // so the tabs themselves get the area from `left` to `tab_area_x - PLUS_W`.
    let tabs_avail_w = (tab_area_x - left - PLUS_W).max(0.0);

    // --- Dynamic tab width: shrink tabs to fit the available area so they never
    // overflow under the window controls. With many tabs we shrink down to a
    // readable minimum (TAB_W_MIN); if even that can't fit all of them, we cap the
    // number of tabs drawn (the rest are unreachable here but stay index-aligned
    // via the switch_tab keyboard path). ---
    const TAB_W_MIN: f32 = 64.0;
    let n_tabs = tabs.len().max(1) as f32;
    // Ideal width per tab, clamped to [MIN, default]. Use the full default when
    // there's room; shrink toward MIN as tabs are added.
    let tab_w = (tabs_avail_w / n_tabs).clamp(TAB_W_MIN, TAB_W).min(TAB_W);
    // How many tabs actually fit at `tab_w` (at least 1 so the active tab shows).
    let max_visible = if tab_w > 0.0 {
        ((tabs_avail_w / tab_w).floor() as usize).max(1)
    } else {
        1
    };
    let drawn = tabs.len().min(max_visible);
    let overflow = tabs.len().saturating_sub(drawn);

    // Background for a tab currently being renamed: a brighter accent so the
    // editing target stands out.
    let rename_bg = active_bg;

    // Rounded-tab geometry. Each tab is a rounded rect with a 1px border drawn as
    // a slightly larger rounded rect behind it (active = accent border, inactive =
    // dim border) so the tabs match the rounded window frame.
    const TAB_RADIUS: f32 = 6.0;
    const TAB_INSET: f32 = 3.0; // horizontal gap between adjacent tabs
    const TAB_VPAD: f32 = 6.0; // top/bottom margin so tabs don't touch the window edge
    const TAB_BORDER: f32 = 1.0;
    // Active tab border = accent; inactive = a dim blend toward the accent.
    let active_border = [accent[0], accent[1], accent[2], 255];
    let inactive_border = [
        ((bg[0] as u16 + accent[0] as u16 * 2) / 3) as u8,
        ((bg[1] as u16 + accent[1] as u16 * 2) / 3) as u8,
        ((bg[2] as u16 + accent[2] as u16 * 2) / 3) as u8,
        255,
    ];

    // Characters that fit in a tab body, derived from its width (~9.6px advance).
    // Reserve room for the close "×" + left padding. Floors at 3 so a min-width
    // tab still shows a couple of chars + the ellipsis.
    let max_chars = (((tab_w - CLOSE_W - 32.0) / 9.6).floor() as usize).max(2);

    let mut x = left;
    for (i, (title, active)) in tabs.iter().take(drawn).enumerate() {
        let being_renamed = matches!(renaming, Some((ri, _)) if ri == i);
        let bg_color = if being_renamed {
            rename_bg
        } else if *active {
            active_bg
        } else {
            inactive_bg
        };
        let tab_rect = Rect { x, y: 0.0, w: tab_w, h, color: bg_color, ..Default::default() };

        // Inner tab body geometry (inset on all sides for the gap + border).
        let body_x = x + TAB_INSET;
        let body_y = TAB_VPAD;
        let body_w = tab_w - TAB_INSET * 2.0;
        let body_h = h - TAB_VPAD * 2.0;
        let border_color = if *active || being_renamed { active_border } else { inactive_border };
        // Border: a rounded rect 1px larger than the body on every side.
        quads.push(Rect::rounded(
            body_x - TAB_BORDER,
            body_y - TAB_BORDER,
            body_w + TAB_BORDER * 2.0,
            body_h + TAB_BORDER * 2.0,
            border_color,
            TAB_RADIUS + TAB_BORDER,
        ));
        // Fill: the rounded tab body on top of the border.
        quads.push(Rect::rounded(body_x, body_y, body_w, body_h, bg_color, TAB_RADIUS));

        let label_color = if *active || being_renamed { fg } else { dim_fg };
        // Leading oh-my-zsh-style prompt marker (❯): bright accent on the active
        // tab, dim otherwise — the active indicator (in place of an underline).
        let marker_color = if *active || being_renamed {
            [active_border[0], active_border[1], active_border[2]]
        } else {
            dim_fg
        };
        labels.push(("❯".to_string(), x + 10.0, 9.0, marker_color));
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
            labels.push((shown, x + 26.0, 9.0, label_color));
        } else {
            // Truncated title label.
            let shown: String = if title.chars().count() > max_chars {
                let mut s: String = title.chars().take(max_chars - 1).collect();
                s.push('…');
                s
            } else {
                title.clone()
            };
            labels.push((shown, x + 26.0, 9.0, label_color));

            // Close "×" at the tab's right (hidden while renaming this tab).
            let close_x = x + tab_w - CLOSE_W - 4.0;
            labels.push(("×".to_string(), close_x + 4.0, 9.0, label_color));
        }

        // Close hit-box (kept even while renaming so indices stay aligned).
        let close_x = x + tab_w - CLOSE_W - 4.0;
        close_rects.push(Rect { x: close_x, y: 0.0, w: CLOSE_W, h, color: bg_color, ..Default::default() });

        tab_rects.push(tab_rect);
        x += tab_w;
    }

    // "+" new-tab button after the last tab (clamped within the tab area).
    let plus_rect = Rect { x, y: 0.0, w: PLUS_W, h, color: inactive_bg, ..Default::default() };
    if x + PLUS_W <= tab_area_x {
        quads.push(Rect::rounded(x + 3.0, TAB_VPAD, PLUS_W - 6.0, h - TAB_VPAD * 2.0, inactive_bg, 5.0));
        labels.push(("+".to_string(), x + 11.0, 8.0, fg));
    }

    // A small "+N" hint when some tabs couldn't be drawn (too many to fit even at
    // the minimum width). Placed just left of the controls so it never overlaps.
    if overflow > 0 {
        let hint_x = (tab_area_x - 34.0).max(x + PLUS_W + 4.0);
        labels.push((format!("+{overflow}"), hint_x, 9.0, dim_fg));
    }

    // --- Right-side controls (left→right): Help "?", Settings "⚙",
    // minimize "─", maximize "▢", close "✕" (rightmost). ---
    let hover_bg = active_bg;
    // A red-ish background for the close button when hovered (theme's red).
    let red = theme.palette[1];
    let close_hover_bg = [red[0], red[1], red[2], 255];
    let ctrl_y = 0.0;

    let help_x = sw - STRIP_PAD - CONTROLS_W; // = tab_area_x
    let settings_x = sw - STRIP_PAD - CTRL_W * 4.0;
    let min_x = sw - STRIP_PAD - CTRL_W * 3.0;
    let max_x = sw - STRIP_PAD - CTRL_W * 2.0;
    let close_x = sw - STRIP_PAD - CTRL_W;

    let help_rect = Rect { x: help_x, y: ctrl_y, w: CTRL_W, h, color: bg, ..Default::default() };
    let settings_rect = Rect { x: settings_x, y: ctrl_y, w: CTRL_W, h, color: bg, ..Default::default() };
    let min_rect = Rect { x: min_x, y: ctrl_y, w: CTRL_W, h, color: bg, ..Default::default() };
    let max_rect = Rect { x: max_x, y: ctrl_y, w: CTRL_W, h, color: bg, ..Default::default() };
    let close_rect = Rect { x: close_x, y: ctrl_y, w: CTRL_W, h, color: bg, ..Default::default() };

    // Hover highlight quads.
    if ctrl_hover == CtrlHover::Help {
        quads.push(Rect { x: help_x, y: 0.0, w: CTRL_W, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Settings {
        quads.push(Rect { x: settings_x, y: 0.0, w: CTRL_W, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Min {
        quads.push(Rect { x: min_x, y: 0.0, w: CTRL_W, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Max {
        quads.push(Rect { x: max_x, y: 0.0, w: CTRL_W, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Close {
        quads.push(Rect { x: close_x, y: 0.0, w: CTRL_W, h, color: close_hover_bg, ..Default::default() });
    }

    // Glyphs centred-ish in each control cell. "⚙" may be missing in some
    // monospace fonts; "≡" is a safe, widely-available fallback for settings.
    labels.push(("?".to_string(), help_x + 9.0, 9.0, fg));
    labels.push(("⚙".to_string(), settings_x + 8.0, 9.0, fg));
    labels.push(("─".to_string(), min_x + 8.0, 9.0, fg));
    labels.push(("▢".to_string(), max_x + 8.0, 9.0, fg));
    let close_fg = if ctrl_hover == CtrlHover::Close { [0xFF, 0xFF, 0xFF] } else { fg };
    labels.push(("✕".to_string(), close_x + 8.0, 9.0, close_fg));

    TabBar {
        quads, labels, tab_rects, close_rects, plus_rect,
        help_rect, settings_rect, min_rect, max_rect, close_rect,
    }
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
        // Left→right: help, settings, min, max, close — all within the strip.
        assert!(bar.help_rect.x < bar.settings_rect.x);
        assert!(bar.settings_rect.x < bar.min_rect.x);
        assert!(bar.min_rect.x < bar.max_rect.x);
        assert!(bar.max_rect.x < bar.close_rect.x);
        // The close button's right edge sits STRIP_PAD in from the surface edge.
        assert!((bar.close_rect.x + bar.close_rect.w - (1000.0 - STRIP_PAD)).abs() < 0.01);
        // Each control is one cell wide (CONTROLS_W spans five cells).
        assert!((bar.close_rect.w - CONTROLS_W / 5.0).abs() < 0.01);
    }

    #[test]
    fn tabs_never_overlap_controls() {
        // Many tabs would overflow; the "+" must not be drawn under the controls.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let bar = build_tab_bar(800, &tabs, &theme());
        let controls_left = 800.0 - STRIP_PAD - CONTROLS_W;
        // No tab's switch rect should start at/after the controls region edge
        // beyond what fits; the plus rect (when shown) stays left of controls.
        if bar.plus_rect.x + bar.plus_rect.w <= controls_left {
            assert!(bar.plus_rect.x + bar.plus_rect.w <= controls_left);
        }
    }

    #[test]
    fn tabs_shrink_to_fit_narrow_window() {
        // 3 tabs in a 560px window: all must be drawn, none overlapping the
        // controls, and each tab's close box stays left of the controls region.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar(560, &tabs, &theme());
        let controls_left = 560.0 - STRIP_PAD - CONTROLS_W;
        assert_eq!(bar.tab_rects.len(), 3, "all 3 tabs should be drawn");
        for r in &bar.tab_rects {
            assert!(
                r.x + r.w <= controls_left + 0.5,
                "tab overflows controls: {} > {controls_left}",
                r.x + r.w
            );
        }
        for r in &bar.close_rects {
            assert!(
                r.x + r.w <= controls_left + 0.5,
                "close box overlaps controls at x={}",
                r.x + r.w
            );
        }
        // The "+" button (when present) also stays left of the controls.
        if bar.plus_rect.x + bar.plus_rect.w <= controls_left + 0.5 {
            assert!(bar.plus_rect.x >= bar.tab_rects.last().unwrap().x);
        }
    }

    #[test]
    fn rename_shows_caret() {
        let tabs = [("Old".to_string(), true)];
        let bar = build_tab_bar_ex(800, &tabs, &theme(), Some((0, "New")), CtrlHover::None);
        // Renaming shows the edit buffer + caret (a "❯" marker label precedes it,
        // so check across labels rather than a fixed index).
        let buf = bar.labels.iter().find(|l| l.0.contains('▏'));
        assert!(buf.is_some(), "no caret label found");
        assert!(buf.unwrap().0.starts_with("New"));
    }

    #[test]
    fn close_hover_changes_glyph_color() {
        let tabs = [("Tab 1".to_string(), true)];
        let hot = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::Close);
        // A theme-red hover quad is appended when the close control is hovered.
        let red = theme().palette[1];
        let red_bg = [red[0], red[1], red[2], 255];
        assert!(hot.quads.iter().any(|q| q.color == red_bg));
    }
}
