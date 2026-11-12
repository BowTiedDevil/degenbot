#![expect(clippy::expect_used)]

//! The PRE-ADMITTED fan-out entry, exercised from an EXTERNAL crate.
//!
//! `AdmittedCandidate` transports a candidate a caller has already projected
//! through its own pre-simulation admission, and
//! `dispatch_admitted_across_deployments` finalizes it authoritatively before
//! the shared simulation body runs. This target is an external consumer: a
//! name, method, or re-export that stops resolving is a compile error here
//! rather than a silent break for a `cargo add degenbot-arbitrage` caller.

use std::sync::{Arc, Mutex};

use alloy::primitives::aliases::U112;
use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    dispatch_across_deployments, dispatch_admitted_across_deployments, project_early_admission,
    AdmittedCandidate, DeploymentFanOutOutcome, DeploymentRegistry, DispatchCandidate,
    DispatchRefusal, EarlyAdmissionPolicy, EarlyDropReason, EarlyProjection, FeeOnTransferRegistry,
    PoolDivergence, PoolDivergenceKey, SimulationBlock, SimulationDeployment, SolveStep,
    TargetSelection, MAX_SIMULATE_CONCURRENT,
};
use degenbot_core::fee::FeeRatio;
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpecs, TransactionRequest,
    V2HopInfo,
};
use degenbot_pools::{DexVariant, RegisterV2PoolParams};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{PathSuppression, PATH_SUPPRESS_THRESHOLD};
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");
const TARGET_A: Address = address!("0x00000000000000000000000000000000000000a1");
const TARGET_B: Address = address!("0x00000000000000000000000000000000000000b2");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const OTHER: Address = address!("0x3333333333333333333333333333333333333333");
/// The fixture pools — one flaggable, one clean.
const POOL_A: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const POOL_B: Address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;

/// Each adapter reports its own gross figure.
const GROSS: U256 = U256::from_limbs([9_000_000_000_000_000, 0, 0, 0]);

fn deployment_a() -> DeploymentId {
    DeploymentId::new(1)
}

fn deployment_b() -> DeploymentId {
    DeploymentId::new(2)
}

/// The settled path every target is offered: one V2 hop through `POOL_A`.
fn v2_path() -> PathInfo {
    PathInfo::new(vec![degenbot_execution::HopInfo::V2(V2HopInfo {
        pool_address: POOL_A,
        token0_address: WETH,
        token1_address: OTHER,
        fee: 30,
        zfo: true,
    })])
}

/// One solved candidate at `deployment`, snapshotting `state_nonce` — the
/// fixture's freshness knob (`live_nonce` is fresh, anything else is stale).
fn candidate(deployment: DeploymentId, state_nonce: u64) -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: CandidateId::new(7, deployment),
        optimal_input: OPTIMAL_INPUT,
        engine_profit: 1,
        steps: Box::new([SolveStep {
            output: OPTIMAL_INPUT,
            consumed_input: OPTIMAL_INPUT,
            state_nonce,
        }]),
        solve_block: BLOCK,
        path_info: v2_path(),
        prepared: None,
    }
}

/// One foreign target's adapter: its own ABI, its own target, its own gross.
#[derive(Clone, Copy)]
struct TargetAdapter {
    target: Address,
    selector: [u8; 4],
}

