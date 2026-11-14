#![expect(clippy::expect_used)]
// Solidity/EVM identifiers (`PoolManager`, `balanceOf`, `msg.value`) are
// ubiquitous in this crate's prose; match the crate's doc-markdown stance.
#![expect(clippy::doc_markdown)]

//! The canonical production simulation entry over an INJECTED execution
//! deployment (ADR-098 D3/D6).
//!
//! Two seams are pinned here:
//!
//! 1. **Foreign production simulation.** A contract with no WETH, no
//!    PoolManager, no command stream, and no version/release metadata drives
//!    the canonical fan-out (`dispatch_profitable_results`) through its own
//!    Encode, its own declared probes, and its own Assess. The settlement
//!    world is EMPTY: no WETH9, no PoolManager, no Multicall3 code exists in
//!    the fixture, so a path that hardcoded the settlement balance vector
//!    could not produce a profitable result.
//! 2. **Built-in regression.** The built-in `cmd_executor` binding declares no
//!    probe reads (its contract reports its own receipt), still produces the
//!    command-stream bytes for the session's encode policy, still targets the
//!    one deployed executor, and simulates deployed code unless an explicit
//!    declared-code override is injected.

use std::sync::{Arc, Mutex};

use alloy::primitives::{address, keccak256, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    dispatch_profitable_results, CmdExecutorBinding, DispatchCandidate, DispatchOutcome,
    FeeOnTransferRegistry, PoolDivergence, PreparedEncoding, SimulatePath, SimulationBlock,
    SimulationDeployment, SolveStep, MIN_PROFIT_NET,
};
use degenbot_execution::{
    AssessError, ComposeError, ComposeOptions, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpec, ProbeSpecs,
    TransactionRequest, V2HopInfo, ZeroCapitalDeficit,
};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest,
};
use degenbot_executor::compute_simulation_warmup_slots;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");

/// The fixture deployments' operator-assigned identity.
fn test_deployment_id() -> DeploymentId {
    DeploymentId::new(1)
}

/// The lab token the fixture paths cross beside mainnet WETH. Its storage layout
/// is NOT pinned by the production source, so a probe over it must declare one
/// explicitly ([`lab_layouts`]) — that is the point of the layout source.
const LAB_TOKEN: Address = address!("0x3333333333333333333333333333333333333333");

/// The layouts the fixture declares explicitly: the lab token's `balances` base
/// slot, on top of the pinned mainnet set.
fn lab_layouts() -> degenbot_simulation::CapitalLayouts {
    degenbot_simulation::CapitalLayouts::verified().with_token(LAB_TOKEN, 5)
}

/// One fan-out's path value for the fixture path: the amounts the solver fixed
/// for it, which the capital-requirement intake reads beside the hops.
fn sim_path(optimal_input: u128) -> SimulatePath {
    SimulatePath {
        candidate_id: degenbot_execution::CandidateId::new(1, test_deployment_id()),
        optimal_input,
        steps: vec![
            SolveStep {
                output: PREPAY_U128,
                consumed_input: PREPAY_U128,
                state_nonce: 0,
            },
            SolveStep {
                output: PREPAY_U128,
                consumed_input: PREPAY_U128,
                state_nonce: 0,
            },
        ]
        .into_boxed_slice(),
        path_info: two_v2_hops(),
        solve_block: BLOCK,
    }
}
const FOREIGN: Address = address!("0x2222222222222222222222222222222222222222");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");
const EXECUTOR: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;

/// The foreign contract's prepayment (`msg.value`).
const PREPAY: U256 = U256::from_limbs([5_000_000_000_000_000, 0, 0, 0]);
const PREPAY_U128: u128 = 5_000_000_000_000_000;

/// The foreign adapter's own priority fee — deliberately orders of magnitude
/// below any market-aware sizing the settlement strategy would derive for this
/// gross, so a re-priced result is unmistakable.
const FOREIGN_FEE: u128 = 7_777;

