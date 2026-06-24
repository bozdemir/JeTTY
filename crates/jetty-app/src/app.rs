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
    /// F9 global hotkey was pressed — toggle window visibility.
    ToggleVisibility,
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

/// Fallback grid dimensions used only when computing cols/rows from the window
/// is not yet possible (e.g. before `resumed` completes). In practice the
/// derived grid replaces these immediately; they are never used for the actual
/// Terminal or PTY once a window exists.
const FALLBACK_COLS: usize = 80;
const FALLBACK_ROWS: usize = 24;

/// Height of the tab bar (re-exported from the renderer so app.rs has one name).
const TABBAR_H: f32 = jetty_render::TABBAR_H;

/// A single terminal session: its grid model, PTY, writer, and tab title. One
/// `Tab` per visible tab. Per-tab scroll/selection live inside `terminal`.
struct Tab {
    terminal: Terminal,
    pty: PtySession,
    writer: Box<dyn Write + Send>,
    title: String,
}

/// Logical size of the separate Settings window. Sized to exactly fit the panel
/// (`build_panel` uses PANEL_W=380 × PANEL_H=504 plus a 2px border on each side)
/// so the panel fills the window with no margin and the OS frame handles moving.
const SETTINGS_WIN_W: u32 = 384;
const SETTINGS_WIN_H: u32 = 652;

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
    /// Dropdown height as a fraction of the monitor height (clamped 0.25..=1.0).
    dropdown_height_pct: f32,
    /// Dropdown width as a fraction of the monitor width (clamped 0.2..=1.0).
    /// Reserved; MVP ships full-width (1.0) and has no UI slider yet.
    dropdown_width_pct: f32,
    /// Start instant of the active Dropdown SLIDE animation, or None when idle.
    /// The slide is a render-side content translate; while Some the redraw loop
    /// self-drives frames (idle 0 CPU once cleared).
    slide_anim: Option<std::time::Instant>,
    /// Hide the window on focus loss (Yakuake auto-hide). Default ON.
    focus_autohide: bool,
    /// The id of the most recently focused window (main or settings). Used to
    /// suppress auto-hide when focus moved to our own Settings window.
    last_focused_window: Option<WindowId>,
    /// Whether the user is dragging the Dropdown-height slider in Settings.
    dragging_dropdown: bool,
    /// One-time guard for the Wayland "positioning is a no-op" diagnostic.
    wayland_warned: bool,
    /// Start instant of the active summon (crystallize) animation, or None when
    /// idle. While Some, the redraw loop self-drives frames; None = idle 0 CPU.
    summon_anim: Option<std::time::Instant>,
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
    /// Current font family name (runtime-settable via the font picker).
    font_family: String,
    /// Cached sorted monospace family list (populated once TextLayer is built).
    font_families: Vec<String>,
    /// Scroll offset into `font_families` for the panel's font-family list.
    font_scroll_offset: usize,
    /// Track held modifier keys so Ctrl+Shift combos can be detected.
    modifiers: winit::keyboard::ModifiersState,
    /// Last known cursor position in physical pixels.
    cursor: (f64, f64),
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
    /// Last known cursor position inside the settings window (physical px), used
    /// for hit-testing the panel in the settings window's own coordinate space.
    settings_cursor: (f64, f64),
    /// Whether the user is currently dragging the opacity slider in the Settings panel.
    dragging_slider: bool,
    /// Whether the user is currently dragging the corner-radius slider.
    dragging_radius: bool,
    /// Whether the user is currently dragging a text selection with the mouse.
    selecting: bool,
    /// Whether JETTY_DEBUG is set — enables input/panel state logging to stderr.
    debug: bool,
    /// When Some, the right-click context menu is open at this physical-pixel position.
    context_menu: Option<(f32, f32)>,
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
            quad: None,
            corner_mask: None,
            bayer_reveal: None,
            phosphor: None,
            liquid: None,
            focus: None,
            offscreen: None,
            summon_effect: SummonEffect::Bayer,
            window_mode: WindowMode::Center,
            dropdown_height_pct: 0.50,
            dropdown_width_pct: 1.0,
            slide_anim: None,
            focus_autohide: true,
            last_focused_window: None,
            dragging_dropdown: false,
            wayland_warned: false,
            summon_anim: None,
            corner_radius,
            tabs: Vec::new(),
            active: 0,
            theme_idx,
            opacity,
            font_logical: FONT_LOGICAL_DEFAULT,
            font_family,
            font_families: Vec::new(),
            font_scroll_offset: 0,
            modifiers: winit::keyboard::ModifiersState::empty(),
            cursor: (0.0, 0.0),
            dragging_scrollbar: false,
            drag_grab_dy: 0.0,
            settings_window: None,
            settings_gpu: None,
            settings_text: None,
            settings_quad: None,
            settings_cursor: (0.0, 0.0),
            dragging_slider: false,
            dragging_radius: false,
            selecting: false,
            debug,
            context_menu: None,
            menu_hover: None,
            renaming: None,
            rename_buf: String::new(),
            last_strip_click: None,
            resize_cursor: ResizeZone::None,
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
        app.corner_radius = cfg.corner_radius.clamp(0.0, 24.0);
        app.summon_effect = SummonEffect::from_config(&cfg.summon_effect);
        app.window_mode = WindowMode::from_config(&cfg.window_mode);
        app.dropdown_height_pct = cfg.dropdown_height_pct.clamp(0.25, 1.0);
        app.dropdown_width_pct = cfg.dropdown_width_pct.clamp(0.2, 1.0);
        app.focus_autohide = cfg.focus_autohide;

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
            corner_radius: self.corner_radius,
            summon_effect: self.summon_effect.to_config().to_string(),
            window_mode: self.window_mode.to_config().to_string(),
            dropdown_height_pct: self.dropdown_height_pct,
            dropdown_width_pct: self.dropdown_width_pct,
            focus_autohide: self.focus_autohide,
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
        self.summon_anim = Some(std::time::Instant::now());
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
        let (Some(gpu), Some(text)) = (&self.gpu, &self.text) else {
            return (FALLBACK_COLS, FALLBACK_ROWS);
        };
        let (cw, ch) = text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return (FALLBACK_COLS, FALLBACK_ROWS);
        }
        let cols = (gpu.config.width as f32 / cw).floor().max(1.0) as usize;
        let rows = ((gpu.config.height as f32 - TABBAR_H) / ch).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Spawn a new tab sized to the current grid, themed like the others, make it
    /// active, and redraw. The new PTY shares the same wake proxy so one
    /// `AppEvent::Wake` drains every tab.
    fn new_tab(&mut self) {
        let (cols, rows) = self.grid_dims();
        let proxy_wake = self.proxy.clone();
        let pty = match PtySession::spawn(cols as u16, rows as u16, move || {
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
        // The scrollbar track lives below the tab bar.
        let track_h = (h as f32 - TABBAR_H).max(0.0);
        // Recompute thumb height the same way as the geometry function.
        let rows = self.active_tab().terminal.rows();
        let total = rows + max;
        let thumb_h = (track_h * rows as f32 / total as f32).max(24.0);

        let travel = track_h - thumb_h;
        if travel <= 0.0 {
            return;
        }

        // Cursor y is absolute; subtract the bar offset so 0 == track top.
        let thumb_top = ((self.cursor.1 as f32 - TABBAR_H) - self.drag_grab_dy).clamp(0.0, travel);
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
            }
            WindowMode::Dropdown => {
                // Recompute dock geometry on the next summon (ignore stale pos).
                self.last_pos = None;
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

    /// Convert the current cursor pixel position into 1-based terminal cell
    /// coordinates `(col, row)` using the renderer's cell size. Returns `None`
    /// when the renderer (and thus cell metrics) is not yet available.
    fn cursor_cell(&self) -> Option<(usize, usize)> {
        let (cell_w, cell_h) = self.text.as_ref()?.cell_size();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let col = (self.cursor.0 as f32 / cell_w).floor() as i64 + 1;
        // The grid starts below the tab bar; subtract TABBAR_H before dividing.
        let y = (self.cursor.1 as f32 - TABBAR_H).max(0.0);
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
        // The grid is offset down by the tab bar; subtract it (clamped ≥0).
        let y = (self.cursor.1 as f32 - TABBAR_H).max(0.0);
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
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let mut had = false;
            while let Ok(chunk) = tab.pty.output().try_recv() {
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
        (active_had_data, exited)
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
        let (Some(gpu), Some(text)) = (&self.gpu, &self.text) else { return };
        let (cw, ch) = text.cell_size();
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let w = gpu.config.width;
        let h = gpu.config.height;
        let cols = (w as f32 / cw).floor().max(1.0) as usize;
        // The grid occupies the area below the tab bar.
        let rows = ((h as f32 - TABBAR_H) / ch).floor().max(1.0) as usize;
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
        if let Some(ref g) = self.gpu {
            let new_text = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, clamped * scale, &self.font_family,
            );
            self.text = Some(new_text);
            // Re-populate family list from the new TextLayer.
            if let Some(ref t) = self.text {
                self.font_families = t.monospace_families();
            }
        }
        self.reflow();
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
        self.reflow();
        self.persist();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Toggle window visibility (F9 / Yakuake-style summon).
    ///
    /// When summoning (making visible), the window is re-centred on its
    /// current monitor, focused, and redrawn. The PTY keeps running while the
    /// window is hidden — nothing is killed or suspended.
    fn toggle_visibility(&mut self, _event_loop: &ActiveEventLoop) {
        self.visible = !self.visible;
        let mode = self.window_mode;
        if let Some(win) = &self.window {
            if self.visible {
                match mode {
                    WindowMode::Center => {
                        win.set_visible(true);
                        // Re-summon at the spot the user left it; first → center.
                        match self.last_pos {
                            Some(pos) => {
                                let _ = win.set_outer_position(pos);
                            }
                            None => center_window(win),
                        }
                    }
                    WindowMode::Dropdown => {
                        // Dock geometry is recomputed fresh each summon (ignores
                        // last_pos). Position+size FIRST, then show, so the
                        // resize happens before the window is visible.
                        dock_window_top(win, self.dropdown_width_pct, self.dropdown_height_pct);
                        win.set_visible(true);
                        // Arm the render-side slide-down.
                        self.slide_anim = Some(std::time::Instant::now());
                    }
                }
                win.focus_window();
                // Crystallize/reveal on every summon (F9 show), mirroring first open.
                self.summon_anim = Some(std::time::Instant::now());
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
            let text = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, self.font_logical * scale, &self.font_family,
            );
            let quad = QuadLayer::new(&g.device, g.format);
            self.settings_text = Some(text);
            self.settings_quad = Some(quad);
        }
        self.settings_gpu = gpu;
        window.focus_window();
        window.request_redraw();
        self.settings_window = Some(window);
        if self.debug {
            eprintln!("SETTINGS window opened");
        }
    }

    /// Drop the settings window and its render stack (closes/hides the OS window).
    fn close_settings_window(&mut self) {
        self.settings_window = None;
        self.settings_gpu = None;
        self.settings_text = None;
        self.settings_quad = None;
        // Clear BOTH drag flags so a drag in progress when the window closes
        // doesn't leave a stale flag set that misbehaves on reopen.
        self.dragging_slider = false;
        self.dragging_radius = false;
        self.dragging_dropdown = false;
        if self.debug {
            eprintln!("SETTINGS window closed");
        }
    }

    /// Build the panel view for the settings window in its own coordinate space
    /// (the panel is centred to fill the fixed-size window; no drag offset).
    fn settings_panel_view(&self, w: u32, h: u32) -> jetty_render::PanelView {
        let theme = self.current_theme();
        jetty_render::build_panel(
            w, h, self.opacity, self.theme_idx, self.font_logical,
            &self.font_families, &self.font_family, self.font_scroll_offset,
            self.corner_radius, self.summon_effect.display_name(),
            self.window_mode.display_name(), self.dropdown_height_pct,
            self.window_mode == WindowMode::Dropdown, self.focus_autohide,
            0.0, 0.0, &theme,
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
        let is_dropdown = self.window_mode == WindowMode::Dropdown;
        let focus_autohide = self.focus_autohide;
        // Clone the small inputs build_panel needs so we can borrow the render
        // stack mutably below without overlapping the immutable self borrows.
        let families = self.font_families.clone();
        let family = self.font_family.clone();
        let theme = self.current_theme();
        let (Some(gpu), Some(text), Some(quad)) =
            (&mut self.settings_gpu, &mut self.settings_text, &mut self.settings_quad)
        else {
            return;
        };
        let width = gpu.config.width;
        let height = gpu.config.height;
        let pv = jetty_render::build_panel(
            width, height, opacity, theme_idx, font_logical,
            &families, &family, font_scroll_offset, corner_radius, summon_name,
            window_mode_name, dropdown_height_pct, is_dropdown, focus_autohide,
            0.0, 0.0, &theme,
        );
        if let Some((frame, view)) = gpu.acquire_frame() {
            // Clear the window background to the panel border color, then paint the
            // panel quads (the panel fills the window, so margins are ~none).
            quad.render_clear(
                &gpu.device,
                &gpu.queue,
                &view,
                width,
                height,
                &pv.quads,
                wgpu::Color { r: 0.02, g: 0.02, b: 0.03, a: 1.0 },
            );
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
            frame.present();
        }
    }

    /// Apply a panel `MouseAction` decoded in the settings window. Updates shared
    /// state AND the live main terminal (theme/font/opacity), then requests a
    /// redraw of BOTH windows so each reflects the change immediately.
    fn handle_settings_action(
        &mut self,
        action: input::MouseAction,
        track: Option<jetty_render::Rect>,
        radius_track: Option<jetty_render::Rect>,
        dropdown_track: Option<jetty_render::Rect>,
    ) {
        let cx = self.settings_cursor.0 as f32;
        match action {
            input::MouseAction::StartSliderDrag => {
                self.dragging_slider = true;
                if let Some(track_rect) = track {
                    self.opacity = self.opacity_from_cursor(cx, &track_rect);
                    self.apply_theme();
                }
            }
            input::MouseAction::StartRadiusDrag => {
                self.dragging_radius = true;
                if let Some(track_rect) = radius_track {
                    self.corner_radius = self.radius_from_cursor(cx, &track_rect);
                }
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
            input::MouseAction::StartDropdownDrag => {
                // No-op in Center mode (the slider is grayed/disabled there).
                if self.window_mode == WindowMode::Dropdown {
                    self.dragging_dropdown = true;
                    if let Some(track_rect) = dropdown_track {
                        self.dropdown_height_pct =
                            self.dropdown_pct_from_cursor(cx, &track_rect);
                    }
                }
            }
            input::MouseAction::ToggleFocusAutoHide => {
                self.focus_autohide = !self.focus_autohide;
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
                if let Some(ref g) = self.settings_gpu {
                    let new_text = TextLayer::new_with_family(
                        &g.device, &g.queue, g.format, self.font_logical * scale, &self.font_family,
                    );
                    self.settings_text = Some(new_text);
                }
                if let Some(w) = &self.settings_window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.settings_cursor = (position.x, position.y);
                // Continue an opacity-, radius-, or dropdown-height drag started here.
                if self.dragging_slider || self.dragging_radius || self.dragging_dropdown {
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
                // Persist the final opacity/corner-radius after a drag settles
                // (the drag itself only updates live state to keep writes cheap).
                if self.dragging_slider || self.dragging_radius || self.dragging_dropdown {
                    self.persist();
                }
                self.dragging_slider = false;
                self.dragging_radius = false;
                self.dragging_dropdown = false;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let Some(gpu) = &self.settings_gpu else { return };
                let (w, h) = (gpu.config.width, gpu.config.height);
                let pv = self.settings_panel_view(w, h);
                let track = pv.geom.slider_track;
                let radius_track = pv.geom.radius_track;
                let dropdown_track = pv.geom.dropdown_track;
                let cx = self.settings_cursor.0 as f32;
                let cy = self.settings_cursor.1 as f32;
                // Hit-test the panel only (no scrollbar in the settings window).
                let action = input::decide_mouse_press(Some(&pv.geom), None, cx, cy);
                self.handle_settings_action(action, Some(track), Some(radius_track), Some(dropdown_track));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wheel over the font-family list scrolls it (same as the old
                // in-app panel behaviour), now in the settings window.
                if self.font_families.is_empty() {
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
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_settings_window();
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
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
        let pty_handle = std::thread::spawn(move || {
            PtySession::spawn(FALLBACK_COLS as u16, FALLBACK_ROWS as u16, move || {
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
            let cols = (size.width as f32 / cw).floor().max(1.0) as usize;
            let rows = (size.height as f32 / ch).floor().max(1.0) as usize;
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
            self.summon_anim = Some(std::time::Instant::now());
        }

        self.window = Some(window);
        self.gpu = gpu;
        self.text = text;
        self.quad = quad;
        // Allocate the Tier-B offscreen scene texture now that the GPU exists.
        if let Some(g) = &self.gpu {
            self.offscreen = Some(Self::make_offscreen(g));
        }

        // Build the first tab with the derived grid size so the PTY and terminal
        // agree with the actual window layout. The on_data callback wakes the
        // winit event loop the instant bytes arrive (within ~1ms) — critical for
        // p10k's cursor-position / capability queries which have tight timeouts.
        let mut terminal = Terminal::new(cols, rows);
        terminal.set_theme(self.current_theme());
        // Join the PTY worker (forked in parallel with the GPU block) and resize
        // it from the provisional grid to the real cols/rows now that the cell
        // size is known.
        let pty = pty_handle.join().expect("pty worker panicked").expect("pty");
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
            std::thread::spawn(move || {
                let manager = match global_hotkey::GlobalHotKeyManager::new() {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("global hotkey F9 unavailable (Wayland? already grabbed?) — {e}");
                        return;
                    }
                };
                let hotkey = global_hotkey::hotkey::HotKey::new(
                    None,
                    global_hotkey::hotkey::Code::F9,
                );
                if let Err(e) = manager.register(hotkey) {
                    eprintln!("global hotkey F9 unavailable (Wayland? already grabbed?) — {e}");
                    return;
                }
                // Forward F9-pressed events to the winit loop. Keeps `manager`
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
                // Re-create the Tier-B offscreen scene texture at the new size so
                // a later Tier-B summon samples a correctly-sized frame.
                if let Some(gpu) = &self.gpu {
                    self.offscreen = Some(Self::make_offscreen(gpu));
                }
                // Recompute grid to match the new surface size and notify PTY.
                self.reflow();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Fired when the window is moved between monitors with different DPI.
                // Rebuild TextLayer with the new physical font size (logical * new
                // scale), then reflow() recomputes the grid from the current GPU
                // surface size and the new cell metrics. A subsequent Resized event
                // handles the surface-size change, so we only touch the font here.
                let scale = scale_factor as f32;
                if let Some(ref g) = self.gpu {
                    let new_text = TextLayer::new_with_family(
                        &g.device, &g.queue, g.format, self.font_logical * scale, &self.font_family,
                    );
                    self.text = Some(new_text);
                }
                self.reflow();
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
            }
            WindowEvent::Focused(false) => {
                // Yakuake-style auto-hide: hide when the window loses focus, but
                // only when ENABLED, currently visible, NOT mid-summon (X11 fires
                // a synthetic Focused(false) during set_visible/focus), and focus
                // did NOT move to our own Settings window.
                let settings_id = self.settings_window.as_ref().map(|w| w.id());
                let to_settings = self.last_focused_window.is_some()
                    && self.last_focused_window == settings_id;
                if self.focus_autohide
                    && self.visible
                    && self.summon_anim.is_none()
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
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let prev = self.cursor;
                self.cursor = (position.x, position.y);
                // --- Resize-edge cursor feedback (borderless window) ---
                // Only update the cursor when the zone changes, and never while a
                // host drag (scrollbar / selection) is in progress.
                if !self.dragging_scrollbar && !self.selecting {
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
                }
                // Repaint when the window-control hover state changes so the
                // min/max/close highlight tracks the cursor.
                if let Some(gpu) = &self.gpu {
                    let w = gpu.config.width;
                    let before = ctrl_hover_at(prev.0 as f32, prev.1 as f32, w);
                    let after = ctrl_hover_at(position.x as f32, position.y as f32, w);
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
                if self.selecting && !self.active_tab().terminal.mouse_mode() {
                    if let Some((line, col)) = self.cursor_cell_0() {
                        self.active_tab_mut().terminal.selection_update(line, col);
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
                // --- Context menu hover update ---
                if let Some((mx, my)) = self.context_menu {
                    if let Some(gpu) = &self.gpu {
                        let (win_w, win_h) = (gpu.config.width, gpu.config.height);
                        let theme = self.current_theme();
                        let menu = jetty_render::build_context_menu(mx, my, win_w, win_h, None, &theme);
                        let cx = self.cursor.0 as f32;
                        let cy = self.cursor.1 as f32;
                        let new_hover = menu.item_rects.iter().position(|r| {
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
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (w, h) = if let Some(gpu) = &self.gpu {
                    (gpu.config.width, gpu.config.height)
                } else {
                    return;
                };

                // --- Quit confirmation popup is modal (highest priority) ---
                if self.confirm_quit {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let theme = self.current_theme();
                    let popup =
                        jetty_render::build_confirm(w, h, "Quit JeTTY? — all tabs will close", &theme);
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
                    let popup = jetty_render::build_confirm_close(w, h, &title, &theme);
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
                if let Some((mx, my)) = self.context_menu.take() {
                    self.menu_hover = None;
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let theme = self.current_theme();
                    let menu = jetty_render::build_context_menu(mx, my, w, h, None, &theme);
                    let hit = menu.item_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if let Some(idx) = hit {
                        match idx {
                            0 => {
                                // Copy
                                if let Some(text) = self.active_tab().terminal.selection_text() {
                                    if !text.is_empty() {
                                        clipboard::set(&text);
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
                    let help = jetty_render::build_help_overlay(w, h, &theme);
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
                if cy < TABBAR_H {
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
                    let bar = jetty_render::build_tab_bar_ex(
                        w, &tabs_meta, &theme, rename_ref, jetty_render::CtrlHover::None,
                    );

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

                let rows = self.active_tab().terminal.rows();
                let scroll_offset = self.active_tab().terminal.scroll_offset();
                let scroll_max = self.active_tab().terminal.scroll_max();
                // Color is irrelevant for hit-test geometry; pass transparent.
                let scrollbar = jetty_render::scrollbar_rect_geom(rows, scroll_offset, scroll_max, w, h, TABBAR_H, [0, 0, 0, 0]);

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
                    | input::MouseAction::SummonPrev
                    | input::MouseAction::SummonNext
                    | input::MouseAction::WinModePrev
                    | input::MouseAction::WinModeNext
                    | input::MouseAction::StartDropdownDrag
                    | input::MouseAction::ToggleFocusAutoHide
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
                        if self.active_tab().terminal.mouse_mode() {
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
                if cy < TABBAR_H {
                    return;
                }
                // Commit any in-progress rename and close the help overlay so the
                // menu can't be orphaned under it.
                self.commit_rename();
                self.help_open = false;
                self.context_menu = Some((cx, cy));
                self.menu_hover = None;
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
                if !was_dragging && self.active_tab().terminal.mouse_mode() {
                    self.send_mouse_report(input::MouseEvent::LeftRelease);
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
                    let over_scrollbar = {
                        let rows = self.active_tab().terminal.rows();
                        let off = self.active_tab().terminal.scroll_offset();
                        let max = self.active_tab().terminal.scroll_max();
                        if let Some(gpu) = &self.gpu {
                            let (w, h) = (gpu.config.width, gpu.config.height);
                            jetty_render::scrollbar_rect_geom(rows, off, max, w, h, TABBAR_H, [0, 0, 0, 0])
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
                let action = input::decide_key(ctrl, shift, alt, event.physical_key.clone(), &event.logical_key, false, app_cursor);
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
                        // Copy the current selection to the clipboard.
                        if let Some(text) = self.active_tab().terminal.selection_text() {
                            if !text.is_empty() {
                                clipboard::set(&text);
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
                        // Any real keystroke jumps back to the bottom so the user sees their input.
                        self.active_tab_mut().terminal.scroll_to_bottom();
                        let w = &mut self.tabs[self.active].writer;
                        let _ = w.write_all(&bytes);
                        let _ = w.flush();
                    }
                    input::KeyAction::None => {}
                }
            }
            WindowEvent::RedrawRequested => {
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
                let tabs_meta: Vec<(String, bool)> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (t.title.clone(), i == self.active))
                    .collect();
                let context_menu = self.context_menu;
                let menu_hover = self.menu_hover;
                let help_open = self.help_open;
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
                let top_flush = self.window_mode == WindowMode::Dropdown
                    && self
                        .window
                        .as_ref()
                        .and_then(|w| {
                            let p = w.outer_position().ok()?;
                            let mon = w.current_monitor().or_else(|| w.available_monitors().next())?;
                            Some(p.y <= mon.position().y + 1)
                        })
                        .unwrap_or(false);
                let corner_mask = self.corner_mask.as_ref();
                let bayer_reveal = self.bayer_reveal.as_ref();
                let phosphor = self.phosphor.as_ref();
                let liquid = self.liquid.as_ref();
                let focus = self.focus.as_ref();
                let offscreen = self.offscreen.as_ref();
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
                // Theme accent for the reveal glow (captured before the mutable
                // gpu/text/quad borrow below).
                let summon_accent: [f32; 3] = {
                    let a = self.current_theme().palette[4];
                    [a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0]
                };
                let (Some(gpu), Some(text), Some(quad)) =
                    (&mut self.gpu, &mut self.text, &mut self.quad)
                else {
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
                // Compute window-control hover from the last cursor position.
                let ctrl_hover = ctrl_hover_at(self.cursor.0 as f32, self.cursor.1 as f32, width);
                let rename_ref = rename_state.as_ref().map(|(i, b)| (*i, b.as_str()));
                let mut bar = jetty_render::build_tab_bar_ex(
                    width, &tabs_meta, &theme, rename_ref, ctrl_hover,
                );
                // Offset the tab-bar quads + labels by the slide so the bar moves
                // with the content during the Dropdown slide-in.
                if slide_y_offset != 0.0 {
                    for q in &mut bar.quads {
                        q.y += slide_y_offset;
                    }
                    for l in &mut bar.labels {
                        l.2 += slide_y_offset;
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
                    let scene_view: &wgpu::TextureView = if tier_b_active {
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
                    let bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h, TABBAR_H + slide_y_offset, selection_bg);
                    quad.render_clear(
                        &gpu.device,
                        &gpu.queue,
                        scene_view,
                        width,
                        height,
                        &bg_rects,
                        jetty_render::default_bg_clear(&snap),
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
                        TABBAR_H + slide_y_offset,
                    );
                    // Pass 3: the tab bar (y 0..TABBAR_H) over the grid.
                    quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &bar.quads);
                    if !bar.labels.is_empty() {
                        let _ = text.render_overlays(
                            &gpu.device, &gpu.queue, scene_view, width, height, &bar.labels,
                        );
                    }
                    // Pass 4: scrollbar (spans the grid area below the bar).
                    let mut rects: Vec<jetty_render::Rect> = Vec::new();
                    if let Some(mut r) =
                        jetty_render::scrollbar_rect(&snap, width, height, TABBAR_H, scrollbar_thumb)
                    {
                        r.y += slide_y_offset;
                        rects.push(r);
                    }
                    quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &rects);
                    // Draw the right-click context menu on top of everything.
                    if let Some((mx, my)) = context_menu {
                        let menu = jetty_render::build_context_menu(mx, my, width, height, menu_hover, &theme);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &menu.quads);
                        if !menu.labels.is_empty() {
                            let _ = text.render_overlays(
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
                        let help = jetty_render::build_help_overlay(width, height, &theme);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &help.quads);
                        if !help.labels.is_empty() {
                            let _ = text.render_overlays(
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
                        );
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &popup.quads);
                        if !popup.labels.is_empty() {
                            let _ = text.render_overlays(
                                &gpu.device, &gpu.queue, scene_view, width, height, &popup.labels,
                            );
                        }
                    } else if let Some(title) = &confirm_close {
                        let popup = jetty_render::build_confirm_close(width, height, title, &theme);
                        quad.render(&gpu.device, &gpu.queue, scene_view, width, height, &popup.quads);
                        if !popup.labels.is_empty() {
                            let _ = text.render_overlays(
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
                    if let Some(mask) = corner_mask {
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
                    // (Bayer/Phosphor) write straight to the surface and compose with
                    // the dst-multiply mask. Tier-B (Liquid/Focus) SAMPLE the
                    // offscreen scene (`scene_view`) and write the displaced/blurred
                    // result to the surface `view`. At t>=1 every effect is fully
                    // resolved (zero residue, identity blit) and we stop the
                    // animation; otherwise self-drive the next frame.
                    if let Some(t) = summon_t {
                        if t < 1.0 {
                            match summon_effect {
                                SummonEffect::None => {}
                                SummonEffect::Bayer => {
                                    if let Some(reveal) = bayer_reveal {
                                        reveal.apply(
                                            &gpu.device, &gpu.queue, &view, width, height, t,
                                        );
                                    }
                                }
                                SummonEffect::Phosphor => {
                                    if let Some(ph) = phosphor {
                                        ph.apply(
                                            &gpu.device, &gpu.queue, &view, width, height,
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
                    // Self-drive the next frame while EITHER animation is live so
                    // idle CPU returns to ~0 only once BOTH have cleared.
                    if self.summon_anim.is_some() || self.slide_anim.is_some() {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    frame.present();
                }
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

/// Scrollbar thumb color derived from the active theme: theme fg at alpha 160.
fn scrollbar_thumb_for(theme: &jetty_core::Theme) -> [u8; 4] {
    // A DIM shade just above the background — subtle, not glaring. (fg/accent are
    // too bright for a scrollbar.) Blend bg→fg ~35%.
    let bg = theme.bg;
    let fg = theme.fg;
    let mix = |i: usize| (bg[i] as f32 + (fg[i] as f32 - bg[i] as f32) * 0.35) as u8;
    [mix(0), mix(1), mix(2), 210]
}

fn ctrl_hover_at(cx: f32, cy: f32, width: u32) -> jetty_render::CtrlHover {
    use jetty_render::CtrlHover;
    if cy >= TABBAR_H {
        return CtrlHover::None;
    }
    let sw = width as f32;
    let ctrl_w = jetty_render::CONTROLS_W / 5.0;
    let help_x = sw - jetty_render::CONTROLS_W; // sw - 5*ctrl_w
    let settings_x = sw - ctrl_w * 4.0;
    let min_x = sw - ctrl_w * 3.0;
    let max_x = sw - ctrl_w * 2.0;
    let close_x = sw - ctrl_w;
    if cx >= close_x {
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
        win.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        let _ = win.request_inner_size(winit::dpi::PhysicalSize::new(
            win_w.round() as u32,
            win_h.round() as u32,
        ));
    }
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
