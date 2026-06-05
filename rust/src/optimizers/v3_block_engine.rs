//! V3 Block Engine — Rust-centric arbitrage engine for Uniswap V3 paths.
//!
//! Owns the per-block lifecycle for V3 pools: Swap event decoding, pool state
//! updates (including tick-level changes), tick-range computation, and
//! Mobius piecewise solver dispatch.
//!
//! # Design
//!
//! The engine stores V3 pool state (including tick data) and constructs
//! [`V3TickRangeSequence`] objects for each registered pool+direction. Paths
//! are registered as ordered lists of (`pool_key`, `zero_for_one`) pairs.
//!
//! On `process_block()`:
//! 1. Decode Swap events and apply updates (including tick priors for journal)
//! 2. Rebuild tick-range sequences from updated pool state
//! 3. Solve all registered paths
//! 4. Store results for Python to read

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U128, U256};

use crate::bot_core::tick_bitmap::{compute_tick_ranges, V3TickRangeForSolver};
use crate::bot_core::TickInfo;
use crate::bot_core::v3_mint_burn_decoder::{decode_v3_mint_log, decode_v3_burn_log};
use crate::bot_core::v3_swap_decoder::decode_v3_swap_log;
use crate::optimizers::mobius::HopState;
use crate::optimizers::mobius_int::{u256_to_f64, IntHopState};
use crate::optimizers::mobius_v3::{V3TickRangeHop, V3TickRangeSequence};
use crate::optimizers::mobius_v3_int::{IntV3TickRangeHop, IntV3TickRangeSequence};
use crate::optimizers::mobius_v3_v3::solve_v3_v3;

// Q96 = 2^96, used to convert Q128.96 sqrt prices to plain floats
const Q96_F64: f64 = 79_228_162_514_264_337_593_543_950_336.0;

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
    /// Whether to apply buffered `Mint`/`Burn` events on top of the
    /// provided `tick_data`. Set to `true` when `tick_data` comes from a
    /// stale DB snapshot (the buffer brings it forward). Set to `false`
    /// when `tick_data` was fetched at the current block via RPC (applying
    /// the buffer would double-count those events).
    pub apply_buffer: bool,
}

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

    // Cached tick ranges (interior mutability for lazy computation from &self).
    // Invalidated on apply_swap / apply_liquidity_update.
    cached_tick_ranges: std::sync::Mutex<TickRangeCache>,
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
            cached_tick_ranges: std::sync::Mutex::new(TickRangeCache::default()),
        }
    }
}

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
struct TickRangeCache {
    zfo: Option<Vec<V3TickRangeForSolver>>,
    ofz: Option<Vec<V3TickRangeForSolver>>,
}

