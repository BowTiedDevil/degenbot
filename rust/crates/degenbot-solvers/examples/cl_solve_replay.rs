//! Offline replay harness for the all-CL solver (int_solve_cl_path).
//!
//! Reads the heavy-path captures (one JSON object per line) written by the
//! bot's DEGENBOT_SOLVER_CAPTURE=1 hook, reconstructs each all-CL path's
//! Vec of IntV3TickRangeSequence, replays the CL solve offline (no bot, no
//! network), and reports time / walk sims / pieces while asserting the
//! golden result reproduces. This is the fast loop for optimizing the CL
//! solver without spinning up a full bot run.
//!
//! Usage:
//!   cargo run -p degenbot-solvers --example cl_solve_replay
//!   cargo run -p degenbot-solvers --example cl_solve_replay <captures.jsonl>

use std::process::ExitCode;

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mobius_v3_int::{int_solve_cl_path, reset_walk_stats, take_last_walk_stats};
use serde_json::Value;

fn u256(s: &str) -> Result<U256, String> {
    let t = s.trim();
    U256::from_str_radix(t, 10)
        .or_else(|_| U256::from_str_radix(t.trim_start_matches("0x"), 16))
        .map_err(|e| e.to_string())
}

fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {k}"))
        .map(String::from)
}

fn range(v: &Value) -> Result<IntV3TickRangeHop, String> {
    let wbp: Vec<U256> = v
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

fn seq(hop: &Value) -> Result<IntV3TickRangeSequence, String> {
    let ranges = hop
        .as_array()
        .ok_or_else(|| "hop is not a range array".to_string())?
        .iter()
        .map(range)
        .collect::<Result<Vec<_>, String>>()?;
    if ranges.is_empty() {
        return Err("empty tick-range sequence".to_string());
    }
    Ok(IntV3TickRangeSequence { ranges })
}

fn main() -> ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/heavy_cl_solve_captures.jsonl".to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut replayed: u64 = 0;
    let mut matched: u64 = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad json line: {e}");
                continue;
            }
        };
        let pid = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let hops = match doc.get("hops").and_then(Value::as_array).cloned() {
            Some(h) if !h.is_empty() => h,
            _ => {
                eprintln!("path {pid}: no hops to replay");
                continue;
            }
        };
        let seqs: Vec<IntV3TickRangeSequence> =
            match hops.iter().map(seq).collect::<Result<Vec<_>, String>>() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("path {pid}: parse error: {e}");
                    continue;
                }
            };
        let refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();

        reset_walk_stats();
        let t0 = std::time::Instant::now();
        let res = int_solve_cl_path(refs.as_slice());
        let micros = t0.elapsed().as_micros();
        let (pieces, sims) = take_last_walk_stats();
        replayed += 1;

        let golden = doc.get("golden").cloned().unwrap_or(Value::Null);
        let replay_profitable = res.as_ref().map_or(false, |(opt, _p, ho)| {
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
            ok = !replay_profitable;
            if !ok {
                eprintln!("path {pid}: golden says unprofitable but replay found profit");
            }
        } else {
            match res.as_ref() {
                Some((opt, _p, ho)) => {
                    let go = golden
                        .get("optimal_input")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if opt.to_string() != go {
                        eprintln!(
                            "path {pid}: optimal_input replay={} golden={go}",
                            opt.to_string()
                        );
                        ok = false;
                    }
                    let gh = golden
                        .get("hop_outputs")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect::<Vec<String>>()
                        });
                    let rh: Vec<String> = ho.iter().map(|o| o.to_string()).collect();
                    if gh.as_ref().map(|v| v.as_slice()) != Some(rh.as_slice()) {
                        eprintln!("path {pid}: hop_outputs mismatch");
                        ok = false;
                    }
                }
                None => {
                    eprintln!("path {pid}: golden present but replay returned None");
                    ok = false;
                }
            }
        }
        if ok {
            matched += 1;
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
            "path {pid}  replay={micros}us  sims={sims}  pieces={pieces}  captured(t={ctime}us,s={csims},p={cpieces})  golden={g}  ranges/hop={rph:?}  n_word_bounds={n_wbp}",
            g = if ok { "OK" } else { "MISMATCH" },
            rph = ranges_per_hop,
        );
    }

    println!("----");
    println!("replayed {replayed} path(s), golden matched {matched}/{replayed}; file={path}");
    if replayed == 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
