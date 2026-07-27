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
//! | [`result_channel`] | Result batch channel, diff computation, de-registration |
//! | [`lifecycle`] | Path registration, buffer management, engine accessors |
//! | [`py_binding`] | PyO3 wrapper (`PyArbitrageEngine`) |
//! | [`tests`] | Unit tests |

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ::degenbot_solvers::mixed::{HopType, MixedPath, ResolvedMixedPath, SolvePathResult};
#[cfg(test)]
use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, U256};

use crate::bot_core::BotState;

// Sub-modules — each contains `impl ArbitrageEngine` or `impl PyArbitrageEngine` blocks.
#[allow(clippy::module_inception)]
mod diagnostic;
pub mod engine_handle;
pub mod engine_subscriber;
mod event_routing;
mod lifecycle;
pub mod path_info;
mod result_channel;
pub mod snapshot_verify;
mod solver_dispatch;
#[cfg(test)]
mod tests;

pub use diagnostic::{
    compute_field_diffs, DiagnosticHop, DiagnosticPathState, DiagnosticPoolState, FieldDiff,
};

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
    /// backrun bot:
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
// engine code + external references (`crate::solvers::arb_engine::BlockMetadata`)
// keep working (ADR-006 D4).
pub use crate::bot_core::BlockMetadata;

/// Forwarded newHeads tick — the authoritative block clock for Python.
///
/// Distinct from [`ResultBatch`] (which carries solve results + the solve
/// block as metadata): the consumer derives its block clock from
/// `BlockNotification`s pushed by the pump on every `WsEvent::BlockHeader`,
/// NOT from `ResultBatch::solve_block`. `solve_block` lags by the send
/// debounce + only advances when a batch is actually sent, so using it as the
/// clock makes the bot's `[block: N]` freeze behind the pump's `current_block`
/// (epic 6W35AI; `docs/architecture/rust-owned-bot.md` §6.1 specifies this
/// `block_tx.send(BlockNotification)` channel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockNotification {
    /// The block number (the clock field).
    pub number: u64,
    /// Block timestamp.
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks).
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block.
    pub gas_used: u64,
    /// Gas limit of this block.
    pub gas_limit: u64,
}

impl BlockNotification {
    /// Build a notification from a block number + its `BlockMetadata`.
    #[must_use]
    pub fn from_metadata(number: u64, metadata: &BlockMetadata) -> Self {
        Self {
            number,
            timestamp: metadata.timestamp,
            base_fee_per_gas: metadata.base_fee_per_gas,
            gas_used: metadata.gas_used,
            gas_limit: metadata.gas_limit,
        }
    }
}

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
}

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
    /// V2 + V3 + V4 pool state owner (ADR-003). The shared
    /// `Arc<RwLock<BotState>>` (ADR-006 D1+D2): read methods take a read guard,
    /// mutations a write guard. Lock ordering when nested is
    /// engine-then-core; no code path ever nests in the opposite direction.
    pub core: Arc<parking_lot::RwLock<BotState>>,
    /// Registered path pool refs (immutable after registration).
    pub path_pools: HashMap<u64, MixedPath>,
    /// Resolved path states (mutated on each solve).
    path_resolved: HashMap<u64, ResolvedMixedPath>,
    /// Reverse index: (`hop_type`, `pool_key`) maps to list of `path_ids` that use this pool.
    /// Vec instead of `HashSet` — sets are typically 1-4 entries, dedup at collection time.
    pool_to_paths: HashMap<(HopType, u64), Vec<u64>>,
    /// Last solved results, keyed by path ID for O(1) updates.
    results: HashMap<u64, SolvePathResult>,
    /// Block number for the last solved results
    results_block: u64,
    /// Last block number processed by `process_block`.
    /// `None` means no block has been processed yet.
    /// Used by the pump to determine the backfill boundary on startup.
    last_processed_block: Option<u64>,
    /// The last block this engine's `finalize_block` guard advanced past
    /// (i.e. the last block whose dirty-path solve + diff send completed).
    /// Owned by the engine since ergo task LEZJAS (the pump's `&mut` out-
    /// params retired). Initialize to `0` so the first header/tombstone
    /// `finalize_block(block > 0)` fires; survives a mid-flight engine
    /// joining the pump (ADR-006 D4).
    last_solved_block: u64,
    /// Whether any forward log applied since the last `finalize_block`.
    /// `true` after `record_logs_this_block()` (the pump's forward-log path),
    /// cleared by `finalize_block`. Owned by the engine since LEZJAS.
    has_logs_this_block: bool,
    /// Paths registered via `register_and_solve_path` that have been eagerly
    /// solved and appended to `results`. Tracked so `rebuild_and_solve_affected`
    /// can merge them instead of discarding them when it replaces `self.results`.
    pending_new_paths: HashSet<u64>,
    /// Auto-incrementing path ID
    next_path_id: u64,
    /// The above-threshold results that have been **actually delivered to
    /// Python** via the result channel. Used to compute incremental diffs.
    ///
    /// # Invariant
    ///
    /// `delivered` is advanced **only** by `compute_diff_and_send`, and only
    /// after building a `ResultBatch` for the current above-threshold subset
    /// of `results`. It must stay **empty before the first pump-driven send**
    /// (e.g. during cold-start / `solve_all_paths`), since Python has not yet
    /// received anything. Advancing it without a live channel would poison
    /// `fresh`/`expired` computation for the next real send — see
    /// `solve_all_paths` (solve-only)
    /// [`deregister_path`] removes entries as paths are de-registered.
    delivered: HashMap<u64, SolvePathResult>,
    /// Path IDs that have been de-registered since the last batch.
    /// Drained into the next batch's `removed` field.
    deregistered: Vec<u64>,
    /// Accumulated dirty V2 pool keys from `apply_log` calls since the last
    /// `finalize_block`. Used by the pump for eager log processing.
    dirty_v2: HashSet<u64>,
    /// Accumulated dirty V3 pool keys from `apply_log` calls since the last
    /// `finalize_block`. Used by the pump for eager log processing.
    dirty_v3: HashSet<u64>,
    /// Accumulated dirty V4 pool keys from `apply_log` calls since the last
    /// `finalize_block`. Used by the pump for eager log processing.
    dirty_v4: HashSet<u64>,
    /// Minimum profit (in wei) for a result to appear in the batch channel.
    /// Paths below this threshold are excluded from `delivered` and batches.
    min_profit: U256,
    /// Maximum profit (in wei) for a result to appear in the batch channel.
    /// Paths above this are likely solver defects or scam tokens.
    max_profit: U256,
    /// Sender for the result batch channel. Created in `PyArbitrageEngine::new()`.
    result_tx: Option<tokio::sync::mpsc::UnboundedSender<ResultBatch>>,
    /// Sender for the block-notification channel (epic 6W35AI). The pump
    /// forwards every accepted `WsEvent::BlockHeader` here via
    /// `DrainSink::notify_block`, so Python can drive its block clock from
    /// `newHeads` (not from `ResultBatch::solve_block`). Plumbed parallel to
    /// `result_tx`; `compute_diff_and_send` never touches it.
    block_tx: Option<tokio::sync::mpsc::UnboundedSender<BlockNotification>>,
}

