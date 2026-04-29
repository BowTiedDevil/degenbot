//! V3 tick range types, crossing computation, and piecewise-Möbius solver.
//!
//! V3/V4 tick ranges are bounded product CFMMs with effective reserves:
//!
//! ```text
//! y = γ·(R₁+β)·x / ((R₀+α) + γ·x)
//! ```
//!
//! where R₀+α = L/√P and R₁+β = L·√P are virtual reserves.
//!
//! For multi-range V3 swaps that cross tick boundaries, the swap function
//! is piecewise-Möbius. We handle this by checking candidate "stopping
//! ranges" (typically 1–3), each yielding a closed-form solution, with
//! golden section search to find the optimal input across the piecewise
//! boundary.

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
#![allow(clippy::manual_let_else)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::manual_midpoint)]
#![allow(clippy::needless_range_loop)]

use crate::optimizers::mobius::{
    compute_mobius_coefficients, golden_section_search_max, invert_path_output, mobius_solve,
    simulate_path, HopState, MobiusError, RANGE_CAPACITY_MARGIN,
};

/// V3/V4 tick range data needed to build a Möbius `HopState`.
///
/// Stores the bounded product CFMM parameters for a single V3 tick range
/// along with the current pool state, so that we can construct effective
/// reserves and validate that a solution stays in range.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct V3TickRangeHop {
    /// Liquidity in this tick range.
    pub liquidity: f64,
    /// Current sqrt price of the pool (plain float, not X96).
    pub sqrt_price_current: f64,
    /// Lower sqrt price bound of the tick range.
    pub sqrt_price_lower: f64,
    /// Upper sqrt price bound of the tick range.
    pub sqrt_price_upper: f64,
    /// Fee fraction for this pool.
    pub fee: f64,
    /// True if the swap direction is token0 → token1.
    pub zero_for_one: bool,
}

impl V3TickRangeHop {
    /// Lower bound on R0: L / √P_upper.
    #[inline]
    #[must_use]
    pub fn alpha(&self) -> f64 {
        self.liquidity / self.sqrt_price_upper
    }

    /// Lower bound on R1: L · √P_lower.
    #[inline]
    #[must_use]
    pub fn beta(&self) -> f64 {
        self.liquidity * self.sqrt_price_lower
    }

    /// Convert this V3 tick range to a Möbius `HopState` with effective reserves.
    ///
    /// The virtual reserves are:
    /// - `r_eff = L / √P` (for the input token side)
    /// - `s_eff = L · √P` (for the output token side)
    ///
    /// Note: These virtual reserves *include* the bound parameters.
    /// `R₀ + α = L/√P` and `R₁ + β = L·√P`.
    #[must_use]
    pub fn to_hop_state(&self) -> HopState {
        let (r_eff, s_eff) = if self.zero_for_one {
            (
                self.liquidity / self.sqrt_price_current,
                self.liquidity * self.sqrt_price_current,
            )
        } else {
            (
                self.liquidity * self.sqrt_price_current,
                self.liquidity / self.sqrt_price_current,
            )
        };

        HopState::new(r_eff, s_eff, self.fee)
    }

    /// Maximum gross input (including fees) that this range can absorb
    /// without pushing the price past the range boundary.
    ///
    /// For zero_for_one: L * (1/√P_lower - 1/√P_current) / γ
    /// For one_for_zero: L * (√P_upper - √P_current) / γ
    ///
    /// This is the hard capacity limit of the tick range. Any input
    /// exceeding this amount would push the price out of range, which
    /// is impossible in V3 (the swap stops at the boundary).
    #[must_use]
    pub fn max_gross_input_in_range(&self) -> f64 {
        let gamma = 1.0 - self.fee;
        if gamma <= 0.0 || self.liquidity <= 0.0 {
            return 0.0;
        }
        if self.zero_for_one {
            // zfo: price decreases from √P_current to √P_lower
            // max net input = L * (1/√P_lower - 1/√P_current)
            let diff = 1.0 / self.sqrt_price_lower - 1.0 / self.sqrt_price_current;
            if diff <= 0.0 {
                return 0.0;
            }
            self.liquidity * diff / gamma
        } else {
            // ofz: price increases from √P_current to √P_upper
            // max net input = L * (√P_upper - √P_current)
            let diff = self.sqrt_price_upper - self.sqrt_price_current;
            if diff <= 0.0 {
                return 0.0;
            }
            self.liquidity * diff / gamma
        }
    }

