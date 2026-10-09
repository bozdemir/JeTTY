//! Backdrop image loading — PNG / JPEG decode, downscale, frosted blur, CPU
//! mipmaps and the luminance percentiles smart dim reads. Pure CPU code with
//! no GPU or window types, run ONCE per image on a worker thread (the app's
//! `AppEvent` wakes the UI when it is done); see `backdrop.rs` for the GPU side.
//!
//! Untrusted-input safe: the file size, the header dimensions (≤ 8192 per side)
//! and the decoders' own allocations are capped BEFORE the pixel buffer is
//! allocated, and a decoder panic is caught and reported as an error. A broken
//! or missing file is an `Err` with a short human-readable reason — the app
//! shows it in a notice and keeps the base gradient; it never crashes.
//!
//! Color: every resample / blur / mip step averages in LINEAR light (sRGB →
//! linear via a table, back via a 16K-entry table), on premultiplied alpha, so
//! downscaled photos keep their brightness and transparent edges never fringe.
//! The output texels are sRGB-encoded premultiplied RGBA8, uploaded as
//! `Rgba8UnormSrgb` (the sampler linearizes them for free).

use std::path::Path;
use std::sync::OnceLock;

/// Largest accepted image side (px). "8K": larger images are refused rather
/// than decoded (the decode alone would need hundreds of MB).
pub const MAX_IMAGE_DIM: u32 = 8192;

/// Largest accepted file (bytes). An 8192² PNG of noise is ~200 MB, but any
/// real wallpaper is far below this.
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Allocation cap handed to the PNG decoder for its ancillary chunks and line
/// buffers (the frame buffer itself is bounded by `MAX_IMAGE_DIM`).
const PNG_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// A frosted (blurred) image is stored at this fraction of its laid-out size
/// per axis: a blur hides the detail anyway, and the GPU's bilinear upscale of
/// a blurred image is smooth.
const BLUR_DOWNSCALE: u32 = 4;

/// One mip level: `w`×`h` sRGB-encoded premultiplied RGBA8, rows tight.
#[derive(Clone, PartialEq, Eq)]
pub struct MipLevel {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// A decoded backdrop image, ready to upload.
#[derive(Clone)]
pub struct DecodedImage {
    /// The size the fit math lays out (after the monitor downscale, before the
    /// frosted downscale): `center` / `tile` draw it at this many pixels.
    pub layout_w: u32,
    pub layout_h: u32,
    /// Level 0 first, each half the previous (rounded up) down to 1×1.
    pub mips: Vec<MipLevel>,
    /// Relative luminance (0..1, linear) of the 5th / 95th percentile of the
    /// displayed image — smart dim keeps text readable against these ends.
    pub lum_p5: f32,
    pub lum_p95: f32,
    /// Whether the image was blurred (drawn with the frosted tint + grain).
    pub blurred: bool,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.mips.first().map_or((0, 0), |m| (m.w, m.h));
        f.debug_struct("DecodedImage")
            .field("layout", &(self.layout_w, self.layout_h))
            .field("texture", &(w, h))
            .field("mips", &self.mips.len())
            .field("lum_p5", &self.lum_p5)
            .field("lum_p95", &self.lum_p95)
            .field("blurred", &self.blurred)
            .finish()
    }
}

// ── Color tables ─────────────────────────────────────────────────────────────

/// sRGB code (0..=255) → linear light.
fn to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            *v = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
        }
        t
    })
}

const ENC_LUT_LEN: usize = 16384;

/// Linear light (sampled at `ENC_LUT_LEN` steps over 0..=1) → sRGB code. Fine
/// enough that the rounding error stays under half a code step near black,
/// where the sRGB curve is steepest.
fn to_srgb_lut() -> &'static [u8; ENC_LUT_LEN] {
    static LUT: OnceLock<Box<[u8; ENC_LUT_LEN]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = Box::new([0u8; ENC_LUT_LEN]);
        for (i, v) in t.iter_mut().enumerate() {
            let l = i as f32 / (ENC_LUT_LEN - 1) as f32;
            let s = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
            *v = (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        t
    })
}

/// Linear light → sRGB code (table lookup, clamped).
#[inline]
fn encode(l: f32) -> u8 {
    let i = (l.clamp(0.0, 1.0) * (ENC_LUT_LEN - 1) as f32 + 0.5) as usize;
    to_srgb_lut()[i.min(ENC_LUT_LEN - 1)]
}

#[inline]
fn alpha_code(a: f32) -> u8 {
    (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

// ── Decode ───────────────────────────────────────────────────────────────────

/// What kind of file `head` starts like (magic bytes, never the extension).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
}

/// Sniff PNG / JPEG from the first bytes.
pub fn sniff(head: &[u8]) -> Option<ImageKind> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageKind::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else {
        None
    }
}

/// A decoded frame: straight (non-premultiplied) sRGB RGBA8.
pub struct RawImage {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RawImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawImage({}×{}, {} bytes)", self.w, self.h, self.rgba.len())
    }
}

fn check_dims(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("the image is empty".into());
    }
    if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
        return Err(format!("{w}×{h} is too large (max {MAX_IMAGE_DIM}×{MAX_IMAGE_DIM})"));
    }
    Ok(())
}

/// Decode PNG or JPEG bytes into straight RGBA8, upright: a photo's EXIF
/// orientation (a phone stores portrait shots turned) is applied. Dimensions
/// are checked from the header before the frame buffer is allocated.
#[cfg(test)]
fn decode_bytes(data: &[u8]) -> Result<RawImage, String> {
    decode_stored(data).map(|(raw, o)| orient(raw, o))
}

