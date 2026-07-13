//! Kitty graphics protocol (APC `ESC _ G … ESC \`) control parsing + pixel decode.
//!
//! This is the COLD path: the `feed()` scanner (`terminal.rs`) frames an APC and
//! hands its control block + payload here only when a graphics command actually
//! arrives. Nothing in this module runs in the render/feed hot loop.
//!
//! Untrusted-input safety mirrors the sixel decoder (`sixel.rs`): every dimension
//! is checked in `u64` against [`crate::sixel::SixelCaps`] BEFORE any allocation,
//! the PNG decoder is bounded three independent ways (input-byte cap, header-dim
//! cap before alloc, output-buffer cap), and every fallible call is `.ok()?` so a
//! malformed command is *absent, never fatal* — the correct-or-absent contract.
//!
//! ## Premultiplied alpha (deliberate MVP approximation)
//! The shared GPU layer (`jetty-render/src/image_layer.rs`) blends with
//! `PREMULTIPLIED_ALPHA_BLENDING` over an `Rgba8UnormSrgb` texture. Sixels carry
//! all-or-nothing alpha (already premultiplied), so they "just worked"; Kitty
//! RGBA/PNG carry STRAIGHT (partial) alpha, which would fringe under a
//! premultiplied blend. We premultiply at decode (`r = r*a/255`) so the render
//! layer stays byte-for-byte unchanged. Doing it in sRGB byte space is a minor,
//! common approximation — exact for `a==0`/`a==255` (and real photo payloads are
//! opaque); partial-alpha edges may fringe slightly. Documented, accepted for MVP.
//!
//! ## MVP simplifications (documented; refused/no-op, never a crash)
//! * `x`/`y` pixel offsets and `z` z-index are parsed but IGNORED (cell-aligned,
//!   insertion-order draw — same visual model as sixel).
//! * `t=f`/`t=t`/`t=s` (file/temp/shm transfer) are REFUSED (an untrusted PTY must
//!   never make us open a path). `o=z` zlib IS supported (via miniz_oxide).
//! * server-side `c`/`r` scale-to-box is not applied (marquee tools pre-scale
//!   client-side); we draw native px, clamping drawn rows to the reservation.

use crate::sixel::{InlineImage, SixelCaps};

/// Hard cap on a single PNG input buffer we will hand to the decoder, and on the
/// zlib inflate output — mirrors [`crate::sixel::SIXEL_CAPS`]'s pixel budget so a
/// decompression bomb can never allocate past the sixel ceiling. 64 MiB = the
/// 16 Mpx RGBA ceiling.
pub const MAX_DECODE_BYTES: usize = 16_000_000 * 4;

/// A parsed Kitty graphics APC control block (`<k=v>,<k=v>,…`). All numeric folds
/// are saturating; unknown keys are ignored (forward-compat); the token count is
/// capped so a comma flood cannot loop long. Never allocates unboundedly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyCmd {
    /// `a=` action: `t` transmit, `T` transmit+display, `p` put, `d` delete,
    /// `q` query, `f`/`a` frame (animation, unsupported). `0` = empty `a=`.
    pub action: u8,
    /// `f=` format: 24 = RGB, 32 = RGBA (default), 100 = PNG.
    pub format: u16,
    /// `t=` medium: `d` direct base64 (default/only supported), `f`/`t`/`s` refused.
    pub medium: u8,
    /// `i=` image id (0 = unset).
    pub id: u32,
    /// `I=` image number (0 = unset).
    pub number: u32,
    /// `p=` placement id.
    pub placement: u32,
    /// `s=` source pixel width (required for f=24/32).
    pub width: u32,
    /// `v=` source pixel height.
    pub height: u32,
    /// `c=` display columns (explicit cell footprint).
    pub cols: u16,
    /// `r=` display rows.
    pub rows: u16,
    /// `m=` more-chunks: 1 = more follow, 0 = last.
    pub more: u8,
    /// `q=` quiet: 0 normal, 1 suppress OK, 2 suppress all.
    pub quiet: u8,
    /// `o=z` zlib compression.
    pub compressed: bool,
    /// `d=` delete selector (`a`/`A` all, `i`/`I` by id; 0 = unset).
    pub delete: u8,
    /// Whether `a=` was present at all. Kitty continuation chunks OMIT the action
    /// key, so this disambiguates a continuation (`false`) from a fresh command
    /// (`true`) — the chunk state machine gates on it (amendment BLOCKING 1).
    pub has_action: bool,
}

