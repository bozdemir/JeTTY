/// Headless performance benchmark for Jetty's hot path — NO window/display.
///
/// Measures the numbers that define the perf budget (docs/perf-budget.md):
///   - gpu_init:   time to acquire the wgpu adapter + device (startup-dominant)
///   - throughput: MB/s feeding typical colored VT output through the parser+grid
///   - snapshot:   per-frame CPU cost of building a GridSnapshot
///   - render:     per-frame GPU+CPU cost of rendering a full screen offscreen
///   - pipeline_1byte_cpu: per-byte CPU PIPELINE COMPUTE (feed 1 byte → snapshot)
///
/// NOTE: `pipeline_1byte_cpu` is NOT keypress→glyph input latency — it excludes the
/// PTY write, shell-echo round-trip, reader-thread wake, winit, and the compositor/
/// display. Real input latency is measured on the running app via `JETTY_PERF_LOG=1`
/// (see perf-budget.md). It is an informational pipeline-compute proxy only.
///
/// Run: cargo run --release -p jetty-app --bin jetty-bench
///
/// `JETTY_BENCH_CPU_ONLY=1` runs a no-GPU subset (throughput + snapshot +
/// pipeline_1byte_cpu) against a fixed baseline grid — never constructs a wgpu
/// instance/adapter/device. This is what CI runs: it avoids GPU-availability and
/// software-rasterizer timing variance on shared runners (NOT because the GPU bench
/// "crashes" there — it simply removes GPU-dependent numbers from the report).
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use jetty_app::perf::{env_enabled, percentile};
use jetty_render::TextLayer;

/// The system allocator, counting every allocation (and its bytes) so the frame
/// tables can report what each frame allocates — the hot path should allocate
/// next to nothing. Two relaxed atomic adds per allocation: no measurable cost.
struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// `(allocations, bytes)` so far.
fn alloc_counts() -> (u64, u64) {
    (ALLOCS.load(Ordering::Relaxed), ALLOC_BYTES.load(Ordering::Relaxed))
}

/// The typical colored prompt+output line fed for the throughput test.
const VT_LINE: &[u8] = b"\x1b[1;32muser@host\x1b[0m:\x1b[34m~/src/jetty\x1b[0m$ \x1b[33mcargo build\x1b[0m --release --workspace   \x1b[2m# building 4 crates\x1b[0m\r\n";

/// Build ~`target` bytes of `VT_LINE` repeated.
fn make_payload(target: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(target + VT_LINE.len());
    while payload.len() < target {
        payload.extend_from_slice(VT_LINE);
    }
    payload
}

/// Feed `payload` into `term` in 8 KiB chunks — the PTY reader's read size, so
/// the live drain's chunk shape (measured: 4 / 8 / 64 KiB chunks feed within
/// noise of each other).
fn feed_chunked(term: &mut jetty_core::Terminal, payload: &[u8]) {
    let chunk = 8192;
    let mut i = 0;
    while i < payload.len() {
        let end = (i + chunk).min(payload.len());
        term.feed(&payload[i..end]);
        i = end;
    }
}

