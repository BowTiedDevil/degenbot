//! V3 tick-range hop types for intermediate-value (Int) swap simulation —
//! the V3-family analogue of [`degenbot_v2_math::IntHopState`].
//!
//! **Relocated** from `degenbot-bot/src/solvers/mobius_v3_int.rs` (these are
//! pure value types, previously mis-homed under `solvers/`); re-exported there
//! at the historical `crate::solvers::mobius_v3_int::IntV3*` path so consumers
//! resolve unchanged. Transient re-export — repointed natively by USPN7M/P2CKRL.

use alloy::primitives::{U256, U512};
use degenbot_v2_math::IntHopState;

/// Ceiling division of `U512` / `U512` → `U256` (matches on-chain
/// `get_amount_delta(..., round_up=true)` and
/// `get_next_sqrt_price_from_amount0_rounding_up` for the exact-input
/// `amount_in` and `sqrt_price_next` derivations respectively).
///
/// Returns `num / denom` when `num % denom == 0`, else `num / denom + 1`.
#[must_use]
pub fn u512_div_ceil(num: U512, denom: U512) -> U256 {
    if denom.is_zero() {
        return U256::ZERO;
    }
    let q = num / denom;
    let r = num % denom;
    u512_to_u256(if r.is_zero() { q } else { q + U512::from(1u64) })
}

