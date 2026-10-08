use crate::chrome::{fit_head, ChromeMeasure, ChromeMetrics};
use crate::ui_palette::{rgba, UiPalette};
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

    // --- Theme-derived popup colors (the shared UiPalette) ---
    let ui = UiPalette::cached(theme);
    let panel_bg = rgba(ui.surface, 242);
    let border_col = rgba(ui.border, 255);
    let text_col = ui.text;
    // Confirm = the theme's green with ITS text color (a fixed near-white label
    // read under 3:1 on 19 of 22 themes); Cancel = a raised neutral.
    let close_btn = rgba(ui.success, 255);
    let close_text = ui.on_success;
    let cancel_btn = rgba(ui.surface_hi, 255);

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

    // Width fits the widest line (prompt, or the two buttons side by side). In
    // a window too narrow for that (a large UI font), the prompt wraps onto up
    // to three lines and the buttons stack, so nothing runs past the window.
    let room = (sw - 32.0 - cm.px(12.0)).max(0.0);
    let btn_label_pad = cm.px(10.0);
    let btn_close_w = m.text_w(close_label) + 2.0 * btn_label_pad;
    let btn_cancel_w = m.text_w(cancel_label) + 2.0 * btn_label_pad;
    let buttons_w = btn_close_w + btn_gap + btn_cancel_w;
    let stack_buttons = buttons_w > room;
    // Stacked buttons share one width (the wider one, capped to the room).
    let stacked_btn_w = btn_close_w.max(btn_cancel_w).min(room);
    let lines = wrap_lines(m, &prompt, room, 3);
    let prompt_w = lines.iter().map(|l| m.text_w(l)).fold(0.0, f32::max);
    let content_w = prompt_w.max(if stack_buttons { stacked_btn_w } else { buttons_w });
    let panel_w = (content_w + pad * 2.0).min((sw - 32.0).max(0.0)).max(content_w + cm.px(12.0));

    // Height: pad + prompt line(s) + gap + button row(s) + pad.
    let line_pitch = cm.px(24.0);
    let extra_lines_h = lines.len().saturating_sub(1) as f32 * line_pitch;
    let stack_gap = cm.px(10.0);
    let buttons_h = if stack_buttons { btn_h * 2.0 + stack_gap } else { btn_h };
    let panel_h = pad + cm.px(28.0) + extra_lines_h + cm.px(18.0) + buttons_h + pad;

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();
    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: ui.scrim, ..Default::default() });
    // Border (rounded).
    quads.push(Rect::rounded(
        px - 2.0, py - 2.0, panel_w + 4.0, panel_h + 4.0,
        border_col, radius + 2.0,
    ));
    // Background panel (rounded).
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, radius);
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Prompt line(s) (the first vertically centered in its ~28px region).
    for (i, line) in lines.into_iter().enumerate() {
        labels.push((line, px + pad, py + pad + cm.px(5.0) + i as f32 * line_pitch, text_col));
    }

    let btn_y = py + panel_h - pad - btn_h;
    let (close_rect, cancel_rect) = if stack_buttons {
        // Close above Cancel, one shared width, each centered in the panel; a
        // label too wide even for the window is ellipsized.
        let x = px + (panel_w - stacked_btn_w) / 2.0;
        let close_y = btn_y - stack_gap - btn_h;
        let close_rect = Rect::rounded(x, close_y, stacked_btn_w, btn_h, close_btn, cm.px(5.0));
        let cancel_rect = Rect::rounded(x, btn_y, stacked_btn_w, btn_h, cancel_btn, cm.px(5.0));
        let label_room = (stacked_btn_w - 2.0 * btn_label_pad).max(0.0);
        for (label, y, col) in [
            (close_label, close_y, close_text),
            (cancel_label, btn_y, text_col),
        ] {
            let shown = fit_head(m, label, label_room, false);
            let lx = x + (stacked_btn_w - m.text_w(&shown)) / 2.0;
            labels.push((shown, lx, y + cm.px(6.0), col));
        }
        (close_rect, cancel_rect)
    } else {
        // Buttons row, centered horizontally within the panel.
        let total_btn_w = btn_close_w + btn_gap + btn_cancel_w;
        let btn_x0 = px + (panel_w - total_btn_w) / 2.0;
        let close_rect = Rect::rounded(btn_x0, btn_y, btn_close_w, btn_h, close_btn, cm.px(5.0));
        let cancel_x = btn_x0 + btn_close_w + btn_gap;
        let cancel_rect = Rect::rounded(cancel_x, btn_y, btn_cancel_w, btn_h, cancel_btn, cm.px(5.0));
        // Button labels: the green's own text color for Close, the text for Cancel.
        let btn_text_y = btn_y + cm.px(6.0);
        labels.push((close_label.to_string(), btn_x0 + btn_label_pad, btn_text_y, close_text));
        labels.push((cancel_label.to_string(), cancel_x + btn_label_pad, btn_text_y, text_col));
        (close_rect, cancel_rect)
    };
    quads.push(close_rect);
    quads.push(cancel_rect);

    ConfirmPopup { quads, labels, panel, close_rect, cancel_rect }
}

