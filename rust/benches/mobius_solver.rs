//! Benchmarks for the integer Möbius optimizer.

#![allow(clippy::unwrap_used)]

use alloy::primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use degenbot_rs::optimizers::mobius_int::{int_simulate_path, IntHopState};
use degenbot_rs::optimizers::mobius_int_exact::exact_mobius_solve;

fn u256(n: u64) -> U256 {
    U256::from(n)
}

fn bench_mobius_solver(c: &mut Criterion) {
    let mut group = c.benchmark_group("mobius_solver");

    // 2-hop path with typical DEX reserves
    let hops_2 = vec![
        IntHopState::new(
            u256(1_000_000_000_000_000_000),
            u256(2_000_000_000_000_000_000),
            997,
            1000,
        ),
        IntHopState::new(
            u256(2_000_000_000_000_000_000),
            u256(500_000_000_000_000_000),
            997,
            1000,
        ),
    ];

    group.bench_function("int_simulate_path/2hop", |b| {
        let x = u256(1_000_000_000_000_000);
        b.iter(|| int_simulate_path(black_box(x), black_box(&hops_2)));
    });

    group.bench_function("exact_mobius_solve/2hop", |b| {
        b.iter(|| exact_mobius_solve(black_box(&hops_2)).unwrap());
    });

    // 3-hop path with typical DEX reserves
    let hops_3 = vec![
        IntHopState::new(
            u256(1_000_000_000_000_000_000),
            u256(2_000_000_000_000_000_000),
            997,
            1000,
        ),
        IntHopState::new(
            u256(2_000_000_000_000_000_000),
            u256(800_000_000_000_000_000),
            997,
            1000,
        ),
        IntHopState::new(
            u256(800_000_000_000_000_000),
            u256(1_500_000_000_000_000_000),
            997,
            1000,
        ),
    ];

    group.bench_function("int_simulate_path/3hop", |b| {
        let x = u256(1_000_000_000_000_000);
        b.iter(|| int_simulate_path(black_box(x), black_box(&hops_3)));
    });

    group.bench_function("exact_mobius_solve/3hop", |b| {
        b.iter(|| exact_mobius_solve(black_box(&hops_3)).unwrap());
    });

    group.finish();
}

criterion_group!(benches, bench_mobius_solver);
criterion_main!(benches);
