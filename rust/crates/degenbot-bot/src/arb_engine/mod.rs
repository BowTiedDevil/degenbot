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
//! - A [`BotState`](crate::bot_core::BotState) for V2+V3 pool state (ADR-003:
//!   `BotState` is the single state owner, peer to this engine)
//! - A [`BotState`](crate::bot_core::BotState) for all pool state (V2+V3+V4 —
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
//! Hook filtering: V4 pools with amount-modifying hooks are rejected at
//! registration time in the V4 engine. The unified engine never sees them.
//!
//! # Module layout
//!
//! | Module | Concern |
//! |--------|---------|
//! | [`event_routing`] | Log event routing, block processing, backfill |
//! | [`solver_dispatch`] | Path resolution, solver dispatch, rebuild logic |
//! | [`delivery_lifecycle`] | Delivery lifecycle: channel open/send/close + the end-of-stream contract (incident 2026-08-20 #2) |
//! | [`delivery_policy`] | Delivery policy: diff computation, thresholds, delivered-bookkeeping (BI7UZV) |
//! | [`block_cursor`] | The engine block cursor — one owner of the engine-side block-coordinate residue (6XB6NJ) |
//! | [`detached_cycle`] | THE one detached/in-cycle solve-arm machine: per-cycle states, the merge pipe, the gauge pair, the seq counters, the ledger door, the disposition counters, the fan-in tripwire, and the ONE sidecar spawn (P37YJG) |
//! | [`lifecycle`] | Path registration, buffer management, engine accessors |
//! | [`py_binding`] | PyO3 wrapper (`PyArbitrageEngine`) |
//! | [`tests`] | Unit tests |

use dashmap::DashMap;
use hashbrown::{HashMap, HashSet};
use std::sync::Arc;

use ::degenbot_solvers::mixed::{MixedPath, SolvePathResult};
#[cfg(test)]
use alloy::primitives::aliases::U112;
use alloy::primitives::Address;

use self::block_cursor::BlockCursor;
use self::boot_stamp::BootStamp;
use self::delivery_policy::DeliveryPolicy;
use self::path_registry::PathRegistry;
use self::solve_cycle::SolveCycle;
use crate::bot_core::resolve::HopProjectionCache;
use crate::bot_core::state_lock::StateLock;
use crate::bot_core::BotState;

// THE construction-stamped fleet boot carrier (YI5NGB): the engine's own
// FleetBoot value + the ride ledger the per-role fleet statics consult.
mod boot_stamp;
// 6XB6NJ: the ONE engine block cursor — the consolidated owner of the
// engine-side block-coordinate residue (completes ADR-041 §3.5's
// engine-side anchor fold; see the module's own doc header).
pub(crate) mod block_cursor;
// Sub-modules — each contains `impl ArbitrageEngine` or `impl PyArbitrageEngine` blocks.
mod delivery_lifecycle;
mod delivery_policy;
// THE one detached/in-cycle solve-arm machine (P37YJG): the per-cycle
// states, the merge pipe, the gauge pair, the seq counters, the ledger
// door, the disposition counters, the fan-in tripwire, and the ONE
// sidecar spawn — see the module's own doc header.
pub(crate) mod detached_cycle;