/// Greedy word wrap of `s` into at most `max_lines` lines no wider than
/// `max_w` (measured as drawn). A single word wider than a line, and whatever
/// is left for the last line, are ellipsized.
fn wrap_lines(m: &mut dyn ChromeMeasure, s: &str, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        if lines.len() + 1 >= max_lines || m.text_w(rest) <= max_w {
            lines.push(fit_head(m, rest, max_w, false));
            break;
        }
        // The longest run of whole words that fits.
        let mut cut = None;
        for (i, _) in rest.match_indices(' ') {
            if m.text_w(&rest[..i]) > max_w {
                break;
            }
            cut = Some(i);
        }
        let end = cut.unwrap_or_else(|| rest.find(' ').unwrap_or(rest.len()));
        lines.push(fit_head(m, &rest[..end], max_w, false));
        rest = rest[end..].trim_start();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
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
    // Clip first: the title is program-controlled (OSC 0/2) and may be huge.
    let (title, _) = crate::chrome::clip_head(title);
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
    fn narrow_window_wraps_the_prompt_and_stacks_the_buttons() {
        // A 28pt UI font in a 420px window: the prompt wraps, the buttons
        // stack, and the popup plus every label stays inside the window (it
        // used to run ~100px past the right edge).
        let cm = ChromeMetrics::new(1.0, 28.0);
        let mut m = MonoMeasure(9.6 * cm.u);
        let p = build_confirm(420, 520, "Quit JeTTY? — all tabs will close", &theme(), &mut m, cm);
        assert!(p.panel.x >= 0.0 && p.panel.x + p.panel.w <= 420.0 + 0.5, "popup past the window");
        for (text, x, _y, _c) in &p.labels {
            assert!(x + m.text_w(text) <= p.panel.x + p.panel.w + 0.5, "{text:?} overflows");
        }
        assert!(p.labels[1].2 > p.labels[0].2, "prompt wrapped onto a second line");
        let prompt: Vec<&str> =
            p.labels.iter().filter(|l| l.2 < p.close_rect.y).map(|l| l.0.as_str()).collect();
        assert_eq!(prompt.join(" "), "Quit JeTTY? — all tabs will close", "the wrap keeps every word");
        assert!(p.close_rect.y + p.close_rect.h <= p.cancel_rect.y, "buttons stacked");
        assert!(p.cancel_rect.y + p.cancel_rect.h <= p.panel.y + p.panel.h);
        // Wide window: one line, buttons side by side.
        let p = build_confirm(1000, 700, "Quit JeTTY? — all tabs will close", &theme(), &mut mono(), CM);
        assert_eq!(p.close_rect.y, p.cancel_rect.y);
        assert_eq!(p.labels.len(), 3);
    }

    #[test]
    fn buttons_and_prompt_read_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let p = build_confirm(1000, 700, "Quit JeTTY? — all tabs will close", &t, &mut mono(), CM);
            let col = |text: &str| p.labels.iter().find(|l| l.0 == text).unwrap().3;
            let n = &t.name;
            let close = cr(col("Enter — Close"), rgb(p.close_rect.color));
            assert!(close >= 4.5, "{n}: Close label {close}");
            let cancel = cr(col("Esc — Cancel"), rgb(p.cancel_rect.color));
            assert!(cancel >= 4.5, "{n}: Cancel label {cancel}");
            let prompt = cr(p.labels[0].3, rgb(p.panel.color));
            assert!(prompt >= 4.5, "{n}: prompt {prompt}");
            // The modal dim comes from the palette (lighter on light themes).
            assert_eq!(p.quads[0].color, UiPalette::cached(&t).scrim);
        }
    }

    #[test]
    fn generic_confirm_shows_prompt() {
        let p = build_confirm(1000, 700, "Quit JeTTY?", &theme(), &mut mono(), CM);
        assert!(p.labels.iter().any(|l| l.0.contains("Quit JeTTY?")));
    }
}
