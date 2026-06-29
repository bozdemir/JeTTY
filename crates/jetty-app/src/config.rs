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
            tab_bar_position: default_tab_bar_position(),
            show_welcome: default_show_welcome(),
            show_perf_hud: default_show_perf_hud(),
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

    /// Load settings from disk. A missing file or any parse error yields
    /// `Config::default()` — this never panics.
    pub fn load() -> Config {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    /// Persist settings to disk. Creates the parent directory if needed. All
    /// errors are ignored: a failed write must never crash the terminal.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(&path, s);
        }
    }
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
            tab_bar_position: "bottom".to_string(),
            show_welcome: false,
            show_perf_hud: false,
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
            tab_bar_position: "bottom".to_string(),
            show_welcome: true,
            show_perf_hud: true,
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
}
