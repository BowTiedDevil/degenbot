//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::{I256, U256};
use rayon::prelude::*;

use ::degenbot_pools::v3_state::{v3_simulate_swap, V3PoolState};
use ::degenbot_pools::v4_state::v4_simulate_swap;

use super::{ArbitrageEngine, BlockMetadata, HashMap, HashSet};

// There is deliberately NO solve-time "staleness" pre-gate here (ergo YXHHKR,
// resolved QNFYR5). The former TQ43TU `hop_is_too_stale` gate deferred a whole
// path on any co-hop whose price-clock `update_block` trailed > 10 blocks — but
// `update_block` is a last-activity clock, so a pool that swapped once and then
// went quiet (state byte-identical to on-chain) was falsely deferred: QNFYR5's
// instrumented live run showed 3,550 such defers (V2/V3/V4, gap 11-16, 0 genuine)
// with a healthy engine solve→sim path underneath. A static age check cannot
// distinguish "quiet but current" from "genuinely moved but only moderately
// behind" (AV42C7 — the zero-tolerance retread was already REVERTED for the same
// over-deferral). The accurate discriminator requires a fresh on-chain read, which
// the ADR-021 tripwire (`solver_state_tripwire::judge`) already performs at
// the publish point, diffing each hop at its OWN `update_block` anchor and
// `std::process::abort`ing on the first real desync before simulation. That
// tripwire — not an age heuristic — is the sole chain/solver-mismatch guard; on a
// genuine stale/desync pool it fails HARD and LOUDLY, which is the preferred
// behavior (develop on loud failures).

use crate::bot_core::resolve::resolve_hops;
use ::degenbot_solvers::mixed::{HopType, ResolvedMixedPath, SolvePathResult};

/// How many slowest-path entries the solve-cycle completion event names
/// (D63GSE intra-solve visibility).
const SLOWEST_PATHS_K: usize = 5;

impl ArbitrageEngine {
    /// The CL-hop clamp margin (absolute wei, subtracted from `input_consumed`
    /// before it is committed). VAASFM decision: 1 wei — commit
    /// `input_consumed - 1` so the exact-in loop converts nearly everything and
    /// stops on `amountRemaining==0` at the last funded tick. 1 wei is the
    /// maximum-extraction choice; a larger margin can be revisited if runaway
    /// swaps recur. Override via the `CLAMP_MARGIN` env var for sensitivity
    /// sweeps (twin of the `path5000_v2v4v3_solver_fixture` fixture).
    ///
    /// ## Measured basis (ergo 7E5D7W)
    ///
    /// The margin must be strictly larger than the worst solver-vs-engine
    /// (solver `hop_outputs[i]` vs the tier-3-proven `v4_simulate_swap`/
    /// `v3_simulate_swap` pool twin) OVER-prediction, so the clamp never lands
    /// exactly on an over-predicted tight value and re-enters the EMPTY march
    /// (UO3JM4). The `v4_crossing_solver_vs_sim_parity`/
    /// `v4_word_boundary_solver_divergence`/`v4_fee1_solver_path_matches_v4_simulate_swap`
    /// suites assert byte-exact solver==twin across the fee-3000/ts-60 multi-tick
    /// corpus AND the fee-1/ts-1 low-fee topology in both swap directions — i.e.
    /// the worst observed over-prediction is **0 wei**. The historical live
    /// `+1..+3` wei residuals (fee-1, ts=1) were localized to crossing-math
    /// rounding and fixed (the zfo step-0 current-tick flooring), not absorbed
    /// by margin. A dedicated sweep
    /// (`cl_hop_clamp_margin_exceeds_worst_solver_over_prediction`) measures the
    /// strict over-prediction direction across the corpus and asserts
    /// `margin > worst`, guarding this choice against regression. 1 wei is the
    /// smallest positive integer > 0, giving zero extraction loss (path-5000
    /// fixture: clamped output == solver output byte-identical).
    fn cl_hop_clamp_margin() -> U256 {
        std::env::var("CLAMP_MARGIN")
            .ok()
            .and_then(|s| s.parse::<u128>().ok())
            .map_or_else(|| U256::from(1u128), U256::from)
    }

