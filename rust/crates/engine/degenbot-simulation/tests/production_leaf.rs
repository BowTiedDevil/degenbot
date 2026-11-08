#![expect(clippy::expect_used)]
// Solidity/EVM identifiers (`PoolManager`, `msg.value`) are ubiquitous in this
// crate's prose; match the crate's doc-markdown stance.
#![expect(clippy::doc_markdown)]

//! The production simulation leaf over a foreign adapter (ADR-098 D2/D3/D6):
//! one adapter transaction → engine execution + declared probe reads → typed
//! `ExecutionObservation` → the adapter's own Assess.
//!
//! The adapter is deliberately foreign: its contract has no WETH, no
//! PoolManager, no command stream, no version/release metadata, and no
//! seven-call balance bundle. It takes a native-ETH prepayment as `msg.value`
//! (so value preservation is visible in the executed call: booked in storage,
//! returned, and emitted) and it observes and assesses its own evidence.
//!
//! Everything here runs on real deployed bytecode in an in-memory EVM — the
//! same engine mechanics the canonical dispatch path will call.

use ::evm2::bytecode::Bytecode;
use ::evm2::ethereum::RecoveredTxEnvelope;
use ::evm2::evm::{AccountInfo, CacheDB, Db, InMemoryDB};
use alloy::primitives::{address, keccak256, Address, Bytes, TxKind, B256, U256};

use degenbot_execution::{
    AssessError, ComposeError, ComposerInputs, ExecutionAdapter, ExecutionObservation,
    ExecutionResult, ExecutionStatus, PathInfo, ProbeSpec, ProbeSpecs, SolveResult,
    TransactionRequest,
};
use degenbot_simulation::sim::evm2::session::{Evm2CaptureMode, Evm2SimSession, Evm2SimStep};
use degenbot_simulation::sim::production::{
    simulate_opportunity, simulate_transaction, ProductionSimContext, ProductionSimError,
    ProductionSimulation,
};

const SENDER: Address = address!("0x1111111111111111111111111111111111111111");
const ROUTER: Address = address!("0x2222222222222222222222222222222222222222");
const REVERTER: Address = address!("0x3333333333333333333333333333333333333333");
const FROZEN: Address = address!("0x4444444444444444444444444444444444444444");
/// No account at all: a read with no callee.
const BLANK: Address = address!("0x5555555555555555555555555555555555555555");

const BLOCK: u64 = 21_000_000;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;

/// The foreign contract's native prepayment, in wei: nonzero, so the value must
/// travel with the call.
const PREPAY: U256 = U256::from_limbs([5_000_000_000_000_000, 0, 0, 0]);

/// The solved flash input the adapter encodes as its call's argument.
const FLASH_INPUT: u128 = 1_000_000_000_000_000_000;

/// The 4-byte calldata the declared read carries (a selector-shaped read).
const READ_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// The profitable-looking figure the reverting contract puts in its revert
/// payload — evidence a revert must not turn into a pass.
const CONTRACT_PROFIT: u128 = 9_000_000_000_000_000;

fn prepaid_topic() -> B256 {
    keccak256(b"Prepaid(uint256)")
}

fn prepay_selector() -> [u8; 4] {
    let digest = keccak256(b"prepay(uint256)");
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&digest[..4]);
    selector
}

// ════════════════════════════════════════════════════════════════════════════
// Fixture bytecode — a hand-built contract, assembled with resolved labels
// ════════════════════════════════════════════════════════════════════════════

/// A two-pass mini assembler: PUSH1 label fixups resolve to the `JUMPDEST`
/// offset, so a fixture edit cannot silently skew a jump target.
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

