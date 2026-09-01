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
use ::degenbot_solvers::mixed::{HopType, ResolvedHop, ResolvedMixedPath, SolvePathResult};

/// How many slowest-path entries the solve-cycle completion event names
/// (D63GSE intra-solve visibility).
const SLOWEST_PATHS_K: usize = 5;

/// Q3 dense one-shot alert flag — the CONSUMER side of the moved alert: the
/// walk reports `WalkStats::max_dense_words`; this logs once per process.
static WALK_DENSE_ALERTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ---------------------------------------------------------------------------
// RAYPAR T3: LPT-pre-balanced scoped-thread partition
// ---------------------------------------------------------------------------

#[expect(clippy::doc_markdown)]
/// RAYPAR T3: LPT (longest-processing-time) bin-packing. Sorts items by
/// descending cost and greedily assigns each to the least-loaded bin. Returns
/// indices into the original items slice, one Vec per bin.
///
/// The RAYPAR lab (docs/rayon-parallelism-lab.md) showed rayon work-stealing
/// par_iter achieves only 4.91/8 efficiency on the heavy-CL capture corpus
/// because the workload has extreme cost skew (top 8 of 80 paths = 60% of CPU).
/// LPT pre-balances so no thread gets stuck with an unsplittable giant while
/// others idle — achieving 7.80/8 (35% wall reduction). Same solver, same
/// threads, same memory bandwidth.
fn lpt_partition(n_items: usize, n_bins: usize, cost: impl Fn(usize) -> usize) -> Vec<Vec<usize>> {
    if n_bins == 0 {
        return Vec::new();
    }
    if n_items == 0 {
        return vec![Vec::new(); n_bins];
    }
    let mut idx: Vec<usize> = (0..n_items).collect();
    idx.sort_unstable_by_key(|&i| std::cmp::Reverse(cost(i)));
    let mut loads = vec![0usize; n_bins];
    let mut bins: Vec<Vec<usize>> = vec![Vec::new(); n_bins];
    for i in idx {
        let mi = loads
            .iter()
            .enumerate()
            .min_by_key(|&(_, l)| l)
            .map_or(0, |(i, _)| i);
        bins[mi].push(i);
        loads[mi] += cost(i);
    }
    bins
}

#[expect(clippy::doc_markdown)]
/// Resolve-time cost proxy for LPT binning: the total number of word-boundary
/// prices across all CL hops. Correlates with walk combinatorics without
/// requiring a solve, so it is available at to_solve collection time.
fn path_cost_proxy(resolved: &ResolvedMixedPath) -> usize {
    resolved
        .hops
        .iter()
        .filter_map(|h| h.as_int_sequence())
        .flat_map(|seq| seq.ranges.iter())
        .map(|r| r.word_boundary_prices.len())
        .sum()
}

/// LPT cost used at binning: max(structural word-boundary proxy, previous
/// block's measured walk sims + measured gate µs). The measured counts
/// predict the current block's combinatorics better for stable pool shapes;
/// the proxy floors it for freshly dirty pools. (loop-12 BY7BLS KUKHMX;
/// loop-18 adds the gate-µs term — gate-heavy paths carry sims≈0 and were
/// bin-packed cheap while dominating wall time.) The sims and gate terms add
/// (same µs-scale: a walk sim ≈0.7-0.8µs, so `sims` ≈ walk µs).
fn sims_aware_cost(proxy: usize, last_sims: Option<u64>, last_gate_us: Option<u64>) -> usize {
    let measured = match last_sims {
        Some(v) => usize::try_from(v).unwrap_or(usize::MAX),
        None => 0,
    };
    let measured_gate = match last_gate_us {
        Some(v) => usize::try_from(v).unwrap_or(usize::MAX),
        None => 0,
    };
    proxy.max(measured.saturating_add(measured_gate))
}

#[expect(clippy::doc_markdown)]
/// RAYPAR T3: LPT-pre-balanced scoped-thread partition replaces rayon
/// par_iter work-stealing fan-out for the solve phase. Default ON; set
/// DEGENBOT_LPT_PARTITION=0 to fall back to rayon par_iter for A/B comparison.
/// The lab report shows LPT achieves 7.80/8 vs rayon 4.91/8.
/// (T4: the stance is an engine construction field set once from env —
/// never read at call time.)
fn lpt_partition_enabled() -> bool {
    LPT_PARTITION_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pre-solve profitability floor for the profit-envelope gate (SU7MAE).
/// Precedence: `DEGENBOT_MIN_PROFIT_WEI` (decimal wei) > default 0. Default 0
/// skips only paths whose rigorous upper bound proves zero-or-negative profit.
/// The full fee-aware derivation (`gas × base_fee_next + priority_fee`, the
/// same shape as degenbot-execution's assess rule) replaces this once live
/// numbers justify it — the solver API needs no change for that.
/// (T4: parsed once from env at engine construction — see the runtime
/// stance installer; the fn reads the static, never the environment.)
fn min_profit_floor() -> U256 {
    MIN_PROFIT_FLOOR_WEI.get().copied().unwrap_or(U256::ZERO)
}

static LPT_PARTITION_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);
static MIN_PROFIT_FLOOR_WEI: std::sync::OnceLock<U256> = std::sync::OnceLock::new();

