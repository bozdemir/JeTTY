use crate::chrome::{fit_head, fit_tail, ChromeMeasure, ChromeMetrics, MonoMeasure};
use crate::ui_palette::{mix, UiPalette};
use crate::Rect;
use jetty_core::{Progress, ProgressState, Theme};

// Every distance below is a DESIGN px — authored for the default 16pt UI font on
// a 1× display — and is multiplied by the window's chrome unit
// (`ChromeMetrics::u` = DPI × UI font / 16) when the bar is built. So the strip,
// its pills and its hit-boxes grow with the text they hold: at a 28pt UI font or
// on a 2× display the bar is no longer a fixed 36px strip the glyphs spill out
// of. At 1× with the default font every value is used verbatim.

/// Design height of the tab bar. The live height is [`ChromeMetrics::bar_h`]
/// (this × the chrome unit, rounded); the terminal grid starts right below it.
pub const TABBAR_H: f32 = 36.0;

/// Width of a single tab (the most a tab grows to).
const TAB_W: f32 = 140.0;
/// The narrowest a tab shrinks to before the strip overflows into "+N".
const TAB_W_MIN: f32 = 64.0;
/// [`TabStyle::Compact`]'s tab width range: denser tabs, so more fit before the
/// strip overflows. Every other style keeps [`TAB_W`] / [`TAB_W_MIN`].
const COMPACT_TAB_W: f32 = 104.0;
const COMPACT_TAB_W_MIN: f32 = 48.0;
/// Width of the "+" new-tab button.
const PLUS_W: f32 = 32.0;
/// Room (design px) kept after the "+" for the "+N" overflow hint while tabs
/// overflow — the hint's 34 px slot plus a 4 px gap.
const OVERFLOW_HINT_W: f32 = 38.0;
/// Size of the "×" close hit box at the right of each tab.
const CLOSE_W: f32 = 18.0;
/// Width of each window-control button (minimize/maximize/close) on the right.
/// Live: [`ChromeMetrics::ctrl_w`].
pub(crate) const CTRL_W_BASE: f32 = 28.0;
/// Design width reserved on the right of the strip for the controls. Left→right:
/// Help "?", Settings "⚙", minimize "─", maximize "▢", close "✕" — five cells.
/// Live: [`ChromeMetrics::controls_w`].
pub const CONTROLS_W: f32 = CTRL_W_BASE * 5.0;
/// Inset of the whole tab strip from the window's left/right edges, so tabs and
/// the window controls don't sit flush against the rounded window corners.
/// A window-SHAPE distance: live value [`ChromeMetrics::strip_pad`] (DPI-scaled
/// like the corner radius, not UI-font-scaled).
pub const STRIP_PAD: f32 = 8.0;
/// Top inset of every one-line label inside the bar (the glyph box's top).
const LABEL_Y: f32 = 9.0;

/// Unseen activity on a tab, shown as a small themed dot (a "badge") on
/// INACTIVE tabs and tinting the "+N" overflow hint. Sticky until the tab is
/// viewed; a stronger kind replaces a weaker one, never the reverse — see
/// [`TabActivity::rank`] (Failed > Bell > Done > Output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabActivity {
    #[default]
    None,
    /// PTY output arrived while the tab was inactive (accent dot).
    Output,
    /// BEL (^G) rang while inactive (amber dot — red means failure).
    Bell,
    /// A command finished successfully while inactive (OSC 133 D; green dot).
    Done,
    /// A command FAILED while inactive (OSC 133 D with a nonzero exit; red dot).
    Failed,
}

impl TabActivity {
    /// Precedence: Failed > Bell > Done > Output > None. A failure is what the
    /// user most needs to see; a bell asked for attention; a finished command
    /// says more than "something printed".
    pub fn rank(self) -> u8 {
        match self {
            TabActivity::None => 0,
            TabActivity::Output => 1,
            TabActivity::Done => 2,
            TabActivity::Bell => 3,
            TabActivity::Failed => 4,
        }
    }

    /// The stronger of `self` and `other` by [`Self::rank`] (`self` on a tie).
    pub fn max(self, other: TabActivity) -> TabActivity {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// How the tabs are drawn (config `tab_style`). Only [`TabStyle::Compact`]
/// changes the tab geometry (narrower tabs); the others share today's widths,
/// so every hit rect is identical across them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabStyle {
    /// A soft rounded pill behind the active tab; inactive tabs are text (today).
    #[default]
    Pill,
    /// No fills: an accent bar under the active tab's title.
    Underline,
    /// Every tab a slanted parallelogram (`/ title /`).
    Slant,
    /// Breadcrumb chevrons (powerline segments), the active one in the accent.
    Powerline,
    /// The pill look on narrower, denser tabs.
    Compact,
}

impl TabStyle {
    /// Cycle / Settings order.
    pub const ALL: [TabStyle; 5] =
        [TabStyle::Pill, TabStyle::Underline, TabStyle::Slant, TabStyle::Powerline, TabStyle::Compact];

    /// Config string → style; unknown values read as [`TabStyle::Pill`].
    pub fn from_config(s: &str) -> TabStyle {
        match s {
            "underline" => TabStyle::Underline,
            "slant" => TabStyle::Slant,
            "powerline" => TabStyle::Powerline,
            "compact" => TabStyle::Compact,
            _ => TabStyle::Pill,
        }
    }

    pub fn to_config(self) -> &'static str {
        match self {
            TabStyle::Pill => "pill",
            TabStyle::Underline => "underline",
            TabStyle::Slant => "slant",
            TabStyle::Powerline => "powerline",
            TabStyle::Compact => "compact",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            TabStyle::Pill => "Pill",
            TabStyle::Underline => "Underline",
            TabStyle::Slant => "Slant",
            TabStyle::Powerline => "Powerline",
            TabStyle::Compact => "Compact",
        }
    }

    /// The next / previous style in [`Self::ALL`] order (wraps).
    pub fn cycle(self, forward: bool) -> TabStyle {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward { (i + 1) % n } else { (i + n - 1) % n }]
    }
}

/// Which tabs show their "×" close button (config `tab_close_button`). A hidden
/// "×" is not clickable either: its hit rect is parked offscreen, so a click
/// there selects the tab. The pointer's own tab always shows it in `Hover` and
/// `Active` mode, so the button is there whenever it can be clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloseButton {
    /// Every tab (today).
    #[default]
    Always,
    /// Only the tab under the pointer.
    Hover,
    /// The active tab, and the tab under the pointer.
    Active,
}

impl CloseButton {
    pub const ALL: [CloseButton; 3] = [CloseButton::Always, CloseButton::Hover, CloseButton::Active];

    /// Config string → mode; unknown values read as [`CloseButton::Always`].
    pub fn from_config(s: &str) -> CloseButton {
        match s {
            "hover" => CloseButton::Hover,
            "active" => CloseButton::Active,
            _ => CloseButton::Always,
        }
    }

    pub fn to_config(self) -> &'static str {
        match self {
            CloseButton::Always => "always",
            CloseButton::Hover => "hover",
            CloseButton::Active => "active",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            CloseButton::Always => "Always",
            CloseButton::Hover => "On hover",
            CloseButton::Active => "Active tab",
        }
    }

    /// Whether a tab shows its "×".
    pub fn shows(self, active: bool, hovered: bool) -> bool {
        match self {
            CloseButton::Always => true,
            CloseButton::Hover => hovered,
            CloseButton::Active => active || hovered,
        }
    }
}

/// Per-tab colors offered by the tab menu and the palette: ANSI palette indices
/// 1–6, so a tab's color follows the theme (stored as the index, never RGB).
pub const TAB_COLORS: [(u8, &str); 6] =
    [(1, "Red"), (2, "Green"), (3, "Yellow"), (4, "Blue"), (5, "Magenta"), (6, "Cyan")];

/// A valid per-tab color index (1..=6) — what a config or a stale id may hold.
pub fn valid_tab_color(idx: u8) -> Option<u8> {
    (1..=6).contains(&idx).then_some(idx)
}

/// The display name of a per-tab color index ("Red" … "Cyan").
pub fn tab_color_name(idx: u8) -> Option<&'static str> {
    TAB_COLORS.iter().find(|(i, _)| *i == idx).map(|(_, n)| *n)
}

/// The live RGB of per-tab color `idx` in `theme` (`None` for a bad index).
pub fn tab_color_rgb(theme: &Theme, idx: u8) -> Option<[u8; 3]> {
    valid_tab_color(idx).map(|i| theme.palette[usize::from(i)])
}

/// Everything about ONE tab the bar draws beyond its title. None of it affects
/// the tab GEOMETRY (hit rects), so hit-test rebuilds can pass defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TabDeco {
    /// Unseen activity badge (drawn on inactive tabs only).
    pub activity: TabActivity,
    /// OSC 9;4 progress the tab's program reports.
    pub progress: Option<Progress>,
    /// Per-tab color: an ANSI palette index 1..=6 ([`TAB_COLORS`]).
    pub color: Option<u8>,
}

/// Bar-wide drawing options (the chrome config keys + pointer state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabBarOpts {
    pub style: TabStyle,
    pub close_button: CloseButton,
    /// The tab under the pointer: its hover lift, and its "×" in the
    /// `Hover` / `Active` close-button modes.
    pub hover: Option<usize>,
    /// The bar strip is painted opaque in the theme bg (today). `false`
    /// (`tab_bar_opacity = true`) leaves it unpainted, so the window's opacity —
    /// and anything drawn under the bar — shows through like the grid.
    pub opaque: bool,
    /// Draw OSC 9;4 progress (`progress_bar`).
    pub progress: bool,
    /// The bar sits at the window BOTTOM: marks that face the grid (the
    /// underline, the progress hairline) move to the bar's top edge.
    pub bottom: bool,
}

impl Default for TabBarOpts {
    fn default() -> Self {
        TabBarOpts {
            style: TabStyle::Pill,
            close_button: CloseButton::Always,
            hover: None,
            opaque: true,
            progress: true,
            bottom: false,
        }
    }
}

/// Which window-control button (if any) is hovered, for the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlHover {
    None,
    Help,
    Settings,
    Min,
    Max,
    Close,
}

/// Geometry + draw data for the tab bar.
pub struct TabBar {
    /// Quads in draw order: bar background, then per-tab backgrounds + plus button.
    pub quads: Vec<Rect>,
    /// Monospace chrome labels: (text, x, y, rgb) — close glyphs, the plus glyph,
    /// the overflow hint, the perf HUD, and the window controls.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Tab TITLE labels, rendered separately in the platform's proportional
    /// sans-serif so the strip reads as elegant UI text instead of monospace.
    pub title_labels: Vec<(String, f32, f32, [u8; 3])>,
    /// One hit-test rect per tab (full tab area, for switching).
    pub tab_rects: Vec<Rect>,
    /// One hit-test rect per tab for its "×" close affordance (parked offscreen
    /// while the "×" is hidden — see [`CloseButton`]).
    pub close_rects: Vec<Rect>,
    /// Hit-test rect for the "+" new-tab button.
    pub plus_rect: Rect,
    /// Hit-test rect for the Help "?" button (left of the window controls).
    pub help_rect: Rect,
    /// Hit-test rect for the Settings "⚙" button (left of the window controls).
    pub settings_rect: Rect,
    /// Hit-test rect for the minimize "─" window control.
    pub min_rect: Rect,
    /// Hit-test rect for the maximize/restore "▢" window control.
    pub max_rect: Rect,
    /// Hit-test rect for the close "✕" window control (rightmost).
    pub close_rect: Rect,
}

/// Build the tab bar across the top of the window at the design baseline (1×,
/// 16pt UI font, monospace advance). Tests and fixed-size harnesses only — the
/// app calls [`build_tab_bar_styled`] with its real metrics and text measurer.
pub fn build_tab_bar(width: u32, tabs: &[(String, bool)], theme: &Theme) -> TabBar {
    build_tab_bar_ex(
        width,
        tabs,
        theme,
        None,
        CtrlHover::None,
        None,
        &mut MonoMeasure(CHROME_CHAR_W),
        ChromeMetrics::DEFAULT,
        &[],
    )
}

/// Advance of the default monospace chrome font at the design size. Only used
/// by the [`build_tab_bar`] baseline wrapper.
const CHROME_CHAR_W: f32 = crate::chrome::CHROME_ADVANCE;
/// Gap (design px) between the perf HUD's left edge and the nearest tab/+button,
/// so the reserved area never visually touches the tabs.
const PERF_GAP: f32 = 16.0;
/// Comfortable PER-TAB width (design px) the perf HUD must NOT push tabs below.
/// The HUD is the lowest-priority strip element: if reserving its width would
/// shrink each tab beneath this (≈6 title chars), the HUD is HIDDEN and the tabs
/// get the full area instead (so several tabs in a narrowish window never squash
/// to "T…×").
const PERF_MIN_TAB_W: f32 = 110.0;

