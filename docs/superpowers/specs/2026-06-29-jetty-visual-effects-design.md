# JeTTY Visual Effects — Design Spec

**Date:** 2026-06-29
**Status:** Approved (brainstorming) → ready for implementation plan
**Scope:** CRT post-processing, scanlines, keypress caret effect, a new Settings "Effects" tab with parametric controls, and a fix for the missing macOS app icon.

---

## 1. Overview & Goals

Add high-end, polished, **parametric** visual effects to JeTTY, controllable from a new Settings tab:

1. **CRT** post-process — animated tier: barrel curvature, RGB shadow-mask/aperture grille, bloom/glow, chromatic aberration, vignette, plus optional rolling scanline, flicker, and jitter.
2. **Scanlines** — part of the CRT pass (intensity + tint).
3. **Caret effect on keypress** — two paths, both shipped: a CPU-only flash+pulse (default), and an optional GPU glow/ripple.
4. **Effects settings tab** — a 5th tab with sliders, toggles, and full RGB color controls, **vertically scrollable**, persisted to `config.toml`.
5. **macOS app icon fix** — wire up the existing (correct) bundling script and document it.

### Non-negotiable principle: preserve 0-CPU idle

JeTTY today renders **0 frames when idle** (damage-driven redraw; `app.rs:4229–4240`, Tier-B offscreen guard `app.rs:3820`). This is a headline feature (see `docs/perf-budget.md`). The design preserves it:

- **All effects default OFF** except the cheap caret flash+pulse (see §9). Out-of-box behavior and battery profile are unchanged.
- CRT enabled but **rolling/flicker/jitter all OFF** → static CRT that composites into frames that are *already happening* → **still 0-CPU idle**.
- Any of rolling/flicker/jitter ON → continuous redraw **only while CRT is enabled**; the user opts into this cost explicitly per-toggle.
- Caret flash+pulse → a **bounded** redraw burst per keystroke that stops at `t≥1` and returns to idle. Acceptable.

## 2. Non-Goals (v1)

- True multi-pass separable/gaussian bloom with ping-pong textures (v1 uses an in-shader multi-tap glow). Follow-up.
- Cursor *shape* variants (bar/underline) — JeTTY renders a block cursor only; effects target the block. Out of scope.
- macOS code-signing / notarization / `.dmg` / CI automation of the bundle (the icon fix is script + README + hardening only).
- Externalized/hot-reloaded shader source — shaders stay embedded `const &str` like every existing effect.
- Per-summon-effect coupling — effects are **global**, independent of the summon selector.

## 3. Architecture Principle (§A)

- Effects are **global, persisted** settings in `config.toml`, applied to all rendering.
- Each animated CRT sub-feature (rolling scanline, flicker, jitter) is an **independent toggle** so battery cost is user-controlled and idle can stay at 0 CPU.
- No observer/event bus — the render loop reads the effect params each frame and passes them as GPU uniforms (the existing pattern; `phosphor.rs:192–202`).

## 4. Rendering Design (§B)

### 4.1 New module `crates/jetty-render/src/crt.rs`

Modeled byte-for-byte on `phosphor.rs`:

- Embedded `const CRT_SHADER: &str` (WGSL).
- `struct Crt { pipeline, uniform_buf, bind_group, sampler }`.
- `Crt::new(device, format) -> Crt`.
- `Crt::apply(device, queue, encoder/dst_view, src_view, width, height, &CrtUniform)` — a single fullscreen-triangle (`vi < 3`) sample pass.
- Exported from `crates/jetty-render/src/lib.rs`.

Uniform packed as a flat `[f32; N]` (no WGSL `vec3` — host/GPU layout parity; see `phosphor.rs:18`), written via `queue.write_buffer`.

### 4.2 CRT pass shader features (single sample pass)

Input = the fully composited scene sampled from the offscreen texture (linear space). Applied in shader:

1. **Curvature** — barrel-warp the sample UV (`crt_curvature`); samples outside [0,1] read black (CRT bezel).
2. **RGB shadow-mask / aperture grille** — per-output-pixel RGB cell modulation (`crt_mask_intensity`).
3. **Scanlines** — vertical beam-profile darkening (`crt_scanline_intensity`), tinted by `crt_scanline_tint` (RGB). Rolling vertical phase added from the `time` uniform **only when `crt_animate_roll`**.
4. **Bloom/glow** — in-shader multi-tap (e.g. 9–13 weighted taps) thresholded glow, added back (`crt_bloom`). Single pass, no ping-pong.
5. **Chromatic aberration** — R/G/B sampled at slightly divergent UVs scaled by `crt_chromatic`, growing toward edges.
6. **Vignette** — radial edge darkening (`crt_vignette`).
7. **Flicker** — global brightness modulation from `time` **only when `crt_flicker`**.
8. **Jitter** — sub-pixel horizontal sample offset from `time` **only when `crt_jitter`**.
9. **Rounded-corner coverage** — the shader computes its own rounded-rect SDF coverage on the **un-warped output coords** and sets output alpha, replicating `phosphor.rs:74`'s `cov` gate, so the transparent rounded window corners survive the warp.

