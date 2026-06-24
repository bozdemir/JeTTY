//! Bayer Crystallize summon reveal — the whole frame materializes out of an
//! ordered-dither (Bayer 4×4) lattice, with a theme-accent "crystallizing front"
//! that glows where pixels are freshly switching on (peaks mid-animation, fades
//! to ZERO at the end → no residue).
//!
//! Two fullscreen-triangle passes share one uniform:
//!   1. REVEAL (multiply-dst blend, src=Zero/dst=Src): outputs `vec4(coverage)`
//!      so the destination RGBA is multiplied by the dither coverage — pixels go
//!      from transparent (hidden) to unchanged (revealed) in ordered-dither order.
//!   2. SEAM   (additive blend, src=One/dst=One): adds the theme accent color on
//!      the thin band of pixels that just crystallized, brightest at the front,
//!      gated by a sin envelope so it is 0 at t=0 and t=1.
//!
//! Self-contained: our own wgpu/WGSL, no offscreen texture, no desktop-environment
//! / compositor / OS-specific code.

const REVEAL_SHADER: &str = r#"
// 16-byte uniform (4 scalars). NEVER use vec3<f32> in a uniform here — its 16-byte
// alignment would pad the struct to 32 bytes and mismatch the Rust buffer.
struct P { t: f32, ar: f32, ag: f32, ab: f32 };
@group(0) @binding(0) var<uniform> p: P;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(verts[vi], 0.0, 1.0);
}

fn bayer4(pix: vec2<f32>) -> f32 {
    // 2px dither cells (floor(pix/2)) make the lattice chunkier / more visible.
    let c = floor(pix / 2.0);
    let x = u32(c.x) & 3u;
    let y = u32(c.y) & 3u;
    var m = array<f32,16>(
         0.0, 8.0, 2.0,10.0,
        12.0, 4.0,14.0, 6.0,
         3.0,11.0, 1.0, 9.0,
        15.0, 7.0,13.0, 5.0);
    return (m[y*4u + x] + 0.5) / 16.0;
}

// Pass 1: dither reveal (multiply the destination by coverage).
@fragment
fn fs_reveal(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let te  = pow(clamp(p.t, 0.0, 1.0), 0.45);   // front-loaded ease
    let cov = step(bayer4(frag.xy), te);          // 1 = revealed, 0 = hidden
    return vec4<f32>(cov, cov, cov, cov);
}

// Pass 2: accent glow on the freshly-crystallizing front (additive).
@fragment
fn fs_seam(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let tt  = clamp(p.t, 0.0, 1.0);
    let te  = pow(tt, 0.45);
    let thr = bayer4(frag.xy);
    let d   = te - thr;                 // >=0 revealed; small positive = just now
    let bandw = 0.20;
    let fresh = clamp(d / bandw, 0.0, 1.0);   // 0 at the front .. 1 well behind it
    let front = (1.0 - fresh) * step(0.0, d); // 1 at the front, 0 behind / unrevealed
    let envelope = sin(tt * 3.14159265);      // 0 at t=0 and t=1, peak at t=0.5
    let g = front * envelope * 0.85;
    let accent = vec3<f32>(p.ar, p.ag, p.ab);
    return vec4<f32>(accent * g, g * 0.55);
}
"#;

pub struct BayerReveal {
    reveal_pipeline: wgpu::RenderPipeline,
    seam_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl BayerReveal {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bayer-reveal-shader"),
            source: wgpu::ShaderSource::Wgsl(REVEAL_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bayer-reveal-uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bayer-reveal-bgl"),
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
            label: Some("bayer-reveal-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bayer-reveal-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        // Reveal: multiply the destination color AND alpha by coverage.
        let mul_dst = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };
        // Seam: add the fragment on top of the destination.
        let additive = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };

        let make = |entry: &str, blend: wgpu::BlendComponent| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("bayer-reveal-pipeline"),
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
        let seam_pipeline = make("fs_seam", additive);

        Self { reveal_pipeline, seam_pipeline, uniform_buf, bind_group }
    }

    /// Run the Bayer crystallize reveal over `view` at progress `t` (0..1) with a
    /// theme `accent` color (0..1 RGB) for the crystallizing-front glow. At
    /// `t >= 1.0` coverage is full and the seam envelope is 0 — caller should stop
    /// driving the animation there so idle CPU returns to zero.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        _width: u32,
        _height: u32,
        t: f32,
        accent: [f32; 3],
    ) {
        let params: [f32; 4] = [t, accent[0], accent[1], accent[2]];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&params));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bayer-reveal-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bayer-reveal-pass"),
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
            // 1) dither reveal (multiply), 2) accent seam (additive) on top.
            pass.set_pipeline(&self.reveal_pipeline);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.seam_pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// CPU mirror of the shader's `bayer4` 4×4 ordered-dither threshold (with the same
/// 2px cell size), normalized to (0,1). Kept for tests.
pub fn bayer4(x: u32, y: u32) -> f32 {
    const M: [f32; 16] = [
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    ];
    let xi = ((x / 2) & 3) as usize;
    let yi = ((y / 2) & 3) as usize;
    (M[yi * 4 + xi] + 0.5) / 16.0
}

/// CPU mirror of the shader's reveal coverage at pixel `(x, y)` for progress `t`.
pub fn reveal_coverage(x: u32, y: u32, t: f32) -> f32 {
    let te = t.clamp(0.0, 1.0).powf(0.45);
    if bayer4(x, y) <= te { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::{bayer4, reveal_coverage};

    #[test]
    fn bayer4_thresholds_are_distinct() {
        // The 16 distinct thresholds (sampled at the 2px cell centers) are unique.
        let mut seen = Vec::new();
        for cy in 0..4u32 {
            for cx in 0..4u32 {
                let v = bayer4(cx * 2, cy * 2);
                assert!(v > 0.0 && v < 1.0, "threshold {v} out of (0,1)");
                assert!(!seen.contains(&v.to_bits()), "duplicate threshold {v}");
                seen.push(v.to_bits());
            }
        }
        assert_eq!(seen.len(), 16);
    }

    #[test]
    fn dither_cells_are_2px() {
        // Adjacent pixels in the same 2px cell share a threshold; the period is 8px.
        assert_eq!(bayer4(0, 0), bayer4(1, 1));
        assert_eq!(bayer4(0, 0), bayer4(8, 8));
        assert_ne!(bayer4(0, 0), bayer4(2, 0));
    }

    #[test]
    fn t_zero_hides_everything() {
        for y in 0..8u32 {
            for x in 0..8u32 {
                assert_eq!(reveal_coverage(x, y, 0.0), 0.0);
            }
        }
    }

    #[test]
    fn t_one_reveals_everything_zero_residue() {
        for y in 0..8u32 {
            for x in 0..8u32 {
                assert_eq!(reveal_coverage(x, y, 1.0), 1.0);
            }
        }
    }

    #[test]
    fn reveal_is_monotonic_in_t() {
        for y in 0..8u32 {
            for x in 0..8u32 {
                let mut revealed = false;
                let mut t = 0.0f32;
                while t <= 1.0 {
                    let c = reveal_coverage(x, y, t);
                    if revealed {
                        assert_eq!(c, 1.0, "pixel un-revealed at t={t}");
                    }
                    if c == 1.0 {
                        revealed = true;
                    }
                    t += 0.05;
                }
            }
        }
    }
}
