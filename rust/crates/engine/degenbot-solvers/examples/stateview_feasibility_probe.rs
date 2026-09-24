// Throwaway spike harness (StateView mechanism feasibility).
// Lives only for this spike; deleted after supervisor sign-off per
// docs/architecture/stateview-feasibility.md §7. Lint-exempt wholesale: it is
// diagnostic-only and never ships.
#![expect(
    clippy::print_stdout,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::explicit_into_iter_loop,
    clippy::uninlined_format_args
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::aliases::U112;
use alloy::primitives::{U128, U256};
use degenbot_pools::state_history::{
    ReorgJournal, ScalarPriors, TickBefore, V2BlockDelta, V3BlockDelta,
};
use degenbot_pools::TickInfo;

fn percentile(mut v: Vec<f64>, q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.is_empty() {
        return 0.0;
    }
    let idx = (((v.len() as f64) * q).ceil() as usize)
        .saturating_sub(1)
        .min(v.len() - 1);
    v[idx]
}
fn p50(v: Vec<f64>) -> f64 {
    percentile(v, 0.5)
}
fn p90(v: Vec<f64>) -> f64 {
    percentile(v, 0.9)
}
fn p99(v: Vec<f64>) -> f64 {
    percentile(v, 0.99)
}
fn pmax(v: Vec<f64>) -> f64 {
    percentile(v, 1.0)
}

fn rss_kb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let kb = s
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|x| x.parse::<u64>().ok())
        .unwrap_or(0);
    (kb * 4) as f64
}

fn make_ticks(n: usize) -> HashMap<i32, TickInfo> {
    (0..n)
        .map(|i| {
            let tick = (i as i32).wrapping_mul(61);
            (
                tick,
                TickInfo {
                    liquidity_gross: U128::from(i as u64),
                    liquidity_net: i128::from(i as i64),
                    block: 25_900_000u64,
                },
            )
        })
        .collect()
}

fn jade_journal(k: usize) -> ReorgJournal<V3BlockDelta> {
    let mut j: ReorgJournal<V3BlockDelta> = ReorgJournal::new(32);
    for block in 25_900_000u64..25_900_000u64 + 32 {
        j.push_delta(V3BlockDelta {
            block,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: U256::from(1u8),
                liquidity_before: 1u128,
                tick_before: 0,
            }),
            update_block_before: Some(block),
            tick_data_block_before: Some(block),
            tick_priors: (0..k)
                .map(|t| {
                    (
                        (block as i32).wrapping_add(t as i32).wrapping_mul(61),
                        TickBefore {
                            liquidity_gross_before: Some(U128::from(t as u64)),
                            liquidity_net_before: 1,
                        },
                    )
                })
                .collect(),
        });
    }
    j
}

fn apply_v3(
    target: &mut HashMap<i32, TickInfo>,
    res: &degenbot_pools::state_history::V3RestoreResult,
) {
    if let Some(p) = res.scalar_priors {
        std::hint::black_box((p.sqrt_price_x96_before, p.liquidity_before, p.tick_before));
    }
    for (tick, before) in &res.tick_priors {
        match before.liquidity_gross_before {
            Some(lg) => {
                target.entry(*tick).or_insert(TickInfo {
                    liquidity_gross: lg,
                    liquidity_net: before.liquidity_net_before,
                    block: 0,
                });
            }
            None => {
                target.remove(tick);
            }
        }
    }
}

