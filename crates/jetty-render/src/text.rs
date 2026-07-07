use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
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

/// Upper bound on the number of distinct shaped fallback (overdraw) glyph
/// buffers kept cached. Sits well above any single frame's distinct-fallback
/// count (a maximized grid is a few thousand cells), so eviction only ever
/// trims glyphs from long-past frames, never currently-visible ones (F25).
const FALLBACK_GLYPH_CAP: usize = 4096;

/// Evict oldest entries from a FIFO-ordered cache down to `cap`, never removing
/// a key present in `visible` (chars drawn this frame). A visible key scanned
/// during eviction is rotated to the back (treated as most-recent) instead of
/// dropped. Pure + generic so it is unit-testable independent of cosmic-text
/// `Buffer`. (F25)
fn evict_fifo_cache<V>(
    map: &mut std::collections::HashMap<char, V>,
    order: &mut std::collections::VecDeque<char>,
    visible: &std::collections::HashSet<char>,
    cap: usize,
) {
    let mut scanned = 0usize;
    let cap_scan = order.len();
    while map.len() > cap && scanned < cap_scan {
        let Some(old) = order.pop_front() else { break };
        scanned += 1;
        if visible.contains(&old) {
            order.push_back(old);
        } else {
            map.remove(&old);
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
    /// `(byte_start, byte_end, fg color, shape_bits)` per coalesced run.
    /// `shape_bits` = `attrs & SHAPE_MASK` (BOLD|ITALIC) — the run breaks when it
    /// changes so each run carries a single weight/style. STRIKE / underline never
    /// break a run (they are drawn as quads, not shaped).
    cell_ranges_scratch: Vec<(usize, usize, Color, u8)>,
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
    /// Insertion order of `fallback_glyphs` keys, used to evict the oldest
    /// entries once the cache exceeds `FALLBACK_GLYPH_CAP` so a session scrolling
    /// through a large CJK/emoji corpus can't accumulate shaped buffers without
    /// bound (F25). Chars visible in the current frame are never evicted.
    fallback_order: std::collections::VecDeque<char>,
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
    /// Cached underline/strikethrough quads and the key they were built for:
    /// `(grid_decoration_key, cell_w bits, cell_h bits, y_offset bits)`. Rebuilt
    /// only when that changes, so a caret-flash / CRT / scrollbar-only animate
    /// frame (same grid) reuses them — decorations never rebuild per frame; only
    /// the CURSOR quads do (drawn app-side). Consumed via `decoration_rects()`.
    deco_rects: Vec<crate::quad::Rect>,
    deco_cache_key: Option<(u64, u32, u32, u32)>,
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

        // The cursor is drawn as a QuadLayer rect (see `quad::cursor_rects`), not a
        // text-atlas block glyph, so there is no cursor buffer to build here.

        // Scratch buffer for glyph-coverage probing (see `covers`).
        let mut coverage_buffer = Buffer::new(&mut font_system, metrics);
        coverage_buffer.set_size(&mut font_system, None, None);

        // Measure a monospace cell by shaping a single 'M'.
        let cell_w = measure_advance_family(&mut font_system, metrics, family);
        let cell_h = line_height;

        // Snap every grid glyph's advance to the cell width. cosmic-text rounds
        // each glyph's x_advance to the nearest `cell_w` (shape.rs), which keeps
        // a real Bold/Italic face — or any stray wide/fallback glyph — column-
        // aligned even when its natural advance differs from Regular. This is the
        // alignment guarantee that lets us render real bold/italic faces (v0.13
        // amendment). Set ONLY on the grid buffer; the chrome overlay buffers are
        // proportional (Shaping::Advanced) and never get this.
        buffer.set_monospace_width(&mut font_system, Some(cell_w));

        Self {
            font_system,
            swash,
            atlas,
            viewport,
            renderer,
            buffer,
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
            fallback_order: std::collections::VecDeque::new(),
            fallback_cells_scratch: Vec::new(),
            shape_gen: 0,
            last_grid_hash: None,
            deco_rects: Vec::new(),
            deco_cache_key: None,
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
        self.fallback_order.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
        // Re-measure cell width with the new family.
        self.cell_w = measure_advance_family(&mut self.font_system, self.metrics, name);
        // Re-snap the grid buffer's monospace advance to the new cell width so a
        // bold/italic run in the new family stays column-aligned.
        self.buffer.set_monospace_width(&mut self.font_system, Some(self.cell_w));
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
        // Re-metric the coverage probe buffer too (F6). `route()` shapes the probed
        // char in `coverage_buffer` and compares its advance against the CURRENT
        // `cell_w`; leaving the probe buffer at the construction-time size made a
        // wide (CJK) glyph misroute after a >~33% size/DPI change, permanently
        // shifting that row's columns. The verdict is cached in `glyph_route`, so
        // that must be cleared here as well or the misroute survives a size reset.
        self.coverage_buffer.set_metrics(&mut self.font_system, self.metrics);
        self.coverage_buffer.set_size(&mut self.font_system, None, None);
        self.glyph_route.clear();
        // Re-measure the cell at the new size. For the terminal layer (`ui_family`
        // == Sans, never set away from default) this measures the monospace
        // `font_family` — the grid cell. For the chrome layer it measures the
        // active chrome family, so a UI-font SIZE change re-derives chrome_char_w.
        self.cell_w = self.measure_chrome_advance();
        self.cell_h = line_height;
        // Re-snap the grid buffer's monospace advance to the new cell width. (On a
        // chrome layer `self.buffer` is unused — chrome renders via overlay_buffers
        // — so this only matters for the terminal grid layer, where it keeps
        // bold/italic aligned after a font-size / DPI change.)
        self.buffer.set_monospace_width(&mut self.font_system, Some(self.cell_w));
        // Cached fallback glyphs were shaped at the old size; drop them and force a
        // grid re-shape at the new metrics.
        self.fallback_glyphs.clear();
        self.fallback_order.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
    }

    /// Returns the currently active font family name.
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// The grid buffer's monospace snap width (`Some(cell_w)` once set). Exposed
    /// for the alignment self-test / inspection; the grid glyph advances are
    /// rounded to this so real bold/italic faces stay column-aligned.
    pub fn grid_monospace_width(&self) -> Option<f32> {
        self.buffer.monospace_width()
    }

    /// The cached underline/strikethrough quads for the last rendered frame, built
    /// at the `top_offset` passed to `render_to`. The caller appends these to its
    /// Pass-4 quad batch (they draw over the glyphs, under the cursor).
    pub fn decoration_rects(&self) -> &[crate::quad::Rect] {
        &self.deco_rects
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
        // Separate fingerprint for the underline/strike quads (folded in the SAME
        // per-cell loop, so no extra pass): captures decoration state that the
        // shape hash deliberately excludes, so an underline-only change still
        // rebuilds decorations without forcing a re-shape.
        let mut deco_hasher = std::collections::hash_map::DefaultHasher::new();

        for row in 0..snapshot.rows {
            // Run-length coalesce consecutive same-fg cells into ONE span per run:
            // cosmic-text's set_rich_text allocates (and clones the family string)
            // per span, and a terminal row has only a handful of color changes.
            // Shaping is byte-identical under Shaping::Basic — color is not part of
            // shaping and every monospace glyph advances exactly one cell regardless.
            let mut run_start = text.len();
            let mut run_key: Option<(Color, u8)> = None;
            for col in 0..snapshot.cols {
                let cell = snapshot.cell(row, col);
                cell.fg.hash(&mut hasher);
                // Fold ONLY the shaping-affecting bits (BOLD|ITALIC) into the grid
                // fingerprint: a color-identical bold toggle must re-shape (a
                // different face), while strike/underline/underline-color must NOT
                // (they are quads). SPEED: keeps underline changes off the re-shape.
                cell.shape_bits().hash(&mut hasher);
                crate::quad::fold_decoration(&mut deco_hasher, cell);
                // A glyph the primary font lacks (tofu box under Shaping::Basic, no
                // fallback) or renders double-width (a CJK glyph advances ~2 cells and
                // would shift the rest of the row) is blanked on the main grid so it
                // stays exactly one column wide, and recorded for an overdraw — the
                // real glyph is drawn on top, aligned. ASCII and already-blank cells
                // skip the (cached) probe entirely.
                // alacritty stores a literal '\t' in the cell at a tab stop (so
                // copies preserve tabs); control chars have no glyph, so render
                // them as blanks instead of routing them to the overdraw (tofu).
                let ch = if cell.c.is_control() { ' ' } else { cell.c };
                let overdraw = ch != ' ' && self.route(ch) == CellRoute::Overdraw;
                if overdraw {
                    fallback_cells.push((
                        col as f32 * cell_w,
                        row as f32 * cell_h,
                        ch,
                        cell.fg,
                    ));
                }
                let color = Color::rgb(cell.fg[0], cell.fg[1], cell.fg[2]);
                let key = (color, cell.shape_bits());
                if run_key != Some(key) {
                    if let Some((pc, pb)) = run_key {
                        cell_ranges.push((run_start, text.len(), pc, pb));
                    }
                    run_start = text.len();
                    run_key = Some(key);
                }
                text.push(if overdraw { ' ' } else { ch });
            }
            // Flush the row's final run, then include the newline as its own span:
            // set_rich_text builds the text FROM the spans, so without the '\n' the
            // line breaks were dropped and the whole grid collapsed onto one line.
            if let Some((pc, pb)) = run_key {
                cell_ranges.push((run_start, text.len(), pc, pb));
            }
            let nl_start = text.len();
            text.push('\n');
            cell_ranges.push((nl_start, text.len(), Color::rgb(220, 220, 220), 0));
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
                cell_ranges.iter().map(|(s, e, color, shape)| {
                    // BOLD -> real Bold face, ITALIC -> real Italic face, under
                    // Shaping::Basic. Monospace alignment is guaranteed by
                    // set_monospace_width(cell_w) (below), which snaps every glyph's
                    // advance onto the cell grid regardless of the matched face.
                    let weight = if shape & jetty_core::attr::BOLD != 0 {
                        Weight::BOLD
                    } else {
                        Weight::NORMAL
                    };
                    let style = if shape & jetty_core::attr::ITALIC != 0 {
                        Style::Italic
                    } else {
                        Style::Normal
                    };
                    (
                        &text[*s..*e],
                        Attrs::new()
                            .family(Family::Name(&family_name))
                            .color(*color)
                            .weight(weight)
                            .style(style),
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

        // Rebuild the cached underline/strike quads only when the decoration
        // content OR the cell metrics / grid offset changed. On a caret-flash /
        // CRT / scrollbar-only frame (same grid, same offset) this is a cheap key
        // compare and the previously-built rects are reused (SPEED: decorations
        // stay off the animate-only path; only the cursor quad rebuilds per frame).
        let deco_key = (
            deco_hasher.finish(),
            cell_w.to_bits(),
            cell_h.to_bits(),
            top_offset.to_bits(),
        );
        if self.deco_cache_key != Some(deco_key) {
            self.deco_rects.clear();
            crate::quad::text_decoration_rects(
                snapshot,
                cell_w,
                cell_h,
                top_offset,
                &mut self.deco_rects,
            );
            self.deco_cache_key = Some(deco_key);
        }

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
                    self.fallback_order.push_back(*c);
                }
            }
            // Evict the oldest cached buffers once the map exceeds the cap, so a
            // session scrolling through a large CJK/emoji corpus can't accumulate
            // shaped buffers unbounded (F25). Never evict a char visible THIS
            // frame (it was just needed and is drawn below). The cap sits well
            // above any single frame's distinct-fallback-char count, so this only
            // trims chars from long-past frames; it runs only when over cap.
            if self.fallback_glyphs.len() > FALLBACK_GLYPH_CAP {
                let visible: std::collections::HashSet<char> =
                    fallback_cells.iter().map(|(_, _, c, _)| *c).collect();
                evict_fifo_cache(
                    &mut self.fallback_glyphs,
                    &mut self.fallback_order,
                    &visible,
                    FALLBACK_GLYPH_CAP,
                );
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

        // Build a Vec of TextAreas; fallback overdraws and scrollbar are pushed
        // when applicable. The CURSOR is no longer a text glyph here — it is drawn
        // as a QuadLayer rect app-side (see `quad::cursor_rects`) so it can take an
        // arbitrary shape (block/beam/underline/hollow) and ride the caret flash.

        let mut areas: Vec<TextArea> = vec![text_area];

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
            // Shaping::Advanced: chrome text now carries user/shell-controlled
            // strings (OSC tab titles, search queries, rename buffers), so it
            // needs cosmic-text's font fallback — under Basic every glyph the
            // chrome family lacks (emoji, CJK, symbols on a custom UI font)
            // rendered as a tofu box. Chrome is proportional overlay text with
            // no grid-alignment constraint, and overlays only shape on rendered
            // frames (idle draws nothing), so Advanced is safe here.
            buf.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet, VecDeque};

    #[test]
    fn evict_fifo_bounds_map_and_keeps_visible() {
        // Regression (F25): the fallback-glyph cache must stay bounded, evicting
        // the OLDEST non-visible entries while never dropping a char drawn this
        // frame.
        let mut map: HashMap<char, ()> = HashMap::new();
        let mut order: VecDeque<char> = VecDeque::new();
        // Insert 10 distinct chars 'a'..'j' in order.
        for c in "abcdefghij".chars() {
            map.insert(c, ());
            order.push_back(c);
        }
        // 'a' and 'b' are the oldest but 'a' is visible this frame → keep it.
        let visible: HashSet<char> = ['a', 'z'].into_iter().collect();
        evict_fifo_cache(&mut map, &mut order, &visible, 6);
        assert!(map.len() <= 6, "map bounded to cap; got {}", map.len());
        assert!(map.contains_key(&'a'), "visible 'a' must survive eviction");
        assert!(!map.contains_key(&'b'), "oldest non-visible 'b' evicted");
    }

    #[test]
    fn evict_fifo_noop_under_cap() {
        let mut map: HashMap<char, ()> = "abc".chars().map(|c| (c, ())).collect();
        let mut order: VecDeque<char> = "abc".chars().collect();
        let visible = HashSet::new();
        evict_fifo_cache(&mut map, &mut order, &visible, 10);
        assert_eq!(map.len(), 3, "no eviction while under cap");
    }

    #[test]
    fn evict_fifo_terminates_when_all_visible() {
        // If every over-cap entry is visible, eviction must not loop forever —
        // it rotates them and stops after one full scan (the cap may be exceeded
        // this frame, which is fine; next frame's set differs).
        let mut map: HashMap<char, ()> = "abcde".chars().map(|c| (c, ())).collect();
        let mut order: VecDeque<char> = "abcde".chars().collect();
        let visible: HashSet<char> = "abcde".chars().collect();
        evict_fifo_cache(&mut map, &mut order, &visible, 2);
        assert_eq!(map.len(), 5, "all-visible entries are retained, no hang");
        assert_eq!(order.len(), 5, "order queue preserved");
    }
}
