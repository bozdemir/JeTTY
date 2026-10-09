use crate::colors::{contrast_ratio, SelectionPaint, SELECTION_MIN_CONTRAST};
use crate::gpu::GpuContext;
use crate::{builtin, emoji};
use glyphon::{
    Attrs, Buffer, Cache, Color, ContentType, CustomGlyph, Family, FontSystem, Metrics, PrepareError,
    RasterizeCustomGlyphRequest, RasterizedCustomGlyph, Resolution, Shaping, Style, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use jetty_core::{CellSnapshot, GridSnapshot};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;
use std::sync::Arc;
use unicode_width::UnicodeWidthChar;
use wgpu::MultisampleState;

/// glyphon's custom-glyph rasterizer for every prepare on a [`TextLayer`]: the
/// built-in box / block / Powerline / braille / sextant glyphs (`builtin.rs`).
/// Every prepare passes it — not only the grid's — because glyphon re-rasterizes
/// the custom glyphs already in its atlas whenever the atlas grows, and panics if
/// the rasterizer of THAT prepare cannot.
fn rasterize_builtin(req: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
    builtin::rasterize(req.id, req.width, req.height)
        .map(|data| RasterizedCustomGlyph { data, content_type: ContentType::Mask })
}

/// The first physical pixel a rect edge at `x` covers — the GPU's pixel-centre
/// rule for the background quads (a pixel is inside when its centre is). A
/// built-in glyph spanning `[px_edge(x0), px_edge(x1))` covers exactly the
/// pixels of its cell's background quad `[x0, x1)`, so adjacent glyphs tile with
/// no gap or overlap even at a fractional cell width.
#[inline]
fn px_edge(x: f32) -> f32 {
    (x - 0.5).ceil()
}

/// One built-in glyph cell of a packed frame: `(row, col, builtin slot, fg)`.
type BuiltinCell = (u16, u16, u16, [u8; 3]);

/// `glyph_route` key: the char plus its cell's BOLD|ITALIC bits — routing is per
/// FACE, since a bold or italic face can lack a glyph the regular one has.
#[inline]
fn route_key(c: char, shape: u8) -> u32 {
    c as u32 | ((shape & jetty_core::SHAPE_MASK) as u32) << 24
}

/// The attrs a grid cell of style `shape` (BOLD|ITALIC bits) is shaped with —
/// shared by the row shaping and the coverage probe so both pick the same face.
fn face_attrs(family: &str, shape: u8) -> Attrs<'_> {
    let weight = if shape & jetty_core::attr::BOLD != 0 { Weight::BOLD } else { Weight::NORMAL };
    let style = if shape & jetty_core::attr::ITALIC != 0 { Style::Italic } else { Style::Normal };
    Attrs::new().family(Family::Name(family)).weight(weight).style(style)
}

/// A char drawn as a color emoji (when an emoji font is installed and
/// `color_emoji` is on): wide and emoji-presentation (😀 ✅ 🚀) — never a
/// text-default symbol (✔ ❤ ☐ stay on the font unless a VS16 follows).
fn is_color_emoji_char(c: char) -> bool {
    emoji::is_emoji_presentation(c) && c.width() == Some(2)
}

/// Probe how the row shaping would lay `c` out in a cell of style `shape`: shape
/// it alone with the SAME face (`face_attrs`) under `Shaping::Basic` (no
/// fallback). A glyph id of 0 (`.notdef`, the tofu box) means that face lacks the
/// char — e.g. MesloLGS NF Bold has no box drawing although Regular does — and an
/// advance wider than ~1.5 cells means a double-width glyph that would shift the
/// row if laid out inline. Both take the `Overdraw` route (blanked in the row,
/// overdrawn at the exact cell origin from a buffer shaped with font fallback,
/// which finds the regular face or another font that has it).
fn probe_route(font_system: &mut FontSystem, probe: &mut Buffer, family: &str, c: char, shape: u8, cell_w: f32) -> CellRoute {
    let mut tmp = [0u8; 4];
    let s = c.encode_utf8(&mut tmp);
    probe.set_text(font_system, s, &face_attrs(family, shape), Shaping::Basic, None);
    probe
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .next()
        .map(|g| if g.glyph_id == 0 || g.w > cell_w * 1.5 { CellRoute::Overdraw } else { CellRoute::Inline })
        // No glyph laid out at all (e.g. zero-width/control) — leave it inline for
        // the main grid; don't try to overdraw.
        .unwrap_or(CellRoute::Inline)
}

/// A shaped overdraw glyph (a fallback char, a grapheme cluster or a color
/// emoji) and the x offset that centres it in its cells (0 except for emoji).
struct OverdrawGlyph {
    buffer: Buffer,
    dx: f32,
}

/// Extra per-frame inputs for [`TextLayer::render_grid`]. `Default` is the plain
/// grid: no block-cursor recolor, selected glyphs keep their colors, no graphemes.
#[derive(Debug, Clone, Copy, Default)]
pub struct GridPaint<'a> {
    /// `(row, col, color)` of the glyph under a SOLID block cursor. The block is a
    /// quad painted UNDER the text (see `cursor::cursor_draw`), so the glyph
    /// stays visible on top of it in this contrast color.
    pub cursor_glyph: Option<(usize, usize, [u8; 3])>,
    /// Selected-glyph coloring (see [`SelectionPaint`]); `None` = unchanged colors.
    pub selection: Option<SelectionPaint>,
    /// Sparse grapheme-cluster overrides `(row, col, cluster)`: a cell whose base
    /// char carries combining marks / VS16 / ZWJ parts is drawn from the whole
    /// cluster (shaped with font fallback, at the cell origin) instead of the bare
    /// base char. Empty — the common case — costs nothing.
    pub graphemes: &'a [(usize, usize, &'a str)],
    /// Glyph recolor spans `(row, first col, last col, color)`, sorted by
    /// `(row, first col)` and non-overlapping: every glyph inside takes `color`
    /// (the CURRENT search match, drawn on a strong fill — see
    /// [`crate::search_recolor_spans`]). Concealed text stays concealed. Applied
    /// after the selection colors, before the block-cursor glyph. Empty — the
    /// common case — costs one compare per cell.
    pub recolor: &'a [(usize, usize, usize, [u8; 3])],
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
    left_bits: u32,
    top_bits: u32,
    rows: usize,
    cols: usize,
    keys: Vec<u64>,
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    /// `(x, y, index into clusters, fg, half)` per cluster-drawn cell.
    graphemes: Vec<GraphemeCell>,
    /// The distinct (clamped) clusters those cells draw — bounded by
    /// `GRAPHEME_GLYPH_CAP` × `MAX_CLUSTER_BYTES`.
    clusters: Vec<Box<str>>,
    /// The built-in glyph cells (their row keys hold a blank).
    builtin: Vec<BuiltinCell>,
}

/// One cluster-drawn cell: `(x, y, index into the frame's clusters, fg, half)`.
/// `half` = a color emoji squeezed into ONE cell (a narrow base + VS16 with a
/// non-blank neighbour), drawn at half scale; otherwise an emoji spans two cells.
type GraphemeCell = (f32, f32, u32, [u8; 3], bool);

/// The default terminal font. Matches the user's Konsole profile: MesloLGS NF
/// — a Nerd Font, so the zsh prompt's powerline/icon glyphs render correctly.
const FONT_FAMILY_DEFAULT: &str = "MesloLGS NF";

/// Line height as a multiple of the font size: the default (the long-standing
/// 1.3) and the range the `line_height` config key is clamped to. Only the
/// terminal grid layer changes it (`TextLayer::set_line_height`); chrome
/// layers keep the default. cosmic-text centres the glyphs in the taller line.
pub const LINE_HEIGHT_DEFAULT: f32 = 1.3;
pub const LINE_HEIGHT_MIN: f32 = 1.0;
pub const LINE_HEIGHT_MAX: f32 = 2.0;

/// `mult` clamped to `LINE_HEIGHT_MIN..=LINE_HEIGHT_MAX`; non-finite → the
/// default.
pub fn clamp_line_height(mult: f32) -> f32 {
    if mult.is_finite() { mult.clamp(LINE_HEIGHT_MIN, LINE_HEIGHT_MAX) } else { LINE_HEIGHT_DEFAULT }
}

/// The whole-pixel size a layer rasterizes a requested physical `size` at.
///
/// cosmic-text 0.18's monospace snap (shape.rs, active on every grid row via
/// `Buffer::set_monospace_width`) rounds `advance_px / (cell_w / font_size)`
/// to an integer — a px length divided by an EM ratio — which is the identity
/// only while the font size itself is whole. At a fractional physical size
/// (13 pt × 1.25 = 16.25 px) every advance snapped from 9.78 to 9.63 px, so
/// column 99 drew 15 px left of its cell; at 22.5 px the snap rounded the
/// other way (+30 px). Rounding the size (≤ 0.5 px) keeps every advance
/// exactly `cell_w`. Non-finite input falls back to 16 px.
fn rounded_font_px(size: f32) -> f32 {
    if size.is_finite() { size.round().max(1.0) } else { 16.0 }
}

