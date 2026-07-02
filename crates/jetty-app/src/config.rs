//! Persisted user settings.
//!
//! Stores the small subset of UI state the user can tweak (theme, opacity,
//! font size + family, corner radius) as a TOML file under the OS config dir
//! (`~/.config/jetty/config.toml` on Linux). Loading is best-effort: a missing
//! file or any parse error falls back to `Config::default()` and never panics.
//! Saving is also best-effort: directory-create and write errors are ignored so
//! a read-only home can never crash the terminal.

use serde::{Deserialize, Serialize};

/// The persisted user settings. Field names are the TOML keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Theme preset name (must match a `jetty_core::theme::PRESETS` entry).
    pub theme: String,
    /// Background opacity in 0.0..=1.0.
    pub opacity: f32,
    /// Logical font size in points.
    pub font_size: f32,
    /// Monospace font family name.
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
    pub corner_radius: f32,
    /// Window-summon reveal effect: "none", "bayer", "phosphor", "liquid", or
    /// "focus" (the last two are Tier-B effects that sample the rendered frame).
    #[serde(default = "default_summon_effect")]
    pub summon_effect: String,
    /// Window summon mode: "center" (re-summon centered/last-pos) or "dropdown"
    /// (Yakuake-style top-anchored full-width strip that slides down).
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
    /// `window_mode` — usable in both Center and Dropdown modes.
    #[serde(default = "default_tab_bar_position")]
    pub tab_bar_position: String,
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
            theme: "catppuccin_mocha".to_string(),
            opacity: 1.0,
            font_size: 16.0,
            font_family: "MesloLGS NF".to_string(),
            ui_font_family: default_ui_font_family(),
            ui_font_size: default_ui_font_size(),
            corner_radius: 10.0,
            summon_effect: default_summon_effect(),
            window_mode: default_window_mode(),
            dropdown_height_pct: default_dropdown_height_pct(),
            dropdown_width_pct: default_dropdown_width_pct(),
            focus_autohide: default_focus_autohide(),
            launch_at_login: default_launch_at_login(),
            summon_hotkey: default_summon_hotkey(),
            shell: default_shell(),
            tab_bar_position: default_tab_bar_position(),
            show_welcome: default_show_welcome(),
            show_perf_hud: default_show_perf_hud(),
            effects: EffectsConfig::default(),
        }
    }
}

impl Config {
    /// Resolve the config file path: `<config_dir>/jetty/config.toml`, falling
    /// back to `~/.config/jetty/config.toml` when `dirs::config_dir()` is
    /// unavailable.
    fn path() -> std::path::PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".config")
        });
        base.join("jetty").join("config.toml")
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
        // Effects floats are sanitized by `EffectsConfig::clamped` (see load()).
    }

    /// Load settings from disk. A missing file or any parse error yields
    /// `Config::default()`; non-finite floats fall back to their defaults —
    /// this never panics.
    pub fn load() -> Config {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let mut cfg: Config = toml::from_str(&s).unwrap_or_default();
                cfg.sanitize_floats();
                cfg.effects = cfg.effects.clamped();
                cfg
            }
            Err(_) => Config::default(),
        }
    }

    /// Persist settings to disk ATOMICALLY. Creates the parent directory if
    /// needed. All errors are ignored: a failed write must never crash the
    /// terminal.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = write_atomic(&path, s.as_bytes());
        }
    }
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
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent dir")
    })?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        // Flush file contents to disk BEFORE the rename so a crash/power loss
        // right after the rename can't leave a zero-length "new" file.
        f.sync_all()?;
        std::fs::rename(&tmp, path)
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
            show_welcome: false,
            show_perf_hud: false,
            effects: EffectsConfig::default(),
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
            show_welcome: true,
            show_perf_hud: true,
            effects: EffectsConfig::default(),
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
    fn effects_config_roundtrips_through_toml() {
        let mut e = EffectsConfig::default();
        e.crt_enabled = true; e.crt_curvature = 0.42; e.crt_flicker = true;
        e.caret_flash_color = [0.1, 0.2, 0.3];
        let mut cfg = Config::default();
        cfg.effects = e.clone();
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.effects, e);
    }
}
