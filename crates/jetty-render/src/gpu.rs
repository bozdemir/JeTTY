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

/// The descriptor of an instance JeTTY creates for `backends` ([`GpuContext::new`];
/// jetty-shot and jetty-bench use it too, so they measure and draw on the same
/// device setup). Only an instance with the GL backend carries the platform
/// display ([`set_platform_display`]) — GL presents through it; Vulkan never reads
/// it. wgpu's indirect-call validation is off: JeTTY issues no indirect draws or
/// dispatches, and that validation (on by default in release builds) compiled two
/// compute pipelines into every device creation — ~2 ms of cold start on Vulkan
/// (Intel iGPU, lavapipe), ~3.5 ms on GL (llvmpipe).
pub fn instance_descriptor(backends: wgpu::Backends) -> wgpu::InstanceDescriptor {
    let display = if backends.contains(wgpu::Backends::GL) {
        PLATFORM_DISPLAY.get().map(|d| Box::new(Arc::clone(d)) as Box<dyn WgpuHasDisplayHandle>)
    } else {
        None
    };
    let flags = wgpu::InstanceFlags::default() - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL;
    wgpu::InstanceDescriptor { backends, display, flags, ..wgpu::InstanceDescriptor::new_without_display_handle() }
}

/// The backends of the instances [`GpuContext::new`] tries, in order. Unset
/// `WGPU_BACKEND` (`pinned`): Vulkan alone first — its cold start skips EGL —
/// then every backend. wgpu's own `WGPU_BACKEND` (`gl`, `vulkan`, …) pins them:
/// the way around a broken Vulkan driver, and how the GL path runs on any machine.
/// A value naming no backend is ignored.
fn backend_attempts(pinned: Option<wgpu::Backends>) -> [Option<wgpu::Backends>; 2] {
    match pinned.filter(|b| !b.is_empty()) {
        Some(backends) => [Some(backends), None],
        None => [Some(wgpu::Backends::VULKAN), Some(wgpu::Backends::all())],
    }
}

/// The backend's name as users know it — the welcome splash's "Render" row, the
/// startup log, jetty-shot and jetty-bench (wgpu's enum spells GL "Gl").
pub fn backend_display_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Noop => "none",
    }
}

/// GL renderer names of software rasterizers. GL reports no device type: wgpu
/// guesses it from the renderer string and only knows llvmpipe, SwiftShader and
/// "Mesa offscreen" as CPU — softpipe (Mesa without LLVM) or swrast came out
/// "Other", like a real GPU.
const SOFTWARE_GL: [&str; 5] = ["llvmpipe", "softpipe", "swrast", "swiftshader", "mesa offscreen"];

/// Whether `info` is a software (CPU) rasterizer: lavapipe, llvmpipe, softpipe,
/// SwiftShader. Vulkan's device type says so; a GL adapter is also matched by
/// its renderer name ([`SOFTWARE_GL`]).
fn is_software(info: &wgpu::AdapterInfo) -> bool {
    info.device_type == wgpu::DeviceType::Cpu
        || (info.backend == wgpu::Backend::Gl && {
            let name = info.name.to_lowercase();
            SOFTWARE_GL.iter().any(|s| name.contains(s))
        })
}

/// Whether [`GpuContext::new`] looks at GL after its first pick: only when that
/// pick is a software Vulkan adapter — lavapipe on a machine whose GPU has no
/// Vulkan driver (an older GPU, a VM with virgl, WSL's d3d12) — and `WGPU_BACKEND`
/// did not pin the backends. A hardware Vulkan adapter never pays for the probe.
fn wants_gl_probe(first: &wgpu::AdapterInfo, pinned: bool) -> bool {
    !pinned && first.backend == wgpu::Backend::Vulkan && first.device_type == wgpu::DeviceType::Cpu
}

/// A GL adapter as [`prefers_gl`] weighs it: wgpu's info, and whether its driver
/// offers compute shaders (`DownlevelFlags::COMPUTE_SHADERS`).
struct GlCandidate {
    info: wgpu::AdapterInfo,
    compute_shaders: bool,
}

impl GlCandidate {
    fn of(adapter: &wgpu::Adapter) -> Self {
        let flags = adapter.get_downlevel_capabilities().flags;
        Self { info: adapter.get_info(), compute_shaders: flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS) }
    }
}

