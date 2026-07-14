//! V2 constant-product hop state + EVM-exact swap primitives.
//!
//! These types and functions moved here from
//! `degenbot-bot/src/solvers/mobius_int.rs` (the Möbius solver module): a
//! single-hop swap amount calc is a pool-specific, value-only concern, not an
//! arbitrage/path-optimization concern, so it does not belong inside the
//! solver module. See the crate-level docs for the full rationale.
//!
//! The Möbius solver in `degenbot-bot` imports [`IntHopState`] from here and
//! composes per-hop swaps into multi-hop arbitrage paths
//! (`compute_int_mobius_coefficients`, `exact_mobius_solve`), which stay in
//! the solver crate — only the primitive swap surface lives here.

use alloy::primitives::U256;

// -----------------------------------------------------------------------
// SimulationResult
// -----------------------------------------------------------------------

/// Result from simulating a multi-hop swap path.
///
/// `hop_outputs[i]` is the output amount after hop `i` (0-indexed).
/// `consumed_inputs[i]` is the gross input actually consumed by hop `i`
/// (including fees). For V2 hops, `consumed_inputs[i] == amount_in_to_hop`
/// (constant-product pools always consume the full input). For V3/V4 hops,
/// if the range boundary is hit, `consumed_inputs[i] < amount_in_to_hop`;
/// the unused remainder is retained by the caller.
///
/// `final_output` equals `hop_outputs.last()` for non-empty paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulationResult {
    /// Final output amount after all hops.
    pub final_output: U256,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`.
    pub hop_outputs: Vec<U256>,
    /// Per-hop consumed input amounts. `consumed_inputs[i]` = gross input
    /// actually consumed by hop `i` (including fees).
    pub consumed_inputs: Vec<U256>,
}

// -----------------------------------------------------------------------
// HopSwapError
// -----------------------------------------------------------------------

/// A single V2 constant-product hop swap reverted — mirrors the on-chain
/// `getAmountOut` revert conditions.
///
/// On-chain, Uniswap V2's `getAmountOut` computes in `uint256` with `SafeMath`
/// `.mul` / `.add`; an intermediate overflow reverts the call. The hop swap
/// primitive must surface the same condition (rather than silently widening
/// to a wider integer and returning a phantom output the chain would never
/// produce), so callers can treat the path as reverting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopSwapError {
    /// A `uint256` intermediate (`amountIn * gamma`, `amountInWithFee *
    /// reserveOut`, `reserveIn * feeDenom`, or their sum) overflowed — the
    /// on-chain `getAmountOut` reverts here via `SafeMath`.
    Overflow,
}

// -----------------------------------------------------------------------
// IntHopState
// -----------------------------------------------------------------------

/// Fee parameters for a single pool hop.
///
/// In Uniswap V2, the fee is expressed as `gamma_numer / fee_denom`:
/// - 0.3% fee → `gamma_numer = 997`, `fee_denom = 1000`
/// - 0.05% fee → `gamma_numer = 9995`, `fee_denom = 10000`
///
/// `gamma_numer` is the retained fraction (what passes through after fees),
/// not the fee amount itself.
///
/// The swap formula is:
/// `y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)`
///
/// Reserves are held as `U256` to match the on-chain `getAmountOut` arithmetic
/// width (`uint256`): the pair contract stores reserves as `uint112`, but the
/// swap math widens them to `uint256` (`SafeMath`). Reserves sourced from
/// [`V2PoolState`](degenbot_pools::v2_state::V2PoolState) (`uint112`) are
/// widened at the call site — the same widening `Solidity` performs.
///
/// `gamma_numer` / `fee_denom` are likewise held as `U256` (the swap-math
/// width), widened once in [`new`](IntHopState::new) from their natural `u64`
/// fee-parameter representation. This eliminates a per-swap `U256::from`
/// conversion in the hot path (the constructor runs once per pool-state
/// snapshot; [`swap`](IntHopState::swap) runs per candidate amount). They
/// remain small (e.g. 997 / 1000) and fit comfortably in `uint256`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntHopState {
    /// Reserve of the input token (`uint256` swap-math width).
    pub reserve_in: U256,
    /// Reserve of the output token (`uint256` swap-math width).
    pub reserve_out: U256,
    /// Gamma numerator: the retained fraction (e.g. 997 for 0.3% fee), held as
    /// `uint256` swap-math width (widened once in `new`).
    pub gamma_numer: U256,
    /// Fee denominator (e.g. 1000 for 0.3% fee), held as `uint256` swap-math
    /// width (widened once in `new`).
    pub fee_denom: U256,
}

