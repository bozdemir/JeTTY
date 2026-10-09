//! Scrollback-search cost with the search bar open while output streams.
//!
//!     cargo run -p jetty-core --release --example search_bench
//!
//! A 240×50 terminal with a full 100k-line scrollback (the config's maximum) of
//! build-log lines. `set_query` is one keystroke in the bar (a full scan);
//! `refresh` is the throttled re-collect the app runs at most every 150 ms while
//! output streams, measured after each burst of `BURST` new lines. A rare query
//! has to read every cell; a common one stops at the match cap.
use jetty_core::Terminal;

const COLS: usize = 240;
const ROWS: usize = 50;
const SCROLLBACK: usize = 100_000;
/// Lines a 150 ms refresh window typically sees from a streaming build.
const BURST: usize = 40;
const REFRESHES: usize = 50;

fn line(i: usize) -> String {
    let mut s = format!(
        "\x1b[1;32m   Compiling\x1b[0m crate-{i:06} v0.{}.{} (/home/dev/src/workspace/crates/crate-{i:06}) took {}ms",
        i % 7,
        i % 13,
        i % 997
    );
    if i % 5000 == 2500 {
        s.push_str(" needle");
    }
    s.push_str("\r\n");
    s
}

fn main() {
    let mut t = Terminal::new(COLS, ROWS);
    t.set_scrollback_lines(SCROLLBACK);
    let mut fill = String::new();
    for i in 0..SCROLLBACK + ROWS {
        fill.push_str(&line(i));
    }
    for chunk in fill.as_bytes().chunks(8192) {
        t.feed(chunk);
    }
    let mut next = SCROLLBACK + ROWS;
    for query in ["needle", "crate"] {
        let start = std::time::Instant::now();
        let (_, total) = t.search_set_query(query);
        let set_ms = start.elapsed().as_secs_f64() * 1e3;
        let mut worst = 0f64;
        let mut sum = 0f64;
        for _ in 0..REFRESHES {
            let mut burst = String::new();
            for _ in 0..BURST {
                burst.push_str(&line(next));
                next += 1;
            }
            t.feed(burst.as_bytes());
            let start = std::time::Instant::now();
            t.search_refresh();
            let ms = start.elapsed().as_secs_f64() * 1e3;
            worst = worst.max(ms);
            sum += ms;
        }
        std::hint::black_box(t.search_counter());
        println!(
            "BENCH {query:<8} matches {total:>5}  set_query {set_ms:>8.2} ms  refresh avg {:>8.3} ms  worst {worst:>8.3} ms",
            sum / REFRESHES as f64
        );
        t.search_clear();
    }
}
