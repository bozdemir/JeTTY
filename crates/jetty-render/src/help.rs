use crate::chrome::{fit_head, ChromeMeasure, ChromeMetrics, CHROME_ADVANCE};
use crate::ui_palette::{rgba, UiPalette};
use crate::Rect;

/// The keyboard-shortcut rows shown in the Help overlay — ONE binding per line
/// (single column) so a row's text can never overflow the panel's width. The
/// panel width is computed from the longest row below.
// Grouped into sections (`## ` = header, "" = blank spacer) with each shortcut
// as "KEY — description" so the overlay renders headers + aligned key/description
// columns. `App::compute_help_rows` emits the SAME shape from the live keymap.
pub const HELP_ROWS: &[&str] = &[
    "## Tabs & windows",
    "Ctrl+Shift+T — New tab",
    "Ctrl+Shift+W — Close tab",
    "Ctrl+Tab / Ctrl+Shift+Tab — Next / previous tab",
    "Ctrl+1…9 — Jump to tab",
    "Ctrl+Shift+D — Detach / reattach tab   (drag off bar; right-click for menu)",
    "Double-click tab / top bar — Rename / maximize",
    "F11 — Fullscreen (whole monitor)",
    "Drag top bar / edges — Move / resize window",
    "",
    "## Appearance",
    "Ctrl+= / Ctrl+- / Ctrl+0 — Font size",
    "Ctrl+Alt+= / Ctrl+Alt+- — Transparency",
    "Ctrl+, / Ctrl+Shift+O — Settings",
    "Ctrl+Shift+P — Command palette",
    "",
    "## Clipboard & selection",
    "Ctrl+Shift+C / Ctrl+Shift+V — Copy / paste",
    "Ctrl+Shift+Enter — Run selection in a new tab   (multi-line lands staged)",
    "Left-drag — Select text (auto-copies)",
    "Shift+drag — Select over mouse apps (vim / htop / Claude Code)",
    "Right-click / Menu — Context menu   (arrows move, Enter picks)",
    "",
    "## Search & scroll",
    "Ctrl+Shift+F — Search scrollback   (Enter next, Shift+Enter prev, Esc close)",
    "Ctrl+Shift+Z / Ctrl+Shift+X — Previous / next prompt",
    "Shift+PageUp / Shift+PageDown — Scroll",
    "Ctrl+L — Clear",
    "",
    "## Keyboard modes & links",
    "Ctrl+Shift+H — Hint mode: copy a URL / path   (Alt = open, Esc cancel)",
    "Ctrl+Shift+Space — Copy-mode: keyboard select   (hjkl, v/V/Ctrl+V, y = yank, r = run)",
    "Ctrl+click — Open URL   (Ctrl+hover underlines)",
    "",
    "## Other",
    "F9 (configurable) — Summon / hide window",
    "Ctrl+D — Close shell (EOF)",
    "Esc — Close this help",
];

/// The built-in help rows as owned strings. `App` generates its own rows from the
/// live keymap (so a remap is reflected); this is the default set (used by the
/// render-crate tests and as a fallback), byte-identical to today's overlay.
pub fn default_help_rows() -> Vec<String> {
    HELP_ROWS.iter().map(|s| s.to_string()).collect()
}

/// Geometry + draw data for the Help overlay.
pub struct HelpOverlay {
    /// Quads in draw order: full-screen dim, border, background panel, header
    /// rules, and the scroll thumb when the rows overflow.
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb) — title, then per row a section header, a
    /// key + a description label, or nothing (a blank spacer). A narrow window
    /// stacks each description on its own row under its key.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The panel rect (for hit-testing "click outside closes").
    pub panel: Rect,
    /// First row shown (the requested scroll, clamped to `max_scroll`).
    pub first_row: usize,
    /// Rows hidden when scrolled to the top: 0 when every row fits; otherwise
    /// the rows scroll (they never overlap — see `build_help_overlay`).
    pub max_scroll: usize,
    /// Rows visible at once (one PgUp/PgDn step).
    pub page_rows: usize,
}

