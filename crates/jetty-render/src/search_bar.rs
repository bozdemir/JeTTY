use crate::chrome::{fit_tail, ChromeMeasure, ChromeMetrics, CHROME_ADVANCE};
use crate::colors::contrast_ratio;
use crate::quad::SCROLLBAR_W;
use crate::ui_palette::{mix, rgba, UiPalette};
use crate::Rect;

/// Geometry + draw data for the scrollback-search bar (Ctrl+Shift+F): a
/// rounded themed pill anchored to the top-right of the grid with the query,
/// a static caret, the "current/total" counter, and a ✕ close button.
pub struct SearchBar {
    /// Quads in draw order: border, background panel, caret.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — "Find: query", counter, ✕.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The panel rect (clicks inside are swallowed; outside falls through).
    pub panel: Rect,
    /// Hit area of the ✕ close button.
    pub close_rect: Rect,
}

/// Right inset between the bar and the scrollbar gutter / window edge.
const RIGHT_GAP: f32 = 8.0;

/// Build the search bar for a window `win_w` px wide with the grid starting
/// at `grid_top` (both physical px). All colors come from the theme's
/// [`UiPalette`] (same surface language as help.rs) — no hardcoded RGB. All
/// metrics scale with the chrome unit `cm` (DPI × UI font), and the prefix,
/// query and counter are MEASURED with `m` — so the caret hugs the last glyph
/// and wide (CJK) or proportional text never overlaps the counter (F8). A long
/// query is TAIL-truncated so the caret end is always visible; the whole bar
/// clamps to `win_w - SCROLLBAR_W - 16` so it fits narrow windows.
#[allow(clippy::too_many_arguments)]
pub fn build_search_bar(
    win_w: u32,
    grid_top: f32,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    query: &str,
    current: usize,
    total: usize,
) -> SearchBar {
    let ui = UiPalette::cached(theme);
    let panel_bg = rgba(ui.surface, 242);
    let border_col = rgba(ui.border, 255);
    let text_col = ui.text_dim;

    // HiDPI × UI-font scale (the overlay unit; ≈0.983 at 1×/16pt, see `OVERLAY_SCALE`).
    let vscale = cm.overlay_u();
    let bar_h = 34.0 * vscale;
    let pad = 10.0 * vscale;
    let caret_w = 2.0;
    let caret_gap = 2.0;
    let close_w = 28.0 * vscale;

    // Counter text: "cur/total", "cur/5000+" at the match cap, "0/0" (dimmed)
    // when there is no match.
    let counter = if total == 0 {
        "0/0".to_string()
    } else if total >= jetty_core::SEARCH_MAX_MATCHES {
        format!("{current}/{}+", jetty_core::SEARCH_MAX_MATCHES)
    } else {
        format!("{current}/{total}")
    };
    let counter_col = if total == 0 { ui.text_hint } else { ui.text };

    // All text is MEASURED as the chrome layer draws it: CJK/fullwidth query
    // glyphs (explicitly supported via IME commits) render ~2× a monospace
    // cell and a proportional UI font is no cell grid at all — estimating made
    // them overflow the pill and overlap the counter/✕ (F8).
    const PREFIX: &str = "Find: ";
    let prefix_w = m.text_w(PREFIX);
    let gap = cm.px(CHROME_ADVANCE); // one chrome char between query/counter/close
    let counter_w = m.text_w(&counter);
    // Everything except the query text itself.
    let fixed_w = pad + prefix_w + caret_gap + caret_w + gap + counter_w + gap + close_w + pad;

    // Clamp the bar to the window, keeping clear of the scrollbar gutter.
    let max_bar_w = (win_w as f32 - SCROLLBAR_W - 16.0).max(0.0);
    // Tail-truncate the query: show the LAST chars that fit, so the end the
    // user is typing at stays visible next to the caret.
    let shown = fit_tail(m, query, (max_bar_w - fixed_w).max(0.0), false);
    // The prefix + query are ONE drawn label; measure it whole (kerning across
    // the join included) so the caret lands exactly after the last glyph.
    let label = format!("{PREFIX}{shown}");
    let label_w = m.text_w(&label);
    let shown_w = (label_w - prefix_w).max(0.0);
    let bar_w = (fixed_w + shown_w).min(max_bar_w).max(0.0);

    let x = (win_w as f32 - bar_w - SCROLLBAR_W - RIGHT_GAP).max(0.0);
    let y = grid_top + 8.0;

    let mut quads: Vec<Rect> = Vec::new();
    // Border + background, same rounded idiom as the help overlay.
    quads.push(Rect::rounded(
        (x - 2.0).max(0.0), (y - 2.0).max(0.0), bar_w + 4.0, bar_h + 4.0, border_col, 10.0,
    ));
    let panel = Rect::rounded(x, y, bar_w, bar_h, panel_bg, 8.0);
    quads.push(panel);

    // Chrome text line box (the UI font's em); center it vertically.
    let text_h = 16.0 * vscale;
    let text_y = y + (bar_h - text_h) / 2.0;

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    labels.push((label, x + pad, text_y, text_col));

    // Static caret right after the query text (no animation — the bar never
    // self-drives frames).
    let caret_x = x + pad + label_w + caret_gap;
    quads.push(Rect::new(caret_x, text_y, caret_w, text_h, rgba(ui.text, 255)));

    // Counter, right-aligned against the close button.
    let close_x = x + bar_w - pad - close_w;
    let counter_x = close_x - gap - counter_w;
    labels.push((counter, counter_x, text_y, counter_col));

    // ✕ close button (label centered in its square hit area).
    let close_rect = Rect::new(close_x, y + (bar_h - close_w) / 2.0, close_w, close_w, [0, 0, 0, 0]);
    let close_glyph_w = m.text_w("✕");
    labels.push((
        "✕".to_string(),
        close_x + (close_w - close_glyph_w) / 2.0,
        text_y,
        text_col,
    ));

    SearchBar { quads, labels, panel, close_rect }
}

