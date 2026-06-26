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
    /// Summon-effect "‹" (previous) cycle button.
    pub summon_prev: Rect,
    /// Summon-effect "›" (next) cycle button.
    pub summon_next: Rect,
    /// Window-mode "‹" (previous) cycle button.
    pub win_mode_prev: Rect,
    /// Window-mode "›" (next) cycle button.
    pub win_mode_next: Rect,
    /// Tab-bar-position "‹" (previous) cycle button.
    pub tab_bar_prev: Rect,
    /// Tab-bar-position "›" (next) cycle button.
    pub tab_bar_next: Rect,
    /// Dropdown-height slider track.
    pub dropdown_track: Rect,
    /// Dropdown-height slider handle.
    pub dropdown_handle: Rect,
    /// Dropdown-width slider track.
    pub dropdown_width_track: Rect,
    /// Dropdown-width slider handle.
    pub dropdown_width_handle: Rect,
    /// "Auto-hide on focus loss" toggle pill.
    pub autohide_toggle: Rect,
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
const CHIP_NAMES: [&str; 5] = ["Mocha", "Tokyo", "Gruv", "Drac", "Onyx"];

/// Maximum number of font-family rows displayed in the panel at once.
/// If more families exist, the list scrolls via `font_scroll_offset`.
const MAX_FONT_ROWS: usize = 5;

/// Chrome-font character advance (px). Used for right-aligned value readouts
/// and scroll-hint sizing. Matches the advance used in help.rs/tabbar.rs.
const CHAR_W: f32 = 9.8;

/// Settings-panel dimensions in logical px. The separate Settings OS window is
/// sized to these (+ border) — see `SETTINGS_WIN_*` in jetty-app. Growing the
/// panel here (e.g. adding a row) automatically resizes that window, so the
/// bottom rows can never be clipped off a too-short window again.
///
/// PANEL_H was grown from 744→860 to accommodate the 2-column×3-row theme card
/// grid (card_h=40, gap=8 → 3 rows = 136px) replacing the original single-row
/// chips (36px). 860−744=116px delta (136−36=100px new content + 16px margin).
pub const PANEL_W: f32 = 380.0;
pub const PANEL_H: f32 = 860.0;

// Compile-time lockstep: CHIP_NAMES must always have one entry per PRESETS entry.
// This panics at build time if PRESETS grows without updating CHIP_NAMES (or vice versa).
const _: () = assert!(CHIP_NAMES.len() == jetty_core::theme::PRESETS.len());

