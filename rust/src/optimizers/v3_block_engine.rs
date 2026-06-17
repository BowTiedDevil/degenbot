//! V3 Block Engine — V3 pool state owned by [`crate::optimizers::uniswap_engine::UniswapEngine`].
//!
//! This struct owns V3 pool state (sqrt price, liquidity, tick, tick data) and
//! builds integer tick-range sequences for the unified engine. It no longer
//! owns a path/solve subsystem: `UniswapEngine` resolves mixed paths against
//! this state and solves them through the gen-3 integer-exact Möbius solver
//! directly (`int_solve_v3_v3` / `int_solve_cl_path`), never through a
//! stand-alone per-block solve on this engine. The previous f64-based
//! stand-alone solve (`solve_all` / `resolve_path` / `process_block` using
//! `V3TickRangeSequence` + `solve_v3_v3`) has been retired — see
//! `rust/CONTEXT.md` ruling "f64 vs U512 Möbius solver stack".
//!
//! All methods here are pure state accessors/mutators + tick-range sequence
//! construction; solving lives in
//! [`crate::optimizers::uniswap_engine::solver_dispatch`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, U256};

use crate::bot_core::tick_bitmap::{
    apply_liquidity_to_tick_range, compute_tick_ranges, V3TickRangeForSolver,
};
use crate::bot_core::TickInfo;
use crate::optimizers::affected_keys::AffectedKeys;
use crate::optimizers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};

// ---------------------------------------------------------------------------
// V3 pool state (engine-internal, mirrors BotCore::V3PoolState fields)
// ---------------------------------------------------------------------------

/// Parameters for registering a V3 pool with the engine.
///
/// Bundles all fields to satisfy `clippy::too_many_arguments`.
#[derive(Clone, Debug)]
pub struct RegisterV3PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_data: HashMap<i32, TickInfo>,
    pub update_block: u64,
    /// Whether tick data came from the snapshot (Tracked) or has no
    /// snapshot coverage (Sparse). In the new design, the buffer is
    /// always applied (the snapshot is always stale data from the DB).
    pub coverage: crate::optimizers::uniswap_engine::PoolTickCoverage,
}

use crate::optimizers::liquidity_event_buffer::LiquidityEvent;

/// A buffered liquidity update (Mint or Burn) for an unregistered V3 pool.
///
/// When a Mint or Burn event arrives for a pool that hasn't been registered
/// yet, the raw update is stored here. When the pool is later registered,
/// all buffered updates are applied eagerly to bring the tick data current.
///
/// Stores raw event data (not collapsed) to support future reorg handling.
#[derive(Clone, Debug)]
pub struct BufferedV3LiquidityUpdate {
    /// The tick lower boundary of the position.
    pub tick_lower: i32,
    /// The tick upper boundary of the position.
    pub tick_upper: i32,
    /// The signed liquidity delta: positive for Mint, negative for Burn.
    pub liquidity_delta: i128,
    /// The block number of this event.
    pub block_number: u64,
}

impl LiquidityEvent for BufferedV3LiquidityUpdate {
    fn block_number(&self) -> u64 {
        self.block_number
    }
}

/// V3 pool state as owned by the engine.
#[derive(Debug)]
pub struct V3PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,

    // Mutable state
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub update_block: u64,

    // Tick data
    pub tick_data: HashMap<i32, TickInfo>,

    /// Whether the snapshot provided complete tick data for this pool.
    /// `Tracked` = complete (may have empty `tick_data` = genuinely illiquid).
    /// `Sparse` = no snapshot data, solver results may be inaccurate.
    pub coverage: crate::optimizers::uniswap_engine::PoolTickCoverage,

    // Cached tick ranges (interior mutability for lazy computation from &self).
    // Invalidated on apply_swap / apply_liquidity_update.
    // Shared infra: consumed only by `build_int_v3_sequence` (gen-3 integer solver).
    cached_tick_ranges: parking_lot::Mutex<TickRangeCache>,
}

