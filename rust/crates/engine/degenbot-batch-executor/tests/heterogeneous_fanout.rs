#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the fixture drives known-valid values and asserts on the typed product"
)]

//! Heterogeneous fan-out through the ONE batch executor (ADR-098 D4).
//!
//! The session registers the built-in settlement deployment plus two foreign
//! deployments, and ONE solved path goes to all three:
//!
//! - the assembly binds the path to every registered deployment, so the batch
//!   carries one candidate per target (distinct `CandidateId`s);
//! - each lane simulates its own candidate against its own adapter, so the
//!   outcomes are the targets' own;
//! - the built-in target's refusal (its contract is code-less in this fixture,
//!   so its receipt is absent evidence) is confined to that candidate while the
//!   two foreign targets still reach the submit lane;
//! - the submit planning stamps each target's own `to`, calldata, and native
//!   value.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    project_early_admission, AdmittedCandidate, CmdExecutorBinding, DeploymentRegistry,
    DispatchCandidate, EarlyAdmissionPolicy, FeeOnTransferRegistry, PoolDivergence,
    PoolDivergenceKey, SimulationBlock, SimulationDeployment, TargetSelection,
    MAX_SIMULATE_CONCURRENT,
};
use degenbot_batch_executor::assembly::{assemble_batch, AssemblyInputs};
use degenbot_batch_executor::{
    join_sim_result, BatchExecutor, BatchOutcome, BatchWork, ExecutorConfig, ExecutorRuntime,
    PathResolver, RawResult, SimulateVerdict, SubmitVerdict,
};
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, HopInfo, PathInfo, ProbeSpecs,
    TransactionRequest, V2HopInfo,
};
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{
    Dispatcher, PathSuppression, ReceiptProbe, SkipReason, SubmissionLedger, SubmissionResult,
    SubmissionTarget, SubmitRecord,
};
use degenbot_substrate::nonce::NonceAuthority;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

/// The operator account both the signer and every deployment's owner name.
const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// The built-in settlement executor's address (code-less in this fixture).
const EXECUTOR: Address = address!("0x543c7ef4f2368a9411c94a055e7236e6dc6f99d5");

const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");

/// The two foreign targets — different contracts, different ABIs, different
/// funding models.
const TARGET_A: Address = address!("0x00000000000000000000000000000000000000a1");
const TARGET_B: Address = address!("0x00000000000000000000000000000000000000b2");

const PATH_ID: u64 = 7;
const BLOCK: u64 = 100;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;

const PREPAY_A: U256 = U256::from_limbs([5_000_000_000_000_000, 0, 0, 0]);
const GROSS_A: U256 = U256::from_limbs([9_000_000_000_000_000, 0, 0, 0]);
const GROSS_B: U256 = U256::from_limbs([7_000_000_000_000_000, 0, 0, 0]);

/// The built-in executor's declared build reports this receipt gross (the
/// fixture's stub returns the two receipt words — ADR-098 D7).
const BUILTIN_GROSS: U256 = U256::from_limbs([3_000_000_000_000_000, 0, 0, 0]);

/// The declared `execute` stub: return `(gross_profit, 0)` as two ABI words.
fn receipt_stub(gross: U256) -> Bytes {
    let mut code = Vec::with_capacity(76);
    code.push(0x7f); // PUSH32 gross_profit
    code.extend_from_slice(&gross.to_be_bytes::<32>());
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0x00 MSTORE
    code.push(0x7f); // PUSH32 bribe_paid
    code.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
    code.extend_from_slice(&[0x60, 0x20, 0x52]); // PUSH1 0x20 MSTORE
    code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xf3]); // PUSH1 0x40 PUSH1 0x00 RETURN
    Bytes::from(code)
}

/// The foreign deployments' operator-assigned identities (the built-in one is
/// `ExecutorConfig::BUILTIN_DEPLOYMENT_ID`).
fn deployment_a() -> DeploymentId {
    DeploymentId::new(2)
}

fn deployment_b() -> DeploymentId {
    DeploymentId::new(3)
}

fn signer() -> degenbot_submission::TxSigner {
    degenbot_submission::TxSigner::from_key_hex(ANVIL_KEY, 1).expect("the anvil key is valid")
}

fn owner() -> Address {
    signer().address()
}

fn prepay_selector() -> [u8; 4] {
    [0xa1, 0xa2, 0xa3, 0xa4]
}

fn settle_selector() -> [u8; 4] {
    [0xb1, 0xb2, 0xb3, 0xb4]
}

/// A WETH → token → WETH round trip: the built-in command grammar encodes this
/// shape, and the foreign adapters ignore the hops entirely.
fn v2_path() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token0_address: WETH,
            token1_address: address!("0x3333333333333333333333333333333333333333"),
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: address!("0xcccccccccccccccccccccccccccccccccccccccc"),
            token0_address: address!("0x3333333333333333333333333333333333333333"),
            token1_address: WETH,
            fee: 30,
            zfo: true,
        }),
    ])
}

/// The one solved row's per-hop figures (two hops, matching the path).
const HOP: u128 = OPTIMAL_INPUT;

/// One raw solver row for the fixture's path, bound to `deployment_id` by the
/// crate's own candidate mint.
fn candidate(deployment_id: DeploymentId) -> degenbot_arbitrage::DispatchCandidate {
    RawResult {
        path_id: PATH_ID,
        optimal_input: OPTIMAL_INPUT,
        profit: 1,
        hop_outputs: vec![HOP, HOP],
        consumed_inputs: vec![HOP, HOP],
        solve_block: BLOCK,
        state_nonces: vec![0, 0],
    }
    .to_candidate(v2_path(), deployment_id)
}