/// The EXIF Orientation (tag 0x0112) of a TIFF-structured EXIF block — what a
/// JPEG's APP1 "Exif" segment and a PNG's eXIf chunk carry (a leading
/// "Exif\0\0" is skipped): 1..=8, 1 = as stored. `None` when absent or
/// malformed. Reads IFD0 only; every access is bounds-checked (the bytes come
/// from an untrusted file).
pub fn exif_orientation(exif: &[u8]) -> Option<u8> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let le = match tiff.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let bytes = |at: usize, n: usize| tiff.get(at..at.checked_add(n)?);
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = bytes(at, 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = bytes(at, 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    for i in 0..usize::from(u16_at(ifd)?) {
        let entry = ifd.checked_add(2 + i * 12)?;
        if u16_at(entry)? != 0x0112 {
            continue;
        }
        // One SHORT: the value sits in the first two bytes of the value field.
        if u16_at(entry + 2)? != 3 {
            return None;
        }
        return u8::try_from(u16_at(entry + 8)?).ok().filter(|o| (1..=8).contains(o));
    }
    None
}

/// `raw` turned upright for EXIF orientation `o`: 2 / 4 mirror it, 3 turns it
/// 180°, 6 / 8 turn it 90° clockwise / counter-clockwise, 5 / 7 transpose it
/// across either diagonal (5..=8 swap width and height). Any other value — and
/// a buffer that does not match its size — leaves it as stored.
pub fn orient(raw: RawImage, o: u8) -> RawImage {
    let (w, h) = (raw.w as usize, raw.h as usize);
    if !(2..=8).contains(&o) || raw.rgba.len() != w * h * 4 {
        return raw;
    }
    let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; raw.rgba.len()];
    for y in 0..oh {
        for x in 0..ow {
            // The stored pixel shown at (x, y).
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let (s, d) = ((sy * w + sx) * 4, (y * ow + x) * 4);
            out[d..d + 4].copy_from_slice(&raw.rgba[s..s + 4]);
        }
    }
    RawImage { w: ow as u32, h: oh as u32, rgba: out }
}

/// Decode PNG or JPEG bytes into straight RGBA8 in the file's STORED
/// orientation, plus the EXIF orientation that shows it upright (1 = none).
fn decode_stored(data: &[u8]) -> Result<(RawImage, u8), String> {
    match sniff(data) {
        Some(ImageKind::Png) => decode_png(data),
        Some(ImageKind::Jpeg) => decode_jpeg(data),
        None => Err("not a PNG or JPEG file".into()),
    }
}

/// A decoder's error as ONE short line ("bad JPEG (…)"): the reason feeds a
/// notice pill, and some decoders' messages span lines.
fn decode_error(kind: &str, detail: &str) -> String {
    let mut d: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if d.chars().count() > 80 {
        d = d.chars().take(79).collect::<String>() + "…";
    }
    format!("bad {kind} ({d})")
}

fn decode_png(data: &[u8]) -> Result<(RawImage, u8), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits { bytes: PNG_LIMIT_BYTES });
    let mut reader = decoder.read_info().map_err(|e| decode_error("PNG", &e.to_string()))?;
    {
        let info = reader.info();
        check_dims(info.width, info.height)?;
    }
    let size = reader.output_buffer_size().ok_or("bad PNG (frame size)")?;
    // Defensive: never more than 4 bytes per pixel after EXPAND | STRIP_16
    // (8-bit RGBA at most).
    let (iw, ih) = (reader.info().width as usize, reader.info().height as usize);
    let full = iw * ih * 4;
    if size > full {
        return Err("bad PNG (frame size)".into());
    }
    // ONE buffer, allocated at the final RGBA size: the frame decodes into its
    // front and is widened to RGBA in place — never a second full-size copy
    // (an 8192² RGB wallpaper peaked at ~470 MB with one).
    let mut buf = Vec::with_capacity(full);
    buf.resize(size, 0u8);
    let frame = reader.next_frame(&mut buf).map_err(|e| decode_error("PNG", &e.to_string()))?;
    let (w, h) = (frame.width, frame.height);
    check_dims(w, h)?;
    let px = w as usize * h as usize;
    let n = match frame.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        // Indexed is expanded by EXPAND; anything else is unexpected.
        _ => return Err("unsupported PNG color type".into()),
    };
    if buf.len() < px * n || px * 4 > full {
        return Err("bad PNG (short frame)".into());
    }
    let rgba = widen_to_rgba(buf, px, n);
    // An eXIf chunk (before the image data) may say how to show it.
    let orientation = reader.info().exif_metadata.as_deref().and_then(exif_orientation).unwrap_or(1);
    Ok((RawImage { w, h, rgba }, orientation))
}

/// `buf`'s first `px` pixels of `n` bytes each (RGBA 4, RGB 3, gray + alpha
/// 2, gray 1) widened IN PLACE to `px` straight RGBA8 pixels. Back to front:
/// pixel i's RGBA lands at `4i ≥ n·i`, past every source byte still unread.
fn widen_to_rgba(mut buf: Vec<u8>, px: usize, n: usize) -> Vec<u8> {
    buf.resize(px * 4, 0);
    if n == 4 {
        return buf;
    }
    for i in (0..px).rev() {
        let s = i * n;
        let p = match n {
            3 => [buf[s], buf[s + 1], buf[s + 2], 255],
            2 => [buf[s], buf[s], buf[s], buf[s + 1]],
            _ => [buf[s], buf[s], buf[s], 255],
        };
        buf[i * 4..i * 4 + 4].copy_from_slice(&p);
    }
    buf
}

