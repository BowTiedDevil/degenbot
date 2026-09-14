//! The solve cycle as a deep module (ADR-045, ergo task `E7V2S6`).
//!
//! ## Scope (this file)
//!
//! **T1** pinned the data-type seam of ADR-045: the `CycleOutcome` /
//! `CycleArm` / `ResolveCensus` / `Registration` vocabulary, with
//! `CycleArm::label()` reproducing the ADR-043 `cycle.arm` labels
//! `"shed" | "skipped_empty" | "detached"` byte-for-byte.
//!
//! **T3** (`ANVHXW`) assembled [`SolveCycle`] — the resolve companions, the
//! cycle-transient stash, the solve output, the admissions stance, the
//! detached-arm collaborator, the walk/projection recorders, and the seven
//! `for_test` knobs. The engine holds it as `engine.cycle`; the pinned
//! `*_for_test` engine setters delegate into it.
//!
//! **T4** (`OUJAJR`) landed the interface and moved its bodies onto
//! [`SolveCycle`]: `draw` / `run_epoch` / `register_path` /
//! `register_and_solve_path` / `solve_all_paths` / `merge_detached_item` /
//! `forget`. The T1 vocabulary is consumed by that surface.
//!
//! **T5** (`DI4GJQ`) hard-cut the string stashes: [`run_epoch`] returns the
//! typed [`CycleOutcome`], the `cycle_arm` / `solve_entry` string fields are
//! deleted, and the stage hook records `cycle.arm` off the outcome. The
//! `CycleEntry` enum existed only to feed the deleted `solve_entry`, so it is
//! gone with it.
//!
//! ## Test ledger
//!
//! * **Green pins** (the ADR-043 vocabulary and the target type shape):
//!   `cycle_arm_labels_are_byte_stable`,
//!   `cycle_arm_is_dispatch_selects_detached_arms`,
//!   `cycle_outcome_exposes_block_and_arm_label`,
//!   `registration_encodes_created_and_resolved`,
//!   `pending_new_path_result_survives_one_dirty_cycle`.
//! * **Flipped at T4**: `dedup_hit_does_not_touch_pending_new_carry` is now
//!   the positive contract — a dedup hit yields
//!   `Registration { created: false, .. }` and the carry write in
//!   `register_and_solve_path` is gated on `Registration.created`, so the
//!   pending-new carry is untouched. The two formerly-ignored sketches are
//!   live: `run_epoch_consumes_the_epoch_work_carried_delta` (the cycle
//!   consumes the epoch's work-carried delta — one owner, no second
//!   swappable handle) and
//!   `register_and_solve_path_reports_registration_created`.
//!
//! There is no module-level `dead_code` expectation: the T1 vocabulary is
//! consumed by the T5 surface. The remaining per-item expectation covers the
//! test-only `CycleArm::is_dispatch` helper.

use std::sync::Arc;

use dashmap::DashMap;
use hashbrown::{HashMap, HashSet};

use ::degenbot_solvers::mixed::{
    HopType, MixedPath, MixedPoolRef, PoolHop, ResolvedMixedPath, SolvePathResult,
};

use super::block_cursor::BlockCursor;
use super::delivery_policy::DeliveryPolicy;
use super::detached_cycle::DetachedCycle;
use super::inline_sim::SimulatedPathResult;
use super::lane_walk::{drive_lane_walk, LaneArmPolicy, LaneWalkBinPlan, WalkSubmitCtx};
use super::path_info::describe_hop;
use super::path_lifecycle::PathSolveStatus;
use super::path_registry::{PathRegistration, PathRegistrationError, PathRegistry};
use super::solver_capture::{gate_capture_from_cfg, CaptureVariant, HeavyPathCapture};
use super::workload_partition::{
    lpt_partition, path_cost_proxy, plan_bins, sims_aware_cost, solve_bin_count,
};
use super::DeferredReRecordHook;
use crate::arb_engine::detached_cycle::{self, DetachedArm, LaneDrainCounts};
use crate::arb_engine::executor::{run_solve_lane, LaneOutcome, SolveLane, SolveOutcome};
use crate::arb_engine::fleet_solve_executor::SOLVE_BIN_KEY_BASE;
use crate::bot_core::resolve::resolve_hops;
use crate::bot_core::resolve::HopProjectionCache;
use crate::bot_core::{BlockMetadata, BotState, EpochDelta};
use alloy::primitives::{I256, U256};
use degenbot_core::diag;
use degenbot_core::{op_error, op_info};
use degenbot_pools::v3_state::{v3_simulate_swap, V3PoolState};
use degenbot_pools::v4_state::v4_simulate_swap;
use degenbot_solvers::affected_keys::AffectedKey;
use degenbot_workers::dispatcher::SeatSurvivesPolicy;
use degenbot_workers::lane::LaneCtx;
use std::sync::PoisonError;

// ---------------------------------------------------------------------------
// 5WCRWZ T5: statics/fns from the retired grab file, each
// moved beside its sole production consumer (or its near consumers).
// ---------------------------------------------------------------------------

/// 7LV6VN T2: chunked parallel resolve of the affected paths (sharded hop
/// cache preserves cross-path hit reuse). Default ON; set
/// `DEGENBOT_SOLVE_RESOLVE_PAR=0` for the serial A/B fallback.
const RESOLVE_CHUNK: usize = 256;
const RESOLVE_PAR_MIN: usize = 512;

struct ResolveChunkOut {
    resolved: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)>,
    status: Vec<(u64, Vec<crate::bot_core::resolve::HopDeficit>)>,
    snapshots: Vec<(u64, Vec<u64>)>,
    same_state: u64,
    projections: u64,
    invalid_reasons: HashMap<String, u64>,
    deferred: Vec<u64>,
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

pub(crate) static MIN_PROFIT_FLOOR_WEI: std::sync::OnceLock<U256> = std::sync::OnceLock::new();

/// Min-heap (via `Reverse`) keeping only the K slowest paths in O(K) memory.
/// The record tuple now lives with the walk (`arb_engine::lane_walk`,
/// 5WCRWZ T4); T5 retires the heap itself.
pub(crate) type PathTimesHeap =
    std::collections::BinaryHeap<std::cmp::Reverse<super::lane_walk::PathTimeRecord>>;

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
    pub(crate) capture: Option<std::sync::Arc<HeavyPathCapture>>,
    pub(crate) capture_mixed: Option<std::sync::Arc<HeavyPathCapture>>,
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

/// The solve cycle's owned state (ADR-045, ergo task `ANVHXW`).
///
/// The resolve companions, the cycle-transient stash, the solve output,
/// the admissions stance, the detached-arm collaborator, the
/// walk/projection recorders, and the seven `for_test` knobs. The T4
/// interface (`draw` / `run_epoch` / `register_path` /
/// `register_and_solve_path` / `solve_all_paths` / `merge_detached_item` /
/// `forget`) now lands its bodies here.
///
/// The engine holds this as `engine.cycle`; the white-box tests re-index
/// mechanically to `engine.cycle.*` and the pinned `*_for_test` engine
/// setters delegate into these fields (ADR-041: knob names never change at
/// the call site).
#[expect(clippy::struct_excessive_bools)] // construction stances (projection memo, admission, streaming, resolve-par) + the draw verdict — distinct stances, not flag soup
pub(crate) struct SolveCycle {
    // --- Resolve companions -------------------------------------------
    /// Resolved path states (mutated on each solve). Entries are Arc-shared
    /// into the parallel solve dispatch (f701ccd3 staging fix) — immutable
    /// between resolve passes, so staging is refcount bumps, not deep clones
    /// of the CL tick-range sequences.
    pub(crate) path_resolved: HashMap<u64, Arc<ResolvedMixedPath>>,
    /// Path solve-eligibility state machine (R522XA): per registered path the
    /// [`PathSolveStatus`] that decides whether a dirty-pool fan-out must
    /// (re)resolve it. Replaces the scattered `valid` bool + ad-hoc skip rules.
    pub(crate) path_status: HashMap<u64, PathSolveStatus>,
    /// Per-path snapshot of every hop's `pool_update_block` at the last
    /// successful resolve. A byte-identical snapshot on the next cycle means
    /// the whole solve intake (all hop states) is unchanged (epic RZRORC last
    /// leaf).
    pub(crate) resolved_update_snapshot: HashMap<u64, Vec<u64>>,
    /// Hop-projection memo (pool,direction) -> snapshot@nonce. Shared across
    /// all resolve call sites so a dirty pool's tick walk runs once per state
    /// change and serves every referencing path from the cache.
    pub(crate) hop_projection_cache: HopProjectionCache,
    /// Monotonic count of actual family projections (cache misses). Test-
    /// observable; emitted on the solve-phase resolve event.
    pub(crate) hop_projection_count: u64,
    /// Fused hop-projection memo switch (KGXFT7 winner promotion): resolved
    /// ONCE at construction.
    pub(crate) cl_projection_memo: bool,
    /// Telemetry string cache: path id -> formatted hop description, built
    /// once at first emission (paths are immutable after registration).
    pub(crate) path_description_cache: parking_lot::Mutex<HashMap<u64, Arc<str>>>,

    // --- Cycle-transient stash ----------------------------------------
    /// 6XB6NJ: the ONE engine block cursor — the consolidated owner of the
    /// block-coordinate residue. Every advance rule lives on the cursor.
    pub(crate) cursor: BlockCursor,
    /// QTZGFL: the DRAW's consumption verdict for the cycle currently between
    /// `on_resolve` and the dispatch — `true` when the admission draw's
    /// budget was zero (the cycle SHEDS).
    pub(crate) admission_draw_zero: bool,
    /// The typed latch of the most recent cycle's arm (ADR-045 T5). The
    /// string stash is gone: [`CycleOutcome`] is authoritative and the stage
    /// hook records `cycle.arm` straight off it. This latch survives only so
    /// the white-box probes and the latency histograms can name the arm AFTER
    /// the cycle returns. `None` = no cycle dispatched yet (`"unset"`).
    pub(crate) last_arm: Option<CycleArm>,
    /// Paths registered via `register_and_solve_path` that have been eagerly
    /// solved and appended to `results`, awaiting the next dirty-cycle merge.
    pub(crate) pending_new_paths: HashSet<u64>,
    /// Reuse-eligibility counter for the current solve cycle (probe only;
    /// reset each `solve_dirty` and surfaced on the resolve event).
    pub(crate) paths_same_state_this_cycle: u64,

