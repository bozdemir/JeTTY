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
        self.render_inner(device, queue, view, screen_w, screen_h, rects, None);
    }

    /// Clear `view` to `clear_color`, then draw `rects` on top. Used for the
    /// per-cell background pass that runs UNDER the terminal text: it owns the
    /// frame clear so `TextLayer::render_to` can run with `LoadOp::Load`.
    ///
    /// Unlike `render`, this always runs (even with no rects) so the clear is not
    /// skipped on a screen made entirely of default-bg cells.
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
        self.render_inner(device, queue, view, screen_w, screen_h, rects, Some(clear_color));
    }

    fn render_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        rects: &[Rect],
        clear_color: Option<wgpu::Color>,
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
/// snapshot's theme bg. Premultiplied by alpha so transparent themes composite
/// correctly under `PreMultiplied` alpha mode (and harmless when alpha == 255).
///
/// This is the same value `TextLayer::render_to` used to clear with; it now lives
/// here so the per-cell background quad pass (which owns the clear) can reuse it.
pub fn default_bg_clear(snapshot: &jetty_core::GridSnapshot) -> wgpu::Color {
    let [br, bg_, bb, ba] = snapshot.bg_rgba;
    let a = ba as f64 / 255.0;
    wgpu::Color {
        r: srgb_to_linear(br) * a,
        g: srgb_to_linear(bg_) * a,
        b: srgb_to_linear(bb) * a,
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

/// Compute the scrollbar thumb rectangle from raw geometry values.
/// This is the canonical geometry computation shared by drawing and hit-testing.
/// Returns `None` when `scroll_max == 0` (nothing to scroll).
pub fn scrollbar_rect_geom(
    rows: usize,
    scroll_offset: usize,
    scroll_max: usize,
    screen_w: u32,
    screen_h: u32,
    top_offset: f32,
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
    const GAP: f32 = 4.0; // keep in sync with apply_scroll_from_cursor
    let track_top = top_offset + GAP;
    let track_h = (screen_h as f32 - crate::TABBAR_H - GAP * 2.0).max(0.0);
    let total = rows + scroll_max;
    let thumb_h = (track_h * rows as f32 / total as f32).max(24.0);
    let frac = (scroll_max - scroll_offset) as f32 / scroll_max as f32;
    let thumb_y = track_top + frac * (track_h - thumb_h);
    let thumb_w = 14.0; // wide enough to grab comfortably
    Some(Rect {
        x: screen_w as f32 - thumb_w,
        y: thumb_y,
        w: thumb_w,
        h: thumb_h,
        color: thumb,
        ..Default::default()
    })
}

pub fn scrollbar_rect(
    snapshot: &jetty_core::GridSnapshot,
    screen_w: u32,
    screen_h: u32,
    top_offset: f32,
    thumb: [u8; 4],
) -> Option<Rect> {
    scrollbar_rect_geom(
        snapshot.rows,
        snapshot.scroll_offset,
        snapshot.scroll_max,
        screen_w,
        screen_h,
        top_offset,
        thumb,
    )
}
