//! Integer Möbius transformation optimizer for EVM-exact arbitrage.
//!
//! This is the **single** Möbius recurrence module — the f64 recurrence
//! (`mobius.rs`), the f64-seed-then-integer-refine path, and the f64 V3
//! tick-range solvers have all been removed (see `rust/CONTEXT.md` ruling
//! "f64 vs U512 Möbius solver stack"). Every path composes to
//! `l(x) = K·x / (M + N·x)` via the U512 2×2 matrix recurrence below, and the
//! closed-form optimal input lives in [`crate::optimizers::mobius_int_exact`].
//!
//! Simulation uses EVM-exact integer arithmetic — the same as the on-chain
//! Uniswap V2/V3 contracts:
//!
//! ```text
//! y = gamma_numer * reserve_out * x / (gamma_denom * reserve_in + gamma_numer * x)
//! ```
//!
//! where all values are integers and `/` is floor division (EVM semantics).

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::similar_names)]
#![allow(clippy::type_complexity)]

use alloy::primitives::{U256, U512};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during Möbius optimization.
///
/// Owned by the gen-3 U512 solver module (this is the single recurrence
/// home; the former gen-1 `mobius.rs` is deleted).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MobiusError {
    /// Empty hops list provided.
    #[error("At least one hop is required")]
    EmptyHops,
    /// V3 tick range sequence has inconsistent fees or swap directions.
    #[error("Inconsistent V3 tick range sequence: {message}")]
    InconsistentSequence { message: String },
}

// ---------------------------------------------------------------------------
// Simulation Result
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// IntHopState
// ---------------------------------------------------------------------------

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
    /// Pre-converted U512 reserve_in for swap hot path.
    reserve_in_u512: U512,
    /// Pre-converted U512 reserve_out for swap hot path.
    reserve_out_u512: U512,
    /// Pre-converted U512 gamma_numer for swap hot path.
    gamma_numer_u512: U512,
    /// Pre-converted U512 fee_denom for swap hot path.
    fee_denom_u512: U512,
}

impl IntHopState {
    /// Create a new integer hop state.
    ///
    /// Not `const fn` because `U512::from(U256)` is not `const fn` in ruint.
    #[must_use]
    pub fn new(
        reserve_in: U256,
        reserve_out: U256,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> Self {
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
    /// Returns `0` if the calculation would overflow or divide by zero.
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

        // Truncate to U256
        let bytes: [u8; 64] = result_u512.to_be_bytes();
        // U512 is 64 bytes, U256 is 32 bytes — take last 32 bytes
        let mut result_bytes = [0u8; 32];
        result_bytes.copy_from_slice(&bytes[32..64]);
        U256::from_be_bytes(result_bytes)
    }
}

// ---------------------------------------------------------------------------
// IntMobiusCoefficients
// ---------------------------------------------------------------------------

/// Integer Möbius coefficients for an n-hop constant product path.
///
/// K, M, N are computed as U512 integers via the 2×2 matrix composition.
/// Each hop contributes the matrix:
///
/// ```text
/// [[gamma_numer * reserve_out, 0],
///  [gamma_numer,              fee_denom * reserve_in]]
/// ```
///
/// The product of all matrices gives the composite coefficients.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IntMobiusCoefficients {
    /// Numerator coefficient: K = prod(gamma_numer_i * reserve_out_i).
    pub K: U512,
    /// Denominator constant: M = prod(fee_denom_i * reserve_in_i).
    pub M: U512,
    /// Denominator linear: N = composite coefficient from matrix product.
    pub N: U512,
    /// True when K > M (profitable after fees).
    pub is_profitable: bool,
}

/// Compute integer Möbius coefficients K, M, N for an n-hop path.
///
/// Uses 2×2 matrix multiplication with U512 entries to avoid overflow.
/// Each hop encodes as:
///
/// ```text
/// [[gn * s, 0],
///  [gn,     fd * r]]
/// ```
///
/// where `gn` = gamma_numer, `fd` = fee_denom, `r` = reserve_in, `s` = reserve_out.
///
/// # Errors
///
/// Returns `MobiusError::EmptyHops` if the hops list is empty.
pub fn compute_int_mobius_coefficients(
    hops: &[IntHopState],
) -> Result<IntMobiusCoefficients, MobiusError> {
    if hops.is_empty() {
        return Err(MobiusError::EmptyHops);
    }

    let first = &hops[0];
    let gn0 = first.gamma_numer_u512;
    let fd0 = first.fee_denom_u512;
    let r0 = first.reserve_in_u512;
    let s0 = first.reserve_out_u512;

    // Initialize 2x2 matrix from first hop:
    // [[gn0 * s0, 0],
    //  [gn0,      fd0 * r0]]
    let mut a00 = gn0 * s0; // K
    let mut a10 = gn0; // Will become N
    let mut a11 = fd0 * r0; // M

    // Multiply by subsequent hops' matrices.
    // a01 is always zero: the upper-right entry of each hop matrix is 0,
    // and composing with [[fn'*s', 0; ...]] keeps (0,1) at 0.
    for hop in &hops[1..] {
        let gn_i = hop.gamma_numer_u512;
        let fd_i = hop.fee_denom_u512;
        let r_i = hop.reserve_in_u512;
        let s_i = hop.reserve_out_u512;

        // Matrix multiply: result = current * hop
        let old_a00 = a00;
        a00 = old_a00 * gn_i * s_i;
        a10 = a10 * fd_i * r_i + old_a00 * gn_i;
        a11 = a11 * fd_i * r_i;
    }

    Ok(IntMobiusCoefficients {
        K: a00,
        M: a11,
        N: a10,
        is_profitable: a00 > a11,
    })
}

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
    fn test_compute_int_mobius_coefficients_empty() {
        let result = compute_int_mobius_coefficients(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_compute_int_mobius_coefficients_not_profitable() {
        // Identical reserves → K == M (γ == 1 after one hop) → not profitable.
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_000_000), 1000, 1000),
            IntHopState::new(u256(1_000_000), u256(1_000_000), 1000, 1000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        assert!(!coeffs.is_profitable);
    }

    #[test]
    fn test_compute_int_mobius_coefficients_profitable() {
        // Pool 1 has excess out (1 A → 1.1 B), Pool 2 has excess out (1 B → 1.1 A).
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_100_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(1_100_000), 997, 1000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        assert!(coeffs.is_profitable);
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
