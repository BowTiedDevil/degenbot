//! The ordered lane's lifecycle: admission, terminal failure, idempotent
//! teardown.
//!
//! Seams: [`SimSubmitPipeline`]'s public enqueue / shutdown / failure / counter
//! surface. These pin one lifecycle owner (a typed decline with NO side effect
//! outside `Accepting`, the first failure staying authoritative after it is
//! observed, a shutdown that is total and idempotent) beside the lane's
//! ordering contract (FIFO submit under reverse sim completion, bounded
//! concurrency, `Ok(None)` skip counting). Relay and node behavior are out of
//! scope: no leaf here touches an RPC.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the tests assert on known-valid fixtures and the lifecycle's own reports"
)]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use degenbot_submission::{
    EnqueueDecline, EnqueueOutcome, PipelineFailure, PipelineState, SimFuture, SimLeaf,
    SimSubmitPipeline, SubmitFuture, SubmitLeaf, TerminalFailure,
};

/// One test batch: an id + a per-batch sleep, so sim completion order can be
/// made to disagree with arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TestWork {
    id: u64,
    sleep_ms: u64,
}

fn failure(detail: &str) -> PipelineFailure {
    PipelineFailure {
        detail: detail.to_string(),
    }
}

fn work(id: u64, sleep_ms: u64) -> TestWork {
    TestWork { id, sleep_ms }
}

/// A sim leaf that counts its own invocations (the "did any work spawn?" probe),
/// tracks peak concurrency, and fails the given ids.
struct CountingSim {
    calls: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    fail_ids: Vec<u64>,
}

impl SimLeaf<TestWork, u64> for CountingSim {
    fn simulate<'a>(&'a self, work: &'a TestWork) -> SimFuture<'a, u64> {
        let calls = Arc::clone(&self.calls);
        let in_flight = Arc::clone(&self.in_flight);
        let max_in_flight = Arc::clone(&self.max_in_flight);
        let fails = self.fail_ids.contains(&work.id);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(work.sleep_ms)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            if fails {
                return Err(format!("sim failed for {}", work.id));
            }
            Ok(Some(work.id))
        })
    }
}

fn counting_sim(fail_ids: &[u64]) -> Arc<CountingSim> {
    Arc::new(CountingSim {
        calls: Arc::new(AtomicU64::new(0)),
        in_flight: Arc::new(AtomicUsize::new(0)),
        max_in_flight: Arc::new(AtomicUsize::new(0)),
        fail_ids: fail_ids.to_vec(),
    })
}

/// A submit leaf that records arrival order and can fail one id.
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

fn recording_submit(fail_id: Option<u64>) -> (Arc<RecordingSubmit>, Arc<Mutex<Vec<u64>>>) {
    let order = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(RecordingSubmit {
            order: Arc::clone(&order),
            fail_id,
        }),
        order,
    )
}

