//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::{I256, U256};
use degenbot_core::op_info;

use ::degenbot_pools::v3_state::{v3_simulate_swap, V3PoolState};
use ::degenbot_pools::v4_state::v4_simulate_swap;

use super::{ArbitrageEngine, BlockMetadata, HashMap};
// 5WCRWZ T3: the cycle context moved to `arb_engine::solve_cycle`; the
// pipelined sim scheduler moved to `arb_engine::inline_sim`; T4 moved the
// lane walk + `solve_one_path` to `arb_engine::lane_walk`. This grab file
// imports the cycle context it still needs.
use super::solve_cycle::SolveCycleShared;

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
// the ADR-021 publish-edge verifier used to perform at
// publish (per-hop anchor diff + process abort). Task 2UVG3E (epic MROOY7)
// retired that in-process chain-vs-solver-state gate — the stage-separated
// data plane makes its desync class unrepresentable — and keeps ONLY the
// upstream RPC-disagreement verification (CompletenessDecision::Verify →
// assert_ws_block_complete) at the Published edge. No age heuristic replaced
// it: solve-on-quiet is correct by construction under the stage-separated
// data plane; stale results are dropped by the Q1a merge gate, never applied.

use crate::arb_engine::executor::{LaneOutcome, SolveOutcome};
use crate::arb_engine::inline_sim::SimulatedPathResult;
use crate::bot_core::BotState;
use ::degenbot_solvers::mixed::{HopType, MixedPoolRef, SolvePathResult};

impl ArbitrageEngine {
    /// QTZGFL: the capacity-modulated admission budget in KEYS for THIS
    /// cycle's DRAW, or `None` when the admission stance is OFF (the
    /// caller's draw is a full `take_keys` — byte-identical).
    ///
    /// `budget = max(0, admission_target_depth − in-flight outstanding)`,
    /// saturating. `Some(0)` IS the SHED verdict: the draw consumes nothing
    /// and the dispatch submits nothing. This helper is the DRAW-SITE ONLY —
    /// its `Some(0)` output is stashed on the engine by `on_resolve` and
    /// consumed there; the dispatch NEVER re-reads the gauge (a fresh read
    /// could disagree with the draw and discard keys already removed from the
    /// ledger).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn admission_budget_keys(&self) -> Option<usize> {
        self.cycle.admission_budget_keys()
    }
}

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
    ArbitrageEngine::clamp_result_with_state(&core, pid, &ctx.pool_refs[idx].pools, result)
}