/// A two-pass mini assembler so a fixture edit cannot silently skew a jump.
enum Asm<'a> {
    Bytes(&'a [u8]),
    PushLabel(&'static str),
    Label(&'static str),
}

fn assemble(items: &[Asm<'_>]) -> Vec<u8> {
    let mut code = Vec::new();
    let mut labels: Vec<(&str, usize)> = Vec::new();
    let mut fixups: Vec<(usize, &str)> = Vec::new();
    for item in items {
        match item {
            Asm::Bytes(bytes) => code.extend_from_slice(bytes),
            Asm::PushLabel(target) => {
                code.push(0x60);
                fixups.push((code.len(), target));
                code.push(0x00);
            }
            Asm::Label(name) => {
                labels.push((name, code.len()));
                code.push(0x5b);
            }
        }
    }
    for (at, target) in fixups {
        let offset = labels
            .iter()
            .find(|(name, _)| name == &target)
            .expect("the fixture defines every label")
            .1;
        code[at] = u8::try_from(offset).expect("the fixture's offsets fit one byte");
    }
    code
}

fn topic_booked() -> [u8; 32] {
    keccak256(b"Booked(uint256)").0
}

/// The foreign contract: 4-byte calldata is the declared read (it returns the
/// amount it booked); any other calldata is the prepayment, which books
/// `msg.value` in slot 0, emits `Booked(msg.value)`, and returns it.
fn foreign_code() -> Vec<u8> {
    let mut push_topic = Vec::with_capacity(33);
    push_topic.push(0x7f); // PUSH32
    push_topic.extend_from_slice(&topic_booked());
    assemble(&[
        Asm::Bytes(&[0x36, 0x60, 0x04, 0x14]), // CALLDATASIZE PUSH1 4 EQ
        Asm::PushLabel("read"),
        Asm::Bytes(&[0x57]), // JUMPI
        Asm::PushLabel("prepay"),
        Asm::Bytes(&[0x56]), // JUMP
        Asm::Label("read"),
        Asm::Bytes(&[0x60, 0x00, 0x54]),             // PUSH1 0 SLOAD
        Asm::Bytes(&[0x60, 0x00, 0x52]),             // PUSH1 0 MSTORE
        Asm::Bytes(&[0x60, 0x20, 0x60, 0x00, 0xf3]), // RETURN mem[0..32]
        Asm::Label("prepay"),
        Asm::Bytes(&[0x34, 0x60, 0x00, 0x55]), // CALLVALUE PUSH1 0 SSTORE
        Asm::Bytes(&[0x34, 0x60, 0x00, 0x52]), // CALLVALUE PUSH1 0 MSTORE
        Asm::Bytes(&push_topic),               // PUSH32 topic
        Asm::Bytes(&[0x60, 0x20, 0x60, 0x00, 0xa1]), // LOG1 mem[0..32]
        Asm::Bytes(&[0x34, 0x60, 0x00, 0x52]), // CALLVALUE PUSH1 0 MSTORE
        Asm::Bytes(&[0x60, 0x20, 0x60, 0x00, 0xf3]), // RETURN mem[0..32]
    ])
}

fn prepay_selector() -> [u8; 4] {
    let digest = keccak256(b"prepay(uint256)");
    [digest[0], digest[1], digest[2], digest[3]]
}

fn booked_selector() -> [u8; 4] {
    let digest = keccak256(b"booked()");
    [digest[0], digest[1], digest[2], digest[3]]
}

/// A transport that serves empty accounts (zero balance/nonce, no code, zero
/// storage) plus a block hash — every settlement address is code-less, so the
/// built-in balance vector could read nothing.
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

/// A transport that COUNTS every request it is asked to serve (and answers with
/// the same empty chain `EmptyTransport` serves). The counting is the point: a
/// probe refused at capital-plan composition must issue NO request at all, and a
/// counter proves it without inferring anything from an outcome.
#[derive(Clone, Default)]
struct CountingTransport {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl CountingTransport {
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl tower::Service<RequestPacket> for CountingTransport {
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
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut empty = EmptyTransport;
        tower::Service::call(&mut empty, req)
    }
}

/// A transport that RECORDS every method it is asked to serve (and answers with
/// the same empty chain `EmptyTransport` serves). The log is the point: a fan-out
/// that executes no candidate must issue no EVM-executing request at all, and a
/// method log proves it without inferring anything from an outcome.
#[derive(Clone, Default)]
struct RecordingTransport {
    methods: Arc<Mutex<Vec<String>>>,
}

impl RecordingTransport {
    fn methods(&self) -> Vec<String> {
        self.methods
            .lock()
            .expect("the method log is never poisoned")
            .clone()
    }
}

impl tower::Service<RequestPacket> for RecordingTransport {
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
        {
            let mut methods = self
                .methods
                .lock()
                .expect("the method log is never poisoned");
            match &req {
                RequestPacket::Single(request) => methods.push(request.method().to_string()),
                RequestPacket::Batch(requests) => {
                    methods.extend(requests.iter().map(|request| request.method().to_string()));
                }
            }
        }
        let mut empty = EmptyTransport;
        tower::Service::call(&mut empty, req)
    }
}

/// Whether `method` is a provider request that would EXECUTE EVM code or submit a
/// transaction. Account, storage, block, and code reads are not: they are how a
/// fan-out builds its view of the chain.
fn executes_evm(method: &str) -> bool {
    matches!(
        method,
        "eth_call"
            | "eth_callMany"
            | "eth_estimateGas"
            | "eth_sendRawTransaction"
            | "eth_sendTransaction"
            | "eth_simulateV1"
    ) || method.starts_with("debug_")
}

fn mock_provider() -> AlloyProvider {
    let client = RpcClient::new(EmptyTransport, true);
    let root: RootProvider = RootProvider::new(client);
    AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    )
}

/// The foreign adapter: its own Encode (one call to its own contract carrying
/// its own `prepay(uint256)` ABI), its own declared probes (a storage read plus
/// its contract's native balance), and its own Assess (the emitted `Booked`
/// event, cross-checked against the declared storage probe).
struct ForeignAdapter {
    probes: ProbeSpecs,
    /// The adapter's OWN Fee policy — the pricing half of its Assess.
    fee_policy: FeePolicy,
    /// The adapter's OWN pass threshold (wei of net profit).
    min_net_profit: U256,
}

impl ForeignAdapter {
    fn new() -> Self {
        Self::with_pricing(FeePolicy::default(), U256::ZERO)
    }

    fn with_pricing(fee_policy: FeePolicy, min_net_profit: U256) -> Self {
        Self {
            probes: vec![
                ProbeSpec::call(
                    "BOOKED",
                    FOREIGN,
                    Bytes::copy_from_slice(&booked_selector()),
                ),
                ProbeSpec::native_balance("ETH_FOREIGN", FOREIGN),
            ],
            fee_policy,
            min_net_profit,
        }
    }
}

impl ExecutionAdapter for ForeignAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(&prepay_selector());
        data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
        Ok(TransactionRequest::new(FOREIGN, Bytes::from(data), PREPAY))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let event = observation
            .events
            .iter()
            .find(|event| {
                event.topics.first() == Some(&alloy::primitives::B256::from(topic_booked()))
            })
            .ok_or_else(|| AssessError::evidence("no Booked event was captured"))?;
        let data = event
            .data
            .get(..32)
            .ok_or_else(|| AssessError::evidence("Booked event data is not one word"))?;
        let booked = U256::from_be_slice(data);
        // The declared storage probe must have observed the same movement —
        // a reading that never ran (or read the wrong account) is absent
        // evidence, never a fabricated delta.
        let probe = observation
            .probe_reads
            .iter()
            .find(|reading| reading.spec.label() == "BOOKED")
            .ok_or_else(|| AssessError::evidence("the BOOKED probe is not declared"))?;
        let delta = probe
            .delta()
            .ok_or_else(|| AssessError::evidence("the BOOKED probe has no complete reading"))?;
        if U256::from(delta.unsigned_abs()) != booked {
            return Err(AssessError::evidence(
                "the BOOKED probe disagrees with the emitted event",
            ));
        }
        Ok(ExecutionResult::price(
            booked,
            Vec::new(),
            observation,
            self.min_net_profit,
            &self.fee_policy,
        ))
    }
}

