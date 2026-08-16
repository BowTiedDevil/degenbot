//! Single-hop swap simulation latency for Curve `StableSwap` pools.
//!
//! `swap_fn` for a Curve hop calls `stableswap_get_y` (which internally calls
//! `stableswap_get_d` once per call). Benches at mainnet-like 3pool magnitudes.

#![expect(clippy::cast_precision_loss)]
#![expect(clippy::many_single_char_names)]
#![expect(clippy::unwrap_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use degenbot_curve_math::stableswap::{stableswap_get_d, stableswap_get_y, DVariant, YVariant};

/// Curve 3pool-like reserves (DAI/USDC/USDT, 18dp normalized via precisions).
/// ~$30M pool. amp=2000 (A=200), `n_coins=3`.
fn xp_3pool() -> Vec<alloy::primitives::U256> {
    use alloy::primitives::U256;
    // 10M of each, 18dp (after precision scaling).
    vec![
        U256::from(10_000_000_000_000_000_000_000_000u128), // 10M * 1e18
        U256::from(10_000_000_000_000_000_000_000_000u128),
        U256::from(10_000_000_000_000_000_000_000_000u128),
    ]
}

const AMP: u128 = 2000;
const A_PRECISION: u128 = 100;
const N_COINS: u64 = 3;

fn bench_curve_get_d(c: &mut Criterion) {
    let xp = xp_3pool();
    c.bench_function("stableswap_get_d (3pool, A=2000)", |b| {
        b.iter(|| {
            black_box(
                stableswap_get_d(
                    black_box(&xp),
                    black_box(alloy::primitives::U256::from(AMP)),
                    black_box(alloy::primitives::U256::from(N_COINS)),
                    black_box(alloy::primitives::U256::from(A_PRECISION)),
                    black_box(DVariant::Standard),
                )
                .unwrap(),
            )
        });
    });
}

fn bench_curve_get_y(c: &mut Criterion) {
    let xp = xp_3pool();
    // Swap 1000 DAI (18dp) for USDC: i=0, j=1.
    let x_in = alloy::primitives::U256::from(1_000_000_000_000_000_000_000u128); // 1000 * 1e18
    c.bench_function("stableswap_get_y (3pool swap)", |b| {
        b.iter(|| {
            black_box(
                stableswap_get_y(
                    black_box(0),
                    black_box(1),
                    black_box(x_in),
                    black_box(&xp),
                    black_box(alloy::primitives::U256::from(AMP)),
                    black_box(alloy::primitives::U256::from(N_COINS)),
                    black_box(alloy::primitives::U256::from(A_PRECISION)),
                    black_box(YVariant::Standard),
                    black_box(DVariant::Standard),
                )
                .unwrap(),
            )
        });
    });
}

/// f64 analog of `get_y` — used to measure the bracket-narrow alternative.
/// Curve invariant: x³y + xy³ = D³ (for n=2) iteratively, but for stableswap
/// we can use a scalar Newton on the same recurrence in f64.
fn curve_get_y_f64(xp: &[f64], amp: f64, n_coins: f64, i: usize, j: usize, x: f64, d: f64) -> f64 {
    let n = n_coins;
    let mut c = d;
    let mut s = 0.0;
    for (k, &xk) in xp.iter().enumerate() {
        if k == j {
            continue;
        }
        let xx = if k == i { x } else { xk };
        s += xx;
        c = c * d / (xx * n);
    }
    let ann = amp * n;
    let c = c * d / (ann * n);
    let b = s + d / ann;
    let mut y = d;
    for _ in 0..255 {
        let y_prev = y;
        y = (y * y + c) / (2.0 * y + b - d);
        if (y - y_prev).abs() <= 1.0 {
            return y;
        }
    }
    y
}

fn bench_curve_get_y_f64(c: &mut Criterion) {
    let xp = xp_3pool();
    let xp_f: Vec<f64> = xp.iter().map(|v| v.to::<u128>() as f64).collect();
    let d = stableswap_get_d(
        &xp,
        alloy::primitives::U256::from(AMP),
        alloy::primitives::U256::from(N_COINS),
        alloy::primitives::U256::from(A_PRECISION),
        DVariant::Standard,
    )
    .unwrap()
    .to::<u128>() as f64;
    let x_in = 1_000_000_000_000_000_000_000u128 as f64;
    c.bench_function("curve_get_y_f64 (3pool swap)", |b| {
        b.iter(|| {
            black_box(curve_get_y_f64(
                black_box(&xp_f),
                black_box(AMP as f64),
                black_box(N_COINS as f64),
                black_box(0),
                black_box(1),
                black_box(x_in),
                black_box(d),
            ))
        });
    });
}

criterion_group!(
    benches,
    bench_curve_get_d,
    bench_curve_get_y,
    bench_curve_get_y_f64
);
criterion_main!(benches);
