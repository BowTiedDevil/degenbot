//! Benchmarks for Uniswap V3 tick math operations.

#![allow(clippy::unwrap_used, clippy::similar_names)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use degenbot_rs::get_sqrt_ratio_at_tick_internal;
use degenbot_rs::get_tick_at_sqrt_ratio_internal;

fn bench_tick_math(c: &mut Criterion) {
    let mut group = c.benchmark_group("tick_math");

    group.bench_function("get_sqrt_ratio_at_tick/0", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(0)));
    });

    group.bench_function("get_sqrt_ratio_at_tick/100000", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(100_000)));
    });

    group.bench_function("get_sqrt_ratio_at_tick/-887272", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(-887_272)));
    });

    let ratio_at_100k = get_sqrt_ratio_at_tick_internal(100_000).unwrap();
    group.bench_function("get_tick_at_sqrt_ratio/mid", |b| {
        b.iter(|| get_tick_at_sqrt_ratio_internal(black_box(ratio_at_100k)));
    });

    let ratio_at_min = get_sqrt_ratio_at_tick_internal(-887_272).unwrap();
    group.bench_function("get_tick_at_sqrt_ratio/min_bound", |b| {
        b.iter(|| get_tick_at_sqrt_ratio_internal(black_box(ratio_at_min)));
    });

    group.finish();
}

criterion_group!(benches, bench_tick_math);
criterion_main!(benches);
