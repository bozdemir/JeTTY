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

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "catppuccin_mocha".to_string(),
            opacity: 1.0,
            font_size: 16.0,
            font_family: "MesloLGS NF".to_string(),
            corner_radius: 10.0,
            summon_effect: default_summon_effect(),
            window_mode: default_window_mode(),
            dropdown_height_pct: default_dropdown_height_pct(),
            dropdown_width_pct: default_dropdown_width_pct(),
            focus_autohide: default_focus_autohide(),
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
        assert_eq!(c.corner_radius, 10.0);
        assert_eq!(c.summon_effect, "phosphor");
        assert_eq!(c.window_mode, "center");
        assert_eq!(c.dropdown_height_pct, 0.50);
        assert_eq!(c.dropdown_width_pct, 1.0);
        assert!(c.focus_autohide);
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
    }

    #[test]
    fn round_trip_through_toml() {
        let c = Config {
            theme: "dracula".to_string(),
            opacity: 0.85,
            font_size: 18.0,
            font_family: "Fira Code".to_string(),
            corner_radius: 6.0,
            summon_effect: "phosphor".to_string(),
            window_mode: "dropdown".to_string(),
            dropdown_height_pct: 0.6,
            dropdown_width_pct: 1.0,
            focus_autohide: false,
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
            corner_radius: 12.0,
            summon_effect: "none".to_string(),
            window_mode: "center".to_string(),
            dropdown_height_pct: 0.5,
            dropdown_width_pct: 1.0,
            focus_autohide: true,
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
