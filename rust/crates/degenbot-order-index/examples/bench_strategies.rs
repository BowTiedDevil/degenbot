//! Spike S2 microbench: cost components of the order index at scale.
//!
//! Measures the operations that determine per-block and per-mutation cost so
//! the incremental-maintenance strategy can be justified by numbers.
//!
//! `remove` is still a full rebuild in this task (incremental remove is Task 4),
//! so single-insert costs are measured on a representative smaller index — the
//! insert cost is O(log h + k), independent of N.
//!
//! Run: `cargo run -p degenbot-order-index --example bench_strategies --release`

// Demo-only casts (f64 sqrt for point generation) — fine outside prod math.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use alloy_primitives::U256;
use std::time::Instant;

use degenbot_order_index::EnvelopeIndex;
use degenbot_order_index::OrderIndex;

fn lcg_state(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Deterministic (gas, gross) pairs, gross always positive.
fn cands(n: u64, mut seed: u64) -> Vec<(u64, U256, U256)> {
    const A: i128 = 1_000_000_000_000_000_000;
    (0..n)
        .map(|id| {
            let gas = 21_000u64 + lcg_state(&mut seed) % 8_000_000_000;
            let sqrt = (gas as f64).sqrt() as i128; // demo-only f64
            let base = A * sqrt;
            let signed = i128::from(lcg_state(&mut seed) % 21);
            let noise = (signed - 10) * 10;
            let gross = base + base * noise / 1000;
            (id, U256::from(gas), U256::from(gross as u128))
        })
        .collect()
}

fn main() {
    const N: u64 = 1_000_000;
    const K: usize = 50;
    let cands = cands(N, 0x9E37_79B9_7F4A_7C15);

    // 1. Batched/incremental build (insert loop; insert is incremental).
    let mut idx = EnvelopeIndex::<u64>::new();
    let mut small = EnvelopeIndex::<u64>::new();
    let t = Instant::now();
    for (i, (id, gas, gross)) in cands.iter().enumerate() {
        idx.insert(*id, *gas, *gross);
        if i < 100_000 {
            small.insert(*id, *gas, *gross);
        }
    }
    let t_build = t.elapsed();
    // 2. Per-block hot reclassify + top-K (the O(N log h) baseline).
    let x = U256::from(80_000_000_000u64);
    let t = Instant::now();
    let top = idx.top_k(x, K);
    let t_topk = t.elapsed();

    // 3. Hull-only per-block floor.
    let t = Instant::now();
    let best = idx.best(x);
    let t_best = t.elapsed();

    // 4. Interior insert — points far below the hull (binary search only).
    let batch_n = 100_000u64;
    let t = Instant::now();
    for i in 0..batch_n {
        small.insert(
            N + 1000 + i,
            U256::from(3_000_000u64),
            U256::from(1_000 + i),
        );
    }
    let t_interior = t.elapsed() / batch_n as u32;

    // 5. Above-hull insert — points far above the frontier (splice).
    let batch_a = 20_000u64;
    let t = Instant::now();
    for i in 0..batch_a {
        let gas = 21_000u64 + (i % 8_000_000);
        small.insert(
            2_000_000_000 + i,
            U256::from(gas),
            U256::from(1u128) << 100, // huge gross -> guaranteed above the hull
        );
    }
    let t_above = t.elapsed() / batch_a as u32;

    println!(
        "== spike S2 microbench: N={N}, K={K}, hull={} ==",
        idx.hull_len()
    );
    println!("incremental build(1M)   : {t_build:?}");
    println!(
        "per-block top_k({K})    : {t_topk:?}  ({} results)",
        top.len()
    );
    println!("hull-only best()        : {t_best:?}  (best idx {best:?})");
    println!("single interior insert  : {t_interior:?}");
    println!("single above-hull insert: {t_above:?}");
}
