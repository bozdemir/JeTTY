use crate::Rect;

/// The keyboard-shortcut rows shown in the Help overlay — ONE binding per line
/// (single column) so a row's text can never overflow the panel's width. The
/// panel width is computed from the longest row below.
pub const HELP_ROWS: [&str; 16] = [
    "F9 — Summon / hide",
    "Ctrl+Shift+T — New tab",
    "Ctrl+Shift+W — Close tab",
    "Ctrl+Tab / Ctrl+Shift+Tab — Next / Prev tab",
    "Ctrl+1..9 — Jump to tab",
    "Double-click tab — Rename",
    "Double-click top bar — Maximize",
    "Ctrl+Shift+P — Settings",
    "Ctrl+= / Ctrl+- / Ctrl+0 — Font size",
    "Ctrl+Shift+C / Ctrl+Shift+V — Copy / Paste",
    "Right-click — Copy/Paste menu",
    "Drag top bar — Move window",
    "Drag edges/corners — Resize",
    "Ctrl+D — Close tab",
    "Esc — Close this help",
    "? — Toggle this help",
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
pub fn build_help_overlay(win_w: u32, win_h: u32) -> HelpOverlay {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // Approximate width of one rendered overlay character. The overlay text uses
    // the configured monospace font (default MesloLGS NF) near 16px, whose
    // advance is ~9.6px; we slightly over-estimate so the computed panel always
    // contains the text rather than clipping it.
    const CHAR_W: f32 = 9.8;
    const PAD: f32 = 20.0;
    const TITLE_H: f32 = 34.0;
    const ROW_H: f32 = 26.0;
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
    let content_w = longest_chars * CHAR_W;

    let rows = HELP_ROWS.len() as f32;
    let panel_h = (PAD + TITLE_H + rows * ROW_H + PAD).min(sh.max(0.0));

    // Ideal width fits the content with full padding; clamp to the window with a
    // margin. If the window is narrower, reduce padding (down to MIN_PAD) so the
    // text still sits inside the border instead of overflowing. The HARD floor is
    // content + 2*MIN_PAD: text-inside-the-border wins over staying on-screen, so
    // for an absurdly narrow window the panel keeps its text (and is simply
    // clamped to x>=0), never clipping a row.
    const MARGIN: f32 = 16.0;
    let max_panel_w = (sw - MARGIN * 2.0).max(0.0);
    let min_panel_w = content_w + MIN_PAD * 2.0;
    let ideal_w = content_w + PAD * 2.0;
    // Prefer ideal, clamp down toward the window, but never below the hard floor.
    let panel_w = ideal_w.min(max_panel_w).max(min_panel_w);
    // Effective horizontal padding after sizing: split the leftover space, but
    // never below MIN_PAD.
    let pad_x = ((panel_w - content_w) / 2.0).clamp(MIN_PAD, PAD);

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();

    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: [0, 0, 0, 150], ..Default::default() });
    // Border.
    quads.push(Rect {
        x: px - 2.0,
        y: py - 2.0,
        w: panel_w + 4.0,
        h: panel_h + 4.0,
        color: [80, 80, 110, 255], ..Default::default() });
    // Background panel.
    let panel = Rect { x: px, y: py, w: panel_w, h: panel_h, color: [26, 26, 34, 245], ..Default::default() };
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push((
        "Keyboard Shortcuts".to_string(),
        px + pad_x,
        py + PAD,
        [235, 235, 245],
    ));

    // Shortcut rows (one binding per row → never overflows the panel width).
    let rows_top = py + PAD + TITLE_H;
    for (i, row) in HELP_ROWS.iter().enumerate() {
        let y = rows_top + i as f32 * ROW_H;
        labels.push((row.to_string(), px + pad_x, y, [200, 205, 220]));
    }

    HelpOverlay { quads, labels, panel }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_is_centered_and_on_screen() {
        let h = build_help_overlay(1000, 700);
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
        for w in [320u32, 500, 700, 1000, 1600] {
            let h = build_help_overlay(w, 700);
            let panel_right = h.panel.x + h.panel.w;
            for (text, x, _y, _c) in &h.labels {
                let est_right = x + text.chars().count() as f32 * 9.8;
                assert!(
                    est_right <= panel_right + 0.5,
                    "row {text:?} overflows panel at width {w}: {est_right} > {panel_right}"
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
        let h = build_help_overlay(1000, 700);
        let joined: String = h.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("F9"));
        assert!(joined.contains("Ctrl+Shift+P"));
        assert!(joined.contains("Ctrl+D"));
    }
}