mod diagnostic;
// SZJUKL seam retirement: the arb engine's StageHandlers implementation —
// the ONE surface left between the machine driver and the engine. The
// dissolved `engine_handle` (wrapper Mutex), `engine_subscriber` (liveness
// adapter), and `epoch_delta_parity`/`test_oracle` (the LXDY4C parity
// oracle, GONE now that `EpochDelta` is sole authority) are deleted —
// hard cutover, Q6.
pub mod engine_stages;
mod event_routing;
pub(crate) mod executor;
pub mod fleet_intake;
mod fleet_registration_executor;
mod fleet_sim_executor;
pub(crate) mod fleet_solve_executor;
pub mod fleet_status;
pub mod inline_sim;
pub mod lifecycle;
pub mod path_info;
mod path_lifecycle;
// ADR-045 (`C4UAFP`): the path-identity registry (`PathRegistry`) —
// registered paths, reverse index, signatures, id allocator, cap, dedups.
mod path_registry;
// RZEWTX: the ONE pooled-seat host for the WorkQueue fleet roles (sim +
// registration) — the executors are thin role descriptors over it; the
// solve executor's exclusion (per-seat channel model + posture-invariant
// typed-submit admission) is documented in the module doc.
pub mod fleet_wake;
mod seat_host;
mod snapshot_verify;
mod solver_dispatch;
// 5WCRWZ T1: heavy-path capture diagnostics, extracted from the solver_dispatch
// grab file (import-only move; the module owns its honesty probe + test island).
mod solver_capture;
// 5WCRWZ T2: the pure workload-analysis cluster (solve-bin sizing, LPT
// partition, named cordon-fallback seat plan, resolve-time cost proxies),
// extracted from the solver_dispatch grab file (import-only move; the module
// owns its honesty probe + test islands).
mod workload_partition;
// ADR-045 T1 (`E7V2S6`): the solve-cycle data-type seam (`CycleOutcome` /
// `CycleArm` / `ResolveCensus` / `Registration`) — T3/T4 assemble
// `SolveCycle` on top of it; nothing consumes the types yet.
mod solve_cycle;
#[cfg(test)]
mod tests;

pub use engine_stages::EngineStages;

pub use diagnostic::{
    compute_field_diffs, DiagnosticHop, DiagnosticPathState, DiagnosticPoolState, FieldDiff,
};
pub use inline_sim::{
    AccessListRow, CapturedSwapRow, InlineSimFailure, InlineSimRequest, InlineSimulator,
    InlineSwapFamily, SimulatedPathResult,
};
pub use path_info::build_path_info;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
// Engine phase state machine (Plan 098)
// ---------------------------------------------------------------------------

/// Lifecycle phase of the engine, enforcing correct ordering of
/// `subscribe()`, `load_snapshot()`, `backfill()`, and `resume()`.
///
/// Transitions:
/// ```text
/// Created ──subscribe()──► Subscribed ──load_snapshot()──► SnapshotLoaded
///                                                        ──backfill()──► Backfilled
///                                                        ──resume()──► Resumed
///
/// Construction-time-load path (RUQ637/TJT63P): snapshot loaded at `Bot`
/// construction BEFORE subscribe. The snapshot lives in the shared core
/// `BotState` and never advances the engine phase, so `subscribe()` uses
/// `EnginePhase::after_subscribe(current, core_has_snapshot)` to reflect
/// reality — landing at `SnapshotLoaded` (not `Subscribed`) so `resume()`
/// (which requires `>= SnapshotLoaded`) is reachable:
/// Created ──load_snapshot_from_db()──[core has snapshot]──► subscribe()
///        ──► SnapshotLoaded ──resume()──► Resumed
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum EnginePhase {
    /// Engine just created, no connections.
    Created = 0,
    /// WS `subscribe()` completed, first block observed.
    Subscribed = 1,
    /// Snapshot data loaded into Rust (at least one of V3/V4).
    SnapshotLoaded = 2,
    /// Backfill from snapshot block to first WS block completed.
    Backfilled = 3,
    /// Pump processing live blocks.
    Resumed = 4,
}

