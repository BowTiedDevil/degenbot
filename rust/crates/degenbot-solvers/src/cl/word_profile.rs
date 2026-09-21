use alloy::primitives::U256;

use super::IntV3TickRangeHop;

use super::hop_sim::V3RangeSwapResult;
use super::telemetry::bump_word_steps;

// ---------------------------------------------------------------------------
// Event-solver inversion (loop 15)
// ---------------------------------------------------------------------------

// The floor-cancel lemma: for integer W, `floor(f(x)) >= W  ⟺  f(x) >= W`.
// Applied per hop, the realized (integer, floor-rounded-at-every-stage)
// chain inverts EXACTLY by nested exact-out inversions with ceiling at each
// stage — unlike the loop-14 prefix-composed Möbius inverse, whose single
// real-domain preimage misses by the accumulated quantizer drift. This
// section implements that nested inversion.

/// Smallest gross input whose exact-input `compute_swap_step_v3` from
/// `price` toward `target` produces output >= `w`.
///
/// Uses the canonical NEGATIVE-remaining (exact-out) branch of
/// `compute_swap_step_v3` — the same arithmetic the V3 pool itself runs for
/// exact-out swaps — and returns `amount_in + fee_amount`: the gross input
/// that buys `w` through this step.
fn v3_step_min_gross_for_output(
    price: U256,
    target: U256,
    liquidity: i128,
    fee_pips: U256,
    w: U256,
) -> Option<U256> {
    use alloy::primitives::I256;
    use degenbot_math::cl::swap_math::compute_swap_step_v3;
    let w_signed = I256::try_from(w).ok()?;
    let step = compute_swap_step_v3(price, target, liquidity, -w_signed, fee_pips).ok()?;
    step.amount_in.checked_add(step.fee_amount)
}

/// Smallest ending-range input whose realized profile output is >= `w`.
/// Returns `None` when `w` exceeds the ending range's total output capacity.
pub(super) fn word_profile_min_input_for_output(profile: &ClWordProfile, w: U256) -> Option<U256> {
    if w.is_zero() {
        return Some(U256::ZERO);
    }
    // `output[]` is non-decreasing (step outputs are non-negative): find the
    // first step boundary that reaches `w`.
    if profile.output.last().is_none_or(|o| *o < w) {
        return None; // beyond the ending range's total capacity
    }
    let m = profile.output.partition_point(|o| *o < w);
    debug_assert!(m >= 1, "output[0] == 0 < w");
    // The crossing lives inside step m−1 (`price[m−1] -> target[m−1]`), whose
    // completed form is at `consumed[m]`. The partial-step demand is
    // `w − output[m−1]`, bought by the exact-out step at the step's own fee.
    let base_c = profile.consumed[m - 1];
    let base_o = profile.output[m - 1];
    let w_step = w - base_o;
    let full_gross = profile.consumed[m] - base_c;
    let g = v3_step_min_gross_for_output(
        profile.price[m - 1],
        profile.target[m - 1],
        profile.liquidity,
        profile.fee_pips,
        w_step,
    )?;
    Some(base_c + g.min(full_gross))
}

/// One-time precomputed forward word-boundary profile of a single dense CL
/// `ending_range` for `simulate_v3_range_swap`. The active-set walk calls
/// `simulate_v3_range_swap` ~`sims` times on the SAME ending range (fixed entry
/// price, liquidity, fee, and word-boundary list — only `amount_in` varies), so
/// the per-boundary prefix is recomputed on nearly every simulation. For a range
/// with K word boundaries that is ~`sims × K` `compute_swap_step_v3` calls; the
/// profile reduces a query to a binary search + one partial landing step (~1
/// call) after a one-time O(K) build.
///
/// Byte-for-byte equivalent to the linear walk: the prefix is built with a
/// maximal `remaining` (I256::MAX) so each step reaches its boundary exactly as
/// a per-sim walk would; the landing step is then computed live with the
/// candidate's real remaining. `consumed` is non-decreasing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClWordProfile {
    liquidity: i128,
    fee_pips: U256,
    /// `price[j]` = price after completing `j` full steps (j=0 is the entry).
    price: Vec<U256>,
    /// `target[j]` = the boundary/exit price the (j+1)-th step runs toward.
    target: Vec<U256>,
    /// `consumed[j]` / `output[j]` = cumulative gross input / output after `j`
    /// full steps (j = 0..=target.len()).
    pub(super) consumed: Vec<U256>,
    output: Vec<U256>,
}

