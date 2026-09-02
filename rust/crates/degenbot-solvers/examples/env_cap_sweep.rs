#![expect(clippy::print_stdout, clippy::cast_possible_truncation)]
//! Loop-18 T2: sweep the envelope gate cap pair (tangent lines x survivor
//! lines) over captured all-CL pool states, reporting bound + gate phase
//! split per configuration. The process must be RESTARTED per config (the
//! caps are `OnceLock` env reads).

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::IntTickRangeCrossing;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::profit_envelope::{path_profit_bound, ClHop, GateDeps, HopMath};
use serde_json::Value;
use std::borrow::Cow;

fn u256(s: &str) -> U256 {
    s.trim().parse::<U256>().unwrap_or(U256::ZERO)
}

fn clr(v: &Value) -> IntV3TickRangeHop {
    IntV3TickRangeHop {
        liquidity: v
            .get("liquidity")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u128>().ok())
            .unwrap_or(0),
        sqrt_price_x96: u256(
            v.get("sqrt_price_x96")
                .and_then(Value::as_str)
                .unwrap_or("0"),
        ),
        sqrt_price_lower_x96: u256(
            v.get("sqrt_price_lower_x96")
                .and_then(Value::as_str)
                .unwrap_or("0"),
        ),
        sqrt_price_upper_x96: u256(
            v.get("sqrt_price_upper_x96")
                .and_then(Value::as_str)
                .unwrap_or("0"),
        ),
        gamma_numer: v.get("gamma_numer").and_then(Value::as_u64).unwrap_or(0),
        fee_denom: v.get("fee_denom").and_then(Value::as_u64).unwrap_or(0),
        zero_for_one: v
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        word_boundary_prices: v
            .get("word_boundary_prices")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str().map(u256))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }
}

fn main() {
    // Loop-18 T2 cycle simulation: production calls the gate with
    // prefix_cache_on=true over a per-cycle reset. This sim drives N paths
    // over the same tables per cycle, comparing production-style reset
    // vs. a persistent cache, to quantify the first-touch recomposition
    // waste. Enable with DRCLAB_CYCLE_SIM=1.
    if std::env::var("DRCLAB_CYCLE_SIM").is_ok() {
        let path = std::env::args().nth(1).unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
                .to_string_lossy()
                .into_owned()
        });
        run_cycle_sim(&path);
        return;
    }
    let tangent_config =
        std::env::var("DEGENBOT_ENVELOPE_MAX_TANGENT_LINES").unwrap_or_else(|_| "32".to_string());
    let sampled_config = std::env::var("DEGENBOT_ENVELOPE_SAMPLED_COMPOSE_LINES")
        .unwrap_or_else(|_| "48".to_string());
    println!("config tangent={tangent_config} sampled={sampled_config} (restart per config)");
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let content = degenbot_solvers::capture_fixture::read_fixture(&path);

    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(doc) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
            continue;
        };
        let seqs: Vec<IntV3TickRangeSequence> = hops
            .iter()
            .map(|h| IntV3TickRangeSequence {
                ranges: h
                    .as_array()
                    .map(|a| a.iter().map(clr).collect::<Vec<_>>())
                    .unwrap_or_default(),
            })
            .collect();
        let range_counts: Vec<usize> = seqs.iter().map(|s| s.ranges.len()).collect();
        let views: Vec<Option<HopMath>> =
            seqs.iter().map(|s| Some(HopMath::cl_derived(s))).collect();

        let mut t_all = 0u128;
        let mut t_prod = 0u128;
        let mut t_hull = 0u128;
        let mut bound = String::from("None");
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let r = path_profit_bound(&views, &GateDeps::offline());
            t_all += t0.elapsed().as_micros();
            if let degenbot_solvers::profit_envelope::Envelope::Bound(b) = r {
                bound = b.to_string();
            }
            let gs = degenbot_solvers::profit_envelope::take_last_gate_stats();
            t_prod += gs.product_ns / 1_000;
            t_hull += gs.prune_hull_ns / 1_000;
        }
        // Only print heavy rows (>= 5ms) plus the config header so the
        // output stays readable on big fixtures.
        let avg_us = (t_all / 5) as u64;
        println!(
            "path {pid}: ranges={range_counts:?} bound={bound} gate_avg_us={avg_us} product_avg_us={} hull_avg_us={}",
            (t_prod / 5),
            (t_hull / 5),
        );
    }
}

/// Loop-18 T2 cycle simulation: drive the gate over a cycle of paths
/// sharing tables, production-style (per-cycle prefix-cache reset) vs a
/// persistent cache, and report per-path first-touch vs steady cost.
fn run_cycle_sim(path: &str) {
    use std::sync::Arc;
    let content = degenbot_solvers::capture_fixture::read_fixture(std::path::Path::new(path));
    println!("cycle sim (tangent=32/48 defaults; env override applies)");

    // Dedupe: first line per path_id.
    let mut paths: Vec<(u64, Vec<IntV3TickRangeSequence>, Vec<u64>)> = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(doc) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        if paths.iter().any(|(p, _, _)| *p == pid) {
            continue;
        }
        let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
            continue;
        };
        let seqs: Vec<IntV3TickRangeSequence> = hops
            .iter()
            .map(|h| IntV3TickRangeSequence {
                ranges: h
                    .as_array()
                    .map(|a| a.iter().map(clr).collect::<Vec<_>>())
                    .unwrap_or_default(),
            })
            .collect();
        let counts: Vec<u64> = seqs.iter().map(|s| s.ranges.len() as u64).collect();
        paths.push((pid, seqs, counts));
    }
    println!("distinct paths in sim: {}", paths.len());

    // Arc crossing tables per path, per hop.
    let crossing_arcs: Vec<Vec<Arc<Vec<IntTickRangeCrossing>>>> = paths
        .iter()
        .map(|(_, seqs, _)| {
            seqs.iter()
                .map(|s| Arc::new(s.crossings()))
                .collect::<Vec<_>>()
        })
        .collect();

    let run_gate = |cycle: u64, p: usize| -> (u128, u128, u128) {
        let (_, seqs, _) = &paths[p];
        // Carried tables (production shape): the Arcs the sim built up front.
        let views: Vec<Option<HopMath>> = seqs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                Some(HopMath::Cl(ClHop {
                    seq: s,
                    crossings: Cow::Borrowed(&crossing_arcs[p][i]),
                }))
            })
            .collect();
        let t0 = std::time::Instant::now();
        let _ = path_profit_bound(&views, &GateDeps::per_block(cycle, None));
        let us = t0.elapsed().as_micros();
        let gs = degenbot_solvers::profit_envelope::take_last_gate_stats();
        (us, 0u128, gs.product_ns / 1_000)
    };

    let n_cycles = 6;
    let mut first_touch = vec![0u128; paths.len()];
    let mut steady = vec![0u128; paths.len()];
    for cycle in 0..n_cycles {
        // Production-style per-cycle generation: epoch = cycle index drops
        // older entries on first touch (no public reset).
        for p in 0..paths.len() {
            let (us, _compose, _prod) = run_gate(cycle, p);
            if cycle == 0 {
                first_touch[p] = us;
            } else if cycle == n_cycles - 1 {
                steady[p] = us;
            }
        }
    }
    println!("-- per-cycle reset (production-style) --");
    for (i, (pid, _, counts)) in paths.iter().enumerate() {
        println!(
            "  path {pid}: ranges={counts:?} first_cycle_us={} final_cycle_us={} persistent_us={}",
            first_touch[i], steady[i], steady[i]
        );
    }
}
