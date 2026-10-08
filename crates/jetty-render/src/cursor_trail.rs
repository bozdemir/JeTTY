//! Cursor trail (`[cursor] trail`): when the cursor JUMPS, a short smear
//! stretches from where it was and catches up with it — kitty's
//! `cursor_trail.c` corner model.
//!
//! Each of the trail quad's four corners chases the matching corner of the
//! cursor rect with an exponential ease-out, `c += (target − c)·(1 − 2^(−10·dt/decay))`
//! (so after `decay` seconds a corner has closed all but 1/1024 of its
//! distance). The corners on the LEADING side of the motion use a fast decay
//! and the trailing ones a slow decay, interpolated by how well each corner
//! points along the motion — that is what stretches the quad into a smear
//! instead of sliding a box. The real cursor is drawn at its destination from
//! the first frame; the trail only fills the space behind it (its fragment
//! skips the cursor rect).
//!
//! [`TrailModel`] is the pure state machine (start rules, dwell, the corner
//! physics, the end of the animation) — unit-tested. [`CursorTrailLayer`] is
//! the GPU side: one 6-vertex draw recorded INSIDE the grid render pass,
//! between the cell backgrounds and the glyphs, built lazily the first time a
//! trail is drawn (zero cost while `trail = false`).

use std::time::{Duration, Instant};

/// The fast (leading-corner) decay is this fraction of the slow one — kitty's
/// default 0.1 s / 0.4 s ratio.
pub const TRAIL_FAST_RATIO: f32 = 0.25;
/// The cursor must stay on a new cell this long before the trail chases it
/// (one timed wake, never a poll): a full-screen program that parks the cursor
/// somewhere for a frame while it redraws never trails there.
pub const TRAIL_DWELL: Duration = Duration::from_millis(20);
/// The animation ends once every corner is within this many pixels of its
/// target.
const TRAIL_SETTLE_PX: f32 = 0.5;
/// Opacity of the smear (the cursor color at this alpha).
pub const TRAIL_ALPHA: f32 = 0.7;

/// Where the cursor is, in the units the trail cares about: which surface /
/// tab / grid geometry / scroll position it belongs to (`key` — any change
/// means "no trail", e.g. a tab switch, resize, font change or scrolling), the
/// cell, and the cursor shape's pixel rect `[x, y, w, h]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailPos {
    pub key: [u64; 4],
    pub cell: (usize, usize),
    pub rect: [f32; 4],
}

impl TrailPos {
    /// Manhattan distance in cells (kitty's start threshold metric).
    fn cells_to(&self, other: &TrailPos) -> usize {
        self.cell.0.abs_diff(other.cell.0) + self.cell.1.abs_diff(other.cell.1)
    }
}

/// The trail's tunables (`[cursor] trail_ms` / `trail_threshold`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailParams {
    /// Seconds for the slow (trailing) corners to close all but 1/1024 of the
    /// distance; the leading corners take [`TRAIL_FAST_RATIO`] of it.
    pub decay_slow: f32,
    /// A jump must cover MORE than this many cells (Manhattan: rows + columns)
    /// to trail — kitty's `cursor_trail_start_threshold` rule.
    pub threshold: usize,
}

impl TrailParams {
    pub fn new(trail_ms: u32, threshold: u32) -> Self {
        TrailParams { decay_slow: (trail_ms.max(1) as f32) / 1000.0, threshold: threshold.max(1) as usize }
    }

    fn decay_fast(&self) -> f32 {
        self.decay_slow * TRAIL_FAST_RATIO
    }

    /// Hard wall-clock cap on one animation (the corners are converged well
    /// before; this only guarantees the loop can never stay in Poll).
    pub fn max_secs(&self) -> f32 {
        self.decay_slow * 1.5 + 0.05
    }
}

/// The four corners of a rect `[x, y, w, h]`: top-right, bottom-right,
/// bottom-left, top-left (kitty's order; the quad is drawn as (0,1,2)+(0,2,3)).
pub fn rect_corners(r: [f32; 4]) -> [[f32; 2]; 4] {
    let [x, y, w, h] = r;
    [[x + w, y], [x + w, y + h], [x, y + h], [x, y]]
}

