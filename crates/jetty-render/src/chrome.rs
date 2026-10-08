//! Shared chrome SCALE and text MEASUREMENT for every UI-chrome builder (tab
//! bar, status strip, pills, menus, palette, search bar, help, hint chips,
//! confirm dialogs, the Settings panel).
//!
//! Two independent inputs decide how chrome is laid out, and they used to be
//! conflated through one number — the chrome font's measured 'M' advance:
//!
//! * [`ChromeMetrics`] — HOW BIG the chrome is: the window's DPI scale × the
//!   UI font size. Every design constant (bar height, row pitch, paddings) is
//!   authored at the 16pt UI font on a 1× display and multiplied by
//!   [`ChromeMetrics::u`], so bars, rows and pills grow with the text they hold.
//!   Deriving the scale from a glyph advance instead made it depend on the
//!   FONT: a proportional UI font (wide 'M') inflated the palette/help/panel
//!   ~1.5×, and switching the terminal font family resized the chrome.
//! * [`ChromeMeasure`] — HOW WIDE a label actually renders, shaped with the
//!   same family/size/shaping as the chrome text pass. `chars × advance` is
//!   only right for a monospace font; with a proportional UI font it misplaced
//!   carets, fuzzy-match highlights, right-aligned values and truncation.

use unicode_width::UnicodeWidthChar;

/// UI font size (logical pt) every chrome design constant is authored at.
pub const UI_FONT_BASE: f32 = 16.0;
/// Unscaled height of the bottom status strip (the perf HUD).
pub const STATUS_H_BASE: f32 = 22.0;
/// Unscaled height of a toast pill (Shift-drag hint, run-selection status).
pub const PILL_H_BASE: f32 = 26.0;

/// Chrome size for one window: its DPI scale and the UI font size, folded into
/// a single chrome unit `u` (physical px per design px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeMetrics {
    /// Window DPI scale factor (physical px per logical px). Drives the few
    /// window-SHAPE distances (inset from the rounded corners), which follow
    /// the window, not the text.
    pub dpi: f32,
    /// Chrome unit: `dpi × ui_font / 16`. Exactly `1.0` on a 1× display with
    /// the default 16pt UI font, where every metric equals its design constant.
    pub u: f32,
}

impl Default for ChromeMetrics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ChromeMetrics {
    /// 1× display, 16pt UI font — the design baseline (tests, headless shots).
    pub const DEFAULT: ChromeMetrics = ChromeMetrics { dpi: 1.0, u: 1.0 };

    /// Metrics for a window at `dpi` showing chrome at `ui_font_logical` pt.
    /// Non-finite / non-positive inputs fall back to the baseline so a bogus
    /// scale factor can never collapse or explode the chrome.
    pub fn new(dpi: f32, ui_font_logical: f32) -> Self {
        let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 1.0 };
        let font = if ui_font_logical.is_finite() && ui_font_logical > 0.0 {
            ui_font_logical
        } else {
            UI_FONT_BASE
        };
        Self { dpi, u: dpi * font / UI_FONT_BASE }
    }

    /// Scale a design-px distance (authored at 16pt, 1×) to physical px.
    #[inline]
    pub fn px(self, design: f32) -> f32 {
        design * self.u
    }

    /// Scale a logical-px window-shape distance by the DPI only.
    #[inline]
    pub fn dpx(self, logical: f32) -> f32 {
        logical * self.dpi
    }

    /// Height of the tab bar / detached title bar, in whole physical px (the
    /// grid starts right below it, so it stays pixel-aligned).
    #[inline]
    pub fn bar_h(self) -> f32 {
        (crate::tabbar::TABBAR_H * self.u).round()
    }

    /// Height of the bottom status strip (perf HUD), in whole physical px.
    #[inline]
    pub fn status_h(self) -> f32 {
        (STATUS_H_BASE * self.u).round()
    }

    /// Height of a toast pill, in whole physical px.
    #[inline]
    pub fn pill_h(self) -> f32 {
        (PILL_H_BASE * self.u).round()
    }

    /// Nominal height of one chrome text line for vertical centring: the UI
    /// font's em (16 design px). Builders centre a label in a band of height
    /// `h` at `(h - text_h) / 2`, the idiom every chrome surface already used.
    #[inline]
    pub fn text_h(self) -> f32 {
        UI_FONT_BASE * self.u
    }

    /// Inset of the tab strip / window controls from the window's left/right
    /// edges, clear of the rounded corners (DPI-scaled like the corner radius).
    #[inline]
    pub fn strip_pad(self) -> f32 {
        crate::tabbar::STRIP_PAD * self.dpi
    }

    /// Width of one window-control cell (?, ⚙, ─, ▢, ✕).
    #[inline]
    pub fn ctrl_w(self) -> f32 {
        crate::tabbar::CTRL_W_BASE * self.u
    }

    /// Width of the five-cell window-control cluster at the bar's right end.
    #[inline]
    pub fn controls_w(self) -> f32 {
        self.ctrl_w() * 5.0
    }
}