/// Wait (bounded) for the lane to record a failure: the pipeline's own report is
/// the signal, never a guessed sleep.
async fn await_failure(pipeline: &SimSubmitPipeline<TestWork, u64>) -> PipelineFailure {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(failure) = pipeline.failure() {
            return failure;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the lane never recorded a failure"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Resolve when the runtime has nothing left to run. "A decline spawns no
/// work" is a claim about every later schedule, which no finite sleep can
/// witness: with the clock paused (`start_paused`), time only advances when
/// no task is runnable, so this resolves strictly after every task spawned
/// before it has been polled. Work a decline spawned shows up here; nothing
/// that never existed can show up late.
async fn await_quiescence() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// A submit leaf that holds the ordered lane inside one submit until the test
/// releases it, counting the submits that actually ran.
struct GatedSubmit {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    runs: Arc<AtomicU64>,
}

impl SubmitLeaf<TestWork, u64> for GatedSubmit {
    fn submit<'a>(&'a self, _work: &'a TestWork, _outcome: &'a u64) -> SubmitFuture<'a> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        let runs = Arc::clone(&self.runs);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

// ── a shutdown lane declines, side-effect free ──

#[tokio::test(start_paused = true)]
async fn enqueue_after_shutdown_is_declined_and_has_no_side_effects() {
    let sim = counting_sim(&[]);
    let calls = Arc::clone(&sim.calls);
    let (submit, order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(2, sim, submit);

    assert_eq!(pipeline.enqueue(work(1, 0)), EnqueueOutcome::Accepted);
    pipeline.shutdown().await.unwrap();

    let enqueued = pipeline.enqueued();
    let submitted = pipeline.submitted();
    let sim_calls = calls.load(Ordering::SeqCst);
    assert_eq!((enqueued, submitted, sim_calls), (1, 1, 1));

    assert_eq!(
        pipeline.enqueue(work(2, 0)),
        EnqueueOutcome::Declined(EnqueueDecline::Closed)
    );
    // The decline must leave nothing for any schedule to run later: observe
    // the "no work" claim at runtime quiescence, never at an arbitrary
    // instant after a sleep.
    await_quiescence().await;

    assert_eq!(pipeline.enqueued(), enqueued, "the decline counted nothing");
    assert_eq!(pipeline.submitted(), submitted);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        sim_calls,
        "the decline spawned no sim work"
    );
    assert_eq!(pipeline.state(), PipelineState::Closed(None));
    assert_eq!(*order.lock().unwrap(), vec![1]);
}

// ── a failed lane declines, and the FIRST failure stays authoritative ──

#[tokio::test(start_paused = true)]
async fn enqueue_after_a_leaf_failure_is_declined_and_the_first_failure_is_kept() {
    let sim = counting_sim(&[3]);
    let calls = Arc::clone(&sim.calls);
    let (submit, _order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(1, sim, submit);

    assert_eq!(pipeline.enqueue(work(3, 0)), EnqueueOutcome::Accepted);
    assert_eq!(await_failure(&pipeline).await.detail, "sim failed for 3");

    let enqueued = pipeline.enqueued();
    let sim_calls = calls.load(Ordering::SeqCst);
    assert_eq!(
        pipeline.enqueue(work(4, 0)),
        EnqueueOutcome::Declined(EnqueueDecline::Failed)
    );
    // Same contract on the failed lane: the decline's "no work" is observed
    // at runtime quiescence, not guessed at with a sleep.
    await_quiescence().await;

    assert_eq!(pipeline.enqueued(), enqueued, "the decline counted nothing");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        sim_calls,
        "the decline spawned no sim work"
    );
    assert_eq!(
        pipeline.state(),
        PipelineState::Failed(TerminalFailure {
            failure: failure("sim failed for 3"),
            reported: false,
        })
    );

    let error = pipeline
        .shutdown()
        .await
        .expect_err("the sim failure is the teardown's verdict");
    assert_eq!(error.detail, "sim failed for 3");
}

#[tokio::test]
async fn the_first_failure_in_arrival_order_is_the_authoritative_one() {
    // Both batches fail, each with its own literal, and batch 21's sim fails
    // FIRST in wall-clock time — the ordered lane awaits batch 20 first, so
    // arrival order decides which failure the pipeline keeps.
    let sim = counting_sim(&[20, 21]);
    let (submit, _order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(2, sim, submit);

    assert_eq!(pipeline.enqueue(work(20, 30)), EnqueueOutcome::Accepted);
    assert_eq!(pipeline.enqueue(work(21, 0)), EnqueueOutcome::Accepted);
    let error = pipeline.shutdown().await.expect_err("both sims failed");
    assert_eq!(error.detail, "sim failed for 20");
    assert_eq!(pipeline.failure(), Some(failure("sim failed for 20")));
}

#[tokio::test]
async fn a_submit_failure_is_terminal_for_new_work() {
    let sim = counting_sim(&[]);
    let (submit, order) = recording_submit(Some(7));
    let pipeline = SimSubmitPipeline::new(1, sim, submit);

    assert_eq!(pipeline.enqueue(work(7, 0)), EnqueueOutcome::Accepted);
    assert_eq!(await_failure(&pipeline).await.detail, "submit failed for 7");
    assert_eq!(
        pipeline.enqueue(work(8, 0)),
        EnqueueOutcome::Declined(EnqueueDecline::Failed)
    );
    let error = pipeline
        .shutdown()
        .await
        .expect_err("the submit failure is the teardown's verdict");
    assert_eq!(error.detail, "submit failed for 7");
    assert!(order.lock().unwrap().is_empty(), "nothing was submitted");
}

// ── observation reports once and never reopens admission ──

#[tokio::test]
async fn observing_the_failure_reports_once_without_reopening_admission() {
    let sim = counting_sim(&[5]);
    let (submit, _order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(1, sim, submit);

    assert_eq!(pipeline.enqueue(work(5, 0)), EnqueueOutcome::Accepted);
    assert_eq!(await_failure(&pipeline).await.detail, "sim failed for 5");

    let observed = pipeline
        .raise_if_failed()
        .expect_err("the first observation reports the failure");
    assert_eq!(observed.detail, "sim failed for 5");
    assert!(
        pipeline.raise_if_failed().is_ok(),
        "a repeated observation is a no-op"
    );

    // Reporting did not make the pipeline healthy: admission stays shut and the
    // failure stays the pipeline's own, now marked observed.
    assert_eq!(
        pipeline.enqueue(work(6, 0)),
        EnqueueOutcome::Declined(EnqueueDecline::Failed)
    );
    assert_eq!(
        pipeline.state(),
        PipelineState::Failed(TerminalFailure {
            failure: failure("sim failed for 5"),
            reported: true,
        })
    );
    assert_eq!(pipeline.failure(), Some(failure("sim failed for 5")));

    // Teardown never reports a fresh success that contradicts the state.
    let first = pipeline
        .shutdown()
        .await
        .expect_err("a failure is still the verdict");
    assert_eq!(first.detail, "sim failed for 5");
    let second = pipeline
        .shutdown()
        .await
        .expect_err("the same failure, every call");
    assert_eq!(second.detail, "sim failed for 5");
    assert_eq!(
        pipeline.state(),
        PipelineState::Closed(Some(TerminalFailure {
            failure: failure("sim failed for 5"),
            reported: true,
        }))
    );
}

// ── an idempotent shutdown ──

#[tokio::test]
async fn shutdown_is_idempotent_and_joins_the_submitter_once() {
    let sim = counting_sim(&[]);
    let (submit, order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(2, sim, submit);

    for id in [10_u64, 11, 12] {
        assert_eq!(pipeline.enqueue(work(id, 1)), EnqueueOutcome::Accepted);
    }
    pipeline.shutdown().await.unwrap();
    assert_eq!((pipeline.enqueued(), pipeline.submitted()), (3, 3));
    assert_eq!(pipeline.state(), PipelineState::Closed(None));

    // A second (and third) teardown joins nothing, re-reports no success a
    // failed lane would contradict, and never re-runs the lane.
    tokio::time::timeout(Duration::from_secs(1), pipeline.shutdown())
        .await
        .expect("a repeated shutdown returns instead of awaiting a join twice")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), pipeline.shutdown())
        .await
        .expect("still joins nothing")
        .unwrap();

    assert_eq!(pipeline.submitted(), 3, "no batch was submitted twice");
    assert_eq!(pipeline.state(), PipelineState::Closed(None));
    assert_eq!(*order.lock().unwrap(), vec![10, 11, 12]);
}

// ── concurrent teardown: the second caller joins the first drain ──

#[tokio::test]
async fn a_concurrent_shutdown_waits_for_the_first_drain_to_finish() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let runs = Arc::new(AtomicU64::new(0));
    let pipeline = Arc::new(SimSubmitPipeline::new(
        1,
        counting_sim(&[]),
        Arc::new(GatedSubmit {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            runs: Arc::clone(&runs),
        }),
    ));

    assert_eq!(pipeline.enqueue(work(1, 0)), EnqueueOutcome::Accepted);

    // The first caller starts the teardown and gets as far as the gated submit:
    // the drain is in progress and the lane is `Closing`, not done.
    let first = {
        let pipeline = Arc::clone(&pipeline);
        tokio::spawn(async move { pipeline.shutdown().await })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the drain reached the gated submit");
    assert_eq!(pipeline.state(), PipelineState::Closing(None));

    // A second caller must wait for that drain: it may neither return early
    // (reporting a success the in-flight submit contradicts) nor close the lane
    // underneath the running submitter.
    let mut second = {
        let pipeline = Arc::clone(&pipeline);
        tokio::spawn(async move { pipeline.shutdown().await })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "a concurrent shutdown returned before the first drain finished"
    );
    assert_eq!(
        pipeline.state(),
        PipelineState::Closing(None),
        "a concurrent shutdown closed the lane mid-drain"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the submit is still gated");

    release.notify_one();
    first
        .await
        .unwrap()
        .expect("the first drain reports no failure");
    second
        .await
        .unwrap()
        .expect("the second caller reports the same verdict");

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the submit ran exactly once"
    );
    assert_eq!((pipeline.enqueued(), pipeline.submitted()), (1, 1));
    assert_eq!(pipeline.state(), PipelineState::Closed(None));
}

// ── preserved lane contract under the lifecycle ──

#[tokio::test]
async fn reverse_sim_completion_still_submits_in_fifo_order() {
    let sim = counting_sim(&[]);
    let (submit, order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(4, sim, submit);

    // Later batches sleep less, so sim completion order is the reverse of
    // arrival order; the single submit lane must still be FIFO (= nonce order).
    for (id, sleep_ms) in [(10_u64, 20), (11, 10), (12, 5), (13, 1)] {
        assert_eq!(
            pipeline.enqueue(work(id, sleep_ms)),
            EnqueueOutcome::Accepted
        );
    }
    pipeline.shutdown().await.unwrap();

    assert_eq!(*order.lock().unwrap(), vec![10, 11, 12, 13]);
    assert_eq!((pipeline.enqueued(), pipeline.submitted()), (4, 4));
}

#[tokio::test]
async fn the_sim_fan_out_stays_bounded_and_skips_are_counted() {
    let sim = counting_sim(&[]);
    let max_in_flight = Arc::clone(&sim.max_in_flight);
    let (submit, order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(2, sim, submit);

    for id in 0..5_u64 {
        assert_eq!(pipeline.enqueue(work(id, 10)), EnqueueOutcome::Accepted);
    }
    pipeline.shutdown().await.unwrap();

    let peak = max_in_flight.load(Ordering::SeqCst);
    assert!(peak <= 2, "peak {peak} exceeded the concurrency bound");
    assert!(peak >= 2, "peak {peak} shows no concurrency");
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn an_ok_none_outcome_is_skipped_but_still_counted() {
    struct NoneSim;
    impl SimLeaf<TestWork, u64> for NoneSim {
        fn simulate<'a>(&'a self, _work: &'a TestWork) -> SimFuture<'a, u64> {
            Box::pin(async { Ok(None) })
        }
    }

    let (submit, order) = recording_submit(None);
    let pipeline = SimSubmitPipeline::new(1, Arc::new(NoneSim), submit);
    assert_eq!(pipeline.enqueue(work(1, 0)), EnqueueOutcome::Accepted);
    pipeline.shutdown().await.unwrap();

    assert!(order.lock().unwrap().is_empty(), "nothing was submitted");
    assert_eq!(pipeline.submitted(), 1, "the skip is still counted");
}

// ── the queue-send failure takes the leaf-failure transition ──

#[tokio::test]
async fn a_queue_send_failure_takes_the_leaf_failure_transition() {
    /// Panics on its first batch: the submitter task dies, and its queue
    /// receiver dies with it.
    struct PanickingSubmit;

    impl SubmitLeaf<TestWork, u64> for PanickingSubmit {
        fn submit<'a>(&'a self, _work: &'a TestWork, _outcome: &'a u64) -> SubmitFuture<'a> {
            Box::pin(async { panic!("the submit leaf exploded") })
        }
    }

    let pipeline = SimSubmitPipeline::new(1, counting_sim(&[]), Arc::new(PanickingSubmit));
    assert_eq!(pipeline.enqueue(work(1, 0)), EnqueueOutcome::Accepted);

    // Nothing announces the dead submitter: the failed queue send IS the
    // discovery, and it must land on the same terminal transition a leaf
    // failure does. A failed send counts nothing, so probing is side-effect
    // free apart from the batches the live lane accepted while it lasted.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let (decline, enqueued_before) = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the queue send never failed"
        );
        let enqueued_before = pipeline.enqueued();
        match pipeline.enqueue(work(1, 0)) {
            EnqueueOutcome::Accepted => tokio::time::sleep(Duration::from_millis(2)).await,
            EnqueueOutcome::Declined(decline) => break (decline, enqueued_before),
        }
    };

    assert_eq!(decline, EnqueueDecline::Failed);
    assert_eq!(
        pipeline.enqueued(),
        enqueued_before,
        "the declined batch was not counted"
    );
    assert_eq!(
        pipeline.failure(),
        Some(failure("pipeline submitter dropped"))
    );
    assert_eq!(
        pipeline
            .raise_if_failed()
            .expect_err("the send failure is reported")
            .detail,
        "pipeline submitter dropped"
    );
    assert_eq!(
        pipeline
            .shutdown()
            .await
            .expect_err("the pipeline stays failed")
            .detail,
        "pipeline submitter dropped"
    );
}
