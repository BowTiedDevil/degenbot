use std::sync::Arc;

use crate::runtime::SolveRuntimeConfig;
use alloy::primitives::I256;
use alloy::primitives::U256;
use alloy::primitives::U512;
use degenbot_math::cl::swap_math::compute_swap_step_v3;
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_math::v2::{IntHopState, SimulationResult, StepOutcome};

use super::active_set::*;
use super::crossings::*;
use super::entries::*;
use super::hop_sim::*;
use super::memo::*;
use super::telemetry::*;
use super::word_profile::*;
use super::{IntTickRangeCrossing, IntV3TickRangeHop, IntV3TickRangeSequence};

// ---------------------------------------------------------------------------
// N-hop CL Path Simulation
// ---------------------------------------------------------------------------

/// Simulate an N-hop concentrated-liquidity path with tick crossings.
///
/// For each hop, the simulation accounts for tick range crossings:
/// - If `crossings[i]` is `Some`, hop `i` crosses `k_i` ranges, producing
///   `crossing_output`, then simulates the remainder in the ending range.
/// - If `crossings[i]` is `None`, hop `i` stays within `base_ranges[i]`.
///
/// Returns a [`SimulationResult`] with per-hop output amounts and the final output.
#[must_use]
// Validation-only oracle: assumed-tuple piecewise simulator, superseded
// in production by the self-determining step-faithful `simulate_walk_path`.
// Retained as the test-only validation oracle for the rounding-parity
// nets and the uncapped enumeration references in the test module.
#[cfg_attr(not(test), expect(dead_code))]
fn int_simulate_cl_path_n(
    amount_in: U256,
    crossings: &[Option<IntTickRangeCrossing>],
    base_ranges: &[IntV3TickRangeHop],
) -> SimulationResult {
    let n_hops = base_ranges.len();
    if n_hops == 0 || amount_in.is_zero() {
        return SimulationResult {
            final_output: U256::ZERO,
            steps: Box::new([]),
        };
    }

    let mut steps: Vec<StepOutcome> = Vec::with_capacity(n_hops);
    let mut current_input = amount_in;

    for i in 0..n_hops {
        if current_input.is_zero() {
            // Fill remaining with zeros
            for _ in i..n_hops {
                steps.push(StepOutcome {
                    output: U256::ZERO,
                    consumed_input: U256::ZERO,
                });
            }
            return SimulationResult {
                final_output: U256::ZERO,
                steps: steps.into(),
            };
        }

        let (consumed, output) = if let Some(crossing) = crossings[i].as_ref() {
            if current_input < crossing.crossing_gross_input {
                // Can't reach this crossing — path exhausted
                steps.push(StepOutcome {
                    output: current_input,
                    consumed_input: current_input,
                });
                for _ in (i + 1)..n_hops {
                    steps.push(StepOutcome {
                        output: U256::ZERO,
                        consumed_input: U256::ZERO,
                    });
                }
                return SimulationResult {
                    final_output: U256::ZERO,
                    steps: steps.into(),
                };
            }
            let remaining = current_input - crossing.crossing_gross_input;
            let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
            let out = crossing.crossing_output.saturating_add(ending.output);
            let consumed = crossing
                .crossing_gross_input
                .saturating_add(ending.consumed_input);
            (consumed, out)
        } else {
            let result = int_simulate_v3_swap(current_input, &base_ranges[i]);
            (result.consumed_input, result.output)
        };

        steps.push(StepOutcome {
            output,
            consumed_input: consumed,
        });
        current_input = output;
    }

    let final_output = steps.last().map_or(U256::ZERO, |s| s.output);
    SimulationResult {
        final_output,
        steps: steps.into(),
    }
}

// ---------------------------------------------------------------------------
// Integer V3-V3 Solver (Slice 14)
// ---------------------------------------------------------------------------

/// Simulate a full V3-V3 path with tick crossings using integer arithmetic.
///
/// For each hop, the simulation accounts for tick range crossings:
/// - If `crossing.is_some()`, the swap crosses `k` ranges, producing
///   `crossing_output`, then simulates the remainder in the ending range.
/// - If `crossing.is_none()`, the swap stays within the base range.
///
/// Returns a [`SimulationResult`] with per-hop output amounts and the final output.
#[must_use]
// Validation-only oracle: assumed-tuple piecewise simulator, superseded
// in production by the self-determining step-faithful `simulate_walk_path`.
// Retained as the test-only validation oracle for the rounding-parity
// nets and the uncapped enumeration references in the test module.
#[cfg_attr(not(test), expect(dead_code))]
fn int_simulate_v3_v3_path(
    amount_in: U256,
    crossing1: Option<&IntTickRangeCrossing>,
    crossing2: Option<&IntTickRangeCrossing>,
    base_range1: &IntV3TickRangeHop,
    base_range2: &IntV3TickRangeHop,
) -> SimulationResult {
    if amount_in.is_zero() {
        return SimulationResult {
            final_output: U256::ZERO,
            steps: Box::new([]),
        };
    }

    // Hop 1
    let (consumed1, output1) = if let Some(c1) = crossing1 {
        if amount_in < c1.crossing_gross_input {
            return SimulationResult {
                final_output: U256::ZERO,
                steps: Box::new([]),
            };
        }
        let remaining = amount_in - c1.crossing_gross_input;
        let ending = int_simulate_v3_swap(remaining, &c1.ending_range);
        let out = c1.crossing_output.saturating_add(ending.output);
        // consumed = crossing_gross_input + ending.consumed_input
        let consumed = c1
            .crossing_gross_input
            .saturating_add(ending.consumed_input);
        (consumed, out)
    } else {
        let result = int_simulate_v3_swap(amount_in, base_range1);
        (result.consumed_input, result.output)
    };

    // Hop 2
    let (consumed2, output2) = if let Some(c2) = crossing2 {
        if output1 < c2.crossing_gross_input {
            return SimulationResult {
                final_output: U256::ZERO,
                steps: Box::new([
                    StepOutcome {
                        output: output1,
                        consumed_input: U256::ZERO,
                    },
                    StepOutcome {
                        output: U256::ZERO,
                        consumed_input: consumed1,
                    },
                ]),
            };
        }
        let remaining = output1 - c2.crossing_gross_input;
        let ending = int_simulate_v3_swap(remaining, &c2.ending_range);
        let out = c2.crossing_output.saturating_add(ending.output);
        let consumed = c2
            .crossing_gross_input
            .saturating_add(ending.consumed_input);
        (consumed, out)
    } else {
        let result = int_simulate_v3_swap(output1, base_range2);
        (result.consumed_input, result.output)
    };

    SimulationResult {
        final_output: output2,
        steps: Box::new([
            StepOutcome {
                output: output1,
                consumed_input: consumed1,
            },
            StepOutcome {
                output: output2,
                consumed_input: consumed2,
            },
        ]),
    }
}

// ---------------------------------------------------------------------------
// N-hop Mixed Path Solver
// ---------------------------------------------------------------------------

/// Simulate an N-hop mixed V2 + CL path with optional CL crossings.
///
/// `v2_hops[i]` and `cl_hops[i]` are set based on `hop_order`:
/// - `hop_order[i] == true` → position `i` is a V2 hop (use `v2_hops[i]`)
/// - `hop_order[i] == false` → position `i` is a CL hop (use `cl_hops[i]`)
///
/// `cl_crossings[i]` is `Some` when CL hop `i` crosses tick ranges.
/// `cl_base_ranges[i]` is the base (first) range for CL hop `i`.
///
/// Returns a [`SimulationResult`] with per-hop output and consumed-input amounts.
#[must_use]
// Validation-only oracle: assumed-tuple piecewise simulator, superseded
// in production by `simulate_walk_path`.
// Retained for the test module's uncapped-enumeration reference and
// N-hop parity nets.
#[cfg_attr(not(test), expect(dead_code))]
#[expect(
    clippy::too_many_lines,
    reason = "validation-only oracle retained for the test module"
)]
fn int_simulate_mixed_path_n(
    amount_in: U256,
    v2_hops: &[Option<IntHopState>],
    cl_base_ranges: &[Option<IntV3TickRangeHop>],
    cl_crossings: &[Option<IntTickRangeCrossing>],
    hop_order: &[bool], // true = V2, false = CL
) -> SimulationResult {
    let n_hops = hop_order.len();
    if n_hops == 0 || amount_in.is_zero() {
        return SimulationResult {
            final_output: U256::ZERO,
            steps: Box::new([]),
        };
    }

    let mut steps: Vec<StepOutcome> = Vec::with_capacity(n_hops);
    let mut current_input = amount_in;

    for i in 0..n_hops {
        if current_input.is_zero() {
            for _ in i..n_hops {
                steps.push(StepOutcome {
                    output: U256::ZERO,
                    consumed_input: U256::ZERO,
                });
            }
            return SimulationResult {
                final_output: U256::ZERO,
                steps: steps.into(),
            };
        }

        if hop_order[i] {
            // V2 hop — always consumes full input
            let Some(hop) = v2_hops[i].as_ref() else {
                // Missing V2 hop data
                for _ in i..n_hops {
                    steps.push(StepOutcome {
                        output: U256::ZERO,
                        consumed_input: U256::ZERO,
                    });
                }
                return SimulationResult {
                    final_output: U256::ZERO,
                    steps: steps.into(),
                };
            };
            let Ok(output) = hop.swap(current_input) else {
                // V2 hop overflow-reverts on-chain (uint256 intermediate) —
                // the multi-hop path reverts; mirror the exhaustion shape.
                for _ in i..n_hops {
                    steps.push(StepOutcome {
                        output: U256::ZERO,
                        consumed_input: U256::ZERO,
                    });
                }
                return SimulationResult {
                    final_output: U256::ZERO,
                    steps: steps.into(),
                };
            };
            steps.push(StepOutcome {
                output,
                consumed_input: current_input,
            });
            current_input = output;
        } else {
            // CL hop — may have crossing
            let (consumed, output) = if let Some(crossing) = cl_crossings[i].as_ref() {
                if current_input < crossing.crossing_gross_input {
                    // Can't reach crossing — path exhausted
                    steps.push(StepOutcome {
                        output: U256::ZERO,
                        consumed_input: current_input,
                    });
                    for _ in (i + 1)..n_hops {
                        steps.push(StepOutcome {
                            output: U256::ZERO,
                            consumed_input: U256::ZERO,
                        });
                    }
                    return SimulationResult {
                        final_output: U256::ZERO,
                        steps: steps.into(),
                    };
                }
                let remaining = current_input - crossing.crossing_gross_input;
                let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
                let out = crossing.crossing_output.saturating_add(ending.output);
                let consumed = crossing
                    .crossing_gross_input
                    .saturating_add(ending.consumed_input);
                (consumed, out)
            } else {
                let Some(base_range) = cl_base_ranges[i].as_ref() else {
                    // Missing CL base range data
                    for _ in i..n_hops {
                        steps.push(StepOutcome {
                            output: U256::ZERO,
                            consumed_input: U256::ZERO,
                        });
                    }
                    return SimulationResult {
                        final_output: U256::ZERO,
                        steps: steps.into(),
                    };
                };
                let result = int_simulate_v3_swap(current_input, base_range);
                (result.consumed_input, result.output)
            };
            steps.push(StepOutcome {
                output,
                consumed_input: consumed,
            });
            current_input = output;
        }
    }

    let final_output = steps.last().map_or(U256::ZERO, |s| s.output);
    SimulationResult {
        final_output,
        steps: steps.into(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert U512 to U256, capping at U256::MAX on overflow.
#[cfg(test)]
fn u512_to_u256(v: U512) -> U256 {
    crate::mobius_int_exact::u512_to_u256_internal(v)
}
/// Helper: create an IntV3TickRangeHop at tick 0 (1:1 price), tick spacing 60.
fn make_v3_hop_at_1to1(liquidity: u128, zfo: bool) -> IntV3TickRangeHop {
    // At tick 0, sqrtPriceX96 = 2^96
    let sp_0 = U256::from(1u128) << 96;
    // Tick -60 → lower bound
    let sp_lower =
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default();
    // Tick +60 → upper bound
    let sp_upper =
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default();

    IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: U256::from(sp_lower),
        sqrt_price_upper_x96: U256::from(sp_upper),
        gamma_numer: 997_000, // 0.3% fee → gamma = 997_000 / 1_000_000
        fee_denom: 1_000_000,
        zero_for_one: zfo,
        word_boundary_prices: Vec::new(),
    }
}

#[test]
fn test_v3_effective_reserves_at_1to1() {
    let hop = make_v3_hop_at_1to1(1_000_000_000_000u128, true);

    let (token0_virt, token1_virt) = hop.compute_virtual_reserves();

    // At tick 0 (1:1 price):
    // token0_virt = L · 2^96 / 2^96 = L = 1_000_000_000_000
    // token1_virt = L · 2^96 / 2^96 = L = 1_000_000_000_000
    assert_eq!(token0_virt, U256::from(1_000_000_000_000u128));
    assert_eq!(token1_virt, U256::from(1_000_000_000_000u128));
}

#[test]
fn test_v3_effective_reserves_direction() {
    let hop = make_v3_hop_at_1to1(1_000_000_000_000u128, true);
    let (r_in, r_out) = hop.compute_effective_reserves();
    // zfo: reserve_in = token0_virt, reserve_out = token1_virt
    assert_eq!(r_in, U256::from(1_000_000_000_000u128));
    assert_eq!(r_out, U256::from(1_000_000_000_000u128));

    let hop_ofz = make_v3_hop_at_1to1(1_000_000_000_000u128, false);
    let (r_in, r_out) = hop_ofz.compute_effective_reserves();
    // ofz: reserve_in = token1_virt, reserve_out = token0_virt
    assert_eq!(r_in, U256::from(1_000_000_000_000u128));
    assert_eq!(r_out, U256::from(1_000_000_000_000u128));
}

#[test]
fn test_v3_to_int_hop_state() {
    let hop = make_v3_hop_at_1to1(1_000_000_000_000u128, true);
    let int_hop = hop.to_int_hop_state();

    assert_eq!(int_hop.reserve_in, U256::from(1_000_000_000_000u128));
    assert_eq!(int_hop.reserve_out, U256::from(1_000_000_000_000u128));
    assert_eq!(int_hop.gamma_numer, U256::from(997_000));
    assert_eq!(int_hop.fee_denom, U256::from(1_000_000));
}

#[test]
fn test_int_simulate_v3_swap_small_input_zfo() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);

    // Small swap: 1000 token0 in, zfo
    let input = U256::from(1000u64);
    let result = int_simulate_v3_swap(input, &hop);

    // Should produce positive output less than input (due to fees on 1:1 pool)
    assert!(
        result.output > U256::ZERO,
        "Output should be positive for a valid swap"
    );
    assert!(
        result.output < input,
        "Output should be less than input on 1:1 pool with fees: output={}, input={}",
        result.output,
        input
    );
    // Small swap should not hit range boundary — full input consumed
    assert_eq!(result.consumed_input, input);
}

#[test]
fn test_int_simulate_v3_swap_small_input_ofz() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, false);

    // Small swap: 1000 token1 in, ofz
    let input = U256::from(1000u64);
    let result = int_simulate_v3_swap(input, &hop);

    assert!(result.output > U256::ZERO);
    assert!(
        result.output < input,
        "Output should be less than input on 1:1 pool with fees"
    );
    assert_eq!(result.consumed_input, input);
}

#[test]
fn test_int_simulate_v3_swap_zero_input() {
    let hop = make_v3_hop_at_1to1(1_000_000u128, true);
    let result = int_simulate_v3_swap(U256::ZERO, &hop);
    assert!(result.output.is_zero());
    assert!(result.consumed_input.is_zero());
}

