//! Integer-exact Möbius transformation solver.
//!
//! Replaces the f64 Möbius solver with a U512-native implementation that
//! produces EVM-exact results without any float conversion.
//!
//! The key mathematical insight: the Möbius recurrence produces rational
//! coefficients K, M, N that are products of integer reserve × gamma values.
//! The closed-form model-optimal input `x* = (√(K·M) - M) / N` uses only
//! integer square root (available via U512) and integer division.
//!
//! This avoids the precision loss from f64↔integer conversions that caused
//! false positives in mixed V3-V2 paths.
//!
//! ## Two layers of "exact"
//!
//! The module has a deliberate two-layer discipline so each candidate name
//! self-documents what it is exact *at*:
//!
//! - [`compute_mobius_model_optimal_input`] is exact at the **Möbius-model
//!   layer** — it returns the analytic argmax of the smooth rational envelope
//!   `l(x) = K·x / (M + N·x)` as `x* = (√(K·M) - M) / N`, computed in `U512`
//!   with floor-`isqrt` and floor-division. The model envelope treats each
//!   per-hop swap `y = γ·s·x / (D·r + γ·x)` as if its floor division were
//!   mathematical division.
//! - [`exact_mobius_solve`] bridges to the **discrete EVM-simulation
//!   layer** — the actual on-chain chain applies floor (`EVM DIV`) at every
//!   hop, producing a piecewise-linear staircase that *approximates* that
//!   rational envelope. Near the envelope optimum the staircase is flat to
//!   within the floor jog (~1 wei per hop) over a plateau that measured
//!   thousands of wei wide on thin-margin paths, so the discrete argmax is
//!   ILL-CONDITIONED: it can sit arbitrarily far from the model anchor, and
//!   no fixed-radius sweep finds it. [`exact_mobius_solve`] therefore does
//!   NOT search for it: it returns the floor-exact evaluation AT the model
//!   anchor. The residual against the discrete max is bounded by the
//!   COMPUTED propagated floor jog (`mobius_int::floor_jog_bound`): hop
//!   `i`'s sub-wei floor jitter carried at the marginal rates of every hop
//!   downstream of it (`Σᵢ Π_{j>i} γⱼsⱼ/rⱼ`), plus the plateau-slope slack
//!   of flooring the anchor. That is the familiar one-wei-per-hop count on
//!   any path whose hop rates stay at or under 1, and the sound
//!   generalization of it where a skewed pool amplifies earlier jitter —
//!   never a measured constant. Measured against a ±4096-wei brute-scan
//!   reference (see the `displacement` test module): max residual 1 wei on
//!   2-hop and 2 wei on 3-hop ladders, both under their bounds. The envelope
//!   dominates the floored chain pointwise while the anchor evaluation
//!   reaches it to within the jog, so tightening sweeps capture at most that
//!   jog — a sub-wei lottery on a minority of paths — and the solve
//!   evaluates the anchor once and stops.
//!
//! Module-level qualifier: the "exact" in *this module's name*
//! (`mobius_int_exact`) is a discipline label distinguishing the integer
//! pathway (no floats — pure `U512` + `isqrt_u512` + floor-division; contrast
//! with the now-deleted f64 recurrence module). The per-function rename above
//! keeps that discipline label while making the layer of exactness explicit
//! at each per-candidate symbol.

// The module is `pub(crate)` since the W2 demotion, so
// `clippy::must_use_candidate` (a public-API pedantic lint) no longer applies
// here; the former module-level expectation was removed with the door cut.

use alloy::primitives::{U256, U512};

/// 2048-bit unsigned integer — width for the composed determinant magnitude
/// in the shifted-piece fast path (`mobius_shifted_piece`). Not re-exported
/// at the alloy root; construct via `Uint<2048, 32>`.
type U2048 = alloy::primitives::Uint<2048, 32>;

use crate::solve::mobius_int::{
    compute_int_mobius_coefficients, IntMobiusCoefficients, MobiusError,
};
use degenbot_math::v2::{int_simulate_path, IntHopState};

