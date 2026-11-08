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
    FeeOnTransferRegistry, PoolDivergence, SimulationBlock, SimulationDeployment, SolveStep,
    MIN_PROFIT_NET,
};
use degenbot_execution::{
    AssessError, ComposeError, ComposeOptions, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, ProbeSpec, ProbeSpecs,
    TransactionRequest, V2HopInfo,
};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest,
};
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

/// Pins: the composition boundary represents a deployment with no PoolManager /
/// WETH identity at all, and such a deployment still derives the path's own
/// prefetch words (only the settlement-side words are skipped).
#[test]
fn a_deployment_without_settlement_identities_is_representable() {
    let deployment = foreign_deployment();
    assert!(deployment.settlement().is_none());
    assert!(deployment.code().is_some());
    let overrides = deployment.override_params();
    assert!(overrides.settlement.is_none());
    assert_eq!(overrides.owner, OWNER);
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
