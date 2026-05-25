//! Benchmarks comparing the f64 Möbius solver vs. the integer-exact solver.

#![allow(clippy::unwrap_used)]

use alloy::primitives::{U256, U512};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use degenbot_rs::optimizers::mobius::{mobius_solve, HopState};
use degenbot_rs::optimizers::mobius_int::{int_mobius_solve, int_simulate_path, IntHopState};
use degenbot_rs::optimizers::mobius_int_exact::exact_mobius_solve;

fn u256(n: u64) -> U256 {
    U256::from(n)
}

/// Convert IntHopState to f64 HopState for the legacy solver.
fn int_hop_to_f64(hop: &IntHopState) -> HopState {
    let fee = 1.0 - hop.gamma_numer as f64 / hop.fee_denom as f64;
    HopState::new(
        degenbot_rs::optimizers::mobius_int::u256_to_f64(hop.reserve_in),
        degenbot_rs::optimizers::mobius_int::u256_to_f64(hop.reserve_out),
        fee,
    )
}

fn bench_mobius_exact_vs_f64(c: &mut Criterion) {
    let mut group = c.benchmark_group("mobius_exact_vs_f64");

    // ── 2-hop: typical DEX reserves ─────────────────────────────
    let int_hops_2 = vec![
        IntHopState::new(u256(1_000_000_000_000_000_000), u256(2_000_000_000_000_000_000), 997, 1000),
        IntHopState::new(u256(2_000_000_000_000_000_000), u256(500_000_000_000_000_000), 997, 1000),
    ];
    let f64_hops_2: Vec<HopState> = int_hops_2.iter().map(int_hop_to_f64).collect();

    group.bench_function("f64_mobius_solve/2hop", |b| {
        b.iter(|| mobius_solve(black_box(&f64_hops_2), None));
    });

    group.bench_function("int_mobius_solve/2hop", |b| {
        b.iter(|| int_mobius_solve(black_box(&int_hops_2)).unwrap());
    });

    group.bench_function("exact_mobius_solve/2hop", |b| {
        b.iter(|| exact_mobius_solve(black_box(&int_hops_2)).unwrap());
    });

    // ── 3-hop: typical DEX reserves ─────────────────────────────
    let int_hops_3 = vec![
        IntHopState::new(u256(1_000_000_000_000_000_000), u256(2_000_000_000_000_000_000), 997, 1000),
        IntHopState::new(u256(2_000_000_000_000_000_000), u256(800_000_000_000_000_000), 997, 1000),
        IntHopState::new(u256(800_000_000_000_000_000), u256(1_500_000_000_000_000_000), 997, 1000),
    ];
    let f64_hops_3: Vec<HopState> = int_hops_3.iter().map(int_hop_to_f64).collect();

    group.bench_function("f64_mobius_solve/3hop", |b| {
        b.iter(|| mobius_solve(black_box(&f64_hops_3), None));
    });

    group.bench_function("int_mobius_solve/3hop", |b| {
        b.iter(|| int_mobius_solve(black_box(&int_hops_3)).unwrap());
    });

    group.bench_function("exact_mobius_solve/3hop", |b| {
        b.iter(|| exact_mobius_solve(black_box(&int_hops_3)).unwrap());
    });

    // ── Large reserves (WETH-scale, 18 decimal) ──────────────────
    let weth_reserves = U256::from(40_000u128) * U256::from(10u128).pow(U256::from(18u64)); // 40K WETH
    let usdc_reserves = U256::from(100_000_000_000_000u64); // 100M USDC

    let int_hops_large = vec![
        IntHopState::new(weth_reserves, usdc_reserves, 997, 1000),
        IntHopState::new(usdc_reserves, weth_reserves, 997, 1000),
    ];
    let f64_hops_large: Vec<HopState> = int_hops_large.iter().map(int_hop_to_f64).collect();

    group.bench_function("f64_mobius_solve/large_reserves", |b| {
        b.iter(|| mobius_solve(black_box(&f64_hops_large), None));
    });

    group.bench_function("exact_mobius_solve/large_reserves", |b| {
        b.iter(|| exact_mobius_solve(black_box(&int_hops_large)).unwrap());
    });

    // ── Simulation only (no coefficient computation) ──────────────
    group.bench_function("int_simulate_path/2hop", |b| {
        let x = u256(1_000_000_000_000_000);
        b.iter(|| int_simulate_path(black_box(x), black_box(&int_hops_2)));
    });

    group.finish();
}

/// Benchmark the isqrt_u512 function at different scales.
fn bench_isqrt(c: &mut Criterion) {
    let mut group = c.benchmark_group("isqrt_u512");

    // Small values
    for (name, val) in [
        ("small_100", U512::from(100u64)),
        ("medium_1e18", U512::from(1_000_000_000_000_000_000u64)),
        ("large_u256_max", U512::from(U256::MAX)),
    ] {
        group.bench_function(BenchmarkId::new("isqrt", name), |b| {
            b.iter(|| degenbot_rs::optimizers::mobius_int_exact::isqrt_u512(black_box(val)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_mobius_exact_vs_f64, bench_isqrt);
criterion_main!(benches);
