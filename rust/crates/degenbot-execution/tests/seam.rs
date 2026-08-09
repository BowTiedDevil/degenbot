#![expect(clippy::expect_used)]
//! Exercise the `degenbot-execution` scaffold seam (ADR-025): the value types,
//! the `PayloadComposer` Encode part, and the `ExecutionStrategy` trait that
//! wraps it with built-in Probe/Assess/Fee defaults.
//!
//! These pin the scaffold contract so downstream tasks (facet A and facet B, the
//! `PyO3` lift, and the sample foreign-contract strategies) can build on the shape
//! without re-deriving it.

use alloy::primitives::{address, Address, Bytes, U256};

use degenbot_execution::{
    AssessRule, ComposeError, ComposeOptions, ComposerInputs, ExecutionStrategy, FeePolicy,
    PayloadComposer, ProbeSpec, SolveResult,
};
use degenbot_executor::composers::PathInfo;

const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

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

#[test]
fn payload_composer_produces_distinct_foreign_bytes() {
    let path = PathInfo::new(vec![]);
    let hop_outputs = [1u128, 2u128];
    let inputs = ComposerInputs {
        optimal_input: 42,
        hop_outputs: &hop_outputs,
        consumed_inputs: &hop_outputs,
        opts: ComposeOptions,
    };
    let bytes = ForeignComposer.compose(&path, &inputs).expect("encode");
    // Distinct from any cmd_executor output: starts with the "FX" tag.
    assert_eq!(&bytes[..2], b"FX");
    assert_eq!(bytes.len(), 2 + 16 * 3); // tag + optimal_input + 2 outputs (u128 = 16 bytes)
}

#[test]
fn payload_composer_blanket_satisfies_execution_strategy() {
    // A `PayloadComposer` meets the full `ExecutionStrategy` seam through the
    // blanket impl (built-in Probe/Assess/Fee defaults) — matching the docs'
    // "impl PayloadComposer" Rust path.
    let path = PathInfo::new(vec![]);
    let hop_outputs = [7u128, 11u128];
    let inputs = ComposerInputs {
        optimal_input: 5,
        hop_outputs: &hop_outputs,
        consumed_inputs: &hop_outputs,
        opts: ComposeOptions,
    };
    let composer = ForeignComposer;
    let encoded = ExecutionStrategy::encode(&composer, &path, &inputs).expect("encode");
    // Blanket defaults: sum-of-deltas probe list is empty; fee is market-percentile.
    assert!(composer.probe_spec().is_empty());
    assert_eq!(composer.fee_policy(), FeePolicy::default());
    assert!(!encoded.is_empty());
}

#[test]
fn solve_result_projection_carries_amounts_and_hop_count() {
    // A 2-hop path: optimal input + per-hop outputs + consumed inputs.
    let path = PathInfo::new(vec![]);
    let _ = path;

    // Build a SolvePathResult manually (the amounts ride this type, ADR-025 D4).
    let result = degenbot_solvers::mixed::SolvePathResult {
        optimal_input: U256::from(10u64),
        profit: U256::from(3u64),
        hop_outputs: vec![U256::from(20u64), U256::from(23u64)],
        consumed_inputs: vec![U256::from(10u64), U256::from(20u64)],
        state_nonces: vec![],
        solver_pool_states: vec![],
    };

    // Project via a path of two V2 hops so hop_count + descriptors populate.
    let v2_hop = |i: u64| {
        degenbot_executor::composers::HopInfo::V2(degenbot_executor::composers::V2HopInfo {
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
    assert_eq!(view.optimal_input, U256::from(10u64));
    assert_eq!(view.net_profit, U256::from(3u64));
    assert_eq!(view.hop_descriptors.len(), 2);
}

#[test]
fn probe_spec_roundtrips_label_address_selector() {
    let spec = ProbeSpec::new("WETH".to_string(), Some(WETH), [0x70, 0xa0, 0x82, 0x31]);
    assert_eq!(spec.label, "WETH");
    assert_eq!(spec.address, Some(WETH));
    assert_eq!(spec.selector, [0x70, 0xa0, 0x82, 0x31]);
    // Native-ETH pseudo-read has no address.
    let native = ProbeSpec::new("ETH".to_string(), None, [0x47, 0xd5, 0x1f, 0x2d]);
    assert_eq!(native.address, None);
}

#[test]
fn assess_defaults_use_sum_of_deltas_and_market_percentile_fee() {
    let composer = ForeignComposer;
    // Realistic arb scale: gross = Σ deltas = 1 ETH (1e18 wei); default
    // market-percentile priority fee is 1 gwei; gas = 100k at 10 wei/gas base
    // → gas_cost ≈ 1e14 wei ≪ gross, so net > 0 → passes the zero threshold.
    let result = composer.assess(
        &[500_000_000_000_000_000, 500_000_000_000_000_000],
        100_000,
        10,
    );
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
        composer.fee_policy().priority_fee(),
        FeePolicy::builtin_priority_fee()
    );

    // A heavy loss (gross below gas cost) is a fail — not a zero-profit pass.
    let loss = composer.assess(&[10_000], 100_000, 10);
    assert!(!loss.passed);
    assert_eq!(loss.net_profit, U256::ZERO);
}

#[test]
fn assess_rule_and_options_defaults() {
    assert_eq!(AssessRule::default(), AssessRule::SumOfDeltas);
    let opts = degenbot_execution::AssessOptions::default();
    assert_eq!(opts.rule, Some(AssessRule::SumOfDeltas));
    assert_eq!(opts.min_net_profit, U256::ZERO);
}

#[test]
fn assess_breaks_even_at_exactly_zero_net() {
    // gross == gas cost → net == 0 exactly, and the default 0 threshold passes
    // it (a true break-even, distinct from an underflowing loss).
    let composer = ForeignComposer;
    let base: u128 = 10;
    let priority: u128 = FeePolicy::builtin_priority_fee();
    let gas_cost = 10_000 * (base + priority);
    let gas_cost_i128 = i128::try_from(gas_cost).expect("fits");
    let deltas = match gas_cost % 2 {
        0 => [gas_cost_i128 / 2, gas_cost_i128 / 2],
        _ => [gas_cost_i128 / 2, gas_cost_i128 - gas_cost_i128 / 2],
    };
    let result = composer.assess(&deltas, 10_000, base);
    assert!(result.passed);
    assert_eq!(result.net_profit, U256::ZERO);
}