impl IntHopState {
    /// Create a new integer hop state.
    ///
    /// `gamma_numer` and `fee_denom` are widened to `U256` here (once per
    /// pool-state snapshot) so the per-swap hot path needs no conversion.
    #[must_use]
    pub fn new(reserve_in: U256, reserve_out: U256, gamma_numer: u64, fee_denom: u64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            gamma_numer: U256::from(gamma_numer),
            fee_denom: U256::from(fee_denom),
        }
    }

    /// Simulate a swap through this hop using EVM-exact integer arithmetic.
    ///
    /// Returns `Ok(0)` if the denominator (`fee_denom * reserve_in +
    /// gamma_numer * x`) is zero — the constant-product formula is undefined
    /// there, but the swap output is well-defined as zero (no positive `x`
    /// can extract anything from a pool whose `reserve_in`=0 or whose fee
    /// convention degenerates).
    ///
    /// # Errors
    ///
    /// Returns [`HopSwapError::Overflow`] when a `uint256` intermediate
    /// overflows — mirroring the on-chain `getAmountOut` `SafeMath` revert.
    /// On-chain a wider-than-`uint256` result is never produced (the call
    /// reverts first), so the swap primitive surfaces the same condition
    /// instead of returning a phantom output via a wider integer.
    pub fn swap(&self, x: U256) -> Result<U256, HopSwapError> {
        // Mirror on-chain `getAmountOut` exactly: all arithmetic in `uint256`
        // with `SafeMath` `.mul` / `.add`, which revert on overflow. Reserves
        // and fee parameters arrive as `U256` (the swap-math width — `uint112`
        // storage widened at the call site, as `Solidity` does; fee params
        // widened once in `new`). Widening to a wider integer would here only
        // manufacture outputs the chain never produces: any `uint256`-
        // overflowing intermediate reverts on-chain, so we surface it as
        // `Overflow` rather than returning a phantom result.
        //
        // `getAmountOut`:
        //   amountInWithFee = amountIn * rate            (gamma_numer)
        //   numerator       = amountInWithFee * reserveOut
        //   denominator     = reserveIn * base + amountInWithFee
        //                  = reserveIn * fee_denom + amountInWithFee
        //   amountOut       = numerator / denominator   (EVM floor DIV)
        let amount_in_with_fee = x
            .checked_mul(self.gamma_numer)
            .ok_or(HopSwapError::Overflow)?;
        let numerator = amount_in_with_fee
            .checked_mul(self.reserve_out)
            .ok_or(HopSwapError::Overflow)?;
        let denom = self
            .reserve_in
            .checked_mul(self.fee_denom)
            .ok_or(HopSwapError::Overflow)?
            .checked_add(amount_in_with_fee)
            .ok_or(HopSwapError::Overflow)?;

        if denom.is_zero() {
            return Ok(U256::ZERO);
        }

        // Floor division — EVM `DIV` semantics.
        Ok(numerator / denom)
    }
}

// -----------------------------------------------------------------------
// int_simulate_path
// -----------------------------------------------------------------------

