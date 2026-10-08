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
        // All-flat corners ⇒ the pass would be a no-op multiply by 1.0: skip it
        // entirely (one fewer render pass + uniform write per frame). The app's
        // fullscreen corner suppression feeds radius 0.0 and so lands here — but
        // that saves exactly THIS PASS, not the frame: every other pass still
        // scales with surface AREA, and a fullscreen surface is several times the
        // default window (see the CHANGELOG's honest cost note). Pinned by
        // `all_radii_flat`'s unit test.
        if all_radii_flat(r_tl, r_tr, r_bl, r_br) {
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
            // Coverage is < 1 only near the rounded corners and on the outermost
            // pixel row/column (the ~1px edge feather), so scissor the fullscreen
            // triangle to just those regions instead of a read-modify-write blend
            // over the WHOLE surface every frame. Pixels outside are multiplied by
            // exactly 1.0 by the old full-screen pass, so the result is identical.
            for [x, y, w, h] in mask_regions(width, height, [params[2], params[3], params[4], params[5]]) {
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..3, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// The scissor rects `[x, y, w, h]` (physical px, within a `width`×`height`
/// target) outside of which the corner mask's coverage is exactly 1.0: one square
/// per rounded corner — its radius plus the ~1px antialias feather — and the four
/// 1px edge strips (the SDF puts the outermost pixel row/column at d = −0.5, i.e.
/// coverage ≈ 0.93, whenever any corner is rounded). `radii` = already-clamped
/// [tl, tr, bl, br].
///
/// The rects are DISJOINT: the pass multiplies by coverage once per draw, so a
/// pixel inside two rects would be masked twice (too transparent). The strips
/// stop where the corner squares begin, and a target too small for the squares
/// to stay apart is masked by one full-target rect instead.
fn mask_regions(width: u32, height: u32, radii: [f32; 4]) -> Vec<[u32; 4]> {
    let mut out = Vec::with_capacity(8);
    if width == 0 || height == 0 {
        return out;
    }
    let side = |r: f32| if r > 0.0 { (r.ceil() as u32) + 2 } else { 0 };
    let [tl, tr, bl, br] = radii.map(side);
    if tl + tr > width || bl + br > width || tl + bl > height || tr + br > height {
        out.push([0, 0, width, height]);
        return out;
    }
    for (s, right, bottom) in [(tl, false, false), (tr, true, false), (bl, false, true), (br, true, true)] {
        if s > 0 {
            out.push([if right { width - s } else { 0 }, if bottom { height - s } else { 0 }, s, s]);
        }
    }
    let mut strip = |x: u32, y: u32, w: u32, h: u32| {
        if w > 0 && h > 0 {
            out.push([x, y, w, h]);
        }
    };
    // Top/bottom rows between the corner squares (one row when height == 1).
    strip(tl, 0, width - tl - tr, 1);
    if height > 1 {
        strip(bl, height - 1, width - bl - br, 1);
    }
    // Left/right columns between the squares, minus the rows the top/bottom
    // strips already cover (one column when width == 1).
    let span = |a: u32, b: u32| {
        let (y0, y1) = (a.max(1), height.saturating_sub(b.max(1)));
        (y0, y1.saturating_sub(y0))
    };
    let (y0, h) = span(tl, bl);
    strip(0, y0, 1, h);
    if width > 1 {
        let (y0, h) = span(tr, br);
        strip(width - 1, y0, 1, h);
    }
    // Pathological shapes (corner squares meeting across a small or very
    // non-square target) can still make two rects touch the same pixel: mask
    // the whole target once instead.
    let overlaps = |a: &[u32; 4], b: &[u32; 4]| {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    };
    if (0..out.len()).any(|i| (i + 1..out.len()).any(|j| overlaps(&out[i], &out[j]))) {
        return vec![[0, 0, width, height]];
    }
    out
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

/// Whether all four corner radii are flat (≤ 0), i.e. the rounding pass would be
/// a no-op. `CornerMask::apply` early-returns on this, which is the load-bearing
/// property behind "fullscreen skips the corner-mask pass entirely": the app feeds
/// radius `0.0` while fullscreen instead of adding a shader/uniform branch.
///
/// Extracted as a pure fn so the early-return can be unit-tested without a GPU.
pub fn all_radii_flat(r_tl: f32, r_tr: f32, r_bl: f32, r_br: f32) -> bool {
    r_tl <= 0.0 && r_tr <= 0.0 && r_bl <= 0.0 && r_br <= 0.0
}

/// Uniform-radius shim over [`rounded_rect_coverage_per`] (all four corners
/// equal). Kept so existing callers (jetty-shot CPU compositing) are unchanged.
pub fn rounded_rect_coverage(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    rounded_rect_coverage_per(x, y, w, h, radius, radius, radius, radius)
}

#[cfg(test)]
mod tests {
    use super::{all_radii_flat, mask_regions, rounded_rect_coverage, rounded_rect_coverage_per};

    #[test]
    fn scissor_regions_never_overlap() {
        // Each draw multiplies by coverage again: a pixel in two rects would be
        // masked twice. Check every pixel is covered at most once, across sizes
        // (incl. tiny targets where the corner squares would collide).
        for (w, h) in [(1000u32, 640u32), (2000, 1280), (40, 30), (20, 20), (5, 3), (1, 1), (1, 9), (9, 1)] {
            for radii in [[10.0, 10.0, 10.0, 10.0], [20.0, 0.0, 0.0, 20.0], [0.0; 4], [3.0, 8.0, 0.0, 12.0]] {
                let mut hits = vec![0u8; (w * h) as usize];
                for [x, y, rw, rh] in mask_regions(w, h, radii) {
                    assert!(x + rw <= w && y + rh <= h, "rect out of bounds {w}x{h} {radii:?}");
                    for py in y..y + rh {
                        for px in x..x + rw {
                            hits[(py * w + px) as usize] += 1;
                        }
                    }
                }
                assert!(hits.iter().all(|&n| n <= 1), "overlapping regions at {w}x{h} {radii:?}");
            }
        }
    }

    #[test]
    fn scissor_regions_cover_every_pixel_the_mask_changes() {
        // The scissored pass must be byte-identical to the old full-screen one:
        // every pixel OUTSIDE the regions must have coverage exactly 1.0 (so the
        // full-screen multiply left it unchanged). Checked against the CPU mirror of
        // the shader, for symmetric, Dropdown (square top) and odd/huge radii.
        let cases: [(u32, u32, [f32; 4]); 5] = [
            (160, 90, [10.0, 10.0, 10.0, 10.0]),
            (160, 90, [0.0, 0.0, 16.0, 16.0]),
            (97, 61, [7.5, 3.2, 12.9, 0.4]),
            (64, 40, [20.0, 20.0, 20.0, 20.0]),
            (33, 200, [16.0, 16.0, 16.0, 16.0]),
        ];
        for (w, h, r) in cases {
            let regions = mask_regions(w, h, r);
            for &[x, y, rw, rh] in &regions {
                assert!(x + rw <= w && y + rh <= h, "region {x},{y} {rw}x{rh} outside {w}x{h}");
            }
            for py in 0..h {
                for px in 0..w {
                    let inside = regions
                        .iter()
                        .any(|&[x, y, rw, rh]| px >= x && px < x + rw && py >= y && py < y + rh);
                    if !inside {
                        let c = rounded_rect_coverage_per(
                            px as f32, py as f32, w as f32, h as f32, r[0], r[1], r[2], r[3],
                        );
                        assert_eq!(c, 1.0, "{w}x{h} r={r:?}: ({px},{py}) has coverage {c} but is not scissored in");
                    }
                }
            }
        }
    }

    #[test]
    fn corner_mask_zero_radius_is_a_no_op() {
        // The early-return `CornerMask::apply` takes: all-zero radii skip the pass.
        // This is the whole reason the app can suppress rounding while fullscreen by
        // feeding 0.0 — and get a CHEAPER frame than windowed — with no shader,
        // uniform or pipeline change.
        assert!(all_radii_flat(0.0, 0.0, 0.0, 0.0));
        // Negative is treated as flat too (defensive, matches `<= 0.0`).
        assert!(all_radii_flat(-1.0, 0.0, -0.5, 0.0));
        // ANY non-zero radius must still run the pass — including the Dropdown
        // bottom-only case, whose top radii are zero.
        assert!(!all_radii_flat(0.0, 0.0, 16.0, 16.0));
        assert!(!all_radii_flat(16.0, 16.0, 16.0, 16.0));
        assert!(!all_radii_flat(0.0, 0.0, 0.0, 0.001));
    }

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
