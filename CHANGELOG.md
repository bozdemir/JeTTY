# Changelog

All notable changes to JeTTY are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.3.0] — 2026-06-29

A customization release: the window chrome now has its own font.

### Added
- **Separate UI (chrome) font** — pick a family and size for ALL window chrome
  (tab titles, the bottom status bar, the right-click menu, the Settings panel,
  help overlay, dialogs, the welcome splash), independent of the terminal grid
  font. New `ui_font_family` (default `""` = the platform's proportional sans)
  and `ui_font_size` (10–28pt, default 16) config keys.
- **"UI FONT" Settings section** — a size −/+/reset control with a live, true-size
  "Aa" specimen and a scrollable proportional-family picker (with a
  "System Sans (default)" row).

### Changed
- A UI-font size change resizes the chrome in place (no fontconfig rescan) and a
  family change swaps without a rescan; neither reflows the terminal grid, so the
  hot path and ~0% idle are untouched. The default look is unchanged: with the
  empty-string default the chrome renders exactly as in 0.2.0 (the platform sans
  lacks the ⇧⌃⚡⚙ symbol glyphs, so symbol-bearing chrome stays in the mono Nerd
  Font until a named UI family is chosen).

---

## [0.2.0] — 2026-06-27

A polish + correctness release: a redesigned, elegant tab bar, a proper bottom
status bar for the perf HUD, and a large wave of fixes from a deep multi-agent
audit (89 agents) — including several that make JeTTY a *correct* terminal for
TUIs (vim/htop/fzf), not just a fast one.

### Added
- **Bottom status bar** — the live perf HUD (ms · fps · CPU% · VT MB/s) moved off
  the tab row into a slim status bar at the window bottom (`show_perf_hud`).
- `CONTRIBUTING.md`, `CHANGELOG.md`, release notes, and GitHub issue/PR templates.

### Changed
- **Redesigned tab bar** — frameless, modern (Safari/Zed/Arc style): the active
  tab is a soft theme-derived pill (no per-tab borders or `❯` marker); inactive
  tabs are dim text only.
- **Tab titles render in the platform's proportional sans-serif** (San Francisco
  on macOS, the system UI sans on Linux) for an elegant, non-"code" look.
- Chrome width math now uses the **measured** font advance — fixes HiDPI/Retina
  overflow in menus, the HUD, and the settings panel.

### Fixed
- **Keyboard**: Home/End/Delete/Insert/F1–F12 were dropped entirely; `Ctrl/Shift/
  Alt`+Arrow collapsed to bare arrows; Shift+Tab sent TAB. Now emit the proper
  xterm sequences — vim/htop/less/fzf/readline editing works.
- **Idle CPU**: a debounced resize held the loop in `Poll` and re-rendered ~15
  frames for nothing — restored ~0% idle.
- **macOS**: window transparency (correct `alpha_mode` selection) and Option-key
  composed glyphs (©/ü) now reach the shell.
- **Processes**: closed/exited shells are reaped (no more zombie/orphan leak).
- Dropdown re-summons on the last-used monitor; lazy Tier-B offscreen; IPC socket
  TOCTOU + UID-namespaced fallback; phosphor WGSL fixes; many smaller robustness
  and consistency fixes.

---

## [0.1.0] — 2026-06-26

First public release of JeTTY — a blazing-fast, GPU-accelerated terminal with a
center-summon / Yakuake-style dropdown hotkey.

### Added

**Core terminal**
- True-color VT100/VT220 emulation via `alacritty_terminal`; answers host queries
  (DSR/DA), proper `TERM=xterm-256color`, 10k-line scrollback.
- PTY fork + drain loop; `Ctrl+D` closes the shell cleanly.
- Window resize with grid reflow; terminal tracks physical pixel size changes.