fn two_v2_hops() -> PathInfo {
    PathInfo::new(
        vec![
            V2HopInfo {
                pool_address: address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                token0_address: WETH,
                token1_address: address!("0x3333333333333333333333333333333333333333"),
                fee: 30,
                zfo: true,
            },
            V2HopInfo {
                pool_address: address!("0xcccccccccccccccccccccccccccccccccccccccc"),
                token0_address: address!("0x3333333333333333333333333333333333333333"),
                token1_address: WETH,
                fee: 30,
                zfo: true,
            },
        ]
        .into_iter()
        .map(degenbot_execution::HopInfo::V2)
        .collect::<Vec<_>>(),
    )
}

/// The same round trip over the PINNED mainnet pair (WETH ↔ USDC): every asset it
/// touches has a verified layout, so a probe posture can compose its plan.
fn weth_usdc_v2_hops() -> PathInfo {
    let usdc = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    PathInfo::new(
        vec![
            V2HopInfo {
                pool_address: address!("0xdddddddddddddddddddddddddddddddddddddddd"),
                token0_address: WETH,
                token1_address: usdc,
                fee: 30,
                zfo: true,
            },
            V2HopInfo {
                pool_address: address!("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
                token0_address: usdc,
                token1_address: WETH,
                fee: 30,
                zfo: true,
            },
        ]
        .into_iter()
        .map(degenbot_execution::HopInfo::V2)
        .collect::<Vec<_>>(),
    )
}

fn candidate() -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: degenbot_execution::CandidateId::new(7, test_deployment_id()),
        optimal_input: PREPAY_U128,
        engine_profit: 1,
        steps: Box::new([
            SolveStep {
                output: PREPAY_U128,
                consumed_input: PREPAY_U128,
                state_nonce: 0,
            },
            SolveStep {
                output: PREPAY_U128,
                consumed_input: PREPAY_U128,
                state_nonce: 0,
            },
        ]),
        solve_block: BLOCK,
        path_info: two_v2_hops(),
        prepared: None,
    }
}

/// A foreign deployment declares NO settlement identity, and its contract is an
/// explicit declared-code override (the fixture's chain holds no such code).
fn foreign_deployment() -> SimulationDeployment {
    foreign_deployment_with(ForeignAdapter::new())
}

/// The same foreign deployment over an adapter with its own pricing policy.
fn foreign_deployment_with(adapter: ForeignAdapter) -> SimulationDeployment {
    SimulationDeployment::new(
        test_deployment_id(),
        Arc::new(adapter),
        OWNER,
        FOREIGN,
        GAS_LIMIT,
        Some(degenbot_simulation::DeclaredCode {
            address: FOREIGN,
            runtime: Bytes::from(foreign_code()),
        }),
        None,
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

/// Drive the canonical fan-out over one injected deployment (one candidate).
fn run_fanout(deployment: &SimulationDeployment) -> DispatchOutcome {
    let provider = mock_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let pool_divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                let ctx = degenbot_arbitrage::SimulateContext {
                    block: block(&provider),
                    deployment,
                };
                runtime.block_on(async {
                    dispatch_profitable_results(
                        vec![candidate()],
                        &ctx,
                        &suppression,
                        BLOCK,
                        0,
                        &pool_divergence,
                        &fot,
                        Some(bot_state),
                        Some(warm_cache),
                    )
                    .expect("the fixture carries one row per candidate identity")
                })
            })
            .join()
            .expect("fan-out thread")
    })
}

/// Pins: the canonical production fan-out drives an INJECTED foreign deployment
/// through its own transaction, its own probes, and its own Assess — with no
/// WETH, PoolManager, Multicall3, command stream, or version metadata anywhere
/// in the picture.
#[test]
fn the_canonical_fanout_simulates_a_foreign_deployment_through_its_own_adapter() {
    let deployment = foreign_deployment();
    assert!(
        deployment.settlement().is_none(),
        "a foreign deployment declares no settlement identity"
    );
    assert_eq!(deployment.owner(), OWNER);
    assert_eq!(deployment.gas_limit(), GAS_LIMIT);
    assert_eq!(
        deployment.code().map(|code| code.address),
        Some(FOREIGN),
        "the fixture's contract is an explicit declared-code override"
    );

    let outcome = run_fanout(&deployment);

    assert_eq!(outcome.candidate_count, 1);
    assert_eq!(outcome.exception_count, 0);
    assert_eq!(
        outcome.fail_count,
        0,
        "the foreign opportunity simulated: buckets {:?}",
        outcome.fail_buckets.buckets()
    );
    assert_eq!(outcome.gas_profitable.len(), 1);
    let result = &outcome.gas_profitable[0];

    assert_eq!(
        result.gross_profit, PREPAY,
        "the gross is the foreign adapter's OWN evidence (its Posted event, \
         cross-checked against its declared storage probe)"
    );
    assert!(
        result.net_profit < result.gross_profit,
        "the adapter's own Fee policy priced the observed gas above the gross"
    );
    assert_eq!(
        result.priority_fee,
        FeePolicy::default().priority_fee(),
        "the priority fee is the foreign adapter's own Fee decision"
    );
    assert_eq!(result.candidate_id.path_id(), 7);
    assert_eq!(
        result.candidate_id.deployment_id(),
        test_deployment_id(),
        "the result carries the deployment that produced it"
    );

    // Its own transaction: target + calldata + native value.
    assert_eq!(result.execute_calldata.len(), 4 + 32);
    assert_eq!(&result.execute_calldata[..4], &prepay_selector());
    // Single-target: the deployment's adapter names its own contract.
    let transaction = ForeignAdapter::new()
        .encode(
            &PathInfo::new(Vec::new()),
            &ComposerInputs {
                optimal_input: PREPAY_U128,
                hop_outputs: &[],
                consumed_inputs: &[],
                opts: ComposeOptions,
            },
        )
        .expect("the foreign adapter encodes");
    assert_eq!(transaction.to, FOREIGN);
    assert_eq!(transaction.value, PREPAY);
}

