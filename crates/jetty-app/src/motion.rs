//! Cursor & motion settings: the parsed `[cursor]` table and the pure decisions
//! behind reduce-motion, the visual bell and the command status pulse.
//!
//! Everything here is plain data and functions (unit-tested below); `app.rs`
//! only wires it to the event loop. Config values are kept as the user's
//! strings in `config.rs`; unknown words read as the default, so a typo never
//! fails a load.

use std::time::{Duration, Instant};

use jetty_core::CursorShapeSnap;
use jetty_render::{CursorColor, CursorStyle, UnderlineCursor, UnfocusedCursor};

/// Lower-case, trimmed, `-`/space → `_` — the spelling every parser matches.
fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// `[cursor] shape`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShapePref {
    #[default]
    Block,
    Beam,
    Underline,
    DoubleUnderline,
    /// The "vintage" tall underline bar.
    ThickUnderline,
}

impl CursorShapePref {
    pub fn parse(s: &str) -> Self {
        match norm(s).as_str() {
            "beam" | "bar" | "ibeam" | "i_beam" | "line" => CursorShapePref::Beam,
            "underline" => CursorShapePref::Underline,
            "double_underline" | "double" => CursorShapePref::DoubleUnderline,
            "thick_underline" | "thick" | "vintage" => CursorShapePref::ThickUnderline,
            _ => CursorShapePref::Block,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CursorShapePref::Block => "block",
            CursorShapePref::Beam => "beam",
            CursorShapePref::Underline => "underline",
            CursorShapePref::DoubleUnderline => "double_underline",
            CursorShapePref::ThickUnderline => "thick_underline",
        }
    }

    /// The DECSCUSR shape a fresh screen shows / programs reset to.
    pub fn terminal_shape(self) -> CursorShapeSnap {
        match self {
            CursorShapePref::Block => CursorShapeSnap::Block,
            CursorShapePref::Beam => CursorShapeSnap::Beam,
            CursorShapePref::Underline
            | CursorShapePref::DoubleUnderline
            | CursorShapePref::ThickUnderline => CursorShapeSnap::Underline,
        }
    }

    /// How every underline cursor is drawn (a program's DECSCUSR 3/4 too).
    pub fn underline(self) -> UnderlineCursor {
        match self {
            CursorShapePref::DoubleUnderline => UnderlineCursor::Double,
            CursorShapePref::ThickUnderline => UnderlineCursor::Thick,
            _ => UnderlineCursor::Single,
        }
    }
}

/// `[cursor] guide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuideMode {
    #[default]
    Off,
    /// Only on the primary screen (not in full-screen programs).
    Shell,
    Always,
}

impl GuideMode {
    pub fn parse(s: &str) -> Self {
        match norm(s).as_str() {
            "shell" | "prompt" => GuideMode::Shell,
            "always" | "on" | "true" => GuideMode::Always,
            _ => GuideMode::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GuideMode::Off => "off",
            GuideMode::Shell => "shell",
            GuideMode::Always => "always",
        }
    }

    /// Whether the band shows for a terminal on (`alt_screen`) or off the
    /// alternate screen.
    pub fn shows(self, alt_screen: bool) -> bool {
        match self {
            GuideMode::Off => false,
            GuideMode::Shell => !alt_screen,
            GuideMode::Always => true,
        }
    }
}

pub fn parse_unfocused(s: &str) -> UnfocusedCursor {
    match norm(s).as_str() {
        "unchanged" | "same" | "keep" => UnfocusedCursor::Unchanged,
        "none" | "hidden" | "hide" | "off" => UnfocusedCursor::None,
        _ => UnfocusedCursor::Hollow,
    }
}

pub fn unfocused_str(u: UnfocusedCursor) -> &'static str {
    match u {
        UnfocusedCursor::Hollow => "hollow",
        UnfocusedCursor::Unchanged => "unchanged",
        UnfocusedCursor::None => "none",
    }
}

pub fn parse_cursor_color(s: &str) -> CursorColor {
    match norm(s).as_str() {
        "cell" | "reverse" | "inverse" => CursorColor::Cell,
        "auto" => CursorColor::Auto,
        _ => CursorColor::Theme,
    }
}

pub fn cursor_color_str(c: CursorColor) -> &'static str {
    match c {
        CursorColor::Theme => "theme",
        CursorColor::Cell => "cell",
        CursorColor::Auto => "auto",
    }
}

/// The render-side look for a `[cursor]` table.
pub(crate) fn cursor_style(cfg: &crate::config::CursorConfig) -> CursorStyle {
    CursorStyle {
        thickness: cfg.thickness,
        unfocused: parse_unfocused(&cfg.unfocused),
        underline: CursorShapePref::parse(&cfg.shape).underline(),
        color: parse_cursor_color(&cfg.color),
    }
}

