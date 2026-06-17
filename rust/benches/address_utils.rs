//! Benchmark for `to_checksum_address`: `GIL` detach vs hold.
//!
//! Measures whether releasing the `GIL` during address checksumming is a net win
//! or loss. The computation takes ~50ns; `GIL` release/reacquire costs ~200ns.
//! If holding the `GIL` is faster (as expected), this benchmark provides the
//! numbers for the `SAFETY` comment on `to_checksum_address`.
//!
//! Two benchmarks:
//! 1. `pure_rust` — calls the Rust function directly (baseline, no `GIL`)
//! 2. `gil_held` — calls the Rust function with `GIL` held (simulates `PyO3` wrapper without detach)
//!
//! The `GIL` overhead is the difference between (1) and (2). If the `GIL` hold
//! overhead is less than ~200ns (the detach/reacquire cost), holding the `GIL`
//! is the faster choice.

#![allow(clippy::unwrap_used)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pyo3::prelude::*;

const ADDR_HEX: &str = "0x66f9664f97f2b50f62d13ea064982f936de76657";

fn bench_to_checksum_address(c: &mut Criterion) {
    let mut group = c.benchmark_group("address_utils");

    // Baseline: pure Rust, no GIL involvement
    group.bench_function("to_checksum_address/pure_rust", |b| {
        b.iter(|| {
            black_box(
                degenbot_rs::address_utils::to_checksum_address_str(black_box(ADDR_HEX)).unwrap(),
            )
        });
    });

    // With GIL held (simulates the PyO3 wrapper's current behavior — no detach)
    group.bench_function("to_checksum_address/gil_held", |b| {
        b.iter_custom(|iters| {
            Python::attach(|py| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    black_box(
                        degenbot_rs::address_utils::to_checksum_address_str(black_box(ADDR_HEX))
                            .unwrap(),
                    );
                    // Touch `py` to ensure the compiler doesn't elide the GIL hold
                    let _ = py.None();
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(benches, bench_to_checksum_address);
criterion_main!(benches);
