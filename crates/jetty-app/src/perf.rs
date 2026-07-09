//! Zero-cost-when-off real-window performance instrumentation (`JETTY_PERF_LOG=1`).
//!
//! JeTTY's brand is speed, so the three metrics that define terminal-speed culture
//! must be *measured honestly*, not asserted:
//!   * **input latency** (keypress → glyph on screen),
//!   * **cold start** (process exec → first presented frame),
//!   * **idle RSS**.
//!
//! This module stamps those three from the *running* app, gated behind a single
//! `bool` (`Perf::on`) that is read ONCE from the environment at startup. When the
//! flag is unset the hot paths are byte-identical to a build without this module:
//!  * the per-byte drain (`drain_pty` / `drain_one_tab`) gets nothing,
//!  * the per-frame present path adds a single predictable-false `if self.perf.on`,
//!  * no `Instant::now()`, no allocation, no `Vec` touch when off.
//!
//! ## Honesty of the numbers (this is the whole point of the release)
//! Every reported figure is labelled EXACTLY for what it includes and excludes; no
//! flattering framing.
//!  * **Input latency** is sampled ONLY for keystrokes pressed while the active tab
//!    was quiescent (a real prompt), so a streaming tab can't flip the next frame's
//!    `had` and record a near-zero non-echo latency (which would pull the median
//!    down). Two numbers are reported:
//!      - `keypress→frame-ready`: PTY write → the instant the frame's CPU data is
//!        built (just before the swapchain acquire). Includes the shell-echo round
//!        trip + reader-thread wake + drain + snapshot. Excludes winit event-in,
//!        the swapchain-acquire (vsync) wait, GPU submit, and scanout. This is the
//!        primary honest "app + echo" contribution, free of display cadence.
//!      - `keypress→pre-present`: the same, plus the swapchain-acquire (vsync) wait
//!        and GPU submit, up to just before `present()`. Still excludes scanout.
//!    The display refresh rate is reported alongside so the vsync component of the
//!    second number is interpretable. (JeTTY acquires the swapchain BEFORE encoding
//!    the render passes, so the vsync throttle sits at `acquire_frame`, not at a
//!    separate submit — the two stamps bracket exactly that wait.)
//!  * **Cold start** is a genuine exec→first-frame delta on Linux (`/proc/self/stat`
//!    field 22 vs `/proc/uptime`, USER_HZ=100), which INCLUDES the dynamic-linker /
//!    pre-`main` static-init time. Where that basis is unavailable (macOS without a
//!    libc dep) it falls back to a `main()`→first-frame `Instant` and SAYS SO.
//!  * **Percentiles** use linear interpolation, so p99 on a small sample is a blend
//!    of the top ranks — never silently the max. The sample is accumulated across
//!    the whole session (not a 64-wide window) and `n` is always printed.
//!  * **RSS** is `sysinfo::Process::memory()` (bytes) — resident set, which includes
//!    shared pages (RSS, not PSS); the doc notes that for apples-to-apples with
//!    alacritty/foot.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Process-start `Instant`, stamped as the very first statement of `main()`.
///
/// This is the fallback cold-start basis (it misses pre-`main` loader time); the
/// Linux path prefers the true exec time from `/proc`. Stored in a `OnceLock` so it
/// is set exactly once and readable from anywhere without threading it through.
static PROC_START: OnceLock<Instant> = OnceLock::new();

/// Record the earliest Rust entry instant. Call as the first line of `main()`.
pub fn mark_process_start() {
    let _ = PROC_START.set(Instant::now());
}

/// The `main()`-entry instant, if it was stamped.
pub fn process_start() -> Option<Instant> {
    PROC_START.get().copied()
}

/// A keystroke pressed within this window of the active tab's last output is treated
/// as NON-quiescent (the tab was streaming), so its latency is not sampled — that
/// frame's echo can't be told apart from the stream. Keeps the metric honest to the
/// quiescent-prompt case without biasing it downward.
const QUIESCENT_WINDOW: Duration = Duration::from_millis(40);

/// Emit a fresh percentile summary after this many NEW accumulated samples. The
/// emit runs from `about_to_wait` (off the timed present path), never inline on the
/// frame being measured.
const REPORT_EVERY: usize = 64;

/// Cap on retained latency samples (per stream). ~50k f32 ≈ 200 KB; far more than a
/// human session produces, and it keeps a pathological long-run from growing without
/// bound. Once reached we stop pushing (the distribution is already well-formed).
const SAMPLE_CAP: usize = 50_000;

