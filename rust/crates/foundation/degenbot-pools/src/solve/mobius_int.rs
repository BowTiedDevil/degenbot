//! Integer Möbius transformation solver for EVM-exact arbitrage.
//!
//! This is the **single** Möbius recurrence module — the f64 recurrence
//! (`mobius.rs`), the f64-seed-then-integer-refine path, and the f64 V3
//! tick-range solvers have all been removed. Every path composes to
//! `l(x) = K·x / (M + N·x)` via the U512 2×2 matrix recurrence below, and the
//! closed-form optimal input lives in [`crate::solve::mobius_int_exact`].
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

#[cfg(test)]
use alloy::primitives::U256;
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
    #[expect(
        dead_code,
        reason = "typed error arm kept for the widened solver paths; unreached today"
    )]
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
    /// Numerator coefficient: K = `prod(gamma_numer_i` * `reserve_out_i`).
    pub K: U512,
    /// Denominator constant: M = `prod(fee_denom_i` * `reserve_in_i`).
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
/// where `gn` = `gamma_numer`, `fd` = `fee_denom`, `r` = `reserve_in`, `s` = `reserve_out`.
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

// ---------------------------------------------------------------------------
// floor_jog_bound
// ---------------------------------------------------------------------------

// The residual bound is the solver contract's *measured* side: its consumer is
// the `displacement` test module in `mobius_int_exact`, which is why these
// items compile under `cfg(test)` only. A production consumer that needs the
// bound (rather than only the tests that assert it) drops the cfg.

/// `⌈numer / denom⌉` over `U512`. Every call site guards `denom` non-zero.
#[cfg(test)]
fn ceil_div(numer: U512, denom: U512) -> U512 {
    let quotient = numer / denom;
    if (numer % denom).is_zero() {
        quotient
    } else {
        quotient + U512::from(1u64)
    }
}

/// Scale `acc` by one hop's zero-input marginal rate `ρ = γ·s/r`, rounding
/// up (`acc` and the result are in `1/2³²` wei). `None` when the
/// widened product leaves `U512` or the hop degenerates to a zero
/// denominator — both mean a jog far past `U256`.
#[cfg(test)]
fn scaled_hop_rate(acc: U512, hop: &IntHopState) -> Option<U512> {
    let rate_numer = U512::from(hop.gamma_numer).checked_mul(U512::from(hop.reserve_out))?;
    let numer = rate_numer.checked_mul(acc)?;
    let denom = U512::from(hop.fee_denom).checked_mul(U512::from(hop.reserve_in))?;
    if denom.is_zero() {
        return None;
    }
    Some(ceil_div(numer, denom))
}

