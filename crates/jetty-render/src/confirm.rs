use crate::Rect;

/// Geometry + draw data for the "Close this tab?" confirmation popup. Reuses the
/// overlay/panel visual style (full-screen dim + bordered rounded panel) and
/// exposes clickable Close / Cancel button rects.
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

/// Build a centered confirmation popup asking whether to close the tab titled
/// `title`, for a window of `win_w`×`win_h` physical pixels.
pub fn build_confirm_close(
    win_w: u32,
    win_h: u32,
    title: &str,
    theme: &jetty_core::Theme,
) -> ConfirmPopup {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived popup colors (mirrors panel.rs::build_panel) ---
    let tbg = theme.bg;
    let tfg = theme.fg;
    let green = theme.palette[2]; // confirm/close button = theme green
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

    const CHAR_W: f32 = 9.8;
    const PAD: f32 = 20.0;
    const RADIUS: f32 = 8.0;
    const BTN_H: f32 = 30.0;
    const BTN_GAP: f32 = 16.0;

    // The two button labels (kept for width sizing + drawing).
    let close_label = "Enter — Close";
    let cancel_label = "Esc — Cancel";

    // Truncate the title for the prompt so it never overflows the popup.
    let shown_title: String = if title.chars().count() > 28 {
        let t: String = title.chars().take(27).collect();
        format!("{t}…")
    } else {
        title.to_string()
    };
    let prompt = format!("Close tab \"{shown_title}\"?");

    // Width fits the widest line (prompt, or the two buttons side by side).
    let btn_close_w = close_label.chars().count() as f32 * CHAR_W + 20.0;
    let btn_cancel_w = cancel_label.chars().count() as f32 * CHAR_W + 20.0;
    let buttons_w = btn_close_w + BTN_GAP + btn_cancel_w;
    let prompt_w = prompt.chars().count() as f32 * CHAR_W;
    let content_w = prompt_w.max(buttons_w);
    let panel_w = (content_w + PAD * 2.0).min((sw - 32.0).max(0.0)).max(content_w + 12.0);

    // Height: pad + prompt line + gap + buttons + pad.
    let panel_h = PAD + 28.0 + 18.0 + BTN_H + PAD;

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();
    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: [0, 0, 0, 150], ..Default::default() });
    // Border (rounded).
    quads.push(Rect::rounded(
        px - 2.0, py - 2.0, panel_w + 4.0, panel_h + 4.0,
        border_col, RADIUS + 2.0,
    ));
    // Background panel (rounded).
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, RADIUS);
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Prompt line.
    labels.push((prompt, px + PAD, py + PAD, text_col));

    // Buttons row, centered horizontally within the panel.
    let btn_y = py + panel_h - PAD - BTN_H;
    let total_btn_w = btn_close_w + BTN_GAP + btn_cancel_w;
    let btn_x0 = px + (panel_w - total_btn_w) / 2.0;
    let close_rect = Rect::rounded(btn_x0, btn_y, btn_close_w, BTN_H, close_btn, 5.0);
    let cancel_x = btn_x0 + btn_close_w + BTN_GAP;
    let cancel_rect = Rect::rounded(cancel_x, btn_y, btn_cancel_w, BTN_H, cancel_btn, 5.0);
    quads.push(close_rect);
    quads.push(cancel_rect);

    // Button labels: white-on-green for Close, theme fg for Cancel.
    labels.push((close_label.to_string(), btn_x0 + 10.0, btn_y + 8.0, [245, 245, 245]));
    labels.push((cancel_label.to_string(), cancel_x + 10.0, btn_y + 8.0, text_col));

    ConfirmPopup { quads, labels, panel, close_rect, cancel_rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    #[test]
    fn popup_is_centered_and_has_buttons() {
        let p = build_confirm_close(1000, 700, "Tab 1", &theme());
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
        let p = build_confirm_close(1000, 700, &long, &theme());
        let prompt = &p.labels[0].0;
        assert!(prompt.contains('…'), "long title should be truncated: {prompt}");
        // Panel still fits the window width.
        assert!(p.panel.x + p.panel.w <= 1000.0 + 0.5);
    }
}