/// The desktop GL version a GL adapter reports in `driver_info` ("3.3 (Core
/// Profile) Mesa …" → (3, 3), "4.6.0 NVIDIA …" → (4, 6)); `None` for OpenGL ES
/// ("OpenGL ES 3.0 …"), whose compute shaders come with GLSL ES 3.10.
fn desktop_gl_version(driver_info: &str) -> Option<(u32, u32)> {
    let mut parts = driver_info.split_whitespace().next()?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// wgpu 29's GL backend puts the texture bindings into the shader whenever the
/// driver offers compute shaders, but naga writes them only for GLSL 4.20+ /
/// GLSL ES 3.10+. On a desktop GL 3.3–4.1 driver that advertises
/// `GL_ARB_compute_shader` — Mesa's softpipe does — every texture is then
/// sampled from unit 0: no text, a blank CRT frame.
fn gl_textures_misbound(gl: &GlCandidate) -> bool {
    gl.info.backend == wgpu::Backend::Gl
        && gl.compute_shaders
        && desktop_gl_version(&gl.info.driver_info).is_some_and(|v| v < (4, 2))
}

/// Whether the GL adapter `gl` (looked for because `first` is software Vulkan)
/// replaces it: only a hardware one that draws correctly does
/// ([`gl_textures_misbound`]). A software GL renderer, or no GL adapter, keeps
/// the Vulkan one — CPU Vulkan presents fine.
fn prefers_gl(first: &wgpu::AdapterInfo, gl: Option<&GlCandidate>) -> bool {
    is_software(first)
        && gl.is_some_and(|g| {
            g.info.backend == wgpu::Backend::Gl && !is_software(&g.info) && !gl_textures_misbound(g)
        })
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
    /// What every window's layers draw with ([`SharedPipelines`]).
    pipelines: SharedPipelines,
}

/// The pipelines every window on a device draws with — each layer's shader
/// module, layouts and pipeline (wgpu handles, cloned cheaply) — built once per
/// layer type and surface format, on first use. A detached or Settings window
/// then compiles nothing its layers need: naga translates each shader once per
/// device, not once per window. [`GpuShared`] holds the device's
/// ([`GpuContext::pipelines`]).
#[derive(Default)]
pub struct SharedPipelines(std::sync::Mutex<Vec<SharedPipeline>>);

/// One entry of [`SharedPipelines`]: the type, the format it targets, the value.
type SharedPipeline = (std::any::TypeId, wgpu::TextureFormat, Box<dyn std::any::Any + Send + Sync>);

impl SharedPipelines {
    /// The `T` for `format` on `device` — `build` runs on first use.
    pub(crate) fn get<T: Clone + Send + Sync + 'static>(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        build: impl FnOnce(&wgpu::Device, wgpu::TextureFormat) -> T,
    ) -> T {
        self.get_or(format, || build(device, format))
    }

    /// [`Self::get`]'s lookup: the `T` kept for `format`, else `build`'s.
    fn get_or<T: Clone + Send + Sync + 'static>(&self, format: wgpu::TextureFormat, build: impl FnOnce() -> T) -> T {
        let ty = std::any::TypeId::of::<T>();
        let mut built = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let found =
            built.iter().find(|(t, f, _)| *t == ty && *f == format).and_then(|(_, _, v)| v.downcast_ref::<T>());
        if let Some(v) = found {
            return v.clone();
        }
        let v = build();
        built.push((ty, format, Box::new(v.clone())));
        v
    }
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

