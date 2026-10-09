//! CPU-only `Terminal::feed` throughput gate (SPEED is the #1 requirement).
//!
//!     cargo run -p jetty-core --release --example feed_bench
//!
//! A standalone binary on purpose: an `#[ignore]`d test shares its binary with
//! the whole test suite, and that code-layout change alone moved these numbers
//! by 3-4% — a separate example measures the codegen that actually ships.
//! Compare two builds with `taskset -c N` and best-of-several runs; the noise
//! floor between identical binaries is about ±1%.
//!
//! Each workload is fed in 8 KiB chunks (the PTY reader's read size) into a
//! 120×40 terminal at the default 10k scrollback, so the full-history path is
//! what gets measured. The image workloads include the decode, which runs on
//! the UI thread as the bytes arrive.
use jetty_core::Terminal;

fn repeat(unit: &[u8], total: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(total + unit.len());
    while v.len() < total {
        v.extend_from_slice(unit);
    }
    v
}

/// A ~1 MB sixel: 800×1920 px, every sixel row in four colors.
fn sixel_image() -> Vec<u8> {
    let mut s = b"\x1bPq\"1;1;800;1920#0;2;90;10;10#1;2;10;90;10#2;2;10;10;90#3;2;90;90;10".to_vec();
    for row in 0..320 {
        for color in 0..4 {
            s.extend_from_slice(format!("#{color}").as_bytes());
            s.extend((0..800).map(|x| b'?' + ((x + row + color * 7) % 64) as u8));
            s.push(b'$');
        }
        s.push(b'-');
    }
    s.extend_from_slice(b"\x1b\\");
    s
}

/// A 512×512 RGBA PNG of noise (it hardly compresses: ~1 MB), transmitted and
/// shown kitty-style (`a=T,f=100`) in 4 KiB base64 chunks.
fn kitty_png() -> Vec<u8> {
    let (w, h) = (512u32, 512u32);
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let pixels: Vec<u8> = (0..w * h * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    let mut png_bytes = Vec::new();
    let mut enc = png::Encoder::new(&mut png_bytes, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&pixels)).expect("png encode");
    let b64 = base64(&png_bytes);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
    let mut out = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let head = if i == 0 { format!("a=T,f=100,q=2,m={more}") } else { format!("m={more}") };
        out.extend_from_slice(format!("\x1b_G{head};").as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            s.push(if i <= c.len() { A[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    s
}

fn main() {
    const TOTAL: usize = 48 * 1024 * 1024;
    let long_line = {
        let mut s = "abcdefghij".repeat(12);
        s.truncate(118);
        s.push_str("\r\n");
        s
    };
    let color: &[u8] =
        b"\x1b[1;32mINFO\x1b[0m \x1b[38;5;208mcompiling\x1b[0m crate v1.2.3 (\x1b[4m/src/lib.rs\x1b[24m)\r\n";
    // A shell-integrated session: a prompt (OSC 133 A/C/D) every 50 colored lines,
    // so prompt marks stay live and the exact scroll accounting is always on.
    let prompts = {
        let mut unit = b"\x1b]133;A\x07$ make\r\n\x1b]133;C\x07".to_vec();
        for _ in 0..50 {
            unit.extend_from_slice(color);
        }
        unit.extend_from_slice(b"\x1b]133;D;0\x07");
        unit
    };
    // A TUI redrawing its region of the primary screen in synchronized updates
    // (Claude Code's frames), some of them spanning reads.
    let frames = {
        let mut unit = b"\x1b[?2026h\x1b[10;1H".to_vec();
        for _ in 0..30 {
            unit.extend_from_slice(color);
        }
        unit.extend_from_slice(b"\x1b[?2026l");
        unit
    };
    let workloads: [(&str, Vec<u8>, bool); 9] = [
        ("yes", repeat(b"y\r\n", TOTAL), false),
        ("yes+mark", repeat(b"y\r\n", TOTAL), true),
        ("long120", repeat(long_line.as_bytes(), TOTAL), false),
        ("esc-heavy", repeat(color, TOTAL), false),
        ("esc-heavy+mark", repeat(color, TOTAL), true),
        ("prompts-every-50", repeat(&prompts, TOTAL), false),
        ("sixel-1MB", repeat(&sixel_image(), TOTAL / 8), false),
        ("kitty-png-chunked", repeat(&kitty_png(), TOTAL / 8), false),
        ("sync-frames+mark", repeat(&frames, TOTAL), true),
    ];
    for (name, data, mark) in &workloads {
        let mut best = f64::MAX;
        let mut images = 0;
        for _ in 0..3 {
            let mut t = Terminal::new(120, 40);
            if *mark {
                t.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
            }
            let start = std::time::Instant::now();
            for chunk in data.chunks(8192) {
                t.feed(chunk);
            }
            best = best.min(start.elapsed().as_secs_f64());
            std::hint::black_box(t.scroll_max());
            images = t.visible_images().len();
        }
        let mbps = data.len() as f64 / (1024.0 * 1024.0) / best;
        // An image workload that shows nothing measured a refusal, not a decode.
        let shown = if images > 0 { format!("  (on screen: {images})") } else { String::new() };
        println!("BENCH {name:<18} {mbps:>8.1} MB/s{shown}");
    }
}
