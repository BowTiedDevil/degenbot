#![expect(clippy::expect_used)]

//! Explicit all-target fan-out over an ordered deployment registry (ADR-098 D4).
//!
//! One solved path, two MEANINGFULLY DIFFERENT execution targets: distinct
//! addresses, distinct calldata (two different contracts' ABIs), and distinct
//! native values (one prepays for its own execution, the other does not). The
//! registry fan-out must bind the path to BOTH deployments, produce two
//! distinct `CandidateId`s with independently correct results, and confine a
//! refusal at one target to that target's candidate.

use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    dispatch_across_deployments, dispatch_profitable_results, DeploymentRegistry,
    DeploymentRegistryError, DispatchCandidate, SimulateContext, SimulationBlock,
    SimulationDeployment, SolveStep, TargetSelection,
};
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpecs, TransactionRequest,
    V2HopInfo,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");

/// The first target's contract — a contract that prepays for its execution.
const TARGET_A: Address = address!("0x00000000000000000000000000000000000000a1");

/// The second target's contract — a different contract, different ABI, funded
/// from its own balance.
const TARGET_B: Address = address!("0x00000000000000000000000000000000000000b2");

const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;

/// Target A's prepayment: its contract needs the native value up front.
const PREPAY_A: U256 = U256::from_limbs([5_000_000_000_000_000, 0, 0, 0]);

/// Each adapter reports its own gross figure — the seam never infers one.
const GROSS_A: U256 = U256::from_limbs([9_000_000_000_000_000, 0, 0, 0]);
const GROSS_B: U256 = U256::from_limbs([7_000_000_000_000_000, 0, 0, 0]);

/// The deployments' operator-assigned identities.
fn deployment_a() -> DeploymentId {
    DeploymentId::new(2)
}

fn deployment_b() -> DeploymentId {
    DeploymentId::new(3)
}

/// The settled path every target is offered (one V2 hop — the hop set is what
/// the post-target join keys its mutual exclusion on).
fn v2_path() -> PathInfo {
    PathInfo::new(vec![degenbot_execution::HopInfo::V2(V2HopInfo {
        pool_address: address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        token0_address: WETH,
        token1_address: address!("0x3333333333333333333333333333333333333333"),
        fee: 30,
        zfo: true,
    })])
}

fn candidate(deployment: DeploymentId) -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: CandidateId::new(7, deployment),
        optimal_input: OPTIMAL_INPUT,
        engine_profit: 1,
        steps: Box::new([SolveStep {
            output: OPTIMAL_INPUT,
            consumed_input: OPTIMAL_INPUT,
            state_nonce: 0,
        }]),
        solve_block: BLOCK,
        path_info: v2_path(),
        prepared: None,
    }
}

/// One foreign target's adapter: its OWN ABI (a selector plus the optimal input
/// word), its own native `value`, its own gross figure, and — optionally — a
/// capability refusal rather than an encoding.
#[derive(Clone, Copy)]
struct TargetAdapter {
    target: Address,
    selector: [u8; 4],
    value: U256,
    gross: U256,
    refuses: bool,
}

impl TargetAdapter {
    const fn encoder(target: Address, selector: [u8; 4], value: U256, gross: U256) -> Self {
        Self {
            target,
            selector,
            value,
            gross,
            refuses: false,
        }
    }

    /// The same adapter over a contract that cannot encode this path at all.
    const fn refusing(mut self) -> Self {
        self.refuses = true;
        self
    }

    /// This adapter's own pricing policy: no market-percentile guess (the seam
    /// must not re-price an adapter-owned deployment).
    const fn fee_policy() -> FeePolicy {
        FeePolicy {
            market_percentile: false,
            override_priority_fee: 0,
        }
    }
}

impl ExecutionAdapter for TargetAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        if self.refuses {
            return Err(ComposeError::unsupported(
                "this contract has no producer for this path",
            ));
        }
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
            &Self::fee_policy(),
        ))
    }
}

/// Compose one registry entry: no settlement identity, no declared code (the
/// simulated block executes the code the chain holds), and the target the
/// adapter encodes to.
fn deployment(id: DeploymentId, adapter: TargetAdapter) -> Arc<SimulationDeployment> {
    let target = adapter.target;
    deployment_declaring(id, adapter, target)
}

