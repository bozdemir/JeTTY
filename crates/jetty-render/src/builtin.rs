//! Built-in glyphs: box drawing (U+2500–257F), block elements (U+2580–259F),
//! Powerline separators (U+E0B0–E0BF, E0D2, E0D4), braille (U+2800–28FF) and
//! sextants (U+1FB00–1FB3B), rasterized to the EXACT cell instead of taken from
//! the font.
//!
//! A font draws these from its own outlines at a fractional x and baseline, so a
//! `─` half-covers two pixel rows, a `` is a fraction of a pixel short of the
//! cell (seams in a p10k prompt), braille falls back to whatever font has it (gaps
//! between btop graph rows) and sextants spill into the next cell. Here every
//! glyph is painted into a `w × h` alpha mask that IS the cell — the same pixel
//! span as the cell's background quad — so lines meet their neighbours with no
//! gap, overlap or blur, at any size.
//!
//! Pure: integer rects for lines, blocks, braille and sextants; `zeno` (already in
//! the tree via swash) for the anti-aliased arcs, diagonals and Powerline shapes.
//! The renderer hands each glyph to glyphon as a `CustomGlyph` whose id is
//! [`glyph_id`]; glyphon caches the rasterized mask per (id, w, h) in its atlas,
//! so a glyph is rasterized once per size, never per frame.

use zeno::{Angle, ArcSize, ArcSweep, Cap, Command, Join, Mask, PathBuilder, Stroke};

/// Bits of a custom glyph id that select the glyph (its "slot"); the bits above
/// carry the light line thickness, so the cache key (id, w, h) pins everything the
/// rasterizer draws.
const SLOT_BITS: u16 = 9;
const SLOT_MASK: u16 = (1 << SLOT_BITS) - 1;
/// Largest encodable light line thickness (px).
const MAX_LIGHT: u16 = 1 << (16 - SLOT_BITS);

// Slot ranges, in code-point order within each block.
const BOX: u16 = 0; // U+2500..=U+257F (128)
const BLOCK: u16 = 128; // U+2580..=U+259F (32)
const PL: u16 = 160; // U+E0B0..=U+E0BF (16)
const PL_D2: u16 = 176; // U+E0D2
const PL_D4: u16 = 177; // U+E0D4
const BRAILLE: u16 = 178; // U+2800..=U+28FF (256)
const SEXTANT: u16 = 434; // U+1FB00..=U+1FB3B (60)
const SLOTS: u16 = 494;

/// The built-in slot drawing `c`, or `None` when the font draws it. U+2800 (the
/// blank braille pattern) has a slot too but draws nothing — see [`is_blank`].
pub fn slot(c: char) -> Option<u16> {
    let cp = c as u32;
    let s = match cp {
        0x2500..=0x257F => BOX as u32 + (cp - 0x2500),
        0x2580..=0x259F => BLOCK as u32 + (cp - 0x2580),
        0xE0B0..=0xE0BF => PL as u32 + (cp - 0xE0B0),
        0xE0D2 => PL_D2 as u32,
        0xE0D4 => PL_D4 as u32,
        0x2800..=0x28FF => BRAILLE as u32 + (cp - 0x2800),
        0x1FB00..=0x1FB3B => SEXTANT as u32 + (cp - 0x1FB00),
        _ => return None,
    };
    Some(s as u16)
}

/// A built-in char that draws nothing at all (the blank braille pattern U+2800,
/// all over a btop graph's empty area): the renderer skips it entirely.
pub fn is_blank(c: char) -> bool {
    c == '\u{2800}'
}

/// The char a slot draws (the inverse of [`slot`]).
#[cfg(test)]
fn slot_char(slot: u16) -> Option<char> {
    let s = slot as u32;
    let cp = match slot {
        BOX..=127 => 0x2500 + s,
        BLOCK..=159 => 0x2580 + (s - BLOCK as u32),
        PL..=175 => 0xE0B0 + (s - PL as u32),
        PL_D2 => 0xE0D2,
        PL_D4 => 0xE0D4,
        BRAILLE..=433 => 0x2800 + (s - BRAILLE as u32),
        SEXTANT..=493 => 0x1FB00 + (s - SEXTANT as u32),
        _ => return None,
    };
    char::from_u32(cp)
}

/// Light line thickness (px) for a physical font size: `max(1, round(0.07·px))`.
/// Heavy lines are twice that; double lines are two light lines a light apart.
pub fn light_thickness(font_px: f32) -> u16 {
    let t = (font_px * 0.07).round();
    if t.is_finite() {
        (t as i64).clamp(1, MAX_LIGHT as i64) as u16
    } else {
        1
    }
}

/// The glyphon custom-glyph id for `slot` drawn with light lines of `light` px.
pub fn glyph_id(slot: u16, light: u16) -> u16 {
    (slot & SLOT_MASK) | ((light.clamp(1, MAX_LIGHT) - 1) << SLOT_BITS)
}

/// Rasterize custom glyph `id` (see [`glyph_id`]) into a `w × h` row-major alpha
/// mask spanning the whole cell. `None` for an id that is not a built-in glyph or
/// an empty size. Deterministic: the same (id, w, h) always yields the same mask
/// (glyphon re-rasterizes from it when its atlas grows).
pub fn rasterize(id: u16, w: u16, h: u16) -> Option<Vec<u8>> {
    let slot = id & SLOT_MASK;
    if slot >= SLOTS || w == 0 || h == 0 {
        return None;
    }
    let light = (id >> SLOT_BITS) + 1;
    let mut cv = Canvas::new(w as i32, h as i32, light as i32);
    match slot {
        BOX..=127 => box_drawing(&mut cv, 0x2500 + slot as u32),
        BLOCK..=159 => block(&mut cv, 0x2580 + (slot - BLOCK) as u32),
        PL..=175 => powerline(&mut cv, 0xE0B0 + (slot - PL) as u32),
        PL_D2 => trapezoids(&mut cv, false),
        PL_D4 => trapezoids(&mut cv, true),
        BRAILLE..=433 => braille(&mut cv, (slot - BRAILLE) as u8),
        _ => sextant(&mut cv, slot - SEXTANT),
    }
    Some(cv.a)
}

/// A `w × h` coverage mask plus the cell's line metrics.
struct Canvas {
    w: i32,
    h: i32,
    a: Vec<u8>,
    /// Light line thickness, clamped so a heavy line still fits the cell.
    t: i32,
}

impl Canvas {
    fn new(w: i32, h: i32, light: i32) -> Self {
        // A heavy line (2·t) must leave room on both sides of a narrow cell.
        let t = light.min((w.min(h) / 4).max(1)).max(1);
        Canvas { w, h, a: vec![0; (w * h) as usize], t }
    }

