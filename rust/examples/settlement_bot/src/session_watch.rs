//! Session watch: the driver-side judgement of how a pump session ended —
//! parity-ledger row 19 (Gap G5).
//!
//! [`SessionEndVerdict`] is the typed analogue of the Python verdict set
//! (`PumpEnded` / `RegistrationFailed` / `WatchdogTripped`), and [`SessionWatch`]
//! is the single owner of a session's end-state: the watch-set ({consumer} + an
//! optional detection fact channel + optional {registration}), the same-batch
//! ranking (a fail-fast registration outranks a detection fact), and the cancel
//! duties (it aborts the CONSUMER on a watchdog trip — it never owns or exits
//! the process, the *watch-as-observer* discipline).
//!
//! *Why* the session ended is the core's ([`degenbot::session_end`]): the pump
//! completion surface resolves [`SessionEndCause::PumpFinished`] and the
//! heartbeat stall watchdog publishes [`SessionEndCause::StallWatchdogTripped`]
//! through [`SessionEndFacts`], once. This watch reads that fact and applies its
//! own ranking; it builds no detection of its own.

use std::time::Duration;

use degenbot::session_end::{Heartbeat, SessionEndDetection, SessionEndFacts};
use tokio::task::JoinHandle;

/// How a pump session's main loop ended (the *session watch* verdict).
///
/// The three variants mirror `SessionEndVerdict` in
/// `src/degenbot/runner/_session_watch.py` one-for-one:
///
/// - [`SessionEndVerdict::PumpEnded`]: the consumer task itself ended (the
///   result stream closed, or the consumer raised — the error is surfaced
///   through [`SessionWatch::consumer_output`]).
/// - [`SessionEndVerdict::RegistrationFailed`]: a fatal registration error
///   was surfaced through the cross-task fail-fast channel; the consumer was
///   cancelled and the error stored on the watch. In a same-batch race this
///   verdict OUTRANKS [`SessionEndVerdict::WatchdogTripped`].
/// - [`SessionEndVerdict::WatchdogTripped`]: the detection fact fired (the pump
///   ended / the consume loop stalled outside `stop()`); it already cancelled
///   the consumer and the session leaves via the normal graceful teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEndVerdict {
    /// The consumer task itself ended.
    PumpEnded,
    /// A fatal registration error ended the session.
    RegistrationFailed,
    /// The watchdog tripped and cancelled the consumer.
    WatchdogTripped,
}

/// One owner of a pump session's end-state: watch-set, verdict, teardown.
///
/// Generic over the consumer's successful output `T` so the live arm can hand
/// it the result-consumer's `(ConsumerReport, BlockClock)` payload.
pub struct SessionWatch<T> {
    consumer: Option<JoinHandle<T>>,
    registration: Option<JoinHandle<Result<(), String>>>,
    facts: Option<SessionEndFacts>,
    registration_error: Option<String>,
    consumer_output: Option<Result<T, String>>,
    verdict: Option<SessionEndVerdict>,
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

    /// The consumer's output (present once [`Self::wait`] observed its
    /// completion; absent on the `RegistrationFailed` / abort paths).
    #[must_use]
    pub fn consumer_output(&self) -> Option<&Result<T, String>> {
        self.consumer_output.as_ref()
    }

    /// Take the consumer's output (moves it out of the watch).
    pub fn take_consumer_output(&mut self) -> Option<Result<T, String>> {
        self.consumer_output.take()
    }

