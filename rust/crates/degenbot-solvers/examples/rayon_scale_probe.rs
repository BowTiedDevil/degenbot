// Dev/example-only harness: offline rayon scaling probe for the solver fan-out.
#![expect(
    clippy::borrow_deref_ref,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unnecessary_sort_by,
    clippy::unwrap_used
)]

//! Offline solver-parallelism scaling probe (ergo RAYPAR T1).
//!
//! Reconstructs production ResolvedMixedPath inputs (V3 CL hops carrying the
//! production build_cl_word_profiles + build_cl_crossing_table precomputes)
//! from the committed heavy-CL capture fixture and re-runs the production
//! solver entry mixed::solve_path_with_min_profit under controlled thread
//! counts. Measures wall / sum-CPU / efficiency per configuration so the
//! production drain's achieved parallelism (4.21/8 in the KGXFT7 gate-on run)
//! can be attributed to scheduling vs contention vs per-item diagnostics vs
//! intrinsic (memory-bound) behavior.
//!
//! Each path is solved at least once for a golden-lite gate: the recorded
//! capture golden profit must be reproduced within PROFIT_EPS wei, else the
//! reconstructed path is flagged and excluded (never silent).
//!
//! Usage:
//!   cargo run -p degenbot-solvers --release --example rayon_scale_probe
//!     [-- <fixture.jsonl>] [--paths N] [--serial-passes N]
//!
//! Output CSV columns:
//!   kind,label,threads,items,wall_us,sum_item_us,efficiency,item_med_us,item_p95_us

use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mixed::{solve_path_with_min_profit, ResolvedHop, ResolvedMixedPath};
use degenbot_solvers::mobius_v3_int::{
    build_cl_crossing_table, build_cl_word_profiles, reset_walk_stats, take_last_walk_stats_full,
};
use degenbot_solvers::profit_envelope::{reset_gate_stats, take_last_gate_stats};
use rayon::prelude::*;
use serde_json::Value;

const PROFIT_EPS: u128 = 100_000;
const SLOWEST_PATHS_K: usize = 10;

#[derive(Clone)]
struct Row {
    kind: &'static str,
    label: String,
    threads: usize,
    items: usize,
    wall_us: u128,
    sum_item_us: u128,
    item_med_us: u128,
    item_p95_us: u128,
}

fn pct(values: &[u128], p: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    v[((v.len() as f64) * p).floor() as usize % v.len()]
}

impl Row {
    fn efficiency(&self) -> f64 {
        if self.wall_us == 0 {
            0.0
        } else {
            self.sum_item_us as f64 / self.wall_us as f64
        }
    }

    fn print(&self) {
        println!(
            "{},{},{},{},{},{},{:.3},{},{}",
            self.kind,
            self.label,
            self.threads,
            self.items,
            self.wall_us,
            self.sum_item_us,
            self.efficiency(),
            self.item_med_us,
            self.item_p95_us
        );
    }
}

fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
        .map(String::from)
}

