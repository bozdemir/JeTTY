use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use jetty_core::GridSnapshot;
use wgpu::MultisampleState;

/// The default terminal font. Matches the user's Konsole profile: MesloLGS NF
/// — a Nerd Font, so the zsh prompt's powerline/icon glyphs render correctly.
const FONT_FAMILY_DEFAULT: &str = "MesloLGS NF";

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
    /// Current font family name (runtime-settable via `set_font_family`).
    font_family: String,
}

impl TextLayer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, font_size: f32) -> Self {
        Self::new_with_family(device, queue, format, font_size, FONT_FAMILY_DEFAULT)
    }

    /// Builds the cosmic-text `FontSystem` (scans fontconfig defaults + the
    /// user's ~/.local/share/fonts). This is GPU-independent and `Send`, so the
    /// app runs it on a worker thread overlapping the GPU device block — see
    /// `new_with_family_and_fonts`. Costs ~20ms (essentially all of text_init).
    pub fn build_font_system() -> FontSystem {
        let mut font_system = FontSystem::new();
        // Insurance: make sure user-installed fonts (e.g. ~/.local/share/fonts,
        // where MesloLGS NF lives) are in the database, not only the fontconfig
        // defaults that FontSystem::new() scans.
        if let Ok(home) = std::env::var("HOME") {
            font_system
                .db_mut()
                .load_fonts_dir(format!("{home}/.local/share/fonts"));
        }
        font_system
    }

    /// Like `new`, but allows specifying the initial font family. Builds the
    /// FontSystem synchronously; use `new_with_family_and_fonts` to supply a
    /// prebuilt (e.g. thread-overlapped) FontSystem.
    pub fn new_with_family(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_size: f32,
        family: &str,
    ) -> Self {
        Self::new_with_family_and_fonts(device, queue, format, font_size, family, Self::build_font_system())
    }

    /// Like `new_with_family`, but takes a prebuilt `FontSystem` so its ~20ms
    /// load can be overlapped with GPU device creation on a worker thread.
    pub fn new_with_family_and_fonts(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_size: f32,
        family: &str,
        font_system: FontSystem,
    ) -> Self {
        let mut font_system = font_system;
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
        let cursor_attrs = Attrs::new().family(Family::Name(family));
        cursor_buffer.set_text(
            &mut font_system,
            "\u{2588}",
            &cursor_attrs,
            Shaping::Basic,
            None,
        );

        // Measure a monospace cell by shaping a single 'M'.
        let cell_w = measure_advance_family(&mut font_system, metrics, family);
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
            font_family: family.to_string(),
        }
    }

    /// Returns the sorted, deduplicated list of monospaced font family names
    /// known to the font system. Uses `fontdb::FaceInfo::monospaced` to detect
    /// monospace faces; falls back to name-based matching when the flag is absent.
    pub fn monospace_families(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut families: Vec<String> = Vec::new();

        for face in self.font_system.db().faces() {
            if face.monospaced {
                // The first family entry is always English US.
                if let Some((name, _)) = face.families.first() {
                    if seen.insert(name.clone()) {
                        families.push(name.clone());
                    }
                }
            }
        }

        // Fallback: if nothing was found via the flag, collect by name patterns.
        if families.is_empty() {
            let keywords = ["Mono", "Code", "Consolas", "Menlo", "Meslo", "Term", "Fixed"];
            for face in self.font_system.db().faces() {
                if let Some((name, _)) = face.families.first() {
                    let matches = keywords.iter().any(|kw| name.contains(kw));
                    if matches && seen.insert(name.clone()) {
                        families.push(name.clone());
                    }
                }
            }
        }

        families.sort();
        families
    }

    /// Change the active font family at runtime. Updates `font_family`, remeasures
    /// the cell size, and resets the cursor buffer glyph with the new family.
    /// The caller must call `reflow()` and `request_redraw()` after this.
    pub fn set_font_family(&mut self, name: &str) {
        self.font_family = name.to_string();
        // Re-measure cell width with the new family.
        self.cell_w = measure_advance_family(&mut self.font_system, self.metrics, name);
        // Reset cursor buffer glyph so the block cursor uses the new family.
        let cursor_attrs = Attrs::new().family(Family::Name(&self.font_family));
        self.cursor_buffer.set_text(
            &mut self.font_system,
            "\u{2588}",
            &cursor_attrs,
            Shaping::Basic,
            None,
        );
    }

    /// Returns the currently active font family name.
    pub fn font_family(&self) -> &str {
        &self.font_family
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
    ///
    /// When `clear` is true this pass clears the view to the theme background
    /// first (legacy self-contained behavior). When false it uses `LoadOp::Load`
    /// so it draws ON TOP of an already-painted background — used by callers that
    /// run a per-cell background quad pass (which owns the clear) before the text.
    pub fn render_to(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        snapshot: &GridSnapshot,
        clear: bool,
        top_offset: f32,
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
        let family_name = self.font_family.clone();
        let spans: Vec<(&str, Attrs)> = cell_ranges
            .iter()
            .map(|(s, e, color)| {
                (
                    &text[*s..*e],
                    Attrs::new().family(Family::Name(&family_name)).color(*color),
                )
            })
            .collect();

        // Bound the layout height to the surface so cosmic-text lays out ALL
        // rows. With height = None it shapes only the first visible line, which
        // made every row after the first disappear.
        self.buffer
            .set_size(&mut self.font_system, None, Some(height as f32));

        let default_attrs = Attrs::new().family(Family::Name(&family_name));
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
            top: top_offset,
            scale: 1.0,
            bounds: win_bounds,
            default_color: Color::rgb(220, 220, 220),
            custom_glyphs: &[],
        };

        // Build a Vec of TextAreas; cursor and scrollbar are pushed when applicable.
        let mut areas: Vec<TextArea> = vec![text_area];

        // Block cursor area when the cursor is visible and within bounds.
        // Apps that hide the cursor (DECTCEM `\e[?25l`) clear `cursor_visible`.
        let cursor_in_bounds = snapshot.cursor_row < snapshot.rows
            && snapshot.cursor_col < snapshot.cols;
        if snapshot.cursor_visible && cursor_in_bounds {
            let [cr, cg, cb] = snapshot.cursor_rgb;
            areas.push(TextArea {
                buffer: &self.cursor_buffer,
                left: snapshot.cursor_col as f32 * self.cell_w,
                top: snapshot.cursor_row as f32 * self.cell_h + top_offset,
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
            // When clearing, build the clear color from the snapshot's theme bg.
            // Premultiplied by alpha so the value is correct for PreMultiplied
            // alpha_mode surfaces and harmless for Opaque ones. This matches the
            // per-cell background pass's `default_bg_clear`. When `clear` is false
            // the background was already painted by a prior quad pass, so we load.
            let load = if clear {
                wgpu::LoadOp::Clear(crate::quad::default_bg_clear(snapshot))
            } else {
                wgpu::LoadOp::Load
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
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
        let family_name = self.font_family.clone();
        for (i, (text, _x, _y, _rgb)) in labels.iter().enumerate() {
            let buf = &mut self.overlay_buffers[i];
            buf.set_size(&mut self.font_system, None, Some(height as f32));
            let attrs = Attrs::new().family(Family::Name(&family_name));
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
        // Self-contained path: this pass owns the frame clear.
        self.render_to(&gpu.device, &gpu.queue, &view, gpu.config.width, gpu.config.height, snapshot, true, 0.0)?;
        frame.present();
        Ok(())
    }
}

fn measure_advance_family(font_system: &mut FontSystem, metrics: Metrics, family: &str) -> f32 {
    let mut b = Buffer::new(font_system, metrics);
    let attrs = Attrs::new().family(Family::Name(family));
    // Shaping::Basic avoids kerning so the advance width matches the terminal grid.
    b.set_text(font_system, "M", &attrs, Shaping::Basic, None);
    b.set_size(font_system, None, Some(metrics.line_height));
    b.layout_runs()
        .next()
        .and_then(|run| run.glyphs.iter().map(|g| g.w).next())
        .unwrap_or(metrics.font_size * 0.6)
}
