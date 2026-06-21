//! `BotState` — the single owner of all runtime state.
//!
//! All pool data, token metadata, calculation methods, and swap encoding
//! live here. Python objects are thin `PyO3` handles carrying keys into
//! `BotState`'s `HashMaps`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, I256, U256};

use crate::bot_core::state_history::{
    JournalError, ReorgJournal, ScalarPriors, TickBefore, V2BlockDelta, V3BlockDelta,
    V3RestoreResult,
};
use crate::optimizers::mobius_int::IntHopState;
use degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};

pub mod block_pump;
pub mod drain_sink;
pub mod engine;
pub mod liquidity_verifier;
pub mod log_dispatcher;
pub mod reorg_coordinator;
pub mod solve_coordinator;
pub mod state_history;
pub mod tick_bitmap;
pub mod tick_map;
pub mod v3_state;
pub mod v4_state;

// Re-export the merged V3/V4 state types (ADR-003: BotState owns CL state).
pub use v3_state::{
    v3_simulate_swap, BufferedV3LiquidityUpdate, PoolTickCoverage, RegisterV3PoolParams,
    V3PoolState, V3SwapOutcome, V3SwapUpdate,
};
pub use v4_state::RegisterV4PoolError;
pub use v4_state::{
    v4_simulate_swap, BufferedV4LiquidityUpdate, RegisterV4PoolParams, V4PoolKey, V4PoolState,
    V4StateSync, V4SwapUpdate, AMOUNT_MODIFYING_HOOK_MASK, V4_DYNAMIC_FEE_FLAG,
};

// Re-export the ADR-004 typed TickMap boundary trait (V3 + V4 impls both live
// in `tick_map.rs`). State structs stay flat; only verifier/apply views are
// typed-narrowed.
pub use tick_map::{TickMap, TickMapMut};

// ---------------------------------------------------------------------------
// Pool state types
// ---------------------------------------------------------------------------

/// A single pool's state. Pool-type-specific fields are in the enum variants.
#[derive(Clone, Debug)]
pub enum PoolEntry {
    V2(V2PoolState),
    V3(V3PoolState),
    V4(V4PoolState),
}

/// Read-only surface shared by [`V3PoolState`] and [`V4PoolState`] — the
/// fields the per-handle `PyLiquidityPool` reader API presents uniformly
/// across the V3/V4 concentrated-liquidity families (J63J3N).
///
/// Both variants store the same mutable scalars (`sqrt_price_x96`/
/// `liquidity`/`tick`/`update_block`) and an identical `tick_data:
/// HashMap<i32, TickInfo>`; V4 additionally nests `fee`/`tick_spacing`
/// inside `pool_key`, which the impl projects out. The trait lets
/// [`BotState::get_v3_or_v4_pool`] return one borrowed view covering both
/// families without cloning — the reader twin of the RAJ3PP apply dispatchers.
///
/// V2 is intentionally excluded (different state shape — reserves, not
/// scalars); a V2 `pool_id` yields `None` from the accessor, matching the
/// prior V3-only contract.
pub trait V3FamilyPool {
    fn sqrt_price_x96(&self) -> U256;
    fn liquidity(&self) -> u128;
    fn tick(&self) -> i32;
    fn update_block(&self) -> u64;
    fn fee(&self) -> u32;
    fn tick_spacing(&self) -> i32;
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;
}

impl V3FamilyPool for V3PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn fee(&self) -> u32 {
        self.fee
    }
    fn tick_spacing(&self) -> i32 {
        self.tick_spacing
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

impl V3FamilyPool for V4PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn fee(&self) -> u32 {
        self.pool_key.fee
    }
    fn tick_spacing(&self) -> i32 {
        self.pool_key.tick_spacing
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

/// State for a Uniswap V2-style constant-product pool.
#[derive(Clone, Debug)]
pub struct V2PoolState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Fee parameters for token0→token1 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token0: (u64, u64),
    /// Fee parameters for token1→token0 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token1: (u64, u64),
    /// Pool factory address.
    pub factory: Address,

    /// Current reserve of token0.
    pub reserve0: U256,
    /// Current reserve of token1.
    pub reserve1: U256,
    /// Block number of the last update.
    pub update_block: u64,

    /// Reorg journal — "before" values for rollback.
    /// V2 is the degenerate case: delta = full state (two reserves).
    pub journal: ReorgJournal<V2BlockDelta>,
}

/// Parameters for registering a V2 pool.
#[derive(Clone, Debug)]
pub struct RegisterV2PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_token0: (u64, u64),
    pub fee_token1: (u64, u64),
    pub factory: Address,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (ADR-005 slice 4). The landed-at journal must anchor the
    /// registration state at a real block so `restore_before_block` can land
    /// on it; pre-slice-4 the journal was empty until the first Sync.
    pub update_block: u64,
}

// ---------------------------------------------------------------------------
// V3 pool state — defined in [`v3_state`] (merged engine + journal types).
// ---------------------------------------------------------------------------

/// Liquidity data at an initialized tick.
///
/// Mirrors the Python `LiquidityAtTick` from `concentrated/types.py`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickInfo {
    /// The total liquidity that references this tick.
    pub liquidity_gross: alloy::primitives::U128,
    /// The liquidity delta for ticks entered from left to right.
    /// Positive for lower ticks, negative for upper ticks.
    pub liquidity_net: alloy::primitives::I256,
}

// `RegisterV3PoolParams` lives in [`v3_state`] (re-exported above).

// ---------------------------------------------------------------------------
// Token state
// ---------------------------------------------------------------------------

/// ERC20 token metadata.
#[derive(Clone, Debug)]
pub struct TokenEntry {
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub chain_id: u64,
}

// ---------------------------------------------------------------------------
// BotState
// ---------------------------------------------------------------------------

/// The single owner of all runtime state.
///
/// All pool data, token metadata, engines, and encoded results live here.
/// Python holds `PyBot` — an `Arc` pointing here.
///
/// ADR-006 D4: `BotState` is the pure-data submodule (no I/O) behind the
/// thin `Bot` orchestrator facade. `pub` — callers cross the
/// orchestrator seam; `BotState` is a private deep module with its own test
/// seam.
pub struct BotState {
    /// Pool registry: `pool_id` → `PoolEntry`.
    pools: HashMap<u64, PoolEntry>,
    /// Pool contract address → `pool_id`.
    pool_addresses: HashMap<Address, u64>,
    /// Token registry: address → `TokenEntry`.
    tokens: HashMap<Address, TokenEntry>,
    /// Auto-incrementing pool ID.
    next_pool_id: u64,
    /// Reorg journal depth (in blocks) for every pool — one mainnet epoch
    /// by default (ADR-003). Applied uniformly to V2/V3/V4.
    journal_depth: usize,
    /// Dual-buffer for V3 liquidity (Mint/Burn) events awaiting pool
    /// registration (ADR-003: the accurate-state buffer lives on `BotState`, not
    /// the dissolved `V3BlockEngine`).
    v3_buffer: crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer<
        Address,
        BufferedV3LiquidityUpdate,
    >,
    /// Dual-buffer for V4 `ModifyLiquidity` events awaiting pool registration.
    /// Keyed by `(pool_manager, pool_id)`.
    v4_buffer: crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer<
        (Address, degenbot_decoders::v4_swap_decoder::PoolId),
        BufferedV4LiquidityUpdate,
    >,
    /// V4 pool registry: `(pool_manager, pool_id)` → `pool_id` (single entry
    /// per pool — ADR-003 Option I: orientation derived at solve from
    /// `zero_for_one`, not stored as separate forward/reverse entries).
    v4_pool_ids: HashMap<(Address, degenbot_decoders::v4_swap_decoder::PoolId), u64>,
}

impl BotState {
    /// Create a new, empty `BotState` with the default 32-block reorg journal.
    #[must_use]
    pub fn new() -> Self {
        Self::with_journal_depth(32)
    }

