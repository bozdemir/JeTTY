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
use std::time::Instant;

use jetty_app::perf::{env_enabled, percentile};
use jetty_render::TextLayer;

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

/// Feed `payload` into `term` in 64 KiB chunks (matches the live PTY drain shape).
fn feed_chunked(term: &mut jetty_core::Terminal, payload: &[u8]) {
    let chunk = 65536;
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
    let t0 = Instant::now();
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
    let mut instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: power,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))?
        }
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("jetty-bench"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        ..Default::default()
    }))?;
    let gpu_init_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let t1 = Instant::now();
    let mut text = TextLayer::new_with_family(&device, &queue, format, font_size, "MesloLGS NF");
    let text_init_ms = t1.elapsed().as_secs_f64() * 1000.0;

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

    // Split each frame into CPU-prep (build spans + shape + glyphon prepare +
    // queue.submit, all inside render_to) vs GPU-execute (the device.poll wait for
    // the GPU to finish). This shows where the budget actually goes — JeTTY's grid
    // render is CPU-prep-dominated (text shaping + atlas prep), so the GPU portion
    // is small and a faster GPU barely moves the total.
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

    println!("=== Jetty perf bench ({} {:?}) ===", adapter.get_info().name, adapter.get_info().backend);
    println!("grid          {cols}x{rows} cells (cell {cw:.1}x{ch:.1}px) @ {width}x{height}");
    println!("gpu_init      {gpu_init_ms:6.1} ms    (adapter + device acquisition)");
    println!("text_init     {text_init_ms:6.1} ms    (font system + atlas)");
    println!("throughput    {mbps:6.0} MB/s   (fed {mb:.0} MB colored VT in {feed_s:.2}s)");
    println!("snapshot      {snap_ms:8.3} ms/frame  ({:.0}k cells)", (cols * rows) as f64 / 1000.0);
    println!("render        {frame_ms:8.3} ms/frame  ({:.0} fps cap)", 1000.0 / frame_ms);
    println!("  ├─ cpu prep {cpu_ms:8.3} ms/frame  (build spans + shape + atlas prepare + submit)");
    println!("  └─ gpu exec {gpu_ms:8.3} ms/frame  (device.poll wait for GPU completion)");
    print_pipeline_1byte_cpu(&mut term);
    Ok(())
}

/// No-GPU subset for CI (`JETTY_BENCH_CPU_ONLY=1`): throughput + snapshot +
/// pipeline_1byte_cpu on a FIXED baseline grid (199×57 — the grid the live bench
/// derives at 1920×1200 @ 16px MesloLGS NF on the reference machine, so the numbers
/// are comparable). Constructs no wgpu instance/adapter/device and no TextLayer.
fn run_cpu_only() -> Result<(), Box<dyn std::error::Error>> {
    let (cols, rows) = (199usize, 57usize);

    let mut term = jetty_core::Terminal::new(cols, rows);

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
    print_pipeline_1byte_cpu(&mut term);
    Ok(())
}