/// Per-byte CPU pipeline compute: feed ONE byte then build a full snapshot, `n`
/// times, and return (min, p50, p99, n) in ms.
///
/// This is the CPU half of the echo pipeline (what happens once a byte has already
/// arrived), NOT keypress→glyph latency. It is dominated by the snapshot cost, so
/// its p50 tracks the `snapshot` metric — it is informational, never a hard gate.
fn pipeline_1byte_cpu(term: &mut jetty_core::Terminal) -> (f32, f32, f32, usize) {
    let n = 2000usize;
    let mut lat = Vec::with_capacity(n);
    for k in 0..n {
        // Vary the byte so the parser does real work each iteration.
        let b = [b"x0123456789abcdef"[k & 15]];
        let t = Instant::now();
        term.feed(&b);
        let _s = term.snapshot();
        lat.push(t.elapsed().as_secs_f32() * 1000.0);
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (
        lat.first().copied().unwrap_or(0.0),
        percentile(&lat, 50.0),
        percentile(&lat, 99.0),
        n,
    )
}

/// Print the `pipeline_1byte_cpu` line with its full exclusion label so it can never
/// be misread as input latency.
fn print_pipeline_1byte_cpu(term: &mut jetty_core::Terminal) {
    let (lmin, l50, l99, n) = pipeline_1byte_cpu(term);
    println!(
        "pipeline_1byte_cpu  min {lmin:.3} p50 {l50:.3} p99 {l99:.3} ms  (n={n}; feed 1 byte→snapshot, CPU compute only — \
         NOT input latency: excludes PTY write + shell-echo round-trip + reader-thread wake + winit + compositor/display)"
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // CI / no-GPU path: skip ALL of wgpu (never construct an instance/adapter/device
    // or a TextLayer) and report only the display-independent CPU metrics.
    if env_enabled(std::env::var_os("JETTY_BENCH_CPU_ONLY")) {
        return run_cpu_only();
    }

    // Match the user's actual monitor so the frame budget is realistic.
    let width: u32 = 1920;
    let height: u32 = 1200;
    let font_size: f32 = 16.0;

    // --- startup-dominant cost: GPU adapter + device ---
    // Match the live app: Vulkan-only instance (skips GLES enumeration), with an
    // all-backends fallback if no Vulkan adapter is present.
    // GPU selection. By default the bench requests LowPower → the integrated GPU,
    // matching the live app (which deliberately avoids the discrete GPU on hybrid
    // systems, where driving it under a live X11/Wayland surface can destabilize
    // the compositor). Set JETTY_BENCH_GPU=high (aliases: `discrete`, `dgpu`) to
    // benchmark on the high-performance discrete GPU instead — safe here because
    // the bench is HEADLESS (offscreen texture, no compositor surface at risk).
    let power = match std::env::var("JETTY_BENCH_GPU").as_deref() {
        Ok("high") | Ok("discrete") | Ok("dgpu") => wgpu::PowerPreference::HighPerformance,
        _ => wgpu::PowerPreference::LowPower,
    };
    // The app's startup Vulkan driver filter (`vk_loader`), installed the same
    // way (single-threaded, before the first instance). `JETTY_BENCH_NO_VK_FILTER`
    // measures the unfiltered loader instead.
    let t0 = Instant::now();
    let filter = if env_enabled(std::env::var_os("JETTY_BENCH_NO_VK_FILTER")) {
        None
    } else {
        jetty_render::vk_loader::install(power == wgpu::PowerPreference::HighPerformance)
    };
    let instance_ms = std::cell::Cell::new(0.0f64);
    let attempts = std::cell::Cell::new(0u32);
    let vulkan = jetty_render::vk_loader::with_prefilter(
        || {
            attempts.set(attempts.get() + 1);
            let t = Instant::now();
            let instance = wgpu::Instance::new(jetty_render::instance_descriptor(wgpu::Backends::VULKAN));
            instance_ms.set(instance_ms.get() + t.elapsed().as_secs_f64() * 1000.0);
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
            .map(|adapter| (instance, adapter))
        },
        |(instance, adapter)| jetty_render::vk_loader::Probe::of(instance, adapter),
    );
    let (_instance, adapter) = match vulkan {
        Ok(t) => t,
        Err(_) => {
            let instance = wgpu::Instance::new(jetty_render::instance_descriptor(wgpu::Backends::all()));
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))?;
            (instance, adapter)
        }
    };
    let instance_ms = instance_ms.get();
    let adapter_ms = t0.elapsed().as_secs_f64() * 1000.0 - instance_ms;
    let only = std::env::var("JETTY_BENCH_ONLY").ok();
    // `JETTY_BENCH_ONLY=backdrop`: just the backdrop section (quick repeats).
    if only.as_deref() == Some("backdrop") {
        return bench_backdrop(&adapter);
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("jetty-bench"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        ..Default::default()
    }))?;
    let gpu_init_ms = t0.elapsed().as_secs_f64() * 1000.0;
    // `JETTY_BENCH_ONLY=gpu_init`: the startup-dominant GPU block alone, split
    // into its steps (quick repeats to compare driver / loader setups), plus
    // what it left resident (Linux).
    if only.as_deref() == Some("gpu_init") {
        print_gpu_init(&adapter, instance_ms, adapter_ms, gpu_init_ms);
        match &filter {
            Some(p) => println!(
                "vk filter     {} attempt(s); skipped {}",
                attempts.get(),
                p.disable.replace('*', "").replace(',', " ")
            ),
            None => println!("vk filter     none (every installed driver initialized)"),
        }
        drop((device, queue));
        return Ok(());
    }
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    // `JETTY_BENCH_ONLY=frames`: just the per-frame table + scene passes (quick
    // repeats for A/B runs).
    if only.as_deref() == Some("frames") {
        return bench_frames(&device, &queue, format, font_size);
    }
    // `JETTY_BENCH_ONLY=first_frame`: the cold start's text layers and first-frame
    // effect pipelines, serial or overlapped (one measurement per process).
    if only.as_deref() == Some("first_frame") {
        return bench_first_frame(&device, &queue, format, font_size);
    }

    // Split as the app pays it: the font-DB scan overlaps GPU init on a worker
    // thread, the layer (atlas, pipelines, font loads) is built on the UI thread
    // before the first frame — once per text layer.
    let t1 = Instant::now();
    let fonts = TextLayer::build_font_system();
    let font_scan_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    let mut text = TextLayer::new_with_family_and_fonts(&device, &queue, format, font_size, "MesloLGS NF", fonts);
    let layer_init_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let text_init_ms = font_scan_ms + layer_init_ms;

    let (cw, ch) = text.cell_size();
    let cols = (width as f32 / cw).floor().max(1.0) as usize;
    let rows = (height as f32 / ch).floor().max(1.0) as usize;

    // --- throughput: feed ~50 MB of typical colored prompt+output ---
    let mut term = jetty_core::Terminal::new(cols, rows);
    let payload = make_payload(50 * 1024 * 1024);
    let t2 = Instant::now();
    feed_chunked(&mut term, &payload);
    let feed_s = t2.elapsed().as_secs_f64();
    let mb = payload.len() as f64 / 1_048_576.0;
    let mbps = mb / feed_s;

    // --- per-frame CPU: snapshot() ---
    let mut snap = term.snapshot();
    let n_snap = 500;
    let t3 = Instant::now();
    for _ in 0..n_snap {
        snap = term.snapshot();
    }
    let snap_ms = t3.elapsed().as_secs_f64() * 1000.0 / n_snap as f64;

    // --- per-frame GPU+CPU: render a full screen offscreen ---
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    // warm up (shader/pipeline compile, atlas upload)
    text.render_to(&device, &queue, &view, width, height, &snap, true, 0.0)?;
    device.poll(wgpu::PollType::wait_indefinitely())?;

    // Split each frame into CPU-prep (build spans + glyphon prepare + queue.submit,
    // all inside render_to) vs GPU-execute (the device.poll wait for the GPU to
    // finish). NOTE: this re-renders the SAME snapshot every frame, so after the
    // warm-up no row is ever re-shaped — it measures the unchanged-grid path (what
    // a caret-flash / CRT-only frame costs), NOT a frame with new output. The
    // `frames` section below measures frames whose content actually changes.
    let n_frames = 200;
    let mut cpu_accum = 0.0f64;
    let t4 = Instant::now();
    for _ in 0..n_frames {
        let c = Instant::now();
        text.render_to(&device, &queue, &view, width, height, &snap, true, 0.0)?;
        cpu_accum += c.elapsed().as_secs_f64();
        device.poll(wgpu::PollType::wait_indefinitely())?;
    }
    let frame_ms = t4.elapsed().as_secs_f64() * 1000.0 / n_frames as f64;
    let cpu_ms = cpu_accum * 1000.0 / n_frames as f64;
    let gpu_ms = (frame_ms - cpu_ms).max(0.0);

    println!(
        "=== Jetty perf bench ({} {}) ===",
        adapter.get_info().name,
        jetty_render::backend_display_name(adapter.get_info().backend)
    );
    println!("grid          {cols}x{rows} cells (cell {cw:.1}x{ch:.1}px) @ {width}x{height}");
    println!(
        "gpu_init      {gpu_init_ms:6.1} ms    (adapter + device acquisition; {})",
        if filter.is_some() { "startup Vulkan driver filter" } else { "every installed Vulkan driver" }
    );
    println!(
        "text_init     {text_init_ms:6.1} ms    (font-DB scan {font_scan_ms:.1} — a worker thread in the app — \
         + layer init {layer_init_ms:.1} on the UI thread)"
    );
    println!("throughput    {mbps:6.0} MB/s   (fed {mb:.0} MB colored VT in {feed_s:.2}s)");
    println!("snapshot      {snap_ms:8.3} ms/frame  ({:.0}k cells)", (cols * rows) as f64 / 1000.0);
    println!("render        {frame_ms:8.3} ms/frame  ({:.0} fps cap; SAME snapshot every frame → no re-shape)", 1000.0 / frame_ms);
    println!("  ├─ cpu prep {cpu_ms:8.3} ms/frame  (build spans + atlas prepare + submit; shaping skipped — unchanged grid)");
    println!("  └─ gpu exec {gpu_ms:8.3} ms/frame  (device.poll wait for GPU completion)");
    print_pipeline_1byte_cpu(&mut term);
    print_font_db_costs(&text);
    bench_frames(&device, &queue, format, font_size)?;
    bench_post_pass(&device, &queue, format, font_size)?;
    bench_backdrop(&adapter)?;
    Ok(())
}

