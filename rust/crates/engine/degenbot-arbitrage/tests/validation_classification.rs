#![expect(clippy::expect_used)]

//! Declared-interface validation through the production fan-out (ADR-100 D5).
//!
//! One classification decision, proven where it matters: a transaction that
//! violates the interface its adapter declared is a **hard** encode failure —
//! the target-selection walk stops for that candidate, the simulation records
//! the pre-encode exception, and no submission row exists. A capability decline
//! keeps its own, untouched behavior: it advances the walk to the next target.

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
    dispatch_across_deployments, DeploymentFanOutOutcome, DeploymentRegistry, DispatchCandidate,
    DispatchRefusal, FanOutInputs, PoolDivergence, SimulationBlock, SimulationDeployment,
    SolveStep, TargetSelection,
};
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, ContractInterface, DeploymentId,
    ExecutionAdapter, ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpecs,
    TransactionRequest, V2HopInfo, ValidatedAdapter, ValuePolicy,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");
const TARGET_A: Address = address!("0x00000000000000000000000000000000000000a1");
const TARGET_B: Address = address!("0x00000000000000000000000000000000000000b2");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;

const GROSS_A: U256 = U256::from_limbs([9_000_000_000_000_000, 0, 0, 0]);
const GROSS_B: U256 = U256::from_limbs([7_000_000_000_000_000, 0, 0, 0]);

fn deployment_a() -> DeploymentId {
    DeploymentId::new(2)
}

fn deployment_b() -> DeploymentId {
    DeploymentId::new(3)
}

fn settle_selector() -> [u8; 4] {
    [0xb1, 0xb2, 0xb3, 0xb4]
}

/// Target A's own selector — deliberately NOT the one its declaration names.
fn undeclared_selector() -> [u8; 4] {
    [0xa1, 0xa2, 0xa3, 0xa4]
}

fn calls_arc() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// What one target's adapter answers its encode callback with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Encode `selector || optimal_input`.
    Accept([u8; 4]),
    /// Decline for want of capability.
    Decline,
}

/// One foreign target's adapter: its own ABI, its own native `value`, its own
/// gross figure, its own answer, and a call counter the assertions read.
struct TargetAdapter {
    target: Address,
    answer: Answer,
    gross: U256,
    calls: Arc<AtomicUsize>,
}

impl TargetAdapter {
    fn new(target: Address, answer: Answer, gross: U256, calls: &Arc<AtomicUsize>) -> Self {
        Self {
            target,
            answer,
            gross,
            calls: Arc::clone(calls),
        }
    }
}

impl ExecutionAdapter for TargetAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.answer {
            Answer::Decline => Err(ComposeError::unsupported(
                "this contract has no producer for this path",
            )),
            Answer::Accept(selector) => {
                let mut data = Vec::with_capacity(36);
                data.extend_from_slice(&selector);
                data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
                Ok(TransactionRequest::call(self.target, Bytes::from(data)))
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
            &FeePolicy {
                market_percentile: false,
                override_priority_fee: 0,
            },
        ))
    }
}

/// The interface target A declares: its contract's real surface, a zero native
/// value, at its own address.
fn declared_interface(target: Address) -> ContractInterface {
    ContractInterface::from_signatures(["settle(uint256)"])
        .expect("the target's surface is a usable declaration")
        .with_target(target)
        .expect("a nonzero declared target")
        .with_value_policy(ValuePolicy::Zero)
}

/// A foreign deployment with no settlement identity and no declared code (the
/// fixture's chain executes nothing, and the adapter's own Assess prices it).
fn deployment(
    id: DeploymentId,
    adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
    target: Address,
) -> Arc<SimulationDeployment> {
    Arc::new(SimulationDeployment::new(
        id, adapter, OWNER, target, GAS_LIMIT, None, None,
    ))
}

/// A two-target registry: target A's adapter is validated against its own
/// declaration, target B is a plain foreign adapter encoding `settle(uint256)`.
fn two_target_registry(
    answer: Answer,
    calls_a: &Arc<AtomicUsize>,
    calls_b: &Arc<AtomicUsize>,
) -> DeploymentRegistry {
    let validating = ValidatedAdapter::new(
        TargetAdapter::new(TARGET_A, answer, GROSS_A, calls_a),
        declared_interface(TARGET_A),
    )
    .expect("the validated target's declarations agree");

    DeploymentRegistry::new(vec![
        deployment(deployment_a(), Arc::new(validating), TARGET_A),
        deployment(
            deployment_b(),
            Arc::new(TargetAdapter::new(
                TARGET_B,
                Answer::Accept(settle_selector()),
                GROSS_B,
                calls_b,
            )),
            TARGET_B,
        ),
    ])
    .expect("two distinct identities compose a valid registry")
}

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

/// A transport that serves empty accounts plus a block hash: no deployed
/// bytecode is needed, because a call to an account with no code succeeds with
/// empty return data and the adapter's own Assess prices the result.
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

fn run_fanout(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
    selection: TargetSelection,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    run_fanout_with_subscriber(registry, candidates, selection, None)
}

