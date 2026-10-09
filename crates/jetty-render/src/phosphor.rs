//! Phosphor Ignition summon reveal — a CRT-style "power-on": a descending
//! scanline reveals the interior while a neon accent rim hugs the window's
//! rounded-rect border (corners light first) and a bright scan line sweeps down.
//!
//! Two fullscreen-triangle passes share one 48-byte uniform ([`PhosphorUniform`],
//! scalars only — no vec3 — so the Rust layout matches the WGSL `P` exactly):
//!   Pass A (multiply-dst, src=Zero/dst=Src): the unlit area is darkened to ~10%
//!     and brightens to full behind a descending reveal line.
//!   Pass B (additive, src=One/dst=One): a neon accent rim just inside the
//!     rounded-rect edge (corner-staggered) plus a bright moving scan line. Gated
//!     by a sin envelope so it is 0 at t=0 and t=1 (no residue).
//!
//! The rounded rect is the window's real shape: separate top / bottom corner
//! radii (a top-flush Dropdown keeps its top corners square) and the shape's
//! vertical offset during the Dropdown slide (the corner mask's `offset_y`), so
//! the rim, the scan line and the wipe ride the sliding strip instead of
//! lighting the full window rect over the desktop below it.
//!
//! Self-contained: our own wgpu/WGSL, reusing the rounded-rect SDF from mask.rs.
//! No offscreen texture, no desktop-environment / compositor / OS-specific code.

const PHOSPHOR_SHADER: &str = r#"
// 48-byte uniform (12 scalars). Avoid vec3<f32> so the host buffer layout is exact.
// radius = the BOTTOM corners, radius_top = the top ones; offset_y moves the
// window shape down (the Dropdown slide, <= 0).
struct P {
    w: f32, h: f32, radius: f32, t: f32,
    ar: f32, ag: f32, ab: f32, offset_y: f32,
    radius_top: f32, _p0: f32, _p1: f32, _p2: f32,
};
@group(0) @binding(0) var<uniform> p: P;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let v = verts[vi];
    var o: VsOut;
    o.pos = vec4(v, 0.0, 1.0);
    // uv in 0..1, y down.
    o.uv = vec2(v.x * 0.5 + 0.5, 1.0 - (v.y * 0.5 + 0.5));
    return o;
}

fn sd_round_rect(pt: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(pt) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

// The fragment's height in the window shape, 0..1 top to bottom (it moves by
// p.offset_y during the Dropdown slide).
fn shape_v(uv_y: f32) -> f32 {
    return (uv_y * p.h - p.offset_y) / p.h;
}

// Pass A: descending scanline reveal (multiply the destination by brightness).
@fragment
fn fs_reveal(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(p.t, 0.0, 1.0);
    let scan_y = smoothstep(0.15, 1.0, t);
    let vy = shape_v(in.uv.y);
    // 1 ABOVE the descending line (already revealed — the beam has passed it),
    // 0 below (still dark). vy grows downward, so "above" is vy < scan_y.
    // The `1.0 -` is load-bearing: bf44be9 swapped the smoothstep edges into
    // ascending order (edge0 > edge1 is undefined in WGSL) but dropped the
    // inversion, which played the reveal upside down (the window ~90% dark,
    // then popping in at t = 1). `phosphor_reveal_wipe` mirrors this line.
    let wipe = 1.0 - smoothstep(scan_y - 0.02, scan_y + 0.05, vy);
    let b = mix(0.10, 1.0, wipe);
    return vec4<f32>(b, b, b, b);
}

// Pass B: neon accent rim + bright scan line (additive).
@fragment
fn fs_glow(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(p.t, 0.0, 1.0);
    let hsize = vec2<f32>(p.w, p.h) * 0.5;
    let pos = vec2<f32>(in.uv.x * p.w, in.uv.y * p.h - p.offset_y) - hsize;
    let d = sd_round_rect(pos, hsize, select(p.radius_top, p.radius, pos.y > 0.0));
    // Thin band just inside the edge.
    let rim = smoothstep(-5.0, -2.0, d) * (1.0 - smoothstep(-2.0, 0.5, d));
    // Corner-stagger: corners light first as t rises.
    let ct = smoothstep(0.0, 0.45, t);
    // Descending bright scan line (matches the reveal front).
    let scan_y = smoothstep(0.15, 1.0, t);
    // Fix: smoothstep(0.05, 0.0, …) had edge0 > edge1 (spec-undefined).
    // Rewrite as 1.0 - smoothstep(0.0, 0.05, …) which is equivalent and well-defined.
    let scan = 1.0 - smoothstep(0.0, 0.05, abs(shape_v(in.uv.y) - scan_y));
    // Ignite envelope: 0 at t=0 and t=1 → no residue.
    let ignite = sin(t * 3.14159265);
    // Gate glow contribution by the rounded-rect SDF coverage so the additive
    // pass doesn't re-opaque masked (rounded) corners that should be transparent.
    let cov = clamp(-d, 0.0, 1.0);
    let g = (rim * ct + scan) * ignite * cov;
    let accent = vec3<f32>(p.ar, p.ag, p.ab);
    return vec4<f32>(accent * g, g * 0.6);
}
"#;

/// The pass's uniform, laid out exactly like the WGSL `P` (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PhosphorUniform {
    /// Target size in physical px. (offset 0)
    pub size: [f32; 2],
    /// Bottom corner radius in physical px. (offset 8)
    pub radius: f32,
    /// Progress 0..1. (offset 12)
    pub t: f32,
    /// Accent RGB 0..1. (offset 16)
    pub accent: [f32; 3],
    /// The window shape's vertical offset (≤ 0 during the Dropdown slide). (offset 28)
    pub offset_y: f32,
    /// Top corner radius in physical px (0 for a top-flush Dropdown). (offset 32)
    pub radius_top: f32,
    pub _pad: [f32; 3],
}