/// Discard a keystroke whose measured round-trip exceeds this (occlusion, suspend,
/// or a hung shell) so a single multi-second outlier can't distort the summary.
const STALE_MS: f32 = 2_000.0;

/// True iff an environment flag is present (value irrelevant). The single seam both
/// `JETTY_PERF_LOG` (this module) and `JETTY_BENCH_CPU_ONLY` (the bench) select on,
/// so the "is it enabled" rule is defined and unit-tested in exactly one place.
pub fn env_enabled(v: Option<OsString>) -> bool {
    v.is_some()
}

/// Linear-interpolated percentile (`p` in `[0,100]`) over an ASCENDING-sorted slice.
///
/// Uses the same interpolation as NumPy's default (`linear`): the fractional rank
/// `p/100 * (n-1)` is blended between its two neighbours. This is the M5 fix — the
/// naive nearest-rank `(n*99)/100` makes "p99" equal the MAX on a 64-sample window,
/// which over-reports the tail. With interpolation, p99 on a small sample is a blend
/// of the top two ranks, not the single worst point.
pub fn percentile(sorted: &[f32], p: f64) -> f32 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let rank = (p / 100.0).clamp(0.0, 1.0) * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            if lo == hi {
                sorted[lo]
            } else {
                let frac = (rank - lo as f64) as f32;
                sorted[lo] + frac * (sorted[hi] - sorted[lo])
            }
        }
    }
}

/// Genuine cold-start delta (ms) and the label of the basis used.
///
/// Linux: exec→now from `/proc` (includes loader / pre-`main`). Elsewhere / on any
/// parse failure: the `main()`-entry `Instant` fallback, explicitly labelled as
/// excluding pre-`main`. `main_start` is the stamped `main()` instant (may be `None`
/// in tests).
pub fn cold_start(main_start: Option<Instant>) -> (f64, &'static str) {
    #[cfg(target_os = "linux")]
    {
        if let Some(ms) = linux_exec_to_now_ms() {
            return (ms, "exec→first-frame (/proc, USER_HZ=100, ±10ms; incl. loader/pre-main)");
        }
    }
    match main_start {
        Some(t) => (
            t.elapsed().as_secs_f64() * 1000.0,
            if cfg!(target_os = "linux") {
                "main()→first-frame (fallback; excl. pre-main loader)"
            } else {
                "main()→first-frame (exec basis needs libc on this OS; excl. pre-main loader)"
            },
        ),
        None => (0.0, "unavailable"),
    }
}

/// Linux exec→now in ms, from `/proc/self/stat` field 22 (starttime, in USER_HZ
/// ticks since boot) and `/proc/uptime` (seconds since boot). Both are measured
/// against the same boot epoch in the same clock, so the subtraction yields the
/// process's true age — capturing the dynamic-linker / static-init time before
/// `main()` runs.
///
/// USER_HZ (`sysconf(_SC_CLK_TCK)`) is 100 on every mainstream Linux and has been for
/// the kernel's entire modern history; we hardcode it because this crate carries no
/// libc dependency to call `sysconf`. The resulting resolution is ~10ms — adequate
/// for a ~100ms cold-start figure, and honestly labelled as such.
#[cfg(target_os = "linux")]
fn linux_exec_to_now_ms() -> Option<f64> {
    const USER_HZ: f64 = 100.0;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The `comm` field (2) is parenthesised and may itself contain spaces/parens,
    // so split on the LAST ')' to skip it; the remaining whitespace fields begin at
    // `state` (field 3). starttime is field 22 → index (22 - 3) = 19 in `rest`.
    let rparen = stat.rfind(')')?;
    let rest = stat.get(rparen + 2..)?; // skip ") "
    let starttime_ticks: f64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let age_secs = uptime_secs - starttime_ticks / USER_HZ;
    if age_secs.is_finite() {
        // Clamp a sub-tick rounding artifact to 0: `uptime` and `starttime` each have
        // ~10ms resolution, so a process sampled microseconds after exec can compute
        // a tiny NEGATIVE age. That's a rounding floor, not a parse failure, so we
        // still return Some (≈0) rather than falling back to the main() basis.
        Some(age_secs.max(0.0) * 1000.0)
    } else {
        None
    }
}

/// Resident-set size (bytes) of THIS process, or `None` if unavailable.
///
/// `sysinfo::Process::memory()` returns the resident set in BYTES on both Linux and
/// macOS (RSS — it includes shared pages, not PSS), so this is a cfg-free,
/// no-new-dep, apples-to-apples figure vs alacritty/foot. `refresh_processes`
/// populates memory by default. Sampled once per session on the idle path, so the
/// small `System` it builds is not on any hot path.
pub fn current_rss_bytes() -> Option<u64> {
    use sysinfo::{ProcessesToUpdate, System};
    let pid = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.memory())
}

