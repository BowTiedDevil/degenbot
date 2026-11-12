#![expect(clippy::expect_used)]

//! The `CandidateId` identity contract, exercised from an EXTERNAL crate.
//!
//! One identity is one row (see `degenbot_arbitrage::identity`). Every dispatch
//! entry refuses a set that repeats an identity BEFORE it stamps a ledger,
//! records a tally, composes an encoding, or simulates — and it names the
//! identity it refused. This target is an external consumer: a name, method, or
//! re-export that stops resolving is a compile error here rather than a silent
//! break for a `cargo add degenbot-arbitrage` caller.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    dispatch_across_deployments, dispatch_admitted_across_deployments, dispatch_profitable_results,
    project_early_admission, AdmittedCandidate, DeploymentFanOutOutcome, DeploymentRegistry,
    DispatchCandidate, DispatchRefusal, EarlyAdmissionPolicy, FeeOnTransferRegistry,
    PoolDivergence, SimulateContext, SimulationBlock, SimulationDeployment, SolveStep,
    TargetSelection, MAX_SIMULATE_CONCURRENT,
};
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpecs, TransactionRequest,
    V2HopInfo,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{PathSuppression, PATH_SUPPRESS_RETRY_INTERVAL, PATH_SUPPRESS_THRESHOLD};
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");
const TARGET: Address = address!("0x00000000000000000000000000000000000000a1");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const OTHER: Address = address!("0x3333333333333333333333333333333333333333");

/// The fixture pool — one clean V2 pair.
const POOL: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;

/// The block a benched candidate is retry-DUE at: `is_suppressed` spends the
/// retry interval on its stamping read, so the refusal must leave it unspent.
const DUE_BLOCK: u64 = PATH_SUPPRESS_RETRY_INTERVAL;

fn deployment_id() -> DeploymentId {
    DeploymentId::new(1)
}

/// The settled path every target is offered: one V2 hop through `POOL`.
fn v2_path() -> PathInfo {
    PathInfo::new(vec![degenbot_execution::HopInfo::V2(V2HopInfo {
        pool_address: POOL,
        token0_address: WETH,
        token1_address: OTHER,
        fee: 30,
        zfo: true,
    })])
}

/// One solved candidate at `deployment`.
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

/// A target whose encode callback counts its calls: a refusal that happens
/// BEFORE composition leaves the counter at zero.
struct CountingAdapter {
    target: Address,
    calls: Arc<AtomicUsize>,
}

impl ExecutionAdapter for CountingAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut data = vec![0xa1, 0xa2, 0xa3, 0xa4];
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
            U256::from(9_000_000_000_000_000u64),
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

/// A transport that serves empty accounts plus a block hash.
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

/// One deployment whose adapter counts the encodings the walk asks it for.
fn counted_registry(calls: &Arc<AtomicUsize>) -> DeploymentRegistry {
    let deployment = Arc::new(SimulationDeployment::new(
        deployment_id(),
        Arc::new(CountingAdapter {
            target: TARGET,
            calls: Arc::clone(calls),
        }),
        OWNER,
        TARGET,
        GAS_LIMIT,
        None,
        None,
    ));
    DeploymentRegistry::new(vec![deployment]).expect("one distinct identity composes a registry")
}

/// One session's live policy state — the arcs both entries read and stamp.
struct Harness {
    suppression: Arc<Mutex<PathSuppression>>,
    divergence: Arc<Mutex<PoolDivergence>>,
    fot: Arc<Mutex<FeeOnTransferRegistry>>,
}

impl Harness {
    fn new() -> Self {
        Self {
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
        }
    }

    /// Bench `candidate_id`: `PATH_SUPPRESS_THRESHOLD` consecutive failures put
    /// it in the suppressed set with no retry stamp yet.
    fn bench(&self, candidate_id: CandidateId) {
        let mut suppression = self.suppression.lock().expect("suppression ledger");
        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(candidate_id);
        }
    }

    /// Is the candidate's retry interval still UNSPENT at `block`? (The
    /// stamping read spends it; the non-stamping read does not.)
    fn retry_still_due(&self, candidate_id: CandidateId, block: u64) -> bool {
        !self
            .suppression
            .lock()
            .expect("suppression ledger")
            .is_suppressed_without_stamp(candidate_id, block)
    }
}

