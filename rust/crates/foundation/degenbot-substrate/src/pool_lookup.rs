//! The one pool key an orchestration verb resolves.
//!
//! A pool is addressed two ways in this codebase — a V3 pool by contract
//! address, a V4 pool by `(pool_manager, pool_id)` — and the state owner used
//! to publish each orchestration verb once per shape (`mark_registration_live` /
//! `set_pool_live`, and so on). [`PoolLookup`] is the value that ends
//! that: a caller resolves its key once, and a verb reads the shape off the
//! lookup.
//!
//! The verbs here delegate to the state owner's own accessors — this is a join
//! over the two key shapes, not a second reading of pool state. The id-keyed
//! accessors remain the interior a hot path uses when it already holds a
//! `pool_id`.

use alloy::primitives::Address;
use hashbrown::HashMap;

use crate::concentrated_liquidity_orchestration::{V3Family, V4Family};
use crate::state_lock::StateLock;
use crate::{BotState, ConcentratedLiquidityPoolMut, PoolEntry, PoolTickCoverage, TickInfo};
use degenbot_decoders::v4_swap_decoder::V4PoolId;

/// The pool key the verify lifecycle runs over.
///
/// The lifecycle is one machine over one key. The two concentrated-liquidity
/// designs differ only in how the state owner addresses a pool — a V3 pool by
/// contract address, a V4 pool by `(pool_manager, pool_id)` — so the key is a
/// value and the eight state verbs the machine needs hang off it. Both arms
/// delegate to the state owner's own accessors: this is a join, not a second
/// reading of pool state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PoolLookup {
    /// A V3 concentrated-liquidity pool, addressed by contract address.
    Address(Address),
    /// A V4 concentrated-liquidity pool, addressed by `(pool_manager, pool_id)`.
    V4 {
        /// The canonical `PoolManager`.
        pool_manager: Address,
        /// The pool's on-chain id.
        pool_id: V4PoolId,
    },
}

impl PoolLookup {
    /// The address-keyed lookup, for any key that converts to an `Address`.
    pub fn from_address(address: impl Into<Address>) -> Self {
        Self::Address(address.into())
    }

