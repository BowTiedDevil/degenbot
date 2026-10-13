// Dev/example-only harness: an offline entry A/B readout for the solve-entry
// unification decision.
// Pedantic + restriction lints that production code denies are relaxed here.
#![expect(clippy::print_stderr, clippy::print_stdout, clippy::too_many_lines)]

//! Entry-level A/B readout: today's `all-CL` dispatch route vs the unified
//! route (`all-CL` workloads through the mixed entry), on real captured
//! paths. This is one of the two bench inputs the unify-or-split decision
//! needs; the other is the functions-timing run over the `cl_solve.*` labels
//! (same binary under `--features hotpath`, see the usage block).
//!
//! Usage:
//!
//! ```text
//!   cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers \
//!     --example entry_ab_readout [-- <capture.jsonl>]
//!   DR_AB_ITERS=49 ...   // reps per path per arm (default 25)
//!   DR_AB_MEMO=1 ...     // memo stance ON (default 0 = handle present, inactive)
//!
//!   Telemetry run (functions-timing over the cl_solve.* labels):
//!   cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers \
//!     --features hotpath --example entry_ab_readout
//!   HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=hp.json \
//!   HOTPATH_REPORT=functions-timing,threads
//! ```
//!
//! Arms measured per `all-CL` path (interleaved reps, order alternates per
//! rep):
//!
//! - Arm "today": the production branch route in `mixed::families::cl` — the
//!   prepared hops are CLONED from the resolved path (three `Arc` bumps per
//!   hop) and handed to `solve_cl_piecewise`.
//! - Arm "unified": one borrowed `MixedHopView::Cl` per position handed to
//!   `solve_mixed_piecewise` — the route an `all-CL`-unifying change would
//!   take.
//!
//! Parity gate: both arms must return byte-equal
//! `(optimal_input, profit, hop_outputs)`; any divergence fails the process
//! (exit 1) — unification is only a pure refactor if this holds.
//!
//! The row census answers "which entry is hotter": each captured row is
//! classified by the route the production branch would pick (all hops CL ->
//! `all-CL` entry; any `V2` hop -> mixed entry; other families -> refused
//! here).

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;
use degenbot_solvers::cl::{
    solve_cl_piecewise, solve_mixed_piecewise, MixedHopView, PreparedClHop, WalkMemo,
};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use serde_json::Value;

#[path = "common/cl_rows.rs"]
mod cl_rows;
#[path = "common/mod.rs"]
mod common;

use cl_rows::parse_cl_hops;

/// Median of a mutable slice of microsecond samples (sorts in place).
fn median_us(samples: &mut [u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let n = samples.len();
    if n % 2 == 1 {
        samples[n / 2]
    } else {
        samples[n / 2 - 1].midpoint(samples[n / 2])
    }
}

/// 95th percentile of a mutable slice (nearest-rank, integer-only).
fn p95_us(samples: &mut [u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = (samples.len() * 95).div_ceil(100);
    samples[idx.saturating_sub(1).min(samples.len() - 1)]
}

/// Row route classification at the JSON level, mirroring the production
/// branch test (all hops carry an int sequence -> `all-CL` entry).
fn route_of(doc: &Value) -> &'static str {
    let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
        return "other";
    };
    if doc.get("hop_order").is_some() {
        // Mixed-schema row: per-hop `kind` discriminant.
        let all_cl = hops
            .iter()
            .all(|h| h.get("kind").and_then(Value::as_str) == Some("CL"));
        let any_foreign = hops.iter().any(
            |h| matches!(h.get("kind").and_then(Value::as_str), Some(k) if k != "CL" && k != "V2"),
        );
        if any_foreign {
            "other"
        } else if all_cl {
            "all_cl"
        } else {
            "mixed"
        }
    } else {
        // Pure-CL-schema row: hops are arrays of CL ranges.
        match parse_cl_hops(hops) {
            Ok(_) => "all_cl",
            Err(_) => "other",
        }
    }
}