/// The foreign contract: a 4-byte calldata is the declared read (it returns the
/// prepayment it booked); any other calldata is the prepayment itself, which
/// books `msg.value` in slot 0, emits `Prepaid(msg.value)`, and returns it.
fn router_code(topic: B256) -> Vec<u8> {
    let mut push_topic = Vec::with_capacity(33);
    push_topic.push(0x7f); // PUSH32
    push_topic.extend_from_slice(topic.as_slice());
    assemble(&[
        // calldatasize == 4 → read, otherwise → prepay
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

/// A contract that reverts with a full 32-byte word of "profit".
fn reverter_code() -> Vec<u8> {
    let mut code = vec![0x7f]; // PUSH32
    code.extend_from_slice(&U256::from(CONTRACT_PROFIT).to_be_bytes::<32>());
    code.extend_from_slice(&[
        0x60, 0x00, 0x52, // PUSH1 0 MSTORE
        0x60, 0x20, 0x60, 0x00, 0xfd, // REVERT mem[0..32]
    ]);
    code
}

/// A contract whose every call reverts with no data.
fn frozen_code() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xfd]
}

// ════════════════════════════════════════════════════════════════════════════
// Fixture EVM + the foreign adapter
// ════════════════════════════════════════════════════════════════════════════

fn fixture_evm() -> ::evm2::Evm<'static, ::evm2::BaseEvmTypes> {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &SENDER,
        AccountInfo::default()
            .with_balance(U256::from(10u64) * U256::from(10u64).pow(U256::from(18u64))),
    );
    db.insert_account_info(
        &ROUTER,
        AccountInfo::default()
            .with_code(Bytecode::new_raw(Bytes::from(router_code(prepaid_topic())))),
    );
    db.insert_account_info(
        &REVERTER,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from(reverter_code()))),
    );
    db.insert_account_info(
        &FROZEN,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from(frozen_code()))),
    );
    degenbot_simulation::sim::evm2::build_block_evm2(
        Db::new(CacheDB::new(Db::new(db))),
        BASE_FEE,
        BLOCK,
        TIMESTAMP,
    )
}

fn with_session<T>(run: impl FnOnce(&mut Evm2SimSession<'_, '_>) -> T) -> T {
    let mut evm = fixture_evm();
    let mut session = Evm2SimSession::with_block_env(
        &mut evm,
        degenbot_simulation::sim::evm2::handle_block_env(BASE_FEE, BLOCK, TIMESTAMP),
    );
    run(&mut session)
}

fn context() -> ProductionSimContext {
    ProductionSimContext {
        caller: SENDER,
        gas_limit: GAS_LIMIT,
    }
}

fn path() -> PathInfo {
    PathInfo::new(Vec::new())
}

fn solved() -> SolveResult {
    SolveResult {
        path_id: 1,
        hop_count: 0,
        optimal_input: U256::from(FLASH_INPUT),
        hop_outputs: Vec::new(),
        consumed_inputs: Vec::new(),
        net_profit: U256::ZERO,
        hop_descriptors: Vec::new(),
    }
}

/// How the foreign adapter interprets its observation — the two built-in
/// shapes are defaults, so the test drives a bespoke one beside them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ForeignAssess {
    /// The defaulted sum-of-deltas gate over declared probe reads.
    Default,
    /// The adapter's own receipt shape: the `Prepaid` event it emits.
    OwnEvent,
    /// The contract's own return word.
    ReturnWord,
}

struct ForeignAdapter {
    target: Address,
    value: U256,
    probes: ProbeSpecs,
    assess: ForeignAssess,
}

impl ForeignAdapter {
    fn prepaying() -> Self {
        Self {
            target: ROUTER,
            value: PREPAY,
            probes: vec![booked_probe()],
            assess: ForeignAssess::OwnEvent,
        }
    }

    fn with_probes(probes: ProbeSpecs, assess: ForeignAssess) -> Self {
        Self {
            probes,
            assess,
            ..Self::prepaying()
        }
    }
}

