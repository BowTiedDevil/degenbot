//! Bounded concurrent sim fan-out + a single ordered submit lane.
//!
//! Three behaviors that only mean anything together:
//!
//! - **Bounded concurrency**: every batch's simulate work spawns as its own
//!   task, bounded by a semaphore sized by a plain `usize` cap.
//! - **Ordered submit lane**: ONE submitter drains the batches in ARRIVAL
//!   (FIFO) order, awaits each batch's own sim, then submits. Submission
//!   order is therefore nonce order, and the single lane is what lets a
//!   driver fetch one nonce per submit at the moment of submit.
//! - **Loud abort**: a sim or submit leaf failure is stored and re-raised in
//!   the caller's frame ([`SimSubmitPipeline::raise_if_failed`]) — no silent
//!   pump death.
//!
//! One [`PipelineState`] owns the lane's lifecycle: it is the ONLY admission
//! authority ([`SimSubmitPipeline::enqueue`] has no effect outside
//! `Accepting`), the ONLY home of the terminal failure (the first leaf — or
//! queue-send — failure, which later failures never replace), and the ONLY
//! record of whether that failure has been reported. A queue sender or a
//! submitter handle being present says nothing about the lifecycle state.
//!
//! The concurrency bound and the ordered lane are deliberately one module:
//! split apart they leave two shallow halves every driver must re-compose.
//!
//! The in-flight cap is injected as a value. The posture authority that
//! derives a cap (a fleet's cordon floor, an operator knob) stays with the
//! driver, so this core module never reads an env var or a process verdict.
//!
//! The pipeline is generic over the work payload `W` and the sim outcome `O`
//! and drives injected [`SimLeaf`]/[`SubmitLeaf`] implementations; the sim
//! leaf owns the encode/simulate reduction, the submit leaf the render/submit
//! step.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, Semaphore};

/// The background sim handle for one in-flight batch.
type SimTask<O> = tokio::task::JoinHandle<Result<Option<O>, String>>;

/// The boxed future a sim leaf returns.
pub type SimFuture<'a, O> = Pin<Box<dyn Future<Output = Result<Option<O>, String>> + Send + 'a>>;

/// The boxed future a submit leaf returns.
pub type SubmitFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// The simulate leaf for one batch. `Ok(None)` = nothing dispatchable.
pub trait SimLeaf<W, O>: Send + Sync {
    /// Simulate one batch.
    fn simulate<'a>(&'a self, work: &'a W) -> SimFuture<'a, O>;
}

/// The submit leaf for one completed batch outcome.
pub trait SubmitLeaf<W, O>: Send + Sync {
    /// Render + submit one completed batch outcome.
    fn submit<'a>(&'a self, work: &'a W, outcome: &'a O) -> SubmitFuture<'a>;
}

/// A pipeline leaf failure (stored, then re-raised in the caller's frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineFailure {
    /// The failure detail.
    pub detail: String,
}

/// One terminal failure plus whether a caller has already observed it.
///
/// The failure itself is never erased: reporting it is bookkeeping about the
/// caller's frame, not a transition back to accepting work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFailure {
    /// The first (authoritative) failure.
    pub failure: PipelineFailure,
    /// Whether a caller already observed the failure.
    pub reported: bool,
}

impl TerminalFailure {
    /// Report the failure the FIRST time it is observed; a later observation is
    /// a no-op `Ok`.
    fn observe(&mut self) -> Result<(), PipelineFailure> {
        if self.reported {
            return Ok(());
        }
        self.reported = true;
        Err(self.failure.clone())
    }

    /// Report the failure on EVERY call — the teardown's total verdict.
    fn report(&mut self) -> Result<(), PipelineFailure> {
        self.reported = true;
        Err(self.failure.clone())
    }
}

