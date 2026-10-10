//! Caret glow/ripple GPU pass — an OPTIONAL fullscreen-triangle pass that
//! draws a soft radial halo plus an expanding ring around the cursor cell on
//! each keystroke burst, in the main window AND detached windows.
//!
//! The pass is dispatched ONLY when `caret_glow_enabled` is true AND a caret
//! burst is live; otherwise it is a true zero-cost no-op, and no pipeline exists
//! until the glow is first turned on (`CaretFx::prepare` builds just the
//! variant the theme needs).
//!
//! Two blend variants, picked by the theme background ([`caret_glow_look`]):
//! * DARK themes — additive (src=One / dst=One): the pass only ever BRIGHTENS.
//!   Alpha output is 0 so the destination alpha is untouched. NOTE: on a
//!   PreMultiplied surface the compositor still displays nonzero RGB at
//!   alpha=0, so this pass must run BEFORE the corner mask — the mask's coverage
//!   multiply then clips the glow at the rounded corners.
//! * LIGHT themes — a multiply tint (src=Zero / dst=Src): an additive glow is
//!   invisible on a near-white page, so the halo DARKENS toward the
//!   contrast-safe flash color instead (black for the default white flash).
//!   Multiplying color by ≤ 1 keeps premultiplication valid; alpha is untouched.
//!
//! The halo/ring is negligible beyond ~3.7 cells, so the fullscreen triangle is
//! scissored to ±4.5 cells around the cursor ([`caret_glow_scissor`]) — ~95% less
//! fragment work at a typical window size, identical pixels.
//!
//! Self-contained: our own wgpu/WGSL, no offscreen texture, no scene sampling.
//! Model: phosphor.rs (`fs_glow` additive pass).

// 48-byte uniform. No vec3<f32> so the Rust #[repr(C)] layout matches the WGSL
// struct byte-for-byte. Field byte offsets (Rust == WGSL), all naturally aligned:
//   resolution  vec2<f32>  @  0  (align 8)
//   cursor_px   vec2<f32>  @  8  (align 8)
//   cell        vec2<f32>  @ 16  (align 8)
//   t           f32        @ 24  (align 4)
//   intensity   f32        @ 28  (align 4)
//   color       vec4<f32>  @ 32  (align 16; rgb + pad)
//   => size 48, align 4, 48 % 16 == 0 (satisfies WebGPU uniform stride req.).

const CARET_FX_SHADER: &str = r#"
// 48-byte uniform. No vec3<f32>; field offsets match the Rust struct exactly.
struct C {
    resolution: vec2<f32>,   // physical size (px)          @ 0
    cursor_px:  vec2<f32>,   // cursor cell centre (px)     @ 8
    cell:       vec2<f32>,   // cell size (w, h in px)      @ 16
    t:          f32,          // burst progress [0..1]       @ 24
    intensity:  f32,          // effect brightness           @ 28
    color:      vec4<f32>,   // linear rgb + pad            @ 32
};
@group(0) @binding(0) var<uniform> p: C;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let v = verts[vi];
    var o: VsOut;
    o.pos = vec4(v, 0.0, 1.0);
    // uv in 0..1, y-down (matches offscreen / surface frame orientation).
    o.uv = vec2(v.x * 0.5 + 0.5, 1.0 - (v.y * 0.5 + 0.5));
    return o;
}

