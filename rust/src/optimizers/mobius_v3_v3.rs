//! V3-V3 arbitrage solver.
//!
//! For paths with two V3 hops (both potentially crossing tick boundaries),
//! we use a nested approach:
//! - For each candidate ending range (k1) on hop 1, compute the crossing
//! - For each candidate ending range (k2) on hop 2, compute the crossing
//! - For each (k1, k2) pair, solve the 2-hop piecewise problem
//! - Return the best overall result

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::items_after_statements)]

use crate::optimizers::mobius::{
    golden_section_search_max, invert_hop_output, mobius_solve, simulate_path, HopState,
    INVALID_RANGE_PENALTY, RANGE_CAPACITY_MARGIN, SQRT_PRICE_REL_TOL,
};
use crate::optimizers::mobius_v3::{
    estimate_v3_final_sqrt_price, TickRangeCrossing, V3TickRangeHop, V3TickRangeSequence,
};

/// Compute the effective max_input constrained by V3 tick range capacities.
///
/// For a 2-hop V3-V3 path, the input `x` must satisfy:
/// 1. x ≤ hop1's range capacity (max_gross_input_in_range)
/// 2. simulate_path(x, &[hop1]) ≤ hop2's range capacity
///
/// Condition 2 is nonlinear, so we invert the single-hop Möbius formula:
/// output = γ·s·x / (r + γ·x)  →  x = output·r / (γ·(s - output))
///
/// Returns f64::INFINITY if no constraint applies.
#[must_use]
fn compute_range_constrained_max_input(
    v3_hop1: &V3TickRangeHop,
    v3_hop2: &V3TickRangeHop,
    user_max_input: Option<f64>,
) -> f64 {
    let max1 = v3_hop1.max_gross_input_in_range();
    let max2 = v3_hop2.max_gross_input_in_range();

    if max1 <= 0.0 || max2 <= 0.0 {
        return 0.0;
    }

    // Slightly under-estimate to avoid float64 boundary precision issues
    let max1 = max1 * (1.0 - RANGE_CAPACITY_MARGIN);
    let max2 = max2 * (1.0 - RANGE_CAPACITY_MARGIN);

    let hop1 = v3_hop1.to_hop_state();

    let max_x_for_hop2 = invert_hop_output(max2, &hop1).unwrap_or(f64::INFINITY);

    let constrained = max1.min(max_x_for_hop2);

    user_max_input.map_or(constrained, |m| constrained.min(m))
}

/// Solve a 2-hop V3-V3 path with range-constrained max_input and validate
/// that both hops stay within their tick range bounds.
///
/// Returns `(x, profit, iters)` if profitable and in-range, otherwise `(0.0, 0.0, iters)`.
fn solve_v3_v3_validated(
    v3_hop1: &V3TickRangeHop,
    v3_hop2: &V3TickRangeHop,
    max_input: Option<f64>,
) -> (f64, f64, u32) {
    let hop1 = v3_hop1.to_hop_state();
    let hop2 = v3_hop2.to_hop_state();
    let constrained_max = compute_range_constrained_max_input(v3_hop1, v3_hop2, max_input);
    let (x, profit, iters) = mobius_solve(&[hop1.clone(), hop2], Some(constrained_max));

    if profit > 0.0 {
        let final_p1 = estimate_v3_final_sqrt_price(x, v3_hop1);
        let output1 = simulate_path(x, std::slice::from_ref(&hop1));
        let final_p2 = estimate_v3_final_sqrt_price(output1, v3_hop2);
        if v3_hop1.contains_sqrt_price_tol(final_p1, SQRT_PRICE_REL_TOL)
            && v3_hop2.contains_sqrt_price_tol(final_p2, SQRT_PRICE_REL_TOL)
        {
            return (x, profit, iters);
        }
    }

    (0.0, 0.0, iters)
}

