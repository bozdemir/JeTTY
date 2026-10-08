//! Crt post-effect — a GPU pass that samples a fully-rendered offscreen scene
//! and writes a CRT-styled result to the surface: barrel/curvature warp (with a
//! transparent bezel outside the tube), chromatic aberration, a monochrome
//! phosphor/paper remap, tinted scanlines, a shadow-mask / aperture grille,
//! bloom, film grain, a radial vignette, a 1-bit ordered dither and the event
//! glitch. It ALSO computes its own rounded-corner alpha (on the UN-warped
//! output coords) so the transparent rounded window corners are restored — the
//! corner mask pass is skipped while CRT is on, so the CRT pass owns the corners.
//!
//! **Speed.** Every feature is a WGSL `override` constant (`FEAT_*`, one bit of
//! [`CrtKey`]): a slider at 0 compiles its code out of the pipeline instead of
//! paying for it per pixel. One pipeline per feature combination is built by
//! [`Crt::prepare`] when the settings change (never per frame) and cached; a
//! combination the user never selects is never compiled. With no curvature,
//! jitter or glitch the scene is read with ONE `textureLoad` per pixel (the
//! sample position is the pixel itself). Bloom runs at quarter resolution — a
//! bright-pass downsample (4×4 texels → 1, four bilinear taps) into small
//! `Rgba16Float` textures owned by this pass, blurred (separable 9-tap Gaussian,
//! twice) only when `crt_bloom_radius` > 0 — and the composite adds it with one
//! bilinear tap plus the pixel's own bright-pass (the old in-shader bloom read
//! 13 full-res taps).
//!
//! **Bloom keys on the light ABOVE the background** (the theme background, or
//! the phosphor's unlit "paper" color): a light theme's bright background no
//! longer blooms itself into a wash-out, and on dark themes the result is the
//! old look (the background is ~black).
//!
//! **Uniforms.** Binding 0 is the frozen 64-byte [`CrtUniform`]; binding 3 the
//! 96-byte [`CrtExtUniform`] (theme colors, phosphor, grain/dither, glitch).
//! Both are built by ONE helper, [`CrtParams::build`], from settings-level
//! [`CrtSettings`] + per-frame [`CrtFrame`] — the main window, detached windows
//! and jetty-shot all go through it.
//!
//! Every effect is a no-op at its `param == 0`, so all-zero params reduce to a
//! passthrough blit (plus corner rounding). Animation reads the `time` + `flags`
//! fields: bit0 = roll (scanline crawl), bit1 = flicker (subtle brightness
//! wobble), bit2 = jitter (sub-pixel horizontal sample shift); each collapses to
//! the EXACT static result when its bit is clear.
//!
//! Fullscreen-triangle passes with a `replace` blend (the main pass owns every
//! output pixel). Color and alpha are both multiplied by the corner+bezel
//! coverage so corners fade out cleanly (mirrors mask.rs's dst-multiply
//! convention — keeps premultiplication consistent, never re-opaques corners).
//!
//! Self-contained: our own wgpu/WGSL, no desktop-environment / compositor /
//! OS-specific code.

use std::cell::RefCell;

// 64-byte uniform. No vec3<f32> so the host (#[repr(C)]) layout matches the WGSL
// struct byte-for-byte. Field byte offsets (Rust == WGSL), all naturally aligned:
//   resolution    vec2<f32>  @  0   (align 8)
//   curvature     f32        @  8
//   scanline      f32        @ 12
//   mask          f32        @ 16
//   bloom         f32        @ 20
//   chromatic     f32        @ 24
//   vignette      f32        @ 28
//   tint          vec4<f32>  @ 32   (align 16; rgb + pad)
//   corner_radius f32        @ 48   (BOTTOM corners)
//   time          f32        @ 52   (animation phase, seconds)
//   flags         u32        @ 56   (roll/flicker/jitter bitfield)
//   corner_radius_top f32    @ 60   (TOP corners; 0 when top-flush Dropdown)
//   => size 64, align 16. FROZEN: new parameters go into `CrtExtUniform`.

/// CRT animation flag bits packed into [`CrtUniform::flags`]. This is the single
/// source of truth for the bit layout: [`CrtParams::build`] ORs these together,
/// and the WGSL fragment tests the SAME bit positions with literal masks
/// (`(flags & 1u) != 0u`, etc.). Keep the WGSL masks in sync with these values.
pub const CRT_FLAG_ROLL: u32 = 1 << 0; // bit0: rolling scanline crawl
pub const CRT_FLAG_FLICKER: u32 = 1 << 1; // bit1: global brightness flicker
pub const CRT_FLAG_JITTER: u32 = 1 << 2; // bit2: sub-pixel horizontal jitter

/// Bloom blur step (quarter-resolution texels between Gaussian taps) at
/// `crt_bloom_radius` = 1. Radius 0 (the default — the pre-v2 tight glow) runs
/// NO blur pass: the halo is the 4×4 bright-pass downsample plus the bilinear
/// upsample. Up to radius 0.5 the separable 9-tap Gaussian runs once (H, V);
/// above, twice (H, V, H, V) with a smaller step — σ ≈ 4.9·radius quarter-res
/// texels (≈ 20 px at 1) either way, continuous across the switch, and no tap
/// ghosting at wide radii (see [`bloom_blur`]).
pub const BLOOM_STEP_MAX: f32 = 2.0;

/// The rate (frames per second) the animated seeds (`crt_grain_animate`, the
/// glitch tear pattern) advance at: the time is quantized to this clock, so a
/// faster repaint (typing) never makes grain or a glitch strobe faster.
pub const ANIM_SEED_FPS: f64 = 30.0;

/// Texture format of the quarter-resolution bloom targets.
const BLOOM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Pieces shared by the main and the bloom shader modules: the extension
/// uniform (binding 3), the phosphor remap, the fullscreen-triangle vertex
/// stage. WGSL has no includes, so the sources are joined in Rust.
const COMMON_WGSL: &str = r#"
// 96-byte extension uniform (binding 3). Every field is a vec4 — the host
// layout (`CrtExtUniform`, [f32; 4] fields) matches byte-for-byte:
//   bg     @ 0   rgb theme background (shader color space), w = 1 when the scene is premultiplied
//   fg     @16   rgb theme foreground, w = bloom blur step (quarter-res texels)
//   paper  @32   rgb phosphor "unlit" color (paper), w = bloom radius 0..1
//   phos   @48   rgb phosphor "lit" color (ink), w = hue keep 0..1
//   fx     @64   x grain 0..1, y dither/grain cell px, z grain seed, w DPI scale
//   glitch @80   x burst intensity 0..1, y tear seed, zw reserved
struct X {
    bg: vec4<f32>,
    fg: vec4<f32>,
    paper: vec4<f32>,
    phos: vec4<f32>,
    fx: vec4<f32>,
    glitch: vec4<f32>,
};
@group(0) @binding(3) var<uniform> x: X;

override FEAT_PHOS: bool = true;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var verts = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let v = verts[vi];
    var o: VsOut;
    o.pos = vec4(v, 0.0, 1.0);
    // uv in 0..1, y down (matches the offscreen frame's orientation).
    o.uv = vec2(v.x * 0.5 + 0.5, 1.0 - (v.y * 0.5 + 0.5));
    return o;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// Perceptual lightness (≈ sRGB-encoded luminance) of a linear color.
fn lightness(c: vec3<f32>) -> f32 {
    return sqrt(max(luma(c), 0.0));
}

// Divisor that turns a scene texel into its straight (un-premultiplied) color.
fn alpha_div(a: f32) -> f32 {
    return select(1.0, max(a, 0.0001), x.bg.w > 0.5);
}

