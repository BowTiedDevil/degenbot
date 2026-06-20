//! Uniswap Engine — mixed V2/V3/V4 arbitrage engine.
//!
//! A unified engine that handles Uniswap V2, V3, and V4 pools in the same
//! per-block lifecycle. Supports mixed paths (e.g., V2→V3, V3→V4, V4→V2 hops).
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
//! On [`UniswapEngine::process_block`]:
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
//! | [`py_binding`] | PyO3 wrapper (`PyUniswapArbEngine`) |
//! | [`tests`] | Unit tests |

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, U256};

use crate::bot_core::BotState;

// Sub-modules — each contains `impl UniswapEngine` or `impl PyUniswapArbEngine` blocks.
#[allow(clippy::module_inception)]
mod diagnostic;
mod engine_handle;
mod engine_subscriber;
mod event_routing;
mod lifecycle;
mod py_binding;
mod result_channel;
mod snapshot_verify;
mod solver_dispatch;
#[cfg(test)]
mod tests;

pub use diagnostic::{DiagnosticHop, DiagnosticPathState, DiagnosticPoolState};

// Re-export public types from this module.
pub use py_binding::{
    DynamicFeePoolRejectedError, HookedPoolRejectedError, PyUniswapArbEngine,
    VerificationMismatchError, VerificationRpcError,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum value that fits in a signed 128-bit integer.
///
/// V4's `BalanceDelta` packs two `int128` values. The `toInt128()` cast in
/// V4's `toBalanceDelta()` reverts with `SafeCastOverflow` if either
/// component exceeds this value. The solver must reject paths where any
/// V4 hop would produce amounts exceeding this limit.
pub(super) const INT128_MAX: U256 =
    U256::from_limbs([0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF, 0, 0]);

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
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum EnginePhase {
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
    /// Check that the current phase allows the given required phase.
    /// Returns `Err` with a descriptive message if the transition is invalid.
    pub(crate) fn require(self, required: Self, method_name: &str) -> Result<(), String> {
        if self >= required {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is in phase {self:?}, but requires {required:?}"
            ))
        }
    }

    /// Require that the engine has not yet reached the given phase.
    pub(crate) fn require_before(self, phase: Self, method_name: &str) -> Result<(), String> {
        if self < phase {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is already in phase {self:?} (requires before {phase:?})"
            ))
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

/// V3 snapshot data: pool address → tick data (consumed at registration).
pub(crate) type V3SnapshotData = HashMap<Address, HashMap<i32, crate::bot_core::TickInfo>>;

/// V4 snapshot data: (`pool_manager`, `pool_id`) → tick data (consumed at registration).
pub(crate) type V4SnapshotData =
    HashMap<(Address, [u8; 32]), HashMap<i32, crate::bot_core::TickInfo>>;

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// Which engine owns a given hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HopType {
    /// V2 constant-product hop
    V2,
    /// V3 concentrated-liquidity hop
    V3,
    /// V4 concentrated-liquidity hop (same math as V3, different settlement)
    V4,
}

impl HopType {
    /// Whether this hop type uses concentrated-liquidity math.
    ///
    /// V3 and V4 hops are both CL — they share the same solver dispatch.
    #[must_use]
    pub const fn is_concentrated_liquidity(&self) -> bool {
        matches!(self, Self::V3 | Self::V4)
    }
}

/// A pool reference in a mixed path.
///
/// Internal representation: `hop_type` is derived from the associated
/// `BotState`'s `PoolEntry` variant at `register_path` time (ADR-006 D3 — the
/// engine never constructs pools, so it learns each hop's family from the
/// `BotState` that owns it). The public intake is [`PoolHop`].
#[derive(Clone, Debug)]
pub struct MixedPoolRef {
    /// Which engine owns this hop
    pub hop_type: HopType,
    /// For V2: `pool_id` in V2 engine. For V3: `pool_idx` in V3 engine.
    pub pool_key: u64,
    /// Direction (V2: implied by `pool_id` orientation; V3: explicit)
    pub zero_for_one: bool,
}