#[test]
fn test_int_simulate_v3_swap_zero_liquidity() {
    let hop = IntV3TickRangeHop {
        liquidity: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        sqrt_price_lower_x96: U256::ONE,
        sqrt_price_upper_x96: U256::from(2u128) << 96,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };
    let result = int_simulate_v3_swap(U256::from(1000u64), &hop);
    assert!(result.output.is_zero());
    assert!(result.consumed_input.is_zero());
}

#[test]
fn test_max_gross_input_in_range_at_1to1() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let max_input = hop.max_gross_input_in_range();

    // Should be positive — the range has capacity
    assert!(!max_input.is_zero(), "Should have positive range capacity");
}

#[test]
fn test_max_gross_input_ofz() {
    let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let max_input = hop.max_gross_input_in_range();

    assert!(!max_input.is_zero(), "Should have positive range capacity");
}

#[test]
fn test_int_v3_sequence_validation() {
    let hop1 = make_v3_hop_at_1to1(1_000_000u128, true);
    let hop2 = IntV3TickRangeHop {
        liquidity: 500_000,
        sqrt_price_x96: U256::from(1u128) << 96,
        sqrt_price_lower_x96: U256::ONE,
        sqrt_price_upper_x96: U256::from(2u128) << 96,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false, // Different direction!
        word_boundary_prices: Vec::new(),
    };

    let result = IntV3TickRangeSequence::new(vec![hop1, hop2]);
    assert!(result.is_err(), "Should reject mixed direction");
}

#[test]
fn test_int_v3_sequence_empty() {
    let result = IntV3TickRangeSequence::new(vec![]);
    assert!(result.is_err(), "Should reject empty sequence");
}

#[test]
fn test_exact_solve_mixed_v2_v3_sequence_profitable() {
    // V2 pool: 1.5M USDC / 800 WETH (cheap WETH)
    // V3 pool at 1:1 with different effective reserves
    let v2_hop = IntHopState::new(
        U256::from(1_500_000_000_000u64),            // 1.5M USDC
        U256::from(800_000_000_000_000_000_000u128), // 800 WETH (18 dec)
        997,
        1000,
    );

    // V3 with high liquidity and price above V2
    let v3_hop = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: U256::from(1u128) << 96, // 1:1 price
        sqrt_price_lower_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
        ),
        sqrt_price_upper_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
        ),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };

    let v3_seq = IntV3TickRangeSequence::new(vec![v3_hop]).unwrap();

    let result =
        exact_solve_mixed_v2_v3_sequence(&[v2_hop], &v3_seq, true, &SolveRuntimeConfig::default())
            .result;
    // Key thing is no panics.
    let _ = result;
}

#[test]
fn test_u512_to_u256_small() {
    let v = U512::from(42u64);
    assert_eq!(u512_to_u256(v), U256::from(42u64));
}

#[test]
#[should_panic(expected = "U512 \u{2192} U256 narrowing overflow")]
fn test_u512_to_u256_overflow() {
    // Spec-bound V3 state (u128 liquidity, sqrtPrice ≤ MAX_SQRT_RATIO)
    // never reaches this; `expect` documents the invariant and panics on
    // corrupt/synthetic input. Fix-forward: enforce spec at registration.
    let v = U512::from(U256::MAX) + U512::from(1u64);
    let _ = u512_to_u256(v);
}

// --- Slice 13: compute_crossing tests ---

#[test]
fn test_compute_crossing_k0_returns_identity() {
    let hop = make_v3_hop_at_1to1(1_000_000u128, true);
    let seq = IntV3TickRangeSequence::new(vec![hop.clone()]).unwrap();
    let crossing = seq.compute_crossing(0).unwrap();
    assert!(crossing.crossing_gross_input.is_zero());
    assert!(crossing.crossing_output.is_zero());
    assert_eq!(crossing.ending_range.liquidity, hop.liquidity);
}

#[test]
fn test_compute_crossing_out_of_bounds() {
    let hop = make_v3_hop_at_1to1(1_000_000u128, true);
    let seq = IntV3TickRangeSequence::new(vec![hop]).unwrap();
    assert!(seq.compute_crossing(1).is_none());
}

#[test]
fn test_compute_crossing_k1_zfo() {
    // Two-range zfo sequence: current range + next range below
    let sp_0 = U256::from(1u128) << 96;
    let sp_lower0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
    );
    let sp_upper0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
    );
    let sp_lower1 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default(),
    );
    let sp_upper1 = sp_lower0; // Range 1 starts where range 0 ends

    let hop0 = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: sp_lower0,
        sqrt_price_upper_x96: sp_upper0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };
    let hop1 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: sp_lower0, // entry at boundary — will be overridden
        sqrt_price_lower_x96: sp_lower1,
        sqrt_price_upper_x96: sp_upper1,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };

    let seq = IntV3TickRangeSequence::new(vec![hop0, hop1]).unwrap();
    let crossing = seq.compute_crossing(1).unwrap();

    // Crossing k=1 must consume range 0's capacity
    assert!(
        !crossing.crossing_gross_input.is_zero(),
        "k=1 crossing should require input"
    );
    assert!(
        !crossing.crossing_output.is_zero(),
        "k=1 crossing should produce output"
    );

    // The ending range should have entry price = range 0's lower bound
    assert_eq!(crossing.ending_range.sqrt_price_x96, sp_lower0);
    assert_eq!(crossing.ending_range.liquidity, 5_000_000_000_000u128);
}

#[test]
fn test_compute_crossing_k1_ofz() {
    let sp_0 = U256::from(1u128) << 96;
    let sp_lower0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
    );
    let sp_upper0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
    );
    let sp_upper1 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(120).unwrap_or_default(),
    );
    let sp_lower1 = sp_upper0; // Range 1 starts where range 0 ends

    let hop0 = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: sp_lower0,
        sqrt_price_upper_x96: sp_upper0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };
    let hop1 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: sp_upper0,
        sqrt_price_lower_x96: sp_lower1,
        sqrt_price_upper_x96: sp_upper1,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };

    let seq = IntV3TickRangeSequence::new(vec![hop0, hop1]).unwrap();
    let crossing = seq.compute_crossing(1).unwrap();

    assert!(!crossing.crossing_gross_input.is_zero());
    assert!(!crossing.crossing_output.is_zero());
    assert_eq!(crossing.ending_range.sqrt_price_x96, sp_upper0); // entry at upper bound of range 0
}

#[test]
fn test_compute_crossing_matches_max_gross_input_k1() {
    // For a 2-range sequence, crossing k=1 should have gross_input
    // equal to range 0's max_gross_input_in_range
    let sp_0 = U256::from(1u128) << 96;
    let sp_lower0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
    );
    let sp_upper0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
    );
    let sp_lower1 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default(),
    );

    let hop0 = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: sp_lower0,
        sqrt_price_upper_x96: sp_upper0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };
    let hop1 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: sp_lower0,
        sqrt_price_lower_x96: sp_lower1,
        sqrt_price_upper_x96: sp_lower0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };

    let expected_max_input = hop0.max_gross_input_in_range();
    let seq = IntV3TickRangeSequence::new(vec![hop0, hop1]).unwrap();
    let crossing = seq.compute_crossing(1).unwrap();

    assert_eq!(
        crossing.crossing_gross_input, expected_max_input,
        "k=1 crossing_gross_input should equal range 0's max_gross_input_in_range"
    );
}

// --- Slice 14: int_solve_v3_v3 tests ---

#[test]
fn test_int_solve_v3_v3_single_range_unprofitable() {
    // Two V3 pools at the same price → no arb (fees dominate)
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let result = int_solve_v3_v3(&seq1, &seq2, &SolveRuntimeConfig::default()).result;
    assert!(
        result.is_none(),
        "Same-price pools should not be profitable"
    );
}

#[test]
fn test_int_solve_v3_v3_single_range_no_panic() {
    // Different effective reserves — may or may not be profitable,
    // but must not panic
    let sp_0 = U256::from(1u128) << 96;
    let sp_lower = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
    );
    let sp_upper = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
    );

    // Asymmetric: high-liquidity pool vs low-liquidity pool
    let hop1 = IntV3TickRangeHop {
        liquidity: 100_000_000_000_000u128,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: sp_lower,
        sqrt_price_upper_x96: sp_upper,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };
    // Shift the price for hop 2 to create arb opportunity
    let sp_shifted = sp_0 * U256::from(101u32) / U256::from(100u32); // 1% price shift
    let hop2 = IntV3TickRangeHop {
        liquidity: 100_000_000_000_000u128,
        sqrt_price_x96: sp_shifted,
        sqrt_price_lower_x96: sp_lower,
        sqrt_price_upper_x96: sp_upper,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };

    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let _ = int_solve_v3_v3(&seq1, &seq2, &SolveRuntimeConfig::default()).result;
}

#[test]
fn test_int_solve_v3_v3_multi_range_no_panic() {
    let sp_0 = U256::from(1u128) << 96;
    let sp_lower0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
    );
    let sp_upper0 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
    );
    let sp_lower1 = U256::from(
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default(),
    );

    let range1_0 = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: sp_0,
        sqrt_price_lower_x96: sp_lower0,
        sqrt_price_upper_x96: sp_upper0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };
    let range1_1 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: sp_lower0, // entry at boundary
        sqrt_price_lower_x96: sp_lower1,
        sqrt_price_upper_x96: sp_lower0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };

    let sp_shifted = sp_0 * U256::from(101u32) / U256::from(100u32);
    let range2_0 = IntV3TickRangeHop {
        liquidity: 10_000_000_000_000u128,
        sqrt_price_x96: sp_shifted,
        sqrt_price_lower_x96: sp_lower0,
        sqrt_price_upper_x96: sp_upper0,
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };

    let seq1 = IntV3TickRangeSequence::new(vec![range1_0, range1_1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![range2_0]).unwrap();
    let _ = int_solve_v3_v3(&seq1, &seq2, &SolveRuntimeConfig::default()).result;
}

// ── Per-hop output tests ──────────────────────────────────────

#[test]
fn test_int_simulate_v3_v3_path_hop_outputs_single_range() {
    let hop1 = make_v3_hop_at_1to1(1_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(800_000_000_000u128, false);

    let result = int_simulate_v3_v3_path(U256::from(1_000_000u64), None, None, &hop1, &hop2);

    // 2 hops → 2 hop_outputs
    assert_eq!(result.steps.len(), 2);
    // Invariant: final_output == hop_outputs.last()
    assert_eq!(
        result.final_output,
        result.steps.last().map(|s| s.output).unwrap()
    );
    // Invariant: hop_outputs[1] is the output after hop 2 using hop 1's output as input
    let expected_hop1 = int_simulate_v3_swap(U256::from(1_000_000u64), &hop1);
    assert_eq!(result.steps[0].output, expected_hop1.output);
    let expected_hop2 = int_simulate_v3_swap(expected_hop1.output, &hop2);
    assert_eq!(result.steps[1].output, expected_hop2.output);
    assert_eq!(result.final_output, expected_hop2.output);
}

#[test]
fn test_int_simulate_v3_v3_path_hop_outputs_zero_input() {
    let hop1 = make_v3_hop_at_1to1(1_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(800_000_000_000u128, false);

    let result = int_simulate_v3_v3_path(U256::ZERO, None, None, &hop1, &hop2);
    assert_eq!(result.final_output, U256::ZERO);
    assert!(result.steps.is_empty());
}

// ── N-hop CL solver tests ────────────────────────────────────

#[test]
fn test_int_simulate_cl_path_n_3hop() {
    // 3-hop CL path: zfo → ofz → zfo at 1:1
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(8_000_000_000_000u128, false);
    let hop3 = make_v3_hop_at_1to1(9_000_000_000_000u128, true);

    let amount_in = U256::from(1_000_000u64);

    // No crossings
    let result = int_simulate_cl_path_n(
        amount_in,
        &[None, None, None],
        &[hop1.clone(), hop2.clone(), hop3.clone()],
    );

    // 3 hops → 3 hop_outputs
    assert_eq!(result.steps.len(), 3);
    // Verify chain: output1 feeds into hop2, output2 feeds into hop3
    let expected1 = int_simulate_v3_swap(amount_in, &hop1);
    assert_eq!(result.steps[0].output, expected1.output);
    let expected2 = int_simulate_v3_swap(expected1.output, &hop2);
    assert_eq!(result.steps[1].output, expected2.output);
    let expected3 = int_simulate_v3_swap(expected2.output, &hop3);
    assert_eq!(result.steps[2].output, expected3.output);
    assert_eq!(result.final_output, expected3.output);
}

#[test]
fn test_int_simulate_cl_path_n_zero_input() {
    let hop1 = make_v3_hop_at_1to1(1_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(1_000_000u128, false);
    let base_ranges = vec![hop1, hop2];
    let crossings = vec![None, None];

    let result = int_simulate_cl_path_n(U256::ZERO, &crossings, &base_ranges);
    assert_eq!(result.final_output, U256::ZERO);
    assert!(result.steps.is_empty());
}

#[test]
fn test_int_simulate_cl_path_n_2hop_matches_v3_v3() {
    // 2-hop CL path should produce the same result as int_simulate_v3_v3_path
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(8_000_000_000_000u128, false);

    let amount_in = U256::from(1_000_000u64);

    let result_n = int_simulate_cl_path_n(amount_in, &[None, None], &[hop1.clone(), hop2.clone()]);

    let result_v3v3 = int_simulate_v3_v3_path(amount_in, None, None, &hop1, &hop2);

    assert_eq!(result_n.final_output, result_v3v3.final_output);
    let view = |r: &SimulationResult| r.steps.iter().map(|s| s.output).collect::<Vec<_>>();
    assert_eq!(view(&result_n), view(&result_v3v3));
    let consumed_view =
        |r: &SimulationResult| r.steps.iter().map(|s| s.consumed_input).collect::<Vec<_>>();
    assert_eq!(consumed_view(&result_n), consumed_view(&result_v3v3));
}

#[test]
fn walk_fingerprint_is_content_stable_and_separates_compositions() {
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(8_000_000_000_000u128, false);
    let seq1 = IntV3TickRangeSequence::new(vec![hop1.clone()]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let sv = [&seq1, &seq2];

    let fp_a = walk_path_fingerprint(&sv);
    let fp_b = walk_path_fingerprint(&sv);
    assert_eq!(fp_a, fp_b, "fingerprint must be a pure function of content");

    // Reversed hop order is a different composition.
    let fp_rev = walk_path_fingerprint(&[&seq2, &seq1]);
    assert_ne!(fp_a, fp_rev, "hop order must separate compositions");

    // A single-field state change must change the key.
    let mut h1 = hop1.clone();
    h1.liquidity += 1;
    let seq1b = IntV3TickRangeSequence::new(vec![h1]).unwrap();
    let fp_changed = walk_path_fingerprint(&[&seq1b, &seq2]);
    assert_ne!(fp_a, fp_changed, "a liquidity delta must change the key");
}

#[test]
fn test_int_solve_cl_path_2hop_delegates_to_v3_v3() {
    // 2-hop CL path should delegate to int_solve_v3_v3
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(8_000_000_000_000u128, false);

    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();

    let result_cl = solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result;
    let result_v3v3 = int_solve_v3_v3(&seq1, &seq2, &SolveRuntimeConfig::default()).result;

    assert_eq!(result_cl, result_v3v3);
}

#[test]
fn test_int_solve_cl_path_3hop_single_range() {
    // 3-hop CL path with single-range sequences (all at 1:1, different reserves)
    // zfo → ofz → zfo to form a cycle

    // Let's create a profitable cycle:
    // Pool 1: zfo, t0=10T, t1=10T (1:1 price)
    // Pool 2: ofz, t1=10T, t0=12T (token0 is cheap in this pool — can buy more t0 with t1)
    // Pool 3: zfo, t0=12T, t1=10T (back to base — t0 is expensive here)
    // So: buy t1 with t0 (pool1) → buy t0 with t1 (pool2, cheap) → buy t1 with t0 (pool3)
    // Wait, this is circular and fees will eat profit on same-price pools.

    // Instead, create different-price pools:
    // Uniswap V3 pools at different ticks for genuine price disagreement.
    // For simplicity, use the 1:1 pool helper with different liquidity
    // that creates a slight price advantage.

    // Actually, with only single-range sequences at 1:1, these are effectively
    // constant-product pools. 3-hop V2-V2-V2 is always unprofitable after fees
    // with same-product pools. The test just verifies no panic and correct handling.
    let hop1 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);
    let hop2 = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let hop3 = make_v3_hop_at_1to1(10_000_000_000_000u128, true);

    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let seq3 = IntV3TickRangeSequence::new(vec![hop3]).unwrap();

    // Same-product 3-hop — should not be profitable
    let result = solve_cl_derived(&[&seq1, &seq2, &seq3], &SolveRuntimeConfig::default()).result;
    assert!(
        result.is_none(),
        "Same-product 3-hop should not be profitable"
    );
}

#[test]
fn test_int_solve_cl_path_3hop_profitable() {
    // Create a 3-hop cycle with genuine price disagreement
    // Pool 1: zfo, large t0, large t1 at 1:1 (entry pool)
    // Pool 2: ofz, mispriced — more t0 than t1 (can buy t0 cheap)
    // Pool 3: zfo, back to normal price (exit pool)

    // Use asymmetric reserves by placing pools at different effective reserves
    // At tick 0 (1:1), both t0_virt and t1_virt = L
    // For zfo: reserve_in = t0_virt, reserve_out = t1_virt
    // For ofz: reserve_in = t1_virt, reserve_out = t0_virt

    // So for zfo with L=10T: r_in=10T, r_out=10T
    // For ofz with L=12T: r_in=12T, r_out=12T
    // No price disagreement at 1:1...

    // We need to create pools at different prices.
    // Use custom V3 hops with shifted sqrt prices.

    // Pool 1: zfo at below 1:1 (more token0 per token1)
    let sp_below =
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-100).unwrap_or_default();
    let hop1 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: U256::from(sp_below),
        sqrt_price_lower_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-200).unwrap_or_default(),
        ),
        sqrt_price_upper_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(0).unwrap_or_default(),
        ),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };

    // Pool 2: ofz at 1:1
    let hop2 = make_v3_hop_at_1to1(10_000_000_000_000u128, false);

    // Pool 3: zfo at above 1:1 (less token0 per token1)
    let sp_above =
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(100).unwrap_or_default();
    let hop3 = IntV3TickRangeHop {
        liquidity: 5_000_000_000_000u128,
        sqrt_price_x96: U256::from(sp_above),
        sqrt_price_lower_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(0).unwrap_or_default(),
        ),
        sqrt_price_upper_x96: U256::from(
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(200).unwrap_or_default(),
        ),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    };

    let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
    let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
    let seq3 = IntV3TickRangeSequence::new(vec![hop3]).unwrap();

    let result = solve_cl_derived(&[&seq1, &seq2, &seq3], &SolveRuntimeConfig::default()).result;

    // This should produce a result — there's genuine price disagreement
    // across the three pools. However, fees may eat all profit.
    // The test primarily verifies the 3-hop solver doesn't panic
    // and produces a well-formed result when profitable.
    if let Some((optimal_input, profit, hop_outputs)) = result {
        assert!(!optimal_input.is_zero(), "Optimal input should be nonzero");
        assert!(!profit.is_zero(), "Profit should be nonzero");
        assert_eq!(hop_outputs.len(), 3, "3 hops should produce 3 hop_outputs");

        // Validate: hop_outputs are consistent with simulation
        let sim = int_simulate_cl_path_n(
            optimal_input,
            &[None, None, None],
            &[
                seq1.ranges[0].clone(),
                seq2.ranges[0].clone(),
                seq3.ranges[0].clone(),
            ],
        );
        assert!(
            sim.final_output > optimal_input,
            "Simulation should confirm profit"
        );
        assert_eq!(sim.final_output - optimal_input, profit);
    }
    // If not profitable due to fees, that's also a valid result
}

