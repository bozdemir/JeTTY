//! Inline-image GPU layer — a per-window textured-quad pipeline that draws
//! decoded sixel images (from `jetty_core`) over the terminal grid.
//!
//! Modeled on `QuadLayer` (a persistent, grown-on-demand instance buffer) crossed
//! with `Crt` (a texture + sampler bind group). Each visible image is a single
//! textured quad drawn at its NATIVE pixel size at the placement anchor; images
//! are uploaded ONCE and cached under a VRAM byte budget with frame-counter LRU.
//!
//! PER-WINDOW: wgpu textures / views / bind groups / pipelines are device-scoped,
//! and detached windows own their own device. So the main `App` holds one
//! `ImageLayer` on the main device and each `DetachedWindow` holds its own — the
//! decoded RGBA lives device-independent in `jetty_core`; each layer uploads it to
//! ITS device on demand (exactly like `dw.crt`).
//!
//! Color: image textures are `Rgba8UnormSrgb`, so the sampler auto-linearizes and
//! the sRGB render target auto-encodes — the decoded sixel RGBA (sRGB) needs no
//! manual gamma. The decoder emits all-or-nothing alpha (opaque or fully
//! transparent), which is already premultiplied, so a PREMULTIPLIED blend +
//! clamp-to-edge linear sampling antialiases transparent edges without black
//! fringing.
//!
//! Zero cost when no image is visible: `render` early-returns on an empty draw
//! list (after reclaiming any VRAM whose images just left view).
//!
//! Self-contained: our own wgpu/WGSL, no desktop-environment / OS-specific code.

use std::collections::HashMap;

pub(crate) const IMAGE_SHADER: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
};

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,   // dst x, y, w, h (physical px)
    @location(1) opacity: f32,
) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[vi];
    let px = rect.xy + c * rect.zw;
    let ndc = vec2(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.pos = vec4(ndc, 0.0, 1.0);
    o.uv = c;                       // 0..1 across the native image
    o.opacity = opacity;
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Rgba8UnormSrgb sample → linear; multiply the whole (premultiplied) texel
    // by opacity so a transparent theme / fade keeps premultiplication valid.
    let c = textureSampleLevel(tex, samp, in.uv, 0.0);
    return c * in.opacity;
}
"#;

/// One image to draw this frame. Built app-side from a `VisibleImage`
/// (`dst` = native px rect at the anchor). `rgba`/`w`/`h` are used only for a
/// FIRST-SIGHT upload — on a cache hit they are ignored (no re-copy).
pub struct ImageDraw<'a> {
    pub id: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: &'a [u8],
    /// Destination rect `[x, y, w, h]` in physical pixels (native image size).
    pub dst: [f32; 4],
    /// 0..1 (usually 1.0). Multiplies the premultiplied texel.
    pub opacity: f32,
}

/// A cached GPU texture for one image id.
struct Cached {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    #[allow(dead_code)]
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    w: u32,
    h: u32,
    bytes: u64,
    last_frame: u64,
}

/// VRAM budget for cached image textures (per window). LRU-evicted past this.
const IMAGE_VRAM_BUDGET: u64 = 256 * 1024 * 1024;

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    screen_bg: wgpu::BindGroup,
    tex_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instance_buf: Option<wgpu::Buffer>,
    instance_cap: u64,
    instance_scratch: Vec<f32>,
    textures: HashMap<u64, Cached>,
    vram_bytes: u64,
    frame: u64,
    /// The device's real max 2D texture dimension; a decoded image larger than
    /// this is skipped (drawn as nothing) rather than handed to wgpu.
    max_dim: u32,
}