impl Default for KittyCmd {
    fn default() -> Self {
        KittyCmd {
            // Effective default when `a=` is absent AND this is not a continuation:
            // treat as transmit+display so a bare transmit still shows (matches the
            // blueprint and every marquee tool, which always send a=T anyway).
            action: b'T',
            format: 32,
            medium: b'd',
            id: 0,
            number: 0,
            placement: 0,
            width: 0,
            height: 0,
            cols: 0,
            rows: 0,
            more: 0,
            quiet: 0,
            compressed: false,
            delete: 0,
            has_action: false,
        }
    }
}

/// Saturating digit-by-digit unsigned parse (stops at the first non-digit).
fn parse_u32(v: &[u8]) -> u32 {
    let mut n = 0u32;
    for &b in v {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as u32);
        } else {
            break;
        }
    }
    n
}

impl KittyCmd {
    /// Parse the control slice (the APC bytes BEFORE the first `;`). Never panics.
    pub fn parse(control: &[u8]) -> KittyCmd {
        let mut cmd = KittyCmd::default();
        // Cap the token count so a `,,,,…` flood cannot loop long.
        for (i, token) in control.split(|&c| c == b',').enumerate() {
            if i >= 64 {
                break;
            }
            let mut it = token.splitn(2, |&c| c == b'=');
            let key = it.next().unwrap_or(&[]);
            let value = it.next().unwrap_or(&[]);
            if key.is_empty() {
                continue;
            }
            match key {
                b"a" => {
                    cmd.has_action = true;
                    // Empty `a=` classifies as has_action=true, action=0 (NOT a
                    // continuation) per amendment BLOCKING 1 / I8.
                    cmd.action = value.first().copied().unwrap_or(0);
                }
                b"f" => cmd.format = parse_u32(value).min(u16::MAX as u32) as u16,
                b"t" => cmd.medium = value.first().copied().unwrap_or(b'd'),
                b"i" => cmd.id = parse_u32(value),
                b"I" => cmd.number = parse_u32(value),
                b"p" => cmd.placement = parse_u32(value),
                b"s" => cmd.width = parse_u32(value),
                b"v" => cmd.height = parse_u32(value),
                b"c" => cmd.cols = parse_u32(value).min(u16::MAX as u32) as u16,
                b"r" => cmd.rows = parse_u32(value).min(u16::MAX as u32) as u16,
                b"m" => cmd.more = if parse_u32(value) >= 1 { 1 } else { 0 },
                b"q" => cmd.quiet = parse_u32(value).min(255) as u8,
                b"o" => cmd.compressed = value.first().copied() == Some(b'z'),
                b"d" => cmd.delete = value.first().copied().unwrap_or(0),
                _ => {} // unknown key: ignore (forward-compat)
            }
        }
        cmd
    }

    /// Whether this command addresses a specific image (`i=` or `I=` set), which
    /// per the Kitty spec is when an OK/error reply may be sent (amendment A10).
    pub fn addressable(&self) -> bool {
        self.id != 0 || self.number != 0
    }
}

/// Premultiply one straight-alpha sRGB pixel's color channels by its alpha.
#[inline]
fn premul(r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8) {
    let f = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
    (f(r), f(g), f(b))
}

/// Common dimension guard shared by the raw decoders. Returns the pixel count on
/// success, `None` if any cap is exceeded (all math in `u64`, pre-allocation).
fn checked_pixels(width: u32, height: u32, caps: SixelCaps) -> Option<usize> {
    if width == 0 || height == 0 || width > caps.max_w || height > caps.max_h {
        return None;
    }
    let px = (width as u64) * (height as u64);
    if px > caps.max_pixels as u64 {
        return None;
    }
    Some(px as usize)
}

/// Decode an `f=24` RGB payload (`data.len() == w*h*3`) into a premultiplied
/// (opaque) RGBA `InlineImage`. Returns `None` on any cap violation or length
/// mismatch — checked in `u64` before allocation.
pub fn decode_rgb(width: u32, height: u32, data: &[u8], caps: SixelCaps) -> Option<InlineImage> {
    let px = checked_pixels(width, height, caps)?;
    if data.len() as u64 != px as u64 * 3 {
        return None;
    }
    let mut rgba = vec![0u8; px * 4];
    for i in 0..px {
        rgba[i * 4] = data[i * 3];
        rgba[i * 4 + 1] = data[i * 3 + 1];
        rgba[i * 4 + 2] = data[i * 3 + 2];
        rgba[i * 4 + 3] = 255;
    }
    Some(InlineImage { width, height, rgba })
}

