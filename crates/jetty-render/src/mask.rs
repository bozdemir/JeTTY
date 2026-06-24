//! Rounded-corner alpha mask for the borderless window.
//!
//! The window surface is transparent, so to "round" the corners we make the
//! pixels OUTSIDE a rounded rectangle fully transparent — the compositor then
//! shows the rounding. This is a final fullscreen pass that runs AFTER all the
//! scene layers (text / quad / tabbar / menu / panel) have drawn to the surface.
//!
//! The pass multiplies BOTH the destination color and alpha by an antialiased
//! rounded-rect coverage value (an SDF with ~1px feather). Because the scene is
//! drawn with premultiplied alpha, multiplying color and alpha by the same
//! coverage keeps premultiplication consistent, so corners fade out cleanly.
//!
//! With `radius == 0` coverage is 1.0 everywhere → the frame is unchanged, so a
//! square window renders byte-identical to before.

const MASK_SHADER: &str = r#"
struct Params { size: vec2<f32>, radius: f32, _pad: f32 };
@group(0) @binding(0) var<uniform> params: Params;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle.
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let p = verts[vi];
    var o: VsOut;
    o.pos = vec4(p, 0.0, 1.0);
    // Map clip space to pixel space (y down).
    o.uv = vec2((p.x * 0.5 + 0.5) * params.size.x, (1.0 - (p.y * 0.5 + 0.5)) * params.size.y);
    return o;
}

// Signed distance from point p to a rounded rectangle of half-size b and corner
// radius r, centered at the origin. Negative inside, positive outside.
fn sd_round_rect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let r = params.radius;
    // Center-relative pixel coordinate.
    let half = params.size * 0.5;
    let p = in.uv - half;
    let d = sd_round_rect(p, half, r);
    // ~1px antialiased edge: coverage 1 inside, 0 outside, smooth across the seam.
    let cov = 1.0 - smoothstep(-0.75, 0.75, d);
    // Output coverage in all channels; the blend pipeline multiplies the
    // destination (premultiplied) color AND alpha by this value.
    return vec4(cov, cov, cov, cov);
}
"#;

pub struct CornerMask {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl CornerMask {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("corner-mask-shader"),
            source: wgpu::ShaderSource::Wgsl(MASK_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("corner-mask-uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("corner-mask-bgl"),
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
            label: Some("corner-mask-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("corner-mask-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        // Multiply the destination color AND alpha by the fragment's coverage:
        //   new = src_factor*src + dst_factor*dst, with src_factor = Zero and
        //   dst_factor = Src → new = coverage * dst (for both color and alpha).
        let mul_dst = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("corner-mask-pipeline"),
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
                    blend: Some(wgpu::BlendState {
                        color: mul_dst,
                        alpha: mul_dst,
                    }),
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

    /// Run the rounded-corner mask over `view`. A `radius <= 0` is a no-op (the
    /// pass is skipped entirely, so a square window is byte-identical to before).
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        radius: f32,
    ) {
        if radius <= 0.0 {
            return;
        }
        // Clamp the radius so it never exceeds half the smaller dimension.
        let max_r = (width.min(height) as f32) / 2.0;
        let radius = radius.min(max_r).max(0.0);
        let params: [f32; 4] = [width as f32, height as f32, radius, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&params));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("corner-mask-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("corner-mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
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
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// Antialiased rounded-rectangle coverage at pixel `(x, y)` for a `w`×`h` frame
/// and corner `radius` (in pixels). 1.0 fully inside, 0.0 fully outside, with a
/// ~1px feather across the boundary. Mirrors the shader's SDF so the headless
/// `jetty-shot` (CPU compositing) can apply the SAME mask as the live GPU pass.
pub fn rounded_rect_coverage(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 1.0;
    }
    let max_r = w.min(h) / 2.0;
    let r = radius.min(max_r).max(0.0);
    let hw = w / 2.0;
    let hh = h / 2.0;
    // Center-relative pixel center (+0.5 to sample the pixel center).
    let px = (x + 0.5) - hw;
    let py = (y + 0.5) - hh;
    let qx = px.abs() - hw + r;
    let qy = py.abs() - hh + r;
    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);
    let d = qx.max(qy).min(0.0) + (outside_x * outside_x + outside_y * outside_y).sqrt() - r;
    // smoothstep(-0.75, 0.75, d), then invert for coverage.
    let t = ((d + 0.75) / 1.5).clamp(0.0, 1.0);
    let s = t * t * (3.0 - 2.0 * t);
    1.0 - s
}

#[cfg(test)]
mod tests {
    use super::rounded_rect_coverage;

    #[test]
    fn radius_zero_is_fully_opaque_everywhere() {
        // With no radius the coverage is 1.0 at every pixel, including corners.
        assert_eq!(rounded_rect_coverage(0.0, 0.0, 100.0, 100.0, 0.0), 1.0);
        assert_eq!(rounded_rect_coverage(99.0, 99.0, 100.0, 100.0, 0.0), 1.0);
    }

    #[test]
    fn corner_pixel_is_transparent_with_radius() {
        // The very corner of the frame is outside a 16px-radius rounded rect.
        let cov = rounded_rect_coverage(0.0, 0.0, 100.0, 100.0, 16.0);
        assert!(cov < 0.01, "corner coverage should be ~0, got {cov}");
        // The opposite corner too.
        let cov2 = rounded_rect_coverage(99.0, 99.0, 100.0, 100.0, 16.0);
        assert!(cov2 < 0.01, "corner coverage should be ~0, got {cov2}");
    }

    #[test]
    fn center_is_opaque_with_radius() {
        let cov = rounded_rect_coverage(50.0, 50.0, 100.0, 100.0, 16.0);
        assert!((cov - 1.0).abs() < 1e-4, "center should be opaque, got {cov}");
    }

    #[test]
    fn edge_midpoint_is_opaque() {
        // The middle of an edge (far from any corner) stays fully inside.
        let cov = rounded_rect_coverage(50.0, 1.0, 100.0, 100.0, 16.0);
        assert!(cov > 0.99, "edge midpoint should be opaque, got {cov}");
    }
}