impl ImageLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image-shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image-uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let screen_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image-screen-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let screen_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-screen-bg"),
            layout: &screen_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        // Group 1: the per-image texture + sampler (the `Crt` pattern).
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image-layout"),
            bind_group_layouts: &[Some(&screen_bgl), Some(&tex_bgl)],
            ..Default::default()
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 32, // [rect(4)=16][opacity(1)+pad(3)=16]
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: 16,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The decoder emits premultiplied (all-or-nothing) alpha, so a
                    // premultiplied blend antialiases edges without black fringing.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            screen_bg,
            tex_bgl,
            sampler,
            instance_buf: None,
            instance_cap: 0,
            instance_scratch: Vec::new(),
            textures: HashMap::new(),
            vram_bytes: 0,
            frame: 0,
            max_dim: device.limits().max_texture_dimension_2d,
        }
    }

    /// Upload an image to this device if not already cached (or the cached entry
    /// has a different size). Stamps `last_frame` so eviction never drops it this
    /// frame. Returns false (draws nothing) if the image exceeds the device's max
    /// texture size or the RGBA length is inconsistent.
    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u64, w: u32, h: u32, rgba: &[u8]) -> bool {
        if w == 0 || h == 0 || w > self.max_dim || h > self.max_dim {
            return false;
        }
        if (w as usize) * (h as usize) * 4 != rgba.len() {
            return false; // defensive: never hand wgpu a short buffer
        }
        if let Some(c) = self.textures.get_mut(&id) {
            if c.w == w && c.h == h {
                c.last_frame = self.frame;
                return true;
            }
            // Same id, different size (astronomically unlikely with the folded
            // hash): drop the stale entry and re-upload below.
            self.vram_bytes = self.vram_bytes.saturating_sub(c.bytes);
            self.textures.remove(&id);
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image-texture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // Tight rows; write_texture (unlike a buffer copy) needs no
                // 256-byte alignment.
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-tex-bg"),
            layout: &self.tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let bytes = (w as u64) * (h as u64) * 4;
        self.vram_bytes += bytes;
        self.textures.insert(
            id,
            Cached { texture, view, bind_group, w, h, bytes, last_frame: self.frame },
        );
        true
    }

    /// Draw the visible images into `view` (`LoadOp::Load`), clipped to `scissor`
    /// (`[x, y, w, h]`, physical px, already clamped to the attachment by the
    /// caller). Each image draws at its native size at `dst`.
    ///
    /// The frame counter is advanced ONCE here BEFORE any upload, so a texture
    /// touched this frame is never evicted this frame (no re-upload thrash).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        draws: &[ImageDraw],
        scissor: [u32; 4],
    ) {
        // No images this frame: reclaim any VRAM whose images just left view
        // (event-driven, once), then nothing more. When nothing is cached this is
        // effectively free — the no-image hot path.
        if draws.is_empty() {
            if !self.textures.is_empty() {
                self.textures.clear();
                self.vram_bytes = 0;
            }
            return;
        }
        // Degenerate scissor (fully clipped, e.g. mid dropdown-slide): draw
        // nothing this frame, but still let the cache persist.
        let [sx, sy, sw, sh] = scissor;
        if sw == 0 || sh == 0 {
            return;
        }

        // Advance the frame BEFORE ensure() so this frame's textures stamp the
        // new frame and survive eviction (amendment R4).
        self.frame = self.frame.wrapping_add(1);

        // First-sight upload for each draw; collect which have a live texture.
        for d in draws {
            self.ensure(device, queue, d.id, d.w, d.h, d.rgba);
        }

        // Pack the per-instance rects (+ opacity) into the persistent buffer.
        self.instance_scratch.clear();
        self.instance_scratch.reserve(draws.len() * 8);
        for d in draws {
            self.instance_scratch.extend_from_slice(&d.dst);
            self.instance_scratch.push(d.opacity);
            self.instance_scratch.push(0.0);
            self.instance_scratch.push(0.0);
            self.instance_scratch.push(0.0);
        }
        let bytes = bytemuck::cast_slice::<f32, u8>(&self.instance_scratch);
        let needed = bytes.len() as u64;
        if self.instance_buf.is_none() || self.instance_cap < needed {
            let new_cap = needed.max(self.instance_cap * 2).max(256);
            self.instance_buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("image-instances"),
                size: new_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.instance_cap = new_cap;
        }
        queue.write_buffer(self.instance_buf.as_ref().unwrap(), 0, bytes);

        let uniform: [f32; 4] = [screen_w as f32, screen_h as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("image-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("image-pass"),
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
            pass.set_scissor_rect(sx, sy, sw, sh);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.screen_bg, &[]);
            let buf = self.instance_buf.as_ref().unwrap();
            pass.set_vertex_buffer(0, buf.slice(..));
            for (i, d) in draws.iter().enumerate() {
                // Absent (over-size / failed upload) ⇒ draw nothing for it.
                let Some(c) = self.textures.get(&d.id) else { continue };
                pass.set_bind_group(1, &c.bind_group, &[]);
                pass.draw(0..6, i as u32..i as u32 + 1);
            }
        }
        queue.submit(Some(encoder.finish()));

        self.evict_over_budget();
    }

    /// Evict least-recently-used textures NOT touched this frame until under the
    /// VRAM budget. Never touches this frame's visible set (`last_frame == frame`).
    fn evict_over_budget(&mut self) {
        if self.vram_bytes <= IMAGE_VRAM_BUDGET {
            return;
        }
        // Collect evictable ids (older than this frame), oldest first.
        let mut candidates: Vec<(u64, u64)> = self
            .textures
            .iter()
            .filter(|(_, c)| c.last_frame < self.frame)
            .map(|(&id, c)| (c.last_frame, id))
            .collect();
        candidates.sort_unstable();
        for (_, id) in candidates {
            if self.vram_bytes <= IMAGE_VRAM_BUDGET {
                break;
            }
            if let Some(c) = self.textures.remove(&id) {
                self.vram_bytes = self.vram_bytes.saturating_sub(c.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image WGSL must parse and pass naga validation (the always-run gate,
    /// like `crt_shader_compiles`).
    #[test]
    fn image_shader_compiles() {
        let module = naga::front::wgsl::parse_str(IMAGE_SHADER)
            .expect("IMAGE_SHADER must parse as valid WGSL");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("IMAGE_SHADER must pass naga validation");
    }

    /// GPU smoke test: build the layer, upload one texture, draw one image.
    /// `#[ignore]` because a GPU adapter may be unavailable in CI (like
    /// `crt_new_with_device`). Run: `cargo test -p jetty-render image_ -- --ignored`.
    #[test]
    #[ignore]
    fn image_layer_uploads_and_draws() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("device");
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut layer = ImageLayer::new(&device, format);

        // A tiny 2×2 opaque red image.
        let rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 4).flatten().collect();
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("t"),
            size: wgpu::Extent3d { width: 32, height: 32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let draws = [ImageDraw {
            id: 1,
            w: 2,
            h: 2,
            rgba: &rgba,
            dst: [0.0, 0.0, 2.0, 2.0],
            opacity: 1.0,
        }];
        layer.render(&device, &queue, &view, 32, 32, &draws, [0, 0, 32, 32]);
        assert!(layer.textures.contains_key(&1), "texture cached after draw");
        assert!(layer.vram_bytes > 0);
        // Empty next frame reclaims the VRAM.
        layer.render(&device, &queue, &view, 32, 32, &[], [0, 0, 32, 32]);
        assert!(layer.textures.is_empty(), "VRAM reclaimed when images leave view");
        assert_eq!(layer.vram_bytes, 0);
    }
}