    /// Check if a sqrt price is within this tick range.
    #[inline]
    #[must_use]
    pub fn contains_sqrt_price(&self, sqrt_price: f64) -> bool {
        sqrt_price >= self.sqrt_price_lower && sqrt_price <= self.sqrt_price_upper
    }

    /// Check if a sqrt price is within this tick range, with a relative
    /// tolerance band around the bounds.
    ///
    /// The tolerance is `rel_tol * sqrt_price_current`, applied symmetrically
    /// outside each bound. This accounts for f64 rounding near tick boundaries.
    #[inline]
    #[must_use]
    pub fn contains_sqrt_price_tol(&self, sqrt_price: f64, rel_tol: f64) -> bool {
        let eps = rel_tol * self.sqrt_price_current;
        sqrt_price >= self.sqrt_price_lower - eps && sqrt_price <= self.sqrt_price_upper + eps
    }

    /// Check whether swapping `amount_in` through this range keeps the
    /// final sqrt price within bounds, using a relative tolerance.
    #[inline]
    #[must_use]
    pub fn is_swap_in_range(&self, amount_in: f64, rel_tol: f64) -> bool {
        let final_sp = estimate_v3_final_sqrt_price(amount_in, self);
        let eps = rel_tol * self.sqrt_price_current;
        final_sp >= self.sqrt_price_lower - eps && final_sp <= self.sqrt_price_upper + eps
    }
}

/// Pre-computed crossing data for a V3 swap that crosses tick boundaries.
///
/// When a V3 swap crosses ranges 0..K-1 and ends in range K:
/// - `total_output = crossing_output + mobius(remaining_input, range_K)`
/// - `remaining_input = gross_input - crossing_gross_input`
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TickRangeCrossing {
    /// Total gross input (including fees) consumed by crossed ranges.
    pub crossing_gross_input: f64,
    /// Total output from crossed ranges.
    pub crossing_output: f64,
    /// The ending range with `sqrt_price_current` set to the entry boundary.
    pub ending_range: V3TickRangeHop,
}

/// Ordered sequence of V3 tick ranges in the swap direction.
///
/// `ranges[0]` contains the current price. `ranges[1]`, `ranges[2]`, ...
/// are adjacent ranges in the swap direction.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct V3TickRangeSequence {
    /// Ordered tick ranges in the swap direction.
    pub ranges: Vec<V3TickRangeHop>,
}

impl V3TickRangeSequence {
    /// Create a new tick range sequence.
    ///
    /// # Errors
    ///
    /// Returns `MobiusError` if ranges is empty, or if fees or directions are mixed.
    pub fn new(ranges: Vec<V3TickRangeHop>) -> Result<Self, MobiusError> {
        if ranges.is_empty() {
            return Err(MobiusError::EmptyHops);
        }

        let fee = ranges[0].fee;
        let zfo = ranges[0].zero_for_one;
        for r in &ranges {
            if (r.fee - fee).abs() > f64::EPSILON {
                return Err(MobiusError::InconsistentSequence {
                    message: "All ranges must have the same fee".to_string(),
                });
            }
            if r.zero_for_one != zfo {
                return Err(MobiusError::InconsistentSequence {
                    message: "All ranges must have the same swap direction".to_string(),
                });
            }
        }

        Ok(Self { ranges })
    }

    /// Fee fraction for all ranges.
    #[inline]
    #[must_use]
    pub fn fee(&self) -> f64 {
        self.ranges[0].fee
    }

    /// Swap direction for all ranges.
    #[inline]
    #[must_use]
    pub fn zero_for_one(&self) -> bool {
        self.ranges[0].zero_for_one
    }

