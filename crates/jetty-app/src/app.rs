use std::io::Write;
use std::sync::Arc;
use jetty_core::{PtySession, Terminal};
use jetty_render::{GpuContext, QuadLayer, TextLayer};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::event::MouseScrollDelta;
use winit::window::{Window, WindowId};
use crate::input;

/// Logical (device-independent) font size in points. Scaled by the display's
/// scale_factor before being passed to TextLayer so glyphs are rendered at
/// physical-pixel resolution on HiDPI screens.
const FONT_LOGICAL: f32 = 16.0;

/// Fallback grid dimensions used only when computing cols/rows from the window
/// is not yet possible (e.g. before `resumed` completes). In practice the
/// derived grid replaces these immediately; they are never used for the actual
/// Terminal or PTY once a window exists.
const FALLBACK_COLS: usize = 80;
const FALLBACK_ROWS: usize = 24;

pub struct App {
    proxy: EventLoopProxy<()>,
    window: Option<Arc<Window>>,
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
    /// Whether JETTY_DEBUG is set — enables input/panel state logging to stderr.
    debug: bool,
}

impl App {
    pub fn new(proxy: EventLoopProxy<()>) -> Self {
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

        let mut app = App {
            proxy,
            window: None,
            gpu: None,
            text: None,
            quad: None,
            terminal: Terminal::new(FALLBACK_COLS, FALLBACK_ROWS),
            pty: None,
            writer: None,
            theme_idx,
            opacity,
            modifiers: winit::keyboard::ModifiersState::empty(),
            cursor: (0.0, 0.0),
            dragging_scrollbar: false,
            drag_grab_dy: 0.0,
            panel_open: false,
            dragging_slider: false,
            debug,
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
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = jetty_platform::build_window(event_loop, "Jetty", (1000, 640));
        let size = window.inner_size();
        // HiDPI: the display's scale factor (>1.0 on HiDPI/Retina screens).
        // inner_size() already returns physical pixels; we multiply the logical
        // font size by scale to get the physical font size so glyphs are sharp.
        let scale = window.scale_factor() as f32;
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        let (text, quad, cols, rows) = if let Some(ref g) = gpu {
            let text = TextLayer::new(&g.device, &g.queue, g.format, FONT_LOGICAL * scale);
            let (cw, ch) = text.cell_size();
            // Derive the grid from the physical pixel size and the physical cell size.
            let cols = (size.width as f32 / cw).floor().max(1.0) as usize;
            let rows = (size.height as f32 / ch).floor().max(1.0) as usize;
            let quad = QuadLayer::new(&g.device, g.format);
            (Some(text), Some(quad), cols, rows)
        } else {
            (None, None, FALLBACK_COLS, FALLBACK_ROWS)
        };

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
            let _ = proxy_wake.send_event(());
        }).expect("pty");
        let writer = pty.writer();

        self.window = Some(window);
        self.gpu = gpu;
        self.text = text;
        self.quad = quad;
        self.pty = Some(pty);
        self.writer = Some(writer);

        // Slow safety heartbeat — 100ms is enough for any future time-based UI
        // while virtually eliminating idle CPU waste. Real responsiveness now
        // comes from the on_data wake above, not from this tick.
        spawn_waker(self.proxy.clone());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _ev: ()) {
        let had_data = self.drain_pty();
        // If the shell child exited while we were draining its last output,
        // close the window. The waker fires this ~10x/s, so we react within a
        // frame of the shell exiting. `event_loop.exit()` is safe to call here.
        if self.terminal.child_exited() {
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
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Fired when the window is moved between monitors with different DPI.
                // Rebuild TextLayer with the new physical font size, re-derive the
                // grid, and resize the terminal + PTY so the shell sees the correct
                // dimensions. The surface size is updated by a subsequent Resized
                // event that winit sends automatically, so we only need to handle
                // the font/grid here.
                let scale = scale_factor as f32;
                if let Some(ref g) = self.gpu {
                    let new_text = TextLayer::new(&g.device, &g.queue, g.format, FONT_LOGICAL * scale);
                    let (cw, ch) = new_text.cell_size();
                    let phys_w = g.config.width;
                    let phys_h = g.config.height;
                    let cols = (phys_w as f32 / cw).floor().max(1.0) as usize;
                    let rows = (phys_h as f32 / ch).floor().max(1.0) as usize;
                    self.text = Some(new_text);
                    // Rebuild terminal with new grid (resets content — acceptable
                    // for a monitor-move event which is rare).
                    self.terminal = Terminal::new(cols, rows);
                    self.apply_theme();
                    if let Some(pty) = &self.pty {
                        pty.resize(cols as u16, rows as u16);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // --- Slider drag continuation ---
                if self.dragging_slider {
                    if let Some(gpu) = &self.gpu {
                        let (w, h) = (gpu.config.width, gpu.config.height);
                        let pv = jetty_render::build_panel(w, h, self.opacity, self.theme_idx);
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
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (w, h) = if let Some(gpu) = &self.gpu {
                    (gpu.config.width, gpu.config.height)
                } else {
                    return;
                };

                let panel_geom = if self.panel_open {
                    Some(jetty_render::build_panel(w, h, self.opacity, self.theme_idx).geom)
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
                        // scrollbar widget). Only when the running app enabled
                        // mouse reporting do we forward it as an SGR mouse press;
                        // otherwise leave behavior unchanged (a no-op here).
                        if self.terminal.mouse_mode() {
                            self.send_mouse_report(input::MouseEvent::LeftPress);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Right-click toggles the Settings panel — reliable, no keyboard
                // layout dependency.
                self.panel_open = !self.panel_open;
                if self.debug {
                    eprintln!("PANEL toggled via right-click -> open={}", self.panel_open);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // If we were dragging a host widget (scrollbar/slider), the
                // release just ends that drag and is never forwarded to the app.
                let was_dragging = self.dragging_scrollbar || self.dragging_slider;
                self.dragging_scrollbar = false;
                self.dragging_slider = false;

                // Forward a release report only when the app enabled mouse mode
                // and this release did not terminate a host-widget drag. We do
                // not re-check widget hit-testing here: the matching press was
                // only forwarded when it landed in the terminal area (action ==
                // None), so a non-drag release pairs with that forwarded press.
                if !was_dragging && self.terminal.mouse_mode() {
                    self.send_mouse_report(input::MouseEvent::LeftRelease);
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
                    input::KeyAction::Send(bytes) => {
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
                if self.terminal.child_exited() {
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
                        let pv = jetty_render::build_panel(width, height, opacity, theme_idx);
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
                    frame.present();
                }
            }
            _ => {}
        }
    }
}


fn spawn_waker(proxy: EventLoopProxy<()>) {
    // Slow safety heartbeat: 100ms is sufficient for any time-based UI ticking.
    // Responsiveness for PTY data (including p10k query replies) is now driven
    // by the on_data callback in PtySession::spawn, which wakes the loop
    // immediately on every chunk — so this tick no longer sets the latency floor.
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if proxy.send_event(()).is_err() {
            break;
        }
    });
}