/// The `JETTY_BENCH_ONLY=gpu_init` report: the GPU block's steps, the adapter it
/// chose, and (Linux) the shared libraries the Vulkan loader left mapped — every
/// installed ICD it initialized, chosen or not — with the process's resident set.
fn print_gpu_init(adapter: &wgpu::Adapter, instance_ms: f64, adapter_ms: f64, total_ms: f64) {
    let info = adapter.get_info();
    println!(
        "adapter       {} ({}, {:?})",
        info.name,
        jetty_render::backend_display_name(info.backend),
        info.device_type
    );
    println!(
        "gpu_init      {total_ms:6.1} ms    (instance {instance_ms:5.1} + adapter {adapter_ms:5.1} + device {:5.1})",
        total_ms - instance_ms - adapter_ms
    );
    #[cfg(target_os = "linux")]
    {
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
        let mut libs: Vec<&str> = maps
            .lines()
            .filter_map(|l| l.split_whitespace().nth(5))
            .filter(|p| p.contains(".so"))
            .collect();
        libs.sort_unstable();
        libs.dedup();
        let mib = |p: &str| std::fs::metadata(p).map_or(0.0, |m| m.len() as f64 / 1_048_576.0);
        let total: f64 = libs.iter().map(|p| mib(p)).sum();
        let mut big: Vec<(f64, &str)> = libs.iter().map(|p| (mib(p), *p)).filter(|(m, _)| *m >= 4.0).collect();
        big.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        println!("mapped libs   {} files, {total:.0} MiB on disk", libs.len());
        for (m, p) in big {
            println!("  {m:6.1} MiB {}", p.rsplit('/').next().unwrap_or(p));
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for key in ["VmRSS:", "RssAnon:", "RssFile:"] {
            if let Some(l) = status.lines().find(|l| l.starts_with(key)) {
                println!("{:<13} {}", key.trim_end_matches(':'), l[key.len()..].trim());
            }
        }
    }
}

/// The cold start's critical path after the GPU block (`JETTY_BENCH_ONLY=
/// first_frame`): the two text layers `resumed` builds (from an already-loaded
/// font database, as there), and the first frame's effect pipelines — the
/// default summon pass (Phosphor) and the Retro CRT preset's post-pass variant —
/// compiled after them on this thread (`JETTY_BENCH_FIRST_FRAME=serial`: what
/// frame 1 did) or on a worker while they build (the default, `overlap`: what
/// `resumed` does). ONE measurement per process — a second build would hit the
/// driver's in-memory pipeline cache — so compare many runs of each, with and
/// without `MESA_SHADER_CACHE_DISABLE=true` (a cold on-disk shader cache).
fn bench_first_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    font_size: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    use jetty_app::effects::{self, EffectsConfig};
    let overlap = std::env::var("JETTY_BENCH_FIRST_FRAME").map_or(true, |v| v != "serial");
    let mut fx = EffectsConfig::default();
    if let Some(p) = effects::effect_presets().iter().find(|p| p.id == "retro_crt") {
        p.patch.apply_to(&mut fx);
    }
    let key = effects::crt_settings(&fx).key();
    // The pipelines frame 1 needs, and how long they took (ms).
    fn effect_pipelines(device: &wgpu::Device, format: wgpu::TextureFormat, key: jetty_render::CrtKey) -> f64 {
        let t = Instant::now();
        let _phosphor = jetty_render::PhosphorIgnition::new(device, format);
        let crt = jetty_render::Crt::new(device, format);
        crt.prepare(device, key);
        t.elapsed().as_secs_f64() * 1000.0
    }
    let fonts = TextLayer::build_font_system();
    let t0 = Instant::now();
    let worker = overlap.then(|| {
        let device = device.clone();
        std::thread::spawn(move || effect_pipelines(&device, format, key))
    });
    let grid = TextLayer::new_with_family_and_fonts(device, queue, format, font_size, "MesloLGS NF", fonts);
    let chrome_fonts = grid.clone_font_system();
    let _chrome = TextLayer::new_with_family_and_fonts(
        device,
        queue,
        format,
        jetty_render::UI_FONT_BASE,
        "MesloLGS NF",
        chrome_fonts,
    );
    let text_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let fx_ms = match worker {
        Some(h) => h.join().map_err(|_| "effect worker panicked")?,
        None => effect_pipelines(device, format, key),
    };
    let wall = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "first_frame   {}: {wall:6.1} ms (text layers {text_ms:5.1} ms, effect pipelines {fx_ms:5.1} ms)",
        if overlap { "overlap" } else { "serial " }
    );
    Ok(())
}

/// The CRT post pass (`jetty_render::Crt`, the app's own settings path) on a
/// real rendered terminal frame at 2560×1440: the pipeline build per variant and
/// the GPU-synchronized ms/pass for CRT off (corners only — the floor), the
/// shipped defaults, a subtle everyday look, and the effect presets.
fn bench_post_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    font_size: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    use jetty_app::effects::{self, EffectsConfig};
    let (width, height) = (2560u32, 1440u32);
    let tex = |label: &str| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };
    let (scene, out) = (tex("bench-post-scene"), tex("bench-post-out"));
    // A full screen of colored terminal output as the scene.
    let mut text = TextLayer::new_with_family(device, queue, format, font_size, "MesloLGS NF");
    let (cw, ch) = text.cell_size();
    let (cols, rows) = ((width as f32 / cw) as usize, (height as f32 / ch) as usize);
    let mut term = jetty_core::Terminal::new(cols.max(1), rows.max(1));
    for k in 0..rows * 2 {
        term.feed(&numbered_line(k));
    }
    text.render_to(device, queue, &scene, width, height, &term.snapshot(), true, 0.0)?;
    device.poll(wgpu::PollType::wait_indefinitely())?;

    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    let frame = jetty_render::CrtFrame {
        width,
        height,
        corner_radius: 12.0,
        corner_radius_top: 12.0,
        time: 0.0,
        bg: [theme.bg[0], theme.bg[1], theme.bg[2]],
        fg: theme.fg,
        premultiplied: true,
        srgb: format.is_srgb(),
        dpi_scale: 1.0,
        glitch: 0.0,
    };
    let everyday = EffectsConfig {
        crt_enabled: true,
        crt_scanline: 0.10,
        crt_mask: 0.24,
        crt_bloom: 0.135,
        crt_chromatic: 0.15,
        crt_vignette: 0.50,
        ..EffectsConfig::default()
    };
    let defaults = EffectsConfig { crt_enabled: true, ..EffectsConfig::default() };
    let mut looks: Vec<(String, jetty_render::CrtSettings)> = vec![
        ("crt off (corners only)".into(), jetty_render::CrtSettings::PASSTHROUGH),
        ("defaults".into(), effects::crt_settings(&defaults)),
        ("everyday (subtle)".into(), effects::crt_settings(&everyday)),
    ];
    for p in effects::effect_presets().iter().filter(|p| p.patch.crt_enabled == Some(true)) {
        let mut fx = EffectsConfig::default();
        p.patch.apply_to(&mut fx);
        looks.push((format!("preset {}", p.name), effects::crt_settings(&fx)));
    }
    println!("post pass     {width}x{height}, Crt::apply incl. bloom chain, GPU-synced (median of 5×30 passes)");
    let crt = jetty_render::Crt::new(device, format);
    // Warm-up: clock the GPU up first, so the first row is not measured at idle
    // clocks (an iGPU ramps over ~100 ms of load).
    {
        let params = jetty_render::CrtParams::build(&looks[1].1, &frame);
        let warm = Instant::now();
        while warm.elapsed().as_millis() < 400 {
            for _ in 0..10 {
                crt.apply(device, queue, &out, &scene, &params);
            }
            device.poll(wgpu::PollType::wait_indefinitely())?;
        }
    }
    for (name, settings) in &looks {
        let params = jetty_render::CrtParams::build(settings, &frame);
        let t = Instant::now();
        crt.prepare(device, params.key);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;
        crt.apply(device, queue, &out, &scene, &params);
        device.poll(wgpu::PollType::wait_indefinitely())?;
        let mut runs: Vec<f64> = (0..5)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..30 {
                    crt.apply(device, queue, &out, &scene, &params);
                }
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
                t.elapsed().as_secs_f64() * 1000.0 / 30.0
            })
            .collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "  {name:<24} {:7.3} ms/pass  (variant {:#06x}, pipeline build {build_ms:5.1} ms)",
            runs[2],
            params.key.bits()
        );
    }
    Ok(())
}

