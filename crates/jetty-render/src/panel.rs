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
    /// "Launch at login" toggle pill.
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
    /// UI-font-size reset button ("rst").
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
    /// "CRT ENABLED" master toggle pill.
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
    /// CRT animation toggle pills (roll / flicker / jitter), three on one band.
    pub crt_roll_toggle: Rect,
    pub crt_flicker_toggle: Rect,
    pub crt_jitter_toggle: Rect,
    /// Caret flash-enabled toggle pill.
    pub caret_flash_toggle: Rect,
    /// Caret glow-enabled toggle pill.
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
    /// `last_band_bottom − content_top`. Stored for the scroll task (Task 6).
    /// Independent of screen size; always 652.0 (14 bands × 44 px pitch + 36 px
    /// for the last slider handle's bottom offset).
    pub effects_content_h: f32,

    // ── Scroll viewport bounds (used by hit-testing for the Effects tab) ──────
    /// Y coordinate (physical px) where the scrollable content area begins, i.e.
    /// `panel.y + 100`. Clicks above this row must not trigger Effects widgets
    /// even if an Effects rect has scrolled into that region.
    pub content_top: f32,
    /// Y coordinate (physical px) of the panel's bottom edge, i.e.
    /// `panel.y + PANEL_H`. Clicks at or below this row are outside the panel.
    pub content_bottom: f32,
}