/// Measures chrome text exactly as the chrome text pass will render it (same
/// family, size and shaping), in physical px.
///
/// `title` selects the tab-TITLE family (the platform sans at the default UI
/// font) instead of the family every other chrome label uses (the monospace
/// Nerd Font at the default, for its symbol glyphs). With a user-chosen UI
/// family both are that family.
pub trait ChromeMeasure {
    /// x offset of every char boundary of `s`: `out[i]` is the rendered width
    /// of the first `i` chars, so `out.len() == s.chars().count() + 1`,
    /// `out[0] == 0` and the last entry is the full width. Non-decreasing.
    fn char_xs(&mut self, s: &str, title: bool, out: &mut Vec<f32>);

    /// Rendered width of a non-title chrome label.
    fn text_w(&mut self, s: &str) -> f32 {
        let mut xs = Vec::new();
        self.char_xs(s, false, &mut xs);
        xs.last().copied().unwrap_or(0.0)
    }

    /// Rendered width of a tab-title label.
    fn title_w(&mut self, s: &str) -> f32 {
        let mut xs = Vec::new();
        self.char_xs(s, true, &mut xs);
        xs.last().copied().unwrap_or(0.0)
    }
}

/// Fixed-advance measurer: every char is its display width (wide CJK = 2,
/// zero-width = 0) × the advance. Exact for a monospace chrome font; used by
/// unit tests and as the fallback before a GPU text layer exists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonoMeasure(pub f32);

impl ChromeMeasure for MonoMeasure {
    fn char_xs(&mut self, s: &str, _title: bool, out: &mut Vec<f32>) {
        out.clear();
        out.push(0.0);
        let mut cells = 0usize;
        for c in s.chars() {
            cells += c.width().unwrap_or(0);
            out.push(cells as f32 * self.0);
        }
    }
}

/// The ellipsis appended to head-truncated labels.
pub const ELLIPSIS: &str = "…";

/// Longest head of `s` that fits `max_w` px, with [`ELLIPSIS`] appended (its
/// width reserved) when anything was cut. At least one char is kept so a
/// truncated label never collapses to a bare ellipsis.
pub fn fit_head(m: &mut dyn ChromeMeasure, s: &str, max_w: f32, title: bool) -> String {
    let full = if title { m.title_w(s) } else { m.text_w(s) };
    if full <= max_w {
        return s.to_string();
    }
    let ell = if title { m.title_w(ELLIPSIS) } else { m.text_w(ELLIPSIS) };
    let mut xs = Vec::new();
    m.char_xs(s, title, &mut xs);
    // Largest k (≥ 1) whose prefix plus the ellipsis still fits.
    let mut k = 0usize;
    for (i, x) in xs.iter().enumerate().skip(1) {
        if x + ell <= max_w {
            k = i;
        } else {
            break;
        }
    }
    let k = k.max(1).min(xs.len().saturating_sub(1));
    let mut out: String = s.chars().take(k).collect();
    out.push_str(ELLIPSIS);
    out
}