/// A single hop in a path submitted to [`UniswapEngine::register_path`].
///
/// The caller supplies the `BotState`-owned `pool_id` (obtained from
/// `PyBot::register_v*_pool` / `BotState::register_v*_pool`) and the swap
/// direction; the engine derives the pool family (`hop_type`) from the
/// `BotState`'s `PoolEntry` and rejects any `pool_id` not registered there
/// (ADR-006 D3). One `pool_id` per pool; orientation is selected via
/// `zero_for_one` (no forward/reverse id duplication).
#[derive(Clone, Copy, Debug)]
pub struct PoolHop {
    /// The `BotState`-owned pool id this hop references.
    pub pool_id: u64,
    /// Swap direction: token0→token1 when `true`.
    pub zero_for_one: bool,
}

/// A registered mixed arbitrage path.
#[derive(Clone, Debug)]
pub(super) struct MixedPath {
    pools: Vec<MixedPoolRef>,
}

/// Resolved state for a single hop in a mixed path.
///
/// Each variant bundles only the data its hop type needs — no parallel
/// `Vec<Option<T>>` with `None` placeholders. The hop type is the enum
/// variant itself, not a separate index.
///
/// Variant sizes differ because V2 carries full reserve/fee state while
/// CL hops only carry a pre-built integer sequence pointer. This is
/// intentional and not a hot allocation path.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum ResolvedHop {
    /// V2 constant-product hop
    V2 {
        state: crate::optimizers::mobius_int::IntHopState,
    },
    /// V3 concentrated-liquidity hop
    V3 {
        int_seq: crate::optimizers::mobius_v3_int::IntV3TickRangeSequence,
    },
    /// V4 concentrated-liquidity hop (same CL math as V3, different settlement)
    V4 {
        int_seq: crate::optimizers::mobius_v3_int::IntV3TickRangeSequence,
    },
}

impl ResolvedHop {
    /// The hop type for this resolved hop.
    #[allow(dead_code)] // Used in unit tests and available for future dispatch.
    pub(super) const fn hop_type(&self) -> HopType {
        match self {
            Self::V2 { .. } => HopType::V2,
            Self::V3 { .. } => HopType::V3,
            Self::V4 { .. } => HopType::V4,
        }
    }

    /// The V2 `IntHopState`, if this is a V2 hop.
    pub(super) const fn as_v2_state(&self) -> Option<&crate::optimizers::mobius_int::IntHopState> {
        match self {
            Self::V2 { state, .. } => Some(state),
            _ => None,
        }
    }

    /// The integer tick-range sequence, if this is a CL hop (V3 or V4).
    pub(super) const fn as_int_sequence(
        &self,
    ) -> Option<&crate::optimizers::mobius_v3_int::IntV3TickRangeSequence> {
        match self {
            Self::V3 { int_seq, .. } | Self::V4 { int_seq, .. } => Some(int_seq),
            Self::V2 { .. } => None,
        }
    }
}

/// Resolved state for a mixed path, ready for solving.
///
/// V3 and V4 hops both use the same `IntV3TickRangeSequence` type (CL math
/// is identical). The [`ResolvedHop`] enum variant distinguishes which engine
/// owns each hop at the path level.
#[derive(Clone, Debug, Default)]
pub(super) struct ResolvedMixedPath {
    hops: Vec<ResolvedHop>,
    /// Whether this path is valid for solving
    valid: bool,
}

// ---------------------------------------------------------------------------
// UniswapEngine
// ---------------------------------------------------------------------------

