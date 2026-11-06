//! Arbitrage engine — multi-DEX cyclic arbitrage over a shared `BotState`.
//!
//! A unified engine that handles Uniswap V2/V3/V4, Solidly-family
//! (Aerodrome, Camelot) and (in progress) Curve + Balancer pools in the same
//! per-block lifecycle. Supports mixed paths (e.g., V2→V3, V3→V4, V4→V2,
//! V2→Solidly hops).
//!
//! # Design
//!
//! The engine composes:
//! - A [`BotState`] for V2 pool state and constant-product solving (ADR-003:
//!   `BotState` is the single state owner; the engine is a consumer)
//! - A [`BotState`](degenbot_substrate::BotState) for V2+V3 pool state (ADR-003:
//!   `BotState` is the single state owner, peer to this engine)
//! - A [`BotState`](degenbot_substrate::BotState) for all pool state (V2+V3+V4 —
//!   ADR-003), the single Rust state owner peer to this engine
//!
//! V4 pools share identical concentrated-liquidity math with V3. The solver
//! treats V3 and V4 hops identically — both produce `IntV3TickRangeSequence`.
//!
//! On [`ArbitrageEngine::process_block`]:
//! 1. Decode Sync, V3 Swap, and V4 Swap events from logs
//! 2. Route V2 Sync events to the V2 engine, V3 Swap events to the V3 engine,
//!    V4 Swap events to the V4 engine
//! 3. Solve registered paths using the appropriate solver
//!
//! Hooked V4 pools: amount-modifying hooks are ADMITTED at registration and
//! at hop projection (ADR-090). The search tier ranks them by the standard
//! concentrated-liquidity walk — the approximation `Caveats::HOOKED_POOL`
//! flags on the solved result — and the finalist re-quote
//! ([`hooked_leg_requote`](crate::arb_engine::HookedLegReQuote)) prices each
//! hooked leg black-box at the solve anchor before the delivery gate, so a
//! candidate whose exact figure no longer clears the bar dies on economics
//! rather than on a hook.
//!
//! # Module layout
//!
//! Every module below is `pub(crate)`; the items a driver needs are
//! re-exported from this module (ADR-084 D2 — a door, not a tree).
//!
//! | Module | Concern |
//! |--------|---------|
//! | `lane_walk` | THE ONE lane walk: per-bin solve, the pipelined-sim pacing, and the walk-adjacent clamp/flush helpers |
//! | `solve_cycle` | The solve cycle as a deep module (ADR-045): the concentrated-liquidity-hop clamp + profit recompute live here |
//! | `delivery_lifecycle` | Delivery lifecycle: channel open/send/close + the end-of-stream contract |
//! | `delivery_policy` | Delivery policy: diff computation, thresholds, delivered-bookkeeping |
//! | `block_cursor` | The engine block cursor — one owner of the engine-side block-coordinate residue |
//! | `detached_cycle` | THE one detached/in-cycle solve-arm machine: per-cycle states, the merge pipe, the gauge pair, the seq counters, the ledger door, the disposition counters, the fan-in tripwire, and the ONE sidecar spawn |
//! | `lifecycle` | Path registration, buffer management, engine accessors |
//! | `session_end` | The session-end detection facts and the pure Session-end ranking (`rank`) the drivers render |
//! | `tests` | Unit tests |
//!
//! The `PyO3` wrapper (`PyArbitrageEngine`) lives in the `degenbot_rs` shell.
use self::block_cursor::BlockCursor;
use self::boot_stamp::BootStamp;
use self::delivery_policy::DeliveryPolicy;
use self::path_registry::PathRegistry;
use self::solve_cycle::{BlockScope, SolveCycle};
use ::degenbot_pools::solve::{MixedPath, SolvePathResult};
#[cfg(test)]
use alloy::primitives::aliases::U112;
#[cfg(test)]
use alloy::primitives::Address;
use dashmap::DashMap;
#[cfg(test)]
use degenbot_core::fee::fee_ratio;
use degenbot_substrate::resolve::HopProjectionCache;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use hashbrown::HashMap;
use std::sync::Arc;
// THE construction-stamped fleet boot carrier: the engine's own
// FleetBoot value + the ride ledger the per-role fleet statics consult.
mod boot_stamp;
// the ONE engine block cursor — the consolidated owner of the
// engine-side block-coordinate residue (completes ADR-041 §3 item 5's
// engine-side anchor fold; see the module's own doc header).
pub(crate) mod block_cursor;

