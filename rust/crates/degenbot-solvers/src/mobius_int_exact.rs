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
//! - [`exact_mobius_solve`] is exact at the **discrete EVM-simulation
//!   layer** — the actual on-chain chain applies floor (`EVM DIV`) at every
//!   hop, producing a piecewise-linear staircase that *approximates* that
//!   rational envelope. The discrete optimum can sit at `x* + δ` for small
//!   `|δ|` because the staircase is locally sub-constant-slope near `x*`.
//!   [`exact_mobius_solve`] calls [`compute_mobius_model_optimal_input`] for
//!   the model-optimum anchor, then runs `int_simulate_path` at `x*` and
//!   `x* ± 2` (the floor-exact swap chain) to find the discrete argmax of
//!   `int_simulate_path(x) - x` — the true on-chain profit optimum.
//!
//! The closed form and the ±2 search compose into a single EVM-exact result:
//! the closed form narrows the search to a tight neighborhood; the ±2 sweep
//! patches the staircase jog that the rational envelope smooths over.
//!
//! Module-level qualifier: the "exact" in *this module's name*
//! (`mobius_int_exact`) is a discipline label distinguishing the integer
//! pathway (no floats — pure `U512` + `isqrt_u512` + floor-division; contrast
//! with the now-deleted f64 recurrence module). The per-function rename above
//! keeps that discipline label while making the layer of exactness explicit
//! at each per-candidate symbol.

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

use crate::mobius_int::{compute_int_mobius_coefficients, IntMobiusCoefficients, MobiusError};
use degenbot_v2_math::{int_simulate_path, IntHopState};