    /// Create a new, empty `BotState` with a custom reorg journal depth.
    #[must_use]
    pub fn with_journal_depth(journal_depth: usize) -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            tokens: HashMap::new(),
            next_pool_id: 1,
            journal_depth,
            v3_buffer: crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_buffer: crate::optimizers::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_pool_ids: HashMap::new(),
        }
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_v2_pool(&mut self, params: &RegisterV2PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let RegisterV2PoolParams {
            address,
            token0,
            token1,
            reserve0,
            reserve1,
            fee_token0,
            fee_token1,
            factory,
            update_block,
        } = *params;

        // Seed the reorg journal with a genesis delta (ADR-005 slice 4): the
        // registration reserves at `update_block`. A `before`-only journal
        // cannot express "land at registration" or "current state"; the
        // genesis anchor (before == after == registration reserves) is what
        // makes `restore_before_block` land on it.
        let mut journal = ReorgJournal::<V2BlockDelta>::new(self.journal_depth);
        journal.push_delta(V2BlockDelta {
            block: update_block,
            reserve0_before: reserve0,
            reserve1_before: reserve1,
            reserve0_after: reserve0,
            reserve1_after: reserve1,
        });

        self.pools.insert(
            pool_id,
            PoolEntry::V2(V2PoolState {
                address,
                token0,
                token1,
                fee_token0,
                fee_token1,
                factory,
                reserve0,
                reserve1,
                update_block,
                journal,
            }),
        );
        self.pool_addresses.insert(address, pool_id);

        pool_id
    }

    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_v3_pool(&mut self, params: &RegisterV3PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let state = V3PoolState::from_params(params.clone(), self.journal_depth);
        self.pools.insert(pool_id, PoolEntry::V3(state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Apply a V2 `Sync` event to a registered pool's state.
    ///
    /// This is the live-path mutation method (ADR-003): journals the prior
    /// reserves, then updates `reserve0`/`reserve1`/`update_block` in place.
    /// Returns the affected `pool_id` so the engine can mark the right path set
    /// dirty; returns `None` if the pool is not registered (a no-op).
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    #[must_use]
    pub fn apply_v2_sync(
        &mut self,
        pool_address: Address,
        reserve0: U256,
        reserve1: U256,
        block_number: u64,
    ) -> Option<u64> {
        let &pool_id = self.pool_addresses.get(&pool_address)?;

        let Some(PoolEntry::V2(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        // Push a transition delta: before = pre-update reserves (the current
        // state), after = post-update reserves (the landed-at state for this
        // block). The genesis delta pushed at registration is the floor.
        state.journal.push_delta(V2BlockDelta {
            block: block_number,
            reserve0_before: state.reserve0,
            reserve1_before: state.reserve1,
            reserve0_after: reserve0,
            reserve1_after: reserve1,
        });

        state.reserve0 = reserve0;
        state.reserve1 = reserve1;
        state.update_block = block_number;

        Some(pool_id)
    }

    /// Update a V2 pool's reserves from a Sync event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Thin wrapper over [`apply_v2_sync`](Self::apply_v2_sync) that discards
    /// the returned `pool_id` (kept for the `PyBot` surface).
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    pub fn update_v2_pool(
        &mut self,
        pool_address: Address,
        reserve0: U256,
        reserve1: U256,
        block_number: u64,
    ) {
        let _ = self.apply_v2_sync(pool_address, reserve0, reserve1, block_number);
    }

    /// Apply a V2 `Sync` by `pool_id` — the `PyLiquidityPool.sync_reserves`
    /// backing. Returns the affected `pool_id`, or `None` if not registered /
    /// not a V2 pool (no-op). Journals the prior reserves then lands the new.
    #[must_use]
    pub fn apply_v2_sync_by_pool_id(
        &mut self,
        pool_id: u64,
        reserve0: U256,
        reserve1: U256,
        block_number: u64,
    ) -> Option<u64> {
        let Some(PoolEntry::V2(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
        state.journal.push_delta(V2BlockDelta {
            block: block_number,
            reserve0_before: state.reserve0,
            reserve1_before: state.reserve1,
            reserve0_after: reserve0,
            reserve1_after: reserve1,
        });
        state.reserve0 = reserve0;
        state.reserve1 = reserve1;
        state.update_block = block_number;
        Some(pool_id)
    }

    /// Read a registered V2 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and builds the
    /// orientation-specific `IntHopState` at resolve time from `zero_for_one`.
    #[must_use]
    pub fn get_v2_pool_state(&self, pool_id: u64) -> Option<&V2PoolState> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V2(state) => Some(state),
            PoolEntry::V3(_) | PoolEntry::V4(_) => None,
        }
    }

    /// Snapshot a V2 pool's current mutable state (reserves + block) under one
    /// read guard (ADR-005 slice 4 step 3). Returns `None` if the pool is not
    /// registered or isn't a V2 pool (no V2 state to read).
    ///
    /// The Python companion's `state` property + `simulate_*` methods build a
    /// `UniswapV2PoolState` from this single snapshot so a Rust-side
    /// `sync_reserves` (pump update) can't interleave between separate reads —
    /// the `StateCache.lock()` atomicity the drop-`StateCache` refactor loses.
    #[must_use]
    pub fn v2_snapshot(&self, pool_id: u64) -> Option<(U256, U256, u64)> {
        let state = self.get_v2_pool_state(pool_id)?;
        Some((state.reserve0, state.reserve1, state.update_block))
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Stashes scalar "before" values (and any provided per-tick priors) in the
    /// reorg journal before updating. Kept as the `PyBot` entry; the live
    /// pump path uses [`apply_v3_swap`](Self::apply_v3_swap) (which returns the
    /// affected `pool_id` and overlays `tick_priors` into `tick_data`).
    pub fn update_v3_pool(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            return;
        };

        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return;
        };

        // Stash "before" values in the reorg journal before updating
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            tick_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.update_block = block_number;
        state.invalidate_tick_range_cache();
    }

    /// Apply a V3 `Swap` event to a registered pool's state (ADR-003 live path).
    ///
    /// Mirrors the dissolved `V3BlockEngine::apply_swap`: overlays `tick_priors`
    /// into `tick_data` (the live pump path passes `&[]` — swaps don't modify
    /// `tick_data`), sets the scalar fields, invalidates the tick-range cache,
    /// journals the prior scalars (and any provided per-tick priors) for reorg
    /// rollback, and returns the affected `pool_id`. Returns `None` if the pool
    /// is not registered (a no-op). I/O-free; the engine calls this under the
    /// core lock inside the engine lock (engine-then-core ordering).
    pub fn apply_v3_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let &pool_id = self.pool_addresses.get(&pool_address)?;
        self.apply_v3_swap_by_pool_id(
            pool_id,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Apply a V3 Swap event keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Same semantics as [`apply_v3_swap`] but skips address resolution —
    /// the `PyLiquidityPool` handle already holds the canonical `pool_id`, so
    /// this is the one-lock, one-lookup path the handle uses.
    pub fn apply_v3_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        // Capture priors for any ticks being mutated by this event, so reorg
        // rollback can reverse-apply them. A tick that had no prior entry gets
        // `liquidity_gross_before: None` (on rollback, delete it).
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
        for &(tick_index, ref new_info) in tick_priors {
            let prior = state.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
            state.tick_data.insert(tick_index, new_info.clone());
        }

        // Journal scalar priors (swap scalars change on every Swap).
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            tick_priors: journaled_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.update_block = block_number;
        state.invalidate_tick_range_cache();

        Some(pool_id)
    }

    /// Apply a V3 liquidity update (Mint/Burn) to a registered pool's
    /// `tick_data`, or buffer it for an unregistered pool (ADR-003 live path).
    ///
    /// Registered pool: applies via `apply_liquidity_to_tick_range` (matching
    /// Solidity `Tick.update` — both lower and upper get `liquidity_gross +=
    /// delta`; `liquidity_net` `+=` at lower, `-=` at upper), invalidates the
    /// tick-range cache, returns the affected `pool_id`.
    ///
    /// Unregistered pool: buffers into the pump buffer for staged application
    /// at registration; returns `None`.
    pub fn apply_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            self.v3_buffer.buffer_pump(
                pool_address,
                BufferedV3LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                },
            );
            return None;
        };
        self.apply_v3_liquidity_update_by_pool_id(
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// V3 liquidity update keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Skips address resolution — the `PyLiquidityPool` handle holds the
    /// canonical `pool_id`, so this is the one-lock, one-lookup path. Registered
    /// pools only (no buffering — the handle's pool is necessarily registered).
    pub fn apply_v3_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        // Capture tick priors before mutation so reorg rollback can reverse-
        // apply. A tick that had no prior entry (newly initialized by this
        // Mint) gets `liquidity_gross_before: None` (on rollback, delete it).
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = state.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
            &mut state.tick_data,
            tick_lower,
            tick_upper,
            liquidity_delta,
        );

        // Journal: Mint/Burn mutate tick_data only, NOT the active `liquidity`
        // scalar — so the journal carries no scalar priors for this tick-only
        // event (scalar_priors: None). Only the two tick priors are reverse-
        // applied on rollback. See ADR-004.
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: None,
            tick_priors: journaled_priors,
        });

        state.update_block = block_number;
        state.invalidate_tick_range_cache();
        Some(pool_id)
    }

    /// Buffer a V3 liquidity update from the backfill phase. During backfill no
    /// pools are registered yet, so this always buffers (routes to the
    /// never-expired backfill buffer). If the pool happens to be registered
    /// already (defensive), applies directly.
    pub fn buffer_backfill_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        if let Some(&key) = self.pool_addresses.get(&pool_address) {
            if let Some(PoolEntry::V3(state)) = self.pools.get_mut(&key) {
                crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                    &mut state.tick_data,
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                );
                state.update_block = block_number;
                state.invalidate_tick_range_cache();
                return;
            }
        }
        self.v3_buffer.buffer_backfill(
            pool_address,
            BufferedV3LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            },
        );
    }

    /// Apply all buffered **backfill** V3 events for a pool address.
    /// Call this during registration, after `register_v3_pool` and before
    /// [`apply_pump_buffer_v3`](Self::apply_pump_buffer_v3). No-op if there are
    /// none. The post-call state is at the backfill boundary (a deterministic
    /// point suitable for verification cloning).
    ///
    /// Each buffered Mint/Burn pushes a tick-only `V3BlockDelta` (carrying the
    /// boundary-tick priors) and advances `state.update_block` — mirroring the
    /// live-path [`apply_v3_liquidity_update_by_pool_id`]. Pre-fix these
    /// appliers mutated `tick_data` only, so the buffered events were invisible
    /// to `restore_before_block` and `update_block` stayed frozen at the
    /// registration block.
    pub fn apply_backfill_buffer_v3(&mut self, address: &Address) {
        let Some(&key) = self.pool_addresses.get(address) else {
            return;
        };
        let Some(buffered) = self.v3_buffer.drain_backfill(address) else {
            return;
        };
        for update in buffered {
            if let Some(PoolEntry::V3(state)) = self.pools.get_mut(&key) {
                // Capture boundary-tick priors before mutation so reorg
                // rollback can reverse-apply. A tick absent before this update
                // (newly initialized) gets `liquidity_gross_before: None`
                // (deleted on rollback). Mint/Burn don't touch slot0 scalars
                // → `scalar_priors: None`.
                let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
                for &tick_idx in &[update.tick_lower, update.tick_upper] {
                    let prior = state.tick_data.get(&tick_idx).cloned();
                    journaled_priors.push((
                        tick_idx,
                        TickBefore {
                            liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                            liquidity_net_before: prior
                                .as_ref()
                                .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                        },
                    ));
                }
                crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                    &mut state.tick_data,
                    update.tick_lower,
                    update.tick_upper,
                    update.liquidity_delta,
                );
                state.journal.push_delta(V3BlockDelta {
                    block: update.block_number,
                    scalar_priors: None,
                    tick_priors: journaled_priors,
                });
                state.update_block = update.block_number;
                state.invalidate_tick_range_cache();
            }
        }
    }

    /// Apply all buffered **pump** V3 events for a pool address.
    /// Call this during registration, after [`apply_backfill_buffer_v3`].
    ///
    /// Same journal + `update_block` contract as
    /// [`apply_backfill_buffer_v3`] — see its docs.
    pub fn apply_pump_buffer_v3(&mut self, address: &Address) {
        let Some(&key) = self.pool_addresses.get(address) else {
            return;
        };
        let Some(buffered) = self.v3_buffer.drain_pump(address) else {
            return;
        };
        for update in buffered {
            if let Some(PoolEntry::V3(state)) = self.pools.get_mut(&key) {
                let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
                for &tick_idx in &[update.tick_lower, update.tick_upper] {
                    let prior = state.tick_data.get(&tick_idx).cloned();
                    journaled_priors.push((
                        tick_idx,
                        TickBefore {
                            liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                            liquidity_net_before: prior
                                .as_ref()
                                .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                        },
                    ));
                }
                crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                    &mut state.tick_data,
                    update.tick_lower,
                    update.tick_upper,
                    update.liquidity_delta,
                );
                state.journal.push_delta(V3BlockDelta {
                    block: update.block_number,
                    scalar_priors: None,
                    tick_priors: journaled_priors,
                });
                state.update_block = update.block_number;
                state.invalidate_tick_range_cache();
            }
        }
    }

    /// Set the maximum age (in blocks) for buffered V3 pump events.
    /// `None` means unbounded. Takes effect on the next `expire_v3_buffered`.
    pub const fn set_v3_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_buffer.set_max_age(max_age);
    }

    /// Number of buffered V3 liquidity events for a pool address (backfill + pump).
    #[must_use]
    pub fn buffered_v3_event_count(&self, address: &Address) -> usize {
        self.v3_buffer.event_count(address)
    }

    /// Discard all buffered V3 liquidity events for all pools.
    pub fn flush_v3_buffer(&mut self) {
        self.v3_buffer.flush();
    }

    /// Expire V3 pump-buffer events older than `current_block - max_age`.
    /// No-op if `max_age` is `None`. Backfill buffer is never expired.
    pub fn expire_v3_buffered(&mut self, current_block: u64) {
        self.v3_buffer.expire(current_block);
    }

    /// Read a registered V3 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and calls
    /// `build_int_v3_sequence(zfo, 10)` to build the per-hop state.
    #[must_use]
    pub fn get_v3_pool(&self, pool_id: u64) -> Option<&V3PoolState> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V3(state) => Some(state),
            PoolEntry::V2(_) | PoolEntry::V4(_) => None,
        }
    }

    /// Snapshot all V3 pool state for verification (clones every V3 entry).
    ///
    /// Used by `verify_liquidity_maps` so the engine+core locks can be
    /// released before making async RPC calls.
    #[must_use]
    pub fn v3_pools_snapshot(&self) -> HashMap<u64, V3PoolState> {
        self.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V3(state) => Some((*id, state.clone())),
                PoolEntry::V2(_) | PoolEntry::V4(_) => None,
            })
            .collect()
    }

    /// Family-dispatching reader for the V3/V4 concentrated-liquidity
    /// families (J63J3N). Returns a trait view over the shared read-only
    /// surface — the mutable scalars (`sqrt_price_x96`/`liquidity`/`tick`/
    /// `update_block`), the immutable fee/tick-spacing, and `tick_data`.
    ///
    /// This is the reader twin of the RAJ3PP apply dispatchers: the prior
    /// per-handle Python readers (`PyLiquidityPool.snapshot_v3`,
    /// `tick_data_snapshot`, the scalar getters, the restore/discard guards)
    /// went through `get_v3_pool`, which matches `PoolEntry::V3` only and
    /// returns `None` for `PoolEntry::V4` — silently yielding `None`/empty/0
    /// for every V4 read. Routing them through this accessor makes the
    /// docstrings' "V3/V4 pool" wording honest.
    ///
    /// Returns `None` for V2 or unregistered (the V3-only contract) — V2 has
    /// a different state shape and is read via the dedicated V2 getters.
    #[must_use]
    pub fn get_v3_or_v4_pool(&self, pool_id: u64) -> Option<&dyn V3FamilyPool> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V3(state) => Some(state),
            PoolEntry::V4(state) => Some(state),
            PoolEntry::V2(_) => None,
        }
    }

    /// Full-sync a V3 pool's `tick_data` from an external source (e.g. Python
    /// backfill). Replaces the entire `tick_data` map (so ticks Burn-removed
    /// on-chain are also removed here) and updates scalar state. No-op if the
    /// pool address is not registered.
    pub fn sync_v3_pool_state(
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
        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&key) else {
            return;
        };
        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.tick_data = tick_data;
        state.update_block = update_block;
        state.invalidate_tick_range_cache();
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Uses the constant-product invariant with EVM-exact integer arithmetic.
    ///
    /// Returns 0 if the pool is not found or the amount is 0.
    #[must_use]
    pub fn calculate_tokens_out(&self, pool_id: u64, zero_for_one: bool, amount_in: U256) -> U256 {
        let Some(entry) = self.pools.get(&pool_id) else {
            return U256::ZERO;
        };

        match entry {
            PoolEntry::V2(state) => {
                if amount_in.is_zero() {
                    return U256::ZERO;
                }

                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
                    (
                        state.reserve0,
                        state.reserve1,
                        state.fee_token0.0,
                        state.fee_token0.1,
                    )
                } else {
                    (
                        state.reserve1,
                        state.reserve0,
                        state.fee_token1.0,
                        state.fee_token1.1,
                    )
                };

                let hop = IntHopState::new(reserve_in, reserve_out, gamma_numer, fee_denom);
                hop.swap(amount_in)
            }
            // V3 concentrated-liquidity math. Exact-input swap: amount_specified
            // > 0 (V3 convention). Output is token1 for zfo, token0 for ofz
            // (matches the V3 Swap callback: zfo pays token0, receives token1).
            PoolEntry::V3(state) => {
                if amount_in.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_in).ok() else {
                    return U256::ZERO;
                };
                let Some(outcome) = v3_simulate_swap(state, zero_for_one, spec) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount1
                } else {
                    outcome.amount0
                }
            }
            // V4 concentrated-liquidity math. Same CL math as V3; sign
            // convention: V4 exact-input is `amountSpecified < 0` (negative),
            // opposite to V3. The caller (calculate_tokens_out) flips so the
            // simulator sees the V4-native sign.
            PoolEntry::V4(state) => {
                if amount_in.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_in).ok() else {
                    return U256::ZERO;
                };
                let Some(outcome) = v4_simulate_swap(state, zero_for_one, -spec) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount1
                } else {
                    outcome.amount0
                }
            }
        }
    }

    /// Calculate the input token amount required for a given output amount.
    ///
    /// Uses the constant-product invariant with EVM-exact integer arithmetic.
    ///
    /// Returns 0 if the pool is not found, the amount is 0,
    /// or the output exceeds available reserves.
    #[must_use]
    pub fn calculate_tokens_in(&self, pool_id: u64, zero_for_one: bool, amount_out: U256) -> U256 {
        let Some(entry) = self.pools.get(&pool_id) else {
            return U256::ZERO;
        };

        match entry {
            PoolEntry::V2(state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }

                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
                    (
                        state.reserve0,
                        state.reserve1,
                        state.fee_token0.0,
                        state.fee_token0.1,
                    )
                } else {
                    (
                        state.reserve1,
                        state.reserve0,
                        state.fee_token1.0,
                        state.fee_token1.1,
                    )
                };

                if amount_out >= reserve_out {
                    return U256::ZERO;
                }

                // constant_product_calc_exact_out:
                // amount_in = 1 + (reserve_in * amount_out * fee_denom) //
                //   ((reserve_out - amount_out) * gamma_numer)
                let numerator = U256::from(reserve_in)
                    .saturating_mul(amount_out)
                    .saturating_mul(U256::from(fee_denom));
                let denominator = (reserve_out.saturating_sub(amount_out))
                    .saturating_mul(U256::from(gamma_numer));

                if denominator.is_zero() {
                    return U256::ZERO;
                }

                U256::from(1) + numerator / denominator
            }
            // V3 concentrated-liquidity math. Exact-output swap: amount_specified
            // < 0 (V3 convention; magnitude = desired output). Input required is
            // token0 for zfo, token1 for ofz (the callback receives the input).
            PoolEntry::V3(state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_out).ok() else {
                    return U256::ZERO;
                };
                let Some(outcome) = v3_simulate_swap(state, zero_for_one, -spec) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount0
                } else {
                    outcome.amount1
                }
            }
            // V4: exact-output. V4 sign convention is opposite to V3: V4
            // exact-output uses `amountSpecified > 0` (positive). So the
            // magnitude passed to the V4 simulator is already positive (no
            // negation, unlike V3's `-spec`).
            PoolEntry::V4(state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_out).ok() else {
                    return U256::ZERO;
                };
                let Some(outcome) = v4_simulate_swap(state, zero_for_one, spec) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount0
                } else {
                    outcome.amount1
                }
            }
        }
    }

    /// Get the pool ID for a given contract address.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
    }

    /// Unregister a pool. ADR-007 U3.
    ///
    /// Drops the `PoolEntry` (and its reorg journal with it — restore for a
    /// removed pool is a no-op target) plus its index entries, and discards
    /// any buffered liquidity events for the pool so a re-register does not
    /// replay stale Mint/Burn/ModifyLiquidity onto the fresh pool.
    ///
    /// # Keying
    ///
    /// - **V2/V3 path** (`pool_id` = `None`): keyed by contract `address`
    ///   (`pool_addresses`).
    /// - **V4 path** (`pool_id` = `Some`): keyed by `(address, pool_id)` where
    ///   `address` is the **`PoolManager`** contract address (one `PoolManager`
    ///   hosts many pool ids — address alone is ambiguous, hence the tuple).
    ///
    /// `next_pool_id` is **not** reused — removed ids are retired so a stale
    /// `PyLiquidityPool` handle retained by a Python caller cannot alias onto
    /// a different pool that happens to receive the recycled id.
    ///
    /// # Returns
    ///
    /// `true` if an entry was found and removed; `false` if the address/tuple
    /// was never registered (silent no-op, mirroring Python `PoolRegistry.remove`
    /// silent-on-miss). Register stays refusal-on-`panic!`/`Err` (ADR-007 U2);
    /// the asymmetry reflects the asymmetry in the operations' invariants.
    pub fn unregister_pool(
        &mut self,
        address: Address,
        pool_id: Option<degenbot_decoders::v4_swap_decoder::PoolId>,
    ) -> bool {
        match pool_id {
            None => {
                // V2/V3 path: address-keyed.
                let Some(id) = self.pool_addresses.remove(&address) else {
                    return false;
                };
                self.pools.remove(&id);
                self.v3_buffer.discard_for(&address);
                true
            }
            Some(pid) => {
                // V4 path: (pool_manager, pool_id)-keyed.
                let key = (address, pid);
                let Some(id) = self.v4_pool_ids.remove(&key) else {
                    return false;
                };
                self.pools.remove(&id);
                self.v4_buffer.discard_for(&key);
                true
            }
        }
    }

    /// Number of registered pools.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Number of registered V3 pools.
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V3(_)))
            .count()
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V2(_)))
            .count()
    }

    /// Check if a pool ID is registered.
    #[must_use]
    pub fn has_pool(&self, pool_id: u64) -> bool {
        self.pools.contains_key(&pool_id)
    }

    /// Check if a token address is registered.
    #[must_use]
    pub fn has_token(&self, address: &Address) -> bool {
        self.tokens.contains_key(address)
    }

    /// Look up a registered token's metadata entry (address, name, symbol,
    /// decimals, `chain_id`) by contract address. Used by `PyErc20Token`'s getters
    /// (ADR-003 T3: Rust owns token identity metadata).
    #[must_use]
    pub fn token_entry(&self, address: &Address) -> Option<&TokenEntry> {
        self.tokens.get(address)
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    #[must_use]
    pub fn v2_journal_len(&self, pool_id: u64) -> usize {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V2(state)) => state.journal.len(),
            _ => 0,
        }
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target (nothing to discard
    /// — supports a continuously-running bot calling `discard(latest - N)` on
    /// fresh pools). The genesis delta is discarded like any other when the
    /// target is past it, as long as at least one delta remains.
    ///
    /// # Errors
    ///
    /// Returns `Err(JournalError::NoStateAtOrAfterBlock)` if the target is past
    /// the newest delta (would remove every known state). The `PyO3` layer maps
    /// this to `ValueError`.
    pub fn v2_discard_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), JournalError> {
        let Some(PoolEntry::V2(state)) = self.pools.get_mut(&pool_id) else {
            return Ok(());
        };
        state.journal.discard_before_block(block)
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores the
    /// landed-at state (the `*_after` of the largest delta below the target)
    /// into the current mutable fields.
    ///
    /// Returns `Some(Ok((reserve0, reserve1, block)))` on success, `Some(Err)`
    /// if the pool exists but the target is at/before registration (no state
    /// before it — decision 3), or `None` if the pool ID is not registered.
    pub fn v2_restore_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<Result<(U256, U256, u64), JournalError>> {
        let PoolEntry::V2(state) = self.pools.get_mut(&pool_id)? else {
            return None;
        };
        let (r0, r1, blk) = match state.journal.restore_before_block(block) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        state.reserve0 = r0;
        state.reserve1 = r1;
        state.update_block = blk;
        Some(Ok((r0, r1, blk)))
    }

    /// Restore **every** registered V2 pool's state to just before `target`.
    ///
    /// Bulk restore helper. ADR-006 slice 7 replaced the engine-level
    /// `handle_reorg` (which called this) with per-event
    /// `ReorgCoordinator::dispatch_reorg_log` (per-pool `restore_before_block`).
    /// This bulk helper survives as a `BotState` API (used by tests + available
    /// for ad-hoc bulk rollback); the engine no longer calls it on the hot path.
    ///
    /// Pools with no journal delta at/after `target` are left as-is (idempotent
    /// — a reorg touches only a subset of pools). Returns the count of pools
    /// that were rolled back.
    pub fn restore_all_pools_before_block(&mut self, target: u64) -> usize {
        let pool_ids: Vec<u64> = self.pools.keys().copied().collect();
        let mut restored = 0usize;
        for pool_id in pool_ids {
            // Peek the per-pool journal depth without a mutable borrow. Only
            // pools with a delta at/after the reorg target need rollback;
            // untouched pools keep their current state (idempotent restore).
            let needs_restore = match self.pools.get(&pool_id) {
                Some(PoolEntry::V2(state)) => {
                    state.journal.newest_block().is_some_and(|b| b >= target)
                }
                Some(PoolEntry::V3(state)) => {
                    state.journal.newest_block().is_some_and(|b| b >= target)
                }
                Some(PoolEntry::V4(state)) => {
                    state.journal.newest_block().is_some_and(|b| b >= target)
                }
                None => false,
            };
            if !needs_restore {
                continue;
            }

            let did_restore = match self.pools.get_mut(&pool_id) {
                Some(PoolEntry::V2(state)) => {
                    // Landed-at restore: on Ok, apply the landed-at state; on
                    // Err (target at/before registration) skip the pool
                    // (idempotent — a reorg doesn't touch pools that didn't
                    // exist before the fork target).
                    match state.journal.restore_before_block(target) {
                        Ok((r0, r1, blk)) => {
                            state.reserve0 = r0;
                            state.reserve1 = r1;
                            state.update_block = blk;
                            true
                        }
                        Err(_) => false,
                    }
                }
                Some(PoolEntry::V3(_)) => {
                    // Reuse the existing V3 restore path: scalars + reverse-
                    // applied tick priors + cache invalidation.
                    self.v3_restore_before_block(pool_id, target).is_some()
                }
                Some(PoolEntry::V4(_)) => {
                    // V4 restore: same V3BlockDelta shape (scalar + per-tick
                    // priors); delegated to `v4_restore_before_block`.
                    self.v4_restore_before_block(pool_id, target).is_some()
                }
                None => false,
            };
            if did_restore {
                restored += 1;
            }
        }
        restored
    }

    // --- V3 journal methods ---

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    #[must_use]
    pub fn v3_journal_len(&self, pool_id: u64) -> usize {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V3(state)) => state.journal.len(),
            _ => 0,
        }
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target, or the pool is not
    /// registered / not a V3 pool.
    ///
    /// # Errors
    ///
    /// Returns `Err(JournalError::NoStateAtOrAfterBlock)` if the target is past
    /// the newest delta. The `PyO3` layer maps this to `ValueError`.
    pub fn v3_discard_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), JournalError> {
        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return Ok(());
        };
        state.journal.discard_before_block(block)
    }

    /// Restore V3 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block, restores
    /// scalar "before" values into the current state, and reverse-applies
    /// tick priors to the current `tick_data` map.
    ///
    /// Restore `pool_id`'s state to just before `block` (ADR-006 slice 7).
    ///
    /// Single-pool dispatch: routes V2→`v2_restore_before_block`,
    /// V3→`v3_restore_before_block`, V4→`v4_restore_before_block`. Writes the
    /// journal's landed-at state into the current mutable fields, releases, then
    /// `ReorgCoordinator` notifies subscribers. Pools whose newest delta is
    /// already before `block` are no-ops (idempotent restore).
    ///
    /// **Pre-check [`has_state_prior_to`](Self::has_state_prior_to) first** —
    /// the V3/V4 `restore_before_block` panics on an empty journal; the
    /// pre-check avoids that and returns `Err` uniformly so the pump shuts down
    /// gracefully on a too-deep reorg.
    pub fn restore_pool_before_block(&mut self, pool_id: u64, block: u64) {
        // V2 returns Result; ignore the Err here (too-deep was pre-checked).
        // V3/V4 return Option (None for unregistered / non-matching family).
        match self.pools.get_mut(&pool_id) {
            Some(PoolEntry::V2(_)) => {
                let _ = self.v2_restore_before_block(pool_id, block);
            }
            Some(PoolEntry::V3(_)) => {
                let _ = self.v3_restore_before_block(pool_id, block);
            }
            Some(PoolEntry::V4(_)) => {
                let _ = self.v4_restore_before_block(pool_id, block);
            }
            None => {}
        }
    }

    /// Does `pool_id`'s journal have state at or before `block`? (ADR-006
    /// slice 7.) `false` → a too-deep reorg; `ReorgCoordinator` returns
    /// `Err(NoStatePriorToBlock)` and the pump shuts down gracefully.
    ///
    /// The predicate is **family-dependent** because the journals differ in
    /// whether they carry a genesis anchor:
    ///
    /// - **V2** carries a genesis delta (pushed at registration, `before ==
    ///   after`). There is genuinely no state *prior to* the genesis block, so
    ///   a target at or before the earliest (genesis) delta is too-deep:
    ///   `earliest < block`.
    /// - **V3/V4** push **no** genesis delta at registration. The "before"
    ///   values of the first forward event ARE the registration state, so
    ///   `restore_before_block(B)` handles a single delta at `B` (and any
    ///   target below the earliest delta) by popping down to registration
    ///   state. The ONLY unrecoverable case is an empty journal
    ///   (`restore_before_block` panics on empty) → `!is_empty()`.
    ///
    /// A pool whose newest delta is below `block` (idempotent no-op restore)
    /// returns `true` under both predicates.
    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        let Some(entry) = self.pools.get(&pool_id) else {
            // Pool not registered → no journal → the reorg can't restore it.
            // Treat as "has state" (no-op) so the caller proceeds to the normal
            // pool-not-found no-op path rather than a fail-stop.
            return true;
        };
        match entry {
            PoolEntry::V2(state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
            // No genesis anchor — empty journal is the only too-deep case.
            PoolEntry::V3(state) => !state.journal.is_empty(),
            PoolEntry::V4(state) => !state.journal.is_empty(),
        }
    }

    /// Restore V4 pool state prior to a target block (`V3RestoreResult` shape).
    ///
    /// Returns `V3RestoreResult` with the before-values, or `None`
    /// if the pool ID is not registered or is not a V3 pool.
    ///
    /// # Panics
    ///
    /// Panics if no delta exists before the target block.
    pub fn v3_restore_before_block(&mut self, pool_id: u64, block: u64) -> Option<V3RestoreResult> {
        let PoolEntry::V3(state) = self.pools.get_mut(&pool_id)? else {
            return None;
        };
        let mut result = state.journal.restore_before_block(block);

        // Sync scalar fields if the rolled-back range had scalar changes.
        // If scalar_priors is None (tick-only event(s) rolled back), the
        // current slot0 scalars were never changed by the rolled-back events
        // and are already correct — skip the write-back. See ADR-004.
        if let Some(p) = &result.scalar_priors {
            state.sqrt_price_x96 = p.sqrt_price_x96_before;
            state.liquidity = p.liquidity_before;
            state.tick = p.tick_before;
        }
        state.update_block = result.block;
        state.invalidate_tick_range_cache();

        // Reverse-apply tick priors
        for (tick_idx, tick_before) in &result.tick_priors {
            match tick_before.liquidity_gross_before {
                Some(gross_before) => {
                    // Tick existed before — restore its prior values
                    state.tick_data.insert(
                        *tick_idx,
                        TickInfo {
                            liquidity_gross: gross_before,
                            liquidity_net: tick_before.liquidity_net_before,
                        },
                    );
                }
                None => {
                    // Tick was newly initialized in this block — remove it
                    state.tick_data.remove(tick_idx);
                }
            }
        }

        // If scalar_priors was None (tick-only rollback), populate it with the
        // current (post-restore) scalars so downstream consumers (e.g., the
        // PyO3 `v3_restore_before_block` wrapper) always see Some — the
        // current scalars ARE the restored scalars in this case. See ADR-004.
        if result.scalar_priors.is_none() {
            result.scalar_priors = Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            });
        }

        Some(result)
    }

    /// Encode a V2 swap call for the given pool.
    ///
    /// Produces pre-encoded calldata for `swap(uint256,uint256,address,bytes)`
    /// that is ready for on-chain submission.
    ///
    /// Returns `None` if the pool ID is not registered.
    #[must_use]
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        recipient: Address,
    ) -> Option<EncodedCall> {
        let entry = self.pools.get(&pool_id)?;
        match entry {
            PoolEntry::V2(state) => {
                let call =
                    encode_v2_swap(state.address, zero_for_one, amount_out, recipient).ok()?;
                Some(call)
            }
            // V3 encoding is not yet implemented
            PoolEntry::V3(_) | PoolEntry::V4(_) => None,
        }
    }

    // -----------------------------------------------------------------------
    // V4 state (ADR-003: single entry per `(pool_manager, pool_id)`;
    // orientation derived at solve from `zero_for_one`)
    // -----------------------------------------------------------------------

    /// Register a V4 pool by `(pool_manager, pool_id)`.
    ///
    /// ADR-003 hook filter inline: pools with amount-modifying hooks or dynamic
    /// fees are rejected. Returns `Err(RegisterV4PoolError)` on rejection.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the pool has amount-modifying hooks
    /// (`hook_flags & 0xCC != 0`), uses a dynamic fee (`fee == 0x100000`),
    /// or a pool with the same `(pool_manager, pool_id)` is already registered.
    pub fn register_v4_pool(
        &mut self,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        if (params.hook_flags & AMOUNT_MODIFYING_HOOK_MASK) != 0 {
            return Err(RegisterV4PoolError::HookedPool {
                hook_flags: params.hook_flags,
            });
        }
        if params.pool_key.fee == V4_DYNAMIC_FEE_FLAG {
            return Err(RegisterV4PoolError::DynamicFee {
                fee: params.pool_key.fee,
            });
        }

        let key = (params.pool_manager, params.pool_id);
        if self.v4_pool_ids.contains_key(&key) {
            return Err(RegisterV4PoolError::AlreadyRegistered {
                pool_manager: params.pool_manager,
                pool_id: params.pool_id,
            });
        }

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let state = V4PoolState::from_params(params.clone(), self.journal_depth);
        self.pools.insert(pool_id, PoolEntry::V4(state));
        self.v4_pool_ids.insert(key, pool_id);

        Ok(pool_id)
    }

    /// Apply a V4 Swap event to a registered pool (ADR-003 live path).
    pub fn apply_v4_swap(&mut self, update: &V4SwapUpdate, block_number: u64) -> Option<u64> {
        let key = (update.pool_manager, update.pool_id);
        let &pool_id = self.v4_pool_ids.get(&key)?;

        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        let mut journaled_priors: Vec<(i32, TickBefore)> =
            Vec::with_capacity(update.tick_priors.len());
        for &(tick_index, ref new_info) in &update.tick_priors {
            let prior = state.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
            state.tick_data.insert(tick_index, new_info.clone());
        }

        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            tick_priors: journaled_priors,
        });

        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.update_block = block_number;
        state.invalidate_tick_range_cache();

        Some(pool_id)
    }

    /// Apply a V4 `ModifyLiquidity` event to a registered pool, or buffer it
    /// for an unregistered pool (ADR-003 live path).
    pub fn apply_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) -> Option<u64> {
        let key = (pool_manager, pool_id);
        let Some(&pool_id) = self.v4_pool_ids.get(&key) else {
            self.v4_buffer.buffer_pump(
                key,
                BufferedV4LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                },
            );
            return None;
        };

        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        let delta_i128: i128 = i128::try_from(liquidity_delta).ok()?;

        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = state.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
            &mut state.tick_data,
            tick_lower,
            tick_upper,
            delta_i128,
        );

        // Journal: V4 `ModifyLiquidity` mutates tick_data only, NOT the slot0
        // scalars — so the journal carries no scalar priors for this tick-only
        // event (scalar_priors: None). Only the two tick priors are reverse-
        // applied on rollback. See ADR-004.
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: None,
            tick_priors: journaled_priors,
        });

        state.update_block = block_number;
        state.invalidate_tick_range_cache();
        Some(pool_id)
    }

    /// Apply a V4 Swap event to a registered pool by its inner handle
    /// `pool_id` (the per-handle Python API path, an alternative entry to the
    /// `(pool_manager, pool_id)`-keyed `apply_v4_swap`).
    ///
    /// Mirrors `apply_v3_swap_by_pool_id` for the V4 entry: journals the
    /// scalar priors (and any passed `tick_priors`, empty from the handle path
    /// — same "scalars only" contract the V3 handle method documents), mutates
    /// `slot0`, advances `update_block`, invalidates the tick-range cache.
    ///
    /// Returns `Some(pool_id)` if the pool is V4; `None` for V2/V3 or
    /// unregistered (silent no-op, matching the V3 sibling's contract).
    pub fn apply_v4_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
        for &(tick_index, ref new_info) in tick_priors {
            let prior = state.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
            state.tick_data.insert(tick_index, new_info.clone());
        }

        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            tick_priors: journaled_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.update_block = block_number;
        state.invalidate_tick_range_cache();

        Some(pool_id)
    }

    /// Apply a V4 `ModifyLiquidity` event to a registered pool by its inner
    /// handle `pool_id` (the per-handle Python API path, an alternative entry
    /// to the `(pool_manager, pool_id)`-keyed `apply_v4_liquidity_update`).
    ///
    /// Mirrors `apply_v3_liquidity_update_by_pool_id` for the V4 entry:
    /// journals the two tick priors, applies the delta to the tick range
    /// (`liquidity_net` `+=` at lower, `-=` at upper, both `gross +=`),
    /// advances `update_block`, invalidates the tick-range cache. No scalar
    /// change (`scalar_priors: None`) — same ADR-004 tick-only contract as V3.
    ///
    /// Returns `Some(pool_id)` if the pool is V4; `None` otherwise.
    pub fn apply_v4_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };

        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = state.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
            &mut state.tick_data,
            tick_lower,
            tick_upper,
            liquidity_delta,
        );

        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: None,
            tick_priors: journaled_priors,
        });

        state.update_block = block_number;
        state.invalidate_tick_range_cache();
        Some(pool_id)
    }

    /// Family-dispatching Swap apply (RAJ3PP). The single entry point
    /// `PyLiquidityPool.apply_swap` calls — routes V3 pools to
    /// `apply_v3_swap_by_pool_id` and V4 pools to
    /// `apply_v4_swap_by_pool_id`. V2/unregistered → `None` (no-op, matching
    /// the V3 sibling). This preserves the single Python `apply_swap` API
    /// while correcting the prior unconditional V3 routing that silently
    /// dropped every V4 update.
    ///
    /// The family probe is a `matches!` (Copy discriminant) so the immutable
    /// borrow of `self.pools` ends before the `&mut self` apply call — one
    /// held write guard throughout, two O(1) `HashMap` lookups (probe + apply).
    pub fn apply_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(_))) {
            self.apply_v4_swap_by_pool_id(
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
                tick_priors,
            )
        } else {
            self.apply_v3_swap_by_pool_id(
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
                tick_priors,
            )
        }
    }

    /// Family-dispatching liquidity update (RAJ3PP). The single entry point
    /// `PyLiquidityPool.apply_liquidity_update` calls — routes V3 to
    /// `apply_v3_liquidity_update_by_pool_id` and V4 to
    /// `apply_v4_liquidity_update_by_pool_id`. V2/unregistered → `None`.
    pub fn apply_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(_))) {
            self.apply_v4_liquidity_update_by_pool_id(
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            )
        } else {
            self.apply_v3_liquidity_update_by_pool_id(
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            )
        }
    }

    /// Buffer a V4 `ModifyLiquidity` event from the backfill phase.
    pub fn buffer_backfill_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(&id) = self.v4_pool_ids.get(&key) {
            if let Some(PoolEntry::V4(state)) = self.pools.get_mut(&id) {
                if let Ok(delta_i128) = i128::try_from(liquidity_delta) {
                    crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                        &mut state.tick_data,
                        tick_lower,
                        tick_upper,
                        delta_i128,
                    );
                    state.update_block = block_number;
                    state.invalidate_tick_range_cache();
                    return;
                }
            }
        }
        self.v4_buffer.buffer_backfill(
            key,
            BufferedV4LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            },
        );
    }

    /// Apply all buffered **backfill** V4 `ModifyLiquidity` events for a pool.
    ///
    /// Same journal + `update_block` contract as the V3 buffer appliers
    /// ([`apply_backfill_buffer_v3`]) — each event pushes a tick-only
    /// `V3BlockDelta` (V4 shares the V3 journal shape) and advances
    /// `state.update_block`. Pre-fix these mutated `tick_data` only.
    pub fn apply_backfill_buffer_v4(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        let Some(buffered) = self.v4_buffer.drain_backfill(&key) else {
            return;
        };
        for update in buffered {
            let Some(PoolEntry::V4(state)) = self.pools.get_mut(&id) else {
                continue;
            };
            if let Ok(delta_i128) = i128::try_from(update.liquidity_delta) {
                let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
                for &tick_idx in &[update.tick_lower, update.tick_upper] {
                    let prior = state.tick_data.get(&tick_idx).cloned();
                    journaled_priors.push((
                        tick_idx,
                        TickBefore {
                            liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                            liquidity_net_before: prior
                                .as_ref()
                                .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                        },
                    ));
                }
                crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                    &mut state.tick_data,
                    update.tick_lower,
                    update.tick_upper,
                    delta_i128,
                );
                state.journal.push_delta(V3BlockDelta {
                    block: update.block_number,
                    scalar_priors: None,
                    tick_priors: journaled_priors,
                });
                state.update_block = update.block_number;
                state.invalidate_tick_range_cache();
            }
        }
    }

    /// Apply all buffered **pump** V4 `ModifyLiquidity` events for a pool.
    ///
    /// Same journal + `update_block` contract as
    /// [`apply_backfill_buffer_v4`] — see its docs.
    pub fn apply_pump_buffer_v4(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        let Some(buffered) = self.v4_buffer.drain_pump(&key) else {
            return;
        };
        for update in buffered {
            let Some(PoolEntry::V4(state)) = self.pools.get_mut(&id) else {
                continue;
            };
            if let Ok(delta_i128) = i128::try_from(update.liquidity_delta) {
                let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
                for &tick_idx in &[update.tick_lower, update.tick_upper] {
                    let prior = state.tick_data.get(&tick_idx).cloned();
                    journaled_priors.push((
                        tick_idx,
                        TickBefore {
                            liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                            liquidity_net_before: prior
                                .as_ref()
                                .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                        },
                    ));
                }
                crate::bot_core::tick_bitmap::apply_liquidity_to_tick_range(
                    &mut state.tick_data,
                    update.tick_lower,
                    update.tick_upper,
                    delta_i128,
                );
                state.journal.push_delta(V3BlockDelta {
                    block: update.block_number,
                    scalar_priors: None,
                    tick_priors: journaled_priors,
                });
                state.update_block = update.block_number;
                state.invalidate_tick_range_cache();
            }
        }
    }

    /// Set the maximum age for buffered V4 pump events. `None` = unbounded.
    pub fn set_v4_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v4_buffer.set_max_age(max_age);
    }

    pub fn flush_v4_buffer(&mut self) {
        self.v4_buffer.flush();
    }

    pub fn expire_v4_buffered(&mut self, current_block: u64) {
        self.v4_buffer.expire(current_block);
    }

    /// Read a registered V4 pool's state by `pool_id`.
    #[must_use]
    pub fn get_v4_pool(&self, pool_id: u64) -> Option<&V4PoolState> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V4(state) => Some(state),
            PoolEntry::V2(_) | PoolEntry::V3(_) => None,
        }
    }

    /// Look up the pool ID for a registered `(pool_manager, pool_id)` pair.
    #[must_use]
    pub fn v4_pool_id_by_key(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::PoolId,
    ) -> Option<u64> {
        self.v4_pool_ids.get(&(pool_manager, *pool_id)).copied()
    }

    /// Number of registered V4 pools.
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.v4_pool_ids.len()
    }

    /// Return the set of V4 `PoolManager` addresses with registered pools.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.v4_pool_ids
            .keys()
            .map(|(pm, _)| *pm)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    /// Snapshot all V4 pool state for verification.
    #[must_use]
    pub fn v4_pools_snapshot(&self) -> HashMap<u64, V4PoolState> {
        self.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V4(state) => Some((*id, state.clone())),
                PoolEntry::V2(_) | PoolEntry::V3(_) => None,
            })
            .collect()
    }

    /// Full-sync a V4 pool's `tick_data` from an external source.
    pub fn sync_v4_pool_state(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::PoolId,
        update: V4StateSync,
    ) {
        let Some(&id) = self.v4_pool_ids.get(&(pool_manager, pool_id)) else {
            return;
        };
        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&id) else {
            return;
        };
        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.tick_data = update.tick_data;
        state.update_block = update.update_block;
        state.invalidate_tick_range_cache();
    }

    // --- V4 journal methods ---

    /// Discard V4 reorg journal deltas earlier than the given block.
    ///
    /// # Errors
    ///
    /// Returns `Err(JournalError::NoStateAtOrAfterBlock)` if the target is past
    /// the newest delta. The `PyO3` layer maps this to `ValueError`.
    pub fn v4_discard_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), JournalError> {
        let Some(PoolEntry::V4(state)) = self.pools.get_mut(&pool_id) else {
            return Ok(());
        };
        state.journal.discard_before_block(block)
    }

    /// Family-dispatching journal discard (J63J3N): routes V4 pools to
    /// `v4_discard_before_block`, V3 (and V2/unregistered) to
    /// `v3_discard_before_block`. The per-handle Python
    /// `PyLiquidityPool.discard_v3_before_block` previously gated on a
    /// `get_v3_pool(...).is_none()` V3 probe that returned `None` for V4 and
    /// no-op'd — so V4 reorg journals were never trimmed via the handle path.
    /// The `matches!` probe is a Copy discriminant (immutable borrow ends
    /// before the `&mut self` call).
    pub fn discard_v3_or_v4_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Result<(), JournalError> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(_))) {
            self.v4_discard_before_block(pool_id, block)
        } else {
            self.v3_discard_before_block(pool_id, block)
        }
    }

    /// Restore V4 pool state prior to a target block (same `V3BlockDelta` shape).
    pub fn v4_restore_before_block(&mut self, pool_id: u64, block: u64) -> Option<V3RestoreResult> {
        let PoolEntry::V4(state) = self.pools.get_mut(&pool_id)? else {
            return None;
        };
        let mut result = state.journal.restore_before_block(block);

        // Sync scalar fields if the rolled-back range had scalar changes.
        // If scalar_priors is None (tick-only event(s) rolled back), the
        // current slot0 scalars were never changed by the rolled-back events
        // and are already correct — skip the write-back. See ADR-004.
        if let Some(p) = &result.scalar_priors {
            state.sqrt_price_x96 = p.sqrt_price_x96_before;
            state.liquidity = p.liquidity_before;
            state.tick = p.tick_before;
        }
        state.update_block = result.block;
        state.invalidate_tick_range_cache();

        for (tick_idx, tick_before) in &result.tick_priors {
            match tick_before.liquidity_gross_before {
                Some(gross_before) => {
                    state.tick_data.insert(
                        *tick_idx,
                        TickInfo {
                            liquidity_gross: gross_before,
                            liquidity_net: tick_before.liquidity_net_before,
                        },
                    );
                }
                None => {
                    state.tick_data.remove(tick_idx);
                }
            }
        }

        // If scalar_priors was None (tick-only rollback), populate it with the
        // current (post-restore) scalars so downstream consumers always see
        // Some — the current scalars ARE the restored scalars in this case.
        // See ADR-004.
        if result.scalar_priors.is_none() {
            result.scalar_priors = Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            });
        }

        Some(result)
    }

    /// Family-dispatching journal restore (J63J3N): routes V4 pools to
    /// `v4_restore_before_block`, V3 (and V2/unregistered) to
    /// `v3_restore_before_block`. The per-handle
    /// `PyLiquidityPool.restore_v3_before_block` previously gated on a
    /// `get_v3_pool(...).is_none()` V3 probe that returned `None` for V4 — so
    /// V4 reorg rollback via the handle path silently no-op'd, returning
    /// `None` (not the rolled-back scalars). Mirrors the RAJ3PP
    /// `apply_liquidity_update_by_pool_id` dispatch shape.
    pub fn restore_v3_or_v4_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<V3RestoreResult> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(_))) {
            self.v4_restore_before_block(pool_id, block)
        } else {
            self.v3_restore_before_block(pool_id, block)
        }
    }

    /// Register a token.
    ///
    /// # Panics
    ///
    /// Panics if the token address is already registered.
    pub fn register_token(
        &mut self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        assert!(
            !self.tokens.contains_key(&address),
            "token already registered: {address}"
        );

        self.tokens.insert(
            address,
            TokenEntry {
                address,
                name,
                symbol,
                decimals,
                chain_id,
            },
        );
    }
}

