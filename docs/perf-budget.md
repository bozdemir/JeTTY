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
#   JETTY_BENCH_GRID=240x70 picks the grid.

# One section only, for quick interleaved A/B runs of two builds:
JETTY_BENCH_ONLY=gpu_init target/release/jetty-bench  # instance/adapter/device split,
                                                       # mapped driver libs, RSS
JETTY_BENCH_ONLY=frames   target/release/jetty-bench  # per-frame table + scene + chrome
JETTY_BENCH_ONLY=backdrop target/release/jetty-bench
#   JETTY_BENCH_NO_VK_FILTER=1: GPU init without the startup Vulkan driver filter.
JETTY_BENCH_ONLY=first_frame target/release/jetty-bench  # cold start's text layers +
                                                          # first-frame effect pipelines
#   JETTY_BENCH_FIRST_FRAME=serial: the pre-0.30 order. One measurement per process
#   (the driver caches pipelines in memory): alternate many runs of each.

# Live metrics on the running app: exec→first-frame cold start, input latency
# (keypress→glyph, percentiles), and idle RSS. Zero cost unless the flag is set.
JETTY_PERF_LOG=1 target/release/jetty
```

`jetty-bench` runs on a counting global allocator: the frame tables print
allocations and KB per frame next to the CPU time (the hot path should allocate
next to nothing; every render pass + submit costs ~65 allocations in wgpu). The
throughput test feeds 8 KiB chunks — the PTY reader's read size, so the live
drain's shape (4 / 8 / 64 KiB chunks measure 154 / 151 / 146 MB/s on a quiet
machine: smaller chunks stay in cache).

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
| **Idle CPU** | ~0 % (event-driven terminals) | **0 %** when nothing changes | **0 wakeups / 10 s**, shown, hidden or behind another window (until 2026-10-09 the X11 hotkey thread polled at 20 Hz — see §"Performance pass") | ✅ |
| **Per-frame CPU** (snapshot, ~11k cells; grid computed from cell metrics) | n/a | ≤ **1 ms** | **~0.037 ms** at 199×57, 0.051 ms at 240×70, 0.18 ms at 480×135 (2026-10-09; was 0.076 / 0.109 / 0.41) | ✅ ~27× under |
| **Throughput** (parse+grid, colored VT) | alacritty class: very high; VTE/Konsole: lower | ≥ **150 MB/s**; stretch ≥ 300 | **~146–150 MB/s** quiet machine, 8 KiB chunks as the live drain feeds (the same build fed 64 KiB chunks: ~141). Unchanged parser cost — see the correction note | ⚠ at the target, not above it |
| **Pipeline compute** (`pipeline_1byte_cpu`: feed 1 byte → snapshot, CPU only) — **NOT input latency** | n/a | informational | p50 **~0.035 ms** at 199×57 (2026-10-09; was ~0.075). Excludes PTY write + shell-echo round-trip + reader-thread wake + winit + compositor/display | — informational proxy |
| **Cold start** (process exec → first frame) | foot ~40–60 ms; alacritty ~100–300 ms | < **150 ms**; stretch < 80 ms | `gpu_init` **~20 ms** quiet machine (2026-10-09, startup Vulkan driver filter; every installed driver: ~74 ms) — adapter+device only, a *subcomponent*; `text_init` warm ~24–36 ms, of which only the font-DB scan overlaps on a worker thread — the text layers (atlas, pipelines, font loads) are built on the UI thread before the first frame (`jetty-bench` prints the split). End-to-end **exec→first-frame instrumented** (`JETTY_PERF_LOG=1`, `/proc`-based on Linux, incl. pre-`main`) | ✅ subcomponent meets; end-to-end instrumented (read live) |
| **Input latency** (keypress → glyph) | foot ≈ 1 frame; the latency leader | ≤ **1 frame** added (< 5 ms beyond display) | **instrumented** (`JETTY_PERF_LOG=1`): app-side `keypress→frame-ready` (no vsync) + `keypress→pre-present` (vsync-throttled), quiescent-prompt, percentiles + refresh rate. Not the bench proxy above | ✅ instrumented (read live) |
| **Idle RSS** | alacritty ~30–50 MB; foot lower | < **80 MB** | **instrumented** (`JETTY_PERF_LOG=1` → `idle RSS … MB`, via `sysinfo`; RSS incl. shared pages, not PSS). The startup driver filter keeps ~90 MB of unused Vulkan drivers out of it on a hybrid laptop (`jetty-bench` right after the GPU block: 106 → 15 MiB) | ✅ instrumented (read live) |
| **Binary size** | — | informational | 20 MB (release build, symbols kept — what the tarball and AppImage ship; 17.6 MB stripped), 2026-10-09 | — |

> **⚠ Throughput correction (v0.17).** Earlier revisions of this file claimed
> **154 MB/s**. On the current release binary the same `jetty-bench` throughput test
> measures a **median of ~118 MB/s** (105–137 across runs) — the 154 figure is **not
> reproduced**. It is unclear whether 154 was a regression since, a different
> measurement basis, or an error; it is corrected here rather than re-published. The
> ≥150 MB/s target is retained but **currently unmet on this binary** (OPEN — see the
> TODO list). No unverified figure is shipped.
>
> **2026-10-09:** on a quiet machine the same parser measures **~141 MB/s** with
> 64 KiB chunks (the visuals-v2 round-robin agrees: 141/142) and **~150 MB/s** with
> the 8 KiB chunks the live drain actually feeds — the bench now uses those. The
> parser's own cost did not change; the ~118 above was measured under load. 150 is
> reached, not beaten: the target stays open as a "match alacritty" item.

### Visuals v2 vs v0.26.1 (2026-10-09)

The overnight visuals program (themes, padding, built-in glyphs, backdrop, effects,
cursor, tabs, Settings) was checked against v0.26.1 with a **round-robin** of eleven
`jetty-bench` binaries (v0.26.1, nine merge points, HEAD), four rounds, medians,
Intel ARL iGPU, quiet machine:

| Metric (CPU ms/frame) | v0.26.1 | HEAD | Δ |
|---|---|---|---|
| 240×70 typing | 0.499 | 0.486 | −0.013 |
| 240×70 scroll | 0.502 | 0.489 | −0.013 |
| 240×70 static | 0.215 | 0.226 | +0.011 |
| 120×40 typing | 0.243 | 0.235 | −0.008 |
| 120×40 static | 0.093 | 0.094 | +0.001 |
| snapshot | 0.072 | 0.074 | +0.002 |
| pipeline_1byte p50 | 0.071 | 0.073 | +0.002 |
| throughput (MB/s) | 141 | 142 | — |
| render, gpu exec (median) | 0.468 | 0.470 | — |

New scenarios: `tui` (btop-like box + braille, 240×70) **2.33 → 1.06 ms** and
`boxtype` (typing inside a box) **0.90 → 0.28 ms** — built-in glyphs skip font
fallback. CRT post pass (owner's look, 2560×1440): **1.59 → 0.86 ms**; CRT off: nothing
built. Backdrop: none = nothing built; on = 0.19–0.44 ms/frame GPU (baked once, copied
per frame).

> **⚠ Measuring the GPU here is noisy.** `render … gpu exec` (a `device.poll` wait)
> follows the iGPU's frequency state: two builds of *identical* render code measured
> 0.47 vs 0.71 ms in alternating pairs, and a fresh build measured right after a
> compile runs slower (the CPU and iGPU share one power budget). Compare GPU numbers
> only in a round-robin of many binaries over several rounds, never from one pair.

### Performance pass (2026-10-09, after v0.27.0)

Measured against the shared `main` of the day (v0.27.0 + that day's fixes), same
machine; A/B runs interleaved, medians.

**Idle wakeups and summon latency.** `global-hotkey`'s X11 backend serviced its
key grab with a loop that polled the X connection and slept 50 ms: **20 wakeups a
second** for the life of the process, shown or hidden, and 0–50 ms added to every
F9. Linux/BSD now grab the key in `jetty_platform::hotkey` on a thread that
blocks on its own X connection (same grab: NumLock/CapsLock combinations, XKB
detectable auto-repeat, a taken key reported). Nested X server (Xvfb + KWin,
lavapipe), the owner's CRT config, F9 by XTEST → the window's Map/UnmapNotify:

| | before | after |
|---|---|---|
| idle wakeups (10 s, shown / hidden / Settings focused) | 199 | **0** |
| F9 → hidden, p50 (min–max) | 45.7 ms (3.4–49.2) | **3.1 ms** (0.9–5.5) |
| F9 → shown, p50 | 23.6 ms | **10.8 ms** |

(The X server itself delivers a grabbed key in ~0.4 ms there.)

**Cold start: the Vulkan loader initialized every installed driver.** Creating a
Vulkan instance makes the loader `dlopen` and initialize every ICD to ask for its
extensions and devices. On this laptop that is Intel ANV (the one used), NVIDIA's
driver (~50 ms of device enumeration on its own), lavapipe and RADV (both pull in
the 138 MB libLLVM), nouveau, asahi, virtio, gfxstream, hasvk — and JeTTY kept
`/dev/nvidia*` open and ~90 MB of those libraries resident for its whole life, all
to pick the integrated GPU. `jetty_render::vk_loader` sets the loader's standard
`VK_LOADER_DRIVERS_DISABLE` for the **first instance only**, decided from the
kernel's view of the GPUs (`/sys/class/drm` render nodes, `/sys/module/nvidia`):

- a Mesa / NVIDIA driver whose kernel driver is absent cannot have a device —
  skipped (AMDVLK, ARM drivers and anything unknown are never skipped);
- lavapipe only wins when no hardware adapter can present — skipped while a real
  GPU's kernel driver is present;
- NVIDIA under the default low-power preference while an integrated-capable GPU
  (i915/xe/amdgpu/asahi) is present — wgpu then picks the integrated one.

It is a first attempt, never a different pick: unless the adapter it yields is
provably the unfiltered choice (integrated when NVIDIA was skipped; hardware when
lavapipe was; any adapter on a loader older than 1.3.234, which ignores the
variable), the instance is created again unfiltered. The one configuration that
pays for it: an X server without DRI3 (Xvfb, VNC, Xpra), where the hardware GPU
cannot present and lavapipe must — the filtered attempt finds nothing and the
retry adds ~10–20 ms (nested Xvfb with the Intel, RADV and lavapipe drivers
visible: exec→first-frame 150 → 170 ms, 10 ms resolution). Off when the user
steers drivers or GPUs (`VK_ICD_FILENAMES`, `VK_DRIVER_FILES`, `VK_ADD_DRIVER_FILES`,
`VK_LOADER_DRIVERS_SELECT/DISABLE`, `DRI_PRIME`, `MESA_VK_DEVICE_SELECT`,
`__NV_PRIME_RENDER_OFFLOAD`, `WGPU_BACKEND`; `JETTY_GPU=high` keeps NVIDIA). The
variable is set in `run()` while the process has one thread, released right after
the first instance (overwritten with "", which the loader reads as no filter — a
pointer store, never an environment shift under other threads), and removed from
every shell's environment (`jetty_core::hide_from_shells`): the first shell spawns
while it is still set, and the user's own Vulkan programs must see every GPU.

`JETTY_BENCH_ONLY=gpu_init`, 10 interleaved rounds, quiet machine:

| | every driver | filter |
|---|---|---|
| gpu_init (instance + adapter + device) | 74.2 ms (instance 58.3) | **20.3 ms** (instance 3.7) |
| process RSS right after the GPU block | 106.4 MiB (file-backed 91.8) | **15.3 MiB** (12.5) |
| shared libraries mapped | 49 (338 MiB on disk) | **26** (31 MiB) |

Not changed: an instance-creation hook (`wgpu-hal`'s `init_with_callback`) can't do
this — wgpu enumerates instance extensions (which loads every driver) before the
callback runs; `VK_LUNARG_direct_driver_loading` would mean JeTTY loading driver
libraries itself. GPU-loss recovery and every later window build unfiltered, as
before.

**Frame CPU.**

- *Snapshot* (every frame): one `&[Cell]` slice per row instead of a per-cell
  ring-buffer index through `display_iter`, a one-entry color memo for fg and bg,
  cells pushed in order (no blank pre-fill), the plain cell skipping the attribute
  ladder. Output identical — the previous implementation is kept in the tests as
  the reference and a randomized test (160 screens × 40 steps) compares every
  field. CPU-only bench, `main` vs this pass, 6 interleaved rounds:

  | grid | snapshot | pipeline_1byte p50 |
  |---|---|---|
  | 199×57 | 0.076 → **0.037 ms** | 0.075 → **0.035 ms** |
  | 240×70 | 0.109 → **0.051 ms** | 0.107 → **0.050 ms** |
  | 480×135 (4K) | 0.411 → **0.179 ms** | 0.404 → **0.171 ms** |

  Whole grid frames (`frames` table: snapshot + render_to, CPU ms; full-bench
  round-robin `main` vs this pass, 3 rounds, quiet machine): 240×70 static
  0.246 → **0.170**, typing 0.509 → **0.434**, scroll 0.542 → **0.433**, tui
  1.044 → **0.766**, boxtype 0.268 → **0.192**; 120×40 static 0.085 → **0.063**,
  boxtype 0.112 → **0.094**, typing / scroll / tui within noise (−4 / +4 / +8 %:
  tenths of a ms at that size).

- *Window chrome* (every frame: tab bar + status HUD; the bench's `chrome` line
  draws the main window's 4-tab bar and HUD exactly as the app does). Each label
  was re-shaped (Advanced shaping, font fallback) every frame, and the chrome was
  five render passes + submits. Labels are now shaped once and cached by content
  (`OverlayCache`: per family, bounded, never evicting the current pass), and the
  bar + strip draw in ONE pass (`TextLayer::render_chrome`; pixel-identical — an
  ignored GPU test compares readbacks, and the live main / detached / bottom-bar
  windows match):

  | chrome per frame | CPU | allocations |
  |---|---|---|
  | v0.27.0 | 0.239 ms | 596 |
  | label cache | 0.137 ms | 306 |
  | label cache + one pass | **0.047 ms** | **102** |

  The scrollbar / decoration / cursor rects that followed in a pass of their own
  (every frame with the default `scrollbar = "always"` once there is scrollback)
  now ride that same pass: a typical main-window frame is three submits — grid,
  chrome, corner mask — where v0.27.0 recorded seven.

**Memory per scrollback line (measured, not changed).** alacritty_terminal stores
every history row at full width, 24 B per cell: a 120-column tab's default 10 000
lines take **+28 MiB**, a 240-column one **+55 MiB** (~2.9 / 5.8 KiB per line),
per tab. The largest steady-state memory item once tabs fill their scrollback;
shrinking it means a different history representation inside the grid.

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
  - *Per-frame CPU* — snapshot is ~37 µs (199×57); the window chrome ~50 µs in one
    pass; render is GPU-bound.
- **Match:**
  - *Throughput / frame time* — we use alacritty_terminal's parser, so raw
    parse speed tracks alacritty; render at ~1.1–1.8 ms/full-frame clears 144 Hz.
    Both already beat VTE-based Konsole/GNOME Terminal on this machine. (Throughput
    measures ~146–150 MB/s at the live drain's 8 KiB chunks — at the ≥150 target, not
    above it; see the correction note.)
- **Fixed (was the one red metric):**
  - *Cold start* — gpu_init went **224 ms → ~85 ms warm** by restricting the wgpu
    instance to the **Vulkan backend** (the default probed every backend), the
    single biggest win. On top of that, the **FontSystem font-DB scan and the
    PTY fork now run on worker threads** that overlap the remaining device
    acquisition, and **global-hotkey registration moved off the main thread** on
    Linux (macOS must register it on the main thread — a cheap Carbon call there);
    `[profile.release] lto = "thin"` trims runtime. `gpu_init` measured warm
    ~85–94 ms (cold ~278 ms, first run of a cold cache) — and **~20 ms** since the
    startup Vulkan driver filter (2026-10-09: the loader no longer initializes
    NVIDIA, lavapipe, RADV, … to pick the iGPU; see §"Performance pass"). The **end-to-end
    exec→first-frame** number (which the gpu_init figure is only a subcomponent of)
    is now instrumented via `JETTY_PERF_LOG=1` — a genuine `/proc`-based exec delta
    on Linux that includes pre-`main` loader time.
  - *Remaining headroom:* a CPU-painted first frame before GPU warmup could
    shave perceived latency further, but it is no longer the bottleneck.

## Gates (review rules)

Timings are enforced by review and by running the bench before a release — **not
by a failing CI job** (CI only reports them; see rule 6). What is exact fails CI:
allocation counts (rule 7) and the paint chokepoint (rule 3's
`scripts/check-paint-choke.sh`).

1. `jetty-bench` render ≤ 6.9 ms/frame and snapshot ≤ 1 ms/frame on the baseline.
2. Throughput ≥ 150 MB/s. *(~146–150 at the live 8 KiB chunk size on a quiet
   machine since 2026-10-09 — at the floor, not above it; see the correction note.)*
3. Idle redraw stays damage-driven (no unconditional per-tick `request_redraw`); the only permitted idle wake is the perf-HUD one-shot `WaitUntil`, which must fire at most once per activity burst. **No thread polls either**: every helper thread blocks in the kernel (PTY reader, hotkey grab, IPC, config watcher, portal) — check with per-thread context switches over 10 s idle (shown, hidden, unfocused), not just the main loop.
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
   gating stays **open** (planned for v0.18, not done): it needs floors at ~50 % of
   the CI runner's observed minimum, set after watching its real distribution across
   many runs. Until then nothing in CI fails on a perf regression. CPU-only avoids
   GPU-availability / software-rasterizer timing variance on runners (it is
   display-independent, not a claim that the GPU bench "crashes" there).
7. **Allocation counts (blocking).** Unlike timings they are exact, so CI fails on
   them: `crates/jetty-core/tests/alloc.rs` holds `Terminal::feed` of output into a
   full scrollback (plain text and SGR runs) at **0** allocations and the per-frame
   `snapshot()` of a plain screen at **1** (its cells). jetty-bench's frames table
   prints allocations per frame for the GPU side; its `page` row (a page of fresh
   lines every frame — every row re-shaped) is the frame the 6.9 ms gate is about,
   and `feed_bench` has a sixel and a chunked kitty PNG workload, decode included.

## Live metrics (in-app HUD)

The bottom status strip carries a live performance HUD (toggle: `show_perf_hud`, on by default).
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
- **Throughput vs. the ≥150 target**: ~146–150 MB/s at the live chunk size
  (2026-10-09) — at the floor; beating it means work inside alacritty's parser.
- **vs. market**: same `cat 50MB` / `time seq` workload through Jetty vs. Konsole
  vs. GNOME Terminal, wall-clock compared.