/// The V2 pool the fixture path's FIRST hop routes through — the pool key a
/// lane-1 `SolverCalc` divergence flags, and the one every lane's candidates
/// route through.
const FLAGGED_POOL: Address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

/// A second distinct pool for the fee-on-transfer threshold's second suspicion.
const SECOND_POOL: Address = address!("0xdddddddddddddddddddddddddddddddddddddddd");

/// Lane 1's OWN path in the fee-on-transfer fixture: a token the suspicion leg
/// never touches, so lane 1's same-batch success cannot clear lane 2's record
/// through the registry's 0-success disambiguator.
const QUIET_IN: Address = address!("0x8888888888888888888888888888888888888888");
const QUIET_OUT: Address = address!("0x9999999999999999999999999999999999999999");
const QUIET_POOL: Address = address!("0x7777777777777777777777777777777777777777");

/// One V2 hop through the quiet pool, from `QUIET_IN` to `QUIET_OUT`.
fn quiet_path() -> PathInfo {
    PathInfo::new(vec![HopInfo::V2(V2HopInfo {
        pool_address: QUIET_POOL,
        token0_address: QUIET_IN,
        token1_address: QUIET_OUT,
        fee: 30,
        zfo: true,
    })])
}

/// The fixture's solved row expressed over the quiet path.
fn quiet_candidate(deployment_id: DeploymentId) -> degenbot_arbitrage::DispatchCandidate {
    RawResult {
        path_id: PATH_ID,
        optimal_input: OPTIMAL_INPUT,
        profit: 1,
        hop_outputs: vec![HOP],
        consumed_inputs: vec![HOP],
        solve_block: BLOCK,
        state_nonces: vec![0],
    }
    .to_candidate(quiet_path(), deployment_id)
}

/// The fixture path's first-hop input token (`zfo`, so `token0`) — the token the
/// fee-on-transfer registry keys its suspicions on.
const FOT_TOKEN: Address = WETH;

fn raw_row() -> RawResult {
    RawResult {
        path_id: PATH_ID,
        optimal_input: OPTIMAL_INPUT,
        profit: 1,
        hop_outputs: vec![HOP, HOP],
        consumed_inputs: vec![HOP, HOP],
        solve_block: BLOCK,
        state_nonces: vec![0, 0],
    }
}

/// One foreign target's adapter: its own ABI (selector + optimal input word),
/// its own native value, and its own gross figure.
#[derive(Clone, Copy)]
struct TargetAdapter {
    target: Address,
    selector: [u8; 4],
    value: U256,
    gross: U256,
}

impl ExecutionAdapter for TargetAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(&self.selector);
        data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
        Ok(TransactionRequest::new(
            self.target,
            Bytes::from(data),
            self.value,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        Ok(ExecutionResult::price(
            self.gross,
            Vec::new(),
            observation,
            U256::ZERO,
            &FeePolicy {
                market_percentile: false,
                override_priority_fee: 0,
            },
        ))
    }
}

fn deployment(id: DeploymentId, adapter: TargetAdapter) -> Arc<SimulationDeployment> {
    Arc::new(SimulationDeployment::new(
        id,
        Arc::new(adapter),
        owner(),
        adapter.target,
        1_000_000,
        None,
        None,
    ))
}

fn registry() -> DeploymentRegistry {
    DeploymentRegistry::new(vec![
        deployment(
            deployment_a(),
            TargetAdapter {
                target: TARGET_A,
                selector: prepay_selector(),
                value: PREPAY_A,
                gross: GROSS_A,
            },
        ),
        deployment(
            deployment_b(),
            TargetAdapter {
                target: TARGET_B,
                selector: settle_selector(),
                value: U256::ZERO,
                gross: GROSS_B,
            },
        ),
    ])
    .expect("two distinct identities compose a valid registry")
}

// ── The offline RPC fixture ──────────────────────────────────────────────

#[derive(Clone, Copy)]
struct EmptyTransport;

fn success(value: &str) -> ResponsePayload {
    let raw: Box<RawValue> =
        to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
    ResponsePayload::Success(raw)
}

impl tower::Service<RequestPacket> for EmptyTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        Box::pin(async move {
            let respond = |req: &SerializedRequest| -> Response {
                let payload = match req.method() {
                    "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                    "eth_getCode" => success("0x"),
                    "eth_getStorageAt" => success(&format!("0x{:064x}", 0)),
                    "eth_getBlockByNumber" => ResponsePayload::Success(
                        to_raw_value(
                            &serde_json::json!({ "hash": format!("0x{}", "11".repeat(32)) }),
                        )
                        .expect("valid JSON value"),
                    ),
                    other => {
                        return Response {
                            id: req.id().clone(),
                            payload: ResponsePayload::Failure(ErrorPayload {
                                code: -32601,
                                message: format!("method not found: {other}").into(),
                                data: None,
                            }),
                        }
                    }
                };
                Response {
                    id: req.id().clone(),
                    payload,
                }
            };
            Ok(match req {
                RequestPacket::Single(r) => ResponsePacket::Single(respond(&r)),
                RequestPacket::Batch(rs) => ResponsePacket::Batch(rs.iter().map(respond).collect()),
            })
        })
    }
}

fn mock_provider() -> AlloyProvider {
    let client = RpcClient::new(EmptyTransport, true);
    let root: RootProvider = RootProvider::new(client);
    AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    )
}

fn block(provider: &AlloyProvider) -> SimulationBlock<'_> {
    SimulationBlock {
        provider,
        base_fee_next: 1_000_000_000,
        current_block: BLOCK,
        block_timestamp: 0,
        block_priority_fees: None,
    }
}

struct MapResolver(HashMap<u64, PathInfo>);

impl PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

struct NoopProbe;

