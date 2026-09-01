#![expect(clippy::unwrap_used, clippy::expect_used)]
//! Spike (ergo 77LOQT / ADR-015 deferred hop-shape deepening): measure the
//! cost of the two *candidate* hop digests (Balancer stable `D`, Curve `xp`)
//! relative to their family's Phase B golden-section solve. CL is the
//! already-cached reference (not benched here — its O(N log N) tick walk is
//! the existence proof, not a candidate).
//!
//! The headline number per family is `T_digest / (T_digest + T_solve)`:
//! the share of per-resolve cost the digest represents. Cross-path
//! memoization saves `(N_paths_sharing_pool − 1) × T_digest` per pool per
//! block, so the ratio bounds the ceiling of the win.
//!
//! Fixtures mirror the real profitable-path setups from
//! `arb_engine/tests.rs` (`balancer_stable_finds_profitable_arb`,
//! `curve_stable_finds_profitable_arb`): 1000/2000 vs 1000/1950 reserves
//! (18dp), which exercises the full 25-iteration golden-section search
//! (no early-out).

#![expect(clippy::doc_markdown)]

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use degenbot_math::balancer::stable_math::calculate_invariant_deployed;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_solvers::mixed::{
    solve_path, BalancerStableHopState, CurveStableswapHopState, ResolvedHop, ResolvedMixedPath,
};

use alloy::primitives::U256;

// ---------------------------------------------------------------------------
// Fixtures — mirror arb_engine/tests.rs profitable-path setups.
// ---------------------------------------------------------------------------

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// amp = 200 × AMP_PRECISION(1000) = 200_000 — matches `balancer_stable_params`.
fn balancer_amp() -> U256 {
    U256::from(200_000u64)
}

/// Upscaled balances `[b0, b1]` at 18dp (scaling_factors are identity in the
/// test fixture, so upscale is a no-op). Amp/name come from the test.
fn balancer_balances(b0: u128, b1: u128) -> Vec<U256> {
    let e = one_e18();
    vec![U256::from(b0) * e, U256::from(b1) * e]
}

/// 3-token variant — tests the n_tokens scaling of `calculate_invariant`'s
/// inner D_P loop. `[b0, b1, b2]` at 18dp.
fn balancer_balances_3(b0: u128, b1: u128, b2: u128) -> Vec<U256> {
    let e = one_e18();
    vec![U256::from(b0) * e, U256::from(b1) * e, U256::from(b2) * e]
}

/// 4-token variant — realistic upper bound for a hot ComposableStable hop.
fn balancer_balances_4(b0: u128, b1: u128, b2: u128, b3: u128) -> Vec<U256> {
    let e = one_e18();
    vec![
        U256::from(b0) * e,
        U256::from(b1) * e,
        U256::from(b2) * e,
        U256::from(b3) * e,
    ]
}

