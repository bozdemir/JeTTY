//! Crt post-effect — a GPU pass that samples a fully-rendered offscreen scene
//! and writes a CRT-styled result to the surface. The WGSL fragment applies, in
//! one sample pass: barrel/curvature warp (with a transparent bezel outside the
//! tube), chromatic aberration, tinted scanlines, a shadow-mask / aperture
//! grille, single-pass in-shader bloom, and a radial vignette. It ALSO computes
//! its own rounded-corner alpha (on the UN-warped output coords) so the
//! transparent rounded window corners are restored — the corner mask pass is
//! skipped while CRT is on, so the CRT pass owns the corners.
//!
//! Every effect is a no-op at its `param == 0`, so all-zero params reduce to a
//! near-passthrough blit (plus corner rounding). Animation (Task 10) reads the
//! `time` + `flags` uniform fields: bit0 = roll (scanline crawl), bit1 = flicker
//! (subtle global brightness wobble), bit2 = jitter (sub-pixel horizontal sample
//! shift). Each animated term collapses to the EXACT static result when its flag
//! bit is clear, so `flags == 0` is byte-identical to the static (Task 9) look.
//!
//! One fullscreen-triangle pass with bindings {uniform, src_texture, sampler}
//! and a `replace` blend (this pass owns every output pixel). Color and alpha are
//! both multiplied by the corner+bezel coverage so corners fade out cleanly
//! (mirrors mask.rs's dst-multiply convention — keeps premultiplication
//! consistent, never re-opaques the corners).
//!
//! Self-contained: our own wgpu/WGSL, no desktop-environment / compositor /
//! OS-specific code.

// 64-byte uniform. No vec3<f32> so the host (#[repr(C)]) layout matches the WGSL
// struct byte-for-byte. Field byte offsets (Rust == WGSL), all naturally aligned:
//   resolution    vec2<f32>  @  0   (align 8)
//   curvature     f32        @  8
//   scanline      f32        @ 12
//   mask          f32        @ 16
//   bloom         f32        @ 20
//   chromatic     f32        @ 24
//   vignette      f32        @ 28
//   tint          vec4<f32>  @ 32   (align 16; rgb + pad)
//   corner_radius f32        @ 48   (BOTTOM corners)
//   time          f32        @ 52   (animation phase, seconds — Task 10)
//   flags         u32        @ 56   (roll/flicker/jitter bitfield — Task 10)
//   corner_radius_top f32    @ 60   (TOP corners; 0 when top-flush Dropdown)
//   => size 64, align 16.

/// CRT animation flag bits packed into [`CrtUniform::flags`]. This is the single
/// source of truth for the bit layout: the Rust packing site (jetty-app) ORs
/// these together, and the WGSL fragment tests the SAME bit positions with literal
/// masks (`(flags & 1u) != 0u`, etc.). Keep the WGSL masks in `CRT_SHADER` in sync
/// with these values.
pub const CRT_FLAG_ROLL: u32 = 1 << 0; // bit0: rolling scanline crawl
pub const CRT_FLAG_FLICKER: u32 = 1 << 1; // bit1: global brightness flicker
pub const CRT_FLAG_JITTER: u32 = 1 << 2; // bit2: sub-pixel horizontal jitter

pub(crate) const CRT_SHADER: &str = r#"
struct P {
    resolution: vec2<f32>,
    curvature: f32,
    scanline: f32,
    mask: f32,
    bloom: f32,
    chromatic: f32,
    vignette: f32,
    tint: vec4<f32>,
    corner_radius: f32,
    time: f32,
    flags: u32,
    corner_radius_top: f32,
};
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

const PI: f32 = 3.14159265359;