impl ReceiptProbe for NoopProbe {
    fn receipt_found(
        &self,
        _tx_hash: alloy::primitives::B256,
    ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }
}

// ── The executor fixture ─────────────────────────────────────────────────

/// The production executor over the offline transport, with `additional`
/// registered after the built-in deployment. `dry_run` is on: the submit
/// orchestration plans, orders, and arbitrates every candidate, then skips the
/// broadcast.
async fn executor(additional: Vec<Arc<SimulationDeployment>>) -> BatchExecutor {
    executor_with(additional, TargetSelection::AllTargets).await
}

/// The same executor under a named target-selection policy.
async fn executor_with(
    additional: Vec<Arc<SimulationDeployment>>,
    selection: TargetSelection,
) -> BatchExecutor {
    let provider = Arc::new(mock_provider());
    let mut verdict = degenbot_config::BotConfig::default();
    // The built-in target runs a DECLARED build in this fixture: its `execute`
    // stub returns a receipt, so the built-in lane's outcome is its own
    // assessment rather than an absent-receipt refusal.
    verdict.simulation.inject_executor_code = true;
    let runtime = ExecutorRuntime {
        max_candidates: 50,
        use_v4_batch: false,
        dry_run: true,
        resolver: Arc::new(MapResolver(HashMap::from([(PATH_ID, v2_path())]))),
        suppression: Arc::new(Mutex::new(PathSuppression::new())),
        divergence: Arc::new(Mutex::new(PoolDivergence::new())),
        fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
        provider: Arc::clone(&provider),
        binding: CmdExecutorBinding::new(
            owner(),
            EXECUTOR,
            WETH,
            POOL_MANAGER,
            EncodeOptions::default(),
        ),
        additional_deployments: additional,
        target_selection: selection,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: receipt_stub(BUILTIN_GROSS),
        bot_state: Some(Arc::new(StateLock::new(BotState::new()))),
        warm_cache: Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
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
    };
    BatchExecutor::new(
        ExecutorConfig::from_verdict(&verdict, runtime)
            .expect("the fixture's three distinct deployments compose a valid registry"),
    )
    .await
    .expect("this fixture's cache carries no BAL opt-in, so no capability probe can refuse")
}

fn work() -> BatchWork {
    BatchWork {
        rows: vec![raw_row()],
        payloads: Vec::new(),
        current_block: BLOCK,
        base_fee_next: 1_000_000_000,
        block_timestamp: 0,
        block_priority_fees: None,
    }
}

/// Drain one batch through the real executor.
async fn run_batch(executor: &BatchExecutor) -> (Vec<BatchOutcome>, Vec<SubmitRecord>) {
    assert_eq!(
        executor.enqueue(work()),
        degenbot_submission::EnqueueOutcome::Accepted
    );
    executor.shutdown().await.expect("no leaf failed");
    let set = executor.next_outcome().await.expect("one batch drained");
    (set.records, set.submit_records)
}