/// The `[cursor]` table rewritten in canonical spellings (what Settings saves).
pub(crate) fn canonical_cursor(cfg: &crate::config::CursorConfig) -> crate::config::CursorConfig {
    crate::config::CursorConfig {
        shape: CursorShapePref::parse(&cfg.shape).as_str().to_string(),
        unfocused: unfocused_str(parse_unfocused(&cfg.unfocused)).to_string(),
        color: cursor_color_str(parse_cursor_color(&cfg.color)).to_string(),
        guide: GuideMode::parse(&cfg.guide).as_str().to_string(),
        ..cfg.clone()
    }
}

/// A whole `[cursor]` table parsed — what a window needs to draw the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorSpec {
    pub shape: CursorShapePref,
    pub style: CursorStyle,
    pub guide: GuideMode,
}

/// A compact `[cursor]` spelling for tools (jetty-shot's `JETTY_SHOT_CURSOR`):
/// comma-separated `key=value` pairs using the table's own keys, e.g.
/// `"shape=beam,thickness=0.2,color=auto,guide=always,unfocused=none"`.
/// Missing keys keep their defaults.
pub fn parse_cursor_spec(spec: &str) -> CursorSpec {
    let mut cfg = crate::config::CursorConfig::default();
    for part in spec.split(',') {
        let Some((k, v)) = part.split_once('=') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "shape" => cfg.shape = v,
            "thickness" => cfg.thickness = v.parse().unwrap_or(cfg.thickness),
            "unfocused" => cfg.unfocused = v,
            "color" => cfg.color = v,
            "guide" => cfg.guide = v,
            _ => {}
        }
    }
    cursor_spec(&cfg.clamped())
}

/// The parsed form of a `[cursor]` table.
pub(crate) fn cursor_spec(cfg: &crate::config::CursorConfig) -> CursorSpec {
    CursorSpec {
        shape: CursorShapePref::parse(&cfg.shape),
        style: cursor_style(cfg),
        guide: GuideMode::parse(&cfg.guide),
    }
}

/// `reduce_motion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReduceMotion {
    #[default]
    Off,
    On,
    /// Follow the desktop's reduced-motion setting.
    System,
}

impl ReduceMotion {
    pub fn parse(s: &str) -> Self {
        match norm(s).as_str() {
            "on" | "true" | "yes" | "reduce" => ReduceMotion::On,
            "system" | "auto" => ReduceMotion::System,
            _ => ReduceMotion::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReduceMotion::Off => "off",
            ReduceMotion::On => "on",
            ReduceMotion::System => "system",
        }
    }

    /// Whether motion is reduced, given the desktop's setting (`system`).
    pub fn active(self, system: bool) -> bool {
        match self {
            ReduceMotion::Off => false,
            ReduceMotion::On => true,
            ReduceMotion::System => system,
        }
    }
}

/// `visual_bell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisualBell {
    #[default]
    Off,
    /// A brief veil over the whole window.
    Flash,
    /// A brief glow along the window edge.
    Rim,
}

impl VisualBell {
    pub fn parse(s: &str) -> Self {
        match norm(s).as_str() {
            "flash" | "on" | "true" | "visual" => VisualBell::Flash,
            "rim" | "border" | "edge" => VisualBell::Rim,
            _ => VisualBell::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VisualBell::Off => "off",
            VisualBell::Flash => "flash",
            VisualBell::Rim => "rim",
        }
    }

    /// The bell actually played: a full-window flash becomes the rim while
    /// motion is reduced (a flash of the whole frame is the harsher one).
    pub fn effective(self, reduce_motion: bool) -> VisualBell {
        match (self, reduce_motion) {
            (VisualBell::Flash, true) => VisualBell::Rim,
            (b, _) => b,
        }
    }
}

/// How long one visual bell plays.
pub const BELL_SECS: f32 = 0.15;
/// The shortest gap between two visual bells (a `yes $'\a'` flood flashes at
/// most three times a second).
pub const BELL_MIN_GAP: Duration = Duration::from_millis(333);

/// `command_pulse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandPulse {
    #[default]
    Off,
    Failures,
    All,
}

/// Which pulse a finished command earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulseKind {
    /// A failed command (nonzero exit): red.
    Failure,
    /// A long command that succeeded (`command_pulse = "all"`): the accent.
    Success,
}

impl CommandPulse {
    pub fn parse(s: &str) -> Self {
        match norm(s).as_str() {
            "failures" | "failure" | "errors" | "error" | "on" | "true" => CommandPulse::Failures,
            "all" | "always" => CommandPulse::All,
            _ => CommandPulse::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CommandPulse::Off => "off",
            CommandPulse::Failures => "failures",
            CommandPulse::All => "all",
        }
    }