fn run_fanout_with_subscriber(
    registry: &DeploymentRegistry,
    candidates: Vec<DispatchCandidate>,
    selection: TargetSelection,
    subscriber: Option<&tracing::Dispatch>,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let block = SimulationBlock {
        provider: &provider,
        base_fee_next: BASE_FEE,
        current_block: BLOCK,
        block_timestamp: 0,
        block_priority_fees: None,
    };
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _guard = subscriber.map(tracing::dispatcher::set_default);
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(degenbot_arbitrage::FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                let inputs = FanOutInputs {
                    deployments: registry,
                    block: &block,
                    current_block: BLOCK,
                    target_selection: selection,
                    path_suppression: &suppression,
                    pool_divergence: &divergence,
                    fot_registry: &fot,
                    bot_state: Some(&bot_state),
                    warm_cache: Some(&warm_cache),
                };
                runtime.block_on(async {
                    dispatch_across_deployments(candidates, inputs.with_thin_margin(0))
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// A validated target whose encoder emits a selector its own declaration does
/// not name, plus a healthy second target: the walk must stop at the defect.
#[test]
fn a_validation_failure_is_a_hard_encode_failure_not_a_capability_decline() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = two_target_registry(Answer::Accept(undeclared_selector()), &calls_a, &calls_b);

    // The ordered walk examines target A, sees a hard failure, and stops: the
    // violation is `PreparedEncoding::Failed`, never `Declined`.
    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        calls_b.load(Ordering::SeqCst),
        0,
        "a validation violation is not evidence the next target fares better"
    );
    assert_eq!(
        outcome.retired,
        vec![CandidateId::new(7, deployment_b())],
        "the walk stopped at the violating target"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert_eq!(
        lane_a.exception_count,
        1,
        "a validation violation is the candidate's own exception: buckets {:?}",
        lane_a.fail_buckets.buckets()
    );
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        None,
        "the violation is never folded into the routine capability bucket"
    );
    assert!(
        lane_a.gas_profitable.is_empty() && lane_a.gas_unprofitable.is_empty(),
        "no submission row exists for a violating candidate"
    );

    // The explicit fan-out composes in the simulation leaf (no selection walk),
    // so the same classification is proven on the other entry point.
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = two_target_registry(Answer::Accept(undeclared_selector()), &calls_a, &calls_b);

    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::AllTargets,
    )
    .expect("every candidate names a registered deployment");

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert_eq!(lane_a.exception_count, 1);
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        None
    );
    assert!(lane_a.gas_profitable.is_empty());
    assert_eq!(
        calls_b.load(Ordering::SeqCst),
        1,
        "the independent target still runs its own candidate"
    );
    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(
        lane_b.gas_profitable.len(),
        1,
        "the healthy target still produces its own result: buckets {:?}",
        lane_b.fail_buckets.buckets()
    );
}

#[test]
fn a_hard_encode_exception_preserves_its_path_and_cause_in_the_sim_log() {
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    static SECOND_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND_DISPATCH
        .get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));

    let buffer = Buffer::default();
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(buffer.clone())
            .finish(),
    );
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = two_target_registry(Answer::Accept(undeclared_selector()), &calls_a, &calls_b);
    let outcome = run_fanout_with_subscriber(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::AllTargets,
        Some(&subscriber),
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        outcome
            .lane(deployment_a())
            .expect("lane A")
            .exception_count,
        1
    );
    let text = String::from_utf8_lossy(&buffer.0.lock().expect("log buffer")).into_owned();
    assert!(
        text.contains("candidate simulation raised an exception"),
        "{text}"
    );
    assert!(text.contains("path_id=7"), "{text}");
    assert!(text.contains("is not declared by the interface"), "{text}");
}

/// The untouched contrast case: `ComposeError::Unsupported` under a declared
/// interface is still the routine capability decline — the walk advances, the
/// next target runs, and the declining candidate keeps its `encode-failed`
/// bucket rather than becoming an exception.
#[test]
fn a_capability_decline_still_advances_to_the_next_target() {
    let calls_a = calls_arc();
    let calls_b = calls_arc();
    let registry = two_target_registry(Answer::Decline, &calls_a, &calls_b);

    let outcome = run_fanout(
        &registry,
        vec![candidate(deployment_a()), candidate(deployment_b())],
        TargetSelection::FirstCompatible,
    )
    .expect("every candidate names a registered deployment");

    assert_eq!(
        calls_a.load(Ordering::SeqCst),
        1,
        "the declining target composed once"
    );
    assert_eq!(
        calls_b.load(Ordering::SeqCst),
        1,
        "a capability decline still advances the walk"
    );
    assert!(
        outcome.retired.is_empty(),
        "nothing is retired when a decline advances the walk"
    );

    let lane_a = outcome.lane(deployment_a()).expect("lane A");
    assert_eq!(
        lane_a.exception_count, 0,
        "a decline is not an exception, declared interface or not"
    );
    assert_eq!(
        lane_a.fail_buckets.buckets().get("encode-failed").copied(),
        Some(1),
        "the decline keeps its own bucket: {:?}",
        lane_a.fail_buckets.buckets()
    );

    let lane_b = outcome.lane(deployment_b()).expect("lane B");
    assert_eq!(lane_b.gas_profitable.len(), 1);
    assert_eq!(lane_b.gas_profitable[0].gross_profit, GROSS_B);
    assert_eq!(
        &lane_b.gas_profitable[0].execute_calldata[..4],
        &settle_selector()
    );
}
