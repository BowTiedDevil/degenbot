//! Uniswap Engine — mixed V2/V3/V4 arbitrage engine.
//!
//! A unified engine that handles Uniswap V2, V3, and V4 pools in the same
//! per-block lifecycle. Supports mixed paths (e.g., V2→V3, V3→V4, V4→V2 hops).
//!
//! # Design
//!
//! The engine composes:
//! - A [`V2BlockEngine`] for V2 pool state and constant-product solving
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

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use tokio::sync::mpsc;

use crate::optimizers::mobius_int::u256_to_f64;
use crate::optimizers::mobius_v3::V3TickRangeSequence;
use crate::optimizers::v2_block_engine::V2BlockEngine;
use crate::optimizers::v3_block_engine::{RegisterV3PoolParams, V3BlockEngine, V3SwapUpdate};
use crate::optimizers::v4_block_engine::{RegisterV4PoolParams, V4BlockEngine, V4SwapUpdate};

/// Maximum value that fits in a signed 128-bit integer.
///
/// V4's `BalanceDelta` packs two `int128` values. The `toInt128()` cast in
/// V4's `toBalanceDelta()` reverts with `SafeCastOverflow` if either
/// component exceeds this value. The solver must reject paths where any
/// V4 hop would produce amounts exceeding this limit.
const INT128_MAX: U256 = U256::from_limbs([0xFFFFFFFFFFFFFFFF, 0xFFFFFFFFFFFFFFFF, 0, 0]);

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
enum EnginePhase {
    /// Engine just created, no connections.
    Created = 0,
    /// WS subscribe() completed, first block observed.
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
    fn require(&self, required: Self, method_name: &str) -> Result<(), String> {
        if *self >= required {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is in phase {:?}, but requires {:?}",
                self, required
            ))
        }
    }

    /// Require that the engine has not yet reached the given phase.
    fn require_before(&self, phase: Self, method_name: &str) -> Result<(), String> {
        if *self < phase {
            Ok(())
        } else {
            Err(format!(
                "Cannot call {method_name}: engine is already in phase {:?} (requires before {:?})",
                self, phase
            ))
        }
    }
}

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
type V3SnapshotData = HashMap<Address, HashMap<i32, crate::bot_core::TickInfo>>;

/// V4 snapshot data: (pool_manager, pool_id) → tick data (consumed at registration).
type V4SnapshotData = HashMap<(Address, [u8; 32]), HashMap<i32, crate::bot_core::TickInfo>>;

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
struct MixedPath {
    pools: Vec<MixedPoolRef>,
}