/// Longest TAIL of `s` that fits `max_w` px (no ellipsis — used where the end
/// must stay visible, e.g. a query next to its caret).
pub fn fit_tail(m: &mut dyn ChromeMeasure, s: &str, max_w: f32, title: bool) -> String {
    let mut xs = Vec::new();
    m.char_xs(s, title, &mut xs);
    let total = xs.last().copied().unwrap_or(0.0);
    if total <= max_w {
        return s.to_string();
    }
    // Smallest k whose suffix (total - xs[k]) fits.
    let start = xs.iter().position(|x| total - x <= max_w).unwrap_or(xs.len().saturating_sub(1));
    s.chars().skip(start).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metrics_are_the_design_constants() {
        let m = ChromeMetrics::new(1.0, 16.0);
        assert_eq!(m, ChromeMetrics::DEFAULT);
        assert_eq!(m.bar_h(), 36.0);
        assert_eq!(m.status_h(), 22.0);
        assert_eq!(m.pill_h(), 26.0);
        assert_eq!(m.px(13.0), 13.0);
        assert_eq!(m.strip_pad(), 8.0);
        assert_eq!(m.ctrl_w(), 28.0);
    }

    #[test]
    fn metrics_scale_with_dpi_and_ui_font() {
        // 2× display at the default font: everything doubles.
        let hi = ChromeMetrics::new(2.0, 16.0);
        assert_eq!(hi.bar_h(), 72.0);
        assert_eq!(hi.status_h(), 44.0);
        assert_eq!(hi.strip_pad(), 16.0);
        // 1× display at a 28pt UI font: chrome grows with the text (1.75×),
        // but the window-shape inset stays DPI-only.
        let big = ChromeMetrics::new(1.0, 28.0);
        assert!((big.u - 1.75).abs() < 1e-6);
        assert_eq!(big.bar_h(), 63.0);
        assert_eq!(big.strip_pad(), 8.0);
    }

    #[test]
    fn bogus_inputs_fall_back_to_baseline() {
        for (d, f) in [(0.0, 16.0), (f32::NAN, 16.0), (1.0, 0.0), (1.0, f32::INFINITY), (-2.0, -3.0)] {
            assert_eq!(ChromeMetrics::new(d, f), ChromeMetrics::DEFAULT, "({d}, {f})");
        }
    }

    #[test]
    fn mono_measure_counts_display_cells() {
        let mut m = MonoMeasure(10.0);
        assert_eq!(m.text_w("abc"), 30.0);
        assert_eq!(m.text_w("你好"), 40.0, "wide chars are two cells");
        let mut xs = Vec::new();
        m.char_xs("a你b", false, &mut xs);
        assert_eq!(xs, vec![0.0, 10.0, 30.0, 40.0]);
        assert_eq!(m.text_w(""), 0.0);
    }

    #[test]
    fn fit_head_reserves_the_ellipsis() {
        let mut m = MonoMeasure(10.0);
        assert_eq!(fit_head(&mut m, "short", 50.0, false), "short");
        // 7 chars = 70px > 50: keep 4 chars (40) + ellipsis (10) = 50.
        assert_eq!(fit_head(&mut m, "abcdefg", 50.0, false), "abcd…");
        // Never collapses to a bare ellipsis.
        assert_eq!(fit_head(&mut m, "abcdefg", 5.0, false), "a…");
        // Wide chars are never split across the budget.
        assert_eq!(fit_head(&mut m, "你好世界", 45.0, false), "你…");
    }

    #[test]
    fn fit_tail_keeps_the_end() {
        let mut m = MonoMeasure(10.0);
        assert_eq!(fit_tail(&mut m, "abc", 30.0, false), "abc");
        assert_eq!(fit_tail(&mut m, "abcdefg", 30.0, false), "efg");
        assert_eq!(fit_tail(&mut m, "ab你好", 50.0, false), "b你好");
        assert_eq!(fit_tail(&mut m, "abc", 0.0, false), "");
    }
}