/// One live trail animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailAnim {
    pub corners: [[f32; 2]; 4],
    pub target: [f32; 4],
    pub started: Instant,
    updated: Instant,
}

impl TrailAnim {
    /// Advance the corners by `dt` seconds toward `target` (kitty's update:
    /// per-corner decay picked by how well the corner points along its own
    /// motion, relative to the cursor's half-diagonal).
    fn step(&mut self, dt: f32, p: &TrailParams) {
        let target = rect_corners(self.target);
        let [x, y, w, h] = self.target;
        let center = [x + w * 0.5, y + h * 0.5];
        let half_diag = (w * w + h * h).sqrt().max(1e-3) * 0.5;
        let mut d = [[0.0f32; 2]; 4];
        let mut dot = [0.0f32; 4];
        let mut moving = [false; 4];
        for i in 0..4 {
            d[i] = [target[i][0] - self.corners[i][0], target[i][1] - self.corners[i][1]];
            let len = (d[i][0] * d[i][0] + d[i][1] * d[i][1]).sqrt();
            if len < 1e-6 {
                continue;
            }
            moving[i] = true;
            dot[i] = (d[i][0] * (target[i][0] - center[0]) + d[i][1] * (target[i][1] - center[1])) / half_diag / len;
        }
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in (0..4).filter(|&i| moving[i]) {
            lo = lo.min(dot[i]);
            hi = hi.max(dot[i]);
        }
        let (slow, fast) = (p.decay_slow, p.decay_fast());
        for i in (0..4).filter(|&i| moving[i]) {
            let decay = if hi > lo { slow + (fast - slow) * (dot[i] - lo) / (hi - lo) } else { slow };
            let k = 1.0 - (-10.0 * dt / decay.max(1e-4)).exp2();
            self.corners[i][0] += d[i][0] * k;
            self.corners[i][1] += d[i][1] * k;
        }
    }

    /// Whether any corner is still visibly away from its target.
    fn visible(&self) -> bool {
        let t = rect_corners(self.target);
        (0..4).any(|i| {
            (t[i][0] - self.corners[i][0]).abs() >= TRAIL_SETTLE_PX
                || (t[i][1] - self.corners[i][1]).abs() >= TRAIL_SETTLE_PX
        })
    }
}

/// What a frame should do with the trail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrailFrame {
    /// Nothing to draw, nothing owed.
    Idle,
    /// Nothing to draw yet; the cursor reached a new cell and the trail may
    /// start once it has dwelled there — wake once at this instant.
    WakeAt(Instant),
    /// Draw the trail quad (`corners`) behind the cursor at `skip`; keep
    /// painting frames (the animation is live).
    Draw { corners: [[f32; 2]; 4], skip: [f32; 4] },
}

/// The trail state machine for ONE window.
#[derive(Debug, Clone, Default)]
pub struct TrailModel {
    /// Where the cursor last DWELLED — the start of the next trail.
    settled: Option<TrailPos>,
    /// Where the cursor is now, and since when.
    seen: Option<(TrailPos, Instant)>,
    anim: Option<TrailAnim>,
}

impl TrailModel {
    /// Forget everything (hide, resize, font change, tab switch, reduce
    /// motion, a trail toggle): the next position is simply where the cursor
    /// is, with no trail.
    pub fn reset(&mut self) {
        *self = TrailModel::default();
    }

    /// Whether an animation is live (the event loop keeps painting frames).
    pub fn animating(&self) -> bool {
        self.anim.is_some()
    }

    /// The live animation, if any (jetty-shot draws it).
    pub fn anim(&self) -> Option<&TrailAnim> {
        self.anim.as_ref()
    }

