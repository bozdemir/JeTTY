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

use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::time::Duration;

/// One notification for the worker to fire.
pub enum NotifyMsg {
    Fire {
        /// Notification title — names the firing tab + status + duration.
        summary: String,
        /// Notification body — the command's last output line (may be empty).
        body: String,
        /// A failed command: raises urgency to `Critical` (freedesktop) so the
        /// daemon surfaces it prominently.
        critical: bool,
    },
}

/// Bounded queue depth. A wedged daemon backs the worker up to at most this many
/// pending toasts; further `fire()`s are dropped rather than blocking the UI
/// thread or growing without bound (amendments §5b).
const NOTIFY_QUEUE_BOUND: usize = 16;

/// Hard ceiling on how long the worker waits for ONE `show()` (the blocking
/// D-Bus round trip). zbus applies no default reply timeout and notify-rust owns
/// its connection, so a daemon that ACCEPTS the call but never replies would
/// otherwise wedge the worker forever. We run each delivery on a throwaway thread
/// and abandon it after this deadline — the strand dies with the process; the
/// NEXT notification still fires (amendments §5b).
const NOTIFY_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// A cheap, clonable handle to the notification worker. `fire()` is non-blocking.
#[derive(Clone)]
pub struct Notifier {
    tx: SyncSender<NotifyMsg>,
}

impl Notifier {
    /// Queue a notification. NEVER blocks the caller (the UI thread): on a full
    /// queue or a dead worker the message is simply dropped — the winit urgency
    /// hint has already informed the user, so a lost toast is harmless.
    pub fn fire(&self, summary: String, body: String, critical: bool) {
        match self.tx.try_send(NotifyMsg::Fire { summary, body, critical }) {
            // Sent, queue full (drop), or worker gone (drop) — all non-fatal.
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Spawn the long-lived notification worker thread and return a `Notifier`.
///
/// The worker blocks on `recv` (the reactor thread `zbus` later starts idles at
/// epoll-wait — ~0% idle, no busy loop) and exits when the last `Notifier` — held
/// by the `App` — drops. One reused worker (vs. a thread per toast) bounds
/// resource use under spam and preserves ordering.
pub fn spawn_notifier() -> Notifier {
    let (tx, rx) = sync_channel::<NotifyMsg>(NOTIFY_QUEUE_BOUND);
    // If the thread fails to spawn, `fire()` still degrades cleanly: sends land on
    // a live channel whose receiver is gone → dropped (the urgency hint remains).
    let _ = std::thread::Builder::new()
        .name("jetty-notify".into())
        .spawn(move || {
            for msg in rx {
                let NotifyMsg::Fire { summary, body, critical } = msg;
                // Deliver on a throwaway thread and wait at most NOTIFY_CALL_TIMEOUT.
                // In the normal case (fast daemon) the delivery finishes in a few ms
                // and `recv_timeout` returns immediately; a wedged daemon strands
                // only that one thread (harmless — it completes or dies with the
                // process) and the worker moves on to the next message. The upstream
                // bounded queue + per-tab anti-spam keep the spawn rate low, so this
                // is not the resource-hungry "thread per notification" case.
                let (done_tx, done_rx) = sync_channel::<()>(1);
                let _ = std::thread::Builder::new()
                    .name("jetty-notify-send".into())
                    .spawn(move || {
                        show(&summary, &body, critical);
                        let _ = done_tx.try_send(());
                    });
                let _ = done_rx.recv_timeout(NOTIFY_CALL_TIMEOUT);
            }
        });
    Notifier { tx }
}

/// Linux/BSD: full freedesktop toast via the pure-Rust zbus backend.
#[cfg(all(unix, not(target_os = "macos")))]
fn show(summary: &str, body: &str, critical: bool) {
    use notify_rust::{Hint, Notification, Urgency};
    // Fire-and-forget: the returned handle is dropped (no action callbacks), so
    // we never wait on a click. A slow daemon blocks only THIS worker; zbus'
    // own method-call timeout unwedges it and the bounded queue sheds load
    // meanwhile. Errors (no daemon) are swallowed. Timeout stays the default
    // (`Timeout::Default`) so the DAEMON owns the expiry — a fire-and-hide ping
    // never leaves a sticky bubble (amendments §5c).
    let _ = Notification::new()
        .appname("JeTTY")
        .summary(summary)
        .body(body)
        .icon("jetty")
        .hint(Hint::Urgency(if critical { Urgency::Critical } else { Urgency::Normal }))
        .show();
}

/// macOS (and any non-freedesktop platform): no-op. `notify-rust` is not a
/// dependency here — a non-bundled binary's ObjC toast is suppressed/mis-
/// attributed anyway, and full macOS toasts need a `.app` bundle (future). The
/// guaranteed macOS signal is the winit dock-bounce urgency fired by `app.rs`.
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn show(_summary: &str, _body: &str, _critical: bool) {}

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
    fn fmt_duration_shapes() {
        assert_eq!(fmt_duration(Duration::from_secs(5)), "5s");
        assert_eq!(fmt_duration(Duration::from_secs(72)), "1m 12s");
        assert_eq!(fmt_duration(Duration::from_secs(3661)), "1h 1m");
        assert_eq!(fmt_duration(Duration::from_secs(600)), "10m 0s");
    }

    #[test]
    fn spawn_notifier_send_does_not_block_or_panic() {
        // The worker + non-blocking fire() path: many rapid fires must return
        // immediately (bounded queue drops overflow) and never panic, whether or
        // not a daemon is present.
        let n = spawn_notifier();
        for i in 0..100 {
            n.fire(format!("t{i}"), String::new(), i % 2 == 0);
        }
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
            false,
        );
        n.fire(
            "Tab 3 · make — failed (exit 2) · 8s".to_string(),
            "make: *** [all] Error 2".to_string(),
            true,
        );
        // Give the worker time to complete the blocking D-Bus round trip.
        std::thread::sleep(Duration::from_millis(800));
    }
}