// The Rust-owned registration crawl. The tree is crate-private: its operator
// surface is re-exported at the crate-root `crawl` door (ADR-084 D2 — a driver
// names the door, never a module beneath it).
pub(crate) mod crawl;
// The admission floor as ONE decision (the walk's inline-sim seam and the
// delivery window are its two enforcement points; see the module's docs for
// which seam owns the count).
mod admission_floor;
// Sub-modules — each contains `impl ArbitrageEngine` or `impl PyArbitrageEngine` blocks.
mod delivery_lifecycle;
mod delivery_policy;
// THE one detached/in-cycle solve-arm machine: the per-cycle
// states, the merge pipe, the gauge pair, the seq counters, the ledger
// door, the disposition counters, the fan-in tripwire, and the ONE
// sidecar spawn — see the module's own doc header.
pub(crate) mod detached_cycle;
mod diagnostic;
// The arb engine's StageHandlers implementation —
// the ONE surface left between the machine driver and the engine. The
// dissolved `engine_handle` (wrapper Mutex), `engine_subscriber` (liveness
// adapter), and `epoch_delta_parity`/`test_oracle` (the parity
// oracle, GONE now that `EpochDelta` is sole authority) are deleted —
// hard cutover, Q6.
mod engine_stages;
// ADR-050: the public Rust driver seam above the one
// stage seam. The engine type stays `pub(crate)`; this composes
// `EngineStages` + the pump session state so a `cargo add degenbot`
// consumer can run the subscribe→resume(+auto-backfill)→stop ritual.
mod driver;
pub(crate) mod executor;

// The PRG-3 registration intake surface — a crate-private tree behind the
// crate-root `fleet_intake` door.
pub(crate) mod fleet_intake;
mod fleet_registration_executor;
mod fleet_sim_executor;
pub(crate) mod fleet_solve_executor;
mod fleet_status;
mod inline_sim;
// ADR-090 H3: the hooked-leg black-box re-quote seam — the installed tier the
// clamp consults for an amount-modifying V4 leg, and the plan/stage vocabulary
// both arms of that decision share.
mod hooked_leg_requote;
mod lifecycle;
mod path_info;
mod path_lifecycle;
pub mod pump_session;
// The engine-session end DETECTION facts: the cause vocabulary, the heartbeat
// stall watchdog, and the once-only delivery channel shared by both drivers.
mod session_end;

// The session's ONE ordered event stream and its ONE consumer claim (ADR-093 D5):
// `Block(N)` before every `Batch(N)`, no `End` item, natural end = the stream's
// closure.
mod session_events;
// ADR-045: the path-identity registry (`PathRegistry`) —
// registered paths, reverse index, signatures, id allocator, cap, dedups.
mod path_registry;
// the session's path-object ADAPTER over that registry — the owner side of
// `degenbot_substrate::session_registry::PathObjectAdapter`, so the session names
// canonical paths without a second path store.
mod path_objects;
// The typed operator re-parameterization value crossing
// the driver seam — `EngineRetune`, applied at construction and at runtime via
// `EngineStages::apply_retune`.
mod retune;
// The ONE pooled-seat host for the WorkQueue fleet roles (sim +
// registration) — the executors are thin role descriptors over it; the
// solve executor's exclusion (per-seat channel model + posture-invariant
// typed-submit admission) is documented in the module doc.
mod fleet_wake;
mod seat_host;

