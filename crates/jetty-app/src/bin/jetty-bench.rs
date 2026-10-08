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

    println!("=== Jetty perf bench ({} {:?}) ===", adapter.get_info().name, adapter.get_info().backend);
    println!("grid          {cols}x{rows} cells (cell {cw:.1}x{ch:.1}px) @ {width}x{height}");
    println!("gpu_init      {gpu_init_ms:6.1} ms    (adapter + device acquisition)");
    println!("text_init     {text_init_ms:6.1} ms    (font system + atlas)");
    println!("throughput    {mbps:6.0} MB/s   (fed {mb:.0} MB colored VT in {feed_s:.2}s)");
    println!("snapshot      {snap_ms:8.3} ms/frame  ({:.0}k cells)", (cols * rows) as f64 / 1000.0);
    println!("render        {frame_ms:8.3} ms/frame  ({:.0} fps cap; SAME snapshot every frame → no re-shape)", 1000.0 / frame_ms);
    println!("  ├─ cpu prep {cpu_ms:8.3} ms/frame  (build spans + atlas prepare + submit; shaping skipped — unchanged grid)");
    println!("  └─ gpu exec {gpu_ms:8.3} ms/frame  (device.poll wait for GPU completion)");
    print_pipeline_1byte_cpu(&mut term);
    print_font_db_costs(&text);
    bench_frames(&device, &queue, format, font_size)?;
    bench_post_pass(&device, &queue, format, font_size)?;
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
                       step: &mut dyn FnMut(&mut jetty_core::Terminal, usize)|
         -> Result<(), Box<dyn std::error::Error>> {
            let n = 300usize;
            let mut cpu = Vec::with_capacity(n);
            let mut total = 0.0f64;
            for k in 0..n {
                step(&mut term, k);
                let t = Instant::now();
                let snap = term.snapshot();
                text.render_to(device, queue, &view, width, height, &snap, true, 0.0)?;
                cpu.push(t.elapsed().as_secs_f32() * 1000.0);
                device.poll(wgpu::PollType::wait_indefinitely())?;
                total += t.elapsed().as_secs_f64() * 1000.0;
            }
            let mean = cpu.iter().sum::<f32>() / n as f32;
            cpu.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            println!(
                "  {cols:3}x{rows:<3} {label:<7} cpu {mean:7.3} ms | total {:7.3} ms | p99 cpu {:7.3} ms",
                total / n as f64,
                percentile(&cpu, 99.0)
            );
            Ok(())
        };
        run("static", &mut |_t, _k| {})?;
        run("typing", &mut |t, k| {
            // Wrap like a long command line would, but stay on the prompt row most
            // of the time: a CR+LF every 100 chars starts a fresh prompt line.
            if k % 100 == 99 {
                t.feed(b"\r\n$ ");
            } else {
                t.feed(&[b"abcdefghijklmnopqrstuvwxyz"[k % 26]]);
            }
        })?;
        let mut line = 1_000_000usize;
        run("scroll", &mut |t, _k| {
            line += 1;
            t.feed(&numbered_line(line));
        })?;
    }
    bench_scene_passes(device, queue, format, font_size)
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
    let (mut two, mut one) = (Vec::new(), Vec::new());
    for k in 0..600usize {
        if k % 100 == 99 {
            term.feed(b"\r\n$ ");
        } else {
            term.feed(&[b"abcdefghijklmnopqrstuvwxyz"[k % 26]]);
        }
        let t = Instant::now();
        let snap = term.snapshot();
        let bg = jetty_render::cell_bg_rects(&snap, cw, ch, 0.0, [60, 80, 120]);
        let clear = jetty_render::default_bg_clear(&snap, true);
        if k % 2 == 0 {
            quad.render_clear(device, queue, &view, width, height, &bg, clear);
            text.render_to(device, queue, &view, width, height, &snap, false, 0.0)?;
            two.push(t.elapsed().as_secs_f32() * 1000.0);
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
                quad.draw_uploaded(&mut pass, n);
                if ready {
                    text.draw_grid(&mut pass);
                }
            }
            queue.submit(Some(encoder.finish()));
            text.end_grid_frame();
            one.push(t.elapsed().as_secs_f32() * 1000.0);
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    println!(
        "scene         {cols}x{rows} typing, bg quads + glyphs: 2 passes/submits cpu {:.3} ms | 1 pass/submit cpu {:.3} ms",
        mean(&two),
        mean(&one)
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
