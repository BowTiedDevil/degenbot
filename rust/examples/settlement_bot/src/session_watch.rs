//! Session watch: the driver-side ADAPTER over the core Session-end ranking —
//! parity-ledger row 19 (Gap G5).
//!
//! [`SessionWatch`] owns the watch-set ({consumer} + an optional detection fact
//! channel + optional {registration}) and the cancel/reap duties; it gathers
//! the end facts, retrieves the consumer's output on EVERY path, translates
//! its own surface into the core's vocabulary, and folds everything through
//! the ONE core [`rank`] — it never decides a verdict from which task happened
//! to finish first.
//!
//! *Why* the session ended is the core's detection ([`SessionEndCause`]); how
//! the gathered facts rank is the core's pure fold ([`SessionEndVerdict`],
//! [`RaiseSlot`], [`rank`]). This watch is the *watch-as-observer* discipline:
//! it aborts the CONSUMER on a watchdog trip — it never owns or exits the
//! process. The `ConsumerEnd` translation lives in `degenbot-batch-executor`
//! (the upper crate); the core module never names it.

use std::time::Duration;

use degenbot::session_end::{
    rank, ConsumerOutcome, Heartbeat, RaiseSlot, RegistrationOutcome, SessionEndCause,
    SessionEndDetection, SessionEndFacts, SessionEndObservation,
};
use tokio::task::JoinHandle;

/// The core's ranked verdict — re-exported so `main` reads the ONE vocabulary
/// (`PumpEnded` / `RegistrationFailed` / `WatchdogTripped`).
pub use degenbot::session_end::SessionEndVerdict;

/// The translation of the result-consumer's retrieved output into the core's
/// consumer-outcome vocabulary — the live arm's share of the seam.
///
/// A clean report carries its own end verdict (the `ConsumerEnd` translation
/// is `degenbot-batch-executor`'s); a failed or panicked consumer task is the
/// consumer failure.
#[must_use]
pub fn live_consumer_outcome(
    output: &Result<Result<degenbot::batch_executor::SessionReport, String>, String>,
) -> ConsumerOutcome {
    match output {
        Ok(Ok(report)) => ConsumerOutcome::from(&report.consumer),
        Ok(Err(_)) | Err(_) => ConsumerOutcome::Failed,
    }
}

/// One owner of a pump session's end-state: watch-set, gather, cancel/reap,
/// render hand-off.
///
/// Generic over the consumer's successful output `T` so the live arm can hand
/// it the result-consumer's report payload.
pub struct SessionWatch<T> {
    consumer: Option<JoinHandle<T>>,
    registration: Option<JoinHandle<Result<(), String>>>,
    facts: Option<SessionEndFacts>,
    registration_error: Option<String>,
    consumer_output: Option<Result<T, String>>,
    verdict: Option<SessionEndVerdict>,
    slot: Option<RaiseSlot>,
}

impl<T> std::fmt::Debug for SessionWatch<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWatch")
            .field("consumer", &self.consumer.is_some())
            .field("registration", &self.registration.is_some())
            .field("facts", &self.facts.is_some())
            .field("registration_error", &self.registration_error)
            .field(
                "consumer_output",
                &self.consumer_output.as_ref().map(Result::is_ok),
            )
            .field("verdict", &self.verdict)
            .field("slot", &self.slot)
            .finish()
    }
}

