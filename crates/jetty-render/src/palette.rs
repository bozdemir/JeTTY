use crate::search_bar::{char_cells, display_cells};
use crate::Rect;

/// Maximum number of result rows visible in the palette at once (the scroll
/// window). Shared with jetty-app so its scroll/PageUp-Down math and this
/// builder's visible-slice assumption stay in lockstep.
pub const MAX_PALETTE_ROWS: usize = 9;

/// One result row handed to [`build_command_palette`]: a (possibly already
/// tail/head-truncated) title, the matched CHARACTER indices into that title
/// (for the accent highlight), and whether it is the selected row.
pub struct PaletteRow<'a> {
    pub title: &'a str,
    pub match_indices: &'a [usize],
    pub selected: bool,
}

/// Geometry + draw data for the command-palette overlay.
pub struct CommandPalette {
    /// Quads in draw order: full-screen dim, border, panel, selection highlight,
    /// input divider, caret, scrollbar thumb.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — the input line, the counter, each row
    /// title, and the per-matched-char accent overlays.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The panel rect (clicks inside are swallowed; outside closes).
    pub panel: Rect,
    /// Per-visible-row hit rects (top→bottom) for future mouse hover/click.
    pub row_hits: Vec<Rect>,
}

/// Build the centered, HiDPI, theme-derived command palette for a window of
/// `win_w`×`win_h` physical pixels. Mirrors `help.rs`/`search_bar.rs`: all
/// colours blend the theme's bg→fg (no hardcoded RGB), all metrics scale with
/// the measured chrome advance `char_w` via the `char_w/9.8` vscale idiom, and
/// text is measured in DISPLAY cells so wide (CJK) theme/tab titles never
/// overflow. `rows` is the already-scrolled VISIBLE slice (≤ `MAX_PALETTE_ROWS`);
/// `first_visible` + `total_matches` drive the scrollbar thumb.
#[allow(clippy::too_many_arguments)]
pub fn build_command_palette(
    win_w: u32,
    win_h: u32,
    theme: &jetty_core::Theme,
    char_w: f32,
    query: &str,
    rows: &[PaletteRow],
    total_matches: usize,
    first_visible: usize,
) -> CommandPalette {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived chrome (identical language to help.rs / search_bar.rs) ---
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
    let sel3 = lerp(0.18);
    let sel_bg: [u8; 4] = [sel3[0], sel3[1], sel3[2], 255];
    let input_col = tfg; // typed query
    let placeholder_col = lerp(0.45);
    let row_col = lerp(0.70); // unmatched title text, unselected row
    let counter_col = lerp(0.45);
    let accent = theme.palette[4]; // matched-char highlight
    let caret_col: [u8; 4] = [tfg[0], tfg[1], tfg[2], 255];
    let thumb3 = lerp(0.40);
    let thumb_col: [u8; 4] = [thumb3[0], thumb3[1], thumb3[2], 255];

    // --- Vertical metrics (scale with DPI; floored so a short window still fits) ---
    let vscale = (char_w / 9.8).max(0.1);
    let text_h = 16.0 * vscale;
    let pad_v = (14.0 * vscale).max(6.0);
    let input_h = (34.0 * vscale).max(24.0);
    let div_h = 1.0;
    let n = rows.len();
    let mut row_h = (28.0 * vscale).max(16.0);

    let avail_h = sh.max(0.0);
    let ideal_h = 2.0 * pad_v + input_h + div_h + n as f32 * row_h;
    // Shrink the row pitch (last-resort) so every visible row fits a short window.
    if ideal_h > avail_h && n > 0 {
        row_h = ((avail_h - 2.0 * pad_v - input_h - div_h) / n as f32).clamp(1.0, row_h);
    }
    let panel_h = (2.0 * pad_v + input_h + div_h + n as f32 * row_h).min(avail_h);

    // --- Horizontal metrics: a spotlight-width box, content tail/head-truncated ---
    const MARGIN: f32 = 16.0;
    let max_w = (sw - MARGIN * 2.0).max(0.0);
    let want_lo = (420.0 * vscale).min(max_w);
    let panel_w = (sw * 0.6).clamp(want_lo, max_w).max(0.0);
    let pad_x = (16.0 * vscale).min(panel_w * 0.12).max(4.0);
    let content_w = (panel_w - 2.0 * pad_x).max(char_w);
    // Total content cells available on one line.
    let content_cells = (content_w / char_w).floor().max(1.0) as usize;

    // Anchor the box slightly above centre (spotlight feel), clamped on-screen.
    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = (sh * 0.14).min((sh - panel_h).max(0.0)).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut row_hits: Vec<Rect> = Vec::new();

    // Full-screen dim so the palette reads as modal.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: [0, 0, 0, 150], ..Default::default() });
    // Border + panel (rounded, matching the window/tab frame).
    quads.push(Rect::rounded(
        (px - 2.0).max(0.0),
        (py - 2.0).max(0.0),
        panel_w + 4.0,
        panel_h + 4.0,
        border_col,
        10.0,
    ));
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, 8.0);
    quads.push(panel);

    let text_x = px + pad_x;

    // --- Input line: "> query" + static caret + right-aligned counter ---
    const PROMPT: &str = "> ";
    let prompt_cells = display_cells(PROMPT);
    let counter = if total_matches == 0 {
        "no matches".to_string()
    } else if total_matches == 1 {
        "1 result".to_string()
    } else {
        format!("{total_matches} results")
    };
    let counter_cells = display_cells(&counter);

    let input_y = py + pad_v;
    let input_text_y = input_y + (input_h - text_h) / 2.0;

    // Budget for the query text: content minus prompt, caret+gap, and counter+gap.
    let reserve = prompt_cells + 2 + counter_cells + 1;
    let query_budget = content_cells.saturating_sub(reserve);
    // Tail-truncate the query (keep the caret end visible, like the search bar).
    let shown_query = tail_fit(query, query_budget);
    let shown_query_cells = display_cells(&shown_query);

    let input_text = if query.is_empty() {
        format!("{PROMPT}Type a command…")
    } else {
        format!("{PROMPT}{shown_query}")
    };
    let input_text_col = if query.is_empty() { placeholder_col } else { input_col };
    labels.push((input_text, text_x, input_text_y, input_text_col));

    // Static caret right after the query text (bar never self-drives frames).
    let caret_x = text_x + (prompt_cells + shown_query_cells) as f32 * char_w + 2.0;
    quads.push(Rect::new(caret_x, input_text_y, 2.0, text_h, caret_col));

    // Counter, right-aligned against the right padding.
    let counter_x = px + panel_w - pad_x - counter_cells as f32 * char_w;
    labels.push((counter, counter_x, input_text_y, counter_col));

    // Divider between the input line and the result rows.
    let divider_y = input_y + input_h;
    quads.push(Rect::new(text_x, divider_y, content_w, div_h, border_col));

    // --- Result rows ---
    let rows_top = divider_y + div_h;
    for (i, row) in rows.iter().enumerate() {
        let row_top = rows_top + i as f32 * row_h;
        let row_text_y = row_top + (row_h - text_h) / 2.0;
        row_hits.push(Rect::new(px, row_top, panel_w, row_h, [0, 0, 0, 0]));

        // Selected-row highlight behind the text.
        if row.selected {
            let sel_x = px + 4.0 * vscale;
            let sel_w = (panel_w - 8.0 * vscale).max(0.0);
            quads.push(Rect::rounded(sel_x, row_top, sel_w, row_h, sel_bg, 6.0));
        }

        // Head-truncate the title to the content width (append … when it overflows),
        // and keep only the matched indices that survive inside the visible head.
        let (shown_title, kept) = head_fit(row.title, content_cells);
        let base_col = if row.selected { tfg } else { row_col };
        labels.push((shown_title.clone(), text_x, row_text_y, base_col));

        // Overlay each surviving matched char in the accent colour, positioned by
        // its DISPLAY-cell offset (correct for wide glyphs — never char index).
        let chars: Vec<char> = shown_title.chars().collect();
        for &idx in row.match_indices {
            if idx >= kept {
                continue; // fell into the truncated tail
            }
            let prefix_cells: usize = chars[..idx].iter().map(|&c| char_cells(c)).sum();
            let cx = text_x + prefix_cells as f32 * char_w;
            labels.push((chars[idx].to_string(), cx, row_text_y, accent));
        }
    }

    // --- Scrollbar thumb: only when the list overflows the visible window ---
    if total_matches > MAX_PALETTE_ROWS && n > 0 {
        let track_top = rows_top;
        let track_h = n as f32 * row_h;
        let visible = n as f32;
        let total = total_matches as f32;
        let thumb_h = (track_h * (visible / total)).max(8.0 * vscale).min(track_h);
        let max_first = (total_matches - n).max(1) as f32;
        let frac = (first_visible as f32 / max_first).clamp(0.0, 1.0);
        let thumb_y = track_top + (track_h - thumb_h) * frac;
        let tw = 3.0 * vscale;
        let tx = px + panel_w - pad_x * 0.5 - tw;
        quads.push(Rect::rounded(tx, thumb_y, tw, thumb_h, thumb_col, tw * 0.5));
    }

    CommandPalette { quads, labels, panel, row_hits }
}

