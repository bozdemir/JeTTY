//! Phosphor Ignition summon reveal — a CRT-style "power-on": a descending
//! scanline reveals the interior while a neon accent rim hugs the window's
//! rounded-rect border (corners light first) and a bright scan line sweeps down.
//!
//! Two fullscreen-triangle passes share one 32-byte uniform
//! `{ w, h, radius, t, ar, ag, ab, _pad }` (8 scalars = 32 bytes; no vec3 so the
//! Rust buffer layout matches exactly):
//!   Pass A (multiply-dst, src=Zero/dst=Src): the unlit area is darkened to ~10%
//!     and brightens to full behind a descending reveal line.
//!   Pass B (additive, src=One/dst=One): a neon accent rim just inside the
//!     rounded-rect edge (corner-staggered) plus a bright moving scan line. Gated
//!     by a sin envelope so it is 0 at t=0 and t=1 (no residue).
//!
//! Self-contained: our own wgpu/WGSL, reusing the rounded-rect SDF from mask.rs.
//! No offscreen texture, no desktop-environment / compositor / OS-specific code.

const PHOSPHOR_SHADER: &str = r#"
// 32-byte uniform (8 scalars). Avoid vec3<f32> so the host buffer layout is exact.
struct P {
    w: f32, h: f32, radius: f32, t: f32,
    ar: f32, ag: f32, ab: f32, _pad: f32,
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

// Pass A: descending scanline reveal (multiply the destination by brightness).
@fragment
fn fs_reveal(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(p.t, 0.0, 1.0);
    let scan_y = smoothstep(0.15, 1.0, t);
    // 1 below the descending line (revealed), 0 above (still dark).
    let wipe = smoothstep(scan_y - 0.02, scan_y + 0.05, in.uv.y);
    let b = mix(0.10, 1.0, wipe);
    return vec4<f32>(b, b, b, b);
}

// Pass B: neon accent rim + bright scan line (additive).
@fragment
fn fs_glow(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(p.t, 0.0, 1.0);
    let hsize = vec2<f32>(p.w, p.h) * 0.5;
    let pos = vec2<f32>(in.uv.x * p.w, in.uv.y * p.h) - hsize;
    let d = sd_round_rect(pos, hsize, p.radius);
    // Thin band just inside the edge.
    let rim = smoothstep(-5.0, -2.0, d) * (1.0 - smoothstep(-2.0, 0.5, d));
    // Corner-stagger: corners light first as t rises.
    let ct = smoothstep(0.0, 0.45, t);
    // Descending bright scan line (matches the reveal front).
    let scan_y = smoothstep(0.15, 1.0, t);
    // Fix: smoothstep(0.05, 0.0, …) had edge0 > edge1 (spec-undefined).
    // Rewrite as 1.0 - smoothstep(0.0, 0.05, …) which is equivalent and well-defined.
    let scan = 1.0 - smoothstep(0.0, 0.05, abs(in.uv.y - scan_y));
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
            size: 32,
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
        let params: [f32; 8] = [
            width as f32,
            height as f32,
            radius.max(0.0),
            t,
            accent[0],
            accent[1],
            accent[2],
            0.0,
        ];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&params));

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
