// Dev/example-only harness: a WalkMemoStats before/after readout over the captured
// heavy-CL corpus. Pedantic + restriction lints that production code denies are
// relaxed here.
#![expect(clippy::print_stderr, clippy::print_stdout, clippy::too_many_lines)]

//! Offline `WalkMemo` cross-block A/B readout over the heavy-CL capture fixture.
//!
//! Usage:
//!   cargo run -p degenbot-solvers --example `walkmemo_readout` [-- <capture.jsonl>]
//!   `DR_READOUT_EPOCHS=8` ...  // more epochs over the same path set (default 5)
//!
//! Loads the same capture fixture the other replays use
//! (`heavy_cl_solve_captures.jsonl` via `capture_fixture::fixture_path`),
//! derives [`ClSolveTables`] ONCE per captured path (the production shape —
//! the A/B must not measure table derivation), then runs `E` epochs over the
//! FULL path set in two arms:
//!
//! - Arm A (memo off): `solve_cl_piecewise` with `None` memo every epoch;
//!   records per-epoch total walk sims + wall time. This is the "before"
//!   baseline the memo arm is read against.
//! - Arm B (memo on, stats on): one `WalkMemo::new(true, true)` handle;
//!   `begin_block(epoch)` at each epoch start BEFORE the per-path probes
//!   (the engine contract in `arb_engine::solve_cycle`); every path is
//!   solved with `Some(&memo)`; `take_stats()` closes the epoch.
//!
//! HONESTY CAVEAT (read before quoting any number): the fixture is per-path,
//! not per-block — there is no natural epoch structure. This replay therefore
//! measures the memo at SYNTHETIC MAXIMUM recurrence (every composition in the
//! set recurs every epoch), not live block-to-block recurrence. The counters
//! that WOULD decide the live questions (probes vs hits vs `cache_plays`
//! divergence across blocks) are all in place and reported here.

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::cl::{solve_cl_piecewise, ClSolveTables, WalkMemo, WalkMemoStats};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use serde_json::Value;

/// One parsed capture row: a path id plus its owned CL sequences.
struct CapturedPath {
    pid: u64,
    seqs: Vec<IntV3TickRangeSequence>,
}

/// Arm A (memo off) per-epoch accounting.
struct ArmARow {
    epoch: u64,
    walk_sims: usize,
    wall_ms: f64,
}

