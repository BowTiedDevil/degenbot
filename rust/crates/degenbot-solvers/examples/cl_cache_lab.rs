//! Cache-lab driver (epic KIMRKS): replay a capture JSONL as a command of
//! deterministic pool-state transitions and solve every state through every
//! registered cache strategy, checking byte-equality against the full-rebuild
//! reference at each epoch and printing the rebuild-cost matrix.
//!
//! Usage:
//!   cargo run -p degenbot-solvers --example cl_cache_lab -- [<capture.jsonl>]
//!   DRCLAB_MAX_PATHS=4 DRCLAB_TRANS=8 ...
//!
//! Exit 1 if any strategy's solution diverges from the reference on any
//! transitioned state (exact accuracy — no tolerance).

#![expect(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::too_many_lines
)]

extern crate degenbot_solvers;

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;
use degenbot_solvers::cl_cache::{strategy_catalog, CacheEvent, ClCacheStrategy, PreparedHop};
use degenbot_solvers::mobius_v3_int::int_solve_cl_path;
use serde_json::Value;

fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

fn str_of<'a>(item: &'a Value, k: &str) -> Result<&'a str, String> {
    item.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
}

fn parse_hop(v: &Value) -> Result<IntV3TickRangeSequence, String> {
    let arr = v.as_array().ok_or("hop not an array")?;
    let mut ranges = Vec::with_capacity(arr.len());
    for item in arr {
        let liquidity: u128 = str_of(item, "liquidity")?
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        let wbp: Vec<U256> = item
            .get("word_boundary_prices")
            .and_then(Value::as_array)
            .ok_or("missing word_boundary_prices")?
            .iter()
            .map(|w| {
                w.as_str()
                    .ok_or_else(|| "wbp not a string".to_string())
                    .and_then(u256)
            })
            .collect::<Result<Vec<_>, String>>()?;
        ranges.push(degenbot_pools::int_v3_hop::IntV3TickRangeHop {
            liquidity,
            sqrt_price_x96: u256(str_of(item, "sqrt_price_x96")?)?,
            sqrt_price_lower_x96: u256(str_of(item, "sqrt_price_lower_x96")?)?,
            sqrt_price_upper_x96: u256(str_of(item, "sqrt_price_upper_x96")?)?,
            gamma_numer: item
                .get("gamma_numer")
                .and_then(Value::as_u64)
                .ok_or("missing gamma_numer")?,
            fee_denom: item
                .get("fee_denom")
                .and_then(Value::as_u64)
                .ok_or("missing fee_denom")?,
            zero_for_one: item
                .get("zero_for_one")
                .and_then(Value::as_bool)
                .ok_or("missing zero_for_one")?,
            word_boundary_prices: wbp,
        });
    }
    Ok(IntV3TickRangeSequence { ranges })
}