/// Simulate a swap through all hops using EVM-exact integer arithmetic.
///
/// Each hop applies: `y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)`
/// with floor division (EVM semantics).
///
/// Returns a [`SimulationResult`] with per-hop output and consumed-input amounts.
///
/// # Errors
///
/// Returns [`HopSwapError`] if any hop reverts (on-chain `getAmountOut`
/// overflow) — the whole multi-hop swap reverts on-chain if any hop does.
pub fn int_simulate_path(x: U256, hops: &[IntHopState]) -> Result<SimulationResult, HopSwapError> {
    let mut amount = x;
    let mut hop_outputs = Vec::with_capacity(hops.len());
    // V2 constant-product pools always consume the full input
    let consumed_inputs = vec![x; hops.len()];
    for hop in hops {
        if amount.is_zero() {
            return Ok(SimulationResult {
                final_output: U256::ZERO,
                hop_outputs,
                consumed_inputs,
            });
        }
        amount = hop.swap(amount)?;
        hop_outputs.push(amount);
    }
    Ok(SimulationResult {
        final_output: amount,
        hop_outputs,
        consumed_inputs,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn u256(n: u64) -> U256 {
        U256::from(n)
    }

    #[test]
    fn new_widens_fee_params_to_u256_once() {
        // The fee parameters are widened to `U256` in `new` so the per-swap
        // hot path issues no `U256::from` conversion. This locks that
        // invariant: the struct holds the widened values, not the `u64` inputs.
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        assert_eq!(hop.gamma_numer, U256::from(997u64));
        assert_eq!(hop.fee_denom, U256::from(1000u64));
    }

    #[test]
    fn test_int_hop_swap_zero_input() {
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(U256::ZERO).unwrap();
        assert!(output.is_zero());
    }

    #[test]
    fn test_int_hop_swap_basic() {
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(u256(1000)).unwrap();
        assert!(!output.is_zero());
        assert!(output < u256(2000)); // Output < 2x input
    }

    #[test]
    fn test_int_hop_swap_inverse_round_trip() {
        // Swapping the output back through the reverse pool should recover
        // (approximately) the input — sanity check for the swap formula.
        let fwd = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let rev = IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000);
        let mid = fwd.swap(u256(1000)).unwrap();
        let back = rev.swap(mid).unwrap();
        // After two 0.3% fees, recovered amount is strictly less than input.
        assert!(back < u256(1000));
    }

    #[test]
    fn test_int_simulate_path_zero_input() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = int_simulate_path(U256::ZERO, &hops).unwrap();
        assert!(result.final_output.is_zero());
        assert_eq!(result.hop_outputs.len(), 0);
    }

    #[test]
    fn test_int_simulate_path_multi_hop() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = int_simulate_path(u256(1000), &hops).unwrap();
        // Two hops, both consume full input.
        assert_eq!(result.consumed_inputs, vec![u256(1000), u256(1000)]);
        assert_eq!(result.hop_outputs.len(), 2);
        assert_eq!(result.final_output, *result.hop_outputs.last().unwrap());
    }

    // ── on-chain revert parity (U512 removal) ──────────────────────────

    /// On-chain `getAmountOut` computes `amountInWithFee = amountIn * gamma`
    /// in `uint256` via `SafeMath` and reverts on overflow. With `x = MAX` and a
    /// non-trivial `gamma`, `x * gamma` overflows `uint256` — the trade
    /// reverts. The hop swap must surface this as `Overflow`, NOT return a
    /// phantom output by silently widening to a wider integer.
    #[test]
    fn swap_reverts_on_amount_in_times_gamma_overflow() {
        let hop = IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000);
        // 997 * U256::MAX overflows uint256 → on-chain revert.
        assert_eq!(hop.swap(U256::MAX), Err(HopSwapError::Overflow));
    }

    /// `amountInWithFee * reserveOut` is the second `SafeMath` `mul`; an
    /// overflow here also reverts on-chain.
    #[test]
    fn swap_reverts_on_amount_in_with_fee_times_reserve_out_overflow() {
        // `amountIn * gamma` fits (gamma = 1), but `amountInWithFee *
        // reserveOut` = MAX * MAX overflows uint256 → revert.
        let hop = IntHopState::new(u256(1), U256::MAX, 1, 1000);
        assert_eq!(hop.swap(U256::MAX), Err(HopSwapError::Overflow));
    }

    /// `reserveIn * feeDenom` (a denominator term) overflows uint256 → revert.
    #[test]
    fn swap_reverts_on_reserve_in_times_fee_denom_overflow() {
        let hop = IntHopState::new(U256::MAX, u256(1), 1, u64::MAX);
        assert_eq!(hop.swap(u256(1)), Err(HopSwapError::Overflow));
    }

    /// A multi-hop path reverts if ANY hop reverts — matching on-chain
    /// (the whole multi-hop swap reverts).
    #[test]
    fn int_simulate_path_reverts_if_any_hop_overflows() {
        let good = IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000);
        let reverting = IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000);
        let hops = vec![good, reverting];
        assert_eq!(
            int_simulate_path(U256::MAX, &hops),
            Err(HopSwapError::Overflow),
        );
    }
}