    /// Fill `[x0, x1) × [y0, y1)` (clamped to the cell) with `v` (max-blended).
    fn rect_a(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, v: u8) {
        let (x0, x1) = (x0.clamp(0, self.w), x1.clamp(0, self.w));
        let (y0, y1) = (y0.clamp(0, self.h), y1.clamp(0, self.h));
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            for px in &mut self.a[row + x0 as usize..row + x1 as usize] {
                *px = (*px).max(v);
            }
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.rect_a(x0, y0, x1, y1, 255);
    }

    /// Left edge of a vertical line of thickness `t`, centred in the cell.
    fn vx(&self, t: i32) -> i32 {
        (self.w - t) / 2
    }

    /// Top edge of a horizontal line of thickness `t`, centred in the cell.
    fn hy(&self, t: i32) -> i32 {
        (self.h - t) / 2
    }

    /// Rasterize a path (anti-aliased, exact area coverage) and max-blend it in.
    fn path(&mut self, cmds: &[Command], stroke: Option<Stroke>) {
        let mut buf = vec![0u8; self.a.len()];
        let mut mask = Mask::new(cmds);
        mask.size(self.w as u32, self.h as u32);
        if let Some(s) = stroke {
            mask.style(s);
        }
        mask.render_into(&mut buf, None);
        for (d, s) in self.a.iter_mut().zip(buf) {
            *d = (*d).max(s);
        }
    }
}

/// Weight of one arm of a box-drawing char.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wt {
    None,
    Light,
    Heavy,
    Double,
}

/// `(up, right, down, left)` arm weights of the line-drawing chars, written as
/// four chars each: `.` none, `l` light, `h` heavy, `d` double. Indexed by
/// `cp - 0x2500`; `None` marks the dashes, arcs and diagonals (drawn apart).
const BOX_ARMS: [Option<&str>; 128] = {
    let mut t: [Option<&str>; 128] = [None; 128];
    t[0x00] = Some(".l.l"); // ─
    t[0x01] = Some(".h.h"); // ━
    t[0x02] = Some("l.l."); // │
    t[0x03] = Some("h.h."); // ┃
    t[0x0C] = Some(".ll."); // ┌
    t[0x0D] = Some(".hl."); // ┍
    t[0x0E] = Some(".lh."); // ┎
    t[0x0F] = Some(".hh."); // ┏
    t[0x10] = Some("..ll"); // ┐
    t[0x11] = Some("..lh"); // ┑
    t[0x12] = Some("..hl"); // ┒
    t[0x13] = Some("..hh"); // ┓
    t[0x14] = Some("ll.."); // └
    t[0x15] = Some("lh.."); // ┕
    t[0x16] = Some("hl.."); // ┖
    t[0x17] = Some("hh.."); // ┗
    t[0x18] = Some("l..l"); // ┘
    t[0x19] = Some("l..h"); // ┙
    t[0x1A] = Some("h..l"); // ┚
    t[0x1B] = Some("h..h"); // ┛
    t[0x1C] = Some("lll."); // ├
    t[0x1D] = Some("lhl."); // ┝
    t[0x1E] = Some("hll."); // ┞
    t[0x1F] = Some("llh."); // ┟
    t[0x20] = Some("hlh."); // ┠
    t[0x21] = Some("hhl."); // ┡
    t[0x22] = Some("lhh."); // ┢
    t[0x23] = Some("hhh."); // ┣
    t[0x24] = Some("l.ll"); // ┤
    t[0x25] = Some("l.lh"); // ┥
    t[0x26] = Some("h.ll"); // ┦
    t[0x27] = Some("l.hl"); // ┧
    t[0x28] = Some("h.hl"); // ┨
    t[0x29] = Some("h.lh"); // ┩
    t[0x2A] = Some("l.hh"); // ┪
    t[0x2B] = Some("h.hh"); // ┫
    t[0x2C] = Some(".lll"); // ┬
    t[0x2D] = Some(".llh"); // ┭
    t[0x2E] = Some(".hll"); // ┮
    t[0x2F] = Some(".hlh"); // ┯
    t[0x30] = Some(".lhl"); // ┰
    t[0x31] = Some(".lhh"); // ┱
    t[0x32] = Some(".hhl"); // ┲
    t[0x33] = Some(".hhh"); // ┳
    t[0x34] = Some("ll.l"); // ┴
    t[0x35] = Some("ll.h"); // ┵
    t[0x36] = Some("lh.l"); // ┶
    t[0x37] = Some("lh.h"); // ┷
    t[0x38] = Some("hl.l"); // ┸
    t[0x39] = Some("hl.h"); // ┹
    t[0x3A] = Some("hh.l"); // ┺
    t[0x3B] = Some("hh.h"); // ┻
    t[0x3C] = Some("llll"); // ┼
    t[0x3D] = Some("lllh"); // ┽
    t[0x3E] = Some("lhll"); // ┾
    t[0x3F] = Some("lhlh"); // ┿
    t[0x40] = Some("hlll"); // ╀
    t[0x41] = Some("llhl"); // ╁
    t[0x42] = Some("hlhl"); // ╂
    t[0x43] = Some("hllh"); // ╃
    t[0x44] = Some("hhll"); // ╄
    t[0x45] = Some("llhh"); // ╅
    t[0x46] = Some("lhhl"); // ╆
    t[0x47] = Some("hhlh"); // ╇
    t[0x48] = Some("lhhh"); // ╈
    t[0x49] = Some("hlhh"); // ╉
    t[0x4A] = Some("hhhl"); // ╊
    t[0x4B] = Some("hhhh"); // ╋
    t[0x50] = Some(".d.d"); // ═
    t[0x51] = Some("d.d."); // ║
    t[0x52] = Some(".dl."); // ╒
    t[0x53] = Some(".ld."); // ╓
    t[0x54] = Some(".dd."); // ╔
    t[0x55] = Some("..ld"); // ╕
    t[0x56] = Some("..dl"); // ╖
    t[0x57] = Some("..dd"); // ╗
    t[0x58] = Some("ld.."); // ╘
    t[0x59] = Some("dl.."); // ╙
    t[0x5A] = Some("dd.."); // ╚
    t[0x5B] = Some("l..d"); // ╛
    t[0x5C] = Some("d..l"); // ╜
    t[0x5D] = Some("d..d"); // ╝
    t[0x5E] = Some("ldl."); // ╞
    t[0x5F] = Some("dld."); // ╟
    t[0x60] = Some("ddd."); // ╠
    t[0x61] = Some("l.ld"); // ╡
    t[0x62] = Some("d.dl"); // ╢
    t[0x63] = Some("d.dd"); // ╣
    t[0x64] = Some(".dld"); // ╤
    t[0x65] = Some(".ldl"); // ╥
    t[0x66] = Some(".ddd"); // ╦
    t[0x67] = Some("ld.d"); // ╧
    t[0x68] = Some("dl.l"); // ╨
    t[0x69] = Some("dd.d"); // ╩
    t[0x6A] = Some("ldld"); // ╪
    t[0x6B] = Some("dldl"); // ╫
    t[0x6C] = Some("dddd"); // ╬
    t[0x74] = Some("...l"); // ╴
    t[0x75] = Some("l..."); // ╵
    t[0x76] = Some(".l.."); // ╶
    t[0x77] = Some("..l."); // ╷
    t[0x78] = Some("...h"); // ╸
    t[0x79] = Some("h..."); // ╹
    t[0x7A] = Some(".h.."); // ╺
    t[0x7B] = Some("..h."); // ╻
    t[0x7C] = Some(".h.l"); // ╼
    t[0x7D] = Some("l.h."); // ╽
    t[0x7E] = Some(".l.h"); // ╾
    t[0x7F] = Some("h.l."); // ╿
    t
};

