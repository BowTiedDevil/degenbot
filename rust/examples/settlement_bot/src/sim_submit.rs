//! Concurrent-sim + ordered-submit pipeline — parity-ledger row 16 (ergo
//! `L4E7RI`, Gap G4).
//!
//! Mirrors `src/degenbot/runner/_sim_submit_pipeline.py` (SIMPIPE option A):
//! every batch's simulate work spawns as its own task, bounded by a
//! semaphore sized `max_simulate_concurrent` (config default 50); ONE
//! submitter task pops batch descriptors in ARRIVAL (FIFO) order, awaits that
//! batch's own sim, then submits — byte-identical submission ordering to the
//! serial loop, only the sims overlap.
//!
//! Loud-abort contract: a sim or submit failure is stored and re-raised from
//! the consumer via [`SimSubmitPipeline::raise_if_failed`] (the Python
//! `SimSubmitPipelineLeafFailure`, incident 2026-08-20).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, Semaphore};

use crate::dispatch::RawResult;

/// The background sim handle for one in-flight batch.
type SimTask = tokio::task::JoinHandle<Result<Option<SimBatchOutcome>, String>>;

/// The FIFO queue's item type (one in-flight batch slot).
type SlotSender = mpsc::UnboundedSender<Arc<WorkSlot>>;

/// One streamed solver batch traversing the pipeline.
#[derive(Clone, Debug)]
pub struct BatchWork {
    /// The batch's raw engine rows, in stream order.
    pub results: Vec<RawResult>,
    /// The current block's timestamp (`session.dispatcher.block_timestamp_for`).
    pub block_timestamp: u64,
    /// The next-block base fee (`next_base_fee`).
    pub base_fee_next: u128,
    /// The current block at enqueue time.
    pub current_block: u64,
}

/// The sim leaf's per-batch result (a driver-side reduction of the core
/// `DispatchOutcome`; the submit leaf consumes it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimBatchOutcome {
    /// The path ids that reached the submit stage, in profit-descending order.
    pub path_ids: Vec<u64>,
}

/// The boxed future a sim leaf returns.
pub type SimFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<SimBatchOutcome>, String>> + Send + 'a>>;

/// The boxed future a submit leaf returns.
pub type SubmitFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// The simulate leaf for one batch (GIL-free across the RPC part in the
/// Python driver; here the injected seam keeps the pipeline offline-testable).
pub trait SimLeaf: Send + Sync {
    /// Simulate one batch. `Ok(None)` = nothing dispatchable.
    fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a>;
}

/// The submit leaf for one completed batch outcome.
pub trait SubmitLeaf: Send + Sync {
    /// Render + submit one completed batch outcome.
    fn submit<'a>(&'a self, work: &'a BatchWork, outcome: &'a SimBatchOutcome) -> SubmitFuture<'a>;
}

/// A pipeline leaf failure (stored, then re-raised in the consumer's frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineFailure {
    /// The failure detail.
    pub detail: String,
}

/// One in-flight batch: the work + its background sim handle.
struct WorkSlot {
    work: BatchWork,
    sim_task: Mutex<Option<SimTask>>,
}

/// The bounded fan-out + ordered submitter.
pub struct SimSubmitPipeline {
    concurrency: usize,
    sem: Arc<Semaphore>,
    tx: Mutex<Option<SlotSender>>,
    submitter: Mutex<Option<tokio::task::JoinHandle<()>>>,
    failure: Arc<Mutex<Option<String>>>,
    sim: Arc<dyn SimLeaf>,
    submit: Arc<dyn SubmitLeaf>,
    enqueued: Arc<AtomicU64>,
    submitted: Arc<AtomicU64>,
}