impl ClWordProfile {
    /// Build the profile with one full walk. `None` for a degenerate hop (zero
    /// liquidity or no word boundaries) which the linear walk already handles.
    pub(super) fn build(v3_hop: &IntV3TickRangeHop) -> Option<Self> {
        use alloy::primitives::I256;
        use degenbot_math::cl::swap_math::compute_swap_step_v3;
        let liquidity = i128::try_from(v3_hop.liquidity).ok()?;
        if v3_hop.liquidity == 0 {
            return None;
        }
        let fee_pips = U256::from(v3_hop.fee_denom - v3_hop.gamma_numer);
        let exit_price = if v3_hop.zero_for_one {
            v3_hop.sqrt_price_lower_x96
        } else {
            v3_hop.sqrt_price_upper_x96
        };
        let full = I256::MAX;
        let nb = v3_hop.word_boundary_prices.len();
        let mut price = Vec::with_capacity(nb + 1);
        let mut target = Vec::with_capacity(nb);
        let mut consumed = Vec::with_capacity(nb + 1);
        let mut output = Vec::with_capacity(nb + 1);
        let mut sp = v3_hop.sqrt_price_x96;
        let mut cum_c = U256::ZERO;
        let mut cum_o = U256::ZERO;
        price.push(sp);
        consumed.push(U256::ZERO);
        output.push(U256::ZERO);
        for target_price in v3_hop
            .word_boundary_prices
            .iter()
            .copied()
            .chain(std::iter::once(exit_price))
        {
            bump_word_steps(1);
            let Ok(step) = compute_swap_step_v3(sp, target_price, liquidity, full, fee_pips) else {
                return None;
            };
            target.push(target_price);
            cum_c = cum_c.saturating_add(step.amount_in.saturating_add(step.fee_amount));
            cum_o = cum_o.saturating_add(step.amount_out);
            consumed.push(cum_c);
            output.push(cum_o);
            sp = step.sqrt_price_next;
            price.push(sp);
        }
        Some(Self {
            liquidity,
            fee_pips,
            price,
            target,
            consumed,
            output,
        })
    }

    /// O(log K) replacement for `simulate_v3_range_swap(amount_in, v3_hop)` on the
    /// hop this profile was built from.
    pub(super) fn swap(&self, amount_in: U256) -> V3RangeSwapResult {
        use alloy::primitives::I256;
        use degenbot_math::cl::swap_math::compute_swap_step_v3;
        if amount_in.is_zero() {
            return V3RangeSwapResult::default();
        }
        let n = self.target.len();
        // `j` = largest index with `consumed[j] <= amount_in` (`consumed[0] ==
        // 0`, so the partition point is >=1 and `j >= 0`). `j == n` means the
        // input covers the full walk to the exit.
        let j = self.consumed.partition_point(|c| c <= &amount_in) - 1;
        if j >= n {
            return V3RangeSwapResult {
                consumed_input: self.consumed[n],
                output: self.output[n],
            };
        }
        let base_c = self.consumed[j];
        let base_o = self.output[j];
        let remaining = amount_in - base_c;
        if remaining.is_zero() {
            return V3RangeSwapResult {
                consumed_input: base_c,
                output: base_o,
            };
        }
        bump_word_steps(1);
        let Ok(step) = compute_swap_step_v3(
            self.price[j],
            self.target[j],
            self.liquidity,
            I256::try_from(remaining).unwrap_or(I256::MAX),
            self.fee_pips,
        ) else {
            return V3RangeSwapResult::default();
        };
        let c = step.amount_in.saturating_add(step.fee_amount);
        V3RangeSwapResult {
            consumed_input: base_c.saturating_add(c),
            output: base_o.saturating_add(step.amount_out),
        }
    }
}
