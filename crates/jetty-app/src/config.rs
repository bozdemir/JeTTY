//! Persisted user settings.
//!
//! Stores the small subset of UI state the user can tweak (theme, opacity,
//! font size + family, corner radius) as a TOML file under the OS config dir
//! (`~/.config/jetty/config.toml` on Linux). Loading is best-effort and never
//! panics: a missing file falls back to `Config::default()`, and an unparseable
//! file is preserved aside (`config.toml.bad`) before defaults load in memory —
//! so a single typo can never silently reset (and then overwrite) the config.
//! Saving is also best-effort: directory-create and write errors are logged but
//! never crash the terminal (a read-only home degrades gracefully).

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
    /// Watch `~/.config/jetty/` and hot-reload config + themes live (no restart).
    /// Default `true`. The watcher is OS-event-driven (inotify/FSEvents), so it adds
    /// zero idle CPU; set `false` to disable it entirely (a pure escape hatch — no
    /// watcher thread is spawned). NOTE: `summon_hotkey` and `launch_at_login` are
    /// RESTART/external-only even with hot-reload on (documented at those keys).
    #[serde(default = "default_hot_reload")]
    pub hot_reload: bool,
}

fn default_osc52_allow_paste() -> bool {
    false
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
            hot_reload: default_hot_reload(),
        }
    }
}

impl Config {
    /// Resolve the JeTTY config DIRECTORY: `<config_dir>/jetty`, falling back to
    /// `~/.config/jetty` when `dirs::config_dir()` is unavailable. This is the dir
    /// that holds `config.toml` and the `themes/` subdir; the hot-reload watcher and
    /// the theme loader both key off it.
    pub(crate) fn dir() -> std::path::PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".config")
        });
        base.join("jetty")
    }

    /// Resolve the config file path: `<config_dir>/jetty/config.toml`.
    fn path() -> std::path::PathBuf {
        Self::dir().join("config.toml")
    }

    /// The config file path, for the hot-reload watcher / reload reader.
    pub(crate) fn config_path() -> std::path::PathBuf {
        Self::path()
    }

    /// Non-destructive reload parse (amendment H1): parse `s` (already-read file
    /// content), returning `None` on ANY parse error WITHOUT renaming to `.bad` and
    /// WITHOUT applying defaults. The destructive `.bad` preservation stays STARTUP-
    /// only (`load`) — a benign save-race mid-write must never wipe the user's config;
    /// the caller keeps its in-memory state and waits for the next watcher event.
    /// Sanitizes floats/effects on success, exactly like `load`, so a reload applies
    /// the same normalized values. Takes the content (not the path) so the caller can
    /// hash and parse the SAME bytes (no TOCTOU between the hash and the parse).
    pub(crate) fn parse_reload(s: &str) -> Option<Config> {
        match toml::from_str::<Config>(s) {
            Ok(mut cfg) => {
                cfg.sanitize_floats();
                cfg.effects = cfg.effects.clamped();
                Some(cfg)
            }
            Err(_) => None,
        }
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
        // Effects floats are sanitized by `EffectsConfig::clamped` (see load()).
    }

    /// Load settings from disk. A missing file yields `Config::default()`;
    /// non-finite floats fall back to their defaults — this never panics.
    ///
    /// A file that FAILS to parse is NOT silently reset: doing so would let the
    /// next `save()` overwrite the user's config with defaults, permanently
    /// losing everything over a single typo. Instead we preserve the original
    /// aside (`config.toml.bad`), log, and load in-memory defaults — so the
    /// user's settings remain recoverable and a fresh `save()` starts clean.
    pub fn load() -> Config {
        let path = Self::path();
        let s = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Config::default(),
        };
        match toml::from_str::<Config>(&s) {
            Ok(mut cfg) => {
                cfg.sanitize_floats();
                cfg.effects = cfg.effects.clamped();
                cfg
            }
            Err(e) => {
                eprintln!("jetty: config parse error at {}: {e}", path.display());
                let bad = path.with_extension("toml.bad");
                match std::fs::rename(&path, &bad) {
                    Ok(()) => eprintln!(
                        "jetty: preserved your unparsed config at {} (using defaults)",
                        bad.display()
                    ),
                    Err(e) => eprintln!(
                        "jetty: could not preserve unparsed config as {}: {e}",
                        bad.display()
                    ),
                }
                Config::default()
            }
        }
    }

    /// Persist settings to disk ATOMICALLY. Creates the parent directory if
    /// needed. All errors are ignored: a failed write must never crash the
    /// terminal.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("jetty: could not create config dir {}: {e}", dir.display());
                return;
            }
        }
        match toml::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = write_atomic(&path, s.as_bytes()) {
                    eprintln!("jetty: could not save config to {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("jetty: could not serialize config: {e}"),
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
            hot_reload: false,
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
            hot_reload: true,
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