/// Real-window perf state. A single field on `App` (`self.perf`), built once via
/// [`Perf::from_env`]. When `on` is false EVERYTHING here is inert.
pub struct Perf {
    /// Read ONCE from `JETTY_PERF_LOG` at startup; a plain bool thereafter. Every
    /// stamp site gates on this, so a default launch pays nothing but one
    /// predictable-false branch on the present path.
    pub on: bool,

    // --- input-latency stamping (main window only) ---
    /// Instant of the PTY write for a quiescent keystroke awaiting its echo. `None`
    /// between samples. Holds the OLDEST un-echoed key (type-ahead is conservative).
    key_pending: Option<Instant>,
    /// Set once the active tab has drained output while `key_pending` is armed —
    /// i.e. the echo has arrived — so the next present records it.
    echo_seen: bool,
    /// Last instant the active tab produced output, for the quiescent gate.
    last_active_output_at: Option<Instant>,
    /// key→frame-ready samples (ms): excludes vsync-acquire + GPU submit + scanout.
    lat_ready_ms: Vec<f32>,
    /// key→pre-present samples (ms): includes vsync-acquire + GPU submit.
    lat_present_ms: Vec<f32>,
    /// Sample count at the last emit, so `about_to_wait` reports each new batch once.
    reported: usize,
    /// Display refresh rate (Hz), captured at first frame; contextualises the vsync
    /// component of the pre-present number.
    refresh_hz: Option<f32>,

    // --- one-shot cold-start + idle-RSS ---
    /// True once the cold-start line has been emitted (guards every present path).
    pub first_frame_logged: bool,
    /// True once the idle-RSS line has been emitted (sampled once per session).
    pub idle_rss_logged: bool,
}

impl Perf {
    /// Construct from the environment: `on` iff `JETTY_PERF_LOG` is set.
    pub fn from_env() -> Self {
        Self::new(env_enabled(std::env::var_os("JETTY_PERF_LOG")))
    }

    /// Construct with an explicit on/off (used by `from_env` and by tests). When off,
    /// the sample buffers never allocate (`Vec::new` → capacity 0).
    pub fn new(on: bool) -> Self {
        Perf {
            on,
            key_pending: None,
            echo_seen: false,
            last_active_output_at: None,
            lat_ready_ms: if on { Vec::with_capacity(256) } else { Vec::new() },
            lat_present_ms: if on { Vec::with_capacity(256) } else { Vec::new() },
            reported: 0,
            refresh_hz: None,
            first_frame_logged: false,
            idle_rss_logged: false,
        }
    }

    /// Record a keystroke's PTY write. Arms latency capture ONLY when the active tab
    /// was quiescent (no recent output) and no earlier key is still awaiting its echo
    /// — so we measure the representative keypress→echo case, never a stream frame.
    #[inline]
    pub fn note_key_send(&mut self) {
        if !self.on || self.key_pending.is_some() {
            return;
        }
        if self.quiescent() {
            self.key_pending = Some(Instant::now());
            self.echo_seen = false;
        }
    }

    /// True iff the active tab has been quiescent for at least [`QUIESCENT_WINDOW`].
    fn quiescent(&self) -> bool {
        self.last_active_output_at
            .is_none_or(|t| t.elapsed() >= QUIESCENT_WINDOW)
    }

    /// Note that the active tab produced output this drain: refreshes the quiescent
    /// clock and, if a keystroke is armed, marks its echo as seen so the next present
    /// records the sample. Called from BOTH drain sites (Wake + RedrawRequested)
    /// because the echo is usually consumed by the Wake drain before the redraw.
    #[inline]
    pub fn note_active_output(&mut self) {
        if !self.on {
            return;
        }
        self.last_active_output_at = Some(Instant::now());
        if self.key_pending.is_some() {
            self.echo_seen = true;
        }
    }

    /// Elapsed ms since the armed keystroke's PTY write, but only once its echo has
    /// been drained. Peeks (does not consume) so the same pending key can be sampled
    /// at both the pre-acquire and pre-present stamps of one frame.
    #[inline]
    pub fn pending_elapsed_ms(&self) -> Option<f32> {
        if self.echo_seen {
            self.key_pending
                .map(|t0| t0.elapsed().as_secs_f32() * 1000.0)
        } else {
            None
        }
    }

