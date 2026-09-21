use alloy::primitives::U256;

use super::IntV3TickRangeHop;

use super::telemetry::WALK_WORD_STEPS;

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
/// Result of simulating a V3 swap within a single tick range.
///
/// V3's swap function partial-fills: if the input exceeds the range capacity,
/// only the consumed portion is used and the unused remainder is retained by
/// the caller (cf. `amountSpecified - amountSpecifiedRemaining` in
/// UniswapV3Pool.sol). `consumed_input` tracks this consumed amount so that
/// the profit calculation uses the actual cost, not the full specified input.
#[derive(Clone, Debug, Default)]
pub struct V3SwapResult {
    /// Gross input actually consumed (including fees).
    ///
    /// When the swap does NOT reach the range boundary, `consumed_input ==
    /// amount_in` (the entire input is consumed). When the boundary is hit,
    /// `consumed_input < amount_in`; the remainder stays with the caller.
    pub consumed_input: U256,
    /// Output amount from the swap.
    pub output: U256,
}

/// Simulate a V3 swap within a single tick range using integer arithmetic.
///
/// Returns a [`V3SwapResult`] with the consumed input and output amounts.
/// When the range boundary is reached before the full input is consumed,
/// `consumed_input` is the amount that would actually be charged by the V3
/// pool — matching the on-chain behavior where `amountSpecifiedRemaining`
/// tracks the unused portion.
///
/// This matches `computeSwapStep` in the Uniswap V3/V4 contracts: each step
/// computes `amountIn + feeAmount` as the consumed gross input and `amountOut`
/// as the output. If the price target is reached, only the portion needed to
/// reach the target is consumed.
#[must_use = "the V3 swap result should be used"]
#[hotpath::measure(label = "cl_solve.int_simulate_v3_swap")]
pub fn int_simulate_v3_swap(amount_in: U256, v3_hop: &IntV3TickRangeHop) -> V3SwapResult {
    // Per-step parity: per-step rounding is delegated to the canonical V3
    // step function `compute_swap_step_v3` — the single source of the
    // word-boundary flooring parity. The on-chain V3/V4 PoolManager floors
    // `computeSwapStep` at EVERY word boundary, so this function walks
    // `word_boundary_prices` (entry→exit, swap order) one
    // `compute_swap_step_v3` per boundary — exactly mirroring
    // `v3_simulate_swap`'s loop — so the accumulated per-step fee rounding
    // matches the sim byte-for-byte on sparse-tick pools. For a single-word
    // range (`word_boundary_prices` empty) the walk degenerates to one step
    // to the exit boundary.
    use alloy::primitives::I256;
    use degenbot_math::cl::swap_math::compute_swap_step_v3;

    if amount_in.is_zero() || v3_hop.liquidity == 0 {
        return V3SwapResult::default();
    }

    let liquidity = i128::try_from(v3_hop.liquidity).unwrap_or(i128::MAX);
    let fee_pips = U256::from(v3_hop.fee_denom - v3_hop.gamma_numer);
    let exit_price = if v3_hop.zero_for_one {
        v3_hop.sqrt_price_lower_x96
    } else {
        v3_hop.sqrt_price_upper_x96
    };
    // Absurd inputs (>= 2^255 — the active-set walk's window-edge probes can
    // synthesize them) saturate to `I256::MAX`; the canonical step then hits
    // the range boundary and reports the boundary-crossing consumed amount
    // (matching the prior closed form's saturating semantics).
    let mut remaining = I256::try_from(amount_in).unwrap_or(I256::MAX);

    let mut sp = v3_hop.sqrt_price_x96;
    let mut total_output = U256::ZERO;
    let mut total_consumed = U256::ZERO;

    // Walk entry → [interior word boundaries] → exit, one
    // `compute_swap_step_v3` per target. The walk stops early when the
    // remaining input is exhausted before reaching a target (the partial
    // landing step) — identical to `v3_simulate_swap`'s loop.
    for target in v3_hop
        .word_boundary_prices
        .iter()
        .chain(std::iter::once(&exit_price))
    {
        if remaining <= I256::ZERO {
            break;
        }
        WALK_WORD_STEPS.with(|c| c.set(c.get() + 1));
        let Ok(step) = compute_swap_step_v3(sp, *target, liquidity, remaining, fee_pips) else {
            return V3SwapResult::default();
        };
        let consumed = step.amount_in.saturating_add(step.fee_amount);
        total_consumed = total_consumed.saturating_add(consumed);
        total_output = total_output.saturating_add(step.amount_out);
        sp = step.sqrt_price_next;
        // Subtract the consumed gross input from remaining (exact-in: the
        // step consumed `amount_in + fee_amount`). If the step did NOT reach
        // the target (`sqrt_price_next != target`), the remaining input was
        // exhausted at a partial landing — stop the walk.
        remaining = remaining
            .checked_sub(I256::try_from(consumed).unwrap_or(I256::MAX))
            .unwrap_or(I256::ZERO);
        if sp != *target {
            break;
        }
    }

    V3SwapResult {
        consumed_input: total_consumed,
        output: total_output,
    }
}