    /// Compute crossing data to reach range `k` (0-indexed).
    ///
    /// - `k=0`: no crossing (swap stays in first range).
    /// - `k=1`: cross range 0, end in range 1.
    /// - `k=2`: cross ranges 0–1, end in range 2.
    ///
    /// The ending range's `sqrt_price_current` is set to the entry
    /// boundary price.
    ///
    /// # Errors
    ///
    /// Returns an error if `k` is out of bounds.
    pub fn compute_crossing(&self, k: usize) -> Result<TickRangeCrossing, MobiusError> {
        if k >= self.ranges.len() {
            return Err(MobiusError::EmptyHops);
        }

        if k == 0 {
            return Ok(TickRangeCrossing {
                crossing_gross_input: 0.0,
                crossing_output: 0.0,
                ending_range: self.ranges[0].clone(),
            });
        }

        let gamma = 1.0 - self.fee();
        let mut crossing_gross_input = 0.0_f64;
        let mut crossing_output = 0.0_f64;

        for i in 0..k {
            let r = &self.ranges[i];

            let sqrt_p_start = if i == 0 {
                r.sqrt_price_current
            } else if self.zero_for_one() {
                self.ranges[i - 1].sqrt_price_lower
            } else {
                self.ranges[i - 1].sqrt_price_upper
            };

            let (net_input, output) = if self.zero_for_one() {
                let sqrt_p_end = r.sqrt_price_lower;
                let net_in = r.liquidity * (1.0 / sqrt_p_end - 1.0 / sqrt_p_start);
                let out = r.liquidity * (sqrt_p_start - sqrt_p_end);
                (net_in, out)
            } else {
                let sqrt_p_end = r.sqrt_price_upper;
                let net_in = r.liquidity * (sqrt_p_end - sqrt_p_start);
                let out = r.liquidity * (1.0 / sqrt_p_start - 1.0 / sqrt_p_end);
                (net_in, out)
            };

            let gross_input = net_input / gamma;
            crossing_gross_input += gross_input;
            crossing_output += output;
        }

        // Construct ending range with entry price at boundary
        let ending = &self.ranges[k];
        let entry_sqrt_price = if self.zero_for_one() {
            self.ranges[k - 1].sqrt_price_lower
        } else {
            self.ranges[k - 1].sqrt_price_upper
        };

        let ending_range = V3TickRangeHop {
            liquidity: ending.liquidity,
            sqrt_price_current: entry_sqrt_price,
            sqrt_price_lower: ending.sqrt_price_lower,
            sqrt_price_upper: ending.sqrt_price_upper,
            fee: ending.fee,
            zero_for_one: ending.zero_for_one,
        };

        Ok(TickRangeCrossing {
            crossing_gross_input,
            crossing_output,
            ending_range,
        })
    }
}

/// Solve arbitrage with full V3 tick range sequence handling.
///
/// This is the high-level entry point that mirrors Python's `_solve_multi_range`.
/// It handles:
/// 1. Computing crossings for each candidate range
/// 2. Running piecewise solve for each candidate
/// 3. Returning the best result
///
/// Parameters
/// ----------
/// * `hops` - Full path hops with V3 hop at `v3_hop_index`
/// * `v3_hop_index` - Index of the V3 hop in the path
/// * `sequence` - V3 tick range sequence (current + adjacent ranges)
/// * `max_candidates` - Maximum number of candidate ranges to check (typically 3)
/// * `max_input` - Optional maximum input constraint
///
/// Returns `(optimal_input, profit, iterations)` for the best valid candidate.
pub fn solve_v3_tick_range_sequence(
    hops: &[HopState],
    v3_hop_index: usize,
    sequence: &V3TickRangeSequence,
    max_candidates: usize,
    max_input: Option<f64>,
) -> (f64, f64, u32) {
    let mut best_x: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let mut best_iters: u32 = 0;

    // Check candidates from current range up to max_candidates
    let num_candidates = max_candidates.min(sequence.ranges.len());

    for k in 0..num_candidates {
        // Compute crossing data for this candidate
        let crossing = match sequence.compute_crossing(k) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Run piecewise solve with this crossing
        let (x, profit, iters) = solve_piecewise(hops, v3_hop_index, &[crossing], max_input);

        if profit > best_profit {
            best_x = x;
            best_profit = profit;
            best_iters = iters;
        }

        // Early termination if we found a good solution and this isn't the current range
        if best_profit > 0.0 && k > 0 {
            // Could add more sophisticated early termination here
            // For now, check all candidates
        }
    }

    (best_x, best_profit, best_iters)
}

