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

/// Checks that make problems visible: unknown keys and invalid values with the
/// closest valid spelling, enum values read by the app's own parsers, ranges,
/// fonts that are not installed.
pub(crate) mod check;

/// The persisted user settings. Field names are the TOML keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Theme preset name (must match a `jetty_core::theme::PRESETS` entry).
    #[serde(default = "default_theme")]
    pub theme: String,
    // ── System appearance & theme UX (visuals v2) ────────────────────────────
    /// Follow the desktop's light/dark preference — the freedesktop settings
    /// portal on Linux/BSD (any desktop that runs one), the system appearance on
    /// macOS: `light_theme` while it prefers light (or states no preference),
    /// `theme` while it prefers dark. Default OFF. Without a portal (a bare
    /// window manager) `theme` stays. Hot-reloadable.
    #[serde(default)]
    pub follow_system_theme: bool,
    /// The theme shown while `follow_system_theme` is on and the system is
    /// light; `theme` is the dark (and default) one. While the light one is on
    /// screen, a theme picked in the palette or Settings is saved here.
    #[serde(default = "default_light_theme")]
    pub light_theme: String,
    /// Minimum WCAG contrast between every glyph and its cell background,
    /// 1.0..=21.0. `1.0` (default) = off; `4.5` = WCAG AA, `3.0` = large text.
    /// Text below it is pushed toward white or black, keeping its hue —
    /// concealed text and powerline / block / sextant glyphs never change.
    #[serde(default = "default_minimum_contrast")]
    pub minimum_contrast: f32,
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
    /// Draw box drawing (─│╭╮), block elements (▀▄█░), Powerline separators
    /// (), braille (⣿) and sextants as built-in glyphs sized to the exact
    /// cell, so borders, prompts and graphs join seamlessly at any font size.
    /// Default `true`; `false` takes them from the font like any other char.
    #[serde(default = "default_builtin_glyphs")]
    pub builtin_glyphs: bool,
    /// Draw emoji (😀 ✅ and VS16 sequences like ❤️) in color from the
    /// installed emoji font (the first family named "…Emoji…"), two cells wide.
    /// Default `true`. Text-style symbols (✔ ❤ ☐) stay text.
    #[serde(default = "default_color_emoji")]
    pub color_emoji: bool,
    /// Bold text in one of the 8 normal ANSI colors renders in its bright twin
    /// (the classic xterm look some color schemes assume). Default `false`.
    #[serde(default)]
    pub bold_is_bright: bool,
    /// Window corner radius in logical px (0..=24).
    #[serde(default = "default_corner_radius")]
    pub corner_radius: f32,
    /// Inner padding between the window edges and the terminal grid, in
    /// logical px (scaled by the display's DPI), left and right. The scrollbar
    /// lives in the right padding, so the right side is at least the scrollbar
    /// gutter. Clamped to 0..=64; `0` = no padding (the pre-0.27 look).
    #[serde(default = "default_padding_x")]
    pub padding_x: f32,
    /// Inner padding above and below the grid, in logical px (0..=64).
    #[serde(default = "default_padding_y")]
    pub padding_y: f32,
    /// Terminal line height as a multiple of the font size (1.0..=2.0;
    /// default 1.3, the long-standing spacing). Each glyph is centred in its
    /// taller row; backgrounds, selection and the cursor fill the whole row.
    #[serde(default = "default_line_height")]
    pub line_height: f32,
    /// When the scrollbar shows: `"always"` (default), `"auto"` (while
    /// scrolled back, dragging it or hovering its gutter) or `"never"` (no
    /// thumb and no gutter — the grid gets the full width). See
    /// [`ScrollbarMode`].
    #[serde(default)]
    pub scrollbar: ScrollbarMode,
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
    /// `.desktop` file under `~/.config/autostart/`; a LaunchAgent on macOS).
    /// Default OFF. `true` writes the entry (at startup too); the toggle, or an
    /// EDIT of the key while JeTTY runs, writes or removes it. Startup never
    /// removes it: without the key, with a config that failed to load, or with a
    /// `false` (maybe a stale one — older saves wrote every key) the app mirrors
    /// the entry instead. With `JETTY_CONFIG_DIR` set the real login item is not
    /// touched.
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
    // ── Chrome (visuals v2) ───────────────────────────────────────────────────
    /// Tab look: "pill" (default — a soft rounded pill behind the active tab),
    /// "underline" (an accent bar under the active title), "slant" (slanted
    /// tabs), "powerline" (breadcrumb chevrons) or "compact" (the pill look on
    /// narrower tabs). Unknown values read as "pill".
    #[serde(default = "default_tab_style")]
    pub tab_style: String,
    /// Which tabs show their "×": "always" (default), "hover" (only the tab
    /// under the pointer) or "active" (the active tab and the hovered one).
    #[serde(default = "default_tab_close_button")]
    pub tab_close_button: String,
    /// `true` lets the tab bar follow `opacity` like the terminal area (and
    /// show what is behind the window); `false` (default) keeps it opaque.
    #[serde(default = "default_tab_bar_opacity")]
    pub tab_bar_opacity: bool,
    /// Draw OSC 9;4 progress (cargo with `CARGO_TERM_PROGRESS_TERM_INTEGRATION=
    /// true`, Claude Code, winget…) in the tab and on the bar's grid edge.
    /// Default `true`.
    #[serde(default = "default_progress_bar")]
    pub progress_bar: bool,
    /// A thin ring around the window: "none" (default), "focus" (while the
    /// window has keyboard focus) or "always" (muted while unfocused). Drawn in
    /// the accent, or the active tab's color.
    #[serde(default = "default_window_border")]
    pub window_border: String,
    /// Tab titles: "osc" (default — the program's title, else "Tab N") or
    /// "auto" (the program's title, else the running command or the shell's
    /// directory, via shell integration). A manual rename always wins.
    #[serde(default = "default_tab_title")]
    pub tab_title: String,
    /// Visual effects (CRT, scanlines, caret). See `EffectsConfig`. Backward
    /// compatible: old configs without `[effects]` load with all defaults.
    #[serde(default)]
    pub effects: EffectsConfig,
    /// The background layer under the grid (`[backdrop]` table, visuals v2):
    /// a curated per-theme look, a gradient, an image or a pattern. See
    /// `BackdropConfig`. `mode = "none"` (the default) draws and builds nothing.
    #[serde(default)]
    pub backdrop: BackdropConfig,
    /// The shell cursor's look and motion (`[cursor]`: shape, thickness,
    /// unfocused look, color, row guide, trail). See `CursorConfig`; an old
    /// config without the table loads today's cursor.
    #[serde(default)]
    pub cursor: CursorConfig,
    /// Calm the motion down: `"off"` (default), `"on"`, or `"system"` (follow
    /// the desktop's reduced-motion setting, via the freedesktop settings
    /// portal / the macOS accessibility flag). While active the summon reveal
    /// becomes a short fade, the dropdown does not slide, CRT roll / flicker /
    /// jitter and the caret ripple stop, the cursor trail is off, and the
    /// visual bell is the rim.
    #[serde(default = "default_reduce_motion")]
    pub reduce_motion: String,
    /// Visual bell (BEL in the active tab): `"off"` (default), `"flash"` (a
    /// 150 ms flash of the whole window) or `"rim"` (a 150 ms glow along the
    /// window edge). At most one every 333 ms; an unfocused window asks for
    /// attention instead.
    #[serde(default = "default_visual_bell")]
    pub visual_bell: String,
    /// Command status pulse (needs OSC 133 shell integration): `"off"`
    /// (default), `"failures"` (a ~0.4 s red rim pulse when a command in the
    /// active tab fails) or `"all"` (also an accent pulse when a long command
    /// succeeds). A command that finished while JeTTY was hidden pulses on the
    /// next summon.
    #[serde(default = "default_command_pulse")]
    pub command_pulse: String,
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
    /// macOS: which Option key(s) act as Meta (ESC-prefix, like Alt elsewhere)
    /// instead of composing characters — `"none"` (default: Option types
    /// `@ { } [ ] | ~` on non-US layouts), `"left"`, `"right"` or `"both"`.
    /// Ignored on other platforms. Unknown values read as `"none"`.
    #[serde(default)]
    pub macos_option_as_alt: crate::input::OptionAsAlt,
    /// Where a finished mouse selection is copied: `"primary"` (default — the
    /// X11/Wayland select-to-copy convention; a middle click pastes it and the
    /// clipboard is left alone), `"clipboard"` (a middle click in JeTTY then
    /// pastes the clipboard too), `"both"` or `"off"`. macOS and Windows have no
    /// primary selection: there `"primary"` means the clipboard.
    #[serde(default)]
    pub copy_on_select: crate::clipboard::CopyOnSelect,
    /// The kitty keyboard protocol: a program that asks for it (`CSI > u`) gets
    /// unambiguous key reports (Ctrl+I ≠ Tab, key releases, …). Default `true`;
    /// `false` turns it off in every tab, so programs see a terminal without it
    /// and use legacy keys. Hot-reloadable (a change resets the flags programs
    /// pushed).
    #[serde(default = "default_kitty_keyboard")]
    pub kitty_keyboard: bool,
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
    /// Host scrollback paging, default Shift+PageUp / Shift+PageDown (plain Page
    /// keys reach the program). Pre-v0.26 behaviour:
    /// `scroll_page_up = ["Shift+PageUp", "PageUp"]` (+ the `_down` twin).
    #[serde(default, skip_serializing_if = "Option::is_none")] pub scroll_page_up: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub scroll_page_down: Option<ChordSpec>,
    /// Step to the next / previous theme. No default chord (palette:
    /// "Next theme" / "Previous theme"); e.g. `next_theme = "Ctrl+Alt+T"`.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub next_theme: Option<ChordSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub prev_theme: Option<ChordSpec>,
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
fn default_builtin_glyphs() -> bool {
    true
}
fn default_color_emoji() -> bool {
    true
}
fn default_hot_reload() -> bool {
    true
}
fn default_kitty_keyboard() -> bool {
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

fn default_light_theme() -> String {
    "catppuccin_latte".to_string()
}

fn default_minimum_contrast() -> f32 {
    1.0
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

fn default_padding_x() -> f32 {
    8.0
}

fn default_padding_y() -> f32 {
    4.0
}

fn default_line_height() -> f32 {
    jetty_render::LINE_HEIGHT_DEFAULT
}

/// When the scrollbar thumb shows (config key `scrollbar`). Every mode only
/// ever draws a thumb when there IS history to scroll.
/// * `"always"` (default, the long-standing behaviour) — always;
/// * `"auto"` — while the view is scrolled back into history, while the thumb
///   is being dragged, or while the pointer is over the scrollbar gutter. No
///   fade timer: a change costs exactly one repaint, idle stays idle;
/// * `"never"` — no thumb and no gutter: the grid gets the full width and the
///   right edge belongs to the grid (the wheel still scrolls).
///
/// Unknown values read as `"always"`; a bool reads as always/never.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollbarMode {
    #[default]
    Always,
    Auto,
    Never,
}

impl ScrollbarMode {
    /// The config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ScrollbarMode::Always => "always",
            ScrollbarMode::Auto => "auto",
            ScrollbarMode::Never => "never",
        }
    }

    /// Lenient parse (case-insensitive); anything unknown is the default.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => ScrollbarMode::Auto,
            "never" | "off" | "none" | "hidden" => ScrollbarMode::Never,
            _ => ScrollbarMode::Always,
        }
    }

    /// Whether the grid reserves the scrollbar gutter — and presses and the
    /// wheel over the thumb column belong to the scrollbar.
    pub fn has_gutter(self) -> bool {
        self != ScrollbarMode::Never
    }

    /// Whether the thumb is drawn right now (given there is history):
    /// `scrolled_back` = the view is above the live bottom, `dragging` = the
    /// thumb is held, `hovering` = the pointer is over the gutter.
    pub fn shows_thumb(self, scrolled_back: bool, dragging: bool, hovering: bool) -> bool {
        match self {
            ScrollbarMode::Always => true,
            ScrollbarMode::Auto => scrolled_back || dragging || hovering,
            ScrollbarMode::Never => false,
        }
    }
}

