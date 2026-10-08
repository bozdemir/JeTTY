use crate::chrome::{ChromeMeasure, ChromeMetrics};
use crate::Rect;

/// Geometry + draw data for a confirmation popup. Reuses the overlay/panel
/// visual style (full-screen dim + bordered rounded panel) and exposes clickable
/// Close / Cancel button rects.
pub struct ConfirmPopup {
    /// Quads in draw order: full-screen dim, border, background panel, buttons.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb).
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The panel rect (for hit-testing "click outside cancels").
    pub panel: Rect,
    /// The "Enter — Close" button rect (confirm).
    pub close_rect: Rect,
    /// The "Esc — Cancel" button rect (cancel).
    pub cancel_rect: Rect,
}

/// Build a centered confirmation popup with an arbitrary `prompt` line plus
/// Enter—Close / Esc—Cancel buttons, for a `win_w`×`win_h` (physical px) window.
///
/// `m` measures the prompt and button labels as drawn; `cm` scales paddings,
/// the button row and the corner radius with DPI × UI font, so the popup grows
/// with its text instead of the glyphs overflowing fixed 30px buttons.
pub fn build_confirm(
    win_w: u32,
    win_h: u32,
    prompt: &str,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
) -> ConfirmPopup {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived popup colors (mirrors panel.rs::build_panel) ---
    let tbg = theme.bg;
    let tfg = theme.fg;
    let green = theme.palette[2]; // confirm button = theme green
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
    let text_col = tfg;
    let close_btn: [u8; 4] = [green[0], green[1], green[2], 255];
    let cancel3 = lerp(0.18);
    let cancel_btn: [u8; 4] = [cancel3[0], cancel3[1], cancel3[2], 255];

    // Design px (16pt UI font, 1×), scaled by the chrome unit.
    let pad = cm.px(20.0);
    let radius = cm.px(8.0);
    let btn_h = cm.px(30.0);
    let btn_gap = cm.px(16.0);

    let close_label = "Enter — Close";
    let cancel_label = "Esc — Cancel";

    // Truncate the prompt so it never overflows the popup.
    let prompt: String = if prompt.chars().count() > 44 {
        let t: String = prompt.chars().take(43).collect();
        format!("{t}…")
    } else {
        prompt.to_string()
    };

    // Width fits the widest line (prompt, or the two buttons side by side).
    let btn_label_pad = cm.px(10.0);
    let btn_close_w = m.text_w(close_label) + 2.0 * btn_label_pad;
    let btn_cancel_w = m.text_w(cancel_label) + 2.0 * btn_label_pad;
    let buttons_w = btn_close_w + btn_gap + btn_cancel_w;
    let prompt_w = m.text_w(&prompt);
    let content_w = prompt_w.max(buttons_w);
    let panel_w = (content_w + pad * 2.0).min((sw - 32.0).max(0.0)).max(content_w + cm.px(12.0));

    // Height: pad + prompt line + gap + buttons + pad.
    let panel_h = pad + cm.px(28.0) + cm.px(18.0) + btn_h + pad;

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();
    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: [0, 0, 0, 150], ..Default::default() });
    // Border (rounded).
    quads.push(Rect::rounded(
        px - 2.0, py - 2.0, panel_w + 4.0, panel_h + 4.0,
        border_col, radius + 2.0,
    ));
    // Background panel (rounded).
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, radius);
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Prompt line (vertically centered in its ~28px region).
    labels.push((prompt, px + pad, py + pad + cm.px(5.0), text_col));

    // Buttons row, centered horizontally within the panel.
    let btn_y = py + panel_h - pad - btn_h;
    let total_btn_w = btn_close_w + btn_gap + btn_cancel_w;
    let btn_x0 = px + (panel_w - total_btn_w) / 2.0;
    let close_rect = Rect::rounded(btn_x0, btn_y, btn_close_w, btn_h, close_btn, cm.px(5.0));
    let cancel_x = btn_x0 + btn_close_w + btn_gap;
    let cancel_rect = Rect::rounded(cancel_x, btn_y, btn_cancel_w, btn_h, cancel_btn, cm.px(5.0));
    quads.push(close_rect);
    quads.push(cancel_rect);

    // Button labels: white-on-green for Close, theme fg for Cancel.
    let btn_text_y = btn_y + cm.px(6.0);
    labels.push((close_label.to_string(), btn_x0 + btn_label_pad, btn_text_y, [245, 245, 245]));
    labels.push((cancel_label.to_string(), cancel_x + btn_label_pad, btn_text_y, text_col));

    ConfirmPopup { quads, labels, panel, close_rect, cancel_rect }
}

/// Confirmation popup asking whether to close the tab titled `title`.
///
/// `m` / `cm` are forwarded to `build_confirm`; see its docs.
pub fn build_confirm_close(
    win_w: u32,
    win_h: u32,
    title: &str,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
) -> ConfirmPopup {
    let shown_title: String = if title.chars().count() > 28 {
        let t: String = title.chars().take(27).collect();
        format!("{t}…")
    } else {
        title.to_string()
    };
    build_confirm(win_w, win_h, &format!("Close tab \"{shown_title}\"?"), theme, m, cm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests.
    const TEST_CHAR_W: f32 = 9.8;

    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    #[test]
    fn popup_is_centered_and_has_buttons() {
        let p = build_confirm_close(1000, 700, "Tab 1", &theme(), &mut mono(), CM);
        assert!(p.panel.x >= 0.0 && p.panel.y >= 0.0);
        assert!(p.panel.x + p.panel.w <= 1000.0 + 0.5);
        // Close button sits left of Cancel.
        assert!(p.close_rect.x < p.cancel_rect.x);
        // The prompt mentions the tab title.
        assert!(p.labels.iter().any(|l| l.0.contains("Tab 1")));
    }

    #[test]
    fn long_title_is_truncated() {
        let long = "a".repeat(80);
        let p = build_confirm_close(1000, 700, &long, &theme(), &mut mono(), CM);
        let prompt = &p.labels[0].0;
        assert!(prompt.contains('…'), "long title should be truncated: {prompt}");
        assert!(p.panel.x + p.panel.w <= 1000.0 + 0.5);
    }

    #[test]
    fn generic_confirm_shows_prompt() {
        let p = build_confirm(1000, 700, "Quit JeTTY?", &theme(), &mut mono(), CM);
        assert!(p.labels.iter().any(|l| l.0.contains("Quit JeTTY?")));
    }
}
