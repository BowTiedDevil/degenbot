//! The batch executor's construction boundary for the startup BAL capability
//! probe (`simulation.delta_dispatch` ON).
//!
//! Three claims, one test each: an ON session whose endpoint cannot serve a
//! decodable block access list fails CONSTRUCTION and leaves its head pipeline
//! UNARMED; an ON session whose endpoint can serve one constructs and arms it;
//! and an OFF session constructs exactly as before — no probe, no head read, no
//! list request at all.
//!
//! The node is the shared simulation `chain_stub` (the scripted chain the
//! capability suites drive), so this boundary suite owns no second node stub and
//! no second worked-list fixture. Every test is a DEFAULT `#[tokio::test]`, i.e.
//! a CURRENT-THREAD runtime: the constructor awaits the probe on the caller's
//! own runtime, so a construction that bridged a blocking call into the runtime
//! would panic here rather than silently requiring a multi-threaded flavor.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "the fixture drives known-valid values and asserts on the typed product"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, B256};

use degenbot_arbitrage::{
    CmdExecutorBinding, FeeOnTransferRegistry, PoolDivergence, TargetSelection,
};
use degenbot_batch_executor::{
    BalCapabilityError, BatchExecutor, ExecutorConfig, ExecutorRuntime, PathResolver,
};
use degenbot_execution::PathInfo;
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::warm_code_cache::DeltaDispatchConfig;
use degenbot_simulation::WarmCodeCacheInner;
use degenbot_submission::{
    Dispatcher, PathSuppression, ReceiptProbe, SubmissionLedger, SubmissionResult, SubmissionTarget,
};
use degenbot_substrate::nonce::NonceAuthority;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

#[path = "../../degenbot-simulation/examples/shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;

use chain_stub::ChainStub;

/// The operator account both the signer and the deployment's owner name.
const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

const EXECUTOR: Address = address!("0x543c7ef4f2368a9411c94a055e7236e6dc6f99d5");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");

/// The head the scripted node reports.
const HEAD: u64 = 21_000_000;

/// The exact hash of [`HEAD`] — the hash the probe must ask the list at.
fn head_hash() -> B256 {
    B256::new([0x11; 32])
}

// ── The executor fixture ─────────────────────────────────────────────────

struct NoopResolver;

impl PathResolver for NoopResolver {
    fn resolve(&self, _path_id: u64) -> Option<PathInfo> {
        None
    }
}

struct NoopProbe;

impl ReceiptProbe for NoopProbe {
    fn receipt_found(
        &self,
        _tx_hash: B256,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }
}

fn signer() -> degenbot_submission::TxSigner {
    degenbot_submission::TxSigner::from_key_hex(ANVIL_KEY, 1).expect("the anvil key is valid")
}

/// An EXECUTOR that never submits: every batch is planned and dry-run only.
fn executor_config(
    provider: Arc<AlloyProvider>,
    warm_cache: Option<Arc<parking_lot::RwLock<WarmCodeCacheInner>>>,
) -> ExecutorConfig {
    let verdict = degenbot_config::BotConfig::default();
    ExecutorConfig::from_verdict(
        &verdict,
        ExecutorRuntime {
            max_candidates: 0,
            use_v4_batch: false,
            dry_run: true,
            resolver: Arc::new(NoopResolver),
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            provider,
            binding: CmdExecutorBinding::new(
                signer().address(),
                EXECUTOR,
                WETH,
                POOL_MANAGER,
                EncodeOptions::default(),
            ),
            additional_deployments: Vec::new(),
            target_selection: TargetSelection::FirstCompatible,
            injected_address: None,
            runtime_bytecode: Bytes::new(),
            bot_state: Some(Arc::new(StateLock::new(BotState::new()))),
            warm_cache,
            dispatcher: Arc::new(Mutex::new(Dispatcher::for_block(0))),
            signer: Arc::new(signer()),
            probe: Arc::new(NoopProbe),
            nonce_lane: Arc::new(Arc::new(NonceAuthority::new(0)).mint_lane(
                "settlement",
                Arc::new(SubmissionLedger::new()),
                degenbot_substrate::nonce::LanePolicy::default(),
            )),
            extra_broadcast: Vec::new(),
            target: SubmissionTarget::Public,
        },
    )
    .expect("the fixture's one-entry deployment registry is valid")
}

