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
    tx: Mutex<Option<SlotSender<W, O>>>,
    submitter: Mutex<Option<tokio::task::JoinHandle<()>>>,
    failure: Arc<Mutex<Option<String>>>,
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
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let enqueued = Arc::new(AtomicU64::new(0));
        let submitted = Arc::new(AtomicU64::new(0));
        let submit_handle = tokio::spawn(submit_loop(
            rx,
            Arc::clone(&failure),
            Arc::clone(&submitted),
            submit,
        ));
        Self {
            concurrency: concurrency.max(1),
            sem,
            tx: Mutex::new(Some(tx)),
            submitter: Mutex::new(Some(submit_handle)),
            failure,
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

    /// How many batches have been enqueued.
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

    /// Enqueue one batch: spawn its sim (bounded by the semaphore) and
    /// register it in the FIFO submit queue. Returns immediately — the caller
    /// keeps advancing.
    pub fn enqueue(&self, work: W) {
        self.enqueued.fetch_add(1, Ordering::SeqCst);
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
        if let Some(sender) = sender {
            if sender.send(slot).is_err() {
                let mut f = self
                    .failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *f = Some("pipeline submitter dropped".to_string());
            }
        }
    }

    /// Graceful teardown: close the queue, drain in-flight work, await the
    /// submitter. A DEAD submitter re-raises its stored failure instead of
    /// hanging the join.
    ///
    /// # Errors
    ///
    /// Returns the first stored leaf failure, if any.
    pub async fn shutdown(&self) -> Result<(), PipelineFailure> {
        drop(
            self.tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        let handle = self
            .submitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        self.raise_if_failed()
    }

    /// Re-raise the first leaf failure in the CALLER's frame (loud abort).
    ///
    /// # Errors
    ///
    /// Returns [`PipelineFailure`] when a sim/submit leaf failed.
    pub fn raise_if_failed(&self) -> Result<(), PipelineFailure> {
        let mut guard = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.take() {
            Some(detail) => Err(PipelineFailure { detail }),
            None => Ok(()),
        }
    }
}

/// The single ordered submitter: pop in arrival (FIFO) order, await that
/// batch's sim, then submit. A leaf failure is stored + stops the loop.
async fn submit_loop<W, O>(
    mut rx: mpsc::UnboundedReceiver<Arc<WorkSlot<W, O>>>,
    failure: Arc<Mutex<Option<String>>>,
    submitted: Arc<AtomicU64>,
    submit: Arc<dyn SubmitLeaf<W, O>>,
) where
    W: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    while let Some(slot) = rx.recv().await {
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
                    *failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
                    return;
                }
            }
            Ok(None) => {}
            Err(detail) => {
                *failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
                return;
            }
        }
        submitted.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
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

    fn mock(fail_id: Option<u64>) -> Arc<MockSim> {
        let (in_flight, max_in_flight) = counters();
        Arc::new(MockSim {
            in_flight,
            max_in_flight,
            fail_id,
        })
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
            pipeline.enqueue(TestWork { id, sleep_ms: 20 });
        }
        pipeline.shutdown().await.unwrap();
        assert_eq!(pipeline.enqueued(), 5);
        assert_eq!(pipeline.submitted(), 5);
        let peak = max_in_flight.load(Ordering::SeqCst);
        assert!(peak <= 2, "peak {peak} exceeded the concurrency bound");
        assert!(peak >= 2, "peak {peak} shows no concurrency");
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
            pipeline.enqueue(TestWork { id, sleep_ms });
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
            pipeline.enqueue(TestWork { id, sleep_ms: 1 });
        }
        let err = pipeline.shutdown().await.unwrap_err();
        assert_eq!(err.detail, "sim failed for 1");
        // The stored failure is drained exactly once.
        assert!(pipeline.raise_if_failed().is_ok());
    }

    #[tokio::test]
    async fn submit_failure_aborts_loudly() {
        let sim = mock(None);
        let submit = Arc::new(RecordingSubmit {
            order: Arc::new(Mutex::new(Vec::new())),
            fail_id: Some(7),
        });
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        pipeline.enqueue(TestWork { id: 7, sleep_ms: 0 });
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
            pipeline.enqueue(TestWork { id, sleep_ms: 0 });
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
        pipeline.enqueue(TestWork { id: 1, sleep_ms: 0 });
        pipeline.shutdown().await.unwrap();
        assert!(order.lock().unwrap().is_empty());
        assert_eq!(pipeline.submitted(), 1);
    }
}