/// Result of [`exact_mobius_solve`]: the floor-exact EVM-sim evaluation at
/// the Möbius-model anchor, not the discrete argmax. The module doc owns the
/// two-layer contract and the residual bound.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExactMobiusResult {
    /// Optimal input amount (uint256). Zero if not profitable.
    pub optimal_input: U256,
    /// Profit = int_simulate_path(optimal_input, hops) - optimal_input.
    /// Zero if not profitable.
    pub profit: U256,
    /// Whether the path is profitable (K > M).
    pub is_profitable: bool,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`.
    /// Empty if not profitable.
    pub hop_outputs: Vec<U256>,
}

/// Solve for the optimal arbitrage input using integer-exact Möbius coefficients.
///
/// # Algorithm
///
/// 1. Compute K, M, N as U512 integers via `compute_int_mobius_coefficients`.
/// 2. Check K > M for profitability (the path is profitable iff the rational
///    envelope `l(x) = K·x / (M + N·x)` is monotone-increasing at `x = 0`,
///    i.e. `K > M`).
/// 3. Compute the **model-optimum anchor** `x* = (isqrt(K * M) - M) / N` —
///    the analytic argmax of the rational envelope, via
///    [`compute_mobius_model_optimal_input`]. This is the Möbius-model
///    optimum (exact over the reals), NOT the discrete EVM-sim optimum.
/// 4. EVM-simulate once at `x*` via `int_simulate_path` and return
///    `int_simulate_path(x*) - x*`.
///
/// The result is the anchor evaluation, not the discrete argmax; the module
/// doc owns the two-layer contract, the ill-conditioning, and the residual
/// bound. No float conversion occurs on any step.
///
/// # Errors
///
/// Returns `MobiusError::EmptyHops` if the hops list is empty.
pub fn exact_mobius_solve(hops: &[IntHopState]) -> Result<ExactMobiusResult, MobiusError> {
    let coeffs = compute_int_mobius_coefficients(hops)?;

    if !coeffs.is_profitable {
        return Ok(ExactMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            is_profitable: false,
            hop_outputs: Vec::new(),
        });
    }

    let x_approx = compute_mobius_model_optimal_input(&coeffs);

    if x_approx.is_zero() {
        // K > M yet the floored anchor is zero: the envelope optimum lies
        // in (0, 1) wei, so every integer input sits past the peak, where
        // envelope profit strictly decreases. The discrete staircase tracks
        // that decrease within the floor jog, leaving the smallest inputs as
        // the only candidates — 1 is the minimal probe, and anything larger
        // differs from it by at most the jog the anchor evaluation accepts.
        let sim = int_simulate_path(U256::from(1u64), hops);
        if let Ok(sim) = sim {
            if sim.final_output > U256::from(1u64) {
                return Ok(ExactMobiusResult {
                    optimal_input: U256::from(1u64),
                    profit: sim.final_output - U256::from(1u64),
                    is_profitable: true,
                    hop_outputs: sim.steps.iter().map(|s| s.output).collect::<Vec<_>>(),
                });
            }
        }
        return Ok(ExactMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            is_profitable: false,
            hop_outputs: Vec::new(),
        });
    }

    // No neighborhood tightening around the anchor: the envelope dominates
    // the floored chain pointwise, so the residual against the discrete max
    // is bounded by the propagated floor jog (see
    // `mobius_int::floor_jog_bound` — the one-wei-per-hop count wherever no
    // hop rate exceeds 1), and at the
    // envelope optimum the composed marginal rate is 1 — every nearby
    // candidate is a near-independent draw at that jog (measured: a ±2
    // sweep's capture is exactly 1 wei on ~16% of thin-margin paths, never
    // more; wider windows show no asymmetry and smooth diminishing
    // returns). One wei is below any executable profit signal, so the solve
    // evaluates the anchor once. The anchor is also the SMALLEST input on
    // the plateau among equal-profit points: it floors the model optimum,
    // and profit past it is flat within the jog.
    let Ok(sim) = int_simulate_path(x_approx, hops) else {
        // Anchor's swap reverts on-chain (uint256 overflow) → no viable
        // arb input.
        return Ok(ExactMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            is_profitable: false,
            hop_outputs: Vec::new(),
        });
    };
    let profit = sim.final_output.saturating_sub(x_approx);

    Ok(ExactMobiusResult {
        optimal_input: if profit.is_zero() {
            U256::ZERO
        } else {
            x_approx
        },
        profit,
        is_profitable: !profit.is_zero(),
        hop_outputs: if profit.is_zero() {
            Vec::new()
        } else {
            sim.steps.iter().map(|s| s.output).collect::<Vec<_>>()
        },
    })
}

/// Compute the closed-form optimal input for the smooth Möbius model
/// `l(x) = K·x / (M + N·x)`.
///
/// ```text
/// x* = (isqrt(K * M) - M) / N
/// ```
///
/// This is the **analytic argmax of the Möbius model envelope** over the
/// reals — NOT the discrete optimum of the actual swap chain, which applies
/// floor (`EVM DIV`) at every hop. [`exact_mobius_solve`] uses it as the
/// anchor; the module doc owns the two-layer contract and the residual bound.
///
/// # Arithmetic discipline
///
/// All arithmetic is `U512`. The integer square root is exact floor
/// `isqrt_u512`. Floor division matches `EVM DIV` semantics.
///
/// This function is also used by the mixed V2-V3 integer solver
/// (`concentrated_liquidity::solve_piecewise`) and the
/// Solidly-bracketed dispatcher
/// (`arb_engine::solver_dispatch::solve_solidly`), each of which uses the
/// model-optimum output as an anchor for its own discrete search
/// (golden-section for the latter, ±\[1,N\] EVM-sim sweep for the mixed path).
pub fn compute_mobius_model_optimal_input(coeffs: &IntMobiusCoefficients) -> U256 {
    // `K * M` needs `bit_len(K) + bit_len(M)` bits. Composed multi-hop
    // coefficients reach ~140-290 bits each, so on deep paths the product
    // exceeds the 512 bits a `U512` multiply provides, and ruint's `Mul`
    // wraps modulo 2^512 rather than panicking. A wrapped product shrinks
    // below `M`, collapsing the anchor to zero and handing the caller a
    // false micro-profit. Widen the discriminant to `U2048`, where every
    // product of `U512` operands is exact. This raises the *product* budget
    // only: `K` and `M` are still `U512`-bounded inputs, so coefficient
    // widths beyond that remain a separate limit of the types.
    let km = u512_to_u2048(coeffs.K) * u512_to_u2048(coeffs.M);
    let m = u512_to_u2048(coeffs.M);
    let n = u512_to_u2048(coeffs.N);

    // Integer square root of the true K*M
    let sqrt_km = isqrt_u2048(km);

    // sqrt(K*M) - M. If the floor root falls below M the integer truncation
    // has erased a vanishingly small (K > M) profit; no usable anchor exists.
    let numerator = if sqrt_km >= m {
        sqrt_km - m
    } else {
        return U256::ZERO;
    };

    // (sqrt(K*M) - M) / N
    if coeffs.N.is_zero() {
        return U256::ZERO;
    }
    let x_u2048 = numerator / n;

    // Narrow U2048 → U512 → U256, keeping the U512→U256 overflow contract.
    narrow_u512_to_u256(u2048_to_u512(x_u2048))
}