/// Solve through a strategy's prepared tables (production walk). Returns
/// None for the S0 sentinel (drive `int_solve_cl_path` directly).
fn solve_prepared<S: ClCacheStrategy + ?Sized>(
    strategy: &mut S,
    seqs: &[IntV3TickRangeSequence],
    event: &CacheEvent,
    seq_refs: &[&IntV3TickRangeSequence],
) -> Option<(U256, U256, Vec<U256>)> {
    let prepared: Vec<PreparedHop> = strategy.refill(seqs, event);
    if prepared.is_empty() {
        return degenbot_solvers::mobius_v3_int::solve_cl_derived(seq_refs).result;
    }
    let crossings: Vec<&std::sync::Arc<degenbot_solvers::mobius_v3_int::ClCrossingTable>> =
        prepared.iter().map(|(c, _)| c).collect();
    let profiles: Vec<&std::sync::Arc<degenbot_solvers::mobius_v3_int::ClProfileTable>> =
        prepared.iter().map(|(_, p)| p).collect();
    let prepared_hops: Vec<degenbot_solvers::mobius_v3_int::ClPrepared> = crossings
        .iter()
        .zip(profiles.iter())
        .map(|(c, p)| degenbot_solvers::mobius_v3_int::ClPrepared {
            crossings: std::sync::Arc::clone(c),
            profiles: std::sync::Arc::clone(p),
        })
        .collect();
    int_solve_cl_path(seq_refs, &prepared_hops, None).result
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let max_paths: usize = std::env::var("DRCLAB_MAX_PATHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let trans: usize = std::env::var("DRCLAB_TRANS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    let content = std::fs::read_to_string(&path).expect("capture fixture readable");
    let mut catalog = strategy_catalog();
    let mut total_mismatches: usize = 0;
    let mut n_paths = 0usize;
    // Class-indexed rebuild-delta matrix rows: [strategy][class], class 0-3 = price/liquidity/tick/restore.
    let mut class_agg: Vec<[u64; 4]> = vec![[0; 4]; catalog.len()];
    let mut class_ns: Vec<[u128; 4]> = vec![[0; 4]; catalog.len()];
    let mut reference_ns: u128 = 0;
    let mut total_events: u64 = 0;
    // Micro-bench capture (DRCLAB_MICRO=1): densest hop-0 path tables.
    let mut micro_seqs: Option<Vec<IntV3TickRangeSequence>> = None;
    let mut micro_best: usize = 0;

    for (line_no, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        if n_paths >= max_paths {
            break;
        }
        let doc: Value = serde_json::from_str(line).expect("capture line is JSON");
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let baseline: Vec<IntV3TickRangeSequence> = {
            let hops = doc.get("hops").and_then(Value::as_array).expect("hops");
            let mut seqs = Vec::new();
            for h in hops {
                match parse_hop(h) {
                    Ok(s) => seqs.push(s),
                    Err(e) => {
                        eprintln!("line {line_no} path {pid}: skip ({e})");
                        break;
                    }
                }
            }
            seqs
        };
        if baseline.is_empty() {
            continue;
        }
        if baseline
            .first()
            .is_some_and(|s| s.ranges.len() > micro_best)
        {
            micro_best = baseline[0].ranges.len();
            micro_seqs = Some(baseline.clone());
        }
        let mut seqs = baseline.clone();
        let mut seed: u64 = pid.wrapping_add(line_no as u64).wrapping_mul(0x9E37_79B9);
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            seed
        };
        let n_hops = seqs.len();
        let mut printable = Vec::with_capacity(catalog.len());
        for s in &mut catalog {
            printable.push(s.name());
        }
        println!(
            "path {pid}: hops={n_hops} range_lens={:?} transitions={trans} strategies={:?}",
            seqs.iter().map(|s| s.ranges.len()).collect::<Vec<_>>(),
            printable
        );
        let mut mismatches = vec![0u32; catalog.len()];

        for t in 0..trans {
            let r = (next() % 5) as usize;
            let hop = (next() as usize) % n_hops;
            let event = match r {
                0 => {
                    price_move(&mut seqs, hop);
                    CacheEvent::PriceMove { hop }
                }
                1 => {
                    price_move(&mut seqs, (hop + 1) % n_hops);
                    CacheEvent::PriceMove {
                        hop: (hop + 1) % n_hops,
                    }
                }
                2 => {
                    let up = next() % 2 == 0;
                    let range = liquidity_jitter(&mut seqs, hop, up);
                    CacheEvent::Liquidity { hop, range }
                }
                3 => {
                    if seqs[hop].ranges.len() > 2 {
                        window_slide(&mut seqs, hop);
                        CacheEvent::TickCross { hop }
                    } else {
                        price_move(&mut seqs, hop);
                        CacheEvent::PriceMove { hop }
                    }
                }
                _ => {
                    seqs.clone_from(&baseline);
                    CacheEvent::Restore
                }
            };
            let class: usize = match &event {
                CacheEvent::PriceMove { .. } => 0,
                CacheEvent::Liquidity { .. } => 1,
                CacheEvent::TickCross { .. } => 2,
                _ => 3,
            };
            let pre: Vec<degenbot_solvers::cl_cache::BuildCounters> =
                catalog.iter().map(|s| s.counters().clone()).collect();
            let seq_refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
            let t_ref = std::time::Instant::now();
            let reference = degenbot_solvers::mobius_v3_int::solve_cl_derived(&seq_refs).result;
            reference_ns += t_ref.elapsed().as_nanos();
            total_events += 1;
            if std::env::var("DRCLAB_DIGEST").is_ok() {
                println!("DIGEST path {pid} t={t} {reference:?}");
            }
            for (si, s) in catalog.iter_mut().enumerate() {
                let t_strat = std::time::Instant::now();
                let cached = if si == 0 {
                    // S0 IS the reference; still run its refill so the
                    // rebuild counters stay comparable across the catalog.
                    let _ = s.refill(&seqs, &event);
                    reference.clone()
                } else {
                    solve_prepared(s.as_mut(), &seqs, &event, &seq_refs)
                };
                class_ns[si][class] += t_strat.elapsed().as_nanos();
                let name = s.name();
                if cached != reference {
                    mismatches[si] += 1;
                    total_mismatches += 1;
                    eprintln!(
                        "  DIVERGENCE path {pid} t={t} strat={name}: cached={cached:?} ref={reference:?}"
                    );
                }
            }
            for (si, s) in catalog.iter().enumerate() {
                let post = s.counters();
                class_agg[si][class] = class_agg[si][class]
                    .saturating_add(post.crossing_tables.saturating_sub(pre[si].crossing_tables));
                let _ = &class_ns;
            }
        }
        println!(
            "  path {pid} mismatches={:?}",
            mismatches
                .iter()
                .zip(&catalog)
                .map(|(m, s)| (s.name(), *m))
                .collect::<Vec<_>>()
        );
        n_paths += 1;
    }

    let sim_ns = degenbot_solvers::mobius_v3_int::WALK_SIM_NS_TOTAL
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let anchor_ns = degenbot_solvers::mobius_v3_int::WALK_ANCHOR_NS_TOTAL
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let pred_ns = degenbot_solvers::mobius_v3_int::WALK_PRED_NS_TOTAL
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let solve_ns = degenbot_solvers::mobius_v3_int::WALK_SOLVE_NS_TOTAL
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let edge_ns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_EDGE_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let edge_sims = degenbot_solvers::mobius_v3_int::WALK_CENSUS_EDGE_SIMS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let edge_simns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_EDGE_SIMNS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let redge_ns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REDGE_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let redge_sims = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REDGE_SIMS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let redge_simns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REDGE_SIMNS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let dir_ns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_DIR_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let dir_sims = degenbot_solvers::mobius_v3_int::WALK_CENSUS_DIR_SIMS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let dir_simns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_DIR_SIMNS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let refine_ns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REFINE_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let refine_sims = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REFINE_SIMS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let refine_simns = degenbot_solvers::mobius_v3_int::WALK_CENSUS_REFINE_SIMNS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let ab_ns = degenbot_solvers::mobius_v3_int::WALK_ANCHOR_BUILD_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let ac_ns = degenbot_solvers::mobius_v3_int::WALK_ANCHOR_COMPOSE_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    let am_ns = degenbot_solvers::mobius_v3_int::WALK_ANCHOR_ARGMAX_NS
        .swap(0, std::sync::atomic::Ordering::Relaxed);
    println!(
        "  anchor split: build {:.1} ms | compose {:.1} ms | argmax {:.1} ms (of anchors total)",
        ab_ns as f64 / 1e6,
        ac_ns as f64 / 1e6,
        am_ns as f64 / 1e6
    );
    println!(
        "  walk-sim: {:.1} ms | anchors: {:.1} ms | predictions: {:.1} ms | solve: {:.1} ms (all strategies+reference)",
        sim_ns as f64 / 1e6,
        anchor_ns as f64 / 1e6,
        pred_ns as f64 / 1e6,
        solve_ns as f64 / 1e6
    );
    println!(
        "  sections: edge {:.1} ms ({} sims, sim {:.1} ms) | redge {:.1} ms ({} sims, sim {:.1} ms) | dir {:.1} ms ({} sims, sim {:.1} ms) | refine {:.1} ms ({} sims, sim {:.1} ms) | in-section sim total {:.1} ms",
        edge_ns as f64 / 1e6,
        edge_sims,
        edge_simns as f64 / 1e6,
        redge_ns as f64 / 1e6,
        redge_sims,
        redge_simns as f64 / 1e6,
        dir_ns as f64 / 1e6,
        dir_sims,
        dir_simns as f64 / 1e6,
        refine_ns as f64 / 1e6,
        refine_sims,
        refine_simns as f64 / 1e6,
        (edge_simns + redge_simns + dir_simns + refine_simns) as f64 / 1e6
    );
    println!(
        "---- per-transition averages over {total_events} events ----\n  reference (full build+solve): {:.2} ms/event",
        reference_ns as f64 / 1e6 / total_events.max(1) as f64
    );
    for (si, s) in catalog.iter().enumerate() {
        let tot: u128 = class_ns[si].iter().sum();
        println!(
            "  {} refill+cached_solve: {:.2} ms/event",
            s.name(),
            tot as f64 / 1e6 / total_events.max(1) as f64
        );
    }
    println!("---- class rebuild deltas ----");
    for (si, s) in catalog.iter().enumerate() {
        let [t_price, t_liq, t_tick, t_restore]: [u128; 4] = class_ns[si];
        println!(
            "  {} refill+solve ms/class: price={:.2} liquidity={:.2} tick={:.2} restore={:.2}",
            s.name(),
            t_price as f64 / 1e6,
            t_liq as f64 / 1e6,
            t_tick as f64 / 1e6,
            t_restore as f64 / 1e6
        );
        println!(
            "{} price={} liquidity={} tick={} restore={}",
            s.name(),
            class_agg[si][0],
            class_agg[si][1],
            class_agg[si][2],
            class_agg[si][3]
        );
    }
    println!("---- counters ----");
    for s in &catalog {
        let c = s.counters();
        println!(
            "{} crossing={} profiles={} seq_rebuilds={} partial={} solves={}",
            s.name(),
            c.crossing_tables,
            c.profile_tables,
            c.sequence_rebuilds,
            c.partial_rebuilds,
            c.solves
        );
    }
    if std::env::var("DRCLAB_MICRO").is_ok() {
        run_micro_bench(micro_seqs);
    }
    println!(
        "----\nreplayed {n_paths} path(s) with {trans} transitions each | total divergences {total_mismatches}"
    );
    if total_mismatches > 0 {
        eprintln!("CACHE-LAB FAILED: exact accuracy violated");
        std::process::exit(1);
    }
}