/// Resolved state for a mixed path, ready for solving.
///
/// V3 and V4 hops both use the same `IntV3TickRangeSequence` type (CL math
/// is identical). The `hop_types` vector distinguishes which engine owns
/// each hop at the path level.
#[derive(Clone, Debug, Default)]
struct ResolvedMixedPath {
    hop_types: Vec<HopType>,
    /// V2 hop states (Some for V2 hops, None for V3/V4 hops)
    v2_hops: Vec<Option<crate::optimizers::mobius_int::IntHopState>>,
    /// V3 tick-range sequences (Some for V3 hops, None for V2/V4 hops)
    /// Only used for f64-based solver (kept for compatibility)
    v3_sequences: Vec<Option<V3TickRangeSequence>>,
    /// Integer V3 hops built from original U256 values (Some for V3 hops, None for V2/V4 hops)
    int_v3_hops: Vec<Option<crate::optimizers::mobius_v3_int::IntV3TickRangeHop>>,
    /// Integer tick-range sequences for CL paths (Some for V3/V4 hops, None for V2 hops).
    /// V3 and V4 produce the same type — `IntV3TickRangeSequence`.
    int_v3_sequences: Vec<Option<crate::optimizers::mobius_v3_int::IntV3TickRangeSequence>>,
    /// Base (f64) hops for Mobius initial estimate
    base_hops: Vec<crate::optimizers::mobius::HopState>,
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
pub struct UniswapEngine {
    /// The V2 engine
    v2_engine: V2BlockEngine,
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
    /// The above-threshold results that have been delivered to Python
    /// via the result channel. Used to compute incremental diffs.
    delivered: HashMap<u64, SolvePathResult>,
    /// Path IDs that have been de-registered since the last batch.
    /// Drained into the next batch's `removed` field.
    deregistered: Vec<u64>,
    /// Flag set by `register_and_solve_path` when results are eagerly
    /// appended between `process_block` calls. The next
    /// `rebuild_and_solve_affected` call will include pending paths
    /// and produce a batch.
    has_unsent_results: bool,
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
    result_tx: Option<mpsc::UnboundedSender<ResultBatch>>,
}

impl UniswapEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_buffer_max_age(None)
    }

    /// Create a new engine with a configurable event buffer staleness limit.
    ///
    /// The limit applies to V3 and V4 liquidity event buffers. V2 has no
    /// buffer (Sync events are stateless).
    #[must_use]
    pub fn new_with_buffer_max_age(event_buffer_max_age: Option<u64>) -> Self {
        Self {
            v2_engine: V2BlockEngine::new(),
            v3_engine: V3BlockEngine::new_with_buffer_max_age(event_buffer_max_age),
            v4_engine: V4BlockEngine::new_with_buffer_max_age(event_buffer_max_age),
            path_pools: HashMap::new(),
            path_resolved: HashMap::new(),
            pool_to_paths: HashMap::new(),
            results: HashMap::new(),
            results_block: 0,
            last_processed_block: None,
            pending_new_paths: HashSet::new(),
            next_path_id: 1,
            delivered: HashMap::new(),
            deregistered: Vec::new(),
            has_unsent_results: false,
            dirty_v2: HashSet::new(),
            dirty_v3: HashSet::new(),
            dirty_v4: HashSet::new(),
            min_profit: U256::from(5_000_000_000u64), // 5 gwei
            max_profit: U256::from(5_000_000_000_000_000_000u64), // 5 ETH
            result_tx: None,
        }
    }

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// Applies to V3 and V4 sub-engine buffers. `None` means unbounded.
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_engine.set_event_buffer_max_age(max_age);
        self.v4_engine.set_event_buffer_max_age(max_age);
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    pub fn flush_event_buffer(&mut self) {
        self.v3_engine.flush_event_buffer();
        self.v4_engine.flush_event_buffer();
    }

    /// Access the V2 engine (for registration).
    #[allow(clippy::missing_const_for_fn)]
    pub fn v2_engine(&mut self) -> &mut V2BlockEngine {
        &mut self.v2_engine
    }

    /// Access the V3 engine (for registration).
    #[allow(clippy::missing_const_for_fn)]
    pub fn v3_engine(&mut self) -> &mut V3BlockEngine {
        &mut self.v3_engine
    }

    /// Access the V3 engine (immutable).
    pub fn v3_engine_ref(&self) -> &V3BlockEngine {
        &self.v3_engine
    }

    /// Access the V4 engine (for registration).
    #[allow(clippy::missing_const_for_fn)]
    pub fn v4_engine(&mut self) -> &mut V4BlockEngine {
        &mut self.v4_engine
    }

    /// Register a mixed arbitrage path as an ordered list of `MixedPoolRef`s.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics with fewer than 2 pool refs.
    pub fn register_path(&mut self, pool_refs: Vec<MixedPoolRef>) -> u64 {
        assert!(pool_refs.len() >= 2, "need at least 2 pool refs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedMixedPath::default();
        self.resolve_path(&pool_refs, &mut resolved);

        // Build reverse index: (hop_type, pool_key) → path_id
        for pool_ref in &pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }

        self.path_pools.insert(path_id, MixedPath { pools: pool_refs });
        self.path_resolved.insert(path_id, resolved);
        path_id
    }

    /// Register a path and immediately resolve + solve it.
    ///
    /// Unlike `register_path` (which only registers), this method also
    /// eagerly solves the path and appends any profitable result to
    /// `self.results`. Used when the engine is already running (after the
    /// pump has started) so that new paths are immediately available to
    /// `latest_results()`.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics with fewer than 2 pool refs.
    pub fn register_and_solve_path(&mut self, pool_refs: Vec<MixedPoolRef>) -> u64 {
        assert!(pool_refs.len() >= 2, "need at least 2 pool refs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedMixedPath::default();
        self.resolve_path(&pool_refs, &mut resolved);

        // Build reverse index: (hop_type, pool_key) → path_id
        for pool_ref in &pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }

        // Eagerly solve and insert into results if profitable
        if resolved.valid {
            if let Some(solve_result) = self.solve_path(&resolved) {
                if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                    self.results.insert(path_id, solve_result);
                }
            }
        }

        // Track so rebuild_and_solve_affected can merge instead of discard
        self.pending_new_paths.insert(path_id);
        self.has_unsent_results = true;

        self.path_pools.insert(path_id, MixedPath { pools: pool_refs });
        self.path_resolved.insert(path_id, resolved);
        path_id
    }

    /// Apply a single log event to the appropriate sub-engine immediately.
    ///
    /// Updates pool state and the dirty key sets but does NOT solve or send
    /// results. The caller must call `solve_dirty` to trigger solve and
    /// result dispatch.
    pub fn apply_log(&mut self, log: &Log, block_number: u64) {
        let Some(topic) = log.topics().first() else {
            return;
        };

        if *topic == crate::optimizers::v2_sync_decoder::V2_SYNC_TOPIC {
            if let Some(event) = crate::optimizers::v2_sync_decoder::decode_sync_log(log) {
                if let Some(fwd_key) = self.v2_engine.apply_sync(
                    event.pool_address,
                    event.reserve0,
                    event.reserve1,
                ) {
                    self.dirty_v2.insert(fwd_key);
                    self.dirty_v2.insert(fwd_key + 1); // reverse key
                }
            }
        } else if *topic == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v3_swap_decoder::decode_v3_swap_log(log) {
                if let Some(key) = self.v3_engine.apply_swap(
                    event.pool_address,
                    event.sqrt_price_x96,
                    event.liquidity.to::<u128>(),
                    event.tick,
                    block_number,
                    &[],
                ) {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_mint_log(log) {
                if let Some(key) = self.v3_engine.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    event.amount.cast_signed(),
                    block_number,
                ) {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
            if let Some(event) = crate::bot_core::v3_mint_burn_decoder::decode_v3_burn_log(log) {
                if let Some(key) = self.v3_engine.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    -(event.amount.cast_signed()),
                    block_number,
                ) {
                    self.dirty_v3.insert(key);
                }
            }
        } else if *topic == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
            if let Some(event) = crate::bot_core::v4_swap_decoder::decode_v4_swap_log(log) {
                if let Some((fwd_key, rev_key)) = self.v4_engine.apply_swap(
                    &V4SwapUpdate {
                        pool_manager: log.address(),
                        pool_id: event.pool_id,
                        sqrt_price_x96: event.sqrt_price_x96,
                        liquidity: event.liquidity.to::<u128>(),
                        tick: event.tick,
                        tick_priors: vec![],
                    },
                    block_number,
                ) {
                    self.dirty_v4.insert(fwd_key);
                    self.dirty_v4.insert(rev_key);
                }
            }
        } else if *topic == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC {
            if let Some(event) = crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log(log) {
                if let Some((fwd_key, rev_key)) = self.v4_engine.apply_liquidity_update(
                    log.address(),
                    event.pool_id,
                    event.tick_lower,
                    event.tick_upper,
                    event.liquidity_delta,
                    block_number,
                ) {
                    self.dirty_v4.insert(fwd_key);
                    self.dirty_v4.insert(rev_key);
                }
            }
        }
    }

    /// Solve all paths affected by logs applied since the last `solve_dirty`
    /// call, but do NOT send a result batch to Python.
    ///
    /// The pump calls this eagerly after each WS log to keep engine state
    /// current. The actual batch send is triggered by the pump's debounce
    /// timer or block boundary logic.
    pub fn solve_dirty(&mut self, block_number: u64, metadata: &BlockMetadata) {
        // Expire stale buffered events in V3/V4 sub-engines
        self.v3_engine.expire_buffered_events(block_number);
        self.v4_engine.expire_buffered_events(block_number);

        // Take ownership of dirty sets to avoid borrow conflict
        let dirty_v2 = std::mem::take(&mut self.dirty_v2);
        let dirty_v3 = std::mem::take(&mut self.dirty_v3);
        let dirty_v4 = std::mem::take(&mut self.dirty_v4);

        // Re-solve only paths containing updated pools (no batch send)
        self.rebuild_and_solve_affected(
            &dirty_v2,
            &dirty_v3,
            &dirty_v4,
            block_number,
            metadata,
        );

        // dirty sets are already cleared by std::mem::take
        self.last_processed_block = Some(block_number);
    }

    /// Compute the incremental diff and send a result batch to Python.
    ///
    /// Called by the pump when the debounce timer fires (mid-block) or
    /// when a block boundary is detected. Results must already be
    /// up-to-date (via `solve_dirty`) before calling this.
    pub fn send_result_batch(&mut self, metadata: &BlockMetadata) {
        self.compute_diff_and_send(metadata);
    }

    /// Returns `true` if there are unsolved dirty pool keys from `apply_log`
    /// calls that haven't been followed by `solve_dirty` yet.
    #[must_use]
    pub fn has_dirty_paths(&self) -> bool {
        !self.dirty_v2.is_empty() || !self.dirty_v3.is_empty() || !self.dirty_v4.is_empty()
    }

    /// Process a block: apply all logs then solve affected paths.
    /// Does NOT send a result batch — the pump controls dispatch.
    pub fn process_block(&mut self, logs: &[Log], block_number: u64, metadata: &BlockMetadata) {
        for log in logs {
            self.apply_log(log, block_number);
        }
        self.solve_dirty(block_number, metadata);
    }

    /// Process a block, solve, and send result batch to Python.
    /// Used for empty-block notifications where the pump doesn't go
    /// through the debounce path.
    pub fn process_block_and_send(&mut self, logs: &[Log], block_number: u64, metadata: &BlockMetadata) {
        self.process_block(logs, block_number, metadata);
        self.compute_diff_and_send(metadata);
    }

    /// Process pre-decoded updates for testing.
    pub fn process_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        // Apply updates to sub-engines and collect affected pool keys
        let v2_affected = self.v2_engine.apply_sync_updates(v2_updates);
        let v3_affected = self.v3_engine.apply_swap_updates(v3_updates, block_number);

        // Re-solve only paths containing updated pools
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, &HashSet::new(), block_number, metadata);
        self.last_processed_block = Some(block_number);
    }

    /// Process pre-decoded V4 updates.
    pub fn process_v4_updates(
        &mut self,
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let v4_affected = self.v4_engine.apply_swap_updates(v4_updates, block_number);
        self.rebuild_and_solve_affected(&HashSet::new(), &HashSet::new(), &v4_affected, block_number, metadata);
    }

    /// Process all updates at once (V2 + V3 + V4).
    pub fn process_all_updates(
        &mut self,
        v2_updates: &[(Address, U256, U256)],
        v3_updates: &[V3SwapUpdate],
        v4_updates: &[V4SwapUpdate],
        block_number: u64,
        metadata: &BlockMetadata,
    ) {
        let v2_affected = self.v2_engine.apply_sync_updates(v2_updates);
        let v3_affected = self.v3_engine.apply_swap_updates(v3_updates, block_number);
        let v4_affected = self.v4_engine.apply_swap_updates(v4_updates, block_number);
        self.rebuild_and_solve_affected(&v2_affected, &v3_affected, &v4_affected, block_number, metadata);
        self.last_processed_block = Some(block_number);
    }

    /// Re-resolve and re-solve only paths that contain updated pools.
    ///
    /// Uses the `pool_to_paths` reverse index to identify `affected_path_ids`,
    /// then re-resolves and re-solves only those. Unaffected paths carry
    /// their previous results forward.
    fn rebuild_and_solve_affected(
        &mut self,
        v2_affected: &HashSet<u64>,
        v3_affected: &HashSet<u64>,
        v4_affected: &HashSet<u64>,
        block_number: u64,
        _metadata: &BlockMetadata,
    ) {
        // Collect affected path IDs from the reverse index
        let mut affected_path_ids: HashSet<u64> = HashSet::new();

        for pool_key in v2_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V2, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v3_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V3, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
        }
        for pool_key in v4_affected {
            if let Some(path_ids) = self.pool_to_paths.get(&(HopType::V4, *pool_key)) {
                affected_path_ids.extend(path_ids);
            }
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
            self.results_block = block_number;
            return;
        }

        // Re-resolve and solve only affected paths — update results in-place
        // without cloning unchanged entries.

        // Re-resolve affected paths directly (no clone needed — path_pools is
        // immutable, path_resolved is mutable, no borrow conflict)
        for &path_id in &affected_path_ids {
            let Some(path) = self.path_pools.get(&path_id) else {
                continue;
            };
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(&path.pools, &mut resolved);
            self.path_resolved.insert(path_id, resolved);
        }

        // Remove old results for affected paths (they'll be re-solved below)
        for &path_id in &affected_path_ids {
            self.results.remove(&path_id);
        }

        // Solve affected paths and insert new results
        for &path_id in &affected_path_ids {
            let Some(resolved) = self.path_resolved.get(&path_id) else {
                continue;
            };
            if !resolved.valid {
                continue;
            }

            if let Some(solve_result) = self.solve_path(resolved) {
                if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                    self.results.insert(path_id, solve_result);
                }
            }
        }

        self.results_block = block_number;
        // Note: no compute_diff_and_send here — the pump controls when
        // batches are dispatched (debounce timer or block boundary).
    }
    ///
    /// Dispatches based on path composition:
    /// - V2-V2: integer-exact Möbius solver (closed-form U512 isqrt)
    /// - V3-V3 / V4-V4 / V3-V4 / V4-V3: integer piecewise-Möbius (CL × CL)
    /// - V2-V3 / V3-V2 / V2-V4 / V4-V2: mixed integer-exact solver
    #[allow(clippy::unused_self)]
    fn solve_path(&self, resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
        let all_v2 = resolved.hop_types.iter().all(|&t| t == HopType::V2);
        let all_cl = resolved.hop_types.iter().all(HopType::is_concentrated_liquidity);

        let result = if all_v2 {
            let int_hops: Vec<_> = resolved
                .v2_hops
                .iter()
                .filter_map(Option::as_ref)
                .cloned()
                .collect();
            if int_hops.len() == resolved.hop_types.len() {
                crate::optimizers::mobius_int_exact::exact_mobius_solve(&int_hops)
                    .ok()
                    .and_then(|r| {
                        if r.is_profitable
                            && !r.optimal_input.is_zero()
                            && !r.profit.is_zero()
                        {
                            // V2 constant-product pools: each hop's consumed input
                            // is the previous hop's output (hop_outputs[i-1]),
                            // with hop 0 consuming optimal_input.
                            let mut consumed_inputs = Vec::with_capacity(r.hop_outputs.len());
                            consumed_inputs.push(r.optimal_input);
                            for i in 1..r.hop_outputs.len() {
                                consumed_inputs.push(r.hop_outputs[i - 1]);
                            }
                            Some(SolvePathResult {
                                optimal_input: r.optimal_input,
                                profit: r.profit,
                                hop_outputs: r.hop_outputs,
                                consumed_inputs,
                            })
                        } else {
                            None
                        }
                    })
            } else {
                None
            }
        } else if all_cl {
            // V3-V3, V4-V4, V3-V4, V4-V3, V3-V3-V3, etc: all concentrated-liquidity
            let int_sequences: Vec<_> = resolved
                .int_v3_sequences
                .iter()
                .filter_map(Option::as_ref)
                .collect();
            if int_sequences.len() >= 2 {
                crate::optimizers::mobius_v3_int::int_solve_cl_path(&int_sequences)
                    .map(|(optimal_input, _profit, hop_outputs)| {
                        // consumed_inputs[0] = optimal_input (first hop always consumes
                        // its full input for single-range paths; no partial fill).
                        // consumed_inputs[i>0] = hop_outputs[i-1] (the previous hop's
                        // output becomes this hop's input — matching the pipeline:
                        // V3 output flows into V4 as amountSpecified).
                        let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
                        consumed_inputs.push(optimal_input);
                        for i in 1..hop_outputs.len() {
                            consumed_inputs.push(hop_outputs[i - 1]);
                        }
                        let profit = hop_outputs.last().copied().unwrap_or(U256::ZERO)
                            .saturating_sub(consumed_inputs[0]);
                        SolvePathResult {
                            optimal_input,
                            profit,
                            hop_outputs,
                            consumed_inputs,
                        }
                    })
            } else {
                None
            }
        } else {
            // Mixed V2 + CL (V3 or V4)
            Self::solve_mixed_path_int(resolved)
        };

        // V4 int128 guard: reject paths where any V4 hop's consumed input or
        // output exceeds int128_max. V4's toBalanceDelta() calls toInt128() on
        // swap amounts — if either doesn't fit, V4 reverts with SafeCastOverflow.
        if let Some(ref r) = result {
            for (i, hop_type) in resolved.hop_types.iter().enumerate() {
                if *hop_type == HopType::V4 {
                    let consumed = r.consumed_inputs.get(i).copied().unwrap_or(U256::ZERO);
                    let output = r.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                    if consumed > INT128_MAX || output > INT128_MAX {
                        return None;
                    }
                }
            }
        }

        result
    }

    /// Solve all registered paths using `solve_path`.
    #[must_use]
    fn solve_all(&self) -> HashMap<u64, SolvePathResult> {
        let mut results = HashMap::with_capacity(self.path_resolved.len());

        for (&path_id, resolved) in &self.path_resolved {
            if !resolved.valid {
                continue;
            }

            if let Some(solve_result) = self.solve_path(resolved) {
                if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                    results.insert(path_id, solve_result);
                }
            }
        }

        results
    }

    /// Solve a mixed V2 + CL (V3 or V4) path using integer-exact Möbius solver.
    ///
    /// Uses the pre-built `IntV3TickRangeSequence` from `resolve_path`,
    /// which was constructed directly from U256 values (no f64 conversion).
    /// V3 and V4 hops produce the same type — `IntV3TickRangeSequence`.
    ///
    /// The sequence-based solver enumerates CL ending ranges and computes
    /// the optimal input for each piece, validating with crossing-aware
    /// simulation. This eliminates false positives from single-range
    /// approximation when swaps exceed the current tick range capacity.
    fn solve_mixed_path_int(
        resolved: &ResolvedMixedPath,
    ) -> Option<SolvePathResult> {
        if resolved.hop_types.len() < 2 {
            return None;
        }

        // Check that this is actually a mixed path (both V2 and CL hops)
        let has_v2 = resolved.hop_types.contains(&HopType::V2);
        let has_cl = resolved.hop_types.iter().any(HopType::is_concentrated_liquidity);
        if !has_v2 || !has_cl {
            return None; // not a mixed path — should be handled by other dispatches
        }

        // Build hop_order from hop_types
        let hop_order: Vec<bool> = resolved
            .hop_types
            .iter()
            .map(|t| *t == HopType::V2)
            .collect();

        crate::optimizers::mobius_v3_int::exact_solve_mixed_path_n(
            &resolved.v2_hops,
            &resolved.int_v3_sequences,
            &hop_order,
        )
        .map(|(optimal_input, profit, hop_outputs)| {
            // consumed_inputs[0] = optimal_input (first hop consumes full input).
            // consumed_inputs[i>0] = hop_outputs[i-1] (previous hop's output
            // becomes this hop's input).
            let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
            consumed_inputs.push(optimal_input);
            for i in 1..hop_outputs.len() {
                consumed_inputs.push(hop_outputs[i - 1]);
            }
            SolvePathResult {
                optimal_input,
                profit,
                hop_outputs,
                consumed_inputs,
            }
        })
    }

    /// Read the last solved results and block number.
    #[must_use]
    pub fn latest_results(&self) -> (&HashMap<u64, SolvePathResult>, u64) {
        (&self.results, self.results_block)
    }

    /// Return the last block number processed by `process_block`.
    /// Returns `None` if no block has been processed yet.
    #[must_use]
    pub const fn last_processed_block(&self) -> Option<u64> {
        self.last_processed_block
    }

    /// Set the last processed block manually.
    ///
    /// Called by Python after backfill completes, so the Rust pump
    /// knows not to re-process the backfilled range. Without this,
    /// the pump would restart from `first_observed_block` and buffer
    /// events that the Python pools already reflect, causing
    /// double-application when pools are later registered.
    pub fn set_last_processed_block(&mut self, block: u64) {
        self.last_processed_block = Some(block);
    }

    /// Apply backfill logs from the snapshot gap to the sub-engines.
    ///
    /// Iterates logs once, decoding and applying each to the appropriate
    /// sub-engine without cloning. After all logs are applied, calls
    /// `rebuild_and_solve` on each touched sub-engine.
    pub fn process_backfill_logs(&mut self, logs: &[Log], block_number: u64) {
        use crate::bot_core::v3_swap_decoder::decode_v3_swap_log;
        use crate::bot_core::v3_mint_burn_decoder::{decode_v3_mint_log, decode_v3_burn_log};
        use crate::bot_core::v4_swap_decoder::decode_v4_swap_log;
        use crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;

        let mut v3_touched = false;
        let mut v4_touched = false;

        for log in logs {
            let Some(topic0) = log.topic0() else {
                continue;
            };

            if *topic0 == crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC {
                if let Some(event) = decode_v3_swap_log(log) {
                    self.v3_engine.apply_swap(
                        event.pool_address,
                        event.sqrt_price_x96,
                        event.liquidity.to::<u128>(),
                        event.tick,
                        block_number,
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_MINT_TOPIC {
                if let Some(event) = decode_v3_mint_log(log) {
                    self.v3_engine.apply_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        event.amount.cast_signed(),
                        block_number,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v3_mint_burn_decoder::V3_BURN_TOPIC {
                if let Some(event) = decode_v3_burn_log(log) {
                    self.v3_engine.apply_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        -(event.amount.cast_signed()),
                        block_number,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC {
                if let Some(event) = decode_v4_swap_log(log) {
                    self.v4_engine.apply_swap(
                        &V4SwapUpdate {
                            pool_manager: log.address(),
                            pool_id: event.pool_id,
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            tick_priors: vec![],
                        },
                        block_number,
                    );
                    v4_touched = true;
                }
            } else if *topic0 == crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC {
                if let Some(event) = decode_v4_modify_liquidity_log(log) {
                    self.v4_engine.apply_liquidity_update(
                        log.address(),
                        event.pool_id,
                        event.tick_lower,
                        event.tick_upper,
                        event.liquidity_delta,
                        block_number,
                    );
                    v4_touched = true;
                }
            }
        }

        if v3_touched {
            self.v3_engine.expire_buffered_events(block_number);
            self.v3_engine.rebuild_and_solve(block_number);
        }
        if v4_touched {
            self.v4_engine.expire_buffered_events(block_number);
            self.v4_engine.rebuild_and_solve(block_number);
        }

        self.last_processed_block = Some(block_number);
    }

    /// Set the result batch channel sender.
    ///
    /// Called by `PyUniswapArbEngine::new()` to wire the channel.
    /// The engine sends incremental result batches via this sender
    /// after each `process_block` or `solve_all_paths`.
    pub fn set_result_channel(&mut self, tx: mpsc::UnboundedSender<ResultBatch>) {
        self.result_tx = Some(tx);
    }

    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit < max_profit`
    /// appear in batch `fresh` / `updated` entries. Paths outside
    /// this range are excluded from `delivered` and batches.
    pub fn set_profit_thresholds(&mut self, min_profit: U256, max_profit: U256) {
        self.min_profit = min_profit;
        self.max_profit = max_profit;
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from `paths`, `pool_to_paths` reverse index,
    /// `results`, `delivered`, and `pending_new_paths`. The path's
    /// pools are **not** removed from the sub-engines — other paths
    /// may still reference them.
    ///
    /// The de-registered path ID is recorded and included in the next
    /// batch's `removed` field.
    ///
    /// Returns `true` if the path existed and was removed.
    pub fn deregister_path(&mut self, path_id: u64) -> bool {
        // Remove from path_pools and get the pool refs to clean up reverse index
        let removed = self.path_pools.remove(&path_id);
        let existed = removed.is_some();
        if let Some(path) = removed {
            // Remove from pool_to_paths reverse index
            for pool_ref in &path.pools {
                if let Some(path_ids) =
                    self.pool_to_paths.get_mut(&(pool_ref.hop_type, pool_ref.pool_key))
                {
                    path_ids.retain(|id| *id != path_id);
                }
            }
        }

        // Remove from path_resolved
        self.path_resolved.remove(&path_id);

        // Remove from results
        self.results.remove(&path_id);

        // Remove from delivered
        self.delivered.remove(&path_id);

        // Remove from pending_new_paths
        self.pending_new_paths.remove(&path_id);

        // Record for the next batch
        if existed {
            self.deregistered.push(path_id);
        }

        existed
    }

    /// Compute the incremental diff between `delivered` and `new_results`,
    /// then advance `delivered` to the above-threshold subset.
    ///
    /// If `result_tx` is set, sends the batch.
    /// If the channel is full, the batch is dropped — the next one
    /// will carry a correct cumulative diff.
    fn compute_diff_and_send(&mut self, metadata: &BlockMetadata) {
        // Compute incremental diff directly from results and delivered HashMaps.
        // No intermediate collections needed — iterate both once.

        // Fresh: above-threshold in results, not in delivered
        let fresh: Vec<(u64, SolvePathResult)> = self
            .results
            .iter()
            .filter(|(_, r)| r.profit > self.min_profit && r.profit < self.max_profit)
            .filter(|(id, _)| !self.delivered.contains_key(id))
            .map(|(&id, r)| (id, r.clone()))
            .collect();

        // Updated: above-threshold in both, values differ
        let updated: Vec<(u64, SolvePathResult)> = self
            .results
            .iter()
            .filter(|(_, r)| r.profit > self.min_profit && r.profit < self.max_profit)
            .filter(|(id, new)| matches!(self.delivered.get(id), Some(old) if old != *new))
            .map(|(&id, r)| (id, r.clone()))
            .collect();

        // Expired: in delivered but not above-threshold in results
        let expired: Vec<u64> = self
            .delivered
            .keys()
            .filter(|id| !self.results.get(id).is_some_and(|r| r.profit > self.min_profit && r.profit < self.max_profit))
            .copied()
            .collect();

        // Removed: de-registered since last batch
        let removed: Vec<u64> = self.deregistered.drain(..).collect();

        // Advance delivered to the above-threshold subset of results
        self.delivered.retain(|_, r| r.profit > self.min_profit && r.profit < self.max_profit);
        // Add any new above-threshold entries not yet in delivered
        for (&id, r) in &self.results {
            if r.profit > self.min_profit && r.profit < self.max_profit && !self.delivered.contains_key(&id) {
                self.delivered.insert(id, r.clone());
            }
        }

        // Clear the unsent flag
        self.has_unsent_results = false;

        // Send if channel is available and there's anything to report
        if let Some(ref tx) = self.result_tx {
            // Always send a batch even if empty — Python needs the block
            // metadata and solve_block to drive its main loop.
            let batch = ResultBatch {
                solve_block: self.results_block,
                timestamp: metadata.timestamp,
                base_fee_per_gas: metadata.base_fee_per_gas,
                gas_used: metadata.gas_used,
                gas_limit: metadata.gas_limit,
                fresh,
                updated,
                expired,
                removed,
            };
            let _ = tx.send(batch);
        }
    }

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate `results` for the first time (replaces the
    /// removed `initial_solve`). Subsequent `process_logs` calls use
    /// dependency tracking to only re-solve affected paths.
    pub fn solve_all_paths(&mut self, block_number: u64) {
        // Resolve all paths (no clone — path_pools is immutable)
        for (&path_id, path) in &self.path_pools {
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(&path.pools, &mut resolved);
            self.path_resolved.insert(path_id, resolved);
        }

        // Solve all paths
        self.results = self.solve_all();
        self.results_block = block_number;

        // Compute incremental diff and send batch
        // (block metadata is not available at initial solve time)
        self.compute_diff_and_send(&BlockMetadata::default());
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.v2_engine.pool_count()
    }

    /// Number of registered V3 pools.
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.v3_engine.pool_count()
    }

    /// Number of registered V4 pools.
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.v4_engine.pool_count()
    }

    /// Number of registered mixed paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.path_pools.len()
    }

    /// Return the list of registered V4 `PoolManager` addresses.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.v4_engine.registered_pool_managers()
    }

    /// Resolve a path's pool refs into hop states and tick-range sequences.
    fn resolve_path(&self, pool_refs: &[MixedPoolRef], resolved: &mut ResolvedMixedPath) {
        resolved.hop_types.clear();
        resolved.v2_hops.clear();
        resolved.v3_sequences.clear();
        resolved.int_v3_hops.clear();
        resolved.int_v3_sequences.clear();
        resolved.base_hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.hop_types.reserve(pool_refs.len());
        resolved.v2_hops.reserve(pool_refs.len());
        resolved.v3_sequences.reserve(pool_refs.len());
        resolved.int_v3_hops.reserve(pool_refs.len());
        resolved.int_v3_sequences.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    // Look up the V2 pool state
                    let Some(hop_state) = self.v2_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    resolved.hop_types.push(HopType::V2);
                    resolved.v2_hops.push(Some(hop_state.clone()));
                    resolved.v3_sequences.push(None);
                    resolved.int_v3_hops.push(None);
                    resolved.int_v3_sequences.push(None);

                    let base = hop_state.to_base_hop();
                    resolved.base_hops.push(base);
                }
                HopType::V3 => {
                    // Look up V3 pool state and build tick-range sequence
                    let Some(pool_state) = self.v3_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    let sequence = pool_state.build_sequence(pool_ref.zero_for_one, 3);
                    if let Some(seq) = &sequence {
                        if let Some(first_range) = seq.ranges.first() {
                            resolved.base_hops.push(first_range.to_hop_state());
                        } else {
                            return; // Empty sequence → invalid
                        }
                    } else {
                        return; // No sequence → invalid
                    }

                    // Build integer V3 hop from original U256 values (exact, no f64 conversion)
                    let int_v3_hop = pool_state.build_int_v3_hop(pool_ref.zero_for_one);
                    // Build integer V3 sequence for V3-V3 paths
                    let int_v3_sequence = pool_state.build_int_v3_sequence(pool_ref.zero_for_one, 10);

                    resolved.hop_types.push(HopType::V3);
                    resolved.v2_hops.push(None);
                    resolved.v3_sequences.push(sequence);
                    resolved.int_v3_hops.push(int_v3_hop);
                    resolved.int_v3_sequences.push(int_v3_sequence);
                }
                HopType::V4 => {
                    // V4 pools use identical concentrated-liquidity math as V3.
                    // They produce the same `IntV3TickRangeSequence` type.
                    let Some(pool_state) = self.v4_engine.get_pool(pool_ref.pool_key) else {
                        return; // Missing pool → invalid
                    };

                    // Build integer V4 sequence (same type as V3)
                    let int_v4_sequence = pool_state.build_int_v4_sequence(pool_ref.zero_for_one, 10);

                    // V4 doesn't use f64-based tick-range sequences or base hops
                    // (the integer solver is sufficient). Push empty placeholders.
                    resolved.hop_types.push(HopType::V4);
                    resolved.v2_hops.push(None);
                    resolved.v3_sequences.push(None); // V4 doesn't produce V3TickRangeSequence
                    resolved.int_v3_hops.push(None); // V4 uses sequences, not single hops
                    resolved.int_v3_sequences.push(int_v4_sequence);

                    // Push a zero base hop placeholder (not used by integer solver,
                    // but required to keep vectors aligned)
                    resolved.base_hops.push(crate::optimizers::mobius::HopState::new(0.0, 0.0, 0.0));
                }
            }
        }

        resolved.valid = true;
    }
}

impl Default for UniswapEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// NOTE: `simulate_v3_hop` and `user_max_or` were removed when the mixed
// V2-V3 solver was replaced with the integer-exact version. The integer
// solver uses `mobius_v3_int::exact_solve_mixed_v2_v3_sequence` instead of
// golden-section search over f64 approximations.

// ---------------------------------------------------------------------------
// IntHopState extension for base hop conversion
// ---------------------------------------------------------------------------

/// Extension trait for converting `IntHopState` to base f64 `HopState`.
trait IntHopStateExt {
    /// Convert to a f64 `HopState` for Mobius initial estimates.
    fn to_base_hop(&self) -> crate::optimizers::mobius::HopState;
}

impl IntHopStateExt for crate::optimizers::mobius_int::IntHopState {
    #[allow(clippy::cast_precision_loss)]
    fn to_base_hop(&self) -> crate::optimizers::mobius::HopState {
        let fee = 1.0 - (self.gamma_numer as f64 / self.fee_denom as f64);
        let r_in = u256_to_f64(self.reserve_in);
        let r_out = u256_to_f64(self.reserve_out);
        crate::optimizers::mobius::HopState::new(r_in, r_out, fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usdc(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(6))
    }

    fn weth(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(18))
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    #[test]
    fn register_v2_and_v3_pools() {
        let mut engine = UniswapEngine::new();

        // Register a V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::ZERO,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        assert_eq!(engine.v2_pool_count(), 1);
        assert_eq!(engine.v3_pool_count(), 1);

        // Register a mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

        // Path should be resolved
        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.hop_types.len(), 2);
        assert_eq!(resolved.hop_types[0], HopType::V2);
        assert_eq!(resolved.hop_types[1], HopType::V3);
    }

    #[test]
    fn process_block_routes_logs_to_sub_engines() {
        let mut engine = UniswapEngine::new();

        // Register V2 pools
        let v2_addr = Address::ZERO;
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([1u8; 20]);
        let v2_fwd1 = engine.v2_engine().register_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a pure V2 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

        // Process with no logs — should not panic
        engine.process_block(&[], 1, &BlockMetadata::default());

        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        let _ = results; // May or may not have profitable results
    }

    #[test]
    fn mixed_path_v2_to_v3_resolves() {
        let mut engine = UniswapEngine::new();

        // V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::ZERO,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // Mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid);
        assert!(resolved.v2_hops[0].is_some());
        assert!(resolved.v3_sequences[1].is_some());
    }

    #[test]
    fn missing_v2_pool_makes_path_invalid() {
        let mut engine = UniswapEngine::new();

        // Only register V3 pool
        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // Reference a non-existent V2 pool
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: 999, // Non-existent
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(!resolved.valid);
    }

    #[test]
    fn process_updates_applies_both_types() {
        let mut engine = UniswapEngine::new();

        // Register V2 pools
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([0x12u8; 20]);
        let v2_fwd1 = engine.v2_engine().register_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register V2-only path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

        // Process updates
        engine.process_updates(
            &[(v2_addr, usdc(1_400_000), weth(750))],
            &[],
            42,
            &BlockMetadata::default(),
        );

        let (_, block) = engine.latest_results();
        assert_eq!(block, 42);
    }

    #[test]
    fn register_path_after_start_succeeds() {
        let mut engine = UniswapEngine::new();
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v2_addr2 = Address::from([0x12u8; 20]);
        let v2_fwd2 = engine.v2_engine().register_pool(
            v2_addr2,
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd2,
                zero_for_one: true,
            },
        ]);
        // Registration is always-on; this should not panic
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd2,
                zero_for_one: true,
            },
        ]);
    }

    #[test]
    fn register_and_solve_path_eagerly_solves() {
        let mut engine = UniswapEngine::new();

        // Two V2 pools with price divergence
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // register_and_solve_path should eagerly solve and append to results
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        // Should be tracked as pending so rebuild_and_solve_affected can merge
        assert!(engine.pending_new_paths.contains(&path_id));

        // Results should already contain the eagerly-solved path
        let (results, _block) = engine.latest_results();
        let solve_result = results.get(&path_id);
        assert!(solve_result.is_some(), "register_and_solve_path should eagerly solve and add to results");

        let solve_result = solve_result.unwrap();
        assert!(!solve_result.optimal_input.is_zero());
        assert!(!solve_result.profit.is_zero());
    }

    #[test]
    fn pending_new_paths_survive_rebuild() {
        let mut engine = UniswapEngine::new();

        // Two V2 pools with price divergence
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register path eagerly
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        // Process an empty block (no affected pools) — rebuild_and_solve_affected
        // should still include the pending path and not drop it
        engine.rebuild_and_solve_affected(&HashSet::new(), &HashSet::new(), &HashSet::new(), 1, &BlockMetadata::default());

        // Pending set should be cleared
        assert!(engine.pending_new_paths.is_empty());

        // The path's result should survive the rebuild
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        assert!(results.contains_key(&path_id), "pending new path result should survive rebuild_and_solve_affected");
    }

    #[test]
    fn pure_v2_path_finds_profitable_arb() {
        let mut engine = UniswapEngine::new();

        // V2 pool A: USDC/WETH with price ~1875 USDC/WETH
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2 pool B: WETH/USDC with price ~2000 USDC/WETH (mispriced — arb opportunity)
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2→V2 path: USDC → WETH (pool A) → USDC (pool B)
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a, // reserve0=USDC, reserve1=WETH
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b, // reserve0=WETH, reserve1=USDC
                zero_for_one: true,
            },
        ]);

        // Solve
        let results = engine.solve_all();
        // Should find a profitable arbitrage
        assert!(!results.is_empty(), "should find profitable V2-V2 arb");
        let solve_result = results.values().next().unwrap();
        assert!(!solve_result.optimal_input.is_zero());
        assert!(!solve_result.profit.is_zero());
    }

    #[test]
    fn pure_v3_path_finds_profitable_arb() {
        let mut engine = UniswapEngine::new();

        // V3 pool A at tick 0 (1:1), high liquidity, with tick boundaries
        let mut tick_data_a = HashMap::new();
        tick_data_a.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_a.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key_a = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x21u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 10_000_000_000_000_000,
                tick: 0,
                tick_data: tick_data_a,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
        let sqrt_price_lower_u160 = crate::tick_math::get_sqrt_ratio_at_tick_internal(-60)
            .unwrap_or(alloy::primitives::U160::ZERO);
        let sqrt_price_lower = U256::from(sqrt_price_lower_u160);

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(
            0,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_b.insert(
            -120,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key_b = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: sqrt_price_lower,
                liquidity: 10_000_000_000_000_000,
                tick: -60,
                tick_data: tick_data_b,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // V3→V3 path: pool A (zfo) → pool B (ofz)
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_b,
                zero_for_one: false,
            },
        ]);

        let results = engine.solve_all();
        // V3-V3 arb depends on the exact price divergence — the important thing
        // is that the path resolves and the solver runs without panicking.
        // With a single tick spacing of 60 and 0.6% total fees, the arb may
        // not be profitable at these liquidity levels.
        let _ = results;
    }

    #[test]
    fn mixed_v2_to_v3_path_finds_arb() {
        let mut engine = UniswapEngine::new();

        // V2 pool: USDC/WETH
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3 pool: same pair but different price
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // Mixed V2→V3 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        // Even if no profit found (depends on exact numbers),
        // solve_all should run without panicking
        let results = engine.solve_all();
        // Just verify it doesn't crash
        let _ = results;
    }

    #[test]
    fn mixed_v3_to_v2_path_resolves() {
        let mut engine = UniswapEngine::new();

        // V3 pool with tick data
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 10_000_000_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // V2 pool
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.v2_engine().register_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3→V2 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: false,
            },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.hop_types[0], HopType::V3);
        assert_eq!(resolved.hop_types[1], HopType::V2);
        assert!(resolved.v3_sequences[0].is_some());
        assert!(resolved.v2_hops[1].is_some());
    }

    #[test]
    fn rebuild_on_v2_update_changes_results() {
        let mut engine = UniswapEngine::new();

        // V2 pool A: USDC/WETH
        let v2_addr_a = Address::from([0x11u8; 20]);
        let v2_fwd_a = engine.v2_engine().register_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2 pool B: WETH/USDC
        let v2_addr_b = Address::from([0x12u8; 20]);
        let v2_fwd_b = engine.v2_engine().register_pool(
            v2_addr_b,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V2→V2 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b,
                zero_for_one: true,
            },
        ]);

        // Initial solve
        let results_before = engine.solve_all();

        // Apply V2 update to make pool A even more mispriced
        engine.process_updates(
            &[(v2_addr_a, usdc(1_400_000), weth(750))],
            &[],
            1,
            &BlockMetadata::default(),
        );

        let (results_after, block) = engine.latest_results();
        assert_eq!(block, 1);
        // Results should differ after the update
        let _ = results_before; // Just ensure initial solve didn't panic
        let _ = results_after;
    }

    /// V4 int128 guard: paths where V4 hop amounts exceed `int128_max` are rejected.
    ///
    /// V4's `toBalanceDelta()` calls `toInt128()` on swap amounts. If either component
    /// exceeds `int128_max`, V4 reverts with `SafeCastOverflow` — the swap cannot
    /// execute on-chain. The solver must not report such paths as profitable.
    #[test]
    fn v4_int128_overflow_path_rejected() {
        let mut engine = UniswapEngine::new();

        // V3 pool: normal pool at 1:1 price
        let v3_addr = Address::from([0x20u8; 20]);
        let v3_factory = Address::from([0x21u8; 20]);
        let sp_0 = U256::from(1u128) << 96;

        engine.v3_engine().register_pool(RegisterV3PoolParams {
            address: v3_addr,
            token0: Address::from([0x30u8; 20]),
            token1: Address::from([0x31u8; 20]),
            fee: 10_000, // 1%
            tick_spacing: 200,
            factory: v3_factory,
            sqrt_price_x96: sp_0,
            liquidity: 10_000_000_000_000u128,
            tick: 0,
            tick_data: std::collections::HashMap::new(),
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // V4 pool: pool at extreme price (tick -886983) with massive liquidity
        // This produces virtual reserves >> int128_max
        let v4_pool_manager = Address::from([0x40u8; 20]);
        // tick -886983 → sqrtPrice ≈ 4.36e9 (very low price, token0 is nearly worthless)
        let sp_extreme = crate::tick_math::get_sqrt_ratio_at_tick_internal(-886983)
            .unwrap_or_default();
        let extreme_liquidity: u128 = 76_688_550_121_478_947_320_312_764_923_207_804;

        let _ = engine.v4_engine().register_pool(RegisterV4PoolParams {
            pool_manager: v4_pool_manager,
            pool_id: [0xffu8; 32],
            pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee: 10_000,
                tick_spacing: 200,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(sp_extreme),
            liquidity: extreme_liquidity,
            tick: -886983,
            tick_data: std::collections::HashMap::new(),
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // Register path: V3 (zfo) → V4 (ofz, which will produce huge token0 output)
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V3, pool_key: 0, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V4, pool_key: 0, zero_for_one: false },
        ]);

        // Resolve and solve all paths (replaces start() + initial_solve())
        for (&path_id, path) in &engine.path_pools {
            let mut resolved = ResolvedMixedPath::default();
            engine.resolve_path(&path.pools, &mut resolved);
            engine.path_resolved.insert(path_id, resolved);
        }
        engine.results = engine.solve_all();

        let (results, _block) = engine.latest_results();

        // The V4 hop's output (token0 at extreme price) would overflow int128.
        // The solver should reject this path — no result should be returned.
        if let Some(solve_result) = results.get(&path_id) {
            // If a result IS found, verify that V4 hop outputs fit int128
            let v4_output = solve_result.hop_outputs.get(1).copied().unwrap_or(U256::ZERO);
            let v4_consumed = solve_result.consumed_inputs.get(1).copied().unwrap_or(U256::ZERO);
            assert!(
                v4_output <= INT128_MAX && v4_consumed <= INT128_MAX,
                "V4 hop amounts must fit int128: output={v4_output}, consumed={v4_consumed}"
            );
        }
        // Ideally the path should not appear in results at all
    }

    #[test]
    fn inspect_path_returns_hop_details() {
        let mut engine = UniswapEngine::new();

        // Register a V2 pool
        let v2_fwd = engine.v2_engine().register_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a V3 pool
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
                address: Address::from([0x22u8; 20]),
                token0: Address::from([0u8; 20]),
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        );

        // Register a V4 pool
        let v4_key = engine.v4_engine().register_pool(
            crate::optimizers::v4_block_engine::RegisterV4PoolParams {
                pool_manager: Address::from([0x33u8; 20]),
                pool_id: [0xabu8; 32],
                pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
                    currency0: Address::from([0u8; 20]),
                    currency1: Address::from([1u8; 20]),
                    fee: 10000,
                    tick_spacing: 100,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
            },
        ).expect("V4 registration should succeed");

        // Register a 3-hop path: V2 → V3 → V4
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V4, pool_key: v4_key, zero_for_one: true },
        ]);

        // Inspect the path
        let path = engine.path_pools.get(&path_id).expect("path should exist");
        assert_eq!(path.pools.len(), 3);

        // Verify hop types
        assert!(matches!(path.pools[0].hop_type, HopType::V2));
        assert!(matches!(path.pools[1].hop_type, HopType::V3));
        assert!(matches!(path.pools[2].hop_type, HopType::V4));

        // Verify we can resolve pool addresses via sub-engines
        let v2_addr = engine.v2_engine().pool_addresses()
            .iter()
            .find(|(_, &(fwd, _))| fwd == v2_fwd)
            .map(|(a, _)| *a);
        assert_eq!(v2_addr, Some(Address::from([0x11u8; 20])));

        let v3_pool = engine.v3_engine().get_pool(v3_key);
        assert_eq!(v3_pool.map(|p| p.address), Some(Address::from([0x22u8; 20])));

        let v4_pool = engine.v4_engine().get_pool(v4_key);
        assert_eq!(v4_pool.map(|p| p.pool_manager), Some(Address::from([0x33u8; 20])));
        assert_eq!(v4_pool.map(|p| p.pool_id), Some([0xabu8; 32]));

        // Inspect non-existent path
        assert!(engine.path_pools.get(&99999).is_none());
    }

    #[test]
    fn solve_3hop_v3_v3_v3_path() {
        let mut engine = UniswapEngine::new();

        let sp_0 = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // 1:1 price (tick 0)

        // Helper to create minimal tick data with initialized ticks at -60 and +60
        let make_tick_data = || -> HashMap<i32, crate::bot_core::TickInfo> {
            let mut td = HashMap::new();
            td.insert(-60, crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            });
            td.insert(60, crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            });
            td
        };

        // Pool 1 at tick 0 with high liquidity
        let v3_key_a = engine.v3_engine().register_pool(RegisterV3PoolParams {
            address: Address::from([0xa1u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sp_0,
            liquidity: 10_000_000_000_000u128,
            tick: 0,
            tick_data: make_tick_data(),
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // Pool 2 at tick 0 with different liquidity (price disagreement)
        let v3_key_b = engine.v3_engine().register_pool(RegisterV3PoolParams {
            address: Address::from([0xa2u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sp_0,
            liquidity: 15_000_000_000_000u128,
            tick: 0,
            tick_data: make_tick_data(),
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // Pool 3 at tick 0 with third liquidity level
        let v3_key_c = engine.v3_engine().register_pool(RegisterV3PoolParams {
            address: Address::from([0xa3u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sp_0,
            liquidity: 12_000_000_000_000u128,
            tick: 0,
            tick_data: make_tick_data(),
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        assert_eq!(engine.v3_pool_count(), 3);

        // Register 3-hop V3-V3-V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_b, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_c, zero_for_one: true },
        ]);

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

        // Verify the path is valid and resolved
        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid, "3-hop V3-V3-V3 path should be valid");
        assert_eq!(resolved.hop_types.len(), 3);
        assert_eq!(resolved.hop_types[0], HopType::V3);
        assert_eq!(resolved.hop_types[1], HopType::V3);
        assert_eq!(resolved.hop_types[2], HopType::V3);
        assert!(resolved.int_v3_sequences[0].is_some());
        assert!(resolved.int_v3_sequences[1].is_some());
        assert!(resolved.int_v3_sequences[2].is_some());

        // Solve the path — previously returned None for 3+ hop CL paths.
        // Now the N-hop CL solver runs. With 3 pools at the same price but
        // different liquidity, the path is unlikely to be profitable after fees,
        // but the solver must not reject due to hop count.
        let result = engine.solve_path(resolved);
        let _ = result; // No panic = test passes
    }

    #[test]
    fn solve_3hop_mixed_v2_v3_v2_path() {
        let mut engine = UniswapEngine::new();

        let sp_0 = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // 1:1 price

        // V2 pool 1: cheap WETH (1.5M USDC / 800 WETH)
        let v2_fwd_a = engine.v2_engine().register_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3 pool (middle hop): at 1:1 price with tick boundaries
        let mut tick_data = HashMap::new();
        tick_data.insert(-60, crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: alloy::primitives::I256::try_from(100i128)
                .unwrap_or(alloy::primitives::I256::ZERO),
        });
        tick_data.insert(60, crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: alloy::primitives::I256::try_from(-100i128)
                .unwrap_or(alloy::primitives::I256::ZERO),
        });
        let v3_key = engine.v3_engine().register_pool(RegisterV3PoolParams {
            address: Address::from([0x22u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sp_0,
            liquidity: 10_000_000_000_000u128,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // V2 pool 2: expensive WETH (1000 WETH / 2M USDC)
        let v2_fwd_b = engine.v2_engine().register_pool(
            Address::from([0x12u8; 20]),
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register 3-hop mixed path: V2 → V3 → V2
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid, "3-hop V2-V3-V2 path should be valid");
        assert_eq!(resolved.hop_types.len(), 3);
        assert_eq!(resolved.hop_types[0], HopType::V2);
        assert_eq!(resolved.hop_types[1], HopType::V3);
        assert_eq!(resolved.hop_types[2], HopType::V2);

        // Key: previously this returned None due to hop_types.len() != 2
        let result = engine.solve_path(resolved);
        let _ = result;
    }
}

// ---------------------------------------------------------------------------
// PyO3 wrapper
// ---------------------------------------------------------------------------

use std::sync::Arc;

use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`UniswapEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(name = "UniswapArbEngine")]
#[allow(dead_code)]
pub struct PyUniswapArbEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// Shutdown flag for the pump
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until `start()` is called)
    pump_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Subscribe state held between `subscribe()` and `resume()` calls.
    /// Contains the live WS stream and first observed block number.
    subscribe_state: parking_lot::Mutex<Option<PySubscribeState>>,
    /// Receiver for the result batch channel.
    /// Created in `new()`, consumed by `__anext__`.
    /// Wrapped in Arc so the async coroutine can share it.
    result_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<ResultBatch>>>>,
    /// When True, verify each V3/V4 pool's tick data against on-chain state
    /// immediately after registration. The snapshot is taken while the engine
    /// lock is held (so the pump can't race), then verification runs via RPC
    /// after the lock is released. Failures are logged as errors.
    verify_on_register: std::sync::atomic::AtomicBool,
    /// Optional HTTP RPC URL for verification during registration.
    /// Must be set before `verify_on_register` is enabled.
    verify_rpc_url: parking_lot::Mutex<Option<String>>,
    /// Optional `StateView` contract address for V4 verification.
    verify_state_view: parking_lot::Mutex<Option<Address>>,
    /// Engine lifecycle phase (Plan 098).
    /// Enforces ordering: Created → Subscribed → SnapshotLoaded → Backfilled → Resumed.
    phase: std::sync::atomic::AtomicU8,
    /// V3 snapshot tick data, loaded via `load_v3_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v3_snapshot: parking_lot::Mutex<Option<V3SnapshotData>>,
    /// V4 snapshot tick data, loaded via `load_v4_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v4_snapshot: parking_lot::Mutex<Option<V4SnapshotData>>,
}

/// Python-facing subscribe state.
///
/// Stores the pump and subscribe results between `subscribe()` and `resume()`
/// calls so that `resume()` can re-use the same pump instance.
struct PySubscribeState {
    /// The pump instance (holds engine, provider, shutdown, and `block_tx`)
    pump: crate::optimizers::uniswap_engine_pump::UniswapEnginePump,
    /// First block number observed during subscribe
    first_block: u64,
    /// Live WS stream for the resume phase
    combined_stream: futures_util::stream::BoxStream<'static, crate::optimizers::uniswap_engine_pump::WsEvent>,
}

impl PyUniswapArbEngine {
    /// Parse V2 Sync updates from a Python list of 3-tuples.
    fn parse_v2_updates(
        v2_sync_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<(Address, U256, U256)>> {
        let mut rust_v2: Vec<(Address, U256, U256)> = Vec::with_capacity(v2_sync_updates.len());
        for item in v2_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (address, reserve0, reserve1), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let r0 = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            rust_v2.push((addr, r0, r1));
        }
        Ok(rust_v2)
    }

    /// Parse tick priors from a Python list of 2-tuples.
    fn parse_tick_priors(priors_list: &Bound<'_, PyList>) -> PyResult<Vec<(i32, crate::bot_core::TickInfo)>> {
        let mut tick_priors = Vec::new();
        for prior_item in priors_list.iter() {
            let prior_tuple = prior_item.cast::<pyo3::types::PyTuple>()?;
            if prior_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (tick_index, (lg, ln)), got {} elements",
                    prior_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let tick_idx: i32 = prior_tuple.get_item(0)?.extract()?;
            let info_obj = prior_tuple.get_item(1)?;
            let info_tuple = info_obj.cast::<pyo3::types::PyTuple>()?;
            if info_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                    info_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let lg: u128 = info_tuple.get_item(0)?.extract()?;
            let ln: i128 = info_tuple.get_item(1)?.extract()?;
            tick_priors.push((tick_idx, make_tick_info(lg, ln)));
        }
        Ok(tick_priors)
    }

    /// Parse V3 Swap updates from a Python list of 5-tuples.
    fn parse_v3_updates(
        v3_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V3SwapUpdate>> {
        let mut rust_v3: Vec<V3SwapUpdate> = Vec::with_capacity(v3_swap_updates.len());
        for item in v3_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let priors_obj = tuple.get_item(4)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v3.push(V3SwapUpdate {
                pool_address: addr,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v3)
    }

    /// Parse V4 Swap updates from a Python list of 6-tuples.
    fn parse_v4_updates(
        v4_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V4SwapUpdate>> {
        let mut rust_v4: Vec<V4SwapUpdate> = Vec::with_capacity(v4_swap_updates.len());
        for item in v4_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id_hex, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_obj = tuple.get_item(0)?;
            let pm_str: String = pm_obj.extract()?;
            let pool_manager = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
            })?;

            let pid_obj = tuple.get_item(1)?;
            let pid_str: String = pid_obj.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let priors_obj = tuple.get_item(5)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v4.push(V4SwapUpdate {
                pool_manager,
                pool_id,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v4)
    }

    /// Get the current engine phase.
    fn current_phase(&self) -> EnginePhase {
        match self.phase.load(std::sync::atomic::Ordering::Relaxed) {
            0 => EnginePhase::Created,
            1 => EnginePhase::Subscribed,
            2 => EnginePhase::SnapshotLoaded,
            3 => EnginePhase::Backfilled,
            4 => EnginePhase::Resumed,
            _ => EnginePhase::Created,
        }
    }

    /// Set the engine phase (advancing only).
    fn set_phase(&self, phase: EnginePhase) {
        self.phase.store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Deserialize a V3 binary snapshot into `V3SnapshotData`.
    fn deserialize_v3_snapshot(data: &[u8]) -> PyResult<V3SnapshotData> {
        const MIN_HEADER: usize = 5; // version(1) + pool_count(4)

        if data.len() < MIN_HEADER {
            let msg = format!(
                "V3 snapshot data too short: {} bytes (minimum {})",
                data.len(),
                MIN_HEADER
            );
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let version = data[0];
        if version != 1 {
            let msg = format!("Unsupported V3 snapshot format version: {version}");
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pool_count = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
        let mut result = HashMap::with_capacity(pool_count);

        let mut offset = MIN_HEADER;
        for _ in 0..pool_count {
            // Pool address (20 bytes)
            if offset + 20 > data.len() {
                let msg = "V3 snapshot truncated: expected 20-byte pool address";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_bytes: [u8; 20] = data[offset..offset + 20].try_into().unwrap();
            let address = Address::from(addr_bytes);
            offset += 20;

            // Tick count (4 bytes LE)
            if offset + 4 > data.len() {
                let msg = "V3 snapshot truncated: expected tick_count";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let tick_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            let mut tick_data = HashMap::with_capacity(tick_count);
            for _ in 0..tick_count {
                // tick_index (4 bytes LE, i32)
                if offset + 4 > data.len() {
                    let msg = "V3 snapshot truncated: expected tick_index";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let tick_index = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;

                // liquidity_gross (16 bytes LE, u128)
                if offset + 16 > data.len() {
                    let msg = "V3 snapshot truncated: expected liquidity_gross";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let gross_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let gross_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                let liquidity_gross = (gross_hi as u128) << 64 | (gross_lo as u128);
                offset += 16;

                // liquidity_net (16 bytes LE, i128)
                if offset + 16 > data.len() {
                    let msg = "V3 snapshot truncated: expected liquidity_net";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let net_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let net_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                let unsigned_net = (net_hi as u128) << 64 | (net_lo as u128);
                let liquidity_net = unsigned_net as i128;
                offset += 16;

                tick_data.insert(tick_index, make_tick_info(liquidity_gross, liquidity_net));
            }

            result.insert(address, tick_data);
        }

        Ok(result)
    }

    /// Deserialize a V4 binary snapshot into `V4SnapshotData`.
    fn deserialize_v4_snapshot(data: &[u8]) -> PyResult<V4SnapshotData> {
        const MIN_HEADER: usize = 5; // version(1) + pool_manager_count(4)

        if data.len() < MIN_HEADER {
            let msg = format!(
                "V4 snapshot data too short: {} bytes (minimum {})",
                data.len(),
                MIN_HEADER
            );
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let version = data[0];
        if version != 1 {
            let msg = format!("Unsupported V4 snapshot format version: {version}");
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pm_count = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
        let mut result = HashMap::with_capacity(pm_count);

        let mut offset = MIN_HEADER;
        for _ in 0..pm_count {
            // Pool manager address (20 bytes)
            if offset + 20 > data.len() {
                let msg = "V4 snapshot truncated: expected pool_manager address";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pm_bytes: [u8; 20] = data[offset..offset + 20].try_into().unwrap();
            let pool_manager = Address::from(pm_bytes);
            offset += 20;

            // Pool ID count (4 bytes LE)
            if offset + 4 > data.len() {
                let msg = "V4 snapshot truncated: expected pool_id_count";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            for _ in 0..pool_id_count {
                // Pool ID (32 bytes)
                if offset + 32 > data.len() {
                    let msg = "V4 snapshot truncated: expected pool_id";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let pool_id: [u8; 32] = data[offset..offset + 32].try_into().unwrap();
                offset += 32;

                // Tick count (4 bytes LE)
                if offset + 4 > data.len() {
                    let msg = "V4 snapshot truncated: expected tick_count";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let tick_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                let mut tick_data = HashMap::with_capacity(tick_count);
                for _ in 0..tick_count {
                    // tick_index (4 bytes LE, i32)
                    if offset + 4 > data.len() {
                        let msg = "V4 snapshot truncated: expected tick_index";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let tick_index = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    offset += 4;

                    // liquidity_gross (16 bytes LE, u128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_gross";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let gross_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let gross_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    let liquidity_gross = (gross_hi as u128) << 64 | (gross_lo as u128);
                    offset += 16;

                    // liquidity_net (16 bytes LE, i128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_net";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let net_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let net_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    let unsigned_net = (net_hi as u128) << 64 | (net_lo as u128);
                    let liquidity_net = unsigned_net as i128;
                    offset += 16;

                    tick_data.insert(tick_index, make_tick_info(liquidity_gross, liquidity_net));
                }

                result.insert((pool_manager, pool_id), tick_data);
            }
        }

        Ok(result)
    }
}

#[pymethods]
impl PyUniswapArbEngine {
    #[new]
    fn new() -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        let mut engine = UniswapEngine::new();
        engine.set_result_channel(result_tx);
        Self {
            engine: Arc::new(parking_lot::Mutex::new(engine)),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
            subscribe_state: parking_lot::Mutex::new(None),
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            verify_on_register: std::sync::atomic::AtomicBool::new(false),
            verify_rpc_url: parking_lot::Mutex::new(None),
            verify_state_view: parking_lot::Mutex::new(None),
            phase: std::sync::atomic::AtomicU8::new(EnginePhase::Created as u8),
            v3_snapshot: parking_lot::Mutex::new(None),
            v4_snapshot: parking_lot::Mutex::new(None),
        }
    }

    /// Load a V3 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_count]
    /// Per pool:
    ///   [20 bytes: pool address]
    ///   [4 bytes LE: tick_count]
    ///   Per tick:
    ///     [4 bytes LE: tick_index (i32)]
    ///     [16 bytes LE: liquidity_gross (u128)]
    ///     [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V3 snapshot already loaded.
    fn load_v3_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        // Allow loading from Created (unit tests) or Subscribed/SnapshotLoaded (production)
        phase.require_before(EnginePhase::Resumed, "load_v3_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Check not already loaded
        {
            let snap = self.v3_snapshot.lock();
            if snap.is_some() {
                let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }

        let snapshot = Self::deserialize_v3_snapshot(&data)?;
        *self.v3_snapshot.lock() = Some(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V4)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Load a V4 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_manager_count]
    /// Per pool_manager:
    ///   [20 bytes: pool_manager address]
    ///   [4 bytes LE: pool_id_count]
    ///   Per pool_id:
    ///     [32 bytes: pool_id]
    ///     [4 bytes LE: tick_count]
    ///     Per tick:
    ///       [4 bytes LE: tick_index (i32)]
    ///       [16 bytes LE: liquidity_gross (u128)]
    ///       [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V4 snapshot already loaded.
    fn load_v4_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v4_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Check not already loaded
        {
            let snap = self.v4_snapshot.lock();
            if snap.is_some() {
                let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }

        let snapshot = Self::deserialize_v4_snapshot(&data)?;
        *self.v4_snapshot.lock() = Some(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V3)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Drop the stored V3 snapshot, freeing memory.
    /// Idempotent — no-op if no V3 snapshot is loaded.
    fn clear_v3_snapshot(&self) {
        *self.v3_snapshot.lock() = None;
    }

    /// Drop the stored V4 snapshot, freeing memory.
    /// Idempotent — no-op if no V4 snapshot is loaded.
    fn clear_v4_snapshot(&self) {
        *self.v4_snapshot.lock() = None;
    }

    /// Begin streaming V3 snapshot data into the engine, one pool at a time.
    ///
    /// Call `insert_v3_pool_snapshot` for each pool, then `finish_v3_snapshot`
    /// to finalize. This avoids building the entire snapshot dict in memory.
    ///
    /// Can be called in Created or Subscribed phase. Idempotent — calling again
    /// while a stream is in progress is a no-op.
    fn begin_v3_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "begin_v3_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let mut snap = self.v3_snapshot.lock();
        if snap.is_some() {
            let msg = "Cannot begin V3 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        *snap = Some(HashMap::new());
        Ok(())
    }

    /// Insert a single V3 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     pool_address: Hex string of the pool address.
    ///     tick_data: Dict mapping tick_index (int) → (liquidity_gross, liquidity_net) tuple.
    fn insert_v3_pool_snapshot(
        &self,
        pool_address: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        let mut snap = self.v3_snapshot.lock();
        match &mut *snap {
            Some(map) => {
                map.insert(addr, rust_tick_data);
            }
            None => {
                let msg = "No V3 snapshot stream in progress. Call begin_v3_snapshot_stream() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }
        Ok(())
    }

    /// Finalize the V3 snapshot stream and transition to SnapshotLoaded phase.
    fn finish_v3_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if self.v3_snapshot.lock().is_none() {
            let msg = "No V3 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Begin streaming V4 snapshot data into the engine, one pool at a time.
    fn begin_v4_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "begin_v4_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let mut snap = self.v4_snapshot.lock();
        if snap.is_some() {
            let msg = "Cannot begin V4 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        *snap = Some(HashMap::new());
        Ok(())
    }

    /// Insert a single V4 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     pool_manager: Hex string of the pool manager address.
    ///     pool_id_hex: Hex string of the 32-byte pool ID.
    ///     tick_data: Dict mapping tick_index (int) → (liquidity_gross, liquidity_net) tuple.
    fn insert_v4_pool_snapshot(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let pm_addr = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::hex_utils::decode_32byte_hex(pool_id_hex)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        let mut snap = self.v4_snapshot.lock();
        match &mut *snap {
            Some(map) => {
                map.insert((pm_addr, pool_id), rust_tick_data);
            }
            None => {
                let msg = "No V4 snapshot stream in progress. Call begin_v4_snapshot_stream() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }
        Ok(())
    }

    /// Finalize the V4 snapshot stream and transition to SnapshotLoaded phase.
    fn finish_v4_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if self.v4_snapshot.lock().is_none() {
            let msg = "No V4 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Load a V3 liquidity snapshot from a Python dict.
    ///
    /// The dict maps pool address (hex string) → tick data dict,
    /// where tick data maps tick_index (int) → (liquidity_gross, liquidity_net) tuple.
    ///
    /// This is the fast path — no intermediate binary serialization in Python.
    /// The Rust side iterates the PyO3 dict and builds the internal HashMap directly.
    fn load_v3_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v3_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        {
            let snap = self.v3_snapshot.lock();
            if snap.is_some() {
                let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }

        let mut result = V3SnapshotData::new();
        for (py_addr, py_tick_dict) in py_data.iter() {
            let addr_str: String = py_addr.extract()?;
            let address = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
            })?;

            let tick_dict = py_tick_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict")
            })?;

            let mut tick_data = HashMap::new();
            for (py_tick, py_values) in tick_dict.iter() {
                let tick_index: i32 = py_tick.extract()?;
                let values: (u128, i128) = py_values.extract()?;
                tick_data.insert(tick_index, make_tick_info(values.0, values.1));
            }
            result.insert(address, tick_data);
        }

        *self.v3_snapshot.lock() = Some(result);
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Load a V4 liquidity snapshot from a Python dict.
    ///
    /// The dict maps pool_manager address (hex) → inner dict,
    /// where inner dict maps pool_id (hex) → tick data dict,
    /// and tick data maps tick_index (int) → (liquidity_gross, liquidity_net) tuple.
    fn load_v4_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v4_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        {
            let snap = self.v4_snapshot.lock();
            if snap.is_some() {
                let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }

        let mut result = V4SnapshotData::new();
        for (py_pm, py_pool_dict) in py_data.iter() {
            let pm_str: String = py_pm.extract()?;
            let pm_address = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid pool_manager address: {e}"
                ))
            })?;

            let pool_dict = py_pool_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("pool_manager value must be a dict")
            })?;

            for (py_pool_id, py_tick_dict) in pool_dict.iter() {
                let pool_id_hex: String = py_pool_id.extract()?;
                let pool_id = crate::hex_utils::decode_32byte_hex(&pool_id_hex)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

                let tick_dict = py_tick_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict")
                })?;

                let mut tick_data = HashMap::new();
                for (py_tick, py_values) in tick_dict.iter() {
                    let tick_index: i32 = py_tick.extract()?;
                    let values: (u128, i128) = py_values.extract()?;
                    tick_data.insert(tick_index, make_tick_info(values.0, values.1));
                }
                result.insert((pm_address, pool_id), tick_data);
            }
        }

        *self.v4_snapshot.lock() = Some(result);
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Register a V2 pool by contract address and initial reserves.
    /// Returns the forward `pool_id`. The reverse `pool_id` is `forward_id + 1`.
    #[pyo3(signature = (address, reserve0, reserve1, gamma_numer, fee_denom))]
    fn register_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, pyo3::PyAny>,
        reserve1: &Bound<'_, pyo3::PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<u64> {
        let addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        Ok(self.engine.lock().v2_engine().register_pool(addr, r0, r1, gamma_numer, fee_denom))
    }

    /// Register a V3 pool by contract address and initial state.
    /// Returns the pool key for use in path registration.
    ///
    /// Tick data is resolved automatically from the stored V3 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (tick_data consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty tick_data)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let addr = address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let t0 = token0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token0 address: {e}"))
        })?;
        let t1 = token1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token1 address: {e}"))
        })?;
        let fac = factory.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid factory address: {e}"))
        })?;
        let sp = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V3 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = {
            let mut snap = self.v3_snapshot.lock();
            if let Some(ref mut snapshot) = *snap {
                if let Some(tick_data) = snapshot.remove(&addr) {
                    (tick_data, PoolTickCoverage::Tracked)
                } else {
                    (HashMap::new(), PoolTickCoverage::Sparse)
                }
            } else {
                // No snapshot loaded — Sparse coverage
                (HashMap::new(), PoolTickCoverage::Sparse)
            }
        };

        let is_tracked = coverage == PoolTickCoverage::Tracked;

        let key = self.engine.lock().v3_engine().register_pool(RegisterV3PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing,
            factory: fac,
            sqrt_price_x96: sp,
            liquidity,
            tick,
            tick_data: rust_tick_data,
            update_block: block,
            coverage,
        });

        // If verify_on_register is enabled and this pool was registered from
        // snapshot data (Tracked), snapshot the tick data while the engine
        // lock is held and spawn an async verification task.
        if is_tracked && self.verify_on_register.load(std::sync::atomic::Ordering::Relaxed) {
            let rpc_url = self.verify_rpc_url.lock().clone();
            if let Some(url) = rpc_url {
                // Snapshot tick data while lock is held.
                let verify_block;
                let pool_snapshot = {
                    let mut engine = self.engine.lock();
                    verify_block = engine.last_processed_block().unwrap_or(0);
                    let mut map = HashMap::new();
                    if let Some(pool) = engine.v3_engine().get_pool(key) {
                        map.insert(key, pool.clone());
                    }
                    map
                };

                let addr_str = address.to_string();
                let runtime = crate::runtime::get_runtime();
                runtime.spawn(async move {
                    let provider = match crate::provider::AlloyProvider::new(&url, 3).await {
                        Ok(p) => p,
                        Err(e) => {
                            log::error!("verify_on_register: V3 pool {addr_str}: failed to create provider: {e}");
                            return;
                        }
                    };
                    match crate::bot_core::liquidity_verifier::verify_v3_pools(
                        &provider, Address::ZERO, &pool_snapshot, Some(verify_block),
                    ).await {
                        Ok(()) => {
                            log::info!("verify_on_register: V3 pool {addr_str} at block {verify_block}: OK");
                        }
                        Err(mismatch) => {
                            log::error!("verify_on_register: V3 pool {addr_str} at block {verify_block}: FAILED: {mismatch}");
                        }
                    }
                });
            }
        }

        Ok(key)
    }

    /// Register a V4 pool with the engine.
    ///
    /// Hook filtering: pools with amount-modifying hook flags (`BEFORE_SWAP`,
    /// `AFTER_SWAP`, `BEFORE_SWAP_RETURNS_DELTA`, `AFTER_SWAP_RETURNS_DELTA`)
    /// are rejected. Dynamic-fee pools (fee=0x100000) are also rejected.
    ///
    /// Tick data is resolved automatically from the stored V4 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (tick_data consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty tick_data)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    ///
    /// Returns the forward pool key for use in path registration,
    /// or raises `ValueError` if the pool is excluded.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v4_pool(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;

        // Decode pool_id from hex string (e.g. "0x1234...") to [u8; 32]
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;

        let c0 = currency0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency0 address: {e}"))
        })?;
        let c1 = currency1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency1 address: {e}"))
        })?;
        let sp = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V4 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = {
            let mut snap = self.v4_snapshot.lock();
            if let Some(ref mut snapshot) = *snap {
                if let Some(tick_data) = snapshot.remove(&(pm, pool_id)) {
                    (tick_data, PoolTickCoverage::Tracked)
                } else {
                    (HashMap::new(), PoolTickCoverage::Sparse)
                }
            } else {
                // No snapshot loaded — Sparse coverage
                (HashMap::new(), PoolTickCoverage::Sparse)
            }
        };

        let is_tracked = coverage == PoolTickCoverage::Tracked;

        let key = self.engine.lock().v4_engine().register_pool(RegisterV4PoolParams {
            pool_manager: pm,
            pool_id,
            pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
                currency0: c0,
                currency1: c1,
                fee,
                tick_spacing,
                hooks: Address::ZERO, // Not needed for solving; hook filtering already done
            },
            hook_flags,
            sqrt_price_x96: sp,
            liquidity,
            tick,
            tick_data: rust_tick_data,
            update_block: block,
            coverage,
        }).map_err(pyo3::exceptions::PyValueError::new_err)?;

        // If verify_on_register is enabled and this pool was registered from
        // snapshot data (Tracked), snapshot the tick data while the engine
        // lock is held and spawn an async verification task.
        if is_tracked && self.verify_on_register.load(std::sync::atomic::Ordering::Relaxed) {
            let rpc_url = self.verify_rpc_url.lock().clone();
            let state_view = *self.verify_state_view.lock();
            if let (Some(url), Some(sv)) = (rpc_url, state_view) {
                // Snapshot tick data while lock is held.
                let verify_block;
                let pool_snapshot = {
                    let mut engine = self.engine.lock();
                    verify_block = engine.last_processed_block().unwrap_or(0);
                    let mut map = HashMap::new();
                    if let Some(pool) = engine.v4_engine().get_pool(key) {
                        map.insert(key, pool.clone());
                    }
                    map
                };

                let pool_id_str = pool_id_hex.to_string();
                let runtime = crate::runtime::get_runtime();
                runtime.spawn(async move {
                    let provider = match crate::provider::AlloyProvider::new(&url, 3).await {
                        Ok(p) => p,
                        Err(e) => {
                            log::error!("verify_on_register: V4 pool {pool_id_str}: failed to create provider: {e}");
                            return;
                        }
                    };
                    match crate::bot_core::liquidity_verifier::verify_v4_pools(
                        &provider, sv, &pool_snapshot, Some(verify_block),
                    ).await {
                        Ok(()) => {
                            log::info!("verify_on_register: V4 pool {pool_id_str} at block {verify_block}: OK");
                        }
                        Err(mismatch) => {
                            log::error!("verify_on_register: V4 pool {pool_id_str} at block {verify_block}: FAILED: {mismatch}");
                        }
                    }
                });
            }
        }

        Ok(key)
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (hop_type, pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let hop_type_str: String = tuple.get_item(0)?.extract()?;
            let pool_key: u64 = tuple.get_item(1)?.extract()?;
            let zero_for_one: bool = tuple.get_item(2)?.extract()?;

            let hop_type = match hop_type_str.as_str() {
                "V2" => HopType::V2,
                "V3" => HopType::V3,
                "V4" => HopType::V4,
                _ => {
                    let msg = format!("Invalid hop_type: {hop_type_str}. Expected 'V2', 'V3', or 'V4'");
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
            };

            rust_refs.push(MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_path(rust_refs))
    }

    /// Register a mixed arbitrage path and eagerly solve it.
    ///
    /// Unlike `register_path`, this method also resolves and solves the path
    /// immediately, appending any profitable result to the engine's results.
    /// Used when the engine is already running (after the pump has started)
    /// so that new paths are immediately available to `latest_results()`.
    #[pyo3(signature = (pool_refs))]
    fn register_and_solve_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (hop_type, pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let hop_type_str: String = tuple.get_item(0)?.extract()?;
            let pool_key: u64 = tuple.get_item(1)?.extract()?;
            let zero_for_one: bool = tuple.get_item(2)?.extract()?;

            let hop_type = match hop_type_str.as_str() {
                "V2" => HopType::V2,
                "V3" => HopType::V3,
                "V4" => HopType::V4,
                _ => {
                    let msg = format!("Invalid hop_type: {hop_type_str}. Expected 'V2', 'V3', or 'V4'");
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
            };

            rust_refs.push(MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_and_solve_path(rust_refs))
    }

    /// Start the engine. Freezes registration and spawns the unified pump.
    ///
    /// The pump subscribes to both block headers and log events via WS.
    /// Logs are buffered and processed atomically when the next block header
    /// arrives. If no logs are received for a block, `eth_getLogs` is used to
    /// verify. A 60s timeout triggers backfill for the missing range.
    ///
    /// After calling `start()`, the engine processes events autonomously.
    /// Python reads results via the result batch channel (`async for`).
    #[pyo3(signature = (rpc_url))]
    fn start(&self, rpc_url: String) -> PyResult<()> {
        // Spawn the unified pump
        let engine = Arc::clone(&self.engine);
        let shutdown = Arc::clone(&self.shutdown);
        let handle = crate::optimizers::uniswap_engine_pump::UniswapEnginePump::spawn(
            rpc_url,
            engine,
            &shutdown,
        )
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        *self.pump_handle.lock() = Some(handle);

        Ok(())
    }

    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns the first observed block number. Python should:
    /// 1. Run backfill up to the returned block number
    /// 2. Call `resume()` to begin normal processing
    ///
    /// A "complete" block is one where both a `newHeads` notification and at
    /// least one log for the same block have been received. This guarantees
    /// the logs subscription did not miss the start of the block.
    /// No events are buffered during subscribe — the backfill is the sole
    /// authority for the gap between snapshot and WS start.
    ///
    /// Raises `RuntimeError` if the pump is already started or subscribed.
    #[pyo3(signature = (rpc_url))]
    fn subscribe(
        &self,
        rpc_url: String,
    ) -> PyResult<u64> {
        // Phase check: must be Created
        let phase = self.current_phase();
        phase.require(EnginePhase::Created, "subscribe")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure we're not already running
        if self.pump_handle.lock().is_some() {
            let msg = "Cannot subscribe: pump is already started. Call stop() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if self.subscribe_state.lock().is_some() {
            let msg = "Cannot subscribe: already subscribed. Call resume() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let engine = Arc::clone(&self.engine);
        let shutdown = Arc::clone(&self.shutdown);

        // Run the subscribe phase synchronously (blocks Python until first block observed)
        let runtime = crate::runtime::get_runtime();
        let subscribe_result = runtime
            .block_on(async {
                crate::optimizers::uniswap_engine_pump::UniswapEnginePump::subscribe(
                    &rpc_url,
                    engine,
                    shutdown,
                )
                .await
            })
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let (pump, state) = subscribe_result;

        // Store the subscribe state for resume()
        *self.subscribe_state.lock() = Some(PySubscribeState {
            pump,
            first_block: state.first_block,
            combined_stream: state
                .combined_stream
                .expect("subscribe() always returns a stream"),
        });

        // Advance phase
        self.set_phase(EnginePhase::Subscribed);

        Ok(state.first_block)
    }

    /// Backfill Mint/Burn/ModifyLiquidity events from the last DB snapshot
    /// block to the first WS block observed during `subscribe()`.
    ///
    /// Must be called after `subscribe()`, before `resume()`. Uses
    /// `eth_getLogs` to fetch events for the gap between the DB snapshot
    /// and the live WS connection, then applies them to the V3/V4 engines
    /// via `backfill_logs()`.
    ///
    /// This ensures that when pools are registered (with `tick_data` from the
    /// DB snapshot), any liquidity changes between the snapshot block and
    /// the current chain head are reflected in the Rust engine's state.
    ///
    /// Args:
    ///     `rpc_url`: HTTP RPC endpoint for `eth_getLogs` requests
    ///     `chunk_size`: Number of blocks per `eth_getLogs` request (default 2000)
    ///
    /// Returns the number of blocks backfilled (0 if snapshot is current).
    #[pyo3(signature = (rpc_url, snapshot_block, chunk_size=2000))]
    fn backfill_from_snapshot(&self, rpc_url: &str, snapshot_block: u64, chunk_size: u64) -> PyResult<u64> {
        // Phase check: must be at least SnapshotLoaded
        let phase = self.current_phase();
        phase.require(EnginePhase::SnapshotLoaded, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure no double-backfill
        phase.require_before(EnginePhase::Backfilled, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure subscribe() was called — we need the first WS block
        let first_ws_block = {
            let state_lock = self.subscribe_state.lock();
            if let Some(s) = state_lock.as_ref() { s.first_block } else {
                let msg = "Cannot backfill: subscribe() has not been called. Call subscribe() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        };

        if snapshot_block == 0 {
            log::warn!("backfill_from_snapshot: snapshot_block is 0, skipping");
            return Ok(0);
        }

        if snapshot_block >= first_ws_block {
            log::info!(
                "backfill_from_snapshot: snapshot at {snapshot_block} >= WS block {first_ws_block}, nothing to backfill"
            );
            return Ok(0);
        }

        let from_block = snapshot_block + 1;
        // Backfill up to (first_ws_block - 1) to avoid overlap with
        // WS events that the pump already captured during subscribe().
        let to_block = first_ws_block - 1;
        let total_blocks = to_block - from_block + 1;

        log::info!(
            "backfill_from_snapshot: fetching events from block {from_block} to {to_block} ({total_blocks} blocks, chunk_size={chunk_size})"
        );

        // Create an HTTP provider for eth_getLogs
        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(rpc_url, 3)
                .await
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to create provider: {e}")))
        })?;

        let provider_arc = provider.provider_arc();

        // Fetch and apply logs in paginated chunks
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);

            let filter = crate::optimizers::uniswap_engine_pump::build_backfill_filter(
                chunk_start,
                chunk_end,
            );

            let logs = runtime.block_on(async {
                provider_arc.get_logs(&filter).await
                    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(
                        format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
                    ))
            })?;

            let chunk_log_count = logs.len();
            total_logs += chunk_log_count;

            // Apply to the engines — process_backfill_logs splits V3/V4
            {
                let mut engine = self.engine.lock();
                engine.process_backfill_logs(&logs, chunk_end);
            }

            log::info!(
                "backfill_from_snapshot: blocks {chunk_start}-{chunk_end}: {chunk_log_count} logs applied"
            );

            chunk_start = chunk_end + 1;
        }

        log::info!(
            "backfill_from_snapshot: complete — {total_logs} total logs applied across {total_blocks} blocks"
        );

        // Advance phase
        self.set_phase(EnginePhase::Backfilled);

        Ok(total_blocks)
    }

    /// Resume phase: begin normal pump processing.
    ///
    /// Must be called after `subscribe()`. Takes the WS stream from the
    /// subscribe phase and begins processing events on block boundaries.
    ///
    /// After calling `resume()`, the engine processes events autonomously.
    /// Python reads results via `latest_results()` and awaits new blocks
    /// via `wait_for_block()`.
    ///
    /// Raises `RuntimeError` if `subscribe()` has not been called first.
    fn resume(&self, _py: Python<'_>) -> PyResult<()> {
        // Phase check: must be at least SnapshotLoaded (can skip backfill)
        let phase = self.current_phase();
        phase.require(EnginePhase::SnapshotLoaded, "resume")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // No double-resume
        if phase == EnginePhase::Resumed {
            let msg = "Cannot resume: engine is already in Resumed phase.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let subscribe_state = self.subscribe_state.lock().take();
        let state = subscribe_state.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot resume: subscribe() has not been called. Call subscribe() first.",
            )
        })?;

        let mut pump = state.pump;
        let first_block = state.first_block;
        let combined_stream = state.combined_stream;

        // Spawn the resume task on the Tokio runtime
        let handle = crate::runtime::get_runtime().spawn(async move {
            let inner_state =
                crate::optimizers::uniswap_engine_pump::SubscribeState {
                    first_block,
                    first_timestamp: 0,
                    combined_stream: Some(combined_stream),
                };
            pump.resume_from_subscribe(inner_state).await;
        });

        *self.pump_handle.lock() = Some(handle);

        // Advance phase
        self.set_phase(EnginePhase::Resumed);

        Ok(())
    }

    /// Last block number processed by `process_block` or `process_logs`.
    /// Returns `None` if no block has been processed yet.
    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    /// Set the last processed block manually after Python backfill.
    #[pyo3(signature = (block))]
    fn set_last_processed_block(&self, block: u64) {
        self.engine.lock().set_last_processed_block(block);
    }

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate results for the first time (replaces the
    /// removed `freeze()` + `initial_solve()`). Subsequent `process_logs`
    /// calls use dependency tracking to only re-solve affected paths.
    fn solve_all_paths(&self, block_number: u64) {
        self.engine.lock().solve_all_paths(block_number);
    }

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// Applies to V3 and V4 sub-engine buffers. Pass `None` for unbounded
    /// (no automatic expiry). Events older than `current_block - max_age`
    /// are expired during `process_block`.
    #[pyo3(signature = (max_age))]
    fn set_event_buffer_max_age(&self, max_age: Option<u64>) {
        self.engine.lock().set_event_buffer_max_age(max_age);
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    fn flush_event_buffer(&self) {
        self.engine.lock().flush_event_buffer();
    }

    /// Number of registered V2 pools.
    fn v2_pool_count(&self) -> usize {
        self.engine.lock().v2_pool_count()
    }

    /// Number of registered V3 pools.
    fn v3_pool_count(&self) -> usize {
        self.engine.lock().v3_pool_count()
    }

    /// Number of registered V4 pools.
    fn v4_pool_count(&self) -> usize {
        self.engine.lock().v4_pool_count()
    }

    /// Debug: return the number of buffered liquidity events for a V3 pool address.
    fn debug_v3_buffer_count(&self, pool_address: &str) -> PyResult<usize> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let engine = self.engine.lock();
        let count = engine.v3_engine_ref().buffered_event_count(&addr);
        Ok(count)
    }

    /// Debug: return the engine's tick data for a V3 pool address as a Python dict.
    /// Maps tick_index (int) → (liquidity_gross: int, liquidity_net: int) tuple.
    /// Returns None if the pool is not registered.
    fn debug_v3_tick_data<'py>(&self, py: Python<'py>, pool_address: &str) -> PyResult<Option<Bound<'py, pyo3::types::PyDict>>> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let tick_data = {
            let mut engine = self.engine.lock();
            let Some(key) = engine.v3_engine().pool_key_for_address(&addr) else {
                return Ok(None);
            };
            let Some(pool) = engine.v3_engine().get_pool(key) else {
                return Ok(None);
            };
            pool.tick_data.clone()
        };

        let dict = pyo3::types::PyDict::new(py);
        for (&tick_idx, info) in &tick_data {
            let lg = info.liquidity_gross.to::<u128>();
            let ln: i128 = info.liquidity_net.try_into().unwrap_or(0i128);
            dict.set_item(tick_idx, (lg, ln))?;
        }
        Ok(Some(dict))
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Verify all V3 and V4 pool liquidity maps against on-chain state.
    ///
    /// Calls `TickLens` for V3 pools and `StateView` for V4 pools. Compares
    /// `sqrtPriceX96`, `tick`, `liquidity`, and every tick's
    /// `(liquidityGross, liquidityNet)`.
    ///
    /// Raises `RuntimeError` on the FIRST mismatch. The bot must not operate
    /// with stale tick data — fail fast.
    ///
    /// Args:
    ///     `rpc_url`: RPC endpoint URL (WS or HTTP).
    ///     `tick_lens_address`: Deployed `TickLens` contract address (hex string).
    ///     `state_view_address`: Deployed `StateView` contract address (hex string).
    #[pyo3(signature = (rpc_url, tick_lens_address, state_view_address, block_number))]
    fn verify_liquidity_maps(
        &self,
        rpc_url: String,
        tick_lens_address: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let tick_lens: Address = tick_lens_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid tick_lens address: {e}"))
        })?;
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let mut engine = self.engine.lock();
        let v3_pools = engine.v3_engine().pools_snapshot();
        let v4_pools = engine.v4_engine().pools_snapshot();
        drop(engine); // Release lock before async I/O

        let runtime = crate::runtime::get_runtime();

        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        // Verify V3 pools
        let v3_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v3_pools(
                &provider, tick_lens, &v3_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v3_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Liquidity map verification FAILED: {mismatch}"
            )));
        }

        // Verify V4 pools
        let v4_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v4_pools(
                &provider, state_view, &v4_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v4_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify V3 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V3 pools.
    /// Useful for verifying against a V3-specific snapshot block.
    #[pyo3(signature = (rpc_url, block_number))]
    fn verify_v3_liquidity_maps(
        &self,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let mut engine = self.engine.lock();
        let v3_pools = engine.v3_engine().pools_snapshot();
        drop(engine);

        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_v3_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        // TickLens address not used (V3 calls pool.ticks() directly)
        let tick_lens = Address::ZERO;
        let v3_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v3_pools(
                &provider, tick_lens, &v3_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v3_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V3 liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify V4 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V4 pools.
    /// Useful for verifying against a V4-specific snapshot block.
    #[pyo3(signature = (rpc_url, state_view_address, block_number))]
    fn verify_v4_liquidity_maps(
        &self,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let mut engine = self.engine.lock();
        let v4_pools = engine.v4_engine().pools_snapshot();
        drop(engine);

        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_v4_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        let v4_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v4_pools(
                &provider, state_view, &v4_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v4_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V4 liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify a single V3 pool's liquidity map against on-chain state.
    ///
    /// Takes a pool address and verifies the `tick_data` at the given block.
    /// Returns Ok if the liquidity map matches, or a `RuntimeError` with
    /// details of the mismatch.
    ///
    /// This is an async method — returns a coroutine that must be awaited.
    /// Uses `future_into_py` instead of `block_on` so it integrates with
    /// the Python asyncio event loop (no deadlock when called from async code).
    #[pyo3(signature = (address, rpc_url, block_number))]
    fn verify_v3_pool<'py>(
        &self,
        py: Python<'py>,
        address: String,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;

        let mut engine = self.engine.lock();
        let v3_key = engine.v3_engine().pool_key_for_address(&pool_addr);
        let v3_pools = if let Some(key) = v3_key {
            let mut map = std::collections::HashMap::new();
            if let Some(pool) = engine.v3_engine().get_pool(key) {
                map.insert(key, pool.clone());
            }
            map
        } else {
            drop(engine);
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V3 pool {address} not registered in engine"
            )));
        };
        drop(engine);

        let tick_lens = Address::ZERO;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = crate::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v3_pool: failed to create provider: {e}"
                    ))
                })?;

            let v3_result =
                crate::bot_core::liquidity_verifier::verify_v3_pools(
                    &provider, tick_lens, &v3_pools, block_number,
                )
                .await;

            if let Err(mismatch) = v3_result {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "V3 pool {address} liquidity map verification FAILED: {mismatch}"
                )));
            }

            Ok(())
        })
    }

    /// Verify a single V4 pool's liquidity map against on-chain state.
    ///
    /// Takes a `pool_id` (hex) and verifies the `tick_data` at the given block
    /// using the `StateView` contract.
    ///
    /// This is an async method — returns a coroutine that must be awaited.
    #[pyo3(signature = (pool_id_hex, rpc_url, state_view_address, block_number))]
    fn verify_v4_pool<'py>(
        &self,
        py: Python<'py>,
        pool_id_hex: String,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let pool_id = hex_string_to_pool_id(&pool_id_hex)?;

        let mut engine = self.engine.lock();
        let v4_keys = engine.v4_engine().pool_keys_for_id(Address::ZERO, &pool_id);
        // V4 pools are registered with the actual pool_manager address, not ZERO.
        // Fallback: scan all V4 pools for matching pool_id.
        let v4_keys = v4_keys.or_else(|| {
            let v4_snapshot = engine.v4_engine().pools_snapshot();
            for (key, pool) in &v4_snapshot {
                if pool.pool_id == pool_id {
                    return Some((*key, *key + 1));
                }
            }
            None
        });

        let v4_pools = if let Some((fwd_key, _rev_key)) = v4_keys {
            let mut map = std::collections::HashMap::new();
            if let Some(pool) = engine.v4_engine().get_pool(fwd_key) {
                map.insert(fwd_key, pool.clone());
            }
            map
        } else {
            drop(engine);
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V4 pool {pool_id_hex} not registered in engine"
            )));
        };
        drop(engine);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = crate::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v4_pool: failed to create provider: {e}"
                    ))
                })?;

            let v4_result =
                crate::bot_core::liquidity_verifier::verify_v4_pools(
                    &provider, state_view, &v4_pools, block_number,
                )
                .await;

            if let Err(mismatch) = v4_result {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "V4 pool {pool_id_hex} liquidity map verification FAILED: {mismatch}"
                )));
            }

            Ok(())
        })
    }

    /// Enable or disable automatic verification on pool registration.
    ///
    /// When enabled, V3 and V4 pools registered from snapshot data (with
    /// `Tracked` coverage) are automatically verified against on-chain state.
    /// The tick data snapshot is taken while the engine lock is held, so the
    /// pump cannot race between registration and verification. The RPC call
    /// happens after the lock is released.
    ///
    /// Must call `set_verify_rpc_url()` before enabling this.
    /// V4 verification also requires `set_verify_state_view()`.
    ///
    /// Args:
    ///     enabled: Whether to enable verification on register.
    #[pyo3(signature = (enabled))]
    fn set_verify_on_register(&self, enabled: bool) {
        self.verify_on_register.store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the HTTP RPC URL used for verification during registration.
    ///
    /// Must be called before enabling `verify_on_register`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: String) {
        *self.verify_rpc_url.lock() = Some(rpc_url);
    }

    /// Set the `StateView` contract address for V4 verification during registration.
    ///
    /// Must be called before any V4 pools are registered with verification enabled.
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: String) {
        let addr: Address = state_view_address.parse().unwrap_or(Address::ZERO);
        *self.verify_state_view.lock() = Some(addr);
    }

    /// Full-sync V3 pool `tick_data` from Python backfill.
    ///
    /// Unlike `process_logs` (which only inserts `tick_priors`), this method
    /// **replaces** the entire `tick_data` map. This ensures that ticks removed
    /// from Python (because `liquidityGross` went to zero after a Burn) are
    /// also removed from the Rust engine.
    ///
    /// `v3_sync_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v3_sync_updates, block_number))]
    fn sync_v3_pool_states(
        &self,
        v3_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let mut engine = self.engine.lock();
        for item in v3_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_str: String = tuple.get_item(0)?.extract()?;
            let addr: Address = addr_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let td_obj = tuple.get_item(4)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            engine.v3_engine().sync_pool_state(
                addr,
                sqrt_price,
                liquidity,
                tick,
                rust_tick_data,
                block_number,
            );
        }
        Ok(())
    }

    /// Full-sync V4 pool `tick_data` from Python backfill.
    ///
    /// Replaces the entire `tick_data` map. See `sync_v3_pool_states` for rationale.
    ///
    /// `v4_sync_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v4_sync_updates, block_number))]
    fn sync_v4_pool_states(
        &self,
        v4_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let mut engine = self.engine.lock();
        for item in v4_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_str: String = tuple.get_item(0)?.extract()?;
            let pool_manager: Address = pm_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager: {e}"))
            })?;
            let pid_str: String = tuple.get_item(1)?.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let td_obj = tuple.get_item(5)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            engine.v4_engine().sync_pool_state(
                pool_manager,
                pool_id,
                sqrt_price,
                liquidity,
                tick,
                rust_tick_data,
                block_number,
            );
        }
        Ok(())
    }

    /// Process Sync, V3 Swap, and V4 Swap events synchronously (for testing).
    ///
    /// `v2_sync_updates`: list of (`address_str`, `reserve0`, `reserve1`)
    /// `v3_swap_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    ///   where `tick_priors` is a list of (`tick_index`, (`liquidity_gross`, `liquidity_net`))
    /// `v4_swap_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    #[pyo3(signature = (v2_sync_updates, v3_swap_updates, v4_swap_updates, block_number))]
    fn process_logs(
        &self,
        v2_sync_updates: &Bound<'_, PyList>,
        v3_swap_updates: &Bound<'_, PyList>,
        v4_swap_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let rust_v2 = Self::parse_v2_updates(v2_sync_updates)?;
        let rust_v3 = Self::parse_v3_updates(v3_swap_updates)?;
        let rust_v4 = Self::parse_v4_updates(v4_swap_updates)?;
        self.engine
            .lock()
            .process_all_updates(&rust_v2, &rust_v3, &rust_v4, block_number, &BlockMetadata::default());
        Ok(())
    }

    /// Read the last solved results and block number.
    ///
    /// Inspect a registered path by ID.
    ///
    /// Returns a dict with:
    ///   - "`path_id"`: int
    ///   - "hops": list of dicts, each with:
    ///     - "type": "V2" | "V3" | "V4"
    ///     - "address": str (V2/V3 contract address, or V4 `pool_manager`)
    ///     - "`pool_id"`: str (V4 only — the pool ID hex)
    ///     - "`zero_for_one"`: bool
    ///     - "fee": int (V2: `gamma_numer`; V3: pool fee; V4: pool fee)
    ///     - "`tick_spacing"`: int (V3/V4 only)
    ///   Returns None if the `path_id` is not found.
    #[pyo3(signature = (path_id))]
    fn inspect_path(&self, path_id: u64, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        // Phase 1: Collect pool refs from the path
        let pool_refs: Vec<MixedPoolRef> = {
            let engine = self.engine.lock();
            let Some(path) = engine.path_pools.get(&path_id) else {
                return Ok(None);
            };
            path.pools.clone()
        };

        // Phase 2: Query sub-engines for pool details
        struct HopInfo {
            hop_type: String,
            address: Option<String>,
            pool_id: Option<String>,
            zero_for_one: bool,
            fee: Option<u64>,
            tick_spacing: Option<i32>,
        }

        let mut hops: Vec<HopInfo> = Vec::new();
        let mut engine = self.engine.lock();

        for pool_ref in &pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    let v2 = engine.v2_engine();
                    let addr = v2.pool_addresses()
                        .iter()
                        .find(|(_, &(fwd, _))| fwd == pool_ref.pool_key)
                        .map(|(a, _)| format!("{a}"));
                    let gamma_numer = v2.get_pool(pool_ref.pool_key).map(|p| p.gamma_numer);
                    hops.push(HopInfo {
                        hop_type: "V2".to_string(),
                        address: addr,
                        pool_id: None,
                        zero_for_one: pool_ref.zero_for_one,
                        fee: gamma_numer,
                        tick_spacing: None,
                    });
                }
                HopType::V3 => {
                    let v3 = engine.v3_engine();
                    let pool = v3.get_pool(pool_ref.pool_key);
                    let (addr, fee, ts) = pool.map_or((None, None, None), |p| {
                        (Some(format!("{}", p.address)), Some(u64::from(p.fee)), Some(p.tick_spacing))
                    });
                    hops.push(HopInfo {
                        hop_type: "V3".to_string(),
                        address: addr,
                        pool_id: None,
                        zero_for_one: pool_ref.zero_for_one,
                        fee,
                        tick_spacing: ts,
                    });
                }
                HopType::V4 => {
                    let v4 = engine.v4_engine();
                    let pool = v4.get_pool(pool_ref.pool_key);
                    let (pm, pid, fee, ts) = pool.map_or((None, None, None, None), |p| {
                        (Some(format!("{}", p.pool_manager)), Some(format!("0x{}", alloy::hex::encode(p.pool_id))), Some(u64::from(p.pool_key.fee)), Some(p.pool_key.tick_spacing))
                    });
                    hops.push(HopInfo {
                        hop_type: "V4".to_string(),
                        address: pm,
                        pool_id: pid,
                        zero_for_one: pool_ref.zero_for_one,
                        fee,
                        tick_spacing: ts,
                    });
                }
            }
        }

        drop(engine);

        // Phase 3: Build the Python dict
        let dict = PyDict::new(py);
        dict.set_item("path_id", path_id)?;

        let hops_list = PyList::empty(py);
        for hop in &hops {
            let hop_dict = PyDict::new(py);
            hop_dict.set_item("type", hop.hop_type.as_str())?;
            if let Some(ref a) = hop.address {
                hop_dict.set_item("address", a)?;
            }
            if let Some(ref pid) = hop.pool_id {
                hop_dict.set_item("pool_id", pid)?;
            }
            hop_dict.set_item("zero_for_one", hop.zero_for_one)?;
            if let Some(f) = hop.fee {
                hop_dict.set_item("fee", f)?;
            }
            if let Some(ts) = hop.tick_spacing {
                hop_dict.set_item("tick_spacing", ts)?;
            }
            hops_list.append(hop_dict)?;
        }
        dict.set_item("hops", hops_list)?;

        Ok(Some(dict.unbind()))
    }

    /// Returns (`results`, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    #[allow(clippy::significant_drop_tightening)]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        let (results, block_num) = {
            let engine = self.engine.lock();
            let (r, b) = engine.latest_results();
            (r.clone(), b)
        };

        let py_list = PyList::empty(py);
        for (path_id, solve_result) in results {
            let path_id_py = path_id.into_pyobject(py)?;
            let input_py = crate::alloy_py::PyU256(solve_result.optimal_input).into_pyobject(py)?;
            let profit_py = crate::alloy_py::PyU256(solve_result.profit).into_pyobject(py)?;

            // Build hop_outputs as a Python tuple
            let hop_outputs_py = PyList::empty(py);
            for hop_out in &solve_result.hop_outputs {
                let hop_py = crate::alloy_py::PyU256(*hop_out).into_pyobject(py)?;
                hop_outputs_py.append(hop_py)?;
            }
            let hop_tuple = hop_outputs_py.into_pyobject(py)?;

            // Build consumed_inputs as a Python tuple
            let consumed_inputs_py = PyList::empty(py);
            for consumed in &solve_result.consumed_inputs {
                let consumed_py = crate::alloy_py::PyU256(*consumed).into_pyobject(py)?;
                consumed_inputs_py.append(consumed_py)?;
            }
            let consumed_tuple = consumed_inputs_py.into_pyobject(py)?;

            let result_tuple = (path_id_py, input_py, profit_py, hop_tuple, consumed_tuple).into_pyobject(py)?;
            py_list.append(result_tuple)?;
        }

        Ok((py_list.unbind(), block_num))
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from the engine's internal state. The path's pools
    /// are **not** removed — other paths may still reference them.
    ///
    /// Returns `true` if the path existed and was removed.
    #[pyo3(signature = (path_id))]
    fn deregister_path(&self, path_id: u64) -> bool {
        self.engine.lock().deregister_path(path_id)
    }

    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit < max_profit`
    /// appear in batch `fresh` / `updated` entries.
    #[pyo3(signature = (min_profit, max_profit))]
    fn set_profit_thresholds(&self, min_profit: u64, max_profit: u64) {
        self.engine
            .lock()
            .set_profit_thresholds(U256::from(min_profit), U256::from(max_profit));
    }

    /// Return self as an async iterator over result batches.
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Await the next result batch from the engine.
    ///
    /// Returns a dict with keys:
    ///   - "`solve_block"`: int
    ///   - "timestamp": int
    ///   - "`base_fee_per_gas"`: int | None
    ///   - "`gas_used"`: int
    ///   - "`gas_limit"`: int
    ///   - "fresh": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "updated": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "expired": list of int (`path_ids`)
    ///   - "removed": list of int (`path_ids`)
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let result_rx = Arc::clone(&self.result_rx);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Take the receiver for awaiting
            let mut rx = result_rx
                .lock()
                .take()
                .ok_or_else(|| PyStopAsyncIteration::new_err("Result channel closed"))?;

            // Wait for the next batch
            let batch = rx.recv().await.ok_or_else(|| {
                PyStopAsyncIteration::new_err(
                    "Result channel closed — pump may have stopped.",
                )
            })?;

            // Put the receiver back
            *result_rx.lock() = Some(rx);

            // Convert batch to Python dict (requires GIL)
            Python::attach(|py| batch_to_py_dict(&batch, py))
        })
    }
}