fn decode_jpeg(data: &[u8]) -> Result<(RawImage, u8), String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_IMAGE_DIM as usize)
        .set_max_height(MAX_IMAGE_DIM as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder.decode_headers().map_err(|e| decode_error("JPEG", &format!("{e:?}")))?;
    let info = decoder.info().ok_or("bad JPEG (no header)")?;
    let (w, h) = (info.width as u32, info.height as u32);
    check_dims(w, h)?;
    // A camera / phone photo's APP1 EXIF says how to show it (portrait shots
    // are stored turned).
    let orientation = decoder.exif().and_then(|e| exif_orientation(e)).unwrap_or(1);
    let pixels = decoder.decode().map_err(|e| decode_error("JPEG", &format!("{e:?}")))?;
    let px = w as usize * h as usize;
    if pixels.len() != px * 4 {
        return Err("bad JPEG (unexpected output size)".into());
    }
    Ok((RawImage { w, h, rgba: pixels }, orientation))
}

// ── Premultiply / resample / blur ────────────────────────────────────────────

/// Premultiply straight sRGB RGBA8 by alpha IN LINEAR LIGHT (opaque images are
/// left untouched — the common case costs one scan).
pub fn premultiply(img: &mut RawImage) {
    if img.rgba.chunks_exact(4).all(|p| p[3] == 255) {
        return;
    }
    let lut = to_linear_lut();
    for p in img.rgba.chunks_exact_mut(4) {
        let a = p[3];
        if a == 255 {
            continue;
        }
        let af = a as f32 / 255.0;
        for c in &mut p[..3] {
            *c = encode(lut[*c as usize] * af);
        }
    }
}

/// Area (box) weights mapping `src` samples onto `dst ≤ src` samples: for each
/// destination index the contributing `(source index, weight)` pairs, the
/// weights summing to 1 (exact fractional coverage at both ends).
pub fn area_weights(src: u32, dst: u32) -> Vec<Vec<(u32, f32)>> {
    let scale = src as f64 / dst.max(1) as f64;
    (0..dst)
        .map(|d| {
            let a = d as f64 * scale;
            let b = ((d + 1) as f64 * scale).min(src as f64);
            let mut v = Vec::with_capacity(scale.ceil() as usize + 1);
            let mut s = a.floor() as u32;
            while (s as f64) < b && s < src {
                let lo = (s as f64).max(a);
                let hi = ((s + 1) as f64).min(b);
                let w = ((hi - lo) / (b - a)) as f32;
                if w > 0.0 {
                    v.push((s, w));
                }
                s += 1;
            }
            v
        })
        .collect()
}

/// Downscale premultiplied sRGB RGBA8 `src` (`sw`×`sh`) to `dw`×`dh` (each ≤
/// the source side) with an exact area filter in linear light. Streams the
/// source one row at a time: memory is the output plus two scratch rows.
pub fn resize_area(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (dw, dh) = (dw.clamp(1, sw.max(1)), dh.clamp(1, sh.max(1)));
    let lut = to_linear_lut();
    let hw = area_weights(sw, dw);
    let vw = area_weights(sh, dh);
    let row_len = dw as usize * 4;
    let mut out = vec![0u8; row_len * dh as usize];
    let mut acc = vec![0.0f32; row_len];
    // The last horizontally-resampled source row: a source row straddling two
    // destination rows is resampled once.
    let mut cached: Option<(u32, Vec<f32>)> = None;
    let hrow = |sy: u32, cached: &mut Option<(u32, Vec<f32>)>| {
        if cached.as_ref().is_some_and(|(r, _)| *r == sy) {
            return;
        }
        let mut row = cached.take().map(|(_, r)| r).unwrap_or_else(|| vec![0.0f32; row_len]);
        let base = sy as usize * sw as usize * 4;
        for (dx, taps) in hw.iter().enumerate() {
            let mut px = [0.0f32; 4];
            for &(sx, w) in taps {
                let i = base + sx as usize * 4;
                px[0] += lut[src[i] as usize] * w;
                px[1] += lut[src[i + 1] as usize] * w;
                px[2] += lut[src[i + 2] as usize] * w;
                px[3] += src[i + 3] as f32 / 255.0 * w;
            }
            row[dx * 4..dx * 4 + 4].copy_from_slice(&px);
        }
        *cached = Some((sy, row));
    };
    for (dy, taps) in vw.iter().enumerate() {
        acc.iter_mut().for_each(|v| *v = 0.0);
        for &(sy, w) in taps {
            hrow(sy, &mut cached);
            let row = &cached.as_ref().expect("row computed above").1;
            for (a, r) in acc.iter_mut().zip(row) {
                *a += r * w;
            }
        }
        let o = &mut out[dy * row_len..(dy + 1) * row_len];
        for (px, a) in o.chunks_exact_mut(4).zip(acc.chunks_exact(4)) {
            px[0] = encode(a[0]);
            px[1] = encode(a[1]);
            px[2] = encode(a[2]);
            px[3] = alpha_code(a[3]);
        }
    }
    out
}

/// One horizontal box-blur pass of radius `r` over linear RGBA f32 rows
/// (clamped edges, sliding-window sum: O(w) per row whatever the radius).
fn box_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut dst[y * w * 4..(y + 1) * w * 4];
        let at = |x: isize| -> &[f32] {
            let x = x.clamp(0, w as isize - 1) as usize;
            &row[x * 4..x * 4 + 4]
        };
        let mut sum = [0.0f32; 4];
        for k in -(r as isize)..=(r as isize) {
            let p = at(k);
            for c in 0..4 {
                sum[c] += p[c];
            }
        }
        for x in 0..w {
            for c in 0..4 {
                out[x * 4 + c] = sum[c] * norm;
            }
            let add = at(x as isize + r as isize + 1);
            let sub = at(x as isize - r as isize);
            for c in 0..4 {
                sum[c] += add[c] - sub[c];
            }
        }
    }
}