/// Build the settings panel for the given screen size, opacity (0.1..=1.0),
/// selected theme index (index into `jetty_core::theme::PRESETS`), current
/// logical font size (`font_size`), the list of monospace font families, the
/// currently selected family name, the scroll offset into the family list, and
/// a user drag offset (`dx`, `dy`) added to the centered position so the dialog
/// can be moved. The panel is clamped to remain fully on-screen.
#[allow(clippy::too_many_arguments)]
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
    summon_effect_name: &str,
    window_mode_name: &str,
    tab_bar_name: &str,
    dropdown_height_pct: f32,
    dropdown_width_pct: f32,
    is_dropdown: bool,
    focus_autohide: bool,
    dx: f32,
    dy: f32,
    theme: &jetty_core::Theme,
) -> PanelView {

    // --- Theme-derived panel chrome colors ---
    // All panel colors are derived from the ACTIVE theme so the settings window
    // re-skins itself when the theme changes (instead of staying a fixed dark
    // gray). `lerp` blends bg→fg by `t` for shades that keep contrast on both
    // dark and light themes.
    let tbg = theme.bg; // [r,g,b,a]
    let tfg = theme.fg; // [r,g,b]
    let accent = theme.palette[4]; // blue accent
    let lerp = |t: f32| -> [u8; 3] {
        [
            (tbg[0] as f32 + (tfg[0] as f32 - tbg[0] as f32) * t).round() as u8,
            (tbg[1] as f32 + (tfg[1] as f32 - tbg[1] as f32) * t).round() as u8,
            (tbg[2] as f32 + (tfg[2] as f32 - tbg[2] as f32) * t).round() as u8,
        ]
    };
    // Panel surface: a slightly lifted shade of the theme bg, made nearly opaque.
    let panel_bg3 = lerp(0.06);
    let panel_bg: [u8; 4] = [panel_bg3[0], panel_bg3[1], panel_bg3[2], 242];
    // Border: a lighter shade so the panel reads as a card on any theme.
    let border3 = lerp(0.30);
    let panel_border: [u8; 4] = [border3[0], border3[1], border3[2], 255];
    // Button fills: a mid shade between bg and fg.
    let btn3 = lerp(0.14);
    let btn_fill: [u8; 4] = [btn3[0], btn3[1], btn3[2], 255];
    // Slider track: dim shade.
    let track3 = lerp(0.18);
    let slider_track_col: [u8; 4] = [track3[0], track3[1], track3[2], 255];
    // Accent for handles / selection highlights.
    let accent_col: [u8; 4] = [accent[0], accent[1], accent[2], 255];
    // Accent fill for the "filled" (left) portion of slider tracks.
    // Uses a slightly translucent accent so the track gradient reads clearly.
    let accent_fill: [u8; 4] = [accent[0], accent[1], accent[2], 200];
    // Selected font-row background: a dim accent blend.
    let row_sel: [u8; 4] = [
        ((tbg[0] as u16 + accent[0] as u16 * 2) / 3) as u8,
        ((tbg[1] as u16 + accent[1] as u16 * 2) / 3) as u8,
        ((tbg[2] as u16 + accent[2] as u16 * 2) / 3) as u8,
        255,
    ];
    // Unselected font-row background: a faint lift over the panel bg.
    let row_unsel3 = lerp(0.10);
    let row_unsel: [u8; 4] = [row_unsel3[0], row_unsel3[1], row_unsel3[2], 220];
    // Text colors.
    let text_main = tfg;
    let text_dim = lerp(0.70);
    let text_btn = lerp(0.78);
    // Section headers (CAPS) use a slightly dimmer shade than body text so they
    // read as labels rather than values, matching the design's "muted CAPS" look.
    let text_header = lerp(0.55);

    // ── Vertical layout (non-overlapping bands, each ≥12px gap from prior) ──
    //
    //  py+ 0  .. py+36   Title bar  (title label at py+12)
    //  py+48  .. py+96   Opacity band  (CAPS label+value at py+48, track at py+72)
    //                    slider track h=6 → bottom py+78; handle h=18 → bottom py+84 (± 3)
    //  py+96  .. py+144  Corner-radius band  (CAPS label+value at py+96, track at py+120)
    //                    track h=6 → bottom py+126; handle h=18 → bottom py+132
    //  py+144 .. py+192  Summon-effect band  (CAPS label at py+144, ‹ name › INLINE on the header row at py+138, h=28)
    //  py+192 .. py+240  Window-mode band    (CAPS label at py+192, ‹ name › INLINE on the header row at py+186, h=28)
    //  py+240 .. py+288  Tab-bar band        (CAPS label at py+240, ‹ name › INLINE on the header row at py+234, h=28)
    //  py+300 .. py+336  Dropdown-height band (CAPS label+value at py+300, track at py+324, handle py+318)
    //  py+348 .. py+384  Dropdown-width band  (CAPS label+value at py+348, track at py+372, handle py+366)
    //  py+396 .. py+432  Auto-hide band      (CAPS label at py+396, toggle pill at py+396, h=28)
    //  py+444 .. py+492  Font-size band  (CAPS label+value at py+444, buttons at py+464, btn h=28 → bottom py+492)
    //  py+504 .. py+512  "FONT" section header label + scroll arrows
    //  py+526 ..         Font-family list rows (5×(22+2)=120px → bottom py+646)
    //  py+658            "THEME" label (12px gap after list bottom)
    //  py+678            Theme cards — 2-col × 3-row grid:
    //                    col_w=(348−8)/2=170; card_h=40; gap=8
    //                    row0 at py+678..py+718; row1 at py+726..py+766; row2 at py+774..py+814
    //  py+814..py+820   6px bottom margin
    //  PANEL_H = 820 + 6 = 826  → use 860 to leave a small extra margin
    //  (PANEL_W/PANEL_H are module-level pub consts above)

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
        color: [0, 0, 0, 140], ..Default::default() };

    // --- Border + background ---
    let border_rect = Rect::rounded(
        px - 2.0, py - 2.0, PANEL_W + 4.0, PANEL_H + 4.0, panel_border, 10.0,
    );
    let bg_rect = Rect::rounded(px, py, PANEL_W, PANEL_H, panel_bg, 8.0);

    // --- Title bar (draggable handle: py+0 .. py+36) ---
    let title_bar = Rect {
        x: px,
        y: py,
        w: PANEL_W,
        h: 36.0,
        color: [0, 0, 0, 0], // drawn via bg; color unused for hit-test
        ..Default::default()
    };

    // Helper: right-align a text value on the section-header row.
    // Returns the x-position such that the text's right edge sits at px+PANEL_W-16.
    let right_x = |text: &str| -> f32 {
        let w = text.chars().count() as f32 * CHAR_W;
        px + PANEL_W - 16.0 - w
    };

    // --- Opacity band (py+48 .. py+96) ---
    // CAPS label at py+48; value right-aligned on same row; track at py+72 (h=6);
    // handle at py+66 (h=18). The filled left portion of the track shows progress.
    let slider_track = Rect::rounded(px + 16.0, py + 72.0, 348.0, 6.0, slider_track_col, 3.0);
    let frac = ((opacity - 0.1) / 0.9).clamp(0.0, 1.0);
    let handle_x = px + 16.0 + frac * (348.0 - 14.0);
    let slider_handle = Rect::rounded(handle_x, py + 66.0, 14.0, 18.0, accent_col, 4.0);
    // Filled portion: from track left to handle centre, in accent color.
    let opacity_fill_w = (frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let opacity_fill = Rect::rounded(px + 16.0, py + 72.0, opacity_fill_w, 6.0, accent_fill, 3.0);

    // --- Corner-radius band (py+96 .. py+144) ---
    // CAPS label at py+96; value right-aligned; track at py+120 (h=6); handle at py+114 (h=18).
    // Radius range is [0, 24] px.
    const RADIUS_MAX: f32 = 24.0;
    let radius_track = Rect::rounded(px + 16.0, py + 120.0, 348.0, 6.0, slider_track_col, 3.0);
    let r_frac = (corner_radius / RADIUS_MAX).clamp(0.0, 1.0);
    let radius_handle_x = px + 16.0 + r_frac * (348.0 - 14.0);
    let radius_handle = Rect::rounded(radius_handle_x, py + 114.0, 14.0, 18.0, accent_col, 4.0);
    let radius_fill_w = (r_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let radius_fill = Rect::rounded(px + 16.0, py + 120.0, radius_fill_w, 6.0, accent_fill, 3.0);

    // --- Summon-effect band (py+144 .. py+192) ---
    // CAPS label at py+144; ‹ name › cycle control INLINE on the same row,
    // right-aligned (button h=28 centred on the header text → top at py+138),
    // mirroring the sliders' header+value-on-one-row rhythm.
    let summon_btn_y = py + 138.0;
    let summon_prev_x = px + 200.0;
    let summon_next_x = px + PANEL_W - 16.0 - 28.0; // rightmost
    let summon_prev = Rect::rounded(summon_prev_x, summon_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let summon_next = Rect::rounded(summon_next_x, summon_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Window-mode band (py+192 .. py+240) ---
    // CAPS label at py+192; ‹ name › cycle control inline on the header row.
    let winmode_btn_y = py + 186.0;
    let win_mode_prev = Rect::rounded(summon_prev_x, winmode_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let win_mode_next = Rect::rounded(summon_next_x, winmode_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Tab-bar band (py+240 .. py+288) ---
    // CAPS label at py+240; ‹ name › cycle control inline on the header row.
    let tabbar_btn_y = py + 234.0;
    let tab_bar_prev = Rect::rounded(summon_prev_x, tabbar_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let tab_bar_next = Rect::rounded(summon_next_x, tabbar_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Dropdown-height band (py+300 .. py+336) ---
    // CAPS label at py+300; value right-aligned; track at py+324 (h=6); handle at py+318 (h=18).
    // Range 25%..100%. Grayed (treated as no-op) when mode==Center.
    let dropdown_track = Rect::rounded(px + 16.0, py + 324.0, 348.0, 6.0, slider_track_col, 3.0);
    let dh_frac = ((dropdown_height_pct - 0.25) / 0.75).clamp(0.0, 1.0);
    let dropdown_handle_x = px + 16.0 + dh_frac * (348.0 - 14.0);
    let dropdown_handle = Rect::rounded(dropdown_handle_x, py + 318.0, 14.0, 18.0, accent_col, 4.0);
    let dh_fill_w = (dh_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let dh_fill = Rect::rounded(px + 16.0, py + 324.0, dh_fill_w, 6.0, accent_fill, 3.0);

    // --- Dropdown-width band (py+348 .. py+384) ---
    // CAPS label at py+348; value right-aligned; track at py+372 (h=6); handle at py+366 (h=18).
    // Range 20%..100%. Grayed (treated as no-op) when mode==Center.
    let dropdown_width_track = Rect::rounded(px + 16.0, py + 372.0, 348.0, 6.0, slider_track_col, 3.0);
    let dw_frac = ((dropdown_width_pct - 0.2) / 0.8).clamp(0.0, 1.0);
    let dropdown_width_handle_x = px + 16.0 + dw_frac * (348.0 - 14.0);
    let dropdown_width_handle = Rect::rounded(dropdown_width_handle_x, py + 366.0, 14.0, 18.0, accent_col, 4.0);
    let dw_fill_w = (dw_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let dw_fill = Rect::rounded(px + 16.0, py + 372.0, dw_fill_w, 6.0, accent_fill, 3.0);

    // --- Auto-hide band (py+396 .. py+432) ---
    // CAPS label at py+396; toggle pill at the right (h=28). Pill is accent when ON.
    let autohide_pill_col: [u8; 4] = if focus_autohide { accent_col } else { btn_fill };
    let autohide_toggle = Rect::rounded(px + PANEL_W - 16.0 - 56.0, py + 396.0, 56.0, 28.0, autohide_pill_col, 14.0);

    // --- Font-size band (py+444 .. py+492) ---
    // CAPS label+value at py+444; "Npt" readout right-aligned; buttons at py+464 (h=28 → bottom py+492).
    let font_btn_y = py + 464.0;
    let font_minus_x = px + 200.0;
    let font_plus_x  = font_minus_x + 36.0;
    let font_reset_x = font_plus_x  + 36.0;

    let font_minus = Rect::rounded(font_minus_x, font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let font_plus = Rect::rounded(font_plus_x, font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let font_reset = Rect::rounded(font_reset_x, font_btn_y, 44.0, 28.0, btn_fill, 4.0);

    // --- Font scroll buttons (▲ / ▼) in the "FONT" header row at py+504 ---
    // Two 20×20 buttons placed at the right side of the header row.
    let scroll_btn_y = py + 502.0;
    let scroll_down_x = px + PANEL_W - 16.0 - 20.0;        // ▼ rightmost
    let scroll_up_x   = scroll_down_x - 24.0;               // ▲ left of ▼
    let font_scroll_up = Rect::rounded(scroll_up_x, scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);
    let font_scroll_down = Rect::rounded(scroll_down_x, scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);

    // --- Font-family list (py+526 .. py+646) ---
    // "FONT" header at py+504; list rows start at py+526.
    // 5 rows × (22px row + 2px gap) = 120px → list bottom = py+646.
    // Theme label at py+658 (12px gap); cards start at py+678.
    const ROW_H: f32 = 22.0;
    const ROW_GAP: f32 = 2.0;
    let list_top = py + 526.0;
    let list_x = px + 16.0;
    let list_w = PANEL_W - 32.0;

    let offset = font_scroll_offset.min(families.len().saturating_sub(1));
    let visible_count = (families.len().saturating_sub(offset)).min(MAX_FONT_ROWS);

    let mut font_row_rects: Vec<Rect> = Vec::with_capacity(visible_count);
    for i in 0..visible_count {
        let row_y = list_top + i as f32 * (ROW_H + ROW_GAP);
        let family_idx = offset + i;
        let is_selected = families.get(family_idx).map(|n| n.as_str()) == Some(selected_family);
        let row_color = if is_selected { row_sel } else { row_unsel };
        font_row_rects.push(Rect::rounded(list_x, row_y, list_w, ROW_H, row_color, 3.0));
    }

    // --- Theme cards — 2-column × 3-row grid (py+678 .. py+854) ---
    // "THEME" label at py+658; cards start at py+678.
    // Each card: col_w = (348 − 8) / 2 = 170px; card_h = 40px; row_gap = 8px.
    // Layout: row0=[Mocha,Tokyo], row1=[Gruv,Drac], row2=[Onyx,(empty)].
    // Each card contains a 3-dot color preview (bg, palette[4]/accent, palette[2])
    // and the theme name beneath the dots, giving an at-a-glance swatch.
    let presets = jetty_core::theme::PRESETS;
    let num_presets = presets.len(); // 5

    const CARD_COLS: usize = 2;
    const CARD_H: f32 = 40.0;
    const CARD_ROW_GAP: f32 = 8.0;
    const CARD_COL_GAP: f32 = 8.0;
    let card_w = (348.0 - (CARD_COLS as f32 - 1.0) * CARD_COL_GAP) / CARD_COLS as f32; // 170px
    let card_top = py + 678.0;

    let mut chip_rects: Vec<Rect> = Vec::with_capacity(num_presets);
    for i in 0..num_presets {
        let col = (i % CARD_COLS) as f32;
        let row = (i / CARD_COLS) as f32;
        let card_x = px + 16.0 + col * (card_w + CARD_COL_GAP);
        let card_y = card_top + row * (CARD_H + CARD_ROW_GAP);
        let theme_bg = jetty_core::Theme::by_name(presets[i]).bg;
        chip_rects.push(Rect::rounded(
            card_x, card_y, card_w, CARD_H,
            [theme_bg[0], theme_bg[1], theme_bg[2], 255], 6.0,
        ));
    }

    // Compute bottom of last card for PANEL_H sanity — last card row for 5 items:
    // row = (4/2) = 2, bottom = card_top + 2*(40+8) + 40 = card_top + 136
    // card_top = py+678, so bottom = py+814. PANEL_H=860 gives 46px margin. ✓

    // --- Build quads in draw order ---
    // Order: dim, border, bg, buttons, filled-track portions, tracks, handles,
    //        chip border highlight, chip fills, font-family rows.
    let mut quads: Vec<Rect> = Vec::new();
    quads.push(dim_rect);
    quads.push(border_rect);
    quads.push(bg_rect);

    // Summon-effect cycle buttons.
    quads.push(summon_prev);
    quads.push(summon_next);

    // Window-mode cycle buttons.
    quads.push(win_mode_prev);
    quads.push(win_mode_next);

    // Tab-bar position cycle buttons.
    quads.push(tab_bar_prev);
    quads.push(tab_bar_next);

    // Dropdown-height slider (track → filled portion → handle).
    // Grayed to ~0.4 alpha when the window mode is Center (control is a no-op).
    let dim_alpha = |mut r: Rect| -> Rect {
        if !is_dropdown {
            r.color[3] = (r.color[3] as f32 * 0.4).round() as u8;
        }
        r
    };
    quads.push(dim_alpha(dropdown_track));
    quads.push(dim_alpha(dh_fill));       // accent-filled left portion
    quads.push(dim_alpha(dropdown_handle));

    // Dropdown-width slider (track → filled portion → handle). Grayed identically.
    quads.push(dim_alpha(dropdown_width_track));
    quads.push(dim_alpha(dw_fill));       // accent-filled left portion
    quads.push(dim_alpha(dropdown_width_handle));

    // Auto-hide toggle pill.
    quads.push(autohide_toggle);

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
        quads.push(Rect::rounded(
            chip.x - 2.0, chip.y - 2.0, chip.w + 4.0, chip.h + 4.0, accent_col, 7.0,
        ));
    }

    // Chip fills.
    quads.extend_from_slice(&chip_rects);

    // Color-dot quads for each theme card (3 dots: bg-neighbor, accent, bright).
    // Dots are 8×8 circles laid out at 3px from the card left edge, vertically
    // centred in the top half of the card so the name label fits below them.
    const DOT_R: f32 = 8.0;
    const DOT_GAP: f32 = 4.0;
    for i in 0..num_presets {
        let card = &chip_rects[i];
        let t = jetty_core::Theme::by_name(presets[i]);
        // Three representative colors: bg-lifted, accent (palette[4]), bright (palette[6]).
        let dot_colors = [
            // Slightly lifted bg — "surface" color.
            {
                let c = t.bg;
                let lift = 40u8;
                [
                    c[0].saturating_add(lift),
                    c[1].saturating_add(lift),
                    c[2].saturating_add(lift),
                    255,
                ]
            },
            [t.palette[4][0], t.palette[4][1], t.palette[4][2], 255], // accent
            [t.palette[2][0], t.palette[2][1], t.palette[2][2], 255], // green/bright
        ];
        let dot_y = card.y + 8.0; // top area of card
        for d in 0..3_usize {
            let dot_x = card.x + 8.0 + d as f32 * (DOT_R + DOT_GAP);
            quads.push(Rect::rounded(dot_x, dot_y, DOT_R, DOT_R, dot_colors[d], DOT_R / 2.0));
        }
    }

    // Opacity slider: dim track, then accent-filled left portion, then handle.
    quads.push(slider_track);
    quads.push(opacity_fill);
    quads.push(slider_handle);

    // Corner-radius slider: same pattern.
    quads.push(radius_track);
    quads.push(radius_fill);
    quads.push(radius_handle);

    // --- Labels ---
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push(("Settings".to_string(), px + 16.0, py + 12.0, text_main));

    // OPACITY header (py+48) — CAPS with right-aligned "97%" value.
    let pct = (opacity * 100.0).round() as i32;
    let pct_str = format!("{}%", pct);
    labels.push(("OPACITY".to_string(), px + 16.0, py + 48.0, text_header));
    labels.push((pct_str.clone(), right_x(&pct_str), py + 48.0, text_main));

    // CORNER RADIUS header (py+96) — CAPS with right-aligned "Npx" value.
    let radius_px = corner_radius.round() as i32;
    let radius_str = format!("{}px", radius_px);
    labels.push(("CORNER RADIUS".to_string(), px + 16.0, py + 96.0, text_header));
    labels.push((radius_str.clone(), right_x(&radius_str), py + 96.0, text_main));

    // Helper: center a (possibly truncated) cycle-name label between the ‹ and ›
    // buttons. The gap runs from [summon_prev_x+28] to [summon_next_x]; we clamp
    // the x so a long name never overruns either button.
    // Max chars that fit in the gap (108px / 9.8px ≈ 11 chars).
    const CYCLE_MAX_CHARS: usize = 11;
    let cycle_gap_left  = summon_prev_x + 28.0; // right edge of ‹ button
    let cycle_gap_right = summon_next_x;         // left edge of › button
    let cycle_gap_w     = cycle_gap_right - cycle_gap_left;
    let center_cycle = |name: &str| -> (String, f32) {
        // Truncate long names to avoid overrunning the buttons.
        let shown: String = if name.chars().count() > CYCLE_MAX_CHARS {
            let mut s: String = name.chars().take(CYCLE_MAX_CHARS - 1).collect();
            s.push('…');
            s
        } else {
            name.to_string()
        };
        let text_w = shown.chars().count() as f32 * CHAR_W;
        // Center in the gap, clamped so text stays between the buttons.
        let x = (cycle_gap_left + (cycle_gap_w - text_w) * 0.5)
            .clamp(cycle_gap_left, (cycle_gap_right - text_w).max(cycle_gap_left));
        (shown, x)
    };

    // SUMMON EFFECT section (py+144) — CAPS header.
    labels.push(("SUMMON EFFECT".to_string(), px + 16.0, py + 144.0, text_header));
    // Effect name centered between the ‹ / › buttons.
    {
        let (shown, name_x) = center_cycle(summon_effect_name);
        labels.push((shown, name_x, summon_btn_y + 6.0, text_main));
    }
    // Cycle button labels (‹ / ›).
    labels.push(("<".to_string(), summon_prev_x + 9.0, summon_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), summon_next_x + 9.0, summon_btn_y + 6.0, text_btn));

    // WINDOW MODE section (py+192) — CAPS header.
    labels.push(("WINDOW MODE".to_string(), px + 16.0, py + 192.0, text_header));
    {
        let (shown, name_x) = center_cycle(window_mode_name);
        labels.push((shown, name_x, winmode_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), summon_prev_x + 9.0, winmode_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), summon_next_x + 9.0, winmode_btn_y + 6.0, text_btn));

    // TAB BAR section (py+240) — CAPS header.
    labels.push(("TAB BAR".to_string(), px + 16.0, py + 240.0, text_header));
    {
        let (shown, name_x) = center_cycle(tab_bar_name);
        labels.push((shown, name_x, tabbar_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), summon_prev_x + 9.0, tabbar_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), summon_next_x + 9.0, tabbar_btn_y + 6.0, text_btn));

    // DROPDOWN HEIGHT section (py+300) — CAPS header + right-aligned value.
    // Grayed when mode==Center.
    let dh_text = if is_dropdown { text_header } else { text_btn };
    let dh_val_text = if is_dropdown { text_main } else { text_btn };
    let dh_pct = (dropdown_height_pct * 100.0).round() as i32;
    let dh_str = format!("{}%", dh_pct);
    labels.push(("DROPDOWN HEIGHT".to_string(), px + 16.0, py + 300.0, dh_text));
    labels.push((dh_str.clone(), right_x(&dh_str), py + 300.0, dh_val_text));

    // DROPDOWN WIDTH section (py+348) — CAPS header + right-aligned value.
    let dw_text = if is_dropdown { text_header } else { text_btn };
    let dw_val_text = if is_dropdown { text_main } else { text_btn };
    let dw_pct = (dropdown_width_pct * 100.0).round() as i32;
    let dw_str = format!("{}%", dw_pct);
    labels.push(("DROPDOWN WIDTH".to_string(), px + 16.0, py + 348.0, dw_text));
    labels.push((dw_str.clone(), right_x(&dw_str), py + 348.0, dw_val_text));

    // AUTO-HIDE section (py+396) — CAPS header with ON/OFF pill label.
    labels.push(("AUTO-HIDE ON FOCUS LOSS".to_string(), px + 16.0, py + 396.0, text_header));
    let (pill_text, pill_col) = if focus_autohide {
        ("ON", [20u8, 20, 20])
    } else {
        ("OFF", text_btn)
    };
    labels.push((
        pill_text.to_string(),
        autohide_toggle.x + 16.0,
        autohide_toggle.y + 6.0,
        pill_col,
    ));

    // FONT SIZE section (py+444) — CAPS header + right-aligned "Npt" value.
    let fs_display = font_size.round() as i32;
    let fs_str = format!("{}pt", fs_display);
    labels.push(("FONT SIZE".to_string(), px + 16.0, py + 444.0, text_header));
    labels.push((fs_str.clone(), right_x(&fs_str), py + 444.0, text_main));

    // Font button labels.
    labels.push(("-".to_string(),  font_minus_x + 9.0,  font_btn_y + 6.0,  text_btn));
    labels.push(("+".to_string(),  font_plus_x  + 8.0,  font_btn_y + 6.0,  text_btn));
    labels.push(("rst".to_string(), font_reset_x + 6.0, font_btn_y + 6.0,  text_btn));

    // FONT section header (py+504) — CAPS header; list starts at py+526.
    labels.push(("FONT".to_string(), px + 16.0, py + 504.0, text_header));

    // Scroll button labels (▲ / ▼).
    labels.push(("^".to_string(), scroll_up_x   + 6.0, scroll_btn_y + 4.0, text_btn));
    labels.push(("v".to_string(), scroll_down_x + 6.0, scroll_btn_y + 4.0, text_btn));

    // Font-family row labels.
    for i in 0..visible_count {
        let family_idx = offset + i;
        if let Some(name) = families.get(family_idx) {
            let row_y = list_top + i as f32 * (ROW_H + ROW_GAP) + 4.0;
            let is_selected = name.as_str() == selected_family;
            let text_color: [u8; 3] = if is_selected {
                text_main
            } else {
                text_dim
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
        // Right-align the "(shown/total)" hint so its right edge stays clear of
        // the ▲ scroll button (left edge at px+PANEL_W-60) regardless of how many
        // digits the counts have — on desktops with hundreds of fonts a fixed
        // px+PANEL_W-132 anchor overran the arrows.
        let hint = format!("({}/{})", offset + visible_count, families.len());
        let scroll_up_left = px + PANEL_W - 60.0;
        let hint_w = hint.chars().count() as f32 * CHAR_W;
        let hint_x = scroll_up_left - 6.0 - hint_w;
        labels.push((hint, hint_x, py + 504.0, text_dim));
    }

    // THEME section header (py+658) — CAPS, 12px gap after font-list bottom (py+646).
    labels.push(("THEME".to_string(), px + 16.0, py + 658.0, text_header));

    // Theme card name labels and color-dot labels.
    for i in 0..num_presets {
        let card = &chip_rects[i];
        // Pick black or white text per card based on its own bg luminance so the
        // name stays legible on any theme swatch.
        let cb = jetty_core::Theme::by_name(presets[i]).bg;
        let lum = 0.299 * cb[0] as f32 + 0.587 * cb[1] as f32 + 0.114 * cb[2] as f32;
        let label_color: [u8; 3] = if lum > 140.0 { [20, 20, 20] } else { [235, 235, 240] };
        // Theme name sits in the lower half of the card, below the dot row.
        labels.push((
            CHIP_NAMES[i].to_string(),
            card.x + 8.0,
            card.y + 22.0, // below the 3-dot row (dots at y+8, dot_h=8 → bottom y+16)
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
        ..Default::default()
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
        summon_prev,
        summon_next,
        win_mode_prev,
        win_mode_next,
        tab_bar_prev,
        tab_bar_next,
        dropdown_track,
        dropdown_handle,
        dropdown_width_track,
        dropdown_width_handle,
        autohide_toggle,
    };

    PanelView { quads, labels, geom }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a default panel view with representative inputs for assertion helpers.
    fn default_panel(screen_w: u32, screen_h: u32) -> PanelView {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let families: Vec<String> = vec![
            "JetBrains Mono".to_string(),
            "Fira Code".to_string(),
            "Hack".to_string(),
            "Source Code Pro".to_string(),
            "Inconsolata".to_string(),
            "Cascadia Code".to_string(),
        ];
        build_panel(
            screen_w,
            screen_h,
            0.97,            // opacity
            0,               // theme_idx = Mocha
            15.0,            // font_size
            &families,
            "JetBrains Mono",
            0,               // font_scroll_offset
            8.0,             // corner_radius
            "Phosphor",      // summon_effect_name
            "Dropdown",      // window_mode_name
            "Top",           // tab_bar_name
            0.5,             // dropdown_height_pct
            0.7,             // dropdown_width_pct
            true,            // is_dropdown
            false,           // focus_autohide
            0.0, 0.0,        // dx, dy
            &theme,
        )
    }

    #[test]
    fn panel_fits_on_screen_at_various_sizes() {
        // Screen sizes that can fully contain the panel (PANEL_H = 860, PANEL_W = 380).
        // Very small screens (< PANEL_H in height) clamp to y=0, which is correct
        // but means panel_bottom > screen_h — we only assert the panel stays ≥0
        // for those cases to avoid a false-failure on tiny displays.
        for (w, h) in [(1920u32, 1080u32), (1280, 900), (1440, 900), (2560, 1440)] {
            let pv = default_panel(w, h);
            let g = &pv.geom;
            let sw = w as f32;
            let sh = h as f32;
            assert!(
                g.panel.x >= 0.0,
                "panel.x < 0 at {w}×{h}: {}",
                g.panel.x
            );
            assert!(
                g.panel.y >= 0.0,
                "panel.y < 0 at {w}×{h}: {}",
                g.panel.y
            );
            assert!(
                g.panel.x + g.panel.w <= sw + 0.5,
                "panel overflows right at {w}×{h}: {} > {}",
                g.panel.x + g.panel.w,
                sw
            );
            assert!(
                g.panel.y + g.panel.h <= sh + 0.5,
                "panel overflows bottom at {w}×{h}: {} > {}",
                g.panel.y + g.panel.h,
                sh
            );
        }
        // Smaller screens: panel is clamped to y=0, x=0. Just assert non-negative.
        for (w, h) in [(1024u32, 768u32), (800, 600)] {
            let pv = default_panel(w, h);
            let g = &pv.geom;
            assert!(g.panel.x >= 0.0, "panel.x < 0 at {w}×{h}");
            assert!(g.panel.y >= 0.0, "panel.y < 0 at {w}×{h}");
        }
    }

    #[test]
    fn exactly_five_chips_in_presets_order() {
        // There are exactly 5 PRESETS; chips must match 1-to-1.
        let pv = default_panel(1920, 1080);
        assert_eq!(
            pv.geom.chips.len(),
            5,
            "expected 5 theme chips, got {}",
            pv.geom.chips.len()
        );
        // Every chip rect must lie inside the panel rect.
        let panel = &pv.geom.panel;
        for (i, chip) in pv.geom.chips.iter().enumerate() {
            assert!(
                chip.x >= panel.x && chip.x + chip.w <= panel.x + panel.w + 0.5,
                "chip[{i}] x out of panel: chip_x={} chip_x+w={} panel_x={} panel_right={}",
                chip.x, chip.x + chip.w, panel.x, panel.x + panel.w
            );
            assert!(
                chip.y >= panel.y && chip.y + chip.h <= panel.y + panel.h + 0.5,
                "chip[{i}] y out of panel: chip_y={} chip_y+h={} panel_y={} panel_bottom={}",
                chip.y, chip.y + chip.h, panel.y, panel.y + panel.h
            );
        }
    }

    #[test]
    fn slider_handles_within_tracks() {
        let pv = default_panel(1920, 1080);
        let g = &pv.geom;

        // Opacity slider: handle must start no earlier than track left and end
        // no later than track right.
        let track = &g.slider_track;
        let handle = &g.slider_handle;
        assert!(
            handle.x >= track.x - 0.5,
            "opacity handle left of track: hx={} tx={}",
            handle.x, track.x
        );
        assert!(
            handle.x + handle.w <= track.x + track.w + 0.5,
            "opacity handle right of track: hx+w={} tx+w={}",
            handle.x + handle.w, track.x + track.w
        );

        // Corner-radius slider.
        let rtrack = &g.radius_track;
        let rhandle = &g.radius_handle;
        assert!(rhandle.x >= rtrack.x - 0.5);
        assert!(rhandle.x + rhandle.w <= rtrack.x + rtrack.w + 0.5);

        // Dropdown-height slider.
        let dtrack = &g.dropdown_track;
        let dhandle = &g.dropdown_handle;
        assert!(dhandle.x >= dtrack.x - 0.5);
        assert!(dhandle.x + dhandle.w <= dtrack.x + dtrack.w + 0.5);

        // Dropdown-width slider.
        let dwtrack = &g.dropdown_width_track;
        let dwhandle = &g.dropdown_width_handle;
        assert!(dwhandle.x >= dwtrack.x - 0.5);
        assert!(dwhandle.x + dwhandle.w <= dwtrack.x + dwtrack.w + 0.5);
    }

    #[test]
    fn no_adjacent_control_bands_overlap_vertically() {
        // Check that major control bands don't overlap. We test select pairs of
        // rects that should be strictly above/below each other. Tolerates 1px
        // for rounding.
        let pv = default_panel(1920, 1080);
        let g = &pv.geom;
        let pairs: &[(&Rect, &Rect, &str)] = &[
            (&g.title_bar, &g.slider_track, "title_bar vs opacity_track"),
            (&g.slider_track, &g.radius_track, "opacity_track vs radius_track"),
            (&g.radius_track, &g.summon_prev, "radius_track vs summon_prev"),
            (&g.summon_prev, &g.win_mode_prev, "summon_prev vs win_mode_prev"),
            (&g.win_mode_prev, &g.tab_bar_prev, "win_mode_prev vs tab_bar_prev"),
            (&g.dropdown_track, &g.dropdown_width_track, "dropdown_h_track vs dropdown_w_track"),
            (&g.autohide_toggle, &g.font_minus, "autohide vs font_minus"),
            (&g.font_minus, &g.font_scroll_up, "font_minus vs font_scroll_up"),
        ];
        for (a, b, label) in pairs {
            let a_bottom = a.y + a.h;
            let b_top = b.y;
            assert!(
                a_bottom <= b_top + 1.0,
                "{label}: a_bottom={a_bottom} > b_top={b_top} (overlap)"
            );
        }
    }

    #[test]
    fn theme_cards_form_2col_grid() {
        let pv = default_panel(1920, 1080);
        let chips = &pv.geom.chips;
        assert_eq!(chips.len(), 5);

        // Column 0 chips (i=0,2,4) share the same x.
        let col0_x = chips[0].x;
        assert!((chips[2].x - col0_x).abs() < 0.5, "chip[2].x != chip[0].x");
        assert!((chips[4].x - col0_x).abs() < 0.5, "chip[4].x != chip[0].x");

        // Column 1 chips (i=1,3) share the same x, which is > col0_x.
        let col1_x = chips[1].x;
        assert!(col1_x > col0_x, "col1 not to the right of col0");
        assert!((chips[3].x - col1_x).abs() < 0.5, "chip[3].x != chip[1].x");

        // Row pairs are vertically separated.
        let row0_bottom = chips[0].y + chips[0].h;
        let row1_top = chips[2].y;
        assert!(row1_top >= row0_bottom - 0.5, "row1 overlaps row0");
        let row1_bottom = chips[2].y + chips[2].h;
        let row2_top = chips[4].y;
        assert!(row2_top >= row1_bottom - 0.5, "row2 overlaps row1");
    }

    #[test]
    fn caps_headers_present_in_labels() {
        let pv = default_panel(1920, 1080);
        let all_text: Vec<String> = pv.labels.iter().map(|l| l.0.clone()).collect();
        for expected in &[
            "OPACITY", "CORNER RADIUS", "SUMMON EFFECT", "WINDOW MODE",
            "TAB BAR", "DROPDOWN HEIGHT", "DROPDOWN WIDTH",
            "AUTO-HIDE ON FOCUS LOSS", "FONT SIZE", "FONT", "THEME",
        ] {
            assert!(
                all_text.iter().any(|s| s == expected),
                "missing CAPS header: {expected}"
            );
        }
    }

    #[test]
    fn right_aligned_values_in_labels() {
        // Opacity, corner-radius, font-size, dropdown values must appear as
        // separate right-aligned labels (not appended to the header string).
        let pv = default_panel(1920, 1080);
        let all_text: Vec<String> = pv.labels.iter().map(|l| l.0.clone()).collect();
        // With opacity=0.97 → "97%", corner_radius=8→"8px", font=15→"15pt", dh=50%→"50%"
        assert!(all_text.iter().any(|s| s == "97%"),  "missing opacity value label");
        assert!(all_text.iter().any(|s| s == "8px"),  "missing corner radius value label");
        assert!(all_text.iter().any(|s| s == "15pt"), "missing font size value label");
        assert!(all_text.iter().any(|s| s == "50%"),  "missing dropdown height value label");
    }
}