/// The two foreign targets' per-target submit rows: each carries its OWN `to`,
/// calldata, and native value, and each keeps its own candidate identity.
#[test]
fn the_submit_plan_carries_each_targets_own_transaction() {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let registry = registry();
    let block = block(&provider);
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                let state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                let inputs = degenbot_arbitrage::FanOutInputs {
                    deployments: &registry,
                    block: &block,
                    current_block: BLOCK,
                    target_selection: degenbot_arbitrage::TargetSelection::AllTargets,
                    path_suppression: &suppression,
                    pool_divergence: &divergence,
                    fot_registry: &fot,
                    bot_state: Some(&state),
                    warm_cache: Some(&warm_cache),
                };
                runtime.block_on(async {
                    degenbot_arbitrage::dispatch_across_deployments(
                        vec![candidate(deployment_a()), candidate(deployment_b())],
                        inputs.with_thin_margin(0),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
    .expect("every candidate names a registered deployment");

    let mut planned = Vec::new();
    for lane in &outcome.lanes {
        let deployment = registry
            .get(lane.deployment_id)
            .expect("the lane's deployment is registered");
        for result in &lane.outcome.gas_profitable {
            planned.push((
                lane.deployment_id,
                join_sim_result(result, Some(&v2_path()), deployment.target()),
            ));
        }
    }
    assert_eq!(planned.len(), 2, "both targets produced a submit row");

    let (id_a, row_a) = &planned[0];
    assert_eq!(*id_a, deployment_a());
    assert_eq!(
        row_a.candidate_id,
        CandidateId::new(PATH_ID, deployment_a())
    );
    assert_eq!(row_a.execution_target, TARGET_A, "target A's own `to`");
    assert_eq!(&row_a.execute_calldata[..4], &prepay_selector());
    assert_eq!(row_a.value, PREPAY_A, "target A's own native value");

    let (id_b, row_b) = &planned[1];
    assert_eq!(*id_b, deployment_b());
    assert_eq!(
        row_b.candidate_id,
        CandidateId::new(PATH_ID, deployment_b())
    );
    assert_eq!(row_b.execution_target, TARGET_B, "target B's own `to`");
    assert_eq!(&row_b.execute_calldata[..4], &settle_selector());
    assert_eq!(row_b.value, U256::ZERO, "target B funds itself");
    assert_ne!(row_a.execution_target, row_b.execution_target);

    // One solved path, one pool set: the mutual-exclusion keys are the path's,
    // not the target's.
    assert_eq!(row_a.path_pools, row_b.path_pools);
}

/// The executor binds ONE solved path to every registered deployment: one
/// record per target, distinct identities, and independently correct outcomes.
/// Every target's own assessment lands on its own record, and every target's
/// candidate reaches the submit lane.
///
/// The multi-thread flavor is required: the engine's sync DB bridge uses
/// `block_in_place`, which a current-thread runtime refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_path_becomes_one_candidate_per_registered_deployment() {
    let executor = executor(vec![
        deployment(
            deployment_a(),
            TargetAdapter {
                target: TARGET_A,
                selector: prepay_selector(),
                value: PREPAY_A,
                gross: GROSS_A,
            },
        ),
        deployment(
            deployment_b(),
            TargetAdapter {
                target: TARGET_B,
                selector: settle_selector(),
                value: U256::ZERO,
                gross: GROSS_B,
            },
        ),
    ])
    .await;
    let (records, submit_records) = run_batch(&executor).await;

    // Deployment-major: the built-in lane first, then the two registered
    // deployments in registry order.
    let ids: Vec<CandidateId> = records.iter().map(|r| r.candidate_id).collect();
    assert_eq!(
        ids,
        vec![
            CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
            CandidateId::new(PATH_ID, deployment_a()),
            CandidateId::new(PATH_ID, deployment_b()),
        ],
        "one solved path, one candidate per target, merged in registry order"
    );
    assert_eq!(records.len(), 3);

    let builtin = &records[0];
    let Some(SimulateVerdict::Profitable(builtin)) = &builtin.simulate else {
        panic!("the built-in target must report its own profitable receipt")
    };
    assert_eq!(
        builtin.gross_profit, BUILTIN_GROSS,
        "the built-in reports its contract's retained receipt figure"
    );

    // The foreign targets' own assessments: each at its own gross.
    let Some(SimulateVerdict::Profitable(a)) = &records[1].simulate else {
        panic!("target A must be profitable")
    };
    let Some(SimulateVerdict::Profitable(b)) = &records[2].simulate else {
        panic!("target B must be profitable")
    };
    assert_eq!(a.gross_profit, GROSS_A);
    assert_eq!(b.gross_profit, GROSS_B);
    assert_ne!(
        a.net_profit, b.net_profit,
        "each target's net is its own assessment"
    );

    // Submission stays ONE ordered lane over the session signer: every target's
    // candidate is planned and joined for its own target, the lane's single
    // dry run selects the highest net, and the path's pools are committed once
    // for the rest.
    assert_eq!(
        submit_records.len(),
        3,
        "one submit row per profitable target"
    );
    let submitted: Vec<(CandidateId, SkipReason)> = submit_records
        .iter()
        .map(|record| match record {
            SubmitRecord::Skipped {
                candidate_id,
                reason,
            } => (*candidate_id, reason.clone()),
            SubmitRecord::Submitted { candidate_id, .. } => {
                panic!("a dry run never broadcasts ({candidate_id})")
            }
        })
        .collect();
    assert_eq!(
        submitted,
        vec![
            (
                CandidateId::new(PATH_ID, deployment_a()),
                SkipReason::DryRun
            ),
            (
                CandidateId::new(PATH_ID, deployment_b()),
                SkipReason::PoolsClaimed
            ),
            (
                CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
                SkipReason::PoolsClaimed
            ),
        ],
        "net-descending order, one nonce lane, the path's pools claimed once"
    );
    assert_eq!(
        records[1].submit,
        Some(SubmitVerdict::Failed(
            degenbot_batch_executor::FailureKind::from_skip_reason(&SkipReason::DryRun)
        ))
    );
}

/// A foreign adapter whose compose callback is COUNTED: the proof that a
/// default-policy session never reaches a later target is that this counter
/// stays zero.
struct UncomposedAdapter(Arc<std::sync::atomic::AtomicUsize>);

impl ExecutionAdapter for UncomposedAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(TransactionRequest::new(
            TARGET_A,
            Bytes::from(
                U256::from(inputs.optimal_input)
                    .to_be_bytes::<32>()
                    .to_vec(),
            ),
            U256::ZERO,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        Ok(ExecutionResult::price(
            GROSS_A,
            Vec::new(),
            observation,
            U256::ZERO,
            &FeePolicy {
                market_percentile: false,
                override_priority_fee: 0,
            },
        ))
    }
}

/// The DEFAULT policy executes at the first compatible deployment: the built-in
/// one leads the registry, so an additionally registered foreign target is never
/// composed, never simulated, and earns neither a record nor a submit row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_policy_executes_only_at_the_first_compatible_deployment() {
    let compositions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let foreign = Arc::new(SimulationDeployment::new(
        deployment_a(),
        Arc::new(UncomposedAdapter(Arc::clone(&compositions))),
        owner(),
        TARGET_A,
        1_000_000,
        None,
        None,
    ));
    let executor = executor_with(vec![foreign], TargetSelection::FirstCompatible).await;
    let (records, submit_records) = run_batch(&executor).await;

    assert_eq!(
        records
            .iter()
            .map(|record| record.candidate_id)
            .collect::<Vec<_>>(),
        vec![CandidateId::new(
            PATH_ID,
            ExecutorConfig::BUILTIN_DEPLOYMENT_ID
        )],
        "only the first-compatible deployment produced a record"
    );
    let Some(SimulateVerdict::Profitable(builtin)) = &records[0].simulate else {
        panic!("the built-in target must report its own profitable receipt")
    };
    assert_eq!(builtin.gross_profit, BUILTIN_GROSS);
    assert_eq!(
        submit_records.len(),
        1,
        "the retired target contributes no submit row"
    );
    assert_eq!(
        compositions.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the default policy never composes a target after the first acceptance"
    );
}

/// A one-entry registry is the built-in session: one candidate per solved path,
/// exactly as before the registry existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_entry_registry_keeps_the_single_target_behavior() {
    let executor = executor(Vec::new()).await;
    let (records, _submit_records) = run_batch(&executor).await;
    assert_eq!(
        records.len(),
        1,
        "the built-in session simulates one candidate per path"
    );
    assert_eq!(
        records[0].candidate_id,
        CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID)
    );
    assert_eq!(
        executor.inline_deployment_id(),
        ExecutorConfig::BUILTIN_DEPLOYMENT_ID
    );
}