// The process-wide boot statics (`install_boot`, `global_executor`, the
// `BootSlot`/`FleetBootRegistry` keyed boot owner) — process-wide, not
// seat-scoped (ADR-079 D1).
mod fleet_boot;

// the per-bin lane walk's home — `solve_one_path` (the per-path
// solve + diagnostics body the Solver seats' bins execute) lives here; the
// walk driver and Lane* policy types are later tasks' inhabitants.
mod lane_walk;

// the solve cycle's single owner of per-path cost accounting (commit 2 of
// PR C4): the pipeline instruments, the K-slowest retention, and the two
// heavy-path capture hooks, behind one `record` call from the lane walk.
mod path_cost_ledger;
// heavy-path capture diagnostics, extracted from the retired
// grab file (import-only move; the module owns its honesty probe + test island).
mod solver_capture;
// the pure workload-analysis cluster (solve-bin sizing, LPT
// partition, named cordon-fallback seat plan, resolve-time cost proxies),
// extracted from the retired grab file (import-only move; the module
// owns its honesty probe + test islands).
mod workload_partition;
// The per-epoch WalkMemoStats tap: drains the
// engine-owned WalkMemo ONCE per begin_block boundary (drain-then-advance)
// plus a final drain at EngineDriver::close, into the logs/walkmemo_stats.jsonl
// sink and the ADR-043 `degenbot.solver.walk_memo_*` instruments.
// Observation only — zero behavior change to solving.
pub(crate) mod walk_telemetry;
// ADR-045 T1: the solve-cycle data-type seam (`CycleOutcome` /
// `CycleArm` / `ResolveCensus` / `Registration`) — T3/T4 assemble
// `SolveCycle` on top of it; nothing consumes the types yet.
mod solve_cycle;
// The cycle ticket: the solve cycle's transient-handoff value. The Resolved
// stage row (`bot_core::AffectedPaths`) carries it from the draw to the epoch,
// so the stage machine can name its payload without reaching into the module.
pub(crate) use solve_cycle::CycleTicket;
// the executor A/B probe fixtures (heavy-concentrated-liquidity corpus loader, shared
// solve-cycle fixture, production LPT bin packer) live test-only beside their
// consumers; `#[cfg(all(test, feature = "executor-ab-probe"))]` so the fixtures
// never compile into production AND the fixture-loading corpus lane (pools'
// zstd-backed `capture-fixtures`) stays out of the default test build (ADR-084
// D3). The test lanes enable the feature explicitly.
#[cfg(all(test, feature = "executor-ab-probe"))]
mod executor_ab_probe;
#[cfg(test)]
mod test_harness;
#[cfg(test)]
mod tests;
pub use diagnostic::{
    compute_field_diffs, DiagnosticHop, DiagnosticPathState, DiagnosticPoolState, FieldDiff,
};
pub use driver::{
    BackfillGap, ConnectRefusal, Connected, CrawlBoot, CrawlRefusal, DriverError,
    EngineChannelHandles, EngineDriver, PhaseError, SubscribeRefusal, TeardownReport,
    SESSION_EVENT_CHANNEL_NAME,
};
pub use engine_stages::EngineStages;
pub use fleet_status::{
    fleet_profile_summary, fleet_runtime_status, FleetBinding, FleetProfileSummary,
};
pub(crate) use fleet_wake::feed_throttle;
pub use fleet_wake::wake_hosts;
pub use hooked_leg_requote::{
    commit_hook_fees, commit_plan_hook_fees, hooked_leg_facts, HookedLeg, HookedLegAnchor,
    HookedLegAnswer, HookedLegPlan, HookedLegQuote, HookedLegReQuote, HookedLegStage,
};
pub use inline_sim::{
    AccessListRow, CapturedSwapRow, InlineSimFailure, InlineSimRequest, InlineSimulator,
    InlineSwapFamily, SimulatedPathResult,
};
/// Path registration's rejection vocabulary — the typed refusal the drivers
/// map onto their own error surfaces.
pub use lifecycle::PathRegistrationError;
pub use pump_session::{transition, CrawlExit, PumpDecline, PumpEvent, PumpExit, PumpPhase};
pub use retune::EngineRetune;
pub use session_end::{
    rank, ConsumerOutcome, Heartbeat, RaiseSlot, RegistrationOutcome, SessionEndCause,
    SessionEndDetection, SessionEndFacts, SessionEndObservation, SessionEndPublisher,
    SessionEndRanking, SessionEndVerdict,
};
pub use session_events::{
    ClaimRefusal, ConsumerClaim, EventHandoff, SessionEvent, SessionEventSource, SessionStream,
};
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------
// The path/snapshot/hop-state value types live in `degenbot_pools::solve`.
pub use degenbot_substrate::BlockMetadata;
// ADR-027 completion (2026-08-20 review): the block-clock pipe is
// coordinator-owned; the type moved to bot_core. Re-exported so external
// references keep working (same pattern as BlockMetadata above).
pub use degenbot_core::block_notification::BlockNotification;
/// Incremental result batch pushed to Python via the result channel.
///
/// Each batch contains only paths that changed since the last batch
/// Python consumed — unchanged entries stay in Rust.
#[derive(Clone, Debug)]
pub struct ResultBatch {
    /// The block number these results were solved for
    pub solve_block: u64,
    /// Block timestamp
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks)
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block
    pub gas_used: u64,
    /// Gas limit of this block
    pub gas_limit: u64,
    /// Paths above the profit threshold and NOT in the previous delivered set
    pub fresh: Vec<(u64, SolvePathResult)>,
    /// Paths above the threshold in both, but any field changed (full `PartialEq`)
    pub updated: Vec<(u64, SolvePathResult)>,
    /// Path IDs that were above threshold but are now below (still registered)
    pub expired: Vec<u64>,
    /// Path IDs that were de-registered (permanently gone)
    pub removed: Vec<u64>,
    /// SIMPIPE2 T3: the inline-sim payloads for the `fresh`/`updated` paths
    /// (SIMPIPE stance `DEGENBOT_SOLVE_INLINE_SIM`). Absent (empty map) = the
    /// legacy FFI-sim path for every entry — per-entry presence decides.
    pub payloads: HashMap<u64, inline_sim::SimulatedPathResult>,
}
/// the deferred-path re-record hook (the ledger carry). The
/// dispatch calls it with the deferred paths' hop-pool keys and the cycle's
/// solve block; the `EngineStages` constructor installs the
/// `EpochDelta::record` closure so a deferred path re-enters through the
/// next draw (same freshness ordering, admission budget, retention window —
/// one deferral concept). `None` on direct engine drives (unit tests, the
/// cold-start `solve_all`): the deferral then keeps its pre-fold dropped
/// behavior.
pub(crate) type DeferredReRecordHook =
    std::sync::Arc<dyn Fn(&[degenbot_pools::AffectedKey], u64) + Send + Sync>;