/// Transpose a `w`×`h` RGBA f32 image (so the vertical pass reuses `box_h`).
fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s = (y * w + x) * 4;
            let d = (x * h + y) * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

/// Three box blurs (≈ a Gaussian of sigma ≈ `radius`) over premultiplied sRGB
/// RGBA8, in linear light. `radius` 0 returns the input unchanged.
pub fn blur3(rgba: &[u8], w: u32, h: u32, radius: u32) -> Vec<u8> {
    if radius == 0 || w == 0 || h == 0 {
        return rgba.to_vec();
    }
    let (w, h) = (w as usize, h as usize);
    let lut = to_linear_lut();
    let mut a: Vec<f32> = rgba
        .chunks_exact(4)
        .flat_map(|p| [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize], p[3] as f32 / 255.0])
        .collect();
    let mut b = vec![0.0f32; a.len()];
    let r = radius as usize;
    for _ in 0..3 {
        box_h(&a, &mut b, w, h, r);
        let t = transpose(&b, w, h);
        let mut t2 = vec![0.0f32; t.len()];
        box_h(&t, &mut t2, h, w, r);
        a = transpose(&t2, h, w);
    }
    a.chunks_exact(4)
        .flat_map(|p| [encode(p[0]), encode(p[1]), encode(p[2]), alpha_code(p[3])])
        .collect()
}

/// The full mip chain of `level0`, sized exactly as the GPU expects: level `k`
/// is `max(1, w >> k)` × `max(1, h >> k)`, down to 1×1 (so the level count is
/// `floor(log2(max(w, h))) + 1`). Each level is an exact area downscale of the
/// previous in linear light — an odd edge column/row is shared, not dropped.
pub fn build_mips(level0: MipLevel) -> Vec<MipLevel> {
    let mut mips = vec![level0];
    loop {
        let prev = mips.last().expect("level 0 present");
        if prev.w <= 1 && prev.h <= 1 {
            break;
        }
        let (nw, nh) = ((prev.w / 2).max(1), (prev.h / 2).max(1));
        let rgba = resize_area(&prev.rgba, prev.w, prev.h, nw, nh);
        mips.push(MipLevel { w: nw, h: nh, rgba });
    }
    mips
}

/// The 5th and 95th percentile relative luminance (linear 0..1) of a
/// premultiplied sRGB RGBA8 image, from a strided sample of at most ~256K
/// pixels into a 1024-bin histogram.
pub fn luminance_percentiles(rgba: &[u8], w: u32, h: u32) -> (f32, f32) {
    const BINS: usize = 1024;
    let lut = to_linear_lut();
    let px = w as usize * h as usize;
    if px == 0 {
        return (0.0, 0.0);
    }
    let step = ((px / 262_144) as f64).sqrt().ceil().max(1.0) as usize;
    let mut hist = [0u32; BINS];
    let mut n = 0u32;
    for y in (0..h as usize).step_by(step) {
        for x in (0..w as usize).step_by(step) {
            let i = (y * w as usize + x) * 4;
            let l = 0.2126 * lut[rgba[i] as usize]
                + 0.7152 * lut[rgba[i + 1] as usize]
                + 0.0722 * lut[rgba[i + 2] as usize];
            hist[((l.clamp(0.0, 1.0) * (BINS - 1) as f32) + 0.5) as usize] += 1;
            n += 1;
        }
    }
    let pick = |q: f32| -> f32 {
        let target = (q * n as f32).ceil().max(1.0) as u32;
        let mut seen = 0u32;
        for (b, &c) in hist.iter().enumerate() {
            seen += c;
            if seen >= target {
                return b as f32 / (BINS - 1) as f32;
            }
        }
        1.0
    };
    (pick(0.05), pick(0.95))
}

