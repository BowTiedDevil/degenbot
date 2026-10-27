//! The solve cycle as a deep module .
//!
//! ## Scope (this file)
//!
//! **T1** pinned the data-type seam of ADR-045: the `CycleOutcome` /
//! `CycleArm` / `ResolveCensus` / `Registration` vocabulary, with
//! `CycleArm::label()` reproducing the ADR-043 `cycle.arm` labels
//! `"shed" | "skipped_empty" | "detached"` byte-for-byte.
//!
//! **T3** assembled [`SolveCycle`] — the resolve companions, the
//! cycle-transient stash, the solve output, the admissions stance, the
//! detached-arm collaborator, the walk/projection recorders, and the test
//! knobs. The engine holds it as `engine.cycle`.
//!
//! **ADR-076 D1/D2/D3** shrank the surface back to production shape: the test
//! knobs are construction-time fields on [`SolveCycle`] (internal seams, never
//! interface methods), the pending-new carry no longer hands out a `&mut`, and
//! [`run_epoch`] is the composition spine over the explicit [`EpochContext`]
//! whose phases live in the `shed` / `resolve` / `solve` / `merge` / `deliver`
//! submodules — each a machine-direct free function, with only the draw, the
//! merge doors, and the `&self` clamp accessor keeping a `SolveCycle` door here.
//!
//! **T4** landed the interface and moved its bodies onto
//! [`SolveCycle`]: `draw` / `run_epoch` / `register_path` /
//! `register_and_solve_path` / `solve_all_paths` / `merge_detached_item` /
//! `forget`. The T1 vocabulary is consumed by that surface.
//!
//! **T5** hard-cut the string stashes: [`run_epoch`] returns the
//! typed [`CycleOutcome`], the `cycle_arm` / `solve_entry` string fields are
//! deleted, and the stage hook records `cycle.arm` off the outcome. The
//! `CycleEntry` enum existed only to feed the deleted `solve_entry`, so it is
//! gone with it.
//!
//! ## The ticket (the transient stash's one door)
//!
//! The admission draw's verdict used to be a `bool` on the cycle, written by
//! the resolve call and read by the epoch in a later call — the retired
//! pending-new-paths disease. [`CycleTicket`] replaces it: the draw mints one,
//! the Resolved stage row carries it, and the epoch consumes it by value.
//!
//! The staleness class cannot compile. The cycle holds no verdict field, no
//! other module can mint a ticket, and the epoch takes the one it is handed,
//! so the retired shape has nothing left to name:
//!
//! ```text
//! // The engine-side stash the draw used to write (and a LATER call to read):
//! fn stale_verdict_read(cycle: &SolveCycle) {
//!     let _ = cycle.admission_draw_zero; // E0609: no field `admission_draw_zero`
//! }
//!
//! // A ticket cannot be fabricated outside the draw that mints it:
//! fn fabricated_ticket() -> CycleTicket {
//!     CycleTicket { affected: Vec::new(), draw_zero: false } // E0603: private field
//! }
//!
//! // And one ticket cannot be spent twice — the epoch takes it by value:
//! fn ticket_is_spent_once(cycle: &mut SolveCycle, ticket: CycleTicket, epoch: u64) {
//!     run(cycle, ticket, epoch);
//!     run(cycle, ticket, epoch); // E0382: use of moved value: `ticket`
//! }
//! ```
//!
//! The runtime half of the same guard is the Resolved row's take-once slot:
//! [`crate::bot_core::AffectedPaths::take_ticket`] moves the ticket out exactly
//! once, so a second Solved pass over one Resolved row panics instead of
//! solving from a spent ticket.
//!
//! ## Test ledger
//!
//! * **The cycle ticket** (a deliberate half-step): the draw mints it, the
//!   epoch consumes it, and the Resolved row's take-once slot is pinned by
//!   `the_resolved_row_takes_its_ticket_exactly_once`.
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
use super::block_cursor::BlockCursor;
use super::delivery_policy::DeliveryPolicy;
use super::detached_cycle::DetachedCycle;
#[cfg(test)]
use super::inline_sim::InlineSimRequest;
use super::inline_sim::SimulatedPathResult;
use super::path_info::describe_hop;
use super::path_lifecycle::PathSolveStatus;
use super::path_registry::{PathRegistration, PathRegistrationError, PathRegistry};
use super::solver_capture::DegeneratePathFileSink;
use super::workload_partition::{lpt_partition, path_cost_proxy, sims_aware_cost, solve_bin_count};
use super::DeferredReRecordHook;
#[cfg(test)]
use crate::arb_engine::detached_cycle;
use crate::arb_engine::executor::LaneOutcome;
use ::degenbot_pools::solve::{
    HopType, MixedPath, MixedPoolRef, PoolHop, ResolvedMixedPath, SolvePathResult,
};
use alloy::primitives::U256;
use dashmap::DashMap;
use degenbot_core::diag;
use degenbot_pools::AffectedKey;
use degenbot_substrate::resolve::resolve_hops;
use degenbot_substrate::resolve::HopProjectionCache;
use degenbot_substrate::{BlockMetadata, BotState, EpochDelta};
use hashbrown::{HashMap, HashSet};
use std::sync::Arc;

mod deliver;
mod merge;
mod resolve;
mod shed;
mod solve;

/// The clamp/merge bodies live in `merge.rs` (ADR-076 D1). The clamp body is
/// production-reachable through this re-export (the SIMPIPE2 worker and
/// `solve_all`); the two test-only doors are re-exported under `cfg(test)` so a
/// non-test build carries no unreachable import.
pub(crate) use merge::clamp_result_with_state;
#[cfg(test)]
pub(crate) use merge::{clamp_skip_kind, recompute_clamped_profit};

/// The solve-cycle block scope: the ONE owner of the block boundary.
///
/// It holds the engine-owned walk-composition memo, walk-telemetry collector,
/// and prefix-composition cache handles and owns [`advance`](Self::advance) —
/// the memo's drain-and-advance in ONE call, returning the epoch that just
/// ended — plus the block number every per-path `GateDeps::per_block` is built
/// from. Both epoch notions deliberately share ONE block number: the memo's
/// census epoch and the prefix cache's generation are the same solved block,
/// so a block's deps can never disagree with the memo's epoch. Keeping the
/// collector here too means every per-block handle a path's deps needs lives
/// in one place.
#[derive(Clone)]
pub(crate) struct BlockScope {
    walk_memo: Arc<::degenbot_pools::WalkMemo>,
    walk_telemetry: Arc<::degenbot_pools::WalkTelemetry>,
    prefix_cache: Arc<::degenbot_pools::PrefixCache>,
    epoch: u64,
}

impl BlockScope {
    /// A fresh scope at epoch 0 over the given walk-composition memo and
    /// walk-telemetry collector handles.
    pub(crate) fn new(
        walk_memo: Arc<::degenbot_pools::WalkMemo>,
        walk_telemetry: Arc<::degenbot_pools::WalkTelemetry>,
    ) -> Self {
        Self {
            walk_memo,
            walk_telemetry,
            prefix_cache: Arc::new(::degenbot_pools::PrefixCache::new()),
            epoch: 0,
        }
    }

    /// Advance the scope to `block`: drain the memo's epoch that just ended
    /// (returning its stats) and move the memo's census forward by ONE
    /// boundary. The scope's epoch — read by every per-path
    /// `GateDeps::per_block` — is the same `block`.
    #[must_use]
    pub(crate) fn advance(&mut self, block: u64) -> ::degenbot_pools::WalkMemoStats {
        self.epoch = block;
        self.walk_memo.begin_block(block)
    }

    /// The block every per-path gate deps value is built from.
    #[must_use]
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The engine-owned walk-composition memo handle.
    #[must_use]
    pub(crate) fn walk_memo(&self) -> &Arc<::degenbot_pools::WalkMemo> {
        &self.walk_memo
    }

    /// The engine-owned walk telemetry collector handle.
    #[must_use]
    pub(crate) fn walk_telemetry(&self) -> &Arc<::degenbot_pools::WalkTelemetry> {
        &self.walk_telemetry
    }

    /// The engine-owned prefix-composition cache handle.
    #[must_use]
    pub(crate) fn prefix_cache(&self) -> &Arc<::degenbot_pools::PrefixCache> {
        &self.prefix_cache
    }
}

