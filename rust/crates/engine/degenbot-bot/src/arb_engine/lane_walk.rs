//! The per-bin lane walk: home of `solve_one_path` — the per-path solve +
//! diagnostics body every Solver seat's bin executes — and of THE ONE
//! LANE WALK (`drive_lane_walk` plus its policy/context/result
//! types (`LaneArmPolicy`, `LaneWalkBinPlan`, `WalkSubmitCtx`,
//! `LaneWalkReads`).
//!
//! The per-path cost sinks (the pipeline instruments, the K-slowest
//! retention, the capture hooks, and the LPT carry-over) live in the
//! [`super::path_cost_ledger::PathCostLedger`] the walk records into.
//!
//! Direction of dependency is `lane_walk -> {solve_cycle, executor,
//! inline_sim}`: the walk reads the cycle context, `min_profit_floor`, and
//! the shared clamp body (`clamp_result_with_state`) from `solve_cycle`, and
//! the lane/pipelined-sim seams from `executor`/`inline_sim`. The
//! walk-adjacent helpers (`clamp_result_in_worker`, `inline_sim_payload`) and
//! the walk's witness doors are defined HERE.
use super::hooked_leg_requote::{
    commit_plan_hook_fees, hooked_leg_facts, HookedLegAnchor, HookedLegPlan,
};
use super::solve_cycle::clamp_result_with_state;
use super::solve_cycle::SolveCycleShared;
use super::{BlockMetadata, HashMap};
use crate::arb_engine::executor::{SolveClamp, SolveLane, SolveOutcome};
use crate::arb_engine::inline_sim::{PipelinedSims, SimulatedPathResult};
use ::degenbot_pools::solve::{ResolvedMixedPath, SolvePathResult};
use degenbot_core::diag;
// ===========================================================================
// the walk-adjacent helpers, moved here with the walk they serve.
// ===========================================================================
/// The WORKER-side clamp — drive the merge-site-identical clamp from the
/// solve worker's `to_solve`-aligned pool-ref snapshot, so a result is
/// clamp-committed BEFORE the streaming handoff (the inline sim runs on a
/// payload whose `consumed_inputs` are the merge-committed values with no
/// engine-lock round-trip). Stance-gated: with `DEGENBOT_SOLVE_INLINE_SIM`
/// unset the result rides UNFINALIZED and the merge runs that one
/// finalization itself.
///
/// `None` is the clamp's refusal (a hooked leg the plan could not price
/// exactly): the candidate has no executable figures, so the walk records it
/// as one Suppressed outcome and the merge never sees it.
pub(crate) fn clamp_result_in_worker(
    ctx: &SolveCycleShared,
    idx: usize,
    pid: u64,
    result: SolvePathResult,
) -> Option<SolveClamp> {
    if !ctx.worker_clamp || idx >= ctx.pool_refs.len() {
        // No worker clamp for this bin: the merge finalizes exactly once.
        return Some(SolveClamp::Unfinalized(result));
    }
    let pools = &ctx.pool_refs[idx].pools;
    // ADR-090: the hooked-leg facts come off ONE short guard, the re-quote
    // runs with the guard DROPPED (the tier prices against its own
    // anchor-pinned scratch EVM and the node, never `BotState`), and the
    // clamp then commits both arms under a fresh guard.
    let anchor = HookedLegAnchor::at(ctx.solve_block, &ctx.metadata);
    let plan = {
        let core = ctx
            .core
            .read_at(degenbot_substrate::state_lock::LockSite::Solver);
        let facts = hooked_leg_facts(&core, pools, &result.consumed_inputs);
        drop(core);
        HookedLegPlan::requote(facts, anchor, ctx.hooked_leg_requote.as_deref())
    };
    // F2: the charges the re-quote measured are recorded before the clamp
    // commits the amounts (the worker-side twin of the merge-site door).
    commit_plan_hook_fees(&ctx.core, &plan, anchor.block);
    let core = ctx
        .core
        .read_at(degenbot_substrate::state_lock::LockSite::Solver);
    clamp_result_with_state(&core, pid, pools, ctx.clamp_margin, &plan, result)
        .map(SolveClamp::Finalized)
}
/// The WORKER-side inline sim — resolve the per-path payload from the
/// clamp-committed result on the SAME worker context the worker clamp opened
/// (shared core + to_solve-aligned pool refs; stance + hook gated).
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
    // it pin the production assembly directly (span shape, verdict
    // discipline).
    let request = crate::arb_engine::inline_sim::build_inline_sim_request(ctx, idx, pid, result);
    crate::arb_engine::inline_sim::run_inline_sim(sim, request, result.profit, parent_span.clone())
}