fn booked_probe() -> ProbeSpec {
    ProbeSpec::new("BOOKED".to_string(), Some(ROUTER), READ_SELECTOR)
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
        Ok(TransactionRequest::new(
            self.target,
            Bytes::from(data),
            self.value,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let gross = match self.assess {
            ForeignAssess::Default => {
                return self
                    .assess_options()
                    .assess(observation, &self.fee_policy());
            }
            ForeignAssess::OwnEvent => {
                let event = observation
                    .events
                    .iter()
                    .find(|event| event.topics.first() == Some(&prepaid_topic()))
                    .ok_or_else(|| AssessError::evidence("no Prepaid event was captured"))?;
                let data = event
                    .data
                    .get(..32)
                    .ok_or_else(|| AssessError::evidence("Prepaid event data is not one word"))?;
                U256::from_be_slice(data)
            }
            ForeignAssess::ReturnWord => observation
                .first_return_word()
                .ok_or_else(|| AssessError::evidence("the contract returns the booked amount"))?,
        };
        Ok(ExecutionResult::price(
            gross,
            Vec::new(),
            observation,
            self.assess_options().min_net_profit,
            &self.fee_policy(),
        ))
    }
}

/// An adapter that declines to encode — the Encode part's typed failure.
struct DecliningAdapter;

impl ExecutionAdapter for DecliningAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Err(ComposeError::unsupported("this contract cannot prepay"))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NONE: ProbeSpecs = Vec::new();
        &NONE
    }
}

/// `(label, addr, selector)` envelopes rebuilt here, independently of the leaf,
/// so the gas pin compares the engine's measurement with a second measurement
/// instead of with the leaf's own bookkeeping.
fn legacy_envelope(to: Address, data: &Bytes, value: U256, gas_limit: u64) -> RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(to),
            input: data.clone(),
            value,
            gas_limit,
            gas_price: BASE_FEE,
            ..Default::default()
        }),
        SENDER,
    )
}

fn simulate(adapter: &ForeignAdapter) -> ProductionSimulation {
    with_session(|session| {
        simulate_opportunity(session, adapter, &path(), &solved(), &context())
            .expect("the foreign opportunity simulates")
    })
}

fn word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

// ════════════════════════════════════════════════════════════════════════════
// Execution — one opportunity, one transaction, its nonzero value
// ════════════════════════════════════════════════════════════════════════════

/// Pins: the leaf executes exactly the adapter's transaction — its target, its
/// calldata, and its nonzero native value — and that value reaches the executed
/// call (the contract books `msg.value` and returns it).
#[test]
fn leaf_executes_the_adapter_transaction_with_its_value() {
    let adapter = ForeignAdapter::prepaying();
    let simulation = simulate(&adapter);
    let observation = &simulation.observation;

    assert_eq!(observation.transaction.to, ROUTER);
    assert_eq!(observation.transaction.value, PREPAY);
    assert_ne!(observation.transaction.value, U256::ZERO);
    assert_eq!(&observation.transaction.data[..4], &prepay_selector());
    assert_eq!(observation.transaction.data.len(), 4 + 32);

    assert_eq!(observation.status, ExecutionStatus::Success);
    assert!(
        observation.gas_used > 0,
        "the executed call burned gas: {}",
        observation.gas_used
    );
    assert_eq!(
        observation.first_return_word(),
        Some(PREPAY),
        "the contract returned the prepayment it booked from msg.value"
    );
}

/// Pins: the declared probe reads run in declared order, before and after the
/// transaction inside one journaled sequence — so the read observes the booked
/// prepayment — and a read the engine could not turn into a figure is ABSENT,
/// never a fabricated zero.
#[test]
fn leaf_reads_declared_probes_in_order_and_reports_absent_edges() {
    let adapter = ForeignAdapter::with_probes(
        vec![
            booked_probe(),
            ProbeSpec::new("FROZEN".to_string(), Some(FROZEN), READ_SELECTOR),
            ProbeSpec::new("BLANK".to_string(), Some(BLANK), READ_SELECTOR),
            ProbeSpec::new("NO_CALLEE".to_string(), None, READ_SELECTOR),
        ],
        ForeignAssess::ReturnWord,
    );
    let simulation = simulate(&adapter);
    let readings = &simulation.observation.probe_reads;

    let labels: Vec<&str> = readings
        .iter()
        .map(|reading| reading.spec.label.as_str())
        .collect();
    assert_eq!(labels, vec!["BOOKED", "FROZEN", "BLANK", "NO_CALLEE"]);

    assert_eq!(readings[0].before, Some(U256::ZERO));
    assert_eq!(readings[0].after, Some(PREPAY));
    assert_eq!(readings[0].delta(), Some(5_000_000_000_000_000i128));

    assert_eq!(
        (readings[1].before, readings[1].after),
        (None, None),
        "a reverting read observed nothing"
    );
    assert_eq!(readings[1].delta(), None);

    assert_eq!(
        (readings[2].before, readings[2].after),
        (None, None),
        "a callee with no code returned no figure"
    );

    assert_eq!(
        (readings[3].before, readings[3].after),
        (None, None),
        "a pseudo-read with no callee cannot be read"
    );
}

