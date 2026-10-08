//! Persisted user settings.
//!
//! Stores the UI state the user can tweak as a TOML file under the OS config dir
//! (`~/.config/jetty/config.toml` on Linux, `~/Library/Application Support/jetty/
//! config.toml` on macOS, or `$JETTY_CONFIG_DIR/config.toml`). The file is the
//! user's: JeTTY never resets or rewrites it wholesale.
//!
//! * **Loading** is per key: one wrong-typed value (`opacity = "0.9"`) falls back
//!   to its default (at startup) or keeps the live value (on hot-reload) while every
//!   other key still applies, and each problem — plus every unknown key — becomes a
//!   human-readable warning the app shows in its own UI. A file that is not valid
//!   TOML at all loads defaults in memory, is copied aside (`config.toml.bad-<ts>`,
//!   never moved — a symlinked config stays linked) and is never written until fixed.
//! * **Saving** edits the file IN PLACE (`toml_edit`): only keys whose value changed
//!   are touched, so comments, formatting, key order and unknown keys survive. Saves
//!   are debounced and written by a background thread, which re-reads the file first
//!   so a concurrent external edit is merged, never clobbered.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The persisted user settings. Field names are the TOML keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Theme preset name (must match a `jetty_core::theme::PRESETS` entry).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Background opacity in 0.0..=1.0.
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Logical font size in points.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Monospace font family name.
    #[serde(default = "default_font_family")]
    pub font_family: String,
    /// UI (chrome) font family — tab titles, status bar, menus, panel, help,
    /// dialogs, welcome. SEPARATE from the terminal `font_family`. An empty
    /// string means the platform's proportional sans (glyphon `Family::SansSerif`)
    /// — the elegant out-of-box default that cannot collide with a real installed
    /// family name and needs no special-casing in family lookup/validation.
    #[serde(default = "default_ui_font_family")]
    pub ui_font_family: String,
    /// UI (chrome) font size in logical points. SEPARATE from the terminal
    /// `font_size`. Clamped on load to [10.0, 28.0]; default 16.0 (== today's
    /// chrome size, so the default look is unchanged).
    #[serde(default = "default_ui_font_size")]
    pub ui_font_size: f32,
    /// Window corner radius in logical px (0..=24).
    #[serde(default = "default_corner_radius")]
    pub corner_radius: f32,
    /// Window-summon reveal effect: "none", "bayer", "phosphor", "liquid", or
    /// "focus" (the last two are Tier-B effects that sample the rendered frame).
    #[serde(default = "default_summon_effect")]
    pub summon_effect: String,
    /// Window summon mode: "center" (re-summon centered/last-pos), "dropdown"
    /// (Yakuake-style top-anchored full-width strip that slides down), or
    /// "fullscreen" (cover the whole monitor the window is on; rounded corners
    /// are suppressed while fullscreen). Unknown values fall back to "center".
    ///
    /// This is the SUMMON POLICY, re-applied on every F9. The transient
    /// per-window fullscreen toggle (`toggle_fullscreen`, default F11) never
    /// writes this key.
    #[serde(default = "default_window_mode")]
    pub window_mode: String,
    /// Dropdown height as a fraction of the monitor height (0.25..=1.0).
    #[serde(default = "default_dropdown_height_pct")]
    pub dropdown_height_pct: f32,
    /// Dropdown width as a fraction of the monitor width (0.2..=1.0). Reserved;
    /// the MVP ships full-width (1.0). No UI slider yet.
    #[serde(default = "default_dropdown_width_pct")]
    pub dropdown_width_pct: f32,
    /// Hide the window on focus loss (Yakuake-style auto-hide). Default ON.
    #[serde(default = "default_focus_autohide")]
    pub focus_autohide: bool,
    /// Launch JeTTY at login via the freedesktop XDG autostart standard (a
    /// `.desktop` file under `~/.config/autostart/`). Default OFF. The autostart
    /// file's existence is the source of truth at runtime; this stored bool is a
    /// mirror.
    #[serde(default = "default_launch_at_login")]
    pub launch_at_login: bool,
    /// Global summon hotkey, e.g. "F9" (default), "F12", or "Ctrl+Shift+F12".
    /// Parsed by `global_hotkey`'s `HotKey::from_str`. Config-only (no panel UI).
    #[serde(default = "default_summon_hotkey")]
    pub summon_hotkey: String,
    /// Shell to launch. Empty (default) = auto-detect: `$SHELL`, then the
    /// passwd login shell, then `/bin/bash`. Set an absolute path (e.g.
    /// "/usr/bin/zsh", "/usr/bin/fish") to force a specific shell — useful when
    /// your login shell is bash but you live in another shell. Config-only.
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Tab-bar position: "top" (default) or "bottom". Orthogonal to
    /// `window_mode` — usable in Center, Dropdown and Fullscreen modes alike.
    #[serde(default = "default_tab_bar_position")]
    pub tab_bar_position: String,
    /// Scrollback history limit in lines (default 10_000). Clamped on load to
    /// 100..=100_000 — the ceiling is alacritty's own UI max; at ≤24 B/cell a
    /// fully-filled 100k-line history costs hundreds of MB per tab, so raising
    /// it further needs a memory revisit. Hand-edited values are kept verbatim
    /// (the Settings cycler snaps to its nearest step only when clicked).
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    /// Show the neofetch-style welcome splash on launch (dismissed on first input).
    /// Default `true`. Set to `false` to skip the splash entirely.
    #[serde(default = "default_show_welcome")]
    pub show_welcome: bool,
    /// Show the live performance HUD in the tab bar (frame ms · fps · CPU% ·
    /// VT MB/s). Default `true`. The HUD never forces a redraw — it updates only
    /// inside frames already happening for some other reason, so the 0-CPU idle
    /// path is preserved. Set to `false` to skip it (and the sysinfo sampling)
    /// entirely.
    #[serde(default = "default_show_perf_hud")]
    pub show_perf_hud: bool,
    /// Visual effects (CRT, scanlines, caret). See `EffectsConfig`. Backward
    /// compatible: old configs without `[effects]` load with all defaults.
    #[serde(default)]
    pub effects: EffectsConfig,
    // ── Run & Notify (v0.15) ──────────────────────────────────────────────────
    /// Notify (freedesktop toast + taskbar/dock urgency) when a command finishes
    /// while JeTTY is hidden/unfocused. Default ON — but inert until the user
    /// wires up OSC 133 shell integration, so a default install never notifies.
    /// Each key is `#[serde(default)]`, so an older config (missing them) loads
    /// with these defaults, exactly like every other flat key.
    #[serde(default = "default_notify_on_command_finish")]
    pub notify_on_command_finish: bool,
    /// Minimum command duration (seconds) to notify on SUCCESS. Failures may ping
    /// below this (see the notifier's failure floor). Clamped 1..=86_400 on load.
    #[serde(default = "default_notify_min_seconds")]
    pub notify_min_seconds: u64,
    /// Only notify on FAILED commands (nonzero exit). Default off. Note: plain
    /// bash (no bash-preexec) emits no duration, so it is failure-only regardless.
    #[serde(default = "default_notify_only_on_failure")]
    pub notify_only_on_failure: bool,
    /// Raise + focus JeTTY (and activate the firing tab) when a command finishes,
    /// but ONLY when it is fully hidden — never steal focus mid-typing. Default
    /// OFF. Inherits `notify_only_on_failure` (so it can be a failures-only summon).
    #[serde(default = "default_auto_summon_on_finish")]
    pub auto_summon_on_finish: bool,
    // ── SSH-ready & yours (v0.16) ─────────────────────────────────────────────
    /// Allow OSC 52 clipboard PASTE — i.e. let a program in the terminal (including
    /// a remote host over SSH) READ the local system clipboard. Default `false`
    /// (the SECURE default alacritty enforces): OSC 52 COPY always works, but paste
    /// can exfiltrate whatever is on the clipboard (passwords/tokens), so it is
    /// strictly opt-in. Applies to newly-spawned tabs.
    #[serde(default = "default_osc52_allow_paste")]
    pub osc52_allow_paste: bool,
    /// Run-selection-in-a-new-tab (the "open link in a new tab" gesture for
    /// commands): select text → run it in a new tab. Default ON. `false`
    /// disables EVERY trigger (context-menu row, Ctrl+Shift+Enter, palette,
    /// copy-mode `r`, detached menu) — the opt-out for users who don't want a
    /// feature that writes to a PTY. Hot-reloadable.
    #[serde(default = "default_run_selection")]
    pub run_selection: bool,
    /// Watch `~/.config/jetty/` and hot-reload config + themes live (no restart).
    /// Default `true`. The watcher is OS-event-driven (inotify/FSEvents), so it adds
    /// zero idle CPU; set `false` to disable it entirely (a pure escape hatch — no
    /// watcher thread is spawned). NOTE: `summon_hotkey` and `launch_at_login` are
    /// RESTART/external-only even with hot-reload on (documented at those keys).
    #[serde(default = "default_hot_reload")]
    pub hot_reload: bool,
    /// User keybinding overrides (`[keys]` table). Every action defaults to its
    /// built-in chord when omitted; `""`/`[]` explicitly UNBINDS an action (the
    /// chord reverts to its raw terminal meaning). Backward compatible: an old
    /// config without `[keys]` loads with every default. The whole table is
    /// skipped on save when empty, so a default install never writes a bare
    /// `[keys]` header.
    #[serde(default, skip_serializing_if = "KeyBindings::is_empty")]
    pub keys: KeyBindings,
}

