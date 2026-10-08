// Per-instance data: rect (xywh), color (rgba), and shape params (shear,
// unused, corner radius, _pad). The fragment computes an antialiased
// rounded-rect SDF coverage; radius == 0 yields full coverage everywhere, so
// every existing (sharp) quad is byte-identical to before. `shear` slants the
// quad into a parallelogram (x shifts by shear × the distance above the rect's
// vertical centre): the SDF runs in the unsheared frame, so the corners stay
// rounded and the slanted edges antialiased. 0 = an upright rect.
//
// A NEGATIVE radius (never a corner radius) marks a patterned text decoration
// (see `Deco`): `-radius = style · DECO_STYLE_UNIT + param`. The fragment then
// draws an anti-aliased undercurl / round dots / even dashes inside the rect
// instead of filling it — one instance per underline run.
const QUAD_SHADER: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,   // pixel offset from the rect center
    @location(2) hsize: vec2<f32>,   // rect half-size in pixels (NOT `half` — a Metal reserved type)
    @location(3) radius: f32,        // corner radius in pixels
    @location(4) @interpolate(flat) deco: vec2<f32>, // decoration (style, param); style 0 = none
};
const DECO_STYLE_UNIT: f32 = 4096.0;
@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) round: vec4<f32>,   // shear, _unused, radius (< 0: decoration), _pad
) -> VsOut {
    var corners = array<vec2<f32>, 6>(vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let c = corners[vi];
    var px = rect.xy + c * rect.zw;
    px.x += round.x * (0.5 - c.y) * rect.w;
    let ndc = vec2(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.pos = vec4(ndc, 0.0, 1.0);
    o.color = color;
    let hsize = rect.zw * 0.5;
    o.local = (c - vec2(0.5, 0.5)) * rect.zw; // center-relative pixel coord
    o.hsize = hsize;
    o.radius = max(round.z, 0.0);
    var deco = vec2(0.0, 0.0);
    if (round.z < 0.0) {
        let style = floor(-round.z / DECO_STYLE_UNIT);
        deco = vec2(style, -round.z - style * DECO_STYLE_UNIT);
    }
    o.deco = deco;
    return o;
}
fn s2l(c: f32) -> f32 { if (c <= 0.04045) { return c / 12.92; } return pow((c + 0.055) / 1.055, 2.4); }
// Signed distance to a rounded rect (negative inside).
fn sd_round_rect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}
// Coverage of a patterned underline (`in.deco.x` = 1 undercurl, 2 dotted,
// 3 dashed). x phases run on the ABSOLUTE framebuffer x, so adjacent runs (a
// colour change mid-word) continue the same wave / dot rhythm seamlessly.
fn deco_coverage(in: VsOut) -> f32 {
    let style = u32(in.deco.x + 0.5);
    let x = in.pos.x;
    if (style == 1u) {
        // Undercurl: a sine of wavelength `param` (one cell) through the band's
        // centre line. The band is 2.15·t + 1 px tall for a weight t: stroke
        // 0.65·t, amplitude 0.75·t, half a pixel of anti-aliasing above/below.
        let t = max((in.hsize.y * 2.0 - 1.0) / 2.15, 1.0);
        let stroke = 0.65 * t;
        let amp = 0.75 * t;
        let k = 6.2831853 / max(in.deco.y, 4.0);
        let yc = amp * sin(k * x);
        let slope = amp * k * cos(k * x);
        let d = abs(in.local.y - yc) / sqrt(1.0 + slope * slope);
        return clamp(stroke * 0.5 + 0.5 - d, 0.0, 1.0);
    }
    if (style == 2u) {
        // Dotted: round dots as wide as the band is tall, every `param` px.
        let th = in.hsize.y * 2.0;
        let pitch = max(in.deco.y, th + 1.0);
        let dx = x - (floor(x / pitch) + 0.5) * pitch;
        let r = max(th * 0.5, 0.75);
        return clamp(r + 0.5 - length(vec2(dx, in.local.y)), 0.0, 1.0);
    }
    if (style == 3u) {
        // Dashed: one dash per cell (`param` = cell width; the run starts on a
        // cell edge), the gap centred on the cell boundary — even everywhere.
        let cw = max(in.deco.y, 2.0);
        let t = (in.local.x + in.hsize.x) - floor((in.local.x + in.hsize.x) / cw) * cw;
        let a = cw * 0.2;
        return clamp(min(t - a, cw - a - t) + 0.5, 0.0, 1.0);
    }
    return 1.0;
}
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var cov = 1.0;
    if (in.radius > 0.0) {
        let r = min(in.radius, min(in.hsize.x, in.hsize.y));
        let d = sd_round_rect(in.local, in.hsize, r);
        cov = 1.0 - smoothstep(-0.75, 0.75, d);
    } else if (in.deco.x > 0.5) {
        cov = deco_coverage(in);
    }
    return vec4(s2l(in.color.r), s2l(in.color.g), s2l(in.color.b), in.color.a * cov);
}
"#;

/// A patterned text decoration drawn procedurally by the quad shader (see
/// [`Rect::decoration`]): one instance spans a whole underline run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deco {
    /// A sine wave (wavelength = the param, one cell) filling a band 2.15·t + 1
    /// tall (stroke 0.65·t, amplitude 0.75·t; see `undercurl_band`).
    Undercurl = 1,
    /// Round dots as wide as the rect is tall, the param apart.
    Dotted = 2,
    /// One dash per cell (param = cell width), the gaps on the cell boundaries.
    Dashed = 3,
}