impl<T> Default for SessionWatch<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SessionWatch<T> {
    /// An empty watch (attach the members before [`Self::wait`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            consumer: None,
            registration: None,
            facts: None,
            registration_error: None,
            consumer_output: None,
            verdict: None,
            slot: None,
        }
    }

    /// Attach the always-on consumer member (called the moment the consumer
    /// task exists, so a teardown after any later failure still reaches it).
    pub fn attach_consumer(&mut self, consumer: JoinHandle<T>) {
        self.consumer = Some(consumer);
    }

    /// Attach the optional registration member (the background registration
    /// task). Inline/injected sessions never call this.
    pub fn attach_registration(&mut self, registration: JoinHandle<Result<(), String>>) {
        self.registration = Some(registration);
    }

    /// Attach the core's once-only detection fact channel.
    pub fn attach_session_end_facts(&mut self, facts: SessionEndFacts) {
        self.facts = Some(facts);
    }

    /// The fatal registration error behind the `RegistrationFailed` verdict
    /// (`None` until then).
    #[must_use]
    pub fn registration_error(&self) -> Option<&str> {
        self.registration_error.as_deref()
    }

    /// The consumer's output — retrieved on EVERY path (including a
    /// simultaneous registration failure) and retained; it is never raised
    /// again over a registration or session-end failure.
    #[must_use]
    pub fn consumer_output(&self) -> Option<&Result<T, String>> {
        self.consumer_output.as_ref()
    }

    /// Take the consumer's output (moves it out of the watch).
    pub fn take_consumer_output(&mut self) -> Option<Result<T, String>> {
        self.consumer_output.take()
    }

    /// The ranked raise slot (`None` before [`Self::wait`]).
    #[must_use]
    pub fn slot(&self) -> Option<RaiseSlot> {
        self.slot
    }

    /// Gather the session's end facts until the session ends, retrieve the
    /// consumer's output, and rank them through the core's one fold.
    ///
    /// The gather is order-free: whichever member resolves first, the fold
    /// reads the gathered CONTENTS (the same-batch registration failure and
    /// consumer completion are both collected), so the verdict cannot depend
    /// on task completion order. The consumer's output is retrieved on every
    /// path — including the top-of-loop finished check and a simultaneous
    /// registration failure — and a still-climbing registration stays on the
    /// watch so [`Self::teardown_registration`] can drain it.
    ///
    /// # Errors
    ///
    /// Returns an error string only when no consumer was attached (the
    /// adapter's own refusal — never a ranked verdict). The consumer's own
    /// failure rides [`Self::consumer_output`] as `Err(_)`.
    pub async fn wait(
        &mut self,
        translate: impl FnOnce(&Result<T, String>) -> ConsumerOutcome,
    ) -> Result<SessionEndVerdict, String> {
        if let Some(verdict) = self.verdict {
            return Ok(verdict);
        }
        let Some(mut consumer) = self.consumer.take() else {
            return Err("session watch has no consumer attached".to_string());
        };
        let mut registration = self.registration.take();
        let facts = self.facts.clone();
        let mut cause: Option<SessionEndCause> = None;
        let mut registration_outcome: Option<RegistrationOutcome> = None;
        let mut registration_error: Option<String> = None;
        let mut consumer_output: Option<Result<T, String>> = None;

        loop {
            // Output retrieval: a consumer that finished before this
            // iteration is still retrieved HERE — a top-of-loop exit must not
            // drop its output.
            if consumer.is_finished() {
                consumer_output = Some((&mut consumer).await.map_err(|join| join.to_string()));
                break;
            }
            let registration_live = registration.is_some();
            let facts_live = facts.is_some();
            tokio::select! {
                biased;
                result = &mut consumer => {
                    consumer_output = Some(result.map_err(|join| join.to_string()));
                    break;
                }
                registration_result = async {
                    match registration.as_mut() {
                        Some(handle) => handle.await,
                        None => std::future::pending().await,
                    }
                }, if registration_live => {
                    // Only a clean `Ok(Ok(()))` is clean completion: the
                    // task's own error AND a panic (the join error) are
                    // registration failures, and the fail-fast arm ends the
                    // gather here (registration failure is maximal — the
                    // fold below confirms it).
                    match registration_result {
                        Ok(Ok(())) => {
                            registration_outcome = Some(RegistrationOutcome::Completed);
                            registration = None;
                        }
                        Ok(Err(error)) => {
                            registration_outcome = Some(RegistrationOutcome::Failed);
                            registration_error = Some(error);
                            break;
                        }
                        Err(join) => {
                            registration_outcome = Some(RegistrationOutcome::Failed);
                            registration_error = Some(join.to_string());
                            break;
                        }
                    }
                }
                detected = async {
                    match facts.as_ref() {
                        Some(facts) => facts.wait().await,
                        None => std::future::pending().await,
                    }
                }, if facts_live => {
                    cause = Some(detected);
                    break;
                }
            }
        }

        // The fold reads gathered CONTENTS, never arrival order: pick up every
        // fact that resolved in the same batch as the one that ended the
        // gather.
        if cause.is_none() {
            cause = facts.as_ref().and_then(SessionEndFacts::current);
        }
        if registration_outcome.is_none() {
            if let Some(handle) = registration.take() {
                if handle.is_finished() {
                    match handle.await {
                        Ok(Ok(())) => registration_outcome = Some(RegistrationOutcome::Completed),
                        Ok(Err(error)) => {
                            registration_outcome = Some(RegistrationOutcome::Failed);
                            registration_error = Some(error);
                        }
                        Err(join) => {
                            registration_outcome = Some(RegistrationOutcome::Failed);
                            registration_error = Some(join.to_string());
                        }
                    }
                } else {
                    // Still climbing at the session's end: the handle STAYS on
                    // the watch so `teardown_registration` can drain it (a
                    // dropped handle would detach the task).
                    self.registration = Some(handle);
                }
            }
        }

        // The cancel/reap duty: a consumer that never reported is aborted and
        // its join retrieved, so no consumer output is ever lost.
        if consumer_output.is_none() {
            consumer.abort();
            consumer_output = Some((&mut consumer).await.map_err(|join| join.to_string()));
        }

        let consumer_end = consumer_output.as_ref().map(translate);
        let ranking = rank(SessionEndObservation {
            cause,
            consumer: consumer_end,
            registration: registration_outcome,
            output: consumer_output,
        });
        self.registration_error = registration_error;
        self.consumer_output = ranking.output;
        self.slot = Some(ranking.slot);
        self.verdict = Some(ranking.verdict);
        Ok(ranking.verdict)
    }

    /// Cancel/drain the background registration task (idempotent).
    pub fn teardown_registration(&mut self) {
        if let Some(handle) = self.registration.take() {
            if !handle.is_finished() {
                handle.abort();
            }
        }
    }

    /// The idempotent end-of-session teardown: registration drain + detection
    /// drop + consumer cancel.
    pub fn teardown(&mut self) {
        self.teardown_registration();
        self.facts = None;
        if let Some(handle) = self.consumer.take() {
            if !handle.is_finished() {
                handle.abort();
            }
        }
    }
}

