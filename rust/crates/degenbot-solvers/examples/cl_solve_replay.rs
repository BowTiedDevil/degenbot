//! Offline heavy-CL-path replay harness with N-repeat stable timing.
//!
//! Usage:
//!   cargo run -p degenbot-solvers --example cl_solve_replay [-- <capture.jsonl>]
//!   DR_REPLAY_ITERS=25 ...   // more reps for tighter p95 (default 9)
//!
//! Re-reads a capture JSONL produced by the live hook
//! (solver_dispatch DEGENBOT_SOLVER_CAPTURE=1) or by cl_capture_gen, rebuilds
//! each Vec<IntV3TickRangeSequence> from the per-range fields, and re-runs
//! int_solve_cl_path — the production all-CL solver (the exact call
//! mixed::solve_path makes, initial input ONE) — OFFLINE, with no bot / RPC /
//! DB. Each path is solved N times; the median / p95 / min of the per-run wall
//! time is the stable A/B signal. As a bonus the N runs must agree — int_solve
//! is pure math, so per-run nondeterminism (e.g. HashMap-iteration order in the
//! active-set walk) is flagged, which also bears on the desync investigation.
//!
//! F2 two-sided golden gate: exits 1 (so CI blocks merge) if any path (a)
//! under-shoots the recorded golden profit beyond PROFIT_EPS, (b) over-shoots
//! it past the few-wei tolerance (phantom profit / stale golden, never silent),
//! or (c) is nondeterministic across reps.

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mobius_v3_int::{
    int_solve_cl_path, last_refine_sims, reset_walk_stats, take_last_walk_stats,
    take_last_word_boundary_steps,
};
use serde_json::Value;

