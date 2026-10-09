use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// A platform display connection wgpu can hold: `HasDisplayHandle` + `Debug` +
/// `Send` + `Sync` + `'static` (winit's `OwnedDisplayHandle` is one).
pub use wgpu::wgt::WgpuHasDisplayHandle;

/// The windowing system's display connection — the X11 `Display*`, the Wayland
/// `wl_display` — that every JeTTY window lives on. See [`set_platform_display`].
static PLATFORM_DISPLAY: OnceLock<Arc<dyn WgpuHasDisplayHandle>> = OnceLock::new();

/// Register the windowing system's display connection (winit's
/// `OwnedDisplayHandle`) BEFORE the first window acquires the GPU. wgpu's GL
/// backend binds EGL to it when the instance is created; without it EGL falls back
/// to its surfaceless platform, whose adapter can present to no window — so a
/// machine with no Vulkan driver (a VM, an old GPU, Mesa llvmpipe only) mapped a
/// window that was never painted. One per process, like winit's event loop: later
/// calls are ignored. Vulkan and Metal never read it.
pub fn set_platform_display(display: impl WgpuHasDisplayHandle) {
    let _ = PLATFORM_DISPLAY.set(Arc::new(display));
}

/// What JeTTY tells the user when no GPU path at all (no Vulkan, no OpenGL
/// through EGL) can draw its window — it then exits instead of sitting invisible.
pub const NO_GPU_HELP: &str = "jetty: no graphics driver can draw the window — JeTTY needs Vulkan, \
or OpenGL 3.3 / OpenGL ES 3.0 through EGL. Install your GPU's Vulkan driver or Mesa's \
EGL/OpenGL drivers, then start JeTTY again.";

/// The descriptor of an instance [`GpuContext::new`] creates for `backends`. Only
/// an instance with the GL backend carries the platform display
/// ([`set_platform_display`]) — GL presents through it; Vulkan never reads it, so
/// the Vulkan-first instance stays exactly as it was.
fn instance_descriptor(backends: wgpu::Backends) -> wgpu::InstanceDescriptor {
    let display = if backends.contains(wgpu::Backends::GL) {
        PLATFORM_DISPLAY.get().map(|d| Box::new(Arc::clone(d)) as Box<dyn WgpuHasDisplayHandle>)
    } else {
        None
    };
    wgpu::InstanceDescriptor { backends, display, ..wgpu::InstanceDescriptor::new_without_display_handle() }
}

/// The GPU objects every JeTTY window shares: ONE wgpu instance, adapter, device
/// and queue. Acquiring them is the dominant GPU cost (~70–90 ms of adapter
/// enumeration + device creation on the reference machine); a second window —
/// Settings, a detached tab — needs only its own `Surface`, so it reuses these
/// instead of blocking the UI thread on a whole new device (and every window's
/// pipelines/atlases live on the same device).
pub struct GpuShared {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    backend: wgpu::Backend,
    backend_name: String,
    /// The adapter is a CPU (software) rasterizer, e.g. lavapipe/llvmpipe:
    /// every frame costs real CPU, so animations pace slower.
    cpu: bool,
    /// Max 2D texture dimension the device enforces (surface size clamp).
    max_dim: u32,
    /// Set by the device-lost callback on a genuine loss (driver reset, GPU
    /// hang, suspend) — see [`GpuContext::is_lost`].
    lost: Arc<AtomicBool>,
}

/// The surface's composite alpha mode from what it offers, transparency first:
/// PreMultiplied → PostMultiplied → Opaque → Auto.
fn pick_alpha_mode(offered: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    use wgpu::CompositeAlphaMode as M;
    [M::PreMultiplied, M::PostMultiplied, M::Opaque]
        .into_iter()
        .find(|m| offered.contains(m))
        .unwrap_or(M::Auto)
}

/// Whether the frame's color must be premultiplied by its alpha
/// ([`GpuContext::premultiply_clear`]): for a PreMultiplied surface;
/// PostMultiplied (Metal) and a truly Opaque surface want straight color.
///
/// wgpu's GL backend is the exception: it offers only `Opaque` yet never forces
/// alpha to 1, so the frame's alpha reaches the compositor through JeTTY's ARGB
/// window (X11) or ARGB buffer (Wayland) — read as PREMULTIPLIED there, exactly
/// like the Vulkan surface on the same window. A straight frame on GL composited
/// translucent themes brighter than on Vulkan.
fn premultiplied_frame(alpha_mode: wgpu::CompositeAlphaMode, backend: wgpu::Backend) -> bool {
    alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied
        || (cfg!(all(unix, not(target_vendor = "apple"))) && backend == wgpu::Backend::Gl)
}