fn parse_arms(s: &str) -> [Wt; 4] {
    let mut out = [Wt::None; 4];
    for (o, b) in out.iter_mut().zip(s.bytes()) {
        *o = match b {
            b'l' => Wt::Light,
            b'h' => Wt::Heavy,
            b'd' => Wt::Double,
            _ => Wt::None,
        };
    }
    out
}

fn box_drawing(cv: &mut Canvas, cp: u32) {
    let i = (cp - 0x2500) as usize;
    if let Some(arms) = BOX_ARMS[i] {
        let [u, r, d, l] = parse_arms(arms);
        if [u, r, d, l].contains(&Wt::Double) {
            double_lines(cv, u, r, d, l);
        } else {
            lines(cv, u, r, d, l);
        }
        return;
    }
    let t = cv.t;
    match cp {
        // Dashed lines: (count, horizontal, heavy).
        0x2504 => dashes(cv, 3, true, t),
        0x2505 => dashes(cv, 3, true, 2 * t),
        0x2506 => dashes(cv, 3, false, t),
        0x2507 => dashes(cv, 3, false, 2 * t),
        0x2508 => dashes(cv, 4, true, t),
        0x2509 => dashes(cv, 4, true, 2 * t),
        0x250A => dashes(cv, 4, false, t),
        0x250B => dashes(cv, 4, false, 2 * t),
        0x254C => dashes(cv, 2, true, t),
        0x254D => dashes(cv, 2, true, 2 * t),
        0x254E => dashes(cv, 2, false, t),
        0x254F => dashes(cv, 2, false, 2 * t),
        0x256D..=0x2570 => arc(cv, cp),
        0x2571 => diagonal(cv, true),
        0x2572 => diagonal(cv, false),
        0x2573 => {
            diagonal(cv, true);
            diagonal(cv, false);
        }
        _ => {}
    }
}

/// Light / heavy line junctions. Every arm runs from the cell edge to the far side
/// of the perpendicular stroke, so corners are square and T/cross joins solid.
fn lines(cv: &mut Canvas, u: Wt, r: Wt, d: Wt, l: Wt) {
    let th = |w: Wt| match w {
        Wt::None => 0,
        Wt::Heavy => 2 * cv.t,
        _ => cv.t,
    };
    let (tu, tr, td, tl) = (th(u), th(r), th(d), th(l));
    let tv = tu.max(td); // thickest vertical arm
    let thz = tl.max(tr); // thickest horizontal arm
    if tl > 0 {
        let y = cv.hy(tl);
        let x1 = if tv > 0 { cv.vx(tv) + tv } else { cv.vx(tl) + tl };
        cv.rect(0, y, x1, y + tl);
    }
    if tr > 0 {
        let y = cv.hy(tr);
        let x0 = if tv > 0 { cv.vx(tv) } else { cv.vx(tr) };
        cv.rect(x0, y, cv.w, y + tr);
    }
    if tu > 0 {
        let x = cv.vx(tu);
        let y1 = if thz > 0 { cv.hy(thz) + thz } else { cv.hy(tu) + tu };
        cv.rect(x, 0, x + tu, y1);
    }
    if td > 0 {
        let x = cv.vx(td);
        let y0 = if thz > 0 { cv.hy(thz) } else { cv.hy(td) };
        cv.rect(x, y0, x + td, cv.h);
    }
}

/// Junctions involving double lines (U+2550–256C): two light strokes a light
/// apart, with outer/inner corners and T-joins that keep the channel open.
fn double_lines(cv: &mut Canvas, u: Wt, r: Wt, d: Wt, l: Wt) {
    let t = cv.t;
    let (w, h) = (cv.w, cv.h);
    let has = |x: Wt| x != Wt::None;
    let hw = if has(l) { l } else { r };
    let vw = if has(u) { u } else { d };
    // Single-line positions and the double pairs (outer edges 3·t apart).
    let (xs, ys) = (cv.vx(t), cv.hy(t));
    let xl = (w - 3 * t) / 2;
    let xr = xl + 2 * t;
    let ya = (h - 3 * t) / 2;
    let yb = ya + 2 * t;
    let hseg = |cv: &mut Canvas, y: i32, x0: i32, x1: i32| cv.rect(x0, y, x1, y + t);
    let vseg = |cv: &mut Canvas, x: i32, y0: i32, y1: i32| cv.rect(x, y0, x + t, y1);
    match (hw == Wt::Double, vw == Wt::Double) {
        (true, false) => {
            // Double horizontal (A over B), single (or no) vertical.
            let x0 = if has(l) { 0 } else { xs };
            let x1 = if has(r) { w } else { xs + t };
            hseg(cv, ya, x0, x1);
            hseg(cv, yb, x0, x1);
            if has(u) && has(d) {
                vseg(cv, xs, 0, h);
            } else if has(u) {
                vseg(cv, xs, 0, if has(l) && has(r) { ya + t } else { yb + t });
            } else if has(d) {
                vseg(cv, xs, if has(l) && has(r) { yb } else { ya }, h);
            }
        }
        (false, true) => {
            // Double vertical (L, R), single (or no) horizontal.
            let y0 = if has(u) { 0 } else { ys };
            let y1 = if has(d) { h } else { ys + t };
            vseg(cv, xl, y0, y1);
            vseg(cv, xr, y0, y1);
            if has(l) && has(r) {
                hseg(cv, ys, 0, w);
            } else if has(l) {
                hseg(cv, ys, 0, if has(u) && has(d) { xl + t } else { xr + t });
            } else if has(r) {
                hseg(cv, ys, if has(u) && has(d) { xr } else { xl }, w);
            }
        }
        _ => {
            // Double both ways: each stroke is cut where the perpendicular pair
            // passes, leaving the channels open (╬ is four corner Ls).
            let horiz = |cv: &mut Canvas, y: i32, blocked: bool| {
                if blocked {
                    if has(l) {
                        hseg(cv, y, 0, xl + t);
                    }
                    if has(r) {
                        hseg(cv, y, xr, w);
                    }
                } else if has(l) && has(r) {
                    hseg(cv, y, 0, w);
                } else if has(l) {
                    hseg(cv, y, 0, xr + t);
                } else if has(r) {
                    hseg(cv, y, xl, w);
                }
            };
            horiz(cv, ya, has(u));
            horiz(cv, yb, has(d));
            let vert = |cv: &mut Canvas, x: i32, blocked: bool| {
                if blocked {
                    if has(u) {
                        vseg(cv, x, 0, ya + t);
                    }
                    if has(d) {
                        vseg(cv, x, yb, h);
                    }
                } else if has(u) && has(d) {
                    vseg(cv, x, 0, h);
                } else if has(u) {
                    vseg(cv, x, 0, yb + t);
                } else if has(d) {
                    vseg(cv, x, ya, h);
                }
            };
            vert(cv, xl, has(l));
            vert(cv, xr, has(r));
        }
    }
}

