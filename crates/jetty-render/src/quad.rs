// Per-instance data: rect (xywh), color (rgba), and rounded-rect params
// (half-size xy, corner radius, _pad). The fragment computes an antialiased
// rounded-rect SDF coverage; radius == 0 yields full coverage everywhere, so
// every existing (sharp) quad is byte-identical to before.
const QUAD_SHADER: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,   // pixel offset from the rect center
    @location(2) hsize: vec2<f32>,   // rect half-size in pixels (NOT `half` — a Metal reserved type)
    @location(3) radius: f32,        // corner radius in pixels
};
@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) round: vec4<f32>,   // half.xy, radius, _pad
) -> VsOut {
    var corners = array<vec2<f32>, 6>(vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let c = corners[vi];
    let px = rect.xy + c * rect.zw;
    let ndc = vec2(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.pos = vec4(ndc, 0.0, 1.0);
    o.color = color;
    let hsize = rect.zw * 0.5;
    o.local = (c - vec2(0.5, 0.5)) * rect.zw; // center-relative pixel coord
    o.hsize = hsize;
    o.radius = round.z;
    return o;
}
fn s2l(c: f32) -> f32 { if (c <= 0.04045) { return c / 12.92; } return pow((c + 0.055) / 1.055, 2.4); }
// Signed distance to a rounded rect (negative inside).
fn sd_round_rect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var cov = 1.0;
    if (in.radius > 0.0) {
        let r = min(in.radius, min(in.hsize.x, in.hsize.y));
        let d = sd_round_rect(in.local, in.hsize, r);
        cov = 1.0 - smoothstep(-0.75, 0.75, d);
    }
    return vec4(s2l(in.color.r), s2l(in.color.g), s2l(in.color.b), in.color.a * cov);
}
"#;

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 4],
    /// Corner radius in pixels. `0.0` = sharp rectangle (the default), so all
    /// existing quads render unchanged. A positive value rounds the corners via
    /// an antialiased rounded-rect SDF in the shader.
    pub radius: f32,
}

impl Default for Rect {
    fn default() -> Self {
        Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0, color: [0, 0, 0, 0], radius: 0.0 }
    }
}

impl Rect {
    /// A sharp (radius 0) rect — convenience matching the old field-only literal.
    pub fn new(x: f32, y: f32, w: f32, h: f32, color: [u8; 4]) -> Self {
        Rect { x, y, w, h, color, radius: 0.0 }
    }

    /// A rounded rect with the given corner `radius` in pixels.
    pub fn rounded(x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32) -> Self {
        Rect { x, y, w, h, color, radius }
    }
}

pub struct QuadLayer {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Persistent instance buffer, grown on demand and rewritten each frame via
    /// `queue.write_buffer` instead of being recreated. `instance_cap` is the
    /// current capacity in bytes.
    instance_buf: Option<wgpu::Buffer>,
    instance_cap: u64,
    /// Scratch CPU buffer reused across frames to pack instance floats.
    instance_scratch: Vec<f32>,
}

