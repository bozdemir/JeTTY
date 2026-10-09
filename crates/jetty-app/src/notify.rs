//! "Command finished" desktop notifications (v0.15 Run & Notify).
//!
//! The blocking D-Bus round trip runs on ONE long-lived worker thread, fed by a
//! BOUNDED channel: the UI thread only ever `try_send`s (never blocks), and a
//! slow/absent daemon can neither stall the event loop nor grow the queue —
//! overflow is dropped (the winit taskbar-urgency baseline still informs the
//! user). See `v015-amendments.md` §5.
//!
//! DE-independence: on Linux/BSD the `notify-rust` `z` (pure-Rust `zbus`) backend
//! talks to whatever freedesktop notification daemon is running (KDE, GNOME,
//! dunst, mako, swaync, …) over `org.freedesktop.Notifications`. NO KDE/GNOME-
//! specific API. On macOS `notify-rust` is NOT a dependency (its ObjC backend is
//! suppressed for a non-bundled binary and needs a `.app` bundle — future), so
//! `show()` is a no-op there and the guaranteed macOS signal is the winit
//! dock-bounce urgency fired on the UI thread by `app.rs`, not this module.
//!
//! PTY-child safety: the `z` backend pulls `zbus → async-io/async-process`, but
//! `async-process`'s reaper is behind a `OnceLock` that is only initialized when
//! it actually spawns a child. `zbus` spawns nothing when a session bus address
//! is set (always, in a desktop session), and even if it did, on Linux ≥5.3 the
//! reaper uses per-child pidfds — never a global `SIGCHLD` handler. So no PTY
//! child's exit status can be stolen (verified against async-process 2.5).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// One notification for the worker to fire.
pub enum NotifyMsg {
    Fire {
        /// Notification title — names the firing tab + status + duration.
        summary: String,
        /// Notification body — the command's last output line (may be empty).
        body: String,
    },
}

/// Bounded queue depth. A wedged daemon backs the worker up to at most this many
/// pending toasts; further `fire()`s are dropped rather than blocking the UI
/// thread or growing without bound (amendments §5b).
const NOTIFY_QUEUE_BOUND: usize = 16;

/// Max delivery threads alive at once. A daemon that ACCEPTS the D-Bus Notify
/// call but never replies (zbus applies no default reply timeout) would strand
/// the delivering thread forever; we cap how many such strands can accumulate so
/// a long hidden session under a pathological daemon can't leak threads without
/// bound. Beyond the cap, further toasts are shed (the winit urgency still fired).
const MAX_INFLIGHT_SENDS: usize = 4;

/// A cheap, clonable handle to the notification worker. `fire()` is non-blocking.
#[derive(Clone)]
pub struct Notifier {
    tx: SyncSender<NotifyMsg>,
}