/// A single chord string, or an array of chord strings that all trigger the same
/// action. Accepts both TOML forms (`copy = "Ctrl+Shift+C"` or
/// `paste = ["Ctrl+Shift+V", "Shift+Insert"]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChordSpec {
    One(String),
    Many(Vec<String>),
}

impl ChordSpec {
    /// The chord strings in this spec (one, or many). An empty string / empty
    /// array yields no usable chords → the action is explicitly unbound.
    pub fn chords(&self) -> Vec<&str> {
        match self {
            ChordSpec::One(s) => vec![s.as_str()],
            ChordSpec::Many(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Per-action keybinding overrides. Each field is `Option<ChordSpec>`: `None`
/// (the field omitted from `[keys]`) uses the built-in default; `Some` replaces
/// it. `select_tab_1..9` cover the `Ctrl+1..9` tab jumps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyBindings {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub toggle_settings: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub open_palette: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub new_tab: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub close_tab: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub detach_tab: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub search_toggle: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub prev_prompt: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub next_prompt: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub prev_tab: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub next_tab: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_1: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_2: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_3: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_4: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_5: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_6: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_7: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_8: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_tab_9: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub copy: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub paste: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub opacity_up: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub opacity_down: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_up: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_down: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub font_reset: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub select_all: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub quit: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub hint_mode: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub copy_mode: Option<ChordSpec>,
    /// `run_selection = ""` unbinds the chord (the menu/palette/copy-mode
    /// triggers remain; the whole feature's off-switch is the top-level
    /// `run_selection = false`).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub run_selection: Option<ChordSpec>,
    /// `toggle_fullscreen = ""` gives bare F11 back to the shell (`\e[23~`).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub toggle_fullscreen: Option<ChordSpec>,
}

impl KeyBindings {
    /// True when no action is overridden — used to skip serializing a bare,
    /// empty `[keys]` table on a default install.
    pub fn is_empty(&self) -> bool {
        *self == KeyBindings::default()
    }
}

fn default_osc52_allow_paste() -> bool {
    false
}
fn default_run_selection() -> bool {
    true
}
fn default_hot_reload() -> bool {
    true
}

fn default_notify_on_command_finish() -> bool {
    true
}
fn default_notify_min_seconds() -> u64 {
    10
}
fn default_notify_only_on_failure() -> bool {
    false
}
fn default_auto_summon_on_finish() -> bool {
    false
}

fn default_theme() -> String {
    "catppuccin_mocha".to_string()
}

fn default_opacity() -> f32 {
    1.0
}

fn default_font_size() -> f32 {
    16.0
}

fn default_font_family() -> String {
    "MesloLGS NF".to_string()
}

fn default_corner_radius() -> f32 {
    10.0
}

fn default_shell() -> String {
    String::new()
}

fn default_summon_effect() -> String {
    "phosphor".to_string()
}

fn default_window_mode() -> String {
    "center".to_string()
}

fn default_dropdown_height_pct() -> f32 {
    0.50
}

fn default_dropdown_width_pct() -> f32 {
    1.0
}

fn default_focus_autohide() -> bool {
    true
}

fn default_launch_at_login() -> bool {
    false
}

fn default_summon_hotkey() -> String {
    "F9".to_string()
}

fn default_tab_bar_position() -> String {
    "top".to_string()
}

fn default_scrollback_lines() -> usize {
    10_000
}

fn default_show_welcome() -> bool {
    true
}

fn default_show_perf_hud() -> bool {
    true
}

/// UI font default: empty string → platform proportional sans. Mirrors the
/// terminal default look (tab titles already render in sans), so a config
/// without this key renders chrome exactly as before.
fn default_ui_font_family() -> String {
    String::new()
}

/// UI font default size: 16pt == today's fixed chrome size, so an upgraded
/// config without this key looks identical.
fn default_ui_font_size() -> f32 {
    16.0
}

/// All visual-effect parameters. Every field is `#[serde(default)]` so adding
/// the `[effects]` table is backward compatible: an old config without it (or
/// missing any field) loads with the defaults below. All effects default OFF
/// except `caret_flash_enabled`, so the out-of-box look/idle profile is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectsConfig {
    #[serde(default = "ef_false")] pub crt_enabled: bool,
    #[serde(default = "ef_curvature")] pub crt_curvature: f32,
    #[serde(default = "ef_scanline")] pub crt_scanline: f32,
    #[serde(default = "ef_mask")] pub crt_mask: f32,
    #[serde(default = "ef_bloom")] pub crt_bloom: f32,
    #[serde(default = "ef_chromatic")] pub crt_chromatic: f32,
    #[serde(default = "ef_vignette")] pub crt_vignette: f32,
    #[serde(default = "ef_white")] pub crt_scanline_tint: [f32; 3],
    #[serde(default = "ef_false")] pub crt_animate_roll: bool,
    #[serde(default = "ef_false")] pub crt_flicker: bool,
    #[serde(default = "ef_false")] pub crt_jitter: bool,
    #[serde(default = "ef_true")] pub caret_flash_enabled: bool,
    #[serde(default = "ef_false")] pub caret_glow_enabled: bool,
    #[serde(default = "ef_flash_ms")] pub caret_flash_ms: f32,
    #[serde(default = "ef_white")] pub caret_flash_color: [f32; 3],
}

fn ef_false() -> bool { false }
fn ef_true() -> bool { true }
fn ef_curvature() -> f32 { 0.0 }
fn ef_scanline() -> f32 { 0.50 }
fn ef_mask() -> f32 { 0.30 }
fn ef_bloom() -> f32 { 0.40 }
fn ef_chromatic() -> f32 { 0.20 }
fn ef_vignette() -> f32 { 0.40 }
fn ef_flash_ms() -> f32 { 130.0 }
fn ef_white() -> [f32; 3] { [1.0, 1.0, 1.0] }

impl Default for EffectsConfig {
    fn default() -> Self {
        EffectsConfig {
            crt_enabled: ef_false(), crt_curvature: ef_curvature(), crt_scanline: ef_scanline(),
            crt_mask: ef_mask(), crt_bloom: ef_bloom(), crt_chromatic: ef_chromatic(),
            crt_vignette: ef_vignette(), crt_scanline_tint: ef_white(),
            crt_animate_roll: ef_false(), crt_flicker: ef_false(), crt_jitter: ef_false(),
            caret_flash_enabled: ef_true(), caret_glow_enabled: ef_false(),
            caret_flash_ms: ef_flash_ms(), caret_flash_color: ef_white(),
        }
    }
}

/// Sanitize an `f32` loaded from config: a non-finite value (NaN/±inf — TOML
/// 1.x allows literal `nan`, and `f32::clamp` PROPAGATES NaN, silently
/// defeating every load-time clamp) falls back to `default`; a finite value is
/// returned unchanged for the caller's normal clamp.
fn finite_or(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

impl EffectsConfig {
    /// Clamp every numeric field into its valid range, replacing non-finite
    /// values (NaN/±inf survive TOML parsing and pass through `clamp`) with the
    /// field's default. Called on load.
    pub fn clamped(mut self) -> Self {
        let c01 = |v: f32, d: f32| finite_or(v, d).clamp(0.0, 1.0);
        self.crt_curvature = c01(self.crt_curvature, ef_curvature());
        self.crt_scanline = c01(self.crt_scanline, ef_scanline());
        self.crt_mask = c01(self.crt_mask, ef_mask());
        self.crt_bloom = c01(self.crt_bloom, ef_bloom());
        self.crt_chromatic = c01(self.crt_chromatic, ef_chromatic());
        self.crt_vignette = c01(self.crt_vignette, ef_vignette());
        for ch in &mut self.crt_scanline_tint { *ch = c01(*ch, 1.0); }
        for ch in &mut self.caret_flash_color { *ch = c01(*ch, 1.0); }
        self.caret_flash_ms = finite_or(self.caret_flash_ms, ef_flash_ms()).clamp(60.0, 400.0);
        self
    }

    /// True iff an *animated* CRT sub-effect is live: CRT enabled AND at least one
    /// of roll/flicker/jitter toggled on. Static CRT (enabled, all three off) is
    /// `false`, so it stays damage-driven (0-CPU idle). Single source of truth for
    /// BOTH the `RedrawRequested` self-redraw guard AND the `about_to_wait`
    /// `main_pending` Poll term — keeping them identical is what makes the loop pump
    /// frames under `Poll` on macOS (where a `request_redraw` issued under `Wait` is
    /// not delivered until input) yet fall back to `Wait`/idle the instant animation
    /// is off. Lives on `EffectsConfig` (not `App`) so callers borrow only the `fx`
    /// field, leaving `gpu`/`text` free to be mutably borrowed in the render path.
    pub fn crt_anim_live(&self) -> bool {
        self.crt_enabled && (self.crt_animate_roll || self.crt_flicker || self.crt_jitter)
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: default_theme(),
            opacity: default_opacity(),
            font_size: default_font_size(),
            font_family: default_font_family(),
            ui_font_family: default_ui_font_family(),
            ui_font_size: default_ui_font_size(),
            corner_radius: default_corner_radius(),
            summon_effect: default_summon_effect(),
            window_mode: default_window_mode(),
            dropdown_height_pct: default_dropdown_height_pct(),
            dropdown_width_pct: default_dropdown_width_pct(),
            focus_autohide: default_focus_autohide(),
            launch_at_login: default_launch_at_login(),
            summon_hotkey: default_summon_hotkey(),
            shell: default_shell(),
            tab_bar_position: default_tab_bar_position(),
            scrollback_lines: default_scrollback_lines(),
            show_welcome: default_show_welcome(),
            show_perf_hud: default_show_perf_hud(),
            effects: EffectsConfig::default(),
            notify_on_command_finish: default_notify_on_command_finish(),
            notify_min_seconds: default_notify_min_seconds(),
            notify_only_on_failure: default_notify_only_on_failure(),
            auto_summon_on_finish: default_auto_summon_on_finish(),
            osc52_allow_paste: default_osc52_allow_paste(),
            run_selection: default_run_selection(),
            hot_reload: default_hot_reload(),
            keys: KeyBindings::default(),
        }
    }
}

/// What [`Config::load_from`] read.
pub struct Loaded {
    pub cfg: Config,
    /// Human-readable problems — invalid values, unknown keys, a syntax error —
    /// for the app to show in its own UI (a desktop launch has no visible stderr).
    pub warnings: Vec<String>,
    /// Hash of the bytes read (`None` when there was no file): the persister's
    /// baseline for telling its own writes from external edits.
    pub hash: Option<u64>,
}

impl Config {
    /// Resolve the JeTTY config DIRECTORY: `$JETTY_CONFIG_DIR` when set (a whole
    /// alternate config tree — e.g. to try a setup without touching your own),
    /// else `<config_dir>/jetty` (`~/.config/jetty` on Linux, `~/Library/
    /// Application Support/jetty` on macOS), falling back to `~/.config/jetty` when
    /// the OS dir is unknown. It holds `config.toml` and `themes/`; the hot-reload
    /// watcher and the theme loader both key off it.
    pub(crate) fn dir() -> PathBuf {
        if let Some(d) = std::env::var_os("JETTY_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(d);
        }
        let base = dirs::config_dir().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
        base.join("jetty")
    }

    /// The config file path: `<dir>/config.toml`.
    pub(crate) fn config_path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Load `config.toml` at startup (see [`Config::load_from`]).
    pub fn load() -> Loaded {
        Self::load_from(&Self::config_path())
    }

    /// Load settings from `path`. Never panics and never modifies the file.
    ///
    /// * missing file → defaults, no warnings;
    /// * valid TOML → every valid key applies; an invalid value falls back to its
    ///   default and an unknown key is ignored — each with a warning;
    /// * not TOML at all → defaults in memory, and the file is COPIED aside to
    ///   `config.toml.bad-<unix-secs>` (never moved, so a symlinked config stays
    ///   linked; an earlier copy is never overwritten). The original is left alone
    ///   and saving is refused until it parses again (the writer re-checks on every
    ///   save), so a typo can never turn into a config reset.
    pub fn load_from(path: &Path) -> Loaded {
        let s = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { cfg: Config::default(), warnings: Vec::new(), hash: None };
            }
            Err(e) => {
                return Loaded {
                    cfg: Config::default(),
                    warnings: vec![format!(
                        "could not read {}: {e} — using the default settings",
                        path.display()
                    )],
                    hash: None,
                };
            }
        };
        let hash = Some(hash_str(&s));
        match Self::parse_with_base(&s, &Config::default(), "using the default") {
            Ok((cfg, warnings)) => Loaded { cfg, warnings, hash },
            Err(syntax) => {
                let copy = match preserve_copy(path, "bad") {
                    Ok(p) => format!("a copy is at {}", p.display()),
                    Err(e) => format!("could not copy it aside: {e}"),
                };
                Loaded {
                    cfg: Config::default(),
                    warnings: vec![format!(
                        "config.toml is not valid TOML ({syntax}) — running on defaults; \
                         your file is untouched ({copy}) and settings changes are not \
                         saved until it is fixed"
                    )],
                    hash,
                }
            }
        }
    }