/// `n` evenly spaced dashes along the centre line, each segment with its gap split
/// over both ends so the rhythm continues across cells.
fn dashes(cv: &mut Canvas, n: i32, horizontal: bool, t: i32) {
    let len = if horizontal { cv.w } else { cv.h };
    for i in 0..n {
        let s0 = (len * i + n / 2) / n;
        let s1 = (len * (i + 1) + n / 2) / n;
        let seg = s1 - s0;
        if seg <= 0 {
            continue;
        }
        let gap = ((seg as f32 * 0.35).round() as i32).clamp(1, (seg - 1).max(1));
        let a = s0 + gap / 2;
        let b = s1 - (gap - gap / 2);
        if b <= a {
            continue;
        }
        if horizontal {
            let y = cv.hy(t);
            cv.rect(a, y, b, y + t);
        } else {
            let x = cv.vx(t);
            cv.rect(x, a, x + t, b);
        }
    }
}

/// ╭ ╮ ╯ ╰: a quarter circle joining the light horizontal and vertical centre
/// lines, tangent to both, so it continues straight into `─` and `│` neighbours.
fn arc(cv: &mut Canvas, cp: u32) {
    let t = cv.t;
    let (w, h) = (cv.w as f32, cv.h as f32);
    let cx = cv.vx(t) as f32 + t as f32 / 2.0;
    let cy = cv.hy(t) as f32 + t as f32 / 2.0;
    let r = cx.min(w - cx).min(cy).min(h - cy).max(0.5);
    let mut p: Vec<Command> = Vec::new();
    // (horizontal edge x, vertical edge y, arc sweep) per corner.
    let (ex, ey, sweep) = match cp {
        0x256D => (w, h, ArcSweep::Negative),   // ╭ right + down
        0x256E => (0.0, h, ArcSweep::Positive), // ╮ left + down
        0x256F => (0.0, 0.0, ArcSweep::Negative), // ╯ left + up
        _ => (w, 0.0, ArcSweep::Positive),      // ╰ right + up
    };
    let sx = if ex > cx { 1.0 } else { -1.0 };
    let sy = if ey > cy { 1.0 } else { -1.0 };
    p.move_to([ex, cy])
        .line_to([cx + sx * r, cy])
        .arc_to(r, r, Angle::from_degrees(0.0), ArcSize::Small, sweep, [cx, cy + sy * r])
        .line_to([cx, ey]);
    cv.path(&p, Some(*Stroke::new(t as f32).cap(Cap::Butt).join(Join::Round)));
}

/// ╱ (`rising`) or ╲ corner to corner, stroked past the corners so diagonal
/// neighbours meet.
fn diagonal(cv: &mut Canvas, rising: bool) {
    let (w, h) = (cv.w as f32, cv.h as f32);
    let (x0, y0, x1, y1) = if rising { (0.0, h, w, 0.0) } else { (0.0, 0.0, w, h) };
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let e = cv.t as f32;
    let (ex, ey) = (dx / len * e, dy / len * e);
    let mut p: Vec<Command> = Vec::new();
    p.move_to([x0 - ex, y0 - ey]).line_to([x1 + ex, y1 + ey]);
    cv.path(&p, Some(Stroke::new(cv.t as f32)));
}

/// U+2580–259F: eighths, halves, quadrants and the three shades.
fn block(cv: &mut Canvas, cp: u32) {
    let (w, h) = (cv.w, cv.h);
    // k/8 of the width / height, rounded — the shared edges of complementary
    // blocks (▌▐, ▀▄, every quadrant) come from the same numbers.
    let cx = |k: i32| (w * k + 4) / 8;
    let cy = |k: i32| (h * k + 4) / 8;
    match cp {
        0x2580 => cv.rect(0, 0, w, cy(4)), // ▀ upper half
        0x2581..=0x2588 => {
            // ▁▂▃▄▅▆▇█ lower k/8
            let k = (cp - 0x2580) as i32;
            cv.rect(0, cy(8 - k), w, h);
        }
        0x2589..=0x258F => {
            // ▉▊▋▌▍▎▏ left (8-k)/8
            let k = 8 - (cp - 0x2588) as i32;
            cv.rect(0, 0, cx(k), h);
        }
        0x2590 => cv.rect(cx(4), 0, w, h), // ▐ right half
        0x2591 => cv.rect_a(0, 0, w, h, 64), // ░
        0x2592 => cv.rect_a(0, 0, w, h, 128), // ▒
        0x2593 => cv.rect_a(0, 0, w, h, 192), // ▓
        0x2594 => cv.rect(0, 0, w, cy(1)), // ▔ upper 1/8
        0x2595 => cv.rect(cx(7), 0, w, h), // ▕ right 1/8
        _ => {
            // Quadrants ▖▗▘▙▚▛▜▝▞▟: bit 0 UL, 1 UR, 2 LL, 3 LR.
            let q = match cp {
                0x2596 => 0b0100,
                0x2597 => 0b1000,
                0x2598 => 0b0001,
                0x2599 => 0b1101,
                0x259A => 0b1001,
                0x259B => 0b0111,
                0x259C => 0b1011,
                0x259D => 0b0010,
                0x259E => 0b0110,
                _ => 0b1110, // ▟
            };
            let (mx, my) = (cx(4), cy(4));
            if q & 1 != 0 {
                cv.rect(0, 0, mx, my);
            }
            if q & 2 != 0 {
                cv.rect(mx, 0, w, my);
            }
            if q & 4 != 0 {
                cv.rect(0, my, mx, h);
            }
            if q & 8 != 0 {
                cv.rect(mx, my, w, h);
            }
        }
    }
}