/// Full description of how to draw the settings panel for one frame.
pub struct PanelView {
    /// Chrome rects in draw order (border → bg → chip highlights → chip fills →
    /// opacity/radius tracks → scrollbar indicator).  These are rendered WITHOUT
    /// a scissor so the panel chrome is always fully visible.
    pub quads: Vec<Rect>,
    /// Chrome text labels: title, tab strip, section headers for non-Effects tabs,
    /// font/window/shell widget values.  Rendered without clip bounds.
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

/// Short display names for each preset, in PRESETS order.
const CHIP_NAMES: [&str; 5] = ["Mocha", "Tokyo", "Gruv", "Drac", "Onyx"];

/// Maximum number of font-family rows displayed in the panel at once.
/// If more families exist, the list scrolls via `font_scroll_offset`.
const MAX_FONT_ROWS: usize = 5;

/// Maximum number of UI-font-family rows shown at once. Kept to 4 (not 5) to
/// bound the panel's vertical growth from the new section.
const MAX_UI_FONT_ROWS: usize = 4;

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
pub(crate) const CHAR_W_FALLBACK: f32 = 9.8;

/// Settings-panel dimensions in logical px. The separate Settings OS window is
/// sized to these (+ border) — see `SETTINGS_WIN_*` in jetty-app. Growing the
/// panel here automatically resizes that window.
///
/// The panel is organised into FIVE tabs (a strip of clickable labels sits just
/// under the title): only the active tab's bands are laid out, so PANEL_H is
/// sized to the TALLEST tab rather than the sum of every band:
///
/// * Tab 0 "Look"    — opacity slider, corner-radius slider, theme cards.
/// * Tab 1 "Fonts"   — font-size, font-family list, UI-font-size + "Aa" specimen,
///                     UI-font-family list. (TALLEST non-scrolling tab — drives PANEL_H.)
/// * Tab 2 "Window"  — summon effect, window mode, dropdown height, dropdown
///                     width, tab-bar position, auto-hide toggle.
/// * Tab 3 "Shell"   — shell picker, launch-at-login toggle.
/// * Tab 4 "Effects" — CRT controls (enable + curvature/scanline/mask/bloom/
///                     chromatic/vignette/tint/animate) and Caret flash/glow.
///                     Its content exceeds PANEL_H, so this tab alone scrolls
///                     (GPU-clipped to the viewport; see `effects_viewport`).
///
/// All five tabs share one `content_top` (py+100) and lay their bands out
/// top-down from there. The Fonts tab's last UI-font row bottoms at ~py+520, so
/// PANEL_H = 560 leaves a comfortable bottom margin; shorter tabs simply have
/// empty space below their last band.
pub const PANEL_W: f32 = 380.0;
pub const PANEL_H: f32 = 560.0;

/// Y-offset from the panel's top edge to the scrollable content area (title bar
/// + tab strip chrome). Single-sourced here so `EFFECTS_VISIBLE_H` and the
/// `content_top` local in `build_panel` can both derive from it.
const CONTENT_TOP_OFFSET: f32 = 100.0;

/// Total pixel height of the Effects-tab content region (14 bands × 44 px pitch
/// + 36 px for the last slider handle's bottom offset). Single source of truth
/// for the scroll clamp in `App` and the scrollbar indicator in `build_panel`.
/// Value: `14 × 44 + 36 = 652.0`.
pub const EFFECTS_CONTENT_H: f32 = 14.0 * 44.0 + 36.0;

/// Visible height of the Effects-tab content viewport (the panel height minus the
/// chrome strip). Scroll is clamped to `[0, EFFECTS_CONTENT_H − EFFECTS_VISIBLE_H]`
/// = `[0, 192.0]`. Value: `PANEL_H − CONTENT_TOP_OFFSET = 460.0`.
pub const EFFECTS_VISIBLE_H: f32 = PANEL_H - CONTENT_TOP_OFFSET;

// Compile-time lockstep: CHIP_NAMES must always have one entry per PRESETS entry.
// This panics at build time if PRESETS grows without updating CHIP_NAMES (or vice versa).
const _: () = assert!(CHIP_NAMES.len() == jetty_core::theme::PRESETS.len());

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
    // `launch_at_login`: drives the "LAUNCH AT LOGIN" toggle pill (accent when
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
    //   and toggle pill states. Ignored on other tabs (bands are at OFF).
    effects: &EffectsParams,
    // `effects_scroll`: vertical scroll offset (physical px, 0 = top) for the
    //   Effects tab.  Subtracted from every `t_fx_*` band top when active_tab==4
    //   so the drawn positions and the PanelGeom hit-rects stay in sync.
    //   Clamped to [0, max(0, content_h - visible_h)] by the caller (App).
    //   Ignored on other tabs (bands are at OFF regardless).
    effects_scroll: f32,
) -> PanelView {
    let active_tab = active_tab.min(4);

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

    // Center, then apply the user drag offset, then clamp to screen edges.
    let sw = screen_w as f32;
    let sh = screen_h as f32;
    let px = (((sw - PANEL_W) / 2.0).floor() + dx).clamp(0.0, (sw - PANEL_W).max(0.0));
    let py = (((sh - PANEL_H) / 2.0).floor() + dy).clamp(0.0, (sh - PANEL_H).max(0.0));

    // ── Per-band tops ────────────────────────────────────────────────────────
    // Every band keeps its existing internal (sub-element) layout relative to a
    // single "band top" Y. For the ACTIVE tab the band tops run top-down from
    // `content_top`; for every other tab they are OFF (offscreen), so all rects
    // and labels derived from them fall far offscreen and can neither be hit-tested
    // nor seen. This keeps app.rs's hit-test logic tab-agnostic.
    //
    //   Tab 0 "Look":   opacity(48) · corner-radius(48) · theme cards(grid)
    //   Tab 1 "Fonts":  font-size(60) · font list(154) · UI-size+specimen(90) · UI list
    //   Tab 2 "Window": summon · win-mode · drop-h · drop-w · tab-bar · auto-hide (48 each)
    //   Tab 3 "Shell":  shell cycler(48) · launch-at-login toggle
    let content_top = py + 100.0;
    let (mut t_opacity, mut t_radius, mut t_theme) = (OFF, OFF, OFF);
    let (mut t_fontsize, mut t_fontlist, mut t_uifontsize, mut t_uifontlist) =
        (OFF, OFF, OFF, OFF);
    let (mut t_summon, mut t_winmode, mut t_droph, mut t_dropw, mut t_tabbar, mut t_autohide) =
        (OFF, OFF, OFF, OFF, OFF, OFF);
    let (mut t_shell, mut t_launch) = (OFF, OFF);
    // Effects-tab band tops (15 bands, 44 px pitch each).
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
            t_radius = content_top + 48.0;
            t_theme = content_top + 96.0;
        }
        1 => {
            t_fontsize = content_top;
            t_fontlist = content_top + 60.0;
            t_uifontsize = content_top + 214.0;
            t_uifontlist = content_top + 304.0;
        }
        2 => {
            t_summon = content_top;
            t_winmode = content_top + 48.0;
            t_droph = content_top + 96.0;
            t_dropw = content_top + 144.0;
            t_tabbar = content_top + 192.0;
            t_autohide = content_top + 240.0;
        }
        3 => {
            t_shell = content_top;
            t_launch = content_top + 48.0;
        }
        4 => {
            // 15 bands × 44 px pitch, top-to-bottom from content_top.
            // `effects_scroll` (physical px) is subtracted from each band top so
            // the drawn positions and the PanelGeom hit-rects are identical —
            // the caller (App) has already clamped it to [0, max_scroll].
            // The OFF sentinel for inactive tabs is NOT modified here.
            let s = effects_scroll; // shorthand
            t_fx_crt_hdr  = content_top             - s;     // band 0: "CRT" section header
            t_fx_crt_en   = content_top +  1.0*44.0 - s;    // band 1: crt_enabled toggle
            t_fx_curv     = content_top +  2.0*44.0 - s;    // band 2: crt_curvature slider
            t_fx_scan     = content_top +  3.0*44.0 - s;    // band 3: crt_scanline slider
            t_fx_mask     = content_top +  4.0*44.0 - s;    // band 4: crt_mask slider
            t_fx_bloom    = content_top +  5.0*44.0 - s;    // band 5: crt_bloom slider
            t_fx_chroma   = content_top +  6.0*44.0 - s;    // band 6: crt_chromatic slider
            t_fx_vignette = content_top +  7.0*44.0 - s;    // band 7: crt_vignette slider
            t_fx_tint     = content_top +  8.0*44.0 - s;    // band 8: crt_scanline_tint RGB
            t_fx_anim     = content_top +  9.0*44.0 - s;    // band 9: roll/flicker/jitter pills
            t_fx_caret_hdr= content_top + 10.0*44.0 - s;   // band 10: "CARET" section header
            t_fx_flash    = content_top + 11.0*44.0 - s;   // band 11: caret_flash_enabled toggle
            t_fx_glow     = content_top + 12.0*44.0 - s;   // band 12: caret_glow_enabled toggle
            t_fx_dur      = content_top + 13.0*44.0 - s;   // band 13: caret_flash_ms slider
            t_fx_color    = content_top + 14.0*44.0 - s;   // band 14: caret_flash_color RGB
        }
        _ => {
            t_shell = content_top;
            t_launch = content_top + 48.0;
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

    // --- Tab strip (py+50 .. py+82): 5 evenly distributed clickable labels ---
    // Labels baseline at py+56; the active tab gets a 2px accent underline at
    // py+78. The hit rects span the full cell so the whole label area is clickable.
    let tab_w = (PANEL_W - 32.0) / 5.0;
    let tab_strip_y = py + 56.0;
    let mut tab_rects: [Rect; 5] = [Rect::default(); 5];
    for (i, r) in tab_rects.iter_mut().enumerate() {
        let cell_x = px + 16.0 + i as f32 * tab_w;
        *r = Rect {
            x: cell_x,
            y: py + 50.0,
            w: tab_w,
            h: 30.0,
            color: [0, 0, 0, 0],
            ..Default::default()
        };
    }

    // Helper: right-align a text value on the section-header row.
    // Returns the x-position such that the text's right edge sits at px+PANEL_W-16.
    let right_x = |text: &str| -> f32 {
        let w = text.chars().count() as f32 * char_w;
        px + PANEL_W - 16.0 - w
    };

    // --- Opacity band (Look) ---
    // CAPS label + value on the header row (t_opacity); track at +24 (h=6); handle
    // at +18 (h=18). The filled left portion of the track shows progress.
    let slider_track = Rect::rounded(px + 16.0, t_opacity + 24.0, 348.0, 6.0, slider_track_col, 3.0);
    let frac = ((opacity - 0.1) / 0.9).clamp(0.0, 1.0);
    let handle_x = px + 16.0 + frac * (348.0 - 14.0);
    let slider_handle = Rect::rounded(handle_x, t_opacity + 18.0, 14.0, 18.0, accent_col, 4.0);
    let opacity_fill_w = (frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let opacity_fill = Rect::rounded(px + 16.0, t_opacity + 24.0, opacity_fill_w, 6.0, accent_fill, 3.0);

    // --- Corner-radius band (Look) ---
    // Radius range is [0, 24] px.
    const RADIUS_MAX: f32 = 24.0;
    let radius_track = Rect::rounded(px + 16.0, t_radius + 24.0, 348.0, 6.0, slider_track_col, 3.0);
    let r_frac = (corner_radius / RADIUS_MAX).clamp(0.0, 1.0);
    let radius_handle_x = px + 16.0 + r_frac * (348.0 - 14.0);
    let radius_handle = Rect::rounded(radius_handle_x, t_radius + 18.0, 14.0, 18.0, accent_col, 4.0);
    let radius_fill_w = (r_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let radius_fill = Rect::rounded(px + 16.0, t_radius + 24.0, radius_fill_w, 6.0, accent_fill, 3.0);

    // Shared x-positions for the ‹ / › cycle buttons (Summon/WinMode/TabBar/Shell).
    let cycle_prev_x = px + 200.0;
    let cycle_next_x = px + PANEL_W - 16.0 - 28.0; // rightmost

    // --- Summon-effect band (Window) ---
    // CAPS label at t_summon; ‹ name › cycle control INLINE on the same row,
    // right-aligned (button h=28 centred on the header text → top at t_summon-6).
    let summon_btn_y = t_summon - 6.0;
    let summon_prev = Rect::rounded(cycle_prev_x, summon_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let summon_next = Rect::rounded(cycle_next_x, summon_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Window-mode band (Window) ---
    let winmode_btn_y = t_winmode - 6.0;
    let win_mode_prev = Rect::rounded(cycle_prev_x, winmode_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let win_mode_next = Rect::rounded(cycle_next_x, winmode_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Tab-bar band (Window) ---
    let tabbar_btn_y = t_tabbar - 6.0;
    let tab_bar_prev = Rect::rounded(cycle_prev_x, tabbar_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let tab_bar_next = Rect::rounded(cycle_next_x, tabbar_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // --- Dropdown-height band (Window) ---
    // CAPS label + value at t_droph; track at +24 (h=6); handle at +18 (h=18).
    // Range 25%..100%. Grayed (treated as no-op) when mode==Center.
    let dropdown_track = Rect::rounded(px + 16.0, t_droph + 24.0, 348.0, 6.0, slider_track_col, 3.0);
    let dh_frac = ((dropdown_height_pct - 0.25) / 0.75).clamp(0.0, 1.0);
    let dropdown_handle_x = px + 16.0 + dh_frac * (348.0 - 14.0);
    let dropdown_handle = Rect::rounded(dropdown_handle_x, t_droph + 18.0, 14.0, 18.0, accent_col, 4.0);
    let dh_fill_w = (dh_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let dh_fill = Rect::rounded(px + 16.0, t_droph + 24.0, dh_fill_w, 6.0, accent_fill, 3.0);

    // --- Dropdown-width band (Window) ---
    // Range 20%..100%. Grayed (treated as no-op) when mode==Center.
    let dropdown_width_track = Rect::rounded(px + 16.0, t_dropw + 24.0, 348.0, 6.0, slider_track_col, 3.0);
    let dw_frac = ((dropdown_width_pct - 0.2) / 0.8).clamp(0.0, 1.0);
    let dropdown_width_handle_x = px + 16.0 + dw_frac * (348.0 - 14.0);
    let dropdown_width_handle = Rect::rounded(dropdown_width_handle_x, t_dropw + 18.0, 14.0, 18.0, accent_col, 4.0);
    let dw_fill_w = (dw_frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
    let dw_fill = Rect::rounded(px + 16.0, t_dropw + 24.0, dw_fill_w, 6.0, accent_fill, 3.0);

    // --- Auto-hide band (Window) ---
    // CAPS label at t_autohide; toggle pill at the right (h=28). Accent when ON.
    let autohide_pill_col: [u8; 4] = if focus_autohide { accent_col } else { btn_fill };
    let autohide_toggle = Rect::rounded(px + PANEL_W - 16.0 - 56.0, t_autohide, 56.0, 28.0, autohide_pill_col, 14.0);

    // --- Font-size band (Fonts) ---
    // CAPS label + value at t_fontsize; buttons at +20 (h=28).
    let font_btn_y = t_fontsize + 20.0;
    let font_minus_x = px + 200.0;
    let font_plus_x  = font_minus_x + 36.0;
    let font_reset_x = font_plus_x  + 36.0;

    let font_minus = Rect::rounded(font_minus_x, font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let font_plus = Rect::rounded(font_plus_x, font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let font_reset = Rect::rounded(font_reset_x, font_btn_y, 44.0, 28.0, btn_fill, 4.0);

    // --- Font scroll buttons (▲ / ▼) on the "FONT" header row (Fonts) ---
    let scroll_btn_y = t_fontlist - 2.0;
    let scroll_down_x = px + PANEL_W - 16.0 - 20.0;        // ▼ rightmost
    let scroll_up_x   = scroll_down_x - 24.0;               // ▲ left of ▼
    let font_scroll_up = Rect::rounded(scroll_up_x, scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);
    let font_scroll_down = Rect::rounded(scroll_down_x, scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);

    // --- Font-family list (Fonts) ---
    // "FONT" header at t_fontlist; rows start at +22. 5 rows × (22+2) = 120px.
    const ROW_H: f32 = 22.0;
    const ROW_GAP: f32 = 2.0;
    let list_top = t_fontlist + 22.0;
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

    // ── UI (chrome) FONT size band + "Aa" specimen (Fonts) ───────────────────
    //  t_uifontsize       "UI FONT SIZE" CAPS header + right-aligned "Npt" readout
    //  t_uifontsize+20    − / + / rst buttons (h=28)
    //  t_uifontsize+58    live "Aa" specimen baseline (drawn at the TRUE ui_font_size)
    let ui_font_btn_y = t_uifontsize + 20.0;
    let ui_font_minus_x = px + 200.0;
    let ui_font_plus_x = ui_font_minus_x + 36.0;
    let ui_font_reset_x = ui_font_plus_x + 36.0;
    let ui_font_minus = Rect::rounded(ui_font_minus_x, ui_font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let ui_font_plus = Rect::rounded(ui_font_plus_x, ui_font_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let ui_font_reset = Rect::rounded(ui_font_reset_x, ui_font_btn_y, 44.0, 28.0, btn_fill, 4.0);

    // Live "Aa" specimen baseline. The app overdraws "Aa" here at the TRUE UI size
    // AFTER the capped panel-text pass. Offscreen unless the Fonts tab is active,
    // so the "Aa" is only drawn on that tab.
    let ui_specimen_pos = if active_tab == 1 {
        (px + 16.0, t_uifontsize + 58.0)
    } else {
        (px + 16.0, OFF)
    };

    // ── UI (chrome) FONT family list (Fonts) ─────────────────────────────────
    //  t_uifontlist       "UI FONT" list header + ▲/▼ scroll arrows + "(shown/total)"
    //  t_uifontlist+22    4 family rows × (22+2)
    let ui_scroll_btn_y = t_uifontlist - 2.0;
    let ui_scroll_down_x = px + PANEL_W - 16.0 - 20.0;
    let ui_scroll_up_x = ui_scroll_down_x - 24.0;
    let ui_font_scroll_up = Rect::rounded(ui_scroll_up_x, ui_scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);
    let ui_font_scroll_down = Rect::rounded(ui_scroll_down_x, ui_scroll_btn_y, 20.0, 20.0, btn_fill, 4.0);

    // UI FONT family list — 4 rows. Index 0 of ui_families is "System Sans".
    let ui_list_top = t_uifontlist + 22.0;
    let ui_offset = ui_font_scroll_offset.min(ui_families.len().saturating_sub(1));
    let ui_visible_count = (ui_families.len().saturating_sub(ui_offset)).min(MAX_UI_FONT_ROWS);
    let mut ui_font_row_rects: Vec<Rect> = Vec::with_capacity(ui_visible_count);
    for i in 0..ui_visible_count {
        let row_y = ui_list_top + i as f32 * (ROW_H + ROW_GAP);
        let family_idx = ui_offset + i;
        // Index 0 = "System Sans (default)" → maps to "" (empty selection).
        let row_value: &str = if family_idx == 0 {
            ""
        } else {
            ui_families.get(family_idx).map(|n| n.as_str()).unwrap_or("")
        };
        let is_selected = row_value == selected_ui_family;
        let row_color = if is_selected { row_sel } else { row_unsel };
        ui_font_row_rects.push(Rect::rounded(list_x, row_y, list_w, ROW_H, row_color, 3.0));
    }

    // --- Theme cards — 2-column × 3-row grid (Look) ---
    // "THEME" label at t_theme; cards start at +20.
    // Each card: col_w = (348 − 8) / 2 = 170px; card_h = 40px; row_gap = 8px.
    let presets = jetty_core::theme::PRESETS;
    let num_presets = presets.len(); // 5

    const CARD_COLS: usize = 2;
    const CARD_H: f32 = 40.0;
    const CARD_ROW_GAP: f32 = 8.0;
    const CARD_COL_GAP: f32 = 8.0;
    let card_w = (348.0 - (CARD_COLS as f32 - 1.0) * CARD_COL_GAP) / CARD_COLS as f32; // 170px
    let card_top = t_theme + 20.0;

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

    // --- Launch-at-login band (Shell) ---
    // CAPS label at t_launch; toggle pill at the right (h=28). Accent when ON.
    let launch_login_pill_col: [u8; 4] = if launch_at_login { accent_col } else { btn_fill };
    let launch_login_toggle =
        Rect::rounded(px + PANEL_W - 16.0 - 56.0, t_launch, 56.0, 28.0, launch_login_pill_col, 14.0);

    // --- Shell band (Shell) ---
    // CAPS label + ‹ name › cycler, MIRRORING the SUMMON EFFECT / WINDOW MODE
    // bands exactly (same ‹/› button rects, same name-centering).
    let shell_btn_y = t_shell - 6.0;
    let shell_prev = Rect::rounded(cycle_prev_x, shell_btn_y, 28.0, 28.0, btn_fill, 4.0);
    let shell_next = Rect::rounded(cycle_next_x, shell_btn_y, 28.0, 28.0, btn_fill, 4.0);

    // ── Effects tab (tab 4) widget geometry ───────────────────────────────────
    // Patterns reused from existing bands:
    //   • Full-width slider (348 px track): opacity slider pattern (lines above).
    //   • Toggle pill (56×28, radius 14): autohide_toggle / launch_login_toggle.
    //   • RGB mini-slider triple: three 108 px tracks + 12 px gaps = 348 px total.
    //     3×108 + 2×12 = 324 + 24 = 348. Handle: 14×18 (same as full slider).
    //
    // When active_tab ≠ 4, all band tops are OFF so every derived rect lands
    // far offscreen and can never be hit-tested or seen.

    // ── Helper values ──
    let fx_track_x = px + 16.0;        // left edge of full-width slider track
    let fx_pill_x  = px + PANEL_W - 16.0 - 56.0; // right-aligned single toggle pill

    // RGB mini-track x positions: R at fx_track_x, G and B offset by 120 px each.
    //   108 px track + 12 px gap = 120 px per column; 3 × 108 + 2 × 12 = 348. ✓
    // RGB mini-sliders for TINT/COLOR: start AFTER the section header (which sits
    // at px+16) so the header label and the "R" sub-label/slider never collide.
    // Three 88px sliders + 14px gaps fit from fx_track_x+56 to the right margin.
    let rgb_r_x = fx_track_x + 56.0;
    let rgb_g_x = fx_track_x + 158.0;
    let rgb_b_x = fx_track_x + 260.0;

    // ── CRT ENABLED toggle (band 1) ──
    let crt_en_col = if effects.crt_enabled { accent_col } else { btn_fill };
    let crt_enabled_toggle = Rect::rounded(fx_pill_x, t_fx_crt_en, 56.0, 28.0, crt_en_col, 14.0);

    // ── Macro-like inline for a full-width [0,1] slider ──
    // Returns (track, fill, handle). Track at band_y+24; handle at band_y+18.
    macro_rules! fx_slider {
        ($band_y:expr, $frac:expr) => {{
            let frac = ($frac as f32).clamp(0.0, 1.0);
            let track  = Rect::rounded(fx_track_x, $band_y + 24.0, 348.0, 6.0, slider_track_col, 3.0);
            let fill_w = (frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
            let fill   = Rect::rounded(fx_track_x, $band_y + 24.0, fill_w, 6.0, accent_fill, 3.0);
            let hx     = fx_track_x + frac * (348.0 - 14.0);
            let handle = Rect::rounded(hx, $band_y + 18.0, 14.0, 18.0, accent_col, 4.0);
            (track, fill, handle)
        }};
    }

    // ── Macro-like inline for a 108-px RGB mini-slider ──
    macro_rules! fx_mini_slider {
        ($bx:expr, $band_y:expr, $frac:expr) => {{
            let frac = ($frac as f32).clamp(0.0, 1.0);
            let track  = Rect::rounded($bx, $band_y + 24.0, 88.0, 6.0, slider_track_col, 3.0);
            let fill_w = (frac * (88.0 - 14.0) + 7.0).max(6.0).min(88.0);
            let fill   = Rect::rounded($bx, $band_y + 24.0, fill_w, 6.0, accent_fill, 3.0);
            let hx     = $bx + frac * (88.0 - 14.0);
            let handle = Rect::rounded(hx, $band_y + 18.0, 14.0, 18.0, accent_col, 4.0);
            (track, fill, handle)
        }};
    }

    // CRT sliders (bands 2–7): curvature, scanline, mask, bloom, chromatic, vignette.
    let (crt_curvature_track,  crt_curvature_fill,  crt_curvature_handle)  = fx_slider!(t_fx_curv,     effects.crt_curvature);
    let (crt_scanline_track,   crt_scanline_fill,   crt_scanline_handle)   = fx_slider!(t_fx_scan,     effects.crt_scanline);
    let (crt_mask_track,       crt_mask_fill,       crt_mask_handle)       = fx_slider!(t_fx_mask,     effects.crt_mask);
    let (crt_bloom_track,      crt_bloom_fill,      crt_bloom_handle)      = fx_slider!(t_fx_bloom,    effects.crt_bloom);
    let (crt_chromatic_track,  crt_chromatic_fill,  crt_chromatic_handle)  = fx_slider!(t_fx_chroma,   effects.crt_chromatic);
    let (crt_vignette_track,   crt_vignette_fill,   crt_vignette_handle)   = fx_slider!(t_fx_vignette, effects.crt_vignette);

    // CRT scanline tint RGB mini-sliders (band 8).
    let (crt_tint_r_track, crt_tint_r_fill, crt_tint_r_handle) = fx_mini_slider!(rgb_r_x, t_fx_tint, effects.crt_scanline_tint[0]);
    let (crt_tint_g_track, crt_tint_g_fill, crt_tint_g_handle) = fx_mini_slider!(rgb_g_x, t_fx_tint, effects.crt_scanline_tint[1]);
    let (crt_tint_b_track, crt_tint_b_fill, crt_tint_b_handle) = fx_mini_slider!(rgb_b_x, t_fx_tint, effects.crt_scanline_tint[2]);

    // CRT animation toggle pills (band 9): ROLL / FLKR / JITR.
    // Start AFTER the "ANIMATE" header (px+16) so the header and the first pill
    // don't collide: three 56px pills spread from fx_track_x+74 to the right
    // margin (jitter ends at fx_track_x+348 = the right edge).
    let roll_col    = if effects.crt_animate_roll { accent_col } else { btn_fill };
    let flicker_col = if effects.crt_flicker      { accent_col } else { btn_fill };
    let jitter_col  = if effects.crt_jitter       { accent_col } else { btn_fill };
    let crt_roll_toggle    = Rect::rounded(fx_track_x + 74.0,  t_fx_anim, 56.0, 28.0, roll_col,    14.0);
    let crt_flicker_toggle = Rect::rounded(fx_track_x + 183.0, t_fx_anim, 56.0, 28.0, flicker_col, 14.0);
    let crt_jitter_toggle  = Rect::rounded(fx_track_x + 292.0, t_fx_anim, 56.0, 28.0, jitter_col,  14.0);

    // Caret toggles (bands 11, 12): caret_flash_enabled, caret_glow_enabled.
    let caret_flash_col = if effects.caret_flash_enabled { accent_col } else { btn_fill };
    let caret_glow_col  = if effects.caret_glow_enabled  { accent_col } else { btn_fill };
    let caret_flash_toggle = Rect::rounded(fx_pill_x, t_fx_flash, 56.0, 28.0, caret_flash_col, 14.0);
    let caret_glow_toggle  = Rect::rounded(fx_pill_x, t_fx_glow,  56.0, 28.0, caret_glow_col,  14.0);

    // Caret flash-duration slider (band 13): maps 60..=400 ms → 0..1.
    let caret_dur_frac = ((effects.caret_flash_ms - 60.0) / (400.0 - 60.0)).clamp(0.0, 1.0);
    let (caret_dur_track, caret_dur_fill, caret_dur_handle) = {
        let frac   = caret_dur_frac;
        let track  = Rect::rounded(fx_track_x, t_fx_dur + 24.0, 348.0, 6.0, slider_track_col, 3.0);
        let fill_w = (frac * (348.0 - 14.0) + 7.0).max(6.0).min(348.0);
        let fill   = Rect::rounded(fx_track_x, t_fx_dur + 24.0, fill_w, 6.0, accent_fill, 3.0);
        let hx     = fx_track_x + frac * (348.0 - 14.0);
        let handle = Rect::rounded(hx, t_fx_dur + 18.0, 14.0, 18.0, accent_col, 4.0);
        (track, fill, handle)
    };

    // Caret flash-color RGB mini-sliders (band 14).
    let (caret_color_r_track, caret_color_r_fill, caret_color_r_handle) = fx_mini_slider!(rgb_r_x, t_fx_color, effects.caret_flash_color[0]);
    let (caret_color_g_track, caret_color_g_fill, caret_color_g_handle) = fx_mini_slider!(rgb_g_x, t_fx_color, effects.caret_flash_color[1]);
    let (caret_color_b_track, caret_color_b_fill, caret_color_b_handle) = fx_mini_slider!(rgb_b_x, t_fx_color, effects.caret_flash_color[2]);

    // Effects content height and visible height come from the module-level
    // `pub const`s so the App's scroll clamp and this scrollbar indicator
    // are guaranteed to use identical values.
    let effects_content_h = EFFECTS_CONTENT_H; // 652.0
    let visible_h = EFFECTS_VISIBLE_H;          // 460.0

    // --- Build quads in draw order ---
    // `quads` = chrome: always visible, rendered WITHOUT scissor.
    // `effects_quads` = Effects-tab content: rendered WITH scissor when tab 4.
    let mut quads: Vec<Rect> = Vec::new();
    let mut effects_quads: Vec<Rect> = Vec::new();
    quads.push(dim_rect);
    quads.push(border_rect);
    quads.push(bg_rect);

    // Active-tab underline bar under the tab strip.
    {
        let name = TAB_NAMES[active_tab];
        let text_w = name.chars().count() as f32 * char_w;
        let cell_x = px + 16.0 + active_tab as f32 * tab_w;
        let bar_w = text_w + 10.0;
        let bar_x = cell_x + (tab_w - bar_w) * 0.5;
        quads.push(Rect {
            x: bar_x,
            y: py + 78.0,
            w: bar_w,
            h: 2.0,
            color: accent_col,
            ..Default::default()
        });
    }

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

    // Launch-at-login toggle pill.
    quads.push(launch_login_toggle);

    // Shell-picker cycle buttons.
    quads.push(shell_prev);
    quads.push(shell_next);

    // ── Effects-tab quads (populate effects_quads; empty when tab ≠ 4) ────────
    // When active_tab == 4 the band tops carry the scroll offset, so these rects
    // are at their scrolled positions. The caller renders them with a hardware
    // scissor rect (`effects_viewport`) so overflow is GPU-clipped.
    // When active_tab ≠ 4 ALL t_fx_* are at OFF (1e6) so the rects land far
    // offscreen — we skip pushing them entirely (they'd be culled anyway, but
    // keeping effects_quads empty simplifies the render path).
    if active_tab == 4 {
        // Draw order per slider band: track → fill → handle (same as opacity slider).
        effects_quads.push(crt_enabled_toggle);
        effects_quads.push(crt_curvature_track);  effects_quads.push(crt_curvature_fill);  effects_quads.push(crt_curvature_handle);
        effects_quads.push(crt_scanline_track);   effects_quads.push(crt_scanline_fill);   effects_quads.push(crt_scanline_handle);
        effects_quads.push(crt_mask_track);       effects_quads.push(crt_mask_fill);       effects_quads.push(crt_mask_handle);
        effects_quads.push(crt_bloom_track);      effects_quads.push(crt_bloom_fill);      effects_quads.push(crt_bloom_handle);
        effects_quads.push(crt_chromatic_track);  effects_quads.push(crt_chromatic_fill);  effects_quads.push(crt_chromatic_handle);
        effects_quads.push(crt_vignette_track);   effects_quads.push(crt_vignette_fill);   effects_quads.push(crt_vignette_handle);
        effects_quads.push(crt_tint_r_track);     effects_quads.push(crt_tint_r_fill);     effects_quads.push(crt_tint_r_handle);
        effects_quads.push(crt_tint_g_track);     effects_quads.push(crt_tint_g_fill);     effects_quads.push(crt_tint_g_handle);
        effects_quads.push(crt_tint_b_track);     effects_quads.push(crt_tint_b_fill);     effects_quads.push(crt_tint_b_handle);
        effects_quads.push(crt_roll_toggle);
        effects_quads.push(crt_flicker_toggle);
        effects_quads.push(crt_jitter_toggle);
        effects_quads.push(caret_flash_toggle);
        effects_quads.push(caret_glow_toggle);
        effects_quads.push(caret_dur_track);      effects_quads.push(caret_dur_fill);      effects_quads.push(caret_dur_handle);
        effects_quads.push(caret_color_r_track);  effects_quads.push(caret_color_r_fill);  effects_quads.push(caret_color_r_handle);
        effects_quads.push(caret_color_g_track);  effects_quads.push(caret_color_g_fill);  effects_quads.push(caret_color_g_handle);
        effects_quads.push(caret_color_b_track);  effects_quads.push(caret_color_b_fill);  effects_quads.push(caret_color_b_handle);

        // ── Scrollbar indicator ──────────────────────────────────────────────
        // Thin accent rect on the right edge of the content viewport, sized and
        // positioned to show the current scroll position. Only emitted when
        // content is taller than the viewport AND when actually scrollable.
        let max_scroll = (effects_content_h - visible_h).max(0.0);
        if max_scroll > 0.0 {
            let indicator_h = (visible_h * visible_h / effects_content_h).max(10.0);
            let t = effects_scroll / max_scroll;
            let indicator_y = content_top + t * (visible_h - indicator_h);
            let ind_col: [u8; 4] = [accent[0], accent[1], accent[2], 160];
            effects_quads.push(Rect::rounded(
                px + PANEL_W - 6.0, indicator_y, 4.0, indicator_h, ind_col, 2.0,
            ));
        }
    }

    // Font-size buttons.
    quads.push(font_minus);
    quads.push(font_plus);
    quads.push(font_reset);

    // Font-family scroll buttons (▲ / ▼).
    quads.push(font_scroll_up);
    quads.push(font_scroll_down);

    // Font-family list background rows.
    quads.extend_from_slice(&font_row_rects);

    // UI (chrome) FONT section: size buttons, scroll buttons, family rows.
    quads.push(ui_font_minus);
    quads.push(ui_font_plus);
    quads.push(ui_font_reset);
    quads.push(ui_font_scroll_up);
    quads.push(ui_font_scroll_down);
    quads.extend_from_slice(&ui_font_row_rects);

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
    const DOT_R: f32 = 8.0;
    const DOT_GAP: f32 = 4.0;
    for i in 0..num_presets {
        let card = &chip_rects[i];
        let t = jetty_core::Theme::by_name(presets[i]);
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
    // `labels` = chrome: title, tab strip, and non-Effects tab widget labels.
    // `effects_labels` = Effects-tab widget labels; clipped to content viewport.
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut effects_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Title.
    labels.push(("Settings".to_string(), px + 16.0, py + 12.0, text_main));

    // Tab strip labels — active tab accent-colored, others dim.
    for (i, name) in TAB_NAMES.iter().enumerate() {
        let cell_x = px + 16.0 + i as f32 * tab_w;
        let text_w = name.chars().count() as f32 * char_w;
        let label_x = cell_x + (tab_w - text_w) * 0.5;
        let col = if i == active_tab { accent } else { text_header };
        labels.push((name.to_string(), label_x, tab_strip_y, col));
    }

    // OPACITY header — CAPS with right-aligned "97%" value.
    let pct = (opacity * 100.0).round() as i32;
    let pct_str = format!("{}%", pct);
    labels.push(("OPACITY".to_string(), px + 16.0, t_opacity, text_header));
    labels.push((pct_str.clone(), right_x(&pct_str), t_opacity, text_main));

    // CORNER RADIUS header — CAPS with right-aligned "Npx" value.
    let radius_px = corner_radius.round() as i32;
    let radius_str = format!("{}px", radius_px);
    labels.push(("CORNER RADIUS".to_string(), px + 16.0, t_radius, text_header));
    labels.push((radius_str.clone(), right_x(&radius_str), t_radius, text_main));

    // Helper: center a (possibly truncated) cycle-name label between the ‹ and ›
    // buttons. The gap runs from [cycle_prev_x+28] to [cycle_next_x]; we clamp
    // the x so a long name never overruns either button.
    let cycle_gap_left  = cycle_prev_x + 28.0; // right edge of ‹ button
    let cycle_gap_right = cycle_next_x;         // left edge of › button
    let cycle_gap_w     = cycle_gap_right - cycle_gap_left;
    let cycle_max_chars = if char_w > 0.0 {
        ((cycle_gap_w / char_w).floor() as usize).max(3)
    } else {
        11
    };
    let center_cycle = |name: &str| -> (String, f32) {
        // Truncate long names to avoid overrunning the buttons.
        let shown: String = if name.chars().count() > cycle_max_chars {
            let mut s: String = name.chars().take(cycle_max_chars - 1).collect();
            s.push('…');
            s
        } else {
            name.to_string()
        };
        let text_w = shown.chars().count() as f32 * char_w;
        let x = (cycle_gap_left + (cycle_gap_w - text_w) * 0.5)
            .clamp(cycle_gap_left, (cycle_gap_right - text_w).max(cycle_gap_left));
        (shown, x)
    };

    // SUMMON EFFECT band (Window) — CAPS header.
    labels.push(("SUMMON EFFECT".to_string(), px + 16.0, t_summon, text_header));
    {
        let (shown, name_x) = center_cycle(summon_effect_name);
        labels.push((shown, name_x, summon_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), cycle_prev_x + 9.0, summon_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), cycle_next_x + 9.0, summon_btn_y + 6.0, text_btn));

    // WINDOW MODE band (Window) — CAPS header.
    labels.push(("WINDOW MODE".to_string(), px + 16.0, t_winmode, text_header));
    {
        let (shown, name_x) = center_cycle(window_mode_name);
        labels.push((shown, name_x, winmode_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), cycle_prev_x + 9.0, winmode_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), cycle_next_x + 9.0, winmode_btn_y + 6.0, text_btn));

    // DROPDOWN HEIGHT band (Window) — CAPS header + right-aligned value.
    let dh_text = if is_dropdown { text_header } else { text_btn };
    let dh_val_text = if is_dropdown { text_main } else { text_btn };
    let dh_pct = (dropdown_height_pct * 100.0).round() as i32;
    let dh_str = format!("{}%", dh_pct);
    labels.push(("DROPDOWN HEIGHT".to_string(), px + 16.0, t_droph, dh_text));
    labels.push((dh_str.clone(), right_x(&dh_str), t_droph, dh_val_text));

    // DROPDOWN WIDTH band (Window) — CAPS header + right-aligned value.
    let dw_text = if is_dropdown { text_header } else { text_btn };
    let dw_val_text = if is_dropdown { text_main } else { text_btn };
    let dw_pct = (dropdown_width_pct * 100.0).round() as i32;
    let dw_str = format!("{}%", dw_pct);
    labels.push(("DROPDOWN WIDTH".to_string(), px + 16.0, t_dropw, dw_text));
    labels.push((dw_str.clone(), right_x(&dw_str), t_dropw, dw_val_text));

    // TAB BAR band (Window) — CAPS header.
    labels.push(("TAB BAR".to_string(), px + 16.0, t_tabbar, text_header));
    {
        let (shown, name_x) = center_cycle(tab_bar_name);
        labels.push((shown, name_x, tabbar_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), cycle_prev_x + 9.0, tabbar_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), cycle_next_x + 9.0, tabbar_btn_y + 6.0, text_btn));

    // AUTO-HIDE band (Window) — CAPS header with ON/OFF pill label.
    labels.push(("AUTO-HIDE ON FOCUS LOSS".to_string(), px + 16.0, t_autohide, text_header));
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

    // FONT SIZE band (Fonts) — CAPS header + right-aligned "Npt" value.
    let fs_display = font_size.round() as i32;
    let fs_str = format!("{}pt", fs_display);
    labels.push(("FONT SIZE".to_string(), px + 16.0, t_fontsize, text_header));
    labels.push((fs_str.clone(), right_x(&fs_str), t_fontsize, text_main));

    // Font button labels.
    labels.push(("-".to_string(),  font_minus_x + 9.0,  font_btn_y + 6.0,  text_btn));
    labels.push(("+".to_string(),  font_plus_x  + 8.0,  font_btn_y + 6.0,  text_btn));
    labels.push(("rst".to_string(), font_reset_x + 6.0, font_btn_y + 6.0,  text_btn));

    // FONT list header (Fonts) — CAPS header; list starts at +22.
    labels.push(("FONT".to_string(), px + 16.0, t_fontlist, text_header));

    // Scroll button labels (▲ / ▼).
    labels.push(("^".to_string(), scroll_up_x   + 6.0, scroll_btn_y + 4.0, text_btn));
    labels.push(("v".to_string(), scroll_down_x + 6.0, scroll_btn_y + 4.0, text_btn));

    // Font-family row labels.
    for i in 0..visible_count {
        let family_idx = offset + i;
        if let Some(name) = families.get(family_idx) {
            let row_y = list_top + i as f32 * (ROW_H + ROW_GAP) + 4.0;
            let is_selected = name.as_str() == selected_family;
            let text_color: [u8; 3] = if is_selected { text_main } else { text_dim };
            // Char-boundary-safe truncation (multibyte-safe).
            let display = if name.chars().count() > 36 {
                let truncated: String = name.chars().take(34).collect();
                format!("{}…", truncated)
            } else {
                name.clone()
            };
            labels.push((display, list_x + 6.0, row_y, text_color));
        }
    }

    // Scroll hint if there are more families than visible rows.
    if families.len() > MAX_FONT_ROWS {
        let hint = format!("({}/{})", offset + visible_count, families.len());
        let scroll_up_left = px + PANEL_W - 60.0;
        let hint_w = hint.chars().count() as f32 * char_w;
        let hint_x = scroll_up_left - 6.0 - hint_w;
        labels.push((hint, hint_x, t_fontlist, text_dim));
    }

    // ── UI FONT section labels (Fonts) ──
    // UI FONT SIZE header + right-aligned "Npt" readout (TRUE size).
    let ui_fs_display = ui_font_size.round() as i32;
    let ui_fs_str = format!("{}pt", ui_fs_display);
    labels.push(("UI FONT SIZE".to_string(), px + 16.0, t_uifontsize, text_header));
    labels.push((ui_fs_str.clone(), right_x(&ui_fs_str), t_uifontsize, text_main));
    labels.push(("-".to_string(),  ui_font_minus_x + 9.0, ui_font_btn_y + 6.0, text_btn));
    labels.push(("+".to_string(),  ui_font_plus_x  + 8.0, ui_font_btn_y + 6.0, text_btn));
    labels.push(("rst".to_string(), ui_font_reset_x + 6.0, ui_font_btn_y + 6.0, text_btn));

    // UI FONT family list header + scroll arrows.
    labels.push(("UI FONT".to_string(), px + 16.0, t_uifontlist, text_header));
    labels.push(("^".to_string(), ui_scroll_up_x   + 6.0, ui_scroll_btn_y + 4.0, text_btn));
    labels.push(("v".to_string(), ui_scroll_down_x + 6.0, ui_scroll_btn_y + 4.0, text_btn));

    // UI-font-family row labels (row 0 = "System Sans (default)" → "").
    for i in 0..ui_visible_count {
        let family_idx = ui_offset + i;
        let row_y = ui_list_top + i as f32 * (ROW_H + ROW_GAP) + 4.0;
        let (name, row_value): (&str, &str) = if family_idx == 0 {
            ("System Sans (default)", "")
        } else {
            let n = ui_families.get(family_idx).map(|s| s.as_str()).unwrap_or("");
            (n, n)
        };
        let is_selected = row_value == selected_ui_family;
        let text_color: [u8; 3] = if is_selected { text_main } else { text_dim };
        let display = if name.chars().count() > 36 {
            let truncated: String = name.chars().take(34).collect();
            format!("{}…", truncated)
        } else {
            name.to_string()
        };
        labels.push((display, list_x + 6.0, row_y, text_color));
    }

    // Scroll hint for the UI-font list if more entries than visible rows.
    if ui_families.len() > MAX_UI_FONT_ROWS {
        let hint = format!("({}/{})", ui_offset + ui_visible_count, ui_families.len());
        let scroll_up_left = px + PANEL_W - 60.0;
        let hint_w = hint.chars().count() as f32 * char_w;
        let hint_x = scroll_up_left - 6.0 - hint_w;
        labels.push((hint, hint_x, t_uifontlist, text_dim));
    }

    // THEME band (Look) — CAPS header.
    labels.push(("THEME".to_string(), px + 16.0, t_theme, text_header));

    // Theme card name labels and color-dot labels.
    for i in 0..num_presets {
        let card = &chip_rects[i];
        // Pick black or white text per card based on its own bg luminance so the
        // name stays legible on any theme swatch.
        let cb = jetty_core::Theme::by_name(presets[i]).bg;
        let lum = 0.299 * cb[0] as f32 + 0.587 * cb[1] as f32 + 0.114 * cb[2] as f32;
        let label_color: [u8; 3] = if lum > 140.0 { [20, 20, 20] } else { [235, 235, 240] };
        labels.push((
            CHIP_NAMES[i].to_string(),
            card.x + 8.0,
            card.y + 22.0, // below the 3-dot row (dots at y+8, dot_h=8 → bottom y+16)
            label_color,
        ));
    }

    // LAUNCH AT LOGIN band (Shell) — CAPS header with ON/OFF pill label.
    labels.push(("LAUNCH AT LOGIN".to_string(), px + 16.0, t_launch, text_header));
    let (launch_pill_text, launch_pill_col) = if launch_at_login {
        ("ON", [20u8, 20, 20])
    } else {
        ("OFF", text_btn)
    };
    labels.push((
        launch_pill_text.to_string(),
        launch_login_toggle.x + 16.0,
        launch_login_toggle.y + 6.0,
        launch_pill_col,
    ));

    // SHELL band (Shell) — CAPS header with a ‹ name › cycler.
    labels.push(("SHELL".to_string(), px + 16.0, shell_btn_y + 6.0, text_header));
    {
        let (shown, name_x) = center_cycle(shell_display);
        labels.push((shown, name_x, shell_btn_y + 6.0, text_main));
    }
    labels.push(("<".to_string(), cycle_prev_x + 9.0, shell_btn_y + 6.0, text_btn));
    labels.push((">".to_string(), cycle_next_x + 9.0, shell_btn_y + 6.0, text_btn));

    // ── Effects-tab labels (into effects_labels; empty when tab ≠ 4) ────────
    // When active_tab == 4, band tops carry the scroll offset so label Y
    // positions already reflect scrolling. The caller renders effects_labels
    // with TextArea.bounds = content viewport so labels outside the viewport
    // are suppressed by glyphon.
    if active_tab == 4 {
        // Section header "CRT" (band 0).
        effects_labels.push(("CRT".to_string(), px + 16.0, t_fx_crt_hdr, text_header));

        // CRT ENABLED band (band 1) — header + pill ON/OFF label.
        effects_labels.push(("CRT ENABLED".to_string(), px + 16.0, t_fx_crt_en, text_header));
        {
            let (txt, col) = if effects.crt_enabled { ("ON", [20u8, 20, 20]) } else { ("OFF", text_btn) };
            effects_labels.push((txt.to_string(), crt_enabled_toggle.x + 16.0, crt_enabled_toggle.y + 6.0, col));
        }

        // CRT slider bands (2–7): CAPS header + right-aligned "N%" value.
        macro_rules! fx_slider_label {
            ($label:expr, $band_y:expr, $val:expr) => {
                effects_labels.push(($label.to_string(), px + 16.0, $band_y, text_header));
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

        // CRT scanline-tint RGB triple (band 8): section header + "R"/"G"/"B" sub-labels.
        effects_labels.push(("TINT".to_string(), px + 16.0, t_fx_tint, text_header));
        effects_labels.push(("R".to_string(), rgb_r_x,        t_fx_tint, text_dim));
        effects_labels.push(("G".to_string(), rgb_g_x,        t_fx_tint, text_dim));
        effects_labels.push(("B".to_string(), rgb_b_x,        t_fx_tint, text_dim));

        // CRT animation pills (band 9): header + three pill labels.
        effects_labels.push(("ANIMATE".to_string(), px + 16.0, t_fx_anim, text_header));
        {
            let (rt, rc) = if effects.crt_animate_roll { ("ROLL",  [20u8, 20, 20]) } else { ("ROLL",  text_btn) };
            let (ft, fc) = if effects.crt_flicker      { ("FLKR",  [20u8, 20, 20]) } else { ("FLKR",  text_btn) };
            let (jt, jc) = if effects.crt_jitter       { ("JITR",  [20u8, 20, 20]) } else { ("JITR",  text_btn) };
            effects_labels.push((rt.to_string(), crt_roll_toggle.x    + 8.0, crt_roll_toggle.y    + 6.0, rc));
            effects_labels.push((ft.to_string(), crt_flicker_toggle.x + 8.0, crt_flicker_toggle.y + 6.0, fc));
            effects_labels.push((jt.to_string(), crt_jitter_toggle.x  + 8.0, crt_jitter_toggle.y  + 6.0, jc));
        }

        // Section header "CARET" (band 10).
        effects_labels.push(("CARET".to_string(), px + 16.0, t_fx_caret_hdr, text_header));

        // CARET FLASH ENABLED toggle (band 11).
        effects_labels.push(("FLASH".to_string(), px + 16.0, t_fx_flash, text_header));
        {
            let (txt, col) = if effects.caret_flash_enabled { ("ON", [20u8, 20, 20]) } else { ("OFF", text_btn) };
            effects_labels.push((txt.to_string(), caret_flash_toggle.x + 16.0, caret_flash_toggle.y + 6.0, col));
        }

        // CARET GLOW ENABLED toggle (band 12).
        effects_labels.push(("GLOW".to_string(), px + 16.0, t_fx_glow, text_header));
        {
            let (txt, col) = if effects.caret_glow_enabled { ("ON", [20u8, 20, 20]) } else { ("OFF", text_btn) };
            effects_labels.push((txt.to_string(), caret_glow_toggle.x + 16.0, caret_glow_toggle.y + 6.0, col));
        }

        // FLASH MS slider (band 13): maps 60..=400 → shows raw ms value.
        effects_labels.push(("FLASH MS".to_string(), px + 16.0, t_fx_dur, text_header));
        {
            let ms_str = format!("{}ms", effects.caret_flash_ms.round() as i32);
            effects_labels.push((ms_str.clone(), right_x(&ms_str), t_fx_dur, text_main));
        }

        // CARET flash-color RGB triple (band 14): section header + "R"/"G"/"B" sub-labels.
        effects_labels.push(("COLOR".to_string(), px + 16.0, t_fx_color, text_header));
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
    let geom = PanelGeom {
        panel: panel_rect,
        tab_rects,
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
        content_bottom: py + PANEL_H,
    };

    // Viewport for the Effects tab scissor / text-clip pass.
    // `None` on other tabs so the caller skips the scissored pass entirely.
    let effects_viewport = if active_tab == 4 {
        Some([px as u32, content_top as u32, PANEL_W as u32, visible_h as u32])
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
        )
    }

    /// Tab-0 ("Look") panel, the most commonly asserted layout.
    fn default_panel(screen_w: u32, screen_h: u32) -> PanelView {
        panel_tab(screen_w, screen_h, 0)
    }

    /// The visible (on-screen) rects for the active tab — drops any rect pushed
    /// offscreen (inactive-tab band). Used to assert per-tab layout.
    fn is_visible(r: &Rect) -> bool {
        r.y < 1.0e5
    }

    #[test]
    fn panel_fits_on_screen_at_various_sizes() {
        // Screen sizes that can fully contain the panel (PANEL_H=560, PANEL_W=380).
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
            // All 5 tab hit rects exist, are inside the panel horizontally, and sit
            // in the strip band (below the title, above content).
            for (i, r) in g.tab_rects.iter().enumerate() {
                assert!(
                    r.x >= g.panel.x && r.x + r.w <= g.panel.x + g.panel.w + 0.5,
                    "tab_rect[{i}] x out of panel"
                );
                assert!(r.y > g.panel.y, "tab_rect[{i}] not below panel top");
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
    fn exactly_five_chips_in_presets_order() {
        // The THEME cards live on tab 0.
        let pv = panel_tab(1920, 1080, 0);
        assert_eq!(pv.geom.chips.len(), 5, "expected 5 theme chips");
        let panel = &pv.geom.panel;
        for (i, chip) in pv.geom.chips.iter().enumerate() {
            assert!(
                chip.x >= panel.x && chip.x + chip.w <= panel.x + panel.w + 0.5,
                "chip[{i}] x out of panel"
            );
            assert!(
                chip.y >= panel.y && chip.y + chip.h <= panel.y + panel.h + 0.5,
                "chip[{i}] y out of panel"
            );
        }
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
    fn each_tab_bands_stack_without_overlap_and_fit() {
        // For each tab, the visible bands run strictly top-down with no overlap and
        // every band fits within the panel rect. We pick one representative rect per
        // band (for side-by-side controls, the left/primary one) in layout order.
        let representatives: [Vec<fn(&PanelGeom) -> Rect>; 5] = [
            // Tab 0 "Look": opacity track, radius track, first card, last card.
            vec![
                |g| g.slider_track,
                |g| g.radius_track,
                |g| g.chips[0],
                |g| g.chips[4],
            ],
            // Tab 1 "Fonts": font-size button, font scroll, last font row,
            //                ui-size button, ui scroll, last ui row.
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
            // Tab 4 "Effects": CRT-section bands (0–9) fit within the 560 px panel
            // at 1920×1080 (py=260, content_top=360, panel bottom=820).
            // Caret bands (11–14) overflow past 820 px — that is expected and
            // handled by the scroll task (Task 6). Only the CRT bands are listed
            // here so the "fits within panel" assertion holds.
            vec![
                |g: &PanelGeom| g.crt_enabled_toggle,   // band 1
                |g: &PanelGeom| g.crt_curvature_track,  // band 2
                |g: &PanelGeom| g.crt_scanline_track,   // band 3
                |g: &PanelGeom| g.crt_mask_track,       // band 4
                |g: &PanelGeom| g.crt_bloom_track,      // band 5
                |g: &PanelGeom| g.crt_chromatic_track,  // band 6
                |g: &PanelGeom| g.crt_vignette_track,   // band 7
                |g: &PanelGeom| g.crt_tint_r_track,     // band 8 (RGB triple – leftmost)
                |g: &PanelGeom| g.crt_roll_toggle,      // band 9 (leftmost pill)
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
    fn theme_cards_form_2col_grid() {
        let pv = panel_tab(1920, 1080, 0);
        let chips = &pv.geom.chips;
        assert_eq!(chips.len(), 5);

        let col0_x = chips[0].x;
        assert!((chips[2].x - col0_x).abs() < 0.5, "chip[2].x != chip[0].x");
        assert!((chips[4].x - col0_x).abs() < 0.5, "chip[4].x != chip[0].x");

        let col1_x = chips[1].x;
        assert!(col1_x > col0_x, "col1 not to the right of col0");
        assert!((chips[3].x - col1_x).abs() < 0.5, "chip[3].x != chip[1].x");

        let row0_bottom = chips[0].y + chips[0].h;
        let row1_top = chips[2].y;
        assert!(row1_top >= row0_bottom - 0.5, "row1 overlaps row0");
        let row1_bottom = chips[2].y + chips[2].h;
        let row2_top = chips[4].y;
        assert!(row2_top >= row1_bottom - 0.5, "row2 overlaps row1");
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
        // height on tab 2. Each must appear as a separate right-aligned label.
        let t0: Vec<String> = panel_tab(1920, 1080, 0).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t0.iter().any(|s| s == "97%"), "missing opacity value label");
        assert!(t0.iter().any(|s| s == "8px"), "missing corner radius value label");

        let t1: Vec<String> = panel_tab(1920, 1080, 1).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t1.iter().any(|s| s == "15pt"), "missing font size value label");

        let t2: Vec<String> = panel_tab(1920, 1080, 2).labels.iter().map(|l| l.0.clone()).collect();
        assert!(t2.iter().any(|s| s == "50%"), "missing dropdown height value label");
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
        // The specimen baseline is below the UI-font-size buttons and above the list.
        let (sx, sy) = pv.ui_specimen_pos;
        assert!(sx >= panel.x && sx <= panel.x + panel.w, "specimen x out of panel");
        assert!(
            sy > g.ui_font_minus.y + g.ui_font_minus.h && sy < g.ui_font_rows[0].y,
            "specimen baseline ({sy}) not between the size buttons and the family list"
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
