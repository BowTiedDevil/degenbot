//! V4 Block Engine — Rust-centric arbitrage engine for Uniswap V4 paths.
//!
//! Owns the per-block lifecycle for V4 pools: Swap event decoding from
//! PoolManager, pool state updates (including tick-level changes), tick-range
//! computation, and solver dispatch.
//!
//! # Design
//!
//! V4 pools share identical concentrated-liquidity math with V3 (same tick
//! structure, same sqrtPriceX96, same liquidity tracking). The V4BlockEngine
//! mirrors [`V3BlockEngine`]'s structure but identifies pools by
//! `(pool_manager, pool_id)` instead of just contract address.
//!
//! The engine constructs [`IntV3TickRangeSequence`] objects (same type as V3)
//! — the solver can't distinguish V3 from V4 hops, and shouldn't need to.
//!
//! # Hook Filtering
//!
//! V4 pools with amount-modifying hook flags are rejected at registration.
//! The four hook flags that modify swap amounts:
//! - `BEFORE_SWAP` (1<<7 = 0x80)
//! - `AFTER_SWAP` (1<<6 = 0x40)
//! - `BEFORE_SWAP_RETURNS_DELTA` (1<<3 = 0x08)
//! - `AFTER_SWAP_RETURNS_DELTA` (1<<2 = 0x04)
//!
//! Bitmask: `0x80 | 0x40 | 0x08 | 0x04 = 0xCC`
//!
//! Pools with any amount-modifying hook set are excluded from arbitrage
//! because the solver assumes standard V3 math — hooked pools can produce
//! arbitrary deltas that violate this assumption.
//!
//! # Dynamic Fee Exclusion
//!
//! V4 pools with `fee == 0x100000` (dynamic fee flag) have swap fees that
//! change between blocks. The solver assumes a fixed fee per pool, so dynamic-
//! fee pools are excluded at registration time.

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, I256, U128, U256};

use crate::bot_core::tick_bitmap::compute_tick_ranges;
use crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
use crate::bot_core::v4_swap_decoder::{decode_v4_swap_log, PoolId};
use crate::bot_core::TickInfo;
use crate::optimizers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};

// ---------------------------------------------------------------------------
// Hook filtering constants
// ---------------------------------------------------------------------------

/// Bitmask for the four hook flags that modify swap amounts:
/// BEFORE_SWAP | AFTER_SWAP | BEFORE_SWAP_RETURNS_DELTA | AFTER_SWAP_RETURNS_DELTA
///
/// A pool with `(hook_flags & AMOUNT_MODIFYING_HOOK_MASK) != 0` is excluded.
const AMOUNT_MODIFYING_HOOK_MASK: u16 = 0x80 | 0x40 | 0x08 | 0x04; // = 0xCC

/// V4 dynamic fee flag. Pools with this fee value have fees that change
/// at runtime and cannot be used with the fixed-fee solver.
const V4_DYNAMIC_FEE_FLAG: u32 = 0x100000;

// ---------------------------------------------------------------------------
// V4 PoolKey
// ---------------------------------------------------------------------------

/// V4 PoolKey — identifies a pool within PoolManager.
///
/// Matches the Solidity struct:
/// ```solidity
/// struct PoolKey {
///     Currency currency0;
///     Currency currency1;
///     uint24 fee;
///     int24 tickSpacing;
///     IHooks hooks;
/// }
/// ```
#[derive(Clone, Debug)]
pub struct V4PoolKey {
    pub currency0: Address,
    pub currency1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
}

// ---------------------------------------------------------------------------
// V4 pool state
// ---------------------------------------------------------------------------

/// Parameters for registering a V4 pool with the engine.
#[derive(Clone, Debug)]
pub struct RegisterV4PoolParams {
    /// The PoolManager contract address
    pub pool_manager: Address,
    /// The pool's PoolId (bytes32)
    pub pool_id: PoolId,
    /// The pool's PoolKey
    pub pool_key: V4PoolKey,
    /// Hook flags (bitmask from the hooks contract address)
    pub hook_flags: u16,
    /// Current sqrt price (Q128.96)
    pub sqrt_price_x96: U256,
    /// Current active liquidity
    pub liquidity: u128,
    /// Current tick
    pub tick: i32,
    /// Tick data: {tick_index: (liquidity_gross, liquidity_net)}
    pub tick_data: HashMap<i32, TickInfo>,
}

/// V4 pool state as owned by the engine.
#[derive(Clone, Debug)]
pub struct V4PoolState {
    pub pool_manager: Address,
    pub pool_id: PoolId,
    pub pool_key: V4PoolKey,

    // Mutable state
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub update_block: u64,

    // Tick data
    pub tick_data: HashMap<i32, TickInfo>,
}

impl V4PoolState {
    /// Build an integer V3-style tick range sequence for the V4 pool.
    ///
    /// This is identical to [`V3PoolState::build_int_v3_sequence`] because
    /// V4 uses the same concentrated-liquidity math. The type is named
    /// `IntV3TickRangeSequence` but applies equally to V4 pools.
    ///
    /// Returns `None` if insufficient tick data.
    #[must_use]
    pub fn build_int_v4_sequence(
        &self,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let (ranges, _current_idx) = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            self.pool_key.tick_spacing,
            self.liquidity,
            zero_for_one,
            max_ranges,
        )?;

        let gamma_numer = u64::from(1_000_000 - self.pool_key.fee);
        let fee_denom = 1_000_000u64;