/// The ranked product [`supervise_consumer`] reports back through its
/// `JoinHandle`: the verdict, the one raise slot, the fatal registration
/// error, and the retained consumer output.
#[derive(Debug)]
pub struct SessionWatchOutcome<T> {
    /// The end-state verdict.
    pub verdict: SessionEndVerdict,
    /// The ONE slot a primary failure may be delivered in.
    pub slot: RaiseSlot,
    /// The fatal registration error, if the verdict was `RegistrationFailed`.
    pub registration_error: Option<String>,
    /// The consumer's result (its own error is `Err(_)`), retrieved on every
    /// path.
    pub consumer: Option<Result<T, String>>,
}

/// Spawn-friendly supervisor: own a consumer task + the core's stall detection,
/// and report the [`SessionWatchOutcome`] once the session ends.
///
/// This is the live-arm wiring: the consumer beats `heartbeat` per batch and
/// the watch aborts it on a stall, reporting `WatchdogTripped`. The detection
/// is the core's [`SessionEndDetection`]; the ranking is the core's [`rank`];
/// this function owns only the gather, the cancel/reap, and the product.
///
/// # Errors
///
/// The watch's own refusal (`no consumer attached`) is propagated here —
/// never masked as a ranked verdict.
pub async fn supervise_consumer<T: Send + 'static>(
    consumer: JoinHandle<T>,
    heartbeat: Heartbeat,
    stall_after: Duration,
    translate: impl FnOnce(&Result<T, String>) -> ConsumerOutcome,
) -> Result<SessionWatchOutcome<T>, String> {
    let detection = SessionEndDetection::stall(heartbeat, stall_after);
    let mut watch = SessionWatch::new();
    watch.attach_consumer(consumer);
    watch.attach_session_end_facts(detection.facts().clone());
    let verdict = watch.wait(translate).await?;
    let slot = watch
        .slot()
        .ok_or_else(|| "session watch ranked no raise slot".to_string())?;
    Ok(SessionWatchOutcome {
        verdict,
        slot,
        registration_error: watch.registration_error().map(ToString::to_string),
        consumer: watch.take_consumer_output(),
    })
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests assert on known-valid inputs; one double panics on purpose"
)]
mod tests {
    use super::*;
    use degenbot::session_end::{SessionEndCause, SessionEndPublisher};