impl Serialize for ScrollbarMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScrollbarMode {
    /// The string spellings and, leniently, a bool (`true` = always,
    /// `false` = never).
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Bool(bool),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Str(s) => ScrollbarMode::parse(&s),
            Raw::Bool(true) => ScrollbarMode::Always,
            Raw::Bool(false) => ScrollbarMode::Never,
        })
    }
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

fn default_tab_style() -> String {
    "pill".to_string()
}

fn default_tab_close_button() -> String {
    "always".to_string()
}

fn default_tab_bar_opacity() -> bool {
    false
}

fn default_progress_bar() -> bool {
    true
}

fn default_window_border() -> String {
    "none".to_string()
}

fn default_tab_title() -> String {
    "osc".to_string()
}

fn default_reduce_motion() -> String {
    "off".to_string()
}

fn default_visual_bell() -> String {
    "off".to_string()
}

fn default_command_pulse() -> String {
    "off".to_string()
}

/// `[cursor]` — the shell cursor. Every key is `#[serde(default)]`, so a config
/// without the table (or missing any key) loads today's cursor; unknown words
/// read as the default (see `crate::motion`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorConfig {
    /// `"block"` (default), `"beam"`, `"underline"`, `"double_underline"` or
    /// `"thick_underline"` (the "vintage" tall bar). The shape a fresh screen
    /// shows and programs reset to (`CSI 0 SP q`); a program's own DECSCUSR
    /// still wins until it resets. The underline variants also style any
    /// underline cursor a program asks for.
    #[serde(default = "cur_shape")]
    pub shape: String,
    /// Beam / underline stroke as a fraction of the cell (0.04 ..= 0.5,
    /// default 0.12 — the classic look).
    #[serde(default = "cur_thickness")]
    pub thickness: f32,
    /// The cursor of an unfocused window: `"hollow"` (default — a block becomes
    /// its outline), `"unchanged"` or `"none"`.
    #[serde(default = "cur_unfocused")]
    pub unfocused: String,
    /// `"theme"` (default — the theme's cursor color), `"cell"` (reverse video
    /// of the cell under it) or `"auto"` (the theme color, reverse video where
    /// it would be under 2:1 on its cell).
    #[serde(default = "cur_color")]
    pub color: String,
    /// A faint band across the cursor's row: `"off"` (default), `"shell"` (not
    /// in full-screen programs — the alternate screen) or `"always"`.
    #[serde(default = "cur_guide")]
    pub guide: String,
    /// Cursor trail: when the cursor jumps more than `trail_threshold` cells it
    /// leaves a short smear that catches up with it over `trail_ms` (kitty's
    /// model).
    #[serde(default)]
    pub trail: bool,
    /// How long the trail takes to catch up (60 ..= 1000 ms, default 200).
    #[serde(default = "cur_trail_ms")]
    pub trail_ms: u32,
    /// A jump must cover MORE than this many cells (rows + columns) to leave a
    /// trail — kitty's rule (1 ..= 40, default 2: typing, even a fast burst,
    /// never trails).
    #[serde(default = "cur_trail_threshold")]
    pub trail_threshold: u32,
}

fn cur_shape() -> String {
    "block".to_string()
}
fn cur_thickness() -> f32 {
    0.12
}
fn cur_unfocused() -> String {
    "hollow".to_string()
}
fn cur_color() -> String {
    "theme".to_string()
}
fn cur_guide() -> String {
    "off".to_string()
}
fn cur_trail_ms() -> u32 {
    200
}
fn cur_trail_threshold() -> u32 {
    2
}

impl Default for CursorConfig {
    fn default() -> Self {
        CursorConfig {
            shape: cur_shape(),
            thickness: cur_thickness(),
            unfocused: cur_unfocused(),
            color: cur_color(),
            guide: cur_guide(),
            trail: false,
            trail_ms: cur_trail_ms(),
            trail_threshold: cur_trail_threshold(),
        }
    }
}

impl CursorConfig {
    /// Clamp the numbers into range (non-finite → default). Called on load.
    pub fn clamped(mut self) -> Self {
        self.thickness = finite_or(self.thickness, cur_thickness()).clamp(0.04, 0.5);
        self.trail_ms = self.trail_ms.clamp(60, 1000);
        self.trail_threshold = self.trail_threshold.clamp(1, 40);
        self
    }
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
    // ── Post-processing v2 ───────────────────────────────────────────────────
    /// Bloom spread 0..1 (quarter-resolution blur width). The default keeps the
    /// pre-v2 tight glow.
    #[serde(default = "ef_bloom_radius")] pub crt_bloom_radius: f32,
    /// Monochrome display color: "off" (full color), "amber", "green",
    /// "white", "blue", "paper" (dark ink on light paper) or "custom"
    /// (`crt_phosphor_color` on black).
    #[serde(default)] pub crt_phosphor: PhosphorMode,
    /// The `custom` phosphor color (rgb 0..1).
    #[serde(default = "ef_phosphor_color")] pub crt_phosphor_color: [f32; 3],
    /// How much of the text's own hue the phosphor keeps (0 = pure mono, 1 = full).
    #[serde(default = "ef_zero")] pub crt_phosphor_hue: f32,
    /// Film grain 0..1 (static unless `crt_grain_animate`).
    #[serde(default = "ef_zero")] pub crt_grain: f32,
    /// Re-roll the grain at ≤30 fps (an animation: it repaints continuously).
    #[serde(default = "ef_false")] pub crt_grain_animate: bool,
    /// 1-bit ordered dither (8×8 Bayer, DPI-scaled): the e-ink look.
    #[serde(default = "ef_false")] pub crt_dither: bool,
    /// A 200 ms color-split/tear glitch on a failed command (OSC 133) …
    #[serde(default = "ef_false")] pub glitch_on_error: bool,
    /// … and on the terminal bell. At most one per second.
    #[serde(default = "ef_false")] pub glitch_on_bell: bool,
    /// Keep CRT animations (roll/flicker/jitter, animated grain) running while
    /// the window is unfocused. Default off: an unfocused window is static (0% CPU).
    #[serde(default = "ef_false")] pub animate_unfocused: bool,
}

/// The CRT phosphor color mode (`crt_phosphor`). Unknown values fall back to
/// "off" with a warning (the per-key config loader).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhosphorMode {
    #[default]
    Off,
    Amber,
    Green,
    White,
    Blue,
    Paper,
    Custom,
}

impl PhosphorMode {
    /// Every mode, in Settings/palette order.
    pub const ALL: [PhosphorMode; 7] = [
        PhosphorMode::Off,
        PhosphorMode::Amber,
        PhosphorMode::Green,
        PhosphorMode::White,
        PhosphorMode::Blue,
        PhosphorMode::Paper,
        PhosphorMode::Custom,
    ];

    /// The human-facing label.
    pub fn display_name(self) -> &'static str {
        match self {
            PhosphorMode::Off => "Off",
            PhosphorMode::Amber => "Amber",
            PhosphorMode::Green => "Green",
            PhosphorMode::White => "White",
            PhosphorMode::Blue => "Blue",
            PhosphorMode::Paper => "Paper",
            PhosphorMode::Custom => "Custom",
        }
    }

    /// The mode named `name` (its config value, case-insensitive).
    pub fn from_name(name: &str) -> Option<PhosphorMode> {
        Self::ALL.into_iter().find(|m| m.display_name().eq_ignore_ascii_case(name.trim()))
    }

    /// The next mode (Settings cycler).
    pub fn next(self) -> PhosphorMode {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// `(ink, paper)` — the lit and the unlit color (sRGB 0..1) — or `None` for
    /// full color. The phosphors glow on a near-black screen tinted like them;
    /// paper is ink #101010 on #F2F2EC; custom glows `custom` on black.
    pub fn colors(self, custom: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
        let hex = |c: u32| [(c >> 16) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0];
        Some(match self {
            PhosphorMode::Off => return None,
            PhosphorMode::Amber => (hex(0xFFB000), hex(0x140C02)),
            PhosphorMode::Green => (hex(0x33FF66), hex(0x03120A)),
            PhosphorMode::White => (hex(0xE8EEF2), hex(0x0C0E10)),
            PhosphorMode::Blue => (hex(0x5CB8FF), hex(0x030A16)),
            PhosphorMode::Paper => (hex(0x101010), hex(0xF2F2EC)),
            PhosphorMode::Custom => (custom.map(|v| finite_or(v, 1.0).clamp(0.0, 1.0)), [0.0; 3]),
        })
    }
}

fn ef_false() -> bool { false }
fn ef_true() -> bool { true }
fn ef_zero() -> f32 { 0.0 }
fn ef_curvature() -> f32 { 0.0 }
fn ef_scanline() -> f32 { 0.50 }
fn ef_mask() -> f32 { 0.30 }
fn ef_bloom() -> f32 { 0.40 }
fn ef_bloom_radius() -> f32 { 0.0 }
fn ef_chromatic() -> f32 { 0.20 }
fn ef_vignette() -> f32 { 0.40 }
fn ef_flash_ms() -> f32 { 130.0 }
fn ef_white() -> [f32; 3] { [1.0, 1.0, 1.0] }
fn ef_phosphor_color() -> [f32; 3] { [1.0, 0.69, 0.0] }