impl QuadLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad-shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad-uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 48,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: 16,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: 32,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            uniform_buf,
            bind_group,
            instance_buf: None,
            instance_cap: 0,
            instance_scratch: Vec::new(),
        }
    }

    /// Pack `rects` into the persistent instance buffer, growing it only when the
    /// existing capacity is too small. Returns the byte length of the packed data.
    /// The data is uploaded via `queue.write_buffer`, never recreated per frame.
    fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rects: &[Rect],
    ) -> u64 {
        self.instance_scratch.clear();
        self.instance_scratch.reserve(rects.len() * 12);
        for r in rects {
            self.instance_scratch.push(r.x);
            self.instance_scratch.push(r.y);
            self.instance_scratch.push(r.w);
            self.instance_scratch.push(r.h);
            self.instance_scratch.push(r.color[0] as f32 / 255.0);
            self.instance_scratch.push(r.color[1] as f32 / 255.0);
            self.instance_scratch.push(r.color[2] as f32 / 255.0);
            self.instance_scratch.push(r.color[3] as f32 / 255.0);
            // Round params: half-size xy (unused by the shader; derived from
            // rect there too), corner radius, _pad.
            self.instance_scratch.push(r.w * 0.5);
            self.instance_scratch.push(r.h * 0.5);
            self.instance_scratch.push(r.radius);
            self.instance_scratch.push(0.0);
        }
        let bytes = bytemuck::cast_slice::<f32, u8>(&self.instance_scratch);
        let needed = bytes.len() as u64;

        // Grow the persistent buffer only when it cannot hold this frame's data.
        if self.instance_buf.is_none() || self.instance_cap < needed {
            // Round up to reduce churn from frame-to-frame size jitter.
            let new_cap = needed.max(self.instance_cap * 2).max(256);
            self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quad-instances"),
                size: new_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.instance_cap = new_cap;
        }

        queue.write_buffer(self.instance_buf.as_ref().unwrap(), 0, bytes);
        needed
    }

    /// Draw `rects` over whatever is already in `view` (`LoadOp::Load`).
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
    ) {
        self.render_inner(device, queue, view, screen_w, screen_h, rects, None, None);
    }

    /// Clear `view` to `clear_color`, then draw `rects` on top. Used for the
    /// per-cell background pass that runs UNDER the terminal text: it owns the
    /// frame clear so `TextLayer::render_to` can run with `LoadOp::Load`.
    ///
    /// Unlike `render`, this always runs (even with no rects) so the clear is not
    /// skipped on a screen made entirely of default-bg cells.
    #[allow(clippy::too_many_arguments)]
    pub fn render_clear(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
        clear_color: wgpu::Color,
    ) {
        self.render_inner(device, queue, view, screen_w, screen_h, rects, Some(clear_color), None);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
        clear_color: Option<wgpu::Color>,
        // Optional scissor rect [x, y, w, h] in physical pixels that
        // restricts drawing to a sub-region of the surface. None = no scissor
        // (the default viewport covers the whole surface, which is the standard
        // behaviour for all existing callers).
        scissor: Option<[u32; 4]>,
    ) {
        // With nothing to draw and no clear requested, there is no work to do.
        if rects.is_empty() && clear_color.is_none() {
            return;
        }

        let uniform_data: [f32; 4] = [screen_w as f32, screen_h as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform_data));

        if !rects.is_empty() {
            self.upload_instances(device, queue, rects);
        }

        let load = match clear_color {
            Some(c) => wgpu::LoadOp::Clear(c),
            None => wgpu::LoadOp::Load,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("quad-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quad-pass"),
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
            // Scissor restricts all subsequent draws to a sub-region of the
            // surface (physical px). Called before draw so the restriction is in
            // effect for every instance we emit. When None the hardware default
            // (full viewport) applies — identical to the existing behaviour.
            if let Some([sx, sy, sw, sh]) = scissor {
                pass.set_scissor_rect(sx, sy, sw, sh);
            }
            if !rects.is_empty() {
                let buf = self.instance_buf.as_ref().unwrap();
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..rects.len() as u32);
            }
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Render `rects` on top of existing content (`LoadOp::Load`) with a
    /// **scissor rect** that clips drawing to `[x, y, w, h]` in physical pixels.
    /// Used for the Effects-tab content area so widgets scrolled above/below the
    /// visible region are hardware-clipped and never bleed into the chrome.
    #[allow(clippy::too_many_arguments)]
    pub fn render_load_scissored(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
        scissor: [u32; 4],
    ) {
        self.render_inner(device, queue, view, screen_w, screen_h, rects, None, Some(scissor));
    }
}