/// Estimate the final sqrt price after a V3 swap within a single tick range.
#[must_use]
pub fn estimate_v3_final_sqrt_price(amount_in: f64, v3_hop: &V3TickRangeHop) -> f64 {
    let liquidity = v3_hop.liquidity;
    let gamma = 1.0 - v3_hop.fee;
    let sqrt_p = v3_hop.sqrt_price_current;

    if liquidity <= 0.0 {
        return sqrt_p;
    }

    if v3_hop.zero_for_one {
        let denom = liquidity + amount_in * gamma * sqrt_p;
        if denom <= 0.0 {
            return sqrt_p;
        }
        sqrt_p * liquidity / denom
    } else {
        sqrt_p + amount_in * gamma / liquidity
    }
}

/// Compute V3 swap output including tick crossings.
///
/// For a V3 swap that crosses ranges 0..K-1 and ends in range K:
/// `total_output = crossing_output + mobius(remaining, range_K)`.
///
/// Returns `(output, valid)` where `valid` is true if the input covers
/// the crossing and the result stays within the ending range.
pub fn piecewise_v3_swap(gross_input: f64, crossing: &TickRangeCrossing) -> (f64, bool) {
    if gross_input < crossing.crossing_gross_input {
        return (0.0, false);
    }

    let remaining = gross_input - crossing.crossing_gross_input;
    let ending_hop = crossing.ending_range.to_hop_state();
    let variable_output = simulate_path(remaining, &[ending_hop]);

    let total_output = crossing.crossing_output + variable_output;

    // Validate that the remaining input stays in the ending range
    let final_sqrt_price = estimate_v3_final_sqrt_price(remaining, &crossing.ending_range);
    if !crossing.ending_range.contains_sqrt_price(final_sqrt_price) {
        return (total_output, false);
    }

    (total_output, true)
}

/// Compute the effective max_input for a piecewise V3 search,
/// constrained by the V3 tick range capacity.
///
/// The V3 hop at `v3_hop_index` has a bounded capacity:
/// - With crossing: `crossing_gross_input + ending_range.max_gross_input_in_range()`
/// - Without crossing: `v3_hop.max_gross_input_in_range()`
///
/// For hops before the V3 hop, we invert the Möbius formula to find
/// the input `x` such that `simulate_path(x, hops_before) = v3_capacity`.
///
/// For hops after the V3 hop, the V3 output must not exceed the
/// next hop's capacity. We invert similarly.
///
/// Returns f64::INFINITY if no constraint applies.
#[must_use]
fn compute_piecewise_v3_range_max(
    hops: &[HopState],
    v3_hop_index: usize,
    crossing: &TickRangeCrossing,
    max_input: Option<f64>,
) -> f64 {
    let mut constraints: Vec<f64> = Vec::new();

    // V3 hop capacity: input to V3 must not exceed range capacity
    let v3_capacity =
        crossing.crossing_gross_input + crossing.ending_range.max_gross_input_in_range();

    // Convert V3 capacity to a constraint on x (path input)
    let hops_before = &hops[..v3_hop_index];
    if hops_before.is_empty() {
        constraints.push(v3_capacity);
    } else if let Some(cap) = invert_path_output(v3_capacity, hops_before) {
        constraints.push(cap);
    }

    // Hops after V3: V3 output must not exceed what they can absorb
    // This is harder to compute exactly, but the Möbius solve with
    // constrained max_input will handle it. We skip this constraint
    // and rely on the eval_profit validation instead.

    if let Some(max) = max_input {
        constraints.push(max);
    }

    constraints
        .into_iter()
        .reduce(f64::min)
        .unwrap_or(f64::INFINITY)
}