impl Clone for V3PoolState {
    fn clone(&self) -> Self {
        Self {
            address: self.address,
            token0: self.token0,
            token1: self.token1,
            fee: self.fee,
            tick_spacing: self.tick_spacing,
            factory: self.factory,
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            update_block: self.update_block,
            tick_data: self.tick_data.clone(),
            coverage: self.coverage,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
struct TickRangeCache {
    zfo: Option<Arc<[V3TickRangeForSolver]>>,
    ofz: Option<Arc<[V3TickRangeForSolver]>>,
}

impl V3PoolState {
    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = None;
        cache.ofz = None;
    }

    /// Get cached tick ranges for the given direction, computing and caching
    /// if absent. Uses `max_ranges=15` so that all callers can slice the result.
    fn get_cached_tick_ranges(&self, zero_for_one: bool) -> Option<Arc<[V3TickRangeForSolver]>> {
        {
            let cache = self.cached_tick_ranges.lock();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            if let Some(ranges) = slot {
                return Some(Arc::clone(ranges));
            }
        }

        // Not cached — compute and store
        let ranges = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            self.tick_spacing,
            self.liquidity,
            zero_for_one,
            15,
        )
        .map(|(ranges, _)| Arc::<[V3TickRangeForSolver]>::from(ranges));

        if let Some(ref r) = ranges {
            let mut cache = self.cached_tick_ranges.lock();
            if zero_for_one {
                cache.zfo = Some(Arc::clone(r));
            } else {
                cache.ofz = Some(Arc::clone(r));
            }
        }

        ranges
    }

    /// Build an integer V3 tick range sequence with up to `max_ranges` ranges,
    /// using original U256 sqrt prices and i128→u128 liquidity (no f64 conversion).
    ///
    /// This produces an [`IntV3TickRangeSequence`] suitable for the
    /// integer-exact V3-V3 solver, preserving full precision.
    ///
    /// Returns `None` if insufficient tick data.
    #[must_use]
    pub fn build_int_v3_sequence(
        &self,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(zero_for_one)?;
        let use_ranges = ranges.get(..ranges.len().min(max_ranges))?;

        let gamma_numer = u64::from(1_000_000 - self.fee);
        let fee_denom = 1_000_000u64;

        let mut int_ranges = Vec::with_capacity(use_ranges.len());
        for (i, r) in use_ranges.iter().enumerate() {
            // Current sqrt price for this range
            let sqrt_price_x96 = if i == 0 {
                self.sqrt_price_x96
            } else if zero_for_one {
                // Walking down: entered from the upper boundary of prior range
                use_ranges[i - 1].sqrt_price_upper
            } else {
                // Walking up: entered from the lower boundary of prior range
                use_ranges[i - 1].sqrt_price_lower
            };

            // Compute the active liquidity for this range
            //
            // In compute_tick_ranges, `liquidity_net` at `tick_lower` (zfo) or
            // `tick_upper` (ofz) follows the standard Uniswap convention:
            // positive = liquidity added when crossing from below (ascending).
            //
            // When walking DOWN (zfo=True), we cross boundaries from above,
            // so the net change is the NEGATE of the stored value.
            // When walking UP (zfo=False), we cross from below, matching the
            // stored value, so no negation needed.
            let range_liquidity = if i == 0 {
                self.liquidity
            } else {
                let mut l = self.liquidity.cast_signed();
                for prev_range in &use_ranges[..i] {
                    let net = prev_range.liquidity_net;
                    if zero_for_one {
                        // Walking down: crossing from above subtracts the net
                        l -= net;
                    } else {
                        // Walking up: crossing from below adds the net
                        l += net;
                    }
                }
                // i128 → u128: if negative, liquidity is 0 (depleted range)
                if l.is_negative() {
                    0u128
                } else {
                    l.cast_unsigned()
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

impl From<RegisterV3PoolParams> for V3PoolState {
    fn from(params: RegisterV3PoolParams) -> Self {
        Self {
            address: params.address,
            token0: params.token0,
            token1: params.token1,
            fee: params.fee,
            tick_spacing: params.tick_spacing,
            factory: params.factory,

            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: params.update_block,
            tick_data: params.tick_data,
            coverage: params.coverage,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

// ---------------------------------------------------------------------------
// V3BlockEngine
// ---------------------------------------------------------------------------

/// The V3 block engine — owns V3 pool state and constructs tick-range
/// sequences for the unified engine. State mutations only; solving lives
/// in [`crate::optimizers::uniswap_engine::solver_dispatch`].
pub struct V3BlockEngine {
    /// V3 pool state: auto-incrementing key → state
    pools: HashMap<u64, V3PoolState>,
    /// Pool contract address → key
    pool_addresses: HashMap<Address, u64>,
    /// Auto-incrementing pool key
    next_pool_key: u64,
    /// Dual buffer for liquidity updates (Mint/Burn) awaiting pool registration.
    /// Keyed by pool contract address. When a pool is registered, all
    /// buffered updates for its address are applied eagerly.
    event_buffer: crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer<
        Address,
        BufferedV3LiquidityUpdate,
    >,
}

impl V3BlockEngine {
    /// Create a new engine with unbounded event buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_buffer_max_age(None)
    }

    /// Create a new engine with a configurable event buffer staleness limit.
    ///
    /// `max_age`: if `Some(n)`, buffered events older than `n` blocks are
    /// automatically expired during `process_block`. `None` means unbounded.
    #[must_use]
    pub fn new_with_buffer_max_age(event_buffer_max_age: Option<u64>) -> Self {
        let mut event_buffer = crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer::new();
        event_buffer.set_max_age(event_buffer_max_age);
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            next_pool_key: 1,
            event_buffer,
        }
    }

    /// Register a V3 pool by contract address and initial state.
    ///
    /// This only creates the pool entry — buffer application is handled
    /// separately via `apply_backfill_buffer()` and `apply_pump_buffer()`
    /// so the caller can snapshot state at deterministic points for
    /// verification.
    ///
    /// Returns the pool key for use in path registration.
    pub fn register_pool(&mut self, params: RegisterV3PoolParams) -> u64 {
        let key = self.next_pool_key;
        self.next_pool_key += 1;

        let address = params.address;
        self.pools.insert(key, V3PoolState::from(params));
        self.pool_addresses.insert(address, key);

        key
    }

    /// Apply all buffered backfill events for a pool.
    ///
    /// Called during registration, after `register_pool()` and before
    /// `apply_pump_buffer()`. The pool state after this call is at the
    /// backfill boundary — the last block of the backfill range.
    /// This is a deterministic point suitable for verification cloning.
    ///
    /// # Panics
    ///
    /// Panics if a buffered update references a pool key that does not exist
    /// in `self.pools` (should never happen given the registration flow).
    pub fn apply_backfill_buffer(&mut self, address: &Address) {
        let Some(&key) = self.pool_addresses.get(address) else {
            return;
        };
        let Some(buffered) = self.event_buffer.drain_backfill(address) else {
            return;
        };
        for update in buffered {
            let state = self.pools.get_mut(&key).unwrap();
            apply_liquidity_to_tick_range(
                &mut state.tick_data,
                update.tick_lower,
                update.tick_upper,
                update.liquidity_delta,
            );
        }
    }

    /// Apply all buffered pump events for a pool.
    ///
    /// Called during registration, after `apply_backfill_buffer()`.
    /// The pool state after this call reflects all pump-processed events
    /// and is ready for solving.
    ///
    /// # Panics
    ///
    /// Panics if a buffered update references a pool key that does not exist
    /// in `self.pools` (should never happen given the registration flow).
    pub fn apply_pump_buffer(&mut self, address: &Address) {
        let Some(&key) = self.pool_addresses.get(address) else {
            return;
        };
        let Some(buffered) = self.event_buffer.drain_pump(address) else {
            return;
        };
        for update in buffered {
            let state = self.pools.get_mut(&key).unwrap();
            apply_liquidity_to_tick_range(
                &mut state.tick_data,
                update.tick_lower,
                update.tick_upper,
                update.liquidity_delta,
            );
        }
    }

    // ── Event buffer management ──────────────────────────────────

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// `None` means unbounded (no automatic expiry). `Some(n)` means
    /// events older than `n` blocks from the current block are expired.
    /// Takes effect on the next call to `expire_buffered_events` or
    /// `process_block`.
    pub const fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.event_buffer.set_max_age(max_age);
    }

    /// Return the total number of buffered liquidity events for a pool address
    /// (both backfill and pump buffers).
    #[must_use]
    pub fn buffered_event_count(&self, address: &Address) -> usize {
        self.event_buffer.event_count(address)
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    ///
    /// Frees memory. Called when the operator knows that certain pools
    /// will never be registered (e.g., after a path-loading phase completes).
    pub fn flush_event_buffer(&mut self) {
        self.event_buffer.flush();
    }

    /// Expire buffered events older than `current_block - max_age`.
    ///
    /// Called internally during `process_block` and `rebuild_and_solve`.
    /// If `event_buffer_max_age` is `None`, this is a no-op.
    /// Only expires pump buffer events — backfill buffer is never expired.
    pub fn expire_buffered_events(&mut self, current_block: u64) {
        self.event_buffer.expire(current_block);
    }

    /// Update a V3 pool with new Swap event data.
    ///
    /// Updates the scalar fields (`sqrt_price`, liquidity, tick) and records
    /// tick-level priors for the journal. The `tick_data` map is mutated
    /// in-place.
    ///
    /// Returns an [`AffectedKeys`] containing the pool key, or empty if the
    /// pool is not registered.
    pub fn apply_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)], // (tick_index, prior_state)
    ) -> AffectedKeys {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            return AffectedKeys::empty();
        };
        let Some(pool) = self.pools.get_mut(&key) else {
            return AffectedKeys::empty();
        };

        // Apply tick priors updates to tick_data
        for &(tick_index, ref prior) in tick_priors {
            pool.tick_data.insert(tick_index, prior.clone());
        }

        pool.sqrt_price_x96 = sqrt_price_x96;
        pool.liquidity = liquidity;
        pool.tick = tick;
        pool.update_block = block_number;
        pool.invalidate_tick_range_cache();

        AffectedKeys::single(key)
    }

    /// Apply a liquidity update (Mint or Burn) to a V3 pool's `tick_data`.
    ///
    /// For a Mint event at `[tick_lower, tick_upper]` with `amount`:
    /// - `tick_data[tick_lower].liquidity_net += amount`
    /// - `tick_data[tick_upper].liquidity_net -= amount`
    /// - `tick_data[tick_lower].liquidity_gross += amount`
    /// - `tick_data[tick_upper].liquidity_gross += amount`
    ///
    /// For a Burn event, the delta is negative (amount is negated before calling).
    ///
    /// Buffer a liquidity update from the backfill phase for an unregistered pool.
    ///
    /// Unlike `apply_liquidity_update`, this always buffers — during backfill,
    /// no pools are registered yet, so there's no registered-pool fast path.
    /// Routes to `backfill_event_buffer` which is never expired.
    ///
    /// # Panics
    ///
    /// Panics if the pool is already registered but `self.pools.get_mut()`
    /// returns `None` for the known key (should never happen).
    pub fn buffer_backfill_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        // During backfill, no pools are registered — always buffer
        // Check if already registered (shouldn't happen during backfill,
        // but be defensive)
        if let Some(&key) = self.pool_addresses.get(&pool_address) {
            // Pool already registered (unusual during backfill) — apply directly
            let pool = self.pools.get_mut(&key).unwrap();
            apply_liquidity_to_tick_range(&mut pool.tick_data, tick_lower, tick_upper, liquidity_delta);
            pool.update_block = block_number;
            pool.invalidate_tick_range_cache();
            return;
        }

        self.event_buffer.buffer_backfill(
            pool_address,
            BufferedV3LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            },
        );
    }

    /// If a tick does not yet exist in `tick_data`, it is inserted with
    /// `liquidity_gross = |delta|` and `liquidity_net = delta`. This handles
    /// new position initialization.
    pub fn apply_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> AffectedKeys {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            // Pool not registered — buffer the update in the pump buffer
            self.event_buffer.buffer_pump(
                pool_address,
                BufferedV3LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                },
            );
            return AffectedKeys::empty();
        };
        let Some(pool) = self.pools.get_mut(&key) else {
            return AffectedKeys::empty();
        };