/// Font-database cost on the main thread: a fresh fontconfig scan (what every extra
/// `TextLayer` — chrome, Settings, each detached window — used to pay) vs. building
/// from the already-loaded database (`TextLayer::clone_font_system`).
fn print_font_db_costs(text: &TextLayer) {
    let median3 = |f: &dyn Fn()| {
        let mut v: Vec<f32> = (0..3)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f32() * 1000.0
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[1]
    };
    let scan = median3(&|| drop(TextLayer::build_font_system()));
    let clone = median3(&|| drop(text.clone_font_system()));
    println!("font_db_scan  {scan:6.1} ms    (median of 3; fresh fontconfig scan)");
    println!("font_db_clone {clone:6.2} ms    (median of 3; reuse the loaded database)");
}

/// One content line for the frame benchmarks, numbered so every row is distinct
/// (identical rows would let a content-keyed cache cheat).
fn numbered_line(k: usize) -> Vec<u8> {
    let mut v = format!("\x1b[2m{k:06}\x1b[0m ").into_bytes();
    v.extend_from_slice(VT_LINE);
    v
}

/// Per-frame cost of frames whose CONTENT changes, at two fixed grid sizes:
///   static — same snapshot again (caret-flash / CRT-only frame)
///   typing — one printable char echoed at the prompt per frame
///   scroll — one new line per frame (the whole screen moves up one row)
///   tui    — a btop-like box-drawing frame + braille graph, repainted per frame
///   boxtype — one char per frame typed inside a full-screen box (Claude Code)
/// Each frame = snapshot + render_to (CPU) and then the GPU wait (total).
fn bench_frames(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    font_size: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("frames        (per frame: snapshot + render_to CPU | incl. GPU wait; mean, p99 CPU; n=300)");
    for &(cols, rows) in &[(120usize, 40usize), (240, 70)] {
        let mut text = TextLayer::new_with_family(device, queue, format, font_size, "MesloLGS NF");
        let (cw, ch) = text.cell_size();
        let width = (cols as f32 * cw).ceil() as u32 + 1;
        let height = (rows as f32 * ch).ceil() as u32 + 1;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench-frames-tex"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut term = jetty_core::Terminal::new(cols, rows);
        for k in 0..rows * 2 {
            term.feed(&numbered_line(k));
        }
        let snap = term.snapshot();
        text.render_to(device, queue, &view, width, height, &snap, true, 0.0)?;
        device.poll(wgpu::PollType::wait_indefinitely())?;

        let mut run = |label: &str,
                       setup: Option<fn(&mut jetty_core::Terminal, usize, usize)>,
                       step: &mut dyn FnMut(&mut jetty_core::Terminal, usize)|
         -> Result<(), Box<dyn std::error::Error>> {
            if let Some(setup) = setup {
                // Untimed: draw the scenario's starting screen and render it once.
                setup(&mut term, cols, rows);
                let snap = term.snapshot();
                text.render_to(device, queue, &view, width, height, &snap, true, 0.0)?;
                device.poll(wgpu::PollType::wait_indefinitely())?;
            }
            let n = 300usize;
            let mut cpu = Vec::with_capacity(n);
            let (mut total, mut snap_total) = (0.0f64, 0.0f64);
            let (mut allocs, mut alloc_bytes) = (0u64, 0u64);
            for k in 0..n {
                step(&mut term, k);
                let t = Instant::now();
                let (a0, b0) = alloc_counts();
                let snap = term.snapshot();
                snap_total += t.elapsed().as_secs_f64() * 1000.0;
                text.render_to(device, queue, &view, width, height, &snap, true, 0.0)?;
                cpu.push(t.elapsed().as_secs_f32() * 1000.0);
                let (a1, b1) = alloc_counts();
                (allocs, alloc_bytes) = (allocs + a1 - a0, alloc_bytes + b1 - b0);
                device.poll(wgpu::PollType::wait_indefinitely())?;
                total += t.elapsed().as_secs_f64() * 1000.0;
            }
            let mean = cpu.iter().sum::<f32>() / n as f32;
            cpu.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            println!(
                "  {cols:3}x{rows:<3} {label:<7} cpu {mean:7.3} ms (snapshot {:5.3}) | total {:7.3} ms | p99 cpu {:7.3} ms | {:5.1} allocs {:6.1} KB/frame",
                snap_total / n as f64,
                total / n as f64,
                percentile(&cpu, 99.0),
                allocs as f64 / n as f64,
                alloc_bytes as f64 / n as f64 / 1024.0
            );
            Ok(())
        };
        run("static", None, &mut |_t, _k| {})?;
        run("typing", None, &mut |t, k| {
            // Wrap like a long command line would, but stay on the prompt row most
            // of the time: a CR+LF every 100 chars starts a fresh prompt line.
            if k % 100 == 99 {
                t.feed(b"\r\n$ ");
            } else {
                t.feed(&[b"abcdefghijklmnopqrstuvwxyz"[k % 26]]);
            }
        })?;
        let mut line = 1_000_000usize;
        run("scroll", None, &mut |t, _k| {
            line += 1;
            t.feed(&numbered_line(line));
        })?;
        // A page of fresh output per frame (a flood, PageDown in a pager, a tab
        // switch): every row is new and re-shaped — the frame the 6.9 ms budget
        // is about.
        let mut page_line = 2_000_000usize;
        run("page", None, &mut |t, _k| {
            for _ in 0..rows {
                page_line += 1;
                t.feed(&numbered_line(page_line));
            }
        })?;
        // A btop-like TUI: a rounded box-drawing frame around braille graph rows,
        // redrawn in place every frame with the graph shifted one column (the
        // borders stay, every graph row changes).
        run("tui", None, &mut |t, k| t.feed(&tui_frame(cols, rows, k)))?;
        // Typing inside a box (Claude Code's input box): the frame stays, one char
        // per frame lands on the boxed input row.
        run("boxtype", Some(box_setup), &mut |t, k| {
            if k % 100 == 99 {
                t.feed(b"\x1b[2;3H\x1b[K\x1b[2;3H");
            } else {
                t.feed(&[b"abcdefghijklmnopqrstuvwxyz"[k % 26]]);
            }
        })?;
    }
    bench_scene_passes(device, queue, format, font_size)?;
    bench_chrome(device, queue, format)?;
    bench_corner_mask(device, queue, format)
}

