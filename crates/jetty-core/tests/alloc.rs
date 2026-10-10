//! The hot path allocates nothing it doesn't need. Timings are too noisy for CI
//! to gate on (see docs/perf-budget.md), but allocation counts are exact: these
//! hold what feeding output and taking the per-frame snapshot allocate, so a
//! regression fails CI instead of showing up in a later bench run.
//!
//! The counter is per thread (the test harness runs tests side by side).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations `f` makes on this thread.
fn allocs_in(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

const LINE: &[u8] = b"the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnop\r\n";

/// A 120x40 terminal whose 1000-line scrollback is already full, so its ring
/// never grows again — the steady state of any tab that has run a while.
fn full_terminal() -> jetty_core::Terminal {
    let mut t = jetty_core::Terminal::new(120, 40);
    t.set_scrollback_lines(1000);
    for _ in 0..1200 {
        t.feed(LINE);
    }
    t
}

#[test]
fn feeding_output_into_a_full_scrollback_allocates_nothing() {
    let mut t = full_terminal();
    // ~8 KiB reads, as the PTY reader hands them over: plain text, then text
    // with 16-color, 256-color and true-color SGR runs.
    let plain = LINE.repeat(110);
    let colored = b"\x1b[1;32mok\x1b[0m \x1b[38;5;208mwarn\x1b[0m \x1b[38;2;10;20;30mrgb\x1b[0m done\r\n".repeat(80);
    for chunk in [&plain, &colored] {
        let n = allocs_in(|| {
            for _ in 0..20 {
                t.feed(chunk);
            }
        });
        assert_eq!(n, 0, "Terminal::feed allocated {n} times for {} KiB", chunk.len() * 20 / 1024);
    }
}

#[test]
fn a_frames_snapshot_of_a_plain_screen_allocates_once() {
    // The snapshot runs every frame: its cells are one Vec, and nothing else
    // (no per-row or per-cell allocation) is allowed in.
    let t = full_terminal();
    let n = allocs_in(|| {
        let _ = std::hint::black_box(t.snapshot());
    });
    assert!(n <= 1, "Terminal::snapshot allocated {n} times");
}
