//! Engine-session end detection and the pure Session-end ranking.
//!
//! The core owns *why* a pump session ended AND how the gathered end facts
//! rank: [`SessionEndCause`] is the detection vocabulary, fed by the
//! pump-completion surface ([`EngineDriver::wait_session_end`]) or the heartbeat
//! stall watchdog, and [`SessionEndFacts`] is its once-only delivery channel:
//! the first fact published is retained for every current or future waiter and
//! later publications are dropped.
//!
//! # Session-end ranking
//!
//! [`rank`] is the ONE fold over one closed [`SessionEndObservation`] — the
//! detection cause, the translated consumer outcome, the registration outcome,
//! and the consumer's already-retrieved output — into one
//! [`SessionEndVerdict`], one [`RaiseSlot`], and the retained output. Task
//! completion order can never re-author the verdict: the adapters gather the
//! facts, translate their own surface into this vocabulary (the
//! `degenbot-batch-executor` `ConsumerEnd` translation lives at that crate /
//! the adapters — this module never names it), and render the ranked product.
//! Registration failure is maximal; an intended consumer end disarms a
//! simultaneous detection fact; the fold is total over every input
//! combination.
//!
//! Adapters own the watch-set, cancellation/reaping, and rendering; this
//! module owns no tasks, no cancellation, and no display.

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

    /// Parse the stable spelling back into the cause (`None` for any other
    /// name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "PumpFinished" => Some(Self::PumpFinished),
            "StallWatchdogTripped" => Some(Self::StallWatchdogTripped),
            _ => None,
        }
    }
}

/// The consumer's outcome at session end — the ADAPTER's translation of its
/// own surface into the core's one vocabulary.
///
/// The core never names `degenbot-batch-executor::ConsumerEnd`: that crate (or
/// the driver adapter) translates `QuietEnd` / `PumpStopped` at its own
/// boundary, and a Python consumer's report or task fate is translated at the
/// `PyO3` seam. A consumer task that panicked or raised lands [`Self::Failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConsumerOutcome {
    /// The consumer ended where the session intended (the `QuietEnd`
    /// translation). The expected close publishes `PumpFinished` too, so this
    /// outcome disarms a simultaneous detection fact.
    QuietEnd,
    /// The consumer's stream ended where the session did NOT intend it (the
    /// `PumpStopped` translation): the unexpected session end.
    PumpStopped,
    /// The consumer failed; its failure is the retained output's error half.
    Failed,
}

impl ConsumerOutcome {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuietEnd => "QuietEnd",
            Self::PumpStopped => "PumpStopped",
            Self::Failed => "Failed",
        }
    }

    /// Parse the stable spelling back into the outcome (`None` for any other
    /// name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "QuietEnd" => Some(Self::QuietEnd),
            "PumpStopped" => Some(Self::PumpStopped),
            "Failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The background registration task's outcome at session end, as the ranking
/// sees it.
///
/// A task panic is NEVER clean completion: the adapters translate a panicked
/// (or cancelled-into-error) join onto [`Self::Failed`], so only a genuine
/// clean return is [`Self::Completed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegistrationOutcome {
    /// The registration task completed cleanly.
    Completed,
    /// The registration task failed — its own error, or its panic.
    Failed,
}

impl RegistrationOutcome {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }

    /// Parse the stable spelling back into the outcome (`None` for any other
    /// name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Completed" => Some(Self::Completed),
            "Failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// How a pump session's main loop ended — the Session-end ranking's verdict,
/// the typed analogue of the Python `SessionEndVerdict` set both driver shells
/// exchange by [`Self::name`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionEndVerdict {
    /// The consumer ended the session (its intended close, or its own failure).
    PumpEnded,
    /// A fatal registration error ended the session — the maximal fact.
    RegistrationFailed,
    /// The session ended outside its intended close: detection fired without an
    /// intended consumer end, or the consumer's stream stopped.
    WatchdogTripped,
}

impl SessionEndVerdict {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PumpEnded => "PumpEnded",
            Self::RegistrationFailed => "RegistrationFailed",
            Self::WatchdogTripped => "WatchdogTripped",
        }
    }

    /// Parse the stable spelling back into the verdict (`None` for any other
    /// name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "PumpEnded" => Some(Self::PumpEnded),
            "RegistrationFailed" => Some(Self::RegistrationFailed),
            "WatchdogTripped" => Some(Self::WatchdogTripped),
            _ => None,
        }
    }
}

/// The ONE slot an adapter may deliver a primary failure in.
///
/// An adapter delivers at most one failure, in exactly this slot; the consumer
/// output is retained and never raised again over a registration or
/// session-end failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RaiseSlot {
    /// Nothing raises: the intended close, or the empty no-failure remainder.
    Nothing,
    /// Raise the registration failure only.
    Registration,
    /// Raise the consumer's own failure only.
    Consumer,
    /// Raise the unexpected-session-end failure only (the loud abort a
    /// supervised session must not survive): the consumer's `PumpStopped`
    /// end, or a detection fact without an intended consumer end.
    SessionEnd,
}

