//! Settings controls as DATA.
//!
//! Every row of the Settings panel is one [`Desc`] in [`DESCS`]: its id (the
//! config key path), tab, section, label, kind (slider / toggle / cycler /
//! steps / stepper / RGB / chips / list, plus the theme gallery and the UI-font
//! specimen) and `get` / `set` over [`Config`]. Everything else is generic over
//! the table:
//!
//! * the panel content ([`tab_items`] → `jetty_render::build_panel`);
//! * hit testing and dragging (`MouseAction::Ctl { id, part }` → [`press`] /
//!   [`drag_value`]);
//! * applying a change: the app snapshots its settings as a `Config`, calls
//!   `set`, and feeds the result through the same diff-and-apply path a config
//!   hot-reload takes — so a control can never apply a setting differently
//!   from an edit of config.toml;
//! * defaults and "Reset tab" (a control's default is its value in
//!   `Config::default()`, see [`reset_tab`]);
//! * palette deep links ("Settings › Effects › Bloom", see [`deep_links`]).
//!
//! **Adding a setting** = its config key (config.rs, with its mirror / persist /
//! reload lines in app.rs) + ONE `Desc` here, e.g.
//!
//! ```ignore
//! Desc { id: "effects.crt_grain", tab: EFFECTS, section: "fx.crt", label: "Grain",
//!        kind: PCT, get: get_f!(effects.crt_grain), set: set_f!(effects.crt_grain),
//!        ..Desc::DEFAULT },
//! ```
//!
//! Sections listed in [`SECTIONS`] without any control yet are hook points for
//! upcoming settings: they stay hidden until a `Desc` names them.

use jetty_render::{CtlId, CtlPart, CtlRow, CtlShow, PanelItem, RowState, TAB_NAMES};

use crate::config::Config;

/// Tab indices (`jetty_render::TAB_NAMES` order).
pub const LOOK: usize = 0;
pub const FONTS: usize = 1;
pub const WINDOW: usize = 2;
pub const SHELL: usize = 3;
pub const EFFECTS: usize = 4;

/// A control's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    B(bool),
    F(f32),
    U(u64),
    S(String),
    Rgb([f32; 3]),
    /// One bit per chip.
    Bits(u32),
}

impl Val {
    fn b(&self) -> bool {
        matches!(self, Val::B(true))
    }
    fn f(&self) -> f32 {
        if let Val::F(x) = self { *x } else { 0.0 }
    }
    fn u(&self) -> u64 {
        if let Val::U(x) = self { *x } else { 0 }
    }
    fn s(&self) -> &str {
        if let Val::S(x) = self { x } else { "" }
    }
    fn bits(&self) -> u32 {
        if let Val::Bits(x) = self { *x } else { 0 }
    }
    fn rgb(&self) -> [f32; 3] {
        if let Val::Rgb(x) = self { *x } else { [0.0; 3] }
    }
}

/// Runtime state the controls read besides the config.
pub struct Ctx<'a> {
    /// The main window is in OS fullscreen right now (an F11 or Fullscreen mode).
    pub main_fullscreen: bool,
    /// Installed monospace families (the terminal-font list).
    pub mono_families: &'a [String],
    /// UI-font candidates, index 0 = the synthetic "System Sans (default)" row.
    pub ui_families: &'a [String],
    /// Detected shells (absolute paths), the Shell cycler's options after
    /// "System default".
    pub shells: &'a [String],
    /// The terminal / UI families actually SHOWN (a chosen family that is not
    /// installed shows a fallback; the lists highlight what is shown then).
    pub font_shown: &'a str,
    pub ui_font_shown: &'a str,
    /// First visible row of each list.
    pub font_offset: usize,
    pub ui_font_offset: usize,
    /// The image files in `<config dir>/backgrounds/` (the backdrop picker),
    /// listed when Settings opens — never per frame.
    pub backdrop_images: &'a [String],
    /// The theme on screen (its id) — what a look's theme is compared with
    /// (the light slot may be showing).
    pub shown_theme: &'a str,
    /// Collapsed section ids.
    pub collapsed: &'a [&'static str],
    /// A drag that applies on release: `(control, value so far)` — shown in
    /// place of the config value until it lands.
    pub drag: Option<(CtlId, &'a Val)>,
}

impl Ctx<'_> {
    /// A context with no fonts, shells, collapse or drag — tests, jetty-shot.
    pub fn empty() -> Ctx<'static> {
        Ctx {
            main_fullscreen: false,
            mono_families: &[],
            ui_families: &[],
            shells: &[],
            font_shown: "",
            ui_font_shown: "",
            font_offset: 0,
            ui_font_offset: 0,
            backdrop_images: &[],
            shown_theme: "",
            collapsed: &[],
            drag: None,
        }
    }
}

/// Where a list control's items come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListSrc {
    /// Installed monospace families.
    Mono,
    /// UI-font candidates; row 0 ("System Sans (default)") is the value "".
    Ui,
}

/// A control's kind, with its range / options.
pub enum Kind {
    /// `min..=max` along the track, snapped to multiples of `step` above `min`
    /// (0 = continuous). `live`: applied on every drag move (else once, on
    /// release — for settings that re-dock a window).
    Slider { min: f32, max: f32, step: f32, fmt: fn(f32) -> String, live: bool },
    Toggle,
    /// `< value >` over `(config value, label)` options. A value that is not
    /// an option (hand-edited) shows verbatim; cycling from it starts over.
    Choice { options: fn(&Ctx) -> Vec<(String, String)> },
    /// `< value >` over numeric steps; a value between steps snaps to its
    /// nearest step first.
    Steps { steps: &'static [u64], fmt: fn(u64) -> String },
    /// `- value +` and a Reset button (to the default).
    Stepper { min: f32, max: f32, step: f32, fmt: fn(f32) -> String },
    /// Three 0..=1 channels.
    Rgb,
    /// Independent on/off chips, one bit each.
    Chips { labels: &'static [&'static str] },
    /// The [`LOOKS`] (a wrapping chip row; a click is `Press::Look`, applied by
    /// the app — a look's theme goes through the slot-aware theme pick).
    Looks,
    /// Named looks applied as a whole (a wrapping chip row). `get` returns the
    /// index of the preset the config matches (`u64::MAX` = none: "Custom");
    /// `set` applies preset `i`.
    Presets { names: fn() -> Vec<&'static str> },
    List { src: ListSrc, rows: usize },
    /// The theme gallery (its own clicks / keys, see `App::gallery_*`).
    Gallery,
    /// The live "Aa" specimen line (layout only; not a setting).
    Specimen,
}

/// A control whose change is not applied through the config path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    /// Writes / removes the login autostart entry first, and flips only when
    /// that worked (`App::toggle_launch_at_login_setting`).
    LaunchAtLogin,
}

/// One control.
pub struct Desc {
    /// The config key path — the control's id everywhere (hits, drags, deep
    /// links, focus).
    pub id: CtlId,
    pub tab: usize,
    /// A [`SECTIONS`] id on the same tab.
    pub section: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub get: fn(&Config) -> Val,
    pub set: fn(&mut Config, Val),
    /// A helper line under the control.
    pub hint: Option<&'static str>,
    /// Dimmed / disabled from the config or the runtime context.
    pub state: Option<fn(&Config, &Ctx) -> RowState>,
    /// Shown only when this holds (rows that only matter in one mode — the
    /// backdrop's image rows in image mode — keep their section short).
    pub visible: Option<fn(&Config) -> bool>,
    /// Part of "Reset tab".
    pub reset: bool,
    pub special: Option<Special>,
}

impl Desc {
    /// Field defaults for `..Desc::DEFAULT`.
    pub const DEFAULT: Desc = Desc {
        id: "",
        tab: LOOK,
        section: "",
        label: "",
        kind: Kind::Toggle,
        get: |_| Val::B(false),
        set: |_, _| {},
        hint: None,
        state: None,
        visible: None,
        reset: true,
        special: None,
    };

    /// A real setting (not the specimen layout row).
    pub fn is_setting(&self) -> bool {
        !matches!(self.kind, Kind::Specimen)
    }
}

/// A section of a tab: a collapsible header, optionally carrying a master
/// switch (a Toggle control of the section, drawn in the header; the rest of
/// the section dims while it is off).
pub struct Section {
    pub id: &'static str,
    pub tab: usize,
    pub title: &'static str,
    pub master: Option<CtlId>,
    pub hint: Option<&'static str>,
}

impl Section {
    const DEFAULT: Section = Section { id: "", tab: LOOK, title: "", master: None, hint: None };
}

/// Every section, in display order per tab. Sections with no control yet are
/// hook points (hidden until a control lands in them).
pub static SECTIONS: &[Section] = &[
    // One-click bundles of theme, effects, backdrop, summon and cursor.
    Section { id: "look.looks", tab: LOOK, title: "Looks", ..Section::DEFAULT },
    Section { id: "look.window", tab: LOOK, title: "Opacity & corners", ..Section::DEFAULT },
    // The background layer: a mode, then only the rows that mode uses.
    Section { id: "look.backdrop", tab: LOOK, title: "Backdrop", ..Section::DEFAULT },
    // Tab look, close buttons, titles, progress, the window border.
    Section { id: "look.chrome", tab: LOOK, title: "Tabs & border", ..Section::DEFAULT },
    // Following the system light/dark setting, the light theme, minimum contrast.
    Section { id: "look.appearance", tab: LOOK, title: "Appearance", ..Section::DEFAULT },
    // The gallery is long (every theme): last, so nothing hides below it.
    Section { id: "look.theme", tab: LOOK, title: "Theme", ..Section::DEFAULT },
    Section { id: "fonts.terminal", tab: FONTS, title: "Terminal font", ..Section::DEFAULT },
    Section { id: "fonts.ui", tab: FONTS, title: "Interface font", ..Section::DEFAULT },
    // Line height, built-in glyphs, color emoji, bold is bright.
    Section { id: "fonts.render", tab: FONTS, title: "Rendering", ..Section::DEFAULT },
    Section { id: "window.summon", tab: WINDOW, title: "Summon", ..Section::DEFAULT },
    Section { id: "window.dropdown", tab: WINDOW, title: "Dropdown", ..Section::DEFAULT },
    // Tab bar position, scrollbar, grid padding, scrollback.
    Section { id: "window.layout", tab: WINDOW, title: "Layout", ..Section::DEFAULT },
    // Hook: reduce motion.
    Section { id: "window.motion", tab: WINDOW, title: "Motion", ..Section::DEFAULT },
    Section { id: "shell.startup", tab: SHELL, title: "Startup", ..Section::DEFAULT },
    Section {
        id: "shell.notify",
        tab: SHELL,
        title: "Run & Notify",
        master: Some("notify_on_command_finish"),
        hint: Some("Notify on finish while hidden"),
    },
    // Effect presets (Clean, Retro CRT, Amber, Green Phosphor, Neon, Paper, E-ink).
    Section { id: "fx.presets", tab: EFFECTS, title: "Presets", ..Section::DEFAULT },
    Section { id: "fx.crt", tab: EFFECTS, title: "CRT", master: Some("effects.crt_enabled"), ..Section::DEFAULT },
    // The cursor's shape, look and trail, and the caret flash / glow.
    Section { id: "fx.cursor", tab: EFFECTS, title: "Cursor", ..Section::DEFAULT },
    // The glitch, the visual bell, the command pulse.
    Section { id: "fx.bell", tab: EFFECTS, title: "Alerts", ..Section::DEFAULT },
];

// ── get / set helpers for `Desc` literals ─────────────────────────────────────

/// `get` / `set` for an `f32` config field, by path.
macro_rules! get_f {
    ($($p:ident).+) => { |c: &Config| Val::F(c.$($p).+) };
}
macro_rules! set_f {
    ($($p:ident).+) => { |c: &mut Config, v: Val| { if let Val::F(x) = v { c.$($p).+ = x; } } };
}
/// `get` / `set` for a `bool` config field.
macro_rules! get_b {
    ($($p:ident).+) => { |c: &Config| Val::B(c.$($p).+) };
}
macro_rules! set_b {
    ($($p:ident).+) => { |c: &mut Config, v: Val| { if let Val::B(x) = v { c.$($p).+ = x; } } };
}
/// `get` / `set` for a `String` config field.
macro_rules! get_s {
    ($($p:ident).+) => { |c: &Config| Val::S(c.$($p).+.clone()) };
}
macro_rules! set_s {
    ($($p:ident).+) => { |c: &mut Config, v: Val| { if let Val::S(x) = v { c.$($p).+ = x; } } };
}
/// `get` / `set` for an `[f32; 3]` color.
macro_rules! get_rgb {
    ($($p:ident).+) => { |c: &Config| Val::Rgb(c.$($p).+) };
}
macro_rules! set_rgb {
    ($($p:ident).+) => { |c: &mut Config, v: Val| { if let Val::Rgb(x) = v { c.$($p).+ = x; } } };
}

