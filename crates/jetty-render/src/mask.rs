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
// Per-corner radii (r_tl/r_tr/r_bl/r_br) so Dropdown mode can round only the
// BOTTOM corners. Layout is two 16-byte rows: {size.xy, r_tl, r_tr} then
// {r_bl, r_br, _pad0, _pad1} — keeps std140 alignment (32 bytes total).
struct Params { size: vec2<f32>, r_tl: f32, r_tr: f32, r_bl: f32, r_br: f32, _pad0: f32, _pad1: f32 };
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

// Signed distance to a rounded rectangle with a DIFFERENT radius per corner.
// p is center-relative (y down), b is the half-size. The radius is selected by
// quadrant: top corners use the top radii, bottom corners the bottom radii.
fn sd_round_rect_per(p: vec2<f32>, b: vec2<f32>, r_tl: f32, r_tr: f32, r_bl: f32, r_br: f32) -> f32 {
    // Pick the radius for the quadrant the point lies in.
    let r_top = select(r_tl, r_tr, p.x > 0.0);
    let r_bot = select(r_bl, r_br, p.x > 0.0);
    let r = select(r_top, r_bot, p.y > 0.0);
    let q = abs(p) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Center-relative pixel coordinate.
    let hsize = params.size * 0.5;
    let p = in.uv - hsize;
    let d = sd_round_rect_per(p, hsize, params.r_tl, params.r_tr, params.r_bl, params.r_br);
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
            size: 32,
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

    /// Run the rounded-corner mask over `view` with a per-corner radius
    /// (top-left, top-right, bottom-left, bottom-right). When all four radii are
    /// `<= 0` the pass is skipped entirely (square window, byte-identical to
    /// before). In Dropdown mode the two top radii are zeroed so only the bottom
    /// corners round.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        r_tl: f32,
        r_tr: f32,
        r_bl: f32,
        r_br: f32,
    ) {
        if r_tl <= 0.0 && r_tr <= 0.0 && r_bl <= 0.0 && r_br <= 0.0 {
            return;
        }
        // Clamp each radius so it never exceeds half the smaller dimension.
        let max_r = (width.min(height) as f32) / 2.0;
        let c = |r: f32| r.min(max_r).max(0.0);
        // Layout: [size.x, size.y, r_tl, r_tr, r_bl, r_br, _pad, _pad] (32 bytes).
        let params: [f32; 8] = [
            width as f32,
            height as f32,
            c(r_tl),
            c(r_tr),
            c(r_bl),
            c(r_br),
            0.0,
            0.0,
        ];
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
/// with a PER-CORNER radius (top-left, top-right, bottom-left, bottom-right) in
/// pixels. 1.0 fully inside, 0.0 fully outside, with a ~1px feather across the
/// boundary. Mirrors the shader's per-quadrant SDF so the headless `jetty-shot`
/// (CPU compositing) applies the SAME mask as the live GPU pass.
#[allow(clippy::too_many_arguments)]
pub fn rounded_rect_coverage_per(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r_tl: f32,
    r_tr: f32,
    r_bl: f32,
    r_br: f32,
) -> f32 {
    if r_tl <= 0.0 && r_tr <= 0.0 && r_bl <= 0.0 && r_br <= 0.0 {
        return 1.0;
    }
    let max_r = w.min(h) / 2.0;
    let clamp_r = |r: f32| r.min(max_r).max(0.0);
    let hw = w / 2.0;
    let hh = h / 2.0;
    // Center-relative pixel center (+0.5 to sample the pixel center).
    let px = (x + 0.5) - hw;
    let py = (y + 0.5) - hh;
    // Select the radius for the quadrant this pixel lies in (matches the shader).
    let r_top = if px > 0.0 { r_tr } else { r_tl };
    let r_bot = if px > 0.0 { r_br } else { r_bl };
    let r = clamp_r(if py > 0.0 { r_bot } else { r_top });
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

/// Uniform-radius shim over [`rounded_rect_coverage_per`] (all four corners
/// equal). Kept so existing callers (jetty-shot CPU compositing) are unchanged.
pub fn rounded_rect_coverage(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    rounded_rect_coverage_per(x, y, w, h, radius, radius, radius, radius)
}

#[cfg(test)]
mod tests {
    use super::{rounded_rect_coverage, rounded_rect_coverage_per};

    #[test]
    fn top_flush_keeps_top_corners_square() {
        // Dropdown: top radii zeroed, bottom radii 16. The TOP corners must stay
        // square — a few px inside each top corner is fully opaque (a square
        // corner only feathers the single edge pixel, unlike a 16px-rounded
        // corner which carves out a whole quarter-disc). The BOTTOM corners round.
        let tl = rounded_rect_coverage_per(3.0, 3.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        let tr = rounded_rect_coverage_per(96.0, 3.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        assert!(tl > 0.99, "top-left should be square/opaque, got {tl}");
        assert!(tr > 0.99, "top-right should be square/opaque, got {tr}");
        // 6px in from each top corner along the would-be quarter-disc is still
        // opaque for the square corner (a 16px-rounded corner would be ~0 here).
        let tl_disc = rounded_rect_coverage_per(2.0, 2.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        assert!(tl_disc > 0.5, "square top corner not carved away, got {tl_disc}");
        let bl = rounded_rect_coverage_per(0.0, 99.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        let br = rounded_rect_coverage_per(99.0, 99.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        assert!(bl < 0.01, "bottom-left should round (transparent), got {bl}");
        assert!(br < 0.01, "bottom-right should round (transparent), got {br}");
        // And a few px into the bottom corner IS carved away (rounded).
        let br_disc = rounded_rect_coverage_per(96.0, 96.0, 100.0, 100.0, 0.0, 0.0, 16.0, 16.0);
        assert!(br_disc < 0.5, "bottom corner should be rounded, got {br_disc}");
    }

    #[test]
    fn per_corner_rounds_only_the_requested_corner() {
        // Only the bottom-right corner is rounded; the other three stay square
        // (sample a few px inside each square corner — fully opaque).
        let r = 16.0;
        let tl = rounded_rect_coverage_per(3.0, 3.0, 100.0, 100.0, 0.0, 0.0, 0.0, r);
        let tr = rounded_rect_coverage_per(96.0, 3.0, 100.0, 100.0, 0.0, 0.0, 0.0, r);
        let bl = rounded_rect_coverage_per(3.0, 96.0, 100.0, 100.0, 0.0, 0.0, 0.0, r);
        let br = rounded_rect_coverage_per(99.0, 99.0, 100.0, 100.0, 0.0, 0.0, 0.0, r);
        assert!(tl > 0.99 && tr > 0.99 && bl > 0.99, "three corners square");
        assert!(br < 0.01, "only bottom-right rounds, got {br}");
    }

    #[test]
    fn uniform_shim_matches_all_equal_per() {
        let a = rounded_rect_coverage(7.0, 3.0, 100.0, 80.0, 12.0);
        let b = rounded_rect_coverage_per(7.0, 3.0, 100.0, 80.0, 12.0, 12.0, 12.0, 12.0);
        assert_eq!(a, b);
    }

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
