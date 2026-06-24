use std::sync::Arc;

pub struct GpuContext {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub format: wgpu::TextureFormat,
}

impl GpuContext {
    pub fn new<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static>(
        window: Arc<W>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        // Try a Vulkan-only instance first: this skips the GLES libEGL dlopen /
        // eglInitialize + GL adapter enumeration that Backends::all() pays on every
        // cold start, even though the Vulkan adapter is what gets selected anyway.
        // This is the dominant cold-start win (~78ms off gpu_init on the Intel Arc).
        // If no Vulkan adapter is found (no working ICD), fall back to all backends.
        let make_instance_surface_adapter = |backends: wgpu::Backends|
            -> Option<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter)> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let surface = instance.create_surface(window.clone()).ok()?;
            // Prefer the integrated (Intel) GPU. On hybrid Intel+NVIDIA systems
            // under X11, driving the discrete NVIDIA GPU via Vulkan can crash the
            // KWin/X compositor — and a terminal has no need for discrete power.
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })).ok()?;
            Some((instance, surface, adapter))
        };
        let (instance, surface, adapter) = match make_instance_surface_adapter(wgpu::Backends::VULKAN) {
            Some(t) => t,
            None => match make_instance_surface_adapter(wgpu::Backends::all()) {
                Some(t) => t,
                None => {
                    eprintln!("jetty: GPU init failed (no adapter); running without rendering");
                    return None;
                }
            },
        };
        let _ = &instance;
        eprintln!(
            "jetty: GPU adapter = {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );
        let (device, queue) = match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jetty-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            ..Default::default()
        })) {
            Ok(dq) => dq,
            Err(e) => {
                eprintln!("jetty: GPU init failed (device: {e}); running without rendering");
                return None;
            }
        };

        let caps = surface.get_capabilities(&adapter);
        // Prefer an sRGB format; if the driver reports no formats at all (e.g. an
        // incompatible surface returns an empty list), fall back to a sane default
        // rather than panicking on `formats[0]`.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);

        // Prefer an alpha-capable composite mode for window transparency.
        // PreMultiplied is most widely supported on Wayland/macOS; fall back to
        // PostMultiplied, then whatever the driver reports first, then Auto.
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Some(Self { surface, device, queue, config, format })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Acquire the next frame from the swap chain, handling all surface-lost/outdated cases.
    /// Returns `Some((texture, view))` on success, or `None` if the frame should be skipped
    /// (surface was reconfigured, occluded, or timed out).
    pub fn acquire_frame(&mut self) -> Option<(wgpu::SurfaceTexture, wgpu::TextureView)> {
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Stale configuration (e.g. after a resize); reconfigure and skip
                // this frame. The next acquire will use the new config.
                self.surface.configure(&self.device, &self.config);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // A genuinely lost surface: reconfigure and retry the acquire
                // once. Reconfiguring is the best safe recovery available here,
                // since full surface recreation would require the window handle,
                // which GpuContext does not retain (DEFERRED — see below).
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    other => {
                        // Reconfigure did not recover the surface. Log rather than
                        // spinning silently so the failure is observable.
                        eprintln!(
                            "jetty: surface lost and reconfigure did not recover it ({other:?}); \
                             skipping frame (surface recreation not yet supported)"
                        );
                        return None;
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Validation => return None,
        };
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some((texture, view))
    }

    pub fn clear(&mut self, rgba: [f64; 4]) -> Result<(), String> {
        let Some((frame, view)) = self.acquire_frame() else {
            return Ok(());
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("clear") });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: rgba[0],
                            g: rgba[1],
                            b: rgba[2],
                            a: rgba[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}