/// Per-path solve + diagnostics: the former `solve_fn`
/// closure moved out verbatim so every dispatch arm (the legacy
/// the dedicated tokio executor - dispatch a path identically. Takes the
/// shared per-cycle context by reference; workers touch NO engine state
/// and NO core.lock (engine-then-core lock ordering preserved unchanged),
/// and the passed span is re-entered per item exactly as the `par_iter`
/// closure did (worker threads have no ambient context). Each
/// item also emits a `degenbot.arb.path` DEBUG child span parented under
/// that re-entered cycle span (per-path latency as attributes).
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
    // Worker-local view of the cycle gate deps: the block scope carries the
    // memo + prefix-cache handles AND the epoch, so the deps for this block
    // can never disagree with the memo's epoch. Same semantics as the old
    // single borrowed GateDeps shared by the scope workers.
    let gate_deps = ::degenbot_pools::GateDeps::per_block(
        ctx.scope.epoch(),
        ctx.scope.prefix_cache(),
        ctx.gate_capture
            .as_ref()
            .map(|s| s as &dyn ::degenbot_pools::DegeneratePathSink),
        Some(&**ctx.scope.walk_memo()),
        ctx.runtime,
    )
    .with_telemetry(Some(&**ctx.scope.walk_telemetry()));
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
    let t0 = std::time::Instant::now();
    let outcome =
        ::degenbot_pools::solve::solve_path_with_min_profit(resolved, ctx.min_profit, &gate_deps);
    // The solve wall time is measured ONCE: it feeds the tracing
    // span attributes here and the ledger's single `record` call below.
    let elapsed = t0.elapsed();
    let micros = elapsed.as_micros();
    // Gate telemetry rides the solve outcome now — no TLS read-back.
    let gs = outcome.gate_stats;
    let gate_us = u64::try_from(gs.duration_ns / 1_000).unwrap_or(u64::MAX);
    // Walk telemetry OUT the return path: the outcome carries this path's
    // counters — no TLS read-back. The Q3 dense one-shot alert rides
    // `PathCostLedger::record` below, with the other per-path sinks.
    let outcome_stats = &outcome.stats;
    let ws = *outcome_stats;
    let (pieces, sims) = (ws.pieces, ws.sims);
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
    // ONE ledger call carries every per-path cost sink: the pipeline
    // instruments, the K-slowest retention, the two capture hooks, and this
    // block's measured sims + gate micros for the next block's LPT bins (the
    // fan-out lives inside `PathCostLedger::record`).
    ctx.path_cost_ledger
        .record(pid, resolved, &outcome, elapsed);

    outcome.result.map(|r| (pid, r))
}
// ---------------------------------------------------------------------------
// --------------------------------------------------------------------------
// THE arm policy + THE one drain ).
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
// (search "contract 1..4"; the accept conditions).
// --------------------------------------------------------------------------
// Contract 1 — admission-draw backpressure: retired the cap-based
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
/// The arm policy (trimmed to the ONE detached arm): the
/// lane walk body is ONE function; every per-arm behavior rides this value
/// (the carrier stamps and the drain's ledger seq). The detached gauge hook
/// stays on the lane itself — fused into `SolveLane::new`, contract
/// 1's send-success-only bump.
pub(crate) struct LaneArmPolicy {
    /// The ledger key half (the ONE-domain rule): the exact
    /// `solve_seq` tick the drain claims `(seq, pid)` with on the shared ONE
    /// ledger for carrier-keyed outcomes (Solved always; Suppressed/Failed
    /// never claim — contract 4). The seq half is MACHINE-ISSUED —
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
    // (pipelined arm): a clamp-committed outcome PARKS on the lane witness
    // until its sim lands; the walk never waits on a sim. Sims pace on the
    // fleet SimDriver seat pool (the budget's sim slot cap), so walk + sim
    // demand never exceeds the CPU quota by construction.
    let mut pending = PipelinedSims::default();