### 4.3 Frame routing when CRT enabled

```
scene_view = offscreen  (when crt_enabled OR tier_b_summon_active)

[clear+bg  app.rs:3936] → [text  app.rs:3950] → [chrome/overlays 3961–4149] → offscreen
(summon composite, if any, into offscreen)
CRT pass: sample offscreen → surface view (does curvature/mask/scanline/bloom/CA/vignette/flicker/jitter + corner SDF alpha)
frame.present()  app.rs:4241
```

Concrete edits in `app.rs`:

- **Persistent offscreen:** extend the allocation guard (`app.rs:3820`) and scene-view routing (`app.rs:3921–3928`) so `scene_view = &offscreen.1` when `crt_enabled || tier_b_active`. `make_offscreen` (`app.rs:863`) is reused as-is.
- **Skip corner mask when CRT on:** the CRT shader does the rounding; `mask.apply` (`app.rs:4155`) runs only when CRT is OFF (today's path, unchanged).
- **CRT apply** runs after the scene/summon composite, as the final blit to the surface, before `frame.present()`.
- **New App fields** (beside the effect structs, `app.rs:231–245`): `crt: Option<jetty_render::Crt>`, `caret_fx: Option<jetty_render::CaretFx>`; built where `phosphor`/`bayer_reveal` are constructed (~`app.rs:640`).

### 4.4 Summon × CRT interaction (known risk)

Both want `self.offscreen`. v1 rule: when a **Tier-B summon is active** (Liquid/Focus — short, a few hundred ms) **and** CRT is enabled in the same frame, **CRT is bypassed for those frames** (the summon owns the offscreen; the brief flagged double-sample/double-alloc as a risk). Tier-A summons (Bayer/Phosphor) write into the scene target (offscreen) and CRT then samples normally. This must be verified in the plan's review step.

### 4.5 Animation clock & redraw guard

- Add a free-running `crt_clock: std::time::Instant` (started once). Per frame, `time = crt_clock.elapsed().as_secs_f32()` → uniform (shader uses `fract`/`sin`, so unbounded growth is fine).
- Extend the self-redraw guard (`app.rs:4236`) to keep scheduling redraws when `crt_enabled && (crt_animate_roll || crt_flicker || crt_jitter)`, **and** when `caret_anim.is_some()`. Otherwise idle returns to 0 CPU.

### 4.6 Constraints honored

- sRGB surface (`Rgba8UnormSrgb`); all math in linear, like existing passes.
- Alpha mode PostMultiplied(macOS/Metal)/PreMultiplied(Vulkan) — match Phosphor's blend choices via `gpu.premultiply_clear` (`app.rs:3946`); the corner SDF gate prevents re-opaquing transparent corners.
- Default GPU `PowerPreference::LowPower` — CRT cost is the user's explicit opt-in.

## 5. Caret Effect Design (§C)

### 5.1 Trigger

In the `KeyAction::Send(bytes)` arm (`app.rs:3661–3681`), after the PTY write: if the caret effect is enabled and `bytes` is a **printable keystroke** (not a pure control/escape sequence — gate on bytes), set `self.caret_anim = Some(Instant::now())` and request redraw. Re-arming on every keystroke means rapid typing yields a continuous pulse (desirable). Coalescing is implicit (start = now each time).

### 5.2 State & timing

- New `caret_anim: Option<Instant>` on `App` (next to `summon_anim`, `app.rs:317`).
- Per frame near `app.rs:3844`: `let caret_t = self.caret_anim.map(|s| (s.elapsed().as_secs_f32() / (self.fx.caret_flash_ms/1000.0)).min(1.0));`
- Clear `caret_anim` at `t≥1`; include `caret_anim.is_some()` in the redraw guard (§4.5).

### 5.3 Path 1 — Flash + pulse (CPU-only, default ON)

In `text.rs` cursor rendering (`text.rs:557–573`), thread `caret_t` + caret params into `render_to` (`text.rs:405`) and modulate the cursor `TextArea` **before `prepare()`**, no GPU code:

- **Color:** lerp `default_color` from `cursor_rgb` toward `caret_flash_color` by an ease-out curve of `caret_t`.
- **Pulse:** scale `1.0 → ~1.15 → 1.0` (ease-out) via `TextArea.scale`, anchored so the glyph stays centered on the cell.
- (Optional subtle vertical recoil via `top` nudge — keep minimal.)

### 5.4 Path 2 — Glow/ripple (GPU, optional toggle, default OFF)

New module `crates/jetty-render/src/caret_fx.rs` (phosphor.rs pattern): an additive fullscreen pass that draws, around the cursor cell, a soft glow halo + an expanding ring, driven by uniforms `{ caret_t, cursor_px_x, cursor_px_y, cell_w, cell_h, color, intensity }`. Runs only when `caret_glow_enabled`. When off, no pass is dispatched. Exported from `lib.rs`.

`cursor` pixel position is the existing mapping `left = cursor_col*cell_w`, `top = cursor_row*cell_h + top_offset` (`text.rs:565–566`).

## 6. Settings "Effects" Tab (§D) — scrollable

### 6.1 5th tab plumbing (`panel.rs` + callers)

- `TAB_NAMES: [&str; 4] → [&str; 5]` adding `"Effects"` (`panel.rs:110`).
- `tab_w = (PANEL_W - 32.0) / 4.0 → / 5.0` (`panel.rs:340`); `tab_rects: [Rect; 4] → [Rect; 5]` (`panel.rs:342`) + loop (`panel.rs:343`).
- Lift every `4`-tab cap to `5`: `active_tab.min(3) → .min(4)` (`panel.rs:211`), doc/`_ =>` arms (`panel.rs:161,207,307`), and the app-side `self.settings_tab = i.min(3) → .min(4)` (`app.rs:2027`).
- Add an `active_tab == 4` arm to the band-top `match` (`panel.rs:287–311`) laying out Effects widgets.

### 6.2 Widget inventory (top→bottom, grouped)

**CRT group**
- `Enable CRT` — toggle
- `Curvature` — slider
- `Scanline intensity` — slider
- `Mask intensity` — slider
- `Bloom` — slider
- `Chromatic aberration` — slider
- `Vignette` — slider
- `Scanline tint` — **RGB triple** (3 mini-sliders R/G/B on one band row)
- `Animate:` `Rolling` · `Flicker` · `Jitter` — 3 toggles on one band row

**Caret group**
- `Flash + pulse` — toggle
- `Glow / ripple` — toggle
- `Flash duration` — slider
- `Flash color` — **RGB triple** (one band row)

Widget geometry reuses existing patterns in `panel.rs`: slider = opacity slider (track `panel.rs:365`, handle `:368`, fill `:369`); toggle = focus-autohide pill; RGB triple = three narrowed (~110px) instances of the slider laid horizontally in one band. `build_panel(...)` (`panel.rs:167`) signature extended with the effect params; new widget `Rect`s added to the returned `PanelView`/`PanelGeom`.

### 6.3 Vertical scrolling (chosen over growing the window)

The content (~16 bands) exceeds `PANEL_H=560`, so the Effects tab scrolls inside the fixed panel:

- **State:** `effects_scroll: f32` on `App` (default 0).
- **Layout:** `build_panel` lays Effects widgets at their natural Y and returns the **content height**; the visible content region is `[content_top, PANEL_H - bottom_margin]`.
- **Render clipping:** set the render pass **scissor rect** (`set_scissor_rect`) to the content region for the Effects tab's quad + text draws, and offset every Effects widget Y by `-effects_scroll`. The tab strip and panel chrome are drawn outside the scissor so they stay fixed.
- **Wheel input:** on `WindowEvent::MouseWheel` over the settings window while on the Effects tab, `effects_scroll = clamp(effects_scroll - delta·step, 0, content_h - visible_h)` and request redraw. (Plan locates the settings-window wheel handler.)
- **Hit-testing:** in `input.rs`, Effects-tab widget hit-tests add `effects_scroll` back into the compared Y and reject clicks outside the content region.
- **Indicator:** a thin scrollbar indicator on the content's right edge (reuse the terminal scrollbar quad pattern) when `content_h > visible_h`.

### 6.4 Input wiring (`input.rs`)

- New `MouseAction` variants beside existing ones (`input.rs:234–305`): e.g. `ToggleCrt`, `StartCrtCurvatureDrag`, `StartScanlineIntensityDrag`, `StartMaskIntensityDrag`, `StartBloomDrag`, `StartChromaticDrag`, `StartVignetteDrag`, `StartScanlineTint{R,G,B}Drag`, `ToggleCrtRoll`, `ToggleCrtFlicker`, `ToggleCrtJitter`, `ToggleCaretFlash`, `ToggleCaretGlow`, `StartCaretDurationDrag`, `StartCaretColor{R,G,B}Drag`.
- Hit-tested in `decide_mouse_press` (`input.rs:325`) **after** the tab strip (`input.rs:335`, so tab switching is unaffected), in priority order, with the `effects_scroll` offset applied.

## 7. Config / State / Persistence (§E)

### 7.1 `EffectsConfig` struct (new, in `config.rs`)

To avoid bloating the 4000-line `app.rs`, group all effect params in one struct, embedded in `Config` behind a single `#[serde(default)]` field, with each inner field also `#[serde(default)]` for forward/backward compat:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectsConfig {
    #[serde(default = "df_false")]      pub crt_enabled: bool,
    #[serde(default = "df_curvature")]  pub crt_curvature: f32,        // 0.0..=1.0
    #[serde(default = "df_scanline")]   pub crt_scanline: f32,         // 0.0..=1.0
    #[serde(default = "df_mask")]       pub crt_mask: f32,             // 0.0..=1.0
    #[serde(default = "df_bloom")]      pub crt_bloom: f32,            // 0.0..=1.0
    #[serde(default = "df_chromatic")]  pub crt_chromatic: f32,        // 0.0..=1.0
    #[serde(default = "df_vignette")]   pub crt_vignette: f32,         // 0.0..=1.0
    #[serde(default = "df_white")]      pub crt_scanline_tint: [f32; 3],
    #[serde(default = "df_false")]      pub crt_animate_roll: bool,
    #[serde(default = "df_false")]      pub crt_flicker: bool,
    #[serde(default = "df_false")]      pub crt_jitter: bool,
    #[serde(default = "df_true")]       pub caret_flash_enabled: bool,
    #[serde(default = "df_false")]      pub caret_glow_enabled: bool,
    #[serde(default = "df_flash_ms")]   pub caret_flash_ms: f32,       // 60..=400
    #[serde(default = "df_white")]      pub caret_flash_color: [f32; 3],
}
```

`Config` gains `#[serde(default)] pub effects: EffectsConfig` and `impl Default`. Old configs (no `[effects]` table) load fine; all fields fall back to defaults. Values are clamped to their ranges on load.

### 7.2 Propagation

- Mirror fields on `App` (a single `fx: EffectsConfig` runtime copy is simplest), loaded next to `settings_tab` (~`app.rs:687`).
- `persist()` (`app.rs:784–810`) maps `self.fx` into the `Config { … }` literal.
- `handle_settings_action` (`app.rs:1905`, slider pattern at `:1922`): each new `MouseAction` updates `self.fx`, calls `self.persist()`, and `request_redraw()` on both windows.
- Render loop reads `self.fx` each frame and packs uniforms for `crt.apply` / `caret_fx.apply` / `text.render_to`.

## 8. macOS Icon Fix (§F)

Root cause: the app runs as a **bare binary**; winit's `window_icon` is a no-op on macOS (`jetty-platform/src/window.rs:10–21`). The Dock/Finder icon requires a `.app` bundle with `CFBundleIconFile` → `.icns`. The bundle script `scripts/make-macos-app.sh` already does this **correctly** but is unused/undocumented.

Fix:
1. **README** macOS section (~lines 78–89): after `cargo build --release`, run `sh scripts/make-macos-app.sh`, then `open JeTTY.app` (not the bare binary). Note Dock icon caching may need `killall Dock` once.
2. **Harden the script:** check `command -v sips`/`iconutil`; verify the `.icns` was produced; inject `CARGO_PKG_VERSION` into `CFBundleVersion`/`CFBundleShortVersionString` (currently hardcoded `0.1.0`).
3. **Optional follow-up (noted, not built):** `build.rs`/`cargo-bundle` automation + a macOS CI job.

## 9. Parameter Reference & Defaults

| Param | Type | Range | Default | Idle-safe when off? |
|---|---|---|---|---|
| `crt_enabled` | bool | — | **false** | n/a |
| `crt_curvature` | f32 | 0.0–1.0 | 0.30 | yes (static) |
| `crt_scanline` | f32 | 0.0–1.0 | 0.50 | yes (static) |
| `crt_mask` | f32 | 0.0–1.0 | 0.30 | yes (static) |
| `crt_bloom` | f32 | 0.0–1.0 | 0.40 | yes (static) |
| `crt_chromatic` | f32 | 0.0–1.0 | 0.20 | yes (static) |
| `crt_vignette` | f32 | 0.0–1.0 | 0.40 | yes (static) |
| `crt_scanline_tint` | RGB | 0–1 each | [1,1,1] | yes (static) |
| `crt_animate_roll` | bool | — | **false** | **breaks idle when ON** |
| `crt_flicker` | bool | — | **false** | **breaks idle when ON** |
| `crt_jitter` | bool | — | **false** | **breaks idle when ON** |
| `caret_flash_enabled` | bool | — | **true** | bounded burst only |
| `caret_glow_enabled` | bool | — | **false** | bounded burst only |
| `caret_flash_ms` | f32 | 60–400 | 130 | — |
| `caret_flash_color` | RGB | 0–1 each | [1,1,1] | — |

**Default-behavior note for review:** everything ships OFF *except* `caret_flash_enabled = true` (cheap, no idle cost, the explicitly-requested headline effect). If you'd rather ship 100% opt-in, flip this default to `false` — flag it during spec review.

## 10. File-by-File Change Map

**New**
- `crates/jetty-render/src/crt.rs` — CRT post pass (struct + WGSL + apply).
- `crates/jetty-render/src/caret_fx.rs` — optional caret glow/ripple pass.

**Modified**
- `crates/jetty-render/src/lib.rs` — export `Crt`, `CaretFx`.
- `crates/jetty-render/src/text.rs` — caret flash+pulse CPU path in cursor render (`:405`, `:557–573`).
- `crates/jetty-render/src/panel.rs` — 5th tab, Effects layout, RGB triples, scroll layout + content height, scissor.
- `crates/jetty-app/src/app.rs` — `crt`/`caret_fx` fields, `crt_clock`, `caret_anim`, `effects_scroll`, `fx`; offscreen routing + persistent alloc; CRT/caret dispatch; redraw guard; keypress trigger; `persist`; `handle_settings_action`; wheel→scroll; `min(3)→min(4)`.
- `crates/jetty-app/src/config.rs` — `EffectsConfig` + `Config.effects` + defaults + clamps.
- `crates/jetty-app/src/input.rs` — new `MouseAction`s + Effects-tab hit-tests (scroll-aware).
- `scripts/make-macos-app.sh` — hardening + version injection.
- `README.md` — macOS bundle/run instructions.

## 11. Testing & Verification

- **Unit (`config.rs`):** `EffectsConfig` serde round-trip; old config (no `[effects]`) loads with all defaults; out-of-range values clamp on load.
- **Shader compile:** WGSL for `crt.rs`/`caret_fx.rs` compiles under naga (a `device.create_shader_module` smoke path or a build-time naga check).
- **0-CPU idle regression (critical):** with CRT enabled but roll/flicker/jitter OFF, confirm no redraws are scheduled while idle (the headline invariant). With an animate toggle ON, confirm continuous redraw only while enabled, and 0-CPU returns when disabled.
- **Caret:** flash+pulse fires on printable keys, not on control/escape sequences; burst stops at `t≥1`.
- **Perf budget:** measure CRT-on frame cost against `docs/perf-budget.md`; record the delta.
- **Visual:** manual checklist + regenerate effect screenshots via the repo's existing screenshot path (used in commit `6a50ec3`).
- **macOS icon:** build bundle via the hardened script, `open JeTTY.app`, confirm Dock/Finder icon renders (after `killall Dock` if cached).
- **5-tab layout:** verify tab labels still fit/center at `tab_w/5`, the Effects tab is reachable, all widgets are hittable (no stray `min(3)` cap left), and scroll clamps correctly at both ends.

## 12. Key Risks

1. **0-CPU idle regression** — highest. Mitigated by default-OFF + per-animation toggles + an explicit idle regression test.
2. **Alpha/sRGB correctness at transparent rounded corners** — the CRT shader must replicate Phosphor's `cov` SDF gate or it re-opaques the corners / breaks window transparency.
3. **Offscreen reuse: summon × CRT** — v1 bypasses CRT during Tier-B summons; verify no double-sample/double-alloc.
4. **Scroll in a hand-built GPU panel** — scissor + scroll-offset hit-testing is new; off-by-one risks in clamp and click mapping.
5. **Scattered `min(3)`/"0..=3" caps** — missing one silently makes the Effects tab unreachable or its widgets unhittable.
6. **Bloom quality vs cost** — single-pass in-shader glow is the v1 compromise; true bloom deferred.

## 13. Follow-ups (out of scope)

- True separable/gaussian bloom (ping-pong).
- Cursor shape variants + per-shape effects.
- macOS `.app` automation (build.rs/cargo-bundle), code-signing, notarization, `.dmg`, macOS CI.
- Animation **intensity** sliders for roll/flicker/jitter (v1 ships on/off toggles with baked intensities).
