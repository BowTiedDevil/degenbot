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
    SimulationDeployment, SolveStep,
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

/// Run one registry fan-out on a dedicated multi-thread runtime (the engine's
/// sync DB bridge requires the ambient runtime).
fn run_fanout(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
) -> Result<degenbot_arbitrage::DeploymentFanOutOutcome, degenbot_arbitrage::DeploymentFanOutError>
{
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
        degenbot_arbitrage::DeploymentFanOutError::UnregisteredDeployment {
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