    /// Post-solve, pool-state-aware reconciliation of each CL hop's committed
    /// input against the pool's true max-convertible capacity — the tier-3-
    /// validated `v3_simulate_swap`/`v4_simulate_swap` twin (UO3JM4: the pure
    /// solver's frozen int walk can over-predict the pools twin by a few wei,
    /// so the authoritative bound comes from pool state, not the solver).
    ///
    /// `solve_path` runs lock-free on its `IntV3TickRangeSequence` snapshot
    /// (ADR-015: the guard drops before the rayon `par_iter`) and reports
    /// `consumed_inputs[i] = hop_outputs[i-1]` — the FULL forward, which can
    /// over-feed a CL pool past its on-chain capacity. When that happens the
    /// exact-in loop cannot exhaust the input and marches empty bitmap words to
    /// `MAX_SQRT_PRICE` (the path-5000 20.7M-gas / 5M-ceiling EMPTY-HALT class,
    /// AGENTS.md UO3JM4). This method re-reads the live
    /// `V3PoolState`/`V4PoolState` from the core at the solve→result merge seam
    /// and caps each CL hop's committed input to `input_consumed - margin`, so
    /// the on-chain loop exits on `amountRemaining==0` at the last funded tick.
    ///
    /// `hop_outputs[i]` is left untouched: for an over-feeding CL pool,
    /// `output(capacity) == output(over-feed)`, so the solver's predicted output
    /// is already correct (verified byte-exact by the path-5000 fixture). Only
    /// CL hops (V3/V4) have the word-boundary empty-march class; V2 / Curve /
    /// Balancer / Solidly consume their full input at the boundary and need no
    /// clamp.
    #[expect(clippy::too_many_lines)] // multi-hop CL twin loop + post-clamp profit recompute
    /// Returns the number of twin simulations executed (telemetry:
    /// `clamp.twins` on the solve-cycle completion event).
    pub(crate) fn clamp_cl_hop_capacity(&self, path_id: u64, result: &mut SolvePathResult) -> u64 {
        let Some(path) = self.path_pools.get(&path_id) else {
            return 0; // Unknown path → nothing to clamp
        };
        let pools = &path.pools;
        if pools.len() != result.consumed_inputs.len() {
            return 0; // Index misalignment — never clamp a wrong hop
        }
        let margin = Self::cl_hop_clamp_margin();
        let core = self.core.read();
        // D63GSE: successful twin simulations executed this call (returned to
        // the caller for the solve-cycle completion event).
        let mut twins_executed: u64 = 0;
        for (i, pool_ref) in pools.iter().enumerate() {
            let requested = result.consumed_inputs[i];
            // Run the tier-3-validated twin once per clamped family so we can
            // (a) clamp this hop's INPUT (CL marching empty-word EMPTY-HALT
            // class), (b) clamp this hop's FORWARD (`consumed_inputs[i+1]` =
            // the next hop's input, which the composer's V4 take/exchange
            // derives from this hop's OUTPUT) to the byte-exact twin output,
            // and (c) re-align this hop's REPORTED output. (b)/(c) close the
            // path-73385 class: the solver OVer-predicted the V4 output by
            // 3 wei, so the take (`consumed_inputs[i+1]`) over-took the pool's
            // actual output and the trailing V4_SETTLE_ALL repaid the 3-wei
            // residual via a `USDT.transfer(PM,3)` that halted (0xfe). (c) is
            // equally load-bearing for a V2 hop whose INPUT the upstream hop's
            // (b) just reduced: the walk-frozen `hop_outputs[i]` would
            // otherwise keep the pre-clamp input's output — the
            // path-182449/110302 1-wei over-prediction that failed on-chain
            // with `UniswapV2: K`.
            let (out, input_clamp): (U256, Option<U256>) = match pool_ref.hop_type {
                HopType::V3 => {
                    let (Some(state), Some(identity)) = (
                        core.get_v3_pool(pool_ref.pool_key),
                        core.get_v3_identity(pool_ref.pool_key),
                    ) else {
                        continue; // Pool state unavailable → can't clamp
                    };
                    let Ok(amount) = I256::try_from(requested) else {
                        continue; // Input too large for i256 → skip
                    };
                    let limit = V3PoolState::default_sqrt_price_limit(pool_ref.zero_for_one);
                    let Some(twin) = v3_simulate_swap(
                        state,
                        identity.fee,
                        identity.tick_spacing,
                        pool_ref.zero_for_one,
                        amount,
                        limit,
                    )
                    .ok() else {
                        continue;
                    };
                    let out = if pool_ref.zero_for_one {
                        twin.amount1
                    } else {
                        twin.amount0
                    };
                    twins_executed += 1;
                    (out, twin.exact_input_clamp_bound(requested, margin))
                }
                HopType::V4 => {
                    let (Some(state), Some(identity)) = (
                        core.get_v4_pool(pool_ref.pool_key),
                        core.get_v4_identity(pool_ref.pool_key),
                    ) else {
                        continue;
                    };
                    let Ok(amount) = I256::try_from(requested) else {
                        continue;
                    };
                    // V4 exact-in passes a NEGATIVE amount (opposite sign to V3).
                    let Some(neg) = amount.checked_neg() else {
                        continue; // MIN_i256 (no positive twin) → skip
                    };
                    let limit = V3PoolState::default_sqrt_price_limit(pool_ref.zero_for_one);
                    let Some(twin) = v4_simulate_swap(
                        state,
                        identity.pool_key.fee,
                        identity.pool_key.tick_spacing,
                        pool_ref.zero_for_one,
                        neg,
                        limit,
                    )
                    .ok() else {
                        continue;
                    };
                    let out = if pool_ref.zero_for_one {
                        twin.amount1
                    } else {
                        twin.amount0
                    };
                    twins_executed += 1;
                    (out, twin.exact_input_clamp_bound(requested, margin))
                }
                HopType::V2 => {
                    // V2 has no empty-march class (no input clamp), but its
                    // byte-exact twin output must still be the authoritative
                    // report once (b) has forward-clamped its input upstream.
                    // Orientation mirrors `simulate_swap`'s V2 arm.
                    let (Some(state), Some(identity)) = (
                        core.get_v2_pool_state(pool_ref.pool_key),
                        core.get_v2_identity(pool_ref.pool_key),
                    ) else {
                        continue; // Pool state unavailable → can't clamp
                    };
                    let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one
                    {
                        (
                            state.reserve0.to::<U256>(),
                            state.reserve1.to::<U256>(),
                            identity.fee_token0.0,
                            identity.fee_token0.1,
                        )
                    } else {
                        (
                            state.reserve1.to::<U256>(),
                            state.reserve0.to::<U256>(),
                            identity.fee_token1.0,
                            identity.fee_token1.1,
                        )
                    };
                    let Some(out) = degenbot_math::v2::IntHopState::new(
                        reserve_in,
                        reserve_out,
                        gamma_numer,
                        fee_denom,
                    )
                    .swap(requested)
                    .ok() else {
                        continue; // overflow reverts on-chain → nothing to align
                    };
                    twins_executed += 1;
                    (out, None)
                }
                // Curve / Balancer / Solidly — no byte-exact twin at this
                // seam; their reported outputs stand (see module note).
                _ => continue,
            };
            // (a) Input clamp: cap this CL hop's committed input at
            // `input_consumed - margin` when over-fed (the empty-march class).
            if let Some(clamped) = input_clamp {
                if clamped < requested {
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_clamp();
                    }
                    tracing::info!(
                        target: crate::telemetry::DIAGNOSTIC_TARGET,
                        "[clamp-cl] path_id={path_id} hop={i} family={:?} input requested={requested} \
                         clamped={clamped} reduction={}",
                        pool_ref.hop_type,
                        requested - clamped
                    );
                    result.consumed_inputs[i] = clamped;
                }
            }
            // (c) Align the solver's REPORTED output (`hop_outputs[i]`) to the
            // byte-exact twin output, so the solver is exact (not merely the
            // consumed forward). This is the path-73385 fix: the solver
            // over-predicted the V4 output by 3 wei; the twin is the on-chain
            // truth, so the published hop_outputs become byte-exact too.
            if let Some(hop_out) = result.hop_outputs.get_mut(i) {
                if *hop_out != out {
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_clamp();
                    }
                    tracing::info!(
                        target: crate::telemetry::DIAGNOSTIC_TARGET,
                        "[clamp-cl-hop] path_id={path_id} hop={i} family={:?} hop_outputs={hop_out} \
                         twin_out={out} delta={}",
                        pool_ref.hop_type,
                        if *hop_out > out { *hop_out - out } else { out - *hop_out }
                    );
                    *hop_out = out;
                }
            }
            // (b) Forward clamp: the next hop's executable input
            // (`consumed_inputs[i+1]` — what the composer's V4 take/exchange
            // withdraws from this hop's output) must not exceed this hop's
            // actual yield, or the pool is over-taken and a residual delta is
            // repaid via a failing USDT transfer (path-73385).
            if i + 1 < pools.len() {
                let forward = result.consumed_inputs[i + 1];
                if out < forward {
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_clamp();
                    }
                    tracing::info!(
                        target: crate::telemetry::DIAGNOSTIC_TARGET,
                        "[clamp-cl-out] path_id={path_id} hop={i} family={:?} forward={forward} \
                         twin_out={out} reduction={}",
                        pool_ref.hop_type,
                        forward - out
                    );
                    result.consumed_inputs[i + 1] = out;
                }
            }
        }

        // BUG-B FIX (path-142603 `no-profit` crash): the solver's `profit` is
        // computed on its RAW (over-predicted) hop outputs; the CL clamp above
        // realigns execution to the twin but was not feeding back a recomputed
        // profit, so an actually-unprofitable path stayed `> min_profit` and was
        // dispatched → executed to a loss → `no-profit` abort. Recompute the
        // selection profit from the clamped values (see `recompute_clamped_profit`);
        // a post-clamp loss saturates to 0 and is dropped.
        if let Some(recomputed) = Self::recompute_clamped_profit(result) {
            let profit_before = result.profit;
            if recomputed != profit_before {
                tracing::info!(
                    path_id,
                    profit_before = %profit_before,
                    profit_after = %recomputed,
                    profit_delta = %profit_before.saturating_sub(recomputed),
                    "[profit-clamp] recomputed selection profit from twin-aligned outputs"
                );
                result.profit = recomputed;
            }
        }
        twins_executed
    }

    /// Recompute a path result's selection profit from its CLAMPED
    /// (twin-aligned) outputs, per the documented `SolvePathResult::profit`
    /// semantics `final_output - consumed_inputs[0]` (with
    /// `final_output = hop_outputs[last]`), evaluated on the corrected values
    /// so it reflects the executable state rather than the solver's pre-clamp
    /// over-prediction. A post-clamp loss saturates to `0`, which is dropped by
    /// the `profit > min_profit` delivery gate. Returns `None` for a degenerate
    /// path (no `hop_outputs` / `consumed_inputs`). Pure (no env, no `core`
    /// lock) so it is directly unit-testable independent of the CL-twin
    /// machinery.
    #[must_use]
    fn recompute_clamped_profit(result: &SolvePathResult) -> Option<U256> {
        let final_output = result.hop_outputs.last().copied()?;
        let first_consumed = result.consumed_inputs.first().copied()?;
        Some(final_output.saturating_sub(first_consumed))
    }

    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    #[expect(clippy::too_many_lines)] // telemetry events + solve pipeline are one narrative
    pub fn rebuild_and_solve_affected(
        &mut self,
        v2_affected: &HashSet<u64>,
        v3_affected: &HashSet<u64>,
        v4_affected: &HashSet<u64>,
        block_number: u64,
        _metadata: &BlockMetadata,
    ) {
        // MQUKB6-T0: rayon worker threads have no ambient tracing context — any
        // span emitted inside a par_iter closure would orphan into a root trace.
        // Capture the caller's span (the drainer's `degenbot.arb.solve`) once
        // and re-enter it per work item below.
        let solve_span = tracing::Span::current();
        // D63GSE visibility: phase timing so a multi-second solve EXPLAINS
        // itself — fan-out / resolve / par-solve / clamp are separate events,
        // and the K slowest paths name where the wall-clock went.
        let cycle_start = std::time::Instant::now();
        // Collect affected path IDs from the reverse index
        let mut affected_path_ids: HashSet<u64> = HashSet::new();

        for pool_key in v2_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V2, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v3_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V3, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v4_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V4, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }

        // Also re-solve any paths registered via register_and_solve_path that
        // haven't been through rebuild_and_solve_affected yet. These paths were
        // eagerly solved at registration time, but the pump's process_block
        // replaces self.results entirely — so we must include them to avoid
        // dropping their results.
        affected_path_ids.extend(&self.pending_new_paths);
        self.pending_new_paths.clear();

        // ADR-021 change-set accumulation (pump-freeze fix): record every path
        // re-solved this solve cycle so the solver-state verifier can scope its
        // per-block on-chain diff to ONLY the paths actually solved (consumed
        // + cleared at the publish point via `take_solver_path_pool_refs_change_set`)
        // instead of the whole registered set — the verified root of the
        // confirmed O(registered × hops × RPC) pump freeze. Union/accumulate
        // (not overwrite) so a multi-solve-before-publish batch is fully
        // covered rather than only its final solve.
        self.last_solved_path_ids.extend(&affected_path_ids);

        // Solve-block anchor (rule owner + history: `crate::bot_core::solve_anchor`):
        // the batch's `solve_block` (= `results_block`) is the block the pool
        // state actually reflects — the pool-state head, NOT the
        // (possibly-lagging) drain `block_number`. Since BO5FBS the pump
        // pre-promotes `active_block` before calling `on_drain`, so
        // `block_number` here is already >= the head and the re-anchor is a
        // defensive no-op on the pump path — it stays the guard for callers
        // that bypass the pump (e.g. tests driving `solve_dirty` directly).
        let anchor =
            crate::bot_core::solve_anchor::SolveAnchor::resolve(block_number, &self.core.read());
        let solve_block = anchor.block();
        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            self.results_block = solve_block;
            return;
        }

        // Telemetry: name EVERY path the dirty-pool fan-out just activated,
        // with its concrete hop list — a Jaeger trace now answers "which pools
        // are in this path" without cross-referencing Python state. Runs under
        // the drainer's `degenbot.arb.solve` span, so the events parent there.
        for &path_id in &affected_path_ids {
            tracing::info!(
                target: "degenbot::engine",
                block_number = solve_block,
                path.id = path_id,
                path.hops = %self.describe_path(path_id),
                dirty.v2 = v2_affected.len(),
                dirty.v3 = v3_affected.len(),
                dirty.v4 = v4_affected.len(),
                "[path] activated by dirty pool"
            );
        }

        // Telemetry: fan-out summary (activations above can be hundreds of
        // events; this one line carries the aggregate).
        let fanout_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            paths.affected = affected_path_ids.len(),
            dirty.v2 = v2_affected.len(),
            dirty.v3 = v3_affected.len(),
            dirty.v4 = v4_affected.len(),
            phase_us = fanout_us,
            "[solve-phase] fanned out to affected paths"
        );

        // Re-resolve and solve only affected paths — update results in-place
        // without cloning unchanged entries.

        // Re-derive resolved hop states under the core lock — a single
        // consistent snapshot of BotState for the whole re-derive (ADR-003
        // Option A: one core-lock window per `solve_dirty`). V3/V4 state still
        // reads from the per-family block engines here; Slices 2/3 migrate
        // those into BotState too. The guard drops before `solve_path` runs,
        // which is pure `&self`.
        //
        // AV42C7 lesson: a per-path `update_block`-MIX freshness gate was
        // attempted here and REVERTED — it deferred every legitimate
        // single-pool-update arb (Sync pool A, solve with a stable reference
        // pool B at an older `update_block`). A zero-tolerance `update_block`
        // check cannot distinguish "this pool had no block-N event" (normal)
        // from "this pool is genuinely far behind" (missed swap events), so
        // its false-positive rate is catastrophic.
        //
        // YXHHKR (resolved QNFYR5): NO solve-time staleness gate here. The former
        // TQ43TU bounded-window gate deferred a whole path on any co-hop trailing
        // >10 blocks, but `update_block` is a last-activity clock, so a quiet-but-
        // current pool was falsely deferred (QNFYR5 proved 3,550 of them live).
        // `deferred_paths` is now reserved for the genuinely illegitimate future-
        // price case below; genuine chain/solver divergence is left to the ADR-021
        // verifier, which fatal-aborts loudly (the preferred failure, esp. in dev).
        let mut deferred_paths: HashSet<u64> = HashSet::new();
        // Invalidation reason histogram — names WHY the invalid slice of the
        // affected set dies before solving (SequenceUnavailable vs MissingState
        // vs ...). Emitted on the resolve phase event.
        let mut invalid_reasons: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        {
            let core = self.core.read();
            for &path_id in &affected_path_ids {
                let Some(path) = self.path_pools.get(&path_id) else {
                    continue;
                };
                // U6RNHH T1 solve-stage future-price tripwire: a hop whose PRICE
                // clock runs ahead of the solve anchor is never legitimate and is
                // rejected loudly (deferred + logged), not solved — a future-price
                // solve reports a misleading downstream IIA. Rule owner:
                // `crate::bot_core::solve_anchor`; after the head floor a hop can
                // beat the anchor only on a mid-solve state advance (belt +
                // suspenders, normally unreachable).
                let future = path
                    .pools
                    .iter()
                    .any(|pool_ref| anchor.is_future(core.pool_update_block(pool_ref.pool_key)));
                if future {
                    deferred_paths.insert(path_id);
                    tracing::error!(
                        "[future-price] path_id={path_id} rejected at solve block {solve_block}: \
                         a hop price clock runs AHEAD of the solve block (update_block > \
                         solve_block) — never legitimate"
                    );
                    continue;
                }
                let mut resolved = ResolvedMixedPath::default();
                if let Some(reason) = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    &mut self.hop_projection_cache,
                    Some(&mut self.hop_projection_count),
                ) {
                    *invalid_reasons.entry(reason.to_string()).or_insert(0u64) += 1;
                    tracing::debug!(%path_id, %reason, "[resolve] path invalid at resolve");
                }
                self.path_resolved.insert(path_id, resolved);
            }
        }

        // Telemetry: resolve phase complete (core-lock window + hop re-derive).
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            paths.resolved = affected_path_ids.len(),
            hop.projections = self.hop_projection_count,
            paths.deferred_future_price = deferred_paths.len(),
            invalid.reasons = %invalid_reasons.iter().map(|(r, c)| format!("{c}x {r}")).collect::<Vec<_>>().join(", "),
            phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            "[solve-phase] resolved hop snapshots"
        );

        // Remove old results for affected paths (they'll be re-solved below).
        // A deferred path's result is dropped too: it is excluded from this
        // live solve (its pool is stale, so its prior result is stale as well).
        for &path_id in &affected_path_ids {
            self.results.remove(&path_id);
        }

        // Solve only the non-deferred affected set.
        let solve_path_ids: HashSet<u64> = affected_path_ids
            .iter()
            .filter(|p| !deferred_paths.contains(p))
            .copied()
            .collect();

        // Solve affected paths and insert new results.
        //
        // ADR-005 slice 15b-1: rayon `par_iter` parallelizes the solve across
        // the affected-path set. `Self::solve_path` is a free-standing dispatch
        // (no `&self` read); each work item takes the `path_id` + a CLONED
        // `ResolvedMixedPath` (the `Clone` derive is cheap; the V3-V4 path
        // math reads only immutable statics), then writes — under the parallel
        // closure — into the engine-level result-set via a `Mutex`-free
        // pattern: collect `(path_id, SolvePathResult)` pairs into a Vec, then
        // merge sequentially into `self.results`. The parallel workers touch
        // NO engine state and NO core.lock — engine-then-core lock ordering is
        // preserved unchanged (rayon's internal thread pool never re-enters the
        // engine `Mutex`). For tiny batches the par_iter dispatch overhead is
        // bounded by rayon's lazy split (see `par_iter` docs); the sequential
        // cost dominates below the rayon internal cutoff.
        //
        // Pre-collect the work items (path_id + resolved-snapshot). The clone
        // drops the immutable borrow on `self.path_resolved` that would block
        // parallel dispatch.
        let mut invalid_count: u64 = 0;
        let to_solve: Vec<(u64, ResolvedMixedPath)> = solve_path_ids
            .iter()
            .filter_map(|&pid| {
                let resolved = self.path_resolved.get(&pid)?;
                if !resolved.valid {
                    invalid_count += 1;
                    return None;
                }
                // A path whose `max_update_block` is AHEAD of the drain
                // `block_number` is LIVE head state (the pools advanced by
                // backfill), not poison — it is correctly re-anchored at
                // `solve_block` above (B2); skipping it would DROP a capturable
                // opportunity. The genuinely-future case (`update_block >
                // solve_block`) is already rejected by the U6RNHH T1 belt-and-
                // suspenders guard in the gate loop above, which removes the
                // path from `solve_path_ids` entirely.
                Some((pid, resolved.clone()))
            })
            .collect();

        // Filter out empty/profitless results in the same pass that produces
        // them — the contract is identical to the prior serial loop.
        // D63GSE: per-path wall time is captured so the K slowest paths can be
        // named on the completion event (a min-heap keeps this O(K) memory;
        // the closure itself only does one Instant pair + map insert).
        // (time_us, pieces_visited, path_sims, pid) for the K-slowest
        // attribution — lets the completion event name the walk-combinatorial
        // cost driver of the slowest routes, not just their wall time.
        let path_times: std::sync::Mutex<
            std::collections::BinaryHeap<std::cmp::Reverse<(u128, u64, u64, u64)>>,
        > = std::sync::Mutex::new(std::collections::BinaryHeap::new());
        // Total CPU µs across all solved paths — dividing by the rayon wall
        // time yields achieved parallelism (8 workers ⇒ target ≈ 8.0).
        let solve_cpu_us: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Walk-combinatorial totals across the solve cycle (Σ pieces visited,
        // Σ path simulations) — the diagnostic multiplier behind a slow solve.
        let walk_pieces_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_sims_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Optional offline CL-solver capture (DEGENBOT_SOLVER_CAPTURE=1): dump
        // the exact all-CL pool state the solver consumed for heavy paths so
        // the CL solver can be optimized offline. None (no-op) unless gated.
        let capture = HeavyClPathCapture::from_env();
        let capture_ref: Option<&HeavyClPathCapture> = capture.as_ref();
        let solved: Vec<(u64, SolvePathResult)> = to_solve
            .par_iter()
            .filter_map(|(pid, resolved)| {
                let _solve_ctx = solve_span.enter();
                // Scope the walk counters to THIS path (same rayon worker
                // thread runs solve_path synchronously; the walk spawns no
                // sub-tasks, so the per-thread Cell is consistent).
                ::degenbot_solvers::mobius_v3_int::reset_walk_stats();
                let t0 = std::time::Instant::now();
                let result = ::degenbot_solvers::mixed::solve_path(resolved);
                let micros = t0.elapsed().as_micros();
                solve_cpu_us.fetch_add(
                    u64::try_from(micros).unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                let (pieces, sims) = ::degenbot_solvers::mobius_v3_int::take_last_walk_stats();
                walk_pieces_total.fetch_add(
                    u64::try_from(pieces).unwrap_or(0),
                    std::sync::atomic::Ordering::Relaxed,
                );
                walk_sims_total.fetch_add(
                    u64::try_from(sims).unwrap_or(0),
                    std::sync::atomic::Ordering::Relaxed,
                );
                if let Ok(mut heap) = path_times.lock() {
                    let worst = heap
                        .peek()
                        .map_or(u128::MAX, |std::cmp::Reverse((w, _, _, _))| *w);
                    if heap.len() < SLOWEST_PATHS_K || micros > worst {
                        heap.push(std::cmp::Reverse((
                            micros,
                            u64::try_from(pieces).unwrap_or(0),
                            u64::try_from(sims).unwrap_or(0),
                            *pid,
                        )));
                        if heap.len() > SLOWEST_PATHS_K {
                            heap.pop();
                        }
                    }
                }
                // Offline CL-solver capture gate (no-op unless the env gate is set).
                if let Some(cap) = capture_ref {
                    cap.maybe_capture(
                        *pid,
                        u64::try_from(solve_block).unwrap_or(0),
                        u64::try_from(micros).unwrap_or(u64::MAX),
                        u64::try_from(sims).unwrap_or(0),
                        u64::try_from(pieces).unwrap_or(0),
                        result.as_ref(),
                        resolved,
                    );
                }
                result.map(|r| (*pid, r))
            })
            // Log solver pool state for every solved path (including
            // unprofitable) — diagnostic cross-referencing against sim
            // captured swaps.
            .inspect(|(pid, r)| {
                if !r.solver_pool_states.is_empty() {
                    tracing::debug!(
                        "[solver-st] path_id={pid} hops=[{}]",
                        r.solver_pool_states.join(";")
                    );
                }
            })
            .filter(|(_, r)| !r.optimal_input.is_zero() && !r.profit.is_zero())
            .collect();
        if let Some(c) = capture.as_ref() {
            tracing::info!(
                target: "degenbot::solver",
                captured = c.count.load(std::sync::atomic::Ordering::Relaxed),
                out = %c.out_path.display(),
                "[solve-capture] heavy all-CL path capture active"
            );
        }

        // Telemetry: pure solver phase done — name the K slowest paths.
        let slowest: Vec<String> = path_times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|std::cmp::Reverse((us, pieces, sims, pid))| {
                format!("{pid}:{us}us:sims={sims}:pieces={pieces}")
            })
            .collect();
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            paths.solved = to_solve.len(),
            paths.invalid = invalid_count,
            solve.cpu_us = solve_cpu_us.load(std::sync::atomic::Ordering::Relaxed),
            walk.pieces = walk_pieces_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.sims = walk_sims_total.load(std::sync::atomic::Ordering::Relaxed),
            profitable = solved.len(),
            slowest.paths = %slowest.join(","),
            phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            "[solve-phase] rayon solve complete"
        );

        // Sequential merge — no lock acquisition; workers above owned their
        // clones. Apply the pool-state-aware CL-hop capacity clamp per path
        // (reads `core` to reconcile each CL hop's committed input against the
        // pools twin) BEFORE inserting, so the stored result carries truthful
        // `consumed_inputs` (a CL hop fed past its max-convertible capacity
        // would march empty bitmap words on-chain — UO3JM4).
        let clamp_twins_start = std::time::Instant::now();
        let mut clamp_twin_count: u64 = 0;
        let solved_count = solved.len();
        for (pid, mut solve_result) in solved {
            clamp_twin_count += self.clamp_cl_hop_capacity(pid, &mut solve_result);
            // Telemetry: profitable solves are the signal in the noise — emit
            // the economics + the concrete hop list on the solve span.
            tracing::info!(
                target: "degenbot::solver",
                block_number = solve_block,
                path.id = pid,
                input = %solve_result.optimal_input,
                profit = %solve_result.profit,
                path.hops = %self.describe_path(pid),
                "[path] profitable solve"
            );
            self.results.insert(pid, solve_result);
        }

        // Telemetry: clamp phase done — the twin simulations are a known
        // multi-second contributor on CL-heavy batches, so they get their own
        // line item.
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            clamp.paths = solved_count,
            clamp.twins = clamp_twin_count,
            clamp.phase_us = u64::try_from(clamp_twins_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            total_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            "[solve-phase] cycle complete (clamp done)"
        );

        self.results_block = solve_block;
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
    }

    /// Solve all registered paths using `solve_path`.
    ///
    /// `solve_all` is not currently used live — the pump calls
    /// `solve_all_paths` which calls this only at cold start; subsequent
    /// re-solves go through `rebuild_and_solve_affected`.
    ///
    /// ADR-005 slice 15b-1: the solve loop runs under rayon `par_iter` over
    /// the registered `path_resolved` map. `Self::solve_path` is receiver-free
    /// (slice 15b-1: pure dispatch to the freestanding math helpers), so the
    /// parallel closure borrows only the `path_resolved` entry — no `&self`
    /// mutation under the workers; they collect pairs that the outer loop
    /// inserts into the fresh result map sequentially. The engine-then-core
    /// lock ordering is unchanged: this method is `&self` (no core.lock taken
    /// here; the caller already resolved the paths under `core.read()` at the
    /// `solve_all_paths` entry).
    #[must_use]
    pub fn solve_all(&self) -> HashMap<u64, SolvePathResult> {
        // MQUKB6-T0: same rayon context re-entry as rebuild_and_solve_affected.
        let solve_span = tracing::Span::current();
        self.path_resolved
            .par_iter()
            .filter_map(|(&path_id, resolved)| {
                let _solve_ctx = solve_span.enter();
                if !resolved.valid {
                    return None;
                }
                ::degenbot_solvers::mixed::solve_path(resolved)
                    // Log solver pool state for diagnostic cross-referencing
                    // (path_id -> pool state at solve time).
                    .inspect(|r| {
                        if !r.solver_pool_states.is_empty() {
                            tracing::debug!(
                                "[solver-st] path_id={path_id} hops=[{}]",
                                r.solver_pool_states.join(";")
                            );
                        }
                    })
                    .filter(|r| !r.optimal_input.is_zero() && !r.profit.is_zero())
                    .map(|r| (path_id, r))
            })
            .map(|(path_id, mut r)| {
                // Pool-state-aware CL-hop capacity clamp (UO3JM4) — reconcile
                // each CL hop's committed input against the pools twin before
                // the result escapes to the caller (cold-start / test path;
                // `rebuild_and_solve_affected` applies the same clamp).
                self.clamp_cl_hop_capacity(path_id, &mut r);
                (path_id, r)
            })
            .collect()
    }
}

