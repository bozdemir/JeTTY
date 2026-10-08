//! Post-processing glue shared by the main window, detached windows and
//! `jetty-shot`: `[effects]` → the CRT pass's settings ([`crt_settings`], the
//! ONE place a setting reaches the uniforms through `CrtParams::build`), the
//! effect presets ([`effect_presets`], [`matches_preset`]), animation pacing
//! ([`anim_step`]) and the event glitch ([`Glitch`]). Everything here is pure —
//! no window, no GPU — so it is unit-tested directly.

use std::time::{Duration, Instant};

pub use crate::config::{EffectsConfig, PhosphorMode};
use jetty_render::{CrtKey, CrtSettings, Phosphor};

/// `[effects]` → the CRT pass's settings (as if CRT were on; see
/// [`frame_settings`] for what actually runs).
pub fn crt_settings(fx: &EffectsConfig) -> CrtSettings {
    CrtSettings {
        curvature: fx.crt_curvature,
        scanline: fx.crt_scanline,
        mask: fx.crt_mask,
        bloom: fx.crt_bloom,
        bloom_radius: fx.crt_bloom_radius,
        chromatic: fx.crt_chromatic,
        vignette: fx.crt_vignette,
        tint: fx.crt_scanline_tint,
        roll: fx.crt_animate_roll,
        flicker: fx.crt_flicker,
        jitter: fx.crt_jitter,
        phosphor: fx
            .crt_phosphor
            .colors(fx.crt_phosphor_color)
            .map(|(ink, paper)| Phosphor { ink, paper, hue: fx.crt_phosphor_hue }),
        grain: fx.crt_grain,
        grain_animate: fx.crt_grain_animate,
        dither: fx.crt_dither,
        glitch: fx.glitch_enabled(),
    }
}

/// The `[effects]` table of config.toml (`$JETTY_CONFIG_DIR`-aware, sanitized
/// like the app's own load) — for the tools (`jetty-shot`, `jetty-bench`).
pub fn configured_effects() -> EffectsConfig {
    crate::config::Config::load().cfg.effects
}

/// What the post pass runs THIS frame: CRT when it is on; else, while a glitch
/// burst plays, the glitch-only passthrough; else nothing (the scene goes
/// straight to the surface).
pub fn frame_settings(fx: &EffectsConfig, glitch_burst: bool) -> Option<CrtSettings> {
    if fx.crt_enabled {
        Some(crt_settings(fx))
    } else if glitch_burst && fx.glitch_enabled() {
        Some(CrtSettings::GLITCH_ONLY)
    } else {
        None
    }
}

/// The one pipeline variant to keep built for these settings — prepared when the
/// settings change, so neither a slider frame nor a glitch burst compiles a
/// shader: CRT's own variant; else the glitch-only one while a glitch trigger
/// is enabled; else none (CRT off costs nothing).
pub fn prepared_key(fx: &EffectsConfig) -> Option<CrtKey> {
    if fx.crt_enabled {
        Some(crt_settings(fx).key())
    } else if fx.glitch_enabled() {
        Some(CrtSettings::GLITCH_ONLY.key())
    } else {
        None
    }
}

// ── Effect presets ──────────────────────────────────────────────────────────

/// A partial `[effects]`: the keys a preset writes (`None` = left alone). A
/// preset is a macro — no preset name is stored; [`matches_preset`] recognizes
/// one from the values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EffectsPatch {
    pub crt_enabled: Option<bool>,
    pub crt_curvature: Option<f32>,
    pub crt_scanline: Option<f32>,
    pub crt_mask: Option<f32>,
    pub crt_bloom: Option<f32>,
    pub crt_bloom_radius: Option<f32>,
    pub crt_chromatic: Option<f32>,
    pub crt_vignette: Option<f32>,
    pub crt_phosphor: Option<PhosphorMode>,
    pub crt_phosphor_hue: Option<f32>,
    pub crt_grain: Option<f32>,
    pub crt_dither: Option<bool>,
    pub caret_glow_enabled: Option<bool>,
}

/// Float tolerance for recognizing a preset (sliders store full precision).
const PRESET_EPS: f32 = 0.005;

