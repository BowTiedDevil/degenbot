//! The epoch's resolve phase: the solve-block anchor, the dirty-status scan
//! that turns the drawn ledger keys into affected paths, and the hop
//! re-resolve that refreshes the context's resolved snapshots and status rows
//! under one core-lock window.
//!
//! Machine-direct free functions over the epoch's [`EpochContext`]: the phase
//! mutates the context's caches cluster and hands back only the census facts
//! the spine and the later solve/deliver phases read.

use super::EpochContext;
use crate::bot_core::solve_anchor::SolveAnchor;
use ::degenbot_pools::solve::ResolvedMixedPath;
use degenbot_core::{diag, op_error};
use degenbot_pools::AffectedKey;
use degenbot_substrate::resolve::resolve_hops;
use hashbrown::{HashMap, HashSet};
use std::sync::PoisonError;

// ---------------------------------------------------------------------------
// statics/fns from the retired grab file, each
// moved beside its sole production consumer (or its near consumers).
// ---------------------------------------------------------------------------
/// chunked parallel resolve of the affected paths (sharded hop
/// cache preserves cross-path hit reuse). Default ON; set
/// `DEGENBOT_SOLVE_RESOLVE_PAR=0` for the serial A/B fallback.
const RESOLVE_CHUNK: usize = 256;
const RESOLVE_PAR_MIN: usize = 512;
struct ResolveChunkOut {
    resolved: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)>,
    status: Vec<(u64, Vec<degenbot_substrate::resolve::HopDeficit>)>,
    snapshots: Vec<(u64, Vec<u64>)>,
    same_state: u64,
    projections: u64,
    invalid_reasons: HashMap<String, u64>,
    deferred: Vec<u64>,
}

/// The dirty-status scan: the drawn ledger keys become the affected-path set
/// through the registry's reverse index, and each touched path's state
/// machine decides whether it must (re)resolve.
pub(super) fn scan_dirty_status(
    ctx: &mut EpochContext<'_>,
    affected: &[AffectedKey],
) -> HashSet<u64> {
    // Collect affected path IDs from the reverse index
    let mut affected_path_ids: HashSet<u64> = HashSet::new();
    // the state machine decides which touched paths actually need a
    // (re)resolve. Solvable/Unresolved re-check on any hop dirty; an Invalid
    // path re-checks ONLY when a responsible pool goes dirty AND the
    // container empties (last faulty pool cleared). Unrelated co-hop dirt
    // leaves an Invalid path untouched — no 100k-path re-resolve churn.
    hotpath::measure_block!("arb_solve.dirty_status_scan", {
        // the affected keys ARE the delta's taken (HopType,
        // pool_id) reverse-index keys — one loop, no per-family intake.
        for key in affected {
            if let Some(path_ids) = ctx.registry.paths_for(&key.path_index_key()) {
                for &path_id in path_ids {
                    if ctx
                        .path_status
                        .entry(path_id)
                        .or_default()
                        .on_pool_dirty(key.path_index_key())
                    {
                        affected_path_ids.insert(path_id);
                    }
                }
            }
        }
    });
    affected_path_ids
}

/// The solve-block anchor: the block the pool state actually reflects, never
/// the possibly-lagging drain block.
pub(super) fn resolve_anchor(ctx: &EpochContext<'_>, block_number: u64) -> SolveAnchor {
    // Solve-block anchor (rule owner + history: `crate::bot_core::solve_anchor`):
    // the batch's `solve_block` (= `results_block`) is the block the pool
    // state actually reflects — the pool-state head, NOT the
    // (possibly-lagging) drain `block_number`. Since the fix, the pump
    // pre-promotes `active_block` before calling `on_drain`, so
    // `block_number` here is already >= the head and the re-anchor is a
    // defensive no-op on the pump path — it stays the guard for callers
    // that bypass the pump (e.g. tests driving `solve_dirty` directly).
    crate::bot_core::solve_anchor::SolveAnchor::resolve(
        block_number,
        &ctx.core
            .read_at(degenbot_substrate::state_lock::LockSite::Solver),
    )
}

/// The census facts the resolve phase hands back to the epoch spine: the
/// same-state reuse count, the projection count, and the deferred
/// (future-price) paths.
pub(super) struct ResolvePhase {
    pub(super) same_state: u64,
    pub(super) projections: u64,
    pub(super) deferred: HashSet<u64>,
}