/// One parsed help row: a section header, a key+description item, or a blank
/// spacer line between sections. Derived from the flat `&[String]` rows so the
/// App's live keymap-driven strings and the static `HELP_ROWS` share one format.
enum HelpEntry {
    Header(String),
    Item(String, String),
    Spacer,
}

/// One drawn help row: an entry as is, or — in the STACKED layout of a narrow
/// window — an item split into its key row and an indented description row.
#[derive(Clone, Copy)]
enum HelpLine<'a> {
    Header(&'a str),
    Item(&'a str, &'a str),
    Key(&'a str),
    Desc(&'a str),
    Spacer,
}

/// Build the centered "Keyboard Shortcuts" help overlay for a window of size
/// `win_w`×`win_h` (physical pixels). The panel is sized to fit the rows and
/// clamped on-screen. A click outside `panel` (or Esc / the "?" button) closes it.
///
/// `m` measures every key/description/header exactly as the chrome layer draws
/// it (so the description column aligns and the panel fits for any UI font);
/// `cm` scales the vertical rhythm and paddings with DPI × UI font. `scroll` is
/// the first row to show when the rows overflow the window (clamped; see
/// `HelpOverlay::max_scroll`).
pub fn build_help_overlay(
    win_w: u32,
    win_h: u32,
    theme: &jetty_core::Theme,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    rows: &[String],
    scroll: usize,
) -> HelpOverlay {
    let sw = win_w as f32;
    let sh = win_h as f32;

    // --- Theme-derived overlay chrome (the shared UiPalette) ---
    // Every color follows the active theme, so the overlay re-skins itself
    // instead of being a fixed dark card (which was invisible on the light
    // theme and clashed on Gruvbox/Dracula), and reads on light themes too.
    let ui = UiPalette::cached(theme);
    let panel_bg = rgba(ui.surface, 242);
    let border_col = rgba(ui.border, 255);
    let title_col = ui.text;
    // Colour hierarchy so the dialog scans at a glance: section HEADERS in the
    // theme's cursor hue (lifted when it is faint on the card — Palenight's
    // purple read 2.6:1), KEYS at full text brightness so the shortcut pops,
    // DESCRIPTIONS muted but still text-grade.
    let header_col = ui.readable(theme.cursor, UiPalette::TEXT_FLOOR);
    let key_col = ui.text;
    let desc_col = ui.text_dim;

    // Ideal vertical metrics. When the window is too SHORT to fit every row, the
    // padding / title / row heights are scaled DOWN proportionally (to a readable
    // floor) so the overlay always fits and no row clips off-screen.
    const PAD_IDEAL: f32 = 20.0;
    const TITLE_H_IDEAL: f32 = 34.0;
    const ROW_H_IDEAL: f32 = 26.0;
    // Readable floors: below these we stop shrinking (the panel is clamped to the
    // window top instead, which still keeps all rows on a very short window).
    const ROW_H_MIN: f32 = 16.0;
    const TITLE_H_MIN: f32 = 22.0;
    const PAD_MIN_V: f32 = 8.0;
    // Minimum padding kept even when the window is too narrow to fit the ideal
    // padding — we shrink padding before we ever let text overflow.
    const MIN_PAD: f32 = 6.0;

    // The panel must fit the LONGEST row (and the title). Width = longest text
    // width + padding on both sides.
    // Parse the flat rows into a readable structure: a `## `-prefixed row is a
    // SECTION HEADER, an empty row is a SPACER (blank line between sections), and
    // everything else is an ITEM split on the first " — " into (key, description)
    // so the two can be drawn as ALIGNED, colour-differentiated columns instead
    // of one dense grey line each.
    let entries: Vec<HelpEntry> = rows
        .iter()
        .map(|r| {
            if r.is_empty() {
                HelpEntry::Spacer
            } else if let Some(h) = r.strip_prefix("## ") {
                HelpEntry::Header(h.to_string())
            } else if let Some((k, d)) = r.split_once(" — ") {
                HelpEntry::Item(k.trim_end().to_string(), d.trim_start().to_string())
            } else {
                HelpEntry::Item(r.clone(), String::new())
            }
        })
        .collect();
    // Column metrics (MEASURED as drawn): the key column is as wide as the
    // widest key so every description lines up in a second column;
    // headers/title only constrain the overall width.
    let mut key_w = 0.0f32;
    let mut desc_w = 0.0f32;
    let mut header_w = 0.0f32;
    for e in &entries {
        match e {
            HelpEntry::Item(k, d) => {
                key_w = key_w.max(m.text_w(k));
                desc_w = desc_w.max(m.text_w(d));
            }
            HelpEntry::Header(h) => header_w = header_w.max(m.text_w(h)),
            HelpEntry::Spacer => {}
        }
    }
    // Gap between the key and description columns (2.5 chrome chars).
    let char_w = cm.px(CHROME_ADVANCE);
    let col_gap = 2.5 * char_w;
    let desc_x_off = key_w + col_gap;
    let title_w = m.text_w("Keyboard Shortcuts");
    let two_col_w = (desc_x_off + desc_w).max(header_w).max(title_w);

    // The vertical / padding metrics are design px, but the chrome line box is
    // `ceil(font_size * 1.3)` with `font_size = ui_font_logical * scale`, so it
    // grows with DPI and the UI font. Scale every vertical metric (ideals AND
    // floors) by the same chrome unit the text uses so rows never overlap their
    // neighbour on a 2× display or at a large UI font. This is the overlay unit:
    // ≈0.983 at 1×/16pt (`OVERLAY_SCALE`), as the default always rendered.
    let vscale = cm.overlay_u();
    let pad_ideal = PAD_IDEAL * vscale;
    let title_h_ideal = TITLE_H_IDEAL * vscale;
    let row_h_ideal = ROW_H_IDEAL * vscale;
    let pad_min_v = PAD_MIN_V * vscale;
    let title_h_min = TITLE_H_MIN * vscale;
    let row_h_min = ROW_H_MIN * vscale;
    let min_pad = MIN_PAD * vscale;

    // Width: the ideal fits the content with full padding, clamped to the window
    // with a margin; a narrower window first gives up padding (down to MIN_PAD).
    // If the two columns still don't fit (a large UI font, a narrow window), the
    // long DESCRIPTIONS (and headers/title) are ellipsized to the room left —
    // while a description keeps room for a dozen characters. Narrower than that
    // the layout STACKS: each description gets its own indented row under its
    // key, and anything still too wide is ellipsized, so no text ever runs past
    // the panel or the window.
    const MARGIN: f32 = 16.0;
    let max_panel_w = (sw - MARGIN * 2.0).max(0.0);
    let fit_w = max_panel_w - min_pad * 2.0;
    let desc_room = fit_w - desc_x_off;
    let stacked = two_col_w > fit_w && desc_room < 12.0 * char_w;
    let squeeze = two_col_w > fit_w && !stacked;
    let indent = 2.0 * char_w;
    let content_w = if stacked {
        key_w.max(indent + desc_w).max(header_w).max(title_w).min(fit_w).max(0.0)
    } else if squeeze {
        fit_w
    } else {
        two_col_w
    };
    // The drawn rows: one per entry, or key + description rows when stacked.
    let mut lines: Vec<HelpLine> = Vec::with_capacity(entries.len() * if stacked { 2 } else { 1 });
    for e in &entries {
        match e {
            HelpEntry::Item(k, d) if stacked => {
                lines.push(HelpLine::Key(k));
                if !d.is_empty() {
                    lines.push(HelpLine::Desc(d));
                }
            }
            HelpEntry::Item(k, d) => lines.push(HelpLine::Item(k, d)),
            HelpEntry::Header(h) => lines.push(HelpLine::Header(h)),
            HelpEntry::Spacer => lines.push(HelpLine::Spacer),
        }
    }

    let row_count = lines.len() as f32;
    // Ideal content height; if it exceeds the window, scale the vertical metrics
    // down by a single factor (clamped so each metric keeps its readable floor).
    let ideal_h = pad_ideal + title_h_ideal + row_count * row_h_ideal + pad_ideal;
    let avail_h = sh.max(0.0);
    let scale = if ideal_h > avail_h && ideal_h > 0.0 {
        (avail_h / ideal_h).clamp(0.0, 1.0)
    } else {
        1.0
    };
    // Apply the scale, then enforce per-metric floors so text stays legible.
    let pad_v = (pad_ideal * scale).max(pad_min_v);
    let title_h = (title_h_ideal * scale).max(title_h_min);
    let mut row_h = (row_h_ideal * scale).max(row_h_min);
    // Last resort: on a window too short even for the floored metrics, tighten
    // the row pitch to fit every row — but by at most 10% under its readable
    // floor (descenders may brush the next row's capitals, never cover them).
    // Squeezing further overlapped neighbouring rows outright (a 28pt UI font in
    // an ordinary window); those rows keep the floored pitch and SCROLL instead
    // (wheel, arrows, PgUp/PgDn, Home/End in the app), with a thumb on the
    // panel's right edge.
    if 2.0 * pad_v + title_h + row_count * row_h > avail_h && row_count > 0.0 {
        let fitted = (avail_h - 2.0 * pad_v - title_h) / row_count;
        if fitted >= row_h_min * 0.9 {
            row_h = fitted.clamp(1.0, row_h);
        }
    }
    let n_rows = lines.len();
    let rows_room = avail_h - 2.0 * pad_v - title_h;
    // Whole rows that fit (the epsilon absorbs the rounding of a pitch that was
    // just fitted exactly above).
    let visible = if row_h > 0.0 {
        ((rows_room / row_h + 1e-3).floor().max(1.0) as usize).min(n_rows)
    } else {
        n_rows
    };
    let max_scroll = n_rows - visible;
    let first = scroll.min(max_scroll);
    // Recompute the actual height from the (possibly floored) metrics, then clamp
    // to the window so the panel can never exceed it.
    let panel_h = (2.0 * pad_v + title_h + visible as f32 * row_h).min(avail_h.max(0.0));
    // `PAD` is the vertical text padding (top inset for the title).
    let pad_top = pad_v;

    let min_panel_w = content_w + min_pad * 2.0;
    let ideal_w = content_w + pad_ideal * 2.0;
    // Prefer ideal, clamp down toward the window, but never below the hard floor.
    let panel_w = ideal_w.min(max_panel_w).max(min_panel_w);
    // Effective horizontal padding after sizing: split the leftover space, but
    // never below min_pad.
    let pad_x = ((panel_w - content_w) / 2.0).clamp(min_pad, pad_ideal);

    let px = ((sw - panel_w) / 2.0).max(0.0).floor();
    let py = ((sh - panel_h) / 2.0).max(0.0).floor();

    let mut quads: Vec<Rect> = Vec::new();

    // Full-screen dim.
    quads.push(Rect { x: 0.0, y: 0.0, w: sw, h: sh, color: ui.scrim, ..Default::default() });
    // Border (rounded to match the window/tab frame). Clamp the top to y>=0 so a
    // very short window (py==0) never draws the border off-screen at y=-2.
    let border_y = (py - 2.0).max(0.0);
    quads.push(Rect::rounded(
        (px - 2.0).max(0.0), border_y, panel_w + 4.0, panel_h + 4.0, border_col, 10.0,
    ));
    // Background panel (rounded).
    let panel = Rect::rounded(px, py, panel_w, panel_h, panel_bg, 8.0);
    quads.push(panel);

    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Squeezed / stacked layouts: ellipsize to the room inside the window.
    let fit = |m: &mut dyn ChromeMeasure, s: &str, room: f32| -> String {
        if squeeze || stacked { fit_head(m, s, room, false) } else { s.to_string() }
    };

    // Title.
    labels.push((
        fit(m, "Keyboard Shortcuts", content_w),
        px + pad_x,
        py + pad_top,
        title_col,
    ));

    // Rows: section headers (accent), aligned key (bright) + description (muted)
    // columns, and blank spacers between sections. The description column starts
    // at a fixed offset so keys and descriptions each line up vertically (or, when
    // stacked, each description sits indented under its key).
    let rows_top = py + pad_top + title_h;
    for (i, line) in lines.iter().enumerate().skip(first).take(visible) {
        let y = rows_top + (i - first) as f32 * row_h;
        match *line {
            HelpLine::Spacer => {}
            HelpLine::Header(h) => {
                let shown = fit(m, h, content_w);
                let end = px + pad_x + m.text_w(&shown);
                labels.push((shown, px + pad_x, y, header_col));
                // A thin, subtle accent rule from the header's end to the
                // content's right edge, through the header line (the Settings
                // panel's section idiom), crisply separates the sections. It
                // used to run UNDER the header at 90% of the row pitch — which
                // shrinks to fit the window, putting the rule on the header's
                // baseline (the default 1000×640 window) — while a rule beside
                // the text can never touch it at any pitch.
                let rx = end + 1.25 * char_w;
                let rw = px + panel_w - pad_x - rx;
                if rw > 2.0 * char_w {
                    quads.push(Rect {
                        x: rx,
                        y: (y + cm.px(10.0)).round(),
                        w: rw,
                        h: (1.5 * vscale).max(1.0),
                        color: [header_col[0], header_col[1], header_col[2], 70],
                        ..Default::default()
                    });
                }
            }
            HelpLine::Item(key, desc) => {
                labels.push((key.to_string(), px + pad_x, y, key_col));
                if !desc.is_empty() {
                    labels.push((fit(m, desc, desc_room), px + pad_x + desc_x_off, y, desc_col));
                }
            }
            HelpLine::Key(key) => labels.push((fit(m, key, content_w), px + pad_x, y, key_col)),
            HelpLine::Desc(desc) => labels.push((
                fit(m, desc, (content_w - indent).max(0.0)),
                px + pad_x + indent,
                y,
                desc_col,
            )),
        }
    }

    // Scroll thumb in the right padding (the palette's idiom), only when rows
    // overflow: its length is the visible share, its offset the scroll position.
    if max_scroll > 0 {
        let track_h = visible as f32 * row_h;
        let thumb_h = (track_h * visible as f32 / n_rows as f32).max(8.0 * vscale).min(track_h);
        let thumb_y = rows_top + (track_h - thumb_h) * (first as f32 / max_scroll as f32);
        let tw = 3.0 * vscale;
        quads.push(Rect::rounded(
            px + panel_w - (pad_x + tw) * 0.5,
            thumb_y,
            tw,
            thumb_h,
            rgba(ui.text_hint, 255),
            tw * 0.5,
        ));
    }

    HelpOverlay { quads, labels, panel, first_row: first, max_scroll, page_rows: visible }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn theme() -> jetty_core::Theme {
        jetty_core::Theme::by_name("catppuccin_mocha")
    }

    /// Scale-1 char advance used in tests (matches the historical fallback constant).
    const TEST_CHAR_W: f32 = 9.8;

    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    fn mono() -> MonoMeasure {
        MonoMeasure(TEST_CHAR_W)
    }

    #[test]
    fn panel_is_centered_and_on_screen() {
        let h = build_help_overlay(1000, 700, &theme(), &mut mono(), CM, &default_help_rows(), 0);
        assert!(h.panel.x >= 0.0 && h.panel.y >= 0.0);
        assert!(h.panel.x + h.panel.w <= 1000.0 + 0.5);
        assert!(h.panel.y + h.panel.h <= 700.0 + 0.5);
        // Title first; then at least one label per non-spacer row (items with a
        // description add a second, key/desc column label).
        assert_eq!(h.labels[0].0, "Keyboard Shortcuts");
        let non_spacer = HELP_ROWS.iter().filter(|r| !r.is_empty()).count();
        assert!(h.labels.len() >= non_spacer + 1);
    }

    #[test]
    fn every_row_text_fits_inside_panel() {
        // Across a range of widths (including very narrow), no row's estimated
        // rendered text right edge may exceed the panel's right border.
        // The estimate uses the same advance the test measurer uses, so the
        // panel is always sized to contain the text.
        for w in [320u32, 500, 700, 1000, 1600] {
            let h = build_help_overlay(w, 700, &theme(), &mut mono(), CM, &default_help_rows(), 0);
            let panel_right = h.panel.x + h.panel.w;
            for (text, x, _y, _c) in &h.labels {
                let est_right = x + text.chars().count() as f32 * TEST_CHAR_W;
                assert!(
                    est_right <= panel_right + 0.5,
                    "row {text:?} overflows panel at width {w}: {est_right} > {panel_right}"
                );
            }
        }
    }

    #[test]
    fn every_row_fits_vertically_at_short_heights() {
        // At short window heights the overlay must still fit every row on-screen
        // (the lower rows must not clip off the bottom of the window).
        for h in [360u32, 420, 480, 640] {
            let overlay = build_help_overlay(700, h, &theme(), &mut mono(), CM, &default_help_rows(), 0);
            // The panel itself fits the window.
            assert!(
                overlay.panel.y >= 0.0 && overlay.panel.y + overlay.panel.h <= h as f32 + 0.5,
                "panel exceeds window at height {h}"
            );
            // Every label's baseline sits inside the window.
            for (text, _x, y, _c) in &overlay.labels {
                assert!(
                    *y >= 0.0 && *y <= h as f32,
                    "row {text:?} clips off-screen at height {h}: y={y}"
                );
            }
        }
    }

    #[test]
    fn rows_do_not_overlap_in_the_readable_range() {
        // Evaluation of F40 (SPLIT): the row PITCH must stay at or above the
        // chrome font ink height (~= font_size = ROW_H_MIN at scale 1) for every
        // window height down to the point where the floored metrics still fit —
        // so adjacent rows never overlap in the readable range. Below that the
        // rows scroll at the floor pitch (`short_windows_scroll_instead_of_
        // overlapping`).
        let ink_floor = 16.0_f32; // ROW_H_MIN == font_size at scale 1 (vscale==1)
        // The readable lower bound rises with the ENTRY count: the sectioned
        // overlay now has ~37 entries (headers + items + blank spacers — the
        // run-selection row of v0.25 added one), so the floored metrics
        // (2·8 + 22 + 37·16 ≈ 630px) need ~660px before the last-resort pitch
        // tightening kicks in. 660 is the smallest clear of that.
        for h in [660u32, 760, 900, 1100] {
            let overlay = build_help_overlay(700, h, &theme(), &mut mono(), CM, &default_help_rows(), 0);
            // labels[0] is the title; labels[1..] are the row labels. An item emits
            // a key AND a description label at the SAME y (side-by-side columns),
            // so collapse consecutive equal-y labels to get the distinct row pitch.
            let mut ys: Vec<f32> = overlay.labels[1..].iter().map(|(_t, _x, y, _c)| *y).collect();
            ys.dedup();
            for pair in ys.windows(2) {
                let pitch = pair[1] - pair[0];
                assert!(
                    pitch >= ink_floor - 0.01,
                    "adjacent help rows overlap at height {h}: pitch {pitch} < {ink_floor}"
                );
            }
        }
    }

    #[test]
    fn short_windows_scroll_instead_of_overlapping() {
        // Too short for every row even at the floor pitch (a 28pt UI font in an
        // ordinary window, or a tiny one at the default): the rows keep their
        // readable pitch and SCROLL — they used to be squeezed into each other.
        let rows = default_help_rows();
        for (cm, h) in [
            (ChromeMetrics::new(1.0, 28.0), 640u32),
            (ChromeMetrics::new(1.0, 28.0), 900),
            (CM, 360),
            (CM, 480),
        ] {
            let mut m = MonoMeasure(CHROME_ADVANCE * cm.u);
            let floor = 16.0 * cm.overlay_u();
            let top = build_help_overlay(1000, h, &theme(), &mut m, cm, &rows, 0);
            assert!(top.max_scroll > 0, "{h}px at {}pt must scroll", 16.0 * cm.u);
            assert_eq!(top.page_rows + top.max_scroll, rows.len());
            for o in [&top, &build_help_overlay(1000, h, &theme(), &mut m, cm, &rows, usize::MAX)] {
                assert!(o.panel.y >= 0.0 && o.panel.y + o.panel.h <= h as f32 + 0.5);
                let mut ys: Vec<f32> = o.labels[1..].iter().map(|l| l.2).collect();
                ys.dedup();
                for pair in ys.windows(2) {
                    assert!(pair[1] - pair[0] >= floor - 0.01, "rows overlap at {h}px: {pair:?}");
                }
                // Every row label sits inside the panel (scrolled-out rows are
                // not drawn at all).
                for l in &o.labels[1..] {
                    assert!(l.2 >= o.panel.y && l.2 + floor <= o.panel.y + o.panel.h + 0.5, "{l:?}");
                }
            }
            // Scrolled to the end (clamped): the last row shows, the first doesn't.
            let end = build_help_overlay(1000, h, &theme(), &mut m, cm, &rows, usize::MAX);
            assert_eq!(end.first_row, end.max_scroll);
            assert!(end.labels.iter().any(|l| l.0 == "Esc"), "last row reachable at {h}px");
            assert!(!end.labels.iter().any(|l| l.0 == "Ctrl+Shift+T"), "first row scrolled out");
            assert!(top.labels.iter().any(|l| l.0 == "Ctrl+Shift+T"));
        }
        // The default font in a 640px window still fits without scrolling.
        let fits = build_help_overlay(1000, 640, &theme(), &mut MonoMeasure(CHROME_ADVANCE), CM, &rows, 3);
        assert_eq!((fits.max_scroll, fits.first_row), (0, 0));
    }

    #[test]
    fn narrow_windows_stack_descriptions_under_their_keys() {
        // Too narrow for the two columns (a 28pt UI font in a 420px window, or
        // 320px at the default): every description gets its own indented row
        // under its key, and NO label runs past the panel or the window — the
        // key column used to push the panel off-window here.
        let rows = default_help_rows();
        for (cm, w) in [(ChromeMetrics::new(1.0, 28.0), 420u32), (CM, 320)] {
            let mut m = MonoMeasure(CHROME_ADVANCE * cm.u);
            let h = build_help_overlay(w, 2400, &theme(), &mut m, cm, &rows, 0);
            assert!(h.panel.x >= 0.0 && h.panel.x + h.panel.w <= w as f32 + 0.5, "panel past {w}px");
            for (text, x, _y, _c) in &h.labels {
                let right = x + m.text_w(text);
                assert!(right <= h.panel.x + h.panel.w + 0.5, "{text:?} overflows at {w}px: {right}");
            }
            let key = h.labels.iter().find(|l| l.0 == "Ctrl+Shift+T").expect("key row");
            let desc = h.labels.iter().find(|l| l.0 == "New tab").expect("description row");
            assert!(desc.2 > key.2, "description stacked below its key");
            assert!(desc.1 > key.1, "description indented");
        }
        // Wide enough for two columns: unchanged side-by-side rows.
        let h = build_help_overlay(1000, 900, &theme(), &mut MonoMeasure(CHROME_ADVANCE), CM, &rows, 0);
        let key = h.labels.iter().find(|l| l.0 == "Ctrl+Shift+T").unwrap();
        let desc = h.labels.iter().find(|l| l.0 == "New tab").unwrap();
        assert_eq!(desc.2, key.2, "side by side");
    }

    #[test]
    fn every_label_reads_on_the_card_on_every_theme() {
        use crate::colors::contrast_ratio as cr;
        for i in 0..jetty_core::theme::PRESETS.len() {
            let t = jetty_core::theme::theme_at(i);
            let h = build_help_overlay(1000, 900, &t, &mut mono(), CM, &default_help_rows(), 0);
            let card = [h.panel.color[0], h.panel.color[1], h.panel.color[2]];
            for (text, _, _, c) in &h.labels {
                assert!(cr(*c, card) >= 4.5, "{}: {text:?} {}", t.name, cr(*c, card));
            }
            assert_eq!(h.quads[0].color, UiPalette::cached(&t).scrim, "{}: palette scrim", t.name);
        }
    }

    #[test]
    fn single_column_rows() {
        // No row contains the two-column "·" separator anymore.
        for r in HELP_ROWS.iter() {
            assert!(!r.contains('·'), "row should be single-column: {r:?}");
        }
    }

    #[test]
    fn lists_core_bindings() {
        let h = build_help_overlay(1000, 700, &theme(), &mut mono(), CM, &default_help_rows(), 0);
        let joined: String = h.labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("F9"));
        assert!(joined.contains("Ctrl+Shift+P"));
        assert!(joined.contains("Ctrl+D"));
    }

    #[test]
    fn large_ui_font_ellipsizes_descriptions_into_the_window() {
        // A 28pt UI font in a 1000px window: the content is wider than the
        // window, so descriptions are ellipsized to fit — the panel and every
        // label stay inside the window instead of running off its right edge.
        let cm = ChromeMetrics::new(1.0, 28.0);
        let mut m = MonoMeasure(9.6 * cm.u);
        let h = build_help_overlay(1000, 900, &theme(), &mut m, cm, &default_help_rows(), 0);
        assert!(h.panel.x >= 0.0 && h.panel.x + h.panel.w <= 1000.0 + 0.5, "panel past the window");
        for (text, x, _y, _c) in &h.labels {
            let right = x + m.text_w(text);
            assert!(right <= h.panel.x + h.panel.w + 0.5, "{text:?} overflows: {right}");
        }
        assert!(h.labels.iter().any(|l| l.0.ends_with('…')), "long descriptions are ellipsized");
        // Keys are never cut.
        assert!(h.labels.iter().any(|l| l.0 == "Ctrl+Tab / Ctrl+Shift+Tab"));
    }

    /// A section header's rule never runs through the header's text. It sat
    /// at 90% of the row pitch — below the text at the ideal pitch, but the
    /// pitch shrinks to fit the window, and in the default 1000×640 window
    /// it ran along the header's baseline, striking through its letters.
    #[test]
    fn section_rules_never_cross_their_header() {
        let rows = default_help_rows();
        for cm in [CM, ChromeMetrics::new(2.0, 16.0), ChromeMetrics::new(1.0, 13.0)] {
            let mut m = MonoMeasure(CHROME_ADVANCE * cm.u);
            for (w, h) in [(1000u32, 640u32), (1000, 480), (700, 560), (1000, 760), (1600, 1200)] {
                let (w, h) = ((w as f32 * cm.dpi) as u32, (h as f32 * cm.dpi) as u32);
                let o = build_help_overlay(w, h, &theme(), &mut m, cm, &rows, 0);
                let header = UiPalette::cached(&theme()).readable(theme().cursor, UiPalette::TEXT_FLOOR);
                let rules: Vec<&Rect> = o.quads.iter().filter(|q| q.color[3] == 70).collect();
                let headers: Vec<_> = o.labels.iter().filter(|l| l.3 == header).collect();
                assert!(!headers.is_empty() && rules.len() == headers.len(), "{w}×{h}: one rule per header");
                for (t, x, y, _) in headers {
                    // The header's ink: cap tops to descenders of its line.
                    let (l, r) = (*x, x + m.text_w(t));
                    let (top, bot) = (y + cm.px(3.0), y + cm.px(19.0));
                    for q in &rules {
                        let apart = q.x >= r || q.x + q.w <= l || q.y >= bot || q.y + q.h <= top;
                        assert!(apart, "{w}×{h} @{}: the rule crosses {t:?}", cm.u);
                    }
                }
            }
        }
    }

    #[test]
    fn wide_window_keeps_full_descriptions() {
        let h = build_help_overlay(1600, 1200, &theme(), &mut mono(), CM, &default_help_rows(), 0);
        assert!(h.labels.iter().all(|l| !l.0.ends_with('…')), "nothing to squeeze at 1600px");
    }
}
