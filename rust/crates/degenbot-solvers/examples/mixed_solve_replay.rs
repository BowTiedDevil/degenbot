// Dev/example-only harness: offline replay + profiler for captured *mixed*
// V2+CL solver fixtures. Pedantic lints production code denies are relaxed.
#![expect(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::map_unwrap_or,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_cast
)]

//! Offline heavy mixed V2+CL path replay harness with N-repeat stable timing.
//! Mirrors `cl_solve_replay.rs` for paths that dispatch to
//! `exact_solve_mixed_path_n_cached` (V2→V3→V3, V2→V3→V4, …).
//!
//! Usage:
//!   `cargo run -p degenbot-solvers --example mixed_solve_replay -- [<capture.jsonl>]`
//!   `DR_REPLAY_ITERS=25`   // more reps for tighter p95 (default 9)
//!
//! Re-reads a `heavy_mixed_solve_captures.jsonl` produced by the live hook
//! (`solver_dispatch` `DEGENBOT_SOLVER_CAPTURE=1`), rebuilds
//! `IntHopState` per V2 hop + `IntV3TickRangeSequence` per CL hop from the
//! captured fields, and re-runs `exact_solve_mixed_path_n_cached` — the exact
//! decomposed call `mixed::solve_mixed_path_int` makes — OFFLINE, with no
//! bot / RPC / DB. Each path is solved N times; the median / p95 / min wall
//! time is the stable A/B signal. Per-run nondeterminism is flagged (the solver
//! is pure math).
//!
//! # Bottleneck-localization note
//!
//! The capture records `measured.time_us` for the FULL `solve_path_with_min`
//! call (profit-envelope gate + resolve + the decomposed solver). This harness
//! re-runs only the decomposed solver entry
//! (`exact_solve_mixed_path_n_cached`, no crossings/profiles → rebuilt per
//! call). If the replay time ≪ `measured.time_us`, the bottleneck is NOT the
//! active-set walk — it lives in the profit-envelope gate or the resolve phase
//! (which run before the decomposed solver). The walk-stats line
//! (`pieces/sims/steps/refine`) then confirms: zeros mean the walk never ran
//! (gate skipped), which localizes the cost to the gate.
//!
//! Two-sided golden gate: exits 1 (so CI blocks merge) if a non-null golden is
//! under-shot past `PROFIT_EPS` or over-shot past `OVER_SHOOT_TOLERANCE_WEI`,
//! or if reps disagree.

use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mobius_v3_int::{
    exact_solve_mixed_path_n_cached, reset_walk_stats, take_last_walk_stats_full, WalkStats,
};
use serde_json::Value;

const PROFIT_EPS: u128 = 100_000;
const OVER_SHOOT_TOLERANCE_WEI: u128 = 8;

fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}

fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
        .map(String::from)
}

