//! Discriminant-domain characterization for the integer Möbius closed form.
//!
//! `compute_mobius_model_optimal_input` forms `K * M` to feed an integer
//! square root. The product needs `bit_len(K) + bit_len(M)` bits; when that
//! exceeds 512 the `U512` multiply wraps modulo 2^512 (ruint's `Mul` is
//! `wrapping_mul` — it never panics, in any profile). The wrapped product is
//! smaller than the true `K*M`, so its floor square root can fall below `M`;
//! the guard in `compute_mobius_model_optimal_input` then collapses the model
//! anchor to zero, and `exact_mobius_solve` takes its 1-wei micro-profit
//! fallback and reports `optimal_input = 1` as a plausible optimum.
//!
//! This file pins the domain boundary on the all-V2 synthetic grid from
//! `examples/all_v2_fastpath_ab.rs`, verifies that the widened (U2048)
//! closed form reproduces the unified walk on the diverging 3-hop paths, and
//! checks bit-identity with the pre-fix formula everywhere the U512
//! discriminant does not wrap.

#![expect(clippy::unwrap_used, clippy::many_single_char_names)]

use alloy::primitives::U256;
use degenbot_math::v2::{int_simulate_path, IntHopState};
use degenbot_solvers::cl::{solve_mixed_piecewise, ClSolveTables, IntV3TickRangeSequence};
use degenbot_solvers::mobius_int::{compute_int_mobius_coefficients, IntMobiusCoefficients};
use degenbot_solvers::mobius_int_exact::{
    compute_mobius_model_optimal_input, exact_mobius_solve, isqrt_u512, u512_to_u256_internal,
};
use degenbot_solvers::runtime::SolveRuntimeConfig;

/// The exact closed-form discriminant is trustworthy only when the U512
/// product does not wrap. This is the code-checkable precondition over the
/// composed coefficients.
fn u512_product_fits(coeffs: &IntMobiusCoefficients) -> bool {
    coeffs.K.checked_mul(coeffs.M).is_some()
}

/// The pre-fix closed form verbatim: a U512 discriminant with wrapping `Mul`.
/// Meaningful as a reference only inside [`u512_product_fits`], where the old
/// multiply was exact.
fn prefix_model_anchor(coeffs: &IntMobiusCoefficients) -> U256 {
    let km = coeffs.K * coeffs.M;
    let sqrt_km = isqrt_u512(km);
    let numerator = if sqrt_km >= coeffs.M {
        sqrt_km - coeffs.M
    } else {
        return U256::ZERO;
    };
    if coeffs.N.is_zero() {
        return U256::ZERO;
    }
    u512_to_u256_internal(numerator / coeffs.N)
}

type Solve = Option<(U256, U256, Vec<U256>)>;

fn baseline(hops: &[IntHopState]) -> Solve {
    let r = exact_mobius_solve(hops).ok()?;
    if !r.is_profitable || r.optimal_input.is_zero() || r.profit.is_zero() {
        return None;
    }
    Some((r.optimal_input, r.profit, r.hop_outputs))
}

fn walk(hops: &[IntHopState]) -> Solve {
    let v2: Vec<Option<IntHopState>> = hops.iter().cloned().map(Some).collect();
    let seqs: Vec<Option<&IntV3TickRangeSequence>> = (0..hops.len()).map(|_| None).collect();
    let prepared: Vec<Option<ClSolveTables>> = (0..hops.len()).map(|_| None).collect();
    let order: Vec<bool> = (0..hops.len()).map(|_| true).collect();
    solve_mixed_piecewise(
        &v2,
        &seqs,
        &prepared,
        &order,
        &SolveRuntimeConfig::default(),
        None,
    )
    .result
}

/// `(opt, profit, outputs)` from sweeping `int_simulate_path` at `anchor - 2
/// ..= anchor + 2`, mirroring the `exact_mobius_solve` neighborhood.
fn sweep(anchor: U256, hops: &[IntHopState]) -> Solve {
    let mut best: Solve = None;
    for delta in -2i32..=2 {
        let candidate = if delta >= 0 {
            anchor.saturating_add(U256::from(delta.cast_unsigned()))
        } else {
            anchor.saturating_sub(U256::from((-delta).cast_unsigned()))
        };
        if candidate.is_zero() {
            continue;
        }
        let Ok(sim) = int_simulate_path(candidate, hops) else {
            continue;
        };
        if sim.final_output > candidate {
            let profit = sim.final_output - candidate;
            let outputs: Vec<U256> = sim.steps.iter().map(|s| s.output).collect();
            if best.as_ref().is_none_or(|(_, p, _)| profit > *p) {
                best = Some((candidate, profit, outputs));
            }
        }
    }
    best
}

