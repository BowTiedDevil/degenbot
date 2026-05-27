//! Integer-exact V3 tick range representation and Möbius coefficient computation.
//!
//! Converts V3 concentrated liquidity parameters (L, √P, tick bounds) into
//! integer effective reserves compatible with the exact Möbius solver
//! (`exact_mobius_solve`).
//!
//! # V3 Effective Reserves
//!
//! A V3 tick range is a bounded-product CFMM with virtual reserves:
//!
//! ```text
//! R₀ + α = L / √P    (token0 virtual reserves, in Q96 integer form)
//! R₁ + β = L · √P    (token1 virtual reserves, in Q96 integer form)
//! ```
//!
//! Since √P is stored as `sqrtPriceX96 = √P · 2^96`, the effective reserves are:
//!
//! ```text
//! R₀ + α = (L · 2^96) / sqrtPriceX96
//! R₁ + β = (L · sqrtPriceX96) / 2^96
//! ```
//!
//! These are integer divisions that match EVM truncation semantics.
//!
//! # Fee Representation
//!
//! V3 fee is stored as `fee / 1_000_000` (e.g., 3000 = 0.3%).
//! Gamma = 1 - fee = (1_000_000 - fee) / 1_000_000.
//! We store `gamma_numer = 1_000_000 - fee`, `fee_denom = 1_000_000`.

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::similar_names)]
#![allow(clippy::unreadable_literal)]

use alloy::primitives::{U256, U512};

use crate::optimizers::mobius_int::{IntHopState, IntMobiusCoefficients};

// ---------------------------------------------------------------------------
// Integer V3 Tick Range Hop
// ---------------------------------------------------------------------------

/// V3 tick range data with integer (U256) sqrt price and u128 liquidity.
///
/// Unlike [`crate::optimizers::mobius_v3::V3TickRangeHop`] which stores
/// liquidity and sqrt price as f64, this struct preserves full integer
/// precision for EVM-exact computation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntV3TickRangeHop {
    /// Active liquidity in this tick range (u128 — same as Solidity).
    pub liquidity: u128,
    /// Current sqrt price of the pool as Q128.96 (U256).
    pub sqrt_price_x96: U256,
    /// Lower sqrt price bound of the tick range as Q128.96 (U256).
    pub sqrt_price_lower_x96: U256,
    /// Upper sqrt price bound of the tick range as Q128.96 (U256).
    pub sqrt_price_upper_x96: U256,
    /// Fee numerator: gamma_numer = 1_000_000 - fee.
    /// E.g., for 0.3% fee: gamma_numer = 997_000, fee_denom = 1_000_000.
    pub gamma_numer: u64,
    /// Fee denominator: 1_000_000.
    pub fee_denom: u64,
    /// True if the swap direction is token0 → token1.
    pub zero_for_one: bool,
}

impl IntV3TickRangeHop {
    /// Compute the effective V3 reserves as an [`IntHopState`].
    ///
    /// For zero_for_one:
    /// - reserve_in = (L · 2^96) / sqrtPriceX96   (token0 virtual reserves)
    /// - reserve_out = (L · sqrtPriceX96) / 2^96   (token1 virtual reserves)
    ///
    /// For one_for_zero:
    /// - reserve_in = (L · sqrtPriceX96) / 2^96    (token1 virtual reserves)
    /// - reserve_out = (L · 2^96) / sqrtPriceX96   (token0 virtual reserves)
    ///
    /// The divisions match Solidity's truncation-toward-zero semantics.
    #[must_use]
    pub fn to_int_hop_state(&self) -> IntHopState {
        let (reserve_in, reserve_out) = self.compute_effective_reserves();
        IntHopState::new(reserve_in, reserve_out, self.gamma_numer, self.fee_denom)
    }

    /// Compute V3 effective reserves as (R_in, R_out) U256 pair.
    ///
    /// Uses U512 intermediates to avoid overflow:
    /// - token0_virt = L · 2^96 / sqrtPriceX96 (U512 ÷ U256)
    /// - token1_virt = L · sqrtPriceX96 / 2^96  (U512 ÷ U256)
    #[must_use]
    pub fn compute_effective_reserves(&self) -> (U256, U256) {
        let (token0_virt, token1_virt) = self.compute_virtual_reserves();

        if self.zero_for_one {
            // Swap token0 → token1: reserve_in = token0_virt, reserve_out = token1_virt
            (token0_virt, token1_virt)
        } else {
            // Swap token1 → token0: reserve_in = token1_virt, reserve_out = token0_virt
            (token1_virt, token0_virt)
        }
    }

    /// Compute both virtual reserves: (token0_virt, token1_virt).
    ///
    /// ```text
    /// token0_virt = (L · 2^96) / sqrtPriceX96
    /// token1_virt = (L · sqrtPriceX96) / 2^96
    /// ```
    ///
    /// L is u128, sqrtPriceX96 is U256. The numerator L · sqrtPriceX96
    /// fits in U256 (128 + 160 bits = 288 bits — fits in U512). The
    /// numerator L · 2^96 fits in u128 + 96 = 224 bits — fits in U256.
    fn compute_virtual_reserves(&self) -> (U256, U256) {
        let l = U256::from(self.liquidity);
        let sp = self.sqrt_price_x96;
        let q96 = U256::from(1u128) << 96;

        // token0_virt = (L · 2^96) / sqrtPriceX96
        // L·2^96 fits in U256 (max u128 · 2^96 = 224 bits)
        let numerator_0 = l * q96;
        let token0_virt = if sp.is_zero() {
            U256::MAX
        } else {
            // U512 division for exact truncation
            let numerator_0_u512 = U512::from(numerator_0);
            let sp_u512 = U512::from(sp);
            u512_to_u256(numerator_0_u512 / sp_u512)
        };

        // token1_virt = (L · sqrtPriceX96) / 2^96
        // L·sqrtPriceX96: max u128 · U256 = 288 bits, fits in U512
        let numerator_1 = U512::from(l) * U512::from(sp);
        let token1_virt = u512_to_u256(numerator_1 / U512::from(q96));

        (token0_virt, token1_virt)
    }