/// Convert an sRGB component (0..=255) to linear float (0.0..=1.0), matching the
/// quad shader's `s2l` and `TextLayer`'s clear-color conversion. The surface is
/// sRGB, so wgpu `Clear` values must be linear.
fn srgb_to_linear(c: u8) -> f64 {
    let s = c as f64 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// The wgpu clear color for the terminal's default background, derived from the
/// snapshot's theme bg. `premultiply` MUST match the surface's chosen
/// `CompositeAlphaMode` (see `GpuContext::premultiply_clear`):
///   • `true`  (PreMultiplied surface — Vulkan/Wayland): rgb is multiplied by
///     alpha so transparent themes composite correctly.
///   • `false` (PostMultiplied surface — Metal/macOS, or Opaque): rgb stays
///     STRAIGHT; the compositor multiplies by alpha itself. Premultiplying here
///     would double-darken transparent themes (and on Metal, without selecting
///     PostMultiplied at all the window can't be see-through).
/// Harmless either way when alpha == 255.
///
/// This is the same value `TextLayer::render_to` used to clear with; it now lives
/// here so the per-cell background quad pass (which owns the clear) can reuse it.
pub fn default_bg_clear(snapshot: &jetty_core::GridSnapshot, premultiply: bool) -> wgpu::Color {
    let [br, bg_, bb, ba] = snapshot.bg_rgba;
    let a = ba as f64 / 255.0;
    let m = if premultiply { a } else { 1.0 };
    wgpu::Color {
        r: srgb_to_linear(br) * m,
        g: srgb_to_linear(bg_) * m,
        b: srgb_to_linear(bb) * m,
        a,
    }
}

/// Build per-cell background rectangles for every cell whose background differs
/// from the theme's default background (`snapshot.bg_rgba[0..3]`), plus
/// selection highlight rects for all selected cells (overriding their normal bg).
/// Horizontal runs of cells sharing the same effective bg in a row are coalesced
/// into a single Rect.
///
/// Each rect is opaque (alpha 255): a colored cell background should fully cover,
/// even on a transparent theme — only default-bg cells stay transparent (handled
/// by the frame clear, which keeps the theme's alpha).
pub fn cell_bg_rects(
    snapshot: &jetty_core::GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    selection_bg: [u8; 3],
) -> Vec<Rect> {
    let default_bg = [snapshot.bg_rgba[0], snapshot.bg_rgba[1], snapshot.bg_rgba[2]];
    let mut rects: Vec<Rect> = Vec::new();

    for row in 0..snapshot.rows {
        let mut col = 0;
        while col < snapshot.cols {
            let cell = snapshot.cell(row, col);
            // Effective bg: selection overrides normal bg.
            let effective_bg = if cell.selected { selection_bg } else { cell.bg };
            if effective_bg == default_bg && !cell.selected {
                col += 1;
                continue;
            }
            // Extend the run while the effective bg stays equal.
            let start = col;
            col += 1;
            while col < snapshot.cols {
                let next = snapshot.cell(row, col);
                let next_bg = if next.selected { selection_bg } else { next.bg };
                if next_bg != effective_bg {
                    break;
                }
                col += 1;
            }
            let run = (col - start) as f32;
            rects.push(Rect {
                x: start as f32 * cell_w,
                y: row as f32 * cell_h + y_offset,
                w: run * cell_w,
                h: cell_h,
                color: [effective_bg[0], effective_bg[1], effective_bg[2], 255],
                ..Default::default()
            });
        }
    }

    rects
}

/// Underline / strikethrough stroke thickness for a given (physical) cell
/// height, floored at 1px so it never vanishes on a small font / low-DPI.
#[inline]
fn decoration_thickness(cell_h: f32) -> f32 {
    (cell_h * 0.075).round().max(1.0)
}

/// Fold ONE cell's decoration state (strike + underline style, and the relevant
/// colors) into a hasher. Used by both `grid_decoration_key` (the pure,
/// testable seam) and, inline, by `TextLayer::render_to` so the two never drift.
/// Cell position is implicit in the caller's iteration order.
#[inline]
pub(crate) fn fold_decoration<H: std::hash::Hasher>(h: &mut H, cell: &jetty_core::CellSnapshot) {
    use std::hash::Hash;
    let deco = cell.attrs & (jetty_core::attr::STRIKE | jetty_core::attr::UL_MASK);
    deco.hash(h);
    if deco != 0 {
        // Underline color and strike color (fg) only matter when a decoration is
        // actually present — skip them for the (common) undecorated cell.
        cell.uline.hash(h);
        cell.fg.hash(h);
    }
}

/// Content fingerprint of everything `text_decoration_rects` draws (strike +
/// underline style + colors, positionally). Two snapshots with the SAME key
/// produce identical decoration rects, so the caller can cache the built rects
/// and rebuild only when this changes — decorations never need to rebuild on a
/// caret-flash / CRT / scrollbar-only animate frame. Excludes `c`/`fg` of
/// undecorated cells, so a plain text edit that adds no decoration is a no-op.
pub fn grid_decoration_key(snap: &jetty_core::GridSnapshot) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for cell in &snap.cells {
        fold_decoration(&mut h, cell);
    }
    h.finish()
}

/// Emit the quads for ONE horizontal underline run of the given `style` spanning
/// `[x0, x0+run_w)` with its bottom edge at `bottom`. Single/double are wide
/// quads; dotted/dashed/undercurl are small patterned quads. `color` is the
/// resolved underline color (uline, already falling back to fg cell-side).
fn emit_underline(
    out: &mut Vec<Rect>,
    style: u8,
    x0: f32,
    run_w: f32,
    bottom: f32,
    th: f32,
    cell_w: f32,
    color: [u8; 4],
) {
    use jetty_core::attr;
    let y = bottom - th; // top of the (lowest) stroke
    match style {
        attr::UL_SINGLE => {
            out.push(Rect::new(x0, y, run_w, th, color));
        }
        attr::UL_DOUBLE => {
            // Two strokes separated by a `th` gap: lower flush at the cell bottom,
            // upper `2*th` above it.
            out.push(Rect::new(x0, y, run_w, th, color));
            out.push(Rect::new(x0, y - 2.0 * th, run_w, th, color));
        }
        attr::UL_DOTTED => {
            // `th`-wide dots on a `2*th` pitch (dot + equal gap).
            let step = (th * 2.0).max(2.0);
            let mut x = x0;
            while x < x0 + run_w {
                let w = th.min(x0 + run_w - x);
                if w <= 0.0 {
                    break;
                }
                out.push(Rect::new(x, y, w, th, color));
                x += step;
            }
        }
        attr::UL_DASHED => {
            // ~0.4·cell_w dashes with ~0.2·cell_w gaps.
            let dash = (cell_w * 0.4).max(2.0);
            let gap = (cell_w * 0.2).max(1.0);
            let step = dash + gap;
            let mut x = x0;
            while x < x0 + run_w {
                let w = dash.min(x0 + run_w - x);
                if w <= 0.0 {
                    break;
                }
                out.push(Rect::new(x, y, w, th, color));
                x += step;
            }
        }
        attr::UL_UNDERCURL => {
            // Stepped triangle wave: short `seg`-wide, `th`-tall quads whose y
            // follows a triangle of amplitude `amp` and period `cell_w`. Sparse in
            // practice (a squiggle under a diagnostic word), so plain quads suffice.
            let amp = (th * 1.5).max(1.0);
            let seg = th.max(1.0);
            let period = cell_w.max(4.0);
            let top = bottom - th - amp; // top of the wave band
            let mut x = x0;
            while x < x0 + run_w {
                let w = seg.min(x0 + run_w - x);
                if w <= 0.0 {
                    break;
                }
                let phase = ((x - x0) / period).fract(); // 0..1
                let tri = if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 };
                out.push(Rect::new(x, top + tri * amp, w, th, color));
                x += seg;
            }
        }
        _ => {}
    }
}