impl Default for ArbitrageEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod profit_clamp_recompute_tests {
    #![expect(clippy::expect_used)] // tests assert recompute invariants
    use super::{ArbitrageEngine, SolvePathResult, U256};

    /// Path-142603 (V4-V4-V3 @25723658) regression: the solver reported a
    /// phantom +346,369,630 wei profit because its V3 hop2 output
    /// (351,476,391,576,684) over-predicted the byte-exact twin
    /// (351,475,872,056,229) by 519,520,455 wei. After the CL clamp aligns
    /// `hop_outputs`/`consumed_inputs` to the twin, the selection profit MUST
    /// be recomputed from the clamped values: the round trip nets
    /// -173,150,825 wei -> saturates to 0 -> dropped by the `profit > min_profit`
    /// delivery gate instead of being selected and executing to a `no-profit`
    /// trap. (Regression for the BUG-B fix in `clamp_cl_hop_capacity`.)
    #[test]
    fn post_clamp_last_hop_loss_saturates_profit_to_zero() {
        let mut r = SolvePathResult {
            optimal_input: U256::from(351_476_045_207_054u64),
            // Profit the SOLVER computed on its over-predicted raw hop2 output
            // (= 351_476_391_576_684 - 351_476_045_207_054 = +346,369,630).
            profit: U256::from(346_369_630u64),
            // Twin-aligned outputs after the CL clamp: hop2 (last) clamped
            // DOWN to the byte-exact twin 351,475,872,056,229.
            hop_outputs: vec![
                U256::from(676_293u64),
                U256::from(676_607u64),
                U256::from(351_475_872_056_229u64),
            ],
            consumed_inputs: vec![U256::from(351_476_045_207_054u64)],
            ..Default::default()
        };
        let recomputed = ArbitrageEngine::recompute_clamped_profit(&r).expect("has outputs");
        // final_output - consumed_inputs[0] = -173,150,825 -> saturating 0.
        assert_eq!(recomputed, U256::ZERO, "post-clamp loss must saturate to 0");
        // The clamp writes the recomputed value back (the fix).
        r.profit = recomputed;
        assert!(
            r.profit.is_zero(),
            "selection profit must be zero (dropped)"
        );
    }

