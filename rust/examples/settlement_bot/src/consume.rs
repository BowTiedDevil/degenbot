//! Result-batch consumption + the session block clock — parity-ledger
//! rows 7 + 16 (ergo `L4E7RI`, Gap G4).
//!
//! Mirrors `src/degenbot/runner/_consume.py::consume_result_batches`: the
//! consumer awaits the `EngineDriver`'s `ResultBatch` stream in per-block
//! order, advances the session block clock, and fans each batch into the
//! sim/submit pipeline. A natural stream end is the pump-death signal
//! (ADR-050 D6 — `EngineDriver::stop` closes the channel so a pending
//! `recv().await` sees end-of-stream exactly once); without
//! `allow_quiet_end` it aborts loudly, mirroring the Python "no silent pump
//! death" rule (incident 2026-08-20).
//!
//! The consumer is deliberately generic over the batch sink so the whole
//! loop is unit-testable without RPC: the offline tests drive a
//! `tokio::sync::mpsc` channel in place of `EngineDriver::take_result_receiver`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use degenbot::bot::arb_engine::ResultBatch;

/// The session block clock (the consumer-owned `[block: N]` state).
///
/// Python's clock is driven by the forwarded `newHeads` block stream; the
/// driver example advances it from each `ResultBatch`'s `solve_block` so the
/// batch stream is the single clock source (ADR-050 D3 exposes both receivers;
/// this example consumes the result stream — the block receiver is a G5
/// follow-up,).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockClock {
    /// The latest accepted block number.
    pub current_block: u64,
    /// The latest accepted block timestamp.
    pub block_timestamp: u64,
    /// The EIP-1559 base fee of the NEXT block (`degenbot_core::eip_1559`).
    pub base_fee_next: u128,
}

impl BlockClock {
    /// Advance the clock from a `ResultBatch` if the batch is not stale.
    ///
    /// Returns `true` when the clock moved (the batch's `solve_block` is at
    /// or beyond the current block), `false` for a stale/out-of-order batch.
    #[must_use]
    pub fn advance(&mut self, batch: &ResultBatch) -> bool {
        if batch.solve_block < self.current_block {
            return false;
        }
        self.current_block = batch.solve_block;
        self.block_timestamp = batch.timestamp;
        self.base_fee_next = degenbot::eip_1559::next_base_fee(
            u128::from(batch.base_fee_per_gas.unwrap_or(0)),
            u128::from(batch.gas_used),
            u128::from(batch.gas_limit),
            None,
            degenbot::eip_1559::DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR,
            degenbot::eip_1559::DEFAULT_ELASTICITY_MULTIPLIER,
        );
        true
    }
}

/// A lock-free progress view of the consume loop, read by the run-loop
/// heartbeat . The consumer records each batch's
/// block; the heartbeat task reads without blocking or locking.
#[derive(Clone, Debug, Default)]
pub struct SessionProgress {
    batches: Arc<AtomicU64>,
    blocks: Arc<AtomicU64>,
    current_block: Arc<AtomicU64>,
}

impl SessionProgress {
    /// A fresh, zeroed progress view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one consumed batch at the consumer's current clock value.
    ///
    /// A batch is not a block: under streaming delivery one block fans out
    /// into many single-entry batches, so the batch count and the distinct
    /// block count are tracked separately. `blocks` advances only when the
    /// clock's block number actually changes.
    pub fn note(&self, clock: &BlockClock) {
        self.batches.fetch_add(1, Ordering::Relaxed);
        let previous = self
            .current_block
            .swap(clock.current_block, Ordering::Relaxed);
        if clock.current_block != previous {
            self.blocks.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Batches consumed so far.
    #[must_use]
    pub fn batches(&self) -> u64 {
        self.batches.load(Ordering::Relaxed)
    }

    /// Distinct block numbers the consumer's clock has advanced through.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    /// The consumer's current block (0 before the first batch).
    #[must_use]
    pub fn current_block(&self) -> u64 {
        self.current_block.load(Ordering::Relaxed)
    }
}

/// The boxed future a batch sink returns (the pipeline's async `enqueue`).
pub type SinkFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// A consumer-side sink for one ordered `ResultBatch`.
///
/// Production wires this to the sim/submit pipeline's `enqueue`; the offline
/// tests wire a recording sink. `Send + Sync` because the consumer may be
/// driven on any runtime worker.
pub trait BatchSink: Send + Sync {
    /// Handle one batch at the current clock value.
    fn on_batch<'a>(&'a self, batch: &'a ResultBatch, clock: &'a BlockClock) -> SinkFuture<'a>;
}

/// The natural-end / abort verdict for the consumer loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerEnd {
    /// The stream ended naturally and `allow_quiet_end` was set.
    QuietEnd,
    /// The stream ended naturally and quiet-end was NOT allowed — the pump
    /// died; the caller must abort (Python raises `RuntimeError`).
    PumpStopped,
}

