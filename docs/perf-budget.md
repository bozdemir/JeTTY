# Jetty Performance Budget

> Jetty = **Jet**. Raw speed is the #1 priority, above features. The goal is to be
> **faster than the terminals on the market** (alacritty, kitty, foot, Konsole/VTE,
> wezterm). This file is the gate: a change that regresses a budgeted metric is a
> bug, not a tradeoff.

## How to measure (reproducible)

```bash
# Hot-path numbers (headless, no window): GPU init, throughput, snapshot, render.
cargo run --release -p jetty-app --bin jetty-bench
```

Live metrics (startup-to-first-frame, input latency, idle CPU, RSS) are measured
on the running app — see "Live metrics" below.

Baseline machine: Intel Core Ultra 9 275HX (24 threads), 62 GiB RAM,
Intel Arc (Arrow Lake) iGPU via Vulkan (LowPower — the NVIDIA dGPU is avoided on
purpose), 1920×1200 @ 59.95 Hz. Compared against the terminals installed here:
Konsole 23.08.5, GNOME Terminal / VTE 0.76.

## The budget

| Metric | Market reference (fastest class) | Jetty **target** (gate) | Jetty **current** | Status |
|---|---|---|---|---|
| **Frame render** (full screen, 199×57) | 60 Hz = 16.7 ms; 144 Hz = 6.9 ms/frame | ≤ **6.9 ms** (144 Hz-ready); hard ≤ 16.7 ms | **5.5 ms** (180 fps cap) | ✅ meets 144 Hz |
| **Idle CPU** | ~0 % (event-driven terminals) | **0 %** when nothing changes | ~0 % (damage-driven redraw) | ✅ |
| **Per-frame CPU** (snapshot, 11k cells) | n/a | ≤ **1 ms** | **0.047 ms** | ✅ 20× under |
| **Throughput** (parse+grid, colored VT) | alacritty class: very high; VTE/Konsole: lower | ≥ **150 MB/s**; stretch ≥ 300 | **154 MB/s** | ✅ meets (stretch open) |
| **Cold start** (process → first frame) | foot ~40–60 ms; alacritty ~100–300 ms | < **150 ms**; stretch < 80 ms | gpu_init **224 ms** + text 23 ms → est. ~300 ms | ❌ **needs work** |
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
- **Must improve to truly beat the market:**
  - *Cold start* — **the one red metric.** ~224 ms is wgpu adapter+device
    acquisition (Vulkan ICD + driver init). foot wins startup by being
    CPU-rendered (no GPU init). Options, in order of appeal:
    1. Show the window + a CPU-rendered first frame immediately, warm the GPU in
       the background, swap to GPU rendering once ready (hides the 224 ms).
    2. Create the GPU device concurrently with window creation, not after.
    3. Cache/skip redundant init work; lazy-load the font DB enumeration
       (only needed when the settings dialog opens).

## Gates (CI-style rules)

1. `jetty-bench` render ≤ 6.9 ms/frame and snapshot ≤ 1 ms/frame on the baseline.
2. Throughput ≥ 150 MB/s.
3. Idle redraw stays damage-driven (no unconditional per-tick `request_redraw`).
4. Nothing added to the keystroke → PTY → render path that isn't strictly needed.
5. Cold start trends **down**, never up; target < 150 ms.

## Live metrics (to fill in)

- **Cold start**: time from `exec` to first presented frame (instrument a one-shot
  `eprintln!` with an env-gated timestamp at first `RedrawRequested`).
- **Input latency**: Typometer or a high-FPS capture of keypress → glyph.
- **Idle RSS**: `ps -o rss` after the prompt settles.
- **vs. market**: same `cat 50MB` / `time seq` workload through Jetty vs. Konsole
  vs. GNOME Terminal, wall-clock compared.
