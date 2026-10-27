//! The epoch's solve phase: the fan-out activation telemetry, the LPT
//! partition of the staged work, and the detached enqueue that hands every
//! staged path to a solve bin.
//!
//! Machine-direct free functions over the epoch's [`EpochContext`]: the phase
//! stages the work out of the context's resolved snapshots, packs the bins
//! from the cost ledger, and hands the spine back only the census facts its
//! dispatch arm reads. The enqueue is enqueue-END — the epoch returns before
//! any result is merged.

use super::{describe_path_cached_with, EpochContext, SolveCycleShared};
use crate::arb_engine::detached_cycle::DetachedArm;
use crate::arb_engine::executor::{run_solve_lane, SolveLane};
use crate::arb_engine::fleet_solve_executor::SOLVE_BIN_KEY_BASE;
use crate::arb_engine::lane_walk::{
    drive_lane_walk, LaneArmPolicy, LaneWalkBinPlan, WalkSubmitCtx,
};
use crate::arb_engine::solver_capture::{gate_capture_from_cfg, CaptureVariant, HeavyPathCapture};
use crate::arb_engine::workload_partition::{
    lpt_partition, path_cost_proxy, plan_bins, sims_aware_cost, solve_bin_count,
};
use ::degenbot_pools::solve::{MixedPath, ResolvedMixedPath};
use degenbot_core::diag;
use degenbot_pools::AffectedKey;
use degenbot_workers::dispatcher::SeatSurvivesPolicy;
use degenbot_workers::lane::LaneCtx;
use hashbrown::{HashMap, HashSet};

/// The census facts the solve phase hands back to the epoch spine: the staged
/// (binned) path count, the invalid/deferred tallies the arm carries, the bin
/// count, and the cycle's detached sequence number.
pub(super) struct SolvePhase {
    pub(super) staged: usize,
    pub(super) invalid: u64,
    pub(super) deferred: usize,
    pub(super) bins: usize,
    pub(super) seq: u64,
}

/// The fan-out activation telemetry over the epoch's final affected set: one
/// `degenbot.arb.fanout` phase node, the per-path activation events under it,
/// and the aggregate summary line.
pub(super) fn fanout_telemetry(
    ctx: &EpochContext<'_>,
    affected: &[AffectedKey],
    affected_path_ids: &HashSet<u64>,
    solve_block: u64,
    cycle_start: std::time::Instant,
) {
    // Telemetry: name EVERY path the dirty-pool fan-out just activated,
    // with its concrete hop list — a Jaeger trace now answers "which pools
    // are in this path" without cross-referencing Python state. Runs under
    // the drainer's `degenbot.arb.solve` span, so the events parent there.
    // phase span - the fan-out activation telemetry gets its
    // own Jaeger node under the cycle span (matches `arb_solve.*`
    // hotpath labels 1:1); the aggregate rides span attributes, so even
    // the phase-summary EVENT below can no longer orphan phase data.
    let fanout_ctx = tracing::info_span!(
        target: "degenbot::solver",
        "degenbot.arb.fanout",
        block.number = solve_block,
        paths.affected = affected_path_ids.len(),
    )
    .entered();
    hotpath::measure_block!("arb_solve.fanout_activate_telemetry", {
        // Per-path activation events are debug-level now: the
        // per-event span plumbing dominated the fan-out phase; the
        // diagnostic remains reachable via RUST_LOG degenbot::engine=debug.
        if tracing::enabled!(target: "degenbot::engine", tracing::Level::DEBUG) {
            for &path_id in affected_path_ids {
                diag!(domain = solver, block_number = solve_block,
                    path.id = path_id,
                    path.hops = %describe_path_cached_with(
                        ctx.path_description_cache,
                        ctx.core,
                        ctx.registry,
                        path_id,
                    ),
                    dirty.keys = affected.len(),
                    "activated by dirty pool"
                );
            }
        }
    });
    // Telemetry: fan-out summary (activations above can be hundreds of
    // events; this one line carries the aggregate).
    let fanout_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX);
    diag!(
        domain = solver,
        block_number = solve_block,
        paths.affected = affected_path_ids.len(),
        dirty.keys = affected.len(),
        phase_us = fanout_us,
        "fanned out to affected paths"
    );
    drop(fanout_ctx);
}