/// Weight of the CURRENT match's fill (bg → ANSI yellow): strong, so it reads
/// as "you are here"; its glyphs are recolored to stay readable on it
/// ([`search_recolor_spans`]).
const CURRENT_HIT_T: f32 = 0.85;
/// Weight of every other match's tint, and the least it backs off to.
const HIT_T: f32 = 0.45;
const HIT_T_MIN: f32 = 0.25;
/// The theme fg must keep this contrast on an ordinary match's tint (its
/// glyphs keep their own colors there).
const HIT_TEXT_FLOOR: f32 = 3.0;

/// `(ordinary, current)` match fills. The ordinary tint backs off from
/// [`HIT_T`] (to [`HIT_T_MIN`] at most) only while the theme fg would fall
/// under 3:1 on it — a pale yellow (Poimandres) used to leave 1.8:1.
fn hit_fills(theme: &jetty_core::Theme) -> ([u8; 3], [u8; 3]) {
    let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
    let yellow = theme.palette[3];
    let mut t = HIT_T;
    while t > HIT_T_MIN && contrast_ratio(theme.fg, mix(bg, yellow, t)) < HIT_TEXT_FLOOR {
        t -= 0.01;
    }
    (mix(bg, yellow, t.max(HIT_T_MIN)), mix(bg, yellow, CURRENT_HIT_T))
}

/// The glyph color inside the CURRENT match: readable (≥ 4.5:1) on its strong
/// fill — the theme bg/fg when one reaches it, else black/white.
pub fn search_current_fg(theme: &jetty_core::Theme) -> [u8; 3] {
    UiPalette::cached(theme).on_fill(hit_fills(theme).1)
}

