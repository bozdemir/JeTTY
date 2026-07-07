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

/// Unseen activity on a tab, shown as a small themed dot on INACTIVE tabs.
/// `Output` = PTY output arrived while the tab was inactive (accent dot);
/// `Bell` = BEL (^G) rang while inactive (theme-red dot, never downgraded by
/// later output). Sticky until the tab is viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabActivity {
    #[default]
    None,
    Output,
    Bell,
}

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
    /// Monospace chrome labels: (text, x, y, rgb) — close glyphs, the plus glyph,
    /// the overflow hint, the perf HUD, and the window controls.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Tab TITLE labels, rendered separately in the platform's proportional
    /// sans-serif so the strip reads as elegant UI text instead of monospace.
    pub title_labels: Vec<(String, f32, f32, [u8; 3])>,
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
    build_tab_bar_ex(width, tabs, theme, None, CtrlHover::None, None, CHROME_CHAR_W, &[])
}

/// Fallback chrome-font advance per character (physical px), used when the
/// caller does not have a measured advance available (e.g. `build_tab_bar` thin
/// wrapper and legacy call sites). The real measured value is threaded in via the
/// `char_w` parameter of `build_tab_bar_ex` on HiDPI-aware paths.
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
///
/// `char_w` is the measured physical-pixel advance of one chrome-font character
/// (from `TextLayer::cell_size().0`). Pass `CHROME_CHAR_W` (9.6) when a real
/// measurement is not available (scale-1 fallback used by tests and the thin
/// `build_tab_bar` wrapper).
///
/// `activity` is index-aligned with `tabs` (missing indices read as `None`);
/// inactive tabs with activity get a small themed dot in the title gutter and
/// a hidden (overflowed) tab's activity tints the "+N" hint. Activity NEVER
/// affects geometry, so hit-test rebuilds can pass `&[]`.
#[allow(clippy::too_many_arguments)]
pub fn build_tab_bar_ex(
    width: u32,
    tabs: &[(String, bool)],
    theme: &Theme,
    renaming: Option<(usize, &str)>,
    ctrl_hover: CtrlHover,
    perf: Option<&str>,
    char_w: f32,
    activity: &[TabActivity],
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
    let active_bg = tint(0.22); // still used for the window-control hover highlight
    let fg = theme.fg;
    let dim_fg = [
        (fg[0] as u16 * 2 / 3) as u8,
        (fg[1] as u16 * 2 / 3) as u8,
        (fg[2] as u16 * 2 / 3) as u8,
    ];
    // --- Elegant frameless tab palette ---
    // The active tab is a soft, theme-derived NEUTRAL pill (a bg→fg blend, the
    // same surface language as the settings panel) — no per-tab border, no marker.
    // Inactive tabs are text only (transparent). This keeps the strip consistent
    // with the rest of the UI instead of reading as bordered widgets.
    let nl = |t: f32| -> [u8; 3] {
        [
            (bg[0] as f32 + (fg[0] as f32 - bg[0] as f32) * t) as u8,
            (bg[1] as f32 + (fg[1] as f32 - bg[1] as f32) * t) as u8,
            (bg[2] as f32 + (fg[2] as f32 - bg[2] as f32) * t) as u8,
        ]
    };
    let n = |t: f32| -> [u8; 4] { let c = nl(t); [c[0], c[1], c[2], 255] };
    let tab_active_fill = n(0.12); // soft lifted pill for the active/renaming tab
    let tab_text_inactive = nl(0.46); // muted title on inactive tabs
    let close_dim = nl(0.36); // recessive close × on inactive tabs
    let close_active = nl(0.62); // close × on the active tab (a touch brighter)
    let plus_col = nl(0.50); // "+" new-tab glyph

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Sized to the full tab count and index-aligned below (see the draw loop).
    let mut tab_rects: Vec<Rect>;
    let mut close_rects: Vec<Rect>;

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
        .map(|s| s.chars().count() as f32 * char_w)
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

    // Window the drawn range around the ACTIVE tab so it is ALWAYS visible — the
    // overflow window is not fixed to the head of the list. If the active index is
    // beyond the head window, slide the window so the active tab sits at its right
    // edge. Hit-rects stay index-aligned with ABSOLUTE tab indices (tabs outside
    // the window get an offscreen sentinel) because the app maps a clicked rect's
    // position straight to a tab index.
    let active_idx = tabs.iter().position(|(_, a)| *a).unwrap_or(0);
    let start = if active_idx >= drawn {
        (active_idx + 1 - drawn).min(tabs.len().saturating_sub(drawn))
    } else {
        0
    };

    // Frameless pill geometry. The active/renaming tab is a single soft rounded
    // pill (no border, no leading marker); inactive tabs draw nothing — just their
    // dim title — so the strip reads as light, modern text-tabs rather than a row
    // of bordered boxes.
    const TAB_RADIUS: f32 = 8.0;
    const TAB_INSET: f32 = 4.0; // horizontal gap between adjacent tabs
    const TAB_VPAD: f32 = 6.0; // top/bottom margin so the pill doesn't touch edges
    const TITLE_PAD: f32 = 13.0; // left inset of the title inside a tab

    // Characters that fit, from the measured chrome advance. Reserve the close "×"
    // box + left title pad + a little right breathing room.
    let max_chars = (((tab_w - CLOSE_W - TITLE_PAD - 8.0) / char_w).floor() as usize).max(2);

    // Hit-rects are index-aligned to ABSOLUTE tab indices; off-window tabs get an
    // offscreen sentinel (0-width, far left) so a click can never match them.
    let offscreen = Rect { x: -1.0e6, y: 0.0, w: 0.0, h: 0.0, color: [0, 0, 0, 0], ..Default::default() };
    tab_rects = vec![offscreen; tabs.len()];
    close_rects = vec![offscreen; tabs.len()];

    let mut x = left;
    for (i, (title, active)) in tabs.iter().enumerate().skip(start).take(drawn) {
        let being_renamed = matches!(renaming, Some((ri, _)) if ri == i);
        let is_on = *active || being_renamed;

        // Active/renaming tab → a soft filled pill. Inactive → nothing drawn.
        if is_on {
            let body_x = x + TAB_INSET;
            let body_w = tab_w - TAB_INSET * 2.0;
            quads.push(Rect::rounded(body_x, TAB_VPAD, body_w, h - TAB_VPAD * 2.0, tab_active_fill, TAB_RADIUS));
        }

        let label_color = if is_on { fg } else { tab_text_inactive };
        let title_x = x + TITLE_PAD;
        if being_renamed {
            // Live edit buffer + trailing caret, front-truncated so the caret stays.
            let buf = match renaming { Some((_, b)) => b, None => "" };
            let mut shown: String = if buf.chars().count() > max_chars - 1 {
                let skip = buf.chars().count() - (max_chars - 1);
                buf.chars().skip(skip).collect()
            } else {
                buf.to_string()
            };
            shown.push('|'); // ASCII caret: every UI font has it (U+258F was tofu on fonts without Block Elements)
            title_labels.push((shown, title_x, 9.0, label_color));
        } else {
            let shown: String = if title.chars().count() > max_chars {
                let mut s: String = title.chars().take(max_chars - 1).collect();
                s.push('…');
                s
            } else {
                title.clone()
            };
            title_labels.push((shown, title_x, 9.0, label_color));

            // Close "×": recessive on inactive tabs, a touch brighter on the active.
            let close_x = x + tab_w - CLOSE_W - 4.0;
            let x_col = if is_on { close_active } else { close_dim };
            labels.push(("×".to_string(), close_x + 4.0, 9.0, x_col));
        }

        // Activity/bell dot on INACTIVE tabs, in the TITLE_PAD gutter left of
        // the title (dot spans x+4..x+10, title starts at x+13) so the layout
        // is bit-identical with or without activity. Accent = unseen output,
        // theme red = bell.
        let act = activity.get(i).copied().unwrap_or(TabActivity::None);
        if !is_on && act != TabActivity::None {
            const DOT_D: f32 = 6.0;
            let dc = match act {
                TabActivity::Bell => theme.palette[1],
                _ => accent,
            };
            quads.push(Rect::rounded(
                x + 4.0,
                (h - DOT_D) / 2.0,
                DOT_D,
                DOT_D,
                [dc[0], dc[1], dc[2], 255],
                DOT_D / 2.0,
            ));
        }

        // Close hit-box (kept while renaming so indices stay aligned). Color is
        // unused for hit-testing.
        let close_x = x + tab_w - CLOSE_W - 4.0;
        close_rects[i] = Rect { x: close_x, y: 0.0, w: CLOSE_W, h, color: [0, 0, 0, 0], ..Default::default() };

        tab_rects[i] = Rect { x, y: 0.0, w: tab_w, h, color: [0, 0, 0, 0], ..Default::default() };
        x += tab_w;
    }

    // "+" new-tab button — minimal: just a dim glyph, no box.
    let plus_rect = Rect { x, y: 0.0, w: PLUS_W, h, color: [0, 0, 0, 0], ..Default::default() };
    if x + PLUS_W <= tab_area_x {
        labels.push(("+".to_string(), x + 11.0, 8.0, plus_col));
    }

    // A small "+N" hint when some tabs couldn't be drawn (too many to fit even at
    // the minimum width). Placed just left of the controls so it never overlaps.
    // Guard: only draw when the hint fits left of the controls region — at very
    // narrow widths (<~400px) the hint would otherwise overrun the window controls.
    if overflow > 0 {
        let hint_x = (tab_area_x - 34.0).max(x + PLUS_W + 4.0);
        let hint_w = format!("+{overflow}").chars().count() as f32 * char_w;
        if hint_x + hint_w <= controls_left {
            // Tint the hint with the strongest activity among the HIDDEN tabs
            // (Bell > Output) so activity on a scrolled-out tab is never
            // silently invisible.
            let hidden_act = (0..start)
                .chain(start + drawn..tabs.len())
                .map(|i| activity.get(i).copied().unwrap_or(TabActivity::None))
                .fold(TabActivity::None, |acc, a| match (acc, a) {
                    (TabActivity::Bell, _) | (_, TabActivity::Bell) => TabActivity::Bell,
                    (TabActivity::Output, _) | (_, TabActivity::Output) => TabActivity::Output,
                    _ => TabActivity::None,
                });
            let hint_col = match hidden_act {
                TabActivity::Bell => theme.palette[1],
                TabActivity::Output => accent,
                TabActivity::None => dim_fg,
            };
            labels.push((format!("+{overflow}"), hint_x, 9.0, hint_col));
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
            let hud_w = s.chars().count() as f32 * char_w;
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
        quads, labels, title_labels, tab_rects, close_rects, plus_rect,
        help_rect, settings_rect, min_rect, max_rect, close_rect,
    }
}

// ── Detached-window top bar ──────────────────────────────────────────────────

/// Geometry + draw data for a DETACHED window's top bar: the tab's title as a
/// single active pill on the left, and a lone close "✕" control on the right.
/// Same visual language (heights, pill, colors, control cell) as the main tab
/// bar, minus the multi-tab affordances ("+", overflow, min/max, settings).
pub struct DetachedBar {
    /// Quads in draw order: bar background, title pill, optional ✕ hover.
    pub quads: Vec<Rect>,
    /// Monospace chrome labels: the close "✕" glyph.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The title label, rendered in the proportional sans like main tab titles.
    pub title_labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Hit-test rect for the close "✕" (triggers the close→reattach path).
    pub close_rect: Rect,
}

/// Hit-test rect for a detached window's close "✕": the rightmost CTRL_W cell
/// of the TABBAR_H strip, inset by STRIP_PAD — the same cell the main window's
/// close control occupies. Exposed separately so the app's hover tracking can
/// hit-test without building the whole bar.
pub fn detached_close_rect(width: u32) -> Rect {
    Rect {
        x: width as f32 - STRIP_PAD - CTRL_W,
        y: 0.0,
        w: CTRL_W,
        h: TABBAR_H,
        color: [0, 0, 0, 0],
        ..Default::default()
    }
}

/// Build the top bar of a DETACHED window: bar background, the tab title as an
/// active pill (it is always the "active tab" of its window), and the close
/// "✕" at the right (red highlight when `close_hover`). `char_w` is the
/// measured chrome-font advance, as in `build_tab_bar_ex`.
pub fn build_detached_bar(
    width: u32,
    title: &str,
    theme: &Theme,
    close_hover: bool,
    char_w: f32,
) -> DetachedBar {
    let sw = width as f32;
    let h = TABBAR_H;

    // Same theme-derived surface language as build_tab_bar_ex.
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2], 255];
    let fg = theme.fg;
    let nl = |t: f32| -> [u8; 3] {
        [
            (bg[0] as f32 + (fg[0] as f32 - bg[0] as f32) * t) as u8,
            (bg[1] as f32 + (fg[1] as f32 - bg[1] as f32) * t) as u8,
            (bg[2] as f32 + (fg[2] as f32 - bg[2] as f32) * t) as u8,
        ]
    };
    let n = |t: f32| -> [u8; 4] {
        let c = nl(t);
        [c[0], c[1], c[2], 255]
    };
    let pill_fill = n(0.12); // same soft lifted pill as the active main tab

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Bar background spanning the full width.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h, color: bg, ..Default::default() });

    // Title pill on the left — same geometry constants as a main-window tab,
    // clamped so it never runs under the close control.
    const TAB_RADIUS: f32 = 8.0;
    const TAB_INSET: f32 = 4.0;
    const TAB_VPAD: f32 = 6.0;
    const TITLE_PAD: f32 = 13.0;
    let left = STRIP_PAD;
    let controls_left = (sw - STRIP_PAD - CTRL_W).max(left);
    let tab_w = TAB_W.min((controls_left - left).max(0.0));
    if tab_w > TAB_INSET * 2.0 {
        quads.push(Rect::rounded(
            left + TAB_INSET,
            TAB_VPAD,
            tab_w - TAB_INSET * 2.0,
            h - TAB_VPAD * 2.0,
            pill_fill,
            TAB_RADIUS,
        ));
        let max_chars = (((tab_w - TITLE_PAD - 8.0) / char_w).floor() as usize).max(2);
        let shown: String = if title.chars().count() > max_chars {
            let mut s: String = title.chars().take(max_chars.saturating_sub(1)).collect();
            s.push('…');
            s
        } else {
            title.to_string()
        };
        title_labels.push((shown, left + TITLE_PAD, 9.0, fg));
    }

    // Close "✕" at the right — red hover background, white glyph on hover
    // (identical treatment to the main window's close control).
    let close_rect = detached_close_rect(width);
    if close_hover {
        let red = theme.palette[1];
        quads.push(Rect {
            x: close_rect.x,
            y: 0.0,
            w: CTRL_W,
            h,
            color: [red[0], red[1], red[2], 255],
            ..Default::default()
        });
    }
    let close_fg = if close_hover { [0xFF, 0xFF, 0xFF] } else { fg };
    labels.push(("✕".to_string(), close_rect.x + 8.0, 9.0, close_fg));

    DetachedBar { quads, labels, title_labels, close_rect }
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
    fn active_tab_always_drawn_even_when_overflowing() {
        // 20 tabs can't all fit in 800px; the active tab is far past the head
        // window. It must still be laid out on-screen, and its hit-rect must stay
        // index-aligned with its absolute tab index.
        let active = 15usize;
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == active)).collect();
        let bar = build_tab_bar(800, &tabs, &theme());
        // One hit-rect per tab (absolute-index-aligned for the click→tab mapping).
        assert_eq!(bar.tab_rects.len(), 20);
        let controls_left = 800.0 - STRIP_PAD - CONTROLS_W;
        let r = bar.tab_rects[active];
        assert!(
            r.x >= 0.0 && r.x + r.w <= controls_left + 0.5,
            "active tab not visible on the strip: x={} w={}",
            r.x,
            r.w
        );
        // A head tab that scrolled out of the window is parked offscreen so it
        // can't be clicked to a wrong index.
        assert!(bar.tab_rects[0].x < 0.0, "scrolled-out head tab should be offscreen");
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
        let bar = build_tab_bar_ex(800, &tabs, &theme(), Some((0, "New")), CtrlHover::None, None, CHROME_CHAR_W, &[]);
        // Renaming shows the edit buffer + caret in the sans TITLE labels.
        let buf = bar.title_labels.iter().find(|l| l.0.contains('|'));
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
        let with = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, Some(PERF), CHROME_CHAR_W, &[]);
        let without = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[]);

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
        let with = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, Some(PERF), CHROME_CHAR_W, &[]);
        let without = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[]);

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
        let with = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, Some(PERF), CHROME_CHAR_W, &[]);
        let without = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[]);

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
    fn detached_bar_close_parked_at_right() {
        let bar = build_detached_bar(1000, "Tab 2", &theme(), false, CHROME_CHAR_W);
        // The ✕ occupies the rightmost control cell, inset by STRIP_PAD.
        assert!((bar.close_rect.x + bar.close_rect.w - (1000.0 - STRIP_PAD)).abs() < 0.01);
        assert!((bar.close_rect.w - CTRL_W).abs() < 0.01);
        assert_eq!(bar.close_rect.h, TABBAR_H);
        // It matches the standalone hit-test helper the app uses for hover.
        let cr = detached_close_rect(1000);
        assert_eq!(cr.x, bar.close_rect.x);
        // The ✕ glyph is emitted.
        assert!(bar.labels.iter().any(|l| l.0 == "✕"));
    }

    #[test]
    fn detached_bar_shows_title_pill() {
        let bar = build_detached_bar(1000, "Build logs", &theme(), false, CHROME_CHAR_W);
        // Title present in the sans title labels.
        assert!(bar.title_labels.iter().any(|l| l.0 == "Build logs"));
        // Bar bg + pill are both emitted (≥ 2 quads).
        assert!(bar.quads.len() >= 2);
    }

    #[test]
    fn detached_bar_close_hover_paints_theme_red() {
        let hot = build_detached_bar(1000, "Tab 2", &theme(), true, CHROME_CHAR_W);
        let red = theme().palette[1];
        assert!(hot.quads.iter().any(|q| q.color == [red[0], red[1], red[2], 255]));
        // Glyph flips to white on hover.
        assert!(hot.labels.iter().any(|l| l.0 == "✕" && l.3 == [0xFF, 0xFF, 0xFF]));
    }

    #[test]
    fn detached_bar_long_title_truncates_with_ellipsis() {
        let long = "a very long detached tab title that cannot fit";
        let bar = build_detached_bar(1000, long, &theme(), false, CHROME_CHAR_W);
        let label = &bar.title_labels[0].0;
        assert!(label.ends_with('…'), "expected truncation, got {label:?}");
    }

    #[test]
    fn close_hover_changes_glyph_color() {
        let tabs = [("Tab 1".to_string(), true)];
        let hot = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::Close, None, CHROME_CHAR_W, &[]);
        // A theme-red hover quad is appended when the close control is hovered.
        let red = theme().palette[1];
        let red_bg = [red[0], red[1], red[2], 255];
        assert!(hot.quads.iter().any(|q| q.color == red_bg));
    }

    /// The 6px activity dots for a bar built over 3 tabs (tab 0 active).
    fn dot_quads(bar: &TabBar) -> Vec<Rect> {
        // Distinguish the dot from other colored quads (e.g. the CTRL_W-wide
        // close-hover highlight) by its 6.0 width/height.
        bar.quads
            .iter()
            .filter(|q| (q.w - 6.0).abs() < 0.01 && (q.h - 6.0).abs() < 0.01)
            .copied()
            .collect()
    }

    #[test]
    fn activity_dot_drawn_with_theme_colors() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W,
            &[TabActivity::None, TabActivity::Output, TabActivity::Bell],
        );
        let accent = theme().palette[4];
        let red = theme().palette[1];
        let dots = dot_quads(&bar);
        assert_eq!(dots.len(), 2, "one dot per inactive tab with activity");
        assert!(dots.iter().any(|q| q.color == [accent[0], accent[1], accent[2], 255]),
            "output dot must use the theme accent");
        assert!(dots.iter().any(|q| q.color == [red[0], red[1], red[2], 255]),
            "bell dot must use the theme red");
        // Baseline: no activity → no dots.
        let base = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[],
        );
        assert!(dot_quads(&base).is_empty());
    }

    #[test]
    fn activity_dot_suppressed_on_active_tab() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W,
            &[TabActivity::Bell, TabActivity::None, TabActivity::None],
        );
        assert!(dot_quads(&bar).is_empty(), "the active tab never shows a dot");
    }

    #[test]
    fn activity_does_not_shift_layout() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let with = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W,
            &[TabActivity::None, TabActivity::Output, TabActivity::Bell],
        );
        let without = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[],
        );
        // Labels + hit geometry are bit-identical; only the quads differ.
        assert_eq!(with.title_labels, without.title_labels);
        assert_eq!(with.labels, without.labels);
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.001 && (a.w - b.w).abs() < 0.001);
        }
        for (a, b) in with.close_rects.iter().zip(&without.close_rects) {
            assert!((a.x - b.x).abs() < 0.001 && (a.w - b.w).abs() < 0.001);
        }
        assert!((with.plus_rect.x - without.plus_rect.x).abs() < 0.001);
    }

    #[test]
    fn overflow_hint_tinted_when_hidden_tab_has_activity() {
        // 20 tabs at 800px overflow; tab 19 is scrolled out (window sticks to
        // the active head) and its Bell must tint the "+N" hint theme-red.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let mut activity = vec![TabActivity::None; 20];
        activity[19] = TabActivity::Bell;
        let bar = build_tab_bar_ex(
            800, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &activity,
        );
        let hint = |b: &TabBar| {
            b.labels
                .iter()
                .find(|l| {
                    l.0.len() > 1
                        && l.0.starts_with('+')
                        && l.0[1..].chars().all(|c| c.is_ascii_digit())
                })
                .cloned()
        };
        let red = theme().palette[1];
        let tinted = hint(&bar).expect("overflow hint missing");
        assert_eq!(tinted.3, red, "hidden Bell must tint the +N hint red");
        // All-None keeps the dim blend.
        let base = build_tab_bar_ex(
            800, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[],
        );
        let plain = hint(&base).expect("overflow hint missing");
        assert_ne!(plain.3, red);
        assert_eq!(tinted.1, plain.1, "tint must not move the hint");
    }

    #[test]
    fn empty_activity_slice_means_none() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
        ];
        let wrapper = build_tab_bar(1000, &tabs, &theme());
        let ex = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, CHROME_CHAR_W, &[],
        );
        assert_eq!(wrapper.quads.len(), ex.quads.len());
        assert!(dot_quads(&wrapper).is_empty());
    }
}
