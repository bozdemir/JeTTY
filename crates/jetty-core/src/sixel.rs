//! Pure, dependency-free, SAFE sixel (DEC DCS `ESC P … q … ST`) decoder.
//!
//! Sixel data arrives from the PTY — i.e. it is UNTRUSTED. A malformed or
//! adversarial sixel MUST NEVER panic, over-allocate, or write out of bounds.
//! Every guarantee here is structural, not incidental:
//!
//! * **No trust in the declared raster** (`" Pan;Pad;Ph;Pv`). A one-line
//!   adversarial sixel can declare a tiny raster then write pixels far past it,
//!   or declare a gigantic raster to force a huge allocation. So sizing is done
//!   by a **bounded measuring pass** over the actual data (`measure`), which
//!   rejects (returns `None`) the instant any set pixel or repeat run would
//!   exceed the caps. The raster attributes are parsed only to keep the token
//!   stream in sync; they never drive allocation.
//! * **Overflow-safe counting.** Every parsed number is folded digit-by-digit
//!   with `saturating_mul(10).saturating_add(d)` (never wraps); all position
//!   math is `u64`; the final `w * h * 4` allocation size is checked against
//!   `max_pixels` in `u64` BEFORE the `Vec` is created.
//! * **Bounds-checked writes.** Even though `measure` guarantees the fit, the
//!   fill pass (`render_into`) still bounds-checks every pixel write
//!   (`x < w && y < h`), so a divergence between the two passes can only clip a
//!   pixel — never corrupt memory.
//!
//! Color registers: RGB (`#Pc;2;…`) and DEC HLS (`#Pc;1;…`) are both supported.
//! DEC HLS uses a rotated hue (0° = BLUE); the conversion offsets the hue by
//! +240° into the standard HSL space (unit-tested against blue/red/green).
//!
//! Unwritten pixels stay transparent (`a == 0`) so the terminal background shows
//! through — the DCS `P2` background-select mode is accepted but not yet acted on
//! (reserved; MVP is always-transparent), matching most modern sixel terminals.

/// A decoded sixel image as a tightly-packed RGBA8 buffer
/// (`rgba.len() == width * height * 4`), sRGB-encoded (the color registers are
/// sRGB values), premultiplied-alpha-safe (unwritten pixels are all-zero).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SixelImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Hard size caps enforced by [`decode_sixel`]. A decode that would exceed any
/// of these returns `None` (correct-or-absent). `max_pixels` bounds the RGBA
/// allocation (`max_pixels * 4` bytes); `max_w`/`max_h` bound each dimension so
/// the result also fits a GPU texture (the render layer additionally clamps to
/// the adapter's real `max_texture_dimension_2d`).
#[derive(Clone, Copy, Debug)]
pub struct SixelCaps {
    pub max_w: u32,
    pub max_h: u32,
    pub max_pixels: u32,
}

/// Default caps: 4096×4096, 16 M pixels (≤ 64 MiB RGBA). Comfortably above any
/// real terminal image; well within a device's max texture size and the VRAM
/// budget.
pub const SIXEL_CAPS: SixelCaps =
    SixelCaps { max_w: 4096, max_h: 4096, max_pixels: 16_000_000 };

/// Number of color registers (a sixel `#Pc` selects `Pc % 256`).
const PALETTE_LEN: usize = 256;

/// Decode a sixel DCS body into an RGBA image.
///
/// `p2` is the DCS `P2` background-select parameter (0/2 = fill unset with the
/// background, 1 = leave unset transparent). It is accepted for forward
/// compatibility but not yet acted on — the MVP always leaves unwritten pixels
/// transparent, which is the safest and best-looking default. `data` is the raw
/// bytes AFTER the introducing `q`, BEFORE the string terminator.
///
/// Returns `None` on empty / structurally-empty input or any cap violation
/// (correct-or-absent). Pure: no I/O, no globals, never panics.
pub fn decode_sixel(p2: u32, data: &[u8], caps: SixelCaps) -> Option<SixelImage> {
    // Reserved: DECSDM background-fill mode. MVP keeps unset pixels transparent
    // regardless, so P2 does not change the output yet.
    let _ = p2;

    let (width, height) = measure(data, caps)?;
    // `measure` already proved width*height <= max_pixels in u64, so this cannot
    // overflow usize on any 64-bit target and stays within the cap.
    let len = (width as usize) * (height as usize) * 4;
    let mut rgba = vec![0u8; len];
    render_into(data, width, height, &mut rgba);
    Some(SixelImage { width, height, rgba })
}

