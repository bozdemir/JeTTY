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

const COLS: usize = 100;
const ROWS: usize = 30;

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
            terminal: Terminal::new(COLS, ROWS),
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

    fn drain_pty(&mut self) {
        if let Some(pty) = &self.pty {
            while let Ok(chunk) = pty.output().try_recv() {
                self.terminal.feed(&chunk);
            }
        }
        // The terminal may have produced replies to host queries (DSR/DA, etc.)
        // while feeding output. Send those back to the PTY so the shell's
        // startup queries succeed (fixes the red "x" at the first prompt).
        let replies = self.terminal.drain_pty_writes();
        if !replies.is_empty() {
            if let Some(w) = &mut self.writer {
                let _ = w.write_all(&replies);
                let _ = w.flush();
            }
        }
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = jetty_platform::build_window(event_loop, "Jetty", (1000, 640));
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone(), size.width, size.height);
        let (text, quad) = if let Some(ref g) = gpu {
            let text = TextLayer::new(&g.device, &g.queue, g.format, 16.0);
            let quad = QuadLayer::new(&g.device, g.format);
            (Some(text), Some(quad))
        } else {
            (None, None)
        };

        let pty = PtySession::spawn(COLS as u16, ROWS as u16).expect("pty");
        let writer = pty.writer();

        self.window = Some(window);
        self.gpu = gpu;
        self.text = text;
        self.quad = quad;
        self.pty = Some(pty);
        self.writer = Some(writer);

        spawn_waker(self.proxy.clone());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _ev: ()) {
        self.drain_pty();
        if let Some(w) = &self.window {
            w.request_redraw();
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
                    input::MouseAction::None => {}
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
                self.dragging_scrollbar = false;
                self.dragging_slider = false;
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
                    self.terminal.scroll_lines(lines);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let alt = self.modifiers.alt_key();
                let action = input::decide_key(ctrl, shift, alt, event.physical_key.clone(), &event.logical_key, self.panel_open);
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
                    let _ = text.render_to(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        width,
                        height,
                        &snap,
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
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(16));
        if proxy.send_event(()).is_err() {
            break;
        }
    });
}
