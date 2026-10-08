//! Transform summon effects — Pop, Glide and Fade: the rendered frame comes in
//! scaled, drifted and/or faded, sampled from the offscreen scene at a
//! transformed position (a Tier-B pass like Liquid/Focus: bindings {uniform,
//! frame texture, sampler}, a `replace` blend writing every pixel).
//!
//! * **Pop** — 96% → 100% with a light spring (≈0.5% overshoot), opacity in
//!   over the first ~70 ms.
//! * **Glide** — drifts up the last 3% of the window height into place while
//!   fading in.
//! * **Fade** — opacity only, 90 ms. Also the reduce-motion summon (shorter).
//!
//! The per-effect curves are plain data ([`transform_params`], unit-tested);
//! one shader serves all three, so a single small pipeline is built — and only
//! when one of them is the selected summon effect. At t ≥ 1 every effect is the
//! identity (scale 1, no offset, opacity 1); the caller stops the animation
//! there, so idle CPU returns to zero.
//!
//! Self-contained: our own wgpu/WGSL, no desktop-environment / compositor /
//! OS-specific code.

/// Which transform effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformKind {
    Pop,
    Glide,
    Fade,
}

/// Seconds each effect lasts.
pub fn transform_secs(kind: TransformKind) -> f32 {
    match kind {
        TransformKind::Pop => 0.22,
        TransformKind::Glide => 0.20,
        TransformKind::Fade => 0.09,
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `[scale, dx, dy, alpha]` for `kind` at progress `t` (0..=1): the frame is
/// drawn scaled by `scale` about the window centre, moved by (`dx`, `dy`) (in
/// fractions of the window, y down) and multiplied by `alpha`.
pub fn transform_params(kind: TransformKind, t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return [1.0, 0.0, 0.0, 1.0];
    }
    match kind {
        TransformKind::Pop => {
            // A lightly damped spring from 0.96: 1 − 0.04·e^(−6t)·cos(9t)
            // overshoots to ~1.005 near t = 0.35 and settles to 1.
            let scale = 1.0 - 0.04 * (-6.0 * t).exp() * (9.0 * t).cos();
            // Opacity in over the first ~70 ms of 220.
            let alpha = smoothstep(0.0, 0.07 / transform_secs(kind), t);
            [scale, 0.0, 0.0, alpha]
        }
        TransformKind::Glide => {
            // Ease-out cubic drift up from 3% below, fading in meanwhile.
            let e = 1.0 - (1.0 - t).powi(3);
            [1.0, 0.0, 0.03 * (1.0 - e), smoothstep(0.0, 0.6, t)]
        }
        TransformKind::Fade => [1.0, 0.0, 0.0, smoothstep(0.0, 1.0, t)],
    }
}

const TRANSFORM_SHADER: &str = r#"
// 32-byte uniform: [scale, dx, dy, alpha] + [w, h, _, _]. No vec3.
struct P { scale: f32, dx: f32, dy: f32, alpha: f32, w: f32, h: f32, _a: f32, _b: f32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var frame: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let v = verts[vi];
    var o: VsOut;
    o.pos = vec4(v, 0.0, 1.0);
    o.uv = vec2(v.x * 0.5 + 0.5, 1.0 - (v.y * 0.5 + 0.5));
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Where this output pixel comes from in the rendered frame.
    let src = (in.uv - vec2(0.5, 0.5) - vec2(p.dx, p.dy)) / p.scale + vec2(0.5, 0.5);
    // Antialiased frame edge: coverage from the distance (output px) to the
    // nearest source edge, so a scaled/drifted frame never shows a stair-step.
    let inside_px = min(min(src.x, 1.0 - src.x) * p.w, min(src.y, 1.0 - src.y) * p.h) * p.scale;
    let cov = clamp(inside_px + 0.5, 0.0, 1.0);
    let c = textureSampleLevel(frame, samp, clamp(src, vec2(0.0), vec2(1.0)), 0.0);
    // Premultiplied frame: scale every channel (the window fades in as a whole).
    return c * (p.alpha * cov);
}
"#;

/// The transform summon pass (Pop / Glide / Fade).
pub struct SummonTransform {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Cached (offscreen view, bind group); rebuilt only when the view changes.
    cached_bind: std::cell::RefCell<Option<(wgpu::TextureView, wgpu::BindGroup)>>,
}

impl SummonTransform {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transform-shader"),
            source: wgpu::ShaderSource::Wgsl(TRANSFORM_SHADER.into()),
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("transform-uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transform-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("transform-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transform-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transform-pipeline"),
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
                    blend: Some(wgpu::BlendState::REPLACE),
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
        Self { pipeline, uniform_buf, bind_group_layout, sampler, cached_bind: std::cell::RefCell::new(None) }
    }

    /// Sample `frame_tex_view` (the offscreen scene) through `kind`'s transform
    /// at progress `t` and write the result into `dst_view` (the surface).
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst_view: &wgpu::TextureView,
        frame_tex_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        kind: TransformKind,
        t: f32,
    ) {
        let [scale, dx, dy, alpha] = transform_params(kind, t);
        let params: [f32; 8] = [scale.max(1e-3), dx, dy, alpha, width as f32, height as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&params));

        let mut cache = self.cached_bind.borrow_mut();
        let stale = cache.as_ref().map(|(v, _)| v != frame_tex_view).unwrap_or(true);
        if stale {
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("transform-bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(frame_tex_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            *cache = Some((frame_tex_view.clone(), bg));
        }
        let bind_group = &cache.as_ref().expect("bind group cached above").1;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("transform-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transform-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    resolve_target: None,
                    // REPLACE over a fullscreen triangle writes every pixel.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TransformKind; 3] = [TransformKind::Pop, TransformKind::Glide, TransformKind::Fade];

    #[test]
    fn shader_validates() {
        let module = naga::front::wgsl::parse_str(TRANSFORM_SHADER).expect("parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("validates");
    }

    #[test]
    fn every_effect_ends_at_the_identity() {
        for k in ALL {
            assert_eq!(transform_params(k, 1.0), [1.0, 0.0, 0.0, 1.0], "{k:?}");
            assert_eq!(transform_params(k, 7.0), [1.0, 0.0, 0.0, 1.0], "{k:?} clamps");
            // …and is already within a pixel of it on the last frames.
            let [s, dx, dy, a] = transform_params(k, 0.97);
            assert!((s - 1.0).abs() < 0.002 && dx == 0.0 && dy.abs() < 0.0001 && a > 0.99, "{k:?}");
        }
    }

    #[test]
    fn every_effect_starts_invisible() {
        for k in ALL {
            assert_eq!(transform_params(k, 0.0)[3], 0.0, "{k:?}");
        }
    }

    #[test]
    fn pop_springs_from_96_percent_with_a_light_overshoot() {
        assert!((transform_params(TransformKind::Pop, 0.0)[0] - 0.96).abs() < 1e-6);
        let peak = (1..100).map(|i| transform_params(TransformKind::Pop, i as f32 / 100.0)[0]).fold(0.0, f32::max);
        assert!(peak > 1.0 && peak < 1.01, "overshoot {peak}");
        // Opacity is in by ~70 ms.
        let at = |ms: f32| transform_params(TransformKind::Pop, ms / 1000.0 / transform_secs(TransformKind::Pop))[3];
        assert!(at(35.0) > 0.3 && at(35.0) < 0.7);
        assert!(at(70.0) > 0.999);
    }

    #[test]
    fn glide_drifts_three_percent_and_fades_in() {
        let [s, dx, dy, a] = transform_params(TransformKind::Glide, 0.0);
        assert_eq!((s, dx, a), (1.0, 0.0, 0.0));
        assert!((dy - 0.03).abs() < 1e-6, "starts 3% low");
        let mut prev = dy;
        for i in 1..=10 {
            let d = transform_params(TransformKind::Glide, i as f32 / 10.0)[2];
            assert!(d <= prev, "drift only closes");
            prev = d;
        }
    }

    #[test]
    fn fade_is_opacity_only_and_short() {
        for i in 0..=10 {
            let [s, dx, dy, _] = transform_params(TransformKind::Fade, i as f32 / 10.0);
            assert_eq!((s, dx, dy), (1.0, 0.0, 0.0));
        }
        assert!(transform_secs(TransformKind::Fade) <= 0.09);
        let mut prev = -1.0;
        for i in 0..=10 {
            let a = transform_params(TransformKind::Fade, i as f32 / 10.0)[3];
            assert!(a >= prev);
            prev = a;
        }
    }
}
