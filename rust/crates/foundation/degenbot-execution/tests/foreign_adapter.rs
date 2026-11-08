#![expect(clippy::expect_used)]

//! The foreign-adapter production boundary (ADR-098 D2/D6): Encode returns one
//! complete transaction, Assess interprets one typed observation.
//!
//! This is a consumer test — it reaches every type off `degenbot_execution`
//! alone. The adapter it implements is deliberately bespoke: its contract takes
//! a native-ETH prepayment as `msg.value`, names its own probe asset, and
//! interprets its own return data and captured event. None of the built-in
//! executor's settlement assumptions appear here — no WETH balance delta
//! bundle, no `PoolManager`, no command stream, no seven-call balance probe.

use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};

use degenbot_execution::{
    AssessError, AssessOptions, AssessRule, CapturedEvent, ComposeError, ComposerInputs,
    ExecutionAdapter, ExecutionObservation, ExecutionResult, ExecutionStatus, FeePolicy, PathInfo,
    PayloadAdapter, PayloadComposer, ProbeReading, ProbeSpec, ProbeSpecs, TransactionRequest,
};

const PREPAY_ROUTER: Address = address!("1234567890abcdef1234567890abcdef12345678");
const CREDIT_LEDGER: Address = address!("00000000000000000000000000000000000000fe");

/// The foreign contract's native prepayment, in wei. Nonzero: the contract is
/// funded by the caller's `msg.value`, so the transaction must carry it.
const PREPAY: U256 = U256::from_limbs([5_000_000_000_000_000, 0, 0, 0]);

/// `prepay(uint256)` — the sample contract's own selector.
fn prepay_selector() -> [u8; 4] {
    let digest = keccak256(b"prepay(uint256)");
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&digest[..4]);
    selector
}

fn prepay_calldata(amount: u128) -> Bytes {
    let mut data = Vec::with_capacity(4 + 16);
    data.extend_from_slice(&prepay_selector());
    data.extend_from_slice(&amount.to_be_bytes());
    Bytes::from(data)
}

fn no_probes() -> &'static ProbeSpecs {
    static EMPTY: ProbeSpecs = Vec::new();
    &EMPTY
}

fn credit_probe() -> ProbeSpecs {
    // The adapter's own observed asset — a credit ledger in its own system, not
    // a settlement-token balance the engine assumes on its behalf.
    vec![ProbeSpec::new(
        "CREDIT".to_string(),
        Some(CREDIT_LEDGER),
        [0xde, 0xad, 0xbe, 0xef],
    )]
}

fn sample_inputs() -> (Vec<u128>, Vec<u128>) {
    (
        vec![1_000_000_000_000_000_000u128, 1_010_000_000_000_000_000u128],
        vec![1_000_000_000_000_000_000u128, 1_010_000_000_000_000_000u128],
    )
}

/// The full foreign adapter: it returns a target + calldata + value, and it
/// observes its own credit ledger rather than asserting the built-in
/// settlement bundle.
struct PrepayAdapter {
    probes: ProbeSpecs,
}

impl PrepayAdapter {
    fn new() -> Self {
        Self {
            probes: credit_probe(),
        }
    }
}

impl ExecutionAdapter for PrepayAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::new(
            PREPAY_ROUTER,
            prepay_calldata(inputs.optimal_input),
            PREPAY,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }
}

fn encode_with(adapter: &dyn ExecutionAdapter) -> TransactionRequest {
    let (hop_outputs, consumed) = sample_inputs();
    let inputs = ComposerInputs {
        optimal_input: hop_outputs[0],
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: degenbot_execution::ComposeOptions,
    };
    adapter
        .encode(&PathInfo::new(Vec::new()), &inputs)
        .expect("a foreign adapter encodes its own transaction")
}

fn observation_of(status: ExecutionStatus) -> ExecutionObservation {
    ExecutionObservation::new(
        TransactionRequest::new(PREPAY_ROUTER, prepay_calldata(1), PREPAY),
        status,
        50_000,
        10_000_000_000,
    )
}