/// `DEGENBOT_SOLVE_INLINE_SIM` —
/// stance semantics unchanged; it is the `SolveCycle` instance
/// value `inline_sim_enabled`, never a process static.
/// Per-cycle shared solve context: everything the
/// per-path dispatch touches besides the resolved snapshot. Bundled once
/// per cycle so a worker handle is static for the dedicated-executor
/// arm; the caller retains its own Arc for the drain + tail telemetry.
pub(crate) struct SolveCycleShared {
    pub(crate) solve_block: u64,
    pub(crate) gate_capture: Option<DegeneratePathFileSink>,
    /// The block scope the per-path gate deps are built from: its epoch IS
    /// the memo's epoch, so a path's deps can never disagree with the memo.
    pub(crate) scope: BlockScope,
    /// The pre-solve profitability floor as an instance value, set at
    /// construction from `cfg.solve.min_profit_wei`.
    pub(crate) min_profit: U256,
    /// The concentrated-liquidity-hop clamp margin (wei) as an instance value, packed from
    /// `solve.clamp_margin_wei` through `EngineRetune` — never a process
    /// static read.
    pub(crate) clamp_margin: U256,
    /// the instance-scoped solver runtime stance, threaded down —
    /// the solve tree has no process-global config anymore.
    pub(crate) runtime: ::degenbot_pools::SolveRuntimeConfig,
    /// Every per-path cost sink — the pipeline instruments, the K-slowest
    /// retention, the two capture hooks, and the next-cycle LPT cost history —
    /// behind ONE record call from the lane walk, plus the ONE typed snapshot
    /// the bin packer reads (commits 2-3 of PR C4).
    pub(crate) path_cost_ledger: super::path_cost_ledger::PathCostLedger,
    /// Test-only deterministic per-path delay hook.
    #[cfg(test)]
    pub(crate) test_solve_delay: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
    /// test-only per-path PANIC hook — a bin body that
    /// dies mid-walk so the breaker suite can pin the detached arm's
    /// witness (typed Failed records) and gauge pairing through a panic.
    #[cfg(test)]
    pub(crate) test_solve_panic: Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>,
    /// SIMPIPE2 T2: the shared core (Arc-cloned from the engine at cycle
    /// build) — the WORKER-side clamp takes the same short core read the
    /// merge-site clamp took; no engine state is touched (
    /// engine-then-core ordering, short read, no guard across awaits).
    pub(crate) core: std::sync::Arc<degenbot_substrate::state_lock::StateLock<BotState>>,
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

#[cfg(test)]
impl SolveCycleShared {
    /// The white-box test constructor: the fixture sites vary only these
    /// fields; everything else takes the cold-cycle default (zero epoch, no
    /// captures, empty walk/prefix memos, zero min-profit, default runtime
    /// stance, a fresh ledger whose block mirrors `solve_block`, no test
    /// hooks). Keeps the three fixtures from hand-assembling the full literal.
    pub(crate) fn for_test(
        solve_block: u64,
        metadata: BlockMetadata,
        core: std::sync::Arc<degenbot_substrate::state_lock::StateLock<BotState>>,
        pool_refs: Vec<std::sync::Arc<MixedPath>>,
        worker_clamp: bool,
        inline_sim: Option<std::sync::Arc<dyn crate::arb_engine::inline_sim::InlineSimulator>>,
    ) -> Self {
        Self {
            solve_block,
            metadata,
            gate_capture: None,
            scope: BlockScope::new(
                std::sync::Arc::new(::degenbot_pools::WalkMemo::new(false, false)),
                std::sync::Arc::new(::degenbot_pools::WalkTelemetry::new()),
            ),
            min_profit: U256::ZERO,
            clamp_margin: U256::from(1u128),
            runtime: ::degenbot_pools::SolveRuntimeConfig::default(),
            path_cost_ledger: super::path_cost_ledger::PathCostLedger::new(
                solve_block,
                None,
                None,
                std::sync::Arc::new(super::path_cost_ledger::PathCostHistory::new()),
            ),
            #[cfg(test)]
            test_solve_delay: None,
            #[cfg(test)]
            test_solve_panic: None,
            core,
            pool_refs,
            worker_clamp,
            inline_sim,
        }
    }
}
/// The cycle-transient stash: the stashes a cycle carries that are NOT the
/// epoch's own work — the arm latch the white-box probes and latency
/// histograms name AFTER the cycle returns, the eager-registration carry
/// awaiting the next merge, and the same-state reuse counter of the cycle just
/// run.
///
/// They live behind one module-private value with a cold [`Default`], so the
/// engine's construction stands them up without the fields becoming readable
/// outside this module. The dispatch verdict is NOT here: it rides the
/// [`CycleTicket`] the draw mints and the epoch consumes.
#[derive(Default)]
pub(crate) struct CycleTransients {
    /// The typed latch of the most recent cycle's arm (ADR-045 T5). The
    /// string stash is gone: [`CycleOutcome`] is authoritative and the stage
    /// hook records `cycle.arm` straight off it. This latch survives only so
    /// the white-box probes and the latency histograms can name the arm AFTER
    /// the cycle returns. `None` = no cycle dispatched yet (`"unset"`).
    last_arm: Option<CycleArm>,
    /// Paths registered via `register_and_solve_path` that have been eagerly
    /// solved and appended to `results`, awaiting the next dirty-cycle merge.
    pending_new_paths: HashSet<u64>,
    /// Reuse-eligibility counter for the current solve cycle (probe only;
    /// reset each `solve_dirty` and surfaced on the resolve event).
    paths_same_state_this_cycle: u64,
}

/// The cycle ticket: the ONE carrier of a cycle's transient draw facts from
/// the resolve that drew them to the epoch that consumes them.
///
/// The admission draw is the single consumption decision of the epoch ledger
/// ([`EpochDelta`]), and its verdict — a zero budget SHEDS the cycle — used to
/// live on the cycle as a `bool` written by the resolve call and read by the
/// epoch in a LATER call. That is the retired pending-new-paths disease:
/// engine state a later call can consult after the decision it describes is
/// gone. The ticket makes the handoff a value instead. [`SolveCycle::draw`]
/// mints it, the Resolved stage row carries it, and
/// [`SolveCycle::run_epoch`] consumes it, so a verdict cannot outlive the
/// cycle it belongs to (the module docs carry the compile-fail probe).
///
/// The ticket is linear: no `Clone`, no constructor outside the draw, fields
/// module-private. The drawn ledger keys have a white-box read; the verdict
/// has NO accessor at all, so the epoch's consume is its only reader.
pub(crate) struct CycleTicket {
    /// The drawn ledger keys, freshest-first (the epoch's affected set).
    affected: Vec<AffectedKey>,
    /// The draw's consumption verdict: a zero budget SHED this cycle.
    draw_zero: bool,
}

impl CycleTicket {
    /// Whether the draw carried no keys at all.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.affected.is_empty()
    }
}

#[cfg(test)]
impl CycleTicket {
    /// The drawn ledger keys, freshest-first — the white-box read of what the
    /// draw handed over (production consumes the ticket whole).
    #[must_use]
    pub(crate) fn affected(&self) -> &[AffectedKey] {
        &self.affected
    }

    /// The white-box constructor behind the sanctioned test door
    /// ([`super::test_harness`]): a direct engine drive has no stage row to
    /// carry the ticket, so it mints the value the draw would have minted
    /// (a non-admission draw never sheds).
    pub(crate) fn for_test(affected: Vec<AffectedKey>) -> Self {
        Self {
            affected,
            draw_zero: false,
        }
    }
}