/// One quad's 12 instance floats — the 48-byte stride the pipeline declares:
/// rect (xywh), color (rgba, 0..1), shape params (shear, the old half-height
/// slot — unused by the shader, which derives the half-size from rect — corner
/// radius or a negative decoration code, _pad).
#[inline]
fn instance_floats(r: &Rect) -> [f32; 12] {
    let c = r.color.map(|v| v as f32 / 255.0);
    [r.x, r.y, r.w, r.h, c[0], c[1], c[2], c[3], r.shear, r.h * 0.5, r.radius, 0.0]
}

/// `-radius = style · DECO_STYLE_UNIT + param` (the shader's decoding): params
/// (a pitch or a cell width, in px) stay below it.
const DECO_STYLE_UNIT: f32 = 4096.0;

/// The `Rect::radius` encoding of a decoration (`param` clamped into its slot).
fn deco_radius(style: Deco, param: f32) -> f32 {
    let p = if param.is_finite() { param.clamp(0.0, DECO_STYLE_UNIT - 1.0) } else { 0.0 };
    -(style as u32 as f32 * DECO_STYLE_UNIT + p)
}

/// Decode a `Rect::radius` the way the shader does: `Some((style, param))` for a
/// decoration, `None` for a plain / rounded quad.
#[cfg(test)]
fn decode_deco(radius: f32) -> Option<(u32, f32)> {
    (radius < 0.0).then(|| {
        let style = (-radius / DECO_STYLE_UNIT).floor();
        (style as u32, -radius - style * DECO_STYLE_UNIT)
    })
}

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 4],
    /// Corner radius in pixels. `0.0` = sharp rectangle (the default), so all
    /// existing quads render unchanged. A positive value rounds the corners via
    /// an antialiased rounded-rect SDF in the shader. Negative values encode a
    /// patterned decoration — build those with [`Rect::decoration`].
    pub radius: f32,
    /// Horizontal slant: x shifts by `shear × (h/2 − y_local)` px, so a positive
    /// value leans the top edge right (`/`). `0.0` (the default) = upright. The
    /// tab bar's slant / powerline styles use it; give a sheared quad a small
    /// `radius` so its slanted edges are antialiased (radius 0 skips the SDF).
    pub shear: f32,
}

impl Default for Rect {
    fn default() -> Self {
        Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0, color: [0, 0, 0, 0], radius: 0.0, shear: 0.0 }
    }
}

impl Rect {
    /// A sharp (radius 0) rect — convenience matching the old field-only literal.
    pub fn new(x: f32, y: f32, w: f32, h: f32, color: [u8; 4]) -> Self {
        Rect { x, y, w, h, color, radius: 0.0, shear: 0.0 }
    }

    /// A rounded rect with the given corner `radius` in pixels.
    pub fn rounded(x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32) -> Self {
        Rect { x, y, w, h, color, radius, shear: 0.0 }
    }

    /// The x extent `[left, right]` this quad COVERS once sheared (equal to
    /// `[x, x + w]` when upright): the slant moves the top and bottom edges by
    /// `±shear·h/2`.
    pub fn sheared_x_span(&self) -> (f32, f32) {
        let lean = (self.shear * self.h * 0.5).abs();
        (self.x - lean, self.x + self.w + lean)
    }

    /// A patterned text decoration over `[x, x+w) × [y, y+h)`, anti-aliased by
    /// the shader (`param`: the undercurl wavelength / dot pitch / dash cell
    /// width, px). Encoded in the otherwise unused NEGATIVE radius range, so the
    /// instance layout is unchanged.
    pub fn decoration(x: f32, y: f32, w: f32, h: f32, color: [u8; 4], style: Deco, param: f32) -> Self {
        Rect { x, y, w, h, color, radius: deco_radius(style, param), shear: 0.0 }
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
            self.instance_scratch.extend_from_slice(&instance_floats(r));
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

        let count = self.upload(device, queue, screen_w, screen_h, rects);

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
            self.draw_uploaded(&mut pass, count);
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Upload `rects` (instances + the screen-size uniform) for a draw recorded by
    /// [`Self::draw_uploaded`] into a CALLER-owned render pass — so several layers
    /// can share one pass and one queue submit (each pass + submit costs tens of µs
    /// of CPU on the frame path). The upload lands at the next `queue.submit`, so
    /// that submit must carry the recorded draw before this layer uploads again.
    /// Returns the instance count to pass to `draw_uploaded`.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
    ) -> u32 {
        let uniform_data: [f32; 4] = [screen_w as f32, screen_h as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform_data));
        if !rects.is_empty() {
            self.upload_instances(device, queue, rects);
        }
        rects.len() as u32
    }

    /// Record the draw of the last [`Self::upload`] (`count` instances) into `pass`.
    pub fn draw_uploaded(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        // No instances (or no upload yet): nothing to draw.
        let Some(buf) = self.instance_buf.as_ref().filter(|_| count > 0) else { return };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, buf.slice(..));
        pass.draw(0..6, 0..count);
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

/// Where a row's text decorations go, in px from the row's top (the grid
/// layer measures it from its font: `TextLayer::underline_geom`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnderlineGeom {
    /// Top of a (single / dotted / dashed / upper double) underline stroke —
    /// the font's own underline position below the baseline.
    pub top: f32,
    /// Bottom of the text's line box: the undercurl rests on it and no
    /// underline reaches below it. The cell bottom at the default line height.
    pub bottom: f32,
    /// Stroke thickness of underlines and strikethrough.
    pub thickness: f32,
}

impl UnderlineGeom {
    /// Everything resting on the cell bottom — the look before the font's
    /// underline position was used; what a caller without a text layer gets.
    pub fn cell_bottom(cell_h: f32) -> Self {
        let th = decoration_thickness(cell_h);
        UnderlineGeom { top: cell_h - th, bottom: cell_h, thickness: th }
    }