/// The LPT bin-pack over the staged solve set: the phase's cost-ledger
/// snapshot, the structural worker count, and the partition. The
/// `degenbot.arb.lpt` span is the phase node.
pub(super) fn lpt_bins(
    to_solve: &[(u64, std::sync::Arc<ResolvedMixedPath>)],
    shared: &SolveCycleShared,
) -> Vec<Vec<usize>> {
    // phase span for the LPT bin-pack (shared by both
    // dispatch arms - and the detached arm's identical binning).
    let _lpt_ctx = tracing::info_span!(
        target: "degenbot::solver",
        "degenbot.arb.lpt",
        paths = to_solve.len(),
    )
    .entered();
    // bins follow the cgroup-budget worker count (not the
    // old pool width) so each arm is exactly n_bins tasks
    // on n_workers persistent threads, per the solve_executor doc.
    // Sizing reconciliation: fleet-hosted cycles bin at the
    // fleet's STRUCTURAL seat count — pins and bins are the same
    // number, so every bin owns a warm keyed seat across cycles.
    let n_threads = solve_bin_count();
    // LW-T7 (Seam F): the bin fan-out goes through the typed runtime
    // fallback decision — a capability narrower than the intended
    // width is a NAMED-AND-LOGGED plan, never a silent narrower bin.
    let plan = plan_bins(n_threads, n_threads);
    let n_threads = plan.bins;
    // Loop-12 lab: previous-block measured walk sims refine the
    // LPT cost; the ledger's typed snapshot is taken once (single
    // lock) before binning. Loop-18: measured gate us rides the same
    // snapshot - gate-heavy paths (dense-concentrated-liquidity compose) need the bins to
    // know they are expensive despite sims~0.
    let last_cost = shared.path_cost_ledger.snapshot();
    let costs: Vec<usize> = to_solve
        .iter()
        .map(|(pid, r)| {
            let measured = last_cost.cost(*pid);
            sims_aware_cost(
                path_cost_proxy(r),
                measured.map(|c| c.sims),
                measured.map(|c| c.gate_us),
            )
        })
        .collect();
    lpt_partition(to_solve.len(), n_threads, |i| costs[i])
}

