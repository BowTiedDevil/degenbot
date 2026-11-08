#![expect(clippy::expect_used)]

//! Exercise the `degenbot-execution` seam (ADR-025): the value types, the
//! `PayloadComposer` Encode part and its `PayloadAdapter` binding, and the
//! `ExecutionAdapter` defaults over a typed `ExecutionObservation`.
//!
//! These pin the seam's own contract so the `PyO3` lift and the foreign-contract
//! samples build on the shape without re-deriving it.

use alloy::primitives::{address, Address, Bytes, U256};

use degenbot_execution::{
    AssessOptions, AssessRule, ComposeError, ComposeOptions, ComposerInputs, ExecutionAdapter,
    ExecutionObservation, ExecutionStatus, FeePolicy, PathInfo, PayloadAdapter, PayloadComposer,
    ProbeReading, ProbeSpec, SolveResult, TransactionRequest,
};

const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL: Address = address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");

/// A fake foreign Encode part — produces a short, deterministic (non-ABI)
/// payload byte string unique to this strategy, distinct from `cmd_executor`.
#[derive(Clone, Debug)]
struct ForeignComposer;

impl PayloadComposer for ForeignComposer {
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        // A deliberately simple, contract-agnostic encoding: concatenate the
        // per-hop outputs and prepend the optimal input, tagged with "FX".
        let mut out = b"FX".to_vec();
        out.extend_from_slice(&inputs.optimal_input.to_be_bytes());
        for h in inputs.hop_outputs {
            out.extend_from_slice(&h.to_be_bytes());
        }
        let _ = path; // hop descriptors are unused by this toy composer
        Ok(Bytes::from(out))
    }
}

fn sample_inputs(hop_outputs: &[u128]) -> ComposerInputs<'_> {
    ComposerInputs {
        optimal_input: hop_outputs[0],
        hop_outputs,
        consumed_inputs: hop_outputs,
        opts: ComposeOptions,
    }
}

#[test]
fn payload_composer_produces_distinct_foreign_bytes() {
    let path = PathInfo::new(vec![]);
    let hop_outputs = [1u128, 2u128];
    let inputs = sample_inputs(&hop_outputs);

    let bytes = ForeignComposer.compose(&path, &inputs).expect("encode");

    // Distinct from any cmd_executor output: starts with the "FX" tag.
    assert_eq!(&bytes[..2], b"FX");
    assert_eq!(bytes.len(), 2 + 16 * 3); // tag + optimal_input + 2 outputs (u128 = 16 bytes)
}

#[test]
fn payload_adapter_binds_a_composer_to_a_complete_transaction() {
    // An encoding-only composer meets the full `ExecutionAdapter` seam through
    // the binding, which names the target and the native value the bytes
    // cannot carry. Defaults: no probes, sum-of-deltas assessment, market fee.
    let hop_outputs = [7u128, 11u128];
    let inputs = sample_inputs(&hop_outputs);
    let bound = PayloadAdapter::new(ForeignComposer, POOL).with_value(U256::from(3u64));

    let transaction = ExecutionAdapter::encode(&bound, &PathInfo::new(vec![]), &inputs)
        .expect("encode through the binding");

    assert_eq!(
        transaction,
        TransactionRequest::new(
            POOL,
            ForeignComposer
                .compose(&PathInfo::new(vec![]), &inputs)
                .expect("encode"),
            U256::from(3u64),
        )
    );
    assert!(bound.probe_spec().is_empty());
    assert_eq!(bound.assess_options(), AssessOptions::default());
    assert_eq!(bound.fee_policy(), FeePolicy::default());
}

#[test]
fn solve_result_projection_carries_amounts_and_hop_count() {
    // Build a SolvePathResult manually (the amounts ride this type, ADR-025 D4).
    let result = degenbot_pools::solve::SolvePathResult {
        optimal_input: U256::from(10u64),
        profit: U256::from(3u64),
        hop_outputs: vec![U256::from(20u64), U256::from(23u64)],
        consumed_inputs: vec![U256::from(10u64), U256::from(20u64)],
        state_nonces: vec![],
        solver_pool_states: vec![],
        ..Default::default()
    };

    // Project via a path of two V2 hops so hop_count + descriptors populate.
    let v2_hop = |i: u64| {
        degenbot_execution::HopInfo::V2(degenbot_execution::V2HopInfo {
            pool_address: address!("2222222222222222222222222222222222222222"),
            token0_address: WETH,
            token1_address: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            fee: 30,
            zfo: i == 0,
        })
    };
    let path = PathInfo::new(vec![v2_hop(0), v2_hop(1)]);

    let view = SolveResult::from_solve_path(7, &result, &path);
    assert_eq!(view.path_id, 7);
    assert_eq!(view.hop_count, 2);
    assert_eq!(view.optimal_input, U256::from(10u64));
    assert_eq!(view.hop_outputs, vec![U256::from(20u64), U256::from(23u64)]);
    assert_eq!(view.net_profit, U256::from(3u64));
    assert_eq!(view.hop_descriptors.len(), 2);
}