// Position of `c` on the `lo` (0) → `hi` (1) axis, in perceptual lightness and
// unclamped. Works for a dark pair (hi brighter) and a light one (hi darker); a
// degenerate pair (lo ≈ hi) counts as a tiny step instead of dividing by zero.
fn axis_t(c: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> f32 {
    let a = lightness(lo);
    let d = lightness(hi) - a;
    let step = select(d, select(-0.001, 0.001, d >= 0.0), abs(d) < 0.001);
    return (lightness(c) - a) / step;
}

// Bloom bright-pass of one scene color (premultiplied as stored): the light
// ABOVE the display background — the theme background, or with a phosphor the
// unlit paper (after the same remap the composite applies) — thresholded on its
// brightest channel. A light theme's background is not "above" itself, so it
// never blooms into a wash-out; on a dark theme this is the classic threshold.
// A wider bloom (radius > 0, x.paper.w) also lowers the threshold, so a soft
// neon halo catches bright text, not only solid bright areas.
fn bright(c: vec4<f32>) -> vec3<f32> {
    let a = alpha_div(c.a);
    var s = c.rgb / a;
    var base = x.bg.rgb;
    if (FEAT_PHOS) {
        s = phosphor_map(s);
        base = x.paper.rgb;
    }
    let ex = max(s - base, vec3(0.0, 0.0, 0.0));
    let l = max(ex.r, max(ex.g, ex.b));
    let r = clamp(x.paper.w, 0.0, 1.0);
    return ex * (smoothstep(0.55 - 0.33 * r, 0.9 - 0.32 * r, l) * a);
}

// The scene (straight color) as a monochrome display: the theme background maps
// to the unlit paper, the theme foreground to the lit phosphor (brighter text a
// little beyond), interpolated in perceptual space; `hue` keeps that much of the
// source's own chroma so syntax colors stay distinguishable.
fn phosphor_map(c: vec3<f32>) -> vec3<f32> {
    let t = clamp(axis_t(c, x.bg.rgb, x.fg.rgb), 0.0, 1.3);
    var s = mix(sqrt(x.paper.rgb), sqrt(x.phos.rgb), t);
    s = s + (sqrt(max(c, vec3(0.0, 0.0, 0.0))) - vec3(lightness(c))) * x.phos.w;
    s = clamp(s, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0));
    return s * s;
}
"#;

/// The main (composite) CRT pass. `override` feature switches default to `true`
/// so the raw module is the all-features shader (what naga validates).
const MAIN_WGSL: &str = r#"
struct P {
    resolution: vec2<f32>,
    curvature: f32,
    scanline: f32,
    mask: f32,
    bloom: f32,
    chromatic: f32,
    vignette: f32,
    tint: vec4<f32>,
    corner_radius: f32,
    time: f32,
    flags: u32,
    corner_radius_top: f32,
};
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;
@group(0) @binding(4) var bloom_tex: texture_2d<f32>;

override FEAT_CURVE: bool = true;
override FEAT_CHROMA: bool = true;
override FEAT_SCAN: bool = true;
override FEAT_MASK: bool = true;
override FEAT_BLOOM: bool = true;
override FEAT_VIGN: bool = true;
override FEAT_ANIM: bool = true;
override FEAT_GRAIN: bool = true;
override FEAT_DITHER: bool = true;
override FEAT_GLITCH: bool = true;

const PI: f32 = 3.14159265359;

// Animation tunables. Each animated term is gated by a flag bit and is a no-op
// when that bit is clear, so these only matter while the matching toggle is on.
// Kept tasteful and subtle (sub-strobe, sub-pixel).
const ROLL_SPEED: f32 = 6.0;     // scanline phase advance (rad/s): gentle crawl
const FLICKER_FREQ: f32 = 50.0;  // brightness wobble angular freq (rad/s, ~8 Hz)
const FLICKER_AMP: f32 = 0.04;   // brightness dip amplitude (4%), sub-strobe
const JITTER_AMP: f32 = 0.5;     // horizontal sync jitter peak (sub-pixel px)

// Bloom gains, calibrated against the pre-v2 13-tap kernel (jetty-shot pixel
// diffs: 52.7 dB on the owner's look, ~39 dB at the default bloom .4): the
// pixel's own bright-pass (that kernel's on-stroke taps) + the quarter-res halo.
const BLOOM_LOCAL: f32 = 0.4;
const BLOOM_SPREAD: f32 = 0.7;
// Grain amplitude in perceptual (sqrt) units at crt_grain = 1.
const GRAIN_AMP: f32 = 0.22;
// Event glitch: color split (px at full intensity) and the tear's max shift
// (fraction of the width).
const GLITCH_SPLIT_PX: f32 = 7.0;
const GLITCH_TEAR: f32 = 0.05;

