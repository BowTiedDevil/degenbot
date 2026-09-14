//! The per-bin lane walk (from the `solver_dispatch` dissolution, ergo epic
//! `5WCRWZ` task 3): home of `solve_one_path` — the per-path solve +
//! diagnostics body every Solver seat's bin executes (epic BXUSGL T1).
//!
//! Created by T3 as the module the `solve_one_path` bin body moves into.
//! The walk driver itself (`drive_lane_walk`) and the Lane* policy types
//! (`LaneArmPolicy`, `LaneWalkBinPlan`, `WalkSubmitCtx`) are LATER tasks'
//! inhabitants (5WCRWZ T4+): only the per-bin body lives here today, so the
//! module's public surface is intentionally a single function while the
//! driver is still in the grab file.
//!
//! Direction of dependency is `lane_walk -> solver_dispatch` for now: the
//! body reads the `SLOWEST_PATHS_K`/`WALK_DENSE_ALERTED` walk telemetry
//! statics and `min_profit_floor` still owned by the grab file, and the
//! cycle context from `solve_cycle`. Those re-point when the later tasks
//! land the driver and retire the grab file.

use super::solve_cycle::SolveCycleShared;
use super::solver_dispatch::{min_profit_floor, SLOWEST_PATHS_K, WALK_DENSE_ALERTED};
use ::degenbot_solvers::mixed::{ResolvedMixedPath, SolvePathResult};
use degenbot_core::op_warn;