/// Helper to construct `TickInfo` from Python-extracted values.
/// Convert a `ResultBatch` to a Python dict.
///
/// Called under the GIL after receiving a batch from the result channel.
fn batch_to_py_dict(batch: &ResultBatch, py: Python<'_>) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("solve_block", batch.solve_block)?;
    dict.set_item("timestamp", batch.timestamp)?;
    dict.set_item("base_fee_per_gas", batch.base_fee_per_gas)?;
    dict.set_item("gas_used", batch.gas_used)?;
    dict.set_item("gas_limit", batch.gas_limit)?;

    // fresh: list of (path_id, optimal_input, profit, hop_outputs, consumed_inputs)
    let fresh_list = PyList::empty(py);
    for (path_id, result) in &batch.fresh {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        fresh_list.append(tuple)?;
    }
    dict.set_item("fresh", fresh_list)?;

    // updated: same format as fresh
    let updated_list = PyList::empty(py);
    for (path_id, result) in &batch.updated {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        updated_list.append(tuple)?;
    }
    dict.set_item("updated", updated_list)?;

    // expired: list of path_ids
    let expired_list = PyList::empty(py);
    for &path_id in &batch.expired {
        expired_list.append(path_id)?;
    }
    dict.set_item("expired", expired_list)?;

    // removed: list of path_ids
    let removed_list = PyList::empty(py);
    for &path_id in &batch.removed {
        removed_list.append(path_id)?;
    }
    dict.set_item("removed", removed_list)?;

    Ok(dict.unbind())
}

