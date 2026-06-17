//! Uniswap Engine — mixed V2/V3/V4 arbitrage engine.
//!
//! A unified engine that handles Uniswap V2, V3, and V4 pools in the same
//! per-block lifecycle. Supports mixed paths (e.g., V2→V3, V3→V4, V4→V2 hops).
//!
//! # Design
//!
//! The engine composes:
//! - A [`BotCore`] for V2 pool state and constant-product solving (ADR-003:
//!   `BotCore` is the single state owner; the engine is a consumer)
//! - A [`V3BlockEngine`] for V3 pool state, tick ranges, and piecewise V3 solving
//! - A [`V4BlockEngine`] for V4 pool state (same CL math as V3, different settlement)
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

use crate::bot_core::BotCore;
use crate::optimizers::v3_block_engine::V3BlockEngine;
use crate::optimizers::v4_block_engine::V4BlockEngine;

// Sub-modules — each contains `impl UniswapEngine` or `impl PyUniswapArbEngine` blocks.
#[allow(clippy::module_inception)]
mod diagnostic;
mod event_routing;
mod lifecycle;
mod py_binding;
mod result_channel;
mod solver_dispatch;
#[cfg(test)]
mod tests;

pub use diagnostic::{DiagnosticHop, DiagnosticPathState, DiagnosticPoolState};

// Re-export public types from this module.
pub use py_binding::PyUniswapArbEngine;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum value that fits in a signed 128-bit integer.
///
/// V4's `BalanceDelta` packs two `int128` values. The `toInt128()` cast in
/// V4's `toBalanceDelta()` reverts with `SafeCastOverflow` if either
/// component exceeds this value. The solver must reject paths where any
/// V4 hop would produce amounts exceeding this limit.
pub(super) const INT128_MAX: U256 = U256::from_limbs([0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF, 0, 0]);

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
/// `Tracked` means the snapshot provided complete tick data (may be empty =
/// genuinely illiquid). `Sparse` means no snapshot data exists for this pool
/// — solver results may contain errors or phantom profits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolTickCoverage {
    /// Snapshot provided complete tick data. Solver results are trustworthy.
    Tracked,
    /// No snapshot data exists. Solver results may be inaccurate.
    Sparse,
}

/// V3 snapshot data: pool address → tick data (consumed at registration).
pub(crate) type V3SnapshotData = HashMap<Address, HashMap<i32, crate::bot_core::TickInfo>>;

/// V4 snapshot data: (`pool_manager`, `pool_id`) → tick data (consumed at registration).
pub(crate) type V4SnapshotData = HashMap<(Address, [u8; 32]), HashMap<i32, crate::bot_core::TickInfo>>;

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
#[derive(Clone, Debug)]
pub struct MixedPoolRef {
    /// Which engine owns this hop
    pub hop_type: HopType,
    /// For V2: `pool_id` in V2 engine. For V3: `pool_idx` in V3 engine.
    pub pool_key: u64,
    /// Direction (V2: implied by `pool_id` orientation; V3: explicit)
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
    pub(super) const fn as_int_sequence(&self) -> Option<&crate::optimizers::mobius_v3_int::IntV3TickRangeSequence> {
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

/// Block metadata included in each `ResultBatch`.
///
/// Passed from the pump's WS block header into `process_block()`,
/// then forwarded to Python via the result batch channel.
#[derive(Clone, Debug, Default)]
pub struct BlockMetadata {
    /// Block timestamp
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks)
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block
    pub gas_used: u64,
    /// Gas limit of this block
    pub gas_limit: u64,
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
/// V2 pool state lives in [`BotCore`] (ADR-003: the single Rust state owner,
/// peer to this engine). The engine holds an `Arc<Mutex<BotCore>>` and reads /
/// mutates V2 state through it. Lock ordering when nested is
/// **engine-then-core** — no code path ever nests core-then-engine.
pub struct UniswapEngine {
    /// V2 pool state owner (ADR-003). V3/V4 state still lives on the
    /// per-family block engines until Slices 2/3 migrate them.
    core: Arc<parking_lot::Mutex<BotCore>>,
    /// The V3 engine
    v3_engine: V3BlockEngine,
    /// The V4 engine
    v4_engine: V4BlockEngine,
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
    /// Create a new engine.
    #[must_use] 
    pub fn new() -> Self {
        Self {
            core: Arc::new(parking_lot::Mutex::new(BotCore::new())),
            v3_engine: V3BlockEngine::new(),
            v4_engine: V4BlockEngine::new(),
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

    /// Create a new engine with a custom event buffer max age for V3/V4 sub-engines.
    #[must_use] 
    pub fn new_with_buffer_max_age(event_buffer_max_age: Option<u64>) -> Self {
        Self {
            v3_engine: V3BlockEngine::new_with_buffer_max_age(event_buffer_max_age),
            v4_engine: V4BlockEngine::new_with_buffer_max_age(event_buffer_max_age),
            ..Self::new()
        }
    }

    /// Register a V2 pool by contract address and initial reserves.
    ///
    /// Delegates to [`BotCore::register_v2_pool`] (ADR-003: V2 state lives in
    /// the core). The single fee `(gamma_numer, fee_denom)` is applied
    /// symmetrically to both swap directions — V2-fork asymmetric fees are a
    /// future concern. Token0/token1/factory default to zero (the V2 *solve*
    /// path computes on reserves + fee only; identity is an encoding-layer
    /// concern).
    ///
    /// Returns the assigned `pool_id`. Paths reference this single id and
    /// select orientation via `zero_for_one` (ADR-003 "Swap Orientation":
    /// single `PoolEntry` per address, orientation derived at solve).
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
        };
        self.core.lock().register_v2_pool(&params)
    }
}