/// A payload row minted for the built-in deployment joins the built-in lane
/// only: a heterogeneous session must not reject it merely because another
/// deployment is registered, and it must not leak into another target's lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_builtin_payload_row_rides_only_its_own_lane() {
    let executor = executor(vec![deployment(
        deployment_a(),
        TargetAdapter {
            target: TARGET_A,
            selector: prepay_selector(),
            value: PREPAY_A,
            gross: GROSS_A,
        },
    )])
    .await;
    let mut work = work();
    work.payloads = vec![degenbot_batch_executor::PayloadRow {
        candidate_id: CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
        gross_profit: GROSS_A,
        net_profit: GROSS_A,
        gas_used: 21_000,
        priority_fee: 0,
        base_fee_next: 1_000_000_000,
        execute_calldata: Bytes::from_static(&[0xde, 0xad]),
        access_list: None,
        failure: None,
        captured_swaps: Vec::new(),
    }];
    assert_eq!(
        executor.enqueue(work),
        degenbot_submission::EnqueueOutcome::Accepted
    );
    executor.shutdown().await.expect("no leaf failed");
    let set = executor.next_outcome().await.expect("one batch drained");

    // The built-in lane: the payload's own record (it was simulated inline, so
    // its raw row never reaches that lane's sim) plus its joined submit row.
    // The foreign lane: the raw row, simulated by its own adapter.
    let ids: Vec<CandidateId> = set.records.iter().map(|r| r.candidate_id).collect();
    assert_eq!(
        ids,
        vec![
            CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
            CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
            CandidateId::new(PATH_ID, deployment_a()),
        ],
        "the payload row is its own record; the raw row is skipped only in the built-in lane"
    );
    assert!(set
        .records
        .iter()
        .any(|record| record.assembly.label() == "skip-payload-served"
            && record.candidate_id.deployment_id() == ExecutorConfig::BUILTIN_DEPLOYMENT_ID));
    assert!(
        !set.records
            .iter()
            .any(|record| record.assembly.label() == "skip-payload-served"
                && record.candidate_id.deployment_id() == deployment_a()),
        "the foreign lane still simulates its own candidate"
    );
    assert_eq!(
        set.submit_records.len(),
        2,
        "the built-in payload's join and the foreign lane's survivor both reach the lane"
    );
}

// ── Same-batch cross-lane feedback + the leaf loud-failure contract ──────

fn adapter_a() -> TargetAdapter {
    TargetAdapter {
        target: TARGET_A,
        selector: prepay_selector(),
        value: PREPAY_A,
        gross: GROSS_A,
    }
}

fn adapter_b() -> TargetAdapter {
    TargetAdapter {
        target: TARGET_B,
        selector: settle_selector(),
        value: U256::ZERO,
        gross: GROSS_B,
    }
}

/// One registered deployment over an arbitrary adapter (the feedback fixtures
/// need a different adapter per lane).
fn deployment_over(
    id: DeploymentId,
    adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
    target: Address,
) -> Arc<SimulationDeployment> {
    Arc::new(SimulationDeployment::new(
        id,
        adapter,
        owner(),
        target,
        1_000_000,
        None,
        None,
    ))
}

/// What a lane-1 adapter writes when its simulation is assessed: the same-block
/// policy feedback a real `SolverCalc` / fee-on-transfer failure produces, which
/// every lane that finalizes AFTER it reads.
enum Feedback {
    /// One `SolverCalc` divergence at `FLAGGED_POOL`.
    Divergence(Arc<Mutex<PoolDivergence>>),
    /// One fee-on-transfer suspicion per listed pool for `FOT_TOKEN`.
    Suspicions {
        registry: Arc<Mutex<FeeOnTransferRegistry>>,
        pools: Vec<PoolDivergenceKey>,
    },
}

/// A target adapter that prices like [`TargetAdapter`] AND writes one policy
/// registry at assessment time.
struct FeedbackAdapter {
    inner: TargetAdapter,
    feedback: Feedback,
}

impl ExecutionAdapter for FeedbackAdapter {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.inner.encode(path, inputs)
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        self.inner.probe_spec()
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        match &self.feedback {
            Feedback::Divergence(memo) => {
                memo.lock()
                    .expect("divergence memo")
                    .record_divergence(PoolDivergenceKey::V2(FLAGGED_POOL), BLOCK);
            }
            Feedback::Suspicions { registry, pools } => {
                let mut registry = registry.lock().expect("fot registry");
                for pool in pools {
                    registry.record_suspicion(FOT_TOKEN, *pool, BLOCK);
                }
            }
        }
        self.inner.assess(observation)
    }
}

/// One session's shared policy arcs — the state the batch's assembly and the
/// fan-out's leaf both read.
struct PolicyArcs {
    suppression: Arc<Mutex<PathSuppression>>,
    divergence: Arc<Mutex<PoolDivergence>>,
    fot: Arc<Mutex<FeeOnTransferRegistry>>,
}

fn policy_arcs() -> PolicyArcs {
    PolicyArcs {
        suppression: Arc::new(Mutex::new(PathSuppression::new())),
        divergence: Arc::new(Mutex::new(PoolDivergence::new())),
        fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
    }
}