/// The pipeline's lifecycle: the ONE authority for enqueue admission, for the
/// authoritative failure, and for whether that failure has been reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineState {
    /// Accepting batches — the only state in which an enqueue has any effect.
    Accepting,
    /// A sim/submit leaf (or the queue send) failed: terminal for new work.
    Failed(TerminalFailure),
    /// Shutdown began: the queue takes nothing new while accepted work drains.
    Closing(Option<TerminalFailure>),
    /// Shutdown finished: the submitter is joined and the queue is closed.
    Closed(Option<TerminalFailure>),
}

impl PipelineState {
    /// Enqueue admission: `Accepting` is the only state that admits work.
    fn admit(&self) -> Result<(), EnqueueDecline> {
        match self {
            Self::Accepting => Ok(()),
            Self::Failed(_) => Err(EnqueueDecline::Failed),
            Self::Closing(_) => Err(EnqueueDecline::Closing),
            Self::Closed(_) => Err(EnqueueDecline::Closed),
        }
    }

    /// The authoritative failure, reported or not.
    fn failure(&self) -> Option<&PipelineFailure> {
        match self {
            Self::Accepting => None,
            Self::Failed(terminal) => Some(&terminal.failure),
            Self::Closing(failure) | Self::Closed(failure) => {
                failure.as_ref().map(|terminal| &terminal.failure)
            }
        }
    }

    /// The carried terminal failure, for the two observation transitions.
    fn terminal_mut(&mut self) -> Option<&mut TerminalFailure> {
        match self {
            Self::Accepting => None,
            Self::Failed(terminal) => Some(terminal),
            Self::Closing(failure) | Self::Closed(failure) => failure.as_mut(),
        }
    }

    /// Leaf (or queue-send) failure: the FIRST failure is authoritative, is
    /// terminal for new work, and is never replaced by a later one.
    fn record_failure(&mut self, failure: PipelineFailure) {
        match self {
            Self::Accepting => {
                *self = Self::Failed(TerminalFailure {
                    failure,
                    reported: false,
                });
            }
            Self::Failed(_) => {}
            Self::Closing(slot) | Self::Closed(slot) => {
                if slot.is_none() {
                    *slot = Some(TerminalFailure {
                        failure,
                        reported: false,
                    });
                }
            }
        }
    }

    /// Failure observation (the between-batch poll): report once, then no-op.
    fn observe_failure(&mut self) -> Result<(), PipelineFailure> {
        self.terminal_mut().map_or(Ok(()), TerminalFailure::observe)
    }

    /// Failure observation (the teardown): every call reports a carried failure,
    /// so no later call can report a success the terminal state contradicts.
    fn report_failure(&mut self) -> Result<(), PipelineFailure> {
        self.terminal_mut().map_or(Ok(()), TerminalFailure::report)
    }

    /// Shutdown start: admission closes; a recorded failure rides along.
    fn begin_shutdown(&mut self) {
        let closing = match self {
            Self::Accepting => Some(Self::Closing(None)),
            Self::Failed(terminal) => Some(Self::Closing(Some(terminal.clone()))),
            Self::Closing(_) | Self::Closed(_) => None,
        };
        if let Some(state) = closing {
            *self = state;
        }
    }

    /// Submitter exit: the drain is over, so the state is final. A lane that
    /// died on a failure keeps `Failed` — the failure is already terminal and
    /// stays distinguishable from a shutdown that started.
    fn finish_shutdown(&mut self) {
        let closed = match self {
            Self::Accepting => Some(Self::Closed(None)),
            Self::Closing(failure) => Some(Self::Closed(failure.clone())),
            Self::Failed(_) | Self::Closed(_) => None,
        };
        if let Some(state) = closed {
            *self = state;
        }
    }
}

/// Why [`SimSubmitPipeline::enqueue`] declined a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueDecline {
    /// A leaf (or the queue send) failed: terminal for new work.
    Failed,
    /// Shutdown has begun: the queue takes no new work.
    Closing,
    /// Shutdown finished: the pipeline is closed.
    Closed,
}

