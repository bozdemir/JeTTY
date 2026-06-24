use crate::Rect;

/// Hit-testing geometry exposed for the upcoming mouse-interaction task.
pub struct PanelGeom {
    pub panel: Rect,
    pub slider_track: Rect,
    pub slider_handle: Rect,
    pub chips: Vec<Rect>,
    /// Font-size decrement button ("−").
    pub font_minus: Rect,
    /// Font-size increment button ("+").
    pub font_plus: Rect,
    /// Font-size reset button ("reset").
    pub font_reset: Rect,
    /// One Rect per visible font-family row in the font picker list.
    /// Index i maps to `families[font_scroll_offset + i]`.
    pub font_rows: Vec<Rect>,
    /// The scroll offset into the families list at the time of rendering.
    pub font_scroll_offset: usize,
    /// The draggable title-bar strip at the top of the panel (~36px tall).
    /// A left-press here that does NOT hit any widget starts a dialog drag.
    pub title_bar: Rect,
    /// ▲ scroll button — decrements font_scroll_offset.
    pub font_scroll_up: Rect,
    /// ▼ scroll button — increments font_scroll_offset.
    pub font_scroll_down: Rect,
    /// Corner-radius slider track.
    pub radius_track: Rect,
    /// Corner-radius slider handle.
    pub radius_handle: Rect,
}

/// Full description of how to draw the settings panel for one frame.
pub struct PanelView {
    /// Rects in draw order (border → bg → chip highlights → chip fills → track → handle).
    pub quads: Vec<Rect>,
    /// Text labels: (text, x, y, rgb).
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Pixel geometry for hit-testing (used by the next mouse-interaction task).
    pub geom: PanelGeom,
}

/// Short display names for each preset, in PRESETS order.
const CHIP_NAMES: [&str; 4] = ["Mocha", "Tokyo", "Gruvbox", "Dracula"];

/// Maximum number of font-family rows displayed in the panel at once.
/// If more families exist, the list scrolls via `font_scroll_offset`.
const MAX_FONT_ROWS: usize = 5;