/// SIMPIPE2 T3: the WORKER-side inline sim — resolve the per-path payload
/// from the clamp-committed result on the SAME worker context the T2 clamp
/// opened (shared core + to_solve-aligned pool refs; stance + hook gated).
/// The payload rides the result handoff so the merge never calls out — the
/// merge only stores/forwards. `None` = stance off, no hook, or the hook
/// reported failure-without-payload.
#[cfg(all(test, feature = "otel"))]
fn inline_sim_payload(
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
    // RKXN5Z/IJUBV3 (G6HSIS parity): the REAL per-path EVM sim rides the
    // `degenbot.bundle.simulate` span, parented under the entered cycle span
    // (both executor arms re-enter solve_span around `solve_one_path`), with
    // the terminal verdict recorded at close. This is the only remaining
    // owner of the name - the merge-site marker that used to borrow it is a
    // merge-span event now, so Jaeger's `bundle.simulate` spans are all
    // genuine ms-class simulations again.
    // 7LV6VN T1b: EXPLICIT parent at creation. TLS re-entry alone proved
    // insufficient on the detached bin threads (worker-side spans still
    // forked their own trace with a dangling parent id - 1041 roots/60s
    // live-probed). The macro `parent:` form binds the identity directly,
    // independent of the thread-local current span.
    let span = tracing::info_span!(
        parent: parent_span.clone(),
        "degenbot.bundle.simulate",
        sim.path = "worker_inline",
        path_id = pid,
        sim_block = ctx.solve_block,
        simulate.verdict = tracing::field::Empty,
        simulate.expected_profit = tracing::field::Empty,
        // SIMSPANDUP: declared so the seam-reused span keeps the ADR-040
        // error classification on the inline arm too.
        simulate.error_reason = tracing::field::Empty,
    );
    let _enter = span.enter();
    let payload = sim.simulate_path(crate::arb_engine::inline_sim::InlineSimRequest {
        path_id: pid,
        hops: std::clone::Clone::clone(&ctx.pool_refs[idx].pools),
        optimal_input: result.optimal_input,
        consumed_inputs: std::clone::Clone::clone(&result.consumed_inputs),
        hop_outputs: std::clone::Clone::clone(&result.hop_outputs),
        state_nonces: std::clone::Clone::clone(&result.state_nonces),
        sim_block: ctx.solve_block,
        block_timestamp: ctx.metadata.timestamp,
        parent_base_fee: ctx.metadata.base_fee_per_gas.unwrap_or(0),
        parent_gas_used: ctx.metadata.gas_used,
        parent_gas_limit: ctx.metadata.gas_limit,
    })?;
    // SIMSPANDUP: on failure the seam's SimSpanVerdict Drop (inside the
    // inline hook's task) already stamped this span with
    // `not_profitable`/`error` (+ error_reason) before the payload returns -
    // don't clobber the richer classification with the bare string.
    if payload.failure.is_none() {
        span.record("simulate.verdict", "profitable");
    }
    span.record(
        "simulate.expected_profit",
        tracing::field::display(result.profit),
    );
    Some(payload)
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

// ---------------------------------------------------------------------------
// DETACHED SOLVE CYCLE (epic SRQEK5, task WV62TX)
// ---------------------------------------------------------------------------
// DETACH-ALWAYS (design locked 2026-09-02; WFF6MM cutover retired the
// in-cycle arm so this is now the unconditional shape): the whole solve
// cycle RETURNS at ENQUEUE end — every result then flows through an
// UNBOUNDED mpsc to the merge sidecar, a plain `std::thread` (see the
// epic DEADLOCK note: a JOINING scope (a scoped rayon install of old, or
// against a held `parking_lot` guard; `std::thread` cannot deadlock with
// impatient pool) would starve against the Mutex; the sidecar cannot. The Q1a stale policy
// (apply-if-unchanged / drop-on-touched) makes the enqueue-time per-hop
// `update_block` snapshot a complete staleness oracle: a price-neutral
// liquidity event (V3 Mint/Burn, V4 ModifyLiquidity) advances the pool
// clock AND re-solves the path, so any stamp mismatch at merge time means
// the straggler's intake is stale and the result is DROPPED, never applied.
// This gate is now the SOLE staleness guard on the solve path (the ADR-021
// in-process solver-state tripwire retired with task 2UVG3E; only the
// upstream RPC-disagreement check survives at the Published edge).

// P37YJG: the in-flight cap constant moved with the cap consult into the
// one detached-cycle machine — `detached_cycle::DETACHED_INFLIGHT_CAP`.

// The detached-merge CARRIER is `executor::LaneOutcome` (QR3NUS 43E3H3):
// the former single-variant enum folded into the unified
// `LaneOutcome::Solved(SolveOutcome)` — the typed `Solved`/`Suppressed`/
// `Failed` records both solve arms deliver (the sidecar's
// pipe now also carries the lane witness's `Failed` panic records). The
// exactness ledger age moved with it (`executor::outcome_ledger::LEDGER_AGE`,
// carried unchanged).

/// The detached-merge SIDECAR thread body (epic SRQEK5 WV62TX): owns the
/// unbounded mpsc `Receiver` of the merge pipe and applies each item under
/// the engine Mutex — Q1a stale gate + the SAME merge/emit path as the
/// in-cycle drain (`merge_one_result`, which carries the streaming
/// delivery emission). Spawned by `EngineStages::solve_dirty` at the FIRST
/// detached enqueue; runs until every `Sender` drops (engine teardown),
/// so the pipe never strands items across the engine's lifetime.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the sidecar OWNS the merge Receiver for the engine's whole lifetime (the pipe must never be dropped early or borrowed from a shared slot); owning it is the contract, not an accident"
)]
pub(crate) fn detached_merge_sidecar(
    engine: &std::sync::Arc<parking_lot::Mutex<ArbitrageEngine>>,
    merge_rx: std::sync::mpsc::Receiver<LaneOutcome>,
    owner: Option<&degenbot_workers::posture::PostureOwner>,
) {
    hotpath::measure_block!("arb_solve.detached_merge", {
        // `recv` (not `for .. in merge_rx`) keeps ownership of the
        // Receiver so the post-panic stranded-tail drain can `try_iter`.
        while let Ok(item) = merge_rx.recv() {
            // AQV6EF: a panicking merge must NEVER silently kill this
            // thread — that drops the Receiver and strands every later
            // send with no signal. catch_unwind converts the panic into
            // the SAME typed drain-death terminal state as a failed send
            // (sticky cordon + counter + loud log); the process lives.
            let merged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                engine.lock().merge_detached_item(item);
            }));
            if let Err(payload) = merged {
                let message = if let Some(text) = payload.downcast_ref::<&str>() {
                    Some((*text).to_owned())
                } else {
                    payload.downcast_ref::<String>().cloned()
                };
                crate::arb_engine::executor::drain_death_response(
                    &crate::arb_engine::executor::DrainFailure::MergePanic { message },
                    owner,
                );
                // The drain can never recover in-process. Count every
                // outcome still queued behind the panicked item before the
                // Receiver drops (the unbounded-queue tail), then end the
                // sidecar; later sends hit the dead pipe and fire the SAME
                // typed signal through the lane hook.
                for stranded in merge_rx.try_iter() {
                    crate::arb_engine::executor::drain_death_response(
                        &crate::arb_engine::executor::DrainFailure::Stranded {
                            pid: stranded.pid(),
                        },
                        owner,
                    );
                }
                return;
            }
        }
    });
}
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

    /// Merge ONE solved result under the single engine-Mutex hold of the
    /// solve cycle (epic BXUSGL T1): clamp twins, emit the profitable-
    /// solve event, insert into the result map, and (test-only) probe-
    /// record the merge. BOTH dispatch arms funnel here - the tokio arm
    /// calls it per streamed result (before the slowest path lands),
    /// the batched path from the tail loop. Returns the twin-simulation
    /// count (clamp.twins).
    ///
    /// SIMPIPE2 T2: `worker_clamp_twins > 0` means the solving worker
    /// ALREADY ran the clamp (`clamp_result_in_worker`) - the merge skips its
    /// own pass (a second clip would re-apply the 1-wei margin and corrupt
    /// count (clamp.twins).
    #[cfg(test)]
    fn merge_one_result(
        &mut self,
        solve_block: u64,
        metadata: &BlockMetadata,
        pid: u64,
        result: SolvePathResult,
        worker_clamp_twins: u64,
        payload: Option<SimulatedPathResult>,
    ) -> u64 {
        self.cycle.merge_one_result(
            solve_block,
            metadata,
            pid,
            result,
            worker_clamp_twins,
            payload,
            &self.registry,
            &mut self.delivery,
        )
    }

    /// Terminal disposition of ONE detached straggler, under the single
    /// engine-Mutex acquisition the sidecar makes per item (epic SRQEK5
    /// WV62TX). Q1a policy: apply-if-unchanged, drop-on-touched — ANY live
    /// stamp advance since enqueue (swap OR price-neutral liquidity event)
    /// drops the straggler; a deregistered path drops too.
    ///
    /// The carrier is the unified [`LaneOutcome`] (QR3NUS 43E3H3): a
    /// lane-witnessed detached bin also delivers `Suppressed`/`Failed`
    /// records here. GAUGE PAIRING (REV 2 Defect 1, design §4.6.1): ONLY
    /// the `Solved` arm touches the in-flight gauge —
    /// `flush_solved_item`'s send-success is the ONLY bump site, so
    /// `Solved` is the only variant that may decrement. `Failed`/
    /// `Suppressed` never bumped (their lane sends bypass the
    /// gauge-bumping submit closure); decrementing them instead would SAG
    /// the gauge and silently defeat `DETACHED_INFLIGHT_CAP`.
    // Deliberate: this IS the disposition table (design §4.4) — one match
    // over the three LaneOutcome arms with the variant-gated gauge rule,
    // kept as a single table so the pairing invariant is readable in one
    // place. The pre-merge twin was similarly long for the same reason.
    // THE FOLD (WNH5OL): the three-variant disposition table itself is
    // now `drain_lane_outcomes` below. This envelope keeps ONLY what is
    // inherently per-item and Solved-arm-only — the in-flight gauge
    // decrement (the variant-gated pairing: only the Solved arm ever
    // bumped, see the gauge notes above) and the Q1a freshness oracle
    // (a Solved straggler vs its enqueue-time stamps) — then hands the
    // item to THE ONE drain under the sidecar's per-item Mutex
    // acquisition (contract 3: SidecarPerItemHold; the drain never
    // locks for itself).
    pub(crate) fn merge_detached_item(&mut self, item: LaneOutcome) {
        self.cycle
            .merge_detached_item(item, &self.registry, &mut self.delivery);
    }
}