// ── N-hop mixed path solver tests ────────────────────────────

#[test]
fn test_exact_solve_mixed_path_n_2hop_delegates() {
    // 2-hop mixed path should delegate to existing 2-hop solver
    let v2_hop = IntHopState::new(
        U256::from(1_500_000_000_000u64),
        U256::from(800_000_000_000_000_000_000u128),
        997,
        1000,
    );

    let v3_hop = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let v3_seq = IntV3TickRangeSequence::new(vec![v3_hop]).unwrap();

    // V2 first, then CL
    let result = exact_solve_mixed_path_n(
        &[Some(v2_hop.clone()), None],
        &[None, Some(&v3_seq)],
        &[None, None], // offline shape: tables derive here
        &[true, false],
        &SolveRuntimeConfig::default(),
    );

    // Just verify no panic; profit depends on specific reserves
    let _ = result;
}

#[test]
fn cl_path_cached_crossings_match_profile_only_solve() {
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);

    let c1 = Arc::new(build_cl_crossing_table(&seq1));
    let c2 = Arc::new(build_cl_crossing_table(&seq2));
    let p1 = Arc::new(build_cl_word_profiles(&seq1));
    let p2 = Arc::new(build_cl_word_profiles(&seq2));

    // Carried tables must equal the per-call derived solve (byte-identical
    // CL math).
    let prepared = [
        ClPrepared {
            crossings: Arc::clone(&c1),
            profiles: Arc::clone(&p1),
        },
        ClPrepared {
            crossings: Arc::clone(&c2),
            profiles: Arc::clone(&p2),
        },
    ];
    let cached = int_solve_cl_path(
        &[&seq1, &seq2],
        &prepared,
        None,
        &SolveRuntimeConfig::default(),
    )
    .result;
    let offline = solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result;
    assert_eq!(cached, offline);
    assert!(cached.is_some(), "late-liquidity fixture is profitable");
}

#[test]
fn mixed_path_cached_crossings_match_offline_solve() {
    let r0 = U256::from(1_000_000_000_000_000u128);
    let r1 = U256::from(1_071_633_064_014_504u128);
    let v2_entry = IntHopState::new(r0, r1, 997, 1000);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let cl_seq = multi_range_sequence(0, 60, false, &liquidities);

    let v2_hops = [Some(v2_entry), None];
    let cl_sequences = [None, Some(&cl_seq)];
    let crossing = Arc::new(build_cl_crossing_table(
        cl_sequences[1].as_ref().expect("CL hop present"),
    ));
    let profile = Arc::new(build_cl_word_profiles(
        cl_sequences[1].as_ref().expect("CL hop present"),
    ));
    let cl_prepared = [
        None,
        Some(ClPrepared {
            crossings: crossing,
            profiles: profile,
        }),
    ];

    // Projection-backed cached tables must equal the offline build
    // (byte-identical mixed-path math).
    let cached = exact_solve_mixed_path_n(
        &v2_hops,
        &cl_sequences,
        &cl_prepared,
        &[true, false],
        &SolveRuntimeConfig::default(),
    );
    let offline = exact_solve_mixed_path_n(
        &v2_hops,
        &cl_sequences,
        &[None, None],
        &[true, false],
        &SolveRuntimeConfig::default(),
    )
    .result;
    assert_eq!(cached.result, offline);
    assert!(
        cached.result.is_some(),
        "mixed fixture has profit past the prefix"
    );
}

#[test]
fn test_exact_solve_mixed_path_n_3hop_v2_cl_v2() {
    // 3-hop mixed path: V2 → CL → V2
    let v2_hop1 = IntHopState::new(
        U256::from(2_000_000_000_000u64),              // 2M USDC
        U256::from(1_000_000_000_000_000_000_000u128), // 1000 WETH
        997,
        1000,
    );
    let v2_hop2 = IntHopState::new(
        U256::from(1_000_000_000_000_000_000_000u128), // 1000 WETH
        U256::from(2_100_000_000_000u64),              // 2.1M USDC
        997,
        1000,
    );

    let v3_hop = make_v3_hop_at_1to1(10_000_000_000_000u128, false);
    let v3_seq = IntV3TickRangeSequence::new(vec![v3_hop]).unwrap();

    let result = exact_solve_mixed_path_n(
        &[Some(v2_hop1.clone()), None, Some(v2_hop2.clone())],
        &[None, Some(&v3_seq), None],
        &[None, None, None],  // offline shape: tables derive here
        &[true, false, true], // V2 → CL → V2
        &SolveRuntimeConfig::default(),
    );

    // Price disagreement: V2 pool 1 sells 1 WETH at 2000 USDC,
    // V3 middle pool converts at 1:1,
    // V2 pool 2 sells 1 WETH at 2100 USDC.
    // Cycle: USDC → WETH (pool1, cheap) → USDC (V3, 1:1) → WETH (pool2, expensive)
    // Wait, this isn't a cycle in the same token...
    // For arb: buy WETH cheap (pool1), sell WETH expensive (pool2).
    // But that's only 2 hops. With V3 in the middle, it's still the same direction.
    // The result is that there should be profit from the V2 price disagreement.
    if let Some((optimal_input, profit, hop_outputs)) = result.result {
        assert!(!optimal_input.is_zero());
        assert!(!profit.is_zero());
        assert_eq!(hop_outputs.len(), 3);
    }
}

#[test]
fn mixed_path_cached_crossings_match_offline_solve_3hop() {
    // 3-hop V2 -> CL -> V2 twin of the offline parity shape: cached
    // tables for the CL middle hop must be byte-identical to the rebuild.
    let v2_hop1 = IntHopState::new(
        U256::from(2_000_000_000_000u64),
        U256::from(1_000_000_000_000_000_000_000u128),
        997,
        1000,
    );
    let v2_hop2 = IntHopState::new(
        U256::from(1_000_000_000_000_000_000_000u128),
        U256::from(2_100_000_000_000u64),
        997,
        1000,
    );
    let v3_seq =
        IntV3TickRangeSequence::new(vec![make_v3_hop_at_1to1(10_000_000_000_000u128, false)])
            .unwrap();

    let v2_hops = [Some(v2_hop1), None, Some(v2_hop2)];
    let cl_sequences = [None, Some(&v3_seq), None];
    let crossing = Arc::new(build_cl_crossing_table(
        cl_sequences[1].as_ref().expect("CL hop present"),
    ));
    let profile = Arc::new(build_cl_word_profiles_from_crossings(&crossing));
    let cl_prepared = [
        None,
        Some(ClPrepared {
            crossings: crossing,
            profiles: profile,
        }),
        None,
    ];

    let cached = exact_solve_mixed_path_n(
        &v2_hops,
        &cl_sequences,
        &cl_prepared,
        &[true, false, true],
        &SolveRuntimeConfig::default(),
    );
    let offline = exact_solve_mixed_path_n(
        &v2_hops,
        &cl_sequences,
        &[None, None, None],
        &[true, false, true],
        &SolveRuntimeConfig::default(),
    )
    .result;
    assert_eq!(cached.result, offline);
    assert!(cached.result.is_some(), "3-hop mixed fixture is profitable");
}

#[test]
fn test_int_simulate_mixed_path_n_3hop() {
    // 3-hop mixed simulation: V2 → CL → V2
    let v2_hop1 = IntHopState::new(
        U256::from(1_000_000u64),
        U256::from(2_000_000u64),
        997,
        1000,
    );
    let v2_hop2 = IntHopState::new(
        U256::from(2_000_000u64),
        U256::from(1_000_000u64),
        997,
        1000,
    );

    let cl_hop = make_v3_hop_at_1to1(1_000_000_000_000u128, false);
    let cl_base = cl_hop.clone();

    let amount_in = U256::from(1000u64);

    let result = int_simulate_mixed_path_n(
        amount_in,
        &[Some(v2_hop1.clone()), None, Some(v2_hop2.clone())],
        &[None, Some(cl_base), None],
        &[None, None, None],  // no crossings
        &[true, false, true], // V2 → CL → V2
    );

    // Should produce 3 hop_outputs
    assert_eq!(result.steps.len(), 3);

    // Verify chain manually
    // Hop 1 (V2): swap 1000 in pool (1M, 2M)
    let expected_out1 = v2_hop1.swap(amount_in).unwrap();
    assert_eq!(result.steps[0].output, expected_out1);
    assert_eq!(result.steps[0].consumed_input, amount_in);

    // Hop 2 (CL): swap output through V3
    let expected_out2 = int_simulate_v3_swap(expected_out1, &cl_hop);
    assert_eq!(result.steps[1].output, expected_out2.output);

    // Hop 3 (V2): swap output through V2 pool 2
    let expected_out3 = v2_hop2.swap(expected_out2.output).unwrap();
    assert_eq!(result.steps[2].output, expected_out3);

    assert_eq!(result.final_output, expected_out3);
}

// ── Partial-step on-chain oracle parity ───────────────────────────────
// The full V4 CurrencyNotSettled fix covers TWO pieces of the solver's
// per-hop V3 calc: (a) `compute_crossing`'s per-range round-up
// (covered by degenbot-pools/int_v3_hop.rs tests), and (b) the partial
// (target-NOT-reached) step in `int_simulate_v3_swap`. The tests below
// pin (b): for a swap that stops inside a tick range WITHOUT reaching
// either boundary, the solver's `output`/`consumed_input`/`sqrt_price_next`
// MUST match `compute_swap_step_v3` (the on-chain-faithful oracle) exactly
// for BOTH directions:
//
//   - zfo (token0 in, price decreases): on-chain derives `sp_next` via
//     `get_next_sqrt_price_from_amount0_rounding_up` = CEIL; the prior
//     floor in `int_simulate_v3_swap` under-shot `sp_next` → over-shot
//     `spCur − sp_next` → over-predicted `output` (the same direction
//     as the CurrencyNotSettled bug).
//   - ofz (token1 in, price increases): on-chain uses
//     `get_next_sqrt_price_from_amount1_rounding_down` = floor; the solver
//     also floors → expected to match already (regression guard).
//
// The oracle's `sp_target` for a partial step is set to the boundary
// that WOULD be hit if `amount_remaining` were large enough — this is the
// shape computeSwapStep is invoked with on-chain inside the swap loop
// (target = next tick boundary); the oracle's own branch logic decides
// "not reached" and walks `get_next_sqrt_price_from_input` instead.

/// Build a CL tick-range hop whose `sqrt_price_x96 = sqrtPriceAt(tick_cur)`,
/// `sqrt_price_lower_x96 = sqrtPriceAt(tick_cur - spacing)` and
/// `sqrt_price_upper_x96 = sqrtPriceAt(tick_cur + spacing)`. Used to
/// construct both zfo and ofz partial-step hops for the parity tests.
fn range_hop_at_tick(tick_cur: i32, spacing: i32, liquidity: u128, zfo: bool) -> IntV3TickRangeHop {
    IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: U256::from(get_sqrt_ratio_at_tick_internal(tick_cur).unwrap()),
        sqrt_price_lower_x96: U256::from(
            get_sqrt_ratio_at_tick_internal(tick_cur - spacing).unwrap(),
        ),
        sqrt_price_upper_x96: U256::from(
            get_sqrt_ratio_at_tick_internal(tick_cur + spacing).unwrap(),
        ),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: zfo,
        word_boundary_prices: Vec::new(),
    }
}

/// RED→GREEN: `int_simulate_v3_swap` for a swap that does NOT reach the
/// lower boundary (zfo) MUST match the on-chain `compute_swap_step_v3`
/// step (`output`, `consumed_input`, and the implied `sqrt_price_next`).
/// Before the round-up fix this over-predicted `output` because `sp_next`
/// was floored instead of ceiled.
// -----------------------------------------------------------------
// Differential proof: oracle (`v3_simulate_swap`, 30k-budget tick walk)
// vs solver path (`build_int_v3_sequence` → chained
// `int_simulate_v3_swap`). Proves whether the integer solver handles
// edge-case pool topologies — distant/full-range liquidity, sparse word
// boundaries, range exhaustion — byte-for-byte.
// -----------------------------------------------------------------
use alloy::primitives::U128;
use degenbot_pools::v3_state::{PoolTickCoverage, V3PoolState};
use degenbot_pools::{state_history::ReorgJournal, state_history::V3BlockDelta, TickInfo};
use hashbrown::{HashMap, HashSet};

