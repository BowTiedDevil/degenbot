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
//! over the two key shapes, not a second reading of pool state. Those id-keyed
//! accessors are the interior: a hot path already holding a `pool_id` starts
//! there, and every key-shaped verb above turns a caller's key into that id
//! through them. They answer from the registration tables the state owner
//! writes, so a driver holding an address, a `(pool_manager, pool_id)` pair, or
//! a typed identity reaches one registry — never a mirror.

use std::fmt;

use alloy::primitives::Address;
use hashbrown::HashMap;

use crate::concentrated_liquidity_orchestration::{V3Family, V4Family};
use crate::state_lock::StateLock;
use crate::{
    BotState, ConcentratedLiquidityPoolMut, PoolEntry, PoolIdentity, PoolTickCoverage,
    RegisteredPoolFamily, TickInfo,
};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::TickMap;
use degenbot_rpc::liquidity_verifier::LiquidityMapTarget;

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
        match state.pools.get(pool_id)? {
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
    pub const fn family(&self) -> degenbot_pools::RegisteredPoolFamily {
        match self {
            Self::Address(_) => degenbot_pools::RegisteredPoolFamily::V3,
            Self::V4 { .. } => degenbot_pools::RegisteredPoolFamily::V4,
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
            .pools
            .get_mut(pool_id)
            .and_then(PoolEntry::as_concentrated_liquidity_mut)
    }

    /// The on-chain contract identity this key names — what the
    /// contract-facing liquidity-map verifier reads.
    ///
    /// The family travels in the value (ADR-082): a caller hands a key to the
    /// verifier and never names a design there.
    #[must_use]
    pub fn liquidity_map_target(&self) -> LiquidityMapTarget {
        match self {
            Self::Address(address) => LiquidityMapTarget::V3(*address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => LiquidityMapTarget::V4 {
                pool_manager: *pool_manager,
                pool_id: (*pool_id).into(),
            },
        }
    }

    /// The pool's immutable tick spacing, or `None` when the key names no
    /// registered concentrated-liquidity pool.
    ///
    /// Takes the lock rather than a guard, exactly like
    /// [`Self::own_liquidity_clock`]: the state owner's fused accessors chain
    /// key → spacing under ONE `read_at(Registration)`, so this must not be
    /// recomposed from two single-step reads (nested reads are a self-deadlock
    /// when a writer queues between them — soak-2026-08-22).
    #[must_use]
    pub fn tick_spacing(&self, core: &StateLock<BotState>) -> Option<i32> {
        match self {
            Self::Address(address) => core.pool_tick_spacing_by_address(address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => core.pool_tick_spacing_by_v4_key(*pool_manager, pool_id),
        }
    }

    /// The pool's tick bookkeeping map, cloned into an owned family-free
    /// handle so a verifier can compare it once the state lock is released.
    ///
    /// `None` when the key names no registered concentrated-liquidity pool.
    /// The family match is the ONE place that decides which registry entry
    /// holds the map; everything above reads it through [`TickMap`].
    #[must_use]
    pub fn tick_map_snapshot(&self, state: &BotState) -> Option<Box<dyn TickMap + Send + Sync>> {
        let pool_id = self.resolve(state)?;
        match state.pools.get(pool_id)? {
            PoolEntry::V3(pool) => Some(pool.clone()),
            PoolEntry::V4(pool) => Some(pool.clone()),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
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
                .begin_registration_quarantine::<V3Family>(&mut state.pools, *address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .begin_registration_quarantine::<V4Family>(
                    &mut state.pools,
                    (*pool_manager, *pool_id),
                ),
        }
    }

    /// Consume the pinned snapshot seed (exactly once, so memory is bounded).
    pub fn take_seed(&self, state: &mut BotState) -> Option<HashMap<i32, TickInfo>> {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .take_snapshot_seed::<V3Family>(&mut state.pools, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .take_snapshot_seed::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id)),
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
                    .apply_backfill_buffer::<V3Family>(&mut state.pools, address);
                state
                    .concentrated_liquidity
                    .apply_pump_buffer::<V3Family>(&mut state.pools, address);
            }
            Self::V4 {
                pool_manager,
                pool_id,
            } => {
                state
                    .concentrated_liquidity
                    .apply_backfill_buffer::<V4Family>(
                        &mut state.pools,
                        &(*pool_manager, *pool_id),
                    );
                state
                    .concentrated_liquidity
                    .apply_pump_buffer::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id));
            }
        }
    }

    /// Apply only the buffered backfill onto the pool (the granular half of
    /// [`Self::drain`], for a caller that has already pumped or must not).
    pub fn backfill(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .apply_backfill_buffer::<V3Family>(&mut state.pools, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .apply_backfill_buffer::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id)),
        }
    }

    /// Apply only the buffered pump events onto the pool.
    pub fn pump(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .apply_pump_buffer::<V3Family>(&mut state.pools, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .apply_pump_buffer::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id)),
        }
    }

    /// Freeze the post-drain `(tick_data, block)` pair atomically with the
    /// drain.
    pub fn pin_post_drain(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .pin_post_drain_snapshot::<V3Family>(&mut state.pools, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .pin_post_drain_snapshot::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id)),
        }
    }

    /// Consume the pinned post-drain pair (exactly once).
    pub fn take_post_drain(&self, state: &mut BotState) -> Option<(HashMap<i32, TickInfo>, u64)> {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .take_post_drain_snapshot::<V3Family>(&mut state.pools, address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .take_post_drain_snapshot::<V4Family>(&mut state.pools, &(*pool_manager, *pool_id)),
        }
    }

    /// The terminal transition: only reachable after a verified success.
    pub fn mark_registration_live(&self, state: &mut BotState) {
        match self {
            Self::Address(address) => state
                .concentrated_liquidity
                .mark_registration_live::<V3Family>(&mut state.pools, *address),
            Self::V4 {
                pool_manager,
                pool_id,
            } => state
                .concentrated_liquidity
                .mark_registration_live::<V4Family>(&mut state.pools, (*pool_manager, *pool_id)),
        }
    }
}

