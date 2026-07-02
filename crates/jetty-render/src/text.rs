use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use jetty_core::GridSnapshot;
use std::hash::{Hash, Hasher};
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

/// How a grid cell's char must be drawn relative to the PRIMARY terminal font.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CellRoute {
    /// The primary font covers the char at a single cell width — lay it out inline
    /// in the main grid run.
    Inline,
    /// The primary font either lacks the glyph (tofu box under `Shaping::Basic`), or
    /// renders it double-width (a CJK glyph advances ~2 cells and would shift every
    /// following column of the row if laid out inline). Either way, blank the cell in
    /// the main run and overdraw the real glyph from its own buffer at the exact cell
    /// origin, keeping the grid aligned regardless of the glyph's advance.
    Overdraw,
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
    /// Per-char routing cache for the PRIMARY terminal font (`font_family`): does the
    /// char lay out inline, or must it be blanked and overdrawn (missing glyph — e.g.
    /// Claude Code's `⏵⏵` U+23F5 — OR a double-width CJK glyph)? Probed lazily on the
    /// hot path (only non-ASCII, on miss) and read every frame. Cleared when
    /// `font_family` changes (routing is per-font).
    glyph_route: std::collections::HashMap<char, CellRoute>,
    /// Scratch buffer used only to probe glyph coverage/advance (shape one char,
    /// inspect the resulting glyph id and width). Reused across frames.
    coverage_buffer: Buffer,
    /// Shaped single-glyph buffers for the overdraw path, keyed by char so a char
    /// repeated across the grid shares one shaped buffer and an unchanged frame
    /// re-shapes nothing. Shaped with `Shaping::Advanced` so cosmic-text's font
    /// fallback supplies a glyph the primary font lacks (or the primary font's own
    /// double-width glyph). Cleared on `set_font_family`/`set_font_size` (glyphs are
    /// per family + size).
    fallback_glyphs: std::collections::HashMap<char, Buffer>,
    /// Per-frame scratch: `(pixel_x, pixel_y, char, rgb)` for each cell drawn via the
    /// overdraw path (missing glyph or double-width) and overdrawn from `fallback_glyphs`.
    fallback_cells_scratch: Vec<(f32, f32, char, [u8; 3])>,
    /// Monotonic counter bumped whenever a change invalidates the grid buffer's shaped
    /// content (font family, font size, resize). Folded into `last_grid_hash` so such
    /// a change always forces a re-shape even when the grid text/colors are unchanged.
    shape_gen: u64,
    /// Content fingerprint of the grid last uploaded via `set_rich_text` (folds
    /// `shape_gen`, surface dims, per-cell chars and colors). When the current frame's
    /// fingerprint matches, the grid is byte-identical, so `set_rich_text`/`set_size`
    /// are skipped and cosmic-text's cached per-line shaping is reused by `prepare`.
    last_grid_hash: Option<u64>,
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

        // Scratch buffer for glyph-coverage probing (see `covers`).
        let mut coverage_buffer = Buffer::new(&mut font_system, metrics);
        coverage_buffer.set_size(&mut font_system, None, None);

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
            glyph_route: std::collections::HashMap::new(),
            coverage_buffer,
            fallback_glyphs: std::collections::HashMap::new(),
            fallback_cells_scratch: Vec::new(),
            shape_gen: 0,
            last_grid_hash: None,
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
        // Routing is per-font: a glyph present/single-width in the old family may be
        // missing/double-width in the new one. Drop the caches so they re-probe, and
        // bump the shape generation so the grid re-shapes even if its text is unchanged.
        self.glyph_route.clear();
        self.fallback_glyphs.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
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
        // Cached fallback glyphs were shaped at the old size; drop them and force a
        // grid re-shape at the new metrics.
        self.fallback_glyphs.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
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
        // set_size cleared the height bound; force the next frame to re-bound the
        // layout height and re-shape the grid.
        self.shape_gen = self.shape_gen.wrapping_add(1);
        let _ = gpu; // size not used for wrapping; viewport is updated per-frame
    }

    /// How the primary terminal font must render `c` on the grid (see `CellRoute`).
    /// ASCII is always `Inline`. Other chars are probed once — shaped with the primary
    /// family under `Shaping::Basic` (no fallback) — and cached: a glyph id of 0
    /// (`.notdef`, the tofu box) means the font lacks the char, and an advance wider
    /// than ~1.5 cells means a double-width glyph that would shift the row if laid out
    /// inline. Both take the `Overdraw` route (blanked here, overdrawn at the exact
    /// cell origin so the real glyph shows, aligned, like Konsole/Qt).
    fn route(&mut self, c: char) -> CellRoute {
        if (c as u32) < 0x80 {
            return CellRoute::Inline;
        }
        if let Some(&v) = self.glyph_route.get(&c) {
            return v;
        }
        let fam = Arc::clone(&self.font_family);
        let cell_w = self.cell_w;
        let mut tmp = [0u8; 4];
        let s = c.encode_utf8(&mut tmp);
        let attrs = Attrs::new().family(Family::Name(&fam));
        self.coverage_buffer
            .set_text(&mut self.font_system, s, &attrs, Shaping::Basic, None);
        let route = self
            .coverage_buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .next()
            .map(|g| {
                if g.glyph_id == 0 || g.w > cell_w * 1.5 {
                    CellRoute::Overdraw
                } else {
                    CellRoute::Inline
                }
            })
            // No glyph laid out at all (e.g. zero-width/control) — leave it inline for
            // the main grid; don't try to overdraw.
            .unwrap_or(CellRoute::Inline);
        self.glyph_route.insert(c, route);
        route
    }

    /// Renders the terminal grid to an arbitrary TextureView (offscreen or on-screen).
    /// Does NOT acquire a surface frame and does NOT present — the caller controls that.
    ///
    /// When `clear` is true this pass clears the view to the theme background
    /// first (legacy self-contained behavior). When false it uses `LoadOp::Load`
    /// so it draws ON TOP of an already-painted background — used by callers that
    /// run a per-cell background quad pass (which owns the clear) before the text.
    #[allow(clippy::too_many_arguments)]
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
        caret_t: Option<f32>,
        caret_flash_color: [f32; 3],
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
        // Cells whose glyph the primary font lacks: blanked here, overdrawn below.
        let mut fallback_cells = std::mem::take(&mut self.fallback_cells_scratch);
        fallback_cells.clear();
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;

        // Content fingerprint for this frame (see `last_grid_hash`): folds the shape
        // generation, surface dims, every cell's char (via `text`, hashed below) and
        // fg color. An identical grid skips the whole grid re-shape further down.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.shape_gen.hash(&mut hasher);
        width.hash(&mut hasher);
        height.hash(&mut hasher);

        for row in 0..snapshot.rows {
            // Run-length coalesce consecutive same-fg cells into ONE span per run:
            // cosmic-text's set_rich_text allocates (and clones the family string)
            // per span, and a terminal row has only a handful of color changes.
            // Shaping is byte-identical under Shaping::Basic — color is not part of
            // shaping and every monospace glyph advances exactly one cell regardless.
            let mut run_start = text.len();
            let mut run_color: Option<Color> = None;
            for col in 0..snapshot.cols {
                let cell = snapshot.cell(row, col);
                cell.fg.hash(&mut hasher);
                // A glyph the primary font lacks (tofu box under Shaping::Basic, no
                // fallback) or renders double-width (a CJK glyph advances ~2 cells and
                // would shift the rest of the row) is blanked on the main grid so it
                // stays exactly one column wide, and recorded for an overdraw — the
                // real glyph is drawn on top, aligned. ASCII and already-blank cells
                // skip the (cached) probe entirely.
                let overdraw = cell.c != ' '
                    && cell.c != '\0'
                    && self.route(cell.c) == CellRoute::Overdraw;
                if overdraw {
                    fallback_cells.push((
                        col as f32 * cell_w,
                        row as f32 * cell_h,
                        cell.c,
                        cell.fg,
                    ));
                }
                let color = Color::rgb(cell.fg[0], cell.fg[1], cell.fg[2]);
                if run_color != Some(color) {
                    if let Some(pc) = run_color {
                        cell_ranges.push((run_start, text.len(), pc));
                    }
                    run_start = text.len();
                    run_color = Some(color);
                }
                text.push(if overdraw { ' ' } else { cell.c });
            }
            // Flush the row's final run, then include the newline as its own span:
            // set_rich_text builds the text FROM the spans, so without the '\n' the
            // line breaks were dropped and the whole grid collapsed onto one line.
            if let Some(pc) = run_color {
                cell_ranges.push((run_start, text.len(), pc));
            }
            let nl_start = text.len();
            text.push('\n');
            cell_ranges.push((nl_start, text.len(), Color::rgb(220, 220, 220)));
        }

        // Finish the fingerprint with the chars, then decide whether the grid buffer
        // can be reused as-is (skip the re-shape) this frame.
        text.hash(&mut hasher);
        let grid_hash = hasher.finish();
        let grid_unchanged = self.last_grid_hash == Some(grid_hash);
        self.last_grid_hash = Some(grid_hash);

        // Clone the Arc (a refcount bump, not a string copy) so the family name
        // can be borrowed by every span without re-borrowing self.
        let family_name = Arc::clone(&self.font_family);

        // Skip the grid re-shape entirely when nothing that affects it changed —
        // caret-flash/CRT/scrollbar-only frames redraw identical grid text, and
        // cosmic-text's per-line shape cache in `self.buffer` is still valid. Only
        // the (expensive) set_size + set_rich_text are gated; the cursor, fallback
        // overdraws and prepare/render below all still run every frame.
        if !grid_unchanged {
            // Bound the layout height to the surface so cosmic-text lays out ALL
            // rows. With height = None it shapes only the first visible line, which
            // made every row after the first disappear.
            self.buffer
                .set_size(&mut self.font_system, None, Some(height as f32));

            let default_attrs = Attrs::new().family(Family::Name(&family_name));
            // Shaping::Basic avoids kerning/ligatures so every glyph lands exactly
            // one cell-width apart — essential for a terminal grid.
            //
            // Pass the coalesced spans — (&str slice of `text`, Attrs) — as a LAZY
            // iterator straight into set_rich_text. glyphon takes `IntoIterator`, so
            // there is no need to collect into a Vec first. The iterator yields the
            // spans in order, so shaping is byte-identical.
            self.buffer.set_rich_text(
                &mut self.font_system,
                cell_ranges.iter().map(|(s, e, color)| {
                    (
                        &text[*s..*e],
                        Attrs::new().family(Family::Name(&family_name)).color(*color),
                    )
                }),
                &default_attrs,
                Shaping::Basic,
                None,
            );
        }

        // The spans iterator is consumed and its borrows on `text`/`cell_ranges`/
        // `family_name` are released; return the scratch buffers to self for reuse
        // next frame.
        drop(family_name);
        self.text_scratch = text;
        self.cell_ranges_scratch = cell_ranges;

        // Overdraw glyph prep: shape each DISTINCT char once into a cached per-char
        // buffer with Shaping::Advanced, so cosmic-text either falls back to a font
        // that HAS the glyph or uses the primary font's own double-width glyph. Cached
        // across frames (cleared on family/size change), so a char repeated across the
        // grid — e.g. full-screen CJK — shapes only once and an unchanged frame shapes
        // nothing. Each is drawn at its exact cell origin in the SAME prepare() below,
        // so it shifts no neighbor. Usually empty, so this whole block is skipped.
        if !fallback_cells.is_empty() {
            let fam = Arc::clone(&self.font_family);
            let metrics = self.metrics;
            for (_x, _y, c, _rgb) in fallback_cells.iter() {
                if !self.fallback_glyphs.contains_key(c) {
                    let mut buf = Buffer::new(&mut self.font_system, metrics);
                    buf.set_size(&mut self.font_system, None, None);
                    let mut tmp = [0u8; 4];
                    let s = c.encode_utf8(&mut tmp);
                    let attrs = Attrs::new().family(Family::Name(&fam));
                    buf.set_text(&mut self.font_system, s, &attrs, Shaping::Advanced, None);
                    self.fallback_glyphs.insert(*c, buf);
                }
            }
        }

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
            // Caret flash+pulse: modulate color and scale during the animation burst.
            // When caret_t is None this branch is skipped and rendering is unchanged.
            let (cursor_color, cursor_scale, cursor_left, cursor_top) =
                if let Some(t) = caret_t {
                    // ease-out quadratic: fast rise, slow finish
                    let e = 1.0 - (1.0 - t) * (1.0 - t);
                    // bump = 4·e·(1−e): rises to 1 at e=0.5, returns to 0 at e=1.
                    // Both color and scale use the same bump so the color returns
                    // to cursor_rgb by t=1 (no snap at the end of the burst).
                    let bump = 4.0 * e * (1.0 - e);
                    // Color: lerp cursor_rgb → caret_flash_color by bump
                    let [fr, fg, fb] = caret_flash_color;
                    let lerp_ch = |base: u8, target: f32, frac: f32| -> u8 {
                        let b = base as f32 / 255.0;
                        ((b + (target - b) * frac) * 255.0).round().clamp(0.0, 255.0) as u8
                    };
                    let r = lerp_ch(cr, fr, bump);
                    let g = lerp_ch(cg, fg, bump);
                    let b = lerp_ch(cb, fb, bump);
                    // Scale: peaks at bump=1 (~1.15×), returns to 1 at bump=0 (t=1).
                    // Quantize the scale to discrete steps: the cursor glyph's atlas
                    // CacheKey folds font_size*scale, so a continuously-varying scale
                    // would mint a brand-new (permanently-cached) atlas entry every
                    // animation frame. Bucketing bounds it to a handful of keys.
                    let scale = 1.0 + 0.15 * ((bump * 8.0).round() / 8.0);
                    // Keep glyph centered on its cell by offsetting origin inward
                    // by half of the extra width/height the scaling adds.
                    let left = snapshot.cursor_col as f32 * self.cell_w
                        - (scale - 1.0) * self.cell_w * 0.5;
                    let top = snapshot.cursor_row as f32 * self.cell_h + top_offset
                        - (scale - 1.0) * self.cell_h * 0.5;
                    (Color::rgb(r, g, b), scale, left, top)
                } else {
                    // No animation — exact original behavior (byte-identical path).
                    (
                        Color::rgb(cr, cg, cb),
                        1.0_f32,
                        snapshot.cursor_col as f32 * self.cell_w,
                        snapshot.cursor_row as f32 * self.cell_h + top_offset,
                    )
                };
            areas.push(TextArea {
                buffer: &self.cursor_buffer,
                left: cursor_left,
                top: cursor_top,
                scale: cursor_scale,
                bounds: win_bounds,
                // Color::rgba is not available in this glyphon version; use rgb.
                default_color: cursor_color,
                custom_glyphs: &[],
            });
        }

        // Fallback glyphs: drawn ON TOP of the blanked cells, at the exact cell
        // origin, in this same prepare() — so they never shift a neighbor.
        for (x, y, c, rgb) in fallback_cells.iter() {
            if let Some(buffer) = self.fallback_glyphs.get(c) {
                areas.push(TextArea {
                    buffer,
                    left: *x,
                    top: *y + top_offset,
                    scale: 1.0,
                    bounds: win_bounds,
                    default_color: Color::rgb(rgb[0], rgb[1], rgb[2]),
                    custom_glyphs: &[],
                });
            }
        }

        // Prepare the atlas. If it reports AtlasFull, unpin every glyph (trim) so LRU
        // eviction can reclaim space, then retry once — without this a long session
        // eventually wedges with permanently-blank text (the atlas is also trimmed at
        // the end of every frame below, which is what keeps eviction working at all).
        let mut prepared = self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas.iter().cloned(),
            &mut self.swash,
        );
        if prepared == Err(PrepareError::AtlasFull) {
            self.atlas.trim();
            prepared = self.renderer.prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas.iter().cloned(),
                &mut self.swash,
            );
        }
        // areas is consumed; return the scratch Vec for reuse next frame.
        self.fallback_cells_scratch = fallback_cells;
        prepared?;

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
        // Unpin this frame's glyphs so the NEXT prepare can LRU-evict stale ones.
        // glyphon pins every rendered glyph in `glyphs_in_use` and only trim() clears
        // it; without this per-frame trim the atlas grows unbounded until AtlasFull.
        self.atlas.trim();
        Ok(())
    }

    /// Renders arbitrary text labels at pixel positions as a SEPARATE pass with
    /// `LoadOp::Load`, so they draw ON TOP of whatever is already in `view`
    /// (e.g., panel quads drawn by QuadLayer).
    ///
    /// `labels` is a slice of `(text, x, y, rgb_color)` tuples.
    /// Returns `Ok(())` immediately when `labels` is empty.
    #[allow(clippy::too_many_arguments)]
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
        // Optional Y-clip range [top, bottom] in physical pixels applied to
        // ALL labels in this call via TextArea.bounds. None means full
        // window (the default). Used for the Effects-tab scrolled content so
        // labels that have scrolled above/below the content viewport are clipped
        // by glyphon before they ever reach the GPU.
        clip_y: Option<(i32, i32)>,
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

        let (clip_top, clip_bottom) = clip_y.unwrap_or((0, height as i32));
        let win_bounds = TextBounds {
            left: 0,
            top: clip_top,
            right: width as i32,
            bottom: clip_bottom,
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
        // Unpin this frame's glyphs so the next prepare can LRU-evict (see render_to).
        self.atlas.trim();
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
        self.render_overlays_inner(device, queue, view, width, height, labels, false, None)
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
        self.render_overlays_inner(device, queue, view, width, height, labels, true, None)
    }

    /// Render NON-TITLE chrome labels clipped to `[clip_top..clip_bottom]`
    /// (physical pixels). Labels whose glyphs fall entirely outside this Y range
    /// are suppressed by the glyphon `TextArea.bounds` mechanism — no GPU work is
    /// wasted on off-screen text. Used for the Effects-tab scrolled content so
    /// labels that scroll above/below the content viewport are clipped.
    #[allow(clippy::too_many_arguments)]
    pub fn render_overlays_clipped(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        labels: &[(String, f32, f32, [u8; 3])],
        clip_top: i32,
        clip_bottom: i32,
    ) -> Result<(), PrepareError> {
        self.render_overlays_inner(
            device, queue, view, width, height, labels, false,
            Some((clip_top, clip_bottom)),
        )
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
        self.render_to(&gpu.device, &gpu.queue, &view, gpu.config.width, gpu.config.height, snapshot, true, 0.0, None, [0.0, 0.0, 0.0])?;
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