/// Loop-17 T3 micro-bench: split one walk sim (~640ns for 3 dense hops) into
/// crossing-table partition_point, compute_swap_step_v3, and Vec alloc cost.
fn run_micro_bench(micro_seqs: Option<Vec<IntV3TickRangeSequence>>) {
    use degenbot_math::cl::swap_math::compute_swap_step_v3;
    use std::hint::black_box;
    use std::time::Instant;

    const ITERS: u64 = 200_000;
    let Some(seqs) = micro_seqs else {
        println!("micro-bench: no path captured");
        return;
    };
    println!("---- micro-bench (DRCLAB_MICRO, {ITERS} iters) ----");

    // Build the tables for the densest hop once.
    let seq = &seqs[0];
    let crossings = degenbot_solvers::mobius_v3_int::build_cl_crossing_table(seq);
    let profiles =
        degenbot_solvers::mobius_v3_int::build_cl_word_profiles_from_crossings(&crossings);
    println!(
        "  hop0: {} ranges, {} word-profiles",
        crossings.len(),
        profiles.iter().filter(|p| p.is_some()).count(),
    );
    // V3WordProfile fields are private; pull step params out of the first
    // crossing's ending range instead.
    let hop = &crossings[crossings.len() / 2].ending_range;

    // A: partition_point over the crossing table (scattered query values).
    let mut x = U256::from(0x1234_5678_9abc_def0_u64);
    let t0 = Instant::now();
    for _ in 0..ITERS {
        x = x
            .wrapping_mul(U256::from(6_364_136_223_846_793_005_u64))
            .wrapping_add(U256::from(1_442_695_040_888_963_407_u64));
        let k = crossings.partition_point(|c| c.crossing_gross_input <= x);
        black_box(k);
    }
    let a_us = t0.elapsed().as_micros();

    // B: one compute_swap_step_v3 (the partial-landing unit).
    let sp = hop.sqrt_price_x96;
    let target = if hop.zero_for_one {
        hop.sqrt_price_lower_x96
    } else {
        hop.sqrt_price_upper_x96
    };
    let remaining = U256::from(10).pow(U256::from(21));
    let liquidity = hop.liquidity;
    let fee = U256::from(hop.fee_denom - hop.gamma_numer);
    let mut acc = U256::ZERO;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        if let Ok(step) = compute_swap_step_v3(
            sp,
            target,
            i128::try_from(liquidity).unwrap_or(i128::MAX),
            alloy::primitives::I256::try_from(remaining).unwrap_or(alloy::primitives::I256::MAX),
            fee,
        ) {
            acc = acc.wrapping_add(step.amount_out);
        }
    }
    let b_us = t0.elapsed().as_micros();
    black_box(acc);

    // C: the per-sim Vec pair allocation overhead (capacity 3, 3 pushes, drop).
    let t0 = Instant::now();
    for i in 0..ITERS {
        let mut vo: Vec<U256> = Vec::with_capacity(3);
        vo.push(U256::from(i as u64));
        vo.push(U256::from(i as u64 + 1));
        vo.push(U256::from(i as u64 + 2));
        let mut vl: Vec<usize> = Vec::with_capacity(3);
        vl.push(1);
        vl.push(2);
        vl.push(3);
        black_box((vo[2], vl[2]));
    }
    let c_us = t0.elapsed().as_micros();

    use degenbot_math::cl::full_math::{muldiv, muldiv_rounding_up};
    let narrow_a = U256::from(1u64) << U256::from(100);
    let narrow_b = U256::from(0xffff_ffff_ffff_ffff_u64) * U256::from(1_000_003_u64); // < 2^128
    let q96 = U256::from(1) << U256::from(96);
    let mut md_acc = U256::ZERO;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        md_acc = md_acc.wrapping_add(muldiv(narrow_a, narrow_b, q96).unwrap_or_default());
    }
    let d_us = t0.elapsed().as_micros();
    black_box(md_acc);
    let wide_a = U256::from(1u64) << U256::from(200);
    let mut md_acc2 = U256::ZERO;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        md_acc2 = md_acc2.wrapping_add(muldiv(wide_a, narrow_b, q96).unwrap_or_default());
    }
    let e_us = t0.elapsed().as_micros();
    black_box(md_acc2);
    let mut md_acc3 = U256::ZERO;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        md_acc3 =
            md_acc3.wrapping_add(muldiv_rounding_up(narrow_a, narrow_b, q96).unwrap_or_default());
    }
    let f_us = t0.elapsed().as_micros();
    black_box(md_acc3);

    println!(
        "  A crossing partition_point: {:.1} ns/op",
        a_us as f64 * 1e3 / ITERS as f64
    );
    (
        "  A crossing partition_point: {:.1} ns/op",
        a_us as f64 * 1e3 / ITERS as f64,
    );
    println!(
        "  B compute_swap_step_v3:     {:.1} ns/op",
        b_us as f64 * 1e3 / ITERS as f64
    );
    println!(
        "  D muldiv narrow (fits 256): {:.1} ns/op",
        d_us as f64 * 1e3 / ITERS as f64
    );
    println!(
        "  E muldiv wide (needs 512):  {:.1} ns/op",
        e_us as f64 * 1e3 / ITERS as f64
    );
    println!(
        "  F muldiv_rounding_up nar.:  {:.1} ns/op",
        f_us as f64 * 1e3 / ITERS as f64
    );
    println!(
        "  C vec-pair alloc:           {:.1} ns/op",
        c_us as f64 * 1e3 / ITERS as f64
    );
    println!(
        "  per-sim estimate (3 hops):  A*3 + B*~3 + C = {:.0} ns vs measured ~640 ns/sim",
        a_us as f64 * 1e3 / ITERS as f64 * 3.0
            + b_us as f64 * 1e3 / ITERS as f64 * 3.0
            + c_us as f64 * 1e3 / ITERS as f64
    );
}