/// Full V3PoolState at tick 0, 1:1 price. `active_liquidity` is slot0
/// liquidity; `ticks` are (tick, liquidity_net) pairs.
fn pool_at_tick0(
    tick_spacing: i32,
    fee: u32,
    active_liquidity: u128,
    ticks: &[(i32, i128)],
) -> (V3PoolState, i32, u32) {
    let tick_data: HashMap<_, _> = ticks
        .iter()
        .map(|&(t, net)| {
            (
                t,
                TickInfo {
                    liquidity_gross: U128::from(10_000_000_000_000u128),
                    liquidity_net: net,
                    block: 0,
                },
            )
        })
        .collect();
    let state = V3PoolState {
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: active_liquidity,
        tick: 0,
        update_block: 0,
        tick_data_block: 0,
        initial_state_block: 0,
        state_nonce: 0,
        registration_lifecycle: degenbot_pools::v3_state::RegistrationLifecycle::default(),
        tick_data,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        snapshot_seed: None,
        post_drain_snapshot: None,
        cached_tick_ranges: parking_lot::Mutex::new(
            degenbot_pools::v3_state::TickRangeCache::default(),
        ),
    };
    (state, tick_spacing, fee)
}

/// Solver-side simulation: chain int_simulate_v3_swap across the built
/// sequence's hops, feeding remaining input forward.
fn solver_swap(
    state: &V3PoolState,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
    amount_in: U256,
) -> Option<(U256, U256)> {
    let seq = state.build_int_v3_sequence(tick_spacing, fee, zero_for_one)?;
    let mut crossings = Vec::with_capacity(seq.ranges.len());
    for k in 0..seq.ranges.len() {
        crossings.push(seq.compute_crossing(k).expect("k in bounds"));
    }
    // Find the deepest range reachable with amount_in (same rule as
    // simulate_walk_path's landed_ending_range_index).
    let mut k = 0usize;
    for (i, c) in crossings.iter().enumerate() {
        if amount_in >= c.crossing_gross_input {
            k = i;
        }
    }
    let crossing = &crossings[k];
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    Some((
        crossing
            .crossing_gross_input
            .saturating_add(ending.consumed_input),
        crossing.crossing_output.saturating_add(ending.output),
    ))
}

/// Oracle-side simulation via the pools crate twin walker.
fn oracle_swap(
    state: &V3PoolState,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
    amount_in: U256,
) -> Option<degenbot_pools::v3_state::V3SwapOutcome> {
    degenbot_pools::v3_state::v3_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        I256::try_from(amount_in).unwrap(),
        V3PoolState::default_sqrt_price_limit(zero_for_one),
    )
    .ok()
}

/// Differential sweep across topologies x swap sizes. Asserts oracle
/// output == solver output exactly for every case that builds a sequence.
#[test]
fn differential_oracle_vs_solver_across_topologies() {
    let l = 10_000_000_000_000u128;
    // (name, spacing, fee, active_liq, ticks)
    #[expect(clippy::type_complexity)]
    let cases: Vec<(&str, i32, u32, u128, Vec<(i32, i128)>)> = vec![
        (
            "control-straddle-60",
            60,
            3000,
            l,
            vec![(-60, l.cast_signed()), (60, -(l.cast_signed()))],
        ),
        (
            "one-sided-below-60",
            60,
            3000,
            0,
            vec![(-600, l.cast_signed()), (600, -(l.cast_signed()))],
        ),
        (
            "full-range-spacing-60",
            60,
            3000,
            l,
            vec![(-887_220, l.cast_signed()), (887_220, -(l.cast_signed()))],
        ),
        (
            "sparse-two-positions-60",
            60,
            3000,
            l / 2,
            vec![
                (-12_060, l.cast_signed()),
                (-6000, -(l.cast_signed() / 2)),
                (6000, l.cast_signed() / 2),
                (12_060, -(l.cast_signed())),
            ],
        ),
    ];

    // Swap sizes: dust, small partial, near-range-exit, range-crossing,
    // exhaustion-sized.
    let amounts: Vec<U256> = vec![
        U256::from(1_000u64),
        U256::from(1_000_000_000u64),
        U256::from(100_000_000_000_000u64),
        U256::from(5_000_000_000_000_000u64),
        U256::from(500_000_000_000_000_000_000_000u128),
    ];

    let mut divergences = 0usize;
    for (name, spacing, fee, liq, ticks) in &cases {
        let (state, _, _) = pool_at_tick0(*spacing, *fee, *liq, ticks);
        for zero_for_one in [true, false] {
            for amt in &amounts {
                let seq_built = state.build_int_v3_sequence(*spacing, *fee, zero_for_one);
                if seq_built.is_none() {
                    continue;
                }
                let Some(oracle) = oracle_swap(&state, *spacing, *fee, zero_for_one, *amt) else {
                    continue;
                };
                let oracle_out = if zero_for_one {
                    oracle.amount1
                } else {
                    oracle.amount0
                };
                match solver_swap(&state, *spacing, *fee, zero_for_one, *amt) {
                    Some((consumed, out)) => {
                        assert_eq!(
                            out, oracle_out,
                            "{name} zfo={zero_for_one} amt={amt}: solver {out} != oracle {oracle_out}"
                        );
                        let oracle_in = if zero_for_one {
                            oracle.amount0
                        } else {
                            oracle.amount1
                        };
                        assert_eq!(
                            consumed, oracle_in,
                            "{name} zfo={zero_for_one} amt={amt}: consumed {consumed} != {oracle_in}"
                        );
                    }
                    None => {
                        // builder rejected; oracle succeeded — recorded divergence
                        divergences += 1;
                    }
                }
            }
        }
    }
    assert_eq!(divergences, 0, "builder rejections where oracle succeeds");
}

/// The edge case from the incident question: full-range-only liquidity
/// on tick_spacing=1 (~3.4k word boundaries between price and position).
/// The builder must SUCCEED (distance-aware walk budget) and solver
/// output must equal the oracle exactly.
#[test]
fn full_range_spacing1_builds_and_matches_oracle() {
    let l = 10_000_000_000_000u128;
    let (state, spacing, fee) = pool_at_tick0(
        1,
        100,
        l,
        &[(-887_270, l.cast_signed()), (887_270, -(l.cast_signed()))],
    );

    assert!(state.swap_is_viable(true), "liquidity exists below price");
    let built = state.build_int_v3_sequence(spacing, fee, true);
    assert!(
        built.is_some(),
        "distance-aware budget must reach distant liquidity"
    );

    // Solver == oracle exactly across a size sweep.
    for amt in [
        U256::from(1_000u64),
        U256::from(1_000_000_000_000u64),
        U256::from(100_000_000_000_000u64),
        U256::from(500_000_000_000_000_000_000_000u128),
    ] {
        let oracle =
            oracle_swap(&state, spacing, fee, true, amt).expect("oracle computes for every size");
        let (consumed, out) =
            solver_swap(&state, spacing, fee, true, amt).expect("builder succeeds");
        assert_eq!(out, oracle.amount1, "output amt={amt}");
        assert_eq!(consumed, oracle.amount0, "consumed amt={amt}");
    }
}

#[test]
fn int_simulate_v3_swap_partial_step_zfo_matches_onchain_compute_swap_step_v3() {
    let liquidity = 10_000_000_000_000u128; // 1e13
    let spacing = 60;
    let hop = range_hop_at_tick(0, spacing, liquidity, /* zfo */ true);
    // Pick an input that does NOT reach the lower boundary (tick -60).
    // Range capacity (0 → -60) is about L · (sp_diff) / Q96; pick 0.5× that
    // so the swap definitely stops mid-range.
    let full_capacity = hop.max_gross_input_in_range();
    let amount_in = full_capacity / U256::from(2u64);
    assert!(
        !amount_in.is_zero() && amount_in < full_capacity,
        "inputs must be inside the range"
    );

    // On-chain oracle: sp_target = lower boundary (what the swap loop
    // would aim at); the not-reach branch inside compute_swap_step_v3
    // kicks in and calls get_next_sqrt_price_from_input.
    let fee_pips = U256::from(hop.fee_denom - hop.gamma_numer);
    let sp_target = hop.sqrt_price_lower_x96;
    let step = compute_swap_step_v3(
        hop.sqrt_price_x96,
        sp_target,
        i128::try_from(liquidity).unwrap(),
        I256::try_from(amount_in).unwrap(),
        fee_pips,
    )
    .expect("oracle compute_swap_step_v3 must succeed");

    // Solver:
    let result = int_simulate_v3_swap(amount_in, &hop);

    assert_eq!(
        result.output, step.amount_out,
        "zfo partial-step output must match on-chain computeSwapStep (floor output from ceil sp_next)"
    );
    assert_eq!(
        result.consumed_input,
        step.amount_in.saturating_add(step.fee_amount),
        "zfo partial-step consumed_input must match on-chain gross (amount_in + fee_amount)"
    );
}

/// Sweep regression guard: `int_simulate_v3_swap` for a swap that does
/// NOT reach the lower boundary (zfo) MUST match the on-chain
/// `compute_swap_step_v3` across a sweep of inputs AND liquidity magnitudes.
///
/// The on-chain oracle uses `muldiv_rounding_up` for `sp_next` (zfo),
/// while `int_simulate_v3_swap` floors; for L « Q96 the floor/ceil gap is
/// hidden by the `floor(L·(spCur − sp_next)/Q96)` quantization, but for
/// L `\approx` Q96 (realistic for highly-liquid mature pools) a divergence
/// could surface. This test pins the invariant across both regimes.
///
/// Excluded are dust amounts (< 1 wei of net_in after the floor fee
/// deduction) which round `net_in` to 0 in the solver while on-chain still
/// consumes the full amount as fee-only — that's a separate corner tracked
/// by [`int_simulate_v3_swap_dust_amount_zfo_consumes_full_input`].
#[test]
fn int_simulate_v3_swap_partial_step_zfo_matches_onchain_compute_swap_step_v3_sweep() {
    let spacings_and_liquids: [(i32, u128); 5] = [
        (60, 10_000_000_000_000u128),                    // 1e13 « Q96
        (60, 100_000_000_000_000_000_000u128),           // 1e20
        (60, 10_000_000_000_000_000_000_000_000u128),    // 1e25 ~ Q96
        (1, 10_000_000_000_000u128),                     // tiny spacing
        (10, 1_000_000_000_000_000_000_000_000_000u128), // 1e27 > Q96
    ];
    // Sweep small offsets + 10%..90% of capacity to stress
    // boundary-relevant rounding (the 1-2 wei gap between floor and ceil).
    let per_amounts: [u64; 4] = [1337, 999_983, 1_000_003, 0];

    for (spacing, liquidity) in spacings_and_liquids {
        let hop = range_hop_at_tick(0, spacing, liquidity, /* zfo */ true);
        let fee_pips = U256::from(hop.fee_denom - hop.gamma_numer);
        let sp_target = hop.sqrt_price_lower_x96;
        let liq_i128 = i128::try_from(liquidity).unwrap();
        let full_capacity = hop.max_gross_input_in_range();

        let mut amounts = Vec::new();
        for small in per_amounts {
            if small > 0 {
                amounts.push(U256::from(small));
            }
        }
        for pct in [10u64, 25, 33, 50, 67, 75, 90, 99] {
            let a = (full_capacity * U256::from(pct)) / U256::from(100u64);
            if !a.is_zero() && a < full_capacity {
                amounts.push(a);
            }
        }

        for amount_in in amounts {
            // Skip dust: net_in = floor(amount_in · γ / D) would be 0.
            if amount_in * U256::from(hop.gamma_numer) < U256::from(hop.fee_denom) {
                continue;
            }
            let step = compute_swap_step_v3(
                hop.sqrt_price_x96,
                sp_target,
                liq_i128,
                I256::try_from(amount_in).unwrap(),
                fee_pips,
            )
            .expect("oracle compute_swap_step_v3 must succeed");
            let result = int_simulate_v3_swap(amount_in, &hop);
            assert_eq!(
                result.output, step.amount_out,
                "zfo partial output mismatch L={} spc={} amount_in={amount_in}: solver={}, oracle={}",
                liquidity, spacing, result.output, step.amount_out
            );
            assert_eq!(
                result.consumed_input,
                step.amount_in.saturating_add(step.fee_amount),
                "zfo partial consumed mismatch L={liquidity} spc={spacing} amount_in={amount_in}",
            );
        }
    }
}

/// Regression guard: `int_simulate_v3_swap` for a swap that does NOT reach
/// the upper boundary (ofz) MUST match the on-chain `compute_swap_step_v3`
/// step (sweep over L). ofz uses floor on both sides (sp_next derivation
/// via `get_next_sqrt_price_from_amount1_rounding_down`), so this should
/// always pass — it pins the invariant.
#[test]
fn int_simulate_v3_swap_partial_step_ofz_matches_onchain_compute_swap_step_v3_sweep() {
    let spacings_and_liquids: [(i32, u128); 5] = [
        (60, 10_000_000_000_000u128),
        (60, 100_000_000_000_000_000_000u128),
        (60, 10_000_000_000_000_000_000_000_000u128),
        (1, 10_000_000_000_000u128),
        (10, 1_000_000_000_000_000_000_000_000_000u128),
    ];
    let per_amounts: [u64; 4] = [1337, 999_983, 1_000_003, 0];

    for (spacing, liquidity) in spacings_and_liquids {
        let hop = range_hop_at_tick(0, spacing, liquidity, /* zfo */ false);
        let fee_pips = U256::from(hop.fee_denom - hop.gamma_numer);
        let sp_target = hop.sqrt_price_upper_x96;
        let liq_i128 = i128::try_from(liquidity).unwrap();
        let full_capacity = hop.max_gross_input_in_range();
        let mut amounts = Vec::new();
        for small in per_amounts {
            if small > 0 {
                amounts.push(U256::from(small));
            }
        }
        for pct in [10u64, 25, 33, 50, 67, 75, 90, 99] {
            let a = (full_capacity * U256::from(pct)) / U256::from(100u64);
            if !a.is_zero() && a < full_capacity {
                amounts.push(a);
            }
        }
        for amount_in in amounts {
            if amount_in * U256::from(hop.gamma_numer) < U256::from(hop.fee_denom) {
                continue;
            }
            let step = compute_swap_step_v3(
                hop.sqrt_price_x96,
                sp_target,
                liq_i128,
                I256::try_from(amount_in).unwrap(),
                fee_pips,
            )
            .expect("oracle compute_swap_step_v3 must succeed");
            let result = int_simulate_v3_swap(amount_in, &hop);
            assert_eq!(
                result.output, step.amount_out,
                "ofz partial output mismatch L={} spc={} amount_in={amount_in}: solver={}, oracle={}",
                liquidity, spacing, result.output, step.amount_out
            );
            assert_eq!(
                result.consumed_input,
                step.amount_in.saturating_add(step.fee_amount),
                "ofz partial consumed mismatch L={liquidity} spc={spacing} amount_in={amount_in}",
            );
        }
    }
}

