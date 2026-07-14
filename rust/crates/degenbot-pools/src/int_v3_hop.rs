//! V3 tick-range hop types for intermediate-value (Int) swap simulation —
//! the V3-family analogue of [`degenbot_v2_math::IntHopState`].
//!
//! **Relocated** from `degenbot-bot/src/solvers/mobius_v3_int.rs` (these are
//! pure value types, previously mis-homed under `solvers/`); re-exported there
//! at the historical `crate::solvers::mobius_v3_int::IntV3*` path so consumers
//! resolve unchanged. Transient re-export — repointed natively by USPN7M/P2CKRL.

use alloy::primitives::{U256, U512};
use degenbot_v2_math::IntHopState;

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
            let sp_diff = self
                .sqrt_price_x96
                .saturating_sub(self.sqrt_price_lower_x96);
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
            let sp_diff = self
                .sqrt_price_upper_x96
                .saturating_sub(self.sqrt_price_x96);
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