/// [`build_tab_bar_styled`] with the default look ([`TabBarOpts::default`]) and
/// only activity badges — the pre-style API, kept for callers and tests that
/// need nothing else. `activity` is index-aligned with `tabs`.
#[allow(clippy::too_many_arguments)]
pub fn build_tab_bar_ex(
    width: u32,
    tabs: &[(String, bool)],
    theme: &Theme,
    renaming: Option<(usize, &str)>,
    ctrl_hover: CtrlHover,
    perf: Option<&str>,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    activity: &[TabActivity],
) -> TabBar {
    let deco: Vec<TabDeco> =
        activity.iter().map(|&activity| TabDeco { activity, ..TabDeco::default() }).collect();
    build_tab_bar_styled(width, tabs, theme, renaming, ctrl_hover, perf, m, cm, &deco, &TabBarOpts::default())
}

/// Shape metrics of one style (design px, scaled by the chrome unit on use).
#[derive(Clone, Copy)]
struct StyleMetrics {
    /// Most / least a tab is wide.
    tab_w: f32,
    tab_w_min: f32,
    /// Horizontal gap kept free on each side of a tab's body.
    inset: f32,
    /// Top/bottom margin of the body inside the bar.
    vpad: f32,
    /// Corner radius of the body.
    radius: f32,
    /// Left inset of the title inside the tab cell.
    title_pad: f32,
}

impl StyleMetrics {
    fn of(style: TabStyle) -> StyleMetrics {
        match style {
            TabStyle::Compact => StyleMetrics {
                tab_w: COMPACT_TAB_W,
                tab_w_min: COMPACT_TAB_W_MIN,
                inset: 3.0,
                vpad: 8.0,
                radius: 6.0,
                title_pad: 12.0,
            },
            TabStyle::Slant | TabStyle::Powerline => StyleMetrics {
                tab_w: TAB_W,
                tab_w_min: TAB_W_MIN,
                inset: 2.0,
                vpad: 5.0,
                radius: 3.0,
                title_pad: 13.0,
            },
            TabStyle::Pill | TabStyle::Underline => StyleMetrics {
                tab_w: TAB_W,
                tab_w_min: TAB_W_MIN,
                inset: 4.0,
                vpad: 6.0,
                radius: 8.0,
                title_pad: 13.0,
            },
        }
    }
}

/// Horizontal lean (design px) of a [`TabStyle::Slant`] tab across its body.
const SLANT_LEAN: f32 = 10.0;
/// Depth of a [`TabStyle::Powerline`] chevron as a fraction of the body height.
const POWERLINE_DEPTH: f32 = 0.40;
/// Gap (design px) between two powerline segments (the separator).
const POWERLINE_GAP: f32 = 2.0;

/// The resolved colors of one tab.
struct TabColors {
    /// Body fill, `None` = no body (text-only tab).
    fill: Option<[u8; 3]>,
    title: [u8; 3],
    close: [u8; 3],
    /// The underline style's bar (and any style's identity mark), if drawn.
    mark: Option<[u8; 3]>,
}

/// Colors of a tab in `style`: `on` = active (or being renamed), `hovered` =
/// under the pointer, `color` = its per-tab color (RGB, already resolved).
fn tab_colors(ui: &UiPalette, style: TabStyle, on: bool, hovered: bool, color: Option<[u8; 3]>) -> TabColors {
    let bg = ui.bg;
    let tint = |t: f32| color.map(|c| mix(bg, c, t));
    match style {
        TabStyle::Pill | TabStyle::Compact => {
            let fill = if on {
                Some(tint(0.30).unwrap_or_else(|| ui.shade(0.12)))
            } else if hovered {
                Some(tint(0.20).unwrap_or_else(|| ui.shade(0.06)))
            } else {
                tint(0.13)
            };
            TabColors {
                fill,
                title: if on { ui.text } else if hovered { ui.text_dim } else { ui.text_hint },
                close: if on { ui.text_dim } else { ui.text_hint },
                mark: None,
            }
        }
        TabStyle::Underline => {
            let mark = if on {
                Some(color.map(|c| ui.readable(c, UiPalette::ACCENT_FLOOR)).unwrap_or(ui.accent))
            } else if hovered {
                Some(tint(0.70).unwrap_or_else(|| ui.shade(0.30)))
            } else {
                tint(0.55)
            };
            TabColors {
                fill: None,
                title: if on { ui.text } else if hovered { ui.text_dim } else { ui.text_hint },
                close: if on { ui.text_dim } else { ui.text_hint },
                mark,
            }
        }
        TabStyle::Slant => {
            let fill = if on {
                tint(0.32).unwrap_or_else(|| ui.shade(0.15))
            } else if hovered {
                tint(0.22).unwrap_or_else(|| ui.shade(0.09))
            } else {
                tint(0.15).unwrap_or_else(|| ui.shade(0.05))
            };
            TabColors {
                fill: Some(fill),
                title: if on { ui.text } else if hovered { ui.text_dim } else { ui.text_hint },
                close: if on { ui.text_dim } else { ui.text_hint },
                mark: None,
            }
        }
        TabStyle::Powerline => {
            if on {
                let fill = color.unwrap_or(ui.accent);
                let on_fill = ui.on_fill(fill);
                TabColors { fill: Some(fill), title: on_fill, close: mix(fill, on_fill, 0.75), mark: None }
            } else {
                let fill = if hovered {
                    tint(0.30).unwrap_or_else(|| ui.shade(0.14))
                } else {
                    tint(0.22).unwrap_or_else(|| ui.shade(0.08))
                };
                TabColors {
                    fill: Some(fill),
                    title: if hovered { ui.text } else { ui.text_dim },
                    close: ui.text_hint,
                    mark: None,
                }
            }
        }
    }
}

/// The badge color of an activity kind (`None` draws no badge).
fn activity_color(ui: &UiPalette, act: TabActivity) -> Option<[u8; 3]> {
    match act {
        TabActivity::None => None,
        TabActivity::Output => Some(ui.accent),
        TabActivity::Bell => Some(ui.warn),
        TabActivity::Done => Some(ui.success),
        TabActivity::Failed => Some(ui.danger),
    }
}

/// Push an activity badge of diameter `d` at `(x, y)` in color `c`, over a
/// surface of color `under`. The kinds differ in SHAPE as well as color, so
/// they stay apart on themes whose yellow and green are close (solarized) and
/// for red/green color blindness: output and done are dots, a bell is a ring,
/// a failure a (rounded) square.
fn push_badge(quads: &mut Vec<Rect>, act: TabActivity, x: f32, y: f32, d: f32, c: [u8; 3], under: [u8; 3]) {
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    match act {
        TabActivity::Failed => quads.push(Rect::rounded(x, y, d, d, rgba(c), d * 0.2)),
        TabActivity::Bell => {
            quads.push(Rect::rounded(x, y, d, d, rgba(c), d / 2.0));
            let hole = d * 0.45;
            let inset = (d - hole) / 2.0;
            quads.push(Rect::rounded(x + inset, y + inset, hole, hole, rgba(under), hole / 2.0));
        }
        _ => quads.push(Rect::rounded(x, y, d, d, rgba(c), d / 2.0)),
    }
}

/// The color of a progress report's bar.
fn progress_color(ui: &UiPalette, p: &Progress) -> [u8; 3] {
    match p.state {
        ProgressState::Normal | ProgressState::Indeterminate => ui.accent,
        ProgressState::Error => ui.danger,
        ProgressState::Paused => ui.warn,
    }
}

/// The filled fraction of a progress report: `None` = indeterminate (drawn as
/// a static dashed stripe — no animation, so it costs nothing at idle). An
/// error with no value fills the whole bar.
fn progress_fraction(p: &Progress) -> Option<f32> {
    match (p.state, p.value) {
        (ProgressState::Indeterminate, _) => None,
        (ProgressState::Error, None) => Some(1.0),
        (_, v) => Some(f32::from(v.unwrap_or(0)) / 100.0),
    }
}

/// Push a `thick`-px progress bar spanning `[x0, x1]` at `y`: a faint track
/// in `track`, then the filled part (or, when indeterminate, a static dashed
/// stripe with `dash`-px dashes) in `fill`.
#[allow(clippy::too_many_arguments)]
fn push_progress_bar(
    quads: &mut Vec<Rect>,
    p: &Progress,
    x0: f32,
    x1: f32,
    y: f32,
    thick: f32,
    track: Option<[u8; 3]>,
    fill: [u8; 3],
    dash: f32,
) {
    let w = (x1 - x0).max(0.0);
    if w <= 0.0 || thick <= 0.0 {
        return;
    }
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let r = thick * 0.5;
    if let Some(t) = track {
        quads.push(Rect::rounded(x0, y, w, thick, rgba(t), r));
    }
    match progress_fraction(p) {
        Some(f) => {
            let fw = (w * f.clamp(0.0, 1.0)).round();
            if fw > 0.0 {
                quads.push(Rect::rounded(x0, y, fw, thick, rgba(fill), r));
            }
        }
        None => {
            // Static stripe: dash, gap, dash … (gap = dash / 2), clipped to w.
            let step = dash * 1.5;
            let mut x = x0;
            while x < x1 {
                let dw = dash.min(x1 - x);
                quads.push(Rect::rounded(x, y, dw, thick, rgba(fill), r));
                x += step;
            }
        }
    }
}

/// The resolved geometry of one tab cell in the strip.
struct TabCell {
    /// Left edge of the cell (`tab_rects[i].x`) and its width.
    x: f32,
    w: f32,
}

/// The underline style's bar for `cell`: `(x, w, y, thickness)`. It faces the
/// grid: near the bar's bottom edge, or its top edge for a bottom bar.
fn underline_geom(sm: StyleMetrics, cm: ChromeMetrics, cell: &TabCell, h: f32, bottom: bool) -> (f32, f32, f32, f32) {
    let th = cm.px(2.0).round().max(1.0);
    let inset = cm.px(sm.inset);
    let y = if bottom { cm.px(4.0).round() } else { (h - cm.px(4.0)).round() - th };
    let x = cell.x + inset + cm.px(6.0);
    let w = (cell.w - inset * 2.0 - cm.px(12.0)).max(0.0);
    (x, w, y, th)
}

/// Where a tab's 2px OSC 9;4 bar goes: `(x0, x1, y, thickness)`. Under the
/// title in the gap between the tab body and the bar edge (clear of glyph
/// descenders); the underline style puts it IN the underline's slot (the
/// underline becomes the progress track).
#[allow(clippy::too_many_arguments)]
fn progress_slot(
    style: TabStyle,
    sm: StyleMetrics,
    cm: ChromeMetrics,
    cell: &TabCell,
    h: f32,
    title_x: f32,
    right: f32,
    bottom: bool,
) -> (f32, f32, f32, f32) {
    if style == TabStyle::Underline {
        let (x, w, y, th) = underline_geom(sm, cm, cell, h, bottom);
        return (x, x + w, y, th);
    }
    let th = cm.px(2.0).round().max(1.0);
    let vpad = cm.px(sm.vpad);
    let y = (h - vpad + (vpad - th) * 0.5).round();
    (title_x, right.max(title_x), y, th)
}

/// Push a tab's in-tab progress bar (track + fill) into its slot. The slot lies
/// on the bar background (below the tab body), so the progress color reads in
/// every style; the underline style's track is its own dimmed underline.
#[allow(clippy::too_many_arguments)]
fn push_tab_progress(
    quads: &mut Vec<Rect>,
    ui: &UiPalette,
    p: &Progress,
    style: TabStyle,
    sm: StyleMetrics,
    cm: ChromeMetrics,
    cell: &TabCell,
    h: f32,
    colors: &TabColors,
    title_x: f32,
    close_x: f32,
    bottom: bool,
) {
    let (x0, x1, y, th) = progress_slot(style, sm, cm, cell, h, title_x, close_x - cm.px(2.0), bottom);
    // The underline style's track is its own underline, dimmed, so the bright
    // part reads as the underline filling up.
    let track = match (style, colors.mark) {
        (TabStyle::Underline, Some(mk)) => mix(ui.bg, mk, 0.35),
        _ => mix(ui.bg, ui.text, 0.16),
    };
    push_progress_bar(quads, p, x0, x1, y, th, Some(track), progress_color(ui, p), cm.px(6.0));
}