/// The rounded-corner mask (radius 10, the default) after the chrome pass of a
/// typing frame — the tab bar and the status HUD, as `bench_chrome` draws them
/// — as a pass + submit of its own vs. recorded as the chrome pass's last draws
/// (`render_chrome_then`), interleaved frame by frame at 1920×1200.
fn bench_corner_mask(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (1920u32, 1200u32);
    let cm = jetty_render::ChromeMetrics::new(1.0, jetty_render::UI_FONT_BASE);
    let mut chrome = TextLayer::new_with_family(device, queue, format, jetty_render::UI_FONT_BASE, "MesloLGS NF");
    let mut quad = jetty_render::QuadLayer::new(device, format);
    let mask = jetty_render::CornerMask::new(device, format);
    let view = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("bench-mask-tex"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    let tabs: Vec<(String, bool)> =
        ["~/src/jetty", "cargo build", "nvim"].iter().enumerate().map(|(i, t)| (t.to_string(), i == 0)).collect();
    let deco = vec![jetty_render::TabDeco::default(); tabs.len()];
    let opts = jetty_render::TabBarOpts::default();
    let status_h = cm.status_h();
    let radii = [10.0; 4];
    let n = 300usize;
    let mut cpu = [Vec::with_capacity(n), Vec::with_capacity(n)];
    let mut allocs = [0u64; 2];
    for k in 0..2 * n + 40 {
        let folded = k % 2 == 1;
        let hud = format!("⚡ {:.1} ms · {} fps · {}% CPU · 0 MB/s", 16.0 + (k % 7) as f32 * 0.1, 60 - k % 3, k % 5);
        let t = Instant::now();
        let a0 = alloc_counts().0;
        let bar = jetty_render::build_tab_bar_styled(
            width,
            &tabs,
            &theme,
            None,
            jetty_render::CtrlHover::None,
            None,
            &mut chrome,
            cm,
            &deco,
            &opts,
        );
        let strip =
            jetty_render::build_status_strip(width, height as f32 - status_h, status_h, Some(&hud), &theme, &mut chrome, cm);
        let mut quads = bar.quads;
        quads.push(strip.quad);
        let mut labels = bar.labels;
        labels.extend(strip.label);
        let sets = [(&labels[..], false), (&bar.title_labels[..], true)];
        if folded {
            let tail = |pass: &mut wgpu::RenderPass<'_>| mask.record(queue, pass, width, height, radii, 0.0);
            let _ = chrome.render_chrome_then(device, queue, &view, width, height, &mut quad, &quads, &sets, Some(tail));
        } else {
            let _ = chrome.render_chrome(device, queue, &view, width, height, &mut quad, &quads, &sets);
            mask.apply(device, queue, &view, width, height, radii[0], radii[1], radii[2], radii[3]);
        }
        // The first frames shape and cache; measure the steady state.
        if k >= 40 {
            cpu[folded as usize].push(t.elapsed().as_secs_f32() * 1000.0);
            allocs[folded as usize] += alloc_counts().0 - a0;
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
    }
    for (variant, label) in ["own pass", "in the chrome pass"].iter().enumerate() {
        let v = &mut cpu[variant];
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "corner mask   chrome + mask, {width}x{height}, {label:<18}: cpu {mean:.3} ms (p50 {:.3}) | {:.0} allocs/frame",
            percentile(v, 50.0),
            allocs[variant] as f64 / v.len() as f64
        );
    }
    Ok(())
}

/// One full redraw of a btop-like screen: `╭─…─╮`, rows of `│` + a braille graph
/// (shifted by `k` columns) + `│`, `╰─…─╯`, a block-element bar row, all
/// cursor-addressed from the home position like a real TUI repaint.
fn tui_frame(cols: usize, rows: usize, k: usize) -> Vec<u8> {
    let inner = cols.saturating_sub(2);
    let mut s = String::with_capacity(rows * cols * 3);
    s.push_str("\x1b[H\x1b[36m╭");
    s.extend(std::iter::repeat_n('─', inner));
    s.push_str("╮\x1b[0m");
    for r in 1..rows.saturating_sub(2) {
        s.push_str(&format!("\x1b[{};1H\x1b[36m│\x1b[32m", r + 1));
        for c in 0..inner {
            // A deterministic wave: braille dots rising and falling with the column.
            let v = ((c + k) * 7 + r * 13) % 256;
            s.push(char::from_u32(0x2800 + v as u32).unwrap_or('⠀'));
        }
        s.push_str("\x1b[36m│\x1b[0m");
    }
    s.push_str(&format!("\x1b[{};1H\x1b[33m", rows.saturating_sub(1)));
    for c in 0..cols {
        s.push(char::from_u32(0x2581 + ((c + k) % 8) as u32).unwrap_or('█'));
    }
    s.push_str(&format!("\x1b[0m\x1b[{};1H\x1b[36m╰", rows));
    s.extend(std::iter::repeat_n('─', inner));
    s.push_str("╯\x1b[0m");
    s.into_bytes()
}

/// Clear the screen and draw a full-screen rounded box with the cursor parked on
/// its first inner row (the `boxtype` scenario types there).
fn box_setup(term: &mut jetty_core::Terminal, cols: usize, rows: usize) {
    let inner = cols.saturating_sub(2);
    let mut s = String::from("\x1b[2J\x1b[H╭");
    s.extend(std::iter::repeat_n('─', inner));
    s.push('╮');
    for r in 1..rows.saturating_sub(1) {
        s.push_str(&format!("\x1b[{};1H│\x1b[{};{}H│", r + 1, r + 1, cols));
    }
    s.push_str(&format!("\x1b[{};1H╰", rows));
    s.extend(std::iter::repeat_n('─', inner));
    s.push('╯');
    s.push_str("\x1b[2;3H");
    term.feed(s.as_bytes());
}

/// The window chrome the app draws on EVERY frame — the tab bar (4 tabs: quads,
/// monospace labels, sans titles) and the bottom status strip with the perf HUD —
/// built and drawn exactly as the main window does (each layer its own pass and
/// submit), at 1920×1200. The HUD text changes every frame, the titles don't.
fn bench_chrome(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (1920u32, 1200u32);
    let cm = jetty_render::ChromeMetrics::new(1.0, jetty_render::UI_FONT_BASE);
    let mut chrome = TextLayer::new_with_family(device, queue, format, jetty_render::UI_FONT_BASE, "MesloLGS NF");
    let mut quad = jetty_render::QuadLayer::new(device, format);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-chrome-tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let theme = jetty_core::Theme::by_name("catppuccin_mocha");
    let tabs: Vec<(String, bool)> = ["burak@omen: ~/src/jetty", "cargo build --release", "nvim src/main.rs", "htop"]
        .iter()
        .enumerate()
        .map(|(i, t)| (t.to_string(), i == 0))
        .collect();
    let deco = vec![jetty_render::TabDeco::default(); tabs.len()];
    let opts = jetty_render::TabBarOpts::default();
    let status_h = cm.status_h();
    let n = 300usize;
    // [a pass + submit per layer (bar quads, labels, titles, strip quad, strip
    // label), ONE pass for all of it (`render_chrome`)], interleaved frame by frame.
    let mut cpu = [Vec::with_capacity(n), Vec::with_capacity(n)];
    let mut allocs = [0u64; 2];
    for k in 0..2 * n + 40 {
        let variant = k % 2;
        let hud = format!("⚡ {:.1} ms · {} fps · {}% CPU · 0 MB/s", 16.0 + (k % 7) as f32 * 0.1, 60 - k % 3, k % 5);
        let t = Instant::now();
        let a0 = alloc_counts().0;
        let bar = jetty_render::build_tab_bar_styled(
            width,
            &tabs,
            &theme,
            None,
            jetty_render::CtrlHover::None,
            None,
            &mut chrome,
            cm,
            &deco,
            &opts,
        );
        let strip = jetty_render::build_status_strip(
            width,
            height as f32 - status_h,
            status_h,
            Some(&hud),
            &theme,
            &mut chrome,
            cm,
        );
        if variant == 0 {
            quad.render(device, queue, &view, width, height, &bar.quads);
            let _ = chrome.render_overlays(device, queue, &view, width, height, &bar.labels);
            let _ = chrome.render_overlays_sans(device, queue, &view, width, height, &bar.title_labels);
            quad.render(device, queue, &view, width, height, &[strip.quad]);
            if let Some(label) = strip.label {
                let _ = chrome.render_overlays(device, queue, &view, width, height, &[label]);
            }
        } else {
            let mut quads = bar.quads;
            quads.push(strip.quad);
            let mut labels = bar.labels;
            labels.extend(strip.label);
            let _ = chrome.render_chrome(
                device,
                queue,
                &view,
                width,
                height,
                &mut quad,
                &quads,
                &[(&labels, false), (&bar.title_labels, true)],
            );
        }
        // The first frames shape and cache; measure the steady state.
        if k >= 40 {
            cpu[variant].push(t.elapsed().as_secs_f32() * 1000.0);
            allocs[variant] += alloc_counts().0 - a0;
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
    }
    for (variant, label) in ["pass per layer", "one pass"].iter().enumerate() {
        let v = &mut cpu[variant];
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "chrome        tab bar (4 tabs) + status HUD, {width}x{height}, {label:<14}: cpu {mean:.3} ms (p50 {:.3}) | {:.0} allocs/frame",
            percentile(v, 50.0),
            allocs[variant] as f64 / v.len() as f64
        );
    }
    Ok(())
}

/// The app's whole grid scene per frame — cell-background quads + glyphs, typing —
/// recorded as two render passes + two submits (the pre-batching render core) vs.
/// ONE pass + ONE submit (what `render_grid_scene` does now). Interleaved frame by
/// frame so machine load hits both variants alike.
fn bench_scene_passes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    font_size: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let (cols, rows) = (240usize, 70usize);
    let mut text = TextLayer::new_with_family(device, queue, format, font_size, "MesloLGS NF");
    let mut quad = jetty_render::QuadLayer::new(device, format);
    let (cw, ch) = text.cell_size();
    let width = (cols as f32 * cw).ceil() as u32 + 1;
    let height = (rows as f32 * ch).ceil() as u32 + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-scene-tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut term = jetty_core::Terminal::new(cols, rows);
    for k in 0..rows * 2 {
        term.feed(&numbered_line(k));
    }
    // The app's Pass 1 draws the backdrop first; mirror it in the 1-pass variant
    // when JETTY_BENCH_BACKDROP names a mode/pattern (unset = none = today).
    let backdrop = std::env::var("JETTY_BENCH_BACKDROP").ok().and_then(|spec| {
        let s = backdrop_settings(&spec)?;
        let mut bd = jetty_render::Backdrop::new(device, format);
        let frame = backdrop_frame(width, height, None);
        bd.prepare(device, queue, &s, term.theme(), &frame).then_some(bd)
    });
    let (mut two, mut one) = (Vec::new(), Vec::new());
    let (mut two_allocs, mut one_allocs) = (0u64, 0u64);
    for k in 0..600usize {
        if k % 100 == 99 {
            term.feed(b"\r\n$ ");
        } else {
            term.feed(&[b"abcdefghijklmnopqrstuvwxyz"[k % 26]]);
        }
        let t = Instant::now();
        let a0 = alloc_counts().0;
        let snap = term.snapshot();
        let bg = jetty_render::cell_bg_rects(&snap, cw, ch, 0.0, [60, 80, 120]);
        let clear = jetty_render::default_bg_clear(&snap, true);
        if k % 2 == 0 {
            quad.render_clear(device, queue, &view, width, height, &bg, clear);
            text.render_to(device, queue, &view, width, height, &snap, false, 0.0)?;
            two.push(t.elapsed().as_secs_f32() * 1000.0);
            two_allocs += alloc_counts().0 - a0;
        } else {
            let n = quad.upload(device, queue, width, height, &bg);
            let ready = text
                .prepare_grid(device, queue, width, height, &snap, jetty_render::GridOrigin::default(), &jetty_render::GridPaint::default())
                .is_ok();
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if let Some(bd) = &backdrop {
                    bd.draw(&mut pass);
                }
                quad.draw_uploaded(&mut pass, n);
                if ready {
                    text.draw_grid(&mut pass);
                }
            }
            queue.submit(Some(encoder.finish()));
            text.end_grid_frame();
            one.push(t.elapsed().as_secs_f32() * 1000.0);
            one_allocs += alloc_counts().0 - a0;
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    println!(
        "scene         {cols}x{rows} typing, bg quads + glyphs: 2 passes/submits cpu {:.3} ms ({:.0} allocs) | 1 pass/submit cpu {:.3} ms ({:.0} allocs){}",
        mean(&two),
        two_allocs as f64 / two.len() as f64,
        mean(&one),
        one_allocs as f64 / one.len() as f64,
        if backdrop.is_some() { " (1-pass incl. backdrop)" } else { "" }
    );
    Ok(())
}

/// `[backdrop]` settings for a bench spec: a mode (`theme`, `gradient`,
/// `image`) or a pattern name (`stars`, `aurora`, `grid`, `synthwave`).
fn backdrop_settings(spec: &str) -> Option<jetty_render::BackdropSettings> {
    use jetty_render::{BackdropMode, BackdropPattern, BackdropSettings};
    let mut s = BackdropSettings::default();
    match spec {
        "stars" | "aurora" | "grid" | "synthwave" => {
            s.mode = BackdropMode::Pattern;
            s.pattern = BackdropPattern::parse(spec);
        }
        other => s.mode = BackdropMode::parse(other),
    }
    (!s.is_off()).then_some(s)
}

fn backdrop_frame(
    width: u32,
    height: u32,
    image: Option<&std::sync::Arc<jetty_render::GpuImage>>,
) -> jetty_render::BackdropFrame<'_> {
    jetty_render::BackdropFrame {
        width,
        height,
        slide_y: 0.0,
        scroll_px: 0.0,
        dpi: 1.0,
        premultiply: true,
        time: 0.0,
        image,
    }
}