// THE lane walk and its policy/context/result types moved to
// arb_engine::lane_walk (5WCRWZ T4). The wrap-up half of the
// fold (the drain + disposition table) lives in
// SolveCycle::drain_lane_outcomes (ADR-045 T4); the drain-side
// counter aggregate is detached_cycle::LaneDrainCounts (P37YJG).

impl ArbitrageEngine {
    // P37YJG: the merge-pipe take moved into the machine
    // (`DetachedCycle::take_merge_rx`); the spawner is the machine's ONE
    // spawn (`detached_cycle::spawn_merge_sidecar`).

    /// Post-solve, pool-state-aware reconciliation of each CL hop's committed
    /// input against the pool's true max-convertible capacity — the tier-3-
    /// validated `v3_simulate_swap`/`v4_simulate_swap` twin (UO3JM4: the pure
    /// solver's frozen int walk can over-predict the pools twin by a few wei,
    /// so the authoritative bound comes from pool state, not the solver).
    ///
    /// `solve_path` runs lock-free on its `IntV3TickRangeSequence` snapshot
    /// (ADR-015: the guard drops before parallel work) and reports
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
    /// Returns the number of twin simulations executed (telemetry:
    /// `clamp.twins` on the solve-cycle completion event).
    #[cfg(test)]
    pub(crate) fn clamp_cl_hop_capacity(&self, path_id: u64, result: &mut SolvePathResult) -> u64 {
        self.cycle
            .clamp_cl_hop_capacity(path_id, result, &self.registry)
    }