impl Default for EffectsConfig {
    fn default() -> Self {
        EffectsConfig {
            crt_enabled: ef_false(), crt_curvature: ef_curvature(), crt_scanline: ef_scanline(),
            crt_mask: ef_mask(), crt_bloom: ef_bloom(), crt_chromatic: ef_chromatic(),
            crt_vignette: ef_vignette(), crt_scanline_tint: ef_white(),
            crt_animate_roll: ef_false(), crt_flicker: ef_false(), crt_jitter: ef_false(),
            caret_flash_enabled: ef_true(), caret_glow_enabled: ef_false(),
            caret_flash_ms: ef_flash_ms(), caret_flash_color: ef_white(),
            crt_bloom_radius: ef_bloom_radius(), crt_phosphor: PhosphorMode::Off,
            crt_phosphor_color: ef_phosphor_color(), crt_phosphor_hue: ef_zero(),
            crt_grain: ef_zero(), crt_grain_animate: ef_false(), crt_dither: ef_false(),
            glitch_on_error: ef_false(), glitch_on_bell: ef_false(), animate_unfocused: ef_false(),
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
        self.crt_bloom_radius = c01(self.crt_bloom_radius, ef_bloom_radius());
        self.crt_phosphor_hue = c01(self.crt_phosphor_hue, 0.0);
        self.crt_grain = c01(self.crt_grain, 0.0);
        let pc = ef_phosphor_color();
        for (ch, d) in self.crt_phosphor_color.iter_mut().zip(pc) { *ch = c01(*ch, d); }
        self
    }

    /// True iff an *animated* CRT sub-effect is live: CRT enabled AND roll,
    /// flicker, jitter or animated grain on. Static CRT is `false`, so it stays
    /// damage-driven (0-CPU idle). The single source of truth for the paced
    /// animation wake in `about_to_wait` (≤30 fps timed wakes, never Poll; paused
    /// while unfocused unless `animate_unfocused`). Lives on `EffectsConfig` (not
    /// `App`) so callers borrow only the `fx` field, leaving `gpu`/`text` free to
    /// be mutably borrowed in the render path.
    pub fn crt_anim_live(&self) -> bool {
        self.crt_enabled
            && (self.crt_animate_roll
                || self.crt_flicker
                || self.crt_jitter
                || (self.crt_grain > 0.0 && self.crt_grain_animate))
    }

    /// Whether any event-glitch trigger is on (the glitch support is then kept
    /// compiled, so a burst never builds a pipeline mid-frame).
    pub fn glitch_enabled(&self) -> bool {
        self.glitch_on_error || self.glitch_on_bell
    }
}

/// The `[backdrop]` table: the background layer drawn under the terminal grid
/// (visuals v2). Every key is optional (`#[serde(default)]`), so an old config
/// — or a table naming only `mode` — loads with the defaults below. Enum-like
/// keys are kept as strings and parsed leniently by the renderer (an unknown
/// value reads as the default, like `window_mode`): one typo never fails a load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackdropConfig {
    /// `"none"` (default: nothing is built or drawn — today's look), `"theme"`
    /// (a curated look per theme), `"gradient"`, `"image"` or `"pattern"`.
    pub mode: String,
    /// Gradient stops as `"#rrggbb"` (2–4 used). Empty = derived from the theme.
    pub colors: Vec<String>,
    /// Linear gradient direction in degrees, CSS-style: 0 = toward the top, 90 =
    /// toward the right, 135 (default) = top-left → bottom-right.
    pub angle: f32,
    /// `"linear"` (default) or `"radial"`.
    pub shape: String,
    /// How strongly the backdrop shows over the theme background: 0 = the
    /// plain theme bg … 1 = the colors as given. Default 0.5.
    pub strength: f32,
    /// Darkening toward the window corners, 0..1 (default 0).
    pub vignette: f32,
    /// Film grain, 0..1 (default 0). Dithering against banding is always on.
    pub grain: f32,
    /// Image file for `mode = "image"`: absolute, `~/…`, or a name relative to
    /// `<config dir>/backgrounds/`. PNG or JPEG, up to 8192×8192.
    pub image: String,
    /// `"cover"` (default), `"contain"`, `"stretch"`, `"center"` or `"tile"`.
    pub fit: String,
    /// Blend of the image toward the theme background, 0..1 (default 0.7). It is
    /// raised automatically when the image would leave the text below 4.5:1.
    pub dim: f32,
    /// Frosted-glass blur of the image, 0..1 (default 0 = sharp).
    pub blur: f32,
    /// `"stars"` (default), `"aurora"`, `"grid"` or `"synthwave"`.
    pub pattern: String,
    /// Slow motion for gradients and patterns (opt-in; ≤ 30 fps, never on a
    /// software renderer, paused while the window is hidden). Default false.
    pub animate: bool,
    /// Shift the backdrop with the scrollback position. Default false.
    pub parallax: bool,
}

impl Default for BackdropConfig {
    fn default() -> Self {
        BackdropConfig {
            mode: "none".to_string(),
            colors: Vec::new(),
            angle: 135.0,
            shape: "linear".to_string(),
            strength: 0.5,
            vignette: 0.0,
            grain: 0.0,
            image: String::new(),
            fit: "cover".to_string(),
            dim: 0.7,
            blur: 0.0,
            pattern: "stars".to_string(),
            animate: false,
            parallax: false,
        }
    }
}

impl BackdropConfig {
    /// Clamp the numbers into range (non-finite → default; the angle wraps to
    /// 0..360). Strings stay verbatim — the renderer parses them leniently, and
    /// the file keeps what the user wrote. Called on load.
    pub fn clamped(mut self) -> Self {
        let d = BackdropConfig::default();
        let c01 = |v: f32, d: f32| finite_or(v, d).clamp(0.0, 1.0);
        self.strength = c01(self.strength, d.strength);
        self.vignette = c01(self.vignette, d.vignette);
        self.grain = c01(self.grain, d.grain);
        self.dim = c01(self.dim, d.dim);
        self.blur = c01(self.blur, d.blur);
        self.angle = finite_or(self.angle, d.angle).rem_euclid(360.0);
        self
    }

    /// The parsed settings the renderer reads (cheap; built on load / reload /
    /// a settings change, never per frame). Unparseable colors are skipped.
    pub fn settings(&self) -> jetty_render::BackdropSettings {
        jetty_render::BackdropSettings {
            mode: jetty_render::BackdropMode::parse(&self.mode),
            colors: self.colors.iter().filter_map(|c| jetty_render::parse_hex_color(c)).collect(),
            angle: self.angle,
            shape: jetty_render::BackdropShape::parse(&self.shape),
            strength: self.strength,
            vignette: self.vignette,
            grain: self.grain,
            fit: jetty_render::BackdropFit::parse(&self.fit),
            dim: self.dim,
            blur: self.blur,
            pattern: jetty_render::BackdropPattern::parse(&self.pattern),
            animate: self.animate,
            parallax: self.parallax,
        }
    }

    /// The image file `image` names, resolved against `config_dir`: `None` when
    /// empty; `~/…` is the home directory; a relative name lives in
    /// `<config_dir>/backgrounds/` (where Settings lists images).
    pub fn image_path(&self, config_dir: &Path) -> Option<PathBuf> {
        let raw = self.image.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            let home = std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir)?;
            return Some(home.join(rest));
        }
        let p = Path::new(raw);
        Some(if p.is_absolute() { p.to_path_buf() } else { config_dir.join("backgrounds").join(p) })
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: default_theme(),
            follow_system_theme: false,
            light_theme: default_light_theme(),
            minimum_contrast: default_minimum_contrast(),
            opacity: default_opacity(),
            font_size: default_font_size(),
            font_family: default_font_family(),
            ui_font_family: default_ui_font_family(),
            ui_font_size: default_ui_font_size(),
            builtin_glyphs: default_builtin_glyphs(),
            color_emoji: default_color_emoji(),
            bold_is_bright: false,
            corner_radius: default_corner_radius(),
            padding_x: default_padding_x(),
            padding_y: default_padding_y(),
            line_height: default_line_height(),
            scrollbar: ScrollbarMode::default(),
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
            tab_style: default_tab_style(),
            tab_close_button: default_tab_close_button(),
            tab_bar_opacity: default_tab_bar_opacity(),
            progress_bar: default_progress_bar(),
            window_border: default_window_border(),
            tab_title: default_tab_title(),
            effects: EffectsConfig::default(),
            backdrop: BackdropConfig::default(),
            cursor: CursorConfig::default(),
            reduce_motion: default_reduce_motion(),
            visual_bell: default_visual_bell(),
            command_pulse: default_command_pulse(),
            notify_on_command_finish: default_notify_on_command_finish(),
            notify_min_seconds: default_notify_min_seconds(),
            notify_only_on_failure: default_notify_only_on_failure(),
            auto_summon_on_finish: default_auto_summon_on_finish(),
            osc52_allow_paste: default_osc52_allow_paste(),
            run_selection: default_run_selection(),
            hot_reload: default_hot_reload(),
            macos_option_as_alt: crate::input::OptionAsAlt::default(),
            copy_on_select: crate::clipboard::CopyOnSelect::default(),
            kitty_keyboard: default_kitty_keyboard(),
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
    /// `launch_at_login` as the file itself sets it — `None` when there is no
    /// file, it could not be read or parsed, or the key is missing or invalid:
    /// the loaded value is then only a default, which must never decide to
    /// delete the user's login item.
    pub launch_at_login: Option<bool>,
}

