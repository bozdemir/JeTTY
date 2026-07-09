# Changelog

All notable changes to JeTTY are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.16.0] — 2026-07-09

**SSH-ready & yours** — three daily-driver features that remove reasons to reach
for another terminal. Designed from a verified blueprint, stress-tested by two
design critics (9 blocking issues caught pre-implementation), then adversarially
reviewed.

### Added
- **OSC 52 clipboard** — copy from inside `ssh` / `tmux` / `nvim` (or any program
  that emits OSC 52) straight to your **local** system clipboard. Write is on by
  default; **remote paste/read is opt-in** (`osc52_allow_paste`, default off) so a
  remote host can't silently exfiltrate your clipboard. Committed text is capped
  at 100 KiB.
- **User / importable themes** — drop a `~/.config/jetty/themes/*.toml` palette
  (16 ANSI colors + fg/bg/cursor/selection) and it shows up in the theme picker;
  a user theme can **shadow** a built-in of the same name. Malformed theme files
  are skipped (and logged), never crash.
- **Config hot-reload** — edit `~/.config/jetty/config.toml` (or a theme file)
  while JeTTY is running and it **applies live** — theme, opacity, font, effects,
  scrollback, and more — no restart. It watches the *canonical* file (so symlinked
  dotfiles work), never fights its own writes, and **never rewrites or resets your
  config** on a bad edit (it just waits for the next valid save). `summon_hotkey`
  and `launch_at_login` still take effect on restart.

### Notes
- The file watcher is OS-event-driven (inotify / FSEvents) — zero idle CPU.
- Dependency added: `notify` (cross-platform file watching; no GTK/glib).

---

## [0.15.0] — 2026-07-08

**Run & Notify** — the summon terminal's superpower. Designed from a verified
blueprint, stress-tested by two design critics, then adversarially reviewed.

### Added
- **Command-completion notifications** — when a command finishes while you're
  *not* watching JeTTY (hidden or unfocused), you get a native desktop
  notification naming the **tab**, the **exit code**, the **duration**, and the
  command's **last output line**, plus a taskbar/dock **urgency** hint. It never
  fires while you're looking, and only for commands past a threshold.
  - Config: `notify_on_command_finish` (default on), `notify_min_seconds`
    (default 10), `notify_only_on_failure` (default off), and
    `auto_summon_on_finish` (default off — bring the window back on finish).
    All live in Settings → Shell.
  - Built on v0.14's OSC 133 marks: zero idle/hot-path cost (fires only on the
    completion event), and the D-Bus toast runs off the UI thread so a slow or
    absent notification daemon can never stall the terminal. DE-independent
    (any freedesktop daemon: KDE, GNOME, dunst, mako, …).

### Fixed
- Removed a fabricated perf-HUD string from the self-test/screenshot path — the
  HUD now renders only from real measured values.

### Notes
- **Plain bash** (without `bash-preexec`) reports no command duration, so it
  gets **failure-only** notifications; zsh / fish / powerlevel10k report full
  duration and notify on success too.
- **macOS** gets the guaranteed dock-bounce urgency; rich toasts there need an
  app bundle (future). Linux gets the full desktop notification.

---

## [0.14.0] — 2026-07-08

Shell integration (OSC 133). Designed from a verified blueprint, stress-tested
by two design critics, then adversarially reviewed — with the marks model held
to a strict "correct or absent, never wrong" bar.

### Added
- **Failed-command marker** — a themed, high-contrast bar on the prompt row of
  any command that exited non-zero, so failures are obvious when you scroll back.
- **Prompt jump** — `Ctrl+Shift+Z` / `Ctrl+Shift+X` scroll to the previous /
  next shell prompt.