/// What [`SimSubmitPipeline::enqueue`] did with a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum EnqueueOutcome {
    /// The batch holds a queue slot and its bounded sim was spawned.
    Accepted,
    /// The pipeline declined the batch: nothing was counted, spawned, or
    /// queued.
    Declined(EnqueueDecline),
}

impl EnqueueOutcome {
    /// Whether the pipeline accepted the batch.
    #[must_use]
    pub const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// The queue-send failure's detail: the submitter dropped its receiver, so no
/// batch can reach the ordered lane again.
const SUBMITTER_DROPPED: &str = "pipeline submitter dropped";

/// One in-flight batch: the work + its background sim handle.
struct WorkSlot<W, O> {
    work: W,
    sim_task: Mutex<Option<SimTask<O>>>,
}

/// The FIFO queue's item type (one in-flight batch slot).
type SlotSender<W, O> = mpsc::UnboundedSender<Arc<WorkSlot<W, O>>>;

/// The bounded fan-out + ordered submitter.
pub struct SimSubmitPipeline<W, O> {
    concurrency: usize,
    sem: Arc<Semaphore>,
    /// The lifecycle — the ONE admission/failure authority. The move-once
    /// handles below (`tx`, `submitter`) are teardown state only: their
    /// presence says nothing about whether the lane accepts work.
    state: Arc<Mutex<PipelineState>>,
    /// The FIFO queue's sender, consumed by the first `shutdown`.
    tx: Mutex<Option<SlotSender<W, O>>>,
    /// The ordered submitter's handle, consumed by the first `shutdown`.
    submitter: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Serializes WHOLE shutdown calls: a concurrent caller waits for the first
    /// caller's drain instead of observing consumed handles and closing the
    /// lane underneath it. An async lock held across the join await is the
    /// point — the std lifecycle/tx/submitter locks are the ones never held
    /// across an await.
    shutdown_serial: tokio::sync::Mutex<()>,
    sim: Arc<dyn SimLeaf<W, O>>,
    enqueued: Arc<AtomicU64>,
    submitted: Arc<AtomicU64>,
}

impl<W, O> SimSubmitPipeline<W, O>
where
    W: Clone + Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    /// Build a pipeline with the given in-flight sim bound (floor 1).
    #[must_use]
    pub fn new(
        concurrency: usize,
        sim: Arc<dyn SimLeaf<W, O>>,
        submit: Arc<dyn SubmitLeaf<W, O>>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let sem = Arc::new(Semaphore::new(concurrency.max(1)));
        let state: Arc<Mutex<PipelineState>> = Arc::new(Mutex::new(PipelineState::Accepting));
        let enqueued = Arc::new(AtomicU64::new(0));
        let submitted = Arc::new(AtomicU64::new(0));
        let submit_handle = tokio::spawn(submit_loop(
            rx,
            Arc::clone(&state),
            Arc::clone(&submitted),
            submit,
        ));
        Self {
            concurrency: concurrency.max(1),
            sem,
            state,
            tx: Mutex::new(Some(tx)),
            submitter: Mutex::new(Some(submit_handle)),
            shutdown_serial: tokio::sync::Mutex::new(()),
            sim,
            enqueued,
            submitted,
        }
    }

    /// The configured in-flight sim bound (tests + soak logging).
    #[must_use]
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// How many batches have been enqueued (accepted only).
    #[must_use]
    pub fn enqueued(&self) -> u64 {
        self.enqueued.load(Ordering::SeqCst)
    }

    /// How many batches have been submitted (or skipped) by the ordered
    /// submitter.
    #[must_use]
    pub fn submitted(&self) -> u64 {
        self.submitted.load(Ordering::SeqCst)
    }

    /// The pipeline's lifecycle state — the admission authority.
    #[must_use]
    pub fn state(&self) -> PipelineState {
        self.lock_state().clone()
    }

