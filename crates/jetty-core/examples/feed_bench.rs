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
//! what gets measured.
use jetty_core::Terminal;

fn repeat(unit: &[u8], total: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(total + unit.len());
    while v.len() < total {
        v.extend_from_slice(unit);
    }
    v
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
    let workloads: [(&str, Vec<u8>, bool); 6] = [
        ("yes", repeat(b"y\r\n", TOTAL), false),
        ("yes+mark", repeat(b"y\r\n", TOTAL), true),
        ("long120", repeat(long_line.as_bytes(), TOTAL), false),
        ("esc-heavy", repeat(color, TOTAL), false),
        ("esc-heavy+mark", repeat(color, TOTAL), true),
        ("prompts-every-50", repeat(&prompts, TOTAL), false),
    ];
    for (name, data, mark) in &workloads {
        let mut best = f64::MAX;
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
        }
        let mbps = data.len() as f64 / (1024.0 * 1024.0) / best;
        println!("BENCH {name:<18} {mbps:>8.1} MB/s");
    }
}
