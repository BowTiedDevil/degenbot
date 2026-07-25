//! Baseline: V2 constant-product single-hop swap (Möbius closed-form).
//!
//! The reference fast path — engine V2/V3/V4 Möbius solves use cheaper
//! closed-form math; this gives the floor for comparison.

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use degenbot_v2_math::IntHopState;

fn bench_v2_swap(c: &mut Criterion) {
    // Mainnet-ish 0.3% pool: 1e21 WETH reserves, 2e24 USDC reserves (18dp).
    // gamma_numer=997, fee_denom=1000.
    let hop = IntHopState::new(
        alloy::primitives::U256::from(1_000_000_000_000_000_000_000u128),
        alloy::primitives::U256::from(2_000_000_000_000_000_000_000_000u128),
        997,
        1000,
    );
    // Mid-size swap: 1e18 (1 WETH).
    let amount_in = alloy::primitives::U256::from(1_000_000_000_000_000_000u128);

    c.bench_function("IntHopState::swap (V2 0.3%)", |b| {
        b.iter(|| black_box(hop.swap(black_box(amount_in)).unwrap()));
    });
}

criterion_group!(benches, bench_v2_swap);
criterion_main!(benches);
