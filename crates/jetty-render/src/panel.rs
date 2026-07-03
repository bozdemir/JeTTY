use crate::Rect;

/// Visual-effects parameters forwarded from `App.fx` (the `EffectsConfig`
/// runtime mirror). Defined here so `jetty-render` (which cannot depend on
/// `jetty-app`) can receive the full effects state through `build_panel`.
/// All fields mirror the corresponding `EffectsConfig` fields exactly.
#[derive(Debug, Clone)]
pub struct EffectsParams {
    pub crt_enabled: bool,
    pub crt_curvature: f32,
    pub crt_scanline: f32,
    pub crt_mask: f32,
    pub crt_bloom: f32,
    pub crt_chromatic: f32,
    pub crt_vignette: f32,
    pub crt_scanline_tint: [f32; 3],
    pub crt_animate_roll: bool,
    pub crt_flicker: bool,
    pub crt_jitter: bool,
    pub caret_flash_enabled: bool,
    pub caret_glow_enabled: bool,
    pub caret_flash_ms: f32,
    pub caret_flash_color: [f32; 3],
}

impl Default for EffectsParams {
    fn default() -> Self {
        EffectsParams {
            crt_enabled: false,
            crt_curvature: 0.30,
            crt_scanline: 0.50,
            crt_mask: 0.30,
            crt_bloom: 0.40,
            crt_chromatic: 0.20,
            crt_vignette: 0.40,
            crt_scanline_tint: [1.0, 1.0, 1.0],
            crt_animate_roll: false,
            crt_flicker: false,
            crt_jitter: false,
            caret_flash_enabled: true,
            caret_glow_enabled: false,
            caret_flash_ms: 130.0,
            caret_flash_color: [1.0, 1.0, 1.0],
        }
    }
}

/// Hit-testing geometry exposed for the upcoming mouse-interaction task.
pub struct PanelGeom {
    pub panel: Rect,
    /// The 5 tab-strip hit rects ("Look", "Fonts", "Window", "Shell", "Effects"), in order.
    /// A press in `tab_rects[i]` selects tab `i`. These are ALWAYS live (present
    /// regardless of the active tab) so the user can switch tabs.
    pub tab_rects: [Rect; 5],
    pub slider_track: Rect,
    pub slider_handle: Rect,
    /// Collapsed theme-picker combo header (Look tab). A press toggles the open
    /// state of the theme dropdown. Always live on the Look tab.
    pub theme_combo: Rect,
    /// One Rect per visible row in the OPEN theme dropdown; empty when closed.
    /// Index i maps to preset `theme_scroll_offset + i`.
    pub theme_rows: Vec<Rect>,
    /// Whether the theme dropdown is currently open (so hit-testing knows the rows
    /// and scroll arrows are live and should take priority).
    pub theme_open: bool,
    /// Scroll offset into PRESETS at render time (added to a clicked row index).
    pub theme_scroll_offset: usize,
    /// ▲ theme-list scroll button — decrements theme_scroll_offset (offscreen when
    /// the list is closed or doesn't overflow).
    pub theme_scroll_up: Rect,
    /// ▼ theme-list scroll button — increments theme_scroll_offset.
    pub theme_scroll_down: Rect,
    /// Font-size decrement button ("−").
    pub font_minus: Rect,
    /// Font-size increment button ("+").
    pub font_plus: Rect,
    /// Font-size reset button ("Reset").
    pub font_reset: Rect,
    /// One Rect per visible font-family row in the font picker list.
    /// Index i maps to `families[font_scroll_offset + i]`.
    pub font_rows: Vec<Rect>,
    /// The scroll offset into the families list at the time of rendering.
    pub font_scroll_offset: usize,
    /// The draggable title-bar strip at the top of the panel (~44px tall).
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
    /// "Auto-hide on focus loss" toggle switch.
    pub autohide_toggle: Rect,
    /// "Launch at login" toggle switch.
    pub launch_login_toggle: Rect,
    /// Shell-picker "‹" (previous) cycle button.
    pub shell_prev: Rect,
    /// Shell-picker "›" (next) cycle button.
    pub shell_next: Rect,
    // ── UI (chrome) FONT section ──────────────────────────────────────────────
    /// UI-font-size decrement button ("−").
    pub ui_font_minus: Rect,
    /// UI-font-size increment button ("+").
    pub ui_font_plus: Rect,
    /// UI-font-size reset button ("Reset").
    pub ui_font_reset: Rect,
    /// One Rect per visible UI-font-family row. Index i maps to
    /// `ui_families[ui_font_scroll_offset + i]` (index 0 = "System Sans").
    pub ui_font_rows: Vec<Rect>,
    /// The scroll offset into the UI-families list at the time of rendering.
    pub ui_font_scroll_offset: usize,
    /// ▲ UI-font scroll button — decrements ui_font_scroll_offset.
    pub ui_font_scroll_up: Rect,
    /// ▼ UI-font scroll button — increments ui_font_scroll_offset.
    pub ui_font_scroll_down: Rect,

    // ── Effects-tab geometry ──────────────────────────────────────────────────
    // All rects are OFF (1e6) when the Effects tab is not active.
    /// "CRT ENABLED" master toggle switch.
    pub crt_enabled_toggle: Rect,
    /// CRT curvature slider track and handle.
    pub crt_curvature_track: Rect,
    pub crt_curvature_handle: Rect,
    /// CRT scanline-intensity slider track and handle.
    pub crt_scanline_track: Rect,
    pub crt_scanline_handle: Rect,
    /// CRT shadow-mask slider track and handle.
    pub crt_mask_track: Rect,
    pub crt_mask_handle: Rect,
    /// CRT bloom slider track and handle.
    pub crt_bloom_track: Rect,
    pub crt_bloom_handle: Rect,
    /// CRT chromatic-aberration slider track and handle.
    pub crt_chromatic_track: Rect,
    pub crt_chromatic_handle: Rect,
    /// CRT vignette slider track and handle.
    pub crt_vignette_track: Rect,
    pub crt_vignette_handle: Rect,
    /// CRT scanline-tint RGB mini-sliders — one track+handle per channel.
    pub crt_tint_r_track: Rect,
    pub crt_tint_r_handle: Rect,
    pub crt_tint_g_track: Rect,
    pub crt_tint_g_handle: Rect,
    pub crt_tint_b_track: Rect,
    pub crt_tint_b_handle: Rect,
    /// CRT animation toggle chips (roll / flicker / jitter), three on one band.
    pub crt_roll_toggle: Rect,
    pub crt_flicker_toggle: Rect,
    pub crt_jitter_toggle: Rect,
    /// Caret flash-enabled toggle switch.
    pub caret_flash_toggle: Rect,
    /// Caret glow-enabled toggle switch.
    pub caret_glow_toggle: Rect,
    /// Caret flash-duration slider track and handle (maps 60..=400 ms → 0..1).
    pub caret_dur_track: Rect,
    pub caret_dur_handle: Rect,
    /// Caret flash-color RGB mini-sliders — one track+handle per channel.
    pub caret_color_r_track: Rect,
    pub caret_color_r_handle: Rect,
    pub caret_color_g_track: Rect,
    pub caret_color_g_handle: Rect,
    pub caret_color_b_track: Rect,
    pub caret_color_b_handle: Rect,
    /// Total pixel height of the Effects tab's content region:
    /// `last_band_bottom − content_top`. Stored for the scroll task.
    /// Independent of screen size; always `EFFECTS_CONTENT_H` (716.0 =
    /// 14 bands × 48 px pitch + 44 px for the last band's slider bottom).
    pub effects_content_h: f32,

    // ── Scroll viewport bounds (used by hit-testing for the Effects tab) ──────
    /// Y coordinate (physical px) where the scrollable content area begins, i.e.
    /// `panel.y + CONTENT_TOP_OFFSET`. Clicks above this row must not trigger
    /// Effects widgets even if an Effects rect has scrolled into that region.
    pub content_top: f32,
    /// Y coordinate (physical px) where the scrollable content area ends — the
    /// top of the footer band, i.e. `content_top + EFFECTS_VISIBLE_H`. Clicks at
    /// or below this row are outside the Effects viewport.
    pub content_bottom: f32,
}

impl PanelGeom {
    /// Scale every geometric field from `build_panel`'s logical layout space into
    /// physical pixels by `s`. Index / flag fields (`*_scroll_offset`,
    /// `theme_open`) are left untouched. Called once at the end of `build_panel`;
    /// the caller skips it entirely when `s == 1.0` so scale-1 output is unchanged.
    fn scale(&mut self, s: f32) {
        fn sr(r: &mut Rect, s: f32) {
            r.x *= s;
            r.y *= s;
            r.w *= s;
            r.h *= s;
            r.radius *= s;
        }
        macro_rules! scale_fields {
            ($($f:ident),* $(,)?) => { $( sr(&mut self.$f, s); )* };
        }
        scale_fields!(
            panel, slider_track, slider_handle, theme_combo, theme_scroll_up,
            theme_scroll_down, font_minus, font_plus, font_reset, title_bar,
            font_scroll_up, font_scroll_down, radius_track, radius_handle,
            summon_prev, summon_next, win_mode_prev, win_mode_next, tab_bar_prev,
            tab_bar_next, dropdown_track, dropdown_handle, dropdown_width_track,
            dropdown_width_handle, autohide_toggle, launch_login_toggle, shell_prev,
            shell_next, ui_font_minus, ui_font_plus, ui_font_reset,
            ui_font_scroll_up, ui_font_scroll_down, crt_enabled_toggle,
            crt_curvature_track, crt_curvature_handle, crt_scanline_track,
            crt_scanline_handle, crt_mask_track, crt_mask_handle, crt_bloom_track,
            crt_bloom_handle, crt_chromatic_track, crt_chromatic_handle,
            crt_vignette_track, crt_vignette_handle, crt_tint_r_track,
            crt_tint_r_handle, crt_tint_g_track, crt_tint_g_handle, crt_tint_b_track,
            crt_tint_b_handle, crt_roll_toggle, crt_flicker_toggle, crt_jitter_toggle,
            caret_flash_toggle, caret_glow_toggle, caret_dur_track, caret_dur_handle,
            caret_color_r_track, caret_color_r_handle, caret_color_g_track,
            caret_color_g_handle, caret_color_b_track, caret_color_b_handle,
        );
        for r in &mut self.tab_rects {
            sr(r, s);
        }
        for r in &mut self.theme_rows {
            sr(r, s);
        }
        for r in &mut self.font_rows {
            sr(r, s);
        }
        for r in &mut self.ui_font_rows {
            sr(r, s);
        }
        self.effects_content_h *= s;
        self.content_top *= s;
        self.content_bottom *= s;
    }
}

/// Full description of how to draw the settings panel for one frame.
pub struct PanelView {
    /// Chrome rects in draw order (border → bg → hairlines → control fills →
    /// slider tracks → list cards/rows).  These are rendered WITHOUT a scissor
    /// so the panel chrome is always fully visible.
    pub quads: Vec<Rect>,
    /// Chrome text labels: title, tab strip, section headers for non-Effects tabs,
    /// font/window/shell widget values, footer hint.  Rendered without clip bounds.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Effects-tab widget rects (CRT + Caret group), including the scrollbar
    /// indicator as the last element.  Empty when `active_tab != 4`.  Rendered
    /// with a hardware scissor rect equal to `effects_viewport` so content that
    /// has scrolled above or below the content area is GPU-clipped.
    pub effects_quads: Vec<Rect>,
    /// Effects-tab widget labels (CRT + Caret section).  Empty when
    /// `active_tab != 4`.  Rendered with glyphon `TextArea.bounds` = content
    /// viewport so labels outside the visible area are suppressed.
    pub effects_labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Scissor rect `[x, y, w, h]` in **physical pixels** covering the scrollable
    /// content viewport.  `Some` only when `active_tab == 4`; `None` otherwise
    /// (so the renderer can skip the scissored pass entirely on other tabs).
    pub effects_viewport: Option<[u32; 4]>,
    /// Pixel geometry for hit-testing (used by the next mouse-interaction task).
    pub geom: PanelGeom,
    /// Baseline (x, y) at which the app overdraws the live "Aa" specimen at the
    /// TRUE `ui_font_size` (not the capped panel-body size), after the capped
    /// panel-text pass — so the user sees an honest big/small/typeface preview
    /// even though the surrounding panel text is clamped. When the Fonts tab is
    /// NOT active, this is offscreen so the "Aa" is not drawn.
    pub ui_specimen_pos: (f32, f32),
}


/// Maximum number of font-family rows displayed in the panel at once.
/// If more families exist, the list scrolls via `font_scroll_offset`.
const MAX_FONT_ROWS: usize = 5;

/// Maximum number of UI-font-family rows shown at once. Kept to 4 (not 5) to
/// bound the panel's vertical growth from the new section.
const MAX_UI_FONT_ROWS: usize = 4;

/// Maximum number of theme rows shown at once in the open theme dropdown. With
/// more presets than this the list scrolls via `theme_scroll_offset`. Sized so
/// the expanded menu still fits inside PANEL_H below the combo header.
const MAX_THEME_ROWS: usize = 9;