/// Configure `surface`, catching every error the configure raises — an adapter
/// that cannot present here (an iGPU under a compositor on the dGPU), a window
/// that still has another surface's swapchain (VK_ERROR_NATIVE_WINDOW_IN_USE_KHR),
/// out of memory. A device loss reaches no error scope: the lost flag reports it.
fn configure_checked(
    device: &wgpu::Device,
    surface: &wgpu::Surface<'static>,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), wgpu::Error> {
    let invalid = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    surface.configure(device, config);
    let out_of_memory = pollster::block_on(out_of_memory.pop());
    match pollster::block_on(invalid.pop()).or(out_of_memory) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Why [`GpuContext::acquire_frame`] skipped a frame
/// ([`GpuContext::last_acquire_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// The configuration was stale (e.g. after a resize): reconfigured, so the
    /// next acquire should succeed — worth an immediate redraw.
    Outdated,
    /// The surface was lost and neither a reconfigure nor a new surface brought
    /// it back (or it was released for a rebuild of a lost device).
    Lost,
    /// The presentation engine did not hand out an image in time.
    Timeout,
    /// The window is occluded / minimized: there is nothing to draw into until
    /// it is shown again (no retry needed: the window system says when).
    Occluded,
    /// The surface reported a validation error (e.g. left unconfigured by a
    /// refused configure): configured again for the next acquire.
    Validation,
}

impl AcquireError {
    /// Whether the frame should be retried on a timer. `Occluded` waits for
    /// the window to be shown, which repaints it.
    pub fn wants_retry(self) -> bool {
        self != AcquireError::Occluded
    }
}