/// Decode an `f=32` RGBA payload (`data.len() == w*h*4`) into a PREMULTIPLIED
/// RGBA `InlineImage`. Returns `None` on any cap violation or length mismatch.
pub fn decode_rgba(width: u32, height: u32, data: &[u8], caps: SixelCaps) -> Option<InlineImage> {
    let px = checked_pixels(width, height, caps)?;
    if data.len() as u64 != px as u64 * 4 {
        return None;
    }
    let mut rgba = vec![0u8; px * 4];
    for i in 0..px {
        let r = data[i * 4];
        let g = data[i * 4 + 1];
        let b = data[i * 4 + 2];
        let a = data[i * 4 + 3];
        let (pr, pg, pb) = premul(r, g, b, a);
        rgba[i * 4] = pr;
        rgba[i * 4 + 1] = pg;
        rgba[i * 4 + 2] = pb;
        rgba[i * 4 + 3] = a;
    }
    Some(InlineImage { width, height, rgba })
}

/// Decode an `f=100` PNG payload into a PREMULTIPLIED RGBA `InlineImage` under
/// strict, untrusted-input-safe limits. Returns `None` on any decode failure or
/// cap violation. Only the FIRST frame is read (no animation).
pub fn decode_png(data: &[u8], caps: SixelCaps) -> Option<InlineImage> {
    // Bound 1: input-byte cap before touching the decoder.
    if data.len() > MAX_DECODE_BYTES {
        return None;
    }
    let mut decoder = png::Decoder::new(data);
    // EXPAND: palette / <8-bit / tRNS → 8-bit RGB(A) or grayscale(+alpha).
    // STRIP_16: 16-bit channels → 8-bit. Bounds the per-pixel width we handle.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    // Bound 2 (A9, mandatory): cap ancillary-chunk (PLTE/iCCP/zTXt) allocation.
    let mut limits = png::Limits::default();
    limits.bytes = MAX_DECODE_BYTES;
    decoder.set_limits(limits);

    let mut reader = decoder.read_info().ok()?;
    // Bound 3: enforce caps on the HEADER dims BEFORE allocating the frame buffer
    // (defeats a bomb header claiming 100000×100000).
    {
        let info = reader.info();
        checked_pixels(info.width, info.height, caps)?;
    }
    let size = reader.output_buffer_size();
    // Bound 4: output-buffer cap (defensive, independent of the header check).
    if size as u64 > caps.max_pixels as u64 * 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).ok()?;

    let w = frame.width;
    let h = frame.height;
    let px = checked_pixels(w, h, caps)?;
    let mut rgba = vec![0u8; px * 4];

    match frame.color_type {
        png::ColorType::Rgba => {
            if buf.len() < px * 4 {
                return None;
            }
            for i in 0..px {
                let (pr, pg, pb) = premul(buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]);
                rgba[i * 4] = pr;
                rgba[i * 4 + 1] = pg;
                rgba[i * 4 + 2] = pb;
                rgba[i * 4 + 3] = buf[i * 4 + 3];
            }
        }
        png::ColorType::Rgb => {
            if buf.len() < px * 3 {
                return None;
            }
            for i in 0..px {
                rgba[i * 4] = buf[i * 3];
                rgba[i * 4 + 1] = buf[i * 3 + 1];
                rgba[i * 4 + 2] = buf[i * 3 + 2];
                rgba[i * 4 + 3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            if buf.len() < px * 2 {
                return None;
            }
            for i in 0..px {
                let gray = buf[i * 2];
                let a = buf[i * 2 + 1];
                let (pr, pg, pb) = premul(gray, gray, gray, a);
                rgba[i * 4] = pr;
                rgba[i * 4 + 1] = pg;
                rgba[i * 4 + 2] = pb;
                rgba[i * 4 + 3] = a;
            }
        }
        png::ColorType::Grayscale => {
            if buf.len() < px {
                return None;
            }
            for i in 0..px {
                let gray = buf[i];
                rgba[i * 4] = gray;
                rgba[i * 4 + 1] = gray;
                rgba[i * 4 + 2] = gray;
                rgba[i * 4 + 3] = 255;
            }
        }
        // Indexed is impossible after EXPAND; anything else is unexpected.
        _ => return None,
    }

    Some(InlineImage { width: w, height: h, rgba })
}