    // --- Solve output -------------------------------------------------
    /// Last solved results, keyed by path ID for O(1) updates.
    ///
    /// RAYPAR engine-shard T1 (C42WKO): sharded into a `DashMap` so Python
    /// `latest_results` reads never park behind the drain-lock held engine
    /// `Mutex`.
    pub(crate) results: DashMap<u64, SolvePathResult>,
    /// SIMPIPE2 T3: the per-path inline payloads resolved in the solve
    /// workers (SIMPIPE2 T2's off-lock seam). Keyed by path id.
    pub(crate) inline_payloads: DashMap<u64, SimulatedPathResult>,

    // --- Admissions stance --------------------------------------------
    /// QTZGFL: construction-time admission stance (`DEGENBOT_SOLVE_ADMISSION`,
    /// default OFF for the experiment).
    pub(crate) solve_admission: bool,
    /// QTZGFL: the un-merged-result pipe depth target in KEYS, clamped at
    /// construction to `1..=detached_cycle::DETACHED_INFLIGHT_CAP`.
    pub(crate) admission_target_depth: u64,
    /// QTZGFL: the retained (carried) key retention window W in blocks.
    pub(crate) admission_retention_blocks: u64,

    // --- Collaborators / recorders ------------------------------------
    /// THE one solve-arm machine (P37YJG): the per-cycle states, the merge
    /// pipe, the gauge pair, the seq counters, the ledger door, and the
    /// disposition counters. See [`DetachedCycle`].
    pub(crate) detached_cycle: DetachedCycle,
    /// The engine-owned cross-block walk-composition memo (SU7MAE T3, Q12a):
    /// passed into the solve entries by handle; epoch advances at the
    /// block-lifecycle start.
    pub(crate) walk_memo: Arc<::degenbot_solvers::mobius_v3_int::WalkMemo>,
    /// Per-path previous-block MEASURED walk sims (recorded by `solve_fn`
    /// after each solve; lock-free-read at bin construction). Refines the
    /// LPT makespan predictor for stable pool shapes (loop-12 KUKHMX).
    pub(crate) last_walk_sims: Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// Per-path previous-block MEASURED gate time (µs, recorded by `solve_fn`;
    /// lock-free-read at bin construction). Loop-18.
    pub(crate) last_gate_us: Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// T3 (epic BXUSGL): emit each clamp-passed above-threshold result as an
    /// IMMEDIATE single-entry [`ResultBatch`] during the drain instead of
    /// waiting for the pump debounce. Construction-time stance.
    pub(crate) streaming_delivery: bool,
    /// KJWIK5: the deferred-path re-record hook (the ledger carry for
    /// `paths.deferred_future_price` deferrals).
    pub(crate) deferred_re_record: Option<DeferredReRecordHook>,
    /// KAHU5W: the chunked-parallel resolve stance as an instance value.
    pub(crate) resolve_par_stance: bool,

    // --- Shared dependencies (ADR-045 T4: the cycle drives resolve/solve) ---
    /// The shared `BotState` handle (a clone of the engine's Arc; ADR-006 D1).
    pub(crate) core: Arc<crate::bot_core::state_lock::StateLock<crate::bot_core::BotState>>,
    /// KAHU5W config + the instance solver runtime stance (immutable after
    /// construction), and the inline-sim hook (kept in sync by
    /// `set_inline_simulator`).
    pub(crate) cfg: Arc<::degenbot_config::BotConfig>,
    pub(crate) runtime_cfg: ::degenbot_solvers::runtime::SolveRuntimeConfig,
    pub(crate) inline_sim: Option<Arc<dyn super::inline_sim::InlineSimulator>>,

    // --- for_test knobs (ADR-041: names pinned on the engine) ---------
    /// Test-only: hook invoked at the start of each path solve.
    #[cfg(test)]
    pub(crate) test_solve_delay: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// 43E3H3 red-first: test-only per-path PANIC hook.
    #[cfg(test)]
    pub(crate) test_solve_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// KJWIK5 test seam: force the future-price deferral for these path ids.
    #[cfg(test)]
    pub(crate) test_force_deferred: Option<HashSet<u64>>,
    /// Test-only: the drain appends each merged path id here.
    #[cfg(test)]
    pub(crate) merge_probe: Option<Arc<parking_lot::Mutex<Vec<u64>>>>,
    /// AQV6EF red-first: test-only per-path PANIC hook for the MERGE seat.
    #[cfg(test)]
    pub(crate) test_merge_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// WFF6MM test harness: when ON (default), a DIRECT
    /// `rebuild_and_solve_affected` / `solve_dirty` call merges its own
    /// just-enqueued detached pipe INLINE.
    #[cfg(test)]
    pub(crate) test_sync_merge: bool,
    /// WFF6MM test harness: the inline drain's cached handle on the merge pipe.
    #[cfg(test)]
    pub(crate) test_merge_rx:
        Option<std::sync::mpsc::Receiver<crate::arb_engine::executor::LaneOutcome>>,
}

/// The per-cycle resolve census — the submission/resolve counts the stage
/// hooks and telemetry read off a [`CycleOutcome`] (ADR-045).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolveCensus {
    /// Affected keys the cycle drew (the epoch's work-carried delta size).
    pub(crate) affected: u64,
    /// Paths that (re)resolved to a fresh state this cycle.
    pub(crate) resolved: u64,
    /// Paths whose hop-state snapshot was byte-identical to the stored one.
    pub(crate) same_state: u64,
    /// Actual family projections performed (cache misses) this cycle.
    pub(crate) projections: u64,
}

/// The typed cycle arm (ADR-045) — replaces the engine's string `cycle_arm`
/// stash. [`CycleArm::label`] reproduces the ADR-043 `cycle.arm` vocabulary
/// byte-for-byte; [`CycleArm::is_dispatch`] says whether the arm reached the
/// detached solve dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CycleArm {
    /// Draw-time zero budget: nothing submitted, no `begin_cycle`, no seq
    /// tick; the cursor still advances and the keys stay in the ledger for a
    /// later cycle (carry).
    Shed {
        /// Keys left in the ledger by the zero-budget draw.
        keys_affected: usize,
        /// Outstanding detached solves at the shed verdict.
        in_flight: u64,
        /// The admission target depth at the shed verdict.
        target_depth: u64,
    },
    /// No affected paths (and no pending registration carry): a
    /// bookkeeping-only pass that advances the cursor.
    SkippedEmpty {
        /// The (empty) keys-affected count, kept for symmetry.
        keys_affected: usize,
    },
    /// The detached solve arm actually enqueued work.
    Solved {
        /// The detached-cycle sequence number (`begin_cycle`).
        seq: u64,
        /// LPT bin count the enqueue used.
        bins: usize,
        /// Paths staged for solve.
        staged: usize,
        /// Paths skipped as invalid at staging.
        invalid: usize,
        /// Paths deferred because their price clock ran ahead of the anchor.
        deferred_future_price: usize,
    },
    /// The detached arm ran but dissolved to no solve work (every staged path
    /// was invalid or deferred) — still a dispatch cycle.
    Dissolved {
        /// Paths skipped as invalid at staging.
        invalid: usize,
        /// Paths deferred because their price clock ran ahead of the anchor.
        deferred_future_price: usize,
    },
}

impl CycleArm {
    /// The ADR-043 `cycle.arm` label, byte-for-byte.
    #[must_use]
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Shed { .. } => "shed",
            Self::SkippedEmpty { .. } => "skipped_empty",
            Self::Solved { .. } | Self::Dissolved { .. } => "detached",
        }
    }

    /// Whether this arm reached the detached solve dispatch. The shed and
    /// skipped-empty arms are non-dispatch (no `begin_cycle`, no submission);
    /// both detached arms are.
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) const fn is_dispatch(&self) -> bool {
        matches!(self, Self::Solved { .. } | Self::Dissolved { .. })
    }
}

/// The typed fact a solve cycle returns (ADR-045): the solved-block
/// coordinate, the resolve census, and the typed arm. Stage hooks read this
/// instead of poking the engine's string stash.
#[derive(Debug, Clone)]
pub(crate) struct CycleOutcome {
    /// The block the cycle anchored to (the monotone cursor's solved block).
    pub(crate) solved_block: u64,
    /// The cycle's resolve census.
    pub(crate) census: ResolveCensus,
    /// The typed cycle arm.
    pub(crate) arm: CycleArm,
}

impl CycleOutcome {
    /// The solved-block coordinate.
    #[must_use]
    pub(crate) const fn solved_block(&self) -> u64 {
        self.solved_block
    }

    /// The ADR-043 arm label (delegates to [`CycleArm::label`]).
    #[must_use]
    pub(crate) const fn arm_label(&self) -> &'static str {
        self.arm.label()
    }
}

/// The typed registration result (ADR-045) — a dedup hit and a fresh register
/// are typable facts, not `pending_new_paths` timing.
#[derive(Debug, Clone)]
pub(crate) struct Registration {
    /// The registered (or dedup-matched) path id.
    pub(crate) path_id: u64,
    /// `true` for a fresh registration; `false` for a dedup hit.
    pub(crate) created: bool,
    /// The freshly-resolved snapshot for a fresh registration; `None` on a
    /// dedup hit (the existing snapshot is reused).
    pub(crate) resolved: Option<Arc<ResolvedMixedPath>>,
}

