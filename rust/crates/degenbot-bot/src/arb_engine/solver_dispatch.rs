//! Path resolution, solver dispatch, and rebuild logic.

use alloy::primitives::{I256, U256};
use degenbot_core::diag;
use degenbot_core::{op_error, op_info, op_warn};

use ::degenbot_pools::v3_state::{v3_simulate_swap, V3PoolState};
use ::degenbot_pools::v4_state::v4_simulate_swap;

use super::{ArbitrageEngine, BlockMetadata, HashMap};

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

use crate::arb_engine::executor::{LaneOutcome, SolveLane, SolveOutcome};
use crate::arb_engine::inline_sim::{PendingSim, SimPoll, SimulatedPathResult};
use crate::bot_core::BotState;
use ::degenbot_solvers::mixed::{
    HopType, MixedPath, MixedPoolRef, ResolvedHop, ResolvedMixedPath, SolvePathResult,
};

/// How many slowest-path entries the solve-cycle completion event names
/// (D63GSE intra-solve visibility).
const SLOWEST_PATHS_K: usize = 5;

/// Q3 dense one-shot alert flag — the CONSUMER side of the moved alert: the
/// walk reports `WalkStats::max_dense_words`; this logs once per process.
static WALK_DENSE_ALERTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// FF-T1 (BPHR6F): one loud line for the sticky sim-fleet boot refusal — the
// materializer surfaces the typed Err on EVERY dispatch; the log rides a
// once-flag so a refused boot cannot spam the per-block cadence.
static SIM_BOOT_REFUSAL_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// THE one arm-attribution wiring site (cold-start trace): the cycle span is
/// tagged with `cycle.arm` (`detached` | `skipped_empty` | `shed`; `unset`
/// before any cycle). Pipeline-free by design: a consumer without the meter
/// installed is a no-op (pure-Rust/test seams).
///
/// ADR-045 T5: the caller drives it with `CycleOutcome::arm_label()` — the
/// cycle's duration/Mutex hold are observed a frame up, in `EngineStages`,
/// after `solve_dirty` returns, so the OUTCOME (not a post-hoc engine stash)
/// is the byte-stable source of the label.
#[must_use = "returns the label it recorded; callers may name the cycle arm with it"]
pub(crate) fn record_cycle_arm_telemetry(span: &tracing::Span, arm: &'static str) -> &'static str {
    span.record("cycle.arm", arm);
    // Handed back for the caller's per-cycle latch (see the doc above).
    arm
}

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

// ---------------------------------------------------------------------------
// RAYPAR T3: LPT-pre-balanced scoped-thread partition
// ---------------------------------------------------------------------------

/// The ONE solve-bin sizing seam (P6YXA6): fleet-hosted cycles bin at the
/// fleet's structural Solver seat count (pins == bins, so every bin owns a
/// warm keyed seat); every other arm bins at the machine-derived solve
/// worker count. One funnel so the arms can never re-derive the count and
/// drift apart (a hermetic fleet under machine-derived bins aborts at the
/// T2 grant — the host-only `just test-rust` failure).
pub(crate) fn solve_bin_count() -> usize {
    crate::arb_engine::executor::global_executor().bin_count()
}

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
pub(crate) fn lpt_partition(
    n_items: usize,
    n_bins: usize,
    cost: impl Fn(usize) -> usize,
) -> Vec<Vec<usize>> {
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
        // Measured per-path costs are unbounded (the stance fixtures pin
        // placement with near-u64::MAX walks, and the serial tier's ONE
        // solve seat accumulates EVERY item's cost into bin 0): the
        // accumulation must not overflow — saturate instead of panicking.
        loads[mi] = loads[mi].saturating_add(cost(i));
    }
    bins
}

/// The RUNTIME lane-capability decision (LW-T7, Seam F): the cycle either
/// runs at full structural width or takes the NAMED narrower fallback
/// (reth `state_root_task_timeout => sequential` lesson: the fallback is a
/// NAMED-AND-LOGGED decision, never a silent narrower bin mid-drain) — the
/// runtime twin of LW-T4's boot-time capacity floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CordonFallbackDecision {
    /// Structural seats cover the intended bins.
    FullCapacity,
    /// The hosting capability dropped (T9 resize under cordon): the cycle
    /// runs `running` bins (< `intended`) this block — logged at INFO.
    Narrower {
        /// The bins the workload intended.
        intended: usize,
        /// The bins the current capability hosts.
        running: usize,
    },
}

/// One cycle's planned bin fan-out (LW-T7, Seam F).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeatPlan {
    /// The bin count the cycle fans out over.
    pub bins: usize,
    /// The typed fallback decision this plan took.
    pub decision: CordonFallbackDecision,
}

