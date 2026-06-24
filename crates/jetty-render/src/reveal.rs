//! Bayer Crystallize summon reveal — the whole frame materializes out of an
//! ordered-dither (Bayer 4×4) lattice. This is the ORIGINAL subtle look:
//! a single 1px-cell dither reveal, no accent seam.
//!
//! One fullscreen-triangle pass:
//!   REVEAL (multiply-dst blend, src=Zero/dst=Src): outputs `vec4(coverage)`
//!   so the destination RGBA is multiplied by the dither coverage — pixels go
//!   from transparent (hidden) to unchanged (revealed) in ordered-dither order.
//!
//! Self-contained: our own wgpu/WGSL, no offscreen texture, no desktop-environment
//! / compositor / OS-specific code.

const REVEAL_SHADER: &str = r#"
// 16-byte uniform (4 scalars). NEVER use vec3<f32> in a uniform here — its 16-byte
// alignment would pad the struct to 32 bytes and mismatch the Rust buffer.
struct P { t: f32, _a: f32, _b: f32, _c: f32 };
@group(0) @binding(0) var<uniform> p: P;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(verts[vi], 0.0, 1.0);
}

fn bayer4(pix: vec2<f32>) -> f32 {
    // 1px dither cells (the original subtle lattice).
    let x = u32(pix.x) & 3u;
    let y = u32(pix.y) & 3u;
    var m = array<f32,16>(
         0.0, 8.0, 2.0,10.0,
        12.0, 4.0,14.0, 6.0,
         3.0,11.0, 1.0, 9.0,
        15.0, 7.0,13.0, 5.0);
    return (m[y*4u + x] + 0.5) / 16.0;
}

// Dither reveal (multiply the destination by coverage).
@fragment
fn fs_reveal(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let te  = pow(clamp(p.t, 0.0, 1.0), 0.45);   // front-loaded ease
    let cov = step(bayer4(frag.xy), te);          // 1 = revealed, 0 = hidden
    return vec4<f32>(cov, cov, cov, cov);
}
"#;

pub struct BayerReveal {
    reveal_pipeline: wgpu::RenderPipeline,
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

        let reveal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                entry_point: Some("fs_reveal"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState { color: mul_dst, alpha: mul_dst }),
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

        Self { reveal_pipeline, uniform_buf, bind_group }
    }

    /// Run the Bayer crystallize reveal over `view` at progress `t` (0..1). At
    /// `t >= 1.0` coverage is full — caller should stop driving the animation
    /// there so idle CPU returns to zero.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        _width: u32,
        _height: u32,
        t: f32,
    ) {
        let params: [f32; 4] = [t, 0.0, 0.0, 0.0];
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
            pass.set_pipeline(&self.reveal_pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// CPU mirror of the shader's `bayer4` 4×4 ordered-dither threshold (1px cells),
/// normalized to (0,1). Kept for tests.
pub fn bayer4(x: u32, y: u32) -> f32 {
    const M: [f32; 16] = [
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    ];
    let xi = (x & 3) as usize;
    let yi = (y & 3) as usize;
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
        // The 16 distinct thresholds (one per 1px cell) are unique.
        let mut seen = Vec::new();
        for cy in 0..4u32 {
            for cx in 0..4u32 {
                let v = bayer4(cx, cy);
                assert!(v > 0.0 && v < 1.0, "threshold {v} out of (0,1)");
                assert!(!seen.contains(&v.to_bits()), "duplicate threshold {v}");
                seen.push(v.to_bits());
            }
        }
        assert_eq!(seen.len(), 16);
    }

    #[test]
    fn dither_cells_are_1px() {
        // The dither period is 4px; adjacent pixels differ; (0,0) repeats at +4.
        assert_eq!(bayer4(0, 0), bayer4(4, 4));
        assert_ne!(bayer4(0, 0), bayer4(1, 0));
        assert_ne!(bayer4(0, 0), bayer4(1, 1));
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
