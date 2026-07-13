//! Safe, hand-rolled standard base64 (RFC 4648) decoder for UNTRUSTED PTY bytes.
//!
//! Kitty graphics payloads arrive base64-encoded over the PTY, so — exactly like
//! the sixel decoder — a malformed or adversarial payload MUST NEVER panic or
//! over-allocate. Every guarantee here is structural:
//!
//! * **Pre-sized, capped output.** The output length is computed up-front with
//!   saturating math and rejected (`None`) BEFORE any allocation if it would
//!   exceed `max_out`. The `Vec` is created `with_capacity` at that exact bound.
//! * **Strict alphabet.** A 256-entry const table maps each byte to `0..=63` or
//!   "invalid"; any invalid byte (whitespace, high byte, `*`, an INTERIOR `=`)
//!   makes the whole decode fail. We do not silently skip anything — Kitty
//!   payloads contain no whitespace.
//! * **Padding tolerant (A7).** A trailing `=`/`==` is accepted, AND a tool that
//!   omits padding (final group of 2 or 3 symbols → 1 or 2 bytes) still decodes.
//!   A lone trailing symbol (`len % 4 == 1`) is rejected as structurally impossible.
//!
//! No recursion, no user-controlled multiplication that isn't saturating, no
//! unchecked index (the table is indexed by a `u8`, always in range). Invalid
//! input is *rejected*, never interpreted — the correct-or-absent contract.

/// Base64 decode table: `A–Z a–z 0–9 + /` → `0..=63`, everything else → `0xFF`
/// (invalid). `=` is 0xFF here too; it is stripped as trailing padding before the
/// table is consulted, so any `=` that reaches the table is an interior one and
/// is correctly rejected.
const TABLE: [u8; 256] = build_table();

const fn build_table() -> [u8; 256] {
    let mut t = [0xFFu8; 256];
    let mut i = 0u8;
    // A-Z → 0..=25
    while i < 26 {
        t[(b'A' + i) as usize] = i;
        i += 1;
    }
    // a-z → 26..=51
    i = 0;
    while i < 26 {
        t[(b'a' + i) as usize] = 26 + i;
        i += 1;
    }
    // 0-9 → 52..=61
    i = 0;
    while i < 10 {
        t[(b'0' + i) as usize] = 52 + i;
        i += 1;
    }
    t[b'+' as usize] = 62;
    t[b'/' as usize] = 63;
    t
}

#[inline]
fn val(b: u8) -> Option<u32> {
    let v = TABLE[b as usize];
    if v < 64 {
        Some(v as u32)
    } else {
        None
    }
}