/// Max acceptable profit under-shoot (wei) of the exact-wei golden for the
/// coarsened search to count as OK — mirrors the solver's "never under-shoot
/// the fine-grid oracle" contract at a diagnostic epsilon.
const PROFIT_EPS: u128 = 100_000;
// Two-sided golden gate (F2): under-shoot of the exact golden is bounded by
// PROFIT_EPS, and any over-shoot beyond this few-wei rounding tolerance is a
// HARD FAIL — the golden is the recorded exact result, so a solver exceeding
// it is either phantom profit (claiming profit that does not exist) or a stale
// golden needing regeneration. Never silent.
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        // CWD-independent: resolve from the crate root so the default works
        // regardless of where `cargo run --example` is launched from.
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
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
    let mut n_consistent = 0u64;
    let mut heaviest: (u128, u64) = (0, 0); // (median_us, path_id)

    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad capture line: {e}");
                continue;
            }
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops_v = match doc.get("hops").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => continue,
        };
        let mut seqs: Vec<IntV3TickRangeSequence> = Vec::new();
        let mut err = String::new();
        for hop in &hops_v {
            let ra = match hop.as_array() {
                Some(a) => a,
                None => {
                    err = "hop not an array".into();
                    break;
                }
            };
            if ra.is_empty() {
                err = "empty hop".into();
                break;
            }
            let ranges = ra
                .iter()
                .map(range)
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|e| {
                    err = e;
                    Vec::new()
                });
            if !err.is_empty() {
                break;
            }
            seqs.push(IntV3TickRangeSequence {
                ranges,
                truncated: false,
            });
        }
        if !err.is_empty() {
            eprintln!("path {pid}: skip ({err})");
            continue;
        }
        let refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();

        // N independent repetitions for a stable A/B (no single-shot noise).
        // int_solve_cl_path is pure math, so all runs must agree; per-run
        // nondeterminism (e.g. HashMap-iteration order in the active-set walk)
        // is flagged and counted separately.
        let mut times: Vec<u128> = Vec::with_capacity(iters);
        let mut first: Option<(U256, Vec<U256>)> = None;
        let mut consistent = true;
        for _ in 0..iters {
            reset_walk_stats();
            let t0 = std::time::Instant::now();
            let r = int_solve_cl_path(refs.as_slice());
            times.push(t0.elapsed().as_micros());
            match r.as_ref() {
                Some((opt, _p, ho)) => match &first {
                    None => first = Some((opt.clone(), ho.clone())),
                    Some((fo, fh)) => {
                        if fo != opt || fh != ho {
                            consistent = false;
                        }
                    }
                },
                None => {
                    if first.is_some() {
                        consistent = false;
                    }
                }
            }
        }
        let wsteps = take_last_word_boundary_steps();
        let refine = last_refine_sims();
        let (pieces, sims) = take_last_walk_stats();
        if !consistent {
            eprintln!("path {pid}: NON-DETERMINISTIC across {iters} runs — active-set walk order-dependent?");
        } else {
            n_consistent += 1;
        }
        times.sort_unstable();
        let n = times.len();
        let tmin = times[0];
        let med = times[n / 2];
        let p95 = times[((n * 94) / 100).min(n - 1)];

        // Golden check against the (first) run's result.
        let golden = doc.get("golden").cloned().unwrap_or(Value::Null);
        let replay_profitable = first.as_ref().map_or(false, |(opt, ho)| {
            !opt.is_zero()
                && ho
                    .last()
                    .copied()
                    .unwrap_or(U256::ZERO)
                    .saturating_sub(*opt)
                    != U256::ZERO
        });
        let mut ok = true;
        if golden.is_null() {
            if replay_profitable {
                eprintln!("path {pid}: replay reports profit but capture had none");
                ok = false;
            }
        } else {
            match &first {
                Some((opt, ho)) => {
                    let go = golden
                        .get("optimal_input")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let gh: Vec<String> = golden
                        .get("hop_outputs")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default();
                    let go_in: U256 = go.parse().unwrap_or(U256::ZERO);
                    let go_out = gh
                        .last()
                        .cloned()
                        .and_then(|s: String| s.parse().ok())
                        .unwrap_or(U256::ZERO);
                    let golden_profit = go_out.saturating_sub(go_in);
                    let replay_profit = ho
                        .last()
                        .copied()
                        .unwrap_or(U256::ZERO)
                        .saturating_sub(*opt);
                    // The solver's contract (cl_path_solver_matches_fine_grid_oracle)
                    // is to never under-shoot the optimum. The under-shoot vs the
                    // exact-wei golden is the gate for the coarsened search;
                    // F2 adds the mirror over-shoot gate so BOTH profit
                    // directions are pinned vs the recorded golden.
                    let under = golden_profit.saturating_sub(replay_profit);
                    let over = replay_profit.saturating_sub(golden_profit);
                    let in_delta = if *opt >= go_in {
                        opt - go_in
                    } else {
                        go_in - opt
                    };
                    eprintln!(
                        "path {pid}: input_delta={in_delta} wei  under_shoot={under} wei  over_shoot={over} wei (vs recorded golden)",
                    );
                    if opt.to_string() != go {
                        eprintln!(
                            "path {pid}: optimal_input replay={} golden={go} (informational; gate = profit both directions)",
                            opt,
                        );
                    }
                    if under > U256::from(PROFIT_EPS) {
                        eprintln!(
                            "path {pid}: under_shoot {under} wei > PROFIT_EPS {PROFIT_EPS} — search lost profit vs the recorded optimum"
                        );
                        ok = false;
                    }
                    if over > U256::from(OVER_SHOOT_TOLERANCE_WEI) {
                        eprintln!(
                            "path {pid}: over_shoot {over} wei > tolerance {OVER_SHOOT_TOLERANCE_WEI} — solver exceeds the recorded golden (phantom profit or stale golden)"
                        );
                        ok = false;
                    }
                }
                None => {
                    eprintln!("path {pid}: replay=None but golden present");
                    ok = false;
                }
            }
        }
        if ok {
            n_golden_ok += 1;
        }

        let meas = doc.get("measured").cloned().unwrap_or(Value::Null);
        let ctime = meas
            .get("time_us")
            .map(|x| x.to_string())
            .unwrap_or_else(|| "?".into());
        let csims = meas
            .get("sims")
            .map(|x| x.to_string())
            .unwrap_or_else(|| "?".into());
        let cpieces = meas
            .get("pieces")
            .map(|x| x.to_string())
            .unwrap_or_else(|| "?".into());
        let ranges_per_hop: Vec<usize> = seqs.iter().map(|s| s.ranges.len()).collect();
        let n_wbp: usize = seqs
            .iter()
            .flat_map(|s| &s.ranges)
            .map(|r| r.word_boundary_prices.len())
            .sum();
        println!(
            "path {pid}  median={med}us p95={p95}us min={tmin}us ({iters}x)  sims={sims} refine={refine} pieces={pieces} wsteps={wsteps}  captured(t={ctime}us,s={csims},p={cpieces})  golden={}  ranges/hop={ranges_per_hop:?}  n_word_bounds={n_wbp}",
            if ok { "OK" } else { "MISMATCH" }
        );
        n_paths += 1;
        if med > heaviest.0 {
            heaviest = (med, pid);
        }
    }
    println!(
        "----\nreplayed {n_paths} path(s) | golden {n_golden_ok}/{n_paths} match | deterministic {n_consistent}/{n_paths} | iters={iters} | heaviest median: path {} = {}us | file={path}",
        heaviest.1, heaviest.0
    );
    // F2: a CI build must FAIL if any path regresses (golden mismatch, either
    // direction) or is nondeterministic — this is what makes the harness a
    // gate rather than a report.
    if n_golden_ok != n_paths || n_consistent != n_paths {
        eprintln!(
            "F2 GOLDEN GATE FAILED: golden {n_golden_ok}/{n_paths} match, deterministic {n_consistent}/{n_paths}"
        );
        std::process::exit(1);
    }
}