        let mut int_ranges = Vec::with_capacity(ranges.len());
        for (i, r) in ranges.iter().enumerate() {
            // Current sqrt price for this range
            let sqrt_price_x96 = if i == 0 {
                self.sqrt_price_x96
            } else if zero_for_one {
                // Walking down: entered from the upper boundary of prior range
                ranges[i - 1].sqrt_price_upper
            } else {
                // Walking up: entered from the lower boundary of prior range
                ranges[i - 1].sqrt_price_lower
            };

            // Compute the active liquidity for this range
            let range_liquidity = if i == 0 {
                self.liquidity
            } else {
                let mut l = self.liquidity.cast_signed();
                for prev_range in &ranges[..i] {
                    let net = prev_range.liquidity_net;
                    if zero_for_one {
                        l -= net;
                    } else {
                        l += net;
                    }
                }
                if l.is_negative() {
                    0u128
                } else {
                    #[allow(clippy::cast_sign_loss)]
                    {
                        l as u128
                    }
                }
            };

            int_ranges.push(IntV3TickRangeHop {
                liquidity: range_liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: r.sqrt_price_lower,
                sqrt_price_upper_x96: r.sqrt_price_upper,
                gamma_numer,
                fee_denom,
                zero_for_one,
            });
        }

        IntV3TickRangeSequence::new(int_ranges).ok()
    }
}

impl From<RegisterV4PoolParams> for V4PoolState {
    fn from(params: RegisterV4PoolParams) -> Self {
        Self {
            pool_manager: params.pool_manager,
            pool_id: params.pool_id,
            pool_key: params.pool_key,
            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: 0,
            tick_data: params.tick_data,
        }
    }
}

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// A pool reference in a V4 path: (pool_key index, zero_for_one direction).
#[derive(Clone, Debug)]
pub struct V4PoolRef {
    /// Index into the engine's `pools` map.
    pub pool_idx: u64,
    /// Direction for this hop.
    pub zero_for_one: bool,
}

/// A registered V4 arbitrage path.
#[derive(Clone, Debug)]
struct V4Path {
    pools: Vec<V4PoolRef>,
}

/// Resolved state for a V4 path, ready for solving.
#[derive(Clone, Debug, Default)]
struct ResolvedV4Path {
    /// Integer V3-compatible tick-range sequences for V4 hops.
    int_v4_sequences: Vec<Option<IntV3TickRangeSequence>>,
    /// Whether this path is valid for solving.
    valid: bool,
}

// ---------------------------------------------------------------------------
// V4BlockEngine
// ---------------------------------------------------------------------------

/// The V4 block engine — owns V4 pool state, constructs tick-range sequences,
/// and solves arbitrage paths. Mirrors [`V3BlockEngine`] but uses PoolId
/// for pool identification instead of contract address.
pub struct V4BlockEngine {
    /// V4 pool state: auto-incrementing key → state
    pools: HashMap<u64, V4PoolState>,
    /// (pool_manager, pool_id) → (forward_key, reverse_key)
    pool_ids: HashMap<(Address, PoolId), (u64, u64)>,
    /// Registered paths: path_id → (V4Path, ResolvedV4Path)
    paths: HashMap<u64, (V4Path, ResolvedV4Path)>,
    /// Last solved results: (path_id, optimal_input, profit)
    results: Vec<(u64, U256, U256)>,
    /// Block number for the last solved results
    results_block: u64,
    /// Whether the engine is running (freezes registration after start)
    running: bool,
    /// Auto-incrementing path ID
    next_path_id: u64,
    /// Auto-incrementing pool key
    next_pool_key: u64,
}

