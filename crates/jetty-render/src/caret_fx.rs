//! Caret glow/ripple GPU pass — an OPTIONAL additive fullscreen pass that
//! draws a soft radial halo plus an expanding ring around the cursor cell on
//! each keystroke burst.
//!
//! The pass is dispatched ONLY when `caret_glow_enabled` is true AND
//! `caret_anim.is_some()`; otherwise it is a true zero-cost no-op.
//!
//! Additive blending (src=One / dst=One): the pass only ever BRIGHTENS pixels.
//! Alpha output is 0 so the window's premultiplied transparency is untouched —
//! transparent rounded corners stay at alpha=0 regardless of cursor proximity.
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
    color:      vec4<f32>,   // rgb + pad                   @ 32
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

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(p.t, 0.0, 1.0);
    // Fragment pixel position: y=0 at top-left, matching cursor_px coordinate space.
    // (@builtin(position) in a WebGPU fragment shader is in window/viewport space
    // with y=0 at the TOP of the viewport — same convention as cursor_px.)
    let frag = in.pos.xy;
    // Distance from fragment to the cursor cell centre (pixels).
    let d = length(frag - p.cursor_px);
    // Characteristic cell radius used to scale falloff distances.
    let cell_r = max(p.cell.x, p.cell.y);
    // --- Halo: Gaussian radial glow centred on the cursor, fading with time. ---
    // sigma = 1.5 * cell_r  => at d = 2*cell_r: exp(-4/2.25) ≈ 0.17 (still warm),
    //                          at d = 3*cell_r: exp(-4)      ≈ 0.02 (near zero).
    // Temporal envelope (1-t): halo is bright at burst start, gone at t=1 so
    // when caret_anim expires the last frame renders zero contribution cleanly.
    let sigma = 1.5 * cell_r;
    let halo = (1.0 - t) * exp(-d * d / (sigma * sigma));
    // --- Ring: expanding ripple, fades as (1-t). ---
    // Radius grows linearly from 0 at t=0 to 2.5*cell_r at t=1, so the ring
    // just reaches ~2.5 cells away as it fades out — visible but tasteful.
    let ring_radius = 2.5 * cell_r * t;
    // Gaussian ring width: half-power at ±0.4 cells from the ring edge.
    let ring_w = 0.4 * cell_r;
    let delta = d - ring_radius;
    let ring = (1.0 - t) * exp(-(delta * delta) / (ring_w * ring_w));
    // Combined additive contribution, clamped so intensity spikes don't oversaturate.
    let glow = clamp(halo + ring, 0.0, 1.0);
    // Alpha = 0: additive RGB only. The destination alpha is untouched, so the
    // window's premultiplied transparency (and rounded-corner mask) is preserved:
    // a corner pixel with alpha=0 remains alpha=0 after this additive pass.
    return vec4<f32>(p.color.rgb * p.intensity * glow, 0.0);
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
    /// Glow colour: rgb in [0..1], [3] is padding. (offset 32)
    pub color: [f32; 4],
}

/// Caret glow/ripple GPU post-pass. Draws a soft additive halo + expanding
/// ring around the cursor cell on each keystroke burst. Build once at startup;
/// dispatch on each frame where glow is enabled and `caret_anim.is_some()`.
pub struct CaretFx {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl CaretFx {
    /// Build the pipeline. Called once in `resumed()` alongside the other
    /// fullscreen-pass constructors. `format` must match the render target.
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

        // Additive blend: src=One, dst=One. Only ever brightens the destination;
        // never darkens or re-opaques pixels (alpha channel output is 0 so the
        // destination alpha is not changed either).
        let additive = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("caret-fx-pipeline"),
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
                    blend: Some(wgpu::BlendState { color: additive, alpha: additive }),
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

    /// Draw the caret glow/ripple additively onto `dst`.
    ///
    /// Uses `LoadOp::Load` (composites with the existing frame content) and
    /// additive blending. Call only when the glow effect is enabled AND
    /// `caret_anim.is_some()` AND the cursor is visible.
    ///
    /// Compositing targets (caller must pick the right `dst`):
    /// - CRT ON:    `scene_view` (the offscreen) — the CRT pass then processes
    ///              and rounds corners. Glow gets full CRT treatment.
    /// - CRT OFF:   `scene_view` (== surface view) — corner mask already ran,
    ///              alpha=0 output keeps corners transparent.
    /// - Tier-B:    `scene_view` (== offscreen) — the Tier-B effect resamples
    ///              it; glow is displaced/blurred like the rest of the scene.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst: &wgpu::TextureView,
        u: &CaretFxUniform,
    ) {
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(u));

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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
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
