#![expect(clippy::print_stdout, clippy::expect_used)]
//! Minimal reproducer for the `exact_mobius_solve` 3-hop U512 overflow.
//!
//! Builds the all-V2 synthetic 3-hop configurations from
//! `all_v2_fastpath_ab.rs`, dumps the composed Möbius coefficients and the
//! `K * M` product used by the closed form, then compares `exact_mobius_solve`
//! against the unified walk.

use alloy::primitives::{U256, U512};
use degenbot_math::v2::IntHopState;
use degenbot_solvers::cl::{solve_mixed_piecewise, ClSolveTables, IntV3TickRangeSequence};
use degenbot_solvers::mobius_int::compute_int_mobius_coefficients;
use degenbot_solvers::mobius_int_exact::{compute_mobius_model_optimal_input, exact_mobius_solve};
use degenbot_solvers::runtime::SolveRuntimeConfig;

fn pow10(e: u32) -> U256 {
    let mut p = U256::from(1u64);
    for _ in 0..e {
        p *= U256::from(10u64);
    }
    p
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

fn probe(label: &str, hops: &[IntHopState]) {
    let coeffs = compute_int_mobius_coefficients(hops).expect("coeffs");
    let km_checked = coeffs.K.checked_mul(coeffs.M);
    let km_wrapped = coeffs.K * coeffs.M;
    let x_approx = compute_mobius_model_optimal_input(&coeffs);
    let base = baseline(hops);
    let w = walk(hops);
    println!("=== {label} ===");
    println!("  K bit_len={} K={}", coeffs.K.bit_len(), coeffs.K);
    println!("  M bit_len={} M={}", coeffs.M.bit_len(), coeffs.M);
    println!("  N bit_len={} N={}", coeffs.N.bit_len(), coeffs.N);
    println!("  is_profitable={}", coeffs.is_profitable);
    println!(
        "  K*M checked_ok={} bit_len={}",
        km_checked.is_some(),
        km_checked.unwrap_or_default().bit_len()
    );
    println!(
        "  K*M wrapped={km_wrapped} (bit_len={})",
        km_wrapped.bit_len()
    );
    println!("  U512::MAX          ={}", U512::MAX);
    println!("  x_approx(model)    ={x_approx}");
    println!("  fastpath(opt,pft,outs)={base:?}");
    println!("  walk   (opt,pft,outs)={w:?}");
}

fn main() {
    let fees = [
        (997u64, 1000u64, "30bp"),
        (9975, 10000, "25bp"),
        (9995, 10000, "5bp"),
    ];
    for (g, d, fl) in fees {
        for m in [18u32, 24] {
            let base = pow10(m);
            for (a, b, c) in [(100u64, 1u64, 1u64), (100, 10, 1), (10, 1, 1), (100, 1, 10)] {
                let hop0 = IntHopState::new(base, base * U256::from(a), g, d);
                let hop1 = IntHopState::new(base * U256::from(b), base, g, d);
                let hop2 = IntHopState::new(base * U256::from(c), base, g, d);
                probe(
                    &format!("3hop {fl} 1e{m} a{a} b{b} c{c}"),
                    &[hop0, hop1, hop2],
                );
            }
            // 2-hop control
            let hop0 = IntHopState::new(base, base * U256::from(10u64), g, d);
            let hop1 = IntHopState::new(base * U256::from(10u64), base, g, d);
            probe(&format!("2hop {fl} 1e{m} a10 b10"), &[hop0, hop1]);
        }
    }
}
