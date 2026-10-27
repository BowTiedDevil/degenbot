//! The family-agnostic registry surface: the pool table, the token metadata
//! table, and the clocks the pool entries carry.
//!
//! `BotState` owns exactly one pool registry, and this module is its id-keyed
//! front: the entry borrows, the typed slice projections (the family rides the
//! caller's type parameter, never a method name), the update clocks, and the
//! token table. The family orchestration modules reach the same tables through
//! the [`RegistryCore`](crate::RegistryCore) view their callers pass, so both
//! read paths answer from one table set and no family can hold a mirror that
//! disagrees with the state owner.
//!
//! Turning a caller's key (an address, a `(pool_manager, pool_id)` pair, a
//! typed identity) into a `pool_id` is [`crate::pool_lookup`]'s job, not this
//! module's.

use alloy::primitives::Address;

use super::{BotState, PoolEntry, PoolEntrySlice, TokenEntry};

impl BotState {
    /// The registered entry for `pool_id` — the family-agnostic borrow every
    /// structural `Pool` handle reads its family slice through.
    #[must_use]
    pub fn pool_entry(&self, pool_id: u64) -> Option<&PoolEntry> {
        self.registry.pools.get(&pool_id)
    }

    /// The family-neutral typed registry read (ADR-073 D3): the slice or
    /// identity type is the caller's type parameter, so the family knowledge
    /// lives in the type, never in a method name. `None` when the id is
    /// unregistered or its family does not carry a `T`.
    #[must_use]
    pub fn pool_slice<T: PoolEntrySlice + ?Sized>(&self, pool_id: u64) -> Option<&T> {
        self.registry
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::slice::<T>)
    }

    /// The family-neutral typed registration count: how many registered
    /// entries project onto the slice type `T` (the solve-lane sizing rows'
    /// shared body — the count is per typed slice, not per named family).
    #[must_use]
    pub fn pool_count_of<T: PoolEntrySlice>(&self) -> usize {
        self.registry
            .pools
            .values()
            .filter(|entry| entry.slice::<T>().is_some())
            .count()
    }

    /// The registered address of a workspace pool id — the deficit trace's
    /// id→address join so a per-hop reject names the responsible pool.
    #[must_use]
    pub fn pool_address_of(&self, pool_id: u64) -> Option<Address> {
        match self.registry.pools.get(&pool_id)? {
            PoolEntry::V2(p) => Some(p.0.address),
            PoolEntry::V3(p) => Some(p.0.address),
            PoolEntry::V4(p) => Some(p.0.pool_manager),
            PoolEntry::Curve(p) => Some(p.0.address),
            PoolEntry::BalancerWeighted(p) => Some(p.0.address),
            PoolEntry::BalancerStable(p) => Some(p.0.address),
            PoolEntry::AerodromeV2(p) => Some(p.0.address),
        }
    }

    /// The pool's per-mutation state nonce (the staleness gate). Returns
    /// `0` for an unregistered pool (the dispatch seam treats an unknown
    /// pool as fresh — it will fail the path-validity check elsewhere).
    /// Used by the dispatch fan-out to detect a stale solve result: the
    /// solver snapshots each hop's nonce at resolve time; the sim seam
    /// re-reads it and skips candidates whose pool state advanced since.
    #[must_use]
    pub fn pool_state_nonce(&self, pool_id: u64) -> u64 {
        self.registry
            .pools
            .get(&pool_id)
            .map_or(0, PoolEntry::state_nonce)
    }

    /// The `update_block` of the pool at `pool_id` — the block its reserves /
    /// `sqrt_price` / `tick` / liquidity were last mutated by a forward `Sync` /
    /// `Swap` / Mint-Burn event. `0` for an unregistered pool (never advanced).
    ///
    /// `update_block` is a last-activity clock, NOT a staleness signal: a pool
    /// that last mutated N blocks ago is quiet (its stored state is byte-
    /// identical to on-chain), not stale. The former solve-time staleness
    /// gate mis-used it to defer quiet paths and
    /// was REMOVED. The ADR-021 chain-vs-solver tripwire retiree:
    /// in-process chain-vs-solver-state verification is retired
    /// with the stage-separated data plane — desync the plane excludes is
    /// unrepresentable — so `update_block` stays a pure bookkeeping clock
    /// (used by the Q1a merge staleness oracle and the epoch delta). Upstream
    /// verification remains at the Published edge (`CompletenessDecision::Verify`
    /// → `assert_ws_block_complete`).
    #[must_use]
    pub fn pool_update_block(&self, pool_id: u64) -> u64 {
        self.registry
            .pools
            .get(&pool_id)
            .map_or(0, PoolEntry::update_block)
    }

    /// The pool-state **price clock head**: the maximum `update_block` across
    /// every registered pool (V2/V3/V4), `0` when none are registered.
    ///
    /// This is the block the live pool state actually reflects. During a
    /// backfill/drain desync the pools are advanced ahead of the pump's
    /// header clock, so `pool_state_head()` can exceed the drain `block_number`
    /// the correct solve/verify/sim anchor is this head, NOT the lagging
    /// clock. Because a pool is unchanged from its `update_block` onward, a
    /// single head anchor reproduces each path's solver state exactly
    /// (unchanged pools have byte-identical EVM state at `update_block` and
    /// head), so one shared sim cache serves every path.
    #[must_use]
    pub fn pool_state_head(&self) -> u64 {
        self.registry
            .pools
            .values()
            .map(PoolEntry::update_block)
            .max()
            .unwrap_or(0)
    }

    /// The pool's **liquidity** clock (`tick_data_block`, two-stamp rule) —
    /// the block its tick map reflects. See [`PoolEntry::tick_data_block`]. A
    /// concentrated liquidity pool with `pool_tick_data_block` well behind `pool_update_block` is
    /// the staged-clock desync class (`0x5653`): fresh price, stale tick map.
    /// Returns `0` for an unregistered id (the freshness gate treats 0 as
    /// stale, mirroring [`Self::pool_update_block`]).
    #[must_use]
    pub fn pool_tick_data_block(&self, pool_id: u64) -> u64 {
        self.registry
            .pools
            .get(&pool_id)
            .map_or(0, PoolEntry::tick_data_block)
    }

    /// Number of registered pools.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.registry.pools.len()
    }

    /// Check if a pool ID is registered.
    #[must_use]
    pub fn has_pool(&self, pool_id: u64) -> bool {
        self.registry.pools.contains_key(&pool_id)
    }

    /// Check if a token address is registered.
    #[must_use]
    pub fn has_token(&self, address: &Address) -> bool {
        self.registry.tokens.contains_key(address)
    }

    /// Look up a registered token's metadata entry (address, name, symbol,
    /// decimals, `chain_id`) by contract address. Used by `PyErc20Token`'s getters
    /// (ADR-003 T3: Rust owns token identity metadata).
    #[must_use]
    pub fn token_entry(&self, address: &Address) -> Option<&TokenEntry> {
        self.registry.tokens.get(address)
    }

    /// Register a token.
    ///
    /// Idempotent (safe under concurrent registration workers): if the
    /// token address is already registered, the existing entry is canonical and
    /// this is a no-op (no panic). A sibling registration worker may insert the
    /// same token concurrently; racing inserts must not take the process down.
    pub fn register_token(
        &mut self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        self.registry
            .tokens
            .entry(address)
            .or_insert_with(|| TokenEntry {
                address,
                name,
                symbol,
                decimals,
                chain_id,
            });
    }
}
