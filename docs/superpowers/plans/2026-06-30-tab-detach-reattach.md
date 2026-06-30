# Tab Detach / Reattach Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user pop the active terminal tab out into its own OS window (detach) and merge it back into the main window (reattach), all within the single GUI process, without ever restarting the tab's shell.

**Architecture:** A `Tab` already owns its PTY child + VT grid in-process, so "detach" is just *moving the `Tab` value into a different window*. We keep the existing main window exactly as-is and add a `Vec<DetachedWindow>`, where each `DetachedWindow` owns its own winit window + wgpu render stack and holds **exactly one** `Tab` (no tab bar). Detach moves the active tab out of `App::tabs` into a new `DetachedWindow`; reattach (and the detached window's close button) moves that `Tab` back into `App::tabs`. The pure transfer/lifecycle decisions live in small testable free functions; the winit/wgpu plumbing mirrors the existing Settings window.

**Tech Stack:** Rust 2021, winit (`ApplicationHandler`, multi-window in one event loop), wgpu (Metal/Vulkan), portable-pty, alacritty_terminal.

## Global Constraints

- **Single process only** — no daemon, no second process, no socket changes. Both the main and detached windows run in the same event loop.
- **Never respawn the shell** — detach/reattach must move the `Tab` *by value*. The PTY master, child, reader thread, and `Terminal` grid are preserved untouched. If `PtySession` is ever dropped, its `Drop` reaps the child — so the `Tab` must be `Vec::remove`d and `push`ed, never cloned or recreated.
- **No platform-specific code** — must keep building on Linux X11/Wayland and macOS Metal. Follow the Settings-window pattern (`crates/jetty-app/src/app.rs`), which is already cross-platform.
- **MVP scope** — a detached window holds exactly one tab and has no tab bar. Detaching is only allowed when the main window has **≥ 2 tabs** (so the main window never becomes empty). Multi-tab detached windows and drag-to-tear-off are explicitly out of scope (see "Future Work").
- **Keybindings (decided):** `Ctrl+Shift+D` in the main window detaches the active tab. `Ctrl+Shift+D` in a detached window reattaches its tab to the main window. The detached window's ✕ (CloseRequested) also reattaches (never kills the shell).
- **Reattach target** is always the main window (`App::tabs`), appended as the new last tab and made active.

---

## File Structure

- **Create** `crates/jetty-app/src/detached.rs` — `DetachedWindow` struct (window + per-window render stack + the single `Tab`) and pure helper functions for the transfer/lifecycle decisions. New module so `app.rs` (already ~5k lines) does not grow further with logic that can be unit-tested in isolation.
- **Modify** `crates/jetty-app/src/lib.rs` — add `mod detached;` (and re-export if needed).
- **Modify** `crates/jetty-app/src/input.rs` — add `KeyAction::DetachTab` and decode `Ctrl+Shift+D` (`KeyCode::KeyD`) to it.
- **Modify** `crates/jetty-app/src/app.rs`:
  - Add `detached: Vec<DetachedWindow>` to `App` (+ init).
  - `KeyAction::DetachTab` handling (context-sensitive: main → detach, detached → reattach).
  - `detach_active_tab()`, `reattach_tab(window_id)` methods.
  - Extend `window_event` routing so detached window ids render/route input/resize/close.
  - A `render_detached_window(id)` helper that draws the single tab (reusing the existing terminal draw path, minus the tab bar).

The reference for everything GPU/window-related is the existing Settings window: creation at `app.rs:1783-1831` (`window` + `GpuContext::new` + `TextLayer`/`QuadLayer`), routing dispatch at `app.rs:2955-2958`, teardown at `app.rs:1849-1853`.

---

## Task 1: `Ctrl+Shift+D` decodes to `KeyAction::DetachTab`

**Files:**
- Modify: `crates/jetty-app/src/input.rs` (enum `KeyAction` at `:5`, `decide_key` at `:63`, the `Ctrl+Shift` physical-key match around `:96-108`)
- Test: `crates/jetty-app/src/input.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `KeyAction::DetachTab` — a new unit variant consumed by `App` in Task 4/6.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn ctrl_shift_d_decodes_to_detach_tab() {
    let mods = ctrl_shift(); // existing helper used by the other Ctrl+Shift tests
    let action = decide_key(PhysicalKey::Code(KeyCode::KeyD), key_text(None), mods, /*alt=*/false);
    assert_eq!(action, KeyAction::DetachTab);
}
```

> Match the exact `decide_key` signature/helpers used by the sibling tests in this module (e.g. the `Ctrl+Shift+T → NewTab` test). Copy their harness; do not invent new arguments.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jetty-app --lib ctrl_shift_d_decodes_to_detach_tab`
Expected: FAIL — `no variant named DetachTab` (compile error) or assertion mismatch.

- [ ] **Step 3: Add the variant and the decode arm**

In `enum KeyAction`:

```rust
    /// Detach the active tab into its own window, or — when already in a detached
    /// window — reattach it to the main window (Ctrl+Shift+D).
    DetachTab,
```

In the `Ctrl+Shift` physical-key `match` (next to `KeyCode::KeyW => CloseTab`):

```rust
            PhysicalKey::Code(KeyCode::KeyD) => return KeyAction::DetachTab,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jetty-app --lib ctrl_shift_d_decodes_to_detach_tab`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/jetty-app/src/input.rs
git commit -m "feat(input): decode Ctrl+Shift+D to KeyAction::DetachTab"
```

---

## Task 2: Pure transfer + eligibility logic (`detached.rs` core)

This task contains **no GPU/winit code** — only the decisions, so they are unit-tested without an event loop.

**Files:**
- Create: `crates/jetty-app/src/detached.rs`
- Modify: `crates/jetty-app/src/lib.rs` (add `mod detached;`)
- Test: `crates/jetty-app/src/detached.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub fn can_detach(main_tab_count: usize) -> bool` — true iff `main_tab_count >= 2`.
  - `pub fn take_tab(tabs: &mut Vec<Tab>, idx: usize) -> Option<Tab>` — removes and returns the tab at `idx`, or `None` if out of range; adjusts nothing else.
  - `pub fn reattach_index(tabs_len_after_push: usize) -> usize` — the active index after a reattached tab is appended (`tabs_len_after_push - 1`).
- Consumes: `Tab` from `app.rs`. To keep `Tab` private to `app.rs`, define these as generics over the element type so the module needs no access to `Tab`'s fields: `take_tab<T>(v: &mut Vec<T>, idx: usize) -> Option<T>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_requires_at_least_two_tabs() {
        assert!(!can_detach(0));
        assert!(!can_detach(1));
        assert!(can_detach(2));
        assert!(can_detach(5));
    }

    #[test]
    fn take_tab_removes_and_returns_in_range() {
        let mut v = vec!['a', 'b', 'c'];
        assert_eq!(take_tab(&mut v, 1), Some('b'));
        assert_eq!(v, vec!['a', 'c']);
    }

    #[test]
    fn take_tab_out_of_range_is_none_and_no_mutation() {
        let mut v = vec!['a'];
        assert_eq!(take_tab(&mut v, 5), None);
        assert_eq!(v, vec!['a']);
    }

    #[test]
    fn reattached_tab_becomes_active_last() {
        // after pushing onto a vec that now has length 3, active index is 2
        assert_eq!(reattach_index(3), 2);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p jetty-app --lib detached::tests`
Expected: FAIL — module/functions not defined.

- [ ] **Step 3: Implement the pure helpers**

```rust
//! Pure tab-transfer + eligibility logic for tab detach/reattach. GPU/window
//! plumbing lives in `app.rs`; everything here is unit-testable without an
//! event loop.

/// A tab may be detached only when the main window keeps at least one tab.
pub fn can_detach(main_tab_count: usize) -> bool {
    main_tab_count >= 2
}

/// Remove and return the element at `idx`, or `None` if out of range.
/// Generic so this module never needs visibility into `Tab`'s fields.
pub fn take_tab<T>(v: &mut Vec<T>, idx: usize) -> Option<T> {
    if idx < v.len() {
        Some(v.remove(idx))
    } else {
        None
    }
}

/// Active index after a reattached tab is appended to a vec whose length is now
/// `tabs_len_after_push`.
pub fn reattach_index(tabs_len_after_push: usize) -> usize {
    tabs_len_after_push.saturating_sub(1)
}
```

Add to `crates/jetty-app/src/lib.rs` (next to the other `mod` lines):

```rust
mod detached;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p jetty-app --lib detached::tests`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/jetty-app/src/detached.rs crates/jetty-app/src/lib.rs
git commit -m "feat(detached): pure tab-transfer + eligibility helpers"
```

---

## Task 3: `DetachedWindow` struct + construction

Build a window that owns one tab and its own render stack, mirroring the Settings window. No detach action is wired yet — this task only proves the struct constructs and builds.

**Files:**
- Modify: `crates/jetty-app/src/detached.rs` (add the struct + constructor)
- Modify: `crates/jetty-app/src/app.rs` (make `Tab`, `GpuContext` visible to `detached.rs` as needed — e.g. `pub(crate)`)

**Interfaces:**
- Produces:
  - `pub(crate) struct DetachedWindow { pub window: Arc<Window>, pub gpu: GpuContext, pub text: TextLayer, pub chrome_text: TextLayer, pub quad: QuadLayer, pub offscreen: (wgpu::Texture, wgpu::TextureView), pub tab: Tab }`
  - `impl DetachedWindow { pub(crate) fn new(event_loop: &ActiveEventLoop, tab: Tab, w: u32, h: u32) -> Self }`
- Consumes: the exact `GpuContext::new(window, w, h)`, `TextLayer::new(...)`, `QuadLayer::new(...)`, and offscreen-texture construction used by the Settings window (`app.rs:1800-1831`) and the main window (`app.rs:2722-...`). Copy those call sites verbatim — same descriptors, same font wiring.

- [ ] **Step 1: Make the needed app types crate-visible**

In `app.rs`, change `struct Tab {` → `pub(crate) struct Tab {` and its fields to `pub(crate)` (or add a constructor/accessors). Ensure `GpuContext` is already `pub(crate)` or make it so.

- [ ] **Step 2: Add the struct + constructor in `detached.rs`**

```rust
use std::sync::Arc;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Tab;            // adjust path to wherever Tab lives
// Import GpuContext / TextLayer / QuadLayer from the same paths app.rs uses.

pub(crate) struct DetachedWindow {
    pub window: Arc<Window>,
    pub gpu: GpuContext,
    pub text: TextLayer,
    pub chrome_text: TextLayer,
    pub quad: QuadLayer,
    pub offscreen: (wgpu::Texture, wgpu::TextureView),
    pub tab: Tab,
}

impl DetachedWindow {
    pub(crate) fn new(event_loop: &ActiveEventLoop, tab: Tab, w: u32, h: u32) -> Self {
        // Build window + GpuContext + layers EXACTLY as the Settings window does
        // (app.rs:1800-1831). Title from tab.title. Returns a ready-to-render window.
        // ... (copy the verbatim construction; left explicit during implementation) ...
    }
}
```

> Implementer: lift the construction body from `App::open_settings_window` / `resumed` so font sizes, scale factor, and surface format match. Do not hand-roll new descriptors.

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p jetty-app`
Expected: builds (no behavior change yet; `DetachedWindow::new` may warn as unused — acceptable until Task 4).

- [ ] **Step 4: Commit**

```bash
git add crates/jetty-app/src/detached.rs crates/jetty-app/src/app.rs
git commit -m "feat(detached): DetachedWindow struct mirroring the settings window"
```

---

## Task 4: Wire the detach action (create the window, move the tab)

**Files:**
- Modify: `crates/jetty-app/src/app.rs` (add `detached: Vec<DetachedWindow>` field + init; `detach_active_tab`; `KeyAction::DetachTab` handling for the main window)

**Interfaces:**
- Consumes: `detached::can_detach`, `detached::take_tab`, `DetachedWindow::new`, `Terminal::resize(cols, rows)`.
- Produces: `fn detach_active_tab(&mut self, event_loop: &ActiveEventLoop)`.

- [ ] **Step 1: Add the field**

In `struct App`: `detached: Vec<DetachedWindow>,` and in the constructor: `detached: Vec::new(),`.

- [ ] **Step 2: Implement `detach_active_tab`**

```rust
fn detach_active_tab(&mut self, event_loop: &ActiveEventLoop) {
    if !crate::detached::can_detach(self.tabs.len()) {
        return; // keep at least one tab in the main window
    }
    let idx = self.active;
    let Some(tab) = crate::detached::take_tab(&mut self.tabs, idx) else { return };
    // Keep the main window's active index valid after removal.
    self.active = self.active.min(self.tabs.len().saturating_sub(1));

    // New window sized to the main window's current size is a fine default.
    let (w, h) = self.main_window_size(); // existing accessor or read gpu.config
    let mut dw = crate::detached::DetachedWindow::new(event_loop, tab, w, h);

    // Reflow the moved tab to the new window's cell grid.
    let (cols, rows) = self.cell_grid_for(w, h); // reuse existing grid math
    dw.tab.terminal.resize(cols, rows);
    // PTY must learn the new size too — mirror how the main window resizes a tab.
    self.resize_pty(&mut dw.tab, cols, rows);

    self.detached.push(dw);
    self.apply_theme(); // detached tab inherits the active theme
    self.request_redraw_all();
}
```

> Implementer: `main_window_size`, `cell_grid_for`, and `resize_pty` stand in for the existing main-window equivalents (`app.rs` already computes cols/rows from a pixel size in `WindowEvent::Resized` and resizes the PTY there). Reuse those, do not duplicate the math.

- [ ] **Step 3: Route the keybinding (main window only)**

Where `KeyAction` is handled for the main window, add:

```rust
KeyAction::DetachTab => self.detach_active_tab(event_loop),
```

(Detached-window handling of `DetachTab` is added in Task 7.)

- [ ] **Step 4: Build + manual verify**

Run: `cargo build -p jetty-app && cargo run -p jetty-app`
Manual: open ≥2 tabs (`Ctrl+Shift+T`), press `Ctrl+Shift+D`. Expected: a second window appears showing the detached tab's live shell; the main window keeps the remaining tabs. Typing in the new window is **not** wired yet (Task 6) — it should still render the prior grid.

- [ ] **Step 5: Commit**

```bash
git add crates/jetty-app/src/app.rs
git commit -m "feat(app): detach active tab into a new window on Ctrl+Shift+D"
```

---

## Task 5: Render detached windows

**Files:**
- Modify: `crates/jetty-app/src/app.rs` (`window_event` `RedrawRequested` routing; a `render_detached_window` helper)

**Interfaces:**
- Produces: `fn render_detached_window(&mut self, id: WindowId)`.
- Consumes: the existing main-window terminal draw path (snapshot → quads/text → present), minus the tab bar.

- [ ] **Step 1: Add id-based dispatch at the top of `window_event`**

Alongside the existing settings dispatch (`app.rs:2955-2958`):

```rust
if let Some(pos) = self.detached.iter().position(|d| d.window.id() == id) {
    self.handle_detached_event(pos, event_loop, event);
    return;
}
```

- [ ] **Step 2: Implement `render_detached_window`**

Draw the single `tab.terminal` snapshot into the detached window's surface using its own `gpu/text/quad`, reusing the main window's draw routine. Skip the tab bar (single tab, no strip). Use the same grid-top offset minus the tab bar height.

- [ ] **Step 3: Handle `RedrawRequested` for detached windows**

In `handle_detached_event`, on `WindowEvent::RedrawRequested` call `render_detached_window(... )`.

- [ ] **Step 4: Build + manual verify**

Run: `cargo run -p jetty-app`
Manual: detach a tab; the detached window now redraws (resize it; the content repaints). Output produced by the shell before detach is visible.

- [ ] **Step 5: Commit**

```bash
git add crates/jetty-app/src/app.rs
git commit -m "feat(app): render detached windows (single tab, no tab bar)"
```

---

## Task 6: Input + resize routing for detached windows

**Files:**
- Modify: `crates/jetty-app/src/app.rs` (`handle_detached_event`: keyboard → that tab's writer; `Resized` → reflow + PTY resize; `Focused`; `CursorMoved`/selection optional)

**Interfaces:**
- Consumes: the moved tab's `writer` (PTY input) and `terminal` (resize).

- [ ] **Step 1: Forward keystrokes to the detached tab's PTY**

In `handle_detached_event` for `WindowEvent::KeyboardInput` (pressed): decode text/bytes exactly as the main window does and write to `self.detached[pos].tab.writer`. Reserve `Ctrl+Shift+D` for reattach (Task 7) — check it before forwarding.

- [ ] **Step 2: Handle `Resized`**

On `WindowEvent::Resized(size)`: reconfigure the detached `gpu` surface, recompute cols/rows, `tab.terminal.resize(cols, rows)`, and resize the PTY — mirroring the main window's `Resized` arm.

- [ ] **Step 3: Build + manual verify**

Run: `cargo run -p jetty-app`
Manual: detach a tab, type `ls` + Enter in the detached window → output appears; resize the window → grid reflows.

- [ ] **Step 4: Commit**

```bash
git add crates/jetty-app/src/app.rs
git commit -m "feat(app): route keyboard + resize to detached windows"
```

---

## Task 7: Reattach (keybinding + close button)

**Files:**
- Modify: `crates/jetty-app/src/app.rs` (`reattach_tab`; `DetachTab` + `CloseRequested` in `handle_detached_event`)

**Interfaces:**
- Produces: `fn reattach_tab(&mut self, pos: usize)`.
- Consumes: `detached::reattach_index`.

- [ ] **Step 1: Implement `reattach_tab`**

```rust
fn reattach_tab(&mut self, pos: usize) {
    if pos >= self.detached.len() { return; }
    let dw = self.detached.remove(pos);          // window + render stack dropped here
    let mut tab = dw.tab;                          // MOVE the tab out before drop
    // Reflow to the main window's grid before re-adding.
    let (cols, rows) = self.main_cell_grid();
    tab.terminal.resize(cols, rows);
    self.resize_pty(&mut tab, cols, rows);
    self.tabs.push(tab);
    self.active = crate::detached::reattach_index(self.tabs.len());
    self.apply_theme();
    self.request_redraw_all();
}
```

> Order matters: bind `dw.tab` out of `dw` *before* `dw` (and its `gpu`/`window`) drop, so the `Tab` (and its PTY) survives. Dropping `DetachedWindow` must not drop the `Tab`.

- [ ] **Step 2: Route `Ctrl+Shift+D` and ✕ in detached windows**

In `handle_detached_event`:

```rust
WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
    if decide_key(...) == KeyAction::DetachTab {
        self.reattach_tab(pos);
    } else {
        // forward to PTY (Task 6)
    }
}
WindowEvent::CloseRequested => self.reattach_tab(pos),
```

- [ ] **Step 3: Build + manual verify**

Run: `cargo run -p jetty-app`
Manual: detach a tab, run a long-lived command (e.g. `top`), press `Ctrl+Shift+D` (or click ✕) → the tab returns to the main window as the active tab, `top` still running, shell never restarted.

- [ ] **Step 4: Commit**

```bash
git add crates/jetty-app/src/app.rs
git commit -m "feat(app): reattach detached tab to the main window (key + close)"
```

---

## Task 8: Lifecycle edge cases

**Files:**
- Modify: `crates/jetty-app/src/app.rs`

- [ ] **Step 1: Main window close with detached windows alive**

On the **main** window's `CloseRequested`/exit: reattach is moot (we're quitting). Ensure app exit drops all `detached` windows (and thus their tabs / PTYs reap cleanly). Verify no zombie shells with `pgrep`.

- [ ] **Step 2: Shell exit inside a detached tab**

The reader thread sets `child_exited`. After draining output, if a detached tab's `child_exited()` is true, close that detached window (drop it) instead of reattaching an empty shell. Mirror the main window's per-tab child-exit handling.

- [ ] **Step 3: Detach eligibility guard surfaces nothing when blocked**

Pressing `Ctrl+Shift+D` with a single tab in the main window is a silent no-op (`can_detach` is false). Confirm no panic, no stray window.

- [ ] **Step 4: Build + run the full suite**

Run: `cargo build && cargo test && cargo clippy`
Expected: builds; all tests pass; no new clippy warnings beyond the pre-existing baseline.

- [ ] **Step 5: Commit**

```bash
git add crates/jetty-app/src/app.rs
git commit -m "feat(app): handle detached-window lifecycle (exit, shell-exit, guard)"
```

---

## Verification (whole feature)

- `cargo test` green across `jetty-core`, `jetty-render`, `jetty-app`.
- Manual matrix: detach with 2+ tabs; type in detached window; resize detached window; reattach via key; reattach via ✕; shell `exit` in detached window closes it; `Ctrl+Shift+D` with 1 tab is a no-op; quit main window leaves no zombie shells (`pgrep -lf release/jetty` clean after quit).
- `cargo clippy` introduces no new warnings.

## Future Work (explicitly out of MVP scope)

- Multi-tab detached windows + a tab bar in detached windows.
- Drag-to-tear-off (drag a tab off the strip to detach; drag back to reattach), with drag tracking + drop hit-testing + a drag-ghost.
- Reattach to the *originating* window instead of always the main window (origin tracking).
- Persisting detached-window layout across app restarts.