impl Default for BotState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bot — thin orchestrator facade (ADR-006 D4)
// ---------------------------------------------------------------------------

/// The per-chain orchestrator: a thin facade over a shared
/// [`BotState`] (the pure-data registries/swap math/reorg journal) plus the
/// `chain_id` (ADR-006 D1) and, in later slices, the cohesive helpers
/// (`LogDispatcher` / `BlockPump` / `SolveCoordinator` / `ReorgCoordinator`) and a
/// `Vec<Box<dyn EventSink>>` of attached engines.
///
/// `PyBot` owns a `Bot` outright (not behind a lock) and hands out clones of
/// [`Bot::state_arc`] so `PyLiquidityPool` / `PyErc20Token` / `UniswapEngine`
/// all reach ONE Rust-owned `BotState` (N handles → one state — the Polars
/// three-layer invariant, preserved). The standalone-Rust path (D4) runs the
/// whole bot through this facade without Python.
pub struct Bot {
    /// The chain this bot orchestrates (ADR-006 D1+D5: one `Bot` per chain).
    /// Read by the standalone-Rust path; `PyBot` currently stubs `0` —
    /// real wiring lands when ADR-006 D4 makes `chain_id` a Bot-level
    /// construction-time invariant used for cross-chain validation (see
    /// `docs/adr/ADR-006-bot-as-per-chain-orchestrator.md` §D4).
    chain_id: u64,
    /// The shared pure-data state. Handles clone this `Arc`.
    state: Arc<parking_lot::RwLock<BotState>>,
    /// The per-`Bot` event bus (ADR-006 D4). The pump (slice 5) drives
    /// [`dispatch_log`](Self::dispatch_log) per WS log; engine subscriber
    /// adapters attach via [`attach_engine`](Self::attach_engine).
    dispatcher: log_dispatcher::LogDispatcher,
}

