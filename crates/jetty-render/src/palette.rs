use crate::chrome::{fit_head, fit_tail, ChromeMeasure, ChromeMetrics, CHROME_ADVANCE};
use crate::ui_palette::{ensure_contrast, rgba, UiPalette};
use crate::Rect;

/// Maximum number of result rows visible in the palette at once (the scroll
/// window). Shared with jetty-app so its scroll/PageUp-Down math and this
/// builder's visible-slice assumption stay in lockstep.
pub const MAX_PALETTE_ROWS: usize = 9;

/// A palette row's pitch (physical px) for chrome metrics `cm` — before a
/// short window squeezes it. The app converts touchpad travel to rows by it.
pub fn palette_row_h(cm: ChromeMetrics) -> f32 {
    (28.0 * cm.overlay_u()).max(16.0)
}

/// One result row handed to [`build_command_palette`]: a (possibly already
/// tail/head-truncated) title, the matched CHARACTER indices into that title
/// (for the accent highlight), whether it is the selected row, and the
/// command's live shortcut ("" for none — drawn right-aligned and dimmed,
/// like a menu's).
pub struct PaletteRow<'a> {
    pub title: &'a str,
    pub match_indices: &'a [usize],
    pub selected: bool,
    pub hint: &'a str,
}

/// The widest a row's shortcut hint may be, as a share of the content width:
/// past it (a narrow window, a long chord) the hint is dropped so the title
/// keeps its room.
const HINT_MAX_SHARE: f32 = 0.4;

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
/// colours come from the theme's `UiPalette` (no hardcoded RGB), all metrics scale with
/// the chrome unit `cm` (DPI × UI font), and every label is MEASURED with `m`
/// (the chrome layer's real shaping) — so the caret, the counter and the
/// per-char fuzzy highlights sit exactly on the drawn glyphs for any UI font,
/// and wide (CJK) titles truncate by their real width. `rows` is the
/// already-scrolled VISIBLE slice (≤ `MAX_PALETTE_ROWS`); `first_visible` +
/// `total_matches` drive the scrollbar thumb.
#[allow(clippy::too_many_arguments)]
pub fn build_command_palette(
    win_w: u32,
    win_h: u32,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    query: &str,
    rows: &[PaletteRow],
    total_matches: usize,
    first_visible: usize,
) -> CommandPalette {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived chrome (the shared UiPalette, like help.rs / search_bar.rs) ---
    let ui = UiPalette::cached(theme);
    let panel_bg = rgba(ui.surface, 242);
    let border_col = rgba(ui.border, 255);
    let sel_bg = rgba(ui.surface_hi, 255);
    let input_col = ui.text; // typed query
    let placeholder_col = ui.text_hint;
    let row_col = ui.text_dim; // unmatched title text, unselected row
    let counter_col = ui.text_hint;
    // Matched-char highlight: the accent, lifted on the selected row's raised
    // fill when it would be faint there.
    let accent = ui.accent;
    let accent_on_sel = ensure_contrast(ui.accent, &[ui.surface_hi], UiPalette::ACCENT_FLOOR);
    let row_selected_col = ui.text;
    // Shortcut hints: the menus' dim hint color, lifted on the selected row's
    // raised fill where it would be faint there.
    let hint_col = ui.text_hint;
    let hint_on_sel = ensure_contrast(ui.text_hint, &[ui.surface_hi], UiPalette::HINT_FLOOR);
    let caret_col = rgba(ui.text, 255);
    let thumb_col = rgba(ui.text_hint, 255);

    // --- Vertical metrics (scale with DPI × UI font; floored so a short window
    // still fits) ---
    let vscale = cm.overlay_u();
    let text_h = 16.0 * vscale;
    let pad_v = (14.0 * vscale).max(6.0);
    let input_h = (34.0 * vscale).max(24.0);
    let div_h = 1.0;
    let n = rows.len();
    let mut row_h = palette_row_h(cm);

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
    let content_w = (panel_w - 2.0 * pad_x).max(1.0);

    // Anchor the box slightly above centre (spotlight feel), clamped on-screen.
    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = (sh * 0.14).min((sh - panel_h).max(0.0)).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut row_hits: Vec<Rect> = Vec::new();

    // Full-screen dim so the palette reads as modal.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: ui.scrim, ..Default::default() });
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
    let counter = if total_matches == 0 {
        "no matches".to_string()
    } else if total_matches == 1 {
        "1 result".to_string()
    } else {
        format!("{total_matches} results")
    };
    let counter_w = m.text_w(&counter);
    // Gap between the query/caret and the counter (≈ one chrome char).
    let gap = cm.px(CHROME_ADVANCE);

    let input_y = py + pad_v;
    let input_text_y = input_y + (input_h - text_h) / 2.0;

    // Budget for the query text: content minus prompt, caret+gap, and counter+gap.
    let query_budget = (content_w - m.text_w(PROMPT) - 2.0 * gap - counter_w).max(0.0);
    // Tail-truncate the query (keep the caret end visible, like the search bar).
    let shown_query = fit_tail(m, query, query_budget, false);

    let input_text = if query.is_empty() {
        // The placeholder fits the same budget: in a narrow box it ran into
        // the counter.
        format!("{PROMPT}{}", fit_head(m, "Type a command…", query_budget, false))
    } else {
        format!("{PROMPT}{shown_query}")
    };
    let input_text_col = if query.is_empty() { placeholder_col } else { input_col };
    // Static caret right after the TYPED text (bar never self-drives frames) —
    // measured as drawn, so it hugs the last glyph for any UI font. With an empty
    // query it sits right after the prompt, before the placeholder.
    let typed = format!("{PROMPT}{shown_query}");
    let caret_x = text_x + m.text_w(&typed) + 2.0;
    labels.push((input_text, text_x, input_text_y, input_text_col));
    quads.push(Rect::new(caret_x, input_text_y, 2.0, text_h, caret_col));

    // Counter, right-aligned against the right padding.
    let counter_x = px + panel_w - pad_x - counter_w;
    labels.push((counter, counter_x, input_text_y, counter_col));

    // Divider between the input line and the result rows.
    let divider_y = input_y + input_h;
    quads.push(Rect::new(text_x, divider_y, content_w, div_h, border_col));

    // --- Result rows ---
    let rows_top = divider_y + div_h;
    let mut xs: Vec<f32> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let row_top = rows_top + i as f32 * row_h;
        let row_text_y = row_top + (row_h - text_h) / 2.0;
        row_hits.push(Rect::new(px, row_top, panel_w, row_h, [0, 0, 0, 0]));

        // Selected-row highlight behind the text.
        if row.selected {
            let sel_x = px + 4.0 * vscale;
            let sel_w = (panel_w - 8.0 * vscale).max(0.0);
            quads.push(Rect::rounded(sel_x, row_top, sel_w, row_h, sel_bg, cm.px(6.0)));
        }

        // The command's shortcut, right-aligned against the right padding (the
        // counter's edge) by its MEASURED width — the ⇧ ⌃ ⌘ glyphs are not one
        // cell each; the title truncates before it.
        let hint_w = if row.hint.is_empty() { 0.0 } else { m.text_w(row.hint) };
        let show_hint = hint_w > 0.0 && hint_w + gap <= content_w * HINT_MAX_SHARE;
        let title_w = if show_hint { content_w - hint_w - gap } else { content_w };
        if show_hint {
            let col = if row.selected { hint_on_sel } else { hint_col };
            labels.push((row.hint.to_string(), text_x + content_w - hint_w, row_text_y, col));
        }

        // Head-truncate the title to its width (append … when it overflows),
        // and keep only the matched indices that survive inside the visible head.
        let shown_title = fit_head(m, row.title, title_w, false);
        let kept = if shown_title == row.title {
            shown_title.chars().count()
        } else {
            shown_title.chars().count() - 1 // the trailing ellipsis is not original
        };
        let base_col = if row.selected { row_selected_col } else { row_col };
        let hl_col = if row.selected { accent_on_sel } else { accent };
        labels.push((shown_title.clone(), text_x, row_text_y, base_col));

        // Overlay each surviving matched char in the accent colour at its MEASURED
        // x inside the drawn title (correct for wide glyphs and proportional fonts).
        m.char_xs(&shown_title, false, &mut xs);
        let chars: Vec<char> = shown_title.chars().collect();
        for &idx in row.match_indices {
            if idx >= kept {
                continue; // fell into the truncated tail
            }
            let Some(&dx) = xs.get(idx) else { continue };
            let cx = text_x + dx;
            labels.push((chars[idx].to_string(), cx, row_text_y, hl_col));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    const TEST_CHAR_W: f32 = 9.8;
    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    /// A PROPORTIONAL test font: narrow 'i'/'l'/' ', wide 'M'/'W', 8px otherwise.
    struct PropMeasure;
    impl ChromeMeasure for PropMeasure {
        fn char_xs(&mut self, s: &str, _title: bool, out: &mut Vec<f32>) {
            out.clear();
            out.push(0.0);
            let mut x = 0.0;
            for c in s.chars() {
                x += match c {
                    'i' | 'l' | ' ' | '.' => 4.0,
                    'M' | 'W' | 'm' | 'w' => 14.0,
                    _ => 8.0,
                };
                out.push(x);
            }
        }
    }

    fn sample_rows<'a>(titles: &'a [String], sel: usize) -> Vec<PaletteRow<'a>> {
        titles
            .iter()
            .enumerate()
            .map(|(i, t)| PaletteRow { title: t.as_str(), match_indices: &[], selected: i == sel, hint: "" })
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
            let mut rows = sample_rows(&titles, 0);
            for r in &mut rows {
                r.hint = "⇧⌃T";
            }
            let p = build_command_palette(w, 700, &theme(), &mut mono(), CM, "the", &rows, 4, 0);
            assert!(p.panel.x >= 0.0 && p.panel.y >= 0.0, "panel off-screen at {w}");
            assert!(p.panel.x + p.panel.w <= w as f32 + 0.5, "panel exceeds width at {w}");
            let panel_right = p.panel.x + p.panel.w;
            for (text, x, _y, _c) in &p.labels {
                let est_right = x + mono().text_w(text);
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
        let p = build_command_palette(1000, 700, &theme(), &mut mono(), CM, "", &rows, 1, 0);
        let left = p.panel.x;
        let right = 1000.0 - (p.panel.x + p.panel.w);
        assert!((left - right).abs() < 1.5, "box not centered: left {left}, right {right}");
    }

    #[test]
    fn selection_quad_present_when_a_row_is_selected() {
        let titles = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        // With a selection: a rounded highlight quad exists.
        let rows = sample_rows(&titles, 1);
        let p = build_command_palette(1000, 700, &theme(), &mut mono(), CM, "", &rows, 3, 0);
        let sel_quads = p.quads.iter().filter(|q| q.radius == 6.0).count();
        assert_eq!(sel_quads, 1, "exactly one selection highlight expected");
        // With NO selection: none.
        let rows: Vec<PaletteRow> = titles
            .iter()
            .map(|t| PaletteRow { title: t, match_indices: &[], selected: false, hint: "" })
            .collect();
        let p = build_command_palette(1000, 700, &theme(), &mut mono(), CM, "", &rows, 3, 0);
        assert_eq!(p.quads.iter().filter(|q| q.radius == 6.0).count(), 0);
    }

    #[test]
    fn scrollbar_thumb_only_when_overflowing() {
        let titles: Vec<String> = (0..MAX_PALETTE_ROWS).map(|i| format!("row {i}")).collect();
        let rows = sample_rows(&titles, 0);
        // total == visible → no thumb.
        let p = build_command_palette(1000, 900, &theme(), &mut mono(), CM, "", &rows, MAX_PALETTE_ROWS, 0);
        let thin = |q: &Rect| q.w < 6.0 && q.h > 20.0;
        assert!(!p.quads.iter().any(thin), "no thumb when list fits");
        // total > visible → a thumb.
        let p = build_command_palette(1000, 900, &theme(), &mut mono(), CM, "", &rows, 40, 0);
        assert!(p.quads.iter().any(thin), "thumb expected when list overflows");
    }

    #[test]
    fn scales_with_chrome_metrics() {
        // Tall window so no vertical clamp kicks in: a 2× chrome unit → ~2× panel
        // height. The scale comes from the METRICS, not from the font's advance.
        let titles = vec!["New tab".to_string(), "Close tab".to_string()];
        let p1 = build_command_palette(1200, 2000, &theme(), &mut mono(), CM, "", &sample_rows(&titles, 0), 2, 0);
        let p2 = build_command_palette(
            1200, 2000, &theme(), &mut MonoMeasure(19.6), ChromeMetrics::new(2.0, 16.0), "",
            &sample_rows(&titles, 0), 2, 0,
        );
        // Everything scales except the 1px divider, so allow ~1px slack.
        assert!(
            (p2.panel.h - p1.panel.h * 2.0).abs() < 2.0,
            "panel height must scale with the chrome unit: {} vs {}",
            p2.panel.h,
            p1.panel.h
        );
        // A wide-'M' proportional font must NOT inflate the panel (the old
        // char_w/9.8 idiom scaled everything by the font's 'M' advance).
        let p3 = build_command_palette(1200, 2000, &theme(), &mut PropMeasure, CM, "", &sample_rows(&titles, 0), 2, 0);
        assert!((p3.panel.h - p1.panel.h).abs() < 0.01, "font choice changed the panel size");
    }

    #[test]
    fn proportional_font_highlights_and_caret_follow_measured_glyphs() {
        // "Mail list": 'M' is 14px, 'i'/'l'/' ' 4px, others 8px. The matched
        // 'l' (idx 3) must sit at the MEASURED prefix width 14+8+4 = 26, and the
        // caret right after the typed "> ml" (8+4+14+4 = 30) + 2.
        let title = "Mail list".to_string();
        let idx = vec![0usize, 3, 5];
        let rows = vec![PaletteRow { title: &title, match_indices: &idx, selected: true, hint: "" }];
        let p = build_command_palette(1000, 700, &theme(), &mut PropMeasure, CM, "ml", &rows, 1, 0);
        let ui = UiPalette::from_theme(&theme());
        let accent = ensure_contrast(ui.accent, &[ui.surface_hi], UiPalette::ACCENT_FLOOR);
        let base = p.labels.iter().find(|l| l.0 == "Mail list").expect("row label");
        let xs: Vec<(String, f32)> =
            p.labels.iter().filter(|l| l.3 == accent).map(|l| (l.0.clone(), l.1 - base.1)).collect();
        assert_eq!(xs, vec![("M".to_string(), 0.0), ("l".to_string(), 26.0), ("l".to_string(), 34.0)]);
        let input = p.labels.iter().find(|l| l.0 == "> ml").expect("input label");
        let caret = p.quads.iter().find(|q| q.w == 2.0 && q.y == input.2).expect("caret quad");
        assert!((caret.x - (input.1 + 30.0 + 2.0)).abs() < 0.01, "caret at {}", caret.x - input.1);
    }

    #[test]
    fn matched_char_highlight_lands_at_cell_offset() {
        // Title "New tab", matched indices [0,4] ('N','t'). Each accent overlay
        // must sit at text_x + the measured prefix width (monospace here).
        let title = "New tab".to_string();
        let indices = vec![0usize, 4usize];
        let rows = vec![PaletteRow { title: &title, match_indices: &indices, selected: true, hint: "" }];
        let p = build_command_palette(1000, 700, &theme(), &mut mono(), CM, "nt", &rows, 1, 0);
        let ui = UiPalette::from_theme(&theme());
        let accent = ensure_contrast(ui.accent, &[ui.surface_hi], UiPalette::ACCENT_FLOOR);
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
    fn rows_and_highlights_read_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
        let titles = ["Theme: Nord".to_string(), "New tab".to_string()];
        let (i0, i1) = (vec![0usize, 1], vec![0usize, 4]);
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let rows = vec![
                PaletteRow { title: &titles[0], match_indices: &i0, selected: true, hint: "⇧⌃T" },
                PaletteRow { title: &titles[1], match_indices: &i1, selected: false, hint: "⇧⌃T" },
            ];
            let p = build_command_palette(1000, 700, &t, &mut mono(), CM, "", &rows, 2, 0);
            let card = rgb(p.panel.color);
            let sel = rgb(p.quads.iter().find(|q| q.radius == 6.0).expect("selection quad").color);
            let n = &t.name;
            for (text, _, y, c) in &p.labels {
                let on_sel = (*y - p.row_hits[0].y).abs() < p.row_hits[0].h && *y >= p.row_hits[0].y;
                let (bg, floor) = if on_sel { (sel, 3.0) } else { (card, 3.0) };
                assert!(cr(*c, bg) >= floor, "{n}: {text:?} {}", cr(*c, bg));
            }
            // Full row titles are text-grade on their own row.
            let col = |s: &str| p.labels.iter().find(|l| l.0 == s).unwrap().3;
            assert!(cr(col("Theme: Nord"), sel) >= 4.5, "{n}: selected row");
            assert!(cr(col("New tab"), card) >= 4.5, "{n}: unselected row");
            assert_eq!(p.quads[0].color, UiPalette::cached(&t).scrim);
        }
    }

    /// A row's shortcut sits right-aligned at the counter's edge, dimmed like a
    /// menu's, and a long title truncates before it; a box too narrow for both
    /// drops the hint so the title keeps the row.
    #[test]
    fn a_row_shortcut_is_right_aligned_and_the_title_yields_to_it() {
        let long = "Detach tab to new window, a title much longer than the box".to_string();
        let rows = vec![PaletteRow { title: &long, match_indices: &[], selected: false, hint: "⇧⌃D" }];
        let p = build_command_palette(700, 700, &theme(), &mut mono(), CM, "", &rows, 1, 0);
        let right = |l: &(String, f32, f32, [u8; 3])| l.1 + mono().text_w(&l.0);
        let hint = p.labels.iter().find(|l| l.0 == "⇧⌃D").expect("hint label");
        let title = p.labels.iter().find(|l| l.0.starts_with("Detach")).expect("title label");
        let counter = p.labels.iter().find(|l| l.0 == "1 result").expect("counter");
        assert!((right(hint) - right(counter)).abs() < 0.01, "flush with the counter");
        assert!(title.0.ends_with('…'), "the long title truncates: {:?}", title.0);
        assert!(right(title) <= hint.1, "the title stops before the hint");
        assert_eq!(hint.3, UiPalette::from_theme(&theme()).text_hint);
        let p = build_command_palette(100, 700, &theme(), &mut mono(), CM, "", &rows, 1, 0);
        assert!(!p.labels.iter().any(|l| l.0 == "⇧⌃D"), "no room: no hint");
        assert!(p.labels.iter().any(|l| l.0.starts_with('D')), "the title keeps the row");
    }

    /// The empty query's placeholder fits before the counter in a narrow box
    /// (it was drawn whole and ran into "218 results").
    #[test]
    fn the_placeholder_never_runs_into_the_counter() {
        let titles = vec!["New tab".to_string()];
        let rows = sample_rows(&titles, 0);
        for w in [240u32, 300, 450, 1000] {
            let p = build_command_palette(w, 700, &theme(), &mut mono(), CM, "", &rows, 218, 0);
            let input = p.labels.iter().find(|l| l.0.starts_with("> ")).expect("input label");
            let counter = p.labels.iter().find(|l| l.0 == "218 results").expect("counter");
            let input_right = input.1 + mono().text_w(&input.0);
            assert!(input_right <= counter.1, "{w}: placeholder {:?} ends at {input_right} > {}", input.0, counter.1);
        }
    }

    #[test]
    fn long_title_head_truncated_with_ellipsis_inside_panel() {
        // A very long title on a narrow window must be head-truncated with '…'
        // and still fit inside the panel.
        let long = "This is an extremely long command title that will not fit".to_string();
        let rows = vec![PaletteRow { title: &long, match_indices: &[], selected: false, hint: "" }];
        let p = build_command_palette(360, 700, &theme(), &mut mono(), CM, "", &rows, 1, 0);
        let row_label = p.labels.iter().find(|l| l.0.contains('…')).expect("ellipsis title");
        let est_right = row_label.1 + mono().text_w(&row_label.0);
        assert!(est_right <= p.panel.x + p.panel.w + 0.5, "truncated title overflows panel");
    }
}
