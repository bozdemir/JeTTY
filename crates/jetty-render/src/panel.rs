//! The Settings panel — a themed sheet drawn into the Settings window.
//!
//! * **Chrome** (never scrolls): the title row, the tab strip, the footer (a
//!   hint + "Reset tab") and the slim scrollbar.
//! * **Content** (the active tab; every tab scrolls): a vertical stack of
//!   [`PanelItem`]s — collapsible section headers (optionally carrying the
//!   section's master switch), data-driven control rows ([`CtlRow`]: slider,
//!   toggle, cycler, stepper, RGB, chips, list) and two custom items (the theme
//!   gallery and the UI-font "Aa" specimen). The app builds the items from its
//!   control descriptors (jetty-app `settings_ui.rs`); this module only lays
//!   them out, draws them and reports where every interactive part landed.
//! * **Hit testing** is generic: every interactive part is a `(Rect, PanelHit)`
//!   pair in [`PanelGeom`], so a new control needs no geometry field, no
//!   hit-test branch and no mouse action of its own — the app's descriptor
//!   table is the only edit site.
//!
//! Layout runs in a LOGICAL space (design px at the 16pt UI font on a 1×
//! display) and is scaled by the chrome unit on the way out, so text and
//! controls stay proportional at any DPI and UI font size. The content is laid
//! out once in content space, measured, the scroll clamped to it, and only then
//! shifted into the viewport — the drawn rects and the hit rects are the SAME
//! rects, so they can never disagree.
//!
//! Every color comes from [`UiPalette`] (contrast floors on dark AND light
//! themes); decorative fills use its `shade` blends.

use std::borrow::Cow;

use crate::chrome::{fit_head, ChromeMeasure, ChromeMetrics};
use crate::ui_palette::{mix, UiPalette};
use crate::Rect;

/// A text label: `(text, x, y, rgb)` with `(x, y)` the top-left of its line box.
pub type Label = (String, f32, f32, [u8; 3]);

/// Stable id of a Settings control — its config key path (`"opacity"`,
/// `"effects.crt_bloom"`). Also names palette deep links.
pub type CtlId = &'static str;

/// Which part of a control a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlPart {
    /// A slider's track (press = jump there and start dragging).
    Track,
    /// A toggle's switch.
    Switch,
    /// A cycler's "<" segment.
    Prev,
    /// A cycler's ">" segment.
    Next,
    /// A stepper's "-" segment.
    Minus,
    /// A stepper's "+" segment.
    Plus,
    /// A stepper's "Reset" button.
    Reset,
    /// One mini slider of an RGB row (0 = R, 1 = G, 2 = B).
    Channel(u8),
    /// One chip of a chips row.
    Chip(u8),
    /// A list row, by ABSOLUTE item index (scroll offset included).
    Row(usize),
    /// A list's "^" scroll button.
    ScrollUp,
    /// A list's "v" scroll button.
    ScrollDown,
}

/// What a control row shows — its kind with the current value baked in. The
/// app computes it from the control's descriptor and the live config.
#[derive(Clone, Debug, PartialEq)]
pub enum CtlShow {
    /// Full-width slider: knob position (0..=1) and the right-aligned readout.
    Slider { frac: f32, text: String },
    /// A knob-in-track switch.
    Toggle(bool),
    /// `< value >` — the displayed value.
    Cycler(String),
    /// `- value +` plus a Reset button — the displayed value.
    Stepper(String),
    /// Three 0..=1 mini sliders (R, G, B).
    Rgb([f32; 3]),
    /// A row of independent on/off chips: `(label, on)`.
    Chips(Vec<(String, bool)>),
    /// A wrapping row of choice chips (`(label, lit)`, e.g. effect presets)
    /// under the label, with a status readout right of the label ("Custom").
    ChipFlow { chips: Vec<(String, bool)>, status: String },
    /// A scrolling list: the VISIBLE items (`items[i]` is item `offset + i`),
    /// the total item count, the selected item (absolute index) and how many
    /// rows the list shows at once.
    List { items: Vec<String>, offset: usize, total: usize, selected: Option<usize>, rows: usize },
}

/// How a row reads and reacts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RowState {
    #[default]
    Normal,
    /// Faded but live (e.g. the corner radius while fullscreen, or a row under
    /// a section whose master switch is off).
    Dimmed,
    /// Faded and inert: no hit rects are emitted.
    Disabled,
}

/// One data-driven control row.
#[derive(Clone, Debug, PartialEq)]
pub struct CtlRow {
    pub id: CtlId,
    pub label: String,
    pub show: CtlShow,
    pub state: RowState,
    /// A helper line drawn under the control (e.g. "Applies to new tabs").
    pub hint: Option<String>,
}

/// One entry of a tab's content stack, top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelItem {
    /// A collapsible section header. `master` is the section's on/off switch,
    /// drawn in the header (`(control id, on)`).
    Section {
        id: &'static str,
        title: String,
        master: Option<(CtlId, bool)>,
        collapsed: bool,
        hint: Option<String>,
    },
    Row(CtlRow),
    /// The theme gallery (filter chips + cards); reads `PanelInput::theme_idx`,
    /// `filter` and `hover`.
    Gallery,
    /// The live "Aa" UI-font specimen (drawn by the app at the TRUE UI size).
    Specimen,
}

/// The theme gallery's filter chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeFilter {
    #[default]
    All,
    Dark,
    Light,
    /// Themes loaded from the user's `themes/` folder.
    Mine,
}

impl ThemeFilter {
    /// The chips, in display order.
    pub const ALL: [ThemeFilter; 4] = [ThemeFilter::All, ThemeFilter::Dark, ThemeFilter::Light, ThemeFilter::Mine];

    pub fn label(self) -> &'static str {
        match self {
            ThemeFilter::All => "All",
            ThemeFilter::Dark => "Dark",
            ThemeFilter::Light => "Light",
            ThemeFilter::Mine => "Mine",
        }
    }

    /// Whether `t` passes this filter.
    pub fn matches(self, t: &jetty_core::Theme) -> bool {
        match self {
            ThemeFilter::All => true,
            ThemeFilter::Dark => !theme_is_light(t),
            ThemeFilter::Light => theme_is_light(t),
            ThemeFilter::Mine => is_user_theme(t),
        }
    }
}

/// A light theme: its background is brighter than its foreground — the same
/// definition as [`UiPalette::is_light`], computed without the palette memo
/// (the gallery classifies every theme each frame).
pub fn theme_is_light(t: &jetty_core::Theme) -> bool {
    let l = crate::colors::relative_luminance;
    l([t.bg[0], t.bg[1], t.bg[2]]) > l(t.fg)
}

/// A theme loaded from the user's `themes/` folder. Built-ins carry `'static`
/// (borrowed) names; user themes own theirs (jetty-app `themes.rs`), so the
/// ownership of the id is the provenance — including a user theme that
/// shadows a built-in under the same name.
pub fn is_user_theme(t: &jetty_core::Theme) -> bool {
    matches!(t.name, Cow::Owned(_))
}

/// Registry indices of the themes `filter` shows, in registry order — the
/// gallery's card order and the keyboard-navigation order.
pub fn gallery_order(filter: ThemeFilter) -> Vec<usize> {
    (0..jetty_core::theme_count()).filter(|&i| filter.matches(&jetty_core::theme_at(i))).collect()
}

/// Columns of the theme gallery (Up/Down move by this many cards).
pub const GALLERY_COLS: usize = 3;

/// What an interactive part of the panel does when pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelHit {
    /// A settings tab label.
    Tab(usize),
    /// A part of a data-driven control.
    Ctl { id: CtlId, part: CtlPart },
    /// A section header (collapse / expand).
    Section(&'static str),
    /// A theme card (registry index).
    GalleryCard(usize),
    /// A gallery filter chip.
    GalleryFilter(ThemeFilter),
    /// The scrollbar thumb.
    ScrollThumb,
    /// The scrollbar track outside the thumb (page jump).
    ScrollTrack,
    /// The footer's "Reset tab" button.
    ResetTab,
}

/// The footer button's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ResetState {
    /// Every resettable control on the tab is at its default: dimmed, inert.
    #[default]
    Disabled,
    Ready,
    /// Clicked once: the next click resets (a guard against a stray click).
    Armed,
}

/// Everything `build_panel` needs for one frame.
pub struct PanelInput<'a> {
    /// The Settings window's surface, physical px.
    pub screen_w: u32,
    pub screen_h: u32,
    pub theme: &'a jetty_core::Theme,
    /// The Settings window's chrome metrics (its DPI × the capped panel text
    /// size): the panel's layout scale.
    pub cm: ChromeMetrics,
    /// 0..N_TABS — which tab's content is laid out (clamped).
    pub active_tab: usize,
    /// The active tab's scroll offset, physical px (clamped here; the clamped
    /// value is reported back in `PanelGeom::scroll`).
    pub scroll: f32,
    /// The active tab's content, top to bottom.
    pub items: &'a [PanelItem],
    /// The shown theme (registry index) — the gallery's selected card.
    pub theme_idx: usize,
    pub filter: ThemeFilter,
    /// The part under the mouse (gallery cards, filter chips, section headers,
    /// the footer button and the scroll thumb highlight).
    pub hover: Option<PanelHit>,
    /// A deep-linked or keyboard-focused control or section id: its row band
    /// is highlighted.
    pub focus: Option<&'static str>,
    /// The part the keyboard focus is on (a switch, a chip, a slider's
    /// `Track` — ringed at its knob —, a list's selected `Row`, a gallery
    /// card): ringed in the accent.
    pub focus_part: Option<PanelHit>,
    /// The TRUE UI font size (logical pt) — sizes the specimen line.
    pub ui_font_size: f32,
    pub reset: ResetState,
    /// The footer hint; empty = "Esc to close".
    pub footer_hint: &'a str,
    /// The scrollbar thumb is being dragged (drawn emphasized).
    pub scroll_dragging: bool,
}

impl<'a> PanelInput<'a> {
    /// A frame with defaults for everything but the surface, theme, metrics
    /// and content.
    pub fn new(
        screen_w: u32,
        screen_h: u32,
        theme: &'a jetty_core::Theme,
        cm: ChromeMetrics,
        items: &'a [PanelItem],
    ) -> Self {
        PanelInput {
            screen_w,
            screen_h,
            theme,
            cm,
            active_tab: 0,
            scroll: 0.0,
            items,
            theme_idx: 0,
            filter: ThemeFilter::All,
            hover: None,
            focus: None,
            focus_part: None,
            ui_font_size: 16.0,
            reset: ResetState::Ready,
            footer_hint: "",
            scroll_dragging: false,
        }
    }
}

/// Where every interactive part landed (physical px).
#[derive(Clone, Default)]
pub struct PanelGeom {
    /// The content column (the whole surface height).
    pub panel: Rect,
    /// The title row.
    pub title_bar: Rect,
    /// The tab-strip cells, in `TAB_NAMES` order.
    pub tab_rects: [Rect; N_TABS],
    /// Chrome parts (never scrolled): the footer button, the scrollbar.
    pub chrome_hits: Vec<(Rect, PanelHit)>,
    /// Content parts, scrolled into place. Live only inside
    /// `[content_top, content_bottom)` — a part scrolled under the chrome is
    /// clipped from view and from the mouse alike.
    pub hits: Vec<(Rect, PanelHit)>,
    pub content_top: f32,
    pub content_bottom: f32,
    /// The scroll actually applied (the input clamped to `[0, max_scroll]`).
    pub scroll: f32,
    pub max_scroll: f32,
    /// The scrollbar thumb, when the content overflows.
    pub scroll_thumb: Option<Rect>,
    /// `(id, top, bottom)` of every section and control row in CONTENT space
    /// (unscrolled: 0 = the first item's top) — for deep links and
    /// scroll-into-view.
    pub anchors: Vec<(&'static str, f32, f32)>,
}

fn contains(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
}

impl PanelGeom {
    /// The part at `(x, y)`: tabs, then chrome, then content (inside the
    /// viewport only), in each list's priority order.
    pub fn hit_at(&self, x: f32, y: f32) -> Option<PanelHit> {
        if let Some(i) = self.tab_rects.iter().position(|r| contains(r, x, y)) {
            return Some(PanelHit::Tab(i));
        }
        if let Some((_, h)) = self.chrome_hits.iter().find(|(r, _)| contains(r, x, y)) {
            return Some(*h);
        }
        if y >= self.content_top && y < self.content_bottom {
            if let Some((_, h)) = self.hits.iter().find(|(r, _)| contains(r, x, y)) {
                return Some(*h);
            }
        }
        None
    }

