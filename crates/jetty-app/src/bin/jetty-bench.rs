/// Headless performance benchmark for Jetty's hot path — NO window/display.
///
/// Measures the numbers that define the perf budget (docs/perf-budget.md):
///   - gpu_init:   time to acquire the wgpu adapter + device (startup-dominant)
///   - throughput: MB/s feeding typical colored VT output through the parser+grid
///   - snapshot:   per-frame CPU cost of building a GridSnapshot
///   - render:     per-frame GPU+CPU cost of rendering a full screen offscreen
///
/// Run: cargo run --release -p jetty-app --bin jetty-bench
use std::time::Instant;

use jetty_render::TextLayer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let line: &[u8] = b"\x1b[1;32muser@host\x1b[0m:\x1b[34m~/src/jetty\x1b[0m$ \x1b[33mcargo build\x1b[0m --release --workspace   \x1b[2m# building 4 crates\x1b[0m\r\n";
    let target = 50 * 1024 * 1024usize;
    let mut payload = Vec::with_capacity(target + line.len());
    while payload.len() < target {
        payload.extend_from_slice(line);
    }
    let chunk = 65536;
    let t2 = Instant::now();
    let mut i = 0;
    while i < payload.len() {
        let end = (i + chunk).min(payload.len());
        term.feed(&payload[i..end]);
        i = end;
    }
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
    Ok(())
}