    /// Parse config text key by key on top of `base`.
    ///
    /// `Err` only for a TOML syntax error (the whole document is unreadable). Valid
    /// TOML always yields a config: each key with an invalid value is replaced by
    /// `base`'s value — the defaults at startup, the live settings on a hot-reload —
    /// and reported (`fallback` says which, e.g. "using the default"); unknown keys
    /// are ignored with a warning. Generic over the struct: a new field needs no
    /// code here. Takes the already-read content so the caller can hash and parse
    /// the SAME bytes (no TOCTOU between the two).
    pub(crate) fn parse_with_base(
        s: &str,
        base: &Config,
        fallback: &str,
    ) -> Result<(Config, Vec<String>), String> {
        let user: toml::Table = toml::from_str(s).map_err(|e| describe_toml_error(&e, s))?;
        let mut warnings = Vec::new();
        let mut invalid: Vec<Vec<String>> = Vec::new();
        let cfg = match toml::Value::Table(user.clone()).try_into::<Config>() {
            Ok(cfg) => cfg,
            Err(_) => Self::fallback_per_key(&user, base, fallback, &mut warnings, &mut invalid),
        };
        unknown_key_warnings(&user, &cfg, &invalid, &mut warnings);
        Ok((cfg.sanitized(), warnings))
    }

    /// The slow path of [`Config::parse_with_base`]: probe every leaf of `user` on
    /// its own (against the defaults, so a failure can only be that leaf's), swap
    /// each invalid one for `base`'s value (or drop it when `base` has none), then
    /// deserialize the repaired table.
    fn fallback_per_key(
        user: &toml::Table,
        base: &Config,
        fallback: &str,
        warnings: &mut Vec<String>,
        invalid: &mut Vec<Vec<String>>,
    ) -> Config {
        let defaults = to_table(&Config::default());
        let base_t = to_table(base);
        let mut doc = user.clone();
        let mut leaves = Vec::new();
        collect_leaves(user, &defaults, &mut Vec::new(), &mut leaves);
        for (path, value) in leaves {
            let mut probe = defaults.clone();
            set_path(&mut probe, &path, value.clone());
            if let Err(e) = toml::Value::Table(probe).try_into::<Config>() {
                warnings.push(format!(
                    "`{} = {}` is invalid ({}) — {fallback}",
                    path.join("."),
                    short_value(&value),
                    friendly_expected(e.message()),
                ));
                match get_path(&base_t, &path) {
                    Some(v) => set_path(&mut doc, &path, v.clone()),
                    None => remove_path(&mut doc, &path),
                }
                invalid.push(path);
            }
        }
        match toml::Value::Table(doc).try_into::<Config>() {
            Ok(cfg) => cfg,
            // Unreachable for this struct (fields are independent), but a wholly
            // unusable table must still never panic or apply garbage.
            Err(e) => {
                warnings.push(format!(
                    "config.toml could not be applied ({}) — {fallback}",
                    friendly_expected(e.message())
                ));
                base.clone()
            }
        }
    }

    /// Sanitize a freshly deserialized config (non-finite floats, clamped ranges).
    fn sanitized(mut self) -> Config {
        self.sanitize_floats();
        self.effects = self.effects.clamped();
        self
    }

    /// Replace every non-finite float with its default. TOML 1.x allows a
    /// literal `nan` and the toml crate deserializes it, while `f32::clamp`
    /// PROPAGATES NaN — so a hand-edited `opacity = nan` sailed through every
    /// load-time clamp (invisible window, collapsed grid, NaN shader uniforms)
    /// and `save()` persisted it right back. Finite values pass through
    /// untouched; the normal range clamps in `App::new` still apply after.
    fn sanitize_floats(&mut self) {
        self.opacity = finite_or(self.opacity, 1.0);
        self.font_size = finite_or(self.font_size, 16.0);
        self.ui_font_size = finite_or(self.ui_font_size, default_ui_font_size());
        self.corner_radius = finite_or(self.corner_radius, 10.0);
        self.dropdown_height_pct =
            finite_or(self.dropdown_height_pct, default_dropdown_height_pct());
        self.dropdown_width_pct =
            finite_or(self.dropdown_width_pct, default_dropdown_width_pct());
        // Not a float, but this fn is the single sanitize entry point (the name
        // predates non-float sanitizing): keep hand-edited values verbatim, only
        // clamp to the supported range.
        self.scrollback_lines = self.scrollback_lines.clamp(100, 100_000);
        // Notify minimum duration: keep hand-edited values verbatim within a sane
        // range (≥1s so a 0 can't make every command "long"; ≤1 day ceiling).
        self.notify_min_seconds = self.notify_min_seconds.clamp(1, 86_400);
        // Effects floats are sanitized by `EffectsConfig::clamped` (see sanitized()).
    }
}

// ── Per-key parsing helpers ──────────────────────────────────────────────────

/// Hash config-file text to a `u64` (self-write guard for hot-reload). Content-
/// based and dependency-free; only equality matters, so the exact algorithm is
/// irrelevant as long as it is deterministic within a process run.
pub(crate) fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// `cfg` as a TOML table (the same shape `config.toml` has).
fn to_table(cfg: &Config) -> toml::Table {
    match toml::Value::try_from(cfg) {
        Ok(toml::Value::Table(t)) => t,
        _ => toml::Table::new(),
    }
}

fn get_path<'a>(t: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut cur = t;
    for p in parents {
        cur = cur.get(p)?.as_table()?;
    }
    cur.get(last)
}

/// Set `path` to `v`, creating (or replacing non-table values with) the
/// intermediate tables.
fn set_path(t: &mut toml::Table, path: &[String], v: toml::Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = t;
    for p in parents {
        let entry = cur.entry(p.clone()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        cur = entry.as_table_mut().expect("just made a table");
    }
    cur.insert(last.clone(), v);
}

fn remove_path(t: &mut toml::Table, path: &[String]) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = t;
    for p in parents {
        match cur.get_mut(p).and_then(|v| v.as_table_mut()) {
            Some(next) => cur = next,
            None => return,
        }
    }
    cur.remove(last);
}