/// Run one PRE-ADMITTED registry fan-out — the transport the batch's assembly
/// produces — on the dedicated multi-thread runtime the engine's sync DB bridge
/// requires.
fn run_admitted_fanout(
    registry: &DeploymentRegistry,
    admitted: Vec<AdmittedCandidate>,
    arcs: &PolicyArcs,
) -> degenbot_arbitrage::DeploymentFanOutOutcome {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let state = Arc::new(StateLock::new(BotState::new()));
    let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
    let block = block(&provider);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                runtime.block_on(async {
                    degenbot_arbitrage::dispatch_admitted_across_deployments(
                        admitted,
                        degenbot_arbitrage::FanOutInputs {
                            deployments: registry,
                            block: &block,
                            current_block: BLOCK,
                            target_selection: TargetSelection::AllTargets,
                            path_suppression: &arcs.suppression,
                            pool_divergence: &arcs.divergence,
                            fot_registry: &arcs.fot,
                            bot_state: Some(&state),
                            warm_cache: Some(&warm_cache),
                        },
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
    .expect("every candidate names a registered deployment")
}

/// One lane's outcome, by deployment.
fn lane(
    outcome: &degenbot_arbitrage::DeploymentFanOutOutcome,
    deployment: DeploymentId,
) -> &degenbot_arbitrage::DispatchOutcome {
    outcome
        .lane(deployment)
        .expect("the registry holds the lane")
}

/// The two-target registry with a lane-1 feedback adapter and a plain lane 2, in
/// registry order.
fn feedback_registry(feedback: Feedback) -> DeploymentRegistry {
    DeploymentRegistry::new(vec![
        deployment_over(
            deployment_a(),
            Arc::new(FeedbackAdapter {
                inner: adapter_a(),
                feedback,
            }),
            TARGET_A,
        ),
        deployment_over(deployment_b(), Arc::new(adapter_b()), TARGET_B),
    ])
    .expect("two distinct identities compose a valid registry")
}

/// Project raw rows through the shared EARLY projection — the one public way a
/// caller obtains the pre-admitted transport the batch's assembly produces (the
/// unchecked constructor is crate-private). No state owner: these fixtures assert
/// on the leaf's finalization, not on the projection's staleness gate.
fn transport(arcs: &PolicyArcs, candidates: Vec<DispatchCandidate>) -> Vec<AdmittedCandidate> {
    let suppression = arcs.suppression.lock().expect("suppression ledger");
    project_early_admission(
        candidates,
        &EarlyAdmissionPolicy {
            suppression: &suppression,
            divergence: &arcs.divergence,
            fee_on_transfer: &arcs.fot,
            bot_state: None,
            current_block: BLOCK,
            min_profit_margin_bps: 0,
            max_candidates: MAX_SIMULATE_CONCURRENT,
        },
    )
    .into_admitted()
}

/// Two transported candidates — one per registered deployment, both through the
/// fixture path.
fn transported_pair(arcs: &PolicyArcs) -> Vec<AdmittedCandidate> {
    transport(
        arcs,
        vec![candidate(deployment_a()), candidate(deployment_b())],
    )
}

/// The fee-on-transfer pair: lane 1 rides the quiet path (its success touches no
/// token under suspicion), lane 2 rides the fixture path whose input token is
/// `FOT_TOKEN`.
fn transported_pair_with_quiet_first_lane(arcs: &PolicyArcs) -> Vec<AdmittedCandidate> {
    transport(
        arcs,
        vec![quiet_candidate(deployment_a()), candidate(deployment_b())],
    )
}

/// Lane 1's `SolverCalc` divergence still drops lane 2's affected candidates in
/// the SAME batch: the lanes share one memo, lane 1 finalizes and simulates
/// first, and lane 2's finalization re-reads that memo before it simulates.
#[test]
fn a_lane_one_divergence_drops_the_later_lanes_affected_candidates() {
    let arcs = policy_arcs();
    let registry = feedback_registry(Feedback::Divergence(Arc::clone(&arcs.divergence)));

    let outcome = run_admitted_fanout(&registry, transported_pair(&arcs), &arcs);

    let first = lane(&outcome, deployment_a());
    assert_eq!(
        first.candidate_count, 1,
        "lane 1 finalizes before its own feedback lands"
    );
    assert_eq!(first.sim_ok_count(), 1);
    assert_eq!(first.divergent_dropped, 0);

    let second = lane(&outcome, deployment_b());
    assert_eq!(
        second.divergent_dropped, 1,
        "the same batch's lane-1 divergence reached lane 2's final read"
    );
    assert_eq!(second.candidate_count, 0, "lane 2 simulated nothing");
    assert_eq!(second.sim_ok_count(), 0);
    assert!(outcome.retired.is_empty());
}

/// The fee-on-transfer feedback is threshold-confirmed per token: ONE failing
/// pool is not enough for a later lane's finalization to drop its candidate, and
/// the second DISTINCT pool is.
#[test]
fn a_later_lane_drops_only_once_the_feedback_confirms_the_token() {
    // One suspicion: not confirmation — the later lane still simulates.
    let arcs = policy_arcs();
    let registry = feedback_registry(Feedback::Suspicions {
        registry: Arc::clone(&arcs.fot),
        pools: vec![PoolDivergenceKey::V2(FLAGGED_POOL)],
    });
    let unconfirmed = run_admitted_fanout(
        &registry,
        transported_pair_with_quiet_first_lane(&arcs),
        &arcs,
    );
    assert_eq!(
        lane(&unconfirmed, deployment_b()).fot_dropped,
        0,
        "one failing pool is not confirmation"
    );
    assert_eq!(lane(&unconfirmed, deployment_b()).candidate_count, 1);

    // Two distinct failing pools: the token is confirmed, so the later lane's
    // candidate drops at its final read.
    let arcs = policy_arcs();
    let registry = feedback_registry(Feedback::Suspicions {
        registry: Arc::clone(&arcs.fot),
        pools: vec![
            PoolDivergenceKey::V2(FLAGGED_POOL),
            PoolDivergenceKey::V2(SECOND_POOL),
        ],
    });
    let confirmed = run_admitted_fanout(
        &registry,
        transported_pair_with_quiet_first_lane(&arcs),
        &arcs,
    );
    assert_eq!(lane(&confirmed, deployment_a()).candidate_count, 1);
    assert_eq!(
        lane(&confirmed, deployment_b()).fot_dropped,
        1,
        "the confirmed token reached lane 2's final read"
    );
    assert_eq!(lane(&confirmed, deployment_b()).candidate_count, 0);
}

/// Poison `ledger` the way a panicking peer task does (a guard held across an
/// unwind is the only way a std `Mutex` takes poison).
fn poisoned_ledger() -> Arc<Mutex<PathSuppression>> {
    let ledger = Arc::new(Mutex::new(PathSuppression::new()));
    let handle = Arc::clone(&ledger);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = handle.lock().expect("suppression ledger");
        // A genuinely fallible parse: the panic source a poisoned guard needs.
        let _: u8 = "not a number".parse().expect("a number");
    }));
    assert!(ledger.is_poisoned(), "fixture poisons the ledger");
    ledger
}