/// The size an image is laid out at: scaled down (never up) just enough to
/// COVER a `max_w`×`max_h` monitor — `cover` on a full-screen window then maps
/// it 1:1 on its shorter side and never upsamples a large photo.
pub fn layout_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 || max_w == 0 || max_h == 0 {
        return (w.max(1), h.max(1));
    }
    let s = (max_w as f64 / w as f64).max(max_h as f64 / h as f64).min(1.0);
    (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

/// Blur radius (px, at the ¼-res frosted size) for a `blur` of 0..1.
pub fn blur_radius(blur: f32) -> u32 {
    (blur.clamp(0.0, 1.0) * 24.0).round() as u32
}

/// Turn a decoded (upright) frame into an upload-ready [`DecodedImage`]:
/// premultiply, downscale to cover `max_w`×`max_h`, optionally blur (at ¼
/// size), build the mips and measure the luminance percentiles.
pub fn prepare(raw: RawImage, max_w: u32, max_h: u32, blur: f32) -> DecodedImage {
    prepare_oriented((raw, 1), max_w, max_h, blur)
}

/// [`prepare`] for a frame in its stored orientation plus the EXIF
/// orientation that shows it upright: laid out upright, but downscaled while
/// still stored and turned afterwards — the turn copies the small image, never
/// the full-size decode.
fn prepare_oriented((mut raw, orientation): (RawImage, u8), max_w: u32, max_h: u32, blur: f32) -> DecodedImage {
    premultiply(&mut raw);
    let turned = (5..=8).contains(&orientation);
    let (ow, oh) = if turned { (raw.h, raw.w) } else { (raw.w, raw.h) };
    let (lw, lh) = layout_size(ow, oh, max_w, max_h);
    let (sw, sh) = if turned { (lh, lw) } else { (lw, lh) };
    let stored = if (sw, sh) == (raw.w, raw.h) {
        raw
    } else {
        RawImage { w: sw, h: sh, rgba: resize_area(&raw.rgba, raw.w, raw.h, sw, sh) }
    };
    let up = orient(stored, orientation);
    let mut level0 = MipLevel { w: up.w, h: up.h, rgba: up.rgba };
    let radius = blur_radius(blur);
    let blurred = radius > 0;
    if blurred {
        let (bw, bh) = ((lw / BLUR_DOWNSCALE).max(1), (lh / BLUR_DOWNSCALE).max(1));
        let small = resize_area(&level0.rgba, lw, lh, bw, bh);
        level0 = MipLevel { w: bw, h: bh, rgba: blur3(&small, bw, bh, radius) };
    }
    let (lum_p5, lum_p95) = luminance_percentiles(&level0.rgba, level0.w, level0.h);
    DecodedImage { layout_w: lw, layout_h: lh, mips: build_mips(level0), lum_p5, lum_p95, blurred }
}

/// Load `path` for the backdrop: size-checked read, decode, [`prepare`]. Never
/// panics (a decoder panic is caught). The `Err` is a short reason for the
/// user ("not a PNG or JPEG file", "4000×9000 is too large …").
pub fn load(path: &Path, max_w: u32, max_h: u32, blur: f32) -> Result<DecodedImage, String> {
    let data = read_checked(path)?;
    std::panic::catch_unwind(move || decode_stored(&data).map(|d| prepare_oriented(d, max_w, max_h, blur)))
        .unwrap_or_else(|_| Err("the decoder failed on this file".into()))
}

/// What [`load`] would refuse `path` for, found without decoding it — the
/// file's size, its magic bytes and the dimensions in its header — for `jetty
/// --check-config`. `Ok` when it should load.
pub fn check(path: &Path) -> Result<(), String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let data = read_checked(path)?;
    let (w, h) = std::panic::catch_unwind(|| match sniff(&data) {
        Some(ImageKind::Png) => {
            let reader = png::Decoder::new(std::io::Cursor::new(&data[..]))
                .read_info()
                .map_err(|e| decode_error("PNG", &e.to_string()))?;
            Ok((reader.info().width, reader.info().height))
        }
        Some(ImageKind::Jpeg) => {
            let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(&data[..]));
            decoder.decode_headers().map_err(|e| decode_error("JPEG", &format!("{e:?}")))?;
            let info = decoder.info().ok_or("bad JPEG (no header)")?;
            Ok((u32::from(info.width), u32::from(info.height)))
        }
        None => Err("not a PNG or JPEG file".to_string()),
    })
    .unwrap_or_else(|_| Err("the decoder failed on this file".into()))?;
    check_dims(w, h)
}

