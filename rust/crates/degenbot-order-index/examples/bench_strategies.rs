//! Spike S2 microbench: cost components of the order index at scale.
//!
//! Measures the operations that determine per-block and per-mutation cost so
//! the incremental-maintenance strategy can be justified by numbers.
//!
//! `remove` is still a full rebuild in this spike (incremental remove is Task 4),
//! so single-insert costs are measured on a representative smaller index — the
//! insert cost is O(log h + k), independent of N.
//!
//! Run: `cargo run -p degenbot-order-index --example bench_strategies --release`

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::hint::black_box;
use std::time::Instant;

use degenbot_order_index::{Candidate, EnvelopeIndex};

fn lcg_state(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn candidates(n: u64, mut seed: u64) -> Vec<Candidate> {
    const A: i128 = 1_000_000_000_000_000_000;
    (0..n)
        .map(|id| {
            let gas = 21_000i128 + i128::from(lcg_state(&mut seed) % 8_000_000_000);
            let sqrt = (gas as f64).sqrt() as i128; // demo-only f64
            let base = A * sqrt;
            let signed = i128::from(lcg_state(&mut seed) % 21);
            let noise = (signed - 10) * 10;
            Candidate {
                id,
                gas,
                gross: base + base * noise / 1000,
            }
        })
        .collect()
}

fn main() {
    const N: u64 = 1_000_000;
    const K: usize = 50;
    let cands = candidates(N, 0x9E37_79B9_7F4A_7C15);

    // 1. Full rebuild (batched extend).
    let t = Instant::now();
    let mut idx = EnvelopeIndex::new();
    idx.extend(cands.clone());
    let t_rebuild = t.elapsed();

    // 2. Incremental build (insert loop).
    let mut idx_inc = EnvelopeIndex::new();
    let t = Instant::now();
    for c in &cands {
        idx_inc.insert(*c);
    }
    let t_incremental = t.elapsed();
    assert_eq!(idx.hull_len(), idx_inc.hull_len(), "hull must match");

    // 3. Per-block hot reclassify + top-K (the O(N log h) baseline).
    let x = 80_000_000_000i128;
    let t = Instant::now();
    let top = idx_inc.top_k(x, K);
    let t_topk = t.elapsed();

    // 4. Hull-only per-block floor.
    let t = Instant::now();
    let best = idx_inc.best(x);
    let t_best = t.elapsed();

    // Single-insert costs measured on a 100k-prefix incremental index (cost is
    // O(log h + k), independent of N; avoiding `remove`'s full rebuild).
    let mut small = EnvelopeIndex::new();
    for c in cands.iter().take(100_000) {
        small.insert(*c);
    }

    // 5. Interior insert — a point far below the hull (binary search only).
    let batch_n = 100_000u64;
    let t = Instant::now();
    for i in 0..batch_n {
        small.insert(Candidate {
            id: N + 1000 + i,
            gas: 3_000_000,
            gross: 1_000_000 + i128::from(i),
        });
    }
    let t_interior = t.elapsed() / batch_n as u32;

    // 6. Above-hull insert — points far above the frontier (splice).
    let batch_a = 20_000u64;
    let t = Instant::now();
    for i in 0..batch_a {
        let gas = 21_000i128 + (i128::from(i) % 8_000_000);
        small.insert(Candidate {
            id: N + 2000 + i,
            gas,
            gross: 1i128 << 105,
        });
    }
    let t_above = t.elapsed() / batch_a as u32;

    println!(
        "== spike S2 microbench: N={N}, K={K}, hull={} (small-hull={}) ==",
        idx.hull_len(),
        0
    );
    println!("full rebuild     (1M)   : {t_rebuild:?}");
    println!("incremental build (1M)  : {t_incremental:?}");
    println!(
        "per-block top_k({K})    : {t_topk:?}  ({} results)",
        top.len()
    );
    println!("hull-only best()        : {t_best:?}  (best idx {best:?})");
    println!("single interior insert  : {t_interior:?}");
    println!("single above-hull insert: {t_above:?}");
    black_box(top);
}