/// Every leaf of `user` with its path. A table is descended into only where the
/// defaults expect a table there (or have nothing — e.g. the `[keys]` overrides);
/// a table standing where a scalar belongs is itself one (invalid) leaf.
fn collect_leaves(
    user: &toml::Table,
    defaults: &toml::Table,
    prefix: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, toml::Value)>,
) {
    for (k, v) in user {
        prefix.push(k.clone());
        let default_here = get_path(defaults, prefix);
        match v {
            toml::Value::Table(sub) if default_here.is_none_or(|d| d.is_table()) => {
                collect_leaves(sub, defaults, prefix, out);
            }
            _ => out.push((prefix.clone(), v.clone())),
        }
        prefix.pop();
    }
}

/// Warn about every leaf the user wrote that the parsed config doesn't carry —
/// i.e. a key serde silently ignored (a typo like `fontsize`, a stale key). Keys
/// already reported as invalid are skipped. Found by round-tripping the parsed
/// struct, so it needs no list of known keys.
fn unknown_key_warnings(
    user: &toml::Table,
    cfg: &Config,
    invalid: &[Vec<String>],
    warnings: &mut Vec<String>,
) {
    let round = to_table(cfg);
    let mut leaves = Vec::new();
    collect_leaves(user, &toml::Table::new(), &mut Vec::new(), &mut leaves);
    for (path, _) in leaves {
        if invalid.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        if get_path(&round, &path).is_none() {
            warnings.push(format!("unknown key `{}` is ignored", path.join(".")));
        }
    }
}

/// A short TOML rendering of a user value for a warning (long values elided).
fn short_value(v: &toml::Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 {
        let cut: String = s.chars().take(37).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// Turn serde's "invalid type: string \"0.9\", expected f32" into a hint a user
/// can act on ("expected a number").
fn friendly_expected(msg: &str) -> String {
    let msg = msg.trim();
    if msg.contains("untagged enum ChordSpec") {
        return "expected a chord string or a list of chord strings".to_string();
    }
    let Some((_, expected)) = msg.rsplit_once("expected ") else {
        return msg.to_string();
    };
    let what = match expected.trim() {
        "f32" | "f64" => "a number",
        "u8" | "u16" | "u32" | "u64" | "usize" => "a whole number ≥ 0",
        "i8" | "i16" | "i32" | "i64" | "isize" => "a whole number",
        "a boolean" => "true or false",
        "a string" => "a quoted string",
        other => return format!("expected {other}"),
    };
    format!("expected {what}")
}

/// One-line description of a TOML syntax error: `line N: message`.
fn describe_toml_error(e: &toml::de::Error, src: &str) -> String {
    let msg = e.message().trim();
    match e.span() {
        Some(span) => {
            // Count newlines over BYTES: the span is a byte offset, and slicing the
            // str there could split a multi-byte char.
            let end = span.start.min(src.len());
            let line = src.as_bytes()[..end].iter().filter(|&&b| b == b'\n').count() + 1;
            format!("line {line}: {msg}")
        }
        None => msg.to_string(),
    }
}

/// Copy `path` (following a symlink: the CONTENT is copied, the link is left
/// alone) to `<name>.<tag>-<unix-secs>` beside it, opened `create_new` so an
/// existing copy is never overwritten. An identical earlier copy is reused, so a
/// file that stays broken across restarts leaves one copy, not one per launch.
pub(crate) fn preserve_copy(path: &Path, tag: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let content = std::fs::read(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("config.toml");
    let prefix = format!("{name}.{tag}-");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let is_copy = e.file_name().to_str().is_some_and(|n| n.starts_with(&prefix));
            if is_copy && std::fs::read(e.path()).is_ok_and(|c| c == content) {
                return Ok(e.path());
            }
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for n in 0..100u32 {
        let cand = if n == 0 {
            dir.join(format!("{prefix}{secs}"))
        } else {
            dir.join(format!("{prefix}{secs}-{n}"))
        };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&cand) {
            Ok(mut f) => {
                f.write_all(&content)?;
                f.sync_all()?;
                return Ok(cand);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free backup file name"))
}

// ── Diff + in-place edit ─────────────────────────────────────────────────────

/// One changed setting: its TOML key path and new value (`None` = remove the key,
/// e.g. a `[keys]` override reset back to its built-in default).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Change {
    pub path: Vec<String>,
    pub value: Option<toml::Value>,
}

/// The keys whose values differ between `old` and `new`, as minimal changes
/// (nested tables are diffed key by key).
pub(crate) fn diff_configs(old: &Config, new: &Config) -> Vec<Change> {
    let mut out = Vec::new();
    diff_tables(&to_table(old), &to_table(new), &mut Vec::new(), &mut out);
    out
}

fn diff_tables(old: &toml::Table, new: &toml::Table, prefix: &mut Vec<String>, out: &mut Vec<Change>) {
    for (k, nv) in new {
        prefix.push(k.clone());
        match (old.get(k), nv) {
            (Some(ov), _) if ov == nv => {}
            (Some(toml::Value::Table(ot)), toml::Value::Table(nt)) => diff_tables(ot, nt, prefix, out),
            _ => out.push(Change { path: prefix.clone(), value: Some(nv.clone()) }),
        }
        prefix.pop();
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            let mut path = prefix.clone();
            path.push(k.clone());
            out.push(Change { path, value: None });
        }
    }
}

/// A config float (always an `f32` widened to `f64`) in its shortest f32 form, so
/// `0.85` is written as `0.85`, not `0.8500000238418579`.
fn clean_float(f: f64) -> f64 {
    let g = f as f32;
    if g.is_finite() && g as f64 == f {
        g.to_string().parse().unwrap_or(f)
    } else {
        f
    }
}

fn to_edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => s.clone().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => clean_float(*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(d) => (*d).into(),
        toml::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(to_edit_value(x));
            }
            arr.into()
        }
        toml::Value::Table(t) => {
            let mut it = toml_edit::InlineTable::new();
            for (k, x) in t {
                it.insert(k, to_edit_value(x));
            }
            it.into()
        }
    }
}

fn to_edit_item(v: &toml::Value) -> toml_edit::Item {
    match v {
        toml::Value::Table(t) => {
            let mut tab = toml_edit::Table::new();
            for (k, x) in t {
                tab.insert(k, to_edit_item(x));
            }
            toml_edit::Item::Table(tab)
        }
        _ => toml_edit::Item::Value(to_edit_value(v)),
    }
}

/// Apply one change to the document IN PLACE: an existing value is replaced
/// keeping its key, position, surrounding whitespace and trailing comment; a new
/// key is appended to its table (missing tables are created as `[table]`s); a
/// removal deletes the key, and a table it leaves empty.
fn apply_change(doc: &mut toml_edit::DocumentMut, change: &Change) {
    let Some((last, parents)) = change.path.split_last() else { return };
    apply_in(doc.as_table_mut(), parents, last, change.value.as_ref());
}