/// The 8 ANSI palette indices shown as the per-theme swatch strip (the 8 "normal"
/// ANSI colors: black, red, green, yellow, blue, magenta, cyan, white).
const THEME_SWATCH_IDX: [usize; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

/// Per-swatch geometry for the theme color strip.
const THEME_SW_W: f32 = 11.0;
const THEME_SW_H: f32 = 12.0;
const THEME_SW_GAP: f32 = 2.0;
/// Total width of the 8-swatch strip (8 swatches + 7 gaps).
const THEME_STRIP_W: f32 = 8.0 * THEME_SW_W + 7.0 * THEME_SW_GAP; // 102

/// Push the 8-swatch ANSI strip for `t` into `quads`, left edge at `strip_left`,
/// vertically centered on `center_y`. Used by both the collapsed combo header and
/// every open dropdown row so the two always render identical strips.
fn push_theme_swatches(quads: &mut Vec<Rect>, strip_left: f32, center_y: f32, t: &jetty_core::Theme) {
    let sy = center_y - THEME_SW_H / 2.0;
    for (k, &idx) in THEME_SWATCH_IDX.iter().enumerate() {
        let c = t.palette[idx];
        let sx = strip_left + k as f32 * (THEME_SW_W + THEME_SW_GAP);
        quads.push(Rect::rounded(sx, sy, THEME_SW_W, THEME_SW_H, [c[0], c[1], c[2], 255], 2.0));
    }
}

/// The five settings-tab labels, in order. The active tab's bands are the only
/// ones laid out on-screen; every other tab's widgets are positioned offscreen.
const TAB_NAMES: [&str; 5] = ["Look", "Fonts", "Window", "Shell", "Effects"];

/// Y at which an INACTIVE tab's band tops are placed. Far offscreen so every
/// rect derived from such a band lands well past the panel (and the screen):
/// `point_in` uses INCLUSIVE bounds, so a zero-rect at the origin could spuriously
/// match a click at exactly (0,0); pushing inactive geometry offscreen instead is
/// unconditionally safe, and the GPU clips the offscreen quads/labels away.
const OFF: f32 = 1.0e6;

/// Fallback chrome-font character advance (px), used when a measured advance is
/// not available. Passed in via the `char_w` parameter of `build_panel` on
/// HiDPI-aware paths; matches the constant used in help.rs/tabbar.rs.
/// Referenced by tests in this module; the `#[cfg(test)]` gating causes the
/// compiler to warn about dead code in non-test builds, hence the allow.
#[allow(dead_code)]
/// Reference chrome cell advance (scale-1). `build_panel` lays out in LOGICAL
/// space as if `char_w == CHAR_W_FALLBACK` and scales by `dpi = char_w /
/// CHAR_W_FALLBACK`. Exposed so callers (e.g. the Effects-tab scroll clamp in
/// app.rs) can express bounds in the SAME dpi units the panel consumes (F10).
pub const CHAR_W_FALLBACK: f32 = 9.8;

/// Settings-panel dimensions in logical px. The separate Settings OS window is
/// sized to these (+ border) — see `SETTINGS_WIN_*` in jetty-app. Growing the
/// panel here automatically resizes that window.
///
/// The panel is organised into FIVE tabs (a strip of clickable labels sits just
/// under the title): only the active tab's bands are laid out, so PANEL_H is
/// sized to the TALLEST tab rather than the sum of every band:
///
/// * Tab 0 "Look"    — opacity slider, corner-radius slider, theme dropdown.
/// * Tab 1 "Fonts"   — font-size, font-family list, UI-font-size + "Aa" specimen,
///   UI-font-family list. (TALLEST non-scrolling tab — drives PANEL_H.)
/// * Tab 2 "Window"  — summon effect, window mode, dropdown height, dropdown
///   width, tab-bar position, auto-hide toggle.
/// * Tab 3 "Shell"   — shell picker, launch-at-login toggle.
/// * Tab 4 "Effects" — CRT controls (enable + curvature/scanline/mask/bloom/
///   chromatic/vignette/tint/animate) and Caret flash/glow.
///   Its content exceeds PANEL_H, so this tab alone scrolls
///   (GPU-clipped to the viewport; see `effects_viewport`).
///
/// All five tabs share one `content_top` (py + CONTENT_TOP_OFFSET) and lay their
/// bands out top-down from there. A fixed footer band (FOOTER_H) with a hairline
/// and a hint anchors the bottom edge so shorter tabs still read as a complete,
/// bounded sheet rather than trailing off into dead space.
pub const PANEL_W: f32 = 420.0;
pub const PANEL_H: f32 = 592.0;

/// Horizontal padding between the panel edge and content (the 8-px grid ×2.5;
/// every band, slider, and list aligns to `px + PAD` / `px + PANEL_W − PAD`).
const PAD: f32 = 20.0;
/// Content width: `PANEL_W − 2·PAD`.
const CW: f32 = PANEL_W - 2.0 * PAD; // 380

/// Y-offset from the panel's top edge to the scrollable content area (title bar
/// + tab strip chrome + breathing room). Single-sourced here so
///   `EFFECTS_VISIBLE_H` and the `content_top` local in `build_panel` can both
///   derive from it.
const CONTENT_TOP_OFFSET: f32 = 96.0;

/// Height of the fixed footer band at the panel bottom (hairline + hint text).
/// The Effects scroll viewport ends where the footer begins.
const FOOTER_H: f32 = 36.0;

/// Shared control metrics: every button, cycler, stepper and combo is CTL_H
/// tall with the same corner radius, so the whole panel reads as one system.
const CTL_H: f32 = 28.0;
const R_CTL: f32 = 6.0;

/// Toggle-switch metrics (a knob-in-track switch replaces the old ON/OFF pill).
const SW_W: f32 = 44.0;
const SW_H: f32 = 24.0;

/// List rows (font / UI-font pickers) inside their inset card.
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 2.0;
/// Inner padding of list cards (and the open theme menu).
const LIST_PAD: f32 = 5.0;

/// Theme-dropdown row height (taller than plain list rows: swatch strip inside).
const THEME_ROW_H: f32 = 28.0;
const THEME_ROW_GAP: f32 = 2.0;

/// Effects-tab band pitch (uniform; sliders and toggles share it).
const FX_PITCH: f32 = 48.0;

/// Total pixel height of the Effects-tab content region (14 band pitches +
/// 44 px for the last band's slider bottom + breathing room). Single source of
/// truth for the scroll clamp in `App` and the scrollbar indicator in
/// `build_panel`. Value: `14 × 48 + 44 = 716.0`.
pub const EFFECTS_CONTENT_H: f32 = 14.0 * FX_PITCH + 44.0;

/// Visible height of the Effects-tab content viewport (the panel height minus
/// the top chrome and the footer band). Scroll is clamped to
/// `[0, EFFECTS_CONTENT_H − EFFECTS_VISIBLE_H]`.
/// Value: `PANEL_H − CONTENT_TOP_OFFSET − FOOTER_H = 460.0`.
pub const EFFECTS_VISIBLE_H: f32 = PANEL_H - CONTENT_TOP_OFFSET - FOOTER_H;


/// Build the settings panel for the given screen size, opacity (0.1..=1.0),
/// selected theme index (index into `jetty_core::theme::PRESETS`), current
/// logical font size (`font_size`), the list of monospace font families, the
/// currently selected family name, the scroll offset into the family list, and
/// a user drag offset (`dx`, `dy`) added to the centered position so the dialog
/// can be moved. The panel is clamped to remain fully on-screen.
///
/// `active_tab` (0..=4) selects which group of bands is laid out; every other
/// tab's widgets are positioned offscreen so hit-tests can never match them.
///
/// `char_w` is the measured physical-pixel advance of one chrome-font character
/// (from `TextLayer::cell_size().0`). Pass `CHAR_W_FALLBACK` (9.8) when a real
/// measurement is not available (scale-1 fallback used by tests).
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
    // `launch_at_login`: drives the "LAUNCH AT LOGIN" toggle switch (accent when
    //   ON). The app derives this from the XDG autostart file's existence.
    launch_at_login: bool,
    // ── UI (chrome) FONT section inputs ──
    // `ui_font_size`: the TRUE UI font size in logical points (10..=28). Drives
    //   the "Npt" readout and the live "Aa" specimen size; NOT clamped here (the
    //   panel's own body text is clamped separately by the caller's capped layer).
    // `ui_families`: proportional UI-font candidates, with a synthetic index-0
    //   "System Sans (default)" row already prepended by the caller.
    // `selected_ui_family`: the selected UI family ("" highlights index 0).
    // `ui_font_scroll_offset`: scroll offset into `ui_families`.
    ui_font_size: f32,
    ui_families: &[String],
    selected_ui_family: &str,
    ui_font_scroll_offset: usize,
    dx: f32,
    dy: f32,
    theme: &jetty_core::Theme,
    char_w: f32,
    // `shell_display`: the basename of the selected shell (e.g. "zsh"), or
    //   "System default" when the `shell` config key is empty. Drives the SHELL
    //   cycler band's centered name, mirroring `window_mode_name`.
    shell_display: &str,
    // `active_tab`: 0..=4 — which tab's bands are laid out (see TAB_NAMES). Values
    //   above 4 are clamped to 4. Session-only; not persisted.
    active_tab: usize,
    // `effects`: visual-effect parameters from the app's runtime `EffectsConfig`
    //   mirror (`App.fx`). Used on the Effects tab (4) to position widget handles
    //   and toggle switch states. Ignored on other tabs (bands are at OFF).
    effects: &EffectsParams,
    // `effects_scroll`: vertical scroll offset (physical px, 0 = top) for the
    //   Effects tab.  Subtracted from every `t_fx_*` band top when active_tab==4
    //   so the drawn positions and the PanelGeom hit-rects stay in sync.
    //   Clamped to [0, max(0, content_h - visible_h)] by the caller (App).
    //   Ignored on other tabs (bands are at OFF regardless).
    effects_scroll: f32,
    // `theme_dropdown_open`: whether the Look-tab theme picker is expanded into its
    //   scrollable overlay list. When false only the collapsed combo header shows.
    //   Session-only; not persisted.
    theme_dropdown_open: bool,
    // `theme_scroll_offset`: first visible row index into PRESETS when the theme
    //   dropdown is open. Clamped here; ignored when closed.
    theme_scroll_offset: usize,
) -> PanelView {
    let active_tab = active_tab.min(4);

    // ── HiDPI layout scale ────────────────────────────────────────────────────
    // The panel's window is created at a LOGICAL size (SETTINGS_WIN_* = PANEL_W/H
    // + border) but `build_panel` receives the PHYSICAL surface size and a
    // PHYSICAL, DPI-scaled chrome advance (`char_w`). Laying the fixed-px
    // constants out directly against the physical surface leaves every control the
    // designed logical size while the glyphs are `char_w` (≈scale×) wide, so text
    // overflows its control on HiDPI. Instead we lay the whole panel out in a
    // LOGICAL space — as if `char_w == CHAR_W_FALLBACK` — and scale the finished
    // geometry by `dpi` on the way out, so controls/rows/text stay proportional at
    // any DPI. `dpi` is recovered from the measured advance (the only scale signal
    // available without a signature change); at scale 1 the tests pass
    // `CHAR_W_FALLBACK`, so `dpi == 1.0` and every output below is byte-identical.
    // NOTE: `char_w` is the CAPPED settings advance (jetty-app caps the panel body
    // font to [13,17] pt), so for very large UI fonts the panel scales with the
    // capped text, not the raw font — it stays bounded rather than filling more of
    // its window. The settings window itself is sized in jetty-app (not editable
    // here); this fix corrects the intra-panel proportions, which is the visible
    // breakage. Physical inputs (drag, scroll) are converted to logical here.
    let dpi = (char_w / CHAR_W_FALLBACK).max(0.1);
    let char_w = CHAR_W_FALLBACK;
    let dx = dx / dpi;
    let dy = dy / dpi;
    let effects_scroll = effects_scroll / dpi;

    // --- Theme-derived panel chrome colors ---
    // All panel colors are derived from the ACTIVE theme so the settings window
    // re-skins itself when the theme changes (instead of staying a fixed dark
    // gray). `lerp` blends bg→fg by `t` for shades that keep contrast on both
    // dark and light themes. Accent (palette blue) is reserved for ACTIVE /
    // SELECTED state only: the tab underline, slider fills+knobs, switches that
    // are ON, and the selected list row.
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
    let lerp4 = |t: f32, a: u8| -> [u8; 4] {
        let c = lerp(t);
        [c[0], c[1], c[2], a]
    };
    // Panel surface: a slightly lifted shade of the theme bg, made nearly opaque.
    let panel_bg: [u8; 4] = lerp4(0.06, 246);
    // Border: a lighter shade so the panel reads as a card on any theme.
    let panel_border: [u8; 4] = lerp4(0.22, 255);
    // Hairline separators (under the tab strip, above the footer, section rules).
    let hairline: [u8; 4] = lerp4(0.14, 255);
    // Control fills: one shade for every button/cycler/stepper/combo.
    let ctl_fill: [u8; 4] = lerp4(0.10, 255);
    // Thin separators INSIDE segmented controls (cycler / stepper).
    let seg_sep: [u8; 4] = lerp4(0.20, 255);
    // Slider track: dim shade.
    let slider_track_col: [u8; 4] = lerp4(0.16, 255);
    // Inset list-card well (font/UI-font pickers): slightly darker than the panel.
    let card_fill: [u8; 4] = lerp4(0.02, 255);
    // Open theme menu surface: slightly lifted so it floats over the panel.
    let menu_fill: [u8; 4] = lerp4(0.09, 255);
    let menu_border: [u8; 4] = lerp4(0.28, 255);
    // Accent for handles / switch-on / selection.
    let accent_col: [u8; 4] = [accent[0], accent[1], accent[2], 255];
    // Accent fill for the "filled" (left) portion of slider tracks.
    let accent_fill: [u8; 4] = [accent[0], accent[1], accent[2], 200];
    // Selected list-row background: a calm accent tint (≈25% accent over bg).
    let row_sel: [u8; 4] = [
        ((tbg[0] as u16 * 3 + accent[0] as u16) / 4) as u8,
        ((tbg[1] as u16 * 3 + accent[1] as u16) / 4) as u8,
        ((tbg[2] as u16 * 3 + accent[2] as u16) / 4) as u8,
        255,
    ];
    // Switch knob colors: bg-toned on the accent track (ON), dim on the idle track.
    let knob_on: [u8; 4] = [tbg[0], tbg[1], tbg[2], 255];
    let knob_off3 = lerp(0.55);
    let knob_off: [u8; 4] = [knob_off3[0], knob_off3[1], knob_off3[2], 255];
    // Text colors — the hierarchy: values (main) > rows (dim) > CAPS headers.
    let text_main = tfg;
    let text_dim = lerp(0.72);
    let text_btn = lerp(0.75);
    // Section headers (CAPS) are the QUIETEST text on the sheet so they read as
    // labels; the right-aligned values in text_main carry the optical weight.
    let text_header = lerp(0.52);
    // Inline helper/hint text (footer, per-setting descriptions).
    let text_hint = lerp(0.42);
    // Chip/pill text on an accent fill (theme bg so it contrasts the accent).
    let on_accent: [u8; 3] = [tbg[0], tbg[1], tbg[2]];

    // Center, then apply the user drag offset, then clamp to screen edges.
    // Logical surface size (physical / dpi): the layout below is entirely logical.
    let sw = screen_w as f32 / dpi;
    let sh = screen_h as f32 / dpi;
    let px = (((sw - PANEL_W) / 2.0).floor() + dx).clamp(0.0, (sw - PANEL_W).max(0.0));
    let py = (((sh - PANEL_H) / 2.0).floor() + dy).clamp(0.0, (sh - PANEL_H).max(0.0));

    // ── Per-band tops ────────────────────────────────────────────────────────
    // Every band keeps its internal (sub-element) layout relative to a single
    // "band top" Y. For the ACTIVE tab the band tops run top-down from
    // `content_top`; for every other tab they are OFF (offscreen), so all rects
    // and labels derived from them fall far offscreen and can neither be hit-tested
    // nor seen. This keeps app.rs's hit-test logic tab-agnostic.
    //
    // Band pitches (8-px grid): single-line control rows 48, slider rows 56,
    // list sections (header + card) per their row count.
    //
    //   Tab 0 "Look":   opacity(56) · corner-radius(56) · theme combo(+menu)
    //   Tab 1 "Fonts":  size stepper(44) · font list(184) · UI stepper+Aa(84) · UI list
    //   Tab 2 "Window": summon(48) · win-mode(48) · drop-h(56) · drop-w(56) ·
    //                   tab-bar(48) · auto-hide
    //   Tab 3 "Shell":  shell cycler + hint(56) · launch-at-login + hint
    let content_top = py + CONTENT_TOP_OFFSET;
    let (mut t_opacity, mut t_radius, mut t_theme) = (OFF, OFF, OFF);
    let (mut t_fontsize, mut t_fontlist, mut t_uifontsize, mut t_uifontlist) =
        (OFF, OFF, OFF, OFF);
    let (mut t_summon, mut t_winmode, mut t_droph, mut t_dropw, mut t_tabbar, mut t_autohide) =
        (OFF, OFF, OFF, OFF, OFF, OFF);
    let (mut t_shell, mut t_launch) = (OFF, OFF);
    // Effects-tab band tops (15 bands, FX_PITCH px pitch each).
    // Naming: t_fx_<widget>. OFF when tab 4 is not active.
    let (mut t_fx_crt_hdr, mut t_fx_crt_en, mut t_fx_curv, mut t_fx_scan,
         mut t_fx_mask,    mut t_fx_bloom,  mut t_fx_chroma, mut t_fx_vignette,
         mut t_fx_tint,    mut t_fx_anim,
         mut t_fx_caret_hdr, mut t_fx_flash, mut t_fx_glow,
         mut t_fx_dur,    mut t_fx_color) =
        (OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF, OFF);
    match active_tab {
        0 => {
            t_opacity = content_top;
            t_radius = content_top + 56.0;
            t_theme = content_top + 112.0;
        }
        1 => {
            t_fontsize = content_top;
            t_fontlist = content_top + 44.0;
            t_uifontsize = content_top + 228.0;
            t_uifontlist = content_top + 312.0;
        }
        2 => {
            t_summon = content_top;
            t_winmode = content_top + 48.0;
            t_droph = content_top + 96.0;
            t_dropw = content_top + 152.0;
            t_tabbar = content_top + 208.0;
            t_autohide = content_top + 256.0;
        }
        3 => {
            t_shell = content_top;
            t_launch = content_top + 64.0;
        }
        4 => {
            // 15 bands × FX_PITCH px pitch, top-to-bottom from content_top.
            // `effects_scroll` (physical px) is subtracted from each band top so
            // the drawn positions and the PanelGeom hit-rects are identical —
            // the caller (App) has already clamped it to [0, max_scroll].
            // The OFF sentinel for inactive tabs is NOT modified here.
            let s = effects_scroll; // shorthand
            t_fx_crt_hdr  = content_top                  - s; // band 0: "CRT" section header
            t_fx_crt_en   = content_top +  1.0*FX_PITCH - s; // band 1: crt_enabled switch
            t_fx_curv     = content_top +  2.0*FX_PITCH - s; // band 2: crt_curvature slider
            t_fx_scan     = content_top +  3.0*FX_PITCH - s; // band 3: crt_scanline slider
            t_fx_mask     = content_top +  4.0*FX_PITCH - s; // band 4: crt_mask slider
            t_fx_bloom    = content_top +  5.0*FX_PITCH - s; // band 5: crt_bloom slider
            t_fx_chroma   = content_top +  6.0*FX_PITCH - s; // band 6: crt_chromatic slider
            t_fx_vignette = content_top +  7.0*FX_PITCH - s; // band 7: crt_vignette slider
            t_fx_tint     = content_top +  8.0*FX_PITCH - s; // band 8: crt_scanline_tint RGB
            t_fx_anim     = content_top +  9.0*FX_PITCH - s; // band 9: roll/flicker/jitter chips
            t_fx_caret_hdr= content_top + 10.0*FX_PITCH - s; // band 10: "CARET" section header
            t_fx_flash    = content_top + 11.0*FX_PITCH - s; // band 11: caret_flash_enabled switch
            t_fx_glow     = content_top + 12.0*FX_PITCH - s; // band 12: caret_glow_enabled switch
            t_fx_dur      = content_top + 13.0*FX_PITCH - s; // band 13: caret_flash_ms slider
            t_fx_color    = content_top + 14.0*FX_PITCH - s; // band 14: caret_flash_color RGB
        }
        _ => {
            t_shell = content_top;
            t_launch = content_top + 64.0;
        }
    }

    // --- Full-screen dim quad (drawn before everything else) ---
    let dim_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: sw,
        h: sh,
        color: [0, 0, 0, 140], ..Default::default() };

    // --- Border + background ---
    let border_rect = Rect::rounded(
        px - 2.0, py - 2.0, PANEL_W + 4.0, PANEL_H + 4.0, panel_border, 11.0,
    );
    let bg_rect = Rect::rounded(px, py, PANEL_W, PANEL_H, panel_bg, 9.0);

    // --- Title bar (draggable handle: py+0 .. py+44) ---
    let title_bar = Rect {
        x: px,
        y: py,
        w: PANEL_W,
        h: 44.0,
        color: [0, 0, 0, 0], // drawn via bg; color unused for hit-test
        ..Default::default()
    };

    // --- Tab strip (py+44 .. py+76) ---
    // Leading-aligned cells sized to their label (text + adaptive side padding),
    // sitting on a full-width hairline at py+76; the active tab carries a 2-px
    // accent underline exactly as wide as its label (+8) that meets the hairline.
    let tab_names_chars: [f32; 5] = {
        let mut a = [0.0; 5];
        for (i, n) in TAB_NAMES.iter().enumerate() {
            a[i] = n.chars().count() as f32;
        }
        a
    };
    let tabs_text_w: f32 = tab_names_chars.iter().map(|c| c * char_w).sum();
    // Adaptive per-side cell padding: use the slack left of CW, capped to 14 px
    // so the strip stays leading-aligned (never stretches to fill).
    let tab_pad = ((CW - tabs_text_w) / 10.0).clamp(4.0, 14.0);
    let tab_strip_y = py + 52.0; // label baseline
    let mut tab_rects: [Rect; 5] = [Rect::default(); 5];
    {
        let mut cx = px + PAD - tab_pad; // first label still starts at px+PAD
        for (i, r) in tab_rects.iter_mut().enumerate() {
            let cell_w = tab_names_chars[i] * char_w + 2.0 * tab_pad;
            *r = Rect {
                x: cx,
                y: py + 44.0,
                w: cell_w,
                h: 32.0,
                color: [0, 0, 0, 0],
                ..Default::default()
            };
            cx += cell_w;
        }
    }

    // Helper: right-align a text value against the content right edge.
    let right_x = |text: &str| -> f32 {
        let w = text.chars().count() as f32 * char_w;
        px + PANEL_W - PAD - w
    };
    // Helper: center `text` inside [left, left+width].
    let center_x = |text: &str, left: f32, width: f32| -> f32 {
        left + (width - text.chars().count() as f32 * char_w) * 0.5
    };

    // ── Shared control constructors ──────────────────────────────────────────
    // Full-width slider: CAPS header + right value on line 1, a 4-px track with
    // a 16-px round knob on line 2. Returns (hit, track, fill, handle): `hit` is
    // a taller invisible strip stored in PanelGeom so the thin track stays an
    // easy click/drag target; only track/fill/handle are drawn.
    let track_x = px + PAD;
    let slider_at = |t: f32, frac: f32| -> (Rect, Rect, Rect, Rect) {
        let frac = frac.clamp(0.0, 1.0);
        let hit = Rect {
            x: track_x, y: t + 22.0, w: CW, h: 20.0,
            color: [0, 0, 0, 0], ..Default::default()
        };
        let track = Rect::rounded(track_x, t + 30.0, CW, 4.0, slider_track_col, 2.0);
        let fill_w = (frac * (CW - 16.0) + 8.0).clamp(4.0, CW);
        let fill = Rect::rounded(track_x, t + 30.0, fill_w, 4.0, accent_fill, 2.0);
        let handle = Rect::rounded(track_x + frac * (CW - 16.0), t + 24.0, 16.0, 16.0, accent_col, 8.0);
        (hit, track, fill, handle)
    };
    // 90-px RGB mini-slider (same knob language, smaller).
    let mini_slider_at = |bx: f32, t: f32, frac: f32| -> (Rect, Rect, Rect, Rect) {
        let frac = frac.clamp(0.0, 1.0);
        let hit = Rect {
            x: bx, y: t + 22.0, w: 90.0, h: 20.0,
            color: [0, 0, 0, 0], ..Default::default()
        };
        let track = Rect::rounded(bx, t + 30.0, 90.0, 4.0, slider_track_col, 2.0);
        let fill_w = (frac * (90.0 - 14.0) + 7.0).clamp(4.0, 90.0);
        let fill = Rect::rounded(bx, t + 30.0, fill_w, 4.0, accent_fill, 2.0);
        let handle = Rect::rounded(bx + frac * (90.0 - 14.0), t + 25.0, 14.0, 14.0, accent_col, 7.0);
        (hit, track, fill, handle)
    };
    // Toggle switch: knob-in-track, right-aligned on its band. Returns
    // (track, knob); the track rect doubles as the hit target in PanelGeom.
    let switch_x = px + PANEL_W - PAD - SW_W;
    let switch_at = |t: f32, on: bool| -> (Rect, Rect) {
        let track_col = if on { accent_col } else { slider_track_col };
        let track = Rect::rounded(switch_x, t + 2.0, SW_W, SW_H, track_col, SW_H / 2.0);
        let (kx, kc) = if on {
            (switch_x + SW_W - 21.0, knob_on)
        } else {
            (switch_x + 3.0, knob_off)
        };
        let knob = Rect::rounded(kx, t + 5.0, 18.0, 18.0, kc, 9.0);
        (track, knob)
    };

    // ── Cycler: one segmented control  [<] value [>]  right-aligned ──────────
    const CYC_W: f32 = 210.0;
    const CYC_SEG: f32 = 32.0;
    let cyc_x = px + PANEL_W - PAD - CYC_W;
    let cycle_prev_x = cyc_x; // left segment
    let cycle_next_x = cyc_x + CYC_W - CYC_SEG; // right segment
    let cycler_at = |t: f32| -> (Rect, Rect, Rect) {
        let body = Rect::rounded(cyc_x, t, CYC_W, CTL_H, ctl_fill, R_CTL);
        let prev = Rect {
            x: cycle_prev_x, y: t, w: CYC_SEG, h: CTL_H,
            color: [0, 0, 0, 0], ..Default::default()
        };
        let next = Rect {
            x: cycle_next_x, y: t, w: CYC_SEG, h: CTL_H,
            color: [0, 0, 0, 0], ..Default::default()
        };
        (body, prev, next)
    };

    // ── Stepper: one segmented control  [−] value [+]  (+ Reset) ─────────────
    const STEP_W: f32 = 116.0;
    const STEP_SEG: f32 = 36.0;
    const RESET_W: f32 = 64.0;
    let step_x = px + PANEL_W - PAD - STEP_W;
    let reset_x = step_x - 8.0 - RESET_W;
    let stepper_at = |t: f32| -> (Rect, Rect, Rect, Rect) {
        let body = Rect::rounded(step_x, t, STEP_W, CTL_H, ctl_fill, R_CTL);
        let minus = Rect {
            x: step_x, y: t, w: STEP_SEG, h: CTL_H,
            color: [0, 0, 0, 0], ..Default::default()
        };
        let plus = Rect {
            x: step_x + STEP_W - STEP_SEG, y: t, w: STEP_SEG, h: CTL_H,
            color: [0, 0, 0, 0], ..Default::default()
        };
        let reset = Rect::rounded(reset_x, t, RESET_W, CTL_H, ctl_fill, R_CTL);
        (body, minus, plus, reset)
    };

    // --- Look tab: opacity + corner-radius sliders ---
    let frac = ((opacity - 0.1) / 0.9).clamp(0.0, 1.0);
    let (slider_track, opacity_track_q, opacity_fill, slider_handle) = slider_at(t_opacity, frac);
    const RADIUS_MAX: f32 = 24.0;
    let r_frac = (corner_radius / RADIUS_MAX).clamp(0.0, 1.0);
    let (radius_track, radius_track_q, radius_fill, radius_handle) = slider_at(t_radius, r_frac);

    // --- Window tab: cyclers ---
    let (summon_body, summon_prev, summon_next) = cycler_at(t_summon);
    let (winmode_body, win_mode_prev, win_mode_next) = cycler_at(t_winmode);
    let (tabbar_body, tab_bar_prev, tab_bar_next) = cycler_at(t_tabbar);

    // --- Window tab: dropdown height / width sliders ---
    let dh_frac = ((dropdown_height_pct - 0.25) / 0.75).clamp(0.0, 1.0);
    let (dropdown_track, dh_track_q, dh_fill, dropdown_handle) = slider_at(t_droph, dh_frac);
    let dw_frac = ((dropdown_width_pct - 0.2) / 0.8).clamp(0.0, 1.0);
    let (dropdown_width_track, dw_track_q, dw_fill, dropdown_width_handle) =
        slider_at(t_dropw, dw_frac);

    // --- Window tab: auto-hide switch ---
    let (autohide_toggle, autohide_knob) = switch_at(t_autohide, focus_autohide);

    // --- Fonts tab: terminal font-size stepper ---
    let (font_step_body, font_minus, font_plus, font_reset) = stepper_at(t_fontsize);

    // --- Fonts tab: font-family list (header row + inset card) ---
    // Scroll arrows live on the header row, right-aligned; the "n/total"
    // counter sits just left of them.
    let arrow_dn_x = px + PANEL_W - PAD - 22.0;
    let arrow_up_x = arrow_dn_x - 26.0;
    let font_scroll_up = Rect::rounded(arrow_up_x, t_fontlist - 1.0, 22.0, 22.0, ctl_fill, R_CTL);
    let font_scroll_down = Rect::rounded(arrow_dn_x, t_fontlist - 1.0, 22.0, 22.0, ctl_fill, R_CTL);

    let list_x = px + PAD;
    let card_h = |rows: usize| -> f32 {
        2.0 * LIST_PAD + rows as f32 * ROW_H + rows.saturating_sub(1) as f32 * ROW_GAP
    };
    let font_card = Rect::rounded(
        list_x, t_fontlist + 24.0, CW, card_h(MAX_FONT_ROWS), card_fill, 8.0,
    );
    let offset = font_scroll_offset.min(families.len().saturating_sub(1));
    let visible_count = (families.len().saturating_sub(offset)).min(MAX_FONT_ROWS);
    let row_x = list_x + LIST_PAD;
    let row_w = CW - 2.0 * LIST_PAD;
    let mut font_row_rects: Vec<Rect> = Vec::with_capacity(visible_count);
    for i in 0..visible_count {
        let row_y = font_card.y + LIST_PAD + i as f32 * (ROW_H + ROW_GAP);
        font_row_rects.push(Rect::rounded(row_x, row_y, row_w, ROW_H, row_sel, 5.0));
    }

    // ── Fonts tab: UI (chrome) font stepper + "Aa" specimen ──────────────────
    let (ui_step_body, ui_font_minus, ui_font_plus, ui_font_reset) = stepper_at(t_uifontsize);

    // Live "Aa" specimen anchor. The app overdraws "Aa" here at the TRUE UI size
    // AFTER the capped panel-text pass. Offscreen unless the Fonts tab is active,
    // so the "Aa" is only drawn on that tab.
    let mut ui_specimen_pos = if active_tab == 1 {
        (px + PAD, t_uifontsize + 40.0)
    } else {
        (px + PAD, OFF)
    };

    // ── Fonts tab: UI font-family list (header row + inset card) ─────────────
    let ui_font_scroll_up =
        Rect::rounded(arrow_up_x, t_uifontlist - 1.0, 22.0, 22.0, ctl_fill, R_CTL);
    let ui_font_scroll_down =
        Rect::rounded(arrow_dn_x, t_uifontlist - 1.0, 22.0, 22.0, ctl_fill, R_CTL);
    let ui_card = Rect::rounded(
        list_x, t_uifontlist + 24.0, CW, card_h(MAX_UI_FONT_ROWS), card_fill, 8.0,
    );
    let ui_offset = ui_font_scroll_offset.min(ui_families.len().saturating_sub(1));
    let ui_visible_count = (ui_families.len().saturating_sub(ui_offset)).min(MAX_UI_FONT_ROWS);
    let mut ui_font_row_rects: Vec<Rect> = Vec::with_capacity(ui_visible_count);
    for i in 0..ui_visible_count {
        let row_y = ui_card.y + LIST_PAD + i as f32 * (ROW_H + ROW_GAP);
        ui_font_row_rects.push(Rect::rounded(row_x, row_y, row_w, ROW_H, row_sel, 5.0));
    }

    // --- Theme picker — collapsible dropdown combo (Look) ---
    // "THEME" header at t_theme. Below it a full-width "combo" control shows the
    // ACTIVE theme's display name + an 8-swatch ANSI strip + a caret. Clicking it
    // toggles `theme_dropdown_open`. When open, a floating menu of presets (name +
    // swatches, selected row accent-tinted with an accent edge) is laid out below,
    // with header-row ▲/▼ arrows when the list overflows. The menu quads/labels
    // are emitted LAST so they overlay anything sitting below the theme band.
    let presets = jetty_core::theme::PRESETS;
    let num_presets = presets.len();

    let combo_x = px + PAD;
    let combo_w = CW;
    let combo_h = 30.0;
    let combo_y = t_theme + 24.0;
    let theme_combo = Rect::rounded(combo_x, combo_y, combo_w, combo_h, ctl_fill, R_CTL);

    // Visible-row + scroll math (only meaningful when the list is open).
    let theme_offset = theme_scroll_offset.min(num_presets.saturating_sub(1));
    let theme_visible = if theme_dropdown_open {
        (num_presets - theme_offset).min(MAX_THEME_ROWS)
    } else {
        0
    };
    let menu_top = combo_y + combo_h + 6.0;
    let menu_h = 2.0 * LIST_PAD
        + theme_visible as f32 * THEME_ROW_H
        + theme_visible.saturating_sub(1) as f32 * THEME_ROW_GAP;
    let mut theme_row_rects: Vec<Rect> = Vec::with_capacity(theme_visible);
    for i in 0..theme_visible {
        let row_y = menu_top + LIST_PAD + i as f32 * (THEME_ROW_H + THEME_ROW_GAP);
        theme_row_rects.push(Rect::rounded(row_x, row_y, row_w, THEME_ROW_H, row_sel, 5.0));
    }
    // ▲/▼ scroll arrows sit on the "THEME" header row, right-aligned — same
    // pattern as the font lists — but only when the list is open AND overflows.
    // Otherwise they go offscreen (never hit-testable).
    let theme_has_scroll = theme_dropdown_open && num_presets > MAX_THEME_ROWS;
    let (theme_scroll_up, theme_scroll_down) = if theme_has_scroll {
        (
            Rect::rounded(arrow_up_x, t_theme - 1.0, 22.0, 22.0, ctl_fill, R_CTL),
            Rect::rounded(arrow_dn_x, t_theme - 1.0, 22.0, 22.0, ctl_fill, R_CTL),
        )
    } else {
        (
            Rect::rounded(OFF, OFF, 22.0, 22.0, ctl_fill, R_CTL),
            Rect::rounded(OFF, OFF, 22.0, 22.0, ctl_fill, R_CTL),
        )
    };

    // --- Shell tab: shell cycler + launch-at-login switch ---
    let (shell_body, shell_prev, shell_next) = cycler_at(t_shell);
    let (launch_login_toggle, launch_knob) = switch_at(t_launch, launch_at_login);

    // ── Effects tab (tab 4) widget geometry ───────────────────────────────────
    // Same control language as everywhere else: full-width sliders, knob
    // switches, and 56×24 text chips for the three animation toggles.
    // When active_tab ≠ 4, all band tops are OFF so every derived rect lands
    // far offscreen and can never be hit-tested or seen.

    // RGB mini-slider columns: three 90-px tracks at +80/+185/+290 from the left
    // content edge (right edge of the B column == content right edge).
    let rgb_r_x = track_x + 80.0;
    let rgb_g_x = track_x + 185.0;
    let rgb_b_x = track_x + 290.0;

    // ── CRT ENABLED switch (band 1) ──
    let (crt_enabled_toggle, crt_en_knob) = switch_at(t_fx_crt_en, effects.crt_enabled);

    // CRT sliders (bands 2–7): curvature, scanline, mask, bloom, chromatic, vignette.
    let (crt_curvature_track, crt_curv_q, crt_curvature_fill, crt_curvature_handle) =
        slider_at(t_fx_curv, effects.crt_curvature);
    let (crt_scanline_track, crt_scan_q, crt_scanline_fill, crt_scanline_handle) =
        slider_at(t_fx_scan, effects.crt_scanline);
    let (crt_mask_track, crt_mask_q, crt_mask_fill, crt_mask_handle) =
        slider_at(t_fx_mask, effects.crt_mask);
    let (crt_bloom_track, crt_bloom_q, crt_bloom_fill, crt_bloom_handle) =
        slider_at(t_fx_bloom, effects.crt_bloom);
    let (crt_chromatic_track, crt_chroma_q, crt_chromatic_fill, crt_chromatic_handle) =
        slider_at(t_fx_chroma, effects.crt_chromatic);
    let (crt_vignette_track, crt_vig_q, crt_vignette_fill, crt_vignette_handle) =
        slider_at(t_fx_vignette, effects.crt_vignette);

    // CRT scanline tint RGB mini-sliders (band 8).
    let (crt_tint_r_track, crt_tint_r_q, crt_tint_r_fill, crt_tint_r_handle) =
        mini_slider_at(rgb_r_x, t_fx_tint, effects.crt_scanline_tint[0]);
    let (crt_tint_g_track, crt_tint_g_q, crt_tint_g_fill, crt_tint_g_handle) =
        mini_slider_at(rgb_g_x, t_fx_tint, effects.crt_scanline_tint[1]);
    let (crt_tint_b_track, crt_tint_b_q, crt_tint_b_fill, crt_tint_b_handle) =
        mini_slider_at(rgb_b_x, t_fx_tint, effects.crt_scanline_tint[2]);

    // CRT animation toggle chips (band 9): ROLL / FLKR / JITR, right-aligned.
    const CHIP_W: f32 = 56.0;
    const CHIP_H: f32 = 24.0;
    let chip_x2 = px + PANEL_W - PAD - CHIP_W;
    let chip_x1 = chip_x2 - CHIP_W - 8.0;
    let chip_x0 = chip_x1 - CHIP_W - 8.0;
    let roll_col    = if effects.crt_animate_roll { accent_col } else { ctl_fill };
    let flicker_col = if effects.crt_flicker      { accent_col } else { ctl_fill };
    let jitter_col  = if effects.crt_jitter       { accent_col } else { ctl_fill };
    let crt_roll_toggle    = Rect::rounded(chip_x0, t_fx_anim + 2.0, CHIP_W, CHIP_H, roll_col,    CHIP_H / 2.0);
    let crt_flicker_toggle = Rect::rounded(chip_x1, t_fx_anim + 2.0, CHIP_W, CHIP_H, flicker_col, CHIP_H / 2.0);
    let crt_jitter_toggle  = Rect::rounded(chip_x2, t_fx_anim + 2.0, CHIP_W, CHIP_H, jitter_col,  CHIP_H / 2.0);

    // Caret switches (bands 11, 12): caret_flash_enabled, caret_glow_enabled.
    let (caret_flash_toggle, caret_flash_knob) = switch_at(t_fx_flash, effects.caret_flash_enabled);
    let (caret_glow_toggle, caret_glow_knob) = switch_at(t_fx_glow, effects.caret_glow_enabled);

    // Caret flash-duration slider (band 13): maps 60..=400 ms → 0..1.
    let caret_dur_frac = ((effects.caret_flash_ms - 60.0) / (400.0 - 60.0)).clamp(0.0, 1.0);
    let (caret_dur_track, caret_dur_q, caret_dur_fill, caret_dur_handle) =
        slider_at(t_fx_dur, caret_dur_frac);

    // Caret flash-color RGB mini-sliders (band 14).
    let (caret_color_r_track, caret_col_r_q, caret_color_r_fill, caret_color_r_handle) =
        mini_slider_at(rgb_r_x, t_fx_color, effects.caret_flash_color[0]);
    let (caret_color_g_track, caret_col_g_q, caret_color_g_fill, caret_color_g_handle) =
        mini_slider_at(rgb_g_x, t_fx_color, effects.caret_flash_color[1]);
    let (caret_color_b_track, caret_col_b_q, caret_color_b_fill, caret_color_b_handle) =
        mini_slider_at(rgb_b_x, t_fx_color, effects.caret_flash_color[2]);

    // Effects content height and visible height come from the module-level
    // `pub const`s so the App's scroll clamp and this scrollbar indicator
    // are guaranteed to use identical values.
    let effects_content_h = EFFECTS_CONTENT_H; // 716.0
    let visible_h = EFFECTS_VISIBLE_H;          // 460.0

    // --- Build quads in draw order ---
    // `quads` = chrome: always visible, rendered WITHOUT scissor.
    // `effects_quads` = Effects-tab content: rendered WITH scissor when tab 4.
    let mut quads: Vec<Rect> = Vec::new();
    let mut effects_quads: Vec<Rect> = Vec::new();
    quads.push(dim_rect);
    quads.push(border_rect);
    quads.push(bg_rect);

    // Tab-strip baseline hairline (full panel width) + active-tab underline.
    quads.push(Rect {
        x: px,
        y: py + 75.0,
        w: PANEL_W,
        h: 1.0,
        color: hairline,
        ..Default::default()
    });
    {
        let cell = &tab_rects[active_tab];
        let text_w = tab_names_chars[active_tab] * char_w;
        let bar_w = text_w + 8.0;
        quads.push(Rect::rounded(
            cell.x + (cell.w - bar_w) * 0.5,
            py + 74.0,
            bar_w,
            2.0,
            accent_col,
            1.0,
        ));
    }

    // Footer band: hairline above the hint row (fixed to the panel bottom).
    quads.push(Rect {
        x: px,
        y: py + PANEL_H - FOOTER_H,
        w: PANEL_W,
        h: 1.0,
        color: hairline,
        ..Default::default()
    });

    // Segmented-control separators, pushed after each body fill.
    let seg_line = |x: f32, t: f32| -> Rect {
        Rect {
            x,
            y: t + 7.0,
            w: 1.0,
            h: CTL_H - 14.0,
            color: seg_sep,
            ..Default::default()
        }
    };

    // Cyclers (Window + Shell tabs): body + the two inner separators.
    for body in [&summon_body, &winmode_body, &tabbar_body, &shell_body] {
        quads.push(*body);
        quads.push(seg_line(body.x + CYC_SEG, body.y));
        quads.push(seg_line(body.x + CYC_W - CYC_SEG, body.y));
    }

    // Dropdown-height / width sliders. Grayed to ~0.4 alpha when the window mode
    // is Center (the two controls are no-ops there).
    let dim_alpha = |mut r: Rect| -> Rect {
        if !is_dropdown {
            r.color[3] = (r.color[3] as f32 * 0.4).round() as u8;
        }
        r
    };
    quads.push(dim_alpha(dh_track_q));
    quads.push(dim_alpha(dh_fill));
    quads.push(dim_alpha(dropdown_handle));
    quads.push(dim_alpha(dw_track_q));
    quads.push(dim_alpha(dw_fill));
    quads.push(dim_alpha(dropdown_width_handle));

    // Switches (Window + Shell tabs).
    quads.push(autohide_toggle);
    quads.push(autohide_knob);
    quads.push(launch_login_toggle);
    quads.push(launch_knob);

    // ── Effects-tab quads (populate effects_quads; empty when tab ≠ 4) ────────
    // When active_tab == 4 the band tops carry the scroll offset, so these rects
    // are at their scrolled positions. The caller renders them with a hardware
    // scissor rect (`effects_viewport`) so overflow is GPU-clipped.
    if active_tab == 4 {
        // Section hairlines beside the "CRT" / "CARET" headers.
        let section_rule = |t: f32, chars: f32| -> Rect {
            Rect {
                x: track_x + chars * char_w + 12.0,
                y: t + 8.0,
                w: (px + PANEL_W - PAD) - (track_x + chars * char_w + 12.0),
                h: 1.0,
                color: hairline,
                ..Default::default()
            }
        };
        effects_quads.push(section_rule(t_fx_crt_hdr, 3.0));
        effects_quads.push(section_rule(t_fx_caret_hdr, 5.0));

        effects_quads.push(crt_enabled_toggle);
        effects_quads.push(crt_en_knob);
        // Draw order per slider band: track → fill → handle.
        effects_quads.push(crt_curv_q);   effects_quads.push(crt_curvature_fill); effects_quads.push(crt_curvature_handle);
        effects_quads.push(crt_scan_q);   effects_quads.push(crt_scanline_fill);  effects_quads.push(crt_scanline_handle);
        effects_quads.push(crt_mask_q);   effects_quads.push(crt_mask_fill);      effects_quads.push(crt_mask_handle);
        effects_quads.push(crt_bloom_q);  effects_quads.push(crt_bloom_fill);     effects_quads.push(crt_bloom_handle);
        effects_quads.push(crt_chroma_q); effects_quads.push(crt_chromatic_fill); effects_quads.push(crt_chromatic_handle);
        effects_quads.push(crt_vig_q);    effects_quads.push(crt_vignette_fill);  effects_quads.push(crt_vignette_handle);
        effects_quads.push(crt_tint_r_q); effects_quads.push(crt_tint_r_fill);    effects_quads.push(crt_tint_r_handle);
        effects_quads.push(crt_tint_g_q); effects_quads.push(crt_tint_g_fill);    effects_quads.push(crt_tint_g_handle);
        effects_quads.push(crt_tint_b_q); effects_quads.push(crt_tint_b_fill);    effects_quads.push(crt_tint_b_handle);
        effects_quads.push(crt_roll_toggle);
        effects_quads.push(crt_flicker_toggle);
        effects_quads.push(crt_jitter_toggle);
        effects_quads.push(caret_flash_toggle);
        effects_quads.push(caret_flash_knob);
        effects_quads.push(caret_glow_toggle);
        effects_quads.push(caret_glow_knob);
        effects_quads.push(caret_dur_q);    effects_quads.push(caret_dur_fill);     effects_quads.push(caret_dur_handle);
        effects_quads.push(caret_col_r_q);  effects_quads.push(caret_color_r_fill); effects_quads.push(caret_color_r_handle);
        effects_quads.push(caret_col_g_q);  effects_quads.push(caret_color_g_fill); effects_quads.push(caret_color_g_handle);
        effects_quads.push(caret_col_b_q);  effects_quads.push(caret_color_b_fill); effects_quads.push(caret_color_b_handle);

        // ── Scrollbar indicator ──────────────────────────────────────────────
        // Thin accent rect on the right edge of the content viewport, sized and
        // positioned to show the current scroll position. Only emitted when
        // content is taller than the viewport AND when actually scrollable.
        let max_scroll = (effects_content_h - visible_h).max(0.0);
        if max_scroll > 0.0 {
            let indicator_h = (visible_h * visible_h / effects_content_h).max(10.0);
            let t = effects_scroll / max_scroll;
            let indicator_y = content_top + t * (visible_h - indicator_h);
            let ind_col: [u8; 4] = [accent[0], accent[1], accent[2], 150];
            effects_quads.push(Rect::rounded(
                px + PANEL_W - 8.0, indicator_y, 4.0, indicator_h, ind_col, 2.0,
            ));
        }
    }

    // Font-size steppers (terminal + UI): body, separators, reset button.
    for (body, reset) in [(&font_step_body, &font_reset), (&ui_step_body, &ui_font_reset)] {
        quads.push(*reset);
        quads.push(*body);
        quads.push(seg_line(body.x + STEP_SEG, body.y));
        quads.push(seg_line(body.x + STEP_W - STEP_SEG, body.y));
    }

    // Font-list scroll buttons + inset cards + selected rows.
    quads.push(font_scroll_up);
    quads.push(font_scroll_down);
    quads.push(font_card);
    for (i, row) in font_row_rects.iter().enumerate() {
        let family_idx = offset + i;
        let is_selected =
            families.get(family_idx).map(|n| n.as_str()) == Some(selected_family);
        if is_selected {
            quads.push(*row);
            quads.push(Rect::rounded(row.x, row.y + 3.0, 3.0, row.h - 6.0, accent_col, 1.5));
        }
    }
    quads.push(ui_font_scroll_up);
    quads.push(ui_font_scroll_down);
    quads.push(ui_card);
    for (i, row) in ui_font_row_rects.iter().enumerate() {
        let family_idx = ui_offset + i;
        let row_value: &str = if family_idx == 0 {
            ""
        } else {
            ui_families.get(family_idx).map(|n| n.as_str()).unwrap_or("")
        };
        if row_value == selected_ui_family {
            quads.push(*row);
            quads.push(Rect::rounded(row.x, row.y + 3.0, 3.0, row.h - 6.0, accent_col, 1.5));
        }
    }

    // --- Theme combo header (collapsed, always shown on the Look tab) ---
    // Control fill, then the ACTIVE theme's swatch strip; name + caret are
    // emitted in the label pass.
    let active_theme_idx = theme_idx.min(num_presets - 1);
    quads.push(theme_combo);
    {
        let active = jetty_core::Theme::by_name(presets[active_theme_idx]);
        let caret_x = theme_combo.x + theme_combo.w - 20.0;
        let strip_left = caret_x - 10.0 - THEME_STRIP_W;
        push_theme_swatches(&mut quads, strip_left, theme_combo.y + theme_combo.h / 2.0, &active);
    }

    // --- Theme dropdown list (open) — a floating menu over the space below the
    // combo: border ring → menu surface → selected-row tint/edge → swatches →
    // scroll-arrow button fills.
    if theme_dropdown_open {
        quads.push(Rect::rounded(
            combo_x - 1.0, menu_top - 1.0, combo_w + 2.0, menu_h + 2.0, menu_border, 9.0,
        ));
        quads.push(Rect::rounded(combo_x, menu_top, combo_w, menu_h, menu_fill, 8.0));
        for (i, row) in theme_row_rects.iter().enumerate() {
            let preset_idx = theme_offset + i;
            if preset_idx == theme_idx {
                quads.push(*row);
                quads.push(Rect::rounded(row.x, row.y + 4.0, 3.0, row.h - 8.0, accent_col, 1.5));
            }
            let t = jetty_core::Theme::by_name(presets[preset_idx]);
            let strip_left = row.x + row.w - 10.0 - THEME_STRIP_W;
            push_theme_swatches(&mut quads, strip_left, row.y + row.h / 2.0, &t);
        }
        if theme_has_scroll {
            quads.push(theme_scroll_up);
            quads.push(theme_scroll_down);
        }
    }

    // Opacity + corner-radius sliders: dim track, accent fill, knob.
    quads.push(opacity_track_q);
    quads.push(opacity_fill);
    quads.push(slider_handle);
    quads.push(radius_track_q);
    quads.push(radius_fill);
    quads.push(radius_handle);

    // --- Labels ---
    // `labels` = chrome: title, tab strip, and non-Effects tab widget labels.
    // `effects_labels` = Effects-tab widget labels; clipped to content viewport.
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut effects_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push(("Settings".to_string(), px + PAD, py + 14.0, text_main));

    // Tab strip labels — active tab in main text (the accent underline carries
    // the "active" signal), inactive tabs muted.
    for (i, name) in TAB_NAMES.iter().enumerate() {
        let cell = &tab_rects[i];
        let text_w = tab_names_chars[i] * char_w;
        let label_x = cell.x + (cell.w - text_w) * 0.5;
        let col = if i == active_tab { text_main } else { text_header };
        labels.push((name.to_string(), label_x, tab_strip_y, col));
    }

    // Footer hint.
    labels.push((
        "Esc to close · drag title to move".to_string(),
        px + PAD,
        py + PANEL_H - 26.0,
        text_hint,
    ));

    // Segment-glyph helpers (chevrons/steppers centered in their segments).
    let seg_glyph_x = |seg_x: f32, seg_w: f32| seg_x + (seg_w - char_w) * 0.5;

    // OPACITY header — CAPS with right-aligned "97%" value.
    let pct = (opacity * 100.0).round() as i32;
    let pct_str = format!("{}%", pct);
    labels.push(("OPACITY".to_string(), px + PAD, t_opacity, text_header));
    labels.push((pct_str.clone(), right_x(&pct_str), t_opacity, text_main));

    // CORNER RADIUS header — CAPS with right-aligned "Npx" value.
    let radius_px = corner_radius.round() as i32;
    let radius_str = format!("{}px", radius_px);
    labels.push(("CORNER RADIUS".to_string(), px + PAD, t_radius, text_header));
    labels.push((radius_str.clone(), right_x(&radius_str), t_radius, text_main));

    // Helper: center a (possibly truncated) cycler value between its chevrons.
    let cycle_gap_left = cyc_x + CYC_SEG;
    let cycle_gap_w = CYC_W - 2.0 * CYC_SEG;
    let cycle_max_chars = if char_w > 0.0 {
        ((cycle_gap_w / char_w).floor() as usize).max(3)
    } else {
        11
    };
    let center_cycle = |name: &str| -> (String, f32) {
        let shown: String = if name.chars().count() > cycle_max_chars {
            let mut s: String = name.chars().take(cycle_max_chars - 1).collect();
            s.push('…');
            s
        } else {
            name.to_string()
        };
        let x = center_x(&shown, cycle_gap_left, cycle_gap_w)
            .clamp(cycle_gap_left, (cycle_gap_left + cycle_gap_w).max(cycle_gap_left));
        (shown, x)
    };
    // Emit one cycler's labels: chevrons + centered value on the control row.
    let push_cycler_labels = |labels: &mut Vec<(String, f32, f32, [u8; 3])>,
                                  t: f32,
                                  value: &str| {
        let (shown, name_x) = center_cycle(value);
        labels.push((shown, name_x, t + 6.0, text_main));
        labels.push(("<".to_string(), seg_glyph_x(cycle_prev_x, CYC_SEG), t + 6.0, text_btn));
        labels.push((">".to_string(), seg_glyph_x(cycle_next_x, CYC_SEG), t + 6.0, text_btn));
    };

    // SUMMON EFFECT / WINDOW MODE / TAB BAR bands (Window) — CAPS headers with
    // the segmented cycler on the same row.
    labels.push(("SUMMON EFFECT".to_string(), px + PAD, t_summon + 6.0, text_header));
    push_cycler_labels(&mut labels, t_summon, summon_effect_name);
    labels.push(("WINDOW MODE".to_string(), px + PAD, t_winmode + 6.0, text_header));
    push_cycler_labels(&mut labels, t_winmode, window_mode_name);
    labels.push(("TAB BAR".to_string(), px + PAD, t_tabbar + 6.0, text_header));
    push_cycler_labels(&mut labels, t_tabbar, tab_bar_name);

    // DROPDOWN HEIGHT band (Window) — CAPS header + right-aligned value.
    let dh_text = if is_dropdown { text_header } else { text_hint };
    let dh_val_text = if is_dropdown { text_main } else { text_hint };
    let dh_pct = (dropdown_height_pct * 100.0).round() as i32;
    let dh_str = format!("{}%", dh_pct);
    labels.push(("DROPDOWN HEIGHT".to_string(), px + PAD, t_droph, dh_text));
    labels.push((dh_str.clone(), right_x(&dh_str), t_droph, dh_val_text));

    // DROPDOWN WIDTH band (Window) — CAPS header + right-aligned value.
    let dw_pct = (dropdown_width_pct * 100.0).round() as i32;
    let dw_str = format!("{}%", dw_pct);
    labels.push(("DROPDOWN WIDTH".to_string(), px + PAD, t_dropw, dh_text));
    labels.push((dw_str.clone(), right_x(&dw_str), t_dropw, dh_val_text));

    // AUTO-HIDE band (Window) — CAPS header; the switch itself is stateful.
    labels.push(("AUTO-HIDE ON FOCUS LOSS".to_string(), px + PAD, t_autohide + 6.0, text_header));

    // Emit one stepper's labels: − / value / + / Reset.
    let push_stepper_labels = |labels: &mut Vec<(String, f32, f32, [u8; 3])>,
                                   t: f32,
                                   value: &str| {
        labels.push((
            value.to_string(),
            center_x(value, step_x + STEP_SEG, STEP_W - 2.0 * STEP_SEG),
            t + 6.0,
            text_main,
        ));
        labels.push(("-".to_string(), seg_glyph_x(step_x, STEP_SEG), t + 6.0, text_btn));
        labels.push((
            "+".to_string(),
            seg_glyph_x(step_x + STEP_W - STEP_SEG, STEP_SEG),
            t + 6.0,
            text_btn,
        ));
        labels.push((
            "Reset".to_string(),
            center_x("Reset", reset_x, RESET_W),
            t + 6.0,
            text_btn,
        ));
    };

    // FONT SIZE band (Fonts) — CAPS header + stepper with the "Npt" value inside.
    let fs_str = format!("{}pt", font_size.round() as i32);
    labels.push(("FONT SIZE".to_string(), px + PAD, t_fontsize + 6.0, text_header));
    push_stepper_labels(&mut labels, t_fontsize, &fs_str);

    // List-section header helper: CAPS header + n/total counter + ▲▼ arrows.
    let push_list_header = |labels: &mut Vec<(String, f32, f32, [u8; 3])>,
                                t: f32,
                                title: &str,
                                shown_to: usize,
                                total: usize,
                                overflows: bool| {
        labels.push((title.to_string(), px + PAD, t, text_header));
        labels.push(("^".to_string(), arrow_up_x + 6.0, t + 2.0, text_btn));
        labels.push(("v".to_string(), arrow_dn_x + 6.0, t + 2.0, text_btn));
        if overflows {
            let hint = format!("{}/{}", shown_to, total);
            let hint_w = hint.chars().count() as f32 * char_w;
            labels.push((hint, arrow_up_x - 10.0 - hint_w, t, text_hint));
        }
    };

    // FONT list (Fonts).
    push_list_header(
        &mut labels,
        t_fontlist,
        "FONT",
        offset + visible_count,
        families.len(),
        families.len() > MAX_FONT_ROWS,
    );
    for (i, row) in font_row_rects.iter().enumerate() {
        let family_idx = offset + i;
        if let Some(name) = families.get(family_idx) {
            let is_selected = name.as_str() == selected_family;
            let text_color: [u8; 3] = if is_selected { text_main } else { text_dim };
            // Char-boundary-safe truncation (multibyte-safe).
            let display = if name.chars().count() > 34 {
                let truncated: String = name.chars().take(32).collect();
                format!("{}…", truncated)
            } else {
                name.clone()
            };
            labels.push((display, row.x + 12.0, row.y + 4.0, text_color));
        }
    }

    // ── UI FONT section labels (Fonts) ──
    let ui_fs_str = format!("{}pt", ui_font_size.round() as i32);
    labels.push(("UI FONT SIZE".to_string(), px + PAD, t_uifontsize + 6.0, text_header));
    push_stepper_labels(&mut labels, t_uifontsize, &ui_fs_str);

    push_list_header(
        &mut labels,
        t_uifontlist,
        "UI FONT",
        ui_offset + ui_visible_count,
        ui_families.len(),
        ui_families.len() > MAX_UI_FONT_ROWS,
    );
    for (i, row) in ui_font_row_rects.iter().enumerate() {
        let family_idx = ui_offset + i;
        let (name, row_value): (&str, &str) = if family_idx == 0 {
            ("System Sans (default)", "")
        } else {
            let n = ui_families.get(family_idx).map(|s| s.as_str()).unwrap_or("");
            (n, n)
        };
        let is_selected = row_value == selected_ui_family;
        let text_color: [u8; 3] = if is_selected { text_main } else { text_dim };
        let display = if name.chars().count() > 34 {
            let truncated: String = name.chars().take(32).collect();
            format!("{}…", truncated)
        } else {
            name.to_string()
        };
        labels.push((display, row.x + 12.0, row.y + 4.0, text_color));
    }

    // THEME band (Look) — CAPS header (+ scroll arrows/counter when open+overflow).
    labels.push(("THEME".to_string(), px + PAD, t_theme, text_header));

    // Truncate a display name to fit the combo/row width (leaving room for swatches).
    let fit_theme_name = |name: &str| -> String {
        const MAX_CHARS: usize = 22;
        if name.chars().count() > MAX_CHARS {
            let s: String = name.chars().take(MAX_CHARS - 1).collect();
            format!("{}…", s)
        } else {
            name.to_string()
        }
    };

    // Combo header label: active theme name (left) + ▼ / ▲ caret (right).
    {
        let active = jetty_core::Theme::by_name(presets[active_theme_idx]);
        labels.push((
            fit_theme_name(active.display_name),
            theme_combo.x + 12.0,
            theme_combo.y + 7.0,
            text_main,
        ));
        let caret = if theme_dropdown_open { "^" } else { "v" };
        labels.push((
            caret.to_string(),
            theme_combo.x + theme_combo.w - 20.0,
            theme_combo.y + 7.0,
            text_btn,
        ));
    }

    // Open menu: per-row theme name labels + scroll arrows + "shown/total" hint.
    if theme_dropdown_open {
        for (i, row) in theme_row_rects.iter().enumerate() {
            let preset_idx = theme_offset + i;
            let name = jetty_core::Theme::by_name(presets[preset_idx]).display_name;
            let col = if preset_idx == theme_idx { text_main } else { text_dim };
            labels.push((fit_theme_name(name), row.x + 12.0, row.y + 6.0, col));
        }
        if theme_has_scroll {
            labels.push(("^".to_string(), theme_scroll_up.x + 6.0, theme_scroll_up.y + 3.0, text_btn));
            labels.push(("v".to_string(), theme_scroll_down.x + 6.0, theme_scroll_down.y + 3.0, text_btn));
            let hint = format!("{}/{}", theme_offset + theme_visible, num_presets);
            let hint_w = hint.chars().count() as f32 * char_w;
            labels.push((hint, arrow_up_x - 10.0 - hint_w, t_theme, text_hint));
        }
    }

    // SHELL band (Shell) — CAPS header + segmented cycler + helper line.
    labels.push(("SHELL".to_string(), px + PAD, t_shell + 6.0, text_header));
    push_cycler_labels(&mut labels, t_shell, shell_display);
    labels.push(("Applies to new tabs".to_string(), px + PAD, t_shell + 34.0, text_hint));

    // LAUNCH AT LOGIN band (Shell) — CAPS header + switch + helper line.
    labels.push(("LAUNCH AT LOGIN".to_string(), px + PAD, t_launch + 6.0, text_header));
    labels.push((
        "Adds a desktop autostart entry".to_string(),
        px + PAD,
        t_launch + 34.0,
        text_hint,
    ));

    // ── Effects-tab labels (into effects_labels; empty when tab ≠ 4) ────────
    // When active_tab == 4, band tops carry the scroll offset so label Y
    // positions already reflect scrolling. The caller renders effects_labels
    // with TextArea.bounds = content viewport so labels outside the viewport
    // are suppressed by glyphon.
    if active_tab == 4 {
        // Section headers "CRT" / "CARET" (bands 0, 10) — main-text CAPS with a
        // hairline rule to the right (pushed in the quad pass above).
        effects_labels.push(("CRT".to_string(), px + PAD, t_fx_crt_hdr, text_main));
        effects_labels.push(("CARET".to_string(), px + PAD, t_fx_caret_hdr, text_main));

        // CRT ENABLED band (band 1) — header + switch (stateful, no text).
        effects_labels.push(("CRT ENABLED".to_string(), px + PAD, t_fx_crt_en + 6.0, text_header));

        // CRT slider bands (2–7): CAPS header + right-aligned "N%" value.
        macro_rules! fx_slider_label {
            ($label:expr, $band_y:expr, $val:expr) => {
                effects_labels.push(($label.to_string(), px + PAD, $band_y, text_header));
                let pct = ($val * 100.0).round() as i32;
                let pct_str = format!("{}%", pct);
                effects_labels.push((pct_str.clone(), right_x(&pct_str), $band_y, text_main));
            };
        }
        fx_slider_label!("CURVATURE", t_fx_curv,     effects.crt_curvature);
        fx_slider_label!("SCANLINE",  t_fx_scan,     effects.crt_scanline);
        fx_slider_label!("MASK",      t_fx_mask,     effects.crt_mask);
        fx_slider_label!("BLOOM",     t_fx_bloom,    effects.crt_bloom);
        fx_slider_label!("CHROMATIC", t_fx_chroma,   effects.crt_chromatic);
        fx_slider_label!("VIGNETTE",  t_fx_vignette, effects.crt_vignette);

        // CRT scanline-tint RGB triple (band 8): section header + R/G/B sub-labels.
        effects_labels.push(("TINT".to_string(), px + PAD, t_fx_tint, text_header));
        effects_labels.push(("R".to_string(), rgb_r_x, t_fx_tint, text_dim));
        effects_labels.push(("G".to_string(), rgb_g_x, t_fx_tint, text_dim));
        effects_labels.push(("B".to_string(), rgb_b_x, t_fx_tint, text_dim));

        // CRT animation chips (band 9): header + three chip labels.
        effects_labels.push(("ANIMATE".to_string(), px + PAD, t_fx_anim + 6.0, text_header));
        for (chip, on, txt) in [
            (&crt_roll_toggle, effects.crt_animate_roll, "ROLL"),
            (&crt_flicker_toggle, effects.crt_flicker, "FLKR"),
            (&crt_jitter_toggle, effects.crt_jitter, "JITR"),
        ] {
            let col = if on { on_accent } else { text_btn };
            effects_labels.push((
                txt.to_string(),
                center_x(txt, chip.x, chip.w),
                chip.y + 4.0,
                col,
            ));
        }

        // CARET FLASH / GLOW switches (bands 11, 12).
        effects_labels.push(("FLASH".to_string(), px + PAD, t_fx_flash + 6.0, text_header));
        effects_labels.push(("GLOW".to_string(), px + PAD, t_fx_glow + 6.0, text_header));

        // FLASH MS slider (band 13): maps 60..=400 → shows raw ms value.
        effects_labels.push(("FLASH MS".to_string(), px + PAD, t_fx_dur, text_header));
        {
            let ms_str = format!("{}ms", effects.caret_flash_ms.round() as i32);
            effects_labels.push((ms_str.clone(), right_x(&ms_str), t_fx_dur, text_main));
        }

        // CARET flash-color RGB triple (band 14): section header + R/G/B sub-labels.
        effects_labels.push(("COLOR".to_string(), px + PAD, t_fx_color, text_header));
        effects_labels.push(("R".to_string(), rgb_r_x, t_fx_color, text_dim));
        effects_labels.push(("G".to_string(), rgb_g_x, t_fx_color, text_dim));
        effects_labels.push(("B".to_string(), rgb_b_x, t_fx_color, text_dim));
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
    let mut geom = PanelGeom {
        panel: panel_rect,
        tab_rects,
        slider_track,
        slider_handle,
        theme_combo,
        theme_rows: theme_row_rects,
        theme_open: theme_dropdown_open,
        theme_scroll_offset: theme_offset,
        theme_scroll_up,
        theme_scroll_down,
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
        launch_login_toggle,
        shell_prev,
        shell_next,
        ui_font_minus,
        ui_font_plus,
        ui_font_reset,
        ui_font_rows: ui_font_row_rects,
        ui_font_scroll_offset: ui_offset,
        ui_font_scroll_up,
        ui_font_scroll_down,
        // Effects-tab geometry.
        crt_enabled_toggle,
        crt_curvature_track,
        crt_curvature_handle,
        crt_scanline_track,
        crt_scanline_handle,
        crt_mask_track,
        crt_mask_handle,
        crt_bloom_track,
        crt_bloom_handle,
        crt_chromatic_track,
        crt_chromatic_handle,
        crt_vignette_track,
        crt_vignette_handle,
        crt_tint_r_track,
        crt_tint_r_handle,
        crt_tint_g_track,
        crt_tint_g_handle,
        crt_tint_b_track,
        crt_tint_b_handle,
        crt_roll_toggle,
        crt_flicker_toggle,
        crt_jitter_toggle,
        caret_flash_toggle,
        caret_glow_toggle,
        caret_dur_track,
        caret_dur_handle,
        caret_color_r_track,
        caret_color_r_handle,
        caret_color_g_track,
        caret_color_g_handle,
        caret_color_b_track,
        caret_color_b_handle,
        effects_content_h,
        content_top,
        content_bottom: content_top + visible_h,
    };

    // ── Scale the logical layout to physical px ────────────────────────────────
    // At scale 1 (`dpi == 1.0`, i.e. tests / scale-1 displays) this is skipped so
    // the output is byte-identical to the pre-HiDPI layout; otherwise every quad,
    // label and hit-rect is scaled uniformly so it lands where the DPI-scaled
    // glyphs are drawn.
    if dpi != 1.0 {
        let sr = |r: &mut Rect| {
            r.x *= dpi;
            r.y *= dpi;
            r.w *= dpi;
            r.h *= dpi;
            r.radius *= dpi;
        };
        for q in quads.iter_mut() {
            sr(q);
        }
        for q in effects_quads.iter_mut() {
            sr(q);
        }
        for l in labels.iter_mut() {
            l.1 *= dpi;
            l.2 *= dpi;
        }
        for l in effects_labels.iter_mut() {
            l.1 *= dpi;
            l.2 *= dpi;
        }
        geom.scale(dpi);
        ui_specimen_pos.0 *= dpi;
        ui_specimen_pos.1 *= dpi;
    }

    // Viewport for the Effects tab scissor / text-clip pass, in PHYSICAL px and
    // intersected with the surface so `set_scissor_rect` can never exceed the
    // render target (which is a wgpu validation error → uncaptured-error panic).
    // The settings window is non-resizable, but tiling WMs (i3/sway/dwm) ignore
    // that hint and can force a surface smaller than the panel; clamp (and skip
    // the pass entirely if the visible intersection is empty).
    // `None` on other tabs so the caller skips the scissored pass entirely.
    let effects_viewport = if active_tab == 4 {
        let vx = (px * dpi).max(0.0);
        let vy = (content_top * dpi).max(0.0);
        let vw = (PANEL_W * dpi).min(screen_w as f32 - vx);
        let vh = (visible_h * dpi).min(screen_h as f32 - vy);
        if vw >= 1.0 && vh >= 1.0 {
            Some([vx as u32, vy as u32, vw as u32, vh as u32])
        } else {
            None
        }
    } else {
        None
    };

    PanelView { quads, labels, effects_quads, effects_labels, effects_viewport, geom, ui_specimen_pos }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a panel view with representative inputs for the given active tab.
    fn panel_tab(screen_w: u32, screen_h: u32, active_tab: usize) -> PanelView {
        panel_tab_ex(screen_w, screen_h, active_tab, false, 0)
    }

    /// Like `panel_tab` but with explicit theme-dropdown state, for the theme
    /// picker's open/closed and scrolling tests.
    fn panel_tab_ex(
        screen_w: u32,
        screen_h: u32,
        active_tab: usize,
        theme_open: bool,
        theme_scroll: usize,
    ) -> PanelView {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let families: Vec<String> = vec![
            "JetBrains Mono".to_string(),
            "Fira Code".to_string(),
            "Hack".to_string(),
            "Source Code Pro".to_string(),
            "Inconsolata".to_string(),
            "Cascadia Code".to_string(),
        ];
        // UI-font candidates with the synthetic "System Sans (default)" row at 0.
        let ui_families: Vec<String> = vec![
            "System Sans (default)".to_string(),
            "Inter".to_string(),
            "Noto Sans".to_string(),
            "DejaVu Sans".to_string(),
            "Cantarell".to_string(),
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
            false,           // launch_at_login
            18.0,            // ui_font_size (true, not capped)
            &ui_families,
            "",              // selected_ui_family ("" = System Sans)
            0,               // ui_font_scroll_offset
            0.0, 0.0,        // dx, dy
            &theme,
            CHAR_W_FALLBACK, // char_w (scale-1 default for tests)
            "zsh",           // shell_display
            active_tab,
            &EffectsParams::default(), // effects (defaults: CRT off, caret flash on)
            0.0,             // effects_scroll (default: top)
            theme_open,      // theme_dropdown_open
            theme_scroll,    // theme_scroll_offset
        )
    }

    /// Tab-0 ("Look") panel, the most commonly asserted layout.
    fn default_panel(screen_w: u32, screen_h: u32) -> PanelView {
        panel_tab(screen_w, screen_h, 0)
    }

    /// Build a panel with an explicit chrome advance (drives the HiDPI layout
    /// scale) so the DPI path can be exercised. `char_w == CHAR_W_FALLBACK` is
    /// scale 1; `2×CHAR_W_FALLBACK` on a 2× surface must reproduce the scale-1
    /// layout scaled by exactly 2.
    fn panel_scaled(screen_w: u32, screen_h: u32, char_w: f32, active_tab: usize) -> PanelView {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let families: Vec<String> = vec!["JetBrains Mono".to_string(), "Fira Code".to_string()];
        let ui_families: Vec<String> =
            vec!["System Sans (default)".to_string(), "Inter".to_string()];
        build_panel(
            screen_w, screen_h, 0.97, 0, 15.0, &families, "JetBrains Mono", 0, 8.0,
            "Phosphor", "Dropdown", "Top", 0.5, 0.7, true, false, false, 18.0,
            &ui_families, "", 0, 0.0, 0.0, &theme, char_w, "zsh", active_tab,
            &EffectsParams::default(), 0.0, false, 0,
        )
    }

    #[test]
    fn hidpi_layout_is_exact_2x_of_scale1() {
        // A 2× DPI surface with a 2× chrome advance must yield geometry that is
        // exactly twice the scale-1 layout — proving every quad/label/hit-rect is
        // scaled (a missed field would break proportionality on HiDPI only).
        for tab in 0..5 {
            let one = panel_scaled(1200, 900, CHAR_W_FALLBACK, tab);
            let two = panel_scaled(2400, 1800, 2.0 * CHAR_W_FALLBACK, tab);
            assert_eq!(one.quads.len(), two.quads.len());
            for (a, b) in one.quads.iter().zip(&two.quads) {
                assert!((b.x - 2.0 * a.x).abs() < 1e-2, "quad x not 2× at tab {tab}");
                assert!((b.y - 2.0 * a.y).abs() < 1e-2, "quad y not 2× at tab {tab}");
                assert!((b.w - 2.0 * a.w).abs() < 1e-2, "quad w not 2× at tab {tab}");
                assert!((b.h - 2.0 * a.h).abs() < 1e-2, "quad h not 2× at tab {tab}");
                assert!((b.radius - 2.0 * a.radius).abs() < 1e-2, "quad r not 2× at tab {tab}");
            }
            assert_eq!(one.effects_quads.len(), two.effects_quads.len());
            for (a, b) in one.effects_quads.iter().zip(&two.effects_quads) {
                assert!((b.x - 2.0 * a.x).abs() < 1e-2 && (b.y - 2.0 * a.y).abs() < 1e-2);
                assert!((b.w - 2.0 * a.w).abs() < 1e-2 && (b.h - 2.0 * a.h).abs() < 1e-2);
            }
            for (a, b) in one.labels.iter().zip(&two.labels) {
                assert_eq!(a.0, b.0, "label text differs at tab {tab}");
                assert!((b.1 - 2.0 * a.1).abs() < 1e-2, "label x not 2× at tab {tab}");
                assert!((b.2 - 2.0 * a.2).abs() < 1e-2, "label y not 2× at tab {tab}");
            }
            // A representative sample of hit-rects across every tab's controls.
            let gr1 = &one.geom;
            let gr2 = &two.geom;
            for (a, b) in [
                (gr1.panel, gr2.panel),
                (gr1.tab_rects[0], gr2.tab_rects[0]),
                (gr1.tab_rects[4], gr2.tab_rects[4]),
                (gr1.slider_handle, gr2.slider_handle),
                (gr1.theme_combo, gr2.theme_combo),
                (gr1.font_plus, gr2.font_plus),
                (gr1.summon_next, gr2.summon_next),
                (gr1.autohide_toggle, gr2.autohide_toggle),
                (gr1.shell_prev, gr2.shell_prev),
                (gr1.crt_curvature_handle, gr2.crt_curvature_handle),
                (gr1.caret_color_b_track, gr2.caret_color_b_track),
            ] {
                assert!((b.x - 2.0 * a.x).abs() < 1e-2, "geom x not 2× at tab {tab}");
                assert!((b.y - 2.0 * a.y).abs() < 1e-2, "geom y not 2× at tab {tab}");
                assert!((b.w - 2.0 * a.w).abs() < 1e-2, "geom w not 2× at tab {tab}");
            }
        }
    }

    #[test]
    fn effects_viewport_clamped_to_surface() {
        // Effects tab on a surface far SHORTER than the panel needs (a tiling WM
        // forcing a small tile): the scissor rect must stay within the surface or
        // be skipped, never exceeding it (which would panic wgpu validation).
        for (w, h) in [(600u32, 400u32), (300, 200), (420, 120), (1000, 700)] {
            let pv = panel_scaled(w, h, CHAR_W_FALLBACK, 4);
            if let Some([vx, vy, vw, vh]) = pv.effects_viewport {
                assert!(vx + vw <= w, "scissor right {} > surface {w}", vx + vw);
                assert!(vy + vh <= h, "scissor bottom {} > surface {h}", vy + vh);
            }
        }
    }

    /// The visible (on-screen) rects for the active tab — drops any rect pushed
    /// offscreen (inactive-tab band). Used to assert per-tab layout.
    fn is_visible(r: &Rect) -> bool {
        r.y < 1.0e5
    }

    #[test]
    fn panel_fits_on_screen_at_various_sizes() {
        // Screen sizes that can fully contain the panel (PANEL_H=592, PANEL_W=420).
        for tab in 0..5 {
            for (w, h) in [(1920u32, 1200u32), (1600, 900), (2560, 1440), (1440, 700)] {
                let pv = panel_tab(w, h, tab);
                let g = &pv.geom;
                let sw = w as f32;
                let sh = h as f32;
                assert!(g.panel.x >= 0.0, "panel.x < 0 at {w}×{h} tab {tab}");
                assert!(g.panel.y >= 0.0, "panel.y < 0 at {w}×{h} tab {tab}");
                assert!(
                    g.panel.x + g.panel.w <= sw + 0.5,
                    "panel overflows right at {w}×{h} tab {tab}"
                );
                assert!(
                    g.panel.y + g.panel.h <= sh + 0.5,
                    "panel overflows bottom at {w}×{h} tab {tab}"
                );
            }
        }
        // Smaller screens clamp to y=0, x=0. Just assert non-negative.
        for (w, h) in [(1280u32, 600u32), (1024, 560), (800, 480)] {
            let pv = default_panel(w, h);
            let g = &pv.geom;
            assert!(g.panel.x >= 0.0, "panel.x < 0 at {w}×{h}");
            assert!(g.panel.y >= 0.0, "panel.y < 0 at {w}×{h}");
        }
    }

    #[test]
    fn tab_strip_present_and_active_highlighted() {
        for tab in 0..5 {
            let pv = panel_tab(1920, 1080, tab);
            let g = &pv.geom;
            // All 5 tab hit rects exist, are inside the panel horizontally, sit
            // in the strip band (below the title, above content), and run
            // strictly left-to-right without overlapping.
            let mut prev_right = g.panel.x;
            for (i, r) in g.tab_rects.iter().enumerate() {
                assert!(
                    r.x >= g.panel.x && r.x + r.w <= g.panel.x + g.panel.w + 0.5,
                    "tab_rect[{i}] x out of panel"
                );
                assert!(r.y > g.panel.y, "tab_rect[{i}] not below panel top");
                assert!(r.x + 0.5 >= prev_right, "tab_rect[{i}] overlaps its neighbour");
                prev_right = r.x + r.w;
            }
            // The 5 tab labels are present.
            let all_text: Vec<String> = pv.labels.iter().map(|l| l.0.clone()).collect();
            for name in &TAB_NAMES {
                assert!(all_text.iter().any(|s| s == name), "missing tab label {name}");
            }
        }
    }

    #[test]
    fn only_active_tab_widgets_are_on_screen() {
        // The opacity slider belongs to tab 0 only; it must be offscreen on others.
        for tab in 0..5 {
            let pv = panel_tab(1920, 1080, tab);
            let g = &pv.geom;
            assert_eq!(
                is_visible(&g.slider_track),
                tab == 0,
                "opacity track visibility wrong on tab {tab}"
            );
            assert_eq!(
                is_visible(&g.font_minus),
                tab == 1,
                "font_minus visibility wrong on tab {tab}"
            );
            assert_eq!(
                is_visible(&g.summon_prev),
                tab == 2,
                "summon_prev visibility wrong on tab {tab}"
            );
            assert_eq!(
                is_visible(&g.shell_prev),
                tab == 3,
                "shell_prev visibility wrong on tab {tab}"
            );
            // crt_enabled_toggle lives on tab 4 only.
            assert_eq!(
                is_visible(&g.crt_enabled_toggle),
                tab == 4,
                "crt_enabled_toggle visibility wrong on tab {tab}"
            );
        }
    }

    #[test]
    fn theme_combo_closed_has_no_rows() {
        // Closed (default): the combo header is present and within the panel, and
        // no dropdown rows are laid out.
        let pv = panel_tab(1920, 1080, 0);
        let g = &pv.geom;
        assert!(!g.theme_open, "dropdown should be closed by default");
        assert!(g.theme_rows.is_empty(), "closed dropdown must have no rows");
        let panel = &g.panel;
        let c = &g.theme_combo;
        assert!(
            c.x >= panel.x && c.x + c.w <= panel.x + panel.w + 0.5,
            "combo x out of panel"
        );
        assert!(
            c.y >= panel.y && c.y + c.h <= panel.y + panel.h + 0.5,
            "combo y out of panel"
        );
    }

    #[test]
    fn theme_dropdown_open_lists_rows_in_presets_order() {
        // Open at scroll 0: rows == min(MAX_THEME_ROWS, presets), each within the
        // panel and stacked strictly top-down below the combo.
        let pv = panel_tab_ex(1920, 1080, 0, true, 0);
        let g = &pv.geom;
        assert!(g.theme_open);
        let expected = MAX_THEME_ROWS.min(jetty_core::theme::PRESETS.len());
        assert_eq!(g.theme_rows.len(), expected, "wrong visible row count");
        let panel = &g.panel;
        let mut prev_bottom = g.theme_combo.y + g.theme_combo.h;
        for (i, row) in g.theme_rows.iter().enumerate() {
            assert!(row.y + 0.5 >= prev_bottom, "row {i} overlaps the one above");
            assert!(
                row.y + row.h <= panel.y + panel.h + 0.5,
                "row {i} spills past the panel bottom"
            );
            // Rows are inset inside the floating menu but never poke outside the
            // combo's horizontal span.
            assert!(row.x + 0.5 >= g.theme_combo.x, "row {i} left of the menu");
            assert!(
                row.x + row.w <= g.theme_combo.x + g.theme_combo.w + 0.5,
                "row {i} overflows the menu"
            );
            prev_bottom = row.y + row.h;
        }
        // With more presets than MAX_THEME_ROWS the scroll arrows are live.
        if jetty_core::theme::PRESETS.len() > MAX_THEME_ROWS {
            assert!(is_visible(&g.theme_scroll_up), "scroll-up should be visible");
            assert!(is_visible(&g.theme_scroll_down), "scroll-down should be visible");
        }
    }

    #[test]
    fn theme_dropdown_scroll_offset_shifts_window() {
        // Scrolling reports the offset back through the geom so app-side row clicks
        // map to `theme_scroll_offset + i`.
        let pv = panel_tab_ex(1920, 1080, 0, true, 3);
        let g = &pv.geom;
        assert_eq!(g.theme_scroll_offset, 3);
        let remaining = jetty_core::theme::PRESETS.len() - 3;
        assert_eq!(g.theme_rows.len(), MAX_THEME_ROWS.min(remaining));
    }

    #[test]
    fn slider_handles_within_tracks() {
        // Opacity + corner-radius are on tab 0.
        let pv = panel_tab(1920, 1080, 0);
        let g = &pv.geom;
        let track = &g.slider_track;
        let handle = &g.slider_handle;
        assert!(handle.x >= track.x - 0.5, "opacity handle left of track");
        assert!(handle.x + handle.w <= track.x + track.w + 0.5, "opacity handle right of track");

        let rtrack = &g.radius_track;
        let rhandle = &g.radius_handle;
        assert!(rhandle.x >= rtrack.x - 0.5);
        assert!(rhandle.x + rhandle.w <= rtrack.x + rtrack.w + 0.5);

        // Dropdown sliders are on tab 2.
        let pv = panel_tab(1920, 1080, 2);
        let g = &pv.geom;
        let dtrack = &g.dropdown_track;
        let dhandle = &g.dropdown_handle;
        assert!(dhandle.x >= dtrack.x - 0.5);
        assert!(dhandle.x + dhandle.w <= dtrack.x + dtrack.w + 0.5);

        let dwtrack = &g.dropdown_width_track;
        let dwhandle = &g.dropdown_width_handle;
        assert!(dwhandle.x >= dwtrack.x - 0.5);
        assert!(dwhandle.x + dwhandle.w <= dwtrack.x + dwtrack.w + 0.5);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn each_tab_bands_stack_without_overlap_and_fit() {
        // For each tab, the visible bands run strictly top-down with no overlap and
        // every band fits within the panel rect. We pick one representative rect per
        // band (for side-by-side controls, the left/primary one) in layout order.
        let representatives: [Vec<fn(&PanelGeom) -> Rect>; 5] = [
            // Tab 0 "Look": opacity track, radius track, theme combo (closed).
            vec![
                |g| g.slider_track,
                |g| g.radius_track,
                |g| g.theme_combo,
            ],
            // Tab 1 "Fonts": font-size stepper, font scroll, last font row,
            //                ui-size stepper, ui scroll, last ui row.
            vec![
                |g| g.font_minus,
                |g| g.font_scroll_up,
                |g| *g.font_rows.last().unwrap(),
                |g| g.ui_font_minus,
                |g| g.ui_font_scroll_up,
                |g| *g.ui_font_rows.last().unwrap(),
            ],
            // Tab 2 "Window": summon, win-mode, drop-h track, drop-w track,
            //                 tab-bar, auto-hide.
            vec![
                |g| g.summon_prev,
                |g| g.win_mode_prev,
                |g| g.dropdown_track,
                |g| g.dropdown_width_track,
                |g| g.tab_bar_prev,
                |g| g.autohide_toggle,
            ],
            // Tab 3 "Shell": shell cycler, launch toggle.
            vec![|g| g.shell_prev, |g| g.launch_login_toggle],
            // Tab 4 "Effects": CRT-section bands (1–9) fit within the panel at
            // 1920×1080 (py=244, content_top=340, panel bottom=836). Caret bands
            // (11–14) overflow past the viewport — that is expected and handled
            // by the scroll mechanism. Only the CRT bands are listed here so the
            // "fits within panel" assertion holds.
            vec![
                |g: &PanelGeom| g.crt_enabled_toggle,   // band 1
                |g: &PanelGeom| g.crt_curvature_track,  // band 2
                |g: &PanelGeom| g.crt_scanline_track,   // band 3
                |g: &PanelGeom| g.crt_mask_track,       // band 4
                |g: &PanelGeom| g.crt_bloom_track,      // band 5
                |g: &PanelGeom| g.crt_chromatic_track,  // band 6
                |g: &PanelGeom| g.crt_vignette_track,   // band 7
                |g: &PanelGeom| g.crt_tint_r_track,     // band 8 (RGB triple – leftmost)
                |g: &PanelGeom| g.crt_roll_toggle,      // band 9 (leftmost chip)
            ],
        ];

        for (tab, reps) in representatives.iter().enumerate() {
            let pv = panel_tab(1920, 1080, tab);
            let g = &pv.geom;
            let rects: Vec<Rect> = reps.iter().map(|f| f(g)).collect();
            let mut prev_bottom = g.panel.y; // content starts below the panel top
            for (i, r) in rects.iter().enumerate() {
                assert!(is_visible(r), "tab {tab} band {i} unexpectedly offscreen");
                assert!(
                    r.y + 0.5 >= prev_bottom,
                    "tab {tab} band {i} (y={}) overlaps previous (bottom={prev_bottom})",
                    r.y
                );
                assert!(
                    r.y >= g.panel.y && r.y + r.h <= g.panel.y + g.panel.h + 0.5,
                    "tab {tab} band {i} spills past the panel (y={}, bottom={})",
                    r.y,
                    r.y + r.h
                );
                prev_bottom = r.y + r.h;
            }
        }
    }

    #[test]
    fn theme_dropdown_rows_share_geometry() {
        // Rows are a single column inside the floating menu: every visible row
        // shares the same x and width, and rows never overflow the footer band
        // (the menu floats over content, not over the panel edge).
        let pv = panel_tab_ex(1920, 1080, 0, true, 0);
        let g = &pv.geom;
        assert!(g.theme_rows.len() >= 2);
        let x0 = g.theme_rows[0].x;
        let w0 = g.theme_rows[0].w;
        for (i, row) in g.theme_rows.iter().enumerate() {
            assert!((row.x - x0).abs() < 0.5, "row {i} x differs");
            assert!((row.w - w0).abs() < 0.5, "row {i} w differs");
        }
        let last = g.theme_rows.last().unwrap();
        assert!(
            last.y + last.h <= g.panel.y + PANEL_H - FOOTER_H + 0.5,
            "open theme menu spills into the footer band"
        );
    }

    #[test]
    fn caps_headers_present_per_tab() {
        // Each tab carries only its own CAPS headers.
        let expected: [&[&str]; 5] = [
            &["OPACITY", "CORNER RADIUS", "THEME"],
            &["FONT SIZE", "FONT", "UI FONT SIZE", "UI FONT"],
            &[
                "SUMMON EFFECT", "WINDOW MODE", "TAB BAR", "DROPDOWN HEIGHT",
                "DROPDOWN WIDTH", "AUTO-HIDE ON FOCUS LOSS",
            ],
            &["SHELL", "LAUNCH AT LOGIN"],
            // Tab 4 "Effects": section headers + every widget CAPS label.
            &[
                "CRT", "CRT ENABLED", "CURVATURE", "SCANLINE", "MASK",
                "BLOOM", "CHROMATIC", "VIGNETTE", "TINT", "ANIMATE",
                "CARET", "FLASH", "GLOW", "FLASH MS", "COLOR",
            ],
        ];
        for (tab, headers) in expected.iter().enumerate() {
            let pv = panel_tab(1920, 1080, tab);
            // For the Effects tab (4) the widget headers land in `effects_labels`;
            // for other tabs they are in `labels`. Combine both for the check.
            let all_text: Vec<String> = pv.labels.iter()
                .chain(pv.effects_labels.iter())
                .map(|l| l.0.clone())
                .collect();
            for h in headers.iter() {
                assert!(
                    all_text.iter().any(|s| s == h),
                    "tab {tab} missing CAPS header: {h}"
                );
            }
        }
    }

    #[test]
    fn right_aligned_values_in_labels() {
        // Opacity/corner-radius values live on tab 0; font size on tab 1; dropdown
        // height on tab 2. Each must appear as a separate value label (readouts
        // sit on the header row or inside the stepper control).
        let t0: Vec<String> = panel_tab(1920, 1080, 0).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t0.iter().any(|s| s == "97%"), "missing opacity value label");
        assert!(t0.iter().any(|s| s == "8px"), "missing corner radius value label");

        let t1: Vec<String> = panel_tab(1920, 1080, 1).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t1.iter().any(|s| s == "15pt"), "missing font size value label");

        let t2: Vec<String> = panel_tab(1920, 1080, 2).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t2.iter().any(|s| s == "50%"), "missing dropdown height value label");
    }

    #[test]
    fn footer_hint_present_on_every_tab() {
        // The footer band (hairline + hint) anchors the panel bottom on all tabs.
        for tab in 0..5 {
            let pv = panel_tab(1920, 1080, tab);
            assert!(
                pv.labels.iter().any(|l| l.0.contains("Esc to close")),
                "tab {tab} missing footer hint"
            );
        }
    }

    #[test]
    fn ui_font_section_present_and_well_formed() {
        // The UI-font section lives on the Fonts tab (1).
        let pv = panel_tab(1920, 1080, 1);
        let g = &pv.geom;
        let panel = &g.panel;
        for (r, name) in [
            (&g.ui_font_minus, "ui_font_minus"),
            (&g.ui_font_plus, "ui_font_plus"),
            (&g.ui_font_reset, "ui_font_reset"),
            (&g.ui_font_scroll_up, "ui_font_scroll_up"),
            (&g.ui_font_scroll_down, "ui_font_scroll_down"),
        ] {
            assert!(
                r.y >= panel.y && r.y + r.h <= panel.y + panel.h + 0.5,
                "{name} outside panel vertically"
            );
        }
        assert_eq!(g.ui_font_rows.len(), 4, "expected 4 visible UI-font rows (cap)");
        let all_text: Vec<String> = pv.labels.iter().map(|l| l.0.clone()).collect();
        assert!(all_text.iter().any(|s| s == "18pt"), "missing UI font size value label");
        assert!(
            all_text.iter().any(|s| s == "System Sans (default)"),
            "missing synthetic System Sans row"
        );
        // The specimen anchor is below the UI-font-size stepper and above the list.
        let (sx, sy) = pv.ui_specimen_pos;
        assert!(sx >= panel.x && sx <= panel.x + panel.w, "specimen x out of panel");
        assert!(
            sy > g.ui_font_minus.y + g.ui_font_minus.h && sy < g.ui_font_rows[0].y,
            "specimen anchor ({sy}) not between the size stepper and the family list"
        );
    }

    #[test]
    fn specimen_offscreen_when_fonts_tab_inactive() {
        // On non-Fonts tabs the "Aa" specimen must be offscreen (not drawn).
        for tab in [0usize, 2, 3, 4] {
            let pv = panel_tab(1920, 1080, tab);
            assert!(
                pv.ui_specimen_pos.1 >= 1.0e5,
                "specimen should be offscreen on tab {tab}"
            );
        }
    }
}