// The halo + ring intensity (0..1) at this fragment.
fn glow_at(frag: vec2<f32>) -> f32 {
    let t = clamp(p.t, 0.0, 1.0);
    // Distance from fragment to the cursor cell centre (pixels). Fragment
    // position: y=0 at the top-left of the viewport, the cursor_px convention.
    let d = length(frag - p.cursor_px);
    // Characteristic cell radius used to scale falloff distances. At least a
    // pixel, like the scissor (`caret_glow_scissor`): a cell is never smaller,
    // but the falloffs divide by it.
    let cell_r = max(max(p.cell.x, p.cell.y), 1.0);
    // --- Halo: Gaussian radial glow centred on the cursor, fading with time. ---
    // sigma = 1.5 * cell_r  => at d = 2*cell_r: exp(-4/2.25) ≈ 0.17 (still warm),
    //                          at d = 3*cell_r: exp(-4)      ≈ 0.02 (near zero).
    // Temporal envelope (1-t): halo is bright at burst start, gone at t=1 so
    // when the burst expires the last frame renders zero contribution cleanly.
    let sigma = 1.5 * cell_r;
    let halo = (1.0 - t) * exp(-d * d / (sigma * sigma));
    // --- Ring: expanding ripple, fades as (1-t). Radius grows linearly from 0
    // at t=0 to 2.5*cell_r at t=1; Gaussian width 0.4 cells. ---
    let ring_radius = 2.5 * cell_r * t;
    let ring_w = 0.4 * cell_r;
    let delta = d - ring_radius;
    let ring = (1.0 - t) * exp(-(delta * delta) / (ring_w * ring_w));
    // Clamped so intensity spikes don't oversaturate.
    return clamp(halo + ring, 0.0, 1.0);
}

// DARK themes (additive blend): RGB only, alpha = 0 so the destination alpha
// (the window's premultiplied transparency / rounded corners) is untouched.
@fragment
fn fs_add(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(p.color.rgb * p.intensity * glow_at(in.pos.xy), 0.0);
}

// LIGHT themes (multiply blend, dst *= src): darken toward the glow color —
// 1 where there is no glow, `color` at full strength. Alpha 1 keeps dst alpha.
@fragment
fn fs_mul(in: VsOut) -> @location(0) vec4<f32> {
    let k = p.intensity * glow_at(in.pos.xy);
    return vec4<f32>(vec3<f32>(1.0, 1.0, 1.0) - k * (vec3<f32>(1.0, 1.0, 1.0) - p.color.rgb), 1.0);
}
"#;

/// Per-dispatch uniform for the caret glow/ripple pass.
///
/// Layout: 48 bytes. No `vec3<f32>` so the Rust `#[repr(C)]` layout matches the
/// WGSL `struct C` byte-for-byte (see the offset table above and the
/// `caret_fx_uniform_layout` test).
///
/// ```text
/// Field       WGSL type   Rust type   Offset  Size
/// resolution  vec2<f32>   [f32; 2]     0       8
/// cursor_px   vec2<f32>   [f32; 2]     8       8
/// cell        vec2<f32>   [f32; 2]    16       8
/// t           f32         f32         24       4
/// intensity   f32         f32         28       4
/// color       vec4<f32>   [f32; 4]    32      16
///                                  total: 48 bytes
/// ```
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CaretFxUniform {
    /// Physical surface size (width, height) in pixels. (offset 0)
    pub resolution: [f32; 2],
    /// Cursor cell centre in pixels:
    ///   x = col * cell_w + cell_w/2
    ///   y = row * cell_h + grid_top_offset + slide_y + cell_h/2
    /// (offset 8)
    pub cursor_px: [f32; 2],
    /// Cell size (width, height) in pixels. (offset 16)
    pub cell: [f32; 2],
    /// Burst progress [0..1]; 0 = keystroke start, 1 = animation end. (offset 24)
    pub t: f32,
    /// Effect brightness multiplier [0..1]. (offset 28)
    pub intensity: f32,
    /// Glow colour: sRGB rgb in [0..1], [3] is padding. (offset 32)
    pub color: [f32; 4],
}

impl CaretFxUniform {
    /// The uniform as the shader reads it: the color in LINEAR light, which
    /// both variants work in (the additive one adds it to the sRGB target,
    /// the multiply one scales the page toward it) — like every color the
    /// quad and ring shaders draw (`s2l`).
    fn for_gpu(&self) -> Self {
        let [r, g, b, pad] = self.color;
        let [r, g, b] = [r, g, b].map(crate::crt::srgb_to_linear);
        CaretFxUniform { color: [r, g, b, pad], ..*self }
    }
}

