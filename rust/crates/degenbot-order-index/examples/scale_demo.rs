//! Scale demonstration for the [`EnvelopeIndex`] prototype.
//!
//! Simulates the end-state load described in the design discussion: hundreds of
//! thousands to millions of stored path results, ordered by net profit under a
//! per-block gas price `X`. Most results are interior to the `(gas, gross)`
//! envelope (dominated or far below the top-K), so the hot/cold split should
//! collapse the million-point set to a tiny hot set that is scanned each block.
//!
//! Run with:
//! ```text
//! cargo run -p degenbot-order-index --example scale_demo --release
//! ```

// Demo-only casts (f64 sqrt for point generation) — these are fine outside
// production math paths.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::items_after_statements
)]

use std::time::Instant;

use degenbot_order_index::{Candidate, EnvelopeIndex};

fn main() {
    const N: u64 = 1_000_000;
    const K: usize = 50;

    // Deterministic pseudo-random LCG (demo only).
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    // Concave frontier ~ A*sqrt(gas): low gas -> moderate gross, high gas ->
    // larger gross, concavity making intermediate points a real frontier.
    // Additive noise pushes a fraction of points *above* the smooth curve,
    // giving the hull a genuine (random) extent rather than a trivial one.
    const A: i128 = 1_000_000_000_000_000_000; // 1e18
    let build_t0 = Instant::now();
    let mut cands = Vec::with_capacity(N as usize);
    for id in 0..N {
        let gas = 21_000i128 + i128::from(next() % 8_000_000_000); // up to ~8M
        let sqrt = (gas as f64).sqrt() as i128; // demo: f64 is fine here
        let base = A * sqrt;
        // noise in -10%..+10%
        let signed = i128::from(next() % 21); // 0..=20
        let noise = (signed - 10) * 10; // -100..=+100 => -10%..+10% of 1e3
        let gross = base + base * noise / 1000;
        cands.push(Candidate { id, gas, gross });
    }
    let mut idx = EnvelopeIndex::new();
    idx.extend(cands);
    let build = build_t0.elapsed();

    let x = 80_000_000_000i128; // 80 gwei
    let hot = idx.hot_len(x, K);
    let t_top = Instant::now();
    let top = idx.top_k(x, K);
    let top_dur = t_top.elapsed();

    println!("== scale demo: {N} candidates, K={K} ==");
    println!("build+index        : {build:?}");
    println!("hull vertices      : {}", idx.hull_len());
    println!(
        "hot set @ {:.0} gwei: {hot}  ({:.2}% of total)",
        x as f64 / 1e9,
        100.0 * hot as f64 / N as f64
    );
    println!(
        "top_k({K}) time     : {top_dur:?}  ({} results returned)",
        top.len()
    );
    println!(
        "top-5 nets         : {:?}",
        top.iter()
            .take(5)
            .map(|&i| idx.net(i, x))
            .collect::<Vec<_>>()
    );
}