/// The format of the render targets an effect keeps between its passes — the
/// CRT bloom chain, the aurora backdrop's noise layer: `Rgba16Float`, unless the
/// device cannot render to it. That is OpenGL ES 3.0 without
/// `EXT_color_buffer_(half_)float` (older GPUs): there every frame with CRT on
/// failed validation and the window went black. They fall back to
/// `Rgba8UnormSrgb`, whose sRGB encoding keeps the dark end of a glow free of
/// visible banding; what those targets hold is non-negative and at most ~1, so
/// nothing clips. Vulkan, Metal and DX12 always render to `Rgba16Float`: only a
/// GL device is probed (a 1×1 texture, once per effect it builds).
pub(crate) fn effect_target_format(device: &wgpu::Device) -> wgpu::TextureFormat {
    let backend = device.adapter_info().backend;
    let half_float_renders =
        backend != wgpu::Backend::Gl || renders_to(device, wgpu::TextureFormat::Rgba16Float);
    let format = effect_format(backend, half_float_renders);
    if format != wgpu::TextureFormat::Rgba16Float {
        use std::sync::Once;
        static LOG: Once = Once::new();
        LOG.call_once(|| {
            eprintln!(
                "jetty: this GPU cannot render to half-float textures (OpenGL ES without \
                 EXT_color_buffer_float); CRT bloom and the aurora backdrop use 8-bit buffers"
            );
        });
    }
    format
}

/// [`effect_target_format`]'s decision.
fn effect_format(backend: wgpu::Backend, half_float_renders: bool) -> wgpu::TextureFormat {
    if backend != wgpu::Backend::Gl || half_float_renders {
        wgpu::TextureFormat::Rgba16Float
    } else {
        wgpu::TextureFormat::Rgba8UnormSrgb
    }
}

/// Whether `device` accepts a `format` texture as a render target (wgpu's GL
/// backend decides per driver): a 1×1 probe inside a validation error scope.
fn renders_to(device: &wgpu::Device, format: wgpu::TextureFormat) -> bool {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let probe = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-target-probe"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let failed = pollster::block_on(scope.pop()).is_some();
    probe.destroy();
    !failed
}

/// Log an uncaptured wgpu error — each distinct message once. A pass the driver
/// cannot run fails the same way every frame: on a GLES 3.0 GPU with CRT on that
/// was ~50 identical lines a second into the session log, for as long as JeTTY ran.
fn log_wgpu_error(msg: &str) {
    static SEEN: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut seen) = SEEN.lock() else { return };
    match first_report(&mut seen, msg) {
        Report::New => eprintln!("jetty: wgpu error: {msg}"),
        Report::Last => {
            eprintln!("jetty: wgpu error: {msg}");
            eprintln!("jetty: further distinct wgpu errors are not logged");
        }
        Report::Repeat => {}
    }
}

/// How many distinct wgpu error messages [`log_wgpu_error`] prints at most.
const MAX_LOGGED_WGPU_ERRORS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
enum Report {
    /// Never seen: print it.
    New,
    /// Never seen, and the last one that will be printed.
    Last,
    /// Seen before (or past the cap): stay quiet.
    Repeat,
}

/// [`log_wgpu_error`]'s bookkeeping: `seen` holds the hashes of the messages
/// already printed.
fn first_report(seen: &mut Vec<u64>, msg: &str) -> Report {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    msg.hash(&mut h);
    let key = h.finish();
    if seen.len() >= MAX_LOGGED_WGPU_ERRORS || seen.contains(&key) {
        return Report::Repeat;
    }
    seen.push(key);
    if seen.len() == MAX_LOGGED_WGPU_ERRORS {
        Report::Last
    } else {
        Report::New
    }
}