/// Glow strength on a dark page (additive): bright enough to be visible,
/// subtle enough not to dominate (the pre-light-theme constant).
pub const CARET_GLOW_INTENSITY_DARK: f32 = 0.5;
/// Glow strength on a light page (multiply): darkening reads stronger than
/// brightening, so the halo is gentler there.
pub const CARET_GLOW_INTENSITY_LIGHT: f32 = 0.35;
/// The glow never reaches past this many cells (cell = the larger cell side)
/// from the cursor centre: the halo is exp(-9) ~ 1e-4 there and the ring's
/// outer edge sits at 2.5 + 3 x 0.4 = 3.7 cells.
pub const CARET_GLOW_REACH_CELLS: f32 = 4.5;

/// How the glow looks on a page of color `theme_bg` for the configured caret
/// `flash` color: `(light, color, intensity)`. `light` picks the multiply-tint
/// variant; `color` is the contrast-safe flash target against the page (white
/// stays white on a dark theme; it becomes black on a light one, where a white
/// glow is invisible).
pub fn caret_glow_look(flash: [f32; 3], theme_bg: [u8; 3]) -> (bool, [f32; 3], f32) {
    let light = crate::colors::is_light_bg(theme_bg);
    let color = crate::colors::caret_flash_target(flash, theme_bg, theme_bg);
    let intensity = if light { CARET_GLOW_INTENSITY_LIGHT } else { CARET_GLOW_INTENSITY_DARK };
    (light, color, intensity)
}

/// The scissor rect `[x, y, w, h]` (physical px) holding every pixel the glow
/// can touch: +/-[`CARET_GLOW_REACH_CELLS`] around `cursor_px`, clamped to the
/// `width`x`height` target. `None` when it falls entirely outside.
pub fn caret_glow_scissor(cursor_px: [f32; 2], cell: [f32; 2], width: u32, height: u32) -> Option<[u32; 4]> {
    let reach = CARET_GLOW_REACH_CELLS * cell[0].max(cell[1]).max(1.0);
    let clamp_x = |v: f32| v.clamp(0.0, width as f32);
    let clamp_y = |v: f32| v.clamp(0.0, height as f32);
    let (x0, x1) = (clamp_x((cursor_px[0] - reach).floor()), clamp_x((cursor_px[0] + reach).ceil()));
    let (y0, y1) = (clamp_y((cursor_px[1] - reach).floor()), clamp_y((cursor_px[1] + reach).ceil()));
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    (w > 0 && h > 0).then_some([x0 as u32, y0 as u32, w, h])
}