impl V4BlockEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            pool_ids: HashMap::new(),
            paths: HashMap::new(),
            results: Vec::new(),
            results_block: 0,
            running: false,
            next_path_id: 1,
            next_pool_key: 1,
        }
    }

    /// Register a V4 pool with the engine.
    ///
    /// Creates entries in both orientations (forward and reverse), matching
    /// the V2 engine's dual-orientation pattern.
    ///
    /// Returns the forward pool key. The reverse key is `forward_key + 1`.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a description if:
    /// - The pool has amount-modifying hooks (hook_flags & 0xCC != 0)
    /// - The pool has dynamic fees (fee == 0x100000)
    /// - Registration is attempted after `start()`
    ///
    /// # Panics
    ///
    /// Panics if called after `start()`.
    pub fn register_pool(&mut self, params: RegisterV4PoolParams) -> Result<u64, String> {
        assert!(!self.running, "cannot register pools after start()");

        // Hook filtering: reject pools with amount-modifying hooks
        if (params.hook_flags & AMOUNT_MODIFYING_HOOK_MASK) != 0 {
            let msg = format!(
                "V4 pool has amount-modifying hooks (flags=0x{:04X}, mask=0x{:04X}) — excluded from arbitrage",
                params.hook_flags, AMOUNT_MODIFYING_HOOK_MASK
            );
            return Err(msg);
        }

        // Dynamic fee filtering: reject pools with dynamic fees
        if params.pool_key.fee == V4_DYNAMIC_FEE_FLAG {
            let msg = format!(
                "V4 pool has dynamic fee (fee=0x{:06X}) — excluded from arbitrage",
                V4_DYNAMIC_FEE_FLAG
            );
            return Err(msg);
        }

        let forward_key = self.next_pool_key;
        let reverse_key = self.next_pool_key + 1;
        self.next_pool_key += 2;

        let pool_manager = params.pool_manager;
        let pool_id = params.pool_id;

        // Forward state: original orientation
        self.pools.insert(forward_key, V4PoolState::from(params.clone()));

        // Reverse state: swap tokens in PoolKey, flip direction
        let mut reverse_params = params;
        // Swap currency0 and currency1 in the reverse PoolKey
        std::mem::swap(&mut reverse_params.pool_key.currency0, &mut reverse_params.pool_key.currency1);
        self.pools.insert(reverse_key, V4PoolState::from(reverse_params));

        self.pool_ids.insert((pool_manager, pool_id), (forward_key, reverse_key));

        Ok(forward_key)
    }

    /// Register a V4 arbitrage path as an ordered list of V4PoolRefs.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics if called after `start()` or with fewer than 2 pool refs.
    pub fn register_path(&mut self, pool_refs: Vec<V4PoolRef>) -> u64 {
        assert!(!self.running, "cannot register paths after start()");
        assert!(pool_refs.len() >= 2, "need at least 2 pool refs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedV4Path::default();
        self.resolve_path(&pool_refs, &mut resolved);

        self.paths
            .insert(path_id, (V4Path { pools: pool_refs }, resolved));
        path_id
    }

    /// Apply a V4 Swap update to a registered pool.
    ///
    /// Updates the scalar fields (sqrt_price, liquidity, tick) and any
    /// tick-level priors. Both orientations are updated.
    pub fn apply_swap(
        &mut self,
        pool_manager: Address,
        pool_id: PoolId,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        let Some(&(fwd_key, rev_key)) = self.pool_ids.get(&(pool_manager, pool_id)) else {
            return;
        };

        // Apply to forward pool
        if let Some(pool) = self.pools.get_mut(&fwd_key) {
            for &(tick_index, ref prior) in tick_priors {
                pool.tick_data.insert(tick_index, prior.clone());
            }
            pool.sqrt_price_x96 = sqrt_price_x96;
            pool.liquidity = liquidity;
            pool.tick = tick;
            pool.update_block = block_number;
        }

        // Apply to reverse pool (same state, different PoolKey orientation)
        if let Some(pool) = self.pools.get_mut(&rev_key) {
            for &(tick_index, ref prior) in tick_priors {
                pool.tick_data.insert(tick_index, prior.clone());
            }
            pool.sqrt_price_x96 = sqrt_price_x96;
            pool.liquidity = liquidity;
            pool.tick = tick;
            pool.update_block = block_number;
        }
    }

    /// Apply a liquidity update (V4 ModifyLiquidity event) to a registered pool.
    ///
    /// Updates `tick_data` at `tick_lower` and `tick_upper` using the same
    /// logic as V3's `Tick.update()`: both ticks get `liquidity_gross += delta`,
    /// while `liquidity_net += delta` for the lower tick and
    /// `liquidity_net -= delta` for the upper tick.
    ///
    /// If a tick does not yet exist in `tick_data`, it is inserted.
    /// Ticks with zero `liquidity_gross` after the update are removed.
    pub fn apply_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
        block_number: u64,
    ) {
        let Some(&(fwd_key, rev_key)) = self.pool_ids.get(&(pool_manager, pool_id)) else {
            return;
        };

        // Convert I256 liquidity_delta to i128 for tick update
        // If it doesn't fit, skip (shouldn't happen with valid on-chain data)
        let delta_i128: i128 = match liquidity_delta.try_into() {
            Ok(v) => v,
            Err(_) => return,
        };

        // Apply to forward pool
        if let Some(pool) = self.pools.get_mut(&fwd_key) {
            update_tick_liquidity(&mut pool.tick_data, tick_lower, delta_i128, true);
            update_tick_liquidity(&mut pool.tick_data, tick_upper, delta_i128, false);
            pool.tick_data.retain(|_, info| !info.liquidity_gross.is_zero());
            pool.update_block = block_number;
        }

        // Apply to reverse pool
        if let Some(pool) = self.pools.get_mut(&rev_key) {
            update_tick_liquidity(&mut pool.tick_data, tick_lower, delta_i128, true);
            update_tick_liquidity(&mut pool.tick_data, tick_upper, delta_i128, false);
            pool.tick_data.retain(|_, info| !info.liquidity_gross.is_zero());
            pool.update_block = block_number;
        }
    }

    /// Process a block: decode Swap and ModifyLiquidity events, apply updates,
    /// rebuild paths, solve all, and store results.
    ///
    /// Only processes logs from registered pool managers. Logs from other
    /// addresses are ignored.
    pub fn process_block(&mut self, logs: &[alloy::rpc::types::Log], block_number: u64) {
        for log in logs {
            if let Some(event) = decode_v4_swap_log(log) {
                self.apply_swap(
                    log.address(),
                    event.pool_id,
                    event.sqrt_price_x96,
                    event.liquidity.to::<u128>(),
                    event.tick,
                    block_number,
                    &[],
                );
            } else if let Some(event) = decode_v4_modify_liquidity_log(log) {
                self.apply_liquidity_update(
                    log.address(),
                    event.pool_id,
                    event.tick_lower,
                    event.tick_upper,
                    event.liquidity_delta,
                    block_number,
                );
            }
        }

        self.rebuild_and_solve(block_number);
    }

    /// Apply Swap updates and return the set of pool keys that changed.
    /// Does NOT rebuild paths or solve — caller handles that.
    pub fn apply_swap_updates(
        &mut self,
        updates: &[V4SwapUpdate],
        block_number: u64,
    ) -> HashSet<u64> {
        let mut affected = HashSet::new();
        for update in updates {
            let Some(&(fwd_key, rev_key)) = self.pool_ids.get(&(update.pool_manager, update.pool_id)) else {
                continue;
            };
            self.apply_swap(
                update.pool_manager,
                update.pool_id,
                update.sqrt_price_x96,
                update.liquidity,
                update.tick,
                block_number,
                &update.tick_priors,
            );
            affected.insert(fwd_key);
            affected.insert(rev_key);
        }
        affected
    }

    /// Look up the pool keys (forward + reverse) for a registered
    /// (pool_manager, pool_id) pair.
    #[must_use]
    pub fn pool_keys_for_id(&self, pool_manager: Address, pool_id: &PoolId) -> Option<(u64, u64)> {
        self.pool_ids.get(&(pool_manager, *pool_id)).copied()
    }

    /// Get a reference to a V4 pool state by pool key.
    #[must_use]
    pub fn get_pool(&self, pool_key: u64) -> Option<&V4PoolState> {
        self.pools.get(&pool_key)
    }

    /// Re-resolve and re-solve only paths that contain updated pools.
    pub fn rebuild_and_solve_affected(
        &mut self,
        affected: &HashSet<u64>,
        block_number: u64,
    ) {
        // Collect affected path IDs from pool dependency tracking
        let mut affected_path_ids: HashSet<u64> = HashSet::new();
        for (&path_id, (path, _)) in &self.paths {
            for pool_ref in &path.pools {
                if affected.contains(&pool_ref.pool_idx) {
                    affected_path_ids.insert(path_id);
                    break;
                }
            }
        }

        if affected_path_ids.is_empty() {
            self.results_block = block_number;
            return;
        }

        // Re-resolve affected paths
        for &path_id in &affected_path_ids {
            let Some((path, _)) = self.paths.get(&path_id) else {
                continue;
            };
            let pool_refs = path.pools.clone();
            let mut resolved = ResolvedV4Path::default();
            self.resolve_path(&pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(&path_id) {
                *stored = resolved;
            }
        }

        // Re-solve affected paths and merge with unchanged results
        let mut new_results: Vec<(u64, U256, U256)> = Vec::with_capacity(self.paths.len());

        // Carry forward unchanged results
        for &(path_id, ref input, ref profit) in &self.results {
            if !affected_path_ids.contains(&path_id) {
                new_results.push((path_id, *input, *profit));
            }
        }

        // Solve affected paths
        for &path_id in &affected_path_ids {
            let Some((_path, resolved)) = self.paths.get(&path_id) else {
                continue;
            };
            if !resolved.valid {
                continue;
            }

            // V4-V4: use int_solve_v3_v3 (same CL math)
            let int_sequences: Vec<&IntV3TickRangeSequence> = resolved
                .int_v4_sequences
                .iter()
                .filter_map(Option::as_ref)
                .collect();

            if int_sequences.len() == 2 {
                if let Some((opt_input, profit)) =
                    crate::optimizers::mobius_v3_int::int_solve_v3_v3(
                        int_sequences[0],
                        int_sequences[1],
                    )
                {
                    if !opt_input.is_zero() && !profit.is_zero() {
                        new_results.push((path_id, opt_input, profit));
                    }
                }
            }
        }

        new_results.sort_unstable_by_key(|(path_id, _, _)| *path_id);
        self.results = new_results;
        self.results_block = block_number;
    }

    /// Perform initial solve of ALL paths.
    pub fn initial_solve(&mut self, block_number: u64) {
        let path_pool_refs: Vec<(u64, Vec<V4PoolRef>)> = self
            .paths
            .iter()
            .map(|(&id, (path, _))| (id, path.pools.clone()))
            .collect();

        for (path_id, pool_refs) in &path_pool_refs {
            let mut resolved = ResolvedV4Path::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        self.results = self.solve_all();
        self.results_block = block_number;
    }

    /// Solve all registered V4 paths.
    #[must_use]
    pub fn solve_all(&self) -> Vec<(u64, U256, U256)> {
        let mut results = Vec::with_capacity(self.paths.len());

        for (&path_id, (_path, resolved)) in &self.paths {
            if !resolved.valid {
                continue;
            }

            // V4-V4: use int_solve_v3_v3 (same concentrated-liquidity math)
            let int_sequences: Vec<&IntV3TickRangeSequence> = resolved
                .int_v4_sequences
                .iter()
                .filter_map(Option::as_ref)
                .collect();

            if int_sequences.len() == 2 {
                if let Some((opt_input, profit)) =
                    crate::optimizers::mobius_v3_int::int_solve_v3_v3(
                        int_sequences[0],
                        int_sequences[1],
                    )
                {
                    if !opt_input.is_zero() && !profit.is_zero() {
                        results.push((path_id, opt_input, profit));
                    }
                }
            }
        }

        results
    }

    /// Read the last solved results and block number.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn latest_results(&self) -> (&Vec<(u64, U256, U256)>, u64) {
        (&self.results, self.results_block)
    }

    /// Mark the engine as running. Freezes registration.
    pub const fn start(&mut self) {
        self.running = true;
    }

    /// Whether the engine is running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// Return the set of PoolManager addresses with registered pools.
    /// Used by the V4 pump to build the log filter.
    #[must_use]
    pub fn registered_pool_managers(&self) -> Vec<Address> {
        self.pool_ids
            .keys()
            .map(|(pm, _)| *pm)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    /// Rebuild all paths and solve. Convenience method for `process_block`
    /// and initial setup.
    pub fn rebuild_and_solve(&mut self, block_number: u64) {
        // Re-resolve all paths
        let path_pool_refs: Vec<(u64, Vec<V4PoolRef>)> = self
            .paths
            .iter()
            .map(|(&id, (path, _))| (id, path.pools.clone()))
            .collect();

        for (path_id, pool_refs) in &path_pool_refs {
            let mut resolved = ResolvedV4Path::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        // Re-solve all paths
        self.results = self.solve_all();
        self.results_block = block_number;
    }

    /// Number of registered pools (counting PoolId entries, not orientations).
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pool_ids.len()
    }

    /// Number of registered paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Resolve a path's pool refs into tick-range sequences.
    fn resolve_path(&self, pool_refs: &[V4PoolRef], resolved: &mut ResolvedV4Path) {
        resolved.int_v4_sequences.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.int_v4_sequences.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            let Some(pool) = self.pools.get(&pool_ref.pool_idx) else {
                return; // Missing pool → invalid
            };

            let sequence = pool.build_int_v4_sequence(pool_ref.zero_for_one, 10);
            resolved.int_v4_sequences.push(sequence);
        }

        // All sequences must be present
        if resolved.int_v4_sequences.iter().all(Option::is_some) {
            resolved.valid = true;
        }
    }
}