/// Result from solving a single arbitrage path.
///
/// Includes optimality data, per-hop output amounts for the encoder, and
/// per-hop consumed input amounts for correct profit calculation and V4
/// int128 overflow detection.
#[derive(Clone, Debug, PartialEq)]
pub struct SolvePathResult {
    /// Optimal input amount (uint256).
    pub optimal_input: U256,
    /// Profit = `final_output` - `consumed_inputs[0]` (uint256).
    /// Uses consumed input (not full specified input) for correct profit
    /// when the first hop partial-fills at a range boundary.
    pub profit: U256,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`.
    /// For a 2-hop path: `[forward_out, final_output]`.
    pub hop_outputs: Vec<U256>,
    /// Per-hop consumed input amounts. `consumed_inputs[i]` = gross input
    /// actually consumed by hop `i` (including fees). For V2 hops, this
    /// equals the input to that hop. For V3/V4 hops, if the range boundary
    /// is hit, this may be less than the input — the unused remainder is
    /// retained by the caller (matching on-chain partial-fill behavior).
    pub consumed_inputs: Vec<U256>,
}

// `BlockMetadata` lives in `bot_core` (general block data); re-exported here so
// engine code + external references (`crate::optimizers::uniswap_engine::BlockMetadata`)
// keep working (ADR-006 D4).
pub use crate::bot_core::BlockMetadata;

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
/// [`UniswapEngine::with_core`]; `new()` standalone sugar allocates its own)
/// and reads/writes pool state through it. Lock ordering when nested is
/// **engine-then-core** — no code path ever nests core-then-engine.
pub struct UniswapEngine {
    /// V2 + V3 + V4 pool state owner (ADR-003). The shared
    /// `Arc<RwLock<BotState>>` (ADR-006 D1+D2): read methods take a read guard,
    /// mutations a write guard. Lock ordering when nested is
    /// engine-then-core; no code path ever nests in the opposite direction.
    core: Arc<parking_lot::RwLock<BotState>>,
    /// Registered path pool refs (immutable after registration).
    path_pools: HashMap<u64, MixedPath>,
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
    /// `solve_all_paths` (solve-only) and the `ResultBatch` CONTEXT.md note.
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
    /// Sender for the result batch channel. Created in `PyUniswapArbEngine::new()`.
    result_tx: Option<tokio::sync::mpsc::UnboundedSender<ResultBatch>>,
}

impl UniswapEngine {
    /// Create a new engine with its **own** standalone `BotState` (standard
    /// allocation). ADR-006 D1: prefer [`UniswapEngine::with_core`] on the live
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
    /// remains engine-then-core; the engine's `Mutex<UniswapEngine>` engine
    /// state is still engine-local (ADR-006 D2 — engine keeps its own lock
    /// for path/solver state; only the core lock type/flavor changes).
    #[must_use]
    pub(crate) fn with_core(core: Arc<parking_lot::RwLock<BotState>>) -> Self {
        Self {
            core,
            path_pools: HashMap::new(),
            path_resolved: HashMap::new(),
            pool_to_paths: HashMap::new(),
            results: HashMap::new(),
            results_block: 0,
            last_processed_block: None,
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
        }
    }

    /// Create a new engine with a custom event buffer max age for the V4
    /// sub-engine (V3 buffer lives on `BotState` — ADR-003).
    #[must_use]
    pub fn new_with_buffer_max_age(event_buffer_max_age: Option<u64>) -> Self {
        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        if let Some(age) = event_buffer_max_age {
            core.write().set_v3_buffer_max_age(Some(age));
            core.write().set_v4_buffer_max_age(Some(age));
        }
        Self {
            core,
            ..Self::new()
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
impl UniswapEngine {
    /// Register a V2 pool into the engine's `BotState` and return its `pool_id`.
    #[must_use]
    pub fn register_v2_pool(
        &self,
        address: Address,
        reserve0: U256,
        reserve1: U256,
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
        };
        self.core.write().register_v2_pool(&params)
    }

    /// Register a V3 pool into the engine's `BotState` and return its `pool_id`.
    #[must_use]
    pub fn register_v3_pool(&self, params: &crate::bot_core::RegisterV3PoolParams) -> u64 {
        self.core.write().register_v3_pool(params)
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
impl UniswapEngine {
    /// Test-only: are the V2 dirty sets empty? (ADR-006 slice 4 adapter tests.)
    pub(crate) fn dirty_v2_is_empty(&self) -> bool {
        self.dirty_v2.is_empty()
    }
    /// Test-only: are the V3 dirty sets empty?
    pub(crate) fn dirty_v3_is_empty(&self) -> bool {
        self.dirty_v3.is_empty()
    }
    /// Test-only: are the V4 dirty sets empty?
    pub(crate) fn dirty_v4_is_empty(&self) -> bool {
        self.dirty_v4.is_empty()
    }
}