    /// Advance the model for a frame at `now` with the cursor at `pos` (`None`
    /// while it is hidden, off the grid, or the trail must not run: copy-mode,
    /// an IME composition, a flood, reduced motion — each of those also drops
    /// any live trail).
    pub fn frame(&mut self, pos: Option<TrailPos>, now: Instant, p: &TrailParams) -> TrailFrame {
        let Some(pos) = pos else {
            self.reset();
            return TrailFrame::Idle;
        };
        // A different tab / grid / font / scroll position: start over there.
        if self.settled.is_some_and(|s| s.key != pos.key) {
            self.reset();
        }
        match self.seen {
            Some((s, _)) if s.cell == pos.cell && s.key == pos.key => {}
            _ => self.seen = Some((pos, now)),
        }
        let settled = *self.settled.get_or_insert(pos);
        let (seen, since) = self.seen.expect("set above");
        let mut wake = None;
        if seen.cell != settled.cell {
            if self.anim.is_none() && settled.cells_to(&seen) <= p.threshold {
                // A small move (typing, a couple of keys landing in one frame):
                // never a trail, and the start point keeps up at once — so a
                // typing burst can never add up to a "jump" the moment it
                // pauses.
                self.settled = Some(seen);
            } else if now.saturating_duration_since(since) >= TRAIL_DWELL {
                // The cursor dwelled on a far cell: chase it — from wherever a
                // live trail is now, else from where it last dwelled.
                let corners = self.anim.map_or(rect_corners(settled.rect), |a| a.corners);
                let started = self.anim.map_or(now, |a| a.started);
                self.anim = Some(TrailAnim { corners, target: seen.rect, started, updated: now });
                self.settled = Some(seen);
            } else {
                wake = Some(since + TRAIL_DWELL);
            }
        } else if let Some(a) = self.anim.as_mut() {
            // Same cell, but its rect may have changed (a shape change).
            a.target = seen.rect;
        }
        if let Some(a) = self.anim.as_mut() {
            let dt = now.saturating_duration_since(a.updated).as_secs_f32();
            a.updated = now;
            a.step(dt, p);
            let expired = now.saturating_duration_since(a.started).as_secs_f32() >= p.max_secs();
            if !a.visible() || expired {
                self.anim = None;
            }
        }
        match (self.anim, wake) {
            (Some(a), _) => TrailFrame::Draw { corners: a.corners, skip: a.target },
            (None, Some(t)) => TrailFrame::WakeAt(t),
            (None, None) => TrailFrame::Idle,
        }
    }
}

// ── GPU ─────────────────────────────────────────────────────────────────────

/// 64-byte uniform. No vec3 — the Rust layout matches the WGSL struct
/// byte-for-byte (see `trail_uniform_layout`).
///
/// ```text
/// screen   vec4<f32>  @  0  (w, h, _, _)
/// c01      vec4<f32>  @ 16  (corner 0 xy, corner 1 xy)
/// c23      vec4<f32>  @ 32  (corner 2 xy, corner 3 xy)
/// color    vec4<f32>  @ 48  (sRGB rgb 0..1, alpha)
/// skip     vec4<f32>  @ 64  (cursor rect x, y, w, h)
///                     total 80
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TrailUniform {
    pub screen: [f32; 4],
    pub c01: [f32; 4],
    pub c23: [f32; 4],
    pub color: [f32; 4],
    pub skip: [f32; 4],
}

impl TrailUniform {
    /// The uniform for a `w`×`h` target, the trail `corners`, the cursor rect
    /// to leave alone (`skip`) and the trail `rgb` (sRGB 0..255).
    pub fn new(w: u32, h: u32, corners: [[f32; 2]; 4], skip: [f32; 4], rgb: [u8; 3]) -> Self {
        let c = |v: u8| v as f32 / 255.0;
        TrailUniform {
            screen: [w as f32, h as f32, 0.0, 0.0],
            c01: [corners[0][0], corners[0][1], corners[1][0], corners[1][1]],
            c23: [corners[2][0], corners[2][1], corners[3][0], corners[3][1]],
            color: [c(rgb[0]), c(rgb[1]), c(rgb[2]), TRAIL_ALPHA],
            skip,
        }
    }
}