    /// Watch the session's task set until the session ends; return the verdict.
    ///
    /// The same-batch ranking mirrors Python: a fatal registration error
    /// outranks a detection fact in the same wait batch. On the fail-fast path
    /// the consumer is aborted; on the detection path it is aborted too. The
    /// watch never exits the process — it reports, and the caller decides.
    ///
    /// # Errors
    ///
    /// Returns an error string only when no consumer was attached. The
    /// consumer's own failure is stored in [`Self::consumer_output`] as
    /// `Err(_)` — the Python `await main_task` propagation analogue.
    pub async fn wait(&mut self) -> Result<SessionEndVerdict, String> {
        if let Some(verdict) = self.verdict {
            return Ok(verdict);
        }
        let Some(mut consumer) = self.consumer.take() else {
            return Err("session watch has no consumer attached".to_string());
        };
        let mut registration = self.registration.take();
        let facts = self.facts.clone();
        let mut pump_ended = false;
        let mut consumer_output: Option<Result<T, String>> = None;

        loop {
            if consumer.is_finished() {
                break;
            }
            let reg_enabled = registration.is_some();
            let facts_enabled = facts.is_some();
            tokio::select! {
                // Deterministic ranking (mirrors the Python done-set check
                // order): registration is polled first, so a same-batch
                // registration failure outranks a detection fact.
                biased;
                res = &mut consumer => {
                    consumer_output = Some(res.map_err(|e| e.to_string()));
                    break;
                }
                reg_res = async {
                    match registration.as_mut() {
                        Some(handle) => handle.await,
                        None => std::future::pending().await,
                    }
                }, if reg_enabled => {
                    // Fail-fast outranks everything: a fatal registration error
                    // is surfaced even when detection fired in the same batch.
                    if let Ok(Err(error)) = reg_res {
                        consumer.abort();
                        let _ = consumer.await;
                        self.registration_error = Some(error);
                        self.verdict = Some(SessionEndVerdict::RegistrationFailed);
                        return Ok(SessionEndVerdict::RegistrationFailed);
                    }
                    // A clean completion stops watching it, keeps {consumer, detection}.
                    registration = None;
                }
                _cause = async {
                    match facts.as_ref() {
                        Some(facts) => facts.wait().await,
                        None => std::future::pending().await,
                    }
                }, if facts_enabled => {
                    // Same-batch ranking (mirrors the Python done-set
                    // check order): a registration that already finished
                    // with a fatal error OUTRANKS the detection fact, even
                    // if the fact was polled first.
                    if let Some(reg) = registration.take() {
                        if reg.is_finished() {
                            if let Ok(Err(error)) = reg.await {
                                consumer.abort();
                                let _ = consumer.await;
                                self.registration_error = Some(error);
                                self.verdict =
                                    Some(SessionEndVerdict::RegistrationFailed);
                                return Ok(SessionEndVerdict::RegistrationFailed);
                            }
                        } else {
                            reg.abort();
                        }
                    }
                    // The pump ended / stalled: the watch cancels the
                    // consumer (observer discipline) and leaves via the
                    // normal teardown.
                    pump_ended = true;
                    break;
                }
            }
        }

        if pump_ended {
            consumer.abort();
            consumer_output = Some(consumer.await.map_err(|e| e.to_string()));
            self.consumer_output = consumer_output;
            self.verdict = Some(SessionEndVerdict::WatchdogTripped);
            return Ok(SessionEndVerdict::WatchdogTripped);
        }

        self.consumer_output = consumer_output;
        self.verdict = Some(SessionEndVerdict::PumpEnded);
        Ok(SessionEndVerdict::PumpEnded)
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
    /// drop + consumer cancel (mirrors Python `SessionWatch.teardown`).
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

/// The outcome [`supervise_consumer`] reports back through its `JoinHandle`.
#[derive(Debug)]
pub struct SessionWatchOutcome<T> {
    /// The end-state verdict.
    pub verdict: SessionEndVerdict,
    /// The fatal registration error, if the verdict was `RegistrationFailed`.
    pub registration_error: Option<String>,
    /// The consumer's result (its own error is `Err(_)`).
    pub consumer: Option<Result<T, String>>,
}

/// Spawn-friendly supervisor: own a consumer task + the core's stall detection
/// and report the [`SessionWatchOutcome`] once the session ends.
///
/// This is the live-arm wiring: the consumer beats `heartbeat` per batch and
/// the watch aborts it on a stall, reporting `WatchdogTripped`. The detection
/// itself is the core's [`SessionEndDetection`]; this function only owns its
/// lifetime and the ranking.
pub async fn supervise_consumer<T: Send + 'static>(
    consumer: JoinHandle<T>,
    heartbeat: Heartbeat,
    stall_after: Duration,
) -> SessionWatchOutcome<T> {
    let detection = SessionEndDetection::stall(heartbeat, stall_after);
    let mut watch = SessionWatch::new();
    watch.attach_consumer(consumer);
    watch.attach_session_end_facts(detection.facts().clone());
    let verdict = watch
        .wait()
        .await
        .unwrap_or(SessionEndVerdict::WatchdogTripped);
    SessionWatchOutcome {
        verdict,
        registration_error: watch.registration_error().map(ToString::to_string),
        consumer: watch.take_consumer_output(),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;
    use degenbot::session_end::SessionEndCause;

    fn stalled_facts() -> (degenbot::session_end::SessionEndPublisher, SessionEndFacts) {
        let (publisher, facts) = SessionEndFacts::channel();
        publisher.publish(SessionEndCause::StallWatchdogTripped);
        (publisher, facts)
    }

    #[test]
    fn the_verdict_set_matches_the_python_spelling() {
        // Byte-for-byte the three `SessionEndVerdict` member names.
        assert_eq!(format!("{:?}", SessionEndVerdict::PumpEnded), "PumpEnded");
        assert_eq!(
            format!("{:?}", SessionEndVerdict::RegistrationFailed),
            "RegistrationFailed"
        );
        assert_eq!(
            format!("{:?}", SessionEndVerdict::WatchdogTripped),
            "WatchdogTripped"
        );
    }

    #[tokio::test]
    async fn consumer_completion_is_pump_ended() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 7_u64 }));
        assert_eq!(watch.wait().await.unwrap(), SessionEndVerdict::PumpEnded);
        assert_eq!(watch.consumer_output().unwrap().as_ref().unwrap(), &7);
    }

    #[tokio::test]
    async fn consumer_error_is_surfaced_as_output() {
        let mut watch: SessionWatch<Result<(), String>> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async {
            Err::<(), String>("boom".to_string())
        }));
        // `wait` returns the verdict; the consumer's own error lives in the
        // output as the inner `Err` (the outer layer is the join result).
        assert_eq!(watch.wait().await.unwrap(), SessionEndVerdict::PumpEnded);
        assert!(matches!(watch.consumer_output().unwrap(), Ok(Err(_))));
    }

    #[tokio::test]
    async fn watchdog_trip_cancels_the_consumer() {
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        let (_publisher, facts) = stalled_facts();
        watch.attach_session_end_facts(facts);
        assert_eq!(
            watch.wait().await.unwrap(),
            SessionEndVerdict::WatchdogTripped
        );
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
        // Both resolve in the same wait batch: the registration verdict wins.
        let (_publisher, facts) = stalled_facts();
        watch.attach_session_end_facts(facts);
        assert_eq!(
            watch.wait().await.unwrap(),
            SessionEndVerdict::RegistrationFailed
        );
        assert_eq!(watch.registration_error(), Some("reg boom"));
    }

    #[tokio::test]
    async fn clean_registration_stops_watching_it_and_pumps_on() {
        let mut watch: SessionWatch<u64> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(async { 3_u64 }));
        watch.attach_registration(tokio::spawn(async { Ok::<(), String>(()) }));
        assert_eq!(watch.wait().await.unwrap(), SessionEndVerdict::PumpEnded);
    }

    #[tokio::test(start_paused = true)]
    async fn core_stall_detection_trips_the_watch_and_cancels_the_consumer() {
        let detection = SessionEndDetection::stall(Heartbeat::new(), Duration::from_secs(1));
        let mut watch: SessionWatch<()> = SessionWatch::new();
        watch.attach_consumer(tokio::spawn(std::future::pending::<()>()));
        watch.attach_session_end_facts(detection.facts().clone());
        tokio::time::advance(Duration::from_millis(1_100)).await;
        assert_eq!(
            watch.wait().await.unwrap(),
            SessionEndVerdict::WatchdogTripped
        );
    }
}