/// U+E0B0–E0BF: the Powerline (and extra) separators, stretched to the cell.
fn powerline(cv: &mut Canvas, cp: u32) {
    let (w, h) = (cv.w as f32, cv.h as f32);
    let t = cv.t as f32;
    let mut p: Vec<Command> = Vec::new();
    let fill = |cv: &mut Canvas, pts: &[[f32; 2]]| {
        let mut p: Vec<Command> = Vec::new();
        p.move_to(pts[0]);
        for &q in &pts[1..] {
            p.line_to(q);
        }
        p.close();
        cv.path(&p, None);
    };
    match cp {
        0xE0B0 => fill(cv, &[[0.0, 0.0], [w, h / 2.0], [0.0, h]]), //
        0xE0B2 => fill(cv, &[[w, 0.0], [0.0, h / 2.0], [w, h]]),   //
        0xE0B1 | 0xE0B3 => {
            //   thin chevrons, the tip on the far edge.
            let (base, tip) = if cp == 0xE0B1 { (0.0, w - t * 0.5) } else { (w, t * 0.5) };
            p.move_to([base, -t]).line_to([tip, h / 2.0]).line_to([base, h + t]);
            cv.path(&p, Some(*Stroke::new(t).join(Join::Miter).miter_limit(8.0)));
        }
        0xE0B4 | 0xE0B6 => {
            //   half ellipse filling the cell height.
            let (x, sweep) = if cp == 0xE0B4 { (0.0, ArcSweep::Positive) } else { (w, ArcSweep::Negative) };
            p.move_to([x, 0.0])
                .arc_to(w, h / 2.0, Angle::from_degrees(0.0), ArcSize::Small, sweep, [x, h])
                .close();
            cv.path(&p, None);
        }
        0xE0B5 | 0xE0B7 => {
            //   the half ellipse's outline.
            let (x, sweep) = if cp == 0xE0B5 { (0.0, ArcSweep::Positive) } else { (w, ArcSweep::Negative) };
            let (ry, rx) = (h / 2.0 - t * 0.5, w - t * 0.5);
            p.move_to([x, h / 2.0 - ry])
                .arc_to(rx, ry, Angle::from_degrees(0.0), ArcSize::Small, sweep, [x, h / 2.0 + ry]);
            cv.path(&p, Some(Stroke::new(t)));
        }
        0xE0B8 => fill(cv, &[[0.0, 0.0], [w, h], [0.0, h]]), //  ◣
        0xE0BA => fill(cv, &[[w, 0.0], [w, h], [0.0, h]]),   //  ◢
        0xE0BC => fill(cv, &[[0.0, 0.0], [w, 0.0], [0.0, h]]), //  ◤
        0xE0BE => fill(cv, &[[0.0, 0.0], [w, 0.0], [w, h]]), //  ◥
        0xE0B9 | 0xE0BF => diagonal(cv, false),              //   ╲
        _ => diagonal(cv, true),                             //   ╱ (E0BB, E0BD)
    }
}

/// U+E0D2 / U+E0D4: two trapezoids meeting in a thin gap at mid-height (the
/// "trapezoid top-bottom" Powerline extra), mirrored for E0D4.
fn trapezoids(cv: &mut Canvas, mirrored: bool) {
    let (w, h) = (cv.w as f32, cv.h as f32);
    let g = (cv.t as f32).max((h * 0.09).round()) / 2.0;
    let (edge, inner) = if mirrored { (w, w * 2.0 / 3.0) } else { (0.0, w / 3.0) };
    let far = if mirrored { 0.0 } else { w };
    for (y_edge, y_mid) in [(0.0, h / 2.0 - g), (h, h / 2.0 + g)] {
        let mut p: Vec<Command> = Vec::new();
        p.move_to([edge, y_edge]).line_to([far, y_edge]).line_to([inner, y_mid]).line_to([edge, y_mid]).close();
        cv.path(&p, None);
    }
}