fn pow10(e: u32) -> U256 {
    let mut p = U256::from(1u64);
    for _ in 0..e {
        p *= U256::from(10u64);
    }
    p
}

fn hop3(base: U256, a: u64, b: u64, c: u64, g: u64, d: u64) -> Vec<IntHopState> {
    vec![
        IntHopState::new(base, base * U256::from(a), g, d),
        IntHopState::new(base * U256::from(b), base, g, d),
        IntHopState::new(base * U256::from(c), base, g, d),
    ]
}

const FEES: [(u64, u64); 3] = [(997, 1000), (9975, 10000), (9995, 10000)];
/// Profitable ratio triples: composed rate `a/(b*c) > 1`.
const RATIOS: [(u64, u64, u64); 4] = [(100, 1, 1), (100, 10, 1), (10, 1, 1), (100, 1, 10)];

#[test]
fn three_hop_1e24_discriminant_wraps_and_fixed_anchor_is_nonzero() {
    let base = pow10(24);
    for (g, d) in FEES {
        for (a, b, c) in RATIOS {
            let hops = hop3(base, a, b, c, g, d);
            let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
            assert!(coeffs.is_profitable, "grid case must be profitable");
            assert!(
                !u512_product_fits(&coeffs),
                "K*M must exceed U512 for 3-hop 1e24 (a{a} b{b} c{c} g{g})"
            );
            assert!(
                !compute_mobius_model_optimal_input(&coeffs).is_zero(),
                "widened discriminant must yield a usable anchor"
            );
            // The fail-confident artifact (1-wei optimum or None) is gone:
            // the fixed closed form + ±2 sweep reproduces the walk exactly.
            assert_eq!(
                baseline(&hops),
                walk(&hops),
                "fixed solution must match walk (a{a} b{b} c{c} g{g})"
            );
        }
    }
}

#[test]
fn fixed_anchor_recovers_walk_optimum_on_diverging_three_hop_paths() {
    let base = pow10(24);
    for (g, d) in FEES {
        for (a, b, c) in RATIOS {
            let hops = hop3(base, a, b, c, g, d);
            let coeffs = compute_int_mobius_coefficients(&hops).unwrap();
            let anchor = compute_mobius_model_optimal_input(&coeffs);
            assert!(
                anchor > pow10(20),
                "anchor must be a real e2x-scale input, not a micro fallback (a{a} b{b} c{c} g{g})"
            );
            assert_eq!(
                sweep(anchor, &hops),
                walk(&hops),
                "fixed closed form + ±2 sweep must match walk (a{a} b{b} c{c} g{g})"
            );

            // Regression: the old failure entered the micro-profit branch.
            let r = exact_mobius_solve(&hops).unwrap();
            assert!(
                r.used_closed_form,
                "must not take the micro-profit fallback"
            );
            assert_ne!(r.optimal_input, U256::from(1u64));
        }
    }
}

#[test]
fn trustable_domain_fixed_anchor_is_bit_identical_to_prefix_formula() {
    // Grid over magnitudes that span the discriminant boundary. Inside the
    // U512 product budget the widened isqrt delegates to the 512-bit floor
    // square root, so the anchor must be bit-identical to the pre-fix formula.
    for (g, d) in FEES {
        for m in [12u32, 15, 18, 21, 24] {
            let base = pow10(m);
            for (a, b, c) in RATIOS {
                let hops3 = hop3(base, a, b, c, g, d);
                let coeffs3 = compute_int_mobius_coefficients(&hops3).unwrap();
                if u512_product_fits(&coeffs3) {
                    assert_eq!(
                        compute_mobius_model_optimal_input(&coeffs3),
                        prefix_model_anchor(&coeffs3),
                        "3-hop 1e{m} in-domain anchor must be bit-identical"
                    );
                }

                let hops2 = vec![
                    IntHopState::new(base, base * U256::from(a), g, d),
                    IntHopState::new(base * U256::from(b), base, g, d),
                ];
                let coeffs2 = compute_int_mobius_coefficients(&hops2).unwrap();
                if u512_product_fits(&coeffs2) {
                    assert_eq!(
                        compute_mobius_model_optimal_input(&coeffs2),
                        prefix_model_anchor(&coeffs2),
                        "2-hop 1e{m} in-domain anchor must be bit-identical"
                    );
                }
            }
        }
    }
}