/// Build the underline + strikethrough quads for the whole grid, appending into
/// `out` (reused across frames by the caller so it does not reallocate). Called
/// only on a rendered frame whose decoration content changed (see
/// `grid_decoration_key`); idle/animate-only frames reuse the cached rects.
///
/// Underlines coalesce consecutive cells that share `(style, uline)` into one run
/// (single/double become one wide quad); strikethroughs coalesce consecutive
/// equal-`fg` cells. Colors come from the cell (`uline` for underlines — which is
/// already the theme/SGR-58 color with an fg fallback — and `fg` for strike), so
/// every theme is covered with no per-theme code.
pub fn text_decoration_rects(
    snap: &jetty_core::GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    out: &mut Vec<Rect>,
) {
    use jetty_core::attr;
    let th = decoration_thickness(cell_h);
    for row in 0..snap.rows {
        // --- underline pass: coalesce equal (style, uline) runs ---
        let mut col = 0;
        while col < snap.cols {
            let cell = snap.cell(row, col);
            let style = cell.underline_style();
            if style == attr::UL_NONE {
                col += 1;
                continue;
            }
            let uline = cell.uline;
            let start = col;
            col += 1;
            while col < snap.cols {
                let n = snap.cell(row, col);
                if n.underline_style() != style || n.uline != uline {
                    break;
                }
                col += 1;
            }
            let x0 = start as f32 * cell_w;
            let run_w = (col - start) as f32 * cell_w;
            let bottom = y_offset + (row as f32 + 1.0) * cell_h;
            let color = [uline[0], uline[1], uline[2], 255];
            emit_underline(out, style, x0, run_w, bottom, th, cell_w, color);
        }
        // --- strikethrough pass: coalesce equal-fg runs, drawn at mid-cell ---
        let mut col = 0;
        while col < snap.cols {
            let cell = snap.cell(row, col);
            if !cell.is_strike() {
                col += 1;
                continue;
            }
            let fg = cell.fg;
            let start = col;
            col += 1;
            while col < snap.cols {
                let n = snap.cell(row, col);
                if !n.is_strike() || n.fg != fg {
                    break;
                }
                col += 1;
            }
            let x0 = start as f32 * cell_w;
            let run_w = (col - start) as f32 * cell_w;
            let y = y_offset + row as f32 * cell_h + cell_h * 0.5 - th * 0.5;
            out.push(Rect::new(x0, y, run_w, th, [fg[0], fg[1], fg[2], 255]));
        }
    }
}

/// Build the Ctrl+hover / OSC 8 link underline quads. `spans` are
/// `(row, col_start, col_end)` in VIEWPORT cells (col_end inclusive). Reuses the
/// single-underline geometry so link and SGR underlines share one thickness and
/// draw in the same quad batch — the single definition that replaces the three
/// previously hand-rolled copies (main / detached / jetty-shot).
pub fn link_underline_rects(
    spans: &[(usize, usize, usize)],
    color: [u8; 4],
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
) -> Vec<Rect> {
    let th = decoration_thickness(cell_h);
    spans
        .iter()
        .map(|&(row, c0, c1)| {
            Rect::new(
                c0 as f32 * cell_w,
                y_offset + (row as f32 + 1.0) * cell_h - th,
                (c1 - c0 + 1) as f32 * cell_w,
                th,
                color,
            )
        })
        .collect()
}