// Integer hash of a cell coordinate + seed → [0, 1). Stable, texture-free.
fn hash(q: vec2<u32>, seed: u32) -> f32 {
    var h = q.x * 1664525u + q.y * 1013904223u + seed * 2654435761u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 3266489917u;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// 8×8 Bayer threshold in (0, 1): the recursive 2×2 [[0,2],[3,1]] pattern.
fn bayer8(q: vec2<u32>) -> f32 {
    var bx = q.x & 7u;
    var by = q.y & 7u;
    var r = 0u;
    for (var i = 0u; i < 3u; i = i + 1u) {
        let lx = bx & 1u;
        let ly = by & 1u;
        r = (r << 2u) | (((lx ^ ly) << 1u) | ly);
        bx = bx >> 1u;
        by = by >> 1u;
    }
    return (f32(r) + 0.5) / 64.0;
}

// Rounded-rect SDF (mirrors mask.rs / phosphor.rs). Negative inside, 0 on the
// edge, positive outside. `b` is the half-size, `r` the corner radius.
fn sd_round_rect(pt: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(pt) - b + vec2(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0, 0.0))) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let res = p.resolution;
    let pix = vec2<u32>(in.pos.xy);           // this output pixel
    let roll_on = (p.flags & 1u) != 0u;     // bit0: rolling scanline
    let flicker_on = (p.flags & 2u) != 0u;  // bit1: brightness flicker
    let jitter_on = (p.flags & 4u) != 0u;   // bit2: horizontal jitter

    // --- 1) Sample position. `exact` while it is this pixel's own center: the
    // scene is then read with one textureLoad (no filtering). ---
    var suv = in.uv;
    var exact = true;
    if (FEAT_CURVE) {
        // Barrel warp of the SAMPLE uv (output uv stays put): push coords
        // outward by the square of the orthogonal axis. Near the edges the
        // warped uv leaves [0,1] -> bezel below.
        let cc = in.uv * 2.0 - 1.0;
        let warp = p.curvature * 0.25;
        suv = (cc + cc * (cc.yx * cc.yx) * warp) * 0.5 + 0.5;
        exact = false;
    }
    if (FEAT_ANIM && jitter_on) {
        // Sub-pixel horizontal h-sync wobble: two incommensurate sines, bounded
        // to +/-JITTER_AMP px.
        suv.x = suv.x + sin(p.time * 80.0) * sin(p.time * 13.0) * JITTER_AMP / res.x;
        exact = false;
    }
    var g = 0.0;
    if (FEAT_GLITCH) {
        g = x.glitch.x;
    }
    if (g > 0.0) {
        // Tear: random horizontal bands shift sideways (wrapping, like a lost
        // h-sync); the band height and pattern change with the seed.
        let seed = u32(x.glitch.y);
        let band_px = 3.0 + 26.0 * hash(vec2(7u, 3u), seed);
        let band = u32(in.pos.y / band_px);
        if (hash(vec2(band, 11u), seed) > 0.62) {
            let off = (hash(vec2(band, 29u), seed) - 0.5) * 2.0 * GLITCH_TEAR * g;
            suv.x = fract(suv.x + off);
        }
        exact = false;
    }
    // Bezel: feather to transparent JUST OUTSIDE the unit box. Only a moved
    // sample position can leave it (else the coverage is exactly 1).
    var bezel = 1.0;
    if (FEAT_CURVE || FEAT_ANIM || FEAT_GLITCH) {
        let outside = max(max(-suv.x, suv.x - 1.0), max(-suv.y, suv.y - 1.0));
        let fpx = 1.5 / max(res.x, res.y);
        bezel = 1.0 - smoothstep(0.0, fpx, outside);
    }

    // --- 2) Scene color + chromatic aberration (R/B diverge along the radius,
    // growing with the distance from the center) + the glitch's color split. ---
    var cg: vec4<f32>;
    if (exact) {
        cg = textureLoad(src_tex, vec2<i32>(pix), 0);
    } else {
        cg = textureSampleLevel(src_tex, src_samp, suv, 0.0);
    }
    var col = cg.rgb;
    if (FEAT_CHROMA) {
        let dir = suv - vec2(0.5, 0.5);
        let ca = p.chromatic * 0.006;
        col.r = textureSampleLevel(src_tex, src_samp, suv + dir * ca, 0.0).r;
        col.b = textureSampleLevel(src_tex, src_samp, suv - dir * ca, 0.0).b;
    }
    if (g > 0.0) {
        let dx = vec2(g * GLITCH_SPLIT_PX / res.x, 0.0);
        col.r = textureSampleLevel(src_tex, src_samp, suv + dx, 0.0).r;
        col.b = textureSampleLevel(src_tex, src_samp, suv - dx, 0.0).b;
    }
    let scene_a = cg.a;                      // carry the window's alpha through
    let a = alpha_div(scene_a);

    // --- 2b) Phosphor / paper: the scene as a monochrome display (on the
    // straight color; re-premultiplied). ---
    if (FEAT_PHOS) {
        col = phosphor_map(col / a) * a;
    }

    // --- 3) Scanlines (output space), tinted by p.tint.rgb. The static beam is
    // 1 on even rows, 0 on odd ones (sin((y + 0.5)·π)); roll advances its phase. ---
    if (FEAT_SCAN) {
        var beam = select(0.0, 1.0, (pix.y & 1u) == 0u);
        if (FEAT_ANIM && roll_on) {
            beam = 0.5 + 0.5 * sin(in.uv.y * res.y * PI + p.time * ROLL_SPEED);
        }
        let darken = p.scanline * beam;
        col = col * ((1.0 - darken) * mix(vec3(1.0, 1.0, 1.0), p.tint.rgb, darken));
    }

    // --- 4) Shadow-mask / aperture grille: vertical RGB stripes per column. ---
    if (FEAT_MASK) {
        let idx = pix.x % 3u;
        let triad = vec3(
            select(0.0, 1.0, idx == 0u),
            select(0.0, 1.0, idx == 1u),
            select(0.0, 1.0, idx == 2u),
        );
        let depth = p.mask * 0.6;           // off-channels dim to (1 - depth)
        col = col * (vec3(1.0, 1.0, 1.0) - depth * (vec3(1.0, 1.0, 1.0) - triad));
    }

    // --- 5) Bloom: the pixel's own bright-pass (local glow, free: the scene is
    // already sampled) + the quarter-res blurred bright-pass (one bilinear tap). ---
    if (FEAT_BLOOM) {
        let halo = textureSampleLevel(bloom_tex, src_samp, suv, 0.0).rgb;
        col = col + (bright(cg) * BLOOM_LOCAL + halo * BLOOM_SPREAD) * p.bloom;
    }

    // --- 5b) Film grain: monochrome noise per (DPI-scaled) cell, added in
    // perceptual space so dark areas are not over-amplified. Static unless the
    // seed advances (crt_grain_animate). ---
    if (FEAT_GRAIN) {
        let q = vec2<u32>(in.pos.xy / max(x.fx.y, 1.0));
        let n = hash(q, u32(x.fx.z)) - 0.5;
        var s = sqrt(max(col / a, vec3(0.0, 0.0, 0.0)));
        s = max(s + vec3(n * x.fx.x * GRAIN_AMP), vec3(0.0, 0.0, 0.0));
        col = s * s * a;
    }

    // --- 6) Vignette: radial edge darkening (output space). ---
    if (FEAT_VIGN) {
        let vd = length(in.uv - vec2(0.5, 0.5)) * 1.41421356;   // 0 center -> ~1 corner
        let v = 1.0 - 0.85 * smoothstep(0.5, 1.15, vd);
        col = col * mix(1.0, v, p.vignette);
    }

    // --- 6b) Flicker: a subtle global brightness wobble (analog mains flutter),
    // low amplitude so it never strobes. Color only, not the coverage. ---
    if (FEAT_ANIM && flicker_on) {
        let flick = 0.5 + 0.5 * sin(p.time * FLICKER_FREQ);   // [0,1]
        col = col * (1.0 - FLICKER_AMP * flick);
    }

    // --- 6c) 1-bit ordered dither (e-ink): each DPI-scaled cell becomes the
    // paper or the ink, by its perceptual position between them vs an 8×8 Bayer
    // threshold. Paper/ink = the phosphor pair, else the theme bg/fg. ---
    if (FEAT_DITHER) {
        var lo = x.bg.rgb;
        var hi = x.fg.rgb;
        if (FEAT_PHOS) {
            lo = x.paper.rgb;
            hi = x.phos.rgb;
        }
        let t = axis_t(col / a, lo, hi);
        let q = vec2<u32>(in.pos.xy / max(x.fx.y, 1.0));
        col = select(lo, hi, t > bayer8(q)) * a;
    }

    // --- 7) Rounded-corner alpha on the UN-WARPED output coords (so the rounding
    // is NOT distorted by curvature) — replicates mask.rs's SDF, feather and
    // radius clamp exactly so the corners match the non-CRT look. The TOP
    // corners use their own radius (0 for a top-flush Dropdown, mirroring the
    // corner mask's per-corner radii) so CRT-on never opens a transparent
    // notch at the monitor's top edge. ---
    let frag = in.uv * res;                 // un-warped output pixel position
    let half = res * 0.5;
    let pt = frag - half;
    let max_r = min(res.x, res.y) * 0.5;
    let rr_bot = clamp(p.corner_radius, 0.0, max_r);
    let rr_top = clamp(p.corner_radius_top, 0.0, max_r);
    let rr = select(rr_top, rr_bot, pt.y > 0.0);
    let d = sd_round_rect(pt, half, rr);
    let cov_raw = 1.0 - smoothstep(-0.75, 0.75, d);
    // both radii <= 0 => fully opaque everywhere (square window, matches mask.rs skip).
    let cov = select(1.0, cov_raw, max(p.corner_radius, p.corner_radius_top) > 0.0);

    // Multiply BOTH (premultiplied) color and alpha by the combined coverage so
    // corners + bezel fade out without re-opaquing (mirrors mask.rs dst-multiply).
    let amask = cov * bezel;
    return vec4(col * amask, scene_a * amask);
}
"#;

/// The quarter-resolution bloom chain: `fs_down` (bright-pass + 4×4 box
/// downsample of the full-res scene) and `fs_blur` (one axis of a 9-tap
/// Gaussian, 5 bilinear taps; `BLUR_VERTICAL` picks the axis).
const BLOOM_WGSL: &str = r#"
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

override BLUR_VERTICAL: bool = false;

@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    // The 4×4 full-res block under this quarter-res texel, read as its four 2×2
    // quads (one bilinear tap each, at the quad's shared corner) and thresholded
    // per quad: like the pre-v2 13-tap kernel, which thresholded bilinear taps,
    // a thin bright stroke brightens itself (the composite's local term) but
    // only a solid bright area spreads a halo.
    let ts = 1.0 / vec2<f32>(textureDimensions(src_tex));
    let c = in.uv;
    var acc = bright(textureSampleLevel(src_tex, src_samp, c + vec2(-ts.x, -ts.y), 0.0));
    acc = acc + bright(textureSampleLevel(src_tex, src_samp, c + vec2(ts.x, -ts.y), 0.0));
    acc = acc + bright(textureSampleLevel(src_tex, src_samp, c + vec2(-ts.x, ts.y), 0.0));
    acc = acc + bright(textureSampleLevel(src_tex, src_samp, c + vec2(ts.x, ts.y), 0.0));
    return vec4(acc * 0.25, 1.0);
}

@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    let ts = 1.0 / vec2<f32>(textureDimensions(src_tex));
    var d = vec2(ts.x, 0.0);
    if (BLUR_VERTICAL) {
        d = vec2(0.0, ts.y);
    }
    d = d * x.fg.w;
    let uv = in.uv;
    var c = textureSampleLevel(src_tex, src_samp, uv, 0.0).rgb * 0.2270270270;
    c = c + (textureSampleLevel(src_tex, src_samp, uv + d * 1.3846153846, 0.0).rgb
           + textureSampleLevel(src_tex, src_samp, uv - d * 1.3846153846, 0.0).rgb) * 0.3162162162;
    c = c + (textureSampleLevel(src_tex, src_samp, uv + d * 3.2307692308, 0.0).rgb
           + textureSampleLevel(src_tex, src_samp, uv - d * 3.2307692308, 0.0).rgb) * 0.0702702703;
    return vec4(c, 1.0);
}
"#;

/// WGSL source of the main CRT module (shared pieces + the composite pass).
pub fn crt_shader_source() -> String {
    [COMMON_WGSL, MAIN_WGSL].concat()
}

/// WGSL source of the bloom module (shared pieces + down/blur passes).
pub fn crt_bloom_shader_source() -> String {
    [COMMON_WGSL, BLOOM_WGSL].concat()
}