/// A layer's metrics: font `px` and a line box `line_height` × px tall, rounded
/// UP to whole pixels so rows stay pixel-aligned (`(px * 1.3).ceil()` at the
/// default — the historical cell height).
fn layer_metrics(px: f32, line_height: f32) -> Metrics {
    Metrics::new(px, (px * line_height).ceil())
}

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
    /// A built-in glyph (`builtin.rs` slot): blank in the row, drawn as a
    /// cell-exact custom glyph in the cell's fg (box drawing, blocks, Powerline,
    /// braille, sextants — `builtin_glyphs`).
    Builtin(u16),
    /// Draws nothing at all (the blank braille pattern U+2800).
    Blank,
    /// A color emoji (`color_emoji`): blank in the row, overdrawn from the emoji
    /// font, scaled and centred across its two cells.
    Emoji,
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
/// without a font; the renderer caches shaped [`OverdrawGlyph`]s.
struct ClusterGlyphCache<V = OverdrawGlyph> {
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

impl ClusterGlyphCache<OverdrawGlyph> {
    /// Shape every cluster of this frame not cached yet (`Shaping::Advanced`, so
    /// font fallback supplies marks/emoji the primary font lacks). With an emoji
    /// family, emoji clusters (`emoji::is_emoji_cluster`) are shaped in it, sized to
    /// two cells.
    fn ensure(&mut self, font_system: &mut FontSystem, style: &OverdrawStyle, clusters: &[&str]) {
        let attrs = Attrs::new().family(Family::Name(&style.family));
        self.ensure_with(clusters, |cluster| match &style.emoji_family {
            Some(fam) if emoji::is_emoji_cluster(cluster) => style.shape_emoji(font_system, fam, cluster),
            _ => {
                let mut buffer = Buffer::new(font_system, style.metrics);
                buffer.set_size(font_system, None, None);
                buffer.set_text(font_system, cluster, &attrs, Shaping::Advanced, None);
                OverdrawGlyph { buffer, dx: 0.0 }
            }
        });
    }
}

/// What the overdraw / cluster / emoji buffers are shaped with: the grid font and
/// metrics, the cell box an emoji is fitted into, and the emoji family (`None`
/// when `color_emoji` is off or no emoji font is installed).
struct OverdrawStyle {
    family: Arc<str>,
    emoji_family: Option<Arc<str>>,
    metrics: Metrics,
    cell_w: f32,
    cell_h: f32,
}

impl OverdrawStyle {
    /// Shape `text` in the emoji family, scaled so the emoji fills its two-cell
    /// box (emoji are about as tall as they are wide, so the box's smaller side
    /// bounds the advance) and centred in it. The line keeps the cell height, so
    /// cosmic-text centres the glyph vertically in the cell.
    fn shape_emoji(&self, font_system: &mut FontSystem, family: &str, text: &str) -> OverdrawGlyph {
        let attrs = Attrs::new().family(Family::Name(family));
        let mut buffer = Buffer::new(font_system, self.metrics);
        buffer.set_size(font_system, None, None);
        buffer.set_text(font_system, text, &attrs, Shaping::Advanced, None);
        let box_w = 2.0 * self.cell_w;
        let advance = |b: &Buffer| b.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max);
        let natural = advance(&buffer);
        let mut dx = 0.0;
        if natural > 0.0 {
            let k = (box_w.min(self.cell_h) / natural).clamp(0.5, 1.25);
            let metrics = Metrics::new(self.metrics.font_size * k, self.metrics.line_height);
            buffer.set_metrics(font_system, metrics);
            dx = ((box_w - advance(&buffer)) / 2.0).max(0.0);
        }
        OverdrawGlyph { buffer, dx }
    }
}

/// Most (char, style) routes `TextLayer::glyph_route` keeps: far above what real
/// text shows (every CJK ideograph in all four BOLD|ITALIC styles fits), while a
/// program cycling through all of Unicode can no longer grow it toward the
/// ~4.4 M possible keys (tens of MB).
const GLYPH_ROUTE_CAP: usize = 1 << 17;

/// Insert into a bounded cache map; a full map starts over. Every entry of such a
/// cache is a pure function of its key, so dropping them costs only re-deriving
/// what is on screen.
fn insert_bounded<K: std::hash::Hash + Eq, V>(map: &mut FxHashMap<K, V>, key: K, value: V, cap: usize) {
    if map.len() >= cap {
        map.clear();
    }
    map.insert(key, value);
}

/// Entries per family in [`OverlayCache`] before it evicts: far above the
/// distinct labels of any one frame (a tab bar is ~20, the Settings panel a few
/// hundred), and each entry is bounded (`MAX_LABEL_CHARS`).
const OVERLAY_CACHE_CAP: usize = 512;
/// Once over the cap, entries not drawn by the last this-many overlay passes go.
const OVERLAY_CACHE_KEEP_PASSES: u64 = 32;

/// Shaped chrome-label buffers (`render_overlays*`), cached by content per
/// family — `[mono, title]` — so a label that is the same as last frame (tab
/// titles, ×, +, the window controls, menu rows) is never shaped again. Every
/// cached buffer is shaped at the layer's current metrics: a family or size
/// change clears the cache (`clear_measure_caches`).
#[derive(Default)]
struct OverlayCache {
    maps: [FxHashMap<Box<str>, OverlayEntry>; 2],
    /// Counts overlay passes (`next_frame`): the LRU clock.
    pass: u64,
}

struct OverlayEntry {
    buffer: Buffer,
    /// The layout height the buffer was sized for (the target's height).
    height: u32,
    /// [`OverlayCache::pass`] of the last pass that drew it.
    last_used: u64,
}

impl OverlayCache {
    /// Start an overlay pass; returns its clock value.
    fn next_frame(&mut self) -> u64 {
        self.pass = self.pass.wrapping_add(1);
        self.pass
    }

    /// Make sure `text` (in the `title` family) has a buffer — `make` shapes a
    /// new one on a miss — sized for `height`, and stamp it used by `pass`.
    fn ensure(
        &mut self,
        title: bool,
        text: &str,
        height: u32,
        pass: u64,
        font_system: &mut FontSystem,
        make: impl FnOnce(&mut FontSystem) -> Buffer,
    ) {
        let map = &mut self.maps[title as usize];
        match map.get_mut(text) {
            Some(e) => {
                if e.height != height {
                    e.buffer.set_size(font_system, None, Some(height as f32));
                    e.height = height;
                }
                e.last_used = pass;
            }
            None => {
                map.insert(Box::from(text), OverlayEntry { buffer: make(font_system), height, last_used: pass });
            }
        }
    }

    fn get(&self, title: bool, text: &str) -> Option<&Buffer> {
        self.maps[title as usize].get(text).map(|e| &e.buffer)
    }

    /// Over the cap: drop what the last [`OVERLAY_CACHE_KEEP_PASSES`] passes
    /// didn't draw — and, should that not be enough, all but this pass's labels.
    fn evict(&mut self, pass: u64) {
        for map in &mut self.maps {
            if map.len() > OVERLAY_CACHE_CAP {
                map.retain(|_, e| e.last_used.wrapping_add(OVERLAY_CACHE_KEEP_PASSES) >= pass);
            }
            if map.len() > OVERLAY_CACHE_CAP {
                map.retain(|_, e| e.last_used == pass);
            }
        }
    }

    fn clear(&mut self) {
        for map in &mut self.maps {
            map.clear();
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.maps.iter().map(|m| m.len()).sum()
    }
}

/// Scratch vectors for [`pack_grid`], reused frame to frame so the frame path
/// does not reallocate.
#[derive(Default)]
struct PackScratch {
    keys: Vec<u64>,
    row_hashes: Vec<u64>,
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    graphemes: Vec<GraphemeCell>,
    builtin: Vec<BuiltinCell>,
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
    /// double-width, or a color emoji).
    fallback: Vec<(f32, f32, char, [u8; 3])>,
    /// One entry per cell drawn from its grapheme cluster (see [`GraphemeCell`]).
    graphemes: Vec<GraphemeCell>,
    /// The DISTINCT clusters drawn this frame, each clamped by `clamp_cluster`; at
    /// most `GRAPHEME_GLYPH_CAP` of them, `GRAPHEME_FRAME_CHARS` chars across all
    /// cells.
    clusters: Vec<&'a str>,
    /// Cells drawn as built-in glyphs (their row keys hold a blank, so a change of
    /// box/braille glyph alone never re-shapes a row).
    builtin: Vec<BuiltinCell>,
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
            builtin: self.builtin,
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
///
/// `color_emoji`: emoji clusters (VS16 / ZWJ, see `emoji::is_emoji_cluster`) are
/// drawn as color emoji — sized to two cells, or squeezed into one (`half`) when
/// a narrow base char's right neighbour is not blank.
fn pack_grid<'a>(
    snapshot: &GridSnapshot,
    paint: &GridPaint<'a>,
    cell_w: f32,
    cell_h: f32,
    route: &mut dyn FnMut(char, u8) -> CellRoute,
    color_emoji: bool,
    scratch: PackScratch,
) -> PackedGrid<'a> {
    let (rows, cols) = (snapshot.rows, snapshot.cols);
    let PackScratch { mut keys, mut row_hashes, mut fallback, mut graphemes, mut builtin, mut order } = scratch;
    keys.clear();
    keys.reserve(rows * cols);
    row_hashes.clear();
    fallback.clear();
    graphemes.clear();
    builtin.clear();
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
    // Recolor spans, walked with a cursor alongside the cell loop like the
    // grapheme overrides (sorted by the caller; empty = one compare per cell).
    let recolor = paint.recolor;
    let mut r_next = 0usize;
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
            if r_next < recolor.len() {
                // Drop spans that end before this cell, then test the next one.
                while r_next < recolor.len() && (recolor[r_next].0, recolor[r_next].2) < (row, col) {
                    r_next += 1;
                }
                if let Some(&(rr, c0, c1, color)) = recolor.get(r_next) {
                    if rr == row && c0 <= col && col <= c1 && cell.fg != cell.bg {
                        fg = color;
                    }
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
                    // An emoji on a NARROW base (`❤️`: VS16 does not widen the
                    // cell) spans two cells only when its right neighbour is blank;
                    // otherwise it is squeezed into its own cell.
                    let half = color_emoji
                        && ch.width().unwrap_or(1) < 2
                        && emoji::is_emoji_cluster(clusters[ci as usize])
                        && (col + 1 >= cols || snapshot.cell(row, col + 1).c != ' ');
                    graphemes.push((col as f32 * cell_w, row as f32 * cell_h, ci, fg, half));
                    ch = ' ';
                    drawn_as_cluster = true;
                }
            }
            if !drawn_as_cluster && !ch.is_ascii() {
                // ASCII (incl. blank) cells skip the (cached) probe entirely — the
                // primary font always lays them out inline.
                match route(ch, cell.shape_bits()) {
                    CellRoute::Inline => {}
                    CellRoute::Overdraw | CellRoute::Emoji => {
                        // A glyph the primary font lacks (tofu under Shaping::Basic,
                        // no fallback), draws double-width (a CJK glyph advances ~2
                        // cells and would shift the rest of the row) or a color
                        // emoji: blank it here so the row stays on the grid,
                        // overdraw the real glyph on top, aligned.
                        fallback.push((col as f32 * cell_w, row as f32 * cell_h, ch, fg));
                        ch = ' ';
                    }
                    CellRoute::Builtin(slot) => {
                        builtin.push((row as u16, col as u16, slot, fg));
                        ch = ' ';
                    }
                    CellRoute::Blank => ch = ' ',
                }
            }
            inked |= ch != ' ';
            let k = pack_cell(ch, fg, cell.shape_bits());
            h.write_u64(k);
            keys.push(k);
        }
        // 0 marks an all-blank row: no glyphs to shape or draw.
        row_hashes.push(if inked { h.finish() | 1 } else { 0 });
    }
    PackedGrid { keys, row_hashes, fallback, graphemes, clusters, builtin, deco: deco_hasher.finish(), order }
}

