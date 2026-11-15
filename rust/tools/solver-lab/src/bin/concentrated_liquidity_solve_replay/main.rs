// Dev-only harness: an offline replay harness for captured solver fixtures.
// Pedantic + restriction lints that production code denies are relaxed here.
#![expect(clippy::print_stderr, clippy::print_stdout, clippy::too_many_lines)]

//! Offline heavy-concentrated-liquidity-path replay harness with N-repeat stable timing.
//!
//! Usage:
//!   cargo run -p degenbot-solver-lab --features capture-fixtures
//!     --bin `concentrated_liquidity_solve_replay` [-- <capture.jsonl>]
//!   `DR_REPLAY_ITERS=25` ...   // more reps for tighter p95 (default 9)
//!
//! Re-reads a capture JSONL produced by the live hook
//! (`arb_engine::solver_capture` `DEGENBOT_SOLVER_CAPTURE=1`) or by the live
//! `concentrated_liquidity_capture_gen` generator (now
//! `degenbot-fixture-tools`' `--bin`, the only sanctioned exact-wei producer), rebuilds
//! each Vec<IntV3TickRangeSequence> from the per-range fields, and re-runs
//! `solve_concentrated_liquidity_piecewise` — the production all-concentrated-liquidity solver (the exact call
//! `mixed::solve_path` makes, initial input ONE) — OFFLINE, with no bot / RPC /
//! DB. Each path is solved N times; the median / p95 / min of the per-run wall
//! time is the stable A/B signal. As a bonus the N runs must agree — `int_solve`
//! is pure math, so per-run nondeterminism (e.g. HashMap-iteration order in the
//! active-set walk) is flagged, which also bears on the desync investigation.
//!
//! Two-sided golden gate: the durable correctness contract is owned by the
//! `degenbot-pools` integration tests (`tests/concentrated_liquidity_golden_replay.rs`,
//! run by `just test-golden-replay` through the canonical nextest lane). This
//! program performs measurement, readout, and over-shoot-only golden
//! regeneration through the shared evaluator (`degenbot_pools::solve::golden_gate`):
//! every path is classified — under-shoot past `PROFIT_EPS`, over-shoot past
//! the few-wei tolerance, or nondeterminism across reps — and every mismatch
//! is reported loudly as `MISMATCH`, but a mismatch is not an exit code.