impl RaiseSlot {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::Registration => "Registration",
            Self::Consumer => "Consumer",
            Self::SessionEnd => "SessionEnd",
        }
    }

    /// Parse the stable spelling back into the slot (`None` for any other
    /// name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Nothing" => Some(Self::Nothing),
            "Registration" => Some(Self::Registration),
            "Consumer" => Some(Self::Consumer),
            "SessionEnd" => Some(Self::SessionEnd),
            _ => None,
        }
    }
}

/// One closed observation of a session's end: every fact, already gathered.
///
/// Built by the adapter once it has collected the detection cause, translated
/// the consumer's and the registration's outcomes, and retrieved the
/// consumer's output — so the ranking reads contents, never arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEndObservation<T> {
    /// The detection cause, when detection fired.
    pub cause: Option<SessionEndCause>,
    /// The consumer's outcome (the adapter's translation of its own surface).
    pub consumer: Option<ConsumerOutcome>,
    /// The registration task's outcome.
    pub registration: Option<RegistrationOutcome>,
    /// The consumer's already-retrieved output.
    pub output: Option<T>,
}

/// The [`rank`] product: one verdict, one raise slot, and the retained
/// consumer output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEndRanking<T> {
    /// How the session ended.
    pub verdict: SessionEndVerdict,
    /// The ONE slot a primary failure may be delivered in.
    pub slot: RaiseSlot,
    /// The consumer's output, retained — never raised again over a
    /// registration or session-end failure.
    pub output: Option<T>,
}

