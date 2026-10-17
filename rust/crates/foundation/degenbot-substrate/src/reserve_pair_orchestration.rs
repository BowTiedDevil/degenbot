//! Reserve-pair structural family — `impl BotState` orchestration (`V2` + `AerodromeV2`).
//!
//! Carved out of `bot_core/mod.rs` (the `BotState` god-file). This module owns the reserve-pair
//! capability's LIVE-STATE surfaces — `V2`/`AerodromeV2` registration, the unified
//! sync-apply row, and the identity/state getters. State ownership is NOT family
//! behavior (GLOSSARY "concentrated liquidity orchestration capability" pattern): the registration
//! entries and the family-neutral apply/get rows stay here (ADR-073 D3 keeps the
//! named seams), while the family behavior — the swap reads and the resolve
//! projection — folds through the family read module's intent doors
//! (`degenbot_pools::families::reserve_pair`, ADR-073 D1). The family state
//! types live in `degenbot-pools` (I/O-free, ADR-001).
//!
//! Child-module impl blocks reach `BotState`'s private fields directly (same
//! pattern as `divergence_probe.rs`); the public surface is unchanged because
//! these are inherent methods on `BotState`, and `bot_core/mod.rs` remains the
//! assembly + re-export hub.

use alloy::primitives::{aliases::U112, Address, U256};