    /// Resolve a `pool_id` to the key its family addresses the pool by.
    ///
    /// The ONE family match behind the `*_by_pool_id` apply doors: a V3 pool is
    /// keyed by its contract address, a V4 pool by `(pool_manager, pool_id)`,
    /// and a family with no concentrated-liquidity state — or an unregistered
    /// id — has no such key at all, so the doors that need one can refuse it.
    /// Round-trips with [`Self::resolve`], which maps the key back to `pool_id`.
    #[must_use]
    pub fn from_pool_id(state: &BotState, pool_id: u64) -> Option<Self> {
        match state.registry.pools.get(&pool_id)? {
            PoolEntry::V3(p) => Some(Self::Address(p.0.address)),
            PoolEntry::V4(p) => Some(Self::V4 {
                pool_manager: p.0.pool_manager,
                pool_id: p.0.pool_id,
            }),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// The structural family this lookup names.
    ///
    /// The canonical tag is `RegisteredPoolFamily`; a lookup narrows it to the
    /// two concentrated-liquidity families it can express.
    #[must_use]
    pub const fn family(&self) -> degenbot_pools::sim::registry::RegisteredPoolFamily {
        match self {
            Self::Address(_) => degenbot_pools::sim::registry::RegisteredPoolFamily::V3,
            Self::V4 { .. } => degenbot_pools::sim::registry::RegisteredPoolFamily::V4,
        }
    }

    /// The pool id this lookup names, or `None` when the pool is not
    /// registered.
    ///
    /// The one resolution: a caller running several verbs resolves once and
    /// passes the id along, instead of each verb re-resolving through the
    /// registry. The ordering/`Eq` derives exist so a lookup can key a memo.
    #[must_use]
    pub fn resolve(&self, state: &BotState) -> Option<u64> {
        match self {
            Self::Address(address) => state.pool_id_by_address(address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state.pool_id_by_key(*pool_manager, pool_id),
        }
    }

    /// The pool's mutable concentrated-liquidity state — the write twin of
    /// [`Self::coverage`], for the verbs that mutate the scalars and the tick
    /// map directly (`apply_swap`, `apply_liquidity_update`, `seed_genesis`)
    /// instead of routing an event through the buffer.
    ///
    /// The registry's own concentrated projection is the family-neutral handle,
    /// so a caller holding a resolved key never names `V3PoolState` or
    /// `V4PoolState`. `None` when the key names no registered
    /// concentrated-liquidity pool.
    pub fn state_mut<'a>(
        &self,
        state: &'a mut BotState,
    ) -> Option<&'a mut dyn ConcentratedLiquidityPoolMut> {
        let pool_id = self.resolve(state)?;
        state
            .registry
            .pools
            .get_mut(&pool_id)
            .and_then(PoolEntry::as_concentrated_liquidity_mut)
    }

    /// The pool's tick coverage, or `None` when the pool is not registered
    /// under this key (the machine's no-op arm).
    #[must_use]
    pub fn coverage(&self, state: &BotState) -> Option<PoolTickCoverage> {
        state.pool_coverage(*self)
    }

    /// Defer the pool's live events to the pump buffer until verification ends.
    pub fn begin_registration_quarantine(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .begin_registration_quarantine::<V3Family>(&mut state.registry, *address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .begin_registration_quarantine::<V4Family>(
                    &mut state.registry,
                    (*pool_manager, *pool_id),
                ),
        }
    }

    /// Consume the pinned snapshot seed (exactly once, so memory is bounded).
    pub fn take_seed(&self, state: &mut BotState) -> Option<HashMap<i32, TickInfo>> {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .take_snapshot_seed::<V3Family>(&mut state.registry, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .take_snapshot_seed::<V4Family>(&mut state.registry, &(*pool_manager, *pool_id)),
        }
    }

    /// The pool's OWN liquidity clock (the block its tick data reflects); `0`
    /// when unset.
    ///
    /// Takes the lock rather than a guard: the state owner's fused accessor
    /// chains key → clock under ONE `read_at(Registration)`, so this must not
    /// be recomposed from two single-step reads (nested reads are a
    /// self-deadlock when a writer queues between them — soak-2026-08-22).
    pub fn own_liquidity_clock(&self, core: &StateLock<BotState>) -> u64 {
        match self {
            Self::Address(address) => core.pool_tick_data_block_by_address(address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => core.pool_tick_data_block_by_v4_key(*pool_manager, pool_id),
        }
    }

    /// Drain the buffered backfill and pump events onto the pool. Draining is
    /// not verification: it preserves the permanent V2-V2-V3 apply-buffer
    /// behaviour and runs for a Sparse pool too.
    pub fn drain(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => {
                state
                    .concentrated_liquidity
                    .apply_backfill_buffer::<V3Family>(&mut state.registry, address);
                state
                    .concentrated_liquidity
                    .apply_pump_buffer::<V3Family>(&mut state.registry, address);
            }
            Self::V4 {
                pool_manager,
                pool_id,
            } => {
                state
                    .concentrated_liquidity
                    .apply_backfill_buffer::<V4Family>(
                        &mut state.registry,
                        &(*pool_manager, *pool_id),
                    );
                state
                    .concentrated_liquidity
                    .apply_pump_buffer::<V4Family>(&mut state.registry, &(*pool_manager, *pool_id));
            }
        }
    }

    /// Apply only the buffered backfill onto the pool (the granular half of
    /// [`Self::drain`], for a caller that has already pumped or must not).
    pub fn backfill(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .apply_backfill_buffer::<V3Family>(&mut state.registry, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .apply_backfill_buffer::<V4Family>(&mut state.registry, &(*pool_manager, *pool_id)),
        }
    }

    /// Apply only the buffered pump events onto the pool.
    pub fn pump(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .apply_pump_buffer::<V3Family>(&mut state.registry, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .apply_pump_buffer::<V4Family>(&mut state.registry, &(*pool_manager, *pool_id)),
        }
    }

    /// Freeze the post-drain `(tick_data, block)` pair atomically with the
    /// drain.
    pub fn pin_post_drain(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .pin_post_drain_snapshot::<V3Family>(&mut state.registry, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .pin_post_drain_snapshot::<V4Family>(
                    &mut state.registry,
                    &(*pool_manager, *pool_id),
                ),
        }
    }

    /// Consume the pinned post-drain pair (exactly once).
    pub fn take_post_drain(&self, state: &mut BotState) -> Option<(HashMap<i32, TickInfo>, u64)> {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .take_post_drain_snapshot::<V3Family>(&mut state.registry, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .take_post_drain_snapshot::<V4Family>(
                    &mut state.registry,
                    &(*pool_manager, *pool_id),
                ),
        }
    }

    /// The terminal transition: only reachable after a verified success.
    pub fn mark_registration_live(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .mark_registration_live::<V3Family>(&mut state.registry, *address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .mark_registration_live::<V4Family>(&mut state.registry, (*pool_manager, *pool_id)),
        }
    }
}