fn main() {
    println!("## M1: tickmap HashMap<i32,TickInfo> clone-cost sweep (release, 1001 reps)");
    println!(
        "{:>8} {:>12} {:>12} {:>12} {:>12} {:>11}",
        "size", "p50_ns", "p90_ns", "p99_ns", "max_ns", "bytes/entry"
    );
    for &n in &[
        2usize, 4, 16, 64, 256, 682, 1536, 7394, 32768, 65536, 131072, 262144,
    ] {
        let map = make_ticks(n);
        for _ in 0..64 {
            std::hint::black_box(map.clone());
        }
        let mut reps = Vec::with_capacity(1001);
        for _ in 0..1001 {
            let t0 = Instant::now();
            std::hint::black_box(map.clone());
            reps.push(t0.elapsed().as_nanos() as f64);
        }
        let (a, b, c, d) = (
            p50(reps.clone()),
            p90(reps.clone()),
            p99(reps.clone()),
            pmax(reps),
        );
        let before = rss_kb();
        let mut keeps: Vec<HashMap<i32, TickInfo>> = Vec::with_capacity(128);
        for _ in 0..128 {
            keeps.push(map.clone());
        }
        let after = rss_kb();
        std::hint::black_box(&keeps);
        let bpe = if n > 0 {
            ((after - before).max(0.0) * 1024.0) / (128.0 * n as f64)
        } else {
            0.0
        };
        println!(
            "{:>8} {:>12.0} {:>12.0} {:>12.0} {:>12.0} {:>11.1}",
            n, a, b, c, d, bpe
        );
    }

    println!(
        "\n## M2: V3 journal-replay materialization at journal depth 32 (restore + reverse-apply)"
    );
    for &k in &[0usize, 1, 2, 4, 8] {
        let mut target = make_ticks(682);
        let mut pre: Vec<ReorgJournal<V3BlockDelta>> = (0..1001).map(|_| jade_journal(k)).collect();
        let mut reps = Vec::with_capacity(1001);
        for mut jj in pre.drain(..) {
            let t0 = Instant::now();
            let res = jj.restore_before_block(25_900_000);
            apply_v3(&mut target, &res);
            reps.push(t0.elapsed().as_nanos() as f64);
        }
        println!(
            "k_priors/block={:>2}  full 32-block replay+apply: p50={:>9.2}us p90={:>9.2}us p99={:>9.2}us  per-block~{:>7.2}us",
            k, p50(reps.clone()) / 1000.0, p90(reps.clone()) / 1000.0, p99(reps.clone()) / 1000.0, p50(reps) / 32.0 / 1000.0
        );
        std::hint::black_box(&mut target);
    }

    println!("\n## M3: V2 (full-state) journal restore + scalar-state snapshot cost");
    let journals: Vec<ReorgJournal<V2BlockDelta>> = (0..1001)
        .map(|_| {
            let mut j: ReorgJournal<V2BlockDelta> = ReorgJournal::new(32);
            for block in 25_900_000u64..25_900_000u64 + 32 {
                j.push_delta(V2BlockDelta {
                    block,
                    reserve0_before: U112::from(1000u64),
                    reserve1_before: U112::from(2000u64),
                    reserve0_after: U112::from(1001u64),
                    reserve1_after: U112::from(2001u64),
                });
            }
            j
        })
        .collect();
    let mut reps = Vec::with_capacity(1001);
    for mut j in journals.into_iter() {
        let t0 = Instant::now();
        let _ = std::hint::black_box(j.restore_before_block(25_900_000));
        reps.push(t0.elapsed().as_nanos() as f64);
    }
    println!(
        "V2 restore_before_block (pop full 32-block window): p50={:>7.1}ns p90={:>7.1}ns p99={:>7.1}ns",
        p50(reps.clone()), p90(reps.clone()), p99(reps)
    );
    #[expect(dead_code)]
    #[derive(Clone, Copy)]
    struct V2Snap {
        r0: U112,
        r1: U112,
        block: u64,
    }
    let v2s: Vec<V2Snap> = (0..100_000)
        .map(|i| V2Snap {
            r0: U112::from(i as u64),
            r1: U112::from((i as u64) + 1),
            block: 25_900_000,
        })
        .collect();
    let mut reps = Vec::with_capacity(1001);
    for _ in 0..1001 {
        let t0 = Instant::now();
        std::hint::black_box(v2s.clone());
        reps.push(t0.elapsed().as_nanos() as f64);
    }
    println!(
        "V2 100k-pool scalar-state snapshot clone (entire tracked V2 set): p50={:>8.2}us p99={:>8.2}us",
        p50(reps.clone()) / 1000.0, p99(reps) / 1000.0
    );

    println!("\n## M4: COW (Arc) view-construction cost (O(1) arc bump)");
    let map682 = Arc::new(make_ticks(682));
    let map1536 = Arc::new(make_ticks(1536));
    let mut reps = Vec::with_capacity(100_001);
    for _ in 0..100_001 {
        let t0 = Instant::now();
        std::hint::black_box(Arc::clone(&map682));
        reps.push(t0.elapsed().as_nanos() as f64);
    }
    println!(
        "Arc<HashMap> clone (682-tick pool view):  p50={:>5.1}ns p99={:>5.1}ns",
        p50(reps.clone()),
        p99(reps)
    );
    let mut reps = Vec::with_capacity(100_001);
    for _ in 0..100_001 {
        let t0 = Instant::now();
        std::hint::black_box(Arc::clone(&map1536));
        reps.push(t0.elapsed().as_nanos() as f64);
    }
    println!(
        "Arc<HashMap> clone (1536-tick pool view): p50={:>5.1}ns p99={:>5.1}ns",
        p50(reps.clone()),
        p99(reps)
    );

    println!("\n## M6: memory anchors");
    println!(
        "size_of TickInfo={} V3BlockDelta={} (i32,TickInfo)={}",
        std::mem::size_of::<TickInfo>(),
        std::mem::size_of::<V3BlockDelta>(),
        std::mem::size_of::<(i32, TickInfo)>()
    );
    let big = make_ticks(65536);
    let before = rss_kb();
    let mut keeps = Vec::with_capacity(32);
    for _ in 0..32 {
        keeps.push(big.clone());
    }
    let after = rss_kb();
    std::hint::black_box(&keeps);
    println!(
        "65536-tick map clone RSS delta: {:.1} bytes/entry",
        ((after - before).max(0.0) * 1024.0) / (32.0 * 65536.0)
    );
}
