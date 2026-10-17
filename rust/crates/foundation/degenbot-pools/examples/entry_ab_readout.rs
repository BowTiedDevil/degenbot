// Dev/example-only harness: an offline entry A/B readout for the solve-entry
// unification decision.
// Pedantic + restriction lints that production code denies are relaxed here.
#![expect(clippy::print_stderr, clippy::print_stdout, clippy::too_many_lines)]

//! Entry-level A/B readout: the retained `all-concentrated-liquidity` intake convenience vs
//! the unified solve entry directly, on real captured paths. This harness
//! was the entry-level bench input for the solve-entry unification decision
//! (the other was the functions-timing run over the `concentrated_liquidity_solve.*` labels —
//! same binary under `--features hotpath`, see the usage block); post-
//! unification the two arms share ONE solve body, so the ratio measures the
//! intake shape alone and the parity gate pins the forwarding contract.
//!
//! Usage:
//!
//! ```text
//!   cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-pools \
//!     --example entry_ab_readout [-- <capture.jsonl>]
//!   DR_AB_ITERS=49 ...   // reps per path per arm (default 25)
//!   DR_AB_MEMO=1 ...     // memo stance ON (default 0 = handle present, inactive)
//!
//!   Telemetry run (functions-timing over the concentrated_liquidity_solve.* labels):
//!   cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-pools \
//!     --features hotpath --example entry_ab_readout
//!   HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=hp.json \
//!   HOTPATH_REPORT=functions-timing,threads
//! ```
//!
//! Arms measured per `all-concentrated-liquidity` path (interleaved reps, order alternates per
//! rep):
//!
//! - Arm "intake": the retained `all-concentrated-liquidity` convenience route — the prepared
//!   hops CLONED (three `Arc` bumps per hop, the caller shape this harness
//!   models) and handed to the forwarding `solve_concentrated_liquidity_piecewise`.
//! - Arm "direct": one borrowed `MixedHopView::ConcentratedLiquidity` per position handed to
//!   `solve_piecewise`, the ONE solve body both routes share.
//!
//! Parity gate: both arms must return byte-equal
//! `(optimal_input, profit, hop_outputs)`; any divergence fails the process
//! (exit 1) — the forwarding intake contract.
//!
//! The row census answers "which entry is hotter": each captured row is
//! classified by the route the pre-unification dispatch branch picked (all
//! hops concentrated liquidity -> `all-concentrated-liquidity` entry; any `V2` hop -> mixed entry; other families ->
//! refused here) — the decision record's traffic-mix input.

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;
use degenbot_pools::solve::concentrated_liquidity::{
    solve_concentrated_liquidity_piecewise, solve_piecewise, MixedHopView,
    PreparedConcentratedLiquidityHop, WalkMemo,
};
use degenbot_pools::solve::runtime::SolveRuntimeConfig;
use serde_json::Value;

#[path = "common/mod.rs"]
mod common;
#[path = "common/concentrated_liquidity_rows.rs"]
mod concentrated_liquidity_rows;

use concentrated_liquidity_rows::parse_concentrated_liquidity_hops;

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

/// The route the pre-unification dispatch branch picked for a row (the
/// decision record's traffic-mix input).
#[derive(Clone, Copy)]
enum Route {
    /// Every hop carries an int sequence: the all-concentrated-liquidity entry.
    AllConcentratedLiquidity,
    /// Any V2 hop: the mixed entry.
    Mixed,
    /// Foreign family or unparseable row: the dispatch arm refuses it.
    Other,
}

/// Row route classification at the JSON level, mirroring the pre-unification
/// branch test (all hops carry an int sequence -> `all-concentrated-liquidity` entry).
fn route_of(doc: &Value) -> Route {
    let Some(hops) = doc.get("hops").and_then(Value::as_array) else {
        return Route::Other;
    };
    if doc.get("hop_order").is_some() {
        // Mixed-schema row: per-hop `kind` discriminant.
        let all_concentrated_liquidity = hops
            .iter()
            .all(|h| h.get("kind").and_then(Value::as_str) == Some("concentrated_liquidity"));
        let any_foreign = hops.iter().any(
            |h| matches!(h.get("kind").and_then(Value::as_str), Some(k) if k != "concentrated_liquidity" && k != "V2"),
        );
        if any_foreign {
            Route::Other
        } else if all_concentrated_liquidity {
            Route::AllConcentratedLiquidity
        } else {
            Route::Mixed
        }
    } else {
        // Pure-concentrated-liquidity-schema row: hops are arrays of concentrated liquidity ranges.
        match parse_concentrated_liquidity_hops(hops) {
            Ok(_) => Route::AllConcentratedLiquidity,
            Err(_) => Route::Other,
        }
    }
}