/// A consumer-loop failure (batch-sink error), surfaced loudly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumerError {
    /// The batch index whose sink failed (0-based).
    pub batch_index: u64,
    /// The failure detail from the sink.
    pub detail: String,
}

/// The consumer-loop result: how many batches were consumed and how the
/// stream ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerReport {
    /// Batches consumed in order.
    pub batches: u64,
    /// The number of natural end-of-stream observations (always 1, per
    /// ADR-050 D6 — the channel closes once).
    pub end_of_stream: u64,
    /// The terminal verdict.
    pub end: Option<ConsumerEnd>,
}

/// Consume the result-batch stream in per-block order until the channel
/// closes.
///
/// Each batch advances `clock` then is handed to `sink` in arrival order.
/// A natural end (the driver stopped) yields [`ConsumerEnd::PumpStopped`]
/// unless `allow_quiet_end`, in which case it yields
/// [`ConsumerEnd::QuietEnd`].
///
/// # Errors
///
/// Returns [`ConsumerError`] if the batch sink fails; the caller aborts
/// loudly rather than swallowing the failure.
pub async fn consume_result_batches(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<ResultBatch>,
    clock: &mut BlockClock,
    sink: &dyn BatchSink,
    allow_quiet_end: bool,
) -> Result<ConsumerReport, ConsumerError> {
    let mut report = ConsumerReport::default();
    while let Some(batch) = rx.recv().await {
        let _advanced = clock.advance(&batch);
        let index = report.batches;
        if let Err(detail) = sink.on_batch(&batch, clock).await {
            return Err(ConsumerError {
                batch_index: index,
                detail,
            });
        }
        report.batches += 1;
    }
    // End-of-stream exactly once: `recv` returned `None` and the branch is
    // not retried (ADR-050 D6).
    report.end_of_stream = 1;
    report.end = Some(if allow_quiet_end {
        ConsumerEnd::QuietEnd
    } else {
        ConsumerEnd::PumpStopped
    });
    Ok(report)
}

/// A sink that beats the session-watch heartbeat for each consumed batch
/// (G5 session watch,) while counting nothing else.
struct HeartbeatSink {
    heartbeat: Option<crate::session_watch::Heartbeat>,
    progress: Option<SessionProgress>,
}

impl BatchSink for HeartbeatSink {
    fn on_batch<'a>(&'a self, _batch: &'a ResultBatch, clock: &'a BlockClock) -> SinkFuture<'a> {
        Box::pin(async move {
            if let Some(heartbeat) = &self.heartbeat {
                heartbeat.beat();
            }
            if let Some(progress) = &self.progress {
                progress.note(clock);
            }
            Ok(())
        })
    }
}

/// Convenience runner for the live arm: consume the driver's result stream
/// with an optional heartbeat sink, returning the report + the final block
/// clock.
///
/// `allow_quiet_end` is `true` because the driver's `stop()` is the intended
/// teardown (ADR-050 D6); the loud pump-death branch belongs to a supervised
/// consumer (G5 session watch,).
///
/// # Errors
///
/// Returns [`ConsumerError`] if the sink fails.
pub async fn run_result_consumer_watched(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<ResultBatch>,
    heartbeat: Option<crate::session_watch::Heartbeat>,
    progress: Option<SessionProgress>,
) -> Result<(ConsumerReport, BlockClock), ConsumerError> {
    let sink = HeartbeatSink {
        heartbeat,
        progress,
    };
    let mut clock = BlockClock::default();
    let report = consume_result_batches(&mut rx, &mut clock, &sink, true).await?;
    Ok((report, clock))
}