/// Integer square root of a U512 value.
///
/// Uses Newton's method with an initial approximation from the bit length.
/// Converges in O(log(bit_width)) iterations.
pub fn isqrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }

    let bit_len = n.bit_len();
    if bit_len == 1 {
        return U512::from(1u64);
    }

    // Initial guess: 2^((bit_len+1)/2). This is >= ceil(sqrt(n)).
    // Starting above the root and iterating downward ensures correct convergence.
    let half_bits = (bit_len + 1).div_ceil(2);
    let mut x = U512::from(1u64) << half_bits;

    // Newton's method: x_{k+1} = (x_k + n / x_k) / 2
    // Since we start above the root, x_{k+1} <= x_k always.
    // We converge when x_{k+1} >= x_k (interleaving) or x_{k+1} == x_k.
    loop {
        let q = n / x;
        let next = (x + q) >> 1;
        if next >= x {
            // Converged — x is the floor sqrt, or next is.
            // Since x >= sqrt(n), check if x is correct.
            break;
        }
        x = next;
    }

    // Post-condition: x is the floor sqrt of n.
    // Verify: x^2 <= n < (x+1)^2
    // Due to integer division, x might be off by 1.
    // If x^2 > n, decrement.
    while x * x > n {
        x -= U512::from(1u64);
    }
    // If (x+1)^2 <= n, increment.
    while (x + U512::from(1u64)) * (x + U512::from(1u64)) <= n {
        x += U512::from(1u64);
    }

    x
}

/// Integer square root of a U2048 value.
///
/// Used by the shifted-piece Möbius model-optimum computation
/// (`mobius_shifted_piece`): the composed determinant `A·D − B·C`, widened
/// from the I1024 fast-path coefficients, reaches up to ~2046 bits for
/// deep paths whose coefficients still fit I1024 — which overflows U1024.
/// Newton's method, same shape as [`isqrt_u512`].
///
/// Returns the floor of √n. Panics are impossible: `U2048` is wide enough
/// that no input from a valid I1024-coefficient determinant overflows.
pub fn isqrt_u2048(n: U2048) -> U2048 {
    if n.is_zero() {
        return U2048::ZERO;
    }

    let bit_len = n.bit_len();
    if bit_len == 1 {
        return U2048::from(1u64);
    }

    // Loop-17: the walk's anchor argmax calls this per piece, and real
    // 1-3-hop determinants are far narrower than U2048 (each hop multiplies
    // ~140-bit per-hop determinants). Narrow the Newton iteration to the
    // smallest limb count: mathematically the same floor-√ algorithm on the
    // same value, so the result is provably identical at a fraction of the
    // wide-division cost.
    if bit_len <= 256 {
        let narrow = u2048_to_u256_lossy(n);
        return u512_to_u2048(isqrt_u512(u512_from_u256(narrow)));
    }
    if bit_len <= 512 {
        return u512_to_u2048(isqrt_u512(u2048_to_u512(n)));
    }

    let half_bits = (bit_len + 1).div_ceil(2);
    let mut x = U2048::from(1u64) << half_bits;

    loop {
        let q = n / x;
        let next = (x + q) >> 1;
        if next >= x {
            break;
        }
        x = next;
    }

    // The correction walks square their probe, and near the domain ceiling
    // the exact square lands past `U2048` (for `n` just under `2^2048` the
    // probe stands at `2^1024`, whose square is `2^2048`). A wrapped product
    // compares as tiny and the upward walk never terminates; an overflowing
    // square is by definition `> n`, so checked squares decide both walks.
    while x.checked_mul(x).is_none_or(|sq| sq > n) {
        x -= U2048::from(1u64);
    }
    while x
        .checked_add(U2048::from(1u64))
        .and_then(|next| next.checked_mul(next))
        .is_some_and(|sq| sq <= n)
    {
        x += U2048::from(1u64);
    }

    x
}

