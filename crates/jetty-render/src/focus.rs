//! FocusPull summon effect — a rack-focus "pull" where the rendered frame snaps
//! from a soft, chromatically-fringed blur into crisp focus as the window
//! summons. Like LiquidDrop this is a Tier-B effect: it SAMPLES the fully
//! rendered scene from an offscreen texture and writes the blurred/aberrated
//! result to the surface.
//!
//! One fullscreen-triangle pass with bindings {uniform, frame_texture, sampler}
//! and a `replace` blend. The blur radius and radial chromatic aberration both
//! decay to 0 as t → 1, and an alpha fade eases the whole frame in. At
//! `t >= 1.0` the pass is an identity blit (zero residue) — the caller stops
//! driving the animation there so idle CPU is zero.
//!
//! Self-contained: our own wgpu/WGSL, no desktop-environment / compositor /
//! OS-specific code.

const FOCUS_SHADER: &str = r#"
// 16-byte uniform (4 scalars). No vec3<f32> so the host buffer layout is exact.
struct P { t: f32, _a: f32, _b: f32, _c: f32 };
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

// 8-tap (diagonal + cardinal) blur of one channel around `uv`, each tap offset
// by `off` * unit-direction. With off=0 this collapses to a single center tap.
fn blur_chan(uv: vec2<f32>, off: f32, chan: i32) -> f32 {
    var offs = array<vec2<f32>, 8>(
        vec2( 1.0,  0.0), vec2(-1.0,  0.0), vec2( 0.0,  1.0), vec2( 0.0, -1.0),
        vec2( 0.707,  0.707), vec2(-0.707,  0.707),
        vec2( 0.707, -0.707), vec2(-0.707, -0.707));
    var sum = 0.0;
    for (var i = 0; i < 8; i = i + 1) {
        let s = textureSample(frame, samp, clamp(uv + offs[i] * off, vec2(0.0), vec2(1.0)));
        if (chan == 0) { sum = sum + s.r; }
        else if (chan == 1) { sum = sum + s.g; }
        else { sum = sum + s.b; }
    }
    return sum / 8.0;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let t = clamp(p.t, 0.0, 1.0);
    let blur_t = pow(1.0 - t, 2.0);
    let off = blur_t * 0.008;
    // Radial chromatic aberration: split R/B along the radial direction. Kept
    // modest so a low t reads as a soft, coherent defocus rather than a rainbow
    // smear on high-contrast text.
    // Guard the center pixel: normalize(vec2(0,0)) is NaN; NaN * ca (even ca=0)
    // stays NaN and would corrupt the center fragment's R/B samples.
    let dv = uv - vec2(0.5, 0.5);
    let dl = length(dv);
    let dir = select(vec2(0.0, 0.0), dv / dl, dl > 0.0001);
    let ca = (1.0 - t) * 0.012 * dl;
    let r = blur_chan(uv + dir * ca, off, 0);
    let g = blur_chan(uv, off, 1);
    let b = blur_chan(uv - dir * ca, off, 2);
    // Use the center sample's alpha (premultiplied frame); blur it lightly too.
    let a = textureSample(frame, samp, uv).a;
    let alpha_fade = smoothstep(0.0, 0.3, t);
    return vec4<f32>(r, g, b, a) * alpha_fade;
}
"#;

pub struct FocusPull {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Cached (offscreen view, bind group); rebuilt only when the frame view
    /// changes (resize) instead of every animation frame. See LiquidDrop.
    cached_bind: std::cell::RefCell<Option<(wgpu::TextureView, wgpu::BindGroup)>>,
}

impl FocusPull {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("focus-shader"),
            source: wgpu::ShaderSource::Wgsl(FOCUS_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("focus-uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("focus-bgl"),
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
            label: Some("focus-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("focus-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("focus-pipeline"),
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

        Self {
            pipeline,
            uniform_buf,
            bind_group_layout,
            sampler,
            cached_bind: std::cell::RefCell::new(None),
        }
    }

    /// Run the FocusPull pass: sample `frame_tex_view` (the offscreen rendered
    /// scene) and write the blurred/aberrated result into `dst_view` (the
    /// surface) at progress `t` (0..1). At `t >= 1.0` the blur and aberration are
    /// 0, so the output is an identity blit — the caller should stop driving the
    /// animation there so idle CPU returns to zero.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst_view: &wgpu::TextureView,
        frame_tex_view: &wgpu::TextureView,
        _width: u32,
        _height: u32,
        t: f32,
    ) {
        let params: [f32; 4] = [t, 0.0, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&params));

        // Rebuild the bind group only when the offscreen view changes (resize).
        let mut cache = self.cached_bind.borrow_mut();
        let stale = cache.as_ref().map(|(v, _)| v != frame_tex_view).unwrap_or(true);
        if stale {
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("focus-bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(frame_tex_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            *cache = Some((frame_tex_view.clone(), bg));
        }
        let bind_group = &cache.as_ref().unwrap().1;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("focus-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("focus-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    // REPLACE + fullscreen triangle overwrites every pixel, so Clear
                    // (not Load) is identical output while sparing tile-based GPUs a
                    // full-surface load each animation frame.
                    resolve_target: None,
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
