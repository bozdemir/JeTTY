//! Window border / focus ring (config `window_border`): a thin ring hugging the
//! borderless window's own shape — the corner mask's per-corner rounded rect
//! (mask.rs), so it follows Dropdown mode's square top corners too. Drawn into
//! the scene BEFORE the corner mask: the mask then feathers the ring's outer
//! edge exactly like the content's, and the CRT pass (which owns the corners
//! while active) bends it with everything else.
//!
//! Zero cost while off: the app builds a [`FocusRing`] lazily, on the first
//! frame that needs one, and never in `resumed`. On, it is one small uniform
//! write and a few scissored draws covering only the edge bands (a ring 2 px
//! wide touches ~0.5% of a 1000×640 window), not a full-screen pass.
//!
//! Slice F's visual bell / command pulse are also rim effects; they could share
//! this pass (same SDF, a time-varying color/width) — see the integration notes.

const RING_SHADER: &str = r#"
// Two 16-byte rows + the color row (48 bytes, std140-aligned):
// {size.xy, width, _pad} {r_tl, r_tr, r_bl, r_br} {color rgba (sRGB, straight)}.
struct Params { size: vec2<f32>, width: f32, _pad: f32, radii: vec4<f32>, color: vec4<f32> };
@group(0) @binding(0) var<uniform> params: Params;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle; the pass scissors it to the edge bands.
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let p = verts[vi];
    var o: VsOut;
    o.pos = vec4(p, 0.0, 1.0);
    o.uv = vec2((p.x * 0.5 + 0.5) * params.size.x, (1.0 - (p.y * 0.5 + 0.5)) * params.size.y);
    return o;
}

// The corner mask's per-corner rounded-rect SDF (mask.rs), verbatim.
fn sd_round_rect_per(p: vec2<f32>, b: vec2<f32>, r_tl: f32, r_tr: f32, r_bl: f32, r_br: f32) -> f32 {
    let r_top = select(r_tl, r_tr, p.x > 0.0);
    let r_bot = select(r_bl, r_br, p.x > 0.0);
    let r = select(r_top, r_bot, p.y > 0.0);
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

fn s2l(c: f32) -> f32 { if (c <= 0.04045) { return c / 12.92; } return pow((c + 0.055) / 1.055, 2.4); }

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let hsize = params.size * 0.5;
    let d = sd_round_rect_per(in.uv - hsize, hsize, params.radii.x, params.radii.y, params.radii.z, params.radii.w);
    // Inside the window shape (the mask's own edge feather) …
    let shape = 1.0 - smoothstep(-0.75, 0.75, d);
    // … and within `width` px of its edge (a 1 px feather on the inner side).
    let band = smoothstep(-params.width - 0.5, -params.width + 0.5, d);
    let cov = shape * band;
    let c = params.color;
    return vec4(s2l(c.r), s2l(c.g), s2l(c.b), c.a * cov);
}
"#;

/// The ring's uniform, laid out exactly like the WGSL `Params` (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RingUniform {
    /// Target size in physical px. (offset 0)
    pub size: [f32; 2],
    /// Ring width in physical px. (offset 8)
    pub width: f32,
    pub _pad: f32,
    /// Corner radii in physical px: top-left, top-right, bottom-left,
    /// bottom-right (the corner mask's). (offset 16)
    pub radii: [f32; 4],
    /// Ring color, sRGB 0..1 with straight alpha. (offset 32)
    pub color: [f32; 4],
}

/// Ring width in physical px for a window at `dpi`: 1.5 logical px — crisp at
/// 2× (3 px), a soft 1.5 px line at 1×.
pub fn ring_width_px(dpi: f32) -> f32 {
    let dpi = if dpi.is_finite() && dpi > 0.0 { dpi } else { 1.0 };
    (1.5 * dpi).max(1.0)
}

/// The GPU focus-ring pass. Build it only when a ring is wanted.
pub struct FocusRing {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl FocusRing {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("focus-ring-shader"),
            source: wgpu::ShaderSource::Wgsl(RING_SHADER.into()),
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("focus-ring-uniform"),
            size: std::mem::size_of::<RingUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("focus-ring-bgl"),
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
            label: Some("focus-ring-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("focus-ring-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("focus-ring-pipeline"),
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
                // Straight-alpha "over", like every quad (the scene is
                // premultiplied; this keeps it so).
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
        Self { pipeline, uniform_buf, bind_group }
    }