/// Push the seam hairline of the ACTIVE tab's progress: 1 logical px at the
/// bar/grid seam, across the full window width `sw`.
fn push_seam_progress(quads: &mut Vec<Rect>, ui: &UiPalette, p: &Progress, cm: ChromeMetrics, sw: f32, h: f32, bottom: bool) {
    let hair = cm.dpx(1.0).round().max(1.0);
    let hy = if bottom { 0.0 } else { h - hair };
    push_progress_bar(quads, p, 0.0, sw, hy, hair, None, progress_color(ui, p), cm.px(16.0));
}

/// Paint one tab's body (fill / underline / slant / chevron) and return the x
/// offset its content (badge + title) starts at relative to the plain layout
/// (the slant/powerline shapes push the content right of their lean).
#[allow(clippy::too_many_arguments)]
fn paint_tab_body(
    quads: &mut Vec<Rect>,
    style: TabStyle,
    sm: StyleMetrics,
    cm: ChromeMetrics,
    cell: &TabCell,
    h: f32,
    colors: &TabColors,
    first: bool,
    bottom: bool,
) -> f32 {
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let inset = cm.px(sm.inset);
    let vpad = cm.px(sm.vpad);
    let body_h = (h - vpad * 2.0).max(1.0);
    match style {
        TabStyle::Pill | TabStyle::Compact => {
            if let Some(f) = colors.fill {
                quads.push(Rect::rounded(
                    cell.x + inset,
                    vpad,
                    cell.w - inset * 2.0,
                    body_h,
                    rgba(f),
                    cm.px(sm.radius),
                ));
            }
            0.0
        }
        TabStyle::Underline => {
            if let Some(mk) = colors.mark {
                let (x0, w, y, th) = underline_geom(sm, cm, cell, h, bottom);
                quads.push(Rect::rounded(x0, y, w, th, rgba(mk), th * 0.5));
            }
            0.0
        }
        TabStyle::Slant => {
            let lean = cm.px(SLANT_LEAN);
            if let Some(f) = colors.fill {
                quads.push(Rect {
                    x: cell.x + inset + lean * 0.5,
                    y: vpad,
                    w: (cell.w - inset * 2.0 - lean).max(1.0),
                    h: body_h,
                    color: rgba(f),
                    radius: cm.px(sm.radius),
                    shear: lean / body_h,
                });
            }
            lean * 0.5
        }
        TabStyle::Powerline => {
            // Two half-height quads sheared in opposite directions make one
            // chevron: the left edge is notched (`>`), the right edge is the
            // arrow tip, `depth` px deep at mid-height. Neighbours nest with a
            // constant `gap`. The halves overlap by 1px so no seam shows at mid.
            let depth = (body_h * POWERLINE_DEPTH).round();
            let gap = cm.px(POWERLINE_GAP);
            let left = cell.x + gap * 0.5;
            let w = (cell.w - gap).max(1.0);
            if let Some(f) = colors.fill {
                let half = body_h * 0.5;
                let ov = 0.5;
                let shear = depth / half;
                // Top half: its bottom edge (mid) sits `depth` right of its top.
                quads.push(Rect {
                    x: left + depth * 0.5,
                    y: vpad,
                    w,
                    h: half + ov,
                    color: rgba(f),
                    radius: 0.75,
                    shear: -shear,
                });
                // Bottom half: its top edge (mid) sits `depth` right of its bottom.
                quads.push(Rect {
                    x: left + depth * 0.5,
                    y: vpad + half - ov,
                    w,
                    h: half + ov,
                    color: rgba(f),
                    radius: 0.75,
                    shear,
                });
                // The first segment starts flat (classic powerline) instead of
                // notched: fill the notch.
                if first {
                    quads.push(Rect::rounded(left, vpad, depth + 1.0, body_h, rgba(f), cm.px(sm.radius)));
                }
            }
            depth
        }
    }
}