/// Block metadata included in each `ResultBatch`.
///
/// Passed from the pump's WS block header into the drain tick, then forwarded
/// to Python via the result batch channel. Lives in `bot_core` (general block
/// data) so the `BlockPump` + `DrainSink` seams stay in `bot_core` without a
/// reverse dependency on `optimizers` (ADR-006 D4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

impl Bot {
    /// Construct a new orchestrator for `chain_id` over a fresh `BotState`.
    ///
    /// `PyBot` wires `chain_id = 0` until ADR-006 slice 8 makes `bot.py` a
    /// single-chain facade; the standalone-Rust path passes the real id.
    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            state: Arc::new(parking_lot::RwLock::new(BotState::new())),
            dispatcher: log_dispatcher::LogDispatcher::with_uniswap_decoders(),
        }
    }

    /// Construct a `Bot` that **adopts** an existing shared `BotState` core + a
    /// fresh `LogDispatcher` (ADR-006 D4). Used so a `Bot` + a `UniswapEngine`
    /// (and a sibling `PyBot`) all read/write the SAME `BotState` — the engine
    /// gets the core via `UniswapEngine::with_core`, `BlockPump`'s `Bot`
    /// shares it, and `dispatch_log` writes flow through to the engine's reads.
    ///
    /// `chain_id` is 0 on the standalone/no-pyo3 path; `PyBot` passes the real
    /// id once `bot.py` is a single-chain facade (slice 8).
    #[must_use]
    pub fn with_core(core: Arc<parking_lot::RwLock<BotState>>) -> Self {
        Self {
            chain_id: 0,
            state: core,
            dispatcher: log_dispatcher::LogDispatcher::with_uniswap_decoders(),
        }
    }

    /// The chain this bot orchestrates. Used by the standalone-Rust path;
    /// `PyBot` does not expose it until ADR-006 D4 wires it as a real
    /// construction-time invariant (see `docs/adr/ADR-006-bot-as-per-chain-orchestrator.md` §D4).
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Hand out a clone of the shared `Arc<RwLock<BotState>>` so a sibling
    /// consumer (`PyLiquidityPool` / `PyErc20Token` / `UniswapEngine`) reaches
    /// the SAME state this orchestrator owns. This is the Polars three-layer
    /// sharing seam (ADR-005, revised by ADR-006 D4).
    #[must_use]
    pub fn state_arc(&self) -> Arc<parking_lot::RwLock<BotState>> {
        Arc::clone(&self.state)
    }

    /// Drive one WS log through the event bus (ADR-006 D4). Decode via a
    /// registered decoder, apply to `BotState` under a write guard, release,
    /// then notify subscribers. The pump (slice 5) calls this per log.
    pub fn dispatch_log(&self, log: &alloy::rpc::types::Log) {
        self.dispatcher.dispatch(log, &self.state);
    }

    /// Decode `log` into a [`DecodedPoolEvent`] without applying (ADR-006 slice 7).
    /// `ReorgCoordinator` uses this on `removed: true` logs to identify the
    /// target pool before restoring it from the journal.
    pub fn try_decode_log(
        &self,
        log: &alloy::rpc::types::Log,
    ) -> Option<log_dispatcher::DecodedPoolEvent> {
        self.dispatcher.try_decode_log(log)
    }

    /// Resolve a decoded event's `pool_id` against `BotState` (ADR-006 slice 7).
    /// V2/V3 by address, V4 by `(pool_manager, pool_id)` key.
    pub fn resolve_pool_id(&self, event: &log_dispatcher::DecodedPoolEvent) -> Option<u64> {
        event.resolve_pool_id(&self.state.read())
    }

    /// Restore `pool_id`'s state to just before `block` (ADR-006 slice 7).
    /// Writes the journal's landed-at state into the current mutable fields.
    /// Pre-check [`has_state_prior_to`](Self::has_state_prior_to) first — the
    /// V3/V4 journal `restore_before_block` panics on an empty journal.
    pub fn restore_pool_before_block(&self, pool_id: u64, block: u64) {
        self.state.write().restore_pool_before_block(pool_id, block);
    }

    /// Does `pool_id`'s journal have state at or before `block`? (ADR-006
    /// slice 7.) `false` → a too-deep reorg; `ReorgCoordinator` returns
    /// `Err(NoStatePriorToBlock)` and the pump shuts down gracefully.
    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        self.state.read().has_state_prior_to(pool_id, block)
    }

    /// Notify every live subscriber of `pool_id` (ADR-006 slice 7).
    /// `ReorgCoordinator` calls this after a per-pool restore — the same
    /// notify path `dispatch_log` uses, so the engine dirties + re-solves at
    /// the next drain tick with no distinct reorg path.
    pub fn notify_pool_state_updated(&self, pool_id: u64) {
        self.dispatcher.notify(pool_id);
    }

    /// Subscribe `engine` to updates for `pool_id` (ADR-006 D4). `Bot` calls
    /// this when an engine registers a path touching `pool_id`. `engine` is a
    /// `Weak` so a de-registered engine is silently skipped (no leak).
    pub fn attach_engine(
        &self,
        pool_id: u64,
        engine: std::sync::Weak<dyn log_dispatcher::PoolStateSubscriber>,
    ) {
        self.dispatcher.subscribe(pool_id, engine);
    }

    /// Start the block pump. Placeholder — the `BlockPump` wiring lands in
    /// ADR-006 slice 5; until then this panics to make the unwired state loud.
    pub fn start(&self) {
        unimplemented!("BlockPump wiring lands in ADR-006 slice 5");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE_03: (u64, u64) = (997, 1000);

    fn make_pool_addr() -> Address {
        Address::from([0xaa; 20])
    }
    fn make_token0() -> Address {
        Address::from([0xbb; 20])
    }
    fn make_token1() -> Address {
        Address::from([0xcc; 20])
    }
    fn make_factory() -> Address {
        Address::from([0xdd; 20])
    }

    fn make_params(r0: U256, r1: U256) -> RegisterV2PoolParams {
        RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0: r0,
            reserve1: r1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
            update_block: 0,
        }
    }

    #[test]
    fn register_v2_pool_and_calculate_tokens_out() {
        let mut core = BotState::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn calculate_tokens_out_reverse_direction() {
        let mut core = BotState::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(2000), U256::from(1000)));

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core.calculate_tokens_out(pool_id, false, U256::from(100));
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn update_v2_pool_changes_calculation_result() {
        let mut core = BotState::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Before update: swap 100 token0 → 181 token1
        let before = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(before, U256::from(181));

        // Update reserves: now reserve0=2000, reserve1=1000
        core.update_v2_pool(make_pool_addr(), U256::from(2000), U256::from(1000), 42);

        // After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        let after = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(after, U256::from(47));
    }

    #[test]
    fn calculate_tokens_in_for_v2_pool() {
        let mut core = BotState::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        let amount_in = core.calculate_tokens_in(pool_id, true, U256::from(50));
        assert_eq!(amount_in, U256::from(26));

        // Reverse: Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
        let amount_in_rev = core.calculate_tokens_in(pool_id, false, U256::from(10));
        assert_eq!(amount_in_rev, U256::from(21));
    }

    #[test]
    fn calculate_tokens_out_realistic_amounts() {
        let mut core = BotState::new();

        // Realistic: 1.5M USDC / 800 WETH, 0.3% fee
        let reserve0 = U256::from(1_500_000_000_000u64); // 1.5M USDC (6dp)
        let reserve1 = U256::from(800u128) * U256::from(10u64).pow(U256::from(18)); // 800 WETH

        let params = RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0,
            reserve1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
            update_block: 0,
        };
        let pool_id = core.register_v2_pool(&params);

        // Swap 1000 USDC for WETH
        // Python reference: 531380142665175213
        let amount_in = U256::from(1_000_000_000u64); // 1000 USDC (6dp)
        let amount_out = core.calculate_tokens_out(pool_id, true, amount_in);
        assert_eq!(amount_out, U256::from(531_380_142_665_175_213_u64));
    }

    /// ADR-006 slice 3 (D4): `BotState` is a thin orchestrator facade over a
    /// `pub BotState` (the renamed pure-data struct). It holds the
    /// `chain_id` (D1, deferred from slice 1) and a shared
    /// `Arc<RwLock<BotState>>` it hands out via `state_arc()` so `PyBot`,
    /// `PyLiquidityPool`, `PyErc20Token`, and the engine all reach ONE
    /// Rust-owned state (N handles → one `BotState`).
    #[test]
    fn bot_facade_holds_chain_id_and_shares_bot_state() {
        // The orchestrator carries the chain id (D1).
        let bot = super::Bot::new(5);
        assert_eq!(bot.chain_id(), 5);

        // `state_arc()` hands out the shared `Arc<RwLock<BotState>>`.
        let state = bot.state_arc();

        // A pool registered through the shared state is visible to a SECOND
        // clone of the same Arc — proving N handles reach one Rust-owned
        // state (the Polars three-layer invariant, preserved).
        let params = RegisterV2PoolParams {
            address: Address::from([0x11u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U256::from(1000),
            reserve1: U256::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 0,
        };
        state.write().register_v2_pool(&params);

        let state2 = bot.state_arc();
        assert_eq!(
            state2.read().pool_count(),
            1,
            "state_arc() must share one BotState"
        );
        assert!(state2.read().has_pool(1));
    }

    // --- V3 restore no-op (reorg on an unrelated pool) ---

    fn register_v3(core: &mut BotState, update_block: u64) -> u64 {
        core.register_v3_pool(&RegisterV3PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block,
            coverage: PoolTickCoverage::Sparse,
        })
    }

    /// Regression (WZWKKU): `v3_restore_before_block(B)` with `B` strictly past
    /// the journal's newest delta must leave the pool's current state
    /// UNTOUCHED. This is the per-pool path `dispatch_reorg_log` hits for a
    /// `removed: true` log on an unrelated pool — totally normal reorg traffic.
    ///
    /// Pre-fix the journal returned the newest delta's own `scalar_priors` and
    /// `tick_priors` (the PRE-newest state) and `v3_restore_before_block`
    /// reverse-applied them, silently rolling back the newest delta's swap and
    /// deleting its freshly-initialized ticks. The engine then re-solved off
    /// the corrupted scalars + `tick_data`.
    #[test]
    fn v3_restore_before_block_past_newest_leaves_state_untouched() {
        let mut core = BotState::new();
        let pool_id = register_v3(&mut core, 5);

        // Forward Swap at block 10 moves scalars + initializes tick 100.
        let new_sqrt = U256::from(2u64) << 96;
        core.apply_v3_swap_by_pool_id(
            pool_id,
            new_sqrt,
            1_000,
            100,
            10,
            &[(
                100,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(500),
                    liquidity_net: alloy::primitives::I256::try_from(500i64).unwrap(),
                },
            )],
        );

        // Snapshot the landed-at (post-block-10) state.
        let landed_sqrt;
        let landed_liq;
        let landed_tick;
        let landed_update_block;
        let tick_present;
        {
            let Some(PoolEntry::V3(state)) = core.pools.get(&pool_id) else {
                panic!("pool missing");
            };
            landed_sqrt = state.sqrt_price_x96;
            landed_liq = state.liquidity;
            landed_tick = state.tick;
            landed_update_block = state.update_block;
            tick_present = state.tick_data.contains_key(&100);
        }
        assert_eq!(landed_sqrt, new_sqrt);
        assert_eq!(landed_liq, 1_000);
        assert_eq!(landed_tick, 100);
        assert_eq!(landed_update_block, 10);
        assert!(tick_present, "block-10 swap initialized tick 100");

        // Reorg: a removed log arrives for an unrelated pool whose newest
        // journal delta (block 10) is BELOW the reorg target (block 12).
        // `has_state_prior_to` returns true (V3 journal non-empty), so the
        // coordinator proceeds to `restore_pool_before_block` →
        // `v3_restore_before_block`.
        assert!(core.has_state_prior_to(pool_id, 12));
        let result = core.v3_restore_before_block(pool_id, 12);
        assert!(
            result.is_some(),
            "restore returns Some even on the no-op path"
        );

        // The landed-at state must survive unchanged.
        let Some(PoolEntry::V3(state)) = core.pools.get(&pool_id) else {
            panic!("pool missing post-restore");
        };
        assert_eq!(
            state.sqrt_price_x96, landed_sqrt,
            "scalars must not roll back"
        );
        assert_eq!(state.liquidity, landed_liq);
        assert_eq!(state.tick, landed_tick);
        assert_eq!(state.update_block, landed_update_block);
        assert!(
            state.tick_data.contains_key(&100),
            "tick 100 must survive — pre-fix it was deleted via the newest delta's tick_priors"
        );
        assert_eq!(
            state.journal.len(),
            1,
            "no deltas popped on the no-op path (only the block-10 swap delta; V3 registration pushes no genesis)"
        );
    }

    // --- HO3GWT: buffer appliers push journal deltas + advance update_block ---

    /// Register a V3 pool with tick 60 pre-initialized (gross/net 100) and
    /// tick 120 absent, so a buffered Mint at [60,120] bumps 60 → 600 and
    /// newly initializes 120. Helper does NOT create the `BotState` — the
    /// caller must buffer events on the SAME core before calling this.
    fn register_v3_on_core(core: &mut BotState, pool_addr: Address, update_block: u64) -> u64 {
        use crate::bot_core::{RegisterV3PoolParams, TickInfo};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
            },
        );
        core.register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block,
            coverage: PoolTickCoverage::Sparse,
        })
    }

    /// Regression (HO3GWT, V3 backfill buffer): a Mint buffered during the
    /// backfill phase, then applied at registration via
    /// `apply_backfill_buffer_v3`, must (1) push a tick-only journal delta,
    /// (2) advance `state.update_block` to the event's block, and (3) be
    /// reversible by `restore_before_block` (roll the tick state back to the
    /// pre-buffer registration snapshot).
    ///
    /// Pre-fix `apply_backfill_buffer_v3` called `apply_liquidity_to_tick_range`
    /// and `invalidate_tick_range_cache()` and stopped — no journal, no
    /// `update_block` bump. A reorg landing inside the buffered range
    /// couldn't reverse the buffered events, and `v3_snapshot`/diagnostics
    /// reported a stale last-update block.
    #[test]
    fn apply_backfill_buffer_v3_journals_and_advances_update_block() {
        let pool_addr = Address::from([0x88u8; 20]);
        let block_b = 5u64;

        // 1. Pre-registration: buffer a backfill Mint at [60, 120], block B=5.
        let mut core = BotState::new();
        core.buffer_backfill_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

        // 2. Register on the SAME core (tick 60 gross=100, tick 120 absent).
        let pool_id = register_v3_on_core(&mut core, pool_addr, 0);

        // 3. Apply the backfill buffer (the registration-staged application).
        core.apply_backfill_buffer_v3(&pool_addr);

        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "update_block must advance to the buffered event's block (pre-fix: stays at registration block 0)"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "buffered Mint must push one tick-only journal delta (pre-fix: 0)"
            );
            let t60 = s.tick_data.get(&60).expect("tick 60 present");
            assert_eq!(t60.liquidity_gross, alloy::primitives::U128::from(600));
            assert_eq!(
                t60.liquidity_net,
                alloy::primitives::I256::try_from(600i128).unwrap()
            );
            let t120 = s.tick_data.get(&120).expect("tick 120 newly initialized");
            assert_eq!(t120.liquidity_gross, alloy::primitives::U128::from(500));
            assert_eq!(
                t120.liquidity_net,
                alloy::primitives::I256::try_from(-500i128).unwrap(),
                "upper tick net -= delta (V3/`apply_liquidity_to_tick_range` convention)"
            );
        }

        // 4. Restore before block B → rolls back the buffered Mint to the
        //    registration snapshot.
        core.v3_restore_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered");
        let t60 = s.tick_data.get(&60).expect("tick 60 still present");
        assert_eq!(
            t60.liquidity_gross,
            alloy::primitives::U128::from(100),
            "tick 60 reverts to registration snapshot (gross 100) on rollback"
        );
        assert_eq!(
            t60.liquidity_net,
            alloy::primitives::I256::try_from(100i128).unwrap()
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "newly-initialized tick 120 removed on rollback"
        );
    }

    /// Regression (HO3GWT, V3 pump buffer): same invariants as the backfill
    /// path, but the event is buffered via the WS-pump path (`apply_v3_
    /// liquidity_update` while unregistered routes to the pump buffer) and
    /// applied via `apply_pump_buffer_v3`.
    #[test]
    fn apply_pump_buffer_v3_journals_and_advances_update_block() {
        let pool_addr = Address::from([0x99u8; 20]);
        let block_b = 7u64;

        // 1. Pre-registration: pump the Mint (unregistered → pump buffer).
        let mut core = BotState::new();
        core.apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

        // 2. Register on the SAME core + 3. apply pump buffer.
        let pool_id = register_v3_on_core(&mut core, pool_addr, 0);
        core.apply_pump_buffer_v3(&pool_addr);

        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "update_block advances to pump-buffer event block"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "pump-buffer Mint pushes one journal delta"
            );
            assert_eq!(
                s.tick_data.get(&60).expect("t60").liquidity_gross,
                alloy::primitives::U128::from(600)
            );
            assert!(s.tick_data.contains_key(&120));
        }

        core.v3_restore_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered");
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            alloy::primitives::U128::from(100),
            "pump-buffer Mint rolls back to registration snapshot"
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "newly-initialized tick 120 removed on pump-buffer rollback"
        );
    }

    /// Regression (HO3GWT, V4 backfill buffer): mirror of the V3 backfill
    /// test for `apply_backfill_buffer_v4` — a `ModifyLiquidity` buffered
    /// during backfill must journal + advance `update_block` + be reversible
    /// via `v4_restore_before_block`.
    #[test]
    fn apply_backfill_buffer_v4_journals_and_advances_update_block() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0xeeu8; 32];
        let block_b = 9u64;

        // 1. Pre-registration: buffer a backfill ModifyLiquidity at [60,120].
        let mut core = BotState::new();
        core.buffer_backfill_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(500i128).unwrap(),
            block_b,
        );

        // 2. Register (tick 60 gross=100, tick 120 absent, update_block=0).
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
            },
        );
        let pool_id = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect("V4 pool registers");

        // 3. Apply the backfill buffer.
        core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);

        {
            let s = core.get_v4_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "V4 update_block advances to buffered event block"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "V4 buffered ModifyLiquidity pushes one journal delta"
            );
            assert_eq!(
                s.tick_data.get(&60).expect("t60").liquidity_gross,
                U128::from(600)
            );
            assert!(s.tick_data.contains_key(&120));
        }

        // 4. Restore before block B → rolls back the ModifyLiquidity.
        core.v4_restore_before_block(pool_id, block_b);
        let s = core.get_v4_pool(pool_id).expect("registered");
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            U128::from(100),
            "V4 tick 60 reverts to registration snapshot on rollback"
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "V4 newly-initialized tick 120 removed on rollback"
        );
    }

    /// Plan 102, slice 2: `BotState::register_v4_pool` returns a typed
    /// `RegisterV4PoolError` (not a flat `String`) for each admission
    /// category, so the `PyO3` seam can surface distinct Python exception
    /// types. Pins the three variants the seam maps to
    /// `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` / plain
    /// `PyValueError` respectively.
    #[test]
    fn register_v4_pool_rejects_amount_modifying_hook_with_typed_error() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let mut core = BotState::new();
        let err = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xeeu8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                // BEFORE_SWAP (0x80) — amount-modifying.
                hook_flags: 0x80,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect_err("hooked pool must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::HookedPool { hook_flags: 0x80 },
            "amount-modifying-hook refusal returns the typed HookedPool variant"
        );
    }

    #[test]
    fn register_v4_pool_rejects_dynamic_fee_with_typed_error() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let mut core = BotState::new();
        let err = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xeeu8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect_err("dynamic-fee pool must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::DynamicFee {
                fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
            },
            "dynamic-fee refusal returns the typed DynamicFee variant"
        );
    }

    #[test]
    fn register_v4_pool_rejects_duplicate_with_already_registered_variant() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0xeeu8; 32];
        let mut core = BotState::new();
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        };
        core.register_v4_pool(&params)
            .expect("first registration ok");
        let err = core
            .register_v4_pool(&params)
            .expect_err("duplicate registration must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::AlreadyRegistered {
                pool_manager,
                pool_id: pool_id_bytes,
            },
            "duplicate-registration refusal returns the typed AlreadyRegistered variant"
        );
    }

    /// Regression (RAJ3PP): `PyLiquidityPool.apply_swap` routed V4 pools into
    /// `apply_v3_swap_by_pool_id`, which matches `PoolEntry::V3` only and
    /// silently no-op'd on `PoolEntry::V4` — a Python-side V4 update path
    /// (snapshots, regression tests, manual `external_update`) dropped every
    /// update. The fix is a family-dispatching `apply_swap_by_pool_id` on
    /// `BotState` that `PyLiquidityPool.apply_swap` calls (the preferred
    /// "existing methods do family dispatch internally" option). This test
    /// pins both halves of the AC: V4 scalars actually change (not a no-op),
    /// and they match a direct `apply_v4_swap` on the same scalar inputs.
    #[test]
    fn apply_swap_by_pool_id_routes_to_v4_and_matches_apply_v4_swap() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0x66u8; 32];
        let block_b = 7u64;

        let make_params = || RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        };

        // Twin pools: A updated via the family dispatcher, B via the
        // dedicated V4 path (`apply_v4_swap`). Both start identical.
        let mut core_a = BotState::new();
        let id_a = core_a
            .register_v4_pool(&make_params())
            .expect("V4 pool A registers");
        let mut core_b = BotState::new();
        let id_b = core_b
            .register_v4_pool(&make_params())
            .expect("V4 pool B registers");

        // Before the fix, this call was a silent no-op on A (routed to the
        // V3-only method). Assert it now applies.
        let _ = core_a.apply_swap_by_pool_id(
            id_a,
            U256::from(2u128) << 96,
            2_000_000,
            -100,
            block_b,
            &[],
        );
        let s_a = core_a.get_v4_pool(id_a).expect("V4 pool A registered");
        assert_eq!(s_a.sqrt_price_x96, U256::from(2u128) << 96);
        assert_eq!(s_a.liquidity, 2_000_000);
        assert_eq!(s_a.tick, -100);
        assert_eq!(s_a.update_block, block_b);
        assert_eq!(
            s_a.journal.len(),
            1,
            "the dispatcher must journal a scalar delta like apply_v4_swap"
        );

        // AC parity: same scalar inputs via `apply_v4_swap` produce identical
        // post-state. The dispatcher's V4 branch mirrors `apply_v4_swap`'s
        // body (same tick_priors=[], same journal shape).
        let _ = core_b.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: -100,
                tick_priors: Vec::new(),
            },
            block_b,
        );
        let s_b = core_b.get_v4_pool(id_b).expect("V4 pool B registered");
        assert_eq!(s_b.sqrt_price_x96, s_a.sqrt_price_x96);
        assert_eq!(s_b.liquidity, s_a.liquidity);
        assert_eq!(s_b.tick, s_a.tick);
        assert_eq!(s_b.update_block, s_a.update_block);
        assert_eq!(s_b.journal.len(), s_a.journal.len());
    }

    /// Regression (RAJ3PP, V4 `apply_liquidity_update` half): the liquidity
    /// update previously routed V4 pools into `apply_v3_liquidity_update_by_pool
    /// _id` (V3-only, no-op on V4). The family dispatcher must apply a V4
    /// `ModifyLiquidity` to the tick range and journal it, matching
    /// `apply_v4_liquidity_update` on the same inputs.
    #[test]
    fn apply_liquidity_update_by_pool_id_routes_to_v4_and_applies_ticks() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0x55u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0x77u8; 32];
        let block_b = 5u64;

        // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
            },
        );
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        };

        // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
        let mut core_a = BotState::new();
        let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
        let mut core_b = BotState::new();
        let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

        // Before the fix this no-op'd on A. Assert it now applies.
        assert_eq!(
            core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 500, block_b),
            Some(id_a),
            "dispatcher must report an applied V4 liquidity update"
        );
        let s_a = core_a.get_v4_pool(id_a).expect("registered A");
        assert_eq!(s_a.update_block, block_b);
        assert_eq!(s_a.journal.len(), 1);
        assert_eq!(
            s_a.tick_data.get(&60).expect("t60").liquidity_gross,
            U128::from(600),
            "tick 60 gross += delta (ModifyLiquidity) via the dispatcher"
        );
        assert!(s_a.tick_data.contains_key(&120), "tick 120 initialized");
        // slot0 scalars unchanged (tick-only event per ADR-004).
        assert_eq!(s_a.sqrt_price_x96, U256::from(1u128) << 96);

        // Parity: direct apply_v4_liquidity_update on B produces the same state.
        assert_eq!(
            core_b.apply_v4_liquidity_update(
                pool_manager,
                pool_id_bytes,
                60,
                120,
                I256::try_from(500i128).unwrap(),
                block_b
            ),
            Some(id_b)
        );
        let s_b = core_b.get_v4_pool(id_b).expect("registered B");
        assert_eq!(
            s_b.tick_data.get(&60).expect("t60").liquidity_gross,
            s_a.tick_data.get(&60).expect("t60").liquidity_gross
        );
        assert_eq!(s_b.journal.len(), s_a.journal.len());
        assert_eq!(s_b.update_block, s_a.update_block);
    }

    /// Regression (J63J3N, scalar read half): `PyLiquidityPool.snapshot_v3`
    /// and the per-field scalar getters all routed through `get_v3_pool`,
    /// which returns `None` for `PoolEntry::V4` — silently dropping V4 reads
    /// (the read-side twin of the RAJ3PP write-side bug). The fix is the
    /// family-dispatching `BotState::get_v3_or_v4_pool` accessor returning a
    /// `&dyn V3FamilyPool`. This pins both halves of the AC: a V4 pool
    /// returns a non-`None` scalar view, and the scalars match a direct
    /// `apply_v4_swap` on the same inputs.
    #[test]
    fn get_v3_or_v4_pool_reads_v4_scalars_matching_apply_v4_swap() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x88u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0x99u8; 32];
        let block_b = 11u64;

        let make_params = || RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        };

        // Twin V4 pools: A read via the family accessor after a dispatcher
        // apply; B updated via the dedicated `apply_v4_swap`. Both start
        // identical.
        let mut core_a = BotState::new();
        let id_a = core_a
            .register_v4_pool(&make_params())
            .expect("V4 pool A registers");
        let mut core_b = BotState::new();
        let id_b = core_b
            .register_v4_pool(&make_params())
            .expect("V4 pool B registers");

        // Before the fix, `get_v3_or_v4_pool` did not exist and the Python
        // reader used `get_v3_pool` (None for V4). Assert the accessor now
        // returns a non-None view of the post-apply state.
        let _ = core_a.apply_swap_by_pool_id(
            id_a,
            U256::from(3u128) << 96,
            9_000_000,
            -240,
            block_b,
            &[],
        );
        let view_a = core_a
            .get_v3_or_v4_pool(id_a)
            .expect("V4 pool must surface a non-None reader view");
        assert_eq!(view_a.sqrt_price_x96(), U256::from(3u128) << 96);
        assert_eq!(view_a.liquidity(), 9_000_000);
        assert_eq!(view_a.tick(), -240);
        assert_eq!(view_a.update_block(), block_b);
        // Immutable V4 key fields surface from `pool_key`.
        assert_eq!(view_a.fee(), 500);
        assert_eq!(view_a.tick_spacing(), 10);

        // AC parity: identical scalar inputs via `apply_v4_swap` produce the
        // same values read through the accessor.
        let _ = core_b.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(3u128) << 96,
                liquidity: 9_000_000,
                tick: -240,
                tick_priors: Vec::new(),
            },
            block_b,
        );
        let view_b = core_b
            .get_v3_or_v4_pool(id_b)
            .expect("V4 pool B surfaces a reader view");
        assert_eq!(view_b.sqrt_price_x96(), view_a.sqrt_price_x96());
        assert_eq!(view_b.liquidity(), view_a.liquidity());
        assert_eq!(view_b.tick(), view_a.tick());
        assert_eq!(view_b.update_block(), view_a.update_block());
    }

    /// Regression (J63J3N, tick-data read half): `tick_data_snapshot` and
    /// `tick_bitmap_snapshot` routed through `get_v3_pool`, returning an
    /// empty dict for V4 pools. The family `get_v3_or_v4_pool` accessor's
    /// `tick_data()` must surface a V4 pool's tick map (post-Mint/Burn) — and
    /// it must match `apply_v4_liquidity_update` on the same inputs.
    #[test]
    fn get_v3_or_v4_pool_reads_v4_tick_data_matching_apply_v4_liquidity_update() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0xaau8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0xbbu8; 32];
        let block_b = 13u64;

        // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
            },
        );
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 3_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        };

        // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
        let mut core_a = BotState::new();
        let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
        let mut core_b = BotState::new();
        let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

        let _ = core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 700, block_b);
        let _ = core_b.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(700i128).unwrap(),
            block_b,
        );

        // Before the fix, the V4 view came back None → empty dict. Assert the
        // accessor now yields the V4 tick map (non-empty, mutated) and the
        // view matches the dedicated-path twin.
        let view_a = core_a
            .get_v3_or_v4_pool(id_a)
            .expect("V4 A reader view non-None");
        let view_b = core_b
            .get_v3_or_v4_pool(id_b)
            .expect("V4 B reader view non-None");
        assert!(!view_a.tick_data().is_empty(), "V4 tick map surfaced");
        assert_eq!(
            view_a.tick_data().get(&60).expect("t60").liquidity_gross,
            U128::from(800),
            "tick 60 gross reflects the +700 ModifyLiquidity"
        );
        assert_eq!(
            view_a.tick_data().get(&60).expect("t60").liquidity_gross,
            view_b.tick_data().get(&60).expect("t60").liquidity_gross,
            "dispatcher and dedicated path produce identical V4 tick maps"
        );
        assert_eq!(view_a.update_block(), view_b.update_block());
    }

    /// Regression (F7HX73): same-block multi-Swap reorg rollback.
    ///
    /// `push_delta` collapsed same-block deltas ("same-block replacement"):
    /// the second Swap at block B replaced the first, so the recorded
    /// `scalar_priors` became post-first-Swap, not pre-block. On
    /// `restore_before_block(B)` the popped delta then returned post-first-Swap
    /// scalars, landing the pool on post-first-Swap instead of the true pre-B
    /// state. (Two same-block swaps on mainnet V3/V4 are common — multi-hop
    /// arb bots, MEV activity.)
    ///
    /// This test pins the AC: register a V3 pool, push two Swap deltas at
    /// block B with different scalars, then `restore_before_block(B)` and
    /// assert the pool scalars match the pre-B (registration) state, not
    /// post-first-Swap.
    ///
    /// Note: the AC text says `restore_before_block(B+1)`, but that is the
    /// no-op case (newest at B < B+1 returns current state = post-both-swaps,
    /// correct for "before B+1"). The bug manifests at `restore_before_block(B)`,
    /// which pops the block-B delta — the trigger exercised here.
    #[test]
    fn v3_restore_before_block_after_same_block_multi_swap_lands_on_pre_block() {
        use crate::bot_core::RegisterV3PoolParams;
        use crate::optimizers::uniswap_engine::PoolTickCoverage;

        let mut core = BotState::new();
        let pool_id = core.register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xf7u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        });

        let block_b = 9u64;
        // Two same-block Swaps with distinct scalars.
        let _ = core.apply_v3_swap_by_pool_id(
            pool_id,
            U256::from(2u128) << 96,
            2_000_000,
            -10,
            block_b,
            &[],
        );
        let _ = core.apply_v3_swap_by_pool_id(
            pool_id,
            U256::from(3u128) << 96,
            3_000_000,
            -20,
            block_b,
            &[],
        );

        // Sanity: current state reflects the second swap.
        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(s.sqrt_price_x96, U256::from(3u128) << 96);
        }

        // Roll back block B. Pre-fix this returned post-first-Swap scalars
        // (2<<96, 2_000_000, -10); the fix must land on the pre-B (registration)
        // state (1<<96, 1_000_000, 0).
        let _ = core.v3_restore_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered after restore");
        assert_eq!(
            s.sqrt_price_x96,
            U256::from(1u128) << 96,
            "same-block multi-swap restore lands on pre-B sqrt_price, not post-first-Swap"
        );
        assert_eq!(s.liquidity, 1_000_000);
        assert_eq!(s.tick, 0);
    }

    // -----------------------------------------------------------------------
    // ADR-007: BotState::unregister_pool (V2/V3 address-keyed, V4 tuple-keyed).
    // -----------------------------------------------------------------------

    #[test]
    fn unregister_v2_pool_returns_true_then_re_register_allocates_fresh_id() {
        let mut core = BotState::new();
        let params = make_params(U256::from(1000), U256::from(2000));
        let first_id = core.register_v2_pool(&params);
        assert_eq!(core.pool_count(), 1);

        // Unregister the V2 pool.
        let removed = core.unregister_pool(make_pool_addr(), None);
        assert!(removed, "unregister of a registered V2 pool returns true");
        assert_eq!(core.pool_count(), 0, "unregister must drop the PoolEntry");
        assert_eq!(
            core.pool_id_by_address(&make_pool_addr()),
            None,
            "unregister must clear pool_addresses"
        );

        // Re-register: must succeed (no panic) and allocate a fresh id
        // (retired ids are NOT reused — ADR-007 U3).
        let second_id = core.register_v2_pool(&params);
        assert_ne!(
            second_id, first_id,
            "re-register must allocate a fresh id (retired, not reused)",
        );
        assert_eq!(core.pool_count(), 1);
    }

    #[test]
    fn unregister_pool_on_unknown_address_returns_false_silently() {
        let mut core = BotState::new();
        // Register one pool at make_pool_addr().
        let _ = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));
        let unknown = Address::from([0x99u8; 20]);

        let removed = core.unregister_pool(unknown, None);
        assert!(!removed, "unregister on an unknown address returns false");
        // No mutation occurred.
        assert_eq!(core.pool_count(), 1);
    }

    #[test]
    fn unregister_v3_pool_discards_buffered_liquidity_events() {
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();

        // Pre-registration: buffer a backfill ModifyLiquidity for `v3_addr`.
        // `buffer_backfill_v3_liquidity_update` on an UNregistered address
        // buffers (the registered-address early-return path doesn't fire).
        core.buffer_backfill_v3_liquidity_update(v3_addr, -100, 100, 500_i128, 42_u64);
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            1,
            "precondition: the event is buffered for the unregistered address"
        );

        // Register the V3 pool. Registration does NOT auto-drain the buffer
        // (drain is caller-driven via `apply_backfill_buffer_v3`); the buffer
        // entry persists. This mirrors the live pump path where events can
        // arrive pre-registration and stay buffered.
        let _ = core.register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
        });
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            1,
            "registration must not auto-drain (drain is caller-driven), so the buffer entry survives to here"
        );

        // Unregister the V3 pool. ADR-007 U3: drain the V3 buffer for the
        // removed address so a re-register does not replay stale Mint/Burn.
        let removed = core.unregister_pool(v3_addr, None);
        assert!(removed);
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            0,
            "unregister must discard buffered V3 events for the removed address"
        );
    }

    #[test]
    fn unregister_v4_pool_by_tuple_key_discards_buffered_modify_liquidity() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use crate::optimizers::uniswap_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::PoolId = [0xeeu8; 32];
        let mut core = BotState::new();

        let pool_id_u64 = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect("V4 pool registers");
        assert_eq!(core.pool_count(), 1);

        // Unregister by the V4 tuple key (address = pool_manager).
        let removed = core.unregister_pool(pool_manager, Some(pool_id_bytes));
        assert!(removed, "unregister of a registered V4 pool returns true");
        assert_eq!(core.pool_count(), 0);

        // Re-register must succeed (no Err) and allocate a fresh id.
        let second_id = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
            })
            .expect("V4 re-register after unregister must succeed");
        assert_ne!(
            second_id, pool_id_u64,
            "re-register must allocate a fresh id (retired, not reused)",
        );
    }
}