impl EffectsPatch {
    /// Write every key this patch sets into `fx`.
    pub fn apply_to(&self, fx: &mut EffectsConfig) {
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = self.$f { fx.$f = v; } )* };
        }
        set!(
            crt_enabled, crt_curvature, crt_scanline, crt_mask, crt_bloom, crt_bloom_radius,
            crt_chromatic, crt_vignette, crt_phosphor, crt_phosphor_hue, crt_grain, crt_dither,
            caret_glow_enabled
        );
    }

    /// Whether `fx` holds every value this patch sets (floats within ±0.005).
    pub fn matches(&self, fx: &EffectsConfig) -> bool {
        let f = |p: Option<f32>, v: f32| p.is_none_or(|p| (p - v).abs() <= PRESET_EPS);
        let e = |p: Option<bool>, v: bool| p.is_none_or(|p| p == v);
        e(self.crt_enabled, fx.crt_enabled)
            && f(self.crt_curvature, fx.crt_curvature)
            && f(self.crt_scanline, fx.crt_scanline)
            && f(self.crt_mask, fx.crt_mask)
            && f(self.crt_bloom, fx.crt_bloom)
            && f(self.crt_bloom_radius, fx.crt_bloom_radius)
            && f(self.crt_chromatic, fx.crt_chromatic)
            && f(self.crt_vignette, fx.crt_vignette)
            && self.crt_phosphor.is_none_or(|p| p == fx.crt_phosphor)
            && f(self.crt_phosphor_hue, fx.crt_phosphor_hue)
            && f(self.crt_grain, fx.crt_grain)
            && e(self.crt_dither, fx.crt_dither)
            && e(self.caret_glow_enabled, fx.caret_glow_enabled)
    }

    /// Every float this patch sets (for range checks).
    pub fn floats(&self) -> Vec<f32> {
        [
            self.crt_curvature,
            self.crt_scanline,
            self.crt_mask,
            self.crt_bloom,
            self.crt_bloom_radius,
            self.crt_chromatic,
            self.crt_vignette,
            self.crt_phosphor_hue,
            self.crt_grain,
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// One named effects preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectPreset {
    /// Stable id (palette / `JETTY_SHOT_PRESET`), snake_case.
    pub id: &'static str,
    /// Display name ("Effects preset: {name}", the Settings chips).
    pub name: &'static str,
    pub patch: EffectsPatch,
}

/// A CRT look: every CRT knob a preset defines, the rest neutral (0 / off), so
/// applying one never mixes with the previous look. Animation toggles, the
/// scanline tint, caret settings and the glitch triggers are left alone — no
/// preset turns on roll/flicker/jitter.
#[allow(clippy::too_many_arguments)]
const fn look(
    curvature: f32,
    scanline: f32,
    mask: f32,
    bloom: f32,
    bloom_radius: Option<f32>,
    chromatic: f32,
    vignette: f32,
    phosphor: PhosphorMode,
    hue: f32,
    grain: f32,
    dither: bool,
) -> EffectsPatch {
    EffectsPatch {
        crt_enabled: Some(true),
        crt_curvature: Some(curvature),
        crt_scanline: Some(scanline),
        crt_mask: Some(mask),
        crt_bloom: Some(bloom),
        crt_bloom_radius: bloom_radius,
        crt_chromatic: Some(chromatic),
        crt_vignette: Some(vignette),
        crt_phosphor: Some(phosphor),
        crt_phosphor_hue: Some(hue),
        crt_grain: Some(grain),
        crt_dither: Some(dither),
        caret_glow_enabled: None,
    }
}

const PRESETS: [EffectPreset; 7] = [
    EffectPreset {
        id: "clean",
        name: "Clean",
        patch: EffectsPatch {
            crt_enabled: Some(false),
            crt_curvature: None,
            crt_scanline: None,
            crt_mask: None,
            crt_bloom: None,
            crt_bloom_radius: None,
            crt_chromatic: None,
            crt_vignette: None,
            crt_phosphor: None,
            crt_phosphor_hue: None,
            crt_grain: None,
            crt_dither: None,
            caret_glow_enabled: None,
        },
    },
    EffectPreset {
        id: "retro_crt",
        name: "Retro CRT",
        patch: look(0.25, 0.55, 0.35, 0.45, Some(0.3), 0.20, 0.45, PhosphorMode::Off, 0.0, 0.10, false),
    },
    EffectPreset {
        id: "amber",
        name: "Amber",
        patch: look(0.15, 0.35, 0.0, 0.50, Some(0.5), 0.0, 0.35, PhosphorMode::Amber, 0.0, 0.08, false),
    },
    EffectPreset {
        id: "green_phosphor",
        name: "Green Phosphor",
        patch: look(0.15, 0.40, 0.0, 0.55, Some(0.6), 0.0, 0.35, PhosphorMode::Green, 0.10, 0.08, false),
    },
    EffectPreset {
        id: "neon",
        name: "Neon",
        patch: EffectsPatch {
            caret_glow_enabled: Some(true),
            ..look(0.0, 0.15, 0.0, 0.80, Some(0.8), 0.15, 0.30, PhosphorMode::Off, 0.0, 0.0, false)
        },
    },
    EffectPreset {
        id: "paper",
        name: "Paper",
        patch: look(0.0, 0.0, 0.0, 0.0, None, 0.0, 0.15, PhosphorMode::Paper, 0.25, 0.15, false),
    },
    EffectPreset {
        id: "e_ink",
        name: "E-ink",
        patch: look(0.0, 0.0, 0.0, 0.0, None, 0.0, 0.0, PhosphorMode::Paper, 0.0, 0.0, true),
    },
];

/// Every effects preset, in chip / palette order: Clean, Retro CRT, Amber,
/// Green Phosphor, Neon, Paper, E-ink.
pub fn effect_presets() -> &'static [EffectPreset] {
    &PRESETS
}