/// Why [`GpuContext::acquire_frame`] skipped a frame
/// ([`GpuContext::last_acquire_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// The configuration was stale (e.g. after a resize): reconfigured, so the
    /// next acquire should succeed — worth an immediate redraw.
    Outdated,
    /// The surface was lost and a reconfigure did not bring it back.
    Lost,
    /// The presentation engine did not hand out an image in time.
    Timeout,
    /// The window is occluded / minimized: there is nothing to draw into.
    Occluded,
    /// The surface reported a validation error.
    Validation,
}

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
    /// match how the window system composites the surface (true for
    /// PreMultiplied and for GL on X11/Wayland, false for PostMultiplied/Opaque).
    /// See `premultiplied_frame` and `default_bg_clear`.
    pub premultiply_clear: bool,
    /// Max 2D texture dimension the device enforces. Surface `width`/`height` are
    /// clamped to this so `Surface::configure` never fails validation on very large
    /// (multi-monitor) windows.
    max_dim: u32,
    /// The instance/adapter/device/queue this window's surface was created from
    /// (shared with every other window built via [`GpuContext::with_shared`]).
    shared: Arc<GpuShared>,
    /// Why the most recent `acquire_frame` returned `None` (`None` = it succeeded).
    last_acquire_error: Option<AcquireError>,
}