/// Degenerate-path capture config parse (M6776W) — the owner side of the
/// `DEGENBOT_GATE_CAPTURE*` env family (the gate itself reads no env).
#[must_use]
fn gate_capture_from_env() -> Option<::degenbot_solvers::profit_envelope::GateCaptureCfg> {
    if std::env::var_os("DEGENBOT_GATE_CAPTURE").is_none() {
        return None;
    }
    let out_path = std::env::var("DEGENBOT_GATE_CAPTURE_OUT").map_or_else(
        |_| std::path::PathBuf::from("/tmp/gate_degenerate.jsonl"),
        std::path::PathBuf::from,
    );
    let max_paths = std::env::var("DEGENBOT_GATE_CAPTURE_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    Some(::degenbot_solvers::profit_envelope::GateCaptureCfg {
        out_path,
        max_paths,
    })
}

/// T4: the ONE env-parsing point for the engine's runtime stances — called
/// once at engine construction; hot paths read the parsed statics/config.
pub fn install_engine_env_stances() {
    LPT_PARTITION_ENABLED.store(
        std::env::var("DEGENBOT_LPT_PARTITION").map_or(true, |s| {
            s != "0" && !s.eq_ignore_ascii_case("false") && !s.eq_ignore_ascii_case("off")
        }),
        std::sync::atomic::Ordering::Relaxed,
    );
    let min_profit = std::env::var("DEGENBOT_MIN_PROFIT_WEI")
        .ok()
        .and_then(|s| s.parse::<U256>().ok())
        .unwrap_or(U256::ZERO);
    let _ = MIN_PROFIT_FLOOR_WEI.set(min_profit);
    let memo_on = std::env::var("DEGENBOT_SOLVER_WALK_MEMO").as_deref() == Ok("1");
    let memo_stats = std::env::var("DEGENBOT_SOLVER_WALK_MEMO_STATS").as_deref() == Ok("1");
    let projection_memo = match std::env::var("DEGENBOT_CL_PROJECTION_CACHE") {
        Ok(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "disabled"
        ),
        Err(_) => true,
    };
    crate::bot_core::resolve::install_projection_memo_stance(projection_memo);
    ::degenbot_solvers::runtime::set_runtime(::degenbot_solvers::runtime::SolveRuntimeConfig {
        event_solver_legacy: std::env::var("DEGENBOT_WALK_EVENT_SOLVER").as_deref() == Ok("0"),
        walk_event_census: std::env::var("DEGENBOT_WALK_EVENT_CENSUS").as_deref() == Ok("1"),
        anchor_sweep: match std::env::var("DEGENBOT_WALK_ANCHOR_SWEEP").as_deref() {
            Ok("0") => ::degenbot_solvers::runtime::AnchorSweep::Off,
            Ok("2") => ::degenbot_solvers::runtime::AnchorSweep::CenterOnly,
            _ => ::degenbot_solvers::runtime::AnchorSweep::Full,
        },
        max_tangent_lines: std::env::var("DEGENBOT_ENVELOPE_MAX_TANGENT_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32),
        sampled_compose_lines: std::env::var("DEGENBOT_ENVELOPE_SAMPLED_COMPOSE_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48),
        memo_on,
        memo_stats,
    });
}

/// K-slowest-path attribution record: (`time_us`, `pieces_visited`,
/// `path_sims`, `word_steps`, `refine_sims`, `gate_us`, `gate_derive_us`,
/// `gate_compose_us`, `gate_search_us`, `path_id`) — lets the completion
/// event name the cost driver of the slowest routes: gate-envelope bound
/// composition (with its derive/compose/search phase split) vs the walk
/// proper, not just wall time.
type PathTimeRecord = (u128, u64, u64, u64, u64, u64, u64, u64, u64, u64);
/// Min-heap (via `Reverse`) keeping only the K slowest paths in O(K) memory.
type PathTimesHeap = std::collections::BinaryHeap<std::cmp::Reverse<PathTimeRecord>>;

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

        // R522XA: the state machine decides which touched paths actually need a
        // (re)resolve. Solvable/Unresolved re-check on any hop dirty; an Invalid
        // path re-checks ONLY when a responsible pool goes dirty AND the
        // container empties (last faulty pool cleared). Unrelated co-hop dirt
        // leaves an Invalid path untouched — no 100k-path re-resolve churn.
        hotpath::measure_block!("arb_solve.dirty_status_scan", {
            for pool_key in v2_affected {
                if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V2, *pool_key)) {
                    for &path_id in path_ids {
                        if self
                            .path_status
                            .entry(path_id)
                            .or_default()
                            .on_pool_dirty((HopType::V2, *pool_key))
                        {
                            affected_path_ids.insert(path_id);
                        }
                    }
                }
            }
            for pool_key in v3_affected {
                if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V3, *pool_key)) {
                    for &path_id in path_ids {
                        if self
                            .path_status
                            .entry(path_id)
                            .or_default()
                            .on_pool_dirty((HopType::V3, *pool_key))
                        {
                            affected_path_ids.insert(path_id);
                        }
                    }
                }
            }
            for pool_key in v4_affected {
                if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V4, *pool_key)) {
                    for &path_id in path_ids {
                        if self
                            .path_status
                            .entry(path_id)
                            .or_default()
                            .on_pool_dirty((HopType::V4, *pool_key))
                        {
                            affected_path_ids.insert(path_id);
                        }
                    }
                }
            }
        });

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
        // Cross-block walk-composition census: advance the epoch BEFORE the
        // per-path probes so a path solved both this block and the previous
        // one reports a hit (the engine-owned WalkMemo handle, SU7MAE T3).
        self.walk_memo.begin_block(solve_block);
        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            self.results_block = solve_block;
            return;
        }

        // Telemetry: name EVERY path the dirty-pool fan-out just activated,
        // with its concrete hop list — a Jaeger trace now answers "which pools
        // are in this path" without cross-referencing Python state. Runs under
        // the drainer's `degenbot.arb.solve` span, so the events parent there.
        hotpath::measure_block!("arb_solve.fanout_activate_telemetry", {
            // Per-path activation events are debug-level now (N225ET): the
            // per-event span plumbing dominated the fan-out phase; the
            // diagnostic remains reachable via RUST_LOG degenbot::engine=debug.
            if tracing::enabled!(target: "degenbot::engine", tracing::Level::DEBUG) {
                for &path_id in &affected_path_ids {
                    tracing::debug!(
                        target: "degenbot::engine",
                        block_number = solve_block,
                        path.id = path_id,
                        path.hops = %self.describe_path_cached(path_id),
                        dirty.v2 = v2_affected.len(),
                        dirty.v3 = v3_affected.len(),
                        dirty.v4 = v4_affected.len(),
                        "[path] activated by dirty pool"
                    );
                }
            }
        });

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
        let mut invalid_reasons: HashMap<String, u64> = HashMap::new();
        self.paths_same_state_this_cycle = 0;
        hotpath::measure_block!("arb_solve.resolve", {
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
                // Reuse ceiling probe (epic RZRORC last leaf): compare the
                // hop update-block snapshot against the previous cycle's
                // recorded one. Byte-identical ⇒ the solve intake (every hop
                // state) is unchanged since the stored result was produced.
                let update_snapshot: Vec<u64> = path
                    .pools
                    .iter()
                    .map(|pool_ref| core.pool_update_block(pool_ref.pool_key))
                    .collect();
                let same_state = self
                    .resolved_update_snapshot
                    .get(&path_id)
                    .is_some_and(|prev| *prev == update_snapshot);
                self.resolved_update_snapshot
                    .insert(path_id, update_snapshot);
                if same_state {
                    self.paths_same_state_this_cycle += 1;
                }
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
                let deficits = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    &mut self.hop_projection_cache,
                    Some(&mut self.hop_projection_count),
                    self.cl_projection_memo,
                );
                for d in &deficits {
                    *invalid_reasons.entry(d.reason.to_string()).or_insert(0u64) += 1;
                    tracing::debug!(
                        %path_id,
                        hop_type = ?d.hop_type,
                        pool_key = d.pool_key,
                        reason = %d.reason,
                        "[resolve] path invalid at resolve"
                    );
                }
                self.path_resolved.insert(path_id, resolved);
                // R522XA: drive the path state machine from the full deficit set.
                self.path_status
                    .entry(path_id)
                    .or_default()
                    .set_resolved(&deficits);
            }
        });

        // Telemetry: resolve phase complete (core-lock window + hop re-derive).
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            paths.resolved = affected_path_ids.len(),
            paths.same_state = self.paths_same_state_this_cycle,
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
            .filter(|&&p| !deferred_paths.contains(&p))
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
        let path_times: parking_lot::Mutex<PathTimesHeap> =
            parking_lot::Mutex::new(std::collections::BinaryHeap::new());
        // Total CPU µs across all solved paths — dividing by the rayon wall
        // time yields achieved parallelism (8 workers ⇒ target ≈ 8.0).
        let solve_cpu_us: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Walk-combinatorial totals across the solve cycle (Σ pieces visited,
        // Σ path simulations) — the diagnostic multiplier behind a slow solve.
        let walk_pieces_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_sims_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_word_steps_total: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let walk_refine_sims_total: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        // J3OU5F follow-up (loop-7 S3GK3S): refine-phase split + envelope
        // phase splits, explicit counters — impl_type hotpath rows cannot be
        // trusted for skew-sensitive labels, so the finance event carries
        // first-class phase sums instead.
        let walk_ternary_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_grid_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Profit-envelope gate totals: ONE merged GateStats accumulator
        // (SU7MAE gate deepening — replaces the per-field atomic fan-out;
        // merge() is per-path, so this mutex is touched once per path).
        let gate_total: parking_lot::Mutex<::degenbot_solvers::profit_envelope::GateStats> =
            parking_lot::Mutex::new(Default::default());
        // Degenerate-path capture config (M6776W): env parsed ONCE per cycle
        // at the owner; the gate itself reads no environment. The prefix-
        // composition cache is generationed by the block epoch inside the
        // gate deps (no public reset to call anymore).
        let gate_capture = gate_capture_from_env();
        let mut gate_deps = ::degenbot_solvers::profit_envelope::GateDeps::per_block(
            solve_block,
            gate_capture.as_ref(),
        );
        gate_deps.walk_memo = Some(&self.walk_memo);
        // Optional offline CL-solver capture (DEGENBOT_SOLVER_CAPTURE=1): dump
        // the exact all-CL pool state the solver consumed for heavy paths so
        // the CL solver can be optimized offline. None (no-op) unless gated.
        let capture = HeavyClPathCapture::from_env();
        let capture_ref: Option<&HeavyClPathCapture> = capture.as_ref();
        // Optional mixed V2+CL solver capture (same gate): heavy
        // mixed paths (e.g. path 7042 V2→V3→V3) dispatch to
        // `exact_solve_mixed_path_n_cached`, which the all-CL capture skips.
        // Defaults OUT of the fixtures dir (loop-18: working rows never
        // accrete there; goldens are produced only by cl_capture_gen).
        let capture_mixed = HeavyMixedPathCapture::from_env();
        let capture_mixed_ref: Option<&HeavyMixedPathCapture> = capture_mixed.as_ref();
        let sims_recorder: &parking_lot::Mutex<HashMap<u64, u64>> = &self.last_walk_sims;
        let gate_recorder: &parking_lot::Mutex<HashMap<u64, u64>> = &self.last_gate_us;
        let solved: Vec<(u64, SolvePathResult)> = hotpath::measure_block!(
            "arb_solve.rayon_solve",
            {
                // Per-path solve + diagnostics closure (shared by the LPT
                // scoped-thread path and the rayon par_iter fallback).
                let solve_fn = |pid: u64,
                                resolved: &ResolvedMixedPath|
                 -> Option<(u64, SolvePathResult)> {
                    let _solve_ctx = solve_span.enter();
                    ::degenbot_solvers::profit_envelope::reset_gate_stats();
                    let t0 = std::time::Instant::now();
                    let outcome = ::degenbot_solvers::mixed::solve_path_with_min_profit(
                        resolved,
                        min_profit_floor(),
                        &gate_deps,
                    );
                    let micros = t0.elapsed().as_micros();
                    solve_cpu_us.fetch_add(
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
                    gate_total.lock().merge(&gs);
                    // Walk telemetry OUT the return path (SU7MAE T2): the
                    // outcome carries this path's counters — no TLS
                    // read-back. The Q3 dense one-shot alert is the
                    // CONSUMER's decision.
                    let outcome_stats = &outcome.stats;
                    if outcome_stats.max_dense_words
                        >= ::degenbot_solvers::mobius_v3_int::DENSE_OBSERVE_THRESHOLD
                        && !WALK_DENSE_ALERTED.swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        tracing::warn!(
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
                    sims_recorder
                        .lock()
                        .insert(pid, u64::try_from(sims).unwrap_or(0));
                    // Loop-18: record measured gate time for the LPT cost too
                    // (gate-heavy paths carry sims≈0 and were bin-packed cheap).
                    gate_recorder.lock().insert(pid, gate_us);
                    let (gate_derive_us, gate_compose_us, gate_search_us) = (
                        u64::try_from(gs.derive_ns / 1_000).unwrap_or(u64::MAX),
                        u64::try_from(gs.compose_ns / 1_000).unwrap_or(u64::MAX),
                        u64::try_from(gs.search_ns / 1_000).unwrap_or(u64::MAX),
                    );
                    walk_ternary_total.fetch_add(
                        u64::try_from(ternary_sims).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    walk_grid_total.fetch_add(
                        u64::try_from(grid_sims).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    walk_pieces_total.fetch_add(
                        u64::try_from(pieces).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    walk_sims_total.fetch_add(
                        u64::try_from(sims).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    walk_word_steps_total.fetch_add(
                        u64::try_from(word_steps).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    walk_refine_sims_total.fetch_add(
                        u64::try_from(refine_sims).unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    let mut heap = path_times.lock();
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
                    if let Some(cap) = capture_ref {
                        cap.maybe_capture(
                            pid,
                            solve_block,
                            u64::try_from(micros).unwrap_or(u64::MAX),
                            u64::try_from(sims).unwrap_or(0),
                            u64::try_from(pieces).unwrap_or(0),
                            outcome.result.as_ref(),
                            resolved,
                        );
                    }
                    if let Some(cap) = capture_mixed_ref {
                        cap.maybe_capture(
                            pid,
                            solve_block,
                            u64::try_from(micros).unwrap_or(u64::MAX),
                            u64::try_from(sims).unwrap_or(0),
                            u64::try_from(pieces).unwrap_or(0),
                            outcome.result.as_ref(),
                            resolved,
                        );
                    }
                    outcome.result.map(|r| (pid, r))
                };

                if lpt_partition_enabled() {
                    // RAYPAR T3: LPT-pre-balanced partition on rayon's persistent global
                    // pool. Each LPT bin is one s.spawn task — exactly n_threads
                    // tasks on n_threads persistent workers means no splitting
                    // and no stealing: pure static partition with warm L1/L2 +
                    // allocator arenas across drains (the pool was built once
                    // at import by configure_rayon_solver_pool).
                    let n_threads = rayon::current_num_threads().max(1);
                    // Loop-12 KUKHMX: previous-block measured walk sims refine
                    // the LPT cost; snapshot once (single lock) before binning.
                    // Loop-18: measured gate µs rides the same snapshot —
                    // gate-heavy paths (dense-CL compose) need the bins to
                    // know they are expensive despite sims≈0.
                    let last_sims_snapshot: HashMap<u64, u64> = sims_recorder.lock().clone();
                    let last_gate_snapshot: HashMap<u64, u64> = gate_recorder.lock().clone();
                    let costs: Vec<usize> = to_solve
                        .iter()
                        .map(|(pid, r)| {
                            sims_aware_cost(
                                path_cost_proxy(r),
                                last_sims_snapshot.get(pid).copied(),
                                last_gate_snapshot.get(pid).copied(),
                            )
                        })
                        .collect();
                    let bins = lpt_partition(to_solve.len(), n_threads, |i| costs[i]);
                    let to_solve_ref = &to_solve;
                    let solve_ref = &solve_fn;
                    let (tx, rx) = std::sync::mpsc::channel();
                    rayon::scope(|s| {
                        for bin in &bins {
                            let tx = tx.clone();
                            s.spawn(move |_| {
                                let mut out = Vec::with_capacity(bin.len());
                                for &i in bin {
                                    let (pid, resolved) = &to_solve_ref[i];
                                    out.push(solve_ref(*pid, resolved));
                                }
                                let _ = tx.send(out);
                            });
                        }
                    });
                    drop(tx);
                    let per_bin: Vec<Vec<Option<(u64, SolvePathResult)>>> =
                        rx.into_iter().collect();
                    per_bin
                        .into_iter()
                        .flatten()
                        .flatten()
                        .inspect(|(pid, r)| {
                            if !r.solver_pool_states.is_empty() {
                                tracing::debug!(
                                    "[solver-st] path_id={pid} hops=[{}]",
                                    r.solver_pool_states.join(";")
                                );
                            }
                        })
                        .filter(|(_, r)| !r.optimal_input.is_zero() && !r.profit.is_zero())
                        .collect()
                } else {
                    to_solve
                        .par_iter()
                        .filter_map(|(pid, resolved)| solve_fn(*pid, resolved))
                        .inspect(|(pid, r)| {
                            if !r.solver_pool_states.is_empty() {
                                tracing::debug!(
                                    "[solver-st] path_id={pid} hops=[{}]",
                                    r.solver_pool_states.join(";")
                                );
                            }
                        })
                        .filter(|(_, r)| !r.optimal_input.is_zero() && !r.profit.is_zero())
                        .collect()
                }
            }
        );
        if let Some(c) = capture.as_ref() {
            tracing::info!(
                target: "degenbot::solver",
                captured = c.count.load(std::sync::atomic::Ordering::Relaxed),
                out = %c.out_path.display(),
                "[solve-capture] heavy all-CL path capture active"
            );
        }

        // Telemetry: pure solver phase done — name the K slowest paths.
        let memo_stats = self.walk_memo.take_stats();
        let gate_tots = gate_total.into_inner();
        let slowest: Vec<String> = path_times.lock().iter()
            .map(
                |std::cmp::Reverse((
                    us,
                    pieces,
                    sims,
                    word_steps,
                    refine_sims,
                    gate_us,
                    gate_derive_us,
                    gate_compose_us,
                    gate_search_us,
                    pid,
                ))| {
                    format!(
                        "{pid}:{us}us:sims={sims}:pieces={pieces}:steps={word_steps}:refine={refine_sims}:gate={gate_us}us(g={gate_derive_us}/c={gate_compose_us}/s={gate_search_us})"
                    )
                },
            )
            .collect();
        tracing::info!(
            target: "degenbot::solver",
            block_number = solve_block,
            paths.solved = to_solve.len(),
            paths.invalid = invalid_count,
            solve.cpu_us = solve_cpu_us.load(std::sync::atomic::Ordering::Relaxed),
            walk.pieces = walk_pieces_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.sims = walk_sims_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.steps = walk_word_steps_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.refine_sims = walk_refine_sims_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.ternary = walk_ternary_total.load(std::sync::atomic::Ordering::Relaxed),
            walk.grid = walk_grid_total.load(std::sync::atomic::Ordering::Relaxed),
            gate.derive_us = u64::try_from(gate_tots.derive_ns / 1_000).unwrap_or(u64::MAX),
            gate.compose_us = u64::try_from(gate_tots.compose_ns / 1_000).unwrap_or(u64::MAX),
            gate.search_us = u64::try_from(gate_tots.search_ns / 1_000).unwrap_or(u64::MAX),
            gate.prefix_hits = gate_tots.prefix_hits,
            gate.boundaries_composed = gate_tots.boundaries_composed,
            gate.product_us = u64::try_from(gate_tots.product_ns / 1_000).unwrap_or(u64::MAX),
            gate.prune_stage1_us = u64::try_from(gate_tots.prune_stage1_ns / 1_000).unwrap_or(u64::MAX),
            gate.prune_hull_us = u64::try_from(gate_tots.prune_hull_ns / 1_000).unwrap_or(u64::MAX),
            gate.evaluated = gate_tots.evaluated,
            gate.skipped = gate_tots.skipped,
            gate.unsupported = gate_tots.unsupported,
            gate.none_hop_unmapped = gate_tots.none_hop_unmapped,
            gate.none_degenerate = gate_tots.none_degenerate,
            gate.none_overflow = gate_tots.none_overflow,
            gate.min_profit = %min_profit_floor(),
            profitable = solved.len(),
            slowest.paths = %slowest.join(","),
            phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            memo.probes = memo_stats.probes,
            memo.hits = memo_stats.hits,
            memo.distinct = memo_stats.distinct,
            memo.cache_plays = memo_stats.cache_plays,
            memo.negative = memo_stats.negative_entries,
            memo.sims = memo_stats.probes_sims,
            memo.hit_sims = memo_stats.hits_sims,
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
        hotpath::measure_block!("arb_solve.clamp_merge", {
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
                    path.hops = %self.describe_path_cached(pid),
                    "[path] profitable solve"
                );
                self.results.insert(pid, solve_result);
            }
        });

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

        // Pre-collect work items (path_id + cloned resolved). The clone
        // drops the immutable borrow on self.path_resolved so the LPT
        // scoped threads don't borrow &self during the parallel solve.
        let to_solve: Vec<(u64, ResolvedMixedPath)> = self
            .path_resolved
            .iter()
            .filter(|(_, r)| r.valid)
            .map(|(&pid, r)| (pid, r.clone()))
            .collect();

        // RAYPAR T3: LPT-pre-balanced partition on rayons persistent pool.
        // The cold-start path has the same cost skew as the hot path.
        let n_threads = rayon::current_num_threads().max(1);
        // Cold-start has no previous-block sims/gate yet: structural proxy only.
        let last_sims_snapshot: HashMap<u64, u64> = HashMap::new();
        let last_gate_snapshot: HashMap<u64, u64> = HashMap::new();
        let costs: Vec<usize> = to_solve
            .iter()
            .map(|(pid, r)| {
                sims_aware_cost(
                    path_cost_proxy(r),
                    last_sims_snapshot.get(pid).copied(),
                    last_gate_snapshot.get(pid).copied(),
                )
            })
            .collect();
        let bins = lpt_partition(to_solve.len(), n_threads, |i| costs[i]);
        let to_solve_ref = &to_solve;
        let solve_span_ref = &solve_span;
        let self_ref = &self;

        // Cold start: no capture wiring — deps with the registered-epoch
        // guard + the engine's walk-memo handle.
        let mut gate_deps =
            ::degenbot_solvers::profit_envelope::GateDeps::per_block(self.results_block, None);
        gate_deps.walk_memo = Some(&self.walk_memo);
        let (tx, rx) = std::sync::mpsc::channel();
        rayon::scope(|s| {
            for bin in &bins {
                let tx = tx.clone();
                s.spawn(move |_| {
                    let mut out = Vec::with_capacity(bin.len());
                    for &i in bin {
                        let (path_id, resolved) = &to_solve_ref[i];
                        let _solve_ctx = solve_span_ref.enter();
                        if let Some(mut r) = ::degenbot_solvers::mixed::solve_path_with_min_profit(
                            resolved,
                            min_profit_floor(),
                            &gate_deps,
                        )
                        .result
                        .filter(|r| !r.optimal_input.is_zero() && !r.profit.is_zero())
                        .inspect(|r| {
                            if !r.solver_pool_states.is_empty() {
                                tracing::debug!(
                                    "[solver-st] path_id={path_id} hops=[{}]",
                                    r.solver_pool_states.join(";")
                                );
                            }
                        }) {
                            self_ref.clamp_cl_hop_capacity(*path_id, &mut r);
                            out.push((*path_id, r));
                        }
                    }
                    let _ = tx.send(out);
                });
            }
        });
        drop(tx);
        rx.into_iter().flatten().collect()
    }
}

impl Default for ArbitrageEngine {
    fn default() -> Self {
        Self::new()
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
        std::env::var_os("DEGENBOT_SOLVER_CAPTURE")?;
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
            out_path: std::env::var("DEGENBOT_SOLVER_CAPTURE_OUT").map_or_else(
                |_| {
                    // Loop-18: production captures are WORKING rows (state and
                    // recorded answer come from different contexts) — they
                    // must NEVER accrete into the exact-wei fixtures: that
                    // accretion (513 null-golden rows, 9 stale epochs) was
                    // what red the F2 gate pre-re-anchor. Default out of the
                    // fixtures dir; exact-wei goldens are produced ONLY by
                    // cl_capture_gen (see its doc: the sanctioned producer).
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../logs/solver_capture/cl_heavy_paths.jsonl")
                },
                std::path::PathBuf::from,
            ),
            seen: std::sync::Mutex::new(std::collections::HashSet::new()),
            count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Append the CL pool state for a resolved heavy path, if it is a heavy,
    /// not-yet-captured all-CL path.
    // The capture record is a flat diagnostic tuple; a params struct would
    // obscure the field-for-field mapping to the JSONL schema.
    #[expect(clippy::too_many_arguments)]
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
        let Ok(mut seen) = self.seen.lock() else {
            return;
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
            .filter_map(|h| {
                // Every hop carries an int sequence (checked above), so this
                // never drops an element — the `?` just satisfies the type
                // checker without an `unwrap`.
                Some(
                    h.as_int_sequence()?
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
                                    .map(std::string::ToString::to_string)
                                    .collect::<Vec<_>>(),
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let golden_json = golden.map(|g| {
            serde_json::json!({
                "optimal_input": g.optimal_input.to_string(),
                "profit": g.profit.to_string(),
                "hop_outputs": g.hop_outputs.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
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
            let _ = writeln!(f, "{doc}");
        }
    }
}

/// One-shot capture of heavy *mixed* V2+CL solver inputs (the sibling of
/// [`HeavyClPathCapture`] for paths that dispatch to
/// `exact_solve_mixed_path_n_cached`). Records the V2 `IntHopState` per V2 hop
/// and the `IntV3TickRangeSequence` ranges per CL hop (plus `hop_order`) so
/// `examples/mixed_solve_replay.rs` can reconstruct the exact solver call,
/// assert golden determinism, and profile the bottleneck offline.
///
/// Gated by the same `DEGENBOT_SOLVER_CAPTURE=1` env. Writes to
/// `heavy_mixed_solve_captures.jsonl` (override via
/// `DEGENBOT_SOLVER_CAPTURE_OUT`). Captures only paths that mix ≥1 V2 and
/// ≥1 CL hop; all-CL and all-V2 paths are left to the existing captures.
struct HeavyMixedPathCapture {
    min_us: u64,
    min_sims: u64,
    max_captures: u64,
    out_path: std::path::PathBuf,
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
    count: std::sync::atomic::AtomicU64,
}

impl HeavyMixedPathCapture {
    fn from_env() -> Option<Self> {
        std::env::var_os("DEGENBOT_SOLVER_CAPTURE")?;
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
            out_path: std::env::var("DEGENBOT_SOLVER_CAPTURE_OUT").map_or_else(
                |_| {
                    // Loop-18: mixed captures default OUT of the fixtures dir
                    // (working rows; see the Cl-side comment).
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../logs/solver_capture/cl_mixed_paths.jsonl")
                },
                |p| {
                    // If the caller overrides the out path for both captures,
                    // disambiguate the mixed corpus into a sibling filename
                    // rather than overwriting the all-CL fixture.
                    let mut pb = std::path::PathBuf::from(p);
                    if pb.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        if let Some(stem) = pb.file_stem().and_then(|s| s.to_str()) {
                            pb.set_file_name(format!("{stem}_mixed.jsonl"));
                        }
                    }
                    pb
                },
            ),
            seen: std::sync::Mutex::new(std::collections::HashSet::new()),
            count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Append the mixed V2+CL solver input for a resolved heavy path, iff it
    /// is a mixed (≥1 V2 and ≥1 CL) not-yet-captured path.
    #[expect(clippy::too_many_arguments)]
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
        if resolved.hops.len() < 2 {
            return;
        }
        // Only mixed paths: ≥1 V2 hop AND ≥1 CL hop. The all-CL capture owns
        // pure-CL; all-V2 dispatches to the closed-form Möbius solver.
        let has_v2 = resolved
            .hops
            .iter()
            .any(|h| matches!(h, ResolvedHop::V2 { .. }));
        let has_cl = resolved.hops.iter().any(|h| h.as_int_sequence().is_some());
        if !has_v2 || !has_cl {
            return;
        }
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        if !seen.insert(pid) {
            return;
        }
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Per-hop serialization: a `kind` discriminant + the hop's raw fields.
        // V2 → reserve_in/out + gamma + fee_denom (decimal strings, no alloy
        // serde). CL → the same `IntV3TickRangeSequence.ranges` shape the
        // all-CL fixture uses, so the replay harness shares a CL-range parser.
        let hop_order: Vec<bool> = resolved
            .hops
            .iter()
            .map(|h| matches!(h, ResolvedHop::V2 { .. }))
            .collect();
        let hops = resolved
            .hops
            .iter()
            .map(|h| match h {
                ResolvedHop::V2 { state } => serde_json::json!({
                    "kind": "V2",
                    "reserve_in": state.reserve_in.to_string(),
                    "reserve_out": state.reserve_out.to_string(),
                    "gamma_numer": state.gamma_numer.to_string(),
                    "fee_denom": state.fee_denom.to_string(),
                }),
                ResolvedHop::V3 { int_seq, .. } | ResolvedHop::V4 { int_seq, .. } => {
                    serde_json::json!({
                        "kind": "CL",
                        "ranges": int_seq.ranges.iter().map(|r| serde_json::json!({
                            "liquidity": r.liquidity.to_string(),
                            "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                            "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                            "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                            "gamma_numer": r.gamma_numer,
                            "fee_denom": r.fee_denom,
                            "zero_for_one": r.zero_for_one,
                            "word_boundary_prices": r.word_boundary_prices
                                .iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    })
                }
                _ => serde_json::Value::Null,
            })
            .collect::<Vec<_>>();
        let golden_json = golden.map(|g| {
            serde_json::json!({
                "optimal_input": g.optimal_input.to_string(),
                "profit": g.profit.to_string(),
                "hop_outputs": g.hop_outputs.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
            })
        });
        let doc = serde_json::json!({
            "path_id": pid,
            "block": block,
            "n_hops": resolved.hops.len(),
            "hop_order": hop_order,
            "hops": hops,
            "measured": { "time_us": micros_us, "sims": sims, "pieces": pieces },
            "golden": golden_json,
        });
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.out_path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{doc}");
        }
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

#[cfg(test)]
mod lpt_partition_tests {
    use super::*;

    #[test]
    fn lpt_distributes_heavy_items_across_bins() {
        // Costs: [100, 100, 100, 1, 1, 1, 1, 1, 1, 1] — three heavy items
        // must go to three different bins (not clustered on one).
        let costs = [100, 100, 100, 1, 1, 1, 1, 1, 1, 1];
        let bins = lpt_partition(costs.len(), 3, |i| costs[i]);
        assert_eq!(bins.len(), 3);
        // Each bin should have exactly one heavy item.
        for bin in &bins {
            let heavy_count = bin.iter().filter(|&&i| costs[i] == 100).count();
            assert!(
                heavy_count <= 1,
                "bin has {heavy_count} heavy items, expected <= 1"
            );
        }
        // Total items preserved.
        let total: usize = bins.iter().map(Vec::len).sum();
        assert_eq!(total, costs.len());
    }

    #[test]
    fn lpt_empty_items_produces_empty_bins() {
        let bins = lpt_partition(0, 4, |_| 0);
        assert_eq!(bins.len(), 4);
        assert!(bins.iter().all(Vec::is_empty));
    }

    #[test]
    fn lpt_fewer_items_than_bins() {
        // 2 items, 8 bins — each item gets its own bin.
        let costs = [50, 30];
        let bins = lpt_partition(costs.len(), 8, |i| costs[i]);
        assert_eq!(bins.len(), 8);
        let non_empty: usize = bins.iter().filter(|b| !b.is_empty()).count();
        assert_eq!(non_empty, 2);
    }

    #[test]
    #[expect(clippy::unwrap_used)]
    fn lpt_balances_load() {
        // Costs: [10, 9, 8, 7, 6, 5, 4, 3, 2, 1] on 3 bins.
        // LPT assignment: 10→bin0(10), 9→bin1(9), 8→bin2(8), 7→bin1(16),
        // 6→bin2(14), 5→bin0(15), 4→bin2(18), 3→bin1(19), 2→bin0(17),
        // 1→bin0(18). Max load = 19, min load = 18. Well-balanced.
        let costs = [10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
        let bins = lpt_partition(costs.len(), 3, |i| costs[i]);
        let loads: Vec<usize> = bins
            .iter()
            .map(|b| b.iter().map(|&i| costs[i]).sum())
            .collect();
        let max_load = *loads.iter().max().unwrap();
        let min_load = *loads.iter().min().unwrap();
        // LPT guarantees max_load - min_load <= max_item_cost.
        assert!(
            max_load - min_load <= 10,
            "load spread {max_load}-{min_load}={spread} exceeds max_item",
            spread = max_load - min_load
        );
    }

    #[test]
    fn sims_aware_cost_prefers_measured_last_block_walk() {
        // No measured value → structural proxy governs.
        assert_eq!(sims_aware_cost(500, None, None), 500);
        // Measured below the proxy → proxy still governs (fresh pool state
        // can always cost at least the structural floor).
        assert_eq!(sims_aware_cost(500, Some(300), None), 500);
        // Measured above the proxy → measured wins (the last block's sims
        // predict the current block's cost better than structure alone).
        assert_eq!(sims_aware_cost(300, Some(900), None), 900);
        // Oversized measured values saturate to usize::MAX rather than wrap.
        assert_eq!(sims_aware_cost(1, Some(u64::MAX), None), usize::MAX);
        // Loop-18: gate-heavy paths (sims≈0, gate 14ms) now register real cost.
        assert_eq!(sims_aware_cost(1, Some(0), Some(14_000)), 14_000);
        // Sims + gate terms ADD (both µs-scale) before the proxy comparison.
        assert_eq!(sims_aware_cost(500, Some(300), Some(14_000)), 14_300);
    }

    #[test]
    fn lpt_partition_enabled_default_on() {
        // Default is ON (the env var is unset in test environments).
        assert!(lpt_partition_enabled());
    }

    #[test]
    fn lpt_zero_bins_returns_empty_vec() {
        let bins = lpt_partition(5, 0, |_| 1);
        assert!(bins.is_empty());
    }
}