/// A render pass over `view` that keeps its contents (`LoadOp::Load`).
fn load_pass<'e>(encoder: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &'static str) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
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
    /// Line height as a multiple of the font size (`LINE_HEIGHT_DEFAULT` unless
    /// `set_line_height` changed it — the grid layer's `line_height` key).
    line_height: f32,
    cell_w: f32,
    cell_h: f32,
    /// Shaped overlay-label buffers, cached by content (see `OverlayCache`).
    overlays: OverlayCache,
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
    /// Per-(char, BOLD|ITALIC) routing cache for the PRIMARY terminal font
    /// (`font_family`, keyed by `route_key`): does the char lay out inline in that
    /// face, is it a built-in glyph or a color emoji, or must it be blanked and
    /// overdrawn (missing glyph — e.g. Claude Code's `⏵⏵` U+23F5, or box drawing in
    /// MesloLGS NF Bold — OR a double-width CJK glyph)? Probed lazily on the hot
    /// path (only non-ASCII, on miss) and read every frame. Cleared when
    /// `font_family` changes (routing is per-font).
    glyph_route: FxHashMap<u32, CellRoute>,
    /// Scratch buffer used only to probe glyph coverage/advance (shape one char,
    /// inspect the resulting glyph id and width). Reused across frames.
    coverage_buffer: Buffer,
    /// Shaped single-glyph buffers for the overdraw path, keyed by char so a char
    /// repeated across the grid shares one shaped buffer and an unchanged frame
    /// re-shapes nothing. Shaped with `Shaping::Advanced` so cosmic-text's font
    /// fallback supplies a glyph the primary font lacks (or the primary font's own
    /// double-width glyph). Cleared on `set_font_family`/`set_font_size` (glyphs are
    /// per family + size).
    fallback_glyphs: std::collections::HashMap<char, OverdrawGlyph>,
    /// Insertion order of `fallback_glyphs` keys, used to evict the oldest
    /// entries once the cache exceeds `FALLBACK_GLYPH_CAP` so a session scrolling
    /// through a large CJK/emoji corpus can't accumulate shaped buffers without
    /// bound (F25). Chars visible in the current frame are never evicted.
    fallback_order: std::collections::VecDeque<char>,
    /// Monotonic counter bumped whenever a change invalidates shaped grid content
    /// (font family, font size, resize): it drops the row cache and forces the next
    /// frame to re-prepare even when the grid text/colors are unchanged.
    shape_gen: u64,
    /// Cached underline/strikethrough quads (placed at the grid origin) and the
    /// key they were built for: `(grid_decoration_key, cell_w bits, cell_h bits,
    /// origin left bits, origin top bits, cols, rows)`. Rebuilt
    /// only when that changes, so a caret-flash / CRT / scrollbar-only animate
    /// frame (same grid) reuses them — decorations never rebuild per frame; only
    /// the CURSOR quads do (drawn app-side). Consumed via `decoration_rects()`.
    deco_rects: Vec<crate::quad::Rect>,
    deco_cache_key: Option<(u64, u32, u32, u32, u32, u32, u32)>,
    /// Chrome text measurement (`ChromeMeasure`): rendered width per label for
    /// non-title [0] and tab-title [1] chrome. Chrome labels are mostly static
    /// strings rebuilt every rendered frame, so a hit is one hash lookup and a
    /// miss shapes once. Keys are CLIPPED labels and the caches are BYTE-
    /// budgeted (`MeasureCache`), so program-controlled text (huge OSC titles)
    /// can't grow them. Cleared whenever the chrome family/size changes.
    measure_cache: [crate::chrome::MeasureCache<f32>; 2],
    /// Per-char boundary x offsets (`ChromeMeasure::char_xs`) for the few
    /// labels that need them (truncation fits, carets, fuzzy highlights).
    xs_cache: [crate::chrome::MeasureCache<Vec<f32>>; 2],
    /// Scratch buffer the measurement shapes into (no per-call allocation).
    measure_buffer: Buffer,
    /// Draw box drawing / blocks / Powerline / braille / sextants as built-in
    /// cell-exact glyphs (`builtin_glyphs`, default on) instead of from the font.
    builtin_glyphs: bool,
    /// Draw emoji in color from the installed emoji font (`color_emoji`, default
    /// on). Inert without an emoji font.
    color_emoji: bool,
    /// The emoji family — the first installed one whose name contains "Emoji"
    /// (a "Color" one preferred). Looked up once, on the first emoji drawn.
    emoji_family: Option<Option<Arc<str>>>,
    /// Light line thickness (px) of the built-in glyphs at the current font size.
    builtin_light: u16,
    /// Where a row's underlines go, measured from the grid font (see
    /// `measure_underline`).
    underline: crate::quad::UnderlineGeom,
    /// Per-frame scratch: the built-in glyphs handed to glyphon.
    custom_scratch: Vec<CustomGlyph>,
    /// The (empty) buffer of the text area that carries the built-in glyphs.
    empty_buffer: Buffer,
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

        // Whole-px size (see `rounded_font_px`) at the default line height.
        let metrics = layer_metrics(rounded_font_px(font_size), LINE_HEIGHT_DEFAULT);
        let line_height = metrics.line_height;

        // The cursor is drawn as a QuadLayer rect (see `cursor::cursor_draw`),
        // not a text-atlas block glyph, so there is no cursor buffer to build here.
        // Grid rows get their own buffers lazily (see `new_row_buffer`).

        // Scratch buffer for glyph-coverage probing (see `covers`).
        let mut coverage_buffer = Buffer::new(&mut font_system, metrics);
        coverage_buffer.set_size(&mut font_system, None, None);

        // Chrome-measurement scratch buffer (see `ChromeMeasure for TextLayer`).
        let mut measure_buffer = Buffer::new(&mut font_system, metrics);
        measure_buffer.set_size(&mut font_system, None, None);

        // Measure a monospace cell by shaping a single 'M'.
        let cell_w = measure_advance_family(&mut font_system, metrics, family);
        let underline = measure_underline(&mut font_system, metrics, family);
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
            line_height: LINE_HEIGHT_DEFAULT,
            cell_w,
            cell_h,
            overlays: OverlayCache::default(),
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
            measure_cache: Default::default(),
            xs_cache: Default::default(),
            measure_buffer,
            builtin_glyphs: true,
            color_emoji: true,
            emoji_family: None,
            builtin_light: builtin::light_thickness(metrics.font_size),
            underline,
            custom_scratch: Vec::new(),
            empty_buffer: Buffer::new_empty(metrics),
        }
    }

    /// Where a grid row's underlines go (px from the row's top), measured from
    /// the grid font: single / dotted / dashed strokes at the font's underline
    /// position below the baseline, the undercurl resting on the bottom of the
    /// line box, a thickness from the font size — so a taller `line_height`
    /// keeps them with the text instead of at the cell edge. Pass it to
    /// `link_underline_rects_at` so link underlines match.
    pub fn underline_geom(&self) -> crate::quad::UnderlineGeom {
        self.underline
    }

    /// Re-measure `underline` (font family / size / line height changed) and
    /// drop the cached decoration quads built with the old one.
    fn refresh_underline(&mut self) {
        let fam = Arc::clone(&self.font_family);
        self.underline = measure_underline(&mut self.font_system, self.metrics, &fam);
        self.deco_cache_key = None;
    }

    /// Draw box drawing (U+2500–257F), block elements, Powerline separators,
    /// braille and sextants as built-in cell-exact glyphs (`true`, the default) or
    /// from the font. Takes effect on the next frame (re-shapes the grid).
    pub fn set_builtin_glyphs(&mut self, on: bool) {
        if self.builtin_glyphs != on {
            self.builtin_glyphs = on;
            self.invalidate_routing();
        }
    }

    /// Draw emoji in color from the installed emoji font (`true`, the default) or
    /// like any other glyph the primary font lacks (font fallback, often a
    /// monochrome outline). Takes effect on the next frame.
    pub fn set_color_emoji(&mut self, on: bool) {
        if self.color_emoji != on {
            self.color_emoji = on;
            self.invalidate_routing();
        }
    }

    /// Routing (and everything shaped from it) changed: re-probe every char,
    /// re-shape every overdraw glyph and grid row, re-prepare.
    fn invalidate_routing(&mut self) {
        self.glyph_route.clear();
        self.fallback_glyphs.clear();
        self.fallback_order.clear();
        self.clusters.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
        self.prepared = None;
    }

    /// The emoji family when color emoji are on and one is installed (looked up
    /// once — see `emoji_family`).
    fn active_emoji_family(&mut self) -> Option<Arc<str>> {
        if !self.color_emoji {
            return None;
        }
        if self.emoji_family.is_none() {
            self.emoji_family = Some(find_emoji_family(self.font_system.db()).map(Arc::from));
        }
        self.emoji_family.clone().flatten()
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
        grid_row_buffer(&mut self.font_system, self.metrics, self.cell_w)
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
        self.refresh_underline();
        // At the default UI family, non-title chrome renders in THIS family, so
        // every cached chrome width is stale.
        self.clear_measure_caches();
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
        self.clear_measure_caches();
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
        // Whole-px size (see `rounded_font_px`) at this layer's line height.
        self.metrics = layer_metrics(rounded_font_px(font_size), self.line_height);
        let line_height = self.metrics.line_height;
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
        self.builtin_light = builtin::light_thickness(self.metrics.font_size);
        self.refresh_underline();
        // Cached fallback/grapheme glyphs were shaped at the old size; drop them.
        // The shape-gen bump drops every cached grid row too, so rows are rebuilt
        // at the new metrics with the new cell width as their monospace snap
        // (keeps bold/italic aligned after a font-size / DPI change). On a chrome
        // layer the grid rows are never built — chrome renders via `overlays`.
        self.fallback_glyphs.clear();
        self.fallback_order.clear();
        self.clusters.clear();
        self.shape_gen = self.shape_gen.wrapping_add(1);
        self.clear_measure_caches();
    }

    /// Set the line height — a multiple of the font size, clamped by
    /// [`clamp_line_height`] (the grid layer's `line_height` key). The cell
    /// height becomes `ceil(font px × mult)` and cosmic-text centres each glyph
    /// in the taller line; everything else is re-derived exactly like a size
    /// change (rows re-shape). The caller reflows + repaints. Returns whether
    /// anything changed (a no-op keeps every cache).
    pub fn set_line_height(&mut self, mult: f32) -> bool {
        let mult = clamp_line_height(mult);
        if mult == self.line_height {
            return false;
        }
        self.line_height = mult;
        self.set_font_size(self.metrics.font_size);
        true
    }

    /// The line-height multiple this layer lays rows out with.
    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    /// Drop every cached chrome measurement and shaped label (family or size
    /// changed).
    fn clear_measure_caches(&mut self) {
        for c in &mut self.measure_cache {
            c.clear();
        }
        for c in &mut self.xs_cache {
            c.clear();
        }
        self.overlays.clear();
    }

    /// Shape `s` as chrome (`title` = tab-title family) into the scratch buffer
    /// and write every char boundary's x offset into `out` — the SAME family,
    /// metrics and `Shaping::Advanced` (font fallback included) as
    /// `render_overlays_inner`, so the numbers match the drawn glyphs.
    fn shape_char_xs(&mut self, s: &str, title: bool, out: &mut Vec<f32>) {
        out.clear();
        out.push(0.0);
        if s.is_empty() {
            return;
        }
        let ui_family = self.ui_family.clone();
        let mono_fallback = self.font_family.clone();
        let metrics = self.metrics;
        let attrs = Attrs::new().family(ui_family.as_family(title, &mono_fallback));
        let buf = &mut self.measure_buffer;
        buf.set_metrics(&mut self.font_system, metrics);
        buf.set_size(&mut self.font_system, None, Some(metrics.line_height));
        buf.set_text(&mut self.font_system, s, &attrs, Shaping::Advanced, None);
        // Byte offset of each char boundary (the last one = len) → boundary x =
        // the x of the first glyph whose cluster starts at/after it, or the full
        // width past the last glyph. Chrome labels are single-line LTR runs.
        let mut starts: Vec<(usize, f32)> = Vec::new();
        let mut full = 0.0f32;
        for run in buf.layout_runs() {
            for g in run.glyphs.iter() {
                starts.push((g.start, g.x));
                full = full.max(g.x + g.w);
            }
        }
        starts.sort_by_key(|&(b, _)| b);
        let mut gi = 0usize;
        for (byte, _) in s.char_indices().skip(1).chain(std::iter::once((s.len(), ' '))) {
            while gi < starts.len() && starts[gi].0 < byte {
                gi += 1;
            }
            let x = if byte == s.len() {
                full
            } else {
                starts.get(gi).map(|&(_, x)| x).unwrap_or(full)
            };
            // Non-decreasing even across ligature / combining clusters.
            let prev = *out.last().unwrap_or(&0.0);
            out.push(x.max(prev));
        }
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

    /// The cached underline/strikethrough quads for the last rendered frame,
    /// already PLACED at the grid origin passed to `prepare_grid` (window
    /// coordinates — unlike the grid-space quad builders, do not shift them).
    /// The caller appends these to its Pass-4 quad batch (they draw over the
    /// glyphs, under the cursor).
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

    /// How the primary terminal font must render `c` in a cell of style `shape`
    /// (`BOLD|ITALIC` bits) on the grid (see `CellRoute`). ASCII is always
    /// `Inline`. Other chars are classified once per (char, style) and cached:
    /// built-in glyphs and color emoji first (style-independent), else the FACE
    /// the row shapes the cell with is probed (see [`probe_route`]).
    fn route(&mut self, c: char, shape: u8) -> CellRoute {
        if (c as u32) < 0x80 {
            return CellRoute::Inline;
        }
        let key = route_key(c, shape);
        if let Some(&v) = self.glyph_route.get(&key) {
            return v;
        }
        let route = self.classify(c, shape);
        insert_bounded(&mut self.glyph_route, key, route, GLYPH_ROUTE_CAP);
        route
    }

    /// The uncached half of [`Self::route`]: built-in glyph, color emoji, or a
    /// probe of the primary font's face for `shape`.
    fn classify(&mut self, c: char, shape: u8) -> CellRoute {
        if self.builtin_glyphs {
            if builtin::is_blank(c) {
                return CellRoute::Blank;
            }
            if let Some(slot) = builtin::slot(c) {
                return CellRoute::Builtin(slot);
            }
        }
        if is_color_emoji_char(c) && self.active_emoji_family().is_some() {
            return CellRoute::Emoji;
        }
        let fam = Arc::clone(&self.font_family);
        probe_route(&mut self.font_system, &mut self.coverage_buffer, &fam, c, shape, self.cell_w)
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
        let origin = crate::GridOrigin::new(0.0, top_offset);
        self.render_grid(device, queue, view, width, height, snapshot, clear, origin, &GridPaint::default())
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
        origin: crate::GridOrigin,
        paint: &GridPaint,
    ) -> Result<(), PrepareError> {
        self.prepare_grid(device, queue, width, height, snapshot, origin, paint)?;
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
    ///
    /// `origin` is where cell (0, 0) sits in the window (see `grid_geom`): the
    /// glyphs AND the cached decoration quads are placed there.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_grid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        snapshot: &GridSnapshot,
        origin: crate::GridOrigin,
        paint: &GridPaint,
    ) -> Result<(), PrepareError> {
        let (left_offset, top_offset) = (origin.left, origin.top);
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let (rows, cols) = (snapshot.rows, snapshot.cols);

        // ---- 1. Pack every cell (pure; see `pack_grid`): keys, row hashes, the
        // overdraw cells, the built-in glyph cells and the budgeted grapheme
        // clusters, plus the decoration fingerprint.
        let scratch = std::mem::take(&mut self.pack_scratch);
        // Only clusters read it — the emoji font lookup (a font-database scan)
        // waits for the first frame that has an emoji or a VS16/ZWJ cluster.
        let color_emoji = self.color_emoji && !paint.graphemes.is_empty() && self.active_emoji_family().is_some();
        let packed = pack_grid(snapshot, paint, cell_w, cell_h, &mut |c, s| self.route(c, s), color_emoji, scratch);

        // ---- 2. Underline/strike quads: rebuilt only when their content, the cell
        // metrics or the grid offset changed. Grid dims are part of the key:
        // fold_decoration folds cells positionally by LINEAR index, so a
        // cell-count-preserving reflow (e.g. 80x24 -> 60x32) can fold identically
        // yet needs different rects.
        let deco_key = (
            packed.deco,
            cell_w.to_bits(),
            cell_h.to_bits(),
            left_offset.to_bits(),
            top_offset.to_bits(),
            cols as u32,
            rows as u32,
        );
        if self.deco_cache_key != Some(deco_key) {
            self.deco_rects.clear();
            // Built in grid space (x from the grid's left edge), then placed.
            // Underlines are placed by the font (`underline_geom`).
            crate::quad::text_decoration_rects_at(
                snapshot,
                cell_w,
                cell_h,
                self.underline,
                top_offset,
                &mut self.deco_rects,
            );
            crate::grid_geom::shift_x(&mut self.deco_rects, left_offset);
            self.deco_cache_key = Some(deco_key);
        }

        self.viewport.update(queue, Resolution { width, height });

        // ---- 3. Identical to the last successful prepare? Then glyphon's vertex
        // buffer already holds exactly this frame's glyphs: no shaping, no prepare.
        let unchanged = self.prepared.as_ref().is_some_and(|p| {
            p.shape_gen == self.shape_gen
                && p.width == width
                && p.height == height
                && p.left_bits == left_offset.to_bits()
                && p.top_bits == top_offset.to_bits()
                && p.rows == rows
                && p.cols == cols
                && p.keys == packed.keys
                && p.fallback == packed.fallback
                && p.graphemes == packed.graphemes
                && p.builtin == packed.builtin
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

            // Built-in glyphs: one custom glyph per cell, spanning exactly the
            // pixels of the cell's background quad (`px_edge` of the same window
            // coordinates the quads use: the grid origin + col·cell_w), in the
            // cell's fg.
            let mut custom = std::mem::take(&mut self.custom_scratch);
            custom.clear();
            let light = self.builtin_light;
            for &(r, c, slot, fg) in &packed.builtin {
                let x0 = px_edge(left_offset + c as f32 * cell_w);
                let x1 = px_edge(left_offset + (c as f32 + 1.0) * cell_w);
                let y0 = px_edge(top_offset + r as f32 * cell_h);
                let y1 = px_edge(top_offset + (r as f32 + 1.0) * cell_h);
                custom.push(CustomGlyph {
                    id: builtin::glyph_id(slot, light),
                    left: x0,
                    top: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                    color: Some(Color::rgb(fg[0], fg[1], fg[2])),
                    snap_to_physical_pixel: true,
                    metadata: 0,
                });
            }

            let win_bounds = TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            let default_color = Color::rgb(220, 220, 220);
            let mut areas: Vec<TextArea> =
                Vec::with_capacity(rows + 1 + packed.fallback.len() + packed.graphemes.len());
            if !custom.is_empty() {
                areas.push(TextArea {
                    buffer: &self.empty_buffer,
                    left: 0.0,
                    top: 0.0,
                    scale: 1.0,
                    bounds: win_bounds,
                    default_color,
                    custom_glyphs: &custom,
                });
            }
            for (r, &slot) in slots.iter().enumerate() {
                if slot != NO_ROW {
                    areas.push(TextArea {
                        buffer: &self.row_cache[slot as usize].buffer,
                        left: left_offset,
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
            for &(x, y, c, rgb) in packed.fallback.iter() {
                if let Some(g) = self.fallback_glyphs.get(&c) {
                    areas.push(TextArea {
                        buffer: &g.buffer,
                        left: x + left_offset + g.dx,
                        top: y + top_offset,
                        scale: 1.0,
                        bounds: win_bounds,
                        default_color: Color::rgb(rgb[0], rgb[1], rgb[2]),
                        custom_glyphs: &[],
                    });
                }
            }
            for &(x, y, ci, rgb, half) in packed.graphemes.iter() {
                if let Some(g) = self.clusters.get(packed.clusters[ci as usize]) {
                    // A half-scale emoji keeps its centre: half the offset into its
                    // one cell, and a quarter cell down (its line box is halved).
                    let (scale, dx, dy) = if half { (0.5, g.dx * 0.5, cell_h * 0.25) } else { (1.0, g.dx, 0.0) };
                    areas.push(TextArea {
                        buffer: &g.buffer,
                        left: x + left_offset + dx,
                        top: y + top_offset + dy,
                        scale,
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
            let mut prepared = self.renderer.prepare_with_custom(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas.iter().cloned(),
                &mut self.swash,
                rasterize_builtin,
            );
            if prepared == Err(PrepareError::AtlasFull) {
                self.atlas.trim();
                prepared = self.renderer.prepare_with_custom(
                    device,
                    queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    areas.iter().cloned(),
                    &mut self.swash,
                    rasterize_builtin,
                );
            }
            drop(areas);
            self.custom_scratch = custom;
            self.row_slots_scratch = slots;
            if prepared.is_ok() {
                // Remember exactly what the vertex buffer now holds. The key vectors
                // are swapped, not copied: `packed.keys` takes the previous
                // allocation back as next frame's scratch. Cluster strings are
                // copied: at most GRAPHEME_GLYPH_CAP × MAX_CLUSTER_BYTES.
                p.shape_gen = self.shape_gen;
                p.width = width;
                p.height = height;
                p.left_bits = left_offset.to_bits();
                p.top_bits = top_offset.to_bits();
                p.rows = rows;
                p.cols = cols;
                std::mem::swap(&mut p.keys, &mut packed.keys);
                p.fallback.clear();
                p.fallback.extend_from_slice(&packed.fallback);
                p.graphemes.clear();
                p.graphemes.extend_from_slice(&packed.graphemes);
                std::mem::swap(&mut p.builtin, &mut packed.builtin);
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
                // monospace snap (see `new_row_buffer`). A char that face lacks
                // never gets here: `probe_route` sent it to the overdraw.
                (&text[s..e], face_attrs(&family, shape).color(color))
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
        if fallback_cells.is_empty() && clusters.is_empty() {
            return;
        }
        let style = self.overdraw_style();
        if !fallback_cells.is_empty() {
            let attrs = Attrs::new().family(Family::Name(&style.family));
            for (_x, _y, c, _rgb) in fallback_cells {
                if !self.fallback_glyphs.contains_key(c) {
                    let mut tmp = [0u8; 4];
                    let text = c.encode_utf8(&mut tmp);
                    let glyph = match &style.emoji_family {
                        // Routed as a color emoji (see `classify`): its own font,
                        // fitted to its two cells.
                        Some(fam) if is_color_emoji_char(*c) => {
                            style.shape_emoji(&mut self.font_system, fam, text)
                        }
                        _ => {
                            let mut buffer = Buffer::new(&mut self.font_system, style.metrics);
                            buffer.set_size(&mut self.font_system, None, None);
                            buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
                            OverdrawGlyph { buffer, dx: 0.0 }
                        }
                    };
                    self.fallback_glyphs.insert(*c, glyph);
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
            self.clusters.ensure(&mut self.font_system, &style, clusters);
        }
    }

    /// What the overdraw / cluster / emoji buffers are shaped with right now.
    fn overdraw_style(&mut self) -> OverdrawStyle {
        OverdrawStyle {
            family: Arc::clone(&self.font_family),
            emoji_family: self.active_emoji_family(),
            metrics: self.metrics,
            cell_w: self.cell_w,
            cell_h: self.cell_h,
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
        if !self.prepare_overlay_sets(device, queue, width, height, &[(labels, is_title)], clip_y)? {
            return Ok(());
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("overlay-text"),
        });
        {
            let mut pass = load_pass(&mut encoder, view, "overlay-text-pass");
            self.draw_prepared_overlays(&mut pass);
        }
        queue.submit(Some(encoder.finish()));
        // Unpin this frame's glyphs so the next prepare can LRU-evict (see render_to).
        self.atlas.trim();
        Ok(())
    }

    /// Glyphon-prepare every label of `sets` — `(labels, is_title)` pairs, each
    /// in its family (see `render_overlays_inner`) — for ONE draw
    /// ([`Self::draw_prepared_overlays`]). Returns whether there is anything to
    /// draw. Labels are shaped once and cached by content (`OverlayCache`).
    #[allow(clippy::type_complexity)]
    fn prepare_overlay_sets(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        sets: &[(&[(String, f32, f32, [u8; 3])], bool)],
        clip_y: Option<(i32, i32)>,
    ) -> Result<bool, PrepareError> {
        if sets.iter().all(|(labels, _)| labels.is_empty()) {
            return Ok(false);
        }

        let (clip_top, clip_bottom) = clip_y.unwrap_or((0, height as i32));
        let win_bounds = TextBounds {
            left: 0,
            top: clip_top,
            right: width as i32,
            bottom: clip_bottom,
        };

        // First pass: a shaped buffer per DISTINCT label (shaping needs
        // &mut font_system, so the buffers can't be borrowed as &T yet). The
        // buffers are cached by content (`OverlayCache`): chrome is mostly the
        // same strings every frame — tab titles, ×, +, the window controls — and
        // re-shaping each one per frame (Advanced shaping with font fallback)
        // was most of the chrome's CPU. Clone the chrome family + mono fallback
        // out of self so the `Family::Name` borrow doesn't conflict with the
        // &mut font_system.
        let ui_family = self.ui_family.clone();
        let mono_fallback = self.font_family.clone();
        let metrics = self.metrics;
        let frame = self.overlays.next_frame();
        for &(labels, is_title) in sets {
            for (text, _x, _y, _rgb) in labels {
                // Clipped to MAX_LABEL_CHARS: labels can carry program-controlled
                // text (a multi-MB OSC title), and shaping that whole per frame is
                // a DoS — nothing past the clip could be visible anyway. It also
                // bounds every cached key.
                let (text, _) = crate::chrome::clip_head(text);
                let font_system = &mut self.font_system;
                self.overlays.ensure(is_title, text, height, frame, font_system, |font_system| {
                    let mut buf = Buffer::new(font_system, metrics);
                    buf.set_size(font_system, None, Some(height as f32));
                    // A `Named` UI family unifies ALL chrome onto it; the `Sans`
                    // default keeps today's split (titles → sans, rest → mono Nerd
                    // Font) so the default look — including symbol glyphs — is
                    // byte-identical.
                    let attrs = Attrs::new().family(ui_family.as_family(is_title, &mono_fallback));
                    // Shaping::Advanced: chrome text carries user/shell-controlled
                    // strings (OSC tab titles, search queries, rename buffers), so
                    // it needs cosmic-text's font fallback — under Basic every glyph
                    // the chrome family lacks (emoji, CJK, symbols on a custom UI
                    // font) rendered as a tofu box. Chrome is proportional overlay
                    // text with no grid-alignment constraint, so Advanced is safe.
                    buf.set_text(font_system, text, &attrs, Shaping::Advanced, None);
                    buf
                });
            }
        }
        // Keep the cache bounded; never evicts a label drawn this frame.
        self.overlays.evict(frame);

        // Second pass: build TextAreas with shared refs (no mutation of font_system
        // needed), set by set. Repeated labels (a "×" per tab) share one buffer.
        let mut areas: Vec<TextArea> = Vec::with_capacity(sets.iter().map(|(l, _)| l.len()).sum());
        for &(labels, is_title) in sets {
            for (text, x, y, rgb) in labels {
                let (text, _) = crate::chrome::clip_head(text);
                let Some(buffer) = self.overlays.get(is_title, text) else { continue };
                areas.push(TextArea {
                    buffer,
                    left: *x,
                    top: *y,
                    scale: 1.0,
                    bounds: win_bounds,
                    default_color: Color::rgb(rgb[0], rgb[1], rgb[2]),
                    custom_glyphs: &[],
                });
            }
        }

        self.viewport.update(queue, Resolution { width, height });

        // This prepare overwrites the renderer's vertex buffer (and may LRU-evict
        // grid glyphs from the shared atlas), so the grid must re-prepare next frame.
        self.prepared = None;
        // With the built-in rasterizer: an atlas grow re-rasterizes the custom
        // glyphs a grid prepare put there (see `rasterize_builtin`).
        self.renderer.prepare_with_custom(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
            rasterize_builtin,
        )?;
        Ok(true)
    }

    /// Record the labels of the last [`Self::prepare_overlay_sets`].
    fn draw_prepared_overlays(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Err(e) = self.renderer.render(&self.atlas, &self.viewport, pass) {
            eprintln!("jetty: overlay text render error: {e:?}");
        }
    }

    /// Draw a chrome layer — `quads` under every label of `sets` (`(labels,
    /// is_title)` pairs: each label in the family `render_overlays` (false) or
    /// `render_overlays_sans` (true) uses) — over `view` in ONE render pass and
    /// ONE submit, instead of a pass + submit per layer (each costs tens of µs of
    /// CPU and ~65 allocations). Quads first, then the labels set by set: the
    /// same stacking as drawing them one after another, as long as no quad covers
    /// a label of an earlier set.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub fn render_chrome(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        quad: &mut crate::quad::QuadLayer,
        quads: &[crate::quad::Rect],
        sets: &[(&[(String, f32, f32, [u8; 3])], bool)],
    ) -> Result<(), PrepareError> {
        let quad_count = quad.upload(device, queue, width, height, quads);
        let text = self.prepare_overlay_sets(device, queue, width, height, sets, None);
        let has_text = matches!(text, Ok(true));
        if quad_count == 0 && !has_text {
            return text.map(|_| ());
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("chrome"),
        });
        {
            let mut pass = load_pass(&mut encoder, view, "chrome-pass");
            quad.draw_uploaded(&mut pass, quad_count);
            if has_text {
                self.draw_prepared_overlays(&mut pass);
            }
        }
        queue.submit(Some(encoder.finish()));
        if has_text {
            // Unpin this frame's glyphs so the next prepare can LRU-evict.
            self.atlas.trim();
        }
        text.map(|_| ())
    }

    /// Render NON-TITLE chrome labels (menu, status/perf bar, panel, help,
    /// confirm, welcome, window controls). With a `Named` UI family they render in
    /// it; at the `Sans` default they render in the mono Nerd Font (preserving its
    /// symbol glyphs ⇧ ⌃ ⚡ ⚙ ✕ …), exactly as before this feature. Chrome text
    /// is measured through `ChromeMeasure` (the same shaping as this pass).
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

/// Real chrome text measurement: the layer's own family/size/shaping, cached.
/// Chrome builders take `&mut dyn ChromeMeasure` so a proportional UI font
/// positions carets, highlights, right-aligned values and truncation by what
/// is actually drawn (see `crate::chrome`).
impl crate::chrome::ChromeMeasure for TextLayer {
    /// Clips `s` to `MAX_LABEL_CHARS` first (bounded shaping for program-
    /// controlled text); `out` then covers that clipped prefix.
    fn char_xs(&mut self, s: &str, title: bool, out: &mut Vec<f32>) {
        let (s, _) = crate::chrome::clip_head(s);
        let slot = title as usize;
        if let Some(xs) = self.xs_cache[slot].get(s) {
            out.clear();
            out.extend_from_slice(xs);
            return;
        }
        self.shape_char_xs(s, title, out);
        let bytes = out.len() * std::mem::size_of::<f32>();
        self.xs_cache[slot].insert(s, out.clone(), bytes);
    }

    fn text_w(&mut self, s: &str) -> f32 {
        self.cached_width(s, false)
    }

    fn title_w(&mut self, s: &str) -> f32 {
        self.cached_width(s, true)
    }
}

impl TextLayer {
    /// Cached rendered width of a chrome label (`title` = tab-title family).
    /// Measures at most `MAX_LABEL_CHARS` chars — exactly what the overlay
    /// pass will draw of it (it clips the same way).
    fn cached_width(&mut self, s: &str, title: bool) -> f32 {
        let (s, _) = crate::chrome::clip_head(s);
        if s.is_empty() {
            return 0.0;
        }
        let slot = title as usize;
        if let Some(&w) = self.measure_cache[slot].get(s) {
            return w;
        }
        let mut xs = Vec::new();
        self.shape_char_xs(s, title, &mut xs);
        let w = xs.last().copied().unwrap_or(0.0);
        self.measure_cache[slot].insert(s, w, std::mem::size_of::<f32>());
        w
    }
}

/// An empty single-line grid-row buffer at `metrics`, every glyph advance
/// snapped to `cell_w` (see `TextLayer::new_row_buffer`).
fn grid_row_buffer(font_system: &mut FontSystem, metrics: Metrics, cell_w: f32) -> Buffer {
    let mut b = Buffer::new(font_system, metrics);
    b.set_size(font_system, None, Some(metrics.line_height));
    b.set_monospace_width(font_system, Some(cell_w));
    b
}

/// The emoji font: the first installed family whose name contains "Emoji" (any
/// case), a color one ("Noto Color Emoji") preferred over a monochrome one
/// ("Noto Emoji"); ties broken by name so the pick never depends on scan order.
fn find_emoji_family(db: &glyphon::fontdb::Database) -> Option<String> {
    pick_emoji_family(db.faces().filter_map(|f| f.families.first().map(|(n, _)| n.as_str())))
}

/// [`find_emoji_family`] over a list of family names (pure, for tests).
fn pick_emoji_family<'a>(names: impl Iterator<Item = &'a str>) -> Option<String> {
    names
        .filter(|n| n.to_lowercase().contains("emoji"))
        .min_by_key(|n| (!n.to_lowercase().contains("color"), n.to_string()))
        .map(str::to_string)
}

/// Where a grid row's underlines go for `family` at `metrics` (see
/// [`TextLayer::underline_geom`]), from a row laid out like the grid's (the
/// glyph box centred in the line height):
/// * `thickness` — `round(0.1 · font px)`, at least 1: the historical
///   `round(0.075 · cell_h)` at the default line height, but independent of it;
/// * `bottom` — baseline + descent (the line box bottom), rounded, inside the
///   row: the cell bottom at the default line height;
/// * `top` — the font's underline position (the post table's offset of the
///   stroke's top below the baseline; a fifth of the descent without one),
///   rounded, at least a pixel under the baseline, the stroke above `bottom`.
fn measure_underline(font_system: &mut FontSystem, metrics: Metrics, family: &str) -> crate::quad::UnderlineGeom {
    let lh = metrics.line_height;
    let px = metrics.font_size;
    let thickness = (px * 0.1).round().max(1.0);
    let mut b = Buffer::new(font_system, metrics);
    b.set_size(font_system, None, Some(lh));
    b.set_text(font_system, "M", &Attrs::new().family(Family::Name(family)), Shaping::Basic, None);
    let probe = b.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| (r.line_y - r.line_top, g.font_id, g.font_weight)));
    let descent = b.lines.first().and_then(|l| l.layout_opt()).and_then(|l| l.first()).map(|l| l.max_descent);
    let (Some((baseline, font_id, weight)), Some(descent)) = (probe, descent) else {
        return crate::quad::UnderlineGeom::cell_bottom(lh);
    };
    let below = font_system
        .get_font(font_id, weight)
        .and_then(|f| {
            let m = f.metrics();
            m.underline.map(|u| -u.offset / m.units_per_em.max(1) as f32 * px)
        })
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(descent * 0.2);
    let bottom = (baseline + descent).round().clamp(thickness, lh);
    let top = (baseline + below.max(1.0)).round().clamp(0.0, bottom - thickness);
    crate::quad::UnderlineGeom { top, bottom, thickness }
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
            graphemes: Vec::new(),
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
        let packed = pack_grid(&snap, &paint, 10.0, 20.0, &mut |_, _| CellRoute::Inline, true, PackScratch::default());
        let elapsed = t.elapsed();

        assert!(packed.clusters.len() <= GRAPHEME_GLYPH_CAP, "{} distinct clusters", packed.clusters.len());
        for c in &packed.clusters {
            assert!(c.len() <= MAX_CLUSTER_BYTES && c.chars().count() <= 1 + MAX_CLUSTER_MARKS);
        }
        let drawn_chars: usize = packed
            .graphemes
            .iter()
            .map(|&(_, _, ci, _, _)| packed.clusters[ci as usize].chars().count())
            .sum();
        assert!(drawn_chars <= GRAPHEME_FRAME_CHARS, "{drawn_chars} cluster chars drawn");
        assert!(!packed.graphemes.is_empty(), "clusters within budget are still drawn");
        // Row memory: one word per cell, whatever the clusters weigh.
        assert_eq!(packed.keys.len(), rows * cols);
        // Cluster-drawn cells pack as blanks; cells past the budget keep their base
        // char (graceful: the text stays readable, only the marks are dropped).
        let drawn: HashSet<(u32, u32)> =
            packed.graphemes.iter().map(|&(x, y, _, _, _)| ((x / 10.0) as u32, (y / 20.0) as u32)).collect();
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

    /// The router `TextLayer::classify` applies with built-in glyphs on and an
    /// emoji font installed — minus the font probe (everything else is Inline).
    fn test_route(c: char, _shape: u8) -> CellRoute {
        if builtin::is_blank(c) {
            CellRoute::Blank
        } else if let Some(s) = builtin::slot(c) {
            CellRoute::Builtin(s)
        } else if emoji::is_emoji_presentation(c) && c.width() == Some(2) {
            CellRoute::Emoji
        } else {
            CellRoute::Inline
        }
    }

    /// A one-row grid holding `text` (wide chars followed by their spacer blank).
    fn text_grid(text: &str, cols: usize) -> GridSnapshot {
        let mut g = plain_grid(cols, 1);
        for c in g.cells.iter_mut() {
            c.c = ' ';
        }
        let mut col = 0;
        for ch in text.chars() {
            g.cells[col].c = ch;
            col += ch.width().unwrap_or(1).max(1);
        }
        g
    }

    #[test]
    fn builtin_cells_leave_the_row_text_and_carry_their_fg() {
        let mut g = text_grid("╭─ ok ⣿⠀\u{E0B0}", 12);
        g.cells[0].fg = [1, 2, 3];
        let packed = pack_grid(&g, &GridPaint::default(), 10.0, 20.0, &mut test_route, true, PackScratch::default());
        let row_text: String = packed.keys.iter().map(|&k| unpack_cell(k).0).collect();
        assert_eq!(row_text, "   ok       ", "built-in cells pack as blanks (the font never sees them)");
        // ╭ ─ ⣿  — the blank braille pattern draws nothing at all.
        let cols: Vec<u16> = packed.builtin.iter().map(|b| b.1).collect();
        assert_eq!(cols, vec![0, 1, 6, 8]);
        assert_eq!(packed.builtin[0], (0, 0, builtin::slot('╭').unwrap(), [1, 2, 3]));
        assert!(packed.fallback.is_empty());
        // A row of nothing but built-in glyphs has no text to shape at all.
        let g = text_grid("╰──────╯", 8);
        let packed = pack_grid(&g, &GridPaint::default(), 10.0, 20.0, &mut test_route, true, PackScratch::default());
        assert_eq!(packed.row_hashes, vec![0], "an all-box row is a blank row");
        assert_eq!(packed.builtin.len(), 8);
    }

    #[test]
    fn a_glyph_change_alone_keeps_the_row_key() {
        // ─ → ┼ changes the built-in list, not the row's packed keys: the row is a
        // row-cache hit (no re-shape), and the prepare is still redone because
        // the built-in list differs.
        let a = pack_grid(&text_grid("x─y", 3), &GridPaint::default(), 10.0, 20.0, &mut test_route, true, PackScratch::default());
        let b = pack_grid(&text_grid("x┼y", 3), &GridPaint::default(), 10.0, 20.0, &mut test_route, true, PackScratch::default());
        assert_eq!(a.keys, b.keys);
        assert_eq!(a.row_hashes, b.row_hashes);
        assert_ne!(a.builtin, b.builtin);
    }

    #[test]
    fn combining_mark_cells_keep_the_font_path() {
        // A box char carrying a combining mark is drawn from its cluster (font
        // fallback composes the mark), never as a bare built-in glyph.
        let g = text_grid("─│", 2);
        let overrides = [(0usize, 0usize, "─\u{301}")];
        let paint = GridPaint { graphemes: &overrides, ..Default::default() };
        let packed = pack_grid(&g, &paint, 10.0, 20.0, &mut test_route, true, PackScratch::default());
        assert_eq!(packed.graphemes.len(), 1);
        assert_eq!(packed.builtin.len(), 1, "only the plain │ is built in");
        assert_eq!(packed.builtin[0].1, 1);
    }

    #[test]
    fn emoji_route_to_the_overdraw_and_narrow_vs16_squeezes_only_when_crowded() {
        let g = text_grid("😀 ✔", 5);
        let packed = pack_grid(&g, &GridPaint::default(), 10.0, 20.0, &mut test_route, true, PackScratch::default());
        let over: Vec<char> = packed.fallback.iter().map(|f| f.2).collect();
        assert_eq!(over, vec!['😀'], "an emoji-presentation char is overdrawn; ✔ stays text");
        let row_text: String = packed.keys.iter().map(|&k| unpack_cell(k).0).collect();
        assert_eq!(row_text, "   ✔ ");
        // ❤️ (narrow base + VS16): two cells when its neighbour is blank, one
        // (half scale) when text follows right away.
        for (text, half) in [("❤ x", false), ("❤x", true)] {
            let g = text_grid(text, 4);
            let overrides = [(0usize, 0usize, "❤\u{FE0F}")];
            let paint = GridPaint { graphemes: &overrides, ..Default::default() };
            let packed = pack_grid(&g, &paint, 10.0, 20.0, &mut test_route, true, PackScratch::default());
            assert_eq!(packed.graphemes[0].4, half, "{text:?}");
            // With color emoji off, nothing is squeezed (the cluster is text).
            let packed = pack_grid(&g, &paint, 10.0, 20.0, &mut test_route, false, PackScratch::default());
            assert!(!packed.graphemes[0].4);
        }
    }

    #[test]
    fn builtin_spans_tile_the_cell_grid_exactly() {
        // Fractional cell widths: each glyph spans exactly its background quad's
        // pixels — consecutive spans share edges (no gap, no overlap) and every
        // span is floor or ceil of the cell width.
        for cell_w in [9.633f32, 10.0, 7.25, 12.5, 19.266] {
            for left in [0.0f32, 8.0, 10.0] {
                let mut prev = px_edge(left);
                for col in 0..300 {
                    let x0 = px_edge(left + col as f32 * cell_w);
                    let x1 = px_edge(left + (col as f32 + 1.0) * cell_w);
                    assert_eq!(x0, prev, "col {col} at cell_w {cell_w}");
                    let w = x1 - x0;
                    assert!(w == cell_w.floor() || w == cell_w.ceil(), "col {col}: {w} at {cell_w}");
                    prev = x1;
                }
            }
        }
        // The edge is where the GPU's pixel-centre rule puts the quad's edge.
        assert_eq!(px_edge(28.9), 29.0);
        assert_eq!(px_edge(28.4), 28.0);
        assert_eq!(px_edge(28.5), 28.0, "a centre exactly on the left edge is inside");
    }

    #[test]
    fn routing_is_per_face() {
        // The same char in a regular and a bold cell: the route callback sees each
        // cell's BOLD|ITALIC bits, so a glyph only the bold face lacks is overdrawn
        // in the bold cell alone (never drawn as .notdef tofu).
        let mut g = text_grid("ΩΩ", 2);
        g.cells[1].attrs = jetty_core::attr::BOLD;
        let mut route = |c: char, shape: u8| {
            if c == 'Ω' && shape & jetty_core::attr::BOLD != 0 { CellRoute::Overdraw } else { CellRoute::Inline }
        };
        let packed = pack_grid(&g, &GridPaint::default(), 10.0, 20.0, &mut route, true, PackScratch::default());
        assert_eq!(packed.fallback.len(), 1);
        assert_eq!(packed.fallback[0].0, 10.0, "the bold cell is overdrawn");
        assert_eq!(unpack_cell(packed.keys[0]).0, 'Ω', "the regular cell stays inline");
        // Cache keys keep the styles apart, and only BOLD|ITALIC matter.
        assert_ne!(route_key('Ω', 0), route_key('Ω', jetty_core::attr::BOLD));
        assert_ne!(route_key('Ω', jetty_core::attr::BOLD), route_key('Ω', jetty_core::attr::ITALIC));
        assert_eq!(route_key('Ω', jetty_core::attr::STRIKE), route_key('Ω', 0));
        assert_eq!(route_key('\u{10FFFF}', 3) & 0x1F_FFFF, 0x10FFFF);
    }

    #[test]
    fn a_styled_face_missing_a_glyph_routes_to_the_overdraw() {
        // Regression: MesloLGS NF Bold / Bold Italic lack the box-drawing block
        // that Regular has; the probe must ask the face the row will shape with.
        // Needs the font installed (the owner's default) — skipped without it.
        let mut fs = TextLayer::build_font_system();
        let has_meslo_bold = fs.db().faces().any(|f| {
            f.families.iter().any(|(n, _)| n == "MesloLGS NF") && f.weight == glyphon::fontdb::Weight::BOLD
        });
        if !has_meslo_bold {
            return;
        }
        let metrics = Metrics::new(16.0, 21.0);
        let mut probe = Buffer::new(&mut fs, metrics);
        probe.set_size(&mut fs, None, None);
        let cell_w = measure_advance_family(&mut fs, metrics, "MesloLGS NF");
        let fam = "MesloLGS NF";
        assert_eq!(probe_route(&mut fs, &mut probe, fam, 'a', 0, cell_w), CellRoute::Inline);
        assert_eq!(probe_route(&mut fs, &mut probe, fam, 'é', jetty_core::attr::BOLD, cell_w), CellRoute::Inline);
        let bold_box = probe_route(&mut fs, &mut probe, fam, '─', jetty_core::attr::BOLD, cell_w);
        let regular_box = probe_route(&mut fs, &mut probe, fam, '─', 0, cell_w);
        assert_eq!(regular_box, CellRoute::Inline);
        // Whichever faces lack it, the styled cell never lays out a .notdef: a
        // face with the glyph inlines it, one without overdraws it.
        let bold_has_it = {
            probe.set_text(&mut fs, "─", &face_attrs(fam, jetty_core::attr::BOLD), Shaping::Basic, None);
            probe.layout_runs().flat_map(|r| r.glyphs.iter()).next().is_some_and(|g| g.glyph_id != 0)
        };
        assert_eq!(bold_box, if bold_has_it { CellRoute::Inline } else { CellRoute::Overdraw });
    }

    #[test]
    fn underlines_follow_the_text_not_the_cell_edge() {
        // Measured from the font: the stroke just under the baseline, the line
        // box bottom near the cell bottom at the default line height, and the
        // whole geometry moving with the text in a taller line. Needs a
        // monospace font (CI may lack MesloLGS NF) — skipped without one.
        let mut fs = TextLayer::build_font_system();
        let Some(fam) = mono_family(&fs) else { return };
        let at = |fs: &mut FontSystem, lh: f32| measure_underline(fs, Metrics::new(16.0, lh), &fam);
        let d = at(&mut fs, 21.0);
        assert_eq!(d.thickness, 2.0, "round(0.1 × 16 px), the historical 2 px at 21 px rows");
        assert!(d.bottom > 18.0 && d.bottom <= 21.0, "{d:?}: the line box bottom, near the cell bottom");
        assert!(d.top + d.thickness <= d.bottom && d.top > 12.0, "{d:?}: under the baseline, above the bottom");
        // A 2.0 line height adds 11 px, split above and below the glyph box: the
        // geometry moves down ~5.5 px with the text, nowhere near the cell edge.
        let t = at(&mut fs, 32.0);
        assert_eq!(t.thickness, d.thickness, "thickness ignores the line height");
        for (a, b) in [(d.top, t.top), (d.bottom, t.bottom)] {
            assert!((b - a - 5.5).abs() <= 1.0, "{d:?} → {t:?}");
        }
        assert!(t.bottom < 29.0, "{t:?}");
    }

    #[test]
    fn the_emoji_family_prefers_a_color_font() {
        let pick = |names: &[&str]| pick_emoji_family(names.iter().copied());
        assert_eq!(pick(&["DejaVu Sans", "Noto Emoji", "Noto Color Emoji"]), Some("Noto Color Emoji".into()));
        assert_eq!(pick(&["Twemoji Mozilla", "MesloLGS NF"]), Some("Twemoji Mozilla".into()));
        assert_eq!(pick(&["Noto Emoji"]), Some("Noto Emoji".into()));
        assert_eq!(pick(&["DejaVu Sans", "FreeMono"]), None);
    }

    #[test]
    fn recolor_spans_repaint_only_their_glyphs() {
        // The current search match: its glyphs take the span color; cells outside
        // keep theirs, a concealed cell (fg == bg) stays concealed, and the block
        // cursor's glyph still wins.
        let (cols, rows) = (8, 3);
        let mut snap = plain_grid(cols, rows);
        let concealed = [18, 18, 23]; // == the default cell bg
        snap.cells[cols + 3].fg = concealed;
        let (red, blue) = ([255, 0, 0], [0, 0, 255]);
        let spans = [(0, 6, 7, blue), (1, 2, 4, red)];
        let fg_at = |paint: &GridPaint, row: usize, col: usize| {
            let packed = pack_grid(&snap, paint, 10.0, 20.0, &mut |_, _| CellRoute::Inline, true, PackScratch::default());
            unpack_cell(packed.keys[row * cols + col]).1
        };
        let paint = GridPaint { recolor: &spans, ..Default::default() };
        let plain = jetty_core::CellSnapshot::default().fg;
        for row in 0..rows {
            for col in 0..cols {
                let want = match (row, col) {
                    (0, 6..=7) => blue,
                    (1, 2) | (1, 4) => red,
                    (1, 3) => concealed,
                    _ => plain,
                };
                assert_eq!(fg_at(&paint, row, col), want, "cell ({row},{col})");
            }
        }
        let with_cursor = GridPaint { recolor: &spans, cursor_glyph: Some((1, 2, [1, 2, 3])), ..Default::default() };
        assert_eq!(fg_at(&with_cursor, 1, 2), [1, 2, 3]);
        // No spans: identical to the plain grid.
        assert_eq!(fg_at(&GridPaint::default(), 0, 6), plain);
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

    // ── Grid metrics: whole-px font size, line height ───────────────────────

    /// A monospace family to shape with: the default terminal font when it is
    /// installed, else any monospace face (CI runners lack MesloLGS NF); `None`
    /// on a machine without one (the shaping tests then have nothing to test).
    #[test]
    fn glyph_routes_stay_bounded_under_a_stream_of_every_code_point() {
        // A program printing every code point in every style: each new (char,
        // style) is probed and cached. The cache must stay within its cap and keep
        // what was just routed.
        let mut routes: FxHashMap<u32, CellRoute> = FxHashMap::default();
        let mut inserted = 0usize;
        for c in (0x80u32..=0x10FFFF).filter_map(char::from_u32) {
            for shape in [0u8, jetty_core::attr::BOLD, jetty_core::attr::ITALIC] {
                let key = route_key(c, shape);
                insert_bounded(&mut routes, key, CellRoute::Inline, GLYPH_ROUTE_CAP);
                inserted += 1;
                assert!(routes.len() <= GLYPH_ROUTE_CAP);
                assert!(routes.contains_key(&key));
            }
        }
        assert!(inserted > 3 * GLYPH_ROUTE_CAP, "the stream did exceed the cap");
    }

    #[test]
    fn overlay_labels_are_shaped_once_and_reused_per_family() {
        let mut fs = TextLayer::build_font_system();
        let mut cache = OverlayCache::default();
        let mut shaped = 0;
        for _frame in 0..10 {
            let pass = cache.next_frame();
            for text in ["✕", "+", "burak@omen: ~", "✕", "✕"] {
                cache.ensure(false, text, 800, pass, &mut fs, |fs| {
                    shaped += 1;
                    Buffer::new(fs, Metrics::new(16.0, 21.0))
                });
            }
            // A tab title is the TITLE family: its own entry even for equal text.
            cache.ensure(true, "burak@omen: ~", 800, pass, &mut fs, |fs| {
                shaped += 1;
                Buffer::new(fs, Metrics::new(16.0, 21.0))
            });
            cache.evict(pass);
        }
        assert_eq!(shaped, 4, "three distinct mono labels + one title, shaped once over ten frames");
        assert!(cache.get(false, "✕").is_some() && cache.get(true, "burak@omen: ~").is_some());
        assert!(cache.get(true, "✕").is_none(), "families are not mixed");
        cache.clear();
        assert_eq!(cache.len(), 0, "a family/size change drops every shaped label");
    }

    #[test]
    fn overlay_cache_stays_bounded_under_ever_new_labels_and_keeps_the_current_pass() {
        // A HUD whose text changes every frame, or a program rewriting a tab title
        // per frame: every pass brings new labels. The cache must stay bounded and
        // still hold every label of the pass being drawn.
        let mut fs = TextLayer::build_font_system();
        let mut cache = OverlayCache::default();
        for frame in 0..(OVERLAY_CACHE_CAP * 3) {
            let pass = cache.next_frame();
            let labels: Vec<String> = (0..3).map(|i| format!("⚡ {frame}.{i} ms")).collect();
            for l in &labels {
                cache.ensure(false, l, 600, pass, &mut fs, |fs| Buffer::new(fs, Metrics::new(16.0, 21.0)));
            }
            cache.evict(pass);
            assert!(cache.len() <= OVERLAY_CACHE_CAP + 3, "pass {frame}: {} entries", cache.len());
            assert!(labels.iter().all(|l| cache.get(false, l).is_some()), "pass {frame} lost a label it draws");
        }
        // Even one pass with more distinct labels than the cap keeps them all.
        let pass = cache.next_frame();
        let many: Vec<String> = (0..OVERLAY_CACHE_CAP + 50).map(|i| format!("row {i}")).collect();
        for l in &many {
            cache.ensure(false, l, 600, pass, &mut fs, |fs| Buffer::new(fs, Metrics::new(16.0, 21.0)));
        }
        cache.evict(pass);
        assert!(many.iter().all(|l| cache.get(false, l).is_some()));
    }

    fn mono_family(fs: &FontSystem) -> Option<String> {
        let faces = || fs.db().faces();
        if faces().any(|f| f.families.iter().any(|(n, _)| n == FONT_FAMILY_DEFAULT)) {
            return Some(FONT_FAMILY_DEFAULT.to_string());
        }
        faces().find(|f| f.monospaced).and_then(|f| f.families.first().map(|(n, _)| n.clone()))
    }

    /// Each glyph's x after shaping `text` exactly like a grid row
    /// (`TextLayer::shape_row`: the terminal family, `Shaping::Basic`, the row
    /// buffer's monospace snap to `cell_w`).
    fn row_glyph_xs(fs: &mut FontSystem, family: &str, metrics: Metrics, cell_w: f32, text: &str) -> Vec<f32> {
        let mut b = grid_row_buffer(fs, metrics, cell_w);
        let attrs = Attrs::new().family(Family::Name(family));
        b.set_rich_text(fs, [(text, attrs.clone())], &attrs, Shaping::Basic, None);
        b.layout_runs().flat_map(|r| r.glyphs.iter().map(|g| g.x).collect::<Vec<_>>()).collect()
    }

    #[test]
    fn font_px_rounds_to_whole_pixels() {
        assert_eq!(rounded_font_px(16.25), 16.0, "13 pt at 1.25×");
        assert_eq!(rounded_font_px(22.5), 23.0, "15 pt at 1.5×: half rounds away from zero");
        assert_eq!(rounded_font_px(19.25), 19.0, "11 pt at 1.75×");
        assert_eq!(rounded_font_px(32.0), 32.0, "16 pt at 2× is untouched");
        assert_eq!(rounded_font_px(0.2), 1.0);
        assert_eq!(rounded_font_px(f32::NAN), 16.0);
        // The historical cell height at the default line height.
        assert_eq!(layer_metrics(16.0, LINE_HEIGHT_DEFAULT).line_height, 21.0);
        assert_eq!(layer_metrics(22.0, LINE_HEIGHT_DEFAULT).line_height, 29.0);
        assert_eq!(layer_metrics(20.0, LINE_HEIGHT_DEFAULT).line_height, 26.0);
    }

    #[test]
    fn line_height_sets_the_cell_height_and_centres_the_glyphs() {
        // The cell height is ceil(px × mult) over the whole key range.
        assert_eq!(layer_metrics(16.0, 1.0).line_height, 16.0);
        assert_eq!(layer_metrics(16.0, 1.3).line_height, 21.0, "the default: today's cell");
        assert_eq!(layer_metrics(16.0, 1.5).line_height, 24.0);
        assert_eq!(layer_metrics(16.0, 2.0).line_height, 32.0);
        assert_eq!(layer_metrics(19.0, 1.3).line_height, 25.0, "24.7 rounds up to whole px");
        assert_eq!(layer_metrics(32.0, 1.3).line_height, 42.0, "2×: exactly twice the 1× cell");
        assert_eq!(clamp_line_height(0.5), LINE_HEIGHT_MIN);
        assert_eq!(clamp_line_height(2.5), LINE_HEIGHT_MAX);
        assert_eq!(clamp_line_height(1.45), 1.45);
        assert_eq!(clamp_line_height(f32::NAN), LINE_HEIGHT_DEFAULT);

        // Shaped like a grid row, the extra line space splits evenly above and
        // below the glyphs: the baseline moves down by half of it.
        let mut fs = TextLayer::build_font_system();
        let Some(family) = mono_family(&fs) else {
            eprintln!("no monospace font installed: nothing to shape");
            return;
        };
        let mut baseline = |m: Metrics| -> (f32, f32) {
            let mut b = grid_row_buffer(&mut fs, m, 9.6);
            let attrs = Attrs::new().family(Family::Name(&family));
            b.set_rich_text(&mut fs, [("Mg", attrs.clone())], &attrs, Shaping::Basic, None);
            let run = b.layout_runs().next().expect("one laid-out line");
            (run.line_y, run.line_height)
        };
        let (y_tight, h_tight) = baseline(layer_metrics(16.0, 1.0));
        for mult in [1.3f32, 1.6, 2.0] {
            let (y, h) = baseline(layer_metrics(16.0, mult));
            assert_eq!(h, (16.0 * mult).ceil(), "line box = the cell height");
            let moved = y - y_tight;
            let half_extra = (h - h_tight) / 2.0;
            assert!((moved - half_extra).abs() <= 0.5, "×{mult}: baseline moved {moved}, half the extra is {half_extra}");
        }
    }

    #[test]
    fn grid_glyphs_land_on_their_cells_at_fractional_physical_sizes() {
        let mut fs = TextLayer::build_font_system();
        let Some(family) = mono_family(&fs) else {
            eprintln!("no monospace font installed: nothing to shape");
            return;
        };
        let text: String = "M0|iW".repeat(24); // 120 columns
        // 13 pt × 1.25, 15 pt × 1.5, 13 pt × 1.5, 11 pt × 1.75, and a whole size.
        for requested in [16.25f32, 22.5, 19.5, 19.25, 16.0] {
            let metrics = layer_metrics(rounded_font_px(requested), LINE_HEIGHT_DEFAULT);
            let cell_w = measure_advance_family(&mut fs, metrics, &family);
            let xs = row_glyph_xs(&mut fs, &family, metrics, cell_w, &text);
            assert_eq!(xs.len(), 120, "{family} @ {requested}px");
            for (col, &x) in xs.iter().enumerate() {
                let want = col as f32 * cell_w;
                assert!((x - want).abs() < 0.01, "{family} @ {requested}px: col {col} at x {x}, cell at {want}");
            }
        }
        // The bug this guards (research: −15 px at column 99 at 16.25 px, +30 px
        // at 22.5 px): an UNROUNDED size drifts off the grid.
        for (requested, sign) in [(16.25f32, -1.0f32), (22.5, 1.0)] {
            let metrics = layer_metrics(requested, LINE_HEIGHT_DEFAULT);
            let cell_w = measure_advance_family(&mut fs, metrics, &family);
            let xs = row_glyph_xs(&mut fs, &family, metrics, cell_w, &text);
            let drift = xs[99] - 99.0 * cell_w;
            assert!(drift * sign > 5.0, "{family} @ {requested}px unrounded: col 99 drift {drift}");
        }
    }

    /// GPU: `render_chrome` (quads + mono labels + titles in ONE pass) draws
    /// exactly the pixels of the pass-per-layer sequence it replaces — the main
    /// window's tab bar + status strip. `#[ignore]`: needs a GPU adapter (the
    /// low-power one). Run: `cargo test -p jetty-render chrome_in_one -- --ignored`.
    #[test]
    #[ignore]
    fn chrome_in_one_pass_matches_a_pass_per_layer() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let (w, h) = (640u32, 200u32);
        let cm = crate::ChromeMetrics::new(1.0, crate::UI_FONT_BASE);
        let theme = jetty_core::Theme::by_name("catppuccin_mocha");
        let tabs: Vec<(String, bool)> = vec![("burak@omen: ~".into(), true), ("cargo build".into(), false)];
        let deco = vec![crate::TabDeco::default(); tabs.len()];
        let render = |one_pass: bool| -> Vec<u8> {
            let mut chrome = TextLayer::new_with_family(&device, &queue, format, crate::UI_FONT_BASE, "MesloLGS NF");
            let mut quad = crate::quad::QuadLayer::new(&device, format);
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("chrome-test"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            // Two frames: the second draws from the shaped-label cache.
            for _ in 0..2 {
                quad.render_clear(&device, &queue, &view, w, h, &[], wgpu::Color::BLACK);
                let bar = crate::build_tab_bar_styled(
                    w, &tabs, &theme, None, crate::CtrlHover::None, None, &mut chrome, cm, &deco,
                    &crate::TabBarOpts::default(),
                );
                let strip = crate::build_status_strip(
                    w, h as f32 - cm.status_h(), cm.status_h(), Some("⚡ idle · 0% CPU · 0 MB/s"), &theme,
                    &mut chrome, cm,
                );
                if one_pass {
                    let mut quads = bar.quads;
                    quads.push(strip.quad);
                    let mut labels = bar.labels;
                    labels.extend(strip.label);
                    chrome
                        .render_chrome(&device, &queue, &view, w, h, &mut quad, &quads, &[(&labels, false), (&bar.title_labels, true)])
                        .unwrap();
                } else {
                    quad.render(&device, &queue, &view, w, h, &bar.quads);
                    chrome.render_overlays(&device, &queue, &view, w, h, &bar.labels).unwrap();
                    chrome.render_overlays_sans(&device, &queue, &view, w, h, &bar.title_labels).unwrap();
                    quad.render(&device, &queue, &view, w, h, &[strip.quad]);
                    chrome.render_overlays(&device, &queue, &view, w, h, &strip.label.into_iter().collect::<Vec<_>>()).unwrap();
                }
            }
            let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (row * h) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_texture_to_buffer(
                tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            queue.submit(Some(enc.finish()));
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let data = buf.slice(..).get_mapped_range().to_vec();
            data
        };
        let (layered, one) = (render(false), render(true));
        assert!(layered.iter().any(|&b| b != 0), "the chrome drew nothing");
        assert!(layered == one, "one-pass chrome differs from the pass-per-layer draw");
    }
}