fn word(value: u128) -> Bytes {
    let mut buf = [0u8; 32];
    buf[16..].copy_from_slice(&value.to_be_bytes());
    Bytes::from(buf.to_vec())
}

/// An adapter whose assessment reads ONLY the transaction's return data: no
/// probe reads are declared, so a sum-of-deltas gate has nothing to sum.
struct ReturnDataAdapter;

impl ExecutionAdapter for ReturnDataAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::new(
            PREPAY_ROUTER,
            prepay_calldata(1),
            PREPAY,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        no_probes()
    }

    /// Adapter-defined Assess: the contract reports its own profit figure in
    /// its return data, and the adapter prices it with the defaulted fee half.
    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let gross = observation.first_return_word().ok_or_else(|| {
            AssessError::evidence("this contract reports profit in its return data")
        })?;
        Ok(ExecutionResult::price(
            gross,
            Vec::new(),
            observation,
            self.assess_options().min_net_profit,
            &self.fee_policy(),
        ))
    }
}

fn profit_topic() -> B256 {
    keccak256(b"Profit(address,uint256)")
}

/// An adapter whose assessment reads ONLY a captured event — the other
/// evidence shape, also without any declared probe read.
struct EventAdapter;

impl ExecutionAdapter for EventAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::new(
            PREPAY_ROUTER,
            prepay_calldata(1),
            PREPAY,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        no_probes()
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let event = observation
            .events
            .iter()
            .find(|event| event.topics.first() == Some(&profit_topic()))
            .ok_or_else(|| AssessError::evidence("no Profit event was captured"))?;
        let data = event
            .data
            .get(..32)
            .ok_or_else(|| AssessError::evidence("Profit event data is not one full word"))?;
        Ok(ExecutionResult::price(
            U256::from_be_slice(data),
            Vec::new(),
            observation,
            U256::ZERO,
            &self.fee_policy(),
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Encode — one complete transaction
// ════════════════════════════════════════════════════════════════════════════

/// Pins: Encode yields `{ to, data, value }` in one value, with the adapter's
/// own target and a nonzero native value that survives the neutral request.
#[test]
fn foreign_adapter_returns_a_complete_transaction_with_nonzero_value() {
    let adapter = PrepayAdapter::new();
    let transaction = encode_with(&adapter);

    assert_eq!(transaction.to, PREPAY_ROUTER);
    assert_eq!(transaction.value, PREPAY);
    assert_ne!(transaction.value, U256::ZERO, "the prepayment must survive");
    assert_eq!(&transaction.data[..4], &prepay_selector());
    assert_eq!(transaction.data.len(), 4 + 16);
}

/// Pins: the foreign adapter declares its own observation target. Its probe is
/// a credit ledger in its own system — nothing forces a settlement-token
/// balance read, a pool manager, or a command stream onto the seam.
#[test]
fn foreign_adapter_declares_its_own_observation_target() {
    let adapter = PrepayAdapter::new();
    let probes = adapter.probe_spec();

    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].label, "CREDIT");
    assert_eq!(probes[0].address, Some(CREDIT_LEDGER));
    assert_ne!(probes[0].address, Some(PREPAY_ROUTER));
}

// ════════════════════════════════════════════════════════════════════════════
// Assess — typed observation, adapter-owned interpretation
// ════════════════════════════════════════════════════════════════════════════

/// Pins: an adapter assesses typed return data with no probe read at all, so
/// the sum-of-deltas shape is a default and never a requirement. Gas and the
/// priority fee are priced from the observation through the fee half of Assess.
#[test]
fn custom_assessment_reads_return_data_instead_of_probe_deltas() {
    let adapter = ReturnDataAdapter;
    let profit = 20_000_000_000_000_000u128; // 0.02 ETH
    let observation = observation_of(ExecutionStatus::Success).with_return_data(word(profit));

    let result = adapter
        .assess(&observation)
        .expect("return data is present");

    assert!(result.deltas.is_empty(), "no probe delta was consulted");
    assert_eq!(result.gross_profit, U256::from(profit));
    let gas_cost = U256::from(observation.gas_used)
        * (U256::from(observation.base_fee_next) + U256::from(FeePolicy::builtin_priority_fee()));
    assert_eq!(result.net_profit, U256::from(profit) - gas_cost);
    assert_eq!(result.gas_used, observation.gas_used);
    assert_eq!(result.base_fee_next, observation.base_fee_next);
    assert!(result.passed);
}

/// Pins: an adapter that needs return data fails loudly when the observation
/// carries none — `AssessError::Evidence`, never a silent zero-profit pass.
#[test]
fn custom_assessment_without_required_return_data_is_an_evidence_error() {
    let adapter = ReturnDataAdapter;
    let observation = observation_of(ExecutionStatus::Success);

    let error = adapter.assess(&observation).expect_err("no return data");
    assert!(matches!(error, AssessError::Evidence(_)), "{error:?}");
}

/// Pins: the shared pricing helper enforces the terminal-status invariant, so a
/// custom assessment that decodes profitable-looking return data from a
/// REVERTED transaction still declines: a revert moved no value, whatever the
/// payload bytes appear to say. The observed gas and fee facts stay intact.
#[test]
fn custom_assessment_of_a_revert_cannot_pass_on_return_data() {
    let adapter = ReturnDataAdapter;
    let profitable = 20_000_000_000_000_000u128; // looks like 0.02 ETH
    let reverted = observation_of(ExecutionStatus::Revert).with_return_data(word(profitable));

    let result = adapter.assess(&reverted).expect("the return data decodes");

    assert_eq!(result.gross_profit, U256::ZERO, "a revert earns nothing");
    assert_eq!(result.net_profit, U256::ZERO);
    assert!(
        !result.passed,
        "a reverted transaction never passes the gate"
    );
    assert_eq!(result.gas_used, reverted.gas_used);
    assert_eq!(result.base_fee_next, reverted.base_fee_next);
}

/// Pins: the same invariant for event-shaped evidence — a captured `Profit`
/// event on a reverted call is not profit. The helper, not the adapter, owns
/// this check, so a future adapter cannot reintroduce the hole.
#[test]
fn custom_assessment_of_a_revert_cannot_pass_on_event_evidence() {
    let adapter = EventAdapter;
    let profitable = 7_000_000_000_000_000u128;
    let event = CapturedEvent {
        address: CREDIT_LEDGER,
        topics: vec![profit_topic()],
        data: word(profitable),
        log_index: 3,
    };
    let reverted = observation_of(ExecutionStatus::Revert).with_events(vec![event]);

    let result = adapter
        .assess(&reverted)
        .expect("the Profit event is present");

    assert_eq!(result.gross_profit, U256::ZERO, "a revert earns nothing");
    assert_eq!(result.net_profit, U256::ZERO);
    assert!(
        !result.passed,
        "a reverted transaction never passes the gate"
    );
    assert_eq!(result.gas_used, reverted.gas_used);
    assert_eq!(result.base_fee_next, reverted.base_fee_next);
}

/// Pins: captured events are first-class assessment evidence — an event-shaped
/// adapter needs neither probe deltas nor return data.
#[test]
fn custom_assessment_reads_a_captured_event() {
    let adapter = EventAdapter;
    let credited = 7_000_000_000_000_000u128;
    let mut event = CapturedEvent {
        address: CREDIT_LEDGER,
        topics: vec![profit_topic()],
        data: word(credited),
        log_index: 3,
    };
    let observation = observation_of(ExecutionStatus::Success).with_events(vec![event.clone()]);

    let result = adapter
        .assess(&observation)
        .expect("the Profit event is present");
    assert_eq!(result.gross_profit, U256::from(credited));
    assert!(result.deltas.is_empty());

    // A different event (or none) is missing evidence, not a zero-profit pass.
    event.topics = vec![keccak256(b"Unrelated(uint256)")];
    let without_profit = observation_of(ExecutionStatus::Success).with_events(vec![event]);
    let error = adapter
        .assess(&without_profit)
        .expect_err("the Profit event is absent");
    assert!(matches!(error, AssessError::Evidence(_)), "{error:?}");
}

/// Pins: the defaulted Assess shape is sum-of-deltas over declared probe reads,
/// and it requires a complete before/after reading for every probe — an
/// unavailable read is an evidence error naming the probe.
#[test]
fn default_sum_of_deltas_assessment_requires_every_probe_read() {
    let options = AssessOptions::default();
    let fee = FeePolicy::default();
    let spec = ProbeSpec::new(
        "CREDIT".to_string(),
        Some(CREDIT_LEDGER),
        [0xde, 0xad, 0xbe, 0xef],
    );

    let incomplete =
        observation_of(ExecutionStatus::Success).with_probe_reads(vec![ProbeReading::new(
            spec.clone(),
            Some(U256::from(100u64)),
            None,
        )]);
    let error = options
        .assess(&incomplete, &fee)
        .expect_err("the after-reading is unavailable");
    assert!(matches!(error, AssessError::Evidence(_)), "{error:?}");
    if let AssessError::Evidence(message) = error {
        assert!(message.contains("CREDIT"), "{message}");
    }

    let complete =
        observation_of(ExecutionStatus::Success).with_probe_reads(vec![ProbeReading::new(
            spec,
            Some(U256::from(100u64)),
            Some(U256::from(160u64)),
        )]);
    let result = options.assess(&complete, &fee).expect("a complete reading");
    assert_eq!(result.gross_profit, U256::from(60u64));
    assert_eq!(result.deltas, vec![60i128]);
}

/// Pins: the built-in return-data shape reads the first 32-byte word of the
/// transaction's return data and applies the min-net-profit threshold; absent
/// or short return data is an evidence error.
#[test]
fn default_return_data_rule_reads_the_first_word() {
    let options = AssessOptions {
        rule: AssessRule::ReturnData,
        min_net_profit: U256::from(1_000_000_000_000_000u64),
    };
    let fee = FeePolicy::default();

    let observation =
        observation_of(ExecutionStatus::Success).with_return_data(word(20_000_000_000_000_000));
    let result = options.assess(&observation, &fee).expect("return data");
    assert_eq!(result.gross_profit, U256::from(20_000_000_000_000_000u64));
    assert!(result.passed, "net clears the configured minimum");

    let below_minimum = AssessOptions {
        rule: AssessRule::ReturnData,
        min_net_profit: U256::from(20_000_000_000_000_001u64),
    };
    let declined = below_minimum
        .assess(&observation, &fee)
        .expect("return data");
    assert!(!declined.passed, "net below the minimum declines");

    let absent = observation_of(ExecutionStatus::Success);
    let error = options
        .assess(&absent, &fee)
        .expect_err("no return data captured");
    assert!(matches!(error, AssessError::Evidence(_)), "{error:?}");

    let short =
        observation_of(ExecutionStatus::Success).with_return_data(Bytes::from(vec![0u8; 16]));
    let error = options
        .assess(&short, &fee)
        .expect_err("return data is shorter than one word");
    assert!(matches!(error, AssessError::Evidence(_)), "{error:?}");
}

/// Pins: a reverted observation has no profit to assess — the defaulted stance
/// declines with a zero gross while preserving the observed gas and base fee,
/// and it is a verdict, not an assessment error.
#[test]
fn reverted_observation_declines_with_a_zero_gross() {
    let options = AssessOptions::default();
    let fee = FeePolicy::default();
    let reverted =
        observation_of(ExecutionStatus::Revert).with_return_data(Bytes::from(vec![0u8; 4]));

    let result = options
        .assess(&reverted, &fee)
        .expect("a revert is assessable evidence");
    assert!(!result.passed);
    assert_eq!(result.gross_profit, U256::ZERO);
    assert_eq!(result.net_profit, U256::ZERO);
    assert_eq!(result.gas_used, reverted.gas_used);
    assert_eq!(result.base_fee_next, reverted.base_fee_next);
}

/// Pins: the fee half prices every assessment from the observation
/// (`net = gross - gas * (base_fee_next + priority_fee)`), a policy override
/// replaces the market default, and a loss below gas cost is a FAIL with zero
/// net, not a zero-profit pass.
#[test]
fn assessment_prices_the_observation_gas_and_fee_policy() {
    let fee = FeePolicy {
        market_percentile: false,
        override_priority_fee: 2_000_000_000,
    };
    let options = AssessOptions::default();
    let gross = 1_000_000_000_000_000u128;
    let observation =
        observation_of(ExecutionStatus::Success).with_probe_reads(vec![ProbeReading::new(
            ProbeSpec::new(
                "CREDIT".to_string(),
                Some(CREDIT_LEDGER),
                [0xde, 0xad, 0xbe, 0xef],
            ),
            Some(U256::ZERO),
            Some(U256::from(gross)),
        )]);

    let result = options
        .assess(&observation, &fee)
        .expect("a complete reading");
    let gas_cost = U256::from(observation.gas_used)
        * (U256::from(observation.base_fee_next) + U256::from(2_000_000_000u64));
    assert_eq!(result.priority_fee, 2_000_000_000);
    assert_eq!(result.net_profit, U256::from(gross) - gas_cost);
    assert!(result.passed);

    // A gross smaller than the gas cost cannot clear the gate.
    let loss = observation_of(ExecutionStatus::Success).with_probe_reads(vec![ProbeReading::new(
        ProbeSpec::new(
            "CREDIT".to_string(),
            Some(CREDIT_LEDGER),
            [0xde, 0xad, 0xbe, 0xef],
        ),
        Some(U256::ZERO),
        Some(U256::from(1u64)),
    )]);
    let result = options.assess(&loss, &fee).expect("a complete reading");
    assert!(!result.passed);
    assert_eq!(result.net_profit, U256::ZERO);
}

/// Pins: observation evidence availability. A probe reading exposes a delta
/// only when both edges were observed; return data is available only when the
/// engine captured it; events and status are always present.
#[test]
fn observation_reports_evidence_availability() {
    let spec = ProbeSpec::new(
        "CREDIT".to_string(),
        Some(CREDIT_LEDGER),
        [0xde, 0xad, 0xbe, 0xef],
    );
    assert_eq!(ProbeReading::new(spec.clone(), None, None).delta(), None);
    assert_eq!(
        ProbeReading::new(spec.clone(), Some(U256::from(5u64)), None).delta(),
        None
    );
    assert_eq!(
        ProbeReading::new(spec, Some(U256::from(7u64)), Some(U256::from(5u64))).delta(),
        Some(-2)
    );

    let bare = observation_of(ExecutionStatus::Success);
    assert!(bare.return_data.is_none());
    assert!(bare.events.is_empty());
    assert!(bare.probe_reads.is_empty());
    assert!(bare.succeeded());
    assert!(!observation_of(ExecutionStatus::Revert).succeeded());
}

// ════════════════════════════════════════════════════════════════════════════
// The encoding-only route keeps its bytes, with an explicit binding
// ════════════════════════════════════════════════════════════════════════════

/// A bytes-only composer: it owns calldata, not a target or a value.
struct PrepayComposer;

impl PayloadComposer for PrepayComposer {
    fn compose(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<Bytes, ComposeError> {
        Ok(prepay_calldata(inputs.optimal_input))
    }
}

/// Pins: the encoding-only route yields bytes and nothing else — a payload
/// alone does not name a target or carry value. Binding it is an explicit act.
#[test]
fn payload_composer_alone_carries_no_target_or_value() {
    let (hop_outputs, consumed) = sample_inputs();
    let inputs = ComposerInputs {
        optimal_input: hop_outputs[0],
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: degenbot_execution::ComposeOptions,
    };
    let payload = PrepayComposer
        .compose(&PathInfo::new(Vec::new()), &inputs)
        .expect("encode");
    assert_eq!(payload, prepay_calldata(hop_outputs[0]));

    // The binding supplies the target and the native value.
    let bound = PayloadAdapter::new(PrepayComposer, PREPAY_ROUTER).with_value(PREPAY);
    assert_eq!(bound.to(), PREPAY_ROUTER);
    assert_eq!(bound.value(), PREPAY);

    let transaction = encode_with(&bound);
    assert_eq!(transaction.to, PREPAY_ROUTER);
    assert_eq!(transaction.value, PREPAY);
    assert_eq!(transaction.data, payload);
}