impl V3PoolState {
    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock().unwrap();
        cache.zfo = None;
        cache.ofz = None;
    }

    /// Get cached tick ranges for the given direction, computing and caching
    /// if absent. Uses `max_ranges=15` so that all callers can slice the
    /// result to their needs.
    fn get_cached_tick_ranges(&self, zero_for_one: bool) -> Option<Vec<V3TickRangeForSolver>> {
        {
            let cache = self.cached_tick_ranges.lock().unwrap();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            if slot.is_some() {
                return slot.clone();
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
        .map(|(ranges, _)| ranges);

        if let Some(ref r) = ranges {
            let mut cache = self.cached_tick_ranges.lock().unwrap();
            if zero_for_one {
                cache.zfo = Some(r.clone());
            } else {
                cache.ofz = Some(r.clone());
            }
        }

        ranges
    }

    /// Compute tick-range sequences for both swap directions using the
    /// current tick data and [`compute_tick_ranges`].
    ///
    /// Returns `(zfo_sequence, ofz_sequence)` or `None` for either direction
    /// if insufficient initialized ticks.
    #[must_use]
    pub fn build_tick_range_sequences(
        &self,
        max_ranges: usize,
    ) -> (Option<V3TickRangeSequence>, Option<V3TickRangeSequence>) {
        let zfo = self.build_sequence(true, max_ranges);
        let ofz = self.build_sequence(false, max_ranges);
        (zfo, ofz)
    }

    #[must_use]
    pub fn build_sequence(
        &self,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<V3TickRangeSequence> {
        let mut ranges = self.get_cached_tick_ranges(zero_for_one)?;
        // Truncate to caller's max_ranges
        ranges.truncate(max_ranges);

        let fee_f64 = f64::from(self.fee) / 1_000_000.0;

        let mut rust_ranges = Vec::with_capacity(ranges.len());
        for (i, r) in ranges.iter().enumerate() {
            // Current sqrt price for this range:
            // - Range 0: the pool's current sqrt price
            // - Later ranges (zfo): entry from upper boundary of prior range
            // - Later ranges (ofz): entry from lower boundary of prior range
            let sqrt_p_current = if i == 0 {
                u256_to_f64(self.sqrt_price_x96)
            } else {
                let prev = &ranges[i - 1];
                if zero_for_one {
                    // Walking down: entered from the upper boundary of prior range
                    u256_to_f64(prev.sqrt_price_upper)
                } else {
                    // Walking up: entered from the lower boundary of prior range
                    u256_to_f64(prev.sqrt_price_lower)
                }
            };

            // Compute the active liquidity for this range.
            // Range 0 uses the pool's current liquidity.
            // Subsequent ranges accumulate boundary-crossing liquidity_net values.
            //
            // In compute_tick_ranges, `liquidity_net` at `tick_lower` (zfo) or
            // `tick_upper` (ofz) follows the standard Uniswap convention:
            // positive = liquidity added when crossing from below (ascending).
            //
            // When walking DOWN (zfo=True), we cross boundaries from above,
            // so the net change is the NEGATE of the stored value.
            // When walking UP (zfo=False), we cross from below, matching the
            // stored value, so no negation needed.
            #[allow(clippy::cast_precision_loss)]
            let range_liquidity = if i == 0 {
                self.liquidity as f64
            } else {
                let mut l = self.liquidity.cast_signed();
                for prev_range in &ranges[..i] {
                    let net = prev_range.liquidity_net;
                    if zero_for_one {
                        // Walking down: crossing from above subtracts the net
                        l -= net;
                    } else {
                        // Walking up: crossing from below adds the net
                        l += net;
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                {
                    l as f64
                }
            };

            rust_ranges.push(V3TickRangeHop {
                liquidity: range_liquidity,
                sqrt_price_current: sqrt_p_current / Q96_F64,
                sqrt_price_lower: u256_to_f64(r.sqrt_price_lower) / Q96_F64,
                sqrt_price_upper: u256_to_f64(r.sqrt_price_upper) / Q96_F64,
                fee: fee_f64,
                zero_for_one,
            });
        }

        V3TickRangeSequence::new(rust_ranges).ok()
    }

    /// Build an integer V3 tick range hop for the first range, using
    /// original U256 sqrt prices and u128 liquidity (no f64 conversion).
    ///
    /// This produces an [`IntV3TickRangeHop`] suitable for the integer-exact
    /// Möbius solver, preserving full precision.
    ///
    /// Returns `None` if insufficient tick data.
    #[must_use]
    pub fn build_int_v3_hop(
        &self,
        zero_for_one: bool,
    ) -> Option<IntV3TickRangeHop> {
        let ranges = self.get_cached_tick_ranges(zero_for_one)?;

        // Only use the first range (same as the old f64 solver)
        let r = ranges.first()?;

        // Compute the active liquidity for the first range = pool's current liquidity
        let first_range_liquidity = self.liquidity;

        // Gamma representation: gamma = (1_000_000 - fee) / 1_000_000
        let gamma_numer = u64::from(1_000_000 - self.fee);
        let fee_denom = 1_000_000u64;

        Some(IntV3TickRangeHop {
            liquidity: first_range_liquidity,
            sqrt_price_x96: self.sqrt_price_x96,
            sqrt_price_lower_x96: r.sqrt_price_lower,
            sqrt_price_upper_x96: r.sqrt_price_upper,
            gamma_numer,
            fee_denom,
            zero_for_one,
        })
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
        let mut ranges = self.get_cached_tick_ranges(zero_for_one)?;
        ranges.truncate(max_ranges);

        let gamma_numer = u64::from(1_000_000 - self.fee);
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
                for prev_range in &ranges[..i] {
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
            cached_tick_ranges: std::sync::Mutex::new(TickRangeCache::default()),
        }
    }
}

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// A pool reference in a path: (`pool_key` index, `zero_for_one` direction).
#[derive(Clone, Debug)]
pub struct V3PoolRef {
    /// Index into the engine's `pools` map.
    pub pool_idx: u64,
    /// Direction for this hop.
    pub zero_for_one: bool,
}

/// A registered V3 arbitrage path.
#[derive(Clone, Debug)]
struct V3Path {
    pools: Vec<V3PoolRef>,
}

/// Resolved state for a path, ready for solving.
#[derive(Clone, Debug, Default)]
struct ResolvedV3Path {
    /// Pre-built tick-range sequences for V3 hops (index matches path.pools).
    tick_range_sequences: Vec<Option<V3TickRangeSequence>>,
    /// For V2 hops in mixed paths, the [`IntHopState`].
    /// None for pure V3 hops.
    int_hops: Vec<Option<IntHopState>>,
    /// Base (f64) hops for Mobius initial estimate.
    base_hops: Vec<HopState>,
    /// Whether this path is valid for solving.
    valid: bool,
}

// ---------------------------------------------------------------------------
// V3BlockEngine
// ---------------------------------------------------------------------------

/// The V3 block engine — owns V3 pool state, constructs tick-range
/// sequences, and solves arbitrage paths.
pub struct V3BlockEngine {
    /// V3 pool state: auto-incrementing key → state
    pools: HashMap<u64, V3PoolState>,
    /// Pool contract address → key
    pool_addresses: HashMap<Address, u64>,
    /// Registered paths: `path_id` → (`V3Path`, `ResolvedV3Path`)
    paths: HashMap<u64, (V3Path, ResolvedV3Path)>,
    /// Last solved results: (`path_id`, `optimal_input`, profit)
    results: Vec<(u64, U256, U256)>,
    /// Block number for the last solved results
    results_block: u64,
    /// Auto-incrementing path ID
    next_path_id: u64,
    /// Auto-incrementing pool key
    next_pool_key: u64,
    /// Buffered liquidity updates (Mint/Burn) for pools not yet registered.
    /// Keyed by pool contract address. When a pool is registered, all
    /// buffered updates for its address are applied eagerly.
    liquidity_event_buffer: HashMap<Address, Vec<BufferedV3LiquidityUpdate>>,
    /// Maximum age (in blocks) for buffered events. `None` means unbounded.
    /// Events older than `current_block - max_age` are expired during
    /// `process_block` or `expire_buffered_events`.
    event_buffer_max_age: Option<u64>,
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
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            paths: HashMap::new(),
            results: Vec::new(),
            results_block: 0,
            next_path_id: 1,
            next_pool_key: 1,
            liquidity_event_buffer: HashMap::new(),
            event_buffer_max_age,
        }
    }

    /// Register a V3 pool by contract address and initial state.
    ///
    /// After inserting the pool, applies any buffered liquidity updates
    /// (Mint/Burn events received before the pool was registered).
    /// Returns the pool key for use in path registration.
    pub fn register_pool(&mut self, params: RegisterV3PoolParams) -> u64 {
        let key = self.next_pool_key;
        self.next_pool_key += 1;

        let address = params.address;
        let apply_buffer = params.apply_buffer;
        self.pools.insert(key, V3PoolState::from(params));
        self.pool_addresses.insert(address, key);

        // Apply any buffered liquidity updates that arrived before this
        // pool was registered (e.g. from backfill_from_snapshot or the
        // WS subscribe phase). These events are NOT yet reflected in the
        // tick_data when it comes from a stale DB snapshot, so they must
        // be applied on top. However, if the tick_data was fetched at the
        // current block via RPC, the buffer would double-count those
        // events — in that case, simply discard the buffer.
        if let Some(buffered) = self.liquidity_event_buffer.remove(&address) {
            if apply_buffer {
                for update in buffered {
                    let state = self.pools.get_mut(&key).unwrap();
                    update_tick_liquidity(&mut state.tick_data, update.tick_lower, update.liquidity_delta, true);
                    update_tick_liquidity(&mut state.tick_data, update.tick_upper, update.liquidity_delta, false);
                    state.tick_data.retain(|_, info| !info.liquidity_gross.is_zero());
                }
            }
            // If !apply_buffer, the buffered events are simply discarded —
            // the tick_data already reflects them.
        }

        key
    }

    // ── Event buffer management ──────────────────────────────────

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// `None` means unbounded (no automatic expiry). `Some(n)` means
    /// events older than `n` blocks from the current block are expired.
    /// Takes effect on the next call to `expire_buffered_events` or
    /// `process_block`.
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.event_buffer_max_age = max_age;
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    ///
    /// Frees memory. Called when the operator knows that certain pools
    /// will never be registered (e.g., after a path-loading phase completes).
    pub fn flush_event_buffer(&mut self) {
        self.liquidity_event_buffer.clear();
    }

    /// Expire buffered events older than `current_block - max_age`.
    ///
    /// Called internally during `process_block` and `rebuild_and_solve`.
    /// If `event_buffer_max_age` is `None`, this is a no-op.
    pub fn expire_buffered_events(&mut self, current_block: u64) {
        let Some(max_age) = self.event_buffer_max_age else {
            return;
        };

        let cutoff = current_block.saturating_sub(max_age);

        for events in self.liquidity_event_buffer.values_mut() {
            events.retain(|ev| ev.block_number >= cutoff);
        }

        // Remove entries with no remaining events
        self.liquidity_event_buffer.retain(|_, events| !events.is_empty());
    }

    /// Register an arbitrage path as an ordered list of (`pool_key`,
    /// `zero_for_one`) pairs.
    ///
    /// Returns the auto-assigned path ID.
    ///
    /// # Panics
    ///
    /// Panics with fewer than 2 pool refs.
    pub fn register_path(&mut self, pool_refs: Vec<V3PoolRef>) -> u64 {
        assert!(pool_refs.len() >= 2, "need at least 2 pool refs");

        let path_id = self.next_path_id;
        self.next_path_id += 1;

        let mut resolved = ResolvedV3Path::default();
        self.resolve_path(&pool_refs, &mut resolved);

        self.paths
            .insert(path_id, (V3Path { pools: pool_refs }, resolved));
        path_id
    }

    /// Update a V3 pool with new Swap event data.
    ///
    /// Updates the scalar fields (`sqrt_price`, liquidity, tick) and records
    /// tick-level priors for the journal. The `tick_data` map is mutated
    /// in-place.
    pub fn apply_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)], // (tick_index, prior_state)
    ) -> Option<u64> {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            return None;
        };
        let Some(pool) = self.pools.get_mut(&key) else {
            return None;
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

        Some(key)
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
    ) -> Option<u64> {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            // Pool not registered — buffer the update for later
            self.liquidity_event_buffer
                .entry(pool_address)
                .or_default()
                .push(BufferedV3LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                });
            return None;
        };
        let Some(pool) = self.pools.get_mut(&key) else {
            return None;
        };

        // Apply tick-level mutations:
        // In Solidity's Tick.update(), BOTH lower and upper tick get:
        //   liquidity_gross += liquidityDelta
        // But liquidity_net differs:
        //   tick_lower: liquidity_net += liquidityDelta
        //   tick_upper: liquidity_net -= liquidityDelta
        update_tick_liquidity(&mut pool.tick_data, tick_lower, liquidity_delta, true);
        update_tick_liquidity(&mut pool.tick_data, tick_upper, liquidity_delta, false);

        // Remove ticks with zero liquidity_gross (position fully closed)
        pool.tick_data.retain(|_, info| !info.liquidity_gross.is_zero());

        pool.update_block = block_number;
        pool.invalidate_tick_range_cache();

        Some(key)
    }

    /// Process a block: decode Swap, Mint, and Burn events, apply updates,
    /// rebuild paths, solve all, and store results.
    pub fn process_block(&mut self, logs: &[alloy::rpc::types::Log], block_number: u64) {
        for log in logs {
            if let Some(event) = decode_v3_swap_log(log) {
                self.apply_swap(
                    event.pool_address,
                    event.sqrt_price_x96,
                    extract_u128(event.liquidity),
                    event.tick,
                    block_number,
                    &[],
                );
            } else if let Some(event) = decode_v3_mint_log(log) {
                self.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    event.amount.cast_signed(),
                    block_number,
                );
            } else if let Some(event) = decode_v3_burn_log(log) {
                self.apply_liquidity_update(
                    event.pool_address,
                    event.tick_lower,
                    event.tick_upper,
                    -(event.amount.cast_signed()),
                    block_number,
                );
            }
        }

        self.expire_buffered_events(block_number);
        self.rebuild_and_solve(block_number);
    }

    /// Process pre-decoded Swap updates for testing.
    pub fn process_swap_updates(&mut self, updates: &[V3SwapUpdate], block_number: u64) {
        for update in updates {
            self.apply_swap(
                update.pool_address,
                update.sqrt_price_x96,
                update.liquidity,
                update.tick,
                block_number,
                &update.tick_priors,
            );
        }

        self.rebuild_and_solve(block_number);
    }

    /// Apply Swap updates and return the set of pool keys that changed.
    /// Does NOT rebuild paths or solve — caller handles that.
    pub fn apply_swap_updates(&mut self, updates: &[V3SwapUpdate], block_number: u64) -> HashSet<u64> {
        let mut affected = HashSet::new();
        for update in updates {
            if let Some(key) = self.apply_swap(
                update.pool_address,
                update.sqrt_price_x96,
                update.liquidity,
                update.tick,
                block_number,
                &update.tick_priors,
            ) {
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

    /// Rebuild all path resolutions and solve.
    pub fn rebuild_and_solve(&mut self, block_number: u64) {
        // Collect path pool refs so we can rebuild without borrowing self.paths
        let path_pool_refs: Vec<(u64, Vec<V3PoolRef>)> = self
            .paths
            .iter()
            .map(|(&id, (path, _))| (id, path.pools.clone()))
            .collect();

        // Rebuild each path
        for (path_id, pool_refs) in &path_pool_refs {
            let mut resolved = ResolvedV3Path::default();
            self.resolve_path(pool_refs, &mut resolved);
            if let Some((_, stored)) = self.paths.get_mut(path_id) {
                *stored = resolved;
            }
        }

        // Solve all paths
        self.results = self.solve_all(None);
        self.results_block = block_number;
    }

    /// Solve all registered V3 paths.
    ///
    /// Currently supports:
    /// - V3-V3 2-hop paths (piecewise Mobius V3-V3 solver)
    ///
    /// Will be extended for V3-V2 and V2-V3 paths.
    #[must_use]
    pub fn solve_all(&self, max_input: Option<f64>) -> Vec<(u64, U256, U256)> {
        let mut results = Vec::with_capacity(self.paths.len());

        for (&path_id, (_path, resolved)) in &self.paths {
            if !resolved.valid {
                continue;
            }

            // Dispatch based on hop count and type
            let v3_sequences: Vec<&V3TickRangeSequence> = resolved
                .tick_range_sequences
                .iter()
                .filter_map(Option::as_ref)
                .collect();

            // V3-V3 2-hop
            if v3_sequences.len() == 2 && resolved.int_hops.iter().all(Option::is_none) {
                let (x, profit, _iters) = solve_v3_v3(
                    v3_sequences[0],
                    v3_sequences[1],
                    max_input,
                    10, // max_candidates
                );
                if x > 0.0 && profit > 0.0 {
                    #[allow(clippy::cast_possible_truncation)]
                    #[allow(clippy::cast_sign_loss)]
                    {
                        let x_int = U256::from(x as u128);
                        let profit_int = U256::from(profit as u128);
                        if !x_int.is_zero() && !profit_int.is_zero() {
                            results.push((path_id, x_int, profit_int));
                        }
                    }
                }
            }
        }

        results
    }

    /// Read the last solved results and block number.
    #[must_use]
    pub const fn latest_results(&self) -> (&Vec<(u64, U256, U256)>, u64) {
        (&self.results, self.results_block)
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

    /// Number of registered paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
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

    /// Resolve a path's pool refs into tick-range sequences and hop states.
    fn resolve_path(&self, pool_refs: &[V3PoolRef], resolved: &mut ResolvedV3Path) {
        resolved.tick_range_sequences.clear();
        resolved.int_hops.clear();
        resolved.base_hops.clear();
        resolved.valid = false;

        if pool_refs.len() < 2 {
            return;
        }

        resolved.tick_range_sequences.reserve(pool_refs.len());
        resolved.int_hops.reserve(pool_refs.len());

        for pool_ref in pool_refs {
            let Some(pool) = self.pools.get(&pool_ref.pool_idx) else {
                return; // Missing pool → invalid
            };

            let sequence = pool.build_sequence(pool_ref.zero_for_one, 3);
            resolved.tick_range_sequences.push(sequence);
            // V3 hops don't use IntHopState
            resolved.int_hops.push(None);
        }

        // Build base hops from tick-range sequences for the initial Mobius estimate
        for seq_opt in &resolved.tick_range_sequences {
            if let Some(seq) = seq_opt {
                if let Some(first_range) = seq.ranges.first() {
                    resolved.base_hops.push(first_range.to_hop_state());
                } else {
                    return; // Empty sequence → invalid
                }
            } else {
                return; // Missing sequence → invalid
            }
        }

        resolved.valid = true;
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a `u128` from the V3 Swap event's `liquidity` field.
/// The [`V3SwapEvent`] stores liquidity as `U128` (alloy).
fn extract_u128(liquidity: U128) -> u128 {
    liquidity.to::<u128>()
}

/// Update a single tick's `liquidity_gross` and `liquidity_net` in-place.
///
/// Matches the Uniswap V3 `Tick.update()` logic:
/// - `liquidity_gross += delta` (always, for both lower and upper ticks)
/// - `liquidity_net += delta` for the lower tick
/// - `liquidity_net -= delta` for the upper tick
///
/// The `is_lower_tick` parameter controls whether to add or subtract the
/// delta from `liquidity_net` (matches Solidity's `if (upper)` check).
///
/// If a tick doesn't exist yet, it's inserted with appropriate initial values.
/// Ticks with `liquidity_gross == 0` after the update should be removed by the caller.
fn update_tick_liquidity(
    tick_data: &mut HashMap<i32, TickInfo>,
    tick: i32,
    delta: i128,
    is_lower_tick: bool,
) {
    use alloy::primitives::{I256, U128};

    let entry = tick_data.entry(tick).or_insert(TickInfo {
        liquidity_gross: U128::ZERO,
        liquidity_net: I256::ZERO,
    });

    // Update liquidity_gross: += delta (always the same direction for both ticks)
    let current_gross = entry.liquidity_gross.to::<u128>();
    let new_gross_i128 = current_gross.cast_signed() + delta;
    // liquidity_gross is always >= 0 in valid state; negative means an underflow bug
    let new_gross = if new_gross_i128 < 0 {
        U128::ZERO
    } else {
        U128::from(new_gross_i128.cast_unsigned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::I256;

    fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> TickInfo {
        use alloy::primitives::I256;
        TickInfo {
            liquidity_gross: U128::from(liquidity_gross),
            liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
        }
    }

    fn make_pool_state(
        address: Address,
        fee: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
    ) -> V3PoolState {
        V3PoolState {
            address,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee,
            tick_spacing,
            factory: Address::ZERO,
            sqrt_price_x96,
            liquidity,
            tick,
            update_block: 0,
            tick_data,
            cached_tick_ranges: std::sync::Mutex::new(TickRangeCache::default()),
        }
    }

    #[test]
    fn register_v3_pool_stores_state() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);
        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });

        assert_eq!(key, 1);
        assert!(engine.pools.contains_key(&key));
        assert_eq!(engine.pool_addresses[&addr], key);
    }

    #[test]
    fn register_v3_pool_sets_update_block() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);
        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 21_000_000,
            apply_buffer: true,
        });
        let pool = &engine.pools[&key];
        assert_eq!(pool.update_block, 21_000_000);
    }

    #[test]
    fn register_v3_pool_after_start_succeeds() {
        let mut engine = V3BlockEngine::new();
        engine.register_pool(RegisterV3PoolParams {
            address: Address::ZERO,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });
        // Registration is always-on; this should not panic
        engine.register_pool(RegisterV3PoolParams {
            address: Address::from([1u8; 20]),
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::from([4u8; 20]),
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });
    }

    #[test]
    fn register_v3_path_stores_and_resolves() {
        let mut engine = V3BlockEngine::new();

        let addr0 = Address::from([0x11u8; 20]);
        let addr1 = Address::from([0x22u8; 20]);

        let mut tick_data0 = HashMap::new();
        tick_data0.insert(60, make_tick_info(200, 100));
        tick_data0.insert(-60, make_tick_info(150, -50));

        let mut tick_data1 = HashMap::new();
        tick_data1.insert(60, make_tick_info(300, 200));
        tick_data1.insert(-60, make_tick_info(250, -100));

        let key0 = engine.register_pool(RegisterV3PoolParams {
            address: addr0,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: tick_data0,
            update_block: 0,
            apply_buffer: true,
        });

        let key1 = engine.register_pool(RegisterV3PoolParams {
            address: addr1,
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 2_000_000,
            tick: 0,
            tick_data: tick_data1,
            update_block: 0,
            apply_buffer: true,
        });

        let path_id = engine.register_path(vec![
            V3PoolRef {
                pool_idx: key0,
                zero_for_one: true,
            },
            V3PoolRef {
                pool_idx: key1,
                zero_for_one: false,
            },
        ]);

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);
        assert_eq!(resolved.tick_range_sequences.len(), 2);
    }

    #[test]
    fn apply_swap_updates_pool_state() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        let new_sqrt_price = U256::from(79_466_191_966_197_645_195_421_774_833_u128);
        engine.apply_swap(addr, new_sqrt_price, 900_000, 60, 42, &[]);

        let pool = &engine.pools[&key];
        assert_eq!(pool.sqrt_price_x96, new_sqrt_price);
        assert_eq!(pool.liquidity, 900_000);
        assert_eq!(pool.tick, 60);
        assert_eq!(pool.update_block, 42);
    }

    #[test]
    fn apply_swap_updates_tick_data() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });

        let tick_priors = vec![(60, make_tick_info(200, 100))];
        engine.apply_swap(
            addr,
            U256::from(79_466_191_966_197_645_195_421_774_833_u128),
            900_000,
            60,
            42,
            &tick_priors,
        );

        let pool = &engine.pools[&key];
        assert!(pool.tick_data.contains_key(&60));
        let info = &pool.tick_data[&60];
        assert_eq!(info.liquidity_gross, U128::from(200));
    }

    #[test]
    fn apply_swap_ignores_unregistered_pool() {
        let mut engine = V3BlockEngine::new();
        let unregistered = Address::from([0xaau8; 20]);

        engine.apply_swap(unregistered, U256::ONE, 100, 0, 1, &[]);
    }

    #[test]
    fn build_tick_range_sequence_zfo() {
        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(200, -100));
        tick_data.insert(60, make_tick_info(300, 150));

        let pool = make_pool_state(
            Address::ZERO,
            3000,
            60,
            U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            1_000_000,
            0,
            tick_data,
        );

        let (zfo_seq, ofz_seq) = pool.build_tick_range_sequences(3);
        assert!(zfo_seq.is_some(), "zfo sequence should exist");
        assert!(ofz_seq.is_some(), "ofz sequence should exist");

        let zfo = zfo_seq.unwrap();
        assert!(zfo.zero_for_one());
        assert!(!zfo.ranges.is_empty());
    }

    #[test]
    fn build_tick_range_sequence_insufficient_ticks() {
        let pool = make_pool_state(
            Address::ZERO,
            3000,
            60,
            U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            1_000_000,
            0,
            HashMap::new(),
        );

        let (zfo_seq, ofz_seq) = pool.build_tick_range_sequences(3);
        assert!(
            zfo_seq.is_none(),
            "zfo sequence should not exist without tick data"
        );
        assert!(
            ofz_seq.is_none(),
            "ofz sequence should not exist without tick data"
        );
    }

    #[test]
    fn solve_v3_v3_two_hop_path() {
        let mut engine = V3BlockEngine::new();

        let addr0 = Address::from([0x11u8; 20]);
        let addr1 = Address::from([0x22u8; 20]);

        let mut tick_data0 = HashMap::new();
        tick_data0.insert(-60, make_tick_info(200, -100));
        tick_data0.insert(60, make_tick_info(300, 150));
        tick_data0.insert(-120, make_tick_info(100, 50));
        tick_data0.insert(120, make_tick_info(400, 200));

        let mut tick_data1 = HashMap::new();
        tick_data1.insert(-60, make_tick_info(250, -80));
        tick_data1.insert(60, make_tick_info(350, 120));
        tick_data1.insert(-120, make_tick_info(150, 30));
        tick_data1.insert(120, make_tick_info(450, 180));

        let key0 = engine.register_pool(RegisterV3PoolParams {
            address: addr0,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000_000_000,
            tick: 0,
            tick_data: tick_data0,
            update_block: 0,
            apply_buffer: true,
        });

        let key1 = engine.register_pool(RegisterV3PoolParams {
            address: addr1,
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 2_000_000_000_000,
            tick: 0,
            tick_data: tick_data1,
            update_block: 0,
            apply_buffer: true,
        });

        let _path_id = engine.register_path(vec![
            V3PoolRef {
                pool_idx: key0,
                zero_for_one: true,
            },
            V3PoolRef {
                pool_idx: key1,
                zero_for_one: false,
            },
        ]);

        let (_, resolved) = engine.paths.values().next().unwrap();
        assert!(resolved.valid, "path should be valid");
    }

    #[test]
    fn process_swap_updates_rebuilds_and_solves() {
        let mut engine = V3BlockEngine::new();

        let addr0 = Address::from([0x11u8; 20]);
        let addr1 = Address::from([0x22u8; 20]);

        let mut tick_data0 = HashMap::new();
        tick_data0.insert(-60, make_tick_info(500, -200));
        tick_data0.insert(60, make_tick_info(800, 300));

        let mut tick_data1 = HashMap::new();
        tick_data1.insert(-60, make_tick_info(600, -250));
        tick_data1.insert(60, make_tick_info(900, 350));

        let key0 = engine.register_pool(RegisterV3PoolParams {
            address: addr0,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 10_000_000_000_000,
            tick: 0,
            tick_data: tick_data0,
            update_block: 0,
            apply_buffer: true,
        });

        let key1 = engine.register_pool(RegisterV3PoolParams {
            address: addr1,
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 20_000_000_000_000,
            tick: 0,
            tick_data: tick_data1,
            update_block: 0,
            apply_buffer: true,
        });

        engine.register_path(vec![
            V3PoolRef { pool_idx: key0, zero_for_one: true },
            V3PoolRef { pool_idx: key1, zero_for_one: false },
        ]);


        // Process a swap update that changes price in pool 0
        engine.process_swap_updates(
            &[V3SwapUpdate {
                pool_address: addr0,
                sqrt_price_x96: U256::from(79_466_191_966_197_645_195_421_774_833_u128),
                liquidity: 10_000_000_000_000,
                tick: 60,
                tick_priors: vec![],
            }],
            100,
        );

        // Results block should be updated
        let (_, block) = engine.latest_results();
        assert_eq!(block, 100);

        // Pool state should be updated
        let pool = &engine.pools[&key0];
        assert_eq!(pool.tick, 60);
        assert_eq!(pool.update_block, 100);
    }

    #[test]
    fn path_with_missing_pool_is_invalid() {
        let mut engine = V3BlockEngine::new();

        // Register only one pool
        let addr0 = Address::from([0x11u8; 20]);
        let mut tick_data0 = HashMap::new();
        tick_data0.insert(60, make_tick_info(200, 100));
        tick_data0.insert(-60, make_tick_info(150, -50));

        let key0 = engine.register_pool(RegisterV3PoolParams {
            address: addr0,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: tick_data0,
            update_block: 0,
            apply_buffer: true,
        });

        // Register a path that references a non-existent pool
        let path_id = engine.register_path(vec![
            V3PoolRef { pool_idx: key0, zero_for_one: true },
            V3PoolRef { pool_idx: 999, zero_for_one: false }, // Non-existent
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(!resolved.valid, "path with missing pool should be invalid");
    }

    #[test]
    fn process_block_with_no_logs_is_noop() {
        let mut engine = V3BlockEngine::new();

        let addr = Address::from([0x11u8; 20]);
        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));
        tick_data.insert(-60, make_tick_info(150, -50));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            apply_buffer: true,
        });


        // Process a block with no logs — pool state should be unchanged
        engine.process_block(&[], 50);

        let pool = &engine.pools[&key];
        assert_eq!(pool.tick, 0);
        assert_eq!(pool.update_block, 0); // Not updated

        let (_, block) = engine.latest_results();
        assert_eq!(block, 50);
    }

    #[test]
    fn multiple_pools_in_path_resolve_independently() {
        let mut engine = V3BlockEngine::new();

        let addr0 = Address::from([0x11u8; 20]);
        let addr1 = Address::from([0x22u8; 20]);

        let mut tick_data0 = HashMap::new();
        tick_data0.insert(-60, make_tick_info(200, -100));
        tick_data0.insert(60, make_tick_info(300, 150));

        let mut tick_data1 = HashMap::new();
        tick_data1.insert(-60, make_tick_info(250, -80));
        tick_data1.insert(60, make_tick_info(350, 120));

        let key0 = engine.register_pool(RegisterV3PoolParams {
            address: addr0,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: tick_data0,
            update_block: 0,
            apply_buffer: true,
        });

        let key1 = engine.register_pool(RegisterV3PoolParams {
            address: addr1,
            token0: Address::from([2u8; 20]),
            token1: Address::from([3u8; 20]),
            fee: 500,
            tick_spacing: 10,
            factory: Address::from([4u8; 20]),
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 2_000_000,
            tick: 0,
            tick_data: tick_data1,
            update_block: 0,
            apply_buffer: true,
        });

        let path_id = engine.register_path(vec![
            V3PoolRef { pool_idx: key0, zero_for_one: true },
            V3PoolRef { pool_idx: key1, zero_for_one: false },
        ]);

        let (_, resolved) = &engine.paths[&path_id];
        assert!(resolved.valid);

        // Both sequences should exist
        let seq0 = resolved.tick_range_sequences[0].as_ref().unwrap();
        let seq1 = resolved.tick_range_sequences[1].as_ref().unwrap();

        assert!(seq0.zero_for_one());
        assert!(!seq1.zero_for_one());

        // Different fees propagated
        assert!(!seq0.ranges.is_empty());
        assert!(!seq1.ranges.is_empty());
    }

    #[test]
    fn register_path_after_start_succeeds() {
        let mut engine = V3BlockEngine::new();
        let key1 = engine.register_pool(RegisterV3PoolParams {
            address: Address::from([1u8; 20]),
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });
        let key2 = engine.register_pool(RegisterV3PoolParams {
            address: Address::from([2u8; 20]),
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });
        engine.register_path(vec![
            V3PoolRef { pool_idx: key1, zero_for_one: true },
            V3PoolRef { pool_idx: key2, zero_for_one: false },
        ]);
        // Registration is always-on; this should not panic
        engine.register_path(vec![
            V3PoolRef { pool_idx: key1, zero_for_one: true },
            V3PoolRef { pool_idx: key2, zero_for_one: false },
        ]);
    }

    #[test]
    fn mint_buffered_for_unregistered_pool_applied_on_registration() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        // Apply a Mint update BEFORE the pool is registered
        engine.apply_liquidity_update(
            addr,
            -60,  // tick_lower
            60,   // tick_upper
            500,  // liquidity_delta (positive = Mint)
            100,  // block_number
        );

        // The event should be buffered, not dropped
        assert!(engine.liquidity_event_buffer.contains_key(&addr));
        assert_eq!(engine.liquidity_event_buffer[&addr].len(), 1);

        // Register the pool with tick_data from the DB snapshot (which
        // does NOT include the buffered event). The buffer will be
        // applied on top of this initial tick_data.
        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            apply_buffer: true,
        });

        // The buffer should be consumed (applied, not just discarded)
        assert!(engine.liquidity_event_buffer.is_empty());

        // The tick_data should reflect the Mint event applied on top
        let pool = &engine.pools[&key];
        assert!(pool.tick_data.contains_key(&-60));
        assert!(pool.tick_data.contains_key(&60));
        assert_eq!(pool.tick_data[&-60].liquidity_gross, U128::from(500u64));
        assert_eq!(pool.tick_data[&-60].liquidity_net, I256::try_from(500i128).unwrap());
        assert_eq!(pool.tick_data[&60].liquidity_gross, U128::from(500u64));
        assert_eq!(pool.tick_data[&60].liquidity_net, I256::try_from(-500i128).unwrap());
    }

    #[test]
    fn buffered_events_applied_on_registration() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x22u8; 20]);

        // Mint 500 at [-60, 60]
        engine.apply_liquidity_update(addr, -60, 60, 500, 100);
        // Then Burn 200 at [-60, 60]
        engine.apply_liquidity_update(addr, -60, 60, -200, 101);
        // Then Mint 300 at [-120, 120] (new tick positions)
        engine.apply_liquidity_update(addr, -120, 120, 300, 102);

        assert_eq!(engine.liquidity_event_buffer[&addr].len(), 3);

        // Register with DB snapshot tick_data that does NOT include these events.
        // The buffered events will be applied on top.
        // Starting from initial lg=100, ln=50 at tick -60 and lg=100, ln=-50 at tick 60:
        // After Mint 500 + Burn 200: -60: lg=100+500-200=400, ln=50+500-200=350
        // After Mint 300 at [-120,120]: -120: lg=0+300=300, ln=0+300=300; 120: lg=0+300=300, ln=0-300=-300
        let mut snapshot_tick_data = HashMap::new();
        snapshot_tick_data.insert(-60, make_tick_info(100, 50));
        snapshot_tick_data.insert(60, make_tick_info(100, -50));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::ONE,
            liquidity: 100,
            tick: 0,
            tick_data: snapshot_tick_data,
            update_block: 0,
            apply_buffer: true,
        });

        let pool = &engine.pools[&key];
        // Values should reflect snapshot + buffered events applied on top
        assert_eq!(pool.tick_data[&-60].liquidity_gross, U128::from(400u64));
        assert_eq!(pool.tick_data[&-60].liquidity_net, I256::try_from(350i128).unwrap());
        assert_eq!(pool.tick_data[&60].liquidity_gross, U128::from(400u64));
        assert_eq!(pool.tick_data[&60].liquidity_net, I256::try_from(-350i128).unwrap());
        assert!(pool.tick_data.contains_key(&-120));
        assert!(pool.tick_data.contains_key(&120));
    }

    #[test]
    fn buffer_max_age_expires_old_events() {
        let mut engine = V3BlockEngine::new_with_buffer_max_age(Some(10));
        let addr = Address::from([0x33u8; 20]);

        // Buffer an event at block 100
        engine.apply_liquidity_update(addr, -60, 60, 500, 100);

        // At block 110, the event is exactly at the boundary (110 - 10 = 100)
        engine.expire_buffered_events(110);
        assert!(engine.liquidity_event_buffer.contains_key(&addr));

        // At block 111, the event is too old (111 - 10 = 101 > 100)
        engine.expire_buffered_events(111);
        assert!(!engine.liquidity_event_buffer.contains_key(&addr));
    }

    #[test]
    fn flush_event_buffer_discards_all() {
        let mut engine = V3BlockEngine::new();
        let addr1 = Address::from([0x11u8; 20]);
        let addr2 = Address::from([0x22u8; 20]);

        engine.apply_liquidity_update(addr1, -60, 60, 500, 100);
        engine.apply_liquidity_update(addr2, -60, 60, 300, 101);

        assert_eq!(engine.liquidity_event_buffer.len(), 2);

        engine.flush_event_buffer();
        assert!(engine.liquidity_event_buffer.is_empty());
    }

    #[test]
    fn set_event_buffer_max_age_updates_limit() {
        let mut engine = V3BlockEngine::new(); // unbounded by default
        let addr = Address::from([0x44u8; 20]);

        engine.apply_liquidity_update(addr, -60, 60, 500, 100);

        // With no limit, event survives
        engine.expire_buffered_events(200);
        assert!(engine.liquidity_event_buffer.contains_key(&addr));

        // Set a limit that expires the event
        engine.set_event_buffer_max_age(Some(50));
        engine.expire_buffered_events(200);
        assert!(!engine.liquidity_event_buffer.contains_key(&addr));
    }

    #[test]
    fn double_swap_update_applies_both() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let mut tick_data = HashMap::new();
        tick_data.insert(60, make_tick_info(200, 100));
        tick_data.insert(-60, make_tick_info(150, -50));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        // Apply two swaps in the same block
        engine.apply_swap(
            addr,
            U256::from(79_466_191_966_197_645_195_421_774_833_u128),
            900_000,
            60,
            42,
            &[],
        );
        engine.apply_swap(
            addr,
            U256::from(79_714_513_003_271_600_568_814_636_800_u128), // Higher sqrt price
            800_000,
            120,
            42,
            &[],
        );

        // Last swap wins
        let pool = &engine.pools[&key];
        assert_eq!(pool.tick, 120);
        assert_eq!(pool.liquidity, 800_000);
    }

    // ── apply_liquidity_update tests ─────────────────────────────

    #[test]
    fn mint_updates_tick_data() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(100, 50));
        tick_data.insert(60, make_tick_info(100, -50));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        // Mint 500 liquidity from tick -60 to tick 60
        engine.apply_liquidity_update(addr, -60, 60, 500, 100);

        let pool = &engine.pools[&key];
        // tick_lower (-60): liquidity_gross += 500, liquidity_net += 500
        let tick_lower = &pool.tick_data[&-60];
        assert_eq!(tick_lower.liquidity_gross.to::<u128>(), 600); // 100 + 500
        assert_eq!(tick_lower.liquidity_net, I256::try_from(550i128).unwrap()); // 50 + 500

        // tick_upper (60): liquidity_gross += 500, liquidity_net += (-500)
        let tick_upper = &pool.tick_data[&60];
        assert_eq!(tick_upper.liquidity_gross.to::<u128>(), 600); // 100 + 500
        assert_eq!(tick_upper.liquidity_net, I256::try_from(-550i128).unwrap()); // -50 + (-500)
    }

    #[test]
    fn burn_updates_tick_data() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(500, 500));
        tick_data.insert(60, make_tick_info(500, -500));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        // Burn 200 liquidity from tick -60 to tick 60 (delta is negative)
        engine.apply_liquidity_update(addr, -60, 60, -200, 100);

        let pool = &engine.pools[&key];
        // tick_lower (-60): liquidity_gross += -200 → 300, liquidity_net += -200 → 300
        let tick_lower = &pool.tick_data[&-60];
        assert_eq!(tick_lower.liquidity_gross.to::<u128>(), 300); // 500 - 200
        assert_eq!(tick_lower.liquidity_net, I256::try_from(300i128).unwrap()); // 500 - 200

        // tick_upper (60): gross += -200 → 300, net += -(-200) = +200 → -300
        let tick_upper = &pool.tick_data[&60];
        assert_eq!(tick_upper.liquidity_gross.to::<u128>(), 300); // 500 - 200
        assert_eq!(tick_upper.liquidity_net, I256::try_from(-300i128).unwrap()); // -500 + 200
    }

    #[test]
    fn full_burn_removes_ticks_with_zero_gross() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        let mut tick_data = HashMap::new();
        tick_data.insert(-60, make_tick_info(100, 100));
        tick_data.insert(60, make_tick_info(100, -100));

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        // Burn 100 liquidity (the entire position) — gross goes to 0 at both ticks
        engine.apply_liquidity_update(addr, -60, 60, -100, 100);

        let pool = &engine.pools[&key];
        // Ticks with zero liquidity_gross are removed
        assert!(!pool.tick_data.contains_key(&-60));
        assert!(!pool.tick_data.contains_key(&60));
    }

    #[test]
    fn mint_initializes_new_tick() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        // Pool with no initialized ticks at -120/120
        let tick_data = HashMap::new();

        let key = engine.register_pool(RegisterV3PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
        update_block: 0,
        apply_buffer: true,
        });

        // Mint 300 liquidity from tick -120 to tick 120
        engine.apply_liquidity_update(addr, -120, 120, 300, 100);

        let pool = &engine.pools[&key];
        assert!(pool.tick_data.contains_key(&-120));
        assert!(pool.tick_data.contains_key(&120));
    }

    #[test]
    fn apply_liquidity_update_ignores_unregistered_pool() {
        let mut engine = V3BlockEngine::new();
        let addr = Address::from([0x11u8; 20]);

        // No pool registered — should be a no-op, not a panic
        engine.apply_liquidity_update(addr, -60, 60, 100, 1);
    }
}