impl GpuContext {
    /// Acquire the GPU for the FIRST window: instance, adapter, device and queue
    /// (shareable afterwards via [`Self::shared`]) plus this window's surface.
    /// `None` (logged) when no adapter can present to the window.
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
        // If no Vulkan adapter is found (no working ICD), fall back to all backends
        // — with the platform display, which the GL backend presents through.
        let make_instance_surface_adapter = |backends: wgpu::Backends|
            -> Result<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter), String> {
            let instance = wgpu::Instance::new(instance_descriptor(backends));
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
                    eprintln!("jetty: GPU init failed ({e})");
                    return None;
                }
            },
        };
        // Log the adapter ONCE per process (a window that cannot share the device
        // falls back to this path again — no need to reprint).
        use std::sync::Once;
        static LOG_ADAPTER: Once = Once::new();
        LOG_ADAPTER.call_once(|| {
            let info = adapter.get_info();
            // The driver string — on GL the context version ("OpenGL ES 3.2 Mesa
            // …"), what a GL-only machine's bug report needs most.
            let driver = if info.driver_info.is_empty() {
                String::new()
            } else {
                format!(", {}", info.driver_info)
            };
            eprintln!("jetty: GPU adapter = {} ({:?}{driver})", info.name, info.backend);
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
                eprintln!("jetty: GPU init failed (device: {e})");
                return None;
            }
        };

        // wgpu routes Surface::configure (and other) validation failures to the
        // device error sink, which panics by default. A mismatched adapter — e.g. an
        // iGPU that cannot present to a dGPU-driven compositor (see the JETTY_GPU
        // note above) — would abort the process here instead of degrading. Install a
        // non-fatal handler so such failures log and the app keeps running (with no
        // rendering) rather than crashing.
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log_wgpu_error(&e.to_string())));
        // A genuine device loss (driver reset / GPU hang / suspend) leaves every
        // window frozen on its last frame: flag it so the app can rebuild its GPU
        // stack (`is_lost`). `Destroyed` is our own teardown — not a loss.
        let lost = Arc::new(AtomicBool::new(false));
        let lost_flag = Arc::clone(&lost);
        device.set_device_lost_callback(move |reason, msg| {
            if reason == wgpu::DeviceLostReason::Unknown {
                lost_flag.store(true, Ordering::Release);
                eprintln!("jetty: GPU device lost ({msg}); rendering stops until it is rebuilt");
            }
        });

        let shared = Arc::new(GpuShared {
            backend: adapter.get_info().backend,
            backend_name: format!("{:?}", adapter.get_info().backend),
            cpu: adapter.get_info().device_type == wgpu::DeviceType::Cpu,
            max_dim: device.limits().max_texture_dimension_2d,
            instance,
            adapter,
            device,
            queue,
            lost,
        });
        Some(Self::configure(shared, surface, width, height))
    }

    /// A further window on an existing GPU: only its `Surface` is created and
    /// configured — no adapter enumeration, no device creation. `None` when the
    /// surface cannot be created or the shared adapter cannot present to it
    /// (e.g. a window on a screen driven by another GPU); see [`Self::new_sharing`].
    pub fn with_shared<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static>(
        shared: &Arc<GpuShared>,
        window: Arc<W>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        if shared.lost.load(Ordering::Acquire) {
            return None;
        }
        let surface = match shared.instance.create_surface(window) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("jetty: surface creation on the shared GPU failed ({e})");
                return None;
            }
        };
        if !shared.adapter.is_surface_supported(&surface) {
            eprintln!("jetty: the shared GPU cannot present to this window; acquiring another");
            return None;
        }
        Some(Self::configure(Arc::clone(shared), surface, width, height))
    }

    /// [`Self::with_shared`] when a shared GPU is available and can present to
    /// the window, else a full [`Self::new`] (its own adapter + device). What every
    /// window after the first uses.
    pub fn new_sharing<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static>(
        shared: Option<&Arc<GpuShared>>,
        window: Arc<W>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        if let Some(s) = shared {
            if let Some(g) = Self::with_shared(s, window.clone(), width, height) {
                return Some(g);
            }
        }
        Self::new(window, width, height)
    }

    /// Pick this surface's format + alpha mode from ITS capabilities and configure
    /// it on the shared device.
    fn configure(shared: Arc<GpuShared>, surface: wgpu::Surface<'static>, width: u32, height: u32) -> Self {
        let caps = surface.get_capabilities(&shared.adapter);
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
        let alpha_mode = pick_alpha_mode(&caps.alpha_modes);
        let premultiply_clear = premultiplied_frame(alpha_mode, shared.backend);

        let max_dim = shared.max_dim;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.clamp(1, max_dim),
            height: height.clamp(1, max_dim),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            // ONE frame in flight (Vulkan: a 2-image swapchain; Metal: 2 drawables).
            // wgpu: "Choose 1 to minimize latency above all else … For applications
            // like GUIs doing a small amount of GPU work each frame that need low
            // latency, this is a reasonable choice." A terminal frame is well under
            // a millisecond of GPU work, and with 2 a continuously animating frame
            // (caret flash while typing) kept a second frame queued AHEAD of the
            // keystroke's echo — up to one extra refresh of input latency.
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&shared.device, &config);

        Self {
            surface,
            device: shared.device.clone(),
            queue: shared.queue.clone(),
            config,
            format,
            backend_name: shared.backend_name.clone(),
            premultiply_clear,
            max_dim,
            shared,
            last_acquire_error: None,
        }
    }

    /// The shared GPU (instance/adapter/device/queue) — pass to
    /// [`Self::with_shared`] / [`Self::new_sharing`] for every further window.
    pub fn shared(&self) -> Arc<GpuShared> {
        Arc::clone(&self.shared)
    }

    /// Whether the adapter is a CPU (software) rasterizer — lavapipe/llvmpipe,
    /// WARP — from its reported device type: every frame then costs real CPU,
    /// so continuous effect animations pace at a lower rate.
    pub fn is_cpu_adapter(&self) -> bool {
        self.shared.cpu
    }

    /// True once the shared device has been LOST (driver reset, GPU hang,
    /// suspend/resume on some drivers). Nothing rendered on it will ever reach the
    /// screen again: the caller must rebuild its whole GPU stack (`GpuContext` +
    /// every layer) from a fresh [`Self::new`].
    pub fn is_lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }

    /// Why the most recent [`Self::acquire_frame`] returned `None` (`None` when it
    /// succeeded). `Outdated` / `Timeout` / a recovered `Lost` deserve an immediate
    /// retry; `Occluded` should wait for the window to become visible again.
    pub fn last_acquire_error(&self) -> Option<AcquireError> {
        self.last_acquire_error
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
    /// (surface was reconfigured, occluded, or timed out) — the reason is kept in
    /// [`Self::last_acquire_error`] so the caller can decide whether to retry.
    pub fn acquire_frame(&mut self) -> Option<(wgpu::SurfaceTexture, wgpu::TextureView)> {
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Stale configuration (e.g. after a resize); reconfigure and skip
                // this frame. The next acquire will use the new config.
                self.surface.configure(&self.device, &self.config);
                self.last_acquire_error = Some(AcquireError::Outdated);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // A genuinely lost surface: reconfigure and retry the acquire
                // once. Reconfiguring is the best safe recovery available here,
                // since full surface recreation would require the window handle,
                // which GpuContext does not retain.
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
                        self.last_acquire_error = Some(AcquireError::Lost);
                        return None;
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                self.last_acquire_error = Some(AcquireError::Occluded);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.last_acquire_error = Some(AcquireError::Timeout);
                return None;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.last_acquire_error = Some(AcquireError::Validation);
                return None;
            }
        };
        self.last_acquire_error = None;
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