    /// Record one latency sample (both stamps) and consume the pending keystroke.
    /// Called AFTER `frame.present()` so neither the push nor any emit perturbs the
    /// frame being timed (the elapsed values were captured before present).
    pub fn record_latency(&mut self, ready_ms: f32, present_ms: f32) {
        if !self.on {
            return;
        }
        self.key_pending = None;
        self.echo_seen = false;
        // Drop pathological outliers (occlusion/suspend/hang) — measured, not faked.
        if present_ms > STALE_MS {
            return;
        }
        if self.lat_present_ms.len() < SAMPLE_CAP {
            self.lat_ready_ms.push(ready_ms);
            self.lat_present_ms.push(present_ms);
        }
    }

    /// Emit a percentile summary if a new batch has accumulated. MUST be called off
    /// the timed present path (e.g. from `about_to_wait`) — never on the frame being
    /// measured.
    pub fn maybe_report(&mut self) {
        if !self.on {
            return;
        }
        let n = self.lat_present_ms.len();
        if n >= self.reported + REPORT_EVERY {
            self.reported = n;
            self.emit("input-latency");
        }
    }

    /// Sort clones of the accumulated samples and print min/p50/p99 for both stamps,
    /// with fully-qualified inclusion/exclusion labels and honest `n` + refresh rate.
    fn emit(&self, tag: &str) {
        let n = self.lat_present_ms.len();
        if n == 0 {
            return;
        }
        let mut ready = self.lat_ready_ms.clone();
        let mut present = self.lat_present_ms.clone();
        ready.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        present.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let hz = match self.refresh_hz {
            Some(h) => format!("{h:.1}Hz"),
            None => "?Hz".to_string(),
        };
        eprintln!(
            "jetty-perf: {tag} n={n} display={hz} (quiescent-prompt keystrokes only; \
             main window; excl. winit event-in)"
        );
        eprintln!(
            "  keypress→frame-ready  (incl. shell-echo round-trip; excl. vsync-acquire + GPU-submit + scanout):  \
             min {:.2} p50 {:.2} p99 {:.2} ms",
            ready.first().copied().unwrap_or(0.0),
            percentile(&ready, 50.0),
            percentile(&ready, 99.0),
        );
        eprintln!(
            "  keypress→pre-present  (+ vsync-acquire + GPU-submit; excl. scanout):  \
             min {:.2} p50 {:.2} p99 {:.2} ms",
            present.first().copied().unwrap_or(0.0),
            percentile(&present, 50.0),
            percentile(&present, 99.0),
        );
    }

    /// Emit the genuine exec→first-frame cold-start line exactly once, and latch the
    /// display refresh rate for the latency report. Idempotent — safe to call after
    /// every present. `refresh_hz` is the current monitor's refresh, if known.
    pub fn log_first_frame(&mut self, refresh_hz: Option<f32>) {
        if !self.on || self.first_frame_logged {
            return;
        }
        self.first_frame_logged = true;
        self.refresh_hz = refresh_hz;
        let (ms, basis) = cold_start(process_start());
        eprintln!("jetty-perf: cold-start {basis} = {ms:.1} ms");
    }

    /// Final flush on shutdown so a session shorter than one [`REPORT_EVERY`] batch
    /// still reports. Emits only if there are unreported samples.
    fn flush_final(&self) {
        if self.on && self.lat_present_ms.len() > self.reported {
            self.emit("input-latency (final)");
        }
    }
}