/// The pure Session-end ranking: one closed observation in; one verdict, one
/// raise slot, and the retained consumer output out.
///
/// The fold is total over every input combination and independent of fact
/// arrival order (the observation is the only input). The ladder, highest
/// priority first:
///
/// | Priority | Observation | Verdict | [`RaiseSlot`] |
/// |---|---|---|---|
/// | 1 | registration outcome [`RegistrationOutcome::Failed`] (incl. a panic) | [`SessionEndVerdict::RegistrationFailed`] | [`RaiseSlot::Registration`] |
/// | 2 | consumer outcome [`ConsumerOutcome::QuietEnd`] | [`SessionEndVerdict::PumpEnded`] | [`RaiseSlot::Nothing`] |
/// | 3 | consumer outcome [`ConsumerOutcome::PumpStopped`] | [`SessionEndVerdict::WatchdogTripped`] | [`RaiseSlot::SessionEnd`] |
/// | 4 | a detection cause, without an intended consumer end | [`SessionEndVerdict::WatchdogTripped`] | [`RaiseSlot::SessionEnd`] |
/// | 5 | consumer outcome [`ConsumerOutcome::Failed`], no higher fact | [`SessionEndVerdict::PumpEnded`] | [`RaiseSlot::Consumer`] |
/// | 6 | the empty remainder (no consumer outcome, no detection cause) | [`SessionEndVerdict::PumpEnded`] | [`RaiseSlot::Nothing`] |
///
/// Registration failure is maximal (1). The intended consumer end (2)
/// disarms a simultaneous detection fact, because the expected close also
/// publishes `PumpFinished`. `PumpStopped` (3) and detection without an
/// intended consumer end (4) are the unexpected SESSION end: both trip the
/// watchdog verdict and raise their own loud failure at the session-end
/// slot. A consumer failure without a higher-priority fact (5) is the
/// consumer failure; the empty remainder (6) is the no-failure end and
/// raises nothing. The consumer output is retained and never raised again
/// over a registration or session-end failure.
#[must_use]
pub fn rank<T>(observation: SessionEndObservation<T>) -> SessionEndRanking<T> {
    let SessionEndObservation {
        cause,
        consumer,
        registration,
        output,
    } = observation;
    let (verdict, slot) = if registration == Some(RegistrationOutcome::Failed) {
        (
            SessionEndVerdict::RegistrationFailed,
            RaiseSlot::Registration,
        )
    } else if consumer == Some(ConsumerOutcome::QuietEnd) {
        (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing)
    } else if consumer == Some(ConsumerOutcome::PumpStopped) || cause.is_some() {
        // Rows 3 and 4 share one product: the stream stopped where the
        // session did not intend it, or detection fired without an intended
        // consumer end — the unexpected session end, one loud failure at the
        // session-end slot.
        (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd)
    } else if consumer == Some(ConsumerOutcome::Failed) {
        (SessionEndVerdict::PumpEnded, RaiseSlot::Consumer)
    } else {
        (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing)
    };
    SessionEndRanking {
        verdict,
        slot,
        output,
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

    /// The pinned ranking table: `expected[cause][consumer][registration]` —
    /// a literal (verdict, slot) pair for every one of the 3 x 4 x 3 input
    /// combinations, so the fold cannot grow a hidden cell.
    const EXPECTED_TABLE: [[[(SessionEndVerdict, RaiseSlot); 3]; 4]; 3] = [
        // no detection fact
        [
            // no consumer outcome: the empty remainder is the no-failure
            // end (the detection-only shape lives in the cause blocks below)
            [
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            // intended consumer end
            [
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            // the unexpected session end (PumpStopped)
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            // consumer failure alone
            [
                (SessionEndVerdict::PumpEnded, RaiseSlot::Consumer),
                (SessionEndVerdict::PumpEnded, RaiseSlot::Consumer),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
        ],
        // PumpFinished detected
        [
            // detection-only: the unexpected session end, raised at the
            // session-end slot
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            // the intended end disarms the simultaneous detection fact
            [
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            // detection outranks the consumer failure (the output stays
            // retained, never re-raised over the session-end failure)
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
        ],
        // StallWatchdogTripped detected — same fold as PumpFinished
        [
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            [
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (SessionEndVerdict::PumpEnded, RaiseSlot::Nothing),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
            [
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (SessionEndVerdict::WatchdogTripped, RaiseSlot::SessionEnd),
                (
                    SessionEndVerdict::RegistrationFailed,
                    RaiseSlot::Registration,
                ),
            ],
        ],
    ];

    /// The total ranking table, checked cell by cell against the pinned
    /// literal pairs above.
    #[test]
    fn the_ranking_table_is_total_over_every_input_combination() {
        let causes = [
            None,
            Some(SessionEndCause::PumpFinished),
            Some(SessionEndCause::StallWatchdogTripped),
        ];
        let consumers = [
            None,
            Some(ConsumerOutcome::QuietEnd),
            Some(ConsumerOutcome::PumpStopped),
            Some(ConsumerOutcome::Failed),
        ];
        let registrations = [
            None,
            Some(RegistrationOutcome::Completed),
            Some(RegistrationOutcome::Failed),
        ];
        for (cause_index, cause) in causes.iter().enumerate() {
            for (consumer_index, consumer) in consumers.iter().enumerate() {
                for (registration_index, registration) in registrations.iter().enumerate() {
                    let ranking = rank::<u64>(SessionEndObservation {
                        cause: *cause,
                        consumer: *consumer,
                        registration: *registration,
                        output: Some(7),
                    });
                    let (verdict, slot) =
                        EXPECTED_TABLE[cause_index][consumer_index][registration_index];
                    assert_eq!(
                        ranking.verdict, verdict,
                        "verdict for cause={cause:?} consumer={consumer:?} registration={registration:?}"
                    );
                    assert_eq!(
                        ranking.slot, slot,
                        "slot for cause={cause:?} consumer={consumer:?} registration={registration:?}"
                    );
                    assert_eq!(
                        ranking.output,
                        Some(7),
                        "the consumer output is retained for every cell"
                    );
                }
            }
        }
    }

    /// Registration failure is maximal: it outranks every simultaneous
    /// detection fact and consumer outcome, and its slot raises ONLY the
    /// registration failure (the consumer output is retained, not re-raised).
    #[test]
    fn registration_failure_is_maximal_over_simultaneous_facts() {
        let ranking = rank::<u64>(SessionEndObservation {
            cause: Some(SessionEndCause::StallWatchdogTripped),
            consumer: Some(ConsumerOutcome::Failed),
            registration: Some(RegistrationOutcome::Failed),
            output: Some(9),
        });
        assert_eq!(ranking.verdict, SessionEndVerdict::RegistrationFailed);
        assert_eq!(ranking.slot, RaiseSlot::Registration);
        assert_eq!(ranking.output, Some(9));
    }

    /// A registration task panic is never clean completion: only
    /// `RegistrationOutcome::Completed` is, so the adapter's translation of a
    /// panicked join lands `Failed` and this fold answers the fail-fast arm.
    #[test]
    fn a_registration_panic_translation_is_not_clean_completion() {
        let panicked = rank::<()>(SessionEndObservation {
            cause: None,
            consumer: None,
            registration: Some(RegistrationOutcome::Failed),
            output: None,
        });
        assert_eq!(panicked.verdict, SessionEndVerdict::RegistrationFailed);
        assert_eq!(panicked.slot, RaiseSlot::Registration);
    }

    /// The intended consumer end disarms the simultaneous detection fact: the
    /// expected close publishes `PumpFinished` too, so the tie reads as the
    /// clean end it is.
    #[test]
    fn an_intended_end_disarms_a_simultaneous_detection_fact() {
        let ranking = rank::<()>(SessionEndObservation {
            cause: Some(SessionEndCause::PumpFinished),
            consumer: Some(ConsumerOutcome::QuietEnd),
            registration: None,
            output: None,
        });
        assert_eq!(ranking.verdict, SessionEndVerdict::PumpEnded);
        assert_eq!(ranking.slot, RaiseSlot::Nothing);
    }

    /// An unexpected pump end (detection without an intended consumer end) and
    /// a `PumpStopped` consumer outcome both trip the watchdog verdict AND
    /// raise their own loud failure at the one session-end slot — both are the
    /// unexpected SESSION end.
    #[test]
    fn unexpected_pump_end_and_pump_stopped_select_watchdog_tripped() {
        let detected = rank::<()>(SessionEndObservation {
            cause: Some(SessionEndCause::PumpFinished),
            consumer: None,
            registration: None,
            output: None,
        });
        assert_eq!(detected.verdict, SessionEndVerdict::WatchdogTripped);
        assert_eq!(detected.slot, RaiseSlot::SessionEnd);

        let stopped = rank::<()>(SessionEndObservation {
            cause: None,
            consumer: Some(ConsumerOutcome::PumpStopped),
            registration: None,
            output: None,
        });
        assert_eq!(stopped.verdict, SessionEndVerdict::WatchdogTripped);
        assert_eq!(stopped.slot, RaiseSlot::SessionEnd);
    }

    /// A consumer failure with no higher-priority fact is the consumer
    /// failure: its own error stays visible through the consumer slot.
    #[test]
    fn consumer_error_alone_is_the_consumer_failure() {
        let ranking = rank::<&str>(SessionEndObservation {
            cause: None,
            consumer: Some(ConsumerOutcome::Failed),
            registration: None,
            output: Some("boom"),
        });
        assert_eq!(ranking.verdict, SessionEndVerdict::PumpEnded);
        assert_eq!(ranking.slot, RaiseSlot::Consumer);
        assert_eq!(ranking.output, Some("boom"));
    }

    /// The empty observation — no detection fact, no consumer outcome, no
    /// registration outcome — is the no-failure remainder: nothing raises.
    #[test]
    fn the_empty_observation_is_the_no_failure_remainder() {
        let ranking = rank::<()>(SessionEndObservation {
            cause: None,
            consumer: None,
            registration: None,
            output: None,
        });
        assert_eq!(ranking.verdict, SessionEndVerdict::PumpEnded);
        assert_eq!(ranking.slot, RaiseSlot::Nothing);
    }

    /// The verdict, raise-slot and fact vocabularies carry stable spellings;
    /// both driver shells exchange these names.
    #[test]
    fn the_ranking_vocabularies_expose_stable_names() {
        assert_eq!(SessionEndVerdict::PumpEnded.name(), "PumpEnded");
        assert_eq!(
            SessionEndVerdict::RegistrationFailed.name(),
            "RegistrationFailed"
        );
        assert_eq!(SessionEndVerdict::WatchdogTripped.name(), "WatchdogTripped");
        assert_eq!(RaiseSlot::Nothing.name(), "Nothing");
        assert_eq!(RaiseSlot::Registration.name(), "Registration");
        assert_eq!(RaiseSlot::Consumer.name(), "Consumer");
        assert_eq!(RaiseSlot::SessionEnd.name(), "SessionEnd");
        assert_eq!(ConsumerOutcome::QuietEnd.name(), "QuietEnd");
        assert_eq!(ConsumerOutcome::PumpStopped.name(), "PumpStopped");
        assert_eq!(ConsumerOutcome::Failed.name(), "Failed");
        assert_eq!(RegistrationOutcome::Completed.name(), "Completed");
        assert_eq!(RegistrationOutcome::Failed.name(), "Failed");
    }

    /// The name spellings round-trip: the FFI boundary parses exactly the
    /// names the core emits.
    #[test]
    fn the_fact_vocabularies_parse_their_own_names() {
        for cause in [
            SessionEndCause::PumpFinished,
            SessionEndCause::StallWatchdogTripped,
        ] {
            assert_eq!(SessionEndCause::from_name(cause.name()), Some(cause));
        }
        assert_eq!(SessionEndCause::from_name("nope"), None);
        for outcome in [
            ConsumerOutcome::QuietEnd,
            ConsumerOutcome::PumpStopped,
            ConsumerOutcome::Failed,
        ] {
            assert_eq!(ConsumerOutcome::from_name(outcome.name()), Some(outcome));
        }
        assert_eq!(ConsumerOutcome::from_name("nope"), None);
        for outcome in [RegistrationOutcome::Completed, RegistrationOutcome::Failed] {
            assert_eq!(
                RegistrationOutcome::from_name(outcome.name()),
                Some(outcome)
            );
        }
        assert_eq!(RegistrationOutcome::from_name("nope"), None);
    }
}