// ── Readouts ──────────────────────────────────────────────────────────────────

pub fn fmt_pct(v: f32) -> String {
    format!("{}%", (v * 100.0).round() as i32)
}
pub fn fmt_px(v: f32) -> String {
    format!("{}px", v.round() as i32)
}
pub fn fmt_pt(v: f32) -> String {
    format!("{}pt", v.round() as i32)
}
pub fn fmt_ms(v: f32) -> String {
    format!("{}ms", v.round() as i32)
}
/// A multiplier: "1.30×".
pub fn fmt_mult(v: f32) -> String {
    format!("{v:.2}×")
}
/// A contrast ratio: "4.5:1", or "Off" at 1:1 (the no-op ratio).
pub fn fmt_contrast(v: f32) -> String {
    if v <= 1.0 + 1e-3 { "Off".to_string() } else { format!("{v:.1}:1") }
}
/// Whole thousands render as "Nk" (the cycler steps); anything else — a
/// hand-edited value — verbatim.
pub fn fmt_scrollback(n: u64) -> String {
    if n >= 1000 && n.is_multiple_of(1000) { format!("{}k", n / 1000) } else { n.to_string() }
}
/// "10s", "2m".
pub fn fmt_secs(n: u64) -> String {
    if n >= 60 && n.is_multiple_of(60) { format!("{}m", n / 60) } else { format!("{n}s") }
}

/// A 0..=1 slider shown as a percentage (most effect strengths).
pub const PCT: Kind = Kind::Slider { min: 0.0, max: 1.0, step: 0.0, fmt: fmt_pct, live: true };

/// Scrollback-cycler steps. 100_000 is alacritty's own UI max (and the config
/// clamp ceiling): at ≤24 B/cell a fully-filled 100k×120-col history is
/// ~290 MB per tab, so do not raise it without revisiting memory.
pub const SCROLLBACK_STEPS: [u64; 6] = [1_000, 5_000, 10_000, 25_000, 50_000, 100_000];

/// Notify minimum-duration steps, in seconds: "I stepped away" granularity.
pub const NOTIFY_MIN_STEPS: [u64; 6] = [5, 10, 30, 60, 120, 300];

/// The next / previous step (wraps). A value between steps first snaps to
/// its NEAREST step, then moves ±1 — so the first click from a hand-edited
/// value lands on a canonical one instead of jumping erratically.
pub fn cycle_steps(steps: &[u64], cur: u64, forward: bool) -> u64 {
    let Some(i) = steps.iter().enumerate().min_by_key(|(_, &s)| s.abs_diff(cur)).map(|(i, _)| i) else {
        return cur;
    };
    let n = steps.len();
    steps[if forward { (i + 1) % n } else { (i + n - 1) % n }]
}