/// Compose a registry entry whose DECLARED target is chosen independently of
/// the address its adapter encodes to: the same address in every valid
/// composition, and a per-candidate refusal when they disagree.
fn deployment_declaring(
    id: DeploymentId,
    adapter: TargetAdapter,
    declared_target: Address,
) -> Arc<SimulationDeployment> {
    Arc::new(SimulationDeployment::new(
        id,
        Arc::new(adapter),
        OWNER,
        declared_target,
        GAS_LIMIT,
        None,
        None,
    ))
}

/// A transport that serves empty accounts (zero balance/nonce, no code, zero
/// storage) plus a block hash: the fixture needs no deployed bytecode, because
/// a call to an account with no code succeeds with empty return data and the
/// adapter's own Assess prices the result.
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
        base_fee_next: BASE_FEE,
        current_block: BLOCK,
        block_timestamp: 0,
        block_priority_fees: None,
    }
}

/// Run one registry fan-out under the explicit all-target policy (the fan-out
/// fixture's own).
fn run_fanout(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
) -> Result<degenbot_arbitrage::DeploymentFanOutOutcome, degenbot_arbitrage::DispatchRefusal> {
    run_fanout_with(registry, candidates, TargetSelection::AllTargets)
}

/// Run one registry fan-out under a named target-selection policy, on a
/// dedicated multi-thread runtime (the engine's sync DB bridge requires the
/// ambient runtime).
fn run_fanout_with(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
    selection: TargetSelection,
) -> Result<degenbot_arbitrage::DeploymentFanOutOutcome, degenbot_arbitrage::DispatchRefusal> {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let block = block(&provider);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(degenbot_arbitrage::PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(degenbot_arbitrage::FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                runtime.block_on(async {
                    dispatch_across_deployments(
                        candidates,
                        &block,
                        registry,
                        selection,
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        Some(&bot_state),
                        Some(&warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

fn prepay_selector() -> [u8; 4] {
    [0xa1, 0xa2, 0xa3, 0xa4]
}

fn settle_selector() -> [u8; 4] {
    [0xb1, 0xb2, 0xb3, 0xb4]
}

/// The two-target fixture: target A prepays and reports `GROSS_A`; target B is a
/// different contract with a different ABI, no native value, and `GROSS_B`.
fn two_target_registry() -> DeploymentRegistry {
    DeploymentRegistry::new(vec![
        deployment(
            deployment_a(),
            TargetAdapter::encoder(TARGET_A, prepay_selector(), PREPAY_A, GROSS_A),
        ),
        deployment(
            deployment_b(),
            TargetAdapter::encoder(TARGET_B, settle_selector(), U256::ZERO, GROSS_B),
        ),
    ])
    .expect("two distinct identities compose a valid registry")
}

/// One solved path becomes TWO distinct candidates — one per registered
/// deployment — with independently correct transactions and economics.
#[test]
fn one_path_fans_out_to_every_registered_deployment() {
    let registry = two_target_registry();
    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
    )
    .expect("every candidate names a registered deployment");

    // One lane per registered deployment, in registry order.
    let lane_ids: Vec<DeploymentId> = outcome
        .lanes
        .iter()
        .map(|lane| lane.deployment_id)
        .collect();
    assert_eq!(
        lane_ids,
        vec![deployment_a(), deployment_b()],
        "lanes follow registry order"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(
        (lane_a.gas_profitable.len(), lane_b.gas_profitable.len()),
        (1, 1),
        "both targets simulated their own candidate: buckets A {:?} B {:?}",
        lane_a.fail_buckets.buckets(),
        lane_b.fail_buckets.buckets()
    );

    let result_a = &lane_a.gas_profitable[0];
    let result_b = &lane_b.gas_profitable[0];
    assert_ne!(
        result_a.candidate_id, result_b.candidate_id,
        "one path at two targets is two candidates"
    );
    assert_eq!(result_a.candidate_id.path_id(), 7);
    assert_eq!(result_a.candidate_id.deployment_id(), deployment_a());
    assert_eq!(result_b.candidate_id.deployment_id(), deployment_b());

    // Each result is ITS OWN adapter's: gross, calldata, and native value.
    assert_eq!(result_a.gross_profit, GROSS_A);
    assert_eq!(result_b.gross_profit, GROSS_B);
    assert_eq!(&result_a.execute_calldata[..4], &prepay_selector());
    assert_eq!(&result_b.execute_calldata[..4], &settle_selector());
    assert_eq!(
        &result_a.execute_calldata[4..],
        &U256::from(OPTIMAL_INPUT).to_be_bytes::<32>(),
        "each adapter encodes the same solved input into its own ABI"
    );
    assert_eq!(result_a.value, PREPAY_A, "target A prepays");
    assert_eq!(result_b.value, U256::ZERO, "target B funds itself");

    // The deployment facts keep each target honest.
    assert_eq!(
        registry.get(deployment_a()).map(|d| d.target()),
        Some(TARGET_A)
    );
    assert_eq!(
        registry.get(deployment_b()).map(|d| d.target()),
        Some(TARGET_B)
    );
}

/// A capability refusal at ONE target is that candidate's verdict and nothing
/// more: the other target's candidate still simulates and still reports.
#[test]
fn a_refusal_at_one_target_does_not_suppress_another() {
    let registry = DeploymentRegistry::new(vec![
        deployment(
            deployment_a(),
            TargetAdapter::encoder(TARGET_A, prepay_selector(), PREPAY_A, GROSS_A).refusing(),
        ),
        deployment(
            deployment_b(),
            TargetAdapter::encoder(TARGET_B, settle_selector(), U256::ZERO, GROSS_B),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
    )
    .expect("every candidate names a registered deployment");

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert!(
        lane_a.gas_profitable.is_empty(),
        "a refused encode produces no result"
    );
    assert_eq!(lane_a.fail_count, 1, "the refusal is counted, not raised");
    assert_eq!(lane_a.exception_count, 0);
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        Some(1),
        "the refusal is a per-candidate bucket: {:?}",
        lane_a.fail_buckets.buckets()
    );

    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(
        lane_b.gas_profitable.len(),
        1,
        "the other target's candidate is untouched: buckets {:?}",
        lane_b.fail_buckets.buckets()
    );
    assert_eq!(
        lane_b.gas_profitable[0].candidate_id.deployment_id(),
        deployment_b()
    );
    assert_eq!(lane_b.gas_profitable[0].gross_profit, GROSS_B);
}

/// An adapter that encodes away from its deployment's DECLARED target is a
/// per-candidate refusal: the engine executes the transaction's own `to`, so a
/// transaction aimed at a second contract would run code the join never
/// announced. The refusal is attributed to the mismatching candidate, yields no
/// `SimResult` (hence no submit row), and leaves the other target's candidate
/// untouched.
#[test]
fn an_adapter_encoding_away_from_its_declared_target_is_refused() {
    let registry = DeploymentRegistry::new(vec![
        // Declares TARGET_A but encodes TARGET_B — a different contract.
        deployment_declaring(
            deployment_a(),
            TargetAdapter::encoder(TARGET_B, prepay_selector(), PREPAY_A, GROSS_A),
            TARGET_A,
        ),
        deployment(
            deployment_b(),
            TargetAdapter::encoder(TARGET_B, settle_selector(), U256::ZERO, GROSS_B),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
    )
    .expect("every candidate names a registered deployment");

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert!(
        lane_a.gas_profitable.is_empty(),
        "a mismatched target yields no submittable SimResult: buckets {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(lane_a.fail_count, 1, "the refusal is counted, not raised");
    assert_eq!(lane_a.exception_count, 0);
    assert_eq!(
        lane_a
            .fail_buckets
            .buckets()
            .get("target-mismatch")
            .copied(),
        Some(1),
        "the refusal is the target-mismatch bucket: {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(lane_a.failures.len(), 1, "one failure record");
    assert_eq!(lane_a.failures[0].bucket, "target-mismatch");
    assert_eq!(
        lane_a.failures[0].candidate_id,
        CandidateId::new(7, deployment_a()),
        "the refusal names the candidate whose adapter encoded away"
    );

    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(
        lane_b.gas_profitable.len(),
        1,
        "the other target's candidate is untouched: buckets {:?}",
        lane_b.fail_buckets.buckets()
    );
    assert_eq!(
        lane_b.gas_profitable[0].candidate_id.deployment_id(),
        deployment_b()
    );
    assert_eq!(lane_b.gas_profitable[0].gross_profit, GROSS_B);
}

/// A one-entry registry is the one-deployment leaf: same candidates, same
/// survivors, same verdicts.
#[test]
fn a_one_entry_registry_matches_the_single_deployment_leaf() {
    let adapter = TargetAdapter::encoder(TARGET_A, prepay_selector(), PREPAY_A, GROSS_A);
    let single = deployment(deployment_a(), adapter);
    let registry =
        DeploymentRegistry::new(vec![Arc::clone(&single)]).expect("one entry is a valid registry");

    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let block = block(&provider);
    let (via_registry, via_leaf) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(degenbot_arbitrage::PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(degenbot_arbitrage::FeeOnTransferRegistry::new()));
                let registry_run = runtime.block_on(async {
                    dispatch_across_deployments(
                        vec![candidate(deployment_a())],
                        &block,
                        &registry,
                        TargetSelection::AllTargets,
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        Some(&Arc::new(StateLock::new(BotState::new()))),
                        Some(&degenbot_simulation::WarmCodeCacheInner::shared_default()),
                    )
                });
                let ctx = SimulateContext {
                    block: block.clone(),
                    deployment: &single,
                };
                let leaf_run = runtime.block_on(async {
                    dispatch_profitable_results(
                        vec![candidate(deployment_a())],
                        &ctx,
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        // The one-deployment leaf consumes the engine handles;
                        // the registry fan-out borrows the same ones per lane.
                        Some(Arc::new(StateLock::new(BotState::new()))),
                        Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
                    )
                    .expect("the fixture carries one row per candidate identity")
                });
                (registry_run, leaf_run)
            })
            .join()
            .expect("fan-out thread")
    });

    let via_registry = via_registry.expect("the one-entry registry fans out");
    let lane = via_registry.lanes.first().expect("one lane");
    assert_eq!(via_registry.lanes.len(), 1);
    assert_eq!(lane.deployment_id, deployment_a());
    assert_eq!(
        lane.outcome.candidate_count, via_leaf.candidate_count,
        "the cap and the survivor set are the same"
    );
    assert_eq!(
        lane.outcome.gas_profitable.len(),
        via_leaf.gas_profitable.len()
    );
    assert_eq!(
        lane.outcome.gas_profitable[0].candidate_id,
        via_leaf.gas_profitable[0].candidate_id
    );
    assert_eq!(
        lane.outcome.gas_profitable[0].net_profit, via_leaf.gas_profitable[0].net_profit,
        "one entry re-prices nothing"
    );
    assert_eq!(lane.outcome.fail_count, via_leaf.fail_count);
}

/// A candidate naming a deployment the session never registered is refused
/// before any lane runs.
#[test]
fn an_unregistered_candidate_deployment_is_refused() {
    let registry = two_target_registry();
    let error = run_fanout(&registry, vec![candidate(DeploymentId::new(99))])
        .expect_err("deployment 99 is not registered");
    assert_eq!(
        error,
        degenbot_arbitrage::DispatchRefusal::UnregisteredDeployment {
            candidate_id: CandidateId::new(7, DeploymentId::new(99)),
            registered: vec![deployment_a(), deployment_b()],
        }
    );
    assert!(error.to_string().contains("not registered"), "{error}");
}

/// An empty registry has no target to encode against: refused loudly.
#[test]
fn an_empty_registry_is_refused() {
    assert_eq!(
        DeploymentRegistry::new(Vec::new()).expect_err("empty"),
        DeploymentRegistryError::Empty
    );
}

/// Two entries with one identity make every post-target row ambiguous: refused
/// loudly.
#[test]
fn a_duplicate_identity_is_refused() {
    let adapter = TargetAdapter::encoder(TARGET_A, prepay_selector(), PREPAY_A, GROSS_A);
    assert_eq!(
        DeploymentRegistry::new(vec![
            deployment(deployment_a(), adapter),
            deployment(deployment_a(), adapter),
        ])
        .expect_err("duplicate identity"),
        DeploymentRegistryError::DuplicateId {
            deployment_id: deployment_a()
        }
    );
}

/// A deployment owner the session's signer does not hold is refused loudly
/// rather than simulated from an account the session cannot sign for.
#[test]
fn an_owner_the_signer_cannot_sign_for_is_refused() {
    let registry = two_target_registry();
    let authority = address!("0x2222222222222222222222222222222222222222");
    assert_eq!(
        registry
            .validate_signing_authority(authority)
            .expect_err("the fixture's owner is not this authority"),
        DeploymentRegistryError::OwnerNotSigner {
            deployment_id: deployment_a(),
            owner: OWNER,
            authority,
        }
    );
    assert!(
        registry.validate_signing_authority(OWNER).is_ok(),
        "the owner that signed is the account the session holds"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Target selection: ordered first-compatible beside the explicit fan-out
// ─────────────────────────────────────────────────────────────────────────

/// How a fixture target answers one compose call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ComposeAnswer {
    /// Accept, stamping the CALL NUMBER into the calldata.
    Accept,
    /// Refuse for want of capability — the routine decline a first-compatible
    /// walk advances past.
    Unsupported,
    /// Fail a primitive encoding step.
    HardEncode,
}

/// A target whose compose callback is COUNTED.
///
/// Every `encode` call increments the shared counter, and an accepted
/// transaction carries the number of the call that produced it — so a second
/// composition of the same candidate shows up in the simulated bytes, not only
/// in the counter.
struct CountedAdapter {
    target: Address,
    selector: [u8; 4],
    value: U256,
    gross: U256,
    answer: ComposeAnswer,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl CountedAdapter {
    fn new(
        target: Address,
        selector: [u8; 4],
        value: U256,
        gross: U256,
        answer: ComposeAnswer,
        calls: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            target,
            selector,
            value,
            gross,
            answer,
            calls: Arc::clone(calls),
        }
    }
}

impl ExecutionAdapter for CountedAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        match self.answer {
            ComposeAnswer::Unsupported => Err(ComposeError::unsupported(
                "this contract has no producer for this path",
            )),
            ComposeAnswer::HardEncode => {
                Err(ComposeError::encode("optimal_input does not fit int128"))
            }
            ComposeAnswer::Accept => {
                let mut data = Vec::with_capacity(68);
                data.extend_from_slice(&self.selector);
                data.extend_from_slice(&U256::from(call).to_be_bytes::<32>());
                data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
                Ok(TransactionRequest::new(
                    self.target,
                    Bytes::from(data),
                    self.value,
                ))
            }
        }
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
            &TargetAdapter::fee_policy(),
        ))
    }
}

/// Compose one counted target's registry entry (no settlement identity, no
/// declared code: the simulated block executes the code the chain holds).
fn counted_deployment(id: DeploymentId, adapter: CountedAdapter) -> Arc<SimulationDeployment> {
    let target = adapter.target;
    Arc::new(SimulationDeployment::new(
        id,
        Arc::new(adapter),
        OWNER,
        target,
        GAS_LIMIT,
        None,
        None,
    ))
}

/// One fixture target's compose-call count.
fn compositions(calls: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

fn calls_arc() -> Arc<std::sync::atomic::AtomicUsize> {
    Arc::new(std::sync::atomic::AtomicUsize::new(0))
}

fn word(bytes: &[u8]) -> U256 {
    U256::from_be_slice(bytes)
}

fn survivor_ids(outcome: &degenbot_arbitrage::DeploymentFanOutOutcome) -> Vec<CandidateId> {
    outcome
        .lanes
        .iter()
        .flat_map(|lane| {
            lane.outcome
                .gas_profitable
                .iter()
                .map(|result| result.candidate_id)
        })
        .collect()
}

/// Ordered first-compatible: the first accepting target is composed ONCE and
/// simulated with exactly the transaction it accepted, and no later target is
/// composed at all.
#[test]
fn the_first_compatible_policy_selects_only_the_first_accepting_target() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = DeploymentRegistry::new(vec![
        counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Accept,
                &calls_a,
            ),
        ),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                settle_selector(),
                U256::ZERO,
                GROSS_B,
                ComposeAnswer::Accept,
                &calls_b,
            ),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        compositions(&calls_a),
        1,
        "the selected target composed once"
    );
    assert_eq!(
        compositions(&calls_b),
        0,
        "a later compatible target is never composed"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(lane_a.gas_profitable.len(), 1);
    assert!(
        lane_b.gas_profitable.is_empty(),
        "the unexamined target simulated nothing: buckets {:?}",
        lane_b.fail_buckets.buckets()
    );
    assert_eq!(
        lane_b.candidate_count, 0,
        "no candidate reached the later lane"
    );
    assert_eq!(
        lane_b.fail_count, 0,
        "an unexamined target reports no failure of its own"
    );

    let result = &lane_a.gas_profitable[0];
    assert_eq!(result.candidate_id, CandidateId::new(7, deployment_a()));
    assert_eq!(
        result.gross_profit, GROSS_A,
        "the selected adapter assessed"
    );
    assert_eq!(result.value, PREPAY_A, "its own native value rode along");
    assert_eq!(&result.execute_calldata[..4], &prepay_selector());
    assert_eq!(
        word(&result.execute_calldata[4..36]),
        U256::from(1),
        "the simulated bytes are the FIRST (and only) compose's"
    );
    assert_eq!(
        word(&result.execute_calldata[36..68]),
        U256::from(OPTIMAL_INPUT)
    );
    assert_eq!(
        outcome.retired,
        vec![CandidateId::new(7, deployment_b())],
        "the target the walk never reached is retired unexamined"
    );
}

/// An early capability refusal advances the walk: exactly one composition per
/// examined target, the accepting target's own request simulated verbatim, and
/// the refusal left on the refusing target's own record.
#[test]
fn a_capability_decline_advances_and_the_accepted_request_is_simulated_once() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = DeploymentRegistry::new(vec![
        counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Unsupported,
                &calls_a,
            ),
        ),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                settle_selector(),
                U256::ZERO,
                GROSS_B,
                ComposeAnswer::Accept,
                &calls_b,
            ),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        compositions(&calls_a),
        1,
        "the declined target composed once"
    );
    assert_eq!(
        compositions(&calls_b),
        1,
        "the accepting target composed once"
    );
    assert!(
        outcome.retired.is_empty(),
        "the walk examined both targets: {:?}",
        outcome.retired
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert!(lane_a.gas_profitable.is_empty());
    assert_eq!(lane_a.fail_count, 1, "the refusal is counted, not raised");
    assert_eq!(lane_a.exception_count, 0);
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        Some(1),
        "a capability refusal is the routine bucket: {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(lane_a.failures.len(), 1);
    assert_eq!(
        lane_a.failures[0].candidate_id,
        CandidateId::new(7, deployment_a()),
        "the refusal stays on the refusing target's own candidate"
    );

    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(lane_b.gas_profitable.len(), 1);
    let result = &lane_b.gas_profitable[0];
    assert_eq!(result.candidate_id, CandidateId::new(7, deployment_b()));
    assert_eq!(&result.execute_calldata[..4], &settle_selector());
    assert_eq!(
        word(&result.execute_calldata[4..36]),
        U256::from(1),
        "the accepted transaction is reused, never composed again for the sim"
    );
    assert_eq!(
        word(&result.execute_calldata[36..68]),
        U256::from(OPTIMAL_INPUT),
        "to/data/value are the accepted request's"
    );
    assert_eq!(result.value, U256::ZERO);
    assert_eq!(result.gross_profit, GROSS_B);
}

/// Every target declining selects nothing: no lane reports a success, and each
/// examined candidate keeps its own honest refusal.
#[test]
fn every_target_declining_selects_nothing_and_keeps_each_refusal() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = DeploymentRegistry::new(vec![
        counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Unsupported,
                &calls_a,
            ),
        ),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                settle_selector(),
                U256::ZERO,
                GROSS_B,
                ComposeAnswer::Unsupported,
                &calls_b,
            ),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        (compositions(&calls_a), compositions(&calls_b)),
        (1, 1),
        "one composition per examined target"
    );
    assert!(survivor_ids(&outcome).is_empty(), "no selected success");
    assert!(outcome.retired.is_empty(), "all targets were examined");

    for deployment in [deployment_a(), deployment_b()] {
        let lane = outcome
            .lane(deployment)
            .expect("each deployment has a lane");
        assert!(lane.gas_profitable.is_empty());
        assert_eq!(lane.exception_count, 0, "a refusal is not an exception");
        assert_eq!(lane.fail_count, 1);
        assert_eq!(
            lane.fail_buckets.buckets().get("encode-failed").copied(),
            Some(1)
        );
        assert_eq!(
            lane.failures
                .iter()
                .map(|failure| failure.candidate_id)
                .collect::<Vec<_>>(),
            vec![CandidateId::new(7, deployment)],
            "each refusal is attributed to its own candidate"
        );
    }
}