/// Plan this cycle's bin fan-out: a capability narrower than the intended
/// fan-out takes the NAMED narrower fallback (typed + logged at INFO).
#[must_use]
pub(crate) fn plan_bins(intended_bins: usize, structural_seats: usize) -> SeatPlan {
    if structural_seats < intended_bins {
        op_info!(
            domain = solver,
            intended = intended_bins,
            running = structural_seats,
            "capability drop under cordon — running the NAMED \
             narrower fallback (LW-T7; the serial arm remains a downstream decision)"
        );
        SeatPlan {
            bins: structural_seats,
            decision: CordonFallbackDecision::Narrower {
                intended: intended_bins,
                running: structural_seats,
            },
        }
    } else {
        SeatPlan {
            bins: intended_bins,
            decision: CordonFallbackDecision::FullCapacity,
        }
    }
}

#[expect(clippy::doc_markdown)]
/// Resolve-time cost proxy for LPT binning: the total number of word-boundary
/// prices across all CL hops. Correlates with walk combinatorics without
/// requiring a solve, so it is available at to_solve collection time.
pub(crate) fn path_cost_proxy(resolved: &ResolvedMixedPath) -> usize {
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
pub(crate) fn sims_aware_cost(
    proxy: usize,
    last_sims: Option<u64>,
    last_gate_us: Option<u64>,
) -> usize {
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

/// 7LV6VN T2: chunked parallel resolve of the affected paths (sharded hop
/// cache preserves cross-path hit reuse). Default ON; set
/// `DEGENBOT_SOLVE_RESOLVE_PAR=0` for the serial A/B fallback.
pub(crate) const RESOLVE_CHUNK: usize = 256;
pub(crate) const RESOLVE_PAR_MIN: usize = 512;

pub(crate) struct ResolveChunkOut {
    pub(crate) resolved: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)>,
    pub(crate) status: Vec<(u64, Vec<crate::bot_core::resolve::HopDeficit>)>,
    pub(crate) snapshots: Vec<(u64, Vec<u64>)>,
    pub(crate) same_state: u64,
    pub(crate) projections: u64,
    pub(crate) invalid_reasons: HashMap<String, u64>,
    pub(crate) deferred: Vec<u64>,
}

/// Pre-solve profitability floor for the profit-envelope gate (SU7MAE).
/// Precedence: `DEGENBOT_MIN_PROFIT_WEI` (decimal wei) > default 0. Default 0
/// skips only paths whose rigorous upper bound proves zero-or-negative profit.
/// The full fee-aware derivation (`gas × base_fee_next + priority_fee`, the
/// same shape as degenbot-execution's assess rule) replaces this once live
/// numbers justify it — the solver API needs no change for that.
/// (T4: parsed once from env at engine construction — see the runtime
/// stance installer; the fn reads the static, never the environment.)
pub(crate) fn min_profit_floor() -> U256 {
    MIN_PROFIT_FLOOR_WEI.get().copied().unwrap_or(U256::ZERO)
}

static MIN_PROFIT_FLOOR_WEI: std::sync::OnceLock<U256> = std::sync::OnceLock::new();

/// T3 (epic BXUSGL): `DEGENBOT_STREAMING_DELIVERY` — emit each clamp-passed
/// above-threshold result as an immediate single-entry `ResultBatch` during the
/// solve drain instead of waiting for the pump debounce. Parsed ONCE at
/// engine construction ([`install_engine_env_stances`]); engines copy the
/// parsed static into their construction field.
///
/// **Default flipped ON by epic SRQEK5 T3 (SF3QLP):** with detached cycles the
/// streaming mode is the intended shipped behaviour — each clamp-passed result
/// arrives at Python the moment its own solve completes (per-path
/// micro-batches composed with the end-of-cycle debounce sweep, per the
/// V6TOMQ coarse proof: 1360 single-candidate batches / 0 errors / 10-min
/// mainnet). `DEGENBOT_STREAMING_DELIVERY=0` opts out to the debounce sweep
/// (A/B); any other value (or unset) streams.
pub(crate) static STREAMING_DELIVERY_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// The detached solve cycle (enqueue-and-return with sidecar merge) is THE
/// ONLY solve arm since the WFF6MM hard cutover: the DRIVEN solve path takes
/// NO engine-level Mutex — the stage-surface (`EngineStages`) solve hold
/// collapses to enqueue end (µs) and each result merges on the sidecar under
/// its own short per-item acquisition (the Q1a stale policy makes that safe).
/// The `DEGENBOT_DETACHED_SOLVES` stance (and its in-cycle opt-out) retired
/// with the in-cycle arm; backpressure is the admission draw, not the old
/// in-flight cap.
#[cfg(test)]
mod streaming_stance_tests {
    /// KAHU5W (presence-gated bools resolved): `pump.streaming_delivery` is
    /// now a plain schema bool; the env-parse policy matrix above is obsolete
    /// (the loader owns the words). The static default stays streaming.
    #[test]
    fn streaming_delivery_static_default_is_streaming() {
        assert!(super::STREAMING_DELIVERY_ENABLED.load(std::sync::atomic::Ordering::Relaxed,));
    }

    // WFF6MM: the detached-solve stance key retired from the schema; there
    // is no opt-out — the one solve arm is unconditional.
}

/// Degenerate-path capture config parse (M6776W) — the owner side of the
/// `capture` config section (the gate itself reads no env). KAHU5W:
/// `gate_capture` is a typed bool (the presence-gated
/// `DEGENBOT_GATE_CAPTURE` legacy is retired; `0`/false disables).
#[must_use]
pub(crate) fn gate_capture_from_cfg(
    cfg: &::degenbot_config::BotConfig,
) -> Option<::degenbot_solvers::profit_envelope::GateCaptureCfg> {
    cfg.capture
        .gate_capture
        .then(|| ::degenbot_solvers::profit_envelope::GateCaptureCfg {
            out_path: cfg.capture.gate_capture_out.clone(),
            max_paths: u64::try_from(cfg.capture.gate_capture_cap).unwrap_or(u64::MAX),
        })
}

/// KAHU5W: the solver crate's runtime stance is INSTANCE-SCOPED — built
/// fresh per engine from the typed config and passed down; no `OnceLock`.
#[must_use]
pub fn solve_runtime_config_from_cfg(
    cfg: &::degenbot_config::BotConfig,
) -> ::degenbot_solvers::runtime::SolveRuntimeConfig {
    ::degenbot_solvers::runtime::SolveRuntimeConfig {
        event_solver_legacy: cfg.solve.walk_event_solver_legacy,
        walk_event_census: cfg.solve.walk_event_census,
        anchor_sweep: match cfg.solve.walk_anchor_sweep {
            ::degenbot_config::AnchorSweep::Off => ::degenbot_solvers::runtime::AnchorSweep::Off,
            ::degenbot_config::AnchorSweep::CenterOnly => {
                ::degenbot_solvers::runtime::AnchorSweep::CenterOnly
            }
            ::degenbot_config::AnchorSweep::Full => ::degenbot_solvers::runtime::AnchorSweep::Full,
        },
        max_tangent_lines: cfg.solve.envelope_max_tangent_lines,
        sampled_compose_lines: cfg.solve.envelope_sampled_compose_lines,
        memo_on: cfg.solve.solver_walk_memo,
        memo_stats: cfg.solve.solver_walk_memo_stats,
    }
}

/// T4 (KAHU5W): the ONE config parse point for the engine's runtime stances —
/// called at engine construction with the typed `BotConfig`; hot paths read
/// the parsed statics. The crate performs ZERO environment reads: every stance
/// is a schema key (env or TOML loads into it via the degenbot-config loader).
/// The solver-runtime stance is NOT installed globally anymore — the engine
/// holds an instance value built by [`solve_runtime_config_from_cfg`] and
/// threads it down (KAHU5W: the solver `OnceLock` is retired).
///
/// YI5NGB: the boots installed here are the CONSTRUCTION-STAMPED values —
/// the engine derived its own `FleetBoot` from THIS caller cfg and stamped it
/// (`BootStamp`: engine id + deterministic cfg hash); the per-role fleet
/// executors courier the identified stamp to the single fleet
/// materialization, and a divergent-cfg rider is ledgered
/// (`boot_stamp::record_ride`) instead of silently winning the fleet.
pub fn install_engine_stances(
    cfg: &::degenbot_config::BotConfig,
    boot_stamp: &crate::arb_engine::boot_stamp::BootStamp,
) {
    // LW-T9 (no stance, no migration flag): the solve bins ALWAYS ride the
    // fleet-hosted executor; the typed boot descriptor (quota + overrides +
    // posture) is parsed here once.
    // YI5NGB: the engine's OWN construction boot, stamped — each role's
    // install records the identified ride (first-fleet-wins per role).
    crate::arb_engine::fleet_solve_executor::install_boot(boot_stamp.clone());
    // candidate 4 (YUMQU3): the two POOLED roles install through the ONE
    // registry. Sim installs BEFORE registration, so the registry's
    // first-wins canonical process boot is sim's (same descriptor value as
    // registration's — the boot is shared). ADR-042 F4: the SimDriver seat
    // pool shares the boot descriptor; PRG-3: registration shares it too
    // (duty-counted PoolStateUpdater slots, Deferrable cordon class).
    let registry = crate::arb_engine::seat_host::FleetBootRegistry::process();
    registry.install_boot(
        crate::arb_engine::boot_stamp::BootRole::Sim,
        boot_stamp.clone(),
    );
    registry.install_boot(
        crate::arb_engine::boot_stamp::BootRole::Registration,
        boot_stamp.clone(),
    );
    STREAMING_DELIVERY_ENABLED.store(
        cfg.pump.streaming_delivery,
        std::sync::atomic::Ordering::Relaxed,
    );
    // J4HN66: streaming/detached stances are per-engine cfg values now
    // (packed at construction); this install keeps only the statics that
    // still have non-construction consumers (STREAMING; INLINE_SIM).
    INLINE_SIM_ENABLED.store(
        cfg.solve.solve_inline_sim,
        std::sync::atomic::Ordering::Relaxed,
    );
    let min_profit = U256::from(cfg.solve.min_profit_wei);
    let _ = MIN_PROFIT_FLOOR_WEI.set(min_profit);
    crate::bot_core::resolve::install_projection_memo_stance(cfg.solve.cl_projection_cache);
    // 7LV6VN T2 (YI5NGB): the chunked parallel resolve stance is an ENGINE
    // instance value now — packed per construction from
    // cfg.solve.solve_resolve_par (the KAHU5W construction-stance
    // trajectory); no installer store remains here.
}

/// K-slowest-path attribution record: (`time_us`, `pieces_visited`,
/// `path_sims`, `word_steps`, `refine_sims`, `gate_us`, `gate_derive_us`,
/// `gate_compose_us`, `gate_search_us`, `path_id`) — lets the completion
/// event name the cost driver of the slowest routes: gate-envelope bound
/// composition (with its derive/compose/search phase split) vs the walk
/// proper, not just wall time.
type PathTimeRecord = (u128, u64, u64, u64, u64, u64, u64, u64, u64, u64);
/// Min-heap (via `Reverse`) keeping only the K slowest paths in O(K) memory.
pub(crate) type PathTimesHeap = std::collections::BinaryHeap<std::cmp::Reverse<PathTimeRecord>>;

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

/// `DEGENBOT_SOLVE_INLINE_SIM` (SIMPIPE2 T2 → T4, task PIRX3W / AK7VJB):
/// relocate the CL-hop clamp from the engine-Mutex merge site INTO the
/// per-path solve worker, so the worker can simulate on the clamp-committed
/// inputs without an engine-lock round-trip (the M1 seam T1/T3 build on).
/// Parsed ONCE at engine construction.
///
/// **Default ON since the T4 mainnet soak** (2026-09-05): payload counts
/// matched solved paths per cycle, ~99% of sim batches skipped the FFI
/// dispatch, header→first-payload-render p50 1ms / p90 31ms (vs the option-A
/// FFI pipeline's ~26ms solve wall + 49ms async sim tail), and the 46-minute
/// soak ran with zero deadlocks/panics/storage-key incidents through a
/// 200k-path registration flood. `DEGENBOT_SOLVE_INLINE_SIM=0`/`false`
/// opts OUT (restores the legacy merge-site clamp for a run); unset keeps
/// the inline stance. Later 0.7 hardening may remove the env entirely.
pub(crate) static INLINE_SIM_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

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
fn clamp_result_in_worker(
    ctx: &SolveCycleShared,
    idx: usize,
    pid: u64,
    result: &mut SolvePathResult,
) -> u64 {
    if !ctx.worker_clamp || idx >= ctx.pool_refs.len() {
        return 0;
    }
    let core = ctx.core.read();
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

/// One scheduled sim: pid + the receipt the worker polls/joins.
#[derive(Default)]
struct PipelinedSims {
    pending: Vec<(u64, PendingSim)>,
}

impl PipelinedSims {
    fn schedule_one(
        &mut self,
        ctx: &SolveCycleShared,
        idx: usize,
        pid: u64,
        result: &SolvePathResult,
        parent_span: &tracing::Span,
    ) -> bool {
        // No hook / clamp stance off: no sim can ever land, so the caller
        // must flush the item immediately (payload None) — otherwise the
        // held item would wait on a receipt that never exists.
        if !ctx.worker_clamp || idx >= ctx.pool_refs.len() {
            return false;
        }
        let Some(sim) = ctx.inline_sim.as_ref() else {
            return false;
        };
        // 7LV6VN T1b: EXPLICIT parent at creation (TLS re-entry alone forked
        // orphan roots on worker threads). The span is created and entered
        // ON THE DRIVER THREAD (std thread context = no inherited span),
        // mirroring the legacy `inline_sim_payload` worker span byte for
        // byte so Jaeger nesting and the verdict records are unchanged:
        // the span stays open until the sim completes instead of closing
        // when the bin's synchronous call returns.
        let request = crate::arb_engine::inline_sim::InlineSimRequest {
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
        };
        let sim = std::sync::Arc::clone(sim);
        let parent = parent_span.clone();
        let expected_profit = result.profit;
        // Two-runtime pacing (7LV6VN T5): the slot is acquired INSIDE the
        // driver thread, so a saturated sim pipeline parks queued sims at
        // zero CPU cost instead of stalling the bins mid-walk (T5 window:
        // schedule-time blocking starved the walks). Concurrent EXECUTING
        // sims stay bounded by the budget-derived cap - the explicit
        // The sim EXECUTION body is stance-invariant: one span
        // (`degenbot.bundle.simulate`, explicitly parented under the
        // caller's span — 7LV6VN T1b), one `simulate_path` call, the
        // SIMSPANDUP verdict records, and the receipt send. The arms differ
        // ONLY in the machinery that runs it.
        let (tx, rx) = std::sync::mpsc::channel();
        let run_sim_body = move || {
            let span = tracing::info_span!(
                target: "degenbot::solver",
                parent: parent,
                "degenbot.bundle.simulate",
                sim.path = "worker_inline",
                path_id = request.path_id,
                sim_block = request.sim_block,
                simulate.verdict = tracing::field::Empty,
                simulate.expected_profit = tracing::field::Empty,
                // SIMSPANDUP: declared so the seam-reused span keeps the
                // ADR-040 error classification on the inline arm too.
                simulate.error_reason = tracing::field::Empty,
            );
            let _enter = span.enter();
            let payload = sim.simulate_path(request);
            // SIMSPANDUP: as in the sync arm - the seam's SimSpanVerdict
            // Drop stamps the failure verdict (`not_profitable`/`error` +
            // error_reason) before the payload returns; not clobbering it
            // keeps the richer classification. A `None` payload = hook
            // miss (no sim ran), so honestly no verdict stamp at all.
            if payload.as_ref().is_some_and(|p| p.failure.is_none()) {
                span.record("simulate.verdict", "profitable");
            }
            span.record(
                "simulate.expected_profit",
                tracing::field::display(expected_profit),
            );
            let _ = tx.send(payload);
        };
        // ADR-042 F4 (LW-T9, sole posture): the fleet is the sole executor
        // of inline sims — the request rides a pooled SimDriver unit
        // (dispatch lane 2: queued sims drain before new Solver intake;
        // cordon floors the sim intake and never cancels in-flight sims).
        // The seat pool is the budget's sim slot cap — the fleet-side bound
        // that replaced the SimSlots semaphore. Receipts ride the SAME
        // per-request channel, so the poll/join contract is untouched.
        // ADR-042 F4 (LW-T9): submit through the pooled-executor seam — arb_engine
        // hosts TWO executor traits since LNQDOA: Executor (solve, bin-indexed)
        // and FleetIntake (pooled sim/intake, fire-and-dispatch). Pooled SimDriver
        // unit, lane-2 dispatch precedence; receipts stay on the caller's
        // per-request channel (unchanged contract).
        // FF-T1 (BPHR6F): a refused fleet boot surfaces the TYPED, sticky
        // BootError here — never a process abort, and never a submit into a
        // pipe that will not be drained. The walker's existing “no sim can
        // ever land” arm (the same one a missing hook/clamp takes above)
        // flushes the item immediately with a None payload: the outcome
        // ledger counts it, the caller never parks. The refusal logs ONCE
        // per process — every later dispatch re-derives the same sticky Err
        // from the materializer without spamming the per-block cadence.
        match crate::arb_engine::executor::global_sim_executor() {
            Ok(intake) => intake.spawn(Box::new(run_sim_body)),
            Err(err) => {
                if !SIM_BOOT_REFUSAL_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    op_error!(domain = solver, error = %err,
                        "sim dispatch skipped — the fleet boot was refused (typed, FF-T1); the item flushes un-simulated (None payload)"
                    );
                }
                return false;
            }
        }
        self.pending.push((pid, PendingSim::new(rx)));
        true
    }

    /// Non-blocking sweep: hand back every sim that finished while the bin
    /// kept walking. Each pid surfaces exactly once.
    fn drain_ready(
        &mut self,
    ) -> Vec<(
        u64,
        Option<crate::arb_engine::inline_sim::SimulatedPathResult>,
    )> {
        let mut ready = Vec::new();
        let mut still = Vec::with_capacity(self.pending.len());
        for (pid, ps) in self.pending.drain(..) {
            match ps.try_result() {
                SimPoll::Ready(payload) => ready.push((pid, payload.map(|b| *b))),
                SimPoll::InFlight => still.push((pid, ps)),
            }
        }
        self.pending = still;
        ready
    }

    /// Bin-tail join: block for every outstanding sim. Order preserved.
    fn join_all(
        self,
    ) -> impl Iterator<
        Item = (
            u64,
            Option<crate::arb_engine::inline_sim::SimulatedPathResult>,
        ),
    > {
        self.pending.into_iter().map(|(pid, p)| (pid, p.result()))
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Stamp the sim payload onto the held Solved outcome and submit it —
/// ONE flush shape for BOTH solve arms (7LV6VN T5 carry; unified by
/// QR3NUS 43E3H3). The arms differ only in the `submit` closure:
/// the detached arm's closure sends on the merge pipe AND bumps its
/// in-flight gauge at SEND success (a bin that dies before sending
/// never leaks a count); the in-cycle arm's closure is `lane.solved`.
fn flush_solved_item(
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

/// Per-cycle shared solve context (epic BXUSGL T1): everything the
/// per-path dispatch touches besides the resolved snapshot. Bundled once
/// per cycle so a worker handle is static for the dedicated-executor
/// arm; the caller retains its own Arc for the drain + tail telemetry.
pub(crate) struct SolveCycleShared {
    pub(crate) solve_block: u64,
    pub(crate) epoch: u64,
    pub(crate) gate_capture: Option<::degenbot_solvers::profit_envelope::GateCaptureCfg>,
    pub(crate) walk_memo: std::sync::Arc<::degenbot_solvers::mobius_v3_int::WalkMemo>,
    /// KAHU5W: the instance-scoped solver runtime stance, threaded down —
    /// the solver crate has no process-global config anymore.
    pub(crate) runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig,
    pub(crate) capture: Option<std::sync::Arc<HeavyClPathCapture>>,
    pub(crate) capture_mixed: Option<std::sync::Arc<HeavyMixedPathCapture>>,
    pub(crate) path_times: parking_lot::Mutex<PathTimesHeap>,
    pub(crate) gate_total: parking_lot::Mutex<::degenbot_solvers::profit_envelope::GateStats>,
    pub(crate) solve_cpu_us: std::sync::atomic::AtomicU64,
    pub(crate) walk_pieces_total: std::sync::atomic::AtomicU64,
    pub(crate) walk_sims_total: std::sync::atomic::AtomicU64,
    pub(crate) walk_word_steps_total: std::sync::atomic::AtomicU64,
    pub(crate) walk_refine_sims_total: std::sync::atomic::AtomicU64,
    pub(crate) walk_ternary_total: std::sync::atomic::AtomicU64,
    pub(crate) walk_grid_total: std::sync::atomic::AtomicU64,
    /// Engine-owned per-path measured-sims recorder (Arc-d engine field).
    pub(crate) sims_recorder: std::sync::Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// Engine-owned per-path gate-us recorder (Arc-d engine field).
    pub(crate) gate_recorder: std::sync::Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// Test-only deterministic per-path delay hook (epic test knob).
    #[cfg(test)]
    pub(crate) test_solve_delay: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
    /// 43E3H3 red-first: test-only per-path PANIC hook — a bin body that
    /// dies mid-walk so the breaker suite can pin the detached arm's
    /// witness (typed Failed records) and gauge pairing through a panic.
    #[cfg(test)]
    pub(crate) test_solve_panic: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
    /// SIMPIPE2 T2: the shared core (Arc-cloned from the engine at cycle
    /// build) — the WORKER-side clamp takes the same short core read the
    /// merge-site clamp took; no engine state is touched (MQUKB6-T3 intact:
    /// engine-then-core ordering, short read, no guard across awaits).
    pub(crate) core: std::sync::Arc<crate::bot_core::state_lock::StateLock<BotState>>,
    /// Per-path pool-ref snapshot, ALIGNED TO `to_solve` ORDER (index i in
    /// every bin mirrors `to_solve[i]`): the worker clamp's pool list, taken
    /// under the cycle's engine Mutex (stable for the whole cycle).
    pub(crate) pool_refs: Vec<std::sync::Arc<MixedPath>>,
    /// The cycle's block metadata (Copy) — the inline-sim request's block env
    /// (solve block from `solve_block`; timestamp/base-fee from here).
    pub(crate) metadata: BlockMetadata,
    /// SIMPIPE2 T2 worker-side clamp gate (construction-time pack).
    pub(crate) worker_clamp: bool,
    /// SIMPIPE2 T3: the engine's inline-sim hook snapshot. `Some` + stance ON
    /// → the worker resolves the per-path payload right after the clamp (no
    /// engine lock — the same off-lock seam the worker clamp opened).
    pub(crate) inline_sim:
        Option<std::sync::Arc<dyn crate::arb_engine::inline_sim::InlineSimulator>>,
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
// Contract 3 — single lock context (WFF6MM): the only drain caller left is
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
/// stays on the lane itself — `SolveLane::set_on_solved_send`, contract 1's
/// send-success-only bump.
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

// (the drain body + its impl block moved to `SolveCycle::drain_lane_outcomes`, ADR-045 T4)

// P37YJG: the drain's counter aggregate moved with the disposition
// counters into the machine — detached_cycle::LaneDrainCounts.

impl ArbitrageEngine {
    /// THE ONE LANE WALK (WNH5OL): the shared body of the two former `run_bin`
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
                    // A None IS an outcome (QR3NUS): exactly one
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
pub(crate) struct HeavyClPathCapture {
    min_us: u64,
    min_sims: u64,
    max_captures: u64,
    out_path: std::path::PathBuf,
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
    count: std::sync::atomic::AtomicU64,
}

impl HeavyClPathCapture {
    pub(crate) fn from_capture(capture: &::degenbot_config::schema::CaptureConfig) -> Option<Self> {
        capture.solver_capture.then_some(()).map(|()| Self {
            min_us: capture.solver_capture_min_us,
            min_sims: capture.solver_capture_min_sims,
            max_captures: u64::try_from(capture.solver_capture_cap).unwrap_or(u64::MAX),
            out_path: match capture.solver_capture_out.clone() {
                Some(p) => p,
                None => {
                    // Loop-18: production captures are WORKING rows (state and
                    // recorded answer come from different contexts) — they
                    // must NEVER accrete into the exact-wei fixtures: that
                    // accretion (513 null-golden rows, 9 stale epochs) was
                    // what red the F2 gate pre-re-anchor. Default out of the
                    // fixtures dir; exact-wei goldens are produced ONLY by
                    // cl_capture_gen (see its doc: the sanctioned producer).
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../logs/solver_capture/cl_heavy_paths.jsonl")
                }
            },
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
        if let Some(parent) = self.out_path.parent() {
            // The default OUT path lives under logs/solver_capture/ — a
            // directory that only exists if someone created it. A missing
            // parent previously failed every append SILENTLY (the in-process
            // `captured` counter kept advancing), losing the whole run's
            // corpus.
            let _ = std::fs::create_dir_all(parent);
        }
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
pub(crate) struct HeavyMixedPathCapture {
    min_us: u64,
    min_sims: u64,
    max_captures: u64,
    out_path: std::path::PathBuf,
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
    count: std::sync::atomic::AtomicU64,
}

impl HeavyMixedPathCapture {
    pub(crate) fn from_capture(capture: &::degenbot_config::schema::CaptureConfig) -> Option<Self> {
        capture.solver_capture.then_some(()).map(|()| Self {
            min_us: capture.solver_capture_min_us,
            min_sims: capture.solver_capture_min_sims,
            max_captures: u64::try_from(capture.solver_capture_cap).unwrap_or(u64::MAX),
            out_path: match capture.solver_capture_out.clone() {
                Some(p) => {
                    // If the caller overrides the out path for both captures,
                    // disambiguate the mixed corpus into a sibling filename
                    // rather than overwriting the all-CL fixture.
                    let mut pb = p;
                    if pb.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        if let Some(stem) = pb.file_stem().and_then(|s| s.to_str()) {
                            pb.set_file_name(format!("{stem}_mixed.jsonl"));
                        }
                    }
                    pb
                }
                None => {
                    // Loop-18: mixed captures default OUT of the fixtures dir
                    // (working rows; see the Cl-side comment).
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../logs/solver_capture/cl_mixed_paths.jsonl")
                }
            },
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
    use super::clamp_result_in_worker;
    use super::{
        ArbitrageEngine, BlockMetadata, HashMap, PathTimesHeap, SolveCycleShared, SolvePathResult,
        U256,
    };
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

#[cfg(test)]
mod lpt_partition_tests {
    use super::*;

    // ---- LW-T7 (Seam F): determinism, runtime fallback, promotion gate ----

    /// LW-T7 (Seam F): LPT is bit-stable — the same input & cost fn yields
    /// IDENTICAL bins across 50 invocations at widely varying shape, and
    /// equal-cost ties resolve by the FIXED rule (original index order) —
    /// the expected bins are computed by an independent reading of the
    /// documented rule (stable sort desc by cost with index-order ties,
    /// then item-by-item onto the first minimal-load bin).
    #[test]
    fn lpt_partition_is_bit_stable_across_invocations_and_ties_are_index_ordered() {
        let costs = vec![40, 40, 40, 70, 70, 30, 30, 30, 30, 55];
        let n = costs.len();
        // Independent reading of the documented rule (order-stable, ties by
        // ascending original index; min-load bin tie by lowest bin index).
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by_key(|&i| (std::cmp::Reverse(costs[i]), i));
        let mut loads = [0usize; 3];
        let expected: Vec<Vec<usize>> = {
            let mut bins: Vec<Vec<usize>> = vec![Vec::new(); 3];
            for i in idx {
                // Closed 3-bin range: the Option is statically Some.
                #[expect(
                    clippy::expect_used,
                    reason = "the 3-bin range is statically non-empty"
                )]
                let mi = (0..3)
                    .min_by_key(|&bi| (loads[bi], bi))
                    .expect("closed 3-bin range is non-empty");
                bins[mi].push(i);
                loads[mi] += costs[i];
            }
            bins
        };
        for invocation in 0..50 {
            let bins = lpt_partition(n, 3, |i| costs[i]);
            assert_eq!(
                bins, expected,
                "invocation {invocation}: bins deviate from the documented rule"
            );
        }
    }

    /// LW-T7 (Seam F): a seat-capacity drop under a cordon drives a NAMED
    /// typed runtime fallback decision (typed enum, logged at INFO) —
    /// never silent narrower bins mid-drain (the runtime twin of LW-T4's
    /// boot-time capacity floor).
    #[test]
    fn seat_drop_under_cordon_drives_a_named_typed_runtime_fallback() {
        // Narrower capability: the plan NAMES the drop (intended → running).
        let plan = plan_bins(6, 4);
        assert_eq!(plan.bins, 4);
        assert_eq!(
            plan.decision,
            CordonFallbackDecision::Narrower {
                intended: 6,
                running: 4,
            },
            "the fallback must be a NAMED typed decision"
        );
        // Full capability: no fallback, full width.
        let full = plan_bins(6, 6);
        assert_eq!(full.decision, CordonFallbackDecision::FullCapacity);
        assert_eq!(full.bins, 6);
    }

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
    fn lpt_zero_bins_returns_empty_vec() {
        let bins = lpt_partition(5, 0, |_| 1);
        assert!(bins.is_empty());
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

    /// ADR-043 §8 behavioral volume gate (ergo ZJUEXH): one fixture solve
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
        let items = super::executor_ab_probe::load_corpus_fixture();
        let ctx = super::executor_ab_probe::probe_ctx();
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

        let ctx = super::executor_ab_probe::probe_ctx();
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

    use super::{lpt_partition, path_cost_proxy, BotState, PathTimesHeap, SolveCycleShared};
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
    use super::{PathTimesHeap, PipelinedSims, SolveCycleShared};
    use crate::arb_engine::inline_sim::{
        AccessListRow, CapturedSwapRow, InlineSimFailure, InlineSimRequest, InlineSimulator,
        InlineSwapFamily, SimulatedPathResult,
    };
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

#[cfg(test)]
mod dispatch_binning_properties {
    //! JXCAR4 (epic 64ZQLA): solver dispatch binning properties over
    //! ARBITRARY item counts, cost shapes and seat shapes. The spawn-seam
    //! invariant (`FleetSolveExecutor::spawn` aborts on a bin >= the
    //! structural seat count, commit `ccc148275`) must never be the
    //! discovery mechanism again: a future binning regression shows up here
    //! as a shrunk counterexample, not a host-only SIGABRT.
    use super::executor_ab_probe::prod_lpt_bins;
    use super::lpt_partition;
    use degenbot_solvers::mixed::ResolvedMixedPath;
    use proptest::prelude::*;
    use std::sync::Arc;

    /// Synthesized fixture paths (empty hops = zero structural cost; the
    /// properties exercise the BINDER, not the solver).
    fn synth_items(n: usize) -> Vec<Arc<ResolvedMixedPath>> {
        (0..n)
            .map(|_| {
                Arc::new(ResolvedMixedPath {
                    hops: Vec::new(),
                    valid: true,
                    state_nonces: Vec::new(),
                    max_update_block: 0,
                })
            })
            .collect()
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]
        #[test]
        fn lpt_partition_preserves_items_and_stays_enclosed(
            n_items in 0usize..=2048usize,
            n_bins in 1usize..=64usize,
            costs in proptest::collection::vec(0usize..=97usize, 0..=2048),
        ) {
            let cost = |i: usize| costs.get(i).copied().unwrap_or(0);
            let bins = lpt_partition(n_items, n_bins, cost);
            // Enclosure: exactly the requested bin shape, indices inside.
            prop_assert_eq!(bins.len(), n_bins);
            for bin in &bins {
                for &i in bin {
                    prop_assert!(i < n_items);
                }
            }
            // Preservation: every item index appears exactly once.
            let mut seen: Vec<usize> = bins.iter().flatten().copied().collect();
            seen.sort_unstable();
            prop_assert_eq!(seen.len(), n_items);
            for (want, got) in seen.iter().enumerate() {
                prop_assert_eq!(*got, want);
            }
        }

        #[test]
        #[expect(
            clippy::cast_precision_loss,
            reason = "quota units (1e6 scale, <= 6.4e7) are exact in f64"
        )]
        fn prod_bins_never_exceed_the_structural_seats(
            quota_units in 6_000_000u64..=64_000_000u64,
            n_items in 0usize..=1024usize,
        ) {
            // Hostable quotas only (floor >= the pinned-role floor of 6);
            // the seat count comes from the REAL budget table keyed to the
            // quota property, never from this machine's shape.
            let q = (quota_units as f64) / 1_000_000.0;
            let seats = match degenbot_workers::budget::FleetBudget::derive(
                q,
                &degenbot_workers::budget::BudgetOverrides::default(),
            ) {
                Ok(b) => b.solver_pin_count,
                Err(err) => {
                    return Err(TestCaseError::fail(format!(
                        "hostable quota refused: {err:?}"
                    )));
                }
            };
            let items = synth_items(n_items);
            let bins = prod_lpt_bins(&items, seats);
            // Binding at the fleet's own seat count: the spawn normalize
            // (validate_bin_index) can never fire.
            prop_assert_eq!(bins.len(), seats);
            for (bin_idx, bin) in bins.iter().enumerate() {
                prop_assert!(bin_idx < seats);
                for &i in bin {
                    prop_assert!(i < n_items);
                }
            }
            // Item preservation across the seats.
            let total: usize = bins.iter().map(Vec::len).sum();
            prop_assert_eq!(total, n_items);
        }
    }
}
