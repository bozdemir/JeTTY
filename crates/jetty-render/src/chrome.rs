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

/// The OVERLAY surfaces (command palette, help, search bar, hint chips, copy
/// pill, Settings panel) were tuned against a 9.8px reference advance while the
/// default chrome font measures 9.6px, so at the default they have always been
/// drawn at 9.6/9.8 of their design size. That ratio is kept — as a CONSTANT,
/// never re-derived from a font's advance — so the default look is unchanged
/// while the UI-font family can no longer resize them.
pub const OVERLAY_SCALE: f32 = 9.6 / 9.8;

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

    /// Layout scale of the overlay surfaces (palette, help, search bar, hint
    /// chips, copy pill, Settings panel): the chrome unit × [`OVERLAY_SCALE`].
    #[inline]
    pub fn overlay_u(self) -> f32 {
        self.u * OVERLAY_SCALE
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
    /// An implementation may clip `s` to [`MAX_LABEL_CHARS`] chars first (the
    /// GPU layer does — bounded work for program-controlled text); `out` then
    /// covers that prefix, so index it with `get`.
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

/// Most chars a chrome label is ever measured or drawn with. Wider than any
/// chrome surface can show, yet a hard bound on the work and memory a label
/// can cost: chrome draws PROGRAM-controlled text (OSC 0/2 tab titles can be
/// megabytes, pasted search queries, status messages), and shaping or caching
/// such a string whole on every frame is a denial of service. Every measure
/// and draw path clips to this first ([`clip_head`] / [`clip_tail`]).
pub const MAX_LABEL_CHARS: usize = 256;

/// The first [`MAX_LABEL_CHARS`] chars of `s` (char-boundary safe, O(bound) —
/// never a scan of a huge `s`) and whether anything was cut.
pub fn clip_head(s: &str) -> (&str, bool) {
    match s.char_indices().nth(MAX_LABEL_CHARS) {
        Some((i, _)) => (&s[..i], true),
        None => (s, false),
    }
}

/// The last [`MAX_LABEL_CHARS`] chars of `s` (char-boundary safe, O(bound))
/// and whether anything was cut.
pub fn clip_tail(s: &str) -> (&str, bool) {
    match s.char_indices().rev().nth(MAX_LABEL_CHARS - 1) {
        Some((i, _)) if i > 0 => (&s[i..], true),
        _ => (s, false),
    }
}

/// Longest head of `s` that fits `max_w` px, with [`ELLIPSIS`] appended (its
/// width reserved) when anything was cut. At least one char is kept so a
/// truncated label never collapses to a bare ellipsis. Only the first
/// [`MAX_LABEL_CHARS`] chars are ever measured; a longer `s` is always cut.
pub fn fit_head(m: &mut dyn ChromeMeasure, s: &str, max_w: f32, title: bool) -> String {
    let (s, clipped) = clip_head(s);
    let full = if title { m.title_w(s) } else { m.text_w(s) };
    if full <= max_w && !clipped {
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
/// must stay visible, e.g. a query next to its caret). Only the last
/// [`MAX_LABEL_CHARS`] chars are ever measured.
pub fn fit_tail(m: &mut dyn ChromeMeasure, s: &str, max_w: f32, title: bool) -> String {
    let (s, _) = clip_tail(s);
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

/// Byte budget of ONE chrome-measurement cache (keys + values). Chrome shows a
/// few hundred short labels; the budget only bounds an adversarial stream of
/// distinct strings (a program cycling huge OSC titles every frame).
pub(crate) const MEASURE_CACHE_BUDGET: usize = 256 * 1024;

/// Byte-budgeted cache of chrome measurements keyed by label text, GPU-free so
/// its bounds are unit-tested. A key longer than [`MAX_LABEL_CHARS`] chars is
/// never stored (callers clip first; this is the backstop), and once the
/// stored bytes would exceed [`MEASURE_CACHE_BUDGET`] the cache is simply
/// cleared — so memory stays bounded however many distinct labels arrive.
#[derive(Debug, Default)]
pub(crate) struct MeasureCache<V> {
    map: std::collections::HashMap<String, V>,
    bytes: usize,
}

impl<V> MeasureCache<V> {
    pub(crate) fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    /// Store `value` (whose heap size is `value_bytes`) under `key`.
    pub(crate) fn insert(&mut self, key: &str, value: V, value_bytes: usize) {
        if key.len() > MAX_LABEL_CHARS * 4 {
            return; // longer than any clipped label can be: never cached
        }
        // Per-entry overhead (String header + hash slot) folded into the cost.
        let cost = key.len() + value_bytes + 64;
        if self.bytes + cost > MEASURE_CACHE_BUDGET {
            self.clear();
        }
        self.bytes += cost;
        self.map.insert(key.to_string(), value);
    }

    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// Bytes currently accounted (keys + values + overhead).
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measurer that records the longest input it was ever asked to measure,
    /// so tests can prove huge program-controlled text never reaches shaping.
    struct Spy {
        inner: MonoMeasure,
        max_chars_seen: usize,
        calls: usize,
    }
    impl ChromeMeasure for Spy {
        fn char_xs(&mut self, s: &str, title: bool, out: &mut Vec<f32>) {
            self.calls += 1;
            self.max_chars_seen = self.max_chars_seen.max(s.chars().count());
            self.inner.char_xs(s, title, out);
        }
    }

    #[test]
    fn huge_program_text_is_clipped_before_measuring() {
        // A 1 MiB "title" (an OSC 0/2 can carry megabytes): fitting it must
        // never shape more than MAX_LABEL_CHARS chars, and the result is a
        // short, ellipsized label.
        let huge = "x".repeat(1 << 20);
        let mut spy = Spy { inner: MonoMeasure(10.0), max_chars_seen: 0, calls: 0 };
        let head = fit_head(&mut spy, &huge, 1.0e9, true);
        assert!(head.ends_with(ELLIPSIS), "a clipped label is always marked as cut");
        assert!(head.chars().count() <= MAX_LABEL_CHARS + 1);
        let tail = fit_tail(&mut spy, &huge, 1.0e9, false);
        assert!(tail.chars().count() <= MAX_LABEL_CHARS);
        assert!(spy.max_chars_seen <= MAX_LABEL_CHARS, "shaped {} chars", spy.max_chars_seen);
        assert!(spy.calls <= 6, "bounded work per fit, got {} calls", spy.calls);
    }

    #[test]
    fn clips_are_char_boundary_safe_and_bounded() {
        let s = "ğ".repeat(1000); // 2-byte chars
        let (h, cut) = clip_head(&s);
        assert!(cut && h.chars().count() == MAX_LABEL_CHARS);
        let (t, cut) = clip_tail(&s);
        assert!(cut && t.chars().count() == MAX_LABEL_CHARS);
        assert_eq!(clip_head("short"), ("short", false));
        assert_eq!(clip_tail("short"), ("short", false));
        assert_eq!(clip_tail(""), ("", false));
    }

    #[test]
    fn measure_cache_stays_within_its_byte_budget() {
        // Thousands of distinct, maximal (clipped) labels — e.g. a program
        // cycling huge titles — never grow the cache past its budget.
        let mut c: MeasureCache<Vec<f32>> = MeasureCache::default();
        // One 1 MiB buffer whose first 6 bytes become a counter: thousands of
        // DISTINCT 1 MiB titles without allocating a new one per iteration.
        let mut big = "y".repeat(1 << 20);
        for i in 0..5000 {
            big.replace_range(0..6, &format!("{i:06}"));
            let (key, cut) = clip_head(&big);
            assert!(cut);
            let v = vec![0.0f32; MAX_LABEL_CHARS + 1];
            c.insert(key, v, (MAX_LABEL_CHARS + 1) * 4);
            assert!(c.bytes() <= MEASURE_CACHE_BUDGET, "cache grew to {} bytes", c.bytes());
        }
        assert!(c.len() > 0, "recent entries are still cached");
        // An unclipped (over-long) key is never stored at all.
        let before = c.len();
        c.insert(&"z".repeat(1 << 20), Vec::new(), 0);
        assert_eq!(c.len(), before);
    }

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