/// The last path component ("/usr/bin/zsh" → "zsh"); "" → "System default".
pub fn shell_label(path: &str) -> String {
    if path.is_empty() {
        return "System default".to_string();
    }
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn shell_choices(ctx: &Ctx) -> Vec<(String, String)> {
    std::iter::once((String::new(), shell_label("")))
        .chain(ctx.shells.iter().map(|p| (p.clone(), shell_label(p))))
        .collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

/// "#rrggbb" → 0..1 channels (`None` for anything else).
pub fn hex_rgb(s: &str) -> Option<[f32; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let ch = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
    Some([ch(0)?, ch(2)?, ch(4)?])
}

/// 0..1 channels → "#rrggbb".
pub fn rgb_hex(c: [f32; 3]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// The two gradient stops a "custom colors" backdrop starts from: the theme's
/// blue and magenta.
fn seed_stops(c: &Config) -> [String; 2] {
    let t = jetty_core::Theme::by_name(&c.theme);
    let f = |rgb: [u8; 3]| rgb_hex(rgb.map(|v| v as f32 / 255.0));
    [f(t.palette[4]), f(t.palette[5])]
}

/// Gradient stop `i`: the configured one, else the seed (both are written
/// together the first time one is edited).
fn stop(c: &Config, i: usize) -> [f32; 3] {
    c.backdrop
        .colors
        .get(i)
        .and_then(|s| hex_rgb(s))
        .unwrap_or_else(|| hex_rgb(&seed_stops(c)[i]).unwrap_or([0.5; 3]))
}

fn set_stop(c: &mut Config, i: usize, v: Val) {
    if let Val::Rgb(x) = v {
        if c.backdrop.colors.len() < 2 {
            let seed = seed_stops(c);
            c.backdrop.colors = seed.to_vec();
        }
        c.backdrop.colors[i] = rgb_hex(x);
    }
}

/// Backdrop rows by mode.
fn bd_on(c: &Config) -> bool {
    !c.backdrop.mode.eq_ignore_ascii_case("none") && !c.backdrop.mode.is_empty()
}
fn bd_mode(c: &Config, m: &str) -> bool {
    c.backdrop.mode.eq_ignore_ascii_case(m)
}
fn bd_moves(c: &Config) -> bool {
    bd_mode(c, "gradient") || bd_mode(c, "pattern") || bd_mode(c, "theme")
}

fn fmt_deg(v: f32) -> String {
    format!("{}°", v.round() as i32)
}

/// The light themes of the registry (built-in and user), `(name, display)`.
fn light_theme_choices() -> Vec<(String, String)> {
    jetty_render::gallery_order(jetty_render::ThemeFilter::Light)
        .into_iter()
        .map(|i| {
            let t = jetty_core::theme_at(i);
            (t.name.to_string(), t.display_name.to_string())
        })
        .collect()
}

/// The light theme only shows while following the system (still settable).
fn light_theme_state(c: &Config, _: &Ctx) -> RowState {
    if c.follow_system_theme { RowState::Normal } else { RowState::Dimmed }
}

/// The cursor stroke thickness does nothing for a block cursor.
fn thickness_state(c: &Config, _: &Ctx) -> RowState {
    if crate::motion::CursorShapePref::parse(&c.cursor.shape) == crate::motion::CursorShapePref::Block {
        RowState::Dimmed
    } else {
        RowState::Normal
    }
}
/// The trail's timing rows do nothing without the trail.
fn trail_state(c: &Config, _: &Ctx) -> RowState {
    if c.cursor.trail { RowState::Normal } else { RowState::Dimmed }
}
fn fmt_cells(v: f32) -> String {
    let n = v.round() as i32;
    if n == 1 { "1 cell".to_string() } else { format!("{n} cells") }
}

/// Phosphor rows matter only with a phosphor (the color: only for "custom").
fn phosphor_hue_state(c: &Config, _: &Ctx) -> RowState {
    if c.effects.crt_phosphor == crate::config::PhosphorMode::Off { RowState::Dimmed } else { RowState::Normal }
}
fn phosphor_color_state(c: &Config, _: &Ctx) -> RowState {
    if c.effects.crt_phosphor == crate::config::PhosphorMode::Custom { RowState::Normal } else { RowState::Dimmed }
}
/// Animating the grain does nothing without grain.
fn grain_animate_state(c: &Config, _: &Ctx) -> RowState {
    if c.effects.crt_grain > 0.0 { RowState::Normal } else { RowState::Dimmed }
}

/// The dropdown sliders do nothing outside Dropdown mode or while the main
/// window is fullscreen (docking a fullscreen window would yank it into a
/// squared-off strip) — so they are inert there, like `dock_reassert_ok`.
fn dropdown_state(c: &Config, x: &Ctx) -> RowState {
    if crate::app::dropdown_controls_live(&c.window_mode, x.main_fullscreen) {
        RowState::Normal
    } else {
        RowState::Disabled
    }
}

/// The corner radius is suppressed while fullscreen (a rounded fullscreen
/// window would show the desktop through four notches): dimmed, still live.
fn radius_state(c: &Config, x: &Ctx) -> RowState {
    if crate::app::corner_radius_dimmed_for(x.main_fullscreen, &c.window_mode) {
        RowState::Dimmed
    } else {
        RowState::Normal
    }
}

/// Every control, in display order within its section.
pub static DESCS: &[Desc] = &[
    // ── Look ──────────────────────────────────────────────────────────────────
    // ── Look › Looks ──────────────────────────────────────────────────────────
    Desc {
        id: "look",
        section: "look.looks",
        label: "Looks",
        kind: Kind::Looks,
        hint: Some("Theme, effects, backdrop and cursor"),
        reset: false,
        ..Desc::DEFAULT
    },

    Desc {
        id: "opacity",
        section: "look.window",
        label: "Opacity",
        kind: Kind::Slider { min: 0.1, max: 1.0, step: 0.0, fmt: fmt_pct, live: true },
        get: get_f!(opacity),
        set: set_f!(opacity),
        ..Desc::DEFAULT
    },
    Desc {
        id: "corner_radius",
        section: "look.window",
        label: "Corner radius",
        kind: Kind::Slider { min: 0.0, max: 24.0, step: 0.0, fmt: fmt_px, live: true },
        get: get_f!(corner_radius),
        set: set_f!(corner_radius),
        state: Some(radius_state),
        ..Desc::DEFAULT
    },
    // ── Look › Backdrop ───────────────────────────────────────────────────────
    Desc {
        id: "backdrop.mode",
        section: "look.backdrop",
        label: "Backdrop",
        kind: Kind::Choice {
            options: |_| {
                pairs(&[
                    ("none", "Off"),
                    ("theme", "Theme"),
                    ("gradient", "Gradient"),
                    ("image", "Image"),
                    ("pattern", "Pattern"),
                ])
            },
        },
        get: get_s!(backdrop.mode),
        set: set_s!(backdrop.mode),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.image",
        section: "look.backdrop",
        label: "Image",
        kind: Kind::Choice {
            options: |x| {
                std::iter::once((String::new(), "None".to_string()))
                    .chain(x.backdrop_images.iter().map(|n| (n.clone(), n.clone())))
                    .collect()
            },
        },
        get: get_s!(backdrop.image),
        set: set_s!(backdrop.image),
        hint: Some("From backgrounds/, or drop a file here"),
        visible: Some(|c| bd_mode(c, "image")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.fit",
        section: "look.backdrop",
        label: "Fit",
        kind: Kind::Choice {
            options: |_| {
                pairs(&[
                    ("cover", "Cover"),
                    ("contain", "Contain"),
                    ("stretch", "Stretch"),
                    ("center", "Center"),
                    ("tile", "Tile"),
                ])
            },
        },
        get: get_s!(backdrop.fit),
        set: set_s!(backdrop.fit),
        visible: Some(|c| bd_mode(c, "image")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.dim",
        section: "look.backdrop",
        label: "Dim",
        kind: PCT,
        get: get_f!(backdrop.dim),
        set: set_f!(backdrop.dim),
        visible: Some(|c| bd_mode(c, "image")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.blur",
        section: "look.backdrop",
        label: "Blur",
        kind: PCT,
        get: get_f!(backdrop.blur),
        set: set_f!(backdrop.blur),
        visible: Some(|c| bd_mode(c, "image")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.pattern",
        section: "look.backdrop",
        label: "Pattern",
        kind: Kind::Choice {
            options: |_| {
                pairs(&[("stars", "Stars"), ("aurora", "Aurora"), ("grid", "Grid"), ("synthwave", "Synthwave")])
            },
        },
        get: get_s!(backdrop.pattern),
        set: set_s!(backdrop.pattern),
        visible: Some(|c| bd_mode(c, "pattern")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.angle",
        section: "look.backdrop",
        label: "Angle",
        kind: Kind::Slider { min: 0.0, max: 360.0, step: 1.0, fmt: fmt_deg, live: true },
        get: get_f!(backdrop.angle),
        set: set_f!(backdrop.angle),
        visible: Some(|c| bd_mode(c, "gradient")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.shape",
        section: "look.backdrop",
        label: "Shape",
        kind: Kind::Choice { options: |_| pairs(&[("linear", "Linear"), ("radial", "Radial")]) },
        get: get_s!(backdrop.shape),
        set: set_s!(backdrop.shape),
        visible: Some(|c| bd_mode(c, "gradient")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.custom_colors",
        section: "look.backdrop",
        label: "Custom colors",
        kind: Kind::Toggle,
        get: |c| Val::B(!c.backdrop.colors.is_empty()),
        set: |c, v| {
            if let Val::B(on) = v {
                c.backdrop.colors = if on { seed_stops(c).to_vec() } else { Vec::new() };
            }
        },
        hint: Some("Off: colors from the theme"),
        visible: Some(|c| bd_mode(c, "gradient")),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.color_start",
        section: "look.backdrop",
        label: "Start color",
        kind: Kind::Rgb,
        get: |c| Val::Rgb(stop(c, 0)),
        set: |c, v| set_stop(c, 0, v),
        visible: Some(|c| bd_mode(c, "gradient") && !c.backdrop.colors.is_empty()),
        reset: false,
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.color_end",
        section: "look.backdrop",
        label: "End color",
        kind: Kind::Rgb,
        get: |c| Val::Rgb(stop(c, 1)),
        set: |c, v| set_stop(c, 1, v),
        visible: Some(|c| bd_mode(c, "gradient") && !c.backdrop.colors.is_empty()),
        reset: false,
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.strength",
        section: "look.backdrop",
        label: "Strength",
        kind: PCT,
        get: get_f!(backdrop.strength),
        set: set_f!(backdrop.strength),
        visible: Some(bd_on),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.vignette",
        section: "look.backdrop",
        label: "Vignette",
        kind: PCT,
        get: get_f!(backdrop.vignette),
        set: set_f!(backdrop.vignette),
        visible: Some(bd_on),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.grain",
        section: "look.backdrop",
        label: "Grain",
        kind: PCT,
        get: get_f!(backdrop.grain),
        set: set_f!(backdrop.grain),
        visible: Some(bd_on),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.animate",
        section: "look.backdrop",
        label: "Animate",
        kind: Kind::Toggle,
        get: get_b!(backdrop.animate),
        set: set_b!(backdrop.animate),
        hint: Some("Slow drift, up to 30 fps"),
        visible: Some(bd_moves),
        ..Desc::DEFAULT
    },
    Desc {
        id: "backdrop.parallax",
        section: "look.backdrop",
        label: "Parallax",
        kind: Kind::Toggle,
        get: get_b!(backdrop.parallax),
        set: set_b!(backdrop.parallax),
        hint: Some("Moves with the scrollback"),
        visible: Some(bd_on),
        ..Desc::DEFAULT
    },
    // ── Look › Tabs & border ──────────────────────────────────────────────────
    Desc {
        id: "tab_style",
        section: "look.chrome",
        label: "Tab style",
        kind: Kind::Choice {
            options: |_| {
                jetty_render::TabStyle::ALL
                    .iter()
                    .map(|s| (s.to_config().to_string(), s.display_name().to_string()))
                    .collect()
            },
        },
        get: get_s!(tab_style),
        set: set_s!(tab_style),
        ..Desc::DEFAULT
    },
    Desc {
        id: "tab_close_button",
        section: "look.chrome",
        label: "Close buttons",
        kind: Kind::Choice {
            options: |_| {
                jetty_render::CloseButton::ALL
                    .iter()
                    .map(|m| (m.to_config().to_string(), m.display_name().to_string()))
                    .collect()
            },
        },
        get: get_s!(tab_close_button),
        set: set_s!(tab_close_button),
        ..Desc::DEFAULT
    },
    Desc {
        id: "tab_title",
        section: "look.chrome",
        label: "Tab titles",
        kind: Kind::Choice { options: |_| pairs(&[("osc", "Program"), ("auto", "Smart")]) },
        get: get_s!(tab_title),
        set: set_s!(tab_title),
        hint: Some("Smart: the command, else the folder"),
        ..Desc::DEFAULT
    },
    Desc {
        id: "progress_bar",
        section: "look.chrome",
        label: "Progress in tabs",
        kind: Kind::Toggle,
        get: get_b!(progress_bar),
        set: set_b!(progress_bar),
        ..Desc::DEFAULT
    },
    Desc {
        id: "tab_bar_opacity",
        section: "look.chrome",
        label: "Translucent tab bar",
        kind: Kind::Toggle,
        get: get_b!(tab_bar_opacity),
        set: set_b!(tab_bar_opacity),
        ..Desc::DEFAULT
    },
    Desc {
        id: "window_border",
        section: "look.chrome",
        label: "Window border",
        kind: Kind::Choice {
            options: |_| {
                crate::tabmeta::WindowBorder::ALL
                    .iter()
                    .map(|b| (b.to_config().to_string(), b.display_name().to_string()))
                    .collect()
            },
        },
        get: get_s!(window_border),
        set: set_s!(window_border),
        ..Desc::DEFAULT
    },
    // ── Look › Appearance ─────────────────────────────────────────────────────
    Desc {
        id: "follow_system_theme",
        section: "look.appearance",
        label: "Follow system light/dark",
        kind: Kind::Toggle,
        get: get_b!(follow_system_theme),
        set: set_b!(follow_system_theme),
        ..Desc::DEFAULT
    },
    Desc {
        id: "light_theme",
        section: "look.appearance",
        label: "Light theme",
        kind: Kind::Choice { options: |_| light_theme_choices() },
        get: get_s!(light_theme),
        set: set_s!(light_theme),
        hint: Some("Shown while the system is light"),
        state: Some(light_theme_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "minimum_contrast",
        section: "look.appearance",
        label: "Minimum contrast",
        kind: Kind::Slider { min: 1.0, max: 7.0, step: 0.1, fmt: fmt_contrast, live: true },
        get: get_f!(minimum_contrast),
        set: set_f!(minimum_contrast),
        ..Desc::DEFAULT
    },
    Desc {
        id: "theme",
        section: "look.theme",
        label: "Theme",
        kind: Kind::Gallery,
        get: get_s!(theme),
        set: set_s!(theme),
        reset: false,
        ..Desc::DEFAULT
    },
    // ── Fonts ─────────────────────────────────────────────────────────────────
    Desc {
        id: "font_size",
        tab: FONTS,
        section: "fonts.terminal",
        label: "Font size",
        kind: Kind::Stepper { min: 6.0, max: 48.0, step: 1.0, fmt: fmt_pt },
        get: get_f!(font_size),
        set: set_f!(font_size),
        ..Desc::DEFAULT
    },
    Desc {
        id: "font_family",
        tab: FONTS,
        section: "fonts.terminal",
        label: "Font",
        kind: Kind::List { src: ListSrc::Mono, rows: 5 },
        get: get_s!(font_family),
        set: set_s!(font_family),
        ..Desc::DEFAULT
    },
    Desc {
        id: "ui_font_size",
        tab: FONTS,
        section: "fonts.ui",
        label: "UI font size",
        kind: Kind::Stepper { min: 10.0, max: 28.0, step: 1.0, fmt: fmt_pt },
        get: get_f!(ui_font_size),
        set: set_f!(ui_font_size),
        ..Desc::DEFAULT
    },
    Desc {
        id: "ui_font_specimen",
        tab: FONTS,
        section: "fonts.ui",
        label: "Specimen",
        kind: Kind::Specimen,
        reset: false,
        ..Desc::DEFAULT
    },
    Desc {
        id: "ui_font_family",
        tab: FONTS,
        section: "fonts.ui",
        label: "UI font",
        kind: Kind::List { src: ListSrc::Ui, rows: 4 },
        get: get_s!(ui_font_family),
        set: set_s!(ui_font_family),
        ..Desc::DEFAULT
    },
    // ── Fonts › Rendering ─────────────────────────────────────────────────────
    Desc {
        id: "line_height",
        tab: FONTS,
        section: "fonts.render",
        label: "Line height",
        kind: Kind::Slider {
            min: jetty_render::LINE_HEIGHT_MIN,
            max: jetty_render::LINE_HEIGHT_MAX,
            step: 0.05,
            fmt: fmt_mult,
            live: true,
        },
        get: get_f!(line_height),
        set: set_f!(line_height),
        ..Desc::DEFAULT
    },
    Desc {
        id: "builtin_glyphs",
        tab: FONTS,
        section: "fonts.render",
        label: "Built-in box & block glyphs",
        kind: Kind::Toggle,
        get: get_b!(builtin_glyphs),
        set: set_b!(builtin_glyphs),
        hint: Some("Lines, blocks, Powerline, braille"),
        ..Desc::DEFAULT
    },
    Desc {
        id: "color_emoji",
        tab: FONTS,
        section: "fonts.render",
        label: "Color emoji",
        kind: Kind::Toggle,
        get: get_b!(color_emoji),
        set: set_b!(color_emoji),
        ..Desc::DEFAULT
    },
    Desc {
        id: "bold_is_bright",
        tab: FONTS,
        section: "fonts.render",
        label: "Bold is bright",
        kind: Kind::Toggle,
        get: get_b!(bold_is_bright),
        set: set_b!(bold_is_bright),
        hint: Some("Bold text in its bright ANSI color"),
        ..Desc::DEFAULT
    },
    // ── Window ────────────────────────────────────────────────────────────────
    Desc {
        id: "summon_effect",
        tab: WINDOW,
        section: "window.summon",
        label: "Summon effect",
        kind: Kind::Choice { options: |_| crate::app::summon_effect_choices() },
        get: get_s!(summon_effect),
        set: set_s!(summon_effect),
        ..Desc::DEFAULT
    },
    Desc {
        id: "reduce_motion",
        tab: WINDOW,
        section: "window.summon",
        label: "Reduce motion",
        kind: Kind::Choice { options: |_| pairs(&[("off", "Off"), ("on", "On"), ("system", "System")]) },
        get: get_s!(reduce_motion),
        set: set_s!(reduce_motion),
        hint: Some("System follows the desktop (Linux)"),
        ..Desc::DEFAULT
    },
    Desc {
        id: "window_mode",
        tab: WINDOW,
        section: "window.summon",
        label: "Window mode",
        kind: Kind::Choice { options: |_| crate::app::window_mode_choices() },
        get: get_s!(window_mode),
        set: set_s!(window_mode),
        ..Desc::DEFAULT
    },
    Desc {
        id: "focus_autohide",
        tab: WINDOW,
        section: "window.summon",
        label: "Auto-hide on focus loss",
        kind: Kind::Toggle,
        get: get_b!(focus_autohide),
        set: set_b!(focus_autohide),
        ..Desc::DEFAULT
    },
    Desc {
        id: "dropdown_height_pct",
        tab: WINDOW,
        section: "window.dropdown",
        label: "Dropdown height",
        kind: Kind::Slider { min: 0.25, max: 1.0, step: 0.0, fmt: fmt_pct, live: false },
        get: get_f!(dropdown_height_pct),
        set: set_f!(dropdown_height_pct),
        state: Some(dropdown_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "dropdown_width_pct",
        tab: WINDOW,
        section: "window.dropdown",
        label: "Dropdown width",
        kind: Kind::Slider { min: 0.2, max: 1.0, step: 0.0, fmt: fmt_pct, live: false },
        get: get_f!(dropdown_width_pct),
        set: set_f!(dropdown_width_pct),
        state: Some(dropdown_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "tab_bar_position",
        tab: WINDOW,
        section: "window.layout",
        label: "Tab bar",
        kind: Kind::Choice { options: |_| pairs(&[("top", "Top"), ("bottom", "Bottom")]) },
        get: get_s!(tab_bar_position),
        set: set_s!(tab_bar_position),
        ..Desc::DEFAULT
    },
    Desc {
        id: "scrollbar",
        tab: WINDOW,
        section: "window.layout",
        label: "Scrollbar",
        kind: Kind::Choice { options: |_| pairs(&[("always", "Always"), ("auto", "Auto"), ("never", "Never")]) },
        get: |c| Val::S(c.scrollbar.as_str().to_string()),
        set: |c, v| {
            if let Val::S(x) = v {
                c.scrollbar = crate::config::ScrollbarMode::parse(&x);
            }
        },
        ..Desc::DEFAULT
    },
    Desc {
        id: "padding_x",
        tab: WINDOW,
        section: "window.layout",
        label: "Side padding",
        kind: Kind::Slider { min: 0.0, max: jetty_render::PADDING_MAX, step: 1.0, fmt: fmt_px, live: true },
        get: get_f!(padding_x),
        set: set_f!(padding_x),
        ..Desc::DEFAULT
    },
    Desc {
        id: "padding_y",
        tab: WINDOW,
        section: "window.layout",
        label: "Top & bottom padding",
        kind: Kind::Slider { min: 0.0, max: jetty_render::PADDING_MAX, step: 1.0, fmt: fmt_px, live: true },
        get: get_f!(padding_y),
        set: set_f!(padding_y),
        ..Desc::DEFAULT
    },
    Desc {
        id: "scrollback_lines",
        tab: WINDOW,
        section: "window.layout",
        label: "Scrollback lines",
        kind: Kind::Steps { steps: &SCROLLBACK_STEPS, fmt: fmt_scrollback },
        get: |c| Val::U(c.scrollback_lines as u64),
        set: |c, v| {
            if let Val::U(x) = v {
                c.scrollback_lines = x as usize;
            }
        },
        ..Desc::DEFAULT
    },
    // ── Shell ─────────────────────────────────────────────────────────────────
    Desc {
        id: "shell",
        tab: SHELL,
        section: "shell.startup",
        label: "Shell",
        kind: Kind::Choice { options: shell_choices },
        get: get_s!(shell),
        set: set_s!(shell),
        hint: Some("Applies to new tabs"),
        ..Desc::DEFAULT
    },
    Desc {
        id: "launch_at_login",
        tab: SHELL,
        section: "shell.startup",
        label: "Launch at login",
        kind: Kind::Toggle,
        get: get_b!(launch_at_login),
        set: set_b!(launch_at_login),
        hint: Some("Adds a desktop autostart entry"),
        reset: false,
        special: Some(Special::LaunchAtLogin),
        ..Desc::DEFAULT
    },
    Desc {
        id: "notify_on_command_finish",
        tab: SHELL,
        section: "shell.notify",
        label: "Notify on finish",
        kind: Kind::Toggle,
        get: get_b!(notify_on_command_finish),
        set: set_b!(notify_on_command_finish),
        ..Desc::DEFAULT
    },
    Desc {
        id: "notify_only_on_failure",
        tab: SHELL,
        section: "shell.notify",
        label: "Only on failure",
        kind: Kind::Toggle,
        get: get_b!(notify_only_on_failure),
        set: set_b!(notify_only_on_failure),
        ..Desc::DEFAULT
    },
    Desc {
        id: "notify_min_seconds",
        tab: SHELL,
        section: "shell.notify",
        label: "Minimum duration",
        kind: Kind::Steps { steps: &NOTIFY_MIN_STEPS, fmt: fmt_secs },
        get: |c| Val::U(c.notify_min_seconds),
        set: |c, v| {
            if let Val::U(x) = v {
                c.notify_min_seconds = x;
            }
        },
        ..Desc::DEFAULT
    },
    Desc {
        id: "auto_summon_on_finish",
        tab: SHELL,
        section: "shell.notify",
        label: "Auto-summon when hidden",
        kind: Kind::Toggle,
        get: get_b!(auto_summon_on_finish),
        set: set_b!(auto_summon_on_finish),
        ..Desc::DEFAULT
    },
    // ── Effects ───────────────────────────────────────────────────────────────
    Desc {
        id: "effects.preset",
        tab: EFFECTS,
        section: "fx.presets",
        label: "Preset",
        kind: Kind::Presets { names: || crate::effects::effect_presets().iter().map(|p| p.name).collect() },
        get: |c| {
            let i = crate::effects::active_preset(&c.effects)
                .and_then(|a| crate::effects::effect_presets().iter().position(|p| p.id == a.id));
            Val::U(i.map_or(u64::MAX, |i| i as u64))
        },
        set: |c, v| {
            if let Some(p) = usize::try_from(v.u()).ok().and_then(|i| crate::effects::effect_presets().get(i)) {
                p.patch.apply_to(&mut c.effects);
            }
        },
        reset: false,
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_enabled",
        tab: EFFECTS,
        section: "fx.crt",
        label: "CRT enabled",
        kind: Kind::Toggle,
        get: get_b!(effects.crt_enabled),
        set: set_b!(effects.crt_enabled),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_curvature",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Curvature",
        kind: PCT,
        get: get_f!(effects.crt_curvature),
        set: set_f!(effects.crt_curvature),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_scanline",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Scanline",
        kind: PCT,
        get: get_f!(effects.crt_scanline),
        set: set_f!(effects.crt_scanline),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_mask",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Mask",
        kind: PCT,
        get: get_f!(effects.crt_mask),
        set: set_f!(effects.crt_mask),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_bloom",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Bloom",
        kind: PCT,
        get: get_f!(effects.crt_bloom),
        set: set_f!(effects.crt_bloom),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_bloom_radius",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Bloom radius",
        kind: PCT,
        get: get_f!(effects.crt_bloom_radius),
        set: set_f!(effects.crt_bloom_radius),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_chromatic",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Chromatic",
        kind: PCT,
        get: get_f!(effects.crt_chromatic),
        set: set_f!(effects.crt_chromatic),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_vignette",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Vignette",
        kind: PCT,
        get: get_f!(effects.crt_vignette),
        set: set_f!(effects.crt_vignette),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_grain",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Grain",
        kind: PCT,
        get: get_f!(effects.crt_grain),
        set: set_f!(effects.crt_grain),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_grain_animate",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Animate grain",
        kind: Kind::Toggle,
        get: get_b!(effects.crt_grain_animate),
        set: set_b!(effects.crt_grain_animate),
        state: Some(grain_animate_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_phosphor",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Phosphor",
        kind: Kind::Choice {
            options: |_| {
                crate::config::PhosphorMode::ALL
                    .iter()
                    .map(|m| (m.display_name().to_ascii_lowercase(), m.display_name().to_string()))
                    .collect()
            },
        },
        get: |c| Val::S(c.effects.crt_phosphor.display_name().to_ascii_lowercase()),
        set: |c, v| {
            if let Val::S(x) = v {
                c.effects.crt_phosphor = crate::config::PhosphorMode::from_name(&x).unwrap_or_default();
            }
        },
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_phosphor_color",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Phosphor color",
        kind: Kind::Rgb,
        get: get_rgb!(effects.crt_phosphor_color),
        set: set_rgb!(effects.crt_phosphor_color),
        hint: Some("Used by the Custom phosphor"),
        state: Some(phosphor_color_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_phosphor_hue",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Keep colors",
        kind: PCT,
        get: get_f!(effects.crt_phosphor_hue),
        set: set_f!(effects.crt_phosphor_hue),
        state: Some(phosphor_hue_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_dither",
        tab: EFFECTS,
        section: "fx.crt",
        label: "1-bit dither",
        kind: Kind::Toggle,
        get: get_b!(effects.crt_dither),
        set: set_b!(effects.crt_dither),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_scanline_tint",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Tint",
        kind: Kind::Rgb,
        get: get_rgb!(effects.crt_scanline_tint),
        set: set_rgb!(effects.crt_scanline_tint),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.crt_animate",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Animate",
        kind: Kind::Chips { labels: &["Roll", "Flicker", "Jitter"] },
        get: |c| {
            let e = &c.effects;
            Val::Bits(e.crt_animate_roll as u32 | (e.crt_flicker as u32) << 1 | (e.crt_jitter as u32) << 2)
        },
        set: |c, v| {
            if let Val::Bits(b) = v {
                c.effects.crt_animate_roll = b & 1 != 0;
                c.effects.crt_flicker = b & 2 != 0;
                c.effects.crt_jitter = b & 4 != 0;
            }
        },
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.animate_unfocused",
        tab: EFFECTS,
        section: "fx.crt",
        label: "Animate when unfocused",
        kind: Kind::Toggle,
        get: get_b!(effects.animate_unfocused),
        set: set_b!(effects.animate_unfocused),
        ..Desc::DEFAULT
    },
    // ── Effects › Cursor ──────────────────────────────────────────────────────
    Desc {
        id: "cursor.shape",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Shape",
        kind: Kind::Choice {
            options: |_| {
                pairs(&[
                    ("block", "Block"),
                    ("beam", "Beam"),
                    ("underline", "Underline"),
                    ("double_underline", "Double underline"),
                    ("thick_underline", "Thick underline"),
                ])
            },
        },
        get: get_s!(cursor.shape),
        set: set_s!(cursor.shape),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.thickness",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Thickness",
        kind: Kind::Slider { min: 0.04, max: 0.5, step: 0.01, fmt: fmt_pct, live: true },
        get: get_f!(cursor.thickness),
        set: set_f!(cursor.thickness),
        state: Some(thickness_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.unfocused",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Unfocused",
        kind: Kind::Choice {
            options: |_| pairs(&[("hollow", "Hollow"), ("unchanged", "Unchanged"), ("none", "Hidden")]),
        },
        get: get_s!(cursor.unfocused),
        set: set_s!(cursor.unfocused),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.color",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Color",
        kind: Kind::Choice { options: |_| pairs(&[("theme", "Theme"), ("cell", "Reverse"), ("auto", "Auto")]) },
        get: get_s!(cursor.color),
        set: set_s!(cursor.color),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.guide",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Row guide",
        kind: Kind::Choice { options: |_| pairs(&[("off", "Off"), ("shell", "Shell"), ("always", "Always")]) },
        get: get_s!(cursor.guide),
        set: set_s!(cursor.guide),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.trail",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Trail",
        kind: Kind::Toggle,
        get: get_b!(cursor.trail),
        set: set_b!(cursor.trail),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.trail_ms",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Trail duration",
        kind: Kind::Slider { min: 60.0, max: 600.0, step: 10.0, fmt: fmt_ms, live: true },
        get: |c| Val::F(c.cursor.trail_ms as f32),
        set: |c, v| {
            if let Val::F(x) = v {
                c.cursor.trail_ms = x.round().clamp(60.0, 1000.0) as u32;
            }
        },
        state: Some(trail_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "cursor.trail_threshold",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Trail threshold",
        kind: Kind::Slider { min: 1.0, max: 10.0, step: 1.0, fmt: fmt_cells, live: true },
        get: |c| Val::F(c.cursor.trail_threshold as f32),
        set: |c, v| {
            if let Val::F(x) = v {
                c.cursor.trail_threshold = x.round().clamp(1.0, 40.0) as u32;
            }
        },
        hint: Some("A jump must cover more cells to trail"),
        state: Some(trail_state),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.caret_flash_enabled",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Caret flash",
        kind: Kind::Toggle,
        get: get_b!(effects.caret_flash_enabled),
        set: set_b!(effects.caret_flash_enabled),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.caret_glow_enabled",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Caret glow",
        kind: Kind::Toggle,
        get: get_b!(effects.caret_glow_enabled),
        set: set_b!(effects.caret_glow_enabled),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.caret_flash_ms",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Flash duration",
        kind: Kind::Slider { min: 60.0, max: 400.0, step: 0.0, fmt: fmt_ms, live: true },
        get: get_f!(effects.caret_flash_ms),
        set: set_f!(effects.caret_flash_ms),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.caret_flash_color",
        tab: EFFECTS,
        section: "fx.cursor",
        label: "Flash color",
        kind: Kind::Rgb,
        get: get_rgb!(effects.caret_flash_color),
        set: set_rgb!(effects.caret_flash_color),
        ..Desc::DEFAULT
    },
    Desc {
        id: "effects.glitch",
        tab: EFFECTS,
        section: "fx.bell",
        label: "Glitch on",
        kind: Kind::Chips { labels: &["Failure", "Bell"] },
        get: |c| Val::Bits(c.effects.glitch_on_error as u32 | (c.effects.glitch_on_bell as u32) << 1),
        set: |c, v| {
            if let Val::Bits(b) = v {
                c.effects.glitch_on_error = b & 1 != 0;
                c.effects.glitch_on_bell = b & 2 != 0;
            }
        },
        hint: Some("Color split on a failure or the bell"),
        ..Desc::DEFAULT
    },
    Desc {
        id: "visual_bell",
        tab: EFFECTS,
        section: "fx.bell",
        label: "Visual bell",
        kind: Kind::Choice { options: |_| pairs(&[("off", "Off"), ("flash", "Flash"), ("rim", "Rim")]) },
        get: get_s!(visual_bell),
        set: set_s!(visual_bell),
        ..Desc::DEFAULT
    },
    Desc {
        id: "command_pulse",
        tab: EFFECTS,
        section: "fx.bell",
        label: "Command pulse",
        kind: Kind::Choice { options: |_| pairs(&[("off", "Off"), ("failures", "Failures"), ("all", "All")]) },
        get: get_s!(command_pulse),
        set: set_s!(command_pulse),
        hint: Some("Needs shell integration (OSC 133)"),
        ..Desc::DEFAULT
    },
];

/// The image files in `<config dir>/backgrounds/` (the backdrop picker's
/// list) — for jetty-shot; the app caches its own copy.
pub fn backdrop_images() -> Vec<String> {
    crate::backdrop::background_images(&Config::dir())
}

/// A one-click "look": a bundle of keys a click writes (no look name is
/// stored — a look is lit while every key it sets still holds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookDef {
    pub name: &'static str,
    /// The theme id; `None` leaves the theme alone.
    pub theme: Option<&'static str>,
    /// An effects preset id (`effects::effect_presets`).
    pub preset: &'static str,
    pub backdrop: &'static str,
    pub pattern: Option<&'static str>,
    pub strength: Option<f32>,
    pub summon: &'static str,
    pub cursor_shape: Option<&'static str>,
    pub trail: bool,
    pub guide: Option<&'static str>,
}

impl LookDef {
    const DEFAULT: LookDef = LookDef {
        name: "",
        theme: None,
        preset: "clean",
        backdrop: "none",
        pattern: None,
        strength: None,
        summon: "phosphor",
        cursor_shape: None,
        trail: false,
        guide: None,
    };
}

/// The looks, in chip / palette order — one array to tune.
pub static LOOKS: &[LookDef] = &[
    LookDef {
        name: "Amber VT",
        theme: Some("phosphor_amber"),
        preset: "amber",
        summon: "phosphor",
        cursor_shape: Some("block"),
        ..LookDef::DEFAULT
    },
    LookDef {
        name: "P1 Green",
        theme: Some("phosphor_green"),
        preset: "green_phosphor",
        summon: "phosphor",
        cursor_shape: Some("block"),
        ..LookDef::DEFAULT
    },
    LookDef {
        name: "Trinitron",
        theme: Some("tokyo_night"),
        preset: "retro_crt",
        backdrop: "theme",
        summon: "focus",
        ..LookDef::DEFAULT
    },
    LookDef {
        name: "Neon Night",
        theme: Some("synthwave_84"),
        preset: "neon",
        backdrop: "pattern",
        pattern: Some("synthwave"),
        strength: Some(0.5),
        summon: "pop",
        trail: true,
        ..LookDef::DEFAULT
    },
    LookDef {
        name: "Aurora",
        theme: Some("tokyo_night_storm"),
        preset: "clean",
        backdrop: "pattern",
        pattern: Some("aurora"),
        strength: Some(0.5),
        summon: "glide",
        trail: true,
        ..LookDef::DEFAULT
    },
    LookDef { name: "Paper", theme: Some("flexoki_light"), preset: "paper", summon: "fade", ..LookDef::DEFAULT },
    LookDef { name: "Clean", preset: "clean", summon: "phosphor", guide: Some("off"), ..LookDef::DEFAULT },
];

/// Write every key of `look` but the theme (the app picks that one, into the
/// slot on screen) into `c`.
pub fn apply_look_keys(c: &mut Config, look: &LookDef) {
    if let Some(p) = crate::effects::find_preset(look.preset) {
        p.patch.apply_to(&mut c.effects);
    }
    c.backdrop.mode = look.backdrop.to_string();
    if let Some(p) = look.pattern {
        c.backdrop.pattern = p.to_string();
    }
    if let Some(v) = look.strength {
        c.backdrop.strength = v;
    }
    c.summon_effect = look.summon.to_string();
    if let Some(sh) = look.cursor_shape {
        c.cursor.shape = sh.to_string();
    }
    c.cursor.trail = look.trail;
    if let Some(g) = look.guide {
        c.cursor.guide = g.to_string();
    }
}

/// Whether every key `look` sets holds in `c`, its theme compared with
/// `shown_theme` (the theme on screen).
pub fn look_matches(c: &Config, look: &LookDef, shown_theme: &str) -> bool {
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    look.theme.is_none_or(|t| t == shown_theme)
        && crate::effects::find_preset(look.preset).is_some_and(|p| crate::effects::matches_preset(&c.effects, p))
        && eq(&c.backdrop.mode, look.backdrop)
        && look.pattern.is_none_or(|p| eq(&c.backdrop.pattern, p))
        && look.strength.is_none_or(|v| (c.backdrop.strength - v).abs() < 1e-3)
        && eq(&c.summon_effect, look.summon)
        && look.cursor_shape.is_none_or(|sh| eq(&c.cursor.shape, sh))
        && c.cursor.trail == look.trail
        && look.guide.is_none_or(|g| eq(&c.cursor.guide, g))
}

/// The control `id`.
pub fn find(id: &str) -> Option<&'static Desc> {
    DESCS.iter().find(|d| d.id == id)
}

/// The section `id`.
pub fn section(id: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.id == id)
}

/// A control's default: its value in `Config::default()`.
pub fn default_of(d: &Desc) -> Val {
    (d.get)(&Config::default())
}

/// `cfg` with every resettable control of `tab` back at its default.
pub fn reset_tab(cfg: &Config, tab: usize) -> Config {
    let mut c = cfg.clone();
    for d in DESCS.iter().filter(|d| d.tab == tab && d.reset && d.is_setting()) {
        (d.set)(&mut c, default_of(d));
    }
    c
}

/// Whether "Reset tab" would change nothing on `tab`.
pub fn tab_at_defaults(cfg: &Config, tab: usize) -> bool {
    reset_tab(cfg, tab) == *cfg
}

/// A list control's items, first visible row and shown value.
fn list_src<'a>(src: ListSrc, ctx: &'a Ctx) -> (&'a [String], usize, &'a str) {
    match src {
        ListSrc::Mono => (ctx.mono_families, ctx.font_offset, ctx.font_shown),
        ListSrc::Ui => (ctx.ui_families, ctx.ui_font_offset, ctx.ui_font_shown),
    }
}

/// Rows list control `id` shows at once (1 for anything else).
pub fn list_rows(id: &str) -> usize {
    match find(id).map(|d| &d.kind) {
        Some(Kind::List { rows, .. }) => *rows,
        _ => 1,
    }
}

/// The first visible row that shows item `pos` near the middle of a `rows`-row
/// list of `len` items (0 when nothing is selected) — where a list opens.
pub fn list_offset_showing(len: usize, pos: Option<usize>, rows: usize) -> usize {
    pos.map_or(0, |i| i.saturating_sub(rows / 2).min(len.saturating_sub(rows)))
}

/// The config value of list row `i`.
fn list_value(src: ListSrc, items: &[String], i: usize) -> Option<String> {
    match src {
        ListSrc::Ui if i == 0 => Some(String::new()),
        _ => items.get(i).cloned(),
    }
}

/// The row showing `value` (else the shown fallback).
fn list_selected(src: ListSrc, items: &[String], value: &str, shown: &str) -> Option<usize> {
    let pos = |v: &str| {
        if src == ListSrc::Ui && v.is_empty() {
            Some(0)
        } else {
            items.iter().position(|n| n == v)
        }
    };
    pos(value).or_else(|| pos(shown))
}

/// A choice value's label: its option's label, else the raw value (a path
/// shows its file name).
fn choice_label(options: &[(String, String)], value: &str) -> String {
    match options.iter().find(|o| o.0 == value) {
        Some(o) => o.1.clone(),
        None if value.contains('/') => shell_label(value),
        None => value.to_string(),
    }
}

/// The value `d` shows: a pending release-applied drag, else the config.
fn shown_value(d: &Desc, cfg: &Config, ctx: &Ctx) -> Val {
    match ctx.drag {
        Some((id, v)) if id == d.id => v.clone(),
        _ => (d.get)(cfg),
    }
}

/// The panel row for control `d`.
fn row_for(d: &Desc, cfg: &Config, ctx: &Ctx, master_off: bool) -> PanelItem {
    let v = shown_value(d, cfg, ctx);
    let show = match &d.kind {
        Kind::Gallery => return PanelItem::Gallery,
        Kind::Specimen => return PanelItem::Specimen,
        Kind::Slider { min, max, fmt, .. } => {
            let f = v.f();
            let frac = if max > min { (f - min) / (max - min) } else { 0.0 };
            CtlShow::Slider { frac: frac.clamp(0.0, 1.0), text: fmt(f) }
        }
        Kind::Toggle => CtlShow::Toggle(v.b()),
        Kind::Choice { options } => CtlShow::Cycler(choice_label(&options(ctx), v.s())),
        Kind::Steps { fmt, .. } => CtlShow::Cycler(fmt(v.u())),
        Kind::Stepper { fmt, .. } => CtlShow::Stepper(fmt(v.f())),
        Kind::Rgb => CtlShow::Rgb(v.rgb()),
        Kind::Chips { labels } => CtlShow::Chips(
            labels.iter().enumerate().map(|(i, l)| (l.to_string(), v.bits() & (1 << i) != 0)).collect(),
        ),
        Kind::Looks => {
            let shown = if ctx.shown_theme.is_empty() { cfg.theme.as_str() } else { ctx.shown_theme };
            let lit = LOOKS.iter().position(|l| look_matches(cfg, l, shown));
            CtlShow::ChipFlow {
                chips: LOOKS.iter().enumerate().map(|(i, l)| (l.name.to_string(), Some(i) == lit)).collect(),
                status: lit.map_or_else(|| "Custom".to_string(), |i| LOOKS[i].name.to_string()),
            }
        }
        Kind::Presets { names } => {
            let names = names();
            let active = usize::try_from(v.u()).ok().filter(|&i| i < names.len());
            CtlShow::ChipFlow {
                chips: names.iter().enumerate().map(|(i, n)| (n.to_string(), Some(i) == active)).collect(),
                status: active.map_or_else(|| "Custom".to_string(), |i| names[i].to_string()),
            }
        }
        Kind::List { src, rows } => {
            let (items, offset, shown) = list_src(*src, ctx);
            let offset = offset.min(items.len().saturating_sub(*rows));
            let end = (offset + rows).min(items.len());
            CtlShow::List {
                items: items[offset..end].to_vec(),
                offset,
                total: items.len(),
                selected: list_selected(*src, items, v.s(), shown),
                rows: *rows,
            }
        }
    };
    let mut state = d.state.map_or(RowState::Normal, |f| f(cfg, ctx));
    if master_off && state == RowState::Normal {
        state = RowState::Dimmed;
    }
    PanelItem::Row(CtlRow { id: d.id, label: d.label.to_string(), show, state, hint: d.hint.map(str::to_string) })
}

/// The content of settings tab `tab`: its sections (hook sections with no
/// control yet are skipped) and, unless collapsed, their rows.
pub fn tab_items(tab: usize, cfg: &Config, ctx: &Ctx) -> Vec<PanelItem> {
    let mut out = Vec::new();
    for s in SECTIONS.iter().filter(|s| s.tab == tab) {
        let rows: Vec<&Desc> = DESCS
            .iter()
            .filter(|d| d.section == s.id && Some(d.id) != s.master && d.visible.is_none_or(|v| v(cfg)))
            .collect();
        let master = s.master.and_then(find).map(|d| (d.id, (d.get)(cfg).b()));
        if rows.is_empty() && master.is_none() {
            continue;
        }
        let collapsed = ctx.collapsed.contains(&s.id);
        out.push(PanelItem::Section {
            id: s.id,
            title: s.title.to_string(),
            master,
            collapsed,
            hint: s.hint.map(str::to_string),
        });
        if collapsed {
            continue;
        }
        let master_off = master.is_some_and(|(_, on)| !on);
        out.extend(rows.into_iter().map(|d| row_for(d, cfg, ctx, master_off)));
    }
    out
}

/// What a press on a control part does.
#[derive(Clone, Debug, PartialEq)]
pub enum Press {
    /// Set the control to this value.
    Set(Val),
    /// Start dragging (sliders, RGB channels): the value follows the mouse.
    Drag,
    /// Scroll a list by this many rows.
    Scroll(i32),
    /// Apply look `LOOKS[i]`.
    Look(usize),
    /// Nothing (a part the control does not have).
    Nothing,
}

/// The effect of pressing `part` of control `d` (pure).
pub fn press(d: &Desc, part: CtlPart, cfg: &Config, ctx: &Ctx) -> Press {
    let v = (d.get)(cfg);
    match (&d.kind, part) {
        (Kind::Slider { .. }, CtlPart::Track) | (Kind::Rgb, CtlPart::Channel(_)) => Press::Drag,
        (Kind::Toggle, CtlPart::Switch) => Press::Set(Val::B(!v.b())),
        (Kind::Choice { options }, CtlPart::Prev | CtlPart::Next) => {
            let opts = options(ctx);
            if opts.is_empty() {
                return Press::Nothing;
            }
            let n = opts.len();
            let fwd = part == CtlPart::Next;
            let j = match opts.iter().position(|o| o.0 == v.s()) {
                Some(i) if fwd => (i + 1) % n,
                Some(i) => (i + n - 1) % n,
                None if fwd => 0,
                None => n - 1,
            };
            Press::Set(Val::S(opts[j].0.clone()))
        }
        (Kind::Steps { steps, .. }, CtlPart::Prev | CtlPart::Next) => {
            Press::Set(Val::U(cycle_steps(steps, v.u(), part == CtlPart::Next)))
        }
        (Kind::Stepper { min, max, step, .. }, CtlPart::Minus | CtlPart::Plus) => {
            let d = if part == CtlPart::Plus { *step } else { -*step };
            Press::Set(Val::F((v.f() + d).clamp(*min, *max)))
        }
        (Kind::Stepper { .. }, CtlPart::Reset) => Press::Set(default_of(d)),
        (Kind::Chips { labels }, CtlPart::Chip(i)) if (i as usize) < labels.len() => {
            Press::Set(Val::Bits(v.bits() ^ (1 << i)))
        }
        (Kind::Presets { names }, CtlPart::Chip(i)) if (i as usize) < names().len() => Press::Set(Val::U(i as u64)),
        (Kind::Looks, CtlPart::Chip(i)) if (i as usize) < LOOKS.len() => Press::Look(i as usize),
        (Kind::List { src, .. }, CtlPart::Row(i)) => {
            let (items, _, _) = list_src(*src, ctx);
            list_value(*src, items, i).map_or(Press::Nothing, |s| Press::Set(Val::S(s)))
        }
        (Kind::List { .. }, CtlPart::ScrollUp) => Press::Scroll(-1),
        (Kind::List { .. }, CtlPart::ScrollDown) => Press::Scroll(1),
        _ => Press::Nothing,
    }
}

/// The value a drag of `part` of `d` sets at `frac` (0..=1 along its track).
pub fn drag_value(d: &Desc, part: CtlPart, frac: f32, cur: &Val) -> Option<Val> {
    let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
    match (&d.kind, part) {
        (Kind::Slider { min, max, step, .. }, CtlPart::Track) => {
            let v = min + frac * (max - min);
            let v = if *step > 0.0 { min + ((v - min) / step).round() * step } else { v };
            Some(Val::F(v.clamp(*min, *max)))
        }
        (Kind::Rgb, CtlPart::Channel(i)) if i < 3 => {
            let mut c = cur.rgb();
            c[i as usize] = frac;
            Some(Val::Rgb(c))
        }
        _ => None,
    }
}

/// The 0..=1 position of `x` along a track hit rect: the knob's CENTER
/// travels between `knob / 2` from either end, so a press lands the knob
/// under the pointer.
pub fn track_frac(x: f32, track_x: f32, track_w: f32, knob: f32) -> f32 {
    let travel = (track_w - knob).max(1.0);
    ((x - track_x - knob / 2.0) / travel).clamp(0.0, 1.0)
}

/// Whether a drag of `d` applies on every move (else once, on release).
pub fn live(d: &Desc) -> bool {
    !matches!(d.kind, Kind::Slider { live: false, .. })
}

/// One palette deep link into Settings.
pub struct DeepLink {
    /// "Settings › Effects › Bloom" — found by the control's name (the
    /// palette ranks a title match above a keyword-only one).
    pub title: String,
    /// The control it opens.
    pub id: CtlId,
}

/// Palette deep links, one per setting.
pub fn deep_links() -> Vec<DeepLink> {
    DESCS
        .iter()
        .filter(|d| d.is_setting())
        .map(|d| DeepLink { title: format!("Settings › {} › {}", TAB_NAMES[d.tab], d.label), id: d.id })
        .collect()
}

/// Theme-gallery keyboard moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GalleryKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// The theme the gallery moves to from `current` on `key`, within `order`
/// (the filtered cards, `cols` per row); `None` when it cannot move. From a
/// theme the filter hides, any key lands on the first (or last) card.
pub fn gallery_step(order: &[usize], cols: usize, current: usize, key: GalleryKey) -> Option<usize> {
    let n = order.len();
    if n == 0 {
        return None;
    }
    let cols = cols.max(1);
    let Some(p) = order.iter().position(|&i| i == current) else {
        return Some(match key {
            GalleryKey::Left | GalleryKey::Up | GalleryKey::End => order[n - 1],
            _ => order[0],
        });
    };
    let q = match key {
        GalleryKey::Left => p.checked_sub(1)?,
        GalleryKey::Right => (p + 1 < n).then_some(p + 1)?,
        GalleryKey::Up => p.checked_sub(cols)?,
        // Down from above a short last row lands on its last card.
        GalleryKey::Down => (p / cols < (n - 1) / cols).then(|| (p + cols).min(n - 1))?,
        GalleryKey::Home => 0,
        GalleryKey::End => n - 1,
    };
    (q != p).then_some(order[q])
}

/// A gallery browsing session: clicks and arrow keys apply each theme live
/// (and save it), Enter keeps the one shown, Esc restores the theme shown
/// when the session began.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GallerySession {
    origin: Option<usize>,
}

impl GallerySession {
    pub fn active(&self) -> bool {
        self.origin.is_some()
    }

    /// About to switch away from `current`: the session begins there (a
    /// running session keeps its first origin).
    pub fn begin(&mut self, current: usize) {
        self.origin.get_or_insert(current);
    }

    /// Enter: keep what is shown. Whether a session was running.
    pub fn keep(&mut self) -> bool {
        self.origin.take().is_some()
    }

    /// Esc: the theme to restore, ending the session (`None` = no session).
    pub fn restore(&mut self) -> Option<usize> {
        self.origin.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetty_render::{build_panel, ChromeMetrics, MonoMeasure, PanelHit, PanelInput, CHAR_W_FALLBACK, N_TABS};

    fn fonts() -> (Vec<String>, Vec<String>) {
        let mono = ["JetBrains Mono", "Fira Code", "Hack", "Source Code Pro", "Inconsolata", "MesloLGS NF", "Cascadia Code"]
            .map(String::from)
            .to_vec();
        let ui = ["System Sans (default)", "Inter", "Noto Sans", "DejaVu Sans", "Cantarell", "Carlito"]
            .map(String::from)
            .to_vec();
        (mono, ui)
    }

    fn ctx<'a>(mono: &'a [String], ui: &'a [String]) -> Ctx<'a> {
        Ctx { mono_families: mono, ui_families: ui, ..Ctx::empty() }
    }

    #[test]
    fn ids_unique_sections_known_and_masters_are_toggles_of_their_section() {
        let mut ids: Vec<&str> = DESCS.iter().map(|d| d.id).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate control id");
        let mut sids: Vec<&str> = SECTIONS.iter().map(|s| s.id).collect();
        sids.sort();
        sids.dedup();
        assert_eq!(sids.len(), SECTIONS.len(), "duplicate section id");
        for d in DESCS {
            let s = section(d.section).unwrap_or_else(|| panic!("{}: unknown section {}", d.id, d.section));
            assert_eq!(s.tab, d.tab, "{}: section {} is on another tab", d.id, s.id);
            assert!(d.tab < N_TABS);
        }
        for s in SECTIONS {
            if let Some(m) = s.master {
                let d = find(m).unwrap_or_else(|| panic!("{}: unknown master {m}", s.id));
                assert!(matches!(d.kind, Kind::Toggle), "{m} must be a toggle");
                assert_eq!(d.section, s.id, "{m} must live in {}", s.id);
            }
        }
    }

    #[test]
    fn get_set_round_trips_and_changes_every_setting() {
        let (mono, ui) = fonts();
        let x = ctx(&mono, &ui);
        for d in DESCS.iter().filter(|d| d.is_setting()) {
            let mut c = Config::default();
            let v = (d.get)(&c);
            (d.set)(&mut c, v.clone());
            assert_eq!((d.get)(&c), v, "{}: set(get) is not identity", d.id);
            // A different value goes in and comes back out.
            let other = match &d.kind {
                Kind::Slider { min, max, .. } => Val::F(if v.f() == *max { *min } else { *max }),
                Kind::Toggle => Val::B(!v.b()),
                Kind::Choice { options } => {
                    let o = options(&x);
                    Val::S(o.iter().map(|o| o.0.clone()).find(|s| s != v.s()).unwrap_or_else(|| "custom".into()))
                }
                Kind::Steps { steps, .. } => Val::U(cycle_steps(steps, v.u(), true)),
                Kind::Stepper { min, .. } => Val::F(*min),
                // 8-bit exact: hex-backed colors (the backdrop stops) round-trip.
                Kind::Rgb => Val::Rgb([64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0]),
                Kind::Chips { .. } => Val::Bits(v.bits() ^ 1),
                Kind::Presets { .. } => Val::U(1),
                Kind::Looks => continue,
                Kind::List { .. } | Kind::Gallery => Val::S("Something Else".into()),
                Kind::Specimen => unreachable!(),
            };
            (d.set)(&mut c, other.clone());
            assert_eq!((d.get)(&c), other, "{}: the new value did not stick", d.id);
            assert_ne!(c, Config::default(), "{}: set changed nothing", d.id);
        }
    }

    #[test]
    fn reset_tab_restores_that_tab_only() {
        let mut changed = Config {
            opacity: 0.5,
            corner_radius: 3.0,
            font_size: 20.0,
            window_mode: "dropdown".into(),
            ..Config::default()
        };
        changed.effects.crt_enabled = true;
        changed.effects.crt_bloom = 0.9;
        changed.effects.crt_flicker = true;
        changed.theme = "dracula".into();
        changed.launch_at_login = true;
        let r = reset_tab(&changed, EFFECTS);
        assert_eq!(r.effects, Config::default().effects, "every effect back at its default");
        assert_eq!((r.opacity, r.font_size, r.window_mode.as_str()), (0.5, 20.0, "dropdown"), "other tabs untouched");
        let r = reset_tab(&changed, LOOK);
        assert_eq!((r.opacity, r.corner_radius), (1.0, Config::default().corner_radius));
        assert_eq!(r.theme, "dracula", "the theme is not part of Reset tab");
        let r = reset_tab(&changed, SHELL);
        assert!(r.launch_at_login, "launch at login (a system side effect) is not reset");
        assert!(tab_at_defaults(&Config::default(), WINDOW));
        assert!(!tab_at_defaults(&changed, WINDOW));
        for tab in 0..N_TABS {
            assert!(
                DESCS.iter().any(|d| d.tab == tab && d.reset && d.is_setting()),
                "tab {tab} has nothing to reset"
            );
        }
    }

    #[test]
    fn press_semantics_per_kind() {
        let (mono, ui) = fonts();
        let x = ctx(&mono, &ui);
        let c = Config::default();
        let p = |id: &str, part| press(find(id).unwrap(), part, &c, &x);
        assert_eq!(p("focus_autohide", CtlPart::Switch), Press::Set(Val::B(!c.focus_autohide)));
        assert_eq!(p("opacity", CtlPart::Track), Press::Drag);
        assert_eq!(p("effects.crt_scanline_tint", CtlPart::Channel(1)), Press::Drag);
        // Choice wraps both ways.
        assert_eq!(p("window_mode", CtlPart::Next), Press::Set(Val::S("dropdown".into())));
        assert_eq!(p("window_mode", CtlPart::Prev), Press::Set(Val::S("fullscreen".into())));
        // An unknown (hand-edited) value starts over.
        let mut odd = c.clone();
        odd.tab_bar_position = "left".into();
        assert_eq!(press(find("tab_bar_position").unwrap(), CtlPart::Next, &odd, &x), Press::Set(Val::S("top".into())));
        // Steps snap then move.
        assert_eq!(p("scrollback_lines", CtlPart::Next), Press::Set(Val::U(25_000)));
        // Stepper clamps and resets to the default.
        let mut big = c.clone();
        big.font_size = 48.0;
        assert_eq!(press(find("font_size").unwrap(), CtlPart::Plus, &big, &x), Press::Set(Val::F(48.0)));
        assert_eq!(press(find("font_size").unwrap(), CtlPart::Reset, &big, &x), Press::Set(Val::F(16.0)));
        assert_eq!(p("font_size", CtlPart::Minus), Press::Set(Val::F(15.0)));
        // Chips flip one bit.
        assert_eq!(p("effects.crt_animate", CtlPart::Chip(1)), Press::Set(Val::Bits(0b010)));
        assert_eq!(p("effects.crt_animate", CtlPart::Chip(7)), Press::Nothing);
        // Lists pick a value; the UI list's row 0 is the system font ("").
        assert_eq!(p("font_family", CtlPart::Row(1)), Press::Set(Val::S("Fira Code".into())));
        assert_eq!(p("ui_font_family", CtlPart::Row(0)), Press::Set(Val::S(String::new())));
        assert_eq!(p("ui_font_family", CtlPart::Row(2)), Press::Set(Val::S("Noto Sans".into())));
        assert_eq!(p("font_family", CtlPart::Row(99)), Press::Nothing);
        assert_eq!(p("font_family", CtlPart::ScrollDown), Press::Scroll(1));
        // A part the control lacks does nothing.
        assert_eq!(p("opacity", CtlPart::Plus), Press::Nothing);
    }

    #[test]
    fn steps_snap_and_wrap() {
        assert_eq!(cycle_steps(&SCROLLBACK_STEPS, 10_000, true), 25_000);
        assert_eq!(cycle_steps(&SCROLLBACK_STEPS, 100_000, true), 1_000, "forward wraps");
        assert_eq!(cycle_steps(&SCROLLBACK_STEPS, 1_000, false), 100_000, "backward wraps");
        assert_eq!(cycle_steps(&SCROLLBACK_STEPS, 12_345, true), 25_000);
        assert_eq!(cycle_steps(&SCROLLBACK_STEPS, 12_345, false), 5_000);
        assert_eq!(cycle_steps(&NOTIFY_MIN_STEPS, 10, true), 30);
        assert_eq!(cycle_steps(&NOTIFY_MIN_STEPS, 300, true), 5);
        assert_eq!(cycle_steps(&NOTIFY_MIN_STEPS, 50, true), 120);
        assert_eq!(cycle_steps(&NOTIFY_MIN_STEPS, 50, false), 30);
        assert_eq!(cycle_steps(&[], 7, true), 7);
    }

    #[test]
    fn readouts() {
        for s in SCROLLBACK_STEPS {
            assert_eq!(fmt_scrollback(s), format!("{}k", s / 1000));
        }
        assert_eq!(fmt_scrollback(12_345), "12345");
        assert_eq!((fmt_secs(10), fmt_secs(120), fmt_secs(90)), ("10s".into(), "2m".into(), "90s".into()));
        assert_eq!((fmt_pct(0.974), fmt_px(18.97), fmt_pt(13.0), fmt_ms(130.0)), ("97%".into(), "19px".into(), "13pt".into(), "130ms".into()));
        assert_eq!(shell_label("/usr/bin/zsh"), "zsh");
        assert_eq!(shell_label(""), "System default");
    }

    #[test]
    fn drag_values_and_track_mapping() {
        let op = find("opacity").unwrap();
        assert_eq!(drag_value(op, CtlPart::Track, 0.0, &Val::F(0.5)), Some(Val::F(0.1)));
        assert_eq!(drag_value(op, CtlPart::Track, 1.0, &Val::F(0.5)), Some(Val::F(1.0)));
        assert_eq!(drag_value(op, CtlPart::Track, f32::NAN, &Val::F(0.5)), Some(Val::F(0.1)));
        let tint = find("effects.crt_scanline_tint").unwrap();
        assert_eq!(drag_value(tint, CtlPart::Channel(2), 0.25, &Val::Rgb([1.0; 3])), Some(Val::Rgb([1.0, 1.0, 0.25])));
        assert_eq!(drag_value(tint, CtlPart::Track, 0.25, &Val::Rgb([1.0; 3])), None);
        // The knob centre follows the pointer: the ends of the travel are
        // half a knob inside the track.
        assert_eq!(track_frac(108.0, 100.0, 216.0, 16.0), 0.0);
        assert_eq!(track_frac(308.0, 100.0, 216.0, 16.0), 1.0);
        assert_eq!(track_frac(208.0, 100.0, 216.0, 16.0), 0.5);
        assert!(live(op));
        assert!(!live(find("dropdown_height_pct").unwrap()), "dropdown size applies on release");
    }

    #[test]
    fn tab_items_sections_masters_and_states() {
        let (mono, ui) = fonts();
        let x = ctx(&mono, &ui);
        let c = Config::default();
        for tab in 0..N_TABS {
            let items = tab_items(tab, &c, &x);
            assert!(matches!(items.first(), Some(PanelItem::Section { .. })), "tab {tab} starts with a section");
            // Hook sections without controls never show.
            for it in &items {
                if let PanelItem::Section { id, .. } = it {
                    assert!(DESCS.iter().any(|d| d.section == *id), "empty hook section {id} shown");
                }
            }
        }
        // CRT off (the default) → the CRT rows are dimmed under their master.
        let fx = tab_items(EFFECTS, &c, &x);
        let state = |items: &[PanelItem], id: &str| {
            items.iter().find_map(|it| match it {
                PanelItem::Row(r) if r.id == id => Some(r.state),
                _ => None,
            })
        };
        assert_eq!(state(&fx, "effects.crt_bloom"), Some(RowState::Dimmed));
        assert_eq!(state(&fx, "effects.caret_flash_ms"), Some(RowState::Normal));
        assert!(fx.iter().any(|it| matches!(it, PanelItem::Section { id: "fx.crt", master: Some(("effects.crt_enabled", false)), .. })));
        assert!(state(&fx, "effects.crt_enabled").is_none(), "the master is in the header, not a row");
        let mut on = c.clone();
        on.effects.crt_enabled = true;
        assert_eq!(state(&tab_items(EFFECTS, &on, &x), "effects.crt_bloom"), Some(RowState::Normal));
        // Collapsed: header only.
        let collapsed = ["fx.crt"];
        let xc = Ctx { collapsed: &collapsed, ..ctx(&mono, &ui) };
        let fx = tab_items(EFFECTS, &c, &xc);
        assert!(state(&fx, "effects.crt_bloom").is_none());
        assert!(fx.iter().any(|it| matches!(it, PanelItem::Section { id: "fx.crt", collapsed: true, .. })));
        // Dropdown sliders are inert outside Dropdown mode; the radius dims in fullscreen.
        let w = tab_items(WINDOW, &c, &x);
        assert_eq!(state(&w, "dropdown_height_pct"), Some(RowState::Disabled));
        let mut dd = c.clone();
        dd.window_mode = "dropdown".into();
        assert_eq!(state(&tab_items(WINDOW, &dd, &x), "dropdown_width_pct"), Some(RowState::Normal));
        let xf = Ctx { main_fullscreen: true, ..ctx(&mono, &ui) };
        assert_eq!(state(&tab_items(WINDOW, &dd, &xf), "dropdown_width_pct"), Some(RowState::Disabled));
        assert_eq!(state(&tab_items(LOOK, &c, &xf), "corner_radius"), Some(RowState::Dimmed));
        // A pending (release-applied) drag shows in place of the config value.
        let pending = Val::F(0.75);
        let xd = Ctx { drag: Some(("dropdown_height_pct", &pending)), ..ctx(&mono, &ui) };
        let row = tab_items(WINDOW, &dd, &xd).into_iter().find_map(|it| match it {
            PanelItem::Row(r) if r.id == "dropdown_height_pct" => Some(r.show),
            _ => None,
        });
        assert_eq!(row, Some(CtlShow::Slider { frac: (0.75 - 0.25) / 0.75, text: "75%".into() }));
    }

    /// Configs that, between them, show every row a mode or a state hides:
    /// each backdrop mode (gradient with custom stops), the custom phosphor.
    fn revealing_configs() -> Vec<Config> {
        let mut v: Vec<Config> = ["none", "theme", "image", "pattern"].iter().map(|m| with_backdrop(m, &[])).collect();
        v.push(with_backdrop("gradient", &["#102030", "#405060"]));
        let mut fx = Config::default();
        fx.effects.crt_phosphor = crate::config::PhosphorMode::Custom;
        fx.effects.crt_grain = 0.5;
        v.push(fx);
        v
    }

    fn with_backdrop(mode: &str, colors: &[&str]) -> Config {
        Config {
            backdrop: crate::config::BackdropConfig {
                mode: mode.into(),
                colors: colors.iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn backdrop_rows(c: &Config) -> Vec<&'static str> {
        tab_items(LOOK, c, &Ctx::empty())
            .into_iter()
            .filter_map(|it| match it {
                PanelItem::Row(r) if r.id.starts_with("backdrop.") => Some(r.id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn backdrop_rows_follow_the_mode() {
        assert_eq!(backdrop_rows(&with_backdrop("none", &[])), ["backdrop.mode"], "off: just the mode");
        let img = backdrop_rows(&with_backdrop("image", &[]));
        for id in ["backdrop.image", "backdrop.fit", "backdrop.dim", "backdrop.blur", "backdrop.strength", "backdrop.parallax"] {
            assert!(img.contains(&id), "image mode lacks {id}");
        }
        for id in ["backdrop.angle", "backdrop.pattern", "backdrop.animate", "backdrop.custom_colors"] {
            assert!(!img.contains(&id), "image mode shows {id}");
        }
        let grad = backdrop_rows(&with_backdrop("gradient", &[]));
        assert!(grad.contains(&"backdrop.angle") && grad.contains(&"backdrop.custom_colors") && grad.contains(&"backdrop.animate"));
        assert!(!grad.contains(&"backdrop.color_start"), "theme colors: no stop rows");
        let custom = backdrop_rows(&with_backdrop("gradient", &["#102030", "#405060"]));
        assert!(custom.contains(&"backdrop.color_start") && custom.contains(&"backdrop.color_end"));
        let pat = backdrop_rows(&with_backdrop("pattern", &[]));
        assert!(pat.contains(&"backdrop.pattern") && pat.contains(&"backdrop.animate") && !pat.contains(&"backdrop.image"));
    }

    #[test]
    fn backdrop_custom_colors_seed_from_the_theme_and_reset_clears_them() {
        let mut c = with_backdrop("gradient", &[]);
        let toggle = find("backdrop.custom_colors").unwrap();
        (toggle.set)(&mut c, Val::B(true));
        let t = jetty_core::Theme::by_name(&c.theme);
        let hex = |rgb: [u8; 3]| rgb_hex(rgb.map(|v| v as f32 / 255.0));
        assert_eq!(c.backdrop.colors, vec![hex(t.palette[4]), hex(t.palette[5])]);
        // Editing one stop keeps the other.
        (find("backdrop.color_end").unwrap().set)(&mut c, Val::Rgb([1.0, 0.0, 0.0]));
        assert_eq!(c.backdrop.colors[1], "#ff0000");
        assert_eq!(c.backdrop.colors[0], hex(t.palette[4]));
        // Reset tab: back to theme colors (the stop rows are not reset alone).
        assert!(reset_tab(&c, LOOK).backdrop.colors.is_empty());
        (toggle.set)(&mut c, Val::B(false));
        assert!(c.backdrop.colors.is_empty());
    }

    #[test]
    fn hex_colors_round_trip() {
        assert_eq!(hex_rgb("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(rgb_hex([1.0, 128.0 / 255.0, 0.0]), "#ff8000");
        assert_eq!(hex_rgb(" #FF8000 "), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(hex_rgb("ff8000"), None);
        assert_eq!(hex_rgb("#ff80"), None);
        assert_eq!(hex_rgb("#gg0000"), None);
    }

    #[test]
    fn every_look_writes_valid_keys() {
        let opts = |id: &str| -> Vec<String> {
            match &find(id).unwrap().kind {
                Kind::Choice { options } => options(&Ctx::empty()).into_iter().map(|o| o.0).collect(),
                _ => unreachable!(),
            }
        };
        assert!(!LOOKS.is_empty());
        let mut names: Vec<&str> = LOOKS.iter().map(|l| l.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), LOOKS.len(), "duplicate look name");
        for l in LOOKS {
            if let Some(t) = l.theme {
                assert!(jetty_core::theme_index(t).is_some(), "{}: no theme {t}", l.name);
            }
            assert!(crate::effects::find_preset(l.preset).is_some(), "{}: no preset {}", l.name, l.preset);
            assert!(opts("backdrop.mode").iter().any(|m| m == l.backdrop), "{}: backdrop {}", l.name, l.backdrop);
            if let Some(p) = l.pattern {
                assert!(opts("backdrop.pattern").iter().any(|m| m == p), "{}: pattern {p}", l.name);
            }
            assert!(l.strength.is_none_or(|v| (0.0..=1.0).contains(&v)), "{}: strength", l.name);
            assert!(opts("summon_effect").iter().any(|m| m == l.summon), "{}: summon {}", l.name, l.summon);
            if let Some(sh) = l.cursor_shape {
                assert_eq!(crate::motion::CursorShapePref::parse(sh).as_str(), sh, "{}: shape", l.name);
            }
            if let Some(g) = l.guide {
                assert_eq!(crate::motion::GuideMode::parse(g).as_str(), g, "{}: guide", l.name);
            }
        }
    }

    #[test]
    fn applying_a_look_lights_exactly_its_chip() {
        for (i, look) in LOOKS.iter().enumerate() {
            let mut c = Config::default();
            apply_look_keys(&mut c, look);
            // The app picks the theme (the slot on screen); here: the dark one.
            if let Some(t) = look.theme {
                c.theme = t.to_string();
            }
            let lit: Vec<usize> = (0..LOOKS.len()).filter(|&j| look_matches(&c, &LOOKS[j], &c.theme)).collect();
            assert_eq!(lit, vec![i], "{} lights {lit:?}", look.name);
            // The Looks row shows it: one lit chip, named in the status.
            let row = tab_items(LOOK, &c, &Ctx::empty()).into_iter().find_map(|it| match it {
                PanelItem::Row(CtlRow { id: "look", show: CtlShow::ChipFlow { chips, status }, .. }) => Some((chips, status)),
                _ => None,
            });
            let (chips, status) = row.expect("the Looks row");
            assert_eq!(chips.iter().filter(|c| c.1).count(), 1);
            assert!(chips[i].1, "{}'s chip lit", look.name);
            assert_eq!(status, look.name);
        }
        // The defaults ARE the Clean look; one key off any look reads "Custom".
        let custom = Config { summon_effect: "bayer".into(), ..Config::default() };
        let row = tab_items(LOOK, &custom, &Ctx::empty()).into_iter().find_map(|it| match it {
            PanelItem::Row(CtlRow { id: "look", show: CtlShow::ChipFlow { chips, status }, .. }) => Some((chips, status)),
            _ => None,
        });
        let (chips, status) = row.unwrap();
        assert!(chips.iter().all(|c| !c.1));
        assert_eq!(status, "Custom");
    }

    #[test]
    fn a_look_matches_the_theme_on_screen_and_clean_keeps_the_theme() {
        let paper = LOOKS.iter().find(|l| l.name == "Paper").unwrap();
        let mut c = Config { theme: "dracula".into(), follow_system_theme: true, ..Config::default() };
        apply_look_keys(&mut c, paper);
        // The light slot is showing Flexoki Light: Paper is lit…
        assert!(look_matches(&c, paper, "flexoki_light"));
        // …and not while Dracula shows.
        assert!(!look_matches(&c, paper, "dracula"));
        let clean = LOOKS.iter().find(|l| l.name == "Clean").unwrap();
        assert!(clean.theme.is_none(), "Clean leaves the theme alone");
        let mut c = Config { theme: "dracula".into(), ..Config::default() };
        apply_look_keys(&mut c, clean);
        assert_eq!(c.theme, "dracula");
        assert!(look_matches(&c, clean, "dracula"));
        assert!(!c.effects.crt_enabled);
        assert_eq!((c.backdrop.mode.as_str(), c.cursor.trail, c.cursor.guide.as_str()), ("none", false, "off"));
    }

    #[test]
    fn summon_cycler_lists_the_new_effects_and_cursor_rows_dim_sensibly() {
        let summon = match &find("summon_effect").unwrap().kind {
            Kind::Choice { options } => options(&Ctx::empty()),
            _ => unreachable!(),
        };
        for (v, label) in [("pop", "Pop"), ("glide", "Glide"), ("fade", "Fade")] {
            assert!(summon.iter().any(|o| o.0 == v && o.1 == label), "summon effect {v} missing");
        }
        let state = |c: &Config, id: &str| {
            tab_items(EFFECTS, c, &Ctx::empty()).into_iter().find_map(|it| match it {
                PanelItem::Row(r) if r.id == id => Some(r.state),
                _ => None,
            })
        };
        let c = Config::default(); // block cursor, no trail
        assert_eq!(state(&c, "cursor.thickness"), Some(RowState::Dimmed));
        assert_eq!(state(&c, "cursor.trail_ms"), Some(RowState::Dimmed));
        let mut beam = Config::default();
        beam.cursor.shape = "beam".into();
        beam.cursor.trail = true;
        assert_eq!(state(&beam, "cursor.thickness"), Some(RowState::Normal));
        assert_eq!(state(&beam, "cursor.trail_threshold"), Some(RowState::Normal));
        // The trail rows write whole numbers into the u32 keys.
        let mut c = Config::default();
        (find("cursor.trail_ms").unwrap().set)(&mut c, Val::F(254.6));
        (find("cursor.trail_threshold").unwrap().set)(&mut c, Val::F(3.4));
        assert_eq!((c.cursor.trail_ms, c.cursor.trail_threshold), (255, 3));
    }

    #[test]
    fn the_theme_gallery_closes_the_look_tab() {
        // Every theme is a card: anything placed after the gallery would sit
        // a long scroll down.
        let items = tab_items(LOOK, &Config::default(), &Ctx::empty());
        assert_eq!(items.last(), Some(&PanelItem::Gallery));
    }

    #[test]
    fn lists_window_and_select() {
        let (mono, ui) = fonts();
        let mut c = Config { font_family: "Hack".into(), ..Config::default() };
        let x = Ctx { font_offset: 1, ..ctx(&mono, &ui) };
        let items = tab_items(FONTS, &c, &x);
        let list = items.iter().find_map(|it| match it {
            PanelItem::Row(CtlRow { id: "font_family", show: CtlShow::List { items, offset, total, selected, rows }, .. }) => {
                Some((items.clone(), *offset, *total, *selected, *rows))
            }
            _ => None,
        });
        let (vis, offset, total, selected, rows) = list.unwrap();
        assert_eq!((offset, total, rows), (1, 7, 5));
        assert_eq!(vis.first().map(String::as_str), Some("Fira Code"));
        assert_eq!(selected, Some(2), "Hack");
        // A chosen family that is missing highlights the one shown instead.
        c.font_family = "Not Installed".into();
        let xs = Ctx { font_shown: "Inconsolata", ..ctx(&mono, &ui) };
        let sel = tab_items(FONTS, &c, &xs).into_iter().find_map(|it| match it {
            PanelItem::Row(CtlRow { id: "font_family", show: CtlShow::List { selected, .. }, .. }) => Some(selected),
            _ => None,
        });
        assert_eq!(sel, Some(Some(4)));
    }

    /// Every tab, laid out by the real panel builder at 1× and 2× and at both
    /// ends of the capped panel text range, with the widest realistic chrome
    /// font (the monospace default): no label crosses the content column, and
    /// every row label ends before its control.
    #[test]
    fn every_tab_fits_at_1x_2x_and_ui_font_13_and_17() {
        let (mono, ui) = fonts();
        let x = ctx(&mono, &ui);
        let theme = jetty_core::Theme::by_name("catppuccin_latte");
        for base in revealing_configs() {
            let c = Config {
                shell: "/usr/local/bin/a-shell-with-a-long-name".into(),
                window_mode: "dropdown".into(),
                ..base
            };
            for (scale, font) in [(1.0, 16.0), (2.0, 16.0), (1.0, 13.0), (1.0, 17.0), (2.0, 13.0)] {
                let cm = ChromeMetrics::new(scale, font);
                let u = cm.overlay_u();
                let (w, h) = ((jetty_render::PANEL_W * u) as u32, (jetty_render::PANEL_H * u) as u32);
                for tab in 0..N_TABS {
                    let items = tab_items(tab, &c, &x);
                    let mut inp = PanelInput::new(w, h, &theme, cm, &items);
                    inp.active_tab = tab;
                    // Scroll through the whole tab so every row is checked in view.
                    let mut scroll = 0.0;
                    loop {
                        inp.scroll = scroll;
                        let v = build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK * u));
                        let g = &v.geom;
                        let right = g.panel.x + g.panel.w - 20.0 * u;
                        for (t, lx, _, _) in v.labels.iter().chain(&v.content_labels) {
                            let end = lx + t.chars().count() as f32 * CHAR_W_FALLBACK * u;
                            assert!(end <= right + 0.5, "{scale}×/{font}pt tab {tab}: {t:?} ends {end:.1} > {right:.1}");
                        }
                        for (r, hit) in &g.hits {
                            if let PanelHit::Ctl { .. } = hit {
                                assert!(r.x + r.w <= right + 0.5, "{scale}×/{font}pt tab {tab}: {hit:?} past the column");
                            }
                        }
                        if g.scroll >= g.max_scroll {
                            break;
                        }
                        scroll = (g.scroll + g.viewport_h()).min(g.max_scroll);
                    }
                }
            }
        }
    }

    /// Every control label and section title renders WHOLE (never ellipsized)
    /// with the widest realistic chrome font — the monospace default — so a
    /// new control's label must fit its row, not just get cut to fit.
    #[test]
    fn every_label_renders_untruncated_at_the_default_font() {
        let (mono, ui) = fonts();
        let x = ctx(&mono, &ui);
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        for c in revealing_configs() {
            for tab in 0..N_TABS {
                let items = tab_items(tab, &c, &x);
                let mut inp = PanelInput::new(420, 592, &theme, ChromeMetrics::DEFAULT, &items);
                inp.active_tab = tab;
                inp.scroll = 1.0e9;
                let max = build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK)).geom.max_scroll;
                let mut drawn: Vec<String> = Vec::new();
                let mut scroll = 0.0;
                loop {
                    inp.scroll = scroll;
                    let v = build_panel(&inp, &mut MonoMeasure(CHAR_W_FALLBACK));
                    drawn.extend(v.content_labels.into_iter().map(|l| l.0));
                    if scroll >= max {
                        break;
                    }
                    scroll = (scroll + v.geom.viewport_h()).min(max);
                }
                for it in &items {
                    let want: Vec<String> = match it {
                        PanelItem::Section { title, hint, .. } => std::iter::once(title.clone()).chain(hint.clone()).collect(),
                        PanelItem::Row(r) => std::iter::once(r.label.clone()).chain(r.hint.clone()).collect(),
                        _ => continue,
                    };
                    for w in want {
                        assert!(drawn.contains(&w), "tab {tab}: {w:?} is truncated or missing");
                    }
                }
            }
        }
    }

    #[test]
    fn deep_links_cover_every_setting_once() {
        let links = deep_links();
        assert_eq!(links.len(), DESCS.iter().filter(|d| d.is_setting()).count());
        let mut titles: Vec<&str> = links.iter().map(|l| l.title.as_str()).collect();
        titles.sort();
        let n = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), n, "duplicate deep-link title");
        assert!(links.iter().any(|l| l.title == "Settings › Effects › Bloom" && l.id == "effects.crt_bloom"));
        assert!(links.iter().all(|l| find(l.id).is_some_and(|d| l.title.ends_with(d.label))));
    }

    #[test]
    fn gallery_keys_move_within_the_filtered_grid() {
        let order = [0, 2, 4, 6, 8, 10, 12];
        let step = |cur, k| gallery_step(&order, 3, cur, k);
        assert_eq!(step(0, GalleryKey::Right), Some(2));
        assert_eq!(step(0, GalleryKey::Left), None, "no wrap");
        assert_eq!(step(0, GalleryKey::Down), Some(6));
        assert_eq!(step(6, GalleryKey::Up), Some(0));
        assert_eq!(step(4, GalleryKey::Down), Some(10));
        // Down into a short last row lands on its last card.
        assert_eq!(step(10, GalleryKey::Down), Some(12));
        assert_eq!(step(8, GalleryKey::Down), Some(12));
        assert_eq!(step(12, GalleryKey::Down), None);
        assert_eq!(step(6, GalleryKey::End), Some(12));
        assert_eq!(step(6, GalleryKey::Home), Some(0));
        assert_eq!(step(12, GalleryKey::End), None);
        // From a theme the filter hides: the first (or last) card.
        assert_eq!(step(5, GalleryKey::Right), Some(0));
        assert_eq!(step(5, GalleryKey::Up), Some(12));
        assert_eq!(gallery_step(&[], 3, 0, GalleryKey::Right), None);
    }

    #[test]
    fn gallery_session_keeps_or_restores_the_origin() {
        let mut s = GallerySession::default();
        assert!(!s.active());
        assert_eq!(s.restore(), None, "Esc with no session: nothing to restore (closes)");
        s.begin(3);
        s.begin(5); // a later move keeps the first origin
        assert!(s.active());
        assert_eq!(s.restore(), Some(3));
        assert!(!s.active());
        s.begin(7);
        assert!(s.keep(), "Enter ends a running session");
        assert!(!s.keep(), "…and is a no-op without one");
        assert_eq!(s.restore(), None);
    }
}