    /// The pulse a completion with `exit_code` / `duration` earns: a failure in
    /// both modes; under `"all"` also a success that ran at least `long` (an
    /// unknown duration — bash without preexec — never counts as long).
    pub fn pulse_for(self, exit_code: Option<i32>, duration: Option<Duration>, long: Duration) -> Option<PulseKind> {
        let failed = matches!(exit_code, Some(c) if c != 0);
        match self {
            CommandPulse::Off => None,
            _ if failed => Some(PulseKind::Failure),
            CommandPulse::All if duration.is_some_and(|d| d >= long) => Some(PulseKind::Success),
            _ => None,
        }
    }
}

/// How long one command pulse plays.
pub const PULSE_SECS: f32 = 0.4;

/// Envelope of the visual bell / command pulse at progress `t` (0..=1): a fast
/// attack to 1 at t = 0.15, then an ease-out release to exactly 0 at t = 1.
pub fn pulse_envelope(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    const ATTACK: f32 = 0.15;
    if t < ATTACK {
        let a = t / ATTACK;
        a * a * (3.0 - 2.0 * a)
    } else {
        let r = (1.0 - t) / (1.0 - ATTACK);
        r * r
    }
}

/// Accept at most one event per `gap`: `allow` is true (and records `now`)
/// only when the previous accepted event is at least `gap` old.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateLimit {
    last: Option<Instant>,
}