pub struct PhosphorIgnition {
    reveal_pipeline: wgpu::RenderPipeline,
    glow_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl PhosphorIgnition {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("phosphor-shader"),
            source: wgpu::ShaderSource::Wgsl(PHOSPHOR_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phosphor-uniform"),
            size: std::mem::size_of::<PhosphorUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phosphor-bgl"),
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
            label: Some("phosphor-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("phosphor-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        let mul_dst = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };
        let additive = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };

        let make = |entry: &str, blend: wgpu::BlendComponent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("phosphor-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState { color: blend, alpha: blend }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let reveal_pipeline = make("fs_reveal", mul_dst);
        let glow_pipeline = make("fs_glow", additive);

        Self { reveal_pipeline, glow_pipeline, uniform_buf, bind_group }
    }

    /// Run the Phosphor Ignition reveal over `view` at progress `t` (0..1) with a
    /// theme `accent` color (0..1 RGB) and the window's corner `radius` (physical
    /// px). At `t >= 1.0` the interior is fully revealed and the glow envelope is
    /// 0 — caller should stop driving the animation there so idle CPU is zero.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        radius: f32,
        t: f32,
        accent: [f32; 3],
    ) {
        self.apply_slid(device, queue, view, width, height, [radius, radius], 0.0, t, accent);
    }