/// The sequences of a row the production branch would route to the `all-CL`
/// entry, schema-aware: `pure-CL` rows parse through the shared CL parser;
/// mixed-schema rows are measured only when every hop is `kind = "CL"`
/// (the branch's exact test) — anything else is the mixed entry's own
/// traffic, unaffected by the unification question.
fn all_cl_seqs_of(doc: &Value) -> Option<Vec<IntV3TickRangeSequence>> {
    let hops = doc.get("hops").and_then(Value::as_array)?;
    if doc.get("hop_order").is_some() {
        let mut seqs = Vec::with_capacity(hops.len());
        for h in hops {
            if h.get("kind").and_then(Value::as_str) != Some("CL") {
                return None;
            }
            let ranges = h.get("ranges").and_then(Value::as_array)?;
            let rs = ranges
                .iter()
                .map(common::range)
                .collect::<Result<Vec<_>, String>>()
                .ok()?;
            seqs.push(IntV3TickRangeSequence { ranges: rs });
        }
        Some(seqs)
    } else {
        parse_cl_hops(hops).ok()
    }
}

fn main() {
    // Hotpath instrumentation is a compile-time opt-in; with the feature on,
    // this single guard's lifetime spans the run and its drop writes the
    // report (functions-timing over every `#[hotpath::measure]` label the run
    // touched — the `cl_solve.*` entry labels included).
    #[cfg(feature = "hotpath")]
    let _guard = hotpath::HotpathGuardBuilder::new("entry_ab_readout").build();

    let args: Vec<String> = std::env::args().collect();
    let path = common::capture_arg(&args, "heavy_cl_solve_captures.jsonl");
    let iters: usize = std::env::var("DR_AB_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25)
        .max(2);
    let memo_on = std::env::var("DR_AB_MEMO").is_ok_and(|v| v == "1");
    let cfg = SolveRuntimeConfig::default();

    let content = degenbot_solvers::capture_fixture::read_fixture(&path);
    println!(
        "Entry A/B readout: corpus={path} iters={iters} memo_stance={}",
        if memo_on {
            "ON (handle active)"
        } else {
            "OFF (handle present, inactive)"
        }
    );

    // The memo handle is present in both stances (production threads it; the
    // stance flag gates the fold), so the stance difference is exactly the
    // per-probe fingerprint fold + probe/commit cost.
    let memo = WalkMemo::new(memo_on, false);
    if memo_on {
        memo.begin_block(1);
    }

    // ---- Census + per-path A/B --------------------------------------------
    let (mut n_rows, mut n_all_cl, mut n_mixed, mut n_other) = (0u64, 0u64, 0u64, 0u64);
    let mut n_paths = 0u64;
    let mut parity_fail = 0u64;
    let mut sims_today = 0u64;
    let mut sims_unified = 0u64;
    // Per-rep totals (whole-corpus wall) per arm.
    let mut rep_totals_today: Vec<u128> = Vec::with_capacity(iters);
    let mut rep_totals_unified: Vec<u128> = Vec::with_capacity(iters);
    // Per-path median wall per arm (the stable per-path signal).
    let mut path_med_today: Vec<u128> = Vec::new();
    let mut path_med_unified: Vec<u128> = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad capture line: {e}");
                continue;
            }
        };
        n_rows += 1;
        match route_of(&doc) {
            "all_cl" => n_all_cl += 1,
            "mixed" => n_mixed += 1,
            _ => n_other += 1,
        }
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let Some(seqs) = all_cl_seqs_of(&doc) else {
            // Mixed-route or foreign-family row: the unification question does
            // not touch it (silent skip; the census above counted it).
            continue;
        };
        if seqs.len() < 2 {
            continue;
        }
        n_paths += 1;
        // Production shape: derive the prepared hops ONCE per path (the
        // projection hands the branch a prepared value per CL hop).
        let prepared: Vec<PreparedClHop> = seqs
            .iter()
            .map(|s| PreparedClHop::derive(std::sync::Arc::new(IntV3TickRangeSequence::clone(s))))
            .collect();

        let mut times_today: Vec<u128> = Vec::with_capacity(iters);
        let mut times_unified: Vec<u128> = Vec::with_capacity(iters);
        let mut parity_ref: Option<Option<(U256, U256, Vec<U256>)>> = None;

        for rep in 0..iters {
            // Interleaved reps with alternating order: thermal/cache drift
            // cancels across arms instead of landing on one of them.
            let today_first = rep % 2 == 0;
            for arm_today in [today_first, !today_first] {
                if arm_today {
                    // Today's branch route: clone (three Arc bumps per hop),
                    // then the all-CL entry.
                    let t0 = std::time::Instant::now();
                    let cloned: Vec<PreparedClHop> = prepared.clone();
                    let out = solve_cl_piecewise(&cloned, Some(&memo), None, &cfg, None);
                    times_today.push(t0.elapsed().as_micros());
                    sims_today += out.stats.sims as u64;
                    match &parity_ref {
                        None => parity_ref = Some(out.result),
                        Some(want) => {
                            if *want != out.result {
                                parity_fail += 1;
                                eprintln!(
                                    "path {pid} rep {rep}: PARITY DIVERGENCE today arm {want:?} vs {out:?}",
                                    out = out.result
                                );
                            }
                        }
                    }
                } else {
                    // The unified route: one borrowed view per hop, then the
                    // mixed entry.
                    let t0 = std::time::Instant::now();
                    let views: Vec<MixedHopView<'_>> =
                        prepared.iter().map(MixedHopView::Cl).collect();
                    let out = solve_mixed_piecewise(&views, Some(&memo), None, &cfg, None);
                    times_unified.push(t0.elapsed().as_micros());
                    sims_unified += out.stats.sims as u64;
                    match &parity_ref {
                        None => parity_ref = Some(out.result),
                        Some(want) => {
                            if *want != out.result {
                                parity_fail += 1;
                                eprintln!(
                                    "path {pid} rep {rep}: PARITY DIVERGENCE unified arm {want:?} vs {out:?}",
                                    out = out.result
                                );
                            }
                        }
                    }
                }
            }
        }
        path_med_today.push(median_us(&mut times_today));
        path_med_unified.push(median_us(&mut times_unified));
        for r in 0..iters {
            rep_totals_today.push(times_today[r]);
            rep_totals_unified.push(times_unified[r]);
        }
    }

    // ---- 1-hop edge (behavioral divergence, no timing) --------------------
    let one_hop_seq = {
        let doc: Option<Value> = content
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| serde_json::from_str(l).ok());
        doc.as_ref().and_then(all_cl_seqs_of).map(|mut s| {
            s.truncate(1);
            s
        })
    };
    let (one_cl, one_mixed) = match one_hop_seq {
        Some(seq) => {
            let prepared: Vec<PreparedClHop> = seq
                .iter()
                .map(|s| {
                    PreparedClHop::derive(std::sync::Arc::new(IntV3TickRangeSequence::clone(s)))
                })
                .collect();
            let cl = solve_cl_piecewise(&prepared, None, None, &cfg, None);
            let views: Vec<MixedHopView<'_>> = prepared.iter().map(MixedHopView::Cl).collect();
            let mx = solve_mixed_piecewise(&views, None, None, &cfg, None);
            (cl.result.map(|(_, p, _)| p), mx.result.map(|(_, p, _)| p))
        }
        None => (None, None),
    };

    // ---- Report -----------------------------------------------------------
    let total_today: u128 = rep_totals_today.iter().sum();
    let total_unified: u128 = rep_totals_unified.iter().sum();
    let mut med_today = path_med_today.clone();
    let mut med_unified = path_med_unified.clone();
    let mut p95_today = path_med_today;
    let mut p95_unified = path_med_unified;
    println!();
    println!("== Row census (production branch route) ==");
    println!("rows={n_rows} route_all_cl={n_all_cl} route_mixed={n_mixed} route_other={n_other}");
    println!();
    println!("== Entry A/B over {n_paths} all-CL path(s), {iters} reps per arm (interleaved) ==");
    println!("parity: {parity_fail} divergence(s); sims today={sims_today} unified={sims_unified}");
    println!(
        "1-hop edge: all-CL entry profit={one_cl:?} | mixed entry profit={one_mixed:?} (len<2 guard)"
    );
    println!();
    println!("                                 total_us   path_p50_us  path_p95_us");
    println!(
        "arm today   (clone + all-CL)  {:>10} {:>12} {:>11}",
        total_today,
        median_us(&mut med_today),
        p95_us(&mut p95_today)
    );
    println!(
        "arm unified (borrow + mixed)  {:>10} {:>12} {:>11}",
        total_unified,
        median_us(&mut med_unified),
        p95_us(&mut p95_unified)
    );
    // Integer basis points: no float casts (workspace clippy denies them).
    let ratio_bp: u128 = total_unified
        .saturating_mul(10_000)
        .checked_div(total_today)
        .unwrap_or(0);
    if total_today > 0 {
        println!(
            "unified/today total = {}.{:04}x ( <1 = unified faster )",
            ratio_bp / 10_000,
            ratio_bp % 10_000
        );
    }
    println!();
    println!(
        "VERDICT DATA: parity_fail={parity_fail} n_paths={n_paths} sims_equal={} ratio_bp={ratio_bp}",
        sims_today == sims_unified
    );

    if parity_fail > 0 {
        std::process::exit(1);
    }
}