/// Decode standard base64 into bytes, returning `None` on any invalid character,
/// bad length, or an output that would exceed `max_out`. Pure, never panics.
pub fn decode_base64(input: &[u8], max_out: usize) -> Option<Vec<u8>> {
    // Strip at most two trailing '=' pad bytes. Anything beyond that (e.g.
    // "====") leaves a '=' at the new end, which the table rejects below.
    let mut end = input.len();
    let mut pad = 0usize;
    while end > 0 && input[end - 1] == b'=' && pad < 2 {
        end -= 1;
        pad += 1;
    }
    let data = &input[..end];

    let rem = data.len() % 4;
    // A single leftover symbol cannot encode any byte — structurally invalid.
    if rem == 1 {
        return None;
    }

    // Pre-size guard (saturating): reject before allocating if over cap.
    let full_groups = data.len() / 4;
    let tail_out = match rem {
        2 => 1,
        3 => 2,
        _ => 0,
    };
    let out_upper = full_groups.saturating_mul(3).saturating_add(tail_out);
    if out_upper > max_out {
        return None;
    }
    let mut out = Vec::with_capacity(out_upper);

    // Full 4-symbol groups → 3 bytes each.
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let a = val(data[i])?;
        let b = val(data[i + 1])?;
        let c = val(data[i + 2])?;
        let d = val(data[i + 3])?;
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
        i += 4;
    }

    // Trailing partial group (padded or not): 2 symbols → 1 byte, 3 → 2 bytes.
    match rem {
        2 => {
            let a = val(data[i])?;
            let b = val(data[i + 1])?;
            let n = (a << 18) | (b << 12);
            out.push((n >> 16) as u8);
        }
        3 => {
            let a = val(data[i])?;
            let b = val(data[i + 1])?;
            let c = val(data[i + 2])?;
            let n = (a << 18) | (b << 12) | (c << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        }
        _ => {}
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 64 * 1024 * 1024;

    #[test]
    fn valid_padded() {
        assert_eq!(decode_base64(b"aGk=", CAP).unwrap(), b"hi");
    }

    #[test]
    fn valid_unpadded_full() {
        assert_eq!(decode_base64(b"YWJj", CAP).unwrap(), b"abc");
    }

    #[test]
    fn two_pad() {
        assert_eq!(decode_base64(b"YQ==", CAP).unwrap(), b"a");
    }

    #[test]
    fn unpadded_tail_one_byte() {
        // "YQ" (no padding) → one byte 'a' (A7).
        assert_eq!(decode_base64(b"YQ", CAP).unwrap(), b"a");
    }

    #[test]
    fn unpadded_tail_two_bytes() {
        // "aGk" (no padding) → "hi" (A7).
        assert_eq!(decode_base64(b"aGk", CAP).unwrap(), b"hi");
    }

    #[test]
    fn reject_interior_pad() {
        assert!(decode_base64(b"YQ==YWJj", CAP).is_none());
    }

    #[test]
    fn reject_non_alphabet() {
        assert!(decode_base64(b"ab*d", CAP).is_none());
        assert!(decode_base64(b"ab d", CAP).is_none());
        assert!(decode_base64(b"ab\nd", CAP).is_none());
        assert!(decode_base64(&[b'a', b'b', 0x80, b'd'], CAP).is_none());
    }

    #[test]
    fn reject_lone_trailing_symbol() {
        assert!(decode_base64(b"abcde", CAP).is_none()); // len%4 == 1
        assert!(decode_base64(b"Y", CAP).is_none());
    }

    #[test]
    fn reject_excess_padding() {
        assert!(decode_base64(b"====", CAP).is_none());
    }

    #[test]
    fn max_out_rejection_allocates_nothing_over_cap() {
        // 8 symbols → 6 bytes; cap of 5 must reject with None, before allocating.
        assert!(decode_base64(b"YWJjZGVm", 5).is_none());
        // Exactly at cap is fine.
        assert_eq!(decode_base64(b"YWJjZGVm", 6).unwrap(), b"abcdef");
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(decode_base64(b"", CAP).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn roundtrip_all_bytes() {
        // Encode 0..=255 with a reference encoder, decode back, compare.
        let raw: Vec<u8> = (0..=255u8).collect();
        let enc = encode(&raw);
        assert_eq!(decode_base64(enc.as_bytes(), CAP).unwrap(), raw);
    }

    // Tiny reference encoder used only by the roundtrip test.
    fn encode(data: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            s.push(A[((n >> 18) & 63) as usize] as char);
            s.push(A[((n >> 12) & 63) as usize] as char);
            if chunk.len() > 1 {
                s.push(A[((n >> 6) & 63) as usize] as char);
            } else {
                s.push('=');
            }
            if chunk.len() > 2 {
                s.push(A[(n & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
        s
    }

    #[test]
    fn fuzz_never_panics_and_respects_cap() {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..500 {
            let n = (next() % 300) as usize;
            let buf: Vec<u8> = (0..n).map(|_| (next() & 0xff) as u8).collect();
            let cap = (next() % 512) as usize;
            if let Some(out) = decode_base64(&buf, cap) {
                assert!(out.len() <= cap, "output must respect the cap");
            }
        }
    }
}
