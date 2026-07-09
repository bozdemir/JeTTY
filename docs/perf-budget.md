# Jetty Performance Budget

> Jetty = **Jet**. Raw speed is the #1 priority, above features. The goal is to be
> **faster than the terminals on the market** (alacritty, kitty, foot, Konsole/VTE,
> wezterm). This file is the gate: a change that regresses a budgeted metric is a
> bug, not a tradeoff.

## How to measure (reproducible)

```bash
# Hot-path numbers (headless, no window): GPU init, throughput, snapshot, render,
# and the pipeline_1byte_cpu compute proxy.
cargo run --release -p jetty-app --bin jetty-bench

# CI / no-GPU subset (never constructs wgpu): throughput + snapshot +
# pipeline_1byte_cpu on a fixed baseline grid. This is what the CI perf-report runs.
JETTY_BENCH_CPU_ONLY=1 target/release/jetty-bench

# Live metrics on the running app: exec→first-frame cold start, input latency
# (keypress→glyph, percentiles), and idle RSS. Zero cost unless the flag is set.
JETTY_PERF_LOG=1 target/release/jetty
```

`JETTY_PERF_LOG=1` prints (to stderr):
- `cold-start … = N ms` once, at the first presented frame. On Linux this is a
  **genuine exec→first-frame** delta (from `/proc/self/stat` starttime vs
  `/proc/uptime`, so it INCLUDES loader / pre-`main` time; ~10 ms resolution). Where
  that basis is unavailable it falls back to a `main()`→first-frame `Instant` and
  says so.
- `input-latency n=… display=…Hz` every 64 quiescent-prompt keystrokes (and once on
  exit), as two honestly-labelled numbers:
  `keypress→frame-ready` (app + shell-echo round-trip, **excl.** the vsync-acquire
  wait, GPU submit and scanout) and `keypress→pre-present` (**+** vsync-acquire +
  GPU submit; still excl. scanout). Percentiles are linear-interpolated (p99 is never
  silently the max); `n` and the display refresh are printed so the vsync component
  is interpretable. Sampled only at a quiescent prompt so a streaming tab can't
  record a near-zero non-echo latency.
- `idle RSS … MB` once, when the app first settles to idle (resident set incl. shared
  pages — RSS, not PSS).

Everything above is **zero-cost when the flag is unset**: `perf.on` is a single bool
read once at startup; the per-byte drain path is untouched, and the present path pays
one predictable-false branch. See `crates/jetty-app/src/perf.rs`.

Live metrics (idle CPU, live frame ms) are also visible on the running app's HUD —
see "Live metrics" below.

Baseline machine: Intel Core Ultra 9 275HX (24 threads), 62 GiB RAM,
Intel Arc (Arrow Lake) iGPU via Vulkan (LowPower — the NVIDIA dGPU is avoided on
purpose), 1920×1200 @ 59.95 Hz. Compared against the terminals installed here:
Konsole 23.08.5, GNOME Terminal / VTE 0.76.

## The budget

> Numbers below are **real, measured on this machine** (v0.17 release build, LowPower
> iGPU, headless `jetty-bench` — see §"How to measure"). Ranges reflect run-to-run
> spread across ~10 runs. The three **live** metrics (input latency, exec→first-frame,
> idle RSS) are now **instrumented and unit-tested** (`JETTY_PERF_LOG=1`); their exact
> figures are emitted on a live run and are intentionally NOT transcribed here as
> fixed numbers (they depend on display refresh and typing cadence — read them live).

