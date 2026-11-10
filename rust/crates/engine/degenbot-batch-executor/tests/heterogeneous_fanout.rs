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

use std::collections::HashMap;
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
    CmdExecutorBinding, DeploymentRegistry, FeeOnTransferRegistry, PoolDivergence, SimulationBlock,
    SimulationDeployment, TargetSelection,
};
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
                runtime.block_on(async {
                    degenbot_arbitrage::dispatch_across_deployments(
                        vec![candidate(deployment_a()), candidate(deployment_b())],
                        &block,
                        &registry,
                        degenbot_arbitrage::TargetSelection::AllTargets,
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        Some(&Arc::new(StateLock::new(BotState::new()))),
                        Some(&degenbot_simulation::WarmCodeCacheInner::shared_default()),
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