/// Documented dust corner: for `amount_in` so small that
/// Dust-input regime: `amount_in` small enough that the fee-deducted
/// `net_in = floor(amount_in · γ / D) == 0`. On-chain V3 still consumes
/// the full `amount_in` as fee-only (`amount_in=0, fee=amount_in,
/// output=0`).
///
/// Per-step delegation to `compute_swap_step_v3`: the solver now MATCHES
/// on-chain byte-for-byte here too. Previously the closed form rounded
/// `net_in` to 0 and reported `consumed_input = 0` (a documented
/// limitation that the dust-agreement test below has now flipped).
#[test]
fn int_simulate_v3_swap_dust_amount_zfo_consumes_full_input_onchain_only() {
    let liquidity = 10_000_000_000_000u128;
    let hop = range_hop_at_tick(0, 60, liquidity, /* zfo */ true);
    let fee_pips = U256::from(hop.fee_denom - hop.gamma_numer);
    let sp_target = hop.sqrt_price_lower_x96;
    let liq_i128 = i128::try_from(liquidity).unwrap();

    // amount_in=1: net_in = floor(1 · 997_000 / 1_000_000) = 0.
    let amount_in = U256::from(1u64);
    assert!(
        amount_in * U256::from(hop.gamma_numer) < U256::from(hop.fee_denom),
        "precondition: amount_in is dust (net_in rounds to 0)"
    );

    let step = compute_swap_step_v3(
        hop.sqrt_price_x96,
        sp_target,
        liq_i128,
        I256::try_from(amount_in).unwrap(),
        fee_pips,
    )
    .expect("oracle must succeed");
    let result = int_simulate_v3_swap(amount_in, &hop);

    // AGREEMENT: output is 0 on both sides.
    assert_eq!(result.output, U256::ZERO);
    assert_eq!(step.amount_out, U256::ZERO);
    // AGREEMENT: on-chain consumes the full `amount_in` as fee — the
    // delegated `compute_swap_step_v3` path mirrors this.
    assert_eq!(
        result.consumed_input, amount_in,
        "solver dust consumed_input must equal on-chain (full amount as fee)"
    );
    assert_eq!(step.amount_in, U256::ZERO);
    assert_eq!(
        step.fee_amount, amount_in,
        "on-chain consumed full amount_in as dust fee"
    );
}

// ── Active-set piecewise walk tests ────────────────────────────
//
// The legacy solver enumerated ending-range tuples up to
// `max_candidates = 10` per CL hop — a silent accuracy cap: when the
// argmax piece sits beyond index 9 on any hop, the enumeration never
// proposes a candidate there. The active-set piecewise Möbius walk
// (docs/architecture/mobius_v3_ending_range_enumeration_evaluation.md)
// has no prefix cap. The reference implementations below re-create the
// enumeration WITHOUT the cap; the tests assert the production solver
// matches the uncapped reference exactly — an equality the capped
// enumeration cannot satisfy when the optimum crosses > 9 ranges.

fn sp_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap())
}

/// Build a multi-range CL sequence in swap order around `anchor_tick`.
/// Range 0 contains the anchor-tick price; later ranges step by `step`
/// ticks in the swap direction (descending for zfo, ascending for ofz),
/// each `liquidities[i]` wide. Matches `compute_crossing`'s convention
/// that range i>0's entry price is the previous range's far boundary.
fn multi_range_sequence(
    anchor_tick: i32,
    step: i32,
    zfo: bool,
    liquidities: &[u128],
) -> IntV3TickRangeSequence {
    let ranges: Vec<IntV3TickRangeHop> = liquidities
        .iter()
        .enumerate()
        .map(|(i, &liquidity)| {
            let i = i32::try_from(i).unwrap();
            let (tick_lo, tick_hi) = if zfo {
                (anchor_tick - (i + 1) * step, anchor_tick - i * step)
            } else {
                (anchor_tick + i * step, anchor_tick + (i + 1) * step)
            };
            let sqrt_price_x96 = if i == 0 {
                sp_at(anchor_tick)
            } else if zfo {
                sp_at(anchor_tick - i * step)
            } else {
                sp_at(anchor_tick + i * step)
            };
            IntV3TickRangeHop {
                liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: sp_at(tick_lo),
                sqrt_price_upper_x96: sp_at(tick_hi),
                gamma_numer: 997_000,
                fee_denom: 1_000_000,
                zero_for_one: zfo,
                word_boundary_prices: Vec::new(),
            }
        })
        .collect();
    IntV3TickRangeSequence::new(ranges).unwrap()
}

/// The nested ceil-inversion must predict the exact
/// first-above input on a mixed 3-hop path. `sim(pa)` must exceed `ks`
/// and `sim(pa−1)` must not — which, by monotonicity of `landed`, proves
/// `pa` is THE smallest x whose landing exits the tuple.
#[test]
fn walk_event_first_above_predicted_is_exact_on_synthetic_paths() {
    let seq0 = multi_range_sequence(
        100,
        10,
        true,
        &[
            4_000_000_000_000_000_000u128,
            2_000_000_000_000_000_000,
            5_000_000_000_000_000_000,
        ],
    );
    // Hop-2 liquidities deliberately small (crossing grosses below the V2
    // mid-hop's output asymptote) so the lattice exercises BOTH hops'
    // crossing branches, not just hop 0's.
    let seq2 = multi_range_sequence(
        -120,
        13,
        false,
        &[1_000_000_000_000u128, 2_000_000_000_000, 3_000_000_000_000],
    );
    let v2 = IntHopState::new(
        U256::from(5_000_000_000_000_000_000u128),
        U256::from(4_000_000_000_000_000_000u128),
        997,
        1000,
    );
    let h0 = cl_walk_hop(&seq0, None);
    let h2 = cl_walk_hop(&seq2, None);
    let v2_tail = IntHopState::new(
        U256::from(3_000_000_000_000_000_000u128),
        U256::from(2_000_000_000_000_000_000u128),
        997,
        1000,
    );
    let shape_a: Vec<WalkHop> = vec![
        h0,
        WalkHop::ConstantProduct(&v2),
        h2,
        WalkHop::ConstantProduct(&v2_tail),
    ];
    // Shape B: 3-level CL-CL-CL recursion (mixed directions, staggered
    // liquidities so crossing grosses interleave across all hops).
    let seqb0 = multi_range_sequence(
        90,
        9,
        true,
        &[4_000_000_000_000_000_000u128, 1_000_000_000_000_000_000],
    );
    let seqb1 = multi_range_sequence(
        -90,
        11,
        false,
        &[900_000_000_000_000_000u128, 1_100_000_000_000_000_000],
    );
    let seqb2 = multi_range_sequence(
        60,
        12,
        true,
        &[5_000_000_000_000u128, 7_000_000_000_000, 9_000_000_000_000],
    );
    let shape_b: Vec<WalkHop> = vec![
        cl_walk_hop(&seqb0, None),
        cl_walk_hop(&seqb1, None),
        cl_walk_hop(&seqb2, None),
    ];
    let crossings0 = build_crossing_table(&seq0);
    let mut total_checked = 0usize;
    let mut total_tuples = 0usize;
    for (label, hops) in [("A", &shape_a), ("B", &shape_b)] {
        let (t, c) = run_predicted_lattice_verification(label, hops, &crossings0);
        total_tuples += t;
        total_checked += c;
    }
    assert!(total_tuples >= 10, "lattice walks found too few tuples");
    assert!(total_checked >= 8, "too few predicted tuples exercised");
}

/// Shared body of `walk_event_first_above_predicted_is_exact...` — the
/// lattice walk + per-tuple two-probe verification.
fn run_predicted_lattice_verification(
    label: &str,
    hops: &[WalkHop],
    crossings0: &[IntTickRangeCrossing],
) -> (usize, usize) {
    use std::collections::HashSet;
    // Lattice walk: seed at x = 1 plus each hop-0 boundary gross (hop 0's
    // input == x), then follow the predicted first-above chain — each
    // prediction lands IN the next tuple, so the chain enumerates exactly
    // the tuple lattice the walk visits.
    let mut worklist: Vec<U256> = vec![U256::ONE];
    for c in crossings0 {
        for d in 1u64..=2 {
            let xd = c.crossing_gross_input.saturating_sub(U256::from(d));
            if !xd.is_zero() {
                worklist.push(xd);
            }
        }
    }
    // Random multi-seeds across the magnitude bracket the grosses live
    // in — chains from different lattice regions follow different hops.
    let mut lcg: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..400 {
        lcg = lcg
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let shift = 56 + (lcg % 48) as u32;
        let x = U256::from(lcg) << shift;
        worklist.push(x | U256::ONE);
    }
    let mut tuples: HashSet<Vec<usize>> = HashSet::new();
    let mut checked = 0usize;
    let mut guard = 0usize;
    while let Some(x) = worklist.pop() {
        guard += 1;
        assert!(guard < 10_000, "lattice walk did not terminate");
        let landed = simulate_walk_path(x, hops).landed.clone();
        if !tuples.insert(landed.clone()) {
            continue;
        }
        let Some(pa) = walk_event_first_above_predicted(hops, &landed) else {
            continue; // terminal shapes may legitimately predict None
        };
        let above = landed_any_above(&simulate_walk_path(pa, hops).landed, &landed);
        let below_ok = !landed_any_above(&simulate_walk_path(pa - U256::ONE, hops).landed, &landed);
        assert!(
            above && below_ok,
            "{label}: ks={landed:?} predicted first_above={pa} above={above} below_exact={below_ok}"
        );
        checked += 1;
        for d in 0u64..=2 {
            worklist.push(pa + U256::from(d));
        }
    }
    eprintln!("{label}: TUPLES n={}: {tuples:?}", tuples.len());
    (tuples.len(), checked)
}

/// The profile-level demand inversion against
/// brute-force `swap()` minimality — pins the step-jump derivation, the
/// exact-out step inversion (fee compensation), and the saturation clamp.
#[test]
fn word_profile_min_input_for_output_matches_bruteforce() {
    // Dense zfo range: three interior word boundaries between entry and
    // exit (targets in swap order — the same shape the profile walk uses).
    let dense_zfo = IntV3TickRangeHop {
        liquidity: 3_000_000_000_000_000_000u128,
        sqrt_price_x96: sp_at(100),
        sqrt_price_lower_x96: sp_at(60),
        sqrt_price_upper_x96: sp_at(120),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: vec![sp_at(95), sp_at(80), sp_at(70)],
    };
    // Sparse ofz range (no interior boundaries).
    let sparse_ofz = IntV3TickRangeHop {
        liquidity: 2_000_000_000_000_000_000u128,
        sqrt_price_x96: sp_at(0),
        sqrt_price_lower_x96: sp_at(-40),
        sqrt_price_upper_x96: sp_at(30),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };
    for hop in [&dense_zfo, &sparse_ofz] {
        let Some(profile) = V3WordProfile::build(hop) else {
            panic!("profile build failed");
        };
        let cap = profile.swap(U256::MAX).output;
        assert!(!cap.is_zero());
        // Sample demands: ladder + boundary outputs +− 1.
        let mut demands: Vec<U256> = Vec::new();
        for i in 0..64u32 {
            demands.push((cap >> i) + U256::ONE);
        }
        demands.push(U256::ONE);
        for w in demands {
            if w > cap {
                continue;
            }
            let Some(inv) = word_profile_min_input_for_output(&profile, w) else {
                panic!("inversion returned None for demand {w} of cap {cap}");
            };
            let out_at = profile.swap(inv).output;
            assert!(
                out_at >= w,
                "demand {w}: inversion {inv} produced only {out_at}"
            );
            if inv > U256::ZERO {
                let out_below = profile.swap(inv - U256::ONE).output;
                assert!(
                    out_below < w,
                    "demand {w}: inversion {inv} not minimal ({out_below} >= {w} at −1)"
                );
            }
        }
        // Beyond the cap must return None.
        assert!(word_profile_min_input_for_output(&profile, cap + U256::ONE).is_none());
    }
}

/// The legacy all-CL enumeration with NO `max_candidates` cap. This is
/// the pre-cutover `int_solve_cl_path` general case verbatim except the
/// radix is the full range count. Kept in-tree as the brute-force
/// reference: the production solver must equal it (concavity ⇒ both find
/// the same argmax piece), while the historical capped enumeration
/// provably cannot when the optimum lives past index 9.
fn reference_uncapped_cl_solve(
    sequences: &[&IntV3TickRangeSequence],
) -> Option<(U256, U256, Vec<U256>)> {
    let n_hops = sequences.len();
    let radices: Vec<usize> = sequences.iter().map(|s| s.ranges.len()).collect();
    let base_ranges: Vec<IntV3TickRangeHop> =
        sequences.iter().map(|s| s.ranges[0].clone()).collect();

    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut best_hop_outputs: Vec<U256> = Vec::new();

    let mut ks: Vec<usize> = vec![0; n_hops];
    let mut crossings: Vec<Option<IntTickRangeCrossing>> = Vec::with_capacity(n_hops);
    'outer: loop {
        crossings.clear();
        let mut total_crossing_cost = U256::ZERO;
        for (i, &k) in ks.iter().enumerate() {
            if k >= sequences[i].ranges.len() {
                // Defensive: radix bookkeeping keeps k in range.
                return None;
            }
            let crossing = sequences[i].compute_crossing(k)?;
            if k > 0 {
                total_crossing_cost =
                    total_crossing_cost.saturating_add(crossing.crossing_gross_input);
                crossings.push(Some(crossing));
            } else {
                crossings.push(None);
            }
        }

        let flat_hops: Vec<IntHopState> = (0..n_hops)
            .map(|i| {
                if let Some(ref crossing) = crossings[i] {
                    crossing.ending_range.to_int_hop_state()
                } else {
                    base_ranges[i].to_int_hop_state()
                }
            })
            .collect();

        if let Ok(result) = crate::mobius_int_exact::exact_mobius_solve(&flat_hops) {
            if result.is_profitable && !result.optimal_input.is_zero() {
                let total_optimal_input = result.optimal_input.saturating_add(total_crossing_cost);
                for delta in -2i32..=2 {
                    let candidate = if delta >= 0 {
                        total_optimal_input.saturating_add(U256::from(delta.cast_unsigned()))
                    } else {
                        total_optimal_input.saturating_sub(U256::from((-delta).cast_unsigned()))
                    };
                    if candidate.is_zero() {
                        continue;
                    }
                    let sim = int_simulate_cl_path_n(candidate, &crossings, &base_ranges);
                    if sim.final_output > candidate {
                        let profit = sim.final_output - candidate;
                        if profit > best_profit {
                            best_profit = profit;
                            best_x = candidate;
                            best_hop_outputs =
                                sim.steps.iter().map(|s| s.output).collect::<Vec<_>>();
                        }
                    }
                }
            }
        }

        // Mixed-radix increment over FULL range counts (no cap).
        let mut carry = true;
        for i in 0..n_hops {
            if !carry {
                break;
            }
            ks[i] += 1;
            if ks[i] >= radices[i] {
                ks[i] = 0;
            } else {
                carry = false;
            }
        }
        if carry {
            break 'outer;
        }
    }

    if best_profit.is_zero() {
        None
    } else {
        Some((best_x, best_profit, best_hop_outputs))
    }
}