**GPU rendering**
- Full `wgpu`-based render pipeline (Vulkan on Linux, Metal on macOS).
- Text rendering via `glyphon` / `cosmic-text`; live font family + size at runtime.
- Sub-millisecond grid snapshot (0.047 ms / frame at full screen).
- Damage-driven redraw — idle CPU is genuinely 0% (no polling, no cursor-blink timer).
- `jetty-bench` headless benchmark for reproducible perf measurements.

**Summon hotkey & window modes**
- Global F9 hotkey via `global-hotkey` crate (X11 native grab; IPC socket fallback
  for Wayland and macOS).
- Single-instance IPC socket (`$XDG_RUNTIME_DIR/jetty.sock`, fallback `/tmp/jetty.sock`);
  subsequent `jetty` invocations toggle the running instance.
- **Center mode** — drops into the middle of the current monitor.
- **Dropdown mode** — slides down from the top edge, full screen width (Yakuake/Guake
  style), with adjustable width & height percentage.
- Five summon reveal shaders: **Phosphor Ignition** (default), **Bayer Crystallize**,
  **Liquid Drop**, **Focus Pull**, **None**.

**Tabs**
- `Ctrl+Shift+T` new tab, `Ctrl+Shift+W` close (with confirm dialog), `Ctrl+Tab` /
  `Ctrl+1–9` switch, double-click to rename.
- Per-tab PTY; closing the last tab exits the app.

**Themes (5)**
- Catppuccin Mocha (default), Tokyo Night, Gruvbox Dark, Dracula, Onyx.
- Exact community palettes; every UI surface (panel, menus, tab bar, welcome,
  confirm dialogs, help overlay) re-skins with the active theme.

**Settings dialog**
- `Ctrl+Shift+P` opens a movable settings panel; persisted to
  `~/.config/jetty/config.toml`.
- Controls: theme, opacity, corner radius, summon effect, window mode, dropdown
  size, tab-bar position, focus auto-hide, welcome splash, performance HUD, font
  family + size.

**Live performance HUD**
- Optional tab-bar overlay: `⚡ <ms> ms · <fps> fps · <cpu>% · <mb> MB/s`.
- Idle one-shot: fires once after settling, displays `⚡ idle · 0% CPU · 0 MB/s`,
  then sleeps. Never regresses idle CPU.

**Welcome overlay**
- Neofetch-style splash on first launch; dismissed on first key/click/Esc.
- Toggle with `show_welcome` in config.

**Selection & clipboard**
- Left-drag to select (auto-copies), right-click context menu (Copy / Paste /
  Select All / Clear / Close Tab), `Ctrl+Shift+C/V`, middle-click paste,
  bracketed-paste aware.

**Custom window chrome**
- Borderless client-side decorations, our own title bar, rounded corners (radius
  slider), runtime opacity, focus auto-hide.

**Packaging & distribution (Linux x86_64)**
- `cargo build --release` produces a self-contained binary.
- `install.sh` one-line installer with SHA256 checksum verification; supports
  `JETTY_PREFIX` for system-wide installs; writes absolute `Exec=` path in the
  installed `.desktop`.
- `.deb` via `cargo-deb`, AppImage via `linuxdeploy`; CI publishes all artifacts
  on `v*` tags.

**macOS (Metal)**
- Full feature parity on macOS (Metal backend); builds from source without extra
  system packages.

### Known issues

- **Resize/p10k prompt scatter** — resizing the window or changing font size can
  scatter p10k's prompt into fragments. Debounce (`RESIZE_DEBOUNCE_MS`) mitigates
  but does not fully fix this; root cause is alacritty_terminal's reflow interacting
  with complex prompt escape sequences. Investigation ongoing.
- **Wayland: no native global shortcut** — the XDG GlobalShortcuts portal is not
  yet implemented; use the compositor binding + IPC socket workaround described in
  `docs/global-hotkey.md`.
- **macOS: no prebuilt binary** — macOS users must build from source. A prebuilt
  `.app`/`.dmg` is on the roadmap.

---

[0.1.0]: https://github.com/bozdemir/JeTTY/releases/tag/v0.1.0