    /// The recompute is a no-op safety for a genuinely-profitable path whose
    /// outputs were twin-aligned with no net change: profit is preserved.
    #[test]
    fn genuine_profit_preserved_after_clamp() {
        let r = SolvePathResult {
            optimal_input: U256::from(1000u64),
            profit: U256::from(50u64),
            hop_outputs: vec![U256::from(200u64), U256::from(1050u64)],
            consumed_inputs: vec![U256::from(1000u64), U256::from(200u64)],
            ..Default::default()
        };
        let recomputed = ArbitrageEngine::recompute_clamped_profit(&r).expect("has outputs");
        assert_eq!(
            recomputed,
            U256::from(50u64),
            "genuine profit must be preserved"
        );
    }

    /// `profit = final_output - consumed_inputs[0]` (the documented semantics):
    /// a first hop that partial-fills at a range boundary consumes less than the
    /// full `optimal_input`, so the recompute must key off `consumed_inputs[0]`.
    #[test]
    fn recompute_uses_consumed_inputs_zero_not_optimal_input() {
        let r = SolvePathResult {
            optimal_input: U256::from(1000u64),
            profit: U256::from(0u64),
            hop_outputs: vec![U256::from(300u64), U256::from(1050u64)],
            // hop0 consumes 900, not the full 1000 (partial fill at boundary).
            consumed_inputs: vec![U256::from(900u64), U256::from(300u64)],
            ..Default::default()
        };
        let recomputed = ArbitrageEngine::recompute_clamped_profit(&r).expect("has outputs");
        assert_eq!(
            recomputed,
            U256::from(150u64),
            "1050 - 900, not 1050 - 1000"
        );
    }