/// Pins ADR-025 D2 / ADR-098 D3: Fee is part of the adapter's Assess. A foreign
/// deployment's own priority fee and threshold reach the result unchanged —
/// the settlement strategy does not re-price a deployment that never opted
/// into settlement pricing.
#[test]
fn a_foreign_deployment_keeps_its_own_priority_fee_and_net() {
    const FOREIGN_MIN_NET: u128 = 1_000_000_000_000_000;
    let deployment = foreign_deployment_with(ForeignAdapter::with_pricing(
        FeePolicy {
            market_percentile: false,
            override_priority_fee: FOREIGN_FEE,
        },
        U256::from(FOREIGN_MIN_NET),
    ));

    let outcome = run_fanout(&deployment);
    assert_eq!(outcome.exception_count, 0);
    assert_eq!(
        outcome.fail_count,
        0,
        "buckets {:?}",
        outcome.fail_buckets.buckets()
    );
    assert_eq!(outcome.gas_profitable.len(), 1);
    let result = &outcome.gas_profitable[0];

    assert_eq!(result.gross_profit, PREPAY);
    assert_eq!(
        result.priority_fee, FOREIGN_FEE,
        "the adapter's own priority fee survives — a settlement market fee for          this gross is orders of magnitude larger"
    );
    let gas_cost = U256::from(result.gas_used) * (U256::from(BASE_FEE) + U256::from(FOREIGN_FEE));
    assert_eq!(
        result.net_profit,
        result.gross_profit - gas_cost,
        "the net is the adapter's own gross minus its own gas cost"
    );
    assert!(
        result.net_profit >= U256::from(FOREIGN_MIN_NET),
        "the adapter's own threshold passed"
    );
    assert!(
        result.passed,
        "the adapter's own verdict reached the result"
    );
    assert!(
        result.is_submittable(),
        "a passed, above-floor result submits"
    );
}

/// Pins the verdict half of ADR-098 D3/D7: the deployment's final Assess verdict
/// travels through the result into dispatch. A failed assessment with positive
/// gross AND positive net is never gas-profitable (never a submit candidate);
/// its gross-positive result stays visible as a diagnostic row.
#[test]
fn a_failed_foreign_assessment_is_never_gas_profitable() {
    let deployment = foreign_deployment_with(ForeignAdapter::with_pricing(
        FeePolicy {
            market_percentile: false,
            override_priority_fee: 0,
        },
        U256::MAX,
    ));

    let outcome = run_fanout(&deployment);
    assert_eq!(outcome.exception_count, 0);
    assert_eq!(
        outcome.gas_profitable.len(),
        0,
        "a failed assessment never reaches the profitable/submission bucket"
    );
    assert_eq!(
        outcome.gas_unprofitable.len(),
        1,
        "the gross-positive failed assessment stays visible as a diagnostic"
    );
    let result = &outcome.gas_unprofitable[0];
    assert_eq!(
        result.gross_profit, PREPAY,
        "the adapter's gross still reports"
    );
    assert!(
        result.net_profit >= U256::from(MIN_PROFIT_NET),
        "the net clears the dispatch floor; only the adapter's verdict declines it"
    );
    assert!(
        !result.passed,
        "the adapter's own verdict is carried, not re-derived"
    );
    assert!(
        !result.is_submittable(),
        "a failed assessment is never submittable, whatever its net"
    );
}