use degenbot_pools::aerodrome_v2_state::{
    AerodromeV2PoolState, RegisterAerodromePoolError, RegisterAerodromeV2PoolParams,
};
use degenbot_pools::v2_state::{RegisterV2PoolError, RegisterV2PoolParams, V2PoolState};

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
        // Spec-bound admission: reject up-front rather
        // than propagating overlarge reserves into `V2PoolState` (where the
        // downstream swap-math U512→U256 narrowing would silently degrade to
        // `U256::MAX` under the prior sat-cap, or panic — see the helper's
        // `# Panics` section committed in `19218a2c`).
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve0, "reserve0")?;
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve1, "reserve1")?;
        if self.registry.pool_addresses.contains_key(&params.address) {
            return Err(RegisterV2PoolError::AlreadyRegistered {
                address: params.address,
            });
        }

        let pool_id = self.registry.next_pool_id;
        self.registry.next_pool_id += 1;

        // Construction I/O (ADR-003 single source): capture the pair's token
        // decimal scales off the token registry at registration — the same
        // construction seam that carries `stable_swap`/`fee_denominator` onto
        // the identity. The Camelot-stable read arm scales from them; an
        // unregistered token leaves `None` and that arm refuses rather than
        // guess a scale.
        let token_decimals = (
            self.token_entry(&params.token0).map(|t| t.decimals),
            self.token_entry(&params.token1).map(|t| t.decimals),
        );

        // Construct (identity, state) + genesis journal delta on the state
        // struct (ADR-014 D6/Q7 — V2 joins its 6 siblings; the construction
        // + genesis-delta push moved out of `register_v2_pool` into
        // `V2PoolState::from_params`).
        let (identity, state) =
            V2PoolState::from_params(params, self.registry.journal_depth, token_decimals);

        self.registry
            .pools
            .insert(pool_id, PoolEntry::V2(Box::new((identity, state))));
        self.registry.pool_addresses.insert(params.address, pool_id);

        Ok(pool_id)
    }

    /// Apply a reserve-pair `Sync` event keyed by the pool's contract
    /// ADDRESS — the address-keyed twin of [`Self::apply_sync_by_pool_id`]
    /// (ADR-073 D3: the former `apply_v2_sync` wrapper name was V2-shaped, but
    /// the row is family-neutral — an Aerodrome pool's `Sync(uint112,
    /// uint112)` lands here identically, resolved to its `pool_id` then
    /// dispatched through `ReservePairPoolState::apply_sync`). Journals the
    /// prior reserves, then updates `reserve0`/`reserve1`/`update_block` in
    /// place. Returns the affected `pool_id` so the engine can mark the right
    /// path set dirty; returns `None` if the address is not registered (a
    /// no-op).
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    #[must_use]
    pub fn apply_sync_by_address(
        &mut self,
        pool_address: Address,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        // ADR-014 D1: delegate to the pool_id-keyed row (the V3
        // address-keyed wrapper pattern). The inline body that previously
        // lived here was byte-identical to `V2PoolState::apply_sync`, which the
        // twin reaches via `as_reserve_pair_mut()?.apply_sync(...)` — the
        // duplication (the bug-hiding class D1 was written to kill) is removed;
        // the address→pool_id resolution is what this row owns.
        let &pool_id = self.registry.pool_addresses.get(&pool_address)?;
        self.apply_sync_by_pool_id(pool_id, reserve0, reserve1, block_number)
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
    /// (V2 / `AerodromeV2`); `None` otherwise (silent no-op — a concentrated liquidity / Curve /
    /// Balancer `pool_id` yields `None`).
    pub fn apply_sync_by_pool_id(
        &mut self,
        pool_id: u64,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        let entry = self.registry.pools.get_mut(&pool_id)?;
        entry
            .as_reserve_pair_mut()?
            .apply_sync(reserve0, reserve1, block_number);
        Some(pool_id)
    }

    /// Register an Aerodrome V2 pool by contract address (ADR-005 Aerodrome
    /// state port).
    ///
    /// Stores immutable identity (`address`, `token0`, `token1`, `factory`,
    /// `variant`, `stable`, unidirectional `fee`), the registration
    /// reserves, and a genesis reorg-journal anchor (mirror of V2's
    /// discipline). Returns the auto-assigned pool ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterAerodromePoolError::AlreadyRegistered`] if a pool at
    /// this address is already registered (replaces the prior `assert!`
    /// panic, mirroring the V2 conversion). Returns
    /// [`RegisterAerodromePoolError::SpecViolation`] when `reserve0` or
    /// `reserve1` exceed `uint112::MAX` — the on-chain `uint112` storage
    /// width the Solidly pair asserts at `_update`. Both reserves are typed
    /// `U112` at the params layer, so this fires only if the params are ever
    /// re-widened; the check is belt-and-braces parity with V2.
    pub fn register_aerodrome_pool(
        &mut self,
        params: &RegisterAerodromeV2PoolParams,
    ) -> Result<u64, RegisterAerodromePoolError> {
        // Spec-bound admission (parity with `register_v2_pool`): the reserves
        // are already `U112`-typed, so this only rejects a re-widened params
        // layer — kept so the admission contract matches its V2 sibling.
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve0, "reserve0")?;
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve1, "reserve1")?;
        if self.registry.pool_addresses.contains_key(&params.address) {
            return Err(RegisterAerodromePoolError::AlreadyRegistered {
                address: params.address,
            });
        }

        let pool_id = self.registry.next_pool_id;
        self.registry.next_pool_id += 1;
        let (identity, state) =
            AerodromeV2PoolState::from_params(params, self.registry.journal_depth);
        self.registry
            .pools
            .insert(pool_id, PoolEntry::AerodromeV2(Box::new((identity, state))));
        self.registry.pool_addresses.insert(params.address, pool_id);

        Ok(pool_id)
    }

    /// The number of registered pools the V2 solve lane ADMITS: the
    /// reserve-pair family's volatile designs (a V2 entry with
    /// `stable_swap = false`, an Aerodrome entry with `stable = false` —
    /// ADR-059 D6, one dispatch row, two entry arms). A Camelot
    /// `stable_swap` V2 entry and a stable Aerodrome entry are the Solidly
    /// lane's designs and must not be counted here.
    #[must_use]
    pub fn reserve_pair_pool_count(&self) -> usize {
        // The family knowledge lives in the type (the `pool_count_of` row's
        // typed-projection pattern — no central dispatch match here): a
        // volatile design is what the V2 solve lane admits; the stable flags
        // ride the identities the typed slices project.
        self.registry
            .pools
            .values()
            .filter(|entry| {
                entry
                    .slice::<degenbot_pools::v2_state::V2PoolIdentity>()
                    .is_some_and(|i| !i.stable_swap)
                    || entry
                        .slice::<degenbot_pools::aerodrome_v2_state::AerodromeV2PoolIdentity>()
                        .is_some_and(|i| !i.stable)
            })
            .count()
    }

    /// The registered reserve pair's `(reserve0, reserve1, update_block)`
    /// snapshot — the reserves reader the V2 solve lane and the reorg
    /// coordinator reconcile against (the returned tuple is the
    /// reserve-pair slice's values).
    #[must_use]
    pub fn pool_reserves(&self, pool_id: u64) -> Option<(U256, U256, u64)> {
        let state = self.pool_slice::<V2PoolState>(pool_id)?;
        Some((
            U256::from(state.reserve0),
            U256::from(state.reserve1),
            state.update_block,
        ))
    }
}