    /// Maximum gross input (including fees) that this range can absorb
    /// without pushing the price past the range boundary.
    ///
    /// For zero_for_one: L · (1/√P_lower - 1/√P_current) / γ
    /// For one_for_zero: L · (√P_upper - √P_current) / γ
    ///
    /// where γ = gamma_numer / fee_denom, so gross = net · fee_denom / gamma_numer.
    ///
    /// All in integer math with Q128.96 sqrt prices.
    #[must_use]
    pub fn max_gross_input_in_range(&self) -> U256 {
        if self.liquidity == 0 || self.gamma_numer == 0 {
            return U256::ZERO;
        }

        let l = U256::from(self.liquidity);
        let gamma_numer = U256::from(self.gamma_numer);
        let fee_denom = U256::from(self.fee_denom);

        if self.zero_for_one {
            // max_net_input = L · 2^96 · (sp_cur - sp_low) / (sp_low · sp_cur)
            // max_gross = max_net · fee_denom / gamma_numer
            let sp_diff = self.sqrt_price_x96.saturating_sub(self.sqrt_price_lower_x96);
            if sp_diff.is_zero() {
                return U256::ZERO;
            }
            // numerator = L · 2^96 · sp_diff · fee_denom
            // denominator = gamma_numer · sp_low · sp_cur
            let l_u512 = U512::from(l);
            let q96_u512 = U512::from(U256::from(1u128) << 96);
            let sp_diff_u512 = U512::from(sp_diff);
            let fee_denom_u512 = U512::from(fee_denom);
            let denom_u512 = U512::from(gamma_numer)
                * U512::from(self.sqrt_price_lower_x96)
                * U512::from(self.sqrt_price_x96);

            if denom_u512.is_zero() {
                return U256::ZERO;
            }

            let numerator = l_u512 * q96_u512 * sp_diff_u512 * fee_denom_u512;
            u512_to_u256(numerator / denom_u512)
        } else {
            // max_net_input = L · (sp_upper - sp_current) / 2^96
            // max_gross = max_net · fee_denom / gamma_numer
            let sp_diff = self.sqrt_price_upper_x96.saturating_sub(self.sqrt_price_x96);
            if sp_diff.is_zero() {
                return U256::ZERO;
            }

            // numerator = L · sp_diff · fee_denom
            // denominator = gamma_numer · 2^96
            let numerator_u512 = U512::from(l) * U512::from(sp_diff) * U512::from(fee_denom);
            let denom_u512 = U512::from(gamma_numer) * U512::from(U256::from(1u128) << 96);

            u512_to_u256(numerator_u512 / denom_u512)
        }
    }
}

// ---------------------------------------------------------------------------
// Integer V3 Tick Range Sequence
// ---------------------------------------------------------------------------

/// Ordered sequence of integer V3 tick ranges in the swap direction.
///
/// `ranges[0]` contains the current price. `ranges[1]`, `ranges[2]`, ...
/// are adjacent ranges in the swap direction.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntV3TickRangeSequence {
    /// Ordered tick ranges in the swap direction.
    pub ranges: Vec<IntV3TickRangeHop>,
}

impl IntV3TickRangeSequence {
    /// Create a new integer tick range sequence.
    ///
    /// # Errors
    ///
    /// Returns a string error if ranges is empty or if fees/directions are mixed.
    pub fn new(ranges: Vec<IntV3TickRangeHop>) -> Result<Self, String> {
        if ranges.is_empty() {
            return Err("Empty tick range sequence".to_string());
        }

        // Validate consistency: all ranges must have same fee and direction
        let gamma_numer = ranges[0].gamma_numer;
        let fee_denom = ranges[0].fee_denom;
        let zfo = ranges[0].zero_for_one;
        for r in &ranges {
            if r.gamma_numer != gamma_numer || r.fee_denom != fee_denom {
                return Err("All ranges must have the same fee".to_string());
            }
            if r.zero_for_one != zfo {
                return Err("All ranges must have the same swap direction".to_string());
            }
        }

        Ok(Self { ranges })
    }

    /// Combined effective reserves of all ranges, as a single `IntHopState`.
    ///
    /// For the Möbius solver, a V3 tick range sequence can be approximated
    /// as a single constant-product pool with combined effective reserves.
    /// This approximation is exact for single-range paths and a close
    /// approximation for multi-range paths where the shift from crossing
    /// ranges is small relative to total liquidity.
    ///
    /// The combined reserves are:
    /// - token0_virt_total = Σ token0_virt_range_i
    /// - token1_virt_total = Σ token1_virt_range_i
    ///
    /// Then map to (reserve_in, reserve_out) based on swap direction.
    #[must_use]
    pub fn to_int_hop_state(&self) -> IntHopState {
        let mut total_token0 = U256::ZERO;
        let mut total_token1 = U256::ZERO;

        for range in &self.ranges {
            let (t0, t1) = range.compute_virtual_reserves();
            total_token0 = total_token0.saturating_add(t0);
            total_token1 = total_token1.saturating_add(t1);
        }

        let (reserve_in, reserve_out) = if self.ranges[0].zero_for_one {
            (total_token0, total_token1)
        } else {
            (total_token1, total_token0)
        };

        IntHopState::new(
            reserve_in,
            reserve_out,
            self.ranges[0].gamma_numer,
            self.ranges[0].fee_denom,
        )
    }
}