/// Solve V3-V3 arbitrage with full crossing support.
///
/// For two V3 hops, we iterate over all combinations of ending ranges
/// (k1, k2) and solve each as a 2-hop piecewise Möbius problem.
///
/// `max_candidates` controls how many ending ranges to check per hop
/// (default: 10). For most paths, the optimum is within the first 3,
/// but adversarial inputs with many tick crossings need more.
///
/// Returns `(optimal_input, profit, iterations)`.
#[allow(clippy::too_many_lines)]
pub fn solve_v3_v3(
    seq1: &V3TickRangeSequence,
    seq2: &V3TickRangeSequence,
    max_input: Option<f64>,
    max_candidates: usize,
) -> (f64, f64, u32) {
    // Fast path: both single-range → standard 2-hop Möbius
    // with range-constrained max_input
    if seq1.ranges.len() == 1 && seq2.ranges.len() == 1 {
        return solve_v3_v3_validated(&seq1.ranges[0], &seq2.ranges[0], max_input);
    }

    // General case: iterate over ending range combinations
    let n1 = max_candidates.min(seq1.ranges.len());
    let n2 = max_candidates.min(seq2.ranges.len());

    let mut best_x: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let mut best_iters: u32 = 0;

    // Get single-range solution as baseline (k1=0, k2=0)
    let (x_base, profit_base, iters_base) =
        solve_v3_v3_validated(&seq1.ranges[0], &seq2.ranges[0], max_input);
    if profit_base > 0.0 {
        best_x = x_base;
        best_profit = profit_base;
        best_iters = iters_base;
    }

    // Case 1: Hop 1 crosses into range k1, Hop 2 stays in current range
    for k1 in 1..n1 {
        let Ok(crossing1) = seq1.compute_crossing(k1) else {
            continue;
        };

        let hops = [
            crossing1.ending_range.to_hop_state(),
            seq2.ranges[0].to_hop_state(),
        ];
        let (x, profit, iters) = solve_v3_v3_piecewise(
            Some(&crossing1),
            None,
            &hops,
            max_input,
            None,
            Some(&seq2.ranges[0]),
        );

        if profit > best_profit {
            best_x = x;
            best_profit = profit;
            best_iters = iters;
        }
    }

    // Case 2: Hop 1 stays, Hop 2 crosses into range k2
    for k2 in 1..n2 {
        let Ok(crossing2) = seq2.compute_crossing(k2) else {
            continue;
        };

        let hops = [
            seq1.ranges[0].to_hop_state(),
            crossing2.ending_range.to_hop_state(),
        ];
        let (x, profit, iters) = solve_v3_v3_piecewise(
            None,
            Some(&crossing2),
            &hops,
            max_input,
            Some(&seq1.ranges[0]),
            None,
        );

        if profit > best_profit {
            best_x = x;
            best_profit = profit;
            best_iters = iters;
        }
    }

    // Case 3: Both hops cross
    for k1 in 1..n1 {
        let Ok(crossing1) = seq1.compute_crossing(k1) else {
            continue;
        };
        for k2 in 1..n2 {
            let Ok(crossing2) = seq2.compute_crossing(k2) else {
                continue;
            };

            let hops = [
                crossing1.ending_range.to_hop_state(),
                crossing2.ending_range.to_hop_state(),
            ];
            let (x, profit, iters) = solve_v3_v3_piecewise(
                Some(&crossing1),
                Some(&crossing2),
                &hops,
                max_input,
                None,
                None,
            );

            if profit > best_profit {
                best_x = x;
                best_profit = profit;
                best_iters = iters;
            }
        }
    }

    if best_profit > 0.0 {
        (best_x, best_profit, best_iters)
    } else {
        (0.0, 0.0, best_iters)
    }
}

/// Compute the effective max_input for a piecewise V3-V3 search,
/// constrained by tick range capacities.
///
/// For hops with crossings, the range capacity is:
///   crossing_gross_input + ending_range.max_gross_input_in_range()
/// For hops without crossings, the range capacity comes from the V3HopInfo.
///
/// Returns f64::INFINITY if no constraint applies (unbounded).
#[must_use]
fn compute_piecewise_range_max(
    crossing1: Option<&TickRangeCrossing>,
    crossing2: Option<&TickRangeCrossing>,
    hops: &[HopState; 2],
    v3_hop1: Option<&V3TickRangeHop>,
    v3_hop2: Option<&V3TickRangeHop>,
    user_max_input: Option<f64>,
) -> f64 {
    let mut constraints: Vec<f64> = Vec::new();

    // Hop 1 constraint
    if let Some(c1) = crossing1 {
        // x must be ≤ crossing_gross_input + ending_range capacity
        let ending_capacity =
            c1.ending_range.max_gross_input_in_range() * (1.0 - RANGE_CAPACITY_MARGIN);
        constraints.push(c1.crossing_gross_input + ending_capacity);
    } else if let Some(v3) = v3_hop1 {
        // x must be ≤ range capacity
        constraints.push(v3.max_gross_input_in_range() * (1.0 - RANGE_CAPACITY_MARGIN));
    }

    // Hop 2 constraint (output of hop 1 must be ≤ range capacity)
    if let Some(c2) = crossing2 {
        let max_output_for_hop2 = c2.crossing_gross_input
            + c2.ending_range.max_gross_input_in_range() * (1.0 - RANGE_CAPACITY_MARGIN);
        if let Some(x) = invert_hop_output(max_output_for_hop2, &hops[0]) {
            constraints.push(x);
        }
    } else if let Some(v3) = v3_hop2 {
        let max2 = v3.max_gross_input_in_range() * (1.0 - RANGE_CAPACITY_MARGIN);
        if let Some(x) = invert_hop_output(max2, &hops[0]) {
            constraints.push(x);
        }
    }

    if let Some(max) = user_max_input {
        constraints.push(max);
    }

    constraints
        .into_iter()
        .reduce(f64::min)
        .unwrap_or(f64::INFINITY)
}