/// Build the cursor quad(s) for the current frame — the ONE per-frame quad
/// rebuild (everything else is cached), because the caret flash animates.
///
/// Shape → quads: Block = one filled cell rect; Beam = a thin left bar; Underline
/// = a thin bottom bar; HollowBlock = four edge bars. An UNFOCUSED window hollows
/// out a Block cursor (matching most terminals); Beam/Underline are unchanged
/// when unfocused. `caret_t`/`flash_color` apply the keystroke flash (color lerp
/// cursor_rgb→flash, plus a Block-only center-scale bump), ported from the old
/// text-glyph cursor path. Returns empty when the cursor is hidden/out of bounds.
pub fn cursor_rects(
    snap: &jetty_core::GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    focused: bool,
    caret_t: Option<f32>,
    flash_color: [f32; 3],
) -> Vec<Rect> {
    use jetty_core::CursorShapeSnap;
    if !snap.cursor_visible || snap.cursor_col >= snap.cols || snap.cursor_row >= snap.rows {
        return Vec::new();
    }
    // Effective shape: an unfocused window hollows out the BLOCK cursor. Beam and
    // Underline stay as-is when unfocused (only Block hollows — v0.13 amendment).
    let shape = if !focused && snap.cursor_shape == CursorShapeSnap::Block {
        CursorShapeSnap::HollowBlock
    } else {
        snap.cursor_shape
    };
    let [cr, cg, cb] = snap.cursor_rgb;
    // Caret flash: bump = 4·e·(1−e), e = 1−(1−t)². Both color and the Block scale
    // ride the same bump so they return to rest at t=1 (no snap). Byte-faithful to
    // the previous text.rs cursor formula, minus the atlas-key scale quantization
    // that a quad (no glyph atlas) no longer needs.
    let (color, scale) = if let Some(t) = caret_t {
        let e = 1.0 - (1.0 - t) * (1.0 - t);
        let bump = 4.0 * e * (1.0 - e);
        let [fr, fgc, fbc] = flash_color;
        let lerp = |base: u8, target: f32| -> u8 {
            let b = base as f32 / 255.0;
            ((b + (target - b) * bump) * 255.0).round().clamp(0.0, 255.0) as u8
        };
        ([lerp(cr, fr), lerp(cg, fgc), lerp(cb, fbc), 255], 1.0 + 0.15 * bump)
    } else {
        ([cr, cg, cb, 255], 1.0)
    };
    let base_x = snap.cursor_col as f32 * cell_w;
    let base_y = y_offset + snap.cursor_row as f32 * cell_h;
    let mut rects = Vec::new();
    match shape {
        CursorShapeSnap::Block => {
            // Center-scale bump about the cell (matches the old glyph scaling).
            let w = cell_w * scale;
            let h = cell_h * scale;
            rects.push(Rect::new(
                base_x - (w - cell_w) * 0.5,
                base_y - (h - cell_h) * 0.5,
                w,
                h,
                color,
            ));
        }
        CursorShapeSnap::Beam => {
            let w = (cell_w * 0.12).max(1.0);
            rects.push(Rect::new(base_x, base_y, w, cell_h, color));
        }
        CursorShapeSnap::Underline => {
            let h = (cell_h * 0.12).max(1.0);
            rects.push(Rect::new(base_x, base_y + cell_h - h, cell_w, h, color));
        }
        CursorShapeSnap::HollowBlock => {
            let b = (cell_w * 0.1).max(1.0);
            rects.push(Rect::new(base_x, base_y, cell_w, b, color)); // top
            rects.push(Rect::new(base_x, base_y + cell_h - b, cell_w, b, color)); // bottom
            rects.push(Rect::new(base_x, base_y, b, cell_h, color)); // left
            rects.push(Rect::new(base_x + cell_w - b, base_y, b, cell_h, color)); // right
        }
    }
    rects
}

/// Scrollbar thumb width in px. The terminal grid reserves this much on the
/// right (a gutter) so content never renders underneath the scrollbar.
pub const SCROLLBAR_W: f32 = 14.0;

/// Gap (px) between the scrollbar track ends and the bars, so the thumb stays
/// clear of the tab bar / window controls. Shared by the thumb geometry and
/// the drag inverse below so drawing and dragging can never disagree.
pub(crate) const SCROLLBAR_GAP: f32 = 4.0;

/// Minimum thumb height (px) so a huge scrollback still leaves a grabbable thumb.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;