// ---------------------------------------------------------------------------
// PyO3 wrapper — Python-accessible V3 block engine
// ---------------------------------------------------------------------------

use pyo3::prelude::*;
use pyo3::types::PyList;
use std::sync::Arc;

/// V3 block engine — Rust-centric arbitrage engine for Uniswap V3 paths.
///
/// Python constructs the engine (registers pools and paths), then starts
/// a Rust-side pump that drives the full per-block lifecycle. Python reads
/// results via `latest_results()`.
#[pyclass(name = "V3ArbEngine")]
pub struct PyV3ArbEngine {
    /// Shared engine state — Arc allows the pump to hold a reference too
    engine: Arc<parking_lot::Mutex<V3BlockEngine>>,
    /// Shutdown flag for the pump
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until `start()` is called)
    pump_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[pymethods]
impl PyV3ArbEngine {
    #[new]
    fn new() -> Self {
        Self {
            engine: Arc::new(parking_lot::Mutex::new(V3BlockEngine::new())),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
        }
    }

    /// Register a V3 pool by contract address and initial state.
    /// Returns the pool key for use in path registration.
    ///
    /// Args:
    ///     address: Pool contract address (hex string)
    ///     token0: Token0 address (hex string)
    ///     token1: Token1 address (hex string)
    ///     fee: Fee tier (e.g. 3000 for 0.3%)
    ///     `tick_spacing`: Tick spacing (e.g. 60)
    ///     `factory`: Factory address (hex string)
    ///     `sqrt_price_x96`: Current sqrt price (int)
    ///     `liquidity`: Current active liquidity (int)
    ///     `tick`: Current tick (int)
    ///     `tick_data`: Dict mapping tick index -> (`liquidity_gross`, `liquidity_net`)
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, tick_data, block=0, apply_buffer=true))]
    fn register_pool(
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
        tick_data: &Bound<'_, pyo3::types::PyDict>,
        block: u64,
        apply_buffer: bool,
    ) -> PyResult<u64> {
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

        let mut rust_tick_data = HashMap::new();
        for (key, value) in tick_data.iter() {
            let tick_idx: i32 = key.extract()?;
            let tuple = value.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let liquidity_gross: u128 = tuple.get_item(0)?.extract()?;
            let liquidity_net: i128 = tuple.get_item(1)?.extract()?;
            rust_tick_data.insert(tick_idx, make_tick_info_for_py(liquidity_gross, liquidity_net));
        }

        Ok(self.engine.lock().register_pool(RegisterV3PoolParams {
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
            apply_buffer,
        }))
    }

    /// Register a V3 arbitrage path. Returns the path ID.
    ///
    /// Args:
    ///     `pool_refs`: List of (`pool_key`, `zero_for_one`) tuples
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_idx: u64 = tuple.get_item(0)?.extract()?;
            let zero_for_one: bool = tuple.get_item(1)?.extract()?;
            rust_refs.push(V3PoolRef { pool_idx, zero_for_one });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_path(rust_refs))
    }

    /// Start the engine pump with the given RPC URL.
    /// Spawns the `V3EnginePump` on the Tokio runtime.
    #[pyo3(signature = (rpc_url))]
    fn start(&self, rpc_url: String) -> PyResult<()> {
        if self.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            let msg = "Engine already running";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        self.shutdown.store(false, std::sync::atomic::Ordering::Relaxed);

        let handle = crate::optimizers::v3_engine_pump::V3EnginePump::spawn(
            rpc_url,
            Arc::clone(&self.engine),
            &self.shutdown,
        )
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        *self.pump_handle.lock() = Some(handle);

        Ok(())
    }

    /// Stop the engine pump.
    fn stop(&self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        let handle = self.pump_handle.lock().take();
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.engine.lock().pool_count()
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Process Swap events synchronously (for testing without a subscription).
    ///
    /// Each entry is (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    /// where `tick_priors` is a list of (`tick_index`, (`liquidity_gross`, `liquidity_net`)).
    #[pyo3(signature = (swap_updates, block_number))]
    fn process_logs(
        &self,
        swap_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let mut rust_updates: Vec<V3SwapUpdate> = Vec::with_capacity(swap_updates.len());

        for item in swap_updates.iter() {
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

            // Parse tick_priors: list of (tick_index, (liquidity_gross, liquidity_net))
            let priors_obj = tuple.get_item(4)?;
            let priors_list = priors_obj.cast::<PyList>()?;
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
                tick_priors.push((tick_idx, make_tick_info_for_py(lg, ln)));
            }

            rust_updates.push(V3SwapUpdate {
                pool_address: addr,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }

        self.engine.lock().process_swap_updates(&rust_updates, block_number);
        Ok(())
    }

    /// Read the last solved results and block number.
    /// Returns (results, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    #[allow(clippy::significant_drop_tightening)]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        let (results, block_num) = {
            let engine = self.engine.lock();
            let (r, b) = engine.latest_results();
            (r.clone(), b)
        };

        let py_list = PyList::empty(py);
        for (path_id, optimal_input, profit) in results {
            py_list.append(path_id)?;
            let input_py = crate::alloy_py::PyU256(optimal_input).into_pyobject(py)?;
            py_list.append(input_py)?;
            let profit_py = crate::alloy_py::PyU256(profit).into_pyobject(py)?;
            py_list.append(profit_py)?;
        }

        Ok((py_list.unbind(), block_num))
    }
}

/// Helper to construct `TickInfo` from Python-extracted values.
fn make_tick_info_for_py(liquidity_gross: u128, liquidity_net: i128) -> TickInfo {
    use alloy::primitives::{I256, U128};
    TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
    }
}