/// The fail-closed fan-out: a zero-balance probe over a token whose storage
/// layout this build has not verified is REFUSED as a typed planning answer that
/// names the asset, and the fan-out executes nothing — not a single provider
/// request, so no EVM state was ever built.
///
/// This is the counter-proof the retained-balance failure needs: simulating with
/// the token left in place would report whatever the untouched balance produced
/// as if the path had run.
#[test]
fn an_unverified_token_refuses_the_fanout_before_any_evm_work() {
    let transport = CountingTransport::default();
    let client = RpcClient::new(transport.clone(), true);
    let root: RootProvider = RootProvider::new(client);
    let provider = AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    );
    // The pinned production layouts only: the fixture's lab token is unverified.
    let deployment = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
    .with_capital(degenbot_simulation::ProbeCapital::ZeroBalance)
    .deployment(test_deployment_id());

    let refusal = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("multi-thread runtime");
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let pool_divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                let ctx = degenbot_arbitrage::SimulateContext {
                    block: block(&provider),
                    deployment: &deployment,
                };
                runtime.block_on(async {
                    dispatch_profitable_results(
                        vec![candidate()],
                        &ctx,
                        &suppression,
                        BLOCK,
                        0,
                        &pool_divergence,
                        &fot,
                        Some(bot_state),
                        Some(warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
    .expect_err("the unverified token refuses the batch");
    assert_eq!(
        refusal,
        degenbot_arbitrage::DispatchRefusal::CapitalPlan {
            error: degenbot_simulation::CapitalPlanError::UnverifiedLayout {
                asset: degenbot_simulation::CapitalAsset::Erc20 { token: LAB_TOKEN }
            },
        },
        "the refusal names the asset"
    );
    assert!(
        refusal.to_string().contains("no verified storage layout"),
        "the message states the gap: {refusal}"
    );
    assert_eq!(
        transport.calls(),
        0,
        "the refusal happens before any provider read, so no EVM state was built"
    );
}

/// The bounded fan-out filter: a candidate the selection walk already DECLINED
/// cannot suppress a simulatable sibling.
///
/// The plan is composed before every per-candidate encoding decision is final, so
/// its path set is a conservative union — but a prepared decline never reaches the
/// EVM, so its assets need no override, and an unverifiable token on such a path
/// must not refuse a fan-out that carries a path this build CAN simulate. The
/// sibling here is an `Accepted` encoding over the pinned mainnet pair; the
/// declined one crosses a lab token the production layouts do not verify.
#[test]
fn a_declined_candidate_does_not_refuse_a_simulatable_sibling() {
    let transport = CountingTransport::default();
    let client = RpcClient::new(transport.clone(), true);
    let root: RootProvider = RootProvider::new(client);
    let provider = AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    );
    // The pinned production layouts only, and a probe posture: the refused
    // candidate's lab token has none, the sibling's WETH/USDC both do.
    let deployment = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
    .with_capital(degenbot_simulation::ProbeCapital::ZeroBalance)
    .deployment(test_deployment_id());

    let declined = DispatchCandidate {
        candidate_id: degenbot_execution::CandidateId::new(8, test_deployment_id()),
        prepared: Some(PreparedEncoding::Declined(
            "this deployment has no representation for the shape".to_string(),
        )),
        ..candidate()
    };
    let accepted = DispatchCandidate {
        candidate_id: degenbot_execution::CandidateId::new(9, test_deployment_id()),
        path_info: weth_usdc_v2_hops(),
        prepared: Some(PreparedEncoding::Accepted(TransactionRequest::call(
            EXECUTOR,
            Bytes::new(),
        ))),
        ..candidate()
    };

    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("multi-thread runtime");
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let pool_divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                let ctx = degenbot_arbitrage::SimulateContext {
                    block: block(&provider),
                    deployment: &deployment,
                };
                runtime.block_on(async {
                    dispatch_profitable_results(
                        vec![declined, accepted],
                        &ctx,
                        &suppression,
                        BLOCK,
                        0,
                        &pool_divergence,
                        &fot,
                        Some(bot_state),
                        Some(warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
    .expect("the declined candidate's unverified token does not refuse the fan-out");

    assert_eq!(
        outcome.candidate_count, 2,
        "both candidates were admitted; the fan-out was not refused at planning"
    );
    assert!(
        transport.calls() > 0,
        "the accepted sibling reached the EVM, so the plan composed for it"
    );
}

/// The empty-fan-out plan: a probe deployment asked for no path keeps its own
/// posture bound to its executor and writes nothing — never `NoPaths`, and never
/// a silent conversion to production.
#[test]
fn an_empty_probe_fanout_keeps_its_posture_and_writes_nothing() {
    for posture in [
        degenbot_simulation::ProbeCapital::SelfFund,
        degenbot_simulation::ProbeCapital::ZeroBalance,
    ] {
        let deployment = CmdExecutorBinding::new(
            OWNER,
            EXECUTOR,
            WETH,
            POOL_MANAGER,
            EncodeOptions::default(),
        )
        .with_capital(posture)
        .deployment(test_deployment_id());
        let overrides = deployment
            .override_params(&[])
            .expect("an empty fan-out is not a planning refusal");
        assert_eq!(overrides.capital.mode(), posture, "the posture travels");
        assert_eq!(
            overrides.capital.holder(),
            Some(EXECUTOR),
            "the plan still names the executor whose capital it models"
        );
        assert!(
            overrides.capital.overrides().is_empty(),
            "no path touches an asset, so nothing is funded or cleared"
        );
        assert_eq!(overrides.owner, OWNER, "owner gas funding is untouched");
        assert!(
            overrides.settlement.is_some(),
            "the settlement warmup is untouched"
        );
    }
}

/// The all-answered batch: a fan-out whose EVERY candidate the selection walk
/// already refused composes the deployment's no-write plan and proceeds to the
/// per-candidate records.
///
/// Each row here is a prepared capital-policy decline — the shape a certified
/// zero-balance fan-out produces — so the capital path set is empty. The batch
/// must answer with the typed outcomes those rows already carry rather than a
/// planning refusal, and no candidate may reach the EVM. The path crosses the lab
/// token whose layout the production source does not verify: a planner that
/// folded a declined row back into its asset set would refuse here instead.
#[test]
fn an_all_answered_probe_batch_keeps_its_typed_outcomes_and_runs_no_evm() {
    for posture in [
        degenbot_simulation::ProbeCapital::SelfFund,
        degenbot_simulation::ProbeCapital::ZeroBalance,
    ] {
        let transport = RecordingTransport::default();
        let client = RpcClient::new(transport.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        let provider = AlloyProvider::from_provider(
            Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        );
        let deployment = CmdExecutorBinding::new(
            OWNER,
            EXECUTOR,
            WETH,
            POOL_MANAGER,
            EncodeOptions::default(),
        )
        .with_capital(posture)
        .deployment(test_deployment_id());

        let deficit = ZeroCapitalDeficit {
            asset: LAB_TOKEN,
            required: 1_000,
            available: 250,
        };
        let answered = DispatchCandidate {
            prepared: Some(PreparedEncoding::InsolventZeroCapital(deficit)),
            ..candidate()
        };

        let outcome = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .expect("multi-thread runtime");
                    let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                    let pool_divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                    let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                    let bot_state = Arc::new(StateLock::new(BotState::new()));
                    let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                    let ctx = degenbot_arbitrage::SimulateContext {
                        block: block(&provider),
                        deployment: &deployment,
                    };
                    runtime.block_on(async {
                        dispatch_profitable_results(
                            vec![answered],
                            &ctx,
                            &suppression,
                            BLOCK,
                            0,
                            &pool_divergence,
                            &fot,
                            Some(bot_state),
                            Some(warm_cache),
                        )
                    })
                })
                .join()
                .expect("fan-out thread")
        })
        .expect("an answered batch is not a capital-plan refusal");

        assert_eq!(
            outcome.candidate_count, 1,
            "the row was admitted and answered"
        );
        assert_eq!(
            outcome.exception_count, 0,
            "a prepared decline is a record, never a stage exception"
        );
        assert_eq!(outcome.fail_count, 1);
        assert_eq!(
            outcome
                .fail_buckets
                .buckets()
                .get(degenbot_arbitrage::INSOLVENT_ZERO_CAPITAL_BUCKET)
                .copied(),
            Some(1),
            "the prepared deficit reaches the record: {:?}",
            outcome.fail_buckets.buckets()
        );
        assert_eq!(outcome.failures.len(), 1, "one candidate, one record");
        let failure = outcome.failures.first().expect("one candidate, one record");
        assert_eq!(
            failure.zero_capital,
            Some(deficit),
            "the typed deficit the row carried survives the batch"
        );
        let methods = transport.methods();
        assert!(
            !methods.iter().any(|method| executes_evm(method)),
            "no candidate reached the EVM: {methods:?}"
        );
    }
}

/// Pins: the composition boundary represents a deployment with no PoolManager /
/// WETH identity at all, and such a deployment still derives the path's own
/// prefetch words (only the settlement-side words are skipped).
#[test]
fn a_deployment_without_settlement_identities_is_representable() {
    let deployment = foreign_deployment();
    assert!(deployment.settlement().is_none());
    assert!(deployment.code().is_some());
    let overrides = deployment
        .override_params(&[sim_path(PREPAY_U128)])
        .expect("a production deployment composes no capital plan");
    assert!(overrides.settlement.is_none());
    assert_eq!(overrides.owner, OWNER);
    assert_eq!(
        overrides.capital.mode(),
        degenbot_simulation::ProbeCapital::Production,
        "an ordinary deployment installs and clears nothing"
    );
}

/// Pins the built-in packet: the adapter declares no probe reads (the contract
/// returns its own receipt — ADR-098 D7), targets the deployment's one executor
/// with a zero native value, and produces the command-stream bytes the session's
/// encode policy chooses — composed independently here, so the pin is not
/// circular.
#[test]
fn the_builtin_binding_declares_no_probes_and_keeps_its_command_bytes() {
    let opts = EncodeOptions {
        erc6909_profit: false,
        use_v4_batch: false,
        ..EncodeOptions::default()
    };
    let binding = CmdExecutorBinding::new(OWNER, EXECUTOR, WETH, POOL_MANAGER, opts);
    let adapter = binding.adapter();

    assert!(
        adapter.probe_spec().is_empty(),
        "the built-in assessment reads the execute receipt, not pre/post balances"
    );

    let path = two_v2_hops();
    let hop_outputs = [PREPAY_U128, PREPAY_U128];
    let consumed_inputs = [PREPAY_U128, PREPAY_U128];
    let transaction = adapter
        .encode(
            &path,
            &ComposerInputs {
                optimal_input: PREPAY_U128,
                hop_outputs: &hop_outputs,
                consumed_inputs: &consumed_inputs,
                opts: ComposeOptions,
            },
        )
        .expect("the built-in adapter encodes");

    // Independently composed golden: the command stream + the packed config.
    let request = EncodeRequest::new(
        path,
        PREPAY_U128,
        hop_outputs.to_vec(),
        consumed_inputs.to_vec(),
        opts,
    );
    let commands = encode_cmd_stream(&EncodeContext::new(EXECUTOR, POOL_MANAGER, WETH), &request)
        .expect("the fixture path encodes");
    let config = config_for_options(opts, U256::ZERO).expect("axes in range");
    let expected = degenbot_executor::composers::encode_execute_call(EXECUTOR, &commands, config);

    assert_eq!(transaction.to, EXECUTOR, "single-target: its own executor");
    assert_eq!(
        transaction.value,
        U256::ZERO,
        "the execute call is not funded"
    );
    assert_eq!(transaction.data, Bytes::from(expected.data));
}

/// Pins the production-code authority: a binding's default deployment declares
/// NO code override (the sim executes the deployed code), while an explicit
/// injection declares its runtime at its own address.
#[test]
fn the_builtin_binding_injects_code_only_when_declared() {
    let binding = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    );
    let deployed = binding.deployment(test_deployment_id());
    assert!(deployed.code().is_none(), "production uses deployed code");
    assert_eq!(deployed.owner(), OWNER);
    let settlement = deployed
        .settlement()
        .expect("the built-in deployment declares its settlement identities");
    assert_eq!(settlement.weth_address, WETH);
    assert_eq!(settlement.pool_manager_address, Some(POOL_MANAGER));

    let injected = binding
        .injected(test_deployment_id(), EXECUTOR, Bytes::from_static(&[0xfe]))
        .expect("the declared override names the executor");
    let code = injected.code().expect("the declared override is explicit");
    assert_eq!(code.address, EXECUTOR);
    assert_eq!(code.runtime.as_ref(), &[0xfe]);
}

/// The capital-targeting parity (the task's acceptance): the settlement warmup
/// a built-in deployment declares keys its WETH9 / ERC6909 slots on the
/// EXECUTOR in BOTH arms — a declared-code build and the deployed contract —
/// never on the owner, who only funds gas. The probe overrides name that same
/// executor, so the deployed arm can no longer follow the owner by accident.
#[test]
fn capital_overrides_name_the_executor_in_both_deployment_arms() {
    let executor_slots = compute_simulation_warmup_slots(EXECUTOR, WETH);
    let owner_slots = compute_simulation_warmup_slots(OWNER, WETH);
    assert_ne!(
        executor_slots, owner_slots,
        "the fixture's owner and executor are distinct accounts, so their slots differ"
    );

    // A zero-balance probe: the clearing must land on the executor.
    let zero_balance = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
    .with_capital(degenbot_simulation::ProbeCapital::ZeroBalance);
    // The SAME binding's encoder axes: the zero-balance capital value supplies
    // the funding axis and the capital-validation policy, exactly as the
    // self-funded binding below derives its own pair. Nothing beside the capital
    // value is asked to name either one — and the two are DISTINCT: `funding` is
    // a stream axis while `initial_capital` is a validation stance.
    assert_eq!(
        zero_balance.opts().funding,
        degenbot_executor::grammar_ledger::FundingSource::InPathFlash,
        "the zero-balance capital value derives the in-path-flash funding axis"
    );
    assert_eq!(
        zero_balance.opts().initial_capital,
        degenbot_executor::grammar_ledger::InitialCapital::Zero,
        "the zero-balance capital value derives the foundation's zero-capital policy"
    );
    assert_eq!(
        zero_balance.capital(),
        degenbot_simulation::ProbeCapital::ZeroBalance,
        "the binding keeps the posture its axes were projected from"
    );
    let declared = zero_balance
        .injected(test_deployment_id(), EXECUTOR, Bytes::from_static(&[0xfe]))
        .expect("the declared override names the executor")
        .with_capital_layouts(lab_layouts());
    let deployed = zero_balance
        .deployment(test_deployment_id())
        .with_capital_layouts(lab_layouts());
    let paths = [sim_path(PREPAY_U128)];

    for (arm, deployment) in [("declared", declared), ("deployed", deployed)] {
        let settlement = deployment
            .settlement()
            .expect("both arms declare the settlement identities");
        assert_eq!(
            settlement.slots, executor_slots,
            "{arm}: the warmup slots key on the executor, not the owner"
        );
        let capital = deployment
            .override_params(&paths)
            .expect("the fixture declares its lab token's layout")
            .capital;
        assert_eq!(
            capital.mode(),
            degenbot_simulation::ProbeCapital::ZeroBalance,
            "{arm}: the one capital plan clears, in both arms"
        );
        assert_eq!(
            capital.holder(),
            Some(EXECUTOR),
            "{arm}: the capital target is the executor"
        );
        assert!(
            capital
                .assets()
                .any(|asset| asset == degenbot_simulation::CapitalAsset::Erc20 { token: WETH }),
            "{arm}: the path's WETH holding is named"
        );
        assert!(
            capital.assets().any(|asset| asset
                == degenbot_simulation::CapitalAsset::Erc20 { token: LAB_TOKEN }),
            "{arm}: the path's own lab token is named too, from its declared layout"
        );
        assert!(
            capital
                .overrides()
                .iter()
                .all(|entry| entry.value() == alloy::primitives::U256::ZERO),
            "{arm}: a zero-balance plan clears, never funds"
        );
        assert_eq!(
            deployment.capital(),
            degenbot_simulation::ProbeCapital::ZeroBalance,
            "{arm}: the deployment keeps the posture it was composed from"
        );
        assert_eq!(
            deployment.target(),
            EXECUTOR,
            "{arm}: the deployment executes at the executor the capital names"
        );
    }
}

/// The self-funded half of the same parity: the installed capital IS the built
/// Plan's declared requirement, on the executor, in both arms — and every other
/// holding the path touches is cleared rather than granted.
#[test]
fn self_funded_probe_overrides_install_the_plan_requirement() {
    let executor_slots = compute_simulation_warmup_slots(EXECUTOR, WETH);
    let paths = [sim_path(PREPAY_U128)];
    // The capital value — not a hand-set `funding` axis — supplies the encoding.
    let self_funded = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
    .with_capital(degenbot_simulation::ProbeCapital::SelfFund);
    assert_eq!(
        self_funded.opts().funding,
        degenbot_executor::grammar_ledger::FundingSource::SelfFund,
        "the self-funded capital value derives the stream's funding axis"
    );
    assert_eq!(
        self_funded.opts().initial_capital,
        degenbot_executor::grammar_ledger::InitialCapital::Held,
        "a self-funded probe validates against held capital, not the zero policy"
    );
    let declared = self_funded
        .injected(test_deployment_id(), EXECUTOR, Bytes::from_static(&[0xfe]))
        .expect("the declared override names the executor")
        .with_capital_layouts(lab_layouts());
    let deployed = self_funded
        .deployment(test_deployment_id())
        .with_capital_layouts(lab_layouts());
    for (arm, deployment) in [("declared", declared), ("deployed", deployed)] {
        assert_eq!(
            deployment.settlement().map(|settlement| settlement.slots),
            Some(executor_slots),
            "{arm}: the warmup slots key on the executor, not the owner"
        );
        let capital = deployment
            .override_params(&paths)
            .expect("the fixture declares its lab token's layout")
            .capital;
        assert_eq!(
            capital.mode(),
            degenbot_simulation::ProbeCapital::SelfFund,
            "{arm}: self-funded probe capital installs an entry"
        );
        assert_eq!(
            capital.holder(),
            Some(EXECUTOR),
            "{arm}: self-funded probe capital targets the executor"
        );
        // The all-V2 self-funded family seeds the entry it spends: the whole
        // optimal input, in the cycle's closing WETH — and nothing else. The
        // lab token the path crosses is touched, never granted.
        assert_eq!(
            capital
                .overrides()
                .iter()
                .map(|entry| (entry.asset(), entry.value()))
                .collect::<Vec<_>>(),
            vec![
                (
                    degenbot_simulation::CapitalAsset::Native,
                    alloy::primitives::U256::ZERO
                ),
                (
                    degenbot_simulation::CapitalAsset::Erc20 { token: WETH },
                    alloy::primitives::U256::from(PREPAY_U128)
                ),
                (
                    degenbot_simulation::CapitalAsset::Erc20 { token: LAB_TOKEN },
                    alloy::primitives::U256::ZERO
                ),
            ],
            "{arm}: the installed capital IS the Plan's declared requirement, and the \
             path's other touched holding is cleared rather than granted"
        );
    }
}

/// The ordinary production stance, pinned beside the probe arms above: a default
/// session enables no probe capital at all — no self-funded USDC holder and no
/// zero-balance clearing — while still keeping its real owner and its executor
/// target and declaring the CORRECTED settlement warmup on the executor. The
/// probe overrides are explicit session choices, so production can never acquire
/// one by inheriting a warmup subject.
#[test]
fn the_default_production_deployment_enables_no_probe_capital() {
    let opts = EncodeOptions::default();
    assert_eq!(
        opts.funding,
        degenbot_executor::grammar_ledger::FundingSource::InPathFlash,
        "the default session funds its entry in-path, never from the executor's USDC"
    );
    assert_eq!(
        opts.initial_capital,
        degenbot_executor::grammar_ledger::InitialCapital::Held,
        "the default session's capital policy is ordinary validation"
    );

    let binding = CmdExecutorBinding::new(OWNER, EXECUTOR, WETH, POOL_MANAGER, opts);
    assert_eq!(binding.owner(), OWNER);
    assert_eq!(binding.executor(), EXECUTOR);
    assert_eq!(
        binding.capital(),
        degenbot_simulation::ProbeCapital::Production,
        "the default session declares the production capital posture"
    );
    assert_eq!(
        binding.opts().funding,
        degenbot_executor::grammar_ledger::FundingSource::InPathFlash,
        "the production capital value keeps the in-path-flash funding axis"
    );
    assert_eq!(
        binding.opts().initial_capital,
        degenbot_executor::grammar_ledger::InitialCapital::Held,
        "the production capital value keeps ordinary capital validation"
    );

    let default_slots = compute_simulation_warmup_slots(EXECUTOR, WETH);
    assert_ne!(
        default_slots,
        compute_simulation_warmup_slots(OWNER, WETH),
        "the fixture's owner and executor are distinct accounts, so their slots differ"
    );

    let arms = [
        ("deployed", binding.deployment(test_deployment_id()), false),
        (
            "declared",
            binding
                .injected(test_deployment_id(), EXECUTOR, Bytes::from_static(&[0xfe]))
                .expect("the declared override names the executor"),
            true,
        ),
    ];

    for (arm, deployment, declares_code) in arms {
        let overrides = deployment
            .override_params(&[sim_path(PREPAY_U128)])
            .expect("production composes no capital plan");
        assert_eq!(
            overrides.capital.mode(),
            degenbot_simulation::ProbeCapital::Production,
            "{arm}: ordinary production installs and clears no executor capital"
        );
        assert_eq!(
            overrides.capital.holder(),
            None,
            "{arm}: production names no capital target at all"
        );
        assert!(
            overrides.capital.is_empty(),
            "{arm}: production resolves no write, not even an empty one per asset"
        );
        assert_eq!(
            overrides.owner, OWNER,
            "{arm}: the owner is the gas account"
        );
        assert_eq!(
            deployment.target(),
            EXECUTOR,
            "{arm}: the transaction targets the executor"
        );
        assert_eq!(
            deployment.code().is_some(),
            declares_code,
            "{arm}: only an explicit declaration installs code"
        );

        let settlement = deployment
            .settlement()
            .expect("the built-in deployment declares its settlement identities");
        assert_eq!(settlement.weth_address, WETH);
        assert_eq!(settlement.pool_manager_address, Some(POOL_MANAGER));
        assert_eq!(
            settlement.slots, default_slots,
            "{arm}: the warmup keys on the executor, not the owner, and enables no probe"
        );
    }
}

/// Pins the declared-override boundary: the built-in adapter encodes every
/// transaction to the binding's executor, so an override declared anywhere else
/// is refused. A mismatch can never silently simulate code the transaction does
/// not run.
#[test]
fn a_builtin_declared_override_must_name_the_executor() {
    let binding = CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    );
    let elsewhere = address!("0xdddddddddddddddddddddddddddddddddddddddd");

    let mismatch = binding
        .injected(test_deployment_id(), elsewhere, Bytes::from_static(&[0xfe]))
        .expect_err("an override at a foreign address is refused");
    assert_eq!(mismatch.address, elsewhere);
    assert_eq!(mismatch.executor, EXECUTOR);
    assert!(
        mismatch.to_string().contains("would simulate code"),
        "the refusal names why: {mismatch}"
    );

    let valid = binding
        .injected(test_deployment_id(), EXECUTOR, Bytes::from_static(&[0xfe]))
        .expect("the executor's own address is the valid injection");
    assert_eq!(valid.code().map(|code| code.address), Some(EXECUTOR));
}