/// Caret glow/ripple GPU pass. Draws a soft halo + expanding ring around the
/// cursor cell on each keystroke burst -- additive on dark themes, a multiply
/// tint on light ones. Built (by the window that needs it) when the glow is
/// first enabled; each blend variant is compiled on first [`CaretFx::prepare`].
pub struct CaretFx {
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    /// Additive (dark theme) and multiply (light theme) pipelines, compiled on
    /// demand.
    add: Option<wgpu::RenderPipeline>,
    mul: Option<wgpu::RenderPipeline>,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl CaretFx {
    /// Create the shader + buffers; no pipeline is compiled until
    /// [`CaretFx::prepare`]. `format` must match the render target.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("caret-fx-shader"),
            source: wgpu::ShaderSource::Wgsl(CARET_FX_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caret-fx-uniform"),
            size: std::mem::size_of::<CaretFxUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caret-fx-bgl"),
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
            label: Some("caret-fx-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("caret-fx-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        Self { shader, pipeline_layout, format, add: None, mul: None, uniform_buf, bind_group }
    }

    /// Compile the variant for a `light` (multiply) or dark (additive) page if
    /// it does not exist yet. Cheap (one `Option` check) once built; call it
    /// outside the frame's borrows, before [`CaretFx::apply`].
    pub fn prepare(&mut self, device: &wgpu::Device, light: bool) {
        let slot = if light { &mut self.mul } else { &mut self.add };
        if slot.is_some() {
            return;
        }
        let (entry, blend) = if light {
            // dst.rgb *= src.rgb; dst.a unchanged.
            let mul = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::Src,
                operation: wgpu::BlendOperation::Add,
            };
            let keep = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            };
            ("fs_mul", wgpu::BlendState { color: mul, alpha: keep })
        } else {
            // Additive: only ever brightens; alpha output 0 leaves dst alpha.
            let add = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            };
            ("fs_add", wgpu::BlendState { color: add, alpha: add })
        };
        *slot = Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(if light { "caret-fx-mul" } else { "caret-fx-add" }),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some(entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        }));
    }

    /// Draw the caret glow/ripple onto `dst` (additive on a dark page, a
    /// multiply tint on a `light` one -- [`CaretFx::prepare`] that variant
    /// first; an unprepared variant draws nothing), scissored to the reach of
    /// the effect around `u.cursor_px`.
    ///
    /// Uses `LoadOp::Load` (composites with the existing frame content). Call
    /// only when the glow effect is enabled AND a burst is live AND the cursor
    /// is visible.
    ///
    /// Compositing targets (caller must pick the right `dst`; dispatch BEFORE
    /// the corner mask so the mask's coverage multiply clips the glow at the
    /// rounded corners -- an additive pass after the mask would put nonzero RGB
    /// into alpha=0 corner pixels, which PreMultiplied compositors display):
    /// - CRT ON:    `scene_view` (the offscreen) -- the CRT pass then processes
    ///   and rounds corners. Glow gets full CRT treatment.
    /// - CRT OFF:   `scene_view` (== surface view) -- the corner mask runs
    ///   after this pass and clips the glow to the window shape.
    /// - Tier-B:    `scene_view` (== offscreen) -- the Tier-B effect resamples
    ///   it; glow is displaced/blurred like the rest of the scene.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst: &wgpu::TextureView,
        u: &CaretFxUniform,
        light: bool,
    ) {
        let Some(pipeline) = (if light { self.mul.as_ref() } else { self.add.as_ref() }) else {
            return;
        };
        let (w, h) = (u.resolution[0].max(0.0) as u32, u.resolution[1].max(0.0) as u32);
        let Some([sx, sy, sw, sh]) = caret_glow_scissor(u.cursor_px, u.cell, w, h) else {
            return;
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u.for_gpu()));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("caret-fx-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("caret-fx-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_scissor_rect(sx, sy, sw, sh);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validate the caret-fx WGSL shader compiles and passes naga's validator
    /// without requiring a GPU adapter. Always-run gate for the shader source.
    #[test]
    fn caret_fx_shader_compiles() {
        let module = naga::front::wgsl::parse_str(CARET_FX_SHADER)
            .expect("CARET_FX_SHADER must parse as valid WGSL");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("CARET_FX_SHADER must pass naga validation");
    }

    /// The glow color is an sRGB color (the configured `caret_flash_color`),
    /// and both variants work in LINEAR light — the additive one adds it, the
    /// multiply one scales the page toward it — so it is linearized for the
    /// GPU; the rest of the uniform goes up unchanged. The defaults (white on
    /// a dark page, black on a light one) are the same in both spaces.
    #[test]
    fn the_glow_color_goes_up_as_linear_light() {
        let u = CaretFxUniform {
            resolution: [800.0, 600.0],
            cursor_px: [10.0, 20.0],
            cell: [9.0, 18.0],
            t: 0.3,
            intensity: 0.5,
            color: [0.5, 1.0, 0.0, 0.0],
        };
        let g = u.for_gpu();
        assert!((g.color[0] - 0.214_041).abs() < 1e-5, "{:?}", g.color);
        assert_eq!(&g.color[1..], &[1.0, 0.0, 0.0]);
        assert_eq!((g.resolution, g.cursor_px, g.cell, g.t, g.intensity), (u.resolution, u.cursor_px, u.cell, u.t, u.intensity));
    }

    #[test]
    fn caret_fx_has_both_blend_variants() {
        assert!(CARET_FX_SHADER.contains("fn fs_add("));
        assert!(CARET_FX_SHADER.contains("fn fs_mul("));
    }

    #[test]
    fn glow_scissor_covers_the_reach_and_clamps_to_the_target() {
        // Mid-screen: a (2 x 4.5 cells of the larger side) square around the cursor.
        let r = caret_glow_scissor([500.0, 300.0], [10.0, 20.0], 1000, 640).unwrap();
        assert_eq!(r, [410, 210, 180, 180]);
        // ~5% of a 1000x640 frame -- the fragment work the clip saves.
        assert!((r[2] * r[3]) as f32 / (1000.0 * 640.0) < 0.06);
        // At the top-left corner it clamps to the target.
        let c = caret_glow_scissor([5.0, 10.0], [10.0, 20.0], 1000, 640).unwrap();
        assert_eq!((c[0], c[1]), (0, 0));
        assert!(c[2] <= 1000 && c[3] <= 640);
        // Entirely off-target -> nothing to draw.
        assert!(caret_glow_scissor([5000.0, 10.0], [10.0, 20.0], 1000, 640).is_none());
    }

    #[test]
    fn the_scissor_drops_nothing_visible() {
        // Mirror the shader: at the scissor edge (4.5 cells) the halo is below
        // half an 8-bit step at full intensity, and the ring has ended.
        let halo_at_edge = (-(4.5f32 / 1.5).powi(2)).exp();
        assert!(halo_at_edge * 255.0 < 0.5, "halo {halo_at_edge}");
        let ring_edge = 2.5 + 3.0 * 0.4;
        assert!(ring_edge < CARET_GLOW_REACH_CELLS);
    }

    #[test]
    fn glow_look_follows_the_page() {
        // Dark page: additive, the configured white, full strength (as before).
        let (light, color, k) = caret_glow_look([1.0; 3], [11, 14, 20]);
        assert!(!light);
        assert_eq!(color, [1.0; 3]);
        assert_eq!(k, CARET_GLOW_INTENSITY_DARK);
        // Light page (solarized_light): multiply toward black, gentler.
        let (light, color, k) = caret_glow_look([1.0; 3], [253, 246, 227]);
        assert!(light);
        assert_eq!(color, [0.0; 3]);
        assert_eq!(k, CARET_GLOW_INTENSITY_LIGHT);
    }

    /// The Rust `CaretFxUniform` layout must match the WGSL `struct C`
    /// byte-for-byte (see the offset table on `CARET_FX_SHADER`). If these
    /// diverge, `write_buffer` would feed the shader misaligned fields.
    /// 48 bytes, naturally aligned (all f32/[f32;N] fields, align 4).
    #[test]
    fn caret_fx_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<CaretFxUniform>(), 48, "CaretFxUniform must be 48 bytes");
        assert_eq!(align_of::<CaretFxUniform>(), 4);
        assert_eq!(offset_of!(CaretFxUniform, resolution), 0);
        assert_eq!(offset_of!(CaretFxUniform, cursor_px),   8);
        assert_eq!(offset_of!(CaretFxUniform, cell),       16);
        assert_eq!(offset_of!(CaretFxUniform, t),          24);
        assert_eq!(offset_of!(CaretFxUniform, intensity),  28);
        assert_eq!(offset_of!(CaretFxUniform, color),      32);
        // size must be a multiple of align (bytemuck::Pod requirement)
        assert_eq!(size_of::<CaretFxUniform>() % align_of::<CaretFxUniform>(), 0);
        // size must be a multiple of 16 (WebGPU uniform stride)
        assert_eq!(size_of::<CaretFxUniform>() % 16, 0);
    }
}