    /// The authoritative failure, reported or not (`None` while the pipeline is
    /// healthy). This read is not an observation: it reports nothing.
    #[must_use]
    pub fn failure(&self) -> Option<PipelineFailure> {
        self.lock_state().failure().cloned()
    }

    /// Enqueue one batch: spawn its sim (bounded by the semaphore) and
    /// register it in the FIFO submit queue. Returns immediately — the caller
    /// keeps advancing.
    ///
    /// Admission and its effects are ONE synchronous span under the lifecycle
    /// lock: outside `Accepting` nothing is counted, spawned, or queued, and
    /// the caller gets a typed [`EnqueueDecline`].
    pub fn enqueue(&self, work: W) -> EnqueueOutcome {
        let mut state = self.lock_state();
        if let Err(decline) = state.admit() {
            return EnqueueOutcome::Declined(decline);
        }

        let sem = Arc::clone(&self.sem);
        let sim = Arc::clone(&self.sim);
        let work_for_task = work.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire_owned()
                .await
                .map_err(|e| format!("sim semaphore closed: {e}"))?;
            sim.simulate(&work_for_task).await
        });
        let slot = Arc::new(WorkSlot {
            work,
            sim_task: Mutex::new(Some(handle)),
        });

        let sender = self
            .tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let queued = sender.is_some_and(|sender| sender.send(Arc::clone(&slot)).is_ok());
        if !queued {
            // The queue is gone: the batch never holds a slot, so its sim is
            // cancelled and the failed send takes the SAME terminal transition
            // a leaf failure does (a dead submitter is discovered, never
            // inferred from a channel's presence).
            let sim_task = slot
                .sim_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(handle) = sim_task {
                handle.abort();
            }
            state.record_failure(PipelineFailure {
                detail: SUBMITTER_DROPPED.to_string(),
            });
            return EnqueueOutcome::Declined(EnqueueDecline::Failed);
        }

        self.enqueued.fetch_add(1, Ordering::SeqCst);
        EnqueueOutcome::Accepted
    }

    /// Graceful teardown: close the queue, drain in-flight work, await the
    /// submitter, then report the terminal failure.
    ///
    /// # The repeated-read contract
    ///
    /// `shutdown` is total and idempotent. The sender and the submitter handle
    /// are consumed exactly once (a repeat call joins nothing), and while the
    /// pipeline carries a failure EVERY call returns that same failure — never
    /// a fresh success contradicting the terminal state, including a call made
    /// after [`Self::raise_if_failed`] already reported it.
    ///
    /// Concurrent calls SERIALIZE on the teardown: the second caller waits for
    /// the first caller's drain and then reports that drain's verdict. A caller
    /// that saw the already-consumed handles and finished the state on its own
    /// would declare the lane closed while the submitter was still running.
    ///
    /// # Errors
    ///
    /// Returns the pipeline's first leaf (or queue-send) failure, if any.
    pub async fn shutdown(&self) -> Result<(), PipelineFailure> {
        // One teardown at a time: the guard spans the whole drain, so a
        // concurrent caller is pending until this caller's drain completed.
        let _serial = self.shutdown_serial.lock().await;
        // Shutdown start: admission closes; a recorded failure rides along.
        self.lock_state().begin_shutdown();
        // Consume the sender exactly once — this is what closes the queue.
        drop(
            self.tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        // Consume the submitter handle exactly once: a repeat shutdown joins
        // nothing and cannot join the same task twice.
        let handle = self
            .submitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        // The submitter's exit transition already closed the lane; a panicked
        // submitter never ran it, so the transition is total here too.
        let mut state = self.lock_state();
        state.finish_shutdown();
        state.report_failure()
    }

    /// Report the pipeline's first failure in the CALLER's frame (the
    /// loud-abort rule) — once.
    ///
    /// Reporting is not an admission transition: the pipeline stays terminal
    /// and keeps declining new work, and the failure stays readable through
    /// [`Self::state`]/[`Self::failure`]. A second call is a no-op `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineFailure`] the first time a sim/submit leaf failed.
    pub fn raise_if_failed(&self) -> Result<(), PipelineFailure> {
        self.lock_state().observe_failure()
    }

    /// The lifecycle lock, poison-tolerant (no leaf panic may wedge teardown).
    fn lock_state(&self) -> std::sync::MutexGuard<'_, PipelineState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Record a leaf failure on the lane's lifecycle (a leaf failure is never lost).
fn record_failure(state: &Mutex<PipelineState>, detail: String) {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .record_failure(PipelineFailure { detail });
}

/// The single ordered submitter: pop in arrival (FIFO) order, await that
/// batch's sim, then submit. The first leaf failure is recorded on the
/// lifecycle and stops the loop.
async fn submit_loop<W, O>(
    mut rx: mpsc::UnboundedReceiver<Arc<WorkSlot<W, O>>>,
    state: Arc<Mutex<PipelineState>>,
    submitted: Arc<AtomicU64>,
    submit: Arc<dyn SubmitLeaf<W, O>>,
) where
    W: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    loop {
        let Some(slot) = rx.recv().await else { break };
        let handle = {
            slot.sim_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        let Some(handle) = handle else { continue };
        let result = match handle.await {
            Ok(inner) => inner,
            Err(join_err) => Err(format!("sim task panicked: {join_err}")),
        };
        match result {
            Ok(Some(outcome)) => {
                if let Err(detail) = submit.submit(&slot.work, &outcome).await {
                    record_failure(&state, detail);
                    break;
                }
            }
            Ok(None) => {}
            Err(detail) => {
                record_failure(&state, detail);
                break;
            }
        }
        submitted.fetch_add(1, Ordering::SeqCst);
    }
    // Submitter exit: the lane takes no more work and the state is final.
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .finish_shutdown();
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-valid fixtures and the lifecycle's own reports"
)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    /// One test batch: an id + a per-batch sleep so completion order can be
    /// made to disagree with arrival order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestWork {
        id: u64,
        sleep_ms: u64,
    }

    struct MockSim {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        fail_id: Option<u64>,
    }

    impl SimLeaf<TestWork, u64> for MockSim {
        fn simulate<'a>(&'a self, work: &'a TestWork) -> SimFuture<'a, u64> {
            let in_flight = Arc::clone(&self.in_flight);
            let max_in_flight = Arc::clone(&self.max_in_flight);
            let fail_id = self.fail_id;
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(work.sleep_ms)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                if fail_id == Some(work.id) {
                    return Err(format!("sim failed for {}", work.id));
                }
                Ok(Some(work.id))
            })
        }
    }

    /// A submit leaf that records arrival order and can fail on one id.
    struct RecordingSubmit {
        order: Arc<Mutex<Vec<u64>>>,
        fail_id: Option<u64>,
    }

    impl SubmitLeaf<TestWork, u64> for RecordingSubmit {
        fn submit<'a>(&'a self, _work: &'a TestWork, outcome: &'a u64) -> SubmitFuture<'a> {
            let order = Arc::clone(&self.order);
            let fail_id = self.fail_id;
            let id = *outcome;
            Box::pin(async move {
                if fail_id == Some(id) {
                    return Err(format!("submit failed for {id}"));
                }
                order.lock().unwrap().push(id);
                Ok(())
            })
        }
    }

    fn counters() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)))
    }

    /// A submit leaf that holds the ordered lane inside one submit until the
    /// test releases it (the drain-in-progress fixture).
    struct GatedSubmit {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl SubmitLeaf<TestWork, u64> for GatedSubmit {
        fn submit<'a>(&'a self, _work: &'a TestWork, _outcome: &'a u64) -> SubmitFuture<'a> {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(())
            })
        }
    }

    fn mock(fail_id: Option<u64>) -> Arc<MockSim> {
        let (in_flight, max_in_flight) = counters();
        Arc::new(MockSim {
            in_flight,
            max_in_flight,
            fail_id,
        })
    }

    fn accepted(outcome: EnqueueOutcome) {
        assert_eq!(outcome, EnqueueOutcome::Accepted);
    }

    #[tokio::test]
    async fn sim_fanout_is_bounded_by_concurrency() {
        let (in_flight, max_in_flight) = counters();
        let sim = Arc::new(MockSim {
            in_flight: Arc::clone(&in_flight),
            max_in_flight: Arc::clone(&max_in_flight),
            fail_id: None,
        });
        let submit = Arc::new(RecordingSubmit {
            order: Arc::new(Mutex::new(Vec::new())),
            fail_id: None,
        });
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        assert_eq!(pipeline.concurrency(), 2);
        for id in 0..5_u64 {
            accepted(pipeline.enqueue(TestWork { id, sleep_ms: 20 }));
        }
        pipeline.shutdown().await.unwrap();
        assert_eq!(pipeline.enqueued(), 5);
        assert_eq!(pipeline.submitted(), 5);
        let peak = max_in_flight.load(Ordering::SeqCst);
        assert!(peak <= 2, "peak {peak} exceeded the concurrency bound");
        assert!(peak >= 2, "peak {peak} shows no concurrency");
        assert_eq!(pipeline.state(), PipelineState::Closed(None));
    }

    #[tokio::test]
    async fn submitter_preserves_arrival_order_despite_sim_completion_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let sim = mock(None);
        let submit = Arc::new(RecordingSubmit {
            order: Arc::clone(&order),
            fail_id: None,
        });
        let pipeline = SimSubmitPipeline::new(4, sim, submit);
        // Later batches sleep longer, so sim completion order disagrees with
        // arrival order; FIFO must still hold.
        for (id, sleep_ms) in [(10_u64, 1), (11, 5), (12, 10), (13, 20)] {
            accepted(pipeline.enqueue(TestWork { id, sleep_ms }));
        }
        pipeline.shutdown().await.unwrap();
        assert_eq!(*order.lock().unwrap(), vec![10, 11, 12, 13]);
    }

    #[tokio::test]
    async fn sim_failure_aborts_loudly() {
        let sim = mock(Some(1));
        let submit = Arc::new(RecordingSubmit {
            order: Arc::new(Mutex::new(Vec::new())),
            fail_id: None,
        });
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        for id in 0..3_u64 {
            accepted(pipeline.enqueue(TestWork { id, sleep_ms: 1 }));
        }
        let err = pipeline.shutdown().await.unwrap_err();
        assert_eq!(err.detail, "sim failed for 1");
        // The teardown reported it, so the between-batch poll has nothing new
        // to say...
        assert!(pipeline.raise_if_failed().is_ok());
        // ...and the teardown keeps reporting the same failure: reporting is
        // not recovery, and a repeat call never reports a fresh success.
        assert_eq!(
            pipeline.shutdown().await.unwrap_err().detail,
            "sim failed for 1"
        );
    }

    #[tokio::test]
    async fn submit_failure_aborts_loudly() {
        let sim = mock(None);
        let submit = Arc::new(RecordingSubmit {
            order: Arc::new(Mutex::new(Vec::new())),
            fail_id: Some(7),
        });
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        accepted(pipeline.enqueue(TestWork { id: 7, sleep_ms: 0 }));
        let err = pipeline.shutdown().await.unwrap_err();
        assert_eq!(err.detail, "submit failed for 7");
    }

    #[tokio::test]
    async fn counters_track_enqueued_and_submitted() {
        let sim = mock(None);
        let submit = Arc::new(RecordingSubmit {
            order: Arc::new(Mutex::new(Vec::new())),
            fail_id: None,
        });
        let pipeline = SimSubmitPipeline::new(1, sim, submit);
        assert_eq!(pipeline.enqueued(), 0);
        assert_eq!(pipeline.submitted(), 0);
        for id in [1_u64, 2, 3] {
            accepted(pipeline.enqueue(TestWork { id, sleep_ms: 0 }));
        }
        pipeline.shutdown().await.unwrap();
        assert_eq!(pipeline.enqueued(), 3);
        assert_eq!(pipeline.submitted(), 3);
    }

    #[tokio::test]
    async fn a_none_outcome_is_skipped_but_still_counted() {
        struct NoneSim;

        impl SimLeaf<TestWork, u64> for NoneSim {
            fn simulate<'a>(&'a self, _work: &'a TestWork) -> SimFuture<'a, u64> {
                Box::pin(async { Ok(None) })
            }
        }

        let order = Arc::new(Mutex::new(Vec::new()));
        let pipeline = SimSubmitPipeline::new(
            1,
            Arc::new(NoneSim),
            Arc::new(RecordingSubmit {
                order: Arc::clone(&order),
                fail_id: None,
            }),
        );
        accepted(pipeline.enqueue(TestWork { id: 1, sleep_ms: 0 }));
        pipeline.shutdown().await.unwrap();
        assert!(order.lock().unwrap().is_empty());
        assert_eq!(pipeline.submitted(), 1);
    }

    #[tokio::test]
    async fn enqueue_during_a_drain_is_declined_before_any_side_effect() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let pipeline = Arc::new(SimSubmitPipeline::new(
            1,
            mock(None),
            Arc::new(GatedSubmit {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
        ));
        accepted(pipeline.enqueue(TestWork { id: 1, sleep_ms: 0 }));

        let draining = {
            let pipeline = Arc::clone(&pipeline);
            tokio::spawn(async move { pipeline.shutdown().await })
        };
        // The lifecycle's own report is the signal that the drain began.
        tokio::time::timeout(Duration::from_secs(5), async {
            while pipeline.state() != PipelineState::Closing(None) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the shutdown started");

        assert_eq!(
            pipeline.enqueue(TestWork { id: 2, sleep_ms: 0 }),
            EnqueueOutcome::Declined(EnqueueDecline::Closing)
        );
        assert_eq!(pipeline.enqueued(), 1, "the decline counted nothing");
        assert_eq!(pipeline.state(), PipelineState::Closing(None));

        release.notify_one();
        draining
            .await
            .unwrap()
            .expect("no leaf failed in the drain");
        assert_eq!(pipeline.state(), PipelineState::Closed(None));
        assert_eq!((pipeline.enqueued(), pipeline.submitted()), (1, 1));
    }

    #[test]
    fn admission_is_accepting_only() {
        fn failure() -> TerminalFailure {
            TerminalFailure {
                failure: PipelineFailure {
                    detail: "leaf failed".to_string(),
                },
                reported: false,
            }
        }

        assert_eq!(PipelineState::Accepting.admit(), Ok(()));
        assert_eq!(
            PipelineState::Failed(failure()).admit(),
            Err(EnqueueDecline::Failed)
        );
        assert_eq!(
            PipelineState::Closing(Some(failure())).admit(),
            Err(EnqueueDecline::Closing)
        );
        assert_eq!(
            PipelineState::Closed(None).admit(),
            Err(EnqueueDecline::Closed)
        );
    }

    #[test]
    fn a_later_failure_never_replaces_the_first() {
        let mut state = PipelineState::Accepting;
        state.record_failure(PipelineFailure {
            detail: "the first".to_string(),
        });
        state.record_failure(PipelineFailure {
            detail: "the second".to_string(),
        });
        assert_eq!(
            state.failure(),
            Some(&PipelineFailure {
                detail: "the first".to_string(),
            })
        );
        assert_eq!(state.admit(), Err(EnqueueDecline::Failed));
    }
}