// ---------------------------------------------------------------------------
// Integer Tick Range Crossing
// ---------------------------------------------------------------------------

/// Pre-computed crossing data for reaching a target tick range.
///
/// The crossing amounts are **independent of total input** — they are fixed
/// by the range boundaries and liquidity. This is the additive structure of
/// V3 tick crossings: `total_output(x) = crossing_output + mobius(remaining, ending_range)`.
#[derive(Clone, Debug)]
pub struct IntTickRangeCrossing {
    /// Total gross input (including fees) consumed by crossed ranges.
    pub crossing_gross_input: U256,
    /// Total output from crossed ranges.
    pub crossing_output: U256,
    /// The ending range with `sqrt_price_x96` set to the entry boundary.
    pub ending_range: IntV3TickRangeHop,
}

impl IntV3TickRangeSequence {
    /// Compute crossing data to reach range `k` (0-indexed).
    ///
    /// - `k=0`: no crossing (swap stays in first range).
    /// - `k=1`: cross range 0, end in range 1.
    /// - `k=2`: cross ranges 0–1, end in range 2.
    ///
    /// The ending range's `sqrt_price_x96` is set to the entry boundary price.
    ///
    /// Returns `None` if `k` is out of bounds.
    ///
    /// # Integer math
    ///
    /// For zero_for_one (price decreasing):
    /// - net_in = L · (1/√P_end - 1/√P_start) = L · 2^96 · (sp_start - sp_end) / (sp_end · sp_start)
    /// - output  = L · (√P_start - √P_end) = L · (sp_start - sp_end) / 2^96
    /// - gross_in = net_in / γ = net_in · fee_denom / gamma_numer
    ///
    /// For one_for_zero (price increasing):
    /// - net_in = L · (√P_end - √P_start) = L · (sp_end - sp_start) / 2^96
    /// - output  = L · (1/√P_start - 1/√P_end) = L · 2^96 · (sp_end - sp_start) / (sp_start · sp_end)
    /// - gross_in = net_in / γ = net_in · fee_denom / gamma_numer
    #[must_use]
    pub fn compute_crossing(&self, k: usize) -> Option<IntTickRangeCrossing> {
        if k >= self.ranges.len() {
            return None;
        }

        if k == 0 {
            return Some(IntTickRangeCrossing {
                crossing_gross_input: U256::ZERO,
                crossing_output: U256::ZERO,
                ending_range: self.ranges[0].clone(),
            });
        }

        let gamma_numer = U256::from(self.ranges[0].gamma_numer);
        let fee_denom = U256::from(self.ranges[0].fee_denom);
        let q96 = U256::from(1u128) << 96;
        let zfo = self.ranges[0].zero_for_one;

        let mut crossing_gross_input = U256::ZERO;
        let mut crossing_output = U256::ZERO;

        for i in 0..k {
            let r = &self.ranges[i];
            let l = U256::from(r.liquidity);

            // Determine start and end sqrt prices for this range
            let sp_start = if i == 0 {
                r.sqrt_price_x96
            } else if zfo {
                // Previous range's lower boundary is this range's entry point
                self.ranges[i - 1].sqrt_price_lower_x96
            } else {
                self.ranges[i - 1].sqrt_price_upper_x96
            };

            let (net_input, output) = if zfo {
                let sp_end = r.sqrt_price_lower_x96; // zfo: price decreases to lower bound
                // net_in = L · 2^96 · (sp_start - sp_end) / (sp_end · sp_start)
                // output  = L · (sp_start - sp_end) / 2^96
                let sp_diff = sp_start.saturating_sub(sp_end);
                if sp_diff.is_zero() {
                    (U256::ZERO, U256::ZERO)
                } else {
                    let net_in_u512 = U512::from(l) * U512::from(q96) * U512::from(sp_diff);
                    let denom_u512 = U512::from(sp_end) * U512::from(sp_start);
                    let net_in = if denom_u512.is_zero() {
                        U256::ZERO
                    } else {
                        u512_to_u256(net_in_u512 / denom_u512)
                    };
                    let out_u512 = U512::from(l) * U512::from(sp_diff);
                    let out = u512_to_u256(out_u512 / U512::from(q96));
                    (net_in, out)
                }
            } else {
                let sp_end = r.sqrt_price_upper_x96; // ofz: price increases to upper bound
                // net_in = L · (sp_end - sp_start) / 2^96
                // output  = L · 2^96 · (sp_end - sp_start) / (sp_start · sp_end)
                let sp_diff = sp_end.saturating_sub(sp_start);
                if sp_diff.is_zero() {
                    (U256::ZERO, U256::ZERO)
                } else {
                    let net_in_u512 = U512::from(l) * U512::from(sp_diff);
                    let net_in = u512_to_u256(net_in_u512 / U512::from(q96));
                    let out_u512 = U512::from(l) * U512::from(q96) * U512::from(sp_diff);
                    let denom_u512 = U512::from(sp_start) * U512::from(sp_end);
                    let out = if denom_u512.is_zero() {
                        U256::ZERO
                    } else {
                        u512_to_u256(out_u512 / denom_u512)
                    };
                    (net_in, out)
                }
            };

            // gross_input = net_input / γ = net_input · fee_denom / gamma_numer
            let gross_input_u512 = U512::from(net_input) * U512::from(fee_denom);
            let gross_input = if gamma_numer.is_zero() {
                U256::MAX
            } else {
                u512_to_u256(gross_input_u512 / U512::from(gamma_numer))
            };

            crossing_gross_input = crossing_gross_input.saturating_add(gross_input);
            crossing_output = crossing_output.saturating_add(output);
        }

        // Construct ending range with entry price at boundary
        let ending = &self.ranges[k];
        let entry_sqrt_price = if zfo {
            self.ranges[k - 1].sqrt_price_lower_x96
        } else {
            self.ranges[k - 1].sqrt_price_upper_x96
        };

        let ending_range = IntV3TickRangeHop {
            liquidity: ending.liquidity,
            sqrt_price_x96: entry_sqrt_price,
            sqrt_price_lower_x96: ending.sqrt_price_lower_x96,
            sqrt_price_upper_x96: ending.sqrt_price_upper_x96,
            gamma_numer: ending.gamma_numer,
            fee_denom: ending.fee_denom,
            zero_for_one: ending.zero_for_one,
        };

        Some(IntTickRangeCrossing {
            crossing_gross_input,
            crossing_output,
            ending_range,
        })
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
/// Returns the total output amount (U256).
#[must_use]
fn int_simulate_v3_v3_path(
    amount_in: U256,
    crossing1: Option<&IntTickRangeCrossing>,
    crossing2: Option<&IntTickRangeCrossing>,
    base_range1: &IntV3TickRangeHop,
    base_range2: &IntV3TickRangeHop,
) -> U256 {
    if amount_in.is_zero() {
        return U256::ZERO;
    }

    // Hop 1 output
    let output1 = if let Some(c1) = crossing1 {
        if amount_in < c1.crossing_gross_input {
            return U256::ZERO; // Can't cover crossing cost
        }
        let remaining = amount_in - c1.crossing_gross_input;
        let ending_out = int_simulate_v3_swap(remaining, &c1.ending_range);
        c1.crossing_output.saturating_add(ending_out)
    } else {
        int_simulate_v3_swap(amount_in, base_range1)
    };

    // Hop 2 output
    if let Some(c2) = crossing2 {
        if output1 < c2.crossing_gross_input {
            return U256::ZERO; // Can't cover crossing cost
        }
        let remaining = output1 - c2.crossing_gross_input;
        let ending_out = int_simulate_v3_swap(remaining, &c2.ending_range);
        c2.crossing_output.saturating_add(ending_out)
    } else {
        int_simulate_v3_swap(output1, base_range2)
    }
}

// Clippy: allow manual_ok_err in int_solve_v3_v3 match arms
// (the None/Err branches have side effects so let-else doesn't apply)

/// Solve a 2-hop V3-V3 arbitrage path using integer-exact Möbius solver.
///
/// For each candidate pair of ending tick ranges (k1, k2):
/// 1. Build `IntHopState` from each ending range's effective reserves
/// 2. Compute closed-form optimal input via `exact_mobius_solve`
/// 3. Validate with full piecewise simulation (`int_simulate_v3_v3_path`)
/// 4. Search ±2 neighbors for integer rounding
///
/// Returns `(optimal_input, profit)` or `None` if not profitable.
///
/// This replaces the f64 `solve_v3_v3` which uses golden-section search.
/// The closed-form approach is both faster and EVM-exact.
#[must_use]
pub fn int_solve_v3_v3(
    seq1: &IntV3TickRangeSequence,
    seq2: &IntV3TickRangeSequence,
) -> Option<(U256, U256)> {
    let max_candidates = 10;

    // Fast path: both single-range → standard 2-hop Möbius
    if seq1.ranges.len() == 1 && seq2.ranges.len() == 1 {
        let hop1 = seq1.ranges[0].to_int_hop_state();
        let hop2 = seq2.ranges[0].to_int_hop_state();
        let result = crate::optimizers::mobius_int_exact::exact_mobius_solve(&[hop1, hop2]).ok()?;

        if !result.is_profitable || result.optimal_input.is_zero() || result.profit.is_zero() {
            return None;
        }

        // Validate with integer simulation
        let output = int_simulate_v3_v3_path(
            result.optimal_input,
            None,
            None,
            &seq1.ranges[0],
            &seq2.ranges[0],
        );
        if output > result.optimal_input {
            let profit = output - result.optimal_input;
            return Some((result.optimal_input, profit));
        }
        return None;
    }

    // General case: enumerate (k1, k2) ending-range combinations
    let n1 = max_candidates.min(seq1.ranges.len());
    let n2 = max_candidates.min(seq2.ranges.len());

    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;

    for k1 in 0..n1 {
        for k2 in 0..n2 {
            let Some(crossing1) = seq1.compute_crossing(k1) else {
                continue;
            };
            let Some(crossing2) = seq2.compute_crossing(k2) else {
                continue;
            };

            // Build IntHopState from ending ranges
            let hop1 = crossing1.ending_range.to_int_hop_state();
            let hop2 = crossing2.ending_range.to_int_hop_state();

            // Closed-form optimal input for this piece
            let Ok(result) = crate::optimizers::mobius_int_exact::exact_mobius_solve(&[hop1, hop2]) else {
                continue;
            };

            if !result.is_profitable || result.optimal_input.is_zero() {
                continue;
            }

            // The optimal input must be at least crossing_gross_input for hop 1
            // to reach the ending range k1
            let total_optimal_input = result.optimal_input.saturating_add(crossing1.crossing_gross_input);

            // Also check: the output of hop 1 at optimal must cover crossing_gross_input for hop 2
            // This is validated by the full piecewise simulation below

            // Search ±2 around total_optimal_input
            for delta in -2i32..=2 {
                let candidate = if delta >= 0 {
                    total_optimal_input.saturating_add(U256::from(delta as u64))
                } else {
                    total_optimal_input.saturating_sub(U256::from((-delta) as u64))
                };

                if candidate.is_zero() {
                    continue;
                }

                let output = int_simulate_v3_v3_path(
                    candidate,
                    if k1 == 0 { None } else { Some(&crossing1) },
                    if k2 == 0 { None } else { Some(&crossing2) },
                    &seq1.ranges[0],
                    &seq2.ranges[0],
                );

                if output > candidate {
                    let profit = output - candidate;
                    if profit > best_profit {
                        best_profit = profit;
                        best_x = candidate;
                    }
                }
            }
        }
    }

    if best_profit.is_zero() {
        None
    } else {
        Some((best_x, best_profit))
    }
}

/// Compute Möbius coefficients for a mixed V2-V3 path.
///
/// Takes a slice of hops where each hop is either a V2 `IntHopState` or
/// a V3 `IntV3TickRangeHop`, and produces a single (K, M, N) triple that
/// represents the composed Möbius transformation.
///
/// For V2 hops, this is the standard `compute_int_mobius_coefficients`.
/// For V3 hops, we compute effective reserves and use those.
///
/// Returns the composed coefficients, or `None` if the path is not profitable.
#[must_use]
pub fn compute_mixed_int_mobius_coefficients(
    hops: &[MixedIntHop],
) -> Option<IntMobiusCoefficients> {
    if hops.is_empty() {
        return None;
    }

    // Build flat IntHopState list: each V3 sequence becomes one hop
    let flat_hops: Vec<IntHopState> = hops
        .iter()
        .map(|hop| match hop {
            MixedIntHop::V2(state) => state.clone(),
            MixedIntHop::V3(int_v3) => int_v3.to_int_hop_state(),
        })
        .collect();

    crate::optimizers::mobius_int::compute_int_mobius_coefficients(&flat_hops).ok()
}

/// A hop in a mixed V2-V3 path — either a V2 constant-product hop
/// or a V3 concentrated-liquidity tick range.
#[derive(Clone, Debug)]
pub enum MixedIntHop {
    /// V2 constant-product hop.
    V2(IntHopState),
    /// V3 concentrated-liquidity tick range.
    V3(IntV3TickRangeHop),
}

// ---------------------------------------------------------------------------
// Integer V3 Swap Simulation
// ---------------------------------------------------------------------------

/// Simulate a V3 swap within a single tick range using integer arithmetic.
///
/// For `zero_for_one`:
///   output = γ · L · (√P_current - √P_final) / 2^96
///   where √P_final = L · √P_current / (L + γ·x·√P_current/2^96)
///
/// This matches Solidity's `SwapMath.computeSwapStep()` exactly.
///
/// Returns the output amount (U256). Returns 0 if the swap pushes the
/// price out of range or if inputs are invalid.
#[must_use]
pub fn int_simulate_v3_swap(amount_in: U256, v3_hop: &IntV3TickRangeHop) -> U256 {
    if amount_in.is_zero() || v3_hop.liquidity == 0 {
        return U256::ZERO;
    }

    let gamma = U256::from(v3_hop.gamma_numer);
    let fee_denom = U256::from(v3_hop.fee_denom);

    // Net input after fees
    // net_in = amount_in * gamma / fee_denom
    let net_in_u512 = U512::from(amount_in) * U512::from(gamma);
    let net_in = u512_to_u256(net_in_u512 / U512::from(fee_denom));

    if net_in.is_zero() {
        return U256::ZERO;
    }

    let l = U256::from(v3_hop.liquidity);
    let sp_current = v3_hop.sqrt_price_x96;
    let q96 = U256::from(1u128) << 96;

    if v3_hop.zero_for_one {
        // zfo: price decreases from √P_current toward √P_lower
        // Solidity: sqrtPriceNext = (liquidity * sqrtPriceCurrent * 2^96) /
        //          (liquidity * 2^96 + amountRemaining * sqrtPriceCurrent)
        // where amountRemaining is net_in (already fee-deducted)

        // numerator = L · √P_current · 2^96 = L · sp_current · q96
        // denominator = L · 2^96 + net_in · sp_current = L · q96 + net_in · sp_current
        //
        // Both numerator and denominator need U512 (L=128 bits, sp=160 bits, q96=97 bits)
        // numerator: 128+160+97 = 385 bits → fits in U512
        // denominator: max(128+97, 256+160) = 416 bits → fits in U512

        let numerator = U512::from(l) * U512::from(sp_current) * U512::from(q96);
        let denominator = U512::from(l) * U512::from(q96) + U512::from(net_in) * U512::from(sp_current);

        if denominator.is_zero() {
            return U256::ZERO;
        }

        let sp_next_u512 = numerator / denominator;
        let sp_next = u512_to_u256(sp_next_u512);

        // Clamp to range: sp_next must be >= sqrt_price_lower
        let sp_final = sp_next.max(v3_hop.sqrt_price_lower_x96);

        // Output = L · (√P_current - √P_final) / 2^96
        // In Q96: L · (sp_current - sp_final) / q96
        let sp_diff = sp_current.saturating_sub(sp_final);
        if sp_diff.is_zero() {
            return U256::ZERO;
        }

        let output_u512 = U512::from(l) * U512::from(sp_diff);
        u512_to_u256(output_u512 / U512::from(q96))
    } else {
        // ofz: price increases from √P_current toward √P_upper
        // Solidity: sqrtPriceNext = sqrtPriceCurrent + amountRemaining * 2^96 / liquidity

        // sp_next = sp_current + net_in · 2^96 / L
        let increment_u512 = U512::from(net_in) * U512::from(q96);
        let increment = u512_to_u256(increment_u512 / U512::from(l));

        let sp_next = sp_current.saturating_add(increment);

        // Clamp to range: sp_next must be <= sqrt_price_upper
        let sp_final = sp_next.min(v3_hop.sqrt_price_upper_x96);

        // Output = L · (1/√P_current - 1/√P_final) = L · (√P_final - √P_current) / (√P_current · √P_final / 2^96)
        // In Q96: L · 2^96 · (sp_final - sp_current) / (sp_current · sp_final)
        let sp_diff = sp_final.saturating_sub(sp_current);
        if sp_diff.is_zero() {
            return U256::ZERO;
        }

        let numerator_u512 = U512::from(l) * U512::from(q96) * U512::from(sp_diff);
        let denominator_u512 = U512::from(sp_current) * U512::from(sp_final);

        if denominator_u512.is_zero() {
            return U256::ZERO;
        }

        u512_to_u256(numerator_u512 / denominator_u512)
    }
}

// ---------------------------------------------------------------------------
// Integer V3 Exact Solver
// ---------------------------------------------------------------------------

/// Solve mixed V2-V3 arbitrage path using exact integer Möbius solver.
///
/// Computes Möbius coefficients from V3 effective reserves + V2 reserves,
/// then uses the closed-form `exact_mobius_solve` to find the optimal input.
///
/// Simulate a mixed V2-V3 path where the V3 hop may cross tick ranges.
///
/// If `v3_crossing` is `Some`, the V3 hop is split into:
/// 1. Crossing `k` ranges (consuming `crossing_gross_input`, producing `crossing_output`)
/// 2. Remaining input goes into the ending range
///
/// The `base_v3_hop` is used only when `v3_crossing` is `None` (k=0 case).
///
/// Returns the final output amount (U256).
#[must_use]
fn int_simulate_mixed_path_with_crossing(
    amount_in: U256,
    v2_hops: &[IntHopState],
    base_v3_hop: &IntV3TickRangeHop,
    v3_crossing: Option<&IntTickRangeCrossing>,
    v3_first: bool,
) -> U256 {
    if amount_in.is_zero() {
        return U256::ZERO;
    }

    if v3_first {
        // V3 → V2: input goes through V3 first, then V2
        let v3_output = if let Some(crossing) = v3_crossing {
            if amount_in < crossing.crossing_gross_input {
                return U256::ZERO; // Can't cover V3 crossing cost
            }
            let remaining = amount_in - crossing.crossing_gross_input;
            let ending_out = int_simulate_v3_swap(remaining, &crossing.ending_range);
            crossing.crossing_output.saturating_add(ending_out)
        } else {
            int_simulate_v3_swap(amount_in, base_v3_hop)
        };

        // V3 output goes through V2 hops
        int_simulate_v2_hops(v3_output, v2_hops)
    } else {
        // V2 → V3: input goes through V2 first, then V3
        let v2_output = int_simulate_v2_hops(amount_in, v2_hops);

        // V2 output goes through V3 (with optional crossing)
        if let Some(crossing) = v3_crossing {
            if v2_output < crossing.crossing_gross_input {
                return U256::ZERO; // V2 output can't cover V3 crossing cost
            }
            let remaining = v2_output - crossing.crossing_gross_input;
            let ending_out = int_simulate_v3_swap(remaining, &crossing.ending_range);
            crossing.crossing_output.saturating_add(ending_out)
        } else {
            int_simulate_v3_swap(v2_output, base_v3_hop)
        }
    }
}

/// Simulate a chain of V2 hops sequentially.
#[must_use]
fn int_simulate_v2_hops(amount_in: U256, v2_hops: &[IntHopState]) -> U256 {
    let mut current = amount_in;
    for hop in v2_hops {
        if current.is_zero() {
            return U256::ZERO;
        }
        current = crate::optimizers::mobius_int::int_simulate_path(
            current,
            std::slice::from_ref(hop),
        );
    }
    current
}

/// Solve mixed V2-V3 arbitrage path using integer-exact Möbius solver
/// with full tick-range sequence for the V3 side.
///
/// This is the production solver for mixed paths. It enumerates V3 ending
/// ranges (like `int_solve_v3_v3`), computing the optimal input for each
/// piece and validating with full crossing-aware simulation.
///
/// # Algorithm
///
/// For each candidate ending range `k` in the V3 sequence:
/// 1. Compute crossing data (input/output to reach range k)
/// 2. Build `MixedIntHop` list with V3 ending range + V2 hops
/// 3. Compute Möbius coefficients → closed-form optimal input for this piece
/// 4. Total optimal input = crossing_gross_input + piecewise optimal input
/// 5. Validate with crossing-aware simulation
/// 6. Search ±2 neighbors for integer rounding
///
/// Returns the optimal input and profit, or `None` if not profitable.
#[must_use]
pub fn exact_solve_mixed_v2_v3_sequence(
    v2_hops: &[IntHopState],
    v3_sequence: &IntV3TickRangeSequence,
    v3_first: bool,
) -> Option<(U256, U256)> {
    let max_candidates = 10;
    let n_v3 = max_candidates.min(v3_sequence.ranges.len());

    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;

    for k in 0..n_v3 {
        let Some(crossing) = v3_sequence.compute_crossing(k) else {
            continue;
        };

        // Build MixedIntHop list with ending V3 range + V2 hops
        let mixed_hops: Vec<MixedIntHop> = if v3_first {
            let mut h = vec![MixedIntHop::V3(crossing.ending_range.clone())];
            h.extend(v2_hops.iter().map(|h| MixedIntHop::V2(h.clone())));
            h
        } else {
            let mut h: Vec<MixedIntHop> = v2_hops.iter().map(|h| MixedIntHop::V2(h.clone())).collect();
            h.push(MixedIntHop::V3(crossing.ending_range.clone()));
            h
        };

        // Compute Möbius coefficients for this piece
        let Some(coeffs) = compute_mixed_int_mobius_coefficients(&mixed_hops) else {
            continue;
        };

        if !coeffs.is_profitable {
            continue;
        }

        // Closed-form optimal input for the remaining (post-crossing) swap
        let x_piece = crate::optimizers::mobius_int_exact::compute_exact_optimal_input_from_coeffs(&coeffs);

        if x_piece.is_zero() {
            continue;
        }

        // Total optimal input = crossing cost + piecewise optimal input
        let total_optimal_input = x_piece.saturating_add(crossing.crossing_gross_input);

        // Search ±2 around total_optimal_input
        for delta in -2i32..=2 {
            let candidate = if delta >= 0 {
                total_optimal_input.saturating_add(U256::from(delta as u64))
            } else {
                total_optimal_input.saturating_sub(U256::from((-delta) as u64))
            };

            if candidate.is_zero() {
                continue;
            }

            let output = int_simulate_mixed_path_with_crossing(
                candidate,
                v2_hops,
                &v3_sequence.ranges[0],
                if k == 0 { None } else { Some(&crossing) },
                v3_first,
            );

            if output > candidate {
                let profit = output - candidate;
                if profit > best_profit {
                    best_profit = profit;
                    best_x = candidate;
                }
            }
        }
    }

    if best_profit.is_zero() {
        None
    } else {
        Some((best_x, best_profit))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert U512 to U256, capping at U256::MAX on overflow.
fn u512_to_u256(v: U512) -> U256 {
    crate::optimizers::mobius_int_exact::u512_to_u256_internal(v)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Helper: create an IntV3TickRangeHop at tick 0 (1:1 price), tick spacing 60.
    fn make_v3_hop_at_1to1(liquidity: u128, zfo: bool) -> IntV3TickRangeHop {
        // At tick 0, sqrtPriceX96 = 2^96
        let sp_0 = U256::from(1u128) << 96;
        // Tick -60 → lower bound
        let sp_lower =
            crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default();
        // Tick +60 → upper bound
        let sp_upper =
            crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default();

        IntV3TickRangeHop {
            liquidity,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: U256::from(sp_lower),
            sqrt_price_upper_x96: U256::from(sp_upper),
            gamma_numer: 997_000,  // 0.3% fee → gamma = 997_000 / 1_000_000
            fee_denom: 1_000_000,
            zero_for_one: zfo,
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
        assert_eq!(int_hop.gamma_numer, 997_000);
        assert_eq!(int_hop.fee_denom, 1_000_000);
    }

    #[test]
    fn test_int_simulate_v3_swap_small_input_zfo() {
        let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, true);

        // Small swap: 1000 token0 in, zfo
        let input = U256::from(1000u64);
        let output = int_simulate_v3_swap(input, &hop);

        // Should produce positive output less than input (due to fees on 1:1 pool)
        assert!(output > U256::ZERO, "Output should be positive for a valid swap");
        assert!(output < input, "Output should be less than input on 1:1 pool with fees: output={output}, input={input}");
    }

    #[test]
    fn test_int_simulate_v3_swap_small_input_ofz() {
        let hop = make_v3_hop_at_1to1(10_000_000_000_000u128, false);

        // Small swap: 1000 token1 in, ofz
        let input = U256::from(1000u64);
        let output = int_simulate_v3_swap(input, &hop);

        assert!(output > U256::ZERO);
        assert!(output < input, "Output should be less than input on 1:1 pool with fees");
    }

    #[test]
    fn test_int_simulate_v3_swap_zero_input() {
        let hop = make_v3_hop_at_1to1(1_000_000u128, true);
        let output = int_simulate_v3_swap(U256::ZERO, &hop);
        assert!(output.is_zero());
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
        };
        let output = int_simulate_v3_swap(U256::from(1000u64), &hop);
        assert!(output.is_zero());
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
            U256::from(1_500_000_000_000u64),  // 1.5M USDC
            U256::from(800_000_000_000_000_000_000u128), // 800 WETH (18 dec)
            997,
            1000,
        );

        // V3 with high liquidity and price above V2
        let v3_hop = IntV3TickRangeHop {
            liquidity: 10_000_000_000_000u128,
            sqrt_price_x96: U256::from(1u128) << 96, // 1:1 price
            sqrt_price_lower_x96: U256::from(
                crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default(),
            ),
            sqrt_price_upper_x96: U256::from(
                crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default(),
            ),
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: false,
        };

        let v3_seq = IntV3TickRangeSequence::new(vec![v3_hop]).unwrap();

        let result = exact_solve_mixed_v2_v3_sequence(&[v2_hop], &v3_seq, true);
        // Key thing is no panics.
        let _ = result;
    }

    #[test]
    fn test_u512_to_u256_small() {
        let v = U512::from(42u64);
        assert_eq!(u512_to_u256(v), U256::from(42u64));
    }

    #[test]
    fn test_u512_to_u256_overflow() {
        let v = U512::from(U256::MAX) + U512::from(1u64);
        assert_eq!(u512_to_u256(v), U256::MAX); // Capped
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
        let sp_lower0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default());
        let sp_upper0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default());
        let sp_lower1 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default());
        let sp_upper1 = sp_lower0; // Range 1 starts where range 0 ends

