//! Umbrella reach proof for the core bounded-concurrency + ordered-submit
//! pipeline (`degenbot::submission::SimSubmitPipeline`) and the core batch
//! executor (`degenbot::batch_executor::BatchExecutor`) that wraps it.
//!
//! The example is the `cargo add degenbot` consumer: these tests construct
//! the core pipeline and the executor through the umbrella re-exports with
//! offline-safe fixtures and assert the ordered-lane behavior, so ledger
//! rows 16 (+15) are compile-and-run reach claims rather than prose
//! assertions. The mechanisms themselves are unit-tested in
//! `degenbot-submission` / `degenbot-batch-executor`.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "reach tests assert on known-valid fixtures (parse_address, the offline provider)"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use degenbot::batch_executor::{BatchExecutor, BatchWork, ExecutorConfig, ExecutorRuntime};
use degenbot::submission::{
    Dispatcher, NonceLane, PathSuppression, ReceiptProbe, SimFuture, SimLeaf, SimSubmitPipeline,
    SubmissionLedger, SubmissionTarget, SubmitFuture, SubmitLeaf, TxSigner,
};
struct EchoSim;

impl SimLeaf<u64, u64> for EchoSim {
    fn simulate<'a>(&'a self, work: &'a u64) -> SimFuture<'a, u64> {
        Box::pin(async move { Ok(Some(*work)) })
    }
}

struct RecordingSubmit {
    order: Arc<Mutex<Vec<u64>>>,
}

impl SubmitLeaf<u64, u64> for RecordingSubmit {
    fn submit<'a>(&'a self, _work: &'a u64, outcome: &'a u64) -> SubmitFuture<'a> {
        let order = Arc::clone(&self.order);
        let id = *outcome;
        Box::pin(async move {
            order.lock().unwrap().push(id);
            Ok(())
        })
    }
}

#[tokio::test]
async fn core_pipeline_is_reachable_through_the_umbrella() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let pipeline = SimSubmitPipeline::new(
        2,
        Arc::new(EchoSim),
        Arc::new(RecordingSubmit {
            order: Arc::clone(&order),
        }),
    );
    assert_eq!(pipeline.concurrency(), 2);
    for id in [1_u64, 2, 3] {
        pipeline.enqueue(id);
    }
    assert!(pipeline.shutdown().await.is_ok(), "no leaf failure");
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(pipeline.enqueued(), 3);
    assert_eq!(pipeline.submitted(), 3);
}

// ── Row 16: the core batch executor wraps the ordered lane ──
// The executor's value config is fully offline-safe here: dry_run, an empty
// suppression registry, a map resolver, and a dummy provider. The record
// shapes driven below all skip pre-sim, so no RPC fires.

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

/// A map-backed resolver (the offline driver shape; the live driver resolves
/// through `EngineDriver::path_info_for`).
struct MapResolver(HashMap<u64, degenbot::cmd_executor::composers::PathInfo>);

impl degenbot::batch_executor::PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<degenbot::cmd_executor::composers::PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

async fn offline_executor(
    resolver: HashMap<u64, degenbot::cmd_executor::composers::PathInfo>,
) -> BatchExecutor {
    let executor_address =
        degenbot::core::address_utils::parse_address("0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5")
            .unwrap();
    let weth =
        degenbot::core::address_utils::parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
            .unwrap();
    let provider = degenbot::rpc::provider::AlloyProvider::new("http://127.0.0.1:1", 0)
        .await
        .unwrap();
    // The verdict's declared defaults carry the policy floor (0) and the
    // inject stance (off); the serial-reference cap is a fixture choice
    // pinned on the verdict itself.
    let mut verdict = degenbot::config::BotConfig::default();
    verdict.simulation.pipeline_concurrency = 1;
    let config = ExecutorConfig::from_verdict(
        &verdict,
        ExecutorRuntime {
            max_candidates: 50,
            use_v4_batch: false,
            dry_run: true,
            resolver: Arc::new(MapResolver(resolver)),
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(degenbot::arbitrage::PoolDivergence::new())),
            fot: Arc::new(Mutex::new(degenbot::arbitrage::FeeOnTransferRegistry::new())),
            provider: Arc::new(provider),
            executor_owner: degenbot::core::address_utils::parse_address(
                "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
            )
            .unwrap(),
            executor_address,
            weth_address: weth,
            pool_manager_address: alloy::primitives::Address::ZERO,
            multicall3_address: alloy::primitives::Address::ZERO,
            injected_address: None,
            runtime_bytecode: alloy::primitives::Bytes::new(),
            warmup: degenbot::cmd_executor::compute_simulation_warmup_slots(executor_address, weth),
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
            nonce_lane: Arc::new(NonceLane::new(
                Arc::new(degenbot::substrate::nonce::NonceAuthority::new(0)),
                Arc::new(SubmissionLedger::new()),
                "settlement",
            )),
            extra_broadcast: Vec::new(),
            target: SubmissionTarget::Public,
        },
    );
    BatchExecutor::new(config)
}

/// The executor reach claim: construct through the umbrella, enqueue one
/// batch of raw rows (empty hops + an unresolvable path id), and drain the
/// typed Batch outcome records off the stream.
#[tokio::test]
async fn batch_executor_is_reachable_through_the_umbrella() {
    let executor = offline_executor(HashMap::new()).await;
    let work = BatchWork {
        rows: vec![
            degenbot::batch_executor::RawResult {
                path_id: 1,
                optimal_input: 1_000,
                profit: 100,
                hop_outputs: Vec::new(),
                consumed_inputs: Vec::new(),
                solve_block: 100,
                state_nonces: vec![1],
            },
            degenbot::batch_executor::RawResult {
                path_id: 2,
                optimal_input: 1_000,
                profit: 100,
                hop_outputs: vec![100],
                consumed_inputs: vec![1_000],
                solve_block: 100,
                state_nonces: vec![1],
            },
        ],
        payloads: Vec::new(),
        current_block: 100,
        base_fee_next: 1_000_000_000,
        block_timestamp: 1_700_000_000,
        block_priority_fees: None,
    };
    executor.enqueue(work);
    executor.shutdown().await.expect("no leaf failure");
    let set = executor
        .try_next_outcome()
        .await
        .expect("one drained record set");
    let labels: Vec<&str> = set.records.iter().map(|r| r.assembly.label()).collect();
    assert_eq!(labels, vec!["skip-empty-hops", "skip-resolve-miss"]);
    assert_eq!(executor.submitted(), 1);
    assert!(
        executor.try_next_outcome().await.is_none(),
        "stream drained"
    );
}