/// FNV-1a content id over `(width, height, rgba)`. Folding the dimensions in
/// (not just the pixel bytes) makes an id collision require matching BOTH the
/// content and the geometry — so two DISTINCT images can share a texture only on
/// an astronomically unlikely full-64-bit collision. Used to dedupe identical
/// repainted frames (a TUI redrawing the same image reuses its GPU texture).
pub fn content_id(img: &SixelImage) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut fold = |b: u8| {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    };
    for &b in &img.width.to_le_bytes() {
        fold(b);
    }
    for &b in &img.height.to_le_bytes() {
        fold(b);
    }
    for &b in &img.rgba {
        fold(b);
    }
    hash
}

/// Bounded measuring pass: determine the true drawn extent (rightmost and
/// bottom-most set pixel) while rejecting anything that would exceed the caps.
/// Returns `(width, height)` (both ≥ 1) or `None`.
///
/// Position math is `u64` and every count is saturating, so no arithmetic here
/// can overflow or loop unboundedly (a huge `!Pn` repeat is resolved to an end
/// column in O(1) and rejected if it crosses `max_w`).
fn measure(data: &[u8], caps: SixelCaps) -> Option<(u32, u32)> {
    let max_w = caps.max_w as u64;
    let max_h = caps.max_h as u64;
    let mut i = 0usize;
    let mut x: u64 = 0;
    let mut band: u64 = 0;
    // Highest column / row index touched by a SET pixel (-1 = nothing drawn).
    let mut max_col: i64 = -1;
    let mut max_row: i64 = -1;

    while i < data.len() {
        let b = data[i];
        match b {
            // Raster attributes / color introducer: parse & discard the numeric
            // params so their digits are not misread as data. Colors do not
            // affect geometry, so measurement ignores them.
            0x22 => {
                i += 1;
                skip_params(data, &mut i);
            }
            0x23 => {
                i += 1;
                skip_params(data, &mut i);
            }
            // Repeat: `!Pn <databyte>` — apply the next data byte Pn times.
            0x21 => {
                i += 1;
                let n = parse_uint(data, &mut i) as u64;
                if i < data.len() && (0x3F..=0x7E).contains(&data[i]) {
                    let v = data[i] - 0x3F;
                    i += 1;
                    let end = x.saturating_add(n);
                    if v != 0 && n > 0 {
                        // Rightmost written column is end-1; reject if past max_w.
                        if end > max_w {
                            return None;
                        }
                        let row = band * 6 + hi_bit(v) as u64;
                        if row >= max_h {
                            return None;
                        }
                        max_col = max_col.max((end - 1) as i64);
                        max_row = max_row.max(row as i64);
                    }
                    x = end;
                }
            }
            // Graphics carriage return: back to column 0, same band.
            0x24 => {
                x = 0;
                i += 1;
            }
            // Graphics line feed: next 6-pixel band, column 0.
            0x2D => {
                band += 1;
                x = 0;
                i += 1;
            }
            // Sixel data byte: six vertical pixels, bit0 = top.
            0x3F..=0x7E => {
                let v = b - 0x3F;
                i += 1;
                if v != 0 {
                    if x >= max_w {
                        return None;
                    }
                    let row = band * 6 + hi_bit(v) as u64;
                    if row >= max_h {
                        return None;
                    }
                    max_col = max_col.max(x as i64);
                    max_row = max_row.max(row as i64);
                }
                x += 1;
            }
            // Whitespace / unknown: ignore (keeps the parser resilient).
            _ => {
                i += 1;
            }
        }
    }

    if max_col < 0 || max_row < 0 {
        return None; // nothing was drawn
    }
    let width = (max_col + 1) as u64;
    let height = (max_row + 1) as u64;
    if width > max_w || height > max_h {
        return None;
    }
    if width * height > caps.max_pixels as u64 {
        return None;
    }
    Some((width as u32, height as u32))
}