/// `path`'s bytes, when it is a file no larger than [`MAX_FILE_BYTES`].
fn read_checked(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "file not found".to_string(),
        _ => format!("cannot read it ({e})"),
    })?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("the file is larger than {} MB", MAX_FILE_BYTES / (1024 * 1024)));
    }
    std::fs::read(path).map_err(|e| format!("cannot read it ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode straight RGBA8 as an in-memory PNG (the png crate's encoder).
    fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(rgba).unwrap();
        }
        out
    }

    /// A 16×8 baseline JPEG (PIL, quality 95): left half red, right half blue.
    const JPEG_16X8: &[u8] = include_bytes!("../tests/fixtures/backdrop-16x8.jpg");

    #[test]
    fn a_file_is_checked_without_decoding_it() {
        // `jetty --check-config` says what `load` would, from the header only.
        let dir = std::env::temp_dir().join(format!("jetty-backdrop-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        assert_eq!(check(&file("ok.png", &png_bytes(2, 2, &[9; 16]))), Ok(()));
        assert_eq!(check(&file("ok.jpg", JPEG_16X8)), Ok(()));
        assert_eq!(check(&dir.join("gone.png")), Err("file not found".to_string()));
        assert_eq!(check(&file("x.gif", b"GIF89a....")), Err("not a PNG or JPEG file".to_string()));
        // Too wide: refused from the header.
        let wide = png_bytes(9000, 1, &vec![0; 9000 * 4]);
        assert_eq!(check(&file("wide.png", &wide)), Err("9000×1 is too large (max 8192×8192)".to_string()));
        assert!(check(&dir).is_err(), "a folder is not an image");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sniff_by_magic_not_extension() {
        assert_eq!(sniff(&png_bytes(1, 1, &[1, 2, 3, 255])), Some(ImageKind::Png));
        assert_eq!(sniff(JPEG_16X8), Some(ImageKind::Jpeg));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(&[]), None);
        assert!(decode_bytes(b"hello world").unwrap_err().contains("not a PNG or JPEG"));
    }

    /// Encode `data` as an in-memory PNG of `color` / `depth` (with `palette`
    /// for indexed images).
    fn png_of(w: u32, h: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8], palette: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(color);
            e.set_depth(depth);
            if let Some(p) = palette {
                e.set_palette(p.to_vec());
            }
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(data).unwrap();
        }
        out
    }

    /// Every PNG flavor decodes to the same straight RGBA8: RGB, gray, gray +
    /// alpha, indexed (with and without transparency), 16-bit.
    #[test]
    fn every_png_color_type_decodes_to_rgba() {
        use png::{BitDepth, ColorType};
        let rgb: Vec<u8> = (0..3 * 3 * 2).map(|i| (i * 13) as u8).collect();
        let raw = decode_bytes(&png_of(3, 2, ColorType::Rgb, BitDepth::Eight, &rgb, None)).unwrap();
        let want: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        assert_eq!((raw.w, raw.h, raw.rgba), (3, 2, want));

        let gray = [0u8, 64, 128, 255, 7, 99];
        let raw = decode_bytes(&png_of(2, 3, ColorType::Grayscale, BitDepth::Eight, &gray, None)).unwrap();
        let want: Vec<u8> = gray.iter().flat_map(|&g| [g, g, g, 255]).collect();
        assert_eq!(raw.rgba, want);

        let ga = [10u8, 0, 20, 50, 30, 128, 40, 255];
        let raw = decode_bytes(&png_of(4, 1, ColorType::GrayscaleAlpha, BitDepth::Eight, &ga, None)).unwrap();
        let want: Vec<u8> = ga.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect();
        assert_eq!(raw.rgba, want);

        let palette = [255u8, 0, 0, 0, 255, 0, 0, 0, 255];
        let idx = [0u8, 1, 2, 1];
        let raw = decode_bytes(&png_of(2, 2, ColorType::Indexed, BitDepth::Eight, &idx, Some(&palette))).unwrap();
        assert_eq!(raw.rgba, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255]);

        // 16-bit RGB: the high byte of each sample survives (STRIP_16).
        let rgb16: Vec<u8> = [0x1234u16, 0xABCD, 0xFF00, 0x0102, 0x8000, 0x7FFF]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let raw = decode_bytes(&png_of(2, 1, ColorType::Rgb, BitDepth::Sixteen, &rgb16, None)).unwrap();
        assert_eq!(raw.rgba, [0x12, 0xAB, 0xFF, 255, 0x01, 0x80, 0x7F, 255]);
        // A wide odd-sized RGB image (in-place expansion across many rows).
        let (w, h) = (37u32, 23u32);
        let big: Vec<u8> = (0..w * h * 3).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
        let raw = decode_bytes(&png_of(w, h, ColorType::Rgb, BitDepth::Eight, &big, None)).unwrap();
        let want: Vec<u8> = big.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        assert!(raw.rgba == want, "a 37×23 RGB image survives the expansion");
    }

    #[test]
    fn png_round_trip() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| (i * 9) as u8).collect();
        let raw = decode_bytes(&png_bytes(3, 2, &rgba)).unwrap();
        assert_eq!((raw.w, raw.h), (3, 2));
        assert_eq!(raw.rgba, rgba);
    }

    #[test]
    fn jpeg_decodes_to_rgba() {
        let raw = decode_bytes(JPEG_16X8).unwrap();
        assert_eq!((raw.w, raw.h), (16, 8));
        assert_eq!(raw.rgba.len(), 16 * 8 * 4);
        // Left half red, right half blue (lossy: generous tolerance), opaque.
        let px = |x: usize, y: usize| &raw.rgba[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert!(px(2, 4)[0] > 200 && px(2, 4)[2] < 60, "{:?}", px(2, 4));
        assert!(px(13, 4)[2] > 200 && px(13, 4)[0] < 60, "{:?}", px(13, 4));
        assert!(raw.rgba.chunks_exact(4).all(|p| p[3] == 255));
    }

    /// The 16×8 red|blue picture as a phone stores it: pixels turned 90° CCW
    /// (8×16) and EXIF Orientation 6 ("rotate 90° CW to display").
    const JPEG_16X8_EXIF6: &[u8] = include_bytes!("../tests/fixtures/backdrop-16x8-exif6.jpg");

    /// A minimal TIFF block (either byte order) whose IFD0 holds an unrelated
    /// tag, then Orientation `o`.
    fn exif_tiff(o: u16, big_endian: bool) -> Vec<u8> {
        let u16b = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let u32b = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut t = Vec::new();
        t.extend_from_slice(if big_endian { b"MM" } else { b"II" });
        t.extend_from_slice(&u16b(42));
        t.extend_from_slice(&u32b(8)); // IFD0 right after the header
        t.extend_from_slice(&u16b(2)); // two entries: an unrelated tag, then Orientation
        t.extend_from_slice(&u16b(0x010F)); // Make (ASCII) — skipped
        t.extend_from_slice(&u16b(2));
        t.extend_from_slice(&u32b(4));
        t.extend_from_slice(b"abc\0");
        t.extend_from_slice(&u16b(0x0112));
        t.extend_from_slice(&u16b(3)); // SHORT
        t.extend_from_slice(&u32b(1));
        t.extend_from_slice(&u16b(o));
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&u32b(0)); // no next IFD
        t
    }

    #[test]
    fn exif_orientation_is_read_from_either_byte_order() {
        for o in 1..=8u16 {
            assert_eq!(exif_orientation(&exif_tiff(o, false)), Some(o as u8));
            assert_eq!(exif_orientation(&exif_tiff(o, true)), Some(o as u8));
        }
        // A PNG eXIf written with the JPEG APP1 "Exif\0\0" prefix still reads.
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend(exif_tiff(6, false));
        assert_eq!(exif_orientation(&prefixed), Some(6));
        // Out of range, truncated, garbage, empty: no orientation (and no panic).
        assert_eq!(exif_orientation(&exif_tiff(9, false)), None);
        assert_eq!(exif_orientation(&exif_tiff(0, true)), None);
        let t = exif_tiff(6, false);
        for cut in 0..t.len() {
            let _ = exif_orientation(&t[..cut]);
        }
        assert_eq!(exif_orientation(&t[..20]), None);
        assert_eq!(exif_orientation(b"II*\0\xff\xff\xff\xff"), None, "IFD offset past the end");
        assert_eq!(exif_orientation(&[]), None);
    }

    #[test]
    fn every_exif_orientation_turns_the_picture_upright() {
        // Stored 3×2:  a b c / d e f  (one byte per pixel tells them apart).
        let stored = || RawImage { w: 3, h: 2, rgba: (0..6u8).flat_map(|v| [b'a' + v, 0, 0, 255]).collect() };
        let shown = |r: &RawImage| -> (u32, u32, String) {
            (r.w, r.h, r.rgba.chunks_exact(4).map(|p| p[0] as char).collect())
        };
        for (o, want) in [
            (1, (3, 2, "abcdef")),
            (2, (3, 2, "cbafed")),
            (3, (3, 2, "fedcba")),
            (4, (3, 2, "defabc")),
            (5, (2, 3, "adbecf")),
            (6, (2, 3, "daebfc")),
            (7, (2, 3, "fcebda")),
            (8, (2, 3, "cfbead")),
            (0, (3, 2, "abcdef")),
            (42, (3, 2, "abcdef")),
        ] {
            assert_eq!(shown(&orient(stored(), o)), (want.0, want.1, want.2.to_string()), "orientation {o}");
        }
    }

    #[test]
    fn an_exif_rotated_jpeg_decodes_upright() {
        let raw = decode_bytes(JPEG_16X8_EXIF6).unwrap();
        assert_eq!((raw.w, raw.h), (16, 8), "a phone photo is shown the way it was taken");
        let px = |x: usize, y: usize| &raw.rgba[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert!(px(2, 4)[0] > 200 && px(2, 4)[2] < 60, "red on the left: {:?}", px(2, 4));
        assert!(px(13, 4)[2] > 200 && px(13, 4)[0] < 60, "blue on the right: {:?}", px(13, 4));
        // …and through the whole load path (downscale first, then turned).
        let img = prepare_oriented(decode_stored(JPEG_16X8_EXIF6).unwrap(), 4, 4, 0.0);
        assert_eq!((img.layout_w, img.layout_h), (8, 4), "covers 4×4 in the upright aspect");
        let l0 = &img.mips[0];
        assert_eq!((l0.w, l0.h), (8, 4));
        assert!(l0.rgba[(2 * 8) * 4] > 200 && l0.rgba[(2 * 8 + 7) * 4 + 2] > 200, "red left, blue right");
    }

    #[test]
    fn a_png_exif_chunk_orients_it_too() {
        // A 2×1 red|blue picture stored turned 90° CCW (1×2: blue over red)
        // with an eXIf chunk saying "rotate 90° CW" (6): shown red|blue again.
        let mut png = png_bytes(1, 2, &[0, 0, 255, 255, 255, 0, 0, 255]);
        let tiff = exif_tiff(6, true);
        let mut chunk = (tiff.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"eXIf");
        chunk.extend_from_slice(&tiff);
        let crc = crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        // After the 8-byte signature and the 25-byte IHDR chunk.
        png.splice(33..33, chunk);
        let raw = decode_bytes(&png).unwrap();
        assert_eq!((raw.w, raw.h), (2, 1));
        assert_eq!(&raw.rgba[..4], &[255, 0, 0, 255], "red on the left");
        assert_eq!(&raw.rgba[4..], &[0, 0, 255, 255], "blue on the right");
    }

    #[test]
    fn broken_files_are_errors_not_panics() {
        // A PNG signature followed by garbage, a truncated JPEG, an empty file.
        let mut bad_png = png_bytes(4, 4, &[7u8; 64]);
        bad_png.truncate(20);
        assert!(decode_bytes(&bad_png).is_err());
        assert!(decode_bytes(&JPEG_16X8[..40]).is_err());
        // Every reason is one short line (it is shown in a notice pill).
        let mut junk = vec![0xFF, 0xD8, 0xFF, 0xE0];
        junk.extend((0..4000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
        let err = decode_bytes(&junk).unwrap_err();
        assert!(!err.contains('\n') && err.chars().count() < 100, "{err:?}");
        assert!(decode_bytes(&[]).is_err());
    }

    #[test]
    fn oversized_header_is_refused_before_allocation() {
        // A valid 1×1 PNG whose IHDR claims 9000×10: the header check refuses it.
        let mut png = png_bytes(1, 1, &[0, 0, 0, 255]);
        // IHDR data starts at byte 16: width (4 bytes BE), height (4 bytes BE).
        png[16..20].copy_from_slice(&9000u32.to_be_bytes());
        png[20..24].copy_from_slice(&10u32.to_be_bytes());
        // Fix the IHDR CRC so the decoder reaches the dimension check.
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        let err = decode_bytes(&png).unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }

    /// Plain CRC-32 (IEEE) for the PNG chunk fix-up above.
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    #[test]
    fn missing_file_is_a_readable_error() {
        let err = load(Path::new("/nonexistent/jetty/backdrop.png"), 100, 100, 0.0).unwrap_err();
        assert_eq!(err, "file not found");
    }

    #[test]
    fn area_weights_sum_to_one() {
        for (s, d) in [(10, 3), (7, 7), (1920, 1280), (5, 1), (3, 2)] {
            for taps in area_weights(s, d) {
                let sum: f32 = taps.iter().map(|t| t.1).sum();
                assert!((sum - 1.0).abs() < 1e-4, "{s}->{d}: {sum}");
            }
        }
        // 2:1 is a plain pair average.
        assert_eq!(area_weights(4, 2), vec![vec![(0, 0.5), (1, 0.5)], vec![(2, 0.5), (3, 0.5)]]);
    }

    #[test]
    fn downscale_averages_in_linear_light() {
        // A black/white checker averages to linear 0.5 = sRGB code 188, not 128.
        let mut src = Vec::new();
        for y in 0..2u32 {
            for x in 0..2u32 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                src.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let out = resize_area(&src, 2, 2, 1, 1);
        assert!((out[0] as i32 - 188).abs() <= 1, "{out:?}");
        assert_eq!(out[3], 255);
    }

    #[test]
    fn layout_size_covers_the_monitor_without_upscaling() {
        // A 4000×2000 photo on a 1920×1200 monitor: height must cover 1200.
        assert_eq!(layout_size(4000, 2000, 1920, 1200), (2400, 1200));
        // Smaller than the monitor: untouched (never upscaled on the CPU).
        assert_eq!(layout_size(800, 600, 1920, 1200), (800, 600));
        assert_eq!(layout_size(0, 0, 1920, 1200), (1, 1));
    }

    #[test]
    fn mips_follow_the_gpu_size_rule() {
        let l0 = MipLevel { w: 5, h: 3, rgba: vec![255; 5 * 3 * 4] };
        let mips = build_mips(l0);
        let sizes: Vec<(u32, u32)> = mips.iter().map(|m| (m.w, m.h)).collect();
        assert_eq!(sizes, vec![(5, 3), (2, 1), (1, 1)]);
        assert!(mips.iter().all(|m| m.rgba.len() == (m.w * m.h * 4) as usize));
        // A uniform white image stays white at every level.
        assert!(mips.iter().all(|m| m.rgba.iter().all(|&v| v == 255)));
        // Level k is max(1, size >> k) and the count is floor(log2(max)) + 1 —
        // what wgpu validates (a 1100×733 chain has 11 levels, not 12).
        for (w, h) in [(1100u32, 733u32), (275, 183), (1, 1), (4096, 7), (3, 1000)] {
            let mips = build_mips(MipLevel { w, h, rgba: vec![9; (w * h * 4) as usize] });
            assert_eq!(mips.len() as u32, 32 - w.max(h).leading_zeros(), "{w}×{h}");
            for (k, m) in mips.iter().enumerate() {
                assert_eq!((m.w, m.h), ((w >> k).max(1), (h >> k).max(1)), "{w}×{h} level {k}");
            }
        }
    }

    #[test]
    fn blur_keeps_flat_images_flat_and_spreads_a_dot() {
        let flat = vec![100u8; 9 * 9 * 4];
        assert_eq!(blur3(&flat, 9, 9, 2), flat);
        let mut dot = vec![0u8; 9 * 9 * 4];
        let c = (4 * 9 + 4) * 4;
        dot[c..c + 4].copy_from_slice(&[255, 255, 255, 255]);
        let out = blur3(&dot, 9, 9, 1);
        assert!(out[c] < 255 && out[c] > 0, "the center dims");
        assert!(out[(4 * 9 + 6) * 4] > 0, "two px away gets some light");
    }

    #[test]
    fn premultiply_is_linear_and_skips_opaque() {
        let mut opaque = RawImage { w: 1, h: 1, rgba: vec![200, 100, 50, 255] };
        premultiply(&mut opaque);
        assert_eq!(opaque.rgba, vec![200, 100, 50, 255]);
        let mut half = RawImage { w: 1, h: 1, rgba: vec![255, 255, 255, 128] };
        premultiply(&mut half);
        // linear 1.0 × 0.502 → sRGB code ≈ 188.
        assert!((half.rgba[0] as i32 - 188).abs() <= 1, "{:?}", half.rgba);
        assert_eq!(half.rgba[3], 128);
    }

    #[test]
    fn percentiles_of_a_two_tone_image() {
        // 90 % black, 10 % white → p5 = 0, p95 = 1.
        let mut img = Vec::new();
        for i in 0..100 {
            let v = if i < 90 { 0 } else { 255 };
            img.extend_from_slice(&[v, v, v, 255]);
        }
        let (p5, p95) = luminance_percentiles(&img, 10, 10);
        assert!(p5 < 0.01 && p95 > 0.99, "{p5} {p95}");
        // Uniform mid gray: both percentiles at its luminance.
        let gray = vec![128u8; 16 * 4];
        let (a, b) = luminance_percentiles(&gray, 4, 4);
        assert!((a - 0.2158).abs() < 0.01 && (b - 0.2158).abs() < 0.01, "{a} {b}");
    }

    #[test]
    fn prepare_downscales_blurs_and_measures() {
        let w = 64;
        let h = 32;
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| {
            let v = if (i % w) < w / 2 { 30 } else { 220 };
            [v as u8, v as u8, v as u8, 255]
        }).collect();
        let raw = RawImage { w, h, rgba };
        let sharp = prepare(RawImage { w, h, rgba: raw.rgba.clone() }, 32, 16, 0.0);
        assert_eq!((sharp.layout_w, sharp.layout_h), (32, 16));
        assert_eq!((sharp.mips[0].w, sharp.mips[0].h), (32, 16));
        assert!(!sharp.blurred);
        assert!(sharp.lum_p95 > sharp.lum_p5 + 0.5);
        let frosted = prepare(raw, 32, 16, 0.5);
        assert!(frosted.blurred);
        // Laid out at the same size, stored at a quarter of it.
        assert_eq!((frosted.layout_w, frosted.layout_h), (32, 16));
        assert_eq!((frosted.mips[0].w, frosted.mips[0].h), (8, 4));
        assert_eq!(frosted.mips.last().map(|m| (m.w, m.h)), Some((1, 1)));
    }
}
