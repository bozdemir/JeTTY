use crate::colors::{contrast_ratio, SelectionPaint, SELECTION_MIN_CONTRAST};
use crate::gpu::GpuContext;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, PrepareError, Resolution, Shaping,
    Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use jetty_core::{CellSnapshot, GridSnapshot};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;
use std::sync::Arc;
use wgpu::MultisampleState;

/// Extra per-frame inputs for [`TextLayer::render_grid`]. `Default` is the plain
/// grid: no block-cursor recolor, selected glyphs keep their colors, no graphemes.
#[derive(Debug, Clone, Copy, Default)]
pub struct GridPaint<'a> {
    /// `(row, col, color)` of the glyph under a SOLID block cursor. The block is a
    /// quad painted UNDER the text (see `quad::cursor_rects_split`), so the glyph
    /// stays visible on top of it in this contrast color.
    pub cursor_glyph: Option<(usize, usize, [u8; 3])>,
    /// Selected-glyph coloring (see [`SelectionPaint`]); `None` = unchanged colors.
    pub selection: Option<SelectionPaint>,
    /// Sparse grapheme-cluster overrides `(row, col, cluster)`: a cell whose base
    /// char carries combining marks / VS16 / ZWJ parts is drawn from the whole
    /// cluster (shaped with font fallback, at the cell origin) instead of the bare
    /// base char. Empty — the common case — costs nothing.
    pub graphemes: &'a [(usize, usize, &'a str)],
}

/// Pack one cell's DRAWN content — the (possibly blanked) char, its final fg and
/// its shape bits (BOLD|ITALIC) — into one word. A row's shaped `Buffer` depends on
/// exactly these, so rows are compared on the packed words exactly: a hash only
/// nominates the candidate, a collision can never draw stale text.
#[inline]
fn pack_cell(c: char, fg: [u8; 3], shape: u8) -> u64 {
    c as u64 | (fg[0] as u64) << 32 | (fg[1] as u64) << 40 | (fg[2] as u64) << 48 | (shape as u64) << 56
}

#[inline]
fn unpack_cell(k: u64) -> (char, [u8; 3], u8) {
    let c = char::from_u32(k as u32).unwrap_or(' ');
    (c, [(k >> 32) as u8, (k >> 40) as u8, (k >> 48) as u8], (k >> 56) as u8)
}

/// The glyph color of a SELECTED cell. Concealed text (SGR 8 resolves to fg == bg)
/// stays invisible on the highlight; a theme `selection_fg` wins; otherwise the
/// glyph keeps its own color unless it would be unreadable on the highlight.
/// `memo` caches the last (input → output) pair: a selection is mostly one color.
#[inline]
fn selected_glyph_fg(
    cell: &CellSnapshot,
    sel: &SelectionPaint,
    memo: &mut Option<([u8; 3], [u8; 3])>,
) -> [u8; 3] {
    if cell.fg == cell.bg {
        return sel.bg;
    }
    if let Some(fg) = sel.fg {
        return fg;
    }
    if let Some((i, o)) = *memo {
        if i == cell.fg {
            return o;
        }
    }
    let out = if contrast_ratio(cell.fg, sel.bg) < SELECTION_MIN_CONTRAST { sel.fallback_fg } else { cell.fg };
    *memo = Some((cell.fg, out));
    out
}

/// One grid row shaped into its own single-line cosmic-text `Buffer`, cached by
/// content so a row that did not change — or merely scrolled to a new y — is never
/// re-shaped (cosmic-text's `set_rich_text` re-shapes EVERY line of a buffer, so a
/// single whole-grid buffer re-shaped the whole screen for a one-cell change).
struct ShapedRow {
    key: Vec<u64>,
    hash: u64,
    buffer: Buffer,
    /// `TextLayer::frame_no` of the last frame this row was drawn (LRU eviction).
    last_used: u64,
}

/// Slot marker for a blank row (nothing to shape or draw).
const NO_ROW: u32 = u32::MAX;

/// Everything the grid's last successful glyphon `prepare` was built from. When a
/// frame matches it exactly, the renderer's vertex buffer already holds this frame's
/// glyphs, so shaping AND prepare are skipped (caret-flash / CRT-only frames).
#[derive(Default)]
struct PreparedGrid {
    shape_gen: u64,
    width: u32,
    height: u32,
    top_bits: u32,
    rows: usize,
    cols: usize,
    keys: Vec<u64>,
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    /// `(x, y, index into clusters, fg)` per cluster-drawn cell.
    graphemes: Vec<(f32, f32, u32, [u8; 3])>,
    /// The distinct (clamped) clusters those cells draw — bounded by
    /// `GRAPHEME_GLYPH_CAP` × `MAX_CLUSTER_BYTES`.
    clusters: Vec<Box<str>>,
}

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

/// Upper bound on cached shaped grapheme-cluster buffers (same role as
/// `FALLBACK_GLYPH_CAP` for the per-char overdraw cache). Also the per-frame cap
/// on DISTINCT clusters drawn, so the cache never has to hold more than this.
const GRAPHEME_GLYPH_CAP: usize = 1024;

/// The longest grapheme cluster drawn for one cell: its base char plus this many
/// zero-width chars (combining marks, VS16, ZWJ parts). Real text needs a handful
/// (Vietnamese 2, Tibetan stacks ~4–6); the VT engine stores ANY number on a cell
/// (alacritty's `push_zerowidth` is unbounded), so a "Zalgo" flood can stack
/// thousands — the excess is simply not drawn.
const MAX_CLUSTER_MARKS: usize = 32;
/// Byte cap on one drawn cluster (cut on a char boundary), whichever cap hits first.
const MAX_CLUSTER_BYTES: usize = 256;
/// Per-frame budget of cluster chars drawn across ALL cells. Bounds the glyphs a
/// frame's grapheme overrides add to glyphon's prepare (and the shaping a frame
/// of all-new clusters can cost). Cells past it — or past `GRAPHEME_GLYPH_CAP`
/// distinct clusters — draw just their base char. Generous for real text: a
/// screen full of NFD / Thai / Devanagari clusters stays well inside it.
const GRAPHEME_FRAME_CHARS: usize = 32 * 1024;

/// `s` cut to what is drawn for one cell (see `MAX_CLUSTER_MARKS` /
/// `MAX_CLUSTER_BYTES`): a prefix on a char boundary, plus its char count. Scans
/// at most the kept prefix — O(cap), however long `s` is.
fn clamp_cluster(s: &str) -> (&str, usize) {
    let mut end = 0;
    let mut chars = 0;
    for (i, c) in s.char_indices() {
        let next = i + c.len_utf8();
        if chars > MAX_CLUSTER_MARKS || next > MAX_CLUSTER_BYTES {
            break;
        }
        end = next;
        chars += 1;
    }
    (&s[..end], chars)
}