/// Fill pass: re-parse the same token stream, building the palette and writing
/// each set pixel. Every write is bounds-checked (`x < w && y < h`), so this can
/// never index outside `buf` even if it disagreed with `measure`.
fn render_into(data: &[u8], width: u32, height: u32, buf: &mut [u8]) {
    let w = width as u64;
    let h = height as u64;
    let mut palette = default_palette();
    let mut cur: usize = 0;
    let mut i = 0usize;
    let mut x: u64 = 0;
    let mut band: u64 = 0;

    while i < data.len() {
        let b = data[i];
        match b {
            0x22 => {
                i += 1;
                skip_params(data, &mut i);
            }
            0x23 => {
                i += 1;
                parse_color(data, &mut i, &mut palette, &mut cur);
            }
            0x21 => {
                i += 1;
                let n = parse_uint(data, &mut i) as u64;
                if i < data.len() && (0x3F..=0x7E).contains(&data[i]) {
                    let v = data[i] - 0x3F;
                    i += 1;
                    if v != 0 {
                        let rgb = palette[cur];
                        let mut k = 0u64;
                        while k < n {
                            let px = x + k;
                            if px >= w {
                                break;
                            }
                            plot_col(buf, w, h, px, band, v, rgb);
                            k += 1;
                        }
                    }
                    x = x.saturating_add(n);
                }
            }
            0x24 => {
                x = 0;
                i += 1;
            }
            0x2D => {
                band += 1;
                x = 0;
                i += 1;
            }
            0x3F..=0x7E => {
                let v = b - 0x3F;
                i += 1;
                if v != 0 && x < w {
                    plot_col(buf, w, h, x, band, v, palette[cur]);
                }
                x += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
}

/// Write the six vertical pixels of one sixel column value `v` at pen column `x`
/// / band `band`. bit0 = top row, bit5 = bottom. Bounds-checked defensively.
#[inline]
fn plot_col(buf: &mut [u8], w: u64, h: u64, x: u64, band: u64, v: u8, rgb: [u8; 3]) {
    for bit in 0..6u64 {
        if v & (1 << bit) != 0 {
            let y = band * 6 + bit;
            if y < h && x < w {
                let off = ((y * w + x) * 4) as usize;
                // The measure pass proved this fits, but the explicit slice
                // index is still guarded above — no unchecked writes.
                buf[off] = rgb[0];
                buf[off + 1] = rgb[1];
                buf[off + 2] = rgb[2];
                buf[off + 3] = 255;
            }
        }
    }
}

/// Index of the highest set bit of a sixel value `v` (1..=63) in `0..=5`.
/// (For `v == 0` there is no set bit; callers guard `v != 0` first.)
#[inline]
fn hi_bit(v: u8) -> u8 {
    // v occupies bits 0..=5; leading_zeros of a nonzero u8 is 0..=7.
    7 - (v & 0x3f).leading_zeros() as u8
}

/// Saturating digit-by-digit unsigned parse from `data[*i]` (advances `*i` past
/// the digits). Returns 0 if no digit is present. Never overflows.
fn parse_uint(data: &[u8], i: &mut usize) -> u32 {
    let mut v: u32 = 0;
    while *i < data.len() && data[*i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((data[*i] - b'0') as u32);
        *i += 1;
    }
    v
}

/// Consume a `;`-separated list of numeric params (raster attributes), advancing
/// past them without acting. Keeps stray digits from leaking into the data loop.
fn skip_params(data: &[u8], i: &mut usize) {
    loop {
        let _ = parse_uint(data, i);
        if *i < data.len() && data[*i] == b';' {
            *i += 1;
        } else {
            break;
        }
    }
}

/// Parse a color introducer `#Pc` (select) or `#Pc;Pu;Px;Py;Pz` (define) and
/// update `palette` / `cur`. All params are consumed even if there are more than
/// five, so the parser never desyncs.
fn parse_color(data: &[u8], i: &mut usize, palette: &mut [[u8; 3]; PALETTE_LEN], cur: &mut usize) {
    let mut vals = [0u32; 5];
    let mut count = 0usize;
    loop {
        let v = parse_uint(data, i);
        if count < 5 {
            vals[count] = v;
        }
        count += 1;
        if *i < data.len() && data[*i] == b';' {
            *i += 1;
        } else {
            break;
        }
    }
    let pc = (vals[0] as usize) % PALETTE_LEN;
    if count >= 5 {
        let rgb = match vals[1] {
            // Pu == 2: RGB, each component a 0..100 percentage.
            2 => [
                pct_to_u8(vals[2]),
                pct_to_u8(vals[3]),
                pct_to_u8(vals[4]),
            ],
            // Pu == 1: DEC HLS (hue 0..360 with 0° = blue, L/S 0..100).
            1 => hls_to_rgb(vals[2], vals[3], vals[4]),
            // Pu == 0 / unknown: keep the register's current value.
            _ => palette[pc],
        };
        palette[pc] = rgb;
        *cur = pc;
    } else {
        // Bare `#Pc` selects the register.
        *cur = pc;
    }
}

/// A 0..100 percentage → 0..255 (rounded), clamped.
#[inline]
fn pct_to_u8(p: u32) -> u8 {
    let p = p.min(100);
    ((p * 255 + 50) / 100) as u8
}

/// DEC HLS → RGB. DEC's hue is rotated so 0° = BLUE (not red); offsetting by
/// +240° maps it into the standard HSL hue circle, after which the ordinary
/// HSL→RGB conversion applies. Unit-tested against blue(0°)/red(120°)/green(240°).
fn hls_to_rgb(hue: u32, lum: u32, sat: u32) -> [u8; 3] {
    let h = ((hue % 360) as f64 + 240.0) % 360.0;
    let l = (lum.min(100) as f64) / 100.0;
    let s = (sat.min(100) as f64) / 100.0;
    hsl_to_rgb(h, l, s)
}

/// Standard HSL → RGB. `h` in `[0, 360)`, `l`/`s` in `[0, 1]`.
fn hsl_to_rgb(h: f64, l: f64, s: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let cv = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [cv(r1), cv(g1), cv(b1)]
}

/// The 16 DEC VT340 default color registers, converted from their percentage
/// definitions; registers 16..256 default to black. Real encoders redefine the
/// palette (`#Pc;2;…`), so these matter only for hand-written sixels that select
/// a color without defining it.
fn default_palette() -> [[u8; 3]; PALETTE_LEN] {
    let mut p = [[0u8; 3]; PALETTE_LEN];
    // (r, g, b) already in 0..255, from the DEC percentage table.
    const DEC16: [[u8; 3]; 16] = [
        [0, 0, 0],       // 0  black
        [51, 51, 204],   // 1  blue
        [204, 33, 33],   // 2  red
        [51, 204, 51],   // 3  green
        [204, 51, 204],  // 4  magenta
        [51, 204, 204],  // 5  cyan
        [204, 204, 51],  // 6  yellow
        [135, 135, 135], // 7  gray 50%
        [66, 66, 66],    // 8  gray 25%
        [84, 84, 153],   // 9  blue*
        [153, 66, 66],   // 10 red*
        [84, 153, 84],   // 11 green*
        [153, 84, 153],  // 12 magenta*
        [84, 153, 153],  // 13 cyan*
        [153, 153, 84],  // 14 yellow*
        [204, 204, 204], // 15 gray 75%
    ];
    p[..16].copy_from_slice(&DEC16);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: SixelCaps = SIXEL_CAPS;

    // Pixel accessor for assertions.
    fn px(img: &SixelImage, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * img.width + x) * 4) as usize;
        [
            img.rgba[off],
            img.rgba[off + 1],
            img.rgba[off + 2],
            img.rgba[off + 3],
        ]
    }

    #[test]
    fn hi_bit_positions() {
        assert_eq!(hi_bit(0b000001), 0);
        assert_eq!(hi_bit(0b000010), 1);
        assert_eq!(hi_bit(0b100000), 5);
        assert_eq!(hi_bit(0b100001), 5);
        assert_eq!(hi_bit(0b011111), 4);
    }

    #[test]
    fn single_full_column_is_1x6() {
        // Define color 0 as pure red (RGB), select it, then `~` = 0x7E = all six
        // bits set → a 1×6 column of red.
        let img = decode_sixel(0, b"#0;2;100;0;0#0~", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 6));
        for y in 0..6 {
            assert_eq!(px(&img, 0, y), [255, 0, 0, 255], "row {y} red");
        }
    }

    #[test]
    fn top_bit_only_is_1x1_opaque_rest_transparent() {
        // '?' = 0x3F → value 0 (no pixels). '@' = 0x40 → value 1 → bit0 (top).
        let img = decode_sixel(0, b"#0;2;0;100;0#0@", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(px(&img, 0, 0), [0, 255, 0, 255], "top pixel green");
    }

    #[test]
    fn repeat_advances_without_pixels_then_draws() {
        // `!3?` advances x by 3 with NO pixels; then `~` draws a full column at
        // x = 3 → image is 4 wide (cols 0..3), col 3 fully painted, 0..2 clear.
        let img = decode_sixel(0, b"#0;2;100;100;100#0!3?~", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (4, 6));
        for x in 0..3 {
            assert_eq!(px(&img, x, 0)[3], 0, "col {x} transparent");
        }
        assert_eq!(px(&img, 3, 0), [255, 255, 255, 255], "col 3 white");
    }

    #[test]
    fn repeat_paints_a_run() {
        // `!4~` paints four full columns.
        let img = decode_sixel(0, b"#0;2;100;0;0#0!4~", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (4, 6));
        for x in 0..4 {
            for y in 0..6 {
                assert_eq!(px(&img, x, y), [255, 0, 0, 255], "({x},{y}) red");
            }
        }
    }

    #[test]
    fn graphics_cr_overlays_same_band() {
        // Paint col0 top-bit red, `$` (CR) back to col0, select green, paint
        // bottom-bit → same column, two colors in one band. '@'=bit0, 0x60='`'?
        // Use value 0x20+0x3F = '_' (0x5F) = bit5 (bottom).
        let img = decode_sixel(0, b"#0;2;100;0;0#0@$#1;2;0;100;0#1_", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 6));
        assert_eq!(px(&img, 0, 0), [255, 0, 0, 255], "top red");
        assert_eq!(px(&img, 0, 5), [0, 255, 0, 255], "bottom green");
        assert_eq!(px(&img, 0, 1)[3], 0, "middle transparent");
    }

    #[test]
    fn graphics_lf_starts_new_band() {
        // Band 0: col0 full. `-` → band 1. Band 1: col0 full. Height = 12.
        let img = decode_sixel(0, b"#0;2;100;100;100#0~-~", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 12));
        for y in 0..12 {
            assert_eq!(px(&img, 0, y), [255, 255, 255, 255], "row {y} white");
        }
    }

    #[test]
    fn raster_attrs_are_parsed_and_ignored_for_sizing() {
        // A `"1;1;99;99` raster header must NOT size the canvas; the actual drawn
        // extent (1×6) wins, and the huge declared raster is harmless.
        let img = decode_sixel(0, b"\"1;1;99;99#0;2;100;0;0#0~", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 6));
    }

    #[test]
    fn hls_blue_red_green() {
        // DEC HLS: hue 0 = BLUE, 120 = RED, 240 = GREEN (all at L=50, S=100).
        assert_eq!(hls_to_rgb(0, 50, 100), [0, 0, 255], "hue 0 → blue");
        assert_eq!(hls_to_rgb(120, 50, 100), [255, 0, 0], "hue 120 → red");
        assert_eq!(hls_to_rgb(240, 50, 100), [0, 255, 0], "hue 240 → green");
        // Saturation 0 → gray regardless of hue.
        assert_eq!(hls_to_rgb(90, 50, 0), [128, 128, 128], "s=0 → mid gray");
    }

    #[test]
    fn hls_color_register_paints() {
        // Define register 0 via HLS blue and paint a column.
        let img = decode_sixel(0, b"#0;1;0;50;100#0~", CAPS).expect("decodes");
        assert_eq!(px(&img, 0, 0), [0, 0, 255, 255], "HLS blue column");
    }

    #[test]
    fn empty_or_no_pixels_is_none() {
        assert!(decode_sixel(0, b"", CAPS).is_none(), "empty → None");
        assert!(decode_sixel(0, b"???", CAPS).is_none(), "only zero-value → None");
        assert!(decode_sixel(0, b"\"1;1;10;10", CAPS).is_none(), "raster only → None");
    }

    // ---- Adversarial / malformed inputs: must all be bounded, never panic. ----

    #[test]
    fn oversized_raster_declaration_does_not_allocate() {
        // A declared raster far beyond caps is ignored for sizing; only one
        // pixel is drawn, so the image is tiny — no giant allocation.
        let img = decode_sixel(0, b"\"1;1;999999;999999#0;2;100;0;0#0~", CAPS).expect("tiny");
        assert_eq!((img.width, img.height), (1, 6));
    }

    #[test]
    fn huge_repeat_is_rejected_not_looped() {
        // `!999999999~` would paint a billion columns → far over max_w. Must be
        // rejected (None), and must return promptly (no billion-iteration loop).
        let started = std::time::Instant::now();
        assert!(decode_sixel(0, b"#0;2;100;0;0#0!999999999~", CAPS).is_none());
        assert!(started.elapsed().as_millis() < 500, "must not loop unboundedly");
    }

    #[test]
    fn repeat_count_saturates_without_overflow() {
        // A count with more digits than u32 can hold must saturate, not wrap.
        assert!(decode_sixel(0, b"#0;2;100;0;0#0!99999999999999999999~", CAPS).is_none());
    }

    #[test]
    fn many_bands_over_height_cap_rejected() {
        // 2000 `~-` pairs = 2000 bands × 6 = 12000 rows > max_h(4096) → None.
        let mut data = Vec::new();
        data.extend_from_slice(b"#0;2;100;100;100");
        for _ in 0..2000 {
            data.extend_from_slice(b"#0~-");
        }
        assert!(decode_sixel(0, &data, CAPS).is_none());
    }

    #[test]
    fn tight_caps_reject_a_too_big_image() {
        let tiny = SixelCaps { max_w: 2, max_h: 6, max_pixels: 12 };
        // Three columns exceeds max_w = 2.
        assert!(decode_sixel(0, b"#0;2;100;0;0#0~~~", tiny).is_none());
        // Two columns is fine.
        assert!(decode_sixel(0, b"#0;2;100;0;0#0~~", tiny).is_some());
    }

    #[test]
    fn non_ascii_and_control_bytes_are_ignored() {
        // Random high bytes / controls interspersed must not desync or panic.
        let img = decode_sixel(0, b"\x80\x00#0;2;100;0;0\xff\n\r#0~\x01", CAPS).expect("decodes");
        assert_eq!((img.width, img.height), (1, 6));
        assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn truncated_color_introducer_is_safe() {
        // `#` at end of data, or a color select with no register, must not panic.
        assert!(decode_sixel(0, b"#", CAPS).is_none());
        assert!(decode_sixel(0, b"#5", CAPS).is_none()); // selects reg 5, draws nothing
        let img = decode_sixel(0, b"#0;2;100;0;0#0~#", CAPS).expect("trailing # ok");
        assert_eq!((img.width, img.height), (1, 6));
    }

    #[test]
    fn fuzz_style_random_bytes_never_panic() {
        // Deterministic LCG "fuzz": feed many pseudo-random buffers; the only
        // contract is that decode returns (Some or None) without panicking and
        // any returned image has a consistent buffer length.
        let mut state: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..300 {
            let n = (next() % 512) as usize;
            let buf: Vec<u8> = (0..n).map(|_| (next() & 0xff) as u8).collect();
            if let Some(img) = decode_sixel(next() % 3, &buf, CAPS) {
                assert_eq!(
                    img.rgba.len(),
                    (img.width as usize) * (img.height as usize) * 4,
                    "buffer length must match dims"
                );
                assert!(img.width <= CAPS.max_w && img.height <= CAPS.max_h);
            }
        }
    }

    #[test]
    fn content_id_folds_dimensions() {
        // Same pixels, different declared geometry → different ids. (Constructed
        // directly since a real decode ties pixels to dims.)
        let a = SixelImage { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        let b = SixelImage { width: 1, height: 2, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        assert_ne!(content_id(&a), content_id(&b), "dims fold into the id");
        assert_eq!(content_id(&a), content_id(&a.clone()), "stable");
    }
}