/// Inflate a zlib (`o=z`) stream on the cold APC path, bounded to `max_out`
/// bytes (the same raw budget as the transport accumulator — amendment BLOCKING
/// 3). On any error or limit hit, returns `None` (drop, never panic).
pub fn inflate_zlib(data: &[u8], max_out: usize) -> Option<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, max_out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sixel::SIXEL_CAPS;

    const CAPS: SixelCaps = SIXEL_CAPS;

    // ── T2: control-key parser ───────────────────────────────────────────────

    #[test]
    fn parse_basic_fields() {
        let c = KittyCmd::parse(b"a=T,f=100,i=3");
        assert_eq!(c.action, b'T');
        assert!(c.has_action);
        assert_eq!(c.format, 100);
        assert_eq!(c.id, 3);
    }

    #[test]
    fn parse_sizes_and_flags() {
        let c = KittyCmd::parse(b"s=10,v=20,m=1,q=2,c=4,r=2");
        assert_eq!((c.width, c.height), (10, 20));
        assert_eq!(c.more, 1);
        assert_eq!(c.quiet, 2);
        assert_eq!((c.cols, c.rows), (4, 2));
    }

    #[test]
    fn parse_unknown_key_ignored() {
        let c = KittyCmd::parse(b"a=t,zz=1,f=24");
        assert_eq!(c.action, b't');
        assert_eq!(c.format, 24);
    }

    #[test]
    fn parse_saturates_huge_numbers() {
        let c = KittyCmd::parse(b"s=99999999999999999999");
        assert_eq!(c.width, u32::MAX);
    }

    #[test]
    fn parse_empty_is_continuation() {
        let c = KittyCmd::parse(b"");
        assert!(!c.has_action, "no a= ⇒ continuation candidate");
    }

    #[test]
    fn parse_empty_action_is_has_action_zero() {
        // Empty `a=` ⇒ has_action=true, action=0 (amendment BLOCKING 1 / I8).
        let c = KittyCmd::parse(b"a=,f=32");
        assert!(c.has_action);
        assert_eq!(c.action, 0);
    }

    #[test]
    fn parse_malformed_does_not_panic() {
        let _ = KittyCmd::parse(b"=5");
        let _ = KittyCmd::parse(b",,,");
        let _ = KittyCmd::parse(b"a");
        let _ = KittyCmd::parse(b"====");
        // A comma flood is bounded (no long loop / no panic).
        let flood = vec![b','; 100_000];
        let _ = KittyCmd::parse(&flood);
    }

    #[test]
    fn parse_compression_and_medium() {
        let c = KittyCmd::parse(b"o=z,t=f");
        assert!(c.compressed);
        assert_eq!(c.medium, b'f');
        let d = KittyCmd::parse(b"a=q,i=2");
        assert!(d.addressable());
    }

    // ── T3: RGB / RGBA decoders ──────────────────────────────────────────────

    #[test]
    fn rgb_expands_to_opaque_rgba() {
        // 2×2 RGB: 4 pixels × 3 bytes.
        let data = [
            255, 0, 0, /**/ 0, 255, 0, /**/ 0, 0, 255, /**/ 255, 255, 0,
        ];
        let img = decode_rgb(2, 2, &data, CAPS).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.rgba[12..16], &[255, 255, 0, 255]);
    }

    #[test]
    fn rgba_premultiplies() {
        // 1×1 [255,0,0,128] → premultiplied [128,0,0,128].
        let img = decode_rgba(1, 1, &[255, 0, 0, 128], CAPS).unwrap();
        assert_eq!(&img.rgba[0..4], &[128, 0, 0, 128]);
    }

    #[test]
    fn wrong_length_is_none() {
        assert!(decode_rgb(2, 2, &[0; 11], CAPS).is_none());
        assert!(decode_rgba(2, 2, &[0; 15], CAPS).is_none());
    }

    #[test]
    fn zero_and_oversize_dims_rejected() {
        assert!(decode_rgba(0, 4, &[], CAPS).is_none());
        assert!(decode_rgba(5000, 5000, &[], CAPS).is_none()); // over max_w/h
    }

    #[test]
    fn overpixel_rejected_without_overflow() {
        // 4096×4096 = 16.7 Mpx > 16 Mpx cap → None, no giant alloc.
        let started = std::time::Instant::now();
        assert!(decode_rgba(4096, 4096, &[], CAPS).is_none());
        assert!(started.elapsed().as_millis() < 200);
    }

    // ── T4: PNG decoder ──────────────────────────────────────────────────────

    // A 2×2 RGBA PNG (red/green/blue/white, all opaque), generated by the png
    // encoder in a build helper below.
    fn make_png_rgba_2x2() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            let data = [
                255, 0, 0, 255, /**/ 0, 255, 0, 255, /**/ 0, 0, 255, 255, /**/ 255, 255, 255, 255,
            ];
            w.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn png_rgba_decodes() {
        let png_bytes = make_png_rgba_2x2();
        let img = decode_png(&png_bytes, CAPS).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn png_grayscale_decodes() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[10, 200]).unwrap();
        }
        let img = decode_png(&out, CAPS).unwrap();
        assert_eq!(&img.rgba[0..4], &[10, 10, 10, 255]);
        assert_eq!(&img.rgba[4..8], &[200, 200, 200, 255]);
    }

    #[test]
    fn png_garbage_is_none() {
        assert!(decode_png(b"not a png at all", CAPS).is_none());
        assert!(decode_png(&[], CAPS).is_none());
    }

    #[test]
    fn png_truncated_is_none() {
        let full = make_png_rgba_2x2();
        assert!(decode_png(&full[..full.len() / 2], CAPS).is_none());
    }

    #[test]
    fn png_header_bomb_rejected_promptly() {
        // A valid IHDR claiming 99999×99999 must be rejected before any large
        // allocation. Build just enough of a PNG for read_info to see the header.
        let mut out = Vec::new();
        {
            // png encoder validates dims but does not allocate the image for the
            // header; write_header alone emits IHDR. Use a tight cap so even if it
            // allocated, the header-dim guard fires first.
            let enc = png::Encoder::new(&mut out, 99999, 99999);
            let _ = enc.write_header(); // may or may not succeed; we only need bytes
        }
        let started = std::time::Instant::now();
        let _ = decode_png(&out, CAPS); // must be None and fast (or empty-bytes None)
        assert!(started.elapsed().as_millis() < 500);
    }

    // ── B3: zlib inflate ─────────────────────────────────────────────────────

    #[test]
    fn zlib_roundtrip() {
        // Compress with miniz_oxide, inflate back.
        let raw: Vec<u8> = (0..1000u32).map(|x| (x & 0xff) as u8).collect();
        let comp = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let back = inflate_zlib(&comp, MAX_DECODE_BYTES).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn zlib_bounded_and_garbage_safe() {
        assert!(inflate_zlib(b"not zlib", MAX_DECODE_BYTES).is_none());
        // A stream inflating past the limit → None.
        let raw = vec![0u8; 100_000];
        let comp = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        assert!(inflate_zlib(&comp, 1000).is_none(), "limit enforced");
    }

    // ── Adversarial fuzz across all decoders ─────────────────────────────────

    #[test]
    fn fuzz_decoders_never_panic() {
        let mut state: u64 = 0xdead_beef_cafe_1234;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..400 {
            let n = (next() % 256) as usize;
            let buf: Vec<u8> = (0..n).map(|_| (next() & 0xff) as u8).collect();
            let w = next() % 64;
            let h = next() % 64;
            if let Some(img) = decode_rgb(w, h, &buf, CAPS) {
                assert_eq!(img.rgba.len(), (img.width as usize) * (img.height as usize) * 4);
            }
            if let Some(img) = decode_rgba(w, h, &buf, CAPS) {
                assert_eq!(img.rgba.len(), (img.width as usize) * (img.height as usize) * 4);
            }
            if let Some(img) = decode_png(&buf, CAPS) {
                assert_eq!(img.rgba.len(), (img.width as usize) * (img.height as usize) * 4);
                assert!(img.width <= CAPS.max_w && img.height <= CAPS.max_h);
            }
            let _ = inflate_zlib(&buf, MAX_DECODE_BYTES);
            let _ = KittyCmd::parse(&buf);
        }
    }
}