/// The key text a log line or span field carries: a V3 pool's address, or a
/// V4 pool's `pool_manager:pool_id` pair.
impl fmt::Display for PoolLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(address) => write!(f, "{address}"),
            Self::V4 {
                pool_manager,
                pool_id,
            } => write!(
                f,
                "{pool_manager}:0x{}",
                alloy::hex::encode_prefixed(pool_id)
            ),
        }
    }
}

/// The family tag for a registered entry — the single family-name source so
/// [`BotState::pool_family`] and the typed dispatchers cannot drift.
///
/// The spelling lives in [`RegisteredPoolFamily::tag`]; this projects an entry
/// onto it rather than repeating the tag table.
pub(crate) const fn entry_family(entry: &PoolEntry) -> &'static str {
    entry.registered_family().tag()
}

/// The id-keyed identity reads: the interior every [`PoolLookup`] verb and
/// every family dispatch resolves through. Each answers from the registration
/// tables this type owns, so a caller holding a `pool_id`, an address, or a
/// typed identity cannot reach a second mirror of the registry.
impl BotState {
    /// Return the pool-family tag for `pool_id` as a kebab-case string — the
    /// spelling [`RegisteredPoolFamily::tag`] owns — or `None` for an
    /// unregistered `pool_id`.
    ///
    /// The string twin of [`Self::registered_family`]. This is the uniform
    /// family-guard primitive every `from_handle` seam asserts against, so it
    /// is correct for every registered family (unlike the V2-only `variant`
    /// getter on `PyLiquidityPool`, which returns `""` for non-V2). `None` is
    /// explicit so a caller cannot mistake an unregistered pool for a family
    /// gap via an `""` sentinel.
    #[must_use]
    pub fn pool_family(&self, pool_id: u64) -> Option<&'static str> {
        self.registered_family(pool_id)
            .map(RegisteredPoolFamily::tag)
    }

    /// The registered family of `pool_id` as the typed
    /// [`RegisteredPoolFamily`] — the enum twin of [`Self::pool_family`].
    ///
    /// This is what a family dispatch matches on: the match is exhaustive over
    /// the registered families, so adding a family is a compile error at every
    /// dispatch instead of a runtime fallthrough. `None` means the id is not
    /// registered — the same answer [`Self::pool_family`] gives as an absent
    /// tag.
    #[must_use]
    pub fn registered_family(&self, pool_id: u64) -> Option<RegisteredPoolFamily> {
        self.pools.get(pool_id).map(PoolEntry::registered_family)
    }

    /// Get the pool ID for a given contract address.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pools.pool_id_by_address(address)
    }

    /// The address-keyed registration of record, family-tagged (PRG-1 /
    /// registry unification). `BotState` is the sole pool registry,
    /// so the `PyO3` construction door's flight pre-check consults this
    /// reader INSTEAD of a Python-mirror registry: an address this core
    /// already registered is answered here — identity straight off the
    /// registered entry — with no duplicate-handed builder replay and no
    /// terminal `AlreadyRegistered` refusal.
    ///
    /// V4 pools are NOT address-keyed (one `PoolManager` hosts many pool ids,
    /// keying is `(pool_manager, pool_id)`) and return `None` here; their
    /// family tag is resolved by [`Self::registered_family_at`] and their fast
    /// path is the existing `try_registered_v4`.
    #[must_use]
    pub fn registered_pool_by_address(
        &self,
        address: &Address,
    ) -> Option<(u64, RegisteredPoolFamily)> {
        let pool_id = self.pool_id_by_address(address)?;
        match self.pools.get(pool_id)?.registered_family() {
            // An address query cannot name a V4 pool (see the doc above).
            RegisteredPoolFamily::V4 => None,
            family => Some((pool_id, family)),
        }
    }

    /// The `(pool_manager, pool_id)`-keyed twin of
    /// [`Self::registered_pool_by_address`] (ADR-059 D1): V4 pools register
    /// under the pair, so this reader resolves a registered V4 pool's family
    /// tag. `None` for an unregistered pair.
    #[must_use]
    pub fn registered_family_at(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<RegisteredPoolFamily> {
        let id = self.pool_id_by_key(pool_manager, pool_id)?;
        Some(self.pools.get(id)?.registered_family())
    }

    /// The registered pool id for this exact pool identity, DERIVED from the
    /// registration tables this type already owns, so no second pool-id map
    /// exists anywhere.
    ///
    /// The read direction a driver needs when it holds a pool's *identity*
    /// (a family tag plus an address, or a V4 `(PoolManager, pool_id)` pair)
    /// and needs the id the engine hops on. It answers from the same
    /// address and V4 key indexes registration writes, so a
    /// driver cannot hold a mirror that disagrees with the state owner.
    ///
    /// Family is part of the answer: an address-keyed identity that names a
    /// different family than the one registered there is `None` rather than
    /// the other family's id.
    #[must_use]
    pub fn pool_id_for_identity(&self, identity: &PoolIdentity) -> Option<u64> {
        if let PoolIdentity::V4 {
            pool_manager,
            pool_id,
        } = identity
        {
            return self.pool_id_by_key(*pool_manager, pool_id);
        }
        let (pool_id, _) = self.registered_pool_by_address(&identity.address())?;
        (self.pool_family(pool_id) == Some(identity.family_tag())).then_some(pool_id)
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::primitives::{Address, B256, U256};
    use hashbrown::HashMap;

    use super::{BotState, PoolLookup, StateLock, TickInfo};
    use crate::state_lock::LockSite;
    use crate::{PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams, V4PoolKey};
    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_rpc::liquidity_verifier::LiquidityMapTarget;

    fn new_core() -> Arc<StateLock<BotState>> {
        Arc::new(StateLock::new(BotState::new()))
    }

    fn one_tick() -> HashMap<i32, TickInfo> {
        HashMap::from([(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: 100i128,
                block: 0,
            },
        )])
    }

    fn reg_v3(core: &mut BotState, address: Address) {
        core.register_v3_pool(&RegisterV3PoolParams {
            address,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: one_tick(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    }

    fn reg_v4(core: &mut BotState, pool_manager: Address, pool_id: V4PoolId) {
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            state_view: Address::ZERO,
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: one_tick(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V4 registration");
    }

    /// One table over the key shapes: a lookup projects its own family's
    /// contract target, tick spacing, tick map and key text, so these verbs are
    /// the only place the family is read.
    #[test]
    fn every_key_shape_projects_its_own_target_spacing_map_and_text() {
        let core = new_core();
        let address = Address::from([0x11u8; 20]);
        let pool_manager = Address::from([0x22u8; 20]);
        let pool_id = [0x33u8; 32];
        {
            let mut state = core.write_at(LockSite::Registration);
            reg_v3(&mut state, address);
            reg_v4(&mut state, pool_manager, pool_id);
        }

        let cases = [
            (
                PoolLookup::from_address(address),
                LiquidityMapTarget::V3(address),
                60,
                address.to_string(),
            ),
            (
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                },
                LiquidityMapTarget::V4 {
                    pool_manager,
                    pool_id: B256::from(pool_id),
                },
                10,
                format!("{pool_manager}:0x{}", alloy::hex::encode_prefixed(pool_id)),
            ),
        ];
        for (key, target, spacing, text) in cases {
            assert_eq!(key.liquidity_map_target(), target, "target for {key}");
            assert_eq!(key.tick_spacing(&core), Some(spacing), "spacing for {key}");
            assert_eq!(key.to_string(), text, "key text");
            let map = key
                .tick_map_snapshot(&core.read_at(LockSite::Registration))
                .expect("a registered pool has a tick map");
            assert_eq!(map.tick_spacing(), spacing, "map spacing for {key}");
            assert_eq!(map.tick_data().len(), 1, "map rows for {key}");
        }

        let unregistered = PoolLookup::from_address(Address::from([0xeeu8; 20]));
        assert_eq!(unregistered.tick_spacing(&core), None);
        assert!(unregistered
            .tick_map_snapshot(&core.read_at(LockSite::Registration))
            .is_none());
    }
}