/// The batch's finalization leaf keeps the loud-failure contract: a poisoned
/// policy ledger panics rather than reading as "nothing suppressed".
#[test]
#[should_panic(expected = "suppression mutex poisoned")]
fn a_poisoned_ledger_panics_at_the_pre_admitted_leaf() {
    let suppression = poisoned_ledger();
    let registry = registry();
    let provider = mock_provider();
    let block = block(&provider);
    let arcs = policy_arcs();
    let state = Arc::new(StateLock::new(BotState::new()));
    let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
    // The finalization pass runs before any simulation, so the panic lands
    // inside this call — no runtime and no engine handle is needed for it.
    let _ = degenbot_arbitrage::dispatch_admitted_across_deployments(
        Vec::new(),
        degenbot_arbitrage::FanOutInputs {
            deployments: &registry,
            block: &block,
            current_block: BLOCK,
            target_selection: TargetSelection::AllTargets,
            path_suppression: &suppression,
            pool_divergence: &arcs.divergence,
            fot_registry: &arcs.fot,
            bot_state: Some(&state),
            warm_cache: Some(&warm_cache),
        },
    );
}

/// The batch's leaf counters name only what FINALIZATION dropped. The assembly's
/// own early drops never reach the fan-out (the transport carries survivors
/// alone), and the leaf re-runs no thin-margin floor, so a batch whose floor
/// dropped a row up front still reports `thin_dropped == 0` beside a clean
/// `candidate_count` — one policy implementation, not two.
#[test]
fn the_leaf_counts_only_what_finalization_dropped() {
    /// The second path id: same hops, fat economics.
    const FAT_PATH: u64 = 8;

    let arcs = policy_arcs();
    let resolver = MapResolver(HashMap::from([(PATH_ID, v2_path()), (FAT_PATH, v2_path())]));
    // The fixture row's profit is 1 wei on a 1e15 input: thin at a 50 bps floor.
    let thin = raw_row();
    let mut fat = raw_row();
    fat.path_id = FAT_PATH;
    fat.profit = HOP;
    fat.hop_outputs = vec![HOP, HOP];

    let assembled = {
        let mut ledger = arcs.suppression.lock().expect("suppression ledger");
        degenbot_batch_executor::assembly::assemble_batch(
            &degenbot_batch_executor::assembly::AssemblyInputs {
                rows: &[thin, fat],
                payloads: &[],
                resolver: &resolver,
                payload_served: &HashSet::new(),
                suppression: &mut ledger,
                divergence: &arcs.divergence,
                fot: &arcs.fot,
                bot_state: None,
                current_block: BLOCK,
                min_profit_margin_bps: 50,
                max_candidates: 50,
                target: TARGET_A,
                deployment_id: deployment_a(),
            },
        )
        .expect("the fixture rows are well formed")
    };

    assert_eq!(
        assembled
            .candidates
            .iter()
            .map(|admitted| admitted.candidate().candidate_id.path_id())
            .collect::<Vec<_>>(),
        vec![FAT_PATH],
        "the floor dropped the thin row before the fan-out"
    );
    assert_eq!(
        assembled.outcomes.len(),
        1,
        "its typed record is the batch's"
    );

    let registry = registry();
    let outcome = run_admitted_fanout(&registry, assembled.candidates, &arcs);

    let lane = lane(&outcome, deployment_a());
    assert_eq!(lane.candidate_count, 1, "one transported survivor");
    assert_eq!(lane.sim_ok_count(), 1);
    assert_eq!(
        lane.thin_dropped, 0,
        "the leaf has no thin stage: the floor is the assembly's verdict"
    );
    assert_eq!(lane.suppressed_count, 0);
    assert_eq!(lane.divergent_dropped, 0);
    assert_eq!(lane.fot_dropped, 0);
    assert_eq!(lane.stale_dropped, 0);
}

// ── The batch cap under the leaf's optional profit reorder ───────────────

/// The fixture's per-row input basis: `optimal_input` names the row, so an
/// adapter that logs the input it composed for tells the simulation order.
const ORDERED_INPUT: u128 = 1_000_000;

/// A target adapter that prices like [`TargetAdapter`] AND logs the
/// `optimal_input` of every candidate the leaf composed for — the composition
/// happens inside the simulation body, so the log IS the simulation order.
struct RecordingAdapter {
    inner: TargetAdapter,
    composed: Arc<Mutex<Vec<u128>>>,
}