/// Solve a V3-V3 piecewise problem with one or both crossings.
///
/// Given crossing data for hop 1 and/or hop 2, find the optimal input
/// using golden section search on the piecewise profit function.
fn solve_v3_v3_piecewise(
    crossing1: Option<&TickRangeCrossing>,
    crossing2: Option<&TickRangeCrossing>,
    hops: &[HopState; 2],
    max_input: Option<f64>,
    v3_hop1: Option<&V3TickRangeHop>,
    v3_hop2: Option<&V3TickRangeHop>,
) -> (f64, f64, u32) {
    if hops.is_empty() {
        return (0.0, 0.0, 0);
    }

    // Single-range Möbius solution as starting point
    let constrained_max =
        compute_piecewise_range_max(crossing1, crossing2, hops, v3_hop1, v3_hop2, max_input);
    let (x_mobius, _, _) = mobius_solve(hops, Some(constrained_max));

    // Compute search bounds
    let x_min = match (crossing1, crossing2) {
        (Some(c1), _) => c1.crossing_gross_input * 1.001,
        (None, Some(c2)) => {
            let first_hop = &hops[0];
            let gamma = 1.0 - first_hop.fee;
            let est_min =
                c2.crossing_gross_input * first_hop.reserve_in / (gamma * first_hop.reserve_out);
            est_min * 1.001
        }
        _ => 1.0,
    };

    // x_max is constrained by range capacities
    let x_max = if constrained_max.is_finite() {
        constrained_max
    } else {
        (x_mobius * 10.0).max(x_min * 100.0)
    };

    if x_min >= x_max || x_min <= 0.0 {
        return (0.0, 0.0, 0);
    }

    let (x_best, iters) = golden_section_search_max(
        |x| compute_v3_v3_profit(x, crossing1, crossing2, hops, v3_hop1, v3_hop2),
        x_min,
        x_max,
    );
    let profit_best = compute_v3_v3_profit(x_best, crossing1, crossing2, hops, v3_hop1, v3_hop2);

    if profit_best > 0.0 {
        (x_best, profit_best, iters)
    } else {
        (0.0, 0.0, iters)
    }
}