/// The preset with this id or display name (case-insensitive), if any.
pub fn find_preset(name: &str) -> Option<&'static EffectPreset> {
    let n = name.trim();
    PRESETS.iter().find(|p| p.id.eq_ignore_ascii_case(n) || p.name.eq_ignore_ascii_case(n))
}

/// Whether `fx` currently shows `preset` (every key it writes holds its value).
pub fn matches_preset(fx: &EffectsConfig, preset: &EffectPreset) -> bool {
    preset.patch.matches(fx)
}

/// The preset `fx` currently matches, if any (else the look is "Custom").
pub fn active_preset(fx: &EffectsConfig) -> Option<&'static EffectPreset> {
    PRESETS.iter().find(|p| matches_preset(fx, p))
}

// ── Animation pacing ────────────────────────────────────────────────────────

/// CRT animations (roll/flicker/jitter, animated grain, a glitch burst) repaint
/// at most this often — a timed wake, never `Poll` at the display rate…
pub const ANIM_FPS: f64 = 30.0;
/// …and half that on a CPU (software) adapter, where a frame is expensive.
pub const CPU_ANIM_FPS: f64 = 15.0;

/// The paced animation interval for this adapter.
pub fn anim_interval(cpu_adapter: bool) -> Duration {
    Duration::from_secs_f64(1.0 / if cpu_adapter { CPU_ANIM_FPS } else { ANIM_FPS })
}

/// Whether a CONTINUOUS effect animation may run in a window: always while it
/// (or the Settings window previewing it) has focus; when unfocused only with
/// `animate_unfocused`. Bounded event bursts (the glitch) ignore this.
pub fn continuous_anim_allowed(focused: bool, animate_unfocused: bool) -> bool {
    focused || animate_unfocused
}

/// One paced-animation decision for a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimWake {
    /// Nothing animates (or the window can't show it): no wake at all.
    Idle,
    /// A frame is due: paint now.
    PaintNow,
    /// The next frame is due at this instant: one timed wake.
    At(Instant),
}

/// Pace an animating window: paint when `interval` has passed since its last
/// presented frame (`last_paint`), else wake once when it will have. A window
/// that cannot animate (hidden, occluded, no GPU, acquire retry pending) or
/// has nothing animating stays idle.
pub fn anim_step(
    animating: bool,
    can_animate: bool,
    last_paint: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> AnimWake {
    if !animating || !can_animate {
        return AnimWake::Idle;
    }
    match last_paint.map(|t| t + interval) {
        Some(due) if now < due => AnimWake::At(due),
        _ => AnimWake::PaintNow,
    }
}

// ── Event glitch ────────────────────────────────────────────────────────────

/// A glitch burst lasts this long…
pub const GLITCH_DURATION: Duration = Duration::from_millis(200);
/// …and starts at most once per this interval (a bell storm is one glitch).
pub const GLITCH_MIN_GAP: Duration = Duration::from_secs(1);

/// One window's event glitch: a bounded burst (`Option<Instant>` that expires
/// on the wall clock) plus the rate limiter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Glitch {
    /// Start of the burst in flight, if any.
    pub started: Option<Instant>,
    /// Start of the last burst (rate limit).
    last: Option<Instant>,
}