use alloy::primitives::U256;
use degenbot_pools::solve::concentrated_liquidity::WalkEventCensus;
use degenbot_pools::solve::gate::{path_profit_bound, GateDeps, HopMath};
use degenbot_pools::solve::golden_gate::{
    classify, parse_golden, replay_profitable, results_consistent, GateVerdict, ReplaySolution,
    OVER_SHOOT_TOLERANCE_WEI, PROFIT_EPS,
};
use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use degenbot_solver_lab::capture_rows::capture_arg;
use degenbot_solver_lab::concentrated_liquidity_rows::parse_concentrated_liquidity_hops;
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = capture_arg(&args, "heavy_concentrated_liquidity_solve_captures.jsonl");
    let iters: usize = std::env::var("DR_REPLAY_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9)
        .max(1);
    let content = degenbot_pools::solve::fixtures::read_fixture(&path);
    let mut n_paths = 0u64;
    // Loop-18 T4: DR_REPLAY_REGEN=1 refreshes goldens that the solver now
    // EXCEEDS (over-shoot only; under-shoot stays a hard fail) — the loud,
    // deliberate stale-golden regeneration path.
    let regen_golden = std::env::var("DR_REPLAY_REGEN").is_ok();
    let mut regen_queue: Vec<(usize, Value)> = Vec::new();
    let mut regen_count = 0u64;
    // Gate A/B rows : (path_id, golden JSON, derived bound).
    let mut gate_rows: Vec<(u64, Value, Option<U256>)> = Vec::new();
    let mut n_golden_ok = 0u64;
    // Null-golden (gate-skipped) captures, loop-18 T4: excluded from the
    // exact-wei gate and reported separately as reconciliation telemetry.
    let mut n_gate_skipped = 0u64;
    let mut n_consistent = 0u64;
    let mut heaviest: (u128, u64) = (0, 0); // (median_us, path_id)
                                            // Loop-15 census (DEGENBOT_WALK_EVENT_CENSUS=1): grand totals of the
                                            // nested-inversion tally + the T2 cross-block edge-shift analysis.
    let mut census_total = WalkEventCensus::default();
    let mut last_pieces_by_pid: std::collections::HashMap<u64, Vec<(Vec<usize>, U256)>> =
        std::collections::HashMap::new();
    let mut shift_min = None;
    let mut shift_buckets = [0u64; 4];
    let mut tuples_matched = 0u64;
    let mut tuples_new = 0u64;
    let mut cross_block_pids = 0u64;

    for (line_index, line) in content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let doc: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad capture line: {e}");
                continue;
            }
        };
        let pid: u64 = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        // Row policy unchanged by the extraction: a row without a usable
        // `hops` array is skipped silently; a malformed hop prints
        // `path {pid}: skip ({e})` with the shared parser's error text.
        let seqs = match doc.get("hops").and_then(Value::as_array) {
            Some(hops) => match parse_concentrated_liquidity_hops(hops) {
                Ok(seqs) => seqs,
                Err(e) => {
                    eprintln!("path {pid}: skip ({e})");
                    continue;
                }
            },
            None => continue,
        };
        let refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();

        // Gate A/B: derive the profit-envelope bound and time
        // it, so the end-of-run floor analysis can name would-be skips and the
        // derivation overhead per path.
        let views: Vec<Option<HopMath<'_>>> = seqs
            .iter()
            .map(|s| Some(HopMath::concentrated_liquidity_derived(s)))
            .collect();
        let bound_t0 = std::time::Instant::now();
        let bound = match path_profit_bound(&views, &GateDeps::offline()) {
            degenbot_pools::solve::gate::Envelope::Bound(b) => Some(b),
            degenbot_pools::solve::gate::Envelope::Unsupported(cause) => {
                eprintln!("path {pid}: gate unsupported: {cause:?}");
                None
            }
        };
        let bound_us = bound_t0.elapsed().as_micros();
        gate_rows.push((
            pid,
            doc.get("golden").cloned().unwrap_or(Value::Null),
            bound,
        ));
        eprintln!(
            "path {pid}: envelope_derivation={bound_us}us bound={}",
            bound.map_or_else(|| "None(unsupported)".into(), |b| b.to_string())
        );

        // N independent repetitions for a stable A/B (no single-shot noise).
        // solve_concentrated_liquidity_piecewise is pure math, so all runs must agree; per-run
        // nondeterminism (e.g. HashMap-iteration order in the active-set walk)
        // is flagged and counted separately.
        let mut times: Vec<u128> = Vec::with_capacity(iters);
        let mut runs: Vec<Option<ReplaySolution>> = Vec::with_capacity(iters);
        let mut last_stats: Option<degenbot_pools::solve::concentrated_liquidity::WalkStats> = None;
        let mut path_pieces: Vec<(Vec<usize>, U256)> = Vec::new();
        for _ in 0..iters {
            let t0 = std::time::Instant::now();
            let out = degenbot_pools::solve::concentrated_liquidity::derive_and_solve_concentrated_liquidity_piecewise(
                refs.as_slice(),
                &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
            );
            times.push(t0.elapsed().as_micros());
            last_stats = Some(out.stats);
            path_pieces = out.census_pieces;
            runs.push(out.result.map(|solution| ReplaySolution {
                optimal_input: solution.optimal_input,
                hop_outputs: solution.hop_outputs,
            }));
        }
        // Census + per-piece log ride the last rep's WalkOutcome (T2: out
        // the return path; no frozen-thread-local takes).
        let wsteps = last_stats.as_ref().map_or(0, |s| s.word_steps);
        let refine = last_stats.as_ref().map_or(0, |s| s.refine_sims);
        let last_census = last_stats
            .as_ref()
            .map_or(WalkEventCensus::default(), |s| s.census);
        census_total.accumulate_event_census(last_census);
        if let Some(prev) = last_pieces_by_pid.get(&pid) {
            cross_block_pids += 1;
            let prev_map: std::collections::HashMap<&Vec<usize>, &U256> =
                prev.iter().map(|(k, e)| (k, e)).collect();
            for (ks, edge) in &path_pieces {
                if let Some(pe) = prev_map.get(ks).copied() {
                    tuples_matched += 1;
                    let d = if edge > pe { edge - pe } else { pe - edge };
                    *shift_min.get_or_insert(d) = shift_min.map_or(d, |m| m.min(d));
                    let b = if d <= U256::from(4u64) {
                        0
                    } else if d <= U256::from(65_536u64) {
                        1
                    } else if d <= (U256::ONE << 40) {
                        2
                    } else {
                        3
                    };
                    shift_buckets[b] += 1;
                } else {
                    tuples_new += 1;
                }
            }
        }
        last_pieces_by_pid.insert(pid, path_pieces);
        let ws_last = last_stats;
        let pieces = ws_last.as_ref().map_or(0, |s| s.pieces);
        let sims = ws_last.as_ref().map_or(0, |s| s.sims);
        let ls = ws_last.as_ref().map_or(0, |s| s.left_edge_sims);
        let rs = ws_last.as_ref().map_or(0, |s| s.right_edge_sims);
        let ans = ws_last.as_ref().map_or(0, |s| s.anchor_sims);
        let esok = ws_last.as_ref().map_or(0, |s| s.event_solver_ok);
        let esfb = ws_last.as_ref().map_or(0, |s| s.event_solver_fallbacks);
        let consistent = results_consistent(&runs);
        if consistent {
            n_consistent += 1;
        } else {
            eprintln!("path {pid}: NON-DETERMINISTIC across {iters} runs — active-set walk order-dependent?");
        }
        times.sort_unstable();
        let n = times.len();
        let tmin = times[0];
        let med = times[n / 2];
        let p95 = times[((n * 94) / 100).min(n - 1)];

        // Golden classification through the shared evaluator — the one
        // implementation the owning integration tests gate with; this
        // program only reads the verdict out.
        let golden_json = doc.get("golden");
        let golden = parse_golden(golden_json);
        let first = runs.first().and_then(Option::as_ref);
        let verdict = classify(golden.as_ref(), first);
        let mut ok = true;
        if let Some((under, over, input_delta)) = verdict.deltas() {
            eprintln!(
                "path {pid}: input_delta={input_delta} wei  under_shoot={under} wei  over_shoot={over} wei (vs recorded golden)",
            );
            if let (Some(r), Some(go)) = (
                first,
                golden_json
                    .and_then(Value::as_object)
                    .and_then(|g| g.get("optimal_input"))
                    .and_then(Value::as_str),
            ) {
                if r.optimal_input.to_string() != go {
                    eprintln!(
                        "path {pid}: optimal_input replay={} golden={go} (informational; gate = profit both directions)",
                        r.optimal_input
                    );
                }
            }
        }
        match &verdict {
            GateVerdict::GateSkipped => {
                // Null goldens are gate-skipped captures — production
                // answered "nothing" (bound < floor) for this pool state, so
                // the recorded output is not a solver result and CANNOT be
                // compared exact-wei against the gateless replay. Classified
                // separately so the reconciliation volume (the replay-found
                // profit the gate declined to solve) is surfaced as the
                // false-skip telemetry the gate work tracks.
                if let Some(r) = first {
                    if replay_profitable(r) {
                        println!(
                            "path {pid}: GATE-SKIPPED capture — ungated replay profit = {} wei (reconciliation telemetry, not a gate failure)",
                            r.profit()
                        );
                    }
                }
                n_gate_skipped += 1;
                // Fall through to the per-path stat printing but do not fail
                // the golden gate for these lines.
            }
            GateVerdict::Within { .. } => {}
            GateVerdict::UnderShoot { under, .. } => {
                eprintln!(
                    "path {pid}: under_shoot {under} wei > PROFIT_EPS {PROFIT_EPS} — search lost profit vs the recorded optimum"
                );
                ok = false;
            }
            GateVerdict::OverShoot { over, .. } => {
                eprintln!(
                    "path {pid}: over_shoot {over} wei > tolerance {OVER_SHOOT_TOLERANCE_WEI} — solver exceeds the recorded golden (phantom profit or stale golden)"
                );
                if regen_golden {
                    // Regeneration: the solver legitimately EXCEEDS the
                    // recorded golden (improved walk on giant-range shapes),
                    // so the over-shoot-only lines get their goldens
                    // refreshed in place, loudly; under-shoot never reaches
                    // this arm.
                    if let Some(r) = first {
                        regen_queue.push((
                            line_index,
                            serde_json::json!({
                                "optimal_input": r.optimal_input.to_string(),
                                "profit": r.profit().to_string(),
                                "hop_outputs": r
                                    .hop_outputs
                                    .iter()
                                    .map(std::string::ToString::to_string)
                                    .collect::<Vec<_>>(),
                            }),
                        ));
                        eprintln!(
                            "path {pid}: DR_REPLAY_REGEN queued golden refresh (+{over} wei, over-shoot-only)"
                        );
                        regen_count += 1;
                    }
                } else {
                    ok = false;
                }
            }
            GateVerdict::MissingResult => {
                eprintln!("path {pid}: replay=None but golden present");
                ok = false;
            }
        }
        if ok && golden.is_some() {
            n_golden_ok += 1;
        }

        let meas = doc.get("measured").cloned().unwrap_or(Value::Null);
        let ctime = meas
            .get("time_us")
            .map_or_else(|| "?".into(), std::string::ToString::to_string);
        let csims = meas
            .get("sims")
            .map_or_else(|| "?".into(), std::string::ToString::to_string);
        let cpieces = meas
            .get("pieces")
            .map_or_else(|| "?".into(), std::string::ToString::to_string);
        let ranges_per_hop: Vec<usize> = seqs.iter().map(|s| s.ranges.len()).collect();
        let n_wbp: usize = seqs
            .iter()
            .flat_map(|s| &s.ranges)
            .map(|r| r.word_boundary_prices.len())
            .sum();
        println!(
            "path {pid}  median={med}us p95={p95}us min={tmin}us ({iters}x)  sims={sims} refine={refine} pieces={pieces} wsteps={wsteps} left={ls} right={rs} anchor={ans} evtslv={esok}/{esfb}  captured(t={ctime}us,s={csims},p={cpieces})  golden={}  ranges/hop={ranges_per_hop:?}  n_word_bounds={n_wbp}",
            if ok { "OK" } else { "MISMATCH" }
        );
        n_paths += 1;
        if med > heaviest.0 {
            heaviest = (med, pid);
        }
    }
    // Gate floor analysis: for each candidate min_profit
    // floor, how many captured paths would the gate have skipped, and would
    // ANY skip have discarded a path whose golden profit clears the floor
    // (a false skip — must be zero by soundness)?
    println!("---- gate floor analysis (heavy-concentrated-liquidity capture):");
    for &floor_wei in &[0u64, 1_000_000, 1_000_000_000, 1_000_000_000_000] {
        let floor = U256::from(floor_wei);
        let mut skipped = 0u32;
        let mut false_skips = 0u32;
        let mut goldens = 0u32;
        for (pid, golden, bound) in &gate_rows {
            let Some(b) = bound else { continue };
            if *b < floor {
                skipped += 1;
                if !golden.is_null() {
                    goldens += 1;
                    if let Some(g) = parse_golden(Some(golden)) {
                        if g.profit >= floor && !g.profit.is_zero() {
                            false_skips += 1;
                            eprintln!(
                                "FALSE SKIP: path {pid} bound {b} < floor {floor} but golden profit {} >= floor",
                                g.profit
                            );
                        }
                    }
                }
            }
        }
        println!(
            "floor {floor_wei} wei: would_skip {skipped}/{} | goldens checked {goldens} | false skips {false_skips}",
            gate_rows.len()
        );
    }
    if census_total.pieces + census_total.pred_none + census_total.terminal_agree > 0 {
        let pi_ = |v: u64, d: u64| (100 * v) / d.max(1);
        println!(
            "----\nEVENT CENSUS: pieces={} exact={} ({}%) in_bracket_only={} early={:?} late={:?} pred_none={} terminal_agree={} terminal_disagree={} census_sims={}",
            census_total.pieces,
            census_total.exact,
            pi_(census_total.exact, census_total.pieces),
            census_total.in_bracket - census_total.exact,
            census_total.early,
            census_total.late,
            census_total.pred_none,
            census_total.terminal_agree,
            census_total.terminal_disagree,
            census_total.census_sims,
        );
        println!(
            "EDGE-SHIFT (T2): pids_seen_again={} tuples_matched={} tuples_new={} shift_min={} buckets(d4,64k,2^40,beyond)={:?}",
            cross_block_pids,
            tuples_matched,
            tuples_new,
            shift_min.map_or_else(|| "-".to_string(), |d| d.to_string()),
            shift_buckets
        );
    }
    let exact_goldens = n_paths - n_gate_skipped;
    // Loop-18 T4: apply queued golden refreshes (over-shoot-only) in place.
    if regen_count > 0 {
        let mut rewritten: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();
        let raw_lines: Vec<&str> = content.lines().collect();
        for (idx, refreshed) in &regen_queue {
            if let Some(raw) = raw_lines.get(*idx) {
                let mut row: Value = serde_json::from_str(raw).unwrap_or_default();
                row["golden"] = refreshed.clone();
                rewritten.insert(*idx, serde_json::to_string(&row).unwrap_or_default());
            }
        }
        let final_text: String = raw_lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                rewritten
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(|| (*l).to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, final_text + "\n").unwrap_or_else(|e| {
            eprintln!("golden regen write failed: {e}");
        });
        println!(
            "regen: {regen_count} golden(s) refreshed in {path} (over-shoot-only; rerun the replay to confirm)"
        );
    }
    println!(
        "----\nreplayed {n_paths} path(s) | golden {n_golden_ok}/{exact_goldens} exact match | gate-skipped {n_gate_skipped} | deterministic {n_consistent}/{n_paths} | iters={iters} | heaviest median: path {} = {}us | file={path}",
        heaviest.1, heaviest.0
    );
    // The durable two-sided golden gate lives in the owning `degenbot-pools`
    // integration tests (run by `just test-golden-replay` through the
    // canonical nextest lane); this program is the measurement/readout pass,
    // so a mismatch is reported loudly here but is not an exit code.
    // Null-golden gate-skipped captures are reconciled out (counted
    // separately) and deterministic coverage is computed over the whole
    // population.
    if n_golden_ok != exact_goldens || n_consistent != n_paths {
        eprintln!(
            "GOLDEN MISMATCHES PRESENT: golden {n_golden_ok}/{n_paths} match, deterministic {n_consistent}/{n_paths} (the owning degenbot-pools tests gate this contract)"
        );
    }
}
