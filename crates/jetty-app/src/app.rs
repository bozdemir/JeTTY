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

/// Default logical (device-independent) font size in points. This is the value
/// used when the user resets the font size with Ctrl+0 and on first launch.
/// Scaled by the display's scale_factor before being passed to TextLayer so
/// glyphs are rendered at physical-pixel resolution on HiDPI screens.
const FONT_LOGICAL_DEFAULT: f32 = 16.0;

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

/// A single terminal session: its grid model, PTY, writer, and tab title. One
/// `Tab` per visible tab. Per-tab scroll/selection live inside `terminal`.
struct Tab {
    terminal: Terminal,
    pty: PtySession,
    writer: Box<dyn Write + Send>,
    title: String,
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
    /// and this pass blits it to the surface (passthrough for now). Built in
    /// `resumed` with the surface format; `None` until then.
    crt: Option<jetty_render::Crt>,
    /// Optional GPU caret glow/ripple pass (Task 12). Additive halo + expanding
    /// ring around the cursor cell on each keystroke burst. Built in `resumed`
    /// with the surface format; dispatched only when `fx.caret_glow_enabled` AND
    /// `caret_anim.is_some()` AND the cursor is visible — zero cost otherwise.
    caret_fx: Option<jetty_render::CaretFx>,
    /// Surface-sized offscreen color texture used ONLY while a Tier-B effect is
    /// summoning: the scene is rendered into this, then the effect samples it and
    /// writes the displaced/blurred result to the surface. `None` until built in
    /// `resumed`; re-created on `Resized`. The normal (Tier-A / no-summon) hot
    /// path never touches it — it renders straight to the surface as before.
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
    /// The id of the most recently focused window (main or settings). Used to
    /// suppress auto-hide when focus moved to our own Settings window.
    last_focused_window: Option<WindowId>,
    /// Set when the Settings window gains focus; consumed by the main window's
    /// Focused(false) to suppress auto-hide even when X11 delivers the main
    /// Focused(false) BEFORE the settings Focused(true) (the last_focused_window
    /// check alone loses that race).
    switching_to_settings: bool,
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
    /// Index into jetty_core::theme::PRESETS for the current theme.
    theme_idx: usize,
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
    /// Track held modifier keys so Ctrl+Shift combos can be detected.
    modifiers: winit::keyboard::ModifiersState,
    /// Last known cursor position in physical pixels.
    cursor: (f64, f64),
    /// Where a no-Shift press began while a mouse-reporting app was active (the
    /// press was forwarded to the app). On release, if the cursor moved, the user
    /// was likely trying to select — surface the Shift+drag hint. `take`n on release.
    mouse_grab_press: Option<(f64, f64)>,
    /// While `Some(t)` and `now < t`, the "Hold Shift to select" toast is drawn.
    shift_hint_until: Option<std::time::Instant>,
    /// Throttle: the toast won't re-arm until `now` passes this instant.
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

    /// Whether the in-window "Keyboard Shortcuts" help overlay is open. Drawn on
    /// top of everything in the main window; dismissed by Esc, the "?" button,
    /// or a click outside the panel.
    help_open: bool,
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
}