/// Build a fully-resolved BalancerStable hop state (digest baked in), mirroring
/// what `resolve_path` produces. `invariant_version == 2` → the deployed
/// `calculate_invariant_deployed(amp, &balances, true)` path (round_up=true).
fn balancer_stable_hop(b0: u128, b1: u128, zero_for_one: bool) -> BalancerStableHopState {
    let balances = balancer_balances(b0, b1);
    let invariant = calculate_invariant_deployed(balancer_amp(), &balances, true)
        .expect("invariant converges on valid fixture");
    let (token_index_in, token_index_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    BalancerStableHopState {
        amp: balancer_amp(),
        balances,
        token_index_in,
        token_index_out,
        invariant,
        swap_fee: U256::from(10_000_000_000_000u64), // 0.01% of 1e18
        scaling_factor_in: U256::from(1u64),
        scaling_factor_out: U256::from(1u64),
    }
}

/// Profitable 2-hop balancer-stable path: 1000/2000 (zfo=true) → 1000/1950 (zfo=false).
/// Mirrors `balancer_stable_finds_profitable_arb`.
fn balancer_stable_path() -> ResolvedMixedPath {
    ResolvedMixedPath {
        hops: vec![
            ResolvedHop::BalancerStable {
                state: balancer_stable_hop(1000, 2000, true),
            },
            ResolvedHop::BalancerStable {
                state: balancer_stable_hop(1000, 1950, false),
            },
        ],
        valid: true,
        state_nonces: vec![],
        max_update_block: 0, // no CL hops in this fixture; default write-stamp
    }
}

// --- Curve fixtures ---

fn precision() -> U256 {
    one_e18()
} // 1e18
const FEE_DENOM: U256 = U256::from_limbs([10_000_000_000u64, 0, 0, 0]); // 1e10 — const-constructible
const A_PRECISION: U256 = U256::from_limbs([100u64, 0, 0, 0]);

fn curve_amp() -> U256 {
    U256::from(10u64) * A_PRECISION // a_coefficient=10 × A_PRECISION
}

fn curve_balances(b0: u128, b1: u128) -> Vec<U256> {
    let e = one_e18();
    vec![U256::from(b0) * e, U256::from(b1) * e]
}

/// Identity rate multipliers (1e18) — the test fixture's values.
fn curve_rate_multipliers() -> Vec<U256> {
    vec![precision(), precision()]
}

/// Build the xp digest exactly as `resolve_path` does:
/// `xp[i] = balances[i] * rate_multipliers[i] / PRECISION`.
fn curve_xp(balances: &[U256], rate_multipliers: &[U256]) -> Vec<U256> {
    let p = precision();
    balances
        .iter()
        .zip(rate_multipliers.iter())
        .map(|(b, rm)| b.saturating_mul(*rm) / p)
        .collect()
}

fn curve_stable_hop(b0: u128, b1: u128, zero_for_one: bool) -> CurveStableswapHopState {
    let balances = curve_balances(b0, b1);
    let rms = curve_rate_multipliers();
    let xp = curve_xp(&balances, &rms);
    let (token_index_in, token_index_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    CurveStableswapHopState {
        amp: curve_amp(),
        a_precision: A_PRECISION,
        xp,
        token_index_in,
        token_index_out,
        n_coins: U256::from(2u64),
        fee: U256::from(4_000_000u64), // 0.04% of 1e10
        fee_denom: FEE_DENOM,
        precision: precision(),
        rate_multiplier_in: rms[token_index_in],
        rate_multiplier_out: rms[token_index_out],
        y_variant: YVariant::try_from_u8(1).expect("standard y variant"),
        d_variant: DVariant::try_from_u8(1).expect("standard d variant"),
    }
}

/// Profitable 2-hop curve path: 1000/2000 → 1000/1950. Mirrors
/// `curve_stable_finds_profitable_arb`.
fn curve_stable_path() -> ResolvedMixedPath {
    ResolvedMixedPath {
        hops: vec![
            ResolvedHop::CurveStableswap {
                state: curve_stable_hop(1000, 2000, true),
            },
            ResolvedHop::CurveStableswap {
                state: curve_stable_hop(1000, 1950, false),
            },
        ],
        valid: true,
        state_nonces: vec![],
        max_update_block: 0, // no CL hops in this fixture; default write-stamp
    }
}

// ---------------------------------------------------------------------------
// Benches.
// ---------------------------------------------------------------------------

fn bench_digest_balancer_stable(c: &mut Criterion) {
    let mut g = c.benchmark_group("digest_balancer_stable_D");

    let b2 = balancer_balances(1000, 2000);
    let b3 = balancer_balances_3(1000, 2000, 1500);
    let b4 = balancer_balances_4(1000, 2000, 1500, 1200);
    let amp = balancer_amp();

    g.bench_function("2token", |bencher| {
        bencher.iter(|| {
            let d = calculate_invariant_deployed(black_box(amp), black_box(&b2), true)
                .expect("converges");
            black_box(d);
        });
    });
    g.bench_function("3token", |bencher| {
        bencher.iter(|| {
            let d = calculate_invariant_deployed(black_box(amp), black_box(&b3), true)
                .expect("converges");
            black_box(d);
        });
    });
    g.bench_function("4token", |bencher| {
        bencher.iter(|| {
            let d = calculate_invariant_deployed(black_box(amp), black_box(&b4), true)
                .expect("converges");
            black_box(d);
        });
    });
    g.finish();
}

fn bench_digest_curve_xp(c: &mut Criterion) {
    let mut g = c.benchmark_group("digest_curve_xp");

    let p = precision();
    let balances2 = curve_balances(1000, 2000);
    let rms = curve_rate_multipliers();

    // The 2-coin hot path — what `resolve_path` builds inline today.
    g.bench_function("2coin", |bencher| {
        bencher.iter(|| {
            let xp: Vec<U256> = black_box(&balances2)
                .iter()
                .zip(black_box(&rms).iter())
                .map(|(b, rm)| b.saturating_mul(*rm) / p)
                .collect();
            black_box(xp);
        });
    });

    // 4-coin variant (metapool / multi-coin) — still trivial, but measured.
    let e = one_e18();
    let balances4 = vec![
        U256::from(1000u64) * e,
        U256::from(2000u64) * e,
        U256::from(1500u64) * e,
        U256::from(1200u64) * e,
    ];
    let rms4 = vec![precision(); 4];
    g.bench_function("4coin", |bencher| {
        bencher.iter(|| {
            let xp: Vec<U256> = black_box(&balances4)
                .iter()
                .zip(black_box(&rms4).iter())
                .map(|(b, rm)| b.saturating_mul(*rm) / p)
                .collect();
            black_box(xp);
        });
    });

    g.finish();
}

fn bench_solve_balancer_stable(c: &mut Criterion) {
    let path = balancer_stable_path();
    // Sanity: confirm the fixture is profitable (so the 25-iter search runs).
    let probe = solve_path(
        &path,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    );
    assert!(
        probe.result.is_some(),
        "balancer-stable path must be profitable"
    );
    assert!(!probe.result.unwrap().profit.is_zero());

    c.bench_function("solve_balancer_stable_path", |bencher| {
        bencher.iter(|| {
            let r = solve_path(
                black_box(&path),
                &::degenbot_solvers::profit_envelope::GateDeps::offline(),
            );
            black_box(r);
        });
    });
}

fn bench_solve_curve(c: &mut Criterion) {
    let path = curve_stable_path();
    let probe = solve_path(
        &path,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    );
    assert!(
        probe.result.is_some(),
        "curve-stable path must be profitable"
    );
    assert!(!probe.result.unwrap().profit.is_zero());

    c.bench_function("solve_curve_path", |bencher| {
        bencher.iter(|| {
            let r = solve_path(
                black_box(&path),
                &::degenbot_solvers::profit_envelope::GateDeps::offline(),
            );
            black_box(r);
        });
    });
}

criterion_group!(
    benches,
    bench_digest_balancer_stable,
    bench_digest_curve_xp,
    bench_solve_balancer_stable,
    bench_solve_curve,
);
criterion_main!(benches);