impl Glitch {
    /// Start a burst at `now` unless one started less than [`GLITCH_MIN_GAP`]
    /// ago. Returns whether it started.
    pub fn trigger(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|t| now.saturating_duration_since(t) < GLITCH_MIN_GAP) {
            return false;
        }
        self.last = Some(now);
        self.started = Some(now);
        true
    }

    /// Whether a burst is in flight at `now`.
    pub fn active(&self, now: Instant) -> bool {
        self.started.is_some_and(|s| now.saturating_duration_since(s) < GLITCH_DURATION)
    }

    /// The burst's intensity at `now` (0 when none).
    pub fn intensity(&self, now: Instant) -> f32 {
        self.started.map_or(0.0, |s| glitch_intensity(now.saturating_duration_since(s)))
    }

    /// When the burst in flight ends (the caller's one wake for the clean frame).
    pub fn ends_at(&self) -> Option<Instant> {
        self.started.map(|s| s + GLITCH_DURATION)
    }

    /// Drop a burst that is over; `true` when one just ended (paint once more so
    /// the last glitched frame does not stay on screen).
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.started.is_some() && !self.active(now) {
            self.started = None;
            return true;
        }
        false
    }

    /// Stop a burst (hide paths); the rate limit is kept.
    pub fn cancel(&mut self) {
        self.started = None;
    }
}