/// The two policies differ ONLY by which targets are examined: the winning
/// lane's result is the same, and the explicit fan-out is the one that composes
/// (and simulates) both.
#[test]
fn the_policies_differ_only_by_which_targets_are_examined() {
    let build = || {
        let calls_a = calls_arc();
        let calls_b = calls_arc();
        let registry = DeploymentRegistry::new(vec![
            counted_deployment(
                deployment_a(),
                CountedAdapter::new(
                    TARGET_A,
                    prepay_selector(),
                    PREPAY_A,
                    GROSS_A,
                    ComposeAnswer::Accept,
                    &calls_a,
                ),
            ),
            counted_deployment(
                deployment_b(),
                CountedAdapter::new(
                    TARGET_B,
                    settle_selector(),
                    U256::ZERO,
                    GROSS_B,
                    ComposeAnswer::Accept,
                    &calls_b,
                ),
            ),
        ])
        .expect("two distinct identities compose a valid registry");
        (registry, calls_a, calls_b)
    };

    let (registry, calls_a, calls_b) = build();
    let all = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::AllTargets,
    )
    .expect("every candidate names a registered deployment");
    assert_eq!(
        (compositions(&calls_a), compositions(&calls_b)),
        (1, 1),
        "each deployment composes its own candidate"
    );
    assert_eq!(
        survivor_ids(&all),
        vec![
            CandidateId::new(7, deployment_a()),
            CandidateId::new(7, deployment_b())
        ],
        "the explicit fan-out keeps two independent candidates"
    );
    assert!(
        all.retired.is_empty(),
        "nothing is retired under all-targets"
    );

    let (registry, calls_a, calls_b) = build();
    let first = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");
    assert_eq!(
        (compositions(&calls_a), compositions(&calls_b)),
        (1, 0),
        "only the first target is examined"
    );
    assert_eq!(
        survivor_ids(&first),
        vec![CandidateId::new(7, deployment_a())]
    );

    let all_result = &all.lane(deployment_a()).expect("lane A").gas_profitable[0];
    let first_result = &first.lane(deployment_a()).expect("lane A").gas_profitable[0];
    assert_eq!(first_result.candidate_id, all_result.candidate_id);
    assert_eq!(first_result.gross_profit, all_result.gross_profit);
    assert_eq!(first_result.net_profit, all_result.net_profit);
    assert_eq!(first_result.value, all_result.value);
    assert_eq!(
        first_result.execute_calldata, all_result.execute_calldata,
        "the winning lane's transaction is its target's either way"
    );
}