// Task 10 animation tunables. Each animated term is gated by a flag bit and is a
// no-op when that bit is clear (see the fragment), so these only matter while the
// matching toggle is on. Kept tasteful and subtle (sub-strobe, sub-pixel).
const ROLL_SPEED: f32 = 6.0;     // scanline phase advance (rad/s): gentle crawl
const FLICKER_FREQ: f32 = 50.0;  // brightness wobble angular freq (rad/s, ~8 Hz)
const FLICKER_AMP: f32 = 0.04;   // brightness dip amplitude (4%), sub-strobe
const JITTER_AMP: f32 = 0.5;     // horizontal sync jitter peak (sub-pixel px)

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let v = verts[vi];
    var o: VsOut;
    o.pos = vec4(v, 0.0, 1.0);
    // uv in 0..1, y down (matches the offscreen frame's orientation).
    o.uv = vec2(v.x * 0.5 + 0.5, 1.0 - (v.y * 0.5 + 0.5));
    return o;
}

// Rounded-rect SDF (mirrors mask.rs / phosphor.rs). Negative inside, 0 on the
// edge, positive outside. `b` is the half-size, `r` the corner radius.
fn sd_round_rect(pt: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(pt) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

// One thresholded bloom tap: the scene color at `uv`, keeping only its bright
// part (smoothstep on max channel). textureSampleLevel avoids derivative /
// uniformity constraints so it is safe to call many times here.
fn bright_tap(uv: vec2<f32>) -> vec3<f32> {
    let c = textureSampleLevel(src_tex, src_samp, uv, 0.0).rgb;
    let l = max(c.r, max(c.g, c.b));
    return c * smoothstep(0.55, 0.9, l);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let res = p.resolution;

    // --- 0) Animation flags (Task 10). Each animated term below is a no-op when
    // its bit is clear, so flags == 0 reproduces the static (Task 9) result
    // byte-for-byte. Bit layout mirrors CRT_FLAG_ROLL/FLICKER/JITTER in crt.rs
    // (the Rust packing site). ---
    let roll_on = (p.flags & 1u) != 0u;     // bit0: rolling scanline
    let flicker_on = (p.flags & 2u) != 0u;  // bit1: brightness flicker
    let jitter_on = (p.flags & 4u) != 0u;   // bit2: horizontal jitter

    // --- 1) Barrel / curvature warp of the SAMPLE uv (output uv stays put). ---
    // Push coords outward by the square of the orthogonal axis; identity at
    // curvature == 0. Near the edges the warped uv leaves [0,1] -> bezel below.
    let cc = in.uv * 2.0 - 1.0;             // centered, [-1,1]
    let warp = p.curvature * 0.25;          // 0 => identity
    let suv0 = (cc + cc * (cc.yx * cc.yx) * warp) * 0.5 + 0.5;
    // Jitter (bit2): a sub-pixel horizontal sample shift (analog h-sync wobble).
    // Product of two incommensurate sines -> irregular, bounded to +/-JITTER_AMP
    // px. jitter_dx == 0.0 exactly when off, so suv == suv0 (static sampling
    // unchanged; +0.0 / 0.0-offset is an exact float identity).
    let jitter_px = sin(p.time * 80.0) * sin(p.time * 13.0) * JITTER_AMP;
    let jitter_dx = select(0.0, jitter_px / res.x, jitter_on);
    let suv = suv0 + vec2(jitter_dx, 0.0);

    // Bezel: feather to transparent JUST OUTSIDE the unit box. No-op at
    // curvature == 0 (suv == in.uv stays within [0,1], so `outside` <= 0 and the
    // outermost edge pixels keep full coverage — true passthrough at the edge).
    let outside = max(max(-suv.x, suv.x - 1.0), max(-suv.y, suv.y - 1.0));
    let fpx = 1.5 / max(res.x, res.y);
    let bezel = 1.0 - smoothstep(0.0, fpx, outside);

    // --- 2) Chromatic aberration: R/G/B diverge along the radius, growing with
    // distance from center; identity at chromatic == 0. ---
    let dir = suv - vec2(0.5, 0.5);
    let ca = p.chromatic * 0.006;
    let cr = textureSampleLevel(src_tex, src_samp, suv + dir * ca, 0.0).r;
    let cg = textureSampleLevel(src_tex, src_samp, suv, 0.0);
    let cb = textureSampleLevel(src_tex, src_samp, suv - dir * ca, 0.0).b;
    var col = vec3(cr, cg.g, cb);
    let scene_a = cg.a;                      // carry the window's alpha through

    // --- 3) Scanlines (output space), tinted by p.tint.rgb. Neutral darkening
    // at the default white tint; identity at scanline == 0. ---
    // Roll (bit0): advance the scanline phase by time so the lines crawl. The
    // added phase is exactly 0.0 when off, so the static beam pattern is unchanged.
    let roll_phase = select(0.0, p.time * ROLL_SPEED, roll_on);
    let beam = 0.5 + 0.5 * sin(in.uv.y * res.y * PI + roll_phase);   // [0,1], 1 on the dark line
    let darken = p.scanline * beam;
    let scan_mul = (1.0 - darken) * mix(vec3(1.0, 1.0, 1.0), p.tint.rgb, darken);
    col = col * scan_mul;

    // --- 4) Shadow-mask / aperture grille: vertical RGB stripes per output
    // column; identity at mask == 0. ---
    let idx = u32(floor(in.uv.x * res.x)) % 3u;
    let triad = vec3(
        select(0.0, 1.0, idx == 0u),
        select(0.0, 1.0, idx == 1u),
        select(0.0, 1.0, idx == 2u),
    );
    let depth = p.mask * 0.6;               // off-channels dim to (1 - depth)
    let grille = vec3(1.0, 1.0, 1.0) - depth * (vec3(1.0, 1.0, 1.0) - triad);
    col = col * grille;

    // --- 5) Single-pass bloom: 13 thresholded taps of the warped scene (center
    // + 4 axis @1.5px + 4 diagonal @1.5px + 4 axis @3px); identity at bloom == 0. ---
    let s1 = 1.5 / res;
    let s2 = 3.0 / res;
    var glow = bright_tap(suv) * 0.20;
    glow = glow + (bright_tap(suv + vec2(s1.x, 0.0)) + bright_tap(suv - vec2(s1.x, 0.0))
                 + bright_tap(suv + vec2(0.0, s1.y)) + bright_tap(suv - vec2(0.0, s1.y))) * 0.10;
    glow = glow + (bright_tap(suv + vec2(s1.x, s1.y)) + bright_tap(suv + vec2(s1.x, -s1.y))
                 + bright_tap(suv + vec2(-s1.x, s1.y)) + bright_tap(suv + vec2(-s1.x, -s1.y))) * 0.075;
    glow = glow + (bright_tap(suv + vec2(s2.x, 0.0)) + bright_tap(suv - vec2(s2.x, 0.0))
                 + bright_tap(suv + vec2(0.0, s2.y)) + bright_tap(suv - vec2(0.0, s2.y))) * 0.05;
    col = col + glow * p.bloom;

    // --- 6) Vignette: radial edge darkening (output space); identity at
    // vignette == 0. ---
    let vd = length(in.uv - vec2(0.5, 0.5)) * 1.41421356;   // 0 center -> ~1 corner
    let v = 1.0 - 0.85 * smoothstep(0.5, 1.15, vd);
    let vig = mix(1.0, v, p.vignette);
    col = col * vig;

    // --- 6b) Flicker (bit1): a subtle global brightness wobble (analog mains
    // flutter), low amplitude so it never strobes. flicker_mul == 1.0 exactly when
    // off, so the static brightness is unchanged (an exact *1.0 identity). Scales
    // color only, NOT the corner/bezel alpha below, so coverage is unaffected. ---
    let flick = 0.5 + 0.5 * sin(p.time * FLICKER_FREQ);   // [0,1]
    let flicker_mul = select(1.0, 1.0 - FLICKER_AMP * flick, flicker_on);
    col = col * flicker_mul;

    // --- 7) Rounded-corner alpha on the UN-WARPED output coords (so the rounding
    // is NOT distorted by curvature) — replicates mask.rs's SDF, feather and
    // radius clamp exactly so the corners match the non-CRT look. The TOP
    // corners use their own radius (0 for a top-flush Dropdown, mirroring the
    // corner mask's per-corner radii) so CRT-on never opens a transparent
    // notch at the monitor's top edge. ---
    let frag = in.uv * res;                 // un-warped output pixel position
    let half = res * 0.5;
    let pt = frag - half;
    let max_r = min(res.x, res.y) * 0.5;
    let rr_bot = clamp(p.corner_radius, 0.0, max_r);
    let rr_top = clamp(p.corner_radius_top, 0.0, max_r);
    // Quadrant-select the radius (top vs bottom half), like mask.rs's
    // sd_round_rect_per restricted to a top/bottom split.
    let rr = select(rr_top, rr_bot, pt.y > 0.0);
    let d = sd_round_rect(pt, half, rr);
    let cov_raw = 1.0 - smoothstep(-0.75, 0.75, d);
    // both radii <= 0 => fully opaque everywhere (square window, matches mask.rs skip).
    let cov = select(1.0, cov_raw, max(p.corner_radius, p.corner_radius_top) > 0.0);

    // Multiply BOTH (premultiplied) color and alpha by the combined coverage so
    // corners + bezel fade out without re-opaquing (mirrors mask.rs dst-multiply).
    let amask = cov * bezel;
    return vec4(col * amask, scene_a * amask);
}
"#;

/// Per-frame uniform for the CRT pass.
///
/// Layout: 64 bytes. No `vec3<f32>` so the Rust `#[repr(C)]` layout matches the
/// WGSL `struct P` byte-for-byte (see the offset table on `CRT_SHADER` above and
/// the `crt_uniform_layout` test). Every effect strength is a normalized 0..1
/// slider value; each is a no-op at 0. `time` (animation phase, seconds) and
/// `flags` (CRT_FLAG_* bitfield) drive the roll/flicker/jitter animation; with
/// `flags == 0` the shader output is identical to the static look.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CrtUniform {
    /// Physical width and height in pixels. (offset 0)
    pub resolution: [f32; 2],
    /// Barrel/curvature warp strength. (offset 8)
    pub curvature: f32,
    /// Scanline darkening strength. (offset 12)
    pub scanline: f32,
    /// Shadow-mask / aperture-grille strength. (offset 16)
    pub mask: f32,
    /// Bloom/glow strength. (offset 20)
    pub bloom: f32,
    /// Chromatic-aberration strength. (offset 24)
    pub chromatic: f32,
    /// Vignette strength. (offset 28)
    pub vignette: f32,
    /// Scanline tint: rgb in `[0..2]`, `[3]` is padding. (offset 32, align 16)
    pub tint: [f32; 4],
    /// Rounded-corner radius of the BOTTOM corners in physical px (matches the
    /// corner mask). (offset 48)
    pub corner_radius: f32,
    /// Animation phase in seconds (free-running clock). Drives roll/flicker/jitter
    /// via `sin`, so unbounded growth is fine. (offset 52)
    pub time: f32,
    /// Roll/flicker/jitter bitfield (`CRT_FLAG_*`). 0 => static look. (offset 56)
    pub flags: u32,
    /// Rounded-corner radius of the TOP corners in physical px. 0 when the
    /// window is top-flush (Dropdown mode keeps the top corners square, exactly
    /// like the non-CRT corner mask's per-corner radii). (offset 60)
    pub corner_radius_top: f32,
}

