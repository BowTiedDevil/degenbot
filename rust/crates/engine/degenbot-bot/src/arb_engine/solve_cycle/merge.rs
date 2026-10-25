//! The epoch's merge phase: the per-result clamp, the Q1a intake policy, and
//! the drain that carries a lane outcome into the epoch's result set.
//!
//! Machine-direct free functions over the epoch's [`EpochContext`]: the merge
//! runs on the detached-merge sidecar (and inline for the test harness), so
//! the bodies take the epoch workspace the spine builds rather than the whole
//! cycle. The clamp is the ONE body shared with the SIMPIPE2 T2 worker
//! (`lane_walk::clamp_result_in_worker`), so it takes the core handle and the
//! margin explicitly — the `&self` accessor on [`super::SolveCycle`] is the
//! only caller with no context to hand.

use super::deliver;
use super::{describe_path_cached_with, EpochContext};
use crate::arb_engine::delivery_policy::DeliveryPolicy;
use crate::arb_engine::detached_cycle::{self, LaneDrainCounts};
use crate::arb_engine::executor::{LaneOutcome, SolveOutcome};
use crate::arb_engine::inline_sim::SimulatedPathResult;
use crate::arb_engine::lane_walk::LaneArmPolicy;
use crate::arb_engine::path_registry::PathRegistry;
use ::degenbot_pools::solve::mixed::{HopType, MixedPoolRef, SolvePathResult};
use alloy::primitives::{I256, U256};
use degenbot_core::{diag, op_error, op_info};
use degenbot_pools::v3_state::{v3_simulate_swap, V3PoolState};
use degenbot_pools::v4_state::v4_simulate_swap;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::{BlockMetadata, BotState};
use std::sync::Arc;

// ===========================================================================
// the concentrated-liquidity-hop clamp and its profit recompute, moved off the
// deleted `ArbitrageEngine` twins onto their real owner. `SolveCycle::
// clamp_concentrated_liquidity_hop_capacity` and the worker-side `lane_walk::clamp_result_in_
// worker` both drive the same `clamp_result_with_state`; there is exactly
// one clamp body (ADR-046 "no inherent twins").
// ===========================================================================
/// The clamp-skip error kind for a hop with no byte-exact twin at the clamp
/// seam. `None` for the V2/V3/V4 families that DO execute the twin arm.
pub(crate) fn clamp_skip_kind(hop: HopType) -> Option<&'static str> {
    match hop {
        HopType::SolidlyStable => Some(crate::telemetry::error_kind::CLAMP_SKIP_SOLIDLY_STABLE),
        HopType::BalancerWeighted => {
            Some(crate::telemetry::error_kind::CLAMP_SKIP_BALANCER_WEIGHTED)
        }
        HopType::BalancerStable => Some(crate::telemetry::error_kind::CLAMP_SKIP_BALANCER_STABLE),
        HopType::CurveStableswap => Some(crate::telemetry::error_kind::CLAMP_SKIP_CURVE_STABLESWAP),
        HopType::V2 | HopType::V3 | HopType::V4 => None,
    }
}