impl TargetAdapter {
    const fn encoder(target: Address, selector: [u8; 4]) -> Self {
        Self { target, selector }
    }

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
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(&self.selector);
        data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
        Ok(TransactionRequest::new(
            self.target,
            Bytes::from(data),
            U256::ZERO,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        Ok(ExecutionResult::price(
            GROSS,
            Vec::new(),
            observation,
            U256::ZERO,
            &Self::fee_policy(),
        ))
    }
}

/// A transport that serves empty accounts (no code, zero storage) plus a block
/// hash: a call to an account with no code succeeds with empty return data, and
/// the adapter's own Assess prices the result.
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

fn deployment(id: DeploymentId, adapter: TargetAdapter) -> Arc<SimulationDeployment> {
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

/// The two-target registry, in registry order: `deployment_a` first.
fn two_target_registry() -> DeploymentRegistry {
    DeploymentRegistry::new(vec![
        deployment(
            deployment_a(),
            TargetAdapter::encoder(TARGET_A, [0xa1, 0xa2, 0xa3, 0xa4]),
        ),
        deployment(
            deployment_b(),
            TargetAdapter::encoder(TARGET_B, [0xb1, 0xb2, 0xb3, 0xb4]),
        ),
    ])
    .expect("two distinct identities compose a valid registry")
}

/// One session's live policy state + engine handles: the arcs a caller's
/// assembly and the leaf both read.
struct Harness {
    suppression: Arc<Mutex<PathSuppression>>,
    divergence: Arc<Mutex<PoolDivergence>>,
    fot: Arc<Mutex<FeeOnTransferRegistry>>,
    state: Arc<StateLock<BotState>>,
    warm_cache: Arc<parking_lot::RwLock<degenbot_simulation::WarmCodeCacheInner>>,
}

/// A `BotState` with every pool in `pools` registered as a V2 pair.
fn bot_state_with(pools: &[Address]) -> BotState {
    let mut core = BotState::new();
    for pool in pools {
        core.register_v2_pool(&RegisterV2PoolParams {
            address: *pool,
            token0: WETH,
            token1: OTHER,
            reserve0: U112::from(1_000_000u64),
            reserve1: U112::from(2_000_000u64),
            fee_token0: FeeRatio::new(997, 1_000).expect("valid fee"),
            fee_token1: FeeRatio::new(997, 1_000).expect("valid fee"),
            factory: Address::ZERO,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            update_block: 1,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
        })
        .expect("the fixture pool registers once");
    }
    core
}

impl Harness {
    fn new() -> Self {
        Self {
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            state: Arc::new(StateLock::new(bot_state_with(&[POOL_A, POOL_B]))),
            warm_cache: degenbot_simulation::WarmCodeCacheInner::shared_default(),
        }
    }

    /// The live `state_nonce` of `pool` — the value a fresh candidate
    /// snapshots.
    fn live_nonce(&self, pool: Address) -> u64 {
        let guard = self.state.read_at(LockSite::Sim);
        let pid = guard.pool_id_by_address(&pool).expect("the fixture pool");
        guard.pool_state_nonce(pid)
    }

    /// Advance `pool`'s state — the concurrent state writer a final read must
    /// observe.
    fn advance(&self, pool: Address) {
        let mut guard = self.state.write_at(LockSite::Sim);
        guard
            .apply_sync_by_address(
                pool,
                U112::from(1_500_000u64),
                U112::from(2_500_000u64),
                BLOCK,
            )
            .expect("the fixture pool is registered");
    }

    /// Bench `candidate_id` in the suppression ledger:
    /// `PATH_SUPPRESS_THRESHOLD` consecutive failures.
    fn bench(&self, candidate_id: CandidateId) {
        let mut suppression = self.suppression.lock().expect("suppression ledger");
        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(candidate_id);
        }
    }

    /// Is `candidate_id` suppressed at `block` — asked WITHOUT the retry stamp,
    /// so the probe cannot spend the interval it is checking.
    fn suppressed_without_stamp(&self, candidate_id: CandidateId, block: u64) -> bool {
        self.suppression
            .lock()
            .expect("suppression ledger")
            .is_suppressed_without_stamp(candidate_id, block)
    }

    /// Does the ledger hold a retry stamp that suppresses `candidate_id` at
    /// `block`?
    fn suppressed_at(&self, candidate_id: CandidateId, block: u64) -> bool {
        self.suppression
            .lock()
            .expect("suppression ledger")
            .is_suppressed(candidate_id, block)
    }
}

/// Is the candidate's retry interval still UNSPENT at `block`? The stamping
/// read spends it; the non-stamping read does not.
fn retry_due(harness: &Harness, candidate_id: CandidateId, block: u64) -> bool {
    !harness.suppressed_without_stamp(candidate_id, block)
}

/// Run one pre-admitted registry fan-out on a dedicated multi-thread runtime
/// (the engine's sync DB bridge requires the ambient runtime).
fn run_admitted(
    registry: &DeploymentRegistry,
    admitted: Vec<AdmittedCandidate>,
    selection: TargetSelection,
    harness: &Harness,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
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
                runtime.block_on(async {
                    dispatch_admitted_across_deployments(
                        admitted,
                        &block,
                        registry,
                        selection,
                        &harness.suppression,
                        BLOCK,
                        &harness.divergence,
                        &harness.fot,
                        Some(&harness.state),
                        Some(&harness.warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// Project raw rows through the shared EARLY projection — the one public way a
/// caller obtains the pre-admitted transport — over this session's policy state
/// at `BLOCK`, with no thin floor.
fn project(
    harness: &Harness,
    candidates: Vec<DispatchCandidate>,
    max_candidates: usize,
) -> EarlyProjection {
    let suppression = harness.suppression.lock().expect("suppression ledger");
    project_early_admission(
        candidates,
        &EarlyAdmissionPolicy {
            suppression: &suppression,
            divergence: &harness.divergence,
            fee_on_transfer: &harness.fot,
            bot_state: Some(&harness.state),
            current_block: BLOCK,
            min_profit_margin_bps: 0,
            max_candidates,
        },
    )
}

/// Project under the fan-out ceiling and keep the survivors: the transported
/// set a caller hands the pre-admitted entry.
fn transport(harness: &Harness, candidates: Vec<DispatchCandidate>) -> Vec<AdmittedCandidate> {
    project(harness, candidates, MAX_SIMULATE_CONCURRENT).into_admitted()
}

/// Run one RAW registry fan-out — the entry with NO pre-selection staleness
/// gate, so its leaf is the only place a stale candidate can be dropped.
fn run_raw(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
    selection: TargetSelection,
    harness: &Harness,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
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
                runtime.block_on(async {
                    dispatch_across_deployments(
                        candidates,
                        &block,
                        registry,
                        selection,
                        &harness.suppression,
                        BLOCK,
                        0,
                        &harness.divergence,
                        &harness.fot,
                        Some(&harness.state),
                        Some(&harness.warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// One lane's outcome, by deployment.
fn lane(
    outcome: &DeploymentFanOutOutcome,
    deployment: DeploymentId,
) -> &degenbot_arbitrage::DispatchOutcome {
    outcome
        .lane(deployment)
        .expect("the registry holds the lane")
}

// -- the public transport -------------------------------------------------

/// The ONE public construction: `AdmittedCandidate` is a crate-root name its
/// accessor hands the WHOLE candidate back from — no ID-keyed side table, no
/// re-derivation — and the shared projection is the only way to obtain one (the
/// unchecked constructor is crate-private).
#[test]
fn the_projection_is_the_public_construction_and_carries_the_complete_candidate() {
    let harness = Harness::new();
    let raw = candidate(deployment_a(), 0);
    let projection = project(&harness, vec![raw], MAX_SIMULATE_CONCURRENT);
    assert!(projection.drops().is_empty(), "a clean row rides whole");
    let admitted = &projection.admitted()[0];
    let carried = admitted.candidate();
    assert_eq!(carried.candidate_id, CandidateId::new(7, deployment_a()));
    assert_eq!(carried.optimal_input, OPTIMAL_INPUT);
    assert_eq!(carried.engine_profit, 1);
    assert_eq!(carried.solve_block, BLOCK);
    assert_eq!(carried.steps.len(), 1);
    assert_eq!(carried.steps[0].state_nonce, 0);
    assert_eq!(carried.path_info.hops.len(), 1);
    assert!(carried.prepared.is_none());
    assert!(format!("{admitted:?}").contains("AdmittedCandidate"));
}

/// An empty projection admits nothing and reports nothing, and its empty
/// transport fans out to one empty lane per registered deployment, in registry
/// order, with every counter zero.
#[test]
fn an_empty_projection_fans_out_to_empty_lanes() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let projection = project(&harness, Vec::new(), MAX_SIMULATE_CONCURRENT);
    assert!(projection.admitted().is_empty());
    assert!(projection.drops().is_empty());

    let outcome = run_admitted(
        &registry,
        projection.into_admitted(),
        TargetSelection::AllTargets,
        &harness,
    )
    .expect("no candidates to refuse");

    let ids: Vec<DeploymentId> = outcome.lanes.iter().map(|l| l.deployment_id).collect();
    assert_eq!(ids, vec![deployment_a(), deployment_b()]);
    assert!(outcome.retired.is_empty());
    for lane in &outcome.lanes {
        let o = &lane.outcome;
        assert_eq!(o.candidate_count, 0);
        assert_eq!(o.suppressed_count, 0);
        assert_eq!(o.thin_dropped, 0);
        assert_eq!(o.divergent_dropped, 0);
        assert_eq!(o.fot_dropped, 0);
        assert_eq!(o.stale_dropped, 0);
        assert_eq!(o.exception_count, 0);
        assert_eq!(o.fail_count, 0);
        assert_eq!(o.sim_ok_count(), 0);
    }
}

/// A transported candidate naming an identity the registry does not hold is
/// refused BEFORE any lane runs.
#[test]
fn an_unregistered_deployment_is_refused_before_any_lane_runs() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let unregistered = DeploymentId::new(9);
    let outcome = run_admitted(
        &registry,
        transport(&harness, vec![candidate(unregistered, 0)]),
        TargetSelection::AllTargets,
        &harness,
    );
    assert_eq!(
        outcome.err(),
        Some(DispatchRefusal::UnregisteredDeployment {
            candidate_id: CandidateId::new(7, unregistered),
            registered: vec![deployment_a(), deployment_b()],
        })
    );
}

// -- authoritative finalization -------------------------------------------

/// A retry-due candidate is STAMPED at the suppression stage and then dropped by
/// the final divergence read: the retry interval is spent either way.
#[test]
fn a_retry_due_candidate_is_stamped_then_dropped_by_the_final_divergence_read() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let id = CandidateId::new(7, deployment_a());
    harness.bench(id);

    // The projection admits the retry-due row (a non-stamping read: no retry
    // spent) and no divergence is flagged yet; the memo moves AFTER it.
    let admitted = transport(&harness, vec![candidate(deployment_a(), 0)]);
    assert_eq!(admitted.len(), 1, "the projection admitted the row");
    assert!(
        retry_due(&harness, id, BLOCK),
        "the projection spent no retry"
    );
    harness
        .divergence
        .lock()
        .expect("divergence memo")
        .record_divergence(PoolDivergenceKey::V2(POOL_A), BLOCK);

    let outcome = run_admitted(&registry, admitted, TargetSelection::AllTargets, &harness)
        .expect("the identity is registered");

    let dropped = lane(&outcome, deployment_a());
    assert_eq!(dropped.divergent_dropped, 1, "the final read dropped it");
    assert_eq!(dropped.suppressed_count, 0, "the retry was admitted");
    assert_eq!(
        dropped.thin_dropped, 0,
        "the pre-admitted leaf has no thin stage"
    );
    assert_eq!(dropped.candidate_count, 0);
    assert_eq!(dropped.fail_count, 0);
    assert!(
        harness.suppressed_at(id, BLOCK),
        "the stamp landed before the drop, so the retry is spent"
    );
    // The other lane never saw a candidate.
    let untouched = lane(&outcome, deployment_b());
    assert_eq!(untouched.candidate_count, 0);
    assert_eq!(untouched.divergent_dropped, 0);
}

/// A registry write BETWEEN admission and finalization is authoritative: the
/// transported candidate was clean when it was admitted.
#[test]
fn a_divergence_written_after_admission_drops_the_transported_candidate() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let admitted = transport(&harness, vec![candidate(deployment_a(), 0)]);
    // The concurrent writer: a SolverCalc divergence lands after admission.
    harness
        .divergence
        .lock()
        .expect("divergence memo")
        .record_divergence(PoolDivergenceKey::V2(POOL_A), BLOCK);

    let outcome = run_admitted(&registry, admitted, TargetSelection::AllTargets, &harness)
        .expect("the identity is registered");
    assert_eq!(lane(&outcome, deployment_a()).divergent_dropped, 1);
    assert_eq!(lane(&outcome, deployment_a()).candidate_count, 0);
}

/// Same authority for the fee-on-transfer registry: a token confirmed by two
/// distinct failing pools after admission drops the transport.
#[test]
fn a_fee_on_transfer_confirmation_after_admission_drops_the_transported_candidate() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let admitted = transport(&harness, vec![candidate(deployment_a(), 0)]);
    {
        let mut fot = harness.fot.lock().expect("fot registry");
        fot.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), BLOCK);
        fot.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), BLOCK);
    }

    let outcome = run_admitted(&registry, admitted, TargetSelection::AllTargets, &harness)
        .expect("the identity is registered");
    assert_eq!(lane(&outcome, deployment_a()).fot_dropped, 1);
    assert_eq!(lane(&outcome, deployment_a()).candidate_count, 0);
    assert_eq!(
        harness
            .fot
            .lock()
            .expect("fot registry")
            .total_fot_dropped(),
        1,
        "the final read feeds the same lifetime tally"
    );
}

/// And the same for the live state: a pool that advanced between admission and
/// finalization makes the transported candidate stale.
#[test]
fn a_state_write_after_admission_drops_the_transported_candidate_as_stale() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let admitted = transport(
        &harness,
        vec![candidate(deployment_a(), harness.live_nonce(POOL_A))],
    );
    harness.advance(POOL_A);
    assert_ne!(
        harness.live_nonce(POOL_A),
        admitted[0].candidate().steps[0].state_nonce,
        "the fixture advanced the pool's nonce"
    );

    let outcome = run_admitted(&registry, admitted, TargetSelection::AllTargets, &harness)
        .expect("the identity is registered");
    assert_eq!(lane(&outcome, deployment_a()).stale_dropped, 1);
    assert_eq!(lane(&outcome, deployment_a()).candidate_count, 0);
}

/// One identity, one transported row: a repeated `CandidateId` is REFUSED
/// before the target-selection walk, and the refusal spends nothing — the
/// retry-due ledger entry is still due afterwards, so no stamping read ran.
#[test]
fn a_repeated_transported_identity_is_refused_without_stamping() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let id = CandidateId::new(7, deployment_a());
    harness.bench(id);
    assert!(
        retry_due(&harness, id, BLOCK),
        "the fixture benches a retry-due candidate"
    );