pub struct Crt {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Cached (src view, bind group); rebuilt only when the src view changes
    /// (e.g. on window resize). The bind group references the stable uniform
    /// buffer + sampler and the per-frame src texture view. `RefCell` because
    /// `apply` takes `&self` (caller may hold a `&mut gpu`).
    cached_bind: std::cell::RefCell<Option<(wgpu::TextureView, wgpu::BindGroup)>>,
}

impl Crt {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crt-shader"),
            source: wgpu::ShaderSource::Wgsl(CRT_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt-uniform"),
            size: std::mem::size_of::<CrtUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt-bgl"),
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
            label: Some("crt-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        // Replace blend: this pass owns every output pixel (it re-samples the
        // whole frame), so write src directly with no blending.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("crt-pipeline"),
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

    /// Run the full CRT post-pass: sample `src` (the offscreen rendered scene)
    /// and write the result into `dst` (the surface), applying curvature warp,
    /// scanlines, shadow-mask, bloom, chromatic aberration, vignette, optional
    /// roll/flicker/jitter animation, and rounded-corner alpha compositing.
    ///
    /// `width` and `height` are the physical pixel dimensions; they are written
    /// into the uniform as the pass resolution.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst: &wgpu::TextureView,
        src: &wgpu::TextureView,
        width: u32,
        height: u32,
        u: &CrtUniform,
    ) {
        // Use the caller's full uniform, overriding resolution from the explicit
        // width/height so callers don't have to compute it twice.
        let mut uniform = *u;
        uniform.resolution = [width as f32, height as f32];
        // Wrap the animation phase into [0, 2π) before the f32 write. Every shader
        // rate (6/13/50/80 rad/s) is an integer, so wrapping at 2π preserves every
        // sin() phase exactly while keeping `time` small — otherwise f32 precision
        // decays with uptime and the roll/flicker/jitter animation quantizes into
        // visible strobing after a few days of a terminal being left open.
        uniform.time = (u.time as f64 % std::f64::consts::TAU) as f32;
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniform));