    /// A degenerate path (no hop outputs / consumed inputs) recomputes to None
    /// and is left untouched by the clamp.
    #[test]
    fn degenerate_path_returns_none() {
        let r = SolvePathResult::default();
        assert!(ArbitrageEngine::recompute_clamped_profit(&r).is_none());
    }
}

/// One-shot capture of the exact all-CL solver input for heavy paths, so the
/// CL solver (`int_solve_cl_path` / active-set walk) can be optimized against
/// real captured pool state offline, without a full bot run.
///
/// Gated by `DEGENBOT_SOLVER_CAPTURE=1` (`from_env` yields `None` otherwise).
/// For each heavy all-CL path (the first `DEGENBOT_SOLVER_CAPTURE_CAP`, deduped
/// by path id, heavy = `time_us >= MIN_US` or `sims >= MIN_SIMS`) it appends one
/// JSON line to `DEGENBOT_SOLVER_CAPTURE_OUT` with the per-hop
/// `IntV3TickRangeSequence` ranges, the measured (time, walk sims, pieces), and
/// the golden result - so the offline replay harness asserts determinism.
struct HeavyClPathCapture {
    min_us: u64,
    min_sims: u64,
    max_captures: u64,
    out_path: std::path::PathBuf,
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
    count: std::sync::atomic::AtomicU64,
}