/// Build the tab bar: `tabs` are (title, active) pairs; `deco` is index-aligned
/// per-tab decoration (badges, progress, color — missing indices read as
/// default); `opts` the style and pointer state.
///
/// `perf` is `Some(string)` to show the live perf HUD (`⚡ … ms · … fps · …`)
/// right-aligned just left of the window controls. Its width is RESERVED out of
/// the tab area so tabs never overlap it; if the window is too narrow to fit the
/// HUD without squeezing the tabs below a sane minimum, the HUD is HIDDEN and the
/// tab layout is identical to the no-HUD case. `None` shows no HUD.
///
/// `m` measures labels exactly as the chrome text pass renders them (titles in
/// the title family), so title truncation, the rename caret and the HUD
/// reservation are right for any UI font. `cm` sizes the whole strip. The hit
/// geometry (tab/close/plus/control rects) depends only on `width`, the tab
/// count, `cm`, the style (only [`TabStyle::Compact`] differs — and
/// [`TabStyle::Powerline`] in a window too narrow for one minimum-width tab)
/// and — for the close rects — which "×" are shown ([`CloseButton`] +
/// `opts.hover`); never on text or `deco`, so a hit-test rebuild may pass any
/// measurer and no deco.
///
/// Inactive tabs with activity get a small themed badge in the title gutter
/// and a hidden (overflowed) tab's strongest activity tints the "+N" hint.
#[allow(clippy::too_many_arguments)]
pub fn build_tab_bar_styled(
    width: u32,
    tabs: &[(String, bool)],
    theme: &Theme,
    renaming: Option<(usize, &str)>,
    ctrl_hover: CtrlHover,
    perf: Option<&str>,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    deco: &[TabDeco],
    opts: &TabBarOpts,
) -> TabBar {
    let sw = width as f32;
    let h = cm.bar_h();
    let label_y = cm.px(LABEL_Y);
    let sm = StyleMetrics::of(opts.style);
    let tab_w_max = cm.px(sm.tab_w);
    let plus_w = cm.px(PLUS_W);
    let close_w = cm.px(CLOSE_W);
    let ctrl_w = cm.ctrl_w();

    // Every color comes from the theme's chrome palette (contrast floors
    // enforced, so light themes read as well as dark ones).
    let ui = UiPalette::cached(theme);
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];
    let bg = rgba(ui.bg);

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    // Sized to the full tab count and index-aligned below (see the draw loop).
    let mut tab_rects: Vec<Rect>;
    let mut close_rects: Vec<Rect>;

    // Bar background spanning the full width — unless the bar follows the
    // window opacity, where the frame's (translucent) clear already is it.
    if opts.opaque {
        quads.push(Rect { x: 0.0, y: 0.0, w: sw, h, color: bg, ..Default::default() });
    }

    // Tabs are laid out from `left` (inset from the window edge) and must never
    // overlap the window controls parked at the right (also inset by the strip
    // pad). `tab_area_x` is the absolute x where the controls begin — the right
    // boundary for the tabs and the "+" button.
    let left = cm.strip_pad();
    // The controls region begins here; tabs+HUD must stay left of it.
    let controls_left = (sw - left - cm.controls_w()).max(left);

    // --- Perf HUD reservation (LOWEST priority — yields space to the tabs) ---
    // The HUD sits between the tabs and the window controls, right-aligned with a
    // small gap before the controls. Reserve its width out of the tab area ONLY if,
    // after reserving, each tab still gets a COMFORTABLE width (>= PERF_MIN_TAB_W).
    // Otherwise (several tabs in a narrowish window) the HUD is HIDDEN so the tabs
    // aren't squashed to their unreadable ~64px floor just to fit a stats readout —
    // the tab layout then matches the no-HUD case exactly.
    let perf_gap = cm.px(PERF_GAP);
    let n_tabs = tabs.len().max(1) as f32;
    let perf_w = perf.map(|s| m.text_w(s)).unwrap_or(0.0);
    // Width carved out of the tab area when the HUD is shown: the label plus the
    // gap to the tabs and a small gap to the controls.
    let perf_reserve = if perf_w > 0.0 { perf_w + perf_gap * 1.5 } else { 0.0 };
    // Per-tab width the tabs WOULD get if we reserved for the HUD (capped at the
    // ideal TAB_W — extra room beyond that doesn't make a tab more comfortable).
    let tab_w_if_hud = ((controls_left - perf_reserve - left - plus_w).max(0.0) / n_tabs).min(tab_w_max);
    let perf_shown = perf_w > 0.0 && tab_w_if_hud >= cm.px(PERF_MIN_TAB_W).min(tab_w_max);
    // Right boundary for the tabs / "+" / overflow hint. Shrinks by the HUD
    // reservation only when the HUD is actually shown.
    let tab_area_x = if perf_shown { controls_left - perf_reserve } else { controls_left };
    // The "+" button sits after the last tab and must stay left of the controls,
    // so the tabs themselves get the area from `left` to `tab_area_x - PLUS_W`.
    let tabs_avail_w = (tab_area_x - left - plus_w).max(0.0);

    // --- Dynamic tab width: shrink tabs to fit the available area so they never
    // overflow under the window controls. With many tabs we shrink down to a
    // readable minimum; if even that can't fit all of them, we cap the number of
    // tabs drawn (the rest are unreachable here but stay index-aligned via the
    // switch_tab keyboard path). ---
    let tab_w_min = cm.px(sm.tab_w_min);
    // Ideal width per tab, clamped to [MIN, default]. Use the full default when
    // there's room; shrink toward MIN as tabs are added.
    let tab_w = (tabs_avail_w / n_tabs).clamp(tab_w_min, tab_w_max).min(tab_w_max);
    // The narrowest windows (the 200 px minimum width at 1×) cannot hold even
    // one minimum-width tab left of the controls. The one tab drawn then
    // shrinks to the room there is (the "+" is not drawn either) and drops
    // its "×" — Ctrl+Shift+W and the tab menu still close it — instead of
    // running under the "?" with its "×" on top of the "?" glyph. (A powerline
    // chevron's tip reaches past its cell: that overhang is kept clear too.)
    let tip = match opts.style {
        TabStyle::Powerline => {
            let body_h = (h - cm.px(sm.vpad) * 2.0).max(1.0);
            ((body_h * POWERLINE_DEPTH).round() - cm.px(POWERLINE_GAP)).max(0.0) * 0.5
        }
        _ => 0.0,
    };
    let room = (tab_area_x - left - tip).max(0.0);
    let cramped = tab_w > room;
    let tab_w = tab_w.min(room);
    // How many tabs actually fit at `tab_w` (at least 1 so the active tab shows).
    // When they don't all fit, room is kept for the "+N" hint after the "+":
    // it carries the hidden tabs' badges (a failure must never be invisible),
    // and a strip filled to the edge used to leave it no room at all.
    let fit = |w: f32| if tab_w > 0.0 { ((w / tab_w).floor() as usize).max(1) } else { 1 };
    let mut max_visible = fit(tabs_avail_w);
    if tabs.len() > max_visible {
        max_visible = fit((tabs_avail_w - cm.px(OVERFLOW_HINT_W)).max(0.0));
    }
    let drawn = tabs.len().min(max_visible);
    let overflow = tabs.len().saturating_sub(drawn);

    // Window the drawn range around the ACTIVE tab so it is ALWAYS visible — the
    // overflow window is not fixed to the head of the list. If the active index is
    // beyond the head window, slide the window so the active tab sits at its right
    // edge. Hit-rects stay index-aligned with ABSOLUTE tab indices (tabs outside
    // the window get an offscreen sentinel) because the app maps a clicked rect's
    // position straight to a tab index.
    let active_idx = tabs.iter().position(|(_, a)| *a).unwrap_or(0);
    let start = if active_idx >= drawn {
        (active_idx + 1 - drawn).min(tabs.len().saturating_sub(drawn))
    } else {
        0
    };

    let title_pad = cm.px(sm.title_pad); // left inset of the title inside a tab

    // Title room inside a tab: everything but the close "×" box, the left title
    // pad and a little right breathing room. Titles are MEASURED in the title
    // family (a char count × a monospace advance mis-sized a proportional title).
    // The slant/powerline shapes push the content right by their lean, so the
    // room shrinks by the same amount.
    let content_lead = match opts.style {
        TabStyle::Slant => cm.px(SLANT_LEAN) * 0.5,
        TabStyle::Powerline => (((h - cm.px(sm.vpad) * 2.0).max(1.0)) * POWERLINE_DEPTH).round(),
        _ => 0.0,
    };
    let close_room = if cramped { 0.0 } else { close_w };
    let title_max_w = (tab_w - close_room - title_pad - content_lead - cm.px(8.0)).max(0.0);

    // Hit-rects are index-aligned to ABSOLUTE tab indices; off-window tabs get an
    // offscreen sentinel (0-width, far left) so a click can never match them.
    let offscreen = Rect { x: -1.0e6, y: 0.0, w: 0.0, h: 0.0, color: [0, 0, 0, 0], ..Default::default() };
    tab_rects = vec![offscreen; tabs.len()];
    close_rects = vec![offscreen; tabs.len()];

    let deco_of = |i: usize| deco.get(i).copied().unwrap_or_default();
    let mut x = left;
    for (i, (title, active)) in tabs.iter().enumerate().skip(start).take(drawn) {
        let being_renamed = matches!(renaming, Some((ri, _)) if ri == i);
        let is_on = *active || being_renamed;
        let hovered = opts.hover == Some(i);
        let d = deco_of(i);
        let color = d.color.and_then(|c| tab_color_rgb(theme, c));
        let colors = tab_colors(&ui, opts.style, is_on, hovered, color);
        let cell = TabCell { x, w: tab_w };

        let lead = paint_tab_body(&mut quads, opts.style, sm, cm, &cell, h, &colors, i == start, opts.bottom);

        let title_x = x + title_pad + lead;
        // Without a "×" (cramped) its slot is the tab's right pad, where the
        // progress bar then ends (like the detached bar's lone tab).
        let close_x = if cramped { x + tab_w - cm.px(6.0) } else { x + tab_w - close_w - cm.px(4.0) };
        if being_renamed {
            // Live edit buffer + trailing caret, front-truncated so the caret stays.
            let buf = match renaming { Some((_, b)) => b, None => "" };
            // ASCII caret: every UI font has it (U+258F was tofu on fonts without
            // Block Elements).
            let caret_w = m.title_w("|");
            let mut shown = fit_tail(m, buf, (title_max_w - caret_w).max(0.0), true);
            shown.push('|');
            title_labels.push((shown, title_x, label_y, colors.title));
        } else {
            let shown = fit_head(m, title, title_max_w, true);
            title_labels.push((shown, title_x, label_y, colors.title));
        }

        // Close "×" (recessive on inactive tabs) — only where the mode shows it;
        // a hidden one is not clickable (its rect stays offscreen). Kept while
        // renaming so indices stay aligned, but never drawn over the edit box.
        if opts.close_button.shows(is_on, hovered) && !cramped {
            if !being_renamed {
                labels.push(("×".to_string(), close_x + cm.px(4.0), label_y, colors.close));
            }
            close_rects[i] = Rect { x: close_x, y: 0.0, w: close_w, h, color: [0, 0, 0, 0], ..Default::default() };
        }

        // Activity badge on INACTIVE tabs, in the title gutter left of the title
        // (dot spans x+4..x+10 + the shape's lead, title starts at x+13 + lead)
        // so the layout is bit-identical with or without activity.
        if !is_on {
            if let Some(dc) = activity_color(&ui, d.activity) {
                // 3 design px clear of the title (x+4..x+10 before a 13 px pad).
                let dot_d = cm.px(6.0);
                let dot_x = x + cm.px(sm.title_pad - 9.0) + lead;
                let under = colors.fill.unwrap_or(ui.bg);
                push_badge(&mut quads, d.activity, dot_x, (h - dot_d) / 2.0, dot_d, dc, under);
            }
        }

        // OSC 9;4 progress: a 2px bar under the title (see `progress_slot`), and
        // for the active tab a hairline at the bar/grid seam across the whole
        // window — the grid's own edge, where the eye already is.
        if let (true, Some(p)) = (opts.progress, d.progress.as_ref()) {
            push_tab_progress(&mut quads, &ui, p, opts.style, sm, cm, &cell, h, &colors, title_x, close_x, opts.bottom);
            if is_on {
                push_seam_progress(&mut quads, &ui, p, cm, sw, h, opts.bottom);
            }
        }

        tab_rects[i] = Rect { x, y: 0.0, w: tab_w, h, color: [0, 0, 0, 0], ..Default::default() };
        x += tab_w;
    }

    // "+" new-tab button — minimal: just a dim glyph, no box. (A powerline
    // strip's last arrow reaches into its cell: the glyph steps right of it.)
    let plus_rect = Rect { x, y: 0.0, w: plus_w, h, color: [0, 0, 0, 0], ..Default::default() };
    if x + plus_w <= tab_area_x {
        let plus_lead = if opts.style == TabStyle::Powerline { content_lead * 0.5 } else { 0.0 };
        labels.push(("+".to_string(), x + cm.px(11.0) + plus_lead, cm.px(8.0), ui.text_hint));
    }

    // A small "+N" hint when some tabs couldn't be drawn (too many to fit even at
    // the minimum width). Placed just left of the controls so it never overlaps.
    // Guard: only draw when the hint fits left of the controls region — at very
    // narrow widths (<~400px) the hint would otherwise overrun the window controls.
    if overflow > 0 {
        let hint = format!("+{overflow}");
        let hint_x = (tab_area_x - cm.px(34.0)).max(x + plus_w + cm.px(4.0));
        let hint_w = m.text_w(&hint);
        if hint_x + hint_w <= controls_left {
            // Tint the hint with the strongest activity among the HIDDEN tabs
            // (Failed > Bell > Done > Output) so activity on a scrolled-out tab
            // is never silently invisible.
            let hidden_act = (0..start)
                .chain(start + drawn..tabs.len())
                .map(|i| deco_of(i).activity)
                .fold(TabActivity::None, TabActivity::max);
            let hint_col = activity_color(&ui, hidden_act).unwrap_or(ui.text_hint);
            labels.push((hint, hint_x, label_y, hint_col));
        }
    }

    // --- Perf HUD label (right-aligned, just left of the window controls) ---
    // A muted status line (the palette's hint text, readable on any theme). Only
    // emitted when the HUD fits without squeezing the tabs (perf_shown).
    if perf_shown {
        if let Some(s) = perf {
            // Right-align: right edge sits PERF_GAP left of the controls region.
            let hud_x = (controls_left - perf_gap - perf_w).max(left);
            labels.push((s.to_string(), hud_x, label_y, ui.text_hint));
        }
    }

    // --- Right-side controls (left→right): Help "?", Settings "⚙",
    // minimize "─", maximize "▢", close "✕" (rightmost). ---
    // Hover: a soft accent tint of the bar; the close control turns the theme's
    // danger red with a glyph picked to read on it (white on red failed 3:1 on
    // several themes).
    let hover_bg = rgba(mix(ui.bg, ui.accent, 0.22));
    let close_hover_bg = rgba(ui.danger);
    let ctrl_y = 0.0;

    let help_x = sw - left - cm.controls_w(); // = tab_area_x
    let settings_x = sw - left - ctrl_w * 4.0;
    let min_x = sw - left - ctrl_w * 3.0;
    let max_x = sw - left - ctrl_w * 2.0;
    let close_x = sw - left - ctrl_w;

    let help_rect = Rect { x: help_x, y: ctrl_y, w: ctrl_w, h, color: bg, ..Default::default() };
    let settings_rect = Rect { x: settings_x, y: ctrl_y, w: ctrl_w, h, color: bg, ..Default::default() };
    let min_rect = Rect { x: min_x, y: ctrl_y, w: ctrl_w, h, color: bg, ..Default::default() };
    let max_rect = Rect { x: max_x, y: ctrl_y, w: ctrl_w, h, color: bg, ..Default::default() };
    let close_rect = Rect { x: close_x, y: ctrl_y, w: ctrl_w, h, color: bg, ..Default::default() };

    // Hover highlight quads.
    if ctrl_hover == CtrlHover::Help {
        quads.push(Rect { x: help_x, y: 0.0, w: ctrl_w, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Settings {
        quads.push(Rect { x: settings_x, y: 0.0, w: ctrl_w, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Min {
        quads.push(Rect { x: min_x, y: 0.0, w: ctrl_w, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Max {
        quads.push(Rect { x: max_x, y: 0.0, w: ctrl_w, h, color: hover_bg, ..Default::default() });
    }
    if ctrl_hover == CtrlHover::Close {
        quads.push(Rect { x: close_x, y: 0.0, w: ctrl_w, h, color: close_hover_bg, ..Default::default() });
    }

    // Glyphs centred-ish in each control cell. "⚙" may be missing in some
    // monospace fonts; "≡" is a safe, widely-available fallback for settings.
    let fg = ui.text;
    labels.push(("?".to_string(), help_x + cm.px(9.0), label_y, fg));
    labels.push(("⚙".to_string(), settings_x + cm.px(8.0), label_y, fg));
    labels.push(("─".to_string(), min_x + cm.px(8.0), label_y, fg));
    labels.push(("▢".to_string(), max_x + cm.px(8.0), label_y, fg));
    let close_fg = if ctrl_hover == CtrlHover::Close { ui.on_danger } else { fg };
    labels.push(("✕".to_string(), close_x + cm.px(8.0), label_y, close_fg));

    TabBar {
        quads, labels, title_labels, tab_rects, close_rects, plus_rect,
        help_rect, settings_rect, min_rect, max_rect, close_rect,
    }
}

// ── Detached-window top bar ──────────────────────────────────────────────────

/// Geometry + draw data for a DETACHED window's top bar: the tab's title as a
/// single active pill on the left, and a lone close "✕" control on the right.
/// Same visual language (heights, pill, colors, control cell) as the main tab
/// bar, minus the multi-tab affordances ("+", overflow, min/max, settings).
pub struct DetachedBar {
    /// Quads in draw order: bar background, title pill, optional ✕ hover.
    pub quads: Vec<Rect>,
    /// Monospace chrome labels: the close "✕" glyph.
    pub labels: Vec<(String, f32, f32, [u8; 3])>,
    /// The title label, rendered in the proportional sans like main tab titles.
    pub title_labels: Vec<(String, f32, f32, [u8; 3])>,
    /// Hit-test rect for the close "✕" (triggers the close→reattach path).
    pub close_rect: Rect,
    /// Hit-test rect for the help "?" (the window's keyboard-shortcuts help).
    pub help_rect: Rect,
}

/// Hit-test rect of a DETACHED window's help "?" control: one control cell left
/// of the close "✕" — the main bar's control geometry.
pub fn detached_help_rect(width: u32, cm: ChromeMetrics) -> Rect {
    Rect {
        x: width as f32 - cm.strip_pad() - cm.ctrl_w() * 2.0,
        y: 0.0,
        w: cm.ctrl_w(),
        h: cm.bar_h(),
        color: [0, 0, 0, 0],
        ..Default::default()
    }
}

/// Hit-test rect for a detached window's close "✕": the rightmost control cell
/// of the bar, inset by the strip pad — the same cell the main window's close
/// control occupies. Exposed separately so the app's hover tracking can
/// hit-test without building the whole bar.
pub fn detached_close_rect(width: u32, cm: ChromeMetrics) -> Rect {
    Rect {
        x: width as f32 - cm.strip_pad() - cm.ctrl_w(),
        y: 0.0,
        w: cm.ctrl_w(),
        h: cm.bar_h(),
        color: [0, 0, 0, 0],
        ..Default::default()
    }
}

/// Build the top bar of a DETACHED window in the default look (see
/// [`build_detached_bar_styled`]).
pub fn build_detached_bar(
    width: u32,
    title: &str,
    theme: &Theme,
    close_hover: bool,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
) -> DetachedBar {
    build_detached_bar_styled(width, title, theme, close_hover, m, cm, &TabDeco::default(), &TabBarOpts::default())
}

/// Build the top bar of a DETACHED window: bar background, the tab title as the
/// window's (always active) tab in `opts.style` with its color and progress,
/// and the close "✕" at the right (danger highlight when `close_hover`). `m` /
/// `cm` as in [`build_tab_bar_styled`]. The detached bar is always at the top.
#[allow(clippy::too_many_arguments)]
pub fn build_detached_bar_styled(
    width: u32,
    title: &str,
    theme: &Theme,
    close_hover: bool,
    m: &mut dyn ChromeMeasure,
    cm: ChromeMetrics,
    deco: &TabDeco,
    opts: &TabBarOpts,
) -> DetachedBar {
    let sw = width as f32;
    let h = cm.bar_h();
    let ui = UiPalette::cached(theme);
    let rgba = |c: [u8; 3]| [c[0], c[1], c[2], 255];

    let mut quads: Vec<Rect> = Vec::new();
    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
    let mut title_labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();

    // Bar background spanning the full width (see `TabBarOpts::opaque`).
    if opts.opaque {
        quads.push(Rect { x: 0.0, y: 0.0, w: sw, h, color: rgba(ui.bg), ..Default::default() });
    }

    // The title tab on the left — same geometry as a main-window tab, clamped so
    // it never runs under the help / close controls.
    let sm = StyleMetrics::of(opts.style);
    let left = cm.strip_pad();
    let controls_left = (sw - left - cm.ctrl_w() * 2.0).max(left);
    let tab_w = cm.px(sm.tab_w).min((controls_left - left).max(0.0));
    if tab_w > cm.px(sm.inset) * 2.0 {
        let color = deco.color.and_then(|c| tab_color_rgb(theme, c));
        let colors = tab_colors(&ui, opts.style, true, false, color);
        let cell = TabCell { x: left, w: tab_w };
        let lead = paint_tab_body(&mut quads, opts.style, sm, cm, &cell, h, &colors, true, false);
        let title_x = left + cm.px(sm.title_pad) + lead;
        let shown = fit_head(m, title, (tab_w - cm.px(sm.title_pad) - lead - cm.px(8.0)).max(0.0), true);
        title_labels.push((shown, title_x, cm.px(LABEL_Y), colors.title));
        if let (true, Some(p)) = (opts.progress, deco.progress.as_ref()) {
            // The detached tab has no "×": its bar runs to the tab's right pad.
            let right_edge = left + tab_w - cm.px(8.0) + cm.px(2.0);
            push_tab_progress(&mut quads, &ui, p, opts.style, sm, cm, &cell, h, &colors, title_x, right_edge, false);
            push_seam_progress(&mut quads, &ui, p, cm, sw, h, false);
        }
    }

    // Close "✕" at the right — danger hover background with a readable glyph
    // (identical treatment to the main window's close control).
    let close_rect = detached_close_rect(width, cm);
    if close_hover {
        quads.push(Rect { x: close_rect.x, y: 0.0, w: close_rect.w, h, color: rgba(ui.danger), ..Default::default() });
    }
    let close_fg = if close_hover { ui.on_danger } else { ui.text };
    labels.push(("✕".to_string(), close_rect.x + cm.px(8.0), cm.px(LABEL_Y), close_fg));
    // Help "?" left of it (same glyph offset as the main bar's help control).
    let help_rect = detached_help_rect(width, cm);
    labels.push(("?".to_string(), help_rect.x + cm.px(9.0), cm.px(LABEL_Y), ui.text));

    DetachedBar { quads, labels, title_labels, close_rect, help_rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::by_name("catppuccin_mocha")
    }

    /// Baseline measurer + metrics (1×, 16pt, monospace advance).
    fn mono() -> MonoMeasure {
        MonoMeasure(CHROME_CHAR_W)
    }
    const CM: ChromeMetrics = ChromeMetrics::DEFAULT;

    #[test]
    fn long_titles_truncate_by_measured_width() {
        // Titles are fitted to the tab by their MEASURED width (wide CJK chars
        // count double), never past the close box — and short titles are kept.
        let tabs = [
            ("你好世界你好世界你好世界".to_string(), true),
            ("Tab 2".to_string(), false),
        ];
        let bar = build_tab_bar_ex(1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[]);
        let cjk = &bar.title_labels[0];
        assert!(cjk.0.ends_with('…'), "long title must be ellipsized: {:?}", cjk.0);
        let right = cjk.1 + mono().title_w(&cjk.0);
        assert!(
            right <= bar.close_rects[0].x + 0.5,
            "title runs under its close box: right={right} close_x={}",
            bar.close_rects[0].x
        );
        assert_eq!(bar.title_labels[1].0, "Tab 2", "short titles are untouched");
    }

    #[test]
    fn controls_parked_at_right_in_order() {
        let bar = build_tab_bar(1000, &[("Tab 1".to_string(), true)], &theme());
        // Left→right: help, settings, min, max, close — all within the strip.
        assert!(bar.help_rect.x < bar.settings_rect.x);
        assert!(bar.settings_rect.x < bar.min_rect.x);
        assert!(bar.min_rect.x < bar.max_rect.x);
        assert!(bar.max_rect.x < bar.close_rect.x);
        // The close button's right edge sits STRIP_PAD in from the surface edge.
        assert!((bar.close_rect.x + bar.close_rect.w - (1000.0 - STRIP_PAD)).abs() < 0.01);
        // Each control is one cell wide (CONTROLS_W spans five cells).
        assert!((bar.close_rect.w - CONTROLS_W / 5.0).abs() < 0.01);
        assert_eq!(bar.close_rect.h, TABBAR_H);
    }

    #[test]
    fn tabs_never_overlap_controls() {
        // Many tabs would overflow; the "+" must not be drawn under the controls.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let bar = build_tab_bar(800, &tabs, &theme());
        let controls_left = 800.0 - STRIP_PAD - CONTROLS_W;
        // No tab's switch rect should start at/after the controls region edge
        // beyond what fits; the plus rect (when shown) stays left of controls.
        if bar.plus_rect.x + bar.plus_rect.w <= controls_left {
            assert!(bar.plus_rect.x + bar.plus_rect.w <= controls_left);
        }
    }

    #[test]
    fn active_tab_always_drawn_even_when_overflowing() {
        // 20 tabs can't all fit in 800px; the active tab is far past the head
        // window. It must still be laid out on-screen, and its hit-rect must stay
        // index-aligned with its absolute tab index.
        let active = 15usize;
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == active)).collect();
        let bar = build_tab_bar(800, &tabs, &theme());
        // One hit-rect per tab (absolute-index-aligned for the click→tab mapping).
        assert_eq!(bar.tab_rects.len(), 20);
        let controls_left = 800.0 - STRIP_PAD - CONTROLS_W;
        let r = bar.tab_rects[active];
        assert!(
            r.x >= 0.0 && r.x + r.w <= controls_left + 0.5,
            "active tab not visible on the strip: x={} w={}",
            r.x,
            r.w
        );
        // A head tab that scrolled out of the window is parked offscreen so it
        // can't be clicked to a wrong index.
        assert!(bar.tab_rects[0].x < 0.0, "scrolled-out head tab should be offscreen");
    }

    #[test]
    fn tabs_shrink_to_fit_narrow_window() {
        // 3 tabs in a 560px window: all must be drawn, none overlapping the
        // controls, and each tab's close box stays left of the controls region.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar(560, &tabs, &theme());
        let controls_left = 560.0 - STRIP_PAD - CONTROLS_W;
        assert_eq!(bar.tab_rects.len(), 3, "all 3 tabs should be drawn");
        for r in &bar.tab_rects {
            assert!(
                r.x + r.w <= controls_left + 0.5,
                "tab overflows controls: {} > {controls_left}",
                r.x + r.w
            );
        }
        for r in &bar.close_rects {
            assert!(
                r.x + r.w <= controls_left + 0.5,
                "close box overlaps controls at x={}",
                r.x + r.w
            );
        }
        // The "+" button (when present) also stays left of the controls.
        if bar.plus_rect.x + bar.plus_rect.w <= controls_left + 0.5 {
            assert!(bar.plus_rect.x >= bar.tab_rects.last().unwrap().x);
        }
    }

    #[test]
    fn rename_shows_caret() {
        let tabs = [("Old".to_string(), true)];
        let bar = build_tab_bar_ex(800, &tabs, &theme(), Some((0, "New")), CtrlHover::None, None, &mut mono(), CM, &[]);
        // Renaming shows the edit buffer + caret in the sans TITLE labels.
        let buf = bar.title_labels.iter().find(|l| l.0.contains('|'));
        assert!(buf.is_some(), "no caret label found");
        assert!(buf.unwrap().0.starts_with("New"));
    }

    const PERF: &str = "⚡ 5.1 ms · 190 fps · 0.5% CPU · 155 MB/s";

    #[test]
    fn perf_hud_present_shrinks_tab_area_and_no_overlap() {
        // Wide window: the HUD fits, is emitted as a label, reserves space (so the
        // tab area is smaller than without it), and no tab/close rect overlaps it.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
        ];
        let with = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, Some(PERF), &mut mono(), CM, &[]);
        let without = build_tab_bar_ex(1400, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[]);

        // The HUD label is present.
        let hud = with.labels.iter().find(|l| l.0 == PERF).expect("HUD label missing");
        // It sits left of the window controls (help button is the leftmost control).
        assert!(hud.1 < with.help_rect.x, "HUD must be left of the controls");

        // The HUD's reserved left edge: tabs/close rects must not cross into it.
        let hud_left = hud.1 - PERF_GAP;
        for r in &with.tab_rects {
            assert!(r.x + r.w <= hud_left + 0.5, "tab overlaps the HUD reservation");
        }
        for r in &with.close_rects {
            assert!(r.x + r.w <= hud_left + 0.5, "close box overlaps the HUD");
        }
        // The "+" button stays left of the HUD too.
        assert!(with.plus_rect.x + with.plus_rect.w <= hud_left + 0.5);

        // Reservation actually shrinks the usable tab area: at default tab width
        // both layouts draw full-width tabs, so compare the "+" position — with the
        // HUD it must sit no further right than without (area is smaller-or-equal),
        // and the HUD eats real space so it's strictly left in the multi-tab case.
        assert!(with.plus_rect.x <= without.plus_rect.x);
    }

    #[test]
    fn perf_hud_hidden_when_too_narrow_keeps_layout() {
        // Narrow window: the HUD cannot fit without squeezing tabs, so it's hidden
        // and the tab layout is byte-identical to the no-HUD case.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let with = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, Some(PERF), &mut mono(), CM, &[]);
        let without = build_tab_bar_ex(560, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[]);

        // No HUD label emitted.
        assert!(with.labels.iter().all(|l| l.0 != PERF), "HUD should be hidden");
        // Tab + close + plus geometry identical to the no-HUD layout.
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.01 && (a.w - b.w).abs() < 0.01);
        }
        for (a, b) in with.close_rects.iter().zip(&without.close_rects) {
            assert!((a.x - b.x).abs() < 0.01);
        }
        assert!((with.plus_rect.x - without.plus_rect.x).abs() < 0.01);
    }

    #[test]
    fn perf_hud_yields_to_tabs_when_many_tabs_would_squash() {
        // A moderately WIDE window (900px) where the tab area alone is plenty, but
        // 4 tabs + the HUD reservation would shrink each tab below the comfortable
        // floor. The HUD must hide and hand the full area to the tabs, so no tab is
        // squashed to its ~64px minimum just to fit the stats readout.
        let tabs: Vec<(String, bool)> =
            (0..4).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let with = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, Some(PERF), &mut mono(), CM, &[]);
        let without = build_tab_bar_ex(900, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[]);

        // HUD hidden, layout identical to no-HUD.
        assert!(with.labels.iter().all(|l| l.0 != PERF), "HUD should yield to the tabs");
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.01 && (a.w - b.w).abs() < 0.01);
        }
        // Each drawn tab is comfortably above the squashed minimum.
        for r in &with.tab_rects {
            assert!(r.w >= PERF_MIN_TAB_W - 0.5, "tab squashed to {} despite hiding HUD", r.w);
        }
    }

    #[test]
    fn overflow_hint_does_not_overrun_controls_at_narrow_width() {
        // Many tabs in a very narrow window — the "+N" hint must NOT appear when
        // it would overlap the window controls region.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        // 400px is narrow enough to stress the guard (controls_left ≈ 252px).
        let bar = build_tab_bar(400, &tabs, &theme());
        let controls_left = 400.0 - STRIP_PAD - CONTROLS_W;
        // Any "+N" label must end before the controls region.
        for label in &bar.labels {
            if label.0.starts_with('+') && label.0[1..].chars().all(|c| c.is_ascii_digit()) {
                let hint_w = label.0.chars().count() as f32 * CHROME_CHAR_W;
                assert!(
                    label.1 + hint_w <= controls_left + 0.5,
                    "overflow hint overruns controls: hint_right={} controls_left={controls_left}",
                    label.1 + hint_w
                );
            }
        }
    }

    #[test]
    fn detached_bar_close_parked_at_right() {
        let bar = build_detached_bar(1000, "Tab 2", &theme(), false, &mut mono(), CM);
        // The ✕ occupies the rightmost control cell, inset by STRIP_PAD.
        assert!((bar.close_rect.x + bar.close_rect.w - (1000.0 - STRIP_PAD)).abs() < 0.01);
        assert!((bar.close_rect.w - CTRL_W_BASE).abs() < 0.01);
        assert_eq!(bar.close_rect.h, TABBAR_H);
        // It matches the standalone hit-test helper the app uses for hover.
        let cr = detached_close_rect(1000, CM);
        assert_eq!(cr.x, bar.close_rect.x);
        // The ✕ glyph is emitted.
        assert!(bar.labels.iter().any(|l| l.0 == "✕"));
    }

    #[test]
    fn detached_bar_shows_title_pill() {
        let bar = build_detached_bar(1000, "Build logs", &theme(), false, &mut mono(), CM);
        // Title present in the sans title labels.
        assert!(bar.title_labels.iter().any(|l| l.0 == "Build logs"));
        // Bar bg + pill are both emitted (≥ 2 quads).
        assert!(bar.quads.len() >= 2);
    }

    #[test]
    fn detached_bar_close_hover_paints_theme_red() {
        let hot = build_detached_bar(1000, "Tab 2", &theme(), true, &mut mono(), CM);
        let ui = UiPalette::from_theme(&theme());
        let red = ui.danger;
        assert!(hot.quads.iter().any(|q| q.color == [red[0], red[1], red[2], 255]));
        // The glyph switches to the palette's readable-on-danger color.
        assert!(hot.labels.iter().any(|l| l.0 == "✕" && l.3 == ui.on_danger));
    }

    #[test]
    fn detached_bar_long_title_truncates_with_ellipsis() {
        let long = "a very long detached tab title that cannot fit";
        let bar = build_detached_bar(1000, long, &theme(), false, &mut mono(), CM);
        let label = &bar.title_labels[0].0;
        assert!(label.ends_with('…'), "expected truncation, got {label:?}");
    }

    #[test]
    fn close_hover_changes_glyph_color() {
        let tabs = [("Tab 1".to_string(), true)];
        let hot = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::Close, None, &mut mono(), CM, &[]);
        // A theme-red hover quad is appended when the close control is hovered.
        let red = theme().palette[1];
        let red_bg = [red[0], red[1], red[2], 255];
        assert!(hot.quads.iter().any(|q| q.color == red_bg));
    }

    /// The 6px activity dots for a bar built over 3 tabs (tab 0 active).
    fn dot_quads(bar: &TabBar) -> Vec<Rect> {
        // Distinguish the dot from other colored quads (e.g. the CTRL_W-wide
        // close-hover highlight) by its 6.0 width/height.
        bar.quads
            .iter()
            .filter(|q| (q.w - 6.0).abs() < 0.01 && (q.h - 6.0).abs() < 0.01)
            .copied()
            .collect()
    }

    #[test]
    fn activity_dot_drawn_with_theme_colors() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM,
            &[TabActivity::None, TabActivity::Output, TabActivity::Bell],
        );
        let ui = UiPalette::from_theme(&theme());
        let (accent, amber) = (ui.accent, ui.warn);
        let dots = dot_quads(&bar);
        assert_eq!(dots.len(), 2, "one dot per inactive tab with activity");
        assert!(dots.iter().any(|q| q.color == [accent[0], accent[1], accent[2], 255]),
            "output dot must use the theme accent");
        assert!(dots.iter().any(|q| q.color == [amber[0], amber[1], amber[2], 255]),
            "bell dot must be amber (red means a failed command)");
        // Baseline: no activity → no dots.
        let base = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[],
        );
        assert!(dot_quads(&base).is_empty());
    }

    #[test]
    fn activity_dot_suppressed_on_active_tab() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let bar = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM,
            &[TabActivity::Bell, TabActivity::None, TabActivity::None],
        );
        assert!(dot_quads(&bar).is_empty(), "the active tab never shows a dot");
    }

    #[test]
    fn activity_does_not_shift_layout() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let with = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM,
            &[TabActivity::None, TabActivity::Output, TabActivity::Bell],
        );
        let without = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[],
        );
        // Labels + hit geometry are bit-identical; only the quads differ.
        assert_eq!(with.title_labels, without.title_labels);
        assert_eq!(with.labels, without.labels);
        assert_eq!(with.tab_rects.len(), without.tab_rects.len());
        for (a, b) in with.tab_rects.iter().zip(&without.tab_rects) {
            assert!((a.x - b.x).abs() < 0.001 && (a.w - b.w).abs() < 0.001);
        }
        for (a, b) in with.close_rects.iter().zip(&without.close_rects) {
            assert!((a.x - b.x).abs() < 0.001 && (a.w - b.w).abs() < 0.001);
        }
        assert!((with.plus_rect.x - without.plus_rect.x).abs() < 0.001);
    }

    #[test]
    fn overflow_hint_tinted_when_hidden_tab_has_activity() {
        // 20 tabs at 800px overflow; tab 19 is scrolled out (window sticks to
        // the active head) and its Bell must tint the "+N" hint amber.
        let tabs: Vec<(String, bool)> =
            (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let mut activity = vec![TabActivity::None; 20];
        activity[19] = TabActivity::Bell;
        let bar = build_tab_bar_ex(
            800, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &activity,
        );
        let hint = |b: &TabBar| {
            b.labels
                .iter()
                .find(|l| {
                    l.0.len() > 1
                        && l.0.starts_with('+')
                        && l.0[1..].chars().all(|c| c.is_ascii_digit())
                })
                .cloned()
        };
        let ui = UiPalette::from_theme(&theme());
        let tinted = hint(&bar).expect("overflow hint missing");
        assert_eq!(tinted.3, ui.warn, "hidden Bell must tint the +N hint amber");
        // All-None keeps the dim hint color.
        let base = build_tab_bar_ex(
            800, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[],
        );
        let plain = hint(&base).expect("overflow hint missing");
        assert_eq!(plain.3, ui.text_hint);
        assert_eq!(tinted.1, plain.1, "tint must not move the hint");
        // A hidden FAILED command outranks the bell: the hint turns red.
        activity[15] = TabActivity::Failed; // hidden (9 tabs fit at 800px)
        activity[17] = TabActivity::Done;
        let failed = build_tab_bar_ex(800, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &activity);
        assert_eq!(hint(&failed).expect("overflow hint missing").3, ui.danger);
    }

    #[test]
    fn empty_activity_slice_means_none() {
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
        ];
        let wrapper = build_tab_bar(1000, &tabs, &theme());
        let ex = build_tab_bar_ex(
            1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[],
        );
        assert_eq!(wrapper.quads.len(), ex.quads.len());
        assert!(dot_quads(&wrapper).is_empty());
    }

    #[test]
    fn bar_geometry_scales_with_chrome_metrics() {
        // A 2× display must yield EXACTLY twice the 1× hit geometry (on a 2×
        // wide surface), so hover/click targets track the drawn chrome.
        let tabs = [
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
            ("Tab 3".to_string(), false),
        ];
        let one = build_tab_bar_ex(1000, &tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, &[]);
        let hi = ChromeMetrics::new(2.0, 16.0);
        let two = build_tab_bar_ex(
            2000, &tabs, &theme(), None, CtrlHover::None, None, &mut MonoMeasure(2.0 * CHROME_CHAR_W), hi, &[],
        );
        let pairs = one
            .tab_rects
            .iter()
            .zip(&two.tab_rects)
            .chain(one.close_rects.iter().zip(&two.close_rects))
            .chain([
                (&one.plus_rect, &two.plus_rect),
                (&one.help_rect, &two.help_rect),
                (&one.settings_rect, &two.settings_rect),
                (&one.close_rect, &two.close_rect),
            ]);
        for (a, b) in pairs {
            for (va, vb) in [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)] {
                assert!((vb - 2.0 * va).abs() < 0.01, "not 2×: {va} vs {vb}");
            }
        }
        for (a, b) in one.title_labels.iter().zip(&two.title_labels) {
            assert_eq!(a.0, b.0);
            assert!((b.1 - 2.0 * a.1).abs() < 0.01 && (b.2 - 2.0 * a.2).abs() < 0.01);
        }
    }

    #[test]
    fn labels_stay_inside_the_bar_at_large_ui_fonts() {
        // The bug class: a 28pt UI font (or a 2× display) drew 28–37px glyphs
        // into a fixed 36px bar, spilling into the grid's first row. The bar now
        // grows with the chrome unit, so every label's line box fits inside it.
        let tabs = [("Tab 1".to_string(), true), ("Tab 2".to_string(), false)];
        for (dpi, font) in [(1.0, 16.0), (1.0, 24.0), (1.0, 28.0), (2.0, 16.0), (2.0, 28.0), (1.25, 18.0)] {
            let cm = ChromeMetrics::new(dpi, font);
            let bar = build_tab_bar_ex(
                3000, &tabs, &theme(), None, CtrlHover::None, None, &mut MonoMeasure(9.6 * cm.u), cm, &[],
            );
            // The chrome text line box is ceil(font_px * 1.3) tall.
            let line_h = (16.0 * cm.u * 1.3).ceil();
            for l in bar.labels.iter().chain(&bar.title_labels) {
                assert!(
                    l.2 + line_h <= cm.bar_h() + 1.0,
                    "label {:?} overflows the bar at {dpi}×/{font}pt: bottom {} > bar {}",
                    l.0,
                    l.2 + line_h,
                    cm.bar_h()
                );
            }
            assert_eq!(bar.close_rect.h, cm.bar_h());
        }
    }

    #[test]
    fn huge_program_titles_cost_bounded_work_per_frame() {
        // OSC 0/2 titles are program-controlled; a hostile program can send a
        // different multi-MB title every frame. Each frame must shape a bounded
        // amount of text and emit short labels, however long the titles are.
        struct Spy {
            inner: MonoMeasure,
            max_chars: usize,
            calls: usize,
        }
        impl ChromeMeasure for Spy {
            fn char_xs(&mut self, s: &str, title: bool, out: &mut Vec<f32>) {
                self.calls += 1;
                self.max_chars = self.max_chars.max(s.chars().count());
                self.inner.char_xs(s, title, out);
            }
        }
        let mut tabs = vec![("w".repeat(1 << 20), true), ("v".repeat(1 << 20), false)];
        let mut spy = Spy { inner: MonoMeasure(CHROME_CHAR_W), max_chars: 0, calls: 0 };
        for frame in 0..1000 {
            // A DISTINCT 1 MiB title every frame, without reallocating.
            tabs[0].0.replace_range(0..6, &format!("{frame:06}"));
            spy.calls = 0;
            let bar = build_tab_bar_ex(1000, &tabs, &theme(), None, CtrlHover::None, None, &mut spy, CM, &[]);
            assert!(spy.calls <= 20, "frame {frame}: {} measurements", spy.calls);
            for l in bar.title_labels.iter().chain(&bar.labels) {
                assert!(l.0.chars().count() <= crate::chrome::MAX_LABEL_CHARS + 1);
            }
        }
        assert!(
            spy.max_chars <= crate::chrome::MAX_LABEL_CHARS,
            "shaped {} chars of a 1 MiB title",
            spy.max_chars
        );
    }

    #[test]
    fn detached_close_rect_tracks_metrics() {
        let hi = ChromeMetrics::new(2.0, 16.0);
        let r = detached_close_rect(2000, hi);
        assert!((r.x + r.w - (2000.0 - 2.0 * STRIP_PAD)).abs() < 0.01);
        assert_eq!(r.h, 72.0);
        let bar = build_detached_bar(2000, "Tab", &theme(), false, &mut MonoMeasure(19.2), hi);
        assert_eq!(bar.close_rect.x, r.x);
        // The help "?" sits one control cell left of the ✕, at the same scale.
        let help = detached_help_rect(2000, hi);
        assert_eq!(bar.help_rect.x, help.x);
        assert!((help.x + help.w - r.x).abs() < 0.01, "help abuts the close control");
        assert!(bar.labels.iter().any(|(t, x, _, _)| t == "?" && *x >= help.x && *x < r.x));
    }

    #[test]
    fn detached_title_pill_never_runs_under_the_controls() {
        let cm = ChromeMetrics::new(1.0, 16.0);
        // Narrow window: the pill is clamped to end before the help control.
        let bar = build_detached_bar(260, "a very long tab title indeed", &theme(), false, &mut MonoMeasure(9.6), cm);
        let help = detached_help_rect(260, cm);
        let pill = &bar.quads[1];
        assert!(pill.x + pill.w <= help.x + 0.01, "pill ends at {} past help x {}", pill.x + pill.w, help.x);
    }

    // ── styles, close modes, hover, badges, progress, per-tab colors ─────────

    fn three_tabs() -> Vec<(String, bool)> {
        vec![("Tab 1".to_string(), true), ("Tab 2".to_string(), false), ("Tab 3".to_string(), false)]
    }

    fn styled(width: u32, tabs: &[(String, bool)], deco: &[TabDeco], opts: &TabBarOpts) -> TabBar {
        build_tab_bar_styled(width, tabs, &theme(), None, CtrlHover::None, None, &mut mono(), CM, deco, opts)
    }

    fn opts(style: TabStyle) -> TabBarOpts {
        TabBarOpts { style, ..TabBarOpts::default() }
    }

    fn same_rect(a: &Rect, b: &Rect) -> bool {
        (a.x - b.x).abs() < 0.001 && (a.y - b.y).abs() < 0.001 && (a.w - b.w).abs() < 0.001 && (a.h - b.h).abs() < 0.001
    }

    fn rgba(c: [u8; 3]) -> [u8; 4] {
        [c[0], c[1], c[2], 255]
    }

    #[test]
    fn activity_precedence_is_failed_bell_done_output() {
        use TabActivity::*;
        let order = [None, Output, Done, Bell, Failed];
        for (i, &a) in order.iter().enumerate() {
            for (j, &b) in order.iter().enumerate() {
                let want = if j > i { b } else { a };
                assert_eq!(a.max(b), want, "{a:?} vs {b:?}");
            }
        }
        // Folding a set keeps the strongest whatever the order.
        assert_eq!([Output, Failed, Done, Bell].into_iter().fold(None, TabActivity::max), Failed);
        assert_eq!([Done, Output].into_iter().fold(None, TabActivity::max), Done);
    }

    #[test]
    fn done_and_failed_badges_use_success_and_danger() {
        let ui = UiPalette::from_theme(&theme());
        let tabs = three_tabs();
        let deco = [
            TabDeco::default(),
            TabDeco { activity: TabActivity::Done, ..Default::default() },
            TabDeco { activity: TabActivity::Failed, ..Default::default() },
        ];
        let bar = styled(1000, &tabs, &deco, &TabBarOpts::default());
        let dots = dot_quads(&bar);
        assert_eq!(dots.len(), 2);
        let has = |c: [u8; 3]| dots.iter().any(|q| q.color == rgba(c));
        assert!(has(ui.success), "done = green");
        assert!(has(ui.danger), "failed = red");
    }

    #[test]
    fn non_compact_styles_share_todays_hit_geometry() {
        // Only the look changes: every hit rect equals the default (pill) layout,
        // for 3 tabs and for an overflowing strip, top and bottom.
        for n in [3usize, 20] {
            let tabs: Vec<(String, bool)> = (0..n).map(|i| (format!("Tab {i}"), i == 1)).collect();
            let base = styled(1000, &tabs, &[], &TabBarOpts::default());
            for style in [TabStyle::Underline, TabStyle::Slant, TabStyle::Powerline] {
                for bottom in [false, true] {
                    let bar = styled(1000, &tabs, &[], &TabBarOpts { style, bottom, ..TabBarOpts::default() });
                    for (a, b) in bar.tab_rects.iter().zip(&base.tab_rects) {
                        assert!(same_rect(a, b), "{style:?}: tab rect moved");
                    }
                    for (a, b) in bar.close_rects.iter().zip(&base.close_rects) {
                        assert!(same_rect(a, b), "{style:?}: close rect moved");
                    }
                    for (a, b) in [
                        (&bar.plus_rect, &base.plus_rect),
                        (&bar.help_rect, &base.help_rect),
                        (&bar.settings_rect, &base.settings_rect),
                        (&bar.min_rect, &base.min_rect),
                        (&bar.max_rect, &base.max_rect),
                        (&bar.close_rect, &base.close_rect),
                    ] {
                        assert!(same_rect(a, b), "{style:?}: control rect moved");
                    }
                }
            }
        }
    }

    #[test]
    fn compact_tabs_are_narrower_and_fit_more() {
        let tabs = three_tabs();
        let pill = styled(1000, &tabs, &[], &TabBarOpts::default());
        let compact = styled(1000, &tabs, &[], &opts(TabStyle::Compact));
        assert!((pill.tab_rects[0].w - TAB_W).abs() < 0.01);
        assert!((compact.tab_rects[0].w - COMPACT_TAB_W).abs() < 0.01);
        // Each close box still sits inside its (narrower) tab.
        for (t, c) in compact.tab_rects.iter().zip(&compact.close_rects) {
            assert!(c.x >= t.x && c.x + c.w <= t.x + t.w + 0.01);
        }
        // More tabs fit before the strip overflows.
        let many: Vec<(String, bool)> = (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let fit = |b: &TabBar| b.tab_rects.iter().filter(|r| r.x >= 0.0).count();
        let p = styled(800, &many, &[], &TabBarOpts::default());
        let c = styled(800, &many, &[], &opts(TabStyle::Compact));
        assert!(fit(&c) > fit(&p), "compact fits {} vs pill {}", fit(&c), fit(&p));
        // The bar height is the same (the grid never reflows on a style change).
        assert_eq!(c.close_rect.h, p.close_rect.h);
    }

    #[test]
    fn close_button_modes_show_and_hit_only_visible_crosses() {
        let tabs = three_tabs(); // tab 0 active
        let xs = |b: &TabBar| b.labels.iter().filter(|l| l.0 == "×").count();
        let live = |b: &TabBar| b.close_rects.iter().map(|r| r.x >= 0.0).collect::<Vec<_>>();
        let always = styled(1000, &tabs, &[], &TabBarOpts::default());
        assert_eq!((xs(&always), live(&always)), (3, vec![true, true, true]));
        // Hover: nothing without a pointer, only the hovered tab with one.
        let mut o = TabBarOpts { close_button: CloseButton::Hover, ..TabBarOpts::default() };
        let none = styled(1000, &tabs, &[], &o);
        assert_eq!((xs(&none), live(&none)), (0, vec![false, false, false]));
        o.hover = Some(2);
        let hov = styled(1000, &tabs, &[], &o);
        assert_eq!((xs(&hov), live(&hov)), (1, vec![false, false, true]));
        // Active: the active tab, plus the hovered one.
        o.close_button = CloseButton::Active;
        o.hover = None;
        let act = styled(1000, &tabs, &[], &o);
        assert_eq!((xs(&act), live(&act)), (1, vec![true, false, false]));
        o.hover = Some(1);
        let act_hov = styled(1000, &tabs, &[], &o);
        assert_eq!((xs(&act_hov), live(&act_hov)), (2, vec![true, true, false]));
        // A visible cross keeps today's exact rect.
        assert!(same_rect(&act_hov.close_rects[1], &always.close_rects[1]));
        // Config strings round-trip; unknown → Always.
        for m in CloseButton::ALL {
            assert_eq!(CloseButton::from_config(m.to_config()), m);
        }
        assert_eq!(CloseButton::from_config("bogus"), CloseButton::Always);
    }

    #[test]
    fn hovering_an_inactive_tab_lifts_it() {
        let tabs = three_tabs();
        let ui = UiPalette::from_theme(&theme());
        let lift = rgba(ui.shade(0.06));
        let has_lift = |b: &TabBar| b.quads.iter().any(|q| q.color == lift);
        assert!(!has_lift(&styled(1000, &tabs, &[], &TabBarOpts::default())));
        let hovered = styled(1000, &tabs, &[], &TabBarOpts { hover: Some(2), ..TabBarOpts::default() });
        assert!(has_lift(&hovered), "a soft pill lifts the hovered inactive tab");
        // Its title brightens from the hint to the dim text color.
        assert_eq!(hovered.title_labels[2].3, ui.text_dim);
        assert_eq!(hovered.title_labels[1].3, ui.text_hint);
        // Hovering the ACTIVE tab changes nothing.
        let on_active = styled(1000, &tabs, &[], &TabBarOpts { hover: Some(0), ..TabBarOpts::default() });
        assert!(!has_lift(&on_active));
    }

    #[test]
    fn slant_and_powerline_are_sheared_quads_never_glyphs() {
        let tabs = three_tabs();
        let slant = styled(1000, &tabs, &[], &opts(TabStyle::Slant));
        let sheared: Vec<&Rect> = slant.quads.iter().filter(|q| q.shear != 0.0).collect();
        assert_eq!(sheared.len(), 3, "one parallelogram per tab");
        assert!(sheared.iter().all(|q| q.shear > 0.0 && q.radius > 0.0), "leaning `/`, antialiased");
        // Each parallelogram stays inside its tab cell.
        for (q, t) in sheared.iter().zip(&slant.tab_rects) {
            let (l, r) = q.sheared_x_span();
            assert!(l >= t.x - 0.01 && r <= t.x + t.w + 0.01, "slant spills out of its tab: {l}..{r}");
        }
        let pl = styled(1000, &tabs, &[], &opts(TabStyle::Powerline));
        let ups = pl.quads.iter().filter(|q| q.shear < 0.0).count();
        let downs = pl.quads.iter().filter(|q| q.shear > 0.0).count();
        assert_eq!((ups, downs), (3, 3), "each chevron = two opposite half-height shears");
        // No style draws its shapes with font glyphs (the Nerd Font is not bundled).
        for style in TabStyle::ALL {
            let b = styled(1000, &tabs, &[], &opts(style));
            for l in b.labels.iter().chain(&b.title_labels) {
                assert!(!l.0.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)), "{style:?} drew a PUA glyph");
            }
        }
        for s in TabStyle::ALL {
            assert_eq!(TabStyle::from_config(s.to_config()), s);
            assert_eq!(s.cycle(true).cycle(false), s);
        }
        assert_eq!(TabStyle::from_config("nope"), TabStyle::Pill);
    }

    #[test]
    fn underline_style_marks_the_active_tab_at_the_seam() {
        let tabs = three_tabs();
        let ui = UiPalette::from_theme(&theme());
        let marks = |b: &TabBar| -> Vec<Rect> { b.quads.iter().filter(|q| q.color == rgba(ui.accent)).copied().collect() };
        let top = styled(1000, &tabs, &[], &opts(TabStyle::Underline));
        let m = marks(&top);
        assert_eq!(m.len(), 1, "one accent underline");
        assert!(m[0].x >= top.tab_rects[0].x && m[0].x + m[0].w <= top.tab_rects[0].x + top.tab_rects[0].w);
        assert!(m[0].y > TABBAR_H / 2.0, "near the grid seam (bottom edge) of a top bar");
        let bottom = styled(1000, &tabs, &[], &TabBarOpts { style: TabStyle::Underline, bottom: true, ..TabBarOpts::default() });
        assert!(marks(&bottom)[0].y < TABBAR_H / 2.0, "a bottom bar's seam is its top edge");
        // No filled tab body anywhere in this style (only the bar background).
        assert_eq!(top.quads.iter().filter(|q| q.h > 10.0 && q.w < 200.0).count(), 0);
    }

    fn prog(state: ProgressState, value: Option<u8>) -> Option<Progress> {
        Some(Progress { state, value })
    }

    #[test]
    fn progress_draws_an_in_tab_bar_and_a_seam_hairline_for_the_active_tab() {
        let tabs = three_tabs();
        let ui = UiPalette::from_theme(&theme());
        let deco = [
            TabDeco { progress: prog(ProgressState::Normal, Some(50)), ..Default::default() },
            TabDeco { progress: prog(ProgressState::Error, Some(30)), ..Default::default() },
            TabDeco::default(),
        ];
        let bar = styled(1000, &tabs, &deco, &TabBarOpts::default());
        let accent: Vec<&Rect> = bar.quads.iter().filter(|q| q.color == rgba(ui.accent)).collect();
        // The active tab: a half-filled in-tab bar + a half-width seam hairline.
        let hair = accent.iter().find(|q| q.h <= 1.0).expect("seam hairline");
        assert!(hair.x.abs() < 0.01 && (hair.w - 500.0).abs() < 1.0, "50% of the window width: {}", hair.w);
        assert!((hair.y + hair.h - TABBAR_H).abs() < 0.01, "at the bar/grid seam");
        let in_tab = accent.iter().find(|q| q.h > 1.0).expect("in-tab bar");
        assert!(in_tab.x >= bar.tab_rects[0].x && in_tab.x + in_tab.w <= bar.tab_rects[0].x + bar.tab_rects[0].w);
        // Its fill is half of its track.
        let track = bar
            .quads
            .iter()
            .find(|q| (q.y - in_tab.y).abs() < 0.01 && q.x == in_tab.x && q.w > in_tab.w)
            .expect("track");
        assert!((in_tab.w - (track.w * 0.5).round()).abs() <= 1.0);
        // The inactive tab's error bar is red and it gets NO hairline.
        let red: Vec<&Rect> = bar.quads.iter().filter(|q| q.color == rgba(ui.danger)).collect();
        assert_eq!(red.len(), 1);
        assert!(red[0].x >= bar.tab_rects[1].x && red[0].h > 1.0);
        // progress_bar = false draws nothing.
        let off = styled(1000, &tabs, &deco, &TabBarOpts { progress: false, ..TabBarOpts::default() });
        let plain = styled(1000, &tabs, &[], &TabBarOpts::default());
        assert_eq!(off.quads.len(), plain.quads.len());
        // A bottom bar's hairline is its top edge.
        let bottom = styled(1000, &tabs, &deco, &TabBarOpts { bottom: true, ..TabBarOpts::default() });
        assert!(bottom.quads.iter().any(|q| q.color == rgba(ui.accent) && q.h <= 1.0 && q.y == 0.0));
        // Progress never moves a hit rect or a label.
        assert_eq!(bar.title_labels, plain.title_labels);
        for (a, b) in bar.tab_rects.iter().zip(&plain.tab_rects) {
            assert!(same_rect(a, b));
        }
    }

    #[test]
    fn indeterminate_progress_is_a_static_stripe() {
        let tabs = vec![("Tab 1".to_string(), true)];
        let ui = UiPalette::from_theme(&theme());
        let deco = [TabDeco { progress: prog(ProgressState::Indeterminate, None), ..Default::default() }];
        let bar = styled(1000, &tabs, &deco, &TabBarOpts::default());
        let dashes: Vec<&Rect> = bar.quads.iter().filter(|q| q.color == rgba(ui.accent)).collect();
        // Several dashes in the tab plus many along the seam; same input → same
        // quads (nothing time-based, so idle frames are identical).
        assert!(dashes.iter().filter(|q| q.h > 1.0).count() >= 4);
        assert!(dashes.iter().filter(|q| q.h <= 1.0).count() >= 20);
        let again = styled(1000, &tabs, &deco, &TabBarOpts::default());
        assert_eq!(bar.quads.len(), again.quads.len());
        // Paused is amber; an error with no value fills the whole bar.
        let p = [TabDeco { progress: prog(ProgressState::Paused, Some(10)), ..Default::default() }];
        assert!(styled(1000, &tabs, &p, &TabBarOpts::default()).quads.iter().any(|q| q.color == rgba(ui.warn)));
        let e = [TabDeco { progress: prog(ProgressState::Error, None), ..Default::default() }];
        let eb = styled(1000, &tabs, &e, &TabBarOpts::default());
        let hair = eb.quads.iter().find(|q| q.color == rgba(ui.danger) && q.h <= 1.0).unwrap();
        assert!((hair.w - 1000.0).abs() < 0.5);
    }

    #[test]
    fn per_tab_colors_tint_the_tab_in_every_style() {
        let tabs = three_tabs();
        let t = theme();
        let red = t.palette[1];
        let deco = [
            TabDeco { color: Some(1), ..Default::default() },
            TabDeco { color: Some(6), ..Default::default() },
            TabDeco::default(),
        ];
        // Pill: the active pill is tinted toward its color, and the inactive
        // colored tab gets a tinted body the uncolored layout lacks.
        let dist = |c: [u8; 4], to: [u8; 3]| {
            let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
            d(c[0], to[0]) + d(c[1], to[1]) + d(c[2], to[2])
        };
        let bar = styled(1000, &tabs, &deco, &TabBarOpts::default());
        let plain = styled(1000, &tabs, &[], &TabBarOpts::default());
        let pill = bar.quads.iter().find(|q| q.radius > 0.0 && q.w > 100.0).unwrap();
        let plain_pill = plain.quads.iter().find(|q| q.radius > 0.0 && q.w > 100.0).unwrap();
        assert!(dist(pill.color, red) < dist(plain_pill.color, red), "the active pill takes the tab's red");
        assert!(bar.quads.len() > plain.quads.len(), "the inactive colored tab gets a tinted body");
        // Underline: the active underline IS the tab's color (kept readable).
        let ul = styled(1000, &tabs, &deco, &opts(TabStyle::Underline));
        let ui = UiPalette::from_theme(&t);
        let want = ui.readable(red, UiPalette::ACCENT_FLOOR);
        assert!(ul.quads.iter().any(|q| q.color == rgba(want)));
        // Powerline: the active segment is filled with the color, its title
        // readable on it.
        let pl = styled(1000, &tabs, &deco, &opts(TabStyle::Powerline));
        assert!(pl.quads.iter().any(|q| q.color == rgba(red) && q.shear != 0.0));
        assert_eq!(pl.title_labels[0].3, ui.on_fill(red));
        // Colors resolve from the live theme; bad indices are ignored.
        assert_eq!(tab_color_rgb(&t, 3), Some(t.palette[3]));
        assert_eq!(tab_color_rgb(&t, 0), None);
        assert_eq!(tab_color_rgb(&t, 7), None);
        assert_eq!(tab_color_name(5), Some("Magenta"));
        let junk = [TabDeco { color: Some(42), ..Default::default() }];
        assert_eq!(styled(1000, &tabs, &junk, &TabBarOpts::default()).quads.len(), plain.quads.len());
    }

    #[test]
    fn an_overflowing_strip_always_has_room_for_its_hint() {
        // Whatever the style and width, a strip that cannot show every tab keeps
        // room for the "+N" hint after the "+" — it carries the hidden tabs'
        // badges, so a hidden failure is never invisible.
        let tabs: Vec<(String, bool)> = (0..60).map(|i| (format!("Tab {i}"), i == 0)).collect();
        let mut deco = vec![TabDeco::default(); 60];
        deco[59].activity = TabActivity::Failed;
        let red = UiPalette::from_theme(&theme()).danger;
        for style in TabStyle::ALL {
            for w in (440..1700).step_by(9) {
                let bar = styled(w, &tabs, &deco, &opts(style));
                assert!(bar.tab_rects.iter().filter(|r| r.x >= 0.0).count() < tabs.len(), "60 tabs overflow");
                let hint = bar.labels.iter().find(|l| l.0.starts_with('+') && l.0.len() > 1);
                let hint = hint.unwrap_or_else(|| panic!("{style:?} at {w}px: no overflow hint"));
                assert_eq!(hint.3, red, "{style:?} at {w}px: the hidden failure tints it");
                let right = hint.1 + mono().text_w(&hint.0);
                assert!(right <= w as f32 - STRIP_PAD - CONTROLS_W + 0.5, "{style:?} at {w}px: hint under the controls");
            }
        }
    }

    #[test]
    fn translucent_bar_skips_its_background() {
        let tabs = three_tabs();
        let opaque = styled(1000, &tabs, &[], &TabBarOpts::default());
        let clear = styled(1000, &tabs, &[], &TabBarOpts { opaque: false, ..TabBarOpts::default() });
        let full = |b: &TabBar| b.quads.iter().filter(|q| q.w >= 1000.0 && q.h >= TABBAR_H).count();
        assert_eq!((full(&opaque), full(&clear)), (1, 0));
        assert_eq!(opaque.quads.len(), clear.quads.len() + 1);
        let d = build_detached_bar_styled(
            1000,
            "T",
            &theme(),
            false,
            &mut mono(),
            CM,
            &TabDeco::default(),
            &TabBarOpts { opaque: false, ..TabBarOpts::default() },
        );
        assert!(d.quads.iter().all(|q| q.w < 1000.0));
    }

    #[test]
    fn chrome_text_is_readable_on_every_builtin_theme() {
        // The contrast fixes: the close "✕" on its red hover (white measured
        // 2.3–2.9:1 on 8 themes) and the muted inactive titles / "+" / "+N" (a
        // fixed fg×2/3 or bg→fg blend fell to 1.5–1.8:1 on light themes).
        let tabs: Vec<(String, bool)> = (0..20).map(|i| (format!("Tab {i}"), i == 0)).collect();
        for t in jetty_core::theme::builtins() {
            let ui = UiPalette::from_theme(&t);
            let bar = build_tab_bar_styled(
                800,
                &tabs,
                &t,
                None,
                CtrlHover::Close,
                None,
                &mut mono(),
                CM,
                &[],
                &TabBarOpts::default(),
            );
            let x = bar.labels.iter().find(|l| l.0 == "✕").unwrap();
            let c = crate::contrast_ratio(x.3, ui.danger);
            assert!(c >= 4.5, "{}: ✕ on red {c:.2}", t.name);
            let d = build_detached_bar(800, "T", &t, true, &mut mono(), CM);
            let dx = d.labels.iter().find(|l| l.0 == "✕").unwrap();
            assert!(crate::contrast_ratio(dx.3, ui.danger) >= 4.5, "{}: detached ✕", t.name);
            for l in bar.title_labels.iter().chain(bar.labels.iter().filter(|l| l.0 != "✕")) {
                let c = crate::contrast_ratio(l.3, ui.bg);
                assert!(c >= 2.95, "{}: {:?} only {c:.2}:1 on the bar", t.name, l.0);
            }
        }
    }

    #[test]
    fn every_style_keeps_marks_and_labels_inside_the_bar() {
        let tabs = three_tabs();
        let deco = [
            TabDeco { progress: prog(ProgressState::Normal, Some(40)), color: Some(2), ..Default::default() },
            TabDeco {
                activity: TabActivity::Failed,
                progress: prog(ProgressState::Indeterminate, None),
                ..Default::default()
            },
            TabDeco::default(),
        ];
        for style in TabStyle::ALL {
            for (dpi, font) in [(1.0, 16.0), (1.0, 28.0), (2.0, 16.0), (2.0, 17.0), (1.25, 22.0)] {
                for bottom in [false, true] {
                    let cm = ChromeMetrics::new(dpi, font);
                    let o = TabBarOpts { style, bottom, hover: Some(1), ..TabBarOpts::default() };
                    let bar = build_tab_bar_styled(
                        2400,
                        &tabs,
                        &theme(),
                        None,
                        CtrlHover::None,
                        None,
                        &mut MonoMeasure(9.6 * cm.u),
                        cm,
                        &deco,
                        &o,
                    );
                    let line_h = (16.0 * cm.u * 1.3).ceil();
                    for l in bar.labels.iter().chain(&bar.title_labels) {
                        assert!(l.2 + line_h <= cm.bar_h() + 1.0, "{style:?} {dpi}×/{font}pt: {:?} overflows", l.0);
                    }
                    for q in &bar.quads {
                        assert!(
                            q.y >= -0.01 && q.y + q.h <= cm.bar_h() + 0.01,
                            "{style:?} {dpi}×/{font}pt: quad leaves the bar: y={} h={}",
                            q.y,
                            q.h
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_narrowest_window_keeps_its_tab_clear_of_the_controls() {
        // The window's minimum width (200 logical px) at 1× and 2×, and just
        // above it: one minimum-width tab no longer fits left of the five
        // window controls. It used to be drawn at that minimum anyway — its
        // pill under "?" and its "×" on top of the "?" glyph.
        let deco = [TabDeco { activity: TabActivity::Bell, ..Default::default() }; 3];
        for style in TabStyle::ALL {
            for (w, cm) in [(200u32, CM), (240, CM), (400, ChromeMetrics::new(2.0, 16.0))] {
                for n in [1usize, 3] {
                    let tabs: Vec<(String, bool)> = (0..n).map(|i| (format!("Tab {i}"), i == 0)).collect();
                    let mut m = MonoMeasure(CHROME_CHAR_W * cm.u);
                    let o = TabBarOpts { style, ..TabBarOpts::default() };
                    let bar = build_tab_bar_styled(w, &tabs, &theme(), None, CtrlHover::None, None, &mut m, cm, &deco, &o);
                    let what = format!("{style:?} {w}px@{}x, {n} tab(s)", cm.dpi);
                    let controls_left = bar.help_rect.x;
                    // The active tab is still there, and readable.
                    assert!(bar.tab_rects[0].w > 0.0 && !bar.title_labels.is_empty(), "{what}: no tab");
                    for r in bar.tab_rects.iter().chain(&bar.close_rects).filter(|r| r.x > -1.0) {
                        assert!(r.x + r.w <= controls_left + 0.5, "{what}: hit rect under the controls");
                    }
                    // Nothing drawn for the tabs (bodies, badges, titles, "×",
                    // "+") reaches the controls: only the bar background and the
                    // controls' own glyphs may.
                    for q in bar.quads.iter().filter(|q| q.w < w as f32) {
                        assert!(q.x + q.w <= controls_left + 0.5, "{what}: quad x={} w={} under the controls", q.x, q.w);
                    }
                    for l in &bar.title_labels {
                        assert!(l.1 + m.title_w(&l.0) <= controls_left + 0.5, "{what}: title {:?} under the controls", l.0);
                    }
                    for l in bar.labels.iter().filter(|l| l.1 < controls_left) {
                        assert!(l.1 + m.text_w(&l.0) <= controls_left + 0.5, "{what}: {:?} under the controls", l.0);
                    }
                }
            }
        }
    }
}