/// Per-frame uniform for the CRT pass (binding 0).
///
/// Layout: 64 bytes, FROZEN. No `vec3<f32>` so the Rust `#[repr(C)]` layout
/// matches the WGSL `struct P` byte-for-byte (see the offset table above and the
/// `crt_uniform_layout` test). Every effect strength is a normalized 0..1 slider
/// value; each is a no-op at 0. `time` (animation phase, seconds) and `flags`
/// (CRT_FLAG_* bitfield) drive the roll/flicker/jitter animation; with
/// `flags == 0` the shader output is identical to the static look. Built by
/// [`CrtParams::build`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CrtUniform {
    /// Physical width and height in pixels. (offset 0)
    pub resolution: [f32; 2],
    /// Barrel/curvature warp strength. (offset 8)
    pub curvature: f32,
    /// Scanline darkening strength. (offset 12)
    pub scanline: f32,
    /// Shadow-mask / aperture-grille strength. (offset 16)
    pub mask: f32,
    /// Bloom/glow strength. (offset 20)
    pub bloom: f32,
    /// Chromatic-aberration strength. (offset 24)
    pub chromatic: f32,
    /// Vignette strength. (offset 28)
    pub vignette: f32,
    /// Scanline tint: rgb in `[0..2]`, `[3]` is padding. (offset 32, align 16)
    pub tint: [f32; 4],
    /// Rounded-corner radius of the BOTTOM corners in physical px (matches the
    /// corner mask). (offset 48)
    pub corner_radius: f32,
    /// Animation phase in seconds, wrapped into [0, 2π) (every shader rate is an
    /// integer rad/s, so the wrap preserves each `sin` phase). (offset 52)
    pub time: f32,
    /// Roll/flicker/jitter bitfield (`CRT_FLAG_*`). 0 => static look. (offset 56)
    pub flags: u32,
    /// Rounded-corner radius of the TOP corners in physical px. 0 when the
    /// window is top-flush (Dropdown mode keeps the top corners square, exactly
    /// like the non-CRT corner mask's per-corner radii). (offset 60)
    pub corner_radius_top: f32,
}

/// Extension uniform for the CRT pass (binding 3): 96 bytes, six vec4s (see the
/// table in the WGSL `struct X`). Colors are in the SHADER's color space —
/// linear when the scene texture is sRGB (it decodes on sampling), else the raw
/// encoded values. Built by [`CrtParams::build`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CrtExtUniform {
    /// rgb = theme background; `[3]` = 1.0 when the scene is premultiplied. (offset 0)
    pub bg: [f32; 4],
    /// rgb = theme foreground; `[3]` = bloom blur step in quarter-res texels. (offset 16)
    pub fg: [f32; 4],
    /// rgb = phosphor "unlit" color (paper); `[3]` = bloom radius 0..1. (offset 32)
    pub paper: [f32; 4],
    /// rgb = phosphor "lit" color (ink); `[3]` = hue keep 0..1. (offset 48)
    pub phos: [f32; 4],
    /// x = grain 0..1, y = dither/grain cell px, z = grain seed, w = DPI scale. (offset 64)
    pub fx: [f32; 4],
    /// x = glitch intensity 0..1, y = tear seed, zw reserved. (offset 80)
    pub glitch: [f32; 4],
}

/// The set of CRT features compiled into one pipeline variant — one bit per WGSL
/// `override FEAT_*` switch. Derived from the SETTINGS ([`CrtSettings::key`]),
/// never from per-frame state, so a variant is built only when the settings
/// change ([`Crt::prepare`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CrtKey(u16);

impl CrtKey {
    pub const NONE: CrtKey = CrtKey(0);
    /// Barrel warp + bezel (`crt_curvature > 0`).
    pub const CURVE: CrtKey = CrtKey(1 << 0);
    /// Chromatic aberration (`crt_chromatic > 0`): two extra taps.
    pub const CHROMA: CrtKey = CrtKey(1 << 1);
    /// Scanlines (`crt_scanline > 0`).
    pub const SCAN: CrtKey = CrtKey(1 << 2);
    /// Shadow mask (`crt_mask > 0`).
    pub const MASK: CrtKey = CrtKey(1 << 3);
    /// Quarter-res bloom (`crt_bloom > 0`): the down/blur passes + one tap.
    pub const BLOOM: CrtKey = CrtKey(1 << 4);
    /// Vignette (`crt_vignette > 0`).
    pub const VIGN: CrtKey = CrtKey(1 << 5);
    /// Roll / flicker / jitter (any toggle on).
    pub const ANIM: CrtKey = CrtKey(1 << 6);
    /// Phosphor / paper monochrome remap (`crt_phosphor != "off"`).
    pub const PHOS: CrtKey = CrtKey(1 << 7);
    /// Film grain (`crt_grain > 0`).
    pub const GRAIN: CrtKey = CrtKey(1 << 8);
    /// 1-bit ordered dither (`crt_dither`).
    pub const DITHER: CrtKey = CrtKey(1 << 9);
    /// Event glitch support (`glitch_on_error` / `glitch_on_bell`).
    pub const GLITCH: CrtKey = CrtKey(1 << 10);
    /// Every feature (the raw module's defaults).
    pub const ALL: CrtKey = CrtKey((1 << 11) - 1);

    /// Each feature bit with the WGSL override constant it drives — the one table
    /// the pipeline builder and the naga tests both read.
    pub const FEATURES: [(CrtKey, &'static str); 11] = [
        (CrtKey::CURVE, "FEAT_CURVE"),
        (CrtKey::CHROMA, "FEAT_CHROMA"),
        (CrtKey::SCAN, "FEAT_SCAN"),
        (CrtKey::MASK, "FEAT_MASK"),
        (CrtKey::BLOOM, "FEAT_BLOOM"),
        (CrtKey::VIGN, "FEAT_VIGN"),
        (CrtKey::ANIM, "FEAT_ANIM"),
        (CrtKey::PHOS, "FEAT_PHOS"),
        (CrtKey::GRAIN, "FEAT_GRAIN"),
        (CrtKey::DITHER, "FEAT_DITHER"),
        (CrtKey::GLITCH, "FEAT_GLITCH"),
    ];

    pub const fn bits(self) -> u16 {
        self.0
    }

    /// The key with exactly these bits (unknown bits dropped).
    pub const fn from_bits(bits: u16) -> CrtKey {
        CrtKey(bits & CrtKey::ALL.0)
    }

    pub const fn contains(self, other: CrtKey) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with `feature` set (`on`) or cleared.
    pub const fn with(self, feature: CrtKey, on: bool) -> CrtKey {
        if on { CrtKey(self.0 | feature.0) } else { CrtKey(self.0 & !feature.0) }
    }

    /// The pipeline-overridable constants of the MAIN module for this variant
    /// (every `FEAT_*`, 1.0 = compiled in, 0.0 = compiled out).
    pub fn constants(self) -> Vec<(&'static str, f64)> {
        CrtKey::FEATURES
            .iter()
            .map(|&(bit, name)| (name, if self.contains(bit) { 1.0 } else { 0.0 }))
            .collect()
    }
}

impl std::ops::BitOr for CrtKey {
    type Output = CrtKey;
    fn bitor(self, rhs: CrtKey) -> CrtKey {
        CrtKey(self.0 | rhs.0)
    }
}

/// A monochrome display's two colors (sRGB, 0..1): `ink` is the lit phosphor (or
/// the ink on paper), `paper` the unlit screen (or the paper). `hue` 0..1 keeps
/// that much of the source's own chroma.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Phosphor {
    pub ink: [f32; 3],
    pub paper: [f32; 3],
    pub hue: f32,
}

/// The CRT pass's SETTINGS-level inputs (what `[effects]` says) — they change
/// only on a settings or theme change. The pipeline variant ([`Self::key`]) is
/// derived from these alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrtSettings {
    pub curvature: f32,
    pub scanline: f32,
    pub mask: f32,
    pub bloom: f32,
    /// 0..1 → the blur step between [`BLOOM_STEP_MIN`] and [`BLOOM_STEP_MAX`].
    pub bloom_radius: f32,
    pub chromatic: f32,
    pub vignette: f32,
    /// Scanline tint (rgb 0..1; white = neutral).
    pub tint: [f32; 3],
    pub roll: bool,
    pub flicker: bool,
    pub jitter: bool,
    /// `None` = full color.
    pub phosphor: Option<Phosphor>,
    pub grain: f32,
    /// Advance the grain seed on the [`ANIM_SEED_FPS`] clock (an animation).
    pub grain_animate: bool,
    pub dither: bool,
    /// Compile the event-glitch support (a burst is a per-frame intensity).
    pub glitch: bool,
}

impl CrtSettings {
    /// Every effect off: a passthrough blit that only rounds the corners.
    pub const PASSTHROUGH: CrtSettings = CrtSettings {
        curvature: 0.0,
        scanline: 0.0,
        mask: 0.0,
        bloom: 0.0,
        bloom_radius: 0.0,
        chromatic: 0.0,
        vignette: 0.0,
        tint: [1.0, 1.0, 1.0],
        roll: false,
        flicker: false,
        jitter: false,
        phosphor: None,
        grain: 0.0,
        grain_animate: false,
        dither: false,
        glitch: false,
    };