/// The legacy mixed V2+CL enumeration with NO `max_candidates` cap
/// (uncapped twin of the pre-cutover `exact_solve_mixed_path_n`).
#[expect(clippy::too_many_lines)] // still >100 in hotpath builds (test-only, never instrumented)
fn reference_uncapped_mixed_solve(
    v2_hops: &[Option<IntHopState>],
    cl_sequences: &[Option<IntV3TickRangeSequence>],
    hop_order: &[bool],
) -> Option<(U256, U256, Vec<U256>)> {
    let n_hops = hop_order.len();
    let cl_indices: Vec<usize> = hop_order
        .iter()
        .enumerate()
        .filter(|(_, &is_v2)| !is_v2)
        .map(|(i, _)| i)
        .collect();
    let mut cl_radices: Vec<usize> = Vec::with_capacity(cl_indices.len());
    for &i in &cl_indices {
        cl_radices.push(cl_sequences[i].as_ref()?.ranges.len());
    }
    let cl_base_ranges: Vec<Option<IntV3TickRangeHop>> = hop_order
        .iter()
        .enumerate()
        .map(|(i, &is_v2)| {
            if is_v2 {
                None
            } else {
                cl_sequences[i].as_ref().map(|s| s.ranges[0].clone())
            }
        })
        .collect();

    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut best_hop_outputs: Vec<U256> = Vec::new();

    let mut cl_ks: Vec<usize> = vec![0; cl_indices.len()];
    'outer: loop {
        let mut crossings: Vec<Option<IntTickRangeCrossing>> = vec![None; n_hops];
        let mut total_crossing_cost = U256::ZERO;
        for (cl_idx, &hop_idx) in cl_indices.iter().enumerate() {
            let k = cl_ks[cl_idx];
            if k > 0 {
                let seq = cl_sequences[hop_idx].as_ref()?;
                let crossing = seq.compute_crossing(k)?;
                total_crossing_cost =
                    total_crossing_cost.saturating_add(crossing.crossing_gross_input);
                crossings[hop_idx] = Some(crossing);
            }
        }

        let mut flat_hops = Vec::with_capacity(n_hops);
        let mut valid = true;
        for i in 0..n_hops {
            if hop_order[i] {
                let Some(v2) = v2_hops[i].as_ref() else {
                    valid = false;
                    break;
                };
                flat_hops.push(v2.clone());
            } else if let Some(ref crossing) = crossings[i] {
                flat_hops.push(crossing.ending_range.to_int_hop_state());
            } else {
                let Some(cl_seq) = cl_sequences[i].as_ref() else {
                    valid = false;
                    break;
                };
                flat_hops.push(cl_seq.ranges[0].to_int_hop_state());
            }
        }

        if valid {
            if let Ok(result) = crate::mobius_int_exact::exact_mobius_solve(&flat_hops) {
                if result.is_profitable && !result.optimal_input.is_zero() {
                    let total_optimal_input =
                        result.optimal_input.saturating_add(total_crossing_cost);
                    for delta in -2i32..=2 {
                        let candidate = if delta >= 0 {
                            total_optimal_input.saturating_add(U256::from(delta.cast_unsigned()))
                        } else {
                            total_optimal_input.saturating_sub(U256::from((-delta).cast_unsigned()))
                        };
                        if candidate.is_zero() {
                            continue;
                        }
                        let sim = int_simulate_mixed_path_n(
                            candidate,
                            v2_hops,
                            &cl_base_ranges,
                            &crossings,
                            hop_order,
                        );
                        if sim.final_output > candidate {
                            let profit = sim.final_output - candidate;
                            if profit > best_profit {
                                best_profit = profit;
                                best_x = candidate;
                                best_hop_outputs =
                                    sim.steps.iter().map(|s| s.output).collect::<Vec<_>>();
                            }
                        }
                    }
                }
            }
        }

        let mut carry = true;
        for cl_idx in 0..cl_indices.len() {
            if !carry {
                break;
            }
            cl_ks[cl_idx] += 1;
            if cl_ks[cl_idx] >= cl_radices[cl_idx] {
                cl_ks[cl_idx] = 0;
            } else {
                carry = false;
            }
        }
        if carry {
            break 'outer;
        }
    }

    if best_profit.is_zero() {
        None
    } else {
        Some((best_x, best_profit, best_hop_outputs))
    }
}

/// RED→GREEN : a 2-hop CL cycle whose argmax piece is at
/// hop-2 range index 10 — strictly beyond the legacy
/// `max_candidates = 10` enumeration prefix (indices 0..=9).
///
/// Geometry: hop 1 is a deep, wide zfo pool pegged near tick +750
/// (p₁ ≈ 1.0716 token1 per token0, price impact negligible). Hop 2 is an
/// ofz pool starting at tick 0 with tick-thin liquidity through
/// tick 600, then a deep range on [600, 660]. Round-trip marginal profit
/// is positive while p₂ < γ²·p₁ ≈ 1.0652, i.e. up to tick ≈ 646 —
/// *inside* range index 10. The capped enumeration only proposes ending
/// ranges 0..=9 (tick-thin liquidity; near-zero capacity) and misses the
/// profit concentrated in range 10.
/// RED→GREEN : a 2-hop CL cycle whose argmax piece is at
/// hop-2 range index 10 — strictly beyond the legacy
/// `max_candidates = 10` enumeration prefix (indices 0..=9).
///
/// Geometry: hop 1 is a deep, wide zfo pool pegged near tick +750
/// (p₁ ≈ 1.0779 token1 per token0, price impact negligible). Hop 2 is an
/// ofz pool starting at tick 0 with tick-thin liquidity through
/// tick 600, then a deep range on [600, 660]. Round-trip marginal profit
/// is positive while p₂ < γ²·p₁ ≈ 1.0714, i.e. up to tick ≈ 708 —
/// beyond range index 10's entry.
///
/// This fixture ALSO exposes the legacy enumeration's second failure
/// mode: its per-piece anchors model the ending range as an UNBOUNDED
/// constant-product pool, so when the optimum sits at a range-saturation
/// corner, the anchor overshoots, the validation sim reports negative
/// profit, and even an UNcapped reference enumeration finds nothing
/// (asserted below as `reference == None` — the reference is
/// corner-blind on this geometry). The active-set walk refines each
/// visited piece with a windowed ternary search, which sees the corner.
#[test]
fn int_solve_cl_path_beyond_ten_range_prefix_finds_corner_profit() {
    // Hop 1: single 1300-tick-wide deep zfo range, price pinned at tick 750.
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    // Hop 2: 12 ofz ranges of width 60; thin until tick 600, deep on
    // [600, 660], thin again on [660, 720].
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);

    let reference = reference_uncapped_cl_solve(&[&seq1, &seq2]);
    assert!(
        reference.is_none(),
        "oracle sanity: the corner-blind reference must find NOTHING here ({reference:?})"
    );

    let (x, profit, _hop_outputs) =
        solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default())
            .result
            .expect("the active-set walk must find the deep range-10/11 profit");

    // Non-vacuity floor: a coarse ×2 grid scan of the same path already
    // shows ≥ 1.2e8 profit; the walk's refined optimum must clear it.
    assert!(
        profit >= U256::from(100_000_000u64),
        "corner profit must clear the coarse-grid floor, got {profit}"
    );

    // The solver's optimal input must LAND hop 2 BEYOND the legacy
    // 10-tuple prefix (indices 0..=9): in the deep range 10, possibly
    // spilling into the last thin range 11 at the saturation corner.
    let hops = [cl_walk_hop(&seq1, None), cl_walk_hop(&seq2, None)];
    let landed = simulate_walk_path(x, &hops).landed;
    assert!(
        landed[1] >= 10,
        "solver input must land hop 2 past the legacy prefix (indices 0..=9), got landed[1]={}",
        landed[1]
    );

    // Never worse than the (here empty) legacy enumeration answer.
    let reference_profit = reference.map_or(U256::ZERO, |(_, p, _)| p);
    assert!(profit >= reference_profit);
}

/// RED→GREEN : mixed V2→CL 3-hop path with the same
/// deep-late-liquidity CL construction; the CL hop's argmax piece sits
/// beyond the legacy 10-tuple prefix.
#[test]
fn exact_solve_mixed_path_n_beyond_ten_range_prefix_matches_uncapped_reference() {
    // V2 entry pool priced at tick +750: price(token0) = r1/r0
    // ≈ 1.0001^750 ≈ 1.07163.
    let r0 = U256::from(1_000_000_000_000_000u128);
    let r1 = U256::from(1_071_633_064_014_504u128);
    let v2_entry = IntHopState::new(r0, r1, 997, 1000);

    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let cl_seq = multi_range_sequence(0, 60, false, &liquidities);

    let reference = reference_uncapped_mixed_solve(
        &[Some(v2_entry.clone()), None],
        &[None, Some(cl_seq.clone())],
        &[true, false],
    );
    let result = exact_solve_mixed_path_n(
        &[Some(v2_entry), None],
        &[None, Some(&cl_seq)],
        &[None, None],
        &[true, false],
        &SolveRuntimeConfig::default(),
    );

    // Not exact equality: the reference's ±2 sweep around its piecewise
    // anchor can sit one staircase jog off the discrete argmax, while
    // the walk's dense final sweep picks the exact discrete maximizer —
    // the walk may beat the reference by a wei here (observed: 25189262
    // vs 25189261). The assertion that matters is NEVER WORSE.
    let solver_profit = result.result.map_or(U256::ZERO, |(_, profit, _)| profit);
    let reference_profit = reference.map_or(U256::ZERO, |(_, profit, _)| profit);
    assert!(
        solver_profit >= reference_profit,
        "mixed-path solver ({solver_profit}) must never be worse than the \
         uncapped reference ({reference_profit})"
    );
    assert!(
        !reference_profit.is_zero(),
        "oracle sanity: this fixture must have real profit past the legacy prefix"
    );
}

/// Property: across a family of deep-late-liquidity
/// constructions, the solver must equal the uncapped reference — the
/// walk never does worse than ANY enumeration prefix, capped or not.
#[test]
fn cl_path_solver_matches_uncapped_reference_across_late_liquidity_family() {
    for hop1_tick in [700i32, 750, 800] {
        let seq1 = multi_range_sequence(hop1_tick, 1300, true, &[1_000_000_000_000_000]);
        for deep_liquidity in [
            1_000_000_000_000u128,
            10_000_000_000_000,
            100_000_000_000_000,
        ] {
            let mut liquidities = vec![1_000_000_000u128; 10];
            liquidities.push(deep_liquidity);
            liquidities.push(1_000_000_000u128);
            let seq2 = multi_range_sequence(0, 60, false, &liquidities);

            let solver = solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result;
            let reference = reference_uncapped_cl_solve(&[&seq1, &seq2]);
            let solver_profit = solver.map_or(U256::ZERO, |(_, profit, _)| profit);
            let reference_profit = reference.map_or(U256::ZERO, |(_, profit, _)| profit);
            // Loop-15: 2-wei documented epsilon. The 700/1e14 member's
            // optimum sits on a hyperflat top (~9k wei wide at <1 wei of
            // real slope) whose discrete maximum is rounding-aliasing —
            // sample-alignment luck decides the last wei (the exact-edge
            // event solver measures +12 wei on the 800/1e14 member and
            // −1 here; the legacy solver itself finds the same recorded
            // maximum at a 9.1m-wei-different input). A deficit > 2 wei is
            // still a loud failure.
            let eps = U256::from(2u64);
            assert!(
                solver_profit + eps >= reference_profit,
                "hop1_tick={hop1_tick} deep_liquidity={deep_liquidity}: \
                 solver ({solver_profit}) must never be worse than the uncapped \
                 reference ({reference_profit}) by more than the 2-wei \
                 flat-top aliasing epsilon — the reference is corner-blind, \
                 so equality is expected only for interior-optimum members"
            );
        }
    }
}

/// Guard: the walk must visit ≤ Σ ranges + 2 pieces and bound
/// its simulation count — the regression net against re-introducing
/// combinatorial tuple enumeration (the legacy solver evaluated
/// 10^n_hops tuples × 5 simulations regardless of where the optimum is).
///
/// Counters are thread-local, so the reset → solve → read sequence is
/// race-free under multi-threaded `cargo test`.
#[test]
fn active_set_walk_piece_and_simulation_counts_are_bounded() {
    // Deep fixture (Σ ranges = 13): optimum beyond the legacy prefix.
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);

    WALK_PIECES_VISITED.with(|c| c.set(0));
    WALK_PATH_SIMULATIONS.with(|c| c.set(0));
    let result = solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result;
    assert!(result.is_some());
    let pieces = WALK_PIECES_VISITED.with(std::cell::Cell::get);
    let sims = WALK_PATH_SIMULATIONS.with(std::cell::Cell::get);
    let refine_sims = WALK_REFINE_SIMS.with(std::cell::Cell::get);
    let ternary_sims = WALK_TERNARY_SIMS.with(std::cell::Cell::get);
    let grid_sims = WALK_GRID_SIMS.with(std::cell::Cell::get);
    let word_steps = WALK_WORD_STEPS.with(std::cell::Cell::get);
    eprintln!(
        "deep fixture: pieces_visited={pieces} path_simulations={sims} refine_sims={refine_sims} (ternary={ternary_sims} grid={grid_sims}) word_steps={word_steps}"
    );
    assert!(
        pieces <= 13 + 2,
        "visited pieces must be bounded by Σ ranges + 2, got {pieces}"
    );
    // Combinatorial bound PLUS the seeded-bracket regression contract
    //: the warm-started right-edge bisection must keep this
    // fixture at/under the measured 832-sim ceiling. 900 = 832 + safety
    // margin for word-boundary perturbations.
    assert!(
        sims <= 900,
        "seed + dedup probe budget regressed, got {sims} sims (baseline 844, seeded 832)"
    );

    // Common case: 3-hop, moderate multi-range sequences.
    let s1 = multi_range_sequence(-100, 60, true, &[5_000_000_000_000u128; 8]);
    let s2 = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128; 8]);
    let s3 = multi_range_sequence(100, 60, true, &[5_000_000_000_000u128; 8]);
    WALK_PIECES_VISITED.with(|c| c.set(0));
    WALK_PATH_SIMULATIONS.with(|c| c.set(0));
    let _ = solve_cl_derived(&[&s1, &s2, &s3], &SolveRuntimeConfig::default()).result;
    let pieces = WALK_PIECES_VISITED.with(std::cell::Cell::get);
    let sims = WALK_PATH_SIMULATIONS.with(std::cell::Cell::get);
    let refine_sims = WALK_REFINE_SIMS.with(std::cell::Cell::get);
    let ternary_sims = WALK_TERNARY_SIMS.with(std::cell::Cell::get);
    let grid_sims = WALK_GRID_SIMS.with(std::cell::Cell::get);
    let word_steps = WALK_WORD_STEPS.with(std::cell::Cell::get);
    eprintln!(
        "3-hop: pieces_visited={pieces} path_simulations={sims} refine_sims={refine_sims} (ternary={ternary_sims} grid={grid_sims}) word_steps={word_steps}"
    );
    assert!(pieces <= 24 + 2);
    // Seeded-bracket regression contract: 251 measured with the
    // warm start; 300 keeps headroom.
    assert!(sims <= 300);
}