impl Notifier {
    /// Queue a notification. NEVER blocks the caller (the UI thread): on a full
    /// queue or a dead worker the message is simply dropped — the winit urgency
    /// hint has already informed the user, so a lost toast is harmless.
    pub fn fire(&self, summary: String, body: String) {
        match self.tx.try_send(NotifyMsg::Fire { summary, body }) {
            // Sent, queue full (drop), or worker gone (drop) — all non-fatal.
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Spawn the long-lived notification worker thread and return a `Notifier`.
///
/// The worker blocks on `recv` (the reactor thread `zbus` later starts idles at
/// epoll-wait — ~0% idle, no busy loop) and exits when the last `Notifier` — held
/// by the `App` — drops. The blocking `show()` runs on a short-lived delivery
/// thread so a slow daemon never stalls the worker; an `AtomicUsize` caps how many
/// deliveries can be in flight at once, so a daemon that accepts-but-never-replies
/// can strand at most `MAX_INFLIGHT_SENDS` threads (then toasts shed) instead of
/// leaking one per completion for the life of the process.
pub fn spawn_notifier() -> Notifier {
    let (tx, rx) = sync_channel::<NotifyMsg>(NOTIFY_QUEUE_BOUND);
    // If the thread fails to spawn, `fire()` still degrades cleanly: sends land on
    // a live channel whose receiver is gone → dropped (the urgency hint remains).
    let inflight = Arc::new(AtomicUsize::new(0));
    let _ = std::thread::Builder::new()
        .name("jetty-notify".into())
        .spawn(move || {
            for msg in rx {
                let NotifyMsg::Fire { summary, body } = msg;
                start_delivery(
                    &inflight,
                    move || show(&summary, &body),
                    |job| std::thread::Builder::new().name("jetty-notify-send".into()).spawn(job).map(drop),
                );
            }
        });
    Notifier { tx }
}

/// Run `deliver` on a thread of its own (`spawn` starts it), holding one of
/// the `MAX_INFLIGHT_SENDS` slots in `inflight` until it returns — or shed it
/// (false) when every slot is taken: deliveries stranded by a daemon that never
/// replies. A thread that cannot be started (out of threads or memory) gives
/// its slot straight back: four such failures used to shed every later toast.
fn start_delivery(
    inflight: &Arc<AtomicUsize>,
    deliver: impl FnOnce() + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> bool {
    if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT_SENDS {
        return false;
    }
    inflight.fetch_add(1, Ordering::Relaxed);
    let slot = Arc::clone(inflight);
    let started = spawn(Box::new(move || {
        deliver();
        slot.fetch_sub(1, Ordering::Relaxed);
    }));
    if started.is_err() {
        inflight.fetch_sub(1, Ordering::Relaxed);
    }
    started.is_ok()
}

/// Escape `& < >` so text is shown verbatim by a server that parses the body
/// as markup — a command's output line like `Vec<String>` would otherwise lose
/// `<String>`, a bare `&` be mangled, and hostile output inject a clickable
/// `<a href>` into the toast.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn escape_markup(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['&', '<', '>']) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Whether the notification server renders the body as markup (it advertises
/// `body-markup` — Plasma, GNOME Shell, dunst, mako do). Asked ONCE, on a
/// delivery thread; a server that can't be asked is assumed to (escaping text
/// for a plain-text server only shows `&amp;`; not escaping for a markup one
/// lets output inject links).
#[cfg(all(unix, not(target_os = "macos")))]
fn body_is_markup() -> bool {
    static MARKUP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MARKUP.get_or_init(|| {
        notify_rust::get_capabilities()
            .map(|caps| caps.iter().any(|c| c == "body-markup"))
            .unwrap_or(true)
    })
}

/// Linux/BSD: full freedesktop toast via the pure-Rust zbus backend.
#[cfg(all(unix, not(target_os = "macos")))]
fn show(summary: &str, body: &str) {
    use notify_rust::{Hint, Notification, Urgency};
    // The summary is plain text by spec; only the body may be parsed as markup.
    let body = if body_is_markup() { escape_markup(body) } else { std::borrow::Cow::Borrowed(body) };
    // Fire-and-forget: the returned handle is dropped (no action callbacks), so
    // we never wait on a click. A slow daemon blocks only THIS worker; zbus'
    // own method-call timeout unwedges it and the bounded queue sheds load
    // meanwhile. Errors (no daemon) are swallowed. Timeout stays the default
    // (`Timeout::Default`) so the DAEMON owns the expiry — a fire-and-hide ping
    // never leaves a sticky bubble (amendments §5c). That is also why a FAILED
    // command stays `Normal`: the spec says a `Critical` notification never
    // expires (Plasma, GNOME, dunst keep it until dismissed) and it breaks
    // through Do Not Disturb. The summary already reads "failed (exit N)".
    let _ = Notification::new()
        .appname("JeTTY")
        .summary(summary)
        .body(&body)
        .icon("jetty")
        .hint(Hint::Urgency(Urgency::Normal))
        .show();
}

/// macOS (and any non-freedesktop platform): no-op. `notify-rust` is not a
/// dependency here — a non-bundled binary's ObjC toast is suppressed/mis-
/// attributed anyway, and full macOS toasts need a `.app` bundle (future). The
/// guaranteed macOS signal is the winit dock-bounce urgency fired by `app.rs`.
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn show(_summary: &str, _body: &str) {}

/// Short floor for FAILURE notifications that carry a KNOWN duration: an instant
/// typo (`cd /nope`, exit 1, sub-second) stays silent even when you're not
/// looking, while a failure whose duration is UNKNOWN (plain bash) or that ran
/// past this floor still pings.
const FAILURE_FLOOR: Duration = Duration::from_secs(1);

/// Pure notification-gating decision (no winit — table-tested). Fire iff ALL of:
///   * the user is NOT looking at the firing window (`!user_watching`), AND
///   * this tab hasn't fired within `anti_spam_gap` (PER-tab; `since_last == None`
///     means it never has), AND
///   * either the command ran ≥ `min_secs`, OR it FAILED with a duration that is
///     unknown or ≥ `FAILURE_FLOOR`.
///
/// `only_on_failure` drops the "long success" arm (fires on qualifying failures
/// only). See amendments §5. `min_secs` is compared in whole seconds so a `9.8s`
/// command doesn't trip a `10s` threshold.
pub fn should_notify(
    user_watching: bool,
    duration: Option<Duration>,
    exit_code: Option<i32>,
    min_secs: u64,
    only_on_failure: bool,
    since_last: Option<Duration>,
    anti_spam_gap: Duration,
) -> bool {
    // Never ping while the user is already looking at the firing window.
    if user_watching {
        return false;
    }
    // Per-tab anti-spam: a tab that just fired stays quiet for the gap. A DIFFERENT
    // tab is unaffected (the caller keys `since_last` per tab), so tab 3's finish
    // is never suppressed by tab 1's recent notification (amendments §2).
    if matches!(since_last, Some(since) if since < anti_spam_gap) {
        return false;
    }
    let failed = matches!(exit_code, Some(c) if c != 0);
    let long_enough = matches!(duration, Some(d) if d.as_secs() >= min_secs);
    // A failure is worth a ping unless it was fast AND its duration is known
    // (instant typo). Unknown duration (plain bash) always qualifies — that's the
    // documented failure-only fallback for shells that emit no C mark.
    let failure_worth = failed && duration.is_none_or(|d| d >= FAILURE_FLOOR);
    if only_on_failure {
        failure_worth
    } else {
        long_enough || failure_worth
    }
}

/// Human-readable duration for the notification summary: `5s`, `1m 12s`, `1h 1m`.
pub fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: Duration = Duration::from_secs(2);
    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    #[test]
    fn watching_never_fires() {
        assert!(!should_notify(true, secs(60), Some(0), 10, false, None, GAP));
        assert!(!should_notify(true, None, Some(1), 10, false, None, GAP)); // even a failure
    }

    #[test]
    fn hidden_long_success_fires() {
        assert!(should_notify(false, secs(30), Some(0), 10, false, None, GAP));
    }

    #[test]
    fn hidden_short_success_is_silent() {
        assert!(!should_notify(false, secs(3), Some(0), 10, false, None, GAP));
    }

    #[test]
    fn hidden_short_failure_with_known_subsecond_duration_is_silent() {
        // Instant typo: exit 1, 0s known duration → below FAILURE_FLOOR → silent.
        assert!(!should_notify(false, secs(0), Some(1), 10, false, None, GAP));
    }

    #[test]
    fn hidden_failure_past_floor_fires() {
        // A failure that ran ≥ FAILURE_FLOOR (but < min_secs) still pings.
        assert!(should_notify(false, secs(2), Some(1), 10, false, None, GAP));
    }

    #[test]
    fn unknown_duration_failure_fires_success_does_not() {
        // Plain bash: duration unknown. Failure pings (failure-only fallback);
        // a successful command with unknown duration cannot pass the threshold.
        assert!(should_notify(false, None, Some(1), 10, false, None, GAP));
        assert!(!should_notify(false, None, Some(0), 10, false, None, GAP));
        assert!(!should_notify(false, None, None, 10, false, None, GAP));
    }

    #[test]
    fn only_on_failure_drops_long_success() {
        assert!(!should_notify(false, secs(300), Some(0), 10, true, None, GAP));
        assert!(should_notify(false, secs(300), Some(1), 10, true, None, GAP));
        assert!(should_notify(false, None, Some(7), 10, true, None, GAP)); // bash failure
    }

    #[test]
    fn per_tab_anti_spam_suppresses_within_gap_only() {
        // Within the gap for THIS tab → suppressed; past it → fires.
        assert!(!should_notify(
            false, secs(30), Some(0), 10, false, Some(Duration::from_secs(1)), GAP
        ));
        assert!(should_notify(
            false, secs(30), Some(0), 10, false, Some(Duration::from_secs(3)), GAP
        ));
        // A tab that never fired (None) is not suppressed.
        assert!(should_notify(false, secs(30), Some(0), 10, false, None, GAP));
    }

    #[test]
    fn escape_markup_shows_output_lines_verbatim() {
        assert!(matches!(escape_markup("cargo build finished"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(escape_markup("Vec<String> & co"), "Vec&lt;String&gt; &amp; co");
        assert_eq!(
            escape_markup("<a href=\"https://evil\">click</a>"),
            "&lt;a href=\"https://evil\"&gt;click&lt;/a&gt;"
        );
        assert_eq!(escape_markup("&amp;"), "&amp;amp;", "pre-escaped text stays literal");
    }

    #[test]
    fn fmt_duration_shapes() {
        assert_eq!(fmt_duration(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_duration(Duration::from_secs(72)), "1m 12s");
        assert_eq!(fmt_duration(Duration::from_secs(3661)), "1h 1m");
        assert_eq!(fmt_duration(Duration::from_secs(600)), "10m 0s");
    }

    #[test]
    fn fire_never_blocks_or_panics_when_queue_full_or_worker_gone() {
        // fire()'s contract: on a full queue OR a dead worker it drops the message
        // and returns immediately (never blocks, never panics). Verified WITHOUT the
        // real worker so `cargo test` NEVER delivers a desktop notification — the
        // real-delivery path is exercised only by the #[ignore]-d smoke test below.
        // (Regression guard: a plain `spawn_notifier()` here used to spam real
        // "t0/t1/…" toasts to the developer's desktop on every test run.)
        // Worker gone: receiver dropped → every send is Disconnected, must not panic.
        let (tx, rx) = sync_channel::<NotifyMsg>(NOTIFY_QUEUE_BOUND);
        drop(rx);
        let n = Notifier { tx };
        for i in 0..1000 {
            n.fire(format!("t{i}"), String::new());
        }
        // Queue full: live but never-drained receiver → overflow is dropped, no block.
        let (tx, _rx) = sync_channel::<NotifyMsg>(NOTIFY_QUEUE_BOUND);
        let n = Notifier { tx };
        for i in 0..1000 {
            n.fire(format!("t{i}"), String::new());
        }
    }

    #[test]
    fn a_delivery_thread_that_cannot_start_gives_its_slot_back() {
        // Nothing here reaches the desktop: `deliver` is a no-op and `spawn`
        // either fails (EAGAIN: out of threads) or hands the job back to us.
        let inflight = Arc::new(AtomicUsize::new(0));
        for _ in 0..MAX_INFLIGHT_SENDS * 2 {
            assert!(!start_delivery(&inflight, || {}, |_| Err(std::io::Error::other("EAGAIN"))));
        }
        assert_eq!(inflight.load(Ordering::Relaxed), 0, "every failed start gave its slot back");
        // Started deliveries hold their slots until they return; past the cap
        // the next one is shed.
        let mut jobs = Vec::new();
        for _ in 0..MAX_INFLIGHT_SENDS {
            assert!(start_delivery(&inflight, || {}, |job| {
                jobs.push(job);
                Ok(())
            }));
        }
        assert!(!start_delivery(&inflight, || {}, |_| panic!("shed: never started")));
        jobs.pop().unwrap()();
        assert_eq!(inflight.load(Ordering::Relaxed), MAX_INFLIGHT_SENDS - 1, "a finished delivery frees its slot");
        assert!(start_delivery(&inflight, || {}, |_| Ok(())));
    }

    #[test]
    #[ignore = "delivers a REAL desktop notification; run manually: \
                cargo test -p jetty-app --lib notify::tests::smoke -- --ignored --nocapture"]
    fn smoke_delivers_a_real_notification() {
        // Manual verification that the worker reaches the freedesktop daemon.
        let n = spawn_notifier();
        n.fire(
            "Tab 2 · cargo — finished · 1m 12s".to_string(),
            "Compiling jetty-app v0.15.0".to_string(),
        );
        n.fire(
            "Tab 3 · make — failed (exit 2) · 8s".to_string(),
            "make: *** [all] Error 2".to_string(),
        );
        // Give the worker time to complete the blocking D-Bus round trip.
        std::thread::sleep(Duration::from_millis(800));
    }
}