/// Compute the scrollbar thumb rectangle from raw geometry values.
/// This is the canonical geometry computation shared by drawing and hit-testing.
/// Returns `None` when `scroll_max == 0` (nothing to scroll).
#[allow(clippy::too_many_arguments)]
pub fn scrollbar_rect_geom(
    rows: usize,
    scroll_offset: usize,
    scroll_max: usize,
    screen_w: u32,
    screen_h: u32,
    top_offset: f32,
    bottom_reserve: f32,
    thumb: [u8; 4],
) -> Option<Rect> {
    if scroll_max == 0 {
        return None;
    }
    // The track is the GRID area, which is ALWAYS TABBAR_H shorter than the
    // surface (the bar takes that height whichever side it sits on). Using
    // `screen_h - top_offset` overshot in BOTTOM-bar mode (top_offset = 0): the
    // scrollbar ran the full height and collided with the bottom window controls
    // (the ✕). A small GAP also keeps the thumb clear of the bar / controls.
    let track_top = top_offset + SCROLLBAR_GAP;
    // `bottom_reserve` is the height reserved at the bottom for the status bar
    // (the perf HUD) — the track must stop above it so the thumb never runs under
    // the status bar. 0 when there is no status bar.
    let track_h =
        (screen_h as f32 - crate::TABBAR_H - bottom_reserve - SCROLLBAR_GAP * 2.0).max(0.0);
    let total = rows + scroll_max;
    let thumb_h = (track_h * rows as f32 / total as f32).max(SCROLLBAR_MIN_THUMB);
    let frac = (scroll_max - scroll_offset) as f32 / scroll_max as f32;
    let travel = (track_h - thumb_h).max(0.0);
    let thumb_y = track_top + frac * travel;
    let thumb_w = SCROLLBAR_W; // wide enough to grab comfortably
    Some(Rect {
        x: screen_w as f32 - thumb_w,
        y: thumb_y,
        w: thumb_w,
        h: thumb_h,
        color: thumb,
        ..Default::default()
    })
}

/// Map an absolute cursor y (physical px) to a scroll offset during a
/// scrollbar drag — the pure inverse of `scrollbar_rect_geom`'s thumb
/// placement (the round-trip is unit-tested). `grab_dy` is the thumb-local y
/// offset captured at press so the thumb never jumps under the pointer.
/// Returns `None` when there is no history (`scroll_max == 0`) or the thumb
/// fills the track (no travel — tiny window).
pub fn scrollbar_offset_from_cursor(
    cursor_y: f32,
    grab_dy: f32,
    rows: usize,
    scroll_max: usize,
    screen_h: u32,
    top_offset: f32,
    bottom_reserve: f32,
) -> Option<usize> {
    if scroll_max == 0 {
        return None;
    }
    // Deliberate asymmetry, mirroring scrollbar_rect_geom: the track HEIGHT
    // always subtracts TABBAR_H (the bar takes that height whichever side it
    // sits on) while the thumb ORIGIN uses `top_offset` (0 in bottom-bar mode).
    let track_h =
        (screen_h as f32 - crate::TABBAR_H - bottom_reserve - SCROLLBAR_GAP * 2.0).max(0.0);
    let total = rows + scroll_max;
    let thumb_h = (track_h * rows as f32 / total as f32).max(SCROLLBAR_MIN_THUMB);
    let travel = track_h - thumb_h;
    if travel <= 0.0 {
        return None;
    }
    let thumb_top = ((cursor_y - top_offset - SCROLLBAR_GAP) - grab_dy).clamp(0.0, travel);
    // frac=0 → thumb at top → scroll_offset=max (oldest history)
    // frac=1 → thumb at bottom → scroll_offset=0 (live bottom)
    let frac = thumb_top / travel;
    Some(((1.0 - frac) * scroll_max as f32).round() as usize)
}