    for &i in &bin_plan.indices {
        let (pid, resolved) = &bin_plan.items[i];
        let outcome = solve_one_path(shared, solve_span, *pid, resolved).and_then(|(pid, r)| {
            // Clamp in the worker (stance-gated) BEFORE the profitless filter —
            // the clamp recompute can zero a candidate, and the filter must see
            // the commit-ready values. `None` here is the clamp's refusal: the
            // candidate dies with the `None` arm below (one Suppressed outcome).
            clamp_result_in_worker(shared, i, pid, r).map(|clamp| (pid, clamp))
        });

        let Some((pid, clamp)) = outcome else {
            // A None IS an outcome: exactly one Suppressed record through the
            // lane (no gauge bump, no ledger claim, no merge).
            conclude_suppressed(lane, *pid);
            continue;
        };

        let result = clamp.result();

        // The profitless filter runs BEFORE the sim is scheduled — a
        // clamp-zeroed candidate never needs its payload. The skip IS one
        // delivered outcome, so it concludes through the walk's Suppressed door.
        if result.optimal_input.is_zero() || result.profit.is_zero() {
            if !result.solver_pool_states.is_empty() {
                diag!(
                    domain = solver,
                    "path_id={pid} hops=[{}]",
                    result.solver_pool_states.join(";")
                );
            }
            conclude_suppressed(lane, pid);
            continue;
        }

        if !pending.schedule_one(shared, i, pid, result, solve_span) {
            // No sim rides this item (no hook / clamp off / refused boot): the
            // outcome concludes now, payload-less.
            conclude_solved(lane, stamp_outcome(pid, clamp, None, policy, ws_ctx));
            continue;
        }

        if !result.solver_pool_states.is_empty() {
            diag!(
                domain = solver,
                "path_id={pid} hops=[{}]",
                result.solver_pool_states.join(";")
            );
        }

        // Park the outcome on its path: the witness owes exactly this record,
        // concluded by the scheduled sim's receipt (or by a failure patch).
        park_outcome(lane, stamp_outcome(pid, clamp, None, policy, ws_ctx));

        // Fan while walking: conclude every parked outcome whose sim landed
        // during this iteration's solve.
        for (done_pid, payload) in pending.drain_ready() {
            deliver_receipt(lane, done_pid, payload);
        }
    }

    // Tail: join every outstanding sim and conclude its parked outcome.
    // `join_all` is total — a receipt whose sim never ran (a dead unit's
    // dropped sender) still arrives, as a payload-less record.
    if !pending.is_empty() {
        for (done_pid, payload) in pending.join_all() {
            deliver_receipt(lane, done_pid, payload);
        }
    }

    LaneWalkReads {
        held_unflushed: lane.parked(),
    }
}

/// The walk's Suppressed door: a refusal is an exactness violation the witness
/// logs at its own door (lane identity + reason), and debug builds fail where
/// the contradiction was produced while release builds keep paying the bin's
/// other obligations.
fn conclude_suppressed(lane: &mut SolveLane, pid: u64) {
    let conclusion = lane.suppressed(pid);
    debug_assert!(
        conclusion.is_ok(),
        "Suppressed record refused for path {pid}"
    );
}

/// The walk's immediate Solved door (no sim rode the path).
fn conclude_solved(lane: &mut SolveLane, item: SolveOutcome) {
    let pid = item.pid;
    let conclusion = lane.solved(item);
    debug_assert!(conclusion.is_ok(), "Solved record refused for path {pid}");
}

/// The walk's park door: the path stays owed the SAME record, concluded by its
/// sim receipt.
fn park_outcome(lane: &mut SolveLane, item: SolveOutcome) {
    let pid = item.pid;
    let parked = lane.park(item);
    debug_assert!(parked.is_ok(), "outcome could not park on path {pid}");
}

/// The walk's receipt door: a sim receipt concludes its parked outcome exactly
/// once (a late or un-parked receipt is the witness's refusal to make).
fn deliver_receipt(lane: &mut SolveLane, pid: u64, payload: Option<SimulatedPathResult>) {
    let conclusion = lane.receipt(pid, payload);
    debug_assert!(conclusion.is_ok(), "sim receipt refused for path {pid}");
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
/// What the walk reports to the bin body that drove it: the outcomes still
/// parked on their sim receipts. The witness owns every other per-path fact, so
/// this is the ONE read (a parked outcome left behind would strand a path the
/// ledger already counts as submitted).
pub(crate) struct LaneWalkReads {
    pub(crate) held_unflushed: usize,
}

/// Stamp ONE outcome from the policy (the carrier-stamp half of the arm
/// difference). The detached arm fills `cycle_seq`/`update_stamp`/
/// `solve_span` from the enqueue; the in-cycle arm fills the inert
/// values its drain never reads (unchanged semantics).
fn stamp_outcome(
    pid: u64,
    clamp: SolveClamp,
    payload: Option<SimulatedPathResult>,
    policy: &LaneArmPolicy,
    ws_ctx: &WalkSubmitCtx,
) -> SolveOutcome {
    SolveOutcome {
        pid,
        clamp,
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
// ----------------- PER-PATH SPAN TELEMETRY-----------------
#[cfg(all(test, feature = "otel"))]
// The island loads the committed capture corpus (`executor_ab_probe::
// load_corpus_fixture`), so it rides the `executor-ab-probe` lane feature
// (ADR-084 D3): the test lanes enable it; a default-feature build skips it.
#[cfg(all(test, feature = "executor-ab-probe"))]
#[expect(clippy::expect_used)] // otel tests assert loudly, per telemetry.rs otel_tests
mod solve_path_span_tests {
    use super::*;
    use crate::otel;
    use degenbot_pools::solve::ResolvedMixedPath;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;
    /// ADR-043 §8 behavioral volume gate : one fixture solve
    /// cycle over the committed heavy-concentrated-liquidity corpus must stay within the INFO
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
            ..Default::default()
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