/// The solve cycle's owned state .
///
/// The resolve companions, the cycle-transient stash, the solve output,
/// the admissions stance, the detached-arm collaborator, the
/// walk/projection recorders, and the construction-time test knobs. The T4
/// interface (`draw` / `run_epoch` / `register_path` /
/// `register_and_solve_path` / `solve_all_paths` / `merge_detached_item` /
/// `forget`) now lands its bodies here.
///
/// The engine holds this as `engine.cycle`; the white-box tests re-index
/// mechanically to `engine.cycle.*`. The test knobs are data, not methods
/// (ADR-076 D2): a test writes the field it varies, and [`run_epoch`] reads
/// them through [`EpochContext`].
///
/// The cycle-transient stash is module-private: the draw verdict lives in the
/// [`CycleTicket`] the draw mints (never here), and the remaining stashes are
/// reachable only through this module's own doors.
#[expect(clippy::struct_excessive_bools)] // construction stances (projection memo, admission, streaming, resolve-par) — distinct stances, not flag soup
pub(crate) struct SolveCycle {
    // --- Resolve companions -------------------------------------------
    /// Resolved path states (mutated on each solve). Entries are Arc-shared
    /// into the parallel solve dispatch (f701ccd3 staging fix) — immutable
    /// between resolve passes, so staging is refcount bumps, not deep clones
    /// of the concentrated liquidity tick-range sequences.
    pub(crate) path_resolved: HashMap<u64, Arc<ResolvedMixedPath>>,
    /// Path solve-eligibility state machine: per registered path the
    /// [`PathSolveStatus`] that decides whether a dirty-pool fan-out must
    /// (re)resolve it. Replaces the scattered `valid` bool + ad-hoc skip rules.
    pub(crate) path_status: HashMap<u64, PathSolveStatus>,
    /// Per-path snapshot of every hop's `pool_update_block` at the last
    /// successful resolve. A byte-identical snapshot on the next cycle means
    /// the whole solve intake (all hop states) is unchanged.
    pub(crate) resolved_update_snapshot: HashMap<u64, Vec<u64>>,
    /// Hop-projection memo (pool,direction) -> snapshot@nonce. Shared across
    /// all resolve call sites so a dirty pool's tick walk runs once per state
    /// change and serves every referencing path from the cache.
    pub(crate) hop_projection_cache: HopProjectionCache,
    /// Monotonic count of actual family projections (cache misses). Test-
    /// observable; emitted on the solve-phase resolve event.
    pub(crate) hop_projection_count: u64,
    /// Fused hop-projection memo switch: resolved
    /// ONCE at construction.
    pub(crate) concentrated_liquidity_projection_memo: bool,
    /// Telemetry string cache: path id -> formatted hop description, built
    /// once at first emission (paths are immutable after registration).
    pub(crate) path_description_cache: parking_lot::Mutex<HashMap<u64, Arc<str>>>,
    /// the ONE engine block cursor — the consolidated owner of the
    /// block-coordinate residue. Every advance rule lives on the cursor, and
    /// its ONE read door hands back a whole
    /// [`BlockCoordinates`](super::block_cursor::BlockCoordinates) reading
    /// rather than four separate pokes.
    pub(crate) cursor: BlockCursor,
    /// The cycle's privatized transient stash (see [`CycleTransients`]): the
    /// stashes a cycle carries that are not the epoch's own work. The dispatch
    /// verdict itself never lands here — the draw mints it on the
    /// [`CycleTicket`] the Resolved row carries, and the epoch consumes it.
    ///
    /// The field is reachable so the engine's construction can stand the cold
    /// stash up; its CONTENTS stay private to this module, so no sibling can
    /// read a latch or the carry.
    pub(crate) transients: CycleTransients,
    // --- Solve output -------------------------------------------------
    /// Last solved results, keyed by path ID for O(1) updates.
    ///
    /// Engine-shard T1: sharded into a `DashMap` so Python
    /// `latest_results` reads never park behind the drain-lock held engine
    /// `Mutex`.
    pub(crate) results: DashMap<u64, SolvePathResult>,
    /// SIMPIPE2 T3: the per-path inline payloads resolved in the solve
    /// workers (SIMPIPE2 T2's off-lock seam). Keyed by path id.
    pub(crate) inline_payloads: DashMap<u64, SimulatedPathResult>,
    // --- Admissions stance --------------------------------------------
    /// Construction-time admission stance (`DEGENBOT_SOLVE_ADMISSION`,
    /// default OFF for the experiment).
    pub(crate) solve_admission: bool,
    /// The un-merged-result pipe depth target in KEYS, clamped at
    /// construction to `1..=detached_cycle::DETACHED_INFLIGHT_CAP`.
    pub(crate) admission_target_depth: u64,
    /// The retained (carried) key retention window W in blocks.
    pub(crate) admission_retention_blocks: u64,
    // --- Collaborators / recorders ------------------------------------
    /// THE one solve-arm machine: the per-cycle states, the merge
    /// pipe, the gauge pair, the seq counters, the ledger door, and the
    /// disposition counters. See [`DetachedCycle`].
    pub(crate) detached_cycle: DetachedCycle,
    /// The block scope: the ONE owner of the block boundary. Holds the
    /// engine-owned walk-composition memo and prefix-composition cache and
    /// owns `advance(block)` — the memo's drain-and-advance plus the epoch
    /// every per-path gate deps value is built from.
    pub(crate) scope: BlockScope,
    /// The pre-solve profitability floor. A packed instance
    /// value, never a process static.
    pub(crate) min_profit_floor: U256,
    /// The concentrated-liquidity-hop clamp margin packed from the typed config
    /// (`solve.clamp_margin_wei`) via `EngineRetune`.
    pub(crate) clamp_margin: U256,
    /// The inline-sim stance as an engine instance value (documented stance
    /// semantics unchanged, SIMPIPE2 T2/T4).
    pub(crate) inline_sim_enabled: bool,
    /// Per-path previous-block MEASURED cost (walk sims + gate µs, written by
    /// the walk's ledger `record` after each solve; ONE typed snapshot read at
    /// bin construction). Refines the LPT makespan predictor for stable pool
    /// shapes (loop-12 lab); the gate term covers gate-heavy paths that carry
    /// sims≈0 and were bin-packed cheap (loop-18).
    pub(crate) path_cost_history: Arc<super::path_cost_ledger::PathCostHistory>,
    /// T3: emit each clamp-passed above-threshold result as an
    /// IMMEDIATE single-entry [`ResultBatch`] during the drain instead of
    /// waiting for the pump debounce. Construction-time stance.
    pub(crate) streaming_delivery: bool,
    /// the deferred-path re-record hook (the ledger carry for
    /// `paths.deferred_future_price` deferrals).
    pub(crate) deferred_re_record: Option<DeferredReRecordHook>,
    /// the chunked-parallel resolve stance as an instance value.
    pub(crate) resolve_par_stance: bool,
    // --- Shared dependencies (ADR-045 T4: the cycle drives resolve/solve) ---
    /// The shared `BotState` handle (a clone of the engine's Arc; ADR-006 D1).
    pub(crate) core: Arc<degenbot_substrate::state_lock::StateLock<degenbot_substrate::BotState>>,
    /// Config + the instance solver runtime stance (immutable after
    /// construction), and the inline-sim hook (kept in sync by
    /// `set_inline_simulator`).
    pub(crate) cfg: Arc<::degenbot_config::BotConfig>,
    pub(crate) runtime_cfg: ::degenbot_pools::SolveRuntimeConfig,
    pub(crate) inline_sim: Option<Arc<dyn super::inline_sim::InlineSimulator>>,
    // --- construction-time test knobs (ADR-076 D2: data, not methods) ---
    /// Test-only: hook invoked at the start of each path solve.
    #[cfg(test)]
    pub(crate) test_solve_delay: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// test-only per-path PANIC hook.
    #[cfg(test)]
    pub(crate) test_solve_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Diagnostic override: force the future-price deferral for these
    /// path ids (the `EngineRetune::force_deferred` field). Production always
    /// leaves it `None`; the real tripwire is otherwise unreachable after the
    /// solve-anchor head floor.
    pub(crate) force_deferred: Option<HashSet<u64>>,
    /// Test-only: the drain appends each merged path id here.
    #[cfg(test)]
    pub(crate) merge_probe: Option<Arc<parking_lot::Mutex<Vec<u64>>>>,
    /// test-only per-path PANIC hook for the MERGE seat.
    #[cfg(test)]
    pub(crate) test_merge_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Test harness: when ON (default), a DIRECT
    /// `rebuild_and_solve_affected` / `solve_dirty` call merges its own
    /// just-enqueued detached pipe INLINE.
    #[cfg(test)]
    pub(crate) test_sync_merge: bool,
    /// Test harness: the inline drain's cached handle on the merge pipe.
    #[cfg(test)]
    pub(crate) test_merge_rx:
        Option<std::sync::mpsc::Receiver<crate::arb_engine::executor::LaneOutcome>>,
}