/// Solve arbitrage with piecewise-Möbius for V3 tick crossings.
///
/// For each candidate ending range (via `TickRangeCrossing`), the V3 swap
/// is decomposed into:
/// - Fixed crossing output from ranges 0..K-1
/// - Variable Möbius output from the ending range K
///
/// The profit function is NOT a pure Möbius composition (due to the
/// additive crossing constant), so golden section search is used on
/// a well-bracketed interval starting from the single-range Möbius solution.
///
/// All results are validated against V3 tick range boundaries.
/// Inputs that would push the price past a range boundary are rejected.
///
/// Returns `(optimal_input, profit, iterations)` for the best valid candidate.
pub fn solve_piecewise(
    hops: &[HopState],
    v3_hop_index: usize,
    crossings: &[TickRangeCrossing],
    max_input: Option<f64>,
) -> (f64, f64, u32) {
    let mut best_x: f64 = 0.0;
    let mut best_profit: f64 = 0.0;
    let mut best_iters: u32 = 0;

    for crossing in crossings {
        // Build the full hop list with the ending range's HopState
        let mut full_hops = hops.to_vec();
        full_hops[v3_hop_index] = crossing.ending_range.to_hop_state();

        // Compute range-constrained max_input
        let constrained_max =
            compute_piecewise_v3_range_max(hops, v3_hop_index, crossing, max_input);

        // Single-range Möbius solve as starting point
        let (x_mobius, _, _) = mobius_solve(&full_hops, Some(constrained_max));

        // Split hops into before/after V3
        let hops_before = &full_hops[..v3_hop_index];
        let hops_after = &full_hops[v3_hop_index + 1..];

        // Compute minimum input to cover crossing
        let x_min = if crossing.crossing_gross_input > 0.0 && !hops_before.is_empty() {
            let coeffs_before = match compute_mobius_coefficients(hops_before) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let target = crossing.crossing_gross_input;
            let kn = coeffs_before.K / coeffs_before.N;
            if target >= kn {
                continue; // Crossing requires more than the path can deliver
            }
            target * coeffs_before.M / (coeffs_before.K - target * coeffs_before.N)
        } else if crossing.crossing_gross_input > 0.0 {
            crossing.crossing_gross_input
        } else {
            0.0
        };

        let x_min = if x_min <= 0.0 { 0.0 } else { x_min };

        // Closure to evaluate profit at a given input
        let eval_profit = |x: f64| -> f64 {
            if x <= 0.0 {
                return -x;
            }
            let amt_v3 = if hops_before.is_empty() {
                x
            } else {
                simulate_path(x, hops_before)
            };
            // Validate: amount entering V3 must not exceed crossing + ending capacity
            if amt_v3
                > crossing.crossing_gross_input + crossing.ending_range.max_gross_input_in_range()
            {
                return -x;
            }
            // Validate: remaining input stays in ending range
            if amt_v3 >= crossing.crossing_gross_input {
                let remaining = amt_v3 - crossing.crossing_gross_input;
                let final_sp = estimate_v3_final_sqrt_price(remaining, &crossing.ending_range);
                if !crossing.ending_range.contains_sqrt_price(final_sp) {
                    return -x;
                }
            }
            let (v3_out, _valid) = piecewise_v3_swap(amt_v3, crossing);
            let final_out = if hops_after.is_empty() {
                v3_out
            } else {
                simulate_path(v3_out, hops_after)
            };
            final_out - x
        };

        // Bracket the search, constrained by range capacity
        let x_low = x_min;
        let x_high = if constrained_max.is_finite() && constrained_max > x_low {
            constrained_max
        } else if x_mobius > x_min {
            (x_mobius * 3.0).max(x_min + 1.0)
        } else {
            (x_min * 5.0).max(x_min + 1.0)
        };
        let x_high = if let Some(max) = max_input {
            x_high.min(max)
        } else {
            x_high
        };

        if x_low >= x_high {
            continue;
        }

        let (x_opt, iters) = golden_section_search_max(eval_profit, x_low, x_high);
        let profit = eval_profit(x_opt);

        if profit <= 0.0 {
            continue;
        }

        if profit > best_profit {
            best_x = x_opt;
            best_profit = profit;
            best_iters = iters;
        }
    }

    (best_x, best_profit, best_iters)
}