impl Default for V4BlockEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tick liquidity update function (mirrors V3BlockEngine)
// ---------------------------------------------------------------------------

/// Update a single tick's `liquidity_gross` and `liquidity_net` in-place.
///
/// Matches V3's Solidity `Tick.update()`:
/// - Both lower and upper tick receive `liquidity_gross += delta`
/// - `liquidity_net += delta` for the lower tick
/// - `liquidity_net -= delta` for the upper tick (controlled by `is_lower_tick`)
///
/// If a tick doesn't exist yet, it's inserted with appropriate initial values.
/// Ticks with `liquidity_gross == 0` after the update should be removed by the caller.
fn update_tick_liquidity(
    tick_data: &mut HashMap<i32, TickInfo>,
    tick: i32,
    delta: i128,
    is_lower_tick: bool,
) {
    let entry = tick_data.entry(tick).or_insert(TickInfo {
        liquidity_gross: U128::ZERO,
        liquidity_net: I256::ZERO,
    });

    // Update liquidity_gross: += delta (always the same direction for both ticks)
    let current_gross = entry.liquidity_gross.to::<u128>();
    let new_gross_i128 = current_gross as i128 + delta;
    // liquidity_gross is always >= 0 in valid state; negative means an underflow bug
    let new_gross = if new_gross_i128 < 0 {
        U128::ZERO
    } else {
        U128::from(new_gross_i128 as u128)
    };
    entry.liquidity_gross = new_gross;

    // Update liquidity_net: += delta for lower tick, -= delta for upper tick
    let delta_i256 = I256::try_from(delta).unwrap_or(I256::ZERO);
    let current_net = entry.liquidity_net;
    entry.liquidity_net = if is_lower_tick {
        current_net.checked_add(delta_i256).unwrap_or(I256::ZERO)
    } else {
        current_net.checked_sub(delta_i256).unwrap_or(I256::ZERO)
    };
}