/// The refusal a duplicate-carrying candidate set earns, unwrapped.
fn refusal_of(result: Result<DeploymentFanOutOutcome, DispatchRefusal>) -> DispatchRefusal {
    result.expect_err("the entry refuses a repeated candidate identity")
}

/// Project raw rows through the shared EARLY projection — the one public way a
/// caller obtains the pre-admitted transport, since the unchecked constructor is
/// crate-private. No state owner here: this fixture exercises the entry's own
/// identity refusal, not the projection's staleness gate.
fn transport(harness: &Harness, candidates: Vec<DispatchCandidate>) -> Vec<AdmittedCandidate> {
    let suppression = harness.suppression.lock().expect("suppression ledger");
    project_early_admission(
        candidates,
        &EarlyAdmissionPolicy {
            suppression: &suppression,
            divergence: &harness.divergence,
            fee_on_transfer: &harness.fot,
            bot_state: None,
            current_block: BLOCK,
            min_profit_margin_bps: 0,
            max_candidates: MAX_SIMULATE_CONCURRENT,
        },
    )
    .into_admitted()
}

/// Run one registry fan-out on a dedicated multi-thread runtime (the engine's
/// sync DB bridge requires the ambient runtime).
fn run_fan_out(
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
    let state: Option<&Arc<StateLock<BotState>>> = None;
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
                        state,
                        None,
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// Run one PRE-ADMITTED registry fan-out on the same runtime shape.
fn run_admitted_fan_out(
    registry: &DeploymentRegistry,
    admitted: Vec<AdmittedCandidate>,
    harness: &Harness,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let block = block(&provider);
    let state: Option<&Arc<StateLock<BotState>>> = None;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                runtime.block_on(async {
                    dispatch_admitted_across_deployments(
                        admitted,
                        &block,
                        registry,
                        TargetSelection::AllTargets,
                        &harness.suppression,
                        BLOCK,
                        &harness.divergence,
                        &harness.fot,
                        state,
                        None,
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// Run the ONE-deployment raw leaf. `bot_state: None` is the assertion's lever:
/// the leaf's simulation body `unreachable!()`s on non-empty input without a
/// state owner, so a test that returns a typed refusal instead of panicking
/// proves the refusal happened BEFORE simulation.
fn run_raw_leaf(
    candidates: Vec<DispatchCandidate>,
    harness: &Harness,
    current_block: u64,
) -> Result<degenbot_arbitrage::DispatchOutcome, DispatchRefusal> {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let registry = counted_registry(&Arc::new(AtomicUsize::new(0)));
    let deployment = registry
        .get(deployment_id())
        .expect("the fixture deployment");
    let block = block(&provider);
    let ctx = SimulateContext { block, deployment };
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                runtime.block_on(async {
                    dispatch_profitable_results(
                        candidates,
                        &ctx,
                        &harness.suppression,
                        current_block,
                        0,
                        &harness.divergence,
                        &harness.fot,
                        None,
                        None,
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// Two HEALTHY rows for one identity: the raw leaf refuses them, names the
/// identity and the row count, and spends nothing — no suppression retry stamp,
/// no divergence/fee-on-transfer lifetime tally, no simulation.
#[test]
fn the_raw_leaf_refuses_a_healthy_duplicate_before_simulation() {
    let harness = Harness::new();
    let outcome = run_raw_leaf(
        vec![candidate(deployment_id()), candidate(deployment_id())],
        &harness,
        BLOCK,
    );
    let refusal = outcome.expect_err("two rows for one identity are refused");
    assert_eq!(
        refusal,
        DispatchRefusal::DuplicateCandidateId {
            candidate_id: CandidateId::new(7, deployment_id()),
            occurrences: 2,
        }
    );
    assert_eq!(
        refusal.to_string(),
        "candidate 7@1 arrived on 2 rows: one identity is one row"
    );

    // No policy side effect: the registries were never written.
    assert_eq!(
        harness
            .divergence
            .lock()
            .expect("divergence memo")
            .total_divergent_dropped(),
        0,
        "a refusal records no divergence tally"
    );
    assert_eq!(
        harness
            .fot
            .lock()
            .expect("fot registry")
            .total_fot_dropped(),
        0,
        "a refusal records no fee-on-transfer tally"
    );
    assert_eq!(
        harness
            .suppression
            .lock()
            .expect("suppression ledger")
            .total_suppressed(),
        0,
        "a refusal suppresses nothing"
    );
}

/// Two rows for a RETRY-DUE benched identity: refused exactly as the healthy
/// pair is, and the refusal does not spend the retry interval — the stamping
/// read never ran.
#[test]
fn the_raw_leaf_refuses_a_retry_due_duplicate_without_spending_its_retry() {
    let harness = Harness::new();
    let candidate_id = CandidateId::new(7, deployment_id());
    harness.bench(candidate_id);
    assert!(
        harness.retry_still_due(candidate_id, DUE_BLOCK),
        "the fixture benches a retry-due candidate"
    );

    let refusal = run_raw_leaf(
        vec![candidate(deployment_id()), candidate(deployment_id())],
        &harness,
        DUE_BLOCK,
    )
    .expect_err("two rows for one identity are refused");
    assert_eq!(
        refusal,
        DispatchRefusal::DuplicateCandidateId {
            candidate_id,
            occurrences: 2,
        }
    );
    assert!(
        harness.retry_still_due(candidate_id, DUE_BLOCK),
        "the refusal precedes the stamping read, so the retry interval is unspent"
    );
}

/// The registry RAW entry refuses a repeated identity under BOTH selection
/// policies, and no adapter is ever asked to compose.
#[test]
fn the_registry_fan_out_refuses_duplicates_before_any_composition() {
    for selection in [
        TargetSelection::AllTargets,
        TargetSelection::FirstCompatible,
    ] {
        let harness = Harness::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = counted_registry(&calls);
        let refusal = refusal_of(run_fan_out(
            &registry,
            vec![candidate(deployment_id()), candidate(deployment_id())],
            selection,
            &harness,
        ));
        assert_eq!(
            refusal,
            DispatchRefusal::DuplicateCandidateId {
                candidate_id: CandidateId::new(7, deployment_id()),
                occurrences: 2,
            }
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "{selection:?}: the refusal precedes the target-selection walk"
        );
    }
}

/// The registry PRE-ADMITTED entry refuses the same repeated identity, also
/// before any composition — the transport can carry one candidate per identity.
#[test]
fn the_pre_admitted_fan_out_refuses_duplicates_before_any_composition() {
    let harness = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = counted_registry(&calls);
    let refusal = refusal_of(run_admitted_fan_out(
        &registry,
        transport(
            &harness,
            vec![candidate(deployment_id()), candidate(deployment_id())],
        ),
        &harness,
    ));
    assert_eq!(
        refusal,
        DispatchRefusal::DuplicateCandidateId {
            candidate_id: CandidateId::new(7, deployment_id()),
            occurrences: 2,
        }
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the refusal precedes the target-selection walk"
    );
}

/// An EMPTY candidate set is not a duplicate, not an error, and reaches no
/// adapter: the entry answers one empty lane per registered deployment.
#[test]
fn an_empty_candidate_set_is_not_a_refusal() {
    let harness = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = counted_registry(&calls);
    let outcome = run_fan_out(&registry, Vec::new(), TargetSelection::AllTargets, &harness)
        .expect("an empty set carries no identity, so nothing is refused");
    assert_eq!(outcome.lanes.len(), 1, "one lane per registered deployment");
    assert_eq!(outcome.lanes[0].outcome.candidate_count, 0);
    assert!(outcome.retired.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing was composed");

    let admitted = run_admitted_fan_out(&registry, Vec::new(), &harness)
        .expect("an empty transported set is not refused either");
    assert_eq!(admitted.lanes.len(), 1);
    assert_eq!(admitted.lanes[0].outcome.candidate_count, 0);

    let leaf = run_raw_leaf(Vec::new(), &harness, BLOCK)
        .expect("an empty set is not a duplicate identity");
    assert_eq!(leaf.candidate_count, 0);
}

/// One row per identity — including one PATH at two deployments — is legal
/// input: the public scan answers `None`, so the rule never conflates a path
/// with its post-target candidate identities.
#[test]
fn one_row_per_identity_is_the_legal_input() {
    let other = DeploymentId::new(2);
    assert_eq!(
        degenbot_arbitrage::duplicate_candidate_id([
            candidate(deployment_id()).candidate_id,
            candidate(other).candidate_id,
        ]),
        None
    );
}
