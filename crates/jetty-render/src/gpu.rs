use std::sync::Arc;

pub struct GpuContext {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub format: wgpu::TextureFormat,
    /// Human-readable wgpu backend name captured at adapter selection, e.g.
    /// "Vulkan", "Metal", "Gl". Used by the Welcome overlay "Render" row.
    pub backend_name: String,
    /// Whether the frame clear should premultiply the theme bg by its alpha, to
    /// match the chosen surface `alpha_mode` (true for PreMultiplied, false for
    /// PostMultiplied/Opaque). See `default_bg_clear`.
    pub premultiply_clear: bool,
    /// Max 2D texture dimension the device enforces. Surface `width`/`height` are
    /// clamped to this so `Surface::configure` never fails validation on very large
    /// (multi-monitor) windows.
    max_dim: u32,
}

impl GpuContext {
    pub fn new<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static>(
        window: Arc<W>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        // GPU power preference. Default LowPower → the integrated GPU: a terminal
        // needs no discrete power, and on some hybrid setups driving the dGPU via
        // Vulkan can destabilize the compositor. BUT on machines where the
        // compositor/display is driven by the DISCRETE GPU (e.g. an NVIDIA-primary
        // laptop), the integrated adapter cannot present to the compositor's
        // surface — `Surface::configure` fails ("does not support the adapter's
        // queue family") with dmabuf-import errors. Set JETTY_GPU=high (aliases
        // `discrete`, `dgpu`) to select HighPerformance → the discrete GPU, which
        // fixes presentation on those systems. (`JETTY_BENCH_GPU` is the headless
        // analogue used only by jetty-bench.)
        let power = match std::env::var("JETTY_GPU").as_deref() {
            Ok("high") | Ok("discrete") | Ok("dgpu") => wgpu::PowerPreference::HighPerformance,
            _ => wgpu::PowerPreference::LowPower,
        };
        // Try a Vulkan-only instance first: this skips the GLES libEGL dlopen /
        // eglInitialize + GL adapter enumeration that Backends::all() pays on every
        // cold start, even though the Vulkan adapter is what gets selected anyway.
        // This is the dominant cold-start win (~78ms off gpu_init on the Intel Arc).
        // If no Vulkan adapter is found (no working ICD), fall back to all backends.
        let make_instance_surface_adapter = |backends: wgpu::Backends|
            -> Result<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter), String> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let surface = instance
                .create_surface(window.clone())
                .map_err(|e| format!("surface creation failed: {e}"))?;
            // Default: prefer the integrated GPU (LowPower) — a terminal needs no
            // discrete power, and on hybrid X11 setups driving the dGPU via Vulkan
            // can crash the compositor. JETTY_GPU=high overrides to the discrete
            // GPU for dGPU-primary systems (see `power` above).
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })).map_err(|e| format!("no compatible adapter: {e}"))?;
            Ok((instance, surface, adapter))
        };
        // Vulkan is tried first; on non-Vulkan systems its failure is expected, so
        // only the all-backends fallback's error is surfaced. That error names the
        // step that actually failed (surface creation vs adapter request) instead of
        // always reporting "no adapter".
        let (instance, surface, adapter) = match make_instance_surface_adapter(wgpu::Backends::VULKAN) {
            Ok(t) => t,
            Err(_) => match make_instance_surface_adapter(wgpu::Backends::all()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("jetty: GPU init failed ({e}); running without rendering");
                    return None;
                }
            },
        };
        let _ = &instance;
        // Log the adapter ONCE per process (the settings window builds its own
        // GpuContext each time it opens — no need to reprint on every open).
        use std::sync::Once;
        static LOG_ADAPTER: Once = Once::new();
        LOG_ADAPTER.call_once(|| {
            eprintln!(
                "jetty: GPU adapter = {} ({:?})",
                adapter.get_info().name,
                adapter.get_info().backend
            );
        });
        let (device, queue) = match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jetty-device"),
            required_features: wgpu::Features::empty(),
            // Request exactly what the adapter reports: `Limits::default()` makes
            // `request_device` fail on downlevel adapters (GL/GLES fallback,
            // Raspberry Pi V3D) whose limits sit below the defaults, and also caps
            // max_texture_dimension_2d at 8192 on capable GPUs. The renderer needs
            // nothing above the adapter's own limits.
            required_limits: adapter.limits(),
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

        // wgpu routes Surface::configure (and other) validation failures to the
        // device error sink, which panics by default. A mismatched adapter — e.g. an
        // iGPU that cannot present to a dGPU-driven compositor (see the JETTY_GPU
        // note above) — would abort the process here instead of degrading. Install a
        // non-fatal handler so such failures log and the app keeps running (with no
        // rendering) rather than crashing.
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
            eprintln!("jetty: wgpu error: {e}");
        }));

        // Surface width/height must not exceed the device's max 2D texture
        // dimension, or `Surface::configure` fails validation.
        let max_dim = device.limits().max_texture_dimension_2d;

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

        // Pick a transparency-capable composite mode from what the SURFACE offers
        // (capability-driven — NOT OS-gated). PreMultiplied and PostMultiplied want
        // DIFFERENT framebuffer conventions:
        //   • PreMultiplied  → fb.rgb already multiplied by alpha; compositor does
        //                       fb + (1-fb.a)*dst.            (Vulkan/Wayland path)
        //   • PostMultiplied → fb.rgb is STRAIGHT; compositor does fb.rgb*fb.a +
        //                       (1-fb.a)*dst.                 (Metal/macOS path —
        //                       Metal surfaces expose Opaque + PostMultiplied but
        //                       NOT PreMultiplied, so without this a Mac falls to
        //                       Opaque and the window is never see-through.)
        // We record which we picked and let the frame clear match it via
        // `premultiply_clear`: premultiply the bg ONLY for PreMultiplied. Feeding a
        // premultiplied clear to PostMultiplied is what made transparent themes
        // "too dark" before — fixed by using a straight clear in that mode.
        // Order: PreMultiplied → PostMultiplied → Opaque → Auto.
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            wgpu::CompositeAlphaMode::Auto
        };
        // The frame clear premultiplies the theme bg by its alpha ONLY for a
        // PreMultiplied surface; PostMultiplied/Opaque want straight rgb.
        let premultiply_clear = alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.clamp(1, max_dim),
            height: height.clamp(1, max_dim),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // Capture the backend name for display in the Welcome overlay.
        let backend_name = format!("{:?}", adapter.get_info().backend);

        Some(Self { surface, device, queue, config, format, backend_name, premultiply_clear, max_dim })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w.min(self.max_dim);
            self.config.height = h.min(self.max_dim);
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
                        // Reconfigure did not recover the surface. Log only once per
                        // process: the caret animation drives continuous redraws, so
                        // an every-frame log would flood stderr/journald without bound
                        // while the surface stays lost.
                        use std::sync::Once;
                        static LOST_ONCE: Once = Once::new();
                        LOST_ONCE.call_once(|| {
                            eprintln!(
                                "jetty: surface lost and reconfigure did not recover it ({other:?}); \
                                 skipping frames (surface recreation not yet supported)"
                            );
                        });
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
