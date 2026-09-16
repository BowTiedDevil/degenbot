//! The per-bin lane walk (from the retired grab-file dissolution, ergo epic
//! `5WCRWZ`; T3 created the module for `solve_one_path`; T4 moved the walk
//! driver in): home of `solve_one_path` — the per-path solve + diagnostics
//! body every Solver seat's bin executes — and of THE ONE
//! LANE WALK (`drive_lane_walk`, WNH5OL) plus its policy/context/result
//! types (`LaneArmPolicy`, `LaneWalkBinPlan`, `WalkSubmitCtx`,
//! `LaneWalkReads`) and the walk-side telemetry statics/record
//! (`SLOWEST_PATHS_K`, `WALK_DENSE_ALERTED`, `PathTimeRecord`).
//!
//! Direction of dependency is `lane_walk -> {solve_cycle, executor,
//! inline_sim}`: the walk reads the cycle context, `min_profit_floor`, and
//! the shared clamp body (`clamp_result_with_state`) from `solve_cycle`, and
//! the lane/pipelined-sim seams from `executor`/`inline_sim`. The
//! walk-adjacent helpers (`clamp_result_in_worker`, `flush_solved_item`,
//! `inline_sim_payload`) are defined HERE.
use super::solve_cycle::clamp_result_with_state;
use super::solve_cycle::SolveCycleShared;
use super::{BlockMetadata, HashMap};
use crate::arb_engine::executor::{SolveLane, SolveOutcome};
use crate::arb_engine::inline_sim::{PipelinedSims, SimulatedPathResult};
use ::degenbot_solvers::mixed::{ResolvedMixedPath, SolvePathResult};
use degenbot_core::{diag, op_warn};
/// How many slowest-path entries the solve-cycle completion event names
/// (D63GSE intra-solve visibility). Walk-side only after the T4 move.
const SLOWEST_PATHS_K: usize = 5;
/// Q3 dense one-shot alert flag — the CONSUMER side of the moved alert: the
/// walk reports `WalkStats::max_dense_words`; this logs once per process.
/// Walk-side only after the T4 move.
static WALK_DENSE_ALERTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// K-slowest-path attribution record: (`time_us`, `pieces_visited`,
/// `path_sims`, `word_steps`, `refine_sims`, `gate_us`, `gate_derive_us`,
/// `gate_compose_us`, `gate_search_us`, `path_id`) — lets the completion
/// event name the cost driver of the slowest routes: gate-envelope bound
/// composition (with its derive/compose/search phase split) vs the walk
/// proper, not just wall time. `pub(crate)` because `solve_cycle::PathTimesHeap`
/// aliases it (5WCRWZ T5 relocated the heap; NO re-export shim remains).
pub(crate) type PathTimeRecord = (u128, u64, u64, u64, u64, u64, u64, u64, u64, u64);
// ===========================================================================
// the walk-adjacent helpers, moved here with the walk they
// serve (the retired grab file is deleted outright).
// ===========================================================================
/// SIMPIPE2 T2: the WORKER-side clamp — drive the merge-site-identical
/// clamp from the solve worker's `to_solve`-aligned pool-ref snapshot, so a
/// result is clamp-committed BEFORE the streaming handoff (the T1/T3 seam
/// simulates on a payload whose `consumed_inputs` are the merge-committed
/// values with no engine-lock round-trip). Stance-gated: with
/// `DEGENBOT_SOLVE_INLINE_SIM` unset the merge-site clamp runs exactly as
/// before (this fn is a no-op returning 0).
/// Invariant: twins > 0 ⟺ the clamp mutated the result (every clamp path
/// runs its twin first) — the merge site treats twins > 0 as
/// already-clamped and never re-clamps (a second pass would re-apply the
/// margin and corrupt the committed inputs).
pub(crate) fn clamp_result_in_worker(
    ctx: &SolveCycleShared,
    idx: usize,
    pid: u64,
    result: &mut SolvePathResult,
) -> u64 {
    if !ctx.worker_clamp || idx >= ctx.pool_refs.len() {
        return 0;
    }
    let core = ctx
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver);
    clamp_result_with_state(&core, pid, &ctx.pool_refs[idx].pools, result)
}
/// SIMPIPE2 T3: the WORKER-side inline sim — resolve the per-path payload
/// from the clamp-committed result on the SAME worker context the T2 clamp
/// opened (shared core + to_solve-aligned pool refs; stance + hook gated).
/// The payload rides the result handoff so the merge never calls out — the
/// merge only stores/forwards. `None` = stance off, no hook, or the hook
/// reported failure-without-payload.
#[cfg(all(test, feature = "otel"))]
pub(crate) fn inline_sim_payload(
    ctx: &SolveCycleShared,
    idx: usize,
    pid: u64,
    result: &SolvePathResult,
    parent_span: &tracing::Span,
) -> Option<crate::arb_engine::inline_sim::SimulatedPathResult> {
    if !ctx.worker_clamp || idx >= ctx.pool_refs.len() {
        return None;
    }
    let sim = ctx.inline_sim.as_ref()?;
    // The assembly lives in ONE home (`arb_engine::inline_sim`); this
    // wrapper is only the guards + shape, and otel tests reachable through
    // it pin the production assembly directly (RKXN5Z/IJUBV3 span shape,
    // SIMSPANDUP verdict discipline).
    let request = crate::arb_engine::inline_sim::build_inline_sim_request(ctx, idx, pid, result);
    crate::arb_engine::inline_sim::run_inline_sim(sim, request, result.profit, parent_span.clone())
}