/// The [`crate::GridPaint::recolor`] spans for the visible `hits`: the CURRENT
/// match's segments in [`search_current_fg`], sorted by `(row, first col)`.
/// Shared by app.rs and jetty-shot so both render identically.
pub fn search_recolor_spans(
    hits: &[jetty_core::SearchHit],
    theme: &jetty_core::Theme,
) -> Vec<(usize, usize, usize, [u8; 3])> {
    if !hits.iter().any(|h| h.is_current) {
        return Vec::new();
    }
    let fg = search_current_fg(theme);
    let mut spans: Vec<(usize, usize, usize, [u8; 3])> = hits
        .iter()
        .filter(|h| h.is_current && h.col_end >= h.col_start)
        .map(|h| (h.row, h.col_start, h.col_end, fg))
        .collect();
    spans.sort_unstable_by_key(|s| (s.0, s.1));
    spans
}

/// Background highlight rects for the visible search matches: every hit gets
/// a bg→palette[3] (yellow) tint, the CURRENT match a much stronger fill (with
/// its glyphs recolored — [`search_recolor_spans`]). Opaque (alpha 255) like
/// the selection rects; appended AFTER them so the match tint wins. Shared by
/// app.rs and jetty-shot so both render identically.
pub fn search_hit_rects(
    hits: &[jetty_core::SearchHit],
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    theme: &jetty_core::Theme,
) -> Vec<Rect> {
    let (normal, current) = hit_fills(theme);
    let (normal, current) = (rgba(normal, 255), rgba(current, 255));
    hits.iter()
        .map(|h| {
            Rect::new(
                h.col_start as f32 * cell_w,
                h.row as f32 * cell_h + y_offset,
                (h.col_end.saturating_sub(h.col_start) + 1) as f32 * cell_w,
                cell_h,
                if h.is_current { current } else { normal },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 chrome advance used by the layout tests (matches help.rs).
    const TEST_CHAR_W: f32 = 9.8;
    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    /// Display width in monospace cells (wide = 2), as `MonoMeasure` counts.
    fn display_cells(s: &str) -> usize {
        s.chars().map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)).sum()
    }

    #[test]
    fn bar_fits_at_all_widths() {
        for w in [320u32, 500, 700, 1000, 1600] {
            let sb = build_search_bar(w, 36.0, &theme(), &mut mono(), CM, "some longish query text", 3, 17);
            assert!(sb.panel.x >= 0.0, "panel off-screen left at width {w}");
            let right_inset = w as f32 - (sb.panel.x + sb.panel.w);
            assert!(
                right_inset >= SCROLLBAR_W,
                "bar overlaps the scrollbar gutter at width {w}: inset {right_inset}"
            );
        }
    }

    #[test]
    fn close_rect_inside_panel() {
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "query", 1, 2);
        let p = &sb.panel;
        let c = &sb.close_rect;
        assert!(c.x >= p.x && c.x + c.w <= p.x + p.w + 0.5, "✕ outside panel horizontally");
        assert!(c.y >= p.y && c.y + c.h <= p.y + p.h + 0.5, "✕ outside panel vertically");
    }

    #[test]
    fn long_query_tail_truncated() {
        let long: String = "abcdefghij".repeat(30); // 300 chars
        let sb = build_search_bar(500, 36.0, &theme(), &mut mono(), CM, &long, 1, 1);
        let panel_right = sb.panel.x + sb.panel.w;
        for (text, x, _y, _c) in &sb.labels {
            let est_right = x + text.chars().count() as f32 * TEST_CHAR_W;
            assert!(
                est_right <= panel_right + 0.5,
                "label {text:?} overflows the panel: {est_right} > {panel_right}"
            );
        }
        // The visible query is the TAIL of the input (the caret end).
        let find = sb.labels.iter().find(|l| l.0.starts_with("Find: ")).unwrap();
        assert!(
            long.ends_with(find.0.trim_start_matches("Find: ")),
            "shown query must be the tail of the full query"
        );
    }

    #[test]
    fn cjk_query_measured_by_display_width() {
        // F8: an 8-char CJK query renders ~16 monospace cells wide; the bar
        // must budget for that, not for 8. The caret sits AFTER the glyphs
        // and the query text never reaches the counter.
        let q = "エラーメッセージ"; // 8 chars, 16 cells
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, q, 1, 2);
        let find = sb.labels.iter().find(|l| l.0.starts_with("Find: ")).unwrap();
        let est_right = find.1 + display_cells(&find.0) as f32 * TEST_CHAR_W;
        let panel_right = sb.panel.x + sb.panel.w;
        assert!(
            est_right <= panel_right + 0.5,
            "CJK query text overflows the panel: {est_right} > {panel_right}"
        );
        // Caret must clear the full display width of the shown query.
        let caret = sb
            .quads
            .iter()
            .find(|r| r.w == 2.0)
            .expect("caret quad (2px wide) present");
        assert!(
            caret.x >= est_right - 0.5,
            "caret {x} sits mid-glyph (query text ends at {est_right})",
            x = caret.x
        );
        // The counter starts after the caret, not under the query glyphs.
        let counter = sb.labels.iter().find(|l| l.0 == "1/2").unwrap();
        assert!(
            counter.1 >= caret.x,
            "counter at {c} overlaps the query/caret at {x}",
            c = counter.1,
            x = caret.x
        );
    }

    #[test]
    fn cjk_long_query_tail_truncated_by_display_width() {
        // 150 wide chars = 300 cells — far beyond a 500px window's budget.
        let long: String = "エラー検索".repeat(30);
        let sb = build_search_bar(500, 36.0, &theme(), &mut mono(), CM, &long, 1, 1);
        let panel_right = sb.panel.x + sb.panel.w;
        for (text, x, _y, _c) in &sb.labels {
            let est_right = x + display_cells(text) as f32 * TEST_CHAR_W;
            assert!(
                est_right <= panel_right + 0.5,
                "label {text:?} overflows the panel: {est_right} > {panel_right}"
            );
        }
        // Still the TAIL of the query (the caret end the user types at).
        let find = sb.labels.iter().find(|l| l.0.starts_with("Find: ")).unwrap();
        let shown = find.0.trim_start_matches("Find: ");
        assert!(!shown.is_empty(), "some tail of the query must be shown");
        assert!(long.ends_with(shown), "shown query must be the tail of the input");
    }

    #[test]
    fn counter_shows_cur_slash_total() {
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "q", 3, 17);
        assert!(sb.labels.iter().any(|l| l.0 == "3/17"), "counter 3/17 missing");
        // 0/0 on no match.
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "q", 0, 0);
        assert!(sb.labels.iter().any(|l| l.0 == "0/0"), "counter 0/0 missing");
        // capped total renders with a trailing '+'.
        let cap = jetty_core::SEARCH_MAX_MATCHES;
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "q", 1, cap);
        assert!(
            sb.labels.iter().any(|l| l.0 == format!("1/{cap}+")),
            "capped counter must show {cap}+"
        );
    }

    #[test]
    fn hit_rects_current_differs() {
        let hits = [
            jetty_core::SearchHit { row: 0, col_start: 0, col_end: 4, is_current: false },
            jetty_core::SearchHit { row: 1, col_start: 2, col_end: 6, is_current: true },
        ];
        let t = theme();
        let rects = search_hit_rects(&hits, 8.0, 16.0, 0.0, &t);
        assert_eq!(rects.len(), 2);
        assert_ne!(rects[0].color, rects[1].color, "current hit must render differently");
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        for r in &rects {
            assert_ne!([r.color[0], r.color[1], r.color[2]], bg, "hit tint must differ from bg");
        }
        // Geometry: row 1, cols 2..=6 at 8x16 cells with a 36px offset.
        let r = &rects[1];
        assert_eq!((r.x, r.y, r.w, r.h), (16.0, 16.0, 40.0, 16.0));
    }

    #[test]
    fn the_current_match_stays_readable_on_every_theme() {
        // Its glyphs used to keep their colors on the 85% yellow fill: 1.05–2.3:1.
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let (normal, current) = hit_fills(&t);
            let fg = search_current_fg(&t);
            assert!(contrast_ratio(fg, current) >= 4.5, "{}: current match glyphs", t.name);
            // Ordinary hits keep the theme fg readable (or sit at the minimum tint).
            let bg = [t.bg[0], t.bg[1], t.bg[2]];
            assert!(
                contrast_ratio(t.fg, normal) >= 3.0 || normal == mix(bg, t.palette[3], HIT_T_MIN),
                "{}: fg on an ordinary hit {}",
                t.name,
                contrast_ratio(t.fg, normal)
            );
            // The current match still stands out from an ordinary one.
            assert_ne!(normal, current, "{}", t.name);
        }
    }

    #[test]
    fn ordinary_hits_keep_todays_tint_where_it_already_read() {
        // Catppuccin Mocha's fg reads ≥3:1 on the 45% tint → unchanged.
        let t = theme();
        let bg = [t.bg[0], t.bg[1], t.bg[2]];
        assert_eq!(hit_fills(&t).0, mix(bg, t.palette[3], 0.45));
        // Poimandres' pale yellow → the tint backs off until fg reads.
        let p = jetty_core::Theme::by_name("poimandres");
        assert!(contrast_ratio(p.fg, hit_fills(&p).0) >= 3.0);
    }

    #[test]
    fn recolor_spans_cover_only_the_current_match_in_order() {
        let t = theme();
        let hits = [
            jetty_core::SearchHit { row: 3, col_start: 0, col_end: 2, is_current: true },
            jetty_core::SearchHit { row: 1, col_start: 4, col_end: 9, is_current: false },
            jetty_core::SearchHit { row: 2, col_start: 70, col_end: 79, is_current: true },
        ];
        let spans = search_recolor_spans(&hits, &t);
        let fg = search_current_fg(&t);
        assert_eq!(spans, vec![(2, 70, 79, fg), (3, 0, 2, fg)], "current segments, sorted");
        let none = [jetty_core::SearchHit { row: 0, col_start: 0, col_end: 3, is_current: false }];
        assert!(search_recolor_spans(&none, &t).is_empty());
    }

    #[test]
    fn bar_text_is_readable_on_every_theme() {
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let sb = build_search_bar(1000, 36.0, &t, &mut mono(), CM, "q", 0, 0);
            let surface = [sb.panel.color[0], sb.panel.color[1], sb.panel.color[2]];
            for (text, _, _, c) in &sb.labels {
                assert!(contrast_ratio(*c, surface) >= 3.0, "{}: {text:?} {}", t.name, contrast_ratio(*c, surface));
            }
        }
    }

    #[test]
    fn bar_scales_with_chrome_metrics() {
        // HiDPI: a 2× chrome unit doubles the bar height/paddings.
        let sb1 = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "q", 1, 1);
        let sb2 = build_search_bar(
            1000, 36.0, &theme(), &mut MonoMeasure(19.6), ChromeMetrics::new(2.0, 16.0), "q", 1, 1,
        );
        assert!((sb2.panel.h - sb1.panel.h * 2.0).abs() < 0.01, "bar height must scale with the chrome unit");
    }

    #[test]
    fn caret_hugs_the_measured_label() {
        // The caret sits caret_gap (2px) after the MEASURED "Find: query" label,
        // whatever the font's per-char widths are.
        let sb = build_search_bar(1000, 36.0, &theme(), &mut mono(), CM, "abc", 1, 1);
        let find = sb.labels.iter().find(|l| l.0 == "Find: abc").unwrap();
        let caret = sb.quads.iter().find(|r| r.w == 2.0).unwrap();
        assert!((caret.x - (find.1 + mono().text_w("Find: abc") + 2.0)).abs() < 0.01);
    }
}