| Metric | Market reference (fastest class) | Jetty **target** (gate) | Jetty **current** (measured) | Status |
|---|---|---|---|---|
| **Frame render** (offscreen, ~199×57 @ 1920×1200, 16px) | 60 Hz = 16.7 ms; 144 Hz = 6.9 ms/frame | ≤ **6.9 ms** (144 Hz-ready); hard ≤ 16.7 ms | **~1.1–1.8 ms** offscreen (this build: cpu ~0.5–0.8 + gpu ~0.6–1.0). Live app is vsync-capped (`PresentMode::Fifo`) | ✅ meets 144 Hz |
| **Idle CPU** | ~0 % (event-driven terminals) | **0 %** when nothing changes | ~0 % (damage-driven redraw) | ✅ |
| **Per-frame CPU** (snapshot, ~11k cells; grid computed from cell metrics) | n/a | ≤ **1 ms** | **~0.08 ms** (0.072–0.087) | ✅ ~12× under |
| **Throughput** (parse+grid, colored VT) | alacritty class: very high; VTE/Konsole: lower | ≥ **150 MB/s**; stretch ≥ 300 | **~118 MB/s** (median; 105–137 observed) — ⚠ **the old "154" is not reproducible** (see correction note) | ⚠ **below target** — investigate before re-asserting ≥150 |
| **Pipeline compute** (`pipeline_1byte_cpu`: feed 1 byte → snapshot, CPU only) — **NOT input latency** | n/a | informational | p50 **~0.07 ms** (min ~0.068, p99 ~0.09; n=2000). Excludes PTY write + shell-echo round-trip + reader-thread wake + winit + compositor/display | — informational proxy |
| **Cold start** (process exec → first frame) | foot ~40–60 ms; alacritty ~100–300 ms | < **150 ms**; stretch < 80 ms | `gpu_init` **warm ~85–94 ms / cold ~278 ms** (adapter+device only — a *subcomponent*; `text_init` warm ~24–36 / cold ~750 ms overlaps on a worker thread). End-to-end **exec→first-frame now instrumented** (`JETTY_PERF_LOG=1`, `/proc`-based on Linux, incl. pre-`main`) | ✅ subcomponent meets; end-to-end instrumented (read live) |
| **Input latency** (keypress → glyph) | foot ≈ 1 frame; the latency leader | ≤ **1 frame** added (< 5 ms beyond display) | **instrumented** (`JETTY_PERF_LOG=1`): app-side `keypress→frame-ready` (no vsync) + `keypress→pre-present` (vsync-throttled), quiescent-prompt, percentiles + refresh rate. Not the bench proxy above | ✅ instrumented (read live) |
| **Idle RSS** | alacritty ~30–50 MB; foot lower | < **80 MB** | **instrumented** (`JETTY_PERF_LOG=1` → `idle RSS … MB`, via `sysinfo`; RSS incl. shared pages, not PSS) | ✅ instrumented (read live) |
| **Binary size** | — | informational | 15 MB (release) | — |

> **⚠ Throughput correction (v0.17).** Earlier revisions of this file claimed
> **154 MB/s**. On the current release binary the same `jetty-bench` throughput test
> measures a **median of ~118 MB/s** (105–137 across runs) — the 154 figure is **not
> reproduced**. It is unclear whether 154 was a regression since, a different
> measurement basis, or an error; it is corrected here rather than re-published. The
> ≥150 MB/s target is retained but **currently unmet on this binary** (OPEN — see the
> TODO list). No unverified figure is shipped.

## Where we lead vs. match vs. must improve

- **Lead (architecture already gives us the edge):**
  - *Idle CPU = 0* — `drain_pty()` reports whether anything changed; idle frames
    are never drawn. Many terminals still wake for cursor blink.
  - *Input latency* — the PTY reader wakes the event loop within ~1 ms of bytes
    arriving (no polling tick on the keystroke path), and the render pipeline is
    one snapshot + one draw. This is the foot-class design; as of v0.17 it is
    **instrumented live** (`JETTY_PERF_LOG=1`) rather than only asserted — two
    honestly-labelled numbers (app-compute-to-frame-ready, and to-pre-present with
    the vsync-acquire wait), sampled at a quiescent prompt, with percentiles.
  - *Per-frame CPU* — snapshot is ~80 µs; render is GPU-bound.
- **Match:**
  - *Throughput / frame time* — we use alacritty_terminal's parser, so raw
    parse speed tracks alacritty; render at ~1.1–1.8 ms/full-frame clears 144 Hz.
    Both already beat VTE-based Konsole/GNOME Terminal on this machine. (Throughput
    currently measures ~118 MB/s — below the ≥150 target; see the correction note.)