/// The session cache carrying the `simulation.delta_dispatch` opt-in.
fn on_cache() -> Arc<parking_lot::RwLock<WarmCodeCacheInner>> {
    WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development())
}

/// The shared scripted chain serving [`HEAD`] and one complete decodable list at
/// its exact hash.
fn serving_node() -> ChainStub {
    let stub = ChainStub::serving();
    stub.serve_head(HEAD, head_hash());
    stub.serve_bal_at(head_hash(), chain_stub::worked_list());
    stub
}

// ── The boundary ─────────────────────────────────────────────────────────

/// An endpoint with no EIP-7928 support cannot start an ON executor: construction
/// returns the probe's error, and the session's pipeline stays UNARMED, so no head
/// is ever delivered and no candidate is ever processed.
#[tokio::test]
async fn an_on_executor_fails_construction_and_arms_nothing_when_the_node_serves_no_list() {
    let stub = serving_node();
    stub.set_bal_unsupported(true);
    let cache = on_cache();

    let error = BatchExecutor::new(executor_config(
        Arc::new(stub.provider()),
        Some(Arc::clone(&cache)),
    ))
    .await
    .err()
    .expect("an unsupported endpoint must not start an ON executor");

    assert!(
        matches!(error, BalCapabilityError::Unusable { number, hash, .. } if number == HEAD && hash == head_hash()),
        "{error:?}"
    );
    let message = error.to_string();
    for needle in [
        "debug_getRawBlockAccessList",
        "eth_getBlockAccessList",
        "21000000",
        "simulation.delta_dispatch",
    ] {
        assert!(message.contains(needle), "{needle} missing from: {message}");
    }

    let pipeline = cache
        .read()
        .bal_head_pipeline()
        .expect("the opt-in mints the pipeline at boot");
    assert!(
        !pipeline.is_armed(),
        "a failed capability probe must leave the pipeline unarmed"
    );

    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList",
            "eth_getBlockAccessList"
        ],
        "the probe is the only work the failed construction did"
    );
}

/// A capable endpoint starts the ON executor and arms the session's one head
/// pipeline — armed only after the probe proved a decodable list.
#[tokio::test]
async fn an_on_executor_proves_capability_and_then_arms_the_pipeline() {
    let stub = serving_node();
    let cache = on_cache();

    let executor = BatchExecutor::new(executor_config(
        Arc::new(stub.provider()),
        Some(Arc::clone(&cache)),
    ))
    .await
    .expect("the node serves a decodable list");

    let pipeline = cache
        .read()
        .bal_head_pipeline()
        .expect("the opt-in mints the pipeline at boot");
    assert!(
        pipeline.is_armed(),
        "the probe succeeded, so the pipeline is armed"
    );
    assert_eq!(
        stub.methods(),
        vec![
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "debug_getRawBlockAccessList"
        ],
        "one head read, one lineage read, one raw list read"
    );
    assert_eq!(
        stub.dispatched()[2],
        format!("debug_getRawBlockAccessList [\"{}\"]", head_hash()),
        "the list is asked at the exact hash the probe resolved"
    );
    drop(executor);
}

/// OFF is unchanged: construction asks the node NOTHING — no probe, no header
/// read, no list request — and mints no pipeline.
#[tokio::test]
async fn an_off_executor_constructs_without_any_probe_header_or_list_request() {
    let stub = serving_node();
    let cache = WarmCodeCacheInner::shared_default();

    let executor = BatchExecutor::new(executor_config(
        Arc::new(stub.provider()),
        Some(Arc::clone(&cache)),
    ))
    .await
    .expect("an OFF executor cannot fail on a capability probe");

    assert!(
        stub.dispatched().is_empty(),
        "OFF is request-free: {:?}",
        stub.dispatched()
    );
    assert!(
        cache.read().bal_head_pipeline().is_none(),
        "OFF mints no pipeline, so no sink is installed either"
    );
    drop(executor);
}