pub struct GpuContext {
    /// The window's surface — `None` once released for a rebuild
    /// ([`Self::release_surface`]; every frame acquire is skipped then) or while
    /// a lost one cannot be recreated yet ([`Self::acquire_frame`]).
    surface: Option<wgpu::Surface<'static>>,
    /// The window the surface draws to: a surface that stays lost is recreated
    /// on it.
    window: Arc<dyn wgpu::DisplayAndWindowHandle>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub format: wgpu::TextureFormat,
    /// Human-readable wgpu backend name captured at adapter selection, e.g.
    /// "Vulkan", "OpenGL", "Metal" ([`backend_display_name`]). Used by the Welcome
    /// overlay "Render" row.
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
        let power = if crate::vk_loader::wants_high_performance() {
            wgpu::PowerPreference::HighPerformance
        } else {
            wgpu::PowerPreference::LowPower
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
        // Vulkan is tried first (`backend_attempts`); on non-Vulkan systems its
        // failure is expected, so only the last attempt's error is surfaced. That
        // error names the step that actually failed (surface creation vs adapter
        // request) instead of always reporting "no adapter". The Vulkan-only
        // attempt runs under the startup driver filter when one is installed
        // (`vk_loader`): the loader then initializes only the drivers that can
        // matter, and the attempt is redone unfiltered unless its adapter is
        // provably the unfiltered pick.
        let pinned = wgpu::Backends::from_env().filter(|b| !b.is_empty());
        let mut picked = Err(String::new());
        for backends in backend_attempts(pinned).into_iter().flatten() {
            picked = if backends == wgpu::Backends::VULKAN {
                crate::vk_loader::with_prefilter(
                    || make_instance_surface_adapter(backends),
                    |(instance, _, adapter)| crate::vk_loader::Probe::of(instance, adapter),
                )
            } else {
                make_instance_surface_adapter(backends)
            };
            if picked.is_ok() {
                break;
            }
        }
        // The filter is for the first instance only (`with_prefilter` released it
        // already unless no Vulkan-only attempt ran).
        crate::vk_loader::release();
        // Software Vulkan (lavapipe) on a machine whose GPU has only a GL driver:
        // draw on that GPU through GL instead (`wants_gl_probe`, `prefers_gl`).
        // A hardware Vulkan pick skips this — no EGL, no added cold-start cost.
        if let Ok((_, _, first)) = &picked {
            let first = first.get_info();
            if wants_gl_probe(&first, pinned.is_some()) {
                if let Ok(gl) = make_instance_surface_adapter(wgpu::Backends::GL) {
                    if prefers_gl(&first, Some(&GlCandidate::of(&gl.2))) {
                        eprintln!(
                            "jetty: Vulkan offers only a software renderer here ({}); drawing on the GPU \
                             through OpenGL",
                            first.name
                        );
                        picked = Ok(gl);
                    }
                }
            }
        }
        let (instance, surface, adapter) = match picked {
            Ok(t) => t,
            Err(e) => {
                eprintln!("jetty: GPU init failed ({e})");
                return None;
            }
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
            let backend = backend_display_name(info.backend);
            eprintln!("jetty: GPU adapter = {} ({backend}{driver})", info.name);
            // No other adapter was left to take: say why text may not draw.
            if info.backend == wgpu::Backend::Gl && gl_textures_misbound(&GlCandidate::of(&adapter)) {
                eprintln!(
                    "jetty: this OpenGL driver offers compute shaders without GLSL 4.20; wgpu 29 then \
                     samples every texture from one unit, so text and effects may not draw. A Vulkan \
                     driver (Mesa's lavapipe works) avoids it."
                );
            }
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

        // wgpu routes validation failures to the device error sink, which panics by
        // default: log them instead (each distinct message once) and keep running.
        // The surface configure below is checked on its own (`configure`).
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
            backend_name: backend_display_name(adapter.get_info().backend).to_string(),
            cpu: is_software(&adapter.get_info()),
            max_dim: device.limits().max_texture_dimension_2d,
            instance,
            adapter,
            device,
            queue,
            lost,
            pipelines: Default::default(),
        });
        let gpu = Self::configure(shared, surface, window, width, height);
        if gpu.is_none() && power == wgpu::PowerPreference::LowPower {
            // An iGPU that cannot present to a compositor running on the dGPU
            // (see `power` above).
            eprintln!("jetty: on a laptop whose display runs on the discrete GPU, start JeTTY with JETTY_GPU=high");
        }
        gpu
    }

    /// A further window on an existing GPU: only its `Surface` is created and
    /// configured — no adapter enumeration, no device creation. `None` when the
    /// surface cannot be created, configured, or the shared adapter cannot
    /// present to it (e.g. a window on a screen driven by another GPU); see
    /// [`Self::new_sharing`].
    pub fn with_shared<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static>(
        shared: &Arc<GpuShared>,
        window: Arc<W>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        if shared.lost.load(Ordering::Acquire) {
            return None;
        }
        let surface = match shared.instance.create_surface(window.clone()) {
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
        Self::configure(Arc::clone(shared), surface, window, width, height)
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
    /// it on the shared device. `None` (logged) when the configure fails: wgpu
    /// leaves that surface unconfigured and PANICS on the first frame acquired
    /// from it, so no context is handed out for it — a first window then gets
    /// `NO_GPU_HELP`, a rebuild tries again later.
    fn configure(
        shared: Arc<GpuShared>,
        surface: wgpu::Surface<'static>,
        window: Arc<dyn wgpu::DisplayAndWindowHandle>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
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
        if let Err(e) = configure_checked(&shared.device, &surface, &config) {
            eprintln!("jetty: the GPU cannot draw to this window: {e}");
            return None;
        }
        if shared.lost.load(Ordering::Acquire) {
            eprintln!("jetty: the GPU was lost while setting up this window");
            return None;
        }

        Some(Self {
            surface: Some(surface),
            window,
            device: shared.device.clone(),
            queue: shared.queue.clone(),
            config,
            format,
            backend_name: shared.backend_name.clone(),
            premultiply_clear,
            max_dim,
            shared,
            last_acquire_error: None,
        })
    }

    /// The shared GPU (instance/adapter/device/queue) — pass to
    /// [`Self::with_shared`] / [`Self::new_sharing`] for every further window.
    pub fn shared(&self) -> Arc<GpuShared> {
        Arc::clone(&self.shared)
    }

    /// The pipelines every window on this device shares ([`SharedPipelines`]).
    pub fn pipelines(&self) -> &SharedPipelines {
        &self.shared.pipelines
    }

    /// Whether the adapter is a CPU (software) rasterizer — lavapipe/llvmpipe,
    /// softpipe, WARP — from its device type (and, on GL, its renderer name:
    /// `is_software`): every frame then costs real CPU, so continuous effect
    /// animations pace at a lower rate.
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

    /// Drop the window's surface — and with it the swapchain it holds — ahead of
    /// rebuilding a LOST context: a new surface on the same window may create
    /// its swapchain only once the old one is gone (Vulkan returns
    /// VK_ERROR_NATIVE_WINDOW_IN_USE_KHR on drivers that enforce it; Mesa on
    /// Wayland makes a second fifo object for the surface, a protocol error that
    /// ends the connection). Every later acquire on this context is skipped while
    /// it reports [`Self::is_lost`], so a failed rebuild is retried; on a live
    /// device the next acquire makes a new surface instead (what the nested
    /// harness's `JETTY_DEBUG_LOSE_SURFACE` drives).
    pub fn release_surface(&mut self) {
        self.surface = None;
    }

    /// Test hook for the nested harness (`JETTY_DEBUG_LOSE_GPU`, never a
    /// setting): lose the shared device the way a driver reset does — every call
    /// on it fails from now on and [`Self::is_lost`] turns true — so the rebuild
    /// can be driven without a real GPU fault.
    pub fn debug_lose_device(&self) {
        self.shared.device.destroy();
        self.shared.lost.store(true, Ordering::Release);
    }

    /// Why the most recent [`Self::acquire_frame`] returned `None` (`None` when it
    /// succeeded) — see [`AcquireError::wants_retry`].
    pub fn last_acquire_error(&self) -> Option<AcquireError> {
        self.last_acquire_error
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w.min(self.max_dim);
            self.config.height = h.min(self.max_dim);
            if let Some(surface) = &self.surface {
                surface.configure(&self.device, &self.config);
            }
        }
    }

    /// Acquire the next frame from the swap chain, handling all surface-lost/outdated cases.
    /// Returns `Some((texture, view))` on success, or `None` if the frame should be skipped
    /// (surface was reconfigured, occluded, or timed out) — the reason is kept in
    /// [`Self::last_acquire_error`] so the caller can decide whether to retry.
    pub fn acquire_frame(&mut self) -> Option<(wgpu::SurfaceTexture, wgpu::TextureView)> {
        let texture = match self.acquire() {
            Ok(t) => t,
            Err(e) => {
                self.last_acquire_error = Some(e);
                return None;
            }
        };
        self.last_acquire_error = None;
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some((texture, view))
    }

    /// [`Self::acquire_frame`]'s acquire and its recovery.
    fn acquire(&mut self) -> Result<wgpu::SurfaceTexture, AcquireError> {
        let texture = |s: &wgpu::Surface| match s.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Ok(t),
            other => Err(other),
        };
        // Released for a rebuild of this lost context — or a lost surface that
        // could not be recreated yet: try again (on a live device only).
        if self.surface.is_none() && (self.is_lost() || !self.recreate_surface()) {
            return Err(AcquireError::Lost);
        }
        let Some(surface) = &self.surface else { return Err(AcquireError::Lost) };
        match texture(surface) {
            Ok(t) => Ok(t),
            Err(wgpu::CurrentSurfaceTexture::Outdated) => {
                // Stale configuration (e.g. after a resize); reconfigure and skip
                // this frame. The next acquire will use the new config.
                surface.configure(&self.device, &self.config);
                Err(AcquireError::Outdated)
            }
            Err(wgpu::CurrentSurfaceTexture::Lost) => {
                // A genuinely lost surface: reconfigure and retry the acquire
                // once; when it stays lost, wgpu's documented recovery — a new
                // surface on the window, configured as before — and once more.
                surface.configure(&self.device, &self.config);
                if let Ok(t) = texture(surface) {
                    return Ok(t);
                }
                if self.recreate_surface() {
                    if let Some(t) = self.surface.as_ref().and_then(|s| texture(s).ok()) {
                        return Ok(t);
                    }
                }
                // Logged once per process: the caret animation drives continuous
                // redraws, so an every-frame log would flood stderr/journald
                // while the surface stays lost.
                use std::sync::Once;
                static LOST_ONCE: Once = Once::new();
                LOST_ONCE.call_once(|| {
                    eprintln!("jetty: the window's surface was lost and could not be recreated yet; skipping frames");
                });
                Err(AcquireError::Lost)
            }
            Err(wgpu::CurrentSurfaceTexture::Validation) => {
                // A configure the driver refused (a resize, a reconfigure after a
                // loss) leaves the surface unconfigured — wgpu drops its old
                // configuration first — and every later acquire lands here:
                // configure it again (the caller retries on a bounded backoff).
                surface.configure(&self.device, &self.config);
                Err(AcquireError::Validation)
            }
            Err(wgpu::CurrentSurfaceTexture::Occluded) => Err(AcquireError::Occluded),
            // Timeout: no image handed out in time.
            Err(_) => Err(AcquireError::Timeout),
        }
    }

    /// Replace a surface that stays lost with a new one on the same window and
    /// device, configured as before; `false` (no surface held) when that fails,
    /// and the next acquire tries again. The old surface goes FIRST: it still
    /// holds the window's swapchain (see [`Self::release_surface`]).
    fn recreate_surface(&mut self) -> bool {
        self.surface = None;
        // Each distinct failure is logged once (`log_wgpu_error`): this repeats
        // on every retry while the window cannot take a surface.
        let surface = match self.shared.instance.create_surface(Arc::clone(&self.window)) {
            Ok(s) => s,
            Err(e) => {
                log_wgpu_error(&format!("recreating the window's surface: {e}"));
                return false;
            }
        };
        if !self.shared.adapter.is_surface_supported(&surface) {
            log_wgpu_error("recreating the window's surface: the GPU cannot present to it");
            return false;
        }
        if let Err(e) = configure_checked(&self.device, &surface, &self.config) {
            log_wgpu_error(&format!("configuring the recreated surface: {e}"));
            return false;
        }
        self.surface = Some(surface);
        true
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

    /// An adapter as wgpu reports it (only the fields the choice reads matter).
    fn adapter(name: &str, backend: Backend, device_type: wgpu::DeviceType) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: name.to_string(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: false,
        }
    }

    /// The backend as users know it: the welcome splash read "wgpu · Gl".
    #[test]
    fn backends_carry_their_user_facing_names() {
        use super::backend_display_name;
        assert_eq!(backend_display_name(Backend::Vulkan), "Vulkan");
        assert_eq!(backend_display_name(Backend::Gl), "OpenGL");
        assert_eq!(backend_display_name(Backend::Metal), "Metal");
        assert_eq!(backend_display_name(Backend::Dx12), "DirectX 12");
        assert_eq!(backend_display_name(Backend::BrowserWebGpu), "WebGPU");
    }

    /// Software rasterizers, whatever device type wgpu inferred: GL has none, and
    /// wgpu's renderer-string guess calls softpipe "Other".
    #[test]
    fn software_rasterizers_are_recognized_on_every_backend() {
        use super::is_software;
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, Other, VirtualGpu};
        assert!(is_software(&adapter("llvmpipe (LLVM 15.0.7, 256 bits)", Backend::Vulkan, Cpu)));
        assert!(is_software(&adapter("llvmpipe (LLVM 15.0.7, 256 bits)", Backend::Gl, Cpu)));
        assert!(is_software(&adapter("softpipe", Backend::Gl, Other)));
        assert!(is_software(&adapter("Gallium 0.4 on SWRAST", Backend::Gl, Other)));
        assert!(is_software(&adapter("zink Vulkan 1.3(llvmpipe (LLVM 15.0.7, 256 bits))", Backend::Gl, Other)));
        assert!(!is_software(&adapter("Mesa Intel(R) HD Graphics 3000 (SNB GT2)", Backend::Gl, IntegratedGpu)));
        assert!(!is_software(&adapter("AMD Radeon HD 6450 (CAICOS, DRM 2.50.0)", Backend::Gl, Other)));
        assert!(!is_software(&adapter("virgl (NVIDIA GeForce RTX 3060)", Backend::Gl, Other)));
        assert!(!is_software(&adapter("NVIDIA GeForce RTX 3060", Backend::Vulkan, DiscreteGpu)));
        assert!(!is_software(&adapter("Virtio-GPU Venus", Backend::Vulkan, VirtualGpu)));
    }

    /// A hardware Vulkan adapter takes today's path — no GL probe, no added cost;
    /// only software Vulkan (lavapipe) looks at GL, and `WGPU_BACKEND` wins.
    #[test]
    fn only_software_vulkan_probes_gl() {
        use super::wants_gl_probe;
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, Other};
        let lavapipe = adapter("llvmpipe (LLVM 15.0.7, 256 bits)", Backend::Vulkan, Cpu);
        assert!(wants_gl_probe(&lavapipe, false));
        assert!(!wants_gl_probe(&lavapipe, true), "WGPU_BACKEND pins the backends");
        assert!(!wants_gl_probe(&adapter("Intel(R) Graphics (ARL)", Backend::Vulkan, IntegratedGpu), false));
        assert!(!wants_gl_probe(&adapter("NVIDIA GeForce RTX 3060", Backend::Vulkan, DiscreteGpu), false));
        // No Vulkan at all: the pick is already GL (or Metal), nothing to compare.
        assert!(!wants_gl_probe(&adapter("llvmpipe (LLVM 15.0.7, 256 bits)", Backend::Gl, Cpu), false));
        assert!(!wants_gl_probe(&adapter("AMD Radeon HD 6450", Backend::Gl, Other), false));
    }

    /// A GL adapter as the choice sees it.
    fn gl(name: &str, device_type: wgpu::DeviceType, version: &str, compute_shaders: bool) -> super::GlCandidate {
        let mut info = adapter(name, Backend::Gl, device_type);
        info.driver_info = version.to_string();
        super::GlCandidate { info, compute_shaders }
    }

    /// Software Vulkan gives way only to a hardware GL adapter that draws
    /// correctly; a software GL renderer (llvmpipe, softpipe), a driver wgpu
    /// misbinds, or none keeps it — CPU Vulkan presents fine.
    #[test]
    fn hardware_gl_beats_software_vulkan_and_nothing_else_does() {
        use super::prefers_gl;
        use wgpu::DeviceType::{Cpu, IntegratedGpu, Other};
        let lavapipe = adapter("llvmpipe (LLVM 15.0.7, 256 bits)", Backend::Vulkan, Cpu);
        let mesa = |v: &str| format!("{v} (Core Profile) Mesa 22.0.1");
        let intel_gl = gl("Mesa Intel(R) HD Graphics 3000 (SNB GT2)", IntegratedGpu, &mesa("3.3"), false);
        let radeon_gl = gl("AMD Radeon HD 6450 (CAICOS, DRM 2.50.0)", Other, &mesa("4.5"), true);
        let virgl = gl("virgl (NVIDIA GeForce RTX 3060)", Other, "4.3 (Core Profile) Mesa 22.0.1", true);
        let gles = gl("Mali-G52 (Panfrost)", IntegratedGpu, "OpenGL ES 3.1 Mesa 22.0.1", true);
        assert!(prefers_gl(&lavapipe, Some(&intel_gl)));
        assert!(prefers_gl(&lavapipe, Some(&radeon_gl)));
        assert!(prefers_gl(&lavapipe, Some(&virgl)));
        assert!(prefers_gl(&lavapipe, Some(&gles)));
        let llvmpipe_gl = gl("llvmpipe (LLVM 15.0.7, 256 bits)", Cpu, "4.5 (Core Profile) Mesa 22.0.1", true);
        assert!(!prefers_gl(&lavapipe, Some(&llvmpipe_gl)));
        let softpipe = gl("softpipe", Other, "3.3 (Core Profile) Mesa 22.0.1", true);
        assert!(!prefers_gl(&lavapipe, Some(&softpipe)));
        // Hardware, but GL 3.3 advertising compute: wgpu would sample every
        // texture from one unit — lavapipe keeps drawing text.
        let misbound = gl("NV98", Other, "3.3 (Core Profile) Mesa 22.0.1", true);
        assert!(!prefers_gl(&lavapipe, Some(&misbound)));
        assert!(!prefers_gl(&lavapipe, None));
        // A hardware first pick is never traded away.
        let arc = adapter("Intel(R) Graphics (ARL)", Backend::Vulkan, IntegratedGpu);
        assert!(!prefers_gl(&arc, Some(&radeon_gl)));
    }

    /// wgpu 29 binds textures in the shader when the driver offers compute
    /// shaders, but naga writes those bindings only for GLSL 4.20+ / ES 3.10+.
    #[test]
    fn misbound_gl_textures_are_detected() {
        use super::{desktop_gl_version, gl_textures_misbound};
        use wgpu::DeviceType::{Cpu, Other};
        assert_eq!(desktop_gl_version("3.3 (Core Profile) Mesa 26.0.8-1ubuntu0.3"), Some((3, 3)));
        assert_eq!(desktop_gl_version("4.6.0 NVIDIA 595.99.02"), Some((4, 6)));
        assert_eq!(desktop_gl_version("OpenGL ES 3.0 Mesa 26.0.8"), None);
        assert_eq!(desktop_gl_version(""), None);
        // Mesa's softpipe: GL 3.3 core + GL_ARB_compute_shader (seen live: no text).
        assert!(gl_textures_misbound(&gl("softpipe", Other, "3.3 (Core Profile) Mesa 26.0.8", true)));
        assert!(gl_textures_misbound(&gl("NV98", Other, "4.1 (Core Profile) Mesa 22.0.1", true)));
        assert!(!gl_textures_misbound(&gl("NV98", Other, "3.3 (Core Profile) Mesa 22.0.1", false)));
        assert!(!gl_textures_misbound(&gl("llvmpipe", Cpu, "4.5 (Core Profile) Mesa 26.0.8", true)));
        assert!(!gl_textures_misbound(&gl("NV98", Other, "4.2 (Core Profile) Mesa 22.0.1", true)));
        assert!(!gl_textures_misbound(&gl("Mali", Other, "OpenGL ES 3.0 Mesa 26.0.8", false)));
        assert!(!gl_textures_misbound(&gl("Mali", Other, "OpenGL ES 3.1 Mesa 26.0.8", true)));
    }

    /// Vulkan alone first, then every backend; `WGPU_BACKEND` (`gl`, `vulkan`, …)
    /// pins them — the way around a broken Vulkan driver.
    #[test]
    fn wgpu_backend_pins_the_backends_tried() {
        use super::backend_attempts;
        let default = [Some(Backends::VULKAN), Some(Backends::all())];
        assert_eq!(backend_attempts(None), default);
        assert_eq!(backend_attempts(Some(Backends::GL)), [Some(Backends::GL), None]);
        assert_eq!(backend_attempts(Some(Backends::VULKAN)), [Some(Backends::VULKAN), None]);
        // An unknown value parses to no backend: ignored, not a GPU-less start.
        assert_eq!(backend_attempts(Some(Backends::empty())), default);
        assert_eq!(backend_attempts(Some(Backends::from_comma_list("foo"))), default);
        assert_eq!(backend_attempts(Some(Backends::from_comma_list("gl"))), [Some(Backends::GL), None]);
    }

    /// JeTTY issues no indirect draws or dispatches: wgpu's indirect-call
    /// validation (on by default in release builds) only compiled two compute
    /// pipelines into every device creation — ~2 ms of cold start on Vulkan,
    /// ~3.5 ms on GL.
    #[test]
    fn no_instance_pays_for_indirect_call_validation() {
        for backends in [Backends::VULKAN, Backends::all()] {
            let flags = instance_descriptor(backends).flags;
            assert!(!flags.contains(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL), "{backends:?}");
            // Everything else stays as wgpu's build-type default.
            assert_eq!(flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL,
                wgpu::InstanceFlags::default() | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
        }
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

    /// Every window's layers take one build per type and surface format: the
    /// first caller builds, the rest get that value.
    #[test]
    fn shared_pipelines_build_once_per_type_and_format() {
        use wgpu::TextureFormat as F;
        let shared = super::SharedPipelines::default();
        let mut builds = 0;
        let mut get = |format, value: u32| {
            shared.get_or(format, || {
                builds += 1;
                value
            })
        };
        assert_eq!(get(F::Bgra8UnormSrgb, 1), 1);
        assert_eq!(get(F::Bgra8UnormSrgb, 2), 1, "kept: not built again");
        assert_eq!(get(F::Rgba8UnormSrgb, 3), 3, "another format builds its own");
        assert_eq!(builds, 2);
        // Another type for the same format is its own entry.
        assert_eq!(shared.get_or(F::Bgra8UnormSrgb, || "quad"), "quad");
        assert_eq!(shared.get_or(F::Bgra8UnormSrgb, || "mask"), "quad");
    }

    /// A failed acquire is retried on the app's bounded timer — except for an
    /// occluded window, whose Occluded(false) repaints it.
    #[test]
    fn only_an_occluded_window_waits_without_a_retry() {
        use super::AcquireError as E;
        for e in [E::Outdated, E::Lost, E::Timeout, E::Validation] {
            assert!(e.wants_retry(), "{e:?}");
        }
        assert!(!E::Occluded.wants_retry());
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