fn range(v: &Value) -> Result<IntV3TickRangeHop, String> {
    let wbp = v
        .get("word_boundary_prices")
        .and_then(Value::as_array)
        .ok_or("word_boundary_prices")?
        .iter()
        .map(|w| {
            w.as_str()
                .ok_or_else(|| "wbp not a string".to_string())
                .and_then(u256)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let liquidity = str_field(v, "liquidity")?
        .parse::<u128>()
        .map_err(|e| e.to_string())?;
    Ok(IntV3TickRangeHop {
        liquidity,
        sqrt_price_x96: u256(&str_field(v, "sqrt_price_x96")?)?,
        sqrt_price_lower_x96: u256(&str_field(v, "sqrt_price_lower_x96")?)?,
        sqrt_price_upper_x96: u256(&str_field(v, "sqrt_price_upper_x96")?)?,
        gamma_numer: v
            .get("gamma_numer")
            .and_then(Value::as_u64)
            .ok_or("gamma_numer")?,
        fee_denom: v
            .get("fee_denom")
            .and_then(Value::as_u64)
            .ok_or("fee_denom")?,
        zero_for_one: v
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .ok_or("zero_for_one")?,
        word_boundary_prices: wbp,
    })
}

/// Reconstruct the production resolved path (V3 hops) from one capture line.
fn reconstruct(doc: &Value) -> Result<(u64, Arc<ResolvedMixedPath>, Option<u128>), String> {
    let pid = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
    let hops_v = doc.get("hops").and_then(Value::as_array).ok_or("hops")?;
    let mut hops: Vec<ResolvedHop> = Vec::new();
    for hop in &*hops_v {
        let ra = hop.as_array().ok_or("hop not an array")?;
        if ra.is_empty() {
            return Err("empty hop".into());
        }
        let ranges = ra.iter().map(range).collect::<Result<Vec<_>, String>>()?;
        let seq = IntV3TickRangeSequence { ranges };
        hops.push(ResolvedHop::V3 {
            word_profiles: Arc::from(build_cl_word_profiles(&seq)),
            crossing_table: Arc::from(build_cl_crossing_table(&seq)),
            int_seq: seq,
        });
    }
    let golden = doc.get("golden").filter(|g| !g.is_null()).and_then(|g| {
        let opt = g
            .get("optimal_input")?
            .as_str()
            .and_then(|s| s.parse::<U256>().ok())?;
        let out = g
            .get("hop_outputs")?
            .as_array()?
            .last()?
            .as_str()
            .and_then(|s| s.parse::<U256>().ok())?;
        u128::try_from(out.checked_sub(opt)?).ok()
    });
    Ok((
        pid,
        Arc::new(ResolvedMixedPath {
            hops,
            valid: true,
            state_nonces: Vec::new(),
            max_update_block: 0,
        }),
        golden,
    ))
}

fn solve_measure(p: &Arc<ResolvedMixedPath>) -> (u128, bool) {
    let t0 = Instant::now();
    let r = solve_path_with_min_profit(
        p.as_ref(),
        U256::ZERO,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    );
    (t0.elapsed().as_micros(), r.is_some())
}

/// Rayon fan-out over the items: bare solve vs production-diagnostics closure
/// (span enter + relaxed atomics + K-slowest heap + walk/gate stat resets).
fn run_rayon(items: &[Arc<ResolvedMixedPath>], threads: usize, diag: bool) -> Row {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("probe-solve-{i}"))
        .build()
        .unwrap();
    let wall0 = Instant::now();
    let results: Vec<u128> = if diag {
        let c0 = AtomicU64::new(0);
        let c1 = AtomicU64::new(0);
        let c2 = AtomicU64::new(0);
        let c3 = AtomicU64::new(0);
        let c4 = AtomicU64::new(0);
        let c5 = AtomicU64::new(0);
        let c6 = AtomicU64::new(0);
        let c7 = AtomicU64::new(0);
        let heap: Mutex<BinaryHeap<std::cmp::Reverse<(u128, u64)>>> = Mutex::new(BinaryHeap::new());
        let span = tracing::span!(tracing::Level::TRACE, "degenbot.arb.solve");
        pool.install(|| {
            items
                .par_iter()
                .map(|p| {
                    let _g = span.enter();
                    reset_walk_stats();
                    reset_gate_stats();
                    let (us, some) = solve_measure(p);
                    c0.fetch_add(u64::try_from(us).unwrap_or(u64::MAX), Relaxed);
                    c1.fetch_add(u64::from(some), Relaxed);
                    let gs = take_last_gate_stats();
                    c2.fetch_add(gs.evaluated, Relaxed);
                    c3.fetch_add(gs.skipped, Relaxed);
                    c4.fetch_add(gs.unsupported, Relaxed);
                    let ws = take_last_walk_stats_full();
                    c5.fetch_add(u64::try_from(ws.pieces).unwrap_or(0), Relaxed);
                    c6.fetch_add(u64::try_from(ws.sims).unwrap_or(0), Relaxed);
                    c7.fetch_add(u64::try_from(ws.refine_sims).unwrap_or(0), Relaxed);
                    if let Ok(mut h) = heap.lock() {
                        let worst = h.peek().map_or(u128::MAX, |r| r.0 .0);
                        if h.len() < SLOWEST_PATHS_K || us > worst {
                            h.push(std::cmp::Reverse((us, 0)));
                            if h.len() > SLOWEST_PATHS_K {
                                h.pop();
                            }
                        }
                    }
                    us
                })
                .collect()
        })
    } else {
        pool.install(|| items.par_iter().map(|p| solve_measure(p).0).collect())
    };
    let wall_us = wall0.elapsed().as_micros();
    let sum: u128 = results.iter().sum();
    Row {
        kind: "solve",
        label: if diag {
            "rayon-diag".into()
        } else {
            "rayon-bare".into()
        },
        threads,
        items: items.len(),
        wall_us,
        sum_item_us: sum,
        item_med_us: pct(&results, 0.5),
        item_p95_us: pct(&results, 0.95),
    }
}

/// std-thread static partition (production alternative #4): contiguous or
/// LPT-balanced bins solved by scoped threads; no work stealing.
fn run_static(
    items: &[Arc<ResolvedMixedPath>],
    weights: &[u128],
    threads: usize,
    lpt: bool,
) -> Row {
    let n = items.len();
    let bins: Vec<Vec<usize>> = if lpt {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_unstable_by_key(|&i| std::cmp::Reverse(weights[i]));
        let mut loads = vec![0u128; threads];
        let mut bins = vec![Vec::new(); threads];
        for i in idx {
            let mut mi = 0;
            for (j, l) in loads.iter().enumerate() {
                if *l < loads[mi] {
                    mi = j;
                }
            }
            bins[mi].push(i);
            loads[mi] += weights[i];
        }
        bins
    } else {
        let mut bins = vec![Vec::new(); threads];
        for (i, bin) in bins.iter_mut().enumerate() {
            bin.extend((0..n).filter(|j| j % threads == i));
        }
        bins
    };
    let wall0 = Instant::now();
    let (all, sum) = std::thread::scope(|s| {
        let handles: Vec<_> = bins
            .iter()
            .map(|bin| {
                s.spawn(move || {
                    let mut out = Vec::with_capacity(bin.len());
                    for &i in bin {
                        out.push(solve_measure(&items[i]).0);
                    }
                    out
                })
            })
            .collect();
        let mut all = Vec::with_capacity(n);
        let mut sum = 0u128;
        for h in handles {
            for x in h.join().unwrap() {
                sum += x;
                all.push(x);
            }
        }
        (all, sum)
    });
    let wall_us = wall0.elapsed().as_micros();
    Row {
        kind: "solve",
        label: if lpt {
            "std-lpt".into()
        } else {
            "std-contig".into()
        },
        threads,
        items: n,
        wall_us,
        sum_item_us: sum,
        item_med_us: pct(&all, 0.5),
        item_p95_us: pct(&all, 0.95),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
        .to_string_lossy()
        .into_owned();
    let mut cap = usize::MAX;
    let mut serial_passes = 2usize;
    let mut bare_only = false;
    let mut skip_control = false;
    let mut skip_gate = false;
    let mut ns_raw: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--paths" => {
                cap = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(usize::MAX);
                i += 2;
            }
            "--serial-passes" => {
                serial_passes = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(2);
                i += 2;
            }
            "--bare-only" => {
                bare_only = true;
                i += 1;
            }
            "--skip-gate" => {
                skip_gate = true;
                i += 1;
            }
            "--skip-control" => {
                skip_control = true;
                i += 1;
            }
            "--ns" => {
                ns_raw = args.get(i + 1).cloned();
                i += 2;
            }
            other if !other.starts_with("--") => {
                fixture = other.to_string();
                i += 1;
            }
            _ => i += 1,
        }
    }
    let thread_counts: Vec<usize> = ns_raw
        .as_deref()
        .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 3, 4, 6, 8]);
    let static_counts: Vec<usize> = ns_raw
        .as_deref()
        .map(|s| {
            s.split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .filter(|&n| n >= 2)
                .collect()
        })
        .unwrap_or_else(|| vec![4, 8]);
    eprintln!(
        "thread_counts={:?} static_counts={:?} bare_only={} skip_control={}",
        thread_counts, static_counts, bare_only, skip_control
    );
    let content = std::fs::read_to_string(&fixture).unwrap_or_else(|e| {
        eprintln!("cannot read {fixture}: {e}");
        std::process::exit(2);
    });
    let mut items: Vec<Arc<ResolvedMixedPath>> = Vec::new();
    let mut golden: Vec<Option<u128>> = Vec::new();
    let mut pids: Vec<u64> = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        if items.len() >= cap {
            break;
        }
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad capture line: {e}");
                continue;
            }
        };
        match reconstruct(&doc) {
            Ok((pid, p, gp)) => {
                items.push(p);
                golden.push(gp);
                pids.push(pid);
            }
            Err(e) => eprintln!("path {pid:?}: skip ({e})", pid = doc.get("path_id")),
        }
    }
    eprintln!(
        "
loaded {} paths",
        items.len()
    );

    // Golden-lite gate: solve each path once serially; flag mismatches.
    // (--skip-gate folds this into the serial calibration pass below)
    let mut bad = vec![false; items.len()];
    let mut golden_ok = 0usize;
    if !skip_gate {
        for (idx, p) in items.iter().enumerate() {
            let r = solve_path_with_min_profit(
                p.as_ref(),
                U256::ZERO,
                &::degenbot_solvers::profit_envelope::GateDeps::offline(),
            );
            match (&r, golden[idx]) {
                (Some(res), Some(gp)) => {
                    let rp = u128::try_from(res.profit).expect("profit fits u128");
                    let diff = rp.abs_diff(gp);
                    if diff <= PROFIT_EPS {
                        golden_ok += 1;
                    } else {
                        eprintln!(
                            "path {}: GOLDEN MISMATCH replay={rp} golden={gp} diff={diff}",
                            pids[idx]
                        );
                        bad[idx] = true;
                    }
                }
                (Some(_), None) => {}
                (None, Some(gp)) => {
                    eprintln!("path {}: golden={gp} but replay=None — excluded", pids[idx]);
                    bad[idx] = true;
                }
                (None, None) => {}
            }
        }
        eprintln!(
            "golden gate: {golden_ok}/{} ({} excluded)",
            items.len(),
            bad.iter().filter(|b| **b).count()
        );
    } // end !skip_gate

    // Serial calibration: per-path samples across passes (weights for LPT + baseline).
    let mut serial_samples: Vec<Vec<u128>> = vec![Vec::new(); items.len()];
    for pass in 0..serial_passes {
        for (idx, p) in items.iter().enumerate() {
            if bad[idx] {
                continue;
            }
            serial_samples[idx].push(solve_measure(p).0);
        }
        eprintln!("serial pass {pass}/{serial_passes}");
    }
    let meds: Vec<u128> = serial_samples.iter().map(|v| pct(v, 0.5)).collect();
    let serial_sum: u128 = meds.iter().sum();
    eprintln!("serial medians: sum={serial_sum}us items={}", meds.len());
    let mut top: Vec<(u128, u64)> = meds.iter().copied().zip(pids.iter().copied()).collect();
    top.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    eprintln!("top-10 per-path medians: {:?}", &top[..10.min(top.len())]);
    eprintln!(
        "tail stats: max={}us p95={}us sum/top8_share={:.2}",
        top[0].0,
        pct(&meds, 0.95),
        top.iter().take(8).map(|(w, _)| *w).sum::<u128>() as f64 / serial_sum as f64
    );

    let good: Vec<Arc<ResolvedMixedPath>> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| !bad[*i])
        .map(|(_, p)| p.clone())
        .collect();
    let meds_good: Vec<u128> = meds
        .iter()
        .enumerate()
        .filter(|(i, _)| !bad[*i])
        .map(|(_, m)| *m)
        .collect();
    // LPT lower bound: optimal makespan >= max(load of LPT bins, longest item).
    let mut rows: Vec<Row> = Vec::new();
    rows.push(Row {
        kind: "solve",
        label: "serial-median".into(),
        threads: 1,
        items: good.len(),
        wall_us: serial_sum,
        sum_item_us: serial_sum,
        item_med_us: pct(&meds_good, 0.5),
        item_p95_us: pct(&meds_good, 0.95),
    });
    for &n in &thread_counts {
        rows.push(run_rayon(&good, n, false));
        if !bare_only {
            rows.push(run_rayon(&good, n, true));
        }
    }
    for &n in &static_counts {
        rows.push(run_static(&good, &meds_good, n, false));
        rows.push(run_static(&good, &meds_good, n, true));
    }
    // Control: pure-CPU spin through the same pools (machine scheduling baseline).
    if !skip_control {
        let spin_calib = {
            let t0 = Instant::now();
            let mut s = 0u64;
            for _ in 0..1_000_000 {
                s = s.wrapping_mul(31).wrapping_add(7);
            }
            let us = t0.elapsed().as_micros();
            std::hint::black_box(s);
            ((2_200_000_000.0f64) / (us as f64)) as u64
        };
        for n in [1usize, 2, 3, 4, 6, 8usize] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .unwrap();
            let wall0 = Instant::now();
            let per: Vec<u128> = pool.install(|| {
                (0..good.len())
                    .into_par_iter()
                    .map(|p| {
                        let t0 = Instant::now();
                        let mut s = p as u64;
                        for _ in 0..spin_calib {
                            s = s.wrapping_mul(31).wrapping_add(7);
                        }
                        std::hint::black_box(s);
                        t0.elapsed().as_micros()
                    })
                    .collect()
            });
            let wall_us = wall0.elapsed().as_micros();
            let sum: u128 = per.iter().sum();
            rows.push(Row {
                kind: "control-spin",
                label: "rayon".into(),
                threads: n,
                items: good.len(),
                wall_us,
                sum_item_us: sum,
                item_med_us: pct(&per, 0.5),
                item_p95_us: pct(&per, 0.95),
            });
        }
    }
    println!("kind,label,threads,items,wall_us,sum_item_us,efficiency,item_med_us,item_p95_us");
    for r in &rows {
        r.print();
    }
}