    /// What runs while CRT is OFF but an event glitch plays: a passthrough with
    /// the glitch compiled in.
    pub const GLITCH_ONLY: CrtSettings = CrtSettings { glitch: true, ..CrtSettings::PASSTHROUGH };

    /// The pipeline variant these settings need: a feature is compiled in only
    /// when its parameter can change a pixel.
    pub fn key(&self) -> CrtKey {
        CrtKey::NONE
            .with(CrtKey::CURVE, self.curvature > 0.0)
            .with(CrtKey::CHROMA, self.chromatic > 0.0)
            .with(CrtKey::SCAN, self.scanline > 0.0)
            .with(CrtKey::MASK, self.mask > 0.0)
            .with(CrtKey::BLOOM, self.bloom > 0.0)
            .with(CrtKey::VIGN, self.vignette > 0.0)
            .with(CrtKey::ANIM, self.roll || self.flicker || self.jitter)
            .with(CrtKey::PHOS, self.phosphor.is_some())
            .with(CrtKey::GRAIN, self.grain > 0.0)
            .with(CrtKey::DITHER, self.dither)
            .with(CrtKey::GLITCH, self.glitch)
    }

    /// True when these settings change the picture over time on their own (an
    /// animation the caller must pace): roll / flicker / jitter, animated grain.
    pub fn animated(&self) -> bool {
        self.roll || self.flicker || self.jitter || (self.grain > 0.0 && self.grain_animate)
    }
}

/// The CRT pass's PER-FRAME inputs: the target, the window shape, the clock, the
/// theme colors, the surface's alpha convention and a glitch burst's intensity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrtFrame {
    /// Physical pass size (the offscreen scene == the output).
    pub width: u32,
    pub height: u32,
    /// Bottom / top corner radii in physical px (top 0 for a top-flush dropdown).
    pub corner_radius: f32,
    pub corner_radius_top: f32,
    /// Free-running animation clock, seconds.
    pub time: f64,
    /// Theme background / foreground (sRGB).
    pub bg: [u8; 3],
    pub fg: [u8; 3],
    /// The scene is premultiplied by alpha (`GpuContext::premultiply_clear`).
    pub premultiplied: bool,
    /// The scene texture is an sRGB format (it decodes to linear on sampling).
    pub srgb: bool,
    /// Display scale factor (dither/grain cells are DPI-scaled).
    pub dpi_scale: f32,
    /// Event-glitch burst intensity 0..1 (0 = none).
    pub glitch: f32,
}

/// Everything one CRT dispatch needs: both uniforms and the pipeline variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrtParams {
    pub base: CrtUniform,
    pub ext: CrtExtUniform,
    pub key: CrtKey,
}

/// sRGB transfer function → linear (exact piecewise curve).
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The bloom blur for `crt_bloom_radius` 0..1: `(step in quarter-res texels,
/// H+V iterations)`. One iteration with a √2 larger step has the σ of two, so
/// the glow widens continuously while a small radius costs two passes, not
/// four. Radius 0 → `(0, 0)`: no blur pass at all.
pub fn bloom_blur(radius: f32) -> (f32, u32) {
    let r = if radius.is_finite() { radius.clamp(0.0, 1.0) } else { 0.0 };
    if r <= 0.0 {
        (0.0, 0)
    } else if r <= 0.5 {
        (BLOOM_STEP_MAX * r * std::f32::consts::SQRT_2, 1)
    } else {
        (BLOOM_STEP_MAX * r, 2)
    }
}

/// The animated-seed counter at `time` (seconds): advances [`ANIM_SEED_FPS`]
/// times per second, kept small enough to be exact in an f32.
pub fn anim_seed(time: f64) -> f32 {
    let t = if time.is_finite() { time.max(0.0) } else { 0.0 };
    ((t * ANIM_SEED_FPS).floor() % 1_000_000.0) as f32
}

impl CrtParams {
    /// THE uniform builder: settings + frame → both uniforms + the variant key.
    /// Shared by the main window, detached windows and jetty-shot.
    pub fn build(s: &CrtSettings, f: &CrtFrame) -> CrtParams {
        let flags = (if s.roll { CRT_FLAG_ROLL } else { 0 })
            | (if s.flicker { CRT_FLAG_FLICKER } else { 0 })
            | (if s.jitter { CRT_FLAG_JITTER } else { 0 });
        let time = if f.time.is_finite() { f.time.rem_euclid(std::f64::consts::TAU) } else { 0.0 };
        let base = CrtUniform {
            resolution: [f.width as f32, f.height as f32],
            curvature: s.curvature,
            scanline: s.scanline,
            mask: s.mask,
            bloom: s.bloom,
            chromatic: s.chromatic,
            vignette: s.vignette,
            tint: [s.tint[0], s.tint[1], s.tint[2], 0.0],
            corner_radius: f.corner_radius,
            time: time as f32,
            flags,
            corner_radius_top: f.corner_radius_top,
        };
        let space = |c: [f32; 3]| -> [f32; 3] {
            let c = c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
            if f.srgb { c.map(srgb_to_linear) } else { c }
        };
        let unit = |c: [u8; 3]| c.map(|v| v as f32 / 255.0);
        let bg = space(unit(f.bg));
        let fg = space(unit(f.fg));
        // Without a phosphor the "paper"/"ink" pair is the theme's own bg/fg
        // (what the dither quantizes to).
        let (paper, ink, hue) = match s.phosphor {
            Some(ph) => (space(ph.paper), space(ph.ink), ph.hue),
            None => (bg, fg, 0.0),
        };
        let unit01 = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        let dpi = if f.dpi_scale.is_finite() && f.dpi_scale > 0.0 { f.dpi_scale } else { 1.0 };
        let cell = dpi.round().max(1.0);
        let seed = anim_seed(f.time);
        let ext = CrtExtUniform {
            bg: [bg[0], bg[1], bg[2], if f.premultiplied { 1.0 } else { 0.0 }],
            fg: [fg[0], fg[1], fg[2], bloom_blur(s.bloom_radius).0],
            paper: [paper[0], paper[1], paper[2], unit01(s.bloom_radius)],
            phos: [ink[0], ink[1], ink[2], unit01(hue)],
            fx: [unit01(s.grain), cell, if s.grain_animate { seed } else { 0.0 }, dpi],
            glitch: [if s.glitch { unit01(f.glitch) } else { 0.0 }, seed, 0.0, 0.0],
        };
        CrtParams { base, ext, key: s.key() }
    }
}

/// The quarter-resolution bloom targets (ping-pong) and their blur bind groups.
struct BloomTargets {
    size: (u32, u32),
    /// Down-pass output, blur-V output, and what the composite samples.
    a: wgpu::TextureView,
    /// Blur-H output.
    b: wgpu::TextureView,
    /// Blur H reads `a`; blur V reads `b`.
    bind_h: wgpu::BindGroup,
    bind_v: wgpu::BindGroup,
}

/// Cached bind groups of one (src view, targets generation).
struct Binds {
    src: wgpu::TextureView,
    gen: u64,
    main: wgpu::BindGroup,
    /// The down pass's (reads `src`); `None` while bloom is off.
    down: Option<wgpu::BindGroup>,
}

/// Lazily built GPU state: nothing here exists until a variant is prepared.
#[derive(Default)]
struct CrtGpu {
    main_module: Option<wgpu::ShaderModule>,
    bloom_module: Option<wgpu::ShaderModule>,
    main_layout: Option<wgpu::PipelineLayout>,
    bloom_layout: Option<wgpu::PipelineLayout>,
    /// One pipeline per prepared variant (a handful at most).
    pipelines: Vec<(CrtKey, wgpu::RenderPipeline)>,
    /// Down pass without / with the phosphor remap.
    down: [Option<wgpu::RenderPipeline>; 2],
    /// Blur (horizontal, vertical).
    blur: Option<(wgpu::RenderPipeline, wgpu::RenderPipeline)>,
    targets: Option<BloomTargets>,
    /// Bumped whenever `targets` is (re)built or dropped: invalidates `binds`.
    gen: u64,
    binds: Option<Binds>,
}

pub struct Crt {
    format: wgpu::TextureFormat,
    uniform_buf: wgpu::Buffer,
    ext_buf: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    bloom_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Bound at binding 4 while bloom is off (the composite never reads it then).
    dummy_view: wgpu::TextureView,
    /// `RefCell` because `apply` takes `&self` (the caller may hold `&mut gpu`).
    gpu: RefCell<CrtGpu>,
}