/// The unified Uniswap engine — owns V2, V3, and V4 pool state and solves
/// mixed arbitrage paths.
///
/// V2 pool state lives in [`BotState`] (ADR-003: the single Rust state owner,
/// peer to this engine). The engine holds the shared `Arc<RwLock<BotState>>`
/// (ADR-006 D1+D2 — `RwLock` on the core, shared with [`PyBot`] via
/// [`ArbitrageEngine::with_core`]; `new()` standalone sugar allocates its own)
/// and reads/writes pool state through it. Lock ordering when nested is
/// **engine-then-core** — no code path ever nests core-then-engine.
pub(crate) struct ArbitrageEngine {
    /// the owner-loaded typed bot config (one loader process-wide;
    /// never re-read from the environment). Construction stances + capture
    /// config read from here. ADR-045 T4: the solve cycle owns its own clone
    /// for the resolve/solve bodies; this field remains for the construction
    /// stances.
    #[expect(dead_code)]
    pub(crate) cfg: std::sync::Arc<::degenbot_config::BotConfig>,
    /// the instance solver runtime stance, built at construction from
    /// [`Self::cfg`] and threaded down into every solve cycle. Replaces the
    /// solve tree's removed process-global RUNTIME `OnceLock`.
    /// ADR-045 T4: the cycle owns its own copy.
    #[expect(dead_code)]
    runtime_cfg: ::degenbot_pools::SolveRuntimeConfig,
    /// V2 + V3 + V4 pool state owner (ADR-003). The shared
    /// `Arc<RwLock<BotState>>` (ADR-006 D1+D2): read methods take a read guard,
    /// mutations a write guard. Lock ordering when nested is
    /// engine-then-core; no code path ever nests in the opposite direction.
    pub(crate) core: Arc<StateLock<BotState>>,
    /// ADR-045: the engine's path-identity registry — registered
    /// paths, the `pool_to_paths` reverse index, signature dedup, the path-id
    /// allocator, the registered-path cap, and the dedup counter. Hot solves
    /// read it through a shared `&PathRegistry`; only registration takes
    /// `&mut`. The invariant "`path_pools` is consistent with `pool_to_paths`"
    /// is internal to [`PathRegistry::commit`] / [`PathRegistry::remove`].
    pub(crate) registry: PathRegistry,
    /// ADR-045: the solve cycle's owned state — the resolve
    /// companions, the cycle-transient stash, the solve output, the admissions
    /// stance, the detached-arm collaborator, the walk/projection recorders,
    /// and the seven `for_test` knobs. Fields only at T3; behavior methods move
    /// at T4. The engine keeps identity (`registry`), the shared core, the
    /// delivery policy, the lifecycle phase, and the construction stances.
    pub(crate) cycle: SolveCycle,
    /// Delivery policy — the optional publish sink that consumes the solve
    /// output (`latest_results`) and pushes diffs over the result/block
    /// channels . Owns `delivered`/`deregistered`, the profit
    /// thresholds, and `result_tx`/`block_tx`; decoupled from solve state so a
    /// standalone consumer gets raw results without it.
    pub(crate) delivery: DeliveryPolicy,
    /// THE construction-stamped fleet boot: the engine's OWN
    /// `FleetBoot`, derived from the CALLER's cfg at construction and
    /// stamped with the engine id + a deterministic cfg hash. Packed in
    /// `with_core_cfg` right beside `streaming_delivery` (the
    /// construction-stance pattern); the per-role fleet statics courier it
    /// to the ONE process fleet materialization per role, and any
    /// divergent-cfg rider is ledgered (`boot_stamp::record_ride`). The
    /// non-test build carries it write-only (the stamp's consumers are the
    /// install-time ledger + the per-role statics); test builds read it
    /// through the white-box probe accessor.
    #[cfg_attr(not(test), expect(dead_code))]
    fleet_boot_stamp: BootStamp,
    /// Does the core hold a configured `max_age` for the V3/V4
    /// buffered-event expiry? With the cockpit default (`max_age=None`)
    /// `expire` is a provable no-op, so the stage cycle must not take a core
    /// write for it — each one bought a ~2.9s writer-queue slot under the
    /// block-apply stream. Flipped by [`Self::set_event_buffer_max_age`].
    event_buffer_expiry_enabled: bool,
}
impl Default for ArbitrageEngine {
    fn default() -> Self {
        Self::new()
    }
}
impl ArbitrageEngine {
    /// Create a new engine with its **own** standalone `BotState` (standard
    /// allocation). ADR-006 D1: prefer [`ArbitrageEngine::with_core`] on the live
    /// path so the engine shares one `Arc<RwLock<BotState>>` with `PyBot`/handles;
    /// this no-arg ctor is the standalone-Rust / no-`pyo3`-test convenience.
    #[must_use]
    pub fn new() -> Self {
        Self::with_core(Arc::new(StateLock::new(BotState::new())))
    }
    /// Adopt an existing shared `Arc<RwLock<BotState>>` (ADR-006 D1+D2). The
    /// engine reads/writes pool state through the *same* core that
    /// `PyBot`/`PyLiquidityPool`/`PyErc20Token` share — dissolving the
    /// dual-`BotState` split the §17 stale-state caveat documented. Lock order
    /// remains engine-then-core; the engine's `Mutex<ArbitrageEngine>` engine
    /// state is still engine-local (ADR-006 D2 — engine keeps its own lock
    /// for path/solver state; only the core lock type/flavor changes).
    #[must_use]
    pub fn with_core(core: Arc<StateLock<BotState>>) -> Self {
        // pack from the INSTALLED loader
        // config (the _ffi module init installs the env/file-loaded BotConfig
        // before any engine construction). A fresh `BotConfig::default()`
        // here meant the python-driven pump's engine never observed
        // construction stances like fleet.stance — schema defaults only
        // apply when no owner installed one (tests / standalone clean-env
        // constructions, byte-compatible per the holder docs).
        Self::with_core_cfg(core, ::degenbot_config::holder::config_arc())
    }
    /// config-threaded construction. `cfg` is the typed `BotConfig`
    /// (loaded ONCE by the owner from the `--config` file / env via the
    /// degenbot-config loader) — the engine packs its construction stances
    /// from it and threads the solver runtime stance down per instance.
    #[must_use]
    pub fn with_core_cfg(
        core: Arc<StateLock<BotState>>,
        cfg: &std::sync::Arc<::degenbot_config::BotConfig>,
    ) -> Self {
        // Construction stances come from the CALLER's
        // own cfg — never from an install-then-read process static. A
        // parallel construction flips such a static between our install and
        // a global read (TOCTOU). The install call remains for its process
        // projections — the construction-STAMPED fleet boots; the
        // stance statics other consumers observe). LW-T9: there is no
        // stance — the fleet installs unconditionally, it is the only
        // behavior.
        let streaming_delivery = cfg.pump.streaming_delivery;
        let resolve_par_stance = cfg.solve.solve_resolve_par;
        // the engine's construction knobs are packed ONCE by
        // the typed retune value (retune.rs) from the CALLER's own cfg — never
        // an install-then-read process static. The local value is applied
        // below, after the engine literal is assembled.
        let retune = EngineRetune::from_config(cfg);
        // the engine OWNS its fleet boot — the stamp is
        // constructed from THIS cfg BEFORE the installer runs, so the
        // construction hand-off carries the caller's value, identified.
        let fleet_boot_stamp =
            BootStamp::of(degenbot_workers::dispatcher::FleetBoot::from_config(cfg));
        lifecycle::install_engine_stances(cfg, &fleet_boot_stamp);
        let cycle_core = Arc::clone(&core);
        let mut engine = Self {
            cfg: std::sync::Arc::clone(cfg),
            runtime_cfg: lifecycle::solve_runtime_config_from_cfg(cfg),
            core,
            registry: PathRegistry::new(),
            cycle: SolveCycle {
                path_resolved: HashMap::new(),
                path_status: HashMap::new(),
                hop_projection_cache: HopProjectionCache::new(),
                hop_projection_count: 0,
                concentrated_liquidity_projection_memo:
                    degenbot_substrate::resolve::projection_memo_enabled(),
                path_description_cache: parking_lot::Mutex::new(HashMap::new()),
                resolved_update_snapshot: HashMap::new(),
                cursor: BlockCursor::default(), // (0, None, 0, false) — the pre-cursor init, unchanged
                transients: solve_cycle::CycleTransients::default(),
                results: DashMap::new(),
                inline_payloads: DashMap::new(),
                solve_admission: retune.solve_admission,
                admission_target_depth: retune.admission_target_depth,
                admission_retention_blocks: retune.admission_retention_blocks,
                // the machine's pre-cycle init lives on the machine
                // (dormant Unopened, pipe closed, counters at 0).
                detached_cycle: detached_cycle::DetachedCycle::new(),
                scope: BlockScope::new(
                    std::sync::Arc::new(::degenbot_pools::WalkMemo::new(
                        cfg.solve.solver_walk_memo,
                        cfg.solve.solver_walk_memo_stats,
                    )),
                    std::sync::Arc::new(::degenbot_pools::WalkTelemetry::new()),
                ),
                min_profit_floor: alloy::primitives::U256::from(cfg.solve.min_profit_wei),
                clamp_margin: retune.clamp_margin,
                inline_sim_enabled: cfg.solve.solve_inline_sim,
                hooked_leg_requote: None,
                path_cost_history: std::sync::Arc::new(
                    crate::arb_engine::path_cost_ledger::PathCostHistory::new(),
                ),
                streaming_delivery,
                deferred_re_record: None,
                resolve_par_stance,
                core: cycle_core,
                cfg: std::sync::Arc::clone(cfg),
                runtime_cfg: lifecycle::solve_runtime_config_from_cfg(cfg),
                inline_sim: None,
                #[cfg(test)]
                test_solve_delay: None,
                #[cfg(test)]
                test_solve_panic: None,
                force_deferred: retune.force_deferred.clone(),
                #[cfg(test)]
                merge_probe: None,
                #[cfg(test)]
                test_merge_panic: None,
                #[cfg(test)]
                test_sync_merge: true,
                #[cfg(test)]
                test_merge_rx: None,
            },
            delivery: DeliveryPolicy::default(),
            fleet_boot_stamp,
            event_buffer_expiry_enabled: false,
        };
        // apply the config-derived retune ONCE at construction
        // the engine's per-instance knob values come from the caller's cfg.
        engine.apply_retune(&retune);
        engine
    }
    /// Apply one [`EngineRetune`] to the live engine — the ONE knob-write
    /// body, shared by construction ([`Self::with_core_cfg`]) and the runtime
    /// operator retune entry ([`super::EngineStages::apply_retune`]).
    ///
    /// The event-buffer core write is skipped unless a max age is configured
    /// or one was enabled before (clearing `Some` → `None` must still reach the
    /// core); every other knob is an in-memory instance value.
    pub(crate) fn apply_retune(&mut self, retune: &EngineRetune) {
        if retune.event_buffer_max_age.is_some() || self.event_buffer_expiry_enabled {
            self.event_buffer_expiry_enabled = retune.event_buffer_max_age.is_some();
            self.core
                .write_at(degenbot_substrate::state_lock::LockSite::Solver)
                .concentrated_liquidity_mut()
                .set_v3_buffer_max_age(retune.event_buffer_max_age);
            self.core
                .write_at(degenbot_substrate::state_lock::LockSite::Solver)
                .concentrated_liquidity_mut()
                .set_v4_buffer_max_age(retune.event_buffer_max_age);
        }
        // The admission trio.
        self.cycle.solve_admission = retune.solve_admission;
        self.cycle.admission_target_depth = retune.admission_target_depth;
        self.cycle.admission_retention_blocks = retune.admission_retention_blocks;
        // The registered-path cap (PRG-4).
        self.registry.set_cap(retune.path_cap);
        // The delivery profit window.
        self.delivery
            .set_profit_thresholds(retune.min_profit, retune.max_profit);
        // The diagnostic force-deferred override.
        self.cycle.clamp_margin = retune.clamp_margin;
        self.cycle.force_deferred.clone_from(&retune.force_deferred);
    }
    /// Immutable access to the shared `BotState` `Arc` (ADR-003 / ADR-006
    /// D1+D2).
    ///
    /// The shared `Arc<RwLock<BotState>>` cannot be reassigned through this
    /// accessor: callers may `read()`/`write()` *through* it, but its identity
    /// stays pinned to the same `Arc` the `EngineStages` stage surface/pump
    /// reference (an invariant no downstream crate can break by swapping the
    /// field).
    #[must_use]
    pub fn core(&self) -> &Arc<StateLock<BotState>> {
        &self.core
    }
    /// Immutable read of the registered path→pool map.
    ///
    /// Read-only: `path_pools` must stay consistent with the `pool_to_paths`
    /// reverse index, which only the engine's internal register/deregister
    /// paths maintain — so no mutable accessor is exposed.
    #[must_use]
    pub fn path_pools(&self) -> &HashMap<u64, std::sync::Arc<MixedPath>> {
        self.registry.path_pools()
    }
    // Production code never registers pools via the engine — pool construction
    // is a `BotState` concern, and the engine discovers pools at `register_path`
    // time by resolving `pool_id`s against the associated `BotState`. These
    // helpers exist so no-pyo3 tests can seed the engine's `BotState` (its
    // `core`) with the same ergonomics the old production `register_v*_pool`
    // methods had; they delegate straight to `BotState::register_*`.
    /// Register a V2 pool into the engine's `BotState` and return its `pool_id`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `BotState::register_v2_pool` rejects the
    /// params (duplicate address, or spec-violating reserve). This is a test
    /// convenience — production code goes through `PyBot::register_v2_pool`,
    /// which surfaces the rejection as a typed Python exception.
    #[cfg(test)]
    #[expect(clippy::expect_used)] // test convenience: assert registration succeeds
    #[must_use]
    pub fn register_v2_pool(
        &self,
        address: Address,
        reserve0: U112,
        reserve1: U112,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> u64 {
        let params = degenbot_pools::RegisterV2PoolParams {
            address,
            token0: Address::ZERO,
            token1: Address::ZERO,
            reserve0,
            reserve1,
            fee_token0: fee_ratio(gamma_numer, fee_denom),
            fee_token1: fee_ratio(gamma_numer, fee_denom),
            factory: Address::ZERO,
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
        };
        self.core
            .write_at(degenbot_substrate::state_lock::LockSite::Solver)
            .register_v2_pool(&params)
            .expect("test setup: V2 registration")
    }
    /// Register a V3 pool into the engine's `BotState` and return its `pool_id`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `BotState::register_v3_pool` rejects the
    /// params (duplicate address, or spec-violating `sqrt_price_x96` / `tick`
    /// / `fee` / `tick_spacing`). This is a test convenience — production
    /// code goes through `PyBot::register_v3_pool`, which surfaces the
    /// rejection as a typed Python exception.
    #[cfg(test)]
    #[expect(clippy::expect_used)] // test convenience: assert registration succeeds
    #[must_use]
    pub fn register_v3_pool(&self, params: &degenbot_pools::RegisterV3PoolParams) -> u64 {
        self.core
            .write_at(degenbot_substrate::state_lock::LockSite::Solver)
            .register_v3_pool(params)
            .expect("test setup: V3 registration")
    }
    /// Register a V4 pool into the engine's `BotState` and return its `pool_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `BotState::register_v4_pool` rejects the pool
    /// (amount-modifying hooks, dynamic fee, or duplicate registration).
    #[cfg(test)]
    pub fn register_v4_pool(
        &self,
        params: &degenbot_pools::RegisterV4PoolParams,
    ) -> Result<u64, degenbot_pools::RegisterV4PoolError> {
        self.core
            .write_at(degenbot_substrate::state_lock::LockSite::Solver)
            .register_v4_pool(params)
    }
}