    /// Draw the ring over `view` (`LoadOp::Load`): `radii` = the corner mask's
    /// (tl, tr, bl, br) in physical px, `ring_w` its width in physical px,
    /// `color` sRGB RGBA. Draws nothing for an empty target or a zero width.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        radii: [f32; 4],
        ring_w: f32,
        color: [u8; 4],
    ) {
        if width == 0 || height == 0 || ring_w <= 0.0 || color[3] == 0 {
            return;
        }
        let radii = clamp_radii(width, height, radii);
        let u = RingUniform {
            size: [width as f32, height as f32],
            width: ring_w,
            _pad: 0.0,
            radii,
            color: color.map(|c| c as f32 / 255.0),
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("focus-ring-encoder") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("focus-ring-pass"),
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
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            for [x, y, w, h] in ring_regions(width, height, radii, ring_w) {
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..3, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// Each radius clamped to half the smaller dimension (as the corner mask does).
fn clamp_radii(width: u32, height: u32, radii: [f32; 4]) -> [f32; 4] {
    let max_r = (width.min(height) as f32) / 2.0;
    radii.map(|r| if r.is_finite() { r.clamp(0.0, max_r) } else { 0.0 })
}

/// The DISJOINT scissor rects `[x, y, w, h]` outside of which the ring's
/// coverage is exactly 0: one square per corner (its radius plus the ring and
/// its feathers) and the four edge strips between them, each as deep as the
/// ring plus its feathers. Disjoint because the pass blends "over": a pixel
/// drawn twice would double its antialiased edge. A target too small for the
/// squares to stay apart is drawn as one full-target rect instead.
pub(crate) fn ring_regions(width: u32, height: u32, radii: [f32; 4], ring_w: f32) -> Vec<[u32; 4]> {
    let mut out = Vec::with_capacity(8);
    if width == 0 || height == 0 {
        return out;
    }
    let band = (ring_w.max(0.0).ceil() as u32).saturating_add(2);
    let side = |r: f32| ((r.max(0.0) + ring_w.max(0.0)).ceil() as u32).saturating_add(2).max(band);
    let [tl, tr, bl, br] = radii.map(side);
    if tl + tr > width || bl + br > width || tl + bl > height || tr + br > height || band * 2 > height || band * 2 > width {
        out.push([0, 0, width, height]);
        return out;
    }
    for (s, right, bottom) in [(tl, false, false), (tr, true, false), (bl, false, true), (br, true, true)] {
        out.push([if right { width - s } else { 0 }, if bottom { height - s } else { 0 }, s, s]);
    }
    // Top / bottom strips between the corner squares.
    out.push([tl, 0, width - tl - tr, band]);
    out.push([bl, height - band, width - bl - br, band]);
    // Left / right strips between the squares (below / above the corner squares).
    out.push([0, tl, band, height - tl - bl]);
    out.push([width - band, tr, band, height - tr - br]);
    out.retain(|r| r[2] > 0 && r[3] > 0);
    out
}

/// CPU mirror of the ring shader's coverage at pixel `(x, y)` of a `w`×`h`
/// target (pixel centres, like the GPU): 1.0 on the ring, 0.0 away from the
/// edges, antialiased at both rims. Used by the unit tests to pin the geometry
/// and the scissor regions.
pub fn ring_coverage(x: f32, y: f32, w: f32, h: f32, radii: [f32; 4], ring_w: f32) -> f32 {
    let [r_tl, r_tr, r_bl, r_br] = radii;
    let (hw, hh) = (w / 2.0, h / 2.0);
    let (px, py) = ((x + 0.5) - hw, (y + 0.5) - hh);
    let r_top = if px > 0.0 { r_tr } else { r_tl };
    let r_bot = if px > 0.0 { r_br } else { r_bl };
    let r = if py > 0.0 { r_bot } else { r_top };
    let (qx, qy) = (px.abs() - hw + r, py.abs() - hh + r);
    let d = qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r;
    let smooth = |e0: f32, e1: f32, v: f32| {
        let t = ((v - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let shape = 1.0 - smooth(-0.75, 0.75, d);
    let band = smooth(-ring_w - 0.5, -ring_w + 0.5, d);
    shape * band
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_shader_passes_naga_validation() {
        let module = naga::front::wgsl::parse_str(RING_SHADER).expect("RING_SHADER must parse");
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("RING_SHADER must pass naga validation");
    }

    #[test]
    fn ring_uniform_layout_matches_the_wgsl_params() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<RingUniform>(), 48);
        assert_eq!(offset_of!(RingUniform, size), 0);
        assert_eq!(offset_of!(RingUniform, width), 8);
        assert_eq!(offset_of!(RingUniform, radii), 16);
        assert_eq!(offset_of!(RingUniform, color), 32);
        assert_eq!(size_of::<RingUniform>() % align_of::<RingUniform>(), 0);
        assert_eq!(size_of::<RingUniform>() % 16, 0, "std140 rows");
    }

    #[test]
    fn ring_sits_on_the_edge_and_follows_rounded_corners() {
        let (w, h, rw) = (200.0, 120.0, 2.0);
        let radii = [16.0; 4];
        // On the straight edges, inside the ring: fully covered.
        assert!(ring_coverage(100.0, 0.0, w, h, radii, rw) > 0.5);
        assert!(ring_coverage(100.0, 119.0, w, h, radii, rw) > 0.5);
        assert!(ring_coverage(0.0, 60.0, w, h, radii, rw) > 0.5);
        assert!(ring_coverage(199.0, 60.0, w, h, radii, rw) > 0.5);
        // Inward of the ring: nothing (the content is untouched).
        for (x, y) in [(100.0, 6.0), (100.0, 60.0), (8.0, 60.0), (40.0, 40.0)] {
            assert_eq!(ring_coverage(x, y, w, h, radii, rw), 0.0, "({x},{y})");
        }
        // Outside a rounded corner (where the mask is transparent): nothing.
        assert!(ring_coverage(0.0, 0.0, w, h, radii, rw) < 0.01);
        // ON the corner arc (45° point of a 16 px radius): covered.
        let a = 16.0 - 16.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert!(ring_coverage(a, a, w, h, radii, rw) > 0.3, "arc pixel");
        // A square (Dropdown top-flush) corner: the ring runs into the corner.
        let flush = [0.0, 0.0, 16.0, 16.0];
        assert!(ring_coverage(0.0, 0.0, w, h, flush, rw) > 0.4);
    }

    #[test]
    fn scissor_regions_cover_every_ring_pixel_exactly_once() {
        for (w, h, radii, rw) in [
            (160u32, 90u32, [10.0, 10.0, 10.0, 10.0], 1.5f32),
            (160, 90, [0.0, 0.0, 16.0, 16.0], 2.0),
            (97, 61, [7.5, 3.2, 12.9, 0.4], 3.0),
            (64, 40, [0.0; 4], 1.0),
            (33, 20, [16.0; 4], 2.0),
            (12, 9, [4.0; 4], 2.0),
        ] {
            let regions = ring_regions(w, h, radii, rw);
            let mut hits = vec![0u8; (w * h) as usize];
            for [x, y, rw_, rh] in &regions {
                assert!(x + rw_ <= w && y + rh <= h, "region outside {w}x{h}");
                for py in *y..y + rh {
                    for px in *x..x + rw_ {
                        hits[(py * w + px) as usize] += 1;
                    }
                }
            }
            assert!(hits.iter().all(|&n| n <= 1), "overlapping regions at {w}x{h} {radii:?}");
            let radii = clamp_radii(w, h, radii);
            for py in 0..h {
                for px in 0..w {
                    let c = ring_coverage(px as f32, py as f32, w as f32, h as f32, radii, rw);
                    if c > 0.0 {
                        assert_eq!(hits[(py * w + px) as usize], 1, "{w}x{h} {radii:?}: ({px},{py}) cov {c} not drawn");
                    }
                }
            }
        }
    }

    #[test]
    fn ring_width_scales_with_dpi() {
        assert_eq!(ring_width_px(1.0), 1.5);
        assert_eq!(ring_width_px(2.0), 3.0);
        assert_eq!(ring_width_px(0.5), 1.0, "never thinner than one physical px");
        assert_eq!(ring_width_px(f32::NAN), 1.5);
    }

    /// Smoke-test the pipeline on a real device (needs a GPU adapter).
    #[test]
    #[ignore]
    fn focus_ring_new_with_device() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("adapter");
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let _ring = FocusRing::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    }
}