/// Compute the profit for a V3-V3 path with crossings.
///
/// profit(x) = output(x) - x
///
/// The output depends on whether each hop has a crossing:
/// - No crossing: output = mobius(x)
/// - With crossing: output = crossing_output + mobius(remaining, ending_range)
///
/// Validates that the input stays within each hop's tick range bounds.
/// Returns [`INVALID_RANGE_PENALTY`] if the input would push the price out of range.
fn compute_v3_v3_profit(
    x: f64,
    crossing1: Option<&TickRangeCrossing>,
    crossing2: Option<&TickRangeCrossing>,
    hops: &[HopState; 2],
    v3_hop1: Option<&V3TickRangeHop>,
    v3_hop2: Option<&V3TickRangeHop>,
) -> f64 {
    // Hop 1 output
    let output1 = if let Some(c1) = crossing1 {
        if x < c1.crossing_gross_input {
            return INVALID_RANGE_PENALTY; // Invalid: can't cover crossing cost
        }
        let remaining = x - c1.crossing_gross_input;
        let final_sqrt_price = estimate_v3_final_sqrt_price(remaining, &c1.ending_range);
        if !c1
            .ending_range
            .contains_sqrt_price_tol(final_sqrt_price, SQRT_PRICE_REL_TOL)
        {
            return INVALID_RANGE_PENALTY; // Input pushes past ending range boundary
        }
        let ending_hop = c1.ending_range.to_hop_state();
        c1.crossing_output + simulate_path(remaining, &[ending_hop])
    } else {
        // No crossing — validate input stays in current range
        if let Some(v3) = v3_hop1 {
            let final_sqrt_price = estimate_v3_final_sqrt_price(x, v3);
            if !v3.contains_sqrt_price_tol(final_sqrt_price, SQRT_PRICE_REL_TOL) {
                return INVALID_RANGE_PENALTY; // Input pushes past range boundary
            }
        }
        simulate_path(x, &[hops[0].clone()])
    };

    // Hop 2 output (using output1 as input to hop 2)
    let output2 = if let Some(c2) = crossing2 {
        if output1 < c2.crossing_gross_input {
            return INVALID_RANGE_PENALTY; // Invalid: can't cover crossing cost
        }
        let remaining = output1 - c2.crossing_gross_input;
        let final_sqrt_price = estimate_v3_final_sqrt_price(remaining, &c2.ending_range);
        if !c2
            .ending_range
            .contains_sqrt_price_tol(final_sqrt_price, SQRT_PRICE_REL_TOL)
        {
            return INVALID_RANGE_PENALTY; // Input pushes past ending range boundary
        }
        let ending_hop = c2.ending_range.to_hop_state();
        c2.crossing_output + simulate_path(remaining, &[ending_hop])
    } else {
        // No crossing — validate input stays in current range
        if let Some(v3) = v3_hop2 {
            let final_sqrt_price = estimate_v3_final_sqrt_price(output1, v3);
            if !v3.contains_sqrt_price_tol(final_sqrt_price, SQRT_PRICE_REL_TOL) {
                return INVALID_RANGE_PENALTY; // Input pushes past range boundary
            }
        }
        simulate_path(output1, &[hops[1].clone()])
    };

    output2 - x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_v3_hop(
        liquidity: f64,
        sqrt_price: f64,
        sqrt_lower: f64,
        sqrt_upper: f64,
        fee: f64,
        zfo: bool,
    ) -> V3TickRangeHop {
        V3TickRangeHop {
            liquidity,
            sqrt_price_current: sqrt_price,
            sqrt_price_lower: sqrt_lower,
            sqrt_price_upper: sqrt_upper,
            fee,
            zero_for_one: zfo,
        }
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn test_v3_v3_single_range() {
        let hop1 = make_v3_hop(1e18, 46.9, 30.0, 70.0, 0.003, true);
        let hop2 = make_v3_hop(1e18, 44.7, 30.0, 70.0, 0.003, false);

        let seq1 = V3TickRangeSequence::new(vec![hop1]).unwrap();
        let seq2 = V3TickRangeSequence::new(vec![hop2]).unwrap();

        let (x, profit, iters) = solve_v3_v3(&seq1, &seq2, None, 10);

        assert!(x > 0.0, "Should find positive input");
        assert!(profit > 0.0, "Should find positive profit");
        assert_eq!(iters, 0, "Single range should use Möbius fast path");
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn test_v3_v3_multi_range_no_panic() {
        let ranges1 = vec![
            make_v3_hop(1e18, 46.9, 42.0, 52.0, 0.003, true),
            make_v3_hop(2e18, 52.0, 42.0, 62.0, 0.003, true),
        ];
        let ranges2 = vec![
            make_v3_hop(1e18, 44.7, 40.0, 50.0, 0.003, false),
            make_v3_hop(2e18, 40.0, 35.0, 45.0, 0.003, false),
        ];

        let seq1 = V3TickRangeSequence::new(ranges1).unwrap();
        let seq2 = V3TickRangeSequence::new(ranges2).unwrap();

        let (_x, _profit, _iters) = solve_v3_v3(&seq1, &seq2, None, 10);
    }

    /// Regression test: narrow tick range where Möbius k=0 baseline
    /// would exceed range bounds. The solver must constrain the
    /// optimal input to stay within the range.
    #[allow(clippy::unwrap_used)]
    #[test]
    fn test_v3_v3_narrow_range_stays_in_bounds() {
        // Stablecoin-like: narrow tick range (100 ticks), current price
        // near the middle, high liquidity.
        // The unconstrained Möbius optimum would push past the boundary.
        let sqrt_p1 = 1.0075; // tick ~150
        let sqrt_lower1 = 1.0050; // tick 100
        let sqrt_upper1 = 1.0100; // tick 200
        let sqrt_p2 = 0.9925; // tick ~-150
        let sqrt_lower2 = 0.9900; // tick ~-200
        let sqrt_upper2 = 0.9950; // tick ~-100

        let hop1 = make_v3_hop(1e18, sqrt_p1, sqrt_lower1, sqrt_upper1, 0.003, true);
        let hop2 = make_v3_hop(1e18, sqrt_p2, sqrt_lower2, sqrt_upper2, 0.003, false);

        let seq1 = V3TickRangeSequence::new(vec![hop1]).unwrap();
        let seq2 = V3TickRangeSequence::new(vec![hop2]).unwrap();

        let (x, profit, _iters) = solve_v3_v3(&seq1, &seq2, None, 10);

        // Should find a profitable result
        assert!(x > 0.0, "Should find positive input");
        assert!(profit > 0.0, "Should find positive profit");

        // The optimal input must not exceed the range capacity
        let max1 = seq1.ranges[0].max_gross_input_in_range();
        assert!(
            x <= max1 * 1.001, // tiny tolerance for float rounding
            "Optimal input {x} exceeds range capacity {max1}"
        );

        // The final sqrt price must stay within range
        let final_p1 = estimate_v3_final_sqrt_price(x, &seq1.ranges[0]);
        assert!(
            seq1.ranges[0].contains_sqrt_price(final_p1),
            "Final sqrt price {final_p1} out of range [{}, {}]",
            seq1.ranges[0].sqrt_price_lower,
            seq1.ranges[0].sqrt_price_upper
        );
    }

    #[test]
    fn test_max_gross_input_in_range_zfo() {
        // zfo: price goes down, max input = L * (1/sqrt_lower - 1/sqrt_current) / gamma
        let hop = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        let max_in = hop.max_gross_input_in_range();
        let gamma = 1.0 - 0.003;
        let expected = 1e18 * (1.0 / 900.0 - 1.0 / 1000.0) / gamma;
        assert!(
            (max_in - expected).abs() < 1.0,
            "max_gross_input = {max_in}, expected = {expected}"
        );
        assert!(max_in > 0.0);
    }

    #[test]
    fn test_max_gross_input_in_range_ofz() {
        // ofz: price goes up, max input = L * (sqrt_upper - sqrt_current) / gamma
        let hop = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, false);
        let max_in = hop.max_gross_input_in_range();
        let gamma = 1.0 - 0.003;
        let expected = 1e18 * (1100.0 - 1000.0) / gamma;
        assert!(
            (max_in - expected).abs() < 1.0,
            "max_gross_input = {max_in}, expected = {expected}"
        );
        assert!(max_in > 0.0);
    }

    #[test]
    fn test_max_gross_input_in_range_exhausted() {
        // Current price at boundary → range is exhausted in swap direction
        let hop = make_v3_hop(1e18, 900.0, 900.0, 1100.0, 0.003, true);
        let max_in = hop.max_gross_input_in_range();
        assert_eq!(max_in, 0.0, "Exhausted range should have 0 max input");
    }

    #[test]
    fn test_compute_v3_v3_profit_rejects_out_of_range() {
        // Verify that compute_v3_v3_profit returns INVALID_RANGE_PENALTY when input
        // pushes past a range boundary (no crossing case)
        let hop = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        let hop_state = hop.to_hop_state();

        // Small input (in range) → positive profit potential.
        // Input must be small enough that hop1's output stays within hop2's range.
        let small_profit = compute_v3_v3_profit(
            1e6,
            None,
            None,
            &[hop_state.clone(), hop_state],
            Some(&hop),
            Some(&hop),
        );
        // Not INVALID_RANGE_PENALTY
        assert!(
            small_profit > INVALID_RANGE_PENALTY / 10.0,
            "Small input should be valid, got {small_profit}"
        );

        // Huge input (out of range) → rejected
        let hop_state2 = hop.to_hop_state();
        let huge_profit = compute_v3_v3_profit(
            1e18,
            None,
            None,
            &[hop_state2.clone(), hop_state2],
            Some(&hop),
            Some(&hop),
        );
        assert_eq!(
            huge_profit, INVALID_RANGE_PENALTY,
            "Huge input should be rejected as out of range"
        );
    }
}