impl Config {
    /// Resolve the JeTTY config DIRECTORY: `$JETTY_CONFIG_DIR` when set (a whole
    /// alternate config tree — e.g. to try a setup without touching your own),
    /// else `<config_dir>/jetty` (`~/.config/jetty` on Linux, `~/Library/
    /// Application Support/jetty` on macOS), falling back to `~/.config/jetty` when
    /// the OS dir is unknown. It holds `config.toml` and `themes/`; the hot-reload
    /// watcher and the theme loader both key off it.
    /// Whether `$JETTY_CONFIG_DIR` points JeTTY at an alternate config tree (a
    /// setup being tried out): what belongs to the user's REAL session — the
    /// login item — is then left alone.
    pub(crate) fn dir_overridden() -> bool {
        std::env::var_os("JETTY_CONFIG_DIR").is_some_and(|d| !d.is_empty())
    }

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
                // A dangling symlink (a dotfiles repo not checked out yet) is not
                // "no config": say so — a save writes the link's target, if its
                // folder exists (JeTTY never creates folders in a dotfiles tree).
                let warnings = symlink_target(path)
                    .map(|target| {
                        let then = if target.parent().is_some_and(Path::is_dir) {
                            "a settings change creates it there"
                        } else {
                            "its folder does not exist either: settings changes are not saved \
                             until it does"
                        };
                        format!(
                            "config.toml links to {}, which does not exist — using the default \
                             settings ({then})",
                            target.display()
                        )
                    })
                    .into_iter()
                    .collect();
                return Loaded { cfg: Config::default(), warnings, hash: None, launch_at_login: None };
            }
            Err(e) => {
                return Loaded {
                    cfg: Config::default(),
                    warnings: vec![format!(
                        "could not read {}: {e} — using the default settings",
                        path.display()
                    )],
                    hash: None,
                    launch_at_login: None,
                };
            }
        };
        let hash = Some(hash_str(&s));
        match Self::parse_with_base(&s, &Config::default(), "using the default") {
            Ok((cfg, warnings)) => {
                Loaded { cfg, warnings, hash, launch_at_login: explicit_launch_at_login(&s) }
            }
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
                    launch_at_login: None,
                }
            }
        }
    }

    /// Parse config text key by key on top of `base`.
    ///
    /// `Err` only for a TOML syntax error (the whole document is unreadable). Valid
    /// TOML always yields a config: each key with an invalid value — a wrong type,
    /// or an enum-like word the app does not know — is replaced by `base`'s value
    /// (the defaults at startup, the live settings on a hot-reload) and reported
    /// (`fallback` says which, e.g. "using the default"), with the closest valid
    /// value when there is one; an enum-like word in another letter case reads as
    /// meant; unknown keys are ignored with a warning naming the closest known
    /// key; a number outside its range is clamped and reported. Generic over the
    /// struct: a new field needs no code here (see [`check`]). Takes the
    /// already-read content so the caller can hash and parse the SAME bytes (no
    /// TOCTOU between the two).
    pub(crate) fn parse_with_base(
        s: &str,
        base: &Config,
        fallback: &str,
    ) -> Result<(Config, Vec<String>), String> {
        let user: toml::Table = toml::from_str(s).map_err(|e| describe_toml_error(&e, s))?;
        let mut warnings = Vec::new();
        let mut invalid: Vec<Vec<String>> = Vec::new();
        let mut cfg = match toml::Value::Table(user.clone()).try_into::<Config>() {
            Ok(cfg) => cfg,
            Err(_) => Self::fallback_per_key(&user, base, fallback, &mut warnings, &mut invalid),
        };
        check::check_choices(&user, &mut cfg, base, fallback, &mut warnings, &mut invalid);
        check::unknown_keys(&user, &invalid, &mut warnings);
        check::color_warnings(&user, &invalid, &mut warnings);
        let clean = cfg.clone().sanitized();
        check::range_warnings(&user, &cfg, &clean, &invalid, &mut warnings);
        Ok((clean, warnings))
    }

    /// The slow path of [`Config::parse_with_base`]: probe every leaf of `user` on
    /// its own (against the defaults, so a failure can only be that leaf's), swap
    /// each invalid one for `base`'s value (or drop it when `base` has none), then
    /// deserialize the repaired table — or, should that still fail, re-validate it
    /// section by section ([`Config::revalidate_sections`]).
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
                    "`{} = {}` is invalid{} ({}) — {fallback}",
                    path.join("."),
                    short_value(&value),
                    check::variant_hint(e.message().trim()),
                    friendly_expected(e.message()),
                ));
                match get_path(&base_t, &path) {
                    Some(v) => set_path(&mut doc, &path, v.clone()),
                    None => remove_path(&mut doc, &path),
                }
                invalid.push(path);
            }
        }
        match toml::Value::Table(doc.clone()).try_into::<Config>() {
            Ok(cfg) => cfg,
            // No single leaf explains the failure (should not happen — fields are
            // independent): never reset the whole file for it.
            Err(_) => Self::revalidate_sections(&doc, base, fallback, warnings, invalid),
        }
    }

    /// The last resort when a document fails as a whole: keep each top-level
    /// entry (a key, or a whole section like `[keys]`) that is valid on its own
    /// against the defaults, replace each one that is not with `base`'s (or drop
    /// it), and report it — so one broken section never takes the rest along.
    pub(crate) fn revalidate_sections(
        doc: &toml::Table,
        base: &Config,
        fallback: &str,
        warnings: &mut Vec<String>,
        invalid: &mut Vec<Vec<String>>,
    ) -> Config {
        let defaults = to_table(&Config::default());
        let base_t = to_table(base);
        let mut kept = toml::Table::new();
        for (k, v) in doc {
            let mut probe = defaults.clone();
            probe.insert(k.clone(), v.clone());
            match toml::Value::Table(probe).try_into::<Config>() {
                Ok(_) => {
                    kept.insert(k.clone(), v.clone());
                }
                Err(e) => {
                    warnings.push(format!(
                        "`{k}` could not be applied ({}) — {fallback}",
                        friendly_expected(e.message())
                    ));
                    if let Some(b) = base_t.get(k) {
                        kept.insert(k.clone(), b.clone());
                    }
                    invalid.push(vec![k.clone()]);
                }
            }
        }
        match toml::Value::Table(kept).try_into::<Config>() {
            Ok(cfg) => cfg,
            // Every entry is valid alone yet not together: still never panic or
            // apply garbage.
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
        self.backdrop = self.backdrop.clamped();
        self.cursor = self.cursor.clamped();
        self
    }

    /// Replace every non-finite float with its default and clamp every number
    /// into the range the app supports. TOML 1.x allows a literal `nan` and the
    /// toml crate deserializes it, while `f32::clamp` PROPAGATES NaN — so a
    /// hand-edited `opacity = nan` sailed through every load-time clamp
    /// (invisible window, collapsed grid, NaN shader uniforms) and `save()`
    /// persisted it right back. In-range values pass through untouched. These
    /// are THE load-time ranges: a value outside one is reported
    /// (`check::range_warnings` finds the range by probing this function).
    fn sanitize_floats(&mut self) {
        // NaN / ±inf → off; otherwise clamped to 1..=21.
        self.minimum_contrast = jetty_core::contrast::clamp_ratio(self.minimum_contrast);
        // A visible floor: 0.0 would load an invisible window, which looks like
        // a launch failure.
        self.opacity = finite_or(self.opacity, 1.0).clamp(0.1, 1.0);
        self.font_size = finite_or(self.font_size, 16.0).clamp(6.0, 48.0);
        self.ui_font_size = finite_or(self.ui_font_size, default_ui_font_size()).clamp(10.0, 28.0);
        self.corner_radius = finite_or(self.corner_radius, 10.0).clamp(0.0, 24.0);
        self.padding_x = finite_or(self.padding_x, default_padding_x()).clamp(0.0, jetty_render::PADDING_MAX);
        self.padding_y = finite_or(self.padding_y, default_padding_y()).clamp(0.0, jetty_render::PADDING_MAX);
        self.line_height = jetty_render::clamp_line_height(self.line_height);
        self.dropdown_height_pct =
            finite_or(self.dropdown_height_pct, default_dropdown_height_pct()).clamp(0.25, 1.0);
        self.dropdown_width_pct =
            finite_or(self.dropdown_width_pct, default_dropdown_width_pct()).clamp(0.2, 1.0);
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

/// Where `path` finally points when it is a symlink — a chain is followed (at
/// most 40 links; a relative target resolves against its link's directory) —
/// WITHOUT requiring the target to exist, unlike `canonicalize`. `None` when
/// `path` is not a symlink (or the chain loops).
pub(crate) fn symlink_target(path: &Path) -> Option<PathBuf> {
    let mut cur = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                let next = std::fs::read_link(&cur).ok()?;
                cur = match cur.parent() {
                    Some(dir) if next.is_relative() => dir.join(next),
                    _ => next,
                };
            }
            _ => return (cur != path).then_some(cur),
        }
    }
    None
}

/// The file a write to `path` must land in: the end of its symlink chain —
/// canonical when it exists, else the dangling target — or `path` itself.
pub(crate) fn real_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| symlink_target(path))
        .unwrap_or_else(|| path.to_path_buf())
}

/// `launch_at_login` as config text `s` itself sets it: `None` when the text is
/// not TOML or the key is missing or not a bool (see [`Loaded::launch_at_login`]).
pub(crate) fn explicit_launch_at_login(s: &str) -> Option<bool> {
    toml::from_str::<toml::Table>(s).ok()?.get("launch_at_login")?.as_bool()
}

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

/// Every leaf of `user` with its path. A table is descended into where the
/// defaults expect a table there, or have nothing at all but its PARENT exists
/// (a top-level section such as `[keys]`, whose overrides have no defaults; an
/// unknown key inside a known section). Anywhere else a table stands where one
/// value belongs and is itself ONE (invalid) leaf — `[keys.copy]`,
/// `copy = { key = "C" }`: split into its own leaves, removing them one by one
/// left an empty `keys.copy = {}` behind that failed the whole document.
fn collect_leaves(
    user: &toml::Table,
    defaults: &toml::Table,
    prefix: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, toml::Value)>,
) {
    for (k, v) in user {
        prefix.push(k.clone());
        let descend = match get_path(defaults, prefix) {
            Some(d) => d.is_table(),
            None => prefix.len() == 1 || get_path(defaults, &prefix[..prefix.len() - 1]).is_some(),
        };
        match v {
            toml::Value::Table(sub) if descend => collect_leaves(sub, defaults, prefix, out),
            _ => out.push((prefix.clone(), v.clone())),
        }
        prefix.pop();
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

/// One-line description of a TOML error in `src`: `line L, column C: message`.
pub(crate) fn describe_toml_error(e: &toml::de::Error, src: &str) -> String {
    let msg = e.message().trim();
    match e.span() {
        Some(span) => format!("{}: {msg}", line_col(src, span.start)),
        None => msg.to_string(),
    }
}

/// `line L, column C` (1-based) of byte offset `at` in `src`. Counted over
/// BYTES — the offset need not be a char boundary, and slicing the str there
/// could split a multi-byte char — with the column in characters (a UTF-8
/// continuation byte starts none).
fn line_col(src: &str, at: usize) -> String {
    let before = &src.as_bytes()[..at.min(src.len())];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let column = before[start..].iter().filter(|&&b| b & 0xC0 != 0x80).count() + 1;
    format!("line {line}, column {column}")
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
    /// A write merged an EXTERNAL edit the app has not applied yet. Until a reload
    /// applies the file (`note_reloaded`), no write counts as purely our own —
    /// or a second settings change before that reload would mark the merged file
    /// as a self-write and the edit would never be applied (file says dracula,
    /// app shows nord).
    external_pending: bool,
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

    /// The settings the file holds, AS THE APP HOLDS THEM — clamped, normalized,
    /// a missing font remembered by its chosen name — become the baseline. A
    /// later save then writes only what the user changes: never a clamped
    /// `opacity`, a normalized `window_mode` or a fallback font over keys they
    /// never touched. Startup only (nothing is pending yet).
    pub(crate) fn rebase(&mut self, synced: Config) {
        debug_assert!(self.pending.is_empty(), "rebase with unsaved changes");
        self.synced = synced;
    }

    /// A hot-reload applied the file (read from bytes hashing to `hash`); `cfg`
    /// is what the app now holds of it (the baseline, as in [`Persister::rebase`]).
    pub(crate) fn note_reloaded(&mut self, cfg: Config, hash: u64) {
        self.synced = cfg;
        let mut s = lock(&self.shared);
        s.known = Some(hash);
        // An identical later re-save of the same content is then a no-op too.
        s.self_written = Some(hash);
        // Whatever external edit our writes merged is applied now.
        s.external_pending = false;
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
    write_changes_racing(path, changes, full, shared, &mut || {})
}

/// How many times a save re-merges because an editor saved `config.toml` while
/// it was being written, before giving up (with a notice).
const SAVE_ATTEMPTS: usize = 5;

/// [`write_changes`], with `race` run between reading the file and committing
/// the rewrite — a test hook standing in for an editor saving right then. The
/// file is re-checked just before the rename: if it changed since it was read,
/// the merge is redone on top of the new content, so the editor's save is never
/// overwritten (what remains is the instant between that check and the rename).
fn write_changes_racing(
    path: &Path,
    changes: &[Change],
    full: &Config,
    shared: &Mutex<SyncShared>,
    race: &mut dyn FnMut(),
) -> Result<(), String> {
    for _ in 0..SAVE_ATTEMPTS {
        let before = read_config_text(path)?;
        // A config.toml without write permission (`chmod a-w`) is the user's
        // way of saying "leave it as it is" — but the atomic rename below needs
        // only a writable folder and would replace it anyway.
        if before.is_some() && std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
            return Err(format!(
                "{} is read-only — settings changes are not saved (make it writable to save them)",
                real_path(path).display()
            ));
        }
        let new_text = merged_text(before.as_deref(), changes, full)?;
        if before.as_deref() == Some(new_text.as_str()) {
            return Ok(()); // nothing to change on disk
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create config dir {}: {e}", dir.display()))?;
        }
        // Through a dangling symlink the file is created at the link's target —
        // but never a folder for it (that could be a dotfiles checkout to come).
        if let Some(target) = symlink_target(path) {
            if !target.parent().is_some_and(Path::is_dir) {
                return Err(format!(
                    "config.toml links to {}, whose folder does not exist — settings changes \
                     are not saved until it does",
                    target.display()
                ));
            }
        }
        race();
        let committed = write_atomic_checked(path, new_text.as_bytes(), || {
            read_config_text(path).ok().as_ref() == Some(&before)
        })
        .map_err(|e| format!("could not save settings to {}: {e}", path.display()))?;
        if committed {
            let h = hash_str(&new_text);
            let mut s = lock(shared);
            if s.known != before.as_deref().map(hash_str) {
                s.external_pending = true;
            }
            s.self_written = if s.external_pending { None } else { Some(h) };
            s.known = Some(h);
            return Ok(());
        }
    }
    Err(format!(
        "config.toml kept changing while settings were saved ({SAVE_ATTEMPTS} tries) — the \
         change was not saved"
    ))
}

/// The config text at `path` (`None` = no file).
fn read_config_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {} to save settings: {e}", path.display())),
    }
}