impl HeavyClPathCapture {
    fn from_env() -> Option<Self> {
        if std::env::var_os("DEGENBOT_SOLVER_CAPTURE").is_none() {
            return None;
        }
        Some(Self {
            min_us: std::env::var("DEGENBOT_SOLVER_CAPTURE_MIN_US")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50_000),
            min_sims: std::env::var("DEGENBOT_SOLVER_CAPTURE_MIN_SIMS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2_000),
            max_captures: std::env::var("DEGENBOT_SOLVER_CAPTURE_CAP")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16),
            out_path: std::path::PathBuf::from(
                std::env::var("DEGENBOT_SOLVER_CAPTURE_OUT")
                    .unwrap_or_else(|_| "tests/fixtures/heavy_cl_solve_captures.jsonl".to_string()),
            ),
            seen: std::sync::Mutex::new(std::collections::HashSet::new()),
            count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Append the CL pool state for a resolved heavy path, if it is a heavy,
    /// not-yet-captured all-CL path.
    fn maybe_capture(
        &self,
        pid: u64,
        block: u64,
        micros_us: u64,
        sims: u64,
        pieces: u64,
        golden: Option<&SolvePathResult>,
        resolved: &ResolvedMixedPath,
    ) {
        if self.count.load(std::sync::atomic::Ordering::Relaxed) >= self.max_captures {
            return;
        }
        if micros_us < self.min_us && sims < self.min_sims {
            return;
        }
        // Must be a pure-CL path (every hop resolves to an int sequence, at
        // least 2 hops) to replay `int_solve_cl_path` directly offline.
        if resolved.hops.len() < 2 || !resolved.hops.iter().all(|h| h.as_int_sequence().is_some()) {
            return;
        }
        let mut seen = match self.seen.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if !seen.insert(pid) {
            return;
        }
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Per path hop -> its `IntV3TickRangeSequence.ranges`; each range as the
        // 8 primitive fields (big ints as decimal strings, so no alloy serde).
        let hops = resolved
            .hops
            .iter()
            .map(|h| {
                h.as_int_sequence()
                    .unwrap()
                    .ranges
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "liquidity": r.liquidity.to_string(),
                            "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                            "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                            "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                            "gamma_numer": r.gamma_numer,
                            "fee_denom": r.fee_denom,
                            "zero_for_one": r.zero_for_one,
                            "word_boundary_prices": r.word_boundary_prices
                                .iter()
                                .map(|w| w.to_string())
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let golden_json = golden.map(|g| {
            serde_json::json!({
                "optimal_input": g.optimal_input.to_string(),
                "profit": g.profit.to_string(),
                "hop_outputs": g.hop_outputs.iter().map(|o| o.to_string()).collect::<Vec<_>>(),
            })
        });
        let doc = serde_json::json!({
            "path_id": pid,
            "block": block,
            "n_hops": resolved.hops.len(),
            "hops": hops,
            "measured": { "time_us": micros_us, "sims": sims, "pieces": pieces },
            "golden": golden_json,
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.out_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{}", doc);
        }
    }
}
