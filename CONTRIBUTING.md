# Contributing to JeTTY

Thanks for your interest in contributing! JeTTY is in active early development
and welcomes contributors at any experience level — whether you want to own a
feature, fix a bug, or just trade ideas.

## Prerequisites

### Rust toolchain

Install via [rustup](https://rustup.rs/) (stable is fine):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Linux system dependencies

```bash
sudo apt-get install -y \
  pkg-config libvulkan-dev libwayland-dev libxkbcommon-dev \
  libx11-dev libxcb1-dev libfontconfig1-dev libgl1-mesa-dev
```

(These are the same deps installed in CI — see `.github/workflows/ci.yml`.)

### macOS

No extra system packages needed. Jetty renders through **Metal** on macOS; the
`wgpu` backend is selected automatically. Xcode command-line tools must be
installed (`xcode-select --install`).

## Build, run, test

```bash
# Clone the repo
git clone https://github.com/bozdemir/JeTTY.git && cd JeTTY

# Debug build (faster compile, some perf loss)
cargo build

# Release build — always use this before manual testing (the user runs the
# release binary directly; debug builds are noticeably slower)
cargo build --release --bin jetty
./target/release/jetty

# Run all tests (every crate — the workspace's default-members cover them all)
cargo test

# Run tests for a specific crate
cargo test -p jetty-render
```

## Headless self-test with `jetty-shot`

`jetty-shot` renders a single frame to a PNG without opening a window
(no X server, no Xvfb needed). Use it to verify visual changes:

```bash
cargo build --release --bin jetty-shot -p jetty-app

# Default render (terminal view, Catppuccin Mocha theme)
JETTY_SHOT_OUT=out.png cargo run --release -p jetty-app --bin jetty-shot

# Settings panel
JETTY_SHOT_PANEL=1 JETTY_THEME="Catppuccin Mocha" \
  JETTY_SHOT_OUT=settings.png \
  cargo run --release -p jetty-app --bin jetty-shot

# Help overlay
JETTY_SHOT_HELP=1 JETTY_SHOT_OUT=help.png \
  cargo run --release -p jetty-app --bin jetty-shot
```

### `jetty-shot` environment variables

| Variable | Description |
|---|---|
| `JETTY_SHOT_OUT` | Output PNG path (default: `jetty-shot.png`) |
| `JETTY_SHOT_WIDTH` | Window width in pixels (default: 1280) |
| `JETTY_SHOT_HEIGHT` | Window height in pixels (default: 720) |
| `JETTY_SHOT_INPUT` | Text to feed into the PTY before capture |
| `JETTY_THEME` | Theme name (e.g. `"Catppuccin Mocha"`, `"Tokyo Night"`, `"Gruvbox Dark"`, `"Dracula"`, `"Onyx"`) |
| `JETTY_OPACITY` | Window opacity 0.0–1.0 |
| `JETTY_FONT_SIZE` | Font size in px |
| `JETTY_FONT_FAMILY` | Font family name |
| `JETTY_CORNER_RADIUS` | Corner radius in px |
| `JETTY_SHOT_PANEL` | `1` — render the settings panel open |
| `JETTY_SHOT_MENU` | `1` — render the right-click context menu |
| `JETTY_SHOT_HELP` | `1` — render the help overlay |
| `JETTY_SHOT_TABBAR` | `1` — render just the tab bar strip |
| `JETTY_SHOT_PERF` | Draw this perf-HUD string in the bottom status strip (with `JETTY_SHOT_TABBAR`) |
| `JETTY_SHOT_WELCOME` | `1` — render the welcome overlay |
| `JETTY_SHOT_CONFIRM` | `1` — render the close-tab confirm dialog (`last`: the last tab's, which quits JeTTY) |
| `JETTY_SHOT_QUIT` | `1` — render the quit-app confirm dialog |
| `JETTY_SHOT_SUMMON_T` | Summon effect blend t value 0.0–1.0 |
| `JETTY_SHOT_PHOSPHOR_T` | Phosphor ignition effect t value 0.0–1.0 |
| `JETTY_SHOT_PTY` | `1` — fork a real PTY shell before capture |

## Performance (`jetty-bench`)

Performance is a requirement, not an afterthought: measure a hot-path change against
the budgets in [`docs/perf-budget.md`](docs/perf-budget.md) before sending it (CI only
reports the CPU-side numbers; it doesn't fail on a regression yet).
Run the headless benchmark before and after any change that touches the hot path:

```bash
cargo run --release -p jetty-app --bin jetty-bench
```

A PR that regresses frame render beyond 6.9 ms/frame or snapshot beyond 1 ms/frame
will be asked to fix it before merge.

## Crate map

| Crate | Responsibility |
|---|---|
| `crates/jetty-core` | VT model (alacritty_terminal), PTY, themes, grid snapshot |
| `crates/jetty-render` | GPU layers — text (glyphon/cosmic-text), quads, panel, menu, summon-effect shaders |
| `crates/jetty-platform` | Window creation (winit), raw-window-handle plumbing |
| `crates/jetty-app` | Event loop, input, clipboard, tabs, settings, hotkey, window modes, the binary |

## Code conventions (hard invariants)

1. **Speed is #1.** Every change that touches the render path or PTY drain must
   be benchmarked. Never add an unconditional `request_redraw()` in the idle
   path; never poll.

2. **No desktop-environment-specific code.** No KDE/GNOME/compositor-specific
   libraries, detection, or behaviour branches. Everything must work on any
   X11 or Wayland compositor (and macOS). See project memory for the history
   of why this rule exists.

3. **Whole-codebase chrome theming.** Every UI surface (panel, menus, tab bar,
   welcome overlay, confirm dialogs, help overlay) must re-skin with the active
   theme. If you add a new surface, derive its colours from `theme.bg`/`theme.fg`
   the same way `panel.rs` and `help.rs` do — never hardcode a colour.

## Pull request steps

1. Fork the repo and create a feature branch from `main`.
2. Run `cargo build --release --bin jetty` (not just `cargo build`).
3. Run `cargo test` — all tests must pass.
4. If your change touches the render path, run `jetty-bench` and include the
   numbers in the PR description.
5. If your change affects any UI surface, run `jetty-shot` and include a
   screenshot in the PR description.
6. Open a PR against `main`. Reference any related issue.

New contributors: the [architecture](#crate-map) section is a good starting
point. Open an [issue](https://github.com/bozdemir/JeTTY/issues) or
[discussion](https://github.com/bozdemir/JeTTY/discussions) before starting
large changes so we can align on design first.
