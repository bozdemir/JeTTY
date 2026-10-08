use std::io::Write;
use crate::overlays::{HintDrawData, HintState, Overlays, PaletteDrawData, Surface};
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
#[derive(Debug, Clone)]
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
    /// A user-facing configuration problem found off the UI thread (a refused
    /// save, an unusable summon hotkey) — shown as a status pill + logged.
    ConfigNotice(String),
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
/// fired. Main tabs key on their stable [`TabId`]; detached windows on their
/// window id.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NotifyKey {
    MainTab(TabId),
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
/// is a Yakuake-style top-anchored full-width strip that slides down;
/// `Fullscreen` covers the whole monitor.
///
/// This is a summon-geometry POLICY (persisted as `window_mode`), NOT the live
/// shape: the transient per-window F11 toggle lives in `App::main_fullscreen` /
/// `DetachedWindow::fullscreen` and never writes this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Center,
    Dropdown,
    /// Summon covering the WHOLE monitor the window is on (borderless
    /// fullscreen). Rounded corners are suppressed while fullscreen (a radius
    /// would show the desktop through four notches at the screen edges), and
    /// neither the dock/center re-assertion counters nor the dropdown slide are
    /// ever armed in this mode.
    Fullscreen,
}

impl WindowMode {
    const ORDER: [WindowMode; 3] =
        [WindowMode::Center, WindowMode::Dropdown, WindowMode::Fullscreen];

    fn display_name(self) -> &'static str {
        match self {
            WindowMode::Center => "Center",
            WindowMode::Dropdown => "Dropdown",
            WindowMode::Fullscreen => "Fullscreen",
        }
    }

    fn cycle(self, forward: bool) -> WindowMode {
        let i = Self::ORDER.iter().position(|&m| m == self).unwrap_or(0);
        let n = Self::ORDER.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        Self::ORDER[j]
    }

    /// Case-SENSITIVE, unknown ⇒ `Center` — unchanged, which is also what gives
    /// forward compatibility for free: an OLDER JeTTY reading
    /// `window_mode = "fullscreen"` falls into `_ => Center` and starts fine.
    fn from_config(s: &str) -> WindowMode {
        match s {
            "dropdown" => WindowMode::Dropdown,
            "fullscreen" => WindowMode::Fullscreen,
            _ => WindowMode::Center,
        }
    }

    fn to_config(self) -> &'static str {
        match self {
            WindowMode::Center => "center",
            WindowMode::Dropdown => "dropdown",
            WindowMode::Fullscreen => "fullscreen",
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

// The tab bar / detached title bar, the bottom status strip (the live perf HUD)
// and the toast pills are sized by each window's `jetty_render::ChromeMetrics`
// (DPI × UI font size) — never a fixed px constant, which the glyphs overflowed
// at large UI fonts / on HiDPI. See `App::chrome_metrics`, `App::bar_h`,
// `App::status_h` and `DetachedWindow::chrome_metrics`.

/// Width reserved on the right of the grid for the scrollbar (a gutter), so the
/// terminal never renders content underneath the scrollbar (which would cover the
/// last column / p10k's right-aligned prompt at some window widths). Scrollbar
/// width + a few px of breathing room.
const SCROLLBAR_GUTTER: f32 = jetty_render::SCROLLBAR_W + 4.0;

/// Maximum bytes of PTY output fed into one tab's terminal per drain pass. Under
/// an output flood (`yes`, `cat huge.log`) the PTY can produce faster than the VT
/// parser consumes; draining to empty in one go would never return to the winit
/// loop (no redraws, no keyboard — the user could not even Ctrl+C the flood).
/// The drain stops after this many bytes and `about_to_wait` re-arms the tab's
/// wake (`PtySession::rearm_wake`), so the rest is drained in the NEXT loop
/// iteration — after pending input events. The backlog itself is bounded by the
/// PTY read queue (the reader blocks and the child with it), not by this.
const PTY_DRAIN_BUDGET: usize = 2 * 1024 * 1024;


/// Wrap period (seconds) for the CRT animation phase before it is narrowed to
/// f32. The three sub-effects all use INTEGER angular frequencies (roll 6,
/// flicker 50, jitter 80 & 13 rad/s — see `crt.rs`), so every whole multiple of
/// TAU is a seamless wrap point. Wrapping keeps the value small so f32's 24-bit
/// mantissa never coarsens the animation step after long uptime (an un-wrapped
/// `elapsed().as_secs_f32()` degrades into visible stutter after ~1.5 days).
const CRT_PHASE_WRAP: f64 = std::f64::consts::TAU;

/// A tab's STABLE identity for its whole life (main window ↔ detached window
/// included). Long-lived references — the rename box, the close confirmation,
/// the tab menu, a tab drag, palette entries, notification keys — hold this,
/// never a `Vec` index, so closing or moving another tab can't retarget them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TabId(pub(crate) u64);

/// A single terminal session: its grid model, PTY, writer, and tab title. One
/// `Tab` per visible tab. Per-tab scroll/selection live inside `terminal`.
pub(crate) struct Tab {
    /// Stable identity (see [`TabId`]).
    pub(crate) id: TabId,
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
    /// Run-selection-in-new-tab: a command staged for injection once this
    /// tab's shell is ready (first OSC 133 A + bracketed paste; see
    /// `runsel::poll_pending`). `None` for every tab that never uses the
    /// feature — the drain hook's only cost then is one `is_some()` branch.
    /// ANY user-originated PTY write to this tab cancels it
    /// (`runsel::cancel_on_user_write` in every input funnel). The field
    /// rides a `detach_tab` move into a `DetachedWindow` — the detached
    /// funnels cancel it the same way, and the detached drain services it.
    pub(crate) pending_inject: Option<crate::runsel::PendingInject>,
    /// Keys this tab was sent a press for (their releases are owed to it under
    /// the kitty keyboard protocol) and its last observed focus (DECSET 1004).
    pub(crate) input: input::TabInputState,
}

/// Which surface a run-selection trigger fired from: the main window's active
/// tab, or a detached window's (single) tab. The destination is ALWAYS a new
/// main-window tab; the source decides whose selection/cwd are used and where
/// feedback pills go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SelSource {
    Main,
    Detached(usize),
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
        // OSC 0/2 titles are program-controlled and can be megabytes: keep only
        // what any title surface (tab bar, OS title, palette, confirm dialog)
        // could show, so no downstream path ever measures/draws/matches a huge
        // string.
        Some(t) => {
            let (head, cut) = jetty_render::clip_head(&t);
            Some(if cut { head.to_string() } else { t })
        }
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

/// How long after a window's last ACTIVE frame its perf HUD flips to "idle".
const PERF_IDLE_AFTER: std::time::Duration = std::time::Duration::from_millis(700);
/// The reading the one-shot idle repaint paints.
const PERF_IDLE_TEXT: &str = "⚡ idle · 0% CPU · 0 MB/s";

/// Exponentially smoothed frame time (ms) of ONE window: the dt since its
/// previous rendered frame. A gap over 1 s (idle) restarts the average instead
/// of spiking it.
fn smooth_frame_ms(
    perf_ms: &mut f32,
    last_frame_at: &mut Option<std::time::Instant>,
    now: std::time::Instant,
) {
    if let Some(prev) = *last_frame_at {
        let dt_ms = now.duration_since(prev).as_secs_f32() * 1000.0;
        if dt_ms <= 1000.0 {
            *perf_ms = if *perf_ms <= 0.0 { dt_ms } else { *perf_ms * 0.9 + dt_ms * 0.1 };
        }
    }
    *last_frame_at = Some(now);
}

/// The live perf-HUD line: frame ms, the fps it implies, process CPU%, VT MB/s.
fn perf_hud_text(ms: f32, cpu: f32, mb: f32) -> String {
    let ms = if ms > 0.0 { ms } else { 0.0 };
    let fps = if ms > 0.0 { (1000.0 / ms).round().clamp(0.0, 9999.0) as i32 } else { 0 };
    format!("⚡ {ms:.1} ms · {fps} fps · {cpu:.0}% CPU · {mb:.0} MB/s")
}

/// What the perf HUD's idle one-shot owes the loop this iteration. After the
/// last ACTIVE frame the loop wakes ONCE (`perf_idle_at`) to repaint the HUD as
/// an honest "idle" reading, then goes fully idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleHud {
    /// Nothing owed: HUD off, idle state already painted, never armed, or the
    /// window is not effectively visible.
    Nothing,
    /// The deadline passed: request the single idle repaint now.
    RepaintNow,
    /// Wake exactly once at this (future) deadline.
    WakeAt(std::time::Instant),
}

/// Pure decision behind [`IdleHud`]. Only an EFFECTIVELY VISIBLE window (shown
/// AND not occluded) may schedule the repaint: a hidden window's
/// `RedrawRequested` early-returns before `perf_idle_shown` is set, and winit's
/// X11 loop delivers redraws to unmapped windows, so requesting it while hidden
/// re-requested it on every iteration — one core pinned at 100% for as long as
/// the window stayed hidden. An elapsed deadline must not reach `WaitUntil`
/// either (a past `WaitUntil` returns immediately → the same spin).
fn perf_idle_decision(
    show_hud: bool,
    idle_shown: bool,
    idle_at: Option<std::time::Instant>,
    effectively_visible: bool,
    now: std::time::Instant,
) -> IdleHud {
    match idle_at {
        Some(d) if show_hud && !idle_shown && effectively_visible => {
            if now >= d {
                IdleHud::RepaintNow
            } else {
                IdleHud::WakeAt(d)
            }
        }
        _ => IdleHud::Nothing,
    }
}

/// What an F9 / `jetty --toggle` / launcher press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToggleAction {
    Show,
    Hide,
    /// Visible but not in front of the user: raise + focus instead of hiding.
    Raise,
}

/// How long a refused raise keeps counting: a toggle within this window of a
/// raise whose focus never arrived hides instead of raising again.
const RAISE_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

/// How long after losing focus the main window still counts as FOCUSED for a
/// toggle. On X11 the summon hotkey is a passive key grab (global-hotkey's
/// `XGrabKey` on the root): pressing it makes the server send the focused
/// window a FocusOut (mode NotifyGrab) — winit reports it as `Focused(false)`,
/// it does not filter by mode — and the matching FocusIn only on key RELEASE,
/// while the hotkey's own event reaches the loop 20–50 ms AFTER the FocusOut
/// (global-hotkey polls the X connection every 50 ms). Measured in Xvfb with
/// XTEST: `Focused(false)` → +20.7 ms hotkey → +30.4 ms `Focused(true)` for a
/// 30 ms tap; `Focused(false)` → +29 ms hotkey → +400 ms `Focused(true)` for a
/// 400 ms hold. Without this grace every F9 on a focused window looked
/// "unfocused" and RAISED it instead of hiding it. 300 ms covers a busy loop.
const FOCUS_CHURN_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// Pure toggle decision. Hiding is right only when the user is LOOKING at the
/// terminal (shown, focused, not occluded); "focused" includes a focus loss
/// within [`FOCUS_CHURN_GRACE`] — the hotkey's own grab churn. A window that is
/// shown but behind other windows or genuinely unfocused (`focus_autohide =
/// false` leaves it up after the user clicks elsewhere) is RAISED instead — the
/// old `!visible` toggle hid it, so getting it back took a second press. If the
/// compositor refuses the raise (focus never arrives, e.g. Wayland without an
/// activation token), the next press within [`RAISE_RETRY_WINDOW`] hides, so
/// the key can never get stuck re-raising a window it cannot focus.
///
/// A hidden window whose focus-loss AUTO-hide fired within the churn grace
/// stays hidden (`Hide` on a hidden window is a no-op): that FocusOut was this
/// very press's grab, and the auto-hide merely beat a slow hotkey event to it —
/// the user asked to hide, so re-showing it would flash the window back up.
fn toggle_action(
    visible: bool,
    focused: bool,
    occluded: bool,
    focus_lost_at: Option<std::time::Instant>,
    autohidden_at: Option<std::time::Instant>,
    last_raise: Option<std::time::Instant>,
    now: std::time::Instant,
) -> ToggleAction {
    let within_churn = |t: Option<std::time::Instant>| {
        t.is_some_and(|t| now.saturating_duration_since(t) < FOCUS_CHURN_GRACE)
    };
    if !visible {
        return if within_churn(autohidden_at) {
            ToggleAction::Hide
        } else {
            ToggleAction::Show
        };
    }
    if (focused || within_churn(focus_lost_at)) && !occluded {
        return ToggleAction::Hide;
    }
    match last_raise {
        Some(t) if now.saturating_duration_since(t) < RAISE_RETRY_WINDOW => ToggleAction::Hide,
        _ => ToggleAction::Raise,
    }
}

/// Retry state for a frame whose swapchain acquire failed (`acquire_frame()` →
/// `None`: Outdated, Lost, Timeout, Occluded, Validation). Without it a failed
/// acquire on the LAST damage-driven frame left the screen stale until some
/// unrelated event; re-requesting blindly would spin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AcquireRetry {
    /// When the next attempt is due.
    pub(crate) due: std::time::Instant,
    /// 0 for the first retry; drives the backoff.
    pub(crate) attempt: u32,
}

/// The next retry after a failed acquire (or a retry that is being issued now):
/// the next frame (16 ms), then doubling up to a 1 s ceiling that repeats while
/// the surface stays broken — bounded wakes, never a spin, never a permanently
/// stale screen.
pub(crate) fn next_acquire_retry(
    prev: Option<AcquireRetry>,
    now: std::time::Instant,
) -> AcquireRetry {
    let attempt = prev.map_or(0, |r| r.attempt.saturating_add(1));
    let ms = (16u64 << attempt.min(6)).min(1000);
    AcquireRetry { due: now + std::time::Duration::from_millis(ms), attempt }
}

/// Wall-clock expiry of a self-driven animation that started at `start` and
/// lasts `secs` (≤ 0 = ends at once). Animations end by TIME in `about_to_wait`,
/// not only inside a successfully presented frame, so a window whose swapchain
/// acquire keeps failing (macOS ordered-out, a Wayland Timeout, the no-GPU
/// fallback) can never pin the loop in `Poll`.
fn anim_expired(start: std::time::Instant, secs: f32, now: std::time::Instant) -> bool {
    secs <= 0.0 || now.saturating_duration_since(start).as_secs_f32() >= secs
}

/// Grace between a PTY-bound keystroke and its FALLBACK paint. The keystroke
/// itself does not paint: the shell's echo (drained a few ms later — zsh
/// highlighters and autosuggestions included) paints the frame that shows it.
/// Painting on the key rendered a frame WITHOUT the echo, and the echo frame
/// then queued a vsync behind it (+1 frame of keypress→glyph latency). The
/// deadline only matters for keys that produce no output (a password prompt,
/// the snap back to the bottom from scrollback) — 25 ms is below perception.
const KEY_ECHO_GRACE: std::time::Duration = std::time::Duration::from_millis(25);

/// Whether a caret-flash burst may drive continuous (`Poll`) frames: only once
/// its FIRST frame was painted by the echo (or the grace fallback) — otherwise
/// the `Poll` would render exactly the pre-echo frame the deferral avoids.
fn caret_drives_frames(
    caret_anim: Option<std::time::Instant>,
    key_paint_due: Option<std::time::Instant>,
) -> bool {
    caret_anim.is_some() && key_paint_due.is_none()
}

/// Whether the user is watching THIS main tab: only the ACTIVE tab of a
/// watched main window is on screen. A background tab's completion must notify
/// even while the user looks at another tab of a focused window (its activity
/// dot is easy to miss).
fn main_tab_watched(main_watching: bool, tab: usize, active: usize) -> bool {
    main_watching && tab == active
}

// The Settings window size is DERIVED at runtime from the panel's scaled size —
// see `App::desired_settings_logical_size` — so it fits ANY UI font (size or
// family), not just the default; the window is also user-resizable.

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
    /// The global summon-hotkey manager. On macOS it must be created AND kept on
    /// the main thread (an upstream `global-hotkey` requirement), so it lives here.
    /// Elsewhere an off-thread worker creates it (registering blocks on X11
    /// round-trips) and keeps it alive itself, so this is a launched-once sentinel.
    #[cfg(target_os = "macos")]
    hotkey_manager: Option<global_hotkey::GlobalHotKeyManager>,
    #[cfg(not(target_os = "macos"))]
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
    /// Whether the MAIN window is currently in OS fullscreen. A MIRROR of the
    /// window's real state, kept so the render path never calls the syscall-backed
    /// `Window::fullscreen()` per frame (it feeds `effective_corner_radius_px` and
    /// the dock/center re-assertion guards).
    ///
    /// SESSION-ONLY — never persisted, never read by `apply_reloaded_config`.
    /// `window_mode == Fullscreen` is the persisted INTENT; this is the live
    /// SHAPE, which an ad-hoc `ToggleFullscreen` (F11) can also set in Center /
    /// Dropdown mode.
    ///
    /// INVARIANT (rule F0): `!main_fullscreen` whenever `!visible`. The OS
    /// fullscreen state is never held while the window is hidden — both hide
    /// paths leave fullscreen BEFORE `set_visible(false)` and the summon path
    /// re-enters it AFTER `set_visible(true)`. That is what makes the enter a
    /// genuine `None → Some` transition winit's X11 backend cannot dedupe away,
    /// keeps `set_simple_fullscreen` off an unmapped (screen-less) macOS window,
    /// and stops an ad-hoc F11 leaking across a hide/summon round-trip.
    ///
    /// KNOWN LIMITATION — a WM-initiated fullscreen change DESYNCS this mirror.
    /// There is no winit event for "the WM took the window in or out of
    /// fullscreen" (and `Window::fullscreen()` returns our own cached intent, not
    /// the WM state — which is why the `window_is_fullscreen` detector was
    /// deleted: it could not detect). So if the user fullscreens JeTTY with a WM
    /// keybinding or the window menu, the mirror is stale: corners stay rounded on
    /// a fullscreen window, the ▢ button and the resize edges behave for the wrong
    /// shape, and — the one that matters — `exit_main_fullscreen_bare` returns
    /// early on the next hide, so the OS fullscreen state survives the hidden
    /// period and the next summon's enter can be deduped away by the X11 backend.
    /// It self-heals: one F11 (or ▢) re-syncs the mirror, two put the window back.
    /// Inherent to a mirror; the alternative (polling the WM state per frame) is a
    /// syscall on the hot path, which this field exists to avoid.
    main_fullscreen: bool,
    /// Hide the window on focus loss (Yakuake auto-hide). Default ON.
    focus_autohide: bool,
    /// Scrollback history limit in lines (config `scrollback_lines`, clamped
    /// 100..=100_000, default 10_000). Applied live to every tab (main +
    /// detached) when changed via the Settings cycler.
    scrollback_lines: usize,
    /// Launch JeTTY at login (config `launch_at_login` — the source of truth: the
    /// XDG autostart entry / macOS LaunchAgent is (re)written or removed to match
    /// at startup, on a hot-reload, and when toggled in Settings or the palette).
    launch_at_login: bool,
    /// Global summon hotkey string (e.g. "F9", "F12", "Ctrl+Shift+F12"). Parsed
    /// by `global_hotkey`'s own `HotKey::from_str`. Default "F9".
    summon_hotkey: String,
    /// Shell to launch (the `shell` config key). Empty = auto-detect
    /// ($SHELL → passwd → /bin/bash); a path forces that shell.
    shell: String,
    /// Cached "the window's top edge touches its monitor's top" (drives the
    /// square top corners in Dropdown mode). On X11 `outer_position()` is a
    /// blocking server round-trip, so this is recomputed only when
    /// `top_flush_dirty` (set by Moved / Resized / ScaleFactorChanged / a
    /// summon) — never per frame — and never mid-slide (the slide is a content
    /// y-offset; the window itself does not move).
    top_flush_pos: bool,
    /// The window may have moved since `top_flush_pos` was computed.
    top_flush_dirty: bool,
    /// Cached tab-bar metadata (title, is-active), rebuilt only when the tab
    /// titles or the active index change. Avoids cloning every tab title on
    /// every RedrawRequested (incl. animation frames) — speed-first hot path.
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
    /// When a toggle last RAISED (rather than hid) the visible-but-unfocused main
    /// window. Cleared when focus arrives; a toggle within `RAISE_RETRY_WINDOW`
    /// of a raise that never got focus hides instead (see `toggle_action`).
    raise_attempt_at: Option<std::time::Instant>,
    /// When the main window last LOST focus (`Focused(false)`); `None` while it
    /// holds focus. A toggle within `FOCUS_CHURN_GRACE` of it still counts as
    /// focused — on X11 the summon hotkey's own key grab produces exactly such a
    /// FocusOut just before the hotkey event arrives (see `toggle_action`).
    focus_lost_at: Option<std::time::Instant>,
    /// When the focus-loss AUTO-hide last hid the main window; cleared on show.
    /// A toggle within `FOCUS_CHURN_GRACE` of it keeps the window hidden (the
    /// auto-hide beat a slow hotkey event to the same press).
    autohidden_at: Option<std::time::Instant>,
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
    /// idle. While Some (and the window is effectively visible) `about_to_wait`
    /// pumps frames; it ends by wall clock there (`anim_expired`) as well as in
    /// the frame that reaches t ≥ 1. None = idle 0 CPU.
    summon_anim: Option<std::time::Instant>,
    /// Start instant of the active caret flash+pulse animation, or None when idle.
    /// Set on every printable keystroke (re-armed each time); ends by wall clock
    /// in `about_to_wait` or in the frame that reaches t ≥ 1. While Some it pumps
    /// frames via Poll — but only after its first frame (`key_paint_due`).
    caret_anim: Option<std::time::Instant>,
    /// Fallback paint deadline for the latest PTY-bound keystroke in the MAIN
    /// window (`KEY_ECHO_GRACE` after it). The keystroke does not paint; the echo
    /// does. Cleared by any main frame; if the echo never comes (no-output key)
    /// `about_to_wait` paints once at the deadline.
    key_paint_due: Option<std::time::Instant>,
    /// Backoff for the main window after `acquire_frame()` failed (see
    /// `AcquireRetry`); `None` while frames present normally. While `Some`,
    /// continuous animation is suspended and the retry schedule paints instead.
    acquire_retry: Option<AcquireRetry>,
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
    /// Backoff after the Settings window's `acquire_frame()` failed (same
    /// `AcquireRetry` schedule as the main/detached windows); `None` while its
    /// frames present normally or while it is closed.
    settings_acquire_retry: Option<AcquireRetry>,
    /// After a GPU device loss whose rebuild failed (the driver still resetting),
    /// the next attempt is not before this instant — see `recover_lost_gpu`.
    gpu_rebuild_retry_at: Option<std::time::Instant>,
    /// When the main window last presented a frame — the anchor of flood frame
    /// pacing (`pace_paint`).
    last_present_at: Option<std::time::Instant>,
    /// The main window's display refresh interval (from its monitor's rate;
    /// 60 Hz when unknown), refreshed on create / move / DPI change.
    frame_interval: std::time::Duration,
    /// A flood paint deferred to the next refresh (`pace_paint`); serviced by
    /// `about_to_wait` as a single WaitUntil wake.
    paced_paint_at: Option<std::time::Instant>,
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
    /// Set when a HIDE dropped a still-pending `reflow_pending_at` deadline, so
    /// the next summon re-arms it once (see the hide legs).
    ///
    /// Rule F0 exits OS fullscreen while the window is still MAPPED, so a hide
    /// from Fullscreen mode shrinks the frame monitor→windowed and the resulting
    /// `Resized` arms the 250 ms debounce. `reflow_due` is not gated on
    /// `self.visible`, so without this the reflow fired 250 ms LATER, while
    /// hidden, SIGWINCHing every tab to a grid the user never sees — then again
    /// on the next summon: two reflows per F9 cycle, feeding the p10k
    /// prompt-scatter bug v0.23.1 fixed. Clearing the deadline alone would be
    /// wrong for the other case (resize the window, hit F9 within 250 ms) because
    /// `gpu.resize` already ran at the `Resized` event, leaving grid ≠ surface —
    /// hence this flag: exactly ONE debounced reflow per VISIBLE transition, at
    /// the final geometry, and never one while hidden.
    reflow_deferred_by_hide: bool,
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
    /// macOS Option-as-Meta sides (mirrors `Config.macos_option_as_alt`; live on
    /// reload, written back by `persist`). Feeds `input::KeyOptions::native`.
    macos_option_as_alt: input::OptionAsAlt,
    /// Where a finished mouse selection is copied (mirrors
    /// `Config.copy_on_select`; live on reload, written back by `persist`).
    copy_on_select: clipboard::CopyOnSelect,
    /// Compiled keybindings (built from `keys` on load / reload). The input path
    /// does ONE cheap hashmap lookup against this per keypress — never per frame.
    keymap: crate::keymap::KeyMap,
    /// The user's raw `[keys]` overrides (mirrors `Config.keys`). Kept so `persist()`
    /// round-trips them and the "Reset keybindings" palette command can clear them.
    keys: crate::config::KeyBindings,
    /// Cached help-overlay rows, regenerated from `keymap` on load/reload so the
    /// Help panel reflects remaps. Cloned only when the overlay is actually drawn.
    help_rows: Vec<String>,
    /// The config/themes file watcher. MUST be kept alive for the process lifetime
    /// (dropping it stops watching). `None` when hot-reload is off; dropped on a live
    /// turn-off. Re-armed after every reload so a recreated dir, a retargeted
    /// symlink or a later `themes/` keep being watched.
    config_watcher: Option<crate::watch::ConfigWatcher>,
    /// Set true ONLY for the duration of `reload_config_and_themes`. While set,
    /// `persist()` is a NO-OP — so a reload applying live keys through the normal
    /// setters can never write config.toml, making the watcher loop-free BY
    /// CONSTRUCTION (amendment H2), independent of the hash guard.
    reloading: bool,
    /// Saves settings changes to config.toml: debounced, in place (only changed
    /// keys; comments and unknown keys survive) and off the UI thread. It also
    /// owns the self-write hash guard: a reload whose on-disk content is our own
    /// write echoing back through the watcher is skipped (the secondary loop guard
    /// after `reloading`). `RefCell` so `persist(&self)` can record a change.
    persister: std::cell::RefCell<crate::config::Persister>,
    /// The theme the user CHOSE (config `theme`), kept separate from the theme on
    /// screen: if it is missing or broken (a user theme file mid-edit), a fallback
    /// is shown but this name is what gets saved and re-resolved on every reload,
    /// so fixing the file brings it back — the fallback never replaces the choice.
    theme_name: String,
    /// Config / theme / keybinding problems found while starting up, printed in
    /// the first tab once it exists (a desktop launch has no visible stderr).
    startup_warnings: Vec<String>,
    /// "Reset keybindings" asks for confirmation: the first run arms it until this
    /// instant; running it again before then resets (after writing a backup).
    reset_keys_armed_until: Option<std::time::Instant>,
    /// Start hidden (`jetty --background`, used by the login autostart entry): the
    /// window is created unmapped and the first summon shows it.
    start_hidden: bool,
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
    /// The same modifiers with their left/right sides (winit's full
    /// `ModifiersChanged` payload): the key encoder needs WHICH Option key is
    /// held for `macos_option_as_alt = "left" | "right"`. One store serves every
    /// window — only the focused window receives key events.
    key_modifiers: winit::event::Modifiers,
    /// The IME's in-progress composition (`Ime::Preedit`) in the main window,
    /// drawn at the cursor until it commits or is cancelled.
    ime_preedit: Option<String>,
    /// Last IME candidate-window anchor handed to winit for the main window
    /// (`input::ime_cursor_area`): re-sent only when the cursor cell moves.
    ime_area: Option<(i32, i32, u32, u32)>,
    /// Last known cursor position in physical pixels.
    cursor: (f64, f64),
    /// The main window's grid mouse state (buttons held by the program, click
    /// counting, edge auto-scroll) — the same `gridmouse` logic every detached
    /// window runs on its own copy.
    grid_mouse: crate::gridmouse::GridMouse,
    /// The one-time "Shift+right-click opens JeTTY's menu" pill was shown (a
    /// right click now goes to a program that tracks the mouse).
    right_click_hint_shown: bool,
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
    /// Run-selection status pill: `(message, until, window)` — drawn only in
    /// window `window` while `now < until` (same window-tagged discipline as
    /// `shift_hint_until`). Carries refusal ("needs bracketed paste") and
    /// staged ("press Enter to run") feedback. `None` when idle — the render
    /// path's only cost then is one `Option` check per frame already happening.
    status_pill: Option<(String, std::time::Instant, winit::window::WindowId)>,
    /// True while ANY tab (main or detached) may hold a `pending_inject`.
    /// The `about_to_wait` gate: when false (the always case), run-selection
    /// adds ONE bool check per loop iteration and nothing else. Set on arm;
    /// recomputed (self-healing after fires/cancels) by the deadline service.
    runsel_active: bool,
    /// Config `run_selection` (default true): the whole feature's opt-out.
    /// Checked once at trigger time — every surface funnels through
    /// `run_selection_in_new_tab`. Hot-reloadable.
    run_selection_enabled: bool,
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
    /// Disabled context-menu indices, computed ONCE at menu open (needs-a-
    /// selection rows: Copy=0 and Run in New Tab=2; Run also when the feature
    /// is config-disabled) and cached beside `menu_item_rects` so hover/click
    /// and the per-frame rebuild never re-query the terminal.
    menu_disabled: Vec<usize>,
    /// Index of the menu item currently under the cursor (for hover highlight).
    menu_hover: Option<usize>,
    /// Next [`TabId`] to hand out (monotonic; ids are never reused).
    next_tab_id: u64,
    /// Inline tab rename: `Some(tab)` while the user is editing a tab title.
    renaming: Option<TabId>,
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

    /// The main window's overlays (search bar, help, command palette, hint
    /// mode, copy-mode). Every detached window owns its own `Overlays`
    /// (`DetachedWindow::ov`); operations name their window with a
    /// [`Surface`].
    ov: Overlays,
    /// When `Some(tab)`, a "Close this tab?" confirmation popup is open for that
    /// tab. The × click / Ctrl+Shift+W / Ctrl+D set this instead of closing
    /// immediately; Enter (or the Close button) confirms, Esc (or Cancel /
    /// click-outside) clears. A tab that vanished meanwhile just drops it.
    confirm_close: Option<TabId>,
    /// Set when the user tries to close the whole app (window × button or the OS
    /// CloseRequested). Shows a "Quit JeTTY?" popup instead of exiting; Enter
    /// confirms, Esc / Cancel / click-outside dismisses.
    confirm_quit: bool,
    /// Where the window was when last hidden, so re-summoning (F9) restores it to
    /// the spot the user left it instead of always re-centering. `None` until the
    /// first hide; the first open is centered.
    last_pos: Option<winit::dpi::PhysicalPosition<i32>>,
    /// The window's OUTER SIZE captured immediately BEFORE it entered OS
    /// fullscreen, so the fullscreen EXIT can centre it without asking the OS how
    /// big it is.
    ///
    /// Why it exists: on X11 `Window::outer_size()` is a live `XGetGeometry`
    /// round-trip, and the un-fullscreen request we issue one statement earlier is
    /// an ASYNC `_NET_WM_STATE` ClientMessage — so a post-exit read still reports
    /// the MONITOR size, the centring subtraction saturates to 0 and the "centre"
    /// resolves to the monitor ORIGIN (the window lands flush in the top-left).
    /// Captured on EVERY enter path (`set_main_fullscreen`, the summon arm and
    /// `resumed`) by `capture_pre_fullscreen`. `None` only before the first enter,
    /// where the live size is not stale and is the right answer anyway.
    last_windowed_size: Option<winit::dpi::PhysicalSize<u32>>,
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
    /// When `Some((x, y, tab))`, the TAB context menu (Detach / Rename /
    /// Close Tab) is open at this physical-pixel anchor for that tab.
    /// Mutually exclusive with `context_menu` (the terminal Copy/Paste menu).
    tab_menu: Option<(f32, f32, TabId)>,
    /// Item labels of the open tab menu, snapshotted when it opened (the
    /// "Detach" row is present only when detaching was allowed at open time).
    tab_menu_labels: Vec<&'static str>,
    /// Cached hit-test rects for the open tab menu (built once on open).
    tab_menu_rects: Vec<jetty_render::Rect>,
    /// Tab-menu item currently under the cursor (hover highlight).
    tab_menu_hover: Option<usize>,


}



/// A left-button drag that began on tab `tab` in the main tab bar. `tearing`
/// flips true once the cursor moves > `TEAR_THRESHOLD_PX` vertically out of the
/// strip (and back false if it returns), so a plain click still selects.
#[derive(Debug, Clone, Copy)]
struct TabDrag {
    tab: TabId,
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
/// of physical size `w`×`h` at DPI scale `dpi`. Edges are within `EDGE` logical
/// px of a side; corners within `CORNER` logical px of a corner (scaled to
/// physical by `dpi`, so the grab band is the same physical-inch target on a 2×
/// display). Corners take priority over edges. Returns `ResizeZone::None` when
/// the cursor is in the interior.
pub(crate) fn resize_zone_at(cx: f32, cy: f32, w: u32, h: u32, dpi: f32) -> ResizeZone {
    let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 1.0 };
    let edge = 6.0 * dpi;
    let corner = 12.0 * dpi;
    let w = w as f32;
    let h = h as f32;
    // Out-of-bounds → no resize.
    if cx < 0.0 || cy < 0.0 || cx > w || cy > h {
        return ResizeZone::None;
    }
    let near_left = cx <= corner;
    let near_right = cx >= w - corner;
    let near_top = cy <= corner;
    let near_bottom = cy >= h - corner;
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
    if cx <= edge {
        return ResizeZone::West;
    }
    if cx >= w - edge {
        return ResizeZone::East;
    }
    if cy <= edge {
        return ResizeZone::North;
    }
    if cy >= h - edge {
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

/// Write bytes produced for a tab's program (mouse reports, wheel arrows) to
/// its PTY. Nothing to write is free.
fn write_pty_bytes(writer: &mut dyn Write, bytes: &[u8]) {
    if !bytes.is_empty() {
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }
}

/// Where a detached window's grid sits (below its title bar, above its status
/// strip), for the shared grid mouse handling.
fn detached_grid_geom(
    dw: &crate::detached::DetachedWindow,
    ui_font: f32,
    show_hud: bool,
) -> crate::gridmouse::GridGeom {
    let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
    let (cell_w, cell_h) = dw.text.cell_size();
    crate::gridmouse::GridGeom {
        top: bar_h,
        bottom: dw.gpu.config.height as f32 - status_h,
        cell_w,
        cell_h,
    }
}

/// [`App::with_main_grid`] for a detached window: run one shared grid-mouse
/// step on its tab and write what it produced to its PTY.
fn with_detached_grid<R>(
    dw: &mut crate::detached::DetachedWindow,
    geom: crate::gridmouse::GridGeom,
    mods: winit::keyboard::ModifiersState,
    f: impl FnOnce(&mut crate::gridmouse::Grid) -> R,
) -> R {
    let mut out = Vec::new();
    let r = f(&mut crate::gridmouse::Grid {
        term: &mut dw.tab.terminal,
        mouse: &mut dw.grid_mouse,
        selecting: &mut dw.selecting,
        geom,
        pointer: dw.cursor,
        mods,
        out: &mut out,
    });
    write_pty_bytes(&mut dw.tab.writer, &out);
    r
}

/// Arm the "Hold Shift while dragging to select text" pill in `window`: after a
/// drag that went to a mouse-grabbing program (throttled by the shared 25 s
/// cooldown), or at once (`explicit`) when the user right-clicks for the menu
/// with nothing selected. Returns whether it was armed (the caller repaints).
fn arm_shift_hint(
    until: &mut Option<(std::time::Instant, winit::window::WindowId)>,
    cooldown: &mut Option<std::time::Instant>,
    window: winit::window::WindowId,
    explicit: bool,
) -> bool {
    let now = std::time::Instant::now();
    if !explicit && cooldown.is_some_and(|t| now < t) {
        return false;
    }
    *until = Some((now + std::time::Duration::from_millis(3500), window));
    *cooldown = Some(now + std::time::Duration::from_secs(25));
    true
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
        // Problems (a skipped theme file) are shown in the first tab.
        let mut startup_warnings = crate::themes::rebuild_registry();
        // The persisted settings (per-key: one bad value never resets the rest).
        let loaded = crate::config::Config::load();
        // Saves are written by a background thread; it reports a refused save (the
        // file is not valid TOML) back through the event loop as a status pill.
        let notice_proxy = proxy.clone();
        let persister = crate::config::Persister::new(
            crate::config::Config::config_path(),
            loaded.cfg.clone(),
            loaded.hash,
            Box::new(move |msg| {
                let _ = notice_proxy.send_event(AppEvent::ConfigNotice(msg));
            }),
        );

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
            // Session-only; the first open / first summon establishes it from
            // `window_mode` (rule F0: never fullscreen while hidden).
            main_fullscreen: false,
            focus_autohide: true,
            scrollback_lines: 10_000,
            launch_at_login: false,
            summon_hotkey: "F9".to_string(),
            shell: String::new(),
            top_flush_pos: false,
            top_flush_dirty: true,
            cached_tabs_meta: Vec::new(),
            cached_tabs_sig: u64::MAX,
            applied_main_os_title: "JeTTY".to_string(),
            last_focused_window: None,
            main_focused: false,
            raise_attempt_at: None,
            focus_lost_at: None,
            autohidden_at: None,
            switching_to_settings: false,
            switching_to_detached: false,
            pending_autohide_at: None,
            dragging_dropdown: false,
            dragging_dropdown_width: false,
            wayland_warned: false,
            crt_clock: std::time::Instant::now(),
            summon_anim: None,
            caret_anim: None,
            key_paint_due: None,
            acquire_retry: None,
            summon_pending: false,
            summon_settle_until: None,
            settings_paint_until: None,
            settings_acquire_retry: None,
            gpu_rebuild_retry_at: None,
            last_present_at: None,
            frame_interval: refresh_interval(None),
            paced_paint_at: None,
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
            reflow_deferred_by_hide: false,
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
            macos_option_as_alt: input::OptionAsAlt::default(),
            copy_on_select: clipboard::CopyOnSelect::default(),
            // Placeholder default keymap; rebuilt from cfg.keys below in `new`.
            keymap: crate::keymap::KeyMap::defaults(),
            keys: crate::config::KeyBindings::default(),
            help_rows: Vec::new(),
            config_watcher: None,
            reloading: false,
            persister: std::cell::RefCell::new(persister),
            // Set from the config below.
            theme_name: String::new(),
            startup_warnings: Vec::new(),
            reset_keys_armed_until: None,
            start_hidden: false,
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
            key_modifiers: winit::event::Modifiers::default(),
            ime_preedit: None,
            ime_area: None,
            cursor: (0.0, 0.0),
            grid_mouse: crate::gridmouse::GridMouse::default(),
            right_click_hint_shown: false,
            scroll_accum: input::ScrollAccumulator::new(),
            shift_hint_until: None,
            shift_hint_cooldown: None,
            status_pill: None,
            runsel_active: false,
            run_selection_enabled: true,
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
            menu_disabled: Vec::new(),
            menu_hover: None,
            next_tab_id: 1,
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
            perf: crate::perf::Perf::from_env(),
            frame_log: std::env::var_os("JETTY_FRAME_LOG").is_some(),
            frames_presented: 0,
            ov: Overlays::default(),
            confirm_close: None,
            confirm_quit: false,
            last_pos: None,
            last_windowed_size: None,
            detached: Vec::new(),
            tab_drag: None,
            tab_menu: None,
            tab_menu_labels: Vec::new(),
            tab_menu_rects: Vec::new(),
            tab_menu_hover: None,
        };
        // Persisted user settings override the env-derived defaults (but env
        // vars still seed the initial values above, so an explicit JETTY_* can
        // win on a fresh config). Apply config BEFORE the first render so the
        // window comes up already themed/sized as the user left it. The font
        // size/family are consumed later by `resumed` when it builds the
        // TextLayer; theme+opacity are pushed into the terminals by apply_theme.
        let crate::config::Loaded { cfg, warnings, .. } = loaded;
        startup_warnings.extend(warnings);
        // The CHOSEN theme is remembered by name even when it can't be shown (a
        // missing/broken user theme file): the fallback on screen is never saved
        // over it, and a reload that fixes the file brings it back.
        app.theme_name = cfg.theme.clone();
        match jetty_core::theme_index(&cfg.theme) {
            Some(i) => app.theme_idx = i,
            None => startup_warnings.push(theme_missing_warning(
                &cfg.theme,
                &jetty_core::theme_at(app.theme_idx).display_name,
            )),
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
        // The config key is the source of truth: (re)write or remove the login
        // autostart entry to match — this also refreshes a stale Exec path (an
        // AppImage moved, a reinstall) on every start.
        app.launch_at_login = cfg.launch_at_login;
        if let Err(e) = set_launch_at_login(app.launch_at_login) {
            startup_warnings.push(e);
        }
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
        app.run_selection_enabled = cfg.run_selection;
        app.hot_reload = cfg.hot_reload;
        app.macos_option_as_alt = cfg.macos_option_as_alt;
        app.copy_on_select = cfg.copy_on_select;
        // Compile the keybindings (defaults + user `[keys]` overrides). Any invalid
        // chord / conflict / rejected bind is logged; the rest still apply.
        app.keys = cfg.keys;
        app.keymap = crate::keymap::KeyMap::compile(&app.keys);
        startup_warnings.extend(app.keymap.warnings().iter().map(|w| format!("[keys] {w}")));
        app.help_rows = App::compute_help_rows(&app.keymap, &app.summon_hotkey);
        for w in &startup_warnings {
            eprintln!("jetty: {w}");
        }
        app.startup_warnings = startup_warnings;

        // Apply the initial theme+opacity so Terminal::new env defaults are
        // overridden by our managed state (avoids double-reads from env). Also
        // populates the `active_theme` cache from the config-resolved theme_idx.
        app.apply_theme();
        app
    }

    /// Start hidden (`jetty --background`): the window is created unmapped and the
    /// first summon shows it. Call before the event loop runs.
    pub fn set_start_hidden(&mut self, hidden: bool) {
        self.start_hidden = hidden;
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
        // Sectioned (`## ` header, "" spacer, "KEY — desc" item) so the overlay
        // renders section headers + aligned key/description columns. Mirrors the
        // static `jetty_render::HELP_ROWS`, but with LIVE keymap chords.
        vec![
            "## Tabs & windows".to_string(),
            format!("{} — New tab", all(A::NewTab)),
            format!("{} — Close tab", all(A::CloseTab)),
            format!("{} / {} — Next / previous tab", first(A::NextTab), first(A::PrevTab)),
            "Ctrl+1…9 — Jump to tab".to_string(),
            format!(
                "{} — Detach / reattach tab   (drag off bar; right-click for menu)",
                all(A::DetachTab)
            ),
            "Double-click tab / top bar — Rename / maximize".to_string(),
            // BOTH chords when macOS seeds its companion (bare F11 is dead there).
            // NOTE: the two rows above/below become partly false while fullscreen —
            // the maximize toggle exits fullscreen instead, and move/resize are
            // inert. Accepted: the overlay describes the normal windowed state, and
            // a mode-dependent help overlay would be a worse trade (amendment I-E).
            format!("{} — Fullscreen (whole monitor)", all(A::ToggleFullscreen)),
            "Drag top bar / edges — Move / resize window".to_string(),
            String::new(),
            "## Appearance".to_string(),
            format!(
                "{} / {} / {} — Font size",
                first(A::FontUp),
                first(A::FontDown),
                first(A::FontReset)
            ),
            format!("{} / {} — Transparency", first(A::OpacityUp), first(A::OpacityDown)),
            format!("{} — Settings", all(A::ToggleSettings)),
            format!("{} — Command palette", first(A::OpenPalette)),
            String::new(),
            "## Clipboard & selection".to_string(),
            format!("{} / {} — Copy / paste", first(A::Copy), first(A::Paste)),
            format!(
                "{} — Run selection in a new tab   (multi-line lands staged)",
                all(A::RunSelection)
            ),
            "Left-drag — Select text (auto-copies)".to_string(),
            "Shift+drag — Select over mouse apps (vim / htop / Claude Code)".to_string(),
            "Right-click — Context menu".to_string(),
            String::new(),
            "## Search & scroll".to_string(),
            format!(
                "{} — Search scrollback   (Enter next, Shift+Enter prev, Esc close)",
                first(A::SearchToggle)
            ),
            format!(
                "{} / {} — Previous / next prompt",
                first(A::PrevPrompt),
                first(A::NextPrompt)
            ),
            format!("{} / {} — Scroll", first(A::ScrollPageUp), first(A::ScrollPageDown)),
            "Ctrl+L — Clear".to_string(),
            String::new(),
            "## Keyboard modes & links".to_string(),
            format!(
                "{} — Hint mode: copy a URL / path   (Alt = open, Esc cancel)",
                first(A::HintMode)
            ),
            format!(
                "{} — Copy-mode: keyboard select   (hjkl, v/V, y = yank, r = run)",
                first(A::CopyMode)
            ),
            "Ctrl+click — Open URL   (Ctrl+hover underlines)".to_string(),
            String::new(),
            "## Other".to_string(),
            format!("{summon_hotkey} (configurable) — Summon / hide window"),
            "Ctrl+D — Close shell (EOF)".to_string(),
            "Esc — Close this help".to_string(),
        ]
    }

    /// Record the current user-tweakable settings for saving. Called whenever a
    /// setting changes. Cheap and non-blocking: only the keys that differ from the
    /// file's last-synced state are queued, and they are written ~400 ms after the
    /// last change, in place and off the UI thread (see `config::Persister`).
    fn persist(&self) {
        // Never write config.toml while applying a reload: that would re-trigger the
        // watcher (a burst of atomic writes) and risk a loop. Combined with the
        // hash guard, this makes reload loop-free BY CONSTRUCTION (amendment H2).
        if self.reloading {
            return;
        }
        let cfg = self.settings_snapshot();
        self.persister.borrow_mut().record(&cfg, std::time::Instant::now());
    }

    /// The current settings as a `Config` (what `persist` saves).
    fn settings_snapshot(&self) -> crate::config::Config {
        crate::config::Config {
            // The theme the user CHOSE — not the fallback shown while it is missing
            // (a broken user theme file must never be replaced by the fallback).
            theme: self.theme_name.clone(),
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
            run_selection: self.run_selection_enabled,
            hot_reload: self.hot_reload,
            macos_option_as_alt: self.macos_option_as_alt,
            copy_on_select: self.copy_on_select,
            // Preserve the user's `[keys]` overrides verbatim (never editable via the
            // Settings UI — a settings-driven persist must not erase them).
            keys: self.keys.clone(),
        }
    }

    /// Select theme `i` as the user's choice (palette / Settings pick): show it and
    /// make it the remembered `theme_name`.
    fn pick_theme(&mut self, i: usize) {
        self.theme_idx = i;
        self.theme_name = jetty_core::theme_at(i).name.to_string();
        self.apply_theme();
    }

    /// Point `theme_idx` at the chosen `theme_name`. When it is missing (a user
    /// theme file deleted or broken mid-edit) keep showing the current theme if it
    /// still exists, else the first built-in — and say so. Never touches
    /// `theme_name`, so the choice survives until the file is fixed.
    fn resolve_chosen_theme(&mut self, warnings: &mut Vec<String>) {
        match jetty_core::theme_index(&self.theme_name) {
            Some(i) => self.theme_idx = i,
            None => {
                let shown = self.active_theme.name.to_string();
                self.theme_idx = jetty_core::theme_index(&shown)
                    .unwrap_or(0)
                    .min(jetty_core::theme_count().saturating_sub(1));
                warnings.push(theme_missing_warning(
                    &self.theme_name,
                    &jetty_core::theme_at(self.theme_idx).display_name,
                ));
            }
        }
    }

    /// Show configuration problems the user must see: a status pill on the main
    /// window (long enough to read; several are summarized as "<first> (+N more)")
    /// plus every one on stderr.
    fn show_config_warnings(&mut self, warnings: &[String]) {
        let Some(first) = warnings.first() else { return };
        for w in warnings {
            eprintln!("jetty: {w}");
        }
        let mut msg = format!("Config: {}", sanitize_notice(first));
        if msg.chars().count() > 96 {
            msg = msg.chars().take(93).collect::<String>() + "…";
        }
        if warnings.len() > 1 {
            msg.push_str(&format!(" (+{} more)", warnings.len() - 1));
        }
        self.show_notice_pill(msg, 8000);
    }

    /// Show `msg` in the main window's status pill for `ms` milliseconds.
    fn show_notice_pill(&mut self, msg: String, ms: u64) {
        let Some(id) = self.window.as_ref().map(|w| w.id()) else { return };
        self.status_pill =
            Some((msg, std::time::Instant::now() + std::time::Duration::from_millis(ms), id));
        self.request_main_paint();
    }

    /// Register the global summon hotkey (`summon_hotkey`) and forward its presses
    /// to the event loop as `ToggleVisibility`. An invalid hotkey string falls back
    /// to F9 and a registration failure is reported in-app — except where it is the
    /// expected state (a Wayland session, which binds `jetty --toggle` instead).
    fn start_summon_hotkey(&mut self) {
        use std::str::FromStr;
        let proxy = self.proxy.clone();
        let spec = self.summon_hotkey.clone();
        // global_hotkey's own parser ("F9", "F12", "Ctrl+Shift+F12").
        let hotkey = match global_hotkey::hotkey::HotKey::from_str(&spec) {
            Ok(h) => h,
            Err(e) => {
                let _ = proxy.send_event(AppEvent::ConfigNotice(format!(
                    "summon_hotkey {spec:?} is invalid ({e}) — using F9"
                )));
                global_hotkey::hotkey::HotKey::new(None, global_hotkey::hotkey::Code::F9)
            }
        };
        // macOS: upstream requires the manager to be created — and kept — on the
        // MAIN thread (here, in `resumed`); registering there is a cheap Carbon
        // call. A background-thread manager (the old code) was documented as
        // fragile and could silently never deliver F9.
        #[cfg(target_os = "macos")]
        {
            let registered = global_hotkey::GlobalHotKeyManager::new()
                .and_then(|m| m.register(hotkey).map(|()| m));
            match registered {
                Ok(manager) => {
                    self.hotkey_manager = Some(manager);
                    std::thread::spawn(move || forward_hotkey_presses(&proxy));
                }
                Err(e) => report_hotkey_failure(&proxy, &spec, &e.to_string()),
            }
        }
        // Linux/BSD: off the main thread — GlobalHotKeyManager::register() blocks on
        // a worker that opens a 2nd X11 connection + xkb round-trips ending in a
        // 50 ms sleep, which used to delay the first redraw. The press events go
        // through the async proxy either way, so moving it changes only WHERE it
        // blocks. The manager is kept alive inside the forwarding loop.
        #[cfg(not(target_os = "macos"))]
        {
            self.hotkey_manager = Some(());
            std::thread::spawn(move || {
                let manager = match global_hotkey::GlobalHotKeyManager::new() {
                    Ok(m) => m,
                    Err(e) => return report_hotkey_failure(&proxy, &spec, &e.to_string()),
                };
                if let Err(e) = manager.register(hotkey) {
                    return report_hotkey_failure(&proxy, &spec, &e.to_string());
                }
                forward_hotkey_presses(&proxy);
                drop(manager);
            });
        }
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

    /// The in-progress rename as the tab bar's `(tab index, buffer)`, if the
    /// renamed tab is (still) in the main window.
    fn rename_ref(&self) -> Option<(usize, &str)> {
        self.renaming.and_then(|id| self.tab_index(id)).map(|i| (i, self.rename_buf.as_str()))
    }

    /// Hand out the next stable [`TabId`].
    fn alloc_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;
        id
    }

    /// The main-window index of tab `id`, if it is (still) there.
    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The `self.detached` index of the window holding tab `id`, if any.
    fn detached_index(&self, id: TabId) -> Option<usize> {
        self.detached.iter().position(|d| d.tab.id == id)
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
        }
        // Theme AND opacity (also applied through here) are shared visuals:
        // repaint every surface — main, detached and the Settings panel, whose
        // swatches/readouts show them — not just the window that changed them.
        self.mark_dirty_all();
    }

    /// Apply a debounced config + themes hot-reload. Runs on the UI thread from
    /// `about_to_wait`. Non-destructive and loop-free by construction:
    ///
    /// * A save still pending or being written goes first: the reload is postponed
    ///   until it lands, or reading the file would revert that change in memory.
    /// * THEMES are ALWAYS rebuilt + reapplied (amendment T3): editing the active
    ///   theme file leaves config.toml untouched, so the config hash-skip must not
    ///   gate the repaint. The CHOSEN theme (`theme_name`) is re-resolved against
    ///   the rebuilt registry (amendment T2) — so a theme file fixed after a broken
    ///   save comes back — then `apply_theme` repaints all tabs + detached.
    /// * CONFIG is parsed key by key: an invalid value keeps the live setting, a
    ///   syntax error keeps everything — never `.bad`, never defaults — and each
    ///   problem is shown in a status pill. A file whose content hashes to our own
    ///   last write is skipped (self-write echo). `self.reloading` disables
    ///   `persist()` for the whole apply, so no live key can write config.toml back
    ///   (amendment H2 — loop-free by construction).
    fn reload_config_and_themes(&mut self) {
        if self.persister.borrow().busy() {
            self.persister.borrow_mut().flush();
            self.pending_reload_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(60));
            return;
        }
        self.reloading = true;
        let mut warnings = Vec::new();

        // (A) Themes — always. Rebuild the registry from disk.
        warnings.extend(crate::themes::rebuild_registry());

        // (B) Config — per-key, hash-guarded.
        if let Ok(s) = std::fs::read_to_string(crate::config::Config::config_path()) {
            let h = crate::config::hash_str(&s);
            // Skip our own write echoing back through the watcher.
            if !self.persister.borrow().is_self_write(h) {
                let live = self.settings_snapshot();
                match crate::config::Config::parse_with_base(&s, &live, "keeping the current value") {
                    Ok((cfg, problems)) => {
                        warnings.extend(problems);
                        self.apply_reloaded_config(cfg.clone(), &mut warnings);
                        // Records the observed hash too, so an identical later
                        // hand-save no-ops.
                        self.persister.borrow_mut().note_reloaded(cfg, h);
                    }
                    Err(syntax) => {
                        warnings.push(format!(
                            "config.toml is not valid TOML ({syntax}) — keeping the current settings"
                        ));
                        self.persister.borrow_mut().note_seen(h);
                    }
                }
            }
        }

        // (C) The chosen theme against the rebuilt registry (indices may have
        // shifted), then repaint every surface with the (possibly changed) palette.
        self.resolve_chosen_theme(&mut warnings);
        self.apply_theme();

        // (D) Keep watching a recreated config dir, a retargeted config symlink or a
        // newly created themes/ dir.
        if let Some(w) = self.config_watcher.as_mut() {
            w.rearm();
        }

        self.reloading = false;
        self.show_config_warnings(&warnings);
        // Repaint chrome (theme/settings) once the reload settled.
        self.request_main_paint();
        self.request_settings_paint();
    }

    /// Apply an externally-edited `Config` LIVE, diffing against current in-memory
    /// state and touching only changed keys. Runs with `self.reloading == true`, so
    /// every setter it calls is non-persisting (they early-return in `persist`).
    /// Problems found while applying (rejected keybindings, an unwritable autostart
    /// entry) are appended to `warnings`.
    ///
    /// Keys mid-DRAG in the Settings panel are skipped (amendment H4): the in-flight
    /// interactive value wins over a concurrent external edit. `summon_hotkey` is
    /// RESTART-only (the global grab is registered once) and deliberately NOT
    /// applied here (only mirrored).
    fn apply_reloaded_config(&mut self, cfg: crate::config::Config, warnings: &mut Vec<String>) {
        let eps = f32::EPSILON;

        // Theme: remember the chosen name; the caller resolves it against the
        // rebuilt registry right after (a missing theme falls back visibly).
        if cfg.theme != self.theme_name {
            self.theme_name = cfg.theme.clone();
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
                // Detached windows round their corners with it too.
                self.mark_dirty_all();
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
        // Perf HUD: changes the reserved status-bar height → grid rows in every
        // window, so reflow them all.
        self.set_perf_hud(cfg.show_perf_hud);
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
        // Run-selection opt-out — live (checked once per trigger, so a bare
        // assign is the whole apply).
        self.run_selection_enabled = cfg.run_selection;
        // Hot-reload toggle: turning it OFF live drops the watcher (stops watching).
        // Turning it ON when it was off is restart-only (no watcher exists to detect
        // the change) — documented.
        self.hot_reload = cfg.hot_reload;
        if !self.hot_reload {
            self.config_watcher = None;
        }
        self.macos_option_as_alt = cfg.macos_option_as_alt;
        self.apply_option_as_alt_everywhere();
        self.copy_on_select = cfg.copy_on_select;
        // Launch at login — live: the config key is the source of truth, so the
        // autostart entry is written/removed to match.
        if cfg.launch_at_login != self.launch_at_login {
            self.launch_at_login = cfg.launch_at_login;
            if let Err(e) = set_launch_at_login(self.launch_at_login) {
                warnings.push(e);
            }
        }
        // Mirror the RESTART-ONLY-EFFECT key too, so a later panel-driven persist()
        // round-trips the user's external edit instead of clobbering it with the
        // stale startup value. Its live EFFECT stays restart-only (the summon grab
        // is registered once at startup) — but the on-disk value must survive an
        // external edit + a subsequent unrelated Settings change.
        self.summon_hotkey = cfg.summon_hotkey.clone();
        self.cfg_show_welcome = cfg.show_welcome;
        // shell: mirror so new tabs spawned after the reload use the edited shell.
        self.shell = cfg.shell.clone();
        // Keybindings — LIVE (not restart-only). Recompile only when the `[keys]`
        // table actually changed (compare the compiled maps, so an unrelated reload
        // skips the rebuild). No redraw needed; the next keypress uses the new map.
        if cfg.keys != self.keys {
            let new_km = crate::keymap::KeyMap::compile(&cfg.keys);
            warnings.extend(new_km.warnings().iter().map(|w| format!("[keys] {w}")));
            self.keys = cfg.keys.clone();
            self.keymap = new_km;
            self.help_rows = App::compute_help_rows(&self.keymap, &self.summon_hotkey);
        }
    }

    /// Re-dock the main window to the top strip when it is a visible Dropdown — used
    /// after a live dropdown width/height change so it re-docks immediately.
    ///
    /// Inert while the window is fullscreen (Dropdown + an ad-hoc F11): docking a
    /// fullscreen window would yank it into a squared-off top strip that cannot
    /// then be moved, resized or un-maximised, with Settings still reading
    /// "Dropdown" — hence the shared `dock_reassert_ok` predicate.
    fn redock_if_dropdown(&mut self) {
        if self.visible && dock_reassert_ok(self.window_mode, self.main_fullscreen) {
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
        let rows = ((gpu.config.height as f32 - self.bar_h() - status_h) / ch).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Chrome metrics of the MAIN window: its DPI × the UI font size. Every
    /// main-window chrome builder AND hit-test derives its geometry from this, so
    /// the bar / strip / menus / pills grow with the text they hold and clicks
    /// land where the chrome is drawn.
    fn chrome_metrics(&self) -> jetty_render::ChromeMetrics {
        let dpi = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
        jetty_render::ChromeMetrics::new(dpi, self.ui_font_logical)
    }

    /// Chrome metrics of the SETTINGS window: its own DPI × the CAPPED panel
    /// text size (the panel body font is clamped to `[PANEL_TEXT_MIN,
    /// PANEL_TEXT_MAX]`, so the panel stays bounded for huge UI fonts). This is
    /// the panel's layout scale (`build_panel`'s `cm`).
    fn settings_metrics(&self) -> jetty_render::ChromeMetrics {
        let dpi = self
            .settings_window
            .as_ref()
            .or(self.window.as_ref())
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        let capped = self.ui_font_logical.clamp(PANEL_TEXT_MIN, PANEL_TEXT_MAX);
        jetty_render::ChromeMetrics::new(dpi, capped)
    }

    /// Height of the main window's tab bar (physical px).
    fn bar_h(&self) -> f32 {
        self.chrome_metrics().bar_h()
    }

    /// Lay out window `s`'s help overlay at first row `scroll` — the SAME call
    /// its draw pass makes, so hit-tests and scroll ranges match what's drawn.
    fn layout_help(&mut self, s: Surface, scroll: usize) -> Option<jetty_render::HelpOverlay> {
        let (w, h, cm, _) = self.surface_layout(s)?;
        let theme = self.current_theme();
        let mut fallback = mono_fallback(cm);
        Some(match s {
            Surface::Main => jetty_render::build_help_overlay(
                w, h, &theme, measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                &self.help_rows, scroll,
            ),
            Surface::Detached(p) => {
                let d = self.detached.get_mut(p)?;
                jetty_render::build_help_overlay(w, h, &theme, &mut d.chrome_text, cm, &self.help_rows, scroll)
            }
        })
    }

    /// Lay out window `s`'s open search bar (same call as its draw pass).
    fn layout_search_bar(&mut self, s: Surface) -> Option<jetty_render::SearchBar> {
        let (w, _, cm, grid_top) = self.surface_layout(s)?;
        let theme = self.current_theme();
        let (q, cur, total) = {
            let t = self.term_of(s)?;
            let (cur, total) = t.search_counter();
            (t.search_query().to_string(), cur, total)
        };
        let mut fallback = mono_fallback(cm);
        let m = measure_or(self.surface_chrome_text(s), &mut fallback);
        Some(jetty_render::build_search_bar(w, grid_top, &theme, m, cm, &q, cur, total))
    }

    /// Lay out window `s`'s open command palette (same call as its draw pass).
    fn layout_palette(&mut self, s: Surface) -> Option<jetty_render::CommandPalette> {
        let (w, h, cm, _) = self.surface_layout(s)?;
        let theme = self.current_theme();
        let (q, vis, total, first) = self.ov_of(s)?.palette_draw()?;
        let prows: Vec<jetty_render::PaletteRow> = vis
            .iter()
            .map(|(t, idx, sel)| jetty_render::PaletteRow { title: t, match_indices: idx, selected: *sel })
            .collect();
        let mut fallback = mono_fallback(cm);
        let m = measure_or(self.surface_chrome_text(s), &mut fallback);
        Some(jetty_render::build_command_palette(w, h, &theme, m, cm, &q, &prows, total, first))
    }

    /// Window `s`'s help-overlay scroll range right now — `(max_scroll,
    /// page_rows)` — or `None` when every row fits (nothing to scroll; the wheel
    /// and keys then behave as if the help were not open).
    fn help_scroll_range(&mut self, s: Surface) -> Option<(usize, usize)> {
        let help = self.layout_help(s, 0)?;
        (help.max_scroll > 0).then_some((help.max_scroll, help.page_rows))
    }

    /// Move window `s`'s help overlay to first row `to`, clamped to `[0, max]`.
    fn set_help_scroll(&mut self, s: Surface, to: isize, max: usize) {
        let to = to.clamp(0, max as isize) as usize;
        let Some(ov) = self.ov_of_mut(s) else { return };
        if to != ov.help_scroll {
            ov.help_scroll = to;
            self.paint_surface(s);
        }
    }

    /// Toggle window `s`'s keyboard-shortcuts help (the "?" button). Opening it
    /// closes that window's context menu so the two are mutually exclusive.
    fn toggle_help(&mut self, s: Surface) {
        let Some(ov) = self.ov_of_mut(s) else { return };
        ov.help_open = !ov.help_open;
        if ov.help_open {
            ov.help_scroll = 0;
            self.dismiss_help_peers(s);
        }
        self.paint_surface(s);
    }

    /// Close the context menus that the help overlay replaces in window `s`
    /// (main: the terminal context menu; detached: its menu).
    fn dismiss_help_peers(&mut self, s: Surface) {
        match s {
            Surface::Main => {
                self.context_menu = None;
                self.menu_hover = None;
            }
            Surface::Detached(_) => self.dismiss_surface_menus(s),
        }
    }

    /// Whole wheel lines for window `s`, through ITS fractional accumulator at
    /// its cell height (slow touchpad scrolling arrives as sub-line deltas).
    fn surface_wheel_lines(&mut self, s: Surface, delta: MouseScrollDelta) -> i32 {
        match s {
            Surface::Main => {
                let cell_h = self.text.as_ref().map_or(0.0, |t| t.cell_size().1);
                self.scroll_accum.add(input::wheel_lines(delta, cell_h))
            }
            Surface::Detached(p) => match self.detached.get_mut(p) {
                Some(d) => {
                    let cell_h = d.text.cell_size().1;
                    d.scroll_accum.add(input::wheel_lines(delta, cell_h))
                }
                None => 0,
            },
        }
    }

    /// The overlays of window `s` that own the mouse wheel. Hint mode and
    /// copy-mode swallow it (scrolling would slide the labelled tokens out from
    /// under their chips, or desync the copy-mode cursor/selection from the
    /// content — k/j/Ctrl+u/d move within the mode); the help scrolls its rows
    /// while they overflow the window; the palette scrolls its list. Returns
    /// whether the wheel was consumed (never reaching the terminal).
    fn overlay_wheel(&mut self, s: Surface, delta: MouseScrollDelta) -> bool {
        let Some(ov) = self.ov_of(s) else { return false };
        if ov.hint_mode.is_some() || ov.copy_mode.is_some() {
            return true;
        }
        if ov.help_open {
            if let Some((max, _)) = self.help_scroll_range(s) {
                let lines = self.surface_wheel_lines(s, delta);
                let cur = self.ov_of(s).map_or(0, |o| o.help_scroll);
                self.set_help_scroll(s, cur.min(max) as isize - lines as isize, max);
                return true;
            }
        }
        if self.ov_of(s).is_some_and(|o| o.palette_open) {
            let step = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y.round() as isize),
                MouseScrollDelta::PixelDelta(p) => {
                    if p.y > 0.0 { -1 } else if p.y < 0.0 { 1 } else { 0 }
                }
            };
            if step != 0 {
                self.palette_move(s, step);
                self.paint_surface(s);
            }
            return true;
        }
        false
    }

    /// Pixel Y origin of the terminal grid. The bar always costs `bar_h` of grid
    /// HEIGHT regardless of side, but the grid's pixel ORIGIN is 0 when the bar is
    /// at the bottom (grid fills from the top) and `bar_h` when it's at the top
    /// (grid starts below the bar).
    fn grid_top_offset(&self) -> f32 {
        if self.tab_bar_bottom { 0.0 } else { self.bar_h() }
    }

    /// Pixel height reserved at the BOTTOM of the window for the status bar (the
    /// perf HUD): the metrics' strip height when the HUD is enabled, else 0 (no
    /// bar, grid uses the full height). The grid and the bottom-mode tab bar both
    /// sit above it.
    fn status_h(&self) -> f32 {
        if self.show_perf_hud { self.chrome_metrics().status_h() } else { 0.0 }
    }

    /// The bottom status strip's height in a DETACHED window (its own DPI).
    fn detached_status_h(&self, dw: &crate::detached::DetachedWindow) -> f32 {
        if self.show_perf_hud {
            dw.chrome_metrics(self.ui_font_logical).status_h()
        } else {
            0.0
        }
    }

    /// Pixel Y of the tab bar's top edge for a surface of physical `height`.
    /// 0 when the bar is at the top; `height - bar_h - status_h` at the bottom
    /// (the status bar always sits below the bottom-mode tab bar).
    fn tabbar_y(&self, height: f32) -> f32 {
        if self.tab_bar_bottom {
            (height - self.bar_h() - self.status_h()).max(0.0)
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
        let _ = self.new_tab_with_cwd(cwd);
    }

    /// Spawn a new tab sized to the current grid, themed like the others, make it
    /// active, and redraw. The new PTY shares the same wake proxy so one
    /// `AppEvent::Wake` drains every tab. `cwd` is the directory the new shell
    /// starts in (`None` = today's spawn-dir/home behavior).
    ///
    /// Returns the new tab's index, or `None` when the PTY spawn failed (no tab
    /// was created and `self.active` is unchanged). Run-selection MUST arm its
    /// pending inject only through the returned index — arming "the active tab"
    /// on a spawn failure would stage the command into the SOURCE shell.
    fn new_tab_with_cwd(&mut self, cwd: Option<std::path::PathBuf>) -> Option<usize> {
        let idx = self.spawn_tab(cwd)?;
        self.set_active_tab(idx);
        Some(idx)
    }

    /// Spawn a new main-window tab WITHOUT making it active (a background tab):
    /// the shared core of `new_tab_with_cwd`, and what a detached window's
    /// run-selection uses so the main window's active tab — and its search bar,
    /// hint/copy mode and selection — are left untouched. Returns the new index.
    fn spawn_tab(&mut self, cwd: Option<std::path::PathBuf>) -> Option<usize> {
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
                // Not silent: a GUI launch never shows stderr, and a new tab that
                // simply doesn't appear reads as a dead shortcut.
                eprintln!("jetty: failed to spawn tab PTY: {e}");
                self.show_status_pill(crate::runsel::Notice {
                    msg: "Couldn't open a new tab — no shell could be started",
                    window: None,
                });
                return None;
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
        // Kitty keyboard protocol: answer `CSI ? u` and track the app's
        // `CSI > u` flag stack — the key path encodes per those flags
        // (`decide_window_key`), so a program that opts in gets kitty keys.
        terminal.set_kitty_keyboard(true);
        // Apply the configured scrollback cap (guard skips the no-op
        // set_options round-trip on the 10k default path).
        if self.scrollback_lines != 10_000 {
            terminal.set_scrollback_lines(self.scrollback_lines);
        }
        // Surface the shell / start-directory fallback notices here too (F2) —
        // through feed_notice: they carry outside data (paths) and must stay inert.
        for notice in pty.startup_notices() {
            terminal.feed_notice(notice);
        }
        let title = format!("Tab {}", self.tabs.len() + 1);
        let id = self.alloc_tab_id();
        self.tabs.push(Tab {
            id,
            terminal,
            pty,
            writer,
            default_title: title.clone(),
            title,
            manually_renamed: false,
            activity: jetty_render::TabActivity::None,
            pending_inject: None,
            input: input::TabInputState::default(),
        });
        // The tab bar gained an entry either way (active or background).
        self.request_main_paint();
        Some(self.tabs.len() - 1)
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
        let active_removed = i == self.active;
        if active_removed {
            // The searched (active) tab is going away; the bar must not stay
            // open silently retargeting whichever tab becomes active (F2/F7).
            self.search_close(Surface::Main);
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
        // A rename box / close confirmation for the removed tab goes with it
        // (they hold stable ids, so every other tab's stay put); any in-progress
        // selection is reset.
        self.drop_stale_tab_refs();
        self.selecting = false;
        // The tab menu / a held tab drag are anchored on the old layout; it
        // just changed under them, so drop both (transient, cheap to reopen).
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
        self.tab_drag = None;
        if active_removed {
            // A different tab is now active: drop the closed tab's hint/copy
            // mode, wheel remainder and hover (the same reset every switch does).
            self.entered_new_active_tab();
        } else {
            // A new tab may be under the pointer: revalidate the cached Ctrl+hover
            // underline against ITS grid (Ctrl+Shift+W keeps Ctrl held) (F12).
            self.update_link_hover(true);
            self.request_main_paint();
        }
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
        // Same reason as `toggle_settings_window`: a new window created over a
        // fullscreen terminal is focused but invisible behind it (a fullscreen
        // window lives in the WM's above-normal layer, and demotion on focus loss is
        // WM-specific — not something we may special-case). Leave fullscreen first.
        // This covers the tear-out gesture too, which routes through here.
        let was_fullscreen = self.main_fullscreen;
        if was_fullscreen {
            self.set_main_fullscreen(false);
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
        self.drop_stale_tab_refs();
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

        // Search state travels inside the Terminal, and the main window's bar
        // targets its ACTIVE tab: if the searched tab is leaving, close the bar
        // and drop its matches so no invisible state rides along (the new
        // window starts with its own closed bar). The bar stays open (showing
        // the next tab's usually-empty query) only when a NON-active tab is
        // detached via its context menu.
        if self.ov.search_open && idx == prev_active {
            self.ov.search_open = false;
            tab.terminal.search_clear();
        }
        // Apply the current theme to the detached tab before it leaves.
        tab.terminal.set_theme(self.current_theme());
        // The tab becomes the visible tab of its own window; drop any pending
        // indicator so it can't resurface stale on a later reattach.
        tab.activity = jetty_render::TabActivity::None;

        // The new window takes the main window's LOGICAL size (`build_window`
        // takes logical px) — its WINDOWED size: when we just left fullscreen
        // above, the exit is asynchronous and the surface is still monitor-sized.
        let (w_logical, h_logical) = detach_logical_size(
            self.gpu.as_ref().map(|g| (g.config.width, g.config.height)),
            self.last_windowed_size.map(|s| (s.width, s.height)),
            was_fullscreen,
            self.window.as_ref().map_or(1.0, |w| w.scale_factor()),
        );

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
        let gpu_shared = self.gpu.as_ref().map(|g| g.shared());
        let mut dw = match crate::detached::DetachedWindow::new(
            event_loop,
            tab,
            w_logical,
            h_logical,
            self.font_logical,
            self.ui_font_logical,
            &self.font_family,
            &self.ui_font_family,
            gpu_shared.as_ref(),
            self.text.as_ref(),
        ) {
            Ok(dw) => {
                // macOS: same Option-as-Meta sides as the main window.
                apply_option_as_alt(&dw.window, self.macos_option_as_alt);
                dw
            }
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
            dw.chrome_metrics(self.ui_font_logical).bar_h(),
            self.detached_status_h(&dw),
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

        if idx == prev_active {
            // The active tab left: a different tab is active now — drop the
            // outgoing tab's hint/copy mode, wheel remainder and hover, exactly
            // like a tab switch (the search bar was handled above).
            self.entered_new_active_tab();
        } else {
            // A different tab may sit under the main-window pointer (Ctrl+Shift+D
            // keeps Ctrl held): revalidate the cached Ctrl+hover underline (F12).
            self.update_link_hover(true);
            // Redraw the main window so the tab bar reflects the removed tab.
            self.request_main_paint();
        }
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
        // This window's overlays go away with it: close its search (dropping
        // the tab's compiled regex + matches, which the main window's closed bar
        // would otherwise carry into every reflow) and its copy-mode selection.
        let s = Surface::Detached(pos);
        self.search_close(s);
        if self.ov_of(s).is_some_and(|o| o.copy_mode.is_some()) {
            self.cancel_copy_mode(s);
        }
        // Leave OS fullscreen while the window still exists: dropping a
        // fullscreen window leaks macOS's app-scoped presentation options (an
        // auto-hidden Dock + menu bar for the rest of the session).
        self.exit_detached_fullscreen_bare(pos);
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
        // The reattached tab becomes the active one. `set_active_tab` closes the
        // search bar and clears the OUTGOING tab's state first (F2/F7/F15) — the
        // push above shifted no index, so `self.active` still names it. (When the
        // main window had no tabs left — a close path adopting a detached shell —
        // that caller performs the outgoing-tab reset itself.)
        self.set_active_tab(crate::detached::reattach_index(self.tabs.len()));
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
        self.menu_disabled.clear();
        self.tab_menu = None;
        self.tab_menu_hover = None;
        self.tab_menu_rects.clear();
        self.tab_menu_labels.clear();
    }

    /// Drop the long-lived tab references (rename box, close confirmation)
    /// whose tab left the main window (closed, exited, detached). They hold
    /// stable ids, so a reference to any OTHER tab stays valid untouched — no
    /// index shuffling that could retarget it.
    fn drop_stale_tab_refs(&mut self) {
        let live: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        self.renaming = still_open(self.renaming, &live);
        self.confirm_close = still_open(self.confirm_close, &live);
        if self.renaming.is_none() {
            self.rename_buf.clear();
        }
        // Ids are never reused, so a gone tab's notification throttle is dead
        // weight — drop it.
        self.notify_last_at
            .retain(|k, _| !matches!(k, NotifyKey::MainTab(id) if !live.contains(id)));
    }

    // ── Per-window overlay plumbing ──────────────────────────────────────────
    //
    // Every overlay (search bar, help, command palette, hint mode, copy-mode)
    // lives in its window's `Overlays` and acts on that window's terminal: the
    // main window's ACTIVE tab, or a detached window's own tab. The helpers below
    // resolve a `Surface` to those, so the main and detached windows run the SAME
    // overlay code.

    /// The overlays of window `s` (`None` for a detached index that's gone).
    fn ov_of(&self, s: Surface) -> Option<&Overlays> {
        match s {
            Surface::Main => Some(&self.ov),
            Surface::Detached(p) => self.detached.get(p).map(|d| &d.ov),
        }
    }

    fn ov_of_mut(&mut self, s: Surface) -> Option<&mut Overlays> {
        match s {
            Surface::Main => Some(&mut self.ov),
            Surface::Detached(p) => self.detached.get_mut(p).map(|d| &mut d.ov),
        }
    }

    /// The terminal window `s`'s overlays act on.
    fn term_of(&self, s: Surface) -> Option<&Terminal> {
        match s {
            Surface::Main => self.tabs.get(self.active).map(|t| &t.terminal),
            Surface::Detached(p) => self.detached.get(p).map(|d| &d.tab.terminal),
        }
    }

    fn term_of_mut(&mut self, s: Surface) -> Option<&mut Terminal> {
        match s {
            Surface::Main => self.tabs.get_mut(self.active).map(|t| &mut t.terminal),
            Surface::Detached(p) => self.detached.get_mut(p).map(|d| &mut d.tab.terminal),
        }
    }

    /// Window `s`'s overlays and terminal together (disjoint borrows).
    fn ov_term_mut(&mut self, s: Surface) -> Option<(&mut Overlays, &mut Terminal)> {
        match s {
            Surface::Main => {
                let tab = self.tabs.get_mut(self.active)?;
                Some((&mut self.ov, &mut tab.terminal))
            }
            Surface::Detached(p) => self.detached.get_mut(p).map(|d| (&mut d.ov, &mut d.tab.terminal)),
        }
    }

    /// Repaint window `s`.
    fn paint_surface(&self, s: Surface) {
        match s {
            Surface::Main => self.request_main_paint(),
            Surface::Detached(p) => {
                if let Some(d) = self.detached.get(p) {
                    d.request_paint();
                }
            }
        }
    }

    /// Close window `s`'s context menu(s) (an overlay taking over the window).
    fn dismiss_surface_menus(&mut self, s: Surface) {
        match s {
            Surface::Main => self.dismiss_menus(),
            Surface::Detached(p) => {
                if let Some(d) = self.detached.get_mut(p) {
                    d.menu_open = None;
                    d.menu_hover = None;
                    d.menu_rects.clear();
                    d.menu_disabled.clear();
                }
            }
        }
    }

    /// The run-selection source for window `s`.
    fn sel_source(s: Surface) -> SelSource {
        match s {
            Surface::Main => SelSource::Main,
            Surface::Detached(p) => SelSource::Detached(p),
        }
    }

    /// Window `s`'s layout for overlay builders and hit-tests: `(width, height,
    /// chrome metrics, grid top)`. `None` before its GPU stack exists.
    fn surface_layout(&self, s: Surface) -> Option<(u32, u32, jetty_render::ChromeMetrics, f32)> {
        match s {
            Surface::Main => {
                let g = self.gpu.as_ref()?;
                Some((g.config.width, g.config.height, self.chrome_metrics(), self.grid_top_offset()))
            }
            Surface::Detached(p) => {
                let d = self.detached.get(p)?;
                let cm = d.chrome_metrics(self.ui_font_logical);
                let (bar_h, _) = d.chrome_bands(self.ui_font_logical, self.show_perf_hud);
                Some((d.gpu.config.width, d.gpu.config.height, cm, bar_h))
            }
        }
    }

    /// Window `s`'s chrome text layer (the measurer its overlays are laid out
    /// with), if built.
    fn surface_chrome_text(&mut self, s: Surface) -> Option<&mut TextLayer> {
        match s {
            Surface::Main => self.chrome_text.as_mut(),
            Surface::Detached(p) => self.detached.get_mut(p).map(|d| &mut d.chrome_text),
        }
    }

    /// Close window `s`'s scrollback-search bar and clear its terminal's search
    /// state (query, compiled regex, matches). The single close path for
    /// Esc / ✕ / the search chord — and, in the main window, for every
    /// active-tab change while the bar is open: the bar targets the active tab,
    /// so leaving a searched tab in the background would strand a compiled regex
    /// and match list on it, and `Terminal::resize` would re-scan that tab's
    /// ENTIRE scrollback on every reflow forever (F2/F7/F15).
    fn search_close(&mut self, s: Surface) {
        let Some(ov) = self.ov_of_mut(s) else { return };
        if !ov.search_open {
            return;
        }
        ov.search_open = false;
        ov.search_dirty = false;
        // Tolerate an empty tabs vec (reattach-from-close_exited_tabs path).
        if let Some(term) = self.term_of_mut(s) {
            term.search_clear();
        }
        self.paint_surface(s);
    }

    /// Open window `s`'s search bar (the search chord / palette). Every close
    /// path clears, so the bar normally opens empty; should query state somehow
    /// survive on this terminal, re-collect so stale points (the scrollback
    /// rotated while closed) never render. No-op re-collect without a query.
    fn search_open(&mut self, s: Surface) {
        let Some(ov) = self.ov_of_mut(s) else { return };
        if !ov.search_open {
            ov.search_open = true;
            if let Some(term) = self.term_of_mut(s) {
                term.search_refresh();
            }
        }
        self.paint_surface(s);
    }

    /// The search chord: close window `s`'s bar if open, else open it.
    fn search_toggle(&mut self, s: Surface) {
        if self.ov_of(s).is_some_and(|o| o.search_open) {
            self.search_close(s);
        } else {
            self.search_open(s);
        }
    }

    /// Window `s`'s search-bar draw data `(query, current, total)` and the
    /// viewport match hits to tint — `None` / empty while its bar is closed
    /// (one bool test on the hot path, zero allocation).
    fn search_draw(&self, s: Surface) -> (Option<(String, usize, usize)>, Vec<jetty_core::SearchHit>) {
        if !self.ov_of(s).is_some_and(|o| o.search_open) {
            return (None, Vec::new());
        }
        let Some(t) = self.term_of(s) else { return (None, Vec::new()) };
        let (cur, total) = t.search_counter();
        (Some((t.search_query().to_string(), cur, total)), t.search_viewport_hits())
    }

    /// Re-collect window `s`'s open search now when a throttled refresh is due
    /// (output rotated its scrollback). Returns whether it re-collected.
    fn refresh_search_if_due(&mut self, s: Surface, now: std::time::Instant) -> bool {
        let Some((ov, term)) = self.ov_term_mut(s) else { return false };
        if !ov.search_refresh_due(now) {
            return false;
        }
        term.search_refresh();
        ov.search_refreshed(now);
        true
    }

    /// Type `text` into window `s`'s open search query (printable chars only).
    fn search_extend_query(&mut self, s: Surface, text: &str) {
        if let Some(term) = self.term_of_mut(s) {
            let mut q = term.search_query().to_string();
            q.extend(text.chars().filter(|c| !c.is_control()));
            term.search_set_query(&q);
        }
    }

    // ── Hint mode (Ctrl+Shift+H) + keyboard copy-mode (Ctrl+Shift+Space) ──────

    /// True while another overlay owns window `s`'s keyboard, so the hint /
    /// copy-mode chords cannot start a mode (single-owner rule). In the main
    /// window the app-level modals (confirm/quit popups, inline rename, the
    /// welcome splash) count too: palette/search/rename/confirm capture the chord
    /// BEFORE `decide_key` runs; welcome + help only capture Esc, so they are
    /// checked explicitly here (amendment 6 — "cannot enter while another owns
    /// keys", INCLUDING welcome, for parity).
    fn overlay_owns_keys(&self, s: Surface) -> bool {
        let window_overlay = self.ov_of(s).is_some_and(|o| o.owns_keys());
        match s {
            Surface::Main => {
                self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.renaming.is_some()
                    || self.welcome_open
                    || window_overlay
            }
            Surface::Detached(_) => window_overlay,
        }
    }

    /// Enter hint mode in window `s`: scan the visible URL/path/hash/IPv4 tokens
    /// ONCE and show their labels. No-op on the alt screen, while another
    /// overlay owns keys, or when the scan finds ZERO tokens (n=0 auto-exit —
    /// never trap the user in an empty mode requiring Esc).
    fn enter_hint_mode(&mut self, s: Surface) {
        if self.overlay_owns_keys(s) || self.ov_of(s).is_none_or(|o| o.copy_mode.is_some()) {
            return;
        }
        let Some(term) = self.term_of(s) else { return };
        if term.alt_screen() {
            return;
        }
        let tokens = term.hint_tokens();
        if tokens.is_empty() {
            return;
        }
        let labels = jetty_core::hints::assign_labels(tokens.len());
        if let Some(ov) = self.ov_of_mut(s) {
            ov.hint_mode = Some(HintState { tokens, labels, typed: String::new() });
        }
        self.paint_surface(s);
    }

    /// Cancel window `s`'s hint mode (Esc / after firing).
    fn exit_hint_mode(&mut self, s: Surface) {
        if let Some(ov) = self.ov_of_mut(s) {
            ov.hint_mode = None;
        }
        self.paint_surface(s);
    }

    /// Handle one key while hint mode owns window `s`'s keyboard. Letters narrow
    /// the typed prefix (matched against the BASE ASCII letter, independent of
    /// Alt/compose — BLOCKING 5); an exact label match COPIES the token (default)
    /// or, for a URL with Alt held at completion, OPENS it. Esc cancels;
    /// Backspace pops; every other key is swallowed.
    fn hint_mode_key(
        &mut self,
        s: Surface,
        physical: winit::keyboard::PhysicalKey,
        logical: &winit::keyboard::Key,
    ) {
        use winit::keyboard::{Key, NamedKey};
        match logical {
            Key::Named(NamedKey::Escape) => {
                self.exit_hint_mode(s);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(hs) = self.ov_of_mut(s).and_then(|o| o.hint_mode.as_mut()) {
                    hs.typed.pop();
                }
                self.paint_surface(s);
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
            let Some(hs) = self.ov_of(s).and_then(|o| o.hint_mode.as_ref()) else { return };
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
                self.exit_hint_mode(s);
            }
            Outcome::Narrow(t) => {
                if let Some(hs) = self.ov_of_mut(s).and_then(|o| o.hint_mode.as_mut()) {
                    hs.typed = t;
                }
                self.paint_surface(s);
            }
            Outcome::Ignore => {}
        }
    }

    /// Enter copy-mode in window `s`: a keyboard vi-cursor over the viewport +
    /// scrollback. No-op on the alt screen or while another overlay owns keys.
    /// Clears any leftover mouse selection on enter so the old highlight never
    /// lingers.
    fn enter_copy_mode(&mut self, s: Surface) {
        if self.overlay_owns_keys(s) || self.ov_of(s).is_none_or(|o| o.hint_mode.is_some()) {
            return;
        }
        let Some(term) = self.term_of_mut(s) else { return };
        if term.alt_screen() {
            return;
        }
        let snap = term.snapshot();
        let (row, col) = if snap.cursor_visible {
            (
                snap.cursor_row.min(snap.rows.saturating_sub(1)),
                snap.cursor_col.min(snap.cols.saturating_sub(1)),
            )
        } else {
            (snap.rows.saturating_sub(1), 0)
        };
        term.selection_clear();
        if let Some(ov) = self.ov_of_mut(s) {
            ov.copy_mode = Some(crate::copymode::CopyMode::new(row, col));
        }
        self.paint_surface(s);
    }

    /// Exit window `s`'s copy-mode (Esc / after yank).
    fn exit_copy_mode(&mut self, s: Surface) {
        if let Some(ov) = self.ov_of_mut(s) {
            ov.copy_mode = None;
        }
        self.paint_surface(s);
    }

    /// Clear window `s`'s selection and leave copy-mode (Esc, the copy-mode
    /// chord, and every "done" path).
    fn cancel_copy_mode(&mut self, s: Surface) {
        if let Some(term) = self.term_of_mut(s) {
            term.selection_clear();
        }
        self.exit_copy_mode(s);
    }

    /// Handle one key while copy-mode owns window `s`'s keyboard.
    fn copy_mode_key(
        &mut self,
        s: Surface,
        physical: winit::keyboard::PhysicalKey,
        logical: &winit::keyboard::Key,
        ctrl: bool,
    ) {
        use crate::copymode::Motion;
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        // Ctrl combos: half-page scroll (keyed on physical position, robust vs
        // control-char logical keys).
        if ctrl {
            if let PhysicalKey::Code(code) = physical {
                match code {
                    KeyCode::KeyU => self.copy_mode_motion(s, Motion::HalfPageUp),
                    KeyCode::KeyD => self.copy_mode_motion(s, Motion::HalfPageDown),
                    _ => {}
                }
            }
            return; // swallow every other Ctrl chord
        }
        // Non-motion commands.
        match logical {
            Key::Named(NamedKey::Escape) => {
                self.cancel_copy_mode(s);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.copy_mode_yank(s);
                return;
            }
            Key::Character(c) if c.as_str() == "y" => {
                self.copy_mode_yank(s);
                return;
            }
            Key::Character(c) if c.as_str() == "r" => {
                // Run the selection in a new tab — `y`'s sibling: `y` copies-
                // and-exits, `r` runs-in-a-new-tab-and-exits. Requires an
                // ACTIVE v/V selection; without one the key is swallowed
                // (copy-mode owns the keyboard) — never guess "current line".
                // Gated on the config opt-out too (sweep M3): with
                // `run_selection = false` the key must be a PURE no-op —
                // swallowed like any unbound copy-mode key, keeping the
                // selection and the mode — not a clear-and-exit surprise.
                let selecting = self.ov_of(s).and_then(|o| o.copy_mode).is_some_and(|cm| cm.selecting);
                if self.run_selection_enabled && selecting {
                    // run_selection_in_new_tab captures + clears the SOURCE
                    // selection BEFORE switching to the new tab; the extra
                    // clear covers the empty-selection no-op path so the exit
                    // mirrors the `y` arm exactly (clear + exit).
                    self.run_selection_in_new_tab(Self::sel_source(s));
                    self.cancel_copy_mode(s);
                }
                return;
            }
            Key::Character(c) if c.as_str() == "v" || c.as_str() == "V" => {
                let line = c.as_str() == "V";
                // Content-pinned anchor: capture the BUFFER line under the cursor
                // NOW, so scrolling while selecting extends into scrollback rather
                // than sliding the whole selection with the viewport.
                let Some((ov, term)) = self.ov_term_mut(s) else { return };
                let now_selecting = match ov.copy_mode.as_mut() {
                    Some(cm) => {
                        let anchor_line = term.viewport_line_to_buffer(cm.row);
                        if cm.selecting && cm.line_mode == line {
                            cm.selecting = false;
                            false
                        } else {
                            cm.begin_select(line, anchor_line);
                            true
                        }
                    }
                    None => false,
                };
                if now_selecting {
                    self.copy_mode_refresh_selection(s);
                } else {
                    if let Some(term) = self.term_of_mut(s) {
                        term.selection_clear();
                    }
                    self.paint_surface(s);
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
            Key::Character(c) if c.chars().count() == 1 => match c.chars().next().unwrap() {
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
            self.copy_mode_motion(s, m);
        }
        // else: swallow the key (copy-mode owns the keyboard).
    }

    /// Apply a copy-mode motion in window `s`: move the cursor, honour the
    /// scroll request, and re-drive the selection from the (possibly scrolled)
    /// viewport coords.
    fn copy_mode_motion(&mut self, s: Surface, motion: crate::copymode::Motion) {
        use crate::copymode::ScrollReq;
        let Some((ov, term)) = self.ov_term_mut(s) else { return };
        let Some(cm) = ov.copy_mode else { return };
        let (rows, cols) = (term.rows(), term.cols());
        let viewport = term.viewport_rows_chars();
        let out = crate::copymode::apply_motion(&cm, motion, rows, cols, &viewport);
        match out.scroll {
            ScrollReq::None => {}
            ScrollReq::Lines(n) => term.scroll_lines(n),
            ScrollReq::Top => {
                let max = term.scroll_max();
                term.scroll_to_offset(max);
            }
            ScrollReq::Bottom => term.scroll_to_bottom(),
        }
        if let Some(cm) = ov.copy_mode.as_mut() {
            cm.row = out.row.min(rows.saturating_sub(1));
            cm.col = out.col.min(cols.saturating_sub(1));
        }
        self.copy_mode_refresh_selection(s);
        self.paint_surface(s);
    }

    /// Rebuild window `s`'s selection from the copy-mode anchor + cursor with the
    /// DERIVED sub-cell sides (BLOCKING 2) — reading-order start=Left, end=Right
    /// — so the highlight/yank is inclusive on both ends regardless of
    /// direction. No-op when not selecting (never clobbers a cleared selection).
    fn copy_mode_refresh_selection(&mut self, s: Surface) {
        let Some((ov, term)) = self.ov_term_mut(s) else { return };
        let Some(cm) = ov.copy_mode else { return };
        if !cm.selecting {
            return;
        }
        let anchor = (cm.anchor_line, cm.anchor_col);
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

    /// Yank window `s`'s selection to the clipboard and exit copy-mode.
    fn copy_mode_yank(&mut self, s: Surface) {
        if let Some(t) = self.term_of(s).and_then(|t| t.selection_text()).filter(|t| !t.is_empty()) {
            crate::clipboard::set(&t);
        }
        self.cancel_copy_mode(s);
    }

    // ── Command palette ──────────────────────────────────────────────────────

    /// (Re)build the palette registry FRESH and open the overlay in window `s`.
    /// Building on open (~50 short entries) — not incrementally and NOT in
    /// `apply_theme` (which auto-repeats on opacity) — keeps the dynamic
    /// theme/tab/detach entries current at zero per-frame cost. Dismisses every
    /// peer overlay of that window so exactly one overlay owns keys + draws on
    /// top.
    fn open_palette(&mut self, s: Surface) {
        self.dismiss_surface_menus(s);
        if s == Surface::Main {
            self.welcome_open = false;
        }
        let themes = jetty_core::theme_list();
        let tabs: Vec<(u64, String)> = self.tabs.iter().map(|t| (t.id.0, t.title.clone())).collect();
        let detached: Vec<(u64, String)> =
            self.detached.iter().map(|d| (d.tab.id.0, d.tab.title.clone())).collect();
        let registry = crate::palette::build_registry(&themes, &tabs, &detached);
        let Some(ov) = self.ov_of_mut(s) else { return };
        ov.help_open = false;
        ov.palette_registry = registry;
        ov.palette_query.clear();
        ov.palette_open = true;
        ov.refilter_palette();
        self.paint_surface(s);
    }

    /// Close window `s`'s palette and free its transient state, so nothing is
    /// allocated while it is closed.
    fn close_palette(&mut self, s: Surface) {
        if self.ov_of_mut(s).is_some_and(|o| o.close_palette()) {
            self.paint_surface(s);
        }
    }

    /// Move window `s`'s palette selection by `delta` rows (clamped), keeping it
    /// inside the `MAX_PALETTE_ROWS` scroll window.
    fn palette_move(&mut self, s: Surface, delta: isize) {
        if let Some(ov) = self.ov_of_mut(s) {
            ov.palette_move(delta);
        }
    }

    /// Keys owned by window `s`'s MODAL overlays, in priority order
    /// (single-overlay-owns-keys): the command palette captures ALL keys while
    /// open — type → query, Up/Down → select, PageUp/Down → page, Enter → run +
    /// close, Esc → close, Backspace → edit, its own chord → toggle closed, every
    /// other Ctrl/Cmd chord swallowed so nothing leaks; then hint mode (letters
    /// narrow the label prefix, Esc cancels, Backspace pops, its chord toggles
    /// closed, everything else swallowed); then copy-mode. Chords are routed
    /// through the keymap so a remapped binding toggles closed consistently
    /// (amendment 4). Returns whether the key was consumed.
    fn overlay_key_modal(
        &mut self,
        s: Surface,
        event: &winit::event::KeyEvent,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(ov) = self.ov_of(s) else { return false };
        let (palette, hint, copy) = (ov.palette_open, ov.hint_mode.is_some(), ov.copy_mode.is_some());
        if !(palette || hint || copy) {
            return false;
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let sup = self.modifiers.super_key();
        let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
        let chord = self.keymap.lookup(mods, event.physical_key, &event.logical_key);
        if palette {
            if chord == Some(input::KeyAction::OpenPalette) {
                self.close_palette(s);
                return true;
            }
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => self.close_palette(s),
                Key::Named(NamedKey::Enter) => {
                    let cmd = self.ov_of(s).and_then(|o| o.palette_pick());
                    self.close_palette(s);
                    if let Some(c) = cmd {
                        self.run_palette_cmd(c, s, event_loop);
                    }
                    return true;
                }
                Key::Named(NamedKey::ArrowDown) => self.palette_move(s, 1),
                Key::Named(NamedKey::ArrowUp) => self.palette_move(s, -1),
                Key::Named(NamedKey::PageDown) => {
                    self.palette_move(s, jetty_render::MAX_PALETTE_ROWS as isize)
                }
                Key::Named(NamedKey::PageUp) => {
                    self.palette_move(s, -(jetty_render::MAX_PALETTE_ROWS as isize))
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(ov) = self.ov_of_mut(s) {
                        ov.palette_query.pop();
                        ov.refilter_palette();
                    }
                }
                _ => {
                    if ctrl || sup {
                        // Swallow other chords while the palette owns keys.
                    } else if let Some(t) = &event.text {
                        self.palette_type(s, t);
                    }
                }
            }
            self.paint_surface(s);
            return true;
        }
        if hint {
            if chord == Some(input::KeyAction::HintMode) {
                self.exit_hint_mode(s);
                return true;
            }
            self.hint_mode_key(s, event.physical_key, &event.logical_key);
            return true;
        }
        if chord == Some(input::KeyAction::CopyMode) {
            self.cancel_copy_mode(s);
            return true;
        }
        self.copy_mode_key(s, event.physical_key, &event.logical_key, ctrl);
        true
    }

    /// Keys owned by window `s`'s BAR overlays. The help captures Escape —
    /// fully consumed: it must NOT also close a tab or reach the shell — and,
    /// only while its rows overflow the window, the scroll keys (otherwise they
    /// reach the shell as before). The scrollback-search bar (after the help
    /// Esc, so help keeps Esc priority) captures every key while open: printable
    /// keys edit the query incrementally; Enter/F3 step older, Shift+Enter /
    /// Shift+F3 newer; Backspace pops; Esc / its chord close and CLEAR (no query
    /// retention); the Paste chord pastes into the query; every other Ctrl/Cmd
    /// chord is swallowed (alacritty-style). Returns whether the key was
    /// consumed.
    fn overlay_key_bars(&mut self, s: Surface, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(ov) = self.ov_of(s) else { return false };
        let (help, search) = (ov.help_open, ov.search_open);
        if help && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            if let Some(ov) = self.ov_of_mut(s) {
                ov.help_open = false;
            }
            self.dismiss_help_peers(s);
            self.paint_surface(s);
            return true;
        }
        if help {
            if let Key::Named(
                k @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::PageUp
                | NamedKey::PageDown
                | NamedKey::Home
                | NamedKey::End),
            ) = &event.logical_key
            {
                if let Some((max, page)) = self.help_scroll_range(s) {
                    let cur = self.ov_of(s).map_or(0, |o| o.help_scroll).min(max) as isize;
                    let page = page.max(1) as isize;
                    let to = match k {
                        NamedKey::ArrowUp => cur - 1,
                        NamedKey::ArrowDown => cur + 1,
                        NamedKey::PageUp => cur - page,
                        NamedKey::PageDown => cur + page,
                        NamedKey::Home => 0,
                        _ => max as isize,
                    };
                    self.set_help_scroll(s, to, max);
                    return true;
                }
            }
        }
        if !search {
            return false;
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let sup = self.modifiers.super_key();
        let mods = crate::keymap::Mods::new(ctrl, shift, alt, sup);
        let chord_action = self.keymap.lookup(mods, event.physical_key, &event.logical_key);
        if chord_action == Some(input::KeyAction::SearchToggle) {
            self.search_close(s);
            return true;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.search_close(s),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::F3) => {
                // Matches stale after a throttled streaming burst? Re-collect
                // FIRST so navigation steps real points instead of scrolling to
                // rotated rows (F10). Enter/F3 = older; +Shift = newer.
                if let Some((ov, term)) = self.ov_term_mut(s) {
                    if ov.search_dirty {
                        term.search_refresh();
                        ov.search_refreshed(std::time::Instant::now());
                    }
                    term.search_nav(!shift);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(term) = self.term_of_mut(s) {
                    let mut q = term.search_query().to_string();
                    q.pop();
                    term.search_set_query(&q);
                }
            }
            _ => {
                if chord_action == Some(input::KeyAction::Paste) {
                    if let Some(text) = clipboard::get() {
                        self.search_extend_query(s, &text);
                    }
                } else if ctrl || sup {
                    // Swallow other Ctrl/Cmd chords while the bar is open.
                } else if let Some(t) = &event.text {
                    self.search_extend_query(s, t);
                }
            }
        }
        self.paint_surface(s);
        true
    }

    /// Type `text` into window `s`'s palette query (printable chars only),
    /// refiltering when it changed.
    fn palette_type(&mut self, s: Surface, text: &str) {
        let Some(ov) = self.ov_of_mut(s) else { return };
        let mut changed = false;
        for ch in text.chars() {
            if !ch.is_control() {
                ov.palette_query.push(ch);
                changed = true;
            }
        }
        if changed {
            ov.refilter_palette();
        }
    }

    /// An IME commit while one of window `s`'s overlays owns its keyboard: hint
    /// mode / copy-mode DROP it (a CJK IME routes even Latin letters through
    /// commits, which must neither leak to the shell behind the overlay nor
    /// silently fail the mode's own keys — BLOCKING 1); the palette and the
    /// search bar take it into their query (CJK queries). Returns whether the
    /// commit was consumed.
    fn overlay_ime_commit(&mut self, s: Surface, text: &str) -> bool {
        let Some(ov) = self.ov_of(s) else { return false };
        if ov.hint_mode.is_some() || ov.copy_mode.is_some() {
            return true;
        }
        if ov.palette_open {
            self.palette_type(s, text);
            self.paint_surface(s);
            return true;
        }
        if ov.search_open {
            self.search_extend_query(s, text);
            self.paint_surface(s);
            return true;
        }
        false
    }

    /// Hint mode and copy-mode are primary-screen only: when a program switched
    /// window `s`'s terminal to the alt screen mid-mode (a full-screen TUI
    /// launched), drop them cleanly rather than draw stale chips / a cursor over
    /// the TUI. Cheap: one test while no mode is active.
    fn exit_modes_on_alt_screen(&mut self, s: Surface) {
        let Some((ov, term)) = self.ov_term_mut(s) else { return };
        if (ov.hint_mode.is_some() || ov.copy_mode.is_some()) && term.alt_screen() {
            if ov.copy_mode.is_some() {
                term.selection_clear();
            }
            ov.hint_mode = None;
            ov.copy_mode = None;
        }
    }

    /// A left press on window `s` while hint mode / copy-mode is active: hint
    /// mode is keyboard-only, so the click is swallowed (returns true);
    /// copy-mode exits and lets the click fall through to the normal mouse path
    /// (predictable, simple).
    fn modes_click(&mut self, s: Surface) -> bool {
        let Some(ov) = self.ov_of(s) else { return false };
        if ov.hint_mode.is_some() {
            return true;
        }
        if ov.copy_mode.is_some() {
            if let Some(term) = self.term_of_mut(s) {
                term.selection_clear();
            }
            if let Some(ov) = self.ov_of_mut(s) {
                ov.copy_mode = None;
            }
            self.paint_surface(s);
        }
        false
    }

    /// A left press while window `s`'s command palette is open. The palette
    /// captures the mouse: every press is consumed so none falls through to
    /// terminal selection, the scrollbar, or a button that could open another
    /// overlay over it. A click on a visible row runs it; a click outside the
    /// panel closes it; inside (non-row) is a no-op. Returns whether it was open.
    fn palette_click(&mut self, s: Surface, cx: f32, cy: f32, event_loop: &ActiveEventLoop) -> bool {
        if !self.ov_of(s).is_some_and(|o| o.palette_open) {
            return false;
        }
        let Some(pal) = self.layout_palette(s) else { return true };
        let first = self.ov_of(s).map_or(0, |o| o.palette_scroll);
        let hit = pal.row_hits.iter().position(|r| input::point_in(r, cx, cy)).map(|vi| first + vi);
        if let Some(gi) = hit {
            let cmd = self.ov_of(s).and_then(|o| o.palette_filtered.get(gi)).map(|h| h.cmd.clone());
            self.close_palette(s);
            if let Some(c) = cmd {
                self.run_palette_cmd(c, s, event_loop);
            }
        } else if !input::point_in(&pal.panel, cx, cy) {
            self.close_palette(s);
        }
        true
    }

    /// A left press while window `s`'s help is open. Modal: a click outside its
    /// panel closes it, a click inside is swallowed — either way the click is
    /// consumed so it never reaches the tab bar, a resize edge, or the terminal.
    fn help_click(&mut self, s: Surface, cx: f32, cy: f32) -> bool {
        let Some(ov) = self.ov_of(s) else { return false };
        if !ov.help_open {
            return false;
        }
        let scroll = ov.help_scroll;
        if let Some(help) = self.layout_help(s, scroll) {
            if !input::point_in(&help.panel, cx, cy) {
                if let Some(ov) = self.ov_of_mut(s) {
                    ov.help_open = false;
                }
            }
        }
        self.paint_surface(s);
        true
    }

    /// A left press while window `s`'s search bar is open — NON-modal for the
    /// mouse: ✕ closes + clears, a click inside the panel is swallowed, clicks
    /// OUTSIDE fall through so terminal selection keeps working. Laid out with
    /// the SAME call as the draw pass for hit parity. Returns whether consumed.
    fn search_bar_click(&mut self, s: Surface, cx: f32, cy: f32) -> bool {
        if !self.ov_of(s).is_some_and(|o| o.search_open) {
            return false;
        }
        let Some(bar) = self.layout_search_bar(s) else { return false };
        if input::point_in(&bar.close_rect, cx, cy) {
            self.search_close(s);
            return true;
        }
        input::point_in(&bar.panel, cx, cy)
    }

    /// Toggle the perf HUD. Extracted so every caller shares the reflow: the HUD
    /// reserves grid rows via `status_h`, so a bare flag flip would leave the grid
    /// the wrong size (a bare `= !; persist; redraw` is a bug — see the config
    /// reload path, which reflows for the same reason).
    fn toggle_perf_hud(&mut self) {
        self.set_perf_hud(!self.show_perf_hud);
        self.persist();
    }

    /// Show/hide the perf HUD in EVERY window (the palette/key toggle and a
    /// config hot-reload both land here). The strip reserves grid rows, so the
    /// main grid reflows now and each detached window's reflow is armed for this
    /// loop pass (its strip height is its own DPI's) — before, only the main
    /// window re-gridded and a detached strip covered its last row (the prompt)
    /// until that window was resized. No-op when unchanged.
    fn set_perf_hud(&mut self, on: bool) {
        if on == self.show_perf_hud {
            return;
        }
        self.show_perf_hud = on;
        self.reflow();
        let now = std::time::Instant::now();
        for dw in &mut self.detached {
            dw.reflow_pending_at = Some(now);
        }
        self.mark_dirty_all();
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
    /// in `about_to_wait` only (the render tails no longer self-drive) and
    /// deliberately does NOT route here.
    fn request_main_paint(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Repaint whichever JeTTY window `id` names, if it is EFFECTIVELY VISIBLE
    /// (main: shown and not occluded; detached: not occluded). A window that is
    /// gone or hidden is a no-op — it repaints on its own when it comes back.
    /// Returns whether a paint was requested.
    fn request_window_paint(&self, id: WindowId) -> bool {
        if self.window.as_ref().is_some_and(|w| w.id() == id) {
            if self.visible && !self.main_occluded {
                self.request_main_paint();
                return true;
            }
        } else if let Some(dw) = self.detached.iter().find(|d| d.window.id() == id) {
            if !dw.occluded {
                dw.request_paint();
                return true;
            }
        }
        false
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
    /// The palette is already closed by the caller. `s` is the window the palette
    /// was opened in: per-window commands (search, hint/copy mode, run
    /// selection, prompt jumps, copy/paste, fullscreen) act on THAT window — and
    /// from a detached window, close/detach tab mean "reattach" exactly like
    /// their chords there; app-wide commands stay app-wide, and the ones that
    /// change the main window (tab nav, new tab, welcome, quit) bring it up so
    /// the user sees the result. Index-bearing variants are `.get()`-guarded
    /// (bounds-checked) so a tab/theme that vanished between open and Enter is a
    /// clean no-op — belt-and-suspenders on top of build-on-open.
    fn run_palette_cmd(&mut self, cmd: crate::palette::PaletteCmd, s: Surface, event_loop: &ActiveEventLoop) {
        use crate::palette::PaletteCmd as C;
        // From a detached window, commands that act on the MAIN window raise it.
        let reveal_main = |app: &mut Self, event_loop: &ActiveEventLoop| {
            if s != Surface::Main {
                app.set_visibility(true, event_loop);
            }
        };
        match cmd {
            C::NewTab => match s {
                Surface::Main => self.new_tab(),
                Surface::Detached(p) => {
                    // Inherit THIS detached tab's cwd; the tab opens in the main window.
                    let cwd = self.detached.get(p).and_then(|dw| dw.tab.pty.cwd());
                    if self.new_tab_with_cwd(cwd).is_some() {
                        reveal_main(self, event_loop);
                    }
                }
            },
            C::CloseTab => match s {
                Surface::Main => {
                    self.confirm_close = self.tabs.get(self.active).map(|t| t.id);
                    self.request_main_paint();
                }
                Surface::Detached(p) => self.reattach_tab(p, event_loop),
            },
            C::NextTab | C::PrevTab => {
                if !self.tabs.is_empty() {
                    self.switch_tab(matches!(cmd, C::NextTab));
                    reveal_main(self, event_loop);
                }
            }
            C::DetachTab => match s {
                Surface::Main => self.detach_tab(self.active, event_loop, None),
                Surface::Detached(p) => self.reattach_tab(p, event_loop),
            },
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
                reveal_main(self, event_loop);
            }
            C::Search => self.search_open(s),
            // The palette has already closed (run_palette_cmd runs after
            // close_palette), so overlay_owns_keys() is false and the mode enters.
            C::HintMode => self.enter_hint_mode(s),
            C::CopyMode => self.enter_copy_mode(s),
            // Clean no-op without a selection (the method aborts on Empty).
            C::RunSelection => self.run_selection_in_new_tab(Self::sel_source(s)),
            C::PrevPrompt | C::NextPrompt => {
                let forward = matches!(cmd, C::NextPrompt);
                if self.term_of_mut(s).is_some_and(|t| t.jump_prompt(forward)) {
                    self.paint_surface(s);
                    match s {
                        Surface::Main => self.update_link_hover(true),
                        Surface::Detached(p) => self.update_detached_link_hover(p, true),
                    }
                }
            }
            C::Copy => {
                let copied = self.term_of(s).and_then(|t| t.selection_text()).filter(|t| !t.is_empty());
                if let Some(text) = copied {
                    clipboard::set(&text);
                    if let Some(term) = self.term_of_mut(s) {
                        term.selection_clear();
                    }
                    self.paint_surface(s);
                }
            }
            C::Paste => {
                if let Some(text) = clipboard::get() {
                    match s {
                        Surface::Main => self.paste_text(&text),
                        Surface::Detached(p) => {
                            if let Some(dw) = self.detached.get_mut(p) {
                                Self::paste_to_tab(&mut dw.tab, &text);
                            }
                        }
                    }
                }
            }
            C::ToggleLaunchAtLogin => {
                self.launch_at_login = !self.launch_at_login;
                if let Err(e) = set_launch_at_login(self.launch_at_login) {
                    self.show_config_warnings(&[e]);
                }
                self.persist();
            }
            C::ResetKeybindings => {
                // Destructive (every hand-written binding goes), so it asks first:
                // the first run arms a confirmation, a second run within 6 s
                // resets — after copying config.toml aside.
                let now = std::time::Instant::now();
                if self.reset_keys_armed_until.is_none_or(|t| now >= t) {
                    self.reset_keys_armed_until = Some(now + std::time::Duration::from_secs(6));
                    self.show_notice_pill(
                        "Run \u{201c}Reset keybindings\u{201d} again within 6 s to confirm (a backup is saved first)"
                            .to_string(),
                        6000,
                    );
                    return;
                }
                self.reset_keys_armed_until = None;
                let backed_up = self.persister.borrow_mut().backup();
                let backup = match backed_up {
                    Ok(path) => path,
                    Err(e) => {
                        // No backup → no reset: the user's bindings stay intact.
                        self.show_config_warnings(&[format!(
                            "keybindings NOT reset — could not back up config.toml: {e}"
                        )]);
                        return;
                    }
                };
                // Clear every user `[keys]` override → back to the built-in defaults.
                self.keys = crate::config::KeyBindings::default();
                self.keymap = crate::keymap::KeyMap::compile(&self.keys);
                self.help_rows = App::compute_help_rows(&self.keymap, &self.summon_hotkey);
                self.persist();
                let msg = match backup {
                    Some(p) => format!(
                        "Keybindings reset to defaults — backup: {}",
                        p.file_name().and_then(|n| n.to_str()).unwrap_or("config.toml.bak")
                    ),
                    None => "Keybindings reset to defaults".to_string(),
                };
                self.show_notice_pill(msg, 6000);
            }
            // Fullscreen toggles the window the palette was opened in.
            C::ToggleFullscreen => match s {
                Surface::Main => self.set_main_fullscreen(!self.main_fullscreen),
                Surface::Detached(p) => self.toggle_fullscreen_detached(p),
            },
            C::Hide => self.set_visibility(false, event_loop),
            C::Quit => {
                self.confirm_quit = true;
                self.request_main_paint();
                reveal_main(self, event_loop);
            }
            // Index-bearing dynamic actions: `.get()`-guard against a stale index.
            C::SetTheme(i) => {
                if i < jetty_core::theme_count() {
                    self.pick_theme(i);
                    self.persist();
                    self.redraw_main_and_detached();
                }
            }
            // Id-bearing dynamic actions: a tab that closed (or moved) between
            // open and Enter resolves to nothing — never to its neighbour.
            C::SelectTab(id) => {
                if let Some(i) = self.tab_index(TabId(id)) {
                    self.select_tab(i);
                    reveal_main(self, event_loop);
                }
            }
            C::Reattach(id) => {
                if let Some(p) = self.detached_index(TabId(id)) {
                    self.reattach_tab(p, event_loop);
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
        let next = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        self.set_active_tab(next);
    }

    /// Make tab `idx` (clamped) the main window's active tab. THE single path for
    /// an active-tab change: new tab, tab switch/select, reattach and (via
    /// [`Self::entered_new_active_tab`]) the close/detach/exit paths that remove
    /// the active tab — each used to reset its own subset of the outgoing tab's
    /// state. Precondition: `self.active` still names the OUTGOING tab, so its
    /// search state is cleared before the index moves. Re-selecting the active
    /// tab only repaints.
    fn set_active_tab(&mut self, idx: usize) {
        if self.tabs.is_empty() {
            return;
        }
        let idx = idx.min(self.tabs.len() - 1);
        if idx == self.active {
            self.request_main_paint();
            return;
        }
        // The search bar targets the ACTIVE tab: close it (clearing the
        // outgoing tab's regex/matches) before the index moves (F2/F7/F15).
        self.search_close(Surface::Main);
        self.active = idx;
        self.entered_new_active_tab();
    }

    /// Drop the transient state that belonged to the previously active tab once
    /// a DIFFERENT tab is active (after `set_active_tab`, or a removal path whose
    /// active tab was the one removed): hint chips and the copy-mode cursor are
    /// anchored on the old grid, an in-progress selection drag and a fractional
    /// wheel remainder were that tab's, and the cached Ctrl+hover underline must
    /// be recomputed against the new grid — Ctrl+Tab keeps Ctrl held (no
    /// ModifiersChanged) and the hovered CELL is unchanged, so without the forced
    /// recompute tab 1's underline ghosts over tab 2's text (F12).
    fn entered_new_active_tab(&mut self) {
        self.ov.hint_mode = None;
        self.ov.copy_mode = None;
        // A selection drag, scrollbar drag, or button the outgoing tab's program
        // saw pressed can't be released into the new tab.
        self.reset_main_pointer();
        self.scroll_accum.reset();
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
        self.set_active_tab(n);
    }

    /// Commit an in-progress tab rename: write `rename_buf` back to the tab's
    /// title and clear the rename state. No-op when not renaming. An empty buffer
    /// is ignored (keep the previous title) so a tab never ends up nameless.
    fn commit_rename(&mut self) {
        if let Some(id) = self.renaming.take() {
            let trimmed = self.rename_buf.trim().to_string();
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id).filter(|_| !trimmed.is_empty()) {
                tab.title = trimmed;
                // Manual rename permanently wins over shell OSC 0/2 titles for
                // this tab. An empty rename (no-op above) deliberately does NOT
                // set the flag, so auto-titles stay live.
                tab.manually_renamed = true;
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

    /// Enter / leave OS fullscreen on the MAIN window — the single chokepoint.
    ///
    /// TRANSIENT: nothing is persisted here. `window_mode == Fullscreen` is the
    /// persisted intent; this is the live shape, which an ad-hoc
    /// `ToggleFullscreen` (F11) can also set in Center / Dropdown mode. Leaving
    /// lands the window back in the geometry the PERSISTED `window_mode` asks
    /// for, so an ad-hoc exit is indistinguishable from never having gone
    /// fullscreen.
    ///
    /// HARD RULES enforced here:
    /// * Rule F0 — never ENTER fullscreen on a hidden window (X11 resolves
    ///   `Borderless(None)` from the window frame; macOS's simple fullscreen
    ///   `expect`s a screen, which an ordered-out window does not have). Both hide
    ///   paths leave fullscreen before unmapping, so `main_fullscreen` is `false`
    ///   whenever `!visible` and the enter is always a genuine `None → Some`
    ///   transition that winit's X11 dedupe cannot swallow.
    /// * Never arm `pending_dock_frames` / `pending_center_frames` (nor issue any
    ///   geometry call) while `!visible`: those counters only decrement inside
    ///   `RedrawRequested`, which a hidden window never receives on macOS, so
    ///   arming them while hidden pins `ControlFlow::Poll` and burns a core
    ///   invisibly (the F18 bug class) — and on X11 it would fire five docks
    ///   → five `Resized` → a SIGWINCH storm to every hidden tab.
    /// * MAXIMIZE and FULLSCREEN stay orthogonal booleans (amendment I-F). F11
    ///   while maximized enters fullscreen and does NOT un-maximize; the exit then
    ///   SKIPS the explicit geometry restore while `is_maximized()`, because a
    ///   position set on a maximized X11 window is ignored or half-applied — the WM
    ///   restores the maximized frame itself. So maximize → F11 → ▢ hands back a
    ///   maximized window, and a second ▢ normalises it.
    /// * Never call `reflow()` or `gpu.resize()` from here: the WM's own `Resized`
    ///   event drives the existing 250 ms debounce, which also collapses macOS's
    ///   multi-event transition into exactly ONE reflow. Reflowing here as well
    ///   would be the double-reflow pattern that scatters p10k's prompt.
    fn set_main_fullscreen(&mut self, on: bool) {
        if self.main_fullscreen == on {
            return;
        }
        let Some(win) = self.window.clone() else {
            // Pre-`resumed`: no window yet. Do NOT latch the flag — it mirrors a
            // MAPPED window, and `resumed` establishes it from `window_mode`.
            return;
        };
        if on {
            // Rule F0: entering requires a mapped window. When hidden, the next
            // summon applies the persisted mode instead.
            if !self.visible {
                return;
            }
            // Remember the spot AND the size BEFORE the frame becomes the monitor:
            // after the async un-fullscreen request, `outer_size()` still reports
            // the monitor on X11, and centring from that lands in the corner.
            capture_pre_fullscreen(
                &win,
                self.window_mode,
                &mut self.last_pos,
                &mut self.last_windowed_size,
            );
            // The other half of the sibling-window rule (see
            // `close_settings_for_fullscreen`): `toggle_settings_window` leaves
            // fullscreen before OPENING Settings, so becoming fullscreen while
            // Settings is already open must close it — otherwise clicking
            // WINDOW MODE ▸ Fullscreen, the primary discovery gesture, buries the
            // panel behind the terminal.
            self.close_settings_for_fullscreen();
            // macOS only: at most one JeTTY window may be fullscreen at a time
            // (an app-scoped presentation-options wart — see the helper). A no-op
            // on every other platform, deliberately.
            self.enforce_single_fullscreen(None);
            // Nothing may re-assert a docked/centred rect over a fullscreen window,
            // and the top-strip slide must not run on a monitor-tall window (it
            // would show a monitor-tall band of desktop for 150 ms).
            self.pending_dock_frames = 0;
            self.pending_center_frames = 0;
            self.pending_center_pos = None;
            self.slide_anim = None;
            self.main_fullscreen = true;
            jetty_platform::set_window_fullscreen(&win, true);
        } else {
            self.main_fullscreen = false;
            // Safe unconditionally: only the ENTER path needs a mapped window.
            jetty_platform::set_window_fullscreen(&win, false);
            // One decision, taken by a PURE function so the "never arm a counter
            // while hidden" invariant is unit-testable (see
            // `fullscreen_exit_frames`). `restorable` folds the F32 stale-monitor
            // gate: a saved position on a since-disconnected monitor is not
            // restorable, so we centre on a live monitor instead of mapping
            // off-screen.
            let maximized = win.is_maximized();
            let restorable = self
                .last_pos
                .map(|p| pos_on_some_monitor(&win, p))
                .unwrap_or(false);
            let (dock_frames, center_frames) =
                fullscreen_exit_frames(self.visible, maximized, self.window_mode, restorable);
            self.pending_dock_frames = dock_frames;
            self.pending_center_frames = center_frames;
            self.pending_center_pos = None;
            // Geometry is only ever issued on a VISIBLE, non-maximized window:
            // hidden ⇒ the next summon places it (and an OS geometry call while
            // hidden is forbidden); maximized ⇒ the WM restores the maximized frame
            // itself, and an explicit position on a maximized X11 window is ignored
            // or half-applied.
            if self.visible && !maximized {
                match self.window_mode {
                    // Fullscreen mode + an ad-hoc F11 exit is a temporary escape that
                    // restores like Center; the next summon re-establishes
                    // fullscreen, so the escape is self-healing and needs no
                    // "escaped" flag.
                    WindowMode::Center | WindowMode::Fullscreen => {
                        match self.last_pos.filter(|_| restorable) {
                            Some(p) => {
                                win.set_outer_position(p);
                                self.pending_center_pos = Some(p);
                            }
                            None => {
                                // Centre from the size captured on the way IN, never
                                // from the live `outer_size()`: the un-fullscreen
                                // request above is an async ClientMessage on X11, so
                                // a live read still reports the MONITOR and the
                                // centring would resolve to the monitor origin.
                                // Re-assert the COMPUTED target for the next few
                                // post-map frames, exactly like the `Some(p)` sibling
                                // — the WM processes our restore and our position
                                // request in its own order, and whichever lands first
                                // the re-assertion settles it.
                                let target = center_window_sized(&win, self.last_windowed_size);
                                self.pending_center_pos = target;
                                if target.is_none() {
                                    // No monitor info ⇒ nothing was issued ⇒ nothing
                                    // to re-assert (never leave a counter armed with
                                    // no work: it would be 5 frames of Poll).
                                    self.pending_center_frames = 0;
                                }
                                self.last_pos = None;
                            }
                        }
                    }
                    WindowMode::Dropdown => {
                        dock_window_top(&win, self.dropdown_width_pct, self.dropdown_height_pct);
                        // No slide here: this is a restore, not a summon.
                    }
                }
            }
        }
        // The corner radius changed ⇒ repaint. No persist(): transient state.
        self.request_main_paint();
        // …and so did the Settings panel's CORNER RADIUS dim state. The panel is
        // rebuilt from scratch on every settings RedrawRequested, so this only has
        // to REQUEST the repaint — without it a live panel shows a stale dim until
        // some unrelated event happens to repaint it. (`set_window_mode` already
        // does this on both of its legs.)
        self.request_settings_paint();
    }

    /// Close the Settings window because the MAIN window is about to become
    /// fullscreen. The symmetric half of `toggle_settings_window`'s
    /// "exit fullscreen before opening Settings".
    ///
    /// A fullscreen window sits in the WM's above-normal EWMH layer, and whether a
    /// WM demotes it on focus loss is WM-specific — which we may not special-case.
    /// So a Settings window that is still open when the terminal goes fullscreen
    /// is focused but INVISIBLE behind it, and Settings is the only UI for leaving
    /// the mode. That is reachable from the PRIMARY discovery gesture (clicking
    /// WINDOW MODE ▸ Fullscreen inside Settings), from an F9 summon in Fullscreen
    /// mode with Settings open, and from a `window_mode` hot-reload.
    ///
    /// Closing is lossless: every panel value is already persisted, and the panel
    /// is rebuilt from scratch on the next open. `Ctrl+Shift+O` then re-opens it
    /// and takes the exit-fullscreen leg, so the two rules compose.
    fn close_settings_for_fullscreen(&mut self) {
        if self.settings_window.is_some() {
            self.close_settings_window();
        }
    }

    /// Rule F0: leave OS fullscreen while the window is still MAPPED, with NO
    /// geometry restore and no repaint. Returns whether we were fullscreen.
    ///
    /// Used by both hide paths — `set_visibility(false)` and
    /// `autohide_main_window` — immediately BEFORE `set_visible(false)`, and
    /// before the window is dropped (app exit). Keeping the OS state out of the
    /// hidden period is what makes the next summon's enter a genuine
    /// `None → Some` transition, keeps macOS's simple fullscreen off a screen-less
    /// window, and stops an ad-hoc F11 leaking across a hide/summon round-trip.
    /// On macOS it is also what restores the app-scoped `presentationOptions`
    /// (auto-hidden Dock + menu bar) — those are only ever restored by the
    /// matching `set_simple_fullscreen(false)`.
    fn exit_main_fullscreen_bare(&mut self) -> bool {
        if !self.main_fullscreen {
            return false;
        }
        self.main_fullscreen = false;
        if let Some(win) = &self.window {
            jetty_platform::set_window_fullscreen(win, false);
        }
        true
    }

    /// Enter / leave OS fullscreen on ONE detached window — the per-window
    /// analogue of `set_main_fullscreen`, and the chokepoint F11 uses inside a
    /// detached window.
    ///
    /// NO geometry restore: a detached window persists no position and has no
    /// `window_mode`, so the WM restores its pre-fullscreen frame on exit (and on
    /// macOS `set_simple_fullscreen(false)` restores the saved rect itself).
    /// NO `reflow()` / `gpu.resize()` either — the WM's own `Resized` event arms
    /// `dw.reflow_pending_at` and the existing 250 ms debounce fires exactly one
    /// reflow, exactly as for a border drag.
    ///
    /// Rule F0 ("never fullscreen an unmapped window") is satisfied by
    /// construction here: a detached window is never hidden, it has no summon
    /// model. The real hazard is DROPPING one while fullscreen — see
    /// `exit_detached_fullscreen_bare`.
    fn set_detached_fullscreen(&mut self, pos: usize, on: bool) {
        match self.detached.get(pos) {
            Some(dw) if dw.fullscreen != on => {}
            // Missing window, or already in the requested state.
            _ => return,
        }
        if on {
            // macOS only (see `enforce_single_fullscreen`): a no-op elsewhere, so
            // F11 in a detached window no longer yanks the main window out of
            // fullscreen — and out of its fullscreen geometry — on X11.
            self.enforce_single_fullscreen(Some(pos));
        }
        let Some(dw) = self.detached.get_mut(pos) else { return };
        dw.fullscreen = on;
        // Drop any in-flight manual top-bar drag. The PRESS site is gated on
        // `!dw.fullscreen`, but a latch taken BEFORE F11 was never cleared, so
        // every subsequent `CursorMoved` kept issuing `set_outer_position` on a
        // fullscreen window (the "broken half-state on X11" the sibling comments
        // warn about) and the release still ran the drop-to-reattach test against
        // the fullscreen origin. The main window has no equivalent latch — its move
        // gesture is the OS-driven `drag_window()`, which the WM ends itself.
        dw.bar_drag = None;
        dw.bar_drag_start = None;
        jetty_platform::set_window_fullscreen(&dw.window, on);
        // The corner radius changed ⇒ repaint. No persist(): transient state.
        dw.request_paint();
    }

    /// F11 inside a detached window toggles THAT window (never the main one).
    fn toggle_fullscreen_detached(&mut self, pos: usize) {
        let on = match self.detached.get(pos) {
            Some(dw) => !dw.fullscreen,
            None => return,
        };
        self.set_detached_fullscreen(pos, on);
    }

    /// Leave OS fullscreen on ONE detached window while it still EXISTS, with no
    /// repaint (the window is about to be destroyed). The detached analogue of
    /// `exit_main_fullscreen_bare`.
    ///
    /// MUST run before any `DetachedWindow` is dropped — reattach, the ✕ /
    /// CloseRequested path, a shell that exited inside it, and app exit. On macOS
    /// `set_simple_fullscreen(true)` saves and overwrites APP-scoped
    /// `NSApplication.presentationOptions` (auto-hide Dock | auto-hide menu bar)
    /// and ONLY the matching `set_simple_fullscreen(false)` restores them, so a
    /// dropped fullscreen window leaves the user's Dock and menu bar auto-hidden
    /// for the rest of the session, in every other app. A no-op everywhere else
    /// and whenever the window is not fullscreen.
    fn exit_detached_fullscreen_bare(&mut self, pos: usize) {
        let Some(dw) = self.detached.get_mut(pos) else { return };
        if !dw.fullscreen {
            return;
        }
        dw.fullscreen = false;
        jetty_platform::set_window_fullscreen(&dw.window, false);
    }

    /// SINGLE-FULLSCREEN-WINDOW RULE — **macOS only**: at most one JeTTY window
    /// may be in OS fullscreen at a time, so every ENTER path first leaves
    /// fullscreen on all the others. A no-op on every other platform.
    ///
    /// WHY IT EXISTS, and why it is macOS-only: `set_simple_fullscreen(true)`
    /// SAVES the APP-scoped `NSApplication.presentationOptions` into the entering
    /// window before overwriting them (auto-hide Dock | auto-hide menu bar), and
    /// only the matching `set_simple_fullscreen(false)` restores them. Two windows
    /// therefore NEST: the second saves the already-modified options, and an
    /// out-of-order exit re-applies an auto-hidden Dock and menu bar permanently,
    /// in every other app, for the rest of the session. That is a genuine
    /// app-scoped API wart with no cross-platform equivalent: X11/Wayland/Windows
    /// all go through `set_fullscreen(Some(Borderless(None)))`, which has no
    /// app-scoped side effect whatsoever.
    ///
    /// WHY NOT EVERYWHERE. The other argument for the rule — "two stacked windows
    /// in the WM's above-normal EWMH layer are a focus/visibility trap" — only
    /// holds when they are on the SAME monitor, and the case the rule forbids
    /// (main fullscreen on monitor 1, a detached window fullscreen on monitor 2)
    /// is exactly the one where no stacking trap exists. Off macOS the rule cost
    /// a real capability AND unrequested motion: F11 in a detached window routed
    /// the main window out through the full `set_main_fullscreen(false)`, which
    /// restores geometry and arms 5 re-assertion frames — a visible jump of a
    /// window the user never touched, to serve an Objective-C ivar that does not
    /// exist on that machine. So the "exit the others" STEP is scoped; the single
    /// named chokepoint stays (all three enter paths still call it, so the
    /// invariant remains greppable).
    ///
    /// NOT scoped, on purpose: the DROP-time exits
    /// (`exit_detached_fullscreen_bare` / `exit_main_fullscreen_bare` before every
    /// reattach, close, shell-exit and `exiting()`). They cost nothing off macOS,
    /// they are the actual presentation-options leak fix, and keeping them
    /// unconditional keeps ONE code path for everyone.
    ///
    /// `keep` names the detached window that is about to enter; `None` means the
    /// MAIN window is. The main window leaves through the full
    /// `set_main_fullscreen(false)` so it lands back in its persisted mode's
    /// geometry rather than sitting monitor-sized and windowed.
    fn enforce_single_fullscreen(&mut self, keep: Option<usize>) {
        #[cfg(target_os = "macos")]
        {
            if keep.is_some() && self.main_fullscreen {
                self.set_main_fullscreen(false);
            }
            for i in 0..self.detached.len() {
                if keep == Some(i) {
                    continue;
                }
                // `false` never recurses back into this helper.
                self.set_detached_fullscreen(i, false);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = keep;
        }
    }

    /// Select a new window mode: persist it, and apply it live. Switching to
    /// Center clears any in-progress slide; switching to Dropdown clears last_pos
    /// so the next summon re-docks from a clean top-flush geometry; switching to
    /// Fullscreen covers the monitor immediately when visible.
    fn set_window_mode(&mut self, mode: WindowMode) {
        if self.window_mode == mode {
            return;
        }
        let was_fullscreen = self.main_fullscreen;
        // Assign FIRST: `set_main_fullscreen` reads `window_mode` to decide both
        // the `last_pos` capture and where an exit lands.
        self.window_mode = mode;
        if mode != WindowMode::Fullscreen && was_fullscreen {
            // Leaving fullscreen (whether it came from the mode or from an ad-hoc
            // F11): exit the OS state AND apply the NEW mode's geometry —
            // `set_main_fullscreen(false)` does both.
            self.set_main_fullscreen(false);
            match mode {
                WindowMode::Center => {
                    // The same housekeeping the `match mode` block below does for
                    // Center, stated EXPLICITLY rather than relying on
                    // `set_main_fullscreen`'s clears happening to cover it: this
                    // early return skips that block, and one future arming site in
                    // the fullscreen state would otherwise leave a stuck
                    // `slide_anim` — a `main_pending` term, i.e. permanent Poll.
                    self.slide_anim = None;
                    self.pending_dock_frames = 0;
                }
                WindowMode::Dropdown => {
                    // Match today's Dropdown mode-switch exactly, so the slide-in
                    // does not depend on whether F11 happened to be pressed earlier.
                    self.last_pos = None;
                    // …but only when the exit actually DOCKED the window.
                    // `set_main_fullscreen(false)` skips the dock while maximized
                    // (amendment I-F: a position on a maximized X11 window is
                    // ignored or half-applied) and `fullscreen_exit_frames` returns
                    // (0, 0) there — so maximize → F11 → Dropdown leaves an
                    // undocked, still-maximized window, and sliding it would animate
                    // a top strip that was never there.
                    let maximized = self.window.as_ref().is_some_and(|w| w.is_maximized());
                    if self.visible && !maximized {
                        self.slide_anim = Some(std::time::Instant::now());
                    }
                }
                // Unreachable: this branch only runs when `mode != Fullscreen`.
                WindowMode::Fullscreen => {}
            }
            self.persist();
            self.request_main_paint();
            self.request_settings_paint();
            return;
        }
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
            WindowMode::Fullscreen => {
                // No slide (a monitor-tall band of desktop for 150 ms) and no
                // dock/center re-assertion can be allowed to fight the fullscreen
                // frame.
                self.slide_anim = None;
                self.pending_dock_frames = 0;
                self.pending_center_frames = 0;
                self.pending_center_pos = None;
                // Apply LIVE when visible so the Settings cycler feels instant;
                // when hidden the next summon applies it (and no OS/geometry call
                // may be issued while hidden anyway).
                if self.visible {
                    self.set_main_fullscreen(true);
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

    /// Logical size the Settings window needs so the panel — which scales its
    /// fixed `PANEL_W`×`PANEL_H` layout by the settings chrome unit (DPI × the
    /// CAPPED panel text size / 16) — is never clipped. A larger UI font size
    /// grows the panel, so the window must grow with it (the panel body font is
    /// capped to `[PANEL_TEXT_MIN, PANEL_TEXT_MAX]`, so this is bounded to
    /// ~1.06×). The UI font FAMILY no longer changes the panel's size (it used
    /// to, via the font's 'M' advance — a proportional family inflated it
    /// ~1.5×). Clamped to the monitor so the window always stays on-screen.
    fn desired_settings_logical_size(&self) -> (u32, u32) {
        let scale = self
            .settings_window
            .as_ref()
            .or(self.window.as_ref())
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0)
            .max(0.5);
        // Physical panel = PANEL_W × u; logical = that / the window's DPI.
        let f = (self.settings_metrics().overlay_u() / scale).max(1.0);
        let mut w = (jetty_render::PANEL_W * f).ceil() as u32 + 4;
        let mut h = (jetty_render::PANEL_H * f).ceil() as u32 + 4;
        // Never exceed the monitor (leave a margin) so the window stays on-screen.
        if let Some(mon) = self
            .settings_window
            .as_ref()
            .or(self.window.as_ref())
            .and_then(|w| w.current_monitor())
        {
            let msz = mon.size();
            let max_w = ((msz.width as f32 / scale) - 40.0).max(200.0) as u32;
            let max_h = ((msz.height as f32 / scale) - 80.0).max(200.0) as u32;
            w = w.min(max_w);
            h = h.min(max_h);
        }
        (w, h)
    }

    /// Resize the open Settings window to fit the panel at the current UI font
    /// (see [`Self::desired_settings_logical_size`]). No-op when Settings is
    /// closed. Called after any UI-font size/family change so the window re-fits.
    fn resize_settings_to_fit(&self) {
        if let Some(win) = self.settings_window.as_ref() {
            let (w, h) = self.desired_settings_logical_size();
            let _ = win.request_inner_size(winit::dpi::LogicalSize::new(w, h));
        }
    }

    /// Convert the current cursor pixel position into 1-based terminal cell
    /// coordinates `(col, row)` using the renderer's cell size, CLAMPED to the
    /// active grid (`1..=cols`, `1..=rows`) — a click in the scrollbar gutter
    /// or the status strip must never put out-of-range coordinates into a
    /// mouse report (xterm clamps to the grid edge; apps hit-testing panes get
    /// confused otherwise). Returns `None` when the renderer (and thus cell
    /// metrics) is not yet available or no tab exists.
    /// Where the main window's grid sits, for the shared grid mouse handling
    /// (`crate::gridmouse`): its origin, the band a click counts as on it (above
    /// a bottom tab bar / the status strip) and the cell size.
    fn main_grid_geom(&self) -> crate::gridmouse::GridGeom {
        let h = self.gpu.as_ref().map_or(0.0, |g| g.config.height as f32);
        let bottom = if self.tab_bar_bottom { self.tabbar_y(h) } else { h - self.status_h() };
        let (cell_w, cell_h) = self.text.as_ref().map_or((0.0, 0.0), |t| t.cell_size());
        crate::gridmouse::GridGeom { top: self.grid_top_offset(), bottom, cell_w, cell_h }
    }

    /// Run one shared grid-mouse step on the main window's active tab and write
    /// what it produced for the program to that tab's PTY. `None` without tabs.
    fn with_main_grid<R>(&mut self, f: impl FnOnce(&mut crate::gridmouse::Grid) -> R) -> Option<R> {
        if self.tabs.is_empty() {
            return None;
        }
        let geom = self.main_grid_geom();
        let mut out = Vec::new();
        let tab = &mut self.tabs[self.active];
        let r = f(&mut crate::gridmouse::Grid {
            term: &mut tab.terminal,
            mouse: &mut self.grid_mouse,
            selecting: &mut self.selecting,
            geom,
            pointer: self.cursor,
            mods: self.modifiers,
            out: &mut out,
        });
        write_pty_bytes(&mut tab.writer, &out);
        Some(r)
    }

    /// End every pointer gesture in the main window — its release can no longer
    /// arrive once the window hides or loses focus (a selection, a scrollbar
    /// drag, buttons the program saw pressed, the edge auto-scroll).
    fn reset_main_pointer(&mut self) {
        self.selecting = false;
        self.dragging_scrollbar = false;
        self.grid_mouse.reset();
    }

    /// A right click just went to a program that tracks the mouse: the first
    /// time, say how to get JeTTY's own menu instead.
    fn teach_shift_right_click(&mut self, window: winit::window::WindowId) {
        if !self.right_click_hint_shown {
            self.right_click_hint_shown = true;
            self.show_status_pill(crate::runsel::Notice {
                msg: "Shift+right-click opens JeTTY's menu",
                window: Some(window),
            });
        }
    }

    /// Run the due edge auto-scroll steps of selection drags in every window.
    /// Returns whether one repainted.
    fn service_grid_autoscroll(&mut self) -> bool {
        let now = std::time::Instant::now();
        let mut painted = false;
        if self.grid_mouse.autoscroll_due().is_some_and(|t| now >= t) {
            // A due step always advances (or ends) the schedule, so the merged
            // wake is never in the past — and with no tab to scroll it ends here.
            match self.with_main_grid(|g| crate::gridmouse::autoscroll_step(g, now)) {
                Some(true) => {
                    self.request_main_paint();
                    painted = true;
                }
                Some(false) => {}
                None => self.grid_mouse.reset(),
            }
        }
        let (ui_font, show_hud, mods) = (self.ui_font_logical, self.show_perf_hud, self.modifiers);
        for dw in &mut self.detached {
            if dw.grid_mouse.autoscroll_due().is_some_and(|t| now >= t) {
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                if with_detached_grid(dw, geom, mods, |g| crate::gridmouse::autoscroll_step(g, now)) {
                    dw.request_paint();
                    painted = true;
                }
            }
        }
        painted
    }

    /// The soonest edge auto-scroll step any window owes.
    fn grid_autoscroll_due(&self) -> Option<std::time::Instant> {
        std::iter::once(self.grid_mouse.autoscroll_due())
            .chain(self.detached.iter().map(|d| d.grid_mouse.autoscroll_due()))
            .flatten()
            .min()
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
            || self.ov.help_open
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
    /// window's own cursor/geometry (grid origin = its top bar, its own modal =
    /// the context menu).
    fn update_detached_link_hover(&mut self, pos: usize, force: bool) {
        let held = link_modifier_held(&self.modifiers);
        let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
        let Some(dw) = self.detached.get_mut(pos) else { return };
        let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
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
            && cy >= bar_h
            && cy < h - status_h;
        if !gated {
            dw.link_hover_cell = None;
            if dw.link_hover.take().is_some() {
                dw.window.set_cursor(dw.resize_zone.cursor_icon());
                dw.request_paint();
            }
            return;
        }
        let gy = (cy - bar_h).max(0.0);
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
    /// The wire bytes — control characters stripped (no `^C` can split a paste
    /// into typed commands, no ESC can forge the end marker), line breaks
    /// normalized — come from the pure, unit-tested `runsel::paste_bytes`.
    fn paste_to_tab(tab: &mut Tab, text: &str) {
        let bytes = crate::runsel::paste_bytes(text, tab.terminal.bracketed_paste());
        if bytes.is_empty() {
            return;
        }
        // A user paste claims the prompt — cancel any staged run-selection
        // inject for this tab (same rule as write_key_to_pty).
        crate::runsel::cancel_on_user_write(&mut tab.pending_inject);
        let _ = tab.writer.write_all(&bytes);
        let _ = tab.writer.flush();
    }

    /// Run the current selection in a NEW tab — the browser's "open link in a
    /// new tab" gesture, transplanted (v0.25). One method behind EVERY trigger:
    /// context menu, Ctrl+Shift+Enter, palette, copy-mode `r`, and the
    /// detached-window menu/chord.
    ///
    /// Pipeline: `selection_text()` → `runsel::sanitize` → `runsel::classify`
    /// → clear the SOURCE selection → `new_tab_with_cwd(source cwd)` → arm the
    /// NEW tab's `pending_inject` (fired by the drain hook once the shell is
    /// ready — see `runsel::poll_pending`). Ordering is load-bearing:
    /// * the source selection is cleared BEFORE the tab switch, or the clear
    ///   would hit the new tab and leave a stale highlight on the source;
    /// * the pending is armed only through `new_tab_with_cwd`'s returned index
    ///   — on spawn failure nothing is armed (never the source shell).
    ///
    /// From a detached source the tab opens in the MAIN window (the only tabbed
    /// window) at the DETACHED tab's cwd, without summoning or focusing it —
    /// the browser's "opened in a background tab"; Run & Notify pings on
    /// completion.
    fn run_selection_in_new_tab(&mut self, source: SelSource) {
        if !self.run_selection_enabled {
            return; // config opt-out: `run_selection = false`
        }
        // 1. Capture everything from the SOURCE tab.
        let (text, cwd, wait_for_mark, notify_window) = match source {
            SelSource::Main => {
                if self.tabs.is_empty() {
                    return;
                }
                let t = &self.tabs[self.active];
                (
                    t.terminal.selection_text(),
                    t.pty.cwd(),
                    t.terminal.prompt_count() > 0,
                    self.window.as_ref().map(|w| w.id()),
                )
            }
            SelSource::Detached(i) => {
                let Some(dw) = self.detached.get(i) else { return };
                (
                    dw.tab.terminal.selection_text(),
                    dw.tab.pty.cwd(),
                    dw.tab.terminal.prompt_count() > 0,
                    Some(dw.window.id()),
                )
            }
        };
        let Some(raw) = text else { return };
        let (text, run) = match crate::runsel::classify(crate::runsel::prepare(&raw)) {
            crate::runsel::Plan::Empty => return,
            crate::runsel::Plan::Run(t) => (t, true),
            crate::runsel::Plan::Type(t) => (t, false),
        };
        // 2. Clear the SOURCE selection now — `new_tab_with_cwd` switches
        //    `self.active`, and neither switch_tab nor select_tab clears an
        //    outgoing terminal's Selection (same highlight-lingering property
        //    menu-Copy handles by clearing).
        match source {
            SelSource::Main => {
                self.tabs[self.active].terminal.selection_clear();
                self.request_main_paint();
            }
            SelSource::Detached(i) => {
                if let Some(dw) = self.detached.get_mut(i) {
                    dw.tab.terminal.selection_clear();
                    dw.request_paint();
                }
            }
        }
        // 3. Create the destination tab and arm ITS pending (and only its).
        //    A detached-source run is the browser's BACKGROUND tab (sweep M2):
        //    spawned without activation, so a shown main window's grid — and its
        //    search bar / hint or copy mode — never flips under the user.
        //    Main-window triggers DO switch (the user fired the gesture there,
        //    and a staged multiline needs their review + Enter).
        let spawned = match source {
            SelSource::Main => self.new_tab_with_cwd(cwd),
            SelSource::Detached(_) => self.spawn_tab(cwd),
        };
        let Some(idx) = spawned else { return };
        self.tabs[idx].pending_inject = Some(crate::runsel::PendingInject {
            text,
            run,
            created: std::time::Instant::now(),
            wait_for_mark,
            notify_window,
        });
        // Wake the `about_to_wait` deadline fold (one bool; false when unused).
        self.runsel_active = true;
    }

    /// Surface a run-selection feedback pill in the window that hosted the
    /// trigger (falls back to the main window when the source window is gone).
    /// Reuses the shift-hint pill surface: themed, bottom-centered, ~4 s.
    fn show_status_pill(&mut self, n: crate::runsel::Notice) {
        let target = n
            .window
            .filter(|id| {
                self.window.as_ref().is_some_and(|w| w.id() == *id)
                    || self.detached.iter().any(|d| d.window.id() == *id)
            })
            .or_else(|| self.window.as_ref().map(|w| w.id()));
        let Some(id) = target else { return };
        self.status_pill =
            Some((n.msg.to_string(), std::time::Instant::now() + std::time::Duration::from_millis(4000), id));
        if self.window.as_ref().is_some_and(|w| w.id() == id) {
            self.request_main_paint();
        } else if let Some(dw) = self.detached.iter().find(|d| d.window.id() == id) {
            dw.request_paint();
        }
    }

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
        // Run-selection feedback pills produced by this drain pass (refusal /
        // staged). Collected locally (the loop holds `self.tabs` mutably) and
        // surfaced after it; empty on every normal pass.
        let mut runsel_notices: Vec<crate::runsel::Notice> = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let (had, title_changed, notice) = Self::drain_one_tab(tab, &mut vt_read);
            chrome_changed |= title_changed;
            if let Some(n) = notice {
                runsel_notices.push(n);
            }
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
        for n in runsel_notices {
            self.show_status_pill(n);
        }
        (active_had_data, chrome_changed, exited)
    }

    /// End-of-iteration re-arm of every tab's coalesced PTY wake (main and
    /// detached). A tab whose drain hit `PTY_DRAIN_BUDGET` — or that received
    /// output after its last drain — still has bytes queued: ONE Wake (for all
    /// of them) is sent so they drain in the NEXT iteration, after pending
    /// input; an idle tab's latch is cleared so its next output wakes us at
    /// once. Must run after the iteration's drains, i.e. from `about_to_wait`
    /// (see `PtySession::rearm_wake` for why not earlier).
    /// DECSET 1004 focus reporting, for every tab of every window: the main
    /// window's ACTIVE tab is focused while that window is shown and has OS
    /// focus; a detached window's tab while its window has OS focus. Each tab
    /// reports only when its state CHANGES (`TabInputState::focus_report`), so a
    /// hide sends `CSI O` once and a summon `CSI I` once whichever event moved it
    /// — and the X11 hotkey grab's FocusOut/FocusIn churn costs at most one
    /// `CSI O` + `CSI I` pair. Called from `about_to_wait` after every event
    /// batch, so no focus/visibility/tab-switch path needs its own hook; it only
    /// reads a mode bit per tab when nothing changed.
    fn sync_focus_reports(&mut self) {
        let main_focused = self.visible && self.main_focused;
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            report_focus(tab, main_focused && i == active);
        }
        for dw in &mut self.detached {
            report_focus(&mut dw.tab, dw.focused);
        }
    }

    /// Re-apply `macos_option_as_alt` to the main and every detached window
    /// (hot-reload); a no-op off macOS.
    fn apply_option_as_alt_everywhere(&self) {
        if let Some(w) = &self.window {
            apply_option_as_alt(w, self.macos_option_as_alt);
        }
        for dw in &self.detached {
            apply_option_as_alt(&dw.window, self.macos_option_as_alt);
        }
    }

    fn rearm_pty_wakes(&self) {
        let mut more = false;
        for tab in &self.tabs {
            more |= tab.pty.rearm_wake();
        }
        for dw in &self.detached {
            more |= dw.tab.pty.rearm_wake();
        }
        if more {
            let _ = self.proxy.send_event(AppEvent::Wake);
        }
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
    ///
    /// The third element is a run-selection feedback [`runsel::Notice`]
    /// (refusal/staged pill) — `None` on every normal pass; the caller
    /// surfaces it via `show_status_pill` (a pill needs `&mut self`).
    fn drain_one_tab(
        tab: &mut Tab,
        vt_read: &mut u64,
    ) -> (bool, bool, Option<crate::runsel::Notice>) {
        // Feed at most PTY_DRAIN_BUDGET bytes this pass so a flood can't starve
        // the event loop (see the const's doc). Whatever remains is scheduled
        // for the next loop iteration by `rearm_pty_wakes` in `about_to_wait`.
        let terminal = &mut tab.terminal;
        let fed = tab.pty.drain_output(PTY_DRAIN_BUDGET, |chunk| terminal.feed(chunk));
        *vt_read += fed as u64;
        let mut had = fed > 0;
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
            // OSC 52 names the selection: `p`/`s` → PRIMARY, `c` → clipboard.
            if tab.terminal.clipboard_store_is_primary() {
                crate::clipboard::set_primary(&text);
            } else {
                crate::clipboard::set(&text);
            }
        }
        // OSC 52 PASTE (load): a program asked to READ the clipboard. Only ever
        // present when the user enabled `osc52_allow_paste` (else alacritty denies it
        // and no request reaches us). Read the clipboard, CAP the reply length, format
        // via alacritty's supplied formatter, and write it back to the PTY.
        if let Some(fmt) = tab.terminal.take_clipboard_load() {
            let text = if tab.terminal.clipboard_load_is_primary() {
                crate::clipboard::get_primary()
            } else {
                crate::clipboard::get()
            };
            if let Some(mut text) = text {
                if text.len() > jetty_core::OSC52_MAX_BYTES {
                    text.truncate(floor_char_boundary(&text, jetty_core::OSC52_MAX_BYTES));
                }
                let reply = fmt(&text);
                let _ = tab.writer.write_all(reply.as_bytes());
                let _ = tab.writer.flush();
                had = true;
            }
        }
        // Run-selection pending inject: poll readiness against the bytes just
        // fed (prompt mark + bracketed paste) and fire/drop. The ONLY new
        // branch on the drain path — `pending_inject` is `None` forever for
        // tabs that never use the feature, so this is one false `is_some()`
        // per drain, same pattern as the take_* hooks above.
        let mut notice = None;
        if tab.pending_inject.is_some() {
            let (wrote, n) = Self::service_pending_inject(tab);
            had |= wrote;
            notice = n;
        }
        (had, title_changed, notice)
    }

    /// Poll + service one tab's pending inject (the drain-hook body and the
    /// `about_to_wait` deadline service share this). Static — it touches only
    /// the `Tab`; the caller surfaces the returned [`runsel::Notice`] (a pill
    /// needs `&mut self`). Returns `(wrote, notice)`.
    ///
    /// The pending is TAKEN before the write, so the injection can never trip
    /// its own user-write cancel; and both fire paths go through
    /// `runsel::fire_pending`, whose byte format is pinned by unit tests.
    fn service_pending_inject(tab: &mut Tab) -> (bool, Option<crate::runsel::Notice>) {
        use crate::runsel::{self, Verdict};
        let Some(p) = tab.pending_inject.as_ref() else {
            return (false, None);
        };
        let verdict = runsel::poll_pending(
            p,
            std::time::Instant::now(),
            tab.terminal.prompt_count(),
            tab.terminal.bracketed_paste(),
        );
        if verdict == Verdict::Wait {
            return (false, None);
        }
        let p = tab.pending_inject.take().expect("checked Some above");
        match verdict {
            Verdict::Fire | Verdict::FireUnbracketed => {
                let wrote = runsel::fire_pending(
                    &mut tab.writer,
                    &p.text,
                    p.run,
                    verdict == Verdict::Fire,
                )
                .unwrap_or(false);
                // Type mode landed staged (multiline / truncated): say so — the
                // user's Enter is the run confirmation.
                let notice = (wrote && !p.run).then_some(runsel::Notice {
                    msg: runsel::MSG_STAGED,
                    window: p.notify_window,
                });
                (wrote, notice)
            }
            Verdict::Drop => (
                // Tell the user (safety MINOR-2): the tab is open but nothing
                // ran — without the pill an empty tab is a silent mystery.
                false,
                Some(runsel::Notice { msg: runsel::MSG_DROPPED, window: p.notify_window }),
            ),
            Verdict::Refuse => (
                false,
                Some(runsel::Notice { msg: runsel::MSG_REFUSED, window: p.notify_window }),
            ),
            Verdict::Wait => unreachable!("early-returned above"),
        }
    }

    /// `about_to_wait`'s half of the pending-inject machinery: service every
    /// pending whose deadline elapsed (a silent shell produces no drain, so
    /// the timeout-fire / TTL-drop need this wake), and recompute
    /// `runsel_active` from what actually remains — self-healing after
    /// user-write cancels, which happen in static funnels that can't touch
    /// App state. Runs ONLY while `runsel_active`; O(tabs) then, zero otherwise.
    fn service_runsel_deadlines(&mut self) {
        let now = std::time::Instant::now();
        let mut notices: Vec<crate::runsel::Notice> = Vec::new();
        let mut any_left = false;
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.detached.iter_mut().map(|d| &mut d.tab))
        {
            if let Some(p) = &tab.pending_inject {
                if p.deadline() <= now {
                    let (_wrote, notice) = Self::service_pending_inject(tab);
                    if let Some(n) = notice {
                        notices.push(n);
                    }
                    // A timeout-fire's echo arrives via the PTY (Wake → drain →
                    // repaint), so no explicit redraw is needed for `_wrote`.
                }
            }
            any_left |= tab.pending_inject.is_some();
        }
        self.runsel_active = any_left;
        for n in notices {
            self.show_status_pill(n);
        }
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
        // the snapshot every LATER completion in the same drain batch would see
        // watching==true and be gated out, and the user would land on the wrong tab.
        let main_watching = self.main_user_watching();
        // …and which tab is ON SCREEN in it (snapshotted with it: a summon later
        // in this batch switches tabs). Only that tab is "watched" — a background
        // tab of a focused window still notifies (`main_tab_watched`).
        let active = self.active;
        // First failure wins the summon; else the last firing tab.
        let mut summon_target: Option<usize> = None;
        let mut summon_is_failure = false;
        for i in 0..self.tabs.len() {
            let completions = self.tabs[i].terminal.take_completions();
            if enabled {
                for c in completions {
                    let watching = main_tab_watched(main_watching, i, active);
                    if let Some(failed) = self.maybe_notify_main(i, c, watching) {
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
    /// whether THIS tab is on screen per the batch-start snapshot
    /// (`main_tab_watched` over `main_user_watching()` + the active tab, so an
    /// auto-summon earlier in the same drain can't suppress this tab). Returns
    /// `Some(failed)` when a notification fired (the caller decides the single
    /// batch auto-summon), or `None` when gated out.
    fn maybe_notify_main(
        &mut self,
        tab: usize,
        c: jetty_core::CommandCompletion,
        watching: bool,
    ) -> Option<bool> {
        // Throttled per TAB (its stable id), not per position: closing an
        // earlier tab must not hand this tab's throttle to its neighbour.
        let key = self.tabs.get(tab).map(|t| NotifyKey::MainTab(t.id))?;
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
        // Skipped while the window holds focus (a BACKGROUND tab of a focused
        // window now notifies): the user is already here, and an X11 urgency hint
        // set on the focused window would stay latched — it is only cleared on
        // the next Focused(true), which never comes for an already-focused window.
        if !(self.visible && self.main_focused) {
            if let Some(w) = &self.window {
                w.request_user_attention(Some(attention_for(failed)));
            }
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
        // Frame time: this (main) window's own smoothed dt.
        smooth_frame_ms(&mut self.perf_ms, &mut self.last_frame_at, now);
        self.refresh_perf_shared(now);
        Some(perf_hud_text(self.perf_ms, self.perf_cpu, self.perf_mb))
    }

    /// The HUD line for detached window `pos`, mirroring the main window's
    /// two render modes: an ACTIVE frame shows THIS window's own smoothed frame
    /// time (plus the shared process CPU% / VT MB/s) and re-arms its one-shot
    /// idle repaint; once that deadline passes with no other frame, the single
    /// repaint `about_to_wait` owes it shows the honest "idle" reading. Before,
    /// every detached strip echoed the MAIN window's cached string, which froze
    /// while the main window was hidden.
    fn detached_perf_label(&mut self, pos: usize) -> Option<String> {
        if !self.show_perf_hud {
            return None;
        }
        let now = std::time::Instant::now();
        let dw = self.detached.get_mut(pos)?;
        if !dw.perf_idle_shown && dw.perf_idle_at.is_some_and(|d| now >= d) {
            dw.perf_idle_shown = true;
            return Some(PERF_IDLE_TEXT.to_string());
        }
        smooth_frame_ms(&mut dw.perf_ms, &mut dw.last_frame_at, now);
        dw.perf_idle_at = Some(now + PERF_IDLE_AFTER);
        dw.perf_idle_shown = false;
        let ms = dw.perf_ms;
        self.refresh_perf_shared(now);
        Some(perf_hud_text(ms, self.perf_cpu, self.perf_mb))
    }

    /// Refresh the HUD's PROCESS-WIDE readings — CPU% of this process and VT
    /// throughput — whichever window is rendering, so a detached window's strip
    /// stays live while the main window is hidden. Both are self-throttled.
    fn refresh_perf_shared(&mut self, now: std::time::Instant) {
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
        let active_removed = exited.contains(&self.active);
        if active_removed {
            // The searched (active) tab's shell exited: close the bar before
            // the removals below retarget it (same invariant as close_tab).
            self.search_close(Surface::Main);
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
        }
        self.drop_stale_tab_refs();
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
        if active_removed {
            // A different tab is active now (same reset as a tab switch).
            self.entered_new_active_tab();
        } else {
            self.request_main_paint();
        }
        true
    }

    /// Shared reflow path: compute cols/rows from the current GPU surface size
    /// and the current TextLayer cell size, then resize the terminal and PTY.
    ///
    /// Called from both `WindowEvent::Resized` and `set_font_size` so both
    /// features share one code path.
    fn reflow(&mut self) {
        let status_h = self.status_h();
        let bar_h = self.bar_h();
        let (Some(gpu), Some(text)) = (&self.gpu, &self.text) else { return };
        let (cw, ch) = text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let w = gpu.config.width;
        let h = gpu.config.height;
        let cols = ((w as f32 - SCROLLBAR_GUTTER) / cw).floor().max(2.0) as usize;
        // The grid occupies the area below the tab bar and above the status bar.
        let rows = ((h as f32 - bar_h - status_h) / ch).floor().max(1.0) as usize;
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
        // Every surface: the Settings panel's size readout too (Ctrl+= while it
        // is open used to leave it stale).
        self.mark_dirty_all();
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
        self.mark_dirty_all();
    }

    /// Change the UI (chrome) font SIZE at runtime, clamped [10, 28]. Resizes the
    /// chrome + settings text layers IN-PLACE (reusing their FontSystems — never
    /// `new_with_family`, which would rescan fontconfig ~20ms). The settings panel
    /// body text is CAPPED to [13, 17] so the absolute-px panel layout never
    /// overflows, while the rest of the chrome (and the live "Aa" specimen) tracks
    /// the true size. The tab bar and status strip are sized by the UI font
    /// (`ChromeMetrics`), so a size change also changes how many grid ROWS fit:
    /// every window's grid/PTY reflow is armed on the same debounced
    /// `reflow_pending_at` path as a window resize (one coalesced SIGWINCH for a
    /// burst of steps — the p10k-scatter guard).
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
        // window's own scale, and re-grid them: their title bar / status strip
        // follow the UI font too (debounced like a border drag).
        let ui_logical = self.ui_font_logical;
        let reflow_at = std::time::Instant::now() + std::time::Duration::from_millis(120);
        for dw in &mut self.detached {
            let dscale = dw.window.scale_factor() as f32;
            dw.chrome_text.set_font_size(ui_logical * dscale);
            dw.reflow_pending_at = Some(reflow_at);
            dw.request_paint();
        }
        // The main window's bar/strip heights changed → its row count did too.
        self.reflow_pending_at = Some(reflow_at);
        self.persist();
        self.request_main_paint();
        // A different UI size grows/shrinks the panel — re-fit the window so the
        // bottom rows are never clipped (then live-preview the change).
        self.resize_settings_to_fit();
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
        // A wider/narrower UI family changes the panel's scaled width — re-fit.
        self.resize_settings_to_fit();
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
        // Rule F0: drop the OS fullscreen state while the window is still MAPPED,
        // BEFORE `set_visible(false)` below.
        let was_fullscreen = self.exit_main_fullscreen_bare();
        if let Some(win) = &self.window {
            // Never overwrite `last_pos` when we were fullscreen: `outer_position()`
            // on a fullscreen (or just-exited) window reports the monitor origin,
            // which would poison the user's remembered spot to the screen corner —
            // and `set_main_fullscreen(true)` already saved the real pre-fullscreen
            // position on the way in.
            if self.window_mode == WindowMode::Center && !was_fullscreen {
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
        // Stamp the auto-hide: if this FocusOut was the summon hotkey's own key
        // grab and the hotkey event is merely late, that toggle must not re-show
        // the window (see `toggle_action`).
        self.autohidden_at = Some(std::time::Instant::now());
        // Save a still-debounced settings change now (non-blocking).
        self.persister.borrow_mut().flush();
        // The matching button-release never arrives once hidden — end the
        // pointer gestures so none resumes stuck on the next summon.
        self.reset_main_pointer();
        // Clear the remaining self-drive terms whose ONLY expiry point is inside
        // RedrawRequested — which a hidden (orderOut) window never receives on
        // macOS — so they can't pin about_to_wait in Poll and spin 100% CPU while
        // hidden (F18). Re-armed naturally on the next keystroke/summon.
        self.caret_anim = None;
        self.pending_dock_frames = 0;
        self.pending_center_frames = 0;
        // Paints owed to a now-invisible window: drop them (the next summon
        // paints anyway). The idle-HUD one-shot in particular must never be
        // requested for a hidden window (see `perf_idle_decision`); the first
        // frame after the summon re-arms it.
        self.disarm_hidden_paints();
        // …and the debounced grid+PTY reflow. Rule F0's exit-fullscreen-while-
        // still-mapped shrinks the frame just above, so a `Resized` may have armed
        // the 250 ms deadline; `reflow_due` is not gated on `self.visible`, so it
        // would SIGWINCH every tab to a grid the user never sees. The next summon
        // re-arms it once, at the final geometry.
        let (pending, deferred) =
            reflow_terms_on_hide(self.reflow_pending_at, self.reflow_deferred_by_hide);
        self.reflow_pending_at = pending;
        self.reflow_deferred_by_hide = deferred;
    }

    /// Drop every paint owed to the main window that only makes sense while it
    /// is on screen — shared by both hide paths. Each one is re-armed by the
    /// first frame after the next summon, so nothing is lost; leaving them armed
    /// while hidden made `about_to_wait` request redraws for an unmapped window.
    fn disarm_hidden_paints(&mut self) {
        self.perf_idle_at = None;
        self.perf_idle_shown = false;
        self.key_paint_due = None;
        self.acquire_retry = None;
        self.raise_attempt_at = None;
    }

    /// Toggle window visibility (F9 / Yakuake-style summon / `jetty --toggle`).
    ///
    /// Hidden → summon. Shown AND in front of the user (focused — counting the
    /// hotkey grab's own momentary FocusOut as focused — and not occluded) →
    /// hide. Shown but NOT in front (behind other windows, or unfocused after the
    /// user clicked elsewhere with `focus_autohide = false`) → raise + focus
    /// instead of hiding it; if the compositor refuses that raise, the next press
    /// hides (see `toggle_action`). The PTY keeps running while the window is
    /// hidden — nothing is killed or suspended.
    fn toggle_visibility(&mut self, event_loop: &ActiveEventLoop) {
        let now = std::time::Instant::now();
        match toggle_action(
            self.visible,
            self.main_focused,
            self.main_occluded,
            self.focus_lost_at,
            self.autohidden_at,
            self.raise_attempt_at,
            now,
        ) {
            ToggleAction::Show => self.set_visibility(true, event_loop),
            ToggleAction::Hide => self.set_visibility(false, event_loop),
            ToggleAction::Raise => {
                self.raise_attempt_at = Some(now);
                // A minimized (iconified) window is "visible" but off-screen:
                // restore it first (a no-op when it isn't minimized).
                if let Some(w) = &self.window {
                    w.set_minimized(false);
                }
                // The already-visible branch of `set_visibility(true)` is exactly
                // a raise: it cancels a scheduled auto-hide, focuses and repaints.
                self.set_visibility(true, event_loop);
            }
        }
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
        // Rule F0: on the HIDE leg, drop the OS fullscreen state while the window is
        // still MAPPED (before `set_visible(false)` below). Computed here, ahead of
        // the `&self.window` borrow, because it needs `&mut self`.
        let was_fullscreen = if self.visible {
            false
        } else {
            self.exit_main_fullscreen_bare()
        };
        // A summon into Fullscreen mode enters fullscreen INLINE below (rule F0
        // wants `set_visible(true)` first), bypassing `set_main_fullscreen` — so
        // the single-fullscreen-window rule is applied here instead, ahead of the
        // `&self.window` borrow that follows.
        if self.visible && mode == WindowMode::Fullscreen {
            // Same two sibling-window rules as `set_main_fullscreen(true)`, applied
            // here because the summon enters fullscreen INLINE below (rule F0 wants
            // `set_visible(true)` first) — and both need `&mut self`, so they must
            // run ahead of the `&self.window` borrow that follows.
            self.close_settings_for_fullscreen();
            // Off macOS this is now a NO-OP, which is the point: a plain F9 must
            // never drag a detached window on another monitor out of fullscreen.
            // On macOS it stays load-bearing — a summon that entered fullscreen
            // alongside an already-fullscreen detached window would NEST the
            // app-scoped `presentationOptions` (see `enforce_single_fullscreen`).
            self.enforce_single_fullscreen(None);
        }
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
                    WindowMode::Fullscreen => {
                        // Show FIRST so the window is mapped: X11 resolves
                        // `Borderless(None)` from the window frame and macOS's
                        // simple fullscreen `expect`s a screen, neither of which an
                        // unmapped window has (rule F0). Because the hide leg exited
                        // fullscreen, this is always a genuine `None → Some`
                        // transition — winit's X11 `set_fullscreen_inner` early-
                        // returns when the requested state equals the cached one, so
                        // holding the state across a hide would silently lose the WM
                        // state and make the summon a no-op.
                        //
                        // HONESTLY, on X11 this enter is still DEFERRED: `set_visible`
                        // leaves winit's visibility at `YesWait` until a
                        // `VisibilityNotify` arrives, so `set_fullscreen_inner` stores
                        // the request in `desired_fullscreen` and replays it post-map.
                        // The window therefore maps at its WINDOWED geometry and
                        // expands a moment later — a brief windowed→fullscreen flash
                        // on every summon. Harmless (the deferral returns before the
                        // cached state is assigned, so the replay is still a genuine
                        // `None → Some`, and `main_fullscreen` is already true so the
                        // corners are flat for those frames), but the blueprint's
                        // "nothing is deferred here" claim was simply wrong.
                        win.set_visible(true);
                        // Same capture as `set_main_fullscreen(true)` — this inline
                        // enter used to bypass it, which left an F11 escape (and a
                        // Settings switch back to Center) with no position AND no
                        // size to restore, so the exit centred from the stale
                        // monitor-sized `outer_size()` and landed in the corner.
                        capture_pre_fullscreen(
                            win,
                            mode,
                            &mut self.last_pos,
                            &mut self.last_windowed_size,
                        );
                        self.main_fullscreen = true;
                        jetty_platform::set_window_fullscreen(win, true);
                        // NO Center/Dropdown geometry, NO dock/center counters, NO
                        // slide: the summon reveal effect (`summon_pending` below) is
                        // this mode's appearance animation, and it is
                        // resolution-independent.
                        self.pending_center_pos = None;
                        self.pending_center_frames = 0;
                        self.pending_dock_frames = 0;
                    }
                }
                // Pay back a reflow the hide leg dropped (see
                // `reflow_deferred_by_hide`) at THIS geometry. When the summon's own
                // geometry change produces a `Resized`, that overwrites this
                // deadline with its own — so a summon always costs exactly one
                // debounced reflow, never two and never zero.
                let (pending, deferred) = reflow_terms_on_summon(
                    self.reflow_pending_at,
                    self.reflow_deferred_by_hide,
                    std::time::Instant::now()
                        + std::time::Duration::from_millis(REFLOW_DEBOUNCE_MS),
                );
                self.reflow_pending_at = pending;
                self.reflow_deferred_by_hide = deferred;
                win.focus_window();
                // Crystallize/reveal on every summon (F9 show), mirroring first open.
                // Start the clock on the FIRST real frame (summon_pending), not here:
                // on macOS the window can take a beat to present, which would
                // otherwise let the whole effect elapse unseen (effectless).
                self.summon_pending = true;
                self.summon_settle_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
                // The summon may have placed the window anywhere: re-derive the
                // Dropdown top-flush once; a fresh show is not a "raise", and any
                // earlier auto-hide no longer matters.
                self.top_flush_dirty = true;
                self.raise_attempt_at = None;
                self.autohidden_at = None;
                self.request_main_paint();
            } else {
                // Remember the current spot before hiding so the next Center
                // summon restores it. Dropdown re-docks, so last_pos is unused.
                // Skipped when we just left fullscreen: `outer_position()` reports
                // the monitor origin there, and the real pre-fullscreen spot was
                // already saved on the way IN (see `set_main_fullscreen`).
                if mode == WindowMode::Center && !was_fullscreen {
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
                // Save a still-debounced settings change now (non-blocking).
                self.persister.borrow_mut().flush();
                // The matching button-release never arrives once hidden — end
                // the pointer gestures so none resumes stuck on the next summon
                // (mirrors autohide_main_window; the F9/IPC hide path reaches
                // here too).
                self.reset_main_pointer();
                // Clear the self-drive terms whose only expiry is in
                // RedrawRequested (never delivered to a hidden macOS window) so
                // they don't pin Poll and spin 100% CPU while hidden (F18).
                self.caret_anim = None;
                self.pending_dock_frames = 0;
                self.pending_center_frames = 0;
                // Paints owed to a now-invisible window (idle HUD, keystroke
                // fallback, acquire retry, a pending raise) — same as the
                // focus-loss hide path.
                self.disarm_hidden_paints();
                // …and the debounced reflow (see `reflow_deferred_by_hide`): rule
                // F0 leaves fullscreen while still mapped just above, so the
                // deadline may be live — and it would fire 250 ms later, hidden.
                let (pending, deferred) =
                    reflow_terms_on_hide(self.reflow_pending_at, self.reflow_deferred_by_hide);
                self.reflow_pending_at = pending;
                self.reflow_deferred_by_hide = deferred;
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

        // A fullscreen window sits in the WM's above-normal layer (EWMH), and
        // whether a WM demotes it on focus loss is WM-specific — which we may not
        // special-case. So a Settings window opened over a fullscreen terminal would
        // be focused but INVISIBLE behind it, and Settings is the only UI for leaving
        // Fullscreen mode. Give the REQUESTING window back first (coherent with "▢
        // while fullscreen = give me my window back"): the chord/gear was used in
        // the focused window, so a fullscreen detached window leaves fullscreen,
        // and the main window is never yanked out of fullscreen by a request that
        // came from a detached window.
        let focused_detached = self
            .detached
            .iter()
            .position(|d| d.focused)
            .map(|p| (p, self.detached[p].fullscreen));
        match settings_fullscreen_exit(self.main_fullscreen, focused_detached) {
            FullscreenExit::Main => self.set_main_fullscreen(false),
            FullscreenExit::Detached(pos) => self.set_detached_fullscreen(pos, false),
            FullscreenExit::None => {}
        }

        let window = match jetty_platform::build_fixed_window(
            event_loop,
            "JeTTY — Settings",
            self.desired_settings_logical_size(),
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
        self.build_settings_stack(&window);
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

    /// (Re)build the Settings window's GPU stack — surface, panel text, specimen
    /// text, quads — for `window`: on open, and after a GPU device loss. Only a
    /// new SURFACE on the main window's device (no adapter enumeration or device
    /// creation on the UI thread), and fonts from the already-loaded font
    /// database (no fontconfig rescan) — opening Settings used to stall every
    /// window and the PTY drain for both. `settings_gpu` is `None` (and the
    /// window simply stays blank) when no GPU can present to it.
    fn build_settings_stack(&mut self, window: &Arc<Window>) {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let shared = self.gpu.as_ref().map(|g| g.shared());
        let gpu = GpuContext::new_sharing(shared.as_ref(), window.clone(), size.width, size.height);
        let fonts = || self.text.as_ref().map_or_else(TextLayer::build_font_system, |t| t.clone_font_system());
        if let Some(ref g) = gpu {
            // The settings panel body text renders at the CAPPED UI size ([13,17])
            // so the absolute-px panel layout never overflows its fixed window,
            // independent of the terminal font. The chosen UI family is applied via
            // set_ui_family (no rescan). The true UI size is used only for the live
            // "Aa" specimen, drawn separately via chrome_text.
            let capped = self.ui_font_logical.clamp(PANEL_TEXT_MIN, PANEL_TEXT_MAX);
            let mut text = TextLayer::new_with_family_and_fonts(
                &g.device, &g.queue, g.format, capped * scale, &self.font_family, fonts(),
            );
            let ui_fam = if self.ui_font_family.is_empty() {
                None
            } else {
                Some(self.ui_font_family.as_str())
            };
            text.set_ui_family(ui_fam);
            // Dedicated TRUE-size specimen layer on the settings device for the
            // live "Aa" preview (the panel body text above is capped).
            let mut specimen = TextLayer::new_with_family_and_fonts(
                &g.device, &g.queue, g.format, self.ui_font_logical * scale, &self.font_family, fonts(),
            );
            specimen.set_ui_family(ui_fam);
            let quad = QuadLayer::new(&g.device, g.format);
            self.settings_text = Some(text);
            self.settings_specimen_text = Some(specimen);
            self.settings_quad = Some(quad);
        }
        self.settings_gpu = gpu;
    }

    /// Re-read the main window's display refresh interval (flood pacing). Cheap:
    /// called on create, move and DPI change, never per frame.
    fn refresh_frame_interval(&mut self) {
        let mhz = self
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .and_then(|m| m.refresh_rate_millihertz());
        self.frame_interval = refresh_interval(mhz);
    }

    /// GPU device-loss recovery. A lost device (driver reset, GPU hang, some
    /// suspend/resume paths) never presents again — every JeTTY window stayed
    /// frozen on its last frame until a restart. Rebuild the main window's GPU
    /// stack from a fresh device, then re-surface every detached window and the
    /// Settings window whose device was lost (they share the main one). When no
    /// GPU can be acquired yet (the driver still resetting) the attempt repeats
    /// after `GPU_REBUILD_RETRY`, as a single `WaitUntil` wake — never a spin.
    /// Returns whether a rebuild was attempted (the caller repaints).
    fn recover_lost_gpu(&mut self, now: std::time::Instant) -> bool {
        let main_lost = self.gpu.as_ref().is_some_and(|g| g.is_lost());
        let detached_lost = self.detached.iter().any(|d| d.gpu.is_lost());
        let settings_lost = self.settings_gpu.as_ref().is_some_and(|g| g.is_lost());
        if !gpu_recovery_due(main_lost || detached_lost || settings_lost, self.gpu_rebuild_retry_at, now) {
            return false;
        }
        let mut ok = true;
        if main_lost {
            ok &= self.rebuild_main_gpu();
        }
        // Everything else re-surfaces on the (possibly new) main device.
        let shared = self.gpu.as_ref().filter(|g| !g.is_lost()).map(|g| g.shared());
        let (font_logical, ui_font_logical) = (self.font_logical, self.ui_font_logical);
        for dw in &mut self.detached {
            if dw.gpu.is_lost() {
                ok &= dw.rebuild_gpu(
                    shared.as_ref(),
                    font_logical,
                    ui_font_logical,
                    &self.font_family,
                    &self.ui_font_family,
                );
            }
        }
        if settings_lost {
            if let Some(win) = self.settings_window.clone() {
                self.build_settings_stack(&win);
                self.settings_acquire_retry = None;
                ok &= self.settings_gpu.as_ref().is_some_and(|g| !g.is_lost());
            }
        }
        self.gpu_rebuild_retry_at = if ok { None } else { Some(now + GPU_REBUILD_RETRY) };
        self.mark_dirty_all();
        true
    }

    /// Rebuild the MAIN window's whole GPU stack on a fresh device (see
    /// `recover_lost_gpu`): the same layers `resumed` builds, at the current font
    /// sizes/families, fonts from the already-loaded font database (no rescan).
    /// Tabs, grids and every UI state are untouched. `false` (keeping the lost
    /// stack for a later retry) when no GPU can present to the window yet.
    fn rebuild_main_gpu(&mut self) -> bool {
        let Some(window) = self.window.clone() else { return false };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let Some(gpu) = GpuContext::new(window, size.width, size.height) else { return false };
        let font_db = || self.text.as_ref().map_or_else(TextLayer::build_font_system, |t| t.clone_font_system());
        let (grid_fonts, chrome_fonts) = (font_db(), font_db());
        let text = TextLayer::new_with_family_and_fonts(
            &gpu.device, &gpu.queue, gpu.format, self.font_logical * scale, &self.font_family, grid_fonts,
        );
        let mut chrome = TextLayer::new_with_family_and_fonts(
            &gpu.device, &gpu.queue, gpu.format, self.ui_font_logical * scale, &self.font_family, chrome_fonts,
        );
        chrome.set_ui_family(if self.ui_font_family.is_empty() {
            None
        } else {
            Some(self.ui_font_family.as_str())
        });
        let (device, format) = (&gpu.device, gpu.format);
        self.quad = Some(QuadLayer::new(device, format));
        self.corner_mask = Some(jetty_render::CornerMask::new(device, format));
        self.bayer_reveal = Some(jetty_render::BayerReveal::new(device, format));
        self.phosphor = Some(jetty_render::PhosphorIgnition::new(device, format));
        self.liquid = Some(jetty_render::LiquidDrop::new(device, format));
        self.focus = Some(jetty_render::FocusPull::new(device, format));
        self.crt = Some(jetty_render::Crt::new(device, format));
        self.image_layer = Some(jetty_render::ImageLayer::new(device, format));
        self.caret_fx = Some(jetty_render::CaretFx::new(device, format));
        // Lazily re-allocated on the next frame that needs it, on the new device.
        self.offscreen = None;
        self.text = Some(text);
        self.chrome_text = Some(chrome);
        self.gpu = Some(gpu);
        self.acquire_retry = None;
        true
    }

    /// End every Settings-window drag (opacity, radius, dropdown size, Effects
    /// sliders) and make its value stick: persist it, and re-dock a dropdown-size
    /// drag — applied on release only, never per move (an X11 resize storm),
    /// re-asserted post-map via `pending_dock_frames`, inert while fullscreen
    /// (`dock_reassert_ok`). The button release, focus loss (the release never
    /// arrives) and the window closing all end drags here; the last two used to
    /// drop the latches WITHOUT persisting, so the live-applied value was lost
    /// on the next start.
    fn end_settings_drags(&mut self) {
        let end = settings_drag_end(
            self.dragging_slider || self.dragging_radius || self.active_fx_drag.is_some(),
            self.dragging_dropdown || self.dragging_dropdown_width,
        );
        if end.persist {
            self.persist();
        }
        if end.redock && self.visible && dock_reassert_ok(self.window_mode, self.main_fullscreen) {
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

    /// Drop the settings window and its render stack (closes/hides the OS window).
    fn close_settings_window(&mut self) {
        // A drag in progress when the window closes ends here: persist its
        // live-applied value (it never gets a release) and clear every latch so
        // nothing misbehaves on reopen.
        self.end_settings_drags();
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
    /// `&mut self` only to MEASURE labels with the settings text layer (the same
    /// cached measurement `render_settings_window` uses, so hit-rects match the
    /// drawn panel exactly).
    fn settings_panel_view(&mut self, w: u32, h: u32) -> jetty_render::PanelView {
        let theme = self.current_theme();
        let shell_display = self.shell_display();
        let cm = self.settings_metrics();
        let mut fallback = mono_fallback(cm);
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
            self.window_mode == WindowMode::Dropdown,
            corner_radius_band_dimmed(self.main_fullscreen, self.window_mode),
            self.focus_autohide,
            self.launch_at_login,
            self.ui_font_logical, &self.ui_font_families, &self.ui_font_family,
            self.ui_font_scroll_offset,
            0.0, 0.0, &theme,
            measure_or(self.settings_text.as_mut(), &mut fallback),
            cm,
            &shell_display,
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
        // Dims the CORNER RADIUS band while fullscreen (the radius is suppressed at
        // display time there, so the slider has no visible effect). Must stay
        // identical to the hit-test view's input in `build_settings_panel`.
        let fullscreen = corner_radius_band_dimmed(self.main_fullscreen, self.window_mode);
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
        // The panel's layout scale: the settings window's DPI × the CAPPED panel
        // text size. Labels are measured with the settings layer that draws them.
        let cm = self.settings_metrics();
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
            window_mode_name, tab_bar_name, &scrollback_name, dropdown_height_pct, dropdown_width_pct, is_dropdown, fullscreen, focus_autohide,
            launch_at_login,
            ui_font_logical, &ui_families, &ui_family, ui_font_scroll_offset,
            0.0, 0.0, &theme, &mut *text, cm,
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
            // The swapchain is healthy again: drop any retry schedule.
            self.settings_acquire_retry = None;
            // Missed-paint proof counter (JETTY_FRAME_LOG only).
            if self.frame_log {
                self.frames_presented += 1;
                eprintln!("JETTY_FRAME {} settings", self.frames_presented);
            }
        } else {
            // Acquire failed: this frame's change was not shown. Same bounded
            // retry schedule as the main/detached windows (`about_to_wait`
            // issues + advances it) — the panel must not stay stale.
            self.settings_acquire_retry
                .get_or_insert_with(|| next_acquire_retry(None, std::time::Instant::now()));
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
                // --- THIS window's overlays own the keyboard first, in the main
                // window's order: command palette, hint mode, copy-mode, then the
                // help (Esc + scroll keys) and the scrollback-search bar ---
                let s = Surface::Detached(pos);
                if self.overlay_key_modal(s, &event, event_loop) || self.overlay_key_bars(s, &event) {
                    return;
                }
                // The SAME key decision as the main window (`decide_window_key`),
                // against THIS window's own terminal. macOS Cmd chords are folded
                // into the keymap and dispatched below through the same action
                // path as the main window; the keymap lookup keeps the "swallow
                // unmapped Cmd" safety net. Cmd+Q stays a detached no-op
                // (guarded below).
                let action = {
                    let Some(dw) = self.detached.get(pos) else { return };
                    decide_window_key(
                        &self.keymap,
                        &event,
                        &self.key_modifiers,
                        &dw.tab.terminal,
                        self.macos_option_as_alt,
                    )
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
                    // New tab / tab switches act on the MAIN window (the only
                    // tabbed one): bring it up so the user sees the result — summon
                    // it when hidden, raise + focus it when it is merely behind —
                    // instead of silently changing a window they can't see.
                    input::KeyAction::NewTab => {
                        // Inherit THIS detached tab's cwd; the new tab opens in
                        // the main window.
                        let cwd = self.detached.get(pos).and_then(|dw| dw.tab.pty.cwd());
                        if self.new_tab_with_cwd(cwd).is_some() {
                            self.set_visibility(true, event_loop);
                        }
                        return;
                    }
                    input::KeyAction::NextTab | input::KeyAction::PrevTab => {
                        if !self.tabs.is_empty() {
                            self.switch_tab(action == input::KeyAction::NextTab);
                            self.set_visibility(true, event_loop);
                        }
                        return;
                    }
                    input::KeyAction::SelectTab(n) => {
                        if !self.tabs.is_empty() {
                            self.select_tab(*n);
                            self.set_visibility(true, event_loop);
                        }
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
                    // Scrollback search, hint mode, keyboard copy-mode and the
                    // command palette open in THIS window, on its own terminal —
                    // exactly like the main window's.
                    input::KeyAction::SearchToggle => {
                        self.search_toggle(s);
                        return;
                    }
                    input::KeyAction::HintMode => {
                        self.enter_hint_mode(s);
                        return;
                    }
                    input::KeyAction::CopyMode => {
                        self.enter_copy_mode(s);
                        return;
                    }
                    input::KeyAction::OpenPalette => {
                        self.open_palette(s);
                        return;
                    }
                    // Run-selection from a DETACHED window: this window's own
                    // selection + cwd; the new tab opens in the MAIN window
                    // (not summoned, focus stays here — the browser's "opened
                    // in a background tab").
                    input::KeyAction::RunSelection => {
                        self.run_selection_in_new_tab(SelSource::Detached(pos));
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
                    // F11 (macOS also Cmd+Ctrl+F) toggles fullscreen on THIS
                    // window. Same shape as the DetachTab arm above: the `dw`
                    // borrow is dead in this arm, so calling back into `self` is
                    // fine.
                    input::KeyAction::ToggleFullscreen => {
                        self.toggle_fullscreen_detached(pos);
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
                            dw.menu_disabled.clear();
                            dw.request_paint();
                            return;
                        }
                        // Snap this window's view to the live bottom (else typing
                        // while scrolled up goes blind, F30) then write to the PTY
                        // — the shared input core, same as the main Send arm
                        // (v0.23 Task 9).
                        write_key_to_pty(&mut dw.tab, &bytes, Some(event.physical_key));
                        // No paint here — the echo paints (same rule and fallback
                        // deadline as the main window's Send arm).
                        let now = std::time::Instant::now();
                        dw.key_paint_due = Some(now + KEY_ECHO_GRACE);
                        // Caret flash on printable keystrokes — same trigger as
                        // the main window, on THIS window's own burst clock. Glow
                        // is main-window-only (its CaretFx pass isn't replicated
                        // per-window), so gate on the flash toggle alone.
                        if self.fx.caret_flash_enabled && is_printable_keystroke(&bytes) {
                            dw.caret_anim = Some(now);
                        }
                    }
                    // Everything else was handled above (app-wide actions
                    // return early) or has no meaning in a detached window.
                    _ => {}
                }
                if viewport_moved {
                    self.update_detached_link_hover(pos, true);
                }
            }
            // Key RELEASE: owed to this window's tab only if it was sent the
            // press (kitty keyboard protocol event types; a no-op otherwise).
            // Synthetic releases (keys still held when focus leaves) are
            // forwarded too, so a program never sees a key stuck down.
            WindowEvent::KeyboardInput { event, .. } => {
                let (keymap, mods, opt) = (&self.keymap, self.key_modifiers, self.macos_option_as_alt);
                if let Some(dw) = self.detached.get_mut(pos) {
                    write_key_release(keymap, &mut dw.tab, &event, &mods, opt);
                }
            }
            // IME composition in progress in THIS window: drawn at its cursor
            // until it commits (mirrors the main window's Preedit arm).
            WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) => {
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let next = (!text.is_empty()).then_some(text);
                if dw.ime_preedit != next {
                    dw.ime_preedit = next;
                    dw.request_paint();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                let Some(dw) = self.detached.get_mut(pos) else { return };
                if dw.ime_preedit.take().is_some() {
                    dw.request_paint();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // IME commit → typed text to THIS window's PTY (no bracketed
                // paste). Mirrors the main window's Ime::Commit arm; the commit
                // ends any composition on screen.
                if let Some(dw) = self.detached.get_mut(pos) {
                    if dw.ime_preedit.take().is_some() {
                        dw.request_paint();
                    }
                }
                if !text.is_empty() {
                    // This window's overlays take the commit first (palette /
                    // search queries; hint/copy-mode drop it) — the main
                    // window's modal priority.
                    if self.overlay_ime_commit(Surface::Detached(pos), &text) {
                        return;
                    }
                    let caret_flash_enabled = self.fx.caret_flash_enabled;
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    // Snap to the live bottom (F30) then write to the PTY — the
                    // shared input core, same as the Send arm (v0.23 Task 9).
                    write_key_to_pty(&mut dw.tab, text.as_bytes(), None);
                    // The echo paints (fallback deadline), as in the Send arm.
                    let now = std::time::Instant::now();
                    dw.key_paint_due = Some(now + KEY_ECHO_GRACE);
                    if caret_flash_enabled && is_printable_keystroke(text.as_bytes()) {
                        dw.caret_anim = Some(now);
                    }
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
                dw.menu_disabled.clear();
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
                self.key_modifiers = m;
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
                // App-wide inputs, read before the dw (self.detached) borrow.
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let Some(dw) = self.detached.get_mut(pos) else { return };
                // This window's chrome geometry (its own DPI × the UI font).
                let cm = dw.chrome_metrics(ui_font);
                let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
                dw.cursor = (position.x, position.y);
                // --- Manual top-bar drag (move the window ourselves) ---
                // global_cursor = outer_position + local cursor; the window's new
                // top-left is global_cursor - the press offset. Doing this manually
                // (instead of win.drag_window()) keeps the RELEASE event in OUR
                // queue, which drop-to-reattach needs. On Wayland outer_position()
                // errs — but then bar_drag is never set (see the press handler),
                // so this arm is unreachable there.
                if let Some((ox, oy)) = dw.bar_drag {
                    // Belt-and-braces: `set_detached_fullscreen` clears the latch on
                    // every transition, so a live drag is always a WINDOWED one —
                    // moving a fullscreen window leaves a broken half-state on X11.
                    if !dw.fullscreen {
                        if let Ok(outer) = dw.window.outer_position() {
                            let nx = outer.x + (position.x - ox).round() as i32;
                            let ny = outer.y + (position.y - oy).round() as i32;
                            dw.window
                                .set_outer_position(winit::dpi::PhysicalPosition::new(nx, ny));
                        }
                    }
                    return;
                }
                let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                let cx = position.x as f32;
                let cy = position.y as f32;
                if dw.menu_open.is_some() {
                    // Menu hover tracking from the cached rects (menu is modal;
                    // no resize/close hover underneath it). Disabled (grayed)
                    // rows are inert: no hover state.
                    let new_hover = dw
                        .menu_rects
                        .iter()
                        .position(|r| {
                            cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                        })
                        .filter(|i| !dw.menu_disabled.contains(i));
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
                        cy, dw.drag_grab_dy, rows, max, h, bar_h, status_h,
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
                    // Inert while THIS window is fullscreen (amendment I-D — the
                    // hover site the blueprint missed): the press site below is
                    // gated too, so a ⤡/↔ cursor over a fullscreen edge would
                    // advertise an affordance that does nothing.
                    let zone = if dw.fullscreen {
                        ResizeZone::None
                    } else {
                        resize_zone_at(cx, cy, w, h, cm.dpi)
                    };
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
                    let hover = input::point_in(&jetty_render::detached_close_rect(w, cm), cx, cy);
                    if hover != dw.close_hover {
                        dw.close_hover = hover;
                        dw.request_paint();
                    }
                }
                // --- Grid pointer motion: the shared gridmouse step, exactly as
                // the main window runs it — extend a selection drag (edge
                // auto-scroll past the top/bottom), or report the motion to a
                // program that tracks it, once per cell. ---
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                let now = std::time::Instant::now();
                if with_detached_grid(dw, geom, self.modifiers, |g| crate::gridmouse::motion(g, now)).paint {
                    dw.request_paint();
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
                    /// Run this window's selection in a new MAIN-window tab.
                    RunSelection,
                    /// Leave fullscreen on THIS window (double-click on its bar
                    /// while fullscreen). Deferred out of the `dw` borrow.
                    ExitFullscreen,
                    /// The bar's help "?" — this window's shortcuts help.
                    ToggleHelp,
                }
                // --- THIS window's overlays first, in the main window's order:
                // hint mode / copy-mode, the command palette (captures the mouse
                // while open), then — unless a context menu takes the click —
                // the help (modal) and the search bar (✕ / panel) ---
                let s = Surface::Detached(pos);
                let Some((cx, cy)) = self.detached.get(pos).map(|d| (d.cursor.0 as f32, d.cursor.1 as f32))
                else {
                    return;
                };
                if self.modes_click(s) || self.palette_click(s, cx, cy, event_loop) {
                    return;
                }
                let menu_open = self.detached.get(pos).is_some_and(|d| d.menu_open.is_some());
                if !menu_open && (self.help_click(s, cx, cy) || self.search_bar_click(s, cx, cy)) {
                    return;
                }
                // App-wide inputs, read before the dw (self.detached) borrow.
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let act = {
                    let Some(dw) = self.detached.get_mut(pos) else { return };
                    let cm = dw.chrome_metrics(ui_font);
                    let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
                    let (cx, cy) = (dw.cursor.0 as f32, dw.cursor.1 as f32);
                    let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                    if dw.menu_open.take().is_some() {
                        // --- Context menu hit-test (consume the click entirely) ---
                        dw.menu_hover = None;
                        let hit = dw
                            .menu_rects
                            .iter()
                            .position(|r| {
                                cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                            })
                            // Disabled rows no-op (the menu still closes).
                            .filter(|i| !dw.menu_disabled.contains(i));
                        dw.menu_rects.clear();
                        dw.menu_disabled.clear();
                        dw.request_paint();
                        // Index → DETACHED_MENU_ITEMS order
                        // (Reattach/Copy/Paste/Run in New Tab).
                        match hit {
                            Some(0) => Act::Reattach,
                            Some(1) => Act::Copy,
                            Some(2) => Act::Paste,
                            Some(3) => Act::RunSelection,
                            _ => Act::None,
                        }
                    } else {
                        // --- Resize edges: corners > edges, before the bar. ---
                        // Inert while fullscreen: `drag_resize_window` on a
                        // fullscreen X11 window leaves it in a broken half-state.
                        let zone = if dw.fullscreen {
                            ResizeZone::None
                        } else {
                            resize_zone_at(cx, cy, w, h, cm.dpi)
                        };
                        if let Some(dir) = zone.direction() {
                            let _ = dw.window.drag_resize_window(dir);
                            return;
                        }
                        // --- Top bar: close ✕ → reattach; empty bar → move. ---
                        if cy < bar_h {
                            if input::point_in(&jetty_render::detached_close_rect(w, cm), cx, cy) {
                                Act::Reattach
                            } else if input::point_in(&jetty_render::detached_help_rect(w, cm), cx, cy) {
                                Act::ToggleHelp
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
                                    dw.last_bar_click = None;
                                    if dw.fullscreen {
                                        // Double-click while fullscreen means
                                        // "give me my window back": leave
                                        // FULLSCREEN ONLY, never `set_maximized`
                                        // (a no-op or a stuck half-state on a
                                        // fullscreen X11 window). A window that was
                                        // maximized before F11 comes back
                                        // maximized; a second double-click
                                        // normalises it (amendment I-F).
                                        Act::ExitFullscreen
                                    } else {
                                        dw.window.set_maximized(!dw.window.is_maximized());
                                        Act::None
                                    }
                                } else if dw.fullscreen {
                                    // Moving a fullscreen window is inert (dragging
                                    // one on X11 leaves a broken half-state).
                                    Act::None
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
                                    Act::None
                                } else {
                                    // Wayland: no readable outer position — fall
                                    // back to the compositor drag. Drop-to-reattach
                                    // is silently unavailable on this path.
                                    let _ = dw.window.drag_window();
                                    Act::None
                                }
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
                                rows, off, max, w, h, bar_h, status_h, [0, 0, 0, 0],
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
                                        cy, dw.drag_grab_dy, rows, max, h, bar_h, status_h,
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
                            // Grid-area press: the shared gridmouse press, exactly
                            // as the main window runs it — a link-modifier click
                            // on a link opens it, a program that tracks the mouse
                            // gets the press, otherwise a cell / word / line
                            // selection starts (Shift always selects).
                            let geom = detached_grid_geom(dw, ui_font, show_hud);
                            let link_mod = link_modifier_held(&self.modifiers);
                            let now = std::time::Instant::now();
                            match with_detached_grid(dw, geom, self.modifiers, |g| {
                                crate::gridmouse::press(g, MouseButton::Left, link_mod, now)
                            }) {
                                crate::gridmouse::Press::OpenLink(uri) => Self::open_url(&uri),
                                crate::gridmouse::Press::Selecting => dw.request_paint(),
                                _ => {}
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
                    Act::RunSelection => self.run_selection_in_new_tab(SelSource::Detached(pos)),
                    Act::ExitFullscreen => self.set_detached_fullscreen(pos, false),
                    Act::ToggleHelp => self.toggle_help(s),
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
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
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
                    // The shared gridmouse release, exactly as the main window
                    // runs it: copy-on-select to the PRIMARY selection, or the
                    // release of a press that went to the program — and, after a
                    // no-Shift DRAG over a mouse-grabbing program, the Shift+drag
                    // hint (cooldown shared across windows; drawn only in THIS
                    // window, F4).
                    let geom = detached_grid_geom(dw, ui_font, show_hud);
                    match with_detached_grid(dw, geom, self.modifiers, |g| {
                        crate::gridmouse::release(g, MouseButton::Left)
                    }) {
                        crate::gridmouse::Release::Copy(text) => {
                            clipboard::copy_on_select(&text, self.copy_on_select);
                            dw.request_paint();
                            return;
                        }
                        crate::gridmouse::Release::Cleared => {
                            dw.request_paint();
                            return;
                        }
                        crate::gridmouse::Release::Program { dragged } => {
                            let id = dw.window.id();
                            if dragged
                                && arm_shift_hint(&mut self.shift_hint_until, &mut self.shift_hint_cooldown, id, false)
                            {
                                dw.request_paint();
                            }
                            return;
                        }
                        crate::gridmouse::Release::Ignored => {}
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
                        // The main window's chrome bands, in ITS physical px.
                        let (main_bar_h, main_status_h) = (self.bar_h() as f64, self.status_h() as f64);
                        if let (Some(win), Some(gpu)) = (&self.window, &self.gpu) {
                            if let Ok(mp) = win.outer_position() {
                                let main_scale = win.scale_factor();
                                // EVERY input in logical points — including the
                                // bar/strip heights, which used to be passed in
                                // physical px (a too-high, half-overlapping band
                                // on a 2× display).
                                if crate::detached::main_tabbar_contains(
                                    gx / dw_scale,
                                    gy / dw_scale,
                                    mp.x as f64 / main_scale,
                                    mp.y as f64 / main_scale,
                                    gpu.config.width as f64 / main_scale,
                                    gpu.config.height as f64 / main_scale,
                                    main_bar_h / main_scale,
                                    main_status_h / main_scale,
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
                // Right-click → Reattach / Copy / Paste / Run in New Tab context
                // menu — except on the grid of a program that tracks the mouse,
                // which gets the click (Shift, or a JeTTY selection, keeps the
                // menu): the shared gridmouse routing, as in the main window.
                // Same modal gates as the main window: no menu over this
                // window's help or palette, none in hint mode (keyboard-only —
                // a menu opened there could never be clicked).
                if self
                    .ov_of(Surface::Detached(pos))
                    .is_none_or(|o| o.help_open || o.palette_open || o.hint_mode.is_some())
                {
                    return;
                }
                let theme = self.current_theme();
                let run_enabled = self.run_selection_enabled;
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let mods = self.modifiers;
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                if geom.contains_y(dw.cursor.1 as f32) {
                    let now = std::time::Instant::now();
                    if with_detached_grid(dw, geom, mods, |g| {
                        crate::gridmouse::press(g, MouseButton::Right, false, now)
                    }) == crate::gridmouse::Press::Reported
                    {
                        let id = dw.window.id();
                        self.teach_shift_right_click(id);
                        return;
                    }
                }
                let cm = dw.chrome_metrics(ui_font);
                let (cx, cy) = (dw.cursor.0 as f32, dw.cursor.1 as f32);
                dw.menu_open = Some((cx, cy));
                dw.menu_hover = None;
                // Disabled rows, computed once at open — the same needs-a-
                // selection class as the main menu: Copy (1) and Run in New
                // Tab (3) dim without a selection; Run also dims when the
                // feature is config-disabled.
                let has_sel = dw
                    .tab
                    .terminal
                    .selection_text()
                    .is_some_and(|t| !t.is_empty());
                dw.menu_disabled = match (has_sel, run_enabled) {
                    (false, _) => vec![1, 3],
                    (true, false) => vec![3],
                    (true, true) => Vec::new(),
                };
                // The v0.25.1 teachable moment, as in the main window: a menu
                // opened (with Shift) over a mouse-grabbing program with nothing
                // selected gets the Shift+drag hint right away.
                if !has_sel && crate::gridmouse::tracking(&dw.tab.terminal) != input::MouseTracking::Off {
                    let id = dw.window.id();
                    arm_shift_hint(&mut self.shift_hint_until, &mut self.shift_hint_cooldown, id, true);
                }
                // Cache the item hit-test rects once (anchor + size fixed for the
                // menu's lifetime), same pattern as the main context menu. Hints
                // come from the live keymap; widths from this window's chrome
                // layer (the same measurement the render pass uses).
                let hints: Vec<String> = crate::detached::DETACHED_MENU_ITEMS
                    .iter()
                    .map(|&l| crate::detached::menu_hint(&self.keymap, l))
                    .collect();
                let items: Vec<(&str, &str)> = crate::detached::DETACHED_MENU_ITEMS
                    .iter()
                    .zip(&hints)
                    .map(|(&l, h)| (l, h.as_str()))
                    .collect();
                let menu = jetty_render::build_menu(
                    cx,
                    cy,
                    dw.gpu.config.width,
                    dw.gpu.config.height,
                    None,
                    &theme,
                    &mut dw.chrome_text,
                    cm,
                    &items,
                    &[],
                    &dw.menu_disabled,
                );
                dw.menu_rects = menu.item_rects;
                dw.request_paint();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => {
                // Middle click, mirroring the main window's arm with this
                // window's equivalent gates:
                //  - its context menu, help or palette open, or hint mode → swallow;
                //  - only over the terminal grid, never the chrome strips;
                //  - a program that tracks the mouse (no Shift) gets the click.
                // Otherwise it pastes the PRIMARY selection (same as main).
                if self
                    .ov_of(Surface::Detached(pos))
                    .is_none_or(|o| o.help_open || o.palette_open || o.hint_mode.is_some())
                {
                    return;
                }
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let mods = self.modifiers;
                let Some(dw) = self.detached.get_mut(pos) else { return };
                if dw.menu_open.is_some() {
                    return;
                }
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                if !geom.contains_y(dw.cursor.1 as f32) {
                    return;
                }
                let now = std::time::Instant::now();
                if with_detached_grid(dw, geom, mods, |g| {
                    crate::gridmouse::press(g, MouseButton::Middle, false, now)
                }) == crate::gridmouse::Press::PastePrimary
                {
                    if let Some(text) = clipboard::get_primary() {
                        Self::paste_to_tab(&mut dw.tab, &text);
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: button @ (MouseButton::Middle | MouseButton::Right | MouseButton::Back | MouseButton::Forward),
                ..
            } => {
                // Releases of middle/right presses that went to the program
                // (their presses have their own arms above), and back/forward
                // over the grid — the same shared handling as the main window.
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let mods = self.modifiers;
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                let now = std::time::Instant::now();
                if state == ElementState::Released {
                    with_detached_grid(dw, geom, mods, |g| crate::gridmouse::release(g, button));
                } else if dw.menu_open.is_none() && geom.contains_y(dw.cursor.1 as f32) {
                    with_detached_grid(dw, geom, mods, |g| crate::gridmouse::press(g, button, false, now));
                }
            }
            WindowEvent::DroppedFile(path) => {
                // A file dropped on this window types its shell-quoted path into
                // its tab (through the sanitizing paste path, like main).
                if let Some(dw) = self.detached.get_mut(pos) {
                    let text = crate::gridmouse::dropped_path_text(&path);
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
                    dw.grid_mouse.reset();
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
                // Wheel over a detached window: the shared gridmouse wheel, as
                // the main window runs it (reports to a tracking program, arrows
                // for an alt-screen pager, else this window's own scrollback;
                // Shift or the scrollbar keep it on the scrollback). THIS window's
                // own line accumulator, so a leftover fraction never bleeds across
                // windows (F26). This window's overlays own the wheel first
                // (hint/copy-mode swallow it, the help and palette scroll).
                if self.overlay_wheel(Surface::Detached(pos), delta) {
                    return;
                }
                let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
                let mods = self.modifiers;
                let Some(dw) = self.detached.get_mut(pos) else { return };
                let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
                let (w, h) = (dw.gpu.config.width, dw.gpu.config.height);
                let over_scrollbar = {
                    let t = &dw.tab.terminal;
                    jetty_render::scrollbar_rect_geom(
                        t.rows(), t.scroll_offset(), t.scroll_max(), w, h, bar_h, status_h, [0, 0, 0, 0],
                    )
                    .is_some_and(|r| {
                        let cx = dw.cursor.0 as f32;
                        cx >= r.x && cx <= r.x + r.w
                    })
                };
                let geom = detached_grid_geom(dw, ui_font, show_hud);
                let mut vertical = std::mem::take(&mut dw.scroll_accum);
                let outcome = with_detached_grid(dw, geom, mods, |g| {
                    crate::gridmouse::wheel(g, delta, over_scrollbar, &mut vertical)
                });
                dw.scroll_accum = vertical;
                match outcome {
                    // User-originated PTY bytes — same cancel rule as main.
                    crate::gridmouse::Wheel::Arrows => {
                        crate::runsel::cancel_on_user_write(&mut dw.tab.pending_inject);
                    }
                    crate::gridmouse::Wheel::Scrolled => {
                        dw.request_paint();
                        // Viewport moved under a stationary pointer (mirrors main).
                        self.update_detached_link_hover(pos, true);
                    }
                    _ => {}
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
    /// a top bar (title pill + close ✕, its metrics' bar height), the bottom status strip
    /// (perf HUD) when `show_perf_hud`, and the Reattach/Copy/Paste context menu
    /// when open. Mirrors the main window's terminal draw passes from the
    /// `RedrawRequested` arm of `window_event` using the detached window's OWN
    /// `gpu`/`text`/`chrome_text`/`quad`, and applies the SAME final effects:
    /// the rounded-corner mask (all four corners — a detached window is never
    /// top-flush), the transparent theme-bg clear, the caret flash, and the CRT
    /// post-pass (which owns the rounded corners while active, exactly like the
    /// main window). Summon/Tier-B reveals stay main-window-only.
    fn render_detached_window(&mut self, pos: usize) {
        // Drain this tab's PTY output into its terminal before snapshotting.
        // Detached tabs are no longer in `self.tabs`, so the main `drain_pty`
        // loop never sees them — without this the detached grid would stay
        // frozen at whatever it looked like the instant it was detached. Uses
        // the shared, byte-budgeted `drain_one_tab` (same flood protection as
        // the main window); any capped remainder is drained by the Wakes the
        // reader queued, which re-request this window's redraw.
        // A run-selection notice needs `&mut self` (pill state), so the drain
        // runs in its own scope before the long `dw` borrow below.
        let notice = {
            let Some(dw) = self.detached.get_mut(pos) else { return };
            // This frame shows the latest keystroke's effect: its fallback paint
            // is no longer owed (mirrors the main window's RedrawRequested).
            dw.key_paint_due = None;
            let mut vt_read: u64 = 0;
            let (had, _, notice) = Self::drain_one_tab(&mut dw.tab, &mut vt_read);
            if had {
                dw.ov.note_output();
            }
            // OSC titles: keep the OS window title in sync (no-op unless changed).
            dw.sync_os_title();
            notice
        };
        if let Some(n) = notice {
            self.show_status_pill(n);
        }
        // This window's open search: throttled streaming re-collect (the main
        // window's render-path twin), and drop hint/copy-mode if a program
        // switched to the alt screen mid-mode.
        self.refresh_search_if_due(Surface::Detached(pos), std::time::Instant::now());
        self.exit_modes_on_alt_screen(Surface::Detached(pos));
        // THIS window's overlays, captured (owned) before the wide borrows — the
        // main window's capture, from this window's own state.
        let s = Surface::Detached(pos);
        let (search_ui, search_hits) = self.search_draw(s);
        let Some(ov) = self.ov_of(s) else { return };
        let (palette_ui, hint_ui, copy_mode_ui) = (ov.palette_draw(), ov.hint_draw(), ov.copy_draw());
        let (help_open, help_scroll) = (ov.help_open, ov.help_scroll);
        // The IME preedit isn't drawn while one of the window's overlays owns
        // its keyboard (mirrors the main window).
        let overlay_owns_keys = ov.owns_keys() || hint_ui.is_some() || copy_mode_ui.is_some();
        let help_rows: Vec<String> = if help_open { self.help_rows.clone() } else { Vec::new() };
        let font_logical = self.font_logical;
        let Some(dw) = self.detached.get_mut(pos) else { return };

        // Snapshot + theme + chrome inputs are read before the mutable
        // gpu/text/quad borrow below (same pattern as the main RedrawRequested).
        let snap = dw.tab.terminal.snapshot();
        let title = dw.tab.title.clone();
        // The IME candidate window follows this window's cursor cell (handed to
        // winit only when that cell moves) — mirrors the main window.
        let ime_area = {
            let (cw, ch) = dw.text.cell_size();
            input::ime_cursor_area(snap.cursor_row, snap.cursor_col, cw, ch, dw.chrome_bands(self.ui_font_logical, self.show_perf_hud).0)
        };
        if dw.ime_area != Some(ime_area) {
            dw.ime_area = Some(ime_area);
            dw.window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(ime_area.0, ime_area.1),
                winit::dpi::PhysicalSize::new(ime_area.2, ime_area.3),
            );
        }
        let preedit_ui = if overlay_owns_keys { None } else { dw.ime_preedit.clone() };
        let close_hover = dw.close_hover;
        let menu_open = dw.menu_open;
        let menu_hover = dw.menu_hover;
        let menu_disabled = dw.menu_disabled.clone();
        // Ctrl+hover link underline spans, snapshotted before the wide
        // gpu/text/quad borrows below (same pattern as the main window).
        let link_spans: Option<Vec<(usize, usize, usize)>> =
            if link_modifier_held(&self.modifiers) {
                dw.link_hover.as_ref().map(|h| h.spans.clone())
            } else {
                None
            };
        let theme = self.current_theme();
        let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
        // Menu hints from the LIVE keymap (only built while the menu is open).
        let menu_hints: Vec<String> = if menu_open.is_some() {
            crate::detached::DETACHED_MENU_ITEMS
                .iter()
                .map(|&l| crate::detached::menu_hint(&self.keymap, l))
                .collect()
        } else {
            Vec::new()
        };
        // THIS window's own HUD line (its frame time, its idle one-shot; the
        // process CPU% / VT MB/s are shared). Never wakes anything by itself.
        let perf_label = self.detached_perf_label(pos);
        // Shift+drag hint toast — the shared timer is tagged with the window
        // the drag happened in; captured here (Copy) and compared against
        // THIS window's id after the dw borrow below, so only that window
        // draws the pill (F4).
        let shift_hint_until = self.shift_hint_until;
        // Run-selection status pill — captured (Clone) before the dw borrow;
        // drawn only when tagged with THIS window's id (same rule as the hint).
        let status_pill = self.status_pill.clone();
        // Effects inputs, captured before the mutable dw borrow below — the
        // SAME settings the main window renders with (visual parity).
        let corner_radius = self.corner_radius;
        let fx = self.fx.clone();
        let crt_time = (self.crt_clock.elapsed().as_secs_f64() % CRT_PHASE_WRAP) as f32;

        let Some(dw) = self.detached.get_mut(pos) else { return };
        // This window's chrome geometry (its own DPI × the UI font).
        let cm = dw.chrome_metrics(ui_font);
        let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
        let shift_hint_show =
            shift_hint_live_in(shift_hint_until, dw.window.id(), std::time::Instant::now());
        let status_pill_msg: Option<String> = status_pill.and_then(|(m, until, wid)| {
            (wid == dw.window.id() && std::time::Instant::now() < until).then_some(m)
        });
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
        // Suppressed to 0 while THIS window is fullscreen — a fullscreen window
        // with rounded corners shows the desktop through four notches at the
        // screen edges — which also SKIPS the corner-mask pass entirely
        // (`CornerMask::apply` early-returns on all-zero radii) and collapses the
        // CRT shader's corner coverage to 1.0. The `corner_radius` FIELD is never
        // mutated: suppression is display-time only.
        let scale = dw.window.scale_factor() as f32;
        let corner_radius_px = effective_corner_radius_px(corner_radius, scale, dw.fullscreen);
        // CRT routing: when enabled, the whole scene renders into this window's
        // offscreen texture and the CRT pass samples it onto the surface — the
        // exact main-window flow (no Tier-B summons here, so no bypass case).
        // Re-allocate the offscreen lazily when stale (same check as main).
        let crt_active = fx.crt_enabled;
        if crt_active
            && dw.offscreen.as_ref().is_none_or(|(t, _)| {
                t.width() != dw.gpu.config.width || t.height() != dw.gpu.config.height
            })
        {
            dw.offscreen = Some(Self::make_offscreen(&dw.gpu));
        }
        let gpu = &mut dw.gpu;
        let text = &mut dw.text;
        let chrome_text = &mut dw.chrome_text;
        let quad = &mut dw.quad;
        let corner_mask = &dw.corner_mask;
        let crt = &dw.crt;
        let offscreen = dw.offscreen.as_ref();
        let image_layer = &mut dw.image_layer;

        let Some((frame, view)) = gpu.acquire_frame() else {
            // Acquire failed: this frame's damage was not shown. Start the
            // bounded retry schedule (`about_to_wait` issues + advances it).
            dw.acquire_retry
                .get_or_insert_with(|| next_acquire_retry(None, std::time::Instant::now()));
            return;
        };
        let width = gpu.config.width;
        let height = gpu.config.height;
        // Scene target: the offscreen when CRT is on, else the surface directly
        // (byte-identical to the pre-CRT hot path).
        let scene_view: &wgpu::TextureView = match (crt_active, offscreen) {
            (true, Some((_, off))) => off,
            _ => &view,
        };

        // The grid sits below the top bar (and above the status strip).
        let grid_top = bar_h;
        let grid_bottom_px = (height as f32 - status_h).max(0.0);

        // Passes 1–4 via the shared render core (v0.23 Task 8). The detached
        // title bar (Pass 3) is the mid-scene chrome, injected between the glyph
        // and scrollbar/cursor passes. `slide_y = 0.0`: a detached window has no
        // dropdown slide; its own search tint and copy-mode cursor come from
        // THIS window's overlays. The main-only caret GLOW / summon reveals live
        // only in the main caller's tail and never reach here.
        let scene = GridScene {
            snap: &snap,
            theme: &theme,
            grid_top,
            slide_y: 0.0,
            grid_bottom: grid_bottom_px,
            status_h,
            scale,
            search_hits: &search_hits,
            failed_rows: &failed_rows,
            link_spans: link_spans.as_ref(),
            images: &images,
            focused,
            caret_t_for_flash,
            caret_flash_color: fx.caret_flash_color,
            copy_mode_active: copy_mode_ui.is_some(),
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
            // Pass 3: the top bar (title pill + close ✕) over the grid.
            |quad, device, queue, view, w, h| {
                let bar = jetty_render::build_detached_bar(
                    w, &title, &theme, close_hover, &mut *chrome_text, cm,
                );
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
            let strip = jetty_render::build_status_strip(
                width, height as f32 - status_h, status_h, perf_label.as_deref(), &theme,
                &mut *chrome_text, cm,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[strip.quad]);
            if let Some(label) = strip.label {
                let _ = chrome_text.render_overlays(
                    &gpu.device, &gpu.queue, scene_view, width, height, &[label],
                );
            }
        }
        // Pass 5b: Shift+drag hint toast — the main window's Pass 4c pill,
        // positioned above the status strip (the detached bar is always on top,
        // so no bottom-bar / slide offset terms apply). Drawn only on frames
        // where the 3.5s flag is live — no steady-state cost.
        let pill_bottom = height as f32 - status_h - cm.px(14.0);
        if shift_hint_show {
            let pill = jetty_render::build_toast_pill(
                width, pill_bottom, 0.0, "Hold Shift while dragging to select text", &theme,
                &mut *chrome_text, cm,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill.quad]);
            let _ = chrome_text.render_overlays(
                &gpu.device, &gpu.queue, scene_view, width, height, &[pill.label],
            );
        }
        // Pass 5b': run-selection status pill — the main window's Pass 4c'
        // twin, stacked above the shift hint on the rare frame both are live.
        if let Some(msg) = &status_pill_msg {
            let stack = if shift_hint_show { cm.pill_h() + cm.px(8.0) } else { 0.0 };
            let pill = jetty_render::build_toast_pill(
                width, pill_bottom - stack, 0.0, msg, &theme, &mut *chrome_text, cm,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill.quad]);
            let _ = chrome_text.render_overlays(
                &gpu.device, &gpu.queue, scene_view, width, height, &[pill.label],
            );
        }
        // Pass 5c-: this window's search bar and hint chips (the main window's
        // Passes 4d / 4e). Hint labels render in the TERMINAL font via the
        // grid layer, so they always fit their one-row chip.
        if let Some((q, cur, total)) = &search_ui {
            let sb = jetty_render::build_search_bar(width, grid_top, &theme, &mut *chrome_text, cm, q, *cur, *total);
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &sb.quads);
            if !sb.labels.is_empty() {
                let _ = chrome_text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &sb.labels);
            }
        }
        if let Some((labeled, typed)) = &hint_ui {
            let refs: Vec<(&str, usize, usize)> = labeled.iter().map(|(l, r, c)| (l.as_str(), *r, *c)).collect();
            let (cell_w, cell_h) = text.cell_size();
            let grid_cm = jetty_render::ChromeMetrics::new(scale, font_logical);
            let ov = jetty_render::build_hint_overlay(
                &refs, cell_w, cell_h, grid_top, &theme, &mut *text, grid_cm, typed, width,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &ov.quads);
            if !ov.labels.is_empty() {
                let _ = text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &ov.labels);
            }
        }
        // Pass 5c: IME preedit at this window's cursor (the main window's
        // Pass 4e' twin): terminal font via the grid layer, underlined.
        if let Some(p) = &preedit_ui {
            let (cell_w, cell_h) = text.cell_size();
            if let Some(ov) = jetty_render::build_preedit_overlay(
                p, snap.cursor_row, snap.cursor_col, snap.cols, cell_w, cell_h, grid_top, &theme, scale,
            ) {
                quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &ov.quads);
                let _ = text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &ov.labels);
            }
        }
        // Pass 5d: this window's copy-mode "COPY" pill (main window's Pass 4f).
        if let Some((_, _, selecting, line_mode)) = copy_mode_ui {
            let pill = jetty_render::build_copy_pill(width, grid_top, &theme, &mut *chrome_text, cm, line_mode, selecting);
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &pill.quads);
            if !pill.labels.is_empty() {
                let _ = chrome_text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &pill.labels);
            }
        }
        // Pass 6: the Reattach/Copy/Paste context menu on top of everything.
        if let Some((mx, my)) = menu_open {
            let items: Vec<(&str, &str)> = crate::detached::DETACHED_MENU_ITEMS
                .iter()
                .zip(&menu_hints)
                .map(|(&l, h)| (l, h.as_str()))
                .collect();
            let menu = jetty_render::build_menu(
                mx, my, width, height, menu_hover, &theme, &mut *chrome_text, cm, &items, &[],
                &menu_disabled,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &menu.quads);
            if !menu.labels.is_empty() {
                let _ = chrome_text.render_overlays(
                    &gpu.device, &gpu.queue, scene_view, width, height, &menu.labels,
                );
            }
        }
        // Pass 7: this window's keyboard-shortcuts help, then the command
        // palette LAST (above everything) — the main window's order.
        if help_open && palette_ui.is_none() {
            let help = jetty_render::build_help_overlay(
                width, height, &theme, &mut *chrome_text, cm, &help_rows, help_scroll,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &help.quads);
            if !help.labels.is_empty() {
                let _ = chrome_text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &help.labels);
            }
        }
        if let Some((q, prows_data, total, first)) = &palette_ui {
            let prows: Vec<jetty_render::PaletteRow> = prows_data
                .iter()
                .map(|(t, idx, sel)| jetty_render::PaletteRow { title: t, match_indices: idx, selected: *sel })
                .collect();
            let pal = jetty_render::build_command_palette(
                width, height, &theme, &mut *chrome_text, cm, q, &prows, *total, *first,
            );
            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &pal.quads);
            if !pal.labels.is_empty() {
                let _ = chrome_text.render_overlays(&gpu.device, &gpu.queue, scene_view, width, height, &pal.labels);
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
        if let (true, Some((_, crt_src))) = (crt_active, offscreen) {
            crt.apply(
                &gpu.device,
                &gpu.queue,
                &view,
                crt_src,
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
        // The swapchain is healthy again: drop any retry schedule.
        dw.acquire_retry = None;
        // Flood pacing anchor (see the main window's present).
        dw.last_present_at = Some(std::time::Instant::now());
        dw.paced_paint_at = None;
        // Missed-paint proof counter (JETTY_FRAME_LOG only; see the field docs).
        // `self.frames_presented`/`self.frame_log` are fields disjoint from the
        // live `dw` borrow of `self.detached`, so this is a plain field bump.
        // (No self-drive here: `about_to_wait` alone pumps the next frame while
        // a caret burst / animated CRT is live on this window, and repaints a
        // pill away once at its expiry.)
        if self.frame_log {
            self.frames_presented += 1;
            eprintln!("JETTY_FRAME {} detached", self.frames_presented);
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
                    self.pick_theme(i);
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
                // No-op in Center/Fullscreen mode (the slider is grayed/disabled
                // there) and while fullscreen — the latch is what enables the
                // release-time re-dock, which must not fire on a fullscreen window.
                if dock_reassert_ok(self.window_mode, self.main_fullscreen) {
                    self.dragging_dropdown = true;
                    self.dropdown_height_pct =
                        self.dropdown_pct_from_cursor(cx, &geom.dropdown_track);
                }
            }
            input::MouseAction::StartDropdownWidthDrag => {
                // No-op in Center/Fullscreen mode (the slider is grayed/disabled
                // there) and while fullscreen — see StartDropdownDrag above.
                if dock_reassert_ok(self.window_mode, self.main_fullscreen) {
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
                // Write/remove the login autostart entry to match; persist() (below)
                // saves the config key, which is the source of truth.
                if let Err(e) = set_launch_at_login(self.launch_at_login) {
                    self.show_config_warnings(&[e]);
                }
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
                self.end_settings_drags();
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
                    // divides it by its layout scale (the settings chrome unit) to
                    // lay bands out in LOGICAL space. So the clamp bound (a LOGICAL
                    // content/viewport delta) must be scaled by the SAME factor, or
                    // on HiDPI the bottom bands (caret RGB sliders) stayed
                    // unreachable and on sub-1× the scroll overshot into blank
                    // space (F10).
                    let dpi = self.settings_metrics().overlay_u().max(0.1);
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
                // gone — end every drag now (sliders must not keep tracking the
                // cursor with no button held after focus returns, F36) AND keep
                // its live-applied value: it is persisted like a release.
                self.end_settings_drags();
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
    /// redraw on each affected, actually-visible window; returns whether it did.
    fn flush_expired_syncs(&mut self, now: std::time::Instant) -> bool {
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
        let mut painted = main_needs_paint;
        if main_needs_paint {
            self.request_main_paint();
        }
        for dw in &mut self.detached {
            if dw.tab.terminal.sync_deadline().is_some_and(|d| now >= d) {
                dw.tab.terminal.flush_sync();
                if !dw.occluded {
                    dw.request_paint();
                    painted = true;
                }
            }
        }
        painted
    }
}

impl ApplicationHandler<AppEvent> for App {
    /// The event loop is about to return: leave OS fullscreen on every JeTTY
    /// window while they still exist.
    ///
    /// This is the ONE chokepoint for app exit — there are five `event_loop.exit()`
    /// sites (last tab closed, last shell exited, the quit confirmation's ✕ and its
    /// Enter, a fatal PTY spawn failure) and winit calls `exiting` after all of
    /// them, with `App` (and therefore every window) still alive.
    ///
    /// Why it matters: macOS `set_simple_fullscreen(true)` SAVES and overwrites the
    /// app-scoped `NSApplication.presentationOptions` (auto-hide Dock + menu bar)
    /// and only the matching `set_simple_fullscreen(false)` restores them. Dropping
    /// a fullscreen window without exiting first therefore leaves the user's Dock
    /// and menu bar auto-hidden for the rest of their session, in every other app.
    /// A no-op on every other platform and whenever nothing is fullscreen.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Write a still-debounced settings change before the process goes away.
        self.persister.borrow_mut().flush_and_wait(std::time::Duration::from_millis(1500));
        self.exit_main_fullscreen_bare();
        // Detached windows are dropped with `App` too, and each one carries the
        // same app-scoped macOS presentation-options hazard.
        for i in 0..self.detached.len() {
            self.exit_detached_fullscreen_bare(i);
        }
    }

    /// THE scheduler. Every time-based wake and every continuous-frame decision
    /// lives here — there is NO periodic heartbeat and NO render-tail self-drive:
    ///
    /// * elapsed deadlines are serviced at the top (sync flush, reflow debounces,
    ///   auto-hide, reload, run-selection, search refresh, animation expiry,
    ///   keystroke fallback paints, pill expiry, acquire retries, idle HUD);
    /// * continuous animation (`Poll` + re-request) only while an animation is
    ///   live on an EFFECTIVELY VISIBLE window with a healthy swapchain;
    /// * otherwise `WaitUntil` the earliest future deadline, or `Wait` (0 CPU).
    ///
    /// macOS does NOT deliver a `RedrawRequested` for a `request_redraw()` issued
    /// under `ControlFlow::Wait` until an input event arrives, so any paint
    /// requested from in here (`painted`) runs ONE `Poll` iteration to deliver it,
    /// then the loop settles back to `Wait`. On X11/Wayland a pending redraw never
    /// blocks the loop anyway, so that extra iteration is free.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // PTY output left queued by this iteration's drains is scheduled for the
        // next one (and idle tabs re-armed). First, so no early return skips it.
        self.rearm_pty_wakes();
        // DECSET 1004 focus reports for whatever this event batch changed
        // (OS focus, summon/hide, a tab switch) — one pass, every window.
        self.sync_focus_reports();
        // Input-latency percentile emit (JETTY_PERF_LOG only): runs HERE, off the
        // timed present path, so printing a batch never stalls the frame it measured
        // (observer-effect fix). Emits at most once per REPORT_EVERY new samples.
        if self.perf.on {
            self.perf.maybe_report();
        }
        // Force-flush any elapsed synchronized update (CSI ?2026) FIRST so a
        // stuck BSU can't freeze the terminal; the next pending one is scheduled
        // via WaitUntil below (F1).
        let mut painted = self.flush_expired_syncs(std::time::Instant::now());
        // A lost GPU device is rebuilt before anything below tries to render on it.
        painted |= self.recover_lost_gpu(std::time::Instant::now());
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
                painted = true;
            }
        }
        // Same debounced reflow for the detached windows (their Resized arm
        // only resizes the surface and arms `reflow_pending_at`, exactly like
        // the main window's — one SIGWINCH per drag, no p10k prompt scatter).
        {
            let (ui_font, show_hud) = (self.ui_font_logical, self.show_perf_hud);
            let now = std::time::Instant::now();
            // Indexed loop (not iter_mut) so the reflowed window's cached
            // Ctrl+hover can be revalidated via &mut self below (F6).
            for pos in 0..self.detached.len() {
                let dw = &mut self.detached[pos];
                if dw.reflow_pending_at.is_some_and(|d| now >= d) {
                    dw.reflow_pending_at = None;
                    let (cw, ch) = dw.text.cell_size();
                    let (bar_h, status_h) = dw.chrome_bands(ui_font, show_hud);
                    let (cols, rows) = crate::detached::grid_dims(
                        dw.gpu.config.width as f32,
                        dw.gpu.config.height as f32,
                        cw,
                        ch,
                        SCROLLBAR_GUTTER,
                        bar_h,
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
                    painted = true;
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
            // A reload repaints every surface it changed; deliver those paints.
            painted = true;
        }
        // Edge auto-scroll of a selection drag held above/below the grid (any
        // window): due steps run HERE, the next one folds into WaitUntil below —
        // a timer, never Poll, and nothing at all without such a drag.
        if self.service_grid_autoscroll() {
            painted = true;
        }
        // Debounced settings save: the burst of changes settled — hand them to the
        // background writer (non-blocking). The deadline is folded into WaitUntil
        // below, so this costs one wake per burst.
        if self
            .persister
            .borrow()
            .due_at()
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.persister.borrow_mut().flush();
        }
        // Run-selection pending-inject deadlines (elapsed ones service HERE,
        // future ones fold into WaitUntil below — the same two-halves pattern
        // as autohide/reload above). A silent destination shell produces no
        // drain, so the timeout/TTL need this wake to fire/drop on time.
        // Gated on ONE bool that is false forever when the feature is unused.
        if self.runsel_active {
            self.service_runsel_deadlines();
        }
        // Trailing scrollback-search refresh (F10): a streaming burst that
        // ended inside the throttle window marked the matches dirty but never
        // got a re-collect (no later drain carries data), leaving highlights,
        // counter and Enter-navigation stale indefinitely. Service the skipped
        // refresh exactly once at the throttle deadline (scheduled via the
        // WaitUntil merge below); the flag only exists while the bar is open
        // AND output was drained, so idle stays at zero work.
        let search_now = std::time::Instant::now();
        if self.refresh_search_if_due(Surface::Main, search_now) && self.visible && !self.main_occluded {
            if let Some(w) = &self.window {
                w.request_redraw();
                painted = true;
            }
        }
        // Same trailing refresh for every detached window's own search bar.
        for p in 0..self.detached.len() {
            if self.refresh_search_if_due(Surface::Detached(p), search_now) {
                if let Some(dw) = self.detached.get(p).filter(|dw| !dw.occluded) {
                    dw.request_paint();
                    painted = true;
                }
            }
        }
        let now = std::time::Instant::now();
        let main_visible = self.visible && !self.main_occluded;
        // Wall-clock animation expiry. Every self-driven animation ENDS BY TIME
        // here, not only inside a frame that reached t ≥ 1 after a successful
        // acquire+present — so a window whose acquire keeps failing (macOS
        // ordered-out, a Wayland Timeout, the no-GPU fallback) can never pin the
        // loop in Poll. An effectively visible window gets one final paint so the
        // settled (effect-free) frame is what stays on screen.
        let mut main_settled = false;
        if self
            .summon_anim
            .is_some_and(|s| anim_expired(s, self.summon_effect.duration(), now))
        {
            self.summon_anim = None;
            main_settled = true;
        }
        if self.slide_anim.is_some_and(|s| anim_expired(s, DROPDOWN_SLIDE_SECS, now)) {
            self.slide_anim = None;
            main_settled = true;
        }
        let caret_secs = self.fx.caret_flash_ms / 1000.0;
        if self.caret_anim.is_some_and(|s| anim_expired(s, caret_secs, now)) {
            self.caret_anim = None;
            main_settled = true;
        }
        // Keystroke fallback paint: the echo normally painted (and cleared this)
        // first; a key that produced no output paints once at its deadline.
        if self.key_paint_due.is_some_and(|d| now >= d) {
            self.key_paint_due = None;
            main_settled = true;
        }
        if main_settled && main_visible {
            self.request_main_paint();
            painted = true;
        }
        for dw in &mut self.detached {
            let mut settled = false;
            if dw.caret_anim.is_some_and(|s| anim_expired(s, caret_secs, now)) {
                dw.caret_anim = None;
                settled = true;
            }
            if dw.key_paint_due.is_some_and(|d| now >= d) {
                dw.key_paint_due = None;
                settled = true;
            }
            if settled && !dw.occluded {
                dw.request_paint();
                painted = true;
            }
        }
        // Transient pills (Shift+drag hint, run-selection status): painted when
        // armed and repainted ONCE here at expiry to clear them — never self-
        // driven per frame while they show (that re-rendered the whole scene for
        // ~4 s per pill).
        if let Some(wid) = self.shift_hint_until.filter(|(t, _)| now >= *t).map(|(_, w)| w) {
            self.shift_hint_until = None;
            painted |= self.request_window_paint(wid);
        }
        if let Some(wid) = self
            .status_pill
            .as_ref()
            .filter(|(_, t, _)| now >= *t)
            .map(|(_, _, w)| *w)
        {
            self.status_pill = None;
            painted |= self.request_window_paint(wid);
        }
        // Failed-acquire retries (see `AcquireRetry`). The retry is advanced as
        // it is ISSUED, so a frame that never reaches `acquire_frame` cannot
        // leave an elapsed deadline behind (which would WaitUntil-spin). A window
        // that is not effectively visible drops its retry: the next summon /
        // un-occlude paints anyway.
        if let Some(r) = self.acquire_retry {
            if !main_visible || self.gpu.is_none() {
                self.acquire_retry = None;
            } else if now >= r.due {
                self.acquire_retry = Some(next_acquire_retry(Some(r), now));
                self.request_main_paint();
                painted = true;
            }
        }
        for dw in &mut self.detached {
            if let Some(r) = dw.acquire_retry {
                if dw.occluded {
                    dw.acquire_retry = None;
                } else if now >= r.due {
                    dw.acquire_retry = Some(next_acquire_retry(Some(r), now));
                    dw.request_paint();
                    painted = true;
                }
            }
        }
        if let Some(r) = self.settings_acquire_retry {
            if self.settings_window.is_none() {
                self.settings_acquire_retry = None;
            } else if now >= r.due {
                self.settings_acquire_retry = Some(next_acquire_retry(Some(r), now));
                self.request_settings_paint();
                painted = true;
            }
        }
        // Flood paints deferred to the next refresh (`pace_paint`): a due one
        // paints now — one frame per refresh while a flood lasts. A window that
        // is hidden/occluded drops it (it repaints when it comes back).
        if let Some(t) = self.paced_paint_at {
            if !main_visible {
                self.paced_paint_at = None;
            } else if now >= t {
                self.paced_paint_at = None;
                self.request_main_paint();
                painted = true;
            }
        }
        for dw in &mut self.detached {
            if let Some(t) = dw.paced_paint_at {
                if dw.occluded {
                    dw.paced_paint_at = None;
                } else if now >= t {
                    dw.paced_paint_at = None;
                    dw.request_paint();
                    painted = true;
                }
            }
        }
        // Idle-HUD one-shot: flip the HUD from its last live value to an honest
        // "idle" reading once the app settles — only for an effectively visible
        // window (see `perf_idle_decision`: requesting it while hidden spun a core
        // at 100% for as long as the window stayed hidden).
        let perf_idle = perf_idle_decision(
            self.show_perf_hud,
            self.perf_idle_shown,
            self.perf_idle_at,
            main_visible,
            now,
        );
        if perf_idle == IdleHud::RepaintNow {
            self.request_main_paint();
            painted = true;
        }
        // The same one-shot per DETACHED window, on its own deadline (an occluded
        // window owes nothing: it repaints when it comes back).
        for dw in &self.detached {
            let idle =
                perf_idle_decision(self.show_perf_hud, dw.perf_idle_shown, dw.perf_idle_at, !dw.occluded, now);
            if idle == IdleHud::RepaintNow {
                dw.request_paint();
                painted = true;
            }
        }

        // Continuous frames (Poll + re-request) are decided HERE only. Each
        // animation term is gated on the main window being EFFECTIVELY VISIBLE
        // (shown + not occluded/minimized), having a GPU, and a healthy swapchain
        // (no acquire retry pending): a hidden, minimized or acquire-failing
        // window must never Poll-render invisible frames (F8/F16/F17/F18). The
        // caret burst starts pumping only after its first frame
        // (`caret_drives_frames`). `crt_anim_live()` is false whenever CRT
        // animation is off, so static/off CRT keeps idle at Wait. Poll is
        // throttled to vsync by Fifo present. The dock/center re-assert counters
        // only need `visible` (they move the window, they don't paint) and count
        // down in RedrawRequested, so they are bounded. A pending (debounced)
        // reflow never selects Poll — it is a single WaitUntil wake below.
        let main_can_animate =
            main_visible && self.gpu.is_some() && self.acquire_retry.is_none();
        let main_pending = (main_can_animate
            && (self.summon_anim.is_some()
                || self.slide_anim.is_some()
                || self.summon_pending
                || self.fx.crt_anim_live()
                || caret_drives_frames(self.caret_anim, self.key_paint_due)))
            || (self.visible && (self.pending_dock_frames > 0 || self.pending_center_frames > 0));
        if main_pending {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        // Detached windows animate under the SAME gates, PER WINDOW: an animated
        // CRT sub-effect (shared setting) or a live caret-flash burst — only for
        // windows that are not occluded/minimized and whose swapchain is healthy.
        let crt_live = self.fx.crt_anim_live();
        let detached_animates = |d: &crate::detached::DetachedWindow| {
            !d.occluded
                && d.acquire_retry.is_none()
                && (crt_live || caret_drives_frames(d.caret_anim, d.key_paint_due))
        };
        let detached_pending = self.detached.iter().any(detached_animates);
        if detached_pending {
            for dw in self.detached.iter().filter(|d| detached_animates(d)) {
                dw.window.request_redraw();
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

        // Earliest FUTURE deadline we owe a single wake for. Nothing polls — we
        // sleep until the soonest and wake exactly once. Every elapsed deadline
        // was serviced (cleared or advanced) above, so each one merged here is
        // strictly in the future: a past `WaitUntil` would return immediately and
        // spin.
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
        // The debounced settings-save deadline (an elapsed one was flushed above).
        if let Some(d) = self.persister.borrow().due_at() {
            merge_wake(&mut wake_at, d);
        }
        // Pending synchronized-update (CSI ?2026) flush deadline: wake exactly
        // once at the soonest so a stuck BSU is force-flushed on time. Any
        // already-elapsed deadline was flushed at the top of this fn, so this is
        // strictly in the future or None (F1).
        if let Some(d) = self.sync_wake_at() {
            merge_wake(&mut wake_at, d);
        }
        // Run-selection pending-inject deadlines: the MIN over all live
        // pendings (per-tab deadlines — two concurrent pendings each get their
        // wake). Elapsed ones were serviced above, so these are strictly in
        // the future. Iterates tabs ONLY while `runsel_active` (one bool,
        // false when the feature is unused — the zero-cost invariant).
        if self.runsel_active {
            for tab in self.tabs.iter().chain(self.detached.iter().map(|d| &d.tab)) {
                if let Some(p) = &tab.pending_inject {
                    merge_wake(&mut wake_at, p.deadline());
                }
            }
        }
        // The next edge auto-scroll step (due ones ran above, so it's future).
        if let Some(d) = self.grid_autoscroll_due() {
            merge_wake(&mut wake_at, d);
        }
        // Skipped (throttled) open-search refresh: wake once at the throttle
        // deadline so the trailing re-collect above runs (F10). An elapsed
        // deadline was serviced above, so this is strictly in the future.
        let search_wakes = std::iter::once(self.ov.search_wake())
            .chain(self.detached.iter().map(|d| d.ov.search_wake()))
            .flatten();
        for t in search_wakes {
            merge_wake(&mut wake_at, t);
        }
        // Idle-HUD one-shot (effectively visible windows only — see above).
        if let IdleHud::WakeAt(d) = perf_idle {
            merge_wake(&mut wake_at, d);
        }
        for dw in &self.detached {
            if let IdleHud::WakeAt(d) =
                perf_idle_decision(self.show_perf_hud, dw.perf_idle_shown, dw.perf_idle_at, !dw.occluded, now)
            {
                merge_wake(&mut wake_at, d);
            }
        }
        // Deferred flood paints (due ones were painted above → strictly future).
        if let Some(t) = self.paced_paint_at {
            merge_wake(&mut wake_at, t);
        }
        for dw in &self.detached {
            if let Some(t) = dw.paced_paint_at {
                merge_wake(&mut wake_at, t);
            }
        }
        // A failed GPU rebuild retries once its backoff elapses (a due one ran
        // at the top of this iteration, so this is strictly in the future).
        if let Some(t) = self.gpu_rebuild_retry_at.filter(|&t| t > now) {
            merge_wake(&mut wake_at, t);
        }
        // Pill expiries: one wake each to repaint the pill away.
        if let Some((t, _)) = self.shift_hint_until {
            merge_wake(&mut wake_at, t);
        }
        if let Some((_, t, _)) = &self.status_pill {
            merge_wake(&mut wake_at, *t);
        }
        // Keystroke fallback paints (normally cleared by the echo's frame first).
        if let Some(d) = self.key_paint_due {
            merge_wake(&mut wake_at, d);
        }
        // Failed-acquire retries (dropped above for windows not effectively
        // visible / closed, so these only ever wake for a window that can present).
        if let Some(r) = self.acquire_retry {
            merge_wake(&mut wake_at, r.due);
        }
        if let Some(r) = self.settings_acquire_retry {
            merge_wake(&mut wake_at, r.due);
        }
        for dw in &self.detached {
            if let Some(d) = dw.key_paint_due {
                merge_wake(&mut wake_at, d);
            }
            if let Some(r) = dw.acquire_retry {
                merge_wake(&mut wake_at, r.due);
            }
        }

        // `painted`: something above requested a one-off paint — run ONE Poll
        // iteration so macOS delivers it (see the fn doc), then settle.
        let control_flow = if main_pending || settings_pending || detached_pending || painted {
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
        // `jetty --background` (the login autostart entry) creates the window
        // UNMAPPED — no flash at login. The first summon then places, maps and
        // reveals it per `window_mode` through the normal show path, exactly like
        // any later summon (rule F0: never fullscreen while hidden).
        let window = jetty_platform::build_window_with_visibility(
            event_loop,
            "JeTTY",
            (1000, 640),
            !self.start_hidden,
        )
        .expect("create_window failed");
        // Allow IME on the terminal window (winit disables it by default):
        // without this, CJK/complex input methods can never commit text and
        // dead-key composition is degraded. Commits arrive as
        // `WindowEvent::Ime(Ime::Commit)` and are sent to the PTY as typed
        // text; preedit rendering is intentionally not implemented.
        window.set_ime_allowed(true);
        // macOS: which Option side(s) are Meta (no-op elsewhere).
        apply_option_as_alt(&window, self.macos_option_as_alt);
        // First open: place the window per the configured mode. Center mode
        // centers; Dropdown mode docks as a top strip and slides in. A hidden
        // start (`--background`) skips this — its first summon does it.
        if self.start_hidden {
            self.visible = false;
        }
        match self.window_mode {
            _ if self.start_hidden => {}
            WindowMode::Center => center_window(&window),
            WindowMode::Fullscreen => {
                // `build_window` already created AND ordered/mapped the window
                // (winit's default attributes are visible+active), so the "entering
                // fullscreen needs a mapped window" precondition holds here.
                // No dock/center counters, no slide (see `set_main_fullscreen`).
                // Capture the windowed geometry first (this inline enter used to
                // bypass it): an F11 escape right after startup must have a size to
                // centre from, or it centres from the monitor-sized live read and
                // lands in the corner.
                capture_pre_fullscreen(
                    &window,
                    WindowMode::Fullscreen,
                    &mut self.last_pos,
                    &mut self.last_windowed_size,
                );
                self.main_fullscreen = true;
                jetty_platform::set_window_fullscreen(&window, true);
            }
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
            // Chrome bands at THIS window's scale (self.window isn't stored yet).
            let cm = jetty_render::ChromeMetrics::new(scale, self.ui_font_logical);
            let status_h = if self.show_perf_hud { cm.status_h() } else { 0.0 };
            let rows = ((size.height as f32 - cm.bar_h() - status_h) / ch).floor().max(1.0) as usize;
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
            // Built from the grid layer's already-loaded font database: a second
            // fontconfig scan here cost ~15–20ms on the main thread at EVERY cold
            // start, undoing the worker-thread overlap above.
            let fonts = text.as_ref().map_or_else(TextLayer::build_font_system, |t| t.clone_font_system());
            let mut chrome = TextLayer::new_with_family_and_fonts(
                &g.device, &g.queue, g.format, self.ui_font_logical * scale, &self.font_family, fonts,
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
        self.refresh_frame_interval();
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
        // Kitty keyboard protocol: answer `CSI ? u` and track the app's
        // `CSI > u` flag stack — the key path encodes per those flags
        // (`decide_window_key`), so a program that opts in gets kitty keys.
        terminal.set_kitty_keyboard(true);
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
        // feed_notice keeps the interpolated paths inert (no escape sequences).
        for notice in pty.startup_notices() {
            terminal.feed_notice(notice);
        }
        // Config / theme / keybinding problems found at startup, printed where the
        // user will see them (a desktop launch has no visible stderr). Sanitized:
        // the text quotes the user's file, which must never inject escapes here.
        for w in std::mem::take(&mut self.startup_warnings) {
            terminal.feed(format!("\x1b[33mjetty: {}\x1b[0m\r\n", sanitize_notice(&w)).as_bytes());
        }
        let writer = pty.writer();
        let id = self.alloc_tab_id();
        self.tabs.push(Tab {
            id,
            terminal,
            pty,
            writer,
            title: "Tab 1".to_string(),
            default_title: "Tab 1".to_string(),
            manually_renamed: false,
            activity: jetty_render::TabActivity::None,
            pending_inject: None,
            input: input::TabInputState::default(),
        });
        self.active = 0;

        // Register the global summon hotkey (Yakuake-style toggle). The manager
        // must stay alive or the hotkey unregisters when it drops.
        if self.hotkey_manager.is_none() {
            self.start_summon_hotkey();
        }

        // NO periodic heartbeat: every wake source is an event — PTY data/EOF
        // (reader thread), shell exit (waiter thread), F9 (hotkey thread), IPC,
        // config changes (watcher) — and every timer is a WaitUntil deadline in
        // `about_to_wait`. The old 100 ms waker kept the loop at 10 wakes/s
        // forever, hidden or not.

        // Config/theme hot-reload watcher (unless disabled). OS-event-driven, so its
        // thread blocks in the kernel and adds ZERO idle CPU. The returned handle is
        // stored so it lives for the process lifetime (dropping it stops watching).
        if self.hot_reload && self.config_watcher.is_none() {
            let proxy = self.proxy.clone();
            self.config_watcher =
                crate::watch::ConfigWatcher::spawn(crate::config::Config::dir(), move || {
                    // Coalesced app-side (debounced in about_to_wait); a send error
                    // just means the loop is gone (shutting down).
                    let _ = proxy.send_event(AppEvent::ConfigChanged);
                });
        }

        self.request_main_paint();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: AppEvent) {
        match ev {
            AppEvent::Wake => {
                let vt_before = self.vt_bytes;
                let (had_data, chrome_changed, exited) = self.drain_pty();
                let main_flood = self.vt_bytes - vt_before >= FLOOD_PACE_BYTES;
                // Input-latency echo signal (JETTY_PERF_LOG only): the Wake drain is
                // usually where the shell's keystroke echo is consumed (before the
                // redraw re-drains empty), so mark it here too. Gated on `perf.on`;
                // drain_pty/drain_one_tab themselves stay byte-identical.
                if self.perf.on && had_data {
                    self.perf.note_active_output();
                }
                // A tab whose shell exited (Ctrl+D / `exit`) closes THAT tab,
                // Yakuake-style; if it was the last tab, close_exited_tabs exits
                // the loop. The PTY's waiter thread sends this Wake the moment it
                // reaps the shell, so we react at once (no polling tick).
                if !self.close_exited_tabs(exited, event_loop) {
                    return;
                }
                // Output rotated the scrollback under the open search: its
                // stored match Points are stale until the next (throttled)
                // re-collect. Marked HERE too — not just in the render-path
                // drain — because this drain may consume the whole burst,
                // leaving the following RedrawRequested drain empty (F10).
                if had_data {
                    self.ov.note_output();
                }
                // Damage-driven: only request a redraw when the active tab's PTY
                // produced data (or query replies were sent). Background tabs still
                // drained above but don't trigger a repaint. A Wake only ever
                // comes from a PTY event (data, EOF, exit), never from a timer.
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
                    // Flood output paints at most once per refresh (`pace_paint`);
                    // interactive output paints now.
                    match pace_paint(main_flood, self.last_present_at, self.frame_interval, std::time::Instant::now()) {
                        PaintPacing::Now => self.request_main_paint(),
                        PaintPacing::At(t) => {
                            self.paced_paint_at = Some(self.paced_paint_at.map_or(t, |p| p.min(t)));
                        }
                    }
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
                // Run-selection pills from detached-tab drains (a pending rides
                // a detach move); collected locally, surfaced after the loop.
                let mut runsel_notices: Vec<crate::runsel::Notice> = Vec::new();
                let frame_interval = self.frame_interval;
                for (i, dw) in self.detached.iter_mut().enumerate() {
                    let read_before = vt_read;
                    let (had, title_changed, notice) =
                        Self::drain_one_tab(&mut dw.tab, &mut vt_read);
                    // Output rotated the scrollback under this window's open
                    // search: its matches are stale until the next re-collect.
                    if had {
                        dw.ov.note_output();
                    }
                    let flood = vt_read - read_before >= FLOOD_PACE_BYTES;
                    if let Some(n) = notice {
                        runsel_notices.push(n);
                    }
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
                        // Same flood pacing as the main window.
                        match pace_paint(flood, dw.last_present_at, frame_interval, std::time::Instant::now()) {
                            PaintPacing::Now => dw.request_paint(),
                            PaintPacing::At(t) => {
                                dw.paced_paint_at = Some(dw.paced_paint_at.map_or(t, |p| p.min(t)));
                            }
                        }
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
                for n in runsel_notices {
                    self.show_status_pill(n);
                }
                // Remove in descending index order so earlier indices stay valid,
                // mirroring `close_exited_tabs`. Dropping the `DetachedWindow`
                // closes its OS window; its already-exited child is reaped
                // harmlessly by `PtySession::Drop`.
                for i in exited_detached.into_iter().rev() {
                    if i < self.detached.len() {
                        // Same reason as `reattach_tab`: never DROP a fullscreen
                        // window (macOS presentation-options leak).
                        self.exit_detached_fullscreen_bare(i);
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
            AppEvent::ConfigNotice(msg) => self.show_config_warnings(&[msg]),
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
            WindowEvent::Moved(_) => {
                // The only event that can change the Dropdown top-flush answer
                // without a resize; re-derived once on the next frame instead of
                // an X11 round-trip on every frame.
                self.top_flush_dirty = true;
                // The window may now be on a monitor with another refresh rate.
                self.refresh_frame_interval();
            }
            WindowEvent::Resized(size) => {
                self.top_flush_dirty = true;
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
                self.top_flush_dirty = true;
                self.refresh_frame_interval();
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
                self.key_modifiers = m;
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
                self.focus_lost_at = None;
                // A toggle-raise succeeded (or focus came back by itself): the
                // next toggle should hide, not count as a refused raise. This is
                // also the FocusIn that ends the summon hotkey's key grab (on
                // release); clearing here is harmless — the window IS focused,
                // so the next toggle hides either way.
                self.raise_attempt_at = None;
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
                // Stamped for the toggle's churn grace: on X11 pressing the
                // summon hotkey itself sends this FocusOut (its key grab) just
                // before the hotkey event — see `FOCUS_CHURN_GRACE`.
                self.focus_lost_at = Some(std::time::Instant::now());
                // A selection / scrollbar drag, buttons the program saw pressed
                // and the edge auto-scroll can't see their release once focus is
                // gone — end them so nothing resumes stuck (detached parity, F14).
                self.reset_main_pointer();
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
                // --- Resize-edge cursor feedback (borderless window) ---
                // Only update the cursor when the zone changes, never while a host
                // drag (scrollbar / selection) is in progress, and never while a
                // modal (confirm / help / context menu) is open — a press there is
                // consumed by the modal, so a resize-edge cursor under it is wrong.
                let modal_open = self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.ov.help_open
                    || self.context_menu.is_some()
                    || self.tab_menu.is_some();
                if !self.dragging_scrollbar
                    && !self.selecting
                    && !modal_open
                    && self.tab_drag.is_none()
                {
                    if let Some(gpu) = &self.gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        // Inert while fullscreen: the press site below is gated
                        // too, so a ⤡/↔ cursor over a fullscreen edge would
                        // advertise an affordance that does nothing (the main-window
                        // twin of the detached hover site, amendment I-D). Resolving
                        // to `None` also RESETS a cursor left over from before the
                        // transition.
                        let zone = if self.main_fullscreen {
                            ResizeZone::None
                        } else {
                            resize_zone_at(position.x as f32, position.y as f32, w, h, self.chrome_metrics().dpi)
                        };
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
                    let cm = self.chrome_metrics();
                    let before = ctrl_hover_at(prev.0 as f32, prev.1 as f32, w, bar_y, cm);
                    let after = ctrl_hover_at(position.x as f32, position.y as f32, w, bar_y, cm);
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
                    // The tear-out band scales with the chrome (a fixed 24px was a
                    // third of a 72px bar on a 2× display).
                    let cm = self.chrome_metrics();
                    let now_tearing = crate::detached::tearing(
                        position.y as f32,
                        bar_y,
                        cm.bar_h(),
                        cm.px(crate::detached::TEAR_THRESHOLD_PX),
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
                // --- Grid pointer motion (shared with detached windows) ---
                // Extends a local selection drag (and arms the edge auto-scroll
                // past the grid's top/bottom), or reports the motion to a program
                // that tracks it (1002 while one of its buttons is held, 1003
                // always), once per cell. Host drags (scrollbar, tab) own the
                // pointer meanwhile.
                if !self.dragging_scrollbar && self.tab_drag.is_none() {
                    let now = std::time::Instant::now();
                    if self.with_main_grid(|g| crate::gridmouse::motion(g, now)).is_some_and(|m| m.paint) {
                        self.request_main_paint();
                    }
                }
                // --- Context menu hover update ---
                // Reuse the cached item_rects (built when the menu opened) instead
                // of rebuilding the whole menu on every (high-frequency) move.
                if self.context_menu.is_some() {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let new_hover = self
                        .menu_item_rects
                        .iter()
                        .position(|r| {
                            cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                        })
                        // Disabled (grayed) rows are inert: no hover state.
                        .filter(|i| !self.menu_disabled.contains(i));
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

                // --- Hint mode / copy-mode, then the command palette (which
                // captures the mouse while open) — shared with the detached
                // windows ---
                if self.modes_click(Surface::Main) {
                    return;
                }
                if self.palette_click(Surface::Main, self.cursor.0 as f32, self.cursor.1 as f32, event_loop) {
                    return;
                }

                // --- Quit confirmation popup is modal (highest priority) ---
                if self.confirm_quit {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let theme = self.current_theme();
                    let cm = self.chrome_metrics();
                    let mut fallback = mono_fallback(cm);
                    let popup = jetty_render::build_confirm(
                        w, h, "Quit JeTTY? — all tabs will close", &theme,
                        measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                    );
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
                if let Some(id) = self.confirm_close {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let target = self.tab_index(id);
                    let title = target.map(|i| self.tabs[i].title.clone()).unwrap_or_default();
                    let theme = self.current_theme();
                    let cm = self.chrome_metrics();
                    let mut fallback = mono_fallback(cm);
                    let popup = jetty_render::build_confirm_close(
                        w, h, &title, &theme, measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                    );
                    if input::point_in(&popup.close_rect, cx, cy) {
                        self.confirm_close = None;
                        if let Some(i) = target {
                            self.close_tab(i, event_loop);
                        }
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
                if let Some((_, _, tab_id)) = self.tab_menu.take() {
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
                    if let Some(tab_idx) = self.tab_index(tab_id) {
                        match label {
                            Some("Detach") => {
                                // Same flow as Ctrl+Shift+D, for THAT tab.
                                self.detach_tab(tab_idx, event_loop, None);
                            }
                            Some("Rename") => {
                                // Same inline-rename flow as double-click.
                                self.renaming = Some(tab_id);
                                self.rename_buf = self.tabs[tab_idx].title.clone();
                            }
                            Some("Close Tab") => {
                                // Same confirm-close flow as the × / Ctrl+Shift+W.
                                self.confirm_close = Some(tab_id);
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
                    // A click on a DISABLED (grayed) row is a no-op that still
                    // closes the menu — the "click anywhere closes" contract.
                    let hit = hit.filter(|i| !self.menu_disabled.contains(i));
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
                                // Run in New Tab — the browser gesture: the
                                // selection runs in a fresh tab at this tab's cwd.
                                self.run_selection_in_new_tab(SelSource::Main);
                            }
                            3 => {
                                // Select All
                                self.active_tab_mut().terminal.select_all();
                            }
                            4 => {
                                // Clear — emulates Ctrl+L (form-feed 0x0C) sent to the active PTY.
                                // This is the same byte the Ctrl+L keybinding produces via
                                // ctrl_byte('L') in input.rs; reuse the same writer path.
                                // A user-originated PTY byte — cancels a staged
                                // run-selection inject (same rule as write_key_to_pty).
                                crate::runsel::cancel_on_user_write(
                                    &mut self.tabs[self.active].pending_inject,
                                );
                                self.active_tab_mut().terminal.scroll_to_bottom();
                                let w = &mut self.tabs[self.active].writer;
                                let _ = w.write_all(&[0x0C]);
                                let _ = w.flush();
                            }
                            5 => {
                                // Close Tab — mirrors the Ctrl+Shift+W handler: set confirm_close
                                // to open the confirmation popup (or close directly if no child).
                                // This reuses the exact same flow as KeyAction::CloseTab.
                                self.confirm_close = self.tabs.get(self.active).map(|t| t.id);
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

                // --- Help (modal) and the search bar (✕ / panel) — shared with
                // the detached windows ---
                if self.help_click(Surface::Main, cx, cy) || self.search_bar_click(Surface::Main, cx, cy) {
                    return;
                }

                // --- Resize edges (borderless window): highest priority after the
                // modal context menu. Corners > edges; a press in a resize zone
                // starts an OS-driven resize and consumes the click so it never
                // begins a selection, tab-bar drag, or window move. ---
                // Inert while fullscreen: `drag_resize_window` on a fullscreen X11
                // window leaves it in a broken half-state.
                let zone = if self.main_fullscreen {
                    ResizeZone::None
                } else {
                    resize_zone_at(cx, cy, w, h, self.chrome_metrics().dpi)
                };
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
                if cy >= bar_y && cy < bar_y + self.bar_h() {
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
                    // Field-level borrows (the builder below also borrows the chrome text).
                    let rename_ref = self
                        .renaming
                        .and_then(|id| self.tabs.iter().position(|t| t.id == id))
                        .map(|i| (i, self.rename_buf.as_str()));
                    // Build with perf=None to MATCH the drawn bar (the perf HUD
                    // moved to the bottom status strip, so the drawn tab bar
                    // reserves no HUD width — see the RedrawRequested build). Passing
                    // self.perf_label here reserved ~250px of phantom width and
                    // shrank the hit tab_w below the drawn tab_w, so clicks near a
                    // tab's right edge / on ✕ / on + landed on the wrong tab (F19).
                    let cm = self.chrome_metrics();
                    let mut fallback = mono_fallback(cm);
                    let mut bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None, None,
                        measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                        &[], // activity never affects geometry; keeps hit rects == drawn rects
                    );
                    // build_tab_bar_ex lays the bar out at y 0..bar_h; shift its
                    // hit-test rects down to the bar's actual position (bottom mode).
                    if bar_y != 0.0 {
                        translate_bar_rects(&mut bar, bar_y);
                    }

                    // Window controls take priority (rightmost region).
                    if input::point_in(&bar.help_rect, cx, cy) {
                        // Toggle the in-window Help overlay. Opening it closes the
                        // context menu so the two overlays are mutually exclusive.
                        self.toggle_help(Surface::Main);
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
                        // ▢ while FULLSCREEN unambiguously means "give me my window
                        // back": leave fullscreen ONLY — never `set_maximized`, which
                        // on a fullscreen X11 window is a no-op or a stuck half-state.
                        // Maximize and fullscreen stay orthogonal booleans, so
                        // maximize → F11 → ▢ hands back a MAXIMIZED window (the exit
                        // skips the geometry restore while `is_maximized()`, because a
                        // position set on a maximized X11 window is ignored or
                        // half-applied); a second ▢ then normalises it (amendment I-F).
                        if self.main_fullscreen {
                            self.set_main_fullscreen(false);
                        } else if let Some(win) = &self.window {
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
                    let renaming_id = self.renaming;

                    // Close buttons take priority over the tab body they sit on.
                    if let Some(i) = bar
                        .close_rects
                        .iter()
                        .position(|r| input::point_in(r, cx, cy))
                    {
                        self.commit_rename();
                        // Ask before closing instead of closing immediately.
                        self.confirm_close = self.tabs.get(i).map(|t| t.id);
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
                        let tab_id = self.tabs[i].id;
                        if is_double && self.renaming != Some(tab_id) {
                            self.renaming = Some(tab_id);
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
                        if renaming_id != Some(tab_id) {
                            self.commit_rename();
                        }
                        // Select immediately (a plain click), and ARM the
                        // drag-out gesture: if the cursor leaves the strip by
                        // more than TEAR_THRESHOLD_PX before release, the drag
                        // becomes a tear-out and the release detaches this tab.
                        self.select_tab(i);
                        self.tab_drag = Some(TabDrag { tab: tab_id, tearing: false });
                        return;
                    }

                    // Empty strip space: commit any rename, then either maximize
                    // (double-click) or start an OS window move (single press).
                    self.commit_rename();
                    if is_double {
                        self.last_strip_click = None;
                        // Same rule as the ▢ button above (amendment I-F).
                        if self.main_fullscreen {
                            self.set_main_fullscreen(false);
                        } else if let Some(win) = &self.window {
                            win.set_maximized(!win.is_maximized());
                        }
                    } else if !self.main_fullscreen {
                        // Dragging a fullscreen window leaves it in a broken
                        // half-state on X11 — the move gesture is inert.
                        if let Some(win) = &self.window {
                            let _ = win.drag_window();
                        }
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
                        // The click landed in the terminal area (not a widget):
                        // the shared grid press (detached windows run the same).
                        // A link-modifier click on a link opens it; a program that
                        // tracks the mouse gets the press (with modifier bits);
                        // otherwise a selection starts — by cell, word (double
                        // click) or line (triple click). Shift always selects: the
                        // terminal convention for copying out of mouse-grabbing
                        // programs (Claude Code, vim, htop, tmux).
                        let link_mod = link_modifier_held(&self.modifiers);
                        let now = std::time::Instant::now();
                        match self.with_main_grid(|g| {
                            crate::gridmouse::press(g, MouseButton::Left, link_mod, now)
                        }) {
                            Some(crate::gridmouse::Press::OpenLink(uri)) => Self::open_url(&uri),
                            Some(crate::gridmouse::Press::Selecting) => self.request_main_paint(),
                            _ => {}
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
                // Hint mode is keyboard-only: a menu opened there could never be
                // clicked (its left press is swallowed by the mode).
                if self.slide_anim.is_some()
                    || self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.ov.help_open
                    || self.ov.palette_open
                    || self.ov.hint_mode.is_some()
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
                if cy >= bar_y && cy < bar_y + self.bar_h() {
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
                    let cm = self.chrome_metrics();
                    let mut fallback = mono_fallback(cm);
                    // Field-level borrows (the builder below also borrows the chrome text).
                    let rename_ref = self
                        .renaming
                        .and_then(|id| self.tabs.iter().position(|t| t.id == id))
                        .map(|i| (i, self.rename_buf.as_str()));
                    // perf=None to match the DRAWN bar (HUD lives in the status
                    // strip); passing perf_label mis-sized the hit-rects (F19).
                    let mut bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None, None,
                        measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
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
                        self.ov.help_open = false;
                        self.context_menu = None;
                        self.menu_hover = None;
                        self.tab_menu = Some((cx, cy, self.tabs[i].id));
                        self.tab_menu_hover = None;
                        self.tab_menu_labels = crate::detached::tab_menu_items(
                            crate::detached::can_detach(self.tabs.len()),
                        );
                        // Cache the item hit-test rects once, same as context_menu.
                        // Hints from the live keymap (same strings the draw uses).
                        let hints: Vec<String> = self
                            .tab_menu_labels
                            .iter()
                            .map(|&l| crate::detached::menu_hint(&self.keymap, l))
                            .collect();
                        let items: Vec<(&str, &str)> = self
                            .tab_menu_labels
                            .iter()
                            .zip(&hints)
                            .map(|(&l, h)| (l, h.as_str()))
                            .collect();
                        let menu = jetty_render::build_menu(
                            cx, cy, w, h, None, &theme,
                            measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                            &items, &[], &[],
                        );
                        self.tab_menu_rects = menu.item_rects;
                        self.request_main_paint();
                    }
                    return;
                }
                // On the grid, a program that tracks the mouse gets the click
                // (nvim / tmux / mc menus) — unless Shift is held or JeTTY holds
                // a selection, which keep JeTTY's menu (shared with detached).
                if self.main_grid_geom().contains_y(cy) {
                    let now = std::time::Instant::now();
                    if self.with_main_grid(|g| crate::gridmouse::press(g, MouseButton::Right, false, now))
                        == Some(crate::gridmouse::Press::Reported)
                    {
                        if let Some(id) = self.window.as_ref().map(|w| w.id()) {
                            self.teach_shift_right_click(id);
                        }
                        return;
                    }
                }
                // Commit any in-progress rename and close the help overlay so the
                // menu can't be orphaned under it. The tab menu is mutually
                // exclusive with the terminal menu.
                self.commit_rename();
                self.ov.help_open = false;
                self.tab_menu = None;
                self.tab_menu_hover = None;
                self.tab_menu_rects.clear();
                self.tab_menu_labels.clear();
                self.context_menu = Some((cx, cy));
                self.menu_hover = None;
                // Disabled rows, computed once at open: "Copy" (0) and "Run in
                // New Tab" (2) share the needs-a-selection property (Copy
                // silently no-ops without one today — dimming is the honest UI
                // for the same property); Run additionally dims when the
                // feature is config-disabled.
                let has_sel = self
                    .active_tab()
                    .terminal
                    .selection_text()
                    .is_some_and(|t| !t.is_empty());
                self.menu_disabled = match (has_sel, self.run_selection_enabled) {
                    (false, _) => vec![0, 2],
                    (true, false) => vec![2],
                    (true, true) => Vec::new(),
                };
                // THE teachable moment for mouse-grabbing apps (Claude Code,
                // vim, htop): the user Shift+right-clicked wanting Copy / Run in
                // New Tab, but their drag was forwarded to the app, so there is
                // no selection and both rows sit dimmed with no explanation.
                // Surface the Shift+drag hint alongside the menu — deliberately
                // BYPASSING the 25s drag-cooldown: an explicit right-click on
                // dimmed rows is a direct question, not a nag.
                let tracking = crate::gridmouse::tracking(&self.active_tab().terminal);
                if !has_sel && tracking != input::MouseTracking::Off {
                    if let Some(id) = self.window.as_ref().map(|w| w.id()) {
                        arm_shift_hint(&mut self.shift_hint_until, &mut self.shift_hint_cooldown, id, true);
                    }
                }
                // Cache the item hit-test rects once (anchor + size fixed for the
                // menu's lifetime) so CursorMoved hover doesn't rebuild the menu.
                if let Some(gpu) = &self.gpu {
                    let (w, h) = (gpu.config.width, gpu.config.height);
                    let theme = self.current_theme();
                    let cm = self.chrome_metrics();
                    let mut fallback = mono_fallback(cm);
                    let hints = crate::detached::context_menu_hints(&self.keymap);
                    let hint_refs: Vec<&str> = hints.iter().map(String::as_str).collect();
                    let menu = jetty_render::build_context_menu(
                        cx, cy, w, h, None, &theme,
                        measure_or(self.chrome_text.as_mut(), &mut fallback), cm,
                        &hint_refs, &self.menu_disabled,
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
                        // The dragged tab by identity: a tab that closed
                        // mid-drag (shell exit) detaches nothing — never its
                        // neighbour.
                        if let Some(idx) = self.tab_index(drag.tab) {
                            self.detach_tab(idx, event_loop, drop_global);
                        }
                        return;
                    }
                }
                // If we were dragging the scrollbar, the release just ends that
                // drag and is never forwarded to the app. (Slider drags happen in
                // the settings window now.)
                if std::mem::take(&mut self.dragging_scrollbar) {
                    return;
                }
                // The shared grid release (detached windows run the same): a
                // selection drag ends with copy-on-select to the PRIMARY
                // selection (an empty click clears the highlight); a press that
                // went to the program gets its release — never a phantom one for
                // a press chrome consumed. A no-Shift DRAG that went to a
                // mouse-grabbing program means the user was probably trying to
                // select: teach Shift+drag (throttled).
                match self.with_main_grid(|g| crate::gridmouse::release(g, MouseButton::Left)) {
                    Some(crate::gridmouse::Release::Copy(text)) => {
                        clipboard::copy_on_select(&text, self.copy_on_select);
                        self.request_main_paint();
                    }
                    Some(crate::gridmouse::Release::Cleared) => self.request_main_paint(),
                    Some(crate::gridmouse::Release::Program { dragged: true }) => {
                        // Tagged with the MAIN window's id: only this window draws
                        // the pill (F4).
                        if let Some(id) = self.window.as_ref().map(|w| w.id()) {
                            if arm_shift_hint(&mut self.shift_hint_until, &mut self.shift_hint_cooldown, id, false) {
                                self.request_main_paint();
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Middle, .. } => {
                // Middle-click paste (X11 primary-selection idiom). Unlike the
                // Left arm this used to skip every gate, so it pasted into a
                // shell hidden behind a modal popup and over the tab bar. Honor
                // the same modal/hit checks:
                //  - any modal open (slide/confirm/help/menus) → swallow;
                //  - only over the terminal grid, never the chrome strips;
                //  - when a program tracks the mouse and Shift is not held, the
                //    button belongs to it — report it instead of pasting.
                // It pastes the PRIMARY selection (the text last selected
                // anywhere), not the clipboard.
                if self.slide_anim.is_some()
                    || self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.ov.help_open
                    || self.ov.palette_open
                    || self.ov.hint_mode.is_some()
                    || self.context_menu.is_some()
                    || self.tab_menu.is_some()
                    || self.tabs.is_empty()
                    || self.gpu.is_none()
                {
                    return;
                }
                if !self.main_grid_geom().contains_y(self.cursor.1 as f32) {
                    return;
                }
                let now = std::time::Instant::now();
                if self.with_main_grid(|g| crate::gridmouse::press(g, MouseButton::Middle, false, now))
                    == Some(crate::gridmouse::Press::PastePrimary)
                {
                    if let Some(text) = clipboard::get_primary() {
                        self.paste_text(&text);
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: button @ (MouseButton::Middle | MouseButton::Right | MouseButton::Back | MouseButton::Forward),
                ..
            } => {
                // The rest of the buttons a program can receive: the release of a
                // middle/right press that went to it (their presses have their
                // own arms above), and back/forward presses + releases over the
                // grid. Shared with detached windows.
                let now = std::time::Instant::now();
                if state == ElementState::Released {
                    self.with_main_grid(|g| crate::gridmouse::release(g, button));
                } else if !self.tabs.is_empty()
                    && self.gpu.is_some()
                    && self.main_grid_geom().contains_y(self.cursor.1 as f32)
                    && !(self.confirm_quit
                        || self.confirm_close.is_some()
                        || self.ov.help_open
                        || self.ov.palette_open
                        || self.ov.hint_mode.is_some()
                        || self.context_menu.is_some()
                        || self.tab_menu.is_some())
                {
                    self.with_main_grid(|g| crate::gridmouse::press(g, button, false, now));
                }
            }
            WindowEvent::DroppedFile(path) => {
                // A file dropped on the terminal types its shell-quoted path into
                // the active tab (through the paste path: sanitized, bracketed
                // when the program asked for it).
                if !self.tabs.is_empty() {
                    let text = crate::gridmouse::dropped_path_text(&path);
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
                // The help overlay (while its rows overflow) and the palette
                // own the wheel too (shared with the detached windows).
                if self.overlay_wheel(Surface::Main, delta) {
                    return;
                }
                // The shared grid wheel (detached windows run the same). Deltas
                // ACCUMULATE fractionally across events (slow touchpad scrolling
                // is many sub-line deltas). A program that tracks the mouse gets
                // wheel reports — vertical and horizontal, one per notch — except
                // over the scrollbar or with Shift held, which always scroll the
                // host scrollback; an alternate-screen pager without tracking
                // gets arrow keys (ALTERNATE_SCROLL, F3); otherwise the host
                // scrollback moves.
                let over_scrollbar = {
                    let t = &self.active_tab().terminal;
                    let (rows, off, max) = (t.rows(), t.scroll_offset(), t.scroll_max());
                    self.gpu.as_ref().is_some_and(|gpu| {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        jetty_render::scrollbar_rect_geom(
                            rows, off, max, w, h, self.grid_top_offset(), self.status_h(), [0, 0, 0, 0],
                        )
                        .is_some_and(|r| {
                            let cx = self.cursor.0 as f32;
                            cx >= r.x && cx <= r.x + r.w
                        })
                    })
                };
                let mut vertical = std::mem::take(&mut self.scroll_accum);
                let outcome = self.with_main_grid(|g| {
                    crate::gridmouse::wheel(g, delta, over_scrollbar, &mut vertical)
                });
                self.scroll_accum = vertical;
                match outcome {
                    // Arrow keys are user input: cancel a staged run-selection
                    // inject (an alt screen can't be a fresh prompt, but the rule
                    // is uniform).
                    Some(crate::gridmouse::Wheel::Arrows) => {
                        crate::runsel::cancel_on_user_write(&mut self.tabs[self.active].pending_inject);
                    }
                    Some(crate::gridmouse::Wheel::Scrolled) => {
                        self.request_main_paint();
                        // The viewport moved under a stationary pointer: the
                        // hovered CELL is unchanged but its content is not.
                        self.update_link_hover(true);
                    }
                    _ => {}
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
                if let Some(id) = self.confirm_close {
                    use winit::keyboard::{Key, NamedKey};
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.confirm_close = None;
                            if let Some(i) = self.tab_index(id) {
                                self.close_tab(i, event_loop);
                            }
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
                if self.renaming.is_some() {
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
                    return;
                }
                // --- The window's modal overlays own the keyboard: command
                // palette, then hint mode, then copy-mode (shared with the
                // detached windows) ---
                if self.overlay_key_modal(Surface::Main, &event, event_loop) {
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
                // --- Help (Esc + its scroll keys) and the scrollback-search bar
                // (every key while open), shared with the detached windows ---
                if self.overlay_key_bars(Surface::Main, &event) {
                    return;
                }
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                // The shared key decision (keymap chords incl. the folded macOS
                // Cmd chords and their "swallow unmapped Cmd" net → kitty
                // keyboard protocol → macOS Option-compose → dead keys → legacy
                // xterm encoders), against the ACTIVE tab's modes. The detached
                // windows call the same helper. Escape in the main window never
                // closes the Settings window (that window handles its own
                // Escape), so Escape forwards an ESC byte as normal.
                let action = decide_window_key(
                    &self.keymap,
                    &event,
                    &self.key_modifiers,
                    &self.active_tab().terminal,
                    self.macos_option_as_alt,
                );
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
                        input::KeyAction::RunSelection => "RunSelection",
                        input::KeyAction::ToggleFullscreen => "ToggleFullscreen",
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
                        self.confirm_close = self.tabs.get(self.active).map(|t| t.id);
                        self.request_main_paint();
                    }
                    input::KeyAction::DetachTab => {
                        self.detach_tab(self.active, event_loop, None);
                    }
                    input::KeyAction::OpenPalette => {
                        self.open_palette(Surface::Main);
                    }
                    input::KeyAction::SearchToggle => {
                        self.search_toggle(Surface::Main);
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
                        self.enter_hint_mode(Surface::Main);
                    }
                    input::KeyAction::CopyMode => {
                        self.enter_copy_mode(Surface::Main);
                    }
                    // Ctrl+Shift+Enter — run the current selection in a new tab
                    // (no-op without a selection; every trigger funnels through
                    // the same method).
                    input::KeyAction::RunSelection => {
                        self.run_selection_in_new_tab(SelSource::Main);
                    }
                    // F11 (macOS also Cmd+Ctrl+F) — toggle OS fullscreen on THIS
                    // (the main) window. Transient: nothing is persisted, so the
                    // hot key path stays free of disk I/O.
                    //
                    // DECISION (amendment I-G): F11 is deliberately NOT special-
                    // cased in the modal short-circuits above. Whichever overlay
                    // owns the keyboard — palette, search, hint mode, copy-mode,
                    // the close/quit confirmations, inline rename, and the
                    // Settings window (which handles only Escape) — swallows F11
                    // exactly like every other binding. The
                    // single-overlay-owns-keys discipline is load-bearing; carving
                    // out one action would fork it for no real gain (close the
                    // overlay with Esc, then press F11).
                    input::KeyAction::ToggleFullscreen => {
                        self.set_main_fullscreen(!self.main_fullscreen);
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
                        // Any real keystroke jumps back to the bottom so the user
                        // sees their input, then writes to the PTY (shared input
                        // core, v0.23 Task 9).
                        write_key_to_pty(self.active_tab_mut(), &bytes, Some(event.physical_key));
                        // Input-latency START stamp (JETTY_PERF_LOG only): record the
                        // keystroke instant so the frame that reflects its echo can
                        // measure keypress→glyph. Gated on `perf.on` (a bool read once
                        // at startup) → the default path pays one predictable-false
                        // branch, no Instant::now(). Arms only at a quiescent prompt
                        // (main window). See crate::perf.
                        if self.perf.on {
                            self.perf.note_key_send();
                        }
                        // NO paint here: the shell's echo (drained a few ms later)
                        // paints the frame that shows this key — painting now would
                        // render a pre-echo frame that the echo frame then queues a
                        // vsync behind. A key that never echoes (password prompt,
                        // the snap back from scrollback) paints at the fallback
                        // deadline in `about_to_wait`.
                        let now = std::time::Instant::now();
                        self.key_paint_due = Some(now + KEY_ECHO_GRACE);
                        // Trigger caret flash+pulse on printable keystrokes.
                        // Arm the shared burst clock when EITHER caret effect is on;
                        // each consumer is independently gated on its own toggle.
                        // The burst starts pumping frames once its first frame
                        // (the echo's) is painted — see `caret_drives_frames`.
                        if (self.fx.caret_flash_enabled || self.fx.caret_glow_enabled)
                            && is_printable_keystroke(&bytes)
                        {
                            self.caret_anim = Some(now);
                        }
                    }
                    input::KeyAction::None => {}
                }
            }
            // Key RELEASE: owed only to the main-window tab that was sent the
            // press (kitty keyboard protocol event types; a no-op otherwise) —
            // even if the user switched tabs meanwhile. Synthetic releases
            // (keys still held when focus leaves) are forwarded too, so a
            // program never sees a key stuck down.
            WindowEvent::KeyboardInput { event, .. } => {
                let (keymap, mods, opt) = (&self.keymap, self.key_modifiers, self.macos_option_as_alt);
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.input.holds(event.physical_key)) {
                    write_key_release(keymap, tab, &event, &mods, opt);
                }
            }
            // IME composition in progress: shown at the cursor (`ime_preedit`)
            // until it commits; an empty preedit / Disabled ends it.
            WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) => {
                let next = (!text.is_empty()).then_some(text);
                if self.ime_preedit != next {
                    self.ime_preedit = next;
                    self.request_main_paint();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                if self.ime_preedit.take().is_some() {
                    self.request_main_paint();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // IME commit (CJK input methods, dead-key composition routed
                // through the IME). It must honor the SAME modal priority chain
                // as KeyboardInput, or composed text leaks into the shell behind
                // a rename box / confirm popup (CJK users could not type non-ASCII
                // tab names at all). The commit ends any composition on screen.
                if self.ime_preedit.take().is_some() {
                    self.request_main_paint();
                }
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
                if self.ov.hint_mode.is_some() || self.ov.copy_mode.is_some() {
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
                // The command palette, then the scrollback-search bar, capture
                // IME commits into their query (mirrors the KeyboardInput arms;
                // same modal priority so composed text never leaks behind an
                // overlay — CJK users can type queries). Shared with the
                // detached windows.
                if self.overlay_ime_commit(Surface::Main, &text) {
                    return;
                }
                if self.welcome_open {
                    self.welcome_open = false;
                }
                // Same discipline as the Send arm: jump to the live bottom then
                // write to the PTY (shared input core, v0.23 Task 9) — and, like
                // it, let the echo paint (fallback deadline, no pre-echo frame).
                write_key_to_pty(self.active_tab_mut(), text.as_bytes(), None);
                let now = std::time::Instant::now();
                self.key_paint_due = Some(now + KEY_ECHO_GRACE);
                if (self.fx.caret_flash_enabled || self.fx.caret_glow_enabled)
                    && is_printable_keystroke(text.as_bytes())
                {
                    self.caret_anim = Some(now);
                }
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
                // This frame shows the latest keystroke's effect (echo or not), so
                // its fallback paint is no longer owed; the caret burst may now
                // pump frames (`caret_drives_frames`).
                self.key_paint_due = None;
                // Auto-exit hint/copy-mode if a program switched to the alt screen
                // while a mode was active (shared with the detached windows).
                self.exit_modes_on_alt_screen(Surface::Main);
                // Re-assert the Dropdown dock AFTER the window is mapped: X11/KWin
                // ignores a set_outer_position issued before the window is realized
                // (it would land centered), so re-apply the top-strip geometry on
                // the first few post-map redraws. Counts down → idle CPU back to 0.
                if self.pending_dock_frames > 0
                    && dock_reassert_ok(self.window_mode, self.main_fullscreen)
                {
                    self.pending_dock_frames -= 1;
                    if let Some(win) = &self.window {
                        dock_window_top(win, self.dropdown_width_pct, self.dropdown_height_pct);
                        if self.pending_dock_frames > 0 {
                            win.request_redraw();
                        }
                    }
                } else if self.pending_dock_frames > 0 {
                    // Mode switched away from Dropdown, or we went fullscreen, mid-
                    // countdown — stop docking. Zeroing here (rather than leaving the
                    // counter set) is what keeps the ~0%-idle invariant: `main_pending`
                    // selects Poll while either counter is non-zero.
                    self.pending_dock_frames = 0;
                }
                // Center-mode position re-assertion (see pending_center_frames).
                if self.pending_center_frames > 0
                    && center_reassert_ok(self.window_mode, self.main_fullscreen)
                {
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
                if had {
                    self.ov.note_output();
                }
                self.refresh_search_if_due(Surface::Main, std::time::Instant::now());
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
                // The IME candidate window follows the cursor cell (handed to
                // winit only when that cell moves, not every frame).
                let ime_area = self.text.as_ref().map(|t| {
                    let (cw, ch) = t.cell_size();
                    input::ime_cursor_area(snap.cursor_row, snap.cursor_col, cw, ch, self.grid_top_offset())
                });
                if let Some(area) = ime_area {
                    if self.ime_area != Some(area) {
                        self.ime_area = Some(area);
                        if let Some(w) = &self.window {
                            w.set_ime_cursor_area(
                                winit::dpi::PhysicalPosition::new(area.0, area.1),
                                winit::dpi::PhysicalSize::new(area.2, area.3),
                            );
                        }
                    }
                }
                // The IME composition, drawn at the terminal cursor — unless a
                // modal / rename box / palette / search / hint / copy-mode owns
                // the keyboard (their own fields receive the commit).
                let preedit_ui: Option<String> = if self.confirm_quit
                    || self.confirm_close.is_some()
                    || self.renaming.is_some()
                    || self.ov.palette_open
                    || self.ov.search_open
                    || self.ov.hint_mode.is_some()
                    || self.ov.copy_mode.is_some()
                {
                    None
                } else {
                    self.ime_preedit.clone()
                };
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
                    Some(PERF_IDLE_TEXT.to_string())
                } else {
                    let s = self.update_perf_hud();
                    if self.show_perf_hud {
                        // (Re)arm the one-shot idle repaint for ~700ms after this
                        // active frame; cleared/rescheduled by the next active frame.
                        self.perf_idle_at = Some(std::time::Instant::now() + PERF_IDLE_AFTER);
                        self.perf_idle_shown = false;
                    }
                    s
                };
                let context_menu = self.context_menu;
                let menu_hover = self.menu_hover;
                // Disabled rows computed at menu open (cheap clone of ≤2 idx).
                let menu_disabled = self.menu_disabled.clone();
                let tab_menu = self.tab_menu;
                let tab_menu_hover = self.tab_menu_hover;
                let tab_menu_labels = self.tab_menu_labels.clone();
                let help_open = self.ov.help_open;
                let help_scroll = self.ov.help_scroll;
                // Clone the (cached, keymap-derived) help rows only when the overlay
                // is actually open — keeps the hot render path allocation-free.
                let help_rows: Vec<String> =
                    if help_open { self.help_rows.clone() } else { Vec::new() };
                // Search bar draw data + visible match highlights, captured
                // before the mutable gpu/text borrow. Both empty/None while
                // the bar is closed (one bool branch on the hot path).
                let (search_ui, search_hits) = self.search_draw(Surface::Main);
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
                let palette_ui: Option<PaletteDrawData> = self.ov.palette_draw();
                let welcome_open = self.welcome_open;
                // Hint-mode chips: (label, first-span row, first-span col) for
                // each token whose label still matches the typed prefix, captured
                // OWNED before the mutable gpu/text borrow. None while inactive
                // (one Option test on the hot path, zero allocation).
                let hint_ui: Option<HintDrawData> = self.ov.hint_draw();
                // Copy-mode cursor + pill: (row, col, selecting, line_mode). None
                // while inactive; `copy_mode_active` suppresses the shell cursor.
                let copy_mode_ui: Option<(usize, usize, bool, bool)> = self.ov.copy_draw();
                let copy_mode_active = copy_mode_ui.is_some();
                // Pill only when the hint is live AND belongs to THIS (the
                // main) window — a detached-window drag must not light it
                // here (F4).
                let shift_hint_show = self.window.as_ref().is_some_and(|w| {
                    shift_hint_live_in(self.shift_hint_until, w.id(), std::time::Instant::now())
                });
                // Run-selection status pill (refusal / staged feedback) — same
                // window-tagged liveness rule as the shift hint above.
                let status_pill_msg: Option<String> = self.window.as_ref().and_then(|w| {
                    self.status_pill.as_ref().and_then(|(m, until, wid)| {
                        (*wid == w.id() && std::time::Instant::now() < *until)
                            .then(|| m.clone())
                    })
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
                    .and_then(|id| self.tab_index(id))
                    .map(|i| self.tabs[i].title.clone());
                let rename_state: Option<(usize, String)> =
                    self.rename_ref().map(|(i, buf)| (i, buf.to_string()));
                // Corner-mask inputs captured before the mutable render borrows.
                // The radius is logical px; scale to physical so it matches the
                // physical-pixel surface (HiDPI-correct rounding).
                let scale = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
                // Fullscreen ⇒ 0 (flat): a rounded fullscreen window would show the
                // desktop through four notches at the screen edges. All four radii 0
                // makes `CornerMask::apply` early-return, so the final mask pass is
                // SKIPPED entirely, and the CRT shader treats 0 as square.
                let corner_radius_px =
                    effective_corner_radius_px(self.corner_radius, scale, self.main_fullscreen);
                // In Dropdown mode the window is flush to the monitor top, so the
                // TOP corners must stay square (only the bottom corners round).
                // Derive "top-flush" from the window's outer position vs the
                // monitor top. On Wayland outer_position() is Err → not flush, so
                // we keep all-4 rounding (accepted degradation, no DE code).
                // The position test is a BLOCKING X11 round-trip, so it runs only
                // when the window may have moved (`top_flush_dirty`: Moved /
                // Resized / ScaleFactorChanged / summon) — never per frame, and
                // never mid-slide (the slide is a content y-offset; the window
                // itself is stationary). Fullscreen short-circuits it: every radius
                // is already 0 there, so the answer is moot.
                if self.window_mode == WindowMode::Dropdown
                    && !self.main_fullscreen
                    && self.top_flush_dirty
                    && self.slide_anim.is_none()
                {
                    self.top_flush_dirty = false;
                    self.top_flush_pos = self
                        .window
                        .as_ref()
                        .and_then(|w| {
                            let p = w.outer_position().ok()?;
                            let mon = jetty_platform::monitor_for_window(w)?;
                            Some(p.y <= mon.position().y + 1)
                        })
                        .unwrap_or(false);
                }
                let top_flush = self.window_mode == WindowMode::Dropdown
                    && !self.main_fullscreen
                    && self.top_flush_pos;
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
                // Summon progress: t in [0,1) drives a reveal pass this frame
                // (`about_to_wait` pumps the next one); t>=1 ends the animation so
                // we return to damage-driven idle (0 CPU). None = not animating.
                // Each effect has its own duration. (None has duration 0 → ends on
                // the first frame.)
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
                // The main window's chrome geometry (DPI × UI font) — every chrome
                // builder below and the hit-tests in the input arms use the SAME
                // metrics, so clicks land where the chrome is drawn.
                let cm = self.chrome_metrics();
                let bar_h = cm.bar_h();
                // Metrics of the TERMINAL font, for overlays anchored to grid
                // cells (hint chips): their labels render in the grid font so
                // they always fit their one-row chip, whatever the UI font size.
                let grid_cm = jetty_render::ChromeMetrics::new(scale, self.font_logical);
                // Menu shortcut hints from the LIVE keymap (built only while a
                // menu is open).
                let context_hints: Vec<String> = if context_menu.is_some() {
                    crate::detached::context_menu_hints(&self.keymap).to_vec()
                } else {
                    Vec::new()
                };
                let tab_menu_hints: Vec<String> = if tab_menu.is_some() {
                    tab_menu_labels
                        .iter()
                        .map(|&l| crate::detached::menu_hint(&self.keymap, l))
                        .collect()
                } else {
                    Vec::new()
                };
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
                // Tab-bar geometry: the bar's pixel Y (0 at top, height-bar_h at
                // bottom) and the grid's pixel ORIGIN (bar_h at top, 0 at bottom).
                // Bottom-mode tab bar sits ABOVE the status bar (height - bar_h
                // - status_h); the status bar (perf HUD) takes the very bottom.
                let bar_y = if tab_bar_bottom { (height as f32 - bar_h - status_h).max(0.0) } else { 0.0 };
                let grid_top = if tab_bar_bottom { 0.0 } else { bar_h };
                // Compute window-control hover from the last cursor position.
                let ctrl_hover = ctrl_hover_at(cursor.0 as f32, cursor.1 as f32, width, bar_y, cm);
                let rename_ref = rename_state.as_ref().map(|(i, b)| (*i, b.as_str()));
                // The perf HUD now lives in the bottom STATUS BAR (off the tab row),
                // so the tab bar is built WITHOUT it (None).
                let mut bar = jetty_render::build_tab_bar_ex(
                    width, &tabs_meta, &theme, rename_ref, ctrl_hover, None, &mut *chrome_text, cm,
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
                        (height as f32 - bar_h - status_h).max(0.0)
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
                                // Chrome: the UI-font layer, so the bar text never
                                // scales with the TERMINAL font; the bar itself is
                                // sized by the same ChromeMetrics as this text.
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
                            // Right-aligned, measured, ellipsized to the window
                            // (shared with detached windows and jetty-shot).
                            let strip = jetty_render::build_status_strip(
                                width, sy, status_h, Some(perf), &theme, &mut *chrome_text, cm,
                            );
                            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[strip.quad]);
                            if let Some(label) = strip.label {
                                let _ = chrome_text.render_overlays(
                                    &gpu.device, &gpu.queue, scene_view, width, height, &[label],
                                );
                            }
                        }
                    }
                    // Pass 4c: Shift+drag hint toast — a brief, centered pill shown
                    // when the user drags (no Shift) inside a mouse-reporting app, so
                    // they discover the Shift+drag-to-select gesture. Throttled.
                    // Pills sit above the bottom-mode tab bar too, not just the
                    // status strip, or they draw over the tab titles.
                    let pill_bottom = height as f32
                        - status_h
                        - if tab_bar_bottom { bar_h } else { 0.0 }
                        - cm.px(14.0);
                    if shift_hint_show {
                        let pill = jetty_render::build_toast_pill(
                            width, pill_bottom, slide_y_offset,
                            "Hold Shift while dragging to select text",
                            &theme, &mut *chrome_text, cm,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill.quad]);
                        let _ = chrome_text.render_overlays(
                            &gpu.device, &gpu.queue, scene_view, width, height, &[pill.label],
                        );
                    }
                    // Pass 4c': run-selection status pill (refusal / staged) —
                    // the same pill surface as the shift hint, stacked one row
                    // above it on the rare frame both are live.
                    if let Some(msg) = &status_pill_msg {
                        let stack = if shift_hint_show { cm.pill_h() + cm.px(8.0) } else { 0.0 };
                        let pill = jetty_render::build_toast_pill(
                            width, pill_bottom - stack, slide_y_offset, msg,
                            &theme, &mut *chrome_text, cm,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &[pill.quad]);
                        let _ = chrome_text.render_overlays(
                            &gpu.device, &gpu.queue, scene_view, width, height, &[pill.label],
                        );
                    }
                    // Pass 4d: the scrollback-search bar (Ctrl+Shift+F) — a
                    // themed pill at the top-right of the grid. Rides the
                    // dropdown slide like its neighbours and is drawn BEFORE
                    // the context menu / help / confirm passes so modals keep
                    // visual priority over it.
                    if let Some((q, cur, total)) = &search_ui {
                        let sb = jetty_render::build_search_bar(
                            width, grid_top + slide_y_offset, &theme, &mut *chrome_text, cm, q, *cur, *total,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &sb.quads);
                        if !sb.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &sb.labels,
                            );
                        }
                    }
                    // Pass 4e: hint-mode label chips — themed/HiDPI, quads then
                    // text. Only while active. The chips are one grid ROW tall, so
                    // their labels render (and are measured) in the TERMINAL font
                    // via the grid layer — like the welcome splash — not the UI
                    // font, which at a large UI size overflowed the chip.
                    if let Some((labeled, typed)) = &hint_ui {
                        let refs: Vec<(&str, usize, usize)> =
                            labeled.iter().map(|(l, r, c)| (l.as_str(), *r, *c)).collect();
                        let ov = jetty_render::build_hint_overlay(
                            &refs,
                            cell_w,
                            cell_h,
                            grid_top + slide_y_offset,
                            &theme,
                            &mut *text,
                            grid_cm,
                            typed,
                            width,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &ov.quads);
                        if !ov.labels.is_empty() {
                            let _ = text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &ov.labels,
                            );
                        }
                    }
                    // Pass 4e': IME preedit at the cursor — the composition in the
                    // TERMINAL font via the grid layer (like the hint chips), on
                    // the theme bg, underlined, until it commits.
                    if let Some(p) = &preedit_ui {
                        if let Some(ov) = jetty_render::build_preedit_overlay(
                            p,
                            snap.cursor_row,
                            snap.cursor_col,
                            snap.cols,
                            cell_w,
                            cell_h,
                            grid_top + slide_y_offset,
                            &theme,
                            scale,
                        ) {
                            quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &ov.quads);
                            let _ = text.render_overlays(
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
                            &mut *chrome_text,
                            cm,
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
                        // Render the neofetch splash with the MONOSPACE terminal
                        // font (its cell metrics), NOT the chrome/UI font: the
                        // block-art logo needs fixed advances + row pitch to stay
                        // aligned, and a proportional UI font garbled it.
                        let (welcome_cw, welcome_ch) = text.cell_size();
                        let mut splash = jetty_render::build_welcome_overlay(
                            width,
                            height,
                            grid_top + slide_y_offset,
                            env!("CARGO_PKG_VERSION"),
                            &gpu_backend_name,
                            &theme,
                            welcome_cw,
                            welcome_ch,
                        );
                        // Clip the splash to the grid area so it never draws over a
                        // bottom tab bar (e.g. on a very short window): drop swatch
                        // quads / label rows below the grid bottom and trim a quad
                        // that straddles the edge. The status strip is always
                        // reserved; the tab bar only in bottom mode.
                        let grid_bottom = if tab_bar_bottom {
                            (height as f32 - bar_h - status_h).max(0.0)
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
                            // Terminal (monospace) layer so the block-art logo
                            // aligns regardless of the UI font.
                            let _ = text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &splash.labels,
                            );
                        }
                    }
                    // Draw the right-click context menu on top of everything.
                    if let Some((mx, my)) = context_menu {
                        let hint_refs: Vec<&str> = context_hints.iter().map(String::as_str).collect();
                        let menu = jetty_render::build_context_menu(
                            mx, my, width, height, menu_hover, &theme, &mut *chrome_text, cm,
                            &hint_refs, &menu_disabled,
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
                    // Draw the TAB context menu (Detach / Rename / Close Tab) —
                    // mutually exclusive with the terminal menu above.
                    if let Some((mx, my, _)) = tab_menu {
                        let items: Vec<(&str, &str)> = tab_menu_labels
                            .iter()
                            .zip(&tab_menu_hints)
                            .map(|(&l, h)| (l, h.as_str()))
                            .collect();
                        let menu = jetty_render::build_menu(
                            mx, my, width, height, tab_menu_hover, &theme, &mut *chrome_text, cm,
                            &items, &[], &[],
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
                        let help = jetty_render::build_help_overlay(
                            width, height, &theme, &mut *chrome_text, cm, &help_rows, help_scroll,
                        );
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
                            width, height, "Quit JeTTY? — all tabs will close", &theme,
                            &mut *chrome_text, cm,
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &popup.quads);
                        if !popup.labels.is_empty() {
                            let _ = chrome_text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &popup.labels,
                            );
                        }
                    } else if let Some(title) = &confirm_close {
                        let popup = jetty_render::build_confirm_close(
                            width, height, title, &theme, &mut *chrome_text, cm,
                        );
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
                            width, height, &theme, &mut *chrome_text, cm, q, &prows, *total, *first,
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
                    // otherwise `about_to_wait` pumps the next frame.
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
                    // The slide ends at t ≥ 1 here too (and by wall clock in
                    // `about_to_wait`, which alone decides whether another frame
                    // is pumped — this tail never self-drives).
                    if let Some(s) = self.slide_anim {
                        if s.elapsed().as_secs_f32() >= DROPDOWN_SLIDE_SECS {
                            self.slide_anim = None;
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
                    // The swapchain is healthy again: drop any retry schedule.
                    self.acquire_retry = None;
                    // Flood pacing anchor; this frame carries every drained byte,
                    // so a deferred flood paint is now redundant.
                    self.last_present_at = Some(std::time::Instant::now());
                    self.paced_paint_at = None;
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
                } else {
                    // Acquire failed (Outdated/Lost/Timeout/Occluded/Validation):
                    // this frame's damage was NOT shown. Start the bounded retry
                    // schedule (`about_to_wait` issues and advances it) — a retry
                    // already in flight keeps its schedule, so each retry counts once.
                    self.acquire_retry
                        .get_or_insert_with(|| next_acquire_retry(None, std::time::Instant::now()));
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
///   Pass 1  clear + per-cell background quads (+ main-only search-hit tint) + the
///           solid block cursor (under its glyph)
///   Pass 2  glyphs (recorded into the SAME render pass + submit as Pass 1)
///   Pass 2b inline (sixel/kitty) images, scissored to the grid area
///   Pass 3  CALLER-INJECTED mid-scene chrome (`draw_chrome`) — the main tab
///           bar or the detached title bar, drawn BETWEEN the glyph pass and
///           the scrollbar/cursor pass exactly as both windows do today
///   Pass 4  scrollbar + failed-command markers + SGR decorations + link
///           underline + the thin cursor shapes (beam / underline / unfocused
///           hollow) (+ main-only copy-mode cursor)
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
    let selection = jetty_render::selection_paint(s.theme);
    let scrollbar_thumb = scrollbar_thumb_for(s.theme);
    // The shell cursor, split by layer: the SOLID block is painted under the
    // glyphs (Pass 1) with the glyph it covers recolored for contrast (Pass 2);
    // beam / underline / unfocused hollow draw over the text (Pass 4). In
    // copy-mode (main only) the shell cursor is SUPPRESSED so only the copy-mode
    // keyboard cursor shows; detached always passes `copy_mode_active = false`.
    let (cursor_under, cursor_over) = if s.copy_mode_active {
        (None, Vec::new())
    } else {
        jetty_render::cursor_rects_split(
            s.snap, cell_w, cell_h, grid_origin_y, s.focused, s.caret_t_for_flash, s.caret_flash_color,
        )
    };

    // Pass 1: clear to the (premultiplied, opacity-correct) theme bg and paint
    // the per-cell background quads under the text. Search-hit tint rects are
    // appended AFTER the selection rects (main-only; empty for detached) so the
    // match tint wins where they overlap, still under the glyphs; the block
    // cursor goes last so it covers both.
    let mut bg_rects = jetty_render::cell_bg_rects(s.snap, cell_w, cell_h, grid_origin_y, selection.bg);
    if !s.search_hits.is_empty() {
        bg_rects.extend(jetty_render::search_hit_rects(
            s.search_hits, cell_w, cell_h, grid_origin_y, s.theme,
        ));
    }
    bg_rects.extend(cursor_under);

    // Pass 2: glyphs over the painted background, offset down by the grid origin.
    // Cells carrying combining marks / VS16 / ZWJ (sparse; empty = no allocation).
    let graphemes: Vec<(usize, usize, &str)> =
        s.snap.graphemes.iter().map(|g| (g.row, g.col, g.text.as_str())).collect();
    let paint = jetty_render::GridPaint {
        cursor_glyph: cursor_under.map(|_| {
            (s.snap.cursor_row, s.snap.cursor_col, jetty_render::cursor_text_color(s.theme, s.snap.cursor_rgb))
        }),
        selection: Some(selection),
        graphemes: &graphemes,
    };
    // Passes 1 + 2 are recorded into ONE render pass and ONE queue submit (each
    // separate pass + submit cost tens of µs of CPU on every frame). Both uploads
    // land at that submit, ahead of the draws.
    let bg_count = quad.upload(device, queue, width, height, &bg_rects);
    let text_ready = text.prepare_grid(device, queue, width, height, s.snap, grid_origin_y, &paint).is_ok();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("grid") });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: scene_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(jetty_render::default_bg_clear(s.snap, gpu.premultiply_clear)),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        quad.draw_uploaded(&mut pass, bg_count);
        if text_ready {
            text.draw_grid(&mut pass);
        }
    }
    queue.submit(Some(encoder.finish()));
    text.end_grid_frame();

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
    // The thin cursor shapes (beam / underline / unfocused hollow) last, over the
    // glyphs + decorations; the solid block was painted under the text (Pass 1).
    rects.extend(cursor_over);
    if let Some((cr, cc, _sel, _lm)) = s.copy_mode_ui {
        rects.extend(jetty_render::copy_cursor_rects(cr, cc, cell_w, cell_h, grid_origin_y, s.theme.cursor));
    }
    quad.render(device, queue, scene_view, width, height, &rects);
}

/// `r` while its tab is still among `live`, else `None` — the stale-reference
/// rule for every id-holding piece of UI state.
fn still_open(r: Option<TabId>, live: &[TabId]) -> Option<TabId> {
    r.filter(|id| live.contains(id))
}

/// Shared input core (v0.23 Task 9 / amendment I5): a keystroke (or IME commit)
/// that was NOT consumed by any chrome/overlay → snap the viewport to the live
/// bottom and write the decoded bytes to this tab's PTY. Deliberately SMALL —
/// the caret-burst arming (gated on different toggles per window; the glow is
/// main-only), the perf keystroke stamp, welcome-splash dismissal, and every
/// modal/menu short-circuit stay in the per-window callers.
///
/// `pressed` is the physical key when `bytes` encode a key PRESS (not an IME
/// commit): the tab then owes that key's release to the program
/// (`write_key_release`).
fn write_key_to_pty(tab: &mut Tab, bytes: &[u8], pressed: Option<winit::keyboard::PhysicalKey>) {
    // The user has claimed this prompt: a staged run-selection inject must
    // never splice into their half-typed line. This ONE line covers every
    // funnel that routes here — main + detached keystrokes AND both windows'
    // IME commits. (Mouse REPORTS deliberately don't cancel: they only flow
    // when an app enabled mouse mode, which no shell has at a fresh prompt,
    // and they are app input, not command-line editing.)
    crate::runsel::cancel_on_user_write(&mut tab.pending_inject);
    // Any real keystroke jumps the view back to the live bottom so typing while
    // scrolled up into scrollback is visible (F30). Order is irrelevant vs the
    // PTY write (viewport offset and the PTY writer are independent).
    tab.terminal.scroll_to_bottom();
    let _ = tab.writer.write_all(bytes);
    let _ = tab.writer.flush();
    if let Some(key) = pressed {
        tab.input.note_press(key);
    }
}

/// The shared decision half of the main and detached key paths: one winit key
/// event (press, auto-repeat or release) → its action, decided against the
/// modes of the tab it goes to — DECCKM, the alternate screen, the kitty
/// keyboard flags its program pushed — with this platform's Option rules.
/// Keymap chords, the kitty protocol, macOS Option-compose, dead keys and the
/// legacy xterm encoders all live behind `input::decide_key_event`; the main
/// window's overlays/menus/modals take their keys BEFORE this is called.
fn decide_window_key(
    keymap: &crate::keymap::KeyMap,
    event: &winit::event::KeyEvent,
    mods: &winit::event::Modifiers,
    terminal: &Terminal,
    option_as_alt: input::OptionAsAlt,
) -> input::KeyAction {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    let base = event.key_without_modifiers();
    let ev = input::KeyInput {
        physical: event.physical_key,
        logical: &event.logical_key,
        key_without_modifiers: &base,
        text: event.text.as_deref(),
        location: event.location,
        kind: input::KeyEventKind::from_winit(event.state, event.repeat),
        mods: input::KeyMods::from_winit(mods),
    };
    let modes = input::KeyModes {
        app_cursor: terminal.app_cursor_keys(),
        // DECKPAM changes no byte (NumLock overrides it — see `KeyModes`).
        app_keypad: false,
        alt_screen: terminal.alt_screen(),
        kitty_flags: terminal.kitty_keyboard_flags(),
    };
    input::decide_key_event(keymap, &ev, &modes, &input::KeyOptions::native(option_as_alt), false)
}

/// A key RELEASE for `tab`, which was sent the press: report it when the
/// program asked for event types (kitty protocol flag 2/8 — otherwise the
/// decision is `None` and nothing is written). Written as-is: no scroll-to-
/// bottom snap and no run-selection cancel — the press already did both.
fn write_key_release(
    keymap: &crate::keymap::KeyMap,
    tab: &mut Tab,
    event: &winit::event::KeyEvent,
    mods: &winit::event::Modifiers,
    option_as_alt: input::OptionAsAlt,
) {
    if !tab.input.take_release(event.physical_key) {
        return;
    }
    if let input::KeyAction::Send(bytes) =
        decide_window_key(keymap, event, mods, &tab.terminal, option_as_alt)
    {
        let _ = tab.writer.write_all(&bytes);
        let _ = tab.writer.flush();
    }
}

/// DECSET 1004 focus reporting for one tab: observe whether it is the focused
/// one and write `CSI I` / `CSI O` when that CHANGED and its program enabled
/// the mode (`TabInputState::focus_report`).
fn report_focus(tab: &mut Tab, focused: bool) {
    let enabled = tab.terminal.focus_reporting();
    if let Some(bytes) = tab.input.focus_report(focused, enabled) {
        let _ = tab.writer.write_all(bytes);
        let _ = tab.writer.flush();
    }
}

/// macOS: tell winit which Option side(s) are Meta (`macos_option_as_alt`) so
/// it stops composing characters there and reports the plain key — the key
/// encoder then ESC-prefixes it. A no-op on other platforms (Alt is Meta).
#[cfg(target_os = "macos")]
fn apply_option_as_alt(window: &winit::window::Window, option_as_alt: input::OptionAsAlt) {
    use winit::platform::macos::WindowExtMacOS;
    window.set_option_as_alt(option_as_alt.to_winit());
}

#[cfg(not(target_os = "macos"))]
fn apply_option_as_alt(_window: &winit::window::Window, _option_as_alt: input::OptionAsAlt) {}


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

/// The warning for a chosen theme that isn't in the registry (`shown` is the
/// display name of the fallback on screen).
fn theme_missing_warning(name: &str, shown: &str) -> String {
    format!(
        "theme {name:?} not found (missing or invalid theme file?) — showing \
         {shown:?} until it loads"
    )
}

/// Make user-derived notice text safe to print into a terminal or a pill: every
/// control character (ESC, CR, C1, …) becomes a visible `�`, so a value quoted
/// from config.toml can never inject an escape sequence.
fn sanitize_notice(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '\u{FFFD}' } else { c }).collect()
}

/// Forward global summon-hotkey presses to the event loop (blocks for the
/// process lifetime on the hotkey receiver).
fn forward_hotkey_presses(proxy: &EventLoopProxy<AppEvent>) {
    let rx = global_hotkey::GlobalHotKeyEvent::receiver();
    while let Ok(ev) = rx.recv() {
        if ev.state == global_hotkey::HotKeyState::Pressed && proxy.send_event(AppEvent::ToggleVisibility).is_err() {
            break;
        }
    }
}

/// Report that the global summon hotkey could not be registered. Logged always;
/// shown in-app unless this is a Wayland session, where apps can't grab keys by
/// design and `jetty --toggle` bound in the compositor is the documented path.
fn report_hotkey_failure(proxy: &EventLoopProxy<AppEvent>, spec: &str, err: &str) {
    eprintln!("jetty: global hotkey {spec} unavailable — {err}");
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland");
    if cfg!(target_os = "macos") || !wayland {
        let hint = if cfg!(target_os = "macos") {
            "grant JeTTY Accessibility permission, or bind `jetty --toggle` to a shortcut"
        } else {
            "another app may hold it — set another `summon_hotkey`, or bind `jetty --toggle`"
        };
        let _ = proxy.send_event(AppEvent::ConfigNotice(format!(
            "summon hotkey {spec} unavailable ({err}) — {hint}"
        )));
    }
}

/// The launchd label of the macOS login item (also its plist file name).
const LAUNCH_AGENT_LABEL: &str = "io.github.bozdemir.jetty";

/// Where the login-autostart entry lives. Linux/BSD: the freedesktop XDG autostart
/// file `$XDG_CONFIG_HOME/autostart/jetty.desktop` (falling back to
/// `~/.config/autostart/`), honored by KDE/GNOME/any DE — no desktop-environment-
/// specific code. macOS (which ignores XDG autostart): a launchd LaunchAgent,
/// `~/Library/LaunchAgents/io.github.bozdemir.jetty.plist`.
fn autostart_path() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| std::path::PathBuf::from(".config"));
        base.join("autostart").join("jetty.desktop")
    }
}

/// The program the autostart entry launches: the AppImage FILE when running from
/// one (`$APPIMAGE` — `current_exe()` is then a temporary `/tmp/.mount_*` path that
/// is gone by the next login), else this executable, else `jetty` from PATH.
fn autostart_program(appimage: Option<String>, exe: Option<String>) -> String {
    appimage
        .filter(|p| !p.is_empty())
        .or(exe)
        .unwrap_or_else(|| "jetty".to_string())
}

/// The freedesktop autostart entry. Starts with `--background`: at login JeTTY
/// comes up hidden, holding the summon hotkey, instead of popping a window.
/// `Exec=` is quoted per the Desktop Entry spec (see `desktop_exec_arg`).
fn autostart_desktop_entry(program: &str) -> String {
    let exec = desktop_exec_arg(program);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=JeTTY\n\
         GenericName=Terminal Emulator\n\
         Comment=Blazing-fast GPU terminal with a center-summon hotkey (starts hidden at login; holds the summon hotkey)\n\
         Exec={exec} --background\n\
         Icon=jetty\n\
         Terminal=false\n\
         Categories=System;TerminalEmulator;Utility;\n\
         StartupWMClass=jetty\n\
         X-GNOME-Autostart-enabled=true\n\
         X-JeTTY-Generated=true\n"
    )
}

/// Was this autostart file written by JeTTY (now or by an older version)? Only
/// those are rewritten or removed: an entry the user made themselves at the same
/// path (e.g. a desktop's own "add to autostart") is never touched.
fn is_jetty_autostart_entry(content: &str) -> bool {
    content.contains("X-JeTTY-Generated=true")
        // Pre-v0.26 JeTTY entries carried this comment and no marker.
        || content.contains("(autostart: holds the F9 grab)")
        || content.contains(&format!("<string>{LAUNCH_AGENT_LABEL}</string>"))
}

/// The macOS LaunchAgent: run `program --background` once at login.
fn launch_agent_plist(program: &str) -> String {
    let xml = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LAUNCH_AGENT_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{}</string>\n\
         \t\t<string>--background</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Interactive</string>\n\
         </dict>\n\
         </plist>\n",
        xml(program)
    )
}

/// Make the login-autostart entry match `enabled` (the config key — the source
/// of truth): write it, refreshing a stale program path, or remove it. Never
/// panics; a failure is returned for display.
fn set_launch_at_login(enabled: bool) -> Result<(), String> {
    let contents = enabled.then(|| {
        let program = autostart_program(
            std::env::var("APPIMAGE").ok(),
            std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_string)),
        );
        if cfg!(target_os = "macos") {
            launch_agent_plist(&program)
        } else {
            autostart_desktop_entry(&program)
        }
    });
    sync_autostart_file(&autostart_path(), contents.as_deref())
}

/// Write `contents` to `path` (only when it differs — no churn on every start) or,
/// for `None`, remove it (a missing file is already the goal). A file at `path`
/// that JeTTY did not write is left alone either way.
fn sync_autostart_file(path: &std::path::Path, contents: Option<&str>) -> Result<(), String> {
    let current = std::fs::read_to_string(path).ok();
    if current.as_deref().is_some_and(|c| !is_jetty_autostart_entry(c)) {
        return Ok(());
    }
    match contents {
        Some(c) => {
            if current.as_deref() == Some(c) {
                return Ok(());
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    format!("launch at login: could not create {}: {e}", dir.display())
                })?;
            }
            std::fs::write(path, c)
                .map_err(|e| format!("launch at login: could not write {}: {e}", path.display()))
        }
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("launch at login: could not remove {}: {e}", path.display())),
        },
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

/// The text measurer for chrome built OUTSIDE a frame (hit-testing): the
/// window's chrome text layer when it exists, else a monospace estimate at the
/// metrics' scale (only before the GPU stack is up, when nothing is drawn).
fn measure_or<'a>(
    layer: Option<&'a mut TextLayer>,
    fallback: &'a mut jetty_render::MonoMeasure,
) -> &'a mut dyn jetty_render::ChromeMeasure {
    match layer {
        Some(t) => t,
        None => fallback,
    }
}

/// Monospace fallback measurer at metrics `cm` (the default chrome font's
/// design advance, scaled).
fn mono_fallback(cm: jetty_render::ChromeMetrics) -> jetty_render::MonoMeasure {
    jetty_render::MonoMeasure(jetty_render::CHROME_ADVANCE * cm.u)
}

/// Which window-control button (if any) the cursor at `(cx, cy)` is over, given
/// the surface `width`. Mirrors the control layout in `build_tab_bar_ex`: three
/// control cells parked at the right of the tab-bar strip (min, max, close),
/// sized by the chrome metrics `cm`.
fn ctrl_hover_at(
    cx: f32,
    cy: f32,
    width: u32,
    bar_y: f32,
    cm: jetty_render::ChromeMetrics,
) -> jetty_render::CtrlHover {
    use jetty_render::CtrlHover;
    if cy < bar_y || cy >= bar_y + cm.bar_h() {
        return CtrlHover::None;
    }
    // The controls are inset from the surface's right edge by the strip pad;
    // mirror that here or every hover zone is shifted right of the buttons.
    let sw = width as f32 - cm.strip_pad();
    let ctrl_w = cm.ctrl_w();
    let help_x = sw - cm.controls_w(); // sw - 5*ctrl_w
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

/// The corner radius (PHYSICAL px) a window should actually render with.
///
/// Fullscreen ⇒ `0.0`: a fullscreen window with rounded corners shows the desktop
/// through four notches at the screen edges. `CornerMask::apply` early-returns
/// when every radius is 0 (`mask.rs`) and the CRT shader treats 0 as square
/// (`crt.rs`), so `0.0` both LOOKS right and SKIPS the final mask pass.
///
/// That skipped pass is NOT a net saving: fullscreen is the most expensive mode.
/// Frame cost scales with surface AREA — the default window is 1000×640, and a
/// fullscreen surface on a 4K monitor is ~13× the pixels for the quad, text,
/// image and CRT passes alike. One pass saved, not a rescue (amendment I-B).
///
/// The `corner_radius` FIELD is never mutated: the Look-tab slider, `persist()`
/// and the config round-trip must keep the user's value, so leaving fullscreen
/// restores the rounding with nothing to restore. Suppression is render-time only.
fn effective_corner_radius_px(radius_logical: f32, scale: f32, fullscreen: bool) -> f32 {
    if fullscreen {
        0.0
    } else {
        radius_logical * scale
    }
}

/// Whether the Settings panel draws the CORNER RADIUS band dimmed — the same
/// "this control has no effect right now" idiom the panel already uses for
/// DROPDOWN HEIGHT/WIDTH outside Dropdown mode.
///
/// `main_fullscreen` alone was unobservable: opening Settings exits fullscreen,
/// so the flag was ALWAYS false while the panel was on screen. Adding the
/// persisted MODE is what makes the feedback real — switching WINDOW MODE to
/// Fullscreen dims the band immediately, which is precisely the user who needs
/// telling (their corner radius is suppressed at display time on every summon).
/// Both the render view and the hit-test view are fed from here so they can
/// never diverge.
fn corner_radius_band_dimmed(main_fullscreen: bool, mode: WindowMode) -> bool {
    main_fullscreen || mode == WindowMode::Fullscreen
}

/// Whether the Dropdown dock geometry may be (re-)asserted right now.
///
/// Fullscreen suppresses it: `pending_dock_frames` re-issues `dock_window_top` on
/// the next few redraws and would yank a fullscreen window into the top strip
/// mid-transition. This matters most in the explicitly-supported
/// **Dropdown + ad-hoc F11** state, where `window_mode == Dropdown` is still
/// true — so every DIRECT `dock_window_top` caller (the post-map re-assertion,
/// `redock_if_dropdown`, the dropdown-slider release re-dock and the slider
/// drag-start latches that enable it) goes through this one predicate.
fn dock_reassert_ok(mode: WindowMode, fullscreen: bool) -> bool {
    mode == WindowMode::Dropdown && !fullscreen
}

/// The post-map re-assertion counters a fullscreen EXIT may arm:
/// `(pending_dock_frames, pending_center_frames)`.
///
/// THE INVARIANT: `(0, 0)` whenever `!visible`. Those counters only decrement
/// inside `RedrawRequested`, which a hidden (ordered-out) window never receives on
/// macOS, while `main_pending` in `about_to_wait` selects `ControlFlow::Poll` for
/// as long as either is non-zero — so arming one while hidden pegs a core
/// invisibly (the F18 bug class). On X11 it would additionally fire five
/// `dock_window_top` calls → five `Resized` events → a SIGWINCH storm to every
/// hidden tab, the p10k-prompt-scatter trigger v0.23.1 fixed.
///
/// Also `(0, 0)` when the window is MAXIMIZED (nothing is restored, so nothing
/// needs re-asserting). Otherwise Center/Fullscreen always get `(0, 5)`: BOTH
/// arms of the restore issue an explicit position — the saved `last_pos` when
/// there is a restorable one, the COMPUTED centre otherwise — and both race the
/// WM's own restore of the pre-fullscreen frame, so both need re-asserting.
/// (`restorable_pos` no longer changes the answer; it is kept as a parameter
/// because it names the distinction the caller acts on and documents that it was
/// considered.)
///
/// Pure, so the invariant is a unit test rather than a code-reading exercise.
/// The debounce window shared by every `reflow_pending_at` arming site: one
/// grid+PTY reflow (and so one SIGWINCH) once the user stops resizing.
const REFLOW_DEBOUNCE_MS: u64 = 250;

/// The `(reflow_pending_at, reflow_deferred_by_hide)` pair a HIDE must leave
/// behind: NEVER a live deadline (a reflow that fires while hidden SIGWINCHes
/// every shell to a grid the user cannot see), but remember that one was owed.
///
/// Pure so "the hide legs clear the deadline" is a unit test. Both hide paths —
/// `set_visibility(false)` and `autohide_main_window` — go through it, alongside
/// the other deferred terms they already zero.
fn reflow_terms_on_hide(
    pending: Option<std::time::Instant>,
    deferred: bool,
) -> (Option<std::time::Instant>, bool) {
    (None, deferred || pending.is_some())
}

/// The same pair after a SUMMON: re-arm the owed reflow ONCE, at the deadline
/// the summon's own geometry change would use, and clear the debt.
///
/// A later `Resized` from the summon's own geometry (the Fullscreen enter, the
/// Dropdown dock) simply overwrites the deadline with its own, so a summon still
/// costs exactly ONE debounced reflow — never two, never zero.
fn reflow_terms_on_summon(
    pending: Option<std::time::Instant>,
    deferred: bool,
    deadline: std::time::Instant,
) -> (Option<std::time::Instant>, bool) {
    if deferred {
        (Some(deadline), false)
    } else {
        (pending, false)
    }
}

fn fullscreen_exit_frames(
    visible: bool,
    maximized: bool,
    mode: WindowMode,
    _restorable_pos: bool,
) -> (u8, u8) {
    if !visible || maximized {
        return (0, 0);
    }
    match mode {
        WindowMode::Dropdown => (5, 0),
        WindowMode::Center | WindowMode::Fullscreen => (0, 5),
    }
}

/// Whether the Center-mode post-map position re-assertion may fire this frame.
///
/// Deliberately `!= Dropdown`, not `== Center`: Fullscreen mode uses it too, so
/// that an ad-hoc F11 EXIT while `window_mode == Fullscreen` can re-assert the
/// restored `last_pos`. Fullscreen itself suppresses it.
fn center_reassert_ok(mode: WindowMode, fullscreen: bool) -> bool {
    mode != WindowMode::Dropdown && !fullscreen
}

/// Whether `pos` (a window outer top-left, physical px) lies within some
/// currently-connected monitor. Used to reject a saved Center-mode position that
/// now falls on a since-disconnected monitor (F32). The containment test itself
/// lives in `jetty_platform::pos_in_monitor_rect`, shared with
/// `monitor_for_window` so "which screen" is decided in exactly one place.
fn pos_on_some_monitor(win: &Arc<Window>, pos: winit::dpi::PhysicalPosition<i32>) -> bool {
    win.available_monitors().any(|m| {
        let p = m.position();
        let s = m.size();
        jetty_platform::pos_in_monitor_rect((pos.x, pos.y), (p.x, p.y), (s.width, s.height))
    })
}

/// Remember where and how big the main window is, immediately BEFORE it enters
/// OS fullscreen — the one thing the exit path cannot ask the OS for afterwards.
///
/// A free function taking the two fields by `&mut` (rather than an `&mut self`
/// method) so it can be called from inside the `if let Some(win) = &self.window`
/// borrow on the summon path, and from `resumed` where `self.window` is not set
/// yet. Called from EVERY enter path — `set_main_fullscreen(true)`, the
/// `set_visibility` summon arm and `resumed` — because the two inline sites used
/// to bypass the capture entirely, which is what left the exit with nothing to
/// restore.
///
/// The position is captured only when the mode is NOT Dropdown (amendment
/// BLOCKING 7: Dropdown re-docks from monitor geometry and never restores a
/// saved spot, and `set_window_mode` assigns the NEW mode before we run, so
/// `!= Dropdown` — not `== Center` — is the reachable predicate). The SIZE is
/// captured in every mode: the exit's centring maths needs it whatever the mode,
/// and a Dropdown exit simply re-docks and ignores it.
fn capture_pre_fullscreen(
    win: &Arc<Window>,
    mode: WindowMode,
    last_pos: &mut Option<winit::dpi::PhysicalPosition<i32>>,
    last_windowed_size: &mut Option<winit::dpi::PhysicalSize<u32>>,
) {
    if mode != WindowMode::Dropdown {
        // `.or(*last_pos)` keeps an existing saved spot when the read fails
        // (Wayland). Consistent with the loosened `center_reassert_ok`.
        *last_pos = win.outer_position().ok().or(*last_pos);
    }
    // Read BEFORE the fullscreen request: afterwards this is the monitor size.
    *last_windowed_size = Some(win.outer_size());
}

/// Logical size for a window torn out of the main window (detach): the main
/// surface's physical size ÷ `scale` — except when the main window was
/// fullscreen at the detach. Leaving fullscreen is asynchronous, so the surface
/// is still MONITOR-sized at that point and the detached window would come up
/// covering the whole monitor; the windowed size captured on fullscreen entry
/// (`capture_pre_fullscreen`) is the size the user actually had. Falls back to
/// 1000×640 when there is no surface yet.
fn detach_logical_size(
    surface: Option<(u32, u32)>,
    pre_fullscreen: Option<(u32, u32)>,
    was_fullscreen: bool,
    scale: f64,
) -> (u32, u32) {
    let physical = match (was_fullscreen, pre_fullscreen) {
        (true, Some(windowed)) => Some(windowed),
        _ => surface,
    };
    let Some((w, h)) = physical else { return (1000, 640) };
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (
        ((w as f64 / scale).round() as u32).max(1),
        ((h as f64 / scale).round() as u32).max(1),
    )
}

/// Bytes drained in one pass above which PTY output is a FLOOD (`cat bigfile`,
/// `yes`, a build log) rather than interactive: a keystroke echoes a few bytes,
/// a prompt redraw a few hundred, a screenful of `ls` a few KiB.
const FLOOD_PACE_BYTES: u64 = 64 * 1024;

/// When to paint freshly drained PTY output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaintPacing {
    Now,
    At(std::time::Instant),
}

/// Frame pacing for PTY output (pure). Interactive output always paints NOW —
/// no added latency for an echo. During a FLOOD, a window renders only once a
/// full refresh has passed since its last present: with Fifo and one frame in
/// flight, an earlier render blocks the UI thread inside the swapchain acquire
/// until the previous frame reaches the screen, and that blocked time is time
/// NOT spent draining — the flood was capped at about one drain budget per
/// refresh. The deferred paint lands at `last_present + interval`, when the
/// acquire no longer waits.
fn pace_paint(
    flood: bool,
    last_present: Option<std::time::Instant>,
    interval: std::time::Duration,
    now: std::time::Instant,
) -> PaintPacing {
    match last_present {
        Some(t) if flood && now < t + interval => PaintPacing::At(t + interval),
        _ => PaintPacing::Now,
    }
}

/// The display refresh interval for a monitor rate in millihertz (winit's
/// `refresh_rate_millihertz`), clamped to 20–250 Hz; 60 Hz when unknown.
fn refresh_interval(mhz: Option<u32>) -> std::time::Duration {
    match mhz {
        Some(m) if m > 0 => std::time::Duration::from_micros((1_000_000_000u64 / m as u64).clamp(4_000, 50_000)),
        _ => std::time::Duration::from_micros(16_667),
    }
}

/// Pause between GPU rebuild attempts while the device stays unavailable (a
/// driver reset can take seconds; each attempt enumerates adapters on the UI
/// thread, so it must not repeat every frame).
const GPU_REBUILD_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether `App::recover_lost_gpu` should attempt a rebuild now: a device is
/// lost and no failed attempt is still backing off.
fn gpu_recovery_due(any_lost: bool, retry_at: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    any_lost && retry_at.is_none_or(|t| now >= t)
}

/// Which window opening Settings must take out of fullscreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullscreenExit {
    None,
    Main,
    Detached(usize),
}

/// Pure decision behind the Settings-vs-fullscreen rule: the REQUESTING window
/// is the focused one — `focused_detached` = `(index, is_fullscreen)` of the
/// focused detached window, if any — else the main window. Only the requester
/// leaves fullscreen.
fn settings_fullscreen_exit(main_fullscreen: bool, focused_detached: Option<(usize, bool)>) -> FullscreenExit {
    match focused_detached {
        Some((pos, true)) => FullscreenExit::Detached(pos),
        Some((_, false)) => FullscreenExit::None,
        None if main_fullscreen => FullscreenExit::Main,
        None => FullscreenExit::None,
    }
}

/// What ending the Settings-window drags owes (see `App::end_settings_drags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsDragEnd {
    /// A drag was live: its value was applied live but never written.
    persist: bool,
    /// A dropdown height/width drag: re-dock the window at the new size.
    redock: bool,
}

/// Pure decision behind `App::end_settings_drags`: any live drag (`value_drag`:
/// opacity/radius/Effects; `dropdown_drag`: dropdown height/width) persists,
/// and only a dropdown drag re-docks. No drag → nothing at all (a stray
/// release or focus loss must not write the config).
fn settings_drag_end(value_drag: bool, dropdown_drag: bool) -> SettingsDragEnd {
    SettingsDragEnd { persist: value_drag || dropdown_drag, redock: dropdown_drag }
}

/// The top-left a window of `win_size` needs to sit centred inside the monitor
/// rect `(mon_pos, mon_size)`. All physical px.
///
/// Pure so the centring arithmetic — the half that the fullscreen EXIT gets
/// wrong when it is fed a stale (monitor-sized) window size — is a unit test
/// rather than a GUI session. `saturating_sub` keeps a window LARGER than the
/// monitor pinned to the monitor origin instead of wrapping to a huge negative.
fn centered_pos(mon_pos: (i32, i32), mon_size: (u32, u32), win_size: (u32, u32)) -> (i32, i32) {
    (
        mon_pos.0 + (mon_size.0.saturating_sub(win_size.0) / 2) as i32,
        mon_pos.1 + (mon_size.1.saturating_sub(win_size.1) / 2) as i32,
    )
}

/// Centre `win` on the monitor it belongs to, using `size` (physical px) as the
/// window's size when given. Returns the position actually requested, or `None`
/// when no monitor info is available (then nothing is issued).
///
/// `size` exists for the fullscreen EXIT: `win.outer_size()` is a live
/// `XGetGeometry` on X11, and the un-fullscreen request issued one statement
/// earlier is an ASYNC `_NET_WM_STATE` ClientMessage the WM has not processed
/// yet — so the live read still reports the MONITOR size and the centring
/// resolves to the monitor origin. Passing the size captured on the way IN
/// (`last_windowed_size`) makes the maths independent of that race.
///
/// The monitor is resolved through the SHARED `jetty_platform::monitor_for_window`
/// chain (current monitor → the monitor containing the last outer position → the
/// first available), which is where `dock_window_top`'s fallback was factored out
/// to. NOTE: on X11 `current_monitor()` already returns the last known monitor
/// unconditionally, so the fallback legs only ever fire on macOS/Wayland — the
/// refactor is about having ONE monitor-choosing chain, not about a hidden-window
/// bug that X11 could exhibit.
fn center_window_sized(
    win: &Arc<Window>,
    size: Option<winit::dpi::PhysicalSize<u32>>,
) -> Option<winit::dpi::PhysicalPosition<i32>> {
    let mon = jetty_platform::monitor_for_window(win)?;
    let mon_pos = mon.position(); // physical px; nonzero on secondary monitors
    let mon_size = mon.size();
    let win_size = size.unwrap_or_else(|| win.outer_size());
    // Center WITHIN the current monitor: add the monitor's origin so a
    // multi-monitor setup centers on the right screen (the old code dropped
    // position() and always centered relative to (0,0) — a real bug).
    let (x, y) = centered_pos(
        (mon_pos.x, mon_pos.y),
        (mon_size.width, mon_size.height),
        (win_size.width, win_size.height),
    );
    let pos = winit::dpi::PhysicalPosition::new(x, y);
    win.set_outer_position(pos);
    Some(pos)
}

/// Centre `win` on its monitor using its CURRENT size. The plain-windowed entry
/// point (first open, Center-mode summon); the fullscreen exit uses
/// `center_window_sized` with the size captured before the enter.
fn center_window(win: &Arc<Window>) {
    let _ = center_window_sized(win, None);
}

/// Dock the window as a Yakuake-style top strip on the current monitor: full
/// monitor width (× `width_pct`), `height_pct` of the monitor height, flush to
/// the top edge (y = monitor top), centered horizontally. Sizes/positions are
/// set ONCE per summon (the slide-in is render-side, not a per-frame reposition).
/// On Wayland set_outer_position/request_inner_size are no-ops — accepted
/// degradation, same as the F9 hotkey.
///
/// The monitor is resolved by the SHARED `jetty_platform::monitor_for_window`
/// chain (current monitor → the monitor containing the last outer position → the
/// first available), which is where this function's own hidden-window fallback
/// was factored out to.
fn dock_window_top(win: &Arc<Window>, width_pct: f32, height_pct: f32) {
    if let Some(mon) = jetty_platform::monitor_for_window(win) {
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
        assert_eq!(resize_zone_at(500.0, 320.0, W, H, 1.0), ResizeZone::None);
    }

    #[test]
    fn edges_map_to_sides() {
        // West/East within 6px of a vertical side (mid-height).
        assert_eq!(resize_zone_at(2.0, 320.0, W, H, 1.0), ResizeZone::West);
        assert_eq!(resize_zone_at(998.0, 320.0, W, H, 1.0), ResizeZone::East);
        // North/South within 6px of a horizontal side (mid-width).
        assert_eq!(resize_zone_at(500.0, 2.0, W, H, 1.0), ResizeZone::North);
        assert_eq!(resize_zone_at(500.0, 638.0, W, H, 1.0), ResizeZone::South);
    }

    #[test]
    fn corners_take_priority_over_edges() {
        // Within 12px of two adjacent sides → the diagonal corner zone.
        assert_eq!(resize_zone_at(3.0, 3.0, W, H, 1.0), ResizeZone::NorthWest);
        assert_eq!(resize_zone_at(997.0, 3.0, W, H, 1.0), ResizeZone::NorthEast);
        assert_eq!(resize_zone_at(3.0, 637.0, W, H, 1.0), ResizeZone::SouthWest);
        assert_eq!(resize_zone_at(997.0, 637.0, W, H, 1.0), ResizeZone::SouthEast);
    }

    #[test]
    fn just_inside_edge_band_is_interior() {
        // 7px from the left edge (> EDGE=6, < CORNER=12 only matters near a corner):
        // at mid-height this is interior, not a resize zone.
        assert_eq!(resize_zone_at(7.0, 320.0, W, H, 1.0), ResizeZone::None);
    }

    #[test]
    fn top_outer_strip_is_resize_inner_is_not() {
        // The top 6px is North (resize); below that (still inside the bar) is the
        // tab bar, so resize_zone_at returns None there.
        assert_eq!(resize_zone_at(500.0, 3.0, W, H, 1.0), ResizeZone::North);
        assert_eq!(resize_zone_at(500.0, 20.0, W, H, 1.0), ResizeZone::None);
    }

    #[test]
    fn out_of_bounds_is_none() {
        assert_eq!(resize_zone_at(-5.0, 320.0, W, H, 1.0), ResizeZone::None);
        assert_eq!(resize_zone_at(500.0, 700.0, W, H, 1.0), ResizeZone::None);
    }

    #[test]
    fn grab_bands_scale_with_dpi() {
        // On a 2× display the edge/corner bands are 12/24 physical px — the same
        // physical-inch target as 6/12 at 1×, not a hair-thin band.
        let (w, h) = (2000u32, 1280u32);
        assert_eq!(resize_zone_at(10.0, 640.0, w, h, 2.0), ResizeZone::West);
        assert_eq!(resize_zone_at(10.0, 640.0, w, h, 1.0), ResizeZone::None);
        assert_eq!(resize_zone_at(20.0, 20.0, w, h, 2.0), ResizeZone::NorthWest);
        // A bogus scale factor falls back to 1×.
        assert_eq!(resize_zone_at(10.0, 640.0, w, h, 0.0), ResizeZone::None);
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
mod stable_tab_id_tests {
    use super::{still_open, TabId};

    #[test]
    fn closing_another_tab_never_retargets_a_reference() {
        // Tabs A(1) B(2) C(3); the rename box is on C. Closing A shifts every
        // index down by one — an index-held reference would now name D-or-none;
        // the id still names C.
        let renaming = Some(TabId(3));
        let after_closing_a = [TabId(2), TabId(3)];
        assert_eq!(still_open(renaming, &after_closing_a), Some(TabId(3)));
    }

    #[test]
    fn closing_the_referenced_tab_drops_the_reference() {
        let confirm = Some(TabId(2));
        assert_eq!(still_open(confirm, &[TabId(1), TabId(3)]), None);
        assert_eq!(still_open(None, &[TabId(1)]), None);
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
mod autostart_tests {
    use super::{autostart_desktop_entry, autostart_program, launch_agent_plist, sync_autostart_file};

    #[test]
    fn appimage_file_wins_over_its_temporary_mount() {
        // Inside an AppImage, current_exe() is /tmp/.mount_XXXX/usr/bin/jetty —
        // gone by the next login. $APPIMAGE is the real file.
        assert_eq!(
            autostart_program(
                Some("/home/u/Apps/JeTTY.AppImage".to_string()),
                Some("/tmp/.mount_abc/usr/bin/jetty".to_string())
            ),
            "/home/u/Apps/JeTTY.AppImage"
        );
        assert_eq!(autostart_program(None, Some("/usr/bin/jetty".to_string())), "/usr/bin/jetty");
        assert_eq!(autostart_program(Some(String::new()), Some("/usr/bin/jetty".to_string())), "/usr/bin/jetty");
        assert_eq!(autostart_program(None, None), "jetty");
    }

    #[test]
    fn desktop_entry_starts_hidden_and_quotes_the_path() {
        let e = autostart_desktop_entry("/home/u/My Apps/jetty");
        assert!(e.contains("Exec=\"/home/u/My Apps/jetty\" --background\n"), "{e}");
        assert!(e.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn launch_agent_runs_at_load_hidden_and_escapes_xml() {
        let p = launch_agent_plist("/Users/u/A&B <x>/jetty");
        assert!(p.contains("<string>/Users/u/A&amp;B &lt;x&gt;/jetty</string>"), "{p}");
        assert!(p.contains("<string>--background</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(p.contains("<string>io.github.bozdemir.jetty</string>"));
    }

    #[test]
    fn sync_writes_refreshes_and_removes_only_its_own_entry() {
        let dir = std::env::temp_dir().join(format!("jetty-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("autostart").join("jetty.desktop");
        let v1 = autostart_desktop_entry("/usr/bin/jetty");
        let v2 = autostart_desktop_entry("/opt/jetty/jetty");
        sync_autostart_file(&path, Some(&v1)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1);
        // A stale entry (old program path) is refreshed.
        sync_autostart_file(&path, Some(&v2)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v2);
        sync_autostart_file(&path, None).unwrap();
        assert!(!path.exists());
        // Removing what isn't there is fine.
        sync_autostart_file(&path, None).unwrap();
        // A pre-v0.26 JeTTY entry (no marker) is still recognized as ours.
        let legacy = "[Desktop Entry]\nComment=Blazing-fast GPU terminal with a center-summon hotkey (autostart: holds the F9 grab)\nExec=/usr/bin/jetty\n";
        std::fs::write(&path, legacy).unwrap();
        sync_autostart_file(&path, Some(&v1)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1, "legacy entry upgraded");
        // An entry the USER made (e.g. their desktop's "add to autostart") is never
        // rewritten or deleted.
        let users = "[Desktop Entry]\nName=JeTTY\nExec=jetty --show\n";
        std::fs::write(&path, users).unwrap();
        sync_autostart_file(&path, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), users);
        sync_autostart_file(&path, Some(&v1)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), users);
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn huge_osc_titles_are_clipped_at_ingestion() {
        // A program can send a multi-MB OSC 0/2 title; only a displayable head
        // is kept, so the tab bar / OS title / palette never handle it whole.
        let huge = "t".repeat(1 << 20);
        let got = resolve_title(Some(huge), false, "Tab 2").unwrap();
        assert_eq!(got.chars().count(), jetty_render::MAX_LABEL_CHARS);
        // Ordinary titles pass through untouched (no reallocation path).
        assert_eq!(resolve_title(Some("vim ~/x".into()), false, "T").unwrap(), "vim ~/x");
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
mod scheduler_tests {
    //! The pure decisions behind `about_to_wait`'s scheduling and the F9 toggle.
    //! The window itself can't run under `cargo test`, so every rule that keeps
    //! the loop at 0-CPU idle (or makes it wake) is a pure function tested here.
    use super::{
        anim_expired, caret_drives_frames, main_tab_watched, next_acquire_retry,
        perf_idle_decision, toggle_action, IdleHud, ToggleAction, FOCUS_CHURN_GRACE,
        KEY_ECHO_GRACE, RAISE_RETRY_WINDOW,
    };
    use std::time::{Duration, Instant};

    // ── idle HUD one-shot (the hidden-window 100% CPU spin) ──────────────────

    #[test]
    fn idle_hud_never_repaints_or_wakes_for_a_hidden_window() {
        let now = Instant::now();
        let due = Some(now - Duration::from_millis(1));
        let future = Some(now + Duration::from_millis(500));
        // Hidden (or occluded) with the deadline elapsed: requesting the repaint
        // here re-requested it on every loop iteration (the early-returning
        // RedrawRequested never set `perf_idle_shown`) — must be Nothing…
        assert_eq!(perf_idle_decision(true, false, due, false, now), IdleHud::Nothing);
        // …and a future deadline must not even schedule a wake.
        assert_eq!(perf_idle_decision(true, false, future, false, now), IdleHud::Nothing);
    }

    #[test]
    fn idle_hud_repaints_once_when_visible_and_due() {
        let now = Instant::now();
        let due = Some(now - Duration::from_millis(1));
        assert_eq!(perf_idle_decision(true, false, due, true, now), IdleHud::RepaintNow);
        // Exactly once: after the idle frame painted (`idle_shown`) nothing is owed.
        assert_eq!(perf_idle_decision(true, true, due, true, now), IdleHud::Nothing);
    }

    #[test]
    fn idle_hud_wakes_at_a_future_deadline_only() {
        let now = Instant::now();
        let d = now + Duration::from_millis(700);
        assert_eq!(perf_idle_decision(true, false, Some(d), true, now), IdleHud::WakeAt(d));
        // HUD off or never armed → nothing at all.
        assert_eq!(perf_idle_decision(false, false, Some(d), true, now), IdleHud::Nothing);
        assert_eq!(perf_idle_decision(true, false, None, true, now), IdleHud::Nothing);
    }

    // ── F9 / launcher toggle ─────────────────────────────────────────────────
    // Argument order: visible, focused, occluded, focus_lost_at, autohidden_at,
    // last_raise, now.

    #[test]
    fn toggle_shows_a_hidden_window_and_hides_a_watched_one() {
        let now = Instant::now();
        assert_eq!(toggle_action(false, false, false, None, None, None, now), ToggleAction::Show);
        assert_eq!(
            toggle_action(false, true, true, Some(now), None, Some(now), now),
            ToggleAction::Show,
            "a hidden window shows unless an auto-hide just raced this press"
        );
        assert_eq!(toggle_action(true, true, false, None, None, None, now), ToggleAction::Hide);
    }

    #[test]
    fn toggle_raises_a_visible_window_that_is_not_in_front() {
        let now = Instant::now();
        let long_ago = Some(now - Duration::from_secs(2));
        // Genuinely unfocused for 2 s (clicked elsewhere, focus_autohide = false):
        // raise, not hide.
        assert_eq!(
            toggle_action(true, false, false, long_ago, None, None, now),
            ToggleAction::Raise
        );
        // Covered by other windows / minimized: raise, even if it kept focus…
        assert_eq!(toggle_action(true, true, true, None, None, None, now), ToggleAction::Raise);
        // …and even inside the churn grace (occlusion is not grab churn).
        let churn = Some(now - Duration::from_millis(30));
        assert_eq!(toggle_action(true, false, true, churn, None, None, now), ToggleAction::Raise);
    }

    /// The X11 grab-churn sequences measured in Xvfb (global-hotkey F9 via
    /// XTEST): the hotkey's own key grab FocusOuts the focused window just
    /// before the hotkey event reaches the loop; FocusIn only comes on release.
    #[test]
    fn toggle_hides_a_focused_window_through_its_own_hotkey_grab_churn() {
        let t_focus_out = Instant::now();
        // 30 ms tap: Focused(false) → hotkey +20.7 ms (FocusIn at +30 ms, after).
        let at = t_focus_out + Duration::from_millis(21);
        assert_eq!(
            toggle_action(true, false, false, Some(t_focus_out), None, None, at),
            ToggleAction::Hide,
            "FocusOut→toggle within 50 ms is the grab, not a real focus loss"
        );
        // 400 ms hold: Focused(false) → hotkey +29 ms (FocusIn only at +400 ms).
        let at = t_focus_out + Duration::from_millis(29);
        assert_eq!(
            toggle_action(true, false, false, Some(t_focus_out), None, None, at),
            ToggleAction::Hide
        );
        // A very short tap whose FocusIn beat the hotkey event: plainly focused.
        assert_eq!(
            toggle_action(true, true, false, None, None, None, at),
            ToggleAction::Hide
        );
        // A loaded loop delivering the hotkey late is still covered…
        let at = t_focus_out + FOCUS_CHURN_GRACE - Duration::from_millis(1);
        assert_eq!(
            toggle_action(true, false, false, Some(t_focus_out), None, None, at),
            ToggleAction::Hide
        );
        // …while a focus loss older than the grace is a genuine one → raise.
        let at = t_focus_out + FOCUS_CHURN_GRACE;
        assert_eq!(
            toggle_action(true, false, false, Some(t_focus_out), None, None, at),
            ToggleAction::Raise
        );
    }

    #[test]
    fn toggle_keeps_hidden_when_the_autohide_raced_the_same_press() {
        // focus_autohide = true: the grab's FocusOut schedules the 100 ms
        // auto-hide; if the hotkey event is later than that, the auto-hide hides
        // first and the late toggle must NOT re-show the window.
        let t_autohide = Instant::now();
        let at = t_autohide + Duration::from_millis(40);
        assert_eq!(
            toggle_action(false, false, false, None, Some(t_autohide), None, at),
            ToggleAction::Hide,
            "Hide on a hidden window is a no-op: it stays hidden"
        );
        // A press well after an auto-hide is a real summon.
        let at = t_autohide + Duration::from_secs(2);
        assert_eq!(
            toggle_action(false, false, false, None, Some(t_autohide), None, at),
            ToggleAction::Show
        );
    }

    #[test]
    fn toggle_hides_after_a_raise_the_compositor_refused() {
        let t0 = Instant::now();
        let unfocused = Some(t0 - Duration::from_secs(5));
        // The raise never produced focus (e.g. Wayland): a press within the
        // window hides instead of raising forever…
        let soon = t0 + RAISE_RETRY_WINDOW - Duration::from_millis(1);
        assert_eq!(
            toggle_action(true, false, false, unfocused, None, Some(t0), soon),
            ToggleAction::Hide
        );
        // …but a much later press is a fresh intent → raise again.
        let later = t0 + RAISE_RETRY_WINDOW;
        assert_eq!(
            toggle_action(true, false, false, unfocused, None, Some(t0), later),
            ToggleAction::Raise
        );
    }

    // ── failed-acquire retry backoff ─────────────────────────────────────────

    #[test]
    fn acquire_retry_starts_next_frame_and_backs_off_to_one_second() {
        let now = Instant::now();
        let mut r = next_acquire_retry(None, now);
        assert_eq!((r.attempt, r.due - now), (0, Duration::from_millis(16)));
        let mut delays = Vec::new();
        for _ in 0..8 {
            r = next_acquire_retry(Some(r), now);
            delays.push((r.due - now).as_millis());
        }
        assert_eq!(delays, vec![32, 64, 128, 256, 512, 1000, 1000, 1000]);
    }

    #[test]
    fn acquire_retry_never_schedules_in_the_past_or_overflows() {
        let now = Instant::now();
        let r = next_acquire_retry(
            Some(super::AcquireRetry { due: now, attempt: u32::MAX }),
            now,
        );
        assert_eq!(r.attempt, u32::MAX, "saturates instead of wrapping to the 16ms step");
        assert!(r.due > now, "a retry is always strictly in the future (no WaitUntil spin)");
    }

    // ── wall-clock animation expiry ──────────────────────────────────────────

    #[test]
    fn animations_end_by_wall_clock() {
        let t0 = Instant::now();
        assert!(!anim_expired(t0, 0.4, t0 + Duration::from_millis(399)));
        assert!(anim_expired(t0, 0.4, t0 + Duration::from_millis(400)));
        // Zero / negative duration (the `None` summon effect) ends at once.
        assert!(anim_expired(t0, 0.0, t0));
        assert!(anim_expired(t0, -1.0, t0));
        // A start "in the future" (clock skew between captures) is not expired.
        assert!(!anim_expired(t0 + Duration::from_millis(5), 0.15, t0));
    }

    // ── keystroke → echo paint ───────────────────────────────────────────────

    #[test]
    fn caret_burst_pumps_frames_only_after_its_first_paint() {
        let now = Instant::now();
        let due = Some(now + KEY_ECHO_GRACE);
        // Armed by the key but not yet painted by the echo: no Poll frames
        // (they would render the pre-echo frame the deferral avoids).
        assert!(!caret_drives_frames(Some(now), due));
        // The echo's frame cleared the deadline → the burst animates.
        assert!(caret_drives_frames(Some(now), None));
        assert!(!caret_drives_frames(None, None));
    }

    #[test]
    fn key_echo_grace_is_imperceptible() {
        // The fallback is only for keys that never echo; it must stay well
        // under perception (and a 25 ms window comfortably covers zsh's
        // highlighter/autosuggest echo).
        assert!(KEY_ECHO_GRACE <= Duration::from_millis(25));
    }

    // ── Run & Notify "watching" ──────────────────────────────────────────────

    #[test]
    fn only_the_visible_tab_of_a_watched_window_is_watched() {
        assert!(main_tab_watched(true, 2, 2), "the tab on screen is watched");
        assert!(!main_tab_watched(true, 1, 2), "a background tab must still notify");
        assert!(!main_tab_watched(false, 2, 2), "hidden/unfocused window → notify");
    }
}

#[cfg(test)]
mod hot_reload_tests {
    use super::{floor_char_boundary, sanitize_notice, theme_missing_warning};
    use crate::config::hash_str as hash_config_str;

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
    fn notices_cannot_inject_escapes() {
        // Config warnings quote the user's file and are printed INTO the first tab;
        // an ESC/CR/BEL/C1 in a value must arrive as a visible placeholder.
        let s = sanitize_notice("`theme = \"x\u{1b}]52;c;AAA\u{7}\r\n\u{9b}31m\"` is invalid");
        assert!(!s.chars().any(|c| c.is_control()), "{s:?}");
        assert!(s.contains("]52;c;AAA"), "printable text is kept: {s:?}");
        assert!(theme_missing_warning("mine", "Catppuccin Mocha").contains("\"mine\""));
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

    /// Which config keys apply LIVE on hot-reload vs require a RESTART. Mirrors
    /// `apply_reloaded_config` (live keys are applied there; only `summon_hotkey`
    /// is skipped — the global grab is registered once). Test-only classifier so
    /// the documented contract is locked in.
    fn is_restart_only(key: &str) -> bool {
        matches!(key, "summon_hotkey")
    }

    #[test]
    fn live_vs_restart_key_classification() {
        // Restart-only key.
        assert!(is_restart_only("summon_hotkey"));
        // Everything else applies live on reload — including launch_at_login, whose
        // config key is the source of truth for the autostart entry.
        for k in [
            "launch_at_login",
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
            "macos_option_as_alt",
            "copy_on_select",
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
        // 11 = request_main_paint def (1) + request_settings_paint def (1)
        //    + about_to_wait animation/lifecycle drive (6: main/detached reflow
        //      services, search refresh, main_pending, detached_pending,
        //      settings_pending)
        //    + dock re-assert (1) + center re-assert (1)
        //    + main-window-open first-frame nudge on a local `window` binding (1).
        // The render tails no longer self-drive: `about_to_wait` is the ONLY
        // place that decides another frame.
        assert_eq!(
            raw_calls(include_str!("app.rs")),
            11,
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

#[cfg(test)]
mod fullscreen_helper_tests {
    //! The three pure fullscreen predicates. All private to `app.rs`, so the
    //! tests live here.
    use super::{
        center_reassert_ok, centered_pos, dock_reassert_ok, effective_corner_radius_px,
        fullscreen_exit_frames, WindowMode,
    };

    #[test]
    fn fullscreen_exit_never_arms_a_counter_while_hidden() {
        // THE ~0%-idle invariant. Reachable via the Settings WINDOW MODE cycler or a
        // `window_mode` hot-reload while the main window is auto-hidden: a counter
        // armed while hidden pins ControlFlow::Poll (it can only decrement inside
        // RedrawRequested, which a hidden macOS window never receives) and on X11
        // also fires a dock/SIGWINCH storm at every hidden tab.
        for mode in WindowMode::ORDER {
            for maximized in [false, true] {
                for restorable in [false, true] {
                    assert_eq!(
                        fullscreen_exit_frames(false, maximized, mode, restorable),
                        (0, 0),
                        "armed a counter while HIDDEN: {mode:?} max={maximized} pos={restorable}"
                    );
                }
            }
        }
    }

    #[test]
    fn fullscreen_exit_frames_match_the_reassertion_predicates() {
        use WindowMode::{Center, Dropdown, Fullscreen};
        // Visible, not maximized: Dropdown re-docks; Center/Fullscreen re-assert
        // the position they issued — the saved `last_pos` when restorable, the
        // COMPUTED centre otherwise. BOTH race the WM's own frame restore, so a
        // centred fallback needs the re-assertion just as much (leaving it at
        // (0,0) is what dumped the window in the monitor's top-left corner).
        assert_eq!(fullscreen_exit_frames(true, false, Dropdown, false), (5, 0));
        assert_eq!(fullscreen_exit_frames(true, false, Dropdown, true), (5, 0));
        assert_eq!(fullscreen_exit_frames(true, false, Center, true), (0, 5));
        assert_eq!(fullscreen_exit_frames(true, false, Center, false), (0, 5));
        assert_eq!(fullscreen_exit_frames(true, false, Fullscreen, true), (0, 5));
        assert_eq!(fullscreen_exit_frames(true, false, Fullscreen, false), (0, 5));
        // Maximized: no geometry restore at all, so no re-assertion either — the WM
        // restores the maximized frame itself.
        for mode in WindowMode::ORDER {
            assert_eq!(fullscreen_exit_frames(true, true, mode, true), (0, 0));
        }
        // Whatever is armed must be something the RedrawRequested guard will
        // actually let run (with fullscreen now false) — otherwise the counter would
        // be zeroed unused, i.e. dead arming.
        for mode in WindowMode::ORDER {
            for restorable in [false, true] {
                let (d, c) = fullscreen_exit_frames(true, false, mode, restorable);
                assert!(d == 0 || dock_reassert_ok(mode, false), "dead dock arming for {mode:?}");
                assert!(
                    c == 0 || center_reassert_ok(mode, false),
                    "dead center arming for {mode:?}"
                );
            }
        }
    }

    #[test]
    fn corner_radius_band_dims_for_the_mode_not_just_the_live_shape() {
        use super::corner_radius_band_dimmed;
        use WindowMode::{Center, Dropdown, Fullscreen};
        // The state the user actually reaches: opening Settings EXITS fullscreen,
        // so `main_fullscreen` is false while the panel is on screen. Feeding the
        // panel that flag alone made the dim unobservable in the real app.
        assert!(corner_radius_band_dimmed(false, Fullscreen));
        // Switching WINDOW MODE to Fullscreen in Settings dims immediately.
        assert!(!corner_radius_band_dimmed(false, Center));
        assert!(!corner_radius_band_dimmed(false, Dropdown));
        // The ad-hoc F11 case (Center/Dropdown + fullscreen shape) still dims.
        assert!(corner_radius_band_dimmed(true, Center));
        assert!(corner_radius_band_dimmed(true, Dropdown));
        assert!(corner_radius_band_dimmed(true, Fullscreen));
        // The dim must be true exactly when the radius is actually suppressed for
        // the shape the panel is describing — i.e. it never lies in the direction
        // that matters (a live fullscreen window with an un-dimmed band).
        for mode in WindowMode::ORDER {
            for fs in [false, true] {
                if effective_corner_radius_px(10.0, 1.0, fs) == 0.0 && fs {
                    assert!(corner_radius_band_dimmed(fs, mode));
                }
            }
        }
    }

    #[test]
    fn a_hide_never_leaves_a_reflow_armed_and_a_summon_pays_it_back_once() {
        use super::{reflow_terms_on_hide, reflow_terms_on_summon};
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let armed = now + Duration::from_millis(250);
        let next = now + Duration::from_millis(900);

        // THE INVARIANT: a hide NEVER leaves a live deadline behind, whatever the
        // state — a reflow that fires while hidden SIGWINCHes every shell to a grid
        // the user never sees (and on the next summon it all resizes back: two
        // reflows per F9 cycle, the p10k-scatter trigger).
        for pending in [None, Some(armed)] {
            for deferred in [false, true] {
                assert_eq!(reflow_terms_on_hide(pending, deferred).0, None);
            }
        }
        // A pending reflow becomes a DEBT rather than being silently dropped:
        // `gpu.resize` already ran at the `Resized`, so grid ≠ surface until one
        // reflow runs.
        assert_eq!(reflow_terms_on_hide(Some(armed), false), (None, true));
        assert_eq!(reflow_terms_on_hide(Some(armed), true), (None, true));
        // Nothing pending ⇒ no debt invented (a plain Center hide must not cost a
        // reflow on the next summon).
        assert_eq!(reflow_terms_on_hide(None, false), (None, false));
        // A debt survives a hide that had nothing pending of its own.
        assert_eq!(reflow_terms_on_hide(None, true), (None, true));

        // The summon pays the debt EXACTLY once, at the new geometry's deadline…
        assert_eq!(reflow_terms_on_summon(None, true, next), (Some(next), false));
        // …and a second summon does not fire another one.
        assert_eq!(reflow_terms_on_summon(None, false, next), (None, false));
        // A hide→summon round-trip is idempotent on the debt flag.
        let (p, d) = reflow_terms_on_hide(Some(armed), false);
        let (p, d) = reflow_terms_on_summon(p, d, next);
        assert_eq!((p, d), (Some(next), false));
        let (p2, d2) = reflow_terms_on_summon(p, d, next);
        assert_eq!((p2, d2), (Some(next), false), "no extra reflow armed");
    }

    #[test]
    fn centering_a_monitor_sized_window_is_the_corner_bug() {
        // The windowed case: a 1000x640 window on a 1920x1200 monitor at the
        // origin, and on a secondary monitor at x=1920 (the monitor origin must be
        // added, or a multi-monitor setup centres on the wrong screen).
        assert_eq!(centered_pos((0, 0), (1920, 1200), (1000, 640)), (460, 280));
        assert_eq!(
            centered_pos((1920, 0), (1920, 1200), (1000, 640)),
            (2380, 280)
        );
        // THE REGRESSION (B1): feeding the maths the size a STILL-FULLSCREEN
        // window reports (== the monitor) collapses the centre onto the monitor
        // ORIGIN — the window lands flush in the top-left corner. This is why the
        // exit path must pass `last_windowed_size`, not `win.outer_size()`.
        assert_eq!(centered_pos((0, 0), (1920, 1200), (1920, 1200)), (0, 0));
        assert_eq!(
            centered_pos((1920, 0), (1920, 1200), (1920, 1200)),
            (1920, 0)
        );
        // …and with the captured windowed size the SAME exit lands centred.
        assert_ne!(
            centered_pos((0, 0), (1920, 1200), (1000, 640)),
            centered_pos((0, 0), (1920, 1200), (1920, 1200)),
        );
        // A window LARGER than the monitor pins to the origin (saturating), never
        // wraps to a huge negative that would map it off-screen.
        assert_eq!(centered_pos((0, 0), (1280, 800), (1920, 1200)), (0, 0));
        assert_eq!(centered_pos((-1920, -100), (1920, 1200), (1000, 640)), (-1460, 180));
    }

    #[test]
    fn help_rows_include_fullscreen() {
        // The live keymap-driven rows and the static mirror must stay
        // format-identical, and BOTH default chords must appear (`all()`, not
        // `first()`) — bare F11 is dead on macOS keyboards without standard
        // function keys, so the companion chord is the discoverable one there.
        let rows = super::App::compute_help_rows(&crate::keymap::KeyMap::defaults(), "F9");
        let live = rows
            .iter()
            .find(|r| r.contains("Fullscreen (whole monitor)"))
            .expect("no fullscreen help row");
        assert!(live.contains("F11"), "{live:?}");
        assert!(live.contains(" — "), "sectioned 'KEY — desc' shape: {live:?}");
        if cfg!(target_os = "macos") {
            assert!(live.contains("Ctrl+") && live.contains("Cmd+"), "{live:?}");
        }
        assert!(
            jetty_render::HELP_ROWS
                .iter()
                .any(|r| *r == "F11 — Fullscreen (whole monitor)"),
            "static HELP_ROWS mirror is missing the fullscreen row"
        );
        // It lives in the window/appearance section (the first `## ` block).
        let idx = rows.iter().position(|r| r == live).unwrap();
        let header = rows[..idx].iter().rfind(|r| r.starts_with("## ")).unwrap();
        assert_eq!(header, "## Tabs & windows");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn help_rows_static_mirror_matches_the_default_keymap_exactly() {
        // The static jetty_render::HELP_ROWS must equal the live rows for the
        // default keymap (macOS adds Cmd companions, so Linux-only), so changing
        // a default chord can never leave the fallback overlay stale.
        let rows = super::App::compute_help_rows(&crate::keymap::KeyMap::defaults(), "F9");
        assert_eq!(rows, jetty_render::default_help_rows());
    }

    #[test]
    fn help_rows_include_run_selection_and_mirror_matches() {
        // Live row: default chord + the staged-multiline note, in the
        // clipboard section; static HELP_ROWS mirror carries the same row
        // verbatim (the default chord pretty-prints as Ctrl+Shift+Enter).
        let rows = super::App::compute_help_rows(&crate::keymap::KeyMap::defaults(), "F9");
        let live = rows
            .iter()
            .find(|r| r.contains("Run selection in a new tab"))
            .expect("no run-selection help row");
        assert!(live.contains("Ctrl+Shift+Enter"), "{live:?}");
        assert!(live.contains(" — "), "sectioned 'KEY — desc' shape: {live:?}");
        assert!(
            jetty_render::HELP_ROWS.iter().any(|r| *r == live.as_str()),
            "static HELP_ROWS mirror out of sync with the live row: {live:?}"
        );
        let idx = rows.iter().position(|r| r == live).unwrap();
        let header = rows[..idx].iter().rfind(|r| r.starts_with("## ")).unwrap();
        assert_eq!(header, "## Clipboard & selection");
        // Copy-mode's row documents `r` in BOTH sources.
        let cm = rows
            .iter()
            .find(|r| r.contains("Copy-mode"))
            .expect("no copy-mode help row");
        assert!(cm.contains("r = run"), "{cm:?}");
        assert!(
            jetty_render::HELP_ROWS
                .iter()
                .any(|r| r.contains("Copy-mode") && r.contains("r = run")),
            "static copy-mode row is missing 'r = run'"
        );
    }

    #[test]
    fn corner_radius_flat_while_fullscreen() {
        // Windowed: logical radius × scale (HiDPI-correct).
        assert_eq!(effective_corner_radius_px(10.0, 2.0, false), 20.0);
        assert_eq!(effective_corner_radius_px(10.0, 1.0, false), 10.0);
        // Fullscreen: FLAT, whatever the configured radius or scale.
        assert_eq!(effective_corner_radius_px(10.0, 2.0, true), 0.0);
        assert_eq!(effective_corner_radius_px(24.0, 3.0, true), 0.0);
        // A configured 0 stays 0 either way (already flat).
        assert_eq!(effective_corner_radius_px(0.0, 1.0, false), 0.0);
        assert_eq!(effective_corner_radius_px(0.0, 1.0, true), 0.0);
        // Fullscreen radii are all-flat, so `CornerMask::apply` skips the pass.
        assert!(jetty_render::all_radii_flat(
            effective_corner_radius_px(24.0, 2.0, true),
            effective_corner_radius_px(24.0, 2.0, true),
            effective_corner_radius_px(24.0, 2.0, true),
            effective_corner_radius_px(24.0, 2.0, true),
        ));
    }

    #[test]
    fn reassert_guards_suppressed_while_fullscreen() {
        use WindowMode::{Center, Dropdown, Fullscreen};
        // Dock re-assertion: Dropdown only, and never while fullscreen. The
        // fullscreen case is the explicitly-supported "Dropdown + ad-hoc F11".
        assert!(dock_reassert_ok(Dropdown, false));
        assert!(!dock_reassert_ok(Dropdown, true));
        assert!(!dock_reassert_ok(Center, false));
        assert!(!dock_reassert_ok(Center, true));
        assert!(!dock_reassert_ok(Fullscreen, false));
        assert!(!dock_reassert_ok(Fullscreen, true));
        // Center re-assertion: every NON-Dropdown mode (so an ad-hoc F11 EXIT in
        // Fullscreen mode can restore `last_pos`), and never while fullscreen.
        assert!(center_reassert_ok(Center, false));
        assert!(!center_reassert_ok(Center, true));
        assert!(center_reassert_ok(Fullscreen, false));
        assert!(!center_reassert_ok(Fullscreen, true));
        assert!(!center_reassert_ok(Dropdown, false));
        assert!(!center_reassert_ok(Dropdown, true));
        // The two are mutually exclusive in every state — no mode/flag combination
        // can arm both geometry re-assertions at once.
        for m in WindowMode::ORDER {
            for fs in [false, true] {
                assert!(
                    !(dock_reassert_ok(m, fs) && center_reassert_ok(m, fs)),
                    "both re-assertions enabled for {m:?} fullscreen={fs}"
                );
            }
        }
    }
}

#[cfg(test)]
mod window_mode_tests {
    //! `WindowMode` is a private-ish app type whose `display_name` /
    //! `from_config` / `to_config` / `cycle` / `ORDER` are private, so these
    //! tests live in `app.rs` itself (a `tests/` integration test could not see
    //! them).
    use super::WindowMode;

    #[test]
    fn window_mode_config_round_trip() {
        for m in [WindowMode::Center, WindowMode::Dropdown, WindowMode::Fullscreen] {
            assert_eq!(
                WindowMode::from_config(m.to_config()),
                m,
                "round-trip failed for {m:?}"
            );
        }
        assert_eq!(WindowMode::from_config("fullscreen"), WindowMode::Fullscreen);
        assert_eq!(WindowMode::Fullscreen.to_config(), "fullscreen");
        assert_eq!(WindowMode::Fullscreen.display_name(), "Fullscreen");
        // Case-SENSITIVE and unknown-tolerant, exactly as before (which is what
        // makes an OLDER JeTTY reading "fullscreen" fall back to Center).
        assert_eq!(WindowMode::from_config("Fullscreen"), WindowMode::Center);
        assert_eq!(WindowMode::from_config("FULLSCREEN"), WindowMode::Center);
        assert_eq!(WindowMode::from_config("garbage"), WindowMode::Center);
        assert_eq!(WindowMode::from_config(""), WindowMode::Center);
    }

    #[test]
    fn window_mode_cycle_order_over_three() {
        assert_eq!(WindowMode::ORDER.len(), 3);
        // Every variant appears exactly once in ORDER.
        for m in [WindowMode::Center, WindowMode::Dropdown, WindowMode::Fullscreen] {
            assert_eq!(
                WindowMode::ORDER.iter().filter(|&&x| x == m).count(),
                1,
                "{m:?} must appear exactly once in ORDER"
            );
        }
        // Forward: Center → Dropdown → Fullscreen → Center.
        assert_eq!(WindowMode::Center.cycle(true), WindowMode::Dropdown);
        assert_eq!(WindowMode::Dropdown.cycle(true), WindowMode::Fullscreen);
        assert_eq!(WindowMode::Fullscreen.cycle(true), WindowMode::Center);
        // Backward is the exact inverse.
        for m in WindowMode::ORDER {
            assert_eq!(m.cycle(true).cycle(false), m);
            assert_eq!(m.cycle(false).cycle(true), m);
            // Three forward steps are the identity.
            assert_eq!(m.cycle(true).cycle(true).cycle(true), m);
        }
    }

    #[test]
    fn window_mode_display_names_are_distinct_and_cycler_sized() {
        let names: Vec<&str> = WindowMode::ORDER.iter().map(|m| m.display_name()).collect();
        assert_eq!(names, vec!["Center", "Dropdown", "Fullscreen"]);
        // The Settings cycler ellipsizes past ~11 chars at the default UI font
        // (panel.rs `cycle_max_chars`); keep every label inside that budget.
        for n in names {
            assert!(n.chars().count() <= 11, "cycler label too long: {n:?}");
        }
    }
}

#[cfg(test)]
mod window_parity_tests {
    use super::{
        detach_logical_size, gpu_recovery_due, pace_paint, perf_hud_text, refresh_interval, settings_drag_end,
        settings_fullscreen_exit, smooth_frame_ms, FullscreenExit, PaintPacing, SettingsDragEnd,
        GPU_REBUILD_RETRY,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn interactive_output_always_paints_immediately() {
        // An echo / prompt (not a flood) never waits — even right after a frame.
        let now = Instant::now();
        let hz60 = refresh_interval(Some(60_000));
        assert_eq!(pace_paint(false, Some(now), hz60, now), PaintPacing::Now);
        assert_eq!(pace_paint(false, None, hz60, now), PaintPacing::Now);
    }

    #[test]
    fn flood_paints_at_most_once_per_refresh() {
        let t0 = Instant::now();
        let hz60 = refresh_interval(Some(60_000));
        // First flood frame (nothing presented yet / long ago) paints now.
        assert_eq!(pace_paint(true, None, hz60, t0), PaintPacing::Now);
        assert_eq!(pace_paint(true, Some(t0), hz60, t0 + hz60), PaintPacing::Now);
        // Within the refresh: deferred to exactly one interval after the present.
        assert_eq!(pace_paint(true, Some(t0), hz60, t0 + Duration::from_millis(3)), PaintPacing::At(t0 + hz60));
    }

    #[test]
    fn refresh_interval_follows_the_monitor_and_stays_sane() {
        assert_eq!(refresh_interval(Some(60_000)), Duration::from_micros(16_666));
        assert_eq!(refresh_interval(Some(144_000)), Duration::from_micros(6_944));
        // Unknown / bogus rates: 60 Hz, and clamped to 20–250 Hz.
        assert_eq!(refresh_interval(None), Duration::from_micros(16_667));
        assert_eq!(refresh_interval(Some(0)), Duration::from_micros(16_667));
        assert_eq!(refresh_interval(Some(1_000)), Duration::from_micros(50_000));
        assert_eq!(refresh_interval(Some(1_000_000)), Duration::from_micros(4_000));
    }

    #[test]
    fn gpu_recovery_runs_on_loss_and_backs_off_after_a_failed_rebuild() {
        let now = Instant::now();
        assert!(!gpu_recovery_due(false, None, now), "nothing lost → nothing to do");
        assert!(gpu_recovery_due(true, None, now), "a loss is rebuilt at once");
        // A failed rebuild waits for its backoff — never a per-frame retry loop…
        assert!(!gpu_recovery_due(true, Some(now + GPU_REBUILD_RETRY), now));
        // …and retries once it elapses.
        assert!(gpu_recovery_due(true, Some(now), now));
    }

    /// Tripwire for the single active-tab path: `self.active` may be ASSIGNED
    /// only by `set_active_tab` plus the index fix-ups of the paths that REMOVE a
    /// tab (close / detach incl. its failed-init restore / shell exit — they keep
    /// the index pointing at the same tab or clamp it, then call
    /// `entered_new_active_tab` when the active tab itself went away) and the
    /// first tab in `resumed`. A new direct assignment would skip the outgoing
    /// tab's search/hint/copy/selection reset again — route it through
    /// `set_active_tab` (or, for a new removal path, `entered_new_active_tab`).
    #[test]
    fn active_tab_changes_go_through_one_path() {
        let src = include_str!("app.rs");
        let assigns = src
            .lines()
            .filter(|l| {
                let s = l.trim_start();
                !s.starts_with("//")
                    && (s.contains(concat!("self.active ", "= ")) || s.contains(concat!("self.active ", "-= ")))
            })
            .count();
        // close_tab (2) + detach_tab (2 fix-ups + 1 restore) + set_active_tab (1)
        // + close_exited_tabs (2) + resumed (1).
        assert_eq!(assigns, 9, "a new direct `self.active` assignment — use set_active_tab");
    }

    #[test]
    fn detaching_uses_the_live_surface_size_in_logical_px() {
        assert_eq!(detach_logical_size(Some((2000, 1280)), None, false, 2.0), (1000, 640));
        assert_eq!(detach_logical_size(Some((1201, 800)), Some((10, 10)), false, 1.0), (1201, 800));
    }

    #[test]
    fn detaching_from_fullscreen_uses_the_windowed_size_not_the_monitor() {
        // Main was fullscreen on a 3840x2160 @2x monitor; it had been 1600x1000.
        let monitor = Some((3840, 2160));
        assert_eq!(detach_logical_size(monitor, Some((1600, 1000)), true, 2.0), (800, 500));
        // Never captured (no entry recorded) → the surface is all there is.
        assert_eq!(detach_logical_size(monitor, None, true, 1.0), (3840, 2160));
    }

    #[test]
    fn detach_size_has_a_fallback_and_never_zero() {
        assert_eq!(detach_logical_size(None, None, false, 1.0), (1000, 640));
        assert_eq!(detach_logical_size(Some((1, 1)), None, false, 4.0), (1, 1));
        // A bogus scale never divides by zero.
        assert_eq!(detach_logical_size(Some((800, 600)), None, false, 0.0), (800, 600));
    }

    #[test]
    fn interrupted_settings_drags_persist_and_dropdown_drags_redock() {
        // No drag: a stray release / focus loss / close writes nothing.
        assert_eq!(settings_drag_end(false, false), SettingsDragEnd { persist: false, redock: false });
        // Opacity / radius / Effects slider: persist the live value.
        assert_eq!(settings_drag_end(true, false), SettingsDragEnd { persist: true, redock: false });
        // Dropdown height/width: persist AND re-dock at the new size.
        assert_eq!(settings_drag_end(false, true), SettingsDragEnd { persist: true, redock: true });
        assert_eq!(settings_drag_end(true, true), SettingsDragEnd { persist: true, redock: true });
    }

    #[test]
    fn settings_leaves_fullscreen_only_on_the_requesting_window() {
        // Requested from the main window (no detached window focused).
        assert_eq!(settings_fullscreen_exit(true, None), FullscreenExit::Main);
        assert_eq!(settings_fullscreen_exit(false, None), FullscreenExit::None);
        // Requested from a fullscreen detached window: that one exits; a
        // fullscreen MAIN window is left alone.
        assert_eq!(settings_fullscreen_exit(false, Some((2, true))), FullscreenExit::Detached(2));
        assert_eq!(settings_fullscreen_exit(true, Some((0, true))), FullscreenExit::Detached(0));
        // Requested from a windowed detached window: nobody is touched, even
        // with the main window fullscreen.
        assert_eq!(settings_fullscreen_exit(true, Some((1, false))), FullscreenExit::None);
    }

    #[test]
    fn frame_time_is_smoothed_per_window_and_restarts_after_idle() {
        let t0 = Instant::now();
        let (mut ms, mut last) = (0.0f32, None);
        smooth_frame_ms(&mut ms, &mut last, t0);
        assert_eq!(ms, 0.0, "the first frame has no previous one");
        smooth_frame_ms(&mut ms, &mut last, t0 + Duration::from_millis(16));
        assert!((ms - 16.0).abs() < 0.5, "seeded with the first dt, got {ms}");
        smooth_frame_ms(&mut ms, &mut last, t0 + Duration::from_millis(48));
        assert!((ms - 17.6).abs() < 0.5, "0.9·16 + 0.1·32, got {ms}");
        // A 5 s idle gap neither spikes nor resets the mean…
        smooth_frame_ms(&mut ms, &mut last, t0 + Duration::from_millis(5048));
        assert!((ms - 17.6).abs() < 0.5, "an idle gap must not spike it, got {ms}");
        // …but the next frame measures from the end of the gap.
        smooth_frame_ms(&mut ms, &mut last, t0 + Duration::from_millis(5064));
        assert!((ms - 17.4).abs() < 0.5, "got {ms}");
    }

    #[test]
    fn hud_text_reports_fps_from_frame_time() {
        assert_eq!(perf_hud_text(16.0, 3.4, 12.6), "⚡ 16.0 ms · 63 fps · 3% CPU · 13 MB/s");
        assert_eq!(perf_hud_text(0.0, 0.0, 0.0), "⚡ 0.0 ms · 0 fps · 0% CPU · 0 MB/s");
        assert_eq!(perf_hud_text(-1.0, 0.0, 0.0), "⚡ 0.0 ms · 0 fps · 0% CPU · 0 MB/s");
    }
}
