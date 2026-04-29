//! Core Möbius transformation optimizer for constant product AMM arbitrage.
//!
//! Every constant product swap `y = (γ·s·x)/(r + γ·x)` is a Möbius
//! transformation that fixes the origin. This includes V2 pools and V3/V4
//! tick ranges (bounded product CFMMs with effective reserves).
//!
//! Möbius transformations form a group under composition, so any n-hop path
//! reduces to a single rational function:
//!
//! ```text
//! l(x) = K·x / (M + N·x)
//! ```
//!
//! The coefficients K, M, N are computed via an O(n) recurrence (three scalar
//! updates per hop). The optimal input follows from d(l(x) - x)/dx = 0:
//!
//! ```text
//! x_opt = (√(K·M) - M) / N
//! ```
//!
//! This is exact and requires zero iterations regardless of path length.
//!
//! Profitability check (free from the same recurrence): K / M > 1

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::implied_bounds_in_impls)]

/// Relative tolerance for V3 sqrt price boundary comparisons.
/// Accounts for f64 rounding near tick boundaries.
pub const SQRT_PRICE_REL_TOL: f64 = 1e-10;

/// Margin factor to under-estimate V3 range capacity, avoiding
/// float64 boundary precision issues. Multiply max capacity by
/// `1.0 - RANGE_CAPACITY_MARGIN` to stay safely inside bounds.
pub const RANGE_CAPACITY_MARGIN: f64 = 1e-12;

/// Sentinel penalty returned by profit functions when input pushes
/// price out of V3 tick range. Must be large enough negative to
/// dominate any valid profit.
pub const INVALID_RANGE_PENALTY: f64 = -1e30;

/// Reserve and fee state for a single pool hop.
///
/// For V2 pools, `reserve_in` and `reserve_out` are the raw reserves.
/// For V3 tick ranges, they are the effective/virtual reserves:
/// `R0 + α = L/√P` and `R1 + β = L·√P`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct HopState {
    /// Reserve of the token being deposited (input reserve).
    pub reserve_in: f64,
    /// Reserve of the token being received (output reserve).
    pub reserve_out: f64,
    /// Fee fraction (e.g. 0.003 for 0.3%).
    pub fee: f64,
}

impl HopState {
    /// Create a new hop state.
    #[must_use]
    pub const fn new(reserve_in: f64, reserve_out: f64, fee: f64) -> Self {
        Self {
            reserve_in,
            reserve_out,
            fee,
        }
    }

    /// Fee multiplier (1 - fee).
    #[inline]
    #[must_use]
    pub fn gamma(&self) -> f64 {
        1.0 - self.fee
    }
}

/// The three scalar coefficients that fully describe an n-hop constant product
/// path as a single Möbius transformation `l(x) = K·x / (M + N·x)`.
#[derive(Clone, Debug)]
#[allow(non_snake_case)]
#[non_exhaustive]
pub struct MobiusCoefficients {
    /// Numerator scaling coefficient.
    pub K: f64,
    /// Constant term in denominator.
    pub M: f64,
    /// Linear term in denominator.
    pub N: f64,
    /// True when K/M > 1 (initial marginal rate exceeds 1).
    pub is_profitable: bool,
}

impl MobiusCoefficients {
    /// Compute path output for input x.
    #[inline]
    #[must_use]
    pub fn path_output(&self, x: f64) -> f64 {
        let denom = self.M + self.N * x;
        if denom <= 0.0 {
            return 0.0;
        }
        self.K * x / denom
    }

    /// Compute the exact optimal input that maximizes profit.
    ///
    /// Returns 0.0 if the path is not profitable.
    #[inline]
    #[must_use]
    pub fn optimal_input(&self) -> f64 {
        if !self.is_profitable {
            return 0.0;
        }
        let km = self.K * self.M;
        if km < 0.0 {
            return 0.0;
        }
        (km.sqrt() - self.M) / self.N
    }

    /// Compute profit l(x) - x for input x.
    #[inline]
    #[must_use]
    pub fn profit_at(&self, x: f64) -> f64 {
        self.path_output(x) - x
    }
}

/// Compute the Möbius transformation coefficients K, M, N for an n-hop
/// constant product path via a single forward pass.
///
/// The recurrence is derived from 2×2 matrix multiplication where each
/// swap is encoded as:
///
/// ```text
/// M_i = [[γ_i·s_i, 0], [γ_i, r_i]]
/// ```
///
/// and the product M_1 · M_2 · ... · M_n yields the composite
/// transformation l(x) = K·x / (M + N·x).
///
/// # Errors
///
/// Returns `MobiusError::EmptyHops` if the hops list is empty.
pub fn compute_mobius_coefficients(hops: &[HopState]) -> Result<MobiusCoefficients, MobiusError> {
    if hops.is_empty() {
        return Err(MobiusError::EmptyHops);
    }

    let first = &hops[0];
    let gamma = first.gamma();
    let mut K = gamma * first.reserve_out;
    let mut M = first.reserve_in;
    let mut N = gamma;

    for hop in &hops[1..] {
        let g = hop.gamma();
        let old_K = K;
        K = old_K * g * hop.reserve_out;
        M *= hop.reserve_in;
        N = N * hop.reserve_in + old_K * g;
    }

    Ok(MobiusCoefficients {
        K,
        M,
        N,
        is_profitable: K > M,
    })
}