pub(crate) const TRAIL_SHADER: &str = r#"
struct T {
    screen: vec4<f32>,
    c01: vec4<f32>,
    c23: vec4<f32>,
    color: vec4<f32>,
    skip: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: T;

struct VsOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles over the (convex) trail quad: (0,1,2) and (0,2,3).
    var corners = array<vec2<f32>, 4>(u.c01.xy, u.c01.zw, u.c23.xy, u.c23.zw);
    var order = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let px = corners[order[vi]];
    var o: VsOut;
    o.pos = vec4(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    return o;
}

fn s2l(c: f32) -> f32 { if (c <= 0.04045) { return c / 12.92; } return pow((c + 0.055) / 1.055, 2.4); }

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // The real cursor is drawn at its destination: never paint over it.
    let p = in.pos.xy;
    if (p.x >= u.skip.x && p.x < u.skip.x + u.skip.z && p.y >= u.skip.y && p.y < u.skip.y + u.skip.w) {
        discard;
    }
    // Premultiplied (the target holds premultiplied color).
    let a = u.color.a;
    return vec4(s2l(u.color.r) * a, s2l(u.color.g) * a, s2l(u.color.b) * a, a);
}
"#;

/// The trail's GPU pass: a lazily built pipeline + one uniform, drawn inside
/// the caller's grid render pass.
pub struct CursorTrailLayer {
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl CursorTrailLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cursor-trail-shader"),
            source: wgpu::ShaderSource::Wgsl(TRAIL_SHADER.into()),
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cursor-trail-uniform"),
            size: std::mem::size_of::<TrailUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cursor-trail-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cursor-trail-bg"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cursor-trail-layout"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cursor-trail-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        CursorTrailLayer { pipeline, uniform_buf, bind_group }
    }

    /// Upload this frame's trail (before the render pass begins; the write
    /// lands at the pass's submit).
    pub fn upload(&self, queue: &wgpu::Queue, u: &TrailUniform) {
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(u));
    }

    /// Record the trail into the caller's render pass (after the cell
    /// backgrounds, before the glyphs).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Simulate a trail for `elapsed` (jetty-shot's `JETTY_SHOT_TRAIL`): the