        // Rebuild the bind group only when the src view changes (resize);
        // otherwise reuse the cached one to avoid a GPU allocator round-trip on
        // every frame.
        let mut cache = self.cached_bind.borrow_mut();
        let stale = cache.as_ref().map(|(v, _)| v != src).unwrap_or(true);
        if stale {
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("crt-bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            *cache = Some((src.clone(), bg));
        }
        let bind_group = &cache.as_ref().unwrap().1;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("crt-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("crt-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    resolve_target: None,
                    // REPLACE + fullscreen triangle overwrites every pixel, so Clear
                    // (not Load) is identical output while sparing tile-based GPUs a
                    // full-surface load every frame the CRT pass runs.
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

    /// Validate the CRT WGSL shader compiles and passes naga's validator without
    /// requiring a GPU adapter. This is the always-run gate for the shader source.
    #[test]
    fn crt_shader_compiles() {
        let module = naga::front::wgsl::parse_str(CRT_SHADER)
            .expect("CRT_SHADER must parse as valid WGSL");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("CRT_SHADER must pass naga validation");
    }

    /// The Rust `CrtUniform` layout must match the WGSL `struct P` byte-for-byte
    /// (see the offset table on `CRT_SHADER`). If these diverge, `write_buffer`
    /// would feed the shader misaligned fields. 64 bytes, 16-byte aligned.
    #[test]
    fn crt_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<CrtUniform>(), 64, "CrtUniform must be 64 bytes");
        assert_eq!(offset_of!(CrtUniform, resolution), 0);
        assert_eq!(offset_of!(CrtUniform, curvature), 8);
        assert_eq!(offset_of!(CrtUniform, scanline), 12);
        assert_eq!(offset_of!(CrtUniform, mask), 16);
        assert_eq!(offset_of!(CrtUniform, bloom), 20);
        assert_eq!(offset_of!(CrtUniform, chromatic), 24);
        assert_eq!(offset_of!(CrtUniform, vignette), 28);
        assert_eq!(offset_of!(CrtUniform, tint), 32);
        assert_eq!(offset_of!(CrtUniform, corner_radius), 48);
        assert_eq!(offset_of!(CrtUniform, time), 52);
        assert_eq!(offset_of!(CrtUniform, flags), 56);
        assert_eq!(offset_of!(CrtUniform, corner_radius_top), 60);
        // bytemuck::Pod requires no padding gaps; size is a multiple of align.
        assert_eq!(size_of::<CrtUniform>() % align_of::<CrtUniform>(), 0);
    }

    /// Smoke-test that `Crt::new` succeeds on a real wgpu device.
    /// Gated with `#[ignore]` because a GPU adapter may be unavailable in
    /// headless / CI environments. Run manually with:
    ///   cargo test -p jetty-render crt_new_with_device -- --ignored
    #[test]
    #[ignore]
    fn crt_new_with_device() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("adapter");
        let (device, _queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .expect("device");
        let _crt = Crt::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    }
}