    let error = run_admitted(
        &registry,
        transport(
            &harness,
            vec![candidate(deployment_a(), 0), candidate(deployment_a(), 0)],
        ),
        TargetSelection::AllTargets,
        &harness,
    )
    .expect_err("one identity may ride the transport once");

    assert_eq!(
        error,
        DispatchRefusal::DuplicateCandidateId {
            candidate_id: id,
            occurrences: 2,
        }
    );
    assert!(
        retry_due(&harness, id, BLOCK),
        "the refusal precedes the stamping read"
    );
}

// -- the shared selection seam --------------------------------------------

/// The FIRST-COMPATIBLE seam, both sides of the pre-selection staleness gate.
///
/// The UNGATED control is the RAW entry: it has no pre-selection gate, so the
/// stale first-target candidate is composed and ACCEPTED by the selection walk,
/// which retires the fresh second-target candidate, and only then does the leaf
/// drop the stale row — nobody simulates. The GATED case is the shared
/// projection, whose gate sits in front of selection.
#[test]
fn a_stale_first_target_candidate_cannot_retire_a_fresh_second_target() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let fresh = harness.live_nonce(POOL_A);
    let stale = fresh + 1;

    // Ungated (the raw entry, which has no pre-selection gate): the stale
    // first-target candidate is composed and ACCEPTED, which retires the fresh
    // second-target candidate; the leaf's admission then drops the stale one and
    // nobody simulates.
    let ungated = run_raw(
        &registry,
        vec![
            candidate(deployment_a(), stale),
            candidate(deployment_b(), fresh),
        ],
        TargetSelection::FirstCompatible,
        &harness,
    )
    .expect("both identities are registered");
    assert_eq!(
        ungated.retired,
        vec![CandidateId::new(7, deployment_b())],
        "the accepted stale target retired the fresh one"
    );
    assert_eq!(lane(&ungated, deployment_a()).stale_dropped, 1);
    assert_eq!(lane(&ungated, deployment_a()).candidate_count, 0);
    assert_eq!(lane(&ungated, deployment_b()).candidate_count, 0);

    // Gated (the shared projection's pre-selection staleness read): the stale
    // first-target candidate never rides the transport, so the fresh
    // second-target candidate is examined and simulated.
    let projection = project(
        &harness,
        vec![
            candidate(deployment_a(), stale),
            candidate(deployment_b(), fresh),
        ],
        MAX_SIMULATE_CONCURRENT,
    );
    assert_eq!(
        projection
            .drops()
            .iter()
            .map(|drop| (drop.reason(), drop.candidate().candidate_id))
            .collect::<Vec<_>>(),
        vec![(EarlyDropReason::Stale, CandidateId::new(7, deployment_a()))],
        "the gate drops the stale first-target row before selection"
    );
    let admitted = projection.into_admitted();
    assert_eq!(admitted.len(), 1, "the gate dropped the stale candidate");

    let gated = run_admitted(
        &registry,
        admitted,
        TargetSelection::FirstCompatible,
        &harness,
    )
    .expect("the identity is registered");
    assert!(
        gated.retired.is_empty(),
        "no candidate was retired: the fresh target was offered the path"
    );
    let simulated = lane(&gated, deployment_b());
    assert_eq!(
        simulated.candidate_count, 1,
        "the fresh candidate simulated"
    );
    assert_eq!(simulated.sim_ok_count(), 1, "and produced a result");
    assert_eq!(simulated.stale_dropped, 0);
    assert_eq!(
        simulated.gas_profitable[0].candidate_id,
        CandidateId::new(7, deployment_b())
    );
    assert_eq!(lane(&gated, deployment_a()).candidate_count, 0);
}

