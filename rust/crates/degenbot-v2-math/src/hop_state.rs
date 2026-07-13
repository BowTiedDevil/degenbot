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

use alloy::primitives::{U256, U512};

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
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntHopState {
    /// Reserve of the input token (uint256 scale).
    pub reserve_in: U256,
    /// Reserve of the output token (uint256 scale).
    pub reserve_out: U256,
    /// Gamma numerator: the retained fraction (e.g. 997 for 0.3% fee).
    pub gamma_numer: u64,
    /// Fee denominator (e.g. 1000 for 0.3% fee).
    pub fee_denom: u64,
    /// Pre-converted U512 `reserve_in` for swap hot path.
    reserve_in_u512: U512,
    /// Pre-converted U512 `reserve_out` for swap hot path.
    reserve_out_u512: U512,
    /// Pre-converted U512 `gamma_numer` for swap hot path.
    gamma_numer_u512: U512,
    /// Pre-converted U512 `fee_denom` for swap hot path.
    fee_denom_u512: U512,
}

impl IntHopState {
    /// Create a new integer hop state.
    ///
    /// Not `const fn` because `U512::from(U256)` is not `const fn` in ruint.
    #[must_use]
    pub fn new(reserve_in: U256, reserve_out: U256, gamma_numer: u64, fee_denom: u64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            gamma_numer,
            fee_denom,
            reserve_in_u512: U512::from(reserve_in),
            reserve_out_u512: U512::from(reserve_out),
            gamma_numer_u512: U512::from(gamma_numer),
            fee_denom_u512: U512::from(fee_denom),
        }
    }

    /// Simulate a swap through this hop using EVM-exact integer arithmetic.
    ///
    /// Returns `0` if the denominator (sum of `fee_denom * reserve_in` and
    /// `gamma_numer * x`) is zero — the constant-product formula is undefined
    /// there, but the swap output is well-defined as zero (no positive `x`
    /// can extract anything from a pool whose `reserve_in`=0 or whose fee
    /// convention degenerates).
    ///
    /// # Panics
    ///
    /// Panics if the quotient overflows `U256` — i.e. if the input violates
    /// the spec-bound pool invariants (`reserve_out > uint112::MAX` for V2,
    /// or non-V2 family pools passed in). Real V2/Solidly-volatile state
    /// satisfies `reserve_out ≤ uint112::MAX`, and the swap output is bounded
    /// by `reserve_out` (you can't extract more than the pool holds), so
    /// this is unreachable for state ingested from on-chain `Sync` events.
    /// The spec widths are enforced at `register_v2_pool` /
    /// `register_v3_pool` / `register_v4_pool` (see `bot_core/spec_bounds.rs`
    /// and ADR-012); see `u512_to_u256_internal` for the narrowing contract.
    #[must_use]
    pub fn swap(&self, x: U256) -> U256 {
        // y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)
        // All U512 values pre-converted at construction to avoid repeated conversions.
        let x_u512 = U512::from(x);

        // numerator = gamma_numer * reserve_out * x
        let numerator = self.gamma_numer_u512 * self.reserve_out_u512 * x_u512;

        // denominator = fee_denom * reserve_in + gamma_numer * x
        let denom = self.fee_denom_u512 * self.reserve_in_u512 + self.gamma_numer_u512 * x_u512;

        if denom.is_zero() {
            return U256::ZERO;
        }

        // Floor division (EVM semantics)
        let result_u512 = numerator / denom;

        // Narrow U512 → U256. Bounded by `reserve_out` (an output swap can
        // never extract more than the pool holds): `result ≤ γ·reserve_out·x /
        // (γ·x) = reserve_out ≤ uint112::MAX` for spec-bound V2 state — so this
        // is unreachable for real pools. The spec widths are now enforced at
        // `register_v2_pool` / `register_v3_pool` / `register_v4_pool`
        // (see `bot_core/spec_bounds.rs` and ADR-012); on-chain-sourced
        // pool state cannot reach this panic.
        assert!(
            result_u512 <= U512::from(U256::MAX),
            "U512 → U256 narrowing overflow (corrupt/synthetic input; \
             spec-bound pool state is unreachable — enforced at register_*_pool)",
        );
        result_u512.to::<U256>()
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
#[must_use]
pub fn int_simulate_path(x: U256, hops: &[IntHopState]) -> SimulationResult {
    let mut amount = x;
    let mut hop_outputs = Vec::with_capacity(hops.len());
    // V2 constant-product pools always consume the full input
    let consumed_inputs = vec![x; hops.len()];
    for hop in hops {
        if amount.is_zero() {
            return SimulationResult {
                final_output: U256::ZERO,
                hop_outputs,
                consumed_inputs,
            };
        }
        amount = hop.swap(amount);
        hop_outputs.push(amount);
    }
    SimulationResult {
        final_output: amount,
        hop_outputs,
        consumed_inputs,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn u256(n: u64) -> U256 {
        U256::from(n)
    }

    #[test]
    fn test_int_hop_swap_zero_input() {
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(U256::ZERO);
        assert!(output.is_zero());
    }

    #[test]
    fn test_int_hop_swap_basic() {
        let hop = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let output = hop.swap(u256(1000));
        assert!(!output.is_zero());
        assert!(output < u256(2000)); // Output < 2x input
    }

    #[test]
    fn test_int_hop_swap_inverse_round_trip() {
        // Swapping the output back through the reverse pool should recover
        // (approximately) the input — sanity check for the swap formula.
        let fwd = IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000);
        let rev = IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000);
        let mid = fwd.swap(u256(1000));
        let back = rev.swap(mid);
        // After two 0.3% fees, recovered amount is strictly less than input.
        assert!(back < u256(1000));
    }

    #[test]
    fn test_int_simulate_path_zero_input() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = int_simulate_path(U256::ZERO, &hops);
        assert!(result.final_output.is_zero());
        assert_eq!(result.hop_outputs.len(), 0);
    }

    #[test]
    fn test_int_simulate_path_multi_hop() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(2_000_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = int_simulate_path(u256(1000), &hops);
        // Two hops, both consume full input.
        assert_eq!(result.consumed_inputs, vec![u256(1000), u256(1000)]);
        assert_eq!(result.hop_outputs.len(), 2);
        assert_eq!(result.final_output, *result.hop_outputs.last().unwrap());
    }
}