fn apply_in(
    table: &mut dyn toml_edit::TableLike,
    parents: &[String],
    last: &str,
    value: Option<&toml::Value>,
) {
    if let Some((p, rest)) = parents.split_first() {
        if !table.get(p).is_some_and(|i| i.is_table_like()) {
            if value.is_none() {
                return; // nothing to remove under a table that isn't there
            }
            table.insert(p, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        let Some(sub) = table.get_mut(p).and_then(|i| i.as_table_like_mut()) else { return };
        apply_in(sub, rest, last, value);
        if value.is_none() && sub.is_empty() {
            table.remove(p);
        }
        return;
    }
    match value {
        None => {
            table.remove(last);
        }
        Some(v) => match table.get_mut(last).and_then(|i| i.as_value_mut()) {
            Some(old) if !v.is_table() => {
                let decor = old.decor().clone();
                *old = to_edit_value(v);
                *old.decor_mut() = decor;
            }
            _ => {
                table.insert(last, to_edit_item(v));
            }
        },
    }
}

// ── Debounced, off-UI-thread persistence ─────────────────────────────────────

/// How long settings changes coalesce before ONE write (a held font/opacity key
/// or a slider drag is one save, not dozens of fsyncs).
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// State shared by the UI thread and the writer thread.
#[derive(Debug, Default)]
struct SyncShared {
    /// Hash of config.toml as JeTTY last saw it (read at load/reload, or written).
    known: Option<u64>,
    /// Hash of JeTTY's own last write: its watcher echo is skipped on reload.
    self_written: Option<u64>,
    /// Writes handed to the writer thread and not finished yet.
    inflight: usize,
}

enum WriterMsg {
    Write { changes: Vec<Change>, full: Box<Config> },
    Barrier(std::sync::mpsc::Sender<()>),
}

fn lock(m: &Mutex<SyncShared>) -> std::sync::MutexGuard<'_, SyncShared> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Saves settings changes to `config.toml` without blocking the UI thread.
///
/// The UI calls [`Persister::record`] with the full current settings on every
/// change; only the keys that differ from the last synced state become pending
/// changes. After [`SAVE_DEBOUNCE`] of quiet the app calls [`Persister::flush`],
/// handing them to a background writer that re-reads the file, edits just those
/// keys in place (`toml_edit`) and writes atomically. Because the file is re-read
/// at write time, a concurrent external edit to OTHER keys survives; when one is
/// detected the write is not marked as our own, so the hot-reload applies it.
pub(crate) struct Persister {
    path: PathBuf,
    /// The settings as the file holds them once every pending change is written.
    synced: Config,
    pending: Vec<Change>,
    due_at: Option<Instant>,
    shared: Arc<Mutex<SyncShared>>,
    tx: Option<std::sync::mpsc::Sender<WriterMsg>>,
    notice: Option<Box<dyn Fn(String) + Send>>,
}

impl Persister {
    /// `synced` = the settings `path` currently holds; `known` = the hash of its
    /// bytes (`None` when there is no file). `notice` receives user-facing save
    /// problems (called on the writer thread).
    pub(crate) fn new(
        path: PathBuf,
        synced: Config,
        known: Option<u64>,
        notice: Box<dyn Fn(String) + Send>,
    ) -> Persister {
        Persister {
            path,
            synced,
            pending: Vec::new(),
            due_at: None,
            shared: Arc::new(Mutex::new(SyncShared { known, ..SyncShared::default() })),
            tx: None,
            notice: Some(notice),
        }
    }

    /// Record the current settings; returns whether anything changed (and a
    /// save is now scheduled `SAVE_DEBOUNCE` from `now`).
    pub(crate) fn record(&mut self, current: &Config, now: Instant) -> bool {
        let changes = diff_configs(&self.synced, current);
        if changes.is_empty() {
            return false;
        }
        for c in changes {
            // A later change to the same key supersedes the pending one; changes
            // are applied in order, so overlapping paths compose correctly.
            self.pending.retain(|p| p.path != c.path);
            self.pending.push(c);
        }
        self.synced = current.clone();
        self.due_at = Some(now + SAVE_DEBOUNCE);
        true
    }

    /// When the debounced save is due (`None` = nothing pending).
    pub(crate) fn due_at(&self) -> Option<Instant> {
        self.due_at
    }

    /// Hand every pending change to the writer now (non-blocking).
    pub(crate) fn flush(&mut self) {
        self.due_at = None;
        if self.pending.is_empty() {
            return;
        }
        let changes = std::mem::take(&mut self.pending);
        let full = Box::new(self.synced.clone());
        lock(&self.shared).inflight += 1;
        let sent = self.writer().is_some_and(|tx| {
            tx.send(WriterMsg::Write { changes: changes.clone(), full: full.clone() }).is_ok()
        });
        if !sent {
            // No writer thread (spawn failed): write synchronously rather than lose
            // the change.
            if let Err(e) = write_changes(&self.path, &changes, &full, &self.shared) {
                eprintln!("jetty: {e}");
            }
            lock(&self.shared).inflight -= 1;
        }
    }

    /// Flush and wait (up to `timeout`) until the writer has finished — for exit,
    /// so the last change reaches the disk. Returns whether it finished in time.
    pub(crate) fn flush_and_wait(&mut self, timeout: Duration) -> bool {
        self.flush();
        let Some(tx) = self.tx.as_ref() else { return true };
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if tx.send(WriterMsg::Barrier(ack_tx)).is_err() {
            return true;
        }
        ack_rx.recv_timeout(timeout).is_ok()
    }

    /// True while a save is pending or being written — a hot-reload must wait,
    /// or it would read the file before our change lands and revert it in memory.
    pub(crate) fn busy(&self) -> bool {
        !self.pending.is_empty() || lock(&self.shared).inflight > 0
    }

    /// Is `hash` the content of our own last write (its watcher echo)?
    pub(crate) fn is_self_write(&self, hash: u64) -> bool {
        lock(&self.shared).self_written == Some(hash)
    }

    /// A hot-reload applied `cfg`, read from bytes hashing to `hash`.
    pub(crate) fn note_reloaded(&mut self, cfg: Config, hash: u64) {
        self.synced = cfg;
        let mut s = lock(&self.shared);
        s.known = Some(hash);
        // An identical later re-save of the same content is then a no-op too.
        s.self_written = Some(hash);
    }

    /// A reload saw bytes hashing to `hash` but couldn't apply them (syntax error).
    pub(crate) fn note_seen(&mut self, hash: u64) {
        lock(&self.shared).known = Some(hash);
    }

    /// Copy the current config aside to `config.toml.bak-<secs>` after flushing
    /// pending saves (so the backup is of what the user has). `Ok(None)` when
    /// there is no file to back up.
    pub(crate) fn backup(&mut self) -> std::io::Result<Option<PathBuf>> {
        self.flush_and_wait(Duration::from_secs(2));
        if !self.path.exists() {
            return Ok(None);
        }
        preserve_copy(&self.path, "bak").map(Some)
    }

    fn writer(&mut self) -> Option<&std::sync::mpsc::Sender<WriterMsg>> {
        if self.tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<WriterMsg>();
            let path = self.path.clone();
            let shared = Arc::clone(&self.shared);
            let notice = self.notice.take().unwrap_or_else(|| Box::new(|_| {}));
            let spawned = std::thread::Builder::new()
                .name("jetty-config-writer".to_string())
                .spawn(move || {
                    for msg in rx {
                        match msg {
                            WriterMsg::Write { changes, full } => {
                                if let Err(e) = write_changes(&path, &changes, &full, &shared) {
                                    eprintln!("jetty: {e}");
                                    notice(e);
                                }
                                let mut s = lock(&shared);
                                s.inflight = s.inflight.saturating_sub(1);
                            }
                            WriterMsg::Barrier(ack) => {
                                let _ = ack.send(());
                            }
                        }
                    }
                });
            if spawned.is_err() {
                return None;
            }
            self.tx = Some(tx);
        }
        self.tx.as_ref()
    }
}

/// Write `changes` into `path` in place (or `full` when there is no file yet),
/// atomically. Refuses — returning a user-facing message — when the existing file
/// is not valid TOML: it would have to be replaced wholesale, losing the user's
/// text. Records the written hash as our own, unless the file changed externally
/// since we last saw it (then the hot-reload must re-apply the merged result).
fn write_changes(
    path: &Path,
    changes: &[Change],
    full: &Config,
    shared: &Mutex<SyncShared>,
) -> Result<(), String> {
    let before = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {} to save settings: {e}", path.display())),
    };
    let new_text = match &before {
        // No file yet: write every setting, so the file documents them all.
        None => toml::to_string_pretty(full).map_err(|e| format!("could not serialize settings: {e}"))?,
        Some(text) => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
                let line = e
                    .span()
                    .map(|s| text.as_bytes()[..s.start.min(text.len())].iter().filter(|&&b| b == b'\n').count() + 1);
                let at = line.map(|l| format!("line {l}: ")).unwrap_or_default();
                format!(
                    "config.toml has a TOML syntax error ({at}{}) — settings changes are not saved until it is fixed",
                    e.message().trim()
                )
            })?;
            for c in changes {
                apply_change(&mut doc, c);
            }
            doc.to_string()
        }
    };
    if before.as_deref() == Some(new_text.as_str()) {
        return Ok(()); // nothing to change on disk
    }
    let h = hash_str(&new_text);
    {
        let mut s = lock(shared);
        let external = s.known != before.as_deref().map(hash_str);
        s.self_written = if external { None } else { Some(h) };
        s.known = Some(h);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create config dir {}: {e}", dir.display()))?;
    }
    write_atomic(path, new_text.as_bytes())
        .map_err(|e| format!("could not save settings to {}: {e}", path.display()))
}

