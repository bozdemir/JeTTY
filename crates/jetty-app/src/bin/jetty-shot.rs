/// Headless offscreen screenshot tool — renders one terminal frame to a PNG
/// with NO window, surface, or display.
///
/// Config via env:
///   JETTY_SHOT_OUT   — output path (default: /tmp/jetty-shot.png)
///   JETTY_SHOT_INPUT — ANSI bytes to feed the terminal (default: built-in sample)
///   JETTY_THEME      — theme name (picked up automatically via Terminal::new)
///   JETTY_OPACITY    — opacity 0.0..1.0 (picked up automatically via Terminal::new)
///
/// If the terminal bg alpha < 255, the rendered image is composited over a
/// checkerboard (alternating 16px squares of [40,40,40] and [90,90,90]) so
/// transparency is visible in the output PNG.
use std::fs::File;
use std::io::BufWriter;

use jetty_render::{QuadLayer, TextLayer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_path =
        std::env::var("JETTY_SHOT_OUT").unwrap_or_else(|_| "/tmp/jetty-shot.png".to_string());

    let width: u32 = 1000;
    let height: u32 = 640;
    // Allow headless renders at different font sizes so the test harness can
    // verify that font-size changes produce a different cell grid.
    let font_size: f32 = std::env::var("JETTY_FONT_SIZE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(6.0, 48.0))
        .unwrap_or(16.0);

    let default_input = "\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/jetty\x1b[0m$ ls --color\r\n\x1b[1;34msrc\x1b[0m  \x1b[33mCargo.toml\x1b[0m  \x1b[31mREADME.md\x1b[0m\r\n\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/jetty\x1b[0m$ \r\n";
    let input_bytes: Vec<u8> = match std::env::var("JETTY_SHOT_INPUT") {
        Ok(s) => s.into_bytes(),
        Err(_) => default_input.as_bytes().to_vec(),
    };

    // --- wgpu offscreen setup (no surface) ---
    // Match the live app: Vulkan-only instance (skips GLES enumeration), with an
    // all-backends fallback if no Vulkan adapter is present.
    let mut instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))?
        }
    };

    eprintln!(
        "jetty-shot: GPU adapter = {} ({:?})",
        adapter.get_info().name,
        adapter.get_info().backend
    );

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jetty-shot-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            ..Default::default()
        }))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // Allow rendering with a specific font family for visual comparison.
    let font_family = std::env::var("JETTY_FONT_FAMILY")
        .unwrap_or_else(|_| "MesloLGS NF".to_string());
    if std::env::var("JETTY_FONT_FAMILY").is_ok() {
        eprintln!("jetty-shot: JETTY_FONT_FAMILY={font_family:?}");
    }

    // --- Build TextLayer ---
    let mut text = TextLayer::new_with_family(&device, &queue, format, font_size, &font_family);
    let (cell_w, cell_h) = text.cell_size();
    let mono_families = text.monospace_families();
    eprintln!("jetty-shot: {} monospace families found (e.g. {:?})", mono_families.len(), mono_families.first());

    let cols = (width as f32 / cell_w).floor().max(1.0) as usize;
    let rows = (height as f32 / cell_h).floor().max(1.0) as usize;

    eprintln!("jetty-shot: grid = {cols}x{rows} cells (cell {cell_w:.1}x{cell_h:.1}px)");

    // --- Build terminal snapshot ---
    // Terminal::new picks up JETTY_THEME and JETTY_OPACITY from the environment.
    let mut terminal = jetty_core::Terminal::new(cols, rows);

    if std::env::var("JETTY_SHOT_PTY").is_ok() {
        // Drive a REAL shell offscreen so we can see the live startup prompt
        // (e.g. zsh+p10k) settle exactly as in the running app. This feeds the
        // shell's output into the terminal and writes the terminal's query
        // replies (DSR/DA/etc.) back to the PTY, which is what clears the
        // startup red "x".
        use std::io::Write;
        let pty = jetty_core::PtySession::spawn(cols as u16, rows as u16, || {})?;
        let mut w = pty.writer();

        // ~3.5s startup settle: 700 iterations of 5ms.
        // Short sleep = replies go out within ~5ms of each query, well inside
        // p10k's capability-probe timeouts (mirrors the fixed live-app latency).
        for _ in 0..700 {
            while let Ok(chunk) = pty.output().try_recv() {
                terminal.feed(&chunk);
            }
            let replies = terminal.drain_pty_writes();
            if !replies.is_empty() {
                w.write_all(&replies).ok();
                w.flush().ok();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Final drain to capture anything emitted during the last sleep.
        while let Ok(chunk) = pty.output().try_recv() {
            terminal.feed(&chunk);
        }
        eprintln!("jetty-shot: JETTY_SHOT_PTY mode drove a real shell for ~3.5s");

        // Optional: inject a command into the live shell after startup settles.
        // Writes the command + newline to the PTY, then runs the SAME tight
        // drain/respond loop for another ~3.5s so the command executes and the
        // prompt fully redraws (including p10k's post-command queries) before we
        // snapshot.
        if let Ok(cmd) = std::env::var("JETTY_SHOT_PTY_CMD") {
            w.write_all(cmd.as_bytes()).ok();
            w.write_all(b"\n").ok();
            w.flush().ok();
            eprintln!("jetty-shot: injected JETTY_SHOT_PTY_CMD={cmd:?}");

            // ~3.5s: 700 iterations of 5ms — tight loop so replies are prompt.
            for _ in 0..700 {
                while let Ok(chunk) = pty.output().try_recv() {
                    terminal.feed(&chunk);
                }
                let replies = terminal.drain_pty_writes();
                if !replies.is_empty() {
                    w.write_all(&replies).ok();
                    w.flush().ok();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Final drain after the command loop.
            while let Ok(chunk) = pty.output().try_recv() {
                terminal.feed(&chunk);
            }
            eprintln!("jetty-shot: ran injected command for ~3.5s");
        }
    } else {
        terminal.feed(&input_bytes);
    }

    // Optional: scroll the view before snapshotting (JETTY_SHOT_SCROLL, i32, positive = up).
    if let Ok(scroll_str) = std::env::var("JETTY_SHOT_SCROLL") {
        if let Ok(n) = scroll_str.parse::<i32>() {
            if n != 0 {
                terminal.scroll_lines(n);
                eprintln!("jetty-shot: scrolled {} lines (positive=up into history)", n);
            }
        }
    }

    let snap = terminal.snapshot();

    let bg_alpha = snap.bg_rgba[3];
    eprintln!(
        "jetty-shot: theme={} bg_rgba={:?} compositing={}",
        terminal.theme().name,
        snap.bg_rgba,
        bg_alpha < 255
    );

    // --- Create offscreen texture ---
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jetty-shot-tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut quad = QuadLayer::new(&device, format);

    // --- Pass 1: clear to theme bg + paint per-cell background quads UNDER text ---
    let (cell_w, cell_h) = text.cell_size();
    let bg_rects = jetty_render::cell_bg_rects(&snap, cell_w, cell_h, 0.0);
    quad.render_clear(
        &device,
        &queue,
        &view,
        width,
        height,
        &bg_rects,
        jetty_render::default_bg_clear(&snap),
    );

    // --- Pass 2: render the grid text on top of the painted background (load) ---
    text.render_to(&device, &queue, &view, width, height, &snap, false, 0.0)?;

    // --- Draw scrollbar quad (and optionally the settings panel) over the text ---
    {
        let mut rects: Vec<jetty_render::Rect> = Vec::new();
        if let Some(r) = jetty_render::scrollbar_rect(&snap, width, height, 0.0) {
            rects.push(r);
        }

        let shot_panel = std::env::var("JETTY_SHOT_PANEL").unwrap_or_else(|_| "0".to_string());
        let mut panel_labels = if shot_panel == "1" {
            // Read opacity + theme_idx from env (same vars as the live app).
            let opacity = std::env::var("JETTY_OPACITY")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .map(|v| v.clamp(0.1, 1.0))
                .unwrap_or(1.0);
            let theme_name = std::env::var("JETTY_THEME").unwrap_or_default();
            let theme_idx = jetty_core::theme::PRESETS
                .iter()
                .position(|&n| n == theme_name.as_str())
                .unwrap_or(0);

            // JETTY_SHOT_PANEL_OFFSET="dx,dy" — two f32 (default "0,0").
            // Lets the caller verify the moveable-dialog path at an offset.
            let (panel_dx, panel_dy) = std::env::var("JETTY_SHOT_PANEL_OFFSET")
                .ok()
                .and_then(|s| {
                    let mut parts = s.splitn(2, ',');
                    let dx = parts.next()?.parse::<f32>().ok()?;
                    let dy = parts.next()?.parse::<f32>().ok()?;
                    Some((dx, dy))
                })
                .unwrap_or((0.0, 0.0));

            let panel_radius = std::env::var("JETTY_CORNER_RADIUS")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 24.0))
                .unwrap_or(10.0);
            let pv = jetty_render::build_panel(
                width, height, opacity, theme_idx, font_size,
                &mono_families,
                mono_families.first().map(String::as_str).unwrap_or(""),
                0,
                panel_radius,
                panel_dx,
                panel_dy,
            );
            rects.extend(pv.quads);
            eprintln!(
                "jetty-shot: panel enabled (opacity={opacity:.2}, theme_idx={theme_idx}, font_size={font_size}, offset=({panel_dx},{panel_dy}))"
            );
            pv.labels
        } else {
            Vec::new()
        };

        // JETTY_SHOT_MENU — render the right-click context menu for visual checks.
        if std::env::var("JETTY_SHOT_MENU").is_ok() {
            let menu = jetty_render::build_context_menu(620.0, 120.0, width, height, Some(1));
            rects.extend(menu.quads);
            panel_labels.extend(menu.labels);
        }

        quad.render(&device, &queue, &view, width, height, &rects);

        // Render panel text labels on top of the panel quads.
        if !panel_labels.is_empty() {
            let _ = text.render_overlays(&device, &queue, &view, width, height, &panel_labels);
        }
    }

    // --- Read back to CPU ---
    // wgpu requires bytes_per_row to be a multiple of 256.
    let unpadded = width * 4;
    let align: u32 = 256;
    let padded = ((unpadded + align - 1) / align) * align;

    let buffer_size = (padded * height) as u64;
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("jetty-shot-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback_buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));

    // Map and read the buffer.
    let (tx, rx) = std::sync::mpsc::channel();
    readback_buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;

    let padded_data = readback_buffer.slice(..).get_mapped_range();
    // Strip row padding: copy only the unpadded bytes per row.
    let mut tight: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let row_start = (row * padded) as usize;
        let row_end = row_start + unpadded as usize;
        tight.extend_from_slice(&padded_data[row_start..row_end]);
    }
    drop(padded_data);
    readback_buffer.unmap();

    // --- Rounded-corner alpha mask (JETTY_CORNER_RADIUS) ---
    // Apply the SAME antialiased rounded-rect SDF mask the live GPU pass uses, so
    // the shot shows transparent (rounded) corners over the checkerboard while the
    // center stays intact. The texture is premultiplied alpha, so multiply r/g/b/a
    // by the coverage to keep premultiplication consistent.
    let corner_radius = std::env::var("JETTY_CORNER_RADIUS")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 24.0))
        .unwrap_or(0.0);
    if corner_radius > 0.0 {
        eprintln!("jetty-shot: applying rounded-corner mask (radius={corner_radius}px)");
        for y in 0..height {
            for x in 0..width {
                let cov = jetty_render::rounded_rect_coverage(
                    x as f32, y as f32, width as f32, height as f32, corner_radius,
                );
                if cov < 1.0 {
                    let idx = ((y * width + x) * 4) as usize;
                    for c in 0..4 {
                        tight[idx + c] = (tight[idx + c] as f32 * cov).round() as u8;
                    }
                }
            }
        }
    }

    // --- Composite over checkerboard if bg alpha < 255 (or a corner radius is set,
    // so the now-transparent corners reveal the checkerboard even on an opaque
    // theme) ---
    // The rendered texture uses premultiplied alpha (the clear color is already
    // premultiplied in text.rs).  We un-premultiply before blending onto the
    // checkerboard, then output an opaque RGBA PNG.
    let composited = if bg_alpha < 255 || corner_radius > 0.0 {
        eprintln!("jetty-shot: compositing over checkerboard (bg alpha={})", bg_alpha);
        const TILE: u32 = 16;
        const DARK: [u8; 3] = [40, 40, 40];
        const LIGHT: [u8; 3] = [90, 90, 90];

        let mut out = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                let src_r = tight[idx] as f32 / 255.0;
                let src_g = tight[idx + 1] as f32 / 255.0;
                let src_b = tight[idx + 2] as f32 / 255.0;
                let src_a = tight[idx + 3] as f32 / 255.0;

                // Checkerboard background
                let tile_x = x / TILE;
                let tile_y = y / TILE;
                let checker = if (tile_x + tile_y) % 2 == 0 { DARK } else { LIGHT };
                let dst_r = checker[0] as f32 / 255.0;
                let dst_g = checker[1] as f32 / 255.0;
                let dst_b = checker[2] as f32 / 255.0;

                // Source is premultiplied alpha: un-premultiply for correct over blend.
                // over(src_premul, dst) = src_premul + dst*(1-alpha)
                let out_r = (src_r + dst_r * (1.0 - src_a)).min(1.0);
                let out_g = (src_g + dst_g * (1.0 - src_a)).min(1.0);
                let out_b = (src_b + dst_b * (1.0 - src_a)).min(1.0);

                out[idx] = (out_r * 255.0) as u8;
                out[idx + 1] = (out_g * 255.0) as u8;
                out[idx + 2] = (out_b * 255.0) as u8;
                out[idx + 3] = 255; // opaque output
            }
        }
        out
    } else {
        tight
    };

    // --- Write PNG ---
    let file = File::create(&out_path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(&composited)?;
    drop(png_writer);

    let file_size = std::fs::metadata(&out_path)?.len();
    println!("wrote {} ({}x{}, {} bytes)", out_path, width, height, file_size);
    if bg_alpha < 255 {
        println!("composited over checkerboard (bg alpha={})", bg_alpha);
    }

    Ok(())
}
