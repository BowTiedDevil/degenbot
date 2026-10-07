//! Engine-session end detection.
//!
//! The core owns *why* a pump session ended; a driver owns how that fact is
//! ranked against its own outcomes, what it cancels, and when it tears down.
//! [`SessionEndCause`] is the one fact vocabulary, fed by the pump-completion
//! surface ([`EngineDriver::wait_session_end`]) or the heartbeat stall watchdog,
//! and [`SessionEndFacts`] is its once-only delivery channel: the first fact
//! published is retained for every current or future waiter and later
//! publications are dropped.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::driver::EngineDriver;

/// Why a pump session ended — the core's one detection fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionEndCause {
    /// The pump-completion surface resolved: the pump ended on its own (stream
    /// end, normal return, abort, or panic) outside `stop()`.
    PumpFinished,
    /// No consume-loop beat arrived within the stall window.
    StallWatchdogTripped,
}

impl SessionEndCause {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PumpFinished => "PumpFinished",
            Self::StallWatchdogTripped => "StallWatchdogTripped",
        }
    }
}

/// A monotonic heartbeat counter a consumer beats per consumed batch.
#[derive(Clone, Debug, Default)]
pub struct Heartbeat {
    seq: Arc<AtomicU64>,
}

impl Heartbeat {
    /// A fresh heartbeat at sequence 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one beat (one consumed batch).
    pub fn beat(&self) {
        self.seq.fetch_add(1, Ordering::Relaxed);
    }

    /// The current beat sequence.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

/// Whether a heartbeat window is stalled: no beat since the last observation.
#[must_use]
pub const fn stalled(last_seq: u64, now_seq: u64) -> bool {
    now_seq == last_seq
}

/// Resolve `StallWatchdogTripped` once no heartbeat arrives within
/// `stall_after`.
pub async fn stall_watchdog(heartbeat: Heartbeat, stall_after: Duration) -> SessionEndCause {
    let mut last = heartbeat.seq();
    loop {
        tokio::time::sleep(stall_after).await;
        let now = heartbeat.seq();
        if stalled(last, now) {
            return SessionEndCause::StallWatchdogTripped;
        }
        last = now;
    }
}

/// The publication half of [`SessionEndFacts`]' once-only channel.
#[derive(Clone, Debug)]
pub struct SessionEndPublisher {
    tx: Arc<watch::Sender<Option<SessionEndCause>>>,
}

impl SessionEndPublisher {
    /// Publish a detection fact. Idempotent: the FIRST cause wins and later
    /// publications are dropped, so a detection race still yields exactly one
    /// fact to every waiter.
    pub fn publish(&self, cause: SessionEndCause) {
        let _ = self.tx.send_if_modified(|slot| {
            if slot.is_some() {
                false
            } else {
                *slot = Some(cause);
                true
            }
        });
    }
}

/// The observation half: awaits the ONE detection fact, however many waiters
/// exist. Retains the published fact for waiters that arrive late.
#[derive(Clone, Debug)]
pub struct SessionEndFacts {
    rx: watch::Receiver<Option<SessionEndCause>>,
    /// Holds a channel end for the facts' lifetime so a waiter never observes a
    /// closed channel instead of the retained fact.
    _keepalive: Arc<watch::Sender<Option<SessionEndCause>>>,
}

impl SessionEndFacts {
    /// A detection channel with no fact delivered yet.
    #[must_use]
    pub fn channel() -> (SessionEndPublisher, Self) {
        let (tx, rx) = watch::channel(None);
        let tx = Arc::new(tx);
        (
            SessionEndPublisher {
                tx: Arc::clone(&tx),
            },
            Self { rx, _keepalive: tx },
        )
    }

    /// The delivered fact, if any.
    #[must_use]
    pub fn current(&self) -> Option<SessionEndCause> {
        *self.rx.borrow()
    }

    /// Await the one detection fact, resolving immediately when it was already
    /// delivered.
    pub async fn wait(&self) -> SessionEndCause {
        let mut rx = self.rx.clone();
        loop {
            if let Some(cause) = *rx.borrow_and_update() {
                return cause;
            }
            // The keepalive sender keeps this from erroring; a wakeup only ever
            // means a fact (or a redundant republish the latch dropped).
            let _ = rx.changed().await;
        }
    }
}

/// Armed detection: owns the watchdog task that feeds a [`SessionEndFacts`]
/// channel and aborts it on drop.
#[derive(Debug)]
pub struct SessionEndDetection {
    facts: SessionEndFacts,
    watchdog: JoinHandle<()>,
}

impl SessionEndDetection {
    /// Arm detection over an arbitrary cause-producing future.
    ///
    /// # Cancel safety
    ///
    /// **Cancel-safe**: this function is NOT async (it takes a future, it
    /// does not await one) — it performs no suspension itself, so it can never
    /// be "cancelled mid-flight". It spawns a watchdog task that awaits
    /// `future` and publishes the resulting [`SessionEndCause`] into this
    /// detection's [`SessionEndFacts`] channel; that task's handle is stored
    /// in the returned `SessionEndDetection`.
    ///
    /// **Teardown is Drop-driven**: the detection's `Drop` impl aborts the
    /// stored watchdog (`self.watchdog.abort()`), so dropping the detection is
    /// the abort path — the watchdog cannot outlive its detection and publish
    /// a stale fact after teardown. This is the fence: the watchdog is owned
    /// by the detection and nothing else may abort or detach it, and dropping
    /// the `SessionEndFacts` handle is harmless because the source's
    /// `_keepalive` sender keeps the channel open (a waiter observes the
    /// retained fact, never a spurious close).
    pub fn from_future<F>(future: F) -> Self
    where
        F: Future<Output = SessionEndCause> + Send + 'static,
    {
        let (publisher, facts) = SessionEndFacts::channel();
        let watchdog = tokio::spawn(async move {
            publisher.publish(future.await);
        });
        Self { facts, watchdog }
    }