/// Per-path solve + diagnostics (epic BXUSGL T1): the former `solve_fn`
/// closure moved out verbatim so every dispatch arm (the legacy
/// the dedicated tokio executor - dispatch a path identically. Takes the
/// shared per-cycle context by reference; workers touch NO engine state
/// and NO core.lock (engine-then-core lock ordering preserved unchanged),
/// and the passed span is re-entered per item exactly as the `par_iter`
/// closure did (MQUKB6-T0: worker threads have no ambient context). Each
/// item also emits a `degenbot.arb.path` DEBUG child span parented under
/// that re-entered cycle span (MQUKB6-T2: per-path latency as attributes).
#[expect(clippy::too_many_lines)] // the moved solve + diagnostics pipeline is one narrative
pub(crate) fn solve_one_path(
    ctx: &SolveCycleShared,
    solve_span: &tracing::Span,
    pid: u64,
    resolved: &ResolvedMixedPath,
) -> Option<(u64, SolvePathResult)> {
    // Test-only deterministic slowen hook (the streaming-merge test).
    #[cfg(test)]
    if let Some(delay) = ctx.test_solve_delay.as_ref() {
        delay(pid);
    }
    // 43E3H3 red-first: test-only panic hook — deliberately kill this
    // path's solve mid-walk (catch_unwind on the seat/lane decides the
    // disposition; the breaker suite pins that disposition).
    #[cfg(test)]
    if let Some(panic_pid) = ctx.test_solve_panic.as_ref() {
        panic_pid(pid);
    }
    // Worker-local view of the cycle gate deps (BXUSGL T1): the Arc-d
    // memo + owned capture cfg land in the shared ctx per cycle; the
    // prefix cache is generationed by the block epoch - same semantics
    // as the old single borrowed GateDeps shared by the scope workers.
    let gate_deps = ::degenbot_solvers::profit_envelope::GateDeps {
        epoch: ctx.epoch,
        prefix_cache: true,
        capture: ctx.gate_capture.as_ref(),
        walk_memo: Some(&*ctx.walk_memo),
        runtime: ctx.runtime,
    };
    let _solve_ctx = solve_span.enter();
    // MQUKB6-T2: per-path child span. Created BEFORE the walk (the exported
    // duration is the real solve wall) and recorded after; the walk counters
    // ride span ATTRIBUTES on this `degenbot.arb.path` node instead of
    // events on the cycle span, making per-path latency a Jaeger query
    // rather than a log grep. DEBUG level is the volume guard: production
    // INFO runs keep one node per CYCLE (a 200-path solve must not fan out
    // 200 Jaeger nodes by default); `RUST_LOG=degenbot::solver=debug` opts
    // into per-path nodes.
    let path_span = tracing::debug_span!(
        target: "degenbot::solver",
        "degenbot.arb.path",
        path.id = pid,
        path.us = tracing::field::Empty,
        path.sims = tracing::field::Empty,
        path.pieces = tracing::field::Empty,
        gate.us = tracing::field::Empty,
        path.profit = tracing::field::Empty,
    );
    let _path_ctx = path_span.enter();
    ::degenbot_solvers::profit_envelope::reset_gate_stats();
    let t0 = std::time::Instant::now();
    let outcome = ::degenbot_solvers::mixed::solve_path_with_min_profit(
        resolved,
        min_profit_floor(),
        &gate_deps,
    );
    let micros = t0.elapsed().as_micros();
    ctx.solve_cpu_us.fetch_add(
        u64::try_from(micros).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    let gs = ::degenbot_solvers::profit_envelope::take_last_gate_stats();
    let gate_us = u64::try_from(gs.duration_ns / 1_000).unwrap_or(u64::MAX);
    if let Some(p) = crate::instruments::pipeline() {
        #[expect(clippy::cast_precision_loss)]
        {
            p.observe_per_path_solve_duration(micros as f64 / 1e6);
            p.observe_per_path_gate_duration(gs.duration_ns as f64 / 1e9);
        }
    }
    ctx.gate_total.lock().merge(&gs);
    // Walk telemetry OUT the return path (SU7MAE T2): the
    // outcome carries this path's counters — no TLS
    // read-back. The Q3 dense one-shot alert is the
    // CONSUMER's decision.
    let outcome_stats = &outcome.stats;
    if outcome_stats.max_dense_words >= ::degenbot_solvers::mobius_v3_int::DENSE_OBSERVE_THRESHOLD
        && !WALK_DENSE_ALERTED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        op_warn!(
            domain = solver,
            max_dense_words = outcome_stats.max_dense_words,
            threshold = ::degenbot_solvers::mobius_v3_int::DENSE_OBSERVE_THRESHOLD,
            "Q3-DENSE: a CL range crossed the dense-word threshold; harvest a real capture"
        );
    }
    let ws = *outcome_stats;
    let (pieces, sims, word_steps, refine_sims, ternary_sims, grid_sims) = (
        ws.pieces,
        ws.sims,
        ws.word_steps,
        ws.refine_sims,
        ws.ternary_sims,
        ws.grid_sims,
    );
    // Record this block's measured walk sims for the next
    // block's LPT cost (loop-12 KUKHMX).
    ctx.sims_recorder
        .lock()
        .insert(pid, u64::try_from(sims).unwrap_or(0));
    // Loop-18: record measured gate time for the LPT cost too
    // (gate-heavy paths carry sims≈0 and were bin-packed cheap).
    ctx.gate_recorder.lock().insert(pid, gate_us);
    let (gate_derive_us, gate_compose_us, gate_search_us) = (
        u64::try_from(gs.derive_ns / 1_000).unwrap_or(u64::MAX),
        u64::try_from(gs.compose_ns / 1_000).unwrap_or(u64::MAX),
        u64::try_from(gs.search_ns / 1_000).unwrap_or(u64::MAX),
    );
    ctx.walk_ternary_total.fetch_add(
        u64::try_from(ternary_sims).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    ctx.walk_grid_total.fetch_add(
        u64::try_from(grid_sims).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    ctx.walk_pieces_total.fetch_add(
        u64::try_from(pieces).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    ctx.walk_sims_total.fetch_add(
        u64::try_from(sims).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    ctx.walk_word_steps_total.fetch_add(
        u64::try_from(word_steps).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    ctx.walk_refine_sims_total.fetch_add(
        u64::try_from(refine_sims).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    // MQUKB6-T2: seal the per-path span - walk counters become attributes
    // on the `degenbot.arb.path` node (guard drops at fn end, so the
    // recorded values are inside the exported duration).
    path_span.record("path.us", u64::try_from(micros).unwrap_or(u64::MAX));
    path_span.record("path.sims", u64::try_from(sims).unwrap_or(u64::MAX));
    path_span.record("path.pieces", u64::try_from(pieces).unwrap_or(u64::MAX));
    path_span.record("gate.us", gate_us);
    if let Some(r) = outcome.result.as_ref() {
        path_span.record("path.profit", tracing::field::display(r.profit));
    }
    let mut heap = ctx.path_times.lock();
    {
        let worst = heap.peek().map_or(
            u128::MAX,
            |std::cmp::Reverse((w, _, _, _, _, _, _, _, _, _))| *w,
        );
        if heap.len() < SLOWEST_PATHS_K || micros > worst {
            heap.push(std::cmp::Reverse((
                micros,
                u64::try_from(pieces).unwrap_or(0),
                u64::try_from(sims).unwrap_or(0),
                u64::try_from(word_steps).unwrap_or(0),
                u64::try_from(refine_sims).unwrap_or(0),
                gate_us,
                gate_derive_us,
                gate_compose_us,
                gate_search_us,
                pid,
            )));
            if heap.len() > SLOWEST_PATHS_K {
                heap.pop();
            }
        }
    }
    if let Some(cap) = ctx.capture.as_ref() {
        cap.maybe_capture(
            pid,
            ctx.solve_block,
            u64::try_from(micros).unwrap_or(u64::MAX),
            u64::try_from(sims).unwrap_or(0),
            u64::try_from(pieces).unwrap_or(0),
            outcome.result.as_ref(),
            resolved,
        );
    }
    if let Some(cap) = ctx.capture_mixed.as_ref() {
        cap.maybe_capture(
            pid,
            ctx.solve_block,
            u64::try_from(micros).unwrap_or(u64::MAX),
            u64::try_from(sims).unwrap_or(0),
            u64::try_from(pieces).unwrap_or(0),
            outcome.result.as_ref(),
            resolved,
        );
    }
    outcome.result.map(|r| (pid, r))
}
// ---------------------------------------------------------------------------
// HONESTY PROBE (5WCRWZ T3 red pin): the lane-walk items are OWNED here, not
// in the retired grab file. While `solver_dispatch.rs` still defines them,
// this probe fails; at cutover it passes. Same technique as T1's capture
// probe and T2's workload probe.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod ownership_probe {
    const GRAB_FILE: &str = include_str!("solver_dispatch.rs");

    #[test]
    fn solver_dispatch_no_longer_defines_the_lane_walk_items() {
        const RETIRED_DEFINITIONS: [&str; 4] = [
            "pub(crate) struct SolveCycleShared",
            "pub(crate) fn solve_one_path(",
            "struct PipelinedSims {",
            "impl PipelinedSims {",
        ];
        for marker in RETIRED_DEFINITIONS {
            assert!(
                !GRAB_FILE.contains(marker),
            "solver_dispatch.rs still defines: {marker:?} — the lane-walk items must be owned by arb_engine::lane_walk / solve_cycle / inline_sim (5WCRWZ T3)"
            );
        }
    }
}