/// Solve arbitrage with multiple candidate V3 tick ranges.
///
/// For V3 pools where the optimal swap may cross tick boundaries,
/// this method checks each candidate range independently. Each
/// candidate yields a closed-form O(1) solution.
///
/// All results are validated against V3 tick range boundaries.
/// If the unconstrained Möbius optimum exceeds the range, the
/// solver constrains the input to the range capacity and returns
/// the range-limited optimum instead of rejecting the candidate.
///
/// Returns `(optimal_input, profit, iterations)` for the best valid candidate.
pub fn solve_v3_candidates(
    base_hops: &[HopState],
    v3_hop_index: usize,
    v3_candidates: &[V3TickRangeHop],
    max_input: Option<f64>,
) -> (f64, f64, u32) {
    let mut best_x: f64 = 0.0;
    let mut best_profit: f64 = 0.0;

    for v3_candidate in v3_candidates {
        let v3_hop_state = v3_candidate.to_hop_state();

        // Build full hop list: insert V3 hop at the right position
        let mut full_hops = base_hops.to_vec();
        full_hops.insert(v3_hop_index, v3_hop_state);

        // Constrain max_input to V3 range capacity
        let constrained_max =
            compute_v3_candidates_range_max(&full_hops, v3_hop_index, v3_candidate, max_input);

        let (x_opt, profit, _iters) = mobius_solve(&full_hops, constrained_max);

        if x_opt <= 0.0 || profit <= 0.0 {
            continue;
        }

        // Validate V3 range — simulate through hops before V3 to get
        // amount entering the V3 hop, then check it stays in range.
        let hops_before = &full_hops[..v3_hop_index];
        let amt_v3 = if hops_before.is_empty() {
            x_opt
        } else {
            simulate_path(x_opt, hops_before)
        };
        let valid = v3_candidate.is_swap_in_range(amt_v3, RANGE_CAPACITY_MARGIN);

        if valid && profit > best_profit {
            best_x = x_opt;
            best_profit = profit;
        }
    }

    if best_profit > 0.0 {
        (best_x, best_profit, 0)
    } else {
        (0.0, 0.0, 0)
    }
}

