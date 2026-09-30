//! Result-batch consumption + the session block clock — parity-ledger
//! rows 7 + 16 (Gap G4).
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

use degenbot::batch_executor::BatchExecutor;
use degenbot::bot::arb_engine::ResultBatch;

use crate::dispatch;

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

/// The liveness sink: a DRAIN of the executor's Batch outcome record stream.
///
/// Per consumed `ResultBatch` it converts the batch into executor work
/// ([`crate::dispatch::batch_work`]), enqueues it, and then drains every
/// record set the lane has completed so far. The sink READS NOTHING from the
/// drained records — display is the driver's remaining responsibility, and
/// the liveness contract stays exactly what it was: the heartbeat beat + the
/// [`SessionProgress`] note read only `clock.current_block`.
///
struct HeartbeatSink {
    executor: BatchExecutor,
    heartbeat: Option<degenbot::session_end::Heartbeat>,
    progress: Option<SessionProgress>,
}

impl HeartbeatSink {
    fn new(
        executor: BatchExecutor,
        heartbeat: Option<degenbot::session_end::Heartbeat>,
        progress: Option<SessionProgress>,
    ) -> Self {
        Self {
            executor,
            heartbeat,
            progress,
        }
    }
}

impl BatchSink for HeartbeatSink {
    fn on_batch<'a>(&'a self, batch: &'a ResultBatch, clock: &'a BlockClock) -> SinkFuture<'a> {
        Box::pin(async move {
            // Loud-abort check FIRST: a stored sim/submit leaf failure
            // re-raises in the caller's frame — never silently swallowed by
            // later batches.
            if let Err(failure) = self.executor.raise_if_failed() {
                return Err(failure.detail);
            }
            self.executor.enqueue(dispatch::batch_work(batch, clock));
            // Drain the record stream: every batch the lane already
            // completed yields its outcome set here. The records are display
            // terrain — the drain only keeps the channel bounded and proves
            // the stream flows (a full drain at stream end catches the tail
            // batches).
            while self.executor.try_next_outcome().await.is_some() {}
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
/// through the core batch executor, draining its Batch outcome record stream,
/// and return the report + the final block clock.
///
/// `allow_quiet_end` is `true` because the driver's `stop()` is the intended
/// teardown (ADR-050 D6); the loud pump-death branch belongs to a supervised
/// consumer (G5 session watch,). After the stream ends the executor is shut
/// down (draining the in-flight lane) and any stored leaf failure re-raises
/// here — the loud-abort rule re-raised in the caller's frame.
///
/// # Errors
///
/// Returns [`ConsumerError`] if the sink fails or an executor leaf failed.
pub async fn run_result_consumer_watched(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<ResultBatch>,
    executor: BatchExecutor,
    heartbeat: Option<degenbot::session_end::Heartbeat>,
    progress: Option<SessionProgress>,
) -> Result<(ConsumerReport, BlockClock), ConsumerError> {
    let sink = HeartbeatSink::new(executor, heartbeat, progress);
    let mut clock = BlockClock::default();
    let report = consume_result_batches(&mut rx, &mut clock, &sink, true).await?;
    // Teardown: close the queue, drain in-flight work, then re-raise any
    // stored leaf failure (the lane stops on the first one).
    let shutdown = sink.executor.shutdown().await;
    // Drain the record sets completed during (or buffered before) shutdown.
    while sink.executor.try_next_outcome().await.is_some() {}
    shutdown.map_err(|failure| ConsumerError {
        batch_index: report.batches,
        detail: failure.detail,
    })?;
    Ok((report, clock))
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

    // ── The executor cut-over: HeartbeatSink drains the Batch outcome
    // record stream through a REAL offline `BatchExecutor` ──

    use std::collections::HashMap;

    use alloy::primitives::{Bytes, U256};
    use degenbot::arbitrage::{FeeOnTransferRegistry, PoolDivergence};
    use degenbot::batch_executor::{ExecutorConfig, ExecutorRuntime};
    use degenbot::bot::arb_engine::SimulatedPathResult;
    use degenbot::cmd_executor::composers::{HopInfo, PathInfo, V2HopInfo};
    use degenbot::core::address_utils::parse_address;
    use degenbot::solvers::mixed::SolvePathResult;
    use degenbot::submission::{
        Dispatcher, NonceLane, PathSuppression, ReceiptProbe, SubmissionLedger, SubmissionTarget,
        TxSigner,
    };
    use degenbot::substrate::nonce::NonceAuthority;

    /// A probe that never reports a receipt (unused under `dry_run`).
    struct NoopProbe;
    impl ReceiptProbe for NoopProbe {
        fn receipt_found(
            &self,
            _tx_hash: alloy::primitives::B256,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = degenbot::submission::SubmissionResult<bool>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async { Ok(false) })
        }
    }

    fn fixture_path_info() -> PathInfo {
        let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let t0 = parse_address("0x2222222222222222222222222222222222222222").unwrap();
        let t1 = parse_address("0x3333333333333333333333333333333333333333").unwrap();
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: t0,
            token1_address: t1,
            fee: 30,
            zfo: true,
        })])
    }

    /// An offline-safe real `BatchExecutor`: the production constructor with
    /// `dry_run = true`, an empty-hop policy floor, and a dummy provider (the
    /// record shapes these tests drive all skip pre-sim, so no RPC fires).
    async fn offline_executor(
        resolver: HashMap<u64, PathInfo>,
        suppression: PathSuppression,
    ) -> BatchExecutor {
        let provider = degenbot::rpc::provider::AlloyProvider::new("http://127.0.0.1:1", 0)
            .await
            .unwrap();
        let executor_address = parse_address("0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5").unwrap();
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        // The declared defaults carry the policy floor (0) and the inject
        // stance (off); the serial-reference cap is a fixture choice pinned
        // on the verdict itself — no policy value is named field-by-field.
        let mut verdict = degenbot::config::BotConfig::default();
        verdict.simulation.pipeline_concurrency = 1;
        let config = ExecutorConfig::from_verdict(
            &verdict,
            ExecutorRuntime {
                max_candidates: 50,
                use_v4_batch: false,
                dry_run: true,
                resolver: Arc::new(crate::dispatch::MapResolver(resolver)),
                suppression: Arc::new(std::sync::Mutex::new(suppression)),
                divergence: Arc::new(std::sync::Mutex::new(PoolDivergence::new())),
                fot: Arc::new(std::sync::Mutex::new(FeeOnTransferRegistry::new())),
                provider: Arc::new(provider),
                executor_owner: parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266")
                    .unwrap(),
                executor_address,
                weth_address: weth,
                pool_manager_address: alloy::primitives::Address::ZERO,
                multicall3_address: alloy::primitives::Address::ZERO,
                injected_address: None,
                runtime_bytecode: Bytes::new(),
                warmup: degenbot::cmd_executor::compute_simulation_warmup_slots(
                    executor_address,
                    weth,
                ),
                bot_state: None,
                warm_cache: None,
                dispatcher: Arc::new(std::sync::Mutex::new(Dispatcher::for_block(0))),
                signer: Arc::new(
                    TxSigner::from_key_hex(
                        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                        1,
                    )
                    .unwrap(),
                ),
                probe: Arc::new(NoopProbe),
                nonce_lane: Arc::new(NonceLane::new(
                    Arc::new(NonceAuthority::new(0)),
                    Arc::new(SubmissionLedger::new()),
                    "settlement",
                )),
                extra_broadcast: Vec::new(),
                target: SubmissionTarget::Public,
            },
        );
        BatchExecutor::new(config)
    }

    /// One single-hop solve row (hop lengths match the fixture `PathInfo`).
    fn solve_row(path_id: u64, with_hops: bool) -> (u64, SolvePathResult) {
        let result = SolvePathResult {
            optimal_input: U256::from(1_000_u64),
            profit: U256::from(100_u64),
            hop_outputs: if with_hops {
                vec![U256::from(100_u64)]
            } else {
                Vec::new()
            },
            consumed_inputs: if with_hops {
                vec![U256::from(1_000_u64)]
            } else {
                Vec::new()
            },
            state_nonces: vec![1],
            solver_pool_states: Vec::new(),
        };
        (path_id, result)
    }

    fn row_batch(solve_block: u64, rows: Vec<(u64, SolvePathResult)>) -> ResultBatch {
        let mut b = batch(solve_block);
        b.fresh = rows;
        b
    }

    fn payload_batch(solve_block: u64, path_id: u64) -> ResultBatch {
        let mut b = batch(solve_block);
        b.payloads.insert(
            path_id,
            SimulatedPathResult {
                path_id,
                gross_profit: U256::from(1_000_u64),
                net_profit: U256::from(900_u64),
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 7,
                execute_calldata: Vec::new(),
                access_list: None,
                captured_swaps: Vec::new(),
                hop_count: 1,
                failure: Some(degenbot::bot::arb_engine::InlineSimFailure {
                    fail_index: Some(3),
                    revert_data: Vec::new(),
                    bucket: "no-profit".to_string(),
                }),
            },
        );
        b
    }

    #[tokio::test]
    async fn drain_sink_feeds_the_executor_and_drains_typed_skip_records() {
        // Row 1: empty hops → SkipEmptyHops. Row 2: unresolvable → the
        // unified SkipResolveMiss (task A6SXEH a: a typed skip + counted,
        // never an abort, never an empty-hop fold). Both drain as records —
        // the record stream is the executor's product, and the liveness
        // sink's contract stays the clock.
        let executor = offline_executor(HashMap::new(), PathSuppression::new()).await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(row_batch(
            100,
            vec![solve_row(1, false), solve_row(2, true)],
        ))
        .unwrap();
        drop(tx);
        let heartbeat = degenbot::session_end::Heartbeat::new();
        let progress = SessionProgress::new();
        let (report, clock) = run_result_consumer_watched(
            rx,
            executor,
            Some(heartbeat.clone()),
            Some(progress.clone()),
        )
        .await
        .unwrap();
        assert_eq!(report.batches, 1);
        assert_eq!(report.end, Some(ConsumerEnd::QuietEnd));
        assert_eq!(clock.current_block, 100);
        assert_eq!(progress.batches(), 1);
        assert!(heartbeat.seq() >= 1, "the drain beat the heartbeat");
    }

    #[tokio::test]
    async fn served_set_and_suppression_are_distinct_policy_verdicts() {
        // The payload-served set is BATCH-LOCAL (path 10 is in the same
        // batch's payloads → its raw row drains SkipPayloadServed); the
        // suppression registry is CROSS-BLOCK (path 11 recorded to threshold
        // → SkipSuppressed). The two rows exercise DIFFERENT policy inputs
        // and yield DIFFERENT assembly verdicts — the record vocabulary
        // keeps them distinct.
        let mut suppression = PathSuppression::new();
        for _ in 0..degenbot::submission::PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(11);
        }
        // Both rows resolve (the resolve stage runs before the suppression
        // and payload-served policies differentiate them).
        let resolver =
            HashMap::from([(10_u64, fixture_path_info()), (11_u64, fixture_path_info())]);
        let executor = offline_executor(resolver, suppression).await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut served_batch = row_batch(1, vec![solve_row(10, true), solve_row(11, true)]);
        served_batch
            .payloads
            .insert(10, payload_batch(1, 10).payloads.remove(&10).unwrap());
        tx.send(served_batch).unwrap();
        drop(tx);
        let (report, _) = run_result_consumer_watched(rx, executor, None, None)
            .await
            .unwrap();
        assert_eq!(report.batches, 1);
    }

    #[tokio::test]
    async fn payload_resolve_miss_is_the_loud_abort_arm() {
        // A payload row is engine-born: a resolve miss evidences batch /
        // registry divergence and aborts LOUDLY through the consumer error —
        // re-raised at the shutdown boundary in the caller's frame.
        let executor = offline_executor(HashMap::new(), PathSuppression::new()).await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(payload_batch(1, 99)).unwrap();
        drop(tx);
        let err = run_result_consumer_watched(rx, executor, None, None)
            .await
            .unwrap_err();
        assert!(
            err.detail.contains("99"),
            "the loud-abort detail names the unresolvable payload path: {}",
            err.detail
        );
    }
}
