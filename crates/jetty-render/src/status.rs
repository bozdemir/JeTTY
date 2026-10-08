//! The bottom status strip (perf HUD) and the centred toast pill. The main
//! window, detached windows and jetty-shot all build them here, so they share
//! one layout — including fitting a narrow window or a large UI font, where the
//! text is ellipsized instead of running past the window edge.

use crate::chrome::{fit_head, ChromeMeasure, ChromeMetrics};
use crate::ui_palette::{rgba, UiPalette};
use crate::Rect;

/// A faint lifted band across the window bottom with its text right-aligned.
pub struct StatusStrip {
    pub quad: Rect,
    /// The text label (`None` when there is no text to show).
    pub label: Option<(String, f32, f32, [u8; 3])>,
}

/// The status strip of a window `win_w` px wide spanning `[strip_y, strip_y +
/// status_h)`. `text` (the perf HUD line) is measured as drawn and
/// right-aligned; a window too narrow for it ellipsizes its tail, keeping the
/// leading (most important) metrics.
pub fn build_status_strip(
    win_w: u32,
    strip_y: f32,
    status_h: f32,
    text: Option<&str>,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
) -> StatusStrip {
    // Theme-derived: a faint lifted strip (the chrome surface) + dim text that
    // still reads on it (hint-grade, ≥ 3:1 — the fixed half blend didn't on
    // light themes).
    let ui = UiPalette::cached(theme);
    let w = win_w as f32;
    let quad = Rect {
        x: 0.0,
        y: strip_y,
        w,
        h: status_h,
        color: rgba(ui.surface, 255),
        ..Default::default()
    };
    let label = text.filter(|t| !t.is_empty()).map(|t| {
        let right_pad = cm.px(12.0);
        let left_pad = cm.px(8.0);
        let shown = fit_head(m, t, (w - right_pad - left_pad).max(0.0), false);
        let x = (w - m.text_w(&shown) - right_pad).max(left_pad);
        (shown, x, strip_y + (status_h - cm.text_h()) / 2.0, ui.text_hint)
    });
    StatusStrip { quad, label }
}

/// A centred toast pill (the Shift-drag hint, the run-selection status).
pub struct ToastPill {
    pub quad: Rect,
    pub label: (String, f32, f32, [u8; 3]),
}

/// A toast pill centred in a window `win_w` px wide whose bottom edge sits at
/// `bottom` (clamped on-screen), then shifted by `y_offset` (the dropdown
/// slide). Its text is measured as drawn and ellipsized only when the pill
/// would otherwise be wider than the window.
pub fn build_toast_pill(
    win_w: u32,
    bottom: f32,
    y_offset: f32,
    text: &str,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
) -> ToastPill {
    let w = win_w as f32;
    let pad = cm.px(14.0);
    let shown = fit_head(m, text, (w - 2.0 * pad).max(0.0), false);
    let pill_w = m.text_w(&shown) + pad * 2.0;
    let pill_h = cm.pill_h();
    let pill_x = ((w - pill_w) / 2.0).max(0.0);
    let pill_y = (bottom - pill_h).max(0.0) + y_offset;
    // The pill keeps the theme's cursor color; its text is that fill's own
    // readable color (a fixed near-black read 1.0–1.6:1 on dark-cursor themes).
    let ui = UiPalette::cached(theme);
    let quad = Rect::rounded(pill_x, pill_y, pill_w, pill_h, rgba(ui.cursor, 235), pill_h / 2.0);
    let label = (shown, pill_x + pad, pill_y + (pill_h - cm.text_h()) / 2.0, ui.on_cursor);
    ToastPill { quad, label }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    const PERF: &str = "⚡ 1.4 ms · 60 fps · 2% CPU · 0.1 MB/s";
    const HINT: &str = "Hold Shift while dragging to select text";

    #[test]
    fn wide_window_keeps_the_classic_layout() {
        let cm = ChromeMetrics::DEFAULT;
        let mut m = MonoMeasure(9.6);
        let s = build_status_strip(1000, 618.0, 22.0, Some(PERF), &theme(), &mut m, cm);
        let (text, x, y, _) = s.label.expect("perf label");
        assert_eq!(text, PERF);
        assert_eq!(x, 1000.0 - m.text_w(PERF) - 12.0, "right-aligned 12px from the edge");
        assert_eq!(y, 618.0 + (22.0 - 16.0) / 2.0);
        let p = build_toast_pill(1000, 604.0, 0.0, HINT, &theme(), &mut m, cm);
        assert_eq!(p.label.0, HINT);
        assert_eq!(p.quad.w, m.text_w(HINT) + 28.0);
        assert_eq!(p.quad.y, 604.0 - 26.0);
        assert!(build_status_strip(1000, 0.0, 22.0, None, &theme(), &mut m, cm).label.is_none());
    }

    #[test]
    fn strip_and_pill_text_read_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
        let cm = ChromeMetrics::DEFAULT;
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let mut m = MonoMeasure(9.6);
            let s = build_status_strip(1000, 618.0, 22.0, Some(PERF), &t, &mut m, cm);
            let c = cr(s.label.unwrap().3, rgb(s.quad.color));
            assert!(c >= 3.0, "{}: status text {c}", t.name);
            let p = build_toast_pill(1000, 604.0, 0.0, HINT, &t, &mut m, cm);
            let c = cr(p.label.3, rgb(p.quad.color));
            assert!(c >= 4.5, "{}: pill text {c}", t.name);
        }
    }

    #[test]
    fn narrow_window_ellipsizes_instead_of_overflowing() {
        // A 28pt UI font in a 420px window: both texts are wider than the window
        // and used to run off its right edge.
        let cm = ChromeMetrics::new(1.0, 28.0);
        let mut m = MonoMeasure(9.6 * cm.u);
        let s = build_status_strip(420, 480.0, cm.status_h(), Some(PERF), &theme(), &mut m, cm);
        let (text, x, _, _) = s.label.unwrap();
        assert!(text.ends_with('…') && text.starts_with("⚡ 1.4 ms"), "{text:?}");
        assert!(x >= 0.0 && x + m.text_w(&text) <= 420.0, "strip text past the window");
        let p = build_toast_pill(420, 460.0, 0.0, HINT, &theme(), &mut m, cm);
        assert!(p.label.0.ends_with('…'), "{:?}", p.label.0);
        assert!(p.quad.x >= 0.0 && p.quad.x + p.quad.w <= 420.0, "pill past the window");
        assert!(p.label.1 + m.text_w(&p.label.0) <= p.quad.x + p.quad.w);
    }
}