/// Parse one CL range (same shape as `cl_solve_replay::range`).
fn cl_range(v: &Value) -> Result<IntV3TickRangeHop, String> {
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
    Ok(IntV3TickRangeHop {
        liquidity: str_field(v, "liquidity")?
            .parse::<u128>()
            .map_err(|e| e.to_string())?,
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

/// Parse the captured per-hop discriminant into the decomposed-solver inputs.
/// Returns `(v2_hops, cl_sequences, hop_order)`.
fn parse_path(
    doc: &Value,
) -> Result<
    (
        Vec<Option<IntHopState>>,
        Vec<Option<IntV3TickRangeSequence>>,
        Vec<bool>,
    ),
    String,
> {
    let hops = doc.get("hops").and_then(Value::as_array).ok_or("hops")?;
    let hop_order: Vec<bool> = doc
        .get("hop_order")
        .and_then(Value::as_array)
        .ok_or("hop_order")?
        .iter()
        .map(|b| b.as_bool().unwrap_or(false))
        .collect();
    if hop_order.len() != hops.len() {
        return Err(format!(
            "hop_order len {} != hops len {}",
            hop_order.len(),
            hops.len()
        ));
    }
    let mut v2_hops = Vec::with_capacity(hops.len());
    let mut cl_seqs = Vec::with_capacity(hops.len());
    for hop in hops {
        let kind = hop.get("kind").and_then(Value::as_str).ok_or("kind")?;
        match kind {
            "V2" => {
                // Captured gamma/fee as decimal strings (U256 in the live
                // struct); IntHopState::new takes u64 (V2 fees always fit).
                let g = str_field(hop, "gamma_numer")?
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                let d = str_field(hop, "fee_denom")?
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                v2_hops.push(Some(IntHopState::new(
                    u256(&str_field(hop, "reserve_in")?)?,
                    u256(&str_field(hop, "reserve_out")?)?,
                    g,
                    d,
                )));
                cl_seqs.push(None);
            }
            "CL" => {
                let ranges = hop
                    .get("ranges")
                    .and_then(Value::as_array)
                    .ok_or("ranges")?
                    .iter()
                    .map(cl_range)
                    .collect::<Result<Vec<_>, _>>()?;
                v2_hops.push(None);
                cl_seqs.push(Some(IntV3TickRangeSequence { ranges }));
            }
            other => return Err(format!("unknown hop kind {other}")),
        }
    }
    Ok((v2_hops, cl_seqs, hop_order))
}

fn pct(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_mixed_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let iters: usize = std::env::var("DR_REPLAY_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9)
        .max(1);
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(2);
    });

    let mut n_paths = 0u64;
    let mut n_golden_ok = 0u64;
    let mut n_golden_null = 0u64;
    let mut n_divergent = 0u64;
    let mut heaviest: (u128, u64) = (0, 0); // (median_us, path_id)
    let mut fail = false;

    println!("mixed_solve_replay: {iters} reps/path, fixture={path}");
    println!(
        "{:>10} {:>6} {:>4} {:>10} {:>10} {:>10} {:>10} {:>14} {:>24}",
        "path_id",
        "n_hops",
        "ord",
        "min_us",
        "med_us",
        "p95_us",
        "live_us",
        "walk(p/s/st/r)",
        "status"
    );

    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad capture line: {e}");
                continue;
            }
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let n_hops = doc.get("n_hops").and_then(Value::as_u64).unwrap_or(0);
        let live_us: u128 = doc
            .get("measured")
            .and_then(|m| m.get("time_us"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u128;
        let order_str: String = doc
            .get("hop_order")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|b| {
                        if b.as_bool().unwrap_or(false) {
                            'V'
                        } else {
                            'C'
                        }
                    })
                    .collect::<String>()
            })
            .unwrap_or_else(|| "?".into());

        let (v2_hops, cl_seqs, hop_order) = match parse_path(&doc) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("path {pid}: skip ({e})");
                continue;
            }
        };

        // Warm + time N reps. Reset walk stats so each rep's stats are
        // attributable (we report the final rep's walk breakdown).
        let mut times_us: Vec<u128> = Vec::with_capacity(iters);
        let mut last_ws: WalkStats = take_last_walk_stats_full();
        let mut last_result: Option<(U256, U256, Vec<U256>)> = None;
        let mut divergent_reps = false;
        for _ in 0..iters {
            reset_walk_stats();
            let t0 = std::time::Instant::now();
            let r = exact_solve_mixed_path_n_cached(&v2_hops, &cl_seqs, None, None, &hop_order);
            let us = t0.elapsed().as_micros() as u128;
            times_us.push(us);
            last_ws = take_last_walk_stats_full();
            if let Some(prev) = last_result.as_ref() {
                if prev.1 != r.as_ref().map(|(_, p, _)| *p).unwrap_or(U256::ZERO) {
                    divergent_reps = true;
                }
            }
            last_result = r;
        }
        times_us.sort_unstable();
        let min_us = *times_us.first().unwrap_or(&0);
        let med_us = pct(&times_us, 0.5);
        let p95_us = pct(&times_us, 0.95);
        if med_us > heaviest.0 {
            heaviest = (med_us, pid);
        }
        if divergent_reps {
            n_divergent += 1;
        }

        // Two-sided golden gate (only when the live solve found profit).
        let golden = doc.get("golden").cloned().unwrap_or(Value::Null);
        let status = if golden.is_null() {
            n_golden_null += 1;
            // No golden to assert — but still report timing + walk stats, the
            // primary goal for non-profitable heavy paths like path 7042.
            if last_result.is_none() {
                "ok(no-profit)"
            } else {
                "ok(no-golden)"
            }
            .to_string()
        } else {
            let g_profit = u256(golden.get("profit").and_then(Value::as_str).unwrap_or("0"))
                .unwrap_or(U256::ZERO);
            let r_profit = last_result
                .as_ref()
                .map(|(_, p, _)| *p)
                .unwrap_or(U256::ZERO);
            let delta_wei: i128 = if r_profit >= g_profit {
                (r_profit - g_profit).try_into().unwrap_or(i128::MAX)
            } else {
                -((g_profit - r_profit).try_into().unwrap_or(i128::MAX))
            };
            if delta_wei < -(PROFIT_EPS as i128) {
                n_divergent += 1;
                fail = true;
                let neg = -delta_wei;
                format!("FAIL under {neg}")
            } else if delta_wei > OVER_SHOOT_TOLERANCE_WEI as i128 {
                n_divergent += 1;
                fail = true;
                format!("FAIL over +{delta_wei}")
            } else if divergent_reps {
                fail = true;
                "FAIL nondeterministic".to_string()
            } else {
                n_golden_ok += 1;
                "ok".to_string()
            }
        };

        n_paths += 1;
        println!(
            "{pid:>10} {n_hops:>6} {order_str:>4} {min_us:>10} {med_us:>10} {p95_us:>10} {live_us:>10} ({}/{}/{}/{}) {status:>24}",
            last_ws.pieces, last_ws.sims, last_ws.word_steps, last_ws.refine_sims,
        );
    }

    println!(
        "\npaths={n_paths} golden_ok={n_golden_ok} golden_null={n_golden_null} divergent={n_divergent} heaviest=path {hid} @ {us}us med",
        hid = heaviest.1,
        us = heaviest.0,
    );
    if fail {
        std::process::exit(1);
    }
}
