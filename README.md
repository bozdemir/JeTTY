<div align="center">

# ⚡ JeTTY

**A blazing-fast, GPU-accelerated terminal that summons to the center of your screen — or drops down Yakuake-style — on a global hotkey.**

*Je**TTY** — a terminal (**TTY**) that moves like a **Jet**. Raw speed is its first priority, above everything else.*

[![CI](https://github.com/bozdemir/JeTTY/actions/workflows/ci.yml/badge.svg)](https://github.com/bozdemir/JeTTY/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/bozdemir/JeTTY?sort=semver)](https://github.com/bozdemir/JeTTY/releases/latest)
![Rust](https://img.shields.io/badge/Rust-2021-CE412B?logo=rust&logoColor=white)
![GPU](https://img.shields.io/badge/Render-wgpu%20%2F%20Vulkan%20%2F%20Metal-4051B5)
![Platform](https://img.shields.io/badge/Linux-X11%20%7C%20Wayland-1f6feb?logo=linux&logoColor=white)
![Platform](https://img.shields.io/badge/macOS-Metal-silver?logo=apple&logoColor=white)
![Desktop](https://img.shields.io/badge/Desktop-KDE%20%7C%20GNOME%20%7C%20any-2ea043)
![License](https://img.shields.io/badge/license-MIT-green)
![Collaborators wanted](https://img.shields.io/badge/collaborators-wanted-ff5c8a)

<img src="assets/screenshots/hero.png" alt="JeTTY with tabs in the Catppuccin Mocha theme" width="820">

</div>

---

> 🤝 **JeTTY is young and looking for collaborators!** If you love terminals, Rust, or GPU rendering, come help shape a fast, beautiful terminal — see [Collaborators wanted](#-collaborators-wanted).

## Contents

- [Features](#-features)
- [Screenshots](#-screenshots)
- [Install](#-install)
- [Keybindings](#️-keybindings)
- [Performance](#-performance)
- [Architecture](#-architecture)
- [Collaborators wanted](#-collaborators-wanted)
- [Roadmap](#️-roadmap)
- [License](#-license)

## ✨ Features

- 🚀 **Blazing fast** — GPU-rendered with [`wgpu`](https://github.com/gfx-rs/wgpu); ~5.5 ms full-screen frames (144 Hz-ready), **~0 % CPU when idle** (damage-driven redraw), 150+ MB/s VT throughput. See the [performance budget](docs/perf-budget.md).
- 🎯 **Global summon hotkey** — press **F9** anywhere to bring JeTTY up. Two modes (switchable in settings):
  - **Center** — drops into the middle of your screen.
  - **Dropdown** — slides down from the top edge, full screen width, Yakuake/Guake style, with adjustable width & height.
- ✨ **Summon effects** — five self-written GPU reveal shaders, selectable in settings: **Phosphor Ignition** (default — CRT power-on), **Bayer Crystallize**, **Liquid Drop**, **Focus Pull**, or **None**.
- 📺 **Visual effects** — an optional **CRT** mode (curvature, scanlines, shadow-mask, bloom, chromatic aberration, vignette + animated roll/flicker/jitter) and a **caret flash/glow**, all under a dedicated **Effects** settings tab. Off by default, so the default look and ~0% idle CPU are untouched.
- 🗂️ **Tabs** — `Ctrl+Shift+T` new (**opens in the current tab's directory**), `Ctrl+Shift+W` close (with confirm), `Ctrl+Tab` / `Ctrl+1‒9` switch, double-click to rename, right-click for a **Detach / Rename / Close** menu. Tabs **auto-title from the shell** (OSC 0/2 — a manual rename always wins), and inactive tabs show an **activity / bell dot**.
- 🔎 **Scrollback search** — `Ctrl+Shift+F`: incremental, case-insensitive, every visible match highlighted, `Enter`/`F3` / `Shift+F3` to jump older/newer, live `3/17` counter.
- 🔗 **Clickable links** — hold **Ctrl** (also ⌘ on macOS) to underline the URL under the pointer, **Ctrl+click** opens it in your browser — plain-text URLs *and* OSC 8 hyperlinks.
- 🪟➡️ **Detachable tabs** — `Ctrl+Shift+D`, the tab's right-click menu, or simply **dragging a tab off the bar** pops it into its own window (with its own title bar and status strip); reattach with `Ctrl+Shift+D`, the window's right-click menu, closing it, or **dropping it back onto the main tab bar**. Detached windows have **full mouse parity**: selection, wheel, scrollbar, middle-click paste.
- 🎨 **22 built-in themes** — Catppuccin (Mocha/Macchiato), Tokyo Night, Gruvbox, Dracula, Onyx, Nord, Solarized (dark/light), One Dark, Monokai (+Pro), Everforest, Rosé Pine, Kanagawa, Material, Ayu (dark/mirage), Tomorrow Night, Oceanic Next, GitHub Dark, Palenight — exact community palettes, picked from a scrollable dropdown with live color previews. Every UI surface re-skins with the active theme.
- 🪟 **Custom-decorated window** — borderless client-side decorations, our own title bar, rounded corners (radius slider), runtime opacity.
- 🔤 **Live font control** — change font **size** (`Ctrl + +/-/0`) and **family** (any installed monospace) at runtime, no restart.
- 📋 **Selection & clipboard** — drag to select (auto-copies), **Shift+drag** to select even inside mouse-aware TUIs (vim/htop/tmux/Claude Code), right-click **Copy / Paste / Select All** menu, `Ctrl+Shift+C/V`, middle-click paste, bracketed-paste aware.
- ⚙️ **Settings dialog** — `Ctrl+Shift+P` opens a movable dialog (theme, opacity, corner radius, summon effect, window mode, dropdown size, tab-bar position, scrollback size, shell, focus auto-hide, welcome splash, performance HUD, font) — all **persisted** to `~/.config/jetty/config.toml`.
- 📊 **Live performance HUD** — an optional tab-bar overlay showing frame ms · fps · CPU% · VT MB/s in real time, and an honest "idle" state when the app settles (never forces a redraw — idle stays ~0% CPU). Toggle with `show_perf_hud`.
- 👋 **Welcome overlay** — a neofetch-style splash on first launch (accent ASCII logo + version/backend), dismissed on the first key/click/Esc. Toggle with `show_welcome`.
- 🖥️ **Desktop-independent** — X11 **and** Wayland, KDE / GNOME / any compositor, every distro. **No DE-specific code**, no compositor libraries.
- ✅ **A real terminal** — true-color, answers host queries (DSR/DA), proper `TERM`, window resize with grid reflow, configurable scrollback (1k–100k lines), Ctrl+D closes cleanly.

## 📸 Screenshots

| Catppuccin Mocha | Tokyo Night |
|:---:|:---:|
| <img src="assets/screenshots/catppuccin.png" width="400"> | <img src="assets/screenshots/tokyo-night.png" width="400"> |
| **Gruvbox Dark** | **Dracula** |
| <img src="assets/screenshots/gruvbox.png" width="400"> | <img src="assets/screenshots/dracula.png" width="400"> |
| **Onyx** | **Settings — 22 themes, live previews** |
| <img src="assets/screenshots/onyx.png" width="400"> | <img src="assets/screenshots/settings.png" width="400"> |

| CRT effect (curvature · scanlines · bloom · vignette) | Summon effect (Phosphor Ignition) |
|:---:|:---:|
| <img src="assets/screenshots/crt.png" width="400"> | <img src="assets/screenshots/phosphor.png" width="400"> |

| Live perf HUD | |
|:---:|:---:|
| <img src="assets/screenshots/perf-hud.png" width="400"> | |

## 🚀 Install

JeTTY runs on **Linux** (X11 / Wayland, Vulkan) and **macOS** (Metal). Building from source needs only the Rust toolchain and works on both.

### 🍎 macOS — build from source

```bash
# 1. Install Rust (skip if you already have it)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && source "$HOME/.cargo/env"

# 2. Build + run
git clone https://github.com/bozdemir/JeTTY.git && cd JeTTY
cargo build --release
./target/release/jetty
```

Renders through **Metal**. Summon with **F9** — on Mac keyboards where the function-row keys default to media actions, press `fn`+`F9` so the OS delivers F9. You can also bind `jetty --toggle` to a shortcut via a launcher (the first press launches JeTTY; each subsequent press toggles the running instance via the single-instance socket). A locally built binary is not quarantined, so there's no Gatekeeper prompt. *(Prebuilt `.app` / `.dmg` are on the [roadmap](#-roadmap).)*

### 🍎 macOS (.app bundle with Dock icon)

```bash
cargo build --release
sh scripts/make-macos-app.sh      # builds JeTTY.app with the Dock/Finder icon
open JeTTY.app                     # run the bundle, NOT ./target/release/jetty
```

> **Note:** the bare binary cannot show a Dock icon on macOS (winit limitation); the `.app` bundle is required. If the icon appears stale in the Dock, run `killall Dock` once to flush the icon cache.

### 🐧 Linux — one-line installer (prebuilt, no toolchain)

```bash
curl -fsSL https://raw.githubusercontent.com/bozdemir/JeTTY/main/install.sh | sh
```

Installs to `~/.local/bin` by default. The script verifies the published `SHA256SUMS.txt` checksum before installing. For a system-wide install:

```bash
curl -fsSL https://raw.githubusercontent.com/bozdemir/JeTTY/main/install.sh | JETTY_PREFIX=/usr/local sudo -E sh
```

Also available: a launcher entry. Or grab a `.deb` / **AppImage** from the [latest release](https://github.com/bozdemir/JeTTY/releases/latest):

```bash
sudo apt install ./jetty_*_amd64.deb                              # Debian / Ubuntu
chmod +x JeTTY-*-x86_64.AppImage && ./JeTTY-*-x86_64.AppImage     # any distro
```

### Build from source (Linux or macOS)

```bash
git clone https://github.com/bozdemir/JeTTY.git && cd JeTTY
cargo build --release && ./target/release/jetty
```

> Prebuilt artifacts (`.deb`, AppImage, tarball, checksums) are published by CI when a `v*` tag is pushed — **Linux x86_64 today; macOS prebuilt builds are on the roadmap.** Until then, macOS users build from source (above).

### Global summon hotkey

- **X11** — `F9` works immediately, no setup.
- **Wayland** — Wayland routes global shortcuts through the compositor, so bind **`jetty --toggle`** to a key (first press launches JeTTY; each press after toggles the running instance via the single-instance socket; `--show` / `--hide` set the state explicitly). See [`docs/global-hotkey.md`](docs/global-hotkey.md). *(Note: in Dropdown mode, top-edge anchoring relies on window positioning, which the compositor controls on Wayland — it works fully on X11.)*

## ⌨️ Keybindings

| Key | Action |
|---|---|
| `F9` | Summon / hide JeTTY (global; `fn`+`F9` on macOS) |
| `Ctrl+Shift+P` | Open settings dialog |
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+W` | Close tab (with confirm) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+Shift+D` | Detach tab into its own window / reattach |
| Drag tab off the bar | Detach (drop the window on the bar to reattach) |
| Right-click tab | Detach / Rename / Close menu |
| `Ctrl+1`‒`9` | Jump to tab |
| `Ctrl` + `+` / `-` / `0` | Font size up / down / reset |
| `Ctrl+Shift` + `+` / `-` | Opacity up / down |
| Left-drag | Select text (auto-copies) |
| **Shift**+drag | Select text over mouse-aware apps (vim/htop/tmux/Claude Code) |
| Right-click | Copy / Paste / Select All / Clear / Close Tab menu |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / Paste |
| `Ctrl+D` | Close the shell (and window) |

*Theme is chosen by clicking a theme card in the Settings dialog (`Ctrl+Shift+P`) — there is no theme keybinding.*

## ⚡ Performance

Measured headlessly on an Intel Arc iGPU at 1920×1200 (`cargo run --release -p jetty-app --bin jetty-bench`):

| Metric | JeTTY | Target |
|---|---|---|
| Frame render (full screen) | **5.5 ms** (180 fps cap) | ≤ 6.9 ms (144 Hz) |
| Idle CPU | **~0 %** | 0 % |
| Per-frame snapshot (11k cells) | **0.047 ms** | ≤ 1 ms |
| VT throughput | **154 MB/s** | ≥ 150 MB/s |

Speed is a gated requirement, not an afterthought — see [`docs/perf-budget.md`](docs/perf-budget.md).

## 🧱 Architecture

A small Cargo workspace with clear boundaries:

| Crate | Responsibility |
|---|---|
| `jetty-core` | VT model (alacritty_terminal), PTY, themes, grid snapshot |
| `jetty-render` | GPU layers — text (glyphon/cosmic-text), quads, panel, menu, summon-effect shaders |
| `jetty-platform` | Window creation (winit), raw-window-handle plumbing |
| `jetty-app` | Event loop, input, clipboard, tabs, settings, hotkey, window modes, the binary |

## 🤝 Collaborators wanted

JeTTY is in active early development and **we're looking for collaborators.** Whether you want to own a feature, fix a bug, or just trade ideas — you're welcome, at any experience level.

Great places to jump in right now:

- Native Wayland global shortcut (XDG GlobalShortcuts portal)
- Multi-monitor awareness & per-monitor dropdown placement
- More summon effects / themes / visual polish
- Faster cold start
- Packaging (PPA, AUR, Flatpak research), docs

**How to get involved:** open an [issue](https://github.com/bozdemir/JeTTY/issues) or discussion, or send a pull request. New to the code? The [architecture](#-architecture) section is a good place to start.

## 🗺️ Roadmap

- Native Wayland global shortcut via the XDG GlobalShortcuts portal
- Multi-monitor awareness
- Launchpad PPA (`apt install jetty`) + AUR package
- Faster cold start
- More summon effects and themes

## 📄 License

MIT — see [`LICENSE`](LICENSE).

---

<div align="center"><sub>Built in Rust. Speed first. 🚀</sub></div>