    /// Clamped into a `cell_h`-tall row: a thickness of at least 1 px, the
    /// bottom inside the row, the stroke above the bottom. Garbage (NaN) falls
    /// back to [`Self::cell_bottom`].
    pub(crate) fn within(self, cell_h: f32) -> Self {
        if !(self.top.is_finite() && self.bottom.is_finite() && self.thickness.is_finite()) {
            return Self::cell_bottom(cell_h);
        }
        let thickness = self.thickness.clamp(1.0, (cell_h * 0.5).max(1.0));
        let bottom = self.bottom.clamp(thickness, cell_h.max(thickness));
        let top = self.top.clamp(0.0, bottom - thickness);
        UnderlineGeom { top, bottom, thickness }
    }
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

/// Height of the undercurl band for an underline thickness `th` (see the
/// shader's `deco_coverage`): weight `t = max(th, 1.5)`, band `2.15·t + 1`.
#[inline]
fn undercurl_band(th: f32) -> f32 {
    2.15 * th.max(1.5) + 1.0
}

/// Emit the quads for ONE horizontal underline run of the given `style` spanning
/// `[x0, x0+run_w)` in the row whose top is `row_top`, placed by `ul` (already
/// `within` the row). Single/double are wide quads; dotted/dashed/undercurl are
/// ONE patterned instance each, anti-aliased by the shader (`Rect::decoration`).
/// `color` is the resolved underline color (uline, already falling back to fg
/// cell-side). `x0` is a cell edge.
#[allow(clippy::too_many_arguments)]
fn emit_underline(
    out: &mut Vec<Rect>,
    style: u8,
    x0: f32,
    run_w: f32,
    row_top: f32,
    ul: UnderlineGeom,
    cell_w: f32,
    color: [u8; 4],
) {
    use jetty_core::attr;
    let th = ul.thickness;
    let y = row_top + ul.top; // top of the stroke, at the font's underline position
    let floor = row_top + ul.bottom; // nothing reaches below the line box
    match style {
        attr::UL_SINGLE => {
            out.push(Rect::new(x0, y, run_w, th, color));
        }
        attr::UL_DOUBLE => {
            // Two strokes a `th` gap apart, the upper on the underline position —
            // lifted together when the lower one would pass the line box bottom.
            let lower = (y + 2.0 * th).min(floor - th);
            out.push(Rect::new(x0, lower - 2.0 * th, run_w, th, color));
            out.push(Rect::new(x0, lower, run_w, th, color));
        }
        attr::UL_DOTTED => {
            // Round `th`-wide dots on a `2·th` pitch (at least 3 px).
            out.push(Rect::decoration(x0, y, run_w, th, color, Deco::Dotted, (th * 2.0).max(3.0)));
        }
        attr::UL_DASHED => {
            // One 0.6-cell dash per cell, the gaps centred on the cell edges.
            out.push(Rect::decoration(x0, y, run_w, th, color, Deco::Dashed, cell_w));
        }
        attr::UL_UNDERCURL => {
            // A sine of one cell's wavelength in a band resting on the line box
            // bottom (the descender zone, clear of the baseline): weight
            // t = max(th, 1.5) → stroke 0.65·t, amplitude 0.75·t, band 2.15·t + 1
            // (the shader decodes t from the height).
            let band = undercurl_band(th).min(ul.bottom);
            out.push(Rect::decoration(x0, floor - band, run_w, band, color, Deco::Undercurl, cell_w.max(4.0)));
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
///
/// Underlines rest on the cell bottom; see [`text_decoration_rects_at`] to place
/// them by the font instead (`TextLayer::underline_geom`).
pub fn text_decoration_rects(
    snap: &jetty_core::GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    y_offset: f32,
    out: &mut Vec<Rect>,
) {
    text_decoration_rects_at(snap, cell_w, cell_h, UnderlineGeom::cell_bottom(cell_h), y_offset, out);
}

/// [`text_decoration_rects`] with underlines placed by `ul` (px from each row's
/// top, clamped into the row): the font's underline position, and the line box
/// bottom for the undercurl — so a taller line height keeps them under the text
/// instead of at the cell edge.
pub fn text_decoration_rects_at(
    snap: &jetty_core::GridSnapshot,
    cell_w: f32,
    cell_h: f32,
    ul: UnderlineGeom,
    y_offset: f32,
    out: &mut Vec<Rect>,
) {
    use jetty_core::attr;
    let ul = ul.within(cell_h);
    let th = ul.thickness;
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
            let row_top = y_offset + row as f32 * cell_h;
            let color = [uline[0], uline[1], uline[2], 255];
            emit_underline(out, style, x0, run_w, row_top, ul, cell_w, color);
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
    link_underline_rects_at(spans, color, cell_w, cell_h, UnderlineGeom::cell_bottom(cell_h), y_offset)
}

/// [`link_underline_rects`] placed by `ul` (see [`text_decoration_rects_at`]) —
/// pass the grid layer's `TextLayer::underline_geom` so link and SGR
/// underlines line up.
pub fn link_underline_rects_at(
    spans: &[(usize, usize, usize)],
    color: [u8; 4],
    cell_w: f32,
    cell_h: f32,
    ul: UnderlineGeom,
    y_offset: f32,
) -> Vec<Rect> {
    let ul = ul.within(cell_h);
    spans
        .iter()
        .map(|&(row, c0, c1)| {
            Rect::new(
                c0 as f32 * cell_w,
                y_offset + row as f32 * cell_h + ul.top,
                (c1 - c0 + 1) as f32 * cell_w,
                ul.thickness,
                color,
            )
        })
        .collect()
}

/// Slim left-edge accent bars marking OSC 133 failed-command (`D;<nonzero>`)
/// prompt rows — an IDE "changed line" style gutter tick. `rows` are 0-based
/// VIEWPORT rows already filtered to the visible grid (typically empty).
/// `width_px` is the physical bar width (HiDPI-scaled by the caller); drawn at
/// window x `x` — in the left padding (`grid_geom::failed_marker_x`), so the
/// bar never covers column 0's glyphs — rounded so it reads as an accent rather
/// than a hard block. Shared by the main window, detached windows, and
/// jetty-shot so all three stay identical.
pub fn failed_marker_rects(
    rows: &[u16],
    cell_h: f32,
    y_offset: f32,
    x: f32,
    width_px: f32,
    color: [u8; 4],
) -> Vec<Rect> {
    let radius = (width_px * 0.5).min(cell_h * 0.5);
    rows.iter()
        .map(|&row| {
            Rect::rounded(x, y_offset + row as f32 * cell_h, width_px, cell_h, color, radius)
        })
        .collect()
}

/// Scrollbar thumb column width in LOGICAL px (scaled by the window's DPI —
/// [`ScrollbarTrack::thumb_w`]): the grab area at the window's right edge.
pub const SCROLLBAR_W: f32 = 14.0;

/// Gap (logical px) between the track ends and the bars, so the thumb stays
/// clear of the tab bar / window controls — and, as the gutter's extra width,
/// between the text and the thumb column. Shared by the thumb geometry and the
/// drag inverse below so drawing and dragging can never disagree.
pub(crate) const SCROLLBAR_GAP: f32 = 4.0;

/// Minimum thumb height (logical px) so a huge scrollback still leaves a
/// grabbable thumb.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;

/// Horizontal inset (logical px) of the DRAWN thumb inside its grab column: a
/// slim rounded pill, clear of the window edge, while the whole column still
/// takes the click.
const SCROLLBAR_INSET: f32 = 3.0;

/// Width (physical px) of the scrollbar gutter a window's grid reserves at its
/// right edge: the thumb column plus the gap to the text, DPI-scaled to whole
/// pixels (18 px at 1×, 36 px at 2×).
pub fn scrollbar_gutter_px(scale: f32) -> f32 {
    ((SCROLLBAR_W + SCROLLBAR_GAP) * sane_scale(scale)).round()
}

fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }
}

/// Where a window's scrollbar runs (physical px): the grid BAND — below a top
/// tab/title bar, above a bottom tab bar and the status strip — at the window's
/// right edge, sized for the window's DPI. The ONE description drawing, hit
/// testing and dragging all take, so they can never disagree (the old API
/// subtracted the unscaled 36 px bar height from the surface: on a 2× display
/// the track overshot into the status strip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarTrack {
    /// Window width; the thumb hugs its right edge.
    pub screen_w: f32,
    /// y of the grid band's top edge (the top bar's bottom, else 0).
    pub band_top: f32,
    /// y of the grid band's bottom edge (a bottom bar's top, else the status
    /// strip's top, else the window bottom).
    pub band_bottom: f32,
    /// The window's DPI scale factor.
    pub scale: f32,
}

impl ScrollbarTrack {
    pub fn new(screen_w: f32, band_top: f32, band_bottom: f32, scale: f32) -> Self {
        Self { screen_w, band_top, band_bottom, scale: sane_scale(scale) }
    }

