use crate::quad::SCROLLBAR_W;
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
/// at `grid_top` (both physical px). All colors derive from the theme's
/// bg→fg lerp (same surface language as help.rs) — no hardcoded RGB. All
/// metrics scale with the measured chrome-font advance `char_w` via the
/// char_w/9.8 vscale idiom, so the bar is HiDPI-correct. A long query is
/// TAIL-truncated so the caret end is always visible; the whole bar clamps
/// to `win_w - SCROLLBAR_W - 16` so it fits narrow windows.
pub fn build_search_bar(
    win_w: u32,
    grid_top: f32,
    theme: &jetty_core::Theme,
    char_w: f32,
    query: &str,
    current: usize,
    total: usize,
) -> SearchBar {
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
    let text_col = lerp(0.70);

    // HiDPI scale from the measured chrome advance (9.8px at scale 1).
    let vscale = (char_w / 9.8).max(0.1);
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
    let counter_col = if total == 0 { lerp(0.45) } else { tfg };

    const PREFIX: &str = "Find: ";
    let prefix_w = PREFIX.chars().count() as f32 * char_w;
    let gap = char_w; // one chrome char between query/counter/close
    let counter_w = counter.chars().count() as f32 * char_w;
    // Everything except the query text itself.
    let fixed_w = pad + prefix_w + caret_gap + caret_w + gap + counter_w + gap + close_w + pad;

    // Clamp the bar to the window, keeping clear of the scrollbar gutter.
    let max_bar_w = (win_w as f32 - SCROLLBAR_W - 16.0).max(0.0);
    // Tail-truncate the query: show the LAST chars that fit, so the end the
    // user is typing at stays visible next to the caret.
    let query_chars = query.chars().count();
    let avail_chars = (((max_bar_w - fixed_w) / char_w).floor().max(0.0)) as usize;
    let shown: String = if query_chars > avail_chars {
        query.chars().skip(query_chars - avail_chars).collect()
    } else {
        query.to_string()
    };
    let shown_w = shown.chars().count() as f32 * char_w;
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

    // Chrome text line box is ~16px at scale 1; center it vertically.
    let text_h = 16.0 * vscale;
    let text_y = y + (bar_h - text_h) / 2.0;

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    labels.push((format!("{PREFIX}{shown}"), x + pad, text_y, text_col));

    // Static caret right after the query text (no animation — the bar never
    // self-drives frames).
    let caret_x = x + pad + prefix_w + shown_w + caret_gap;
    quads.push(Rect::new(caret_x, text_y, caret_w, text_h, [tfg[0], tfg[1], tfg[2], 255]));

    // Counter, right-aligned against the close button.
    let close_x = x + bar_w - pad - close_w;
    let counter_x = close_x - gap - counter_w;
    labels.push((counter, counter_x, text_y, counter_col));

    // ✕ close button (label centered in its square hit area).
    let close_rect = Rect::new(close_x, y + (bar_h - close_w) / 2.0, close_w, close_w, [0, 0, 0, 0]);
    labels.push((
        "✕".to_string(),
        close_x + (close_w - char_w) / 2.0,
        text_y,
        text_col,
    ));

    SearchBar { quads, labels, panel, close_rect }
}

/// Background highlight rects for the visible search matches: every hit gets
/// a bg→palette[3] (yellow) blend, the CURRENT match a much stronger one.
/// Opaque (alpha 255) like the selection rects; appended AFTER them so the
/// match tint wins. Shared by app.rs and jetty-shot so both render identically.
pub fn search_hit_rects(
    hits: &[jetty_core::SearchHit],
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    theme: &jetty_core::Theme,
) -> Vec<Rect> {
    let bg = theme.bg;
    let accent = theme.palette[3];
    let blend = |t: f32| -> [u8; 4] {
        [
            (bg[0] as f32 + (accent[0] as f32 - bg[0] as f32) * t).round() as u8,
            (bg[1] as f32 + (accent[1] as f32 - bg[1] as f32) * t).round() as u8,
            (bg[2] as f32 + (accent[2] as f32 - bg[2] as f32) * t).round() as u8,
            255,
        ]
    };
    let normal = blend(0.45);
    let current = blend(0.85);
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

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 chrome advance used by the layout tests (matches help.rs).
    const TEST_CHAR_W: f32 = 9.8;

    #[test]
    fn bar_fits_at_all_widths() {
        for w in [320u32, 500, 700, 1000, 1600] {
            let sb = build_search_bar(w, 36.0, &theme(), TEST_CHAR_W, "some longish query text", 3, 17);
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
        let sb = build_search_bar(1000, 36.0, &theme(), TEST_CHAR_W, "query", 1, 2);
        let p = &sb.panel;
        let c = &sb.close_rect;
        assert!(c.x >= p.x && c.x + c.w <= p.x + p.w + 0.5, "✕ outside panel horizontally");
        assert!(c.y >= p.y && c.y + c.h <= p.y + p.h + 0.5, "✕ outside panel vertically");
    }

    #[test]
    fn long_query_tail_truncated() {
        let long: String = "abcdefghij".repeat(30); // 300 chars
        let sb = build_search_bar(500, 36.0, &theme(), TEST_CHAR_W, &long, 1, 1);
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
    fn counter_shows_cur_slash_total() {
        let sb = build_search_bar(1000, 36.0, &theme(), TEST_CHAR_W, "q", 3, 17);
        assert!(sb.labels.iter().any(|l| l.0 == "3/17"), "counter 3/17 missing");
        // 0/0 on no match.
        let sb = build_search_bar(1000, 36.0, &theme(), TEST_CHAR_W, "q", 0, 0);
        assert!(sb.labels.iter().any(|l| l.0 == "0/0"), "counter 0/0 missing");
        // capped total renders with a trailing '+'.
        let cap = jetty_core::SEARCH_MAX_MATCHES;
        let sb = build_search_bar(1000, 36.0, &theme(), TEST_CHAR_W, "q", 1, cap);
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
    fn bar_scales_with_char_w() {
        // HiDPI: a 2× chrome advance doubles the bar height/paddings.
        let sb1 = build_search_bar(1000, 36.0, &theme(), 9.8, "q", 1, 1);
        let sb2 = build_search_bar(1000, 36.0, &theme(), 19.6, "q", 1, 1);
        assert!((sb2.panel.h - sb1.panel.h * 2.0).abs() < 0.01, "bar height must scale with char_w");
    }
}