pub fn scrollbar_rect(
    snapshot: &jetty_core::GridSnapshot,
    screen_w: u32,
    screen_h: u32,
    top_offset: f32,
    bottom_reserve: f32,
    thumb: [u8; 4],
) -> Option<Rect> {
    scrollbar_rect_geom(
        snapshot.rows,
        snapshot.scroll_offset,
        snapshot.scroll_max,
        screen_w,
        screen_h,
        top_offset,
        bottom_reserve,
        thumb,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetty_core::{attr, CellSnapshot, CursorShapeSnap, GridSnapshot};

    /// A blank grid with default cells for the decoration/cursor geometry tests.
    fn grid(cols: usize, rows: usize) -> GridSnapshot {
        GridSnapshot {
            cols,
            rows,
            cells: vec![CellSnapshot::default(); cols * rows],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: false,
            bg_rgba: [0, 0, 0, 255],
            cursor_rgb: [200, 200, 200],
            scroll_offset: 0,
            scroll_max: 0,
            cursor_shape: CursorShapeSnap::Block,
        }
    }

    #[test]
    fn single_underline_run_coalesces_to_one_rect() {
        let mut g = grid(5, 1);
        for c in 0..3 {
            let cell = &mut g.cells[c];
            cell.attrs = attr::UL_SINGLE << attr::UL_SHIFT;
            cell.uline = [10, 20, 30];
        }
        let mut out = Vec::new();
        text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
        assert_eq!(out.len(), 1, "3 equal single-underline cells => 1 wide quad");
        let r = out[0];
        assert_eq!(r.x, 0.0);
        assert_eq!(r.w, 30.0, "spans all 3 cells");
        assert_eq!([r.color[0], r.color[1], r.color[2]], [10, 20, 30], "uses uline color");
        // bottom = 20, th = round(20*0.075)=2 => y = 18
        assert_eq!(r.y, 18.0);
        assert_eq!(r.h, 2.0);
    }

    #[test]
    fn underline_run_breaks_on_color_change() {
        let mut g = grid(4, 1);
        for c in 0..4 {
            g.cells[c].attrs = attr::UL_SINGLE << attr::UL_SHIFT;
        }
        g.cells[0].uline = [1, 1, 1];
        g.cells[1].uline = [1, 1, 1];
        g.cells[2].uline = [2, 2, 2];
        g.cells[3].uline = [2, 2, 2];
        let mut out = Vec::new();
        text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
        assert_eq!(out.len(), 2, "two color runs => two rects");
    }

    #[test]
    fn double_underline_emits_two_stacked_rects() {
        let mut g = grid(2, 1);
        for c in 0..2 {
            g.cells[c].attrs = attr::UL_DOUBLE << attr::UL_SHIFT;
        }
        let mut out = Vec::new();
        text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
        assert_eq!(out.len(), 2, "double underline => two quads");
        // The two strokes are separated vertically by a gap.
        assert_ne!(out[0].y, out[1].y);
    }

    #[test]
    fn dotted_dashed_undercurl_emit_bounded_multiple_quads() {
        for style in [attr::UL_DOTTED, attr::UL_DASHED, attr::UL_UNDERCURL] {
            let mut g = grid(6, 1);
            for c in 0..6 {
                g.cells[c].attrs = style << attr::UL_SHIFT;
            }
            let mut out = Vec::new();
            text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
            assert!(out.len() > 1, "patterned style {style} => multiple quads");
            // Bounded: never more than one quad per physical pixel of run width.
            assert!(out.len() <= 60, "style {style} quad count {} too high", out.len());
        }
    }

    #[test]
    fn strike_sits_at_mid_cell_in_fg() {
        let mut g = grid(2, 1);
        for c in 0..2 {
            g.cells[c].attrs = attr::STRIKE;
            g.cells[c].fg = [90, 80, 70];
        }
        let mut out = Vec::new();
        text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
        assert_eq!(out.len(), 1, "strike run coalesces");
        let r = out[0];
        // mid-cell: y = 20*0.5 - th/2 = 10 - 1 = 9
        assert_eq!(r.y, 9.0);
        assert_eq!([r.color[0], r.color[1], r.color[2]], [90, 80, 70], "strike uses fg");
    }

    #[test]
    fn undecorated_grid_emits_no_decorations() {
        let g = grid(10, 5);
        let mut out = Vec::new();
        text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn link_underline_reuses_single_geometry() {
        let spans = vec![(0usize, 2usize, 4usize)];
        let rects = link_underline_rects(&spans, [0, 0, 255, 255], 10.0, 20.0, 0.0);
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!(r.x, 20.0, "starts at col 2");
        assert_eq!(r.w, 30.0, "cols 2..=4 inclusive");
        assert_eq!(r.y, 18.0, "same bottom-th geometry as a single SGR underline");
    }

    #[test]
    fn decoration_key_changes_on_underline_but_not_on_plain_char() {
        let mut a = grid(4, 1);
        a.cells[1].c = 'x';
        let base = grid_decoration_key(&a);
        // Editing a plain (undecorated) char must NOT change the key.
        let mut b = a.clone();
        b.cells[2].c = 'y';
        assert_eq!(grid_decoration_key(&b), base, "plain text edit => same deco key");
        // Adding an underline MUST change it.
        let mut c = a.clone();
        c.cells[2].attrs = attr::UL_SINGLE << attr::UL_SHIFT;
        assert_ne!(grid_decoration_key(&c), base, "adding an underline => new deco key");
        // Changing only the underline color must also change it.
        let mut d = c.clone();
        d.cells[2].uline = [1, 2, 3];
        assert_ne!(grid_decoration_key(&d), grid_decoration_key(&c));
    }

    #[test]
    fn cursor_block_is_one_full_cell_rect() {
        let mut g = grid(5, 3);
        g.cursor_visible = true;
        g.cursor_col = 1;
        g.cursor_row = 2;
        g.cursor_shape = CursorShapeSnap::Block;
        let r = cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0, 1.0, 1.0]);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].x, r[0].y, r[0].w, r[0].h), (10.0, 40.0, 10.0, 20.0));
    }

    #[test]
    fn cursor_beam_and_underline_are_one_thin_rect() {
        let mut g = grid(5, 3);
        g.cursor_visible = true;
        g.cursor_shape = CursorShapeSnap::Beam;
        let beam = cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0, 1.0, 1.0]);
        assert_eq!(beam.len(), 1);
        assert!(beam[0].w < 10.0 && beam[0].h == 20.0, "beam is a thin left bar");
        g.cursor_shape = CursorShapeSnap::Underline;
        let ul = cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0, 1.0, 1.0]);
        assert_eq!(ul.len(), 1);
        assert!(ul[0].h < 20.0 && ul[0].w == 10.0, "underline is a thin bottom bar");
    }

    #[test]
    fn cursor_hollow_is_four_border_rects() {
        let mut g = grid(5, 3);
        g.cursor_visible = true;
        g.cursor_shape = CursorShapeSnap::HollowBlock;
        let r = cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0, 1.0, 1.0]);
        assert_eq!(r.len(), 4, "hollow block => 4 edge quads");
    }

    #[test]
    fn unfocused_block_hollows_out() {
        let mut g = grid(5, 3);
        g.cursor_visible = true;
        g.cursor_shape = CursorShapeSnap::Block;
        // Focused: solid block (1 rect). Unfocused: hollow (4 rects).
        assert_eq!(cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0; 3]).len(), 1);
        assert_eq!(cursor_rects(&g, 10.0, 20.0, 0.0, false, None, [1.0; 3]).len(), 4);
        // A beam cursor stays a beam even when unfocused.
        g.cursor_shape = CursorShapeSnap::Beam;
        assert_eq!(cursor_rects(&g, 10.0, 20.0, 0.0, false, None, [1.0; 3]).len(), 1);
    }

    #[test]
    fn hidden_cursor_emits_nothing() {
        let mut g = grid(5, 3);
        g.cursor_visible = false;
        assert!(cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0; 3]).is_empty());
    }

    #[test]
    fn caret_flash_shifts_color_and_grows_block() {
        let mut g = grid(5, 3);
        g.cursor_visible = true;
        g.cursor_rgb = [0, 0, 0];
        g.cursor_shape = CursorShapeSnap::Block;
        let rest = cursor_rects(&g, 10.0, 20.0, 0.0, true, None, [1.0, 1.0, 1.0]);
        let flash = cursor_rects(&g, 10.0, 20.0, 0.0, true, Some(0.5), [1.0, 1.0, 1.0]);
        // Color moved toward white.
        assert!(flash[0].color[0] > rest[0].color[0], "flash lerps toward flash_color");
        // Block grew (scale bump), centered so it still overlaps the cell.
        assert!(flash[0].w > rest[0].w, "block grows during the flash");
        assert!(flash[0].x < rest[0].x, "growth stays centered");
    }

    #[test]
    fn scrollbar_offset_from_cursor_none_when_no_history() {
        // No scrollback → nothing to drag.
        assert_eq!(
            scrollbar_offset_from_cursor(100.0, 0.0, 40, 0, 640, 36.0, 0.0),
            None
        );
        // Window so short the (min-height) thumb fills the track → no travel.
        assert_eq!(
            scrollbar_offset_from_cursor(40.0, 0.0, 1000, 1, 44, 36.0, 0.0),
            None
        );
    }

    #[test]
    fn scrollbar_offset_from_cursor_track_ends() {
        let (rows, max, h, top, bottom) = (40usize, 200usize, 640u32, 36.0f32, 22.0f32);
        // Cursor at the track top (top_offset + GAP) → oldest history.
        let track_top = top + SCROLLBAR_GAP;
        assert_eq!(
            scrollbar_offset_from_cursor(track_top, 0.0, rows, max, h, top, bottom),
            Some(max)
        );
        // Beyond the top end clamps to the same extreme.
        assert_eq!(
            scrollbar_offset_from_cursor(-500.0, 0.0, rows, max, h, top, bottom),
            Some(max)
        );
        // At/below the track bottom → live bottom (offset 0), clamped too.
        assert_eq!(
            scrollbar_offset_from_cursor(h as f32, 0.0, rows, max, h, top, bottom),
            Some(0)
        );
        assert_eq!(
            scrollbar_offset_from_cursor(h as f32 + 500.0, 0.0, rows, max, h, top, bottom),
            Some(0)
        );
    }

    #[test]
    fn scrollbar_offset_round_trips_with_rect_geom() {
        // Drawing (rect_geom) and dragging (offset_from_cursor) must agree
        // forever: feeding a drawn thumb's y back recovers the same offset.
        let (rows, max, w, h, top, bottom) = (40usize, 200usize, 800u32, 640u32, 36.0f32, 22.0f32);
        for off in [0usize, 50, 123, 200] {
            let rect = scrollbar_rect_geom(rows, off, max, w, h, top, bottom, [0, 0, 0, 0])
                .expect("thumb rect");
            let rec = scrollbar_offset_from_cursor(rect.y, 0.0, rows, max, h, top, bottom)
                .expect("offset");
            assert!(
                (rec as i64 - off as i64).abs() <= 1,
                "offset {off} round-tripped to {rec}"
            );
        }
    }
}