    /// Arm stall detection over `heartbeat`: publishes
    /// [`SessionEndCause::StallWatchdogTripped`] once no beat arrives within
    /// `stall_after`.
    #[must_use]
    pub fn stall(heartbeat: Heartbeat, stall_after: Duration) -> Self {
        Self::from_future(stall_watchdog(heartbeat, stall_after))
    }

    /// Arm pump-finished detection over the driver's completion surface.
    #[must_use]
    pub fn pump_finished(driver: Arc<EngineDriver>) -> Self {
        Self::from_future(async move { driver.wait_session_end().await })
    }

    /// The facts channel this detection feeds.
    #[must_use]
    pub fn facts(&self) -> &SessionEndFacts {
        &self.facts
    }
}

impl Drop for SessionEndDetection {
    fn drop(&mut self) {
        self.watchdog.abort();
    }
}

impl EngineDriver {
    /// Await the session's end as the core detection fact — the public typed
    /// wrapper over the driver's internal pump-completion wait.
    pub async fn wait_session_end(&self) -> SessionEndCause {
        self.wait_pump_finished().await;
        SessionEndCause::PumpFinished
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    #[test]
    fn cause_names_are_stable() {
        assert_eq!(SessionEndCause::PumpFinished.name(), "PumpFinished");
        assert_eq!(
            SessionEndCause::StallWatchdogTripped.name(),
            "StallWatchdogTripped"
        );
    }

    #[test]
    fn stall_decision_is_a_pure_predicate() {
        assert!(stalled(3, 3));
        assert!(!stalled(3, 4));
    }

    #[tokio::test(start_paused = true)]
    async fn stall_watchdog_trips_after_a_stall() {
        let heartbeat = Heartbeat::new();
        let handle = tokio::spawn(stall_watchdog(heartbeat, Duration::from_secs(1)));
        tokio::time::advance(Duration::from_millis(1_100)).await;
        assert_eq!(handle.await.unwrap(), SessionEndCause::StallWatchdogTripped);
    }

    #[tokio::test(start_paused = true)]
    async fn stall_watchdog_is_reset_by_heartbeats() {
        let heartbeat = Heartbeat::new();
        let handle = tokio::spawn(stall_watchdog(heartbeat.clone(), Duration::from_secs(1)));
        for _ in 0..5 {
            tokio::time::advance(Duration::from_millis(500)).await;
            heartbeat.beat();
            tokio::task::yield_now().await;
        }
        assert!(!handle.is_finished(), "beats must reset the stall clock");
        tokio::time::advance(Duration::from_millis(1_100)).await;
        assert_eq!(handle.await.unwrap(), SessionEndCause::StallWatchdogTripped);
    }

    #[tokio::test(start_paused = true)]
    async fn armed_stall_detection_publishes_the_stall_fact() {
        let detection = SessionEndDetection::stall(Heartbeat::new(), Duration::from_secs(1));
        let facts = detection.facts().clone();
        tokio::time::advance(Duration::from_millis(1_100)).await;
        assert_eq!(facts.wait().await, SessionEndCause::StallWatchdogTripped);
    }

    #[tokio::test]
    async fn a_fact_is_delivered_once() {
        let (publisher, facts) = SessionEndFacts::channel();
        assert_eq!(facts.current(), None);
        publisher.publish(SessionEndCause::PumpFinished);
        // A later cause never displaces the first.
        publisher.publish(SessionEndCause::StallWatchdogTripped);
        assert_eq!(facts.current(), Some(SessionEndCause::PumpFinished));
        // Every waiter, current or late, observes the one retained fact.
        let late = facts.clone();
        assert_eq!(facts.wait().await, SessionEndCause::PumpFinished);
        assert_eq!(late.wait().await, SessionEndCause::PumpFinished);
    }

    #[tokio::test]
    async fn detection_from_a_future_publishes_its_cause() {
        let detection = SessionEndDetection::from_future(async { SessionEndCause::PumpFinished });
        assert_eq!(
            detection.facts().wait().await,
            SessionEndCause::PumpFinished
        );
    }
}