/// The sequences of a row the production branch would route to the `all-concentrated-liquidity`
/// entry, schema-aware: `pure-concentrated-liquidity` rows parse through the shared concentrated liquidity parser;
/// mixed-schema rows are measured only when every hop is `kind = "concentrated_liquidity"`
/// (the branch's exact test) — anything else is the mixed entry's own
/// traffic, unaffected by the unification question.
fn all_concentrated_liquidity_seqs_of(doc: &Value) -> Option<Vec<IntV3TickRangeSequence>> {
    let hops = doc.get("hops").and_then(Value::as_array)?;
    if doc.get("hop_order").is_some() {
        let mut seqs = Vec::with_capacity(hops.len());
        for h in hops {
            if h.get("kind").and_then(Value::as_str) != Some("concentrated_liquidity") {
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
        parse_concentrated_liquidity_hops(hops).ok()
    }
}

fn main() {
    // Hotpath instrumentation is a compile-time opt-in; with the feature on,
    // this single guard's lifetime spans the run and its drop writes the
    // report (functions-timing over every `#[hotpath::measure]` label the run
    // touched — the `concentrated_liquidity_solve.*` entry labels included).
    #[cfg(feature = "hotpath")]
    let _guard = hotpath::HotpathGuardBuilder::new("entry_ab_readout").build();

    let args: Vec<String> = std::env::args().collect();
    let path = common::capture_arg(&args, "heavy_concentrated_liquidity_solve_captures.jsonl");
    let iters: usize = std::env::var("DR_AB_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25)
        .max(2);
    let memo_on = std::env::var("DR_AB_MEMO").is_ok_and(|v| v == "1");
    let cfg = SolveRuntimeConfig::default();

    let content = degenbot_pools::solve::capture_fixture::read_fixture(&path);
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
    let (mut n_rows, mut n_all_concentrated_liquidity, mut n_mixed, mut n_other) =
        (0u64, 0u64, 0u64, 0u64);
    let mut n_paths = 0u64;
    let mut parity_fail = 0u64;
    let mut sims_intake = 0u64;
    let mut sims_direct = 0u64;
    // Per-rep totals (whole-corpus wall) per arm.
    let mut rep_totals_intake: Vec<u128> = Vec::with_capacity(iters);
    let mut rep_totals_direct: Vec<u128> = Vec::with_capacity(iters);
    // Per-path median wall per arm (the stable per-path signal).
    let mut path_med_intake: Vec<u128> = Vec::new();
    let mut path_med_direct: Vec<u128> = Vec::new();

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
            Route::AllConcentratedLiquidity => n_all_concentrated_liquidity += 1,
            Route::Mixed => n_mixed += 1,
            Route::Other => n_other += 1,
        }
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let Some(seqs) = all_concentrated_liquidity_seqs_of(&doc) else {
            // Mixed-route or foreign-family row: the unification question does
            // not touch it (silent skip; the census above counted it).
            continue;
        };
        if seqs.len() < 2 {
            continue;
        }
        n_paths += 1;
        // Production shape: derive the prepared hops ONCE per path (the
        // projection hands the branch a prepared value per concentrated liquidity hop).
        let prepared: Vec<PreparedConcentratedLiquidityHop> = seqs
            .iter()
            .map(|s| {
                PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(
                    IntV3TickRangeSequence::clone(s),
                ))
            })
            .collect();

        let mut times_intake: Vec<u128> = Vec::with_capacity(iters);
        let mut times_direct: Vec<u128> = Vec::with_capacity(iters);
        let mut parity_ref: Option<Option<(U256, U256, Vec<U256>)>> = None;

        for rep in 0..iters {
            // Interleaved reps with alternating order: thermal/cache drift
            // cancels across arms instead of landing on one of them.
            let intake_first = rep % 2 == 0;
            for arm_intake in [intake_first, !intake_first] {
                if arm_intake {
                    // The intake route: clone (three Arc bumps per hop),
                    // then the forwarding all-concentrated-liquidity convenience.
                    let t0 = std::time::Instant::now();
                    let cloned: Vec<PreparedConcentratedLiquidityHop> = prepared.clone();
                    let out = solve_concentrated_liquidity_piecewise(
                        &cloned,
                        Some(&memo),
                        None,
                        &cfg,
                        None,
                    );
                    times_intake.push(t0.elapsed().as_micros());
                    sims_intake += out.stats.sims as u64;
                    match &parity_ref {
                        None => parity_ref = Some(out.result),
                        Some(want) => {
                            if *want != out.result {
                                parity_fail += 1;
                                eprintln!(
                                    "path {pid} rep {rep}: PARITY DIVERGENCE intake arm {want:?} vs {out:?}",
                                    out = out.result
                                );
                            }
                        }
                    }
                } else {
                    // The direct route: one borrowed view per hop, then the
                    // one solve body.
                    let t0 = std::time::Instant::now();
                    let views: Vec<MixedHopView<'_>> = prepared
                        .iter()
                        .map(MixedHopView::ConcentratedLiquidity)
                        .collect();
                    let out = solve_piecewise(&views, Some(&memo), None, &cfg, None);
                    times_direct.push(t0.elapsed().as_micros());
                    sims_direct += out.stats.sims as u64;
                    match &parity_ref {
                        None => parity_ref = Some(out.result),
                        Some(want) => {
                            if *want != out.result {
                                parity_fail += 1;
                                eprintln!(
                                    "path {pid} rep {rep}: PARITY DIVERGENCE direct arm {want:?} vs {out:?}",
                                    out = out.result
                                );
                            }
                        }
                    }
                }
            }
        }
        path_med_intake.push(median_us(&mut times_intake));
        path_med_direct.push(median_us(&mut times_direct));
        for r in 0..iters {
            rep_totals_intake.push(times_intake[r]);
            rep_totals_direct.push(times_direct[r]);
        }
    }

    // ---- 1-hop edge (behavioral divergence, no timing) --------------------
    let one_hop_seq = {
        let doc: Option<Value> = content
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| serde_json::from_str(l).ok());
        doc.as_ref()
            .and_then(all_concentrated_liquidity_seqs_of)
            .map(|mut s| {
                s.truncate(1);
                s
            })
    };
    let (one_intake, one_direct) = match one_hop_seq {
        Some(seq) => {
            let prepared: Vec<PreparedConcentratedLiquidityHop> = seq
                .iter()
                .map(|s| {
                    PreparedConcentratedLiquidityHop::derive(std::sync::Arc::new(
                        IntV3TickRangeSequence::clone(s),
                    ))
                })
                .collect();
            let concentrated_liquidity =
                solve_concentrated_liquidity_piecewise(&prepared, None, None, &cfg, None);
            let views: Vec<MixedHopView<'_>> = prepared
                .iter()
                .map(MixedHopView::ConcentratedLiquidity)
                .collect();
            let mx = solve_piecewise(&views, None, None, &cfg, None);
            (
                concentrated_liquidity.result.map(|(_, p, _)| p),
                mx.result.map(|(_, p, _)| p),
            )
        }
        None => (None, None),
    };

    // ---- Report -----------------------------------------------------------
    let total_intake: u128 = rep_totals_intake.iter().sum();
    let total_direct: u128 = rep_totals_direct.iter().sum();
    let mut med_intake = path_med_intake.clone();
    let mut med_direct = path_med_direct.clone();
    let mut p95_intake = path_med_intake;
    let mut p95_direct = path_med_direct;
    println!();
    println!("== Row census (production branch route) ==");
    println!("rows={n_rows} route_all_concentrated_liquidity={n_all_concentrated_liquidity} route_mixed={n_mixed} route_other={n_other}");
    println!();
    println!("== Entry A/B over {n_paths} all-concentrated-liquidity path(s), {iters} reps per arm (interleaved) ==");
    println!("parity: {parity_fail} divergence(s); sims intake={sims_intake} direct={sims_direct}");
    println!(
        "1-hop edge (uniform len<2 guard): intake refused={} | direct refused={}",
        one_intake.is_none(),
        one_direct.is_none()
    );
    println!();
    println!("                                 total_us   path_p50_us  path_p95_us");
    println!(
        "arm intake (clone + forward) {:>10} {:>12} {:>11}",
        total_intake,
        median_us(&mut med_intake),
        p95_us(&mut p95_intake)
    );
    println!(
        "arm direct (borrow + body)   {:>10} {:>12} {:>11}",
        total_direct,
        median_us(&mut med_direct),
        p95_us(&mut p95_direct)
    );
    // Integer basis points: no float casts (workspace clippy denies them).
    let ratio_bp: u128 = total_direct
        .saturating_mul(10_000)
        .checked_div(total_intake)
        .unwrap_or(0);
    if total_intake > 0 {
        println!(
            "direct/intake total = {}.{:04}x ( <1 = direct faster )",
            ratio_bp / 10_000,
            ratio_bp % 10_000
        );
    }
    println!();
    println!(
        "VERDICT DATA: parity_fail={parity_fail} n_paths={n_paths} sims_equal={} ratio_bp={ratio_bp}",
        sims_intake == sims_direct
    );

    if parity_fail > 0 {
        std::process::exit(1);
    }
}