/// Which resize zone (if any) the cursor is over on the borderless main window.
/// Corners take priority over edges; `None` means a normal cursor / no resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeZone {
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
    fn direction(self) -> Option<winit::window::ResizeDirection> {
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
    fn cursor_icon(self) -> winit::window::CursorIcon {
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
fn resize_zone_at(cx: f32, cy: f32, w: u32, h: u32) -> ResizeZone {
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

impl App {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        // Resolve initial theme index from JETTY_THEME env var.
        let theme_name = std::env::var("JETTY_THEME").unwrap_or_default();
        let theme_idx = jetty_core::theme::PRESETS
            .iter()
            .position(|&n| n == theme_name.as_str())
            .unwrap_or(0);

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
            launch_at_login: false,
            summon_hotkey: "F9".to_string(),
            shell: String::new(),
            cached_top_flush: false,
            cached_tabs_meta: Vec::new(),
            cached_tabs_sig: u64::MAX,
            last_focused_window: None,
            switching_to_settings: false,
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
            opacity,
            font_logical: FONT_LOGICAL_DEFAULT,
            reflow_pending_at: None,
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
            modifiers: winit::keyboard::ModifiersState::empty(),
            cursor: (0.0, 0.0),
            mouse_grab_press: None,
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
            // Force the first CPU refresh to run on the first HUD frame.
            last_cpu_at: std::time::Instant::now() - std::time::Duration::from_secs(2),
            perf_cpu: 0.0,
            vt_bytes: 0,
            vt_bytes_at_window_start: 0,
            vt_window_start: std::time::Instant::now(),
            perf_mb: 0.0,
            perf_idle_at: None,
            perf_idle_shown: false,
            perf_label: None,
            help_open: false,
            confirm_close: None,
            confirm_quit: false,
            last_pos: None,
        };
        // Persisted user settings override the env-derived defaults (but env
        // vars still seed the initial values above, so an explicit JETTY_* can
        // win on a fresh config). Apply config BEFORE the first render so the
        // window comes up already themed/sized as the user left it. The font
        // size/family are consumed later by `resumed` when it builds the
        // TextLayer; theme+opacity are pushed into the terminals by apply_theme.
        let cfg = crate::config::Config::load();
        if let Some(i) = jetty_core::theme::PRESETS.iter().position(|&n| n == cfg.theme.as_str()) {
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

        // Apply the initial theme+opacity so Terminal::new env defaults are
        // overridden by our managed state (avoids double-reads from env).
        app.apply_theme();
        app
    }

    /// Write the current user-tweakable settings to the on-disk config file.
    /// Called whenever a setting changes (theme, opacity, font size/family,
    /// corner radius). Best-effort and cheap; errors are swallowed by `save`.
    fn persist(&self) {
        crate::config::Config {
            theme: jetty_core::theme::PRESETS[self.theme_idx].to_string(),
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
            // show_welcome/show_perf_hud are startup preferences (no runtime UI
            // toggles them), cached at startup — write them back from memory so a
            // settings change never re-reads the config file (persist() used to do
            // TWO full Config::load() reads per call, i.e. 2–4 disk reads per
            // settings click). The cached values preserve a user's manual TOML
            // choice exactly as the on-disk read did.
            show_welcome: self.cfg_show_welcome,
            show_perf_hud: self.show_perf_hud,
            effects: self.fx.clone(),
        }
        .save();
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
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

    /// Build the current theme from `theme_idx` with `opacity` applied to its bg
    /// alpha. Shared by `apply_theme` and the tab bar.
    fn current_theme(&self) -> jetty_core::Theme {
        let mut t = jetty_core::Theme::by_name(jetty_core::theme::PRESETS[self.theme_idx]);
        t.bg[3] = (self.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        t
    }

    /// Build the current theme from `theme_idx`, apply `opacity` to its bg
    /// alpha, and push it into EVERY tab's terminal.
    fn apply_theme(&mut self) {
        let t = self.current_theme();
        for tab in &mut self.tabs {
            tab.terminal.set_theme(t.clone());
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
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
    }

    /// Spawn a new tab sized to the current grid, themed like the others, make it
    /// active, and redraw. The new PTY shares the same wake proxy so one
    /// `AppEvent::Wake` drains every tab.
    fn new_tab(&mut self) {
        let (cols, rows) = self.grid_dims();
        let proxy_wake = self.proxy.clone();
        let shell = self.opt_shell();
        let pty = match PtySession::spawn(cols as u16, rows as u16, shell, move || {
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
        let title = format!("Tab {}", self.tabs.len() + 1);
        self.tabs.push(Tab { terminal, pty, writer, title });
        self.active = self.tabs.len() - 1;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Close tab `i` (its PtySession Drop kills the child). Fix up `active`. If no
    /// tabs remain, exit the event loop.
    fn close_tab(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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

    /// Switch to the next (`+1`) or previous (`-1`) tab, wrapping around.
    fn switch_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n <= 1 {
            return;
        }
        self.active = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        // A pending text selection belongs to the previous tab's grid; reset it.
        self.selecting = false;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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
        }
        &self.cached_tabs_meta
    }

    /// Jump to tab `n` (0-based), clamped to the valid range.
    fn select_tab(&mut self, n: usize) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = n.min(self.tabs.len() - 1);
        // A pending text selection belongs to the previous tab's grid; reset it.
        self.selecting = false;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Commit an in-progress tab rename: write `rename_buf` back to the tab's
    /// title and clear the rename state. No-op when not renaming. An empty buffer
    /// is ignored (keep the previous title) so a tab never ends up nameless.
    fn commit_rename(&mut self) {
        if let Some(i) = self.renaming.take() {
            let trimmed = self.rename_buf.trim();
            if i < self.tabs.len() && !trimmed.is_empty() {
                self.tabs[i].title = trimmed.to_string();
            }
            self.rename_buf.clear();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Compute the scroll offset from the current cursor position during a drag.
    /// `w` and `h` are the current surface dimensions in physical pixels.
    fn apply_scroll_from_cursor(&mut self, w: u32, h: u32) {
        let max = self.active_tab().terminal.scroll_max();
        if max == 0 {
            return;
        }
        // The scrollbar track spans the grid area (always TABBAR_H shorter than
        // the surface, whichever side the bar is on), minus a small GAP at each
        // end. Must match scrollbar_rect_geom so the drawn thumb and the drag map
        // agree.
        const GAP: f32 = 4.0;
        let track_h = (h as f32 - TABBAR_H - self.status_h() - GAP * 2.0).max(0.0);
        // Recompute thumb height the same way as the geometry function.
        let rows = self.active_tab().terminal.rows();
        let total = rows + max;
        let thumb_h = (track_h * rows as f32 / total as f32).max(24.0);

        let travel = track_h - thumb_h;
        if travel <= 0.0 {
            return;
        }

        // Cursor y is absolute; subtract the grid origin so 0 == track top.
        let thumb_top = ((self.cursor.1 as f32 - self.grid_top_offset() - GAP) - self.drag_grab_dy).clamp(0.0, travel);
        // frac=0 → thumb at top → scroll_offset=max (oldest history)
        // frac=1 → thumb at bottom → scroll_offset=0 (live bottom)
        let frac = thumb_top / travel;
        let offset = ((1.0 - frac) * max as f32).round() as usize;
        self.active_tab_mut().terminal.scroll_to_offset(offset);
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
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
    /// coordinates `(col, row)` using the renderer's cell size. Returns `None`
    /// when the renderer (and thus cell metrics) is not yet available.
    fn cursor_cell(&self) -> Option<(usize, usize)> {
        let (cell_w, cell_h) = self.text.as_ref()?.cell_size();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let col = (self.cursor.0 as f32 / cell_w).floor() as i64 + 1;
        // Subtract the grid's pixel origin before dividing (0 when the bar is at
        // the bottom, TABBAR_H when at the top).
        let y = (self.cursor.1 as f32 - self.grid_top_offset()).max(0.0);
        let row = (y / cell_h).floor() as i64 + 1;
        Some((col.max(1) as usize, row.max(1) as usize))
    }

    /// Convert the current cursor pixel position into 0-based viewport cell
    /// coordinates `(line, col)` clamped to the terminal grid. Returns `None`
    /// when the renderer is not yet available.
    fn cursor_cell_0(&self) -> Option<(usize, usize)> {
        let (cell_w, cell_h) = self.text.as_ref()?.cell_size();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let cols = self.active_tab().terminal.cols().saturating_sub(1);
        let rows = self.active_tab().terminal.rows().saturating_sub(1);
        // Subtract the grid's pixel origin (0 at bottom, TABBAR_H at top).
        let y = (self.cursor.1 as f32 - self.grid_top_offset()).max(0.0);
        let col = ((self.cursor.0 as f32 / cell_w).floor() as i64).clamp(0, cols as i64) as usize;
        let line = ((y / cell_h).floor() as i64).clamp(0, rows as i64) as usize;
        Some((line, col))
    }

    /// Paste `text` to the PTY, wrapping in bracketed-paste sequences if the
    /// running application has enabled `\e[?2004h`.
    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.tabs.is_empty() {
            return;
        }
        let bracketed = self.active_tab().terminal.bracketed_paste();
        let w = &mut self.tabs[self.active].writer;
        if bracketed {
            let _ = w.write_all(b"\x1b[200~");
        }
        let _ = w.write_all(text.as_bytes());
        if bracketed {
            let _ = w.write_all(b"\x1b[201~");
        }
        let _ = w.flush();
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
    /// Returns `(active_had_data, exited)` where `active_had_data` is true if the
    /// ACTIVE tab consumed bytes (so the caller redraws), and `exited` is the list
    /// of tab indices whose child exited this tick (caller closes them after, to
    /// avoid mutating `tabs` while iterating).
    fn drain_pty(&mut self) -> (bool, Vec<usize>) {
        let mut active_had_data = false;
        let mut exited: Vec<usize> = Vec::new();
        // Perf-HUD VT throughput: count bytes read this drain into a local
        // (avoids a self borrow inside the &mut self.tabs loop), folded into the
        // running total after the loop. Cheap; the rate is derived over ~1s
        // windows in the render path.
        let mut vt_read: u64 = 0;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let mut had = false;
            while let Ok(chunk) = tab.pty.output().try_recv() {
                vt_read += chunk.len() as u64;
                tab.terminal.feed(&chunk);
                had = true;
            }
            // Flush any query replies (DSR/DA, etc.) this tab produced back to its
            // own PTY so the shell's startup probes succeed.
            let replies = tab.terminal.drain_pty_writes();
            if !replies.is_empty() {
                let _ = tab.writer.write_all(&replies);
                let _ = tab.writer.flush();
                had = true;
            }
            if i == self.active && had {
                active_had_data = true;
            }
            if tab.terminal.child_exited() || tab.pty.child_exited() {
                exited.push(i);
            }
        }
        self.vt_bytes += vt_read;
        (active_had_data, exited)
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
    /// valid), fixing up `active`. If no tabs remain, exit the event loop.
    /// Returns true if the app should keep running.
    fn close_exited_tabs(&mut self, mut exited: Vec<usize>, event_loop: &ActiveEventLoop) -> bool {
        if exited.is_empty() {
            return true;
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
            event_loop.exit();
            return false;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        if self.renaming.is_none() {
            self.rename_buf.clear();
        }
        self.selecting = false;
        if let Some(w) = &self.window {
            w.request_redraw();
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
            tab.pty.resize(cols as u16, rows as u16);
        }
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
        self.persist();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Change the font family at runtime. Updates `font_family`, tells the
    /// TextLayer to remeasure, then reflows and requests a redraw.
    fn set_font_family(&mut self, name: String) {
        self.font_family = name;
        if let Some(text) = &mut self.text {
            text.set_font_family(&self.font_family);
        }
        // The chrome is now DECOUPLED from the terminal font: it follows the
        // separate `ui_font_family`/`ui_font_logical` (set via `set_ui_font_*`),
        // NOT the terminal family. So a terminal-font change no longer touches
        // chrome_text — leaving the chrome typeface stable while the grid font
        // changes (and avoiding a chrome re-measure on every terminal-font pick).
        self.reflow();
        self.persist();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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
        self.persist();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        // Live preview in the settings window (specimen + readout) if it's open.
        self.render_settings_window();
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
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
        self.persist();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.render_settings_window();
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
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
                if let Some(win) = &self.window {
                    win.focus_window();
                    win.request_redraw();
                }
            }
            return;
        }
        self.visible = want;
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
                            Some(pos) => {
                                let _ = win.set_outer_position(pos);
                                self.pending_center_pos = Some(pos);
                                self.pending_center_frames = 5;
                            }
                            None => center_window(win),
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
                win.request_redraw();
            } else {
                // Remember the current spot before hiding so the next Center
                // summon restores it. Dropdown re-docks, so last_pos is unused.
                if mode == WindowMode::Center {
                    self.last_pos = win.outer_position().ok();
                }
                self.slide_anim = None;
                win.set_visible(false);
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
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let window = jetty_platform::build_fixed_window(
            event_loop,
            "JeTTY — Settings",
            (SETTINGS_WIN_W, SETTINGS_WIN_H),
        );
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
            self.dropdown_height_pct,
            self.dropdown_width_pct,
            self.window_mode == WindowMode::Dropdown, self.focus_autohide,
            self.launch_at_login,
            self.ui_font_logical, &self.ui_font_families, &self.ui_font_family,
            self.ui_font_scroll_offset,
            0.0, 0.0, &theme, self.settings_char_w(),
            &self.shell_display(),
            self.settings_tab,
            &fx,
            self.effects_scroll,
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
            window_mode_name, tab_bar_name, dropdown_height_pct, dropdown_width_pct, is_dropdown, focus_autohide,
            launch_at_login,
            ui_font_logical, &ui_families, &ui_family, ui_font_scroll_offset,
            0.0, 0.0, &theme, char_w,
            &shell_display,
            settings_tab,
            &fx,
            effects_scroll,
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
                self.theme_idx = i;
                self.apply_theme();
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
            input::MouseAction::CycleShellPrev => {
                self.cycle_shell(false);
            }
            input::MouseAction::CycleShellNext => {
                self.cycle_shell(true);
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
                self.fx.caret_flash_ms = (60.0 + frac * 340.0).clamp(60.0, 400.0);
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
                if let Some(w) = &self.settings_window {
                    w.request_redraw();
                }
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if let Some(w) = &self.settings_window {
            w.request_redraw();
        }
    }

    /// Handle a `WindowEvent` that belongs to the settings window. Hit-testing
    /// uses the settings window's own coordinate space (`settings_cursor`).
    fn settings_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.close_settings_window();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.settings_gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.settings_gpu, &mut self.settings_text) {
                    text.resize(gpu);
                }
                if let Some(w) = &self.settings_window {
                    w.request_redraw();
                }
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
                if let Some(w) = &self.settings_window {
                    w.request_redraw();
                }
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
                                    self.fx.caret_flash_ms = (60.0 + frac * 340.0).clamp(60.0, 400.0);
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    if let Some(w) = &self.settings_window {
                        w.request_redraw();
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
                        w.request_redraw();
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
                    // max_scroll = EFFECTS_CONTENT_H − EFFECTS_VISIBLE_H = 652.0 − 460.0 = 192.0
                    let max_scroll = (jetty_render::EFFECTS_CONTENT_H
                        - jetty_render::EFFECTS_VISIBLE_H).max(0.0);
                    self.effects_scroll = (self.effects_scroll + delta_px).clamp(0.0, max_scroll);
                    if let Some(w) = &self.settings_window {
                        w.request_redraw();
                    }
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
                    if let Some(w) = &self.settings_window {
                        w.request_redraw();
                    }
                } else if over_ui_list {
                    const MAX_UI_FONT_ROWS: usize = 4;
                    let max_offset = self.ui_font_families.len().saturating_sub(MAX_UI_FONT_ROWS);
                    if lines > 0 {
                        self.ui_font_scroll_offset = self.ui_font_scroll_offset.saturating_sub(1);
                    } else {
                        self.ui_font_scroll_offset = (self.ui_font_scroll_offset + 1).min(max_offset);
                    }
                    if let Some(w) = &self.settings_window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                // Escape closes the settings window.
                if matches!(event.logical_key, winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)) {
                    self.close_settings_window();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Focused(true) => {
                // Record that OUR settings window now holds focus so the main
                // window's Focused(false) auto-hide doesn't fire when the user
                // merely clicked into Settings.
                if let Some(w) = &self.settings_window {
                    self.last_focused_window = Some(w.id());
                    self.switching_to_settings = true;
                    // macOS first-paint nudge: a request_redraw issued while the
                    // window was still being shown can be dropped, leaving it blank
                    // until the user clicks. Re-request now that it is shown+focused.
                    w.request_redraw();
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
            }
            _ => {}
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
        let main_pending = self.summon_anim.is_some()
            || self.slide_anim.is_some()
            || self.summon_pending
            || self.pending_dock_frames > 0
            || self.pending_center_frames > 0
            || self.fx.crt_anim_live()
            || self.caret_anim.is_some();
        if main_pending {
            if let Some(w) = &self.window {
                w.request_redraw();
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

        let control_flow = if main_pending || settings_pending {
            winit::event_loop::ControlFlow::Poll
        } else if let Some(d) = wake_at {
            // Wake exactly once at the soonest pending deadline instead of polling.
            winit::event_loop::ControlFlow::WaitUntil(d)
        } else {
            winit::event_loop::ControlFlow::Wait
        };
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
            PtySession::spawn(FALLBACK_COLS as u16, FALLBACK_ROWS as u16, shell, move || {
                let _ = proxy_wake.send_event(AppEvent::Wake);
            })
        });

        let window = jetty_platform::build_window(event_loop, "JeTTY", (1000, 640));
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
            // a Tier-B effect is summoning; Tier-A and normal frames never use it.
            self.liquid = Some(jetty_render::LiquidDrop::new(&g.device, g.format));
            self.focus = Some(jetty_render::FocusPull::new(&g.device, g.format));
            // CRT post-effect (passthrough for now). Same surface format as the
            // rest of the pipeline so the blit-to-surface target matches.
            self.crt = Some(jetty_render::Crt::new(&g.device, g.format));
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
        pty.resize(cols as u16, rows as u16);
        terminal.resize(cols, rows);
        let writer = pty.writer();
        self.tabs.push(Tab { terminal, pty, writer, title: "Tab 1".to_string() });
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
                loop {
                    match rx.recv() {
                        Ok(ev) => {
                            if ev.state == global_hotkey::HotKeyState::Pressed {
                                let _ = proxy_hotkey.send_event(AppEvent::ToggleVisibility);
                            }
                        }
                        Err(_) => break,
                    }
                }
                drop(manager);
            });
        }

        // Slow safety heartbeat — 100ms is enough for any future time-based UI
        // while virtually eliminating idle CPU waste. Real responsiveness now
        // comes from the on_data wake above, not from this tick.
        spawn_waker(self.proxy.clone());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: AppEvent) {
        match ev {
            AppEvent::Wake => {
                let (had_data, exited) = self.drain_pty();
                // A tab whose shell exited (Ctrl+D / `exit`) closes THAT tab,
                // Yakuake-style; if it was the last tab, close_exited_tabs exits
                // the loop. The waker fires ~10x/s, so we react within a frame.
                if !self.close_exited_tabs(exited, event_loop) {
                    return;
                }
                // Damage-driven: only request a redraw when the active tab's PTY
                // produced data (or query replies were sent). Background tabs still
                // drained above but don't trigger a repaint. When idle, the 100ms
                // heartbeat drains nothing and we skip the redraw entirely.
                if had_data {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            AppEvent::ToggleVisibility => {
                self.toggle_visibility(event_loop);
            }
            AppEvent::SetVisible(want) => {
                self.set_visibility(want, event_loop);
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
        match event {
            WindowEvent::CloseRequested => {
                self.confirm_quit = true;
                if let Some(win) = &self.window {
                    win.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.gpu, &mut self.text) {
                    text.resize(gpu);
                }
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
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
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
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::Focused(true) => {
                // The main terminal window gained focus.
                self.last_focused_window = Some(id);
                // macOS first-paint nudge (see the settings window above): ensure a
                // frame is drawn once the window is actually shown + focused.
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
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
                if self.focus_autohide
                    && self.visible
                    && self.summon_anim.is_none()
                    && !self.summon_pending
                    // Don't auto-hide within the post-summon settle window: a
                    // synthetic Focused(false) right after the window maps would
                    // otherwise dismiss a fast (None/Bayer) summon as it appears.
                    && self
                        .summon_settle_until
                        .map_or(true, |d| std::time::Instant::now() >= d)
                    && !to_settings
                {
                    if let Some(win) = &self.window {
                        if self.window_mode == WindowMode::Center {
                            self.last_pos = win.outer_position().ok();
                        }
                        self.slide_anim = None;
                        win.set_visible(false);
                    }
                    self.visible = false;
                    // If auto-hide fired mid-drag, the matching button-release
                    // never arrives — clear the terminal drag state so it doesn't
                    // resume stuck (a no-button selection/scrollbar drag) on the
                    // next summon.
                    self.selecting = false;
                    self.dragging_scrollbar = false;
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
                    || self.help_open
                    || self.context_menu.is_some();
                if !self.dragging_scrollbar && !self.selecting && !modal_open {
                    if let Some(gpu) = &self.gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        let zone = resize_zone_at(position.x as f32, position.y as f32, w, h);
                        if zone != self.resize_cursor {
                            self.resize_cursor = zone;
                            if let Some(win) = &self.window {
                                win.set_cursor(zone.cursor_icon());
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
                }
                // --- Text selection drag continuation ---
                // Gated on `selecting` alone: it is set only when a local selection
                // actually began (mouse reporting off, or Shift held to override it),
                // so a Shift-drag over a mouse-mode app still extends the selection.
                if self.selecting {
                    if let Some((line, col)) = self.cursor_cell_0() {
                        self.active_tab_mut().terminal.selection_update(line, col);
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
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
                                    if let Some(win) = &self.window {
                                        win.request_redraw();
                                    }
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
                    return;
                }

                let cx = self.cursor.0 as f32;
                let cy = self.cursor.1 as f32;

                // --- Help overlay is modal: a click outside its panel closes it;
                // a click inside is swallowed. Either way the click is consumed so
                // it never reaches the tab bar, a resize edge, or the terminal. ---
                if self.help_open {
                    let theme = self.current_theme();
                    let help = jetty_render::build_help_overlay(w, h, &theme, self.chrome_char_w());
                    if !input::point_in(&help.panel, cx, cy) {
                        self.help_open = false;
                    }
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
                    return;
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
                    // Reserve the SAME perf-HUD width the last render used, so the
                    // tab/close hit-rects align with what's drawn (the HUD only
                    // shrinks the tab area; window controls are unaffected).
                    let perf_ref = self.perf_label.as_deref();
                    let mut bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None, perf_ref, self.chrome_char_w(),
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
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
                            if let Some(win) = &self.window {
                                win.request_redraw();
                            }
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
                        self.select_tab(i);
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
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
                    | input::MouseAction::StartDropdownDrag
                    | input::MouseAction::StartDropdownWidthDrag
                    | input::MouseAction::ToggleFocusAutoHide
                    | input::MouseAction::ToggleLaunchAtLogin
                    | input::MouseAction::CycleShellPrev
                    | input::MouseAction::CycleShellNext
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
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                    input::MouseAction::None => {
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
                            if let Some((line, col)) = self.cursor_cell_0() {
                                self.active_tab_mut().terminal.selection_start(line, col);
                            }
                            self.selecting = true;
                            if let Some(win) = &self.window {
                                win.request_redraw();
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Right-click: open the context menu (Copy / Paste / Select All).
                // Settings now live in a separate window, so the main terminal is
                // always free to show its context menu.
                let cx = self.cursor.0 as f32;
                let cy = self.cursor.1 as f32;
                // A right-click on the tab bar must NOT open the terminal Copy/
                // Paste menu (the strip has its own affordances).
                let bar_y = if let Some(gpu) = &self.gpu {
                    self.tabbar_y(gpu.config.height as f32)
                } else {
                    0.0
                };
                if cy >= bar_y && cy < bar_y + TABBAR_H {
                    return;
                }
                // Commit any in-progress rename and close the help overlay so the
                // menu can't be orphaned under it.
                self.commit_rename();
                self.help_open = false;
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
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
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
                    if let Some(win) = &self.window {
                        win.request_redraw();
                    }
                }

                // Forward a release report only when the app enabled mouse mode
                // and this release did not terminate a host-widget drag. We do
                // not re-check widget hit-testing here: the matching press was
                // only forwarded when it landed in the terminal area (action ==
                // None), so a non-drag release pairs with that forwarded press.
                if !was_dragging && !was_selecting && self.active_tab().terminal.mouse_mode() {
                    self.send_mouse_report(input::MouseEvent::LeftRelease);
                }

                // If this release ended a no-Shift DRAG over a mouse-reporting app
                // (press recorded, cursor moved > a few px), the user was likely
                // trying to select — they just don't know Shift is needed. Surface
                // a brief, throttled toast telling them how.
                if let Some((px, py)) = self.mouse_grab_press.take() {
                    let moved = ((self.cursor.0 - px).powi(2) + (self.cursor.1 - py).powi(2)).sqrt();
                    let now = std::time::Instant::now();
                    let off_cooldown = self.shift_hint_cooldown.map_or(true, |t| now >= t);
                    if moved > 8.0 && off_cooldown {
                        self.shift_hint_until = Some(now + std::time::Duration::from_millis(3500));
                        self.shift_hint_cooldown = Some(now + std::time::Duration::from_secs(25));
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Middle, .. } => {
                // Middle-click: paste from clipboard (same as right-click).
                if let Some(text) = clipboard::get() {
                    self.paste_text(&text);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive y = wheel up = scroll into history (older output).
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y.round() as i32) * 3,
                    MouseScrollDelta::PixelDelta(p) => {
                        // Approximate cell height; use 20.0 as a reasonable default.
                        const CELL_H: f64 = 20.0;
                        (p.y / CELL_H).round() as i32
                    }
                };
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
                    } else {
                        self.active_tab_mut().terminal.scroll_lines(lines);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
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
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
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
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
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
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.rename_buf.pop();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        _ => {
                            // Append any printable text the key produced.
                            if let Some(t) = &event.text {
                                for ch in t.chars() {
                                    if !ch.is_control() {
                                        self.rename_buf.push(ch);
                                    }
                                }
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                    // Defensive: keep `i` referenced so the renaming index is valid.
                    let _ = i;
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let alt = self.modifiers.alt_key();
                let app_cursor = self.active_tab().terminal.app_cursor_keys();
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
                            && t.chars().any(|c| !c.is_ascii())
                        {
                            Some(t.as_bytes().to_vec())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                let action = match composed {
                    Some(bytes) => input::KeyAction::Send(bytes),
                    None => input::decide_key(ctrl, shift, alt, event.physical_key.clone(), &event.logical_key, false, app_cursor),
                };
                if self.debug {
                    let action_name = match &action {
                        input::KeyAction::TogglePanel => "TogglePanel",
                        input::KeyAction::ClosePanel => "ClosePanel",
                        input::KeyAction::NewTab => "NewTab",
                        input::KeyAction::CloseTab => "CloseTab",
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
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                    input::KeyAction::NewTab => {
                        self.new_tab();
                    }
                    input::KeyAction::CloseTab => {
                        // Ask before closing instead of closing immediately.
                        self.confirm_close = Some(self.active);
                        if let Some(w) = &self.window {
                            w.request_redraw();
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
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::OpacityDown => {
                        self.opacity = (self.opacity - 0.05).max(0.1);
                        self.apply_theme();
                        self.persist();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::ScrollPageUp => {
                        self.active_tab_mut().terminal.scroll_page(true);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::ScrollPageDown => {
                        self.active_tab_mut().terminal.scroll_page(false);
                        if let Some(w) = &self.window {
                            w.request_redraw();
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
                            if let Some(win) = &self.window {
                                win.request_redraw();
                            }
                        }
                    }
                    input::KeyAction::Paste => {
                        // Paste from the clipboard into the PTY.
                        if let Some(text) = clipboard::get() {
                            self.paste_text(&text);
                        }
                    }
                    input::KeyAction::Send(bytes) => {
                        // Escape closes the context menu (if open) before forwarding to PTY.
                        if bytes == [0x1b] && self.context_menu.is_some() {
                            self.context_menu = None;
                            self.menu_hover = None;
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
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
                        // Trigger caret flash+pulse on printable keystrokes.
                        if self.fx.caret_flash_enabled && is_printable_keystroke(&bytes) {
                            self.caret_anim = Some(std::time::Instant::now());
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                    input::KeyAction::None => {}
                }
            }
            WindowEvent::RedrawRequested => {
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
                        let _ = win.set_outer_position(pos);
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
                let (_had, exited) = self.drain_pty();
                if !self.close_exited_tabs(exited, event_loop) {
                    return;
                }
                if self.tabs.is_empty() {
                    return;
                }
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
                let help_open = self.help_open;
                let welcome_open = self.welcome_open;
                let shift_hint_show = self
                    .shift_hint_until
                    .map_or(false, |t| std::time::Instant::now() < t);
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
                            .map_or(true, |(t, _)| t.width() != gw || t.height() != gh);
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
                let (Some(gpu), Some(text), Some(chrome_text), Some(quad)) =
                    (&mut self.gpu, &mut self.text, &mut self.chrome_text, &mut self.quad)
                else {
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
                    // Pass 1: clear to the theme bg and paint per-cell background
                    // quads (reverse-video / colored backgrounds) UNDER the text.
                    // The grid's bg quads are offset down by the tab bar.
                    let (cell_w, cell_h) = text.cell_size();
                    let selection_bg = selection_bg_for(&theme);
                    let scrollbar_thumb = scrollbar_thumb_for(&theme);
                    let bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h, grid_top + slide_y_offset, selection_bg);
                    quad.render_clear(
                        &gpu.device,
                        &gpu.queue,
                        scene_view,
                        width,
                        height,
                        &bg_rects,
                        // premultiply the clear to match the surface's alpha_mode
                        // (PreMultiplied on Vulkan, straight on Metal/macOS) so the
                        // window is correctly see-through on every backend.
                        jetty_render::default_bg_clear(&snap, gpu.premultiply_clear),
                    );
                    // Pass 2: draw glyphs on top of the painted background (load),
                    // offset down by the tab bar height.
                    let _ = text.render_to(
                        &gpu.device,
                        &gpu.queue,
                        scene_view,
                        width,
                        height,
                        &snap,
                        false,
                        grid_top + slide_y_offset,
                        caret_t,
                        caret_flash_color,
                    );
                    // Pass 3: the tab bar (translated to its actual y) over the grid.
                    quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &bar.quads);
                    if !bar.labels.is_empty() {
                        // Chrome: fixed-size layer so the bar text never scales with
                        // the terminal font (and never overflows the 36px bar).
                        let _ = chrome_text.render_overlays(
                            &gpu.device, &gpu.queue, scene_view, width, height, &bar.labels,
                        );
                    }
                    if !bar.title_labels.is_empty() {
                        // Tab TITLES in the platform's proportional sans-serif for an
                        // elegant UI look — the close ×, "+", overflow hint, perf HUD
                        // and window controls above stay in the monospace chrome font.
                        let _ = chrome_text.render_overlays_sans(
                            &gpu.device, &gpu.queue, scene_view, width, height, &bar.title_labels,
                        );
                    }
                    // Pass 4: scrollbar (spans the grid area below the bar).
                    let mut rects: Vec<jetty_render::Rect> = Vec::new();
                    if let Some(mut r) =
                        jetty_render::scrollbar_rect(&snap, width, height, grid_top, status_h, scrollbar_thumb)
                    {
                        r.y += slide_y_offset;
                        rects.push(r);
                    }
                    quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &rects);

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
                        let pill_y =
                            (height as f32 - status_h - 14.0 - pill_h).max(0.0) + slide_y_offset;
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
                        // that straddles the edge.
                        let grid_bottom = if tab_bar_bottom {
                            (height as f32 - TABBAR_H).max(0.0)
                        } else {
                            height as f32
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
                    // Draw the Help overlay (Keyboard Shortcuts) on top of all
                    // else — a dim layer, a bordered panel, and the binding rows.
                    if help_open {
                        let help = jetty_render::build_help_overlay(width, height, &theme, chrome_char_w);
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
                    // Final pass: round the window corners by zeroing alpha
                    // outside a rounded rect. No-op when radius == 0 (square).
                    // Applied to `scene_view`: for Tier-A this is the surface; for a
                    // Tier-B summon it's the offscreen frame, so the rounded corners
                    // are baked in before the effect samples it.
                    //
                    // When CRT is active (crt_active) the CRT pass will own the
                    // corners (a later task), so SKIP the mask here — for now this
                    // means corners are not rounded while CRT is on (accepted: the
                    // CRT pass is a passthrough this task). During a Tier-B summon
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
                    let hint_live = self
                        .shift_hint_until
                        .map_or(false, |t| std::time::Instant::now() < t);
                    // CRT animation self-drive: keep painting ONLY while CRT is on
                    // AND at least one of roll/flicker/jitter is toggled on. Static
                    // CRT (enabled, all three off) does NOT match here, so it stays
                    // damage-driven (0-CPU idle preserved). Same `crt_anim_live()`
                    // term feeds `about_to_wait`'s `main_pending`, so on macOS the
                    // loop sits in `Poll` (vsync-throttled by Fifo present) while
                    // animating and returns to `Wait`/idle the moment it clears.
                    let crt_anim_live = self.fx.crt_anim_live();
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
                    // Caret glow/ripple pass (Task 12). Additive GPU burst at the
                    // cursor position on each keystroke. Dispatched only when the
                    // toggle is on AND an animation is live AND the cursor is visible.
                    //
                    // Target is always `scene_view`, which routes correctly for all
                    // three compositing cases:
                    //   CRT ON:   scene_view == offscreen → glow composites into the
                    //             offscreen; the CRT pass below samples and re-rounds
                    //             the corners. Glow gets full CRT treatment.
                    //   CRT OFF:  scene_view == &view (surface) → glow goes directly
                    //             onto the surface after the corner mask above.
                    //             Alpha=0 output keeps corner transparency intact.
                    //   Tier-B:   scene_view == offscreen (Tier-B owns it) → glow is
                    //             baked into the offscreen; the Tier-B effect displaces/
                    //             blurs it along with the rest of the scene.
                    //
                    // No new redraw scheduling — the caret_anim guard in the self-drive
                    // block above already keeps frames coming while the burst is live.
                    if self.fx.caret_glow_enabled {
                        if let (Some(cfx), Some(t_val)) = (caret_fx, caret_t) {
                            if snap.cursor_visible
                                && snap.cursor_col < snap.cols
                                && snap.cursor_row < snap.rows
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
                    // CRT post-pass: when CRT is active (enabled AND not bypassed by
                    // an active Tier-B summon — Tier-B owns the offscreen this frame)
                    // blit the fully-composited offscreen scene through the CRT pass
                    // to the surface `view`. Passthrough (identity) for now. `crt`
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
                                    // so feed it the same physical radius the mask
                                    // would use. NOTE: a single radius rounds all
                                    // four corners; in Dropdown (top-flush) mode the
                                    // mask keeps the top corners square — that nuance
                                    // is not modeled here (single corner_radius).
                                    corner_radius: corner_radius_px,
                                    // Animation (Task 10): free-running phase +
                                    // roll/flicker/jitter bitfield. When all three
                                    // toggles are off, flags == 0 and the shader
                                    // output is identical to the static look (each
                                    // animated term collapses to its static value),
                                    // so static CRT is byte-identical here.
                                    time: self.crt_clock.elapsed().as_secs_f32(),
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
                                    _pad0: 0.0,
                                },
                            );
                        }
                    }
                    frame.present();
                }
                // Restore the tab-metadata cache taken above so it persists across
                // frames (its signature still matches, so it won't rebuild).
                self.cached_tabs_meta = tabs_meta;
            }
            _ => {}
        }
    }
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
        let exec = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "jetty".to_string());
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