/// The pre-selection staleness gate is part of the SHARED projection, not the
/// caller's: a hop nonce that turned over is reported as `Stale` — provenance a
/// caller maps onto its own record vocabulary — and never transported.
#[test]
fn the_projection_reports_a_stale_row_and_transports_only_the_fresh_one() {
    let harness = Harness::new();
    let fresh = harness.live_nonce(POOL_A);
    let projection = project(
        &harness,
        vec![
            candidate(deployment_a(), fresh + 1), // the state moved since solve
            candidate(deployment_b(), fresh),     // still current
        ],
        MAX_SIMULATE_CONCURRENT,
    );

    assert_eq!(
        projection
            .admitted()
            .iter()
            .map(|admitted| admitted.candidate().candidate_id)
            .collect::<Vec<_>>(),
        vec![CandidateId::new(7, deployment_b())],
        "only the fresh row rides"
    );
    assert_eq!(projection.drops().len(), 1);
    let drop = &projection.drops()[0];
    assert_eq!(drop.reason(), EarlyDropReason::Stale);
    assert_eq!(
        drop.candidate().candidate_id,
        CandidateId::new(7, deployment_a())
    );
    assert_eq!(
        drop.candidate().steps[0].state_nonce,
        fresh + 1,
        "the report carries the whole row, snapshot included"
    );
}

