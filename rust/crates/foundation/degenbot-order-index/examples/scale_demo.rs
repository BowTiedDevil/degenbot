//! Scale demonstration for the [`EnvelopeIndex`].
//!
//! Simulates the end-state load: hundreds of thousands to millions of stored
//! path results, ordered by net profit under a per-block gas price `X`. Most
//! results are interior to the `(gas, gross)` envelope (dominated or far below
//! the top-K), so the hot/cold split collapses the set to a tiny hot set.
//!
//! Run: `cargo run -p degenbot-order-index --example scale_demo --release`

// Demo-only casts (f64 sqrt for point generation) — fine outside prod math.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use alloy_primitives::U256;
use std::time::Instant;

use degenbot_order_index::{EnvelopeIndex, OrderIndex};

fn main() {
    const N: u64 = 1_000_000;
    const K: usize = 50;
    // Concave frontier ~ A*sqrt(gas) with noise poking above — a rich hull.
    const A: i128 = 1_000_000_000_000_000_000;

    // Deterministic pseudo-random LCG (demo only).
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    let build_t0 = Instant::now();
    let mut idx = EnvelopeIndex::<u64>::new();
    for id in 0..N {
        let gas = 21_000u64 + next() % 8_000_000_000;
        let sqrt = (gas as f64).sqrt() as i128; // demo: f64 is fine here
        let base = A * sqrt;
        let signed = i128::from(next() % 21); // 0..=20
        let noise = (signed - 10) * 10; // -100..=+100
        let gross = base + base * noise / 1000; // always positive in this demo
        idx.insert(id, U256::from(gas), U256::from(gross as u128));
    }
    let build = build_t0.elapsed();

    let x = U256::from(80_000_000_000u64); // 80 gwei
    let hot = idx.hot_len(x, K);
    let t_top = Instant::now();
    let top = idx.top_k(x, K);
    let top_dur = t_top.elapsed();

    println!("== scale demo: {N} candidates, K={K} ==");
    println!("build+index        : {build:?}");
    println!("hull vertices      : {}", idx.hull_len());
    println!(
        "hot set @ {:.0} gwei: {hot}  ({:.2}% of total)",
        (x.to::<u128>() as f64) / 1e9,
        100.0 * hot as f64 / N as f64
    );
    println!(
        "top_k({K}) time     : {top_dur:?}  ({} results returned)",
        top.len()
    );
    println!("top-5 ids          : {top:?}");
}