/// The clamp shared by the merge-site gate and the SIMPIPE2 T2 worker
/// relocation — the pool list is a parameter so the WORKER can drive the
/// identical logic from its `to_solve`-aligned snapshot, and the clamp margin
/// arrives as a parameter packed from the typed config (never a process-static
/// read). The worker takes the SAME short core read the merge-site clamp took
/// (engine-then-core, short read, no guard across awaits).
///
/// ## Margin basis
///
/// `margin` is an absolute wei amount subtracted from `input_consumed` before
/// it is committed. The typed config default is 1 wei: the exact-in loop then
/// converts nearly everything and stops on `amountRemaining==0` at the last
/// funded tick, the maximum-extraction choice. The margin must stay strictly
/// larger than the worst solver-vs-engine over-prediction, so the clamp never
/// lands exactly on an over-predicted tight value and re-enters the empty-word
/// march. The `v4_crossing_solver_vs_sim_parity` /
/// `v4_word_boundary_solver_divergence` / `v4_fee1_solver_path_matches_v4_simulate_swap`
/// suites assert byte-exact solver==twin across the fee-3000/ts-60 multi-tick
/// corpus AND the fee-1/ts-1 low-fee topology in both swap directions — the
/// worst observed over-prediction is **0 wei**. The historical live `+1..+3`
/// wei residuals (fee-1, ts=1) were localized to crossing-math rounding and
/// fixed (the zfo step-0 current-tick flooring), not absorbed by margin. A
/// dedicated sweep (`concentrated_liquidity_hop_clamp_margin_exceeds_worst_solver_over_prediction`)
/// measures the strict over-prediction direction across the corpus and asserts
/// `margin > worst`, guarding the choice against regression. 1 wei is the
/// smallest positive integer > 0, giving zero extraction loss (path-5000
/// fixture: clamped output == solver output byte-identical).
#[expect(clippy::too_many_lines)] // multi-hop concentrated liquidity twin loop + post-clamp profit recompute
pub(crate) fn clamp_result_with_state(
    core: &BotState,
    path_id: u64,
    pools: &[MixedPoolRef],
    margin: U256,
    result: &mut SolvePathResult,
) -> u64 {
    if pools.len() != result.consumed_inputs.len() {
        return 0; // Index misalignment — never clamp a wrong hop
    }
    // successful twin simulations executed this call (returned to
    // the caller for the solve-cycle completion event).
    let mut twins_executed: u64 = 0;
    for (i, pool_ref) in pools.iter().enumerate() {
        let requested = result.consumed_inputs[i];
        // Run the tier-3-validated twin once per clamped family so we can
        // (a) clamp this hop's INPUT (concentrated liquidity marching empty-word EMPTY-HALT
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
                    core.pool_slice::<degenbot_pools::v3_state::V3PoolState>(pool_ref.pool_key),
                    core.pool_slice::<degenbot_pools::v3_state::V3PoolIdentity>(pool_ref.pool_key),
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
                    core.pool_slice::<degenbot_pools::v4_state::V4PoolState>(pool_ref.pool_key),
                    core.pool_slice::<degenbot_pools::v4_state::V4PoolIdentity>(pool_ref.pool_key),
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
                    core.pool_slice::<degenbot_pools::v2_state::V2PoolState>(pool_ref.pool_key),
                    core.pool_slice::<degenbot_pools::v2_state::V2PoolIdentity>(pool_ref.pool_key),
                ) else {
                    continue; // Pool state unavailable → can't clamp
                };
                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one {
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
            // seam; their reported outputs stand (see module note). Counted
            // per family so a first live occurrence is visible without
            // per-hop logging.
            HopType::SolidlyStable
            | HopType::BalancerWeighted
            | HopType::BalancerStable
            | HopType::CurveStableswap => {
                if let Some(kind) = clamp_skip_kind(pool_ref.hop_type) {
                    crate::instruments::record(crate::instruments::CensusEvent::EngineError {
                        kind,
                    });
                }
                continue;
            }
        };
        // (a) Input clamp: cap this concentrated liquidity hop's committed input at
        // `input_consumed - margin` when over-fed (the empty-march class).
        if let Some(clamped) = input_clamp {
            if clamped < requested {
                crate::instruments::record(crate::instruments::CensusEvent::SolverClampApplied);
                op_info!(
                    domain = solver,
                    "path_id={path_id} hop={i} family={:?} input requested={requested} \
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
                crate::instruments::record(crate::instruments::CensusEvent::SolverClampApplied);
                op_info!(
                    domain = solver,
                    "path_id={path_id} hop={i} family={:?} hop_outputs={hop_out} \
                     twin_out={out} delta={}",
                    pool_ref.hop_type,
                    if *hop_out > out {
                        *hop_out - out
                    } else {
                        out - *hop_out
                    }
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
                crate::instruments::record(crate::instruments::CensusEvent::SolverClampApplied);
                op_info!(
                    domain = solver,
                    "path_id={path_id} hop={i} family={:?} forward={forward} \
                     twin_out={out} reduction={}",
                    pool_ref.hop_type,
                    forward - out
                );
                result.consumed_inputs[i + 1] = out;
            }
        }
    }
    // BUG-B FIX (path-142603 `no-profit` crash): the solver's `profit` is
    // computed on its RAW (over-predicted) hop outputs; the concentrated liquidity clamp above
    // realigns execution to the twin but was not feeding back a recomputed
    // profit, so an actually-unprofitable path stayed `> min_profit` and was
    // dispatched → executed to a loss → `no-profit` abort. Recompute the
    // selection profit from the clamped values (see `recompute_clamped_profit`);
    // a post-clamp loss saturates to 0 and is dropped.
    if let Some(recomputed) = recompute_clamped_profit(result) {
        let profit_before = result.profit;
        if recomputed != profit_before {
            op_info!(domain = solver, path_id,
                profit_before = %profit_before,
                profit_after = %recomputed,
                profit_delta = %profit_before.saturating_sub(recomputed),
                "recomputed selection profit from twin-aligned outputs"
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
/// lock) so it is directly unit-testable independent of the concentrated-liquidity-twin
/// machinery.
#[must_use]
pub(crate) fn recompute_clamped_profit(result: &SolvePathResult) -> Option<U256> {
    let final_output = result.hop_outputs.last().copied()?;
    let first_consumed = result.consumed_inputs.first().copied()?;
    Some(final_output.saturating_sub(first_consumed))
}

/// The merge of ONE solved result onto the epoch's result set: the worker's
/// clamp verdict (or the merge-site clamp when the worker reported none), the
/// per-result telemetry, the delivery handoff, and the result-store write.
#[expect(clippy::too_many_arguments)] // the epoch's whole per-result input set
pub(super) fn merge_one_result(
    ctx: &EpochContext<'_>,
    solve_block: u64,
    metadata: &BlockMetadata,
    pid: u64,
    result: SolvePathResult,
    worker_clamp_twins: u64,
    payload: Option<SimulatedPathResult>,
    registry: &PathRegistry,
    delivery: &mut DeliveryPolicy,
) -> u64 {
    let mut result = result;
    let twins = if worker_clamp_twins > 0 {
        worker_clamp_twins
    } else {
        clamp_concentrated_liquidity_hop_capacity(
            ctx.core,
            ctx.clamp_margin,
            pid,
            &mut result,
            registry,
        )
    };
    // Telemetry: profitable solves are the signal in the noise -
    // emit the economics + the concrete hop list on the solve span.
    // M2 probe: per-result events cross the Python log bridge (GIL) -
    // size that cost vs the map/delivery work (hotpath-attributed).
    hotpath::measure_block!("merge.telemetry_event", {
        diag!(domain = path, block_number = solve_block,
            path.id = pid,
            input = %result.optimal_input,
            profit = %result.profit,
            path.hops = %describe_path_cached_with(ctx.path_description_cache, ctx.core, ctx.registry, pid),
            "profitable solve"
        );
    });
    deliver::deliver_merged_result(ctx, delivery, solve_block, metadata, pid, &result, payload);
    ctx.results.insert(pid, result);
    #[cfg(test)]
    if let Some(probe) = ctx.merge_probe {
        probe.lock().push(pid);
    }
    twins
}

/// The detached-merge envelope for ONE lane outcome: the Q1a stale/deregister
/// gates, the per-item claim, and the drain handoff. Every envelope leaves
/// through the drain (the claim's one door).
pub(super) fn merge_detached_item(
    ctx: &mut EpochContext<'_>,
    item: LaneOutcome,
    registry: &PathRegistry,
    delivery: &mut DeliveryPolicy,
) {
    // test-only per-path panic hook — kill this merge
    // mid-item so the sidecar's catch_unwind guard + caught-panic
    // disposition (sticky cordon) can be pinned.
    #[cfg(test)]
    if let Some(panic_pid) = ctx.test_merge_panic.as_ref() {
        panic_pid(item.pid());
    }
    let LaneOutcome::Solved(solved) = &item else {
        // Keyless Suppressed/Failed witnesses have no envelope work
        // (the gauge never bumped them — REV 2 Defect 1): straight to
        // the drain's no-claim arms. Seq/block fields are inert here
        // (nothing is claimed — contract 4's no-claim witness).
        let policy = LaneArmPolicy {
            ledger_seq: 0,
            solve_block: 0,
            metadata: BlockMetadata::default(),
        };
        let mut counts = LaneDrainCounts::default();
        drain_lane_outcomes(
            ctx,
            std::iter::once(item),
            &policy,
            &mut counts,
            registry,
            delivery,
        );
        let _ = counts;
        return;
    };
    // The in-flight gauge was bumped at SEND time in the enqueue
    // half and is decremented here exactly once per Solved item,
    // so a bin that dies before sending never leaks a count.
    // the machine owns the pair — this is the RECEIPT half
    // (the decrement + both meter publishes ride with it,
    // byte-identical).
    ctx.detached_cycle.solved_received();
    // re-enter the enqueue-time cycle span for the
    // whole merge (Q1a drop/apply events + any profit emit
    // parent there). Inert without a subscriber or for
    // `Span::none()` test items.
    let merge_span = solved.solve_span.clone();
    let _merge_ctx = merge_span.enter();
    let age_cycles = ctx
        .detached_cycle
        .issued_seq()
        .saturating_sub(solved.cycle_seq);
    // Q1a deregister: nothing to merge into — drop, never
    // re-create.
    let Some(registered) = registry.get(solved.pid) else {
        ctx.detached_cycle
            .disposition(detached_cycle::Disposition::DroppedDeregistered);
        diag!(
            domain = solver,
            path_id = solved.pid,
            detached_seq = solved.cycle_seq,
            "straggler dropped (path deregistered)"
        );
        return;
    };
    // Q1a stale: re-read the LIVE per-hop clocks; any advance
    // since the enqueue resolve invalidates the straggler's
    // intake.
    let live_stamp: Vec<u64> = {
        let core = ctx
            .core
            .read_at(degenbot_substrate::state_lock::LockSite::Solver);
        registered
            .pools
            .iter()
            .map(|pool_ref| core.pool_update_block(pool_ref.pool_key))
            .collect()
    };
    if live_stamp != solved.update_stamp {
        ctx.detached_cycle
            .disposition(detached_cycle::Disposition::DroppedStale);
        op_info!(
            domain = solver,
            path_id = solved.pid,
            detached_seq = solved.cycle_seq,
            detached_age_cycles = age_cycles,
            "straggler dropped (stale: pools moved during the solve)"
        );
        return;
    }
    // Exactness fuse carried to the DETACHED sidecar
    // Note-(a) carry: the in-cycle drain claims the same engine-side ledger keyed
    // (solve_seq, pid) — this sidecar claims (cycle_seq, pid)
    // with the cycle_seq its enqueue stamped. A duplicate
    // delivery is a bin/pipe bug (an outcome emitted twice):
    // merge-twice would double-apply the Q1a policy and
    // double-emit, so the fuse trips loudly and the item is
    // refused. The ledger keeps recent cycles only (the
    // in-flight cap bounds meaningful straggler age); older
    // cycle keys are pruned. The claim follows the Q1a gates
    // and precedes the applied increment — the ledger records
    // exactly what reached the merge (an item Q1a dropped was
    // never a merge attempt; its flags stay fireable).
    // THE claim itself is the drain's Solved arm below
    // (one claim per outcome — the table's role). This envelope
    // owns the CLAIM'S ORDER: the Q1a gates above run BEFORE the
    // drain, so a Q1a-dropped straggler still never reaches the
    // claim and its flags stay fireable.
    // THE DRAIN: claim + merge for the Q1a-fresh
    // straggler under the sidecar's per-item Mutex hold
    // (contract 3's SidecarPerItemHold; the drain never locks).
    // Borrow-scope note: the envelope's post-drain log fields are
    // read BEFORE the item moves into the drain (the drain's own
    // merge logs carry the same fields forward).
    let (log_pid, log_seq) = (solved.pid, solved.cycle_seq);
    let policy = LaneArmPolicy {
        ledger_seq: solved.cycle_seq,
        solve_block: solved.solve_block,
        metadata: solved.metadata,
    };
    let mut counts = LaneDrainCounts::default();
    drain_lane_outcomes(
        ctx,
        std::iter::once(item),
        &policy,
        &mut counts,
        registry,
        delivery,
    );
    // The applied flag rides the DRAIN's admitted merge: the claim
    // precedes the applied increment and the ledger records exactly
    // what reached the merge — a refused duplicate increments only
    // `duplicate_outcomes` (the fuse), never this flag.
    if counts.solved > 0 {
        ctx.detached_cycle
            .disposition(detached_cycle::Disposition::Applied);
        diag!(
            domain = solver,
            path_id = log_pid,
            detached_seq = log_seq,
            detached_age_cycles = age_cycles,
            "straggler merged (unchanged intake)"
        );
    }
    // ADR-021 publish-verifier scoping retired : the
    // solver-state verifier (and its publish change set) is gone
    // merges apply the Q1a stale policy only.
}

/// The drain: one claim and one merge per solved lane outcome, with the
/// keyless Suppressed/Failed witnesses never claiming.
pub(super) fn drain_lane_outcomes(
    ctx: &mut EpochContext<'_>,
    items: impl IntoIterator<Item = LaneOutcome>,
    policy: &LaneArmPolicy,
    counts: &mut LaneDrainCounts,
    registry: &PathRegistry,
    delivery: &mut DeliveryPolicy,
) {
    for item in items {
        match item {
            LaneOutcome::Solved(o) => {
                let pid = o.pid;
                // the claim runs through the machine's one
                // ledger door (the KEY policy — the seq half — is
                // machine-issued); a refused duplicate lands on the
                // machine's fuse counter.
                if ctx.detached_cycle.claim((policy.ledger_seq, pid)).is_err() {
                    ctx.detached_cycle
                        .disposition(detached_cycle::Disposition::Duplicate);
                    op_error!(
                        domain = solver,
                        path_id = pid,
                        ledger_seq = policy.ledger_seq,
                        "duplicate lane outcome for path — exactness fuse tripped"
                    );
                    continue;
                }
                debug_assert_eq!(
                    o.solve_block, policy.solve_block,
                    "carrier/drain solve_block mismatch — wrong-arm delivery"
                );
                let SolveOutcome {
                    result: solve_result,
                    worker_clamp_twins,
                    payload,
                    ..
                } = o;
                if !solve_result.solver_pool_states.is_empty() {
                    diag!(
                        domain = solver,
                        "path_id={pid} hops=[{}]",
                        solve_result.solver_pool_states.join(";")
                    );
                }
                merge_one_result(
                    ctx,
                    policy.solve_block,
                    &policy.metadata,
                    pid,
                    solve_result,
                    worker_clamp_twins,
                    payload,
                    registry,
                    delivery,
                );
                counts.solved += 1;
            }
            LaneOutcome::Suppressed { pid } => {
                // CONTRACT 4: the pid-only witness NEVER claims.
                diag!(
                    domain = solver,
                    path_id = pid,
                    "suppressed outcome delivered by the lane witness — no merge, no claim"
                );
                counts.suppressed += 1;
            }
            LaneOutcome::Failed { pid, failure } => {
                // CONTRACT 4: the pid-only witness NEVER claims.
                // The envelope's per-item accounting (the ancestral
                // sidecar's Failed arm — the deregistered bucket: the
                // panic record is a genuine final drop, NOT the stale
                // bucket, which the Q1a stale gate alone owns).
                ctx.detached_cycle
                    .disposition(detached_cycle::Disposition::DroppedDeregistered);
                op_error!(domain = solver, path_id = pid,
                    failure = ?failure,
                    "path outcome lost to a seat panic — typed failure record"
                );
                counts.failed += 1;
            }
        }
    }
}

/// The merge-site clamp door: read the path's pool refs and the live state
/// under one short core read, then drive the ONE clamp body.
pub(super) fn clamp_concentrated_liquidity_hop_capacity(
    core: &Arc<StateLock<BotState>>,
    clamp_margin: U256,
    path_id: u64,
    result: &mut SolvePathResult,
    registry: &PathRegistry,
) -> u64 {
    let Some(path) = registry.get(path_id) else {
        return 0; // Unknown path → nothing to clamp
    };
    let core = core.read_at(LockSite::Solver);
    clamp_result_with_state(&core, path_id, &path.pools, clamp_margin, result)
}