/// Keep the LAST characters of `s` whose summed display width fits `budget`
/// cells (the caret end stays visible). Used for the typed query.
fn tail_fit(s: &str, budget: usize) -> String {
    if display_cells(s) <= budget {
        return s.to_string();
    }
    let mut cells = 0usize;
    let mut tail: Vec<char> = Vec::new();
    for c in s.chars().rev() {
        let w = char_cells(c);
        if cells + w > budget {
            break;
        }
        cells += w;
        tail.push(c);
    }
    tail.iter().rev().collect()
}

/// Keep the FIRST characters of `s` that fit `budget` cells; append `…` when it
/// overflowed. Returns the shown string and the number of ORIGINAL leading chars
/// kept (excluding the ellipsis) so matched indices can be range-checked.
fn head_fit(s: &str, budget: usize) -> (String, usize) {
    if display_cells(s) <= budget {
        let kept = s.chars().count();
        return (s.to_string(), kept);
    }
    // Reserve one cell for the ellipsis.
    let budget = budget.saturating_sub(1);
    let mut cells = 0usize;
    let mut out = String::new();
    let mut kept = 0usize;
    for c in s.chars() {
        let w = char_cells(c);
        if cells + w > budget {
            break;
        }
        cells += w;
        out.push(c);
        kept += 1;
    }
    out.push('…');
    (out, kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    const TEST_CHAR_W: f32 = 9.8;

    fn sample_rows<'a>(titles: &'a [String], sel: usize) -> Vec<PaletteRow<'a>> {
        titles
            .iter()
            .enumerate()
            .map(|(i, t)| PaletteRow { title: t.as_str(), match_indices: &[], selected: i == sel })
            .collect()
    }

    #[test]
    fn all_rows_fit_within_box_across_widths() {
        let titles: Vec<String> = vec![
            "New tab".into(),
            "Theme: Catppuccin Macchiato".into(),
            "Detach tab to new window".into(),
            "Toggle performance HUD".into(),
        ];
        for w in [320u32, 500, 700, 1000, 1600] {
            let rows = sample_rows(&titles, 0);
            let p = build_command_palette(w, 700, &theme(), TEST_CHAR_W, "the", &rows, 4, 0);
            assert!(p.panel.x >= 0.0 && p.panel.y >= 0.0, "panel off-screen at {w}");
            assert!(p.panel.x + p.panel.w <= w as f32 + 0.5, "panel exceeds width at {w}");
            let panel_right = p.panel.x + p.panel.w;
            for (text, x, _y, _c) in &p.labels {
                let est_right = x + display_cells(text) as f32 * TEST_CHAR_W;
                assert!(
                    est_right <= panel_right + 0.5,
                    "label {text:?} overflows the panel at width {w}: {est_right} > {panel_right}"
                );
            }
        }
    }

    #[test]
    fn box_is_centered_horizontally() {
        let titles = vec!["New tab".to_string()];
        let rows = sample_rows(&titles, 0);
        let p = build_command_palette(1000, 700, &theme(), TEST_CHAR_W, "", &rows, 1, 0);
        let left = p.panel.x;
        let right = 1000.0 - (p.panel.x + p.panel.w);
        assert!((left - right).abs() < 1.5, "box not centered: left {left}, right {right}");
    }

    #[test]
    fn selection_quad_present_when_a_row_is_selected() {
        let titles = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        // With a selection: a rounded highlight quad exists.
        let rows = sample_rows(&titles, 1);
        let p = build_command_palette(1000, 700, &theme(), TEST_CHAR_W, "", &rows, 3, 0);
        let sel_quads = p.quads.iter().filter(|q| q.radius == 6.0).count();
        assert_eq!(sel_quads, 1, "exactly one selection highlight expected");
        // With NO selection: none.
        let rows: Vec<PaletteRow> = titles
            .iter()
            .map(|t| PaletteRow { title: t, match_indices: &[], selected: false })
            .collect();
        let p = build_command_palette(1000, 700, &theme(), TEST_CHAR_W, "", &rows, 3, 0);
        assert_eq!(p.quads.iter().filter(|q| q.radius == 6.0).count(), 0);
    }

    #[test]
    fn scrollbar_thumb_only_when_overflowing() {
        let titles: Vec<String> = (0..MAX_PALETTE_ROWS).map(|i| format!("row {i}")).collect();
        let rows = sample_rows(&titles, 0);
        // total == visible → no thumb.
        let p = build_command_palette(1000, 900, &theme(), TEST_CHAR_W, "", &rows, MAX_PALETTE_ROWS, 0);
        let thin = |q: &Rect| q.w < 6.0 && q.h > 20.0;
        assert!(!p.quads.iter().any(thin), "no thumb when list fits");
        // total > visible → a thumb.
        let p = build_command_palette(1000, 900, &theme(), TEST_CHAR_W, "", &rows, 40, 0);
        assert!(p.quads.iter().any(thin), "thumb expected when list overflows");
    }

    #[test]
    fn scales_with_char_w() {
        // Tall window so no vertical clamp kicks in: 2× char_w → ~2× panel height.
        let titles = vec!["New tab".to_string(), "Close tab".to_string()];
        let p1 = build_command_palette(1200, 2000, &theme(), 9.8, "", &sample_rows(&titles, 0), 2, 0);
        let p2 = build_command_palette(1200, 2000, &theme(), 19.6, "", &sample_rows(&titles, 0), 2, 0);
        // Everything scales with char_w except the 1px divider, so allow ~1px slack.
        assert!(
            (p2.panel.h - p1.panel.h * 2.0).abs() < 2.0,
            "panel height must scale with char_w: {} vs {}",
            p2.panel.h,
            p1.panel.h
        );
    }

    #[test]
    fn matched_char_highlight_lands_at_cell_offset() {
        // Title "New tab", matched indices [0,4] ('N','t'). Each accent overlay
        // must sit at text_x + display_cells(prefix)*char_w.
        let title = "New tab".to_string();
        let indices = vec![0usize, 4usize];
        let rows = vec![PaletteRow { title: &title, match_indices: &indices, selected: true }];
        let p = build_command_palette(1000, 700, &theme(), TEST_CHAR_W, "nt", &rows, 1, 0);
        let accent = theme().palette[4];
        let accents: Vec<&(String, f32, f32, [u8; 3])> =
            p.labels.iter().filter(|l| l.3 == accent).collect();
        assert_eq!(accents.len(), 2, "two matched-char overlays expected");
        // The row text starts at panel.x + pad_x; derive it from the row label.
        let base = p.labels.iter().find(|l| l.0 == "New tab").expect("row label");
        let text_x = base.1;
        let n_glyph = accents.iter().find(|l| l.0 == "N").unwrap();
        let t_glyph = accents.iter().find(|l| l.0 == "t").unwrap();
        assert!((n_glyph.1 - text_x).abs() < 0.01, "N at wrong x");
        assert!((t_glyph.1 - (text_x + 4.0 * TEST_CHAR_W)).abs() < 0.01, "t at wrong x");
    }

    #[test]
    fn long_title_head_truncated_with_ellipsis_inside_panel() {
        // A very long title on a narrow window must be head-truncated with '…'
        // and still fit inside the panel.
        let long = "This is an extremely long command title that will not fit".to_string();
        let rows = vec![PaletteRow { title: &long, match_indices: &[], selected: false }];
        let p = build_command_palette(360, 700, &theme(), TEST_CHAR_W, "", &rows, 1, 0);
        let row_label = p.labels.iter().find(|l| l.0.contains('…')).expect("ellipsis title");
        let est_right = row_label.1 + display_cells(&row_label.0) as f32 * TEST_CHAR_W;
        assert!(est_right <= p.panel.x + p.panel.w + 0.5, "truncated title overflows panel");
    }
}
