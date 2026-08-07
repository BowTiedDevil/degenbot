//! Reserve-pair structural family — `impl BotState` orchestration (`V2` + `AerodromeV2`).
//!
//! Carved out of `bot_core/mod.rs` (the `BotState` god-file; see
//! `docs/plans/botstate-god-file-split.md`). This module owns the reserve-pair
//! `BotState` method set — `V2`/`AerodromeV2` registration, sync/apply, snapshots,
//! and identity/state getters. Pure `impl BotState` orchestration: the family
//! state types live in `degenbot-pools` (I/O-free, ADR-001).
//!
//! Child-module impl blocks reach `BotState`'s private fields directly (same
//! pattern as `divergence_probe.rs`); the public surface is unchanged because
//! these are inherent methods on `BotState`, and `bot_core/mod.rs` remains the
//! assembly + re-export hub.

use alloy::primitives::{aliases::U112, Address, U256};

use degenbot_pools::aerodrome_v2_state::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromeV2PoolParams,
};
use degenbot_pools::v2_state::{
    RegisterV2PoolError, RegisterV2PoolParams, V2PoolIdentity, V2PoolState,
};

use super::{BotState, PoolEntry};

impl BotState {
    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV2PoolError::AlreadyRegistered`] if a pool at this
    /// address is already registered (replaces the prior `assert!` panic).
    /// Returns [`RegisterV2PoolError::SpecViolation`] when `reserve0` or
    /// `reserve1` exceed `uint112::MAX` — the on-chain `uint112` storage width
    /// v2-core asserts at `UniswapV2Pair._update`. Living pool state from
    /// `Sync(uint112,uint112)` events is structurally spec-bound, so spec
    /// checks fire only on synthetic / corrupt registration.
    pub fn register_v2_pool(
        &mut self,
        params: &RegisterV2PoolParams,
    ) -> Result<u64, RegisterV2PoolError> {
        // Spec-bound admission (epic WOYYS2 / MSTAT2): reject up-front rather
        // than propagating overlarge reserves into `V2PoolState` (where the
        // downstream swap-math U512→U256 narrowing would silently degrade to
        // `U256::MAX` under the prior sat-cap, or panic — see the helper's
        // `# Panics` section committed in `19218a2c`).
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve0, "reserve0")?;
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve1, "reserve1")?;
        if self.pool_addresses.contains_key(&params.address) {
            return Err(RegisterV2PoolError::AlreadyRegistered {
                address: params.address,
            });
        }

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        // Construct (identity, state) + genesis journal delta on the state
        // struct (ADR-014 D6/Q7 — V2 joins its 6 siblings; the construction
        // + genesis-delta push moved out of `register_v2_pool` into
        // `V2PoolState::from_params`).
        let (identity, state) = V2PoolState::from_params(params, self.journal_depth);

        self.pools.insert(pool_id, PoolEntry::V2(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        Ok(pool_id)
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
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        // ADR-014 D1: delegate to the pool_id-keyed dispatcher (the V3
        // address-keyed wrapper pattern). The inline body that previously
        // lived here was byte-identical to `V2PoolState::apply_sync`, which the
        // twin reaches via `as_reserve_pair_mut()?.apply_sync(...)` — the
        // duplication (the bug-hiding class D1 was written to kill) is removed;
        // the address→pool_id resolution is what this wrapper owns.
        let &pool_id = self.pool_addresses.get(&pool_address)?;
        self.apply_sync_by_pool_id(pool_id, reserve0, reserve1, block_number)
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
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) {
        let _ = self.apply_v2_sync(pool_address, reserve0, reserve1, block_number);
    }

    /// Apply a V2 `Sync` by `pool_id` — the `PyLiquidityPool.sync_reserves`
    /// backing. Returns the affected `pool_id`, or `None` if not registered /
    /// not a V2 pool (no-op). Journals the prior reserves then lands the new.
    #[must_use]
    /// Apply a reserve-pair `Sync` event keyed by the handle's `pool_id`,
    /// dispatching through `ReservePairPoolState::apply_sync` (ADR-017 D3 —
    /// replaces the two per-family `apply_v2_sync_by_pool_id` /
    /// `apply_aerodrome_sync_by_pool_id` dispatchers, whose bodies were
    /// byte-identical modulo the variant name). Covers both V2 and Aerodrome
    /// pools (Solidly mirrors v2-core's `Sync(uint112, uint112)`).
    ///
    /// Returns `Some(pool_id)` if the pool is a reserve-pair family
    /// (V2 / `AerodromeV2`); `None` otherwise (silent no-op — a CL / Curve /
    /// Balancer `pool_id` yields `None`).
    pub fn apply_sync_by_pool_id(
        &mut self,
        pool_id: u64,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        let entry = self.pools.get_mut(&pool_id)?;
        entry
            .as_reserve_pair_mut()?
            .apply_sync(reserve0, reserve1, block_number);
        Some(pool_id)
    }

    /// Read a registered V2 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and builds the
    /// orientation-specific `IntHopState` at resolve time from `zero_for_one`.
    #[must_use]
    pub fn get_v2_pool_state(&self, pool_id: u64) -> Option<&V2PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v2)
            .map(|(_, state)| state)
    }

    /// Look up a V2 pool's immutable registration identity (address, tokens,
    /// fees, factory, variant, stable-strategy inputs). Returns `None` if the
    /// pool is not registered or isn't a V2 pool.
    #[must_use]
    pub fn get_v2_identity(&self, pool_id: u64) -> Option<&V2PoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v2)
            .map(|(identity, _)| identity)
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
        Some((
            state.reserve0.to::<U256>(),
            state.reserve1.to::<U256>(),
            state.update_block,
        ))
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V2(..)))
            .count()
    }

    /// Register an Aerodrome V2 pool by contract address (ADR-005 Aerodrome
    /// state port).
    ///
    /// Stores immutable identity (`address`, `token0`, `token1`, `factory`,
    /// `variant`, `stable`, unidirectional `fee`) + the registration reserves
    /// + a genesis reorg-journal anchor (mirror of V2's discipline). Returns
    ///   the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_aerodrome_pool(&mut self, params: &RegisterAerodromeV2PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );
        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;
        let (identity, state) = AerodromeV2PoolState::from_params(params, self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::AerodromeV2(identity, state));
        self.pool_addresses.insert(params.address, pool_id);
        pool_id
    }

    /// Look up an Aerodrome V2 pool's immutable registration identity. Returns
    /// `None` if not registered or not an Aerodrome pool.
    #[must_use]
    pub fn get_aerodrome_identity(&self, pool_id: u64) -> Option<&AerodromeV2PoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::aerodrome_v2)
            .map(|(identity, _)| identity)
    }

    /// Read a registered Aerodrome V2 pool's state by `pool_id` (reserves +
    /// `update_block` + the reorg journal).
    #[must_use]
    pub fn get_aerodrome_pool(&self, pool_id: u64) -> Option<&AerodromeV2PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::aerodrome_v2)
            .map(|(_, state)| state)
    }
}