/// A hard Encode failure is NOT a capability decline: it ends the walk as that
/// candidate's own exception, and no later target is composed.
#[test]
fn a_hard_encode_failure_is_not_a_capability_decline() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = DeploymentRegistry::new(vec![
        counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::HardEncode,
                &calls_a,
            ),
        ),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                settle_selector(),
                U256::ZERO,
                GROSS_B,
                ComposeAnswer::Accept,
                &calls_b,
            ),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        compositions(&calls_a),
        1,
        "the failing target composed once"
    );
    assert_eq!(
        compositions(&calls_b),
        0,
        "a primitive encode failure is not evidence the next target fares better"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert!(lane_a.gas_profitable.is_empty());
    assert_eq!(
        lane_a.exception_count,
        1,
        "a hard encode error is this candidate's exception: buckets {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        None,
        "the exception is never folded into the routine refusal bucket"
    );

    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(lane_b.candidate_count, 0);
    assert!(lane_b.gas_profitable.is_empty());
    assert_eq!(outcome.retired, vec![CandidateId::new(7, deployment_b())]);
}

/// A transaction aimed away from the deployment's declared target is that
/// candidate's refusal, not a capability decline: the walk stops there and the
/// later target is never composed.
#[test]
fn a_target_mismatch_is_not_a_capability_decline() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    // This target's adapter encodes TARGET_B while its deployment declares
    // TARGET_A: a mismatch the engine refuses per candidate.
    let registry = DeploymentRegistry::new(vec![
        Arc::new(SimulationDeployment::new(
            deployment_a(),
            Arc::new(CountedAdapter::new(
                TARGET_B,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Accept,
                &calls_a,
            )),
            OWNER,
            TARGET_A,
            GAS_LIMIT,
            None,
            None,
        )),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                settle_selector(),
                U256::ZERO,
                GROSS_B,
                ComposeAnswer::Accept,
                &calls_b,
            ),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(compositions(&calls_a), 1);
    assert_eq!(
        compositions(&calls_b),
        0,
        "a mismatched target does not advance the walk"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert!(lane_a.gas_profitable.is_empty());
    assert_eq!(lane_a.exception_count, 0);
    assert_eq!(
        lane_a
            .fail_buckets
            .buckets()
            .get("target-mismatch")
            .copied(),
        Some(1),
        "the refusal is the target-mismatch bucket: {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(outcome.retired, vec![CandidateId::new(7, deployment_b())]);
}

/// Two deployments MAY run the same implementation: distinct identities are what
/// the registry requires, and the first-compatible walk picks the first identity
/// in registry order.
#[test]
fn deployments_sharing_one_implementation_keep_distinct_identities() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = DeploymentRegistry::new(vec![
        counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Accept,
                &calls_a,
            ),
        ),
        counted_deployment(
            deployment_b(),
            CountedAdapter::new(
                TARGET_B,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Accept,
                &calls_b,
            ),
        ),
    ])
    .expect("one implementation at two addresses is two identities");

    assert_ne!(deployment_a(), deployment_b());
    let outcome = run_fanout_with(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!((compositions(&calls_a), compositions(&calls_b)), (1, 0));
    assert_eq!(
        survivor_ids(&outcome),
        vec![CandidateId::new(7, deployment_a())]
    );
    assert_eq!(
        outcome.retired,
        vec![CandidateId::new(7, deployment_b())],
        "the same implementation at a later address is retired, not merged"
    );
}