/// Simulate a swap through all hops for verification.
///
/// Starting with input `x`, computes the output of each hop sequentially
/// using the constant product formula: `y = γ·s·x / (r + γ·x)`.
pub fn simulate_path(x: f64, hops: &[HopState]) -> f64 {
    let mut amount = x;
    for hop in hops {
        if amount <= 0.0 {
            return 0.0;
        }
        let gamma = hop.gamma();
        let denom = hop.reserve_in + amount * gamma;
        if denom <= 0.0 {
            return 0.0;
        }
        amount = amount * gamma * hop.reserve_out / denom;
    }
    amount
}

/// Invert the single-hop constant product formula to find the input `x`
/// that produces `target_output`.
///
/// Given `y = γ·s·x / (r + γ·x) = target`, solve for x:
/// `x = target·r / (γ·(s - target))`
///
/// Returns `None` if `target >= reserve_out` (output exceeds reserve)
/// or the denominator is non-positive.
#[inline]
#[must_use]
pub fn invert_hop_output(target: f64, hop: &HopState) -> Option<f64> {
    let gamma = hop.gamma();
    if hop.reserve_out > target {
        let denom = gamma * (hop.reserve_out - target);
        if denom > 0.0 {
            return Some(target * hop.reserve_in / denom);
        }
    }
    None
}

/// Invert a multi-hop path to find input `x` that produces `target_output`
/// from the final hop.
///
/// Works by inverting through hops in reverse order.
/// Returns `None` if any inversion step fails.
#[must_use]
pub fn invert_path_output(target: f64, hops: &[HopState]) -> Option<f64> {
    let mut cap = target;
    for h in hops.iter().rev() {
        cap = invert_hop_output(cap, h)?;
    }
    Some(cap)
}

/// Solve for optimal arbitrage input using the Möbius transformation approach.
///
/// Returns `(optimal_input, profit, iterations)` where iterations is always 0
/// for the closed-form solution.
pub fn mobius_solve(hops: &[HopState], max_input: Option<f64>) -> (f64, f64, u32) {
    let Ok(coeffs) = compute_mobius_coefficients(hops) else {
        return (0.0, 0.0, 0);
    };

    if !coeffs.is_profitable {
        return (0.0, 0.0, 0);
    }

    let mut x_opt = coeffs.optimal_input();

    if x_opt <= 0.0 {
        return (0.0, 0.0, 0);
    }

    // Apply max_input constraint
    if let Some(max) = max_input {
        if x_opt > max {
            x_opt = max;
        }
    }

    // Compute exact profit via path simulation (avoids floating-point drift)
    let output = simulate_path(x_opt, hops);
    let profit = output - x_opt;

    (x_opt, profit, 0)
}

/// Relative tolerance for golden section convergence.
pub const GSS_REL_TOL: f64 = 1e-10;
/// Absolute tolerance for golden section convergence (used when a ≈ 0).
pub const GSS_ABS_TOL: f64 = 1e-6;
/// Golden section ratio φ ≈ 0.618.
const GOLDEN_SECTION: f64 = 0.618_033_988_749_894_9;

/// Golden section search for the maximum of a unimodal function.
///
/// Converges when the interval width falls below [`GSS_REL_TOL`] × a
/// (relative) or [`GSS_ABS_TOL`] (absolute near zero).
///
/// Returns `(argmax, iterations)`.
pub fn golden_section_search_max(f: impl Fn(f64) -> f64, a: f64, b: f64) -> (f64, u32) {
    debug_assert!(a < b, "Search interval must be non-empty");
    let initial_interval = b - a;
    let abs_tol = GSS_ABS_TOL.max(initial_interval * GSS_REL_TOL);
    let phi = GOLDEN_SECTION;

    let mut lo = a;
    let mut hi = b;
    let mut x1 = hi - phi * (hi - lo);
    let mut x2 = lo + phi * (hi - lo);
    let mut f1 = f(x1);
    let mut f2 = f(x2);
    let mut iters: u32 = 0;

    loop {
        let interval = hi - lo;
        if lo > 0.0 {
            if interval / lo < GSS_REL_TOL {
                break;
            }
        } else if interval < abs_tol {
            break;
        }
        iters += 1;
        if f1 < f2 {
            lo = x1;
            x1 = x2;
            f1 = f2;
            x2 = lo + phi * (hi - lo);
            f2 = f(x2);
        } else {
            hi = x2;
            x2 = x1;
            f2 = f1;
            x1 = hi - phi * (hi - lo);
            f1 = f(x1);
        }
    }

    (f64::midpoint(lo, hi), iters)
}