        let hop0 = IntV3TickRangeHop {
            liquidity: 10_000_000_000_000u128,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: sp_lower0,
            sqrt_price_upper_x96: sp_upper0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
        };
        let hop1 = IntV3TickRangeHop {
            liquidity: 5_000_000_000_000u128,
            sqrt_price_x96: sp_lower0, // entry at boundary — will be overridden
            sqrt_price_lower_x96: sp_lower1,
            sqrt_price_upper_x96: sp_upper1,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
        };

        let seq = IntV3TickRangeSequence::new(vec![hop0, hop1]).unwrap();
        let crossing = seq.compute_crossing(1).unwrap();

        // Crossing k=1 must consume range 0's capacity
        assert!(!crossing.crossing_gross_input.is_zero(), "k=1 crossing should require input");
        assert!(!crossing.crossing_output.is_zero(), "k=1 crossing should produce output");

        // The ending range should have entry price = range 0's lower bound
        assert_eq!(crossing.ending_range.sqrt_price_x96, sp_lower0);
        assert_eq!(crossing.ending_range.liquidity, 5_000_000_000_000u128);
    }

    #[test]
    fn test_compute_crossing_k1_ofz() {
        let sp_0 = U256::from(1u128) << 96;
        let sp_lower0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default());
        let sp_upper0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default());
        let sp_upper1 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(120).unwrap_or_default());
        let sp_lower1 = sp_upper0; // Range 1 starts where range 0 ends

        let hop0 = IntV3TickRangeHop {
            liquidity: 10_000_000_000_000u128,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: sp_lower0,
            sqrt_price_upper_x96: sp_upper0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: false,
        };
        let hop1 = IntV3TickRangeHop {
            liquidity: 5_000_000_000_000u128,
            sqrt_price_x96: sp_upper0,
            sqrt_price_lower_x96: sp_lower1,
            sqrt_price_upper_x96: sp_upper1,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: false,
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
        let sp_lower0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default());
        let sp_upper0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default());
        let sp_lower1 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default());

        let hop0 = IntV3TickRangeHop {
            liquidity: 10_000_000_000_000u128,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: sp_lower0,
            sqrt_price_upper_x96: sp_upper0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
        };
        let hop1 = IntV3TickRangeHop {
            liquidity: 5_000_000_000_000u128,
            sqrt_price_x96: sp_lower0,
            sqrt_price_lower_x96: sp_lower1,
            sqrt_price_upper_x96: sp_lower0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
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
        let result = int_solve_v3_v3(&seq1, &seq2);
        assert!(result.is_none(), "Same-price pools should not be profitable");
    }

    #[test]
    fn test_int_solve_v3_v3_single_range_no_panic() {
        // Different effective reserves — may or may not be profitable,
        // but must not panic
        let sp_0 = U256::from(1u128) << 96;
        let sp_lower =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default());
        let sp_upper =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default());

        // Asymmetric: high-liquidity pool vs low-liquidity pool
        let hop1 = IntV3TickRangeHop {
            liquidity: 100_000_000_000_000u128,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: sp_lower,
            sqrt_price_upper_x96: sp_upper,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
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
        };

        let seq1 = IntV3TickRangeSequence::new(vec![hop1]).unwrap();
        let seq2 = IntV3TickRangeSequence::new(vec![hop2]).unwrap();
        let _ = int_solve_v3_v3(&seq1, &seq2);
    }

    #[test]
    fn test_int_solve_v3_v3_multi_range_no_panic() {
        let sp_0 = U256::from(1u128) << 96;
        let sp_lower0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-60).unwrap_or_default());
        let sp_upper0 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(60).unwrap_or_default());
        let sp_lower1 =
            U256::from(crate::tick_math::get_sqrt_ratio_at_tick_internal(-120).unwrap_or_default());

        let range1_0 = IntV3TickRangeHop {
            liquidity: 10_000_000_000_000u128,
            sqrt_price_x96: sp_0,
            sqrt_price_lower_x96: sp_lower0,
            sqrt_price_upper_x96: sp_upper0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
        };
        let range1_1 = IntV3TickRangeHop {
            liquidity: 5_000_000_000_000u128,
            sqrt_price_x96: sp_lower0, // entry at boundary
            sqrt_price_lower_x96: sp_lower1,
            sqrt_price_upper_x96: sp_lower0,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
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
        };

        let seq1 = IntV3TickRangeSequence::new(vec![range1_0, range1_1]).unwrap();
        let seq2 = IntV3TickRangeSequence::new(vec![range2_0]).unwrap();
        let _ = int_solve_v3_v3(&seq1, &seq2);
    }
}