/// A one-entry registry is unaffected by the policy: one deployment, one
/// composition, one candidate.
#[test]
fn a_one_entry_registry_selects_the_same_deployment_under_either_policy() {
    for policy in [
        TargetSelection::FirstCompatible,
        TargetSelection::AllTargets,
    ] {
        let calls = calls_arc();
        let registry = DeploymentRegistry::new(vec![counted_deployment(
            deployment_a(),
            CountedAdapter::new(
                TARGET_A,
                prepay_selector(),
                PREPAY_A,
                GROSS_A,
                ComposeAnswer::Accept,
                &calls,
            ),
        )])
        .expect("one entry is a valid registry");

        let outcome = run_fanout_with(&registry, vec![candidate(deployment_a())], policy)
            .expect("the candidate names the registered deployment");

        assert_eq!(compositions(&calls), 1, "{policy:?}: one composition");
        assert_eq!(outcome.lanes.len(), 1);
        assert_eq!(
            survivor_ids(&outcome),
            vec![CandidateId::new(7, deployment_a())]
        );
        assert!(outcome.retired.is_empty(), "{policy:?}: nothing to retire");
        let result = &outcome.lane(deployment_a()).expect("lane A").gas_profitable[0];
        assert_eq!(result.gross_profit, GROSS_A);
        assert_eq!(result.value, PREPAY_A);
    }
}

/// The policy spellings are closed: a configuration name selects its policy, and
/// an unknown name selects none, so its caller reports it instead of defaulting.
#[test]
fn the_policy_spellings_are_closed() {
    assert_eq!(
        TargetSelection::from_name("first_compatible"),
        Some(TargetSelection::FirstCompatible)
    );
    assert_eq!(
        TargetSelection::from_name("all_targets"),
        Some(TargetSelection::AllTargets)
    );
    assert_eq!(TargetSelection::from_name("all_target"), None);
    assert_eq!(TargetSelection::from_name(""), None);
    assert_eq!(TargetSelection::default(), TargetSelection::FirstCompatible);
    assert_eq!(
        TargetSelection::default().name(),
        "first_compatible",
        "the default multi-target policy is ordered first-compatible"
    );
}