    /// [`Self::apply`] on the window's real shape: `radii` = (top, bottom)
    /// corner radii in physical px (top 0 while a Dropdown is top-flush) and
    /// the shape moved down by `offset_y` physical px (≤ 0 during the Dropdown
    /// slide — the corner mask's `apply_slid`). The rim, scan line and wipe
    /// ride the sliding strip; nothing lights outside it.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_slid(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        radii: [f32; 2],
        offset_y: f32,
        t: f32,
        accent: [f32; 3],
    ) {
        // Each radius at most half the smaller side (the corner mask's clamp).
        let max_r = width.min(height) as f32 / 2.0;
        let r = |v: f32| if v.is_finite() { v.clamp(0.0, max_r) } else { 0.0 };
        let u = PhosphorUniform {
            size: [width as f32, height as f32],
            radius: r(radii[1]),
            t,
            accent,
            offset_y: if offset_y.is_finite() { offset_y } else { 0.0 },
            radius_top: r(radii[0]),
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("phosphor-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("phosphor-pass"),
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
            pass.set_bind_group(0, &self.bind_group, &[]);
            // 1) descending reveal (multiply), 2) accent rim + scan (additive).
            pass.set_pipeline(&self.reveal_pipeline);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.glow_pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU mirror of `fs_reveal`'s brightness at row `uv_y` (0 = top, 1 = bottom)
    /// and progress `t` — the reveal's direction, pinned by tests (the shader
    /// itself needs a GPU; jetty-shot's `JETTY_SHOT_PHOSPHOR_T` renders it).
    fn phosphor_reveal_brightness(uv_y: f32, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let scan_y = smoothstep(0.15, 1.0, t);
        let wipe = 1.0 - smoothstep(scan_y - 0.02, scan_y + 0.05, uv_y);
        0.10 + (1.0 - 0.10) * wipe
    }

    fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// The wipe at window pixel row `y_px` of an `h`-tall window whose shape
    /// is moved by `offset_y` (`shape_v` in the shader).
    fn phosphor_reveal_brightness_slid(y_px: f32, h: f32, offset_y: f32, t: f32) -> f32 {
        phosphor_reveal_brightness((y_px - offset_y) / h, t)
    }

    /// CPU mirror of `fs_glow`'s rim × shape coverage at pixel `(x, y)` (its
    /// centre, like the GPU) — the time-independent part of the glow.
    fn phosphor_glow_coverage(x: f32, y: f32, w: f32, h: f32, r_top: f32, r_bottom: f32, offset_y: f32) -> f32 {
        let (px, py) = (x + 0.5 - w / 2.0, y + 0.5 - offset_y - h / 2.0);
        let r = if py > 0.0 { r_bottom } else { r_top };
        let (qx, qy) = (px.abs() - w / 2.0 + r, py.abs() - h / 2.0 + r);
        let d = qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r;
        let rim = smoothstep(-5.0, -2.0, d) * (1.0 - smoothstep(-2.0, 0.5, d));
        rim * (-d).clamp(0.0, 1.0)
    }

    /// The uniform: 48 bytes, the WGSL `P` byte for byte.
    #[test]
    fn phosphor_uniform_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<PhosphorUniform>(), 48);
        assert_eq!(offset_of!(PhosphorUniform, radius), 8);
        assert_eq!(offset_of!(PhosphorUniform, t), 12);
        assert_eq!(offset_of!(PhosphorUniform, accent), 16);
        assert_eq!(offset_of!(PhosphorUniform, offset_y), 28);
        assert_eq!(offset_of!(PhosphorUniform, radius_top), 32);
        let module = naga::front::wgsl::parse_str(PHOSPHOR_SHADER).unwrap();
        let (_, ty) = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("P")).expect("P");
        let naga::TypeInner::Struct { members, span } = &ty.inner else { panic!("P is a struct") };
        assert_eq!(*span, 48);
        let off = |n: &str| members.iter().find(|m| m.name.as_deref() == Some(n)).map(|m| m.offset);
        assert_eq!((off("offset_y"), off("radius_top")), (Some(28), Some(32)));
    }

    /// Mid Dropdown slide the window shape is moved up by `offset_y`: the
    /// accent rim and scan line light only that moved shape (they were drawn
    /// around the full window, floating over the desktop below the sliding
    /// strip), and a top-flush window's square top corners get a square rim.
    #[test]
    fn the_glow_rides_the_dropdown_slide() {
        let (w, h) = (400.0, 200.0);
        let cov = |x: f32, y: f32, o: f32| phosphor_glow_coverage(x, y, w, h, 0.0, 16.0, o);
        // Static: the bottom rim sits just inside the window's bottom edge.
        assert!(cov(200.0, 197.0, 0.0) > 0.9);
        // Slid up 120 px: nothing below the moving edge (y = 80)…
        for (x, y) in [(200.0, 197.0), (2.0, 150.0), (397.0, 120.0), (200.0, 81.0)] {
            assert_eq!(cov(x, y, -120.0), 0.0, "({x},{y}) is below the moving edge");
        }
        // …the rim runs along it instead.
        assert!(cov(200.0, 77.0, -120.0) > 0.9);
        // Top-flush (r_top = 0): the rim reaches into the square top corners.
        assert!(phosphor_glow_coverage(2.0, 2.0, w, h, 0.0, 16.0, 0.0) > 0.9);
        assert_eq!(phosphor_glow_coverage(2.0, 2.0, w, h, 16.0, 16.0, 0.0), 0.0, "rounded: outside the arc");
        // The reveal wipe rides the slide too: a content row is lit the same.
        for t in [0.3, 0.6] {
            for row in [10.0, 60.0, 150.0] {
                let still = phosphor_reveal_brightness(row / h, t);
                let slid = phosphor_reveal_brightness_slid(row - 120.0, h, -120.0, t);
                assert!((still - slid).abs() < 1e-5, "t={t} row {row}: {still} vs {slid}");
            }
        }
        assert!(PHOSPHOR_SHADER.contains("p.offset_y"));
    }

    #[test]
    fn phosphor_shader_validates() {
        let module = naga::front::wgsl::parse_str(PHOSPHOR_SHADER).expect("PHOSPHOR_SHADER parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("PHOSPHOR_SHADER validates");
    }

    #[test]
    fn the_shader_keeps_the_inverted_wipe() {
        // The regression line itself: without the `1.0 -` the reveal plays upside
        // down (bf44be9).
        assert!(PHOSPHOR_SHADER.contains("let wipe = 1.0 - smoothstep(scan_y - 0.02, scan_y + 0.05, vy);"));
    }

    #[test]
    fn the_reveal_runs_top_down() {
        // Mid-reveal: the top (already swept) is lit, the bottom still dark.
        let top = phosphor_reveal_brightness(0.05, 0.5);
        let bottom = phosphor_reveal_brightness(0.95, 0.5);
        assert!(top > 0.99, "top must be revealed mid-sweep, got {top}");
        assert!(bottom < 0.11, "bottom must still be dark mid-sweep, got {bottom}");
        // Monotonic down the frame at any t: never brighter below than above.
        for t in [0.2, 0.4, 0.6, 0.8] {
            let mut prev = f32::INFINITY;
            for i in 0..=20 {
                let b = phosphor_reveal_brightness(i as f32 / 20.0, t);
                assert!(b <= prev + 1e-6, "t={t}: brighter below row {i}");
                prev = b;
            }
        }
    }

    #[test]
    fn the_reveal_starts_dark_and_ends_fully_lit() {
        // t = 0: the beam sits at the top edge — everything below it is dark.
        assert!(phosphor_reveal_brightness(0.5, 0.0) < 0.11);
        assert!(phosphor_reveal_brightness(0.99, 0.0) < 0.11);
        // t = 1: the whole frame is revealed except the last ~2% band the beam
        // is still leaving (the animation ends there and the next frame has no
        // pass at all).
        for i in 0..=48 {
            let y = i as f32 / 50.0;
            assert!(phosphor_reveal_brightness(y, 1.0) > 0.99, "row {y} dark at t=1");
        }
    }
}
