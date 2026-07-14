use std::io::Write;
use std::sync::Arc;
use jetty_core::{PtySession, Terminal};
use jetty_render::{GpuContext, QuadLayer, TextLayer};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::event::MouseScrollDelta;
use winit::window::{Window, WindowId};
use crate::{clipboard, input};

/// Events sent through the winit user-event channel.
#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    /// PTY data is ready — drain and redraw.
    Wake,
    /// Summon hotkey / `jetty --toggle` — toggle window visibility.
    ToggleVisibility,
    /// `jetty --show` / `--hide` — set window visibility explicitly.
    SetVisible(bool),
    /// A watched config/theme file changed (from the `notify` watcher). Debounced
    /// and applied from `about_to_wait`; carries no payload (the reload re-reads).
    ConfigChanged,
}

/// Window-summon reveal effect, selectable in Settings and persisted in config.
/// A clean dispatch a follow-up can extend with Tier-B (offscreen-texture)
/// effects. Each variant is self-contained — our own wgpu/WGSL, no
/// desktop-environment / compositor / OS-specific code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummonEffect {
    /// No reveal — the window simply appears (animation ends immediately).
    None,
    /// Bayer Crystallize — the original subtle 1px ordered-dither reveal.
    Bayer,
    /// Phosphor Ignition — CRT-style power-on (descending scan + accent rim).
    Phosphor,
    /// Liquid Drop — Tier-B radial refraction ring that samples the frame.
    Liquid,
    /// Focus Pull — Tier-B rack-focus blur + chromatic that samples the frame.
    Focus,
}

impl SummonEffect {
    /// Cycle order for the ‹ / › settings buttons.
    const ORDER: [SummonEffect; 5] = [
        SummonEffect::None,
        SummonEffect::Bayer,
        SummonEffect::Phosphor,
        SummonEffect::Liquid,
        SummonEffect::Focus,
    ];

    /// Whether this is a Tier-B effect: one that SAMPLES the rendered frame from
    /// an offscreen texture (Liquid/Focus). Tier-A effects (None/Bayer/Phosphor)
    /// render straight to the surface, so the normal hot path is untouched.
    fn is_tier_b(self) -> bool {
        matches!(self, SummonEffect::Liquid | SummonEffect::Focus)
    }

    /// Animation duration in seconds for this effect.
    fn duration(self) -> f32 {
        match self {
            SummonEffect::None => 0.0,
            SummonEffect::Bayer => 0.20,
            SummonEffect::Phosphor => 0.25,
            SummonEffect::Liquid => 0.25,
            SummonEffect::Focus => 0.25,
        }
    }

    /// Config string ↔ enum.
    fn from_config(s: &str) -> SummonEffect {
        match s {
            "none" => SummonEffect::None,
            "phosphor" => SummonEffect::Phosphor,
            "liquid" => SummonEffect::Liquid,
            "focus" => SummonEffect::Focus,
            "bayer" => SummonEffect::Bayer,
            _ => SummonEffect::Phosphor, // default / unknown → Phosphor
        }
    }

    fn to_config(self) -> &'static str {
        match self {
            SummonEffect::None => "none",
            SummonEffect::Bayer => "bayer",
            SummonEffect::Phosphor => "phosphor",
            SummonEffect::Liquid => "liquid",
            SummonEffect::Focus => "focus",
        }
    }

    /// Display name shown in the settings selector.
    fn display_name(self) -> &'static str {
        match self {
            SummonEffect::None => "None",
            SummonEffect::Bayer => "Bayer",
            SummonEffect::Phosphor => "Phosphor",
            SummonEffect::Liquid => "Liquid",
            SummonEffect::Focus => "Focus",
        }
    }

    /// The next/previous effect in cycle order (wraps).
    fn cycle(self, forward: bool) -> SummonEffect {
        let i = Self::ORDER.iter().position(|&e| e == self).unwrap_or(1);
        let n = Self::ORDER.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        Self::ORDER[j]
    }
}

/// Scrollback-cycler steps. 100_000 is alacritty's own UI max (and the config
/// clamp ceiling): at ≤24 B/cell a fully-filled 100k×120-col history is
/// ~290 MB per tab, so do not raise it without revisiting memory.
const SCROLLBACK_STEPS: [usize; 6] = [1_000, 5_000, 10_000, 25_000, 50_000, 100_000];

/// The next/previous scrollback step (wraps like `SummonEffect::cycle`). A
/// hand-edited config value between steps first snaps to its NEAREST step,
/// then moves ±1 — so the first click from e.g. 12_345 lands on a canonical
/// value instead of jumping erratically.
fn cycle_scrollback(cur: usize, forward: bool) -> usize {
    let i = SCROLLBACK_STEPS
        .iter()
        .enumerate()
        .min_by_key(|(_, &s)| s.abs_diff(cur))
        .map(|(i, _)| i)
        .unwrap_or(2);
    let n = SCROLLBACK_STEPS.len();
    let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
    SCROLLBACK_STEPS[j]
}

/// Notify minimum-duration cycler steps, in seconds (v0.15). "I stepped away"
/// granularity: 5s … 5m. A hand-edited config value snaps to its nearest step
/// on the first click, then moves ±1 (wraps), mirroring `cycle_scrollback`.
const NOTIFY_MIN_STEPS: [u64; 6] = [5, 10, 30, 60, 120, 300];

/// The next/previous notify-minimum step (wraps).
fn cycle_notify_min(cur: u64, forward: bool) -> u64 {
    let i = NOTIFY_MIN_STEPS
        .iter()
        .enumerate()
        .min_by_key(|(_, &s)| s.abs_diff(cur))
        .map(|(i, _)| i)
        .unwrap_or(1);
    let n = NOTIFY_MIN_STEPS.len();
    let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
    NOTIFY_MIN_STEPS[j]
}

/// Per-tab/window anti-spam floor for command-finish notifications: a single tab
/// pings at most once per this window. A DIFFERENT tab/window is NEVER suppressed
/// (keys are per tab/window), so a burst of finishes across tabs each ping — the
/// exact multi-tab summon use case (amendments §2).
const NOTIFY_MIN_GAP: std::time::Duration = std::time::Duration::from_secs(2);

/// Anti-spam key for command-finish notifications: identifies which surface last
/// fired. Main tabs key on their index; detached windows on their stable id.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NotifyKey {
    MainTab(usize),
    Detached(WindowId),
}

/// Build a command-finish notification's `(summary, body)`. The summary NAMES the
/// firing tab (amendments §1) plus the status and, when known, the duration; the
/// body is the command's last output line.
fn build_notification_text(
    label: &str,
    c: &jetty_core::CommandCompletion,
    failed: bool,
) -> (String, String) {
    let dur = c.duration.map(crate::notify::fmt_duration).unwrap_or_default();
    let status = if failed {
        match c.exit_code {
            Some(code) => format!("failed (exit {code})"),
            None => "failed".to_string(),
        }
    } else {
        "finished".to_string()
    };
    let summary = if dur.is_empty() {
        format!("{label} — {status}")
    } else {
        format!("{label} — {status} · {dur}")
    };
    (summary, c.last_line.clone())
}

/// The winit taskbar/dock urgency level for a completion: `Critical` (persistent /
/// dock-bounce) on failure, `Informational` on success.
fn attention_for(failed: bool) -> winit::window::UserAttentionType {
    if failed {
        winit::window::UserAttentionType::Critical
    } else {
        winit::window::UserAttentionType::Informational
    }
}

/// Display form of a scrollback value: whole thousands render as "Nk" (the
/// cycler steps), anything else (a hand-edited config value) verbatim.
fn format_scrollback(n: usize) -> String {
    if n >= 1000 && n % 1000 == 0 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// How F9 summons the window. Mirrors `SummonEffect`'s ORDER/cycle/from_config
/// pattern. `Center` re-summons centered (or at the last position); `Dropdown`
/// is a Yakuake-style top-anchored full-width strip that slides down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Center,
    Dropdown,
}

impl WindowMode {
    const ORDER: [WindowMode; 2] = [WindowMode::Center, WindowMode::Dropdown];

    fn display_name(self) -> &'static str {
        match self {
            WindowMode::Center => "Center",
            WindowMode::Dropdown => "Dropdown",
        }
    }

    fn cycle(self, forward: bool) -> WindowMode {
        let i = Self::ORDER.iter().position(|&m| m == self).unwrap_or(0);
        let n = Self::ORDER.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        Self::ORDER[j]
    }

    fn from_config(s: &str) -> WindowMode {
        match s {
            "dropdown" => WindowMode::Dropdown,
            _ => WindowMode::Center,
        }
    }

    fn to_config(self) -> &'static str {
        match self {
            WindowMode::Center => "center",
            WindowMode::Dropdown => "dropdown",
        }
    }
}

/// Dropdown slide-in duration in seconds (render-side content translate, not a
/// per-frame reposition). A const, not persisted.
const DROPDOWN_SLIDE_SECS: f32 = 0.15;

/// Grace period (ms) between the main window losing focus and the Yakuake-style
/// auto-hide actually firing. X11 can deliver the main window's Focused(false)
/// BEFORE the Focused(true) of the JeTTY window the user clicked (an already-
/// open detached or Settings window) — the switching_to_* flags only pre-arm
/// window CREATION, so refocusing an existing sibling would wrongly hide the
/// terminal. Deferring the hide lets any of OUR windows' Focused(true) cancel
/// it; 100ms is far above real X11 FocusOut→FocusIn gaps yet imperceptible
/// when focus genuinely leaves JeTTY.
const AUTOHIDE_GRACE_MS: u64 = 100;

/// Default logical (device-independent) font size in points. This is the value
/// used when the user resets the font size with Ctrl+0 and on first launch.
/// Scaled by the display's scale_factor before being passed to TextLayer so
/// glyphs are rendered at physical-pixel resolution on HiDPI screens.
const FONT_LOGICAL_DEFAULT: f32 = 16.0;

/// Visible rows in the open theme dropdown. MUST match `panel::MAX_THEME_ROWS`
/// (panel.rs owns the render-side value; this mirror bounds the scroll clamp).
const MAX_THEME_ROWS: usize = 9;

/// UI (chrome) font-size range in logical points. The chrome — tab titles, the
/// status bar, the right-click menu, help/confirm/welcome overlays — scales
/// across this full range. SEPARATE from the terminal font (which uses its own
/// [6, 48] clamp); a UI-font size change never reflows the grid.
const UI_FONT_MIN: f32 = 10.0;
const UI_FONT_MAX: f32 = 28.0;
/// The Settings panel's OWN body text is CAPPED to this tighter range so the
/// absolute-px panel layout never overflows its fixed window, while the rest of
/// the chrome (and the live "Aa" specimen in the UI-FONT section) tracks the
/// true `ui_font_logical`. The panel is a transient config sheet — the least
/// important surface to scale — so capping it costs nothing the user lives in.
const PANEL_TEXT_MIN: f32 = 13.0;
const PANEL_TEXT_MAX: f32 = 17.0;
/// Default UI font size: 16pt == today's fixed chrome size, so the out-of-box
/// look is unchanged.
const UI_FONT_LOGICAL_DEFAULT: f32 = 16.0;

/// Fallback grid dimensions used only when computing cols/rows from the window
/// is not yet possible (e.g. before `resumed` completes). In practice the
/// derived grid replaces these immediately; they are never used for the actual
/// Terminal or PTY once a window exists.
const FALLBACK_COLS: usize = 80;
const FALLBACK_ROWS: usize = 24;

/// Height of the tab bar (re-exported from the renderer so app.rs has one name).
const TABBAR_H: f32 = jetty_render::TABBAR_H;
/// Height of the bottom status bar (the live perf HUD lives here, OFF the tab
/// row). Reserved from the grid only when `show_perf_hud` is on — see `status_h`.
const STATUS_H: f32 = 22.0;
/// Width reserved on the right of the grid for the scrollbar (a gutter), so the
/// terminal never renders content underneath the scrollbar (which would cover the
/// last column / p10k's right-aligned prompt at some window widths). Scrollbar
/// width + a few px of breathing room.
const SCROLLBAR_GUTTER: f32 = jetty_render::SCROLLBAR_W + 4.0;

/// Maximum bytes of PTY output fed into one tab's terminal per drain pass. Under
/// an output flood (`yes`, `cat huge.log`) the PTY reader thread enqueues data
/// far faster than the VT parser consumes it; draining the channel to empty in
/// one go would never return to the winit loop (no redraws, no keyboard — the
/// user could not even Ctrl+C the flood, and the backlog grows unbounded). The
/// drain stops after this many bytes; the reader queued one Wake per chunk, so
/// the next Wake continues where this left off while input events interleave.
const PTY_DRAIN_BUDGET: usize = 2 * 1024 * 1024;

/// Minimum interval between open-search match re-collects while output
/// streams (each re-collect scans the whole scrollback). Shared by the
/// render-path throttle, the `about_to_wait` trailing one-shot that services
/// a refresh the throttle skipped (F10), and its WaitUntil deadline.
const SEARCH_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// Wrap period (seconds) for the CRT animation phase before it is narrowed to
/// f32. The three sub-effects all use INTEGER angular frequencies (roll 6,
/// flicker 50, jitter 80 & 13 rad/s — see `crt.rs`), so every whole multiple of
/// TAU is a seamless wrap point. Wrapping keeps the value small so f32's 24-bit
/// mantissa never coarsens the animation step after long uptime (an un-wrapped
/// `elapsed().as_secs_f32()` degrades into visible stutter after ~1.5 days).
const CRT_PHASE_WRAP: f64 = std::f64::consts::TAU;

/// A single terminal session: its grid model, PTY, writer, and tab title. One
/// `Tab` per visible tab. Per-tab scroll/selection live inside `terminal`.
pub(crate) struct Tab {
    pub(crate) terminal: Terminal,
    pub(crate) pty: PtySession,
    pub(crate) writer: Box<dyn Write + Send>,
    /// The DISPLAYED title (tab bar, detached bar/OS title, confirm-close).
    pub(crate) title: String,
    /// The frozen "Tab N" fallback restored when the shell resets/clears its
    /// OSC title.
    pub(crate) default_title: String,
    /// Once the user commits a manual rename, shell OSC titles are ignored for
    /// this tab forever (manual > auto > default precedence).
    pub(crate) manually_renamed: bool,
    /// Unseen output/bell that arrived while this tab was INACTIVE, shown as a
    /// themed dot on its tab label; cleared when it renders as the active tab.
    pub(crate) activity: jetty_render::TabActivity,
}

/// Resolve a pending shell title update against the tab's rename state and
/// return the new DISPLAY title to apply, or `None` to leave it unchanged.
/// Precedence: manual rename (permanent) > OSC title > default "Tab N".
/// `update` is `Terminal::take_title_update`'s inner value: `Some(t)` = shell
/// set a title, `None` = shell reset/cleared it (restore the default).
fn resolve_title(
    update: Option<String>,
    manually_renamed: bool,
    default_title: &str,
) -> Option<String> {
    if manually_renamed {
        return None;
    }
    match update {
        Some(t) => Some(t),
        None => Some(default_title.to_string()),
    }
}

/// Grace window after an app-initiated PTY resize (`App::reflow`) during
/// which drained output does NOT light an inactive tab's Output dot: the
/// resize SIGWINCHes every background shell, whose prompt repaint (p10k
/// repaints unconditionally) would otherwise flag "unseen output" on every
/// window/font resize — a self-inflicted false positive (F3). Bell is a real
/// event and is never suppressed.
const REFLOW_ACTIVITY_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// Pure transition for an INACTIVE tab's activity indicator, given what this
/// drain pass observed. Rules (unit-tested):
/// * a bell always escalates to `Bell` (sticky — later output never
///   downgrades it, and the reflow grace never masks it);
/// * output upgrades `None` → `Output`, unless `suppress_output` (the
///   post-reflow SIGWINCH grace, F3) is active;
/// * anything else keeps the current state.
fn next_activity(
    current: jetty_render::TabActivity,
    had_output: bool,
    rang_bell: bool,
    suppress_output: bool,
) -> jetty_render::TabActivity {
    use jetty_render::TabActivity;
    if rang_bell {
        TabActivity::Bell
    } else if had_output && !suppress_output && current == TabActivity::None {
        TabActivity::Output
    } else {
        current
    }
}

/// Whether the Shift+drag hint pill should draw in the window identified by
/// `id`: the shared hint must be live (`now < t`) AND tagged with THIS
/// window — one drag must not light the pill in every window that happens to
/// repaint during the 3.5s (F4). Generic over the id type so it is
/// unit-testable without a winit `WindowId`.
fn shift_hint_live_in<I: PartialEq>(
    hint: Option<(std::time::Instant, I)>,
    id: I,
    now: std::time::Instant,
) -> bool {
    hint.is_some_and(|(t, wid)| wid == id && now < t)
}

/// Logical size of the separate Settings window — DERIVED from the panel size
/// (+ 4px border) so it always fits exactly. Growing the panel (adding a settings
/// row in `build_panel`) resizes this window automatically; the bottom rows
/// (theme chips) can never be clipped off a too-short window again.
const SETTINGS_WIN_W: u32 = jetty_render::PANEL_W as u32 + 4;
const SETTINGS_WIN_H: u32 = jetty_render::PANEL_H as u32 + 4;

/// Identifies which Effects-tab slider is currently being dragged. One variant
/// per draggable slider; `None` stored in `App::active_fx_drag` when no drag is
/// in progress. Mirrors the `dragging_slider` / `dragging_radius` bool pattern
/// but consolidates 13 sliders into a single optional enum so the struct stays
/// compact and the `CursorMoved` handler stays readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FxSlider {
    CrtCurvature,
    CrtScanline,
    CrtMask,
    CrtBloom,
    CrtChromatic,
    CrtVignette,
    CaretDur,
    TintR,
    TintG,
    TintB,
    CaretColorR,
    CaretColorG,
    CaretColorB,
}

pub struct App {
    proxy: EventLoopProxy<AppEvent>,
    window: Option<Arc<Window>>,
    /// Whether the window is currently visible (toggled by F9).
    visible: bool,
    /// Whether the main window is occluded or minimized (`WindowEvent::Occluded(true)`
    /// or the minimize button/WM iconify). Distinct from `visible` (the F9 summon
    /// toggle): a window can be `visible == true` yet fully hidden behind others or
    /// iconified. Every self-driven animation/redraw gates on
    /// `visible && !main_occluded` so a hidden/minimized window returns to true
    /// idle instead of burning CPU rendering invisible frames (F8/F16/F17/F18).
    main_occluded: bool,
    /// Whether the F9 global-hotkey worker has been launched. The manager itself
    /// is kept alive inside that worker thread (it never returns), so we only need
    /// a launched-once sentinel here rather than holding the manager on the App.
    hotkey_manager: Option<()>,
    gpu: Option<GpuContext>,
    text: Option<TextLayer>,
    /// FIXED-size TextLayer used for ALL window chrome (tab bar labels, context
    /// menu, help overlay, confirm popup). Built at `FONT_LOGICAL_DEFAULT * scale`
    /// and rebuilt only on SCALE-factor changes — NOT on terminal font changes —
    /// so the chrome never scales with (and overflows from) the terminal font.
    chrome_text: Option<TextLayer>,
    quad: Option<QuadLayer>,
    /// Final-pass rounded-corner mask for the borderless main window.
    corner_mask: Option<jetty_render::CornerMask>,
    /// Final-pass Bayer crystallize reveal for the summon animation.
    bayer_reveal: Option<jetty_render::BayerReveal>,
    /// Final-pass Phosphor Ignition reveal for the summon animation.
    phosphor: Option<jetty_render::PhosphorIgnition>,
    /// Tier-B LiquidDrop summon effect (samples the offscreen frame).
    liquid: Option<jetty_render::LiquidDrop>,
    /// Tier-B FocusPull summon effect (samples the offscreen frame).
    focus: Option<jetty_render::FocusPull>,
    /// CRT post-effect: when enabled the whole scene is rendered to `offscreen`
    /// and this pass applies the full CRT effect pipeline, writing to the surface.
    /// Built in `resumed` with the surface format; `None` until then.
    crt: Option<jetty_render::Crt>,
    /// Per-window inline-image (sixel) layer on the MAIN device. Draws decoded
    /// images over the grid into `scene_view` (so CRT / corner-mask / summon
    /// compositing apply). Detached windows hold their own on their own device.
    /// Built in `resumed`; `None` until then. Zero cost when no image is visible.
    image_layer: Option<jetty_render::ImageLayer>,
    /// Optional GPU caret glow/ripple pass (Task 12). Additive halo + expanding
    /// ring around the cursor cell on each keystroke burst. Built in `resumed`
    /// with the surface format; dispatched only when `fx.caret_glow_enabled` AND
    /// `caret_anim.is_some()` AND the cursor is visible — zero cost otherwise.
    caret_fx: Option<jetty_render::CaretFx>,
    /// Surface-sized offscreen color texture used while a Tier-B effect is
    /// summoning OR while CRT is enabled: the scene is rendered into this, then
    /// the effect (Liquid/Focus) or the CRT pass samples it and writes to the
    /// surface. `None` until built in `resumed`; re-created on `Resized`. The
    /// normal (Tier-A / no-summon / CRT-off) hot path renders straight to the
    /// surface as before.
    offscreen: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// The currently selected window-summon reveal effect.
    summon_effect: SummonEffect,
    /// How F9 summons the window (Center vs Yakuake-style Dropdown).
    window_mode: WindowMode,
    /// Whether the tab bar (tabs + window controls) sits at the BOTTOM of the
    /// window instead of the TOP. Orthogonal to `window_mode` (works in both
    /// Center and Dropdown). Default `false` (top).
    tab_bar_bottom: bool,
    /// Dropdown height as a fraction of the monitor height (clamped 0.25..=1.0).
    dropdown_height_pct: f32,
    /// Dropdown width as a fraction of the monitor width (clamped 0.2..=1.0).
    /// Reserved; MVP ships full-width (1.0) and has no UI slider yet.
    dropdown_width_pct: f32,
    /// Start instant of the active Dropdown SLIDE animation, or None when idle.
    /// The slide is a render-side content translate; while Some the redraw loop
    /// self-drives frames (idle 0 CPU once cleared).
    slide_anim: Option<std::time::Instant>,
    /// Frames remaining to RE-APPLY the dropdown dock geometry after the window
    /// is mapped. On X11, KWin ignores set_outer_position issued before the
    /// window is realized (it applies its own placement → the window lands
    /// centered), so a single pre-map dock fails. Re-asserting on the first few
    /// post-map redraws makes the WM honor the top-strip position; counts down to
    /// 0 so idle CPU returns to 0.
    pending_dock_frames: u8,
    /// Center-mode analogue of pending_dock_frames: X11/KWin likewise ignores a
    /// set_outer_position issued before the window is mapped, discarding the
    /// user's saved position on every summon. Re-assert it on the first few
    /// post-map redraws; counts down to 0 so idle CPU returns to 0.
    pending_center_frames: u8,
    /// The position to re-assert while pending_center_frames > 0.
    pending_center_pos: Option<winit::dpi::PhysicalPosition<i32>>,
    /// Hide the window on focus loss (Yakuake auto-hide). Default ON.
    focus_autohide: bool,
    /// Scrollback history limit in lines (config `scrollback_lines`, clamped
    /// 100..=100_000, default 10_000). Applied live to every tab (main +
    /// detached) when changed via the Settings cycler.
    scrollback_lines: usize,
    /// Launch JeTTY at login via the XDG autostart `.desktop` file. The file's
    /// existence is the source of truth; this mirrors it for the Settings pill.
    launch_at_login: bool,
    /// Global summon hotkey string (e.g. "F9", "F12", "Ctrl+Shift+F12"). Parsed
    /// by `global_hotkey`'s own `HotKey::from_str`. Default "F9".
    summon_hotkey: String,
    /// Shell to launch (the `shell` config key). Empty = auto-detect
    /// ($SHELL → passwd → /bin/bash); a path forces that shell.
    shell: String,
    /// Cached tab-bar metadata (title, is-active), rebuilt only when the tab
    /// titles or the active index change. Avoids cloning every tab title on
    /// every RedrawRequested (incl. animation frames) — speed-first hot path.
    /// Cached "window top-flush against the monitor" flag (drives top-corner
    /// rounding in Dropdown mode). Recomputed only on non-animating frames so the
    /// outer_position()/current_monitor() syscalls don't run ~60fps during a
    /// dropdown slide (the window doesn't move during the slide — it's a content
    /// y-offset), and reused from the cache while sliding.
    cached_top_flush: bool,
    cached_tabs_meta: Vec<(String, bool)>,
    /// Signature (hash of titles + active index) of `cached_tabs_meta`; when it
    /// differs from the live signature, the cache is rebuilt.
    cached_tabs_sig: u64,
    /// Last string passed to the main window's `set_title` ("{tab} — JeTTY"),
    /// so the taskbar/alt-tab title sync in `tabs_meta` is a no-op string
    /// compare unless the active tab's title really changed.
    applied_main_os_title: String,
    /// The id of the most recently focused window (main or settings). Used to
    /// suppress auto-hide when focus moved to our own Settings window.
    last_focused_window: Option<WindowId>,
    /// Whether the MAIN terminal window currently holds OS focus. Tracked from
    /// its `Focused(true)/(false)` events (last_focused_window is unreliable for
    /// this — it stays set to the main id after focus leaves when auto-hide is
    /// off). Drives the unfocused-hollow cursor.
    main_focused: bool,
    /// Set when the Settings window gains focus; consumed by the main window's
    /// Focused(false) to suppress auto-hide even when X11 delivers the main
    /// Focused(false) BEFORE the settings Focused(true) (the last_focused_window
    /// check alone loses that race).
    switching_to_settings: bool,
    /// Set while focus is moving to one of OUR detached windows (on detach, and
    /// while a detached window holds focus). Consumed by the main window's
    /// Focused(false) to suppress auto-hide so detaching a tab does not hide the
    /// main window — mirrors `switching_to_settings` for the Settings window.
    switching_to_detached: bool,
    /// When `Some`, a focus-loss auto-hide of the main window is SCHEDULED for
    /// this instant (`AUTOHIDE_GRACE_MS` after the Focused(false)). Cancelled by
    /// any JeTTY window (main/settings/detached) gaining focus in the interim —
    /// this closes the X11 race where the main FocusOut is delivered before the
    /// FocusIn of the sibling JeTTY window the user actually clicked. Fired by
    /// `about_to_wait`; also cleared by any explicit visibility change.
    pending_autohide_at: Option<std::time::Instant>,
    /// Whether the user is dragging the Dropdown-height slider in Settings.
    dragging_dropdown: bool,
    /// Whether the user is dragging the Dropdown-width slider in Settings.
    dragging_dropdown_width: bool,
    /// One-time guard for the Wayland "positioning is a no-op" diagnostic.
    wayland_warned: bool,
    /// Free-running clock for CRT animation (roll/flicker/jitter). Initialized
    /// once at construction and never reset; `elapsed().as_secs_f32()` feeds the
    /// CRT uniform's `time`. The shader uses `sin`, so unbounded growth is
    /// fine. This clock does NOT by itself drive redraws — the redraw guard only
    /// self-schedules frames while an animate toggle is on (see `crt_anim_live`).
    crt_clock: std::time::Instant,
    /// Start instant of the active summon (crystallize) animation, or None when
    /// idle. While Some, the redraw loop self-drives frames; None = idle 0 CPU.
    summon_anim: Option<std::time::Instant>,
    /// Start instant of the active caret flash+pulse animation, or None when idle.
    /// Set on every printable keystroke (re-armed each time); cleared when t≥1.
    /// While Some, the redraw loop self-drives frames via Poll; None = idle 0 CPU.
    caret_anim: Option<std::time::Instant>,
    /// Set when a summon is requested; the summon clock (`summon_anim`) starts on
    /// the first redraw AFTER the window is actually shown. On macOS a freshly
    /// shown window can take a beat to present — starting the clock at
    /// set_visible() time would let the whole effect elapse unseen (effectless).
    summon_pending: bool,
    /// Until this instant, suppress focus-loss auto-hide. A summon maps/focuses the
    /// window, which X11 can answer with a SYNTHETIC Focused(false); for a fast
    /// effect (None/Bayer) summon_anim has already ended by then, so without this
    /// bound the window could auto-hide the very frame it appears. ~300ms gate,
    /// independent of the effect duration.
    summon_settle_until: Option<std::time::Instant>,
    /// While `now < this`, the freshly-opened settings window is kept repainting
    /// under Poll. macOS can't present to a brand-new window's surface until the
    /// run loop has displayed it a few times, so a SINGLE redraw on open is
    /// dropped (the window shows blank until clicked). Repaint for a short window
    /// instead, until one frame actually presents. None = idle.
    settings_paint_until: Option<std::time::Instant>,
    /// Window corner radius in logical px, clamped [0, 24]. 0 = square corners.
    corner_radius: f32,
    /// All open terminal sessions, one per tab. Always non-empty once `resumed`
    /// has run; when it becomes empty the event loop exits.
    tabs: Vec<Tab>,
    /// Index of the active tab into `tabs`.
    active: usize,
    /// Ordered index into the theme registry (`jetty_core::theme::theme_list()` —
    /// built-ins + user themes) for the current theme. Re-resolved by NAME and
    /// re-clamped on every config/theme reload, so adding/removing a theme file never
    /// leaves it dangling.
    theme_idx: usize,
    /// The RAW resolved active theme (registry-resolved, WITHOUT the global opacity
    /// applied). Cached so the render hot path (`current_theme`, called every frame
    /// by the tab bar / modals) never locks the theme registry or re-resolves per
    /// frame (amendment T1). Recomputed ONLY in `apply_theme` (i.e. on a theme_idx
    /// change) and on reload; `current_theme` clones it and stamps the live opacity.
    active_theme: jetty_core::Theme,
    /// Whether the Look-tab theme dropdown is expanded. Session-only (not persisted).
    theme_dropdown_open: bool,
    /// First visible row index into the theme list when the dropdown is open.
    theme_scroll_offset: usize,
    /// Background opacity (0.0..=1.0); modifies theme bg alpha at runtime.
    opacity: f32,
    /// Current logical (device-independent) font size in points. Changed at
    /// runtime via Ctrl+Equal/Ctrl+Minus/Ctrl+0 (font up/down/reset).
    font_logical: f32,
    /// When `Some`, a grid+PTY `reflow()` is scheduled for this instant. Rapid
    /// Ctrl+/- font changes set this ~120ms ahead and rebuild the visual font
    /// immediately; `about_to_wait` fires ONE reflow once the user stops, so N
    /// presses coalesce into a single PTY SIGWINCH (avoids stacked p10k prompts).
    reflow_pending_at: Option<std::time::Instant>,
    /// When the last app-initiated `reflow()` resized the tabs' PTYs. Drains
    /// within [`REFLOW_ACTIVITY_GRACE`] of it skip the inactive-tab
    /// None→Output activity upgrade: the resize SIGWINCHed every background
    /// shell, and their prompt repaints must not light false "unseen output"
    /// dots on every window/font resize (F3). Never cleared — it simply ages
    /// out; read only on the (event-driven) drain path, zero idle cost.
    reflow_resized_at: Option<std::time::Instant>,
    /// Current font family name (runtime-settable via the font picker).
    font_family: String,
    /// Cached sorted monospace family list (populated once TextLayer is built).
    font_families: Vec<String>,
    /// Scroll offset into `font_families` for the panel's font-family list.
    font_scroll_offset: usize,
    /// UI (chrome) font family — drives tab titles, status bar, menus, panel,
    /// help/confirm/welcome. SEPARATE from `font_family` (the terminal grid font).
    /// `""` = platform proportional sans (the default look).
    ui_font_family: String,
    /// UI (chrome) font size in logical points, clamped [10, 28]. SEPARATE from
    /// `font_logical`; a change never reflows the grid (chrome size is orthogonal
    /// to cols/rows), so there is no p10k-scatter risk and no debounce.
    ui_font_logical: f32,
    /// Cached PROPORTIONAL family list for the UI-font picker, with a synthetic
    /// index-0 "System Sans (default)" row (→ "") prepended. Populated at init.
    ui_font_families: Vec<String>,
    /// Scroll offset into `ui_font_families` for the panel's UI-font list.
    ui_font_scroll_offset: usize,
    /// Active settings tab (0=Look, 1=Fonts, 2=Window, 3=Shell). Session-only:
    /// NOT persisted to config, so it resets to 0 each launch.
    settings_tab: usize,
    /// Vertical scroll offset (physical px, 0 = top) for the Effects tab (4).
    /// Clamped to [0, max(0, effects_content_h - visible_h)] by the wheel handler.
    effects_scroll: f32,
    /// Runtime mirror of the persisted `EffectsConfig`. Loaded from config on
    /// startup; written back to `Config.effects` by `persist()`. UI/renderer tasks
    /// read and write fields here; the next `persist()` call flushes them to disk.
    fx: crate::config::EffectsConfig,
    // ── SSH-ready & yours (v0.16) ──────────────────────────────────────────────
    /// Allow OSC 52 clipboard PASTE (remote READ of the local clipboard). Mirrors
    /// `Config.osc52_allow_paste`; default OFF (secure). Applied to a tab's terminal
    /// at spawn (and live on reload).
    osc52_allow_paste: bool,
    /// Whether config/theme hot-reload is enabled (mirrors `Config.hot_reload`). When
    /// false the watcher is never spawned (or is dropped on a live turn-off).
    hot_reload: bool,
    /// Compiled keybindings (built from `keys` on load / reload). The input path
    /// does ONE cheap hashmap lookup against this per keypress — never per frame.
    keymap: crate::keymap::KeyMap,
    /// The user's raw `[keys]` overrides (mirrors `Config.keys`). Kept so `persist()`
    /// round-trips them and the "Reset keybindings" palette command can clear them.
    keys: crate::config::KeyBindings,
    /// Cached help-overlay rows, regenerated from `keymap` on load/reload so the
    /// Help panel reflects remaps. Cloned only when the overlay is actually drawn.
    help_rows: Vec<String>,
    /// The `notify` file-watcher handle. MUST be kept alive for the process lifetime
    /// (dropping it stops watching). `None` when hot-reload is off. Named with a
    /// leading `_` intent, but read on a live turn-off to drop it.
    config_watcher: Option<::notify::RecommendedWatcher>,
    /// Set true ONLY for the duration of `reload_config_and_themes`. While set,
    /// `persist()` is a NO-OP — so a reload applying live keys through the normal
    /// setters can never write config.toml, making the watcher loop-free BY
    /// CONSTRUCTION (amendment H2), independent of the hash guard.
    reloading: bool,
    /// Hash of the exact string content of the last config.toml WE wrote (via
    /// `persist`). A reload whose on-disk content hashes to this value is our own
    /// write echoing back through the watcher → skipped (the secondary loop guard
    /// after `reloading`). `Cell` so `persist(&self)` can record it.
    last_written_config_hash: std::cell::Cell<Option<u64>>,
    /// When `Some`, a debounced config/theme reload is due at this instant. Set by a
    /// `ConfigChanged` event (coalescing an editor's write/rename/chmod burst); the
    /// reload runs once from `about_to_wait` when the deadline passes, then clears.
    pending_reload_at: Option<std::time::Instant>,
    // ── Run & Notify (v0.15) runtime mirrors of the persisted config keys ──────
    /// Notify (toast + taskbar/dock urgency) when a command finishes while JeTTY
    /// is hidden/unfocused. Mirrors `Config.notify_on_command_finish`; the whole
    /// feature is inert unless OSC 133 shell integration is enabled.
    notify_on_finish: bool,
    /// Minimum SUCCESS-command duration (seconds) to notify on (failures may ping
    /// below it — see the notifier's failure floor). Mirrors `notify_min_seconds`.
    notify_min_seconds: u64,
    /// Only notify (and auto-summon) on FAILED commands. Mirrors `notify_only_on_failure`.
    notify_only_on_failure: bool,
    /// Raise + focus JeTTY and activate the firing tab when a command finishes —
    /// ONLY when fully hidden. Opt-in, default OFF. Mirrors `auto_summon_on_finish`.
    auto_summon_on_finish: bool,
    /// Handle to the off-UI-thread notification worker. Cheap to clone; a `fire()`
    /// is a non-blocking `try_send` (dropped on a full queue). The worker exits
    /// when this last handle drops with the `App`.
    notifier: crate::notify::Notifier,
    /// Last time each tab/window fired a command-finish notification, for PER-tab
    /// anti-spam (a different tab is never suppressed by another's recent ping).
    /// Keyed by `NotifyKey`; entries are tiny and bounded by the live tab/window
    /// count in practice.
    notify_last_at: std::collections::HashMap<NotifyKey, std::time::Instant>,
    /// Track held modifier keys so Ctrl+Shift combos can be detected.
    modifiers: winit::keyboard::ModifiersState,
    /// Last known cursor position in physical pixels.
    cursor: (f64, f64),
    /// Where a no-Shift press began while a mouse-reporting app was active (the
    /// press was forwarded to the app). On release, if the cursor moved, the user
    /// was likely trying to select — surface the Shift+drag hint. `take`n on release.
    mouse_grab_press: Option<(f64, f64)>,
    /// Fractional wheel-scroll accumulator for the main window: slow touchpad
    /// deltas (sub-line PixelDelta/LineDelta) accumulate across events instead
    /// of being rounded to 0 and dropped. Reset on tab switch so one tab's
    /// remainder never bleeds into another.
    scroll_accum: input::ScrollAccumulator,
    /// While `Some((t, id))` and `now < t`, the "Hold Shift to select" toast
    /// is drawn — ONLY in window `id`, the one the no-Shift drag happened in.
    /// The timer is shared, but untagged it made EVERY window (main and all
    /// detached) draw the pill and self-drive frames for the 3.5s (F4).
    shift_hint_until: Option<(std::time::Instant, winit::window::WindowId)>,
    /// Throttle: the toast won't re-arm until `now` passes this instant.
    /// Deliberately GLOBAL across windows (one hint per 25s app-wide).
    shift_hint_cooldown: Option<std::time::Instant>,
    /// Whether the user is currently dragging the scrollbar thumb.
    dragging_scrollbar: bool,
    /// Y offset from thumb top where the user grabbed, in px.
    drag_grab_dy: f32,
    /// The separate OS window hosting the Settings UI, when open. `None` when the
    /// settings window is closed. The terminal lives in `window`; settings now
    /// live entirely in this second, movable window.
    settings_window: Option<Arc<Window>>,
    /// GPU/render stack for the settings window (parallel to `gpu`/`text`/`quad`).
    settings_gpu: Option<GpuContext>,
    settings_text: Option<TextLayer>,
    settings_quad: Option<QuadLayer>,
    /// A second text layer on the SETTINGS device, kept at the TRUE (uncapped) UI
    /// size, used ONLY to draw the live "Aa" specimen in the UI-FONT section — so
    /// the user sees an honest preview even though the panel body text is capped.
    /// Created/dropped with the settings window (so no GPU layer leaks). Lives on
    /// the settings device because `chrome_text` is bound to the MAIN window's
    /// device and cannot render into the settings surface.
    settings_specimen_text: Option<TextLayer>,
    /// Last known cursor position inside the settings window (physical px), used
    /// for hit-testing the panel in the settings window's own coordinate space.
    settings_cursor: (f64, f64),
    /// Whether the user is currently dragging the opacity slider in the Settings panel.
    dragging_slider: bool,
    /// Whether the user is currently dragging the corner-radius slider.
    dragging_radius: bool,
    /// Which Effects-tab slider (if any) is currently being dragged. `None` when
    /// no effects slider drag is in progress. Mirrors `dragging_slider` etc.
    active_fx_drag: Option<FxSlider>,
    /// Whether the user is currently dragging a text selection with the mouse.
    selecting: bool,
    /// The link under the pointer while the link modifier (Ctrl; also Cmd on
    /// macOS) is held — drawn as an underline and opened on click. Cached
    /// app-side keyed on `link_hover_cell`; spans are revalidated on grid
    /// change (never terminal `Point`s, which history trimming invalidates).
    link_hover: Option<jetty_core::LinkHit>,
    /// The hovered 0-based grid cell `(line, col)` the cache above was
    /// computed for; hover recompute is skipped while the cell is unchanged.
    link_hover_cell: Option<(usize, usize)>,
    /// Whether JETTY_DEBUG is set — enables input/panel state logging to stderr.
    debug: bool,
    /// When Some, the right-click context menu is open at this physical-pixel position.
    context_menu: Option<(f32, f32)>,
    /// Cached item hit-test rects for the open context menu, built once when the
    /// menu opens (they depend only on the anchor + window size). Reused for
    /// hover/click hit-testing so high-frequency CursorMoved doesn't rebuild the
    /// whole menu every move.
    menu_item_rects: Vec<jetty_render::Rect>,
    /// Index of the menu item currently under the cursor (for hover highlight).
    menu_hover: Option<usize>,
    /// Inline tab rename: `Some(tab_index)` while the user is editing a tab title.
    renaming: Option<usize>,
    /// The edit buffer for the in-progress rename (committed/discarded on Enter/Esc).
    rename_buf: String,
    /// Time + physical-pixel position of the last left press on the top strip,
    /// used to detect double-clicks (window maximize / enter-rename).
    last_strip_click: Option<(std::time::Instant, f32, f32)>,
    /// The resize cursor currently applied to the main window. Cached so we only
    /// call `set_cursor` when the zone actually changes (the borderless window
    /// draws its own resize edges).
    resize_cursor: ResizeZone,
    /// Whether the neofetch-style welcome splash is still open. Shown on launch
    /// (when `show_welcome` is true in config); dismissed on the first real PTY
    /// keypress, any mouse click in the grid area, or Esc. A single bool — the
    /// check and the clear are both O(1) so the idle path is unaffected.
    welcome_open: bool,
    /// The persisted `show_welcome` startup preference (distinct from the runtime
    /// `welcome_open` dismissal state). Cached at startup so `persist()` can write
    /// it back WITHOUT re-reading the config file on every settings change.
    cfg_show_welcome: bool,

    // --- Live performance HUD (tab bar: ⚡ ms · fps · CPU% · VT MB/s) ---
    // CRITICAL: none of these fields ever force or schedule a redraw. They are
    // updated ONLY inside frames already happening for another reason; when the
    // app is idle (ControlFlow::Wait) the HUD simply freezes at its last value.
    /// Whether to build/measure the perf HUD at all (mirrors config.show_perf_hud).
    /// When false the HUD is never built and sysinfo is never sampled — zero cost.
    show_perf_hud: bool,
    /// Wall-clock of the previous rendered frame, for the smoothed frame-ms.
    /// `None` until the first frame. Updated each render.
    last_frame_at: Option<std::time::Instant>,
    /// Exponentially-smoothed frame time in ms (ms = ms*0.9 + dt*0.1). fps is
    /// derived from this. Reads the render rate DURING activity; freezes when idle.
    perf_ms: f32,
    /// sysinfo handle scoped to THIS process's CPU usage only (cheap refresh).
    perf_sys: sysinfo::System,
    /// Our own PID, resolved once at startup so per-frame refreshes are O(1).
    perf_pid: sysinfo::Pid,
    /// Last time we refreshed CPU% (gated to ≤1 Hz — sysinfo needs ≥~200ms
    /// between samples for a valid %, and per-frame refresh would be wasteful).
    last_cpu_at: std::time::Instant,
    /// Last sampled process CPU%, held between the ≤1 Hz refreshes.
    perf_cpu: f32,
    /// Running total of bytes read from the PTY(s), incremented at the drain site.
    vt_bytes: u64,
    /// vt_bytes value at the start of the current ~1s throughput window.
    vt_bytes_at_window_start: u64,
    /// Start instant of the current throughput window.
    vt_window_start: std::time::Instant,
    /// Last computed VT throughput in MB/s, held between ~1s window updates.
    perf_mb: f32,
    /// Idle-HUD one-shot: after the last ACTIVE frame, the deadline at which —
    /// if nothing else has drawn — the loop wakes ONCE to repaint the HUD in its
    /// honest "idle" state (so it doesn't sit frozen on a stale fps/CPU value).
    /// Re-armed on every active frame; `None` until the first frame.
    perf_idle_at: Option<std::time::Instant>,
    /// True once the idle-state HUD has been painted, so we don't repaint it in a
    /// loop. Cleared on the next active frame. This is what keeps idle at ~0 CPU:
    /// exactly ONE extra repaint per activity burst, then a true `Wait`.
    perf_idle_shown: bool,
    /// The perf-HUD string built on the most recent render, cached so the
    /// click-time tab-bar hit-test rebuild reserves the IDENTICAL HUD width and
    /// the tab/close hit-rects line up with what's drawn. `None` when the HUD is
    /// disabled or hidden (too-narrow window). Not perf-critical (clone on render).
    perf_label: Option<String>,
    /// Real-window perf instrumentation (`JETTY_PERF_LOG=1`): input latency,
    /// exec→first-frame cold start, idle RSS. `perf.on` is a plain bool read ONCE
    /// from the environment at construction; when false every stamp site below is a
    /// single predictable-false branch and the hot paths are byte-identical. See
    /// `crate::perf`.
    perf: crate::perf::Perf,

    /// Debug missed-paint proof counter (`JETTY_FRAME_LOG=1`, off by default).
    /// When on, every `frame.present()` (main / detached / settings) bumps
    /// `frames_presented` and emits a `JETTY_FRAME <n> <surface>` line to stderr,
    /// so `scripts/verify-idle.sh` can assert a keystroke/PTY burst's FINAL
    /// mutation was actually presented (a dropped final frame = the count stalls
    /// before the last mutation; a self-driving hidden window = the count keeps
    /// climbing with no input). `frame_log` is read ONCE from the environment at
    /// construction; when false the two-field bump is a single predictable-false
    /// branch and the present path stays byte-identical (HARD RULE #1).
    frame_log: bool,
    frames_presented: u64,

    /// Whether the in-window "Keyboard Shortcuts" help overlay is open. Drawn on
    /// top of everything in the main window; dismissed by Esc, the "?" button,
    /// or a click outside the panel.
    help_open: bool,
    /// Whether the scrollback-search bar (Ctrl+Shift+F) is open on the ACTIVE
    /// tab of the main window (detached windows are out of scope). While open,
    /// keys edit the query; Esc / ✕ / Ctrl+Shift+F close it and clear matches.
    search_open: bool,
    /// Last streaming refresh of the open search's matches (throttled to
    /// [`SEARCH_REFRESH_INTERVAL`] on the PTY-drain path so heavy output
    /// never re-scans history every frame). `None` until the first refresh.
    search_refresh_at: Option<std::time::Instant>,
    /// True while the open search's stored matches may be stale: set when a
    /// drain consumed output but the throttle skipped the re-collect, cleared
    /// by every refresh. While set, `about_to_wait` schedules ONE wake at the
    /// throttle deadline so a burst that ENDS inside the window still gets a
    /// trailing refresh (F10) — the flag never exists while idle, so this
    /// adds zero idle work.
    search_dirty: bool,
    /// When `Some(i)`, a "Close this tab?" confirmation popup is open for tab `i`.
    /// The × click / Ctrl+Shift+W / Ctrl+D set this instead of closing immediately;
    /// Enter (or the Close button) confirms, Esc (or Cancel / click-outside) clears.
    confirm_close: Option<usize>,
    /// Set when the user tries to close the whole app (window × button or the OS
    /// CloseRequested). Shows a "Quit JeTTY?" popup instead of exiting; Enter
    /// confirms, Esc / Cancel / click-outside dismisses.
    confirm_quit: bool,
    /// Where the window was when last hidden, so re-summoning (F9) restores it to
    /// the spot the user left it instead of always re-centering. `None` until the
    /// first hide; the first open is centered.
    last_pos: Option<winit::dpi::PhysicalPosition<i32>>,
    /// All open detached terminal windows (one `Tab` each). Created by
    /// `detach_tab`; dropped (closing the OS window and reaping the PTY)
    /// when `reattach_tab` or the window's CloseRequested removes the entry.
    detached: Vec<crate::detached::DetachedWindow>,
    /// In-progress left-button drag that began on a tab in the main tab bar.
    /// `None` when no tab is held. Becomes "tearing" once the cursor leaves the
    /// strip by more than `detached::TEAR_THRESHOLD_PX` vertically; releasing
    /// while tearing detaches that tab at the drop position. Cleared on release
    /// and on focus loss (same discipline as `selecting`/`dragging_scrollbar`).
    tab_drag: Option<TabDrag>,
    /// When `Some((x, y, tab_idx))`, the TAB context menu (Detach / Rename /
    /// Close Tab) is open at this physical-pixel anchor for tab `tab_idx`.
    /// Mutually exclusive with `context_menu` (the terminal Copy/Paste menu).
    tab_menu: Option<(f32, f32, usize)>,
    /// Item labels of the open tab menu, snapshotted when it opened (the
    /// "Detach" row is present only when detaching was allowed at open time).
    tab_menu_labels: Vec<&'static str>,
    /// Cached hit-test rects for the open tab menu (built once on open).
    tab_menu_rects: Vec<jetty_render::Rect>,
    /// Tab-menu item currently under the cursor (hover highlight).
    tab_menu_hover: Option<usize>,

    // --- Command palette (Ctrl+Shift+P / macOS Cmd+Shift+P) ---
    /// Whether the fuzzy command palette overlay is open on the MAIN window.
    /// While open it captures ALL keyboard + mouse input (single-overlay-owns-
    /// keys). Zero cost when closed: the registry/filtered vecs are empty and the
    /// overlay is neither built nor drawn (one bool test on the hot path).
    palette_open: bool,
    /// The typed query. Refiltered only on a keystroke — never per frame.
    palette_query: String,
    /// Index of the highlighted row within `palette_filtered`.
    palette_selected: usize,
    /// First visible row (scroll offset) into `palette_filtered`.
    palette_scroll: usize,
    /// The action registry, rebuilt FRESH on open (never per frame / in
    /// apply_theme, which auto-repeats on opacity) and dropped on close.
    palette_registry: Vec<crate::palette::PaletteEntry>,
    /// The current fuzzy hits (resolved PaletteCmd + title + matched indices),
    /// recomputed on each keystroke. Enter runs the stored cmd, never a stale idx.
    palette_filtered: Vec<crate::palette::PaletteHit>,

    // --- Hint mode (Ctrl+Shift+H) + keyboard copy-mode (Ctrl+Shift+Space) ---
    /// Active hint-mode state (main window + primary screen only). `Some` while
    /// the labelled URL/path/hash/IPv4 chips are shown; the tokens are scanned
    /// ONCE on enter (never per frame). Zero cost when closed (one `Option`
    /// test on the hot path).
    hint_mode: Option<HintState>,
    /// Active copy-mode state: a keyboard vi-cursor over the viewport +
    /// scrollback. `Some` while active; the shell cursor is suppressed and the
    /// alacritty `Selection` drives the highlight. Zero cost when closed.
    copy_mode: Option<crate::copymode::CopyMode>,
}

/// Hint-mode capture state: the scanned tokens, their parallel labels, and the
/// prefix typed so far (for partial narrowing).
struct HintState {
    tokens: Vec<jetty_core::HintToken>,
    labels: Vec<String>,
    typed: String,
}

/// Owned command-palette draw data, captured before the render borrow:
/// `(query, visible rows as (title, matched-char indices, selected), total, first_visible)`.
type PaletteDrawData = (String, Vec<(String, Vec<usize>, bool)>, usize, usize);
/// Hint-mode overlay draw data captured before the mutable render borrow:
/// the visible `(label, vp_row, col_start)` chips + the typed prefix.
type HintDrawData = (Vec<(String, usize, usize)>, String);

/// A left-button drag that began on tab `idx` in the main tab bar. `tearing`
/// flips true once the cursor moves > `TEAR_THRESHOLD_PX` vertically out of the
/// strip (and back false if it returns), so a plain click still selects.
#[derive(Debug, Clone, Copy)]
struct TabDrag {
    idx: usize,
    tearing: bool,
}

/// Which resize zone (if any) the cursor is over on a borderless window (the
/// main window and every detached window share this).
/// Corners take priority over edges; `None` means a normal cursor / no resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizeZone {
    None,
    West,
    East,
    North,
    South,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

impl ResizeZone {
    /// The winit resize direction for this zone (None for `ResizeZone::None`).
    pub(crate) fn direction(self) -> Option<winit::window::ResizeDirection> {
        use winit::window::ResizeDirection as D;
        Some(match self {
            ResizeZone::None => return None,
            ResizeZone::West => D::West,
            ResizeZone::East => D::East,
            ResizeZone::North => D::North,
            ResizeZone::South => D::South,
            ResizeZone::NorthWest => D::NorthWest,
            ResizeZone::NorthEast => D::NorthEast,
            ResizeZone::SouthWest => D::SouthWest,
            ResizeZone::SouthEast => D::SouthEast,
        })
    }

    /// The cursor icon matching this resize zone.
    pub(crate) fn cursor_icon(self) -> winit::window::CursorIcon {
        use winit::window::CursorIcon as C;
        match self {
            ResizeZone::None => C::Default,
            ResizeZone::West | ResizeZone::East => C::EwResize,
            ResizeZone::North | ResizeZone::South => C::NsResize,
            ResizeZone::NorthWest | ResizeZone::SouthEast => C::NwseResize,
            ResizeZone::NorthEast | ResizeZone::SouthWest => C::NeswResize,
        }
    }
}

/// Compute the resize zone for a cursor at `(cx, cy)` (physical px) in a window
/// of physical size `w`×`h`. Edges are within `EDGE` px of a side; corners
/// within `CORNER` px of a corner. Corners take priority over edges. Returns
/// `ResizeZone::None` when the cursor is in the interior.
pub(crate) fn resize_zone_at(cx: f32, cy: f32, w: u32, h: u32) -> ResizeZone {
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 12.0;
    let w = w as f32;
    let h = h as f32;
    // Out-of-bounds → no resize.
    if cx < 0.0 || cy < 0.0 || cx > w || cy > h {
        return ResizeZone::None;
    }
    let near_left = cx <= CORNER;
    let near_right = cx >= w - CORNER;
    let near_top = cy <= CORNER;
    let near_bottom = cy >= h - CORNER;
    // Corners first (within CORNER of two adjacent sides).
    if near_top && near_left {
        return ResizeZone::NorthWest;
    }
    if near_top && near_right {
        return ResizeZone::NorthEast;
    }
    if near_bottom && near_left {
        return ResizeZone::SouthWest;
    }
    if near_bottom && near_right {
        return ResizeZone::SouthEast;
    }
    // Edges (within EDGE of one side).
    if cx <= EDGE {
        return ResizeZone::West;
    }
    if cx >= w - EDGE {
        return ResizeZone::East;
    }
    if cy <= EDGE {
        return ResizeZone::North;
    }
    if cy >= h - EDGE {
        return ResizeZone::South;
    }
    ResizeZone::None
}

/// Whether the link-trigger modifier is held: Ctrl on every platform, PLUS
/// Cmd (Super) additionally on macOS only — the platform's link convention.
/// `cfg!` keeps both arms compiled on both OSes.
fn link_modifier_held(m: &winit::keyboard::ModifiersState) -> bool {
    m.control_key() || (cfg!(target_os = "macos") && m.super_key())
}

/// The base ASCII letter a key event denotes for hint-mode narrowing,
/// INDEPENDENT of Alt/compose (BLOCKING 5): prefer the produced logical letter
/// (layout-correct), falling back to the physical QWERTY position when
/// Alt/Option-compose mangled the produced text into a non-letter.
fn hint_base_letter(
    physical: winit::keyboard::PhysicalKey,
    logical: &winit::keyboard::Key,
) -> Option<char> {
    use winit::keyboard::{Key, PhysicalKey};
    if let Key::Character(s) = logical {
        if s.chars().count() == 1 {
            let c = s.chars().next().unwrap().to_ascii_lowercase();
            if c.is_ascii_alphabetic() {
                return Some(c);
            }
        }
    }
    if let PhysicalKey::Code(code) = physical {
        return keycode_letter(code);
    }
    None
}

/// Map a physical letter key (KeyA..KeyZ) to its lowercase QWERTY char.
fn keycode_letter(code: winit::keyboard::KeyCode) -> Option<char> {
    use winit::keyboard::KeyCode::*;
    Some(match code {
        KeyA => 'a', KeyB => 'b', KeyC => 'c', KeyD => 'd', KeyE => 'e', KeyF => 'f',
        KeyG => 'g', KeyH => 'h', KeyI => 'i', KeyJ => 'j', KeyK => 'k', KeyL => 'l',
        KeyM => 'm', KeyN => 'n', KeyO => 'o', KeyP => 'p', KeyQ => 'q', KeyR => 'r',
        KeyS => 's', KeyT => 't', KeyU => 'u', KeyV => 'v', KeyW => 'w', KeyX => 'x',
        KeyY => 'y', KeyZ => 'z',
        _ => return None,
    })
}

/// Scheme allowlist for Ctrl+click-to-open: only http/https/file may reach
/// the platform opener (never javascript:/mailto:/arbitrary handlers).
/// ASCII case-insensitive, pure — unit-tested without spawning anything.
fn url_scheme_allowed(url: &str) -> bool {
    ["http://", "https://", "file://"]
        .iter()
        // `get` (not slicing) so a multibyte char at the boundary can't panic.
        .any(|p| url.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p)))
}

impl App {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        // Seed the theme registry (built-ins + user themes) BEFORE any theme
        // resolution below (amendment T4): otherwise a `JETTY_THEME`/config value
        // naming a USER theme would resolve to idx 0 and the custom default be lost.
        crate::themes::rebuild_registry();

        // Resolve initial theme index from JETTY_THEME env var (consults the
        // registry, so a user theme name resolves too).
        let theme_name = std::env::var("JETTY_THEME").unwrap_or_default();
        let theme_idx = jetty_core::theme_index(&theme_name).unwrap_or(0);

        // Resolve initial opacity from JETTY_OPACITY env var.
        let opacity = std::env::var("JETTY_OPACITY")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|v| v.clamp(0.0, 1.0))
            .unwrap_or(1.0);

        // Resolve initial corner radius from JETTY_CORNER_RADIUS env var.
        let corner_radius = std::env::var("JETTY_CORNER_RADIUS")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|v| v.clamp(0.0, 24.0))
            .unwrap_or(10.0);

        let debug = std::env::var("JETTY_DEBUG").is_ok();

        // Resolve initial font family from JETTY_FONT_FAMILY env var.
        let font_family = std::env::var("JETTY_FONT_FAMILY")
            .unwrap_or_else(|_| "MesloLGS NF".to_string());

        let mut app = App {
            proxy,
            window: None,
            visible: true,
            main_occluded: false,
            hotkey_manager: None,
            gpu: None,
            text: None,
            chrome_text: None,
            quad: None,
            corner_mask: None,
            bayer_reveal: None,
            phosphor: None,
            liquid: None,
            focus: None,
            crt: None,
            caret_fx: None,
            image_layer: None,
            offscreen: None,
            summon_effect: SummonEffect::Bayer,
            window_mode: WindowMode::Center,
            tab_bar_bottom: false,
            dropdown_height_pct: 0.50,
            dropdown_width_pct: 1.0,
            slide_anim: None,
            pending_dock_frames: 0,
            pending_center_frames: 0,
            pending_center_pos: None,
            focus_autohide: true,
            scrollback_lines: 10_000,
            launch_at_login: false,
            summon_hotkey: "F9".to_string(),
            shell: String::new(),
            cached_top_flush: false,
            cached_tabs_meta: Vec::new(),
            cached_tabs_sig: u64::MAX,
            applied_main_os_title: "JeTTY".to_string(),
            last_focused_window: None,
            main_focused: false,
            switching_to_settings: false,
            switching_to_detached: false,
            pending_autohide_at: None,
            dragging_dropdown: false,
            dragging_dropdown_width: false,
            wayland_warned: false,
            crt_clock: std::time::Instant::now(),
            summon_anim: None,
            caret_anim: None,
            summon_pending: false,
            summon_settle_until: None,
            settings_paint_until: None,
            corner_radius,
            tabs: Vec::new(),
            active: 0,
            theme_idx,
            // Placeholder; `apply_theme()` at the end of `new` recomputes it from the
            // config-resolved theme_idx. Resolved via the registry (seeded above).
            active_theme: jetty_core::theme_at(theme_idx),
            theme_dropdown_open: false,
            theme_scroll_offset: 0,
            opacity,
            font_logical: FONT_LOGICAL_DEFAULT,
            reflow_pending_at: None,
            reflow_resized_at: None,
            font_family,
            font_families: Vec::new(),
            font_scroll_offset: 0,
            // UI font defaults (overridden by config below): "" = platform sans,
            // 16pt = today's chrome size, so the default look is unchanged.
            ui_font_family: String::new(),
            ui_font_logical: UI_FONT_LOGICAL_DEFAULT,
            ui_font_families: Vec::new(),
            ui_font_scroll_offset: 0,
            settings_tab: 0,
            effects_scroll: 0.0,
            fx: crate::config::EffectsConfig::default(),
            // v0.16 — overridden by config below; safe defaults here.
            osc52_allow_paste: false,
            hot_reload: true,
            // Placeholder default keymap; rebuilt from cfg.keys below in `new`.
            keymap: crate::keymap::KeyMap::defaults(),
            keys: crate::config::KeyBindings::default(),
            help_rows: Vec::new(),
            config_watcher: None,
            reloading: false,
            last_written_config_hash: std::cell::Cell::new(None),
            pending_reload_at: None,
            // Run & Notify: overridden by config below; safe defaults here.
            notify_on_finish: true,
            notify_min_seconds: 10,
            notify_only_on_failure: false,
            auto_summon_on_finish: false,
            // Long-lived notification worker (idles at recv; the zbus reactor it
            // later starts idles at epoll-wait — no busy loop, ~0% idle preserved).
            notifier: crate::notify::spawn_notifier(),
            notify_last_at: std::collections::HashMap::new(),
            modifiers: winit::keyboard::ModifiersState::empty(),
            cursor: (0.0, 0.0),
            mouse_grab_press: None,
            scroll_accum: input::ScrollAccumulator::new(),
            shift_hint_until: None,
            shift_hint_cooldown: None,
            dragging_scrollbar: false,
            drag_grab_dy: 0.0,
            settings_window: None,
            settings_gpu: None,
            settings_text: None,
            settings_quad: None,
            settings_specimen_text: None,
            settings_cursor: (0.0, 0.0),
            dragging_slider: false,
            dragging_radius: false,
            active_fx_drag: None,
            selecting: false,
            link_hover: None,
            link_hover_cell: None,
            debug,
            context_menu: None,
            menu_item_rects: Vec::new(),
            menu_hover: None,
            renaming: None,
            rename_buf: String::new(),
            last_strip_click: None,
            resize_cursor: ResizeZone::None,
            welcome_open: true, // overridden below by config.show_welcome
            cfg_show_welcome: true, // overridden below by config.show_welcome
            show_perf_hud: true, // overridden below by config.show_perf_hud
            last_frame_at: None,
            perf_ms: 0.0,
            // Scope sysinfo to nothing-on-construct; the per-process refresh in
            // the render path supplies CPU data. new() with an empty RefreshKind
            // avoids the costly whole-system probe at startup.
            perf_sys: sysinfo::System::new(),
            perf_pid: sysinfo::get_current_pid().unwrap_or(sysinfo::Pid::from(0)),
            // Force the first CPU refresh to run on the first HUD frame. Use
            // checked_sub: within ~2s of boot the monotonic clock can be < 2s,
            // and the plain `Instant - Duration` panics on underflow (an app
            // launched at login on a fast-booting system would crash before the
            // first window). Falling back to `now` just defers the first refresh
            // by ≤1s — harmless.
            last_cpu_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(2))
                .unwrap_or_else(std::time::Instant::now),
            perf_cpu: 0.0,
            vt_bytes: 0,
            vt_bytes_at_window_start: 0,
            vt_window_start: std::time::Instant::now(),
            perf_mb: 0.0,
            perf_idle_at: None,
            perf_idle_shown: false,
            perf_label: None,
            perf: crate::perf::Perf::from_env(),
            frame_log: std::env::var_os("JETTY_FRAME_LOG").is_some(),
            frames_presented: 0,
            help_open: false,
            search_open: false,
            search_refresh_at: None,
            search_dirty: false,
            confirm_close: None,
            confirm_quit: false,
            last_pos: None,
            detached: Vec::new(),
            tab_drag: None,
            tab_menu: None,
            tab_menu_labels: Vec::new(),
            tab_menu_rects: Vec::new(),
            tab_menu_hover: None,
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            palette_scroll: 0,
            palette_registry: Vec::new(),
            palette_filtered: Vec::new(),
            hint_mode: None,
            copy_mode: None,
        };
        // Persisted user settings override the env-derived defaults (but env
        // vars still seed the initial values above, so an explicit JETTY_* can
        // win on a fresh config). Apply config BEFORE the first render so the
        // window comes up already themed/sized as the user left it. The font
        // size/family are consumed later by `resumed` when it builds the
        // TextLayer; theme+opacity are pushed into the terminals by apply_theme.
        let cfg = crate::config::Config::load();
        if let Some(i) = jetty_core::theme_index(&cfg.theme) {
            app.theme_idx = i;
        }
        // Clamp opacity to a VISIBLE floor: a persisted 0.0 would load a fully
        // transparent (invisible) window, which looks like a launch failure.
        app.opacity = cfg.opacity.clamp(0.1, 1.0);
        app.font_logical = cfg.font_size.clamp(6.0, 48.0);
        app.font_family = cfg.font_family;
        // UI (chrome) font, clamped like the terminal font. "" = platform sans;
        // a non-empty family is validated against the installed proportional
        // faces later in `resumed` (a removed font falls back to "" / sans).
        app.ui_font_logical = cfg.ui_font_size.clamp(UI_FONT_MIN, UI_FONT_MAX);
        app.ui_font_family = cfg.ui_font_family;
        app.corner_radius = cfg.corner_radius.clamp(0.0, 24.0);
        app.summon_effect = SummonEffect::from_config(&cfg.summon_effect);
        app.window_mode = WindowMode::from_config(&cfg.window_mode);
        app.tab_bar_bottom = cfg.tab_bar_position == "bottom";
        app.dropdown_height_pct = cfg.dropdown_height_pct.clamp(0.25, 1.0);
        app.dropdown_width_pct = cfg.dropdown_width_pct.clamp(0.2, 1.0);
        app.focus_autohide = cfg.focus_autohide;
        // Re-clamp for belt-and-suspenders (mirrors the opacity/font clamps
        // above); Config::load's sanitize pass already applied this range.
        app.scrollback_lines = cfg.scrollback_lines.clamp(100, 100_000);
        // The autostart FILE's existence is the source of truth (so the toggle
        // reflects reality even if the file was changed externally), not the
        // stored config bool.
        app.launch_at_login = autostart_file_exists();
        app.summon_hotkey = cfg.summon_hotkey;
        app.shell = cfg.shell;
        app.welcome_open = cfg.show_welcome;
        app.cfg_show_welcome = cfg.show_welcome;
        app.show_perf_hud = cfg.show_perf_hud;
        app.fx = cfg.effects.clone();
        // Run & Notify: mirror the persisted keys (min-seconds re-clamped for
        // belt-and-suspenders; Config::load's sanitize already applied the range).
        app.notify_on_finish = cfg.notify_on_command_finish;
        app.notify_min_seconds = cfg.notify_min_seconds.clamp(1, 86_400);
        app.notify_only_on_failure = cfg.notify_only_on_failure;
        app.auto_summon_on_finish = cfg.auto_summon_on_finish;
        app.osc52_allow_paste = cfg.osc52_allow_paste;
        app.hot_reload = cfg.hot_reload;
        // Compile the keybindings (defaults + user `[keys]` overrides). Any invalid
        // chord / conflict / rejected bind is logged; the rest still apply.
        app.keys = cfg.keys;
        app.keymap = crate::keymap::KeyMap::compile(&app.keys);
        for w in app.keymap.warnings() {
            eprintln!("jetty: {w}");
        }
        app.help_rows = App::compute_help_rows(&app.keymap, &app.summon_hotkey);

        // Apply the initial theme+opacity so Terminal::new env defaults are
        // overridden by our managed state (avoids double-reads from env). Also
        // populates the `active_theme` cache from the config-resolved theme_idx.
        app.apply_theme();
        app
    }

    /// Build the Help overlay rows from the CURRENT keymap (so a remap is
    /// reflected) plus the static, non-keymap rows (drag / right-click / URL open /
    /// Ctrl+D EOF / Esc). Called on load + on hot-reload; the result is cached in
    /// `self.help_rows`, so the render path never re-derives it.
    fn compute_help_rows(km: &crate::keymap::KeyMap, summon_hotkey: &str) -> Vec<String> {
        use crate::keymap::BindableAction as A;
        let all = |a: A| {
            let v = km.pretty_chords(a);
            if v.is_empty() { "(unbound)".to_string() } else { v.join(" / ") }
        };
        let first = |a: A| {
            km.pretty_chords(a)
                .into_iter()
                .next()
                .unwrap_or_else(|| "(unbound)".to_string())
        };
        vec![
            format!("{summon_hotkey} (configurable) — Summon / hide"),
            format!("{} — New tab", all(A::NewTab)),
            format!("{} — Close tab", all(A::CloseTab)),
            format!("{} / {} — Next / Prev tab", first(A::NextTab), first(A::PrevTab)),
            "Ctrl+1..9 — Jump to tab".to_string(),
            format!(
                "{} / drag tab off bar — Detach / reattach (right-click tab for menu)",
                all(A::DetachTab)
            ),
            "Double-click tab / top bar — Rename / Maximize".to_string(),
            format!(
                "{} — Command palette   (Settings: {})",
                first(A::OpenPalette),
                all(A::ToggleSettings)
            ),
            format!(
                "{} / {} / {} — Font size",
                first(A::FontUp),
                first(A::FontDown),
                first(A::FontReset)
            ),
            format!("{} / {} — Transparency", first(A::OpacityUp), first(A::OpacityDown)),
            format!("{} / {} — Copy / Paste", first(A::Copy), first(A::Paste)),
            format!(
                "{} — Search scrollback (Enter/F3 next, Shift+Enter prev, Esc close)",
                first(A::SearchToggle)
            ),
            format!(
                "{} / {} — Prev / next prompt (shell integration)",
                first(A::PrevPrompt),
                first(A::NextPrompt)
            ),
            format!(
                "{} — Hint mode: label + copy URLs/paths (Alt = open URL, Esc cancel)",
                first(A::HintMode)
            ),
            format!(
                "{} — Copy-mode: keyboard select (hjkl w/b/e v/V y=yank, Esc exit)",
                first(A::CopyMode)
            ),
            "Ctrl+click — Open URL (Ctrl+hover underlines it)".to_string(),
            "Ctrl+L — Clear".to_string(),
            "PageUp / PageDown — Scroll".to_string(),
            "Left-drag — Select text (auto-copies)".to_string(),
            "Shift+drag — Select text over mouse apps (vim/htop/Claude Code)".to_string(),
            "Right-click — Context menu (Copy/Paste/Select All/Clear/Close Tab)".to_string(),
            "Drag top bar — Move window".to_string(),
            "Drag edges/corners — Resize".to_string(),
            "Ctrl+D — Close shell (sends EOF)".to_string(),
            "Esc — Close this help".to_string(),
        ]
    }

    /// Write the current user-tweakable settings to the on-disk config file.
    /// Called whenever a setting changes (theme, opacity, font size/family,
    /// corner radius). Best-effort and cheap; errors are swallowed by `save`.
    fn persist(&self) {
        // Never write config.toml while applying a reload: that would re-trigger the
        // watcher (a burst of atomic writes) and risk a loop. Combined with the
        // hash guard, this makes reload loop-free BY CONSTRUCTION (amendment H2).
        if self.reloading {
            return;
        }
        let cfg = crate::config::Config {
            // The current theme's stable name (registry-resolved; a user theme keeps
            // its own name). `active_theme` is kept in lockstep with `theme_idx` by
            // `apply_theme`, so this is the selected theme without a registry lock.
            theme: self.active_theme.name.to_string(),
            opacity: self.opacity,
            font_size: self.font_logical,
            font_family: self.font_family.clone(),
            ui_font_family: self.ui_font_family.clone(),
            ui_font_size: self.ui_font_logical,
            corner_radius: self.corner_radius,
            summon_effect: self.summon_effect.to_config().to_string(),
            window_mode: self.window_mode.to_config().to_string(),
            dropdown_height_pct: self.dropdown_height_pct,
            dropdown_width_pct: self.dropdown_width_pct,
            focus_autohide: self.focus_autohide,
            launch_at_login: self.launch_at_login,
            summon_hotkey: self.summon_hotkey.clone(),
            shell: self.shell.clone(),
            tab_bar_position: if self.tab_bar_bottom { "bottom" } else { "top" }.to_string(),
            scrollback_lines: self.scrollback_lines,
            // show_welcome/show_perf_hud are startup preferences (no runtime UI
            // toggles them), cached at startup — write them back from memory so a
            // settings change never re-reads the config file (persist() used to do
            // TWO full Config::load() reads per call, i.e. 2–4 disk reads per
            // settings click). The cached values preserve a user's manual TOML
            // choice exactly as the on-disk read did.
            show_welcome: self.cfg_show_welcome,
            show_perf_hud: self.show_perf_hud,
            effects: self.fx.clone(),
            notify_on_command_finish: self.notify_on_finish,
            notify_min_seconds: self.notify_min_seconds,
            notify_only_on_failure: self.notify_only_on_failure,
            auto_summon_on_finish: self.auto_summon_on_finish,
            osc52_allow_paste: self.osc52_allow_paste,
            hot_reload: self.hot_reload,
            // Preserve the user's `[keys]` overrides verbatim (never editable via the
            // Settings UI — a settings-driven persist must not erase them).
            keys: self.keys.clone(),
        };
        // Record the hash of the EXACT string we're about to write so the watcher's
        // echo of our own save is recognized and skipped on reload (secondary loop
        // guard). `save()` re-serializes the same deterministic string, so this hash
        // matches the on-disk bytes the reload will read.
        if let Ok(s) = toml::to_string_pretty(&cfg) {
            self.last_written_config_hash.set(Some(hash_config_str(&s)));
        }
        cfg.save();
    }

    /// Select a new window-summon reveal effect: persist it, fire a one-shot
    /// PREVIEW summon on the main window so the user immediately SEES the effect,
    /// and redraw the settings window so the new effect name shows.
    fn set_summon_effect(&mut self, effect: SummonEffect) {
        if self.summon_effect == effect {
            return;
        }
        self.summon_effect = effect;
        self.persist();
        // One-shot preview on the main window (self-driving loop handles idle-0).
        self.summon_pending = true;
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// The active tab. Panics if `tabs` is empty, which only happens before
    /// `resumed` has run or after the last tab closed (we exit then).
    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Mutable access to the active tab. Same non-empty invariant as `active_tab`.
    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// The current theme with the global `opacity` applied to its bg alpha.
    ///
    /// HOT PATH (amendment T1): called every frame by the tab bar / modals. It clones
    /// the CACHED `active_theme` (registry-resolved once in `apply_theme`) and stamps
    /// the live opacity — it never locks the theme registry or re-resolves per frame.
    /// Opacity is applied here (not baked into the cache) so an opacity change is live
    /// without invalidating the cache.
    fn current_theme(&self) -> jetty_core::Theme {
        let mut t = self.active_theme.clone();
        t.bg[3] = (self.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        t
    }

    /// Largest valid `theme_scroll_offset` so the open dropdown's last page is full.
    fn max_theme_scroll(&self) -> usize {
        jetty_core::theme_count().saturating_sub(MAX_THEME_ROWS)
    }

    /// Re-resolve the cached `active_theme` from `theme_idx` (via the registry, never
    /// direct-indexing — a stale idx falls back safely), apply `opacity`, and push
    /// the themed palette into EVERY tab's terminal — including the tabs living in
    /// detached windows, so a live theme/opacity change repaints them too (visual
    /// parity: one redraw request each, no polling). Non-persisting (safe on reload).
    fn apply_theme(&mut self) {
        // Refresh the cache from the current index (registry-resolved; `theme_at`
        // never panics on a stale/out-of-range index).
        self.active_theme = jetty_core::theme_at(self.theme_idx);
        let t = self.current_theme();
        for tab in &mut self.tabs {
            tab.terminal.set_theme(t.clone());
        }
        for dw in &mut self.detached {
            dw.tab.terminal.set_theme(t.clone());
            dw.request_paint();
        }
    }

    /// Apply a debounced config + themes hot-reload. Runs on the UI thread from
    /// `about_to_wait`. Non-destructive and loop-free by construction:
    ///
    /// * THEMES are ALWAYS rebuilt + reapplied (amendment T3): editing the active
    ///   theme file leaves config.toml untouched, so the config hash-skip must not
    ///   gate the repaint. `theme_idx` is re-resolved by NAME and re-clamped against
    ///   the rebuilt registry (amendment T2), then `apply_theme` repaints all tabs +
    ///   detached with the (possibly changed) palette.
    /// * CONFIG is parsed NON-DESTRUCTIVELY (amendment H1): a parse error keeps the
    ///   in-memory state and waits for the next event — never `.bad`, never defaults.
    ///   A file whose content hashes to our own last write is skipped (self-write
    ///   echo). `self.reloading` disables `persist()` for the whole apply, so no live
    ///   key can write config.toml back (amendment H2 — loop-free by construction).
    fn reload_config_and_themes(&mut self) {
        self.reloading = true;

        // (A) Themes — always. Rebuild the registry from disk, then re-resolve the
        // active theme BY NAME against it (indices may have shifted) and repaint.
        crate::themes::rebuild_registry();
        let cur_name = self.active_theme.name.to_string();
        self.theme_idx = jetty_core::theme_index(&cur_name)
            .unwrap_or(0)
            .min(jetty_core::theme_count().saturating_sub(1));
        self.apply_theme(); // refreshes active_theme (new palette) + fans out to all surfaces

        // (B) Config — non-destructive, hash-guarded.
        if let Ok(s) = std::fs::read_to_string(crate::config::Config::config_path()) {
            let h = hash_config_str(&s);
            // Skip our own write echoing back through the watcher.
            if self.last_written_config_hash.get() != Some(h) {
                if let Some(cfg) = crate::config::Config::parse_reload(&s) {
                    self.apply_reloaded_config(cfg);
                }
                // Record the observed hash so an identical later hand-save no-ops too.
                self.last_written_config_hash.set(Some(h));
            }
        }

        self.reloading = false;
        // Repaint chrome (theme/settings) once the reload settled.
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// Apply an externally-edited `Config` LIVE, diffing against current in-memory
    /// state and touching only changed keys. Runs with `self.reloading == true`, so
    /// every setter it calls is non-persisting (they early-return in `persist`).
    ///
    /// Keys mid-DRAG in the Settings panel are skipped (amendment H4): the in-flight
    /// interactive value wins over a concurrent external edit. `summon_hotkey` and
    /// `launch_at_login` are RESTART/external-only and deliberately NOT applied here.
    fn apply_reloaded_config(&mut self, cfg: crate::config::Config) {
        let eps = f32::EPSILON;

        // Theme (by name → registry index; never direct-index).
        if let Some(idx) = jetty_core::theme_index(&cfg.theme) {
            if idx != self.theme_idx {
                self.theme_idx = idx;
                self.apply_theme();
            }
        }
        // Opacity — skip while the user is dragging the opacity slider (H4).
        if !self.dragging_slider {
            let op = cfg.opacity.clamp(0.1, 1.0);
            if (op - self.opacity).abs() > eps {
                self.opacity = op;
                self.apply_theme();
            }
        }
        // Terminal font size / family (real setter cores rebuild the atlas + reflow).
        let fs = cfg.font_size.clamp(6.0, 48.0);
        if (fs - self.font_logical).abs() > eps {
            self.set_font_size(fs);
        }
        if cfg.font_family != self.font_family {
            self.set_font_family(cfg.font_family.clone());
        }
        // UI (chrome) font size / family.
        let ufs = cfg.ui_font_size.clamp(UI_FONT_MIN, UI_FONT_MAX);
        if (ufs - self.ui_font_logical).abs() > eps {
            self.set_ui_font_size(ufs);
        }
        if cfg.ui_font_family != self.ui_font_family {
            self.set_ui_font_family(cfg.ui_font_family.clone());
        }
        // Corner radius — skip while dragging the radius slider (H4).
        if !self.dragging_radius {
            let cr = cfg.corner_radius.clamp(0.0, 24.0);
            if (cr - self.corner_radius).abs() > eps {
                self.corner_radius = cr;
                self.request_main_paint();
            }
        }
        // Summon effect: ASSIGN directly (NOT set_summon_effect, which fires a one-
        // shot preview animation on every reload — amendment).
        let se = SummonEffect::from_config(&cfg.summon_effect);
        if se != self.summon_effect {
            self.summon_effect = se;
        }
        // Window mode: needs the real setter (docks/undocks; a bare assign is only
        // half-applied).
        let wm = WindowMode::from_config(&cfg.window_mode);
        if wm != self.window_mode {
            self.set_window_mode(wm);
        }
        // Tab-bar position.
        let bottom = cfg.tab_bar_position == "bottom";
        if bottom != self.tab_bar_bottom {
            self.set_tab_bar_bottom(bottom);
        }
        // Dropdown height/width — skip the one being dragged (H4); re-dock a docked
        // window on change.
        if !self.dragging_dropdown {
            let dh = cfg.dropdown_height_pct.clamp(0.25, 1.0);
            if (dh - self.dropdown_height_pct).abs() > eps {
                self.dropdown_height_pct = dh;
                self.redock_if_dropdown();
            }
        }
        if !self.dragging_dropdown_width {
            let dw = cfg.dropdown_width_pct.clamp(0.2, 1.0);
            if (dw - self.dropdown_width_pct).abs() > eps {
                self.dropdown_width_pct = dw;
                self.redock_if_dropdown();
            }
        }
        // Focus auto-hide.
        self.focus_autohide = cfg.focus_autohide;
        // Scrollback (live to every tab + detached).
        let sb = cfg.scrollback_lines.clamp(100, 100_000);
        if sb != self.scrollback_lines {
            self.set_scrollback_lines(sb);
        }
        // Perf HUD: changes the reserved status-bar height → grid rows, so reflow.
        if cfg.show_perf_hud != self.show_perf_hud {
            self.show_perf_hud = cfg.show_perf_hud;
            self.reflow();
            self.request_main_paint();
        }
        // Visual effects — skip while a Effects slider is being dragged (H4).
        if self.active_fx_drag.is_none() && cfg.effects != self.fx {
            self.fx = cfg.effects.clone();
            self.request_main_paint();
            for dw in &self.detached {
                dw.request_paint();
            }
        }
        // Run & Notify mirrors.
        self.notify_on_finish = cfg.notify_on_command_finish;
        self.notify_min_seconds = cfg.notify_min_seconds.clamp(1, 86_400);
        self.notify_only_on_failure = cfg.notify_only_on_failure;
        self.auto_summon_on_finish = cfg.auto_summon_on_finish;
        // OSC 52 paste: apply LIVE to every existing tab (the setter preserves each
        // tab's scrollback), so it is not merely "new tabs only".
        if cfg.osc52_allow_paste != self.osc52_allow_paste {
            self.osc52_allow_paste = cfg.osc52_allow_paste;
            for tab in &mut self.tabs {
                tab.terminal.set_osc52_allow_paste(cfg.osc52_allow_paste);
            }
            for dw in &mut self.detached {
                dw.tab.terminal.set_osc52_allow_paste(cfg.osc52_allow_paste);
            }
        }
        // Hot-reload toggle: turning it OFF live drops the watcher (stops watching).
        // Turning it ON when it was off is restart-only (no watcher exists to detect
        // the change) — documented.
        self.hot_reload = cfg.hot_reload;
        if !self.hot_reload {
            self.config_watcher = None;
        }
        // Mirror the RESTART-ONLY-EFFECT keys too, so a later panel-driven persist()
        // round-trips the user's external edit instead of clobbering it with the
        // stale startup value. Their live EFFECTS stay restart-only (summon_hotkey is
        // re-read at startup; launch_at_login's source of truth is the autostart file,
        // so it is deliberately NOT mirrored here) — but the on-disk value must
        // survive an external edit + a subsequent unrelated Settings change.
        self.summon_hotkey = cfg.summon_hotkey.clone();
        self.cfg_show_welcome = cfg.show_welcome;
        // shell: mirror so new tabs spawned after the reload use the edited shell.
        self.shell = cfg.shell.clone();
        // Keybindings — LIVE (not restart-only). Recompile only when the `[keys]`
        // table actually changed (compare the compiled maps, so an unrelated reload
        // skips the rebuild). No redraw needed; the next keypress uses the new map.
        if cfg.keys != self.keys {
            let new_km = crate::keymap::KeyMap::compile(&cfg.keys);
            for w in new_km.warnings() {
                eprintln!("jetty: {w}");
            }
            self.keys = cfg.keys.clone();
            self.keymap = new_km;
            self.help_rows = App::compute_help_rows(&self.keymap, &self.summon_hotkey);
        }
    }

    /// Re-dock the main window to the top strip when it is a visible Dropdown — used
    /// after a live dropdown width/height change so it re-docks immediately.
    fn redock_if_dropdown(&mut self) {
        if self.visible && self.window_mode == WindowMode::Dropdown {
            if let Some(w) = &self.window {
                dock_window_top(w, self.dropdown_width_pct, self.dropdown_height_pct);
                self.pending_dock_frames = 5;
                self.request_main_paint();
            }
        }
    }

    /// Allocate a surface-sized offscreen color texture (same format as the
    /// surface) usable as a render target AND a sampled texture. Used ONLY by the
    /// Tier-B summon effects, which render the scene into it then sample it.
    fn make_offscreen(gpu: &GpuContext) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("summon-offscreen"),
            size: wgpu::Extent3d {
                width: gpu.config.width.max(1),
                height: gpu.config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: gpu.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    /// Compute the current grid (cols, rows) from the GPU surface size and cell
    /// metrics, accounting for the tab bar. Falls back to the constants when the
    /// renderer is not yet available.
    fn grid_dims(&self) -> (usize, usize) {
        let status_h = self.status_h();
        let (Some(gpu), Some(text)) = (&self.gpu, &self.text) else {
            return (FALLBACK_COLS, FALLBACK_ROWS);
        };
        let (cw, ch) = text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return (FALLBACK_COLS, FALLBACK_ROWS);
        }
        let cols = ((gpu.config.width as f32 - SCROLLBAR_GUTTER) / cw).floor().max(2.0) as usize;
        let rows = ((gpu.config.height as f32 - TABBAR_H - status_h) / ch).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Pixel Y origin of the terminal grid. The bar always costs `TABBAR_H` of
    /// grid HEIGHT regardless of side, but the grid's pixel ORIGIN is 0 when the
    /// bar is at the bottom (grid fills from the top) and `TABBAR_H` when it's at
    /// the top (grid starts below the bar).
    fn grid_top_offset(&self) -> f32 {
        if self.tab_bar_bottom { 0.0 } else { TABBAR_H }
    }

    /// Pixel height reserved at the BOTTOM of the window for the status bar (the
    /// perf HUD). `STATUS_H` when the HUD is enabled, else 0 (no bar, grid uses the
    /// full height). The grid and the bottom-mode tab bar both sit above it.
    fn status_h(&self) -> f32 {
        if self.show_perf_hud { STATUS_H } else { 0.0 }
    }

    /// Pixel Y of the tab bar's top edge for a surface of physical `height`.
    /// 0 when the bar is at the top; `height - TABBAR_H - status_h` at the bottom
    /// (the status bar always sits below the bottom-mode tab bar).
    fn tabbar_y(&self, height: f32) -> f32 {
        if self.tab_bar_bottom {
            (height - TABBAR_H - self.status_h()).max(0.0)
        } else {
            0.0
        }
    }

    /// The configured shell override for `PtySession::spawn`: `None` when the
    /// `shell` config key is empty (auto-detect), else the configured path.
    fn opt_shell(&self) -> Option<String> {
        if self.shell.is_empty() {
            None
        } else {
            Some(self.shell.clone())
        }
    }

    /// Display name for the SHELL cycler band: "System default" for the empty
    /// (auto-detect) selection, else the basename of the configured shell path.
    fn shell_display(&self) -> String {
        shell_display_name(&self.shell)
    }

    /// Cycle the selected shell. The option list is `["", ...detect_shells()]`
    /// (index 0 = "System default" = auto-detect). Finds the current selection
    /// (defaulting to index 0 when `self.shell` is empty or no longer present),
    /// steps with wraparound, persists, and redraws. New tabs pick the change up
    /// immediately via `opt_shell()`; existing tabs/shells are untouched.
    fn cycle_shell(&mut self, forward: bool) {
        let mut options: Vec<String> = Vec::new();
        options.push(String::new()); // index 0 = System default
        options.extend(detect_shells());
        let cur = options
            .iter()
            .position(|s| s == &self.shell)
            .unwrap_or(0);
        let n = options.len();
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.shell = options[next].clone();
        self.persist();
        self.request_settings_paint();
    }

    /// Spawn a new tab in the main window starting in the active tab's shell
    /// cwd, sampled at the instant of the action (the shell's own pid, not its
    /// foreground child — by design). Falls back to spawn-dir behavior when it
    /// can't be read.
    fn new_tab(&mut self) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.pty.cwd());
        self.new_tab_with_cwd(cwd);
    }

    /// Spawn a new tab sized to the current grid, themed like the others, make it
    /// active, and redraw. The new PTY shares the same wake proxy so one
    /// `AppEvent::Wake` drains every tab. `cwd` is the directory the new shell
    /// starts in (`None` = today's spawn-dir/home behavior).
    fn new_tab_with_cwd(&mut self, cwd: Option<std::path::PathBuf>) {
        let (cols, rows) = self.grid_dims();
        let proxy_wake = self.proxy.clone();
        let shell = self.opt_shell();
        // Report the text-area pixel size so image tools scale correctly from the
        // start (A5); 0 when the font metrics aren't ready yet.
        let (px_w, px_h) = self
            .text
            .as_ref()
            .map(|t| {
                let (cw, ch) = t.cell_size();
                ((cols as f32 * cw).min(65535.0) as u16, (rows as f32 * ch).min(65535.0) as u16)
            })
            .unwrap_or((0, 0));
        let pty = match PtySession::spawn(cols as u16, rows as u16, px_w, px_h, shell, cwd, move || {
            let _ = proxy_wake.send_event(AppEvent::Wake);
        }) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("jetty: failed to spawn tab PTY: {e}");
                return;
            }
        };
        let writer = pty.writer();
        let mut terminal = Terminal::new(cols, rows);
        terminal.set_theme(self.current_theme());
        // Seed the sixel cell-px metric from the live grid font so an image fed
        // before the first reflow reserves the right number of rows.
        if let Some((cw, ch)) = self.text.as_ref().map(|t| t.cell_size()) {
            terminal.set_cell_px(cw, ch);
        }
        // OSC 52 paste (remote clipboard READ) is opt-in and off by default (secure).
        // Applied at spawn so new tabs pick up the current setting.
        terminal.set_osc52_allow_paste(self.osc52_allow_paste);
        // Apply the configured scrollback cap (guard skips the no-op
        // set_options round-trip on the 10k default path).
        if self.scrollback_lines != 10_000 {
            terminal.set_scrollback_lines(self.scrollback_lines);
        }
        // Surface the shell-fallback notice here too (F2).
        if let Some(notice) = pty.startup_notice() {
            terminal.feed(format!("\x1b[33m{notice}\x1b[0m\r\n").as_bytes());
        }
        let title = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab {
            terminal,
            pty,
            writer,
            default_title: title.clone(),
            title,
            manually_renamed: false,
            activity: jetty_render::TabActivity::None,
        });
        self.active = self.tabs.len() - 1;
        self.request_main_paint();
    }

    /// Close tab `i` (its PtySession Drop kills the child). Fix up `active`. If
    /// no tabs remain ANYWHERE (main window or detached), exit the event loop;
    /// when detached windows still hold live shells, the first detached tab is
    /// pulled back into the main window instead — exiting would drop every
    /// `DetachedWindow` and silently SIGKILL their shells mid-job.
    fn close_tab(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        if i >= self.tabs.len() {
            return;
        }
        if i == self.active {
            // The searched (active) tab is going away; the bar must not stay
            // open silently retargeting whichever tab becomes active (F2/F7).
            self.search_close();
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            if self.detached.is_empty() {
                event_loop.exit();
                return;
            }
            // Adopt the first detached tab (its window closes; the shell
            // survives) and continue with the normal fix-ups below.
            self.reattach_tab(0, event_loop);
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > i {
            self.active -= 1;
        }
        // Keep index-bearing UI state aligned with the removed tab so the wrong
        // tab is never renamed/confirmed, and any in-progress selection is reset.
        Self::adjust_index_after_remove(&mut self.renaming, i);
        Self::adjust_index_after_remove(&mut self.confirm_close, i);
        if self.renaming.is_none() {
            self.rename_buf.clear();
        }
        self.selecting = false;
        // The tab menu / a held tab drag hold raw indices; the layout just
        // changed under them, so drop both (transient state, cheap to reopen).
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
        self.tab_drag = None;
        // A new tab is under the pointer: revalidate the cached Ctrl+hover
        // underline against ITS grid (Ctrl+Shift+W keeps Ctrl held) (F12).
        self.update_link_hover(true);
        self.request_main_paint();
    }

    /// Move tab `idx` out of the main window into a new `DetachedWindow`.
    ///
    /// Guarded by `can_detach`: requires ≥ 2 tabs so the main window is never left
    /// empty. The `Tab` (PTY + terminal grid) is moved by value; the shell is never
    /// restarted. Ctrl+Shift+D passes the active index; the tab context menu and
    /// the drag-out gesture pass an arbitrary one.
    ///
    /// `drop_global` is the desired GLOBAL top-left for the new window (the
    /// drag-out release position), clamped on-screen. `None` (hotkey / menu, or
    /// Wayland where the global cursor is unknowable) keeps the platform's
    /// default placement, exactly as before.
    fn detach_tab(
        &mut self,
        idx: usize,
        event_loop: &ActiveEventLoop,
        drop_global: Option<(f64, f64)>,
    ) {
        if !crate::detached::can_detach(self.tabs.len()) {
            return; // keep at least one tab in the main window
        }
        // Original active index, kept so the detach can be fully unwound if the
        // detached window's GPU/window init fails (see the Err arm below).
        let prev_active = self.active;
        let Some(mut tab) = crate::detached::take_tab(&mut self.tabs, idx) else {
            return;
        };
        // Keep the main window's active index valid after the removal, and keep
        // index-bearing UI state aligned with the removed tab (same fix-ups as
        // `close_tab` — the tab left this window either way).
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if self.active > idx {
            self.active -= 1;
        }
        Self::adjust_index_after_remove(&mut self.renaming, idx);
        Self::adjust_index_after_remove(&mut self.confirm_close, idx);
        if self.renaming.is_none() {
            self.rename_buf.clear();
        }
        // The tab menu / a held tab drag hold raw indices; the layout just
        // changed under them, so drop both — same invariant as `close_tab` /
        // `close_exited_tabs` (a stale index would rename/close/tear the
        // WRONG tab after Ctrl+Shift+D with the menu open).
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
        self.tab_drag = None;
        // A selection drag in progress belonged to the tab that just left; without
        // clearing this, every later CursorMoved would stretch the NOW-active
        // tab's stale selection and the release would clobber the clipboard with
        // text the user never selected. Same fix-up close_tab does (F27).
        self.selecting = false;

        // Search state travels inside the Terminal, but detached windows
        // never render it: if the searched (active) tab is leaving, close the
        // bar and drop its matches so no invisible state rides along. The bar
        // stays open (showing the next tab's usually-empty query) only when a
        // NON-active tab is detached via its context menu.
        if self.search_open && idx == prev_active {
            self.search_open = false;
            tab.terminal.search_clear();
        }
        // Apply the current theme to the detached tab before it leaves.
        tab.terminal.set_theme(self.current_theme());
        // The tab becomes the visible tab of its own window; drop any pending
        // indicator so it can't resurface stale on a later reattach.
        tab.activity = jetty_render::TabActivity::None;

        // Derive LOGICAL window size from the GPU physical surface size.
        // `build_window` takes logical px; dividing by scale_factor converts.
        let (w_logical, h_logical) = if let (Some(gpu), Some(win)) = (&self.gpu, &self.window) {
            let scale = win.scale_factor();
            (
                (gpu.config.width as f64 / scale).round() as u32,
                (gpu.config.height as f64 / scale).round() as u32,
            )
        } else {
            (1000, 640) // fallback when GPU not yet initialised
        };

        // Focus is about to move to the new detached window, which makes the main
        // window receive Focused(false). Flag it so the auto-hide there does NOT
        // fire (the user is staying inside Jetty) — mirrors the Settings path.
        // Some platforms deliver the main Focused(false) BEFORE the detached
        // Focused(true), so set this now, before the window is created.
        self.switching_to_detached = true;

        // Build the detached window with the same font settings as the main
        // window. On GPU/window init failure the constructor hands the tab back
        // intact: re-insert it where it came from, restore the active index, and
        // abort the detach — never panic (which would SIGKILL every shell).
        let mut dw = match crate::detached::DetachedWindow::new(
            event_loop,
            tab,
            w_logical,
            h_logical,
            self.font_logical,
            self.ui_font_logical,
            &self.font_family,
            &self.ui_font_family,
        ) {
            Ok(dw) => dw,
            Err(tab) => {
                let at = idx.min(self.tabs.len());
                self.tabs.insert(at, tab);
                self.active = prev_active.min(self.tabs.len().saturating_sub(1));
                self.switching_to_detached = false;
                self.request_main_paint();
                return;
            }
        };

        // Drag-out placement: put the new window's top-left at the release
        // cursor's global position, clamped so it stays on the monitor. When no
        // monitor info is available, use the raw position; on Wayland
        // set_outer_position is a no-op (accepted degradation, no DE code).
        //
        // MIXED-DPI (F9): `drop_global` is main-window-scale physical px (main
        // outer_position + cursor), but each monitor's position()/size() is in
        // ITS OWN scale's physical px — on a mixed-DPI macOS setup those spaces
        // are not comparable, so the containment test picked the wrong monitor and
        // the clamp pinned the window off the drop point. Do the whole
        // containment+clamp in scale-INDEPENDENT LOGICAL points (a single unified
        // desktop space on both macOS and X11) and set a LogicalPosition, so winit
        // maps it back per the target display. At a uniform scale (X11) this is a
        // no-op, so the working path is unchanged.
        if let Some((gx, gy)) = drop_global {
            let main_scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
            // Drop point and window size in logical points.
            let drop_lx = gx / main_scale;
            let drop_ly = gy / main_scale;
            let dw_scale = dw.window.scale_factor();
            let ws = dw.window.outer_size();
            let win_lw = ws.width as f64 / dw_scale;
            let win_lh = ws.height as f64 / dw_scale;
            // A monitor's logical rect = its physical rect / its OWN scale.
            let mon_logical = |m: &winit::monitor::MonitorHandle| {
                let p = m.position();
                let s = m.size();
                let sc = m.scale_factor();
                (p.x as f64 / sc, p.y as f64 / sc, s.width as f64 / sc, s.height as f64 / sc)
            };
            let contains = |m: &winit::monitor::MonitorHandle| {
                let (mx, my, mw, mh) = mon_logical(m);
                drop_lx >= mx && drop_lx < mx + mw && drop_ly >= my && drop_ly < my + mh
            };
            let target = dw
                .window
                .available_monitors()
                .find(contains)
                .or_else(|| {
                    dw.window.available_monitors().min_by(|a, b| {
                        let d = |m: &winit::monitor::MonitorHandle| {
                            let (mx, my, mw, mh) = mon_logical(m);
                            let cx = mx + mw / 2.0;
                            let cy = my + mh / 2.0;
                            (drop_lx - cx).powi(2) + (drop_ly - cy).powi(2)
                        };
                        d(a).total_cmp(&d(b))
                    })
                })
                .or_else(|| dw.window.current_monitor());
            let (lx, ly) = match target {
                Some(mon) => {
                    let (mx, my, mw, mh) = mon_logical(&mon);
                    // Clamp the top-left (in logical points) so the whole window
                    // stays on the target monitor. Sub-pixel logical placement is
                    // irrelevant, so round to integers and reuse clamp_pos.
                    let (cx, cy) = crate::detached::clamp_pos(
                        drop_lx.round() as i32,
                        drop_ly.round() as i32,
                        win_lw.round() as u32,
                        win_lh.round() as u32,
                        (mx.round() as i32, my.round() as i32, mw.round() as u32, mh.round() as u32),
                    );
                    (cx as f64, cy as f64)
                }
                None => (drop_lx, drop_ly),
            };
            dw.window
                .set_outer_position(winit::dpi::LogicalPosition::new(lx, ly));
        }

        // Reflow the moved tab to the detached window's grid: the client area
        // minus its own chrome (top bar + status strip when the perf HUD is on)
        // and the scrollbar gutter. Use the detached window's OWN GPU surface
        // size and cell size (not `self.grid_dims()` — different surface).
        let (cw, ch) = dw.text.cell_size();
        let (cols, rows) = crate::detached::grid_dims(
            dw.gpu.config.width as f32,
            dw.gpu.config.height as f32,
            cw,
            ch,
            SCROLLBAR_GUTTER,
            TABBAR_H,
            self.status_h(),
        );
        dw.tab.terminal.resize(cols, rows);
        dw.tab.terminal.set_cell_px(cw, ch);
        dw.tab.pty.resize(
            cols as u16,
            rows as u16,
            (cols as f32 * cw).min(65535.0) as u16,
            (rows as f32 * ch).min(65535.0) as u16,
        );

        self.detached.push(dw);

        // A different tab now sits under the main-window pointer (Ctrl+Shift+D
        // keeps Ctrl held): revalidate the cached Ctrl+hover underline (F12).
        self.update_link_hover(true);
        // Redraw the main window so the tab bar reflects the removed tab.
        self.request_main_paint();
    }

    /// Move a detached window's tab back into the main window (reattach),
    /// closing the detached OS window in the process.
    ///
    /// `dw.tab` is bound out of `dw` *before* `dw` is allowed to drop, so the
    /// `Tab` (PTY + shell child) survives — dropping `DetachedWindow` while it
    /// still owned the tab would reap the shell. The window/GPU surface still
    /// gets torn down correctly when `dw` drops at the end of this function.
    fn reattach_tab(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        if pos >= self.detached.len() {
            return;
        }
        // The reattached tab becomes the active one below: close the search
        // bar and clear the outgoing active tab's state first (F2/F7/F15).
        self.search_close();
        let dw = self.detached.remove(pos);
        // Drop focus bookkeeping that pointed at the now-destroyed detached window
        // so the main window's auto-hide guard doesn't keep suppressing on a stale
        // id/flag (mirrors `close_settings_window`).
        let dw_id = dw.window.id();
        if self.last_focused_window == Some(dw_id) {
            self.last_focused_window = None;
        }
        self.switching_to_detached = false;
        let mut tab = dw.tab; // move the Tab out before `dw` drops
        // It was visible in its own window until now — no unseen activity.
        tab.activity = jetty_render::TabActivity::None;

        // Reflow to the MAIN window's grid (tab bar accounted for).
        let (cols, rows) = self.grid_dims();
        tab.terminal.resize(cols, rows);
        if let Some((cw, ch)) = self.text.as_ref().map(|t| t.cell_size()) {
            tab.terminal.set_cell_px(cw, ch);
        }
        tab.pty.resize(
            cols as u16,
            rows as u16,
            self.text.as_ref().map(|t| (cols as f32 * t.cell_size().0).min(65535.0) as u16).unwrap_or(0),
            self.text.as_ref().map(|t| (rows as f32 * t.cell_size().1).min(65535.0) as u16).unwrap_or(0),
        );

        self.tabs.push(tab);
        self.active = crate::detached::reattach_index(self.tabs.len());
        self.apply_theme();

        // If the main window is hidden (e.g. the last main tab's shell exited
        // while hidden and close_exited_tabs reattached a detached tab to keep its
        // shell alive), summon it — otherwise the user's live shell would be
        // parked in an invisible window, looking dead until the next F9 (F15). The
        // drag-to-reattach path only runs while visible, so this is a no-op there.
        if !self.visible {
            self.set_visibility(true, event_loop);
        }

        // The reattached tab is now the active one under the pointer:
        // revalidate the cached Ctrl+hover underline against its grid (F12).
        self.update_link_hover(true);
        // `dw` drops here: detached window + GPU surface are closed/destroyed.
        self.request_main_paint();
    }

    /// Dismiss the terminal Copy/Paste context menu AND the tab context menu,
    /// clearing their cached hit rects and hover state. The item rects are
    /// ABSOLUTE positions cached once at open (the menu clamps against the
    /// window size then); a window resize re-clamps the DRAWN menu against the
    /// new size while hover/click would keep hit-testing the stale cache —
    /// clicking the visible row would do nothing and clicking where the menu
    /// used to be would fire an invisible action. Closing on resize is the
    /// standard (and cheapest correct) behavior.
    fn dismiss_menus(&mut self) {
        self.context_menu = None;
        self.menu_hover = None;
        self.menu_item_rects.clear();
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
    }

    /// Adjust an `Option<usize>` index after the tab at `removed` is removed:
    /// clear it if it pointed AT the removed tab; decrement it if it pointed to a
    /// later tab (so it keeps referring to the same logical tab).
    fn adjust_index_after_remove(idx: &mut Option<usize>, removed: usize) {
        match *idx {
            Some(j) if j == removed => *idx = None,
            Some(j) if j > removed => *idx = Some(j - 1),
            _ => {}
        }
    }

    /// Close the scrollback-search bar and clear the ACTIVE tab's search
    /// state (query, compiled regex, matches). The single close path for
    /// Esc / ✕ / Ctrl+Shift+F — and for every active-tab change while the
    /// bar is open (tab switch/select/reattach/close): the bar targets the
    /// active tab, so leaving a searched tab in the background would strand
    /// a compiled regex + match list on it, and `Terminal::resize` would
    /// re-scan that tab's ENTIRE scrollback on every reflow forever
    /// (F2/F7/F15 — an invisible, permanent resize-path slowdown).
    fn search_close(&mut self) {
        if !self.search_open {
            return;
        }
        self.search_open = false;
        self.search_dirty = false;
        // Tolerate an empty tabs vec (reattach-from-close_exited_tabs path).
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.terminal.search_clear();
        }
        self.request_main_paint();
    }

    // ── Hint mode (Ctrl+Shift+H) + keyboard copy-mode (Ctrl+Shift+Space) ──────

    /// True while another overlay owns the keyboard, so the hint/copy-mode chords
    /// cannot start a mode (single-owner rule). Palette/search/rename/confirm
    /// capture the chord BEFORE `decide_key` runs; welcome + help only capture
    /// Esc, so they are checked explicitly here (amendment 6 — "cannot enter
    /// while another owns keys", INCLUDING welcome, for parity).
    fn overlay_owns_keys(&self) -> bool {
        self.confirm_quit
            || self.confirm_close.is_some()
            || self.renaming.is_some()
            || self.palette_open
            || self.welcome_open
            || self.help_open
            || self.search_open
    }

    /// Enter hint mode: scan the visible URL/path/hash/IPv4 tokens ONCE and show
    /// their labels. No-op on the alt screen, while another overlay owns keys, or
    /// when the scan finds ZERO tokens (n=0 auto-exit — never trap the user in an
    /// empty mode requiring Esc).
    fn enter_hint_mode(&mut self) {
        if self.overlay_owns_keys() || self.copy_mode.is_some() {
            return;
        }
        if self.active_tab().terminal.alt_screen() {
            return;
        }
        let tokens = self.active_tab().terminal.hint_tokens();
        if tokens.is_empty() {
            return;
        }
        let labels = jetty_core::hints::assign_labels(tokens.len());
        self.hint_mode = Some(HintState { tokens, labels, typed: String::new() });
        self.request_main_paint();
    }

    /// Cancel hint mode (Esc / after firing).
    fn exit_hint_mode(&mut self) {
        self.hint_mode = None;
        self.request_main_paint();
    }

    /// Handle one key while hint mode owns the keyboard. Letters narrow the typed
    /// prefix (matched against the BASE ASCII letter, independent of Alt/compose —
    /// BLOCKING 5); an exact label match COPIES the token (default) or, for a URL
    /// with Alt held at completion, OPENS it. Esc cancels; Backspace pops; every
    /// other key is swallowed.
    fn hint_mode_key(&mut self, physical: winit::keyboard::PhysicalKey, logical: &winit::keyboard::Key) {
        use winit::keyboard::{Key, NamedKey};
        match logical {
            Key::Named(NamedKey::Escape) => {
                self.exit_hint_mode();
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(hs) = self.hint_mode.as_mut() {
                    hs.typed.pop();
                }
                self.request_main_paint();
                return;
            }
            _ => {}
        }
        let Some(ch) = hint_base_letter(physical, logical) else {
            return; // non-letter key: swallow
        };
        enum Outcome {
            Fire(jetty_core::HintToken),
            Narrow(String),
            Ignore,
        }
        let outcome = {
            let Some(hs) = self.hint_mode.as_ref() else { return };
            let mut typed = hs.typed.clone();
            typed.push(ch);
            if let Some(idx) = hs.labels.iter().position(|l| *l == typed) {
                Outcome::Fire(hs.tokens[idx].clone())
            } else if hs.labels.iter().any(|l| l.starts_with(&typed)) {
                Outcome::Narrow(typed)
            } else {
                Outcome::Ignore
            }
        };
        match outcome {
            Outcome::Fire(tok) => {
                // Alt is read from the live modifier state at completion,
                // decoupled from the label letter (BLOCKING 5). Alt = open ONLY
                // for a URL; every other kind always copies.
                if tok.kind == jetty_core::TokenKind::Url && self.modifiers.alt_key() {
                    App::open_url(&tok.text);
                } else {
                    crate::clipboard::set(&tok.text);
                }
                self.exit_hint_mode();
            }
            Outcome::Narrow(t) => {
                if let Some(hs) = self.hint_mode.as_mut() {
                    hs.typed = t;
                }
                self.request_main_paint();
            }
            Outcome::Ignore => {}
        }
    }

    /// Enter copy-mode: a keyboard vi-cursor over the viewport + scrollback.
    /// No-op on the alt screen or while another overlay owns keys. Clears any
    /// leftover mouse selection on enter so the old highlight never lingers.
    fn enter_copy_mode(&mut self) {
        if self.overlay_owns_keys() || self.hint_mode.is_some() {
            return;
        }
        if self.active_tab().terminal.alt_screen() {
            return;
        }
        let snap = self.active_tab().terminal.snapshot();
        let (row, col) = if snap.cursor_visible {
            (
                snap.cursor_row.min(snap.rows.saturating_sub(1)),
                snap.cursor_col.min(snap.cols.saturating_sub(1)),
            )
        } else {
            (snap.rows.saturating_sub(1), 0)
        };
        self.active_tab_mut().terminal.selection_clear();
        self.copy_mode = Some(crate::copymode::CopyMode::new(row, col));
        self.request_main_paint();
    }

    /// Exit copy-mode (Esc / after yank).
    fn exit_copy_mode(&mut self) {
        self.copy_mode = None;
        self.request_main_paint();
    }

    /// Handle one key while copy-mode owns the keyboard.
    fn copy_mode_key(&mut self, physical: winit::keyboard::PhysicalKey, logical: &winit::keyboard::Key, ctrl: bool) {
        use crate::copymode::Motion;
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        // Ctrl combos: half-page scroll (keyed on physical position, robust vs
        // control-char logical keys).
        if ctrl {
            if let PhysicalKey::Code(code) = physical {
                match code {
                    KeyCode::KeyU => self.copy_mode_motion(Motion::HalfPageUp),
                    KeyCode::KeyD => self.copy_mode_motion(Motion::HalfPageDown),
                    _ => {}
                }
            }
            return; // swallow every other Ctrl chord
        }
        // Non-motion commands.
        match logical {
            Key::Named(NamedKey::Escape) => {
                self.active_tab_mut().terminal.selection_clear();
                self.exit_copy_mode();
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.copy_mode_yank();
                return;
            }
            Key::Character(s) if s.as_str() == "y" => {
                self.copy_mode_yank();
                return;
            }
            Key::Character(s) if s.as_str() == "v" || s.as_str() == "V" => {
                let line = s.as_str() == "V";
                // Content-pinned anchor: capture the BUFFER line under the cursor
                // NOW, so scrolling while selecting extends into scrollback rather
                // than sliding the whole selection with the viewport.
                let cur_row = self.copy_mode.map(|cm| cm.row);
                let anchor_line =
                    cur_row.map(|row| self.active_tab().terminal.viewport_line_to_buffer(row));
                let now_selecting = if let (Some(cm), Some(anchor_line)) =
                    (self.copy_mode.as_mut(), anchor_line)
                {
                    if cm.selecting && cm.line_mode == line {
                        cm.selecting = false;
                        false
                    } else {
                        cm.begin_select(line, anchor_line);
                        true
                    }
                } else {
                    false
                };
                if now_selecting {
                    self.copy_mode_refresh_selection();
                } else {
                    self.active_tab_mut().terminal.selection_clear();
                    self.request_main_paint();
                }
                return;
            }
            _ => {}
        }
        let motion = match logical {
            Key::Named(NamedKey::ArrowLeft) => Some(Motion::Left),
            Key::Named(NamedKey::ArrowRight) => Some(Motion::Right),
            Key::Named(NamedKey::ArrowUp) => Some(Motion::Up),
            Key::Named(NamedKey::ArrowDown) => Some(Motion::Down),
            Key::Character(s) if s.chars().count() == 1 => match s.chars().next().unwrap() {
                'h' => Some(Motion::Left),
                'l' => Some(Motion::Right),
                'k' => Some(Motion::Up),
                'j' => Some(Motion::Down),
                '0' => Some(Motion::LineStart),
                '$' => Some(Motion::LineEnd),
                'w' => Some(Motion::WordFwd),
                'b' => Some(Motion::WordBack),
                'e' => Some(Motion::WordEnd),
                'g' => Some(Motion::Top),
                'G' => Some(Motion::Bottom),
                _ => None,
            },
            _ => None,
        };
        if let Some(m) = motion {
            self.copy_mode_motion(m);
        }
        // else: swallow the key (copy-mode owns the keyboard).
    }

    /// Apply a copy-mode motion: move the cursor, honour the scroll request, and
    /// re-drive the selection from the (possibly scrolled) viewport coords.
    fn copy_mode_motion(&mut self, motion: crate::copymode::Motion) {
        use crate::copymode::ScrollReq;
        let Some(cm) = self.copy_mode else { return };
        let (rows, cols) = {
            let t = &self.active_tab().terminal;
            (t.rows(), t.cols())
        };
        let viewport = self.active_tab().terminal.viewport_rows_chars();
        let out = crate::copymode::apply_motion(&cm, motion, rows, cols, &viewport);
        match out.scroll {
            ScrollReq::None => {}
            ScrollReq::Lines(n) => self.active_tab_mut().terminal.scroll_lines(n),
            ScrollReq::Top => {
                let max = self.active_tab().terminal.scroll_max();
                self.active_tab_mut().terminal.scroll_to_offset(max);
            }
            ScrollReq::Bottom => self.active_tab_mut().terminal.scroll_to_bottom(),
        }
        if let Some(cm) = self.copy_mode.as_mut() {
            cm.row = out.row.min(rows.saturating_sub(1));
            cm.col = out.col.min(cols.saturating_sub(1));
        }
        self.copy_mode_refresh_selection();
        self.request_main_paint();
    }

    /// Rebuild the alacritty selection from the copy-mode anchor + cursor with
    /// the DERIVED sub-cell sides (BLOCKING 2) — reading-order start=Left,
    /// end=Right — so the highlight/yank is inclusive on both ends regardless of
    /// direction. No-op when not selecting (never clobbers a cleared selection).
    fn copy_mode_refresh_selection(&mut self) {
        let Some(cm) = self.copy_mode else { return };
        if !cm.selecting {
            return;
        }
        let anchor = (cm.anchor_line, cm.anchor_col);
        let term = &mut self.active_tab_mut().terminal;
        // The cursor's CURRENT absolute buffer line (viewport row → buffer at the
        // present scroll offset); the anchor is already absolute + fixed, so a
        // scroll extends the selection through scrollback instead of sliding it.
        let cursor_line = term.viewport_line_to_buffer(cm.row);
        let cursor = (cursor_line, cm.col);
        if cm.line_mode {
            let (sr, er) = if cursor.0 >= anchor.0 {
                (anchor.0, cursor.0)
            } else {
                (cursor.0, anchor.0)
            };
            term.selection_start_lines_abs(sr);
            term.selection_update_abs(er, cm.col, false);
        } else {
            let (start, end) = crate::copymode::selection_endpoints(anchor, cursor);
            term.selection_start_abs(start.0, start.1, start.2);
            term.selection_update_abs(end.0, end.1, end.2);
        }
    }

    /// Yank the current selection to the clipboard and exit copy-mode.
    fn copy_mode_yank(&mut self) {
        let text = self
            .active_tab()
            .terminal
            .selection_text()
            .filter(|t| !t.is_empty());
        if let Some(t) = text {
            crate::clipboard::set(&t);
        }
        self.active_tab_mut().terminal.selection_clear();
        self.exit_copy_mode();
    }

    // ── Command palette ──────────────────────────────────────────────────────

    /// (Re)build the palette registry FRESH and open the overlay. Building on
    /// open (~50 short entries) — not incrementally and NOT in `apply_theme`
    /// (which auto-repeats on opacity) — keeps the dynamic theme/tab/detach
    /// entries current at zero per-frame cost. Dismisses every peer overlay so
    /// exactly one overlay owns keys + draws on top.
    fn open_palette(&mut self) {
        self.dismiss_menus();
        self.help_open = false;
        self.welcome_open = false;
        let themes = jetty_core::theme_list();
        let tabs: Vec<String> = self.tabs.iter().map(|t| t.title.clone()).collect();
        let detached: Vec<String> = self.detached.iter().map(|d| d.tab.title.clone()).collect();
        self.palette_registry = crate::palette::build_registry(&themes, &tabs, &detached);
        self.palette_query.clear();
        self.palette_open = true;
        self.refilter_palette();
        self.request_main_paint();
    }

    /// Recompute the fuzzy hit list from the current query. Called ONLY on open +
    /// each keystroke — never per frame. Resets the selection/scroll to the top.
    fn refilter_palette(&mut self) {
        self.palette_filtered = crate::palette::filter(&self.palette_registry, &self.palette_query);
        self.palette_selected = 0;
        self.palette_scroll = 0;
    }

    /// Close the palette and free its transient state, so nothing is allocated
    /// while it is closed.
    fn close_palette(&mut self) {
        if !self.palette_open {
            return;
        }
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_filtered = Vec::new();
        self.palette_registry = Vec::new();
        self.request_main_paint();
    }

    /// Move the palette selection by `delta` rows (clamped), keeping it inside the
    /// `MAX_PALETTE_ROWS` scroll window.
    fn palette_move(&mut self, delta: isize) {
        let n = self.palette_filtered.len();
        if n == 0 {
            return;
        }
        let next = (self.palette_selected as isize + delta).clamp(0, n as isize - 1) as usize;
        self.palette_selected = next;
        let win = jetty_render::MAX_PALETTE_ROWS;
        if next < self.palette_scroll {
            self.palette_scroll = next;
        } else if next >= self.palette_scroll + win {
            self.palette_scroll = next + 1 - win;
        }
    }

    /// Toggle the perf HUD. Extracted so every caller shares the reflow: the HUD
    /// reserves grid rows via `status_h`, so a bare flag flip would leave the grid
    /// the wrong size (a bare `= !; persist; redraw` is a bug — see the config
    /// reload path, which reflows for the same reason).
    fn toggle_perf_hud(&mut self) {
        self.show_perf_hud = !self.show_perf_hud;
        self.reflow();
        self.persist();
        self.request_main_paint();
    }

    /// THE per-surface paint choke for the MAIN window (v0.23 central paint
    /// chokepoint). Every producer-category `request_redraw` for the main window
    /// (input, PTY output, resize, overlays/chrome, sync-flush, lifecycle) routes
    /// through here instead of a raw `self.window.request_redraw()`, so there is
    /// ONE auditable site and a CI grep can assert no raw producer calls leak back.
    ///
    /// NON-stateful by design (v0.23): winit already coalesces multiple
    /// `request_redraw` into a single `RedrawRequested`, so this is a thin, direct
    /// forward — NO `Cell` flag, NO deferred flush (a stateful flag would risk a
    /// dropped frame across the macOS `Wait`/`Poll` seam). The deliverable is
    /// auditability, not fewer syscalls.
    ///
    /// This does NOT gate on `self.visible`/`self.main_occluded`. The LOAD-BEARING
    /// visibility/occlusion gates live at the PRODUCER call sites (the Wake-drain
    /// `self.visible && !self.main_occluded`, sync-flush `main_visible`, etc.) and
    /// at the `RedrawRequested` `!self.visible` early-out — they MUST stay there
    /// verbatim. Category-D animation continuation is driven by RAW `request_redraw`
    /// in `about_to_wait` / the render tail and deliberately does NOT route here.
    fn request_main_paint(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// The per-surface paint choke for the SETTINGS window. Same non-stateful,
    /// non-gating contract as `request_main_paint`. No-op when Settings is closed
    /// (mirrors the previous `if let Some(w) = &self.settings_window` guard).
    fn request_settings_paint(&self) {
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
    }

    /// Fan-out choke: paint the main window, every detached window, and the
    /// settings window. Used by actions that change a shared visual
    /// (theme/opacity/effects). `&self`, non-stateful — pure fan-out over the
    /// per-surface chokes above.
    fn mark_dirty_all(&self) {
        self.request_main_paint();
        for dw in &self.detached {
            dw.request_paint();
        }
        self.request_settings_paint();
    }

    /// Redraw the main window plus every detached and the settings window — used
    /// by palette actions that change a shared visual (theme/opacity/effects).
    /// Thin alias over [`Self::mark_dirty_all`] (kept for its many call sites).
    fn redraw_main_and_detached(&self) {
        self.mark_dirty_all();
    }

    /// Run a resolved palette command by invoking the EXISTING app action for it.
    /// The palette is already closed by the caller. Index-bearing variants are
    /// `.get()`-guarded (bounds-checked) so a tab/theme that vanished between open
    /// and Enter is a clean no-op — belt-and-suspenders on top of build-on-open.
    fn run_palette_cmd(&mut self, cmd: crate::palette::PaletteCmd, event_loop: &ActiveEventLoop) {
        use crate::palette::PaletteCmd as C;
        match cmd {
            C::NewTab => self.new_tab(),
            C::CloseTab => {
                self.confirm_close = Some(self.active);
                self.request_main_paint();
            }
            C::NextTab => self.switch_tab(true),
            C::PrevTab => self.switch_tab(false),
            C::DetachTab => self.detach_tab(self.active, event_loop, None),
            C::OpenSettings => {
                // Open (never toggle-closed): don't dismiss an already-open panel.
                if self.settings_window.is_none() {
                    self.toggle_settings_window(event_loop);
                }
            }
            C::FontUp => self.set_font_size(self.font_logical + 1.0),
            C::FontDown => self.set_font_size(self.font_logical - 1.0),
            C::FontReset => self.set_font_size(FONT_LOGICAL_DEFAULT),
            C::OpacityUp => {
                self.opacity = (self.opacity + 0.05).min(1.0);
                self.apply_theme();
                self.persist();
                self.redraw_main_and_detached();
            }
            C::OpacityDown => {
                self.opacity = (self.opacity - 0.05).max(0.1);
                self.apply_theme();
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCrt => {
                self.fx.crt_enabled = !self.fx.crt_enabled;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCrtRoll => {
                self.fx.crt_animate_roll = !self.fx.crt_animate_roll;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCrtFlicker => {
                self.fx.crt_flicker = !self.fx.crt_flicker;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCrtJitter => {
                self.fx.crt_jitter = !self.fx.crt_jitter;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCaretFlash => {
                self.fx.caret_flash_enabled = !self.fx.caret_flash_enabled;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::ToggleCaretGlow => {
                self.fx.caret_glow_enabled = !self.fx.caret_glow_enabled;
                self.persist();
                self.redraw_main_and_detached();
            }
            C::TogglePerfHud => self.toggle_perf_hud(),
            C::ShowWelcome => {
                self.welcome_open = true;
                self.request_main_paint();
            }
            C::Search => {
                if !self.search_open {
                    self.search_open = true;
                    self.active_tab_mut().terminal.search_refresh();
                }
                self.request_main_paint();
            }
            // The palette has already closed (run_palette_cmd runs after
            // close_palette), so overlay_owns_keys() is false and the mode enters.
            C::HintMode => self.enter_hint_mode(),
            C::CopyMode => self.enter_copy_mode(),
            C::PrevPrompt => {
                if self.active_tab_mut().terminal.jump_prompt(false) {
                    self.request_main_paint();
                    self.update_link_hover(true);
                }
            }
            C::NextPrompt => {
                if self.active_tab_mut().terminal.jump_prompt(true) {
                    self.request_main_paint();
                    self.update_link_hover(true);
                }
            }
            C::Copy => {
                let copied = self
                    .active_tab()
                    .terminal
                    .selection_text()
                    .filter(|t| !t.is_empty());
                if let Some(text) = copied {
                    clipboard::set(&text);
                    self.active_tab_mut().terminal.selection_clear();
                    self.request_main_paint();
                }
            }
            C::Paste => {
                if let Some(text) = clipboard::get() {
                    self.paste_text(&text);
                }
            }
            C::ToggleLaunchAtLogin => {
                self.launch_at_login = !self.launch_at_login;
                set_launch_at_login(self.launch_at_login);
                self.persist();
            }
            C::ResetKeybindings => {
                // Clear every user `[keys]` override → back to the built-in defaults.
                self.keys = crate::config::KeyBindings::default();
                self.keymap = crate::keymap::KeyMap::compile(&self.keys);
                self.help_rows = App::compute_help_rows(&self.keymap, &self.summon_hotkey);
                self.persist();
                self.request_main_paint();
            }
            C::Hide => self.set_visibility(false, event_loop),
            C::Quit => {
                self.confirm_quit = true;
                self.request_main_paint();
            }
            // Index-bearing dynamic actions: `.get()`-guard against a stale index.
            C::SetTheme(i) => {
                if i < jetty_core::theme_count() {
                    self.theme_idx = i;
                    self.apply_theme();
                    self.persist();
                    self.redraw_main_and_detached();
                }
            }
            C::SelectTab(i) => {
                if i < self.tabs.len() {
                    self.select_tab(i);
                }
            }
            C::Reattach(i) => {
                if i < self.detached.len() {
                    self.reattach_tab(i, event_loop);
                }
            }
        }
    }

    /// Switch to the next (`+1`) or previous (`-1`) tab, wrapping around.
    fn switch_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n <= 1 {
            return;
        }
        // The search bar targets the ACTIVE tab: close it (clearing the
        // outgoing tab's regex/matches) before the index moves (F2/F7/F15).
        self.search_close();
        self.active = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        // A pending text selection belongs to the previous tab's grid; reset it.
        self.selecting = false;
        // Same for any fractional wheel remainder (it was that tab's scroll).
        self.scroll_accum.reset();
        // And the cached link hover — Ctrl+Tab keeps Ctrl held (no
        // ModifiersChanged) and the hovered CELL is unchanged, so without the
        // forced recompute tab 1's underline ghosts over tab 2's text (F12).
        self.update_link_hover(true);
        self.request_main_paint();
    }

    /// Return the cached tab-bar metadata, rebuilding it only when the titles
    /// or the active index change (compared via a cheap signature hash). Avoids
    /// cloning every tab title on every frame, including animation frames.
    fn tabs_meta(&mut self) -> &[(String, bool)] {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.active.hash(&mut hasher);
        self.tabs.len().hash(&mut hasher);
        for t in &self.tabs {
            t.title.hash(&mut hasher);
        }
        let sig = hasher.finish();
        if sig != self.cached_tabs_sig {
            self.cached_tabs_meta = self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| (t.title.clone(), i == self.active))
                .collect();
            self.cached_tabs_sig = sig;
            // Sync the main window's OS title to the active tab (the window is
            // undecorated, so this shows in the taskbar/alt-tab only). Runs
            // ONLY inside this sig-changed branch — never per-frame — and the
            // hash covers every title mutation path (OSC, rename, tab switch,
            // close, reattach) for free.
            let active_title =
                self.tabs.get(self.active).map(|t| t.title.as_str()).unwrap_or("JeTTY");
            let desired = format!("{active_title} — JeTTY");
            if desired != self.applied_main_os_title {
                if let Some(w) = &self.window {
                    w.set_title(&desired);
                    self.applied_main_os_title = desired;
                }
            }
        }
        &self.cached_tabs_meta
    }

    /// Jump to tab `n` (0-based), clamped to the valid range.
    fn select_tab(&mut self, n: usize) {
        if self.tabs.is_empty() {
            return;
        }
        let target = n.min(self.tabs.len() - 1);
        if target != self.active {
            // Active tab changes: close the search bar and clear the OUTGOING
            // tab's search state before the index moves (F2/F7/F15).
            self.search_close();
        }
        self.active = target;
        // A pending text selection belongs to the previous tab's grid; reset it.
        self.selecting = false;
        // Same for any fractional wheel remainder (it was that tab's scroll).
        self.scroll_accum.reset();
        // And the cached link hover — recompute against the NEW tab's grid.
        self.update_link_hover(true);
        self.request_main_paint();
    }

    /// Commit an in-progress tab rename: write `rename_buf` back to the tab's
    /// title and clear the rename state. No-op when not renaming. An empty buffer
    /// is ignored (keep the previous title) so a tab never ends up nameless.
    fn commit_rename(&mut self) {
        if let Some(i) = self.renaming.take() {
            let trimmed = self.rename_buf.trim();
            if i < self.tabs.len() && !trimmed.is_empty() {
                self.tabs[i].title = trimmed.to_string();
                // Manual rename permanently wins over shell OSC 0/2 titles for
                // this tab. An empty rename (no-op above) deliberately does NOT
                // set the flag, so auto-titles stay live.
                self.tabs[i].manually_renamed = true;
            }
            self.rename_buf.clear();
            self.request_main_paint();
        }
    }

    /// Compute the scroll offset from the current cursor position during a drag.
    /// `w` and `h` are the current surface dimensions in physical pixels.
    fn apply_scroll_from_cursor(&mut self, w: u32, h: u32) {
        let rows = self.active_tab().terminal.rows();
        let max = self.active_tab().terminal.scroll_max();
        if let Some(offset) = jetty_render::scrollbar_offset_from_cursor(
            self.cursor.1 as f32,
            self.drag_grab_dy,
            rows,
            max,
            h,
            self.grid_top_offset(),
            self.status_h(),
        ) {
            self.active_tab_mut().terminal.scroll_to_offset(offset);
        }
        // Scrollbar interaction moved the viewport: refresh (in practice,
        // clear — the drag gate) the link hover so no stale underline rides it.
        self.update_link_hover(true);
        // suppress unused warning on w
        let _ = w;
    }

    /// Compute opacity from a cursor x relative to a slider track rect.
    fn opacity_from_cursor(&self, cx: f32, track: &jetty_render::Rect) -> f32 {
        let frac = ((cx - track.x) / track.w).clamp(0.0, 1.0);
        (0.1 + frac * 0.9).clamp(0.1, 1.0)
    }

    /// Compute corner radius (px, [0, 24]) from a cursor x relative to the radius
    /// slider track rect.
    fn radius_from_cursor(&self, cx: f32, track: &jetty_render::Rect) -> f32 {
        let frac = ((cx - track.x) / track.w).clamp(0.0, 1.0);
        (frac * 24.0).clamp(0.0, 24.0)
    }

    /// Compute the dropdown-height fraction ([0.25, 1.0]) from a cursor x relative
    /// to the dropdown-height slider track rect.
    fn dropdown_pct_from_cursor(&self, cx: f32, track: &jetty_render::Rect) -> f32 {
        let frac = ((cx - track.x) / track.w).clamp(0.0, 1.0);
        (0.25 + frac * 0.75).clamp(0.25, 1.0)
    }

    /// Compute the dropdown-width fraction ([0.2, 1.0]) from a cursor x relative
    /// to the dropdown-width slider track rect.
    fn dropdown_width_pct_from_cursor(&self, cx: f32, track: &jetty_render::Rect) -> f32 {
        let frac = ((cx - track.x) / track.w).clamp(0.0, 1.0);
        (0.2 + frac * 0.8).clamp(0.2, 1.0)
    }

    /// Compute a [0, 1] fraction from cursor x relative to a slider track rect.
    /// Used for all Effects-tab sliders whose value range maps linearly to 0..1.
    fn fx_frac_from_cursor(&self, cx: f32, track: &jetty_render::Rect) -> f32 {
        ((cx - track.x) / track.w).clamp(0.0, 1.0)
    }

    /// Select a new window mode: persist it, and apply it live. Switching to
    /// Center clears any in-progress slide; switching to Dropdown clears last_pos
    /// so the next summon re-docks from a clean top-flush geometry.
    fn set_window_mode(&mut self, mode: WindowMode) {
        if self.window_mode == mode {
            return;
        }
        self.window_mode = mode;
        match mode {
            WindowMode::Center => {
                self.slide_anim = None;
                // Stop any in-flight dropdown dock re-assertion so it can't snap a
                // just-switched Center window back to the top strip.
                self.pending_dock_frames = 0;
            }
            WindowMode::Dropdown => {
                // Recompute dock geometry (ignore stale pos). If the window is
                // already visible, dock it LIVE so switching mode in settings
                // immediately drops it to the top strip (re-asserted post-map via
                // pending_dock_frames) instead of waiting for the next F9.
                self.last_pos = None;
                if self.visible {
                    if let Some(w) = &self.window {
                        dock_window_top(w, self.dropdown_width_pct, self.dropdown_height_pct);
                    }
                    self.pending_dock_frames = 5;
                    self.slide_anim = Some(std::time::Instant::now());
                }
            }
        }
        self.persist();
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// Flip the tab-bar position (top ↔ bottom): persist it and apply live. The
    /// grid dimensions are unchanged (the bar always costs TABBAR_H of grid
    /// height), so no reflow is needed — only a redraw of both windows.
    fn set_tab_bar_bottom(&mut self, bottom: bool) {
        if self.tab_bar_bottom == bottom {
            return;
        }
        self.tab_bar_bottom = bottom;
        self.persist();
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// Set the scrollback history limit: persist it and live-apply to EVERY
    /// open tab — main window and detached windows alike (the whole-codebase
    /// sweep; a detached tab carries its Terminal, so it must not be skipped).
    /// The main window is redrawn too: a shrink can move/shrink the scrollbar.
    fn set_scrollback_lines(&mut self, lines: usize) {
        if self.scrollback_lines == lines {
            return;
        }
        self.scrollback_lines = lines;
        self.persist();
        for tab in &mut self.tabs {
            tab.terminal.set_scrollback_lines(lines);
        }
        for dw in &mut self.detached {
            dw.tab.terminal.set_scrollback_lines(lines);
            dw.request_paint();
        }
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// Returns the measured physical-pixel advance of one chrome-font character
    /// from the fixed-size chrome `TextLayer`. Falls back to `9.6` when the chrome
    /// layer has not yet been initialised (i.e. before the first GPU frame).
    ///
    /// This is the scale-aware value that must be threaded into every chrome overlay
    /// builder (`build_tab_bar_ex`, `build_panel`, `build_help_overlay`, etc.) so
    /// that right-alignment and width reservations are correct on HiDPI displays.
    #[inline]
    fn chrome_char_w(&self) -> f32 {
        self.chrome_text.as_ref().map(|ct| ct.cell_size().0).unwrap_or(9.6)
    }

    /// Measured advance of the SETTINGS panel's text layer (the CAPPED UI size),
    /// used by `build_panel` so the panel's right-aligned values and family-row
    /// truncation match the layer that actually draws those labels. Falls back to
    /// the main chrome advance, then 9.6, before the settings layer exists.
    #[inline]
    fn settings_char_w(&self) -> f32 {
        self.settings_text
            .as_ref()
            .map(|st| st.cell_size().0)
            .unwrap_or_else(|| self.chrome_char_w())
    }

    /// Convert the current cursor pixel position into 1-based terminal cell
    /// coordinates `(col, row)` using the renderer's cell size, CLAMPED to the
    /// active grid (`1..=cols`, `1..=rows`) — a click in the scrollbar gutter
    /// or the status strip must never put out-of-range coordinates into a
    /// mouse report (xterm clamps to the grid edge; apps hit-testing panes get
    /// confused otherwise). Returns `None` when the renderer (and thus cell
    /// metrics) is not yet available or no tab exists.
    fn cursor_cell(&self) -> Option<(usize, usize)> {
        self.cell_at_pixel(self.cursor.0, self.cursor.1)
    }

    /// Like [`cursor_cell`] but for an arbitrary pixel position (1-based, clamped
    /// to the grid). Used to detect cross-cell pointer motion for mouse motion
    /// reports (F5).
    fn cell_at_pixel(&self, px: f64, py: f64) -> Option<(usize, usize)> {
        if self.tabs.is_empty() {
            return None;
        }
        let (cell_w, cell_h) = self.text.as_ref()?.cell_size();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        // Subtract the grid's pixel origin before dividing (0 when the bar is at
        // the bottom, TABBAR_H when at the top).
        let y = py as f32 - self.grid_top_offset();
        Some(input::cell_at_clamped(
            px as f32,
            y,
            cell_w,
            cell_h,
            self.active_tab().terminal.cols(),
            self.active_tab().terminal.rows(),
        ))
    }

    /// Convert the current cursor pixel position into 0-based viewport cell
    /// coordinates `(line, col)` clamped to the terminal grid, plus whether the
    /// pointer is in the LEFT half of its cell. Returns `None` when the renderer
    /// is not yet available.
    ///
    /// Selection start/update derive the cell `Side` from `left_half` (F4):
    /// hardcoding Left-at-press / Right-at-update dropped the endpoint cells on a
    /// reverse (right-to-left / bottom-to-top) drag.
    fn cursor_cell_0_side(&self) -> Option<(usize, usize, bool)> {
        let (cell_w, cell_h) = self.text.as_ref()?.cell_size();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let y = (self.cursor.1 as f32 - self.grid_top_offset()).max(0.0);
        Some(input::cell_at_0_side(
            self.cursor.0 as f32,
            y,
            cell_w,
            cell_h,
            self.active_tab().terminal.cols(),
            self.active_tab().terminal.rows(),
        ))
    }

    /// The cursor icon the main window should show for `zone`: the link
    /// pointer wins over the default arrow, resize arrows win over both.
    fn desired_cursor(&self, zone: ResizeZone) -> winit::window::CursorIcon {
        if zone == ResizeZone::None && self.link_hover.is_some() {
            winit::window::CursorIcon::Pointer
        } else {
            zone.cursor_icon()
        }
    }

    /// Recompute (or clear) the main window's Ctrl+hover link state. Fully
    /// event-driven: zero work unless the link modifier is held, and the
    /// terminal is only scanned when the hovered CELL changed (`force` skips
    /// that cache — used when the grid/viewport moved under a still pointer).
    fn update_link_hover(&mut self, force: bool) {
        // Same modal predicate as the resize-cursor block in CursorMoved.
        let modal_open = self.confirm_quit
            || self.confirm_close.is_some()
            || self.help_open
            || self.context_menu.is_some()
            || self.tab_menu.is_some();
        let gated = link_modifier_held(&self.modifiers)
            && !self.tabs.is_empty()
            && !self.selecting
            && !self.dragging_scrollbar
            && self.tab_drag.is_none()
            && !modal_open;
        // Cursor must be over the grid band (same bounds as the Middle-click
        // paste arm): below the top chrome, above the bottom strips.
        let in_grid = gated
            && self
                .gpu
                .as_ref()
                .map(|g| {
                    let h = g.config.height as f32;
                    let cy = self.cursor.1 as f32;
                    let grid_bottom = if self.tab_bar_bottom {
                        self.tabbar_y(h)
                    } else {
                        h - self.status_h()
                    };
                    cy >= self.grid_top_offset() && cy < grid_bottom
                })
                .unwrap_or(false);
        if !in_grid {
            self.link_hover_cell = None;
            if self.link_hover.take().is_some() {
                if let Some(win) = &self.window {
                    win.set_cursor(self.desired_cursor(self.resize_cursor));
                    self.request_main_paint();
                }
            }
            return;
        }
        let Some((line, col, _)) = self.cursor_cell_0_side() else {
            return;
        };
        if !force && self.link_hover_cell == Some((line, col)) {
            return;
        }
        let was_some = self.link_hover.is_some();
        self.link_hover = self.active_tab().terminal.link_at(line, col);
        self.link_hover_cell = Some((line, col));
        if let Some(win) = &self.window {
            if was_some != self.link_hover.is_some() {
                win.set_cursor(self.desired_cursor(self.resize_cursor));
            }
            // Redraw whenever the underline could have (dis)appeared or moved.
            if was_some || self.link_hover.is_some() {
                self.request_main_paint();
            }
        }
    }

    /// Clear the Ctrl+hover link state on the main window AND every detached
    /// window. `ModifiersChanged` is delivered per-focused-window only, so a
    /// modifier release must sweep all windows or an unfocused one keeps a
    /// stale underline.
    fn clear_all_link_hovers(&mut self) {
        self.link_hover_cell = None;
        if self.link_hover.take().is_some() {
            if let Some(win) = &self.window {
                win.set_cursor(self.resize_cursor.cursor_icon());
                self.request_main_paint();
            }
        }
        for dw in &mut self.detached {
            dw.link_hover_cell = None;
            if dw.link_hover.take().is_some() {
                dw.window.set_cursor(dw.resize_zone.cursor_icon());
                dw.request_paint();
            }
        }
    }

    /// Recompute (or clear) the Ctrl+hover link state of detached window
    /// `pos` — the detached mirror of [`App::update_link_hover`], using that
    /// window's own cursor/geometry (grid origin `TABBAR_H`, its own modal =
    /// the context menu).
    fn update_detached_link_hover(&mut self, pos: usize, force: bool) {
        let held = link_modifier_held(&self.modifiers);
        let status_h = self.status_h();
        let Some(dw) = self.detached.get_mut(pos) else { return };
        let (cw, ch) = dw.text.cell_size();
        let cy = dw.cursor.1 as f32;
        let h = dw.gpu.config.height as f32;
        let gated = held
            && !dw.selecting
            && !dw.dragging_scrollbar
            && dw.bar_drag.is_none()
            && dw.menu_open.is_none()
            && cw > 0.0
            && ch > 0.0
            && cy >= TABBAR_H
            && cy < h - status_h;
        if !gated {
            dw.link_hover_cell = None;
            if dw.link_hover.take().is_some() {
                dw.window.set_cursor(dw.resize_zone.cursor_icon());
                dw.request_paint();
            }
            return;
        }
        let gy = (cy - TABBAR_H).max(0.0);
        let (line, col, _) = input::cell_at_0_side(
            dw.cursor.0 as f32,
            gy,
            cw,
            ch,
            dw.tab.terminal.cols(),
            dw.tab.terminal.rows(),
        );
        if !force && dw.link_hover_cell == Some((line, col)) {
            return;
        }
        let was_some = dw.link_hover.is_some();
        dw.link_hover = dw.tab.terminal.link_at(line, col);
        dw.link_hover_cell = Some((line, col));
        if was_some != dw.link_hover.is_some() {
            dw.window.set_cursor(
                if dw.link_hover.is_some() && dw.resize_zone == ResizeZone::None {
                    winit::window::CursorIcon::Pointer
                } else {
                    dw.resize_zone.cursor_icon()
                },
            );
        }
        if was_some || dw.link_hover.is_some() {
            dw.request_paint();
        }
    }

    /// Open `url` with the platform opener (`open` on macOS, `xdg-open`
    /// elsewhere — OS-level cfg only, never DE-specific), spawned fully
    /// detached with all three stdio fds null. Restricted to the
    /// http/https/file allowlist; a missing opener degrades to an stderr line.
    fn open_url(url: &str) {
        if !url_scheme_allowed(url) {
            eprintln!("jetty: refusing to open URL with disallowed scheme: {url}");
            return;
        }
        #[cfg(target_os = "macos")]
        let cmd = "open";
        #[cfg(not(target_os = "macos"))]
        let cmd = "xdg-open";
        match std::process::Command::new(cmd)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            // Reap the short-lived child off-thread so it never zombies.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => eprintln!("jetty: failed to spawn {cmd} for URL: {e}"),
        }
    }

    /// Paste `text` to the ACTIVE tab's PTY, wrapping in bracketed-paste
    /// sequences if the running application has enabled `\e[?2004h`.
    fn paste_text(&mut self, text: &str) {
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active;
        Self::paste_to_tab(&mut self.tabs[active], text);
    }

    /// Paste `text` into `tab`'s PTY (bracketed when the app enabled it).
    /// Shared by the main window's paste paths and the detached windows'
    /// context-menu / Ctrl+Shift+V paste, so all windows paste identically.
    fn paste_to_tab(tab: &mut Tab, text: &str) {
        if text.is_empty() {
            return;
        }
        let bracketed = tab.terminal.bracketed_paste();
        let w = &mut tab.writer;
        if bracketed {
            // Strip any embedded end-paste marker (ESC[201~) from the payload so
            // pasted content can never terminate the bracketed-paste guard early
            // and inject the remainder as typed commands (the classic paste
            // injection; xterm/iTerm2/alacritty all do this).
            let clean = Self::strip_paste_end(text.as_bytes());
            let _ = w.write_all(b"\x1b[200~");
            let _ = w.write_all(&clean);
            let _ = w.write_all(b"\x1b[201~");
        } else {
            let _ = w.write_all(text.as_bytes());
        }
        let _ = w.flush();
    }

    /// Remove every embedded bracketed-paste END marker (`ESC[201~`) from
    /// `bytes`. Borrows unchanged when the marker is absent (the common case),
    /// so a normal paste pays no allocation. Checks the OUTPUT tail after each
    /// byte so a marker cannot re-form across a removed one (e.g. the crafted
    /// `ESC[2` + `ESC[201~` + `01~`).
    fn strip_paste_end(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
        const END: &[u8] = b"\x1b[201~";
        if bytes.len() < END.len() || !bytes.windows(END.len()).any(|w| w == END) {
            return std::borrow::Cow::Borrowed(bytes);
        }
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        for &b in bytes {
            out.push(b);
            if out.ends_with(END) {
                out.truncate(out.len() - END.len());
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// Encode a mouse event and write it to the PTY. Used only when the running
    /// application has enabled mouse reporting (`mouse_mode()`). The wire format
    /// matches what the app requested: SGR (1006) encoding when `mouse_sgr()` is
    /// true (`\e[?1006h`), otherwise the legacy X10 encoding.
    fn send_mouse_report(&mut self, event: input::MouseEvent) {
        let Some((col, row)) = self.cursor_cell() else { return };
        if self.tabs.is_empty() {
            return;
        }
        let sgr = self.active_tab().terminal.mouse_sgr();
        let bytes = input::encode_mouse(event, col, row, sgr);
        let w = &mut self.tabs[self.active].writer;
        let _ = w.write_all(&bytes);
        let _ = w.flush();
    }

    /// Drain pending PTY output into the terminal and flush any query replies.
    ///
    /// Returns `true` if any bytes were consumed (PTY data or reply writes),
    /// so the caller can skip `request_redraw()` when nothing changed — making
    /// the 100ms heartbeat essentially free when the terminal is idle.
    /// Drain pending PTY output for EVERY tab into its terminal and flush each
    /// tab's query replies back to its own PTY. Background tabs must keep draining
    /// so their shells never block on a full pipe.
    ///
    /// Returns `(active_had_data, chrome_changed, exited)` where
    /// `active_had_data` is true if the ACTIVE tab consumed bytes (so the caller
    /// redraws), `chrome_changed` is true if the tab bar needs a repaint — an
    /// INACTIVE tab's activity indicator transitioned, or ANY tab's title was
    /// changed by an OSC 0/2 (a background tab whose indicator is already lit
    /// yields no activity transition, yet its new title must still reach the
    /// tab bar / OS title — F1/F14) — and `exited` is the list of tab indices
    /// whose child exited this tick (caller closes them after, to avoid
    /// mutating `tabs` while iterating).
    fn drain_pty(&mut self) -> (bool, bool, Vec<usize>) {
        let mut active_had_data = false;
        let mut chrome_changed = false;
        let mut exited: Vec<usize> = Vec::new();
        // Perf-HUD VT throughput: count bytes read this drain into a local
        // (avoids a self borrow inside the &mut self.tabs loop), folded into the
        // running total after the loop. Cheap; the rate is derived over ~1s
        // windows in the render path.
        let mut vt_read: u64 = 0;
        // App-initiated reflow just SIGWINCHed every background shell; their
        // prompt repaints are about to arrive and are NOT "unseen output" —
        // suppress the None→Output upgrade for the grace window (F3). One
        // cheap comparison on the already-non-idle drain path; Bell is real
        // user-relevant signal and stays through.
        let suppress_output = self
            .reflow_resized_at
            .is_some_and(|t| t.elapsed() < REFLOW_ACTIVITY_GRACE);
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let (had, title_changed) = Self::drain_one_tab(tab, &mut vt_read);
            chrome_changed |= title_changed;
            // Consume the bell flag for EVERY tab (active included) so it never
            // goes stale; only INACTIVE tabs surface it as an indicator. Bell is
            // sticky (never downgraded by later output); Output only lights a
            // clean tab. Rides the existing event-driven drain — zero idle work.
            let rang = tab.terminal.take_bell();
            if i != self.active {
                let new = next_activity(tab.activity, had, rang, suppress_output);
                if new != tab.activity {
                    tab.activity = new;
                    chrome_changed = true;
                }
            }
            if i == self.active && had {
                active_had_data = true;
            }
            if tab.terminal.child_exited() || tab.pty.child_exited() {
                exited.push(i);
            }
        }
        self.vt_bytes += vt_read;
        (active_had_data, chrome_changed, exited)
    }

    /// Drain one tab's PTY output into its terminal, and flush any query
    /// replies (DSR/DA, etc.) the terminal produced back to the PTY. Returns
    /// `(had, title_changed)`: whether the tab fed any bytes or sent any
    /// reply (i.e. "had data"), and whether an OSC 0/2 changed the tab title.
    /// The title is reported SEPARATELY because folding it into `had` only
    /// guaranteed a redraw for the ACTIVE tab — an inactive tab whose
    /// activity dot was already lit produced no transition, so its new title
    /// never repainted the tab bar or the OS/taskbar title (F1/F14).
    /// `vt_read` accumulates bytes read, for the perf-HUD VT throughput
    /// counter; callers that don't track that (e.g. detached windows) pass a
    /// throwaway local.
    ///
    /// Shared by `drain_pty` (per `self.tabs` entry) and the `AppEvent::Wake`
    /// handler's detached-window loop, so both paths drain identically.
    fn drain_one_tab(tab: &mut Tab, vt_read: &mut u64) -> (bool, bool) {
        let mut had = false;
        // Feed at most PTY_DRAIN_BUDGET bytes this pass so a flood can't starve
        // the event loop (see the const's doc). Any remaining chunks are drained
        // by the Wakes the reader already queued for them.
        let mut fed = 0usize;
        while fed < PTY_DRAIN_BUDGET {
            match tab.pty.output().try_recv() {
                Ok(chunk) => {
                    *vt_read += chunk.len() as u64;
                    fed += chunk.len();
                    tab.terminal.feed(&chunk);
                    had = true;
                }
                Err(_) => break,
            }
        }
        // Flush any query replies (DSR/DA, etc.) this tab produced back to its
        // own PTY so the shell's startup probes succeed.
        let replies = tab.terminal.drain_pty_writes();
        if !replies.is_empty() {
            let _ = tab.writer.write_all(&replies);
            let _ = tab.writer.flush();
            had = true;
        }
        // Apply any pending shell-set title (OSC 0/2). Event-driven: rides this
        // drain pass only, zero idle cost (a lock-free flag check when clean).
        // Reported as its own flag so every call site can force the tab-bar /
        // OS-title repaint even when the OSC arrived with no grid change and
        // no activity-indicator transition (F1/F14).
        let mut title_changed = false;
        if let Some(update) = tab.terminal.take_title_update() {
            if let Some(new_title) =
                resolve_title(update, tab.manually_renamed, &tab.default_title)
            {
                if new_title != tab.title {
                    tab.title = new_title;
                    title_changed = true;
                }
            }
        }
        // OSC 52 COPY: a remote/tmux/nvim asked to set the system clipboard. Ride
        // this same drain pass (main + detached both drain here) — lock-free flag
        // check when clean, so zero idle cost. `crate::clipboard::set` is a free fn
        // (no self borrow), so this is conflict-free inside `drain_one_tab`.
        if let Some(text) = tab.terminal.take_clipboard_store() {
            crate::clipboard::set(&text);
        }
        // OSC 52 PASTE (load): a program asked to READ the clipboard. Only ever
        // present when the user enabled `osc52_allow_paste` (else alacritty denies it
        // and no request reaches us). Read the clipboard, CAP the reply length, format
        // via alacritty's supplied formatter, and write it back to the PTY.
        if let Some(fmt) = tab.terminal.take_clipboard_load() {
            if let Some(mut text) = crate::clipboard::get() {
                if text.len() > jetty_core::OSC52_MAX_BYTES {
                    text.truncate(floor_char_boundary(&text, jetty_core::OSC52_MAX_BYTES));
                }
                let reply = fmt(&text);
                let _ = tab.writer.write_all(reply.as_bytes());
                let _ = tab.writer.flush();
                had = true;
            }
        }
        (had, title_changed)
    }

    /// Poll every tab (main + detached) for OSC 133 command completions surfaced
    /// by the just-finished drain, and fire notifications for the ones that pass
    /// the gate. Called after BOTH drain sites — the `Wake` handler (incl. its
    /// detached-drain loop) AND `RedrawRequested` — so a completion in a hidden
    /// window still pings (amendments §3). Index-based iteration avoids an
    /// `iter_mut` vs `&self` borrow conflict. `take_completions()` is drained
    /// unconditionally (even when disabled) so nothing accumulates; it early-outs
    /// to an empty `Vec` in the common (no-completion) case, so this is ~free on
    /// the idle/no-shell-integration path.
    fn dispatch_completions(&mut self, event_loop: &ActiveEventLoop) {
        let enabled = self.notify_on_finish;
        // Snapshot "is the user watching the main window" ONCE for this batch. An
        // auto-summon triggered by an earlier tab flips self.visible mid-loop; without
        // the snapshot every LATER completion in the same ~100ms drain would see
        // watching==true and be gated out, and the user would land on the wrong tab.
        let main_watching = self.main_user_watching();
        // First failure wins the summon; else the last firing tab.
        let mut summon_target: Option<usize> = None;
        let mut summon_is_failure = false;
        for i in 0..self.tabs.len() {
            let completions = self.tabs[i].terminal.take_completions();
            if enabled {
                for c in completions {
                    if let Some(failed) = self.maybe_notify_main(i, c, main_watching) {
                        if self.auto_summon_on_finish && !self.visible {
                            if failed && !summon_is_failure {
                                summon_target = Some(i);
                                summon_is_failure = true;
                            } else if !summon_is_failure {
                                summon_target = Some(i);
                            }
                        }
                    }
                }
            }
        }
        // At most ONE auto-summon for the whole batch, AFTER gating every tab against
        // the pre-loop snapshot (so no sibling completion is suppressed).
        if let Some(tab) = summon_target {
            self.select_tab(tab);
            self.set_visibility(true, event_loop);
        }
        for i in 0..self.detached.len() {
            let completions = self.detached[i].tab.terminal.take_completions();
            if enabled {
                for c in completions {
                    self.maybe_notify_detached(i, c);
                }
            }
        }
    }

    /// Whether the user is actively looking at the MAIN window right now — never
    /// ping then. Handles the post-summon focus lag: `set_visibility(true)` flips
    /// `self.visible` immediately but `self.main_focused` only on the later WM
    /// `Focused(true)`, so a just-summoned window still inside its settle window
    /// counts as "watching" (a completion in that gap must not ping — amendments
    /// adopted §1).
    fn main_user_watching(&self) -> bool {
        if !self.visible || self.main_occluded {
            return false;
        }
        let settling = self
            .summon_settle_until
            .is_some_and(|t| std::time::Instant::now() < t);
        self.main_focused || settling
    }

    /// A label that NAMES a main tab (amendments §1): its displayed title, prefixed
    /// with "Tab N · " only when the shell/user gave it a non-default title (so a
    /// bare default "Tab 3" isn't doubled).
    fn main_tab_label(&self, i: usize) -> String {
        let tab = &self.tabs[i];
        if tab.title == tab.default_title {
            tab.title.clone()
        } else {
            format!("Tab {} · {}", i + 1, tab.title)
        }
    }

    /// Gate + fire a notification for a MAIN-window tab's completion. `watching` is
    /// the batch-start snapshot of `main_user_watching()` (so an auto-summon earlier
    /// in the same drain can't suppress this tab). Returns `Some(failed)` when a
    /// notification fired (the caller decides the single batch auto-summon), or
    /// `None` when gated out.
    fn maybe_notify_main(
        &mut self,
        tab: usize,
        c: jetty_core::CommandCompletion,
        watching: bool,
    ) -> Option<bool> {
        let key = NotifyKey::MainTab(tab);
        let since_last = self.notify_last_at.get(&key).map(|t| t.elapsed());
        if !crate::notify::should_notify(
            watching,
            c.duration,
            c.exit_code,
            self.notify_min_seconds,
            self.notify_only_on_failure,
            since_last,
            NOTIFY_MIN_GAP,
        ) {
            return None;
        }
        self.notify_last_at.insert(key, std::time::Instant::now());
        let failed = matches!(c.exit_code, Some(code) if code != 0);
        let (summary, body) = build_notification_text(&self.main_tab_label(tab), &c, failed);
        self.notifier.fire(summary, body, failed);
        // Taskbar/dock urgency baseline — the guaranteed macOS signal (dock bounce)
        // and a cross-DE hint on Linux even where no notification daemon runs.
        if let Some(w) = &self.window {
            w.request_user_attention(Some(attention_for(failed)));
        }
        Some(failed)
    }

    /// Gate + fire a notification for a DETACHED window's completion. Gated on
    /// THAT window's own `focused`/`occluded` (amendments §2) — a detached window
    /// has no F9 hide, so "watching" == focused and not occluded.
    fn maybe_notify_detached(&mut self, pos: usize, c: jetty_core::CommandCompletion) {
        let (watching, wid) = {
            let dw = &self.detached[pos];
            (dw.focused && !dw.occluded, dw.window.id())
        };
        let key = NotifyKey::Detached(wid);
        let since_last = self.notify_last_at.get(&key).map(|t| t.elapsed());
        if !crate::notify::should_notify(
            watching,
            c.duration,
            c.exit_code,
            self.notify_min_seconds,
            self.notify_only_on_failure,
            since_last,
            NOTIFY_MIN_GAP,
        ) {
            return;
        }
        self.notify_last_at.insert(key, std::time::Instant::now());
        let failed = matches!(c.exit_code, Some(code) if code != 0);
        let label = format!("{} (detached)", self.detached[pos].tab.title);
        let (summary, body) = build_notification_text(&label, &c, failed);
        self.notifier.fire(summary, body, failed);
        self.detached[pos]
            .window
            .request_user_attention(Some(attention_for(failed)));
    }

    /// Update the live perf-HUD metrics and return the formatted HUD string, or
    /// `None` when the HUD is disabled (`show_perf_hud == false`).
    ///
    /// CRITICAL — IDLE-PATH INVARIANT: this is called ONLY from the render path
    /// (inside a frame that is already happening for some other reason). It NEVER
    /// calls `request_redraw()` and NEVER schedules a timer, so it cannot wake the
    /// app or regress the 0-CPU `ControlFlow::Wait` idle. When idle the HUD simply
    /// freezes at its last value.
    ///
    /// Cost discipline:
    /// - frame ms: one `Instant::now()` diff + exponential smooth (per frame).
    /// - CPU%: sysinfo refresh of THIS process ONLY, gated to ≤1 Hz (sysinfo needs
    ///   ≥~200ms between samples for a valid %), so it's nearly free per frame.
    /// - VT MB/s: derived from the running `vt_bytes` counter over ~1s windows.
    fn update_perf_hud(&mut self) -> Option<String> {
        if !self.show_perf_hud {
            return None;
        }
        let now = std::time::Instant::now();

        // Frame time: exponentially-smoothed dt since the previous rendered frame.
        if let Some(prev) = self.last_frame_at {
            let dt_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
            // Ignore absurd gaps (e.g. after a long idle) so one stale dt doesn't
            // spike the smoothed value; treat a >1s gap as a fresh start.
            if dt_ms <= 1000.0 {
                if self.perf_ms <= 0.0 {
                    self.perf_ms = dt_ms;
                } else {
                    self.perf_ms = self.perf_ms * 0.9 + dt_ms * 0.1;
                }
            }
        }
        self.last_frame_at = Some(now);

        // CPU%: refresh only this process, at most once per second.
        if now.duration_since(self.last_cpu_at) >= std::time::Duration::from_secs(1) {
            self.last_cpu_at = now;
            self.perf_sys.refresh_processes(
                sysinfo::ProcessesToUpdate::Some(&[self.perf_pid]),
                true,
            );
            if let Some(proc_) = self.perf_sys.process(self.perf_pid) {
                // sysinfo reports CPU as a % of ONE core (can exceed 100). Keep as-is.
                self.perf_cpu = proc_.cpu_usage();
            }
        }

        // VT throughput: bytes/s over the current ~1s window → MB/s.
        let win = now.duration_since(self.vt_window_start).as_secs_f32();
        if win >= 1.0 {
            let delta = self.vt_bytes.saturating_sub(self.vt_bytes_at_window_start);
            self.perf_mb = (delta as f32 / win) / (1024.0 * 1024.0);
            self.vt_window_start = now;
            self.vt_bytes_at_window_start = self.vt_bytes;
        }

        let ms = if self.perf_ms > 0.0 { self.perf_ms } else { 0.0 };
        let fps = if ms > 0.0 { (1000.0 / ms).round().clamp(0.0, 9999.0) as i32 } else { 0 };
        Some(format!(
            "⚡ {ms:.1} ms · {fps} fps · {cpu:.0}% CPU · {mb:.0} MB/s",
            ms = ms,
            fps = fps,
            cpu = self.perf_cpu,
            mb = self.perf_mb,
        ))
    }

    /// Close every tab index in `exited` (descending so earlier indices stay
    /// valid), fixing up `active`. If no tabs remain anywhere, exit the event
    /// loop; if detached windows still exist, the first detached tab is
    /// reattached instead so their shells survive.
    /// Returns true if the app should keep running.
    fn close_exited_tabs(&mut self, mut exited: Vec<usize>, event_loop: &ActiveEventLoop) -> bool {
        if exited.is_empty() {
            return true;
        }
        if exited.contains(&self.active) {
            // The searched (active) tab's shell exited: close the bar before
            // the removals below retarget it (same invariant as close_tab).
            self.search_close();
        }
        exited.sort_unstable();
        exited.dedup();
        for &i in exited.iter().rev() {
            if i < self.tabs.len() {
                self.tabs.remove(i);
            }
            // Adjust the active index and the index-bearing UI state the same way
            // for each removed tab (highest first) so they all stay aligned.
            if self.active == i {
                // The active tab itself exited; clamp below.
            } else if self.active > i {
                self.active -= 1;
            }
            Self::adjust_index_after_remove(&mut self.renaming, i);
            Self::adjust_index_after_remove(&mut self.confirm_close, i);
        }
        if self.tabs.is_empty() {
            // Exit only when NO tabs exist anywhere: while detached windows
            // hold live shells, adopt the first detached tab into the main
            // window instead (its window closes; the shell keeps running).
            if self.detached.is_empty() {
                event_loop.exit();
                return false;
            }
            self.reattach_tab(0, event_loop);
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        if self.renaming.is_none() {
            self.rename_buf.clear();
        }
        self.selecting = false;
        // Same index-invalidation as `close_tab`: drop the transient tab menu /
        // held tab drag now that the tab layout changed under them.
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
        self.tab_drag = None;
        self.request_main_paint();
        true
    }

    /// Shared reflow path: compute cols/rows from the current GPU surface size
    /// and the current TextLayer cell size, then resize the terminal and PTY.
    ///
    /// Called from both `WindowEvent::Resized` and `set_font_size` so both
    /// features share one code path.
    fn reflow(&mut self) {
        let status_h = self.status_h();
        let (Some(gpu), Some(text)) = (&self.gpu, &self.text) else { return };
        let (cw, ch) = text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let w = gpu.config.width;
        let h = gpu.config.height;
        let cols = ((w as f32 - SCROLLBAR_GUTTER) / cw).floor().max(2.0) as usize;
        // The grid occupies the area below the tab bar and above the status bar.
        let rows = ((h as f32 - TABBAR_H - status_h) / ch).floor().max(1.0) as usize;
        // Reflow every tab so background sessions stay in sync with the window.
        for tab in &mut self.tabs {
            tab.terminal.resize(cols, rows);
            // Keep the sixel footprint metric current (font/DPI/window change);
            // this is the single chokepoint every main-window resize funnels through.
            tab.terminal.set_cell_px(cw, ch);
            tab.pty.resize(
                cols as u16,
                rows as u16,
                (cols as f32 * cw).min(65535.0) as u16,
                (rows as f32 * ch).min(65535.0) as u16,
            );
        }
        // Every background shell just got a SIGWINCH from US: their prompt
        // repaints are self-inflicted, not "unseen output" — arm the activity
        // grace so drain_pty doesn't light false dots on every resize (F3).
        self.reflow_resized_at = Some(std::time::Instant::now());
        // The grid just reflowed under a possibly-held Ctrl+hover: the cached
        // underline spans are in OLD-grid cell coords and the same (line,col)
        // cell index would skip the lazy recompute — revalidate now, exactly
        // like the scroll/tab-switch paths do (F6). No-op unless a link
        // modifier is held.
        self.update_link_hover(true);
    }

    /// Change the font size at runtime. `new_logical` is clamped to [6.0, 48.0].
    /// Rebuilds TextLayer with the new physical font size (logical * scale),
    /// then calls `reflow()` to recompute the grid, and requests a redraw.
    fn set_font_size(&mut self, new_logical: f32) {
        let clamped = new_logical.clamp(6.0, 48.0);
        self.font_logical = clamped;
        let scale = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
        // Resize the font IN-PLACE, reusing the existing FontSystem — rebuilding
        // it (new_with_family) would rescan fontconfig (~20ms) on the main thread
        // on every Ctrl+/Ctrl- press. The family list is unchanged by a size
        // change, so it does not need re-querying.
        if let Some(t) = self.text.as_mut() {
            t.set_font_size(clamped * scale);
        }
        // DEBOUNCE the WHOLE grid+PTY reflow — do NOT resize the terminal grid on
        // each press. Reflowing the grid repeatedly while the shell can't redraw
        // re-wraps p10k's absolute-positioned (non-reflow-friendly) prompt over and
        // over, scattering prompt fragments across the screen. Instead schedule ONE
        // reflow() after the user stops: it resizes the grid AND the PTY together,
        // so the shell gets a single SIGWINCH and repaints its prompt once, cleanly.
        // The new cell size is visible immediately via the rebuilt TextLayer; the
        // grid snaps to the new col/row count when the reflow fires. The window is
        // generous (250ms) so even DELIBERATE, one-at-a-time Ctrl+/- presses (which
        // a short window let through, each firing its own reflow → a staircase of
        // p10k prompts) collapse into a single reflow.
        self.reflow_pending_at =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
        // Propagate to detached windows for visual parity (theme/opacity already
        // do). Each uses its OWN scale_factor; the grid+PTY reflow is debounced
        // via reflow_pending_at so the shell gets one SIGWINCH after the burst,
        // exactly like the main window (F7/F20). Without this a detached window
        // kept its detach-time font until a DPI change snapped it.
        let reflow_at = std::time::Instant::now() + std::time::Duration::from_millis(250);
        for dw in &mut self.detached {
            let dscale = dw.window.scale_factor() as f32;
            dw.text.set_font_size(clamped * dscale);
            dw.reflow_pending_at = Some(reflow_at);
            dw.request_paint();
        }
        // FontUp/Down/Reset are Ctrl chords — the link modifier is BY
        // DEFINITION held when they fire, and the cell metrics just changed
        // in-place: cached underline spans would draw at the new cell size in
        // old-grid coords, and a same-index hovered cell would suppress the
        // lazy recompute. Revalidate every window's hover now (F6); the
        // debounced reflow() revalidates again when the grid snaps.
        self.update_link_hover(true);
        for pos in 0..self.detached.len() {
            self.update_detached_link_hover(pos, true);
        }
        self.persist();
        self.request_main_paint();
    }

    /// Change the font family at runtime. Updates `font_family`, tells the
    /// TextLayer to remeasure, then reflows and requests a redraw.
    fn set_font_family(&mut self, name: String) {
        self.font_family = name;
        if let Some(text) = &mut self.text {
            text.set_font_family(&self.font_family);
        }
        // Detached windows: swap their terminal font too, then debounce their
        // grid/PTY reflow (family changes cell width → cols/rows) (F7/F20).
        let reflow_at = std::time::Instant::now() + std::time::Duration::from_millis(250);
        for dw in &mut self.detached {
            dw.text.set_font_family(&self.font_family);
            dw.reflow_pending_at = Some(reflow_at);
            dw.request_paint();
        }
        // The chrome is now DECOUPLED from the terminal font: it follows the
        // separate `ui_font_family`/`ui_font_logical` (set via `set_ui_font_*`),
        // NOT the terminal family. So a terminal-font change no longer touches
        // chrome_text — leaving the chrome typeface stable while the grid font
        // changes (and avoiding a chrome re-measure on every terminal-font pick).
        self.reflow();
        self.persist();
        self.request_main_paint();
    }

    /// Change the UI (chrome) font SIZE at runtime, clamped [10, 28]. Resizes the
    /// chrome + settings text layers IN-PLACE (reusing their FontSystems — never
    /// `new_with_family`, which would rescan fontconfig ~20ms). The settings panel
    /// body text is CAPPED to [13, 17] so the absolute-px panel layout never
    /// overflows, while the rest of the chrome (and the live "Aa" specimen) tracks
    /// the true size. Crucially does NOT reflow the grid/PTY: chrome size has no
    /// effect on cols/rows, so there is no p10k-scatter risk and no debounce — the
    /// idle 0-CPU path is untouched.
    fn set_ui_font_size(&mut self, new_logical: f32) {
        self.ui_font_logical = new_logical.clamp(UI_FONT_MIN, UI_FONT_MAX);
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        if let Some(ct) = self.chrome_text.as_mut() {
            ct.set_font_size(self.ui_font_logical * scale);
        }
        // The settings-window text layer uses its OWN scale_factor; cap its size.
        let settings_scale = self
            .settings_window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(scale);
        if let Some(st) = self.settings_text.as_mut() {
            let capped = self.ui_font_logical.clamp(PANEL_TEXT_MIN, PANEL_TEXT_MAX);
            st.set_font_size(capped * settings_scale);
        }
        // The specimen layer tracks the TRUE (uncapped) size so the "Aa" preview is honest.
        if let Some(sp) = self.settings_specimen_text.as_mut() {
            sp.set_font_size(self.ui_font_logical * settings_scale);
        }
        // Detached windows: resize THEIR chrome font (title/status/menu) at each
        // window's own scale. No grid reflow — chrome size is orthogonal to
        // cols/rows (F7/F20).
        let ui_logical = self.ui_font_logical;
        for dw in &mut self.detached {
            let dscale = dw.window.scale_factor() as f32;
            dw.chrome_text.set_font_size(ui_logical * dscale);
            dw.request_paint();
        }
        self.persist();
        self.request_main_paint();
        // Live preview in the settings window (specimen + readout) if it's open.
        self.render_settings_window();
        self.request_settings_paint();
    }

    /// Change the UI (chrome) font FAMILY at runtime. `""` selects the platform
    /// proportional sans. Swaps the chrome + settings layers' `ui_family` via the
    /// no-rescan `set_ui_family` (the chrome FontSystem already holds every
    /// installed family). Does NOT reflow the grid/PTY (chrome family is
    /// orthogonal to cols/rows), so the hot/idle paths are untouched.
    fn set_ui_font_family(&mut self, name: String) {
        self.ui_font_family = name;
        let fam = if self.ui_font_family.is_empty() {
            None
        } else {
            Some(self.ui_font_family.as_str())
        };
        if let Some(ct) = self.chrome_text.as_mut() {
            ct.set_ui_family(fam);
        }
        if let Some(st) = self.settings_text.as_mut() {
            st.set_ui_family(fam);
        }
        if let Some(sp) = self.settings_specimen_text.as_mut() {
            sp.set_ui_family(fam);
        }
        // Detached windows: swap THEIR chrome family too. No grid reflow —
        // chrome family is orthogonal to cols/rows (F7/F20).
        for dw in &mut self.detached {
            dw.chrome_text.set_ui_family(fam);
            dw.request_paint();
        }
        self.persist();
        self.request_main_paint();
        self.render_settings_window();
        self.request_settings_paint();
    }

    /// Perform the Yakuake-style focus-loss auto-hide of the main window.
    /// Called from `about_to_wait` when the `pending_autohide_at` grace period
    /// elapsed without any JeTTY window regaining focus (see the field docs).
    fn autohide_main_window(&mut self) {
        if !self.visible {
            return;
        }
        if let Some(win) = &self.window {
            if self.window_mode == WindowMode::Center {
                self.last_pos = win.outer_position().ok();
            }
            self.slide_anim = None;
            // Also stop a mid-flight summon animation: its only expiry point is
            // inside the acquire_frame success path, which a hidden surface may
            // never reach — a stuck summon_anim would pin the loop in Poll.
            self.summon_anim = None;
            self.summon_pending = false;
            win.set_visible(false);
        }
        self.visible = false;
        // The matching button-release never arrives once hidden — clear the
        // terminal drag state so it doesn't resume stuck on the next summon.
        self.selecting = false;
        self.dragging_scrollbar = false;
        // Clear the remaining self-drive terms whose ONLY expiry point is inside
        // RedrawRequested — which a hidden (orderOut) window never receives on
        // macOS — so they can't pin about_to_wait in Poll and spin 100% CPU while
        // hidden (F18). Re-armed naturally on the next keystroke/summon.
        self.caret_anim = None;
        self.pending_dock_frames = 0;
        self.pending_center_frames = 0;
    }

    /// Toggle window visibility (F9 / Yakuake-style summon).
    ///
    /// When summoning (making visible), the window is re-centred on its
    /// current monitor, focused, and redrawn. The PTY keeps running while the
    /// window is hidden — nothing is killed or suspended.
    fn toggle_visibility(&mut self, event_loop: &ActiveEventLoop) {
        self.set_visibility(!self.visible, event_loop);
    }

    fn set_visibility(&mut self, want: bool, _event_loop: &ActiveEventLoop) {
        // A redundant `--show` (already visible) just raises/focuses; a redundant
        // `--hide` (already hidden) is a no-op.
        if want == self.visible {
            if want {
                // An explicit summon supersedes any scheduled focus-loss auto-hide,
                // even on this early-return path — otherwise a `jetty --show` landing
                // inside the grace window let the just-summoned terminal hide ≤100ms
                // later if the WM's FocusIn didn't beat the deadline (F31).
                self.pending_autohide_at = None;
                if let Some(win) = &self.window {
                    win.focus_window();
                    self.request_main_paint();
                }
            }
            return;
        }
        self.visible = want;
        // An explicit visibility change supersedes any scheduled auto-hide.
        self.pending_autohide_at = None;
        let mode = self.window_mode;
        if let Some(win) = &self.window {
            if self.visible {
                match mode {
                    WindowMode::Center => {
                        win.set_visible(true);
                        // Re-summon at the spot the user left it; first → center.
                        // X11/KWin ignores a position issued before the window is
                        // mapped, so re-assert it on the next few post-map redraws
                        // (mirrors pending_dock_frames) or the saved spot is lost.
                        match self.last_pos {
                            // Only restore a saved position that still lands on a
                            // connected monitor. If the monitor was unplugged while
                            // hidden, the verbatim restore (plus the 5-frame
                            // re-assertion) would map the window off-screen and
                            // fight any WM rescue — center on a live monitor
                            // instead and forget the stale spot (F32).
                            Some(pos) if pos_on_some_monitor(win, pos) => {
                                win.set_outer_position(pos);
                                self.pending_center_pos = Some(pos);
                                self.pending_center_frames = 5;
                            }
                            _ => {
                                center_window(win);
                                self.pending_center_pos = None;
                                self.pending_center_frames = 0;
                                self.last_pos = None;
                            }
                        }
                    }
                    WindowMode::Dropdown => {
                        // Show FIRST so the window is mapped, THEN dock: on X11 a
                        // dock issued before the window is realized is ignored by
                        // the WM (the window lands centered). pending_dock_frames
                        // re-asserts the top-strip geometry on the next few
                        // post-map redraws so it actually docks to the top.
                        win.set_visible(true);
                        dock_window_top(win, self.dropdown_width_pct, self.dropdown_height_pct);
                        self.pending_dock_frames = 5;
                        // Arm the render-side slide-down.
                        self.slide_anim = Some(std::time::Instant::now());
                    }
                }
                win.focus_window();
                // Crystallize/reveal on every summon (F9 show), mirroring first open.
                // Start the clock on the FIRST real frame (summon_pending), not here:
                // on macOS the window can take a beat to present, which would
                // otherwise let the whole effect elapse unseen (effectless).
                self.summon_pending = true;
                self.summon_settle_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
                self.request_main_paint();
            } else {
                // Remember the current spot before hiding so the next Center
                // summon restores it. Dropdown re-docks, so last_pos is unused.
                if mode == WindowMode::Center {
                    self.last_pos = win.outer_position().ok();
                }
                self.slide_anim = None;
                // Expire a mid-flight summon animation as well: its only other
                // expiry point is inside the acquire_frame success path, which a
                // hidden surface may never reach (Occluded/Timeout) — a stuck
                // summon_anim would pin about_to_wait in Poll (busy loop) for as
                // long as the window stays hidden.
                self.summon_anim = None;
                self.summon_pending = false;
                win.set_visible(false);
                // The matching button-release never arrives once hidden — clear
                // the terminal drag state so it doesn't resume stuck on the next
                // summon (mirrors autohide_main_window; the F9/IPC hide path
                // reaches here too).
                self.selecting = false;
                self.dragging_scrollbar = false;
                // Clear the self-drive terms whose only expiry is in
                // RedrawRequested (never delivered to a hidden macOS window) so
                // they don't pin Poll and spin 100% CPU while hidden (F18).
                self.caret_anim = None;
                self.pending_dock_frames = 0;
                self.pending_center_frames = 0;
            }
        }
    }

    /// Toggle the separate Settings window. If it is closed, create it (window +
    /// its own GPU/text/quad stack) and show it. If it is already open, close it
    /// by dropping the window and its render stack so it disappears. The terminal
    /// and PTY are never affected either way.
    fn toggle_settings_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.settings_window.is_some() {
            self.close_settings_window();
            // Repaint the main window (nothing visual changed there now, but keep
            // it responsive/consistent).
            self.request_main_paint();
            return;
        }

        let window = match jetty_platform::build_fixed_window(
            event_loop,
            "JeTTY — Settings",
            (SETTINGS_WIN_W, SETTINGS_WIN_H),
        ) {
            Ok(w) => w,
            Err(e) => {
                // Window creation can fail at runtime (X resource/fd exhaustion,
                // compositor restart). Abort opening Settings instead of killing
                // the whole app; the terminal keeps running.
                eprintln!("jetty: failed to open settings window: {e}");
                return;
            }
        };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        if let Some(ref g) = gpu {
            // The settings panel body text renders at the CAPPED UI size ([13,17])
            // so the absolute-px panel layout never overflows its fixed window,
            // independent of the terminal font. The chosen UI family is applied via
            // set_ui_family (no rescan). The true UI size is used only for the live
            // "Aa" specimen, drawn separately via chrome_text.
            let capped = self.ui_font_logical.clamp(PANEL_TEXT_MIN, PANEL_TEXT_MAX);
            let mut text = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, capped * scale, &self.font_family,
            );
            let ui_fam = if self.ui_font_family.is_empty() {
                None
            } else {
                Some(self.ui_font_family.as_str())
            };
            text.set_ui_family(ui_fam);
            // Dedicated TRUE-size specimen layer on the settings device for the
            // live "Aa" preview (the panel body text above is capped).
            let mut specimen = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, self.ui_font_logical * scale, &self.font_family,
            );
            specimen.set_ui_family(ui_fam);
            let quad = QuadLayer::new(&g.device, g.format);
            self.settings_text = Some(text);
            self.settings_specimen_text = Some(specimen);
            self.settings_quad = Some(quad);
        }
        self.settings_gpu = gpu;
        window.focus_window();
        window.request_redraw();
        self.settings_window = Some(window);
        // macOS: keep repainting under Poll for a short window so the surface
        // presents once macOS has displayed the new window (a single redraw on
        // open is dropped, leaving it blank until clicked).
        self.settings_paint_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
        // …and draw the first frame SYNCHRONOUSLY now, before returning to the
        // event loop, so the window is never shown blank even for a frame.
        self.render_settings_window();
        if self.debug {
            eprintln!("SETTINGS window opened");
        }
    }

    /// Drop the settings window and its render stack (closes/hides the OS window).
    fn close_settings_window(&mut self) {
        // Drop any focus bookkeeping that pointed at the now-destroyed settings
        // window so the main window's auto-hide guard doesn't malfunction.
        if self.last_focused_window == self.settings_window.as_ref().map(|w| w.id()) {
            self.last_focused_window = None;
        }
        self.switching_to_settings = false;
        self.settings_window = None;
        self.settings_gpu = None;
        self.settings_text = None;
        self.settings_specimen_text = None;
        self.settings_quad = None;
        // Clear all drag flags so any in-progress drag when the window closes
        // doesn't leave a stale flag set that misbehaves on reopen.
        self.dragging_slider = false;
        self.dragging_radius = false;
        self.dragging_dropdown = false;
        self.dragging_dropdown_width = false;
        self.active_fx_drag = None;
        // Collapse the Look-tab theme dropdown so reopening Settings starts with
        // it closed (its "collapsed unless the user opens it" session semantics).
        // The Escape / OS-close paths bypass handle_settings_action where the
        // collapse normally happens, so without this the panel reopened with the
        // menu already popped open at a stale scroll offset (F28).
        self.theme_dropdown_open = false;
        self.theme_scroll_offset = 0;
        if self.debug {
            eprintln!("SETTINGS window closed");
        }
    }

    /// Build the panel view for the settings window in its own coordinate space
    /// (the panel is centred to fill the fixed-size window; no drag offset).
    fn settings_panel_view(&self, w: u32, h: u32) -> jetty_render::PanelView {
        let theme = self.current_theme();
        let fx = jetty_render::EffectsParams {
            crt_enabled: self.fx.crt_enabled,
            crt_curvature: self.fx.crt_curvature,
            crt_scanline: self.fx.crt_scanline,
            crt_mask: self.fx.crt_mask,
            crt_bloom: self.fx.crt_bloom,
            crt_chromatic: self.fx.crt_chromatic,
            crt_vignette: self.fx.crt_vignette,
            crt_scanline_tint: self.fx.crt_scanline_tint,
            crt_animate_roll: self.fx.crt_animate_roll,
            crt_flicker: self.fx.crt_flicker,
            crt_jitter: self.fx.crt_jitter,
            caret_flash_enabled: self.fx.caret_flash_enabled,
            caret_glow_enabled: self.fx.caret_glow_enabled,
            caret_flash_ms: self.fx.caret_flash_ms,
            caret_flash_color: self.fx.caret_flash_color,
        };
        jetty_render::build_panel(
            w, h, self.opacity, self.theme_idx, self.font_logical,
            &self.font_families, &self.font_family, self.font_scroll_offset,
            self.corner_radius, self.summon_effect.display_name(),
            self.window_mode.display_name(),
            if self.tab_bar_bottom { "Bottom" } else { "Top" },
            &format_scrollback(self.scrollback_lines),
            self.dropdown_height_pct,
            self.dropdown_width_pct,
            self.window_mode == WindowMode::Dropdown, self.focus_autohide,
            self.launch_at_login,
            self.ui_font_logical, &self.ui_font_families, &self.ui_font_family,
            self.ui_font_scroll_offset,
            0.0, 0.0, &theme, self.settings_char_w(),
            &self.shell_display(),
            &jetty_render::NotifyParams {
                enabled: self.notify_on_finish,
                only_on_failure: self.notify_only_on_failure,
                min_seconds: self.notify_min_seconds,
                auto_summon: self.auto_summon_on_finish,
            },
            self.settings_tab,
            &fx,
            self.effects_scroll,
            self.theme_dropdown_open,
            self.theme_scroll_offset,
        )
    }

    /// Render the settings panel into the settings window's surface.
    fn render_settings_window(&mut self) {
        let opacity = self.opacity;
        let theme_idx = self.theme_idx;
        let font_logical = self.font_logical;
        let font_scroll_offset = self.font_scroll_offset;
        let corner_radius = self.corner_radius;
        let summon_name = self.summon_effect.display_name();
        let window_mode_name = self.window_mode.display_name();
        let dropdown_height_pct = self.dropdown_height_pct;
        let dropdown_width_pct = self.dropdown_width_pct;
        let is_dropdown = self.window_mode == WindowMode::Dropdown;
        let focus_autohide = self.focus_autohide;
        let launch_at_login = self.launch_at_login;
        let tab_bar_name = if self.tab_bar_bottom { "Bottom" } else { "Top" };
        let scrollback_name = format_scrollback(self.scrollback_lines);
        // Clone the small inputs build_panel needs so we can borrow the render
        // stack mutably below without overlapping the immutable self borrows.
        let families = self.font_families.clone();
        let family = self.font_family.clone();
        let ui_families = self.ui_font_families.clone();
        let ui_family = self.ui_font_family.clone();
        let ui_font_logical = self.ui_font_logical;
        let ui_font_scroll_offset = self.ui_font_scroll_offset;
        let shell_display = self.shell_display();
        let settings_tab = self.settings_tab;
        let theme = self.current_theme();
        // Panel labels use the SETTINGS (capped) layer advance so right-align /
        // truncation match the layer that draws them.
        let char_w = self.settings_char_w();
        // Specimen color: the theme's blue accent, so the preview pops against the
        // panel surface (same accent the panel chrome uses for handles/selection).
        let accent = theme.palette[4];
        let specimen_rgb = [accent[0], accent[1], accent[2]];
        // Clone Effects params before the mutable borrow of the render stack below.
        let fx = jetty_render::EffectsParams {
            crt_enabled: self.fx.crt_enabled,
            crt_curvature: self.fx.crt_curvature,
            crt_scanline: self.fx.crt_scanline,
            crt_mask: self.fx.crt_mask,
            crt_bloom: self.fx.crt_bloom,
            crt_chromatic: self.fx.crt_chromatic,
            crt_vignette: self.fx.crt_vignette,
            crt_scanline_tint: self.fx.crt_scanline_tint,
            crt_animate_roll: self.fx.crt_animate_roll,
            crt_flicker: self.fx.crt_flicker,
            crt_jitter: self.fx.crt_jitter,
            caret_flash_enabled: self.fx.caret_flash_enabled,
            caret_glow_enabled: self.fx.caret_glow_enabled,
            caret_flash_ms: self.fx.caret_flash_ms,
            caret_flash_color: self.fx.caret_flash_color,
        };
        let effects_scroll = self.effects_scroll;
        let theme_dropdown_open = self.theme_dropdown_open;
        let theme_scroll_offset = self.theme_scroll_offset;
        // Run & Notify params captured before the mutable render-stack borrow.
        let notify_params = jetty_render::NotifyParams {
            enabled: self.notify_on_finish,
            only_on_failure: self.notify_only_on_failure,
            min_seconds: self.notify_min_seconds,
            auto_summon: self.auto_summon_on_finish,
        };
        let (Some(gpu), Some(text), Some(quad), Some(specimen)) = (
            &mut self.settings_gpu,
            &mut self.settings_text,
            &mut self.settings_quad,
            &mut self.settings_specimen_text,
        ) else {
            return;
        };
        let width = gpu.config.width;
        let height = gpu.config.height;
        let pv = jetty_render::build_panel(
            width, height, opacity, theme_idx, font_logical,
            &families, &family, font_scroll_offset, corner_radius, summon_name,
            window_mode_name, tab_bar_name, &scrollback_name, dropdown_height_pct, dropdown_width_pct, is_dropdown, focus_autohide,
            launch_at_login,
            ui_font_logical, &ui_families, &ui_family, ui_font_scroll_offset,
            0.0, 0.0, &theme, char_w,
            &shell_display,
            &notify_params,
            settings_tab,
            &fx,
            effects_scroll,
            theme_dropdown_open,
            theme_scroll_offset,
        );
        if let Some((frame, view)) = gpu.acquire_frame() {
            // Pass 1: Chrome quads — panel border, bg, chips, opacity/radius tracks,
            // tab strip highlights, etc. Uses LoadOp::Clear so the surface starts
            // fresh. No scissor: chrome elements are always fully in-bounds.
            quad.render_clear(
                &gpu.device,
                &gpu.queue,
                &view,
                width,
                height,
                &pv.quads,
                wgpu::Color { r: 0.02, g: 0.02, b: 0.03, a: 1.0 },
            );

            // Pass 2: Effects-tab widget quads — only when on the Effects tab.
            // Uses LoadOp::Load (composites on existing chrome) + hardware scissor
            // so widgets that have scrolled outside the content viewport are clipped.
            if let Some(vp) = pv.effects_viewport {
                if !pv.effects_quads.is_empty() {
                    quad.render_load_scissored(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        width,
                        height,
                        &pv.effects_quads,
                        vp,
                    );
                }
            }

            // Pass 3: Chrome text — title, tab strip, non-Effects widget labels.
            // No clip: these are always within bounds.
            if !pv.labels.is_empty() {
                let _ = text.render_overlays(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    width,
                    height,
                    &pv.labels,
                );
            }

            // Pass 4: Effects-tab widget labels, clipped to content viewport via
            // glyphon TextArea.bounds so labels outside the scroll window are
            // suppressed without a GPU scissor (glyphon handles it per-glyph).
            if let Some(vp) = pv.effects_viewport {
                if !pv.effects_labels.is_empty() {
                    let clip_top = vp[1] as i32;
                    let clip_bottom = (vp[1] + vp[3]) as i32;
                    let _ = text.render_overlays_clipped(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        width,
                        height,
                        &pv.effects_labels,
                        clip_top,
                        clip_bottom,
                    );
                }
            }

            // Overdraw the live "Aa" specimen at the TRUE UI size via the dedicated
            // specimen layer, AFTER the capped panel-text pass — so the user sees an
            // honest big/small/typeface preview that tracks ui_font_size. Use the
            // TITLE path so at the `""` default it previews the platform SANS (the
            // actual default UI face), and a chosen family otherwise.
            let (sx, sy) = pv.ui_specimen_pos;
            let _ = specimen.render_overlays_sans(
                &gpu.device,
                &gpu.queue,
                &view,
                width,
                height,
                &[("Aa".to_string(), sx, sy, specimen_rgb)],
            );
            frame.present();
            // Missed-paint proof counter (JETTY_FRAME_LOG only).
            if self.frame_log {
                self.frames_presented += 1;
                eprintln!("JETTY_FRAME {} settings", self.frames_presented);
            }
        }
    }

    /// Route a `WindowEvent` addressed to the detached window at `self.detached[pos]`:
    /// rendering, keyboard, resize reflow, the top-bar chrome (close→reattach,
    /// manual drag-to-move, double-click maximize), borderless resize edges, the
    /// Reattach/Copy/Paste context menu, and drop-to-reattach on drag release.
    fn handle_detached_event(
        &mut self,
        pos: usize,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::RedrawRequested => self.render_detached_window(pos),
            WindowEvent::Occluded(occluded) if pos < self.detached.len() => {
                // Track per-window occlusion/minimize so a hidden detached window
                // stops self-driving CRT/caret animation and PTY-output redraws
                // (F8/F17). On un-occlude, repaint once.
                self.detached[pos].occluded = occluded;
                if !occluded {
                    self.detached[pos].request_paint();
                }
            }
            WindowEvent::CloseRequested if pos < self.detached.len() => {
                self.reattach_tab(pos, event_loop);
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } if event.state.is_pressed() => {
                // Ignore X11's synthetic focus-gain key presses (keys physically
                // held while this window takes focus) — same guard as the main
                // window's KeyboardInput arm.
                if is_synthetic {
                    return;
                }
                // Same modifier/decode pipeline as the main window's
                // `KeyboardInput` arm, except `app_cursor`/`alt_screen` are sourced
                // from THIS window's own terminal and `panel_open` is always false.
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let alt = self.modifiers.alt_key();
                // macOS Cmd chords in a detached window are now folded into the
                // keymap and dispatched below through the SAME action path as the
                // main window (Copy/Paste/SelectAll/NewTab/CloseTab=reattach/font/
                // settings). decide_key's keymap lookup preserves the "swallow
                // unmapped Cmd" safety net. Cmd+Q / Cmd+P stay detached no-ops
                // (guarded below) — byte-identical with today's detached Cmd block,
                // which had no q/p arm.
                let sup = self.modifiers.super_key();
                let (app_cursor, alt_screen) = {
                    let Some(dw) = self.detached.get(pos) else { return };
                    (dw.tab.terminal.app_cursor_keys(), dw.tab.terminal.alt_screen())
                };
                // macOS Option-compose: see the matching comment on the main
                // window's arm — Alt + a composed non-ASCII glyph is sent as
                // text instead of being ESC-prefixed by decide_key.
                let composed: Option<Vec<u8>> = if alt && !ctrl {
                    event.text.as_ref().and_then(|t| {
                        if !t.is_empty() && t.chars().all(|c| !c.is_control()) && !t.is_ascii() {
                            Some(t.as_bytes().to_vec())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                // Dead-key composition fallback — mirrors the main window's arm.
                // GATED on `!sup` so a bare Cmd chord routes through decide_key's
                // keymap+swallow instead of being sent as composed text.
                let dead_key = if sup {
                    None
                } else {
                    input::dead_key_text_override(ctrl, alt, &event.logical_key, event.text.as_deref())
                };
                let action = match composed.or(dead_key) {
                    Some(bytes) => input::KeyAction::Send(bytes),
                    None => input::decide_key(
                        &self.keymap,
                        ctrl,
                        shift,
                        alt,
                        sup,
                        event.physical_key,
                        &event.logical_key,
                        false,
                        app_cursor,
                        alt_screen,
                    ),
                };
                // App-WIDE shortcuts advertised in README/help now work in a
                // detached window too (they were dropped by the `_ => {}` arm — F39).
                // Handled via `self`, so they must run BEFORE the `dw` borrow below;
                // each returns. Font/opacity changes reach the detached window live
                // via the setters' propagation (F7/F20).
                match &action {
                    input::KeyAction::TogglePanel => {
                        self.toggle_settings_window(event_loop);
                        return;
                    }
                    input::KeyAction::NewTab => {
                        // Inherit THIS detached tab's cwd; the new tab still
                        // opens in the main window as today.
                        let cwd = self.detached.get(pos).and_then(|dw| dw.tab.pty.cwd());
                        self.new_tab_with_cwd(cwd);
                        return;
                    }
                    input::KeyAction::NextTab => {
                        self.switch_tab(true);
                        return;
                    }
                    input::KeyAction::PrevTab => {
                        self.switch_tab(false);
                        return;
                    }
                    input::KeyAction::SelectTab(n) => {
                        self.select_tab(*n);
                        return;
                    }
                    // "Close tab" for a single-tab detached window = reattach it to
                    // the main window (its ✕ semantics), never losing the shell.
                    input::KeyAction::CloseTab => {
                        self.reattach_tab(pos, event_loop);
                        return;
                    }
                    input::KeyAction::FontUp => {
                        self.set_font_size(self.font_logical + 1.0);
                        return;
                    }
                    input::KeyAction::FontDown => {
                        self.set_font_size(self.font_logical - 1.0);
                        return;
                    }
                    input::KeyAction::FontReset => {
                        self.set_font_size(FONT_LOGICAL_DEFAULT);
                        return;
                    }
                    input::KeyAction::OpacityUp => {
                        self.opacity = (self.opacity + 0.05).min(1.0);
                        self.apply_theme();
                        self.persist();
                        self.request_main_paint();
                        for dw in &self.detached { dw.request_paint(); }
                        return;
                    }
                    input::KeyAction::OpacityDown => {
                        self.opacity = (self.opacity - 0.05).max(0.1);
                        self.apply_theme();
                        self.persist();
                        self.request_main_paint();
                        for dw in &self.detached { dw.request_paint(); }
                        return;
                    }
                    // Scrollback search is main-window-only this release: a
                    // detached window swallows the chord as a clean no-op —
                    // it never sends 0x06 to the PTY and never opens the
                    // main-window bar while unfocused.
                    input::KeyAction::SearchToggle => {
                        return;
                    }
                    // Hint mode + keyboard copy-mode are main-window + primary-
                    // screen only this release: a detached window swallows the
                    // chord as a clean no-op (never leaks to the PTY, never opens
                    // the mode on the unfocused main window), exactly like
                    // SearchToggle above.
                    input::KeyAction::HintMode | input::KeyAction::CopyMode => {
                        return;
                    }
                    // The palette is main-window only; keep Ctrl+Shift+P's pre-0.18
                    // behavior in a detached window (open Settings) so the chord
                    // isn't silently dropped here. A bare macOS Cmd+P, however, was a
                    // swallowed no-op in today's detached Cmd block (no p/q arm) — keep
                    // it a no-op so the fold is byte-identical (amendment 6).
                    input::KeyAction::OpenPalette => {
                        if !(sup && !ctrl && !alt) {
                            self.toggle_settings_window(event_loop);
                        }
                        return;
                    }
                    // Quit only arises from macOS Cmd+Q, which was a swallowed no-op
                    // in a detached window today — keep it a no-op (amendment 6).
                    input::KeyAction::Quit => {
                        return;
                    }
                    _ => {}
                }
                let Some(dw) = self.detached.get_mut(pos) else { return };
                // Set when the viewport moved under the pointer this event, so
                // the link hover is refreshed AFTER the dw borrow ends.
                let mut viewport_moved = false;
                match action {
                    // Ctrl+Shift+D in a detached window reattaches its tab.
                    input::KeyAction::DetachTab => {
                        self.reattach_tab(pos, event_loop);
                    }
                    // Scrollback paging on THIS window's own terminal (plain
                    // PageUp/Down on the primary screen, Shift+PageUp/Down
                    // always; the alt screen arrives here as Send instead).
                    input::KeyAction::ScrollPageUp => {
                        dw.tab.terminal.scroll_page(true);
                        dw.request_paint();
                        viewport_moved = true;
                    }
                    input::KeyAction::ScrollPageDown => {
                        dw.tab.terminal.scroll_page(false);
                        dw.request_paint();
                        viewport_moved = true;
                    }
                    // OSC 133 prompt jump on THIS window's own terminal (parity
                    // with the main window). No marks / at-the-end = no-op.
                    input::KeyAction::PrevPrompt | input::KeyAction::NextPrompt => {
                        let forward = action == input::KeyAction::NextPrompt;
                        if dw.tab.terminal.jump_prompt(forward) {
                            dw.request_paint();
                            viewport_moved = true;
                        }
                    }
                    input::KeyAction::Copy => {
                        // Same copy-then-clear flow as the main window.
                        let copied = dw
                            .tab
                            .terminal
                            .selection_text()
                            .filter(|t| !t.is_empty());
                        if let Some(text) = copied {
                            clipboard::set(&text);
                            dw.tab.terminal.selection_clear();
                            dw.request_paint();
                        }
                    }
                    input::KeyAction::Paste => {
                        if let Some(text) = clipboard::get() {
                            Self::paste_to_tab(&mut dw.tab, &text);
                        }
                    }
                    // Folded from the old detached Cmd+A block: select all on THIS
                    // window's own terminal.
                    input::KeyAction::SelectAll => {
                        dw.tab.terminal.select_all();
                        dw.request_paint();
                    }
                    input::KeyAction::Send(bytes) => {
                        // Escape closes this window's context menu (if open)
                        // before anything reaches the PTY — mirrors the main window.
                        if bytes == [0x1b] && dw.menu_open.is_some() {
                            dw.menu_open = None;
                            dw.menu_hover = None;
                            dw.menu_rects.clear();
                            dw.request_paint();
                            return;
                        }
                        let _ = dw.tab.writer.write_all(&bytes);
                        let _ = dw.tab.writer.flush();
                        // Any real keystroke jumps this window's view back to the
                        // live bottom, same as the main window's Send arm — else
                        // typing while scrolled up into scrollback goes blind (F30).
                        dw.tab.terminal.scroll_to_bottom();
                        // Caret flash on printable keystrokes — same trigger as
                        // the main window (app.rs ~5010), on THIS window's own
                        // burst clock. Glow is main-window-only (its CaretFx
                        // pass isn't replicated per-window), so gate on the
                        // flash toggle alone.
                        if self.fx.caret_flash_enabled && is_printable_keystroke(&bytes) {
                            dw.caret_anim = Some(std::time::Instant::now());
                        }
                        dw.request_paint();
                    }
                    // Every other action (new/close/nav tab, font, opacity,
                    // panel, scroll, ...) is a main-window-only feature for
                    // this MVP — ignored in a detached window.
                    _ => {}
                }
                if viewport_moved {
                    self.update_detached_link_hover(pos, true);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // IME commit → typed text to THIS window's PTY (no bracketed
                // paste). Mirrors the main window's Ime::Commit arm.
                if !text.is_empty() {
                    let caret_flash_enabled = self.fx.caret_flash_enabled;
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    let _ = dw.tab.writer.write_all(text.as_bytes());
                    let _ = dw.tab.writer.flush();
                    // Snap to the live bottom on commit, same as the Send arm (F30).
                    dw.tab.terminal.scroll_to_bottom();
                    if caret_flash_enabled && is_printable_keystroke(text.as_bytes()) {
                        dw.caret_anim = Some(std::time::Instant::now());
                    }
                    dw.request_paint();
                }
            }
            WindowEvent::Resized(size) => {
                let Some(dw) = self.detached.get_mut(pos) else { return };
                dw.gpu.resize(size.width, size.height);
                dw.text.resize(&dw.gpu);
                dw.chrome_text.resize(&dw.gpu);
                // Same stale-cache rule as the main window: the context menu's
                // hit rects were clamped against the old size — close it.
                dw.menu_open = None;
                dw.menu_hover = None;
                dw.menu_rects.clear();
                // DEBOUNCE the grid+PTY reflow (mirrors the main window's
                // Resized arm): a borderless-edge drag fires many Resized
                // events, and reflowing + a SIGWINCH on each bombards p10k with
                // redraws and scatters its prompt. The surface already resized
                // above so the window tracks the drag live; ONE reflow fires
                // ~250ms after the drag settles (run by `about_to_wait`, which
                // computes the grid from the settled surface + cell size).
                dw.reflow_pending_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                dw.request_paint();
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Same discipline as the main window's arm: press arms THIS
                // window's hover; release sweeps every window (the event is
                // delivered per-focused-window only).
                if link_modifier_held(&self.modifiers) {
                    self.update_detached_link_hover(pos, true);
                } else {
                    self.clear_all_link_hovers();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // &self method — must be read before the dw (self.detached) borrow.
                let status_h = self.status_h();
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let prev = dw.cursor;
                dw.cursor = (position.x, position.y);
                // --- Manual top-bar drag (move the window ourselves) ---
                // global_cursor = outer_position + local cursor; the window's new
                // top-left is global_cursor - the press offset. Doing this manually
                // (instead of win.drag_window()) keeps the RELEASE event in OUR
                // queue, which drop-to-reattach needs. On Wayland outer_position()
                // errs — but then bar_drag is never set (see the press handler),
                // so this arm is unreachable there.
                if let Some((ox, oy)) = dw.bar_drag {
                    if let Ok(outer) = dw.window.outer_position() {
                        let nx = outer.x + (position.x - ox).round() as i32;
                        let ny = outer.y + (position.y - oy).round() as i32;
                        dw.window
                            .set_outer_position(winit::dpi::PhysicalPosition::new(nx, ny));
                    }
                    return;
                }
                let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                let cx = position.x as f32;
                let cy = position.y as f32;
                if dw.menu_open.is_some() {
                    // Menu hover tracking from the cached rects (menu is modal;
                    // no resize/close hover underneath it).
                    let new_hover = dw.menu_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if new_hover != dw.menu_hover {
                        dw.menu_hover = new_hover;
                        dw.request_paint();
                    }
                    return;
                }
                // --- Scrollbar drag continuation (host widget) ---
                // Never emits motion reports: the drag is a host interaction,
                // not app input. Mirrors the main window's dragging_scrollbar.
                if dw.dragging_scrollbar {
                    let rows = dw.tab.terminal.rows();
                    let max = dw.tab.terminal.scroll_max();
                    if let Some(o) = jetty_render::scrollbar_offset_from_cursor(
                        cy, dw.drag_grab_dy, rows, max, h, TABBAR_H, status_h,
                    ) {
                        dw.tab.terminal.scroll_to_offset(o);
                    }
                    // The drag scrolls content under any hovered link — drop
                    // the underline rather than let it ride the wrong text
                    // (mirrors main's apply_scroll_from_cursor refresh).
                    dw.link_hover_cell = None;
                    dw.link_hover = None;
                    dw.request_paint();
                    return;
                }
                // Resize-edge / close-✕ hover feedback is suppressed while a
                // selection drag is in progress (a scrollbar drag returned
                // above) — parity with main: the cursor must not flip to a
                // resize arrow mid-drag.
                if !dw.selecting {
                    // --- Resize-edge cursor feedback (borderless window) ---
                    let zone = resize_zone_at(cx, cy, w, h);
                    if zone != dw.resize_zone {
                        dw.resize_zone = zone;
                        // Link-aware, like main: the Pointer survives leaving a
                        // resize edge while a link is still hovered.
                        dw.window.set_cursor(
                            if zone == ResizeZone::None && dw.link_hover.is_some() {
                                winit::window::CursorIcon::Pointer
                            } else {
                                zone.cursor_icon()
                            },
                        );
                    }
                    // --- Close ✕ hover highlight ---
                    let hover = input::point_in(&jetty_render::detached_close_rect(w), cx, cy);
                    if hover != dw.close_hover {
                        dw.close_hover = hover;
                        dw.request_paint();
                    }
                }
                // --- Text-selection drag continuation / mouse motion reports ---
                // Mirrors the main window (F37/F5): extend a local selection, or —
                // for a mouse-reporting app — emit one motion report per cell change.
                let (cw, ch) = dw.text.cell_size();
                if cw > 0.0 && ch > 0.0 {
                    if dw.selecting {
                        let gy = (cy - TABBAR_H).max(0.0);
                        let (line, col, left_half) = input::cell_at_0_side(
                            cx, gy, cw, ch,
                            dw.tab.terminal.cols(), dw.tab.terminal.rows(),
                        );
                        dw.tab.terminal.selection_update(line, col, left_half);
                        dw.request_paint();
                    } else {
                        let drag = dw.tab.terminal.mouse_drag();
                        let motion = dw.tab.terminal.mouse_motion();
                        let left_held = dw.mouse_grab_press.is_some();
                        if (drag || motion) && (motion || left_held) {
                            let cols_n = dw.tab.terminal.cols();
                            let rows_n = dw.tab.terminal.rows();
                            let new_cell = input::cell_at_clamped(
                                cx, (cy - TABBAR_H).max(0.0), cw, ch, cols_n, rows_n);
                            let prev_cell = input::cell_at_clamped(
                                prev.0 as f32, (prev.1 as f32 - TABBAR_H).max(0.0), cw, ch, cols_n, rows_n);
                            if new_cell != prev_cell {
                                let base = if left_held { 0u8 } else { 3u8 };
                                let sgr = dw.tab.terminal.mouse_sgr();
                                let bytes = input::encode_mouse(
                                    input::MouseEvent::Motion { button: base },
                                    new_cell.0, new_cell.1, sgr,
                                );
                                let _ = dw.tab.writer.write_all(&bytes);
                                let _ = dw.tab.writer.flush();
                            }
                        }
                    }
                }
                // --- Ctrl+hover link tracking (mirrors the main window) ---
                self.update_detached_link_hover(pos, false);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Decide inside the dw borrow, act on `self` afterwards.
                enum Act {
                    None,
                    Reattach,
                    Copy,
                    Paste,
                }
                // &self method — must be read before the dw (self.detached) borrow.
                let status_h = self.status_h();
                let act = {
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    let (cx, cy) = (dw.cursor.0 as f32, dw.cursor.1 as f32);
                    let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                    if dw.menu_open.take().is_some() {
                        // --- Context menu hit-test (consume the click entirely) ---
                        dw.menu_hover = None;
                        let hit = dw.menu_rects.iter().position(|r| {
                            cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                        });
                        dw.menu_rects.clear();
                        dw.request_paint();
                        // Index → DETACHED_MENU_ITEMS order (Reattach/Copy/Paste).
                        match hit {
                            Some(0) => Act::Reattach,
                            Some(1) => Act::Copy,
                            Some(2) => Act::Paste,
                            _ => Act::None,
                        }
                    } else {
                        // --- Resize edges: corners > edges, before the bar. ---
                        let zone = resize_zone_at(cx, cy, w, h);
                        if let Some(dir) = zone.direction() {
                            let _ = dw.window.drag_resize_window(dir);
                            return;
                        }
                        // --- Top bar: close ✕ → reattach; empty bar → move. ---
                        if cy < TABBAR_H {
                            if input::point_in(&jetty_render::detached_close_rect(w), cx, cy) {
                                Act::Reattach
                            } else {
                                // Double-click on the bar toggles maximize (same
                                // ~400ms/5px window as the main strip).
                                let now = std::time::Instant::now();
                                let is_double = matches!(
                                    dw.last_bar_click,
                                    Some((t, px, py))
                                        if now.duration_since(t)
                                            <= std::time::Duration::from_millis(400)
                                            && (cx - px).abs() <= 5.0
                                            && (cy - py).abs() <= 5.0
                                );
                                dw.last_bar_click = Some((now, cx, cy));
                                if is_double {
                                    dw.window.set_maximized(!dw.window.is_maximized());
                                    dw.last_bar_click = None;
                                } else if let Ok(op) = dw.window.outer_position() {
                                    // Manual drag: record the press offset; the
                                    // CursorMoved arm moves the window and the
                                    // Released arm checks drop-to-reattach. Also
                                    // record the GLOBAL press point so the release
                                    // only counts as a reattach after real
                                    // movement (a plain click just raises).
                                    dw.bar_drag = Some(dw.cursor);
                                    dw.bar_drag_start =
                                        Some((op.x as f64 + dw.cursor.0, op.y as f64 + dw.cursor.1));
                                } else {
                                    // Wayland: no readable outer position — fall
                                    // back to the compositor drag. Drop-to-reattach
                                    // is silently unavailable on this path.
                                    let _ = dw.window.drag_window();
                                }
                                return;
                            }
                        } else {
                            // --- Scrollbar thumb drag / track jump ---
                            // Hit-tested BEFORE mouse reports and selection, the
                            // same priority as the main window's press handler.
                            let rows = dw.tab.terminal.rows();
                            let off = dw.tab.terminal.scroll_offset();
                            let max = dw.tab.terminal.scroll_max();
                            // Color is irrelevant for hit-test geometry.
                            let sb = jetty_render::scrollbar_rect_geom(
                                rows, off, max, w, h, TABBAR_H, status_h, [0, 0, 0, 0],
                            );
                            match input::decide_mouse_press(None, sb.as_ref(), cx, cy) {
                                input::MouseAction::StartScrollbarDrag { grab_dy } => {
                                    dw.dragging_scrollbar = true;
                                    dw.drag_grab_dy = grab_dy;
                                    return;
                                }
                                input::MouseAction::ScrollbarTrackJump => {
                                    // Jump the thumb's CENTER to the click, then
                                    // keep dragging from there (mirrors main).
                                    dw.dragging_scrollbar = true;
                                    dw.drag_grab_dy =
                                        sb.as_ref().map(|r| r.h / 2.0).unwrap_or(0.0);
                                    if let Some(o) = jetty_render::scrollbar_offset_from_cursor(
                                        cy, dw.drag_grab_dy, rows, max, h, TABBAR_H, status_h,
                                    ) {
                                        dw.tab.terminal.scroll_to_offset(o);
                                    }
                                    dw.request_paint();
                                    return;
                                }
                                // Panel variants are unreachable (panel = None);
                                // anything else falls through to the grid press.
                                _ => {}
                            }
                            // Grid-area press (F37): forward a mouse report to a
                            // mouse-reporting app (unless Shift overrides), else
                            // begin a local text selection — same as the main window.
                            let (cw, ch) = dw.text.cell_size();
                            if cw > 0.0 && ch > 0.0 {
                                let mouse_mode = dw.tab.terminal.mouse_mode();
                                let shift = self.modifiers.shift_key();
                                let gy = (cy - TABBAR_H).max(0.0);
                                // Ctrl+click on a link opens it and consumes the
                                // click (same precedence as the main window:
                                // Shift still forces selection). Same grid-band
                                // gate as update_detached_link_hover — a click on
                                // the bottom status strip must not open a
                                // clamped bottom-row URL (F13).
                                if link_modifier_held(&self.modifiers)
                                    && !shift
                                    && cy < h as f32 - status_h
                                {
                                    let (line, col, _) = input::cell_at_0_side(
                                        cx, gy, cw, ch,
                                        dw.tab.terminal.cols(), dw.tab.terminal.rows(),
                                    );
                                    if let Some(hit) = dw.tab.terminal.link_at(line, col) {
                                        Self::open_url(&hit.uri);
                                        return;
                                    }
                                }
                                if mouse_mode && !shift {
                                    let (col, row) = input::cell_at_clamped(
                                        cx, gy, cw, ch,
                                        dw.tab.terminal.cols(), dw.tab.terminal.rows(),
                                    );
                                    let sgr = dw.tab.terminal.mouse_sgr();
                                    let bytes = input::encode_mouse(
                                        input::MouseEvent::LeftPress, col, row, sgr,
                                    );
                                    let _ = dw.tab.writer.write_all(&bytes);
                                    let _ = dw.tab.writer.flush();
                                    dw.mouse_grab_press = Some(dw.cursor);
                                } else {
                                    dw.tab.terminal.selection_clear();
                                    let (line, col, left_half) = input::cell_at_0_side(
                                        cx, gy, cw, ch,
                                        dw.tab.terminal.cols(), dw.tab.terminal.rows(),
                                    );
                                    dw.tab.terminal.selection_start(line, col, left_half);
                                    dw.selecting = true;
                                    dw.request_paint();
                                }
                            }
                            return;
                        }
                    }
                };
                match act {
                    Act::Reattach => self.reattach_tab(pos, event_loop),
                    Act::Copy => {
                        if let Some(dw) = self.detached.get_mut(pos) {
                            let copied = dw
                                .tab
                                .terminal
                                .selection_text()
                                .filter(|t| !t.is_empty());
                            if let Some(text) = copied {
                                clipboard::set(&text);
                                dw.tab.terminal.selection_clear();
                                dw.request_paint();
                            }
                        }
                    }
                    Act::Paste => {
                        if let Some(text) = clipboard::get() {
                            if let Some(dw) = self.detached.get_mut(pos) {
                                Self::paste_to_tab(&mut dw.tab, &text);
                            }
                        }
                    }
                    Act::None => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                // Grid-area release (F37): finish a local selection (copy-on-select)
                // or forward the mouse release report — mutually exclusive with a
                // top-bar drag, so handle it first and return. Mirrors the main
                // window's release logic.
                {
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    // A release ending a scrollbar drag is a host-widget
                    // interaction: it must never end a selection, emit a mouse
                    // report, or count as a bar-drag drop (mirrors main's
                    // was_dragging guard).
                    if dw.dragging_scrollbar {
                        dw.dragging_scrollbar = false;
                        return;
                    }
                    if dw.selecting {
                        dw.selecting = false;
                        match dw.tab.terminal.selection_text() {
                            Some(text) if !text.is_empty() => clipboard::set(&text),
                            // Empty drag (plain click) — clear the highlight.
                            _ => dw.tab.terminal.selection_clear(),
                        }
                        dw.request_paint();
                        return;
                    }
                    if let Some((px, py)) = dw.mouse_grab_press.take() {
                        let (cw, ch) = dw.text.cell_size();
                        if cw > 0.0 && ch > 0.0 {
                            let gy = (dw.cursor.1 as f32 - TABBAR_H).max(0.0);
                            let (col, row) = input::cell_at_clamped(
                                dw.cursor.0 as f32, gy, cw, ch,
                                dw.tab.terminal.cols(), dw.tab.terminal.rows());
                            let sgr = dw.tab.terminal.mouse_sgr();
                            let bytes = input::encode_mouse(
                                input::MouseEvent::LeftRelease, col, row, sgr);
                            let _ = dw.tab.writer.write_all(&bytes);
                            let _ = dw.tab.writer.flush();
                        }
                        // A no-Shift DRAG over a mouse-reporting app: the user
                        // was likely trying to select — surface the Shift+drag
                        // hint, same threshold as main. The COOLDOWN is shared
                        // App state (global throttle across all windows); the
                        // visible flag is tagged with THIS window's id so only
                        // the window the drag happened in draws the pill (F4).
                        let moved =
                            ((dw.cursor.0 - px).powi(2) + (dw.cursor.1 - py).powi(2)).sqrt();
                        let now = std::time::Instant::now();
                        let off_cooldown = self.shift_hint_cooldown.is_none_or(|t| now >= t);
                        if moved > 8.0 && off_cooldown {
                            self.shift_hint_until = Some((
                                now + std::time::Duration::from_millis(3500),
                                dw.window.id(),
                            ));
                            self.shift_hint_cooldown =
                                Some(now + std::time::Duration::from_secs(25));
                            dw.request_paint();
                        }
                        return;
                    }
                }
                // End of a manual top-bar drag: if the global cursor landed on the
                // MAIN window's tab-bar strip, reattach; otherwise it was a move.
                let drop_global = {
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    let start = dw.bar_drag_start.take();
                    if dw.bar_drag.take().is_none() {
                        return;
                    }
                    let global = dw
                        .window
                        .outer_position()
                        .ok()
                        .map(|o| (o.x as f64 + dw.cursor.0, o.y as f64 + dw.cursor.1));
                    // Require real movement (>5px, matching the double-click slop)
                    // before a release counts as drop-to-reattach; a sub-threshold
                    // press/release is a plain click that must not tear the tab
                    // down — critical when the detached bar overlaps the main
                    // window's tab-bar band.
                    match (global, start) {
                        (Some(g), Some(s)) => {
                            let moved = ((g.0 - s.0).powi(2) + (g.1 - s.1).powi(2)).sqrt();
                            if moved > 5.0 {
                                Some(g)
                            } else {
                                None
                            }
                        }
                        _ => global,
                    }
                };
                if let Some((gx, gy)) = drop_global {
                    if self.visible {
                        // Convert the detached-window release point and the main
                        // window's outer rect BOTH into scale-independent LOGICAL
                        // points before the band test, so a drop from a
                        // different-DPI monitor lands correctly (F9). At a uniform
                        // scale this is identity, so the X11 path is unchanged.
                        let dw_scale = self
                            .detached
                            .get(pos)
                            .map(|d| d.window.scale_factor())
                            .unwrap_or(1.0);
                        if let (Some(win), Some(gpu)) = (&self.window, &self.gpu) {
                            if let Ok(mp) = win.outer_position() {
                                let main_scale = win.scale_factor();
                                if crate::detached::main_tabbar_contains(
                                    gx / dw_scale,
                                    gy / dw_scale,
                                    mp.x as f64 / main_scale,
                                    mp.y as f64 / main_scale,
                                    gpu.config.width as f64 / main_scale,
                                    gpu.config.height as f64 / main_scale,
                                    TABBAR_H as f64,
                                    self.status_h() as f64,
                                    self.tab_bar_bottom,
                                ) {
                                    self.reattach_tab(pos, event_loop);
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                // Right-click anywhere → Reattach / Copy / Paste context menu.
                let theme = self.current_theme();
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let (cx, cy) = (dw.cursor.0 as f32, dw.cursor.1 as f32);
                dw.menu_open = Some((cx, cy));
                dw.menu_hover = None;
                // Cache the item hit-test rects once (anchor + size fixed for the
                // menu's lifetime), same pattern as the main context menu.
                let items: Vec<(&str, &str)> = crate::detached::DETACHED_MENU_ITEMS
                    .iter()
                    .map(|&l| (l, crate::detached::menu_hint(l)))
                    .collect();
                let menu = jetty_render::build_menu(
                    cx,
                    cy,
                    dw.gpu.config.width,
                    dw.gpu.config.height,
                    None,
                    &theme,
                    dw.chrome_text.cell_size().0,
                    &items,
                    &[],
                );
                dw.menu_rects = menu.item_rects;
                dw.request_paint();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => {
                // Middle-click paste, mirroring the main window's arm with this
                // window's equivalent gates:
                //  - the context menu (this window's only modal) open → swallow;
                //  - only paste over the terminal grid, never the chrome strips;
                //  - when the app grabbed the mouse (mouse_mode) and Shift is not
                //    held, the button belongs to the app — do NOT inject a paste.
                // Pastes the CLIPBOARD selection (same source as main).
                let shift = self.modifiers.shift_key();
                let status_h = self.status_h();
                let Some(dw) = self.detached.get_mut(pos) else { return };
                if dw.menu_open.is_some() {
                    return;
                }
                let cy = dw.cursor.1 as f32;
                let h = dw.gpu.config.height as f32;
                if cy < TABBAR_H || cy >= h - status_h {
                    return;
                }
                if dw.tab.terminal.mouse_mode() && !shift {
                    return;
                }
                if let Some(text) = clipboard::get() {
                    Self::paste_to_tab(&mut dw.tab, &text);
                }
            }
            WindowEvent::Focused(true) if pos < self.detached.len() => {
                // OUR detached window now holds focus: record it and keep the
                // switch flag set so the main window's Focused(false) auto-hide
                // does not fire (the user is still inside Jetty). Mirrors how the
                // Settings window suppresses auto-hide.
                self.last_focused_window = Some(self.detached[pos].window.id());
                self.switching_to_detached = true;
                self.detached[pos].focused = true;
                // Focus implies on-screen: clear any stale occluded flag in case
                // the WM skipped Occluded(false) on restore (F17).
                self.detached[pos].occluded = false;
                // Clear any command-finish urgency raised on THIS detached window
                // (X11 latches it until cleared; parity with the main window).
                self.detached[pos].window.request_user_attention(None);
                // Cancel any scheduled main-window auto-hide: focus moved to one
                // of OUR windows (this arm can arrive AFTER the main FocusOut on
                // X11 — the exact race the deferred hide exists for).
                self.pending_autohide_at = None;
            }
            WindowEvent::Focused(false) if pos < self.detached.len() => {
                // The detached window lost focus. Clear the switch flag so a later
                // main Focused(false) (focus actually left Jetty) is not mistaken
                // for a switch-to-detached and the terminal hides as it should.
                self.switching_to_detached = false;
                self.detached[pos].focused = false;
                if self.last_focused_window == Some(self.detached[pos].window.id()) {
                    self.last_focused_window = None;
                }
                // If focus left mid-interaction, the matching release/click may
                // never arrive — clear the per-window drag/menu state so nothing
                // resumes stuck (same discipline as the main window's auto-hide).
                if let Some(dw) = self.detached.get_mut(pos) {
                    dw.bar_drag = None;
                    dw.bar_drag_start = None;
                    dw.menu_open = None;
                    dw.menu_hover = None;
                    dw.menu_rects.clear();
                    dw.last_bar_click = None;
                    // A selection/press drag can't see its release once focus is
                    // gone — clear it so it doesn't resume stuck (F14).
                    dw.selecting = false;
                    dw.mouse_grab_press = None;
                    dw.dragging_scrollbar = false;
                    // A link underline can't clear itself while unfocused (the
                    // modifier release is delivered elsewhere) — drop it now.
                    dw.link_hover_cell = None;
                    if dw.link_hover.take().is_some() {
                        dw.window.set_cursor(dw.resize_zone.cursor_icon());
                        dw.request_paint();
                    }
                }
                // F14: focus is leaving THIS detached window. If it departs to a
                // foreign app (not another JeTTY window), the main dropdown must
                // auto-hide too — SCHEDULE the same deferred hide the main
                // window's Focused(false) uses. Any JeTTY window regaining focus
                // within the grace cancels it (its Focused(true) clears
                // pending_autohide_at). Without this, focus leaving JeTTY via a
                // detached/Settings sibling left the terminal on top forever.
                if self.focus_autohide
                    && self.visible
                    && self.summon_anim.is_none()
                    && !self.summon_pending
                {
                    self.pending_autohide_at = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(AUTOHIDE_GRACE_MS),
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wheel scrolling in a detached window (the main window handles
                // this at the sibling arm). Accumulate fractional deltas exactly
                // like the main window, then either forward wheel mouse reports
                // (mouse-mode app, not over the scrollbar) or scroll THIS
                // window's own scrollback.
                let delta_lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                    MouseScrollDelta::PixelDelta(p) => {
                        const CELL_H: f64 = 20.0;
                        (p.y / CELL_H) as f32
                    }
                };
                let status_h = self.status_h();
                let Some(dw) = self.detached.get_mut(pos) else { return };
                // Use THIS window's own accumulator so a leftover fraction never
                // bleeds across windows (F26) — the shared self.scroll_accum did.
                let lines = dw.scroll_accum.add(delta_lines);
                if lines == 0 {
                    return;
                }
                let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                // Wheeling over the scrollbar always scrolls the host scrollback
                // even in mouse-mode apps (mirrors the main window).
                let over_scrollbar = {
                    let rows = dw.tab.terminal.rows();
                    let off = dw.tab.terminal.scroll_offset();
                    let max = dw.tab.terminal.scroll_max();
                    jetty_render::scrollbar_rect_geom(rows, off, max, w, h, TABBAR_H, status_h, [0, 0, 0, 0])
                        .map(|r| {
                            let cx = dw.cursor.0 as f32;
                            cx >= r.x && cx <= r.x + r.w
                        })
                        .unwrap_or(false)
                };
                let (cw, ch) = dw.text.cell_size();
                if dw.tab.terminal.mouse_mode() && !over_scrollbar {
                    let event = if lines > 0 {
                        input::MouseEvent::WheelUp
                    } else {
                        input::MouseEvent::WheelDown
                    };
                    let notches = ((lines.abs() + 2) / 3).clamp(1, 8);
                    if cw > 0.0 && ch > 0.0 {
                        let gy = (dw.cursor.1 as f32 - TABBAR_H).max(0.0);
                        // 1-based cell coords: the encoders are 1-based, so the
                        // old 0-based col/row named the cell one row up / one col
                        // left of the pointer (F12). cell_at_clamped adds the +1,
                        // matching the main window's cursor_cell() path.
                        let (col, row) = input::cell_at_clamped(
                            dw.cursor.0 as f32,
                            gy,
                            cw,
                            ch,
                            dw.tab.terminal.cols(),
                            dw.tab.terminal.rows(),
                        );
                        let sgr = dw.tab.terminal.mouse_sgr();
                        for _ in 0..notches {
                            let bytes = input::encode_mouse(event, col, row, sgr);
                            let _ = dw.tab.writer.write_all(&bytes);
                        }
                        let _ = dw.tab.writer.flush();
                    }
                } else if !over_scrollbar
                    && dw.tab.terminal.alt_screen()
                    && dw.tab.terminal.alternate_scroll()
                {
                    // ALTERNATE_SCROLL: wheel → Up/Down arrows on the alt screen
                    // so less/man/git log scroll here too (F3), mirroring main.
                    let app_cursor = dw.tab.terminal.app_cursor_keys();
                    let seq = input::arrow_scroll_bytes(lines > 0, app_cursor);
                    let steps = (lines.unsigned_abs() as usize).clamp(1, 12);
                    for _ in 0..steps {
                        let _ = dw.tab.writer.write_all(&seq);
                    }
                    let _ = dw.tab.writer.flush();
                } else {
                    dw.tab.terminal.scroll_lines(lines);
                    dw.request_paint();
                    // Viewport moved under a stationary pointer (mirrors main).
                    self.update_detached_link_hover(pos, true);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Moved to a different-DPI monitor: re-scale the fonts in place
                // (no fontconfig rescan) and arm the debounced reflow — the
                // surface resize + grid reflow follow in the Resized event
                // (mirrors the main window's arm). Without this a detached window
                // keeps its creation-time physical font size and mis-scales.
                let scale = scale_factor as f32;
                let font_logical = self.font_logical;
                let ui_font_logical = self.ui_font_logical;
                let Some(dw) = self.detached.get_mut(pos) else { return };
                dw.text.set_font_size(font_logical * scale);
                dw.chrome_text.set_font_size(ui_font_logical * scale);
                dw.reflow_pending_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(120));
                dw.request_paint();
            }
            _ => {}
        }
    }

    /// Render a detached window: its single tab's grid plus the window chrome —
    /// a top bar (title pill + close ✕, TABBAR_H tall), the bottom status strip
    /// (perf HUD) when `show_perf_hud`, and the Reattach/Copy/Paste context menu
    /// when open. Mirrors the main window's terminal draw passes from the
    /// `RedrawRequested` arm of `window_event` using the detached window's OWN
    /// `gpu`/`text`/`chrome_text`/`quad`, and applies the SAME final effects:
    /// the rounded-corner mask (all four corners — a detached window is never
    /// top-flush), the transparent theme-bg clear, the caret flash, and the CRT
    /// post-pass (which owns the rounded corners while active, exactly like the
    /// main window). Summon/Tier-B reveals stay main-window-only.
    fn render_detached_window(&mut self, pos: usize) {
        let Some(dw) = self.detached.get_mut(pos) else { return };

        // Drain this tab's PTY output into its terminal before snapshotting.
        // Detached tabs are no longer in `self.tabs`, so the main `drain_pty`
        // loop never sees them — without this the detached grid would stay
        // frozen at whatever it looked like the instant it was detached. Uses
        // the shared, byte-budgeted `drain_one_tab` (same flood protection as
        // the main window); any capped remainder is drained by the Wakes the
        // reader queued, which re-request this window's redraw.
        let mut vt_read: u64 = 0;
        Self::drain_one_tab(&mut dw.tab, &mut vt_read);
        // OSC titles: keep the OS window title in sync (no-op unless changed).
        dw.sync_os_title();

        // Snapshot + theme + chrome inputs are read before the mutable
        // gpu/text/quad borrow below (same pattern as the main RedrawRequested).
        let snap = dw.tab.terminal.snapshot();
        let title = dw.tab.title.clone();
        let close_hover = dw.close_hover;
        let menu_open = dw.menu_open;
        let menu_hover = dw.menu_hover;
        // Ctrl+hover link underline spans, snapshotted before the wide
        // gpu/text/quad borrows below (same pattern as the main window).
        let link_spans: Option<Vec<(usize, usize, usize)>> =
            if link_modifier_held(&self.modifiers) {
                dw.link_hover.as_ref().map(|h| h.spans.clone())
            } else {
                None
            };
        let theme = self.current_theme();
        let status_h = self.status_h();
        // Same global HUD string the main status bar shows (built on the main
        // window's frames). Reading the cache never wakes anything.
        let perf_label = self.perf_label.clone();
        // Shift+drag hint toast — the shared timer is tagged with the window
        // the drag happened in; captured here (Copy) and compared against
        // THIS window's id after the dw borrow below, so only that window
        // draws the pill (F4).
        let shift_hint_until = self.shift_hint_until;
        // Effects inputs, captured before the mutable dw borrow below — the
        // SAME settings the main window renders with (visual parity).
        let corner_radius = self.corner_radius;
        let fx = self.fx.clone();
        let crt_time = (self.crt_clock.elapsed().as_secs_f64() % CRT_PHASE_WRAP) as f32;
        let crt_anim_live = fx.crt_anim_live();

        let Some(dw) = self.detached.get_mut(pos) else { return };
        let shift_hint_show =
            shift_hint_live_in(shift_hint_until, dw.window.id(), std::time::Instant::now());
        // Caret flash progress on THIS window's burst clock: t∈[0,1], expired at
        // 1.0 — mirrors the main window's caret_t handling (app.rs ~5214).
        let caret_t = dw.caret_anim.map(|s| {
            (s.elapsed().as_secs_f32() / (fx.caret_flash_ms / 1000.0)).min(1.0)
        });
        if caret_t == Some(1.0) {
            dw.caret_anim = None;
        }
        let caret_t_for_flash = if fx.caret_flash_enabled { caret_t } else { None };
        // Window focus drives the unfocused-hollow cursor (captured before the
        // gpu/text/quad borrows below).
        let focused = dw.focused;
        // OSC 133 failed-command marker rows for THIS window's tab (captured
        // before the mutable dw borrows below; parity with the main window).
        let failed_rows = dw.tab.terminal.failed_prompt_rows();
        // Visible inline (sixel) images + decoded RGBA (owned; Arc clone), captured
        // before the mutable dw borrows below — parity with the main window.
        let images: Vec<(jetty_core::VisibleImage, std::sync::Arc<jetty_core::SixelImage>)> = {
            let term = &dw.tab.terminal;
            term.visible_images()
                .into_iter()
                .filter_map(|vi| term.image_rgba(vi.id).map(|img| (vi, img)))
                .collect()
        };
        // Corner radius in physical px (HiDPI-correct, same scaling as main).
        let scale = dw.window.scale_factor() as f32;
        let corner_radius_px = corner_radius * scale;
        // CRT routing: when enabled, the whole scene renders into this window's
        // offscreen texture and the CRT pass samples it onto the surface — the
        // exact main-window flow (no Tier-B summons here, so no bypass case).
        // Re-allocate the offscreen lazily when stale (same check as main).
        let crt_active = fx.crt_enabled;
        if crt_active
            && (dw.offscreen.0.width() != dw.gpu.config.width
                || dw.offscreen.0.height() != dw.gpu.config.height)
        {
            dw.offscreen = Self::make_offscreen(&dw.gpu);
        }
        let gpu = &mut dw.gpu;
        let text = &mut dw.text;
        let chrome_text = &mut dw.chrome_text;
        let quad = &mut dw.quad;
        let corner_mask = &dw.corner_mask;
        let crt = &dw.crt;
        let offscreen = &dw.offscreen;
        let image_layer = &mut dw.image_layer;

        let Some((frame, view)) = gpu.acquire_frame() else { return };
        let width = gpu.config.width;
        let height = gpu.config.height;
        // Scene target: the offscreen when CRT is on, else the surface directly
        // (byte-identical to the pre-CRT hot path).
        let scene_view: &wgpu::TextureView = if crt_active { &offscreen.1 } else { &view };

        // The grid sits below the top bar (and above the status strip).
        let grid_top = TABBAR_H;
        let chrome_char_w = chrome_text.cell_size().0;
        let grid_bottom_px = (height as f32 - status_h).max(0.0);

        // Passes 1–4 via the shared render core (v0.23 Task 8). The detached
        // title bar (Pass 3) is the mid-scene chrome, injected between the glyph
        // and scrollbar/cursor passes exactly as before. `slide_y = 0.0`,
        // `copy_mode = None/false`, and `search_hits = &[]`, so a detached
        // window gains NO dropdown slide, NO copy-mode cursor, and no search
        // tint (BLOCKING 5) — its render is byte-identical to the pre-refactor
        // body. The main-only caret GLOW / summon reveals live only in the main
        // caller's tail and never reach here.
        let scene = GridScene {
            snap: &snap,
            theme: &theme,
            grid_top,
            slide_y: 0.0,
            grid_bottom: grid_bottom_px,
            status_h,
            scale,
            search_hits: &[],
            failed_rows: &failed_rows,
            link_spans: link_spans.as_ref(),
            images: &images,
            focused,
            caret_t_for_flash,
            caret_flash_color: fx.caret_flash_color,
            copy_mode_active: false,
            copy_mode_ui: None,
        };
        render_grid_scene(
            gpu,
            text,
            quad,
            image_layer,
            scene_view,
            width,
            height,
            &scene,
            // Pass 3: the top bar (title pill + close ✕) over the grid.
            |quad, device, queue, view, w, h| {
                let bar = jetty_render::build_detached_bar(w, &title, &theme, close_hover, chrome_char_w);
                quad.render(device, queue, view, w, h, &bar.quads);
                if !bar.labels.is_empty() {
                    let _ = chrome_text.render_overlays(device, queue, view, w, h, &bar.labels);
                }
                if !bar.title_labels.is_empty() {
                    // Title in the platform's proportional sans, like main tab titles.
                    let _ = chrome_text.render_overlays_sans(device, queue, view, w, h, &bar.title_labels);
                }
            },
        );
        // Pass 5: bottom STATUS strip (perf HUD) when enabled — the same slim
        // theme-derived strip as the main window; it may show the same global
        // HUD string (built by the main window's frames).
        if status_h > 0.0 {
            let sy = height as f32 - status_h;
            let tb = theme.bg;
            let tf = theme.fg;
            let nl = |t: f32| -> [u8; 4] {
                [
                    (tb[0] as f32 + (tf[0] as f32 - tb[0] as f32) * t) as u8,
                    (tb[1] as f32 + (tf[1] as f32 - tb[1] as f32) * t) as u8,
                    (tb[2] as f32 + (tf[2] as f32 - tb[2] as f32) * t) as u8,
                    255,
                ]
            };
            let strip = jetty_render::Rect {
                x: 0.0, y: sy, w: width as f32, h: status_h,
                color: nl(0.05), ..Default::default()
            };
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[strip]);
            if let Some(perf) = perf_label.as_deref() {
                let perf_w = perf.chars().count() as f32 * chrome_char_w;
                let px = (width as f32 - perf_w - 12.0).max(8.0);
                let dim = nl(0.5);
                let py = sy + (status_h - 16.0) / 2.0;
                let _ = chrome_text.render_overlays(
                    &gpu.device, &gpu.queue, scene_view, width, height,
                    &[(perf.to_string(), px, py, [dim[0], dim[1], dim[2]])],
                );
            }
        }
        // Pass 5b: Shift+drag hint toast — the main window's Pass 4c pill,
        // byte-for-byte, positioned above the status strip (the detached bar is
        // always on top, so no bottom-bar / slide offset terms apply). Drawn
        // only on frames where the 3.5s flag is live — no steady-state cost.
        if shift_hint_show {
            let hint = "Hold Shift while dragging to select text";
            let tw = hint.chars().count() as f32 * chrome_char_w;
            let pad = 14.0;
            let pill_w = tw + pad * 2.0;
            let pill_h = 26.0;
            let pill_x = ((width as f32 - pill_w) / 2.0).max(0.0);
            let pill_y = (height as f32 - status_h - 14.0 - pill_h).max(0.0);
            let c = theme.cursor;
            let pill = jetty_render::Rect::rounded(
                pill_x, pill_y, pill_w, pill_h, [c[0], c[1], c[2], 235], pill_h / 2.0,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill]);
            let ty = pill_y + (pill_h - 16.0) / 2.0;
            let _ = chrome_text.render_overlays(
                &gpu.device, &gpu.queue, scene_view, width, height,
                &[(hint.to_string(), pill_x + pad, ty, [20, 20, 20])],
            );
        }
        // Pass 6: the Reattach/Copy/Paste context menu on top of everything.
        if let Some((mx, my)) = menu_open {
            let items: Vec<(&str, &str)> = crate::detached::DETACHED_MENU_ITEMS
                .iter()
                .map(|&l| (l, crate::detached::menu_hint(l)))
                .collect();
            let menu = jetty_render::build_menu(
                mx, my, width, height, menu_hover, &theme, chrome_char_w, &items, &[],
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &menu.quads);
            if !menu.labels.is_empty() {
                let _ = chrome_text.render_overlays(
                    &gpu.device, &gpu.queue, scene_view, width, height, &menu.labels,
                );
            }
        }
        // Final pass: round the window corners — the SAME mask pass the main
        // window runs, at the SAME configured radius. A detached window is a
        // free-floating window, so ALL FOUR corners round (the main window's
        // Dropdown top-square nuance never applies here). Skipped while CRT is
        // active: the CRT pass owns the rounded corners then (exactly like the
        // main window's mask/CRT interplay).
        if !crt_active {
            let (r_tl, r_tr, r_bl, r_br) = crate::detached::corner_radii(corner_radius_px);
            corner_mask.apply(
                &gpu.device, &gpu.queue, scene_view, width, height, r_tl, r_tr, r_bl, r_br,
            );
        }
        // CRT post-pass: sample the offscreen scene onto the surface with the
        // same parameters (and free-running clock) as the main window. The CRT
        // uniform carries the corner radius, so corners stay rounded under CRT.
        if crt_active {
            crt.apply(
                &gpu.device,
                &gpu.queue,
                &view,
                &offscreen.1,
                width,
                height,
                &jetty_render::CrtUniform {
                    resolution: [width as f32, height as f32],
                    curvature: fx.crt_curvature,
                    scanline: fx.crt_scanline,
                    mask: fx.crt_mask,
                    bloom: fx.crt_bloom,
                    chromatic: fx.crt_chromatic,
                    vignette: fx.crt_vignette,
                    tint: [
                        fx.crt_scanline_tint[0],
                        fx.crt_scanline_tint[1],
                        fx.crt_scanline_tint[2],
                        0.0,
                    ],
                    corner_radius: corner_radius_px,
                    time: crt_time,
                    flags: (if fx.crt_animate_roll { jetty_render::CRT_FLAG_ROLL } else { 0 })
                        | (if fx.crt_flicker { jetty_render::CRT_FLAG_FLICKER } else { 0 })
                        | (if fx.crt_jitter { jetty_render::CRT_FLAG_JITTER } else { 0 }),
                    // A detached window is free-floating (never top-flush), so
                    // all four corners round — same as its corner mask.
                    corner_radius_top: corner_radius_px,
                },
            );
        }
        frame.present();
        // Missed-paint proof counter (JETTY_FRAME_LOG only; see the field docs).
        // `self.frames_presented`/`self.frame_log` are fields disjoint from the
        // live `dw` borrow of `self.detached`, so this is a plain field bump.
        if self.frame_log {
            self.frames_presented += 1;
            eprintln!("JETTY_FRAME {} detached", self.frames_presented);
        }
        // Self-drive the next frame ONLY while the caret flash is mid-burst, an
        // animated CRT sub-effect is on, or the Shift+drag hint toast is still
        // showing (so it repaints away on expiry instead of freezing on screen)
        // — the same damage-driven gates as the main window (its RedrawRequested
        // has the identical hint_live term). Idle returns to 0-CPU once all
        // clear. Also gated on the window not being occluded/minimized so a
        // hidden detached window returns to true idle instead of self-driving
        // forever (F8).
        if !dw.occluded && (dw.caret_anim.is_some() || crt_anim_live || shift_hint_show) {
            dw.window.request_redraw();
        }
    }

    /// Apply a panel `MouseAction` decoded in the settings window. Updates shared
    /// state AND the live main terminal (theme/font/opacity), then requests a
    /// redraw of BOTH windows so each reflects the change immediately.
    fn handle_settings_action(
        &mut self,
        action: input::MouseAction,
        geom: &jetty_render::PanelGeom,
    ) {
        let cx = self.settings_cursor.0 as f32;
        // Any settings interaction that isn't part of the theme picker collapses
        // the dropdown (click-outside-to-close behavior).
        if !matches!(
            action,
            input::MouseAction::ToggleThemeDropdown
                | input::MouseAction::ThemeScrollUp
                | input::MouseAction::ThemeScrollDown
                | input::MouseAction::SetTheme(_)
        ) {
            self.theme_dropdown_open = false;
        }
        match action {
            input::MouseAction::StartSliderDrag => {
                self.dragging_slider = true;
                self.opacity = self.opacity_from_cursor(cx, &geom.slider_track);
                self.apply_theme();
            }
            input::MouseAction::StartRadiusDrag => {
                self.dragging_radius = true;
                self.corner_radius = self.radius_from_cursor(cx, &geom.radius_track);
            }
            input::MouseAction::SetTheme(i) => {
                if i < jetty_core::theme_count() {
                    self.theme_idx = i;
                    self.apply_theme();
                }
                self.theme_dropdown_open = false;
            }
            input::MouseAction::ToggleThemeDropdown => {
                self.theme_dropdown_open = !self.theme_dropdown_open;
                if self.theme_dropdown_open {
                    // Open with the active theme scrolled into view (centered-ish).
                    let start = self.theme_idx.saturating_sub(MAX_THEME_ROWS / 2);
                    self.theme_scroll_offset = start.min(self.max_theme_scroll());
                }
            }
            input::MouseAction::ThemeScrollUp => {
                self.theme_scroll_offset = self.theme_scroll_offset.saturating_sub(1);
            }
            input::MouseAction::ThemeScrollDown => {
                self.theme_scroll_offset =
                    (self.theme_scroll_offset + 1).min(self.max_theme_scroll());
            }
            input::MouseAction::FontMinus => {
                self.set_font_size(self.font_logical - 1.0);
            }
            input::MouseAction::FontPlus => {
                self.set_font_size(self.font_logical + 1.0);
            }
            input::MouseAction::FontReset => {
                self.set_font_size(FONT_LOGICAL_DEFAULT);
            }
            input::MouseAction::SetFont(idx) => {
                if let Some(name) = self.font_families.get(idx) {
                    let name = name.clone();
                    self.set_font_family(name);
                }
            }
            input::MouseAction::FontScrollUp => {
                self.font_scroll_offset = self.font_scroll_offset.saturating_sub(1);
            }
            input::MouseAction::FontScrollDown => {
                const MAX_FONT_ROWS: usize = 5;
                let max_offset = self.font_families.len().saturating_sub(MAX_FONT_ROWS);
                self.font_scroll_offset = (self.font_scroll_offset + 1).min(max_offset);
            }
            input::MouseAction::UiFontMinus => {
                self.set_ui_font_size(self.ui_font_logical - 1.0);
            }
            input::MouseAction::UiFontPlus => {
                self.set_ui_font_size(self.ui_font_logical + 1.0);
            }
            input::MouseAction::UiFontReset => {
                self.set_ui_font_size(UI_FONT_LOGICAL_DEFAULT);
            }
            input::MouseAction::SetUiFont(idx) => {
                // Index 0 is the synthetic "System Sans (default)" row → "".
                if idx == 0 {
                    self.set_ui_font_family(String::new());
                } else if let Some(name) = self.ui_font_families.get(idx) {
                    let name = name.clone();
                    self.set_ui_font_family(name);
                }
            }
            input::MouseAction::UiFontScrollUp => {
                self.ui_font_scroll_offset = self.ui_font_scroll_offset.saturating_sub(1);
            }
            input::MouseAction::UiFontScrollDown => {
                // 4-row visible cap (MAX_UI_FONT_ROWS in panel.rs).
                const MAX_UI_FONT_ROWS: usize = 4;
                let max_offset = self.ui_font_families.len().saturating_sub(MAX_UI_FONT_ROWS);
                self.ui_font_scroll_offset = (self.ui_font_scroll_offset + 1).min(max_offset);
            }
            input::MouseAction::SummonPrev => {
                self.set_summon_effect(self.summon_effect.cycle(false));
            }
            input::MouseAction::SummonNext => {
                self.set_summon_effect(self.summon_effect.cycle(true));
            }
            input::MouseAction::WinModePrev => {
                self.set_window_mode(self.window_mode.cycle(false));
            }
            input::MouseAction::WinModeNext => {
                self.set_window_mode(self.window_mode.cycle(true));
            }
            input::MouseAction::TabBarPrev | input::MouseAction::TabBarNext => {
                // Only two positions, so prev and next both toggle.
                self.set_tab_bar_bottom(!self.tab_bar_bottom);
            }
            input::MouseAction::ScrollbackPrev => {
                let v = cycle_scrollback(self.scrollback_lines, false);
                self.set_scrollback_lines(v);
            }
            input::MouseAction::ScrollbackNext => {
                let v = cycle_scrollback(self.scrollback_lines, true);
                self.set_scrollback_lines(v);
            }
            input::MouseAction::CycleShellPrev => {
                self.cycle_shell(false);
            }
            input::MouseAction::CycleShellNext => {
                self.cycle_shell(true);
            }
            // RUN & NOTIFY toggles/cycler (Shell tab, v0.15). Each flips a mirror
            // field; the shared persist()/redraw below flushes and repaints.
            input::MouseAction::ToggleNotifyOnFinish => {
                self.notify_on_finish = !self.notify_on_finish;
            }
            input::MouseAction::ToggleNotifyOnlyFailure => {
                self.notify_only_on_failure = !self.notify_only_on_failure;
            }
            input::MouseAction::NotifyDurPrev => {
                self.notify_min_seconds = cycle_notify_min(self.notify_min_seconds, false);
            }
            input::MouseAction::NotifyDurNext => {
                self.notify_min_seconds = cycle_notify_min(self.notify_min_seconds, true);
            }
            input::MouseAction::ToggleAutoSummon => {
                self.auto_summon_on_finish = !self.auto_summon_on_finish;
            }
            input::MouseAction::StartDropdownDrag => {
                // No-op in Center mode (the slider is grayed/disabled there).
                if self.window_mode == WindowMode::Dropdown {
                    self.dragging_dropdown = true;
                    self.dropdown_height_pct =
                        self.dropdown_pct_from_cursor(cx, &geom.dropdown_track);
                }
            }
            input::MouseAction::StartDropdownWidthDrag => {
                // No-op in Center mode (the slider is grayed/disabled there).
                if self.window_mode == WindowMode::Dropdown {
                    self.dragging_dropdown_width = true;
                    self.dropdown_width_pct =
                        self.dropdown_width_pct_from_cursor(cx, &geom.dropdown_width_track);
                }
            }
            // Effects tab toggles: flip the corresponding bool in self.fx.
            input::MouseAction::ToggleCrt => {
                self.fx.crt_enabled = !self.fx.crt_enabled;
            }
            input::MouseAction::ToggleCrtRoll => {
                self.fx.crt_animate_roll = !self.fx.crt_animate_roll;
            }
            input::MouseAction::ToggleCrtFlicker => {
                self.fx.crt_flicker = !self.fx.crt_flicker;
            }
            input::MouseAction::ToggleCrtJitter => {
                self.fx.crt_jitter = !self.fx.crt_jitter;
            }
            input::MouseAction::ToggleCaretFlash => {
                self.fx.caret_flash_enabled = !self.fx.caret_flash_enabled;
            }
            input::MouseAction::ToggleCaretGlow => {
                self.fx.caret_glow_enabled = !self.fx.caret_glow_enabled;
            }
            // Effects tab sliders: mark the active drag and apply initial value.
            // The CursorMoved handler updates the value on every subsequent move;
            // MouseInput::Released clears active_fx_drag and persists the final value.
            input::MouseAction::StartCrtCurvatureDrag => {
                self.active_fx_drag = Some(FxSlider::CrtCurvature);
                self.fx.crt_curvature = self.fx_frac_from_cursor(cx, &geom.crt_curvature_track);
            }
            input::MouseAction::StartScanlineDrag => {
                self.active_fx_drag = Some(FxSlider::CrtScanline);
                self.fx.crt_scanline = self.fx_frac_from_cursor(cx, &geom.crt_scanline_track);
            }
            input::MouseAction::StartMaskDrag => {
                self.active_fx_drag = Some(FxSlider::CrtMask);
                self.fx.crt_mask = self.fx_frac_from_cursor(cx, &geom.crt_mask_track);
            }
            input::MouseAction::StartBloomDrag => {
                self.active_fx_drag = Some(FxSlider::CrtBloom);
                self.fx.crt_bloom = self.fx_frac_from_cursor(cx, &geom.crt_bloom_track);
            }
            input::MouseAction::StartChromaticDrag => {
                self.active_fx_drag = Some(FxSlider::CrtChromatic);
                self.fx.crt_chromatic = self.fx_frac_from_cursor(cx, &geom.crt_chromatic_track);
            }
            input::MouseAction::StartVignetteDrag => {
                self.active_fx_drag = Some(FxSlider::CrtVignette);
                self.fx.crt_vignette = self.fx_frac_from_cursor(cx, &geom.crt_vignette_track);
            }
            input::MouseAction::StartCaretDurDrag => {
                self.active_fx_drag = Some(FxSlider::CaretDur);
                let frac = self.fx_frac_from_cursor(cx, &geom.caret_dur_track);
                self.fx.caret_flash_ms = 60.0 + frac * 340.0;
            }
            input::MouseAction::StartTintRDrag => {
                self.active_fx_drag = Some(FxSlider::TintR);
                self.fx.crt_scanline_tint[0] = self.fx_frac_from_cursor(cx, &geom.crt_tint_r_track);
            }
            input::MouseAction::StartTintGDrag => {
                self.active_fx_drag = Some(FxSlider::TintG);
                self.fx.crt_scanline_tint[1] = self.fx_frac_from_cursor(cx, &geom.crt_tint_g_track);
            }
            input::MouseAction::StartTintBDrag => {
                self.active_fx_drag = Some(FxSlider::TintB);
                self.fx.crt_scanline_tint[2] = self.fx_frac_from_cursor(cx, &geom.crt_tint_b_track);
            }
            input::MouseAction::StartCaretColorRDrag => {
                self.active_fx_drag = Some(FxSlider::CaretColorR);
                self.fx.caret_flash_color[0] = self.fx_frac_from_cursor(cx, &geom.caret_color_r_track);
            }
            input::MouseAction::StartCaretColorGDrag => {
                self.active_fx_drag = Some(FxSlider::CaretColorG);
                self.fx.caret_flash_color[1] = self.fx_frac_from_cursor(cx, &geom.caret_color_g_track);
            }
            input::MouseAction::StartCaretColorBDrag => {
                self.active_fx_drag = Some(FxSlider::CaretColorB);
                self.fx.caret_flash_color[2] = self.fx_frac_from_cursor(cx, &geom.caret_color_b_track);
            }
            input::MouseAction::SetSettingsTab(i) => {
                // Session-only tab switch: change the active tab and redraw the
                // settings window. Not persisted (resets to Look on restart).
                self.settings_tab = i.min(4);
                self.request_settings_paint();
                // Nothing to persist for a tab switch; return early so we don't
                // write config or redraw the main terminal needlessly.
                return;
            }
            input::MouseAction::ToggleFocusAutoHide => {
                self.focus_autohide = !self.focus_autohide;
            }
            input::MouseAction::ToggleLaunchAtLogin => {
                self.launch_at_login = !self.launch_at_login;
                // Write/remove the XDG autostart .desktop file to match. The file's
                // existence is the source of truth; persist() (below) mirrors it.
                set_launch_at_login(self.launch_at_login);
            }
            // The OS title bar moves the window now; in-panel drag/consume are no-ops.
            input::MouseAction::StartDialogDrag
            | input::MouseAction::ConsumePanel
            | input::MouseAction::StartScrollbarDrag { .. }
            | input::MouseAction::ScrollbarTrackJump
            | input::MouseAction::None => {}
        }
        // Persist the new setting. Drag-in-progress (slider/radius) keeps writing
        // on release too, but a write here is cheap and captures theme/font picks
        // that don't go through a release event.
        self.persist();
        // Redraw both windows: settings shows the updated control, main shows the
        // new theme/font/opacity live. set_font_size/set_font_family already redraw
        // the main window, but an extra request is harmless and keeps this simple.
        self.request_main_paint();
        self.request_settings_paint();
        // Detached windows share the same theme/opacity/radius/CRT settings —
        // repaint them too so every surface reflects the change immediately
        // (one damage-driven request each; no polling).
        for dw in &self.detached {
            dw.request_paint();
        }
    }

    /// Handle a `WindowEvent` that belongs to the settings window. Hit-testing
    /// uses the settings window's own coordinate space (`settings_cursor`).
    fn settings_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.close_settings_window();
                self.request_main_paint();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.settings_gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.settings_gpu, &mut self.settings_text) {
                    text.resize(gpu);
                }
                self.request_settings_paint();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let scale = scale_factor as f32;
                // CAPPED UI size ([13,17] * scale): the panel body text stays within
                // the fixed window. Re-scale in place (reusing the FontSystem) so a
                // settings-window DPI change doesn't rescan fontconfig (~20ms) on
                // the main thread.
                if let Some(t) = self.settings_text.as_mut() {
                    let capped = self.ui_font_logical.clamp(PANEL_TEXT_MIN, PANEL_TEXT_MAX);
                    t.set_font_size(capped * scale);
                }
                // The specimen layer tracks the TRUE size (so its "Aa" stays honest).
                if let Some(sp) = self.settings_specimen_text.as_mut() {
                    sp.set_font_size(self.ui_font_logical * scale);
                }
                self.request_settings_paint();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.settings_cursor = (position.x, position.y);
                // Continue an opacity-, radius-, dropdown-height/-width, or Effects slider drag.
                if self.dragging_slider || self.dragging_radius || self.dragging_dropdown || self.dragging_dropdown_width || self.active_fx_drag.is_some() {
                    if let Some(gpu) = &self.settings_gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        let pv = self.settings_panel_view(w, h);
                        let cx = self.settings_cursor.0 as f32;
                        if self.dragging_slider {
                            self.opacity = self.opacity_from_cursor(cx, &pv.geom.slider_track);
                            self.apply_theme();
                        }
                        if self.dragging_radius {
                            self.corner_radius = self.radius_from_cursor(cx, &pv.geom.radius_track);
                        }
                        if self.dragging_dropdown {
                            self.dropdown_height_pct =
                                self.dropdown_pct_from_cursor(cx, &pv.geom.dropdown_track);
                        }
                        if self.dragging_dropdown_width {
                            self.dropdown_width_pct =
                                self.dropdown_width_pct_from_cursor(cx, &pv.geom.dropdown_width_track);
                        }
                        if let Some(fx_slider) = self.active_fx_drag {
                            match fx_slider {
                                FxSlider::CrtCurvature => self.fx.crt_curvature = self.fx_frac_from_cursor(cx, &pv.geom.crt_curvature_track),
                                FxSlider::CrtScanline  => self.fx.crt_scanline  = self.fx_frac_from_cursor(cx, &pv.geom.crt_scanline_track),
                                FxSlider::CrtMask      => self.fx.crt_mask      = self.fx_frac_from_cursor(cx, &pv.geom.crt_mask_track),
                                FxSlider::CrtBloom     => self.fx.crt_bloom     = self.fx_frac_from_cursor(cx, &pv.geom.crt_bloom_track),
                                FxSlider::CrtChromatic => self.fx.crt_chromatic = self.fx_frac_from_cursor(cx, &pv.geom.crt_chromatic_track),
                                FxSlider::CrtVignette  => self.fx.crt_vignette  = self.fx_frac_from_cursor(cx, &pv.geom.crt_vignette_track),
                                FxSlider::CaretDur => {
                                    let frac = self.fx_frac_from_cursor(cx, &pv.geom.caret_dur_track);
                                    self.fx.caret_flash_ms = 60.0 + frac * 340.0;
                                }
                                FxSlider::TintR => self.fx.crt_scanline_tint[0] = self.fx_frac_from_cursor(cx, &pv.geom.crt_tint_r_track),
                                FxSlider::TintG => self.fx.crt_scanline_tint[1] = self.fx_frac_from_cursor(cx, &pv.geom.crt_tint_g_track),
                                FxSlider::TintB => self.fx.crt_scanline_tint[2] = self.fx_frac_from_cursor(cx, &pv.geom.crt_tint_b_track),
                                FxSlider::CaretColorR => self.fx.caret_flash_color[0] = self.fx_frac_from_cursor(cx, &pv.geom.caret_color_r_track),
                                FxSlider::CaretColorG => self.fx.caret_flash_color[1] = self.fx_frac_from_cursor(cx, &pv.geom.caret_color_g_track),
                                FxSlider::CaretColorB => self.fx.caret_flash_color[2] = self.fx_frac_from_cursor(cx, &pv.geom.caret_color_b_track),
                            }
                        }
                    }
                    self.request_main_paint();
                    self.request_settings_paint();
                    // Radius/opacity/CRT sliders apply to detached windows too —
                    // repaint them live during the drag (damage-driven, no polling).
                    for dw in &self.detached {
                        dw.request_paint();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Persist the final value after any drag settles (opacity, radius,
                // dropdown, or Effects slider). The live updates during drag are
                // cheap writes to self.* fields; the final persist here is the
                // authoritative flush to disk.
                if self.dragging_slider || self.dragging_radius || self.dragging_dropdown || self.dragging_dropdown_width || self.active_fx_drag.is_some() {
                    self.persist();
                }
                // Live-apply a dropdown height/width change on RELEASE only (never
                // on every mouse-move — that would trigger an X11 resize storm). If
                // the main window is visible and in Dropdown mode, re-dock the top
                // strip to the new size immediately (re-asserted post-map via
                // pending_dock_frames) instead of waiting for the next F9.
                if (self.dragging_dropdown || self.dragging_dropdown_width)
                    && self.visible
                    && self.window_mode == WindowMode::Dropdown
                {
                    if let Some(w) = &self.window {
                        dock_window_top(w, self.dropdown_width_pct, self.dropdown_height_pct);
                        self.pending_dock_frames = 5;
                        self.request_main_paint();
                    }
                }
                self.dragging_slider = false;
                self.dragging_radius = false;
                self.dragging_dropdown = false;
                self.dragging_dropdown_width = false;
                self.active_fx_drag = None;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let Some(gpu) = &self.settings_gpu else { return };
                let (w, h) = (gpu.config.width, gpu.config.height);
                let pv = self.settings_panel_view(w, h);
                let cx = self.settings_cursor.0 as f32;
                let cy = self.settings_cursor.1 as f32;
                // Hit-test the panel only (no scrollbar in the settings window).
                let action = input::decide_mouse_press(Some(&pv.geom), None, cx, cy);
                self.handle_settings_action(action, &pv.geom);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // ── Effects tab (4): vertical content scroll ─────────────────────
                // Wheel anywhere in the settings window while the Effects tab is
                // active scrolls the content, not the font lists (which are on
                // different tabs). Clamp to [0, max_scroll] and redraw.
                if self.settings_tab == 4 {
                    let delta_px = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y * 24.0,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
                    };
                    // `effects_scroll` accumulates in PHYSICAL px, but build_panel
                    // divides it by `dpi = settings_char_w / CHAR_W_FALLBACK` to
                    // lay bands out in LOGICAL space. So the clamp bound (a LOGICAL
                    // content/viewport delta) must be scaled by the SAME dpi, or on
                    // HiDPI the bottom bands (caret RGB sliders) stayed unreachable
                    // and on sub-1× the scroll overshot into blank space (F10).
                    let dpi = (self.settings_char_w() / jetty_render::CHAR_W_FALLBACK).max(0.1);
                    let max_scroll = (jetty_render::EFFECTS_CONTENT_H
                        - jetty_render::EFFECTS_VISIBLE_H).max(0.0)
                        * dpi;
                    self.effects_scroll = (self.effects_scroll + delta_px).clamp(0.0, max_scroll);
                    self.request_settings_paint();
                    return;
                }
                // ── Font/UI-font list scroll (tabs 1) ────────────────────────────
                // Wheel over the terminal- OR UI-font list scrolls it (same as the
                // old in-app panel behaviour), now in the settings window.
                if self.font_families.is_empty() && self.ui_font_families.is_empty() {
                    return;
                }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y.round() as i32) * 3,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 20.0).round() as i32,
                };
                if lines == 0 {
                    return;
                }
                let Some(gpu) = &self.settings_gpu else { return };
                let (w, h) = (gpu.config.width, gpu.config.height);
                let pv = self.settings_panel_view(w, h);
                let cx = self.settings_cursor.0 as f32;
                let cy = self.settings_cursor.1 as f32;
                let over_list = pv.geom.font_rows.iter().any(|r| {
                    cx >= r.x && cx <= r.x + r.w
                        && cy >= pv.geom.font_rows.first().map(|r| r.y).unwrap_or(0.0)
                        && cy <= pv.geom.font_rows.last().map(|r| r.y + r.h).unwrap_or(0.0)
                });
                // Is the cursor over the UI (chrome) font list?
                let over_ui_list = !pv.geom.ui_font_rows.is_empty() && pv.geom.ui_font_rows.iter().any(|r| {
                    cx >= r.x && cx <= r.x + r.w
                        && cy >= pv.geom.ui_font_rows.first().map(|r| r.y).unwrap_or(0.0)
                        && cy <= pv.geom.ui_font_rows.last().map(|r| r.y + r.h).unwrap_or(0.0)
                });
                if over_list {
                    const MAX_FONT_ROWS: usize = 5;
                    let max_offset = self.font_families.len().saturating_sub(MAX_FONT_ROWS);
                    if lines > 0 {
                        self.font_scroll_offset = self.font_scroll_offset.saturating_sub(1);
                    } else {
                        self.font_scroll_offset = (self.font_scroll_offset + 1).min(max_offset);
                    }
                    self.request_settings_paint();
                } else if over_ui_list {
                    const MAX_UI_FONT_ROWS: usize = 4;
                    let max_offset = self.ui_font_families.len().saturating_sub(MAX_UI_FONT_ROWS);
                    if lines > 0 {
                        self.ui_font_scroll_offset = self.ui_font_scroll_offset.saturating_sub(1);
                    } else {
                        self.ui_font_scroll_offset = (self.ui_font_scroll_offset + 1).min(max_offset);
                    }
                    self.request_settings_paint();
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } if event.state.is_pressed() => {
                // Ignore X11's synthetic focus-gain presses: an Escape held while
                // the settings window takes focus must not instantly close it.
                if is_synthetic {
                    return;
                }
                // Escape closes the settings window.
                if matches!(event.logical_key, winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)) {
                    self.close_settings_window();
                    self.request_main_paint();
                }
            }
            WindowEvent::Focused(true) => {
                // Record that OUR settings window now holds focus so the main
                // window's Focused(false) auto-hide doesn't fire when the user
                // merely clicked into Settings.
                if let Some(w) = &self.settings_window {
                    self.last_focused_window = Some(w.id());
                    self.switching_to_settings = true;
                    // Cancel any scheduled main-window auto-hide (focus moved to
                    // one of OUR windows; the main FocusOut may have come first).
                    self.pending_autohide_at = None;
                    // macOS first-paint nudge: a request_redraw issued while the
                    // window was still being shown can be dropped, leaving it blank
                    // until the user clicks. Re-request now that it is shown+focused.
                    self.request_settings_paint();
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_settings_window();
                // The Poll repaint window (settings_paint_until) self-expires; no
                // need to clear it here — we keep repainting until the surface has
                // presented at least once.
            }
            WindowEvent::Focused(false) => {
                // Settings lost focus: clear last_focused_window so a later main
                // Focused(false) (focus left both Jetty windows to a third app) is
                // not mistaken for a switch-to-settings and the terminal hides.
                self.switching_to_settings = false;
                if self.last_focused_window == self.settings_window.as_ref().map(|w| w.id()) {
                    self.last_focused_window = None;
                }
                // A held slider/drag can never see its button release once focus is
                // gone — clear every drag latch so sliders don't keep tracking the
                // cursor with no button held after focus returns (F36).
                self.dragging_slider = false;
                self.dragging_radius = false;
                self.dragging_dropdown = false;
                self.dragging_dropdown_width = false;
                self.active_fx_drag = None;
                // F14: focus leaving the Settings window to a foreign app must
                // auto-hide the main dropdown too — schedule the deferred hide;
                // any JeTTY window regaining focus cancels it (Focused(true)).
                if self.focus_autohide
                    && self.visible
                    && self.summon_anim.is_none()
                    && !self.summon_pending
                {
                    self.pending_autohide_at = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(AUTOHIDE_GRACE_MS),
                    );
                }
            }
            _ => {}
        }
    }

    /// Earliest pending synchronized-update (`CSI ?2026`) deadline across every
    /// window (main tabs + detached), or `None`. Folded into `about_to_wait`'s
    /// single-wake schedule so a stuck BSU is force-flushed on time (F1) — no
    /// busy polling, damage-driven exactly like `reflow_pending_at`.
    fn sync_wake_at(&self) -> Option<std::time::Instant> {
        let mut earliest: Option<std::time::Instant> = None;
        let mut merge = |d: Option<std::time::Instant>| {
            if let Some(d) = d {
                earliest = Some(match earliest {
                    Some(e) if e <= d => e,
                    _ => d,
                });
            }
        };
        for tab in &self.tabs {
            merge(tab.terminal.sync_deadline());
        }
        for dw in &self.detached {
            merge(dw.tab.terminal.sync_deadline());
        }
        earliest
    }

    /// Force-terminate any window's synchronized update whose 150 ms deadline has
    /// elapsed, so bytes buffered since an unmatched BSU (`CSI ?2026h`) become
    /// visible instead of freezing the display until 2 MiB accumulate or an ESU
    /// arrives (F1 — e.g. an nvim/zellij that paused mid-redraw). Requests a
    /// redraw on each affected, actually-visible window.
    fn flush_expired_syncs(&mut self, now: std::time::Instant) {
        let active = self.active;
        let main_visible = self.visible && !self.main_occluded;
        // Collect whether the active tab flushed while the main window is visible;
        // the `self.request_main_paint()` choke borrows all of `self`, so it cannot
        // be called inside the `self.tabs.iter_mut()` loop — request once after.
        let mut main_needs_paint = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if tab.terminal.sync_deadline().is_some_and(|d| now >= d) {
                tab.terminal.flush_sync();
                if i == active && main_visible {
                    main_needs_paint = true;
                }
            }
        }
        if main_needs_paint {
            self.request_main_paint();
        }
        for dw in &mut self.detached {
            if dw.tab.terminal.sync_deadline().is_some_and(|d| now >= d) {
                dw.tab.terminal.flush_sync();
                if !dw.occluded {
                    dw.request_paint();
                }
            }
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    /// Drive ControlFlow. macOS does NOT deliver a `RedrawRequested` for a
    /// `request_redraw()` issued under `ControlFlow::Wait` until an input event
    /// arrives — so self-driving animations stall and freshly-shown windows stay
    /// blank until clicked. While any visual work is pending, switch to `Poll`
    /// AND actively re-request the frame, so the loop pumps frames; return to
    /// `Wait` (idle 0 CPU) the instant nothing is pending. On X11/Wayland this is
    /// just a brief Poll burst during the animation (redraws already deliver).
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Input-latency percentile emit (JETTY_PERF_LOG only): runs HERE, off the
        // timed present path, so printing a batch never stalls the frame it measured
        // (observer-effect fix). Emits at most once per REPORT_EVERY new samples.
        if self.perf.on {
            self.perf.maybe_report();
        }
        // Force-flush any elapsed synchronized update (CSI ?2026) FIRST so a
        // stuck BSU can't freeze the terminal; the next pending one is scheduled
        // via WaitUntil below (F1).
        self.flush_expired_syncs(std::time::Instant::now());
        // Debounced font-size reflow: when the deadline set by `set_font_size`
        // has elapsed (the user stopped pressing Ctrl+/-), issue ONE pty.resize
        // (via `reflow`) so the shell gets a single SIGWINCH instead of one per
        // press (which left stacked p10k prompts).
        let reflow_due = self
            .reflow_pending_at
            .is_some_and(|d| std::time::Instant::now() >= d);
        if reflow_due {
            self.reflow_pending_at = None;
            self.reflow();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        // Same debounced reflow for the detached windows (their Resized arm
        // only resizes the surface and arms `reflow_pending_at`, exactly like
        // the main window's — one SIGWINCH per drag, no p10k prompt scatter).
        {
            let status_h = self.status_h();
            let now = std::time::Instant::now();
            // Indexed loop (not iter_mut) so the reflowed window's cached
            // Ctrl+hover can be revalidated via &mut self below (F6).
            for pos in 0..self.detached.len() {
                let dw = &mut self.detached[pos];
                if dw.reflow_pending_at.is_some_and(|d| now >= d) {
                    dw.reflow_pending_at = None;
                    let (cw, ch) = dw.text.cell_size();
                    let (cols, rows) = crate::detached::grid_dims(
                        dw.gpu.config.width as f32,
                        dw.gpu.config.height as f32,
                        cw,
                        ch,
                        SCROLLBAR_GUTTER,
                        TABBAR_H,
                        status_h,
                    );
                    dw.tab.terminal.resize(cols, rows);
                    dw.tab.terminal.set_cell_px(cw, ch);
                    dw.tab.pty.resize(
                        cols as u16,
                        rows as u16,
                        (cols as f32 * cw).min(65535.0) as u16,
                        (rows as f32 * ch).min(65535.0) as u16,
                    );
                    dw.window.request_redraw();
                    // Same post-reflow hover revalidation as the main
                    // window's reflow() (F6); no-op unless Ctrl is held.
                    self.update_detached_link_hover(pos, true);
                }
            }
        }
        // Deferred focus-loss auto-hide: the grace period elapsed without any
        // JeTTY window regaining focus (which would have cancelled it) — hide.
        if self
            .pending_autohide_at
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.pending_autohide_at = None;
            self.autohide_main_window();
        }
        // Debounced config/theme hot-reload: the burst settled (no newer
        // ConfigChanged pushed the deadline out) — apply it once. (The deadline is
        // routed through WaitUntil below, so idle stays at zero work.)
        if self
            .pending_reload_at
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.pending_reload_at = None;
            self.reload_config_and_themes();
        }
        // Trailing scrollback-search refresh (F10): a streaming burst that
        // ended inside the throttle window marked the matches dirty but never
        // got a re-collect (no later drain carries data), leaving highlights,
        // counter and Enter-navigation stale indefinitely. Service the skipped
        // refresh exactly once at the throttle deadline (scheduled via the
        // WaitUntil merge below); the flag only exists while the bar is open
        // AND output was drained, so idle stays at zero work.
        if self.search_open
            && self.search_dirty
            && self
                .search_refresh_at
                .is_none_or(|t| t.elapsed() >= SEARCH_REFRESH_INTERVAL)
            && !self.tabs.is_empty()
        {
            self.search_dirty = false;
            self.active_tab_mut().terminal.search_refresh();
            self.search_refresh_at = Some(std::time::Instant::now());
            if self.visible && !self.main_occluded {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        // A pending (debounced) reflow does NOT keep the loop in Poll. The old
        // code folded `reflow_pending_at.is_some()` into `main_pending`, so for up
        // to 250ms after every font/window resize the loop sat in Poll and
        // re-rendered the full scene ~15× for nothing (a SPEED-#1 idle regression).
        // The active resize already drives redraws via per-event request_redraw;
        // here we only need to WAKE ONCE at the debounce deadline to run the reflow
        // (handled by `reflow_due` at the top of this fn). So the reflow deadline
        // is routed through WaitUntil below, never Poll.
        // `crt_anim_live()` is `false` whenever CRT animation is off (CRT disabled,
        // or all three animate toggles off), so this term cannot force Poll at idle:
        // static/off CRT keeps `main_pending` false → `about_to_wait` returns Wait
        // (0-CPU idle). When animation is ON it selects Poll, which Fifo present
        // throttles to ~60fps vsync — exactly how summon/slide animate on macOS,
        // where a `request_redraw` issued under Wait is not delivered until input.
        // The self-driven animation terms (CRT + caret flash) are gated on the
        // window being EFFECTIVELY VISIBLE — shown (F9) AND not occluded/minimized.
        // A hidden dropdown OR a minimized/occluded window must not keep the loop
        // in Poll rendering invisible frames forever (a permanent CPU+GPU burn
        // violating the 0-CPU-idle design; F8/F16/F17/F18). Summoning or restoring
        // the window resumes the animation (free-running clock). The summon/slide/
        // dock/center terms self-terminate in a handful of frames, so they stay
        // ungated (they only run while a show is in progress).
        let main_visible = self.visible && !self.main_occluded;
        let main_pending = self.summon_anim.is_some()
            || self.slide_anim.is_some()
            || self.summon_pending
            || self.pending_dock_frames > 0
            || self.pending_center_frames > 0
            || (main_visible && self.fx.crt_anim_live())
            || (main_visible && self.caret_anim.is_some());
        if main_pending {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        // Detached windows animate under the SAME gates, PER WINDOW: an animated
        // CRT sub-effect (shared setting) or a live caret-flash burst — but ONLY
        // for windows that are not occluded/minimized, so a minimized detached
        // window returns to true idle instead of burning a core forever (F8/F17).
        // False whenever no visible detached window animates (0-CPU preserved).
        let crt_live = self.fx.crt_anim_live();
        let detached_pending = self
            .detached
            .iter()
            .any(|d| !d.occluded && (crt_live || d.caret_anim.is_some()));
        if detached_pending {
            for dw in &self.detached {
                if !dw.occluded && (crt_live || dw.caret_anim.is_some()) {
                    dw.window.request_redraw();
                }
            }
        }
        let settings_pending = self.settings_window.is_some()
            && self
                .settings_paint_until
                .is_some_and(|d| std::time::Instant::now() < d);
        if settings_pending {
            if let Some(w) = &self.settings_window {
                w.request_redraw();
            }
        }

        // Earliest FUTURE deadline we owe a single wake for: the reflow debounce
        // and/or the perf-HUD idle one-shot. Neither polls — we sleep until the
        // soonest and wake exactly once. (A reflow whose deadline already elapsed
        // was run by `reflow_due` above, so `reflow_pending_at` here is always in
        // the future or None.)
        let now = std::time::Instant::now();
        let mut wake_at = self.reflow_pending_at;
        // Merge the earliest future deadline into wake_at (single-wake, no poll).
        let merge_wake = |wake_at: &mut Option<std::time::Instant>,
                          d: std::time::Instant| {
            *wake_at = Some(match *wake_at {
                Some(w) if w <= d => w,
                _ => d,
            });
        };
        // Detached-window debounced reflows (any already-elapsed deadline was
        // run above, so these are strictly in the future).
        for dw in &self.detached {
            if let Some(d) = dw.reflow_pending_at {
                merge_wake(&mut wake_at, d);
            }
        }
        // The scheduled focus-loss auto-hide (elapsed ones ran above).
        if let Some(d) = self.pending_autohide_at {
            merge_wake(&mut wake_at, d);
        }
        // The debounced config/theme reload deadline (elapsed ones ran above), so
        // the loop wakes exactly once to apply it instead of polling.
        if let Some(d) = self.pending_reload_at {
            merge_wake(&mut wake_at, d);
        }
        // Pending synchronized-update (CSI ?2026) flush deadline: wake exactly
        // once at the soonest so a stuck BSU is force-flushed on time. Any
        // already-elapsed deadline was flushed at the top of this fn, so this is
        // strictly in the future or None (F1).
        if let Some(d) = self.sync_wake_at() {
            merge_wake(&mut wake_at, d);
        }
        // Skipped (throttled) open-search refresh: wake once at the throttle
        // deadline so the trailing re-collect above runs (F10). An elapsed
        // deadline was serviced above, so this is strictly in the future.
        if self.search_open && self.search_dirty {
            if let Some(t) = self.search_refresh_at {
                merge_wake(&mut wake_at, t + SEARCH_REFRESH_INTERVAL);
            }
        }
        // Idle-HUD one-shot: flip the HUD from its last live value to an honest
        // "idle" reading once the app settles, then go fully idle.
        let perf_idle_pending = self.show_perf_hud
            && !self.perf_idle_shown
            && self.perf_idle_at.is_some();
        if perf_idle_pending {
            let d = self.perf_idle_at.unwrap();
            if now >= d {
                // Idle repaint is due now: request the single repaint (the redraw
                // request itself wakes the loop to service it).
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            } else {
                wake_at = Some(match wake_at {
                    Some(w) if w <= d => w,
                    _ => d,
                });
            }
        }

        let control_flow = if main_pending || settings_pending || detached_pending {
            winit::event_loop::ControlFlow::Poll
        } else if let Some(d) = wake_at {
            // Wake exactly once at the soonest pending deadline instead of polling.
            winit::event_loop::ControlFlow::WaitUntil(d)
        } else {
            winit::event_loop::ControlFlow::Wait
        };
        // Idle RSS (JETTY_PERF_LOG only): sampled ONCE, the first time the loop
        // settles to a true `Wait` after the prompt is up (≥750ms since exec, so the
        // shell has drawn its prompt). Reuses the HUD's sysinfo handle; latches, so
        // it costs one syscall for the whole session and nothing thereafter. Zero
        // cost when off (guarded by `perf.on`). RSS includes shared pages (not PSS).
        if self.perf.on
            && !self.perf.idle_rss_logged
            && self.perf.first_frame_logged
            && matches!(control_flow, winit::event_loop::ControlFlow::Wait)
            && crate::perf::process_start().is_none_or(|t| {
                t.elapsed() >= std::time::Duration::from_millis(750)
            })
        {
            self.perf.idle_rss_logged = true;
            if let Some(bytes) = crate::perf::current_rss_bytes() {
                eprintln!(
                    "jetty-perf: idle RSS {:.1} MB (resident set incl. shared pages, not PSS; via sysinfo)",
                    bytes as f64 / (1024.0 * 1024.0)
                );
            }
        }
        event_loop.set_control_flow(control_flow);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Cold-start parallelism: the FontSystem (~20ms) and the initial PTY
        // fork/exec are both GPU-independent and Send, so kick them off NOW on
        // worker threads. They run fully overlapped with build_window +
        // GpuContext::new (the GPU adapter/device block dominates cold start),
        // then we join after the GPU is ready. Window/surface stay on the main
        // thread (they are !Send). The PTY is spawned at a provisional grid and
        // resized to the real cols/rows once the cell size is known.
        let font_handle = std::thread::spawn(TextLayer::build_font_system);
        let proxy_wake = self.proxy.clone();
        let shell = self.opt_shell();
        let pty_handle = std::thread::spawn(move || {
            // Provisional grid at startup: the real text-area pixel size is set by
            // the immediate resize once the cell metrics are known (see below).
            PtySession::spawn(FALLBACK_COLS as u16, FALLBACK_ROWS as u16, 0, 0, shell, None, move || {
                let _ = proxy_wake.send_event(AppEvent::Wake);
            })
        });

        // Startup: a failure to create the main window is genuinely fatal (there
        // is nothing to fall back to), so surface it as a clean panic here — the
        // runtime detach/settings call sites handle their `Err` gracefully.
        let window = jetty_platform::build_window(event_loop, "JeTTY", (1000, 640))
            .expect("create_window failed");
        // Allow IME on the terminal window (winit disables it by default):
        // without this, CJK/complex input methods can never commit text and
        // dead-key composition is degraded. Commits arrive as
        // `WindowEvent::Ime(Ime::Commit)` and are sent to the PTY as typed
        // text; preedit rendering is intentionally not implemented.
        window.set_ime_allowed(true);
        // First open: place the window per the configured mode. Center mode
        // centers; Dropdown mode docks as a top strip and slides in.
        match self.window_mode {
            WindowMode::Center => center_window(&window),
            WindowMode::Dropdown => {
                dock_window_top(&window, self.dropdown_width_pct, self.dropdown_height_pct);
                // KWin ignores the pre-map dock above (window not realized yet) →
                // re-assert on the first post-map redraws so it actually lands at
                // the top strip instead of the WM's default (centered) placement.
                self.pending_dock_frames = 5;
                self.slide_anim = Some(std::time::Instant::now());
            }
        }
        // One-time Wayland diagnostic: winit cannot report the outer position on
        // Wayland, so set_outer_position/request_inner_size silently no-op and
        // the compositor places the window. Accepted degradation (no DE code).
        if !self.wayland_warned && window.outer_position().is_err() {
            self.wayland_warned = true;
            eprintln!(
                "jetty: window positioning is a no-op on this platform (Wayland?); \
                 Dropdown/Center geometry falls back to compositor placement + the \
                 reveal effect — same accepted degradation as the F9 hotkey."
            );
        }
        let size = window.inner_size();
        // HiDPI: the display's scale factor (>1.0 on HiDPI/Retina screens).
        // inner_size() already returns physical pixels; we multiply the logical
        // font size by scale to get the physical font size so glyphs are sharp.
        let scale = window.scale_factor() as f32;
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        // GPU is ready — join the font worker (its ~20ms load happened in
        // parallel with the GPU block above, so this join is typically free).
        let font_system = font_handle.join().expect("font worker panicked");
        let (text, quad, cols, rows) = if let Some(ref g) = gpu {
            let text = TextLayer::new_with_family_and_fonts(
                &g.device, &g.queue, g.format, self.font_logical * scale, &self.font_family,
                font_system,
            );
            let (cw, ch) = text.cell_size();
            // Derive the grid from the physical pixel size and the physical cell size.
            let cols = ((size.width as f32 - SCROLLBAR_GUTTER) / cw).floor().max(2.0) as usize;
            let rows = ((size.height as f32 - TABBAR_H - self.status_h()) / ch).floor().max(1.0) as usize;
            let quad = QuadLayer::new(&g.device, g.format);
            (Some(text), Some(quad), cols, rows)
        } else {
            (None, None, FALLBACK_COLS, FALLBACK_ROWS)
        };
        // Populate the cached font family list from the new TextLayer.
        if let Some(ref t) = text {
            self.font_families = t.monospace_families();
            eprintln!("jetty: found {} monospace families", self.font_families.len());

            // Validate the persisted font family: if it's empty or no longer
            // present among the enumerated monospace families (e.g. the user
            // uninstalled it), fall back to the default ("MesloLGS NF" when
            // available, otherwise the first family) and log the substitution.
            let valid = !self.font_family.is_empty()
                && self.font_families.iter().any(|f| f == &self.font_family);
            if !valid {
                let fallback = if self.font_families.iter().any(|f| f == "MesloLGS NF") {
                    "MesloLGS NF".to_string()
                } else {
                    self.font_families.first().cloned().unwrap_or_default()
                };
                if !fallback.is_empty() {
                    eprintln!(
                        "jetty: configured font family {:?} not found; falling back to {:?}",
                        self.font_family, fallback
                    );
                    self.font_family = fallback;
                }
            }
        }

        // Build the rounded-corner mask (final fullscreen pass) for the borderless
        // main window, using the same surface format as the rest of the pipeline.
        if let Some(ref g) = gpu {
            self.corner_mask = Some(jetty_render::CornerMask::new(&g.device, g.format));
            // Build the Bayer crystallize reveal (final fullscreen pass) and arm
            // the first-open summon so the frame materializes out of the dither
            // lattice the instant the window appears.
            self.bayer_reveal = Some(jetty_render::BayerReveal::new(&g.device, g.format));
            self.phosphor = Some(jetty_render::PhosphorIgnition::new(&g.device, g.format));
            // Tier-B effects + their surface-sized offscreen scene texture. The
            // texture is allocated up front (cheap) but only WRITTEN/SAMPLED while
            // a Tier-B effect is summoning or CRT is enabled; Tier-A and normal
            // (CRT-off) frames never use it.
            self.liquid = Some(jetty_render::LiquidDrop::new(&g.device, g.format));
            self.focus = Some(jetty_render::FocusPull::new(&g.device, g.format));
            // CRT post-effect (passthrough for now). Same surface format as the
            // rest of the pipeline so the blit-to-surface target matches.
            self.crt = Some(jetty_render::Crt::new(&g.device, g.format));
            // Inline-image (sixel) layer on the main device. Same surface format
            // as the scene target; zero cost until an image is visible.
            self.image_layer = Some(jetty_render::ImageLayer::new(&g.device, g.format));
            // Caret glow/ripple (Task 12). Built unconditionally so the toggle
            // can be flipped at runtime without a restart; dispatched only when
            // `fx.caret_glow_enabled` is true (zero cost when off).
            self.caret_fx = Some(jetty_render::CaretFx::new(&g.device, g.format));
            self.summon_pending = true;
            self.summon_settle_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
        }

        // Build the chrome TextLayer (tab bar / menus / overlays / status bar). It
        // renders ALL window chrome at the UI font size (ui_font_logical * scale)
        // in the UI family — decoupled from the terminal font, so chrome can't
        // overflow when the terminal font changes. A UI-font SIZE change resizes it
        // IN-PLACE; a FAMILY change swaps ui_family — neither rebuilds the layer.
        if let Some(ref g) = gpu {
            let mut chrome = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, self.ui_font_logical * scale, &self.font_family,
            );
            // Populate the UI-font picker list: a synthetic "System Sans (default)"
            // row (→ "") first, then the installed proportional families.
            self.ui_font_families = std::iter::once("System Sans (default)".to_string())
                .chain(chrome.proportional_families())
                .collect();
            eprintln!(
                "jetty: found {} proportional UI families",
                self.ui_font_families.len().saturating_sub(1)
            );
            // Validate the persisted UI family: a non-empty family that is no
            // longer installed falls back to "" (platform sans) so a removed font
            // never leaves blank chrome.
            if !self.ui_font_family.is_empty()
                && !self.ui_font_families.iter().any(|f| f == &self.ui_font_family)
            {
                eprintln!(
                    "jetty: configured UI font {:?} not found; falling back to system sans",
                    self.ui_font_family
                );
                self.ui_font_family.clear();
            }
            // Apply the (validated) UI family to the chrome layer (no rescan).
            chrome.set_ui_family(if self.ui_font_family.is_empty() {
                None
            } else {
                Some(self.ui_font_family.as_str())
            });
            self.chrome_text = Some(chrome);
        }

        self.window = Some(window);
        self.gpu = gpu;
        self.text = text;
        self.quad = quad;
        // The Tier-B offscreen scene texture is allocated LAZILY (on the first
        // frame of an actual Liquid/Focus summon) rather than eagerly here — it is
        // a full-surface GPU texture used only by those two effects, so most
        // sessions never need it. See the lazy (re)alloc in the render path.

        // Build the first tab with the derived grid size so the PTY and terminal
        // agree with the actual window layout. The on_data callback wakes the
        // winit event loop the instant bytes arrive (within ~1ms) — critical for
        // p10k's cursor-position / capability queries which have tight timeouts.
        let mut terminal = Terminal::new(cols, rows);
        terminal.set_theme(self.current_theme());
        // OSC 52 paste (remote clipboard READ) is opt-in and off by default (secure).
        // Applied at spawn so new tabs pick up the current setting.
        terminal.set_osc52_allow_paste(self.osc52_allow_paste);
        // Apply the configured scrollback cap (guard skips the no-op
        // set_options round-trip on the 10k default path).
        if self.scrollback_lines != 10_000 {
            terminal.set_scrollback_lines(self.scrollback_lines);
        }
        // Join the PTY worker (forked in parallel with the GPU block) and resize
        // it from the provisional grid to the real cols/rows now that the cell
        // size is known.
        let pty = match pty_handle.join().expect("pty worker panicked") {
            Ok(pty) => pty,
            Err(e) => {
                eprintln!("jetty: failed to spawn PTY: {e}");
                event_loop.exit();
                return;
            }
        };
        let (px_w, px_h) = self
            .text
            .as_ref()
            .map(|t| {
                let (cw, ch) = t.cell_size();
                ((cols as f32 * cw).min(65535.0) as u16, (rows as f32 * ch).min(65535.0) as u16)
            })
            .unwrap_or((0, 0));
        pty.resize(cols as u16, rows as u16, px_w, px_h);
        terminal.resize(cols, rows);
        // Surface a one-line notice if the configured shell was unavailable and
        // spawn fell back to another shell, so the fallback is not silent (F2).
        if let Some(notice) = pty.startup_notice() {
            terminal.feed(format!("\x1b[33m{notice}\x1b[0m\r\n").as_bytes());
        }
        let writer = pty.writer();
        self.tabs.push(Tab {
            terminal,
            pty,
            writer,
            title: "Tab 1".to_string(),
            default_title: "Tab 1".to_string(),
            manually_renamed: false,
            activity: jetty_render::TabActivity::None,
        });
        self.active = 0;

        // Register the F9 global hotkey (Yakuake-style toggle). This only works
        // on X11; on Wayland registration will fail and we log a warning without
        // crashing. The manager must be kept alive (stored in self.hotkey_manager)
        // or the hotkey is automatically unregistered when it drops.
        // Off the main thread: GlobalHotKeyManager::register() blocks on a worker
        // that opens a 2nd X11 connection + xkb round-trips at the tail of a loop
        // ending in a 50ms sleep — that wait used to sit at the END of resumed(),
        // directly delaying the first redraw. The F9 event was already delivered
        // through the async proxy (never read synchronously), so moving register()
        // off-thread changes only WHERE it blocks, not the event semantics. The
        // manager is kept alive inside the forwarding loop (which never returns).
        if self.hotkey_manager.is_none() {
            self.hotkey_manager = Some(());
            let proxy_hotkey = self.proxy.clone();
            let summon_hotkey = self.summon_hotkey.clone();
            std::thread::spawn(move || {
                use std::str::FromStr;
                let manager = match global_hotkey::GlobalHotKeyManager::new() {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("global hotkey {summon_hotkey} unavailable (Wayland? already grabbed?) — {e}");
                        return;
                    }
                };
                // Parse the configured string with global_hotkey's own parser
                // (handles "F9", "F12", and even "Ctrl+Shift+F12"); fall back to F9.
                let hotkey = global_hotkey::hotkey::HotKey::from_str(&summon_hotkey)
                    .unwrap_or_else(|e| {
                        eprintln!("invalid summon_hotkey {summon_hotkey:?} ({e}); falling back to F9");
                        global_hotkey::hotkey::HotKey::new(None, global_hotkey::hotkey::Code::F9)
                    });
                if let Err(e) = manager.register(hotkey) {
                    eprintln!("global hotkey {summon_hotkey} unavailable (Wayland? already grabbed?) — {e}");
                    return;
                }
                // Forward summon-key-pressed events to the winit loop. Keeps `manager`
                // alive for the program lifetime (this loop never returns).
                let rx = global_hotkey::GlobalHotKeyEvent::receiver();
                while let Ok(ev) = rx.recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed {
                        let _ = proxy_hotkey.send_event(AppEvent::ToggleVisibility);
                    }
                }
                drop(manager);
            });
        }

        // Slow safety heartbeat — 100ms is enough for any future time-based UI
        // while virtually eliminating idle CPU waste. Real responsiveness now
        // comes from the on_data wake above, not from this tick.
        spawn_waker(self.proxy.clone());

        // Config/theme hot-reload watcher (unless disabled). OS-event-driven, so its
        // thread blocks in the kernel and adds ZERO idle CPU. The returned handle is
        // stored so it lives for the process lifetime (dropping it stops watching).
        if self.hot_reload && self.config_watcher.is_none() {
            self.config_watcher = crate::watch::spawn_config_watcher(self.proxy.clone());
        }

        self.request_main_paint();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: AppEvent) {
        match ev {
            AppEvent::Wake => {
                let (had_data, chrome_changed, exited) = self.drain_pty();
                // Input-latency echo signal (JETTY_PERF_LOG only): the Wake drain is
                // usually where the shell's keystroke echo is consumed (before the
                // redraw re-drains empty), so mark it here too. Gated on `perf.on`;
                // drain_pty/drain_one_tab themselves stay byte-identical.
                if self.perf.on && had_data {
                    self.perf.note_active_output();
                }
                // A tab whose shell exited (Ctrl+D / `exit`) closes THAT tab,
                // Yakuake-style; if it was the last tab, close_exited_tabs exits
                // the loop. The waker fires ~10x/s, so we react within a frame.
                if !self.close_exited_tabs(exited, event_loop) {
                    return;
                }
                // Output rotated the scrollback under the open search: its
                // stored match Points are stale until the next (throttled)
                // re-collect. Marked HERE too — not just in the render-path
                // drain — because this drain may consume the whole burst,
                // leaving the following RedrawRequested drain empty (F10).
                if had_data && self.search_open {
                    self.search_dirty = true;
                }
                // Damage-driven: only request a redraw when the active tab's PTY
                // produced data (or query replies were sent). Background tabs still
                // drained above but don't trigger a repaint. When idle, the 100ms
                // heartbeat drains nothing and we skip the redraw entirely.
                // Also gated on the window being EFFECTIVELY VISIBLE (shown + not
                // occluded/minimized): a hidden dropdown running `cat bigfile`
                // must keep draining (so the shell never blocks) but must NOT run
                // the full render pipeline into an unmapped surface (F16).
                // `chrome_changed` fires only on indicator TRANSITIONS (at
                // most None->Output->Bell between views) or on an actual tab
                // TITLE change (bounded by how often the shell rewrites it),
                // so a flooding background tab costs one extra redraw total,
                // not one per Wake — while a background OSC 0/2 title update
                // still reaches the tab bar and taskbar title (F1/F14).
                if (had_data || chrome_changed) && self.visible && !self.main_occluded {
                    self.request_main_paint();
                    // Grid content changed under an ACTIVE Ctrl+hover: revalidate
                    // the cached spans so the underline tracks (or vanishes with)
                    // the moved text. Only runs while a link is hovered — zero
                    // cost on the idle/no-hover drain path.
                    if self.link_hover.is_some() {
                        self.update_link_hover(true);
                    }
                }
                // Detached windows aren't in `self.tabs`, so the loop above never
                // sees them — without this, a detached window's live shell output
                // wouldn't repaint until an unrelated event (resize/focus) forced
                // a `RedrawRequested`. Drain each detached tab the same way, and
                // redraw only the windows whose tab actually produced data
                // (same damage-driven discipline as the active-tab check above).
                let mut vt_read: u64 = 0;
                let mut exited_detached: Vec<usize> = Vec::new();
                for (i, dw) in self.detached.iter_mut().enumerate() {
                    let (had, title_changed) = Self::drain_one_tab(&mut dw.tab, &mut vt_read);
                    // Consume the bell so a reattach never shows a phantom Bell
                    // dot. Detached windows draw no indicator by design: the tab
                    // IS the visible, active tab of its own window.
                    let _ = dw.tab.terminal.take_bell();
                    // OSC titles: sync the OS window title even when occluded
                    // (the taskbar entry of a minimized window must update).
                    dw.sync_os_title();
                    // Same damage-driven + visibility discipline as the main
                    // window: drain always (keep the shell unblocked) but only
                    // repaint a detached window that isn't occluded/minimized
                    // (F16). A title-only change repaints too: the detached
                    // top bar draws the title (F1/F14).
                    if (had || title_changed) && !dw.occluded {
                        dw.request_paint();
                        // Grid content changed under an ACTIVE Ctrl+hover in this
                        // window: revalidate the cached spans at the same cell
                        // (mirrors the main window's Wake-drain recompute; only
                        // runs while a link is hovered).
                        if dw.link_hover.is_some() {
                            if let Some((line, col)) = dw.link_hover_cell {
                                dw.link_hover = dw.tab.terminal.link_at(line, col);
                                if dw.link_hover.is_none() {
                                    dw.window.set_cursor(dw.resize_zone.cursor_icon());
                                }
                            }
                        }
                    }
                    // Shell exit (Ctrl+D / `exit`) inside a detached window closes
                    // THAT window — never reattach an exited shell. Unlike the main
                    // window's `close_exited_tabs`, there is no "last window" special
                    // case here: the app keeps running even if every detached window
                    // closes, so we never call `event_loop.exit()` for this.
                    if dw.tab.terminal.child_exited() || dw.tab.pty.child_exited() {
                        exited_detached.push(i);
                    }
                }
                self.vt_bytes += vt_read;
                // Remove in descending index order so earlier indices stay valid,
                // mirroring `close_exited_tabs`. Dropping the `DetachedWindow`
                // closes its OS window; its already-exited child is reaped
                // harmlessly by `PtySession::Drop`.
                for i in exited_detached.into_iter().rev() {
                    if i < self.detached.len() {
                        let dw = self.detached.remove(i);
                        // The dying window usually holds focus (the user typed
                        // `exit` in it); once the entry is gone its Focused(false)
                        // can no longer be routed here, so clear the focus
                        // bookkeeping NOW (mirrors reattach_tab) — otherwise
                        // switching_to_detached stays latched true and the main
                        // window's focus auto-hide is silently disabled until the
                        // next detach/reattach cycle.
                        if self.last_focused_window == Some(dw.window.id()) {
                            self.last_focused_window = None;
                            self.switching_to_detached = false;
                        }
                    }
                }
                // Fire "command finished" notifications for OSC 133 completions the
                // drains above surfaced. Placed AFTER both the main drain and the
                // detached-drain loop so completions from EITHER are dispatched
                // (amendments §3) — and this is the hidden-window path, the flagship
                // use case (no RedrawRequested arrives while hidden).
                self.dispatch_completions(event_loop);
            }
            AppEvent::ToggleVisibility => {
                self.toggle_visibility(event_loop);
            }
            AppEvent::SetVisible(want) => {
                self.set_visibility(want, event_loop);
            }
            AppEvent::ConfigChanged => {
                // Debounce an editor's write/rename/chmod burst: schedule ONE reload
                // shortly ahead and coalesce (a newer event just pushes it out). The
                // actual reload runs from `about_to_wait` when the deadline passes —
                // no disk read here. `about_to_wait` folds this into its WaitUntil
                // deadline, so idle stays at zero work (one wake, then back to Wait).
                self.pending_reload_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Route events to the settings window when they belong to it. Everything
        // else falls through to the main-terminal handling below.
        if self.settings_window.as_ref().is_some_and(|w| w.id() == id) {
            self.settings_window_event(event);
            return;
        }
        // Route events to a detached window when they belong to one. Only
        // rendering is wired up here (Task 5); keyboard/resize routing and
        // reattach are added in later tasks.
        if let Some(pos) = self.detached.iter().position(|d| d.window.id() == id) {
            self.handle_detached_event(pos, event_loop, event);
            return;
        }
        // Anything not addressed to a live child window is meant for the main
        // window — but only if the id actually matches it. Events still queued
        // for a window just dropped this pump (settings closed, a detached
        // window removed after its shell exited, a reattach) would otherwise be
        // handled AS IF they targeted the main terminal (a stale CloseRequested
        // popping the quit dialog, a stale Focused(false) scheduling an
        // auto-hide of the focused terminal). Drop them.
        if self.window.as_ref().map(|w| w.id()) != Some(id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.confirm_quit = true;
                self.request_main_paint();
            }
            WindowEvent::Occluded(occluded) => {
                // The compositor tells us the main window is fully hidden behind
                // others (or minimized on platforms that report it here). Track it
                // so every self-driven animation/redraw stops (F17) — a minimized
                // window with CRT animation would otherwise Poll-spin forever. On
                // un-occlude, request one redraw to repaint the freshly-shown surface.
                self.main_occluded = occluded;
                if !occluded {
                    self.request_main_paint();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.gpu, &mut self.text) {
                    text.resize(gpu);
                }
                // A resize invalidates the menus' cached absolute hit rects
                // (built at open against the OLD window size) — close them so
                // hover/click never hit-test stale geometry. Resizes reachable
                // while a menu is open need no in-window click (tiling
                // shortcuts, un-maximize).
                self.dismiss_menus();
                // Invalidate the Tier-B offscreen scene texture (now the wrong
                // size). It is rebuilt LAZILY at the correct size on the next
                // Tier-B summon frame — previously it was eagerly re-created on
                // EVERY Resized event (a full-surface GPU texture freed+rebuilt per
                // drag-frame) though it is never sampled mid-resize.
                self.offscreen = None;
                // DEBOUNCE the grid+PTY reflow (same reasoning as set_font_size): a
                // corner-drag fires many Resized events; reflowing + a SIGWINCH on
                // each bombards p10k with redraws and scatters its prompt across the
                // screen (worst on an empty tab, where the lone prompt is the only
                // content). Schedule ONE reflow after the drag settles (250ms, same
                // as font changes — a short window let aggressive/paused drags fire
                // several reflows, each leaving a stray prompt). The surface already
                // resized above, so the window tracks the drag live; the grid snaps
                // to the new col/row count when the single reflow fires.
                self.reflow_pending_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                self.request_main_paint();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Fired when the window is moved between monitors with different DPI.
                // Rebuild TextLayer with the new physical font size (logical * new
                // scale). The surface has NOT resized yet — gpu.resize() only runs
                // in the following Resized event — so calling reflow() here would
                // SIGWINCH the shell with the stale surface size. Instead, arm the
                // debounced reflow and let the Resized event's reflow correct the
                // grid against the real surface size.
                let scale = scale_factor as f32;
                // Re-scale the font IN-PLACE (reusing the FontSystem) rather than
                // rebuilding the TextLayers — a DPI change must not rescan
                // fontconfig (~20ms) twice on the main thread.
                if let Some(t) = self.text.as_mut() {
                    t.set_font_size(self.font_logical * scale);
                }
                // Chrome scales with the UI font (not the terminal font).
                if let Some(t) = self.chrome_text.as_mut() {
                    t.set_font_size(self.ui_font_logical * scale);
                }
                self.reflow_pending_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(120));
                self.request_main_paint();
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Arm the link hover on modifier press at the current cursor;
                // a release sweeps EVERY window (this event is per-focused-
                // window only, so an unfocused sibling would otherwise keep a
                // stale underline).
                if link_modifier_held(&self.modifiers) {
                    self.update_link_hover(true);
                } else {
                    self.clear_all_link_hovers();
                }
            }
            WindowEvent::Focused(true) => {
                // The main terminal window gained focus.
                self.last_focused_window = Some(id);
                self.main_focused = true;
                // Focus implies the window is on-screen again: clear any stale
                // occluded/minimized flag in case the WM skipped Occluded(false)
                // on restore, so animations/redraws resume (F17).
                self.main_occluded = false;
                // A scheduled auto-hide is void: focus is back on us.
                self.pending_autohide_at = None;
                // Any pending "switching to a sibling window" latch is over too.
                // Without this, a detach whose new window the WM never focused
                // (focus-stealing prevention) would leave switching_to_detached
                // stuck true and silently disable auto-hide.
                self.switching_to_detached = false;
                self.switching_to_settings = false;
                // Clear any taskbar/dock urgency we raised on a command-finish
                // notification: X11 latches XUrgencyHint until explicitly cleared,
                // so without this the taskbar entry stays lit after the user
                // returns. A no-op where none was set / unsupported (Wayland).
                if let Some(w) = &self.window {
                    w.request_user_attention(None);
                    // macOS first-paint nudge (see the settings window above): ensure
                    // a frame is drawn once the window is actually shown + focused.
                    self.request_main_paint();
                }
            }
            WindowEvent::Focused(false) => {
                self.main_focused = false;
                // A held tab drag can never see its release once focus is gone —
                // clear it (and its grabbing cursor) so it doesn't resume stuck.
                if self.tab_drag.take().is_some() {
                    if let Some(win) = &self.window {
                        win.set_cursor(winit::window::CursorIcon::Default);
                    }
                }
                // A link underline can't clear itself while unfocused (the
                // modifier release is delivered elsewhere) — drop it now.
                self.link_hover_cell = None;
                if self.link_hover.take().is_some() {
                    if let Some(win) = &self.window {
                        win.set_cursor(self.resize_cursor.cursor_icon());
                        self.request_main_paint();
                    }
                }
                // Yakuake-style auto-hide: hide when the window loses focus, but
                // only when ENABLED, currently visible, NOT mid-summon (X11 fires
                // a synthetic Focused(false) during set_visible/focus), and focus
                // did NOT move to our own Settings window.
                let settings_id = self.settings_window.as_ref().map(|w| w.id());
                // `switching_to_settings` covers the X11 case where the main
                // Focused(false) arrives BEFORE the settings Focused(true), which
                // the last_focused_window comparison alone would miss.
                let to_settings = self.switching_to_settings
                    || (self.last_focused_window.is_some()
                        && self.last_focused_window == settings_id);
                // Same exemption for OUR detached windows: detaching a tab moves
                // focus to the new detached window, which must not hide the main
                // window. `switching_to_detached` covers the race where the main
                // Focused(false) arrives before the detached Focused(true).
                let detached_ids: Vec<WindowId> =
                    self.detached.iter().map(|d| d.window.id()).collect();
                let to_detached = self.switching_to_detached
                    || crate::detached::focus_in_detached(self.last_focused_window, &detached_ids);
                if self.focus_autohide
                    && self.visible
                    && self.summon_anim.is_none()
                    && !self.summon_pending
                    // Don't auto-hide within the post-summon settle window: a
                    // synthetic Focused(false) right after the window maps would
                    // otherwise dismiss a fast (None/Bayer) summon as it appears.
                    && self
                        .summon_settle_until
                        .is_none_or(|d| std::time::Instant::now() >= d)
                    && !to_settings
                    && !to_detached
                {
                    // SCHEDULE the hide instead of hiding now: X11 can deliver
                    // this FocusOut BEFORE the FocusIn of an already-open JeTTY
                    // sibling window (detached/Settings) the user clicked — the
                    // switching_to_* flags only pre-arm window CREATION. Any of
                    // our windows gaining focus within the grace period cancels
                    // it; a genuine focus departure hides AUTOHIDE_GRACE_MS
                    // later (imperceptible). Fired by `about_to_wait`.
                    self.pending_autohide_at = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(AUTOHIDE_GRACE_MS),
                    );
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let prev = self.cursor;
                self.cursor = (position.x, position.y);
                // --- Mouse motion / drag reports (modes 1002 / 1003) (F5) ---
                // When the app enabled button-drag (1002) or any-motion (1003)
                // reporting, emit one motion report per cell change — mirroring
                // the press/release SGR path. Suppressed while a local Shift
                // selection is in progress so drag-select still works over a
                // mouse-mode app. tmux pane-resize and nvim visual-drag rely on
                // this; previously only press/release were forwarded.
                if !self.selecting && !self.tabs.is_empty() {
                    let (drag, motion) = {
                        let t = &self.active_tab().terminal;
                        (t.mouse_drag(), t.mouse_motion())
                    };
                    if drag || motion {
                        // A forwarded, still-held left press marks the left button
                        // as down (mouse_grab_press is taken on release).
                        let left_held = self.mouse_grab_press.is_some();
                        // 1002 reports only while a button is held; 1003 reports
                        // any motion (base 3 == no button when nothing is held).
                        if motion || left_held {
                            let new_cell = self.cell_at_pixel(position.x, position.y);
                            let prev_cell = self.cell_at_pixel(prev.0, prev.1);
                            if new_cell.is_some() && new_cell != prev_cell {
                                let base = if left_held { 0u8 } else { 3u8 };
                                self.send_mouse_report(input::MouseEvent::Motion { button: base });
                            }
                        }
                    }
                }
                // --- Resize-edge cursor feedback (borderless window) ---
                // Only update the cursor when the zone changes, never while a host
                // drag (scrollbar / selection) is in progress, and never while a
                // modal (confirm / help / context menu) is open — a press there is
                // consumed by the modal, so a resize-edge cursor under it is wrong.
                let modal_open = self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.help_open
                    || self.context_menu.is_some()
                    || self.tab_menu.is_some();
                if !self.dragging_scrollbar
                    && !self.selecting
                    && !modal_open
                    && self.tab_drag.is_none()
                {
                    if let Some(gpu) = &self.gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        let zone = resize_zone_at(position.x as f32, position.y as f32, w, h);
                        if zone != self.resize_cursor {
                            self.resize_cursor = zone;
                            if let Some(win) = &self.window {
                                // Link-aware: the Pointer survives leaving a
                                // resize edge while a link is still hovered.
                                win.set_cursor(self.desired_cursor(zone));
                            }
                        }
                    }
                } else if modal_open && self.resize_cursor != ResizeZone::None {
                    // A modal opened while an edge cursor was showing — reset it.
                    self.resize_cursor = ResizeZone::None;
                    if let Some(win) = &self.window {
                        win.set_cursor(ResizeZone::None.cursor_icon());
                    }
                }
                // Repaint when the window-control hover state changes so the
                // min/max/close highlight tracks the cursor.
                if let Some(gpu) = &self.gpu {
                    let w = gpu.config.width;
                    let bar_y = self.tabbar_y(gpu.config.height as f32);
                    let before = ctrl_hover_at(prev.0 as f32, prev.1 as f32, w, bar_y);
                    let after = ctrl_hover_at(position.x as f32, position.y as f32, w, bar_y);
                    if before != after {
                        self.request_main_paint();
                    }
                }
                if self.dragging_scrollbar {
                    // Copy width/height to avoid borrow conflicts.
                    let (w, h) = if let Some(gpu) = &self.gpu {
                        (gpu.config.width, gpu.config.height)
                    } else {
                        return;
                    };
                    self.apply_scroll_from_cursor(w, h);
                    self.request_main_paint();
                }
                // --- Tab drag-out (tearing) tracking ---
                // While a tab is held, flip the tearing state as the cursor
                // crosses the ±TEAR_THRESHOLD_PX band around the strip. The
                // grabbing cursor is the visual cue; returning to the strip
                // cancels tearing so the release is a plain click again.
                if self.tab_drag.is_some() {
                    let bar_y = self
                        .gpu
                        .as_ref()
                        .map(|g| self.tabbar_y(g.config.height as f32))
                        .unwrap_or(0.0);
                    let now_tearing = crate::detached::tearing(
                        position.y as f32,
                        bar_y,
                        TABBAR_H,
                        crate::detached::TEAR_THRESHOLD_PX,
                    ) && crate::detached::can_detach(self.tabs.len());
                    if let Some(drag) = self.tab_drag.as_mut() {
                        if drag.tearing != now_tearing {
                            drag.tearing = now_tearing;
                            if let Some(win) = &self.window {
                                win.set_cursor(if now_tearing {
                                    winit::window::CursorIcon::Grabbing
                                } else {
                                    winit::window::CursorIcon::Default
                                });
                            }
                        }
                    }
                }
                // --- Tab context menu hover update (cached rects, like above) ---
                if self.tab_menu.is_some() {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let new_hover = self.tab_menu_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if new_hover != self.tab_menu_hover {
                        self.tab_menu_hover = new_hover;
                        self.request_main_paint();
                    }
                }
                // --- Text selection drag continuation ---
                // Gated on `selecting` alone: it is set only when a local selection
                // actually began (mouse reporting off, or Shift held to override it),
                // so a Shift-drag over a mouse-mode app still extends the selection.
                if self.selecting {
                    if let Some((line, col, left_half)) = self.cursor_cell_0_side() {
                        self.active_tab_mut().terminal.selection_update(line, col, left_half);
                        self.request_main_paint();
                    }
                }
                // --- Context menu hover update ---
                // Reuse the cached item_rects (built when the menu opened) instead
                // of rebuilding the whole menu on every (high-frequency) move.
                if self.context_menu.is_some() {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let new_hover = self.menu_item_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if new_hover != self.menu_hover {
                        self.menu_hover = new_hover;
                        self.request_main_paint();
                    }
                }
                // --- Ctrl+hover link tracking (cached on the hovered cell) ---
                self.update_link_hover(false);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // The last tab's shell can exit mid-pump (close_exited_tabs emptied
                // self.tabs), yet winit still delivers this iteration's queued
                // press; the grid-press branch calls active_tab() which panics on
                // an empty vec. Mirror the KeyboardInput/MouseWheel guards (F29).
                if self.tabs.is_empty() {
                    return;
                }
                let (w, h) = if let Some(gpu) = &self.gpu {
                    (gpu.config.width, gpu.config.height)
                } else {
                    return;
                };

                // While the Dropdown slide is animating, the scene is drawn shifted
                // by slide_y_offset but every hit-test uses the settled (unshifted)
                // coordinates — a press now would land on where surfaces WILL be,
                // not where they currently appear. Swallow presses until it settles
                // (~200ms); the user can click once the window is in place.
                if self.slide_anim.is_some() {
                    return;
                }

                // --- Hint mode / copy-mode ---
                // Hint mode is keyboard-only: swallow the click. Copy-mode exits
                // on a left press and lets the click fall through to the normal
                // mouse-selection path (predictable, simple).
                if self.hint_mode.is_some() {
                    return;
                }
                if self.copy_mode.is_some() {
                    self.active_tab_mut().terminal.selection_clear();
                    self.copy_mode = None;
                    self.request_main_paint();
                    // fall through to normal press handling
                }

                // --- Command palette captures the mouse while open ---
                // Swallow every click so none falls through to terminal selection,
                // the scrollbar, or the ? / window-control buttons (which is exactly
                // what would let another overlay open over the palette). A click on
                // a visible row runs it; a click outside the panel closes it.
                if self.palette_open {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let theme = self.current_theme();
                    let cw = self.chrome_char_w();
                    let first = self.palette_scroll;
                    let sel = self.palette_selected;
                    let total = self.palette_filtered.len();
                    let vis: Vec<(String, Vec<usize>, bool)> = self
                        .palette_filtered
                        .iter()
                        .enumerate()
                        .skip(first)
                        .take(jetty_render::MAX_PALETTE_ROWS)
                        .map(|(i, hh)| (hh.title.clone(), hh.indices.clone(), i == sel))
                        .collect();
                    let prows: Vec<jetty_render::PaletteRow> = vis
                        .iter()
                        .map(|(t, idx, s)| jetty_render::PaletteRow {
                            title: t,
                            match_indices: idx,
                            selected: *s,
                        })
                        .collect();
                    let pal = jetty_render::build_command_palette(
                        w, h, &theme, cw, &self.palette_query, &prows, total, first,
                    );
                    let mut hit: Option<usize> = None;
                    for (vi, r) in pal.row_hits.iter().enumerate() {
                        if input::point_in(r, cx, cy) {
                            hit = Some(first + vi);
                            break;
                        }
                    }
                    if let Some(gi) = hit {
                        let cmd = self.palette_filtered.get(gi).map(|hh| hh.cmd.clone());
                        self.close_palette();
                        if let Some(c) = cmd {
                            self.run_palette_cmd(c, event_loop);
                        }
                    } else if !input::point_in(&pal.panel, cx, cy) {
                        // Click on the dim backdrop closes; inside (non-row) is a no-op.
                        self.close_palette();
                    }
                    return;
                }

                // --- Quit confirmation popup is modal (highest priority) ---
                if self.confirm_quit {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let theme = self.current_theme();
                    let popup =
                        jetty_render::build_confirm(w, h, "Quit JeTTY? — all tabs will close", &theme, self.chrome_char_w());
                    if input::point_in(&popup.close_rect, cx, cy) {
                        event_loop.exit();
                        return;
                    } else if input::point_in(&popup.cancel_rect, cx, cy)
                        || !input::point_in(&popup.panel, cx, cy)
                    {
                        self.confirm_quit = false;
                    }
                    self.request_main_paint();
                    return;
                }

                // --- Close-tab confirmation popup is modal ---
                // Clicking Close confirms; Cancel or anywhere outside the panel
                // cancels. Either way the click is fully consumed.
                if let Some(i) = self.confirm_close {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let title = self.tabs.get(i).map(|t| t.title.clone()).unwrap_or_default();
                    let theme = self.current_theme();
                    let popup = jetty_render::build_confirm_close(w, h, &title, &theme, self.chrome_char_w());
                    if input::point_in(&popup.close_rect, cx, cy) {
                        self.confirm_close = None;
                        self.close_tab(i, event_loop);
                    } else if input::point_in(&popup.cancel_rect, cx, cy)
                        || !input::point_in(&popup.panel, cx, cy)
                    {
                        // Cancel button or click-outside cancels.
                        self.confirm_close = None;
                    }
                    self.request_main_paint();
                    return;
                }

                // --- Tab context menu hit-test (consume the click entirely) ---
                if let Some((_, _, tab_idx)) = self.tab_menu.take() {
                    self.tab_menu_hover = None;
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let hit = self.tab_menu_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    // Map the hit through the labels snapshotted at open time
                    // ("Detach" is present only when detaching was allowed).
                    let label = hit.and_then(|i| self.tab_menu_labels.get(i).copied());
                    self.tab_menu_labels.clear();
                    self.tab_menu_rects.clear();
                    if tab_idx < self.tabs.len() {
                        match label {
                            Some("Detach") => {
                                // Same flow as Ctrl+Shift+D, for THAT tab.
                                self.detach_tab(tab_idx, event_loop, None);
                            }
                            Some("Rename") => {
                                // Same inline-rename flow as double-click.
                                self.renaming = Some(tab_idx);
                                self.rename_buf = self.tabs[tab_idx].title.clone();
                            }
                            Some("Close Tab") => {
                                // Same confirm-close flow as the × / Ctrl+Shift+W.
                                self.confirm_close = Some(tab_idx);
                            }
                            _ => {}
                        }
                    }
                    // Hit or not, the menu is closed — consume the click.
                    self.request_main_paint();
                    return;
                }

                // --- Context menu hit-test (consume the click entirely) ---
                if self.context_menu.take().is_some() {
                    self.menu_hover = None;
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    // Reuse the cached item_rects built when the menu opened.
                    let hit = self.menu_item_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if let Some(idx) = hit {
                        match idx {
                            0 => {
                                // Copy — then clear the selection so the highlight
                                // doesn't linger after an explicit copy.
                                let copied = self
                                    .active_tab()
                                    .terminal
                                    .selection_text()
                                    .filter(|t| !t.is_empty());
                                if let Some(text) = copied {
                                    clipboard::set(&text);
                                    self.active_tab_mut().terminal.selection_clear();
                                    self.request_main_paint();
                                }
                            }
                            1 => {
                                // Paste
                                if let Some(text) = clipboard::get() {
                                    self.paste_text(&text);
                                }
                            }
                            2 => {
                                // Select All
                                self.active_tab_mut().terminal.select_all();
                            }
                            3 => {
                                // Clear — emulates Ctrl+L (form-feed 0x0C) sent to the active PTY.
                                // This is the same byte the Ctrl+L keybinding produces via
                                // ctrl_byte('L') in input.rs; reuse the same writer path.
                                self.active_tab_mut().terminal.scroll_to_bottom();
                                let w = &mut self.tabs[self.active].writer;
                                let _ = w.write_all(&[0x0C]);
                                let _ = w.flush();
                            }
                            4 => {
                                // Close Tab — mirrors the Ctrl+Shift+W handler: set confirm_close
                                // to open the confirmation popup (or close directly if no child).
                                // This reuses the exact same flow as KeyAction::CloseTab.
                                self.confirm_close = Some(self.active);
                            }
                            _ => {}
                        }
                    }
                    // Whether we hit an item or clicked outside, the menu is
                    // closed (Take above) — request a redraw and consume the click.
                    self.request_main_paint();
                    return;
                }

                let cx = self.cursor.0 as f32;
                let cy = self.cursor.1 as f32;

                // --- Help overlay is modal: a click outside its panel closes it;
                // a click inside is swallowed. Either way the click is consumed so
                // it never reaches the tab bar, a resize edge, or the terminal. ---
                if self.help_open {
                    let theme = self.current_theme();
                    let help = jetty_render::build_help_overlay(w, h, &theme, self.chrome_char_w(), &self.help_rows);
                    if !input::point_in(&help.panel, cx, cy) {
                        self.help_open = false;
                    }
                    self.request_main_paint();
                    return;
                }

                // --- Search bar (NON-modal for the mouse): ✕ closes+clears, a
                // click inside the panel is swallowed, clicks OUTSIDE fall
                // through so terminal selection keeps working. Built with the
                // SAME args as the draw call for hit parity. ---
                if self.search_open {
                    let theme = self.current_theme();
                    let (q, cur, total) = {
                        let t = &self.active_tab().terminal;
                        let (cur, total) = t.search_counter();
                        (t.search_query().to_string(), cur, total)
                    };
                    let bar = jetty_render::build_search_bar(
                        w, self.grid_top_offset(), &theme, self.chrome_char_w(), &q, cur, total,
                    );
                    if input::point_in(&bar.close_rect, cx, cy) {
                        self.search_close();
                        return;
                    }
                    if input::point_in(&bar.panel, cx, cy) {
                        return;
                    }
                }

                // --- Resize edges (borderless window): highest priority after the
                // modal context menu. Corners > edges; a press in a resize zone
                // starts an OS-driven resize and consumes the click so it never
                // begins a selection, tab-bar drag, or window move. ---
                let zone = resize_zone_at(cx, cy, w, h);
                if let Some(dir) = zone.direction() {
                    if let Some(win) = &self.window {
                        let _ = win.drag_resize_window(dir);
                    }
                    return;
                }

                // --- Tab bar / titlebar hit-test (only when the click is on the strip) ---
                // Window controls, tab switching/close/new, inline-rename, window
                // drag, and double-click-maximize — all BEFORE terminal selection.
                let bar_y = self.tabbar_y(h as f32);
                if cy >= bar_y && cy < bar_y + TABBAR_H {
                    // Detect a double-click on the strip (within ~400ms and ~5px).
                    let now = std::time::Instant::now();
                    let is_double = matches!(
                        self.last_strip_click,
                        Some((t, px, py))
                            if now.duration_since(t) <= std::time::Duration::from_millis(400)
                                && (cx - px).abs() <= 5.0
                                && (cy - py).abs() <= 5.0
                    );
                    self.last_strip_click = Some((now, cx, cy));

                    let theme = self.current_theme();
                    let tabs_meta: Vec<(String, bool)> = self
                        .tabs
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (t.title.clone(), i == self.active))
                        .collect();
                    let rename_ref = self.renaming.map(|i| (i, self.rename_buf.as_str()));
                    // Build with perf=None to MATCH the drawn bar (the perf HUD
                    // moved to the bottom status strip, so the drawn tab bar
                    // reserves no HUD width — see the RedrawRequested build). Passing
                    // self.perf_label here reserved ~250px of phantom width and
                    // shrank the hit tab_w below the drawn tab_w, so clicks near a
                    // tab's right edge / on ✕ / on + landed on the wrong tab (F19).
                    let mut bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None, None, self.chrome_char_w(),
                        &[], // activity never affects geometry; keeps hit rects == drawn rects
                    );
                    // build_tab_bar_ex lays the bar out at y 0..TABBAR_H; shift its
                    // hit-test rects down to the bar's actual position (bottom mode).
                    if bar_y != 0.0 {
                        translate_bar_rects(&mut bar, bar_y);
                    }

                    // Window controls take priority (rightmost region).
                    if input::point_in(&bar.help_rect, cx, cy) {
                        // Toggle the in-window Help overlay. Opening it closes the
                        // context menu so the two overlays are mutually exclusive.
                        self.help_open = !self.help_open;
                        if self.help_open {
                            self.context_menu = None;
                            self.menu_hover = None;
                        }
                        self.request_main_paint();
                        return;
                    }
                    if input::point_in(&bar.settings_rect, cx, cy) {
                        // Same as Ctrl+Shift+P: open/close the Settings window.
                        self.toggle_settings_window(event_loop);
                        return;
                    }
                    if input::point_in(&bar.close_rect, cx, cy) {
                        // Confirm before quitting the whole app (closes every tab).
                        self.confirm_quit = true;
                        self.request_main_paint();
                        return;
                    }
                    if input::point_in(&bar.max_rect, cx, cy) {
                        if let Some(win) = &self.window {
                            win.set_maximized(!win.is_maximized());
                        }
                        return;
                    }
                    if input::point_in(&bar.min_rect, cx, cy) {
                        if let Some(win) = &self.window {
                            win.set_minimized(true);
                        }
                        // Some WMs don't send Occluded on iconify — mark it here
                        // too so animations stop immediately (F17). Restoring the
                        // window delivers Focused/Occluded(false), which clears it.
                        self.main_occluded = true;
                        return;
                    }

                    // A click anywhere on the strip commits an in-progress rename
                    // unless it lands on the tab being renamed (handled below).
                    let renaming_idx = self.renaming;

                    // Close buttons take priority over the tab body they sit on.
                    if let Some(i) = bar
                        .close_rects
                        .iter()
                        .position(|r| input::point_in(r, cx, cy))
                    {
                        self.commit_rename();
                        // Ask before closing instead of closing immediately.
                        self.confirm_close = Some(i);
                        self.request_main_paint();
                        return;
                    }
                    if input::point_in(&bar.plus_rect, cx, cy) {
                        self.commit_rename();
                        self.new_tab();
                        return;
                    }
                    if let Some(i) = bar
                        .tab_rects
                        .iter()
                        .position(|r| input::point_in(r, cx, cy))
                    {
                        // Double-click on a tab → enter inline rename. But a
                        // double-click on the tab ALREADY being renamed must not
                        // reset the in-progress edit buffer (it would discard the
                        // user's typing); leave the rename untouched.
                        if is_double && self.renaming != Some(i) {
                            self.renaming = Some(i);
                            self.rename_buf = self.tabs[i].title.clone();
                            self.last_strip_click = None;
                            self.request_main_paint();
                            return;
                        }
                        if is_double {
                            // Already renaming this tab: swallow the click without
                            // disturbing the buffer.
                            self.last_strip_click = None;
                            return;
                        }
                        // Single click on a different tab commits any rename.
                        if renaming_idx != Some(i) {
                            self.commit_rename();
                        }
                        // Select immediately (a plain click), and ARM the
                        // drag-out gesture: if the cursor leaves the strip by
                        // more than TEAR_THRESHOLD_PX before release, the drag
                        // becomes a tear-out and the release detaches this tab.
                        self.select_tab(i);
                        self.tab_drag = Some(TabDrag { idx: i, tearing: false });
                        return;
                    }

                    // Empty strip space: commit any rename, then either maximize
                    // (double-click) or start an OS window move (single press).
                    self.commit_rename();
                    if is_double {
                        if let Some(win) = &self.window {
                            win.set_maximized(!win.is_maximized());
                        }
                        self.last_strip_click = None;
                    } else if let Some(win) = &self.window {
                        let _ = win.drag_window();
                    }
                    return;
                }
                // A click in the terminal area commits any in-progress rename.
                self.commit_rename();
                // A click in the grid area dismisses the welcome splash.
                if self.welcome_open {
                    self.welcome_open = false;
                    self.request_main_paint();
                }

                let rows = self.active_tab().terminal.rows();
                let scroll_offset = self.active_tab().terminal.scroll_offset();
                let scroll_max = self.active_tab().terminal.scroll_max();
                // Color is irrelevant for hit-test geometry; pass transparent.
                let scrollbar = jetty_render::scrollbar_rect_geom(rows, scroll_offset, scroll_max, w, h, self.grid_top_offset(), self.status_h(), [0, 0, 0, 0]);

                // The settings panel no longer lives in this window, so pass no
                // panel geometry — only the scrollbar and terminal area are hit.
                match input::decide_mouse_press(
                    None,
                    scrollbar.as_ref(),
                    cx,
                    cy,
                ) {
                    // Panel actions cannot occur here (panel == None above).
                    input::MouseAction::StartSliderDrag
                    | input::MouseAction::StartRadiusDrag
                    | input::MouseAction::SetTheme(_)
                    | input::MouseAction::ToggleThemeDropdown
                    | input::MouseAction::ThemeScrollUp
                    | input::MouseAction::ThemeScrollDown
                    | input::MouseAction::FontMinus
                    | input::MouseAction::FontPlus
                    | input::MouseAction::FontReset
                    | input::MouseAction::SetFont(_)
                    | input::MouseAction::FontScrollUp
                    | input::MouseAction::FontScrollDown
                    | input::MouseAction::UiFontMinus
                    | input::MouseAction::UiFontPlus
                    | input::MouseAction::UiFontReset
                    | input::MouseAction::SetUiFont(_)
                    | input::MouseAction::UiFontScrollUp
                    | input::MouseAction::UiFontScrollDown
                    | input::MouseAction::SummonPrev
                    | input::MouseAction::SummonNext
                    | input::MouseAction::WinModePrev
                    | input::MouseAction::WinModeNext
                    | input::MouseAction::TabBarPrev
                    | input::MouseAction::TabBarNext
                    | input::MouseAction::ScrollbackPrev
                    | input::MouseAction::ScrollbackNext
                    | input::MouseAction::StartDropdownDrag
                    | input::MouseAction::StartDropdownWidthDrag
                    | input::MouseAction::ToggleFocusAutoHide
                    | input::MouseAction::ToggleLaunchAtLogin
                    | input::MouseAction::CycleShellPrev
                    | input::MouseAction::CycleShellNext
                    | input::MouseAction::ToggleNotifyOnFinish
                    | input::MouseAction::ToggleNotifyOnlyFailure
                    | input::MouseAction::NotifyDurPrev
                    | input::MouseAction::NotifyDurNext
                    | input::MouseAction::ToggleAutoSummon
                    | input::MouseAction::ToggleCrt
                    | input::MouseAction::ToggleCrtRoll
                    | input::MouseAction::ToggleCrtFlicker
                    | input::MouseAction::ToggleCrtJitter
                    | input::MouseAction::ToggleCaretFlash
                    | input::MouseAction::ToggleCaretGlow
                    | input::MouseAction::StartCrtCurvatureDrag
                    | input::MouseAction::StartScanlineDrag
                    | input::MouseAction::StartMaskDrag
                    | input::MouseAction::StartBloomDrag
                    | input::MouseAction::StartChromaticDrag
                    | input::MouseAction::StartVignetteDrag
                    | input::MouseAction::StartCaretDurDrag
                    | input::MouseAction::StartTintRDrag
                    | input::MouseAction::StartTintGDrag
                    | input::MouseAction::StartTintBDrag
                    | input::MouseAction::StartCaretColorRDrag
                    | input::MouseAction::StartCaretColorGDrag
                    | input::MouseAction::StartCaretColorBDrag
                    | input::MouseAction::SetSettingsTab(_)
                    | input::MouseAction::StartDialogDrag
                    | input::MouseAction::ConsumePanel => {}
                    input::MouseAction::StartScrollbarDrag { grab_dy } => {
                        self.dragging_scrollbar = true;
                        self.drag_grab_dy = grab_dy;
                    }
                    input::MouseAction::ScrollbarTrackJump => {
                        self.dragging_scrollbar = true;
                        self.drag_grab_dy = scrollbar.map(|r| r.h / 2.0).unwrap_or(0.0);
                        self.apply_scroll_from_cursor(w, h);
                        self.request_main_paint();
                    }
                    input::MouseAction::None => {
                        // Ctrl+click (also Cmd+click on macOS) on a detected link
                        // opens it and consumes the click entirely: no mouse report
                        // (our SGR encoder carries no modifier bits anyway) and no
                        // selection start. Shift still wins for selection, so
                        // Ctrl+Shift+click/drag is unchanged. Recomputed at press
                        // time — a click without a prior hover move still works.
                        // Gated on the SAME grid band as update_link_hover:
                        // cursor_cell_0_side CLAMPS into the grid, so a click on
                        // the status strip (or bottom-mode tab bar) would open a
                        // bottom-row URL no underline ever advertised (F13).
                        if link_modifier_held(&self.modifiers) && !self.modifiers.shift_key() {
                            let grid_bottom = if self.tab_bar_bottom {
                                self.tabbar_y(h as f32)
                            } else {
                                h as f32 - self.status_h()
                            };
                            let in_grid =
                                cy >= self.grid_top_offset() && cy < grid_bottom;
                            if in_grid {
                                if let Some((line, col, _)) = self.cursor_cell_0_side() {
                                    if let Some(hit) =
                                        self.active_tab().terminal.link_at(line, col)
                                    {
                                        Self::open_url(&hit.uri);
                                        return;
                                    }
                                }
                            }
                        }
                        // The click landed in the terminal area (not a panel or
                        // scrollbar widget). When the app enabled mouse reporting,
                        // forward the press; otherwise start a text selection.
                        //
                        // Holding Shift OVERRIDES mouse reporting and forces a local
                        // text selection — the standard terminal convention (Konsole/
                        // xterm/kitty) so you can still select & copy inside TUIs that
                        // grab the mouse (Claude Code, vim, htop, tmux).
                        if self.active_tab().terminal.mouse_mode() && !self.modifiers.shift_key() {
                            // Remember where this app-bound press started: if the
                            // user drags (not just clicks), they were probably trying
                            // to select — we surface the Shift+drag hint on release.
                            self.mouse_grab_press = Some(self.cursor);
                            self.send_mouse_report(input::MouseEvent::LeftPress);
                        } else {
                            // Clear prior selection and begin a new one.
                            self.active_tab_mut().terminal.selection_clear();
                            if let Some((line, col, left_half)) = self.cursor_cell_0_side() {
                                self.active_tab_mut().terminal.selection_start(line, col, left_half);
                            }
                            self.selecting = true;
                            self.request_main_paint();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Modal gates (F38): unlike the Left arm (and the v0.10 Middle
                // arm), the Right arm used to open its menu on top of a modal
                // dialog, mid-summon-slide, or after the last tab exited. Consume
                // the click while any modal is up / the scene is sliding / tabs is
                // empty, so no menu appears over a quit/close-confirm or the help,
                // and no menu opens at coordinates the slide has shifted.
                if self.slide_anim.is_some()
                    || self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.help_open
                    || self.palette_open
                    || self.tabs.is_empty()
                {
                    return;
                }
                // Right-click: open the context menu (Copy / Paste / Select All).
                // Settings now live in a separate window, so the main terminal is
                // always free to show its context menu.
                let cx = self.cursor.0 as f32;
                let cy = self.cursor.1 as f32;
                // A right-click on the tab bar must NOT open the terminal Copy/
                // Paste menu (the strip has its own affordances): a right-click
                // ON A TAB opens the tab context menu (Detach / Rename / Close
                // Tab); empty strip space stays a no-op.
                let bar_y = if let Some(gpu) = &self.gpu {
                    self.tabbar_y(gpu.config.height as f32)
                } else {
                    0.0
                };
                if cy >= bar_y && cy < bar_y + TABBAR_H {
                    let Some(gpu) = &self.gpu else { return };
                    let (w, h) = (gpu.config.width, gpu.config.height);
                    let theme = self.current_theme();
                    // Rebuild the bar for hit-testing exactly like the left-press
                    // handler (same tabs/rename/HUD inputs → identical rects).
                    let tabs_meta: Vec<(String, bool)> = self
                        .tabs
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (t.title.clone(), i == self.active))
                        .collect();
                    let rename_ref = self.renaming.map(|i| (i, self.rename_buf.as_str()));
                    // perf=None to match the DRAWN bar (HUD lives in the status
                    // strip); passing perf_label mis-sized the hit-rects (F19).
                    let mut bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None, None, self.chrome_char_w(),
                        &[], // activity never affects geometry; keeps hit rects == drawn rects
                    );
                    if bar_y != 0.0 {
                        translate_bar_rects(&mut bar, bar_y);
                    }
                    if let Some(i) = bar
                        .tab_rects
                        .iter()
                        .position(|r| input::point_in(r, cx, cy))
                    {
                        // Close the other overlays so the menu can't be orphaned
                        // under them (mutually exclusive with the terminal menu).
                        self.commit_rename();
                        self.help_open = false;
                        self.context_menu = None;
                        self.menu_hover = None;
                        self.tab_menu = Some((cx, cy, i));
                        self.tab_menu_hover = None;
                        self.tab_menu_labels = crate::detached::tab_menu_items(
                            crate::detached::can_detach(self.tabs.len()),
                        );
                        // Cache the item hit-test rects once, same as context_menu.
                        let items: Vec<(&str, &str)> = self
                            .tab_menu_labels
                            .iter()
                            .map(|&l| (l, crate::detached::menu_hint(l)))
                            .collect();
                        let menu = jetty_render::build_menu(
                            cx, cy, w, h, None, &theme, self.chrome_char_w(), &items, &[],
                        );
                        self.tab_menu_rects = menu.item_rects;
                        self.request_main_paint();
                    }
                    return;
                }
                // Commit any in-progress rename and close the help overlay so the
                // menu can't be orphaned under it. The tab menu is mutually
                // exclusive with the terminal menu.
                self.commit_rename();
                self.help_open = false;
                self.tab_menu = None;
                self.tab_menu_hover = None;
                self.tab_menu_rects.clear();
                self.tab_menu_labels.clear();
                self.context_menu = Some((cx, cy));
                self.menu_hover = None;
                // Cache the item hit-test rects once (anchor + size fixed for the
                // menu's lifetime) so CursorMoved hover doesn't rebuild the menu.
                if let Some(gpu) = &self.gpu {
                    let theme = self.current_theme();
                    let menu = jetty_render::build_context_menu(
                        cx, cy, gpu.config.width, gpu.config.height, None, &theme, self.chrome_char_w(),
                    );
                    self.menu_item_rects = menu.item_rects;
                }
                self.request_main_paint();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // --- Tab drag-out release ---
                // A release while TEARING detaches that tab into a new window at
                // the release cursor's global position (main outer position +
                // local cursor; None on Wayland → default placement). A release
                // that never tore (a plain click) already selected the tab on
                // press — just clear the drag and fall through.
                if let Some(drag) = self.tab_drag.take() {
                    if drag.tearing {
                        if let Some(win) = &self.window {
                            win.set_cursor(winit::window::CursorIcon::Default);
                        }
                        let drop_global = self
                            .window
                            .as_ref()
                            .and_then(|w| w.outer_position().ok())
                            .map(|p| (p.x as f64 + self.cursor.0, p.y as f64 + self.cursor.1));
                        self.detach_tab(drag.idx, event_loop, drop_global);
                        return;
                    }
                }
                // If we were dragging the scrollbar, the release just ends that
                // drag and is never forwarded to the app. (Slider drags happen in
                // the settings window now.)
                let was_dragging = self.dragging_scrollbar;
                self.dragging_scrollbar = false;
                // Capture before the block below clears it: a release that ended a
                // local selection must NOT also emit a mouse report to the app (the
                // matching press was never forwarded — e.g. a Shift-drag selection
                // over a mouse-mode TUI).
                let was_selecting = self.selecting;

                // End text selection and copy-on-select.
                if self.selecting {
                    self.selecting = false;
                    // Copy-on-select: if we got any text, put it in the clipboard.
                    if let Some(text) = self.active_tab().terminal.selection_text() {
                        if !text.is_empty() {
                            clipboard::set(&text);
                        } else {
                            // Empty drag (plain click) — clear the selection highlight.
                            self.active_tab_mut().terminal.selection_clear();
                        }
                    } else {
                        // No selection text → plain click, clear selection.
                        self.active_tab_mut().terminal.selection_clear();
                    }
                    self.request_main_paint();
                }

                // The press marker is set ONLY when the matching press was
                // actually forwarded to the app (terminal-area hit, mouse mode
                // on). Take it unconditionally so it never goes stale.
                let grab_press = self.mouse_grab_press.take();

                // Forward a release report only when the app enabled mouse mode,
                // this release did not terminate a host-widget drag, AND the
                // matching press WAS forwarded (grab_press). Presses consumed by
                // chrome — tab titles, +/help/gear buttons, popups, menu
                // dismissals — early-return before forwarding, and an unmatched
                // release would register as a phantom click in apps that act on
                // button-up (X10 mode even encodes it as button 3).
                if !was_dragging
                    && !was_selecting
                    && grab_press.is_some()
                    && self.active_tab().terminal.mouse_mode()
                {
                    self.send_mouse_report(input::MouseEvent::LeftRelease);
                }

                // If this release ended a no-Shift DRAG over a mouse-reporting app
                // (press recorded, cursor moved > a few px), the user was likely
                // trying to select — they just don't know Shift is needed. Surface
                // a brief, throttled toast telling them how.
                if let Some((px, py)) = grab_press {
                    let moved = ((self.cursor.0 - px).powi(2) + (self.cursor.1 - py).powi(2)).sqrt();
                    let now = std::time::Instant::now();
                    let off_cooldown = self.shift_hint_cooldown.is_none_or(|t| now >= t);
                    if moved > 8.0 && off_cooldown {
                        if let Some(win) = &self.window {
                            // Tagged with the MAIN window's id: only this
                            // window draws the pill (F4).
                            self.shift_hint_until = Some((
                                now + std::time::Duration::from_millis(3500),
                                win.id(),
                            ));
                            self.shift_hint_cooldown =
                                Some(now + std::time::Duration::from_secs(25));
                            self.request_main_paint();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Middle, .. } => {
                // Middle-click paste (X11 primary-selection idiom). Unlike the
                // Left arm this used to skip every gate, so it pasted into a
                // shell hidden behind a modal popup and over the tab bar. Honor
                // the same modal/hit checks:
                //  - any modal open (slide/confirm/help/menus) → swallow;
                //  - only paste over the terminal grid, never the chrome strips;
                //  - when the app grabbed the mouse (mouse_mode) and Shift is not
                //    held, the button belongs to the app — do NOT inject a paste.
                if self.slide_anim.is_some()
                    || self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.help_open
                    || self.palette_open
                    || self.context_menu.is_some()
                    || self.tab_menu.is_some()
                    || self.tabs.is_empty()
                {
                    return;
                }
                let height = self.gpu.as_ref().map(|g| g.config.height as f32);
                let Some(height) = height else { return };
                let cy = self.cursor.1 as f32;
                let grid_top = self.grid_top_offset();
                let grid_bottom = if self.tab_bar_bottom {
                    self.tabbar_y(height)
                } else {
                    height - self.status_h()
                };
                if cy < grid_top || cy >= grid_bottom {
                    return;
                }
                if self.active_tab().terminal.mouse_mode() && !self.modifiers.shift_key() {
                    return;
                }
                if let Some(text) = clipboard::get() {
                    self.paste_text(&text);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // The last tab's shell can exit mid-pump (close_exited_tabs
                // emptied self.tabs), yet winit still delivers this iteration's
                // queued wheel events; active_tab() would panic on an empty vec.
                // Mirror the KeyboardInput/Ime guards (F29).
                if self.tabs.is_empty() {
                    return;
                }
                // Hint mode / copy-mode own the wheel: swallow it so scrolling
                // does not slide the labelled tokens out from under their chips
                // (hint) or desync the keyboard cursor/selection from the content
                // (copy — use k/j/Ctrl+u/d to move within the mode instead).
                if self.hint_mode.is_some() || self.copy_mode.is_some() {
                    return;
                }
                // The palette owns the wheel while open: scroll its list, never
                // the terminal underneath (swallow so nothing falls through).
                if self.palette_open {
                    let step = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -(y.round() as isize),
                        MouseScrollDelta::PixelDelta(p) => {
                            if p.y > 0.0 { -1 } else if p.y < 0.0 { 1 } else { 0 }
                        }
                    };
                    if step != 0 {
                        self.palette_move(step);
                        self.request_main_paint();
                    }
                    return;
                }
                // Positive y = wheel up = scroll into history (older output).
                // Deltas are ACCUMULATED (fractionally) across events: slow
                // touchpad scrolling arrives as many sub-line deltas that a
                // per-event round() discarded entirely — the accumulator emits
                // whole lines and carries the remainder, so gentle scrolls move
                // both the scrollback and mouse-mode apps.
                let delta_lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                    MouseScrollDelta::PixelDelta(p) => {
                        // Approximate cell height; use 20.0 as a reasonable default.
                        const CELL_H: f64 = 20.0;
                        (p.y / CELL_H) as f32
                    }
                };
                let lines = self.scroll_accum.add(delta_lines);
                if lines != 0 {
                    // When the app enabled mouse reporting, forward wheel events
                    // as SGR button 64 (up) / 65 (down) — but only over the
                    // terminal area, so wheeling over the scrollbar still scrolls
                    // the host scrollback. One report per LineDelta notch
                    // (clamped) keeps apps like less/htop responsive without
                    // flooding the PTY.
                    let grid_top = self.grid_top_offset();
                    let status_h = self.status_h();
                    let over_scrollbar = {
                        let rows = self.active_tab().terminal.rows();
                        let off = self.active_tab().terminal.scroll_offset();
                        let max = self.active_tab().terminal.scroll_max();
                        if let Some(gpu) = &self.gpu {
                            let (w, h) = (gpu.config.width, gpu.config.height);
                            jetty_render::scrollbar_rect_geom(rows, off, max, w, h, grid_top, status_h, [0, 0, 0, 0])
                                .map(|r| {
                                    let cx = self.cursor.0 as f32;
                                    cx >= r.x && cx <= r.x + r.w
                                })
                                .unwrap_or(false)
                        } else {
                            false
                        }
                    };

                    if self.active_tab().terminal.mouse_mode() && !over_scrollbar {
                        let event = if lines > 0 {
                            input::MouseEvent::WheelUp
                        } else {
                            input::MouseEvent::WheelDown
                        };
                        // Emit a bounded number of reports proportional to the
                        // scroll magnitude (one per ~3 lines, i.e. per notch).
                        let notches = ((lines.abs() + 2) / 3).clamp(1, 8);
                        for _ in 0..notches {
                            self.send_mouse_report(event);
                        }
                    } else if !over_scrollbar
                        && self.active_tab().terminal.alt_screen()
                        && self.active_tab().terminal.alternate_scroll()
                    {
                        // ALTERNATE_SCROLL (F3): alt-screen pagers/editors
                        // (less/man/git log) have no host scrollback, so a bare
                        // scroll_lines() is a no-op. Translate wheel ticks into
                        // Up/Down arrow-key sequences (DECCKM-aware), one arrow
                        // per line of scroll, bounded so a big touchpad fling
                        // can't flood the PTY.
                        let app_cursor = self.active_tab().terminal.app_cursor_keys();
                        let seq = input::arrow_scroll_bytes(lines > 0, app_cursor);
                        let steps = (lines.unsigned_abs() as usize).clamp(1, 12);
                        let w = &mut self.tabs[self.active].writer;
                        for _ in 0..steps {
                            let _ = w.write_all(&seq);
                        }
                        let _ = w.flush();
                    } else {
                        self.active_tab_mut().terminal.scroll_lines(lines);
                        self.request_main_paint();
                        // The viewport moved under a stationary pointer: the
                        // hovered CELL is unchanged but its content is not.
                        self.update_link_hover(true);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } if event.state.is_pressed() => {
                // X11 synthesizes PRESSED events for every key physically held
                // the moment a window gains focus (e.g. the F9 summon key, or
                // Tab during an Alt+Tab switch). Those keys were never typed at
                // this window — ignore them so no garbage reaches the PTY.
                if is_synthetic {
                    return;
                }
                // The last tab's shell can exit mid-pump (close_exited_tabs
                // emptied self.tabs and called event_loop.exit()), yet winit
                // still delivers queued key events this iteration. active_tab()
                // and self.tabs[self.active] would panic on an empty vec — bail
                // (mirrors the Ime::Commit / RedrawRequested guards).
                if self.tabs.is_empty() {
                    return;
                }
                // --- Quit confirmation popup captures Enter / Esc (highest priority) ---
                if self.confirm_quit {
                    use winit::keyboard::{Key, NamedKey};
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            event_loop.exit();
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.confirm_quit = false;
                            self.request_main_paint();
                            return;
                        }
                        _ => return,
                    }
                }

                // --- Close-tab confirmation popup captures Enter / Esc ---
                // While the popup is open it is modal: Enter confirms the close,
                // Esc cancels. Both are fully consumed so they never reach the
                // shell, close the help, or fall through to other handlers.
                if let Some(i) = self.confirm_close {
                    use winit::keyboard::{Key, NamedKey};
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.confirm_close = None;
                            self.close_tab(i, event_loop);
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.confirm_close = None;
                            self.context_menu = None;
                            self.menu_hover = None;
                            self.request_main_paint();
                            return;
                        }
                        // Swallow every other key while the popup is open.
                        _ => return,
                    }
                }
                // --- Inline tab rename captures all keys ---
                // While renaming, keys edit the title buffer and never reach the
                // PTY: printable chars append, Backspace pops, Enter commits,
                // Escape cancels. Return early so nothing leaks to the shell.
                if let Some(i) = self.renaming {
                    use winit::keyboard::{Key, NamedKey};
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.commit_rename();
                        }
                        Key::Named(NamedKey::Escape) => {
                            // Cancel: keep the old title.
                            self.renaming = None;
                            self.rename_buf.clear();
                            self.context_menu = None;
                            self.menu_hover = None;
                            self.request_main_paint();
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.rename_buf.pop();
                            self.request_main_paint();
                        }
                        _ => {
                            // Append any printable text the key produced.
                            if let Some(t) = &event.text {
                                for ch in t.chars() {
                                    if !ch.is_control() {
                                        self.rename_buf.push(ch);
                                    }
                                }
                                self.request_main_paint();
                            }
                        }
                    }
                    // Defensive: keep `i` referenced so the renaming index is valid.
                    let _ = i;
                    return;
                }
                // --- Command palette captures ALL keys while open ---
                // Sits above welcome/help/search: once open the palette owns the
                // keyboard (single-overlay-owns-keys). Type → query, Up/Down →
                // select, PageUp/Down → page, Enter → run + close, Esc → close,
                // Backspace → edit, Ctrl+Shift+P / Cmd+Shift+P → toggle closed.
                // Every other Ctrl/Cmd chord is swallowed so nothing leaks.
                if self.palette_open {
                    use winit::keyboard::{Key, NamedKey};
                    let ctrl = self.modifiers.control_key();
                    let shift = self.modifiers.shift_key();
                    let alt = self.modifiers.alt_key();
                    let sup = self.modifiers.super_key();
                    // Toggle closed on the SAME chord that opens the palette — routed
                    // through the keymap so a remapped `open_palette` toggles closed
                    // consistently (amendment 4).
                    let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
                    let is_palette_chord = self
                        .keymap
                        .lookup(mods, event.physical_key, &event.logical_key)
                        == Some(input::KeyAction::OpenPalette);
                    if is_palette_chord {
                        self.close_palette();
                        return;
                    }
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.close_palette(),
                        Key::Named(NamedKey::Enter) => {
                            let cmd = self
                                .palette_filtered
                                .get(self.palette_selected)
                                .map(|h| h.cmd.clone());
                            self.close_palette();
                            if let Some(c) = cmd {
                                self.run_palette_cmd(c, event_loop);
                            }
                            return;
                        }
                        Key::Named(NamedKey::ArrowDown) => self.palette_move(1),
                        Key::Named(NamedKey::ArrowUp) => self.palette_move(-1),
                        Key::Named(NamedKey::PageDown) => {
                            self.palette_move(jetty_render::MAX_PALETTE_ROWS as isize)
                        }
                        Key::Named(NamedKey::PageUp) => {
                            self.palette_move(-(jetty_render::MAX_PALETTE_ROWS as isize))
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.palette_query.pop();
                            self.refilter_palette();
                        }
                        _ => {
                            if ctrl || sup {
                                // Swallow other chords while the palette owns keys.
                            } else if let Some(t) = &event.text {
                                let mut changed = false;
                                for ch in t.chars() {
                                    if !ch.is_control() {
                                        self.palette_query.push(ch);
                                        changed = true;
                                    }
                                }
                                if changed {
                                    self.refilter_palette();
                                }
                            }
                        }
                    }
                    self.request_main_paint();
                    return;
                }
                // --- Hint mode captures ALL keys while active ---
                // Sits below the palette (single-overlay-owns-keys): letters
                // narrow the label prefix, Esc cancels, Backspace pops, the same
                // chord toggles closed, everything else is swallowed.
                if self.hint_mode.is_some() {
                    let ctrl = self.modifiers.control_key();
                    let shift = self.modifiers.shift_key();
                    let alt = self.modifiers.alt_key();
                    let sup = self.modifiers.super_key();
                    let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
                    if self.keymap.lookup(mods, event.physical_key, &event.logical_key)
                        == Some(input::KeyAction::HintMode)
                    {
                        self.exit_hint_mode();
                        return;
                    }
                    self.hint_mode_key(event.physical_key, &event.logical_key);
                    return;
                }
                // --- Copy-mode captures ALL keys while active ---
                if self.copy_mode.is_some() {
                    let ctrl = self.modifiers.control_key();
                    let shift = self.modifiers.shift_key();
                    let alt = self.modifiers.alt_key();
                    let sup = self.modifiers.super_key();
                    let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
                    if self.keymap.lookup(mods, event.physical_key, &event.logical_key)
                        == Some(input::KeyAction::CopyMode)
                    {
                        self.active_tab_mut().terminal.selection_clear();
                        self.exit_copy_mode();
                        return;
                    }
                    self.copy_mode_key(event.physical_key, &event.logical_key, ctrl);
                    return;
                }
                // --- Welcome splash captures Escape (dismiss only, non-modal) ---
                // Esc dismisses the welcome splash without consuming the key further
                // (it still falls through to the help/PTY path so the shell also
                // sees the ESC byte, which is the normal behaviour for Esc → PTY).
                if self.welcome_open
                    && matches!(
                        event.logical_key,
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    )
                {
                    self.welcome_open = false;
                    // Don't return — let Esc continue through to the PTY path.
                }
                // --- Help overlay captures Escape ---
                // When the help overlay is open, Escape closes it and is fully
                // consumed: it must NOT also close a tab or reach the shell.
                if self.help_open
                    && matches!(
                        event.logical_key,
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    )
                {
                    self.help_open = false;
                    self.context_menu = None;
                    self.menu_hover = None;
                    self.request_main_paint();
                    return;
                }
                // --- Scrollback-search bar captures all keys while open ---
                // (after the help-Esc block, so help keeps Esc priority).
                // Printable keys edit the query incrementally; Enter/F3 step
                // older, Shift+Enter/Shift+F3 newer; Backspace pops; Esc /
                // Ctrl+Shift+F close and CLEAR (no query retention). Every
                // other Ctrl/Cmd chord is swallowed (alacritty-style) so
                // nothing leaks to the shell while the bar owns the keyboard.
                if self.search_open {
                    use winit::keyboard::{Key, NamedKey};
                    let ctrl = self.modifiers.control_key();
                    let shift = self.modifiers.shift_key();
                    let alt = self.modifiers.alt_key();
                    let sup = self.modifiers.super_key();
                    // Close on the SAME chord that toggles search — routed through the
                    // keymap so a remapped `search_toggle` closes consistently (amend. 4).
                    let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
                    let chord_action = self
                        .keymap
                        .lookup(mods, event.physical_key, &event.logical_key);
                    if chord_action == Some(input::KeyAction::SearchToggle) {
                        self.search_close();
                        return;
                    }
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.search_close();
                        }
                        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::F3) => {
                            // Matches stale after a throttled streaming burst?
                            // Re-collect FIRST so navigation steps real Points
                            // instead of scrolling to rotated rows (F10).
                            if self.search_dirty {
                                self.search_dirty = false;
                                self.active_tab_mut().terminal.search_refresh();
                                self.search_refresh_at = Some(std::time::Instant::now());
                            }
                            // Enter/F3 = older (up through history); +Shift = newer.
                            self.active_tab_mut().terminal.search_nav(!shift);
                        }
                        Key::Named(NamedKey::Backspace) => {
                            let mut q = self.active_tab().terminal.search_query().to_string();
                            q.pop();
                            self.active_tab_mut().terminal.search_set_query(&q);
                        }
                        _ => {
                            // Paste-into-query on the SAME chord the keymap maps to
                            // Paste (Ctrl+Shift+V / Shift+Insert / macOS Cmd+V, or a
                            // remap), so a remapped paste works in the search bar too.
                            let is_paste = chord_action == Some(input::KeyAction::Paste);
                            if is_paste {
                                if let Some(text) = clipboard::get() {
                                    let mut q =
                                        self.active_tab().terminal.search_query().to_string();
                                    q.extend(text.chars().filter(|c| !c.is_control()));
                                    self.active_tab_mut().terminal.search_set_query(&q);
                                }
                            } else if ctrl || sup {
                                // Swallow other Ctrl/Cmd chords while the bar is open.
                            } else if let Some(t) = &event.text {
                                let mut q =
                                    self.active_tab().terminal.search_query().to_string();
                                q.extend(t.chars().filter(|c| !c.is_control()));
                                self.active_tab_mut().terminal.search_set_query(&q);
                            }
                        }
                    }
                    self.request_main_paint();
                    return;
                }
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let alt = self.modifiers.alt_key();
                // macOS Cmd (winit `super_key()`) chords are now folded into the
                // keymap (Copy/Paste/SelectAll/NewTab/CloseTab/Quit/OpenPalette/
                // font/settings, Shift-agnostic) and dispatched through the SAME
                // action path below. The old inline Cmd block is gone; decide_key's
                // keymap lookup preserves the "swallow unmapped Cmd" safety net so
                // nothing leaks to the PTY.
                let sup = self.modifiers.super_key();
                let app_cursor = self.active_tab().terminal.app_cursor_keys();
                let alt_screen = self.active_tab().terminal.alt_screen();
                // Escape in the main window never closes the settings window
                // (that window handles its own Escape), so panel_open is always
                // false here — Escape forwards an ESC byte to the PTY as normal.
                // macOS Option-compose (no OS gating — keyed on what the OS
                // produced): Option is the primary compose key (Option+G → ©,
                // Option+U U → ü). When Alt is held and the OS composed a printable
                // NON-ASCII glyph in `event.text`, send that glyph to the PTY
                // instead of letting decide_key ESC-prefix it (the Meta
                // convention). Alt+ASCII stays Meta (ESC b for word-back, etc.),
                // and Linux Alt+letter — which produces no composed non-ASCII text —
                // is unaffected. (Dead-key sequences routed via Ime::Commit instead
                // of event.text are a separate, larger path — deferred.)
                let composed: Option<Vec<u8>> = if alt && !ctrl {
                    event.text.as_ref().and_then(|t| {
                        if !t.is_empty()
                            && t.chars().all(|c| !c.is_control())
                            && !t.is_ascii()
                        {
                            Some(t.as_bytes().to_vec())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                // Dead-key composition fallback: when a compose sequence puts
                // the composed glyph in `event.text` (e.g. ' then e → "é") while
                // logical_key still reports the base char, prefer the text —
                // otherwise the accent is silently dropped. Never fires for
                // Ctrl/Alt chords or Named keys (see dead_key_text_override).
                // GATED on `!sup`: a bare Cmd chord must route through decide_key's
                // keymap+swallow, never be sent as composed text (byte-identical
                // with the old Cmd block, which returned before this path).
                let dead_key = if sup {
                    None
                } else {
                    input::dead_key_text_override(ctrl, alt, &event.logical_key, event.text.as_deref())
                };
                let action = match composed.or(dead_key) {
                    Some(bytes) => input::KeyAction::Send(bytes),
                    None => input::decide_key(&self.keymap, ctrl, shift, alt, sup, event.physical_key, &event.logical_key, false, app_cursor, alt_screen),
                };
                if self.debug {
                    let action_name = match &action {
                        input::KeyAction::TogglePanel => "TogglePanel",
                        input::KeyAction::ClosePanel => "ClosePanel",
                        input::KeyAction::OpenPalette => "OpenPalette",
                        input::KeyAction::NewTab => "NewTab",
                        input::KeyAction::CloseTab => "CloseTab",
                        input::KeyAction::DetachTab => "DetachTab",
                        input::KeyAction::NextTab => "NextTab",
                        input::KeyAction::PrevTab => "PrevTab",
                        input::KeyAction::SelectTab(_) => "SelectTab",
                        input::KeyAction::OpacityUp => "OpacityUp",
                        input::KeyAction::OpacityDown => "OpacityDown",
                        input::KeyAction::ScrollPageUp => "ScrollPageUp",
                        input::KeyAction::ScrollPageDown => "ScrollPageDown",
                        input::KeyAction::FontUp => "FontUp",
                        input::KeyAction::FontDown => "FontDown",
                        input::KeyAction::FontReset => "FontReset",
                        input::KeyAction::Copy => "Copy",

                        input::KeyAction::Paste => "Paste",
                        input::KeyAction::SearchToggle => "SearchToggle",
                        input::KeyAction::PrevPrompt => "PrevPrompt",
                        input::KeyAction::NextPrompt => "NextPrompt",
                        input::KeyAction::SelectAll => "SelectAll",
                        input::KeyAction::Quit => "Quit",
                        input::KeyAction::HintMode => "HintMode",
                        input::KeyAction::CopyMode => "CopyMode",
                        input::KeyAction::Send(_) => "Send",
                        input::KeyAction::None => "None",
                    };
                    eprintln!("KEY ctrl={ctrl} shift={shift} physical={:?} -> {action_name}", event.physical_key);
                }
                match action {
                    input::KeyAction::TogglePanel => {
                        // Open or close the separate Settings OS window.
                        self.toggle_settings_window(event_loop);
                    }
                    input::KeyAction::ClosePanel => {
                        // Escape never reaches here from the main window
                        // (panel_open is false), but keep the arm consistent:
                        // ensure the settings window is closed.
                        if self.settings_window.is_some() {
                            self.close_settings_window();
                            self.request_main_paint();
                        }
                    }
                    input::KeyAction::NewTab => {
                        self.new_tab();
                    }
                    input::KeyAction::CloseTab => {
                        // Ask before closing instead of closing immediately.
                        self.confirm_close = Some(self.active);
                        self.request_main_paint();
                    }
                    input::KeyAction::DetachTab => {
                        self.detach_tab(self.active, event_loop, None);
                    }
                    input::KeyAction::OpenPalette => {
                        self.open_palette();
                    }
                    input::KeyAction::SearchToggle => {
                        if self.search_open {
                            self.search_close();
                        } else {
                            self.search_open = true;
                            // Every close path clears, so the bar normally
                            // opens empty; should query state somehow survive
                            // on this tab, re-collect so stale Points (the
                            // scrollback rotated while closed) never render.
                            // No-op when no query is set.
                            self.active_tab_mut().terminal.search_refresh();
                            self.request_main_paint();
                        }
                    }
                    input::KeyAction::NextTab => {
                        self.switch_tab(true);
                    }
                    input::KeyAction::PrevTab => {
                        self.switch_tab(false);
                    }
                    input::KeyAction::SelectTab(n) => {
                        self.select_tab(n);
                    }
                    input::KeyAction::OpacityUp => {
                        self.opacity = (self.opacity + 0.05).min(1.0);
                        self.apply_theme();
                        self.persist();
                        self.request_main_paint();
                    }
                    input::KeyAction::OpacityDown => {
                        self.opacity = (self.opacity - 0.05).max(0.1);
                        self.apply_theme();
                        self.persist();
                        self.request_main_paint();
                    }
                    input::KeyAction::ScrollPageUp => {
                        self.active_tab_mut().terminal.scroll_page(true);
                        self.request_main_paint();
                        // Viewport moved under the pointer (see MouseWheel).
                        self.update_link_hover(true);
                    }
                    input::KeyAction::ScrollPageDown => {
                        self.active_tab_mut().terminal.scroll_page(false);
                        self.request_main_paint();
                        self.update_link_hover(true);
                    }
                    // OSC 133 prompt jump (Ctrl+Shift+Z prev / Ctrl+Shift+X next).
                    // Zero marks / at-the-end = pure no-op (jump_prompt returns
                    // false), so nothing redraws or moves the link hover then.
                    input::KeyAction::PrevPrompt | input::KeyAction::NextPrompt => {
                        let forward = action == input::KeyAction::NextPrompt;
                        if self.active_tab_mut().terminal.jump_prompt(forward) {
                            self.request_main_paint();
                            self.update_link_hover(true);
                        }
                    }
                    input::KeyAction::FontUp => {
                        self.set_font_size(self.font_logical + 1.0);
                    }
                    input::KeyAction::FontDown => {
                        self.set_font_size(self.font_logical - 1.0);
                    }
                    input::KeyAction::FontReset => {
                        self.set_font_size(FONT_LOGICAL_DEFAULT);
                    }
                    input::KeyAction::Copy => {
                        // Copy the current selection to the clipboard, then clear it
                        // so the highlight doesn't linger after an explicit copy.
                        let copied = self
                            .active_tab()
                            .terminal
                            .selection_text()
                            .filter(|t| !t.is_empty());
                        if let Some(text) = copied {
                            clipboard::set(&text);
                            self.active_tab_mut().terminal.selection_clear();
                            self.request_main_paint();
                        }
                    }
                    input::KeyAction::Paste => {
                        // Paste from the clipboard into the PTY.
                        if let Some(text) = clipboard::get() {
                            self.paste_text(&text);
                        }
                    }
                    input::KeyAction::SelectAll => {
                        // Folded from the old macOS Cmd+A block; also reachable via a
                        // user remap on any platform.
                        self.active_tab_mut().terminal.select_all();
                        self.request_main_paint();
                    }
                    input::KeyAction::Quit => {
                        // Folded from the old macOS Cmd+Q block: open the quit
                        // confirmation (never quit outright), matching today.
                        self.confirm_quit = true;
                        self.request_main_paint();
                    }
                    // Hint / copy-mode enter. Only reached when no other overlay
                    // owns keys (they capture the chord earlier and swallow it) —
                    // the enter methods double-check + no-op on the alt screen /
                    // (hint) an empty token scan.
                    input::KeyAction::HintMode => {
                        self.enter_hint_mode();
                    }
                    input::KeyAction::CopyMode => {
                        self.enter_copy_mode();
                    }
                    input::KeyAction::Send(bytes) => {
                        // Escape closes an open context/tab menu before forwarding to PTY.
                        if bytes == [0x1b]
                            && (self.context_menu.is_some() || self.tab_menu.is_some())
                        {
                            self.context_menu = None;
                            self.menu_hover = None;
                            self.tab_menu = None;
                            self.tab_menu_hover = None;
                            self.tab_menu_rects.clear();
                            self.tab_menu_labels.clear();
                            self.request_main_paint();
                            return;
                        }
                        // Esc also dismisses the welcome splash (but still reaches PTY).
                        // Any real Send to the PTY also dismisses the welcome splash.
                        if self.welcome_open {
                            self.welcome_open = false;
                        }
                        // Any real keystroke jumps back to the bottom so the user sees their input.
                        self.active_tab_mut().terminal.scroll_to_bottom();
                        let w = &mut self.tabs[self.active].writer;
                        let _ = w.write_all(&bytes);
                        let _ = w.flush();
                        // Input-latency START stamp (JETTY_PERF_LOG only): record the
                        // keystroke instant so the frame that reflects its echo can
                        // measure keypress→glyph. Gated on `perf.on` (a bool read once
                        // at startup) → the default path pays one predictable-false
                        // branch, no Instant::now(). Arms only at a quiescent prompt
                        // (main window). See crate::perf.
                        if self.perf.on {
                            self.perf.note_key_send();
                        }
                        // Trigger caret flash+pulse on printable keystrokes.
                        // Arm the shared burst clock when EITHER caret effect is on;
                        // each consumer is independently gated on its own toggle.
                        if (self.fx.caret_flash_enabled || self.fx.caret_glow_enabled)
                            && is_printable_keystroke(&bytes)
                        {
                            self.caret_anim = Some(std::time::Instant::now());
                            self.request_main_paint();
                        }
                    }
                    input::KeyAction::None => {}
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // IME commit (CJK input methods, dead-key composition routed
                // through the IME). It must honor the SAME modal priority chain
                // as KeyboardInput, or composed text leaks into the shell behind
                // a rename box / confirm popup (CJK users could not type non-ASCII
                // tab names at all). Preedit is not rendered (Commit-only IME
                // support); Enabled/Preedit/Disabled are intentionally ignored.
                if text.is_empty() || self.tabs.is_empty() {
                    return;
                }
                // Quit / close-tab confirmation popups are modal — drop the
                // commit. Checked FIRST, before the rename/search consumers,
                // to mirror the KeyboardInput priority chain exactly: both
                // popups can be open above the (mouse-non-modal) search bar,
                // and typed keys are swallowed there while IME commits used
                // to edit the query behind the popup (F9).
                if self.confirm_quit || self.confirm_close.is_some() {
                    return;
                }
                // Hint mode / copy-mode own the keyboard: DROP the commit so a CJK
                // IME (which routes even Latin letters through Ime::Commit rather
                // than KeyboardInput) cannot leak typed text to the shell behind
                // the overlay, nor have the mode's own keys silently fail
                // (BLOCKING 1). Mirrors the palette/search short-circuits below.
                if self.hint_mode.is_some() || self.copy_mode.is_some() {
                    return;
                }
                // Inline tab rename captures the commit into the title buffer
                // (mirrors the renaming arm of KeyboardInput).
                if self.renaming.is_some() {
                    for ch in text.chars() {
                        if !ch.is_control() {
                            self.rename_buf.push(ch);
                        }
                    }
                    self.request_main_paint();
                    return;
                }
                // The command palette captures IME commits into its query
                // (mirrors the KeyboardInput palette arm; keeps the same modal
                // priority so composed text never leaks behind the overlay).
                if self.palette_open {
                    let mut changed = false;
                    for ch in text.chars() {
                        if !ch.is_control() {
                            self.palette_query.push(ch);
                            changed = true;
                        }
                    }
                    if changed {
                        self.refilter_palette();
                    }
                    self.request_main_paint();
                    return;
                }
                // The scrollback-search bar captures IME commits so CJK users
                // can type queries (mirrors the KeyboardInput search arm).
                if self.search_open {
                    let mut q = self.active_tab().terminal.search_query().to_string();
                    q.extend(text.chars().filter(|c| !c.is_control()));
                    self.active_tab_mut().terminal.search_set_query(&q);
                    self.request_main_paint();
                    return;
                }
                if self.welcome_open {
                    self.welcome_open = false;
                }
                // Same discipline as the Send arm: jump to the live bottom
                // so the user sees their input.
                self.active_tab_mut().terminal.scroll_to_bottom();
                let w = &mut self.tabs[self.active].writer;
                let _ = w.write_all(text.as_bytes());
                let _ = w.flush();
                if (self.fx.caret_flash_enabled || self.fx.caret_glow_enabled)
                    && is_printable_keystroke(text.as_bytes())
                {
                    self.caret_anim = Some(std::time::Instant::now());
                }
                self.request_main_paint();
            }
            WindowEvent::RedrawRequested => {
                // Hidden (F9) window: never run the full render pipeline
                // (snapshot → shaping → GPU passes → present) into an unmapped
                // surface. PTY draining continues on the Wake path, so the shell
                // stays unblocked; we simply don't paint invisible frames (F16).
                // Occluded/minimized-but-shown windows are covered by the
                // per-source redraw gates plus acquire_frame returning None, so
                // they need no blanket early-out here.
                if !self.visible {
                    return;
                }
                // Auto-exit hint/copy-mode if a program switched to the alt screen
                // while a mode was active (a full-screen TUI launched mid-mode):
                // both modes are primary-screen only, so drop them cleanly rather
                // than draw stale chips / a cursor over the TUI. Cheap: one bool
                // test, only when a mode is active.
                if (self.hint_mode.is_some() || self.copy_mode.is_some())
                    && !self.tabs.is_empty()
                    && self.active_tab().terminal.alt_screen()
                {
                    if self.copy_mode.is_some() {
                        self.active_tab_mut().terminal.selection_clear();
                    }
                    self.hint_mode = None;
                    self.copy_mode = None;
                }
                // Re-assert the Dropdown dock AFTER the window is mapped: X11/KWin
                // ignores a set_outer_position issued before the window is realized
                // (it would land centered), so re-apply the top-strip geometry on
                // the first few post-map redraws. Counts down → idle CPU back to 0.
                if self.pending_dock_frames > 0 && self.window_mode == WindowMode::Dropdown {
                    self.pending_dock_frames -= 1;
                    if let Some(win) = &self.window {
                        dock_window_top(win, self.dropdown_width_pct, self.dropdown_height_pct);
                        if self.pending_dock_frames > 0 {
                            win.request_redraw();
                        }
                    }
                } else if self.pending_dock_frames > 0 {
                    // Mode switched away from Dropdown mid-countdown — stop docking.
                    self.pending_dock_frames = 0;
                }
                // Center-mode position re-assertion (see pending_center_frames).
                if self.pending_center_frames > 0 && self.window_mode == WindowMode::Center {
                    self.pending_center_frames -= 1;
                    if let (Some(win), Some(pos)) = (&self.window, self.pending_center_pos) {
                        win.set_outer_position(pos);
                        if self.pending_center_frames > 0 {
                            win.request_redraw();
                        }
                    }
                } else if self.pending_center_frames > 0 {
                    self.pending_center_frames = 0;
                }
                // Start the summon clock on the first real frame after a show (see
                // `summon_pending`) — guarantees t starts at 0 even if macOS delayed
                // presenting the window, so the reveal effect is never skipped.
                if self.summon_pending {
                    self.summon_pending = false;
                    self.summon_anim = Some(std::time::Instant::now());
                }
                // Drain every tab so background shells keep running; close any
                // whose child exited as part of the output we just drained.
                // (chrome changes are picked up by this same frame's
                // tabs_meta()/tab_activity snapshot below, so the flag is moot here.)
                let (had, _chrome_changed, exited) = self.drain_pty();
                // Input-latency echo signal (JETTY_PERF_LOG only): if this drain
                // consumed active-tab output, refresh the quiescent clock and mark
                // any armed keystroke's echo as seen. Gated on `perf.on`.
                if self.perf.on && had {
                    self.perf.note_active_output();
                }
                if !self.close_exited_tabs(exited, event_loop) {
                    return;
                }
                if self.tabs.is_empty() {
                    return;
                }
                // Fire notifications for any completion THIS drain surfaced (the
                // window-visible path; the hidden path is handled in the Wake arm).
                // Idempotent with the Wake dispatch: take_completions() drains, so
                // whichever drain produced the completion fires it exactly once.
                self.dispatch_completions(event_loop);
                // Streaming search refresh: stored match Points go stale as
                // output rotates the scrollback. Re-collect at most every
                // SEARCH_REFRESH_INTERVAL, only while the bar is open and only
                // when output was drained — event-driven, zero cost otherwise.
                // A drain the throttle skips marks the matches DIRTY instead;
                // about_to_wait then wakes once at the deadline for a trailing
                // refresh, so a burst that ends inside the window can't leave
                // the highlights/counter stale forever (F10).
                if had && self.search_open {
                    self.search_dirty = true;
                }
                if self.search_dirty
                    && self.search_open
                    && self
                        .search_refresh_at
                        .is_none_or(|t| t.elapsed() >= SEARCH_REFRESH_INTERVAL)
                {
                    self.active_tab_mut().terminal.search_refresh();
                    self.search_refresh_at = Some(std::time::Instant::now());
                    self.search_dirty = false;
                }
                // SINGLE clearing point for the activity indicator: the active
                // tab is on screen this frame, so its pending dot is consumed.
                // Covers every switch path (click, Ctrl+Tab, Ctrl+1..9, close
                // fix-ups, reattach) because each already requests a redraw.
                self.tabs[self.active].activity = jetty_render::TabActivity::None;
                // Per-tab activity for the drawn bar, index-aligned with
                // tabs_meta (frames are damage-driven; no idle-path allocation).
                let tab_activity: Vec<jetty_render::TabActivity> =
                    self.tabs.iter().map(|t| t.activity).collect();
                // Snapshot the ACTIVE tab and build the tab bar (immutable reads
                // gathered before borrowing the render stack mutably).
                let snap = self.active_tab().terminal.snapshot();
                let theme = self.current_theme();
                // Refresh the cached tab metadata (rebuilds only on change), then
                // take it out so the later &mut self.gpu/text borrow doesn't
                // conflict with this &self borrow; it is restored after rendering.
                self.tabs_meta();
                let tabs_meta = std::mem::take(&mut self.cached_tabs_meta);
                // Live perf HUD. Two render modes:
                //  • ACTIVE frame  → recompute live metrics (frame ms / CPU% / MB/s)
                //    and (re)arm the idle-repaint deadline. Runs inside a frame
                //    already in progress; it never itself requests a redraw.
                //  • IDLE frame    → when the deadline has elapsed with no other
                //    activity, paint the HUD as an honest "idle" instead of leaving
                //    a frozen, misleading fps/CPU on screen. about_to_wait scheduled
                //    exactly ONE such repaint, so idle still settles at ~0 CPU.
                let render_idle_hud = self.show_perf_hud
                    && !self.perf_idle_shown
                    && self
                        .perf_idle_at
                        .is_some_and(|d| std::time::Instant::now() >= d);
                let perf_string = if render_idle_hud {
                    self.perf_idle_shown = true;
                    Some("⚡ idle · 0% CPU · 0 MB/s".to_string())
                } else {
                    let s = self.update_perf_hud();
                    if self.show_perf_hud {
                        // (Re)arm the one-shot idle repaint for ~700ms after this
                        // active frame; cleared/rescheduled by the next active frame.
                        self.perf_idle_at = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(700),
                        );
                        self.perf_idle_shown = false;
                    }
                    s
                };
                self.perf_label = perf_string.clone();
                let context_menu = self.context_menu;
                let menu_hover = self.menu_hover;
                let tab_menu = self.tab_menu;
                let tab_menu_hover = self.tab_menu_hover;
                let tab_menu_labels = self.tab_menu_labels.clone();
                let help_open = self.help_open;
                // Clone the (cached, keymap-derived) help rows only when the overlay
                // is actually open — keeps the hot render path allocation-free.
                let help_rows: Vec<String> =
                    if help_open { self.help_rows.clone() } else { Vec::new() };
                // Search bar draw data + visible match highlights, captured
                // before the mutable gpu/text borrow. Both empty/None while
                // the bar is closed (one bool branch on the hot path).
                let search_ui: Option<(String, usize, usize)> = if self.search_open {
                    let t = &self.active_tab().terminal;
                    let (cur, total) = t.search_counter();
                    Some((t.search_query().to_string(), cur, total))
                } else {
                    None
                };
                let search_hits = if self.search_open {
                    self.active_tab().terminal.search_viewport_hits()
                } else {
                    Vec::new()
                };
                // OSC 133 failed-command marker rows (captured before the mutable
                // gpu render borrow, like search_hits). Empty in the common case.
                let failed_rows = self.active_tab().terminal.failed_prompt_rows();
                // Visible inline (sixel) images + their decoded RGBA (Arc clone,
                // cheap), captured OWNED before the mutable render borrow so the
                // image pass borrows nothing off self. Empty in the common case
                // (one bool-ish branch on the hot path, zero allocation).
                let images: Vec<(jetty_core::VisibleImage, std::sync::Arc<jetty_core::SixelImage>)> = {
                    let term = &self.active_tab().terminal;
                    term.visible_images()
                        .into_iter()
                        .filter_map(|vi| term.image_rgba(vi.id).map(|img| (vi, img)))
                        .collect()
                };
                // Command-palette draw data, captured (owned) before the mutable
                // gpu/text borrow so the draw pass borrows nothing off self. None
                // while closed — one bool test on the hot path, zero allocation.
                let palette_ui: Option<PaletteDrawData> =
                    if self.palette_open {
                        let first = self.palette_scroll;
                        let sel = self.palette_selected;
                        let total = self.palette_filtered.len();
                        let rows: Vec<(String, Vec<usize>, bool)> = self
                            .palette_filtered
                            .iter()
                            .enumerate()
                            .skip(first)
                            .take(jetty_render::MAX_PALETTE_ROWS)
                            .map(|(i, h)| (h.title.clone(), h.indices.clone(), i == sel))
                            .collect();
                        Some((self.palette_query.clone(), rows, total, first))
                    } else {
                        None
                    };
                let welcome_open = self.welcome_open;
                // Hint-mode chips: (label, first-span row, first-span col) for
                // each token whose label still matches the typed prefix, captured
                // OWNED before the mutable gpu/text borrow. None while inactive
                // (one Option test on the hot path, zero allocation).
                let hint_ui: Option<HintDrawData> =
                    self.hint_mode.as_ref().map(|hs| {
                        let typed = hs.typed.clone();
                        let labeled: Vec<(String, usize, usize)> = hs
                            .labels
                            .iter()
                            .zip(hs.tokens.iter())
                            .filter(|(lab, _)| typed.is_empty() || lab.starts_with(&typed))
                            .filter_map(|(lab, tok)| {
                                tok.spans.first().map(|(r, c, _)| (lab.clone(), *r, *c))
                            })
                            .collect();
                        (labeled, typed)
                    });
                // Copy-mode cursor + pill: (row, col, selecting, line_mode). None
                // while inactive; `copy_mode_active` suppresses the shell cursor.
                let copy_mode_ui: Option<(usize, usize, bool, bool)> =
                    self.copy_mode.as_ref().map(|c| (c.row, c.col, c.selecting, c.line_mode));
                let copy_mode_active = copy_mode_ui.is_some();
                // Pill only when the hint is live AND belongs to THIS (the
                // main) window — a detached-window drag must not light it
                // here (F4).
                let shift_hint_show = self.window.as_ref().is_some_and(|w| {
                    shift_hint_live_in(self.shift_hint_until, w.id(), std::time::Instant::now())
                });
                // Backend name for the welcome overlay (captured before the mutable
                // gpu borrow; falls back to "?" when gpu is not yet available).
                let gpu_backend_name: String = self
                    .gpu
                    .as_ref()
                    .map(|g| g.backend_name.clone())
                    .unwrap_or_else(|| "?".to_string());
                let confirm_quit = self.confirm_quit;
                let confirm_close: Option<String> = self
                    .confirm_close
                    .and_then(|i| self.tabs.get(i).map(|t| t.title.clone()));
                let rename_state: Option<(usize, String)> =
                    self.renaming.map(|i| (i, self.rename_buf.clone()));
                // Corner-mask inputs captured before the mutable render borrows.
                // The radius is logical px; scale to physical so it matches the
                // physical-pixel surface (HiDPI-correct rounding).
                let scale = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
                let corner_radius_px = self.corner_radius * scale;
                // In Dropdown mode the window is flush to the monitor top, so the
                // TOP corners must stay square (only the bottom corners round).
                // Derive "top-flush" from the window's outer position vs the
                // monitor top. On Wayland outer_position() is Err → not flush, so
                // we keep all-4 rounding (accepted degradation, no DE code).
                // Recompute the (syscall-backed) top-flush flag only on
                // non-animating frames; during a dropdown slide the window is
                // stationary, so reuse the cache and skip the per-frame
                // outer_position()/current_monitor() round-trips.
                if self.slide_anim.is_none() {
                    self.cached_top_flush = self.window_mode == WindowMode::Dropdown
                        && self
                            .window
                            .as_ref()
                            .and_then(|w| {
                                let p = w.outer_position().ok()?;
                                let mon = w.current_monitor().or_else(|| w.available_monitors().next())?;
                                Some(p.y <= mon.position().y + 1)
                            })
                            .unwrap_or(false);
                }
                let top_flush = self.cached_top_flush;
                // Lazily (re)allocate the offscreen scene texture when EITHER a
                // Tier-B effect (Liquid/Focus) is actively summoning OR CRT is
                // enabled — and the texture is missing or stale (wrong size).
                // Otherwise it stays unallocated (the normal hot path renders
                // straight to the surface). Done before the `as_ref()` captures
                // below so `offscreen` picks up the freshly-sized texture.
                let want_offscreen = self.fx.crt_enabled
                    || (self.summon_effect.is_tier_b() && self.summon_anim.is_some());
                if want_offscreen {
                    if let Some((gw, gh)) = self.gpu.as_ref().map(|g| (g.config.width, g.config.height)) {
                        let stale = self
                            .offscreen
                            .as_ref()
                            .is_none_or(|(t, _)| t.width() != gw || t.height() != gh);
                        if stale {
                            if let Some(g) = &self.gpu {
                                self.offscreen = Some(Self::make_offscreen(g));
                            }
                        }
                    }
                }
                let corner_mask = self.corner_mask.as_ref();
                let bayer_reveal = self.bayer_reveal.as_ref();
                let phosphor = self.phosphor.as_ref();
                let liquid = self.liquid.as_ref();
                let focus = self.focus.as_ref();
                let caret_fx = self.caret_fx.as_ref();
                let crt = self.crt.as_ref();
                let offscreen = self.offscreen.as_ref();
                // CRT enable flag, captured before the mutable gpu/text borrow.
                let crt_enabled = self.fx.crt_enabled;
                let summon_effect = self.summon_effect;
                // Summon progress: t in [0,1) drives a reveal pass this frame and
                // self-schedules the next; t>=1 ends the animation so we return to
                // damage-driven idle (0 CPU). None = not animating. Each effect has
                // its own duration. (None has duration 0 → ends on the first frame.)
                let summon_t = self.summon_anim.map(|start| {
                    let d = summon_effect.duration();
                    if d <= 0.0 { 1.0 } else { start.elapsed().as_secs_f32() / d }
                });
                // Dropdown slide progress (ease-out cubic). Captured here; the
                // pixel offset is computed once `height` is bound below.
                let slide_anim = self.slide_anim;
                // Tab-bar position + cursor captured before the mutable gpu/text
                // borrow so the render below can place the bar at top or bottom.
                let tab_bar_bottom = self.tab_bar_bottom;
                // Status-bar height (perf HUD) reserved at the window bottom,
                // captured before the mutable gpu/text borrow below.
                let status_h = self.status_h();
                let cursor = self.cursor;
                // Ctrl+hover link underline spans, snapshotted before the
                // gpu/text/quad borrows (drawn only while the modifier is held).
                let link_spans: Option<Vec<(usize, usize, usize)>> =
                    if link_modifier_held(&self.modifiers) {
                        self.link_hover.as_ref().map(|h| h.spans.clone())
                    } else {
                        None
                    };
                // Theme accent for the reveal glow (captured before the mutable
                // gpu/text/quad borrow below).
                let summon_accent: [f32; 3] = {
                    let a = self.current_theme().palette[4];
                    [a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0]
                };
                // Caret flash+pulse progress: t∈[0,1]. Captured and expired before
                // the mutable gpu/text borrow so self can be mutated freely here.
                let caret_flash_color = self.fx.caret_flash_color;
                let caret_t = self.caret_anim.map(|s| {
                    (s.elapsed().as_secs_f32() / (self.fx.caret_flash_ms / 1000.0)).min(1.0)
                });
                if caret_t == Some(1.0) {
                    self.caret_anim = None;
                }
                // Gate the CPU flash independently: pass None to the text renderer
                // when flash is disabled so glow-only mode never triggers the
                // color/scale modulation in text.rs, even if caret_anim is armed.
                let caret_t_for_flash =
                    if self.fx.caret_flash_enabled { caret_t } else { None };
                // Window focus drives the unfocused-hollow cursor (captured before
                // the mutable gpu/text borrow below).
                let main_focused = self.main_focused;
                let (Some(gpu), Some(text), Some(chrome_text), Some(quad), Some(image_layer)) = (
                    &mut self.gpu,
                    &mut self.text,
                    &mut self.chrome_text,
                    &mut self.quad,
                    &mut self.image_layer,
                ) else {
                    self.cached_tabs_meta = tabs_meta;
                    return;
                };
                let width = gpu.config.width;
                let height = gpu.config.height;
                // Render-side Dropdown slide: translate ALL scene content down
                // from -height to 0 via ease-out cubic over DROPDOWN_SLIDE_SECS.
                // This is NOT a per-frame reposition (no X11 ConfigureWindow race,
                // no-op-safe on Wayland) — it just shifts the content y-offset.
                let slide_y_offset = slide_anim
                    .map(|s| {
                        let t = (s.elapsed().as_secs_f32() / DROPDOWN_SLIDE_SECS).min(1.0);
                        let eased = 1.0 - (1.0 - t).powi(3); // ease-out cubic
                        -(height as f32) * (1.0 - eased)
                    })
                    .unwrap_or(0.0);
                // Tab-bar geometry: the bar's pixel Y (0 at top, height-TABBAR_H at
                // bottom) and the grid's pixel ORIGIN (TABBAR_H at top, 0 at bottom).
                // Bottom-mode tab bar sits ABOVE the status bar (height - TABBAR_H
                // - status_h); the status bar (perf HUD) takes the very bottom.
                let bar_y = if tab_bar_bottom { (height as f32 - TABBAR_H - status_h).max(0.0) } else { 0.0 };
                let grid_top = if tab_bar_bottom { 0.0 } else { TABBAR_H };
                // Compute window-control hover from the last cursor position.
                let ctrl_hover = ctrl_hover_at(cursor.0 as f32, cursor.1 as f32, width, bar_y);
                let rename_ref = rename_state.as_ref().map(|(i, b)| (*i, b.as_str()));
                let chrome_char_w = chrome_text.cell_size().0;
                // The perf HUD now lives in the bottom STATUS BAR (off the tab row),
                // so the tab bar is built WITHOUT it (None).
                let mut bar = jetty_render::build_tab_bar_ex(
                    width, &tabs_meta, &theme, rename_ref, ctrl_hover, None, chrome_char_w,
                    &tab_activity,
                );
                // Translate the bar quads + labels to its actual y (bottom mode)
                // PLUS the dropdown slide so it moves with the content.
                let bar_offset = bar_y + slide_y_offset;
                if bar_offset != 0.0 {
                    for q in &mut bar.quads {
                        q.y += bar_offset;
                    }
                    for l in &mut bar.labels {
                        l.2 += bar_offset;
                    }
                    for l in &mut bar.title_labels {
                        l.2 += bar_offset;
                    }
                }
                // Input-latency PRIMARY stamp (JETTY_PERF_LOG only): the frame's CPU
                // data is now fully built and we're about to acquire the swapchain —
                // which in Fifo (vsync) blocks at `acquire_frame` below. Capturing
                // here yields keypress→frame-ready WITHOUT the display-cadence wait.
                // Peek only (the pending key is consumed after present). Gated on
                // `perf.on` → one predictable-false branch on the default path.
                let perf_ready_ms = if self.perf.on {
                    self.perf.pending_elapsed_ms()
                } else {
                    None
                };
                if let Some((frame, view)) = gpu.acquire_frame() {
                    // Tier-B routing: when a Liquid/Focus effect is ACTIVELY
                    // summoning (t in [0,1)) AND the offscreen texture exists,
                    // render the whole scene into the offscreen view; the effect
                    // pass below then samples it and writes the displaced/blurred
                    // result to the surface `view`. For Tier-A effects, the
                    // no-summon idle path, and any frame without offscreen, this is
                    // `&view` — so the normal hot path is byte-identical to before
                    // (it never allocates or touches the offscreen texture).
                    let tier_b_active = summon_effect.is_tier_b()
                        && matches!(summon_t, Some(t) if t < 1.0)
                        && offscreen.is_some();
                    // CRT also routes the whole scene through the offscreen, but
                    // only when no Tier-B summon is using it this frame: a Tier-B
                    // summon OWNS the offscreen and CRT is BYPASSED for that frame
                    // (see the dispatch guard before `present()`). Requires the
                    // offscreen to actually exist (alloc'd above when crt_enabled).
                    let crt_active = crt_enabled && !tier_b_active && offscreen.is_some();
                    // Either consumer routes the scene into the offscreen; otherwise
                    // it renders straight to the surface view (byte-identical to the
                    // pre-CRT hot path).
                    let want_offscreen = tier_b_active || crt_active;
                    let scene_view: &wgpu::TextureView = if want_offscreen {
                        &offscreen.as_ref().unwrap().1
                    } else {
                        &view
                    };
                    // Cell size is needed both by the shared grid core below and
                    // by the main-only caret-glow / hint-overlay passes further
                    // down, so compute it here (a trivial getter; the core reads
                    // it again internally).
                    let (cell_w, cell_h) = text.cell_size();
                    let grid_bottom_px = if tab_bar_bottom {
                        (height as f32 - TABBAR_H - status_h).max(0.0)
                    } else {
                        (height as f32 - status_h).max(0.0)
                    };
                    // Passes 1–4 via the shared render core (v0.23 Task 8). The
                    // MAIN tab bar (Pass 3) is the mid-scene chrome, injected via
                    // the closure BETWEEN the glyph and scrollbar/cursor passes —
                    // exactly where it was drawn before. Main threads its own
                    // slide offset, search-hit tint, and copy-mode cursor through
                    // the params, so this is byte-identical to the pre-refactor
                    // body. The main-only caret GLOW, summon-reveal/Tier-B, and
                    // the corner-mask/CRT tail all stay BELOW, in this caller.
                    let scene = GridScene {
                        snap: &snap,
                        theme: &theme,
                        grid_top,
                        slide_y: slide_y_offset,
                        grid_bottom: grid_bottom_px,
                        status_h,
                        scale,
                        search_hits: &search_hits,
                        failed_rows: &failed_rows,
                        link_spans: link_spans.as_ref(),
                        images: &images,
                        focused: main_focused,
                        caret_t_for_flash,
                        caret_flash_color,
                        copy_mode_active,
                        copy_mode_ui,
                    };
                    render_grid_scene(
                        gpu,
                        text,
                        quad,
                        image_layer,
                        scene_view,
                        width,
                        height,
                        &scene,
                        // Pass 3: the tab bar (already translated to its actual y
                        // + dropdown slide above) over the grid.
                        |quad, device, queue, view, w, h| {
                            quad.render(device, queue, view, w, h, &bar.quads);
                            if !bar.labels.is_empty() {
                                // Chrome: fixed-size layer so the bar text never
                                // scales with the terminal font (never overflows
                                // the 36px bar).
                                let _ = chrome_text.render_overlays(device, queue, view, w, h, &bar.labels);
                            }
                            if !bar.title_labels.is_empty() {
                                // Tab TITLES in the platform's proportional sans;
                                // the ×/+/overflow/HUD/controls stay monospace.
                                let _ = chrome_text.render_overlays_sans(device, queue, view, w, h, &bar.title_labels);
                            }
                        },
                    );

                    // Pass 4a: bottom STATUS BAR (the perf HUD, OFF the tab row).
                    // A slim strip at the very bottom with the perf metrics
                    // right-aligned. Drawn only when show_perf_hud reserved the room
                    // (status_h > 0). It rides the dropdown slide like the rest.
                    if status_h > 0.0 {
                        if let Some(perf) = perf_string.as_deref() {
                            let sy = (height as f32 - status_h) + slide_y_offset;
                            // Theme-derived: a faint lifted strip + dim text (same
                            // bg→fg surface language as the rest of the chrome).
                            let tb = theme.bg;
                            let tf = theme.fg;
                            let nl = |t: f32| -> [u8; 4] {
                                [
                                    (tb[0] as f32 + (tf[0] as f32 - tb[0] as f32) * t) as u8,
                                    (tb[1] as f32 + (tf[1] as f32 - tb[1] as f32) * t) as u8,
                                    (tb[2] as f32 + (tf[2] as f32 - tb[2] as f32) * t) as u8,
                                    255,
                                ]
                            };
                            let strip = jetty_render::Rect {
                                x: 0.0, y: sy, w: width as f32, h: status_h,
                                color: nl(0.05), ..Default::default()
                            };
                            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[strip]);
                            // Right-align the perf text within the strip.
                            let cw = chrome_text.cell_size().0;
                            let perf_w = perf.chars().count() as f32 * cw;
                            let px = (width as f32 - perf_w - 12.0).max(8.0);
                            let dim = nl(0.5);
                            let py = sy + (status_h - 16.0) / 2.0;
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height,
                                &[(perf.to_string(), px, py, [dim[0], dim[1], dim[2]])],
                            );
                        }
                    }
                    // Pass 4c: Shift+drag hint toast — a brief, centered pill shown
                    // when the user drags (no Shift) inside a mouse-reporting app, so
                    // they discover the Shift+drag-to-select gesture. Throttled.
                    if shift_hint_show {
                        let hint = "Hold Shift while dragging to select text";
                        let cw = chrome_text.cell_size().0;
                        let tw = hint.chars().count() as f32 * cw;
                        let pad = 14.0;
                        let pill_w = tw + pad * 2.0;
                        let pill_h = 26.0;
                        let pill_x = ((width as f32 - pill_w) / 2.0).max(0.0);
                        // Sit the pill above the bottom-mode tab bar too, not just
                        // the status strip, or it draws over the tab titles.
                        let pill_y = (height as f32
                            - status_h
                            - if tab_bar_bottom { TABBAR_H } else { 0.0 }
                            - 14.0
                            - pill_h)
                            .max(0.0)
                            + slide_y_offset;
                        let c = theme.cursor;
                        let pill = jetty_render::Rect::rounded(
                            pill_x, pill_y, pill_w, pill_h, [c[0], c[1], c[2], 235], pill_h / 2.0,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill]);
                        let ty = pill_y + (pill_h - 16.0) / 2.0;
                        let _ = chrome_text.render_overlays(
                            &gpu.device, &gpu.queue, scene_view, width, height,
                            &[(hint.to_string(), pill_x + pad, ty, [20, 20, 20])],
                        );
                    }
                    // Pass 4d: the scrollback-search bar (Ctrl+Shift+F) — a
                    // themed pill at the top-right of the grid. Rides the
                    // dropdown slide like its neighbours and is drawn BEFORE
                    // the context menu / help / confirm passes so modals keep
                    // visual priority over it.
                    if let Some((q, cur, total)) = &search_ui {
                        let sb = jetty_render::build_search_bar(
                            width, grid_top + slide_y_offset, &theme, chrome_char_w, q, *cur, *total,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &sb.quads);
                        if !sb.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &sb.labels,
                            );
                        }
                    }
                    // Pass 4e: hint-mode label chips — themed/HiDPI, mirroring the
                    // search bar's draw (quads then chrome text). Only while active.
                    if let Some((labeled, typed)) = &hint_ui {
                        let refs: Vec<(&str, usize, usize)> =
                            labeled.iter().map(|(l, r, c)| (l.as_str(), *r, *c)).collect();
                        let ov = jetty_render::build_hint_overlay(
                            &refs,
                            cell_w,
                            cell_h,
                            grid_top + slide_y_offset,
                            &theme,
                            chrome_char_w,
                            typed,
                            width,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &ov.quads);
                        if !ov.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &ov.labels,
                            );
                        }
                    }
                    // Pass 4f: copy-mode "COPY" pill (top-left, discoverability +
                    // screenshot-verify surface).
                    if let Some((_, _, selecting, line_mode)) = copy_mode_ui {
                        let pill = jetty_render::build_copy_pill(
                            width,
                            grid_top + slide_y_offset,
                            &theme,
                            chrome_char_w,
                            line_mode,
                            selecting,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &pill.quads);
                        if !pill.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &pill.labels,
                            );
                        }
                    }
                    // Pass 4b: welcome splash — drawn over the grid but UNDER all
                    // modals (context menu, help, confirm popups). Only shown when
                    // welcome_open is true (dismissed on first PTY input/click/Esc).
                    // No modal is active at this draw position, so it won't occlude
                    // the splash, and modals drawn afterward sit on top of it.
                    // Skip the splash if any modal is active to avoid visual clutter.
                    if welcome_open
                        && context_menu.is_none()
                        && !help_open
                        && confirm_close.is_none()
                        && !confirm_quit
                        && palette_ui.is_none()
                    {
                        let mut splash = jetty_render::build_welcome_overlay(
                            width,
                            height,
                            grid_top + slide_y_offset,
                            env!("CARGO_PKG_VERSION"),
                            &gpu_backend_name,
                            &theme,
                            chrome_char_w,
                        );
                        // Clip the splash to the grid area so it never draws over a
                        // bottom tab bar (e.g. on a very short window): drop swatch
                        // quads / label rows below the grid bottom and trim a quad
                        // that straddles the edge. The status strip is always
                        // reserved; the tab bar only in bottom mode.
                        let grid_bottom = if tab_bar_bottom {
                            (height as f32 - TABBAR_H - status_h).max(0.0)
                        } else {
                            (height as f32 - status_h).max(0.0)
                        };
                        splash.quads.retain(|q| q.y < grid_bottom);
                        for q in &mut splash.quads {
                            if q.y + q.h > grid_bottom {
                                q.h = (grid_bottom - q.y).max(0.0);
                            }
                        }
                        splash.labels.retain(|l| l.2 + 18.0 <= grid_bottom);
                        if !splash.quads.is_empty() {
                            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &splash.quads);
                        }
                        if !splash.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &splash.labels,
                            );
                        }
                    }
                    // Draw the right-click context menu on top of everything.
                    if let Some((mx, my)) = context_menu {
                        let menu = jetty_render::build_context_menu(mx, my, width, height, menu_hover, &theme, chrome_char_w);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &menu.quads);
                        if !menu.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device,
                                &gpu.queue,
                                scene_view,
                                width,
                                height,
                                &menu.labels,
                            );
                        }
                    }
                    // Draw the TAB context menu (Detach / Rename / Close Tab) —
                    // mutually exclusive with the terminal menu above.
                    if let Some((mx, my, _)) = tab_menu {
                        let items: Vec<(&str, &str)> = tab_menu_labels
                            .iter()
                            .map(|&l| (l, crate::detached::menu_hint(l)))
                            .collect();
                        let menu = jetty_render::build_menu(
                            mx, my, width, height, tab_menu_hover, &theme, chrome_char_w, &items, &[],
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &menu.quads);
                        if !menu.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device,
                                &gpu.queue,
                                scene_view,
                                width,
                                height,
                                &menu.labels,
                            );
                        }
                    }
                    // Draw the Help overlay (Keyboard Shortcuts) on top of all
                    // else — a dim layer, a bordered panel, and the binding rows.
                    if help_open && palette_ui.is_none() {
                        let help = jetty_render::build_help_overlay(width, height, &theme, chrome_char_w, &help_rows);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &help.quads);
                        if !help.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device,
                                &gpu.queue,
                                scene_view,
                                width,
                                height,
                                &help.labels,
                            );
                        }
                    }
                    // Draw the close-tab confirmation popup on top of everything
                    // (above the help overlay): dim + bordered panel + buttons.
                    if confirm_quit {
                        let popup = jetty_render::build_confirm(
                            width, height, "Quit JeTTY? — all tabs will close", &theme, chrome_char_w,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &popup.quads);
                        if !popup.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &popup.labels,
                            );
                        }
                    } else if let Some(title) = &confirm_close {
                        let popup = jetty_render::build_confirm_close(width, height, title, &theme, chrome_char_w);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &popup.quads);
                        if !popup.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device,
                                &gpu.queue,
                                scene_view,
                                width,
                                height,
                                &popup.labels,
                            );
                        }
                    }
                    // Command palette — drawn LAST (above help/welcome/menus/
                    // confirm) so the single active overlay owns the top layer.
                    // Built + drawn strictly inside this Some() branch: nothing when
                    // closed (zero idle cost).
                    if let Some((q, prows_data, total, first)) = &palette_ui {
                        let prows: Vec<jetty_render::PaletteRow> = prows_data
                            .iter()
                            .map(|(t, idx, sel)| jetty_render::PaletteRow {
                                title: t,
                                match_indices: idx,
                                selected: *sel,
                            })
                            .collect();
                        let pal = jetty_render::build_command_palette(
                            width, height, &theme, chrome_char_w, q, &prows, *total, *first,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &pal.quads);
                        if !pal.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &pal.labels,
                            );
                        }
                    }
                    // Caret glow/ripple pass (Task 12). Additive GPU burst at the
                    // cursor position on each keystroke. Dispatched only when the
                    // toggle is on AND an animation is live AND the cursor is visible.
                    //
                    // Runs BEFORE the corner mask below, so the mask's coverage
                    // multiply clips the halo/ring at the rounded corners — an
                    // additive pass AFTER the mask would add RGB into alpha=0
                    // corner pixels, which a PreMultiplied compositor still
                    // displays (glow visibly bleeding outside the window shape).
                    //
                    // Target is always `scene_view`, which routes correctly for all
                    // three compositing cases:
                    //   CRT ON:   scene_view == offscreen → glow composites into the
                    //             offscreen; the CRT pass below samples and rounds
                    //             the corners. Glow gets full CRT treatment.
                    //   CRT OFF:  scene_view == &view (surface) → glow lands before
                    //             the corner mask, which clips it to the shape.
                    //   Tier-B:   scene_view == offscreen (Tier-B owns it); the
                    //             effect samples it after the mask bakes in, so
                    //             the glow is displaced/blurred with the scene.
                    //
                    // No new redraw scheduling — the caret_anim guard in the
                    // self-drive block below keeps frames coming while the burst
                    // is live.
                    if self.fx.caret_glow_enabled {
                        if let (Some(cfx), Some(t_val)) = (caret_fx, caret_t) {
                            if snap.cursor_visible
                                && snap.cursor_col < snap.cols
                                && snap.cursor_row < snap.rows
                                && t_val < 1.0
                            {
                                // Cursor cell centre in physical pixels. x and y both
                                // start from (0,0) at the top-left of the viewport,
                                // matching @builtin(position) in the WGSL fragment.
                                // Mirrors text.rs:585-596's cursor_left/top formula.
                                let cursor_px_x = snap.cursor_col as f32 * cell_w
                                    + cell_w * 0.5;
                                let cursor_px_y = snap.cursor_row as f32 * cell_h
                                    + grid_top + slide_y_offset + cell_h * 0.5;
                                cfx.apply(
                                    &gpu.device,
                                    &gpu.queue,
                                    scene_view,
                                    &jetty_render::CaretFxUniform {
                                        resolution: [width as f32, height as f32],
                                        cursor_px: [cursor_px_x, cursor_px_y],
                                        cell: [cell_w, cell_h],
                                        t: t_val,
                                        // Tasteful default intensity; bright enough to
                                        // be visible, subtle enough not to dominate.
                                        intensity: 0.5,
                                        color: [
                                            caret_flash_color[0],
                                            caret_flash_color[1],
                                            caret_flash_color[2],
                                            0.0, // pad
                                        ],
                                    },
                                );
                            }
                        }
                    }
                    // Final pass: round the window corners by zeroing alpha
                    // outside a rounded rect. No-op when radius == 0 (square).
                    // Applied to `scene_view`: for Tier-A this is the surface; for a
                    // Tier-B summon it's the offscreen frame, so the rounded corners
                    // are baked in before the effect samples it.
                    //
                    // When CRT is active (crt_active) the CRT pass owns the
                    // rounded corners via its own alpha compositing, so SKIP the
                    // mask here to avoid double-rounding. During a Tier-B summon
                    // CRT is bypassed (crt_active is false), so the mask still runs
                    // exactly as today and the summon path is unchanged.
                    if let (Some(mask), false) = (corner_mask, crt_active) {
                        // Bottom corners always round to corner_radius_px; the top
                        // corners are zeroed when the window is top-flush (Dropdown).
                        let r_top = if top_flush { 0.0 } else { corner_radius_px };
                        mask.apply(
                            &gpu.device,
                            &gpu.queue,
                            scene_view,
                            width,
                            height,
                            r_top,
                            r_top,
                            corner_radius_px,
                            corner_radius_px,
                        );
                    }
                    // Final-final pass: the selected summon reveal effect. After the
                    // corner mask, run the per-effect pass at the current t. Tier-A
                    // (Bayer/Phosphor) write into `scene_view` and compose with the
                    // dst-multiply blend. Tier-B (Liquid/Focus) SAMPLE the offscreen
                    // scene (`scene_view`) and write the displaced/blurred result to
                    // the surface `view`. At t>=1 every effect is fully resolved
                    // (zero residue, identity blit) and we stop the animation;
                    // otherwise self-drive the next frame.
                    //
                    // Tier-A dst is `scene_view`, NOT `&view`: when CRT is off (or
                    // bypassed by a Tier-B summon) `scene_view` IS the surface view,
                    // so this is byte-identical to before. When CRT is active
                    // `scene_view` is the offscreen, so the reveal composites into
                    // the offscreen and the CRT pass below blits it to the surface
                    // (instead of CRT clobbering a surface-only reveal). Tier-A
                    // effects use LoadOp::Load + blend and sample no texture, so
                    // there is no src==dst hazard against the CRT read.
                    if let Some(t) = summon_t {
                        if t < 1.0 {
                            match summon_effect {
                                SummonEffect::None => {}
                                SummonEffect::Bayer => {
                                    if let Some(reveal) = bayer_reveal {
                                        reveal.apply(
                                            &gpu.device, &gpu.queue, scene_view, width, height, t,
                                        );
                                    }
                                }
                                SummonEffect::Phosphor => {
                                    if let Some(ph) = phosphor {
                                        ph.apply(
                                            &gpu.device, &gpu.queue, scene_view, width, height,
                                            corner_radius_px, t, summon_accent,
                                        );
                                    }
                                }
                                SummonEffect::Liquid => {
                                    // tier_b_active guarantees scene_view is the
                                    // offscreen frame here; sample it → surface.
                                    if let (Some(lq), true) = (liquid, tier_b_active) {
                                        lq.apply(
                                            &gpu.device, &gpu.queue, &view, scene_view,
                                            width, height, t,
                                        );
                                    }
                                }
                                SummonEffect::Focus => {
                                    if let (Some(fc), true) = (focus, tier_b_active) {
                                        fc.apply(
                                            &gpu.device, &gpu.queue, &view, scene_view,
                                            width, height, t,
                                        );
                                    }
                                }
                            }
                        } else {
                            // Reveal complete — back to idle (no pass next frame).
                            self.summon_anim = None;
                        }
                    }
                    // Dropdown slide self-driver: while the slide is live keep
                    // requesting redraws; clear it at t>=1 so we return to idle.
                    if let Some(s) = self.slide_anim {
                        if s.elapsed().as_secs_f32() >= DROPDOWN_SLIDE_SECS {
                            self.slide_anim = None;
                        }
                    }
                    // Self-drive the next frame while EITHER animation is live, the
                    // Shift+drag hint toast is still showing (so it repaints away on
                    // expiry instead of freezing on screen), OR an animated CRT
                    // sub-effect is on. Idle CPU returns to ~0 once all have cleared.
                    // Self-drive only when the hint belongs to THIS window;
                    // a detached window's pill drives its own frames (F4).
                    let hint_live = self.window.as_ref().is_some_and(|w| {
                        shift_hint_live_in(
                            self.shift_hint_until,
                            w.id(),
                            std::time::Instant::now(),
                        )
                    });
                    // CRT animation self-drive: keep painting ONLY while CRT is on
                    // AND at least one of roll/flicker/jitter is toggled on. Static
                    // CRT (enabled, all three off) does NOT match here, so it stays
                    // damage-driven (0-CPU idle preserved). Same `crt_anim_live()`
                    // term feeds `about_to_wait`'s `main_pending`, so on macOS the
                    // loop sits in `Poll` (vsync-throttled by Fifo present) while
                    // animating and returns to `Wait`/idle the moment it clears.
                    // Gated on `self.visible` like the `main_pending` term: a
                    // hidden window must never self-drive CRT frames.
                    let crt_anim_live = self.visible && self.fx.crt_anim_live();
                    if self.summon_anim.is_some()
                        || self.slide_anim.is_some()
                        || hint_live
                        || crt_anim_live
                        || self.caret_anim.is_some()
                    {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    // CRT post-pass: when CRT is active (enabled AND not bypassed by
                    // an active Tier-B summon — Tier-B owns the offscreen this frame)
                    // run the full CRT effect pipeline (curvature, scanlines, bloom,
                    // chromatic aberration, vignette, roll/flicker/jitter, rounded
                    // corners) sampling the offscreen onto the surface `view`. `crt`
                    // and `offscreen` are both guaranteed present when `crt_active`
                    // (built in `resumed`; offscreen alloc'd above when crt_enabled),
                    // but guard defensively. src=offscreen, dst=surface — never
                    // src==dst; the offscreen was cleared+painted this frame, so it
                    // is never sampled uninitialized. This does NOT request a redraw,
                    // so enabling CRT does not by itself break 0-CPU idle.
                    if crt_active {
                        if let (Some(crt), Some((_, off_view))) = (crt, offscreen) {
                            crt.apply(
                                &gpu.device,
                                &gpu.queue,
                                &view,
                                off_view,
                                width,
                                height,
                                &jetty_render::CrtUniform {
                                    resolution: [width as f32, height as f32],
                                    curvature: self.fx.crt_curvature,
                                    scanline: self.fx.crt_scanline,
                                    mask: self.fx.crt_mask,
                                    bloom: self.fx.crt_bloom,
                                    chromatic: self.fx.crt_chromatic,
                                    vignette: self.fx.crt_vignette,
                                    // Scanline tint rgb (+ pad). White => neutral.
                                    tint: [
                                        self.fx.crt_scanline_tint[0],
                                        self.fx.crt_scanline_tint[1],
                                        self.fx.crt_scanline_tint[2],
                                        0.0,
                                    ],
                                    // The CRT pass owns the rounded corners (the
                                    // corner mask is skipped while CRT is active),
                                    // so feed it the same per-position radii the
                                    // mask would use: bottom corners always round;
                                    // the TOP corners stay square when the window
                                    // is top-flush (Dropdown), so CRT-on never
                                    // opens a transparent notch at the monitor's
                                    // top edge.
                                    corner_radius: corner_radius_px,
                                    corner_radius_top: if top_flush {
                                        0.0
                                    } else {
                                        corner_radius_px
                                    },
                                    // Animation (Task 10): free-running phase +
                                    // roll/flicker/jitter bitfield. When all three
                                    // toggles are off, flags == 0 and the shader
                                    // output is identical to the static look (each
                                    // animated term collapses to its static value),
                                    // so static CRT is byte-identical here.
                                    time: (self.crt_clock.elapsed().as_secs_f64()
                                        % CRT_PHASE_WRAP) as f32,
                                    flags: (if self.fx.crt_animate_roll {
                                        jetty_render::CRT_FLAG_ROLL
                                    } else {
                                        0
                                    }) | (if self.fx.crt_flicker {
                                        jetty_render::CRT_FLAG_FLICKER
                                    } else {
                                        0
                                    }) | (if self.fx.crt_jitter {
                                        jetty_render::CRT_FLAG_JITTER
                                    } else {
                                        0
                                    }),
                                },
                            );
                        }
                    }
                    // Input-latency SECONDARY stamp (JETTY_PERF_LOG only): captured
                    // AFTER the vsync-throttled acquire + GPU-pass submit, just before
                    // present → keypress→pre-present. The Vec push + any emit happen
                    // AFTER present() below so the measurement never perturbs the
                    // frame it is timing (observer-effect fix).
                    let perf_present_ms = if self.perf.on {
                        self.perf.pending_elapsed_ms()
                    } else {
                        None
                    };
                    frame.present();
                    // Missed-paint proof counter (JETTY_FRAME_LOG only).
                    if self.frame_log {
                        self.frames_presented += 1;
                        eprintln!("JETTY_FRAME {} main", self.frames_presented);
                    }
                    if self.perf.on {
                        if let (Some(ready), Some(present)) = (perf_ready_ms, perf_present_ms) {
                            self.perf.record_latency(ready, present);
                        }
                        // Genuine exec→first-frame + display refresh, logged once.
                        // This main-window present is the true cold-start first frame
                        // (the settings/detached presents are user-triggered later).
                        if !self.perf.first_frame_logged {
                            let hz = self
                                .window
                                .as_ref()
                                .and_then(|w| w.current_monitor())
                                .and_then(|m| m.refresh_rate_millihertz())
                                .map(|mhz| mhz as f32 / 1000.0);
                            self.perf.log_first_frame(hz);
                        }
                    }
                }
                // Restore the tab-metadata cache taken above so it persists across
                // frames (its signature still matches, so it won't rebuild).
                self.cached_tabs_meta = tabs_meta;
            }
            _ => {}
        }
    }
}

/// Plain-data inputs to [`render_grid_scene`], grouped to keep the two call
/// sites (main + detached) readable. GPU resources and the mid-scene chrome
/// closure are passed separately.
///
/// EQUIVALENCE CONTRACT (v0.23 BLOCKING 5): a detached window passes
/// `slide_y = 0.0`, `search_hits = &[]`, `copy_mode_active = false`, and
/// `copy_mode_ui = None`. As a result the shared core adds NO dropdown slide
/// and NO copy-mode cursor for detached — exactly as before. The main-only
/// caret GLOW, the summon-reveal / Tier-B passes, and the whole overlay +
/// corner-mask/CRT tail live in the MAIN caller AFTER this core, so a detached
/// window still gains none of them.
struct GridScene<'a> {
    snap: &'a jetty_core::GridSnapshot,
    theme: &'a jetty_core::Theme,
    /// Un-slid grid top (0.0 or `TABBAR_H`). The scrollbar is computed at this
    /// origin and then translated by `slide_y` (main dropdown slide).
    grid_top: f32,
    /// Dropdown-slide pixel offset. Always `0.0` for a detached window.
    slide_y: f32,
    /// Un-slid grid bottom (image scissor). `slide_y` is added inside the core.
    grid_bottom: f32,
    status_h: f32,
    /// Physical-px scale factor (failed-command marker bar width).
    scale: f32,
    /// Search-hit tint source; empty (`&[]`) unless the main search bar is open.
    search_hits: &'a [jetty_core::SearchHit],
    failed_rows: &'a [u16],
    link_spans: Option<&'a Vec<(usize, usize, usize)>>,
    images: &'a [(jetty_core::VisibleImage, std::sync::Arc<jetty_core::SixelImage>)],
    focused: bool,
    caret_t_for_flash: Option<f32>,
    caret_flash_color: [f32; 3],
    /// Copy-mode is main-only. Detached passes `false` → the shell cursor is
    /// never suppressed here.
    copy_mode_active: bool,
    /// Copy-mode keyboard cursor. Detached passes `None` → no extra cursor.
    copy_mode_ui: Option<(usize, usize, bool, bool)>,
}

/// The genuinely-shared per-window grid render body, extracted from the main
/// `RedrawRequested` arm ∩ `render_detached_window` (v0.23 Task 8 / BLOCKING 5).
///
/// It performs ONLY the common sequence:
///   Pass 1  clear + per-cell background quads (+ main-only search-hit tint)
///   Pass 2  glyphs
///   Pass 2b inline (sixel/kitty) images, scissored to the grid area
///   Pass 3  CALLER-INJECTED mid-scene chrome (`draw_chrome`) — the main tab
///           bar or the detached title bar, drawn BETWEEN the glyph pass and
///           the scrollbar/cursor pass exactly as both windows do today
///   Pass 4  scrollbar + failed-command markers + SGR decorations + link
///           underline + cursor (+ main-only copy-mode cursor)
///
/// Everything else stays in the caller: the main-only caret GLOW pass, the
/// summon-reveal / Tier-B routing, the dropdown-slide *decision*, the overlay
/// stack (search/hint/copy/help/confirm/palette/menus/welcome/status/toast),
/// and the corner-mask + CRT tail + present + animation self-drive. The slide
/// OFFSET is threaded through as data (`slide_y`), never a slide the detached
/// path can accidentally acquire (it passes `0.0`).
#[allow(clippy::too_many_arguments)]
fn render_grid_scene(
    gpu: &GpuContext,
    text: &mut TextLayer,
    quad: &mut QuadLayer,
    image_layer: &mut jetty_render::ImageLayer,
    scene_view: &wgpu::TextureView,
    width: u32,
    height: u32,
    s: &GridScene,
    draw_chrome: impl FnOnce(&mut QuadLayer, &wgpu::Device, &wgpu::Queue, &wgpu::TextureView, u32, u32),
) {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let (cell_w, cell_h) = text.cell_size();
    // The glyphs/backgrounds/cursor all draw at the slid origin; the scrollbar
    // is computed at the un-slid `grid_top` and then translated by `slide_y`
    // (matches both windows' pre-refactor behavior; `slide_y == 0` for detached).
    let grid_origin_y = s.grid_top + s.slide_y;
    let selection_bg = selection_bg_for(s.theme);
    let scrollbar_thumb = scrollbar_thumb_for(s.theme);

    // Pass 1: clear to the (premultiplied, opacity-correct) theme bg and paint
    // the per-cell background quads under the text. Search-hit tint rects are
    // appended AFTER the selection rects (main-only; empty for detached) so the
    // match tint wins where they overlap, still under the glyphs.
    let mut bg_rects = jetty_render::cell_bg_rects(s.snap, cell_w, cell_h, grid_origin_y, selection_bg);
    if !s.search_hits.is_empty() {
        bg_rects.extend(jetty_render::search_hit_rects(
            s.search_hits, cell_w, cell_h, grid_origin_y, s.theme,
        ));
    }
    quad.render_clear(
        device, queue, scene_view, width, height, &bg_rects,
        jetty_render::default_bg_clear(s.snap, gpu.premultiply_clear),
    );

    // Pass 2: glyphs over the painted background, offset down by the grid origin.
    let _ = text.render_to(device, queue, scene_view, width, height, s.snap, false, grid_origin_y);

    // Pass 2b: inline images over the grid text, scissored to the grid area
    // (below the bar, above the status strip / bottom tab bar), clamped to the
    // attachment. Called every frame so VRAM is reclaimed when images leave view.
    let image_draws: Vec<jetty_render::ImageDraw> = s
        .images
        .iter()
        .map(|(vi, img)| jetty_render::ImageDraw {
            id: vi.id,
            w: img.width,
            h: img.height,
            rgba: &img.rgba,
            dst: [
                vi.col as f32 * cell_w,
                grid_origin_y + vi.top_row * cell_h,
                vi.px_w as f32,
                vi.px_h as f32,
            ],
            opacity: 1.0,
        })
        .collect();
    let sc_top = grid_origin_y.clamp(0.0, height as f32);
    let sc_bot = (s.grid_bottom + s.slide_y).clamp(0.0, height as f32);
    let sc_y = sc_top as u32;
    let sc_h = (sc_bot as u32).saturating_sub(sc_y);
    image_layer.render(device, queue, scene_view, width, height, &image_draws, [0, sc_y, width, sc_h]);

    // Pass 3: caller-injected mid-scene chrome (main tab bar / detached title
    // bar), drawn over the grid but under the scrollbar/cursor pass.
    draw_chrome(quad, device, queue, scene_view, width, height);

    // Pass 4: scrollbar, failed-command markers, SGR decorations, the
    // Ctrl+hover / OSC 8 link underline, and the cursor — one quad pass.
    let mut rects: Vec<jetty_render::Rect> = Vec::new();
    if let Some(mut r) =
        jetty_render::scrollbar_rect(s.snap, width, height, s.grid_top, s.status_h, scrollbar_thumb)
    {
        r.y += s.slide_y;
        rects.push(r);
    }
    if !s.failed_rows.is_empty() {
        rects.extend(jetty_render::failed_marker_rects(
            s.failed_rows,
            cell_h,
            grid_origin_y,
            (3.0 * s.scale).round().max(2.0),
            s.theme.failed_marker_color(),
        ));
    }
    rects.extend_from_slice(text.decoration_rects());
    if let Some(spans) = s.link_spans {
        let p12 = s.theme.palette[12];
        rects.extend(jetty_render::link_underline_rects(
            spans, [p12[0], p12[1], p12[2], 255], cell_w, cell_h, grid_origin_y,
        ));
    }
    // Cursor last so it draws over glyphs + decorations. In copy-mode (main
    // only) the shell's block cursor is SUPPRESSED so only the copy-mode
    // keyboard cursor shows; detached always passes `copy_mode_active = false`.
    if !s.copy_mode_active {
        rects.extend(jetty_render::cursor_rects(
            s.snap, cell_w, cell_h, grid_origin_y, s.focused, s.caret_t_for_flash, s.caret_flash_color,
        ));
    }
    if let Some((cr, cc, _sel, _lm)) = s.copy_mode_ui {
        rects.extend(jetty_render::copy_cursor_rects(cr, cc, cell_w, cell_h, grid_origin_y, s.theme.cursor));
    }
    quad.render(device, queue, scene_view, width, height, &rects);
}

/// Shared input core (v0.23 Task 9 / amendment I5): a keystroke (or IME commit)
/// that was NOT consumed by any chrome/overlay → snap the viewport to the live
/// bottom and write the decoded bytes to this tab's PTY. Deliberately SMALL —
/// the caret-burst arming (gated on different toggles per window; the glow is
/// main-only), the perf keystroke stamp, welcome-splash dismissal, and every
/// modal/menu short-circuit stay in the per-window callers.
fn write_key_to_pty(tab: &mut Tab, bytes: &[u8]) {
    // Any real keystroke jumps the view back to the live bottom so typing while
    // scrolled up into scrollback is visible (F30). Order is irrelevant vs the
    // PTY write (viewport offset and the PTY writer are independent).
    tab.terminal.scroll_to_bottom();
    let _ = tab.writer.write_all(bytes);
    let _ = tab.writer.flush();
}

/// Shared wheel-delta → fractional-lines conversion (v0.23 Task 9 / amendment
/// I5). Byte-identical in the main and detached `MouseWheel` arms; the
/// accumulation (`ScrollAccumulator::add`) and everything downstream stay
/// per-window because they read window-specific geometry/state.
fn wheel_delta_to_lines(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y * 3.0,
        MouseScrollDelta::PixelDelta(p) => {
            // Approximate cell height; 20.0 is a reasonable default.
            const CELL_H: f64 = 20.0;
            (p.y / CELL_H) as f32
        }
    }
}


/// Largest byte index `<= max` that is a char boundary of `s` (a stable stand-in for
/// the unstable `str::floor_char_boundary`). Used to cap an OSC 52 paste reply
/// without splitting a multibyte char, which would make `String::truncate` panic.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut b = max;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Hash a config-file string to a `u64` (self-write guard for hot-reload). Content-
/// based and dependency-free; only equality matters, so the exact algorithm is
/// irrelevant as long as it is deterministic within a process run.
fn hash_config_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn spawn_waker(proxy: EventLoopProxy<AppEvent>) {
    // Slow safety heartbeat: 100ms is sufficient for any time-based UI ticking.
    // Responsiveness for PTY data (including p10k query replies) is now driven
    // by the on_data callback in PtySession::spawn, which wakes the loop
    // immediately on every chunk — so this tick no longer sets the latency floor.
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if proxy.send_event(AppEvent::Wake).is_err() {
            break;
        }
    });
}

/// Which window-control button (if any) the cursor at `(cx, cy)` is over, given
/// the surface `width`. Mirrors the control layout in `build_tab_bar_ex`: three
/// `28px` cells parked at the right of the `TABBAR_H` strip (min, max, close).
/// Selection-highlight background derived from the active theme: a dim accent
/// blend (mirrors panel.rs's selected-row color) so selections read on any theme.
fn selection_bg_for(theme: &jetty_core::Theme) -> [u8; 3] {
    let bg = theme.bg;
    let accent = theme.palette[4];
    [
        ((bg[0] as u16 + accent[0] as u16 * 2) / 3) as u8,
        ((bg[1] as u16 + accent[1] as u16 * 2) / 3) as u8,
        ((bg[2] as u16 + accent[2] as u16 * 2) / 3) as u8,
    ]
}

/// Path to the XDG autostart entry: `$XDG_CONFIG_HOME/autostart/jetty.desktop`,
/// falling back to `~/.config/autostart/jetty.desktop`. This is the freedesktop
/// standard honored by KDE/GNOME/any DE — no desktop-environment-specific code.
fn autostart_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from(".config"));
    base.join("autostart").join("jetty.desktop")
}

/// True when the autostart `.desktop` file exists — the source of truth for the
/// "Launch at login" toggle state at startup.
fn autostart_file_exists() -> bool {
    autostart_path().exists()
}

/// Write (enabled) or remove (disabled) the XDG autostart `.desktop` file.
/// Best-effort: logs a one-line error and never panics.
fn set_launch_at_login(enabled: bool) {
    let path = autostart_path();
    if enabled {
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("launch-at-login: could not create {}: {e}", dir.display());
                return;
            }
        }
        // Use the absolute path of the current executable so the entry works
        // regardless of install location; fall back to the bare "jetty" name.
        // Quoted/escaped per the Desktop Entry spec: a path with a space would
        // otherwise be parsed as program + argument (autostart silently dead),
        // and a literal '%' would be consumed as a field code.
        let exec = desktop_exec_arg(
            &std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
                .unwrap_or_else(|| "jetty".to_string()),
        );
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=JeTTY\n\
             GenericName=Terminal Emulator\n\
             Comment=Blazing-fast GPU terminal with a center-summon hotkey (autostart: holds the F9 grab)\n\
             Exec={exec}\n\
             Icon=jetty\n\
             Terminal=false\n\
             Categories=System;TerminalEmulator;Utility;\n\
             StartupWMClass=jetty\n\
             X-GNOME-Autostart-enabled=true\n"
        );
        if let Err(e) = std::fs::write(&path, contents) {
            eprintln!("launch-at-login: could not write {}: {e}", path.display());
        }
    } else if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("launch-at-login: could not remove {}: {e}", path.display());
        }
    }
}

/// Quote one argument for a `.desktop` `Exec=` line per the freedesktop
/// Desktop Entry spec. Two escaping passes in the spec's order:
///
/// 1. QUOTING: double-quote the argument, prefixing a backslash before each
///    reserved char (`"`, `` ` ``, `$`, `\`).
/// 2. STRING escape (applied AFTER quoting per the spec's note): the general
///    string-value escape rule doubles every backslash. So a literal `$` inside
///    the quotes is written `\\$` and a literal backslash four backslashes.
///
/// Then every literal `%` is doubled to `%%` (field-code escaping, whole value).
///
/// Without pass 2, a path containing `$ " `` ` `` \` emitted `\$`/`\``, which
/// GLib's GKeyFile treats as an invalid escape sequence and rejects — so GNOME
/// autostart silently launched nothing (F35). (Spaces already worked: no
/// backslash is introduced for them.)
fn desktop_exec_arg(path: &str) -> String {
    // Pass 1: quoting.
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // Pass 2: string escape — double every backslash (the structural quotes are
    // not backslashes, so they are untouched).
    let string_escaped = quoted.replace('\\', "\\\\");
    // Field-code escaping over the whole value.
    string_escaped.replace('%', "%%")
}

/// Display name for a `shell` config value: "System default" for the empty
/// (auto-detect) selection, else the file basename of the path (e.g. "zsh").
fn shell_display_name(shell: &str) -> String {
    if shell.is_empty() {
        "System default".to_string()
    } else {
        std::path::Path::new(shell)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| shell.to_string())
    }
}

/// Detect the login shells installed on the system, POSIX-style.
///
/// Reads `/etc/shells` (the standard, desktop-environment-INDEPENDENT registry
/// of valid login shells): keeps lines starting with `/`, trims whitespace,
/// skips comments (`#`) and blanks, drops paths that don't exist on disk, and
/// dedups by file basename (so `/bin/zsh` and `/usr/bin/zsh` collapse to one —
/// first occurrence wins). If `/etc/shells` is missing/empty, falls back to
/// whichever common shells exist. Returns absolute paths.
fn detect_shells() -> Vec<String> {
    use std::path::Path;
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new(); // basenames already added
    if let Ok(contents) = std::fs::read_to_string("/etc/shells") {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || !line.starts_with('/') {
                continue;
            }
            let path = Path::new(line);
            if !path.exists() {
                continue;
            }
            let base = match path.file_name().and_then(|s| s.to_str()) {
                Some(b) => b.to_string(),
                None => continue,
            };
            if seen.iter().any(|s| s == &base) {
                continue; // dedup by basename, first occurrence wins
            }
            seen.push(base);
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        // No /etc/shells (or nothing usable): fall back to whichever of these
        // common shells actually exist on disk.
        for cand in ["/usr/bin/bash", "/usr/bin/zsh", "/usr/bin/fish", "/bin/bash"] {
            if Path::new(cand).exists() {
                let base = Path::new(cand)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !seen.iter().any(|s| s == &base) {
                    seen.push(base);
                    out.push(cand.to_string());
                }
            }
        }
    }
    out
}

/// Scrollbar thumb color derived from the active theme: theme fg at alpha 160.
fn scrollbar_thumb_for(theme: &jetty_core::Theme) -> [u8; 4] {
    // A DIM shade just above the background — subtle, not glaring. (fg/accent are
    // too bright for a scrollbar.) Blend bg→fg ~35%.
    let bg = theme.bg;
    let fg = theme.fg;
    let mix = |i: usize| (bg[i] as f32 + (fg[i] as f32 - bg[i] as f32) * 0.35) as u8;
    [mix(0), mix(1), mix(2), 210]
}

fn ctrl_hover_at(cx: f32, cy: f32, width: u32, bar_y: f32) -> jetty_render::CtrlHover {
    use jetty_render::CtrlHover;
    if cy < bar_y || cy >= bar_y + TABBAR_H {
        return CtrlHover::None;
    }
    // The controls are inset from the surface's right edge by STRIP_PAD; mirror
    // that here or every hover zone is shifted STRIP_PAD px right of the buttons.
    let sw = width as f32 - jetty_render::STRIP_PAD;
    let ctrl_w = jetty_render::CONTROLS_W / 5.0;
    let help_x = sw - jetty_render::CONTROLS_W; // sw - 5*ctrl_w
    let settings_x = sw - ctrl_w * 4.0;
    let min_x = sw - ctrl_w * 3.0;
    let max_x = sw - ctrl_w * 2.0;
    let close_x = sw - ctrl_w;
    if cx >= sw {
        // Beyond the close button's right edge (in the STRIP_PAD margin).
        CtrlHover::None
    } else if cx >= close_x {
        CtrlHover::Close
    } else if cx >= max_x {
        CtrlHover::Max
    } else if cx >= min_x {
        CtrlHover::Min
    } else if cx >= settings_x {
        CtrlHover::Settings
    } else if cx >= help_x {
        CtrlHover::Help
    } else {
        CtrlHover::None
    }
}

/// Shift every hit-test rect of a `TabBar` down by `dy` so the bar (built at
/// y 0..TABBAR_H) can be placed at the bottom of the window. Mirrors the
/// render-side translate of `bar.quads`/`bar.labels`.
fn translate_bar_rects(bar: &mut jetty_render::TabBar, dy: f32) {
    for r in &mut bar.tab_rects {
        r.y += dy;
    }
    for r in &mut bar.close_rects {
        r.y += dy;
    }
    bar.plus_rect.y += dy;
    bar.help_rect.y += dy;
    bar.settings_rect.y += dy;
    bar.min_rect.y += dy;
    bar.max_rect.y += dy;
    bar.close_rect.y += dy;
}

/// Centre `win` on its current monitor (or the first available monitor if the
/// current one cannot be determined). No-ops gracefully if no monitor info is
/// available.
/// Whether `pos` (a window outer top-left, physical px) lies within some
/// currently-connected monitor. Used to reject a saved Center-mode position that
/// now falls on a since-disconnected monitor (F32).
fn pos_on_some_monitor(win: &Arc<Window>, pos: winit::dpi::PhysicalPosition<i32>) -> bool {
    win.available_monitors().any(|m| {
        let p = m.position();
        let s = m.size();
        pos.x >= p.x
            && pos.x < p.x + s.width as i32
            && pos.y >= p.y
            && pos.y < p.y + s.height as i32
    })
}

fn center_window(win: &Arc<Window>) {
    let mon = win
        .current_monitor()
        .or_else(|| win.available_monitors().next());

    if let Some(mon) = mon {
        let mon_pos = mon.position(); // physical px; nonzero on secondary monitors
        let mon_size = mon.size();
        let win_size = win.outer_size();
        // Center WITHIN the current monitor: add the monitor's origin so a
        // multi-monitor setup centers on the right screen (the old code dropped
        // position() and always centered relative to (0,0) — a real bug).
        let x = mon_pos.x + (mon_size.width.saturating_sub(win_size.width) / 2) as i32;
        let y = mon_pos.y + (mon_size.height.saturating_sub(win_size.height) / 2) as i32;
        win.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    }
}

/// Dock the window as a Yakuake-style top strip on the current monitor: full
/// monitor width (× `width_pct`), `height_pct` of the monitor height, flush to
/// the top edge (y = monitor top), centered horizontally. Sizes/positions are
/// set ONCE per summon (the slide-in is render-side, not a per-frame reposition).
/// On Wayland set_outer_position/request_inner_size are no-ops — accepted
/// degradation, same as the F9 hotkey.
fn dock_window_top(win: &Arc<Window>, width_pct: f32, height_pct: f32) {
    let mon = win
        .current_monitor()
        // A HIDDEN window (the dropdown between summons) reports no
        // current_monitor, so this used to fall straight through to the PRIMARY
        // monitor — re-summoning the dropdown on the wrong screen for multi-monitor
        // users. Prefer the monitor that contains the window's last outer position
        // so it re-appears on the SAME monitor. (If the position is unavailable —
        // e.g. never shown, or Wayland — we fall back to the primary as before, so
        // there is no regression.)
        .or_else(|| {
            win.outer_position().ok().and_then(|a| {
                win.available_monitors().find(|m| {
                    let p = m.position();
                    let s = m.size();
                    a.x >= p.x
                        && a.x < p.x + s.width as i32
                        && a.y >= p.y
                        && a.y < p.y + s.height as i32
                })
            })
        })
        .or_else(|| win.available_monitors().next());
    if let Some(mon) = mon {
        let mon_pos = mon.position();
        let mon_size = mon.size();
        let mon_w = mon_size.width as f32;
        let mon_h = mon_size.height as f32;
        // Clamp to the min_inner_size floor so the strip never collapses.
        let win_w = (mon_w * width_pct).max(400.0).min(mon_w);
        let win_h = (mon_h * height_pct).max(200.0).min(mon_h);
        let x = mon_pos.x + ((mon_w - win_w) / 2.0).round() as i32;
        let y = mon_pos.y; // top-flush
        if std::env::var("JETTY_DEBUG_DOCK").is_ok() {
            eprintln!(
                "jetty dock: chosen monitor pos=({},{}) size={}x{} → target=({},{}) size={}x{}; window currently at outer_position={:?}",
                mon_pos.x, mon_pos.y, mon_size.width, mon_size.height,
                x, y, win_w.round() as u32, win_h.round() as u32,
                win.outer_position(),
            );
        }
        win.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        let _ = win.request_inner_size(winit::dpi::PhysicalSize::new(
            win_w.round() as u32,
            win_h.round() as u32,
        ));
    }
}

/// Returns `true` when `bytes` represent a printable keystroke that should
/// trigger the caret flash+pulse effect.
///
/// Rejects:
/// - empty slices
/// - anything starting with `0x1b` (escape sequences: arrows, F-keys, CSI, etc.)
/// - single bytes < 0x20 (control characters: Enter=0x0d, Tab=0x09, etc.)
/// - single byte `0x7f` (Backspace/Delete)
///
/// Accepts ordinary printable ASCII and multi-byte UTF-8 sequences (which can
/// only occur as actual text — they never start with a control byte).
fn is_printable_keystroke(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // Standalone Escape or any escape sequence (CSI, SS3, etc.)
    if bytes[0] == 0x1b {
        return false;
    }
    // Single-byte control characters (< 0x20) or DEL (0x7f)
    if bytes.len() == 1 && (bytes[0] < 0x20 || bytes[0] == 0x7f) {
        return false;
    }
    true
}

#[cfg(test)]
mod resize_zone_tests {
    use super::{resize_zone_at, ResizeZone};

    const W: u32 = 1000;
    const H: u32 = 640;

    #[test]
    fn interior_is_none() {
        assert_eq!(resize_zone_at(500.0, 320.0, W, H), ResizeZone::None);
    }

    #[test]
    fn edges_map_to_sides() {
        // West/East within 6px of a vertical side (mid-height).
        assert_eq!(resize_zone_at(2.0, 320.0, W, H), ResizeZone::West);
        assert_eq!(resize_zone_at(998.0, 320.0, W, H), ResizeZone::East);
        // North/South within 6px of a horizontal side (mid-width).
        assert_eq!(resize_zone_at(500.0, 2.0, W, H), ResizeZone::North);
        assert_eq!(resize_zone_at(500.0, 638.0, W, H), ResizeZone::South);
    }

    #[test]
    fn corners_take_priority_over_edges() {
        // Within 12px of two adjacent sides → the diagonal corner zone.
        assert_eq!(resize_zone_at(3.0, 3.0, W, H), ResizeZone::NorthWest);
        assert_eq!(resize_zone_at(997.0, 3.0, W, H), ResizeZone::NorthEast);
        assert_eq!(resize_zone_at(3.0, 637.0, W, H), ResizeZone::SouthWest);
        assert_eq!(resize_zone_at(997.0, 637.0, W, H), ResizeZone::SouthEast);
    }

    #[test]
    fn just_inside_edge_band_is_interior() {
        // 7px from the left edge (> EDGE=6, < CORNER=12 only matters near a corner):
        // at mid-height this is interior, not a resize zone.
        assert_eq!(resize_zone_at(7.0, 320.0, W, H), ResizeZone::None);
    }

    #[test]
    fn top_outer_strip_is_resize_inner_is_not() {
        // The top 6px is North (resize); below that (still inside TABBAR_H) is the
        // tab bar, so resize_zone_at returns None there.
        assert_eq!(resize_zone_at(500.0, 3.0, W, H), ResizeZone::North);
        assert_eq!(resize_zone_at(500.0, 20.0, W, H), ResizeZone::None);
    }

    #[test]
    fn out_of_bounds_is_none() {
        assert_eq!(resize_zone_at(-5.0, 320.0, W, H), ResizeZone::None);
        assert_eq!(resize_zone_at(500.0, 700.0, W, H), ResizeZone::None);
    }

    #[test]
    fn directions_and_cursors_pair_up() {
        use winit::window::{CursorIcon, ResizeDirection};
        assert!(ResizeZone::None.direction().is_none());
        assert_eq!(ResizeZone::West.direction(), Some(ResizeDirection::West));
        assert_eq!(ResizeZone::SouthEast.direction(), Some(ResizeDirection::SouthEast));
        assert_eq!(ResizeZone::West.cursor_icon(), CursorIcon::EwResize);
        assert_eq!(ResizeZone::North.cursor_icon(), CursorIcon::NsResize);
        assert_eq!(ResizeZone::NorthWest.cursor_icon(), CursorIcon::NwseResize);
        assert_eq!(ResizeZone::NorthEast.cursor_icon(), CursorIcon::NeswResize);
    }
}

#[cfg(test)]
mod index_adjust_tests {
    use super::App;

    #[test]
    fn clears_when_pointing_at_removed() {
        let mut idx = Some(2);
        App::adjust_index_after_remove(&mut idx, 2);
        assert_eq!(idx, None);
    }

    #[test]
    fn decrements_when_pointing_after_removed() {
        let mut idx = Some(3);
        App::adjust_index_after_remove(&mut idx, 1);
        assert_eq!(idx, Some(2));
    }

    #[test]
    fn unchanged_when_pointing_before_removed() {
        let mut idx = Some(1);
        App::adjust_index_after_remove(&mut idx, 3);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn none_stays_none() {
        let mut idx: Option<usize> = None;
        App::adjust_index_after_remove(&mut idx, 0);
        assert_eq!(idx, None);
    }
}

#[cfg(test)]
mod desktop_exec_arg_tests {
    use super::desktop_exec_arg;

    #[test]
    fn plain_path_is_quoted_verbatim() {
        assert_eq!(desktop_exec_arg("/usr/local/bin/jetty"), "\"/usr/local/bin/jetty\"");
    }

    #[test]
    fn path_with_spaces_stays_one_argument() {
        // The spec parses an unquoted space as an argument separator; quoting
        // keeps "/home/user/My Builds/jetty" a single program path.
        assert_eq!(
            desktop_exec_arg("/home/user/My Builds/jetty"),
            "\"/home/user/My Builds/jetty\"",
        );
    }

    #[test]
    fn percent_is_field_code_escaped() {
        // A literal % must be written %% or the DE consumes it as a field code.
        assert_eq!(desktop_exec_arg("/opt/100%/jetty"), "\"/opt/100%%/jetty\"");
    }

    #[test]
    fn reserved_chars_get_double_backslash_string_escape() {
        // Regression (F35): the spec's general string-escape rule is applied on
        // top of the quoting rule, so a literal `$`/`` ` ``/`"` inside the quotes
        // is written with TWO backslashes and a literal backslash with FOUR.
        // GKeyFile rejects the old single-backslash `\$` as an invalid escape and
        // GNOME autostart then launches nothing.
        let out = desktop_exec_arg("/p/$x/`y`/a\\b/j");
        let expected = String::from("\"")
            + "/p/"
            + "\\\\$"          // \\$  → literal $
            + "x/"
            + "\\\\`" + "y" + "\\\\`" // \\`y\\`
            + "/a"
            + "\\\\\\\\"       // \\\\ → literal backslash
            + "b/j"
            + "\"";
        assert_eq!(out, expected);
        // The invalid single-backslash `\$` escape must NOT appear.
        assert!(out.contains("\\\\$"), "literal $ must be doubly escaped");
    }
}

#[cfg(test)]
mod printable_keystroke_tests {
    use super::is_printable_keystroke;

    #[test]
    fn printable_ascii_lowercase() {
        assert!(is_printable_keystroke(b"a"));
    }

    #[test]
    fn printable_ascii_uppercase() {
        assert!(is_printable_keystroke(b"A"));
    }

    #[test]
    fn printable_utf8_multibyte() {
        // '£' is U+00A3, encoded as 0xC2 0xA3 in UTF-8.
        assert!(is_printable_keystroke("£".as_bytes()));
    }

    #[test]
    fn empty_is_not_printable() {
        assert!(!is_printable_keystroke(b""));
    }

    #[test]
    fn escape_sequence_arrow_up_is_not_printable() {
        assert!(!is_printable_keystroke(b"\x1b[A"));
    }

    #[test]
    fn enter_is_not_printable() {
        assert!(!is_printable_keystroke(b"\r"));
    }

    #[test]
    fn tab_is_not_printable() {
        assert!(!is_printable_keystroke(b"\t"));
    }

    #[test]
    fn backspace_del_is_not_printable() {
        assert!(!is_printable_keystroke(b"\x7f"));
    }

    #[test]
    fn ctrl_c_is_not_printable() {
        assert!(!is_printable_keystroke(b"\x03"));
    }
}

#[cfg(test)]
mod scrollback_cycle_tests {
    use super::{cycle_scrollback, format_scrollback, SCROLLBACK_STEPS};

    #[test]
    fn cycle_scrollback_snaps_and_wraps() {
        // Exact steps move ±1 with wraparound (SummonEffect::cycle semantics).
        assert_eq!(cycle_scrollback(10_000, true), 25_000);
        assert_eq!(cycle_scrollback(100_000, true), 1_000, "forward wraps");
        assert_eq!(cycle_scrollback(1_000, false), 100_000, "backward wraps");
        // A hand-edited value snaps to its NEAREST step, then steps.
        assert_eq!(cycle_scrollback(12_345, true), 25_000);
        assert_eq!(cycle_scrollback(12_345, false), 5_000);
    }

    #[test]
    fn format_scrollback_steps_and_verbatim() {
        // Every cycler step renders in "Nk" form.
        for s in SCROLLBACK_STEPS {
            assert_eq!(format_scrollback(s), format!("{}k", s / 1000));
        }
        // Hand-edited values render verbatim.
        assert_eq!(format_scrollback(12_345), "12345");
        assert_eq!(format_scrollback(100), "100");
    }

    #[test]
    fn cycle_notify_min_snaps_and_wraps() {
        use super::cycle_notify_min;
        // Exact steps move ±1 with wraparound.
        assert_eq!(cycle_notify_min(10, true), 30);
        assert_eq!(cycle_notify_min(300, true), 5, "forward wraps");
        assert_eq!(cycle_notify_min(5, false), 300, "backward wraps");
        // A hand-edited value snaps to its nearest step, then steps.
        assert_eq!(cycle_notify_min(50, true), 120); // nearest is 60 → next 120
        assert_eq!(cycle_notify_min(50, false), 30); // nearest is 60 → prev 30
    }
}

#[cfg(test)]
mod url_open_tests {
    use super::url_scheme_allowed;

    #[test]
    fn allows_http_https_file_case_insensitively() {
        assert!(url_scheme_allowed("http://example.com"));
        assert!(url_scheme_allowed("https://example.com/a?b=c"));
        assert!(url_scheme_allowed("file:///tmp/report.html"));
        assert!(url_scheme_allowed("HTTPS://EXAMPLE.COM"));
        assert!(url_scheme_allowed("HtTp://x.io"));
    }

    #[test]
    fn rejects_everything_else() {
        assert!(!url_scheme_allowed("javascript:alert(1)"));
        assert!(!url_scheme_allowed("mailto:me@example.com"));
        assert!(!url_scheme_allowed("ftp://example.com"));
        assert!(!url_scheme_allowed(""));
        assert!(!url_scheme_allowed("example.com"));
        // Scheme must be a PREFIX, and multibyte text can't panic the check.
        assert!(!url_scheme_allowed("xhttps://example.com"));
        assert!(!url_scheme_allowed("héllo→"));
    }
}

#[cfg(test)]
mod paste_sanitize_tests {
    use super::App;

    #[test]
    fn plain_text_borrows_unchanged() {
        let s = "echo hello\nworld\t!";
        let out = App::strip_paste_end(s.as_bytes());
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*out, s.as_bytes());
    }

    #[test]
    fn embedded_end_marker_is_removed() {
        // The classic injection: an ESC[201~ in the payload would otherwise
        // terminate the bracketed-paste guard and run the rest as commands.
        let out = App::strip_paste_end(b"a\x1b[201~rm -rf ~\n");
        assert_eq!(&*out, b"arm -rf ~\n");
        assert!(!contains(&out, b"\x1b[201~"));
    }

    #[test]
    fn reformed_marker_across_removal_is_defeated() {
        // Crafted so a naive single-pass removal would re-form ESC[201~ by
        // concatenating the surrounding bytes.
        let out = App::strip_paste_end(b"\x1b[2\x1b[201~01~");
        assert!(!contains(&out, b"\x1b[201~"));
    }

    #[test]
    fn multiple_markers_all_removed() {
        let out = App::strip_paste_end(b"\x1b[201~x\x1b[201~y\x1b[201~");
        assert_eq!(&*out, b"xy");
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
}

#[cfg(test)]
mod resolve_title_tests {
    use super::resolve_title;

    #[test]
    fn osc_title_applies_when_not_renamed() {
        assert_eq!(
            resolve_title(Some("x".to_string()), false, "Tab 2"),
            Some("x".to_string())
        );
    }

    #[test]
    fn manual_rename_wins_forever() {
        // Once manually renamed, both new titles and resets are ignored.
        assert_eq!(resolve_title(Some("x".to_string()), true, "Tab 2"), None);
        assert_eq!(resolve_title(None, true, "Tab 2"), None);
    }

    #[test]
    fn reset_restores_default() {
        assert_eq!(resolve_title(None, false, "Tab 2"), Some("Tab 2".to_string()));
    }
}

#[cfg(test)]
mod activity_transition_tests {
    use super::next_activity;
    use jetty_render::TabActivity::{Bell, None as ActNone, Output};

    #[test]
    fn output_lights_a_clean_tab() {
        assert_eq!(next_activity(ActNone, true, false, false), Output);
    }

    #[test]
    fn no_output_keeps_state() {
        assert_eq!(next_activity(ActNone, false, false, false), ActNone);
        assert_eq!(next_activity(Output, false, false, false), Output);
        assert_eq!(next_activity(Bell, false, false, false), Bell);
    }

    #[test]
    fn bell_wins_and_is_never_downgraded() {
        assert_eq!(next_activity(ActNone, true, true, false), Bell);
        assert_eq!(next_activity(Output, false, true, false), Bell);
        // Later output never downgrades a Bell.
        assert_eq!(next_activity(Bell, true, false, false), Bell);
    }

    #[test]
    fn reflow_grace_suppresses_the_output_upgrade() {
        // F3: a SIGWINCH-induced prompt repaint right after an app-initiated
        // reflow must NOT light the dot...
        assert_eq!(next_activity(ActNone, true, false, true), ActNone);
        // ...but it never masks a real bell,
        assert_eq!(next_activity(ActNone, true, true, true), Bell);
        // and never clears an already-lit indicator.
        assert_eq!(next_activity(Output, true, false, true), Output);
    }
}

#[cfg(test)]
mod shift_hint_tests {
    use super::shift_hint_live_in;
    use std::time::{Duration, Instant};

    #[test]
    fn live_only_in_the_tagged_window() {
        let now = Instant::now();
        let hint = Some((now + Duration::from_millis(3500), 7u32));
        // F4: the window the drag happened in shows the pill...
        assert!(shift_hint_live_in(hint, 7u32, now));
        // ...every other window does not.
        assert!(!shift_hint_live_in(hint, 8u32, now));
    }

    #[test]
    fn expired_or_absent_hint_is_dead_everywhere() {
        let now = Instant::now();
        let expired = Some((now - Duration::from_millis(1), 7u32));
        assert!(!shift_hint_live_in(expired, 7u32, now));
        assert!(!shift_hint_live_in(None, 7u32, now));
    }
}

#[cfg(test)]
mod hot_reload_tests {
    use super::{floor_char_boundary, hash_config_str};

    /// The self-write guard: a reload is IGNORED iff the on-disk content hashes to
    /// the value we last wrote (our own save echoing back through the watcher).
    fn is_own_write(observed: u64, last_written: Option<u64>) -> bool {
        last_written == Some(observed)
    }

    #[test]
    fn self_write_hash_guard() {
        let a = "theme = \"dracula\"\nopacity = 0.9\n";
        let b = "theme = \"nord\"\nopacity = 0.9\n";
        // Deterministic within a run: identical content → identical hash.
        assert_eq!(hash_config_str(a), hash_config_str(a));
        assert_ne!(hash_config_str(a), hash_config_str(b));
        // Our own write echoing back is recognized and skipped...
        assert!(is_own_write(hash_config_str(a), Some(hash_config_str(a))));
        // ...an EXTERNAL edit (different content) is applied.
        assert!(!is_own_write(hash_config_str(b), Some(hash_config_str(a))));
        // No prior write recorded → never treated as our own.
        assert!(!is_own_write(hash_config_str(a), None));
    }

    #[test]
    fn osc52_reply_cap_never_splits_a_char() {
        // The paste-reply cap must land on a char boundary so String::truncate can't
        // panic on a multibyte char straddling the cap.
        let s = "a£b€c"; // '£' is 2 bytes, '€' is 3 bytes
        for max in 0..=s.len() + 2 {
            let b = floor_char_boundary(s, max);
            assert!(b <= s.len());
            assert!(s.is_char_boundary(b), "cap {max} landed mid-char at {b}");
        }
        // A cap at/after the end returns the full length.
        assert_eq!(floor_char_boundary(s, s.len()), s.len());
        assert_eq!(floor_char_boundary(s, s.len() + 10), s.len());
    }

    /// Which config keys apply LIVE on hot-reload vs require a RESTART/external action.
    /// Mirrors `apply_reloaded_config` (live keys are applied there; `summon_hotkey`
    /// and `launch_at_login` are deliberately skipped). Test-only classifier so the
    /// documented contract is locked in.
    fn is_restart_only(key: &str) -> bool {
        matches!(key, "summon_hotkey" | "launch_at_login")
    }

    #[test]
    fn live_vs_restart_key_classification() {
        // Restart/external-only keys.
        assert!(is_restart_only("summon_hotkey"));
        assert!(is_restart_only("launch_at_login"));
        // Everything else applies live on reload.
        for k in [
            "theme",
            "opacity",
            "font_size",
            "font_family",
            "ui_font_size",
            "ui_font_family",
            "corner_radius",
            "summon_effect",
            "window_mode",
            "tab_bar_position",
            "dropdown_height_pct",
            "dropdown_width_pct",
            "focus_autohide",
            "scrollback_lines",
            "show_perf_hud",
            "effects",
            "osc52_allow_paste",
            "hot_reload",
            // shell (new tabs pick up the edited shell) and show_welcome apply live;
            // both are also mirrored in apply_reloaded_config so a later persist()
            // round-trips an external edit instead of clobbering it.
            "shell",
            "show_welcome",
            // keybindings recompile live in apply_reloaded_config (not restart-only).
            "keys",
        ] {
            assert!(!is_restart_only(k), "{k} should be live-appliable");
        }
    }
}

#[cfg(test)]
mod paint_choke_tests {
    //! v0.23 central-paint-chokepoint tripwire. A cheap `cargo test` companion to
    //! `scripts/check-paint-choke.sh` (which does the richer context-aware audit):
    //! it counts the raw `.request_redraw()` calls that are ALLOWED to remain
    //! (the two choke definitions + the whitelisted animation/lifecycle self-drive
    //! sites) and fails if the total moves. Any NEW raw producer `request_redraw`
    //! bumps the count → this test trips → run the script to see which site leaked,
    //! then route it through a per-surface paint choke (or, if it is a genuine
    //! animation self-drive, extend the whitelist AND bump the number here).
    //!
    //! This is a TRIPWIRE, not a proof: it counts, it does not classify. It cannot
    //! catch a swap (removing a whitelisted call while adding a producer one keeps
    //! the count equal) — the shell script's context check is the real guard.

    fn raw_calls(src: &str) -> usize {
        // Match the CALL form `…request_redraw();` (with the trailing semicolon) so
        // this test's own prose/string mentions of the bare `request_redraw()` token
        // are not counted; also skip comment lines.
        let needle = concat!(".request_redraw", "();");
        src.lines()
            .filter(|l| {
                let s = l.trim_start();
                l.contains(needle) && !s.starts_with("//") && !s.starts_with('*')
            })
            .count()
    }

    #[test]
    fn no_new_raw_request_redraw_in_app() {
        // 14 = request_main_paint def (1) + request_settings_paint def (1)
        //    + about_to_wait animation/lifecycle self-drive (7)
        //    + main render-tail (1) + detached render-tail in render_detached_window (1)
        //    + dock re-assert (1) + center re-assert (1)
        //    + main-window-open first-frame nudge on a local `window` binding (1).
        assert_eq!(
            raw_calls(include_str!("app.rs")),
            14,
            "raw request_redraw count changed in app.rs — run scripts/check-paint-choke.sh"
        );
    }

    #[test]
    fn no_new_raw_request_redraw_in_detached() {
        // 2 = DetachedWindow::request_paint def (1) + DetachedWindow::new first-frame
        //     nudge on a local `window` binding (1).
        assert_eq!(
            raw_calls(include_str!("detached.rs")),
            2,
            "raw request_redraw count changed in detached.rs — run scripts/check-paint-choke.sh"
        );
    }
}