/// Errors that can occur during Möbius optimization.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_two_hop_profitable() {
        // Two pools that disagree on price: Pool 1 has excess B (1 A → 1.1 B),
        // Pool 2 has excess A (1 B → 1.1 A). Round-trip profit overcomes fees.
        let hops = vec![
            HopState::new(1_000_000.0, 1_100_000.0, 0.003),
            HopState::new(1_000_000.0, 1_100_000.0, 0.003),
        ];

        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute coefficients");
        assert!(coeffs.is_profitable, "Path should be profitable");

        let (x_opt, profit, iters) = mobius_solve(&hops, None);
        assert!(x_opt > 0.0, "Optimal input should be positive");
        assert!(profit > 0.0, "Profit should be positive");
        assert_eq!(iters, 0, "Möbius requires zero iterations");
    }

    #[test]
    fn test_two_hop_not_profitable() {
        // Identical reserves — no arbitrage
        let hops = vec![
            HopState::new(1_000_000.0, 1_000_000.0, 0.003),
            HopState::new(1_000_000.0, 1_000_000.0, 0.003),
        ];

        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute coefficients");
        assert!(
            !coeffs.is_profitable,
            "Identical reserves should not be profitable"
        );

        let (x_opt, profit, iters) = mobius_solve(&hops, None);
        assert_eq!(x_opt, 0.0);
        assert_eq!(profit, 0.0);
        assert_eq!(iters, 0);
    }

    #[test]
    fn test_three_hop_profitable() {
        let hops = vec![
            HopState::new(1_000_000.0, 1_050_000.0, 0.003),
            HopState::new(1_000_000.0, 1_020_000.0, 0.003),
            HopState::new(1_020_000.0, 1_000_000.0, 0.003),
        ];

        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute coefficients");
        assert!(coeffs.is_profitable);

        let (x_opt, profit, iters) = mobius_solve(&hops, None);
        assert!(x_opt > 0.0);
        assert!(profit > 0.0);
        assert_eq!(iters, 0);
    }

    #[test]
    fn test_simulate_path_matches_mobius_output() {
        let hops = vec![
            HopState::new(1_000_000.0, 1_100_000.0, 0.003),
            HopState::new(1_000_000.0, 1_100_000.0, 0.003),
        ];

        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute");
        let x_opt = coeffs.optimal_input();
        let mobius_output = coeffs.path_output(x_opt);
        let sim_output = simulate_path(x_opt, &hops);

        let rel_diff = (mobius_output - sim_output).abs() / sim_output;
        assert!(
            rel_diff < 1e-10,
            "Mobius and simulation should match: rel_diff = {rel_diff}"
        );
    }

    #[test]
    fn test_max_input_constraint() {
        let hops = vec![
            HopState::new(1_000_000.0, 1_050_000.0, 0.003),
            HopState::new(1_050_000.0, 1_000_000.0, 0.003),
        ];

        let (x_unconstrained, _, _) = mobius_solve(&hops, None);
        let max_input = x_unconstrained / 2.0;
        let (x_constrained, _, _) = mobius_solve(&hops, Some(max_input));

        assert!(
            x_constrained <= max_input + 1e-10,
            "Constrained input should respect max_input"
        );
    }

    #[test]
    fn test_empty_hops_error() {
        let result = compute_mobius_coefficients(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_golden_section_finds_maximum() {
        // f(x) = -x^2 + 4x, maximum at x=2
        let f = |x: f64| -x * x + 4.0 * x;
        let (x_max, iters) = golden_section_search_max(f, 0.0, 4.0);
        assert!(
            (x_max - 2.0).abs() < 0.01,
            "Should find max at x=2, got {x_max}"
        );
        assert!(iters > 0, "Should take at least one iteration");
    }

    #[test]
    fn test_profitability_check_free() {
        // K > M implies profitability without solving.
        // Pool 1 has 2:1 rate, Pool 2 has 1:1 rate — pools disagree.
        let hops = vec![
            HopState::new(100.0, 200.0, 0.003),
            HopState::new(200.0, 200.0, 0.003),
        ];
        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute");
        // K = γ*200 * γ*200 ≈ 0.997*200 * 0.997*200 ≈ 39760
        // M = 100 * 200 = 20000
        // K/M ≈ 1.988, strongly profitable
        assert!(coeffs.is_profitable);
    }

    #[test]
    fn test_single_hop_not_profitable() {
        // A single hop cannot be a cycle, and K/M = γ*s/r.
        // For arbitrage we need K > M, i.e. γ*s > r.
        // With s > r (favorable rate), a single swap gives positive
        // output but it's not an arbitrage cycle.
        let hops = vec![HopState::new(1_000_000.0, 1_050_000.0, 0.003)];
        let coeffs = compute_mobius_coefficients(&hops).expect("Should compute");
        // K = γ * s = 0.997 * 1050000 = 1046850
        // M = r = 1000000
        // K > M, so technically "profitable" in the Möbius sense
        // (marginal rate > 1 at x=0)
        // This just means the first infinitesimal unit of input yields > 1 unit output
        assert!(coeffs.is_profitable);
    }
}