impl Crt {
    /// Cheap: buffers, layouts, a sampler and a 1×1 placeholder. No shader is
    /// compiled until [`Self::prepare`] (or the first [`Self::apply`]).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt-uniform"),
            size: std::mem::size_of::<CrtUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ext_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt-ext-uniform"),
            size: std::mem::size_of::<CrtExtUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_entry = |binding: u32, visibility: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt-bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                sampler_entry,
                uniform_entry(3, wgpu::ShaderStages::FRAGMENT),
                texture_entry(4),
            ],
        });
        let bloom_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt-bloom-bgl"),
            entries: &[texture_entry(1), sampler_entry, uniform_entry(3, wgpu::ShaderStages::FRAGMENT)],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crt-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let dummy_view = Self::target(device, (1, 1), "crt-bloom-placeholder");
        Self {
            format,
            uniform_buf,
            ext_buf,
            bgl,
            bloom_bgl,
            sampler,
            dummy_view,
            gpu: RefCell::new(CrtGpu::default()),
        }
    }

    /// A quarter-res bloom render target (sampled by the next pass).
    fn target(device: &wgpu::Device, (w, h): (u32, u32), label: &str) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: BLOOM_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Build (once) everything variant `key` needs: its composite pipeline and,
    /// with bloom, the down/blur pipelines. Call it when the settings change —
    /// never per frame; an already-built variant costs one short scan.
    pub fn prepare(&self, device: &wgpu::Device, key: CrtKey) {
        let mut g = self.gpu.borrow_mut();
        self.ensure_pipelines(&mut g, device, key);
    }

    /// Whether variant `key` (and its bloom chain) is built.
    pub fn is_prepared(&self, key: CrtKey) -> bool {
        let g = self.gpu.borrow();
        let main = g.pipelines.iter().any(|(k, _)| *k == key);
        let bloom = !key.contains(CrtKey::BLOOM)
            || (g.down[key.contains(CrtKey::PHOS) as usize].is_some() && g.blur.is_some());
        main && bloom
    }

    /// One fullscreen-triangle pipeline of a CRT module: `vs` + fragment
    /// `entry` with these override `constants`, REPLACE blend into `format`.
    fn pipeline(
        device: &wgpu::Device,
        label: &str,
        layout: &wgpu::PipelineLayout,
        module: &wgpu::ShaderModule,
        entry: &str,
        format: wgpu::TextureFormat,
        constants: &[(&str, f64)],
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(entry),
                // Replace blend: each pass owns every output pixel.
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants,
                    ..Default::default()
                },
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    fn ensure_pipelines(&self, g: &mut CrtGpu, device: &wgpu::Device, key: CrtKey) {
        if !g.pipelines.iter().any(|(k, _)| *k == key) {
            let module = g.main_module.get_or_insert_with(|| {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("crt-shader"),
                    source: wgpu::ShaderSource::Wgsl(crt_shader_source().into()),
                })
            });
            let layout = g.main_layout.get_or_insert_with(|| {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("crt-layout"),
                    bind_group_layouts: &[Some(&self.bgl)],
                    ..Default::default()
                })
            });
            let pipe = Self::pipeline(device, "crt-pipeline", layout, module, "fs", self.format, &key.constants());
            g.pipelines.push((key, pipe));
        }
        if !key.contains(CrtKey::BLOOM) {
            return;
        }
        let phos = key.contains(CrtKey::PHOS);
        if g.down[phos as usize].is_none() || g.blur.is_none() {
            let module = g.bloom_module.get_or_insert_with(|| {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("crt-bloom-shader"),
                    source: wgpu::ShaderSource::Wgsl(crt_bloom_shader_source().into()),
                })
            });
            let layout = g.bloom_layout.get_or_insert_with(|| {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("crt-bloom-layout"),
                    bind_group_layouts: &[Some(&self.bloom_bgl)],
                    ..Default::default()
                })
            });
            if g.down[phos as usize].is_none() {
                let c = [("FEAT_PHOS", if phos { 1.0 } else { 0.0 })];
                g.down[phos as usize] =
                    Some(Self::pipeline(device, "crt-bloom-down", layout, module, "fs_down", BLOOM_FORMAT, &c));
            }
            if g.blur.is_none() {
                let h = Self::pipeline(
                    device, "crt-bloom-blur-h", layout, module, "fs_blur", BLOOM_FORMAT,
                    &[("BLUR_VERTICAL", 0.0)],
                );
                let v = Self::pipeline(
                    device, "crt-bloom-blur-v", layout, module, "fs_blur", BLOOM_FORMAT,
                    &[("BLUR_VERTICAL", 1.0)],
                );
                g.blur = Some((h, v));
            }
        }
    }

    /// (Re)allocate the bloom targets for a `width`×`height` pass; drop them
    /// when bloom is off (`size == None`).
    fn ensure_targets(&self, g: &mut CrtGpu, device: &wgpu::Device, size: Option<(u32, u32)>) {
        let Some((w, h)) = size else {
            if g.targets.take().is_some() {
                g.gen += 1;
            }
            return;
        };
        let q = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
        if g.targets.as_ref().is_some_and(|t| t.size == q) {
            return;
        }
        let a = Self::target(device, q, "crt-bloom-a");
        let b = Self::target(device, q, "crt-bloom-b");
        let bind = |src: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.bloom_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: self.ext_buf.as_entire_binding() },
                ],
            })
        };
        let bind_h = bind(&a, "crt-bloom-bg-h");
        let bind_v = bind(&b, "crt-bloom-bg-v");
        g.targets = Some(BloomTargets { size: q, a, b, bind_h, bind_v });
        g.gen += 1;
    }

    /// Run the CRT post-pass: sample `src` (the offscreen rendered scene) and
    /// write the result into `dst` (the surface), per `params` (built by
    /// [`CrtParams::build`]). With bloom, the quarter-res down/blur passes run
    /// first — all in one encoder, one submit.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dst: &wgpu::TextureView,
        src: &wgpu::TextureView,
        params: &CrtParams,
    ) {
        let key = params.key;
        let bloom = key.contains(CrtKey::BLOOM);
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&params.base));
        queue.write_buffer(&self.ext_buf, 0, bytemuck::bytes_of(&params.ext));

        let mut g = self.gpu.borrow_mut();
        // A no-op once prepared (the caller prepares on a settings change).
        self.ensure_pipelines(&mut g, device, key);
        let size = (params.base.resolution[0] as u32, params.base.resolution[1] as u32);
        self.ensure_targets(&mut g, device, bloom.then_some(size));

        // Rebuild the bind groups only when the src view (resize) or the bloom
        // targets change; otherwise reuse them — no allocator round-trip per frame.
        let stale = g.binds.as_ref().is_none_or(|b| &b.src != src || b.gen != g.gen);
        if stale {
            let bloom_view = g.targets.as_ref().map_or(&self.dummy_view, |t| &t.a);
            let main = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("crt-bg"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: self.ext_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(bloom_view) },
                ],
            });
            let down = g.targets.is_some().then(|| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("crt-bloom-bg-down"),
                    layout: &self.bloom_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        wgpu::BindGroupEntry { binding: 3, resource: self.ext_buf.as_entire_binding() },
                    ],
                })
            });
            g.binds = Some(Binds { src: src.clone(), gen: g.gen, main, down });
        }

        let g = &*g;
        let binds = g.binds.as_ref().expect("bind groups built above");
        let Some(main_pipe) = g.pipelines.iter().find(|(k, _)| *k == key).map(|(_, p)| p) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("crt-encoder"),
        });
        let pass = |encoder: &mut wgpu::CommandEncoder,
                    label: &str,
                    view: &wgpu::TextureView,
                    pipe: &wgpu::RenderPipeline,
                    bind: &wgpu::BindGroup| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    // REPLACE + fullscreen triangle overwrites every pixel, so Clear
                    // (not Load) is identical output while sparing tile-based GPUs a
                    // full-surface load.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        };
        if let (true, Some(t), Some(down_bind), Some(down), Some((blur_h, blur_v))) = (
            bloom,
            g.targets.as_ref(),
            binds.down.as_ref(),
            g.down[key.contains(CrtKey::PHOS) as usize].as_ref(),
            g.blur.as_ref(),
        ) {
            pass(&mut encoder, "crt-bloom-down", &t.a, down, down_bind);
            // A wider glow (radius > 0): the separable Gaussian, once or twice.
            for _ in 0..bloom_blur(params.ext.paper[3]).1 {
                pass(&mut encoder, "crt-bloom-blur-h", &t.b, blur_h, &t.bind_h);
                pass(&mut encoder, "crt-bloom-blur-v", &t.a, blur_v, &t.bind_v);
            }
        }
        pass(&mut encoder, "crt-pass", dst, main_pipe, &binds.main);
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(src: &str, what: &str) -> (naga::Module, naga::valid::ModuleInfo) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{what} must parse: {e}"));
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        let info = validator.validate(&module).unwrap_or_else(|e| panic!("{what} must validate: {e:?}"));
        (module, info)
    }

    /// Resolve the module's `override` constants for one variant and validate the
    /// result — what wgpu does when it builds that pipeline.
    fn validate_variant(
        module: &naga::Module,
        info: &naga::valid::ModuleInfo,
        entry: &str,
        constants: &[(&str, f64)],
    ) {
        let map: naga::back::PipelineConstants =
            constants.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let (resolved, _) = naga::back::pipeline_constants::process_overrides(
            module,
            info,
            Some((naga::ShaderStage::Fragment, entry)),
            &map,
        )
        .unwrap_or_else(|e| panic!("{entry} {constants:?}: overrides must resolve: {e:?}"));
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator
            .validate(&resolved)
            .unwrap_or_else(|e| panic!("{entry} {constants:?}: variant must validate: {e:?}"));
    }

    /// The raw main module (every feature on — the override defaults) parses and
    /// passes naga's validator without a GPU. Always-run gate for the source.
    #[test]
    fn crt_shader_compiles() {
        validate(&crt_shader_source(), "CRT main shader");
    }

    #[test]
    fn crt_bloom_shader_compiles() {
        validate(&crt_bloom_shader_source(), "CRT bloom shader");
    }

    /// EVERY pipeline variant (all 2^11 feature combinations) resolves its
    /// overrides and validates, so no slider combination can hit a shader error
    /// at runtime.
    #[test]
    fn every_crt_variant_validates() {
        let (module, info) = validate(&crt_shader_source(), "CRT main shader");
        for bits in 0..=CrtKey::ALL.bits() {
            let key = CrtKey::from_bits(bits);
            validate_variant(&module, &info, "fs", &key.constants());
        }
    }

    /// The bloom chain's variants: the down pass with and without the phosphor
    /// remap, and both blur axes.
    #[test]
    fn every_bloom_variant_validates() {
        let (module, info) = validate(&crt_bloom_shader_source(), "CRT bloom shader");
        validate_variant(&module, &info, "fs_down", &[("FEAT_PHOS", 0.0)]);
        validate_variant(&module, &info, "fs_down", &[("FEAT_PHOS", 1.0)]);
        validate_variant(&module, &info, "fs_blur", &[("BLUR_VERTICAL", 0.0)]);
        validate_variant(&module, &info, "fs_blur", &[("BLUR_VERTICAL", 1.0)]);
    }

    /// Every `FEATURES` name is a real override of the main module (a typo would
    /// make wgpu reject the pipeline), and the table covers every key bit once.
    #[test]
    fn feature_table_matches_the_shader() {
        let (module, _) = validate(&crt_shader_source(), "CRT main shader");
        let names: Vec<String> = module.overrides.iter().filter_map(|(_, o)| o.name.clone()).collect();
        let mut all = CrtKey::NONE;
        for (bit, name) in CrtKey::FEATURES {
            assert!(names.iter().any(|n| n == name), "{name} is not an override of the CRT shader");
            assert_eq!(bit.bits().count_ones(), 1, "{name} must be one bit");
            assert!(!all.contains(bit), "{name} shares a bit");
            all = all | bit;
        }
        assert_eq!(all, CrtKey::ALL);
        assert_eq!(names.len(), CrtKey::FEATURES.len(), "every override has a key bit");
    }

    /// The Rust `CrtUniform` layout must match the WGSL `struct P` byte-for-byte
    /// (see the offset table above). FROZEN at 64 bytes, 16-byte multiple.
    #[test]
    fn crt_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<CrtUniform>(), 64, "CrtUniform must be 64 bytes");
        assert_eq!(offset_of!(CrtUniform, resolution), 0);
        assert_eq!(offset_of!(CrtUniform, curvature), 8);
        assert_eq!(offset_of!(CrtUniform, scanline), 12);
        assert_eq!(offset_of!(CrtUniform, mask), 16);
        assert_eq!(offset_of!(CrtUniform, bloom), 20);
        assert_eq!(offset_of!(CrtUniform, chromatic), 24);
        assert_eq!(offset_of!(CrtUniform, vignette), 28);
        assert_eq!(offset_of!(CrtUniform, tint), 32);
        assert_eq!(offset_of!(CrtUniform, corner_radius), 48);
        assert_eq!(offset_of!(CrtUniform, time), 52);
        assert_eq!(offset_of!(CrtUniform, flags), 56);
        assert_eq!(offset_of!(CrtUniform, corner_radius_top), 60);
        // bytemuck::Pod requires no padding gaps; size is a multiple of align.
        assert_eq!(size_of::<CrtUniform>() % align_of::<CrtUniform>(), 0);
    }

    /// The extension uniform (binding 3) is six vec4s: the Rust offsets must be
    /// the WGSL `struct X` ones (a vec4 is 16-byte aligned in a uniform).
    #[test]
    fn crt_ext_uniform_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<CrtExtUniform>(), 96, "CrtExtUniform must be 96 bytes");
        assert_eq!(offset_of!(CrtExtUniform, bg), 0);
        assert_eq!(offset_of!(CrtExtUniform, fg), 16);
        assert_eq!(offset_of!(CrtExtUniform, paper), 32);
        assert_eq!(offset_of!(CrtExtUniform, phos), 48);
        assert_eq!(offset_of!(CrtExtUniform, fx), 64);
        assert_eq!(offset_of!(CrtExtUniform, glitch), 80);
        assert_eq!(size_of::<CrtExtUniform>() % 16, 0, "a uniform struct is a 16-byte multiple");
    }

    /// The WGSL `struct X` really is 96 bytes with vec4 members at 16-byte steps
    /// (naga's own layout of the shader-side struct).
    #[test]
    fn crt_ext_struct_matches_wgsl_layout() {
        let (module, _) = validate(&crt_shader_source(), "CRT main shader");
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("X"))
            .expect("struct X");
        let naga::TypeInner::Struct { members, span } = &ty.inner else { panic!("X is a struct") };
        assert_eq!(*span, 96);
        let offsets: Vec<u32> = members.iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 16, 32, 48, 64, 80]);
        let (_, p) = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("P")).expect("struct P");
        let naga::TypeInner::Struct { span, .. } = &p.inner else { panic!("P is a struct") };
        assert_eq!(*span, 64, "the frozen CrtUniform");
    }

    fn frame() -> CrtFrame {
        CrtFrame {
            width: 800,
            height: 600,
            corner_radius: 12.0,
            corner_radius_top: 0.0,
            time: 1.25,
            bg: [11, 14, 20],
            fg: [191, 189, 182],
            premultiplied: true,
            srgb: true,
            dpi_scale: 2.0,
            glitch: 0.0,
        }
    }

    /// Each slider compiles its feature in only when it can change a pixel.
    #[test]
    fn key_follows_the_settings() {
        assert_eq!(CrtSettings::PASSTHROUGH.key(), CrtKey::NONE);
        assert_eq!(CrtSettings::GLITCH_ONLY.key(), CrtKey::GLITCH);
        let s = CrtSettings { scanline: 0.1, bloom: 0.2, ..CrtSettings::PASSTHROUGH };
        assert_eq!(s.key(), CrtKey::SCAN | CrtKey::BLOOM);
        let s = CrtSettings { jitter: true, dither: true, ..CrtSettings::PASSTHROUGH };
        assert_eq!(s.key(), CrtKey::ANIM | CrtKey::DITHER);
        let ph = Phosphor { ink: [1.0, 0.69, 0.0], paper: [0.0; 3], hue: 0.0 };
        let s = CrtSettings { phosphor: Some(ph), grain: 0.1, curvature: 0.2, chromatic: 0.1, ..CrtSettings::PASSTHROUGH };
        assert_eq!(s.key(), CrtKey::PHOS | CrtKey::GRAIN | CrtKey::CURVE | CrtKey::CHROMA);
        let s = CrtSettings { mask: 0.3, vignette: 0.4, glitch: true, ..CrtSettings::PASSTHROUGH };
        assert_eq!(s.key(), CrtKey::MASK | CrtKey::VIGN | CrtKey::GLITCH);
        // Static grain is not an animation; animated grain is.
        assert!(!CrtSettings { grain: 0.1, ..CrtSettings::PASSTHROUGH }.animated());
        assert!(CrtSettings { grain: 0.1, grain_animate: true, ..CrtSettings::PASSTHROUGH }.animated());
        assert!(!CrtSettings { grain_animate: true, ..CrtSettings::PASSTHROUGH }.animated());
        assert!(CrtSettings { roll: true, ..CrtSettings::PASSTHROUGH }.animated());
    }

    /// The single uniform builder: sliders, flags, corners and wrapped time go to
    /// the frozen 64-B uniform exactly as the three hand-built copies did.
    #[test]
    fn build_fills_the_base_uniform() {
        let s = CrtSettings {
            curvature: 0.1,
            scanline: 0.2,
            mask: 0.3,
            bloom: 0.4,
            chromatic: 0.5,
            vignette: 0.6,
            tint: [0.9, 0.8, 0.7],
            roll: true,
            jitter: true,
            ..CrtSettings::PASSTHROUGH
        };
        let f = CrtFrame { time: 100.0, ..frame() };
        let p = CrtParams::build(&s, &f);
        assert_eq!(p.base.resolution, [800.0, 600.0]);
        assert_eq!(
            (p.base.curvature, p.base.scanline, p.base.mask, p.base.bloom, p.base.chromatic, p.base.vignette),
            (0.1, 0.2, 0.3, 0.4, 0.5, 0.6)
        );
        assert_eq!(p.base.tint, [0.9, 0.8, 0.7, 0.0]);
        assert_eq!(p.base.flags, CRT_FLAG_ROLL | CRT_FLAG_JITTER);
        assert_eq!((p.base.corner_radius, p.base.corner_radius_top), (12.0, 0.0));
        let wrapped = (100.0f64 % std::f64::consts::TAU) as f32;
        assert_eq!(p.base.time, wrapped, "time wraps into [0, 2π)");
        assert_eq!(p.key, s.key());
    }

    /// Theme colors reach the shader linearized (sRGB scene) or as-is, the
    /// premultiply flag is carried, a missing phosphor pairs the theme's bg/fg,
    /// and the glitch intensity only exists when the glitch is compiled in.
    #[test]
    fn build_fills_the_ext_uniform() {
        let f = frame();
        let p = CrtParams::build(&CrtSettings::PASSTHROUGH, &f);
        assert!((p.ext.bg[0] - srgb_to_linear(11.0 / 255.0)).abs() < 1e-6);
        assert!((p.ext.fg[1] - srgb_to_linear(189.0 / 255.0)).abs() < 1e-6);
        assert_eq!(p.ext.bg[3], 1.0, "premultiplied scene");
        assert_eq!(&p.ext.paper[..3], &p.ext.bg[..3], "no phosphor: paper = bg");
        assert_eq!(&p.ext.phos[..3], &p.ext.fg[..3], "no phosphor: ink = fg");
        assert_eq!(p.ext.fx[1], 2.0, "dither cell follows the DPI scale");
        let raw = CrtParams::build(&CrtSettings::PASSTHROUGH, &CrtFrame { srgb: false, premultiplied: false, ..f });
        assert!((raw.ext.bg[0] - 11.0 / 255.0).abs() < 1e-6, "non-sRGB scene: raw values");
        assert_eq!(raw.ext.bg[3], 0.0);
        // Glitch: only with the feature compiled in.
        let burst = CrtFrame { glitch: 0.7, ..f };
        assert_eq!(CrtParams::build(&CrtSettings::PASSTHROUGH, &burst).ext.glitch[0], 0.0);
        assert_eq!(CrtParams::build(&CrtSettings::GLITCH_ONLY, &burst).ext.glitch[0], 0.7);
        // Phosphor colors + hue.
        let ph = Phosphor { ink: [1.0, 0.5, 0.0], paper: [0.0, 0.0, 0.0], hue: 0.3 };
        let p = CrtParams::build(&CrtSettings { phosphor: Some(ph), ..CrtSettings::PASSTHROUGH }, &f);
        assert_eq!(p.ext.phos[0], 1.0);
        assert!((p.ext.phos[1] - srgb_to_linear(0.5)).abs() < 1e-6);
        assert_eq!(p.ext.phos[3], 0.3);
        assert_eq!(&p.ext.paper[..3], &[0.0, 0.0, 0.0]);
    }

    /// Grain is static (seed 0) unless animated; an animated seed advances on the
    /// 30 fps clock only — repainting faster never changes it faster.
    #[test]
    fn grain_seed_is_static_unless_animated() {
        let s = CrtSettings { grain: 0.2, ..CrtSettings::PASSTHROUGH };
        let a = CrtParams::build(&s, &CrtFrame { time: 10.0, ..frame() });
        let b = CrtParams::build(&s, &CrtFrame { time: 99.0, ..frame() });
        assert_eq!((a.ext.fx[2], b.ext.fx[2]), (0.0, 0.0));
        let s = CrtSettings { grain_animate: true, ..s };
        let t0 = CrtParams::build(&s, &CrtFrame { time: 10.0, ..frame() }).ext.fx[2];
        let t1 = CrtParams::build(&s, &CrtFrame { time: 10.01, ..frame() }).ext.fx[2];
        let t2 = CrtParams::build(&s, &CrtFrame { time: 10.04, ..frame() }).ext.fx[2];
        assert_eq!(t0, t1, "within one 1/30 s step");
        assert_ne!(t0, t2, "the next step");
    }

    /// Radius 0 = no blur pass (the pre-v2 tight glow); one H+V iteration up to
    /// 0.5, two above — and the Gaussian σ (∝ step·√iterations) grows
    /// continuously across the switch.
    #[test]
    fn bloom_blur_maps_the_radius() {
        assert_eq!(bloom_blur(0.0), (0.0, 0));
        assert_eq!(bloom_blur(-3.0), (0.0, 0));
        assert_eq!(bloom_blur(f32::NAN), (0.0, 0));
        assert_eq!(bloom_blur(0.3).1, 1);
        assert_eq!(bloom_blur(0.5).1, 1);
        assert_eq!(bloom_blur(0.6).1, 2);
        assert_eq!(bloom_blur(1.0), (BLOOM_STEP_MAX, 2));
        let sigma = |r: f32| {
            let (step, n) = bloom_blur(r);
            step * (n as f32).sqrt()
        };
        assert!((sigma(0.5) - sigma(0.5001)).abs() < 1e-3, "continuous at the switch");
        let mut last = 0.0;
        for i in 1..=100 {
            let s = sigma(i as f32 / 100.0);
            assert!(s > last, "σ grows with the radius");
            last = s;
        }
        let s = CrtSettings { bloom: 0.4, bloom_radius: 0.3, ..CrtSettings::PASSTHROUGH };
        let p = CrtParams::build(&s, &frame());
        assert_eq!(p.ext.fg[3], bloom_blur(0.3).0);
        assert_eq!(p.ext.paper[3], 0.3, "the radius rides in paper.w");
    }

    /// Smoke-test `Crt` on a real device: prepare a few variants (incl. bloom)
    /// and run them on a small offscreen target. Gated with `#[ignore]` because
    /// a GPU adapter may be unavailable in headless / CI environments. Run with:
    ///   cargo test -p jetty-render crt_apply_with_device -- --ignored
    #[test]
    #[ignore]
    fn crt_apply_with_device() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let tex = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: 64, height: 48, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let (src, dst) = (tex("src"), tex("dst"));
        let (src, dst) = (src.create_view(&Default::default()), dst.create_view(&Default::default()));
        let crt = Crt::new(&device, format);
        let ph = Phosphor { ink: [1.0, 0.69, 0.0], paper: [0.0; 3], hue: 0.1 };
        for s in [
            CrtSettings::PASSTHROUGH,
            CrtSettings { bloom: 0.4, scanline: 0.5, ..CrtSettings::PASSTHROUGH },
            CrtSettings { bloom: 0.4, phosphor: Some(ph), grain: 0.1, dither: true, ..CrtSettings::PASSTHROUGH },
            CrtSettings { curvature: 0.2, glitch: true, ..CrtSettings::PASSTHROUGH },
        ] {
            let p = CrtParams::build(&s, &CrtFrame { width: 64, height: 48, glitch: 0.5, ..frame() });
            crt.prepare(&device, p.key);
            assert!(crt.is_prepared(p.key));
            crt.apply(&device, &queue, &dst, &src, &p);
        }
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    }
}