/// The hop re-resolve over the affected paths: one core-lock window, the
/// chunked fan-out, and the serial deterministic merge into the context's
/// resolved snapshots, status rows, and reuse counter.
#[expect(clippy::too_many_lines)] // chunked fan-out + serial merge + the phase's own telemetry
pub(super) fn resolve_epoch(
    ctx: &mut EpochContext<'_>,
    anchor: &SolveAnchor,
    solve_block: u64,
    affected_path_ids: &HashSet<u64>,
    cycle_start: std::time::Instant,
) -> ResolvePhase {
    let mut same_state_total;
    let mut projections_total;
    // Re-resolve and solve only affected paths — update results in-place
    // without cloning unchanged entries.
    // Re-derive resolved hop states under the core lock — a single
    // consistent snapshot of BotState for the whole re-derive (ADR-003
    // Option A: one core-lock window per `solve_dirty`). The guard drops
    // before `solve_path` runs, which is pure `&self`.
    //
    // Lesson: a per-path `update_block`-MIX freshness gate was
    // attempted here and REVERTED — it deferred every legitimate
    // single-pool-update arb (Sync pool A, solve with a stable reference
    // pool B at an older `update_block`). A zero-tolerance `update_block`
    // check cannot distinguish "this pool had no block-N event" (normal)
    // from "this pool is genuinely far behind" (missed swap events), so
    // its false-positive rate is catastrophic.
    //
    // NO solve-time staleness gate here. The former
    // bounded-window gate deferred a whole path on any co-hop trailing
    // >10 blocks, but `update_block` is a last-activity clock, so a quiet-but-
    // current pool was falsely deferred (3,550 proved live).
    // `deferred_paths` is now reserved for the genuinely illegitimate future-
    // price case below; genuine chain/solver divergence is left to the ADR-021
    // verifier, which fatal-aborts loudly (the preferred failure, esp. in dev).
    *ctx.paths_same_state_this_cycle = 0;
    // these accumulate from the chunk merges inside the resolve
    // block below (same content the serial loop used to produce inline).
    let mut deferred_paths: HashSet<u64> = HashSet::new();
    let mut invalid_reasons: HashMap<String, u64> = HashMap::new();
    // clone the deferred re-record hook out before the resolve
    // borrows `self` (Arc bump, cheap); it fires at the deferral site
    // below with the deferred paths' hop-pool keys.
    let deferred_re_record = ctx.deferred_re_record.clone();
    // phase span for the core-lock re-derive window (the
    // summary event after the block stays on the same node).
    let resolve_ctx = tracing::info_span!(
        target: "degenbot::solver",
        "degenbot.arb.resolve",
        block.number = solve_block,
        paths.affected = affected_path_ids.len(),
    )
    .entered();
    // chunked fan-out over the affected paths. The
    // sharded hop cache keeps cross-path hit reuse intact (a chunk-local
    // cache would multiply the expensive concentrated liquidity tick-walk per shared pool),
    // so chunks contend only on shard locks, never on each other's work.
    // Per-path chunk outputs merge serially below in deterministic order;
    // `resolve_hops` semantics are byte-identical (same core-read window,
    // same deficits, same memo validation).
    hotpath::measure_block!("arb_solve.resolve", {
        // Violated only while a writer is queued (parking_lot read acquire):
        // nonzero = core-lock congestion, not compute.
        let core = hotpath::measure_block!(
            "resolve.core_read_acquire",
            ctx.core
                .read_at(degenbot_substrate::state_lock::LockSite::Solver)
        );
        let resolve_chunk = |path_ids: &[u64]| -> ResolveChunkOut {
            let mut out = ResolveChunkOut {
                resolved: Vec::new(),
                status: Vec::new(),
                snapshots: Vec::new(),
                same_state: 0u64,
                projections: 0u64,
                invalid_reasons: HashMap::new(),
                deferred: Vec::new(),
            };
            for (chunk_pos, &path_id) in path_ids.iter().enumerate() {
                let Some(path) = ctx.registry.get(path_id) else {
                    continue;
                };
                // a hop whose PRICE
                // clock runs ahead of the solve anchor is never legitimate and is
                // rejected loudly (deferred + logged), not solved — a future-price
                // solve reports a misleading downstream IIA. Rule owner:
                // `crate::bot_core::solve_anchor`; after the head floor a hop can
                // beat the anchor only on a mid-solve state advance (belt +
                // suspenders, normally unreachable).
                // Reuse ceiling probe: compare the
                // hop update-block snapshot against the previous cycle's
                // recorded one. Byte-identical ⇒ the solve intake (every hop
                // state) is unchanged since the stored result was produced.
                // One pool walk builds the snapshot, the
                // same-state comparison AND the future-price check -
                // the per-path future probe re-walked all pools.
                let mut update_snapshot: Vec<u64> = Vec::with_capacity(path.pools.len());
                // Diagnostic override (the real future-price
                // tripwire is unreachable after the solve-anchor head
                // floor, so a forced set exercises the carry).
                let mut future = ctx
                    .force_deferred
                    .as_ref()
                    .is_some_and(|forced| forced.contains(&path_id));
                for pool_ref in &path.pools {
                    let ub = core.pool_update_block(pool_ref.pool_key);
                    if anchor.is_future(ub) {
                        future = true;
                    }
                    update_snapshot.push(ub);
                }
                let same_state = ctx
                    .resolved_update_snapshot
                    .get(&path_id)
                    .is_some_and(|prev| *prev == update_snapshot);
                out.snapshots.push((path_id, update_snapshot));
                if same_state {
                    out.same_state += 1;
                }
                if future {
                    out.deferred.push(path_id);
                    op_error!(
                        domain = solver,
                        "path_id={path_id} rejected at solve block {solve_block}: \
                         a hop price clock runs AHEAD of the solve block (update_block > \
                         solve_block) — never legitimate"
                    );
                    continue;
                }
                let mut resolved = ResolvedMixedPath::default();
                let mut chunk_projections = out.projections;
                let deficits = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    ctx.hop_projection_cache,
                    Some(&mut chunk_projections),
                    ctx.concentrated_liquidity_projection_memo,
                );
                out.projections = chunk_projections;
                for d in &deficits {
                    *out.invalid_reasons
                        .entry(d.reason.to_string())
                        .or_insert(0u64) += 1u64;
                    diag!(domain = solver, %path_id,
                        hop_type = ?d.hop_type,
                        pool_key = d.pool_key,
                        reason = %d.reason,
                        "path invalid at resolve"
                    );
                }
                out.resolved.push((path_id, std::sync::Arc::new(resolved)));
                // drive the path state machine from the full deficit set.
                out.status.push((path_id, deficits));
                let _ = chunk_pos;
            }
            out
        };
        // Deterministic chunking: hashbrown iteration order varies per
        // process; a sorted snapshot keeps chunk boundaries (and thus
        // debug-log ordering) identical across runs for ~microsecond cost.
        let mut affected_vec: Vec<u64> = affected_path_ids.iter().copied().collect();
        affected_vec.sort_unstable();
        let chunk_outs: Vec<ResolveChunkOut> = hotpath::measure_block!("resolve.chunks", {
            if !ctx.resolve_par_stance || affected_vec.len() < RESOLVE_PAR_MIN {
                vec![resolve_chunk(&affected_vec)]
            } else {
                // the resolve chunk fan-out leaves rayon with
                // the hard cutover (the rayon global pool retires).
                // Chunk boundaries stay byte-identical (the sorted
                // snapshot chunked at RESOLVE_CHUNK); the parallel
                // window moves onto short-lived scoped std::threads —
                // thread t takes chunks t, t+N, … and the index-keyed
                // collect restores the deterministic merge order.
                let chunk_ids: Vec<&[u64]> = affected_vec.chunks(RESOLVE_CHUNK).collect();
                let n_threads =
                    degenbot_core::cpu_budget::solve_worker_count().min(chunk_ids.len());
                // A panicking child propagates out of `thread::scope` (the
                // join is implicit) — the loud-failure posture the rayon
                // join used to have. Results stage behind a mutex AFTER
                // each chunk resolves (never held during the resolve)
                // and the index sort restores the deterministic merge
                // order; slot coverage is structural (round-robin over
                // the chunk list), so no slot dummy is needed.
                let staged: std::sync::Mutex<Vec<(usize, ResolveChunkOut)>> =
                    std::sync::Mutex::new(Vec::new());
                std::thread::scope(|scope| {
                    for t in 0..n_threads {
                        let staged_ref = &staged;
                        let chunk_ids_ref = &chunk_ids;
                        scope.spawn(move || {
                            let local: Vec<(usize, ResolveChunkOut)> = (t..chunk_ids_ref.len())
                                .step_by(n_threads)
                                .map(|i| (i, resolve_chunk(chunk_ids_ref[i])))
                                .collect();
                            let mut ready =
                                staged_ref.lock().unwrap_or_else(PoisonError::into_inner);
                            ready.extend(local);
                        });
                    }
                });
                let mut chunk_pairs = staged.into_inner().unwrap_or_else(PoisonError::into_inner);
                chunk_pairs.sort_unstable_by_key(|(i, _)| *i);
                chunk_pairs
                    .into_iter()
                    .map(|(_, out)| out)
                    .collect::<Vec<ResolveChunkOut>>()
            }
        });
        // Serial, deterministic merge (the engine mutex is held by this
        // cycle, so no other task can race these stores).
        same_state_total = 0u64;
        projections_total = 0u64;
        hotpath::measure_block!("resolve.merge", {
            for ResolveChunkOut {
                resolved,
                status,
                snapshots,
                same_state,
                projections,
                invalid_reasons: chunk_invalid,
                deferred,
            } in chunk_outs
            {
                same_state_total += same_state;
                projections_total += projections;
                deferred_paths.extend(deferred);
                for (path_id, snapshot) in snapshots {
                    ctx.resolved_update_snapshot.insert(path_id, snapshot);
                }
                for (path_id, arc) in resolved {
                    ctx.path_resolved.insert(path_id, arc);
                }
                for (path_id, deficits) in status {
                    ctx.path_status
                        .entry(path_id)
                        .or_default()
                        .set_resolved(&deficits);
                }
                for (reason, count) in chunk_invalid {
                    *invalid_reasons.entry(reason).or_insert(0u64) += count;
                }
            }
            *ctx.paths_same_state_this_cycle = same_state_total;
            // Lifetime counter (the serial loop accumulated in place).
            *ctx.hop_projection_count += projections_total;
        });
    });
    // the ledger carry for deferred paths — re-record EVERY hop
    // pool of each deferred path into the CURRENT cycle's bucket so the
    // next draw re-includes it through the same freshness ordering,
    // admission budget, and retention window. This folds the future-price
    // deferral onto the one retained-lead mechanism (no second queue, no
    // cycle-local carry vector). Pids are sorted for a deterministic
    // payload; keys ride path order. When the hook is unset (direct
    // engine drives — unit tests, the cold-start `solve_all`), the
    // deferred path keeps today's dropped behavior and its retry stays
    // log-driven.
    if let Some(re_record) = deferred_re_record {
        if !deferred_paths.is_empty() {
            let mut deferred_sorted: Vec<u64> = deferred_paths.iter().copied().collect();
            deferred_sorted.sort_unstable();
            let mut keys: Vec<degenbot_pools::AffectedKey> = Vec::new();
            for path_id in deferred_sorted {
                if let Some(path) = ctx.registry.get(path_id) {
                    keys.extend(path.pools.iter().map(|pool_ref| {
                        degenbot_pools::AffectedKey::new(pool_ref.hop_type, pool_ref.pool_key)
                    }));
                }
            }
            if !keys.is_empty() {
                re_record(&keys, solve_block);
            }
        }
    }
    // Per-cycle resolve funnel (hotpath_gauge{key=...}). Reason keys come
    // from the closed HopDeficit-reason set, so the series family stays
    // bounded.
    hotpath::gauge!("resolve_paths_affected").set(f64::from(
        u32::try_from(affected_path_ids.len()).unwrap_or(u32::MAX),
    ));
    hotpath::gauge!("resolve_paths_same_state").set(f64::from(
        u32::try_from(*ctx.paths_same_state_this_cycle).unwrap_or(u32::MAX),
    ));
    hotpath::gauge!("resolve_paths_deferred").set(f64::from(
        u32::try_from(deferred_paths.len()).unwrap_or(u32::MAX),
    ));
    // Dynamic-key gauge: the no-op `gauge!` discards its `$key` tokens, so
    // this loop only compiles in instrumented builds (`reason` would be
    // unused otherwise — zero-cost default builds are the design contract).
    #[cfg(feature = "hotpath")]
    for (reason, count) in &invalid_reasons {
        hotpath::gauge!(format!("resolve_invalid_{reason}"))
            .set(f64::from(u32::try_from(*count).unwrap_or(u32::MAX)));
    }
    diag!(domain = solver, block_number = solve_block,
        paths.resolved = affected_path_ids.len(),
        paths.same_state = *ctx.paths_same_state_this_cycle,
        hop.projections = *ctx.hop_projection_count,
        paths.deferred_future_price = deferred_paths.len(),
        invalid.reasons = %invalid_reasons.iter().map(|(r, c)| format!("{c}x {r}")).collect::<Vec<_>>().join(", "),
        phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
        "resolved hop snapshots"
    );
    drop(resolve_ctx);
    ResolvePhase {
        same_state: same_state_total,
        projections: projections_total,
        deferred: deferred_paths,
    }
}