/// Bound the anchor-only solve's residual on this path, in wei: the
/// floor-exact EVM chain's shortfall against the smooth Möbius envelope it
/// staircases, plus the plateau-slope slack of evaluating the floored anchor.
///
/// This is a property of the hop list alone (nothing about the solver's
/// arithmetic), so it lives beside the coefficients rather than in
/// `mobius_int_exact`.
///
/// # Derivation
///
/// Hop `i`'s swap is the real function
///
/// ```text
/// fᵢ(x) = γᵢ·sᵢ·x / (rᵢ + γᵢ·x)
/// ```
///
/// (`γᵢ` := `gamma_numer` / `fee_denom` — the fee-retained fraction,
/// `rᵢ` = `reserve_in`, `sᵢ` = `reserve_out`), so the EVM hop is
/// `⌊fᵢ(x)⌋ = fᵢ(x) − εᵢ` with the per-hop floor jitter
/// `εᵢ = fᵢ(x) − ⌊fᵢ(x)⌋ ∈ [0, 1)` — EVM `DIV` loses strictly less than one
/// wei (`⌊a/b⌋ > a/b − 1`). Its marginal output rate
///
/// ```text
/// fᵢ'(x) = γᵢ·sᵢ·rᵢ / (rᵢ + γᵢ·x)² ≤ fᵢ'(0) = γᵢ·sᵢ/rᵢ =: ρᵢ
/// ```
///
/// is non-increasing in `x`, so the zero-input rate `ρᵢ` is hop `i`'s
/// largest.
///
/// Let `Xᵢ` be the envelope chain (`X₀ = x`, `X_{i+1} = fᵢ(Xᵢ)`) and `xᵢ`
/// the floored chain (`x₀ = x`, `x_{i+1} = ⌊fᵢ(xᵢ)⌋`) from the same input.
/// Every `fᵢ` is increasing and `⌊·⌋ ≤ ·`, so by induction `Xᵢ ≥ xᵢ`; with
/// `δᵢ := Xᵢ − xᵢ ≥ 0`,
///
/// ```text
/// δ_{i+1} = fᵢ(Xᵢ) − ⌊fᵢ(xᵢ)⌋ = [fᵢ(Xᵢ) − fᵢ(xᵢ)] + εᵢ ≤ ρᵢ·δᵢ + εᵢ
/// ```
///
/// (mean value theorem with `fᵢ' ≤ ρᵢ` on `[xᵢ, Xᵢ]`). Unrolling from
/// `δ₀ = 0` propagates the jog:
///
/// ```text
/// δₙ ≤ Σ_{i<n} εᵢ·Gᵢ < Σ_{i<n} Gᵢ,     Gᵢ := Π_{i<j<n} ρⱼ   (empty product = 1)
/// ```
///
/// — hop `i`'s sub-wei jitter is carried by the marginal rates of every hop
/// downstream of it, while the last hop's jitter has no downstream hop and
/// enters with weight `G_{n−1} = 1`.
///
/// # Why a plain hop count is unsound
///
/// `Gᵢ ≤ 1` iff every downstream `ρⱼ ≤ 1`, so `Σ Gᵢ ≤ n` — the familiar
/// "one wei per hop" — holds only while no hop's rate exceeds 1. A skewed
/// pool (`sᵢ > rᵢ`, hence `ρᵢ > 1`) *amplifies* the jitter of every hop
/// before it, and a count of `n` would then understate the residual.
/// `Σ Gᵢ` is the sound generalization: it equals `n` exactly on a path whose
/// every hop rate is ≤ 1 (each `⌈Gᵢ⌉ = 1`) and exceeds it only where a count
/// would be unsound.
///
/// # Anchor-flooring slack
///
/// `exact_mobius_solve` evaluates the envelope at the floored anchor
/// `x̂ = ⌊x*⌋` instead of searching for the discrete argmax `x*`. The
/// floored chain's profit at input `x` is `P(x) = L(x) − x − δₙ(x)`, so
///
/// ```text
/// P(x*) − P(x̂) ≤ [g(x*) − g(x̂)] + δₙ(x̂),     g(x) := L(x) − x
/// ```
///
/// and `g` is concave with its real maximizer at `x*`, one wei right of
/// `x̂`. Integer inputs at or left of `x̂` never beat it (`g` increases up to
/// `x*`), and integer inputs right of it are at most `x̂ + 1` (`g` decreases
/// past `x*`), so by concavity
///
/// ```text
/// g(x*) − g(x̂) ≤ max(0, L'(x̂) − 1) ≤ max(0, Πᵢρᵢ − 1)
/// ```
///
/// — at most one wei of plateau slope for every profitable path whose
/// zero-input composed rate stays under 2, which is every thin-margin cycle
/// (`L'(0) = Πρᵢ = 1 + margin`). That term is added as `⌈Πρᵢ⌉ − 1`, making
/// the returned value an upper bound on the anchor-only residual for ANY hop
/// list.
///
/// # Rounding
///
/// Ratios accumulate in `1/2³²`-wei fixed point with a ceiling at each step
/// and one final ceiling, so the returned integer over-approximates the exact
/// `Σ Gᵢ + slack`. A path whose jog exceeds `U256::MAX` wei saturates there
/// instead of overflowing (a wrap would understate the bound).
///
/// # Contract use
///
/// The `displacement` tests in `mobius_int_exact` assert the solver's
/// measured residual against a brute-scan reference stays within this bound
/// on 2- and 3-hop ladders. An empty path has no hop to jog and returns
/// zero.
#[cfg(test)]
#[must_use]
pub(crate) fn floor_jog_bound(hops: &[IntHopState]) -> U256 {
    // Fixed-point scale for the rate accumulation (`1/2³²` wei): the
    // per-step ceiling leaks less than one wei over the whole path.
    const SCALE: u64 = 1 << 32;

    let Some((first, downstream)) = hops.split_first() else {
        return U256::ZERO;
    };
    let scale = U512::from(SCALE);
    let ceiling = U512::from(U256::MAX);

    // Walk the tail backwards: `acc` holds `Gᵢ` at hop `i`, in 1/scale wei.
    // The last hop's jitter has no downstream rate to amplify it, so the sum
    // opens with `G_{n−1} = 1`.
    let mut acc = scale;
    let mut sum = scale;
    for hop in downstream.iter().rev() {
        let Some(next_acc) = scaled_hop_rate(acc, hop) else {
            return U256::MAX;
        };
        let Some(next_sum) = sum.checked_add(next_acc) else {
            return U256::MAX;
        };
        acc = next_acc;
        sum = next_sum;
        if acc > ceiling || sum > ceiling {
            return U256::MAX;
        }
    }

    // `Πρᵢ = ρ₀·G₀`: `acc` already carries the downstream product `G₀`, so
    // one more hop-0 ratio completes the zero-input composed rate.
    let Some(composed_rate) = scaled_hop_rate(acc, first) else {
        return U256::MAX;
    };
    if composed_rate > ceiling {
        return U256::MAX;
    }
    let anchor_slack = if composed_rate > scale {
        ceil_div(composed_rate - scale, scale)
    } else {
        U512::ZERO
    };

    let bound = ceil_div(sum + anchor_slack * scale, scale);
    if bound > ceiling {
        return U256::MAX;
    }
    bound.to::<U256>()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

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
    fn floor_jog_bound_empty_path() {
        // No hop, no jog.
        assert_eq!(floor_jog_bound(&[]), U256::ZERO);
    }

    #[test]
    fn floor_jog_bound_is_the_hop_count_when_no_hop_rate_exceeds_one() {
        // γ·s/r = 0.997 on every hop → every downstream product stays ≤ 1, so
        // each ceil'd Gᵢ is 1 and the propagated jog collapses to the plain
        // one-wei-per-hop count. Πρ < 1 here, so no anchor slack either.
        for n in 1..=4usize {
            let hops: Vec<IntHopState> = (0..n)
                .map(|_| IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000))
                .collect();
            assert_eq!(floor_jog_bound(&hops), u256(n as u64));
        }
    }

    #[test]
    fn floor_jog_bound_amplifies_early_jitter_by_downstream_rates() {
        // Hop 1's zero-input rate is ρ₁ = 997·4/1000 = 3.988, so hop 0's
        // sub-wei jitter is carried at that rate: G = [ρ₁, 1] → Σ = 4.988,
        // plus ⌈Πρ − 1⌉ = ⌈0.997·3.988 − 1⌉ = 3 wei of anchor slack, so
        // ⌈7.988⌉ = 8. A hop count of 2 wei would be unsound here.
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(4_000_000), 997, 1000),
        ];
        assert_eq!(floor_jog_bound(&hops), u256(8));
    }

    #[test]
    fn floor_jog_bound_saturates_rather_than_wrapping() {
        // ρ ≈ 2^320 dwarfs any feasible jog; the bound must saturate at
        // U256::MAX, since a wrap would understate it.
        let hops = vec![
            IntHopState::new(u256(1), U256::MAX, u64::MAX, 1),
            IntHopState::new(u256(1), U256::MAX, u64::MAX, 1),
        ];
        assert_eq!(floor_jog_bound(&hops), U256::MAX);
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