/// `before` (the file's text, `None` = no file) with `changes` applied in place
/// — or, without a file, every setting of `full`.
fn merged_text(before: Option<&str>, changes: &[Change], full: &Config) -> Result<String, String> {
    Ok(match before {
        // No file yet: write every setting, so the file documents them all.
        None => toml::to_string_pretty(full).map_err(|e| format!("could not serialize settings: {e}"))?,
        Some(text) => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
                let at = e.span().map(|s| format!("{}: ", line_col(text, s.start))).unwrap_or_default();
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
    })
}

/// Write `data` to `path` atomically: write + fsync a temp file in the SAME
/// directory, then `rename` it over the destination. `persist()` runs on every
/// settings click / font hotkey / slider release, and the previous plain
/// `fs::write` (truncate-then-write) left an empty/partial config.toml if the
/// process died mid-write — which `load()` then silently replaced with full
/// defaults, losing every setting. The temp name is PID-suffixed so two
/// processes never share one temp file; rename is atomic on POSIX, so readers
/// always see either the old or the new complete file.
///
/// The rename happens only if `unchanged()` still holds right before it (after
/// the slow write + fsync of the temp file): `Ok(false)` — temp removed, `path`
/// untouched — when it does not.
fn write_atomic_checked(
    path: &std::path::Path,
    data: &[u8],
    unchanged: impl Fn() -> bool,
) -> std::io::Result<bool> {
    use std::io::Write as _;
    // If `path` is a symlink (common dotfiles setup: ~/.config/jetty/config.toml
    // → ~/dotfiles/jetty/config.toml), resolve it and atomic-rename over the
    // TARGET, not the link. A rename onto the link path replaces the symlink
    // itself with a plain file, silently detaching the dotfiles repo — every
    // later setting change stops reaching it and the next `stow`/`chezmoi` sync
    // reverts them (F33). A DANGLING link (the target not created yet) is written
    // through too — `canonicalize` fails there, and falling back to the link
    // path used to replace the link on the first save. Only a path that is no
    // link at all is created as itself.
    let path = real_path(path);
    let path = path.as_path();
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent dir")
    })?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<bool> {
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
        if !unchanged() {
            return Ok(false);
        }
        std::fs::rename(&tmp, path)?;
        // fsync the parent directory so the rename (the directory-entry update)
        // is itself durable: without it, a power loss just after rename() can
        // lose the new dirent on ext4/xfs and leave the OLD file — or nothing.
        // Best-effort: some platforms/filesystems don't support directory fsync.
        if let Ok(dir_file) = std::fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        // Best-effort cleanup of the temp file on a failure or a refused commit.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`write_atomic_checked`] without a precondition.
    fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
        write_atomic_checked(path, data, || true).map(|_| ())
    }

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

    #[cfg(unix)]
    #[test]
    fn a_dangling_config_symlink_warns_and_saves_through_the_link() {
        // A dotfiles link whose target is not there yet: loading said nothing,
        // and the first settings change REPLACED the link with a plain file.
        use std::os::unix::fs::symlink;
        let base = tmp_dir("dangling");
        let dotfiles = base.join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let target = dotfiles.join("jetty.toml");
        let link = base.join("config.toml");
        symlink(&target, &link).unwrap();
        let loaded = Config::load_from(&link);
        assert_eq!(loaded.cfg, Config::default());
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(loaded.warnings[0].contains("jetty.toml"), "{:?}", loaded.warnings);
        let (mut p, mut cfg, _) = persister_for(&link);
        cfg.theme = "nord".to_string();
        p.record(&cfg, Instant::now());
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(Config::load_from(&link).cfg.theme, "nord", "written through it");
        assert!(std::fs::read_to_string(&target).unwrap().contains("theme = \"nord\""));
        // A link into a folder that does not exist (yet): said so, and a save
        // fails visibly instead of creating folders in someone's dotfiles tree.
        let nowhere = base.join("not-cloned").join("jetty.toml");
        let link2 = base.join("other").join("config.toml");
        std::fs::create_dir_all(link2.parent().unwrap()).unwrap();
        symlink(&nowhere, &link2).unwrap();
        let loaded = Config::load_from(&link2);
        assert!(loaded.warnings[0].contains("not saved until it does"), "{:?}", loaded.warnings);
        let (mut p, mut cfg, notices) = persister_for(&link2);
        cfg.theme = "nord".to_string();
        p.record(&cfg, Instant::now());
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        let n = notices.lock().unwrap().clone();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].contains("folder does not exist"), "{n:?}");
        assert!(!base.join("not-cloned").exists(), "no folder created");
        assert!(std::fs::symlink_metadata(&link2).unwrap().file_type().is_symlink());
        // A relative link (as stow makes them) too.
        let rel = base.join("rel.toml");
        symlink("dotfiles/rel-target.toml", &rel).unwrap();
        write_atomic(&rel, b"x = 1\n").unwrap();
        assert!(std::fs::symlink_metadata(&rel).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(dotfiles.join("rel-target.toml")).unwrap(), "x = 1\n");
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
        assert!(c.kitty_keyboard, "the kitty keyboard protocol is on by default");
        let off: Config = toml::from_str("kitty_keyboard = false").unwrap();
        assert!(!off.kitty_keyboard);
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
        // …and without the visuals-v2 chrome keys, with today's look (progress
        // bars are the one new default-on visual).
        assert_eq!(c.tab_style, "pill");
        assert_eq!(c.tab_close_button, "always");
        assert!(!c.tab_bar_opacity);
        assert!(c.progress_bar);
        assert_eq!(c.window_border, "none");
        assert_eq!(c.tab_title, "osc");
    }

    #[test]
    fn chrome_keys_load_and_reject_wrong_types_per_key() {
        let toml = "tab_style = \"slant\"\ntab_close_button = \"active\"\ntab_bar_opacity = true\n\
                    progress_bar = false\nwindow_border = \"always\"\ntab_title = \"auto\"\n";
        let (c, warnings) = Config::parse_with_base(toml, &Config::default(), "using the default").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((c.tab_style.as_str(), c.tab_close_button.as_str()), ("slant", "active"));
        assert!(c.tab_bar_opacity && !c.progress_bar);
        assert_eq!((c.window_border.as_str(), c.tab_title.as_str()), ("always", "auto"));
        // A wrong type falls back for that key alone, with a warning.
        let (c, warnings) =
            Config::parse_with_base("progress_bar = \"yes\"\ntab_style = \"underline\"\n", &Config::default(), "using the default")
                .unwrap();
        assert!(c.progress_bar, "invalid value → default");
        assert_eq!(c.tab_style, "underline", "the valid key still applies");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
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
    fn cursor_table_defaults_and_clamps() {
        // No [cursor] table → today's cursor; motion keys default off.
        let (c, w) = Config::parse_with_base("theme = \"dracula\"\n", &Config::default(), "x").unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.cursor, CursorConfig::default());
        assert_eq!(c.cursor.shape, "block");
        assert!((c.cursor.thickness - 0.12).abs() < 1e-6);
        assert_eq!((c.reduce_motion.as_str(), c.visual_bell.as_str()), ("off", "off"));
        assert_eq!(c.command_pulse, "off");
        // Out-of-range numbers clamp (and say so); a partial table keeps the
        // other defaults.
        let (c, w) = Config::parse_with_base(
            "[cursor]\nthickness = 3.0\ntrail_ms = 5\ntrail_threshold = 0\nshape = \"beam\"\n",
            &Config::default(),
            "x",
        )
        .unwrap();
        assert_eq!(
            w,
            [
                "`cursor.thickness = 3.0` is out of range (0.04–0.5) — using 0.5",
                "`cursor.trail_ms = 5` is out of range (60–1000) — using 60",
                "`cursor.trail_threshold = 0` is out of range (1–40) — using 1",
            ]
        );
        assert_eq!(c.cursor.thickness, 0.5);
        assert_eq!((c.cursor.trail_ms, c.cursor.trail_threshold), (60, 1));
        assert_eq!((c.cursor.shape.as_str(), c.cursor.color.as_str()), ("beam", "theme"));
        // A wrong-typed key falls back alone, with a warning.
        let (c, w) =
            Config::parse_with_base("[cursor]\ntrail = \"yes\"\nguide = \"always\"\n", &Config::default(), "x")
                .unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(!c.cursor.trail);
        assert_eq!(c.cursor.guide, "always");
    }

    #[test]
    fn round_trip_through_toml() {
        let c = Config {
            theme: "dracula".to_string(),
            follow_system_theme: true,
            light_theme: "solarized_light".to_string(),
            minimum_contrast: 4.5,
            opacity: 0.85,
            font_size: 18.0,
            font_family: "Fira Code".to_string(),
            ui_font_family: "Inter".to_string(),
            ui_font_size: 20.0,
            builtin_glyphs: false,
            color_emoji: false,
            bold_is_bright: true,
            corner_radius: 6.0,
            padding_x: 12.0,
            padding_y: 0.0,
            line_height: 1.6,
            scrollbar: ScrollbarMode::Auto,
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
            tab_style: "powerline".to_string(),
            tab_close_button: "hover".to_string(),
            tab_bar_opacity: true,
            progress_bar: false,
            window_border: "focus".to_string(),
            tab_title: "auto".to_string(),
            effects: EffectsConfig::default(),
            backdrop: BackdropConfig {
                mode: "image".to_string(),
                colors: vec!["#102030".to_string(), "#abc".to_string()],
                angle: 45.0,
                shape: "radial".to_string(),
                strength: 0.8,
                vignette: 0.25,
                grain: 0.1,
                image: "~/Pictures/wall.jpg".to_string(),
                fit: "tile".to_string(),
                dim: 0.4,
                blur: 0.3,
                pattern: "aurora".to_string(),
                animate: true,
                parallax: true,
            },
            cursor: CursorConfig {
                shape: "double_underline".to_string(),
                thickness: 0.2,
                unfocused: "none".to_string(),
                color: "auto".to_string(),
                guide: "shell".to_string(),
                trail: true,
                trail_ms: 300,
                trail_threshold: 3,
            },
            reduce_motion: "system".to_string(),
            visual_bell: "rim".to_string(),
            command_pulse: "all".to_string(),
            notify_on_command_finish: false,
            notify_min_seconds: 30,
            notify_only_on_failure: true,
            auto_summon_on_finish: true,
            osc52_allow_paste: true,
            run_selection: false,
            hot_reload: false,
            macos_option_as_alt: crate::input::OptionAsAlt::Left,
            copy_on_select: crate::clipboard::CopyOnSelect::Both,
            kitty_keyboard: false,
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
            follow_system_theme: false,
            light_theme: "catppuccin_latte".to_string(),
            minimum_contrast: 1.0,
            opacity: 0.5,
            font_size: 14.0,
            font_family: "MesloLGS NF".to_string(),
            ui_font_family: String::new(),
            ui_font_size: 16.0,
            builtin_glyphs: true,
            color_emoji: true,
            bold_is_bright: false,
            corner_radius: 12.0,
            padding_x: 0.0,
            padding_y: 6.5,
            line_height: 1.0,
            scrollbar: ScrollbarMode::Never,
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
            tab_style: "pill".to_string(),
            tab_close_button: "always".to_string(),
            tab_bar_opacity: false,
            progress_bar: true,
            window_border: "none".to_string(),
            tab_title: "osc".to_string(),
            effects: EffectsConfig::default(),
            backdrop: BackdropConfig::default(),
            cursor: CursorConfig::default(),
            reduce_motion: "off".to_string(),
            visual_bell: "off".to_string(),
            command_pulse: "off".to_string(),
            notify_on_command_finish: true,
            notify_min_seconds: 10,
            notify_only_on_failure: false,
            auto_summon_on_finish: false,
            osc52_allow_paste: false,
            run_selection: true,
            hot_reload: true,
            macos_option_as_alt: crate::input::OptionAsAlt::None,
            copy_on_select: crate::clipboard::CopyOnSelect::Primary,
            kitty_keyboard: true,
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
    fn glyph_keys_default_and_parse() {
        let d = Config::default();
        assert!(d.builtin_glyphs && d.color_emoji && !d.bold_is_bright);
        // An older config without the keys loads the defaults, warning-free.
        let (cfg, warnings) =
            Config::parse_with_base("theme = \"dracula\"\n", &Config::default(), "using the default").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(cfg.builtin_glyphs && cfg.color_emoji && !cfg.bold_is_bright);
        let (cfg, warnings) = Config::parse_with_base(
            "builtin_glyphs = false\ncolor_emoji = false\nbold_is_bright = true\n",
            &Config::default(),
            "using the default",
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!cfg.builtin_glyphs && !cfg.color_emoji && cfg.bold_is_bright);
        // A wrong type falls back per key.
        let (cfg, warnings) =
            Config::parse_with_base("builtin_glyphs = \"yes\"\n", &Config::default(), "using the default").unwrap();
        assert!(cfg.builtin_glyphs);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn padding_defaults_and_sanitize() {
        let d = Config::default();
        assert_eq!((d.padding_x, d.padding_y), (8.0, 4.0));
        // A config written before the keys existed loads the padded default.
        let (cfg, warnings) =
            Config::parse_with_base("theme = \"dracula\"\n", &Config::default(), "using the default").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((cfg.padding_x, cfg.padding_y), (8.0, 4.0));
        // 0 keeps the unpadded look; garbage is clamped or replaced.
        let (cfg, _) = Config::parse_with_base(
            "padding_x = 0.0\npadding_y = -5.0\n",
            &Config::default(),
            "using the default",
        )
        .unwrap();
        assert_eq!((cfg.padding_x, cfg.padding_y), (0.0, 0.0));
        let (cfg, _) = Config::parse_with_base(
            "padding_x = nan\npadding_y = 1000.0\n",
            &Config::default(),
            "using the default",
        )
        .unwrap();
        assert_eq!((cfg.padding_x, cfg.padding_y), (8.0, jetty_render::PADDING_MAX));
        // A wrong type falls back to the default with a warning, per key.
        let (cfg, warnings) =
            Config::parse_with_base("padding_x = \"wide\"\n", &Config::default(), "using the default").unwrap();
        assert_eq!(cfg.padding_x, 8.0);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn scrollbar_mode_parses_leniently_and_round_trips() {
        let parse = |s: &str| Config::parse_with_base(s, &Config::default(), "using the default").unwrap();
        assert_eq!(Config::default().scrollbar, ScrollbarMode::Always, "today's behaviour");
        assert_eq!(parse("theme = \"dracula\"\n").0.scrollbar, ScrollbarMode::Always);
        for (src, want) in [
            ("scrollbar = \"always\"", ScrollbarMode::Always),
            ("scrollbar = \"auto\"", ScrollbarMode::Auto),
            ("scrollbar = \"Never\"", ScrollbarMode::Never),
            ("scrollbar = \"off\"", ScrollbarMode::Never),
            ("scrollbar = false", ScrollbarMode::Never),
            ("scrollbar = true", ScrollbarMode::Always),
        ] {
            let (cfg, warnings) = parse(src);
            assert_eq!(cfg.scrollbar, want, "{src}");
            assert!(warnings.is_empty(), "{src}: {warnings:?}");
        }
        // An unknown word still reads as the default — and now says so.
        let (cfg, warnings) = parse("scrollbar = \"sometimes\"\n");
        assert_eq!((cfg.scrollbar, warnings.len()), (ScrollbarMode::Always, 1), "{warnings:?}");
        // A wrong type falls back with a warning, per key.
        let (cfg, warnings) = parse("scrollbar = 3\n");
        assert_eq!((cfg.scrollbar, warnings.len()), (ScrollbarMode::Always, 1));
        // Saved spelling.
        for m in [ScrollbarMode::Always, ScrollbarMode::Auto, ScrollbarMode::Never] {
            let c = Config { scrollbar: m, ..Config::default() };
            let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
            assert_eq!(back.scrollbar, m);
        }
    }

    #[test]
    fn scrollbar_mode_visibility_rules() {
        use ScrollbarMode::*;
        // (scrolled back, dragging, hovering) for every combination.
        for sb in [false, true] {
            for d in [false, true] {
                for h in [false, true] {
                    assert!(Always.shows_thumb(sb, d, h));
                    assert!(!Never.shows_thumb(sb, d, h));
                    assert_eq!(Auto.shows_thumb(sb, d, h), sb || d || h, "auto({sb},{d},{h})");
                }
            }
        }
        assert!(Always.has_gutter() && Auto.has_gutter(), "auto keeps the gutter: the thumb never covers text");
        assert!(!Never.has_gutter());
    }

    #[test]
    fn line_height_default_and_clamp() {
        assert_eq!(Config::default().line_height, 1.3, "the long-standing spacing");
        let parse = |s: &str| Config::parse_with_base(s, &Config::default(), "using the default").unwrap().0;
        assert_eq!(parse("theme = \"dracula\"\n").line_height, 1.3, "an older config loads the default");
        assert_eq!(parse("line_height = 1.5\n").line_height, 1.5);
        assert_eq!(parse("line_height = \"tall\"\n").line_height, 1.3, "the wrong type: default");
        assert_eq!(parse("line_height = 0.5\n").line_height, 1.0, "clamped up");
        assert_eq!(parse("line_height = 9.0\n").line_height, 2.0, "clamped down");
        assert_eq!(parse("line_height = nan\n").line_height, 1.3, "non-finite: default");
    }

    #[test]
    fn backdrop_defaults_are_off() {
        let d = Config::default().backdrop;
        assert_eq!(d.mode, "none");
        assert!(d.colors.is_empty());
        assert_eq!((d.angle, d.strength, d.vignette, d.grain), (135.0, 0.5, 0.0, 0.0));
        assert_eq!((d.fit.as_str(), d.dim, d.blur), ("cover", 0.7, 0.0));
        assert_eq!((d.shape.as_str(), d.pattern.as_str()), ("linear", "stars"));
        assert!(!d.animate && !d.parallax && d.image.is_empty());
        // The renderer sees "off": nothing is built.
        assert!(d.settings().is_off());
        assert_eq!(d.settings(), jetty_render::BackdropSettings::default());
        // A config without the table loads it with no warning.
        let (cfg, warnings) =
            Config::parse_with_base("theme = \"nord\"\n", &Config::default(), "using the default").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.backdrop, BackdropConfig::default());
    }

    #[test]
    fn backdrop_partial_table_and_clamping() {
        let (cfg, warnings) = Config::parse_with_base(
            "[backdrop]\nmode = \"pattern\"\npattern = \"aurora\"\nstrength = 3.0\ndim = -1.0\nangle = -90.0\n\
             blur = nan\ncolors = [\"#ff0000\", \"bogus\", \"0f0\"]\n",
            &Config::default(),
            "using the default",
        )
        .unwrap();
        // Each clamp / skip is reported (the angle's wrap is no problem).
        assert_eq!(
            warnings,
            [
                "`backdrop.colors` entry \"bogus\" is not a color (\"#rrggbb\" or \"#rgb\") — skipped",
                "`backdrop.blur = nan` is not a finite number — using 0",
                "`backdrop.dim = -1.0` is out of range (0–1) — using 0",
                "`backdrop.strength = 3.0` is out of range (0–1) — using 1",
            ]
        );
        let b = &cfg.backdrop;
        assert_eq!((b.mode.as_str(), b.pattern.as_str()), ("pattern", "aurora"));
        assert_eq!(b.strength, 1.0, "clamped to 0..1");
        assert_eq!(b.dim, 0.0);
        assert_eq!(b.angle, 270.0, "the angle wraps");
        assert_eq!(b.blur, 0.0, "nan falls back to the default");
        assert_eq!(b.fit, "cover", "keys left out keep their default");
        let s = b.settings();
        assert_eq!(s.mode, jetty_render::BackdropMode::Pattern);
        assert_eq!(s.pattern, jetty_render::BackdropPattern::Aurora);
        assert_eq!(s.colors, vec![[255, 0, 0], [0, 255, 0]], "unparseable colors are skipped");
        // An unknown mode reads as off, as before — and is reported.
        let (cfg, w) =
            Config::parse_with_base("[backdrop]\nmode = \"wallpaper\"\n", &Config::default(), "x").unwrap();
        assert!(cfg.backdrop.settings().is_off());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("`backdrop.mode = \"wallpaper\"` is invalid"), "{w:?}");
    }

    #[test]
    fn backdrop_bad_values_fall_back_per_key() {
        let (cfg, warnings) = Config::parse_with_base(
            "[backdrop]\nmode = \"theme\"\nstrength = \"high\"\nanimate = 1\nnope = true\n",
            &Config::default(),
            "using the default",
        )
        .unwrap();
        assert_eq!(cfg.backdrop.mode, "theme", "the valid key still applies");
        assert_eq!(cfg.backdrop.strength, 0.5);
        assert!(!cfg.backdrop.animate);
        assert!(warnings.iter().any(|w| w.contains("backdrop.strength")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("backdrop.animate")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("unknown key `backdrop.nope`")), "{warnings:?}");
    }

    #[test]
    fn backdrop_image_paths_resolve() {
        let dir = Path::new("/cfg/jetty");
        let with = |s: &str| BackdropConfig { image: s.to_string(), ..BackdropConfig::default() };
        assert_eq!(with("").image_path(dir), None);
        assert_eq!(with("   ").image_path(dir), None);
        assert_eq!(with("/abs/wall.png").image_path(dir), Some(PathBuf::from("/abs/wall.png")));
        assert_eq!(with("wall.jpg").image_path(dir), Some(PathBuf::from("/cfg/jetty/backgrounds/wall.jpg")));
        assert_eq!(with("sub/a.png").image_path(dir), Some(PathBuf::from("/cfg/jetty/backgrounds/sub/a.png")));
        let home = with("~/Pictures/x.png").image_path(dir).unwrap();
        assert!(home.ends_with("Pictures/x.png") && home.is_absolute(), "{home:?}");
    }

    #[test]
    fn backdrop_default_is_never_written() {
        // A settings change elsewhere must not write a `[backdrop]` table into a
        // file that never had one (only changed keys are saved).
        let old = Config::default();
        let new = Config { opacity: 0.8, ..Config::default() };
        let changes = diff_configs(&old, &new);
        assert!(changes.iter().all(|c| c.path[0] != "backdrop"), "{changes:?}");
        // Changing one backdrop key writes exactly that key.
        let mut on = Config::default();
        on.backdrop.mode = "theme".to_string();
        let changes = diff_configs(&old, &on);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, vec!["backdrop".to_string(), "mode".to_string()]);
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
    fn appearance_keys_default_parse_and_sanitize() {
        let d = Config::default();
        assert!(!d.follow_system_theme, "following the system is opt-in");
        assert_eq!(d.light_theme, "catppuccin_latte");
        assert_eq!(d.minimum_contrast, 1.0, "minimum contrast is off by default");
        let (cfg, warnings) = Config::parse_with_base(
            "follow_system_theme = true\nlight_theme = \"solarized_light\"\nminimum_contrast = 3\n",
            &d,
            "using the default",
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(cfg.follow_system_theme);
        assert_eq!(cfg.light_theme, "solarized_light");
        assert_eq!(cfg.minimum_contrast, 3.0, "an integer ratio is accepted");
        for (text, want) in [("minimum_contrast = nan", 1.0), ("minimum_contrast = 0.2", 1.0), ("minimum_contrast = 40.0", 21.0)] {
            let (cfg, _) = Config::parse_with_base(text, &d, "using the default").unwrap();
            assert_eq!(cfg.minimum_contrast, want, "{text}");
        }
        // A wrong type keeps the base value, with a warning (per-key loading).
        let (cfg, warnings) =
            Config::parse_with_base("follow_system_theme = \"yes\"\n", &d, "using the default").unwrap();
        assert!(!cfg.follow_system_theme);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
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
    fn a_table_under_keys_is_one_bad_binding_not_a_config_reset() {
        // `[keys]` has no defaults to compare against, and each of these used to
        // be split into leaves that were removed one by one — leaving an EMPTY
        // `keys.copy = {}` behind that failed the whole document, so EVERY setting
        // fell back (at startup: a full reset to defaults).
        for (src, key) in [
            ("[keys]\ncopy = { key = \"C\", mods = \"Ctrl+Shift\" }\n", "keys.copy"),
            ("[keys.copy]\nkey = \"C\"\nmods = \"Ctrl+Shift\"\n", "keys.copy"),
            ("[keys]\nnew_tab = { chord = \"Ctrl+T\" }\npaste = \"Ctrl+V\"\n", "keys.new_tab"),
            ("[keys]\nselect_tab_1.key = \"Alt+1\"\n", "keys.select_tab_1"),
        ] {
            let full = format!("theme = \"nord\"\nfont_size = 13.0\n{src}");
            let (cfg, w) = Config::parse_with_base(&full, &Config::default(), "using the default")
                .expect("valid TOML");
            assert_eq!(cfg.theme, "nord", "{src}: the rest of the file applies");
            assert_eq!(cfg.font_size, 13.0, "{src}");
            assert_eq!(w.len(), 1, "{src}: one warning: {w:?}");
            assert!(w[0].contains(&format!("`{key} = ")), "{src}: {w:?}");
            assert!(w[0].contains("chord string"), "{src}: {w:?}");
        }
        // The valid binding next to a bad one still applies.
        let (cfg, _) = Config::parse_with_base(
            "[keys]\nnew_tab = { chord = \"Ctrl+T\" }\npaste = \"Ctrl+V\"\n",
            &Config::default(),
            "x",
        )
        .unwrap();
        assert_eq!(cfg.keys.paste, Some(ChordSpec::One("Ctrl+V".to_string())));
        assert_eq!(cfg.keys.new_tab, None);
        // On a hot-reload the bad binding keeps its live value.
        let live = Config {
            keys: KeyBindings { copy: Some(ChordSpec::One("Ctrl+Y".to_string())), ..KeyBindings::default() },
            ..Config::default()
        };
        let (cfg, _) =
            Config::parse_with_base("theme = \"nord\"\n[keys.copy]\nkey = \"C\"\n", &live, "keeping").unwrap();
        assert_eq!(cfg.theme, "nord");
        assert_eq!(cfg.keys.copy, Some(ChordSpec::One("Ctrl+Y".to_string())));
    }

    #[test]
    fn dotted_unknown_keys_under_keys_are_reported_not_fatal() {
        // `select_tab.1 = …` (a guess at `select_tab_1`) is a TABLE named
        // `select_tab`: an unknown key, never a reset.
        let (cfg, w) = Config::parse_with_base(
            "theme = \"nord\"\n[keys]\nselect_tab.1 = \"Alt+1\"\ncopy = \"Ctrl+Shift+C\"\n",
            &Config::default(),
            "x",
        )
        .unwrap();
        assert_eq!(cfg.theme, "nord");
        assert_eq!(cfg.keys.copy, Some(ChordSpec::One("Ctrl+Shift+C".to_string())));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("keys.select_tab"), "{w:?}");
    }

    #[test]
    fn a_failure_no_single_key_explains_falls_back_per_section() {
        // The last-resort path (the repaired document still fails) keeps every
        // top-level entry that is valid on its own instead of resetting all of it.
        let user: toml::Table = toml::from_str("theme = \"nord\"\nopacity = 0.5\n").unwrap();
        let mut warnings = Vec::new();
        let mut invalid = Vec::new();
        let cfg = Config::revalidate_sections(
            &user,
            &Config::default(),
            "using the default",
            &mut warnings,
            &mut invalid,
        );
        assert_eq!((cfg.theme.as_str(), cfg.opacity), ("nord", 0.5));
        assert!(warnings.is_empty(), "{warnings:?}");
        let user: toml::Table = toml::from_str("theme = \"nord\"\nopacity = \"x\"\n").unwrap();
        let cfg = Config::revalidate_sections(&user, &Config::default(), "x", &mut warnings, &mut invalid);
        assert_eq!(cfg.theme, "nord", "the valid section survives");
        assert_eq!(cfg.opacity, Config::default().opacity);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(invalid, vec![vec!["opacity".to_string()]]);
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
    fn launch_at_login_is_reported_only_when_the_file_sets_it() {
        // A default `false` must never be mistaken for "the user turned it off":
        // the startup sync used to DELETE the login item from it.
        let dir = tmp_dir("launch-explicit");
        let path = dir.join("config.toml");
        assert_eq!(Config::load_from(&path).launch_at_login, None, "no file");
        for (text, want) in [
            ("launch_at_login = true\n", Some(true)),
            ("launch_at_login = false\n", Some(false)),
            ("theme = \"nord\"\n", None),
            ("launch_at_login = \"yes\"\n", None),
            ("launch_at_login = = true\n", None), // not TOML
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(Config::load_from(&path).launch_at_login, want, "{text}");
        }
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
    fn a_rebased_baseline_never_writes_values_the_user_did_not_touch() {
        // The app clamps / normalizes what it loads (opacity 0.05 → 0.1,
        // "Dropdown" → "dropdown", a missing font → a fallback). With the raw
        // file as the baseline, the FIRST unrelated settings change wrote all of
        // those over the user's own text.
        let dir = tmp_dir("rebase");
        let path = dir.join("config.toml");
        let text = "opacity = 0.05\nwindow_mode = \"Dropdown\"\nui_font_family = \"Gone Sans\"\n";
        std::fs::write(&path, text).unwrap();
        let (mut p, cfg, _) = persister_for(&path);
        // What the app holds after applying it.
        let mut held = cfg.clone();
        held.opacity = 0.1;
        held.window_mode = "dropdown".to_string();
        p.rebase(held.clone());
        assert!(!p.record(&held, Instant::now()), "nothing the user changed");
        held.font_size = 18.0;
        assert!(p.record(&held, Instant::now()));
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.starts_with(text), "the user's own lines are untouched: {out}");
        assert!(out.contains("font_size = 18.0"), "{out}");
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
    fn a_second_save_before_the_reload_still_leaves_the_external_edit_to_apply() {
        // The user edits config.toml (theme → dracula); before the hot-reload
        // runs, TWO settings changes are saved. The first merges the edit and is
        // rightly not our own — but the second used to read that merged file as
        // "what we last saw", mark ITS write as our own, and the reload then
        // skipped it as an echo: the file said dracula, the app showed nord.
        let dir = tmp_dir("external-pending");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\nfont_size = 15.0\n").unwrap();
        let (mut p, mut cfg, _) = persister_for(&path);
        std::fs::write(&path, "theme = \"dracula\"\nfont_size = 15.0\n").unwrap();
        for size in [20.0, 21.0] {
            cfg.font_size = size;
            p.record(&cfg, Instant::now());
            assert!(p.flush_and_wait(Duration::from_secs(5)));
        }
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("dracula") && out.contains("21.0"), "{out}");
        assert!(!p.is_self_write(hash_str(&out)), "the reload must still apply the edit");
        // Once a reload applied it, our writes are our own again.
        let (applied, _) = Config::parse_with_base(&out, &cfg, "x").unwrap();
        p.note_reloaded(applied.clone(), hash_str(&out));
        let mut cfg = applied;
        cfg.font_size = 22.0;
        p.record(&cfg, Instant::now());
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        assert!(p.is_self_write(hash_str(&std::fs::read_to_string(&path).unwrap())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_editor_save_between_read_and_rename_is_merged_not_overwritten() {
        let dir = tmp_dir("race");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\nfont_size = 15.0\n").unwrap();
        let loaded = Config::load_from(&path);
        let shared = Mutex::new(SyncShared { known: loaded.hash, ..SyncShared::default() });
        let mut cfg = loaded.cfg.clone();
        cfg.font_size = 20.0;
        let changes = diff_configs(&loaded.cfg, &cfg);
        let mut raced = 0;
        let mut editor = || {
            // The editor saves right after JeTTY read the file (first try only).
            if raced == 0 {
                std::fs::write(&path, "theme = \"dracula\"\nfont_size = 15.0\nopacity = 0.8\n").unwrap();
            }
            raced += 1;
        };
        write_changes_racing(&path, &changes, &cfg, &shared, &mut editor).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("theme = \"dracula\""), "the editor's save survives: {out}");
        assert!(out.contains("opacity = 0.8"), "{out}");
        assert!(out.contains("font_size = 20.0"), "and the setting is saved: {out}");
        assert_eq!(raced, 2, "merged again once");
        assert!(lock(&shared).self_written.is_none(), "the merged edit is left to the reload");
        // An editor that never stops saving: give up, never clobber.
        cfg.font_size = 25.0;
        let changes = diff_configs(&loaded.cfg, &cfg);
        let mut flip = 0u32;
        let mut changing = || {
            flip += 1;
            std::fs::write(&path, format!("theme = \"edit{flip}\"\n")).unwrap();
        };
        let err = write_changes_racing(&path, &changes, &cfg, &shared, &mut changing).unwrap_err();
        assert!(err.contains("kept changing"), "{err}");
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("theme = \"edit"), "untouched");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(leftovers, 0, "no temp file left behind");
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

    #[cfg(unix)]
    #[test]
    fn a_read_only_config_is_never_replaced() {
        // `chmod a-w config.toml` says "do not change this file" — but the atomic
        // save renames a new file over it, which needs only a writable folder, so
        // every settings change silently replaced the locked file anyway.
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("readonly");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let (mut p, mut cfg, notices) = persister_for(&path);
        cfg.opacity = 0.5;
        p.record(&cfg, Instant::now());
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theme = \"nord\"\n", "untouched");
        let n = notices.lock().unwrap().clone();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].contains("read-only") && n[0].contains("not saved"), "{n:?}");
        // Writable again: the next change is saved.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        cfg.font_size = 18.0;
        p.record(&cfg, Instant::now());
        assert!(p.flush_and_wait(Duration::from_secs(5)));
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("font_size = 18.0"), "{out}");
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

    // ── values the app would silently read as something else ────────────────

    fn parse(s: &str) -> (Config, Vec<String>) {
        Config::parse_with_base(s, &Config::default(), "using the default").expect("valid TOML")
    }

    #[test]
    fn enum_values_in_any_case_or_spelling_apply_as_meant() {
        // The app's readers of these keys are exact-match: `"Underline"` drew the
        // pill and `"Dropdown"` summoned centered — silently.
        let (c, w) = parse(
            "tab_style = \"Underline\"\nwindow_mode = \"Dropdown\"\nsummon_effect = \"FADE\"\n\
             tab_bar_position = \"Bottom\"\nwindow_border = \" focus \"\nscrollbar = \"Never\"\n\
             [cursor]\nshape = \"double-underline\"\nguide = \"Always\"\n[backdrop]\nmode = \"Theme\"\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.tab_style, "underline");
        assert_eq!(c.window_mode, "dropdown");
        assert_eq!(c.summon_effect, "fade");
        assert_eq!(c.tab_bar_position, "bottom");
        assert_eq!(c.window_border, "focus");
        assert_eq!(c.scrollbar, ScrollbarMode::Never);
        assert_eq!((c.cursor.shape.as_str(), c.cursor.guide.as_str()), ("double_underline", "always"));
        assert_eq!(c.backdrop.mode, "theme");
        // Aliases a parser accepts read as their canonical value, warning-free.
        let (c, w) = parse("visual_bell = \"none\"\nreduce_motion = \"yes\"\n[cursor]\nshape = \"bar\"\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.visual_bell.as_str(), c.reduce_motion.as_str()), ("off", "on"));
        assert_eq!(c.cursor.shape, "beam");
    }

    #[test]
    fn an_unknown_enum_value_is_reported_with_a_suggestion() {
        let (c, w) = parse("window_mode = \"dropdwn\"\ntheme = \"nord\"\n");
        assert_eq!(c.window_mode, "center", "the default, as before");
        assert_eq!(c.theme, "nord", "the rest still applies");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("`window_mode = \"dropdwn\"` is invalid — did you mean `dropdown`?"), "{w:?}");
        assert!(w[0].contains("`center`, `dropdown`, `fullscreen`") && w[0].ends_with("using the default"), "{w:?}");
        // Every enum-like key is checked (a few spot checks)…
        for (src, key) in [
            ("tab_style = \"pil\"", "tab_style"),
            ("[cursor]\nshape = \"bean\"", "cursor.shape"),
            ("[backdrop]\nfit = \"zoom\"", "backdrop.fit"),
            ("copy_on_select = \"clipbaord\"", "copy_on_select"),
            ("scrollbar = \"sometimes\"", "scrollbar"),
        ] {
            let (_, w) = parse(src);
            assert_eq!(w.len(), 1, "{src}: {w:?}");
            assert!(w[0].starts_with(&format!("`{key} = ")), "{src}: {w:?}");
        }
        // …and a strict enum (serde's own error) gets the suggestion too.
        let (_, w) = parse("[effects]\ncrt_phosphor = \"ambr\"\n");
        assert!(w[0].contains("did you mean `amber`?"), "{w:?}");
    }

    #[test]
    fn a_reload_with_an_unknown_enum_value_keeps_the_live_value() {
        // A typo used to switch the live setting to the default (centered window,
        // pill tabs) instead of keeping it, unlike every other invalid value.
        let live = Config { window_mode: "dropdown".into(), tab_style: "slant".into(), ..Config::default() };
        let (c, w) =
            Config::parse_with_base("window_mode = \"dropdwn\"\ntab_style = \"slant\"\n", &live, "keeping the current value")
                .unwrap();
        assert_eq!(c.window_mode, "dropdown");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].ends_with("keeping the current value"), "{w:?}");
    }

    #[test]
    fn unknown_keys_name_the_closest_valid_key() {
        let (_, w) = parse("fontsize = 18\n");
        assert_eq!(w, ["unknown key `fontsize` is ignored — did you mean `font_size`?"]);
        let (_, w) = parse("crt_enabled = true\n");
        assert_eq!(w, ["unknown key `crt_enabled` is ignored — did you mean `crt_enabled` in `[effects]`?"]);
        let (_, w) = parse("[cursor]\nopacity = 0.5\n");
        assert_eq!(w, ["unknown key `cursor.opacity` is ignored — did you mean the top-level `opacity`?"]);
        let (_, w) = parse("[keys]\nnew_tabb = \"Ctrl+T\"\n");
        assert_eq!(w, ["unknown key `keys.new_tabb` is ignored — did you mean `keys.new_tab`?"]);
        // A misspelled table is one warning, not one per key in it.
        let (_, w) = parse("[efects]\ncrt_enabled = true\ncrt_bloom = 0.2\n");
        assert_eq!(w, ["unknown table `[efects]` is ignored — did you mean `[effects]`?"]);
        // Nothing close: just the key.
        let (_, w) = parse("zzz = 1\n");
        assert_eq!(w, ["unknown key `zzz` is ignored"]);
    }

    #[test]
    fn numbers_outside_their_range_are_reported() {
        let (c, w) = parse("opacity = 5.0\nfont_size = 100\nscrollback_lines = 50\n");
        assert_eq!((c.opacity, c.font_size, c.scrollback_lines), (1.0, 48.0, 100));
        assert_eq!(
            w,
            [
                "`font_size = 100` is out of range (6–48) — using 48",
                "`opacity = 5.0` is out of range (0.1–1) — using 1",
                "`scrollback_lines = 50` is out of range (100–100000) — using 100",
            ]
        );
        let (c, w) = parse("opacity = nan\n[cursor]\nthickness = 3.0\n");
        assert_eq!((c.opacity, c.cursor.thickness), (1.0, 0.5));
        assert_eq!(
            w,
            [
                "`cursor.thickness = 3.0` is out of range (0.04–0.5) — using 0.5",
                "`opacity = nan` is not a finite number — using 1",
            ]
        );
        // A normalization is not a range: an angle of -90 simply is 270.
        let (c, w) = parse("[backdrop]\nangle = -90.0\n");
        assert_eq!(c.backdrop.angle, 270.0);
        assert!(w.is_empty(), "{w:?}");
        // In-range values (and the defaults) say nothing.
        let (_, w) = parse("opacity = 0.1\nfont_size = 6\nui_font_size = 28\ndropdown_height_pct = 0.25\n");
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn backdrop_colors_that_are_not_colors_are_reported() {
        let (c, w) = parse("[backdrop]\ncolors = [\"#ff0000\", \"bogus\", \"0f0\"]\n");
        assert_eq!(c.backdrop.settings().colors, vec![[255, 0, 0], [0, 255, 0]]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"bogus\"") && w[0].contains("backdrop.colors"), "{w:?}");
        let (_, w) = parse("[backdrop]\ncolors = [\"#111\", \"#222\", \"#333\", \"#444\", \"#555\"]\n");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("first 4"), "{w:?}");
    }

    #[test]
    fn syntax_errors_say_line_and_column() {
        let err = Config::parse_with_base("theme = \"nord\"\nopacity = \n", &Config::default(), "x").unwrap_err();
        assert!(err.starts_with("line 2, column 11:"), "{err}");
        // Columns count characters, not bytes.
        let err = Config::parse_with_base("theme = \"çay\" x\n", &Config::default(), "x").unwrap_err();
        assert!(err.starts_with("line 1, column 15:"), "{err}");
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
