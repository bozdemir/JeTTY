//! Window rim glow — a soft band of color just inside the window's rounded
//! edge, fading inward: the visual bell's `"rim"` and the command status pulse
//! (red when a command fails, the accent when a long one succeeds).
//!
//! One fullscreen-triangle pass, premultiplied "over" blending (so it reads on
//! dark AND light pages), scissored to the four edge strips it can touch
//! ([`rim_regions`]). It follows the window shape — per-position corner radii
//! (square top corners for a top-flush Dropdown) and the Dropdown slide offset
//! — and runs BEFORE the corner mask, which clips its outer feather. The
//! pipeline is built only once a rim effect is enabled; nothing runs while no
//! bell / pulse is playing.

/// 48-byte uniform. No vec3 — the Rust layout matches the WGSL struct (see
/// `rim_uniform_layout`).
///
/// ```text
/// size      vec2<f32>  @  0   (w, h physical px)
/// r_top     f32        @  8   (top corner radius)
/// r_bot     f32        @ 12   (bottom corner radius)
/// color     vec4<f32>  @ 16   (sRGB rgb 0..1, strength 0..1)
/// band      f32        @ 32   (how far inward the glow reaches, px)
/// offset_y  f32        @ 36   (Dropdown slide: the shape moved down)
/// _pad      vec2<f32>  @ 40
///                      total 48
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RimUniform {
    pub size: [f32; 2],
    pub r_top: f32,
    pub r_bot: f32,
    pub color: [f32; 4],
    pub band: f32,
    pub offset_y: f32,
    pub _pad: [f32; 2],
}

impl RimUniform {
    /// A rim of `rgb` at `strength` (0..1), reaching `band` px inward, on a
    /// `w`×`h` window whose corners are `r_top` / `r_bot` px (shape moved
    /// down by `offset_y`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(w: u32, h: u32, r_top: f32, r_bot: f32, rgb: [u8; 3], strength: f32, band: f32, offset_y: f32) -> Self {
        let c = |v: u8| v as f32 / 255.0;
        RimUniform {
            size: [w as f32, h as f32],
            r_top: r_top.max(0.0),
            r_bot: r_bot.max(0.0),
            color: [c(rgb[0]), c(rgb[1]), c(rgb[2]), strength.clamp(0.0, 1.0)],
            band: band.max(1.0),
            offset_y,
            _pad: [0.0; 2],
        }
    }
}

/// The scissor rects `[x, y, w, h]` covering every pixel within `reach` px of
/// the window edge (the band plus the corner radius): four DISJOINT strips —
/// the pass blends, so a pixel drawn twice would glow twice. A target too
/// small for that (or a slid shape, whose edge can be anywhere) is one rect.
pub fn rim_regions(width: u32, height: u32, reach: f32, slid: bool) -> Vec<[u32; 4]> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let b = (reach.max(0.0).ceil() as u32).saturating_add(2);
    if slid || 2 * b >= width || 2 * b >= height {
        return vec![[0, 0, width, height]];
    }
    vec![
        [0, 0, width, b],
        [0, height - b, width, b],
        [0, b, b, height - 2 * b],
        [width - b, b, b, height - 2 * b],
    ]
}

pub(crate) const RIM_SHADER: &str = r#"
struct R {
    size: vec2<f32>,
    r_top: f32,
    r_bot: f32,
    color: vec4<f32>,
    band: f32,
    offset_y: f32,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> u: R;

struct VsOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VsOut;
    o.pos = vec4(verts[vi], 0.0, 1.0);
    return o;
}

fn s2l(c: f32) -> f32 { if (c <= 0.04045) { return c / 12.92; } return pow((c + 0.055) / 1.055, 2.4); }

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Signed distance to the (slid) rounded window rect; negative inside.
    let half = u.size * 0.5;
    let p = in.pos.xy - vec2(0.0, u.offset_y) - half;
    let max_r = min(u.size.x, u.size.y) * 0.5;
    let r = clamp(select(u.r_top, u.r_bot, p.y > 0.0), 0.0, max_r);
    let q = abs(p) - half + vec2(r, r);
    let d = min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
    // Solid for the outer third of the band, then a smooth fade inward —
    // blending happens in LINEAR light, where a fast falloff washes a
    // mid-tone rim out on a light page; nothing outside (the corner mask
    // clips the feather anyway).
    let k = u.color.a * (1.0 - smoothstep(u.band * 0.35, u.band, -d)) * step(d, 0.5);
    return vec4(s2l(u.color.r) * k, s2l(u.color.g) * k, s2l(u.color.b) * k, k);
}
"#;