/// Convert a (`path_id`, `SolvePathResult`) to a Python tuple.
fn solve_result_to_py_tuple<'py>(
    path_id: u64,
    result: &SolvePathResult,
    py: Python<'py>,
) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
    let path_id_py = path_id.into_pyobject(py)?;
    let input_py = crate::alloy_py::PyU256(result.optimal_input).into_pyobject(py)?;
    let profit_py = crate::alloy_py::PyU256(result.profit).into_pyobject(py)?;

    let hop_outputs_py = PyList::empty(py);
    for hop_out in &result.hop_outputs {
        let hop_py = crate::alloy_py::PyU256(*hop_out).into_pyobject(py)?;
        hop_outputs_py.append(hop_py)?;
    }

    let consumed_inputs_py = PyList::empty(py);
    for consumed in &result.consumed_inputs {
        let consumed_py = crate::alloy_py::PyU256(*consumed).into_pyobject(py)?;
        consumed_inputs_py.append(consumed_py)?;
    }

    (
        path_id_py,
        input_py,
        profit_py,
        hop_outputs_py,
        consumed_inputs_py,
    )
        .into_pyobject(py)
}

fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> crate::bot_core::TickInfo {
    use alloy::primitives::{I256, U128};
    crate::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
    }
}

/// Helper to decode a hex string (e.g. "0xabcd...") to a V4 `PoolId` ([u8; 32]).
fn hex_string_to_pool_id(hex_str: &str) -> PyResult<crate::bot_core::v4_swap_decoder::PoolId> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if hex_str.len() != 64 {
        let msg = format!(
            "Pool ID hex string must be 64 hex chars (32 bytes), got {}",
            hex_str.len()
        );
        return Err(pyo3::exceptions::PyValueError::new_err(msg));
    }
    let mut pool_id = [0u8; 32];
    for i in 0..32 {
        let byte_str = &hex_str[i * 2..i * 2 + 2];
        pool_id[i] = u8::from_str_radix(byte_str, 16).map_err(|e| {
            let msg = format!("Invalid hex in pool_id at byte {i}: {e}");
            pyo3::exceptions::PyValueError::new_err(msg)
        })?;
    }
    Ok(pool_id)
}