/// The solve phase: stage the epoch's affected set, LPT-pack it, and enqueue
/// every staged path on the detached arm. Returns the census the spine's arm
/// reads.
#[expect(clippy::too_many_lines)] // the stage sweep + LPT pack + detached enqueue in one phase body
pub(super) fn solve_epoch(
    ctx: &mut EpochContext<'_>,
    affected_path_ids: &HashSet<u64>,
    deferred_paths: &HashSet<u64>,
    solve_block: u64,
    cycle_start: std::time::Instant,
    solve_span: &tracing::Span,
) -> SolvePhase {
    let solved_bins;
    // Follow (trace f701ccd36f4ecf80d671e798df218fa4, block
    // 25906841): the window between the close of `arb.resolve` and the
    // open of `arb.lpt` was uninstrumented — 647 ms of wall time on that
    // cold-ramp cycle, ~25 µs/path steady state. The work (results sweep
    // + resolved-snapshot staging) now runs under its own phase span so
    // the pre-LPT cost stays attributable in Jaeger like its fanout/
    // resolve/lpt/merge siblings.
    let stage_span = tracing::info_span!(
        target: "degenbot::solver",
        "degenbot.arb.stage",
        block.number = solve_block,
        paths.affected = affected_path_ids.len(),
        paths.staged = tracing::field::Empty,
    );
    let stage_ctx = stage_span.enter();
    // Remove old results for affected paths (they'll be re-solved below).
    // A deferred path's result is dropped too: it is excluded from this
    // live solve (its pool is stale, so its prior result is stale as well).
    for &path_id in affected_path_ids {
        ctx.results.remove(&path_id);
    }
    // Solve only the non-deferred affected set.
    let solve_path_ids: HashSet<u64> = affected_path_ids
        .iter()
        .filter(|&&p| !deferred_paths.contains(&p))
        .copied()
        .collect();
    // Solve affected paths and insert new results.
    //
    // The solve fans out across executor bins over the affected-path set. The
    // solve dispatch is a free-standing function
    // (no cycle-state read); each work item takes the `path_id` + an **Arc-
    // shared** `ResolvedMixedPath` snapshot (f701ccd3 staging fix: the
    // former per-path deep clone copied every concentrated liquidity
    // `IntV3TickRangeSequence` every cycle — the "clone is cheap" claim
    // was disproven by telemetry at ~25 µs/path steady state, 150-420
    // µs/path on the cold-heap ramp), `path_resolved` entries being
    // immutable between resolve passes. Workers then write — under the
    // parallel closure — into the engine-level result-set via a
    // `Mutex`-free pattern: collect `(path_id, SolvePathResult)` pairs
    // into a Vec, then merge sequentially into `ctx.results`. The
    // parallel workers touch NO engine state and NO core.lock —
    // engine-then-core lock ordering is preserved unchanged (the
    // internal thread pool never re-enters the engine `Mutex`). For tiny
    // batches the dispatch overhead is bounded by the lazy
    // split (see `par_iter` docs); the sequential cost dominates below
    // executor internals.
    //
    // Pre-collect the work items (path_id + resolved-snapshot). The Arc
    // clones drop the immutable borrow on `ctx.path_resolved` that
    // would block parallel dispatch.
    let mut invalid_count: u64 = 0;
    let to_solve: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)> = solve_path_ids
        .iter()
        .filter_map(|&pid| {
            let resolved = ctx.path_resolved.get(&pid)?;
            if !resolved.valid {
                invalid_count += 1;
                return None;
            }
            // A path whose `max_update_block` is AHEAD of the drain
            // `block_number` is LIVE head state (the pools advanced by
            // backfill), not poison — it is correctly re-anchored at
            // `solve_block` above (B2); skipping it would DROP a capturable
            // opportunity. The genuinely-future case (`update_block >
            // solve_block`) is already rejected by the belt-and-
            // suspenders guard in the gate loop above, which removes the
            // path from `solve_path_ids` entirely.
            Some((pid, std::sync::Arc::clone(resolved)))
        })
        .collect();
    drop(stage_ctx);
    stage_span.record("paths.staged", to_solve.len());
    drop(stage_span);
    // Filter out empty/profitless results in the same pass that produces
    // them — the contract is identical to the prior serial loop.
    // per-path wall time is captured so the K slowest paths can be
    // named on the completion event (a min-heap keeps this O(K) memory;
    // the closure itself only does one Instant pair + map insert).
    // (time_us, pieces_visited, path_sims, pid) for the K-slowest
    // attribution — lets the completion event name the walk-combinatorial
    // cost driver of the slowest routes, not just their wall time.
    // -----------------------------------------------------------------
    // per-cycle shared solve context. The pure solver phase
    // is a pure function of the resolved snapshots + this context:
    // workers (the dedicated tokio executor bins) hold Arc
    // CLONES and touch NO engine state, NO core.lock - engine-then-core
    // lock ordering is preserved unchanged. The SINGLE engine-Mutex hold
    // still covers the whole cycle (the drain-side merge happens before
    // the epoch returns), so cycle atomicity - the results.remove
    // above, pending_new_paths, results_block stamping - is preserved
    // by construction with NO epoch guards.
    // -----------------------------------------------------------------
    // Degenerate-path capture config: env parsed ONCE per cycle
    // at the owner; the gate itself reads no environment. The prefix-
    // composition cache is generationed by the block epoch inside the
    // gate deps (no public reset to call anymore).
    let gate_capture = gate_capture_from_cfg(ctx.cfg);
    // Optional offline concentrated-liquidity-solver capture (DEGENBOT_SOLVER_CAPTURE=1): dump
    // the exact all-concentrated-liquidity pool state the solver consumed for heavy paths so
    // the concentrated liquidity solver can be optimized offline. None (no-op) unless gated.
    let capture = HeavyPathCapture::from_capture(
        &ctx.cfg.capture,
        CaptureVariant::HeavyConcentratedLiquidity,
    );
    // Optional mixed V2+concentrated liquidity solver capture (same gate): heavy
    // mixed paths (e.g. path 7042 V2->V3->V3) dispatch to
    // `solve_piecewise_cached`, which the all-concentrated-liquidity capture skips.
    // Defaults OUT of the fixtures dir (loop-18: working rows never
    // accrete there; goldens are produced only by concentrated_liquidity_capture_gen).
    let capture_mixed =
        HeavyPathCapture::from_capture(&ctx.cfg.capture, CaptureVariant::HeavyMixed);
    // SIMPIPE2 T2: pool-ref snapshot aligned to `to_solve` order (the
    // worker clamp's pool list) — captured under this cycle's engine
    // Mutex so it cannot interleave with a re-registration.
    // The snapshot is one Arc bump per path - path_pools
    // values are Arc<MixedPath>, immutable between register/deregister.
    let pool_refs: Vec<std::sync::Arc<MixedPath>> = to_solve
        .iter()
        .map(|(pid, _)| {
            ctx.registry
                .get(*pid)
                .cloned()
                .unwrap_or_else(|| std::sync::Arc::new(MixedPath { pools: Vec::new() }))
        })
        .collect();
    let shared = std::sync::Arc::new(SolveCycleShared {
        solve_block,
        metadata: *ctx.metadata,
        gate_capture,
        scope: ctx.scope.clone(),
        runtime: ctx.runtime_cfg,
        path_cost_ledger: crate::arb_engine::path_cost_ledger::PathCostLedger::new(
            solve_block,
            capture.map(std::sync::Arc::new),
            capture_mixed.map(std::sync::Arc::new),
            std::sync::Arc::clone(ctx.path_cost_history),
        ),
        #[cfg(test)]
        test_solve_delay: ctx.test_solve_delay.clone(),
        #[cfg(test)]
        test_solve_panic: ctx.test_solve_panic.clone(),
        core: std::sync::Arc::clone(ctx.core),
        pool_refs,
        min_profit: ctx.min_profit_floor,
        clamp_margin: ctx.clamp_margin,
        worker_clamp: ctx.inline_sim_enabled,
        inline_sim: ctx.inline_sim.clone(),
    });
    // The LPT bins are Arc-shared for every arm; the
    // arms index through the same deref (byte-identical semantics).
    let to_solve = std::sync::Arc::new(to_solve);
    // -----------------------------------------------------------------
    // DETACHED arm: the only solve arm
    // enqueue the SOLVES on a plain std::thread per LPT bin (riding
    // the fleet executor) and RETURN at enqueue end. Every result flows
    // through the unbounded mpsc to the merge sidecar, which applies the
    // Q1a stale policy under the engine Mutex.
    // -----------------------------------------------------------------
    // the arm decision IS the machine's begin_cycle — the ONE
    // seq tick and the merge-pipe open-once + Sender clone all live on
    // the machine now. The stance input, the cap verdict, and
    // the in-cycle fallback retired; a positive-draw cycle ALWAYS
    // detaches (the draw already owns backpressure; there is no
    // post-begin race-shed).
    let arm = ctx.detached_cycle.begin_cycle();
    let DetachedArm {
        cycle_seq,
        merge_tx,
    } = arm;
    let solved_seq = cycle_seq;
    hotpath::measure_block!("arb_solve.detached_enqueue", {
        // The Q1a freshness oracle: each item rides the per-hop
        // `pool_update_block` snapshot stamped by THIS cycle's
        // resolve phase (above); the sidecar re-reads the live
        // clocks and drops on ANY mismatch.
        let enqueue_stamps: std::sync::Arc<HashMap<u64, Vec<u64>>> = std::sync::Arc::new(
            to_solve
                .iter()
                .filter_map(|(pid, _)| {
                    ctx.resolved_update_snapshot
                        .get(pid)
                        .map(|stamp| (*pid, stamp.clone()))
                })
                .collect(),
        );
        let bins = lpt_bins(&to_solve, &shared);
        // Copy the cycle metadata out: the 'static bin threads
        // outlive the caller's &BlockMetadata borrow.
        let cycle_metadata = *ctx.metadata;
        // The gauge is machine-owned: the ISSUE half is the hook
        // below (bin threads bump it at SEND time — so a bin that
        // dies before sending NEVER leaks a count); the RECEIPT half
        // is the sidecar's solved_received.
        let n_bins = bins.len();
        solved_bins = n_bins;
        for (bin_idx, bin) in bins.into_iter().enumerate() {
            let shared_bin = std::sync::Arc::clone(&shared);
            let to_solve_bin = std::sync::Arc::clone(&to_solve);
            let stamps_bin = std::sync::Arc::clone(&enqueue_stamps);
            let tx = merge_tx.clone();
            let solve_span_bin = tracing::Span::clone(solve_span);
            // the submitted pids (the bin's owed list) are
            // computed from the bin EXACTLY as the in-cycle arm's
            // lane_pids — the lane witness seeds its owed set from
            // the same source the hold vec's pids come from.
            let lane_pids: Vec<u64> = bin.iter().map(|&i| to_solve[i].0).collect();
            // the bin's submit closure — send on the DETACHED
            // merge pipe and bump the in-flight gauge at SEND success
            // ONLY (variant-gated pairing: Suppressed
            // and Failed ride lane.solved/lane.failed, which touch
            // neither the pipe gauge bump nor this closure). A bin
            // that dies before sending never leaks a count.
            // (fix for the breaker over-disposition): the bump now
            // rides THE LANE's Solved send (`SolveLane::solved`),
            // installed below as the lane's gauge hook — so the
            // witness's `emitted` set (the double-delivery guard:
            // every pid released through the lane is excluded from
            // the post-panic patch) and the gauge bump stay in ONE
            // send path. The old standalone closure bypassed
            // `emitted`, so a panicked bin patched Failed records
            // for pids already sent (an over-disposition the
            // detached_undercount breaker caught at 4-vs-3).
            // The gauge hook is 'static (the lane outlives this
            // bin body; an Arc-shared atomic carries the bump) —
            // the machine's ISSUE half:
            let gauge_bump: std::sync::Arc<dyn Fn() + Send + Sync> =
                ctx.detached_cycle.gauge_hook();
            // Bin jobs ride the fleet executor (fleet.stance=fleet) or
            // the dedicated tokio solve executor (persistent warm
            // workers). The body
            // is unchanged; same 'static + Send move semantics, and
            // concurrent detached cycles share the persistent
            // worker set instead of forking one thread per bin.
            // the ONE lane walk —
            // the old detached run_bin body is the shared walk
            // below; arm differences ride the policy + stamp ctx.
            let run_bin = move |lane: &mut SolveLane| {
                let walk_plan = LaneWalkBinPlan {
                    items: std::sync::Arc::clone(&to_solve_bin),
                    indices: bin,
                };
                // THE ARM POLICY (detached enqueue): stamped
                // envelopes ride the walk; Suppressed/Failed keep
                // the keyless no-claim witness (contract 4).
                let lane_policy = LaneArmPolicy {
                    ledger_seq: cycle_seq,
                    solve_block,
                    metadata: cycle_metadata,
                };
                let stamp_ctx = WalkSubmitCtx {
                    cycle_seq,
                    update_stamps: Some(std::sync::Arc::clone(&stamps_bin)),
                    solve_span: Some(solve_span_bin.clone()),
                };
                let reads = drive_lane_walk(
                    &shared_bin,
                    &solve_span_bin,
                    &walk_plan,
                    &lane_policy,
                    &stamp_ctx,
                    lane,
                );
                debug_assert_eq!(
                    reads.held_unflushed, 0,
                    "detached walk must flush every Solved item before the bin body returns"
                );
            };
            // The detached arm
            // seals its bins with the lane witness. The explicit
            // `&LaneCtx` annotation is load-bearing: without it the
            // closure-parameter inference drifts and `run_bin`'s lane
            // binding re-derives. With the witness, a panicked
            // detached bin's undelivered pids arrive as typed
            // `Failed` records on the merge pipe (the silent-
            // undercount gap is closed).
            let lane_key =
                SOLVE_BIN_KEY_BASE.saturating_add(u64::try_from(bin_idx).unwrap_or(u64::MAX));
            let spawn_job = move |_ctx: &LaneCtx| {
                // Gauge pairing: the in-flight bump rides the
                // lane's Solved send-success (send + bump + emitted
                // in one path). Failed/Suppressed sends never fire
                // it (REV 2 Defect 1) — the corresponding disposition
                // arm never decrements.
                //
                // the same lane carries the drain-death
                // hook — a terminal send failure on the merge pipe
                // is counted and trips the sticky cordon instead of
                // being swallowed.
                //
                // Both hooks are fused into the constructor,
                // so a constructed lane can never be driven without
                // them.
                let mut lane = SolveLane::new(
                    lane_key,
                    lane_key,
                    lane_pids,
                    tx.clone(),
                    Some(gauge_bump.clone()),
                    Some(std::sync::Arc::new(|failure| {
                        crate::arb_engine::executor::drain_death_response(failure, None);
                    })),
                );
                run_solve_lane(&mut lane, &SeatSurvivesPolicy, run_bin);
            };
            // LW-T8: both arms submit through the ONE Executor
            // token (the fleet has been the sole executor since the
            // LW-T9 cutover).
            if let Err(err) =
                crate::arb_engine::executor::global_executor().submit(bin_idx, Box::new(spawn_job))
            {
                crate::arb_engine::fleet_solve_executor::abort_loud(
                    "bin submission, posture gate",
                    &format!("{err}"),
                );
            }
        }
        ctx.detached_cycle.publish_gauge();
        diag!(
            domain = solver,
            block_number = solve_block,
            detached_seq = cycle_seq,
            detached_bins = n_bins,
            paths.enqueued = to_solve.len(),
            paths.invalid = invalid_count,
            paths.deferred_future_price = deferred_paths.len(),
            phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            "detached cycle enqueued (merge runs on the sidecar)"
        );
    });
    SolvePhase {
        staged: to_solve.len(),
        invalid: invalid_count,
        deferred: deferred_paths.len(),
        bins: solved_bins,
        seq: solved_seq,
    }
}