/// Convenience runner without a heartbeat (kept for the offline tests).
///
/// # Errors
///
/// Returns [`ConsumerError`] if the sink fails.
pub async fn run_result_consumer(
    rx: tokio::sync::mpsc::UnboundedReceiver<ResultBatch>,
) -> Result<(ConsumerReport, BlockClock), ConsumerError> {
    run_result_consumer_watched(rx, None, None).await
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use degenbot::bot::arb_engine::ResultBatch;

    #[expect(
        clippy::default_trait_access,
        reason = "the payload map's hashbrown value type is not nameable without a direct dep"
    )]
    fn batch(solve_block: u64) -> ResultBatch {
        ResultBatch {
            solve_block,
            timestamp: 1_700_000_000 + solve_block,
            base_fee_per_gas: Some(1_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
            payloads: Default::default(),
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        seen: Mutex<Vec<(u64, u64)>>,
        fail_on: Option<u64>,
    }

    impl BatchSink for RecordingSink {
        fn on_batch<'a>(&'a self, batch: &'a ResultBatch, clock: &'a BlockClock) -> SinkFuture<'a> {
            Box::pin(async move {
                if self.fail_on == Some(batch.solve_block) {
                    return Err("sim leaf failed".to_string());
                }
                self.seen
                    .lock()
                    .unwrap()
                    .push((batch.solve_block, clock.current_block));
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn consumes_batches_in_order_and_marks_end_once() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for b in [100_u64, 101, 102] {
            tx.send(batch(b)).unwrap();
        }
        drop(tx);
        let sink = RecordingSink::default();
        let mut clock = BlockClock::default();
        let report = consume_result_batches(&mut rx, &mut clock, &sink, false)
            .await
            .unwrap();
        assert_eq!(report.batches, 3);
        assert_eq!(report.end_of_stream, 1);
        assert_eq!(report.end, Some(ConsumerEnd::PumpStopped));
        assert_eq!(clock.current_block, 102);
        let seen = sink.seen.lock().unwrap().clone();
        assert_eq!(seen, vec![(100, 100), (101, 101), (102, 102)]);
    }

    #[tokio::test]
    async fn natural_end_is_loud_unless_quiet_allowed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ResultBatch>();
        drop(tx);
        let sink = RecordingSink::default();
        let mut clock = BlockClock::default();
        let loud = consume_result_batches(&mut rx, &mut clock, &sink, false)
            .await
            .unwrap();
        assert_eq!(loud.end, Some(ConsumerEnd::PumpStopped));

        // A second consumer over an already-closed channel still observes the
        // single terminal end (idempotent close).
        let quiet = consume_result_batches(&mut rx, &mut clock, &sink, true)
            .await
            .unwrap();
        assert_eq!(quiet.end_of_stream, 1);
        assert_eq!(quiet.end, Some(ConsumerEnd::QuietEnd));
    }

    #[tokio::test]
    async fn sink_failure_aborts_with_batch_index() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(batch(5)).unwrap();
        tx.send(batch(6)).unwrap();
        drop(tx);
        let sink = RecordingSink {
            seen: Mutex::new(Vec::new()),
            fail_on: Some(6),
        };
        let mut clock = BlockClock::default();
        let err = consume_result_batches(&mut rx, &mut clock, &sink, false)
            .await
            .unwrap_err();
        assert_eq!(err.batch_index, 1);
        assert_eq!(err.detail, "sim leaf failed");
    }

    #[test]
    fn session_progress_counts_batches_and_distinct_blocks_separately() {
        let progress = SessionProgress::new();
        let at = |current_block| BlockClock {
            current_block,
            ..BlockClock::default()
        };
        // Streaming delivery fans one block out into many batches.
        progress.note(&at(100));
        progress.note(&at(100));
        progress.note(&at(100));
        assert_eq!(progress.batches(), 3);
        assert_eq!(progress.blocks(), 1);
        assert_eq!(progress.current_block(), 100);
        // The next block advances each by exactly one.
        progress.note(&at(101));
        assert_eq!(progress.batches(), 4);
        assert_eq!(progress.blocks(), 2);
        assert_eq!(progress.current_block(), 101);
    }

    #[test]
    fn stale_batch_does_not_rewind_the_clock() {
        let mut clock = BlockClock {
            current_block: 10,
            ..BlockClock::default()
        };
        assert!(!clock.advance(&batch(9)));
        assert_eq!(clock.current_block, 10);
        assert!(clock.advance(&batch(11)));
        assert_eq!(clock.current_block, 11);
    }
}