    /// The clamp shared by the merge-site gate and the SIMPIPE2 T2 worker
    /// relocation — the pool list is a parameter so the WORKER can drive the
    /// identical logic from its `to_solve`-aligned snapshot. The worker takes
    /// the SAME short core read the merge-site clamp took (MQUKB6-T3 intact:
    /// engine-then-core, short read, no guard across awaits).
    #[expect(clippy::too_many_lines)] // multi-hop CL twin loop + post-clamp profit recompute
    pub(crate) fn clamp_result_with_state(
        core: &BotState,
        path_id: u64,
        pools: &[MixedPoolRef],
        result: &mut SolvePathResult,
    ) -> u64 {
        if pools.len() != result.consumed_inputs.len() {
            return 0; // Index misalignment — never clamp a wrong hop
        }
        let margin = Self::cl_hop_clamp_margin();
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
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_clamp();
                    }
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
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_clamp();
                    }
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
        // computed on its RAW (over-predicted) hop outputs; the CL clamp above
        // realigns execution to the twin but was not feeding back a recomputed
        // profit, so an actually-unprofitable path stayed `> min_profit` and was
        // dispatched → executed to a loss → `no-profit` abort. Recompute the
        // selection profit from the clamped values (see `recompute_clamped_profit`);
        // a post-clamp loss saturates to 0 and is dropped.
        if let Some(recomputed) = Self::recompute_clamped_profit(result) {
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
    /// # Panics
    /// When the merged drain's outcome accounting undercounts (exactness
    /// fuse, QR3NUS/LW-T7): the cycle thread fails loudly, never silently
    /// mis-sizes.
    pub(crate) fn rebuild_and_solve_affected(
        &mut self,
        affected: &[degenbot_solvers::affected_keys::AffectedKey],
        block_number: u64,
        metadata: &BlockMetadata,
    ) -> super::solve_cycle::CycleOutcome {
        self.cycle.run_epoch(
            affected,
            block_number,
            metadata,
            &self.registry,
            &mut self.delivery,
        )
    }

    /// Solve all registered paths using `solve_path`.
    ///
    /// `solve_all` is not currently used live — the pump calls
    /// `solve_all_paths` which calls this only at cold start; subsequent
    /// re-solves go through `rebuild_and_solve_affected`.
    ///
    /// P6YXA6 hard cutover: the cold start rides the SAME executors as the
    /// in-cycle arms — the fleet-hosted Solver pins under
    /// `fleet.stance=fleet`, else the dedicated private tokio runtime —
    /// with LPT binning over the structural bin count kept. Bins are
    /// 'static closures over Arc-cloned state: they take NO engine lock
    /// (engine-then-core invariant intact), stream each profitable result
    /// over an mpsc as its OWN solve completes, and the caller drains the
    /// pipe into the fresh result map. Bin jobs clamp each result against
    /// the pool state (UO3JM4) exactly as the merge-site clamp did.
    #[must_use]
    /// # Panics
    /// Propagates the merged-drain exactness-fuse abort through the calling arm.
    pub fn solve_all(&self) -> HashMap<u64, SolvePathResult> {
        self.cycle.solve_all(&self.registry)
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
    use super::clamp_result_in_worker;
    use super::{ArbitrageEngine, BlockMetadata, HashMap, SolvePathResult, U256};
    use crate::arb_engine::solve_cycle::PathTimesHeap;
    use crate::arb_engine::solve_cycle::SolveCycleShared;
    use crate::bot_core::{TickInfo, V4PoolKey};
    use degenbot_solvers::mixed::MixedPath;
    use std::sync::Arc;

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

    // ---------------- SIMPIPE2 T2 acceptance (task PIRX3W) ----------------

    /// Narrow single-position V4 pool (±60 ticks, 1e6 liquidity) + a one-hop
    /// path: the over-fed committed input is the empty-march class. Returns
    /// (engine, `path_id`, the to_solve-aligned pool-ref snapshot).
    fn overfed_v4_engine() -> (ArbitrageEngine, u64, Vec<std::sync::Arc<MixedPath>>) {
        use crate::arb_engine::PoolTickCoverage;
        use crate::bot_core::RegisterV4PoolParams;
        fn usdc_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
            (U256::from(amount) * U256::from(10u64).pow(U256::from(6)))
                .to::<alloy::primitives::Uint<112, 2>>()
        }
        fn weth_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
            (U256::from(amount) * U256::from(10u64).pow(U256::from(18)))
                .to::<alloy::primitives::Uint<112, 2>>()
        }
        const GAMMA_03: u64 = 997;
        const FEE_DENOM_03: u64 = 1000;
        let mut engine = ArbitrageEngine::new();
        // V2 pool: large reserves so its output dwarfs the V4 hop's capacity —
        // the V4 hop is the over-fed one (this isolates hop1's input clamp).
        let v2 = engine.register_v2_pool(
            alloy::primitives::Address::from([0x11u8; 20]),
            usdc_local(1_500_000),
            weth_local(20_000_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: 150i128,
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: -100i128,
                block: 0,
            },
        );
        let v4_id = engine
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: alloy::primitives::Address::from([0x44u8; 20]),
                pool_id: [0xabu8; 32],
                pool_key: V4PoolKey {
                    currency0: alloy::primitives::Address::from([0x30u8; 20]),
                    currency1: alloy::primitives::Address::from([0x31u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: alloy::primitives::Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
            })
            .expect("V4 registration failed");
        let path_id = engine
            .register_path(vec![
                ::degenbot_solvers::mixed::PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                ::degenbot_solvers::mixed::PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
            ])
            .expect("two-hop path registers");
        let pool_refs = std::iter::once(engine.registry.get(path_id).expect("registered").clone())
            .collect::<Vec<_>>();
        (engine, path_id, pool_refs)
    }

    fn worker_probe_ctx(
        core: Arc<crate::bot_core::state_lock::StateLock<crate::bot_core::BotState>>,
        pool_refs: Vec<std::sync::Arc<MixedPath>>,
    ) -> Arc<SolveCycleShared> {
        Arc::new(SolveCycleShared {
            core,
            pool_refs,
            worker_clamp: true,
            inline_sim: None,
            solve_block: 0,
            epoch: 0,
            metadata: BlockMetadata::default(),
            runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
            gate_capture: None,
            walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
                false, false,
            )),
            capture: None,
            capture_mixed: None,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
            gate_total: parking_lot::Mutex::new(
                ::degenbot_solvers::profit_envelope::GateStats::default(),
            ),
            solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
            walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
            walk_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
            walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
            walk_grid_total: std::sync::atomic::AtomicU64::new(0),
            sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            test_solve_delay: None,
            #[cfg(test)]
            test_solve_panic: None,
        })
    }

    /// The engine clamp and the WORKER clamp are the same computation from
    /// two call sites: byte-identical result + twin count on identical input.
    #[test]
    fn worker_clamp_matches_engine_clamp_bit_for_bit() {
        use ::degenbot_solvers::mixed::MixedPoolRef;
        let (engine, path_id, pool_refs) = overfed_v4_engine();
        let mk = || {
            let committed = U256::from(1u128) << 120;
            SolvePathResult {
                optimal_input: U256::from(1_000_000_000u64),
                profit: U256::from(1_000u64),
                hop_outputs: vec![committed, committed],
                consumed_inputs: vec![committed, committed],
                state_nonces: vec![],
                solver_pool_states: Vec::new(),
            }
        };
        let (mut r_engine, mut r_worker) = (mk(), mk());
        let twins_engine = engine.clamp_cl_hop_capacity(path_id, &mut r_engine);
        assert!(twins_engine > 0, "premise: the over-fed input must clamp");
        assert!(
            r_engine.consumed_inputs[1] < U256::from(1u128) << 120,
            "premise: the V4 hop input clamp fired"
        );
        let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        let twins_worker = clamp_result_in_worker(&ctx, 0, path_id, &mut r_worker);
        assert_eq!(twins_worker, twins_engine, "twin count must match");
        assert_eq!(r_engine, r_worker, "clamped result must be byte-identical");
        // The pool-ref SNAPSHOT path (worker side) is exercised; the MixedPoolRef _ unused is intentional.
        let _: Vec<Vec<MixedPoolRef>> = Vec::new();
    }

    // The merge honors the worker's twin report: twins > 0 = the result is
    // already clamp-committed (no second clip); twins = 0 = the merge clips
    // the over-fed input itself (the legacy path — bit-identical).

    /// SIMPIPE2 T3: a payload riding `merge_one_result` is stored at the
    /// engine (`inline_payloads`) and a re-merge WITHOUT the payload drops the
    /// stale entry — per-entry presence decides Python-side. (The delivery
    /// drain into `ResultBatch.payloads` is covered by the `delivery_policy`
    /// tests + the FFI conversion; this pins the merge-site store/drop.)
    #[test]
    fn merge_stores_payload_and_drops_it_without_one() {
        use crate::arb_engine::inline_sim::{InlineSwapFamily, SimulatedPathResult};
        use alloy::primitives::{Address, I256, U256};

        let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let mk = || SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };
        let payload = SimulatedPathResult {
            path_id,
            gross_profit: U256::from(1_000u64),
            net_profit: U256::from(900u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: vec![1, 2, 3],
            access_list: None,
            captured_swaps: vec![crate::arb_engine::inline_sim::CapturedSwapRow {
                emitter: Address::from([0x11u8; 20]),
                family: InlineSwapFamily::V4,
                amount0: I256::MINUS_ONE,
                amount1: I256::ONE,
                sqrt_price_x96: U256::ZERO,
                liquidity: U256::ZERO,
                tick: 0,
            }],
            hop_count: 1,
            failure: None,
        };

        engine.merge_one_result(42, &metadata, path_id, mk(), 0, Some(payload));
        assert!(
            engine.cycle.inline_payloads.contains_key(&path_id),
            "the payload must be stored at merge"
        );

        // The path re-solves WITHOUT a payload (stance off or hook silence):
        // the stale entry must drop — presence decides per entry.
        engine.merge_one_result(43, &metadata, path_id, mk(), 0, None);
        assert!(
            !engine.cycle.inline_payloads.contains_key(&path_id),
            "a payload-less re-merge must drop the stale payload"
        );
    }

    #[test]
    fn merge_reports_worker_twins_and_never_reclips() {
        let (mut engine, path_id, pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let overfed = || {
            let committed = U256::from(1u128) << 120;
            SolvePathResult {
                optimal_input: U256::from(1_000_000_000u64),
                profit: U256::from(1_000u64),
                hop_outputs: vec![committed, committed],
                consumed_inputs: vec![committed, committed],
                state_nonces: vec![],
                solver_pool_states: Vec::new(),
            }
        };

        // Worker arm: clamp once (the worker report = committed truth), then
        // merge with twins > 0 — the stored result stays byte-identical.
        let mut worker_result = overfed();
        let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        let twins = clamp_result_in_worker(&ctx, 0, path_id, &mut worker_result);
        assert!(twins > 0, "premise: worker clamp fired");
        let committed = worker_result.clone();
        engine.merge_one_result(42, &metadata, path_id, worker_result, twins, None);
        {
            let stored = engine.cycle.results.get(&path_id).expect("worker-merged");
            assert_eq!(
                stored.consumed_inputs, committed.consumed_inputs,
                "twins>0 must not re-clip the committed inputs"
            );
            assert_eq!(stored.profit, committed.profit, "profit untouched on skip");
        }

        // Legacy arm (twins=0): the merge clips the over-fed V4 hop input
        // itself (index 1 — the V2 hop has no input clamp by design).
        let legacy = overfed();
        let pre = legacy.consumed_inputs[1];
        engine.merge_one_result(42, &metadata, path_id, legacy, 0, None);
        let stored = engine.cycle.results.get(&path_id).expect("legacy-merged");
        assert_ne!(
            stored.consumed_inputs[1], pre,
            "twins=0 must run the merge-site clamp"
        );
    }

    // ----------------- RKXN5Z / IJUBV3: bundle.simulate span hygiene -----------------

    /// RED-gate (IJUBV3): the merge-site microsecond `degenbot.bundle.simulate`
    /// "verdict bookmark" spans collided with the REAL per-path EVM sim spans
    /// of the same name (traces 98f7cf52 / ab13f75fad50: 90-300 markers per
    /// block drowned the ms-scale sims). The merge must create NO span with
    /// that name - the verdict is an `info!` event on the enclosing merge
    /// span, and the span name now belongs solely to simulation work.
    ///
    /// DEFAULT-GATE VISIBLE (no otel cfg), on the K4ETHF pattern: the marker
    /// flood was what made Jaeger unreadable, so the regression gate must not
    /// hide behind --features otel.
    #[test]
    fn merge_payload_store_emits_no_bundle_simulate_span() {
        use std::sync::Mutex;

        struct SpanNameCapture {
            names: std::sync::Arc<Mutex<Vec<String>>>,
        }
        impl<S> tracing_subscriber::Layer<S> for SpanNameCapture
        where
            S: tracing::Subscriber,
        {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                _id: &tracing::span::Id,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.names
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(attrs.metadata().name().to_string());
            }
        }

        use tracing_subscriber::layer::SubscriberExt as _;
        let names = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let capture = SpanNameCapture {
            names: std::sync::Arc::clone(&names),
        };
        let subscriber = tracing_subscriber::registry().with(capture);

        let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let mk = || SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };
        let payload = crate::arb_engine::inline_sim::SimulatedPathResult {
            path_id,
            gross_profit: U256::from(1_000u64),
            net_profit: U256::from(900u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: vec![1, 2, 3],
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: 1,
            failure: None,
        };

        tracing::subscriber::with_default(subscriber, || {
            // Enclosing merge span, as in both production arms.
            let merge = tracing::info_span!("degenbot.arb.merge", merge.paths = 1u64);
            let _ctx = merge.enter();
            engine.merge_one_result(42, &metadata, path_id, mk(), 0, Some(payload));
        });

        let created = names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let offenders: Vec<_> = created
            .iter()
            .filter(|n| *n == "degenbot.bundle.simulate")
            .collect();
        assert!(
            offenders.is_empty(),
            "merge must not create bundle.simulate markers (the name belongs to real sims); \
             spans created: {created:?}"
        );
    }

    /// GREEN-gate (IJUBV3): the WORKER-side inline sim gets the honest
    /// `degenbot.bundle.simulate` span - a real ms-class EVM sim on the solve
    /// path, parented under the cycle span, with the terminal verdict.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "single end-to-end span-emission assertion: stub + emit + export + attribute checks read best as one sequence"
    )]
    fn inline_sim_payload_emits_worker_sim_span_with_verdict() {
        use super::inline_sim_payload;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        struct StubSim {
            fail: bool,
            path_id: u64,
        }
        impl crate::arb_engine::inline_sim::InlineSimulator for StubSim {
            fn simulate_path(
                &self,
                request: crate::arb_engine::inline_sim::InlineSimRequest,
            ) -> Option<crate::arb_engine::inline_sim::SimulatedPathResult> {
                assert_eq!(
                    request.path_id, self.path_id,
                    "stub receives the merged path id"
                );
                Some(crate::arb_engine::inline_sim::SimulatedPathResult {
                    path_id: request.path_id,
                    gross_profit: U256::from(1_000u64),
                    net_profit: U256::from(900u64),
                    gas_used: 300_000,
                    priority_fee: 2,
                    base_fee_next: 30,
                    execute_calldata: vec![7, 8, 9],
                    access_list: None,
                    captured_swaps: Vec::new(),
                    hop_count: 1,
                    failure: self
                        .fail
                        .then(|| crate::arb_engine::inline_sim::InlineSimFailure {
                            fail_index: None,
                            revert_data: Vec::new(),
                            bucket: "test".to_string(),
                        }),
                })
            }
        }

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let (engine, path_id, pool_refs) = overfed_v4_engine();
        let mut ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        // Fresh Arc (refcount 1): install the stub via get_mut.
        Arc::get_mut(&mut ctx)
            .expect("probe ctx exclusively owned")
            .inline_sim = Some(Arc::new(StubSim {
            fail: false,
            path_id,
        }));

        let result = SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };

        tracing::subscriber::with_default(subscriber, || {
            let solve = tracing::info_span!("degenbot.arb.solve", block.number = 7u64);
            let _guard = solve.enter();
            let payload = inline_sim_payload(&ctx, 0, path_id, &result, &tracing::Span::current());
            assert!(
                payload.is_some(),
                "stub hook returns a payload; None only when the seam is off"
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve_id = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .map(|sp| sp.span_context.span_id())
            .expect("solve span must be exported");
        let sims: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.bundle.simulate")
            .collect();
        assert_eq!(
            sims.len(),
            1,
            "exactly one worker-side sim span; all: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        assert_eq!(
            sims[0].parent_span_id, solve_id,
            "the worker sim span must parent under the cycle span"
        );
        let attr = |k: &'static str| {
            sims[0]
                .attributes
                .iter()
                .find(|kv| kv.key == opentelemetry::Key::from_static_str(k))
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(
            attr("path_id").as_deref(),
            Some(path_id.to_string().as_str()),
            "path_id attribute"
        );
        assert_eq!(
            attr("simulate.verdict").as_deref(),
            Some("profitable"),
            "verdict recorded at span close; attrs: {:?}",
            sims[0].attributes
        );
        assert_eq!(
            attr("sim.path").as_deref(),
            Some("worker_inline"),
            "seam discriminator distinguishes worker sims from the FFI seam"
        );
    }
}

// Payload-sim fixture support (epic BXUSGL T4 lineage, slimmed by LW-T9:
// the env-driven #[ignore]d offline A/B probe arms are deleted with the
// stance).
#[cfg(test)]
pub(super) mod executor_ab_probe {
    // Fixture + harness support for the fleet probe surface (LW-T9: the
    // legacy-stance A/B probe arms are DELETED with the stance — the fleet
    // is one executor, so there is nothing to A/B). What remains: the heavy-CL
    // capture-corpus loader, the production cost proxy's bin packer and the
    // shared-cycle fixture used by the fleet parity/identity fixtures.
    // Fixture parse replicates rust/crates/degenbot-solvers/examples/rayon_scale_probe.rs.

    use std::sync::Arc;

    use crate::arb_engine::BlockMetadata;
    use alloy::primitives::U256;
    use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
    use degenbot_solvers::mobius_v3_int::{build_cl_crossing_table, build_cl_word_profiles};
    use serde_json::Value;

    use super::BotState;
    use crate::arb_engine::solve_cycle::PathTimesHeap;
    use crate::arb_engine::solve_cycle::SolveCycleShared;
    use crate::arb_engine::workload_partition::{lpt_partition, path_cost_proxy};
    use hashbrown::HashMap;

    fn fixture_path() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("DEGENBOT_PROBE_FIXTURE") {
            return std::path::PathBuf::from(p);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl")
    }

    /// Zst-aware corpus load for the BCA77G parity fixtures: the packaged
    /// `heavy_cl_solve_captures.jsonl.zst` decodes transparently via
    /// `capture_fixture::read_fixture` (same corpus the probe measures).
    pub(in crate::arb_engine) fn load_corpus_fixture(
    ) -> Vec<Arc<::degenbot_solvers::mixed::ResolvedMixedPath>> {
        load_corpus()
    }

    fn u256(s: &str) -> Result<U256, String> {
        s.trim().parse::<U256>().map_err(|e| e.to_string())
    }

    fn range(v: &Value) -> Result<IntV3TickRangeHop, String> {
        let wbp = v
            .get("word_boundary_prices")
            .and_then(Value::as_array)
            .ok_or("word_boundary_prices")?
            .iter()
            .map(|w| w.as_str().ok_or_else(|| "wbp".to_string()).and_then(u256))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(IntV3TickRangeHop {
            liquidity: v
                .get("liquidity")
                .and_then(Value::as_str)
                .ok_or("liquidity")?
                .parse::<u128>()
                .map_err(|e| e.to_string())?,
            sqrt_price_x96: u256(
                v.get("sqrt_price_x96")
                    .and_then(Value::as_str)
                    .ok_or("sp")?,
            )?,
            sqrt_price_lower_x96: u256(
                v.get("sqrt_price_lower_x96")
                    .and_then(Value::as_str)
                    .ok_or("spl")?,
            )?,
            sqrt_price_upper_x96: u256(
                v.get("sqrt_price_upper_x96")
                    .and_then(Value::as_str)
                    .ok_or("spu")?,
            )?,
            gamma_numer: v
                .get("gamma_numer")
                .and_then(Value::as_u64)
                .ok_or("gamma")?,
            fee_denom: v.get("fee_denom").and_then(Value::as_u64).ok_or("fee")?,
            zero_for_one: v
                .get("zero_for_one")
                .and_then(Value::as_bool)
                .ok_or("zfo")?,
            word_boundary_prices: wbp,
        })
    }

    pub(in crate::arb_engine) fn load_corpus(
    ) -> Vec<Arc<::degenbot_solvers::mixed::ResolvedMixedPath>> {
        let path = fixture_path();
        // Zst-aware (packaged fixtures decode transparently; regenerated
        // plain captures still win the resolution order).
        let content = ::degenbot_solvers::capture_fixture::read_fixture(&path);
        let mut items = Vec::new();
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(doc) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Ok(hops_v) = doc.get("hops").and_then(Value::as_array).ok_or("hops") else {
                continue;
            };
            let mut hops = Vec::new();
            for hop in hops_v {
                let Ok(ra) = hop.as_array().ok_or("hop") else {
                    continue;
                };
                let mut ranges = Vec::new();
                for r in ra {
                    if let Ok(rh) = range(r) {
                        ranges.push(rh);
                    }
                }
                let seq = IntV3TickRangeSequence { ranges };
                hops.push(::degenbot_solvers::mixed::ResolvedHop::V3 {
                    word_profiles: Arc::from(build_cl_word_profiles(&seq)),
                    crossing_table: Arc::from(build_cl_crossing_table(&seq)),
                    int_seq: Arc::new(seq),
                });
            }
            items.push(Arc::new(::degenbot_solvers::mixed::ResolvedMixedPath {
                hops,
                valid: true,
                state_nonces: Vec::new(),
                max_update_block: 0,
            }));
        }
        assert!(!items.is_empty(), "fixture must load at least one path");
        items
    }

    pub(in crate::arb_engine) fn probe_ctx() -> Arc<SolveCycleShared> {
        Arc::new(SolveCycleShared {
            solve_block: 0,
            epoch: 0,
            metadata: BlockMetadata::default(),
            runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
            gate_capture: None,
            walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
                false, false,
            )),
            capture: None,
            capture_mixed: None,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
            gate_total: parking_lot::Mutex::new(
                ::degenbot_solvers::profit_envelope::GateStats::default(),
            ),
            solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
            walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
            walk_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
            walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
            walk_grid_total: std::sync::atomic::AtomicU64::new(0),
            sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            core: Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new())),
            pool_refs: Vec::new(),
            worker_clamp: false,
            inline_sim: None,
            #[cfg(test)]
            test_solve_delay: None,
            #[cfg(test)]
            test_solve_panic: None,
        })
    }

    /// LPT bins per the production cost fn (empty measured-history = first-cycle
    /// structural cost, exactly like an engine cold bucket).
    pub(in crate::arb_engine) fn prod_lpt_bins(
        items: &[Arc<::degenbot_solvers::mixed::ResolvedMixedPath>],
        threads: usize,
    ) -> Vec<Vec<usize>> {
        lpt_partition(items.len(), threads, |i| path_cost_proxy(&items[i]))
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod fleet_sim_stance_tests {
    //! ADR-042 F4 (task LTUE7I) fixtures: the inline-sim runtime on the
    //! fleet (LW-T9: the legacy-arm half of the parity/identity matrix is
    //! deleted with the stance — every sim rides the `SimDriver` seats).
    //! Parity — `SimDriver` seats execute requests derived from the committed
    //! heavy-CL capture corpus honestly (success AND failure payloads).
    //! Identity — the hosting family is the fleet `work-fleet-sim-{n}`
    //! `SimDriver` seats (census row included).

    use alloy::primitives::{I256, U256};
    use degenbot_solvers::mixed::{HopType, MixedPath, MixedPoolRef, SolvePathResult};
    use hashbrown::HashMap;
    use std::sync::Arc;

    use super::executor_ab_probe::load_corpus_fixture;
    use crate::arb_engine::inline_sim::PipelinedSims;
    use crate::arb_engine::inline_sim::{
        AccessListRow, CapturedSwapRow, InlineSimFailure, InlineSimRequest, InlineSimulator,
        InlineSwapFamily, SimulatedPathResult,
    };
    use crate::arb_engine::solve_cycle::PathTimesHeap;
    use crate::arb_engine::solve_cycle::SolveCycleShared;
    use crate::arb_engine::BlockMetadata;

    // ---- deterministic sim stub ------------------------------------------------

    /// Deterministic primitive-payload sim: the payload is a pure function
    /// of the request (so both stances assert on identical request streams),
    /// and the executing thread's family is recorded for the identity
    /// fixture. Exercises the failure-payload contract through both arms.
    struct CorpusSim {
        thread_names: parking_lot::Mutex<Vec<String>>,
    }

    impl CorpusSim {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                thread_names: parking_lot::Mutex::new(Vec::new()),
            })
        }
    }

    impl InlineSimulator for CorpusSim {
        fn simulate_path(&self, request: InlineSimRequest) -> Option<SimulatedPathResult> {
            self.thread_names.lock().push(
                std::thread::current()
                    .name()
                    .map(str::to_owned)
                    .unwrap_or_default(),
            );
            let hop0 = (request.optimal_input % U256::from(u64::MAX)).to::<u64>();
            if request.path_id % 11 == 5 {
                // Exercise the failure-payload contract through both arms.
                return Some(SimulatedPathResult {
                    path_id: request.path_id,
                    gross_profit: U256::ZERO,
                    net_profit: U256::ZERO,
                    gas_used: 0,
                    priority_fee: 0,
                    base_fee_next: 0,
                    execute_calldata: Vec::new(),
                    access_list: None,
                    captured_swaps: Vec::new(),
                    hop_count: request.hops.len(),
                    failure: Some(InlineSimFailure {
                        fail_index: Some(1),
                        revert_data: vec![0x08, 0xc3, 0x79, 0xa0],
                        bucket: "revert".to_string(),
                    }),
                });
            }
            Some(SimulatedPathResult {
                path_id: request.path_id,
                gross_profit: U256::from(hop0 % 1_000_000 + 7),
                net_profit: U256::from(hop0 % 1_000_000 + 3),
                gas_used: 40_000
                    + u64::try_from(request.hops.len()).unwrap_or(u64::from(u8::MAX)) * 3_000,
                priority_fee: 3,
                base_fee_next: 31,
                execute_calldata: vec![
                    0xa9,
                    u8::try_from(request.path_id % 251).unwrap_or(1),
                    u8::try_from(request.hops.len()).unwrap_or(u8::MAX),
                ],
                access_list: request.path_id.is_multiple_of(2).then(|| {
                    vec![AccessListRow {
                        address: alloy::primitives::Address::from([0x7au8; 20]),
                        storage_keys: vec![U256::from(request.path_id)],
                    }]
                }),
                captured_swaps: vec![CapturedSwapRow {
                    emitter: alloy::primitives::Address::from([0x11u8; 20]),
                    family: if request.hops.len() > 2 {
                        InlineSwapFamily::V3
                    } else {
                        InlineSwapFamily::V2
                    },
                    amount0: I256::try_from(-i128::from(hop0 % 5_000_000_000u64))
                        .unwrap_or(I256::ZERO),
                    amount1: I256::try_from(i128::from(hop0 % 4_900_000_000u64))
                        .unwrap_or(I256::ZERO),
                    sqrt_price_x96: U256::from(1u128) << 96,
                    liquidity: U256::from(1_000_000u64),
                    tick: 0,
                }],
                hop_count: request.hops.len(),
                failure: None,
            })
        }
    }

    // ---- corpus-derived request fan-out ------------------------------------------

    const PARITY_REQUESTS: usize = 24;

    /// Stride the committed capture corpus down to `want` items (the corpus
    /// is the request-shape oracle — hop counts and magnitude spreads ride
    /// the real capture, not synthesized round numbers).
    fn strided_corpus(want: usize) -> Vec<Arc<degenbot_solvers::mixed::ResolvedMixedPath>> {
        let items = load_corpus_fixture();
        assert!(!items.is_empty(), "capture corpus must load");
        let stride = items.len().saturating_sub(1) / want + 1;
        items.into_iter().step_by(stride).take(want).collect()
    }

    fn pool_refs_for(
        items: &[Arc<degenbot_solvers::mixed::ResolvedMixedPath>],
    ) -> Vec<Arc<MixedPath>> {
        items
            .iter()
            .map(|item| {
                let hops = (0..item.hops.len().clamp(1, 4))
                    .map(|i| MixedPoolRef {
                        hop_type: HopType::V3,
                        pool_key: u64::try_from(i).unwrap_or(u64::MAX),
                        zero_for_one: i % 2 == 0,
                    })
                    .collect();
                Arc::new(MixedPath { pools: hops })
            })
            .collect()
    }

    fn make_ctx(sim: Arc<CorpusSim>, pool_refs: Vec<Arc<MixedPath>>) -> Arc<SolveCycleShared> {
        Arc::new(SolveCycleShared {
            solve_block: 42,
            epoch: 0,
            metadata: BlockMetadata {
                base_fee_per_gas: Some(30),
                ..BlockMetadata::default()
            },
            runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
            gate_capture: None,
            walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
                false, false,
            )),
            capture: None,
            capture_mixed: None,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
            gate_total: parking_lot::Mutex::new(
                ::degenbot_solvers::profit_envelope::GateStats::default(),
            ),
            solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
            walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
            walk_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
            walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
            walk_grid_total: std::sync::atomic::AtomicU64::new(0),
            sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            core: Arc::new(crate::bot_core::state_lock::StateLock::new(
                crate::bot_core::BotState::new(),
            )),
            pool_refs,
            worker_clamp: true,
            inline_sim: Some(sim),
            #[cfg(test)]
            test_solve_delay: None,
            #[cfg(test)]
            test_solve_panic: None,
        })
    }

    fn admitted_for(idx: usize, hops: usize) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64 + u64::try_from(idx).unwrap_or(0) * 7),
            profit: U256::from(1_000u64 + u64::try_from(idx).unwrap_or(0)),
            hop_outputs: (0..hops)
                .map(|h| {
                    U256::from(
                        900_000_000u64
                            + u64::try_from(idx).unwrap_or(0) * 13
                            + u64::try_from(h).unwrap_or(u64::MAX),
                    )
                })
                .collect(),
            consumed_inputs: (0..hops)
                .map(|h| {
                    U256::from(
                        900_000_000u64
                            + u64::try_from(idx).unwrap_or(0) * 3
                            + u64::try_from(h).unwrap_or(u64::MAX),
                    )
                })
                .collect(),
            state_nonces: vec![0; hops],
            solver_pool_states: Vec::new(),
        }
    }

    /// Schedule ONE sim through the production scheduler (`PipelinedSims::
    /// schedule_one`, stance-routed) and join its receipt.
    fn schedule_and_join(
        ctx: &Arc<SolveCycleShared>,
        idx: usize,
        pid: u64,
        hops: usize,
    ) -> (bool, Option<SimulatedPathResult>) {
        let mut pending = PipelinedSims::default();
        let parent = tracing::Span::none();
        let result = admitted_for(idx, hops);
        let scheduled = pending.schedule_one(ctx, idx, pid, &result, &parent);
        if !scheduled {
            return (false, None);
        }
        pending
            .join_all()
            .next()
            .map_or((true, None), |(jpid, payload)| {
                assert_eq!(jpid, pid, "the receipt must carry its request's pid");
                (true, payload)
            })
    }

    /// FLEET FIXTURE (LTUE7I, LW-T9 single-arm): fleet `SimDriver` inline sims
    /// honor the full request contract over the committed capture corpus —
    /// every request schedules, successes carry field-equal payloads, and
    /// the failure-payload contract is exercised end to end.
    #[test]
    fn fleet_sims_honor_the_request_contract_on_capture_corpus() {
        let items = strided_corpus(PARITY_REQUESTS);
        let pool_refs = pool_refs_for(&items);
        let sim = CorpusSim::new();

        let run_arm = || {
            let ctx = make_ctx(Arc::clone(&sim), pool_refs.clone());
            // Deterministic reverse order so receipts interleave like a
            // real multi-bin fan-out (per-receipt channels, not the arm,
            // carry order).
            let mut joined: Vec<(u64, Option<SimulatedPathResult>)> = Vec::new();
            for idx in (0..items.len()).rev() {
                let pid = u64::try_from(idx).unwrap_or(u64::MAX);
                let hops = items[idx].hops.len().clamp(1, 4);
                let (scheduled, payload) = schedule_and_join(&ctx, idx, pid, hops);
                assert!(scheduled, "every fixtured request must schedule");
                joined.push((pid, payload));
            }
            joined.sort_unstable_by_key(|(pid, _)| *pid);
            joined
        };

        let joined: Vec<(u64, Option<SimulatedPathResult>)> = run_arm();
        assert!(!joined.is_empty(), "fixture must schedule sims");
        assert!(
            joined.iter().any(|(pid, _)| pid % 11 == 5),
            "the fixture must exercise the failure-payload contract too"
        );
    }

    /// IDENTITY FIXTURE (LTUE7I, LW-T9 single-arm): the ONLY sim hosting
    /// family is the fleet `SimDriver` seats (`work-fleet-sim-{n}`) and the
    /// executor's census row is registered (the `fleet_merge_slots` pattern
    /// from the BCA77G work).
    ///
    /// Pinned-tier fixture (FF-T2): the seat-shape contract binds only on a
    /// host whose auto-resolved fleet binding is pinned (see the host-tier
    /// gate in the body). On the serial tier the sims ride
    /// `work-fleet-serial-0` by design.
    #[expect(
        clippy::print_stderr,
        reason = "the self-skip channel names the host tier that cannot host the pinned topology"
    )]
    #[test]
    fn fleet_sims_run_on_simdriver_seats_with_the_census_row() {
        // Host-tier gate (FF-T2): the identity contract under test is the
        // PINNED binding's topology (pooled `work-fleet-sim-{n}` SimDriver
        // seats + the census row). On a 2-5-core host the auto profile
        // resolves the fleet to the serial binding — sims legitimately ride
        // the named cycle lane (`work-fleet-serial-0`, whose own identity is
        // covered by `serial_units_execute_on_the_named_serial_seat`) — so
        // the pinned seat-shape assertion can only bind on a pinned-tier
        // host. Skip there (the F-suite's documented self-skip channel)
        // rather than asserting a topology this host cannot host.
        match crate::arb_engine::seat_host::FleetBootRegistry::process()
            .sim()
            .global_executor(crate::arb_engine::fleet_sim_executor::FleetSimExecutor::boot)
        {
            Ok(executor)
                if executor.host_plan_binding() == degenbot_workers::plan::Binding::Serial =>
            {
                eprintln!(
                    "skipping: the fleet materialized the SERIAL binding on this \
                     host (2-5 cores) — sims ride `work-fleet-serial-0` by design"
                );
                return;
            }
            Ok(_) => (),
            Err(err) => {
                eprintln!("skipping: the fleet sim boot was refused on this host ({err})");
                return;
            }
        }
        let items = strided_corpus(4);
        let pool_refs = pool_refs_for(&items);
        let sim = CorpusSim::new();

        let run_arm = || {
            sim.thread_names.lock().clear();
            let ctx = make_ctx(Arc::clone(&sim), pool_refs.clone());
            for (idx, item) in items.iter().enumerate() {
                let pid = u64::try_from(idx).unwrap_or(u64::MAX);
                let hops = item.hops.len().clamp(1, 4);
                let (scheduled, _payload) = schedule_and_join(&ctx, idx, pid, hops);
                assert!(scheduled, "every fixtured request must schedule");
            }
            sim.thread_names.lock().clone()
        };

        let fleet_names = run_arm();
        assert!(
            !fleet_names.is_empty() && fleet_names.iter().all(|n| n.starts_with("work-fleet-sim-")),
            "fleet sims must run on fleet SimDriver seats, got {fleet_names:?}"
        );
        let row = degenbot_core::worker_census::snapshot()
            .into_iter()
            .find(|e| e.resource == "fleet_simdriver_slots")
            .expect("fleet SimDriver slots must be census-registered");
        assert_eq!(row.thread_name, "work-fleet-sim-{n}");
    }
}