impl EnginePhase {
    /// Reconstruct a phase from its `u8` discriminant (the inverse of the
    /// `#[repr(u8)]` representation). Used by `PumpState` (ADR-006 D4) to read
    /// the phase atomically across PyBot/PyArbitrageEngine wrappers.
    /// Unknown discriminants fall back to `Created` (the safest default — any
    /// phase-gated method will re-validate via `require`/`require_before`).
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Subscribed,
            2 => Self::SnapshotLoaded,
            3 => Self::Backfilled,
            4 => Self::Resumed,
            _ => Self::Created,
        }
    }

    /// Check that the current phase allows the given required phase.
    /// Returns `Err` with a descriptive message if the transition is invalid.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` describing the invalid transition when the
    /// engine's phase is below `required`.
    pub fn require(self, required: Self, method_name: &str) -> Result<(), String> {
        if self >= required {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is in phase {self:?}, but requires {required:?}"
            ))
        }
    }

    /// Require that the engine has not yet reached the given phase.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` describing the invalid transition when the
    /// engine has already reached `phase`.
    pub fn require_before(self, phase: Self, method_name: &str) -> Result<(), String> {
        if self < phase {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is already in phase {self:?} (requires before {phase:?})"
            ))
        }
    }

    /// Phase gate for `subscribe()`. Accepts `Created` (the legacy path:
    /// subscribe first, then load snapshot) OR `SnapshotLoaded` (the
    /// construction-time-load path: snapshot loaded at `Bot` construction,
    /// then subscribe). Rejects `Subscribed`/`Backfilled`/`Resumed`.
    ///
    /// The numeric ordering cannot express this with `require`/`require_before`
    /// alone (SnapshotLoaded=2 > Subscribed=1), so this is an explicit match.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` when the phase is past the subscribe-allowed
    /// window.
    pub fn allow_subscribe(self, method_name: &str) -> Result<(), String> {
        match self {
            Self::Created | Self::SnapshotLoaded => Ok(()),
            other => Err(format!(
                "Cannot call {method_name}: engine is in phase {other:?}, but subscribe requires Created or SnapshotLoaded"
            )),
        }
    }

    /// Compute the phase AFTER `subscribe()` completes (J3FMDO regression
    /// fix for the construction-time-load path).
    ///
    /// `PumpState::subscribe` used to unconditionally `set_phase(Subscribed)`,
    /// which is correct for the legacy path (`Created → subscribe →
    /// Subscribed → load_*_snapshot_from_py → SnapshotLoaded → resume`) but
    /// crashes the construction-time-load path (RUQ637/TJT63P): the snapshot
    /// is loaded into the shared core `BotState` at `Bot` construction —
    /// BEFORE subscribe — and never advances the engine phase. After
    /// subscribe, the phase was `Subscribed` (1), and `resume()`'s
    /// `require(SnapshotLoaded)` guard (needs `>= 2`) crashed the production
    /// settlement-arbitrage bot:
    ///
    /// ```text
    /// RuntimeError: Cannot call resume: engine is in phase Subscribed,
    ///               but requires SnapshotLoaded
    /// ```
    ///
    /// This helper reflects reality: if the engine phase is ALREADY at
    /// `SnapshotLoaded` (the legacy pre-subscribe load via
    /// `load_*_snapshot_from_py`) OR the core has a snapshot loaded
    /// (`core_has_snapshot` — the construction-time-load path), the phase
    /// after subscribe is `SnapshotLoaded`. Otherwise `Subscribed` (the
    /// legacy path that loads the snapshot AFTER subscribe).
    ///
    /// `allow_subscribe` guarantees `current ∈ {Created, SnapshotLoaded}` at
    /// call time (subscribe rejects `Subscribed`/`Backfilled`/`Resumed`), but
    /// the `SnapshotLoaded` arm is preserved for completeness so a future
    /// caller that reaches `after_subscribe` with a higher phase does not
    /// regress.
    #[must_use]
    pub fn after_subscribe(current: Self, core_has_snapshot: bool) -> Self {
        match current {
            // Already at or past SnapshotLoaded — subscribe must NOT regress
            // the phase (the legacy pre-subscribe load + any future path).
            Self::SnapshotLoaded | Self::Backfilled | Self::Resumed => current,
            // Created — advance based on whether the core already has a
            // snapshot (construction-time-load path) or not (legacy path).
            Self::Created if core_has_snapshot => Self::SnapshotLoaded,
            Self::Created | Self::Subscribed => Self::Subscribed,
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage & snapshot types
// ---------------------------------------------------------------------------

/// Describes the completeness of tick data for a registered pool.
///
/// Re-exported from [`crate::bot_core`] where it now lives alongside V3 state
/// (ADR-003). `Tracked` = complete (may have empty `tick_data` = genuinely
/// illiquid); `Sparse` = no snapshot data — solver results may be inaccurate.
pub use crate::bot_core::PoolTickCoverage;

// ADR-015: the path/snapshot/hop-state value types + INT128_MAX constant
// now live in `degenbot-solvers::mixed` (the solver's intake contract). The
// re-export shim that lived here during the staged relocation has been
// dropped — callers import the solver types directly from
// `::degenbot_solvers::mixed::{...}`. `BlockMetadata`, `PoolTickCoverage`,
// `V3SnapshotData`/`V4SnapshotData` (if needed) resolve to their original
// homes (`bot_core`, `degenbot_solvers::mixed`).

// `BlockMetadata` lives in `bot_core` (general block data); re-exported here so
// engine code + external references (`crate::arb_engine::BlockMetadata`)
// keep working (ADR-006 D4).
pub use crate::bot_core::BlockMetadata;
// ADR-027 completion (2026-08-20 review): the block-clock pipe is
// coordinator-owned; the type moved to bot_core. Re-exported so external
// references keep working (same pattern as BlockMetadata above).
pub use degenbot_core::block_clock_pipe::BlockNotification;

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

/// KJWIK5: the deferred-path re-record hook (the ledger carry). The
/// dispatch calls it with the deferred paths' hop-pool keys and the cycle's
/// solve block; the `EngineStages` constructor installs the
/// `EpochDelta::record` closure so a deferred path re-enters through the
/// next draw (same freshness ordering, admission budget, retention window —
/// one deferral concept). `None` on direct engine drives (unit tests, the
/// cold-start `solve_all`): the deferral then keeps its pre-fold dropped
/// behavior.
pub(crate) type DeferredReRecordHook =
    std::sync::Arc<dyn Fn(&[degenbot_solvers::affected_keys::AffectedKey], u64) + Send + Sync>;

/// The unified Uniswap engine — owns V2, V3, and V4 pool state and solves
/// mixed arbitrage paths.
///
/// V2 pool state lives in [`BotState`] (ADR-003: the single Rust state owner,
/// peer to this engine). The engine holds the shared `Arc<RwLock<BotState>>`
/// (ADR-006 D1+D2 — `RwLock` on the core, shared with [`PyBot`] via
/// [`ArbitrageEngine::with_core`]; `new()` standalone sugar allocates its own)
/// and reads/writes pool state through it. Lock ordering when nested is
/// **engine-then-core** — no code path ever nests core-then-engine.
pub struct ArbitrageEngine {
    /// KAHU5W: the owner-loaded typed bot config (one loader process-wide;
    /// never re-read from the environment). Construction stances + capture
    /// config read from here. ADR-045 T4: the solve cycle owns its own clone
    /// for the resolve/solve bodies; this field remains for the construction
    /// stances.
    #[expect(dead_code)]
    pub(crate) cfg: std::sync::Arc<::degenbot_config::BotConfig>,
    /// KAHU5W: the instance solver runtime stance, built at construction from
    /// [`Self::cfg`] and threaded down into every solve cycle. Replaces the
    /// solver crate's removed process-global RUNTIME `OnceLock`.
    /// ADR-045 T4: the cycle owns its own copy.
    #[expect(dead_code)]
    runtime_cfg: ::degenbot_solvers::runtime::SolveRuntimeConfig,
    /// V2 + V3 + V4 pool state owner (ADR-003). The shared
    /// `Arc<RwLock<BotState>>` (ADR-006 D1+D2): read methods take a read guard,
    /// mutations a write guard. Lock ordering when nested is
    /// engine-then-core; no code path ever nests in the opposite direction.
    pub(crate) core: Arc<StateLock<BotState>>,
    /// ADR-045 (`C4UAFP`): the engine's path-identity registry — registered
    /// paths, the `pool_to_paths` reverse index, signature dedup, the path-id
    /// allocator, the registered-path cap, and the dedup counter. Hot solves
    /// read it through a shared `&PathRegistry`; only registration takes
    /// `&mut`. The invariant "`path_pools` is consistent with `pool_to_paths`"
    /// is internal to [`PathRegistry::commit`] / [`PathRegistry::remove`].
    pub(crate) registry: PathRegistry,
    /// ADR-045 (`ANVHXW`): the solve cycle's owned state — the resolve
    /// companions, the cycle-transient stash, the solve output, the admissions
    /// stance, the detached-arm collaborator, the walk/projection recorders,
    /// and the seven `for_test` knobs. Fields only at T3; behavior methods move
    /// at T4. The engine keeps identity (`registry`), the shared core, the
    /// delivery policy, the lifecycle phase, and the construction stances.
    pub(crate) cycle: SolveCycle,
    /// Delivery policy — the optional publish sink that consumes the solve
    /// output (`latest_results`) and pushes diffs over the result/block
    /// channels (ergo BI7UZV). Owns `delivered`/`deregistered`, the profit
    /// thresholds, and `result_tx`/`block_tx`; decoupled from solve state so a
    /// standalone consumer gets raw results without it.
    pub(crate) delivery: DeliveryPolicy,
    /// THE construction-stamped fleet boot (YI5NGB): the engine's OWN
    /// `FleetBoot`, derived from the CALLER's cfg at construction and
    /// stamped with the engine id + a deterministic cfg hash. Packed in
    /// `with_core_cfg` right beside `streaming_delivery` (the KAHU5W
    /// construction-stance pattern); the per-role fleet statics courier it
    /// to the ONE process fleet materialization per role, and any
    /// divergent-cfg rider is ledgered (`boot_stamp::record_ride`). The
    /// non-test build carries it write-only (the stamp's consumers are the
    /// install-time ledger + the per-role statics); test builds read it
    /// through the white-box probe accessor.
    #[cfg_attr(not(test), expect(dead_code))]
    fleet_boot_stamp: BootStamp,
    /// Engine lifecycle phase (ZU7RAF — core-OWNED). Enforces ordering
    /// `Created → Subscribed → SnapshotLoaded → Backfilled → Resumed`.
    /// Previously the `AtomicU8` lived on the pyo3 `PumpState` wrapper;
    /// moving it to the core engine lets a standalone Rust consumer observe +
    /// guard the lifecycle with no Python in the build. Read/written via
    /// [`Self::current_phase`] / [`Self::set_phase`]; the engine sits behind
    /// `Arc<Mutex<..>>` so the atomic read is lock-free across the pyo3
    /// wrappers and the pump task.
    phase: std::sync::atomic::AtomicU8,
    /// LPEOBI: does the core hold a configured `max_age` for the V3/V4
    /// buffered-event expiry? With the cockpit default (`max_age=None`)
    /// `expire` is a provable no-op, so `solve_dirty` must not take a core
    /// write for it — each one bought a ~2.9s writer-queue slot under the
    /// block-apply stream. Flipped by [`Self::set_event_buffer_max_age`].
    event_buffer_expiry_enabled: bool,
    /// The inline-sim hook (SIMPIPE2 T1): `degenbot-python` installs the
    /// implementation at engine construction via
    /// [`ArbitrageEngine::set_inline_simulator`]; `None` = stance-relevant
    /// callers fall back to the batch FFI sim (module `inline_sim` doc).
    inline_sim: Option<std::sync::Arc<dyn inline_sim::InlineSimulator>>,
}

impl ArbitrageEngine {
    /// The pump ended: close the delivery channels so Python's block/result
    /// streams end loudly (incident 2026-08-20 #2). The `StageHandlers`
    /// liveness hook's answer — see [`DeliveryLifecycle::close`] for the
    /// end-of-stream contract.
    pub fn on_pump_ended(&mut self) {
        self.delivery.lifecycle.close();
    }
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
    /// Probe the packed delivery stance (smoke-boot observability; reads no
    /// environment — the field was packed from the typed config at
    /// construction).
    #[must_use]
    pub fn streaming_delivery_probe(&self) -> bool {
        self.cycle.streaming_delivery
    }

    #[must_use]
    pub fn with_core(core: Arc<StateLock<BotState>>) -> Self {
        // KAHU5W/P6YXA6 production-boot fix: pack from the INSTALLED loader
        // config (the _ffi module init installs the env/file-loaded BotConfig
        // before any engine construction). A fresh `BotConfig::default()`
        // here meant the python-driven pump's engine never observed
        // construction stances like fleet.stance — schema defaults only
        // apply when no owner installed one (tests / standalone clean-env
        // constructions, byte-compatible per the holder docs).
        Self::with_core_cfg(core, ::degenbot_config::holder::config_arc())
    }

    /// KAHU5W: config-threaded construction. `cfg` is the typed `BotConfig`
    /// (loaded ONCE by the owner from the `--config` file / env via the
    /// degenbot-config loader) — the engine packs its construction stances
    /// from it and threads the solver runtime stance down per instance.
    #[must_use]
    pub fn with_core_cfg(
        core: Arc<StateLock<BotState>>,
        cfg: &std::sync::Arc<::degenbot_config::BotConfig>,
    ) -> Self {
        // J4HN66 (epic 64ZQLA): construction stances come from the CALLER's
        // own cfg — never from an install-then-read process static. A
        // parallel construction flips such a static between our install and
        // a global read (TOCTOU). The install call remains for its process
        // projections — the construction-STAMPED fleet boots (YI5NGB); the
        // stance statics other consumers observe). LW-T9: there is no
        // stance — the fleet installs unconditionally, it is the only
        // behavior.
        let streaming_delivery = cfg.pump.streaming_delivery;
        let resolve_par_stance = cfg.solve.solve_resolve_par;
        // QTZGFL: the admission stance + its two typed knobs (KAHU5W
        // construction-stance pattern — packed ONCE here, never re-read).
        // `DEGENBOT_SOLVE_ADMISSION` parse matrix (supervisor-confirmed
        // conservative default): unset/0/false ⇒ OFF (current degrade,
        // byte-identical); 1/true/on ⇒ the capacity-modulated draw; any other
        // word fails config load loudly (the loader owns the words). The
        // target is clamped to the design-locked safety valve: it is the SAME
        // number as the in-flight cap, just made explicit/tunable.
        let solve_admission = !cfg!(test) && cfg.solve.admission_shed;
        let admission_target_depth = u64::try_from(cfg.solve.admission_target_depth)
            .unwrap_or(detached_cycle::DETACHED_INFLIGHT_CAP)
            .clamp(1, detached_cycle::DETACHED_INFLIGHT_CAP);
        let admission_retention_blocks = cfg.solve.admission_retention_blocks;
        // YI5NGB: the engine OWNS its fleet boot (KAHU5W) — the stamp is
        // constructed from THIS cfg BEFORE the installer runs, so the
        // construction hand-off carries the caller's value, identified.
        let fleet_boot_stamp =
            BootStamp::of(degenbot_workers::dispatcher::FleetBoot::from_config(cfg));
        solver_dispatch::install_engine_stances(cfg, &fleet_boot_stamp);
        let cycle_core = Arc::clone(&core);
        Self {
            cfg: std::sync::Arc::clone(cfg),
            runtime_cfg: solver_dispatch::solve_runtime_config_from_cfg(cfg),
            core,
            registry: PathRegistry::new(),
            cycle: SolveCycle {
                path_resolved: HashMap::new(),
                path_status: HashMap::new(),
                hop_projection_cache: HopProjectionCache::new(),
                hop_projection_count: 0,
                cl_projection_memo: crate::bot_core::resolve::projection_memo_enabled(),
                path_description_cache: parking_lot::Mutex::new(HashMap::new()),
                resolved_update_snapshot: HashMap::new(),
                cursor: BlockCursor::default(), // (0, None, 0, false) — the pre-cursor init, unchanged
                admission_draw_zero: false,
                last_arm: None,
                pending_new_paths: HashSet::new(),
                paths_same_state_this_cycle: 0,
                results: DashMap::new(),
                inline_payloads: DashMap::new(),
                solve_admission,
                admission_target_depth,
                admission_retention_blocks,
                // P37YJG: the machine's pre-cycle init lives on the machine
                // (dormant Unopened, pipe closed, counters at 0).
                detached_cycle: detached_cycle::DetachedCycle::new(),
                walk_memo: std::sync::Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
                    cfg.solve.solver_walk_memo,
                    cfg.solve.solver_walk_memo_stats,
                )),
                last_walk_sims: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
                last_gate_us: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
                streaming_delivery,
                deferred_re_record: None,
                resolve_par_stance,
                core: cycle_core,
                cfg: std::sync::Arc::clone(cfg),
                runtime_cfg: solver_dispatch::solve_runtime_config_from_cfg(cfg),
                inline_sim: None,
                #[cfg(test)]
                test_solve_delay: None,
                #[cfg(test)]
                test_solve_panic: None,
                #[cfg(test)]
                test_force_deferred: None,
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
            phase: std::sync::atomic::AtomicU8::new(EnginePhase::Created as u8),
            event_buffer_expiry_enabled: false,
            inline_sim: None,
        }
    }
}

impl ArbitrageEngine {
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
}

impl ArbitrageEngine {
    /// Read the current engine lifecycle phase (core-owned source of truth,
    /// ZU7RAF).
    /// Atomic + lock-free (the engine is behind `Arc<Mutex<..>>` across the
    /// pyo3 wrappers and the pump). Reconstructs from the `u8` discriminant;
    /// an unknown discriminant falls back to `Created` (the safest default —
    /// any phase-gated method re-validates via `require`/`require_before`).
    #[must_use]
    pub fn current_phase(&self) -> EnginePhase {
        EnginePhase::from_u8(self.phase.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Advance to `phase` with NO ordering check — the caller validates against
    /// the gated helpers [`Self::require_phase`] / [`Self::require_phase_before`]
    /// (or `EnginePhase::allow_subscribe` / `after_subscribe`). Core-owned
    /// source of truth.
    pub fn set_phase(&self, phase: EnginePhase) {
        self.phase
            .store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Gate a lifecycle-ordered method: succeed only when the current phase is
    /// at or past `required`.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` describing the invalid transition when the phase
    /// is below `required`.
    pub fn require_phase(&self, required: EnginePhase, method_name: &str) -> Result<(), String> {
        self.current_phase().require(required, method_name)
    }

    /// Gate a lifecycle-ordered method: succeed only when the current phase is
    /// strictly BEFORE `phase`.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` when the engine has already reached `phase`.
    pub fn require_phase_before(
        &self,
        phase: EnginePhase,
        method_name: &str,
    ) -> Result<(), String> {
        self.current_phase().require_before(phase, method_name)
    }
}

/// Test-only registration helpers (ADR-006 D3).
///
/// Production code never registers pools via the engine — pool construction
/// is a `BotState` concern, and the engine discovers pools at `register_path`
/// time by resolving `pool_id`s against the associated `BotState`. These helpers
/// exist so no-pyo3 tests can seed the engine's `BotState` (its `core`) with the
/// same ergonomics the old production `register_v*_pool` methods had; they
/// delegate straight to `BotState::register_*`.
#[cfg(test)]
#[expect(clippy::expect_used)] // test convenience: assert registration succeeds
impl ArbitrageEngine {
    /// Register a V2 pool into the engine's `BotState` and return its `pool_id`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `BotState::register_v2_pool` rejects the
    /// params (duplicate address, or spec-violating reserve). This is a test
    /// convenience — production code goes through `PyBot::register_v2_pool`,
    /// which surfaces the rejection as a typed Python exception.
    #[must_use]
    pub fn register_v2_pool(
        &self,
        address: Address,
        reserve0: U112,
        reserve1: U112,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> u64 {
        let params = crate::bot_core::RegisterV2PoolParams {
            address,
            token0: Address::ZERO,
            token1: Address::ZERO,
            reserve0,
            reserve1,
            fee_token0: (gamma_numer, fee_denom),
            fee_token1: (gamma_numer, fee_denom),
            factory: Address::ZERO,
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        };
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
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
    #[must_use]
    pub fn register_v3_pool(&self, params: &crate::bot_core::RegisterV3PoolParams) -> u64 {
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_v3_pool(params)
            .expect("test setup: V3 registration")
    }

    /// Register a V4 pool into the engine's `BotState` and return its `pool_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `BotState::register_v4_pool` rejects the pool
    /// (amount-modifying hooks, dynamic fee, or duplicate registration).
    pub fn register_v4_pool(
        &self,
        params: &crate::bot_core::RegisterV4PoolParams,
    ) -> Result<u64, crate::bot_core::RegisterV4PoolError> {
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_v4_pool(params)
    }
}

// ---------------------------------------------------------------------------
// Epic BXUSGL T1: test-only knobs. Never compiled outside `cargo test` — the
// streaming-orchestration test needs a deterministic per-path delay and an
// observation point on the drain.
// ---------------------------------------------------------------------------
#[cfg(test)]
impl ArbitrageEngine {
    pub(crate) fn set_solve_delay_hook(&mut self, hook: std::sync::Arc<dyn Fn(u64) + Send + Sync>) {
        self.cycle.test_solve_delay = Some(hook);
    }

    pub(crate) fn set_solve_panic_hook(&mut self, hook: std::sync::Arc<dyn Fn(u64) + Send + Sync>) {
        self.cycle.test_solve_panic = Some(hook);
    }

    /// KJWIK5 test seam: force the future-price deferral for `pids` (empty
    /// clears it). The real tripwire is unreachable after the solve-anchor
    /// head floor, so the carry is exercised through this seam.
    pub(crate) fn set_force_deferred_for_test(&mut self, pids: HashSet<u64>) {
        self.cycle.test_force_deferred = if pids.is_empty() { None } else { Some(pids) };
    }

    pub(crate) fn set_merge_probe(&mut self, probe: std::sync::Arc<parking_lot::Mutex<Vec<u64>>>) {
        self.cycle.merge_probe = Some(probe);
    }

    pub(crate) fn set_merge_panic_hook(&mut self, hook: std::sync::Arc<dyn Fn(u64) + Send + Sync>) {
        self.cycle.test_merge_panic = Some(hook);
    }

    /// WFF6MM test harness: toggle the inline merge drain. `EngineStages`
    /// turns it OFF before driving the engine (the sidecar owns the pipe
    /// there — see the field doc).
    pub(crate) fn set_sync_merge_for_test(&mut self, on: bool) {
        self.cycle.test_sync_merge = on;
    }

    // ADR-045 T4: the inline merge drain moved to `SolveCycle::drain_merge_inline`.

    pub(crate) fn set_streaming_delivery(&mut self, on: bool) {
        self.cycle.streaming_delivery = on;
    }

    /// YI5NGB (test-only F-suite probe): the engine's construction-stamped
    /// boot — lets the white-box tests verify twin constructions share a
    /// byte-identical boot value WITHOUT reaching into the fleet statics.
    #[cfg(test)]
    pub(crate) fn fleet_boot_stamp(&self) -> &BootStamp {
        &self.fleet_boot_stamp
    }

    /// QTZGFL: test seam for the admission stance. Production packs it from
    /// `cfg.solve.admission_shed` at construction (never re-read).
    #[cfg(test)]
    pub(crate) fn set_solve_admission(&mut self, on: bool) {
        self.cycle.solve_admission = on;
    }

    /// QTZGFL: test seam for the target depth — the clamp mirrors the
    /// construction clamp exactly.
    #[cfg(test)]
    pub(crate) fn set_admission_target_depth(&mut self, depth: usize) {
        self.cycle.admission_target_depth = u64::try_from(depth)
            .unwrap_or(detached_cycle::DETACHED_INFLIGHT_CAP)
            .clamp(1, detached_cycle::DETACHED_INFLIGHT_CAP);
    }

    /// QTZGFL: test seam for the retention window (blocks).
    #[cfg(test)]
    pub(crate) fn set_admission_retention_blocks(&mut self, window: u64) {
        self.cycle.admission_retention_blocks = window;
    }

    /// YI5NGB: A/B seam (TEST ONLY). The production stance is
    /// construction-frozen from `cfg.solve.solve_resolve_par` (KAHU5W);
    /// the parity test drives both arms through this mutator instead of
    /// flipping a process-global.
    #[cfg(test)]
    pub(crate) fn set_resolve_parallel_for_test(&mut self, on: bool) {
        self.cycle.resolve_par_stance = on;
    }
}