impl Drop for Perf {
    fn drop(&mut self) {
        self.flush_final();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_enabled_is_presence_only() {
        assert!(env_enabled(Some(OsString::from("1"))));
        assert!(env_enabled(Some(OsString::from("0")))); // presence, not truthiness
        assert!(env_enabled(Some(OsString::from(""))));
        assert!(!env_enabled(None));
    }

    #[test]
    fn percentile_on_known_sample() {
        // 1..=100 ascending.
        let v: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        assert!((percentile(&v, 0.0) - 1.0).abs() < 1e-3, "min");
        // Median of 1..=100 by linear interpolation ≈ 50.5.
        assert!((percentile(&v, 50.0) - 50.5).abs() < 0.6, "p50 ~ 50");
        // p99 ≈ 99, and crucially NOT the max (100).
        let p99 = percentile(&v, 99.0);
        assert!((p99 - 99.0).abs() < 0.6, "p99 ~ 99, got {p99}");
        assert!(p99 < 100.0, "p99 must not be the max");
    }

    #[test]
    fn percentile_n64_p99_is_not_the_max() {
        // The exact M5 regression: nearest-rank (n*99)/100 on 64 samples = index 63
        // = the max. Interpolation must return a blend below the max.
        let v: Vec<f32> = (1..=64).map(|i| i as f32).collect();
        let p99 = percentile(&v, 99.0);
        let max = *v.last().unwrap();
        assert!(p99 < max, "p99 ({p99}) must be below max ({max}) on n=64");
        assert!(p99 > v[61], "p99 should sit near the top ranks");
    }

    #[test]
    fn percentile_degenerate_lengths() {
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
    }

    #[test]
    fn cold_start_selects_a_basis_and_is_nonnegative() {
        let (ms, basis) = cold_start(Some(Instant::now()));
        assert!(ms >= 0.0, "cold-start ms must be non-negative, got {ms}");
        assert!(basis.contains("first-frame"), "basis must name the metric: {basis}");
        #[cfg(target_os = "linux")]
        assert!(
            basis.contains("/proc") || basis.contains("main()"),
            "linux basis should be /proc (or the labelled fallback): {basis}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_exec_time_is_plausible() {
        // The current test process has a real /proc entry; its age is a small,
        // finite, NON-NEGATIVE number of ms (a just-spawned test binary can round to
        // ~0 — see the clamp in linux_exec_to_now_ms — so assert >= 0, not > 0).
        let ms = linux_exec_to_now_ms().expect("/proc exec time on linux");
        assert!((0.0..3_600_000.0).contains(&ms), "implausible process age: {ms} ms");
    }

    #[test]
    fn off_is_zero_cost_and_inert() {
        let mut p = Perf::new(false);
        assert!(!p.on);
        assert_eq!(p.lat_ready_ms.capacity(), 0, "no allocation when off");
        assert_eq!(p.lat_present_ms.capacity(), 0, "no allocation when off");
        // Every stamp fn is a no-op when off.
        p.note_active_output();
        p.note_key_send();
        assert!(p.key_pending.is_none(), "note_key_send must not arm when off");
        assert!(p.last_active_output_at.is_none(), "no clock writes when off");
        assert!(p.pending_elapsed_ms().is_none());
        p.record_latency(1.0, 2.0);
        assert!(p.lat_present_ms.is_empty(), "record must not push when off");
        p.log_first_frame(Some(60.0));
        assert!(!p.first_frame_logged, "no first-frame log when off");
    }

    #[test]
    fn current_rss_is_plausible_nonzero() {
        let rss = current_rss_bytes().expect("RSS available for the test process");
        assert!(rss > 0, "RSS must be nonzero");
        // If memory() returned KiB (old sysinfo) or pages this would be implausibly
        // small; if some other unit, implausibly huge. Bytes for a real process are
        // comfortably in (64 KiB, 100 GiB).
        assert!(rss > 64 * 1024, "RSS ({rss} B) implausibly small — wrong unit?");
        assert!(rss < 100 * 1024 * 1024 * 1024, "RSS ({rss} B) implausibly large");
    }

    #[test]
    fn quiescent_gate_arms_only_at_a_quiet_prompt() {
        let mut p = Perf::new(true);
        // Cold: no prior output → quiescent → arms.
        p.note_key_send();
        assert!(p.key_pending.is_some(), "arms at a quiescent prompt");
        // A second key while one is pending keeps the OLDEST (conservative).
        let first = p.key_pending;
        p.note_key_send();
        assert_eq!(p.key_pending, first, "does not overwrite an un-echoed key");
    }

    #[test]
    fn streaming_tab_is_not_sampled() {
        let mut p = Perf::new(true);
        // Active tab just produced output → NOT quiescent → do not arm.
        p.note_active_output();
        p.note_key_send();
        assert!(
            p.key_pending.is_none(),
            "a keystroke during streaming output must not be sampled"
        );
    }

    #[test]
    fn echo_then_record_produces_a_sample() {
        let mut p = Perf::new(true);
        p.note_key_send(); // arm at quiet prompt
        assert!(p.pending_elapsed_ms().is_none(), "no echo yet → nothing to record");
        p.note_active_output(); // echo drained
        let ready = p.pending_elapsed_ms().expect("echo seen → measurable");
        p.record_latency(ready, ready + 1.0);
        assert_eq!(p.lat_present_ms.len(), 1);
        assert!(p.key_pending.is_none(), "sample consumed the pending key");
        assert!(!p.echo_seen);
    }

    #[test]
    fn record_drops_stale_outliers() {
        let mut p = Perf::new(true);
        p.record_latency(1.0, STALE_MS + 1.0);
        assert!(p.lat_present_ms.is_empty(), "multi-second outliers are dropped");
    }
}