// ===========================================================================
// 5WCRWZ T7: the CL-hop clamp and its profit recompute, moved off the
// deleted `ArbitrageEngine` twins onto their real owner. `SolveCycle::
// clamp_cl_hop_capacity` and the worker-side `lane_walk::clamp_result_in_
// worker` both drive the same `clamp_result_with_state`; there is exactly
// one clamp body (ADR-046 "no inherent twins").
// ===========================================================================
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
    let margin = cl_hop_clamp_margin();
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
/// lock) so it is directly unit-testable independent of the CL-twin
/// machinery.
#[must_use]
fn recompute_clamped_profit(result: &SolvePathResult) -> Option<U256> {
    let final_output = result.hop_outputs.last().copied()?;
    let first_consumed = result.consumed_inputs.first().copied()?;
    Some(final_output.saturating_sub(first_consumed))
}

// ===========================================================================
// ADR-045 T4: the moved solve-cycle behavior. Bodies moved verbatim from
// the retired grab file / `lifecycle.rs`; only the receiver (`self` is the
// cycle) and the identity/delivery borrows changed. Lock discipline, span
// names and event names are byte-identical.
// ===========================================================================
impl SolveCycle {
    #[cfg(test)]
    pub(crate) fn admission_budget_keys(&self) -> Option<usize> {
        if !self.solve_admission {
            return None;
        }
        let outstanding = self
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed);
        usize::try_from(self.admission_target_depth.saturating_sub(outstanding)).ok()
    }
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn merge_one_result(
        &mut self,
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
            self.clamp_cl_hop_capacity(pid, &mut result, registry)
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
                path.hops = %self.describe_path_cached(pid, registry),
                "profitable solve"
            );
        });
        // SIMPIPE2 T3: the inline-sim payload — store it so the delivery
        // diff ships it with the batch (`None` = the path re-solved without a
        // payload this cycle — stance off or hook failure — so any stale
        // entry MUST drop). RKXN5Z/IJUBV3: the merge emits the terminal
        // verdict as an EVENT on the enclosing merge span (both arms hold
        // `degenbot.arb.merge` here; the detached sidecar re-enters the solve
        // span). The former `degenbot.bundle.simulate` marker span collided
        // with the real EVM-sim spans of the same name and flooded every
        // block trace with 90-300 microsecond lookalikes. The span name now
        // belongs to simulation work only (worker seam + the FFI seam in
        // degenbot-arbitrage/simulator.rs).
        if let Some(p) = &payload {
            let verdict = if p.failure.is_some() {
                "not_profitable"
            } else {
                "profitable"
            };
            // DEBUG-gated (log-volume cut OPBD7L): this event duplicates the
            // `[path] profitable solve` event above (same path.id/profit) and
            // the Python-side `[sim]` summary; the settle verdict is also
            // observable as an event on the enclosing `degenbot.arb.merge`
            // OTel span. Re-enable with `RUST_LOG=degenbot_bot=debug`.
            diag!(domain = solver, { path.id = pid, verdict, expected_profit = %result.profit, sim.seam = "inline_payload_store" },
                "inline payload settle"
            );
        }
        hotpath::measure_block!("merge.payload_store", {
            match payload {
                Some(p) => {
                    self.inline_payloads.insert(pid, p);
                }
                None => {
                    self.inline_payloads.remove(&pid);
                }
            }
        });
        // T3 (epic BXUSGL): DEGENBOT_STREAMING_DELIVERY - each above-threshold
        // merged result is emitted IMMEDIATELY (before the slowest path can
        // possibly delay it). The per-entry emission composes with the
        // debounce sweep, which still owns expired/removed + the end-of-cycle
        // metadata batch.
        let payload_now = self.inline_payloads.get(&pid).map(|e| e.value().clone());
        if self.streaming_delivery {
            hotpath::measure_block!("merge.delivery_emit", {
                delivery.emit_single_result_batch(
                    solve_block,
                    metadata,
                    pid,
                    &result,
                    payload_now.as_ref(),
                );
            });
        }
        self.results.insert(pid, result);
        #[cfg(test)]
        if let Some(probe) = &self.merge_probe {
            probe.lock().push(pid);
        }
        twins
    }
    pub(crate) fn merge_detached_item(
        &mut self,
        item: LaneOutcome,
        registry: &PathRegistry,
        delivery: &mut DeliveryPolicy,
    ) {
        // AQV6EF red-first: test-only per-path panic hook — kill this merge
        // mid-item so the sidecar's catch_unwind guard + caught-panic
        // disposition (sticky cordon) can be pinned.
        #[cfg(test)]
        if let Some(panic_pid) = self.test_merge_panic.as_ref() {
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
            self.drain_lane_outcomes(
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
        // P37YJG: the machine owns the pair — this is the RECEIPT half
        // (the decrement + both meter publishes ride with it,
        // byte-identical).
        self.detached_cycle.solved_received();
        // MQUKB6-T2: re-enter the enqueue-time cycle span for the
        // whole merge (Q1a drop/apply events + any profit emit
        // parent there). Inert without a subscriber or for
        // `Span::none()` test items.
        let merge_span = solved.solve_span.clone();
        let _merge_ctx = merge_span.enter();
        let age_cycles = self
            .detached_cycle
            .issued_seq()
            .saturating_sub(solved.cycle_seq);
        // Q1a deregister: nothing to merge into — drop, never
        // re-create.
        let Some(registered) = registry.get(solved.pid) else {
            self.detached_cycle
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
            let core = self
                .core
                .read_at(crate::bot_core::state_lock::LockSite::Solver);
            registered
                .pools
                .iter()
                .map(|pool_ref| core.pool_update_block(pool_ref.pool_key))
                .collect()
        };
        if live_stamp != solved.update_stamp {
            self.detached_cycle
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
        // QR3NUS exactness fuse carried to the DETACHED sidecar
        // (LW-T9 note (a) carry; now THE ONE ledger, 43E3H3): the
        // in-cycle drain claims the same engine-side ledger keyed
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
        // WNH5OL: THE claim itself is the drain's Solved arm below
        // (one claim per outcome — the table's role). This envelope
        // owns the CLAIM'S ORDER: the Q1a gates above run BEFORE the
        // drain, so a Q1a-dropped straggler still never reaches the
        // claim and its flags stay fireable.
        // THE DRAIN (WNH5OL): claim + merge for the Q1a-fresh
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
        self.drain_lane_outcomes(
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
            self.detached_cycle
                .disposition(detached_cycle::Disposition::Applied);
            diag!(
                domain = solver,
                path_id = log_pid,
                detached_seq = log_seq,
                detached_age_cycles = age_cycles,
                "straggler merged (unchanged intake)"
            );
        }
        // ADR-021 publish-verifier scoping retired (task 2UVG3E): the
        // solver-state verifier (and its publish change set) is gone
        // — merges apply the Q1a stale policy only.
    }
    fn drain_lane_outcomes(
        &mut self,
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
                    // P37YJG: the claim runs through the machine's one
                    // ledger door (the KEY policy — the seq half — is
                    // machine-issued); a refused duplicate lands on the
                    // machine's fuse counter.
                    if self.detached_cycle.claim((policy.ledger_seq, pid)).is_err() {
                        self.detached_cycle
                            .disposition(detached_cycle::Disposition::Duplicate);
                        op_error!(
                            domain = solver,
                            path_id = pid,
                            ledger_seq = policy.ledger_seq,
                            "duplicate lane outcome for path — exactness fuse tripped (QR3NUS)"
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
                    self.merge_one_result(
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
                    self.detached_cycle
                        .disposition(detached_cycle::Disposition::DroppedDeregistered);
                    op_error!(domain = solver, path_id = pid,
                        failure = ?failure,
                        "path outcome lost to a seat panic — typed failure record (QR3NUS)"
                    );
                    counts.failed += 1;
                }
            }
        }
    }
    pub(crate) fn clamp_cl_hop_capacity(
        &self,
        path_id: u64,
        result: &mut SolvePathResult,
        registry: &PathRegistry,
    ) -> u64 {
        let Some(path) = registry.get(path_id) else {
            return 0; // Unknown path → nothing to clamp
        };
        let core = self
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        clamp_result_with_state(&core, path_id, &path.pools, result)
    }
    #[expect(clippy::too_many_lines)]
    pub(crate) fn run_epoch(
        &mut self,
        affected: &[degenbot_solvers::affected_keys::AffectedKey],
        block_number: u64,
        metadata: &BlockMetadata,
        registry: &PathRegistry,
        delivery: &mut DeliveryPolicy,
    ) -> CycleOutcome {
        let _ = &mut *delivery;
        let mut same_state_total;
        let mut projections_total;
        let solved_bins;
        // MQUKB6-T0: worker threads (executor bins, resolve chunk threads,
        // solve executor's workers, AND the detached solve-bin std-threads)
        // have no ambient tracing context — any span emitted inside a
        // dispatch closure would orphan into a root trace. Capture the
        // caller's span (the drainer's `degenbot.arb.solve`) once and
        // re-enter it per work item; the detached arm additionally carries
        // it into the detached carrier so the sidecar's merges inherit it
        // (MQUKB6-T2).
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
            // LXDY4C: the affected keys ARE the delta's taken (HopType,
            // pool_id) reverse-index keys — one loop, no per-family intake.
            for key in affected {
                if let Some(path_ids) = registry.paths_for(&key.path_index_key()) {
                    for &path_id in path_ids {
                        if self
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

        // Solve-block anchor (rule owner + history: `crate::bot_core::solve_anchor`):
        // the batch's `solve_block` (= `results_block`) is the block the pool
        // state actually reflects — the pool-state head, NOT the
        // (possibly-lagging) drain `block_number`. Since BO5FBS the pump
        // pre-promotes `active_block` before calling `on_drain`, so
        // `block_number` here is already >= the head and the re-anchor is a
        // defensive no-op on the pump path — it stays the guard for callers
        // that bypass the pump (e.g. tests driving `solve_dirty` directly).
        let anchor = crate::bot_core::solve_anchor::SolveAnchor::resolve(
            block_number,
            &self
                .core
                .read_at(crate::bot_core::state_lock::LockSite::Solver),
        );
        let solve_block = anchor.block();
        // Cross-block walk-composition census: advance the epoch BEFORE the
        // per-path probes so a path solved both this block and the previous
        // one reports a hit (the engine-owned WalkMemo handle, SU7MAE T3).
        self.walk_memo.begin_block(solve_block);
        // -----------------------------------------------------------------
        // ADMISSION DRAW (QTZGFL): the DRAW already made the SINGLE
        // consumption decision in `on_resolve`. Consume and clear its verdict
        // HERE — the dispatch NEVER re-reads the live gauge (two reads could
        // disagree, and a fresh zero would discard keys the draw already
        // removed from the ledger). A zero-budget draw consumed nothing, so a
        // shed is genuine: this cycle submits NOTHING. The response lives at
        // the draw/cycle level, never the in-cycle dispatch. The machine is
        // NOT consulted for a shed cycle (no `begin_cycle`, no seq tick, no
        // submission), so no transition row is exercised and no typed
        // RejectionReason can trip; the solved-block cursor advances exactly
        // like the `skipped_empty` bookkeeping pass, and the keys stay in the
        // ledger for a later cycle (carry). The check precedes the
        // pending-new-path merge so a shed never clears work it did not do.
        // -----------------------------------------------------------------
        let draw_zero = std::mem::take(&mut self.admission_draw_zero);
        if self.solve_admission && draw_zero {
            self.detached_cycle.shed();
            op_info!(
                domain = solver,
                block_number = solve_block,
                paths.affected = affected_path_ids.len(),
                in_flight = self
                    .detached_cycle
                    .outstanding
                    .load(std::sync::atomic::Ordering::Relaxed),
                target_depth = self.admission_target_depth,
                "SHED: draw-time zero budget — nothing submitted; keys retained for carry"
            );
            // 6XB6NJ: monotone advance on the block cursor (the
            // skipped_empty bookkeeping contract).
            self.cursor.advance_solved(solve_block);
            let arm = CycleArm::Shed {
                keys_affected: affected_path_ids.len(),
                in_flight: self
                    .detached_cycle
                    .outstanding
                    .load(std::sync::atomic::Ordering::Relaxed),
                target_depth: self.admission_target_depth,
            };
            self.last_arm = Some(arm);
            return CycleOutcome {
                solved_block: solve_block,
                census: ResolveCensus {
                    affected: affected.len() as u64,
                    resolved: 0,
                    same_state: 0,
                    projections: 0,
                },
                arm,
            };
        }
        // Also re-solve any paths registered via register_and_solve_path that
        // haven't been through rebuild_and_solve_affected yet. These paths were
        // eagerly solved at registration time, but the pump's process_block
        // replaces self.results entirely — so we must include them to avoid
        // dropping their results.
        affected_path_ids.extend(&self.pending_new_paths);
        self.pending_new_paths.clear();
        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            // Cold-start trace: keys with NO registered paths reach here as a
            // bookkeeping-only pass (span exists, no dispatch) — stamp so the
            // cycle span never reads as arm-less.
            // 6XB6NJ: monotone advance on the block cursor.
            self.cursor.advance_solved(solve_block);
            let arm = CycleArm::SkippedEmpty { keys_affected: 0 };
            self.last_arm = Some(arm);
            return CycleOutcome {
                solved_block: solve_block,
                census: ResolveCensus {
                    affected: affected.len() as u64,
                    resolved: 0,
                    same_state: 0,
                    projections: 0,
                },
                arm,
            };
        }

        // Telemetry: name EVERY path the dirty-pool fan-out just activated,
        // with its concrete hop list — a Jaeger trace now answers "which pools
        // are in this path" without cross-referencing Python state. Runs under
        // the drainer's `degenbot.arb.solve` span, so the events parent there.
        // MQUKB6-T2: phase span - the fan-out activation telemetry gets its
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
            // Per-path activation events are debug-level now (N225ET): the
            // per-event span plumbing dominated the fan-out phase; the
            // diagnostic remains reachable via RUST_LOG degenbot::engine=debug.
            if tracing::enabled!(target: "degenbot::engine", tracing::Level::DEBUG) {
                for &path_id in &affected_path_ids {
                    diag!(domain = solver, block_number = solve_block,
                        path.id = path_id,
                        path.hops = %self.describe_path_cached(path_id, registry),
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
        self.paths_same_state_this_cycle = 0;
        // 7LV6VN T2: these accumulate from the chunk merges inside the resolve
        // block below (same content the serial loop used to produce inline).
        let mut deferred_paths: HashSet<u64> = HashSet::new();
        let mut invalid_reasons: HashMap<String, u64> = HashMap::new();
        // KJWIK5: clone the deferred re-record hook out before the resolve
        // borrows `self` (Arc bump, cheap); it fires at the deferral site
        // below with the deferred paths' hop-pool keys.
        let deferred_re_record = self.deferred_re_record.clone();
        // MQUKB6-T2: phase span for the core-lock re-derive window (the
        // summary event after the block stays on the same node).
        let resolve_ctx = tracing::info_span!(
            target: "degenbot::solver",
            "degenbot.arb.resolve",
            block.number = solve_block,
            paths.affected = affected_path_ids.len(),
        )
        .entered();
        // 7LV6VN T2: chunked fan-out over the affected paths. The
        // sharded hop cache keeps cross-path hit reuse intact (a chunk-local
        // cache would multiply the expensive CL tick-walk per shared pool),
        // so chunks contend only on shard locks, never on each other's work.
        // Per-path chunk outputs merge serially below in deterministic order;
        // `resolve_hops` semantics are byte-identical (same core-read window,
        // same deficits, same memo validation).

        hotpath::measure_block!("arb_solve.resolve", {
            // Violated only while a writer is queued (parking_lot read acquire):
            // nonzero = core-lock congestion, not compute.
            let core = hotpath::measure_block!(
                "resolve.core_read_acquire",
                self.core
                    .read_at(crate::bot_core::state_lock::LockSite::Solver)
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
                    let Some(path) = registry.get(path_id) else {
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
                    // RLVDUP T3: one pool walk builds the snapshot, the
                    // same-state comparison AND the future-price check -
                    // the per-path future probe re-walked all pools.
                    let mut update_snapshot: Vec<u64> = Vec::with_capacity(path.pools.len());
                    // KJWIK5 test seam: the real future-price tripwire is
                    // unreachable after the solve-anchor head floor, so test
                    // builds seed the flag from the forced-deferral set to
                    // exercise the carry.
                    #[cfg(test)]
                    let mut future = self
                        .test_force_deferred
                        .as_ref()
                        .is_some_and(|forced| forced.contains(&path_id));
                    #[cfg(not(test))]
                    let mut future = false;
                    for pool_ref in &path.pools {
                        let ub = core.pool_update_block(pool_ref.pool_key);
                        if anchor.is_future(ub) {
                            future = true;
                        }
                        update_snapshot.push(ub);
                    }
                    let same_state = self
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
                        &self.hop_projection_cache,
                        Some(&mut chunk_projections),
                        self.cl_projection_memo,
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
                    // R522XA: drive the path state machine from the full deficit set.
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
                if !self.resolve_par_stance || affected_vec.len() < RESOLVE_PAR_MIN {
                    vec![resolve_chunk(&affected_vec)]
                } else {
                    // P6YXA6: the resolve chunk fan-out leaves rayon with
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
                    let mut chunk_pairs =
                        staged.into_inner().unwrap_or_else(PoisonError::into_inner);
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
                        self.resolved_update_snapshot.insert(path_id, snapshot);
                    }
                    for (path_id, arc) in resolved {
                        self.path_resolved.insert(path_id, arc);
                    }
                    for (path_id, deficits) in status {
                        self.path_status
                            .entry(path_id)
                            .or_default()
                            .set_resolved(&deficits);
                    }
                    for (reason, count) in chunk_invalid {
                        *invalid_reasons.entry(reason).or_insert(0u64) += count;
                    }
                }
                self.paths_same_state_this_cycle = same_state_total;
                // Lifetime counter (the serial loop accumulated in place).
                self.hop_projection_count += projections_total;
            });
        });
        // KJWIK5: the ledger carry for deferred paths — re-record EVERY hop
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
                let mut keys: Vec<degenbot_solvers::affected_keys::AffectedKey> = Vec::new();
                for path_id in deferred_sorted {
                    if let Some(path) = registry.get(path_id) {
                        keys.extend(path.pools.iter().map(|pool_ref| {
                            degenbot_solvers::affected_keys::AffectedKey::new(
                                pool_ref.hop_type,
                                pool_ref.pool_key,
                            )
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
            u32::try_from(self.paths_same_state_this_cycle).unwrap_or(u32::MAX),
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
            paths.same_state = self.paths_same_state_this_cycle,
            hop.projections = self.hop_projection_count,
            paths.deferred_future_price = deferred_paths.len(),
            invalid.reasons = %invalid_reasons.iter().map(|(r, c)| format!("{c}x {r}")).collect::<Vec<_>>().join(", "),
            phase_us = u64::try_from(cycle_start.elapsed().as_micros()).unwrap_or(u64::MAX),
            "resolved hop snapshots"
        );
        drop(resolve_ctx);

        // MQUKB6-T2 follow (trace f701ccd36f4ecf80d671e798df218fa4, block
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
        // ADR-005 slice 15b-1: the solve fans out across executor bins
        // the affected-path set. `Self::solve_path` is a free-standing dispatch
        // (no `&self` read); each work item takes the `path_id` + an **Arc-
        // shared** `ResolvedMixedPath` snapshot (f701ccd3 staging fix: the
        // former per-path deep clone copied every CL
        // `IntV3TickRangeSequence` every cycle — the "clone is cheap" claim
        // was disproven by telemetry at ~25 µs/path steady state, 150-420
        // µs/path on the cold-heap ramp), `path_resolved` entries being
        // immutable between resolve passes. Workers then write — under the
        // parallel closure — into the engine-level result-set via a
        // `Mutex`-free pattern: collect `(path_id, SolvePathResult)` pairs
        // into a Vec, then merge sequentially into `self.results`. The
        // parallel workers touch NO engine state and NO core.lock —
        // engine-then-core lock ordering is preserved unchanged (the
        // internal thread pool never re-enters the engine `Mutex`). For tiny
        // batches the dispatch overhead is bounded by the lazy
        // split (see `par_iter` docs); the sequential cost dominates below
        // executor internals.
        //
        // Pre-collect the work items (path_id + resolved-snapshot). The Arc
        // clones drop the immutable borrow on `self.path_resolved` that
        // would block parallel dispatch.
        let mut invalid_count: u64 = 0;
        let to_solve: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)> = solve_path_ids
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
                Some((pid, std::sync::Arc::clone(resolved)))
            })
            .collect();

        drop(stage_ctx);
        stage_span.record("paths.staged", to_solve.len());
        drop(stage_span);

        // Filter out empty/profitless results in the same pass that produces
        // them — the contract is identical to the prior serial loop.
        // D63GSE: per-path wall time is captured so the K slowest paths can be
        // named on the completion event (a min-heap keeps this O(K) memory;
        // the closure itself only does one Instant pair + map insert).
        // (time_us, pieces_visited, path_sims, pid) for the K-slowest
        // attribution — lets the completion event name the walk-combinatorial
        // cost driver of the slowest routes, not just their wall time.
        // -----------------------------------------------------------------
        // BXUSGL T1: per-cycle shared solve context. The pure solver phase
        // is a pure function of the resolved snapshots + this context:
        // workers (the dedicated tokio executor bins) hold Arc
        // CLONES and touch NO engine state, NO core.lock - engine-then-core
        // lock ordering is preserved unchanged. The SINGLE engine-Mutex hold
        // still covers the whole cycle (the drain-side merge happens before
        // this method returns), so cycle atomicity - the results.remove
        // above, pending_new_paths, results_block stamping - is preserved
        // by construction with NO epoch guards.
        // -----------------------------------------------------------------
        let path_times: parking_lot::Mutex<PathTimesHeap> =
            parking_lot::Mutex::new(PathTimesHeap::new());
        let solve_cpu_us: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_pieces_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_sims_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_word_steps_total: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let walk_refine_sims_total: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let walk_ternary_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let walk_grid_total: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Degenerate-path capture config (M6776W): env parsed ONCE per cycle
        // at the owner; the gate itself reads no environment. The prefix-
        // composition cache is generationed by the block epoch inside the
        // gate deps (no public reset to call anymore).
        let gate_capture = gate_capture_from_cfg(&self.cfg);
        // Optional offline CL-solver capture (DEGENBOT_SOLVER_CAPTURE=1): dump
        // the exact all-CL pool state the solver consumed for heavy paths so
        // the CL solver can be optimized offline. None (no-op) unless gated.
        let capture = HeavyPathCapture::from_capture(&self.cfg.capture, CaptureVariant::HeavyCl);
        // Optional mixed V2+CL solver capture (same gate): heavy
        // mixed paths (e.g. path 7042 V2->V3->V3) dispatch to
        // `exact_solve_mixed_path_n_cached`, which the all-CL capture skips.
        // Defaults OUT of the fixtures dir (loop-18: working rows never
        // accrete there; goldens are produced only by cl_capture_gen).
        let capture_mixed =
            HeavyPathCapture::from_capture(&self.cfg.capture, CaptureVariant::HeavyMixed);
        // SIMPIPE2 T2: pool-ref snapshot aligned to `to_solve` order (the
        // worker clamp's pool list) — captured under this cycle's engine
        // Mutex so it cannot interleave with a re-registration.
        // RLVDUP2 T6: the snapshot is one Arc bump per path - path_pools
        // values are Arc<MixedPath>, immutable between register/deregister.
        let pool_refs: Vec<std::sync::Arc<MixedPath>> = to_solve
            .iter()
            .map(|(pid, _)| {
                registry
                    .get(*pid)
                    .cloned()
                    .unwrap_or_else(|| std::sync::Arc::new(MixedPath { pools: Vec::new() }))
            })
            .collect();
        let shared = std::sync::Arc::new(SolveCycleShared {
            solve_block,
            epoch: solve_block,
            metadata: *metadata,
            gate_capture,
            walk_memo: std::sync::Arc::clone(&self.walk_memo),
            runtime: self.runtime_cfg,
            capture: capture.map(std::sync::Arc::new),
            capture_mixed: capture_mixed.map(std::sync::Arc::new),
            path_times,
            gate_total: parking_lot::Mutex::new(
                ::degenbot_solvers::profit_envelope::GateStats::default(),
            ),
            solve_cpu_us,
            walk_pieces_total,
            walk_sims_total,
            walk_word_steps_total,
            walk_refine_sims_total,
            walk_ternary_total,
            walk_grid_total,
            sims_recorder: std::sync::Arc::clone(&self.last_walk_sims),
            gate_recorder: std::sync::Arc::clone(&self.last_gate_us),
            #[cfg(test)]
            test_solve_delay: self.test_solve_delay.clone(),
            #[cfg(test)]
            test_solve_panic: self.test_solve_panic.clone(),
            core: std::sync::Arc::clone(&self.core),
            pool_refs,
            worker_clamp: INLINE_SIM_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
            inline_sim: self.inline_sim.clone(),
        });
        // The LPT bins are Arc-shared for every arm; the
        // arms index through the same deref (byte-identical semantics).
        let to_solve = std::sync::Arc::new(to_solve);

        // LPT cost + binning shared by the LPT arms (both dispatch modes);
        // called lazily inside the arms so the per-mode hotpath labels
        // keep each arm measured span (a few us of bin-pack included, as
        // before).
        let compute_bins = || {
            // MQUKB6-T2: phase span for the LPT bin-pack (shared by both
            // dispatch arms - and the detached arm's identical binning).
            let _lpt_ctx = tracing::info_span!(
                target: "degenbot::solver",
                "degenbot.arb.lpt",
                paths = to_solve.len(),
            )
            .entered();
            // VPD5ZH: bins follow the cgroup-budget worker count (not the
            // old pool width) so each arm is exactly n_bins tasks
            // on n_workers persistent threads, per the solve_executor doc.
            // P6YXA6 sizing reconciliation: fleet-hosted cycles bin at the
            // fleet's STRUCTURAL seat count — pins and bins are the same
            // number, so every bin owns a warm keyed seat across cycles.
            let n_threads = solve_bin_count();
            // LW-T7 (Seam F): the bin fan-out goes through the typed runtime
            // fallback decision — a capability narrower than the intended
            // width is a NAMED-AND-LOGGED plan, never a silent narrower bin.
            let plan = plan_bins(n_threads, n_threads);
            let n_threads = plan.bins;
            // Loop-12 KUKHMX: previous-block measured walk sims refine the
            // LPT cost; snapshot once (single lock) before binning.
            // Loop-18: measured gate us rides the same snapshot - gate-heavy
            // paths (dense-CL compose) need the bins to know they are
            // expensive despite sims~0.
            let last_sims_snapshot: HashMap<u64, u64> = shared.sims_recorder.lock().clone();
            let last_gate_snapshot: HashMap<u64, u64> = shared.gate_recorder.lock().clone();
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
            lpt_partition(to_solve.len(), n_threads, |i| costs[i])
        };

        // -----------------------------------------------------------------
        // DETACHED arm (epic SRQEK5 WV62TX): the ONLY solve arm since WFF6MM
        // — enqueue the SOLVES on a plain std::thread per LPT bin (riding
        // the fleet executor) and RETURN at enqueue end. Every result flows
        // through the unbounded mpsc to the merge sidecar, which applies the
        // Q1a stale policy under the engine Mutex.
        // -----------------------------------------------------------------
        // P37YJG: the arm decision IS the machine's begin_cycle — the ONE
        // seq tick and the merge-pipe open-once + Sender clone all live on
        // the machine now. WFF6MM: the stance input, the cap verdict, and
        // the in-cycle fallback retired; a positive-draw cycle ALWAYS
        // detaches (the draw already owns backpressure; there is no
        // post-begin race-shed).
        let arm = self.detached_cycle.begin_cycle();
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
                        self.resolved_update_snapshot
                            .get(pid)
                            .map(|stamp| (*pid, stamp.clone()))
                    })
                    .collect(),
            );
            // LPT binning over the bin plan computed above.
            let bins = compute_bins();
            // Copy the cycle metadata out: the 'static bin threads
            // outlive the caller's &BlockMetadata borrow.
            let cycle_metadata = *metadata;
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
                let solve_span_bin = solve_span.clone();
                // 43E3H3: the submitted pids (the bin's owed list) are
                // computed from the bin EXACTLY as the in-cycle arm's
                // lane_pids — the lane witness seeds its owed set from
                // the same source the hold vec's pids come from.
                let lane_pids: Vec<u64> = bin.iter().map(|&i| to_solve[i].0).collect();
                // 43E3H3: the bin's submit closure — send on the DETACHED
                // merge pipe and bump the in-flight gauge at SEND success
                // ONLY (variant-gated pairing, design §4.6.1: Suppressed
                // and Failed ride lane.solved/lane.failed, which touch
                // neither the pipe gauge bump nor this closure). A bin
                // that dies before sending never leaks a count. 43E3H3
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
                    self.detached_cycle.gauge_hook();
                // ergo INYMDG: bin jobs ride the fleet executor
                // (fleet.stance=fleet) or the dedicated tokio solve
                // executor (persistent warm workers, BXUSGL T1). The body
                // is unchanged; same 'static + Send move semantics, and
                // concurrent detached cycles share the persistent
                // worker set instead of forking one thread per bin.
                // WNH5OL (epic BPZUCM, card 1): the ONE lane walk —
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
                // 43E3H3 (design §5.2, REV 2 Defect 3): the detached arm
                // seals its bins with the lane witness. The explicit
                // `&LaneCtx` annotation is load-bearing: without it the
                // closure-parameter inference drifts and `run_bin`'s lane
                // binding re-derives. With the witness, a panicked
                // detached bin's undelivered pids arrive as typed
                // `Failed` records on the merge pipe (the silent-
                // undercount gap — design §1.9 — is closed).
                let lane_key =
                    SOLVE_BIN_KEY_BASE.saturating_add(u64::try_from(bin_idx).unwrap_or(u64::MAX));
                let spawn_job = move |_ctx: &LaneCtx| {
                    let mut lane = SolveLane::new(lane_key, lane_key, lane_pids, tx.clone());
                    // 43E3H3 gauge pairing: the in-flight bump rides the
                    // lane's Solved send-success (send + bump + emitted
                    // in one path). Failed/Suppressed sends never fire
                    // it (REV 2 Defect 1) — the corresponding disposition
                    // arm never decrements.
                    lane.set_on_solved_send(gauge_bump.clone());
                    // AQV6EF: the same lane carries the drain-death
                    // hook — a terminal send failure on the merge pipe
                    // is counted and trips the sticky cordon instead of
                    // being swallowed.
                    lane.set_on_send_failed(std::sync::Arc::new(|failure| {
                        crate::arb_engine::executor::drain_death_response(failure, None);
                    }));
                    run_solve_lane(&mut lane, &SeatSurvivesPolicy, run_bin);
                };
                // LW-T8: both arms submit through the ONE Executor
                // token (the fleet has been the sole executor since the
                // LW-T9 cutover).
                if let Err(err) = crate::arb_engine::executor::global_executor()
                    .submit(bin_idx, Box::new(spawn_job))
                {
                    crate::arb_engine::fleet_solve_executor::abort_loud(
                        "bin submission, posture gate",
                        &format!("{err}"),
                    );
                }
            }
            self.detached_cycle.publish_gauge();
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
        // WFF6MM test harness: direct `rebuild_and_solve_affected` /
        // `solve_dirty` callers (the resolve/solve unit tests) merge the
        // just-enqueued pipe INLINE through the sidecar's own per-item
        // merge path, so they keep reading results synchronously. The
        // production path (EngineStages) leaves `test_sync_merge` OFF.
        #[cfg(test)]
        if self.test_sync_merge {
            self.drain_merge_inline(to_solve.len(), registry, delivery);
        }
        // 6XB6NJ: monotone advance on the block cursor.
        self.cursor.advance_solved(solve_block);
        // ENQUEUE-END semantics (T2 acceptance: "return is enqueue-end,
        // not apply-end"): the engine Mutex hold ENDS here; the sidecar
        // re-acquires it per merged straggler.

        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
        let arm = if to_solve.is_empty() {
            CycleArm::Dissolved {
                invalid: usize::try_from(invalid_count).unwrap_or(usize::MAX),
                deferred_future_price: deferred_paths.len(),
            }
        } else {
            CycleArm::Solved {
                seq: solved_seq,
                bins: solved_bins,
                staged: to_solve.len(),
                invalid: usize::try_from(invalid_count).unwrap_or(usize::MAX),
                deferred_future_price: deferred_paths.len(),
            }
        };
        self.last_arm = Some(arm);
        CycleOutcome {
            solved_block: solve_block,
            census: ResolveCensus {
                affected: affected.len() as u64,
                resolved: u64::try_from(affected_path_ids.len()).unwrap_or(u64::MAX),
                same_state: same_state_total,
                projections: projections_total,
            },
            arm,
        }
    }
    pub(crate) fn solve_all(&self, registry: &PathRegistry) -> HashMap<u64, SolvePathResult> {
        // MQUKB6-T0: same span-context re-entry as rebuild_and_solve_affected:
        // bin jobs re-enter this cycle span per work item, so per-path child
        // spans parent under the cold-start cycle instead of forking roots.
        let solve_span = tracing::Span::current();

        // Pre-collect work items (path_id + Arc-shared resolved). The Arc
        // clones drop the immutable borrow on self.path_resolved so the
        // 'static bin jobs don't capture &self at all (f701ccd3 staging fix:
        // Arc clones are refcount bumps, not deep clones of the CL
        // tick-range sequences).
        let to_solve: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)> = self
            .path_resolved
            .iter()
            .filter(|(_, r)| r.valid)
            .map(|(&pid, r)| (pid, std::sync::Arc::clone(r)))
            .collect();

        // RAYPAR T3: LPT-pre-balanced partition. The cold start has the
        // same cost skew as the hot path, so it bins over the structural
        // bin count too — the fleet's Solver seats when fleet-hosted
        // (pins == bins), else solve_worker_count's dedicated-runtime bins.
        let n_bins = solve_bin_count();
        // Cold start has no previous-block sims/gate yet: structural proxy only.
        let costs: Vec<usize> = to_solve
            .iter()
            .map(|(_, r)| sims_aware_cost(path_cost_proxy(r), None, None))
            .collect();
        let bins = lpt_partition(to_solve.len(), n_bins, |i| costs[i]);

        // Bin jobs are 'static over Arc-cloned state: walk memo, core and
        // the pool-ref map for the UO3JM4 clamp. No engine state is touched
        // (engine-then-core invariant intact; the mixer only reads core).
        let memo = std::sync::Arc::clone(&self.walk_memo);
        let path_pools: HashMap<u64, std::sync::Arc<MixedPath>> = registry.path_pools().clone();
        let core = std::sync::Arc::clone(&self.core);
        let results_block = self.cursor.results_block();
        let runtime_cfg = self.runtime_cfg;
        let (tx, rx) = std::sync::mpsc::channel::<(u64, SolvePathResult)>();
        for (bin_idx, bin) in bins.iter().enumerate() {
            let bin = bin.clone();
            let to_solve_bin = to_solve.clone();
            let memo = std::sync::Arc::clone(&memo);
            let path_pools = path_pools.clone();
            let core = std::sync::Arc::clone(&core);
            let tx = tx.clone();
            let solve_span_bin = solve_span.clone();
            let run_bin = move || {
                // Cold start: no capture wiring — deps with the
                // registered-epoch guard + the engine walk-memo handle.
                let mut gate_deps = ::degenbot_solvers::profit_envelope::GateDeps::per_block_with(
                    results_block,
                    None,
                    runtime_cfg,
                );
                gate_deps.walk_memo = Some(&memo);
                for &i in &bin {
                    let (path_id, resolved) = &to_solve_bin[i];
                    let _solve_ctx = solve_span_bin.enter();
                    if let Some(mut r) = ::degenbot_solvers::mixed::solve_path_with_min_profit(
                        resolved,
                        min_profit_floor(),
                        &gate_deps,
                    )
                    .result
                    .filter(|r| !r.optimal_input.is_zero() && !r.profit.is_zero())
                    .inspect(|r| {
                        if !r.solver_pool_states.is_empty() {
                            diag!(
                                domain = solver,
                                "path_id={path_id} hops=[{}]",
                                r.solver_pool_states.join(";")
                            );
                        }
                    }) {
                        if let Some(path) = path_pools.get(path_id) {
                            let core_read =
                                core.read_at(crate::bot_core::state_lock::LockSite::Solver);
                            let _ =
                                clamp_result_with_state(&core_read, *path_id, &path.pools, &mut r);
                        }
                        let _ = tx.send((*path_id, r));
                    }
                }
            };
            // LW-T8: the ONE Executor token submits on both stances.
            if let Err(err) = crate::arb_engine::executor::global_executor()
                .submit(bin_idx, Box::new(move |_ctx| run_bin()))
            {
                crate::arb_engine::fleet_solve_executor::abort_loud(
                    "bin submission, posture gate",
                    &format!("{err}"),
                );
            }
        }
        drop(tx);
        rx.into_iter().collect()
    }
    fn derive_hop_type(core: &BotState, pool_id: u64) -> Option<HopType> {
        // Aerodrome stable pools route to the Solidly solve branch; volatile
        // Aerodrome is constant-product and routes to the V2 (Möbius) branch
        // (matching the Python `arbitrage.solvers.solidly_stable` classification:
        // `AerodromeV2Pool(stable=True)` → `SolidlyStableHop`, else
        // `ConstantProductHop`).
        if let Some(id) = core.get_aerodrome_identity(pool_id) {
            return Some(if id.stable {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        // Camelot stable_swap pools route to the Solidly solve branch;
        // volatile Camelot is constant-product (V2). Same Python-faithful
        // classification as Aerodrome.
        if let Some(id) = core.get_v2_identity(pool_id) {
            return Some(if id.stable_swap {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        if core.get_v3_pool(pool_id).is_some() {
            Some(HopType::V3)
        } else if core.get_v4_pool(pool_id).is_some() {
            Some(HopType::V4)
        } else if core.get_balancer_weighted_pool(pool_id).is_some() {
            Some(HopType::BalancerWeighted)
        } else if core.get_balancer_stable_pool(pool_id).is_some() {
            Some(HopType::BalancerStable)
        } else if core.get_curve_pool(pool_id).is_some() {
            Some(HopType::CurveStableswap)
        } else {
            None
        }
    }
    pub(crate) fn register_path(
        &mut self,
        hops: Vec<PoolHop>,
        registry: &mut PathRegistry,
    ) -> Result<Registration, PathRegistrationError> {
        // R522XA: fewer than two hops is a structural caller bug, not a state —
        // reject loudly at construction.
        if hops.len() < 2 {
            return Err(PathRegistrationError::Invalid(format!(
                "register_path: path has {} hops (need >= 2) — structurally unroutable",
                hops.len()
            )));
        }

        // Telemetry: one Jaeger node per path registration (a root span on the
        // registration worker thread — there is no ambient pump context during
        // `build_paths`). The completion event below carries the CONCRETE hop
        // list so the trace answers "which pools are in this path" directly.
        // FPGOYX: dedup — if the same (pool_id, zero_for_one) sequence is
        // already registered, return the existing path_id instead of creating
        // a duplicate. Without this, `build_paths` re-entry accumulated
        // hundreds of thousands of duplicate paths, OOM-killing the bot.
        let sig: Vec<(u64, bool)> = hops.iter().map(|h| (h.pool_id, h.zero_for_one)).collect();
        if let Some(existing_id) = registry.lookup(&sig) {
            diag!(
                domain = path,
                path_id = existing_id,
                hops.count = hops.len(),
                "duplicate registration skipped (dedup)"
            );
            // PRG-4: the duplicate never crosses the FFI as a skip — the
            // engine counts it for the registration skip telemetry itself.
            registry.note_dedup();
            if let Some(p) = crate::instruments::pipeline() {
                p.count_registration_skip("dup");
            }
            return Ok(Registration {
                path_id: existing_id,
                created: false,
                resolved: None,
            });
        }

        // PRG-4 / IRUMXD: the registered-path cap lives HERE, in the engine
        // path registry (was the Python `MAX_REGISTERED_PATHS` counter +
        // the `DiscoveryCrawlComplete` unwind). A new registration past the
        // cap is refused with the typed benign-stop refusal — the crawl
        // catches it and stops discovery; dedup hits above never reach this
        // check (an existing path is not growth).
        registry.ensure_capacity()?;

        let reg_span = tracing::info_span!("degenbot.path.register", hops.count = hops.len());
        let _reg_guard = reg_span.enter();
        // Resolve each hop's family from the BotState + validate the pool_id
        // exists there. The engine never constructs pools (ADR-006 D3), so
        // hop_type is derived, not caller-supplied.
        let (pool_refs, hop_descs) = self.resolve_hop_refs(hops)?;

        // R522XA: resolve BEFORE storing so an unroutable hop rejects the
        // registration loudly and leaves no half-registered state behind.
        let mut resolved = ResolvedMixedPath::default();
        let deficits = {
            let core = self
                .core
                .read_at(crate::bot_core::state_lock::LockSite::Solver);
            resolve_hops(
                &core,
                &pool_refs,
                &mut resolved,
                &self.hop_projection_cache,
                Some(&mut self.hop_projection_count),
                self.cl_projection_memo,
            )
        };
        if let Some(unroutable) = deficits
            .iter()
            .find(|d| d.reason.is_structurally_unroutable())
        {
            return Err(PathRegistrationError::Invalid(format!(
                "register_path: hop ({hop_type:?} pool {pool_key}) is structurally unroutable ({reason}) — rejecting path at construction",
                hop_type = format!("{:?}", unroutable.hop_type),
                pool_key = unroutable.pool_key,
                reason = unroutable.reason,
            )));
        }

        // Only now commit the path identity (all-or-nothing): allocate the
        // path id (no gaps from rejected registrations), store the immutable
        // pool refs, extend the reverse index, and record the dedup signature.
        let path_id = registry.commit(PathRegistration {
            signature: sig,
            pool_refs,
        });

        // Store the resolve snapshot + drive the state machine. Arc-shared:
        // the solve dispatch stages Arc clones (f701ccd3 staging fix).
        let path_valid = resolved.valid;
        let resolved_arc = std::sync::Arc::new(resolved);
        self.path_resolved
            .insert(path_id, std::sync::Arc::clone(&resolved_arc));
        self.path_status
            .entry(path_id)
            .or_default()
            .set_resolved(&deficits);

        // DEBUG-gated (log-volume cut OPBD7L): one line per path registration
        // was ~48% of a 10G run log (new pools/hop-combos register constantly
        // on a live run). The registration itself stays fully observable via
        // the `degenbot.path.register` OTel span (record filter uncapped) and
        // the `path_pools` count metric; re-enable with
        // `RUST_LOG=degenbot_bot=debug` for desync investigations.
        diag!(domain = path, path_id = path_id,
            hops.count = hop_descs.len(),
            hops = %hop_descs.join(" -> "),
            valid = path_valid,
            "registered"
        );

        Ok(Registration {
            path_id,
            created: true,
            resolved: Some(resolved_arc),
        })
    }
    /// Derive each hop's family from the `BotState` and build the register
    /// refs — split out of `register_path` so the resolve-before-store
    /// ordering stays intact without tripping the `too_many_lines` ceiling
    /// (ADR-006 D3: the engine derives `hop_type`, never the caller).
    ///
    /// # Errors
    ///
    /// Returns `Err` for any hop whose `pool_id` is not registered in the
    /// associated `BotState`.
    fn resolve_hop_refs(
        &self,
        hops: Vec<PoolHop>,
    ) -> Result<(Vec<MixedPoolRef>, Vec<String>), PathRegistrationError> {
        let mut pool_refs = Vec::with_capacity(hops.len());
        let mut hop_descs = Vec::with_capacity(hops.len());
        let core = self
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        for hop in hops {
            let Some(hop_type) = Self::derive_hop_type(&core, hop.pool_id) else {
                return Err(PathRegistrationError::Invalid(format!(
                    "register_path: pool_id {} is not registered in the associated BotState",
                    hop.pool_id
                )));
            };
            hop_descs.push(super::path_info::describe_hop(
                &core,
                hop_type,
                hop.pool_id,
                hop.zero_for_one,
            ));
            pool_refs.push(MixedPoolRef {
                hop_type,
                pool_key: hop.pool_id,
                zero_for_one: hop.zero_for_one,
            });
        }
        Ok((pool_refs, hop_descs))
    }

    /// Register a path and eagerly solve it (cycle layer, ADR-045).
    ///
    /// # Errors
    ///
    /// Returns `Err` if any `pool_id` is not registered in the associated
    /// `BotState` (see [`register_path`](Self::register_path)).
    pub(crate) fn register_and_solve_path(
        &mut self,
        hops: Vec<PoolHop>,
        registry: &mut PathRegistry,
    ) -> Result<Registration, PathRegistrationError> {
        let registration = self.register_path(hops, registry)?;
        if !registration.created {
            return Ok(registration);
        }
        let path_id = registration.path_id;
        // Eagerly solve the newly registered path.
        if let Some(resolved) = registration.resolved.as_ref() {
            if resolved.valid {
                if let Some(mut solve_result) = ::degenbot_solvers::mixed::solve_path(
                    resolved,
                    &::degenbot_solvers::profit_envelope::GateDeps::offline(),
                )
                .result
                {
                    if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                        self.clamp_cl_hop_capacity(path_id, &mut solve_result, registry);
                        self.results.insert(path_id, solve_result);
                        self.pending_new_paths.insert(path_id);
                    }
                }
            }
        }
        Ok(registration)
    }
    pub(crate) fn solve_all_paths(&mut self, block_number: u64, registry: &PathRegistry) {
        // Resolve all paths under the core lock (single consistent snapshot of
        // all family state — ADR-003).
        {
            let core = self
                .core
                .read_at(crate::bot_core::state_lock::LockSite::Solver);
            for (&path_id, path) in registry.iter() {
                let mut resolved = ResolvedMixedPath::default();
                let deficits = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    &self.hop_projection_cache,
                    Some(&mut self.hop_projection_count),
                    self.cl_projection_memo,
                );
                self.path_resolved
                    .insert(path_id, std::sync::Arc::new(resolved));
                // R522XA: cold-start full sweep also refreshes the state machine.
                self.path_status
                    .entry(path_id)
                    .or_default()
                    .set_resolved(&deficits);
            }
        }

        // Solve all paths
        let results = self.solve_all(registry);
        self.results.clear();
        for (pid, r) in results {
            self.results.insert(pid, r);
        }
        // 6XB6NJ: monotone advance on the block cursor (the cold-start
        // sweep can no longer drag a seeded resume anchor backwards).
        self.cursor.advance_solved(block_number);

        // Intentionally no compute_diff_and_send here: dispatching would
        // advance `delivered` (claiming "Python has seen these") before any
        // channel exists — poisoning the diff for the first real send. The
        // pump owns dispatch via `send_result_batch`.
    }
    pub(crate) fn describe_path(&self, path_id: u64, registry: &PathRegistry) -> String {
        let Some(path) = registry.get(path_id) else {
            return format!("path_id={path_id} (unregistered)");
        };
        let core = self
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let hops: Vec<String> = path
            .pools
            .iter()
            .map(|r| describe_hop(&core, r.hop_type, r.pool_key, r.zero_for_one))
            .collect();
        format!("path_id={path_id} [{}]", hops.join(" -> "))
    }
    pub(crate) fn describe_path_cached(
        &self,
        path_id: u64,
        registry: &PathRegistry,
    ) -> std::sync::Arc<str> {
        if let Some(hit) = self.path_description_cache.lock().get(&path_id) {
            return std::sync::Arc::clone(hit);
        }
        let rendered: std::sync::Arc<str> =
            std::sync::Arc::from(self.describe_path(path_id, registry));
        self.path_description_cache
            .lock()
            .insert(path_id, std::sync::Arc::clone(&rendered));
        rendered
    }
}

impl SolveCycle {
    /// QTZGFL: the admission DRAW - the SINGLE consumption decision of the
    /// epoch ledger. `budget = max(0, target - in-flight)` keys freshest-first,
    /// the overflow RETAINED for a later cycle; a zero budget is the SHED
    /// verdict (nothing drawn, the stash records it).
    pub(crate) fn draw(&mut self, delta: &EpochDelta, head: u64) -> Vec<AffectedKey> {
        if !self.solve_admission {
            self.admission_draw_zero = false;
            return delta.take_keys();
        }
        let outstanding = self
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed);
        let budget = usize::try_from(self.admission_target_depth.saturating_sub(outstanding))
            .unwrap_or(usize::MAX);
        let cutoff = head.saturating_sub(self.admission_retention_blocks);
        let expired = delta.expire_older_than(cutoff);
        self.detached_cycle.note_leads_expired(expired);
        self.admission_draw_zero = budget == 0;
        delta.draw_freshest(budget)
    }

    /// ADR-045: the ids-path-deregistered coupling. Drops the path's resolve
    /// companions, results, and pending-new carry. The registry removal and the
    /// delivery bookkeeping stay on the engine.
    pub(crate) fn forget(&mut self, path_id: u64) {
        self.path_resolved.remove(&path_id);
        self.path_status.remove(&path_id);
        self.resolved_update_snapshot.remove(&path_id);
        self.results.remove(&path_id);
        self.pending_new_paths.remove(&path_id);
    }
}

#[cfg(test)]
impl SolveCycle {
    /// WFF6MM test harness: drain up to `expected` items from the merge pipe
    /// INLINE through the sidecar's own per-item merge path
    /// (`merge_detached_item`), so a direct `solve_dirty` /
    /// `rebuild_and_solve_affected` caller reads its results synchronously.
    /// Moved here from `ArbitrageEngine` (ADR-045 T4).
    pub(crate) fn drain_merge_inline(
        &mut self,
        expected: usize,
        registry: &PathRegistry,
        delivery: &mut DeliveryPolicy,
    ) {
        if self.test_merge_rx.is_none() {
            self.test_merge_rx = self.detached_cycle.take_merge_rx();
        }
        let Some(rx) = self.test_merge_rx.take() else {
            return;
        };
        for _ in 0..expected {
            let Ok(item) = rx.recv() else { break };
            self.merge_detached_item(item, registry, delivery);
        }
        self.test_merge_rx = Some(rx);
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
    use ::degenbot_solvers::mixed::{HopType, PoolHop};
    use alloy::primitives::{aliases::U112, Address, U256};
    use hashbrown::HashSet;

    fn usdc(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }

    fn weth(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    /// Two divergent V2 pools plus the profitable two-hop path between them —
    /// the same fixture `register_and_solve_path_eagerly_solves` uses.
    fn divergent_v2_path(engine: &ArbitrageEngine) -> Vec<PoolHop> {
        let a = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let b = engine.register_v2_pool(
            Address::from([0x12u8; 20]),
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        vec![
            PoolHop {
                pool_id: a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: b,
                zero_for_one: true,
            },
        ]
    }

    // ---------------------------------------------------------------------
    // Green-pin: the ADR-043 vocabulary + the ADR-045 type seam.
    // ---------------------------------------------------------------------

    #[test]
    fn cycle_arm_labels_are_byte_stable() {
        assert_eq!(
            CycleArm::Shed {
                keys_affected: 3,
                in_flight: 8,
                target_depth: 8,
            }
            .label(),
            "shed"
        );
        assert_eq!(
            CycleArm::SkippedEmpty { keys_affected: 0 }.label(),
            "skipped_empty"
        );
        assert_eq!(
            CycleArm::Solved {
                seq: 1,
                bins: 4,
                staged: 10,
                invalid: 0,
                deferred_future_price: 0,
            }
            .label(),
            "detached"
        );
        assert_eq!(
            CycleArm::Dissolved {
                invalid: 2,
                deferred_future_price: 1,
            }
            .label(),
            "detached"
        );
    }

    #[test]
    fn cycle_arm_is_dispatch_selects_detached_arms() {
        assert!(!CycleArm::Shed {
            keys_affected: 3,
            in_flight: 8,
            target_depth: 8,
        }
        .is_dispatch());
        assert!(!CycleArm::SkippedEmpty { keys_affected: 0 }.is_dispatch());
        assert!(CycleArm::Solved {
            seq: 1,
            bins: 4,
            staged: 10,
            invalid: 0,
            deferred_future_price: 0,
        }
        .is_dispatch());
        assert!(CycleArm::Dissolved {
            invalid: 2,
            deferred_future_price: 1,
        }
        .is_dispatch());
    }

    #[test]
    fn cycle_outcome_exposes_block_and_arm_label() {
        let outcome = CycleOutcome {
            solved_block: 21_000_000,
            census: ResolveCensus {
                affected: 12,
                resolved: 11,
                same_state: 4,
                projections: 7,
            },
            arm: CycleArm::Solved {
                seq: 9,
                bins: 4,
                staged: 12,
                invalid: 1,
                deferred_future_price: 0,
            },
        };
        assert_eq!(outcome.solved_block(), 21_000_000);
        assert_eq!(outcome.arm_label(), "detached");
        assert_eq!(outcome.census.affected, 12);
        assert!(outcome.arm.is_dispatch());
    }

    #[test]
    fn registration_encodes_created_and_resolved() {
        let fresh = Registration {
            path_id: 7,
            created: true,
            resolved: Some(Arc::new(ResolvedMixedPath::default())),
        };
        assert!(fresh.created);
        assert!(fresh.resolved.is_some());

        let dedup = Registration {
            path_id: 7,
            created: false,
            resolved: None,
        };
        assert!(!dedup.created);
        assert_eq!(dedup.path_id, 7);
        assert!(dedup.resolved.is_none());
    }

    /// Pin today's carry contract: an eager registration survives exactly one
    /// following dirty cycle (merge, not discard).
    #[test]
    fn pending_new_path_result_survives_one_dirty_cycle() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);
        let path_id = engine
            .register_and_solve_path(hops)
            .expect("eager register must solve the divergent path");

        assert!(
            engine.cycle.pending_new_paths.contains(&path_id),
            "register_and_solve_path arms the pending-new carry"
        );

        let empty = crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        engine.cycle.run_epoch(
            &empty,
            1,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );

        assert!(
            engine.cycle.pending_new_paths.is_empty(),
            "the dirty cycle consumes and clears the carry"
        );
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        assert!(
            results.contains_key(&path_id),
            "the eager result merges across the cycle instead of being discarded"
        );
    }

    // ---------------------------------------------------------------------
    // The dedup fact and the pending-new carry (positive contract).
    // ---------------------------------------------------------------------

    /// The positive contract (ADR-045, T4): a dedup hit yields
    /// `Registration { created: false, .. }` and must NOT touch the
    /// pending-new carry. `register_and_solve_path` gates the carry write on
    /// `Registration.created`, so only a fresh register re-arms it and the
    /// pending-new carry stays empty on a dedup hit.
    #[test]
    fn dedup_hit_does_not_touch_pending_new_carry() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);

        let path_id = engine
            .register_and_solve_path(hops.clone())
            .expect("fresh register");
        assert!(
            engine.cycle.pending_new_paths.contains(&path_id),
            "a fresh register arms the carry"
        );

        // Model the carry consumed by one dirty cycle.
        engine.cycle.pending_new_paths.clear();

        let again = engine
            .register_and_solve_path(hops)
            .expect("dedup register");
        assert_eq!(again, path_id, "a dedup hit retains the registry id");

        assert!(
            engine.cycle.pending_new_paths.is_empty(),
            "a dedup hit (created=false) must not touch the pending-new carry"
        );
    }

    // ---------------------------------------------------------------------
    // T4 wiring: the target surface now exists.
    // ---------------------------------------------------------------------

    /// The candidate-8 interlock (live at T4): the cycle consumes the
    /// epoch's work-carried delta (the `affected` vector drawn by the Resolved
    /// stage) and does not hold or re-read a second swappable handle (the
    /// retired `admission_draw_zero` stash).
    #[test]
    fn run_epoch_consumes_the_epoch_work_carried_delta() {
        use crate::bot_core::EpochDelta;

        // The epoch ledger is the SINGLE work-carried owner. The Resolved
        // stage draws the cycle's `affected` keys from it; the Solved stage's
        // `SolveCycle::run_epoch` must consume exactly that vector.
        let delta = EpochDelta::new(11u64);
        delta.record_affected(HopType::V2, 1, 11);
        delta.record_affected(HopType::V2, 2, 11);
        let affected = delta.draw_freshest(usize::MAX);
        assert_eq!(affected.len(), 2, "the draw is the cycle's affected set");
        assert!(
            delta.is_empty(),
            "the draw IS the single consumption decision — no second handle sees the keys"
        );

        let mut engine = ArbitrageEngine::new();
        let metadata = BlockMetadata::default();
        let outcome = engine.cycle.run_epoch(
            &affected,
            11,
            &metadata,
            &engine.registry,
            &mut engine.delivery,
        );
        assert_eq!(outcome.census.affected, 2);
        // No registered paths reference the drawn keys: the bookkeeping-only
        // arm (a shed/empty pass never dispatches).
        assert!(!outcome.arm.is_dispatch());
    }

    /// The fresh-vs-dedup `Registration` facts (ADR-045).
    #[test]
    fn register_and_solve_path_reports_registration_created() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);

        let fresh = engine
            .cycle
            .register_and_solve_path(hops.clone(), &mut engine.registry)
            .expect("fresh register");
        let dedup = engine
            .cycle
            .register_and_solve_path(hops, &mut engine.registry)
            .expect("dedup register");

        assert!(fresh.created);
        assert!(fresh.resolved.is_some());
        assert!(!dedup.created);
        assert!(dedup.resolved.is_none());
        assert_eq!(fresh.path_id, dedup.path_id);
    }
}

#[cfg(test)]
mod clamp_recompute_tests {
    #![expect(clippy::expect_used)] // tests assert recompute invariants
    use super::recompute_clamped_profit;
    use ::degenbot_solvers::mixed::SolvePathResult;
    use alloy::primitives::U256;

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
        let recomputed = recompute_clamped_profit(&r).expect("has outputs");
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
        let recomputed = recompute_clamped_profit(&r).expect("has outputs");
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
        let recomputed = recompute_clamped_profit(&r).expect("has outputs");
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
        assert!(recompute_clamped_profit(&r).is_none());
    }
}
