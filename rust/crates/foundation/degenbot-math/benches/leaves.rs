//! Single-hop swap simulation latency for Balancer pools.
//!
//! Two leaves:
//! - `weighted_math::calc_out_given_in` — V2 weighted pools (pow-based)
//! - `stable_math::calc_out_given_in` — `StableSwap` pools (Newton iter)
//!
//! `stable_math` also benches `calculate_invariant_deployed` separately,
//! since `resolve_path` computes D once (not per solve iteration).

#![expect(clippy::cast_precision_loss)]
#![expect(clippy::unwrap_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use degenbot_math::balancer::{
    stable_math::{calc_out_given_in as stable_calc_out_given_in, calculate_invariant_deployed},
    weighted_math::calc_out_given_in as weighted_calc_out_given_in,
    PowVersion,
};

/// 50/50 weighted pool, ~$1M reserves, 0.3% fee. 18dp.
fn weighted_5050() -> (
    alloy::primitives::U256,
    alloy::primitives::U256,
    alloy::primitives::U256,
) {
    use alloy::primitives::U256;
    let balance_in = U256::from(500_000_000_000_000_000_000_000u128); // 500k * 1e18
    let balance_out = U256::from(500_000_000_000_000_000_000_000u128);
    // weights are FXP 1e18: 0.5 = 5e17.
    let w = U256::from(500_000_000_000_000_000u128);
    (balance_in, balance_out, w)
}

/// 80/20 weighted pool.
fn weighted_8020() -> (
    alloy::primitives::U256,
    alloy::primitives::U256,
    alloy::primitives::U256,
    alloy::primitives::U256,
) {
    use alloy::primitives::U256;
    let balance_in = U256::from(800_000_000_000_000_000_000_000u128);
    let balance_out = U256::from(200_000_000_000_000_000_000_000u128);
    let w_in = U256::from(800_000_000_000_000_000u128);
    let w_out = U256::from(200_000_000_000_000_000u128);
    (balance_in, balance_out, w_in, w_out)
}

fn bench_weighted_5050(c: &mut Criterion) {
    let (b_in, b_out, w) = weighted_5050();
    let amount_in = alloy::primitives::U256::from(1_000_000_000_000_000_000u128); // 1 token
    c.bench_function("weighted calc_out_given_in (50/50, V2)", |b| {
        b.iter(|| {
            black_box(
                weighted_calc_out_given_in(
                    black_box(b_in),
                    black_box(w),
                    black_box(b_out),
                    black_box(w),
                    black_box(amount_in),
                    black_box(PowVersion::V2),
                )
                .unwrap(),
            )
        });
    });
}

fn bench_weighted_8020(c: &mut Criterion) {
    let (b_in, b_out, w_in, w_out) = weighted_8020();
    let amount_in = alloy::primitives::U256::from(1_000_000_000_000_000_000u128);
    c.bench_function("weighted calc_out_given_in (80/20, V2)", |b| {
        b.iter(|| {
            black_box(
                weighted_calc_out_given_in(
                    black_box(b_in),
                    black_box(w_in),
                    black_box(b_out),
                    black_box(w_out),
                    black_box(amount_in),
                    black_box(PowVersion::V2),
                )
                .unwrap(),
            )
        });
    });
}

/// f64 analog of weighted swap — measures the bracket-narrow alternative.
fn weighted_calc_out_given_in_f64(
    balance_in: f64,
    weight_in: f64,
    balance_out: f64,
    weight_out: f64,
    amount_in: f64,
) -> f64 {
    let base = balance_in / (balance_in + amount_in);
    let exponent = weight_in / weight_out;
    balance_out * (1.0 - base.powf(exponent))
}

fn bench_weighted_5050_f64(c: &mut Criterion) {
    let (b_in, b_out, w) = weighted_5050();
    let b_in_f = b_in.to::<u128>() as f64;
    let b_out_f = b_out.to::<u128>() as f64;
    let w_f = w.to::<u128>() as f64 / 1e18;
    let amount_in_f = 1e18f64;
    c.bench_function("weighted calc_out_given_in (50/50, f64)", |b| {
        b.iter(|| {
            black_box(weighted_calc_out_given_in_f64(
                black_box(b_in_f),
                black_box(w_f),
                black_box(b_out_f),
                black_box(w_f),
                black_box(amount_in_f),
            ))
        });
    });
}

// --- Stable pool ---

/// 3-token Balancer stable pool, amp=200, ~$30M reserves.
fn stable_3token() -> Vec<alloy::primitives::U256> {
    use alloy::primitives::U256;
    vec![
        U256::from(10_000_000_000_000_000_000_000_000u128),
        U256::from(10_000_000_000_000_000_000_000_000u128),
        U256::from(10_000_000_000_000_000_000_000_000u128),
    ]
}

const STABLE_AMP: u128 = 200_000; // A=200, scaled by AMP_PRECISION=1000 (deployed convention)

fn bench_stable_invariant(c: &mut Criterion) {
    let balances = stable_3token();
    c.bench_function(
        "stable calculate_invariant_deployed (3-token, A=200)",
        |b| {
            b.iter(|| {
                black_box(
                    calculate_invariant_deployed(
                        black_box(alloy::primitives::U256::from(STABLE_AMP)),
                        black_box(&balances),
                        black_box(false),
                    )
                    .unwrap(),
                )
            });
        },
    );
}

fn bench_stable_swap(c: &mut Criterion) {
    let balances = stable_3token();
    let d =
        calculate_invariant_deployed(alloy::primitives::U256::from(STABLE_AMP), &balances, false)
            .unwrap();
    let amount_in = alloy::primitives::U256::from(1_000_000_000_000_000_000u128); // 1 token
    c.bench_function("stable calc_out_given_in (3-token, precomputed D)", |b| {
        b.iter(|| {
            black_box(
                stable_calc_out_given_in(
                    black_box(alloy::primitives::U256::from(STABLE_AMP)),
                    black_box(&balances),
                    black_box(0),
                    black_box(1),
                    black_box(amount_in),
                    black_box(d),
                )
                .unwrap(),
            )
        });
    });
}

criterion_group!(
    benches,
    bench_weighted_5050,
    bench_weighted_8020,
    bench_weighted_5050_f64,
    bench_stable_invariant,
    bench_stable_swap,
);
criterion_main!(benches);