    /// The rect of `hit` (chrome first, then content).
    pub fn rect_of(&self, hit: PanelHit) -> Option<Rect> {
        if let PanelHit::Tab(i) = hit {
            return self.tab_rects.get(i).copied();
        }
        self.chrome_hits.iter().chain(self.hits.iter()).find(|(_, h)| *h == hit).map(|(r, _)| *r)
    }

    /// `(top, bottom)` of section / control `id` in content space.
    pub fn anchor(&self, id: &str) -> Option<(f32, f32)> {
        self.anchors.iter().find(|a| a.0 == id).map(|a| (a.1, a.2))
    }

    /// The visible content height.
    pub fn viewport_h(&self) -> f32 {
        (self.content_bottom - self.content_top).max(0.0)
    }

    /// The scroll that shows the content-space span `[top, bottom]` with a
    /// small margin, moving as little as possible (unchanged when it is
    /// already in view). Clamped to `[0, max_scroll]`.
    pub fn scroll_to_reveal(&self, top: f32, bottom: f32, margin: f32) -> f32 {
        let vh = self.viewport_h();
        let mut s = self.scroll;
        if top - margin < s {
            s = top - margin;
        } else if bottom + margin > s + vh {
            s = (bottom + margin - vh).min(top - margin);
        }
        s.clamp(0.0, self.max_scroll)
    }
}

/// Full description of how to draw the panel for one frame.
pub struct PanelView {
    /// Chrome quads, drawn unclipped: the backdrop, title row, tab strip,
    /// footer and scrollbar.
    pub quads: Vec<Rect>,
    /// Chrome labels, drawn unclipped.
    pub labels: Vec<Label>,
    /// The active tab's content quads — draw with a hardware scissor of
    /// `content_viewport`.
    pub content_quads: Vec<Rect>,
    /// The active tab's content labels — draw clipped to `content_viewport`.
    pub content_labels: Vec<Label>,
    /// `[x, y, w, h]` (physical px, inside the surface) of the content
    /// viewport; `None` when the surface is too small to show any content.
    pub content_viewport: Option<[u32; 4]>,
    pub geom: PanelGeom,
    /// Top-left of the live "Aa" UI-font specimen (physical px), or far
    /// offscreen when there is none on this tab or it is not fully in view.
    pub ui_specimen_pos: (f32, f32),
    /// The specimen's color (the accent).
    pub specimen_rgb: [u8; 3],
    /// The panel surface: the window's clear color.
    pub surface: [u8; 3],
}

/// The tab labels, in order. Adding a tab = one name here (the strip, its hit
/// rects and the app's per-tab state are all sized from it).
pub const TAB_NAMES: [&str; 5] = ["Look", "Fonts", "Window", "Shell", "Effects"];
/// Number of settings tabs.
pub const N_TABS: usize = TAB_NAMES.len();

/// Reference chrome advance at the design size (16pt, 1×) — the monospace
/// estimate the panel's label budgets were authored against, and the advance
/// the unit tests measure with. The live panel MEASURES its labels and scales
/// by the chrome unit, never by this.
pub const CHAR_W_FALLBACK: f32 = 9.8;

/// The panel's design size in logical px: the Settings window opens at this
/// size (+ a 2-px margin each side) scaled by the chrome unit. The panel then
/// FILLS whatever surface it gets — a taller window shows more content, a
/// wider one a wider column (up to `PANEL_MAX_W`), centered.
pub const PANEL_W: f32 = 420.0;
pub const PANEL_H: f32 = 592.0;
/// The content column never grows wider than this (sliders spanning a
/// maximized window would be absurd); the surface around it is filled.
const PANEL_MAX_W: f32 = 560.0;

/// Horizontal padding between the column edge and the content.
const PAD: f32 = 20.0;
/// Title row (44) + tab strip (32) + breathing room.
const CONTENT_TOP_OFFSET: f32 = 96.0;
/// The footer band (hairline + hint + "Reset tab").
const FOOTER_H: f32 = 40.0;
/// Space above the first item and below the last.
const CONTENT_PAD: f32 = 2.0;

/// Shared control metrics: every button, cycler and stepper is CTL_H tall
/// with the same corner radius, so the whole sheet reads as one system.
const CTL_H: f32 = 28.0;
const R_CTL: f32 = 6.0;
/// Label top inside a CTL_H control (optically centered for the UI fonts).
const LABEL_DY: f32 = 6.0;
/// Toggle switch.
const SW_W: f32 = 44.0;
const SW_H: f32 = 24.0;
/// Cycler `< value >`.
const CYC_W: f32 = 210.0;
const CYC_SEG: f32 = 32.0;
/// Stepper `- value +` and its Reset button. The stepper grows past STEP_W
/// when its value needs it, keeping VAL_PAD clear of each separator.
const STEP_W: f32 = 116.0;
const STEP_SEG: f32 = 36.0;
const RESET_W: f32 = 64.0;
const VAL_PAD: f32 = 8.0;
/// A cycler, stepper or chip row whose control would leave its label less
/// than this (or the label's own width, if shorter) drops the control onto
/// a line of its own below the label — a narrow Settings window — adding
/// STACK_DY to the row.
const LABEL_MIN_W: f32 = 96.0;
const STACK_DY: f32 = 24.0;
/// The tab strip's narrowest padding (each side of a name) before the
/// longest names are ellipsized.
const TAB_PAD_MIN: f32 = 3.0;
/// Chips.
const CHIP_W_MIN: f32 = 72.0;
const CHIP_H: f32 = 24.0;
/// Chip-flow rows: the narrowest chip, and the gap between chips and lines.
const CHIP_FLOW_MIN_W: f32 = 56.0;
const CHIP_FLOW_GAP: f32 = 8.0;
/// RGB rows: the gap between the three channel sliders, the room for each
/// channel letter, and the color swatch beside the label.
const MINI_GAP: f32 = 16.0;
const RGB_LETTER_W: f32 = 16.0;
const RGB_SWATCH_W: f32 = 30.0;
/// List rows inside their inset card.
const LIST_ROW_H: f32 = 24.0;
const LIST_ROW_GAP: f32 = 2.0;
const LIST_PAD: f32 = 5.0;
/// A list's row pitch (logical px; × the panel's `overlay_u` for physical):
/// how far a list scrolls per row, so touchpad travel maps to rows.
pub const LIST_ROW_PITCH: f32 = LIST_ROW_H + LIST_ROW_GAP;

/// Row pitches: single-line controls, sliders, RGB rows; a hint line adds
/// HINT_H. Sections: header height and the gap above every header but the
/// first.
const ROW_PITCH: f32 = 46.0;
const SLIDER_PITCH: f32 = 54.0;
const RGB_PITCH: f32 = 50.0;
const HINT_H: f32 = 20.0;
const SECTION_H: f32 = 28.0;
const SECTION_GAP: f32 = 16.0;

/// Theme gallery: filter-chip row, card size and grid gaps.
const GAL_CHIPS_H: f32 = 38.0;
const GAL_CARD_H: f32 = 72.0;
const GAL_GAP_X: f32 = 16.0;
const GAL_CAPTION_H: f32 = 24.0;
/// The pitch of a wrapped caption's second line.
const GAL_CAPTION_LINE: f32 = 20.0;
const GAL_GAP_Y: f32 = 8.0;

/// Far offscreen (an absent specimen).
const OFF: f32 = 1.0e6;

/// The panel's colors for one frame, all from [`UiPalette`].
#[derive(Clone, Copy)]
struct Colors {
    ui: UiPalette,
    surface: [u8; 4],
    ctl: [u8; 4],
    ctl_hi: [u8; 4],
    seg: [u8; 4],
    track: [u8; 4],
    well: [u8; 4],
    hair: [u8; 4],
    knob_off: [u8; 4],
    knob_on: [u8; 4],
    accent: [u8; 4],
    accent_fill: [u8; 4],
    row_sel: [u8; 4],
    focus: [u8; 4],
}

fn rgba(c: [u8; 3], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

impl Colors {
    fn new(theme: &jetty_core::Theme) -> Colors {
        let ui = UiPalette::cached(theme);
        // Decorative fills are bg→fg blends. Luminance contrast is not linear in
        // sRGB: the same blend that reads on a dark theme all but vanishes on a
        // light one (control bodies, off-switch tracks, hairlines), so light
        // themes blend further. Dark themes keep the historical weights.
        let k = if ui.is_light { 1.7 } else { 1.0 };
        let s = |t: f32| rgba(ui.shade(t * k), 255);
        Colors {
            ui,
            surface: rgba(ui.surface, 255),
            ctl: s(0.10),
            ctl_hi: s(0.16),
            seg: s(0.22),
            track: s(0.16),
            well: s(0.02),
            hair: s(0.14),
            knob_off: rgba(ui.shade(0.55), 255),
            // A knob is a shape, not text: the panel surface reaches the 3:1
            // non-text floor on the accent track by UiPalette's construction
            // (accent ≥ 3:1 on surface) — light on light themes, dark on dark.
            knob_on: rgba(ui.surface, 255),
            accent: rgba(ui.accent, 255),
            accent_fill: rgba(ui.accent, 200),
            row_sel: rgba(mix(ui.surface, ui.accent, 0.25), 255),
            focus: rgba(mix(ui.surface, ui.accent, 0.16), 255),
        }
    }

    /// A row label: dim, or a hint shade when the row is dimmed / disabled.
    fn label(&self, state: RowState) -> [u8; 3] {
        if state == RowState::Normal { self.ui.text_dim } else { self.ui.text_hint }
    }

    /// A row's value readout.
    fn value(&self, state: RowState) -> [u8; 3] {
        if state == RowState::Normal { self.ui.text } else { self.ui.text_hint }
    }
}

/// Fade a quad for a dimmed / disabled row.
fn faded(mut r: Rect, state: RowState) -> Rect {
    if state != RowState::Normal {
        r.color[3] = (r.color[3] as f32 * 0.4).round() as u8;
    }
    r
}

/// An invisible hit rect.
fn area(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x, y, w, h, [0, 0, 0, 0])
}

/// A part's visible outline (a focus ring's shape): position, size, radius.
fn shape(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Rect {
    Rect::rounded(x, y, w, h, [0, 0, 0, 0], radius)
}

/// What the content builder collects (content space: y = 0 at the top).
#[derive(Default)]
struct Out {
    quads: Vec<Rect>,
    labels: Vec<Label>,
    hits: Vec<(Rect, PanelHit)>,
    anchors: Vec<(&'static str, f32, f32)>,
    /// `(x, top, line height)` of the specimen.
    specimen: Option<(f32, f32, f32)>,
}

/// The content builder: measures with the panel's text layer, lays items out
/// top-down in content space.
struct Lay<'m> {
    m: &'m mut dyn ChromeMeasure,
    /// Layout scale (physical px per logical px).
    u: f32,
    /// Content left edge and width.
    x0: f32,
    cw: f32,
    c: Colors,
    hover: Option<PanelHit>,
    focus: Option<&'static str>,
    /// The keyboard-focused part (ringed).
    ring: Option<PanelHit>,
    /// The specimen's line height in logical px.
    spec_line: f32,
    theme_idx: usize,
    filter: ThemeFilter,
    out: Out,
}

impl Lay<'_> {
    /// Rendered width of `s` in logical px.
    fn tw(&mut self, s: &str) -> f32 {
        self.m.text_w(s) / self.u
    }

    /// `s` truncated (with an ellipsis) to `w` logical px.
    fn fit(&mut self, s: &str, w: f32) -> String {
        fit_head(self.m, s, (w * self.u).max(0.0), false)
    }

    fn quad(&mut self, r: Rect) {
        self.out.quads.push(r);
    }

    fn label(&mut self, s: String, x: f32, y: f32, col: [u8; 3]) {
        if !s.is_empty() {
            self.out.labels.push((s, x, y, col));
        }
    }

    fn hit(&mut self, r: Rect, h: PanelHit) {
        self.out.hits.push((r, h));
    }

    /// The keyboard-focus ring around part `hit` at `r` (its visible shape,
    /// radius included), when `hit` is the focused part: an accent outline
    /// with a gap in `gap`, the color behind the part — so it reads around an
    /// accent-filled part too. Draw it BEFORE the part, which covers the
    /// middle.
    fn ring(&mut self, hit: PanelHit, r: Rect, gap: [u8; 4]) {
        if self.ring != Some(hit) {
            return;
        }
        let accent = self.c.accent;
        self.quad(Rect::rounded(r.x - 4.0, r.y - 4.0, r.w + 8.0, r.h + 8.0, accent, r.radius + 4.0));
        self.quad(Rect::rounded(r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0, gap, r.radius + 2.0));
    }

    /// The color behind the parts of row / section `id`: its focus band, or
    /// the surface.
    fn under(&self, id: &str) -> [u8; 4] {
        if self.focus == Some(id) { self.c.focus } else { self.c.surface }
    }

    fn right(&self) -> f32 {
        self.x0 + self.cw
    }

    /// Lay `item` out at `y`; returns the height it took.
    fn item(&mut self, item: &PanelItem, y: f32, first: bool) -> f32 {
        match item {
            PanelItem::Section { id, title, master, collapsed, hint } => {
                self.section(y, first, id, title, *master, *collapsed, hint.as_deref())
            }
            PanelItem::Row(row) => self.row(y, row),
            PanelItem::Gallery => self.gallery(y),
            PanelItem::Specimen => {
                let h = self.spec_line + 8.0;
                self.out.specimen = Some((self.x0, y + 2.0, self.spec_line));
                h
            }
        }
    }

    /// A small solid triangle built from 1-px bars (the quad shader draws
    /// rects only): pointing down when expanded, right when collapsed.
    fn chevron(&mut self, x: f32, y: f32, collapsed: bool, col: [u8; 3]) {
        let c = rgba(col, 255);
        for k in 0..5 {
            let k = k as f32;
            let r = if collapsed {
                Rect::new(x + 2.0 + k, y - 1.0 + k, 1.0, 9.0 - 2.0 * k, c)
            } else {
                Rect::new(x + k, y + 1.0 + k, 9.0 - 2.0 * k, 1.0, c)
            };
            self.quad(r);
        }
    }

    /// A knob-in-track switch at `(x, y)`. Returns the track (the hit target).
    fn switch(&mut self, x: f32, y: f32, on: bool, state: RowState) -> Rect {
        let c = self.c;
        let track = Rect::rounded(x, y, SW_W, SW_H, if on { c.accent } else { c.track }, SW_H / 2.0);
        let (kx, kc) = if on { (x + SW_W - 21.0, c.knob_on) } else { (x + 3.0, c.knob_off) };
        self.quad(faded(track, state));
        self.quad(faded(Rect::rounded(kx, y + 3.0, 18.0, 18.0, kc, 9.0), state));
        track
    }

    #[allow(clippy::too_many_arguments)]
    fn section(
        &mut self,
        y: f32,
        first: bool,
        id: &'static str,
        title: &str,
        master: Option<(CtlId, bool)>,
        collapsed: bool,
        hint: Option<&str>,
    ) -> f32 {
        let c = self.c;
        let (x0, cw) = (self.x0, self.cw);
        let top = if first { 0.0 } else { SECTION_GAP };
        let hy = y + top;
        let sw_room = if master.is_some() { SW_W + 12.0 } else { 0.0 };
        if self.focus == Some(id) {
            self.quad(Rect::rounded(x0 - 8.0, hy - 3.0, cw + 16.0, SECTION_H + 6.0, c.focus, 8.0));
        } else if self.hover == Some(PanelHit::Section(id)) {
            self.quad(Rect::rounded(x0 - 8.0, hy - 3.0, cw + 16.0 - sw_room, SECTION_H + 6.0, c.ctl, 8.0));
        }
        self.chevron(x0, hy + 9.0, collapsed, c.ui.text_dim);
        let tx = x0 + 16.0;
        let t = self.fit(title, cw - 16.0 - sw_room - 24.0);
        let t_w = self.tw(&t);
        self.label(t, tx, hy + 4.0, c.ui.text);
        let rx = tx + t_w + 12.0;
        let rend = x0 + cw - sw_room;
        if rend - rx > 8.0 {
            self.quad(Rect::new(rx, hy + 14.0, rend - rx, 1.0, c.hair));
        }
        if let Some((mid, on)) = master {
            let hit = PanelHit::Ctl { id: mid, part: CtlPart::Switch };
            let sx = x0 + cw - SW_W;
            self.ring(hit, shape(sx, hy + 2.0, SW_W, SW_H, SW_H / 2.0), self.under(id));
            let track = self.switch(sx, hy + 2.0, on, RowState::Normal);
            self.hit(track, hit);
        }
        // After the switch, so the switch wins where they meet.
        self.hit(area(x0 - 8.0, hy - 3.0, cw + 16.0 - sw_room, SECTION_H + 6.0), PanelHit::Section(id));
        let mut h = top + SECTION_H;
        if let Some(hint) = hint.filter(|_| !collapsed) {
            let s = self.fit(hint, cw - 16.0);
            self.label(s, tx, hy + SECTION_H - 2.0, c.ui.text_hint);
            h += HINT_H;
        }
        h += 6.0;
        self.out.anchors.push((id, hy, y + h));
        h
    }

    fn row(&mut self, y: f32, row: &CtlRow) -> f32 {
        let stacked = self.stacks(row);
        let dy = if stacked { STACK_DY } else { 0.0 };
        let h = match &row.show {
            CtlShow::Slider { .. } => SLIDER_PITCH,
            CtlShow::Rgb(_) => RGB_PITCH,
            CtlShow::List { rows, .. } => 24.0 + list_card_h(*rows) + 16.0,
            CtlShow::ChipFlow { chips, .. } => {
                let lines = self.chip_flow_lines(chips).len().max(1) as f32;
                24.0 + lines * (CHIP_H + CHIP_FLOW_GAP) + 8.0
            }
            _ => ROW_PITCH + dy,
        } + if row.hint.is_some() { HINT_H } else { 0.0 };
        if self.focus == Some(row.id) {
            let fc = self.c.focus;
            self.quad(Rect::rounded(self.x0 - 8.0, y - 6.0, self.cw + 16.0, h - 4.0, fc, 8.0));
        }
        match &row.show {
            CtlShow::Slider { frac, text } => self.slider(y, row, *frac, text),
            CtlShow::Toggle(on) => self.toggle(y, row, *on),
            CtlShow::Cycler(v) => self.cycler(y, row, v, stacked),
            CtlShow::Stepper(v) => self.stepper(y, row, v, stacked),
            CtlShow::Rgb(v) => self.rgb(y, row, *v),
            CtlShow::Chips(chips) => self.chips(y, row, chips, stacked),
            CtlShow::ChipFlow { chips, status } => self.chip_flow(y, row, chips, status),
            CtlShow::List { items, offset, total, selected, rows } => {
                self.list(y, row, items, *offset, *total, *selected, *rows)
            }
        }
        if let Some(hint) = &row.hint {
            let hy = match &row.show {
                CtlShow::Slider { .. } => y + 44.0,
                CtlShow::Rgb(_) => y + 42.0,
                CtlShow::List { rows, .. } => y + 24.0 + list_card_h(*rows) + 4.0,
                CtlShow::ChipFlow { chips, .. } => {
                    let lines = self.chip_flow_lines(chips).len().max(1) as f32;
                    y + 24.0 + lines * (CHIP_H + CHIP_FLOW_GAP) + 2.0
                }
                _ => y + dy + CTL_H + 5.0,
            };
            let s = self.fit(hint, self.cw);
            let col = self.c.ui.text_hint;
            self.label(s, self.x0, hy, col);
        }
        self.out.anchors.push((row.id, y, y + h));
        h
    }

    /// A row label left of a control that starts at `ctl_x`.
    fn row_label(&mut self, row: &CtlRow, y: f32, ctl_x: f32) {
        let s = self.fit(&row.label, ctl_x - 12.0 - self.x0);
        let col = self.c.label(row.state);
        self.label(s, self.x0, y, col);
    }

    /// A row label on a line of its own (a stacked row): the full width.
    fn row_label_above(&mut self, row: &CtlRow, y: f32) {
        let ctl_x = self.right() + 12.0;
        self.row_label(row, y, ctl_x);
    }

    /// Whether `row`'s control drops onto a line below its label: beside it,
    /// the label would keep less than `LABEL_MIN_W` (or its own width, when
    /// that is shorter) — only in a narrow window. Sliders, RGB rows, chip
    /// flows and lists always put their control under the label; a toggle
    /// always fits beside it.
    fn stacks(&mut self, row: &CtlRow) -> bool {
        let ctl_w = match &row.show {
            CtlShow::Cycler(_) => CYC_W,
            CtlShow::Stepper(v) => RESET_W + 8.0 + self.stepper_w(v),
            CtlShow::Chips(chips) => self.chips_w(chips).1,
            _ => return false,
        };
        let need = self.tw(&row.label).min(LABEL_MIN_W);
        self.cw - ctl_w - 12.0 < need
    }

    /// A stepper wide enough for `value` with VAL_PAD clear of each separator
    /// (never narrower than STEP_W).
    fn stepper_w(&mut self, value: &str) -> f32 {
        (self.tw(value) + 2.0 * (STEP_SEG + VAL_PAD)).max(STEP_W)
    }

    /// A chip row's chip width (every chip as wide as the widest label
    /// needs) and the row's total width.
    fn chips_w(&mut self, chips: &[(String, bool)]) -> (f32, f32) {
        let n = chips.len().min(8);
        let mut w = CHIP_W_MIN;
        for (t, _) in &chips[..n] {
            w = w.max(self.tw(t) + 16.0);
        }
        (w, n as f32 * w + n.saturating_sub(1) as f32 * 8.0)
    }

    fn live(&self, row: &CtlRow) -> bool {
        row.state != RowState::Disabled
    }

    fn slider(&mut self, y: f32, row: &CtlRow, frac: f32, text: &str) {
        let c = self.c;
        let (x0, cw) = (self.x0, self.cw);
        let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
        let v_w = self.tw(text);
        self.row_label(row, y, x0 + cw - v_w);
        let vc = c.value(row.state);
        self.label(text.to_string(), x0 + cw - v_w, y, vc);
        let st = row.state;
        self.quad(faded(Rect::rounded(x0, y + 30.0, cw, 4.0, c.track, 2.0), st));
        let fill_w = (frac * (cw - 16.0) + 8.0).clamp(4.0, cw);
        self.quad(faded(Rect::rounded(x0, y + 30.0, fill_w, 4.0, c.accent_fill, 2.0), st));
        let kx = x0 + frac * (cw - 16.0);
        let hit = PanelHit::Ctl { id: row.id, part: CtlPart::Track };
        self.ring(hit, shape(kx, y + 24.0, 16.0, 16.0, 8.0), self.under(row.id));
        self.quad(faded(Rect::rounded(kx, y + 24.0, 16.0, 16.0, c.accent, 8.0), st));
        if self.live(row) {
            self.hit(area(x0, y + 20.0, cw, 24.0), hit);
        }
    }

    fn toggle(&mut self, y: f32, row: &CtlRow, on: bool) {
        let x = self.right() - SW_W;
        self.row_label(row, y + LABEL_DY, x);
        let hit = PanelHit::Ctl { id: row.id, part: CtlPart::Switch };
        self.ring(hit, shape(x, y + 2.0, SW_W, SW_H, SW_H / 2.0), self.under(row.id));
        let track = self.switch(x, y + 2.0, on, row.state);
        if self.live(row) {
            self.hit(track, hit);
        }
    }

    /// A segment glyph centered in `[x, x + w]`.
    fn seg_glyph(&mut self, g: &str, x: f32, w: f32, y: f32, col: [u8; 3]) {
        let gx = x + (w - self.tw(g)) * 0.5;
        self.label(g.to_string(), gx, y, col);
    }

    fn seg_line(&mut self, x: f32, y: f32, state: RowState) {
        let col = self.c.seg;
        self.quad(faded(Rect::new(x, y + 7.0, 1.0, CTL_H - 14.0, col), state));
    }

    fn cycler(&mut self, y: f32, row: &CtlRow, value: &str, stacked: bool) {
        let c = self.c;
        let st = row.state;
        let (cyc_w, y) = if stacked {
            // Under its label: the full content width.
            self.row_label_above(row, y);
            (self.cw, y + STACK_DY)
        } else {
            // CYC_W wide, grown to fit a long value (a theme name) as far as
            // the label beside it leaves room — short values keep the common
            // width.
            let label_w = self.tw(&row.label);
            let want = self.tw(value) + 2.0 * CYC_SEG + 16.0;
            let w = want.min(self.cw - label_w - 12.0).max(CYC_W);
            self.row_label(row, y + LABEL_DY, self.right() - w);
            (w, y)
        };
        let x = self.right() - cyc_w;
        let banded = self.under(row.id);
        self.ring(PanelHit::Ctl { id: row.id, part: CtlPart::Next }, shape(x, y, cyc_w, CTL_H, R_CTL), banded);
        self.quad(faded(Rect::rounded(x, y, cyc_w, CTL_H, c.ctl, R_CTL), st));
        self.seg_line(x + CYC_SEG, y, st);
        self.seg_line(x + cyc_w - CYC_SEG, y, st);
        let gap_x = x + CYC_SEG;
        let gap_w = cyc_w - 2.0 * CYC_SEG;
        let shown = self.fit(value, gap_w - 8.0);
        let sx = gap_x + ((gap_w - self.tw(&shown)) * 0.5).max(0.0);
        self.label(shown, sx, y + LABEL_DY, c.value(st));
        let gc = c.label(st);
        self.seg_glyph("<", x, CYC_SEG, y + LABEL_DY, gc);
        self.seg_glyph(">", x + cyc_w - CYC_SEG, CYC_SEG, y + LABEL_DY, gc);
        if self.live(row) {
            self.hit(area(x, y, CYC_SEG, CTL_H), PanelHit::Ctl { id: row.id, part: CtlPart::Prev });
            self.hit(area(x + cyc_w - CYC_SEG, y, CYC_SEG, CTL_H), PanelHit::Ctl { id: row.id, part: CtlPart::Next });
        }
    }

    fn stepper(&mut self, y: f32, row: &CtlRow, value: &str, stacked: bool) {
        let c = self.c;
        let st = row.state;
        let natural = self.stepper_w(value);
        let (step_w, y) = if stacked {
            // Under its label, right-aligned like the one-line row; squeezed
            // (the value ellipsized) only when even the full width is short.
            self.row_label_above(row, y);
            (natural.min(self.cw - RESET_W - 8.0).max(2.0 * STEP_SEG + 8.0), y + STACK_DY)
        } else {
            (natural, y)
        };
        let x = self.right() - step_w;
        let rx = x - 8.0 - RESET_W;
        if !stacked {
            self.row_label(row, y + LABEL_DY, rx);
        }
        let (id, banded) = (row.id, self.under(row.id));
        self.ring(PanelHit::Ctl { id, part: CtlPart::Reset }, shape(rx, y, RESET_W, CTL_H, R_CTL), banded);
        self.quad(faded(Rect::rounded(rx, y, RESET_W, CTL_H, c.ctl, R_CTL), st));
        self.ring(PanelHit::Ctl { id, part: CtlPart::Plus }, shape(x, y, step_w, CTL_H, R_CTL), banded);
        self.quad(faded(Rect::rounded(x, y, step_w, CTL_H, c.ctl, R_CTL), st));
        self.seg_line(x + STEP_SEG, y, st);
        self.seg_line(x + step_w - STEP_SEG, y, st);
        let vw = step_w - 2.0 * STEP_SEG;
        let shown = self.fit(value, vw - VAL_PAD);
        let sx = x + STEP_SEG + ((vw - self.tw(&shown)) * 0.5).max(0.0);
        self.label(shown, sx, y + LABEL_DY, c.value(st));
        let gc = c.label(st);
        self.seg_glyph("-", x, STEP_SEG, y + LABEL_DY, gc);
        self.seg_glyph("+", x + step_w - STEP_SEG, STEP_SEG, y + LABEL_DY, gc);
        let reset = self.fit("Reset", RESET_W - 8.0);
        self.seg_glyph(&reset, rx, RESET_W, y + LABEL_DY, gc);
        if self.live(row) {
            let id = row.id;
            self.hit(area(x, y, STEP_SEG, CTL_H), PanelHit::Ctl { id, part: CtlPart::Minus });
            self.hit(area(x + step_w - STEP_SEG, y, STEP_SEG, CTL_H), PanelHit::Ctl { id, part: CtlPart::Plus });
            self.hit(area(rx, y, RESET_W, CTL_H), PanelHit::Ctl { id, part: CtlPart::Reset });
        }
    }

    /// Line 1: the label (full width) and a swatch of the color; line 2: one
    /// mini slider per channel, each after its letter.
    fn rgb(&mut self, y: f32, row: &CtlRow, v: [f32; 3]) {
        let c = self.c;
        let st = row.state;
        let (x0, cw) = (self.x0, self.cw);
        let ch = |i: usize| if v[i].is_finite() { v[i].clamp(0.0, 1.0) } else { 0.0 };
        let swatch_x = x0 + cw - RGB_SWATCH_W;
        self.row_label(row, y, swatch_x);
        let to8 = |f: f32| (f * 255.0).round() as u8;
        self.quad(faded(Rect::rounded(swatch_x - 1.0, y + 1.0, RGB_SWATCH_W + 2.0, 18.0, c.hair, 5.0), st));
        self.quad(faded(Rect::rounded(swatch_x, y + 2.0, RGB_SWATCH_W, 16.0, [to8(ch(0)), to8(ch(1)), to8(ch(2)), 255], 4.0), st));
        let seg_w = (cw - 2.0 * MINI_GAP) / 3.0;
        let track_w = seg_w - RGB_LETTER_W;
        for (i, name) in ["R", "G", "B"].iter().enumerate() {
            let sx = x0 + i as f32 * (seg_w + MINI_GAP);
            let x = sx + RGB_LETTER_W;
            let f = ch(i);
            let lc = c.label(st);
            self.label(name.to_string(), sx, y + 22.0, lc);
            self.quad(faded(Rect::rounded(x, y + 30.0, track_w, 4.0, c.track, 2.0), st));
            let fill_w = (f * (track_w - 14.0) + 7.0).clamp(4.0, track_w);
            self.quad(faded(Rect::rounded(x, y + 30.0, fill_w, 4.0, c.accent_fill, 2.0), st));
            let kx = x + f * (track_w - 14.0);
            let hit = PanelHit::Ctl { id: row.id, part: CtlPart::Channel(i as u8) };
            self.ring(hit, shape(kx, y + 25.0, 14.0, 14.0, 7.0), self.under(row.id));
            self.quad(faded(Rect::rounded(kx, y + 25.0, 14.0, 14.0, c.accent, 7.0), st));
            if self.live(row) {
                self.hit(area(x, y + 20.0, track_w, 24.0), hit);
            }
        }
    }

    fn chips(&mut self, y: f32, row: &CtlRow, chips: &[(String, bool)], stacked: bool) {
        let c = self.c;
        let st = row.state;
        let n = chips.len().min(8);
        let (mut w, mut total) = self.chips_w(chips);
        let y = if stacked {
            // Under the label, right-aligned; narrowed (labels ellipsized)
            // only when even the full width is short.
            self.row_label_above(row, y);
            if total > self.cw && n > 0 {
                w = ((self.cw - (n - 1) as f32 * 8.0) / n as f32).max(1.0);
                total = self.cw;
            }
            y + STACK_DY
        } else {
            let x_start = self.right() - total;
            self.row_label(row, y + LABEL_DY, x_start);
            y
        };
        let x_start = self.right() - total;
        let banded = self.under(row.id);
        for (i, (t, on)) in chips[..n].iter().enumerate() {
            let x = x_start + i as f32 * (w + 8.0);
            let fill = if *on { c.accent } else { c.ctl };
            let r = Rect::rounded(x, y + 2.0, w, CHIP_H, fill, CHIP_H / 2.0);
            self.ring(PanelHit::Ctl { id: row.id, part: CtlPart::Chip(i as u8) }, r, banded);
            self.quad(faded(r, st));
            let tc = if *on { c.ui.on_accent } else { c.label(st) };
            let shown = self.fit(t, w - 8.0);
            let tx = x + ((w - self.tw(&shown)) * 0.5).max(0.0);
            self.label(shown, tx, y + 2.0 + 4.0, tc);
            if self.live(row) {
                self.hit(r, PanelHit::Ctl { id: row.id, part: CtlPart::Chip(i as u8) });
            }
        }
    }

    /// The chips of a chip-flow row split into lines that fit the content
    /// width: per line, `(chip index, x offset, width)`.
    fn chip_flow_lines(&mut self, chips: &[(String, bool)]) -> Vec<Vec<(usize, f32, f32)>> {
        let mut lines: Vec<Vec<(usize, f32, f32)>> = Vec::new();
        let mut x = 0.0;
        for (i, (t, _)) in chips.iter().enumerate() {
            let w = (self.tw(t) + 24.0).clamp(CHIP_FLOW_MIN_W, self.cw);
            if lines.is_empty() || (x > 0.0 && x + w > self.cw) {
                lines.push(Vec::new());
                x = 0.0;
            }
            lines.last_mut().expect("a line").push((i, x, w));
            x += w + CHIP_FLOW_GAP;
        }
        lines
    }

    /// The label (and its status readout) on line 1; the choice chips flow
    /// below, wrapping to the content width.
    fn chip_flow(&mut self, y: f32, row: &CtlRow, chips: &[(String, bool)], status: &str) {
        let c = self.c;
        let st = row.state;
        // The readout takes at most half the line, so a long one ("Green
        // Phosphor") never crowds the label out in a narrow window.
        let status = self.fit(status, self.cw * 0.5);
        let status_w = self.tw(&status);
        self.row_label(row, y, self.right() - status_w);
        let sc = c.label(st);
        self.label(status, self.right() - status_w, y, sc);
        let under = self.under(row.id);
        for (line, items) in self.chip_flow_lines(chips).into_iter().enumerate() {
            let cy = y + 24.0 + line as f32 * (CHIP_H + CHIP_FLOW_GAP);
            for (i, dx, w) in items {
                let (t, on) = &chips[i];
                let x = self.x0 + dx;
                let r = Rect::rounded(x, cy, w, CHIP_H, if *on { c.accent } else { c.ctl }, CHIP_H / 2.0);
                self.ring(PanelHit::Ctl { id: row.id, part: CtlPart::Chip(i.min(255) as u8) }, r, under);
                self.quad(faded(r, st));
                let shown = self.fit(t, w - 8.0);
                let tx = x + ((w - self.tw(&shown)) * 0.5).max(0.0);
                let tc = if *on { c.ui.on_accent } else { c.label(st) };
                self.label(shown, tx, cy + 4.0, tc);
                if self.live(row) {
                    self.hit(r, PanelHit::Ctl { id: row.id, part: CtlPart::Chip(i.min(255) as u8) });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        y: f32,
        row: &CtlRow,
        items: &[String],
        offset: usize,
        total: usize,
        selected: Option<usize>,
        rows: usize,
    ) {
        let c = self.c;
        let st = row.state;
        let (x0, cw) = (self.x0, self.cw);
        let dn_x = x0 + cw - 22.0;
        let up_x = dn_x - 26.0;
        let visible = items.len().min(rows);
        let mut label_end = up_x;
        if total > rows {
            let counter = format!("{}/{}", (offset + visible).min(total), total);
            let cw_ = self.tw(&counter);
            let cx = up_x - 10.0 - cw_;
            self.label(counter, cx, y, c.ui.text_hint);
            label_end = cx;
        }
        self.row_label(row, y, label_end);
        let gc = c.label(st);
        for (x, g, part) in [(up_x, "^", CtlPart::ScrollUp), (dn_x, "v", CtlPart::ScrollDown)] {
            let r = Rect::rounded(x, y - 1.0, 22.0, 22.0, c.ctl, R_CTL);
            self.quad(faded(r, st));
            self.seg_glyph(g, x, 22.0, y + 1.0, gc);
            if self.live(row) {
                self.hit(r, PanelHit::Ctl { id: row.id, part });
            }
        }
        let card_y = y + 24.0;
        self.quad(faded(Rect::rounded(x0, card_y, cw, list_card_h(rows), c.well, 8.0), st));
        let rx = x0 + LIST_PAD;
        let rw = cw - 2.0 * LIST_PAD;
        for (i, name) in items.iter().take(visible).enumerate() {
            let ry = card_y + LIST_PAD + i as f32 * (LIST_ROW_H + LIST_ROW_GAP);
            let abs = offset + i;
            let sel = selected == Some(abs);
            let r = Rect::rounded(rx, ry, rw, LIST_ROW_H, c.row_sel, 5.0);
            self.ring(PanelHit::Ctl { id: row.id, part: CtlPart::Row(abs) }, r, c.well);
            if sel {
                self.quad(faded(r, st));
                self.quad(faded(Rect::rounded(rx, ry + 3.0, 3.0, LIST_ROW_H - 6.0, c.accent, 1.5), st));
            }
            let shown = self.fit(name, rw - 24.0);
            let tc = if sel { c.value(st) } else { c.label(st) };
            self.label(shown, rx + 12.0, ry + 3.0, tc);
            if self.live(row) {
                self.hit(r, PanelHit::Ctl { id: row.id, part: CtlPart::Row(abs) });
            }
        }
    }

    /// The theme gallery: filter chips, then a grid of theme cards.
    fn gallery(&mut self, y: f32) -> f32 {
        let c = self.c;
        let (x0, cw) = (self.x0, self.cw);
        // The filter chips, wrapping onto a second line in a narrow window.
        let (mut x, mut cy) = (x0, y);
        for f in ThemeFilter::ALL {
            let t = f.label();
            let w = (self.tw(t) + 24.0).max(52.0).min(cw);
            if x > x0 && x + w > x0 + cw {
                (x, cy) = (x0, cy + CHIP_H + CHIP_FLOW_GAP);
            }
            let on = f == self.filter;
            let hov = self.hover == Some(PanelHit::GalleryFilter(f));
            let fill = if on { c.accent } else if hov { c.ctl_hi } else { c.ctl };
            let r = Rect::rounded(x, cy + 2.0, w, CHIP_H, fill, CHIP_H / 2.0);
            self.quad(r);
            let tc = if on { c.ui.on_accent } else { c.ui.text_dim };
            let shown = self.fit(t, w - 8.0);
            let tx = x + ((w - self.tw(&shown)) * 0.5).max(0.0);
            self.label(shown, tx, cy + 6.0, tc);
            self.hit(r, PanelHit::GalleryFilter(f));
            x += w + 6.0;
        }
        let themes: Vec<(usize, jetty_core::Theme)> = (0..jetty_core::theme_count())
            .map(|i| (i, jetty_core::theme_at(i)))
            .filter(|(_, t)| self.filter.matches(t))
            .collect();
        let n = themes.len();
        let count = if n == 1 { "1 theme".to_string() } else { format!("{n} themes") };
        let count_w = self.tw(&count);
        if x0 + cw - count_w > x + 8.0 {
            self.label(count, x0 + cw - count_w, cy + 6.0, c.ui.text_hint);
        }
        let mut h = cy - y + GAL_CHIPS_H;
        if themes.is_empty() {
            let msg = match self.filter {
                ThemeFilter::Mine => "No themes of your own yet: add .toml files to themes/",
                _ => "No themes match",
            };
            let s = self.fit(msg, cw);
            self.label(s, x0, y + h, c.ui.text_hint);
            return h + 28.0;
        }
        let cols = GALLERY_COLS;
        let card_w = ((cw - GAL_GAP_X * (cols - 1) as f32) / cols as f32).floor();
        // Row by row: a row whose names wrap gets a taller caption band.
        let mut cy = y + h;
        for row in themes.chunks(cols) {
            let caps: Vec<Vec<String>> = row.iter().map(|(_, t)| self.caption_lines(&t.display_name, card_w)).collect();
            let lines = caps.iter().map(Vec::len).max().unwrap_or(1);
            let cap_h = GAL_CAPTION_H + lines.saturating_sub(1) as f32 * GAL_CAPTION_LINE;
            for (j, ((idx, t), cap)) in row.iter().zip(caps).enumerate() {
                let cx = x0 + j as f32 * (card_w + GAL_GAP_X);
                self.card(cx, cy, card_w, *idx, t, &cap, cap_h);
            }
            cy += GAL_CARD_H + cap_h + GAL_GAP_Y;
        }
        h = cy - y;
        h
    }

    /// A theme card's caption: the name on one line, or — too long for the
    /// card — wrapped at its last space that fits onto two (the second line
    /// ellipsized if it still overflows), so similar names ("Catppuccin
    /// Mocha", "Catppuccin Latte") never shrink to the same prefix.
    fn caption_lines(&mut self, name: &str, w: f32) -> Vec<String> {
        if self.tw(name) <= w {
            return vec![name.to_string()];
        }
        let mut cut = None;
        for (i, _) in name.match_indices(' ') {
            if self.tw(&name[..i]) <= w {
                cut = Some(i);
            }
        }
        match cut {
            Some(i) => vec![name[..i].to_string(), self.fit(name[i..].trim_start(), w)],
            None => vec![self.fit(name, w)],
        }
    }

    /// One theme card: the theme's background, a two-line colored prompt
    /// sample, its eight normal ANSI colors, and its name (`caption`, one
    /// line or two, in a band `cap_h` tall) underneath.
    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, cx: f32, cy: f32, w: f32, idx: usize, t: &jetty_core::Theme, caption: &[String], cap_h: f32) {
        let c = self.c;
        let h = GAL_CARD_H;
        let selected = idx == self.theme_idx;
        let hovered = self.hover == Some(PanelHit::GalleryCard(idx));
        // Focused: a ring around the whole tile, its name included.
        let tile = shape(cx - 3.0, cy - 3.0, w + 6.0, h + cap_h + 3.0, 11.0);
        self.ring(PanelHit::GalleryCard(idx), tile, c.surface);
        if selected {
            self.quad(Rect::rounded(cx - 3.0, cy - 3.0, w + 6.0, h + 6.0, c.accent, 11.0));
            self.quad(Rect::rounded(cx - 1.0, cy - 1.0, w + 2.0, h + 2.0, c.surface, 9.0));
        } else if hovered {
            self.quad(Rect::rounded(cx - 2.0, cy - 2.0, w + 4.0, h + 4.0, rgba(c.ui.text_hint, 255), 10.0));
        } else {
            self.quad(Rect::rounded(cx - 1.0, cy - 1.0, w + 2.0, h + 2.0, rgba(c.ui.border, 255), 9.0));
        }
        self.quad(Rect::rounded(cx, cy, w, h, [t.bg[0], t.bg[1], t.bg[2], 255], 8.0));
        let p = &t.palette;
        let right = cx + w - 8.0;
        self.segments(cx + 9.0, cy + 6.0, right, &[("~/src", p[4]), (" main", p[5])]);
        self.segments(cx + 9.0, cy + 26.0, right, &[("$", p[2]), (" ls -la", t.fg)]);
        let inner = w - 18.0;
        let sw = ((inner - 7.0 * 2.0) / 8.0).max(2.0);
        for (k, col) in p.iter().take(8).enumerate() {
            let sx = cx + 9.0 + k as f32 * (sw + 2.0);
            self.quad(Rect::rounded(sx, cy + 52.0, sw, 10.0, rgba(*col, 255), 2.0));
        }
        let nc = if selected { c.ui.text } else { c.ui.text_dim };
        for (i, line) in caption.iter().enumerate() {
            self.label(line.clone(), cx, cy + h + 4.0 + i as f32 * GAL_CAPTION_LINE, nc);
        }
        self.hit(area(cx - 3.0, cy - 3.0, w + 6.0, h + cap_h + 3.0), PanelHit::GalleryCard(idx));
    }

    /// Colored text runs left to right from `(x, y)`, cut (with an ellipsis)
    /// where they would cross `right`.
    fn segments(&mut self, mut x: f32, y: f32, right: f32, segs: &[(&str, [u8; 3])]) {
        for &(s, col) in segs {
            let w = self.tw(s);
            if x + w <= right {
                self.label(s.to_string(), x, y, col);
                x += w;
            } else {
                let avail = right - x;
                if avail > 2.0 * self.tw("…") {
                    let f = self.fit(s, avail);
                    self.label(f, x, y, col);
                }
                break;
            }
        }
    }
}

/// Width (logical px) of the knob on the track of `part` — a drag maps the
/// pointer to the knob's CENTER, which travels half a knob inside either end.
pub fn track_knob(part: CtlPart) -> f32 {
    if matches!(part, CtlPart::Channel(_)) { 14.0 } else { 16.0 }
}

/// Height of a list's inset card showing `rows` rows.
fn list_card_h(rows: usize) -> f32 {
    2.0 * LIST_PAD + rows as f32 * LIST_ROW_H + rows.saturating_sub(1) as f32 * LIST_ROW_GAP
}

/// The tab strip's layout (logical px): the names as shown, their widths,
/// the padding each side of a name, and the first cell's x.
struct TabStrip {
    names: [String; N_TABS],
    widths: [f32; N_TABS],
    pad: f32,
    x: f32,
}

/// Lay the tab strip out in the column `[px, px + pw]` whose content starts at
/// `x0` (`cw` wide). Roomy: the classic layout, the first name aligned with the
/// content. Narrower (the Settings window can be sized down to 200 px): the
/// cells spread across the whole column, then the longest names are
/// ellipsized until every tab fits — a tab must never fall off the window.
fn tab_strip(m: &mut dyn ChromeMeasure, u: f32, x0: f32, cw: f32, px: f32, pw: f32) -> TabStrip {
    let n = N_TABS as f32;
    let mut widths = TAB_NAMES.map(|t| m.text_w(t) / u);
    let total: f32 = widths.iter().sum();
    let right = px + pw - 4.0;
    let pad = ((cw - total) / (2.0 * n)).clamp(4.0, 14.0);
    if x0 - pad + total + 2.0 * n * pad <= right {
        return TabStrip { names: TAB_NAMES.map(String::from), widths, pad, x: x0 - pad };
    }
    let left = px + 4.0;
    let room = right - left;
    let pad = ((room - total) / (2.0 * n)).min(4.0);
    if pad >= TAB_PAD_MIN {
        return TabStrip { names: TAB_NAMES.map(String::from), widths, pad, x: left };
    }
    // The widest names give way first: one cap `c` with Σ min(w, c) = budget.
    let budget = (room - 2.0 * n * TAB_PAD_MIN).max(0.0);
    let mut sorted = widths;
    sorted.sort_by(f32::total_cmp);
    let (mut rest, mut cap) = (budget, f32::INFINITY);
    for (i, w) in sorted.iter().enumerate() {
        let k = (N_TABS - i) as f32;
        if w * k > rest {
            cap = rest / k;
            break;
        }
        rest -= w;
    }
    let names = TAB_NAMES.map(|t| fit_head(m, t, cap * u, false));
    for (w, s) in widths.iter_mut().zip(&names) {
        *w = m.text_w(s) / u;
    }
    let pad = ((room - widths.iter().sum::<f32>()) / (2.0 * n)).clamp(0.0, TAB_PAD_MIN);
    TabStrip { names, widths, pad, x: left }
}

/// Build the Settings panel for one frame. `m` measures labels exactly as the
/// Settings text layer draws them.
pub fn build_panel(inp: &PanelInput, m: &mut dyn ChromeMeasure) -> PanelView {
    // ── Layout scale ──────────────────────────────────────────────────────────
    // The window is created at a LOGICAL size but the panel receives the
    // PHYSICAL surface and draws physical, DPI-scaled glyphs. Everything below
    // is laid out in logical space and scaled by `u` (the chrome unit: window
    // DPI × the CAPPED panel text size / 16, × OVERLAY_SCALE) at the end, so
    // controls and text stay proportional at any DPI. The FONT decides text
    // widths only (measured with `m`, converted to logical by `/ u`).
    let u = inp.cm.overlay_u().max(0.1);
    let c = Colors::new(inp.theme);
    let sw = inp.screen_w as f32 / u;
    let sh = inp.screen_h as f32 / u;
    let pw = sw.min(PANEL_MAX_W);
    let px = ((sw - pw) / 2.0).floor().max(0.0);
    let py = 0.0;
    let ph = sh;
    let x0 = px + PAD;
    let cw = (pw - 2.0 * PAD).max(160.0);
    let content_top = py + CONTENT_TOP_OFFSET;
    let content_bottom = (py + ph - FOOTER_H).max(content_top);
    let vis_h = content_bottom - content_top;
    let active_tab = inp.active_tab.min(N_TABS - 1);

    // ── Content, in content space (y = 0 at the content top) ──────────────────
    let spec_line = {
        let size = if inp.ui_font_size.is_finite() { inp.ui_font_size.clamp(6.0, 72.0) } else { 16.0 };
        (size * inp.cm.dpi / u * 1.3).ceil()
    };
    let mut lay = Lay {
        m: &mut *m,
        u,
        x0,
        cw,
        c,
        hover: inp.hover,
        focus: inp.focus,
        ring: inp.focus_part,
        spec_line,
        theme_idx: inp.theme_idx,
        filter: inp.filter,
        out: Out::default(),
    };
    let mut y = CONTENT_PAD;
    for (i, item) in inp.items.iter().enumerate() {
        y += lay.item(item, y, i == 0);
    }
    let content_h = y + CONTENT_PAD + 8.0;
    let Out { quads: mut cq, labels: mut cl, hits: mut ch, anchors, specimen } = lay.out;

    // ── Scroll: clamp, then shift the content into the viewport ──────────────
    let max_scroll = (content_h - vis_h).max(0.0);
    let scroll = if inp.scroll.is_finite() { (inp.scroll / u).clamp(0.0, max_scroll) } else { 0.0 };
    let shift = content_top - scroll;
    for q in &mut cq {
        q.y += shift;
    }
    for l in &mut cl {
        l.2 += shift;
    }
    for (r, _) in &mut ch {
        r.y += shift;
    }
    let ui_specimen_pos = match specimen {
        Some((sx, top, line)) if top + shift >= content_top && top + shift + line <= content_bottom => {
            (sx * u, (top + shift) * u)
        }
        _ => (x0 * u, OFF),
    };

    // ── Chrome ────────────────────────────────────────────────────────────────
    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<Label> = Vec::new();
    let mut chrome_hits: Vec<(Rect, PanelHit)> = Vec::new();
    // The backdrop fills the WHOLE surface in the panel surface color, so the
    // window has no dark margins or corner wedges on any theme.
    quads.push(Rect::new(0.0, 0.0, sw, sh, c.surface));

    // Title row.
    labels.push(("Settings".to_string(), x0, py + 14.0, c.ui.text));

    // Tab strip: leading-aligned cells sized to their measured labels, on a
    // full-width hairline; the active tab carries an accent underline.
    let strip = tab_strip(m, u, x0, cw, px, pw);
    let (names_w, tab_pad) = (strip.widths, strip.pad);
    let mut tab_rects = [Rect::default(); N_TABS];
    let mut tx = strip.x;
    for (i, (r, name)) in tab_rects.iter_mut().zip(strip.names).enumerate() {
        let w = names_w[i] + 2.0 * tab_pad;
        *r = area(tx, py + 44.0, w, 32.0);
        let col = if i == active_tab { c.ui.text } else { c.ui.text_dim };
        labels.push((name, tx + tab_pad, py + 52.0, col));
        tx += w;
    }
    quads.push(Rect::new(0.0, py + 75.0, sw, 1.0, c.hair));
    {
        let cell = tab_rects[active_tab];
        let bar_w = names_w[active_tab] + 8.0;
        quads.push(Rect::rounded(cell.x + (cell.w - bar_w) * 0.5, py + 74.0, bar_w, 2.0, c.accent, 1.0));
    }

    // Footer: hairline, hint, "Reset tab".
    let fy = py + ph - FOOTER_H;
    quads.push(Rect::new(0.0, fy, sw, 1.0, c.hair));
    let (btn_text, btn_fill, btn_col) = match inp.reset {
        ResetState::Armed => ("Confirm reset", rgba(c.ui.danger, 255), c.ui.on_danger),
        ResetState::Ready => {
            let hov = inp.hover == Some(PanelHit::ResetTab);
            ("Reset tab", if hov { c.ctl_hi } else { c.ctl }, c.ui.text_dim)
        }
        ResetState::Disabled => ("Reset tab", rgba(c.ui.shade(0.10), 102), c.ui.text_hint),
    };
    let btn_tw = m.text_w(btn_text) / u;
    let btn_w = (btn_tw + 28.0).max(88.0);
    let btn = Rect::rounded(x0 + cw - btn_w, fy + (FOOTER_H - 26.0) / 2.0, btn_w, 26.0, btn_fill, R_CTL);
    quads.push(btn);
    labels.push((btn_text.to_string(), btn.x + (btn_w - btn_tw) * 0.5, btn.y + 3.0, btn_col));
    if inp.reset != ResetState::Disabled {
        chrome_hits.push((btn, PanelHit::ResetTab));
    }
    let hint = if inp.footer_hint.is_empty() { "Esc to close" } else { inp.footer_hint };
    let hint = fit_head(m, hint, ((btn.x - 12.0 - x0) * u).max(0.0), false);
    labels.push((hint, x0, fy + (FOOTER_H - 21.0) / 2.0, c.ui.text_hint));

    // Scrollbar: a slim thumb at the column's right edge when content overflows.
    let mut scroll_thumb = None;
    if max_scroll > 0.0 && vis_h > 8.0 {
        let tx = px + pw - 10.0;
        let thumb_h = (vis_h * vis_h / content_h).clamp(28.0_f32.min(vis_h), vis_h);
        let thumb_y = content_top + (scroll / max_scroll) * (vis_h - thumb_h);
        let hot = inp.scroll_dragging || inp.hover == Some(PanelHit::ScrollThumb);
        let (w, a) = if hot { (6.0, 230) } else { (4.0, 150) };
        quads.push(Rect::rounded(tx + 2.0 - w / 2.0, thumb_y + 2.0, w, thumb_h - 4.0, rgba(c.ui.text_hint, a), w / 2.0));
        let thumb = area(tx - 5.0, thumb_y, 14.0, thumb_h);
        chrome_hits.push((thumb, PanelHit::ScrollThumb));
        chrome_hits.push((area(tx - 5.0, content_top, 14.0, vis_h), PanelHit::ScrollTrack));
        scroll_thumb = Some(thumb);
    }

    // ── Scale the logical layout to physical px ────────────────────────────────
    let sr = |mut r: Rect| -> Rect {
        r.x *= u;
        r.y *= u;
        r.w *= u;
        r.h *= u;
        r.radius *= u;
        r
    };
    let sl = |l: &mut Label| {
        l.1 *= u;
        l.2 *= u;
    };
    let quads: Vec<Rect> = quads.into_iter().map(sr).collect();
    let content_quads: Vec<Rect> = cq.into_iter().map(sr).collect();
    labels.iter_mut().for_each(sl);
    cl.iter_mut().for_each(sl);
    let geom = PanelGeom {
        panel: sr(area(px, py, pw, ph)),
        title_bar: sr(area(px, py, pw, 44.0)),
        tab_rects: tab_rects.map(sr),
        chrome_hits: chrome_hits.into_iter().map(|(r, h)| (sr(r), h)).collect(),
        hits: ch.into_iter().map(|(r, h)| (sr(r), h)).collect(),
        content_top: content_top * u,
        content_bottom: content_bottom * u,
        scroll: scroll * u,
        max_scroll: max_scroll * u,
        scroll_thumb: scroll_thumb.map(sr),
        anchors: anchors.into_iter().map(|(id, t, b)| (id, t * u, b * u)).collect(),
    };

    // The content viewport in physical px, intersected with the surface so a
    // scissor can never exceed the render target (a wgpu validation error);
    // tiling WMs can force a surface smaller than the panel.
    let content_viewport = {
        let vy = (content_top * u).max(0.0);
        let vh = (vis_h * u).min(inp.screen_h as f32 - vy);
        let vw = inp.screen_w as f32;
        if vw >= 1.0 && vh >= 1.0 {
            Some([0, vy as u32, vw as u32, vh as u32])
        } else {
            None
        }
    };

    PanelView {
        quads,
        labels,
        content_quads,
        content_labels: cl,
        content_viewport,
        geom,
        ui_specimen_pos,
        specimen_rgb: c.ui.accent,
        surface: c.ui.surface,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::MonoMeasure;

    fn row(id: CtlId, label: &str, show: CtlShow) -> PanelItem {
        PanelItem::Row(CtlRow { id, label: label.to_string(), show, state: RowState::Normal, hint: None })
    }

    /// One of every item kind, with long labels/values that must be fitted.
    fn sample_items() -> Vec<PanelItem> {
        vec![
            PanelItem::Section {
                id: "s.one",
                title: "A section with a deliberately long title that cannot fit".into(),
                master: Some(("master", true)),
                collapsed: false,
                hint: Some("A helper line under the header, long enough to need truncating".into()),
            },
            row("slider", "Opacity", CtlShow::Slider { frac: 0.5, text: "50%".into() }),
            row("toggle", "A toggle whose label is far too long for its row width", CtlShow::Toggle(true)),
            row("cycler", "Window mode with a long label", CtlShow::Cycler("A value too wide for the gap".into())),
            row("stepper", "Font size with a long label", CtlShow::Stepper("16pt".into())),
            row("rgb", "Tint", CtlShow::Rgb([1.0, 0.5, 0.0])),
            row(
                "chips",
                "Animate",
                CtlShow::Chips(vec![("Roll".into(), true), ("Flicker".into(), false), ("Jitter".into(), false)]),
            ),
            row(
                "presets",
                "Preset",
                CtlShow::ChipFlow {
                    chips: ["Clean", "Retro CRT", "Amber", "Green Phosphor", "Neon", "Paper", "E-ink"]
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (n.to_string(), i == 1))
                        .collect(),
                    status: "Retro CRT".into(),
                },
            ),
            PanelItem::Section { id: "s.two", title: "Lists".into(), master: None, collapsed: false, hint: None },
            row(
                "list",
                "Font",
                CtlShow::List {
                    items: vec!["JetBrains Mono".into(), "A family name that is very long indeed".into()],
                    offset: 0,
                    total: 9,
                    selected: Some(1),
                    rows: 5,
                },
            ),
            PanelItem::Specimen,
            PanelItem::Section { id: "s.three", title: "Theme".into(), master: None, collapsed: false, hint: None },
            PanelItem::Gallery,
        ]
    }

    fn view_with(items: &[PanelItem], w: u32, h: u32, scale: f32, font: f32, scroll: f32) -> PanelView {
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let cm = ChromeMetrics::new(scale, font);
        let mut inp = PanelInput::new(w, h, &theme, cm, items);
        inp.scroll = scroll;
        // The monospace reference advance at this metrics' unit (≈ the default
        // chrome font, the widest realistic case).
        let adv = CHAR_W_FALLBACK * cm.overlay_u();
        build_panel(&inp, &mut MonoMeasure(adv))
    }

    #[test]
    fn every_label_fits_the_content_column_at_1x_2x_and_both_ui_sizes() {
        let items = sample_items();
        for (scale, font) in [(1.0, 16.0), (2.0, 16.0), (1.0, 13.0), (1.0, 17.0), (2.0, 17.0)] {
            let cm = ChromeMetrics::new(scale, font);
            let (w, h) = ((PANEL_W * cm.overlay_u()) as u32, (PANEL_H * cm.overlay_u()) as u32);
            let v = view_with(&items, w, h, scale, font, 0.0);
            let adv = CHAR_W_FALLBACK * cm.overlay_u();
            let right = v.geom.panel.x + v.geom.panel.w - PAD * cm.overlay_u();
            for (text, x, _, _) in v.labels.iter().chain(v.content_labels.iter()) {
                let end = x + text.chars().count() as f32 * adv;
                assert!(end <= right + 0.5, "{scale}×/{font}pt: {text:?} ends at {end:.1} > {right:.1}");
            }
        }
    }

    #[test]
    fn row_labels_never_run_into_their_controls() {
        // Every control row's label ends left of the first hit rect of that row.
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        for (id, label) in [("toggle", "A toggle"), ("cycler", "Window mode"), ("stepper", "Font size")] {
            let ctl_x = v
                .geom
                .hits
                .iter()
                .filter(|(_, h)| matches!(h, PanelHit::Ctl { id: i, .. } if *i == id))
                .map(|(r, _)| r.x)
                .fold(f32::INFINITY, f32::min);
            let l = v.content_labels.iter().find(|l| l.0.starts_with(label)).expect("label");
            let end = l.1 + l.0.chars().count() as f32 * CHAR_W_FALLBACK * ChromeMetrics::DEFAULT.overlay_u();
            assert!(end <= ctl_x, "{id}: label ends {end} past control {ctl_x}");
        }
    }

    #[test]
    fn hidpi_layout_is_exact_2x_of_scale1() {
        let items = sample_items();
        let one = view_with(&items, 420, 592, 1.0, 16.0, 40.0);
        let two = view_with(&items, 840, 1184, 2.0, 16.0, 80.0);
        let close = |a: f32, b: f32| (b - 2.0 * a).abs() < 0.05;
        assert_eq!(one.quads.len(), two.quads.len());
        assert_eq!(one.content_quads.len(), two.content_quads.len());
        for (a, b) in one.quads.iter().chain(&one.content_quads).zip(two.quads.iter().chain(&two.content_quads)) {
            assert!(close(a.x, b.x) && close(a.y, b.y) && close(a.w, b.w) && close(a.h, b.h));
        }
        for (a, b) in one.labels.iter().chain(&one.content_labels).zip(two.labels.iter().chain(&two.content_labels)) {
            assert_eq!(a.0, b.0);
            assert!(close(a.1, b.1) && close(a.2, b.2), "{:?}", a.0);
        }
        assert_eq!(one.geom.hits.len(), two.geom.hits.len());
        for ((a, ha), (b, hb)) in one.geom.hits.iter().zip(&two.geom.hits) {
            assert_eq!(ha, hb);
            assert!(close(a.x, b.x) && close(a.y, b.y) && close(a.w, b.w) && close(a.h, b.h));
        }
        assert!(close(one.geom.scroll, two.geom.scroll));
    }

    #[test]
    fn scroll_is_clamped_and_reported() {
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 1.0e9);
        assert!(v.geom.max_scroll > 0.0, "the sample overflows the viewport");
        assert_eq!(v.geom.scroll, v.geom.max_scroll);
        let v = view_with(&items, 420, 592, 1.0, 16.0, -50.0);
        assert_eq!(v.geom.scroll, 0.0);
        let v = view_with(&items, 420, 592, 1.0, 16.0, f32::NAN);
        assert_eq!(v.geom.scroll, 0.0);
        // A short tab never scrolls and has no thumb.
        let short = vec![row("t", "Toggle", CtlShow::Toggle(false))];
        let v = view_with(&short, 420, 592, 1.0, 16.0, 300.0);
        assert_eq!((v.geom.max_scroll, v.geom.scroll), (0.0, 0.0));
        assert!(v.geom.scroll_thumb.is_none());
    }

    #[test]
    fn content_hits_live_only_inside_the_viewport() {
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 120.0);
        let g = &v.geom;
        // A hit scrolled above the viewport (under the tab strip) is dead…
        let above = g.hits.iter().find(|(r, _)| r.y + r.h < g.content_top).expect("a part scrolled above");
        let (r, _) = above;
        assert_ne!(g.hit_at(r.x + r.w / 2.0, r.y + r.h / 2.0), Some(above.1));
        // …and one inside it is live.
        let inside = g
            .hits
            .iter()
            .find(|(r, _)| r.y >= g.content_top && r.y + r.h < g.content_bottom)
            .expect("a visible part");
        let (r, h) = inside;
        assert_eq!(g.hit_at(r.x + r.w / 2.0, r.y + r.h / 2.0), Some(*h));
    }

    #[test]
    fn master_switch_wins_over_its_section_header() {
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        let g = &v.geom;
        let sw = g.rect_of(PanelHit::Ctl { id: "master", part: CtlPart::Switch }).unwrap();
        assert_eq!(
            g.hit_at(sw.x + sw.w / 2.0, sw.y + sw.h / 2.0),
            Some(PanelHit::Ctl { id: "master", part: CtlPart::Switch })
        );
        let hdr = g.rect_of(PanelHit::Section("s.one")).unwrap();
        assert_eq!(g.hit_at(hdr.x + 20.0, hdr.y + hdr.h / 2.0), Some(PanelHit::Section("s.one")));
    }

    #[test]
    fn every_control_part_is_reported() {
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        let g = &v.geom;
        for (id, part) in [
            ("slider", CtlPart::Track),
            ("toggle", CtlPart::Switch),
            ("cycler", CtlPart::Prev),
            ("cycler", CtlPart::Next),
            ("stepper", CtlPart::Minus),
            ("stepper", CtlPart::Plus),
            ("stepper", CtlPart::Reset),
            ("rgb", CtlPart::Channel(0)),
            ("rgb", CtlPart::Channel(2)),
            ("chips", CtlPart::Chip(1)),
            ("presets", CtlPart::Chip(0)),
            ("presets", CtlPart::Chip(6)),
            ("list", CtlPart::Row(1)),
            ("list", CtlPart::ScrollUp),
            ("list", CtlPart::ScrollDown),
        ] {
            assert!(g.rect_of(PanelHit::Ctl { id, part }).is_some(), "missing {id} {part:?}");
        }
        for id in ["s.one", "slider", "toggle", "list", "s.three"] {
            assert!(g.anchor(id).is_some(), "missing anchor {id}");
        }
    }

    #[test]
    fn disabled_rows_draw_but_never_hit() {
        let items = vec![PanelItem::Row(CtlRow {
            id: "d",
            label: "Dropdown height".into(),
            show: CtlShow::Slider { frac: 0.3, text: "30%".into() },
            state: RowState::Disabled,
            hint: None,
        })];
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        assert!(v.geom.hits.is_empty());
        assert!(!v.content_quads.is_empty());
    }

    #[test]
    fn gallery_lays_every_matching_theme_in_three_columns() {
        let items = vec![PanelItem::Gallery];
        let v = view_with(&items, 420, 3000, 1.0, 16.0, 0.0);
        let cards: Vec<(Rect, usize)> = v
            .geom
            .hits
            .iter()
            .filter_map(|(r, h)| if let PanelHit::GalleryCard(i) = h { Some((*r, *i)) } else { None })
            .collect();
        assert_eq!(cards.len(), gallery_order(ThemeFilter::All).len());
        let xs: std::collections::BTreeSet<i32> = cards.iter().map(|(r, _)| r.x.round() as i32).collect();
        assert_eq!(xs.len(), GALLERY_COLS, "three columns");
        // Cards run in registry order, left to right then top to bottom.
        let order: Vec<usize> = cards.iter().map(|c| c.1).collect();
        assert_eq!(order, gallery_order(ThemeFilter::All));
        // Every filter chip is clickable.
        for f in ThemeFilter::ALL {
            assert!(v.geom.rect_of(PanelHit::GalleryFilter(f)).is_some());
        }
        // Cards stay inside the column.
        let right = v.geom.panel.x + v.geom.panel.w - PAD;
        assert!(cards.iter().all(|(r, _)| r.x + r.w <= right + 4.0));
    }

    /// Every theme card's caption tells its theme apart, inside its card: a
    /// long name wraps onto a second line instead of being cut to a prefix
    /// several themes share ("Catppuccin …" was Mocha — the default —, Latte,
    /// Frappe and Macchiato; "Tokyo Night…" three more).
    #[test]
    fn gallery_captions_tell_every_theme_apart() {
        for (scale, font) in [(1.0, 16.0), (2.0, 16.0), (1.0, 13.0)] {
            let cm = ChromeMetrics::new(scale, font);
            let u = cm.overlay_u();
            let adv = CHAR_W_FALLBACK * u;
            let v = view_with(&[PanelItem::Gallery], (PANEL_W * u) as u32, 20_000, scale, font, 0.0);
            let mut captions = Vec::new();
            for (r, h) in &v.geom.hits {
                let PanelHit::GalleryCard(_) = h else { continue };
                let below = r.y + (3.0 + GAL_CARD_H) * u;
                let lines: Vec<&Label> = v
                    .content_labels
                    .iter()
                    .filter(|l| l.1 >= r.x - 0.5 && l.1 < r.x + r.w && l.2 >= below && l.2 < r.y + r.h)
                    .collect();
                for l in &lines {
                    let end = l.1 + l.0.chars().count() as f32 * adv;
                    assert!(end <= r.x + r.w + 0.5, "{scale}×: {:?} overflows its card", l.0);
                }
                captions.push(lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>().join(" "));
            }
            let mut unique = captions.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), captions.len(), "{scale}×/{font}pt: ambiguous captions {captions:?}");
            assert!(captions.iter().any(|c| c == "Catppuccin Mocha"), "{scale}×: the default theme's full name");
        }
    }

    #[test]
    fn gallery_filters_split_dark_and_light() {
        let all = gallery_order(ThemeFilter::All);
        let dark = gallery_order(ThemeFilter::Dark);
        let light = gallery_order(ThemeFilter::Light);
        assert_eq!(dark.len() + light.len(), all.len());
        assert!(light.iter().any(|&i| jetty_core::theme_at(i).name == "solarized_light"));
        assert!(dark.iter().any(|&i| jetty_core::theme_at(i).name == "catppuccin_mocha"));
        // No user themes are registered in unit tests.
        assert!(gallery_order(ThemeFilter::Mine).is_empty());
        let mut user = jetty_core::Theme::by_name("dracula");
        user.name = Cow::Owned("dracula".to_string());
        assert!(is_user_theme(&user));
        assert!(!is_user_theme(&jetty_core::Theme::by_name("dracula")));
    }

    #[test]
    fn specimen_is_placed_only_when_fully_visible() {
        let mut items = vec![row("ui_font_size", "UI font size", CtlShow::Stepper("16pt".into())), PanelItem::Specimen];
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        let (_, y) = v.ui_specimen_pos;
        assert!(y >= v.geom.content_top && y < v.geom.content_bottom, "visible at the top");
        // Below the fold (pushed down by the sample rows) → offscreen.
        items.splice(0..0, sample_items());
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        assert!(v.ui_specimen_pos.1 >= 1.0e5, "scrolled out → offscreen");
        let v = view_with(&[], 420, 592, 1.0, 16.0, 0.0);
        assert!(v.ui_specimen_pos.1 >= 1.0e5, "no specimen item → offscreen");
    }

    #[test]
    fn viewport_stays_inside_tiny_surfaces() {
        let items = sample_items();
        for (w, h) in [(600u32, 400u32), (300, 200), (420, 120), (1000, 700), (200, 60)] {
            let v = view_with(&items, w, h, 1.0, 16.0, 0.0);
            if let Some([vx, vy, vw, vh]) = v.content_viewport {
                assert!(vx + vw <= w && vy + vh <= h, "{w}×{h}: {vx},{vy} {vw}×{vh}");
            }
        }
    }

    #[test]
    fn panel_fills_the_surface_and_centers_a_bounded_column() {
        let items = sample_items();
        let v = view_with(&items, 1600, 900, 1.0, 16.0, 0.0);
        let bg = v.quads[0];
        assert!(bg.x == 0.0 && bg.y == 0.0 && (bg.w - 1600.0).abs() < 0.01 && (bg.h - 900.0).abs() < 0.01);
        let p = v.geom.panel;
        assert!(p.w <= PANEL_MAX_W * ChromeMetrics::DEFAULT.overlay_u() + 0.5);
        assert!(((p.x + p.w / 2.0) - 800.0).abs() < 2.0, "centered");
        assert_eq!(v.surface, UiPalette::from_theme(&jetty_core::Theme::by_name("catppuccin_mocha")).surface);
    }

    #[test]
    fn tab_strip_has_one_cell_per_tab_left_to_right() {
        let v = view_with(&[], 420, 592, 1.0, 16.0, 0.0);
        let g = &v.geom;
        let mut prev = f32::NEG_INFINITY;
        for (i, r) in g.tab_rects.iter().enumerate() {
            assert!(r.x + 0.5 >= prev, "tab {i} overlaps");
            assert_eq!(g.hit_at(r.x + r.w / 2.0, r.y + r.h / 2.0), Some(PanelHit::Tab(i)));
            prev = r.x + r.w;
        }
        assert!(prev <= g.panel.x + g.panel.w, "strip fits the column");
        for n in TAB_NAMES {
            assert!(v.labels.iter().any(|l| l.0 == n), "missing tab label {n}");
        }
    }

    #[test]
    fn footer_reset_states() {
        let theme = jetty_core::Theme::by_name("solarized_light");
        let items = sample_items();
        for (state, text, live) in [
            (ResetState::Ready, "Reset tab", true),
            (ResetState::Armed, "Confirm reset", true),
            (ResetState::Disabled, "Reset tab", false),
        ] {
            let mut inp = PanelInput::new(420, 592, &theme, ChromeMetrics::DEFAULT, &items);
            inp.reset = state;
            let v = build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK));
            assert!(v.labels.iter().any(|l| l.0 == text), "{state:?}");
            assert_eq!(v.geom.chrome_hits.iter().any(|(_, h)| *h == PanelHit::ResetTab), live, "{state:?}");
        }
    }

    #[test]
    fn scroll_to_reveal_moves_minimally() {
        let items = sample_items();
        let v = view_with(&items, 420, 592, 1.0, 16.0, 0.0);
        let g = &v.geom;
        // Already visible → unchanged.
        assert_eq!(g.scroll_to_reveal(10.0, 40.0, 8.0), 0.0);
        // Below → scrolls just enough (bottom + margin at the viewport bottom).
        let vh = g.viewport_h();
        let s = g.scroll_to_reveal(vh + 100.0, vh + 140.0, 8.0);
        assert!((s - (148.0f32).min(g.max_scroll)).abs() < 0.5, "{s}");
        // Never past the end.
        assert!(g.scroll_to_reveal(1.0e6, 1.0e6 + 10.0, 8.0) <= g.max_scroll);
    }

    /// The keyboard-focused part gets a ring — an accent outline around its
    /// visible shape, under the part — and nothing else changes; a slider's
    /// ring circles its knob.
    #[test]
    fn the_focused_part_is_ringed() {
        let items = sample_items();
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let plain = view_with(&items, 420, 3000, 1.0, 16.0, 0.0);
        let accent = UiPalette::cached(&theme).accent;
        for (hit, ring_w) in [
            (PanelHit::Ctl { id: "chips", part: CtlPart::Chip(1) }, None),
            (PanelHit::Ctl { id: "toggle", part: CtlPart::Switch }, Some(SW_W)),
            (PanelHit::Ctl { id: "slider", part: CtlPart::Track }, Some(16.0)),
            (PanelHit::Ctl { id: "stepper", part: CtlPart::Reset }, Some(RESET_W)),
            (PanelHit::Ctl { id: "presets", part: CtlPart::Chip(3) }, None),
            (PanelHit::Ctl { id: "list", part: CtlPart::Row(1) }, None),
            (PanelHit::Ctl { id: "master", part: CtlPart::Switch }, Some(SW_W)),
        ] {
            let mut inp = PanelInput::new(420, 3000, &theme, ChromeMetrics::DEFAULT, &items);
            inp.focus_part = Some(hit);
            let v = build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK * ChromeMetrics::DEFAULT.overlay_u()));
            assert_eq!(v.content_quads.len(), plain.content_quads.len() + 2, "{hit:?}: a ring and its gap");
            let key = |q: &Rect| (q.x.to_bits(), q.y.to_bits(), q.w.to_bits(), q.h.to_bits(), q.color);
            let before: Vec<_> = plain.content_quads.iter().map(key).collect();
            let ring = v.content_quads.iter().find(|q| !before.contains(&key(q)) && q.color[..3] == accent);
            let ring = ring.unwrap_or_else(|| panic!("{hit:?}: no accent ring"));
            let r = plain.geom.rect_of(hit).unwrap();
            if let Some(w) = ring_w {
                let u = ChromeMetrics::DEFAULT.overlay_u();
                assert!((ring.w - (w + 8.0) * u).abs() < 0.01, "{hit:?}: the ring hugs the part ({})", ring.w);
            } else {
                assert!(ring.x < r.x && ring.x + ring.w > r.x + r.w, "{hit:?}: the ring encloses the part: ring {} {} {} {} part {} {} {} {}", ring.x, ring.y, ring.w, ring.h, r.x, r.y, r.w, r.h);
            }
            assert_eq!(v.content_labels, plain.content_labels, "{hit:?}: the text is untouched");
        }
        // A part that is not drawn rings nothing.
        let mut inp = PanelInput::new(420, 3000, &theme, ChromeMetrics::DEFAULT, &items);
        inp.focus_part = Some(PanelHit::Ctl { id: "nope", part: CtlPart::Switch });
        assert_eq!(build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK * ChromeMetrics::DEFAULT.overlay_u())).content_quads.len(), plain.content_quads.len());
    }

    /// The stepper's value always shows whole, clear of the "-" / "+"
    /// separators — at fractional scales the real font's hinted advance is
    /// wider than the design estimate ("16pt" came out as "16…" at 1.25× and
    /// 1.5×, and touched both separator lines at 1× and 2×).
    #[test]
    fn stepper_value_is_never_cut_or_crowded() {
        let items = vec![row("font_size", "Font size", CtlShow::Stepper("16pt".into()))];
        // (DPI, UI font, the chrome font's advance as the GPU layer measures it:
        // the design advance, or a hinted one rounded up to whole px.)
        for (dpi, font, adv) in [(1.0, 16.0, 9.6328), (1.25, 14.0, 11.0), (1.5, 13.0, 12.0), (2.0, 16.0, 19.27)] {
            let cm = ChromeMetrics::new(dpi, font);
            let u = cm.overlay_u();
            let (w, h) = ((PANEL_W * u).ceil() as u32, (PANEL_H * u).ceil() as u32);
            let theme = jetty_core::Theme::by_name("catppuccin_mocha");
            let inp = PanelInput::new(w, h, &theme, cm, &items);
            let mut m = MonoMeasure(adv);
            let v = build_panel(&inp, &mut m);
            let g = &v.geom;
            let minus = g.rect_of(PanelHit::Ctl { id: "font_size", part: CtlPart::Minus }).unwrap();
            let plus = g.rect_of(PanelHit::Ctl { id: "font_size", part: CtlPart::Plus }).unwrap();
            let value = v.content_labels.iter().find(|l| l.0.starts_with("16")).expect("value label");
            assert_eq!(value.0, "16pt", "{dpi}×/{font}pt: the value was cut");
            let (l, r) = (value.1, value.1 + m.text_w(&value.0));
            let gap = 4.0 * u;
            assert!(l >= minus.x + minus.w + gap - 0.01, "{dpi}×/{font}pt: value crowds the '-' separator");
            assert!(r <= plus.x - gap + 0.01, "{dpi}×/{font}pt: value crowds the '+' separator");
        }
    }

    /// The tab strip fits any Settings window the user can size (the window's
    /// floor is 200 logical px): every tab stays reachable, no label runs
    /// past the window, and a name that cannot fit is ellipsized. At the
    /// design width every name shows whole, as before.
    #[test]
    fn tab_strip_fits_any_window_width() {
        for (dpi, font) in [(1.0, 16.0), (2.0, 16.0), (1.0, 13.0), (1.0, 17.0)] {
            let cm = ChromeMetrics::new(dpi, font);
            let adv = CHAR_W_FALLBACK * cm.overlay_u();
            for lw in (200..=720).step_by(10) {
                let sw = (lw as f32 * dpi) as u32;
                let v = view_with(&[], sw, (PANEL_H * dpi) as u32, dpi, font, 0.0);
                let g = &v.geom;
                let mut prev = f32::NEG_INFINITY;
                for (i, r) in g.tab_rects.iter().enumerate() {
                    assert!(r.w > 0.0 && r.x >= -0.01 && r.x + r.w <= sw as f32 + 0.01, "{lw}px: tab {i} outside");
                    assert!(r.x + 0.01 >= prev, "{lw}px: tab {i} overlaps");
                    assert_eq!(g.hit_at(r.x + r.w / 2.0, r.y + r.h / 2.0), Some(PanelHit::Tab(i)), "{lw}px: tab {i}");
                    prev = r.x + r.w;
                    let l = v.labels.iter().find(|l| (l.2 - r.y - 8.0 * cm.overlay_u()).abs() < 0.5 && l.1 >= r.x - 0.01 && l.1 < r.x + r.w)
                        .unwrap_or_else(|| panic!("{lw}px: tab {i} has no label"));
                    assert!(l.0 == TAB_NAMES[i] || l.0.ends_with('…'), "{lw}px: {:?} cut without an ellipsis", l.0);
                    let end = l.1 + l.0.chars().count() as f32 * adv;
                    assert!(end <= r.x + r.w + 0.5 && end <= sw as f32, "{lw}px: {:?} runs past its tab", l.0);
                }
            }
            // The design width keeps every name whole.
            let u = cm.overlay_u();
            let v = view_with(&[], (PANEL_W * u) as u32, (PANEL_H * u) as u32, dpi, font, 0.0);
            for n in TAB_NAMES {
                assert!(v.labels.iter().any(|l| l.0 == n), "{dpi}×/{font}pt: {n} cut at the design width");
            }
        }
    }

    /// In a narrow window no row label runs into its control: a control that
    /// leaves its label too little room drops below the label (as sliders
    /// always do), and every control stays inside the content column. (A
    /// 300-px window drew "A…" under the first Animate chip; at 240 px the
    /// chips ran off the window and every cycler covered its label.)
    #[test]
    fn narrow_rows_stack_instead_of_overlapping() {
        let mut items = sample_items();
        items.push(row(
            "anim",
            "Animate",
            CtlShow::Chips(vec![("Roll".into(), true), ("Flicker".into(), false), ("Jitter".into(), false)]),
        ));
        items.push(row("shape", "Shape", CtlShow::Cycler("Block".into())));
        for (dpi, font) in [(1.0, 16.0), (2.0, 16.0), (1.0, 13.0)] {
            let cm = ChromeMetrics::new(dpi, font);
            let u = cm.overlay_u();
            let adv = CHAR_W_FALLBACK * u;
            for lw in (200..=440).step_by(20) {
                let sw = (lw as f32 * dpi) as u32;
                let v = view_with(&items, sw, 40_000, dpi, font, 0.0);
                let g = &v.geom;
                let (x0, right) = (g.panel.x + PAD * u, g.panel.x + g.panel.w - PAD * u);
                let ctl: Vec<&Rect> =
                    g.hits.iter().filter(|(_, h)| matches!(h, PanelHit::Ctl { .. })).map(|(r, _)| r).collect();
                for r in &ctl {
                    assert!(r.x >= x0 - 0.5 && r.x + r.w <= right + 0.5, "{lw}px {dpi}×: a control leaves the column");
                }
                for (t, lx, ly, _) in &v.content_labels {
                    let (l, r) = (*lx, lx + t.chars().count() as f32 * adv);
                    assert!(r <= right + 0.5, "{lw}px {dpi}×: {t:?} past the column");
                    // The label's ink band (not its whole line box).
                    let (top, bot) = (ly + 4.0 * u, ly + 16.0 * u);
                    for c in ctl.iter().filter(|c| c.y < bot && c.y + c.h > top) {
                        let inside = l >= c.x - 0.5 && r <= c.x + c.w + 0.5;
                        let apart = r <= c.x + 0.5 || l >= c.x + c.w - 0.5;
                        assert!(inside || apart, "{lw}px {dpi}×: {t:?} runs into a control at x={:.0}", c.x);
                    }
                }
            }
        }
        // The design width keeps the one-line rows (nothing stacks).
        let wide = view_with(&items, 420, 40_000, 1.0, 16.0, 0.0);
        let cyc = wide.geom.rect_of(PanelHit::Ctl { id: "shape", part: CtlPart::Prev }).unwrap();
        let lab = wide.content_labels.iter().find(|l| l.0 == "Shape").unwrap();
        assert!((cyc.y - lab.2).abs() < 8.0, "a roomy cycler stays beside its label");
    }
}
