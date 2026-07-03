use crate::Rect;

/// The keyboard-shortcut rows shown in the Help overlay — ONE binding per line
/// (single column) so a row's text can never overflow the panel's width. The
/// panel width is computed from the longest row below.
pub const HELP_ROWS: [&str; 21] = [
    "F9 (configurable) — Summon / hide",
    "Ctrl+Shift+T — New tab",
    "Ctrl+Shift+W — Close tab",
    "Ctrl+Tab / Ctrl+Shift+Tab — Next / Prev tab",
    "Ctrl+1..9 — Jump to tab",
    "Ctrl+Shift+D / drag tab off bar — Detach / reattach (right-click tab for menu)",
    "Double-click tab — Rename",
    "Double-click top bar — Maximize",
    "Ctrl+Shift+P — Settings",
    "Ctrl+= / Ctrl+- / Ctrl+0 — Font size",
    "Ctrl+Shift+= / Ctrl+Shift+- — Transparency",
    "Ctrl+Shift+C / Ctrl+Shift+V — Copy / Paste",
    "Ctrl+L — Clear",
    "PageUp / PageDown — Scroll",
    "Left-drag — Select text (auto-copies)",
    "Shift+drag — Select text over mouse apps (vim/htop/Claude Code)",
    "Right-click — Context menu (Copy/Paste/Select All/Clear/Close Tab)",
    "Drag top bar — Move window",
    "Drag edges/corners — Resize",
    "Ctrl+D — Close shell (sends EOF)",
    "Esc — Close this help",
];

/// Geometry + draw data for the Help overlay.
pub struct HelpOverlay {
    /// Quads in draw order: full-screen dim, border, background panel.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — title then one per shortcut row.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The panel rect (for hit-testing "click outside closes").
    pub panel: Rect,
}