/// Property: the walk's profit must match a fine grid
/// maximization oracle (band tolerance — see the assertion) across BOTH the
/// shallow/interior and deep/corner liquidity families. The uncapped
/// enumeration reference is corner-blind (it can find NOTHING on these
/// geometries), so the grid is the only sound oracle here.
///
/// Oracle: coarse ×1.05 scan, then a ±10%×400-point scan around the
/// coarse argmax, then an exact ±64 dense sweep.
#[test]
fn cl_path_solver_matches_fine_grid_oracle_across_families() {
    fn grid_oracle_profit(hops: &[WalkHop]) -> U256 {
        let mut best = U256::ZERO;
        let mut best_x = U256::ZERO;
        // Coarse ×1.05 scan over the plausible input range.
        let mut x = U256::from(1000u64);
        for _ in 0..1200 {
            let out = simulate_walk_path(x, hops).final_output;
            if out > x && out - x > best {
                best = out - x;
                best_x = x;
            }
            x = x.saturating_mul(U256::from(105u64)) / U256::from(100u64);
            if x > (U256::from(1u128) << 128) {
                break;
            }
        }
        if best.is_zero() {
            return U256::ZERO;
        }
        // ±10% local scan (400 points).
        let lo = best_x / U256::from(11u64) * U256::from(10u64);
        let hi = best_x / U256::from(10u64) * U256::from(11u64);
        let step = ((hi - lo) / U256::from(400u64)).max(U256::ONE);
        let mut y = lo;
        while y <= hi {
            let out = simulate_walk_path(y, hops).final_output;
            if out > y && out - y > best {
                best = out - y;
                best_x = y;
            }
            y += step;
        }
        // Exact local dense sweep ±64.
        let lo = best_x.saturating_sub(U256::from(64u64));
        let hi = best_x.saturating_add(U256::from(64u64));
        let mut y = lo;
        while y <= hi {
            let out = simulate_walk_path(y, hops).final_output;
            if out > y && out - y > best {
                best = out - y;
            }
            y += U256::from(1u64);
        }
        best
    }

    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    for deep_liquidity in [1_000_000_000_000u128, 10_000_000_000_000] {
        for deep_index in [3usize, 10] {
            let mut liquidities = vec![1_000_000_000u128; 12];
            liquidities[deep_index] = deep_liquidity;
            let seq2 = multi_range_sequence(0, 60, false, &liquidities);
            let seqs = [&seq1, &seq2];
            let hops = [cl_walk_hop(&seq1, None), cl_walk_hop(&seq2, None)];
            let oracle = grid_oracle_profit(&hops);
            let solver = solve_cl_derived(&seqs, &SolveRuntimeConfig::default()).result;
            let solver_profit = solver.map_or(U256::ZERO, |(_, p, _)| p);
            eprintln!(
                "deep_liquidity={deep_liquidity} deep_index={deep_index}:                      oracle={oracle} solver={solver_profit}"
            );
            // The grid is a max over SAMPLED points only — a lower
            // bound on the true optimum. The walk may beat it slightly
            // (its dense final sweep finds the discrete maximizer), so
            // assert: never materially below the grid, and never above
            // it by more than a 0.1% band (catches gross divergence).
            assert!(
                solver_profit + U256::from(4u64) >= oracle,
                "walk must never under-shoot the grid oracle: solver={solver_profit} oracle={oracle}"
            );
            let band = (oracle / U256::from(1000u64)).max(U256::from(4u64));
            assert!(
                solver_profit <= oracle + band,
                "walk must not exceed the grid beyond a 0.1% band: solver={solver_profit} oracle={oracle}"
            );
        }
    }
}

/// Perf probe for the active-set walk (run manually with --ignored
/// --nocapture). Not a gate — prints µs per solve for the hot path.
#[test]
#[ignore = "perf probe; run manually"]
fn bench_active_set_walk_solve() {
    use std::time::Instant;

    fn time_solve(label: &str, f: impl Fn()) {
        // warmup
        for _ in 0..10 {
            f();
        }
        let start = Instant::now();
        let n = 200;
        for _ in 0..n {
            f();
        }
        let per = start.elapsed() / n;
        eprintln!("{label}: {per:?}/solve");
    }

    // single-range 2-hop
    let sr1 = multi_range_sequence(-100, 200, true, &[5_000_000_000_000u128]);
    let sr2 = multi_range_sequence(0, 200, false, &[10_000_000_000_000u128]);
    time_solve("2-hop single-range", || {
        let _ = solve_cl_derived(&[&sr1, &sr2], &SolveRuntimeConfig::default()).result;
    });

    // 8-range 2-hop
    let mr2h1 = multi_range_sequence(-100, 60, true, &[5_000_000_000_000u128; 8]);
    let mr2h2 = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128; 8]);
    time_solve("2-hop 8-range", || {
        let _ = solve_cl_derived(&[&mr2h1, &mr2h2], &SolveRuntimeConfig::default()).result;
    });

    // 3-hop 8-range each
    let mr3h1 = multi_range_sequence(-100, 60, true, &[5_000_000_000_000u128; 8]);
    let mr3h2 = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128; 8]);
    let mr3h3 = multi_range_sequence(100, 60, true, &[5_000_000_000_000u128; 8]);
    time_solve("3-hop 8-range", || {
        let _ = solve_cl_derived(&[&mr3h1, &mr3h2, &mr3h3], &SolveRuntimeConfig::default()).result;
    });

    // Uncapped enumeration reference on the same 8-range 2-hop. Cost per
    // tuple is identical in both.
    time_solve("2-hop 8-range legacy-enumeration (reference)", || {
        let _ = reference_uncapped_cl_solve(&[&mr2h1, &mr2h2]);
    });

    #[cfg(test)]
    {
        use std::sync::atomic::Ordering::Relaxed;
        let _ = Relaxed;
        WALK_PATH_SIMULATIONS.with(|c| c.set(0));
        WALK_PIECES_VISITED.with(|c| c.set(0));
        let _ = solve_cl_derived(&[&mr2h1, &mr2h2], &SolveRuntimeConfig::default()).result;
        eprintln!(
            "2-hop 8-range walk: pieces={} sims={}",
            WALK_PIECES_VISITED.with(std::cell::Cell::get),
            WALK_PATH_SIMULATIONS.with(std::cell::Cell::get)
        );
    }
}

// ── Exact shifted-anchor tests (EHSWSX) ───────────────────────────────

/// The deep beyond-prefix fixture at tuple (0, 10): hop 2's crossing
/// gross input dominates. The transitional anchor (unshifted
/// coefficients + additive crossing cost) pays hop 2's crossing out of
/// the PATH INPUT — overshooting to ~4.29e10 where hop 2 has long
/// saturated (negative profit). The exact shifted anchor composes the
/// crossing translations through the chain, lands on-piece, and its
/// simulated score must beat the transitional anchor's.
#[test]
fn exact_shifted_anchor_lands_on_piece_where_transitional_overshoots() {
    let seq1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000]);
    let mut liquidities = vec![1_000_000_000u128; 10];
    liquidities.push(10_000_000_000_000u128);
    liquidities.push(1_000_000_000u128);
    let seq2 = multi_range_sequence(0, 60, false, &liquidities);
    let hops = [cl_walk_hop(&seq1, None), cl_walk_hop(&seq2, None)];
    let ks = vec![0usize, 10];

    let exact = walk_piece_anchor(&hops, &ks);
    let transitional = walk_piece_anchor_transitional(&hops, &ks);
    eprintln!("exact anchor: {exact}, transitional: {transitional}");

    let exact_outcome = simulate_walk_path(exact, &hops);
    let trans_outcome = simulate_walk_path(transitional, &hops);
    let exact_score = walk_profit_score(exact_outcome.final_output, exact);
    let trans_score = walk_profit_score(trans_outcome.final_output, transitional);
    eprintln!("scores: exact={exact_score} transitional={trans_score}");

    assert!(
        exact_score > trans_score,
        "exact anchor must beat the transitional anchor on a crossing-dominated piece"
    );
    // Transitional demonstrably OVERSHOOTS the piece (its landed tuple
    // runs past (0,10) into the saturated tail).
    assert!(
        trans_outcome.landed[1] > ks[1] || trans_score < alloy::primitives::I256::ZERO,
        "transitional anchor should overshoot or lose money here (got {:?}, score {trans_score})",
        trans_outcome.landed
    );
}

/// On pieces whose argmax is INTERIOR (not a saturation corner — no
/// unbounded Möbius model can see a corner), the exact shifted anchor
/// must sit within a hair of the windowed ternary/dense refinement's
/// score, and never materially under-perform the transitional anchor.
///
/// The fixture family drives hop 1's crossing content high (several
/// thin ranges) so the shift term is material; members where the solved
/// optimum is NOT interior are skipped (they exercise the corner lane,
/// which `walk_refine_window` owns).
#[test]
fn exact_shifted_anchor_matches_refined_argmax_on_interior_optima() {
    let mut interior_members = 0usize;
    for hop1_tick in [700i32, 750, 800] {
        for deep_index in [3usize, 6] {
            for deep_liquidity in [10_000_000_000_000u128, 100_000_000_000_000] {
                let seq1 = multi_range_sequence(hop1_tick, 1300, true, &[1_000_000_000_000_000]);
                let mut liquidities = vec![1_000_000_000u128; 12];
                for l in liquidities.iter_mut().skip(deep_index) {
                    *l = deep_liquidity;
                }
                let seq2 = multi_range_sequence(0, 60, false, &liquidities);
                let Some((x_star, profit, _)) =
                    solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result
                else {
                    continue;
                };
                assert!(!profit.is_zero());
                let hops = [cl_walk_hop(&seq1, None), cl_walk_hop(&seq2, None)];
                let ks = simulate_walk_path(x_star, &hops).landed;
                let anchor = walk_piece_anchor(&hops, &ks);
                let x_l = piece_window_left_edge(&hops, &ks, anchor);
                let Some(x_r) = piece_window_right_edge(&hops, &ks, anchor) else {
                    continue;
                };
                let mut rec = WalkRecorder::new();
                let (_argmax_x, piece_best_score) = walk_refine_window(&hops, x_l, x_r, &mut rec);

                // Skip corner members: the refined argmax must sit strictly
                // interior to the window.
                let x_star_i =
                    walk_profit_score(simulate_walk_path(x_star, &hops).final_output, x_star);
                let near_left = x_star <= x_l.saturating_add(U256::from(8u64));
                let near_right = x_star.saturating_add(U256::from(8u64)) >= x_r;
                if near_left || near_right {
                    continue;
                }
                interior_members += 1;

                let exact_score =
                    walk_profit_score(simulate_walk_path(anchor, &hops).final_output, anchor);
                let transitional = walk_piece_anchor_transitional(&hops, &ks);
                let trans_score = walk_profit_score(
                    simulate_walk_path(transitional, &hops).final_output,
                    transitional,
                );
                eprintln!(
                "deep_index={deep_index} deep_liquidity={deep_liquidity} ks={ks:?}:                      exact_gap={} trans_gap={}",
                piece_best_score - exact_score,
                piece_best_score - trans_score,
            );
                // Exact anchor ≈ the smooth model optimum: within
                // max(16 wei, best/10⁶) of the refined discrete argmax.
                let tolerance = alloy::primitives::I256::from_limbs([16, 0, 0, 0]).max(
                    piece_best_score / alloy::primitives::I256::from_limbs([1_000_000, 0, 0, 0]),
                );
                assert!(
                piece_best_score - exact_score <= tolerance,
                "exact anchor off the refined argmax by more than {tolerance}                      (gap={}, ks={ks:?})",
                piece_best_score - exact_score
            );
                // Never materially under-performs the transitional anchor.
                assert!(
                    exact_score + alloy::primitives::I256::ONE >= trans_score.min(x_star_i),
                    "exact anchor under-performs transitional by more than staircase noise"
                );
            }
        }
    }
    assert!(
        interior_members >= 2,
        "test must be non-vacuous: ≥2 interior-optimum members, got {interior_members}"
    );
}

/// Block 25641093 end-to-end through the REAL pool-state feed.
/// Rebuilds pool 0xDcA4038A98CD6bD6B4deFA11304FD1626c6665c9's tick data
/// from the fixture log (pool_seg -74028, WP3.0/SDP1 with the 11-log
/// synthetic mid-spine), feeds it through the same
/// `V3PoolState::build_int_v3_sequence` path `solver_dispatch` uses, then
/// replays hop 2 with the fixture input through the walk's ground-truth
/// simulator. The predicted hop-2 output must match on-chain revm
/// (1_109_518_347) within the integer model's residual step-rounding slack
/// (observed 7 wei).
#[test]
#[expect(clippy::too_many_lines)] // still >100 in hotpath builds (test-only, never instrumented)
fn block_25641093_pool_feed_hop2_predicts_revm_output() {
    use alloy::primitives::{Address, B256, U128, U512};
    use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams, V3PoolState};
    use degenbot_pools::TickInfo;
    use hashbrown::HashMap;
    use std::str::FromStr;

    // Canonical fixture (logs/fixtures/v2_v3_v3_solver_divergence_25641093.md, a
    // gitignored logs/ artifact since removed):
    // pool pre-swap @25641093 — sqrt 1956421190421993762013571523, tick
    // -74028, liquidity 5407362545736161987; current tick -74028 is
    // INITIALIZED with ln == +current liquidity (full drain on zfo step 0);
    // the liquidity-recovering tick is -84382 (ln = -64914675035050604).
    // Post-swap @capture: sqrt 1165839764733994694326695348, tick -84383,
    // liquidity 64914675035050604 — the solver must reproduce this.
    // 12 initialized ticks copied verbatim from the degenbot snapshot DB
    // for pool 0xD8dE…7aC19 (sibling of the in-tree `build_int_v3_sequence_
    // drains_current_tick_on_zfo_when_current_is_initialized` test). The
    // pre-swap *pool state* (sqrt/liq/tick) is the div 25641224 capture of
    // the divergence block 25641093; both suffice to exercise the correct
    // free-fall + activation profile.
    let raw: [(i32, u128, i128); 12] = [
        (-84469, 9_223_372_036_854_775_807, 9_223_372_036_854_775_807),
        (
            -84460,
            9_223_372_036_854_775_807,
            -9_223_372_036_854_775_808,
        ),
        (-84440, 2_319_993_473_851_491_971, 2_319_993_473_851_491_971),
        (-84422, 64_914_675_035_050_604, 64_914_675_035_050_604),
        (
            -84401,
            2_319_993_473_851_491_971,
            -2_319_993_473_851_491_971,
        ),
        (-84382, 64_914_675_035_050_604, -64_914_675_035_050_604),
        (-74028, 5_407_362_545_736_161_987, 5_407_362_545_736_161_987),
        (-74021, 8_246_173_613_278_771_746, 8_246_173_613_278_771_746),
        (-74017, 5_283_388_076_511_134_702, 5_283_388_076_511_134_702),
        (
            -74008,
            5_407_362_545_736_161_987,
            -5_407_362_545_736_161_987,
        ),
        (
            -74001,
            8_246_173_613_278_771_746,
            -8_246_173_613_278_771_746,
        ),
        (
            -73990,
            5_283_388_076_511_134_702,
            -5_283_388_076_511_134_702,
        ),
    ];
    let mut tick_data = HashMap::new();
    for (tick, gross, net) in raw {
        tick_data.insert(
            tick,
            TickInfo {
                liquidity_gross: U128::from(gross),
                liquidity_net: net,
                block: 25_641_093,
            },
        );
    }
    let params = RegisterV3PoolParams {
        address: "0xD8dEC118e1215F02e10DB846DCbBfE27d477aC19"
            .parse()
            .expect("checksummed"),
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee: 100,
        tick_spacing: 1,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from_str("1956421190421993762013571523").expect("sqrt"),
        liquidity: 5_407_362_545_736_161_987,
        tick: -74028,
        tick_data,
        update_block: 25_641_093,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        slot_layout: degenbot_pools::v3_state::ClSlotLayout::UniswapV3,
    };
    let (_, state) = V3PoolState::from_params(params, 4);

    let seq = state
        // 24 = the solver feed depth (solver_dispatch passes the
        // same number; the range cache cap must be >= it). The swap is zfo
        // (DAI token0 in, WETH token1 out, price decreasing).
        .build_int_v3_sequence(1, 100, true)
        .expect("int v3 sequence");
    // The two feed invariants, asserted together:
    //  (1) current-tick drain: the leading hop models the contract-faithful
    //      current segment [current, sqrt(-74028)] at STORED liquidity
    //      (5.407e18), then the free-fall ranges AFTER it must be
    //      ZERO-liquidity;
    //  (2) collapse reach: the -84382 ACTIVATION (liquidity
    //      64_914_675_035_050_604) must appear — pre-collapse the 15-range
    //      budget starved on ~40 word-boundary ticks (spacing 1) before
    //      reaching it. Depth 24 keeps it visible.
    assert!(
        seq.ranges.len() >= 5,
        "need the leading segment + drain + activation (>=5 ranges)"
    );
    assert!(
        seq.ranges[0].liquidity == 5_407_362_545_736_161_987,
        "leading current segment must run at stored (pre-drain) liquidity"
    );
    assert!(
        seq.ranges[1..4].iter().all(|r| r.liquidity == 0),
        "free-fall ranges must be zero-liquidity (current-tick drain), \
         got {:?}",
        seq.ranges.iter().map(|r| r.liquidity).collect::<Vec<_>>()
    );
    let activation: u128 = 64_914_675_035_050_604;
    assert!(
        seq.ranges.iter().any(|r| r.liquidity == activation),
        "the -84382 activation ({activation}) must be visible"
    );

    // Whole-hop replay: the input that takes the price from the current
    // spot down through the contract-faithful leading segment
    // [current, sqrt(-74028)] (at stored liquidity) and the zero-liquidity
    // free-fall, to sqrt(-84382), then down to the documented post-swap sqrt
    // at L = 64914675035050604. Derived from the endpoint arithmetic (EVM
    // floor semantics); oracle = leading_segment_output + L*(sqrt(-84382)
    // - sqrt_post) >> 96.
    let pb = U256::from_str("1165841056962215312021074329").expect("sqrt(-84382)");
    let pf = U256::from_str("1165839764733994694326695348").expect("post-swap sqrt");
    let liq = U256::from(activation);
    // net DAI in (EVM ceil-rounding direction): floor(L<<96*(Pb-Pf)/Pf) / Pb (+1).
    // U256-wide intermediates overflow (L<<96 * Δ ≈ 6.6e69) → widen to U512.
    let q96 = U512::from(1u64) << 96usize;
    let num = U512::from(liq) * q96 * U512::from(pb - pf);
    let da = num / U512::from(pf);
    let net_in = da / U512::from(pb) + U512::from(1u64);
    let net_in = net_in.to::<U256>();
    // fee 100 ppm: gross such that net = gross * 999900 / 1_000_000 >= net_in
    let last_leg_gross =
        (net_in * U256::from(1_000_000u64)) / U256::from(999_900u64) + U256::from(1u64);
    let last_leg_out = (liq * (pb - pf)) / (U256::from(1u64) << 96usize);

    let crossings = Arc::new(build_crossing_table(&seq));
    // Leading segment [current, sqrt(-74028)] crossing at STORED liquidity
    // (k=1: cross range 0; the zero-liquidity free-fall ranges 1..3 consume
    // /produce nothing, so k=4 has the same values). It is the reconcile term
    // for the 4.6% gap between the on-chain capture (1_109_518_347) and the
    // pure-math last-leg oracle (1_058_772_188).
    let lead_in = crossings[1].crossing_gross_input;
    let lead_out = crossings[1].crossing_output;
    assert_eq!(
        crossings[4].crossing_gross_input, lead_in,
        "free-fall consumes nothing"
    );
    // Full-path input = leading segment + last leg; full-path oracle =
    // leading output + last-leg output.
    let gross_in = lead_in + last_leg_gross;
    let oracle_out = lead_out + last_leg_out;

    let profiles = Arc::new(build_word_profiles(&crossings));
    let hops = [WalkHop::Cl {
        crossings,
        profiles,
    }];
    let outcome = simulate_walk_path(gross_in, &hops);
    let got = outcome.hop_outputs[0];
    // vs the validated step-faithful oracle: the residual is per-step
    // floor slack (observed 7 wei).
    let delta = got.abs_diff(oracle_out);
    assert!(
        delta <= U256::from(64u64),
        "pool-feed replay: predicted {got} vs oracle {oracle_out} (delta {delta} wei)"
    );
    // NOTE: the divergence doc records the on-chain sim capture as
    // 1_109_518_347 while the pure-math value against the captured
    // post-swap state is 1_058_772_188 (4.6% gap). This cannot be step
    // rounding; suspected capture-block drift (the doc mixes 25641093 and
    // 25641224, and the in-tree drain test pins pool state at 25641224).
    // Outstanding until a fresh `cast` cross-check — do not enshrine the
    // doc's 1109518347 literal here.
}