/// Arm B (memo on, stats on) per-epoch accounting: the [`WalkMemoStats`]
/// counters plus the epoch's actually-executed walk sims and wall time.
struct ArmBRow {
    stats: WalkMemoStats,
    walk_sims: usize,
    wall_ms: f64,
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

/// Parse one capture row into its owned sequences (same shape as
/// `cl_solve_replay`'s row parser; failures are reported, never swallowed).
fn parse_path(doc: &Value) -> Result<CapturedPath, String> {
    let pid = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
    let hops_v = doc.get("hops").and_then(Value::as_array).ok_or("hops")?;
    let mut seqs: Vec<IntV3TickRangeSequence> = Vec::with_capacity(hops_v.len());
    for hop in hops_v {
        let ra = hop.as_array().ok_or("hop not an array")?;
        if ra.is_empty() {
            return Err("empty hop".into());
        }
        let ranges = ra.iter().map(range).collect::<Result<Vec<_>, String>>()?;
        seqs.push(IntV3TickRangeSequence { ranges });
    }
    Ok(CapturedPath { pid, seqs })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        degenbot_solvers::capture_fixture::fixture_path("heavy_cl_solve_captures.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let epochs: u64 = std::env::var("DR_READOUT_EPOCHS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
        .max(1);
    let content = degenbot_solvers::capture_fixture::read_fixture(&path);

    // Parse every row; a parse failure is fatal (exit nonzero) — the readout
    // must never silently shrink the path set it reports on.
    let mut paths: Vec<CapturedPath> = Vec::new();
    let mut n_bad = 0u64;
    for (line_index, line) in content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let parsed = serde_json::from_str::<Value>(line)
            .map_err(|e| e.to_string())
            .and_then(|doc| parse_path(&doc));
        match parsed {
            Ok(p) => paths.push(p),
            Err(e) => {
                eprintln!("line {}: parse failure: {e}", line_index + 1);
                n_bad += 1;
            }
        }
    }
    if n_bad > 0 || paths.is_empty() {
        eprintln!(
            "FAIL: {n_bad} unparsed line(s), {} usable path(s) — fixture/parse failure, refusing to read out a partial set",
            paths.len()
        );
        std::process::exit(1);
    }
    let n_paths = paths.len();
    let n_pids = paths
        .iter()
        .map(|p| p.pid)
        .collect::<std::collections::HashSet<_>>()
        .len();

    // Production shape: derive the crossing tables + word profiles ONCE per
    // path, exactly what the engine's HopProjectionCache rides — the A/B must
    // not measure table derivation.
    let prepared: Vec<Vec<ClSolveTables>> = paths
        .iter()
        .map(|p| p.seqs.iter().map(ClSolveTables::derive).collect())
        .collect();
    let cfg = SolveRuntimeConfig::default();
    println!(
        "WalkMemo readout: {n_paths} captured path(s) ({n_pids} distinct path_id(s)) x {epochs} epoch(s), tables derived once per path, fixture={path}"
    );

    // ---- Arm A: memo off (the "before" baseline) ----
    let mut arm_a: Vec<ArmARow> = Vec::new();
    for epoch in 1..=epochs {
        let mut walk_sims = 0usize;
        let t0 = std::time::Instant::now();
        for (p, prep) in paths.iter().zip(prepared.iter()) {
            let refs: Vec<&IntV3TickRangeSequence> = p.seqs.iter().collect();
            let out = solve_cl_piecewise(&refs, prep, None, &cfg, None);
            walk_sims += out.stats.sims;
        }
        arm_a.push(ArmARow {
            epoch,
            walk_sims,
            wall_ms: t0.elapsed().as_secs_f64() * 1_000.0,
        });
    }

    // ---- Arm B: memo on + stats on (the "after" arm) ----
    // Engine contract (solve_cycle.rs): the cross-block census advances at
    // block start, BEFORE the per-path probes.
    let memo = WalkMemo::new(true, true);
    let mut arm_b: Vec<ArmBRow> = Vec::new();
    for epoch in 1..=epochs {
        memo.begin_block(epoch);
        let mut walk_sims = 0usize;
        let t0 = std::time::Instant::now();
        for (p, prep) in paths.iter().zip(prepared.iter()) {
            let refs: Vec<&IntV3TickRangeSequence> = p.seqs.iter().collect();
            let out = solve_cl_piecewise(&refs, prep, Some(&memo), &cfg, None);
            walk_sims += out.stats.sims;
        }
        let wall_ms = t0.elapsed().as_secs_f64() * 1_000.0;
        let stats = memo.take_stats();
        arm_b.push(ArmBRow {
            stats,
            walk_sims,
            wall_ms,
        });
    }

    // ---- Report ----
    println!();
    println!("== Arm A: memo off (baseline) ==");
    println!("epoch  paths  walk_sims  wall_ms");
    for r in &arm_a {
        println!(
            "{:>5}  {:>5}  {:>9}  {:>7.1}",
            r.epoch, n_paths, r.walk_sims, r.wall_ms
        );
    }
    let a_total_sims: usize = arm_a.iter().map(|r| r.walk_sims).sum();
    let a_total_ms: f64 = arm_a.iter().map(|r| r.wall_ms).sum();
    println!("TOTAL         {a_total_sims:>9}  {a_total_ms:>7.1}");

    println!();
    println!("== Arm B: memo on, stats on (begin_block per epoch, take_stats at epoch end) ==");
    println!(
        "epoch  probes  hits  distinct  cache_plays  neg_played  neg_entries  probe_sims  hit_sims  walk_sims  wall_ms"
    );
    for r in &arm_b {
        let s = &r.stats;
        println!(
            "{:>5}  {:>6}  {:>4}  {:>8}  {:>11}  {:>10}  {:>11}  {:>10}  {:>8}  {:>9}  {:>7.1}",
            s.epoch,
            s.probes,
            s.hits,
            s.distinct,
            s.cache_plays,
            s.negatives_played,
            s.negative_entries,
            s.probes_sims,
            s.hits_sims,
            r.walk_sims,
            r.wall_ms
        );
    }
    let sum_u64 = |f: fn(&ArmBRow) -> u64| -> u64 { arm_b.iter().map(f).sum() };
    let b_probes = sum_u64(|r| r.stats.probes);
    let b_hits = sum_u64(|r| r.stats.hits);
    let b_cache_plays = sum_u64(|r| r.stats.cache_plays);
    let b_negs_played = sum_u64(|r| r.stats.negatives_played);
    let b_probe_sims = sum_u64(|r| r.stats.probes_sims);
    let b_hit_sims = sum_u64(|r| r.stats.hits_sims);
    let b_walk_sims: usize = arm_b.iter().map(|r| r.walk_sims).sum();
    let b_total_ms: f64 = arm_b.iter().map(|r| r.wall_ms).sum();
    let last = &arm_b[arm_b.len() - 1].stats;
    println!(
        "TOTAL  {:>6}  {:>4}     (last)  {:>11}  {:>10}  {:>11}  {:>10}  {:>8}  {:>9}  {:>7.1}",
        b_probes,
        b_hits,
        b_cache_plays,
        b_negs_played,
        last.negative_entries,
        b_probe_sims,
        b_hit_sims,
        b_walk_sims,
        b_total_ms
    );
    println!(
        "(distinct + neg_entries are per-epoch cardinality / a running gauge — the TOTAL row shows the final epoch's values, not sums)"
    );

    // One-line verdict: epoch 1 is the cold epoch by construction (empty
    // previous-epoch census -> zero hits); epochs 2+ are the memo arms under
    // SYNTHETIC MAXIMUM recurrence (every composition recurs every epoch).
    let warm_hits: u64 = arm_b.iter().skip(1).map(|r| r.stats.hits).sum();
    let warm_probes: u64 = arm_b.iter().skip(1).map(|r| r.stats.probes).sum();
    let avoided = a_total_sims.saturating_sub(b_walk_sims);
    let pct = (100 * avoided).checked_div(a_total_sims).unwrap_or(0);
    let avoided_ms = a_total_ms - b_total_ms;
    let pct_ms = if a_total_ms <= f64::EPSILON {
        0.0
    } else {
        (100.0 * avoided_ms) / a_total_ms
    };
    println!();
    println!(
        "VERDICT: epoch 1 = cold (hits={}; A {a1:.1}ms vs B {b1:.1}ms — within-epoch duplicate probes already play the cache); epochs 2+ = memo arms ({warm_hits}/{warm_probes} probes hit, {b_cache_plays} cache plays, {b_negs_played} cached negatives skipped); memo arm ran {b_total_ms:.1}ms of {a_total_ms:.1}ms across all {epochs} epoch(s) ({pct_ms:.0}% wall avoided) and {b_walk_sims} of {a_total_sims} walk sims ({pct}% avoided)",
        arm_b[0].stats.hits,
        a1 = arm_a[0].wall_ms,
        b1 = arm_b[0].wall_ms,
    );
    println!(
        "NOTE: static-fixture replay = SYNTHETIC MAXIMUM recurrence (every composition recurs each epoch), NOT a live block-to-block measurement."
    );
}
