use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use jetty_core::GridSnapshot;
use wgpu::MultisampleState;

/// The terminal font. Matches the user's Konsole profile: MesloLGS NF — a Nerd
/// Font, so the zsh prompt's powerline/icon glyphs render correctly.
const FONT_FAMILY: &str = "MesloLGS NF";

pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    buffer: Buffer,
    cursor_buffer: Buffer,
    // Retained for future use (e.g., rescaling on DPI change in Task 7+).
    #[allow(dead_code)]
    metrics: Metrics,
    cell_w: f32,
    cell_h: f32,
    /// Growable pool of glyphon Buffers reused across frames for overlay labels.
    overlay_buffers: Vec<Buffer>,
}

impl TextLayer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, font_size: f32) -> Self {
        let mut font_system = FontSystem::new();
        // Insurance: make sure user-installed fonts (e.g. ~/.local/share/fonts,
        // where MesloLGS NF lives) are in the database, not only the fontconfig
        // defaults that FontSystem::new() scans.
        if let Ok(home) = std::env::var("HOME") {
            font_system
                .db_mut()
                .load_fonts_dir(format!("{home}/.local/share/fonts"));
        }
        let swash = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, MultisampleState::default(), None);

        let line_height = (font_size * 1.3).ceil();
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        // None width disables line wrapping so columns stay on the monospace grid.
        buffer.set_size(&mut font_system, None, None);

        // Cursor buffer: a single full-block glyph used to draw the block cursor.
        let mut cursor_buffer = Buffer::new(&mut font_system, metrics);
        cursor_buffer.set_size(&mut font_system, None, None);
        let cursor_attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
        cursor_buffer.set_text(
            &mut font_system,
            "\u{2588}",
            &cursor_attrs,
            Shaping::Basic,
            None,
        );

        // Measure a monospace cell by shaping a single 'M'.
        let cell_w = measure_advance(&mut font_system, metrics);
        let cell_h = line_height;

        Self {
            font_system,
            swash,
            atlas,
            viewport,
            renderer,
            buffer,
            cursor_buffer,
            metrics,
            cell_w,
            cell_h,
            overlay_buffers: Vec::new(),
        }
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    pub fn resize(&mut self, gpu: &GpuContext) {
        // None width keeps wrapping disabled after resize.
        self.buffer.set_size(&mut self.font_system, None, None);
        let _ = gpu; // size not used for wrapping; viewport is updated per-frame
    }

    /// Renders the terminal grid to an arbitrary TextureView (offscreen or on-screen).
    /// Does NOT acquire a surface frame and does NOT present — the caller controls that.
    pub fn render_to(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        snapshot: &GridSnapshot,
    ) -> Result<(), PrepareError> {
        // Build per-cell color spans: one (&str slice, Attrs) pair per cell.
        // We build a single String containing all text, then collect borrowed slices from it.
        let mut text = String::new();
        // Store (byte_start, byte_end, Color) for each cell so we can borrow slices after.
        let mut cell_ranges: Vec<(usize, usize, Color)> = Vec::new();

        for row in 0..snapshot.rows {
            for col in 0..snapshot.cols {
                let cell = snapshot.cell(row, col);
                let start = text.len();
                text.push(cell.c);
                let end = text.len();
                cell_ranges.push((
                    start,
                    end,
                    Color::rgb(cell.fg[0], cell.fg[1], cell.fg[2]),
                ));
            }
            // Include the newline as its own span: set_rich_text builds the text
            // FROM the spans, so without this the line breaks were dropped and the
            // whole grid collapsed onto one very long line.
            let nl_start = text.len();
            text.push('\n');
            let nl_end = text.len();
            cell_ranges.push((nl_start, nl_end, Color::rgb(220, 220, 220)));
        }

        // Build the spans iterator: (&str, Attrs) tuples, borrowing slices from `text`.
        // We collect into a Vec to satisfy the borrow checker (spans borrow `text`).
        let spans: Vec<(&str, Attrs)> = cell_ranges
            .iter()
            .map(|(s, e, color)| {
                (
                    &text[*s..*e],
                    Attrs::new().family(Family::Name(FONT_FAMILY)).color(*color),
                )
            })
            .collect();

        // Bound the layout height to the surface so cosmic-text lays out ALL
        // rows. With height = None it shapes only the first visible line, which
        // made every row after the first disappear.
        self.buffer
            .set_size(&mut self.font_system, None, Some(height as f32));

        let default_attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
        // Shaping::Basic avoids kerning/ligatures so every glyph lands exactly
        // one cell-width apart — essential for a terminal grid.
        self.buffer.set_rich_text(
            &mut self.font_system,
            spans,
            &default_attrs,
            Shaping::Basic,
            None,
        );

        self.viewport.update(queue, Resolution { width, height });

        let win_bounds = TextBounds {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };

        let text_area = TextArea {
            buffer: &self.buffer,
            left: 0.0,
            top: 0.0,
            scale: 1.0,
            bounds: win_bounds,
            default_color: Color::rgb(220, 220, 220),
            custom_glyphs: &[],
        };

        // Build a Vec of TextAreas; cursor and scrollbar are pushed when applicable.
        let mut areas: Vec<TextArea> = vec![text_area];

        // Block cursor area when the cursor is within bounds.
        let cursor_in_bounds = snapshot.cursor_row < snapshot.rows
            && snapshot.cursor_col < snapshot.cols;
        if cursor_in_bounds {
            let [cr, cg, cb] = snapshot.cursor_rgb;
            areas.push(TextArea {
                buffer: &self.cursor_buffer,
                left: snapshot.cursor_col as f32 * self.cell_w,
                top: snapshot.cursor_row as f32 * self.cell_h,
                scale: 1.0,
                bounds: win_bounds,
                // Color::rgba is not available in this glyphon version; use rgb.
                default_color: Color::rgb(cr, cg, cb),
                custom_glyphs: &[],
            });
        }

        self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )?;

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("text") });
        {
            // Build the clear color from the snapshot's theme bg.
        // We always premultiply by alpha so the value is correct for
        // PreMultiplied alpha_mode surfaces and harmless for Opaque ones.
        let [br, bg_, bb, ba] = snapshot.bg_rgba;
        let a = ba as f64 / 255.0;
        // The surface is sRGB, so the clear value must be LINEAR. Convert each
        // sRGB component to linear, then premultiply by alpha (for PreMultiplied
        // alpha_mode). Without this, dark theme backgrounds render washed-out.
        fn srgb_to_linear(c: u8) -> f64 {
            let s = c as f64 / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        let clear_color = wgpu::Color {
            r: srgb_to_linear(br) * a,
            g: srgb_to_linear(bg_) * a,
            b: srgb_to_linear(bb) * a,
            a,
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Err(e) = self.renderer.render(&self.atlas, &self.viewport, &mut pass) {
                eprintln!("jetty: text render error: {e:?}");
            }
        }
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    /// Renders arbitrary text labels at pixel positions as a SEPARATE pass with
    /// `LoadOp::Load`, so they draw ON TOP of whatever is already in `view`
    /// (e.g., panel quads drawn by QuadLayer).
    ///
    /// `labels` is a slice of `(text, x, y, rgb_color)` tuples.
    /// Returns `Ok(())` immediately when `labels` is empty.
    pub fn render_overlays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        labels: &[(String, f32, f32, [u8; 3])],
    ) -> Result<(), PrepareError> {
        if labels.is_empty() {
            return Ok(());
        }

        // Ensure we have enough buffers in the pool.
        while self.overlay_buffers.len() < labels.len() {
            let mut buf = Buffer::new(&mut self.font_system, self.metrics);
            buf.set_size(&mut self.font_system, None, Some(height as f32));
            self.overlay_buffers.push(buf);
        }

        let win_bounds = TextBounds {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };

        // First pass: set text content (requires &mut font_system, so can't borrow bufs as &T simultaneously).
        for (i, (text, _x, _y, _rgb)) in labels.iter().enumerate() {
            let buf = &mut self.overlay_buffers[i];
            buf.set_size(&mut self.font_system, None, Some(height as f32));
            let attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
            buf.set_text(&mut self.font_system, text, &attrs, Shaping::Basic, None);
        }

        // Second pass: build TextAreas with shared refs (no mutation of font_system needed).
        let mut areas: Vec<TextArea> = Vec::with_capacity(labels.len());
        for (i, (_text, x, y, rgb)) in labels.iter().enumerate() {
            areas.push(TextArea {
                buffer: &self.overlay_buffers[i],
                left: *x,
                top: *y,
                scale: 1.0,
                bounds: win_bounds,
                default_color: Color::rgb(rgb[0], rgb[1], rgb[2]),
                custom_glyphs: &[],
            });
        }

        self.viewport.update(queue, Resolution { width, height });

        self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )?;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("overlay-text"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay-text-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Err(e) = self.renderer.render(&self.atlas, &self.viewport, &mut pass) {
                eprintln!("jetty: overlay text render error: {e:?}");
            }
        }
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    /// Clears the frame to the terminal background color and renders the grid text.
    ///
    /// Returns `Err(PrepareError)` if glyphon cannot prepare the atlas
    /// (e.g., atlas full). Frame-acquisition failures (surface lost / occluded)
    /// are handled internally by `GpuContext::acquire_frame` and silently skip
    /// the frame — `wgpu::SurfaceError` no longer exists in wgpu 29.
    pub fn render(
        &mut self,
        gpu: &mut GpuContext,
        snapshot: &GridSnapshot,
    ) -> Result<(), PrepareError> {
        let Some((frame, view)) = gpu.acquire_frame() else {
            return Ok(());
        };
        self.render_to(&gpu.device, &gpu.queue, &view, gpu.config.width, gpu.config.height, snapshot)?;
        frame.present();
        Ok(())
    }
}

fn measure_advance(font_system: &mut FontSystem, metrics: Metrics) -> f32 {
    let mut b = Buffer::new(font_system, metrics);
    let attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
    // Shaping::Basic avoids kerning so the advance width matches the terminal grid.
    b.set_text(font_system, "M", &attrs, Shaping::Basic, None);
    b.set_size(font_system, None, Some(metrics.line_height));
    b.layout_runs()
        .next()
        .and_then(|run| run.glyphs.iter().map(|g| g.w).next())
        .unwrap_or(metrics.font_size * 0.6)
}