/// Build the settings panel for the given screen size, opacity (0.1..=1.0),
/// selected theme index (index into `jetty_core::theme::PRESETS`), current
/// logical font size (`font_size`), the list of monospace font families, the
/// currently selected family name, the scroll offset into the family list, and
/// a user drag offset (`dx`, `dy`) added to the centered position so the dialog
/// can be moved. The panel is clamped to remain fully on-screen.
pub fn build_panel(
    screen_w: u32,
    screen_h: u32,
    opacity: f32,
    theme_idx: usize,
    font_size: f32,
    families: &[String],
    selected_family: &str,
    font_scroll_offset: usize,
    corner_radius: f32,
    dx: f32,
    dy: f32,
) -> PanelView {
    const PANEL_W: f32 = 380.0;

    // ── Vertical layout (non-overlapping bands, each ≥12px gap from prior) ──
    //
    //  py+ 0  .. py+36   Title bar  (title label at py+12)
    //  py+48  .. py+96   Opacity band  (label at py+48, track at py+84)
    //                    slider track h=6 → bottom py+90; handle h=18 → bottom py+96
    //  py+108 .. py+156  Corner-radius band  (label at py+108, track at py+144)
    //                    track h=6 → bottom py+150; handle h=18 → bottom py+156
    //  py+168 .. py+216  Font-size band  (label at py+168, buttons at py+188, btn h=28 → bottom py+216)
    //  py+228 .. py+236  "Font" section header label
    //  py+250 ..         Font-family list rows (5×(22+2)=120px → bottom py+370)
    //  py+382            "Theme" label (12px gap after list bottom)
    //  py+402            Theme chips (h=36 → bottom py+438)
    //  PANEL_H = 438 + 18 = 456

    const PANEL_H: f32 = 456.0;

    // Center, then apply the user drag offset, then clamp to screen edges.
    let sw = screen_w as f32;
    let sh = screen_h as f32;
    let px = (((sw - PANEL_W) / 2.0).floor() + dx).clamp(0.0, (sw - PANEL_W).max(0.0));
    let py = (((sh - PANEL_H) / 2.0).floor() + dy).clamp(0.0, (sh - PANEL_H).max(0.0));

    // --- Full-screen dim quad (drawn before everything else) ---
    let dim_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: sw,
        h: sh,
        color: [0, 0, 0, 140],
    };

    // --- Border + background ---
    let border_rect = Rect {
        x: px - 2.0,
        y: py - 2.0,
        w: PANEL_W + 4.0,
        h: PANEL_H + 4.0,
        color: [70, 70, 90, 255],
    };
    let bg_rect = Rect {
        x: px,
        y: py,
        w: PANEL_W,
        h: PANEL_H,
        color: [28, 28, 36, 240],
    };

    // --- Title bar (draggable handle: py+0 .. py+36) ---
    let title_bar = Rect {
        x: px,
        y: py,
        w: PANEL_W,
        h: 36.0,
        color: [0, 0, 0, 0], // drawn via bg; color unused for hit-test
    };

    // --- Opacity band (py+48 .. py+96) ---
    // Label at py+48; track centred at py+84 (h=6); handle at py+78 (h=18).
    let slider_track = Rect {
        x: px + 16.0,
        y: py + 84.0,
        w: 348.0,
        h: 6.0,
        color: [60, 60, 75, 255],
    };
    let frac = ((opacity - 0.1) / 0.9).clamp(0.0, 1.0);
    let handle_x = px + 16.0 + frac * (348.0 - 14.0);
    let slider_handle = Rect {
        x: handle_x,
        y: py + 78.0,
        w: 14.0,
        h: 18.0,
        color: [185, 185, 205, 255],
    };

    // --- Corner-radius band (py+108 .. py+156) ---
    // Label at py+108; track centred at py+144 (h=6); handle at py+138 (h=18).
    // Radius range is [0, 24] px.
    const RADIUS_MAX: f32 = 24.0;
    let radius_track = Rect {
        x: px + 16.0,
        y: py + 144.0,
        w: 348.0,
        h: 6.0,
        color: [60, 60, 75, 255],
    };
    let r_frac = (corner_radius / RADIUS_MAX).clamp(0.0, 1.0);
    let radius_handle_x = px + 16.0 + r_frac * (348.0 - 14.0);
    let radius_handle = Rect {
        x: radius_handle_x,
        y: py + 138.0,
        w: 14.0,
        h: 18.0,
        color: [185, 185, 205, 255],
    };

    // --- Font-size band (py+168 .. py+216) ---
    // Label at py+168; "Npt" readout at py+194; buttons at py+188 (h=28 → bottom py+216).
    let font_btn_y = py + 188.0;
    let font_minus_x = px + 200.0;
    let font_plus_x  = font_minus_x + 36.0;
    let font_reset_x = font_plus_x  + 36.0;

    let font_minus = Rect {
        x: font_minus_x,
        y: font_btn_y,
        w: 28.0,
        h: 28.0,
        color: [55, 55, 72, 255],
    };
    let font_plus = Rect {
        x: font_plus_x,
        y: font_btn_y,
        w: 28.0,
        h: 28.0,
        color: [55, 55, 72, 255],
    };
    let font_reset = Rect {
        x: font_reset_x,
        y: font_btn_y,
        w: 44.0,
        h: 28.0,
        color: [55, 55, 72, 255],
    };

    // --- Font scroll buttons (▲ / ▼) in the "Font" header row at py+228 ---
    // Two 20×20 buttons placed at the right side of the header row.
    let scroll_btn_y = py + 226.0;
    let scroll_down_x = px + PANEL_W - 16.0 - 20.0;        // ▼ rightmost
    let scroll_up_x   = scroll_down_x - 24.0;               // ▲ left of ▼
    let font_scroll_up = Rect {
        x: scroll_up_x,
        y: scroll_btn_y,
        w: 20.0,
        h: 20.0,
        color: [55, 55, 72, 255],
    };
    let font_scroll_down = Rect {
        x: scroll_down_x,
        y: scroll_btn_y,
        w: 20.0,
        h: 20.0,
        color: [55, 55, 72, 255],
    };

    // --- Font-family list (py+250 .. py+370) ---
    // "Font" header at py+228; list rows start at py+250.
    // 5 rows × (22px row + 2px gap) = 120px → list bottom = py+370.
    // Theme label at py+382 (12px gap); chips at py+402.
    const ROW_H: f32 = 22.0;
    const ROW_GAP: f32 = 2.0;
    let list_top = py + 250.0;
    let list_x = px + 16.0;
    let list_w = PANEL_W - 32.0;

    let offset = font_scroll_offset.min(families.len().saturating_sub(1));
    let visible_count = (families.len().saturating_sub(offset)).min(MAX_FONT_ROWS);

    let mut font_row_rects: Vec<Rect> = Vec::with_capacity(visible_count);
    for i in 0..visible_count {
        let row_y = list_top + i as f32 * (ROW_H + ROW_GAP);
        let family_idx = offset + i;
        let is_selected = families.get(family_idx).map(|n| n.as_str()) == Some(selected_family);
        let row_color = if is_selected {
            [60, 80, 110, 255]
        } else {
            [38, 38, 50, 220]
        };
        font_row_rects.push(Rect {
            x: list_x,
            y: row_y,
            w: list_w,
            h: ROW_H,
            color: row_color,
        });
    }

    // --- Theme chips (py+402 .. py+438) ---
    // "Theme" label at py+382; chips at py+402 (h=36 → bottom py+438).
    let presets = jetty_core::theme::PRESETS;
    let num_presets = presets.len(); // should be 4

    let chip_top = py + 402.0;
    let mut chip_rects: Vec<Rect> = Vec::with_capacity(num_presets);
    for i in 0..num_presets {
        let chip_x = px + 16.0 + i as f32 * 88.0;
        let theme_bg = jetty_core::Theme::by_name(presets[i]).bg;
        chip_rects.push(Rect {
            x: chip_x,
            y: chip_top,
            w: 80.0,
            h: 36.0,
            color: [theme_bg[0], theme_bg[1], theme_bg[2], 255],
        });
    }

    // --- Build quads in draw order ---
    // Order: dim, border, bg, font buttons, font-family rows,
    //        per-chip selected-border, chip fills, slider track, slider handle.
    let mut quads: Vec<Rect> = Vec::new();
    quads.push(dim_rect);
    quads.push(border_rect);
    quads.push(bg_rect);

    // Font-size buttons.
    quads.push(font_minus);
    quads.push(font_plus);
    quads.push(font_reset);

    // Font-family scroll buttons (▲ / ▼).
    quads.push(font_scroll_up);
    quads.push(font_scroll_down);

    // Font-family list background rows.
    quads.extend_from_slice(&font_row_rects);

    // Selected-chip border highlight (pushed before chip fills so chip fill sits inside).
    if theme_idx < num_presets {
        let chip = &chip_rects[theme_idx];
        quads.push(Rect {
            x: chip.x - 2.0,
            y: chip.y - 2.0,
            w: 84.0,
            h: 40.0,
            color: [210, 210, 230, 255],
        });
    }

    // Chip fills.
    quads.extend_from_slice(&chip_rects);

    // Opacity slider track + handle.
    quads.push(slider_track);
    quads.push(slider_handle);

    // Corner-radius slider track + handle.
    quads.push(radius_track);
    quads.push(radius_handle);

    // --- Labels ---
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push(("Settings".to_string(), px + 16.0, py + 12.0, [230, 230, 240]));

    // Opacity label (band top at py+48).
    let pct = (opacity * 100.0).round() as i32;
    labels.push((
        format!("Opacity  {}%", pct),
        px + 16.0,
        py + 48.0,
        [200, 200, 210],
    ));

    // Corner-radius label (band top at py+108) with a px readout.
    let radius_px = corner_radius.round() as i32;
    labels.push((
        format!("Corner radius  {}px", radius_px),
        px + 16.0,
        py + 108.0,
        [200, 200, 210],
    ));

    // Font-size section header (band top at py+168).
    labels.push(("Font size".to_string(), px + 16.0, py + 168.0, [200, 200, 210]));

    // Current font-size readout aligned with buttons.
    let fs_display = font_size.round() as i32;
    labels.push((
        format!("{}pt", fs_display),
        px + 140.0,
        py + 194.0,
        [210, 210, 225],
    ));

    // Font button labels.
    labels.push(("-".to_string(),  font_minus_x + 9.0,  font_btn_y + 6.0,  [200, 200, 215]));
    labels.push(("+".to_string(),  font_plus_x  + 8.0,  font_btn_y + 6.0,  [200, 200, 215]));
    labels.push(("rst".to_string(), font_reset_x + 6.0, font_btn_y + 6.0,  [200, 200, 215]));

    // Font-family section header (at py+228; list starts at py+250).
    labels.push(("Font".to_string(), px + 16.0, py + 228.0, [200, 200, 210]));

    // Scroll button labels (▲ / ▼).
    labels.push(("^".to_string(), scroll_up_x   + 6.0, scroll_btn_y + 4.0, [200, 200, 215]));
    labels.push(("v".to_string(), scroll_down_x + 6.0, scroll_btn_y + 4.0, [200, 200, 215]));

    // Font-family row labels.
    for i in 0..visible_count {
        let family_idx = offset + i;
        if let Some(name) = families.get(family_idx) {
            let row_y = list_top + i as f32 * (ROW_H + ROW_GAP) + 4.0;
            let is_selected = name.as_str() == selected_family;
            let text_color: [u8; 3] = if is_selected {
                [220, 235, 255]
            } else {
                [190, 190, 205]
            };
            // Truncate long names to avoid overflowing the panel.
            // Use char-boundary-safe truncation to avoid panicking on
            // multibyte UTF-8 characters (e.g. accented/CJK family names).
            let display = if name.chars().count() > 36 {
                let truncated: String = name.chars().take(34).collect();
                format!("{}…", truncated)
            } else {
                name.clone()
            };
            labels.push((display, list_x + 6.0, row_y, text_color));
        }
    }

    // Show a scroll hint if there are more families than visible rows.
    if families.len() > MAX_FONT_ROWS {
        // Place the "(shown/total)" hint to the LEFT of the ▲/▼ buttons
        // (which sit at px+PANEL_W-60..) so the count and the arrows never overlap.
        let hint = format!("({}/{})", offset + visible_count, families.len());
        labels.push((hint, px + PANEL_W - 132.0, py + 228.0, [140, 140, 155]));
    }

    // Theme section label (at py+382; 12px gap after list bottom py+370).
    labels.push(("Theme".to_string(), px + 16.0, py + 382.0, [200, 200, 210]));

    // Chip name labels.
    for i in 0..num_presets {
        let chip = &chip_rects[i];
        let label_color: [u8; 3] = if presets[i] == "light" {
            [20, 20, 20]
        } else {
            [235, 235, 240]
        };
        labels.push((
            CHIP_NAMES[i].to_string(),
            chip.x + 8.0,
            chip.y + 10.0,
            label_color,
        ));
    }

    // --- PanelGeom for hit-testing ---
    let panel_rect = Rect {
        x: px,
        y: py,
        w: PANEL_W,
        h: PANEL_H,
        color: [0, 0, 0, 0], // color not used for hit-testing
    };
    let geom = PanelGeom {
        panel: panel_rect,
        slider_track,
        slider_handle,
        chips: chip_rects,
        font_minus,
        font_plus,
        font_reset,
        font_rows: font_row_rects,
        font_scroll_offset: offset,
        title_bar,
        font_scroll_up,
        font_scroll_down,
        radius_track,
        radius_handle,
    };

    PanelView { quads, labels, geom }
}