/// Narrow `U512` → `U256` with an overflow assert (mirrors the helper in
/// `degenbot-bot`/`u512_to_u256_internal`; consolidated into a shared
/// `v2-math` pub fn by the `degenbot-solvers` extraction epic).
#[must_use]
fn u512_to_u256(v: U512) -> U256 {
    assert!(
        v <= U512::from(U256::MAX),
        "U512 → U256 narrowing overflow (corrupt/synthetic input;          spec-bound pool state is unreachable — enforced at register_*_pool)",
    );
    v.to::<U256>()
}
/// precision for EVM-exact computation.
#[derive(Clone, Debug)]
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
    #[must_use]
    pub fn compute_virtual_reserves(&self) -> (U256, U256) {
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
    /// This is the single-range case (`i = 0`) of the crossing computation
    /// in [`IntV3TickRangeSequence::compute_crossing`], so it MUST use the
    /// same per-step rounding as on-chain `computeSwapStep` exact-in:
    /// `amount_in` rounded UP (`get_amount_delta(..., round_up=true)`) and
    /// `fee_amount` rounded UP (`muldiv_rounding_up`), so `gross = amount_in + fee_amount`
    /// is the CEILING. See `compute_crossing` for the full derivation.
    ///
    /// Rounding up keeps this consistent with `compute_crossing(k=1)` and
    /// on-chain behaviour; the prior FLOOR version under-estimated the
    /// boundary-reaching input and over-predicted V4 multi-range swap output.
    #[must_use]
    pub fn max_gross_input_in_range(&self) -> U256 {
        if self.liquidity == 0 || self.gamma_numer == 0 {
            return U256::ZERO;
        }

        let l = U256::from(self.liquidity);
        let gamma_numer = U256::from(self.gamma_numer);
        let fee_denom = U256::from(self.fee_denom);
        let q96 = U256::from(1u128) << 96;

        // `amount_in` (round-up), matching `get_amount_delta(..., round_up=true)`.
        let net_in = if self.zero_for_one {
            let sp_diff = self
                .sqrt_price_x96
                .saturating_sub(self.sqrt_price_lower_x96);
            if sp_diff.is_zero() {
                return U256::ZERO;
            }
            let net_in_u512 = U512::from(l) * U512::from(q96) * U512::from(sp_diff);
            let denom_u512 =
                U512::from(self.sqrt_price_lower_x96) * U512::from(self.sqrt_price_x96);
            if denom_u512.is_zero() {
                return U256::ZERO;
            }
            u512_div_ceil(net_in_u512, denom_u512)
        } else {
            let sp_diff = self
                .sqrt_price_upper_x96
                .saturating_sub(self.sqrt_price_x96);
            if sp_diff.is_zero() {
                return U256::ZERO;
            }
            u512_div_ceil(U512::from(l) * U512::from(sp_diff), U512::from(q96))
        };

        // `fee_amount` (round-up), matching `muldiv_rounding_up(amount_in · fee / γ)`.
        let fee = fee_denom - gamma_numer;
        if gamma_numer.is_zero() {
            return net_in;
        }
        let fee_amount = u512_div_ceil(
            U512::from(net_in) * U512::from(fee),
            U512::from(gamma_numer),
        );
        net_in.saturating_add(fee_amount)
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
    /// # Integer math (rounding matches on-chain `computeSwapStep` exact-in)
    ///
    /// On-chain, each tick-range step that REACHES its boundary computes
    /// `amount_in` with ROUND-UP (`get_amount_delta(..., round_up=true)`) and
    /// `fee_amount` with ROUND-UP (`muldiv_rounding_up`), so the consumed
    /// `amount_in + fee_amount` is the CEILING. The output (`amount_out`) uses
    /// ROUND-DOWN (`get_amount_delta(..., round_down=false)`).
    ///
    /// This function mirrors that per-step rounding exactly (otherwise the
    /// accumulated `crossing_gross_input` under-estimates the on-chain consumed
    /// input → the solver over-estimates the remaining input → over-predicts the
    /// output for multi-range swaps — the V4 `CurrencyNotSettled` divergence)
    ///
    /// For zero_for_one (price decreasing):
    /// - amount_in (ceil) = L · 2^96 · (sp_start - sp_end) / (sp_end · sp_start) [round-up]
    /// - output  (floor)  = L · (sp_start - sp_end) / 2^96                       [round-down]
    /// - fee_amount (ceil) = ceil(amount_in · fee / γ)  where γ = 1e6 - fee
    /// - gross_in = amount_in + fee_amount
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
                    // On-chain `amount_in` for an exact-in step reaching the
                    // boundary uses `get_amount0_delta(round_up=true)` (CEIL).
                    let net_in_u512 = U512::from(l) * U512::from(q96) * U512::from(sp_diff);
                    let denom_u512 = U512::from(sp_end) * U512::from(sp_start);
                    let net_in = if denom_u512.is_zero() {
                        U256::ZERO
                    } else {
                        u512_div_ceil(net_in_u512, denom_u512)
                    };
                    // `amount_out` uses round-down (floor) — matches on-chain.
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
                    // `amount_in` (ofz) = `get_amount1_delta(round_up=true)` (CEIL).
                    let net_in_u512 = U512::from(l) * U512::from(sp_diff);
                    let net_in = u512_div_ceil(net_in_u512, U512::from(q96));
                    // `amount_out` uses round-down (floor) — matches on-chain.
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

            // gross_input = amount_in + fee_amount, where on-chain computes
            // `fee_amount = ceil(amount_in · fee / γ)` (muldiv_rounding_up) and
            // γ = fee_denom - fee (== gamma_numer). This is the exact-in
            // target-reachable branch of `computeSwapStep`: the consumed input
            // is `amount_in + fee_amount` with BOTH terms rounded up.
            let fee = fee_denom - gamma_numer;
            let fee_amount = if gamma_numer.is_zero() {
                U256::ZERO
            } else {
                u512_div_ceil(
                    U512::from(net_input) * U512::from(fee),
                    U512::from(gamma_numer),
                )
            };
            let gross_input = net_input.saturating_add(fee_amount);

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

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]
    use super::*;
    use alloy::primitives::I256;
    use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
    use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;

    /// Build a 3-range sequence crossing ticks -60, 0, +60 (tick_spacing=60)
    /// with liquidity `L` at every range + the standard net at each boundary.
    /// A multi-range swap through this MUST match a step-by-step
    /// `compute_swap_step_v3` walk (the on-chain-faithful oracle V3 uses),
    /// since V3 single-range matches exactly on mainnet — the divergence the
    /// V4 CurrencyNotSettled fix targets is in `compute_crossing`'s per-range
    /// rounding for multi-range crossings.
    fn three_range_sequence(liq: u128) -> IntV3TickRangeSequence {
        let gamma_numer = 997_000u64; // 0.3% fee
        let fee_denom = 1_000_000u64;
        let sp_neg60 = U256::from(get_sqrt_ratio_at_tick_internal(-60).unwrap());
        let sp_pos60 = U256::from(get_sqrt_ratio_at_tick_internal(60).unwrap());
        let sp_zero = U256::from(get_sqrt_ratio_at_tick_internal(0).unwrap());
        let make_hop = |liquidity, sp, lower, upper, zfo| IntV3TickRangeHop {
            liquidity,
            sqrt_price_x96: sp,
            sqrt_price_lower_x96: lower,
            sqrt_price_upper_x96: upper,
            gamma_numer,
            fee_denom,
            zero_for_one: zfo,
        };
        // ofz: ranges [0,60), [60,120)... but for a 3-range test we use
        // [-60,0), [0,60), [60,120) with crossing from sp_zero upward.
        let r0 = make_hop(liq, sp_zero, sp_neg60, sp_pos60, false);
        let r1 = make_hop(
            liq,
            sp_pos60,
            sp_pos60,
            U256::from(get_sqrt_ratio_at_tick_internal(120).unwrap()),
            false,
        );
        let r2 = make_hop(
            liq,
            U256::from(get_sqrt_ratio_at_tick_internal(120).unwrap()),
            sp_pos60,
            U256::from(get_sqrt_ratio_at_tick_internal(180).unwrap()),
            false,
        );
        IntV3TickRangeSequence::new(vec![r0, r1, r2]).unwrap()
    }

    /// Step-by-step oracle: walk `compute_swap_step_v3` across the 3 ranges,
    /// matching on-chain `computeSwapStep` exact-in rounding exactly. Returns
    /// `(total_gross_consumed, total_output)` to reach range `k`'s entry.
    fn oracle_crossing(seq: &IntV3TickRangeSequence, k: usize, amount_in: u128) -> (U256, U256) {
        let fee_pips = U256::from(seq.ranges[0].fee_denom - seq.ranges[0].gamma_numer);
        let zfo = seq.ranges[0].zero_for_one;
        let mut remaining = I256::try_from(amount_in).unwrap(); // exact-in positive (V3 convention)
        let mut total_consumed = U256::ZERO;
        let mut total_output = U256::ZERO;
        for i in 0..k {
            let r = &seq.ranges[i];
            let sp_current = if i == 0 {
                r.sqrt_price_x96
            } else if zfo {
                seq.ranges[i - 1].sqrt_price_lower_x96
            } else {
                seq.ranges[i - 1].sqrt_price_upper_x96
            };
            let sp_target = if zfo {
                r.sqrt_price_lower_x96
            } else {
                r.sqrt_price_upper_x96
            };
            let step = compute_swap_step_v3(
                sp_current,
                sp_target,
                i128::try_from(r.liquidity).unwrap(),
                remaining,
                fee_pips,
            )
            .unwrap();
            // exact-in (V3 positive): consumed = amount_in + fee_amount
            let consumed = step.amount_in.saturating_add(step.fee_amount);
            total_consumed = total_consumed.saturating_add(consumed);
            total_output = total_output.saturating_add(step.amount_out);
            remaining = remaining
                .checked_sub(I256::try_from(consumed).unwrap())
                .unwrap();
        }
        (total_consumed, total_output)
    }

    /// RED→GREEN: `compute_crossing` for a multi-range swap MUST match the
    /// step-by-step `compute_swap_step_v3` walk (the on-chain-faithful oracle).
    /// Before the round-up fix, `compute_crossing` used FLOOR for `amount_in`
    /// and the fee, under-estimating the consumed input → over-predicting the
    /// output for multi-range swaps (the V4 CurrencyNotSettled divergence).
    #[test]
    fn compute_crossing_matches_onchain_step_walk_for_multi_range() {
        let liq = 1_000_000_000_000_000u128; // 1e15 liquidity
        let seq = three_range_sequence(liq);
        // A large-enough input to cross 2 ranges (k=2).
        let amount_in: u128 = 50_000_000_000_000_000u128;
        let crossing = seq.compute_crossing(2).unwrap();
        let (oracle_consumed, oracle_output) = oracle_crossing(&seq, 2, amount_in);
        assert_eq!(
            crossing.crossing_gross_input, oracle_consumed,
            "crossing_gross_input must match on-chain per-step consumed (round-up amount_in + fee)"
        );
        assert_eq!(
            crossing.crossing_output, oracle_output,
            "crossing_output must match on-chain per-step output (round-down)"
        );
    }
}