/// The rim pass (bell / command pulse).
pub struct RimLayer {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl RimLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rim-shader"),
            source: wgpu::ShaderSource::Wgsl(RIM_SHADER.into()),
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rim-uniform"),
            size: std::mem::size_of::<RimUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rim-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rim-bg"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rim-layout"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rim-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
        RimLayer { pipeline, uniform_buf, bind_group }
    }

    /// Draw the rim described by `u` over `dst` (LoadOp::Load), scissored to
    /// the edge strips it can reach.
    pub fn apply(&self, device: &wgpu::Device, queue: &wgpu::Queue, dst: &wgpu::TextureView, u: &RimUniform) {
        if u.color[3] <= 0.0 {
            return;
        }
        let (w, h) = (u.size[0] as u32, u.size[1] as u32);
        let reach = u.band + u.r_top.max(u.r_bot);
        let regions = rim_regions(w, h, reach, u.offset_y != 0.0);
        if regions.is_empty() {
            return;
        }
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(u));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rim-encoder") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rim-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            for [x, y, rw, rh] in regions {
                pass.set_scissor_rect(x, y, rw, rh);
                pass.draw(0..3, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_validates() {
        let module = naga::front::wgsl::parse_str(RIM_SHADER).expect("parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("validates");
    }

    #[test]
    fn rim_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<RimUniform>(), 48);
        assert_eq!(align_of::<RimUniform>(), 4);
        assert_eq!(offset_of!(RimUniform, size), 0);
        assert_eq!(offset_of!(RimUniform, r_top), 8);
        assert_eq!(offset_of!(RimUniform, r_bot), 12);
        assert_eq!(offset_of!(RimUniform, color), 16);
        assert_eq!(offset_of!(RimUniform, band), 32);
        assert_eq!(offset_of!(RimUniform, offset_y), 36);
        assert_eq!(size_of::<RimUniform>() % 16, 0);
    }

    #[test]
    fn regions_are_disjoint_edge_strips() {
        let (w, h) = (300u32, 200u32);
        let regions = rim_regions(w, h, 20.0, false);
        assert_eq!(regions.len(), 4);
        let mut hits = vec![0u8; (w * h) as usize];
        for [x, y, rw, rh] in regions {
            assert!(x + rw <= w && y + rh <= h);
            for py in y..y + rh {
                for px in x..x + rw {
                    hits[(py * w + px) as usize] += 1;
                }
            }
        }
        assert!(hits.iter().all(|&n| n <= 1), "a pixel would glow twice");
        // Everything within the reach of an edge is covered; the centre is not.
        assert_eq!(hits[(5 * w + 150) as usize], 1);
        assert_eq!(hits[(100 * w + 3) as usize], 1);
        assert_eq!(hits[(100 * w + 150) as usize], 0, "~80% of the frame is skipped");
    }

    #[test]
    fn small_or_slid_targets_draw_once_over_everything() {
        assert_eq!(rim_regions(30, 30, 20.0, false), vec![[0, 0, 30, 30]]);
        assert_eq!(rim_regions(300, 200, 10.0, true), vec![[0, 0, 300, 200]]);
        assert!(rim_regions(0, 10, 5.0, false).is_empty());
    }

    #[test]
    fn uniform_clamps() {
        let u = RimUniform::new(100, 50, -3.0, 12.0, [255, 0, 0], 1.7, 0.0, -10.0);
        assert_eq!((u.r_top, u.r_bot, u.band), (0.0, 12.0, 1.0));
        assert_eq!(u.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(u.offset_y, -10.0);
    }
}
