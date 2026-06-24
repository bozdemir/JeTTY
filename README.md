<div align="center">

# ⚡ JeTTY

**A blazing-fast, GPU-accelerated terminal that summons to the center of your screen — or drops down Yakuake-style — on a global hotkey.**

*Je**TTY** — a terminal (**TTY**) that moves like a **Jet**. Raw speed is its first priority, above everything else.*

![Rust](https://img.shields.io/badge/Rust-2021-CE412B?logo=rust&logoColor=white)
![GPU](https://img.shields.io/badge/Render-wgpu%20%2F%20Vulkan-4051B5)
![Platform](https://img.shields.io/badge/Linux-X11%20%7C%20Wayland-1f6feb?logo=linux&logoColor=white)
![Desktop](https://img.shields.io/badge/Desktop-KDE%20%7C%20GNOME%20%7C%20any-2ea043)
![License](https://img.shields.io/badge/license-MIT-green)
![Collaborators wanted](https://img.shields.io/badge/collaborators-wanted-ff5c8a)

<img src="assets/screenshots/hero.png" alt="JeTTY with tabs in the Catppuccin Mocha theme" width="820">

</div>

---

> 🤝 **JeTTY is young and looking for collaborators!** If you love terminals, Rust, or GPU rendering, come help shape a fast, beautiful terminal — see [Collaborators wanted](#-collaborators-wanted).

## ✨ Features

- 🚀 **Blazing fast** — GPU-rendered with [`wgpu`](https://github.com/gfx-rs/wgpu); ~5.5 ms full-screen frames (144 Hz-ready), **~0 % CPU when idle** (damage-driven redraw), 150+ MB/s VT throughput. See the [performance budget](docs/perf-budget.md).
- 🎯 **Global summon hotkey** — press **F9** anywhere to bring JeTTY up. Two modes (switchable in settings):
  - **Center** — drops into the middle of your screen.
  - **Dropdown** — slides down from the top edge, full screen width, Yakuake/Guake style, with adjustable width & height.
- ✨ **Summon effects** — five self-written GPU reveal shaders, selectable in settings: **Phosphor Ignition** (default — CRT power-on), **Bayer Crystallize**, **Liquid Drop**, **Focus Pull**, or **None**.
- 🗂️ **Tabs** — `Ctrl+Shift+T` new, `Ctrl+Shift+W` close (with confirm), `Ctrl+Tab` / `Ctrl+1‒9` switch, double-click to rename.
- 🎨 **Beloved themes** — Catppuccin Mocha (default), Tokyo Night, Gruvbox Dark, Dracula, with exact community palettes. Every UI surface re-skins with the theme.
- 🪟 **Custom-decorated window** — borderless client-side decorations, our own title bar, rounded corners (radius slider), runtime opacity.
- 🔤 **Live font control** — change font **size** (`Ctrl + +/-/0`) and **family** (any installed monospace) at runtime, no restart.
- 📋 **Selection & clipboard** — drag to select (auto-copies), right-click **Copy / Paste / Select All** menu, `Ctrl+Shift+C/V`, middle-click paste, bracketed-paste aware.
- ⚙️ **Settings dialog** — `Ctrl+Shift+P` opens a movable dialog (theme, opacity, corner radius, summon effect, window mode, dropdown size, font) — all **persisted** to `~/.config/jetty/config.toml`.
- 🖥️ **Desktop-independent** — X11 **and** Wayland, KDE / GNOME / any compositor, every distro. **No DE-specific code**, no compositor libraries.
- ✅ **A real terminal** — true-color, answers host queries (DSR/DA), proper `TERM`, window resize with grid reflow, 10k-line scrollback, Ctrl+D closes cleanly.

## 📸 Screenshots

| Catppuccin Mocha | Tokyo Night |
|:---:|:---:|
| <img src="assets/screenshots/catppuccin.png" width="400"> | <img src="assets/screenshots/tokyo-night.png" width="400"> |
| **Gruvbox Dark** | **Dracula** |
| <img src="assets/screenshots/gruvbox.png" width="400"> | <img src="assets/screenshots/dracula.png" width="400"> |

| Settings dialog | Summon effect (Phosphor Ignition) |
|:---:|:---:|
| <img src="assets/screenshots/settings.png" width="400"> | <img src="assets/screenshots/phosphor.png" width="400"> |

## 🚀 Install

### One-line installer (recommended — no toolchain needed)

```bash
curl -fsSL https://raw.githubusercontent.com/bozdemir/jetty/main/install.sh | sh
```

Downloads the latest prebuilt binary and installs it for your user (`~/.local/bin` + a launcher entry). For a system-wide install: `curl -fsSL …/install.sh | sudo JETTY_PREFIX=/usr/local sh`.

### Debian / Ubuntu (`.deb`)

Grab `jetty_<version>_amd64.deb` from the [latest release](https://github.com/bozdemir/jetty/releases/latest):

```bash
sudo apt install ./jetty_*_amd64.deb
```

### AppImage (any distro, portable)

Download `JeTTY-<version>-x86_64.AppImage` from [releases](https://github.com/bozdemir/jetty/releases/latest), then:

```bash
chmod +x JeTTY-*-x86_64.AppImage && ./JeTTY-*-x86_64.AppImage
```

### From crates / source (Rust toolchain)

```bash
git clone https://github.com/bozdemir/jetty.git && cd jetty
cargo build --release
./target/release/jetty
```

> Releases (`.deb`, AppImage, tarball, checksums) are built automatically by CI when a `v*` tag is pushed.

### Global summon hotkey

- **X11** — `F9` works immediately, no setup.
- **Wayland** — Wayland routes global shortcuts through the compositor, so bind a key to `jetty --toggle` (toggles the running instance via a single-instance socket). See [`docs/global-hotkey.md`](docs/global-hotkey.md). *(Note: in Dropdown mode, top-edge anchoring relies on window positioning, which the compositor controls on Wayland — it works fully on X11.)*

## ⌨️ Keybindings

| Key | Action |
|---|---|
| `F9` | Summon / hide JeTTY (global) |
| `Ctrl+Shift+P` | Open settings dialog |
| `Ctrl+Shift+T` / `Ctrl+Shift+W` | New / close tab |
| `Ctrl+Tab` / `Ctrl+1‒9` | Switch tab |
| `Ctrl+Shift+T` | (in settings) cycle theme |
| `Ctrl` + `+` / `-` / `0` | Font size up / down / reset |
| `Ctrl+Shift` + `+` / `-` | Opacity up / down |
| Left-drag | Select text (auto-copies) |
| Right-click | Copy / Paste / Select All menu |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / Paste |
| `Ctrl+D` | Close the shell (and window) |

## ⚡ Performance

Measured headlessly on an Intel Arc iGPU at 1920×1200 (`cargo run --release -p jetty-app --bin jetty-bench`):

| Metric | JeTTY | Target |
|---|---|---|
| Frame render (full screen) | **5.5 ms** (180 fps) | ≤ 6.9 ms (144 Hz) |
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

**How to get involved:** open an [issue](https://github.com/bozdemir/jetty/issues) or discussion, or send a pull request. New to the code? The [architecture](#-architecture) section is a good place to start.

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