/// The epoch's explicit workspace (ADR-076 D1/D2): the four state clusters
/// [`SolveCycle::run_epoch`] mutates — status, admission, caches,
/// results/config — plus the epoch inputs and the collaborators the epoch
/// reaches. It is built once at the top of the epoch and handed to each phase
/// by `&mut` (ADR-076 D1); the clusters themselves stay flat on
/// [`SolveCycle`] (the engine's lifecycle surface reads them too), so the
/// context borrows them for exactly one epoch rather than moving them.
///
/// Construction stances that cannot change inside an epoch are copied in, so
/// the phase bodies carry no `&`/`*` noise; the mutable clusters, the
/// admission stashes, and the pending-new carry are borrowed. The carry is a
/// field HERE (ADR-076 D3): the epoch consumes it through the context, and no
/// `&mut` to it crosses the seam.
#[expect(clippy::struct_excessive_bools)] // the packed construction stances (admission, projection memo, inline sim, resolve-par)
pub(crate) struct EpochContext<'a> {
    // --- status cluster -------------------------------------------------
    /// Path solve-eligibility state machine (the dirty-pool fan-out gate).
    pub(crate) path_status: &'a mut HashMap<u64, PathSolveStatus>,
    /// Resolved path states (Arc-shared into the solve dispatch).
    pub(crate) path_resolved: &'a mut HashMap<u64, Arc<ResolvedMixedPath>>,
    /// The ONE engine block cursor.
    pub(crate) cursor: &'a mut BlockCursor,
    // --- admission cluster ----------------------------------------------
    /// Construction-time admission stance (`DEGENBOT_SOLVE_ADMISSION`).
    pub(crate) solve_admission: bool,
    /// The un-merged-result pipe depth target in KEYS.
    pub(crate) admission_target_depth: u64,
    /// THE one solve-arm machine (per-cycle states, merge pipe, gauges).
    pub(crate) detached_cycle: &'a mut DetachedCycle,
    /// The pending-new carry: paths eagerly solved at registration, awaiting
    /// the next dirty-cycle merge.
    pub(crate) pending_new_paths: &'a mut HashSet<u64>,
    /// Reuse-eligibility counter for the current solve cycle (probe only).
    pub(crate) paths_same_state_this_cycle: &'a mut u64,
    /// The deferred-path re-record hook (the ledger carry for deferrals).
    pub(crate) deferred_re_record: &'a Option<DeferredReRecordHook>,
    /// The diagnostic force-deferred override (`EngineRetune::force_deferred`).
    pub(crate) force_deferred: &'a Option<HashSet<u64>>,
    // --- caches cluster --------------------------------------------------
    /// Per-path snapshot of every hop's `pool_update_block` at the last
    /// successful resolve.
    pub(crate) resolved_update_snapshot: &'a mut HashMap<u64, Vec<u64>>,
    /// Hop-projection memo (pool,direction) -> snapshot@nonce.
    pub(crate) hop_projection_cache: &'a HopProjectionCache,
    /// Fused hop-projection memo switch (resolved ONCE at construction).
    pub(crate) concentrated_liquidity_projection_memo: bool,
    // --- results/config cluster ------------------------------------------
    /// Last solved results, keyed by path ID.
    pub(crate) results: &'a DashMap<u64, SolvePathResult>,
    /// Config (immutable after construction) and the instance solver runtime
    /// stance threaded down from it.
    pub(crate) cfg: &'a Arc<::degenbot_config::BotConfig>,
    pub(crate) runtime_cfg: ::degenbot_pools::SolveRuntimeConfig,
    /// Per-path previous-block MEASURED cost (the LPT cost refinement).
    pub(crate) path_cost_history: &'a Arc<super::path_cost_ledger::PathCostHistory>,
    /// The pre-solve profitability floor (packed instance value).
    pub(crate) min_profit_floor: U256,
    /// The concentrated-liquidity-hop clamp margin (packed instance value).
    pub(crate) clamp_margin: U256,
    /// The inline-sim stance (instance value).
    pub(crate) inline_sim_enabled: bool,
    /// The engine's inline-sim hook snapshot.
    pub(crate) inline_sim: &'a Option<Arc<dyn super::inline_sim::InlineSimulator>>,
    /// The chunked-parallel resolve stance (instance value).
    pub(crate) resolve_par_stance: bool,
    // --- phase collaborators (ADR-076 D1: the solve/merge/deliver phases) ---
    /// The inline-sim payloads the delivery diff ships with the batch.
    pub(crate) inline_payloads: &'a DashMap<u64, SimulatedPathResult>,
    /// The streaming single-result delivery stance (instance value).
    pub(crate) streaming_delivery: bool,
    /// The telemetry hop-description string cache (path id -> rendered hops).
    pub(crate) path_description_cache: &'a parking_lot::Mutex<HashMap<u64, Arc<str>>>,
    /// Test-only merge witness (the merged-path order).
    #[cfg(test)]
    pub(crate) merge_probe: &'a Option<Arc<parking_lot::Mutex<Vec<u64>>>>,
    /// Test-only per-path merge panic hook (the sidecar's `catch_unwind` pin).
    #[cfg(test)]
    pub(crate) test_merge_panic: &'a Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Test-only per-path solve delay hook.
    #[cfg(test)]
    pub(crate) test_solve_delay: &'a Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Test-only per-path solve panic hook.
    #[cfg(test)]
    pub(crate) test_solve_panic: &'a Option<Arc<dyn Fn(u64) + Send + Sync>>,
    // --- epoch collaborators (not a cluster; the epoch still reaches them) --
    /// The block scope: the ONE owner of the block boundary.
    pub(crate) scope: &'a mut BlockScope,
    /// The shared `BotState` handle (a clone of the engine's Arc).
    pub(crate) core:
        &'a Arc<degenbot_substrate::state_lock::StateLock<degenbot_substrate::BotState>>,
    /// Monotonic count of actual family projections (cache misses).
    pub(crate) hop_projection_count: &'a mut u64,
    /// The registered-path identity registry (read-only inside an epoch).
    pub(crate) registry: &'a PathRegistry,
    /// The cycle's block metadata (the sim request's block env).
    pub(crate) metadata: &'a BlockMetadata,
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

/// The uncached hop-description render for one path: the ONE body behind
/// [`SolveCycle::describe_path`] and the epoch's cached door. A free function
/// so the epoch can render a path while the context holds `&mut` to other
/// fields (a `&self` method call would borrow the whole cycle).
fn describe_path_with(
    core: &Arc<degenbot_substrate::state_lock::StateLock<BotState>>,
    registry: &PathRegistry,
    path_id: u64,
) -> String {
    let Some(path) = registry.get(path_id) else {
        return format!("path_id={path_id} (unregistered)");
    };
    let core = core.read_at(degenbot_substrate::state_lock::LockSite::Solver);
    let hops: Vec<String> = path
        .pools
        .iter()
        .map(|r| describe_hop(&core, r.hop_type, r.pool_key, r.zero_for_one))
        .collect();
    format!("path_id={path_id} [{}]", hops.join(" -> "))
}

/// The telemetry string-cache door (path id -> formatted hop description,
/// built once at first emission). Same lock discipline as the method it backs:
/// a lookup lock, then an insert lock.
fn describe_path_cached_with(
    cache: &parking_lot::Mutex<HashMap<u64, Arc<str>>>,
    core: &Arc<degenbot_substrate::state_lock::StateLock<BotState>>,
    registry: &PathRegistry,
    path_id: u64,
) -> Arc<str> {
    if let Some(hit) = cache.lock().get(&path_id) {
        return Arc::clone(hit);
    }
    let rendered: Arc<str> = Arc::from(describe_path_with(core, registry, path_id));
    cache.lock().insert(path_id, Arc::clone(&rendered));
    rendered
}