    fn stalled_facts() -> (SessionEndPublisher, SessionEndFacts) {
        let (publisher, facts) = SessionEndFacts::channel();
        publisher.publish(SessionEndCause::StallWatchdogTripped);
        (publisher, facts)
    }

    /// The settled spellings both shells exchange (byte-for-byte the Python
    /// `SessionEndVerdict` / `RaiseSlot` member names).
    #[test]
    fn the_verdict_and_slot_names_match_the_python_spelling() {
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
    }

    #[tokio::test]
    async fn consumer_completion_is_pump_ended() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::PumpEnded
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::Nothing));
        assert_eq!(watch.consumer_output().unwrap().as_ref().unwrap(), &7);
    }

    #[tokio::test]
    async fn consumer_error_is_surfaced_as_output() {
        let mut watch: SessionWatch<Result<(), String>> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async {
            Err::<(), String>("boom".to_string())
        }));
        // The verdict is `PumpEnded`; the consumer's own error lives in the
        // output as the inner `Err`, delivered through the consumer slot.
        assert_eq!(
            watch
                .wait(|output| match output {
                    Ok(Err(_)) | Err(_) => ConsumerOutcome::Failed,
                    Ok(Ok(())) => ConsumerOutcome::QuietEnd,
                })
                .await
                .unwrap(),
            SessionEndVerdict::PumpEnded
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::Consumer));
        assert!(matches!(watch.consumer_output().unwrap(), Ok(Err(_))));
    }

    #[tokio::test]
    async fn watchdog_trip_cancels_the_consumer() {
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        let (_publisher, facts) = stalled_facts();
        watch.attach_session_end_facts(facts);
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::Failed).await.unwrap(),
            SessionEndVerdict::WatchdogTripped
        );
        // Detection without an intended consumer end is the unexpected
        // session end: its own loud failure rides the session-end slot.
        assert_eq!(watch.slot(), Some(RaiseSlot::SessionEnd));
    }

    #[tokio::test]
    async fn registration_failure_outranks_the_watchdog() {
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        watch.attach_registration(tokio::spawn(async {
            Err::<(), String>("reg boom".to_string())
        }));
        // Let the registration task finish before the wait begins, so both
        // members are in the same wait batch.
        tokio::task::yield_now().await;
        let (_publisher, facts) = stalled_facts();
        watch.attach_session_end_facts(facts);
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::Failed).await.unwrap(),
            SessionEndVerdict::RegistrationFailed
        );
        assert_eq!(watch.registration_error(), Some("reg boom"));
        assert_eq!(watch.slot(), Some(RaiseSlot::Registration));
    }

    #[tokio::test]
    async fn clean_registration_stops_watching_it_and_pumps_on() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 3_u64 }));
        watch.attach_registration(tokio::spawn(async { Ok::<(), String>(()) }));
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::PumpEnded
        );
    }

    #[tokio::test(start_paused = true)]
    async fn core_stall_detection_trips_the_watch_and_cancels_the_consumer() {
        let detection = SessionEndDetection::stall(Heartbeat::new(), Duration::from_secs(1));
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        watch.attach_session_end_facts(detection.facts().clone());
        tokio::time::advance(Duration::from_millis(1_100)).await;
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::Failed).await.unwrap(),
            SessionEndVerdict::WatchdogTripped
        );
    }

    /// The top-of-loop consumer-finished check retrieves the output instead of
    /// dropping it: a consumer that finished before the gather began still
    /// reports its payload.
    #[tokio::test]
    async fn a_consumer_finished_before_the_gather_still_reports_its_output() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        tokio::task::yield_now().await;
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::PumpEnded
        );
        assert_eq!(
            watch.consumer_output().unwrap().as_ref().unwrap(),
            &7,
            "the finished consumer's output is retrieved, never lost"
        );
    }

    /// Same-batch registration failure + consumer completion: the fold selects
    /// `RegistrationFailed`, the consumer output is retrieved and retained,
    /// and the ONE delivered failure is the registration failure.
    #[tokio::test]
    async fn a_same_batch_registration_failure_retrieves_the_consumer_output() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        watch.attach_registration(tokio::spawn(async {
            Err::<(), String>("reg boom".to_string())
        }));
        tokio::task::yield_now().await;
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::RegistrationFailed
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::Registration));
        assert_eq!(watch.registration_error(), Some("reg boom"));
        assert_eq!(
            watch.consumer_output().unwrap().as_ref().unwrap(),
            &7,
            "the simultaneous consumer completion is retrieved and retained"
        );
    }

    /// A registration task panic is not clean completion: it ranks as the
    /// registration failure, never as a clean pump end.
    #[tokio::test]
    async fn a_registration_panic_is_not_clean_completion() {
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        watch.attach_registration(tokio::spawn(async { panic!("reg panic") }));
        tokio::task::yield_now().await;
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::Failed).await.unwrap(),
            SessionEndVerdict::RegistrationFailed
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::Registration));
        assert!(
            watch
                .registration_error()
                .is_some_and(|error| error.contains("reg panic")),
            "the panic text is the delivered registration failure"
        );
    }

    /// The intended-end tie: an intended consumer end disarms the simultaneous
    /// detection fact (the expected close publishes `PumpFinished` too).
    #[tokio::test]
    async fn an_intended_end_disarms_the_simultaneous_detection_fact() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        let (publisher, facts) = SessionEndFacts::channel();
        publisher.publish(SessionEndCause::PumpFinished);
        watch.attach_session_end_facts(facts);
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::PumpEnded,
            "the tie reads as the clean end it is"
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::Nothing));
    }

    /// A `PumpStopped` consumer outcome is the unexpected session end: the
    /// watchdog verdict raises its own failure at the session-end slot, and
    /// the retained consumer output is not re-raised over it.
    #[tokio::test]
    async fn a_pump_stopped_end_is_the_unexpected_session_end() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::PumpStopped).await.unwrap(),
            SessionEndVerdict::WatchdogTripped
        );
        assert_eq!(watch.slot(), Some(RaiseSlot::SessionEnd));
        assert_eq!(watch.consumer_output().unwrap().as_ref().unwrap(), &7);
    }

    /// The missing-consumer refusal is the adapter's own refusal: it is never
    /// masked as a `WatchdogTripped` verdict.
    #[tokio::test]
    async fn the_missing_consumer_refusal_is_never_a_watchdog_trip() {
        let mut watch: SessionWatch<()> = SessionWatch::new();
        let refusal = watch.wait(|_| ConsumerOutcome::Failed).await.unwrap_err();
        assert!(refusal.contains("no consumer attached"));
    }

    /// A registration still climbing when the consumer ends stays on the
    /// watch: the teardown duty drains it (a dropped handle would detach the
    /// task and lose the cancel).
    #[tokio::test]
    async fn a_climbing_registration_stays_drainable_after_the_consumer_ended() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        watch.attach_registration(tokio::spawn(std::future::pending::<Result<(), String>>()));
        assert_eq!(
            watch.wait(|_| ConsumerOutcome::QuietEnd).await.unwrap(),
            SessionEndVerdict::PumpEnded
        );
        watch.teardown_registration();
        assert!(
            watch.registration.is_none(),
            "the teardown took the climbing registration"
        );
    }
}
