//! Integer Möbius transformation solver for EVM-exact arbitrage.
//!
//! This is the **single** Möbius recurrence module — the f64 recurrence
//! (`mobius.rs`), the f64-seed-then-integer-refine path, and the f64 V3
//! tick-range solvers have all been removed. Every path composes to
//! `l(x) = K·x / (M + N·x)` via the U512 2×2 matrix recurrence below, and the
//! closed-form optimal input lives in [`crate::solvers::mobius_int_exact`].
//!
//! The per-hop swap primitive — `IntHopState`, `IntHopState::swap`, and
//! `int_simulate_path` — has been extracted into the standalone
//! [`degenbot_math::v2`] leaf crate (a pool-specific, value-only concern, not
//! a solver/arbitrage concern). This module imports `IntHopState` from there
//! and composes it into the Möbius recurrence + `IntMobiusCoefficients` that
//! stay here.
//!
//! Simulation uses EVM-exact integer arithmetic — the same as the on-chain
//! Uniswap V2/V3 contracts:
//!
//! ```text
//! y = gamma_numer * reserve_out * x / (gamma_denom * reserve_in + gamma_numer * x)
//! ```
//!
//! where all values are integers and `/` is floor division (EVM semantics).

#![expect(non_snake_case)]

use alloy::primitives::U512;
use degenbot_math::v2::IntHopState;

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
    // Widen the per-hop U256 fields to U512 for the matrix recurrence.
    // `IntHopState` holds `reserve_in`/`reserve_out`/`gamma_numer`/`fee_denom`
    // as `U256` (the swap-math width); this recurrence needs the wider `U512`
    // to avoid overflow across multi-hop compositions. `U512::from(U256)` is
    // the cross-width widening (a widened copy of the limbs); each hop is read
    // once here, so the cost is one widening per hop.
    let gn0 = U512::from(first.gamma_numer);
    let fd0 = U512::from(first.fee_denom);
    let r0 = U512::from(first.reserve_in);
    let s0 = U512::from(first.reserve_out);

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
        let gn_i = U512::from(hop.gamma_numer);
        let fd_i = U512::from(hop.fee_denom);
        let r_i = U512::from(hop.reserve_in);
        let s_i = U512::from(hop.reserve_out);

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

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn u256(n: u64) -> U256 {
        U256::from(n)
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
}
