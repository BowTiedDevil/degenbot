//! The run loop's consumer adapter: the PULL side of the session consumer.
//!
//! The consume loop itself — the block clock, the per-batch work derivation, the
//! ordered lane enqueue, the loud-abort rule, and the stall watchdog's feed —
//! is the core's ONE implementation
//! (`degenbot::batch_executor::SessionConsumer`). This module contributes only
//! what an adapter owns: wiring the session's hand-off and driver into the
//! consumer, draining the Batch outcome record stream so it cannot back up, and
//! rendering the typed report for the operator.
//!
//! Rendering is pull-based: the loop publishes records, and the drain here reads
//! them (this driver reads nothing off them — display beyond the report is the
//! cockpit's job).

use std::sync::Arc;

use degenbot::batch_executor::{BatchExecutor, SessionConsumer, SessionReport};

/// Drain the executor's Batch outcome record stream to its end.
///
/// The consumer quiesces the lane when its loop ends, which closes this stream:
/// the drain terminates on its own, so the session's teardown never has to
/// cancel it.
pub async fn drain_records(executor: Arc<BatchExecutor>) {
    while executor.next_outcome().await.is_some() {}
}

/// Run one attached session consumer to its end, draining the record stream
/// concurrently.
///
/// The consumer has already attached the hand-off and started production (the
/// driver's `run` is the consumer's move, so production cannot start without the
/// attached-consumer proof). Its `run` consumes the ordered stream, quiesces the
/// lane, and closes the session — the Adr-050 D6 teardown order, owned in one
/// place.
///
/// # Errors
///
/// The consumer's loud-abort detail, naming the batch whose pass failed.
pub async fn run_session_consumer(
    mut consumer: SessionConsumer,
    executor: Arc<BatchExecutor>,
) -> Result<SessionReport, String> {
    let drain = tokio::spawn(drain_records(executor));
    let outcome = consumer.run().await;
    // The loop quiesced the lane, so this join is the drain's own end.
    let _ = drain.await;
    outcome.map_err(|failure| {
        format!(
            "{} (batch {})",
            failure.error.detail, failure.error.batch_index
        )
    })
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::default_trait_access,
    reason = "tests drive known-valid fixtures and assert on the typed product; the payload map's hashbrown value type is not nameable without a direct dependency"
)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use alloy::primitives::{Bytes, U256};
    use degenbot::arbitrage::{FeeOnTransferRegistry, PoolDivergence};
    use degenbot::batch_executor::{
        ConsumerPolicy, SessionClock, SessionConsumer, SessionProgress,
    };
    use degenbot::bot::arb_engine::SimulatedPathResult;
    use degenbot::bot::arb_engine::{BlockNotification, Heartbeat, ResultBatch, SessionEvent};
    use degenbot::core::address_utils::parse_address;
    use degenbot::execution::{HopInfo, PathInfo, V2HopInfo};
    use degenbot::rpc::provider::AlloyProvider;
    use degenbot::solvers::SolvePathResult;
    use degenbot::submission::{
        Dispatcher, PathSuppression, ReceiptProbe, SubmissionLedger, SubmissionTarget, TxSigner,
    };
    use degenbot::substrate::nonce::NonceAuthority;

    use super::*;

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

    /// An offline-safe real executor: the production constructor with
    /// `dry_run = true`, a single-slot lane, and a dead-port provider (every
    /// scripted row skips pre-sim, so no RPC fires).
    async fn offline_executor(resolver: HashMap<u64, PathInfo>) -> Arc<BatchExecutor> {
        let provider = Arc::new(
            AlloyProvider::new("http://127.0.0.1:1", 0)
                .await
                .expect("a lazy provider constructs offline"),
        );
        let executor_address = parse_address("0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5").unwrap();
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let mut verdict = degenbot::config::BotConfig::default();
        verdict.simulation.pipeline_concurrency = 1;
        let config = degenbot::batch_executor::ExecutorConfig::from_verdict(
            &verdict,
            degenbot::batch_executor::ExecutorRuntime {
                max_candidates: 50,
                use_v4_batch: false,
                dry_run: true,
                resolver: Arc::new(crate::dispatch::MapResolver(resolver)),
                suppression: Arc::new(Mutex::new(PathSuppression::new())),
                divergence: Arc::new(Mutex::new(PoolDivergence::new())),
                fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
                provider,
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
                dispatcher: Arc::new(Mutex::new(Dispatcher::for_block(0))),
                signer: Arc::new(
                    TxSigner::from_key_hex(
                        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                        1,
                    )
                    .unwrap(),
                ),
                probe: Arc::new(NoopProbe),
                nonce_lane: Arc::new(Arc::new(NonceAuthority::new(0)).mint_lane(
                    "settlement",
                    Arc::new(SubmissionLedger::new()),
                    degenbot::substrate::nonce::LanePolicy::default(),
                )),
                extra_broadcast: Vec::new(),
                target: SubmissionTarget::Public,
            },
        );
        Arc::new(BatchExecutor::new(config))
    }

    fn block(number: u64, timestamp: u64) -> BlockNotification {
        BlockNotification {
            number,
            timestamp,
            base_fee_per_gas: Some(1_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
        }
    }

    fn empty_batch(solve_block: u64) -> ResultBatch {
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

    fn solve_row() -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000_u64),
            profit: U256::from(100_u64),
            hop_outputs: Vec::new(),
            consumed_inputs: Vec::new(),
            state_nonces: Vec::new(),
            solver_pool_states: Vec::new(),
            ..Default::default()
        }
    }

    fn consumer(executor: Arc<BatchExecutor>) -> (SessionConsumer, SessionProgress) {
        let progress = SessionProgress::new();
        let policy = ConsumerPolicy {
            allow_quiet_end: true,
            heartbeat: Some(Heartbeat::new()),
            progress: progress.clone(),
        };
        (
            SessionConsumer::new(
                executor,
                SessionClock::new(Arc::new(Mutex::new(Dispatcher::for_block(0)))),
                policy,
            ),
            progress,
        )
    }

    /// The umbrella consumer surface both first-class consumers drive: a
    /// scripted ordered stream runs through the loop into the ordered lane, and
    /// the drain reads the executor's typed records.
    #[tokio::test]
    async fn the_consumer_runs_a_scripted_stream_into_the_ordered_lane() {
        let executor = offline_executor(HashMap::new()).await;
        let (mut consumer, progress) = consumer(Arc::clone(&executor));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();

        let mut first = empty_batch(100);
        first.fresh.push((1, solve_row()));
        let mut second = empty_batch(101);
        second.fresh.push((2, solve_row()));
        for event in [
            SessionEvent::Block(block(100, 1_700_000_100)),
            SessionEvent::Batch(first),
            SessionEvent::Block(block(101, 1_700_000_101)),
            SessionEvent::Batch(second),
        ] {
            tx.send(event).unwrap();
        }
        drop(tx);

        let report = consumer
            .consume(&mut rx)
            .await
            .expect("no leaf failed: the rows skip pre-sim");
        assert_eq!(report.batches, 2);
        assert_eq!(report.blocks, 2);
        assert_eq!(
            report.end,
            Some(degenbot::batch_executor::ConsumerEnd::QuietEnd)
        );
        assert_eq!(progress.batches(), 2);

        // The drain: one record set per batch, in arrival order, each row the
        // typed empty-hop skip (the fixture row carries no hops).
        let mut drained: Vec<Vec<u64>> = Vec::new();
        let mut assembly = Vec::new();
        while let Some(set) = executor.next_outcome().await {
            drained.push(set.records.iter().map(|r| r.path_id).collect());
            assembly.extend(set.records.iter().map(|r| r.assembly.label()));
        }
        assert_eq!(drained, vec![vec![1], vec![2]]);
        assert_eq!(
            assembly,
            vec!["skip-empty-hops", "skip-empty-hops"],
            "a hopless raw row is the typed skip, never an abort"
        );
    }

    /// The loud arm: a payload row is engine-born, so its resolve miss aborts the
    /// batch in the consume loop's frame (the assembly stage's corruption arm).
    #[tokio::test]
    async fn payload_resolve_miss_is_the_loud_abort_arm() {
        let executor = offline_executor(HashMap::new()).await;
        let (mut consumer, _progress) = consumer(Arc::clone(&executor));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();

        let mut batch = empty_batch(1);
        batch.payloads.insert(
            99,
            SimulatedPathResult {
                path_id: 99,
                gross_profit: U256::from(1_000_u64),
                net_profit: U256::from(900_u64),
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 7,
                execute_calldata: Vec::new(),
                access_list: None,
                captured_swaps: Vec::new(),
                hop_count: 1,
                failure: None,
            },
        );
        tx.send(SessionEvent::Block(block(1, 1))).unwrap();
        tx.send(SessionEvent::Batch(batch)).unwrap();
        drop(tx);

        let error = consumer
            .consume(&mut rx)
            .await
            .expect_err("an unresolvable payload path aborts loudly");
        assert!(
            error.detail.contains("99"),
            "the loud-abort detail names the unresolvable payload path: {}",
            error.detail
        );
    }
}