/// Stamp the sim payload onto the held Solved outcome and submit it —
/// ONE flush shape for BOTH solve arms (7LV6VN T5 carry; unified by
/// QR3NUS 43E3H3). The arms differ only in the `submit` closure:
/// the detached arm's closure sends on the merge pipe AND bumps its
/// in-flight gauge at SEND success (a bin that dies before sending
/// never leaks a count); the in-cycle arm's closure is `lane.solved`.
pub(crate) fn flush_solved_item(
    held: &mut Vec<(u64, Option<SolveOutcome>)>,
    submit: &mut dyn FnMut(SolveOutcome),
    done_pid: u64,
    payload: Option<SimulatedPathResult>,
) {
    let Some(ix) = held.iter().position(|(pid, _)| *pid == done_pid) else {
        // The outcome already left `held` (the releasing flush ran — its
        // last sim landed and the walk's final `drain_ready` handed it
        // over). A LATER `drain_ready`/`join_all` receipt for the same
        // pid (an aliased or duplicate scheduler receipt accessor) must
        // NOT resurrect it: a second release would re-bump the detached
        // gauge and false-trip the exactness fuse. The late payload (if
        // any) has no carrier — drop it silently (same contract the
        // former twin helpers' "already flushed" arm kept).
        return;
    };
    let Some(o) = held[ix].1.as_mut() else {
        return; // already flushed
    };
    o.payload = payload;
    let (_, outcome) = held.remove(ix);
    if let Some(outcome) = outcome {
        submit(outcome);
    }
}
/// Per-path solve + diagnostics: the former `solve_fn`
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
    // test-only panic hook — deliberately kill this
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
        prefix_store: Some(&ctx.prefix_cache),
        capture: ctx.gate_capture.as_ref(),
        walk_memo: Some(&*ctx.walk_memo),
        runtime: ctx.runtime,
    };
    let _solve_ctx = solve_span.enter();
    // per-path child span. Created BEFORE the walk (the exported
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
    let outcome =
        ::degenbot_solvers::mixed::solve_path_with_min_profit(resolved, ctx.min_profit, &gate_deps);
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
    // seal the per-path span - walk counters become attributes
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
// --------------------------------------------------------------------------
// THE arm policy + THE one drain (WNH5OL, epic BPZUCM fold).
// --------------------------------------------------------------------------
// The two walk arms (
//  - detached enqueue, `run_bin` at ~:2291 (tip), 90 code lines,
//  - in-cycle dispatch, `run_bin` at ~:2528 (tip), 102 code lines
// ) shared ~64 code lines at the adversarial review (6b281ee49 measured
// similarity 0.667). The fold deletes them by making every difference DATA:
// an arm-policy value below (`LaneArmPolicy`) + the one drain consuming
// `LaneOutcome` exactly once (`drain_lane_outcomes`), so the lane walk body
// (`drive_lane_walk`) is parameterized entirely by the policy.
//
// --------------------------------------------------------------------------
// THE FOUR SEQUENCING CONTRACTS — each named at its enforcement site below
// (search "contract 1..4"; the accept conditions of WNH5OL).
// --------------------------------------------------------------------------
// Contract 1 — admission-draw backpressure: WFF6MM retired the cap-based
//   enqueue gate (and its in-cycle degrade). Backpressure is now the
//   admission draw (`budget = max(0, admission_target_depth − in-flight)` in
//   `on_resolve`); a zero-budget draw SHEDS the cycle before any begin. The
//   walk's flush path still bumps the gauge at Solved SEND success (never
//   for Suppressed/Failed), and the sidecar's drain decrements for Solved
//   only (variant-gated pair — the REV2-Defect-1 rule), which is what the
//   draw reads.
// Contract 2 — enqueue-end return: the detached walk runs from 'static bin
//   threads; the enqueue loop submits and RETURNS with the engine Mutex
//   released. The merged stragglers re-acquire the Mutex per item on the
//   sidecar thread.
// Contract 3 — single lock context: the only drain caller left is
//   the detached sidecar, which acquires the engine Mutex PER straggler
//   item. The drain stays a plain `&mut self` method that NEVER locks (the
//   compile-time `&mut self` signature proves no second lock); the
//   in-cycle whole-cycle hold, its `LaneDrainLockContext` name plate, and
//   the lock-context duality retired with the in-cycle arm.
// Contract 4 — keyless Suppressed/Failed witness: the detached Suppressed
//   (and Failed) no-claim semantics are the ONLY semantics now. Those
//   witnesses carry `pid` at `suppressed()`/`failed()` but NO faithful
//   cycle_seq, so a proxy key would either false-trip the fuse against a
//   same-pid Solved of the next cycle or mask a duplicate. The per-cycle
//   Suppressed/Failed claim divergence (`claim_all_lanes`) retired with the
//   in-cycle arm.
// --------------------------------------------------------------------------
/// The arm policy (WNH5OL; WFF6MM trimmed to the ONE detached arm): the
/// lane walk body is ONE function; every per-arm behavior rides this value
/// (the carrier stamps and the drain's ledger seq). The detached gauge hook
/// stays on the lane itself — fused into `SolveLane::new`, contract
/// 1's send-success-only bump.
pub(crate) struct LaneArmPolicy {
    /// The ledger key half (the ONE-domain rule of 43E3H3): the exact
    /// `solve_seq` tick the drain claims `(seq, pid)` with on the shared ONE
    /// ledger for carrier-keyed outcomes (Solved always; Suppressed/Failed
    /// never claim — contract 4). P37YJG: the seq half is MACHINE-ISSUED —
    /// the detached arm's `DetachedArm.cycle_seq` — and the claim itself
    /// runs through the machine's one ledger door (`DetachedCycle::claim`).
    pub(crate) ledger_seq: u64,
    /// The solve block this cycle is solving (the drain's anchor for the
    /// carrier's `solve_block` debug assert and the merge call).
    pub(crate) solve_block: u64,
    /// The cycle's block metadata ('static bins cannot borrow `&metadata`,
    /// so the walk copies it and the drain reads it through the policy).
    pub(crate) metadata: BlockMetadata,
}
/// THE ONE LANE WALK: the shared body of the two former `run_bin`
/// closures (the ~64-common-line fold). Arm differences are the two
/// parameters: the bin's items (`Arc<Vec>` indexed by the bin plan)
/// and the policy value.
pub(crate) fn drive_lane_walk(
    shared: &std::sync::Arc<SolveCycleShared>,
    solve_span: &tracing::Span,
    bin_plan: &LaneWalkBinPlan,
    policy: &LaneArmPolicy,
    ws_ctx: &WalkSubmitCtx,
    lane: &mut SolveLane,
) -> LaneWalkReads {
    // 7LV6VN T5 (pipelined arm): results park until their sim lands;
    // the walk never waits on a sim. Sims pace on the fleet
    // SimDriver seat pool (the budget's sim slot cap), so walk + sim
    // demand never exceeds the CPU quota by construction.
    let mut held: Vec<(u64, Option<SolveOutcome>)> = Vec::new();
    let mut pending = PipelinedSims::default();
    let mut suppressed_but_unsent: Vec<u64> = Vec::new();
    for &i in &bin_plan.indices {
        let (pid, resolved) = &bin_plan.items[i];
        let outcome =
            solve_one_path(shared, solve_span, *pid, resolved).map(|(pid, mut result)| {
                // SIMPIPE2 T2: clamp in the worker (stance-gated) BEFORE
                // the profitless filter — the clamp recompute can zero a
                // candidate, and the filter must see the commit-ready
                // values.
                let twins = clamp_result_in_worker(shared, i, pid, &mut result);
                (pid, result, twins)
            });
        {
            // The profitless filter runs BEFORE the sim is scheduled —
            // a clamp-zeroed candidate never needs its payload.
            // WNH5OL ACCEPTANCE NOTE (arm-as-data): the pre-fold arms
            // DIFFERED here by delivery-shape only (the detached arm
            // suppressed pre-filter `continue` skips; the in-cycle arm
            // suppressed post-solve `None` arms). Both reductions
            // converge at the same exact delivery contract: exactly one
            // outcome per submitted pid.
            if let Some((pid, result, twins)) = outcome {
                if result.optimal_input.is_zero() || result.profit.is_zero() {
                    if !result.solver_pool_states.is_empty() {
                        diag!(
                            domain = solver,
                            "path_id={pid} hops=[{}]",
                            result.solver_pool_states.join(";")
                        );
                    }
                    // The pre-filter skip IS one delivered outcome
                    // (this IS the detached arm's live record —
                    // QR3NUS): no gauge (contract 1's pairing — it
                    // never bumped), keyless → no claim (contract 4).
                    lane.suppressed(pid);
                    continue;
                }
                if !pending.schedule_one(shared, i, pid, &result, solve_span) {
                    // No sim rides this item (no hook / clamp
                    // off) — flush immediately.
                    held.push((
                        pid,
                        Some(stamp_outcome(pid, result, twins, None, policy, ws_ctx)),
                    ));
                    flush_solved_item(&mut held, &mut |o| lane.solved(o), pid, None);
                    continue;
                }
                if !result.solver_pool_states.is_empty() {
                    diag!(
                        domain = solver,
                        "path_id={pid} hops=[{}]",
                        result.solver_pool_states.join(";")
                    );
                }
                held.push((
                    pid,
                    Some(stamp_outcome(pid, result, twins, None, policy, ws_ctx)),
                ));
                // Fan while walking: send every sim that landed
                // during this iteration's solve.
                for (done_pid, payload) in pending.drain_ready() {
                    flush_solved_item(&mut held, &mut |o| lane.solved(o), done_pid, payload);
                }
            } else {
                // A None IS an outcome: exactly one
                // Suppressed record through the LANE (both arms
                // delivered this arm on the lane in the unfused
                // code; the reads vector is the walk's silent
                // label — no gauge, no claim, no merge).
                lane.suppressed(*pid);
                suppressed_but_unsent.push(*pid);
            }
        }
    }
    // Tail: join every outstanding sim and send.
    if !pending.is_empty() {
        for (done_pid, payload) in pending.join_all() {
            flush_solved_item(&mut held, &mut |o| lane.solved(o), done_pid, payload);
        }
    }
    LaneWalkReads {
        suppressed_unsent: suppressed_but_unsent,
        held_unflushed: held.len(),
    }
}
/// The walk's per-bin plan: the items eligible for solving (aligned to
/// cycle order) and the bin's index window into them (the LPT bin's owned
/// indices). Same byte shape both arms compute today.
pub(crate) struct LaneWalkBinPlan {
    pub(crate) items: std::sync::Arc<Vec<(u64, std::sync::Arc<ResolvedMixedPath>)>>,
    pub(crate) indices: Vec<usize>,
}
/// The walk's stamp context (contract 2's 'static carry): the detached
/// arm hands the enqueue-time stamping halves — the issuing `cycle_seq`,
/// the Q1a per-hop resolve snapshot, and the cycle span — so `stamp_outcome`
/// can fill the envelopes inside the 'static bin. The in-cycle arm passes
/// the inert carrier defaults (seq 0 / no stamps / no span) its drain has
/// always expected. The Solved DELIVERY itself never rides here: both
/// arms submit through the ONE lane (`SolveLane::solved`), so contract 1's
/// gauge hook (a detached-lane setting) stays lane-borne.
pub(crate) struct WalkSubmitCtx {
    /// The issuing cycle's seq (`0` = the in-cycle arm's inert stamp).
    pub(crate) cycle_seq: u64,
    /// The enqueue resolve's per-hop `pool_update_block` snapshot (the
    /// Q1a oracle); `None` on the inert in-cycle stamps.
    pub(crate) update_stamps: Option<std::sync::Arc<HashMap<u64, Vec<u64>>>>,
    /// The enqueue cycle span; `None` on the in-cycle arm.
    pub(crate) solve_span: Option<tracing::Span>,
}
/// What the walk reports to the arm that drove it:
/// - `suppressed_unsent`: the post-solve None arms (the CONVERGENT
///   ancestral shape — the DETACHED arm delivered its profitless skips ON
///   the lane instead, per the in-walk record comment; Suppressed never
///   claims/bumps in either arm).
/// - `held_unflushed`: Solved envelopes pushed but not yet handed to the
///   lane (always 0 — the tail flush drains every sim; both arms debug-
///   assert it).
pub(crate) struct LaneWalkReads {
    #[expect(
        dead_code,
        reason = "consumed per-arm by tests asserting which arm delivered a skipped solve"
    )]
    pub(crate) suppressed_unsent: Vec<u64>,
    pub(crate) held_unflushed: usize,
}
/// Stamp ONE outcome from the policy (the carrier-stamp half of the arm
/// difference). The detached arm fills `cycle_seq`/`update_stamp`/
/// `solve_span` from the enqueue; the in-cycle arm fills the inert
/// values its drain never reads (unchanged semantics).
fn stamp_outcome(
    pid: u64,
    result: SolvePathResult,
    worker_clamp_twins: u64,
    payload: Option<SimulatedPathResult>,
    policy: &LaneArmPolicy,
    ws_ctx: &WalkSubmitCtx,
) -> SolveOutcome {
    SolveOutcome {
        pid,
        result,
        worker_clamp_twins,
        payload,
        solve_block: policy.solve_block,
        metadata: policy.metadata,
        cycle_seq: ws_ctx.cycle_seq,
        update_stamp: ws_ctx
            .update_stamps
            .as_ref()
            .and_then(|s| s.get(&pid).cloned())
            .unwrap_or_default(),
        solve_span: ws_ctx
            .solve_span
            .clone()
            .unwrap_or_else(tracing::Span::none),
    }
}
// ----------------- PER-PATH SPAN TELEMETRY (MQUKB6-T2) -----------------
#[cfg(all(test, feature = "otel"))]
#[expect(clippy::expect_used)] // otel tests assert loudly, per telemetry.rs otel_tests
mod solve_path_span_tests {
    use super::*;
    use crate::otel;
    use degenbot_solvers::mixed::ResolvedMixedPath;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;
    /// ADR-043 §8 behavioral volume gate : one fixture solve
    /// cycle over the committed heavy-CL corpus must stay within the INFO
    /// volume budget, and every INFO+ record must land on a closed
    /// `degenbot::<domain>` target.
    ///
    /// Runs the SERIAL path deliberately: a test-scoped `with_default`
    /// subscriber does not reach the fleet's spawned seat threads, so a fleet
    /// run would capture nothing and the gate would pass vacuously.
    #[test]
    #[expect(clippy::print_stdout)] // the measured distribution is the evidence
    fn fixture_solve_stays_within_the_info_volume_budget() {
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use tracing::Level;
        use tracing_subscriber::layer::{Context, Layer};
        use tracing_subscriber::Registry;
        #[derive(Clone, Default)]
        struct Capture(StdArc<StdMutex<Vec<(String, Level)>>>);
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                self.0.lock().expect("capture lock").push((
                    event.metadata().target().to_string(),
                    *event.metadata().level(),
                ));
            }
        }
        let capture = Capture::default();
        let subscriber = Registry::default().with(capture.clone());
        let items = crate::arb_engine::executor_ab_probe::load_corpus_fixture();
        let ctx = crate::arb_engine::executor_ab_probe::probe_ctx();
        let solved = items.len();
        tracing::subscriber::with_default(subscriber, || {
            for (pid, item) in items.iter().enumerate() {
                let solve = tracing::info_span!("degenbot.arb.solve", block.number = 1u64);
                let _guard = solve.enter();
                let _ = solve_one_path(&ctx, &tracing::Span::current(), pid as u64, item);
            }
        });
        let records = capture.0.lock().expect("capture lock").clone();
        let loud: Vec<(String, Level)> = records
            .iter()
            .filter(|(_, level)| *level <= Level::INFO)
            .cloned()
            .collect();
        let mut histogram: std::collections::BTreeMap<(String, String), usize> =
            std::collections::BTreeMap::new();
        for (target, level) in &loud {
            *histogram
                .entry((target.clone(), level.to_string()))
                .or_default() += 1;
        }
        println!("solved={solved} info+={} distribution:", loud.len());
        for ((target, level), count) in &histogram {
            println!("  {count:>6}  {level:<5} {target}");
        }
        // (a) Volume: steady-state INFO+ must be a small constant per solve.
        let budget = 4 + solved / 20;
        assert!(
        loud.len() <= budget,
        "ADR-043 §8 volume gate: {} INFO+ records over {solved} paths (budget {budget}); the histogram above names the offenders",
        loud.len()
    );
        // (b) Every INFO+ record rides a closed `degenbot::<domain>` target.
        let stray: Vec<&(String, Level)> = loud
            .iter()
            .filter(|(target, _)| !target.starts_with("degenbot::"))
            .collect();
        assert!(
            stray.is_empty(),
            "ADR-043 §8 target gate: INFO+ on non-domain targets: {stray:?}"
        );
    }
    /// `solve_one_path` emits one `degenbot.arb.path` child span parented
    /// under the (re-entered) cycle span, carrying `path.id`. Scoped LOCAL
    /// subscriber (`with_default`): no global-slot mutation, no leakage
    /// from other suites' spans into this exporter.
    #[test]
    fn solve_one_path_emits_child_path_span_under_the_cycle_span() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        let ctx = crate::arb_engine::executor_ab_probe::probe_ctx();
        let resolved = ResolvedMixedPath {
            hops: Vec::new(),
            valid: true,
            state_nonces: Vec::new(),
            max_update_block: 0,
        };
        tracing::subscriber::with_default(subscriber, || {
            let solve = tracing::info_span!("degenbot.arb.solve", block.number = 7u64);
            let _guard = solve.enter();
            let _ = solve_one_path(&ctx, &tracing::Span::current(), 77, &resolved);
        });
        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve_id = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .map(|sp| sp.span_context.span_id())
            .expect("solve span must be exported");
        let paths: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.path")
            .collect();
        assert_eq!(
            paths.len(),
            1,
            "exactly one per-path span; got: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        assert_eq!(
            paths[0].parent_span_id, solve_id,
            "degenbot.arb.path must parent under the re-entered cycle span"
        );
        assert!(
            paths[0]
                .attributes
                .iter()
                .any(|kv| kv.key == opentelemetry::Key::from_static_str("path.id")),
            "path.id must ride as a span attribute"
        );
    }
}
