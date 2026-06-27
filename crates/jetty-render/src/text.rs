use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use jetty_core::GridSnapshot;
use std::sync::Arc;
use wgpu::MultisampleState;

/// The default terminal font. Matches the user's Konsole profile: MesloLGS NF
/// — a Nerd Font, so the zsh prompt's powerline/icon glyphs render correctly.
const FONT_FAMILY_DEFAULT: &str = "MesloLGS NF";

/// Which family the chrome-overlay pass (`render_overlays*`) shapes its labels
/// in. Distinct from the TERMINAL grid font (`font_family`): chrome — tab
/// titles, the status bar, the menu, the panel, help/confirm/welcome — renders
/// in the user's chosen UI font, which defaults to the platform proportional
/// sans (`Sans`). `Named` selects an installed family by name.
#[derive(Debug, Clone, PartialEq)]
pub enum ChromeFamily {
    /// Platform proportional sans-serif (`Family::SansSerif`). The default,
    /// matching today's elegant sans tab titles.
    Sans,
    /// A specific installed family, chosen by the user from the UI-font picker.
    Named(String),
}

impl ChromeFamily {
    /// Build the glyphon `Family` to shape a chrome run with.
    ///
    /// `Named(name)` → that family for EVERY surface (the user opted into a UI
    /// font, so all chrome unifies onto it).
    ///
    /// `Sans` is the DEFAULT and must render byte-identical to the pre-feature
    /// chrome, which used two families: tab TITLES in the platform proportional
    /// sans, and everything else (menu, status/perf bar, help/confirm/welcome,
    /// window-control + close glyphs) in the MONOSPACE chrome font. The latter is
    /// load-bearing: the mono font is a Nerd Font that carries the symbol glyphs
    /// (⇧ ⌃ ⚡ ⚙ ✕ …) that the platform sans (e.g. Noto Sans) lacks — rendering
    /// those in plain sans would show tofu boxes. So at the default we route
    /// titles to `SansSerif` (`is_title`) and everything else to the mono
    /// `mono_fallback` family, exactly as before.
    fn as_family<'a>(&'a self, is_title: bool, mono_fallback: &'a str) -> Family<'a> {
        match self {
            ChromeFamily::Named(name) => Family::Name(name),
            ChromeFamily::Sans if is_title => Family::SansSerif,
            ChromeFamily::Sans => Family::Name(mono_fallback),
        }
    }
}

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
    /// `Arc<str>` so per-frame span building can share it without cloning the
    /// string (the family name is captured by every cell's `Attrs`).
    font_family: Arc<str>,
    /// Family the CHROME overlay pass renders in (tab titles, status bar, menu,
    /// panel, help/confirm/welcome). Independent of `font_family` (the terminal
    /// grid font). Defaults to `Sans` so the default chrome look is unchanged
    /// (tab titles already render in `Family::SansSerif`). Set via
    /// `set_ui_family` — no FontSystem rebuild.
    ui_family: ChromeFamily,
    /// Per-frame scratch buffers, reused across `render_to` calls to avoid
    /// reallocating ~rows*cols heap items every frame (speed-first hot path).
    /// Taken out via `mem::take` during the frame and put back at the end.
    text_scratch: String,
    cell_ranges_scratch: Vec<(usize, usize, Color)>,
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
            font_family: Arc::from(family),
            // Chrome defaults to the platform proportional sans, matching the
            // pre-feature look (sans tab titles); the app overrides this from
            // the persisted `ui_font_family` after construction.
            ui_family: ChromeFamily::Sans,
            text_scratch: String::new(),
            cell_ranges_scratch: Vec::new(),
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

    /// Returns the sorted, deduplicated list of PROPORTIONAL (non-monospaced)
    /// font family names known to the font system — the candidates for the UI
    /// (chrome) font picker. Mirrors `monospace_families` but inverts the
    /// `monospaced` flag, so the list offers the user real sans/serif UI faces
    /// (the synthetic "System Sans (default)" row in the panel always provides
    /// the escape hatch back to the platform sans).
    pub fn proportional_families(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut families: Vec<String> = Vec::new();

        for face in self.font_system.db().faces() {
            if !face.monospaced {
                // The first family entry is always English US.
                if let Some((name, _)) = face.families.first() {
                    if seen.insert(name.clone()) {
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
        self.font_family = Arc::from(name);
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

    /// Change the CHROME (UI-overlay) font family at runtime. `None` or an empty
    /// name selects the platform proportional sans (`ChromeFamily::Sans`);
    /// otherwise the named installed family. Re-measures `cell_w` for the new
    /// family so chrome width math (panel right-align, perf-HUD placement) stays
    /// correct — but REUSES the existing `FontSystem` (its db already holds every
    /// installed family from `build_font_system`), so this never pays the ~20ms
    /// fontconfig rescan. Only affects `render_overlays*`; the terminal grid
    /// (which uses `font_family`) is untouched. Caller should `request_redraw`.
    pub fn set_ui_family(&mut self, name: Option<&str>) {
        self.ui_family = match name {
            Some(n) if !n.is_empty() => ChromeFamily::Named(n.to_string()),
            _ => ChromeFamily::Sans,
        };
        // Re-measure the chrome cell advance with the new family so chrome_char_w
        // (and every width reservation derived from it) tracks the UI font.
        self.cell_w = self.measure_chrome_advance();
    }

    /// Measure the advance (`cell_w`) for the CURRENT chrome family at the current
    /// metrics. For the default `Sans` we deliberately measure the MONOSPACE
    /// `font_family` advance (today's ~9.6px chrome cell), NOT the true sans
    /// advance — so a default config's `chrome_char_w` (which drives panel
    /// right-align, perf-HUD placement, tab-bar reservations) is byte-for-byte
    /// what it was before this feature, keeping the default look unchanged. A
    /// user-chosen `Named` UI family measures that family's own advance.
    fn measure_chrome_advance(&mut self) -> f32 {
        match &self.ui_family {
            ChromeFamily::Sans => {
                measure_advance_family(&mut self.font_system, self.metrics, &Arc::clone(&self.font_family))
            }
            ChromeFamily::Named(n) => {
                measure_advance_family(&mut self.font_system, self.metrics, &n.clone())
            }
        }
    }

    /// Change the font size in-place, REUSING the existing `FontSystem` (and its
    /// already-loaded fontconfig + ~/.local/share/fonts database) instead of
    /// rebuilding it. Rebuilding the FontSystem costs ~20ms of fontconfig rescan;
    /// font-size changes (Ctrl+/Ctrl-, DPI changes) must not pay that on the main
    /// thread per keypress. Re-derives metrics, the layout/cursor buffers, and the
    /// cell measurements. The caller must `reflow()` + `request_redraw()` after.
    pub fn set_font_size(&mut self, font_size: f32) {
        let line_height = (font_size * 1.3).ceil();
        self.metrics = Metrics::new(font_size, line_height);
        self.buffer.set_metrics(&mut self.font_system, self.metrics);
        self.buffer.set_size(&mut self.font_system, None, None);
        self.cursor_buffer.set_metrics(&mut self.font_system, self.metrics);
        self.cursor_buffer.set_size(&mut self.font_system, None, None);
        let cursor_attrs = Attrs::new().family(Family::Name(&self.font_family));
        self.cursor_buffer.set_text(
            &mut self.font_system,
            "\u{2588}",
            &cursor_attrs,
            Shaping::Basic,
            None,
        );
        // Re-measure the cell at the new size. For the terminal layer (`ui_family`
        // == Sans, never set away from default) this measures the monospace
        // `font_family` — the grid cell. For the chrome layer it measures the
        // active chrome family, so a UI-font SIZE change re-derives chrome_char_w.
        self.cell_w = self.measure_chrome_advance();
        self.cell_h = line_height;
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
        // Reuse the scratch buffers (taken out so the later &mut self.font_system
        // borrow doesn't conflict with the &self borrows in the spans) to avoid
        // reallocating ~rows*cols heap items per frame.
        let mut text = std::mem::take(&mut self.text_scratch);
        text.clear();
        // Store (byte_start, byte_end, Color) for each cell so we can borrow slices after.
        let mut cell_ranges = std::mem::take(&mut self.cell_ranges_scratch);
        cell_ranges.clear();

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
        // Clone the Arc (a refcount bump, not a string copy) so the family name
        // can be borrowed by every span without re-borrowing self.
        let family_name = Arc::clone(&self.font_family);
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

        // `spans` is consumed and its borrows on `text`/`cell_ranges`/`family_name`
        // are released; return the scratch buffers to self for reuse next frame.
        drop(family_name);
        self.text_scratch = text;
        self.cell_ranges_scratch = cell_ranges;

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
                // This text-owned clear is not the live macOS surface clear (the
                // app loads over the quad pass's clear at default_bg_clear); keep
                // the historical premultiplied value for the bench/convenience paths.
                wgpu::LoadOp::Clear(crate::quad::default_bg_clear(snapshot, true))
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
    fn render_overlays_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        labels: &[(String, f32, f32, [u8; 3])],
        // True for tab TITLES (the only chrome that defaulted to SansSerif before
        // this feature). Steers the `Sans` DEFAULT only: titles → SansSerif, all
        // other chrome → the mono Nerd Font (preserving its symbol glyphs). When a
        // `Named` UI family is set, every surface uses it regardless of this flag.
        is_title: bool,
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

        // First pass: set text content (requires &mut font_system, so can't borrow
        // bufs as &T simultaneously). Clone the chrome family + mono fallback +
        // metrics out of self so the `Family::Name` borrow doesn't conflict with
        // the &mut font_system.
        let ui_family = self.ui_family.clone();
        let mono_fallback = self.font_family.clone();
        let metrics = self.metrics;
        for (i, (text, _x, _y, _rgb)) in labels.iter().enumerate() {
            let buf = &mut self.overlay_buffers[i];
            // POOLED buffers are reused across frames and retain whatever metrics
            // they were created with. After a UI-font SIZE change the pool still
            // holds buffers at the OLD size, so the first frames would render
            // stale-size glyphs. Push the current metrics into every buffer each
            // frame so a size change takes effect immediately (one-liner, easy to
            // miss). Cheap: set_metrics is a no-op when the metrics are unchanged.
            buf.set_metrics(&mut self.font_system, metrics);
            buf.set_size(&mut self.font_system, None, Some(height as f32));
            // A `Named` UI family unifies ALL chrome onto it; the `Sans` default
            // keeps today's split (titles → sans, rest → mono Nerd Font) so the
            // default look — including symbol glyphs — is byte-identical.
            let attrs = Attrs::new().family(ui_family.as_family(is_title, &mono_fallback));
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

    /// Render NON-TITLE chrome labels (menu, status/perf bar, panel, help,
    /// confirm, welcome, window controls). With a `Named` UI family they render in
    /// it; at the `Sans` default they render in the mono Nerd Font (preserving its
    /// symbol glyphs ⇧ ⌃ ⚡ ⚙ ✕ …), exactly as before this feature.
    pub fn render_overlays(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        labels: &[(String, f32, f32, [u8; 3])],
    ) -> Result<(), PrepareError> {
        self.render_overlays_inner(device, queue, view, width, height, labels, false)
    }

    /// Render tab TITLE labels. With a `Named` UI family they render in it (so the
    /// titles follow the user's chosen UI font like the rest of the chrome); at
    /// the `Sans` default they render in the platform proportional sans
    /// (`Family::SansSerif`) — the elegant sans titles, identical to before.
    pub fn render_overlays_sans(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        labels: &[(String, f32, f32, [u8; 3])],
    ) -> Result<(), PrepareError> {
        self.render_overlays_inner(device, queue, view, width, height, labels, true)
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
