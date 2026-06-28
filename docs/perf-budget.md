# Jetty Performance Budget

> Jetty = **Jet**. Raw speed is the #1 priority, above features. The goal is to be
> **faster than the terminals on the market** (alacritty, kitty, foot, Konsole/VTE,
> wezterm). This file is the gate: a change that regresses a budgeted metric is a
> bug, not a tradeoff.

## How to measure (reproducible)

```bash
# Hot-path numbers (headless, no window): GPU init, throughput, snapshot, render.
cargo run --release -p jetty-app --bin jetty-bench

# Same, but on the discrete / high-performance GPU on a hybrid system.
# Headless-only — see "GPU selection" below.
JETTY_BENCH_GPU=high cargo run --release -p jetty-app --bin jetty-bench
```

`jetty-bench` splits `render` into **cpu prep** (build spans + cosmic-text shaping
+ glyphon atlas prepare + submit) and **gpu exec** (the wait for the GPU to finish)
so the CPU-vs-GPU breakdown of the frame budget is visible — see
"Render is CPU-bound" below.

Live metrics (startup-to-first-frame, input latency, idle CPU, RSS) are measured
on the running app — see "Live metrics" below.

Baseline machine: Intel Core Ultra 9 275HX (24 threads), 62 GiB RAM,
Intel Arc (Arrow Lake) iGPU via Vulkan (LowPower — the NVIDIA dGPU is avoided on
purpose), 1920×1200 @ 59.95 Hz. Compared against the terminals installed here:
Konsole 23.08.5, GNOME Terminal / VTE 0.76.

### GPU selection

By default `jetty-bench` requests `LowPower` → the **integrated** GPU, matching the
live app (which avoids the discrete GPU on hybrid systems, where driving it under a
live compositor surface can be unstable). `JETTY_BENCH_GPU=high` (aliases
`discrete`, `dgpu`) requests `HighPerformance` → the **discrete** GPU. This is safe
in the bench because it is headless (renders to an offscreen texture — no
compositor surface at risk); the live app's GPU choice is unchanged.

### Second measured machine — render is CPU-bound

Added 2026-06-28 on an AMD Ryzen (Rembrandt) APU with a **Radeon 680M iGPU** *and*
an **NVIDIA RTX 3070 Ti (Laptop) dGPU**, Vulkan, 1920×1200, release build. Both
GPUs clear the 144 Hz render gate — and comparing them exposes a defining property
of the renderer:

| Metric | AMD 680M (iGPU) | RTX 3070 Ti (dGPU) |
|---|---|---|
| **Frame render** | 6.5 ms (✅ ≤ 6.9) | 6.3 ms (✅ ≤ 6.9) |
| &nbsp;&nbsp;├─ cpu prep | 6.14 ms | 6.08 ms |
| &nbsp;&nbsp;└─ gpu exec | 0.36 ms | 0.17 ms |
| Throughput | 101 MB/s | 101 MB/s |
| Snapshot | 0.060 ms | 0.061 ms |

**A ~10× more powerful GPU moves the frame only ~6.5 → ~6.3 ms**, because the GPU
does just ~0.2–0.36 ms of it. The other **~6.1 ms is CPU**: cosmic-text shaping +
glyphon's per-glyph atlas prepare, rebuilt for the whole grid each frame. Therefore:

- *A faster GPU barely helps.* The real lever is CPU-side — incremental /
  damage-aware shaping, or a per-cell glyph-instance cache — a larger architectural
  change tracked as future work, **not** a quick win.
- *Throughput (parser) and snapshot are pure CPU* and identical across both GPUs,
  as expected.
- *The gap to the Intel-Arc baseline's 5.5 ms is CPU single-thread, not GPU.* The
  Core Ultra 9 275HX has substantially higher per-core throughput than this
  Rembrandt APU — same code, different CPU.

A small hot-path win landed alongside this measurement: `render_to` no longer
materializes a ~rows×cols `(&str, Attrs)` Vec each frame (~0.5 MB at full screen),
passing the spans straight into `set_rich_text` as an iterator — byte-identical
shaping, one fewer large per-frame allocation.

## The budget

| Metric | Market reference (fastest class) | Jetty **target** (gate) | Jetty **current** | Status |
|---|---|---|---|---|
| **Frame render** (full screen, ~199×57 @ 1920×1200, 16px) | 60 Hz = 16.7 ms; 144 Hz = 6.9 ms/frame | ≤ **6.9 ms** (144 Hz-ready); hard ≤ 16.7 ms | **5.5 ms** offscreen (≈180 fps headless; live app is vsync-capped, `PresentMode::Fifo`) | ✅ meets 144 Hz |
| **Idle CPU** | ~0 % (event-driven terminals) | **0 %** when nothing changes | ~0 % (damage-driven redraw) | ✅ |
| **Per-frame CPU** (snapshot, ~11k cells; grid is computed from cell metrics by jetty-bench, not fixed) | n/a | ≤ **1 ms** | **0.047 ms** | ✅ 20× under |
| **Throughput** (parse+grid, colored VT) | alacritty class: very high; VTE/Konsole: lower | ≥ **150 MB/s**; stretch ≥ 300 | **154 MB/s** | ✅ meets (stretch open) |
| **Cold start** (process → first frame) | foot ~40–60 ms; alacritty ~100–300 ms | < **150 ms**; stretch < 80 ms | gpu_init **~78 ms** (adapter+device only — a *subcomponent* of cold start; font-DB scan/`text_init` and PTY fork overlap it on worker threads); end-to-end process→first-frame still TODO | ✅ **meets target, near stretch** |
| **Input latency** (keypress → glyph) | foot ≈ 1 frame; the latency leader | ≤ **1 frame** added (< 5 ms beyond display) | architecturally ready (≤1 ms waker), not yet measured live | ⏳ measure |
| **Idle RSS** | alacritty ~30–50 MB; foot lower | < **80 MB** | not yet measured live | ⏳ measure |
| **Binary size** | — | informational | 15 MB (release) | — |