/// Move hop `i`'s current-range sqrt price toward its exit boundary (midpoint
/// + 1 wei), staying strictly inside the range. Price-only event.
fn price_move(seqs: &mut [IntV3TickRangeSequence], i: usize) {
    let r = &mut seqs[i].ranges[0];
    let price = r.sqrt_price_x96;
    let exit = if r.zero_for_one {
        r.sqrt_price_lower_x96
    } else {
        r.sqrt_price_upper_x96
    };
    let mut target = price.saturating_add(exit) / U256::from(2u64);
    if target <= price || target >= exit {
        target = price.saturating_add(U256::from(1u64));
    }
    r.sqrt_price_x96 = target;
}

/// Bump liquidity of one range (a synthetic position change on the walk).
fn liquidity_jitter(seqs: &mut [IntV3TickRangeSequence], i: usize, up: bool) -> usize {
    let seq = &mut seqs[i];
    let ri = seq.ranges.len() / 2;
    let liq = seq.ranges[ri].liquidity;
    seq.ranges[ri].liquidity = if up {
        liq.saturating_add(liq / 10)
    } else {
        liq.saturating_sub(liq / 10)
    };
    ri
}

/// Slide the walked window past one crossed boundary: old range 0 is consumed,
/// the next range becomes current with its entry-boundary price.
fn window_slide(seqs: &mut [IntV3TickRangeSequence], i: usize) {
    let seq = &mut seqs[i];
    let old_lo = seq.ranges[0].sqrt_price_lower_x96;
    let old_hi = seq.ranges[0].sqrt_price_upper_x96;
    let zfo = seq.ranges[0].zero_for_one;
    let entry = if zfo { old_lo } else { old_hi };
    let mut ranges = seq.ranges.split_off(1);
    ranges[0].sqrt_price_x96 = entry;
    seq.ranges = ranges;
}