/// Approximate bytes one cached grid row costs per cell: the packed key word plus
/// cosmic-text's per-char text, attribute, shape and layout-glyph records
/// (pinned against the real struct sizes by a test).
const ROW_BYTES_PER_CELL: usize = 256;
/// Byte budget for cached grid rows BEYOND the ones on screen. The visible rows
/// must be shaped whatever they cost (O(rows × cols), the grid itself); the
/// off-screen extras that make scrolling back a cache hit get at most this much.
const ROW_CACHE_EXTRA_BYTES: usize = 16 * 1024 * 1024;

/// Row-cache entry cap for a `rows × cols` grid: every visible row, plus up to
/// one more screen (+16) of off-screen rows within `ROW_CACHE_EXTRA_BYTES`.
fn row_cache_cap(rows: usize, cols: usize) -> usize {
    let per_row = cols.max(1) * ROW_BYTES_PER_CELL;
    rows + (rows + 16).min(ROW_CACHE_EXTRA_BYTES / per_row)
}

/// Evict oldest entries from a FIFO-ordered cache down to `cap`, never removing
/// a key for which `visible` is true (keys drawn this frame). A visible key
/// scanned during eviction is rotated to the back (treated as most-recent)
/// instead of dropped. Pure + generic so it is unit-testable independent of
/// cosmic-text `Buffer`. (F25)
fn evict_fifo_cache<K: std::hash::Hash + Eq, V>(
    map: &mut std::collections::HashMap<K, V>,
    order: &mut std::collections::VecDeque<K>,
    visible: impl Fn(&K) -> bool,
    cap: usize,
) {
    let mut scanned = 0usize;
    let cap_scan = order.len();
    while map.len() > cap && scanned < cap_scan {
        let Some(old) = order.pop_front() else { break };
        scanned += 1;
        if visible(&old) {
            order.push_back(old);
        } else {
            map.remove(&old);
        }
    }
}

/// Shaped grapheme-cluster buffers, keyed by the (clamped) cluster. Bounded in
/// entries (`GRAPHEME_GLYPH_CAP`, FIFO, never evicting a cluster drawn this
/// frame — and a frame draws at most that many distinct clusters) and per entry
/// (a key is at most `MAX_CLUSTER_BYTES`, its buffer at most
/// `MAX_CLUSTER_MARKS + 1` chars), so its memory is bounded no matter what a
/// program prints. Generic over the cached value only so the bounds are testable
/// without a font; the renderer caches shaped `Buffer`s.
struct ClusterGlyphCache<V = Buffer> {
    map: std::collections::HashMap<Box<str>, V>,
    order: std::collections::VecDeque<Box<str>>,
}

impl<V> Default for ClusterGlyphCache<V> {
    fn default() -> Self {
        Self { map: std::collections::HashMap::new(), order: std::collections::VecDeque::new() }
    }
}

impl<V> ClusterGlyphCache<V> {
    /// Build (`make`) every cluster of this frame not cached yet, then evict down
    /// to the cap, keeping this frame's clusters.
    fn ensure_with(&mut self, clusters: &[&str], mut make: impl FnMut(&str) -> V) {
        for &cluster in clusters {
            debug_assert!(cluster.len() <= MAX_CLUSTER_BYTES, "clusters are clamped before caching");
            if !self.map.contains_key(cluster) {
                let v = make(cluster);
                self.map.insert(Box::from(cluster), v);
                self.order.push_back(Box::from(cluster));
            }
        }
        if self.map.len() > GRAPHEME_GLYPH_CAP {
            let drawn: rustc_hash::FxHashSet<&str> = clusters.iter().copied().collect();
            evict_fifo_cache(&mut self.map, &mut self.order, |k| drawn.contains(&**k), GRAPHEME_GLYPH_CAP);
        }
    }

    /// Bytes held by the keys (each stored twice: map + FIFO order).
    #[cfg(test)]
    fn key_bytes(&self) -> usize {
        self.map.keys().map(|k| k.len()).sum::<usize>() + self.order.iter().map(|k| k.len()).sum::<usize>()
    }

    fn get(&self, cluster: &str) -> Option<&V> {
        self.map.get(cluster)
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

impl ClusterGlyphCache<Buffer> {
    /// Shape every cluster of this frame not cached yet (`Shaping::Advanced`, so
    /// font fallback supplies marks/emoji the primary font lacks).
    fn ensure(&mut self, font_system: &mut FontSystem, metrics: Metrics, family: &str, clusters: &[&str]) {
        let attrs = Attrs::new().family(Family::Name(family));
        self.ensure_with(clusters, |cluster| {
            let mut buf = Buffer::new(font_system, metrics);
            buf.set_size(font_system, None, None);
            buf.set_text(font_system, cluster, &attrs, Shaping::Advanced, None);
            buf
        });
    }
}

/// Scratch vectors for [`pack_grid`], reused frame to frame so the frame path
/// does not reallocate.
#[derive(Default)]
struct PackScratch {
    keys: Vec<u64>,
    row_hashes: Vec<u64>,
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    graphemes: Vec<(f32, f32, u32, [u8; 3])>,
    order: Vec<usize>,
}

/// One frame's grid, packed for shaping, comparison and drawing.
struct PackedGrid<'a> {
    /// `rows * cols` packed cells (see `pack_cell`) — independent of any cluster:
    /// a cluster-drawn cell packs as a blank, so a row costs O(cols) whatever a
    /// program stacked on its cells.
    keys: Vec<u64>,
    /// Per-row content hash; 0 = all-blank row (nothing to shape or draw).
    row_hashes: Vec<u64>,
    /// `(x, y, char, fg)` per overdraw cell (glyph missing from the primary font,
    /// or double-width).
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    /// `(x, y, index into clusters, fg)` per cell drawn from its grapheme cluster.
    graphemes: Vec<(f32, f32, u32, [u8; 3])>,
    /// The DISTINCT clusters drawn this frame, each clamped by `clamp_cluster`; at
    /// most `GRAPHEME_GLYPH_CAP` of them, `GRAPHEME_FRAME_CHARS` chars across all
    /// cells.
    clusters: Vec<&'a str>,
    /// Underline/strike content fingerprint (see `quad::fold_decoration`).
    deco: u64,
    order: Vec<usize>,
}

impl PackedGrid<'_> {
    fn into_scratch(self) -> PackScratch {
        PackScratch {
            keys: self.keys,
            row_hashes: self.row_hashes,
            fallback: self.fallback,
            graphemes: self.graphemes,
            order: self.order,
        }
    }
}