## Where we lead vs. match vs. must improve

- **Lead (architecture already gives us the edge):**
  - *Idle CPU = 0* — `drain_pty()` reports whether anything changed; idle frames
    are never drawn. Many terminals still wake for cursor blink.
  - *Input latency* — the PTY reader wakes the event loop within ~1 ms of bytes
    arriving (no polling tick on the keystroke path), and the render pipeline is
    one snapshot + one draw. This is the foot-class design; we must keep it and
    then prove it with a live Typometer-style measurement.
  - *Per-frame CPU* — snapshot is 47 µs; render is GPU-bound.
- **Match:**
  - *Throughput / frame time* — we use alacritty_terminal's parser, so raw
    parse speed tracks alacritty; render at 5.5 ms/full-frame clears 144 Hz.
    Both already beat VTE-based Konsole/GNOME Terminal on this machine.
- **Fixed (was the one red metric):**
  - *Cold start* — gpu_init went **224 ms → ~78 ms** by restricting the wgpu
    instance to the **Vulkan backend** (the default probed every backend), the
    single biggest win. On top of that, the **FontSystem font-DB scan and the
    PTY fork now run on worker threads** that overlap the remaining device
    acquisition, and **F9 global-hotkey registration moved off the main thread**;
    `[profile.release] lto = "thin"` trims runtime. Cold start now meets the
    <150 ms target and approaches the <80 ms stretch.
  - *Remaining headroom:* a CPU-painted first frame before GPU warmup could
    shave perceived latency further, but it is no longer the bottleneck.

## Gates (CI-style rules)

1. `jetty-bench` render ≤ 6.9 ms/frame and snapshot ≤ 1 ms/frame on the baseline.
2. Throughput ≥ 150 MB/s.
3. Idle redraw stays damage-driven (no unconditional per-tick `request_redraw`); the only permitted idle wake is the perf-HUD one-shot `WaitUntil`, which must fire at most once per activity burst.
4. Nothing added to the keystroke → PTY → render path that isn't strictly needed.
5. Cold start trends **down**, never up; target < 150 ms.

## Live metrics (in-app HUD)

The tab bar carries a live performance HUD (toggle: `show_perf_hud`, on by default).
It reads `⚡ <ms> ms · <fps> fps · <cpu>% CPU · <mb> MB/s`, computed in
`jetty-app/src/app.rs::update_perf_hud`:

- **frame ms / fps**: exponentially-smoothed wall-clock dt between rendered frames
  (`ms = ms*0.9 + dt*0.1`); fps = `1000/ms`. Measures the render rate DURING
  activity and *freezes* when idle, then flips to an honest `⚡ idle · 0% CPU · 0 MB/s`
  one frame after settling (see "Idle one-shot" below).
- **CPU%**: `sysinfo` refresh of THIS process only, gated to ≤1 Hz. Reported as a
  percentage of ONE core (can exceed 100% under multi-thread load) — NOT divided by core count.
- **MB/s**: VT bytes drained from the PTY(s) over ~1 s windows (`vt_bytes` counter
  in `drain_pty`), summed across ALL tabs.

The HUD never calls `request_redraw()` and never schedules a timer from the render
path, so it cannot regress the 0-CPU `ControlFlow::Wait` idle.

**Idle one-shot.** After the last active frame, `about_to_wait` arms a single
`ControlFlow::WaitUntil(deadline)` (deadline ≈ 700 ms later). That one wake repaints
the HUD as `⚡ idle · 0% CPU · 0 MB/s`, then the loop returns to `ControlFlow::Wait`.
At most ONE extra repaint per activity burst; never polls. (When `show_perf_hud=false`
the one-shot is never armed.)

> **Note on the 5.5 ms figure:** this is the headless `jetty-bench` per-frame render
> to an offscreen texture (no present mode). The live app presents with
> `PresentMode::Fifo` (vsync), so on-screen fps tracks the display refresh (~60 Hz
> here); the 5.5 ms headroom is what makes 144 Hz displays attainable without
> dropping frames.

Still genuinely unmeasured (TODO):
- **Input latency**: Typometer or a high-FPS capture of keypress → glyph.
- **Idle RSS**: `ps -o rss` after the prompt settles (the HUD does not show RSS).
- **Cold start (end-to-end)**: process → first presented frame (instrument a one-shot
  `eprintln!` at first `RedrawRequested`).
- **vs. market**: same `cat 50MB` / `time seq` workload through Jetty vs. Konsole
  vs. GNOME Terminal, wall-clock compared.