/// Pins: the two neutral probe forms round-trip their declarations — a contract
/// call carries its COMPLETE calldata (selector plus ABI arguments) verbatim,
/// and a native-account read names the account itself with no callee and no
/// calldata.
#[test]
fn probe_spec_declares_a_call_or_a_native_account() {
    // `balanceOf(address)` calldata: selector + the ABI argument word.
    let mut data = Vec::from([0x70, 0xa0, 0x82, 0x31]);
    data.extend_from_slice(&U256::from_be_slice(WETH.as_slice()).to_be_bytes::<32>());
    let spec = ProbeSpec::call("WETH", WETH, data.clone());
    assert_eq!(spec.label(), "WETH");
    assert_eq!(
        spec,
        ProbeSpec::Call {
            label: "WETH".to_string(),
            to: WETH,
            data: Bytes::from(data),
        }
    );

    let native = ProbeSpec::native_balance("ETH", WETH);
    assert_eq!(native.label(), "ETH");
    assert_eq!(
        native,
        ProbeSpec::NativeBalance {
            label: "ETH".to_string(),
            account: WETH,
        }
    );
}

fn observed(probe_reads: Vec<ProbeReading>) -> ExecutionObservation {
    ExecutionObservation::new(
        TransactionRequest::new(POOL, Bytes::from_static(b"FX"), U256::ZERO),
        ExecutionStatus::Success,
        100_000,
        10,
    )
    .with_probe_reads(probe_reads)
}

#[test]
fn default_assessment_sums_probe_deltas_and_prices_the_market_fee() {
    // Realistic arb scale: gross = Σ deltas = 1 ETH (1e18 wei); default
    // market-percentile priority fee is 1 gwei; gas = 100k at 10 wei/gas base
    // → gas_cost ≈ 1e14 wei ≪ gross, so net > 0 → passes the zero threshold.
    let spec = ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31]);
    let observation = observed(vec![
        ProbeReading::new(
            spec.clone(),
            Some(U256::from(0u64)),
            Some(U256::from(500_000_000_000_000_000u64)),
        ),
        ProbeReading::new(
            spec,
            Some(U256::from(0u64)),
            Some(U256::from(500_000_000_000_000_000u64)),
        ),
    ]);

    let result = AssessOptions::default()
        .assess(&observation, &FeePolicy::default())
        .expect("complete readings");

    assert_eq!(
        result.gross_profit,
        U256::from(1_000_000_000_000_000_000u128)
    );
    assert_eq!(
        result.net_profit,
        U256::from(1_000_000_000_000_000_000u128 - 100_000 * (10 + 1_000_000_000))
    );
    assert!(result.passed); // net > 0 ≥ min_net_profit (0)
    assert_eq!(
        FeePolicy::default().priority_fee(),
        FeePolicy::builtin_priority_fee()
    );
}

#[test]
fn assess_rule_and_options_defaults() {
    assert_eq!(AssessRule::default(), AssessRule::SumOfDeltas);
    let opts = AssessOptions::default();
    assert_eq!(opts.rule, AssessRule::SumOfDeltas);
    assert_eq!(opts.min_net_profit, U256::ZERO);
}

#[test]
fn assess_breaks_even_at_exactly_zero_net() {
    // gross == gas cost → net == 0 exactly, and the default 0 threshold passes
    // it (a true break-even, distinct from an underflowing loss).
    let base: u128 = 10;
    let priority: u128 = FeePolicy::builtin_priority_fee();
    let gas_cost = 10_000 * (base + priority);
    let gas_cost_i128 = i128::try_from(gas_cost).expect("fits");
    let deltas = match gas_cost % 2 {
        0 => [gas_cost_i128 / 2, gas_cost_i128 / 2],
        _ => [gas_cost_i128 / 2, gas_cost_i128 - gas_cost_i128 / 2],
    };
    let spec = ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31]);
    let observation = ExecutionObservation::new(
        TransactionRequest::new(POOL, Bytes::from_static(b"FX"), U256::ZERO),
        ExecutionStatus::Success,
        10_000,
        base,
    )
    .with_probe_reads(vec![
        ProbeReading::new(
            spec.clone(),
            Some(U256::from(0u64)),
            Some(U256::from(deltas[0].unsigned_abs())),
        ),
        ProbeReading::new(
            spec,
            Some(U256::from(0u64)),
            Some(U256::from(deltas[1].unsigned_abs())),
        ),
    ]);

    let result = AssessOptions::default()
        .assess(&observation, &FeePolicy::default())
        .expect("complete readings");

    assert!(result.passed);
    assert_eq!(result.net_profit, U256::ZERO);
}