/// Pack every cell of `snapshot` (pure: no GPU, no font system — `route` answers
/// whether a char must be overdrawn). Applies the paint inputs (selection colors,
/// the block-cursor glyph color) and the grapheme overrides under their budgets:
/// a cluster is clamped to `MAX_CLUSTER_MARKS` / `MAX_CLUSTER_BYTES`, and a frame
/// draws at most `GRAPHEME_GLYPH_CAP` distinct clusters and `GRAPHEME_FRAME_CHARS`
/// cluster chars — a cell past either budget draws just its base char. However
/// large a program makes a cell's cluster, the work here is O(cells × cap).
fn pack_grid<'a>(
    snapshot: &GridSnapshot,
    paint: &GridPaint<'a>,
    cell_w: f32,
    cell_h: f32,
    route: &mut dyn FnMut(char) -> CellRoute,
    scratch: PackScratch,
) -> PackedGrid<'a> {
    let (rows, cols) = (snapshot.rows, snapshot.cols);
    let PackScratch { mut keys, mut row_hashes, mut fallback, mut graphemes, mut order } = scratch;
    keys.clear();
    keys.reserve(rows * cols);
    row_hashes.clear();
    fallback.clear();
    graphemes.clear();
    // Grapheme overrides in row-major order, walked with a cursor alongside the
    // cell loop (empty — the common case — means no sort and one compare/cell).
    order.clear();
    if !paint.graphemes.is_empty() {
        order.extend(0..paint.graphemes.len());
        order.sort_unstable_by_key(|&i| (paint.graphemes[i].0, paint.graphemes[i].1));
    }
    let g_pos = |i: usize| (paint.graphemes[i].0, paint.graphemes[i].1);
    let mut g_next = 0usize;
    let mut clusters: Vec<&'a str> = Vec::new();
    let mut cluster_index: FxHashMap<&'a str, u32> = FxHashMap::default();
    let mut cluster_chars = 0usize;
    let mut sel_memo: Option<([u8; 3], [u8; 3])> = None;
    // Fingerprint for the underline/strike quads (folded in the SAME per-cell
    // loop, so no extra pass): an underline-only change rebuilds decorations
    // without forcing a re-shape.
    let mut deco_hasher = FxHasher::default();

    for row in 0..rows {
        let mut h = FxHasher::default();
        let mut inked = false;
        for col in 0..cols {
            let cell = snapshot.cell(row, col);
            crate::quad::fold_decoration(&mut deco_hasher, cell);
            let mut fg = cell.fg;
            if cell.selected {
                if let Some(sel) = &paint.selection {
                    fg = selected_glyph_fg(cell, sel, &mut sel_memo);
                }
            }
            if let Some((cr, cc, cursor_fg)) = paint.cursor_glyph {
                if cr == row && cc == col {
                    fg = cursor_fg;
                }
            }
            // alacritty stores a literal '\t' in the cell at a tab stop (so
            // copies preserve tabs); control chars have no glyph, so render
            // them as blanks instead of routing them to the overdraw (tofu).
            let mut ch = if cell.c.is_control() { ' ' } else { cell.c };
            while g_next < order.len() && g_pos(order[g_next]) < (row, col) {
                g_next += 1;
            }
            let mut drawn_as_cluster = false;
            if g_next < order.len() && g_pos(order[g_next]) == (row, col) {
                let (cluster, n) = clamp_cluster(paint.graphemes[order[g_next]].2);
                let ci = if n == 0 || cluster_chars + n > GRAPHEME_FRAME_CHARS {
                    None
                } else if let Some(&ci) = cluster_index.get(cluster) {
                    Some(ci)
                } else if clusters.len() < GRAPHEME_GLYPH_CAP {
                    let ci = clusters.len() as u32;
                    clusters.push(cluster);
                    cluster_index.insert(cluster, ci);
                    Some(ci)
                } else {
                    None
                };
                if let Some(ci) = ci {
                    cluster_chars += n;
                    graphemes.push((col as f32 * cell_w, row as f32 * cell_h, ci, fg));
                    ch = ' ';
                    drawn_as_cluster = true;
                }
            }
            if !drawn_as_cluster && !ch.is_ascii() && route(ch) == CellRoute::Overdraw {
                // A glyph the primary font lacks (tofu under Shaping::Basic, no
                // fallback) or draws double-width (a CJK glyph advances ~2 cells
                // and would shift the rest of the row): blank it here so the row
                // stays on the grid, overdraw the real glyph on top, aligned.
                // ASCII (incl. blank) cells skip the (cached) probe entirely — the
                // primary font always lays them out inline.
                fallback.push((col as f32 * cell_w, row as f32 * cell_h, ch, fg));
                ch = ' ';
            }
            inked |= ch != ' ';
            let k = pack_cell(ch, fg, cell.shape_bits());
            h.write_u64(k);
            keys.push(k);
        }
        // 0 marks an all-blank row: no glyphs to shape or draw.
        row_hashes.push(if inked { h.finish() | 1 } else { 0 });
    }
    PackedGrid { keys, row_hashes, fallback, graphemes, clusters, deco: deco_hasher.finish(), order }
}

pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    /// Shaped grid rows, cached by content (see `ShapedRow`). Bounded to ~2
    /// screens of rows (LRU) so scrolling back and forth keeps hitting.
    row_cache: Vec<ShapedRow>,
    /// `shape_gen` the row cache was built under; a mismatch (font family/size,
    /// resize) drops every cached row.
    row_cache_gen: u64,
    /// Bumped once per frame that re-prepares the grid (row-cache LRU clock).
    frame_no: u64,
    /// The exact inputs of the grid's last successful prepare (`None` = the
    /// renderer's vertex buffer does not hold the grid, e.g. after an overlay
    /// prepare on this layer).
    prepared: Option<PreparedGrid>,
    /// Per-frame scratch, reused so the frame path does not reallocate.
    pack_scratch: PackScratch,
    row_slots_scratch: Vec<u32>,
    row_miss_scratch: Vec<usize>,
    row_index_scratch: FxHashMap<u64, u32>,
    /// Shaped grapheme-cluster buffers for `GridPaint::graphemes` (bounded — see
    /// `ClusterGlyphCache`).
    clusters: ClusterGlyphCache,
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
    /// Scratch for shaping ONE row: its text and `(byte_start, byte_end, fg color,
    /// shape_bits)` per coalesced run. `shape_bits` = `attrs & SHAPE_MASK`
    /// (BOLD|ITALIC) — the run breaks when it changes so each run carries a single
    /// weight/style. STRIKE / underline never break a run (they are quads).
    text_scratch: String,
    cell_ranges_scratch: Vec<(usize, usize, Color, u8)>,
    /// Per-char routing cache for the PRIMARY terminal font (`font_family`): does the
    /// char lay out inline, or must it be blanked and overdrawn (missing glyph — e.g.
    /// Claude Code's `⏵⏵` U+23F5 — OR a double-width CJK glyph)? Probed lazily on the
    /// hot path (only non-ASCII, on miss) and read every frame. Cleared when
    /// `font_family` changes (routing is per-font).
    glyph_route: FxHashMap<char, CellRoute>,
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
    /// Monotonic counter bumped whenever a change invalidates shaped grid content
    /// (font family, font size, resize): it drops the row cache and forces the next
    /// frame to re-prepare even when the grid text/colors are unchanged.
    shape_gen: u64,
    /// Cached underline/strikethrough quads and the key they were built for:
    /// `(grid_decoration_key, cell_w bits, cell_h bits, y_offset bits)`. Rebuilt
    /// only when that changes, so a caret-flash / CRT / scrollbar-only animate
    /// frame (same grid) reuses them — decorations never rebuild per frame; only
    /// the CURSOR quads do (drawn app-side). Consumed via `decoration_rects()`.
    deco_rects: Vec<crate::quad::Rect>,
    deco_cache_key: Option<(u64, u32, u32, u32, u32, u32)>,
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

    /// A new `FontSystem` over THIS layer's already-loaded font database — a copy
    /// of the face index (well under a millisecond), not a fresh fontconfig scan
    /// (~15–20ms). Every layer after the first (chrome text, Settings, each
    /// detached window) builds from this so none of them rescans on the main
    /// thread. Faces installed after startup are not picked up — exactly like a
    /// layer built at startup.
    pub fn clone_font_system(&self) -> FontSystem {
        FontSystem::new_with_locale_and_db(
            self.font_system.locale().to_string(),
            self.font_system.db().clone(),
        )
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

        // The cursor is drawn as a QuadLayer rect (see `quad::cursor_rects_split`),
        // not a text-atlas block glyph, so there is no cursor buffer to build here.
        // Grid rows get their own buffers lazily (see `new_row_buffer`).

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
            row_cache: Vec::new(),
            row_cache_gen: 0,
            frame_no: 0,
            prepared: None,
            pack_scratch: PackScratch::default(),
            row_slots_scratch: Vec::new(),
            row_miss_scratch: Vec::new(),
            row_index_scratch: FxHashMap::default(),
            clusters: ClusterGlyphCache::default(),
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
            glyph_route: FxHashMap::default(),
            coverage_buffer,
            fallback_glyphs: std::collections::HashMap::new(),
            fallback_order: std::collections::VecDeque::new(),
            shape_gen: 0,
            deco_rects: Vec::new(),
            deco_cache_key: None,
        }
    }

    /// A fresh single-line buffer for one grid row. `None` width disables wrapping
    /// so columns stay on the monospace grid; the height bound is one line.
    ///
    /// Every grid glyph's advance is snapped to the cell width: cosmic-text rounds
    /// each glyph's x_advance to the nearest `cell_w` (shape.rs), which keeps a real
    /// Bold/Italic face — or any stray wide/fallback glyph — column-aligned even
    /// when its natural advance differs from Regular. This is the alignment
    /// guarantee that lets us render real bold/italic faces (v0.13 amendment). Set
    /// ONLY on grid rows; the chrome overlay buffers are proportional
    /// (Shaping::Advanced) and never get this.
    fn new_row_buffer(&mut self) -> Buffer {
        let mut b = Buffer::new(&mut self.font_system, self.metrics);
        b.set_size(&mut self.font_system, None, Some(self.metrics.line_height));
        b.set_monospace_width(&mut self.font_system, Some(self.cell_w));
        b
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
        self.clusters.clear();
        // The shape-gen bump also drops every cached grid row: they are rebuilt with
        // the new family and its cell width as their monospace snap, so a
        // bold/italic run in the new family stays column-aligned.
        self.shape_gen = self.shape_gen.wrapping_add(1);
        // Re-measure cell width with the new family.
        self.cell_w = measure_advance_family(&mut self.font_system, self.metrics, name);
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
        // Cached fallback/grapheme glyphs were shaped at the old size; drop them.
        // The shape-gen bump drops every cached grid row too, so rows are rebuilt
        // at the new metrics with the new cell width as their monospace snap
        // (keeps bold/italic aligned after a font-size / DPI change). On a chrome
        // layer the grid rows are never built — chrome renders via overlay_buffers.
        self.fallback_glyphs.clear();
        self.fallback_order.clear();
        self.clusters.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
    }

    /// Returns the currently active font family name.
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// The grid rows' monospace snap width. Exposed for the alignment self-test /
    /// inspection; the grid glyph advances are rounded to this so real bold/italic
    /// faces stay column-aligned.
    pub fn grid_monospace_width(&self) -> Option<f32> {
        Some(self.cell_w)
    }

    /// The cached underline/strikethrough quads for the last rendered frame, built
    /// at the `top_offset` passed to `render_to`. The caller appends these to its
    /// Pass-4 quad batch (they draw over the glyphs, under the cursor).
    pub fn decoration_rects(&self) -> &[crate::quad::Rect] {
        &self.deco_rects
    }

    pub fn resize(&mut self, gpu: &GpuContext) {
        // Rows never wrap (no width bound) and the viewport is updated per frame,
        // so nothing here depends on the new size; the bump just guarantees the
        // next frame re-prepares from freshly shaped rows.
        self.shape_gen = self.shape_gen.wrapping_add(1);
        let _ = gpu;
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
    ///
    /// The plain grid: see [`Self::render_grid`] for the block-cursor glyph,
    /// selection colors and grapheme clusters.
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
        self.render_grid(device, queue, view, width, height, snapshot, clear, top_offset, &GridPaint::default())
    }

    /// [`Self::render_to`] with the per-frame [`GridPaint`] inputs.
    ///
    /// Cost model (the hot path): one pass over the cells packs each cell's drawn
    /// content into a word and folds the decoration fingerprint. Then:
    /// * nothing changed since the last prepare (caret flash, CRT, scrollbar-only
    ///   frames) → no shaping, no glyphon `prepare`, just the draw;
    /// * otherwise each row is looked up in the content-keyed row cache — only
    ///   rows whose content is new are shaped (typing shapes one row; a one-line
    ///   scroll shapes only the new bottom row, every other row is a cache hit at
    ///   its new y) — and glyphon re-prepares the visible glyphs.
    #[allow(clippy::too_many_arguments)]
    pub fn render_grid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        snapshot: &GridSnapshot,
        clear: bool,
        top_offset: f32,
        paint: &GridPaint,
    ) -> Result<(), PrepareError> {
        self.prepare_grid(device, queue, width, height, snapshot, top_offset, paint)?;
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
            self.draw_grid(&mut pass);
        }
        queue.submit(Some(encoder.finish()));
        self.end_grid_frame();
        Ok(())
    }

    /// Record the glyphs of the last successful [`Self::prepare_grid`] into a
    /// caller-owned render pass — e.g. the SAME pass as the background quads, so a
    /// frame's grid costs one pass and one submit instead of two of each.
    pub fn draw_grid(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Err(e) = self.renderer.render(&self.atlas, &self.viewport, pass) {
            eprintln!("jetty: text render error: {e:?}");
        }
    }

    /// Call once the pass carrying [`Self::draw_grid`] is recorded: unpin this
    /// frame's glyphs so the NEXT prepare can LRU-evict stale ones. glyphon pins
    /// every rendered glyph in `glyphs_in_use` and only trim() clears it; without
    /// this per-frame trim the atlas grows unbounded until AtlasFull. (Unpinning is
    /// CPU bookkeeping only — the recorded draw is unaffected. A frame that skipped
    /// prepare pinned nothing new; no prepare on this atlas runs before the next
    /// grid prepare except an overlay's, and that one invalidates `prepared` — so
    /// an evicted glyph is never drawn from stale vertices.)
    pub fn end_grid_frame(&mut self) {
        self.atlas.trim();
    }

    /// Everything [`Self::render_grid`] does before recording the draw: pack the
    /// cells, rebuild decorations, shape new rows and glyphon-prepare — or nothing
    /// at all when the grid is unchanged. Pair with [`Self::draw_grid`] +
    /// [`Self::end_grid_frame`]; on `Err` there is nothing valid to draw.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_grid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        snapshot: &GridSnapshot,
        top_offset: f32,
        paint: &GridPaint,
    ) -> Result<(), PrepareError> {
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let (rows, cols) = (snapshot.rows, snapshot.cols);

        // ---- 1. Pack every cell (pure; see `pack_grid`): keys, row hashes, the
        // overdraw cells and the budgeted grapheme clusters, plus the decoration
        // fingerprint.
        let scratch = std::mem::take(&mut self.pack_scratch);
        let packed = pack_grid(snapshot, paint, cell_w, cell_h, &mut |c| self.route(c), scratch);

        // ---- 2. Underline/strike quads: rebuilt only when their content, the cell
        // metrics or the grid offset changed. Grid dims are part of the key:
        // fold_decoration folds cells positionally by LINEAR index, so a
        // cell-count-preserving reflow (e.g. 80x24 -> 60x32) can fold identically
        // yet needs different rects.
        let deco_key = (packed.deco, cell_w.to_bits(), cell_h.to_bits(), top_offset.to_bits(), cols as u32, rows as u32);
        if self.deco_cache_key != Some(deco_key) {
            self.deco_rects.clear();
            crate::quad::text_decoration_rects(snapshot, cell_w, cell_h, top_offset, &mut self.deco_rects);
            self.deco_cache_key = Some(deco_key);
        }

        self.viewport.update(queue, Resolution { width, height });

        // ---- 3. Identical to the last successful prepare? Then glyphon's vertex
        // buffer already holds exactly this frame's glyphs: no shaping, no prepare.
        let unchanged = self.prepared.as_ref().is_some_and(|p| {
            p.shape_gen == self.shape_gen
                && p.width == width
                && p.height == height
                && p.top_bits == top_offset.to_bits()
                && p.rows == rows
                && p.cols == cols
                && p.keys == packed.keys
                && p.fallback == packed.fallback
                && p.graphemes == packed.graphemes
                && p.clusters.len() == packed.clusters.len()
                && p.clusters.iter().zip(&packed.clusters).all(|(a, b)| **a == **b)
        });

        let mut result = Ok(());
        let mut packed = packed;
        if !unchanged {
            // Whatever happens below, the renderer no longer holds a known grid.
            let mut p = self.prepared.take().unwrap_or_default();
            let slots = self.shape_rows(rows, cols, &packed.keys, &packed.row_hashes);
            self.ensure_overdraw_buffers(&packed.fallback, &packed.clusters);

            let win_bounds = TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            let default_color = Color::rgb(220, 220, 220);
            let mut areas: Vec<TextArea> =
                Vec::with_capacity(rows + packed.fallback.len() + packed.graphemes.len());
            for (r, &slot) in slots.iter().enumerate() {
                if slot != NO_ROW {
                    areas.push(TextArea {
                        buffer: &self.row_cache[slot as usize].buffer,
                        left: 0.0,
                        top: top_offset + r as f32 * cell_h,
                        scale: 1.0,
                        bounds: win_bounds,
                        default_color,
                        custom_glyphs: &[],
                    });
                }
            }
            // Overdraws and clusters: drawn ON TOP of their blanked cells, at the
            // exact cell origin, in this same prepare() — so they never shift a
            // neighbor.
            for (x, y, c, rgb) in packed.fallback.iter() {
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
            for (x, y, ci, rgb) in packed.graphemes.iter() {
                if let Some(buffer) = self.clusters.get(packed.clusters[*ci as usize]) {
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

            // Prepare the atlas. If it reports AtlasFull, unpin every glyph (trim) so
            // LRU eviction can reclaim space, then retry once — without this a long
            // session eventually wedges with permanently-blank text (the atlas is also
            // trimmed at the end of every frame below, which is what keeps eviction
            // working at all).
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
                    areas,
                    &mut self.swash,
                );
            }
            self.row_slots_scratch = slots;
            if prepared.is_ok() {
                // Remember exactly what the vertex buffer now holds. The key vectors
                // are swapped, not copied: `packed.keys` takes the previous
                // allocation back as next frame's scratch. Cluster strings are
                // copied: at most GRAPHEME_GLYPH_CAP × MAX_CLUSTER_BYTES.
                p.shape_gen = self.shape_gen;
                p.width = width;
                p.height = height;
                p.top_bits = top_offset.to_bits();
                p.rows = rows;
                p.cols = cols;
                std::mem::swap(&mut p.keys, &mut packed.keys);
                p.fallback.clear();
                p.fallback.extend_from_slice(&packed.fallback);
                p.graphemes.clear();
                p.graphemes.extend_from_slice(&packed.graphemes);
                p.clusters.clear();
                p.clusters.extend(packed.clusters.iter().map(|&c| Box::from(c)));
                self.prepared = Some(p);
            }
            result = prepared;
        }

        // Return the scratch buffers for reuse next frame.
        self.pack_scratch = packed.into_scratch();
        result
    }

    /// Map every viewport row to a shaped buffer in the row cache, shaping only the
    /// rows whose exact content is not cached yet. Returns the per-row slot
    /// (`NO_ROW` for an all-blank row).
    fn shape_rows(&mut self, rows: usize, cols: usize, keys: &[u64], row_hashes: &[u64]) -> Vec<u32> {
        if self.row_cache_gen != self.shape_gen {
            self.row_cache.clear();
            self.row_cache_gen = self.shape_gen;
        }
        self.frame_no = self.frame_no.wrapping_add(1);
        let frame = self.frame_no;
        // Every visible row plus up to one more screen of off-screen rows (within a
        // byte budget), so scrolling back over what was just on screen hits.
        let cap = row_cache_cap(rows, cols);
        if self.row_cache.len() > cap {
            self.row_cache.sort_unstable_by_key(|r| std::cmp::Reverse(r.last_used));
            self.row_cache.truncate(cap);
        }
        let mut index = std::mem::take(&mut self.row_index_scratch);
        index.clear();
        for (i, r) in self.row_cache.iter().enumerate() {
            index.entry(r.hash).or_insert(i as u32);
        }
        let mut slots = std::mem::take(&mut self.row_slots_scratch);
        slots.clear();
        slots.resize(rows, NO_ROW);
        let mut misses = std::mem::take(&mut self.row_miss_scratch);
        misses.clear();
        // Exact hits first, so a cached row about to be needed at a new y (a scroll)
        // is never recycled for a miss below.
        for r in 0..rows {
            let h = row_hashes[r];
            if h == 0 {
                continue;
            }
            let key = &keys[r * cols..(r + 1) * cols];
            match index.get(&h) {
                Some(&i) if self.row_cache[i as usize].key == key => {
                    self.row_cache[i as usize].last_used = frame;
                    slots[r] = i;
                }
                _ => misses.push(r),
            }
        }
        for &r in &misses {
            let h = row_hashes[r];
            let key = &keys[r * cols..(r + 1) * cols];
            // An identical row shaped earlier in this loop: share its buffer.
            if let Some(&i) = index.get(&h) {
                if self.row_cache[i as usize].key == key {
                    self.row_cache[i as usize].last_used = frame;
                    slots[r] = i;
                    continue;
                }
            }
            // Recycle the least-recently-used row not drawn this frame once the cache
            // is full; otherwise grow it (keeps old rows around for later hits).
            let victim = if self.row_cache.len() >= cap {
                self.row_cache
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.last_used != frame)
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(i, _)| i)
            } else {
                None
            };
            let i = match victim {
                Some(i) => i,
                None => {
                    let buffer = self.new_row_buffer();
                    self.row_cache.push(ShapedRow { key: Vec::new(), hash: 0, buffer, last_used: 0 });
                    self.row_cache.len() - 1
                }
            };
            self.shape_row(i, key, h, frame);
            index.insert(h, i as u32);
            slots[r] = i as u32;
        }
        self.row_index_scratch = index;
        self.row_miss_scratch = misses;
        slots
    }

    /// (Re)shape row-cache entry `i` from its packed cell `key`: coalesce runs of
    /// equal (color, BOLD|ITALIC) into spans — cosmic-text allocates (and clones the
    /// family) per span, and a row has only a handful of color changes — and set them
    /// under `Shaping::Basic` (no kerning/ligatures, so every glyph lands exactly one
    /// cell apart).
    fn shape_row(&mut self, i: usize, key: &[u64], hash: u64, frame: u64) {
        let mut text = std::mem::take(&mut self.text_scratch);
        text.clear();
        let mut runs = std::mem::take(&mut self.cell_ranges_scratch);
        runs.clear();
        let mut run: Option<(Color, u8)> = None;
        let mut start = 0usize;
        // Shape only up to the last inked cell: trailing blanks have no glyph to
        // draw (backgrounds and decorations are quads), but cosmic-text would still
        // shape them and glyphon would still walk them every prepare.
        let inked = key.iter().rposition(|&k| (k as u32) != ' ' as u32).map_or(0, |i| i + 1);
        for &k in &key[..inked] {
            let (ch, rgb, shape) = unpack_cell(k);
            let rk = (Color::rgb(rgb[0], rgb[1], rgb[2]), shape);
            if run != Some(rk) {
                if let Some((c, s)) = run {
                    runs.push((start, text.len(), c, s));
                }
                start = text.len();
                run = Some(rk);
            }
            text.push(ch);
        }
        if let Some((c, s)) = run {
            runs.push((start, text.len(), c, s));
        }
        // Clone the Arc (a refcount bump, not a string copy) so every span can
        // borrow the family name without re-borrowing self.
        let family = Arc::clone(&self.font_family);
        let default_attrs = Attrs::new().family(Family::Name(&family));
        let row = &mut self.row_cache[i];
        row.buffer.set_rich_text(
            &mut self.font_system,
            runs.iter().map(|&(s, e, color, shape)| {
                // BOLD -> real Bold face, ITALIC -> real Italic face, under
                // Shaping::Basic. Monospace alignment is guaranteed by the row's
                // monospace snap (see `new_row_buffer`).
                let weight = if shape & jetty_core::attr::BOLD != 0 { Weight::BOLD } else { Weight::NORMAL };
                let style = if shape & jetty_core::attr::ITALIC != 0 { Style::Italic } else { Style::Normal };
                (
                    &text[s..e],
                    Attrs::new().family(Family::Name(&family)).color(color).weight(weight).style(style),
                )
            }),
            &default_attrs,
            Shaping::Basic,
            None,
        );
        row.key.clear();
        row.key.extend_from_slice(key);
        row.hash = hash;
        row.last_used = frame;
        self.text_scratch = text;
        self.cell_ranges_scratch = runs;
    }

    /// Shape each DISTINCT overdraw char once into a cached buffer with
    /// `Shaping::Advanced`, so cosmic-text either falls back to a font that HAS the
    /// glyph or uses the primary font's own double-width glyph — and likewise each
    /// distinct grapheme cluster of this frame (`ClusterGlyphCache`, which also
    /// composes combining marks / emoji sequences). Cached across frames (cleared
    /// on family/size change), so a char repeated across the grid — e.g.
    /// full-screen CJK — shapes only once. Usually both lists are empty and this
    /// does nothing.
    fn ensure_overdraw_buffers(&mut self, fallback_cells: &[(f32, f32, char, [u8; 3])], clusters: &[&str]) {
        if !fallback_cells.is_empty() {
            let fam = Arc::clone(&self.font_family);
            let metrics = self.metrics;
            let attrs = Attrs::new().family(Family::Name(&fam));
            for (_x, _y, c, _rgb) in fallback_cells {
                if !self.fallback_glyphs.contains_key(c) {
                    let mut buf = Buffer::new(&mut self.font_system, metrics);
                    buf.set_size(&mut self.font_system, None, None);
                    let mut tmp = [0u8; 4];
                    buf.set_text(&mut self.font_system, c.encode_utf8(&mut tmp), &attrs, Shaping::Advanced, None);
                    self.fallback_glyphs.insert(*c, buf);
                    self.fallback_order.push_back(*c);
                }
            }
            // Evict the oldest cached buffers once the cache exceeds its cap, so a
            // session scrolling through a large CJK/emoji corpus can't accumulate
            // shaped buffers unbounded (F25). Never evict one drawn THIS frame. The
            // cap sits well above any single frame's distinct count, so this only
            // trims long-past entries and runs only when over cap.
            if self.fallback_glyphs.len() > FALLBACK_GLYPH_CAP {
                let drawn: rustc_hash::FxHashSet<char> = fallback_cells.iter().map(|(_, _, c, _)| *c).collect();
                evict_fifo_cache(&mut self.fallback_glyphs, &mut self.fallback_order, |c| drawn.contains(c), FALLBACK_GLYPH_CAP);
            }
        }
        if !clusters.is_empty() {
            let fam = Arc::clone(&self.font_family);
            self.clusters.ensure(&mut self.font_system, self.metrics, &fam, clusters);
        }
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

        // This prepare overwrites the renderer's vertex buffer (and may LRU-evict
        // grid glyphs from the shared atlas), so the grid must re-prepare next frame.
        self.prepared = None;
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
    /// Measure the ACTUAL rendered width (physical px) of a chrome overlay string
    /// under the current UI family + size, using the SAME Advanced shaping as
    /// [`Self::render_overlays`]. Chrome overlays are PROPORTIONAL (no grid snap),
    /// so `chars().count() * cell_size().0` mis-measures a non-monospace UI font —
    /// use this to right-align the perf HUD / shift-hint pill correctly.
    pub fn measure_overlay_width(&mut self, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let ui_family = self.ui_family.clone();
        let mono_fallback = self.font_family.clone();
        let metrics = self.metrics;
        let mut b = Buffer::new(&mut self.font_system, metrics);
        let attrs = Attrs::new().family(ui_family.as_family(false, &mono_fallback));
        b.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
        b.set_size(&mut self.font_system, None, Some(metrics.line_height));
        b.layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|g| g.x + g.w)
            .fold(0.0_f32, f32::max)
    }

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

    /// A `cols × rows` grid of plain 'a'..'z' cells (no GPU, no fonts).
    fn plain_grid(cols: usize, rows: usize) -> GridSnapshot {
        let mut cells = vec![jetty_core::CellSnapshot::default(); cols * rows];
        for (i, c) in cells.iter_mut().enumerate() {
            c.c = (b'a' + (i % 26) as u8) as char;
        }
        GridSnapshot {
            cols,
            rows,
            cells,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: false,
            bg_rgba: [0, 0, 0, 255],
            cursor_rgb: [255, 255, 255],
            scroll_offset: 0,
            scroll_max: 0,
            cursor_shape: Default::default(),
        }
    }

    /// `base` + `marks` combining marks, made distinct per `id` by its first marks.
    fn zalgo(base: char, id: usize, marks: usize) -> String {
        let mut s = String::with_capacity(1 + marks * 2);
        s.push(base);
        // U+0300..U+036F: the combining diacritical marks block (112 marks).
        s.push(char::from_u32(0x300 + (id % 112) as u32).unwrap());
        s.push(char::from_u32(0x300 + (id / 112 % 112) as u32).unwrap());
        for k in 0..marks.saturating_sub(2) {
            s.push(char::from_u32(0x300 + (k % 112) as u32).unwrap());
        }
        s
    }

    #[test]
    fn clamp_cluster_caps_marks_and_bytes_and_keeps_real_clusters() {
        // Real clusters pass through untouched.
        for real in ["e\u{301}", "c\u{327}", "\u{2764}\u{fe0f}", "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", "a"] {
            assert_eq!(clamp_cluster(real), (real, real.chars().count()));
        }
        assert_eq!(clamp_cluster(""), ("", 0));
        // Floods of 1/2/3/4-byte zero-width chars: a prefix with the base char and at
        // most MAX_CLUSTER_MARKS marks, within MAX_CLUSTER_BYTES.
        for mark in ['\u{200b}', '\u{301}', '\u{20d0}', '\u{e0100}'] {
            let flood: String = std::iter::once('x').chain(std::iter::repeat_n(mark, 50_000)).collect();
            let (kept, n) = clamp_cluster(&flood);
            assert!(flood.starts_with(kept) && kept.starts_with('x'));
            assert_eq!(n, kept.chars().count());
            assert!(n <= 1 + MAX_CLUSTER_MARKS, "{n} chars kept");
            assert!(kept.len() <= MAX_CLUSTER_BYTES, "{} bytes kept", kept.len());
        }
    }

    #[test]
    fn zalgo_flood_packs_in_bounded_work_and_memory() {
        // A hostile frame: EVERY cell of a 240×70 grid carries a cluster of thousands
        // of combining marks (the VT engine stores any number per cell). Packing
        // must stay O(cells × cap): the clusters are clamped, at most
        // GRAPHEME_GLYPH_CAP distinct ones and GRAPHEME_FRAME_CHARS chars are drawn,
        // and a row's packed size never depends on what was stacked on its cells.
        let (cols, rows) = (240, 70);
        let snap = plain_grid(cols, rows);
        let distinct: Vec<String> = (0..2048).map(|id| zalgo('e', id, 2_000)).collect();
        // One 8 MB cluster shared by a whole row: if anything were O(cluster length)
        // per cell, this alone would scan ~2 GB and the test would crawl.
        let huge = zalgo('h', 0, 4_000_000);
        let overrides: Vec<(usize, usize, &str)> = (0..rows * cols)
            .map(|i| {
                let (r, c) = (i / cols, i % cols);
                let s: &str = if r == 5 { &huge } else { &distinct[i % distinct.len()] };
                (r, c, s)
            })
            .collect();
        let paint = GridPaint { graphemes: &overrides, ..Default::default() };
        let t = std::time::Instant::now();
        let packed = pack_grid(&snap, &paint, 10.0, 20.0, &mut |_| CellRoute::Inline, PackScratch::default());
        let elapsed = t.elapsed();

        assert!(packed.clusters.len() <= GRAPHEME_GLYPH_CAP, "{} distinct clusters", packed.clusters.len());
        for c in &packed.clusters {
            assert!(c.len() <= MAX_CLUSTER_BYTES && c.chars().count() <= 1 + MAX_CLUSTER_MARKS);
        }
        let drawn_chars: usize = packed
            .graphemes
            .iter()
            .map(|&(_, _, ci, _)| packed.clusters[ci as usize].chars().count())
            .sum();
        assert!(drawn_chars <= GRAPHEME_FRAME_CHARS, "{drawn_chars} cluster chars drawn");
        assert!(!packed.graphemes.is_empty(), "clusters within budget are still drawn");
        // Row memory: one word per cell, whatever the clusters weigh.
        assert_eq!(packed.keys.len(), rows * cols);
        // Cluster-drawn cells pack as blanks; cells past the budget keep their base
        // char (graceful: the text stays readable, only the marks are dropped).
        let drawn: HashSet<(u32, u32)> =
            packed.graphemes.iter().map(|&(x, y, _, _)| ((x / 10.0) as u32, (y / 20.0) as u32)).collect();
        for (i, &k) in packed.keys.iter().enumerate() {
            let (ch, _, _) = unpack_cell(k);
            let at = ((i % cols) as u32, (i / cols) as u32);
            if drawn.contains(&at) {
                assert_eq!(ch, ' ');
            } else {
                assert_eq!(ch, snap.cells[i].c, "a cell past the budget draws its base char");
            }
        }
        assert!(elapsed < std::time::Duration::from_secs(2), "packing a Zalgo flood took {elapsed:?}");
    }

    #[test]
    fn cluster_glyph_cache_stays_bounded_under_a_zalgo_stream() {
        // Frame after frame of brand-new hostile clusters (as `pack_grid` hands them
        // over: clamped, ≤ GRAPHEME_GLYPH_CAP per frame). The cache must stay within
        // its entry cap and its key bytes within cap × MAX_CLUSTER_BYTES (×2: the
        // map and the FIFO each hold the key) — and still serve every cluster drawn
        // in the current frame. Hermetic (no GPU, no font): the cached value records
        // the char count that would be shaped, which the clamp bounds per entry.
        let mut cache: ClusterGlyphCache<usize> = ClusterGlyphCache::default();
        for frame in 0..6 {
            let raw: Vec<String> =
                (0..GRAPHEME_GLYPH_CAP).map(|i| zalgo('z', frame * GRAPHEME_GLYPH_CAP + i, 5_000)).collect();
            let clusters: Vec<&str> = raw.iter().map(|s| clamp_cluster(s).0).collect();
            cache.ensure_with(&clusters, |c| c.chars().count());
            assert!(cache.map.values().all(|&n| n <= 1 + MAX_CLUSTER_MARKS), "each entry shapes ≤ the cap");
            assert!(cache.map.len() <= GRAPHEME_GLYPH_CAP, "frame {frame}: {} entries", cache.map.len());
            assert_eq!(cache.map.len(), cache.order.len(), "map and FIFO stay in sync");
            assert!(cache.key_bytes() <= 2 * GRAPHEME_GLYPH_CAP * MAX_CLUSTER_BYTES);
            assert!(clusters.iter().all(|c| cache.get(c).is_some()), "this frame's clusters are all served");
        }
    }

    #[test]
    fn row_cache_is_bounded_by_bytes_beyond_the_visible_rows() {
        // Ordinary grids keep a whole extra screen (+16 rows) for scroll-back hits.
        assert_eq!(row_cache_cap(40, 120), 2 * 40 + 16);
        assert_eq!(row_cache_cap(70, 240), 2 * 70 + 16);
        // Any grid, however huge: every visible row, and the off-screen extras within
        // the byte budget.
        for (rows, cols) in [(1, 1), (70, 240), (400, 1000), (2000, 4000), (5, 100_000)] {
            let cap = row_cache_cap(rows, cols);
            assert!(cap >= rows);
            assert!((cap - rows) * cols * ROW_BYTES_PER_CELL <= ROW_CACHE_EXTRA_BYTES, "{rows}x{cols}");
        }
        // The per-cell estimate covers cosmic-text's real per-glyph records (shape +
        // layout) plus the packed key word and up to 4 UTF-8 bytes of row text.
        let real = std::mem::size_of::<glyphon::ShapeGlyph>() + std::mem::size_of::<glyphon::LayoutGlyph>() + 8 + 4;
        assert!(ROW_BYTES_PER_CELL >= real, "ROW_BYTES_PER_CELL {ROW_BYTES_PER_CELL} < {real}");
    }

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
        evict_fifo_cache(&mut map, &mut order, |k| visible.contains(k), 6);
        assert!(map.len() <= 6, "map bounded to cap; got {}", map.len());
        assert!(map.contains_key(&'a'), "visible 'a' must survive eviction");
        assert!(!map.contains_key(&'b'), "oldest non-visible 'b' evicted");
    }

    #[test]
    fn evict_fifo_noop_under_cap() {
        let mut map: HashMap<char, ()> = "abc".chars().map(|c| (c, ())).collect();
        let mut order: VecDeque<char> = "abc".chars().collect();
        let visible: HashSet<char> = HashSet::new();
        evict_fifo_cache(&mut map, &mut order, |k| visible.contains(k), 10);
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
        evict_fifo_cache(&mut map, &mut order, |k| visible.contains(k), 2);
        assert_eq!(map.len(), 5, "all-visible entries are retained, no hang");
        assert_eq!(order.len(), 5, "order queue preserved");
    }
}