/// Pins: every event the transaction emitted is captured with its emitter, its
/// indexed topics, its data, and its index within the transaction.
#[test]
fn leaf_captures_the_events_the_transaction_emitted() {
    let adapter = ForeignAdapter::prepaying();
    let simulation = simulate(&adapter);
    let events = &simulation.observation.events;

    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].address, ROUTER);
    assert_eq!(events[0].topics, vec![prepaid_topic()]);
    assert_eq!(events[0].data, word(PREPAY));
    assert_eq!(events[0].log_index, 0);
}

/// Pins: the reported gas is the engine's own measurement for the sequence it
/// ran (not a submission-inflated figure and not the transaction's gas limit),
/// and the reported base fee is the fee the transaction actually ran under.
#[test]
fn leaf_reports_the_measured_gas_and_the_block_base_fee() {
    let adapter = ForeignAdapter::prepaying();
    let transaction = adapter
        .compose_view(&path(), &solved())
        .expect("the adapter encodes");

    let (simulation, reference_gas) = with_session(|session| {
        let simulation = simulate_opportunity(session, &adapter, &path(), &solved(), &context())
            .expect("the foreign opportunity simulates");

        // The same journaled sequence (pre-read, transaction, post-read) built
        // independently here: the leaf must report this step's gas verbatim.
        let steps = vec![
            Evm2SimStep::Transact(legacy_envelope(
                ROUTER,
                &Bytes::copy_from_slice(&READ_SELECTOR),
                U256::ZERO,
                degenbot_simulation::sim::production::PROBE_CALL_GAS_LIMIT,
            )),
            Evm2SimStep::Inspect(legacy_envelope(
                transaction.to,
                &transaction.data,
                transaction.value,
                GAS_LIMIT,
            )),
            Evm2SimStep::Transact(legacy_envelope(
                ROUTER,
                &Bytes::copy_from_slice(&READ_SELECTOR),
                U256::ZERO,
                degenbot_simulation::sim::production::PROBE_CALL_GAS_LIMIT,
            )),
        ];
        let sequence = session
            .run_sequence(&steps, Evm2CaptureMode::Seam)
            .expect("the reference sequence runs");
        (simulation, sequence.calls[1].gas_used)
    });

    assert_eq!(simulation.observation.gas_used, reference_gas);
    assert!(
        simulation.observation.gas_used < GAS_LIMIT,
        "the observed gas is the call's, not the envelope's limit"
    );
    assert_eq!(simulation.observation.base_fee_next, BASE_FEE);
    assert_eq!(simulation.result.gas_used, reference_gas);
    assert_eq!(simulation.result.base_fee_next, BASE_FEE);

    let gas_cost = U256::from(reference_gas)
        * (U256::from(BASE_FEE) + U256::from(simulation.result.priority_fee));
    assert_eq!(
        simulation.result.net_profit,
        simulation.result.gross_profit - gas_cost,
        "the pricing half prices the measured gas and the block's base fee"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Assessment — the adapter's own interpretation of the captured evidence
// ════════════════════════════════════════════════════════════════════════════

/// Pins: the leaf hands the observation to the adapter's own Assess — an
/// assessment over its emitted event, not a built-in rule — and returns that
/// verdict.
#[test]
fn leaf_invokes_the_adapters_own_assessment() {
    let adapter = ForeignAdapter::prepaying();
    let simulation = simulate(&adapter);

    assert_eq!(simulation.result.gross_profit, PREPAY);
    assert!(
        simulation.result.deltas.is_empty(),
        "the event-shaped assessment consulted no probe delta"
    );
    assert!(simulation.result.passed);
    assert_eq!(simulation.result.gross_profit, PREPAY);
}

/// Pins: the built-in sum-of-deltas default also runs over real probe readings —
/// a complete before/after pair prices to its delta.
#[test]
fn leaf_default_assessment_sums_the_observed_probe_deltas() {
    let adapter = ForeignAdapter::with_probes(vec![booked_probe()], ForeignAssess::Default);
    let simulation = simulate(&adapter);

    assert_eq!(simulation.result.deltas, vec![5_000_000_000_000_000i128]);
    assert_eq!(simulation.result.gross_profit, PREPAY);
    assert!(simulation.result.passed);
}

/// Pins: an assessment whose evidence the transaction never produced is a typed
/// assessment failure — never a silent zero-profit pass, and never a panic.
#[test]
fn leaf_reports_an_assessment_that_needs_absent_evidence() {
    let adapter = ForeignAdapter::with_probes(Vec::new(), ForeignAssess::OwnEvent);
    let mut adapter = adapter;
    adapter.target = BLANK; // no code: the call succeeds and emits nothing

    let error = with_session(|session| {
        simulate_opportunity(session, &adapter, &path(), &solved(), &context())
            .expect_err("no Prepaid event was emitted")
    });
    assert!(
        matches!(error, ProductionSimError::Assess(AssessError::Evidence(_))),
        "{error:?}"
    );
}

/// Pins: a reverted transaction is evidence, not an assessment error — the
/// observation reports the revert, and the shared pricing invariant declines it
/// to zero gross with `passed = false` even though the revert payload carries a
/// profitable-looking figure.
#[test]
fn leaf_declines_a_reverted_transaction_through_the_shared_pricing() {
    let adapter = ForeignAdapter {
        target: REVERTER,
        value: U256::ZERO,
        probes: Vec::new(),
        assess: ForeignAssess::ReturnWord,
    };
    let simulation = simulate(&adapter);
    let observation = &simulation.observation;

    assert_eq!(observation.status, ExecutionStatus::Revert);
    assert_eq!(
        observation.first_return_word(),
        Some(U256::from(CONTRACT_PROFIT)),
        "the engine captured the revert payload"
    );
    assert_eq!(simulation.result.gross_profit, U256::ZERO);
    assert_eq!(simulation.result.net_profit, U256::ZERO);
    assert!(!simulation.result.passed);
    assert_eq!(simulation.result.gas_used, observation.gas_used);
}

// ════════════════════════════════════════════════════════════════════════════
// Typed failures — encode, engine, orchestration
// ════════════════════════════════════════════════════════════════════════════

/// Pins: an adapter that declines to encode stops the leaf at Encode with a
/// typed failure — no transaction is executed.
#[test]
fn leaf_reports_an_encoder_decline_as_a_typed_error() {
    let error = with_session(|session| {
        simulate_opportunity(session, &DecliningAdapter, &path(), &solved(), &context())
            .expect_err("the adapter declines to encode")
    });
    assert!(matches!(error, ProductionSimError::Encode(_)), "{error:?}");
}

/// Pins: the engine's refusal of the sequence is a typed failure, not a panic
/// and not a manufactured observation.
#[test]
fn leaf_reports_an_engine_refusal_as_a_typed_error() {
    let adapter = ForeignAdapter::prepaying();
    let transaction = adapter
        .compose_view(&path(), &solved())
        .expect("the adapter encodes");
    let starving = ProductionSimContext {
        caller: SENDER,
        gas_limit: 1_000, // below the transaction's intrinsic gas
    };

    let error = with_session(|session| {
        simulate_transaction(session, &adapter, transaction, &starving)
            .expect_err("the engine refuses the envelope")
    });
    assert!(matches!(error, ProductionSimError::Engine(_)), "{error:?}");
}