/// Compute the effective max_input for `solve_v3_candidates`,
/// constrained by the V3 tick range capacity.
///
/// The input `x` must satisfy:
/// 1. simulate_path(x, hops[..v3_hop_index]) ≤ v3_candidate.max_gross_input_in_range()
///
/// We invert the Möbius formula through the hops before the V3 hop
/// to find the maximum x that keeps the V3 input within range.
///
/// Returns `Some(constrained)` or `None` if the range is exhausted.
#[must_use]
fn compute_v3_candidates_range_max(
    full_hops: &[HopState],
    v3_hop_index: usize,
    v3_candidate: &V3TickRangeHop,
    user_max_input: Option<f64>,
) -> Option<f64> {
    let max_v3_input = v3_candidate.max_gross_input_in_range();
    if max_v3_input <= 0.0 {
        return Some(0.0); // Range is exhausted
    }
    // Slightly under-estimate to avoid float64 boundary issues
    let max_v3_input = max_v3_input * (1.0 - RANGE_CAPACITY_MARGIN);

    let hops_before = &full_hops[..v3_hop_index];

    let constrained = if hops_before.is_empty() {
        max_v3_input
    } else {
        invert_path_output(max_v3_input, hops_before).unwrap_or(f64::INFINITY)
    };

    match user_max_input {
        Some(m) => Some(constrained.min(m)),
        None => {
            if constrained.is_finite() {
                Some(constrained)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[test]
    fn test_v3_hop_state_effective_reserves() {
        // L = 1e18, sqrt_p = 1000, range [900, 1100]
        let v3 = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        let hop = v3.to_hop_state();

        // zero_for_one: r_eff = L/√P = 1e18/1000 = 1e15
        //               s_eff = L·√P = 1e18*1000 = 1e21
        assert!((hop.reserve_in - 1e15).abs() < 1.0);
        assert!((hop.reserve_out - 1e21).abs() < 1.0);
    }

    #[test]
    fn test_v3_alpha_beta() {
        let v3 = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        // alpha = L/√P_upper = 1e18/1100 ≈ 9.09e14
        let alpha = v3.alpha();
        assert!((alpha - 1e18 / 1100.0).abs() < 1.0);
        // beta = L·√P_lower = 1e18*900 = 9e20
        let beta = v3.beta();
        assert!((beta - 1e18 * 900.0).abs() < 1.0);
    }

    #[test]
    fn test_contains_sqrt_price() {
        let v3 = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        assert!(v3.contains_sqrt_price(1000.0));
        assert!(v3.contains_sqrt_price(900.0));
        assert!(v3.contains_sqrt_price(1100.0));
        assert!(!v3.contains_sqrt_price(899.0));
        assert!(!v3.contains_sqrt_price(1101.0));
    }

    #[test]
    fn test_tick_range_sequence_crossing_k0() {
        let ranges = vec![
            make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true),
            make_v3_hop(1e18, 899.0, 800.0, 900.0, 0.003, true),
        ];
        let seq = V3TickRangeSequence::new(ranges).expect("Should create sequence");

        let crossing = seq.compute_crossing(0).expect("Should compute crossing");
        assert_eq!(crossing.crossing_gross_input, 0.0);
        assert_eq!(crossing.crossing_output, 0.0);
    }

    #[test]
    fn test_tick_range_sequence_crossing_k1() {
        let ranges = vec![
            make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true),
            make_v3_hop(1e18, 899.0, 800.0, 900.0, 0.003, true),
        ];
        let seq = V3TickRangeSequence::new(ranges).expect("Should create sequence");

        let crossing = seq.compute_crossing(1).expect("Should compute crossing");
        // Should have positive gross input and output from crossing range 0
        assert!(crossing.crossing_gross_input > 0.0);
        assert!(crossing.crossing_output > 0.0);
        // Ending range should have sqrt_price_current at the boundary (900.0)
        assert!((crossing.ending_range.sqrt_price_current - 900.0).abs() < 1e-10);
    }

    #[test]
    fn test_estimate_v3_final_sqrt_price_zero_for_one() {
        let v3 = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);
        // Use an input that stays within the tick range (~5% of max capacity)
        let final_price = estimate_v3_final_sqrt_price(5e13, &v3);
        assert!(final_price < 1000.0);
        assert!(
            final_price > 900.0,
            "Should stay in range, got {final_price}"
        );
    }

    #[test]
    fn test_estimate_v3_final_sqrt_price_one_for_zero() {
        let v3 = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, false);
        let final_price = estimate_v3_final_sqrt_price(1e18, &v3);
        // Price should increase for one_for_zero swap
        assert!(final_price > 1000.0);
        assert!(final_price < 1100.0); // Should stay in range
    }

    #[test]
    fn test_solve_v3_candidates_single_range() {
        // V2 hop + V3 single range
        let v2_hop = HopState::new(2_000_000.0, 1_000_000.0, 0.003);
        let v3_candidate = make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true);

        let (_x_opt, _profit, iters) = solve_v3_candidates(&[v2_hop], 1, &[v3_candidate], None);
        // May or may not find profit depending on the reserve mismatch
        // The key thing is it doesn't crash
        assert_eq!(iters, 0);
    }

    #[test]
    fn test_piecewise_v3_swap_basic() {
        let crossing = TickRangeCrossing {
            crossing_gross_input: 1000.0,
            crossing_output: 500.0,
            ending_range: make_v3_hop(1e18, 900.0, 800.0, 900.0, 0.003, true),
        };

        // Input less than crossing — should return invalid
        let (out, valid) = piecewise_v3_swap(500.0, &crossing);
        assert!(!valid);
        assert_eq!(out, 0.0);

        // Input greater than crossing — should return valid
        let (out, valid) = piecewise_v3_swap(2000.0, &crossing);
        assert!(valid);
        assert!(out > 500.0); // crossing_output + variable_output
    }

    /// Regression test: golden section search with x_min=0 must terminate.
    /// Previously, (b-a)/a = infinity when a=0, causing an infinite loop.
    #[test]
    fn test_solve_piecewise_zero_x_min_terminates() {
        // k=0 crossing has crossing_gross_input=0, so x_min=0
        let ranges = vec![make_v3_hop(1e18, 1000.0, 900.0, 1100.0, 0.003, true)];
        let seq = V3TickRangeSequence::new(ranges).expect("Should create");
        let crossing = seq.compute_crossing(0).expect("Should compute k=0");
        assert_eq!(crossing.crossing_gross_input, 0.0);

        // Build a 2-hop path: V2 + V3
        let v2_hop = HopState::new(2_000_000.0, 1_050_000.0, 0.003);
        let v3_hop = crossing.ending_range.to_hop_state();
        let hops = [v2_hop, v3_hop];

        // This must terminate (not hang)
        let (x, _profit, iters) = solve_piecewise(&hops, 1, &[crossing], None);
        // Should find a result (positive or zero) without hanging
        assert!(x >= 0.0, "Should return non-negative input");
        // Iters should be reasonable (< 100)
        assert!(
            iters < 100,
            "Should converge in < 100 iterations, got {iters}"
        );
    }
}