/// Write `data` to `path` atomically: write + fsync a temp file in the SAME
/// directory, then `rename` it over the destination. `persist()` runs on every
/// settings click / font hotkey / slider release, and the previous plain
/// `fs::write` (truncate-then-write) left an empty/partial config.toml if the
/// process died mid-write — which `load()` then silently replaced with full
/// defaults, losing every setting. The temp name is PID-suffixed so two
/// processes never share one temp file; rename is atomic on POSIX, so readers
/// always see either the old or the new complete file.
fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    // If `path` is a symlink (common dotfiles setup: ~/.config/jetty/config.toml
    // → ~/dotfiles/jetty/config.toml), resolve it and atomic-rename over the
    // TARGET, not the link. A rename onto the link path replaces the symlink
    // itself with a plain file, silently detaching the dotfiles repo — every
    // later setting change stops reaching it and the next `stow`/`chezmoi` sync
    // reverts them (F33). canonicalize errs when the path doesn't exist yet
    // (first save) — then we keep the original path and create it normally.
    let path: std::path::PathBuf =
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = path.as_path();
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent dir")
    })?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        // Flush file contents to disk BEFORE the rename so a crash/power loss
        // right after the rename can't leave a zero-length "new" file.
        f.sync_all()?;
        // Preserve the destination's existing permissions (don't reset to the
        // temp's default 0644) so a user's chmod on config.toml survives (F33).
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &tmp,
                std::fs::Permissions::from_mode(meta.permissions().mode()),
            );
        }
        std::fs::rename(&tmp, path)?;
        // fsync the parent directory so the rename (the directory-entry update)
        // is itself durable: without it, a power loss just after rename() can
        // lose the new dirent on ext4/xfs and leave the OLD file — or nothing.
        // Best-effort: some platforms/filesystems don't support directory fsync.
        if let Ok(dir_file) = std::fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        // Best-effort cleanup of the temp file on any failure.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_symlinked_config() {
        // Regression (F33): saving through a symlinked config.toml (dotfiles
        // setup) must update the TARGET and keep the symlink, not replace the
        // link with a plain file.
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("jetty_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let target = base.join("real_config.toml");
        std::fs::write(&target, b"old").unwrap();
        let link = base.join("config.toml");
        symlink(&target, &link).unwrap();

        write_atomic(&link, b"new-data").unwrap();

        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the config path must remain a symlink after save"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new-data",
            "the symlink target must receive the update"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_has_sensible_values() {
        let c = Config::default();
        assert_eq!(c.theme, "catppuccin_mocha");
        assert_eq!(c.opacity, 1.0);
        assert_eq!(c.font_size, 16.0);
        assert_eq!(c.font_family, "MesloLGS NF");
        // UI (chrome) font defaults: empty family (= platform sans) + 16pt, so
        // the out-of-box chrome look is identical to the pre-feature default.
        assert_eq!(c.ui_font_family, "");
        assert_eq!(c.ui_font_size, 16.0);
        assert_eq!(c.corner_radius, 10.0);
        assert_eq!(c.summon_effect, "phosphor");
        assert_eq!(c.window_mode, "center");
        assert_eq!(c.dropdown_height_pct, 0.50);
        assert_eq!(c.dropdown_width_pct, 1.0);
        assert!(c.focus_autohide);
        assert!(!c.launch_at_login);
        assert_eq!(c.summon_hotkey, "F9");
        assert_eq!(c.tab_bar_position, "top");
        assert!(c.show_welcome);
        assert!(c.show_perf_hud);
        assert!(!c.osc52_allow_paste, "osc52 paste is off by default (secure)");
        assert!(c.hot_reload, "hot reload is on by default");
    }

    #[test]
    fn missing_summon_effect_defaults_to_phosphor() {
        // An older config without a summon_effect key still loads (serde default).
        let toml = "theme = \"dracula\"\nopacity = 1.0\nfont_size = 16.0\nfont_family = \"MesloLGS NF\"\ncorner_radius = 10.0\n";
        let c: Config = toml::from_str(toml).expect("deserialize");
        assert_eq!(c.summon_effect, "phosphor");
    }

    #[test]
    fn missing_dropdown_keys_default() {
        // An older config without the dropdown keys still loads (serde defaults),
        // so an existing config.toml is unchanged on upgrade.
        let toml = "theme = \"dracula\"\nopacity = 1.0\nfont_size = 16.0\nfont_family = \"MesloLGS NF\"\ncorner_radius = 10.0\nsummon_effect = \"phosphor\"\n";
        let c: Config = toml::from_str(toml).expect("deserialize");
        assert_eq!(c.window_mode, "center");
        assert_eq!(c.dropdown_height_pct, 0.50);
        assert_eq!(c.dropdown_width_pct, 1.0);
        assert!(c.focus_autohide);
        // An older config without launch_at_login still loads as false (OFF).
        assert!(!c.launch_at_login);
        // An older config without summon_hotkey still loads as "F9".
        assert_eq!(c.summon_hotkey, "F9");
        // An older config without tab_bar_position still loads as "top".
        assert_eq!(c.tab_bar_position, "top");
        // An older config without show_welcome still loads as true.
        assert!(c.show_welcome);
        // An older config without show_perf_hud still loads as true.
        assert!(c.show_perf_hud);
        // An older config without the UI-font keys still loads with the chrome
        // defaults ("" = platform sans, 16pt), so an upgrade is visually a no-op.
        assert_eq!(c.ui_font_family, "");
        assert_eq!(c.ui_font_size, 16.0);
    }

    #[test]
    fn window_mode_fullscreen_round_trips() {
        // The new third value loads verbatim...
        let toml = "window_mode = \"fullscreen\"\n";
        let c: Config = toml::from_str(toml).expect("deserialize");
        assert_eq!(c.window_mode, "fullscreen");
        // ...survives a serialize/deserialize round-trip as the PARSED value
        // (asserting the parsed value, not byte-identical TOML, so key order or
        // formatting changes in the toml crate can never make this brittle)...
        let s = toml::to_string_pretty(&c).expect("serialize");
        let back: Config = toml::from_str(&s).expect("re-deserialize");
        assert_eq!(back.window_mode, "fullscreen");
        assert_eq!(c, back);
        // ...and `sanitize_floats` (the single sanitize entry point) leaves it
        // alone — `window_mode` is a String, so there is nothing to clamp.
        let mut m = c.clone();
        m.sanitize_floats();
        assert_eq!(m.window_mode, "fullscreen");
        // The DEFAULT is still "center" — adding a value must not change it.
        assert_eq!(Config::default().window_mode, "center");
    }

    #[test]
    fn round_trip_through_toml() {
        let c = Config {
            theme: "dracula".to_string(),
            opacity: 0.85,
            font_size: 18.0,
            font_family: "Fira Code".to_string(),
            ui_font_family: "Inter".to_string(),
            ui_font_size: 20.0,
            corner_radius: 6.0,
            summon_effect: "phosphor".to_string(),
            window_mode: "dropdown".to_string(),
            dropdown_height_pct: 0.6,
            dropdown_width_pct: 1.0,
            focus_autohide: false,
            launch_at_login: false,
            summon_hotkey: "F12".to_string(),
            shell: "/usr/bin/zsh".to_string(),
            tab_bar_position: "bottom".to_string(),
            scrollback_lines: 25_000,
            show_welcome: false,
            show_perf_hud: false,
            effects: EffectsConfig::default(),
            notify_on_command_finish: false,
            notify_min_seconds: 30,
            notify_only_on_failure: true,
            auto_summon_on_finish: true,
            osc52_allow_paste: true,
            run_selection: false,
            hot_reload: false,
            keys: KeyBindings::default(),
        };
        let s = toml::to_string_pretty(&c).expect("serialize");
        let back: Config = toml::from_str(&s).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn round_trip_through_file() {
        let dir = std::env::temp_dir().join(format!("jetty-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let c = Config {
            theme: "tokyo_night".to_string(),
            opacity: 0.5,
            font_size: 14.0,
            font_family: "MesloLGS NF".to_string(),
            ui_font_family: String::new(),
            ui_font_size: 16.0,
            corner_radius: 12.0,
            summon_effect: "none".to_string(),
            window_mode: "center".to_string(),
            dropdown_height_pct: 0.5,
            dropdown_width_pct: 1.0,
            focus_autohide: true,
            launch_at_login: true,
            summon_hotkey: "F9".to_string(),
            shell: String::new(),
            tab_bar_position: "bottom".to_string(),
            scrollback_lines: 10_000,
            show_welcome: true,
            show_perf_hud: true,
            effects: EffectsConfig::default(),
            notify_on_command_finish: true,
            notify_min_seconds: 10,
            notify_only_on_failure: false,
            auto_summon_on_finish: false,
            osc52_allow_paste: false,
            run_selection: true,
            hot_reload: true,
            keys: KeyBindings::default(),
        };
        std::fs::write(&path, toml::to_string_pretty(&c).unwrap()).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opacity_floor_keeps_window_visible() {
        // App applies a [0.1, 1.0] clamp on load so a persisted 0.0 (invisible
        // window) is lifted to the visible floor. Mirror that clamp here to lock
        // in the contract the loader relies on.
        assert_eq!(0.0_f32.clamp(0.1, 1.0), 0.1);
        assert_eq!(0.5_f32.clamp(0.1, 1.0), 0.5);
        assert_eq!(2.0_f32.clamp(0.1, 1.0), 1.0);
    }

    #[test]
    fn missing_file_is_default() {
        // toml::from_str on garbage falls back to default via unwrap_or_default.
        let back: Config = toml::from_str("not valid toml !!!").unwrap_or_default();
        assert_eq!(back, Config::default());
    }

    #[test]
    fn effects_defaults_are_off_except_caret_flash() {
        let e = EffectsConfig::default();
        assert!(!e.crt_enabled);
        assert!(!e.crt_animate_roll && !e.crt_flicker && !e.crt_jitter);
        assert!(e.caret_flash_enabled);      // the one ON-by-default effect
        assert!(!e.caret_glow_enabled);
        assert_eq!(e.crt_scanline_tint, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn old_config_without_effects_table_loads_with_defaults() {
        // a config TOML predating the effects feature
        let toml = r#"theme = "default"
opacity = 1.0
font_size = 14.0
font_family = "monospace"
corner_radius = 8.0
"#;
        let cfg: Config = toml::from_str(toml).expect("must load");
        assert_eq!(cfg.effects, EffectsConfig::default());
    }

    #[test]
    fn old_config_without_notify_keys_loads_with_defaults() {
        // A config predating v0.15 must load with the Run & Notify defaults, so an
        // upgrade is transparent (notifications ON, 10s, all-commands, no summon).
        let toml = r#"theme = "default"
opacity = 1.0
font_size = 14.0
font_family = "monospace"
corner_radius = 8.0
"#;
        let cfg: Config = toml::from_str(toml).expect("must load");
        assert!(cfg.notify_on_command_finish, "notify defaults ON");
        assert_eq!(cfg.notify_min_seconds, 10);
        assert!(!cfg.notify_only_on_failure);
        assert!(!cfg.auto_summon_on_finish, "auto-summon defaults OFF");
    }

    #[test]
    fn old_config_without_v016_keys_loads_with_defaults() {
        // A config predating v0.16 must load with osc52_allow_paste = false (SECURE)
        // and hot_reload = true (idle-free watcher), so an upgrade is transparent.
        let toml = r#"theme = "default"
opacity = 1.0
font_size = 14.0
font_family = "monospace"
corner_radius = 8.0
"#;
        let cfg: Config = toml::from_str(toml).expect("must load");
        assert!(!cfg.osc52_allow_paste, "osc52 paste defaults OFF (secure)");
        assert!(cfg.hot_reload, "hot reload defaults ON");
    }

    #[test]
    fn notify_min_seconds_is_clamped() {
        // 0 → 1 (never let a 0 make every command "long"); a huge value → 1-day cap.
        let mut c = Config { notify_min_seconds: 0, ..Config::default() };
        c.sanitize_floats();
        assert_eq!(c.notify_min_seconds, 1);
        let mut c = Config { notify_min_seconds: 10_000_000, ..Config::default() };
        c.sanitize_floats();
        assert_eq!(c.notify_min_seconds, 86_400);
        // A sane hand-edited value passes through verbatim.
        let mut c = Config { notify_min_seconds: 45, ..Config::default() };
        c.sanitize_floats();
        assert_eq!(c.notify_min_seconds, 45);
    }

    #[test]
    fn effects_clamp_out_of_range() {
        let e = EffectsConfig { crt_curvature: 9.0, crt_bloom: -1.0, caret_flash_ms: 5000.0, ..Default::default() }.clamped();
        assert!(e.crt_curvature <= 1.0 && e.crt_bloom >= 0.0);
        assert!(e.caret_flash_ms <= 400.0);
    }

    #[test]
    fn nan_floats_fall_back_to_defaults() {
        // TOML 1.x parses a literal `nan`, and f32::clamp propagates NaN — so
        // sanitize_floats must replace every non-finite float with its default.
        let toml = "theme = \"dracula\"\nopacity = nan\nfont_size = nan\n\
                    font_family = \"MesloLGS NF\"\ncorner_radius = nan\n\
                    ui_font_size = inf\ndropdown_height_pct = -inf\n";
        let mut cfg: Config = toml::from_str(toml).expect("nan parses in toml 1.x");
        assert!(cfg.opacity.is_nan(), "premise: toml yields NaN");
        cfg.sanitize_floats();
        assert_eq!(cfg.opacity, 1.0);
        assert_eq!(cfg.font_size, 16.0);
        assert_eq!(cfg.corner_radius, 10.0);
        assert_eq!(cfg.ui_font_size, 16.0);
        assert_eq!(cfg.dropdown_height_pct, 0.50);
        assert_eq!(cfg.dropdown_width_pct, 1.0);
    }

    #[test]
    fn sanitize_keeps_finite_values_untouched() {
        let mut cfg = Config {
            opacity: 0.42,
            font_size: 13.0,
            corner_radius: 3.0,
            ..Config::default()
        };
        cfg.sanitize_floats();
        assert_eq!(cfg.opacity, 0.42);
        assert_eq!(cfg.font_size, 13.0);
        assert_eq!(cfg.corner_radius, 3.0);
    }

    #[test]
    fn scrollback_lines_clamped_by_sanitize() {
        // Too small clamps up to the floor.
        let mut low = Config { scrollback_lines: 5, ..Config::default() };
        low.sanitize_floats();
        assert_eq!(low.scrollback_lines, 100);

        // Too large clamps down to alacritty's own UI max.
        let mut high = Config { scrollback_lines: 1_000_000, ..Config::default() };
        high.sanitize_floats();
        assert_eq!(high.scrollback_lines, 100_000);

        // In-range hand-edited values are kept verbatim (no step snapping).
        let mut mid = Config { scrollback_lines: 12_345, ..Config::default() };
        mid.sanitize_floats();
        assert_eq!(mid.scrollback_lines, 12_345);

        // The default roundtrips through TOML unchanged.
        let s = toml::to_string(&Config::default()).expect("serialize");
        let back: Config = toml::from_str(&s).expect("parse");
        assert_eq!(back.scrollback_lines, 10_000);
    }

    #[test]
    fn effects_clamp_replaces_nan_with_defaults() {
        let e = EffectsConfig {
            crt_curvature: f32::NAN,
            crt_bloom: f32::INFINITY,
            caret_flash_ms: f32::NAN,
            crt_scanline_tint: [f32::NAN, 0.5, 1.0],
            ..Default::default()
        }
        .clamped();
        assert_eq!(e.crt_curvature, ef_curvature());
        assert_eq!(e.crt_bloom, ef_bloom(), "non-finite (inf) falls back to the default");
        assert_eq!(e.caret_flash_ms, ef_flash_ms());
        assert_eq!(e.crt_scanline_tint, [1.0, 0.5, 1.0]);
    }

    #[test]
    fn atomic_write_replaces_content_and_cleans_temp() {
        let dir = std::env::temp_dir().join(format!("jetty-atomic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new contents");
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp file must be renamed away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_creates_fresh_file() {
        let dir = std::env::temp_dir().join(format!("jetty-atomic-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        write_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_config_without_keys_table_loads_with_defaults() {
        // A config predating v0.20 must load with an empty (all-default) [keys].
        let toml = r#"theme = "default"
opacity = 1.0
font_size = 14.0
font_family = "monospace"
corner_radius = 8.0
"#;
        let cfg: Config = toml::from_str(toml).expect("must load");
        assert_eq!(cfg.keys, KeyBindings::default());
        assert!(cfg.keys.is_empty());
    }

    #[test]
    fn keys_table_parses_string_and_array_forms() {
        let toml = r#"theme = "default"
opacity = 1.0
font_size = 14.0
font_family = "monospace"
corner_radius = 8.0

[keys]
new_tab = "Ctrl+T"
paste = ["Ctrl+Shift+V", "Shift+Insert"]
select_all = ""
"#;
        let cfg: Config = toml::from_str(toml).expect("must load");
        assert_eq!(cfg.keys.new_tab, Some(ChordSpec::One("Ctrl+T".to_string())));
        assert_eq!(
            cfg.keys.paste,
            Some(ChordSpec::Many(vec!["Ctrl+Shift+V".to_string(), "Shift+Insert".to_string()]))
        );
        assert_eq!(cfg.keys.select_all, Some(ChordSpec::One(String::new())));
        assert!(!cfg.keys.is_empty());
    }

    #[test]
    fn empty_keys_table_not_serialized() {
        // A default install must not write a bare [keys] header.
        let s = toml::to_string_pretty(&Config::default()).expect("serialize");
        assert!(!s.contains("[keys]"), "empty [keys] must be skipped, got:\n{s}");
    }

    #[test]
    fn bad_keys_value_keeps_the_live_binding_on_reload() {
        // A wrong TYPE (integer) fails the untagged ChordSpec. On a reload the bad
        // key keeps the LIVE value (base) while the rest of the file still applies.
        let toml = r#"theme = "dracula"
opacity = 0.7

[keys]
new_tab = 42
copy = "Ctrl+Shift+Y"
"#;
        let live = Config {
            keys: KeyBindings {
                new_tab: Some(ChordSpec::One("Ctrl+T".to_string())),
                ..KeyBindings::default()
            },
            ..Config::default()
        };
        let (cfg, warnings) =
            Config::parse_with_base(toml, &live, "keeping the current value").expect("valid TOML");
        assert_eq!(cfg.theme, "dracula");
        assert_eq!(cfg.opacity, 0.7);
        assert_eq!(cfg.keys.new_tab, Some(ChordSpec::One("Ctrl+T".to_string())), "live value kept");
        assert_eq!(cfg.keys.copy, Some(ChordSpec::One("Ctrl+Shift+Y".to_string())));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("keys.new_tab = 42"), "{warnings:?}");
        assert!(warnings[0].contains("chord string"), "friendly hint: {warnings:?}");
        assert!(warnings[0].contains("keeping the current value"), "{warnings:?}");
    }

    // ── per-key loading (one bad value never resets the whole config) ────────

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jetty-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn one_wrong_typed_value_falls_back_alone() {
        // Each of these used to send the WHOLE file to `.bad` and load defaults.
        for (bad, key) in [
            ("scrollback_lines = 20000.0", "scrollback_lines"),
            ("opacity = \"0.9\"", "opacity"),
            ("focus_autohide = \"false\"", "focus_autohide"),
            ("[effects]\ncrt_enabled = 1", "effects.crt_enabled"),
        ] {
            let src = format!("theme = \"nord\"\nfont_size = 13.0\n{bad}\n");
            let (cfg, warnings) =
                Config::parse_with_base(&src, &Config::default(), "using the default").unwrap();
            assert_eq!(cfg.theme, "nord", "{bad}: the other keys still apply");
            assert_eq!(cfg.font_size, 13.0, "{bad}");
            assert_eq!(warnings.len(), 1, "{bad}: {warnings:?}");
            assert!(warnings[0].contains(key), "{bad}: {warnings:?}");
            assert!(warnings[0].contains("using the default"), "{bad}: {warnings:?}");
        }
        // The fallback really is the default for the bad key.
        let (cfg, _) = Config::parse_with_base("opacity = \"0.9\"\n", &Config::default(), "x").unwrap();
        assert_eq!(cfg.opacity, Config::default().opacity);
        // An integer for a float field is fine (TOML ints widen), no warning.
        let (cfg, w) = Config::parse_with_base("opacity = 1\n", &Config::default(), "x").unwrap();
        assert_eq!(cfg.opacity, 1.0);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn friendly_type_hints() {
        let (_, w) = Config::parse_with_base("opacity = \"0.9\"\n", &Config::default(), "x").unwrap();
        assert!(w[0].contains("expected a number"), "{w:?}");
        let (_, w) = Config::parse_with_base("focus_autohide = 1\n", &Config::default(), "x").unwrap();
        assert!(w[0].contains("true or false"), "{w:?}");
        let (_, w) = Config::parse_with_base("theme = 3\n", &Config::default(), "x").unwrap();
        assert!(w[0].contains("quoted string"), "{w:?}");
    }

    #[test]
    fn a_table_where_a_value_belongs_is_one_invalid_key() {
        let (cfg, w) =
            Config::parse_with_base("opacity = { a = 1 }\ntheme = \"nord\"\n", &Config::default(), "x")
                .unwrap();
        assert_eq!(cfg.theme, "nord");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("`opacity = "), "{w:?}");
    }

    #[test]
    fn unknown_keys_are_reported_not_fatal() {
        let src = "fontsize = 18\ntheme = \"nord\"\n[keys]\nnew_tabb = \"Ctrl+T\"\n[effects]\ncrt = true\n";
        let (cfg, w) = Config::parse_with_base(src, &Config::default(), "x").unwrap();
        assert_eq!(cfg.theme, "nord");
        let joined = w.join("\n");
        for k in ["`fontsize`", "`keys.new_tabb`", "`effects.crt`"] {
            assert!(joined.contains(k), "{k} must be reported: {joined}");
        }
        assert_eq!(w.len(), 3, "{joined}");
        // Known keys (including [keys] overrides) are never "unknown".
        let (_, w) = Config::parse_with_base(
            "theme = \"nord\"\n[keys]\nnew_tab = \"Ctrl+T\"\npaste = []\n",
            &Config::default(),
            "x",
        )
        .unwrap();
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn syntax_error_loads_defaults_copies_aside_and_never_moves_the_file() {
        let dir = tmp_dir("syntax");
        let path = dir.join("config.toml");
        let text = "theme = \"nord\"\nopacity = \n";
        std::fs::write(&path, text).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.cfg, Config::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("line 2"), "{:?}", loaded.warnings);
        // The original is untouched, a byte-identical copy sits beside it.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let copies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("config.toml.bad-"))
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read_to_string(copies[0].path()).unwrap(), text);
        // Loading the same broken file again reuses that copy (no pile-up).
        let _ = Config::load_from(&path);
        let n = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("config.toml.bad-"))
            .count();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn syntax_error_keeps_a_symlinked_config_linked() {
        use std::os::unix::fs::symlink;
        let dir = tmp_dir("symlink-bad");
        let target = dir.join("dotfiles-jetty.toml");
        std::fs::write(&target, "opacity = = 1\n").unwrap();
        let link = dir.join("config.toml");
        symlink(&target, &link).unwrap();
        let _ = Config::load_from(&link);
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "opacity = = 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_loads_defaults_silently() {
        let dir = tmp_dir("missing");
        let loaded = Config::load_from(&dir.join("config.toml"));
        assert_eq!(loaded.cfg, Config::default());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.hash, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_ascii_garbage_never_panics() {
        for src in ["opacity = \"ğüş\"\n", "theme = \"çay\"\n", "ş = 1\n", "[keys]\nnew_tab = \"Ctrl+ş\"\n"] {
            let _ = Config::parse_with_base(src, &Config::default(), "x");
        }
        // A raw NUL inside a string is a TOML syntax error — reported, not a panic.
        assert!(Config::parse_with_base("opacity = \"\u{0}\"\n", &Config::default(), "x").is_err());
    }

    // ── in-place persistence ─────────────────────────────────────────────────

    const HAND_WRITTEN: &str = "# My JeTTY setup — hand-tuned.\n\
theme = \"nord\"   # the cool one\n\
opacity = \"0.9\"  # a typo the user hasn't fixed yet\n\
fontsize = 18     # an unknown key\n\
font_size = 15.0\n\
\n\
[effects]\n\
# glow is nice\n\
caret_glow_enabled = true\n";

    fn persister_for(path: &Path) -> (Persister, Config, Arc<Mutex<Vec<String>>>) {
        let loaded = Config::load_from(path);
        let notices = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        let p = Persister::new(
            path.to_path_buf(),
            loaded.cfg.clone(),
            loaded.hash,
            Box::new(move |m| sink.lock().unwrap().push(m)),
        );
        (p, loaded.cfg, notices)
    }

    #[test]
    fn save_touches_only_changed_keys_and_keeps_comments_and_unknown_keys() {
        let dir = tmp_dir("inplace");
        let path = dir.join("config.toml");
        std::fs::write(&path, HAND_WRITTEN).unwrap();
        let (mut p, mut cfg, notices) = persister_for(&path);
        cfg.font_size = 17.0;
        cfg.effects.crt_enabled = true;
        assert!(p.record(&cfg, Instant::now()));
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# My JeTTY setup — hand-tuned."), "{out}");
        assert!(out.contains("theme = \"nord\"   # the cool one"), "{out}");
        // The invalid value the user wrote is NOT replaced by the in-memory default:
        // it didn't change, so it isn't touched.
        assert!(out.contains("opacity = \"0.9\"  # a typo"), "{out}");
        assert!(out.contains("fontsize = 18     # an unknown key"), "{out}");
        assert!(out.contains("font_size = 17.0"), "{out}");
        assert!(out.contains("# glow is nice\ncaret_glow_enabled = true"), "{out}");
        assert!(out.contains("crt_enabled = true"), "{out}");
        assert!(!out.contains("crt_curvature"), "untouched defaults are not written: {out}");
        assert!(notices.lock().unwrap().is_empty());
        // Our own write is recognized (its watcher echo is skipped)…
        assert!(p.is_self_write(hash_str(&out)));
        // …and an unchanged record is a no-op.
        assert!(!p.record(&cfg, Instant::now()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_keeps_float_text_short_and_value_comments() {
        let dir = tmp_dir("floats");
        let path = dir.join("config.toml");
        std::fs::write(&path, "opacity = 1.0 # full\n").unwrap();
        let (mut p, mut cfg, _) = persister_for(&path);
        cfg.opacity = 0.85;
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "opacity = 0.85 # full\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_merges_an_external_edit_instead_of_clobbering_it() {
        let dir = tmp_dir("external");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\nfont_size = 15.0\n").unwrap();
        let (mut p, mut cfg, _) = persister_for(&path);
        // The user edits the file in an editor…
        std::fs::write(&path, "theme = \"dracula\"\nfont_size = 15.0\n").unwrap();
        // …while a Settings change is pending in the app.
        cfg.font_size = 20.0;
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("theme = \"dracula\""), "external edit survives: {out}");
        assert!(out.contains("font_size = 20.0"), "{out}");
        // NOT marked as our own write, so the hot-reload applies the merged file.
        assert!(!p.is_self_write(hash_str(&out)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_to_overwrite_a_file_that_is_not_toml() {
        let dir = tmp_dir("refuse");
        let path = dir.join("config.toml");
        let broken = "theme = \"nord\"\nopacity = = 1\n";
        std::fs::write(&path, broken).unwrap();
        let (mut p, mut cfg, notices) = persister_for(&path);
        cfg.font_size = 22.0;
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "never replaced wholesale");
        let n = notices.lock().unwrap();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].contains("line 2") && n[0].contains("not saved"), "{n:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_save_without_a_file_writes_every_setting() {
        let dir = tmp_dir("fresh");
        let path = dir.join("sub").join("config.toml");
        let (mut p, mut cfg, _) = persister_for(&path);
        cfg.theme = "nord".to_string();
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        let back = Config::load_from(&path);
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(back.cfg, cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resetting_a_keybinding_removes_it_and_an_emptied_keys_table() {
        let dir = tmp_dir("keys-reset");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\n\n[keys]\nnew_tab = \"Ctrl+T\"\n").unwrap();
        let (mut p, mut cfg, _) = persister_for(&path);
        assert!(cfg.keys.new_tab.is_some());
        cfg.keys = KeyBindings::default();
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("new_tab"), "{out}");
        assert!(!out.contains("[keys]"), "an emptied [keys] table goes too: {out}");
        assert!(out.contains("theme = \"nord\""), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_a_keybinding_creates_the_keys_table() {
        let dir = tmp_dir("keys-add");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\n").unwrap();
        let (mut p, mut cfg, _) = persister_for(&path);
        cfg.keys.new_tab = Some(ChordSpec::One("Ctrl+T".to_string()));
        p.record(&cfg, Instant::now());
        p.flush_and_wait(Duration::from_secs(5));
        let back = Config::load_from(&path);
        assert_eq!(back.cfg.keys.new_tab, Some(ChordSpec::One("Ctrl+T".to_string())));
        assert!(std::fs::read_to_string(&path).unwrap().contains("[keys]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debounce_schedules_one_save_and_busy_tracks_it() {
        let dir = tmp_dir("debounce");
        let path = dir.join("config.toml");
        let (mut p, mut cfg, _) = persister_for(&path);
        let t0 = Instant::now();
        assert_eq!(p.due_at(), None);
        assert!(!p.busy());
        for i in 0..30 {
            cfg.opacity = 0.5 + i as f32 / 100.0;
            p.record(&cfg, t0);
        }
        assert_eq!(p.due_at(), Some(t0 + SAVE_DEBOUNCE));
        assert!(p.busy());
        assert_eq!(p.pending.len(), 1, "a held key coalesces to one pending change");
        p.flush_and_wait(Duration::from_secs(5));
        assert!(!p.busy());
        assert_eq!(p.due_at(), None);
        assert!((Config::load_from(&path).cfg.opacity - 0.79).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_copies_the_current_file() {
        let dir = tmp_dir("backup");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\n[keys]\ncopy = \"Ctrl+C\"\n").unwrap();
        let (mut p, _, _) = persister_for(&path);
        let bak = p.backup().unwrap().expect("a file to back up");
        assert!(bak.file_name().unwrap().to_string_lossy().starts_with("config.toml.bak-"));
        assert_eq!(std::fs::read(&bak).unwrap(), std::fs::read(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_is_minimal() {
        let a = Config::default();
        let mut b = a.clone();
        b.opacity = 0.5;
        b.effects.crt_bloom = 0.1;
        let d = diff_configs(&a, &b);
        let mut paths: Vec<String> = d.iter().map(|c| c.path.join(".")).collect();
        paths.sort();
        assert_eq!(paths, vec!["effects.crt_bloom".to_string(), "opacity".to_string()]);
    }

    #[test]
    fn effects_config_roundtrips_through_toml() {
        let e = EffectsConfig {
            crt_enabled: true,
            crt_curvature: 0.42,
            crt_flicker: true,
            caret_flash_color: [0.1, 0.2, 0.3],
            ..EffectsConfig::default()
        };
        let cfg = Config { effects: e.clone(), ..Config::default() };
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.effects, e);
    }
}