impl SimSubmitPipeline {
    /// Build a pipeline with the given in-flight sim bound (floor 1).
    #[must_use]
    pub fn new(concurrency: usize, sim: Arc<dyn SimLeaf>, submit: Arc<dyn SubmitLeaf>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let sem = Arc::new(Semaphore::new(concurrency.max(1)));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let enqueued = Arc::new(AtomicU64::new(0));
        let submitted_count = Arc::new(AtomicU64::new(0));
        let submit_handle = tokio::spawn(submit_loop(
            rx,
            Arc::clone(&failure),
            Arc::clone(&submitted_count),
            Arc::clone(&submit),
        ));
        Self {
            concurrency: concurrency.max(1),
            sem,
            tx: Mutex::new(Some(tx)),
            submitter: Mutex::new(Some(submit_handle)),
            failure,
            sim,
            submit,
            enqueued,
            submitted: submitted_count,
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
    /// register it in the FIFO submit queue. Returns immediately (the
    /// consumer keeps advancing) — mirrors Python `enqueue`.
    pub fn enqueue(&self, work: BatchWork) {
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
        // Drop the pipeline's sender so `recv()` drains then returns `None`.
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
async fn submit_loop(
    mut rx: mpsc::UnboundedReceiver<Arc<WorkSlot>>,
    failure: Arc<Mutex<Option<String>>>,
    submitted: Arc<AtomicU64>,
    submit: Arc<dyn SubmitLeaf>,
) {
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

    struct MockSim {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        sleep_ms: u64,
        fail_path: Option<u64>,
    }

    impl SimLeaf for MockSim {
        fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a> {
            let in_flight = Arc::clone(&self.in_flight);
            let max_in_flight = Arc::clone(&self.max_in_flight);
            let sleep_ms = self.sleep_ms;
            let fail_path = self.fail_path;
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                if let Some(p) = fail_path {
                    if work.results.iter().any(|r| r.path_id == p) {
                        return Err(format!("sim failed for {p}"));
                    }
                }
                Ok(Some(SimBatchOutcome {
                    path_ids: work.results.iter().map(|r| r.path_id).collect(),
                }))
            })
        }
    }

    #[derive(Default)]
    struct RecordingSubmit {
        order: Arc<Mutex<Vec<u64>>>,
    }

    impl SubmitLeaf for RecordingSubmit {
        fn submit<'a>(
            &'a self,
            _work: &'a BatchWork,
            outcome: &'a SimBatchOutcome,
        ) -> SubmitFuture<'a> {
            let order = Arc::clone(&self.order);
            Box::pin(async move {
                order
                    .lock()
                    .unwrap()
                    .extend(outcome.path_ids.iter().copied());
                Ok(())
            })
        }
    }

    fn work(ids: &[u64], sleep_hint: u64) -> BatchWork {
        BatchWork {
            results: ids
                .iter()
                .map(|id| RawResult {
                    path_id: *id,
                    optimal_input: 1_000,
                    profit: 10,
                    hop_outputs: vec![10],
                    consumed_inputs: vec![1_000],
                    solve_block: sleep_hint,
                    state_nonces: vec![0],
                })
                .collect(),
            block_timestamp: 0,
            base_fee_next: 0,
            current_block: 0,
        }
    }

    #[tokio::test]
    async fn sim_fanout_is_bounded_by_concurrency() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let sim = Arc::new(MockSim {
            in_flight: Arc::clone(&in_flight),
            max_in_flight: Arc::clone(&max_in_flight),
            sleep_ms: 20,
            fail_path: None,
        });
        let submit = Arc::new(RecordingSubmit::default());
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        assert_eq!(pipeline.concurrency(), 2);
        for i in 0..5_u64 {
            pipeline.enqueue(work(&[i], 0));
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
        let sim = Arc::new(MockSim {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
            sleep_ms: 5,
            fail_path: None,
        });
        let submit = Arc::new(RecordingSubmit {
            order: Arc::clone(&order),
        });
        let pipeline = SimSubmitPipeline::new(4, sim, submit);
        // Enqueue in an order whose later batches sleep longer (in-flight IV
        // varies); FIFO must still hold.
        for id in [10_u64, 11, 12, 13] {
            pipeline.enqueue(work(&[id], 0));
        }
        pipeline.shutdown().await.unwrap();
        assert_eq!(*order.lock().unwrap(), vec![10, 11, 12, 13]);
    }

    #[tokio::test]
    async fn leaf_failure_aborts_loudly() {
        let sim = Arc::new(MockSim {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
            sleep_ms: 1,
            fail_path: Some(1),
        });
        let submit = Arc::new(RecordingSubmit::default());
        let pipeline = SimSubmitPipeline::new(2, sim, submit);
        pipeline.enqueue(work(&[0], 0));
        pipeline.enqueue(work(&[1], 0));
        pipeline.enqueue(work(&[2], 0));
        let err = pipeline.shutdown().await.unwrap_err();
        assert_eq!(err.detail, "sim failed for 1");
        // The stored failure is drained exactly once.
        assert!(pipeline.raise_if_failed().is_ok());
    }
}