- **Fixed (was the one red metric):**
  - *Cold start* — gpu_init went **224 ms → ~85 ms warm** by restricting the wgpu
    instance to the **Vulkan backend** (the default probed every backend), the
    single biggest win. On top of that, the **FontSystem font-DB scan and the
    PTY fork now run on worker threads** that overlap the remaining device
    acquisition, and **F9 global-hotkey registration moved off the main thread**;
    `[profile.release] lto = "thin"` trims runtime. `gpu_init` measures warm
    ~85–94 ms (cold ~278 ms, first run of a cold cache). The **end-to-end
    exec→first-frame** number (which the gpu_init figure is only a subcomponent of)
    is now instrumented via `JETTY_PERF_LOG=1` — a genuine `/proc`-based exec delta
    on Linux that includes pre-`main` loader time.
  - *Remaining headroom:* a CPU-painted first frame before GPU warmup could
    shave perceived latency further, but it is no longer the bottleneck.

## Gates (CI-style rules)

1. `jetty-bench` render ≤ 6.9 ms/frame and snapshot ≤ 1 ms/frame on the baseline.
2. Throughput ≥ 150 MB/s. *(Currently unmet on this binary — measures ~118; see the
   correction note. Treated as an OPEN investigation, not a silently-passed gate.)*
3. Idle redraw stays damage-driven (no unconditional per-tick `request_redraw`); the only permitted idle wake is the perf-HUD one-shot `WaitUntil`, which must fire at most once per activity burst.
4. Nothing added to the keystroke → PTY → render path that isn't strictly needed.
   The `JETTY_PERF_LOG=1` instrumentation obeys this: when the flag is unset the
   per-byte drain path is byte-identical and the present path pays one
   predictable-false bool branch (verified — see `crates/jetty-app/src/perf.rs`).
5. Cold start trends **down**, never up; target < 150 ms.
6. **CI perf-report (informational, v0.17).** `.github/workflows/ci.yml` runs
   `JETTY_BENCH_CPU_ONLY=1 scripts/perf-report.sh` (best-of-5) and prints throughput
   + snapshot + `pipeline_1byte_cpu`. It is **non-blocking** (`continue-on-error`,
   and the script always exits 0): hard floors calibrated to a fast dev machine would
   false-fail on a slower, sometimes sustained-contended shared GitHub runner. Hard
   gating is a **v0.18 follow-up** — set floors at ~50 % of the CI runner's observed
   minimum after watching its real distribution across many runs. CPU-only avoids
   GPU-availability / software-rasterizer timing variance on runners (it is
   display-independent, not a claim that the GPU bench "crashes" there).

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

> **Note on the render figure:** this is the headless `jetty-bench` per-frame render
> to an offscreen texture (no present mode) — **~1.1–1.8 ms** on this build. The live
> app presents with `PresentMode::Fifo` (vsync), so on-screen fps tracks the display
> refresh (~60 Hz here); that headroom is what makes 144 Hz displays attainable
> without dropping frames. (Earlier revisions quoted 5.5 ms; the current binary
> measures faster.)

Now instrumented (v0.17 — read live with `JETTY_PERF_LOG=1`, unit-tested in
`crates/jetty-app/src/perf.rs`):
- **Input latency**: `keypress→frame-ready` + `keypress→pre-present` percentiles,
  quiescent-prompt only, with the display refresh printed. (A high-FPS
  camera/Typometer capture would additionally cover winit-in + scanout, which the
  app-side stamps deliberately exclude — a possible future cross-check.)
- **Idle RSS**: `idle RSS … MB` sampled once at idle settle via `sysinfo`.
- **Cold start (end-to-end)**: genuine `exec→first-frame` (Linux `/proc`, incl.
  pre-`main`); the one-shot line prints at the first present.

Still genuinely unmeasured (TODO):
- **Throughput vs. the ≥150 target**: currently ~118 MB/s — resolve the 154
  discrepancy (regression? basis? error?) before re-asserting the claim.
- **vs. market**: same `cat 50MB` / `time seq` workload through Jetty vs. Konsole
  vs. GNOME Terminal, wall-clock compared.