/// cursor dwelled at `from`, jumped to `to`, and frames ran every ~16 ms since
/// the trail started. Returns the corners to draw, or `None` when the trail has
/// already caught up (or the jump is under the threshold).
pub fn simulate_trail(from: TrailPos, to: TrailPos, elapsed: Duration, p: &TrailParams) -> Option<[[f32; 2]; 4]> {
    let t0 = Instant::now();
    let mut m = TrailModel::default();
    m.frame(Some(from), t0, p);
    m.frame(Some(to), t0 + Duration::from_millis(1), p);
    let start = t0 + Duration::from_millis(1) + TRAIL_DWELL;
    let mut last = TrailFrame::Idle;
    let frames = (elapsed.as_secs_f32() / 0.016).ceil() as u32;
    for k in 0..=frames {
        let at = start + elapsed.mul_f32(k as f32 / frames.max(1) as f32);
        last = m.frame(Some(to), at, p);
    }
    match last {
        TrailFrame::Draw { corners, .. } => Some(corners),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (f32, f32) = (10.0, 20.0);

    fn pos(row: usize, col: usize) -> TrailPos {
        TrailPos {
            key: [1, 80, 24, 0],
            cell: (row, col),
            rect: [col as f32 * CELL.0, row as f32 * CELL.1, CELL.0, CELL.1],
        }
    }

    fn params() -> TrailParams {
        TrailParams::new(200, 2)
    }

    #[test]
    fn shader_validates_and_uniform_matches() {
        let module = naga::front::wgsl::parse_str(TRAIL_SHADER).expect("parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("validates");
    }

    #[test]
    fn trail_uniform_layout() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<TrailUniform>(), 80);
        assert_eq!(align_of::<TrailUniform>(), 4);
        assert_eq!(offset_of!(TrailUniform, screen), 0);
        assert_eq!(offset_of!(TrailUniform, c01), 16);
        assert_eq!(offset_of!(TrailUniform, c23), 32);
        assert_eq!(offset_of!(TrailUniform, color), 48);
        assert_eq!(offset_of!(TrailUniform, skip), 64);
        assert_eq!(size_of::<TrailUniform>() % 16, 0);
    }

    #[test]
    fn typing_never_trails() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        assert_eq!(m.frame(Some(pos(5, 10)), t0, &p), TrailFrame::Idle);
        // One cell per keystroke: below the threshold — no trail, no wake.
        let mut t = t0;
        for col in 11..20 {
            t += Duration::from_millis(60);
            assert_eq!(m.frame(Some(pos(5, col)), t, &p), TrailFrame::Idle);
        }
    }

    #[test]
    fn a_fast_typing_burst_never_adds_up_to_a_jump() {
        // Two keys per (slow) frame, faster than the dwell: the start point keeps
        // up, so pausing after the burst trails nothing.
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(5, 10)), t0, &p);
        let mut t = t0;
        for step in 1..=10 {
            t += Duration::from_millis(12);
            assert_eq!(m.frame(Some(pos(5, 10 + 2 * step)), t, &p), TrailFrame::Idle);
        }
        assert_eq!(m.frame(Some(pos(5, 30)), t + Duration::from_millis(500), &p), TrailFrame::Idle);
    }

    #[test]
    fn the_threshold_is_strict_like_kittys() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(5, 10)), t0, &p);
        // Exactly the threshold (2 cells): no trail.
        assert_eq!(m.frame(Some(pos(5, 12)), t0 + Duration::from_millis(100), &p), TrailFrame::Idle);
        // One more (3 cells from the new start): a jump — dwell, then trail.
        let t1 = t0 + Duration::from_millis(200);
        assert_eq!(m.frame(Some(pos(5, 15)), t1, &p), TrailFrame::WakeAt(t1 + TRAIL_DWELL));
        assert!(matches!(m.frame(Some(pos(5, 15)), t1 + TRAIL_DWELL, &p), TrailFrame::Draw { .. }));
    }

    #[test]
    fn a_jump_trails_after_the_dwell_with_one_wake() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(5, 2)), t0, &p);
        let t1 = t0 + Duration::from_millis(500);
        // The jump frame: nothing drawn yet, ONE wake owed at the dwell end.
        assert_eq!(m.frame(Some(pos(20, 40)), t1, &p), TrailFrame::WakeAt(t1 + TRAIL_DWELL));
        assert!(!m.animating(), "no Poll during the dwell");
        // The wake frame starts the trail at the OLD rect, chasing the new one.
        match m.frame(Some(pos(20, 40)), t1 + TRAIL_DWELL, &p) {
            TrailFrame::Draw { corners, skip } => {
                assert_eq!(skip, pos(20, 40).rect, "never paint over the real cursor");
                assert_eq!(corners, rect_corners(pos(5, 2).rect), "starts where the cursor dwelled");
            }
            f => panic!("expected a trail, got {f:?}"),
        }
        assert!(m.animating());
    }

    #[test]
    fn leading_corners_outrun_trailing_ones() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(5, 0)), t0, &p);
        m.frame(Some(pos(5, 30)), t0 + Duration::from_millis(1), &p);
        let start = t0 + Duration::from_millis(1) + TRAIL_DWELL;
        m.frame(Some(pos(5, 30)), start, &p);
        let TrailFrame::Draw { corners, .. } = m.frame(Some(pos(5, 30)), start + Duration::from_millis(16), &p)
        else {
            panic!("trail should be live");
        };
        // Moving right: the RIGHT corners (0, 1) lead, the LEFT ones (2, 3) trail.
        let target = rect_corners(pos(5, 30).rect);
        let gap = |i: usize| (target[i][0] - corners[i][0]).abs();
        assert!(gap(0) < gap(3) && gap(1) < gap(2), "leading corners are closer: {corners:?}");
        // …so the smear is longer than the cursor.
        assert!(corners[0][0] - corners[3][0] > CELL.0 * 2.0);
    }

    #[test]
    fn the_trail_converges_and_ends_by_itself() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(0, 0)), t0, &p);
        m.frame(Some(pos(23, 79)), t0 + Duration::from_millis(1), &p);
        let mut t = t0 + Duration::from_millis(1) + TRAIL_DWELL;
        let mut frames = 0;
        while !matches!(m.frame(Some(pos(23, 79)), t, &p), TrailFrame::Idle) {
            t += Duration::from_millis(16);
            frames += 1;
            assert!(frames < 100, "the trail must end");
        }
        // Converged within the configured time (+ the dwell and a frame).
        let lasted = t.duration_since(t0).as_secs_f32();
        assert!(lasted <= 0.2 + 0.02 + 0.05, "lasted {lasted}s");
        assert!(!m.animating());
    }

    #[test]
    fn a_wall_clock_cap_ends_even_a_stalled_trail() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(0, 0)), t0, &p);
        m.frame(Some(pos(23, 79)), t0 + Duration::from_millis(1), &p);
        let start = t0 + Duration::from_millis(1) + TRAIL_DWELL;
        assert!(matches!(m.frame(Some(pos(23, 79)), start, &p), TrailFrame::Draw { .. }));
        // No frames for a long time (window acquire failing…): the next one ends it.
        let late = start + Duration::from_secs_f32(p.max_secs() + 0.01);
        assert_eq!(m.frame(Some(pos(23, 79)), late, &p), TrailFrame::Idle);
    }

    #[test]
    fn transient_positions_inside_the_dwell_never_become_targets() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(10, 10)), t0, &p);
        // A TUI parks the cursor on its status line for 5 ms, then returns.
        let t1 = t0 + Duration::from_millis(100);
        assert!(matches!(m.frame(Some(pos(23, 0)), t1, &p), TrailFrame::WakeAt(_)));
        let back = t1 + Duration::from_millis(5);
        assert_eq!(m.frame(Some(pos(10, 10)), back, &p), TrailFrame::Idle);
        assert_eq!(m.frame(Some(pos(10, 10)), back + TRAIL_DWELL, &p), TrailFrame::Idle);
    }

    #[test]
    fn context_changes_and_blocks_reset_without_a_trail() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(0, 0)), t0, &p);
        // Another tab / grid / scroll position: start over, no trail.
        let mut other = pos(20, 60);
        other.key[0] = 2;
        m.frame(Some(other), t0 + Duration::from_millis(10), &p);
        assert_eq!(m.frame(Some(other), t0 + Duration::from_millis(100), &p), TrailFrame::Idle);
        // A blocked frame (copy-mode, IME, flood, reduce motion) drops a live one.
        m.frame(Some(pos(0, 0)), t0 + Duration::from_millis(200), &p);
        m.frame(Some(pos(20, 60)), t0 + Duration::from_millis(300), &p);
        m.frame(Some(pos(20, 60)), t0 + Duration::from_millis(330), &p);
        assert!(m.animating());
        assert_eq!(m.frame(None, t0 + Duration::from_millis(340), &p), TrailFrame::Idle);
        assert!(!m.animating());
    }

    #[test]
    fn a_new_jump_mid_trail_retargets_from_where_the_smear_is() {
        let (mut m, p, t0) = (TrailModel::default(), params(), Instant::now());
        m.frame(Some(pos(0, 0)), t0, &p);
        m.frame(Some(pos(0, 40)), t0 + Duration::from_millis(1), &p);
        let s = t0 + Duration::from_millis(1) + TRAIL_DWELL;
        m.frame(Some(pos(0, 40)), s, &p);
        let TrailFrame::Draw { corners: mid, .. } = m.frame(Some(pos(0, 40)), s + Duration::from_millis(30), &p)
        else {
            panic!()
        };
        // Jump again (even by one cell — a live trail keeps chasing).
        m.frame(Some(pos(0, 41)), s + Duration::from_millis(31), &p);
        let TrailFrame::Draw { corners, skip } = m.frame(Some(pos(0, 41)), s + Duration::from_millis(31) + TRAIL_DWELL, &p)
        else {
            panic!("the live trail keeps going")
        };
        assert_eq!(skip, pos(0, 41).rect);
        assert!(corners[3][0] >= mid[3][0], "continues from the smear, not from scratch");
    }

    #[test]
    fn simulate_trail_matches_the_model() {
        let p = params();
        let c = simulate_trail(pos(2, 2), pos(15, 60), Duration::from_millis(40), &p).expect("mid-trail");
        let target = rect_corners(pos(15, 60).rect);
        assert!(c[3][0] < target[3][0], "still catching up at 40 ms");
        assert!(simulate_trail(pos(2, 2), pos(15, 60), Duration::from_millis(400), &p).is_none(), "done");
        assert!(simulate_trail(pos(2, 2), pos(2, 3), Duration::from_millis(10), &p).is_none(), "below threshold");
    }
}