impl RateLimit {
    pub fn allow(&mut self, now: Instant, gap: Duration) -> bool {
        if self.last.is_some_and(|l| now.saturating_duration_since(l) < gap) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The summon reveal under reduce motion: a plain fade of at most this long
/// (no scan line, ripple, blur, drift or slide).
pub const REDUCED_SUMMON_SECS: f32 = 0.08;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_parses_leniently_and_round_trips() {
        for p in [
            CursorShapePref::Block,
            CursorShapePref::Beam,
            CursorShapePref::Underline,
            CursorShapePref::DoubleUnderline,
            CursorShapePref::ThickUnderline,
        ] {
            assert_eq!(CursorShapePref::parse(p.as_str()), p);
        }
        assert_eq!(CursorShapePref::parse(" Double-Underline "), CursorShapePref::DoubleUnderline);
        assert_eq!(CursorShapePref::parse("vintage"), CursorShapePref::ThickUnderline);
        assert_eq!(CursorShapePref::parse("bar"), CursorShapePref::Beam);
        assert_eq!(CursorShapePref::parse("nonsense"), CursorShapePref::Block);
    }

    #[test]
    fn underline_variants_feed_the_terminal_an_underline() {
        assert_eq!(CursorShapePref::DoubleUnderline.terminal_shape(), CursorShapeSnap::Underline);
        assert_eq!(CursorShapePref::ThickUnderline.terminal_shape(), CursorShapeSnap::Underline);
        assert_eq!(CursorShapePref::Beam.terminal_shape(), CursorShapeSnap::Beam);
        assert_eq!(CursorShapePref::DoubleUnderline.underline(), UnderlineCursor::Double);
        assert_eq!(CursorShapePref::ThickUnderline.underline(), UnderlineCursor::Thick);
        assert_eq!(CursorShapePref::Block.underline(), UnderlineCursor::Single);
    }

    #[test]
    fn guide_modes() {
        assert!(!GuideMode::Off.shows(false));
        assert!(GuideMode::Shell.shows(false));
        assert!(!GuideMode::Shell.shows(true), "full-screen programs draw their own");
        assert!(GuideMode::Always.shows(true));
        assert_eq!(GuideMode::parse("SHELL"), GuideMode::Shell);
        assert_eq!(GuideMode::parse("?"), GuideMode::Off);
    }

    #[test]
    fn default_cursor_table_is_the_old_cursor() {
        let cfg = crate::config::CursorConfig::default();
        assert_eq!(cursor_style(&cfg), CursorStyle::default());
        assert_eq!(CursorShapePref::parse(&cfg.shape), CursorShapePref::Block);
        assert_eq!(GuideMode::parse(&cfg.guide), GuideMode::Off);
        assert!(!cfg.trail);
    }

    #[test]
    fn canonical_cursor_normalizes_spellings_only() {
        let cfg = crate::config::CursorConfig {
            shape: "Vintage".into(),
            unfocused: "hide".into(),
            color: "reverse".into(),
            guide: "on".into(),
            thickness: 0.2,
            ..Default::default()
        };
        let c = canonical_cursor(&cfg);
        assert_eq!((c.shape.as_str(), c.unfocused.as_str()), ("thick_underline", "none"));
        assert_eq!((c.color.as_str(), c.guide.as_str()), ("cell", "always"));
        assert_eq!(c.thickness, 0.2);
    }

    #[test]
    fn cursor_spec_parses_the_table_keys() {
        let s = parse_cursor_spec("shape=thick_underline, thickness=0.2,color=auto,guide=shell,unfocused=none");
        assert_eq!(s.shape, CursorShapePref::ThickUnderline);
        assert_eq!(s.style.underline, UnderlineCursor::Thick);
        assert!((s.style.thickness - 0.2).abs() < 1e-6);
        assert_eq!(s.style.color, CursorColor::Auto);
        assert_eq!(s.style.unfocused, UnfocusedCursor::None);
        assert_eq!(s.guide, GuideMode::Shell);
        let d = parse_cursor_spec("");
        assert_eq!((d.shape, d.style, d.guide), (CursorShapePref::Block, CursorStyle::default(), GuideMode::Off));
        assert!((parse_cursor_spec("thickness=9").style.thickness - 0.5).abs() < 1e-6, "clamped");
    }

    #[test]
    fn reduce_motion_mapping() {
        assert!(!ReduceMotion::Off.active(true));
        assert!(ReduceMotion::On.active(false));
        assert!(ReduceMotion::System.active(true));
        assert!(!ReduceMotion::System.active(false));
        assert_eq!(ReduceMotion::parse("System"), ReduceMotion::System);
        assert_eq!(ReduceMotion::parse("true"), ReduceMotion::On);
        assert_eq!(ReduceMotion::parse("x"), ReduceMotion::Off);
        for m in [ReduceMotion::Off, ReduceMotion::On, ReduceMotion::System] {
            assert_eq!(ReduceMotion::parse(m.as_str()), m);
        }
    }

    #[test]
    fn a_reduced_bell_is_the_rim() {
        assert_eq!(VisualBell::Flash.effective(true), VisualBell::Rim);
        assert_eq!(VisualBell::Flash.effective(false), VisualBell::Flash);
        assert_eq!(VisualBell::Rim.effective(true), VisualBell::Rim);
        assert_eq!(VisualBell::Off.effective(true), VisualBell::Off);
        for b in [VisualBell::Off, VisualBell::Flash, VisualBell::Rim] {
            assert_eq!(VisualBell::parse(b.as_str()), b);
        }
    }

    #[test]
    fn pulse_kinds() {
        let long = Duration::from_secs(10);
        let p = CommandPulse::Failures;
        assert_eq!(p.pulse_for(Some(1), Some(Duration::from_secs(1)), long), Some(PulseKind::Failure));
        assert_eq!(p.pulse_for(Some(0), Some(Duration::from_secs(60)), long), None, "failures only");
        assert_eq!(p.pulse_for(None, None, long), None, "unknown exit = not a failure");
        let a = CommandPulse::All;
        assert_eq!(a.pulse_for(Some(0), Some(Duration::from_secs(60)), long), Some(PulseKind::Success));
        assert_eq!(a.pulse_for(Some(0), Some(Duration::from_secs(2)), long), None, "short success");
        assert_eq!(a.pulse_for(Some(0), None, long), None, "unknown duration is not long");
        assert_eq!(a.pulse_for(Some(2), None, long), Some(PulseKind::Failure));
        assert_eq!(CommandPulse::Off.pulse_for(Some(1), None, long), None);
        for m in [CommandPulse::Off, CommandPulse::Failures, CommandPulse::All] {
            assert_eq!(CommandPulse::parse(m.as_str()), m);
        }
    }

    #[test]
    fn envelope_rises_fast_and_ends_at_zero() {
        assert_eq!(pulse_envelope(0.0), 0.0);
        assert!((pulse_envelope(0.15) - 1.0).abs() < 1e-6);
        assert_eq!(pulse_envelope(1.0), 0.0);
        assert!(pulse_envelope(0.05) > 0.0 && pulse_envelope(0.05) < 1.0);
        // Monotonic release.
        let mut prev = 1.0;
        for k in 15..=100 {
            let v = pulse_envelope(k as f32 / 100.0);
            assert!(v <= prev + 1e-6);
            prev = v;
        }
        // Out-of-range input clamps.
        assert_eq!(pulse_envelope(-1.0), 0.0);
        assert_eq!(pulse_envelope(2.0), 0.0);
    }

    #[test]
    fn rate_limit_spaces_events() {
        let t0 = Instant::now();
        let mut r = RateLimit::default();
        assert!(r.allow(t0, BELL_MIN_GAP));
        assert!(!r.allow(t0 + Duration::from_millis(100), BELL_MIN_GAP));
        assert!(!r.allow(t0 + Duration::from_millis(332), BELL_MIN_GAP));
        assert!(r.allow(t0 + Duration::from_millis(333), BELL_MIN_GAP));
        // A rejected event never resets the window.
        assert!(!r.allow(t0 + Duration::from_millis(400), BELL_MIN_GAP));
        assert!(r.allow(t0 + Duration::from_millis(700), BELL_MIN_GAP));
    }
}