/// U+2800–28FF: a 2×4 grid of square dots, evenly spread over the WHOLE cell so a
/// braille graph has the same dot pitch across rows as within them.
fn braille(cv: &mut Canvas, bits: u8) {
    let (w, h) = (cv.w, cv.h);
    let xs = [0, w / 2, w];
    let ys = [0, (h + 2) / 4, (h * 2 + 2) / 4, (h * 3 + 2) / 4, h];
    // The dot size comes from the cell HEIGHT (the same in every column): a
    // fractional cell width alternates cells of floor/ceil px, and a size taken
    // from the width would alternate with them. Never wider than its column.
    let s = ((h as f32 * 0.11).round() as i32).clamp(1, (w / 2 - 1).max(1));
    // Dot bit → (column, row): dots 1-3 left, 4-6 right, 7 left / 8 right bottom.
    const DOTS: [(usize, usize); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
    for (bit, &(c, r)) in DOTS.iter().enumerate() {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let (x0, x1) = (xs[c], xs[c + 1]);
        let (y0, y1) = (ys[r], ys[r + 1]);
        let x = x0 + (x1 - x0 - s) / 2;
        let y = y0 + (y1 - y0 - s) / 2;
        cv.rect(x, y, x + s, y + s);
    }
}

/// U+1FB00–1FB3B: 2×3 block mosaics. The 60 chars are every pattern except the
/// empty and full cells and the two half blocks (▌▐), in order.
fn sextant(cv: &mut Canvas, index: u16) {
    let mut p = index as u32 + 1;
    if p >= 21 {
        p += 1; // skip the left column (▌)
    }
    if p >= 42 {
        p += 1; // skip the right column (▐)
    }
    let (w, h) = (cv.w, cv.h);
    let mx = (w * 4 + 4) / 8; // same split as ▌▐
    let ys = [0, (h + 1) / 3, (h * 2 + 1) / 3, h];
    for cell in 0..6 {
        if p & (1 << cell) == 0 {
            continue;
        }
        let (c, r) = (cell % 2, cell / 2);
        let (x0, x1) = if c == 0 { (0, mx) } else { (mx, w) };
        cv.rect(x0, ys[r], x1, ys[r + 1]);
    }
}

/// The sextant bit pattern drawn for a U+1FB00–1FB3B char (bit 0 = upper left,
/// 1 = upper right, 2/3 = middle, 4/5 = lower). Exposed for tests.
#[cfg(test)]
fn sextant_pattern(c: char) -> Option<u32> {
    let i = (c as u32).checked_sub(0x1FB00).filter(|&i| i < 60)?;
    let mut p = i + 1;
    if p >= 21 {
        p += 1;
    }
    if p >= 42 {
        p += 1;
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(c: char, w: u16, h: u16) -> Vec<u8> {
        rasterize(glyph_id(slot(c).unwrap(), light_thickness(16.0)), w, h).unwrap()
    }

    fn mask_t(c: char, w: u16, h: u16, light: u16) -> Vec<u8> {
        rasterize(glyph_id(slot(c).unwrap(), light), w, h).unwrap()
    }

    fn at(m: &[u8], w: u16, x: i32, y: i32) -> u8 {
        m[(y * w as i32 + x) as usize]
    }

    /// Rows of column `x` that are inked (≥ 50%).
    fn col_ink(m: &[u8], w: u16, h: u16, x: i32) -> Vec<i32> {
        (0..h as i32).filter(|&y| at(m, w, x, y) >= 128).collect()
    }

    /// Columns of row `y` that are inked (≥ 50%).
    fn row_ink(m: &[u8], w: u16, y: i32) -> Vec<i32> {
        (0..w as i32).filter(|&x| at(m, w, x, y) >= 128).collect()
    }

    const SIZES: [(u16, u16); 6] = [(9, 21), (10, 21), (8, 18), (11, 22), (19, 42), (20, 41)];

    #[test]
    fn slots_round_trip_and_cover_every_range() {
        let ranges = [(0x2500, 0x257F), (0x2580, 0x259F), (0xE0B0, 0xE0BF), (0x2800, 0x28FF), (0x1FB00, 0x1FB3B)];
        let mut seen = std::collections::HashSet::new();
        for (a, b) in ranges {
            for cp in a..=b {
                let c = char::from_u32(cp).unwrap();
                let s = slot(c).unwrap_or_else(|| panic!("U+{cp:04X} has no slot"));
                assert!(s < SLOTS && seen.insert(s), "U+{cp:04X} slot {s} reused");
                assert_eq!(slot_char(s), Some(c));
            }
        }
        for c in ['\u{E0D2}', '\u{E0D4}'] {
            assert_eq!(slot_char(slot(c).unwrap()), Some(c));
        }
        // Neighbours of the ranges stay with the font.
        for c in ['a', '\u{24FF}', '\u{25A0}', '\u{E0A0}', '\u{E0C0}', '\u{E0D3}', '\u{27FF}', '\u{2900}', '\u{1FB3C}'] {
            assert_eq!(slot(c), None, "{c:?}");
        }
        assert!(is_blank('\u{2800}') && !is_blank('\u{2801}'));
    }

    #[test]
    fn ids_carry_the_thickness_and_reject_junk() {
        let s = slot('─').unwrap();
        assert_ne!(glyph_id(s, 1), glyph_id(s, 2), "thickness is part of the cache key");
        assert_eq!(glyph_id(s, 1) & SLOT_MASK, s);
        assert_eq!(light_thickness(16.0), 1);
        assert_eq!(light_thickness(14.0), 1);
        assert_eq!(light_thickness(22.0), 2);
        assert_eq!(light_thickness(32.0), 2);
        assert_eq!(light_thickness(0.0), 1);
        assert_eq!(light_thickness(f32::NAN), 1);
        assert!(rasterize(SLOTS, 9, 21).is_none());
        assert!(rasterize(glyph_id(s, 1), 0, 21).is_none());
        // Every slot rasterizes to exactly w*h bytes at odd and even sizes.
        for s in 0..SLOTS {
            for (w, h) in SIZES {
                let m = rasterize(glyph_id(s, 1), w, h).unwrap();
                assert_eq!(m.len(), w as usize * h as usize, "slot {s} at {w}x{h}");
            }
        }
    }

    #[test]
    fn rasterizing_is_deterministic() {
        // glyphon re-rasterizes on atlas growth and asserts the same content type;
        // the mask must be identical every time.
        for s in [0u16, 0x6C, 0x6D, BLOCK + 0x11, PL, PL + 4, BRAILLE + 0x55, SEXTANT + 7] {
            let a = rasterize(glyph_id(s, 2), 19, 42);
            assert_eq!(a, rasterize(glyph_id(s, 2), 19, 42), "slot {s}");
        }
    }

    #[test]
    fn horizontal_and_vertical_lines_span_the_cell_edge_to_edge() {
        for (w, h) in SIZES {
            let m = mask('─', w, h);
            // Same rows inked in the first and last column: neighbours join.
            let first = col_ink(&m, w, h, 0);
            assert!(!first.is_empty(), "─ {w}x{h}");
            assert_eq!(first, col_ink(&m, w, h, w as i32 - 1), "─ {w}x{h}");
            // Fully opaque, crisp (no half-covered rows).
            assert!(m.iter().all(|&v| v == 0 || v == 255), "─ is crisp at {w}x{h}");
            let v = mask('│', w, h);
            let top = row_ink(&v, w, 0);
            assert!(!top.is_empty());
            assert_eq!(top, row_ink(&v, w, h as i32 - 1), "│ {w}x{h}");
        }
    }

    #[test]
    fn junctions_line_up_with_the_straight_lines() {
        // Every char with a right arm puts it on the rows ─ uses; every char with a
        // down arm on the columns │ uses — the property that makes a box seamless.
        for (w, h) in SIZES {
            let horiz = col_ink(&mask('─', w, h), w, h, w as i32 - 1);
            let vert = row_ink(&mask('│', w, h), w, h as i32 - 1);
            for c in ['┌', '┬', '├', '┼', '└', '┴', '╭', '╰', '╶'] {
                let m = mask(c, w, h);
                assert_eq!(col_ink(&m, w, h, w as i32 - 1), horiz, "{c} right arm at {w}x{h}");
            }
            for c in ['┐', '┤', '┘', '┴', '╮', '╯', '╴'] {
                let m = mask(c, w, h);
                assert_eq!(col_ink(&m, w, h, 0), horiz, "{c} left arm at {w}x{h}");
            }
            for c in ['┌', '┐', '┬', '├', '┤', '┼', '╭', '╮', '╷'] {
                let m = mask(c, w, h);
                assert_eq!(row_ink(&m, w, h as i32 - 1), vert, "{c} down arm at {w}x{h}");
            }
            for c in ['└', '┘', '┴', '├', '┤', '┼', '╰', '╯', '╵'] {
                let m = mask(c, w, h);
                assert_eq!(row_ink(&m, w, 0), vert, "{c} up arm at {w}x{h}");
            }
        }
    }

    #[test]
    fn corners_are_closed_and_mirror_symmetric() {
        for (w, h) in SIZES {
            // ┌ mirrored left-right is ┐, top-bottom is └.
            let tl = mask('┌', w, h);
            let tr = mask('┐', w, h);
            let bl = mask('└', w, h);
            let t = Canvas::new(w as i32, h as i32, 1).t;
            let (vx, hy) = (((w as i32) - t) / 2, ((h as i32) - t) / 2);
            // The corner pixel is inked.
            assert_eq!(at(&tl, w, vx, hy), 255, "┌ corner at {w}x{h}");
            assert_eq!(at(&tr, w, vx, hy), 255, "┐ corner at {w}x{h}");
            assert_eq!(at(&bl, w, vx, hy), 255, "└ corner at {w}x{h}");
            // Nothing outside the arms: ┌ has no ink above the line or left of it.
            for y in 0..hy {
                for x in 0..w as i32 {
                    assert_eq!(at(&tl, w, x, y), 0, "┌ ink above the corner at {w}x{h}");
                }
            }
            for y in 0..h as i32 {
                for x in 0..vx {
                    assert_eq!(at(&tl, w, x, y), 0, "┌ ink left of the corner at {w}x{h}");
                }
            }
        }
        // Rounded corners: mirrors of one another on an odd-width cell whose centre
        // lines sit on the cell's centre pixel (± anti-aliasing rounding).
        let (w, h) = (9u16, 21u16);
        let a = mask('╭', w, h);
        let b = mask('╮', w, h);
        let c = mask('╰', w, h);
        let near = |p: u8, q: u8| (p as i32 - q as i32).abs() <= 2;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                assert!(near(at(&a, w, x, y), at(&b, w, w as i32 - 1 - x, y)), "╭/╮ at {x},{y}");
                assert!(near(at(&a, w, x, y), at(&c, w, x, h as i32 - 1 - y)), "╭/╰ at {x},{y}");
            }
        }
    }

    #[test]
    fn rounded_corners_bend_inside_the_corner() {
        // The arc's midpoint lies on the circle centred one radius into the
        // quadrant the arms open to — the corner of the centre lines stays empty.
        for (w, h) in SIZES {
            let t = Canvas::new(w as i32, h as i32, 1).t;
            let cx = ((w as i32 - t) / 2) as f32 + t as f32 / 2.0;
            let cy = ((h as i32 - t) / 2) as f32 + t as f32 / 2.0;
            let r = cx.min(w as f32 - cx).min(cy).min(h as f32 - cy);
            let k = r * (1.0 - std::f32::consts::FRAC_1_SQRT_2);
            for (c, sx, sy) in [('╭', 1.0, 1.0), ('╮', -1.0, 1.0), ('╯', -1.0, -1.0), ('╰', 1.0, -1.0)] {
                let m = mask(c, w, h);
                let (mx, my) = (cx + sx * k, cy + sy * k);
                let (px, py) = (mx.floor() as i32, my.floor() as i32);
                let near_mid = (-1..=1)
                    .flat_map(|dy| (-1..=1).map(move |dx| (px + dx, py + dy)))
                    .filter(|&(x, y)| x >= 0 && y >= 0 && x < w as i32 && y < h as i32)
                    .map(|(x, y)| at(&m, w, x, y))
                    .max()
                    .unwrap_or(0);
                assert!(near_mid > 128, "{c} midpoint at {w}x{h}");
                // The opposite bulge (the arc drawn the wrong way round) is empty.
                let (ox, oy) = (cx + sx * (r - k), cy + sy * (r - k));
                assert_eq!(at(&m, w, ox.floor() as i32, oy.floor() as i32), 0, "{c} wrong-way bulge at {w}x{h}");
            }
        }
    }

    #[test]
    fn heavy_is_twice_light_and_double_is_two_light_lines() {
        let (w, h) = (19u16, 42u16);
        let light = col_ink(&mask_t('─', w, h, 2), w, h, 0).len();
        let heavy = col_ink(&mask_t('━', w, h, 2), w, h, 0).len();
        assert_eq!((light, heavy), (2, 4));
        let double = col_ink(&mask_t('═', w, h, 2), w, h, 0);
        assert_eq!(double.len(), 4, "two light strokes");
        assert_eq!(double[2] - double[1], 3, "a light-wide gap between them");
        // The double pair is centred on the single line.
        let single = col_ink(&mask_t('─', w, h, 2), w, h, 0);
        let mid_single = (single[0] + single[1]) as f32 / 2.0;
        let mid_double = (double[0] + double[3]) as f32 / 2.0;
        assert!((mid_single - mid_double).abs() <= 0.5);
    }

    #[test]
    fn double_corners_keep_the_channel_open() {
        let (w, h) = (19u16, 42u16);
        let m = mask_t('╬', w, h, 2);
        // The centre of ╬ is empty (four corner Ls, no crossing strokes).
        assert_eq!(at(&m, w, w as i32 / 2, h as i32 / 2), 0);
        // ╔ joins ═ to its right and ║ below it.
        let corner = mask_t('╔', w, h, 2);
        assert_eq!(col_ink(&corner, w, h, w as i32 - 1), col_ink(&mask_t('═', w, h, 2), w, h, 0));
        assert_eq!(row_ink(&corner, w, h as i32 - 1), row_ink(&mask_t('║', w, h, 2), w, 0));
    }

    #[test]
    fn blocks_are_cell_exact_and_complementary() {
        for (w, h) in SIZES {
            let full = mask('█', w, h);
            assert!(full.iter().all(|&v| v == 255), "█ fills the cell at {w}x{h}");
            // ▀ + ▄ and ▌ + ▐ tile the cell exactly once.
            for (a, b) in [('▀', '▄'), ('▌', '▐'), ('▘', '▟'), ('▝', '▙'), ('▚', '▞')] {
                let (ma, mb) = (mask(a, w, h), mask(b, w, h));
                for (i, (&x, &y)) in ma.iter().zip(&mb).enumerate() {
                    assert_eq!(x as u32 + y as u32, 255, "{a}{b} pixel {i} at {w}x{h}");
                }
            }
            // Lower eighths grow monotonically and end at the bottom edge.
            let mut prev = 0;
            for k in 1..=8u32 {
                let m = mask(char::from_u32(0x2580 + k).unwrap(), w, h);
                let ink = col_ink(&m, w, h, 0);
                assert_eq!(*ink.last().unwrap(), h as i32 - 1, "lower {k}/8 touches the bottom");
                assert!(ink.len() >= prev, "lower {k}/8 shrank");
                prev = ink.len();
            }
            // Shades are uniform partial coverage.
            for (c, v) in [('░', 64u8), ('▒', 128), ('▓', 192)] {
                assert!(mask(c, w, h).iter().all(|&p| p == v), "{c}");
            }
        }
    }

    #[test]
    fn powerline_separators_touch_the_cell_edges() {
        for (w, h) in SIZES {
            // : the full left edge, the tip at mid-height of the right edge.
            let m = mask('\u{E0B0}', w, h);
            for y in 1..h as i32 - 1 {
                assert!(at(&m, w, 0, y) > 128, " left edge row {y} at {w}x{h}");
            }
            let mid = h as i32 / 2;
            assert!(at(&m, w, w as i32 - 1, mid) > 0, " tip at {w}x{h}");
            assert_eq!(at(&m, w, w as i32 - 1, 0), 0);
            //  mirrors  exactly.
            let l = mask('\u{E0B2}', w, h);
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let (a, b) = (at(&m, w, x, y) as i32, at(&l, w, w as i32 - 1 - x, y) as i32);
                    assert!((a - b).abs() <= 1, " vs  at {x},{y} ({w}x{h})");
                }
            }
            //  (rounded): the left edge solid top to bottom, symmetric top/bottom.
            let r = mask('\u{E0B4}', w, h);
            for y in 2..h as i32 - 2 {
                assert!(at(&r, w, 0, y) > 200, " left edge row {y} at {w}x{h}");
            }
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    // (zeno flattens the arc into curves: a few levels of AA noise)
                    let (a, b) = (at(&r, w, x, y) as i32, at(&r, w, x, h as i32 - 1 - y) as i32);
                    assert!((a - b).abs() <= 4, " symmetric at {x},{y} ({w}x{h})");
                }
            }
            let rl = mask('\u{E0B6}', w, h);
            assert!(at(&rl, w, w as i32 - 1, h as i32 / 2) > 200, " right edge");
        }
    }

    #[test]
    fn braille_dots_spread_over_the_whole_cell() {
        for (w, h) in SIZES {
            let all = mask('\u{28FF}', w, h);
            let rows: Vec<i32> = (0..h as i32).filter(|&y| !row_ink(&all, w, y).is_empty()).collect();
            // Four dot rows: the first in the top quarter, the last in the bottom one.
            assert!(rows[0] < h as i32 / 4 && *rows.last().unwrap() >= h as i32 * 3 / 4, "{w}x{h}: {rows:?}");
            // Each single dot is its own bit.
            for bit in 0..8u32 {
                let m = mask(char::from_u32(0x2800 + (1 << bit)).unwrap(), w, h);
                let n = m.iter().filter(|&&v| v == 255).count();
                assert!(n > 0 && n == all.iter().filter(|&&v| v == 255).count() / 8, "dot {bit} at {w}x{h}");
            }
            // Cells one pixel apart in width (a fractional cell width alternates
            // them) draw the same dot size — no checkered graph.
            let narrower = mask('\u{28FF}', w - 1, h);
            let dots = |m: &[u8]| m.iter().filter(|&&v| v == 255).count();
            assert_eq!(dots(&all), dots(&narrower), "dot size at {w}x{h} vs {}x{h}", w - 1);
            // ⠁ (dot 1) is upper left, ⢀ (dot 8) lower right.
            let d1 = mask('\u{2801}', w, h);
            let d8 = mask('\u{2880}', w, h);
            let first = |m: &[u8]| m.iter().position(|&v| v == 255).unwrap() as i32;
            let (x1, y1) = (first(&d1) % w as i32, first(&d1) / w as i32);
            let (x8, y8) = (first(&d8) % w as i32, first(&d8) / w as i32);
            assert!(x1 < w as i32 / 2 && y1 < h as i32 / 4);
            assert!(x8 >= w as i32 / 2 && y8 >= h as i32 * 3 / 4);
        }
    }

    #[test]
    fn sextants_skip_the_half_blocks_and_tile() {
        assert_eq!(sextant_pattern('\u{1FB00}'), Some(1));
        assert_eq!(sextant_pattern('\u{1FB13}'), Some(20));
        assert_eq!(sextant_pattern('\u{1FB14}'), Some(22), "skips ▌ (21)");
        assert_eq!(sextant_pattern('\u{1FB28}'), Some(43), "skips ▐ (42)");
        assert_eq!(sextant_pattern('\u{1FB3B}'), Some(62));
        assert_eq!(sextant_pattern('\u{1FB3C}'), None);
        for (w, h) in SIZES {
            // 🬀 (upper left) + its complement 🬻 (pattern 62) tile the cell.
            let (a, b) = (mask('\u{1FB00}', w, h), mask('\u{1FB3B}', w, h));
            for (&x, &y) in a.iter().zip(&b) {
                assert_eq!(x as u32 + y as u32, 255);
            }
            // The left column splits where ▌ does (pattern 4 = middle left).
            let half = mask('▌', w, h);
            let mid_row = h as i32 / 2;
            let mid_left = mask('\u{1FB03}', w, h);
            assert_eq!(row_ink(&mid_left, w, mid_row), row_ink(&half, w, mid_row));
        }
    }

    #[test]
    fn dashes_repeat_with_even_rhythm() {
        let (w, h) = (12u16, 24u16);
        for (c, n) in [('┄', 3usize), ('┈', 4), ('╌', 2)] {
            let m = mask(c, w, h);
            let y = col_ink(&mask('─', w, h), w, h, 0)[0];
            let ink = row_ink(&m, w, y);
            // n runs of ink.
            let runs = ink.windows(2).filter(|p| p[1] != p[0] + 1).count() + 1;
            assert_eq!(runs, n, "{c}: {ink:?}");
            assert!(ink.len() < w as usize, "{c} has gaps");
        }
        let v = mask('┆', w, h);
        let x = row_ink(&mask('│', w, h), w, 0)[0];
        let ink = col_ink(&v, w, h, x);
        let runs = ink.windows(2).filter(|p| p[1] != p[0] + 1).count() + 1;
        assert_eq!(runs, 3);
    }

    #[test]
    fn diagonals_reach_both_corners() {
        let (w, h) = (10u16, 21u16);
        let m = mask('╱', w, h);
        assert!(at(&m, w, w as i32 - 1, 0) > 0 && at(&m, w, 0, h as i32 - 1) > 0);
        assert_eq!(at(&m, w, 0, 0), 0);
        let x = mask('╳', w, h);
        assert!(at(&x, w, 0, 0) > 0 && at(&x, w, w as i32 - 1, h as i32 - 1) > 0);
    }
}