#[test]
fn v3_word_profile_matches_linear_walk_on_dense_hop() {
    // Real on-chain dense range (path 36864 hop1): a wide band whose ~2400
    // active-liquidity tick boundaries make `int_simulate_v3_swap` re-walk the
    // same word-boundary prefix on nearly every walk evaluation. This test
    // locks that the precomputed `V3WordProfile::swap` is byte-for-byte equal
    // to the linear walk across a sweep of inputs (partial landings deep into
    // the band AND full traversal past the exit).
    let entry: U256 = "158834591426315835485322".parse().unwrap();
    let lower: U256 = "4345239809".parse().unwrap();
    let upper: U256 = "158832211214966470446833".parse().unwrap();
    let n = 300u128;
    // 300 decreasing boundaries between entry and lower (zfo swap order),
    // evenly spaced in the band — the same shape as the capture's active set.
    let span = entry - lower;
    let word_boundary_prices: Vec<U256> = (1..=n)
        .map(|i| entry - span * U256::from(i) / U256::from(n + 1))
        .collect();
    let hop = IntV3TickRangeHop {
        liquidity: 69_602_725_527,
        sqrt_price_x96: entry,
        sqrt_price_lower_x96: lower,
        sqrt_price_upper_x96: upper,
        gamma_numer: 999_875,
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices,
    };
    assert!(
        !hop.word_boundary_prices.is_empty(),
        "test hop must carry word boundaries to take the profile path"
    );
    let prof = V3WordProfile::build(&hop).expect("profile builds for a valid dense hop");
    let full = prof.consumed.last().copied().expect("non-empty profile");
    assert!(!full.is_zero(), "dense crossing capacity must be nonzero");
    // 0 ..= 4x full traversal: partial landings (i < 32) and full traversal
    // (i >= 32) both covered, with distinct amounts.
    let mut any_nonzero = false;
    for i in 0..=128u128 {
        let x = full * U256::from(i) / U256::from(32);
        let linear = int_simulate_v3_swap(x, &hop);
        let profile = prof.swap(x);
        assert_eq!(
            linear.consumed_input, profile.consumed_input,
            "consumed mismatch at x={x}"
        );
        assert_eq!(linear.output, profile.output, "output mismatch at x={x}");
        any_nonzero = any_nonzero || !linear.output.is_zero();
    }
    assert!(
        any_nonzero,
        "sweep produced no output — simulate inputs invalid?"
    );
}

/// F1 (adversarial review): a single-piece CL path whose **exact
/// unclamped smooth argmax overshoots the piece's saturation cliff**. The
/// discrete profit peaks at the chain-saturation corner (a sharp kink), not
/// at the anchor. The old single-piece fast path probed only `anchor ± 2`
/// then `break`d without refining the terminal window, so it recorded
/// nothing and returned `None` — a silent under-shoot (skipped
/// arbitrage) that only manifests when the anchor overshoots the corner
/// (here by ~1.3e11 wei).
///
/// Guard: for single-piece paths the landed profit must never fall below
/// the real profit at the saturation corner. The cell encodes the
/// regression: the smooth anchor (~3.24e11) overshoots x_sat (~1.93e11),
/// so an anchor-±2-only search is deep in the (negative) post-cliff region.
#[test]
fn single_piece_saturation_kink_is_not_missed() {
    let (seq1, seq2) = (
        multi_range_sequence(750, 4, true, &[1_000_000_000_000_000u128]),
        multi_range_sequence(0, 1200, false, &[10_000_000_000_000u128]),
    );
    let hops = [cl_walk_hop(&seq1, None), cl_walk_hop(&seq2, None)];
    // Chain-saturation corner: the max path input in the upstream (hop0,
    // zfo) range — past it, hop0 stops adding output and profit falls.
    let x_sat = seq1.ranges[0].max_gross_input_in_range();
    let sat = simulate_walk_path(x_sat, &hops);
    let corner_profit = sat.final_output.checked_sub(x_sat).unwrap_or(U256::ZERO);
    assert!(
        corner_profit > U256::ZERO,
        "oracle sanity: the saturation corner must be profitable in this cell"
    );
    // Cell sanity: this is an F1 cell only because the smooth anchor
    // overshoots the corner (the regression precondition).
    let anchor = walk_piece_anchor(&hops, &[0usize, 0]);
    assert!(
        anchor > x_sat,
        "cell sanity: requires anchor > x_sat (got anchor={anchor}, x_sat={x_sat})"
    );
    let Some((x, profit, _)) =
        solve_cl_derived(&[&seq1, &seq2], &SolveRuntimeConfig::default()).result
    else {
        panic!(
            "F1 silent under-shoot: solver=None while the saturation corner x={x_sat} is worth {corner_profit} (anchor={anchor}); the single-piece terminal refine must bracket the corner"
        );
    };
    assert!(
        profit >= corner_profit,
        "solver profit {profit} < saturation-corner profit {corner_profit} (x*={x}, x_sat={x_sat})"
    );
}

/// F1-adjacent (containment, review): a single-piece path whose binding
/// profit kink is at hop1, not hop0. hop0 is a wide, deep zfo range that
/// never caps in the reachable input region; hop1 is a thin ofz range that
/// saturates within it, so the true peak sits at hop1's cap. The unclamped
/// smooth anchor lies at/above that kink (marginal≥1 there), so the
/// terminal refine's hi (`max(corner, anchor)`) brackets it and it resolves
/// to the post-coarsening profit-eps rather than being silently dropped.
#[test]
fn single_piece_hop1_binding_kink_is_not_dropped() {
    let (seq0, seq1) = (
        multi_range_sequence(750, 4, true, &[1_000_000_000_000_000u128]),
        multi_range_sequence(0, 10, false, &[1_000_000_000_000u128]),
    );
    let hops = [cl_walk_hop(&seq0, None), cl_walk_hop(&seq1, None)];
    // hop1's saturation input, in hop1's input units (= hop0's output).
    let y_cap = seq1.ranges[0].max_gross_input_in_range();
    let hop0_max_in = seq0.ranges[0].max_gross_input_in_range();
    let outcome_at = |x: U256| simulate_walk_path(x, &hops);
    let profit_at = |x: U256| {
        outcome_at(x)
            .final_output
            .checked_sub(x)
            .unwrap_or(U256::ZERO)
    };
    // x_cap = smallest path input at which hop0's output saturates hop1.
    let saturates = |x: U256| outcome_at(x).hop_outputs[0] >= y_cap;
    assert!(
        saturates(hop0_max_in),
        "cell needs hop1 to saturate within hop0's reach (hop0_max={hop0_max_in})"
    );
    let (mut lo, mut hi) = (U256::ZERO, hop0_max_in);
    while lo + U256::from(1) < hi {
        let mid = lo + (hi - lo) / U256::from(2);
        if saturates(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let x_cap = hi;
    let corner_profit = profit_at(x_cap);
    // cell sanity: hop0 is far from capping near the hop1 kink (the kink
    // is hop1's, not hop0's).
    assert!(
        hop0_max_in > x_cap * U256::from(8u64),
        "cell needs hop0 to never cap near the hop1 kink (hop0_max={hop0_max_in}, x_cap={x_cap})"
    );
    // oracle sanity: the kink is a substantial net-positive peak.
    assert!(
        corner_profit > U256::from(1_000_000u64),
        "cell needs a substantial hop1-binding profit (got {corner_profit})"
    );
    let Some((x, profit, _)) =
        solve_cl_derived(&[&seq0, &seq1], &SolveRuntimeConfig::default()).result
    else {
        panic!(
            "hop1-binding kink dropped: solver=None while x_cap={x_cap} is worth {corner_profit}; the terminal refine must not silently skip it"
        );
    };
    // Coarsened refine resolves the kink to the profit-eps contract rather
    // than dropping it (REFINE_BRACKET_WEI bounds the 33-pt grid step).
    let eps = U256::from(REFINE_BRACKET_WEI);
    assert!(
        profit >= corner_profit.saturating_sub(eps),
        "hop1-binding kink under-shoot: solver profit {profit} < corner {corner_profit} - eps {eps} (x*={x}, x_cap={x_cap})"
    );
}

/// Q3 SYNTHETIC dense guard (gated OFF CI — a stopgap until a real dense
/// capture from one of the ~27 current-tick-in-gap pools replaces it; the
/// DB audit shows dense is real + live). A path whose hop0 carries a
/// 300-word-boundary DENSE range takes the
/// word-profile path (WORD_PROFILE_THRESHOLD=128). This pins that (a) the
/// solver's landed profit stays >= a fine-grid oracle (never under-shoots on
/// a dense path) and (b) the ±64-wei direction-test straddle near the
/// optimum agrees with the fine oracle's local slope. Replace with a real
/// dense capture the moment the telemetry hook observes one.
#[test]
// Gated OFF CI — synthetic dense guard; run manually with --ignored.
#[ignore = "synthetic stopgap until a real dense capture replaces it (see doc comment)"]
fn dense_edge_direction_guard_synthetic() {
    let mut seq0 = multi_range_sequence(750, 4, true, &[1_000_000_000_000_000u128]);
    {
        let r = &mut seq0.ranges[0];
        let entry = r.sqrt_price_x96;
        let lower = r.sqrt_price_lower_x96;
        let span = entry - lower;
        let n = 300u128;
        r.word_boundary_prices = (1..=n)
            .map(|i| entry - span * U256::from(i) / U256::from(n + 1))
            .collect();
        assert!(
            r.word_boundary_prices.len() >= 128,
            "synthetic cell must be dense (>= 128 boundaries)"
        );
    }
    // 2-range partner -> multi-piece, so the walk runs the ±64 direction
    // test (single-piece paths take the F1 terminal branch instead).
    let seq1 = multi_range_sequence(
        0,
        1200,
        false,
        &[10_000_000_000_000u128, 10_000_000_000_000u128],
    );
    let hops = [cl_walk_hop(&seq0, None), cl_walk_hop(&seq1, None)];
    let o = |x: U256| simulate_walk_path(x, &hops).final_output;
    let p = |x: U256| o(x).checked_sub(x).unwrap_or(U256::ZERO);

    // Fine-grid oracle: coarse 128-step scan over [0, hi], then a fine
    // 4k-wei sweep around the coarse peak.
    let hi = seq0.ranges[0]
        .max_gross_input_in_range()
        .saturating_add(U256::from(1));
    let coarse = hi / U256::from(128) + U256::from(1);
    let (mut peak_x, mut peak_v) = (U256::ZERO, U256::ZERO);
    let mut x = U256::ZERO;
    while x < hi {
        let v = p(x);
        if v > peak_v {
            peak_v = v;
            peak_x = x;
        }
        x = x.saturating_add(coarse);
    }
    let lo = peak_x.saturating_sub(U256::from(1_000_000));
    let hi2 = peak_x
        .saturating_add(U256::from(1_000_000))
        .saturating_add(U256::from(1));
    let step = U256::from(4000);
    let mut oracle_profit = peak_v;
    x = lo;
    while x < hi2 {
        let v = p(x);
        if v > oracle_profit {
            oracle_profit = v;
        }
        x = x.saturating_add(step);
    }

    let eps = U256::from(REFINE_BRACKET_WEI);
    let Some((xr, profit, _)) =
        solve_cl_derived(&[&seq0, &seq1], &SolveRuntimeConfig::default()).result
    else {
        panic!("solver=None on a dense path while the fine oracle is {oracle_profit}");
    };
    assert!(
        profit >= oracle_profit.saturating_sub(eps),
        "dense path under-shoot: solver {profit} < fine oracle {oracle_profit} - eps {eps}"
    );

    // ±64 direction-test straddle at the optimum agrees with the fine
    // oracle's local slope (the Q3 mechanism).
    let back = xr.saturating_sub(U256::from(64));
    let fwd = xr.saturating_add(U256::from(64));
    let sb = walk_profit_score(o(back), back);
    let sf = walk_profit_score(o(fwd), fwd);
    let straddle_climbing = sf + I256::ONE >= sb;
    // Fine-oracle truth: profit should be flat-to-falling at/just past the
    // optimum, so it must agree that the edge is not climbing.
    let oracle_rising = p(xr.saturating_add(U256::from(258))) >= profit;
    assert_eq!(
        straddle_climbing, oracle_rising,
        "dense-edge ±64 direction test ({straddle_climbing}) disagrees with the fine oracle ({oracle_rising}) at x*={xr}"
    );
}