        // Apply tick-level mutations:
        // In Solidity's Tick.update(), BOTH lower and upper tick get:
        //   liquidity_gross += liquidityDelta
        // But liquidity_net differs:
        //   tick_lower: liquidity_net += liquidityDelta
        //   tick_upper: liquidity_net -= liquidityDelta
        apply_liquidity_to_tick_range(&mut pool.tick_data, tick_lower, tick_upper, liquidity_delta);

        pool.update_block = block_number;
        pool.invalidate_tick_range_cache();

        AffectedKeys::single(key)
    }

    /// Apply Swap updates and return the set of pool keys that changed.
    /// Does NOT rebuild paths or solve — caller handles that.
    pub fn apply_swap_updates(&mut self, updates: &[V3SwapUpdate], block_number: u64) -> HashSet<u64> {
        let mut affected = HashSet::new();
        for update in updates {
            for key in self.apply_swap(
                update.pool_address,
                update.sqrt_price_x96,
                update.liquidity,
                update.tick,
                block_number,
                &update.tick_priors,
            ).iter() {
                affected.insert(key);
            }
        }
        affected
    }

    /// Look up the pool key for a registered address.
    /// Returns `None` if the address is not registered.
    #[must_use]
    pub fn pool_key_for_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
    }

    /// Return the list of registered pool addresses.
    #[must_use]
    pub fn registered_addresses(&self) -> Vec<Address> {
        self.pool_addresses.keys().copied().collect()
    }

    /// Number of registered pools.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pool_addresses.len()
    }

    /// Get a reference to a V3 pool state by pool key.
    #[must_use]
    pub fn get_pool(&self, pool_key: u64) -> Option<&V3PoolState> {
        self.pools.get(&pool_key)
    }

    /// Snapshot all pool state for verification (clones the entire map).
    ///
    /// Used by `verify_liquidity_maps` so the engine lock can be released
    /// before making async RPC calls.
    #[must_use]
    pub fn pools_snapshot(&self) -> HashMap<u64, V3PoolState> {
        self.pools.clone()
    }

    /// Full-sync a V3 pool's `tick_data` from an external source (e.g., Python backfill).
    ///
    /// Unlike `apply_swap` (which only inserts/overlays `tick_priors`), this method
    /// **replaces** the entire `tick_data` map for the pool. This ensures that ticks
    /// removed from Python (because `liquidityGross` went to zero after a Burn) are
    /// also removed from the Rust engine.
    ///
    /// Also updates scalar state (`sqrt_price_x96`, `liquidity`, `tick`).
    ///
    /// No-op if the pool address is not registered.
    pub fn sync_pool_state(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            return;
        };
        let Some(pool) = self.pools.get_mut(&key) else {
            return;
        };

        pool.sqrt_price_x96 = sqrt_price_x96;
        pool.liquidity = liquidity;
        pool.tick = tick;
        pool.tick_data = tick_data;
        pool.update_block = update_block;
    }
}

impl Default for V3BlockEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Swap update type for testing
// ---------------------------------------------------------------------------

/// A pre-decoded V3 Swap update for testing without log decoding.
#[derive(Clone, Debug)]
pub struct V3SwapUpdate {
    pub pool_address: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_priors: Vec<(i32, TickInfo)>,
}