/// Result of [`exact_mobius_solve`] — the discrete-EVM-sim-exact optimum of
/// the on-chain swap chain (NOT the closed-form Möbius-model optimum).
///
/// The discrete optimum is found by anchoring at the model-optimum `x* =
/// (√(K·M) - M) / N` (see [`compute_mobius_model_optimal_input`]) and
/// sweeping the floor-exact swap chain `int_simulate_path` at `x*` and `x* ±
/// 2`. The model envelope smooths over the per-hop floor-division staircase
/// jog; the ±2 sweep patches it. See the module-level doc for the two-layer
/// discipline ("model layer" vs "discrete-EVM-sim layer").
///
/// `optimal_input` is the discrete argmax of `int_simulate_path(x) - x`;
/// `profit` is the corresponding on-chain profit. Both are `U256` (zero when
/// unprofitable).
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
    /// Whether the closed-form solution was used (as opposed to bounded search).
    pub used_closed_form: bool,
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
/// 4. EVM-simulate at `x*` and nearby points (±2) via `int_simulate_path`
///    to find the discrete argmax of `int_simulate_path(x) - x` — the true
///    on-chain profit optimum (the model envelope smooths over per-hop
///    floor-division staircase jog; the ±2 sweep patches that jog).
/// 5. Return best result.
///
/// This produces **discrete-EVM-sim-exact** results with zero float
/// conversions: the closed form narrows the search to a tight neighborhood
/// of the model-optimum anchor; the ±2 sweep picks the discrete optimum
/// the staircase actually attains.
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
            used_closed_form: false,
            hop_outputs: Vec::new(),
        });
    }

    let x_approx = compute_mobius_model_optimal_input(&coeffs);

    if x_approx.is_zero() {
        // Even though K > M, the integer square root may give x* = 0
        // when the profit is vanishingly small (K/M ≈ 1).
        // Try a small input to detect micro-profits.
        let sim = int_simulate_path(U256::from(1u64), hops);
        if let Ok(sim) = sim {
            if sim.final_output > U256::from(1u64) {
                return Ok(ExactMobiusResult {
                    optimal_input: U256::from(1u64),
                    profit: sim.final_output - U256::from(1u64),
                    is_profitable: true,
                    used_closed_form: false,
                    hop_outputs: sim.hop_outputs,
                });
            }
        }
        return Ok(ExactMobiusResult {
            optimal_input: U256::ZERO,
            profit: U256::ZERO,
            is_profitable: false,
            used_closed_form: false,
            hop_outputs: Vec::new(),
        });
    }

    // Search ±2 around x_approx for integer rounding
    let mut best_x = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut best_hop_outputs: Vec<U256> = Vec::new();

    for delta in -2i32..=2 {
        let candidate = if delta >= 0 {
            x_approx.saturating_add(U256::from(delta.cast_unsigned()))
        } else {
            x_approx.saturating_sub(U256::from((-delta).cast_unsigned()))
        };

        if candidate.is_zero() {
            continue;
        }

        let Ok(sim) = int_simulate_path(candidate, hops) else {
            // Candidate's swap reverts on-chain (uint256 overflow) →
            // not a viable arb input; skip.
            continue;
        };

        if sim.final_output > candidate {
            let profit = sim.final_output - candidate;
            if profit > best_profit {
                best_profit = profit;
                best_x = candidate;
                best_hop_outputs = sim.hop_outputs;
            }
        }
    }

    Ok(ExactMobiusResult {
        optimal_input: best_x,
        profit: best_profit,
        is_profitable: !best_profit.is_zero(),
        used_closed_form: true,
        hop_outputs: best_hop_outputs,
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
/// reals — NOT the discrete optimum of the actual swap chain. The model
/// envelope treats the per-hop swap `y = γ·s·x / (D·r + γ·x)` as if its
/// floor division were mathematical division; the actual on-chain chain
/// applies floor (`EVM DIV`) at each hop, producing a piecewise-linear
/// staircase that *approximates* this rational envelope. The discrete
/// optimum can sit at `x* + δ` for small `|δ|` because the staircase is
/// locally sub-constant-slope near `x*`.
///
/// [`exact_mobius_solve`] bridges the two layers: it calls this fn for the
/// model-optimum anchor, then runs the **floor-exact EVM-simulation sweep**
/// `int_simulate_path(x*)` at `x*` and `x* ± 2` to find the discrete argmax
/// of `int_simulate_path(x) - x` — the true on-chain profit optimum.
///
/// # Arithmetic discipline
///
/// All arithmetic is `U512`. The integer square root is exact floor
/// `isqrt_u512`. Floor division matches `EVM DIV` semantics.
///
/// This function is also used by the mixed V2-V3 integer solver
/// (`mobius_v3_int::exact_solve_mixed_v2_v3_sequence`) and the
/// Solidly-bracketed dispatcher
/// (`uniswap_engine::solver_dispatch::solve_solidly`), each of which uses the
/// model-optimum output as an anchor for its own discrete search
/// (golden-section for the latter, ±[1,N] EVM-sim sweep for the mixed path).
pub fn compute_mobius_model_optimal_input(coeffs: &IntMobiusCoefficients) -> U256 {
    // K * M fits in U512 (each is at most U512)
    let km = coeffs.K * coeffs.M;

    // Integer square root of K*M
    let sqrt_km = isqrt_u512(km);

    // sqrt(K*M) - M
    // If M > sqrt(K*M), this underflows — but that can't happen when K > M
    // because sqrt(K*M) >= sqrt(M*M) = M.
    let numerator = if sqrt_km >= coeffs.M {
        sqrt_km - coeffs.M
    } else {
        // K > M but sqrt(K*M) < M due to integer truncation.
        // This means K is only slightly larger than M — profit is vanishingly small.
        return U256::ZERO;
    };

    // (sqrt(K*M) - M) / N
    if coeffs.N.is_zero() {
        return U256::ZERO;
    }
    let x_u512 = numerator / coeffs.N;

    // Truncate U512 → U256
    u512_to_u256_internal(x_u512)
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

/// Convert U512 to U256, returning U256::ZERO if the value overflows.
///
/// This is `pub` so that `mobius_v3_int` can use it too.
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
/// CL computations; panicking here makes the contract invariant explicit.
/// The spec widths are enforced at `register_v2_pool` /
/// `register_v3_pool` / `register_v4_pool` (see `spec_bounds.rs` and
/// ADR-012), so on-chain-sourced pool state cannot reach this branch.
pub fn u512_to_u256_internal(v: U512) -> U256 {
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
    fn test_exact_solve_best_in_neighborhood() {
        let hops = vec![
            IntHopState::new(u256(1_000_000), u256(5_000_000), 997, 1000),
            IntHopState::new(u256(1_500_000), u256(3_000_000), 997, 1000),
        ];
        let result = exact_mobius_solve(&hops).unwrap();
        assert!(result.is_profitable);

        let best_pft = result.profit;

        // Check that no ±3 neighbor has better profit
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
                    profit <= result.profit,
                    "Neighbor {candidate} has profit {profit} > best {best_pft}"
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
        assert_eq!(u512_to_u256_internal(v), U256::from(42u64));
    }

    #[test]
    fn test_u512_to_u256_max_u256() {
        let v = U512::from(U256::MAX);
        assert_eq!(u512_to_u256_internal(v), U256::MAX);
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
        let _ = u512_to_u256_internal(v);
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
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::tuple_array_conversions
    )]

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