/// GPU cost of the backdrop layer (visuals v2): ONE render pass of [clear +
/// backdrop] against [clear] alone, per variant, at 1920×1200 and 2560×1440.
/// Timed with GPU timestamp queries when the adapter has them (the pass's own
/// execution time), else with the wall clock around submit + wait. The animated
/// aurora's half-res re-bake (a separate submit at ≤ 30 fps) is timed on the
/// wall clock. Runs on its own device so the main bench's device is untouched.
fn bench_backdrop(adapter: &wgpu::Adapter) -> Result<(), Box<dyn std::error::Error>> {
    use jetty_render::{BackdropFit, BackdropMode, BackdropSettings};
    let ts = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("jetty-bench-backdrop"),
        required_features: if ts { wgpu::Features::TIMESTAMP_QUERY } else { wgpu::Features::empty() },
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        ..Default::default()
    }))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let theme = jetty_core::theme_at(jetty_core::theme_index("tokyo_night").unwrap_or(0));
    // A synthetic photo-sized image (deterministic noise), decoded once.
    let (iw, ih) = (2560u32, 1440u32);
    let rgba: Vec<u8> = (0..iw * ih)
        .flat_map(|i| {
            let (x, y) = (i % iw, i / iw);
            let v = ((x ^ y).wrapping_mul(2_654_435_761) >> 24) as u8;
            [v, v / 2 + (y / 6) as u8 / 2, 255 - v, 255]
        })
        .collect();
    let raw = || jetty_render::backdrop_image::RawImage { w: iw, h: ih, rgba: rgba.clone() };
    let sharp = std::sync::Arc::new(
        jetty_render::GpuImage::upload(&device, &queue, &jetty_render::backdrop_image::prepare(raw(), iw, ih, 0.0))
            .ok_or("image upload")?,
    );
    let frosted = std::sync::Arc::new(
        jetty_render::GpuImage::upload(&device, &queue, &jetty_render::backdrop_image::prepare(raw(), iw, ih, 0.5))
            .ok_or("image upload")?,
    );
    let mode = |m: BackdropMode| BackdropSettings { mode: m, ..BackdropSettings::default() };
    type Variant<'a> = (&'a str, Option<BackdropSettings>, Option<&'a std::sync::Arc<jetty_render::GpuImage>>);
    let variants: Vec<Variant> = vec![
        ("clear only", None, None),
        ("theme", Some(mode(BackdropMode::Theme)), None),
        ("gradient", backdrop_settings("gradient"), None),
        ("image cover", Some(mode(BackdropMode::Image)), Some(&sharp)),
        (
            "image tile",
            Some(BackdropSettings { fit: BackdropFit::Tile, ..mode(BackdropMode::Image) }),
            Some(&sharp),
        ),
        ("image frosted", Some(BackdropSettings { blur: 0.5, ..mode(BackdropMode::Image) }), Some(&frosted)),
        ("stars", backdrop_settings("stars"), None),
        ("grid", backdrop_settings("grid"), None),
        ("synthwave", backdrop_settings("synthwave"), None),
        ("aurora", backdrop_settings("aurora"), None),
    ];
    let n = 200usize;
    let period = queue.get_timestamp_period() as f64;
    // The floor every full-screen shaded pass pays on this GPU: a trivial
    // fragment shader writing one constant color (vs. the clear, which is a
    // fast clear and shades nothing). The backdrop's own work is what it adds
    // over THIS.
    let flat_src = "@vertex fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
        var v = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
        return vec4(v[vi], 0.0, 1.0);
    }
    @fragment fn fs() -> @location(0) vec4<f32> { return vec4(0.1, 0.2, 0.3, 1.0); }";
    let flat_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bench-flat"),
        source: wgpu::ShaderSource::Wgsl(flat_src.into()),
    });
    let flat = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bench-flat"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &flat_module,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &flat_module,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    println!(
        "backdrop      GPU per pass [clear + layer], {n} passes back to back (the GPU stays clocked), {}",
        if ts { "timestamp queries" } else { "wall clock / n (no timestamp queries)" }
    );
    for &(w, h) in &[(1920u32, 1200u32), (2560, 1440)] {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench-backdrop-tex"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let qs = ts.then(|| {
            device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("bench-backdrop-ts"),
                ty: wgpu::QueryType::Timestamp,
                count: 2 * n as u32,
            })
        });
        let bytes = 16 * n as u64;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-backdrop-resolve"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-backdrop-readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        // `n` passes of [clear + draw] recorded back to back, one timestamp pair
        // each; (median, p90) ms per pass. Run twice, the first as a warm-up.
        let measure = |draw: &dyn Fn(&mut wgpu::RenderPass<'_>)| -> Result<(f64, f64), Box<dyn std::error::Error>> {
            let mut result = (0.0, 0.0);
            for _round in 0..2 {
                let t = Instant::now();
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                for k in 0..n as u32 {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: qs.as_ref().map(|q| wgpu::RenderPassTimestampWrites {
                            query_set: q,
                            beginning_of_pass_write_index: Some(2 * k),
                            end_of_pass_write_index: Some(2 * k + 1),
                        }),
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    draw(&mut pass);
                }
                if let Some(q) = &qs {
                    encoder.resolve_query_set(q, 0..2 * n as u32, &resolve, 0);
                    encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, bytes);
                }
                queue.submit(Some(encoder.finish()));
                device.poll(wgpu::PollType::wait_indefinitely())?;
                let wall = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
                let mut samples: Vec<f32> = if qs.is_some() {
                    let slice = readback.slice(..);
                    slice.map_async(wgpu::MapMode::Read, |_| {});
                    device.poll(wgpu::PollType::wait_indefinitely())?;
                    let v = {
                        let data = slice.get_mapped_range();
                        (0..n)
                            .map(|k| {
                                let at = |i: usize| u64::from_le_bytes(data[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
                                (at(2 * k + 1).saturating_sub(at(2 * k)) as f64 * period / 1.0e6) as f32
                            })
                            .collect()
                    };
                    readback.unmap();
                    v
                } else {
                    vec![wall as f32]
                };
                samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                result = (percentile(&samples, 50.0) as f64, percentile(&samples, 90.0) as f64);
            }
            Ok(result)
        };
        let (clear, _) = measure(&|_pass| {})?;
        let (floor, floor90) = measure(&|pass| {
            pass.set_pipeline(&flat);
            pass.draw(0..3, 0..1);
        })?;
        println!("  {w}x{h} {:<14} median {clear:6.3} ms (fast clear, nothing shaded)", "clear only");
        println!(
            "  {w}x{h} {:<14} median {floor:6.3} ms | p90 {floor90:6.3} ms (any full-screen shaded pass)",
            "flat shader"
        );
        // Reference: copying a window-sized cached texture (textureLoad, one
        // read + one write per pixel) — what a pre-baked backdrop costs a frame.
        let copy_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bench-copy"),
            source: wgpu::ShaderSource::Wgsl(
                "@group(0) @binding(0) var t: texture_2d<f32>;
                @vertex fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
                    var v = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
                    return vec4(v[vi], 0.0, 1.0);
                }
                @fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
                    return textureLoad(t, vec2<i32>(p.xy), 0);
                }"
                .into(),
            ),
        });
        let copy = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bench-copy"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &copy_module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &copy_module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let src = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench-copy-src"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
        let copy_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &copy.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) }],
        });
        let (copy_ms, copy90) = measure(&|pass| {
            pass.set_pipeline(&copy);
            pass.set_bind_group(0, &copy_bg, &[]);
            pass.draw(0..3, 0..1);
        })?;
        println!(
            "  {w}x{h} {:<14} median {copy_ms:6.3} ms | p90 {copy90:6.3} ms (reference: copy a cached texture)",
            "cached copy"
        );
        for (name, settings, image) in variants.iter().skip(1) {
            let Some(s) = settings else { continue };
            let mut bd = jetty_render::Backdrop::new(&device, format);
            let frame = backdrop_frame(w, h, *image);
            bd.prepare(&device, &queue, s, &theme, &frame);
            // Per frame: the composite copy of the baked cache.
            let (med, p90) = measure(&|pass| bd.draw(pass))?;
            // On a change (resize / theme / settings): one bake, submit + wait.
            let mut bake = Vec::with_capacity(20);
            for k in 0..23 {
                bd.invalidate();
                let t = Instant::now();
                bd.prepare(&device, &queue, s, &theme, &frame);
                device.poll(wgpu::PollType::wait_indefinitely())?;
                if k >= 3 {
                    bake.push(t.elapsed().as_secs_f32() * 1000.0);
                }
            }
            bake.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            println!(
                "  {w}x{h} {name:<14} per frame median {med:6.3} ms | p90 {p90:6.3} ms | +{:6.3} over the flat pass | bake (on change) {:6.3} ms",
                (med - floor).max(0.0),
                percentile(&bake, 50.0)
            );
        }
    }
    println!(
        "              (bake = wall clock incl. submit + wait; paid on a resize / theme / settings change,\n               \
         and by an animated look once per ≤ 30 fps tick — a static backdrop never bakes per frame)"
    );
    Ok(())
}