    /// The thumb's grab-column width (whole physical px).
    pub fn thumb_w(&self) -> f32 {
        (SCROLLBAR_W * self.scale).round().max(1.0)
    }

    /// Whether window point `(x, y)` is over the scrollbar GUTTER (the band's
    /// right-edge strip the grid leaves free) — the `scrollbar = "auto"` hover.
    pub fn gutter_contains(&self, x: f32, y: f32) -> bool {
        x >= self.screen_w - scrollbar_gutter_px(self.scale)
            && x <= self.screen_w
            && y >= self.band_top
            && y < self.band_bottom
    }

    /// `(top, height)` of the thumb's travel track: the band minus a gap at
    /// each end, whole pixels.
    fn track(&self) -> (f32, f32) {
        let gap = (SCROLLBAR_GAP * self.scale).round();
        let top = self.band_top + gap;
        (top, (self.band_bottom - gap - top).max(0.0))
    }

    /// `(track top, travel, thumb height)` for `rows` visible rows over
    /// `scroll_max` lines of history. The thumb is whole pixels and never
    /// shorter than the (scaled) minimum; `travel` is 0 when it fills the track.
    fn thumb(&self, rows: usize, scroll_max: usize) -> (f32, f32, f32) {
        let (top, track_h) = self.track();
        let total = (rows + scroll_max).max(1);
        let min = (SCROLLBAR_MIN_THUMB * self.scale).round();
        let thumb_h = (track_h * rows as f32 / total as f32).round().max(min);
        (top, (track_h - thumb_h).max(0.0), thumb_h)
    }
}

/// The scrollbar thumb's GRAB rect (the whole thumb column at the window's
/// right edge, whole pixels) — the canonical geometry hit-testing and dragging
/// use; [`scrollbar_rect`] draws a slimmer pill inside it. `None` when
/// `scroll_max == 0` (nothing to scroll).
pub fn scrollbar_rect_geom(
    rows: usize,
    scroll_offset: usize,
    scroll_max: usize,
    track: &ScrollbarTrack,
    thumb: [u8; 4],
) -> Option<Rect> {
    if scroll_max == 0 {
        return None;
    }
    let (top, travel, thumb_h) = track.thumb(rows, scroll_max);
    let frac = (scroll_max - scroll_offset.min(scroll_max)) as f32 / scroll_max as f32;
    let w = track.thumb_w();
    Some(Rect {
        x: track.screen_w - w,
        y: (top + frac * travel).round(),
        w,
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
    track: &ScrollbarTrack,
) -> Option<usize> {
    if scroll_max == 0 {
        return None;
    }
    let (top, travel, _) = track.thumb(rows, scroll_max);
    if travel <= 0.0 {
        return None;
    }
    let thumb_top = (cursor_y - top - grab_dy).clamp(0.0, travel);
    // frac=0 → thumb at top → scroll_offset=max (oldest history)
    // frac=1 → thumb at bottom → scroll_offset=0 (live bottom)
    let frac = thumb_top / travel;
    Some(((1.0 - frac) * scroll_max as f32).round() as usize)
}

/// The scrollbar thumb's color for `theme` — the ONE definition the main
/// window, detached windows and jetty-shot draw with: the palette's
/// thin-mark role (`UiPalette::text_hint`), pushed until it contrasts at
/// least 3:1 with the theme background it sits on, opaque (a translucent
/// thumb would composite below the floor). The old fixed 35 % bg→fg blend
/// read on dark themes but fell to ~1.5:1 on light ones.
pub fn scrollbar_thumb_color(theme: &jetty_core::Theme) -> [u8; 4] {
    let ui = crate::UiPalette::cached(theme);
    let [r, g, b] = crate::ensure_contrast(ui.text_hint, &[ui.bg], 3.0);
    [r, g, b, 255]
}

/// The DRAWN scrollbar thumb for `snapshot`: the grab rect
/// ([`scrollbar_rect_geom`]) inset horizontally into a rounded pill
/// (DPI-scaled, whole pixels). `None` when there is no history.
pub fn scrollbar_rect(
    snapshot: &jetty_core::GridSnapshot,
    track: &ScrollbarTrack,
    thumb: [u8; 4],
) -> Option<Rect> {
    let mut r = scrollbar_rect_geom(snapshot.rows, snapshot.scroll_offset, snapshot.scroll_max, track, thumb)?;
    let inset = (SCROLLBAR_INSET * track.scale).round().min((r.w - 2.0) * 0.5).max(0.0);
    r.x += inset;
    r.w -= 2.0 * inset;
    r.radius = r.w * 0.5;
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetty_core::{attr, CellSnapshot, CursorShapeSnap, GridSnapshot};

    /// The quad WGSL (now with the shear term) parses and passes naga's
    /// validator — the always-run gate for a shader with no GPU in CI.
    #[test]
    fn quad_shader_passes_naga_validation() {
        let module = naga::front::wgsl::parse_str(QUAD_SHADER).expect("QUAD_SHADER must parse");
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("QUAD_SHADER must pass naga validation");
    }

    #[test]
    fn shear_defaults_to_upright_and_widens_the_covered_span() {
        // Every existing constructor stays upright (byte-identical instances).
        assert_eq!(Rect::default().shear, 0.0);
        assert_eq!(Rect::new(1.0, 2.0, 3.0, 4.0, [0; 4]).shear, 0.0);
        assert_eq!(Rect::rounded(1.0, 2.0, 3.0, 4.0, [0; 4], 2.0).shear, 0.0);
        let upright = Rect::new(10.0, 0.0, 50.0, 20.0, [0; 4]);
        assert_eq!(upright.sheared_x_span(), (10.0, 60.0));
        // shear 0.5 over a 20px-tall quad leans each edge 5px either way.
        let slanted = Rect { shear: 0.5, ..upright };
        assert_eq!(slanted.sheared_x_span(), (5.0, 65.0));
        assert_eq!(Rect { shear: -0.5, ..upright }.sheared_x_span(), (5.0, 65.0));
    }

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
            graphemes: Vec::new(),
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
    fn dotted_dashed_undercurl_are_one_patterned_instance_per_run() {
        for (style, deco) in [(attr::UL_DOTTED, Deco::Dotted), (attr::UL_DASHED, Deco::Dashed), (attr::UL_UNDERCURL, Deco::Undercurl)] {
            let mut g = grid(6, 2);
            for c in 0..6 {
                g.cells[c].attrs = style << attr::UL_SHIFT;
            }
            // A colour change splits the run: two instances, sharing the edge.
            g.cells[3].uline = [9, 9, 9];
            g.cells[4].uline = [9, 9, 9];
            g.cells[5].uline = [9, 9, 9];
            let mut out = Vec::new();
            text_decoration_rects(&g, 10.0, 20.0, 0.0, &mut out);
            assert_eq!(out.len(), 2, "style {style}: one instance per run");
            assert_eq!((out[0].x, out[0].w, out[1].x, out[1].w), (0.0, 30.0, 30.0, 30.0));
            for r in &out {
                let (s, param) = decode_deco(r.radius).expect("a decoration");
                assert_eq!(s, deco as u32, "style {style}");
                assert!(param > 0.0);
                // The pattern rests on the cell bottom, inside the row.
                assert_eq!(r.y + r.h, 20.0);
                assert!(r.y >= 0.0);
            }
        }
    }

    #[test]
    fn underlines_are_placed_by_the_underline_geometry() {
        let mut g = grid(6, 2);
        for c in 0..2 {
            g.cells[6 + c].attrs = attr::UL_SINGLE << attr::UL_SHIFT; // row 1
            g.cells[8 + c].attrs = attr::UL_UNDERCURL << attr::UL_SHIFT;
            g.cells[10 + c].attrs = attr::UL_DOUBLE << attr::UL_SHIFT;
        }
        let row_top = 5.0 + 32.0;
        // `text_decoration_rects` = the cell-bottom geometry.
        let (mut a, mut b) = (Vec::new(), Vec::new());
        text_decoration_rects(&g, 10.0, 32.0, 5.0, &mut a);
        text_decoration_rects_at(&g, 10.0, 32.0, UnderlineGeom::cell_bottom(32.0), 5.0, &mut b);
        assert_eq!(a.iter().map(|r| (r.y, r.h)).collect::<Vec<_>>(), b.iter().map(|r| (r.y, r.h)).collect::<Vec<_>>());
        assert_eq!(a[0].y + a[0].h, row_top + 32.0, "single on the cell bottom");
        // A font geometry: single at `top`, the undercurl band on `bottom`, the
        // double's upper stroke at `top` and its lower one above `bottom`.
        let ul = UnderlineGeom { top: 20.0, bottom: 25.0, thickness: 2.0 };
        let mut c = Vec::new();
        text_decoration_rects_at(&g, 10.0, 32.0, ul, 5.0, &mut c);
        assert_eq!((c[0].y, c[0].h), (row_top + 20.0, 2.0), "single");
        assert_eq!(c[1].y + c[1].h, row_top + 25.0, "undercurl rests on the line box");
        let (upper, lower) = (c[2], c[3]);
        assert!(lower.y + lower.h <= row_top + 25.0 && upper.y == lower.y - 4.0, "double: {:?} {:?}", (upper.y, upper.h), (lower.y, lower.h));
        // Garbage is clamped into the row (never below it).
        let mut d = Vec::new();
        let wild = UnderlineGeom { top: 99.0, bottom: 99.0, thickness: 0.0 };
        text_decoration_rects_at(&g, 10.0, 32.0, wild, 5.0, &mut d);
        assert!(d.iter().all(|r| r.y >= row_top && r.y + r.h <= row_top + 32.0), "{:?}", d.iter().map(|r| (r.y, r.h)).collect::<Vec<_>>());
        let nan = UnderlineGeom { top: f32::NAN, bottom: 25.0, thickness: 2.0 };
        assert_eq!(nan.within(32.0), UnderlineGeom::cell_bottom(32.0));
        // Link underlines take the single-underline place.
        let l = link_underline_rects_at(&[(1, 0, 1)], [0; 4], 10.0, 32.0, ul, 5.0);
        assert_eq!((l[0].y, l[0].h), (c[0].y, c[0].h));
        let l = link_underline_rects(&[(1, 0, 1)], [0; 4], 10.0, 32.0, 5.0);
        assert_eq!(l[0].y + l[0].h, row_top + 32.0);
    }

    #[test]
    fn decoration_encoding_round_trips_and_stays_out_of_rounding() {
        for deco in [Deco::Undercurl, Deco::Dotted, Deco::Dashed] {
            for param in [3.0f32, 9.6328125, 19.265625, 4000.0] {
                let r = Rect::decoration(0.0, 0.0, 10.0, 4.0, [0; 4], deco, param);
                assert!(r.radius < 0.0, "never a corner radius");
                let (s, p) = decode_deco(r.radius).unwrap();
                assert_eq!(s, deco as u32);
                assert!((p - param).abs() < 1e-3, "{param} → {p}");
            }
        }
        // Out-of-range params are clamped into their slot, never into the style.
        let r = Rect::decoration(0.0, 0.0, 1.0, 1.0, [0; 4], Deco::Dotted, 1e9);
        assert_eq!(decode_deco(r.radius).unwrap().0, Deco::Dotted as u32);
        let r = Rect::decoration(0.0, 0.0, 1.0, 1.0, [0; 4], Deco::Dashed, f32::NAN);
        assert_eq!(decode_deco(r.radius), Some((Deco::Dashed as u32, 0.0)));
        // Plain and rounded quads decode as no decoration.
        assert_eq!(decode_deco(0.0), None);
        assert_eq!(decode_deco(6.0), None);
    }

    /// The quad shader (rounded rects + patterned decorations) must parse and
    /// pass naga's validator — the always-run gate for its source, no GPU.
    #[test]
    fn quad_shader_compiles() {
        let module = naga::front::wgsl::parse_str(QUAD_SHADER).expect("QUAD_SHADER must parse as valid WGSL");
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("QUAD_SHADER must pass naga validation");
        // The shader decodes with the same unit the CPU encodes with.
        assert!(QUAD_SHADER.contains(&format!("const DECO_STYLE_UNIT: f32 = {DECO_STYLE_UNIT:.1};")));
    }

    /// The instance layout the shader reads: three vec4s (rect, color, round)
    /// per quad — 12 floats, 48 bytes — and the 16-byte screen uniform.
    #[test]
    fn quad_instance_and_uniform_layout() {
        let plain = instance_floats(&Rect::new(1.0, 2.0, 3.0, 4.0, [255, 0, 0, 255]));
        let deco = instance_floats(&Rect::decoration(0.0, 0.0, 8.0, 2.0, [0; 4], Deco::Dashed, 9.5));
        assert_eq!(std::mem::size_of_val(&plain), 48, "array_stride");
        assert_eq!(&plain[..8], &[1.0, 2.0, 3.0, 4.0, 1.0, 0.0, 0.0, 1.0], "rect + color");
        assert_eq!((plain[8], plain[10]), (0.0, 0.0), "upright, sharp");
        assert!(deco[10] < 0.0 && deco[8] == 0.0, "a decoration rides the radius slot, never shears");
        assert_eq!((plain[11], deco[11]), (0.0, 0.0), "the 12th float stays free");
        assert_eq!(std::mem::size_of::<[f32; 4]>(), 16, "Screen {{ size, _pad }}");
        assert!(QUAD_SHADER.contains("struct Screen { size: vec2<f32>, _pad: vec2<f32> };"));
    }

    /// A CPU model of the shader's patterns (same formulas), checked for the
    /// properties that make them read well: the undercurl continues across a
    /// run boundary, dots are evenly spaced and round, dashes are centred in
    /// their cells with the gaps on the cell edges.
    #[test]
    fn decoration_patterns_are_continuous_and_even() {
        // Undercurl: centre line at absolute x — the same value whichever run
        // (rect) the pixel belongs to.
        let curl_y = |x: f32, period: f32, th: f32| th * (std::f32::consts::TAU / period * x).sin();
        let (period, th) = (9.6328125f32, 2.0f32);
        let edge = 3.0 * period; // a run boundary on a cell edge
        let left = curl_y(edge - 0.001, period, th);
        let right = curl_y(edge + 0.001, period, th);
        assert!((left - right).abs() < 0.01, "the wave is continuous across runs");
        // ... and it repeats every cell.
        assert!((curl_y(0.5, period, th) - curl_y(0.5 + 5.0 * period, period, th)).abs() < 1e-3);
        // Dots: centres every `pitch` px of absolute x.
        let pitch = 4.0f32;
        let centre = |x: f32| (x / pitch).floor() * pitch + pitch * 0.5;
        assert_eq!(centre(0.5), 2.0);
        assert_eq!(centre(5.5), 6.0);
        assert_eq!(centre(9.0) - centre(5.0), pitch);
        // Dashes: within each cell [0.2, 0.8) of the width is inked.
        let dash = |t: f32, cw: f32| {
            let t = t - (t / cw).floor() * cw;
            let a = cw * 0.2;
            (t - a).min(cw - a - t) + 0.5
        };
        let cw = 10.0;
        assert!(dash(5.0, cw) >= 1.0 && dash(15.0, cw) >= 1.0, "dash centres");
        assert!(dash(0.5, cw) <= 0.0 && dash(9.5, cw) <= 0.0 && dash(10.5, cw) <= 0.0, "gaps on the cell edges");
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
    fn scrollbar_offset_from_cursor_none_when_no_history() {
        // No scrollback → nothing to drag.
        let t = ScrollbarTrack::new(800.0, 36.0, 640.0, 1.0);
        assert_eq!(scrollbar_offset_from_cursor(100.0, 0.0, 40, 0, &t), None);
        // Window so short the (min-height) thumb fills the track → no travel.
        let t = ScrollbarTrack::new(800.0, 36.0, 44.0, 1.0);
        assert_eq!(scrollbar_offset_from_cursor(40.0, 0.0, 1000, 1, &t), None);
    }

    #[test]
    fn scrollbar_offset_from_cursor_track_ends() {
        let (rows, max, h) = (40usize, 200usize, 640.0f32);
        // Top bar 36, status strip 22.
        let t = ScrollbarTrack::new(800.0, 36.0, h - 22.0, 1.0);
        // Cursor at the track top (band top + GAP) → oldest history.
        let track_top = 36.0 + SCROLLBAR_GAP;
        assert_eq!(scrollbar_offset_from_cursor(track_top, 0.0, rows, max, &t), Some(max));
        // Beyond the top end clamps to the same extreme.
        assert_eq!(scrollbar_offset_from_cursor(-500.0, 0.0, rows, max, &t), Some(max));
        // At/below the track bottom → live bottom (offset 0), clamped too.
        assert_eq!(scrollbar_offset_from_cursor(h, 0.0, rows, max, &t), Some(0));
        assert_eq!(scrollbar_offset_from_cursor(h + 500.0, 0.0, rows, max, &t), Some(0));
    }

    /// The scrollbar track of a `w` × `h` window at DPI `scale` with the app's
    /// chrome at the default UI font (36 px bar, 22 px status strip at 1×,
    /// whole px): the bar on top or at the bottom (above the strip), the
    /// strip on or off.
    fn app_track(scale: f32, w: f32, h: f32, bar_bottom: bool, status: bool) -> ScrollbarTrack {
        let bar_h = (36.0 * scale).round();
        let status_h = if status { (22.0 * scale).round() } else { 0.0 };
        let (top, bottom) = if bar_bottom { (0.0, h - status_h - bar_h) } else { (bar_h, h - status_h) };
        ScrollbarTrack::new(w, top, bottom, scale)
    }

    /// Every chrome layout the scrollbar lives in, at 1× and 2×.
    fn every_layout() -> Vec<(String, ScrollbarTrack)> {
        let mut out = Vec::new();
        for scale in [1.0f32, 2.0] {
            for bar_bottom in [false, true] {
                for status in [true, false] {
                    let (w, h) = (900.0 * scale, 640.0 * scale);
                    let name = format!("{scale}× bar {} status {status}", if bar_bottom { "bottom" } else { "top" });
                    out.push((name, app_track(scale, w, h, bar_bottom, status)));
                }
            }
        }
        out
    }

    #[test]
    fn scrollbar_thumb_stays_inside_the_band_at_1x_and_2x() {
        for (name, t) in every_layout() {
            let gap = (SCROLLBAR_GAP * t.scale).round();
            let (rows, max) = (30usize, 500usize);
            let oldest = scrollbar_rect_geom(rows, max, max, &t, [0; 4]).unwrap();
            let live = scrollbar_rect_geom(rows, 0, max, &t, [0; 4]).unwrap();
            assert_eq!(oldest.y, t.band_top + gap, "{name}: oldest history at the band top");
            assert_eq!(live.y + live.h, t.band_bottom - gap, "{name}: live bottom at the band bottom");
            for r in [oldest, live] {
                assert_eq!(r.w, (14.0 * t.scale).round(), "{name}: DPI-scaled width");
                assert_eq!(r.x + r.w, t.screen_w, "{name}: hugs the right edge");
                assert!(r.h >= (24.0 * t.scale).round(), "{name}: scaled minimum height");
                for v in [r.x, r.y, r.w, r.h] {
                    assert_eq!(v.fract(), 0.0, "{name}: whole pixels ({} {} {} {})", r.x, r.y, r.w, r.h);
                }
            }
        }
    }

    #[test]
    fn scrollbar_at_2x_no_longer_runs_into_the_status_strip() {
        // The old geometry subtracted the UNSCALED 36 px bar from the surface:
        // at 2× (72 px bar) the live-bottom thumb ended 36 px too low — inside
        // the 44 px status strip. Now it ends a gap above the strip.
        let t = app_track(2.0, 1800.0, 1280.0, false, true);
        let live = scrollbar_rect_geom(30, 0, 500, &t, [0; 4]).unwrap();
        let strip_top = 1280.0 - 44.0;
        assert_eq!(live.y + live.h, strip_top - 8.0);
        // Bottom tab bar: it ends above the bar, which sits above the strip.
        let t = app_track(2.0, 1800.0, 1280.0, true, true);
        let live = scrollbar_rect_geom(30, 0, 500, &t, [0; 4]).unwrap();
        assert_eq!(live.y + live.h, strip_top - 72.0 - 8.0);
    }

    #[test]
    fn drawn_thumb_is_a_rounded_pill_inside_the_grab_column() {
        let mut g = grid(80, 30);
        g.scroll_max = 300;
        g.scroll_offset = 120;
        for (name, t) in every_layout() {
            let grab = scrollbar_rect_geom(g.rows, g.scroll_offset, g.scroll_max, &t, [9; 4]).unwrap();
            let pill = scrollbar_rect(&g, &t, [9; 4]).unwrap();
            let inset = (3.0 * t.scale).round();
            assert_eq!(pill.x, grab.x + inset, "{name}");
            assert_eq!(pill.w, grab.w - 2.0 * inset, "{name}");
            assert_eq!((pill.y, pill.h), (grab.y, grab.h), "{name}: same travel as the grab rect");
            assert_eq!(pill.radius, pill.w / 2.0, "{name}: fully rounded ends");
            assert_eq!(pill.color, [9; 4]);
        }
        // No history: nothing drawn.
        g.scroll_max = 0;
        assert!(scrollbar_rect(&g, &every_layout()[0].1, [9; 4]).is_none());
    }

    #[test]
    fn scrollbar_thumb_reads_on_every_theme() {
        use jetty_core::theme::{theme_at, PRESETS};
        for i in 0..PRESETS.len() {
            let theme = theme_at(i);
            let c = scrollbar_thumb_color(&theme);
            assert_eq!(c[3], 255, "{}: opaque", theme.name);
            let bg = [theme.bg[0], theme.bg[1], theme.bg[2]];
            let ratio = crate::contrast_ratio([c[0], c[1], c[2]], bg);
            assert!(ratio >= 3.0, "{}: thumb {:?} on bg {:?} is only {ratio:.2}:1", theme.name, c, bg);
        }
    }

    #[test]
    fn scrollbar_gutter_scales_and_hover_is_the_band_strip() {
        assert_eq!(scrollbar_gutter_px(1.0), 18.0);
        assert_eq!(scrollbar_gutter_px(2.0), 36.0);
        assert_eq!(scrollbar_gutter_px(1.25), 23.0, "22.5 → whole px");
        assert_eq!(scrollbar_gutter_px(f32::NAN), 18.0);
        let t = app_track(2.0, 1800.0, 1280.0, false, true);
        assert!(t.gutter_contains(1799.0, 500.0));
        assert!(t.gutter_contains(1800.0 - 36.0, 72.0), "the gutter's top-left corner");
        assert!(!t.gutter_contains(1800.0 - 37.0, 500.0), "the text side");
        assert!(!t.gutter_contains(1799.0, 71.0), "the tab bar");
        assert!(!t.gutter_contains(1799.0, 1280.0 - 44.0), "the status strip");
    }

    #[test]
    fn scrollbar_offset_round_trips_with_rect_geom() {
        // Drawing (rect_geom) and dragging (offset_from_cursor) must agree
        // forever: feeding a drawn thumb's y back recovers the same offset —
        // in every chrome layout, at 1× and 2×.
        let (rows, max) = (40usize, 200usize);
        let t = ScrollbarTrack::new(800.0, 36.0, 640.0 - 22.0, 1.0);
        let layouts = std::iter::once(("1× classic".to_string(), t)).chain(every_layout());
        for (name, t) in layouts {
            for off in [0usize, 1, 50, 123, 199, 200] {
                let rect = scrollbar_rect_geom(rows, off, max, &t, [0, 0, 0, 0]).expect("thumb rect");
                let rec = scrollbar_offset_from_cursor(rect.y, 0.0, rows, max, &t).expect("offset");
                assert!((rec as i64 - off as i64).abs() <= 1, "{name}: offset {off} round-tripped to {rec}");
                // A grab anywhere inside the thumb, moved by 0 px, stays put too.
                let grab = rect.h * 0.5;
                let rec = scrollbar_offset_from_cursor(rect.y + grab, grab, rows, max, &t).unwrap();
                assert!((rec as i64 - off as i64).abs() <= 1, "{name}: grabbed mid-thumb {off} → {rec}");
            }
        }
    }
}