// ===========================================================================
// ADR-045 T4: the moved solve-cycle behavior. Bodies moved verbatim from
// the retired grab file / `lifecycle.rs`; only the receiver (`self` is the
// cycle) and the identity/delivery borrows changed. Lock discipline, span
// names and event names are byte-identical.
// ===========================================================================
impl SolveCycle {
    /// The CURRENT cycle's dispatch arm (ADR-045 T5): the latency
    /// histograms' `arm` label, read by the caller that observes the cycle.
    /// Backed by the typed `last_arm` latch, not a string stash; the stage
    /// hook reads the `CycleOutcome` directly for its own span/duration.
    /// Moved here from `event_routing.rs::ArbitrageEngine::cycle_arm` — the machine owns the cycle state. Test-only: the
    /// production stage hook attributes the arm from `CycleOutcome`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cycle_arm(&self) -> &'static str {
        self.transients.last_arm.map_or("unset", |arm| arm.label())
    }
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
    /// The epoch's explicit workspace (ADR-076 D1/D2): the four state
    /// clusters plus the epoch inputs, borrowed for exactly one epoch.
    ///
    /// The merge doors build it here. `run_epoch` spells the same literal
    /// inline instead: the shed arm still takes a disjoint `&mut` to the
    /// cycle's arm latch, and only field expressions (not a `&mut self`
    /// method) can borrow two cycle fields at once.
    fn epoch_context<'a>(
        &'a mut self,
        registry: &'a PathRegistry,
        metadata: &'a BlockMetadata,
    ) -> EpochContext<'a> {
        EpochContext {
            path_status: &mut self.path_status,
            path_resolved: &mut self.path_resolved,
            cursor: &mut self.cursor,
            solve_admission: self.solve_admission,
            admission_target_depth: self.admission_target_depth,
            detached_cycle: &mut self.detached_cycle,
            pending_new_paths: &mut self.transients.pending_new_paths,
            paths_same_state_this_cycle: &mut self.transients.paths_same_state_this_cycle,
            deferred_re_record: &self.deferred_re_record,
            force_deferred: &self.force_deferred,
            resolved_update_snapshot: &mut self.resolved_update_snapshot,
            hop_projection_cache: &self.hop_projection_cache,
            concentrated_liquidity_projection_memo: self.concentrated_liquidity_projection_memo,
            results: &self.results,
            cfg: &self.cfg,
            runtime_cfg: self.runtime_cfg,
            path_cost_history: &self.path_cost_history,
            min_profit_floor: self.min_profit_floor,
            clamp_margin: self.clamp_margin,
            inline_sim_enabled: self.inline_sim_enabled,
            inline_sim: &self.inline_sim,
            resolve_par_stance: self.resolve_par_stance,
            scope: &mut self.scope,
            core: &self.core,
            hop_projection_count: &mut self.hop_projection_count,
            inline_payloads: &self.inline_payloads,
            streaming_delivery: self.streaming_delivery,
            path_description_cache: &self.path_description_cache,
            #[cfg(test)]
            merge_probe: &self.merge_probe,
            #[cfg(test)]
            test_merge_panic: &self.test_merge_panic,
            #[cfg(test)]
            test_solve_delay: &self.test_solve_delay,
            #[cfg(test)]
            test_solve_panic: &self.test_solve_panic,
            registry,
            metadata,
        }
    }

    /// The merge of ONE solved result (ADR-076 D1): the body lives in
    /// `merge.rs`. The sidecar reaches the body through `merge_detached_item`;
    /// this door serves the clamp/merge tests that merge one result directly.
    #[cfg(test)]
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
        let ctx = self.epoch_context(registry, metadata);
        merge::merge_one_result(
            &ctx,
            solve_block,
            metadata,
            pid,
            result,
            worker_clamp_twins,
            payload,
            registry,
            delivery,
        )
    }

    /// The detached-merge door (ADR-076 D1): the body lives in `merge.rs`. The
    /// sidecar hands one lane outcome per engine hold, and the epoch workspace
    /// is built per item exactly as the epoch's own merge would.
    pub(crate) fn merge_detached_item(
        &mut self,
        item: LaneOutcome,
        registry: &PathRegistry,
        delivery: &mut DeliveryPolicy,
    ) {
        let metadata = BlockMetadata::default();
        let mut ctx = self.epoch_context(registry, &metadata);
        merge::merge_detached_item(&mut ctx, item, registry, delivery);
    }

    /// The `&self` clamp door (ADR-076 D1): the body lives in `merge.rs`. The
    /// epoch reaches the same body through the context's core/margin; this
    /// accessor serves the callers that hold no context.
    pub(crate) fn clamp_concentrated_liquidity_hop_capacity(
        &self,
        path_id: u64,
        result: &mut SolvePathResult,
        registry: &PathRegistry,
    ) -> u64 {
        merge::clamp_concentrated_liquidity_hop_capacity(
            &self.core,
            self.clamp_margin,
            path_id,
            result,
            registry,
        )
    }

    /// The epoch over ONE cycle ticket.
    ///
    /// The ticket is consumed here, by value: the drawn keys ARE the epoch's
    /// affected set and the draw verdict it carries is the only thing the
    /// dispatch acts on, so no cycle stash survives for a later call to read.
    ///
    /// The composition spine (ADR-076 D1): build the epoch's [`EpochContext`],
    /// then shed/resolve -> solve -> merge -> deliver. Every phase body lives
    /// in its own submodule; the spine owns only the phase order, the arm, and
    /// the census the [`CycleOutcome`] carries.
    #[expect(clippy::too_many_lines)]
    pub(crate) fn run_epoch(
        &mut self,
        ticket: CycleTicket,
        block_number: u64,
        metadata: &BlockMetadata,
        registry: &PathRegistry,
        delivery: &mut DeliveryPolicy,
    ) -> CycleOutcome {
        let CycleTicket {
            affected,
            draw_zero,
        } = ticket;
        let _ = &mut *delivery;
        // The epoch's explicit workspace (ADR-076 D1/D2): the four state
        // clusters plus the epoch inputs, borrowed for exactly this epoch.
        // Every phase reads and mutates THROUGH the context, so the phase
        // bodies live in their own submodules without moving ownership of the
        // cycle's flat state (the engine's lifecycle surface reads it too).
        let mut ctx = EpochContext {
            path_status: &mut self.path_status,
            path_resolved: &mut self.path_resolved,
            cursor: &mut self.cursor,
            solve_admission: self.solve_admission,
            admission_target_depth: self.admission_target_depth,
            detached_cycle: &mut self.detached_cycle,
            pending_new_paths: &mut self.transients.pending_new_paths,
            paths_same_state_this_cycle: &mut self.transients.paths_same_state_this_cycle,
            deferred_re_record: &self.deferred_re_record,
            force_deferred: &self.force_deferred,
            resolved_update_snapshot: &mut self.resolved_update_snapshot,
            hop_projection_cache: &self.hop_projection_cache,
            concentrated_liquidity_projection_memo: self.concentrated_liquidity_projection_memo,
            results: &self.results,
            cfg: &self.cfg,
            runtime_cfg: self.runtime_cfg,
            path_cost_history: &self.path_cost_history,
            min_profit_floor: self.min_profit_floor,
            clamp_margin: self.clamp_margin,
            inline_sim_enabled: self.inline_sim_enabled,
            inline_sim: &self.inline_sim,
            resolve_par_stance: self.resolve_par_stance,
            scope: &mut self.scope,
            core: &self.core,
            hop_projection_count: &mut self.hop_projection_count,
            inline_payloads: &self.inline_payloads,
            streaming_delivery: self.streaming_delivery,
            path_description_cache: &self.path_description_cache,
            #[cfg(test)]
            merge_probe: &self.merge_probe,
            #[cfg(test)]
            test_merge_panic: &self.test_merge_panic,
            #[cfg(test)]
            test_solve_delay: &self.test_solve_delay,
            #[cfg(test)]
            test_solve_panic: &self.test_solve_panic,
            registry,
            metadata,
        };
        // worker threads (executor bins, resolve chunk threads,
        // solve executor's workers, AND the detached solve-bin std-threads)
        // have no ambient tracing context — any span emitted inside a
        // dispatch closure would orphan into a root trace. Capture the
        // caller's span (the drainer's `degenbot.arb.solve`) once and
        // re-enter it per work item; the detached arm additionally carries
        // it into the detached carrier so the sidecar's merges inherit it
        //.
        let solve_span = tracing::Span::current();
        // Phase timing so a multi-second solve EXPLAINS
        // itself — fan-out / resolve / par-solve / clamp are separate events,
        // and the K slowest paths name where the wall-clock went.
        let cycle_start = std::time::Instant::now();
        let mut affected_path_ids = resolve::scan_dirty_status(&mut ctx, &affected);
        let anchor = resolve::resolve_anchor(&ctx, block_number);
        let solve_block = anchor.block();
        // Cross-block walk-composition census, in ONE scope call: `advance`
        // drains the epoch that just ended (the memo owns drain-then-advance
        // by signature) and moves the scope's epoch to `solve_block` — the
        // same block every per-path gate deps value reads, so a block's deps
        // can never disagree with the memo's epoch. The returned stats go
        // straight to the telemetry renderer. The tap is inert on the first
        // boundary of a run and on a disabled memo (the zero-activity gate —
        // see `super::walk_telemetry`).
        let stats = ctx.scope.advance(solve_block);
        super::walk_telemetry::emit_epoch_boundary(&stats, solve_block);
        // -----------------------------------------------------------------
        // ADMISSION DRAW: the ticket consumed above carries the SINGLE
        // consumption decision the DRAW already made in `on_resolve` — the
        // dispatch NEVER re-reads the live gauge (two reads could disagree, and
        // a fresh zero would discard keys the draw already removed from the
        // ledger). A zero-budget draw consumed nothing, so a
        // shed is genuine: this cycle submits NOTHING. The response lives at
        // the draw/cycle level, never the in-cycle dispatch. The machine is
        // NOT consulted for a shed cycle (no `begin_cycle`, no seq tick, no
        // submission), so no transition row is exercised and no typed
        // RejectionReason can trip; the solved-block cursor advances exactly
        // like the `skipped_empty` bookkeeping pass, and the keys stay in the
        // ledger for a later cycle (carry). The check precedes the
        // pending-new-path merge so a shed never clears work it did not do.
        // -----------------------------------------------------------------
        if ctx.solve_admission && draw_zero {
            return shed::shed_epoch(
                &mut ctx,
                &mut self.transients.last_arm,
                &affected,
                &affected_path_ids,
                solve_block,
            );
        }
        // Also re-solve any paths registered via register_and_solve_path that
        // haven't been through rebuild_and_solve_affected yet. These paths were
        // eagerly solved at registration time, but the pump's process_block
        // replaces self.results entirely — so we must include them to avoid
        // dropping their results.
        affected_path_ids.extend(&*ctx.pending_new_paths);
        ctx.pending_new_paths.clear();
        // If no paths are affected, just update the block number
        if affected_path_ids.is_empty() {
            return shed::skipped_empty_epoch(
                &mut ctx,
                &mut self.transients.last_arm,
                &affected,
                solve_block,
            );
        }
        solve::fanout_telemetry(
            &ctx,
            &affected,
            &affected_path_ids,
            solve_block,
            cycle_start,
        );
        let resolve::ResolvePhase {
            same_state: same_state_total,
            projections: projections_total,
            deferred: deferred_paths,
        } = resolve::resolve_epoch(
            &mut ctx,
            &anchor,
            solve_block,
            &affected_path_ids,
            cycle_start,
        );
        let solve::SolvePhase {
            staged,
            invalid,
            deferred,
            bins,
            seq,
        } = solve::solve_epoch(
            &mut ctx,
            &affected_path_ids,
            &deferred_paths,
            solve_block,
            cycle_start,
            &solve_span,
        );
        // Test harness: direct `rebuild_and_solve_affected` /
        // `solve_dirty` callers (the resolve/solve unit tests) merge the
        // just-enqueued pipe INLINE through the sidecar's own per-item
        // merge path, so they keep reading results synchronously. The
        // production path (EngineStages) leaves `test_sync_merge` OFF.
        #[cfg(test)]
        if self.test_sync_merge {
            self.drain_merge_inline(staged, registry, delivery);
        }
        // monotone advance on the block cursor.
        self.cursor.advance_solved(solve_block);
        // ENQUEUE-END semantics (T2 acceptance: "return is enqueue-end,
        // not apply-end"): the engine Mutex hold ENDS here; the sidecar
        // re-acquires it per merged straggler.
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
        let arm = if staged == 0 {
            CycleArm::Dissolved {
                invalid: usize::try_from(invalid).unwrap_or(usize::MAX),
                deferred_future_price: deferred,
            }
        } else {
            CycleArm::Solved {
                seq,
                bins,
                staged,
                invalid: usize::try_from(invalid).unwrap_or(usize::MAX),
                deferred_future_price: deferred,
            }
        };
        self.transients.last_arm = Some(arm);
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
        // same span-context re-entry as rebuild_and_solve_affected:
        // bin jobs re-enter this cycle span per work item, so per-path child
        // spans parent under the cold-start cycle instead of forking roots.
        let solve_span = tracing::Span::current();
        // Pre-collect work items (path_id + Arc-shared resolved). The Arc
        // clones drop the immutable borrow on self.path_resolved so the
        // 'static bin jobs don't capture &self at all (f701ccd3 staging fix:
        // Arc clones are refcount bumps, not deep clones of the concentrated liquidity
        // tick-range sequences).
        let to_solve: Vec<(u64, std::sync::Arc<ResolvedMixedPath>)> = self
            .path_resolved
            .iter()
            .filter(|(_, r)| r.valid)
            .map(|(&pid, r)| (pid, std::sync::Arc::clone(r)))
            .collect();
        // LPT-pre-balanced partition. The cold start has the
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
        // the pool-ref map for the empty-march clamp. No engine state is touched
        // (engine-then-core invariant intact; the mixer only reads core).
        let memo = std::sync::Arc::clone(self.scope.walk_memo());
        let walk_telemetry = std::sync::Arc::clone(self.scope.walk_telemetry());
        let prefix_store_out = std::sync::Arc::clone(self.scope.prefix_cache());
        let path_pools: HashMap<u64, std::sync::Arc<MixedPath>> = registry.path_pools().clone();
        let core = std::sync::Arc::clone(&self.core);
        let results_block = self.cursor.coordinates().results_block;
        let runtime_cfg = self.runtime_cfg;
        let min_floor = self.min_profit_floor;
        let clamp_margin = self.clamp_margin;
        let (tx, rx) = std::sync::mpsc::channel::<(u64, SolvePathResult)>();
        for (bin_idx, bin) in bins.iter().enumerate() {
            let bin = bin.clone();
            let to_solve_bin = to_solve.clone();
            let memo = std::sync::Arc::clone(&memo);
            let prefix_store = std::sync::Arc::clone(&prefix_store_out);
            let walk_telemetry = std::sync::Arc::clone(&walk_telemetry);
            let path_pools = path_pools.clone();
            let core = std::sync::Arc::clone(&core);
            let tx = tx.clone();
            let solve_span_bin = solve_span.clone();
            let run_bin = move || {
                // Cold start: no capture wiring — deps with the
                // registered-epoch guard + the engine walk-memo handle.
                let gate_deps = ::degenbot_pools::GateDeps::per_block(
                    results_block,
                    &prefix_store,
                    None,
                    Some(&memo),
                    runtime_cfg,
                )
                .with_telemetry(Some(&walk_telemetry));
                for &i in &bin {
                    let (path_id, resolved) = &to_solve_bin[i];
                    let _solve_ctx = solve_span_bin.enter();
                    if let Some(mut r) = ::degenbot_pools::solve::solve_path_with_min_profit(
                        resolved, min_floor, &gate_deps,
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
                                core.read_at(degenbot_substrate::state_lock::LockSite::Solver);
                            let _ = clamp_result_with_state(
                                &core_read,
                                *path_id,
                                &path.pools,
                                clamp_margin,
                                &mut r,
                            );
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
        if let Some(id) = core.pool_slice::<degenbot_pools::AerodromeV2PoolIdentity>(pool_id) {
            return Some(if id.stable {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        // Camelot stable_swap pools route to the Solidly solve branch;
        // volatile Camelot is constant-product (V2). Same Python-faithful
        // classification as Aerodrome.
        if let Some(id) = core.pool_slice::<degenbot_pools::V2PoolIdentity>(pool_id) {
            return Some(if id.stable_swap {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        if core
            .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
            .is_some()
        {
            Some(HopType::V3)
        } else if core
            .pool_slice::<degenbot_pools::V4PoolState>(pool_id)
            .is_some()
        {
            Some(HopType::V4)
        } else if core
            .pool_slice::<degenbot_pools::BalancerWeightedPoolState>(pool_id)
            .is_some()
        {
            Some(HopType::BalancerWeighted)
        } else if core
            .pool_slice::<degenbot_pools::BalancerStablePoolState>(pool_id)
            .is_some()
        {
            Some(HopType::BalancerStable)
        } else if core
            .pool_slice::<degenbot_pools::CurvePoolState>(pool_id)
            .is_some()
        {
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
        // fewer than two hops is a structural caller bug, not a state —
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
        // Dedup — if the same (pool_id, zero_for_one) sequence is
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
            crate::instruments::record(crate::instruments::CensusEvent::RegistrationSkip {
                reason: "dup",
            });
            return Ok(Registration {
                path_id: existing_id,
                created: false,
                resolved: None,
            });
        }
        // PRG-4: the registered-path cap lives HERE, in the engine
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
        // resolve BEFORE storing so an unroutable hop rejects the
        // registration loudly and leaves no half-registered state behind.
        let mut resolved = ResolvedMixedPath::default();
        let deficits = {
            let core = self
                .core
                .read_at(degenbot_substrate::state_lock::LockSite::Solver);
            resolve_hops(
                &core,
                &pool_refs,
                &mut resolved,
                &self.hop_projection_cache,
                Some(&mut self.hop_projection_count),
                self.concentrated_liquidity_projection_memo,
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
        // DEBUG-gated: one line per path registration
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
            .read_at(degenbot_substrate::state_lock::LockSite::Solver);
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
                if let Some(mut solve_result) = ::degenbot_pools::solve::solve_path(
                    resolved,
                    &::degenbot_pools::GateDeps::offline(),
                )
                .result
                {
                    if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                        self.clamp_concentrated_liquidity_hop_capacity(
                            path_id,
                            &mut solve_result,
                            registry,
                        );
                        self.results.insert(path_id, solve_result);
                        self.transients.pending_new_paths.insert(path_id);
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
                .read_at(degenbot_substrate::state_lock::LockSite::Solver);
            for (&path_id, path) in registry.iter() {
                let mut resolved = ResolvedMixedPath::default();
                let deficits = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    &self.hop_projection_cache,
                    Some(&mut self.hop_projection_count),
                    self.concentrated_liquidity_projection_memo,
                );
                self.path_resolved
                    .insert(path_id, std::sync::Arc::new(resolved));
                // cold-start full sweep also refreshes the state machine.
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
        // monotone advance on the block cursor (the cold-start
        // sweep can no longer drag a seeded resume anchor backwards).
        self.cursor.advance_solved(block_number);
        // Intentionally no compute_diff_and_send here: dispatching would
        // advance `delivered` (claiming "Python has seen these") before any
        // channel exists — poisoning the diff for the first real send. The
        // pump owns dispatch via `send_result_batch`.
    }
    /// The uncached render: the white-box door over the ONE body (the epoch's
    /// cached telemetry door is [`describe_path_cached_with`]).
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn describe_path(&self, path_id: u64, registry: &PathRegistry) -> String {
        describe_path_with(&self.core, registry, path_id)
    }
    /// Simulate one clamp-admitted path through the installed hook. `None` =
    /// no hook installed, the path is unknown, or the hook reported failure-
    /// without-payload. `metadata` supplies the sim's block env (the same
    /// fields the worker path fills from its cycle snapshot).
    ///
    /// T3 re-home: the cycle owns the hook handle and the solve-anchor cursor,
    /// so the request reads them directly; only the shared registry crosses as
    /// an argument.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn inline_simulate(
        &self,
        path_id: u64,
        registry: &PathRegistry,
        clamp_admitted: &SolvePathResult,
        metadata: &BlockMetadata,
    ) -> Option<SimulatedPathResult> {
        let sim = self.inline_sim.as_ref()?;
        let hops = registry.get(path_id)?.pools.clone();
        let request = InlineSimRequest {
            path_id,
            hops,
            optimal_input: clamp_admitted.optimal_input,
            consumed_inputs: clamp_admitted.consumed_inputs.clone(),
            hop_outputs: clamp_admitted.hop_outputs.clone(),
            state_nonces: clamp_admitted.state_nonces.clone(),
            sim_block: self.cursor.coordinates().results_block,
            block_timestamp: metadata.timestamp,
            parent_base_fee: metadata.base_fee_per_gas.unwrap_or(0),
            parent_gas_used: metadata.gas_used,
            parent_gas_limit: metadata.gas_limit,
        };
        sim.simulate_path(request)
    }
}
impl SolveCycle {
    /// The admission DRAW - the SINGLE consumption decision of the
    /// epoch ledger. `budget = max(0, target - in-flight)` keys freshest-first,
    /// the overflow RETAINED for a later cycle; a zero budget is the SHED
    /// verdict, carried on the returned [`CycleTicket`] (never stashed on the
    /// cycle) so only the epoch that consumes that ticket can act on it.
    #[must_use]
    pub(crate) fn draw(&mut self, delta: &EpochDelta, head: u64) -> CycleTicket {
        shed::draw(
            delta,
            head,
            self.solve_admission,
            self.admission_target_depth,
            self.admission_retention_blocks,
            &mut self.detached_cycle,
        )
    }
    /// ADR-045: the ids-path-deregistered coupling. Drops the path's resolve
    /// companions, results, and pending-new carry. The registry removal and the
    /// delivery bookkeeping stay on the engine.
    pub(crate) fn forget(&mut self, path_id: u64) {
        self.path_resolved.remove(&path_id);
        self.path_status.remove(&path_id);
        self.resolved_update_snapshot.remove(&path_id);
        self.results.remove(&path_id);
        self.transients.pending_new_paths.remove(&path_id);
    }
}
#[cfg(test)]
impl SolveCycle {
    /// The pending-new carry (the eager-registration handoff to the next
    /// dirty cycle). The white-box reads go through the sanctioned test door
    /// ([`super::test_harness`]); the field stays private to this module.
    pub(crate) fn pending_new_paths(&self) -> &HashSet<u64> {
        &self.transients.pending_new_paths
    }
    /// Arm the pending-new carry without registering a path first (the
    /// white-box fixture seam; `register_and_solve_path` is the production
    /// writer). The carry never leaves the cycle: callers ask for the
    /// mutation, never a `&mut` (ADR-076 D3).
    pub(crate) fn note_pending_new_path(&mut self, path_id: u64) {
        self.transients.pending_new_paths.insert(path_id);
    }
    /// Consume the pending-new carry exactly as one dirty epoch does (the
    /// white-box fixture seam; the epoch clears it through [`EpochContext`]).
    pub(crate) fn clear_pending_new_carry(&mut self) {
        self.transients.pending_new_paths.clear();
    }
    /// Test harness: drain up to `expected` items from the merge pipe
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
    // --- the remaining `#[cfg(test)]` knob setters, homed on their owning
    // machine (moved off the `ArbitrageEngine` seam twin). The four ADR-076
    // D2 setters and the three inline epoch flags are gone: those knobs are
    // construction-time fields on [`SolveCycle`], written directly by the
    // tests that vary them (internal seams, never interface methods). ---
    pub(crate) fn set_merge_panic_hook(&mut self, hook: Arc<dyn Fn(u64) + Send + Sync>) {
        self.test_merge_panic = Some(hook);
    }
    pub(crate) fn set_streaming_delivery(&mut self, on: bool) {
        self.streaming_delivery = on;
    }
    /// Test seam for the admission stance. Production packs it from
    /// `cfg.solve.admission_shed` at construction through `EngineRetune`.
    pub(crate) fn set_solve_admission(&mut self, on: bool) {
        self.solve_admission = on;
    }
    /// Test seam for the target depth — the clamp mirrors the
    /// construction clamp exactly.
    pub(crate) fn set_admission_target_depth(&mut self, depth: usize) {
        self.admission_target_depth = u64::try_from(depth)
            .unwrap_or(detached_cycle::DETACHED_INFLIGHT_CAP)
            .clamp(1, detached_cycle::DETACHED_INFLIGHT_CAP);
    }
    /// Test seam for the retention window (blocks).
    pub(crate) fn set_admission_retention_blocks(&mut self, window: u64) {
        self.admission_retention_blocks = window;
    }
}
#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
    use ::degenbot_pools::solve::{HopType, PoolHop};
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
            .cycle
            .register_and_solve_path(hops, &mut engine.registry)
            .expect("eager register must solve the divergent path")
            .path_id;
        assert!(
            engine.cycle.pending_new_paths().contains(&path_id),
            "register_and_solve_path arms the pending-new carry"
        );
        let empty = crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        engine.cycle.run_epoch(
            crate::arb_engine::test_harness::cycle_ticket(&empty),
            1,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
        assert!(
            engine.cycle.pending_new_paths().is_empty(),
            "the dirty cycle consumes and clears the carry"
        );
        assert_eq!(
            engine.cycle.cursor.coordinates().results_block,
            1,
            "the cycle stamps the solve anchor"
        );
        assert!(
            engine.cycle.results.contains_key(&path_id),
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
            .cycle
            .register_and_solve_path(hops.clone(), &mut engine.registry)
            .expect("fresh register")
            .path_id;
        assert!(
            engine.cycle.pending_new_paths().contains(&path_id),
            "a fresh register arms the carry"
        );
        // Model the carry consumed by one dirty cycle.
        engine.cycle.clear_pending_new_carry();
        let again = engine
            .cycle
            .register_and_solve_path(hops, &mut engine.registry)
            .expect("dedup register");
        assert_eq!(
            again.path_id, path_id,
            "a dedup hit retains the registry id"
        );
        assert!(
            engine.cycle.pending_new_paths().is_empty(),
            "a dedup hit (created=false) must not touch the pending-new carry"
        );
    }
    // ---------------------------------------------------------------------
    // T4 wiring: the target surface now exists.
    // ---------------------------------------------------------------------
    /// The candidate-8 interlock, re-pinned on the ticket: the cycle consumes
    /// the epoch's work-carried delta (the keys the draw put on the ticket)
    /// and holds no second swappable handle for the verdict.
    #[test]
    fn run_epoch_consumes_the_epoch_work_carried_delta() {
        use degenbot_substrate::EpochDelta;
        // The epoch ledger is the SINGLE work-carried owner. The Resolved
        // stage draws the cycle's keys from it into the ticket; the Solved
        // stage's `SolveCycle::run_epoch` consumes exactly that ticket.
        let delta = EpochDelta::new(11u64);
        delta.record_affected(HopType::V2, 1, 11);
        delta.record_affected(HopType::V2, 2, 11);
        let mut engine = ArbitrageEngine::new();
        let metadata = BlockMetadata::default();
        let ticket = engine.cycle.draw(&delta, 11);
        assert_eq!(
            ticket.affected().len(),
            2,
            "the draw IS the cycle's affected set, carried on the ticket"
        );
        assert!(
            delta.is_empty(),
            "the draw is the single consumption decision — no second handle sees the keys"
        );
        let outcome = engine.cycle.run_epoch(
            ticket,
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
    /// The runtime half of the ticket guard: the Resolved row's slot yields its
    /// ticket exactly once, so a second Solved pass over one Resolved row
    /// fails loudly instead of solving from a spent ticket.
    #[test]
    #[should_panic(expected = "the Resolved row's cycle ticket")]
    fn the_resolved_row_takes_its_ticket_exactly_once() {
        let row = crate::bot_core::AffectedPaths::drawn(CycleTicket::for_test(Vec::new()));
        let _spent = row.take_ticket();
        let _again = row.take_ticket();
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
    use ::degenbot_pools::solve::SolvePathResult;
    use alloy::primitives::U256;
    /// Path-142603 (V4-V4-V3 @25723658) regression: the solver reported a
    /// phantom +346,369,630 wei profit because its V3 hop2 output
    /// (351,476,391,576,684) over-predicted the byte-exact twin
    /// (351,475,872,056,229) by 519,520,455 wei. After the concentrated liquidity clamp aligns
    /// `hop_outputs`/`consumed_inputs` to the twin, the selection profit MUST
    /// be recomputed from the clamped values: the round trip nets
    /// -173,150,825 wei -> saturates to 0 -> dropped by the `profit > min_profit`
    /// delivery gate instead of being selected and executing to a `no-profit`
    /// trap. (Regression for the BUG-B fix in `clamp_concentrated_liquidity_hop_capacity`.)
    #[test]
    fn post_clamp_last_hop_loss_saturates_profit_to_zero() {
        let mut r = SolvePathResult {
            optimal_input: U256::from(351_476_045_207_054u64),
            // Profit the SOLVER computed on its over-predicted raw hop2 output
            // (= 351_476_391_576_684 - 351_476_045_207_054 = +346,369,630).
            profit: U256::from(346_369_630u64),
            // Twin-aligned outputs after the concentrated liquidity clamp: hop2 (last) clamped
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

// ======================================================================
// ADR-076 D2/D3: the `SolveCycle` surface is production shape.
//
// The epoch's test knobs are internal seams (construction-time fields), never
// interface methods, and the pending-new carry never crosses the seam as a
// `&mut`. The absence half is a compile-time inherent-absence probe (the
// candidate2 `NoInherentTwinProbe` pattern): the probe takes `&mut` and is
// called on a `&mut SolveCycle`, so method resolution meets an inherent method
// at the same autoref step and prefers it — an inherent re-introduction
// shadows the probe and fails to build (arity/type mismatch), while the trait
// methods alone resolve cleanly.
// ======================================================================
#[cfg(test)]
mod epoch_context_seams {
    /// The five names that left the `SolveCycle` surface (ADR-076 D2/D3).
    #[test]
    fn solve_cycle_surface_carries_no_test_knob_methods() {
        struct KnobProbeToken;

        trait NoTestKnobSurfaceProbe {
            fn set_force_deferred_for_test(&mut self, _t: KnobProbeToken);
            fn set_merge_probe(&mut self, _t: KnobProbeToken);
            fn set_sync_merge_for_test(&mut self, _t: KnobProbeToken);
            fn set_resolve_parallel_for_test(&mut self, _t: KnobProbeToken);
            fn pending_new_paths_mut(&mut self, _t: KnobProbeToken);
        }

        impl NoTestKnobSurfaceProbe for super::SolveCycle {
            fn set_force_deferred_for_test(&mut self, _t: KnobProbeToken) {}
            fn set_merge_probe(&mut self, _t: KnobProbeToken) {}
            fn set_sync_merge_for_test(&mut self, _t: KnobProbeToken) {}
            fn set_resolve_parallel_for_test(&mut self, _t: KnobProbeToken) {}
            fn pending_new_paths_mut(&mut self, _t: KnobProbeToken) {}
        }

        let mut engine = crate::arb_engine::ArbitrageEngine::new();
        let cycle = &mut engine.cycle;
        cycle.set_force_deferred_for_test(KnobProbeToken);
        cycle.set_merge_probe(KnobProbeToken);
        cycle.set_sync_merge_for_test(KnobProbeToken);
        cycle.set_resolve_parallel_for_test(KnobProbeToken);
        cycle.pending_new_paths_mut(KnobProbeToken);
    }

    /// The blanket half (the `dissolution_complete` textual-gate idiom): no
    /// `pub(crate) fn` declaration in the cycle module carries a `*_for_test`
    /// name, so a NEW knob method cannot slip past the five-name probe above.
    /// The needle is split so the gate's own text is never an offender.
    #[test]
    fn solve_cycle_source_declares_no_for_test_surface() {
        const MOD: &str = include_str!("mod.rs");
        const SHED: &str = include_str!("shed.rs");
        const RESOLVE: &str = include_str!("resolve.rs");
        const SOLVE: &str = include_str!("solve.rs");
        const MERGE: &str = include_str!("merge.rs");
        const DELIVER: &str = include_str!("deliver.rs");
        let suffix = ["_for", "_test("].concat();
        let mut offenders: Vec<&str> = Vec::new();
        for src in [MOD, SHED, RESOLVE, SOLVE, MERGE, DELIVER] {
            offenders.extend(
                src.lines()
                    .filter(|line| line.contains("pub(crate) fn ") && line.contains(&suffix)),
            );
        }
        assert!(
            offenders.is_empty(),
            "`*_for_test` declarations remain on the SolveCycle surface: {offenders:?}"
        );
    }

    /// The phase seams are machine-direct free functions (ADR-076 D1): each is
    /// defined exactly once, in the submodule that owns it, and never as an
    /// inherent cycle method — a phase body copied back into `mod.rs` (or a
    /// second definition beside the first) fails here. Four seams keep a
    /// `SolveCycle` door in `mod.rs`: the admission draw (it runs from the
    /// Resolved stage row before the epoch exists), the two merge doors the
    /// sidecar/tests call while holding the whole cycle, and the `&self` clamp
    /// accessor.
    #[test]
    fn phase_seams_are_single_free_functions_in_their_owning_module() {
        const MOD: &str = include_str!("mod.rs");
        const SHED: &str = include_str!("shed.rs");
        const RESOLVE: &str = include_str!("resolve.rs");
        const SOLVE: &str = include_str!("solve.rs");
        const MERGE: &str = include_str!("merge.rs");
        const DELIVER: &str = include_str!("deliver.rs");
        // The needles are split so the gate's own text is never an offender.
        let seams = [
            (["fn shed", "_epoch("].concat(), SHED, "shed.rs"),
            (["fn skipped", "_empty_epoch("].concat(), SHED, "shed.rs"),
            (
                ["fn scan", "_dirty_status("].concat(),
                RESOLVE,
                "resolve.rs",
            ),
            (["fn resolve", "_anchor("].concat(), RESOLVE, "resolve.rs"),
            (["fn resolve", "_epoch("].concat(), RESOLVE, "resolve.rs"),
            (["fn fanout", "_telemetry("].concat(), SOLVE, "solve.rs"),
            (["fn lpt", "_bins("].concat(), SOLVE, "solve.rs"),
            (["fn solve", "_epoch("].concat(), SOLVE, "solve.rs"),
            (["fn drain", "_lane_outcomes("].concat(), MERGE, "merge.rs"),
            (
                ["fn deliver", "_merged_result("].concat(),
                DELIVER,
                "deliver.rs",
            ),
        ];
        for (needle, owner, owner_name) in seams {
            assert_eq!(
                owner.matches(&needle).count(),
                1,
                "`{needle}` must be defined exactly once in {owner_name}"
            );
            assert!(
                !MOD.contains(&needle),
                "the phase body `{needle}` must not remain in mod.rs"
            );
        }
        // The doors: the body is defined once in its owning module and once in
        // `mod.rs` as the delegator that builds the epoch workspace.
        let doors = [
            (["fn merge", "_one_result("].concat(), MERGE, "merge.rs"),
            (["fn merge", "_detached_item("].concat(), MERGE, "merge.rs"),
            (
                ["fn clamp", "_concentrated_liquidity_hop_capacity("].concat(),
                MERGE,
                "merge.rs",
            ),
        ];
        for (needle, owner, owner_name) in doors {
            assert_eq!(
                owner.matches(&needle).count(),
                1,
                "`{needle}` must be defined exactly once in {owner_name}"
            );
            assert_eq!(
                MOD.matches(&needle).count(),
                1,
                "mod.rs keeps exactly one `{needle}` door"
            );
        }
        let draw = ["fn dra", "w("].concat();
        assert_eq!(
            SHED.matches(&draw).count(),
            1,
            "the draw body is defined once, in shed.rs"
        );
        assert_eq!(
            MOD.matches(&draw).count(),
            1,
            "mod.rs keeps only the `SolveCycle::draw` delegator"
        );
        for (src, name) in [
            (SHED, "shed.rs"),
            (RESOLVE, "resolve.rs"),
            (SOLVE, "solve.rs"),
            (MERGE, "merge.rs"),
            (DELIVER, "deliver.rs"),
        ] {
            assert!(
                !src.contains("impl SolveCycle"),
                "{name} must not carry an inherent `impl SolveCycle`"
            );
        }
    }

    /// The context's cluster shape is a compile-time fact (ADR-076 D1): the
    /// phases will be handed `&mut EpochContext`, so a cluster field that
    /// leaves the context is a build failure here.
    #[test]
    fn epoch_context_carries_the_four_clusters() {
        fn carries(ctx: &super::EpochContext<'_>) {
            // status
            let _ = &ctx.path_status;
            let _ = &ctx.path_resolved;
            let _ = &ctx.cursor;
            // admission
            let _ = &ctx.solve_admission;
            let _ = &ctx.admission_target_depth;
            let _ = &ctx.detached_cycle;
            let _ = &ctx.pending_new_paths;
            let _ = &ctx.paths_same_state_this_cycle;
            let _ = &ctx.deferred_re_record;
            let _ = &ctx.force_deferred;
            // caches
            let _ = &ctx.resolved_update_snapshot;
            let _ = &ctx.hop_projection_cache;
            let _ = &ctx.concentrated_liquidity_projection_memo;
            // results/config
            let _ = &ctx.results;
            let _ = &ctx.cfg;
            let _ = &ctx.runtime_cfg;
            let _ = &ctx.path_cost_history;
            let _ = &ctx.min_profit_floor;
            let _ = &ctx.clamp_margin;
            let _ = &ctx.inline_sim_enabled;
            let _ = &ctx.inline_sim;
            let _ = &ctx.resolve_par_stance;
            // phase collaborators (the solve/merge/deliver phases)
            let _ = &ctx.inline_payloads;
            let _ = &ctx.streaming_delivery;
            let _ = &ctx.path_description_cache;
            let _ = &ctx.merge_probe;
            let _ = &ctx.test_merge_panic;
            let _ = &ctx.test_solve_delay;
            let _ = &ctx.test_solve_panic;
        }
        let _: fn(&super::EpochContext<'_>) = carries;
    }
}