#[cfg(test)]
mod tests {
    use super::{instance_descriptor, pick_alpha_mode, premultiplied_frame, set_platform_display};
    use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, XlibDisplayHandle};
    use wgpu::{Backend, Backends, CompositeAlphaMode as M};

    /// Stands in for winit's `OwnedDisplayHandle` (no X connection is opened).
    #[derive(Debug)]
    struct TestDisplay;
    impl HasDisplayHandle for TestDisplay {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            // SAFETY: never dereferenced — no instance is created from it.
            Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0))) })
        }
    }

    /// Without the display, wgpu's GL backend binds EGL's surfaceless platform,
    /// whose adapter can present to no window: on a machine without Vulkan the
    /// window was mapped but never painted. The Vulkan-first instance stays
    /// without it (Vulkan never reads it).
    #[test]
    fn the_gl_capable_instance_carries_the_platform_display() {
        set_platform_display(TestDisplay);
        assert!(instance_descriptor(Backends::VULKAN).display.is_none(), "the Vulkan path is unchanged");
        assert!(instance_descriptor(Backends::all()).display.is_some(), "GL presents through the display");
        assert!(instance_descriptor(Backends::GL).display.is_some());
    }

    /// OpenGL ES 3.0 without EXT_color_buffer_float cannot render to
    /// `Rgba16Float`: the CRT bloom chain and the aurora's noise layer go 8-bit
    /// sRGB there instead of failing every frame (a black window with CRT on).
    #[test]
    fn effect_targets_fall_back_to_8_bit_only_where_half_float_cannot_render() {
        use super::effect_format;
        use wgpu::TextureFormat::{Rgba16Float, Rgba8UnormSrgb};
        assert_eq!(effect_format(Backend::Vulkan, true), Rgba16Float);
        assert_eq!(effect_format(Backend::Metal, true), Rgba16Float);
        assert_eq!(effect_format(Backend::Gl, true), Rgba16Float, "a capable GL driver keeps half-float");
        assert_eq!(effect_format(Backend::Gl, false), Rgba8UnormSrgb);
        // Only GL is ever probed; any other backend renders half-float.
        assert_eq!(effect_format(Backend::Vulkan, false), Rgba16Float);
    }

    /// An error that recurs every frame is printed once, not ~50 times a second.
    #[test]
    fn a_recurring_wgpu_error_is_reported_once() {
        use super::{first_report, Report, MAX_LOGGED_WGPU_ERRORS};
        let mut seen = Vec::new();
        assert_eq!(first_report(&mut seen, "Format Rgba16Float is not renderable"), Report::New);
        assert_eq!(first_report(&mut seen, "Format Rgba16Float is not renderable"), Report::Repeat);
        assert_eq!(first_report(&mut seen, "TextureView is invalid"), Report::New);
        // Bounded: the last one printed says so; past the cap all stay quiet.
        for i in 2..MAX_LOGGED_WGPU_ERRORS - 1 {
            assert_eq!(first_report(&mut seen, &format!("error {i}")), Report::New);
        }
        assert_eq!(first_report(&mut seen, "the last one"), Report::Last);
        assert_eq!(first_report(&mut seen, "past the cap"), Report::Repeat);
        assert_eq!(seen.len(), MAX_LOGGED_WGPU_ERRORS);
    }

    #[test]
    fn the_alpha_mode_prefers_transparency() {
        // Vulkan on X11/Wayland, Metal, wgpu-GL, an adapter that offers nothing.
        assert_eq!(pick_alpha_mode(&[M::Opaque, M::PreMultiplied, M::Inherit]), M::PreMultiplied);
        assert_eq!(pick_alpha_mode(&[M::Opaque, M::PostMultiplied]), M::PostMultiplied);
        assert_eq!(pick_alpha_mode(&[M::Opaque]), M::Opaque);
        assert_eq!(pick_alpha_mode(&[]), M::Auto);
    }

    #[test]
    fn the_frame_is_premultiplied_where_the_window_system_reads_it_so() {
        assert!(premultiplied_frame(M::PreMultiplied, Backend::Vulkan));
        // Metal composites straight color itself.
        assert!(!premultiplied_frame(M::PostMultiplied, Backend::Metal));
        // A truly opaque Vulkan surface ignores alpha: straight color, no darkening.
        assert!(!premultiplied_frame(M::Opaque, Backend::Vulkan));
    }

    /// wgpu's GL backend offers only `Opaque` but never forces alpha to 1: the
    /// framebuffer's alpha reaches the compositor through JeTTY's ARGB window
    /// (X11) / ARGB buffer (Wayland), whose convention is premultiplied. A
    /// straight frame there composited translucent themes brighter than Vulkan
    /// did (measured: opacity 0.5 wrote 30,30,46 @127 on GL vs 19,19,31 @127 on
    /// Vulkan into the same window).
    #[test]
    #[cfg(all(unix, not(target_vendor = "apple")))]
    fn gl_frames_are_premultiplied_for_the_compositor() {
        assert!(premultiplied_frame(M::Opaque, Backend::Gl));
    }
}
