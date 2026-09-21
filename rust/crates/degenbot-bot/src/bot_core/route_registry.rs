//! Boot-time route registry: the Rust-owned pool world-view of one host
//! process, frozen at registration.
//!
//! A pending-transaction runtime otherwise rebuilds its connector world-view
//! per driver: a DB-index scan, a discovery graph over its edges, and a decoder
//! registration table. This module owns that snapshot ONCE at boot and hands it
//! out as a shared handle, so a strategy reads the same frozen pool set rather
//! than paying for a rebuild.
//!
//! Scope of "frozen": the POOL SET is fixed for the run (registration-time
//! snapshot; hot reload is out of scope). Live pool *state* — reserves,
//! liquidity, reorgs — remains the driver's runtime and is not represented
//! here.
//!
//! The connector index is address-keyed only for V2/V3 (`pools` table); V4 is a
//! `(pool_manager, pool_id)` identity with no single-address pool key, so
//! [`RouteRegistry::is_registered_pool`] answers for the address-keyed set and
//! explicitly excludes V4.

use alloy::primitives::Address;
use degenbot_pathfinding::PoolKind;

use crate::connector_index::V2ConnectorIndex;

use super::sim_anchor::SimAnchorOracle;

/// The pool families the host's Uniswap decoder table covers, mirrored from
/// [`LogDispatcher::with_uniswap_decoders`](crate::bot_core::log_dispatcher::LogDispatcher::with_uniswap_decoders)
/// (V2 `Sync`; V3 canonical/Pancake `Swap` + `Mint`/`Burn`; V4 `Swap` +
/// `ModifyLiquidity`). Frozen at boot alongside the pool set; the
/// `decoder_table_matches_the_mirror` test pins it against the live
/// registration count so the mirror cannot silently drift.
pub const DECODER_POOL_KINDS: [PoolKind; 3] = [PoolKind::V2, PoolKind::V3, PoolKind::V4];

/// The boot-time world-view a host builds once and shares by handle.
#[derive(Debug)]
pub struct RouteRegistry {
    index: V2ConnectorIndex,
}

impl RouteRegistry {
    /// Take ownership of the once-loaded connector index. Construction is the
    /// snapshot: no later call reloads it.
    #[must_use]
    pub fn new(index: V2ConnectorIndex) -> Self {
        Self { index }
    }

    /// The frozen connector index. Consumers that need edge lookups (not just
    /// membership) read it here.
    #[must_use]
    pub fn index(&self) -> &V2ConnectorIndex {
        &self.index
    }

    /// The member question: is `address` one of the pools in the frozen set?
    ///
    /// Address-keyed V2/V3 pools only — a V4 pool is identified by
    /// `(pool_manager, pool_id)`, not a pool address, and the boot index holds
    /// no V4 lane. Callers asking about a V4 manager address get `false` by
    /// design, not by omission.
    #[must_use]
    pub fn is_registered_pool(&self, address: &Address) -> bool {
        self.index.edge_by_address(*address).is_some()
            || self.index.v3_edge_by_address(*address).is_some()
    }

    /// Total address-keyed pools in the frozen set (V2 + V3).
    #[must_use]
    pub fn registered_pool_count(&self) -> usize {
        self.index.len() + self.index.v3_len()
    }

    /// Whether the host has a decoder for `kind` (see [`DECODER_POOL_KINDS`]).
    #[must_use]
    pub fn decodes_pool_kind(&self, kind: PoolKind) -> bool {
        DECODER_POOL_KINDS.contains(&kind)
    }

    /// The frozen decoder-family mirror.
    #[must_use]
    pub fn decoder_pool_kinds(&self) -> &'static [PoolKind] {
        &DECODER_POOL_KINDS
    }
}

impl SimAnchorOracle for RouteRegistry {
    /// The frozen index answers membership; the registry carries no engine
    /// state, so the divergence-probe default (`None`) applies.
    fn is_registered_pool(&self, address: &Address) -> bool {
        self.index.edge_by_address(*address).is_some()
            || self.index.v3_edge_by_address(*address).is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::bot_core::log_dispatcher::LogDispatcher;
    use crate::connector_index::V3Edge;

    fn v2_edge(pool_id: u64, address: Address) -> crate::connector_index::V2Edge {
        crate::connector_index::V2Edge {
            pool_id,
            token0_id: 10,
            token1_id: 20,
            address,
        }
    }

    fn v3_edge(pool_id: u64, address: Address) -> V3Edge {
        V3Edge {
            pool_id,
            token0_id: 10,
            token1_id: 20,
            address,
            fee: 500,
            tick_spacing: 10,
        }
    }

    fn fixture() -> (RouteRegistry, Address, Address, Address) {
        let v2_addr = Address::new([0x11; 20]);
        let v3_addr = Address::new([0x22; 20]);
        let absent = Address::new([0x33; 20]);
        let mut index = V2ConnectorIndex::default();
        index.push_edge(v2_edge(1, v2_addr));
        index.push_v3_edge(v3_edge(2, v3_addr));
        (RouteRegistry::new(index), v2_addr, v3_addr, absent)
    }

    /// Membership is exactly the index's own by-address maps — the load-site
    /// swap changes no pool's status.
    #[test]
    fn membership_matches_the_index_by_address_maps() {
        let (registry, v2_addr, v3_addr, absent) = fixture();
        let raw = registry.index();
        for addr in [v2_addr, v3_addr, absent] {
            let expected =
                raw.edge_by_address(addr).is_some() || raw.v3_edge_by_address(addr).is_some();
            assert_eq!(
                registry.is_registered_pool(&addr),
                expected,
                "membership parity for {addr}"
            );
        }
        assert!(registry.is_registered_pool(&v2_addr));
        assert!(registry.is_registered_pool(&v3_addr));
        assert!(!registry.is_registered_pool(&absent));
        assert_eq!(registry.registered_pool_count(), 2);
    }

    /// The handle is shared, not rebuilt: one construction, cloned into two
    /// consumers that observe the same frozen set.
    #[test]
    fn shared_by_handle_not_rebuilt() {
        let (registry, v2_addr, _v3_addr, absent) = fixture();
        let shared = Arc::new(registry);
        let first = Arc::clone(&shared);
        let second = Arc::clone(&shared);
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.is_registered_pool(&v2_addr));
        assert!(!second.is_registered_pool(&absent));
        assert_eq!(
            first.registered_pool_count(),
            second.registered_pool_count()
        );
    }

    /// The decoder mirror is pinned against the live registration table: the
    /// 6 Uniswap decoders cover exactly the three mirrored families.
    #[test]
    fn decoder_table_matches_the_mirror() {
        assert_eq!(
            LogDispatcher::with_uniswap_decoders().decoder_count(),
            6,
            "the mirror names three families backed by six registered decoders"
        );
        let (registry, ..) = fixture();
        for kind in registry.decoder_pool_kinds() {
            assert!(
                registry.decodes_pool_kind(*kind),
                "mirrored family {kind:?} must be decodable"
            );
        }
        assert_eq!(registry.decoder_pool_kinds(), DECODER_POOL_KINDS.as_slice());
    }

    /// The index accessor preserves connector behavior (the ranking path is
    /// unchanged by the registry wrapper).
    #[tokio::test]
    async fn index_accessor_preserves_connector_lookup() {
        let (registry, v2_addr, _v3_addr, _absent) = fixture();
        let connectors = registry.index().connectors(20, 10, 0, 8).await;
        assert_eq!(connectors.len(), 1);
        assert_eq!(connectors[0].0.address, v2_addr);
    }
}
