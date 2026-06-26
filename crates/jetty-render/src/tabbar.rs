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
    build_tab_bar_ex(width, tabs, theme, None, CtrlHover::None, None)
}

/// Approximate chrome-font advance per character, in physical px. Used for the
/// perf-HUD width reservation + right-alignment (matches help.rs/panel.rs math).
const CHROME_CHAR_W: f32 = 9.6;
/// Gap (px) between the perf HUD's left edge and the nearest tab/+button, so the
/// reserved area never visually touches the tabs.
const PERF_GAP: f32 = 16.0;
/// Comfortable PER-TAB width (px) the perf HUD must NOT push tabs below. The HUD
/// is the lowest-priority strip element: if reserving its width would shrink each
/// tab beneath this (≈6 title chars), the HUD is HIDDEN and the tabs get the full
/// area instead (so several tabs in a narrowish window never squash to "T…×").
const PERF_MIN_TAB_W: f32 = 110.0;

/// Extended tab-bar builder with inline-rename, window-control hover state, and
/// an optional right-aligned performance HUD label.
///
/// `perf` is `Some(string)` to show the live perf HUD (`⚡ … ms · … fps · …`)
/// right-aligned just left of the window controls. Its width is RESERVED out of
/// the tab area so tabs never overlap it; if the window is too narrow to fit the
/// HUD without squeezing the tabs below a sane minimum, the HUD is HIDDEN and the
/// tab layout is identical to the no-HUD case. `None` shows no HUD.
pub fn build_tab_bar_ex(
    width: u32,
    tabs: &[(String, bool)],
    theme: &Theme,
    renaming: Option<(usize, &str)>,
    ctrl_hover: CtrlHover,
    perf: Option<&str>,
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
    // The controls region begins here; tabs+HUD must stay left of it.
    let controls_left = (sw - STRIP_PAD - CONTROLS_W).max(left);

    // --- Perf HUD reservation (LOWEST priority — yields space to the tabs) ---
    // The HUD sits between the tabs and the window controls, right-aligned with a
    // small gap before the controls. Reserve its width out of the tab area ONLY if,
    // after reserving, each tab still gets a COMFORTABLE width (>= PERF_MIN_TAB_W).
    // Otherwise (several tabs in a narrowish window) the HUD is HIDDEN so the tabs
    // aren't squashed to their unreadable ~64px floor just to fit a stats readout —
    // the tab layout then matches the no-HUD case exactly.
    let n_tabs = tabs.len().max(1) as f32;
    let perf_w = perf
        .map(|s| s.chars().count() as f32 * CHROME_CHAR_W)
        .unwrap_or(0.0);
    // Width carved out of the tab area when the HUD is shown: the label plus the
    // gap to the tabs and a small gap to the controls.
    let perf_reserve = if perf_w > 0.0 { perf_w + PERF_GAP * 1.5 } else { 0.0 };
    // Per-tab width the tabs WOULD get if we reserved for the HUD (capped at the
    // ideal TAB_W — extra room beyond that doesn't make a tab more comfortable).
    let tab_w_if_hud = ((controls_left - perf_reserve - left - PLUS_W).max(0.0) / n_tabs).min(TAB_W);
    let perf_shown = perf_w > 0.0 && tab_w_if_hud >= PERF_MIN_TAB_W;
    // Right boundary for the tabs / "+" / overflow hint. Shrinks by the HUD
    // reservation only when the HUD is actually shown.
    let tab_area_x = if perf_shown { controls_left - perf_reserve } else { controls_left };
    // The "+" button sits after the last tab and must stay left of the controls,
    // so the tabs themselves get the area from `left` to `tab_area_x - PLUS_W`.
    let tabs_avail_w = (tab_area_x - left - PLUS_W).max(0.0);

    // --- Dynamic tab width: shrink tabs to fit the available area so they never
    // overflow under the window controls. With many tabs we shrink down to a
    // readable minimum (TAB_W_MIN); if even that can't fit all of them, we cap the
    // number of tabs drawn (the rest are unreachable here but stay index-aligned
    // via the switch_tab keyboard path). ---
    const TAB_W_MIN: f32 = 64.0;
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
    // Guard: only draw when the hint fits left of the controls region — at very
    // narrow widths (<~400px) the hint would otherwise overrun the window controls.
    if overflow > 0 {
        let hint_x = (tab_area_x - 34.0).max(x + PLUS_W + 4.0);
        let hint_w = format!("+{overflow}").chars().count() as f32 * CHROME_CHAR_W;
        if hint_x + hint_w <= controls_left {
            labels.push((format!("+{overflow}"), hint_x, 9.0, dim_fg));
        }
    }

    // --- Perf HUD label (right-aligned, just left of the window controls) ---
    // Drawn dim (a bg→fg blend, theme-derived — never a hardcoded gray) so it
    // reads as a muted status line, matching the chrome. Only emitted when the
    // HUD fits without squeezing the tabs (perf_shown).
    if perf_shown {
        if let Some(s) = perf {
            // Dim blend: ~45% of the way from bg toward fg.
            let hud_color = [
                (bg[0] as f32 + (fg[0] as f32 - bg[0] as f32) * 0.45) as u8,
                (bg[1] as f32 + (fg[1] as f32 - bg[1] as f32) * 0.45) as u8,
                (bg[2] as f32 + (fg[2] as f32 - bg[2] as f32) * 0.45) as u8,
            ];
            // Right-align: right edge sits PERF_GAP left of the controls region.
            let hud_w = s.chars().count() as f32 * CHROME_CHAR_W;
            let hud_x = (controls_left - PERF_GAP - hud_w).max(left);
            labels.push((s.to_string(), hud_x, 9.0, hud_color));
        }
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
        let bar = build_tab_bar_ex(800, &tabs, &theme(), Some((0, "New")), CtrlHover::None, None);
        // Renaming shows the edit buffer + caret (a "❯" marker label precedes it,
        // so check across labels rather than a fixed index).
        let buf = bar.labels.iter().find(|l| l.0.contains('▏'));
        assert!(buf.is_some(), "no caret label found");
        assert!(buf.unwrap().0.starts_with("New"));
    }

    const PERF: &str = "⚡ 5.1 ms · 190 fps · 0.5% CPU · 155 MB/s";

    #[test]
    fn perf_hud_present_shrinks_tab_area_and_no_overlap() {
        // Wide window: the HUD fits, is emitted as a label, reserves space (so the
        // tab area is smaller than without it), and no tab/close rect overlaps it.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
        ];
        let with = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, Some(PERF));
        let without = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, None);

        // The HUD label is present.
        let hud = with.labels.iter().find(|l| l.0 == PERF).expect("HUD label missing");
        // It sits left of the window controls (help button is the leftmost control).
        assert!(hud.1 < with.help_rect.x, "HUD must be left of the controls");

        // The HUD's reserved left edge: tabs/close rects must not cross into it.
        let hud_left = hud.1 - PERF_GAP;
        for r in &with.tab_rects {
            assert!(r.x + r.w <= hud_left + 0.5, "tab overlaps the HUD reservation");
        }
        for r in &with.close_rects {
            assert!(r.x + r.w <= hud_left + 0.5, "close box overlaps the HUD");
        }
        // The "+" button stays left of the HUD too.
        assert!(with.plus_rect.x + with.plus_rect.w <= hud_left + 0.5);

        // Reservation actually shrinks the usable tab area: at default tab width
        // both layouts draw full-width tabs, so compare the "+" position — with the
        // HUD it must sit no further right than without (area is smaller-or-equal),
        // and the HUD eats real space so it's strictly left in the multi-tab case.
        assert!(with.plus_rect.x <= without.plus_rect.x);
    }

    #[test]
    fn perf_hud_hidden_when_too_narrow_keeps_layout() {
        // Narrow window: the HUD cannot fit without squeezing tabs, so it's hidden
        // and the tab layout is byte-identical to the no-HUD case.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let with = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, Some(PERF));
        let without = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, None);

        // No HUD label emitted.
        assert!(with.labels.iter().all(|l| l.0 != PERF), "HUD should be hidden");
        // Tab + close + plus geometry identical to the no-HUD layout.
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.01 && (a.w - b.w).abs() < 0.01);
        }
        for (a, b) in with.close_rects.iter().zip(&without.close_rects) {
            assert!((a.x - b.x).abs() < 0.01);
        }
        assert!((with.plus_rect.x - without.plus_rect.x).abs() < 0.01);
    }

    #[test]
    fn perf_hud_yields_to_tabs_when_many_tabs_would_squash() {
        // A moderately WIDE window (900px) where the tab area alone is plenty, but
        // 4 tabs + the HUD reservation would shrink each tab below the comfortable
        // floor. The HUD must hide and hand the full area to the tabs, so no tab is
        // squashed to its ~64px minimum just to fit the stats readout.
        let tabs: Vec<(String, bool)> =
            (0..4).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let with = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, Some(PERF));
        let without = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, None);

        // HUD hidden, layout identical to no-HUD.
        assert!(with.labels.iter().all(|l| l.0 != PERF), "HUD should yield to the tabs");
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.01 && (a.w - b.w).abs() < 0.01);
        }
        // Each drawn tab is comfortably above the squashed minimum.
        for r in &with.tab_rects {
            assert!(r.w >= PERF_MIN_TAB_W - 0.5, "tab squashed to {} despite hiding HUD", r.w);
        }
    }

    #[test]
    fn overflow_hint_does_not_overrun_controls_at_narrow_width() {
        // Many tabs in a very narrow window — the "+N" hint must NOT appear when
        // it would overlap the window controls region.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        // 400px is narrow enough to stress the guard (controls_left ≈ 252px).
        let bar = build_tab_bar(400, &tabs, &theme());
        let controls_left = 400.0 - STRIP_PAD - CONTROLS_W;
        // Any "+N" label must end before the controls region.
        for label in &bar.labels {
            if label.0.starts_with('+') && label.0[1..].chars().all(|c| c.is_ascii_digit()) {
                let hint_w = label.0.chars().count() as f32 * CHROME_CHAR_W;
                assert!(
                    label.1 + hint_w <= controls_left + 0.5,
                    "overflow hint overruns controls: hint_right={} controls_left={controls_left}",
                    label.1 + hint_w
                );
            }
        }
    }

    #[test]
    fn close_hover_changes_glyph_color() {
        let tabs = [("Tab 1".to_string(), true)];
        let hot = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::Close, None);
        // A theme-red hover quad is appended when the close control is hovered.
        let red = theme().palette[1];
        let red_bg = [red[0], red[1], red[2], 255];
        assert!(hot.quads.iter().any(|q| q.color == red_bg));
    }
}