/// Glitch intensity `elapsed` into a burst: a hard hit that decays fast, 0 at
/// and after [`GLITCH_DURATION`].
pub fn glitch_intensity(elapsed: Duration) -> f32 {
    let t = elapsed.as_secs_f32() / GLITCH_DURATION.as_secs_f32();
    if !(0.0..1.0).contains(&t) {
        return 0.0;
    }
    1.0 - t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crt_settings_mirrors_the_config() {
        let fx = EffectsConfig {
            crt_curvature: 0.1,
            crt_scanline: 0.2,
            crt_mask: 0.3,
            crt_bloom: 0.4,
            crt_bloom_radius: 0.5,
            crt_chromatic: 0.6,
            crt_vignette: 0.7,
            crt_scanline_tint: [0.5, 0.6, 0.7],
            crt_animate_roll: true,
            crt_phosphor: PhosphorMode::Green,
            crt_phosphor_hue: 0.25,
            crt_grain: 0.15,
            crt_grain_animate: true,
            crt_dither: true,
            glitch_on_bell: true,
            ..EffectsConfig::default()
        };
        let s = crt_settings(&fx);
        assert_eq!(
            (s.curvature, s.scanline, s.mask, s.bloom, s.bloom_radius, s.chromatic, s.vignette),
            (0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7)
        );
        assert_eq!(s.tint, [0.5, 0.6, 0.7]);
        assert!(s.roll && !s.flicker && !s.jitter);
        let ph = s.phosphor.expect("green phosphor");
        assert_eq!(ph.hue, 0.25);
        assert_eq!(Some((ph.ink, ph.paper)), PhosphorMode::Green.colors(fx.crt_phosphor_color));
        assert_eq!((s.grain, s.grain_animate, s.dither, s.glitch), (0.15, true, true, true));
        assert!(crt_settings(&EffectsConfig::default()).phosphor.is_none());
    }

    /// CRT off costs nothing (no variant); a glitch trigger alone keeps the
    /// glitch-only variant built; CRT on keeps its own (with the glitch inside).
    #[test]
    fn prepared_key_follows_crt_and_glitch() {
        let mut fx = EffectsConfig::default();
        assert_eq!(prepared_key(&fx), None);
        assert_eq!(frame_settings(&fx, true), None, "no trigger → never a glitch pass");
        fx.glitch_on_error = true;
        assert_eq!(prepared_key(&fx), Some(CrtKey::GLITCH));
        assert_eq!(frame_settings(&fx, false), None);
        assert_eq!(frame_settings(&fx, true), Some(CrtSettings::GLITCH_ONLY));
        fx.crt_enabled = true;
        let key = prepared_key(&fx).unwrap();
        assert!(key.contains(CrtKey::GLITCH) && key.contains(CrtKey::SCAN) && key.contains(CrtKey::BLOOM));
        assert_eq!(frame_settings(&fx, false).map(|s| s.key()), Some(key));
        fx.glitch_on_error = false;
        assert!(!prepared_key(&fx).unwrap().contains(CrtKey::GLITCH));
    }

    #[test]
    fn crt_anim_live_includes_animated_grain() {
        let mut fx = EffectsConfig { crt_enabled: true, ..EffectsConfig::default() };
        assert!(!fx.crt_anim_live(), "static CRT stays idle");
        fx.crt_grain_animate = true;
        assert!(!fx.crt_anim_live(), "no grain → nothing to animate");
        fx.crt_grain = 0.1;
        assert!(fx.crt_anim_live());
        fx.crt_enabled = false;
        assert!(!fx.crt_anim_live(), "CRT off never animates");
    }

    /// Every preset's values are in their slider ranges (so `clamped` leaves
    /// them alone), names/ids are unique, and they are in chip order.
    #[test]
    fn presets_are_in_range_and_unique() {
        let names: Vec<&str> = effect_presets().iter().map(|p| p.name).collect();
        assert_eq!(names, ["Clean", "Retro CRT", "Amber", "Green Phosphor", "Neon", "Paper", "E-ink"]);
        for (i, p) in effect_presets().iter().enumerate() {
            for v in p.patch.floats() {
                assert!((0.0..=1.0).contains(&v), "{}: {v} out of range", p.name);
            }
            for q in &effect_presets()[i + 1..] {
                assert_ne!(p.id, q.id);
                assert_ne!(p.name, q.name);
            }
            let mut fx = EffectsConfig::default();
            p.patch.apply_to(&mut fx);
            assert_eq!(fx.clone().clamped(), fx, "{}: applying stays in range", p.name);
            assert!(p.id.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{}", p.id);
        }
        // No preset turns on an animation.
        for p in effect_presets() {
            let mut fx = EffectsConfig::default();
            p.patch.apply_to(&mut fx);
            assert!(!fx.crt_anim_live(), "{} animates", p.name);
        }
    }

    /// Apply → matches, from the defaults AND from every other preset (a preset
    /// fully replaces the previous look), and exactly one preset is lit.
    #[test]
    fn presets_round_trip_through_matches_preset() {
        let starts: Vec<EffectsConfig> = std::iter::once(EffectsConfig::default())
            .chain(effect_presets().iter().map(|p| {
                let mut fx = EffectsConfig { crt_bloom_radius: 0.9, ..EffectsConfig::default() };
                p.patch.apply_to(&mut fx);
                fx
            }))
            .collect();
        for start in &starts {
            for p in effect_presets() {
                let mut fx = start.clone();
                p.patch.apply_to(&mut fx);
                assert!(matches_preset(&fx, p), "{} must match after applying it", p.name);
                assert_eq!(active_preset(&fx).map(|a| a.id), Some(p.id), "{} is the one lit", p.name);
                for q in effect_presets().iter().filter(|q| q.id != p.id) {
                    assert!(!matches_preset(&fx, q), "{} also matches {}", p.name, q.name);
                }
            }
        }
    }

    #[test]
    fn matches_preset_tolerates_slider_precision_only() {
        let retro = find_preset("retro_crt").unwrap();
        let mut fx = EffectsConfig::default();
        retro.patch.apply_to(&mut fx);
        fx.crt_scanline += 0.004;
        assert!(matches_preset(&fx, retro), "a slider's float noise still matches");
        fx.crt_scanline += 0.02;
        assert!(!matches_preset(&fx, retro));
        assert!(active_preset(&fx).is_none(), "an edited look is Custom");
        // Clean = CRT off, whatever the sliders say.
        let mut fx = EffectsConfig { crt_scanline: 0.9, ..EffectsConfig::default() };
        assert_eq!(active_preset(&fx).map(|p| p.id), Some("clean"));
        fx.crt_enabled = true;
        assert_ne!(active_preset(&fx).map(|p| p.id), Some("clean"));
    }

    #[test]
    fn find_preset_by_id_or_name() {
        assert_eq!(find_preset("E-ink").map(|p| p.id), Some("e_ink"));
        assert_eq!(find_preset("green_phosphor").map(|p| p.name), Some("Green Phosphor"));
        assert_eq!(find_preset(" neon ").map(|p| p.id), Some("neon"));
        assert!(find_preset("vaporwave").is_none());
    }

    /// The Neon preset adds the caret glow; the paper looks use the paper mode.
    #[test]
    fn preset_details() {
        let mut fx = EffectsConfig::default();
        find_preset("neon").unwrap().patch.apply_to(&mut fx);
        assert!(fx.caret_glow_enabled && fx.crt_enabled);
        let mut fx = EffectsConfig::default();
        find_preset("e_ink").unwrap().patch.apply_to(&mut fx);
        assert!(fx.crt_dither && fx.crt_phosphor == PhosphorMode::Paper && fx.crt_phosphor_hue == 0.0);
        let s = crt_settings(&fx);
        assert_eq!(s.key(), CrtKey::PHOS | CrtKey::DITHER, "e-ink compiles only phosphor + dither");
        let mut fx = EffectsConfig::default();
        find_preset("clean").unwrap().patch.apply_to(&mut fx);
        assert_eq!(fx, EffectsConfig::default(), "Clean on the defaults changes nothing");
    }

    #[test]
    fn phosphor_modes() {
        assert!(PhosphorMode::Off.colors([1.0; 3]).is_none());
        let (ink, paper) = PhosphorMode::Paper.colors([1.0; 3]).unwrap();
        assert!(ink.iter().all(|&c| c < 0.1) && paper.iter().all(|&c| c > 0.9), "dark ink on light paper");
        let (ink, _) = PhosphorMode::Custom.colors([0.2, 2.0, f32::NAN]).unwrap();
        assert_eq!(ink, [0.2, 1.0, 1.0], "custom color sanitized");
        let (ink, paper) = PhosphorMode::Amber.colors([0.0; 3]).unwrap();
        assert_eq!(ink, [1.0, 176.0 / 255.0, 0.0], "amber is #FFB000");
        assert!(paper.iter().all(|&c| c < 0.1));
        // The cycler visits every mode once.
        let mut m = PhosphorMode::Off;
        for _ in 0..PhosphorMode::ALL.len() {
            m = m.next();
        }
        assert_eq!(m, PhosphorMode::Off);
        // Config round trip of the lowercase names, and a bad value is rejected.
        for m in PhosphorMode::ALL {
            let fx = EffectsConfig { crt_phosphor: m, ..EffectsConfig::default() };
            let s = toml::to_string(&fx).unwrap();
            assert!(s.contains(&format!("crt_phosphor = \"{}\"", m.display_name().to_lowercase())), "{s}");
            assert_eq!(toml::from_str::<EffectsConfig>(&s).unwrap(), fx);
        }
        assert!(toml::from_str::<EffectsConfig>("crt_phosphor = \"purple\"").is_err());
    }

    #[test]
    fn new_keys_default_off_and_clamp() {
        let d = EffectsConfig::default();
        assert_eq!(d.crt_phosphor, PhosphorMode::Off);
        assert_eq!((d.crt_grain, d.crt_phosphor_hue), (0.0, 0.0));
        assert!(!d.crt_dither && !d.crt_grain_animate && !d.glitch_on_error && !d.glitch_on_bell);
        assert!(!d.animate_unfocused);
        let e = EffectsConfig {
            crt_bloom_radius: 4.0,
            crt_phosphor_hue: -1.0,
            crt_grain: f32::NAN,
            crt_phosphor_color: [2.0, -1.0, f32::INFINITY],
            ..EffectsConfig::default()
        }
        .clamped();
        assert_eq!((e.crt_bloom_radius, e.crt_phosphor_hue, e.crt_grain), (1.0, 0.0, 0.0));
        assert_eq!(e.crt_phosphor_color, [1.0, 0.0, 0.0]);
        // An old config without the keys loads with these defaults.
        let old: EffectsConfig = toml::from_str("crt_enabled = true\ncrt_bloom = 0.2").unwrap();
        assert_eq!(old.crt_bloom_radius, d.crt_bloom_radius);
        assert_eq!(old.crt_phosphor, PhosphorMode::Off);
    }

    // ── pacing ──

    #[test]
    fn anim_intervals() {
        assert_eq!(anim_interval(false), Duration::from_secs_f64(1.0 / 30.0));
        assert_eq!(anim_interval(true), Duration::from_secs_f64(1.0 / 15.0));
        assert!(anim_interval(true) > anim_interval(false));
    }

    #[test]
    fn anim_step_paces_with_timed_wakes() {
        let now = Instant::now();
        let iv = anim_interval(false);
        assert_eq!(anim_step(false, true, None, now, iv), AnimWake::Idle, "nothing animates");
        assert_eq!(anim_step(true, false, None, now, iv), AnimWake::Idle, "hidden/occluded window");
        assert_eq!(anim_step(true, true, None, now, iv), AnimWake::PaintNow, "first frame");
        let just = now - Duration::from_millis(5);
        assert_eq!(anim_step(true, true, Some(just), now, iv), AnimWake::At(just + iv), "one timed wake");
        let old = now - Duration::from_millis(40);
        assert_eq!(anim_step(true, true, Some(old), now, iv), AnimWake::PaintNow, "overdue");
        // Exactly due paints now (a WaitUntil in the past would spin).
        assert_eq!(anim_step(true, true, Some(now - iv), now, iv), AnimWake::PaintNow);
        // A CPU adapter waits twice as long.
        let cpu = anim_interval(true);
        assert_eq!(anim_step(true, true, Some(old), now, cpu), AnimWake::At(old + cpu));
    }

    #[test]
    fn continuous_animation_pauses_unfocused() {
        assert!(continuous_anim_allowed(true, false));
        assert!(!continuous_anim_allowed(false, false), "paused when unfocused by default");
        assert!(continuous_anim_allowed(false, true), "animate_unfocused keeps it running");
    }

    // ── glitch ──

    #[test]
    fn glitch_is_rate_limited_to_one_per_second() {
        let t0 = Instant::now();
        let mut g = Glitch::default();
        assert!(g.trigger(t0));
        assert!(!g.trigger(t0 + Duration::from_millis(100)), "inside the burst");
        assert!(!g.trigger(t0 + Duration::from_millis(999)), "inside the 1 s gap");
        assert!(g.trigger(t0 + Duration::from_millis(1000)), "the gap elapsed");
        // A refused trigger does not restart the gap.
        let t1 = t0 + Duration::from_millis(1000);
        assert!(!g.trigger(t1 + Duration::from_millis(500)));
        assert!(g.trigger(t1 + Duration::from_millis(1000)));
        // Cancel (hide) stops the burst but keeps the limit.
        let t2 = t1 + Duration::from_millis(1000);
        g.cancel();
        assert!(!g.active(t2));
        assert!(!g.trigger(t2 + Duration::from_millis(10)));
    }

    #[test]
    fn glitch_burst_is_bounded() {
        let t0 = Instant::now();
        let mut g = Glitch::default();
        assert_eq!(g.intensity(t0), 0.0);
        assert!(g.ends_at().is_none());
        g.trigger(t0);
        assert!(g.active(t0));
        assert_eq!(g.intensity(t0), 1.0);
        assert!(g.intensity(t0 + Duration::from_millis(100)) < 1.0);
        assert!(g.intensity(t0 + Duration::from_millis(100)) > 0.0);
        assert_eq!(g.ends_at(), Some(t0 + GLITCH_DURATION));
        assert!(!g.expire(t0 + Duration::from_millis(150)), "still running");
        let end = t0 + GLITCH_DURATION;
        assert!(!g.active(end));
        assert_eq!(g.intensity(end), 0.0);
        assert!(g.expire(end), "just ended → one clean repaint");
        assert!(!g.expire(end), "only once");
        assert!(g.started.is_none());
        assert_eq!(glitch_intensity(Duration::from_secs(5)), 0.0);
    }
}