- **`jetty --print-shell-integration <zsh|bash|fish>`** — prints a snippet you
  opt into from your rc file. It **never edits your dotfiles**. zsh is
  powerlevel10k-aware (set `POWERLEVEL9K_TERM_SHELL_INTEGRATION=true` and JeTTY
  reads p10k's own marks); bash rides `PROMPT_COMMAND` only (no `DEBUG`-trap
  clobber); fish uses events. JeTTY advertises itself via `JETTY`, `JETTY_BIN`,
  `TERM_PROGRAM=jetty`.

  ```sh
  # ~/.zshrc
  [[ -n "$JETTY" ]] && command -v jetty >/dev/null 2>&1 && source <(jetty --print-shell-integration zsh) 2>/dev/null
  ```

### Notes
- OSC 133 is intercepted directly (alacritty's VT engine drops it) with an O(n)
  scanner on the PTY-drain path — no idle/hot-path cost.
- Prompt marks are exact for the common case (default 10k-line scrollback, no
  resize since the mark was made). By design they **degrade to absent — never
  wrong** in two cases: after a window/font resize (reflow rewraps, so marks are
  cleared and re-made on the next prompt), and once the scrollback fills to its
  cap (marks pause until history drops below the cap). JeTTY never draws a marker
  on the wrong row.
- `Ctrl+Shift+Z` / `Ctrl+Shift+X` are now consumed for prompt-jump (plain
  `Ctrl+Z` / `Ctrl+X` still send `0x1a` / `0x18`).
- Inside `tmux`/`screen`, OSC 133 reaches the multiplexer, not JeTTY, unless its
  passthrough is configured.

---

## [0.13.0] — 2026-07-07

The "text attributes" release: JeTTY now renders the SGR attributes it used to
silently drop. Designed from a verified blueprint, stress-tested by two design
critics, then adversarially reviewed before shipping.

### Added
- **Bold and italic** are now rendered (real font faces, not faux weight/skew),
  including bold-italic. Monospace alignment is guaranteed — glyph advances are
  snapped to the cell grid, so bold text can never shift a column, on any font.
- **Underline styles** — single, double, dotted, dashed, and **undercurl**, plus
  **colored underlines** (SGR 58), so nvim / LSP / spellcheck diagnostics show
  their red/colored squiggles.
- **Strikethrough** (SGR 9).
- **Clickable links now actually underline** — v0.12 added Ctrl+click / OSC 8
  link detection, but nothing drew the underline; it's now rendered.
- **Cursor shape** follows `DECSCUSR` (`\e[N q`): block, beam, or underline, and
  a hollow block when the window is unfocused. Drawn as GPU quads.

### Notes
- Bold/italic on fallback (CJK/emoji) glyphs render at regular weight/upright for
  now. **Blink (SGR 5/6) remains intentionally unsupported** — the VT engine
  drops the bit and a blink timer would fight the ~0% idle-CPU design (same call
  as ligatures). No idle-CPU or hot-path regression: decorations are cached and
  only the cursor rebuilds per frame.

---

## [0.12.2] — 2026-07-07

### Fixed
- **Chrome/UI text gets font fallback** — tab titles (now shell-controlled via
  OSC), search queries, and rename buffers are shaped with fallback, so emoji,
  CJK, and symbols missing from the UI font render as real glyphs instead of
  tofu boxes. No idle cost (overlays only shape on rendered frames).
- **Tab-label truncation counts display width** — wide CJK/emoji titles no
  longer overflow the tab pill (main, rename, and detached title bars).

---

## [0.12.1] — 2026-07-07

### Fixed
- **`dir` / `ls -C` no longer print tofu boxes between columns** — the VT
  layer stores a literal `\t` in the cell at each tab stop (so copies
  preserve tabs); the renderer was routing that control char to the
  missing-glyph overdraw. Control-char cells now render as blanks.
- **Tab-rename caret is a plain `|`** — the previous `▏` (U+258F) was a tofu
  box on UI fonts without the Block Elements range.

---

## [0.12.0] — 2026-07-07

A feature release: the table-stakes terminal features JeTTY was still missing —
search, clickable links, smarter tabs — plus full mouse parity for detached
windows. Built from per-feature design blueprints, then adversarially reviewed
(20 raw findings → 15 confirmed → all fixed) before shipping.

### Added
- **Scrollback search** — `Ctrl+Shift+F` opens a themed search bar: type to
  search (case-insensitive, live), `Enter`/`F3` jumps to older matches,
  `Shift+Enter`/`Shift+F3` to newer, with a live `3/17` counter. All visible
  matches are highlighted, the current one accented, and the view scrolls to
  it. `Esc` closes. Streaming output never steals your viewport mid-search.
- **Clickable URLs** — hold `Ctrl` (also `Cmd` on macOS) to underline the link
  under the pointer; `Ctrl+click` opens it. Supports both OSC 8 hyperlinks and
  plain-text URLs — wrapped lines are stitched, trailing punctuation trimmed,
  `http/https/file` schemes only. Works in main *and* detached windows, and
  costs exactly nothing while the modifier isn't held.
- **New tabs open in the current tab's directory** — `Ctrl+Shift+T` / `+`
  inherits the active shell's cwd (Linux `/proc`, macOS `proc_pidinfo`); a
  vanished directory quietly falls back to the old behavior.
- **Shell-driven tab titles (OSC 0/2)** — tabs auto-title from your shell,
  ssh session, or editor; a manual rename (right-click → Rename) always wins.
  The OS window title follows along as `{tab} — JeTTY`, detached windows too.
- **Activity & bell indicators** — an inactive tab that prints output gets a
  small themed dot; a bell gets the urgent color. Cleared the moment you view
  the tab.
- **Configurable scrollback** — `scrollback_lines` in `config.toml`
  (100–100 000, default 10 000) plus a cycler in Settings → Window
  (1k/5k/10k/25k/50k/100k). Applies live to every tab, detached included.
- **Detached windows: full mouse parity** — text selection (including
  Shift+drag over mouse-reporting TUIs and copy-on-select), middle-click
  paste, wheel scrolling with alt-screen arrow translation, and a real
  scrollbar (drag + track-jump), plus the contextual Shift+drag hint.

### Fixed (pre-release adversarial review)
- Background-tab title changes now repaint the tab bar even when the activity
  dot is already lit; detached and attached windows behave identically.
- Search state can no longer leak onto background tabs via tab switch / close /
  reattach — closing the bar clears everywhere, so no hidden full-history
  regex re-scans on resize. Same-size terminal resizes are now no-ops.
- Search matches refresh after a streaming burst (trailing refresh, no idle
  polling) and after a live scrollback shrink.
- Window/font resizes no longer light a false "unseen output" dot on every
  inactive tab (SIGWINCH prompt repaints are ignored for a short window).
- The Shift+drag hint appears only in the window where the drag happened.
- URL trailing-punctuation trimming is linear-time (a pathological line of
  brackets can't stall the event loop); Ctrl+click only fires on the cell
  actually under the pointer; stale link underlines clear on font-size change,
  tab switch, and detach/reattach.
- The search bar measures CJK/double-width queries by display width; IME
  commits respect the modal-popup priority chain.

---

## [0.11.0] — 2026-07-03

A deep correctness + hardening release: a 124-agent audit surfaced 40 bugs, each
double/triple-verified, then fixed and regression-reviewed before shipping.

### Fixed — critical
- **Synchronized-update (DEC 2026) freeze** — an unmatched Begin-Sync (e.g.
  `kill -9` an nvim/zellij mid-redraw) buffered all further output invisibly and
  hung the tab. The 150 ms sync timeout is now serviced (damage-driven, no spin).
- **A dead persisted `shell` no longer bricks startup** — spawn walks a fallback
  chain (config → `$SHELL` → passwd → bash → sh) and shows a one-line notice
  instead of exiting before a window appears.

### Fixed — terminal correctness
- **Mouse wheel now scrolls alt-screen apps** (`less`/`man`/`git log`) via
  ALTERNATE_SCROLL → arrow keys; **mouse drag/motion reports** (modes 1002/1003)
  are now emitted, so tmux/nvim mouse dragging works.
- Right-to-left / bottom-to-top drag selection no longer drops the end cells
  (selection side is read from the sub-cell pointer position).
- Detached-window wheel reports are cell-accurate (were off by one).

### Fixed — input & platform
- **macOS Cmd shortcuts** (Cmd+C/V/A/W/T/Q and font/opacity) actually work now
  instead of typing literal letters into the shell.
- Font & opacity hotkeys are keyed on the logical character, so they work in the
  documented direction on Turkish-Q, German-QWERTZ, and other layouts.

### Fixed — idle CPU & windows
- Hidden / minimized / occluded windows return to true ~0 % idle — no invisible
  render burn from CRT animation, caret bursts, dock/center nudges, or PTY-output
  wakes (now gated on real visibility via `Occluded` tracking).
- Detached windows: live font / UI-font propagation, text selection + click
  forwarding, type-to-bottom, and the advertised keyboard shortcuts now work.
- Focus auto-hide fires correctly when focus leaves JeTTY through a
  detached/Settings window; closing the last main tab keeps detached shells
  alive; several focus/summon/monitor edge cases fixed.

### Fixed — HiDPI, config, misc
- HiDPI unit fixes: tab-tear drop placement, drop-to-reattach hit-testing, and
  the Effects-tab scroll clamp are now consistent across mixed-DPI setups.
- Bounded PTY write queue — a pathological terminal-query flood can no longer
  OOM the app (normal typing/paste is unaffected).
- Atomic config saves preserve a symlinked `config.toml` (dotfiles) and its
  mode; the autostart `.desktop` Exec is escaped per the Desktop Entry spec.
- Wheel/click no longer panics on an empty-tab race; unbounded glyph-fallback
  cache is now capped; scrollbar-drag and touchpad-scroll state no longer leak
  across tabs/windows; and ~a dozen more edge cases.

---

## [0.10.0] — 2026-07-03

A correctness + hardening release. A whole-codebase multi-agent audit
(22 finders → 149 adversarial verifiers) confirmed 79 issues; 68 were fixed
and the diff was re-reviewed for regressions before release. No behavior you
rely on changes — things that used to crash, leak, or misrender no longer do.

### Security
- **Paste-injection blocked** — pasted text now has embedded bracketed-paste
  end markers (`ESC[201~`) stripped, so a crafted clipboard payload can no
  longer break out of the paste guard and run commands.
- **Single-instance IPC socket hardened** — moved from a predictable
  world-writable `/tmp` path to a per-user `0700` runtime directory
  (`$XDG_RUNTIME_DIR`), with a bounded accept-read and inode-guarded cleanup
  so it can't be squatted, wedged by an idle client, or unlink a live socket
  owned by another instance.

### Fixed
- **Output floods can't freeze the UI or run away on memory** — `yes`,
  `cat huge.log`, or a runaway build now feed the terminal in bounded slices,
  keeping input and redraw responsive (Ctrl+C still reaches the shell).
- **Config is never silently reset** — a malformed `config.toml` is preserved
  as `config.toml.bad` and defaults load in memory, instead of the old file
  being wiped and overwritten; partial hand-written configs load per field.
- **Detaching a tab can't crash the app** — if the OS window or GPU context
  fails, the tab is kept and the detach is cancelled instead of aborting the
  whole process (which killed every shell).
- **Text no longer silently blanks out** — the glyph atlas is trimmed again
  (it previously grew until full, then stopped rendering text); wide CJK
  characters keep every following column aligned.
- **Color-scheme queries reflect the live theme** — OSC 10/11/12/4 replies
  follow theme changes; SGR 8 concealed text is actually hidden.
- **No more crashes on odd states** — guarded GPU surface/texture sizing,
  oversized-window resize, non-UTF8 argv, a boot-time clock underflow, and a
  key delivered after the last shell exits.
- **Keyboard encoding** — Ctrl+letter is correct on non-QWERTY layouts,
  Cmd/Super combos no longer leak literal letters into the shell, Shift+Insert
  pastes, and Ctrl/Alt+PageUp/PageDown carry their modifiers.
- **Shells are reliably reaped on tab close** — a wait-based reaper detects
  real exit even when a background job holds the PTY open, with a SIGHUP →
  guarded-SIGKILL escalation so a HUP-ignoring shell can't leak.

### Changed
- **HiDPI chrome** — the Settings panel, context menu, and help overlay scale
  with the UI font on fractional/2× displays instead of overlapping.
- **Detached windows** now handle the mouse wheel and DPI/scale-factor changes,
  and a tab torn out onto a second monitor stays there.

## [0.9.0] — 2026-07-01

A polish + hardening release: detached windows reach full visual parity, the
Settings panel got a design overhaul, and a 60-agent audit's 25 verified bugs
were fixed.

### Added
- **Settings panel redesign** — one consistent control system (segmented
  `‹ value ›` cyclers, knob-in-track switches, matching steppers/chips/sliders),
  calm header/value hierarchy on an 8px grid, inset list wells with accent-edge
  selection, a floating theme menu, roomier tab strip, and a footer hint. All
  theme-derived; verified on dark and light themes.
- **IME & dead-key input** — CJK commit text and dead-key accents (´+e → é) now
  reach the shell; composed text is preferred over the raw key. (No preedit
  overlay yet.)

### Changed
- **Detached windows: full visual parity** — corner radius (all four corners),
  background opacity, the CRT effect, and caret flash now apply to detached
  windows, live with the sliders. No more square opaque pop-outs.
- **PageUp/PageDown are alt-screen aware** — they page inside less/man/vim/htop
  and scroll history in the shell; **Shift+PageUp/Down** always scrolls history.
- **PTY writes are asynchronous** — a dedicated writer thread per session; a
  huge paste into a non-reading program can no longer freeze the UI.
- Closing the last main tab while detached windows exist now pulls a detached
  tab back into the main window instead of quitting (no more killed shells).
- Config saves are atomic (temp + rename); a crash mid-save can no longer wipe
  settings.

### Fixed
- X11 synthetic key presses on focus gain no longer inject garbage (e.g. `~`
  right after an F9 summon).
- Slow touchpad scrolling is no longer dropped (fractional deltas accumulate).
- Spurious mouse-release reports to mouse-mode apps after UI-consumed clicks.
- Ctrl+/ and Ctrl+_ now send readline undo (0x1f); modified Home/End/Delete/
  Insert carry their modifiers.
- Auto-hide no longer fires when focus moves between JeTTY's own windows
  (detached/settings), and a shell exit in a focused detached window no longer
  permanently disables auto-hide.
- CRT roll/flicker/jitter no longer renders while the window is hidden; the CRT
  pass keeps Dropdown top corners square; caret glow no longer bleeds outside
  rounded corners.
- Detached windows debounce resize reflow like the main window; detaching a tab
  invalidates its context menu/drag; menus dismiss on window resize.
- NaN values in a hand-edited config are sanitized; the autostart `.desktop`
  Exec path is properly quoted; two simultaneous cold starts can no longer
  destroy each other's IPC socket; summon animation no longer wedges the event
  loop in Poll when the window hides mid-reveal; mouse reports are clamped to
  the grid.

---

## [0.8.0] — 2026-07-01

Detached tabs grew up: real window chrome, context menus, and drag & drop.

### Added
- **Detached-window chrome** — a detached tab's window now has a proper top bar
  (title + ✕, drag to move), edge/corner resizing, and the bottom perf-HUD strip;
  the terminal grid sits between them. Previously the window was bare and could
  not even be moved.
- **Tab context menu** — right-click a tab for **Detach / Rename / Close Tab**
  (Detach hidden when only one tab). Right-click inside a detached window for
  **Reattach / Copy / Paste**.
- **Drag & drop detach/reattach** — drag a tab off the bar (>24 px) to pop it
  into its own window at the drop point; drag a detached window by its top bar
  and drop it onto the main tab bar to reattach. On platforms without window
  positioning (Wayland), moving falls back to the compositor drag and
  drop-to-reattach is unavailable — no DE-specific code.
- Detach/reattach + the new gestures are documented in the Help overlay ("?")
  and README.

---

## [0.7.0] — 2026-07-01

### Added
- **Detach / reattach a tab into its own window** — `Ctrl+Shift+D` pops the
  active tab out into its own bare terminal window (no tab bar); the same key (or
  closing it) reattaches it to the main window. Damage-driven, so extra windows
  don't cost idle CPU. Desktop-environment-independent (winit, no compositor code).
- **22 built-in themes** (up from 5) — added Nord, Solarized dark/light, One Dark,
  Monokai (+Pro), Everforest, Rosé Pine, Kanagawa, Material, Ayu dark/mirage,
  Tomorrow Night, Oceanic Next, GitHub Dark, Palenight, and Catppuccin Macchiato.
  The theme picker (Settings → Look) is now a scrollable dropdown with a live
  color-swatch preview per theme instead of a fixed card grid.

---

## [0.6.2] — 2026-06-30

### Changed
- **CRT curvature now defaults to 0** — enabling CRT no longer bows the screen
  out of the box; scanlines/mask/bloom/vignette still apply, and the barrel can
  be dialed up under Settings → Effects. Only affects fresh configs.

---

## [0.6.1] — 2026-06-30

### Fixed
- **Effects settings tab layout** — the TINT / COLOR RGB sliders and the ANIMATE
  (roll/flicker/jitter) pills overlapped their section headers ("TINT"+"R",
  "ANIMATE"+"ROLL" rendered on top of each other). Controls now sit beside the
  headers without collision.

---

## [0.6.0] — 2026-06-30

A visual-effects release.

### Added
- **CRT effect** — an optional retro CRT look: screen curvature, scanlines,
  shadow-mask, bloom, chromatic aberration, vignette, and scanline tint, plus
  animated roll / flicker / jitter. All **off by default**, fully tunable.
- **Caret flash & glow** — a brief flash when the caret moves (on by default, but
  it only animates for ~130 ms per move, so idle stays ~0% CPU) and an optional
  additive glow burst at the cursor on each keystroke.
- **Effects settings tab** — a 5th Settings tab grouping all the CRT + caret
  controls. It's the one scrolling tab (its content exceeds the panel height),
  GPU-clipped to the viewport.
- **macOS `.app` icon** — `scripts/make-macos-app.sh` now bundles the Dock/Finder
  icon.

### Notes
- The default look and the ~0% idle profile are unchanged: CRT is off, and the
  caret flash self-drives redraws only while a flash is live, returning to a true
  idle the moment it clears.

---

## [0.5.0] — 2026-06-29

A Settings redesign release.

### Added
- **Tabbed Settings panel** — the long, scroll-heavy Settings dialog is now
  organized into 4 tabs: **Look** (theme, opacity, corner radius), **Fonts**
  (terminal + UI font size & family), **Window** (summon effect, window mode,
  dropdown size, tab-bar position, auto-hide), and **Shell** (shell picker,
  launch at login). The panel is now ~half the height (560 vs 1142px); only the
  active tab's controls show.
- **Shell picker in Settings** — a `‹ … ›` selector under the Shell tab that
  detects installed shells from `/etc/shells` (deduped by basename) and lets you
  pick one, persisted to the `shell` config key. "System default" = auto-detect.
  New tabs use the choice; existing shells are untouched.

### Fixed
- **Explicit copy now clears the selection** — after Ctrl+Shift+C or the
  right-click Copy menu, the selection highlight no longer lingers (it was
  especially stuck over mouse-reporting apps like Claude Code, where a click
  can't clear it). Copy-on-select still keeps the highlight.

---

## [0.4.2] — 2026-06-29

A discoverability release for the Shift+drag selection added in 0.4.0.

### Added
- **Contextual "Hold Shift to select" toast** — when you drag (without Shift)
  inside an app that grabbed the mouse (Claude Code, vim, htop, tmux), JeTTY
  briefly shows a centered hint telling you to hold Shift, right at the moment
  you're trying to select. Throttled, self-dismissing, no idle-CPU cost.
- **Shift+drag is now documented** in the in-app Help overlay (the "?" button)
  and the README keybindings table + feature list. The Help overlay's summon row
  also notes the hotkey is configurable.

---

## [0.4.1] — 2026-06-29

### Added
- **Configurable shell** — new `shell` config key. Empty (default) auto-detects
  in priority order: `$SHELL`, then the passwd login shell, then `/bin/bash`.
  Set an absolute path (e.g. `shell = "/usr/bin/zsh"`) to force a shell — for
  users whose login shell is bash but who live in zsh/fish, so their
  oh-my-zsh/autosuggestions/plugins actually load. Nothing is hardcoded to one
  shell.
- **`jetty --show` / `jetty --hide`** — explicit summon/dismiss commands over the
  single-instance IPC, alongside `--toggle`. Bind a dedicated summon or dismiss
  key in your compositor (Wayland-friendly, no portal/DE-specific code).
  *(Thanks @YKesX, PR #4.)*
- **`JETTY_GPU=high`** (aliases `discrete`, `dgpu`) — env override to select the
  discrete GPU. The default stays LowPower/integrated (a terminal needs no
  discrete power, and the dGPU can destabilize some hybrid compositors); the
  override fixes presentation on dGPU-primary (e.g. NVIDIA-primary) systems where
  the integrated adapter can't drive the compositor surface.
  *(Thanks @YKesX, PR #3.)*

---

## [0.4.0] — 2026-06-29

A usability release: missing glyphs now render, you can select & copy inside
mouse-driven TUIs, the summon hotkey is configurable, and JeTTY can start at
login.

### Added
- **"Launch at login" toggle** in Settings — when ON, writes an XDG autostart
  entry (`~/.config/autostart/jetty.desktop`) so JeTTY starts in the background
  and holds the summon hotkey; OFF removes it. Desktop-environment-independent
  (the freedesktop autostart standard).
- **Configurable summon hotkey** — new `summon_hotkey` config key (default
  `"F9"`). Accepts a bare key (`"F12"`) or a chord (`"Ctrl+Shift+F12"`); an
  invalid value logs a warning and falls back to F9.

### Changed
- **Missing glyphs are drawn from a fallback font instead of tofu boxes.** The
  grid shapes with `Shaping::Basic` (one cell per glyph) which does no font
  fallback, so a char the terminal font lacked (e.g. Claude Code's `⏵⏵`
  permission indicator, U+23F5) rendered as `□`. Such cells are now blanked on
  the main grid and the real glyph is overdrawn from a fallback font at the
  exact cell origin — so it renders like Konsole/Qt while the monospace grid
  stays aligned. Coverage is probed once per char and cached; with no missing
  glyphs the hot path is unchanged.
- **Shift+drag selects text over mouse-reporting TUIs.** Inside apps that grab
  the mouse (Claude Code, vim, htop, tmux), holding **Shift** while dragging now
  forces a local text selection (copy-on-select), the standard terminal
  convention — previously the drag was always forwarded to the app, so you could
  never select & copy there.

---

## [0.3.1] — 2026-06-29

### Added
- `jetty --version` / `--help` now print and exit instead of launching the GUI.

### Changed
- Render hot path no longer allocates a per-frame spans `Vec` (~0.5 MB at full
  screen) — the per-cell spans are passed to glyphon as an iterator; shaping is
  byte-identical. `jetty-bench` gained a `cpu prep` / `gpu exec` split and a
  `JETTY_BENCH_GPU=high` selector, confirming the grid render is CPU-prep-bound.

### Thanks
- The render allocation finding + bench tooling came from @YKesX (PR #2).

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

[0.10.0]: https://github.com/bozdemir/JeTTY/releases/tag/v0.10.0
[0.1.0]: https://github.com/bozdemir/JeTTY/releases/tag/v0.1.0