/// The caller's cap runs BEFORE the staleness gate, in input order: with cap 1
/// over `[stale, fresh]` the STALE row keeps the slot, the fresh row is the
/// cap's `OverCap` drop, and the gate then empties the transport — the stale row
/// frees no slot and earns no record.
#[test]
fn the_caller_cap_runs_before_the_staleness_gate() {
    let harness = Harness::new();
    let fresh = harness.live_nonce(POOL_A);
    let projection = project(
        &harness,
        vec![
            candidate(deployment_a(), fresh + 1), // stale, keeps the input-order slot
            candidate(deployment_b(), fresh),     // fresh, past the cap
        ],
        1,
    );

    assert!(
        projection.admitted().is_empty(),
        "nobody rides: the cap's one slot went to the stale row"
    );
    assert_eq!(
        projection
            .drops()
            .iter()
            .map(|drop| (drop.reason(), drop.candidate().candidate_id.deployment_id()))
            .collect::<Vec<_>>(),
        vec![
            (EarlyDropReason::OverCap, deployment_b()),
            (EarlyDropReason::Stale, deployment_a()),
        ],
        "the cap reports its input-order tail first, the gate runs after it"
    );
}

/// A clean transported candidate reaches simulation through the shared body, and
/// the survival counters agree with it.
#[test]
fn a_clean_transported_candidate_simulates_and_tallies() {
    let registry = two_target_registry();
    let harness = Harness::new();
    let outcome = run_admitted(
        &registry,
        transport(
            &harness,
            vec![candidate(deployment_a(), harness.live_nonce(POOL_A))],
        ),
        TargetSelection::AllTargets,
        &harness,
    )
    .expect("the identity is registered");

    let simulated = lane(&outcome, deployment_a());
    assert_eq!(simulated.candidate_count, 1);
    assert_eq!(simulated.sim_ok_count(), 1);
    assert_eq!(simulated.fail_count, 0);
    assert_eq!(simulated.exception_count, 0);
    assert_eq!(simulated.thin_dropped, 0);
    assert!(outcome.retired.is_empty());
}