impl ExecutionAdapter for RecordingAdapter {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.composed
            .lock()
            .expect("compose log")
            .push(inputs.optimal_input);
        self.inner.encode(path, inputs)
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        self.inner.probe_spec()
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        self.inner.assess(observation)
    }
}

/// One raw solver row whose profit RISES with its input position (so a
/// profit-ranked selection is the reverse of the caller's order) and whose
/// `optimal_input` identifies the row.
fn ordered_row(path_id: u64, solve_block: u64) -> RawResult {
    RawResult {
        path_id,
        optimal_input: ORDERED_INPUT + u128::from(path_id),
        profit: u128::from(path_id) * 1_000,
        hop_outputs: vec![ORDERED_INPUT, ORDERED_INPUT],
        consumed_inputs: vec![ORDERED_INPUT, ORDERED_INPUT],
        solve_block,
        state_nonces: vec![0, 0],
    }
}

/// The assembly's cap is the caller's count in INPUT order — never a ranking —
/// and the leaf's same-cap truncation over what the assembly transported drops
/// nobody. The `order-index` feature moves ONLY the simulation order of those
/// survivors: with it on, the leaf simulates them profit-descending (here the
/// reverse of the input order, because profit rises with the input position);
/// with it off the transported order is the simulation order.
///
/// The exact-order assertion is gated on THIS crate's `order-index` feature, the
/// only reliable proxy at this layer: a `--workspace` build unifies the feature
/// from the `degenbot` umbrella independently of this crate's own flag, and the
/// leaf's default-lane order is pinned by `degenbot-arbitrage`'s own suite.
#[test]
fn an_over_cap_assembly_keeps_the_input_prefix_and_the_leaf_truncates_nobody() {
    let cap = MAX_SIMULATE_CONCURRENT;
    let cap_u64 = u64::try_from(cap).expect("the cap fits u64");
    let over = cap_u64 + 10;
    let rows: Vec<RawResult> = (1..=over).map(|id| ordered_row(id, BLOCK)).collect();
    let resolver = MapResolver((1..=over).map(|id| (id, v2_path())).collect());

    let composed = Arc::new(Mutex::new(Vec::new()));
    let registry = DeploymentRegistry::new(vec![deployment_over(
        deployment_a(),
        Arc::new(RecordingAdapter {
            inner: adapter_a(),
            composed: Arc::clone(&composed),
        }),
        TARGET_A,
    )])
    .expect("one identity composes a valid registry");

    let arcs = policy_arcs();
    let assembled = {
        let mut suppression = arcs.suppression.lock().expect("suppression ledger");
        assemble_batch(&AssemblyInputs {
            rows: &rows,
            payloads: &[],
            resolver: &resolver,
            payload_served: &HashSet::new(),
            suppression: &mut suppression,
            divergence: &arcs.divergence,
            fot: &arcs.fot,
            bot_state: None,
            current_block: BLOCK,
            min_profit_margin_bps: 0,
            max_candidates: cap,
            target: TARGET_A,
            deployment_id: deployment_a(),
        })
        .expect("the fixture rows are well formed")
    };

    let transported: Vec<u64> = assembled
        .candidates
        .iter()
        .map(|admitted| admitted.candidate().candidate_id.path_id())
        .collect();
    assert_eq!(
        transported,
        (1..=cap_u64).collect::<Vec<u64>>(),
        "the assembly cap keeps the INPUT prefix, whatever the profits are"
    );
    assert!(
        assembled.outcomes.is_empty(),
        "a cap-dropped row holds no record"
    );

    let outcome = run_admitted_fanout(&registry, assembled.candidates, &arcs);
    let lane_a = lane(&outcome, deployment_a());
    assert_eq!(
        lane_a.candidate_count, cap,
        "the leaf's same-cap truncation over the transported survivors drops nobody"
    );
    assert_eq!(
        lane_a.suppressed_count
            + lane_a.divergent_dropped
            + lane_a.fot_dropped
            + lane_a.stale_dropped,
        0,
        "no survivor was dropped at finalization"
    );

    let simulated = composed.lock().expect("compose log").clone();
    assert_eq!(
        simulated.len(),
        cap,
        "every transported survivor reached the simulation body"
    );
    let mut permutation = simulated.clone();
    permutation.sort_unstable();
    assert_eq!(
        permutation,
        (1..=cap_u64)
            .map(|id| ORDERED_INPUT + u128::from(id))
            .collect::<Vec<u128>>(),
        "the simulated set is exactly the transported prefix"
    );
    if cfg!(feature = "order-index") {
        assert_eq!(
            simulated,
            (1..=cap_u64)
                .rev()
                .map(|id| ORDERED_INPUT + u128::from(id))
                .collect::<Vec<u128>>(),
            "the feature reorders the simulation profit-descending (profit rises with the input position)"
        );
    }
}

/// The standalone leaf panics on the same ledger: the batch keeps the leaf that
/// panics loudly and only its own ASSEMBLY recovers a poisoned registry.
#[test]
#[should_panic(expected = "suppression mutex poisoned")]
fn a_poisoned_ledger_panics_at_the_standalone_leaf() {
    let suppression = poisoned_ledger();
    let registry = registry();
    let provider = mock_provider();
    let block = block(&provider);
    let arcs = policy_arcs();
    let deployment = registry.entries().first().expect("one entry").clone();
    let ctx = degenbot_arbitrage::SimulateContext {
        block,
        deployment: &deployment,
    };
    let _ = degenbot_arbitrage::dispatch_profitable_results(
        Vec::new(),
        &ctx,
        &suppression,
        BLOCK,
        0,
        &arcs.divergence,
        &arcs.fot,
        None,
        None,
    );
}