/// No-GPU subset for CI (`JETTY_BENCH_CPU_ONLY=1`): throughput + snapshot +
/// pipeline_1byte_cpu on a FIXED baseline grid (199×57 — the grid the live bench
/// derives at 1920×1200 @ 16px MesloLGS NF on the reference machine, so the numbers
/// are comparable). Constructs no wgpu instance/adapter/device and no TextLayer.
fn run_cpu_only() -> Result<(), Box<dyn std::error::Error>> {
    // `JETTY_BENCH_GRID=240x70` overrides the fixed baseline grid;
    // `JETTY_BENCH_MIN_CONTRAST=4.5` measures the snapshot with `minimum_contrast` on.
    let (cols, rows) = std::env::var("JETTY_BENCH_GRID")
        .ok()
        .and_then(|g| {
            let (c, r) = g.split_once('x')?;
            Some((c.parse::<usize>().ok()?.clamp(2, 2000), r.parse::<usize>().ok()?.clamp(1, 1000)))
        })
        .unwrap_or((199, 57));

    let mut term = jetty_core::Terminal::new(cols, rows);
    let min_contrast = std::env::var("JETTY_BENCH_MIN_CONTRAST").ok().and_then(|v| v.parse::<f32>().ok());
    if let Some(r) = min_contrast {
        term.set_minimum_contrast(r);
        println!("minimum_contrast {:.2}", term.minimum_contrast());
    }

    // throughput
    let payload = make_payload(50 * 1024 * 1024);
    let t2 = Instant::now();
    feed_chunked(&mut term, &payload);
    let feed_s = t2.elapsed().as_secs_f64();
    let mb = payload.len() as f64 / 1_048_576.0;
    let mbps = mb / feed_s;

    // snapshot
    let mut snap = term.snapshot();
    let n_snap = 500;
    let t3 = Instant::now();
    for _ in 0..n_snap {
        snap = term.snapshot();
    }
    let snap_ms = t3.elapsed().as_secs_f64() * 1000.0 / n_snap as f64;
    let _ = &snap;

    println!("=== Jetty perf bench (CPU-only; no GPU) ===");
    println!("grid          {cols}x{rows} cells (fixed baseline; no display)");
    println!("throughput    {mbps:6.0} MB/s   (fed {mb:.0} MB colored VT in {feed_s:.2}s)");
    println!("snapshot      {snap_ms:8.3} ms/frame  ({:.0}k cells)", (cols * rows) as f64 / 1000.0);
    // With `minimum_contrast` on: an interleaved in-process A/B (same grid, same
    // noise), so its cost is measurable on a busy machine.
    if let Some(r) = min_contrast {
        // Mean over all batches, and the fastest batch (robust to preemption
        // on a loaded machine).
        let (mut off_s, mut on_s) = (0.0f64, 0.0f64);
        let (mut off_min, mut on_min) = (f64::MAX, f64::MAX);
        for _ in 0..200 {
            term.set_minimum_contrast(1.0);
            let t = Instant::now();
            for _ in 0..10 {
                std::hint::black_box(term.snapshot());
            }
            let d = t.elapsed().as_secs_f64();
            off_s += d;
            off_min = off_min.min(d);
            term.set_minimum_contrast(r);
            let t = Instant::now();
            for _ in 0..10 {
                std::hint::black_box(term.snapshot());
            }
            let d = t.elapsed().as_secs_f64();
            on_s += d;
            on_min = on_min.min(d);
        }
        println!(
            "min_contrast  mean off {:.4} / on {:.4} ms ({:+.1}%), best batch off {:.4} / on {:.4} ms ({:+.1}%)  (interleaved A/B, 2000 snapshots each)",
            off_s / 2.0,
            on_s / 2.0,
            (on_s / off_s - 1.0) * 100.0,
            off_min * 100.0,
            on_min * 100.0,
            (on_min / off_min - 1.0) * 100.0
        );
    }
    print_pipeline_1byte_cpu(&mut term);
    Ok(())
}