impl ArbitrageEngine {
    /// Create a new engine with its **own** standalone `BotState` (standard
    /// allocation). ADR-006 D1: prefer [`ArbitrageEngine::with_core`] on the live
    /// path so the engine shares one `Arc<RwLock<BotState>>` with `PyBot`/handles;
    /// this no-arg ctor is the standalone-Rust / no-`pyo3`-test convenience.
    #[must_use]
    pub fn new() -> Self {
        Self::with_core(Arc::new(parking_lot::RwLock::new(BotState::new())))
    }

    /// Adopt an existing shared `Arc<RwLock<BotState>>` (ADR-006 D1+D2). The
    /// engine reads/writes pool state through the *same* core that
    /// `PyBot`/`PyLiquidityPool`/`PyErc20Token` share — dissolving the
    /// dual-`BotState` split the §17 stale-state caveat documented. Lock order
    /// remains engine-then-core; the engine's `Mutex<ArbitrageEngine>` engine
    /// state is still engine-local (ADR-006 D2 — engine keeps its own lock
    /// for path/solver state; only the core lock type/flavor changes).
    #[must_use]
    pub fn with_core(core: Arc<parking_lot::RwLock<BotState>>) -> Self {
        Self {
            core,
            path_pools: HashMap::new(),
            path_resolved: HashMap::new(),
            pool_to_paths: HashMap::new(),
            results: HashMap::new(),
            results_block: 0,
            last_processed_block: None,
            last_solved_block: 0,
            has_logs_this_block: false,
            pending_new_paths: HashSet::new(),
            next_path_id: 1, // path IDs start at 1
            delivered: HashMap::new(),
            deregistered: Vec::new(),
            dirty_v2: HashSet::new(),
            dirty_v3: HashSet::new(),
            dirty_v4: HashSet::new(),
            min_profit: U256::ZERO,
            max_profit: U256::MAX,
            result_tx: None,
            block_tx: None,
        }
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
            .write()
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
            .write()
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
        self.core.write().register_v4_pool(params)
    }
}

#[cfg(test)]
impl ArbitrageEngine {
    /// Test-only: are the V2 dirty sets empty? (ADR-006 slice 4 adapter tests.)
    #[must_use]
    pub fn dirty_v2_is_empty(&self) -> bool {
        self.dirty_v2.is_empty()
    }
    /// Test-only: are the V3 dirty sets empty?
    #[must_use]
    pub fn dirty_v3_is_empty(&self) -> bool {
        self.dirty_v3.is_empty()
    }
    /// Test-only: are the V4 dirty sets empty?
    #[must_use]
    pub fn dirty_v4_is_empty(&self) -> bool {
        self.dirty_v4.is_empty()
    }
}