// ---------------------------------------------------------------------------
// Swap update type for testing
// ---------------------------------------------------------------------------

/// A pre-decoded V4 Swap update for testing without log decoding.
#[derive(Clone, Debug)]
pub struct V4SwapUpdate {
    pub pool_manager: Address,
    pub pool_id: PoolId,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_priors: Vec<(i32, TickInfo)>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, U128};

    fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> TickInfo {
        use alloy::primitives::I256;
        TickInfo {
            liquidity_gross: U128::from(liquidity_gross),
            liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
        }
    }

    fn make_register_params(
        pool_manager: Address,
        pool_id: PoolId,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
    ) -> RegisterV4PoolParams {
        RegisterV4PoolParams {
            pool_manager,
            pool_id,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee,
                tick_spacing,
                hooks: if hook_flags != 0 {
                    // Generate a dummy hooks address with the given flags
                    // The hooks address encodes flags in the lower bits
                    let mut addr_bytes = [0u8; 20];
                    addr_bytes[18] = ((hook_flags >> 8) & 0xFF) as u8;
                    addr_bytes[19] = (hook_flags & 0xFF) as u8;
                    Address::from(addr_bytes)
                } else {
                    Address::ZERO
                },
            },
            hook_flags,
            sqrt_price_x96,
            liquidity,
            tick,
            tick_data,
        }
    }

    const POOL_MANAGER: Address = Address::new([0x00u8; 19 + 1]); // 0x000...0 (different from ZERO)

    fn make_pool_id(suffix: u8) -> PoolId {
        let mut id = [0u8; 32];
        id[31] = suffix;
        id
    }

    #[test]
    fn register_v4_pool_creates_both_orientations() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(1);

        let fwd_key = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0, // no hooks
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        )).unwrap();

        assert_eq!(fwd_key, 1);
        let rev_key = fwd_key + 1;
        assert_eq!(rev_key, 2);

        // Both entries exist in pools
        assert!(engine.pools.contains_key(&fwd_key));
        assert!(engine.pools.contains_key(&rev_key));

        // Reverse PoolKey has swapped currency0/currency1
        let fwd_pool = &engine.pools[&fwd_key];
        let rev_pool = &engine.pools[&rev_key];
        assert_eq!(fwd_pool.pool_key.currency0, Address::ZERO);
        assert_eq!(fwd_pool.pool_key.currency1, Address::from([1u8; 20]));
        assert_eq!(rev_pool.pool_key.currency0, Address::from([1u8; 20]));
        assert_eq!(rev_pool.pool_key.currency1, Address::ZERO);
    }

    #[test]
    fn register_v4_pool_rejects_hooked_pools() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(2);

        // BEFORE_SWAP flag
        let result = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0x80, // BEFORE_SWAP
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        ));

        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("amount-modifying hooks"));
    }

    #[test]
    fn register_v4_pool_rejects_dynamic_fee() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(3);

        let result = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            0x100000, // dynamic fee flag
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        ));

        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("dynamic fee"));
    }

    #[test]
    fn register_v4_pool_allows_non_amount_hooks() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(4);

        // BEFORE_DONATE (1<<5 = 0x20) and AFTER_DONATE (1<<4 = 0x10)
        // These don't modify swap amounts
        let result = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0x30, // BEFORE_DONATE | AFTER_DONATE
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        ));

        assert!(result.is_ok());
    }

    #[test]
    fn register_v4_pool_after_start_panics() {
        let mut engine = V4BlockEngine::new();
        engine.start();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = engine.register_pool(make_register_params(
                POOL_MANAGER,
                make_pool_id(5),
                3000,
                60,
                0,
                U256::ONE,
                100,
                0,
                HashMap::new(),
            ));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn apply_swap_updates_both_orientations() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(10);
        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));

        let fwd_key = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        let new_sqrt_price = U256::from(79466191966197645195421774833u128);
        engine.apply_swap(
            POOL_MANAGER,
            pool_id,
            new_sqrt_price,
            900_000,
            60,
            42,
            &[],
        );

        // Forward pool updated
        let fwd_pool = &engine.pools[&fwd_key];
        assert_eq!(fwd_pool.sqrt_price_x96, new_sqrt_price);
        assert_eq!(fwd_pool.liquidity, 900_000);
        assert_eq!(fwd_pool.tick, 60);
        assert_eq!(fwd_pool.update_block, 42);

        // Reverse pool updated
        let rev_key = fwd_key + 1;
        let rev_pool = &engine.pools[&rev_key];
        assert_eq!(rev_pool.sqrt_price_x96, new_sqrt_price);
        assert_eq!(rev_pool.liquidity, 900_000);
        assert_eq!(rev_pool.tick, 60);
        assert_eq!(rev_pool.update_block, 42);
    }

    #[test]
    fn apply_swap_ignores_unregistered_pool() {
        let mut engine = V4BlockEngine::new();
        let unregistered_id = make_pool_id(0xFF);

        engine.apply_swap(
            POOL_MANAGER,
            unregistered_id,
            U256::ONE,
            100,
            0,
            1,
            &[],
        );
        // Should not panic
    }

    #[test]
    fn register_v4_path_and_resolve() {
        let mut engine = V4BlockEngine::new();

        let pool_id_a = make_pool_id(1);
        let pool_id_b = make_pool_id(2);

        let mut tick_data_a = HashMap::new();
        tick_data_a.insert(-60, make_tick_info(200, -100));
        tick_data_a.insert(60, make_tick_info(300, 150));

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(-60, make_tick_info(250, -80));
        tick_data_b.insert(60, make_tick_info(350, 120));

        let key_a = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id_a,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data_a,
        )).unwrap();

        let key_b = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id_b,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            2_000_000,
            0,
            tick_data_b,
        )).unwrap();

        let path_id = engine.register_path(vec![
            V4PoolRef { pool_idx: key_a, zero_for_one: true },
            V4PoolRef { pool_idx: key_b, zero_for_one: false },
        ]);

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.int_v4_sequences.len(), 2);
        assert!(resolved.int_v4_sequences[0].is_some());
        assert!(resolved.int_v4_sequences[1].is_some());
    }

    #[test]
    fn path_with_missing_pool_is_invalid() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(1);

        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));
        tick_data.insert(-60, make_tick_info(150, -50));

        let key = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        // Reference a non-existent pool
        let path_id = engine.register_path(vec![
            V4PoolRef { pool_idx: key, zero_for_one: true },
            V4PoolRef { pool_idx: 999, zero_for_one: false },
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(!resolved.valid, "path with missing pool should be invalid");
    }

    #[test]
    fn rebuild_and_solve_affected_after_swap() {
        let mut engine = V4BlockEngine::new();

        let pool_id_a = make_pool_id(1);
        let pool_id_b = make_pool_id(2);

        let mut tick_data_a = HashMap::new();
        tick_data_a.insert(-60, make_tick_info(500, -200));
        tick_data_a.insert(60, make_tick_info(800, 300));

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(-60, make_tick_info(600, -250));
        tick_data_b.insert(60, make_tick_info(900, 350));

        let key_a = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id_a,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            10_000_000_000_000,
            0,
            tick_data_a,
        )).unwrap();

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id_b,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            20_000_000_000_000,
            0,
            tick_data_b,
        )).unwrap();

        engine.register_path(vec![
            V4PoolRef { pool_idx: key_a, zero_for_one: true },
            V4PoolRef { pool_idx: key_a + 1, zero_for_one: false }, // reverse orientation of same pool would be silly but tests the flow
        ]);

        engine.start();

        // Apply swap update
        let affected = engine.apply_swap_updates(
            &[V4SwapUpdate {
                pool_manager: POOL_MANAGER,
                pool_id: pool_id_a,
                sqrt_price_x96: U256::from(79466191966197645195421774833u128),
                liquidity: 10_000_000_000_000,
                tick: 60,
                tick_priors: vec![],
            }],
            100,
        );

        assert!(!affected.is_empty());

        engine.rebuild_and_solve_affected(&affected, 100);

        let (_, block) = engine.latest_results();
        assert_eq!(block, 100);
    }

    #[test]
    fn initial_solve_populates_results() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(1);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(500, -200));
        tick_data.insert(60, make_tick_info(800, 300));

        let key = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        // Need a second pool to form a path
        let pool_id_b = make_pool_id(2);
        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(-60, make_tick_info(600, -250));
        tick_data_b.insert(60, make_tick_info(900, 350));

        let key_b = engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id_b,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            2_000_000,
            0,
            tick_data_b,
        )).unwrap();

        engine.register_path(vec![
            V4PoolRef { pool_idx: key, zero_for_one: true },
            V4PoolRef { pool_idx: key_b, zero_for_one: false },
        ]);

        engine.start();
        engine.initial_solve(0);

        let (results, block) = engine.latest_results();
        assert_eq!(block, 0);
        // May or may not find profitable results depending on prices
        let _ = results;
    }

    #[test]
    fn register_hook_rejection_combinations() {
        let mut engine = V4BlockEngine::new();

        // Test each amount-modifying flag individually
        let amount_modifying_flags: &[u16] = &[
            0x80, // BEFORE_SWAP
            0x40, // AFTER_SWAP
            0x08, // BEFORE_SWAP_RETURNS_DELTA
            0x04, // AFTER_SWAP_RETURNS_DELTA
            0xCC, // all four combined
            0x84, // BEFORE_SWAP + AFTER_SWAP_RETURNS_DELTA
            0x48, // AFTER_SWAP + BEFORE_SWAP_RETURNS_DELTA
        ];

        for (i, &flags) in amount_modifying_flags.iter().enumerate() {
            let pool_id = make_pool_id(i as u8 + 100);
            let result = engine.register_pool(make_register_params(
                POOL_MANAGER,
                pool_id,
                3000,
                60,
                flags,
                U256::from(79228162514264337593543950336u128),
                1_000_000,
                0,
                HashMap::new(),
            ));
            assert!(result.is_err(), "should reject hook flags 0x{:04X}", flags);
        }

        // Test non-amount-modifying flags (should be accepted)
        let safe_flags: &[u16] = &[
            0x00, // no hooks
            0x20, // BEFORE_DONATE
            0x10, // AFTER_DONATE
            0x2000, // BEFORE_INITIALIZE
            0x1000, // AFTER_INITIALIZE
            0x0800, // BEFORE_ADD_LIQUIDITY
            0x0400, // AFTER_ADD_LIQUIDITY
            0x0200, // BEFORE_REMOVE_LIQUIDITY
            0x0100, // AFTER_REMOVE_LIQUIDITY
            0x0300, // AFTER_ADD_LIQUIDITY_RETURNS_DELTA | AFTER_REMOVE_LIQUIDITY_RETURNS_DELTA
        ];

        for (i, &flags) in safe_flags.iter().enumerate() {
            let pool_id = make_pool_id(i as u8 + 200);
            let result = engine.register_pool(make_register_params(
                POOL_MANAGER,
                pool_id,
                3000,
                60,
                flags,
                U256::from(79228162514264337593543950336u128),
                1_000_000,
                0,
                HashMap::new(),
            ));
            assert!(result.is_ok(), "should accept hook flags 0x{:04X}", flags);
        }
    }

    // ── apply_liquidity_update tests ─────────────────────────────

    #[test]
    fn v4_mint_updates_tick_data() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(1);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(100, 50));
        tick_data.insert(60, make_tick_info(100, -50));

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        // Add 500 liquidity: positive delta
        engine.apply_liquidity_update(
            POOL_MANAGER,
            pool_id,
            -60,
            60,
            I256::try_from(500_i128).unwrap(),
            100,
        );

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool = engine.get_pool(fwd_key).unwrap();

        // tick_lower: gross += 500, net += 500
        let lower_tick = &pool.tick_data[&-60];
        assert_eq!(lower_tick.liquidity_gross, U128::from(600)); // 100 + 500
        assert_eq!(lower_tick.liquidity_net, I256::try_from(550_i128).unwrap()); // 50 + 500

        // tick_upper: gross += 500, net -= 500
        let upper_tick = &pool.tick_data[&60];
        assert_eq!(upper_tick.liquidity_gross, U128::from(600)); // 100 + 500
        assert_eq!(upper_tick.liquidity_net, I256::try_from(-550_i128).unwrap()); // -50 - 500
    }

    #[test]
    fn v4_burn_updates_tick_data() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(2);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(500, 250));
        tick_data.insert(60, make_tick_info(500, -250));

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        // Burn 200 liquidity: negative delta
        engine.apply_liquidity_update(
            POOL_MANAGER,
            pool_id,
            -60,
            60,
            I256::try_from(-200_i128).unwrap(),
            100,
        );

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool = engine.get_pool(fwd_key).unwrap();

        // tick_lower: gross -= 200, net -= 200
        let lower_tick = &pool.tick_data[&-60];
        assert_eq!(lower_tick.liquidity_gross, U128::from(300)); // 500 - 200
        assert_eq!(lower_tick.liquidity_net, I256::try_from(50_i128).unwrap()); // 250 - 200

        // tick_upper: gross -= 200, net += 200
        let upper_tick = &pool.tick_data[&60];
        assert_eq!(upper_tick.liquidity_gross, U128::from(300)); // 500 - 200
        assert_eq!(upper_tick.liquidity_net, I256::try_from(-50_i128).unwrap()); // -250 + 200
    }

    #[test]
    fn v4_full_burn_removes_ticks_with_zero_gross() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(3);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(100, 100));
        tick_data.insert(60, make_tick_info(100, -100));

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        // Burn all 100 liquidity: delta = -100, gross goes to 0 → tick removed
        engine.apply_liquidity_update(
            POOL_MANAGER,
            pool_id,
            -60,
            60,
            I256::try_from(-100_i128).unwrap(),
            100,
        );

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool = engine.get_pool(fwd_key).unwrap();

        // Both ticks should be removed since liquidity_gross is now 0
        assert!(!pool.tick_data.contains_key(&-60));
        assert!(!pool.tick_data.contains_key(&60));
    }

    #[test]
    fn v4_mint_initializes_new_tick() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(4);

        // Register with no tick at -120 or 120
        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        )).unwrap();

        engine.start();

        // Mint adds new ticks at -120 and 120
        engine.apply_liquidity_update(
            POOL_MANAGER,
            pool_id,
            -120,
            120,
            I256::try_from(300_i128).unwrap(),
            100,
        );

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool = engine.get_pool(fwd_key).unwrap();

        // New ticks should be initialized
        let lower_tick = &pool.tick_data[&-120];
        assert_eq!(lower_tick.liquidity_gross, U128::from(300));
        assert_eq!(lower_tick.liquidity_net, I256::try_from(300_i128).unwrap());

        let upper_tick = &pool.tick_data[&120];
        assert_eq!(upper_tick.liquidity_gross, U128::from(300));
        assert_eq!(upper_tick.liquidity_net, I256::try_from(-300_i128).unwrap());
    }

    #[test]
    fn v4_apply_liquidity_update_ignores_unregistered_pool() {
        let mut engine = V4BlockEngine::new();
        let unregistered_id = make_pool_id(0xFF);

        // Should not panic
        engine.apply_liquidity_update(
            POOL_MANAGER,
            unregistered_id,
            -60,
            60,
            I256::try_from(100_i128).unwrap(),
            1,
        );
    }

    // ── process_block tests ──────────────────────────────────────

    #[test]
    fn v4_process_block_with_no_logs_is_noop() {
        let mut engine = V4BlockEngine::new();

        let pool_id = make_pool_id(1);
        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));
        tick_data.insert(-60, make_tick_info(150, -50));

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool_before = engine.get_pool(fwd_key).unwrap().clone();

        engine.process_block(&[], 50);

        let pool_after = engine.get_pool(fwd_key).unwrap();
        assert_eq!(pool_after.sqrt_price_x96, pool_before.sqrt_price_x96);
        assert_eq!(pool_after.liquidity, pool_before.liquidity);
        assert_eq!(pool_after.tick, pool_before.tick);
    }

    #[test]
    fn v4_process_block_with_swap_and_modify_liquidity() {
        let mut engine = V4BlockEngine::new();
        let pool_id = make_pool_id(10);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(200, -100));
        tick_data.insert(60, make_tick_info(200, 100));

        engine.register_pool(make_register_params(
            POOL_MANAGER,
            pool_id,
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            tick_data,
        )).unwrap();

        engine.start();

        // Build a swap log
        let swap_log = build_v4_swap_log(
            POOL_MANAGER,
            pool_id,
            Address::ZERO,
            I256::try_from(-1000_i128).unwrap(),
            I256::try_from(500_i128).unwrap(),
            U256::from(79466191966197645195421774833u128),
            U128::from(900_000u64),
            10,
            3000,
        );

        // Build a ModifyLiquidity log (positive delta = add liquidity)
        let modify_log = build_v4_modify_liquidity_log(
            POOL_MANAGER,
            pool_id,
            Address::ZERO,
            -120,
            120,
            I256::try_from(500_i128).unwrap(),
            B256::ZERO,
        );

        engine.process_block(&[swap_log, modify_log], 100);

        let fwd_key = engine.pool_keys_for_id(POOL_MANAGER, &pool_id).unwrap().0;
        let pool = engine.get_pool(fwd_key).unwrap();

        // Swap should have updated scalar state
        assert_eq!(pool.sqrt_price_x96, U256::from(79466191966197645195421774833u128));
        assert_eq!(pool.liquidity, 900_000);
        assert_eq!(pool.tick, 10);

        // ModifyLiquidity should have added ticks at -120 and 120
        assert!(pool.tick_data.contains_key(&-120));
        assert!(pool.tick_data.contains_key(&120));
        let new_lower_tick = &pool.tick_data[&-120];
        assert_eq!(new_lower_tick.liquidity_gross, U128::from(500));
        assert_eq!(new_lower_tick.liquidity_net, I256::try_from(500_i128).unwrap());
        let new_upper_tick = &pool.tick_data[&120];
        assert_eq!(new_upper_tick.liquidity_gross, U128::from(500));
        assert_eq!(new_upper_tick.liquidity_net, I256::try_from(-500_i128).unwrap());
    }

    // ── registered_pool_managers test ─────────────────────────────

    #[test]
    fn registered_pool_managers_returns_unique_addresses() {
        let mut engine = V4BlockEngine::new();

        let pm1 = Address::from([0x11u8; 20]);
        let pm2 = Address::from([0x22u8; 20]);

        engine.register_pool(make_register_params(
            pm1,
            make_pool_id(1),
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        )).unwrap();

        engine.register_pool(make_register_params(
            pm1,
            make_pool_id(2),
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        )).unwrap();

        engine.register_pool(make_register_params(
            pm2,
            make_pool_id(3),
            3000,
            60,
            0,
            U256::from(79228162514264337593543950336u128),
            1_000_000,
            0,
            HashMap::new(),
        )).unwrap();

        let managers = engine.registered_pool_managers();
        assert_eq!(managers.len(), 2);
        assert!(managers.contains(&pm1));
        assert!(managers.contains(&pm2));
    }

    // ── Log construction helpers ──────────────────────────────────

    fn build_v4_swap_log(
        pool_manager: Address,
        pool_id: PoolId,
        sender: Address,
        amount0: I256,
        amount1: I256,
        sqrt_price_x96: U256,
        liquidity: U128,
        tick: i32,
        fee: u32,
    ) -> alloy::rpc::types::Log {
        use alloy::primitives::Bytes;

        let mut data = Vec::with_capacity(192);
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        data.extend_from_slice(&amount1.to_be_bytes::<32>());
        data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
        let liq_bytes = liquidity.to_be_bytes::<16>();
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liq_bytes);
        data.extend_from_slice(&liq_word);
        let tick_i256 = I256::try_from(tick as i128).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_i256.to_be_bytes::<32>());
        let mut fee_word = [0u8; 32];
        fee_word[28..32].copy_from_slice(&fee.to_be_bytes());
        data.extend_from_slice(&fee_word);

        let pool_id_topic = B256::from(pool_id);
        let v4_swap_topic = crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;

        let inner = alloy::primitives::Log::new_unchecked(
            pool_manager,
            vec![v4_swap_topic, pool_id_topic, sender.into_word()],
            Bytes::from(data),
        );
        alloy::rpc::types::Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    fn build_v4_modify_liquidity_log(
        pool_manager: Address,
        pool_id: PoolId,
        sender: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
        salt: B256,
    ) -> alloy::rpc::types::Log {
        use alloy::primitives::Bytes;

        let mut data = Vec::with_capacity(128);
        let tick_lower_i256 = I256::try_from(i128::from(tick_lower)).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_lower_i256.to_be_bytes::<32>());
        let tick_upper_i256 = I256::try_from(i128::from(tick_upper)).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_upper_i256.to_be_bytes::<32>());
        data.extend_from_slice(&liquidity_delta.to_be_bytes::<32>());
        data.extend_from_slice(salt.as_slice());

        let pool_id_topic = B256::from(pool_id);
        let v4_modify_liq_topic = crate::bot_core::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;

        let inner = alloy::primitives::Log::new_unchecked(
            pool_manager,
            vec![v4_modify_liq_topic, pool_id_topic, sender.into_word()],
            Bytes::from(data),
        );
        alloy::rpc::types::Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }
}