/// Build the centered "Keyboard Shortcuts" help overlay for a window of size
/// `win_w`×`win_h` (physical pixels). The panel is sized to fit the rows and
/// clamped on-screen. A click outside `panel` (or Esc / the "?" button) closes it.
///
/// `char_w` is the measured physical-pixel advance of one chrome-font character
/// (from `TextLayer::cell_size().0`). Pass `9.8` when a real measurement is not
/// available (scale-1 fallback used by tests).
pub fn build_help_overlay(win_w: u32, win_h: u32, theme: &jetty_core::Theme, char_w: f32) -> HelpOverlay {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived overlay chrome (mirrors panel.rs::build_panel) ---
    // All colors blend the active theme's bg→fg so the overlay re-skins itself
    // with the theme instead of being a fixed dark card (which was invisible on
    // the light theme and clashed on Gruvbox/Dracula).
    let tbg = theme.bg;
    let tfg = theme.fg;
    let lerp = |t: f32| -> [u8; 3] {
        [
            (tbg[0] as f32 + (tfg[0] as f32 - tbg[0] as f32) * t).round() as u8,
            (tbg[1] as f32 + (tfg[1] as f32 - tbg[1] as f32) * t).round() as u8,
            (tbg[2] as f32 + (tfg[2] as f32 - tbg[2] as f32) * t).round() as u8,
        ]
    };
    let bg3 = lerp(0.06);
    let panel_bg: [u8; 4] = [bg3[0], bg3[1], bg3[2], 242];
    let border3 = lerp(0.30);
    let border_col: [u8; 4] = [border3[0], border3[1], border3[2], 255];
    let title_col = tfg;
    let row_col = lerp(0.70);

    // The caller supplies the measured chrome-font advance via `char_w`.
    // On scale-1 displays this is ~9.8px (the historical hardcoded estimate);
    // on HiDPI it scales proportionally so the panel is always wide enough.
    // Ideal vertical metrics. When the window is too SHORT to fit every row, the
    // padding / title / row heights are scaled DOWN proportionally (to a readable
    // floor) so the overlay always fits and no row clips off-screen.
    const PAD_IDEAL: f32 = 20.0;
    const TITLE_H_IDEAL: f32 = 34.0;
    const ROW_H_IDEAL: f32 = 26.0;
    // Readable floors: below these we stop shrinking (the panel is clamped to the
    // window top instead, which still keeps all rows on a very short window).
    const ROW_H_MIN: f32 = 16.0;
    const TITLE_H_MIN: f32 = 22.0;
    const PAD_MIN_V: f32 = 8.0;
    // Minimum padding kept even when the window is too narrow to fit the ideal
    // padding — we shrink padding before we ever let text overflow.
    const MIN_PAD: f32 = 6.0;

    // The panel must fit the LONGEST row (and the title). Width = longest text
    // width + padding on both sides.
    let longest_chars = HELP_ROWS
        .iter()
        .map(|r| r.chars().count())
        .chain(std::iter::once("Keyboard Shortcuts".chars().count()))
        .max()
        .unwrap_or(0) as f32;
    let content_w = longest_chars * char_w;

    // The vertical / padding metrics are FIXED logical px, but the chrome line box
    // is `ceil(font_size * 1.3)` with `font_size = ui_font_logical * scale`, so it
    // grows with DPI just like `char_w` does. Scale every vertical metric (ideals
    // AND floors) by the same factor the text uses — `char_w` relative to the
    // ~9.8px scale-1 advance — so rows never overlap their neighbour on a 2×
    // display. At scale 1 (`char_w ≈ 9.8`) `vscale == 1` and the layout is
    // unchanged.
    let vscale = (char_w / 9.8).max(0.1);
    let pad_ideal = PAD_IDEAL * vscale;
    let title_h_ideal = TITLE_H_IDEAL * vscale;
    let row_h_ideal = ROW_H_IDEAL * vscale;
    let pad_min_v = PAD_MIN_V * vscale;
    let title_h_min = TITLE_H_MIN * vscale;
    let row_h_min = ROW_H_MIN * vscale;
    let min_pad = MIN_PAD * vscale;

    let rows = HELP_ROWS.len() as f32;
    // Ideal content height; if it exceeds the window, scale the vertical metrics
    // down by a single factor (clamped so each metric keeps its readable floor).
    let ideal_h = pad_ideal + title_h_ideal + rows * row_h_ideal + pad_ideal;
    let avail_h = sh.max(0.0);
    let scale = if ideal_h > avail_h && ideal_h > 0.0 {
        (avail_h / ideal_h).clamp(0.0, 1.0)
    } else {
        1.0
    };
    // Apply the scale, then enforce per-metric floors so text stays legible.
    let pad_v = (pad_ideal * scale).max(pad_min_v);
    let title_h = (title_h_ideal * scale).max(title_h_min);
    let mut row_h = (row_h_ideal * scale).max(row_h_min);
    // Last resort: on a window too short even for the floored metrics, shrink the
    // row pitch BELOW its readable floor so every row still lands inside the
    // clamped panel rather than being drawn off the window bottom (the floors
    // alone can total more than a very short window's height).
    if 2.0 * pad_v + title_h + rows * row_h > avail_h && rows > 0.0 {
        row_h = ((avail_h - 2.0 * pad_v - title_h) / rows).clamp(1.0, row_h);
    }
    // Recompute the actual height from the (possibly floored) metrics, then clamp
    // to the window so the panel can never exceed it.
    let panel_h = (2.0 * pad_v + title_h + rows * row_h).min(avail_h.max(0.0));
    // `PAD` is the vertical text padding (top inset for the title).
    let pad_top = pad_v;

    // Ideal width fits the content with full padding; clamp to the window with a
    // margin. If the window is narrower, reduce padding (down to MIN_PAD) so the
    // text still sits inside the border instead of overflowing. The HARD floor is
    // content + 2*MIN_PAD: text-inside-the-border wins over staying on-screen, so
    // for an absurdly narrow window the panel keeps its text (and is simply
    // clamped to x>=0), never clipping a row.
    const MARGIN: f32 = 16.0;
    let max_panel_w = (sw - MARGIN * 2.0).max(0.0);
    let min_panel_w = content_w + min_pad * 2.0;
    let ideal_w = content_w + pad_ideal * 2.0;
    // Prefer ideal, clamp down toward the window, but never below the hard floor.
    let panel_w = ideal_w.min(max_panel_w).max(min_panel_w);
    // Effective horizontal padding after sizing: split the leftover space, but
    // never below min_pad.
    let pad_x = ((panel_w - content_w) / 2.0).clamp(min_pad, pad_ideal);

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();

    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: [0, 0, 0, 150], ..Default::default() });
    // Border (rounded to match the window/tab frame). Clamp the top to y>=0 so a
    // very short window (py==0) never draws the border off-screen at y=-2.
    let border_y = (py - 2.0).max(0.0);
    quads.push(Rect::rounded(
        (px - 2.0).max(0.0), border_y, panel_w + 4.0, panel_h + 4.0, border_col, 10.0,
    ));
    // Background panel (rounded).
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, 8.0);
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push((
        "Keyboard Shortcuts".to_string(),
        px + pad_x,
        py + pad_top,
        title_col,
    ));

    // Shortcut rows (one binding per row → never overflows the panel width).
    let rows_top = py + pad_top + title_h;
    for (i, row) in HELP_ROWS.iter().enumerate() {
        let y = rows_top + i as f32 * row_h;
        labels.push((row.to_string(), px + pad_x, y, row_col));
    }

    HelpOverlay { quads, labels, panel }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests (matches the historical fallback constant).
    const TEST_CHAR_W: f32 = 9.8;

    #[test]
    fn panel_is_centered_and_on_screen() {
        let h = build_help_overlay(1000, 700, &theme(), TEST_CHAR_W);
        assert!(h.panel.x >= 0.0 && h.panel.y >= 0.0);
        assert!(h.panel.x + h.panel.w <= 1000.0 + 0.5);
        assert!(h.panel.y + h.panel.h <= 700.0 + 0.5);
        // Title + one label per row.
        assert_eq!(h.labels.len(), HELP_ROWS.len() + 1);
        assert_eq!(h.labels[0].0, "Keyboard Shortcuts");
    }

    #[test]
    fn every_row_text_fits_inside_panel() {
        // Across a range of widths (including very narrow), no row's estimated
        // rendered text right edge may exceed the panel's right border.
        // The estimate uses the same char_w passed to the builder so the panel
        // is always sized to contain the text.
        for w in [320u32, 500, 700, 1000, 1600] {
            let h = build_help_overlay(w, 700, &theme(), TEST_CHAR_W);
            let panel_right = h.panel.x + h.panel.w;
            for (text, x, _y, _c) in &h.labels {
                let est_right = x + text.chars().count() as f32 * TEST_CHAR_W;
                assert!(
                    est_right <= panel_right + 0.5,
                    "row {text:?} overflows panel at width {w}: {est_right} > {panel_right}"
                );
            }
        }
    }

    #[test]
    fn every_row_fits_vertically_at_short_heights() {
        // At short window heights the overlay must still fit every row on-screen
        // (the lower rows must not clip off the bottom of the window).
        for h in [360u32, 420, 480, 640] {
            let overlay = build_help_overlay(700, h, &theme(), TEST_CHAR_W);
            // The panel itself fits the window.
            assert!(
                overlay.panel.y >= 0.0 && overlay.panel.y + overlay.panel.h <= h as f32 + 0.5,
                "panel exceeds window at height {h}"
            );
            // Every label's baseline sits inside the window.
            for (text, _x, y, _c) in &overlay.labels {
                assert!(
                    *y >= 0.0 && *y <= h as f32,
                    "row {text:?} clips off-screen at height {h}: y={y}"
                );
            }
        }
    }

    #[test]
    fn rows_do_not_overlap_in_the_readable_range() {
        // Evaluation of F40 (SPLIT): the row PITCH must stay at or above the
        // chrome font ink height (~= font_size = ROW_H_MIN at scale 1) for every
        // window height down to the point where the floored metrics still fit —
        // so adjacent rows never overlap in the readable range. Below that the
        // builder DELIBERATELY tightens the pitch (last-resort branch) to keep all
        // rows on-screen rather than clip, which is documented, intentional
        // behaviour for an extreme (<~381px) window and not exercised here.
        let ink_floor = 16.0_f32; // ROW_H_MIN == font_size at scale 1 (vscale==1)
        for h in [420u32, 480, 560, 700, 900] {
            let overlay = build_help_overlay(700, h, &theme(), TEST_CHAR_W);
            // labels[0] is the title; labels[1..] are the shortcut rows in order.
            let ys: Vec<f32> = overlay.labels[1..].iter().map(|(_t, _x, y, _c)| *y).collect();
            for pair in ys.windows(2) {
                let pitch = pair[1] - pair[0];
                assert!(
                    pitch >= ink_floor - 0.01,
                    "adjacent help rows overlap at height {h}: pitch {pitch} < {ink_floor}"
                );
            }
        }
    }

    #[test]
    fn single_column_rows() {
        // No row contains the two-column "·" separator anymore.
        for r in HELP_ROWS.iter() {
            assert!(!r.contains('·'), "row should be single-column: {r:?}");
        }
    }

    #[test]
    fn lists_core_bindings() {
        let h = build_help_overlay(1000, 700, &theme(), TEST_CHAR_W);
        let joined: String = h.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("F9"));
        assert!(joined.contains("Ctrl+Shift+P"));
        assert!(joined.contains("Ctrl+D"));
    }
}