fn u2048_to_u256_lossy(n: U2048) -> U256 {
    let limbs = n.into_limbs();
    debug_assert!(limbs[4..].iter().all(|&l| l == 0));
    U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

fn u512_from_u256(v: U256) -> U512 {
    let limbs = v.into_limbs();
    let mut wide = [0u64; 8];
    wide[..4].copy_from_slice(&limbs);
    U512::from_limbs(wide)
}

fn u2048_to_u512(n: U2048) -> U512 {
    let limbs = n.into_limbs();
    debug_assert!(limbs[8..].iter().all(|&l| l == 0));
    U512::from_limbs([
        limbs[0], limbs[1], limbs[2], limbs[3], limbs[4], limbs[5], limbs[6], limbs[7],
    ])
}

/// Widen a `U512` into the 2048-bit accumulator. Total by construction:
/// every `U512` value fits in `U2048`.
fn u512_to_u2048(v: U512) -> U2048 {
    let limbs = v.into_limbs();
    let mut wide = [0u64; 32];
    wide[..8].copy_from_slice(&limbs);
    U2048::from_limbs(wide)
}

/// Narrow a `U512` to a `U256`.
///
/// For pool state sourced from on-chain events (V2 `Sync(uint112,uint112)`,
/// V3/V4 `Swap`), the inputs are spec-bound: `uint112` V2 reserves,
/// `uint128` V3/V4 liquidity, and TickMath-bounded `sqrtPriceX96` — all kept
/// well within `U256` by their contract-storage types. The narrowing is
/// therefore unreachable for real pool state.
///
/// # Panics
///
/// Panics if `v > U256::MAX`, i.e. on synthetic / corrupt registration
/// (e.g. a programmatically-instantiated V2 pool carrying reserves beyond
/// `uint112::MAX`). The prior implementation silently saturated to
/// `U256::MAX`, which would propagate garbage through downstream Möbius /
/// concentrated liquidity computations; panicking here makes the contract invariant explicit.
/// The spec widths are enforced at `register_v2_pool` /
/// `register_v3_pool` / `register_v4_pool` (see `spec_bounds.rs` and
/// ADR-012), so on-chain-sourced pool state cannot reach this branch.
pub fn narrow_u512_to_u256(v: U512) -> U256 {
    // Narrowing U512 → U256. For pool state sourced from on-chain events
    // (V2 `Sync(uint112,uint112)`, V3/V4 `Swap`) the underlying uint112
    // reserves, uint128 liquidity, and TickMath-bounded sqrt prices keep
    // every quotient well within U256 — so this is unreachable under
    // spec-bound state. The spec widths are now enforced at
    // `register_v2_pool` / `register_v3_pool` / `register_v4_pool`
    // (see `bot_core/spec_bounds.rs` and ADR-012), so on-chain-sourced
    // pool state cannot reach this panic. The explicit pre-check makes
    // that invariant visible and lets synthetic / corrupt registration
    // fail loudly instead of silently degrading to `U256::MAX`.
    assert!(
        v <= U512::from(U256::MAX),
        "U512 → U256 narrowing overflow (corrupt/synthetic input; \
         spec-bound pool state is unreachable — enforced at register_*_pool)",
    );
    v.to::<U256>()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::solve::mobius_int::floor_jog_bound;

    fn u256(n: u64) -> U256 {
        U256::from(n)
    }

    // ── isqrt_u512 tests ────────────────────────────────────────

    #[test]
    fn test_isqrt_zero() {
        assert_eq!(isqrt_u512(U512::ZERO), U512::ZERO);
    }

    #[test]
    fn test_isqrt_one() {
        assert_eq!(isqrt_u512(U512::from(1u64)), U512::from(1u64));
    }

    #[test]
    fn test_isqrt_perfect_square() {
        // 100 = 10^2
        assert_eq!(isqrt_u512(U512::from(100u64)), U512::from(10u64));
        // 10000 = 100^2
        assert_eq!(isqrt_u512(U512::from(10000u64)), U512::from(100u64));
    }

    #[test]
    fn test_isqrt_non_perfect_square() {
        // floor(sqrt(2)) = 1
        assert_eq!(isqrt_u512(U512::from(2u64)), U512::from(1u64));
        // floor(sqrt(3)) = 1
        assert_eq!(isqrt_u512(U512::from(3u64)), U512::from(1u64));
        // floor(sqrt(8)) = 2
        assert_eq!(isqrt_u512(U512::from(8u64)), U512::from(2u64));
        // floor(sqrt(99)) = 9
        assert_eq!(isqrt_u512(U512::from(99u64)), U512::from(9u64));
    }

    #[test]
    fn test_isqrt_large_perfect_square() {
        // (10^9)^2 = 10^18
        let n = U512::from(1_000_000_000u64) * U512::from(1_000_000_000u64);
        assert_eq!(isqrt_u512(n), U512::from(1_000_000_000u64));
    }

    #[test]
    fn test_isqrt_large_non_perfect_square() {
        // (10^9)^2 - 1 → floor = 10^9 - 1
        let n = U512::from(1_000_000_000u64) * U512::from(1_000_000_000u64) - U512::from(1u64);
        // This is (10^9-1)*(10^9+1) = 10^18 - 1, so isqrt = 999999999
        assert_eq!(isqrt_u512(n), U512::from(999_999_999u64));
    }

    #[test]
    fn test_isqrt_u256_max() {
        // U256::MAX ≈ 1.16e77, sqrt ≈ 1.08e38
        let n = U512::from(U256::MAX);
        let root = isqrt_u512(n);
        // Verify: root^2 <= n < (root+1)^2
        assert!(root * root <= n);
        assert!((root + U512::from(1u64)) * (root + U512::from(1u64)) > n);
    }

    #[test]
    fn test_isqrt_two_pow_256() {
        // 2^256 is a perfect square: sqrt(2^256) = 2^128
        let n = U512::from(1u64) << 256;
        let root = isqrt_u512(n);
        assert_eq!(root, U512::from(1u64) << 128);
    }

    // ── exact_mobius_solve tests ──────────────────────────────────

    #[test]
    fn test_exact_solve_not_profitable() {
        // Same-product pools are never profitable after fees
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(1_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(!result.is_profitable);
        assert!(result.optimal_input.is_zero());
        assert!(result.profit.is_zero());
    }

    #[test]
    fn test_exact_solve_profitable() {
        // Asymmetric reserves where V2 pool 1 has excess output
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(result.is_profitable);
        assert!(!result.optimal_input.is_zero());
        assert!(!result.profit.is_zero());

        // Verify EVM-exact: simulate at optimal_input
        let output = int_simulate_path(result.optimal_input, &hops)
            .unwrap()
            .final_output;
        assert!(output > result.optimal_input);
        assert_eq!(output - result.optimal_input, result.profit);
    }

    #[test]
    fn test_exact_solve_neighborhood_within_floor_jog() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(result.is_profitable);

        // The anchor carries no argmax guarantee, but the COMPUTED floor-jog
        // bound does: no ±3 neighbor may beat the returned profit by more than
        // `mobius_int::floor_jog_bound` allows for these hops — the propagated
        // jog (amplified here by a first-hop rate above 1) plus the floored
        // anchor's plateau slack, never a measured constant.
        let bound = floor_jog_bound(&hops);
        let x_opt = result.optimal_input;
        for delta in -3i64..=3 {
            let candidate = if delta >= 0 {
                x_opt.saturating_add(U256::from(delta.cast_unsigned()))
            } else {
                x_opt.saturating_sub(U256::from((-delta).cast_unsigned()))
            };
            if candidate.is_zero() {
                continue;
            }
            let output = int_simulate_path(candidate, &hops).unwrap().final_output;
            if output > candidate {
                let profit = output - candidate;
                assert!(
                    profit <= result.profit + bound,
                    "Neighbor {candidate} profit {profit} exceeds {} by more than the \
                     computed floor-jog bound {bound}",
                    result.profit
                );
            }
        }
    }

    #[test]
    fn test_exact_solve_empty_hops() {
        let result = exact_mobius_solve(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_exact_solve_single_hop() {
        // Single hop cannot be an arbitrage cycle but K > M is
        // technically possible (γs > r). The solver should produce
        // a valid result since simulate_path works for single hops.
        let hops = vec![IntHopState::new(
            u256(1_000_000),
            u256(2_000_000),
            997,
            1000,
        )];
        let result = exact_mobius_solve(&hops).unwrap();
        // K = 997 * 2_000_000 = 1_994_000_000
        // M = 1000 * 1_000_000 = 1_000_000_000
        // K > M → profitable in the Möbius sense
        assert!(result.is_profitable);
    }

    #[test]
    fn test_exact_solve_three_hop() {
        let hops = vec![
            IntHopState::new(u256(2_000_000), u256(2_100_000), 997, 1000),
            IntHopState::new(u256(2_000_000), u256(2_050_000), 997, 1000),
            IntHopState::new(u256(2_050_000), u256(2_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(result.is_profitable);
        assert!(!result.profit.is_zero());
    }

    // ── Realistic-reserve exact-solve tests ────────────────────

    #[test]
    fn test_exact_vs_f64_realistic_weth_usdc() {
        // USDC (6 decimals) / WETH (18 decimals) pair with realistic reserves
        let usdc_reserves = U256::from(100_000_000_000_000u64); // 100M USDC
        let weth_reserves = U256::from(40_000_000_000_000_000_000u128); // 40K WETH

        let hops = vec![
            IntHopState::new(usdc_reserves, weth_reserves, 997, 1000),
            IntHopState::new(weth_reserves, usdc_reserves, 997, 1000),
        ];

        let exact_result = exact_mobius_solve(&hops).unwrap();

        // Same-product with fees → never profitable
        assert!(!exact_result.is_profitable);
    }

    // ── u512_to_u256 tests ────────────────────────────────────────

    #[test]
    fn test_u512_to_u256_small() {
        let v = U512::from(42u64);
        assert_eq!(narrow_u512_to_u256(v), U256::from(42u64));
    }

    #[test]
    fn test_u512_to_u256_max_u256() {
        let v = U512::from(U256::MAX);
        assert_eq!(narrow_u512_to_u256(v), U256::MAX);
    }

    #[test]
    #[should_panic(expected = "U512 \u{2192} U256 narrowing overflow")]
    fn test_u512_to_u256_overflow() {
        // U256::MAX + 1 overflows U256 — a real pool's reserves/liquidity/
        // sqrtPrice are spec-bound (uint112 / uint128 / MAX_SQRT_RATIO), so
        // this fires only on corrupt/synthetic state. The spec widths are
        // now enforced at `register_v2_pool` / `register_v3_pool` /
        // `register_v4_pool` (see `bot_core/spec_bounds.rs` and ADR-012);
        // on-chain-sourced pool state cannot reach this panic.
        let v = U512::from(U256::MAX) + U512::from(1u64);
        let _ = narrow_u512_to_u256(v);
    }

    // ── Boundary tests ───────────────────────────────────────────

    #[test]
    fn test_exact_solve_vanishing_profit() {
        // K is just barely > M, so the profit is tiny
        // Pool 1: 997 * 1_001 = 997997 vs M = 1000 * 1000 = 1000000
        // K for 2 hops: 997^2 * 1001 * 1001, M: 1000^2 * 1000 * 1000
        // K/M = (997/1000)^2 * (1001/1000)^2 ≈ 0.994009 * 1.002001 ≈ 0.99601
        // Actually K/M < 1, so not profitable. Let me make pools that disagree more.
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(1_010_000), 997, 1000),
            IntHopState::new(u256(1_000_000), u256(1_005_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        // These pools barely disagree — fees may eat all profit
        // Just verify no panic and result is consistent
        if result.is_profitable {
            let output = int_simulate_path(result.optimal_input, &hops)
                .unwrap()
                .final_output;
            assert!(output > result.optimal_input);
            assert_eq!(output - result.optimal_input, result.profit);
        }
    }

    #[test]
    fn test_exact_solve_large_reserves() {
        // Reserves near U256::MAX / 1000 to avoid overflow
        let large = U256::MAX / U256::from(1000u64);
        let hops = vec![
            IntHopState::new(large, large / U256::from(2u64), 997, 1000),
            IntHopState::new(large / U256::from(2u64), large, 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        // Same product with fees → not profitable
        assert!(!result.is_profitable);
    }

    // ── Deep-path discriminant widening ─────────────────────────

    fn pow10(e: u32) -> U256 {
        let mut p = U256::from(1u64);
        for _ in 0..e {
            p *= U256::from(10u64);
        }
        p
    }

    #[test]
    fn test_deep_three_hop_discriminant_does_not_wrap() {
        // 3-hop 1e24: bit_len(K) + bit_len(M) > 512, so the pre-fix U512
        // product wrapped and collapsed the anchor to zero. The widened
        // discriminant must produce a real anchor, and the solver must
        // evaluate it rather than the 1-wei micro-profit fallback.
        let base = pow10(24);
        let hops = vec![
            IntHopState::new(base, base * U256::from(100u64), 9995, 10000),
            IntHopState::new(base, base, 9995, 10000),
            IntHopState::new(base, base, 9995, 10000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        assert!(
            coeffs.K.checked_mul(coeffs.M).is_none(),
            "discriminant must exceed U512"
        );
        assert!(!compute_mobius_model_optimal_input(&coeffs).is_zero());

        let result = exact_mobius_solve(&hops).unwrap();
        // The micro-probe is the only non-anchor path and reports input 1
        // when profitable, so this pins the same fact without a provenance
        // flag.
        assert!(
            result.is_profitable && result.optimal_input != U256::from(1u64),
            "must not enter micro-profit fallback"
        );
        assert!(
            result.optimal_input > pow10(20),
            "must be a real e2x-scale optimum"
        );
        // Profit is verified EVM-exactly at the returned input.
        let output = int_simulate_path(result.optimal_input, &hops)
            .unwrap()
            .final_output;
        assert!(output > result.optimal_input);
        assert_eq!(output - result.optimal_input, result.profit);
    }

    #[test]
    fn test_isqrt_u2048_matches_isqrt_u512_in_domain() {
        for n in [1u64, 2, 3, 99, 100, 1_000_000_007, u64::MAX] {
            let wide = U2048::from(n);
            assert_eq!(u2048_to_u512(isqrt_u2048(wide)), isqrt_u512(U512::from(n)));
        }
        let n = U512::from(U256::MAX);
        assert_eq!(u2048_to_u512(isqrt_u2048(u512_to_u2048(n))), isqrt_u512(n));
    }

    #[test]
    fn isqrt_u2048_is_exact_on_the_upper_type_domain() {
        let ceil_root = (U2048::from(1u64) << 1024) - U2048::from(1u64);
        let ceil_sq = ceil_root * ceil_root;
        assert_eq!(isqrt_u2048(ceil_sq), ceil_root);
        assert_eq!(
            isqrt_u2048(ceil_sq - U2048::from(1u64)),
            ceil_root - U2048::from(1u64)
        );
        assert_eq!(isqrt_u2048(U2048::MAX), ceil_root);
    }
}

// ---------------------------------------------------------------------------
// Staircase displacement: the residual contract
// ---------------------------------------------------------------------------

#[cfg(test)]
mod displacement {
    #![expect(clippy::unwrap_used, clippy::print_stdout)]

    use super::*;
    use crate::solve::mobius_int::floor_jog_bound;

    /// Brute radius of the reference argmax window, in wei — far wider than
    /// any fixed-radius tightening sweep, so the reference is the truth the
    /// anchor-only solve is measured against.
    const WINDOW_RADIUS: u64 = 4096;

    /// Reference discrete argmax over `[anchor - R, anchor + R]` — the
    /// brute-scan truth the solver's residual is measured against.
    fn window_argmax(anchor: U256, radius: u64, hops: &[IntHopState]) -> Option<(U256, U256)> {
        let mut best: Option<(U256, U256)> = None;
        let start = anchor
            .saturating_sub(U256::from(radius))
            .max(U256::from(1u64));
        let end = anchor.saturating_add(U256::from(radius));
        let mut x = start;
        while x <= end {
            if let Ok(sim) = int_simulate_path(x, hops) {
                if sim.final_output > x {
                    let profit = sim.final_output - x;
                    if best.is_none_or(|(_, p)| profit > p) {
                        best = Some((x, profit));
                    }
                }
            }
            if x == U256::MAX {
                break;
            }
            x += U256::from(1u64);
        }
        best
    }

    /// Measure the anchor-only solve's residual against the
    /// ±[`WINDOW_RADIUS`]-wei brute-scan reference and assert it stays within
    /// the COMPUTED [`floor_jog_bound`]. Returns the measured residual and
    /// the bound it was checked against, so a grid can track both; `None` for
    /// a case the solver legitimately skips (not profitable, zero anchor, or
    /// no profitable input in the window).
    fn residual_against_window(hops: &[IntHopState], case: &str) -> Option<(U256, U256)> {
        let coeffs = compute_int_mobius_coefficients(hops).ok()?;
        if !coeffs.is_profitable {
            return None;
        }
        let anchor = compute_mobius_model_optimal_input(&coeffs);
        if anchor.is_zero() {
            return None;
        }
        let (_, window_profit) = window_argmax(anchor, WINDOW_RADIUS, hops)?;
        let solved = exact_mobius_solve(hops).unwrap();
        let residual = window_profit.saturating_sub(solved.profit);
        let bound = floor_jog_bound(hops);
        assert!(
            residual <= bound,
            "residual {residual} > floor_jog_bound {bound} ({case} hops={} anchor={anchor} \
             window={window_profit} solver={})",
            hops.len(),
            solved.profit
        );
        Some((residual, bound))
    }

    /// Near-fee-threshold margins flatten the envelope near the optimum, so
    /// floor jogs dominate and the true discrete argmax wanders far from the
    /// model anchor (the adversarial-review counterexample class). The
    /// residual — the solver's profit shortfall against a ±4096-wei brute-scan
    /// reference — must stay within the COMPUTED [`floor_jog_bound`], never a
    /// measured constant: on these thin-margin ladders the bound is the
    /// one-wei-per-hop count plus one wei of floored-anchor slack (every hop
    /// rate sits at or under 1 except the diverging hop's, whose excess is a
    /// few thousandths of a wei per wei).
    ///
    /// Measured maxima on this grid (re-print by running this test):
    ///
    /// | hops | `floor_jog_bound` | max residual |
    /// |------|-------------------|--------------|
    /// | 2    | 3 wei             | 1 wei        |
    /// | 3    | 4 wei             | 2 wei        |
    #[test]
    fn displacement_residual_stays_within_floor_jog() {
        let fees = [(997u64, 1000u64), (9975, 10000), (9995, 10000)];
        let bases = [
            1_000_000_000_000u128,
            1_000_000_000_000_000_000u128,
            10u128.pow(24),
        ];
        // Indexed by hop count, so a deeper ladder can never silently borrow a
        // shallower ladder's measurement.
        let mut max_residual = [U256::ZERO; 4];
        let mut min_bound = [U256::MAX; 4];
        let mut measured = [0usize; 4];
        for &(g, d) in &fees {
            for &base in &bases {
                let b = U256::from(base);
                // 2-hop ladder: one diverging hop + one neutral hop.
                for delta_ppm in (6100u128..=20_000).step_by(900) {
                    let s1 = b + b * U256::from(delta_ppm) / U256::from(1_000_000u64);
                    let hops = vec![IntHopState::new(b, s1, g, d), IntHopState::new(b, b, g, d)];
                    let case = format!("2-hop g={g}/{d} base={base} dppm={delta_ppm}");
                    if let Some((residual, bound)) = residual_against_window(&hops, &case) {
                        measured[hops.len()] += 1;
                        max_residual[hops.len()] = max_residual[hops.len()].max(residual);
                        min_bound[hops.len()] = min_bound[hops.len()].min(bound);
                    }
                }
                // 3-hop ladder: `hop3`-shaped (one diverging hop, two neutral
                // hops) with the break-even margin scaled by the third fee, so
                // the ladder walks the same near-threshold band one hop deeper.
                let breakeven_ppm = 1_000_000u128 * u128::from(d).pow(3) / u128::from(g).pow(3);
                for delta_ppm in (breakeven_ppm + 600..=breakeven_ppm + 15_000).step_by(900) {
                    let s1 = b + b * U256::from(delta_ppm) / U256::from(1_000_000u64);
                    let hops = vec![
                        IntHopState::new(b, s1, g, d),
                        IntHopState::new(b, b, g, d),
                        IntHopState::new(b, b, g, d),
                    ];
                    let case = format!("3-hop g={g}/{d} base={base} dppm={delta_ppm}");
                    if let Some((residual, bound)) = residual_against_window(&hops, &case) {
                        measured[hops.len()] += 1;
                        max_residual[hops.len()] = max_residual[hops.len()].max(residual);
                        min_bound[hops.len()] = min_bound[hops.len()].min(bound);
                    }
                }
            }
        }
        // The grid's coverage and its measured maxima — the numbers the module
        // doc and the test doc cite, printed rather than pinned so a re-run
        // re-measures them instead of trusting a stale constant.
        println!(
            "grid: 2-hop {} cases max residual {} wei (min bound {}); \
             3-hop {} cases max residual {} wei (min bound {})",
            measured[2], max_residual[2], min_bound[2], measured[3], max_residual[3], min_bound[3],
        );
        for hops in [2usize, 3] {
            assert!(
                measured[hops] > 0,
                "the {hops}-hop ladder must produce measurable, profitable cases"
            );
            // The tightest bound the grid produced still covers the worst
            // residual it measured — the contract, over the whole family.
            assert!(
                max_residual[hops] <= min_bound[hops],
                "{hops}-hop residual {} exceeds the tightest bound {}",
                max_residual[hops],
                min_bound[hops]
            );
        }
    }

    /// Pin the ill-conditioning itself (re-derived counterexample of the
    /// adversarial-review class): the true argmax sits more than 64 wei from
    /// the anchor — outside any small fixed-radius sweep — while the profit
    /// plateau is flat, so the solver's profit still lands within the computed
    /// floor-jog bound of the window max.
    #[test]
    fn argmax_displacement_exceeds_fixed_sweeps_but_residual_is_bounded() {
        let b = U256::from(1_000_000_000_000u128);
        let s1 = b + b * U256::from(9_700u64) / U256::from(1_000_000u64);
        let hops = vec![
            IntHopState::new(b, s1, 997, 1000),
            IntHopState::new(b, b, 997, 1000),
        ];
        let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
        let anchor = compute_mobius_model_optimal_input(&coeffs);
        let (wx, _) = window_argmax(anchor, WINDOW_RADIUS, &hops).unwrap();
        let disp = if wx > anchor {
            wx - anchor
        } else {
            anchor - wx
        };
        assert!(
            disp > U256::from(64u64),
            "counterexample must keep the argmax outside small sweeps (disp={disp})"
        );
        // Two hops' jitter plus the floored anchor's one wei of plateau slope:
        // the bound is computed from the hop list, not a measured residual.
        assert_eq!(floor_jog_bound(&hops), U256::from(3u64));
        assert!(
            residual_against_window(&hops, "argmax-displacement pin").is_some(),
            "the pin must be a profitable, anchored case"
        );
    }
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    #![expect(clippy::unwrap_used, clippy::tuple_array_conversions)]

    use super::*;
    use proptest::prelude::*;

    /// Generate a U256 value by filling from 4 u64 limbs.
    fn arb_u256() -> impl Strategy<Value = U256> {
        (any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>())
            .prop_map(|(a, b, c, d)| U256::from_limbs([a, b, c, d]))
    }

    proptest! {
        /// isqrt(n)^2 <= n < (isqrt(n)+1)^2 for all n
        #[test]
        fn isqrt_is_floor_sqrt(n in arb_u256()) {
            let n_u512 = U512::from(n);
            let root = isqrt_u512(n_u512);
            let root_sq = root * root;
            let root_plus_one = root + U512::from(1u64);
            let rp1_sq = root_plus_one * root_plus_one;

            assert!(
                root_sq <= n_u512,
                "isqrt({n_u512}) = {root}, but {root}^2 = {root_sq} > {n_u512}"
            );
            assert!(
                rp1_sq > n_u512,
                "isqrt({n_u512}) = {root}, but ({root}+1)^2 = {rp1_sq} <= {n_u512}"
            );
        }

        /// exact_mobius_solve never panics, and if profitable, the profit
        /// is verified by EVM-exact simulation.
        #[test]
        fn exact_solve_never_panics(
            r1_lo in 1u64..u64::MAX,
            s1_lo in 1u64..u64::MAX,
            r2_lo in 1u64..u64::MAX,
            s2_lo in 1u64..u64::MAX,
        ) {
            let hops = vec![
                IntHopState::new(U256::from(r1_lo), U256::from(s1_lo), 997, 1000),
                IntHopState::new(U256::from(r2_lo), U256::from(s2_lo), 997, 1000),
            ];
            let result = exact_mobius_solve(&hops).unwrap();
            if result.is_profitable {
                let output = int_simulate_path(result.optimal_input, &hops).unwrap().final_output;
                assert!(output > result.optimal_input);
                assert_eq!(output - result.optimal_input, result.profit);
            }
        }
    }

    // ── Per-hop output tests ──────────────────────────────────────

    fn u256_hop(n: u64) -> U256 {
        U256::from(n)
    }

    #[test]
    fn test_exact_mobius_solve_hop_outputs_profitable() {
        let hops = vec![
            IntHopState::new(u256_hop(1_000_000), u256_hop(5_000_000), 997, 1000),
            IntHopState::new(u256_hop(1_500_000), u256_hop(3_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(result.is_profitable);

        // 2 hops → 2 hop_outputs
        assert_eq!(result.hop_outputs.len(), 2);

        // Invariant: hop_outputs[0] = hops[0].swap(optimal_input)
        let expected_hop1 = hops[0].swap(result.optimal_input).unwrap();
        assert_eq!(result.hop_outputs[0], expected_hop1);

        // Invariant: hop_outputs[1] = hops[1].swap(hop_outputs[0])
        let expected_hop2 = hops[1].swap(expected_hop1).unwrap();
        assert_eq!(result.hop_outputs[1], expected_hop2);

        // Invariant: profit = final_output - optimal_input
        assert_eq!(result.hop_outputs[1] - result.optimal_input, result.profit);
    }

    #[test]
    fn test_exact_mobius_solve_hop_outputs_not_profitable() {
        // Same-product pools → not profitable → empty hop_outputs
        let hops = vec![
            IntHopState::new(u256_hop(1_000_000), u256_hop(1_000_000), 997, 1000),
            IntHopState::new(u256_hop(1_000_000), u256_hop(1_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(!result.is_profitable);
        assert!(result.hop_outputs.is_empty());
    }
}
