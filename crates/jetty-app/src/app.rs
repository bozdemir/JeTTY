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

pub struct App {
    proxy: EventLoopProxy<AppEvent>,
    window: Option<Arc<Window>>,
    /// Whether the window is currently visible (toggled by F9).
    visible: bool,
    /// Global hotkey manager — kept alive so the hotkey stays registered.
    hotkey_manager: Option<global_hotkey::GlobalHotKeyManager>,
    gpu: Option<GpuContext>,
    text: Option<TextLayer>,
    quad: Option<QuadLayer>,
    terminal: Terminal,
    pty: Option<PtySession>,
    writer: Option<Box<dyn Write + Send>>,
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
    /// Whether the Settings panel popup is currently visible.
    panel_open: bool,
    /// Whether the user is currently dragging the opacity slider in the Settings panel.
    dragging_slider: bool,
    /// Whether the user is currently dragging a text selection with the mouse.
    selecting: bool,
    /// Whether JETTY_DEBUG is set — enables input/panel state logging to stderr.
    debug: bool,
    /// Accumulated drag offset for the settings dialog (physical pixels).
    panel_dx: f32,
    panel_dy: f32,
    /// Whether the user is currently dragging the settings dialog by its title bar.
    dragging_dialog: bool,
    /// Cursor position (physical px) where the dialog drag started.
    drag_dialog_anchor: (f32, f32),
    /// panel_dx / panel_dy at the moment the dialog drag started.
    drag_dialog_start: (f32, f32),
    /// When Some, the right-click context menu is open at this physical-pixel position.
    context_menu: Option<(f32, f32)>,
    /// Index of the menu item currently under the cursor (for hover highlight).
    menu_hover: Option<usize>,
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
            terminal: Terminal::new(FALLBACK_COLS, FALLBACK_ROWS),
            pty: None,
            writer: None,
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
            panel_open: false,
            dragging_slider: false,
            selecting: false,
            debug,
            panel_dx: 0.0,
            panel_dy: 0.0,
            dragging_dialog: false,
            drag_dialog_anchor: (0.0, 0.0),
            drag_dialog_start: (0.0, 0.0),
            context_menu: None,
            menu_hover: None,
        };
        // Apply the initial theme+opacity so Terminal::new env defaults are
        // overridden by our managed state (avoids double-reads from env).
        app.apply_theme();
        app
    }

    /// Build the current theme from `theme_idx`, apply `opacity` to its bg
    /// alpha, and push it into the terminal.
    fn apply_theme(&mut self) {
        let mut t = jetty_core::Theme::by_name(jetty_core::theme::PRESETS[self.theme_idx]);
        t.bg[3] = (self.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        self.terminal.set_theme(t);
    }

    /// Compute the scroll offset from the current cursor position during a drag.
    /// `w` and `h` are the current surface dimensions in physical pixels.
    fn apply_scroll_from_cursor(&mut self, w: u32, h: u32) {
        let max = self.terminal.scroll_max();
        if max == 0 {
            return;
        }
        let screen_h = h as f32;
        // Recompute thumb height the same way as the geometry function.
        let rows = self.terminal.rows();
        let total = rows + max;
        let thumb_h = (screen_h * rows as f32 / total as f32).max(24.0);

        let travel = screen_h - thumb_h;
        if travel <= 0.0 {
            return;
        }

        let thumb_top = (self.cursor.1 as f32 - self.drag_grab_dy).clamp(0.0, travel);
        // frac=0 → thumb at top → scroll_offset=max (oldest history)
        // frac=1 → thumb at bottom → scroll_offset=0 (live bottom)
        let frac = thumb_top / travel;
        let offset = ((1.0 - frac) * max as f32).round() as usize;
        self.terminal.scroll_to_offset(offset);
        // suppress unused warning on w
        let _ = w;
    }

    /// Compute opacity from the current cursor x relative to a slider track rect.
    fn opacity_from_cursor(&self, track: &jetty_render::Rect) -> f32 {
        let frac = ((self.cursor.0 as f32 - track.x) / track.w).clamp(0.0, 1.0);
        (0.1 + frac * 0.9).clamp(0.1, 1.0)
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
        let row = (self.cursor.1 as f32 / cell_h).floor() as i64 + 1;
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
        let cols = self.terminal.cols().saturating_sub(1);
        let rows = self.terminal.rows().saturating_sub(1);
        let col = ((self.cursor.0 as f32 / cell_w).floor() as i64).clamp(0, cols as i64) as usize;
        let line = ((self.cursor.1 as f32 / cell_h).floor() as i64).clamp(0, rows as i64) as usize;
        Some((line, col))
    }

    /// Paste `text` to the PTY, wrapping in bracketed-paste sequences if the
    /// running application has enabled `\e[?2004h`.
    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(w) = &mut self.writer {
            if self.terminal.bracketed_paste() {
                let _ = w.write_all(b"\x1b[200~");
            }
            let _ = w.write_all(text.as_bytes());
            if self.terminal.bracketed_paste() {
                let _ = w.write_all(b"\x1b[201~");
            }
            let _ = w.flush();
        }
    }

    /// Encode a mouse event and write it to the PTY. Used only when the running
    /// application has enabled mouse reporting (`mouse_mode()`). The wire format
    /// matches what the app requested: SGR (1006) encoding when `mouse_sgr()` is
    /// true (`\e[?1006h`), otherwise the legacy X10 encoding.
    fn send_mouse_report(&mut self, event: input::MouseEvent) {
        let Some((col, row)) = self.cursor_cell() else { return };
        let sgr = self.terminal.mouse_sgr();
        let bytes = input::encode_mouse(event, col, row, sgr);
        if let Some(w) = &mut self.writer {
            let _ = w.write_all(&bytes);
            let _ = w.flush();
        }
    }

    /// Drain pending PTY output into the terminal and flush any query replies.
    ///
    /// Returns `true` if any bytes were consumed (PTY data or reply writes),
    /// so the caller can skip `request_redraw()` when nothing changed — making
    /// the 100ms heartbeat essentially free when the terminal is idle.
    fn drain_pty(&mut self) -> bool {
        let mut had_data = false;
        if let Some(pty) = &self.pty {
            while let Ok(chunk) = pty.output().try_recv() {
                self.terminal.feed(&chunk);
                had_data = true;
            }
        }
        // The terminal may have produced replies to host queries (DSR/DA, etc.)
        // while feeding output. Send those back to the PTY so the shell's
        // startup queries succeed (fixes the red "x" at the first prompt).
        //
        // Ordering: we drain *after* feeding all pending output above, so every
        // query parsed this tick has already enqueued its reply before we write
        // here, and we flush immediately — the shell sees the answers before the
        // next batch of PTY output is produced. Keystroke writes go through a
        // separate path (KeyAction::Send), so query replies never interleave
        // with, or get delayed behind, user input.
        let replies = self.terminal.drain_pty_writes();
        if !replies.is_empty() {
            if let Some(w) = &mut self.writer {
                let _ = w.write_all(&replies);
                let _ = w.flush();
            }
            had_data = true;
        }
        had_data
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
        let rows = (h as f32 / ch).floor().max(1.0) as usize;
        self.terminal.resize(cols, rows);
        if let Some(pty) = &self.pty {
            pty.resize(cols as u16, rows as u16);
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
        if let Some(win) = &self.window {
            win.set_visible(self.visible);
            if self.visible {
                center_window(win);
                win.focus_window();
                win.request_redraw();
            }
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = jetty_platform::build_window(event_loop, "JeTTY", (1000, 640));
        let size = window.inner_size();
        // HiDPI: the display's scale factor (>1.0 on HiDPI/Retina screens).
        // inner_size() already returns physical pixels; we multiply the logical
        // font size by scale to get the physical font size so glyphs are sharp.
        let scale = window.scale_factor() as f32;
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        let (text, quad, cols, rows) = if let Some(ref g) = gpu {
            let text = TextLayer::new_with_family(
                &g.device, &g.queue, g.format, self.font_logical * scale, &self.font_family,
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
        }

        // Re-build the terminal with the derived grid size so the PTY and
        // terminal agree with the actual window layout.
        self.terminal = Terminal::new(cols, rows);
        self.apply_theme();

        // Pass an `on_data` callback that wakes the winit event loop the
        // instant bytes arrive from the PTY. This means drain_pty (and thus
        // the query-reply write) runs within ~1ms of any PTY output rather
        // than waiting up to 16ms for a polling tick — critical for p10k's
        // cursor-position / capability queries which have tight timeouts.
        let proxy_wake = self.proxy.clone();
        let pty = PtySession::spawn(cols as u16, rows as u16, move || {
            let _ = proxy_wake.send_event(AppEvent::Wake);
        }).expect("pty");
        let writer = pty.writer();

        self.window = Some(window);
        self.gpu = gpu;
        self.text = text;
        self.quad = quad;
        self.pty = Some(pty);
        self.writer = Some(writer);

        // Register the F9 global hotkey (Yakuake-style toggle). This only works
        // on X11; on Wayland registration will fail and we log a warning without
        // crashing. The manager must be kept alive (stored in self.hotkey_manager)
        // or the hotkey is automatically unregistered when it drops.
        if self.hotkey_manager.is_none() {
            match global_hotkey::GlobalHotKeyManager::new() {
                Ok(manager) => {
                    let hotkey = global_hotkey::hotkey::HotKey::new(
                        None,
                        global_hotkey::hotkey::Code::F9,
                    );
                    if let Err(e) = manager.register(hotkey) {
                        eprintln!("global hotkey F9 unavailable (Wayland? already grabbed?) — {e}");
                    } else {
                        // Spawn a thread that forwards F9-pressed events to the winit loop.
                        let proxy_hotkey = self.proxy.clone();
                        std::thread::spawn(move || {
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
                        });
                        self.hotkey_manager = Some(manager);
                    }
                }
                Err(e) => {
                    eprintln!("global hotkey F9 unavailable (Wayland? already grabbed?) — {e}");
                }
            }
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
                let had_data = self.drain_pty();
                // If the shell child exited while we were draining its last output,
                // close the window. The waker fires this ~10x/s, so we react within a
                // frame of the shell exiting. `event_loop.exit()` is safe to call here.
                if self.terminal.child_exited() || self.pty.as_ref().is_some_and(|p| p.child_exited()) {
                    event_loop.exit();
                    return;
                }
                // Damage-driven: only request a redraw when PTY data actually arrived
                // (or query replies were sent). When the terminal is idle, the 100ms
                // heartbeat drains nothing and we skip the redraw — no Vec allocation,
                // no GPU work. Any state change that affects rendering still calls
                // request_redraw directly in the relevant window_event arm.
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(gpu), Some(text)) = (&self.gpu, &mut self.text) {
                    text.resize(gpu);
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
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // --- Dialog drag continuation ---
                if self.dragging_dialog {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    self.panel_dx = self.drag_dialog_start.0 + (cx - self.drag_dialog_anchor.0);
                    self.panel_dy = self.drag_dialog_start.1 + (cy - self.drag_dialog_anchor.1);
                    if let Some(w_) = &self.window {
                        w_.request_redraw();
                    }
                    return;
                }
                // --- Slider drag continuation ---
                if self.dragging_slider {
                    if let Some(gpu) = &self.gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        let pv = jetty_render::build_panel(w, h, self.opacity, self.theme_idx, self.font_logical, &self.font_families, &self.font_family, self.font_scroll_offset, self.panel_dx, self.panel_dy);
                        self.opacity = self.opacity_from_cursor(&pv.geom.slider_track);
                        self.apply_theme();
                    }
                    if let Some(w_) = &self.window {
                        w_.request_redraw();
                    }
                    return; // don't also drag the scrollbar
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
                if self.selecting && !self.terminal.mouse_mode() {
                    if let Some((line, col)) = self.cursor_cell_0() {
                        self.terminal.selection_update(line, col);
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
                // --- Context menu hover update ---
                if let Some((mx, my)) = self.context_menu {
                    if let Some(gpu) = &self.gpu {
                        let (win_w, win_h) = (gpu.config.width, gpu.config.height);
                        let menu = jetty_render::build_context_menu(mx, my, win_w, win_h, None);
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

                // --- Context menu hit-test (consume the click entirely) ---
                if let Some((mx, my)) = self.context_menu.take() {
                    self.menu_hover = None;
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    let menu = jetty_render::build_context_menu(mx, my, w, h, None);
                    let hit = menu.item_rects.iter().position(|r| {
                        cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h
                    });
                    if let Some(idx) = hit {
                        match idx {
                            0 => {
                                // Copy
                                if let Some(text) = self.terminal.selection_text() {
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
                                self.terminal.select_all();
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

                let panel_geom = if self.panel_open {
                    Some(jetty_render::build_panel(w, h, self.opacity, self.theme_idx, self.font_logical, &self.font_families, &self.font_family, self.font_scroll_offset, self.panel_dx, self.panel_dy).geom)
                } else {
                    None
                };

                let rows = self.terminal.rows();
                let scroll_offset = self.terminal.scroll_offset();
                let scroll_max = self.terminal.scroll_max();
                let scrollbar = jetty_render::scrollbar_rect_geom(rows, scroll_offset, scroll_max, w, h);

                let cx = self.cursor.0 as f32;
                let cy = self.cursor.1 as f32;

                match input::decide_mouse_press(
                    panel_geom.as_ref(),
                    scrollbar.as_ref(),
                    cx,
                    cy,
                ) {
                    input::MouseAction::StartSliderDrag => {
                        let track = panel_geom.as_ref().map(|g| g.slider_track);
                        self.dragging_slider = true;
                        if let Some(track_rect) = track {
                            self.opacity = self.opacity_from_cursor(&track_rect);
                            self.apply_theme();
                        }
                        if let Some(w_) = &self.window {
                            w_.request_redraw();
                        }
                    }
                    input::MouseAction::SetTheme(i) => {
                        self.theme_idx = i;
                        self.apply_theme();
                        if let Some(w_) = &self.window {
                            w_.request_redraw();
                        }
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
                        if let Some(w_) = &self.window {
                            w_.request_redraw();
                        }
                    }
                    input::MouseAction::FontScrollDown => {
                        const MAX_FONT_ROWS: usize = 5;
                        let max_offset = self.font_families.len().saturating_sub(MAX_FONT_ROWS);
                        self.font_scroll_offset = (self.font_scroll_offset + 1).min(max_offset);
                        if let Some(w_) = &self.window {
                            w_.request_redraw();
                        }
                    }
                    input::MouseAction::StartDialogDrag => {
                        self.dragging_dialog = true;
                        self.drag_dialog_anchor = (self.cursor.0 as f32, self.cursor.1 as f32);
                        self.drag_dialog_start = (self.panel_dx, self.panel_dy);
                    }
                    input::MouseAction::ConsumePanel => {
                        // Swallow the click; no state change needed.
                    }
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
                        if self.terminal.mouse_mode() {
                            self.send_mouse_report(input::MouseEvent::LeftPress);
                        } else {
                            // Clear prior selection and begin a new one.
                            self.terminal.selection_clear();
                            if let Some((line, col)) = self.cursor_cell_0() {
                                self.terminal.selection_start(line, col);
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
                // If the settings panel is open, ignore — the panel captures all
                // interaction while it is visible.
                if !self.panel_open {
                    let cx = self.cursor.0 as f32;
                    let cy = self.cursor.1 as f32;
                    self.context_menu = Some((cx, cy));
                    self.menu_hover = None;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // If we were dragging a host widget (scrollbar/slider/dialog), the
                // release just ends that drag and is never forwarded to the app.
                let was_dragging = self.dragging_scrollbar || self.dragging_slider || self.dragging_dialog;
                self.dragging_scrollbar = false;
                self.dragging_slider = false;
                self.dragging_dialog = false;

                // End text selection and copy-on-select.
                if self.selecting {
                    self.selecting = false;
                    // Copy-on-select: if we got any text, put it in the clipboard.
                    if let Some(text) = self.terminal.selection_text() {
                        if !text.is_empty() {
                            clipboard::set(&text);
                        } else {
                            // Empty drag (plain click) — clear the selection highlight.
                            self.terminal.selection_clear();
                        }
                    } else {
                        // No selection text → plain click, clear selection.
                        self.terminal.selection_clear();
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
                if !was_dragging && self.terminal.mouse_mode() {
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
                    // When the panel is open and the cursor is over the font-family
                    // list, wheel-scroll the list instead of the terminal scrollback.
                    if self.panel_open && !self.font_families.is_empty() {
                        if let Some(gpu) = &self.gpu {
                            let (w, h) = (gpu.config.width, gpu.config.height);
                            let pv = jetty_render::build_panel(
                                w, h, self.opacity, self.theme_idx, self.font_logical,
                                &self.font_families, &self.font_family, self.font_scroll_offset,
                                self.panel_dx, self.panel_dy,
                            );
                            let cx = self.cursor.0 as f32;
                            let cy = self.cursor.1 as f32;
                            let over_list = pv.geom.font_rows.iter()
                                .any(|r| cx >= r.x && cx <= r.x + r.w
                                         && cy >= pv.geom.font_rows.first().map(|r| r.y).unwrap_or(0.0)
                                         && cy <= pv.geom.font_rows.last().map(|r| r.y + r.h).unwrap_or(0.0));
                            if over_list {
                                // lines > 0 = wheel up = scroll list up (decrement offset).
                                // max_offset: last window that still shows MAX_FONT_ROWS entries.
                                const MAX_FONT_ROWS: usize = 5;
                                let max_offset = self.font_families.len().saturating_sub(MAX_FONT_ROWS);
                                if lines > 0 {
                                    self.font_scroll_offset = self.font_scroll_offset.saturating_sub(1);
                                } else {
                                    self.font_scroll_offset = (self.font_scroll_offset + 1).min(max_offset);
                                }
                                if let Some(win) = &self.window {
                                    win.request_redraw();
                                }
                                return;
                            }
                        }
                    }

                    // When the app enabled mouse reporting, forward wheel events
                    // as SGR button 64 (up) / 65 (down) — but only over the
                    // terminal area, so wheeling over the scrollbar still scrolls
                    // the host scrollback. One report per LineDelta notch
                    // (clamped) keeps apps like less/htop responsive without
                    // flooding the PTY.
                    let over_scrollbar = {
                        let rows = self.terminal.rows();
                        let off = self.terminal.scroll_offset();
                        let max = self.terminal.scroll_max();
                        if let Some(gpu) = &self.gpu {
                            let (w, h) = (gpu.config.width, gpu.config.height);
                            jetty_render::scrollbar_rect_geom(rows, off, max, w, h)
                                .map(|r| {
                                    let cx = self.cursor.0 as f32;
                                    cx >= r.x && cx <= r.x + r.w
                                })
                                .unwrap_or(false)
                        } else {
                            false
                        }
                    };

                    if self.terminal.mouse_mode() && !over_scrollbar {
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
                        self.terminal.scroll_lines(lines);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let alt = self.modifiers.alt_key();
                let app_cursor = self.terminal.app_cursor_keys();
                let action = input::decide_key(ctrl, shift, alt, event.physical_key.clone(), &event.logical_key, self.panel_open, app_cursor);
                if self.debug {
                    let action_name = match &action {
                        input::KeyAction::TogglePanel => "TogglePanel",
                        input::KeyAction::ClosePanel => "ClosePanel",
                        input::KeyAction::CycleTheme => "CycleTheme",
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
                        self.panel_open = !self.panel_open;
                        if self.debug {
                            eprintln!("PANEL toggled via key -> open={}", self.panel_open);
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::ClosePanel => {
                        self.panel_open = false;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::CycleTheme => {
                        self.theme_idx = (self.theme_idx + 1)
                            % jetty_core::theme::PRESETS.len();
                        self.apply_theme();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::OpacityUp => {
                        self.opacity = (self.opacity + 0.05).min(1.0);
                        self.apply_theme();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::OpacityDown => {
                        self.opacity = (self.opacity - 0.05).max(0.1);
                        self.apply_theme();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::ScrollPageUp => {
                        self.terminal.scroll_page(true);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    input::KeyAction::ScrollPageDown => {
                        self.terminal.scroll_page(false);
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
                        if let Some(text) = self.terminal.selection_text() {
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
                        self.terminal.scroll_to_bottom();
                        if let Some(w) = &mut self.writer {
                            let _ = w.write_all(&bytes);
                            let _ = w.flush();
                        }
                    }
                    input::KeyAction::None => {}
                }
            }
            WindowEvent::RedrawRequested => {
                self.drain_pty();
                // The shell may have exited as part of the output we just
                // drained (e.g. the user typed `exit`); close the window rather
                // than render one more dead frame.
                if self.terminal.child_exited() || self.pty.as_ref().is_some_and(|p| p.child_exited()) {
                    event_loop.exit();
                    return;
                }
                let snap = self.terminal.snapshot();
                let panel_open = self.panel_open;
                let opacity = self.opacity;
                let theme_idx = self.theme_idx;
                let (Some(gpu), Some(text), Some(quad)) =
                    (&mut self.gpu, &mut self.text, &mut self.quad)
                else {
                    return;
                };
                let width = gpu.config.width;
                let height = gpu.config.height;
                if let Some((frame, view)) = gpu.acquire_frame() {
                    // Pass 1: clear to the theme bg and paint per-cell background
                    // quads (reverse-video / colored backgrounds) UNDER the text.
                    let (cell_w, cell_h) = text.cell_size();
                    let bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h);
                    quad.render_clear(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        width,
                        height,
                        &bg_rects,
                        jetty_render::default_bg_clear(&snap),
                    );
                    // Pass 2: draw glyphs on top of the painted background (load).
                    let _ = text.render_to(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        width,
                        height,
                        &snap,
                        false,
                    );
                    let mut rects: Vec<jetty_render::Rect> = Vec::new();
                    if let Some(r) =
                        jetty_render::scrollbar_rect(&snap, width, height)
                    {
                        rects.push(r);
                    }
                    let mut labels: Vec<(String, f32, f32, [u8; 3])> = Vec::new();
                    if panel_open {
                        let pv = jetty_render::build_panel(width, height, opacity, theme_idx, self.font_logical, &self.font_families, &self.font_family, self.font_scroll_offset, self.panel_dx, self.panel_dy);
                        rects.extend(pv.quads);
                        labels = pv.labels;
                    }
                    quad.render(&gpu.device, &gpu.queue, &view, width, height, &rects);
                    if !labels.is_empty() {
                        let _ = text.render_overlays(
                            &gpu.device,
                            &gpu.queue,
                            &view,
                            width,
                            height,
                            &labels,
                        );
                    }
                    // Draw the right-click context menu on top of everything.
                    if let Some((mx, my)) = self.context_menu {
                        let menu = jetty_render::build_context_menu(mx, my, width, height, self.menu_hover);
                        quad.render(&gpu.device, &gpu.queue, &view, width, height, &menu.quads);
                        if !menu.labels.is_empty() {
                            let _ = text.render_overlays(
                                &gpu.device,
                                &gpu.queue,
                                &view,
                                width,
                                height,
                                &menu.labels,
                            );
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

/// Centre `win` on its current monitor (or the first available monitor if the
/// current one cannot be determined). No-ops gracefully if no monitor info is
/// available.
fn center_window(win: &Arc<Window>) {
    let mon_size = win
        .current_monitor()
        .or_else(|| win.available_monitors().next())
        .map(|m| m.size());

    if let Some(mon) = mon_size {
        let win_size = win.outer_size();
        let x = (mon.width.saturating_sub(win_size.width) / 2) as i32;
        let y = (mon.height.saturating_sub(win_size.height) / 2) as i32;
        win.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    }
}
