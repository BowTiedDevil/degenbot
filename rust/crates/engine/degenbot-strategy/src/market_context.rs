//! Process-lifetime shared caches for pending-transaction strategies.
//!
//! A [`MarketContext`] owns the services every pending-transaction strategy
//! reads but none owns: the boot-resolved [`StrategyKit`] (the provisioning
//! ingress, the frozen [`RouteRegistry`] handle whose connector index backs
//! the token joins and the discovery graph, and the startup discovery graph),
//! the DB handle behind the token id/address joins, the cross-block warm
//! bytecode/account cache, and the token memos. The heavy handles are
//! expensive to rebuild per transaction and safe to share process-wide; a
//! per-frame refill would re-pay a DB query per pool and forfeit the index's
//! memoized depth rankings.
//!
//! This is NOT strategy identity. A strategy's identity (which pools it
//! reacts to, how it selects candidates, how it prices them) lives in the
//! strategy that reads this context, not here. The kit is the composition
//! surface; the context is the per-frame view onto it.

use std::sync::{Arc, Mutex, PoisonError};

use hashbrown::HashMap;

use alloy::primitives::Address;
use degenbot_db::connection::DegenbotDb;
use degenbot_simulation::sim::evm2::warm_code_cache::DeltaDispatchConfig;
use degenbot_simulation::WarmCodeCacheInner;
use degenbot_substrate::connector_index::V2ConnectorIndex;
use degenbot_substrate::pool_ingress::{PoolIngress, VerifyLevel};
use degenbot_substrate::RouteRegistry;
use parking_lot::RwLock;

use crate::anchored_dfs::AnchoredGraph;
use crate::strategy_kit::StrategyKit;

/// The process-lifetime caches shared by every pending-transaction strategy.
pub struct MarketContext {
    /// The chain the connector index + DB id joins are keyed on.
    pub chain_id: i64,
    /// The discovery fan-out cap (`strategy.mevblocker_backrun`/`strategy.txpool_backrun`).
    pub connector_cap: usize,
    /// The hop-depth cap per discovered cycle: the WETH-entry pin plus up to
    /// `cycle_max_hops - 1` connectors
    /// (`strategy.mevblocker_backrun`/`strategy.txpool_backrun`).
    pub cycle_max_hops: usize,
    /// Cross-block warm bytecode/account cache owner, shared into every
    /// per-block replay handle.
    pub warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The DB handle the index was loaded from: the token id/address joins.
    /// Shared behind an `Arc` so the ingress and the token joins read the same
    /// held connection.
    db: Option<Arc<DegenbotDb>>,
    /// The boot-resolved composition the context views over.
    kit: StrategyKit,
    token_ids: Mutex<HashMap<Address, u64>>,
    token_addrs: Mutex<HashMap<u64, Address>>,
}

impl MarketContext {
    /// Build the per-frame view over the boot-resolved [`StrategyKit`].
    ///
    /// `db` is the same held connection the kit's ingress was constructed
    /// from; the context's token joins read it directly.
    ///
    /// The context's warm cache is built here, once: it carries the BAL
    /// dispatch opt-in exactly when the typed `simulation.delta_dispatch` flag
    /// (default off) says so. The flag is read from the installed config
    /// snapshot at construction — never re-read per frame — so one context's
    /// storage lane cannot flip under the frames viewing it.
    #[must_use]
    pub fn new(
        chain_id: i64,
        db: Option<Arc<DegenbotDb>>,
        kit: StrategyKit,
        connector_cap: usize,
        cycle_max_hops: usize,
    ) -> Self {
        Self {
            chain_id,
            connector_cap,
            cycle_max_hops,
            warm_cache: warm_cache_for(degenbot_config::holder::config().simulation.delta_dispatch),
            db,
            kit,
            token_ids: Mutex::new(HashMap::new()),
            token_addrs: Mutex::new(HashMap::new()),
        }
    }

    /// The provisioning ingress view (the kit's provision cell).
    #[must_use]
    pub fn ingress(&self) -> &PoolIngress {
        self.kit.ingress()
    }

    /// The frame's chain-sample policy, passed to every ingress admission.
    #[must_use]
    pub fn verify_level(&self) -> VerifyLevel {
        self.kit.verify_level()
    }

    /// The frozen registry handle (`None` when the discovery fan is shut).
    #[must_use]
    pub fn registry(&self) -> Option<&Arc<RouteRegistry>> {
        self.kit.registry()
    }

    /// The startup discovery graph (`None` exactly when the registry is `None`
    /// — the walker lane stays shut with it).
    #[must_use]
    pub fn dfs(&self) -> Option<&AnchoredGraph> {
        self.kit.dfs()
    }

    /// The frozen connector index behind the registry handle (`None` when the
    /// boot load failed or the DB was absent).
    #[must_use]
    pub fn index(&self) -> Option<&V2ConnectorIndex> {
        self.registry().map(|r| r.index())
    }

    /// DB id for a token address (memoized across frames). `None` when the
    /// token is absent from the workspace DB or no DB is open.
    #[must_use]
    pub fn token_id(&self, addr: Address) -> Option<u64> {
        let mut ids = self
            .token_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = ids.get(&addr).copied() {
            return Some(hit);
        }
        let db = self.db.as_ref()?;
        let found = db
            .fetch_token_ids_by_address(self.chain_id, &[addr])
            .ok()?
            .into_iter()
            .next()
            .map(|(_, id)| id)?;
        ids.insert(addr, found);
        Some(found)
    }

    /// Address for a token DB id (memoized across frames).
    #[must_use]
    pub fn token_addr(&self, id: u64) -> Option<Address> {
        let mut addrs = self
            .token_addrs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = addrs.get(&id).copied() {
            return Some(hit);
        }
        let id64 = i64::try_from(id).ok()?;
        let row = self.db.as_ref()?.fetch_token_by_id(id64).ok()??;
        let addr = row.address;
        addrs.insert(id, addr);
        Some(addr)
    }
}

/// The context's warm-cache owner: the pure-Rust default (no hook store),
/// carrying the BAL dispatch config exactly when the typed
/// `simulation.delta_dispatch` flag is on. One owner either way — off is the
/// shipped [`WarmCodeCacheInner::shared_default`] shape, on is the same shape
/// plus the opt-in, so nothing about the default cache moves.
fn warm_cache_for(delta_dispatch: bool) -> Arc<RwLock<WarmCodeCacheInner>> {
    if delta_dispatch {
        WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development())
    } else {
        WarmCodeCacheInner::shared_default()
    }
}

#[cfg(test)]
mod tests {
    //! The construction mapping the typed `simulation.delta_dispatch` flag has
    //! onto the context's warm-cache owner: the flag is read ONCE here, OFF is
    //! the shipped pure-Rust default, and ON installs the session's one shared
    //! delta driver rather than a per-frame copy.

    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use super::{warm_cache_for, Arc};
    use degenbot_simulation::sim::evm2::warm_code_cache::DeltaDispatchConfig;

    /// A provider with no node behind it: constructing the driver and sharing
    /// it issues no request, so the identity assertion needs no endpoint.
    fn offline_provider() -> degenbot_rpc::provider::AlloyProvider {
        use alloy::network::Ethereum;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};

        let client = ClientBuilder::default().transport(MockTransport::new(Asserter::new()), true);
        let inner = ProviderBuilder::new().connect_client(client).erased();
        degenbot_rpc::provider::AlloyProvider::from_provider(
            Arc::new(inner) as Arc<dyn Provider<Ethereum>>
        )
    }

    /// OFF (the default) installs no dispatch config, so a fan-out over this
    /// context selects the legacy memo + prefetch lane and requests no BAL —
    /// the driver is not even constructed until an opt-in asks for it.
    #[test]
    fn the_default_context_installs_no_dispatch_opt_in() {
        let cache = warm_cache_for(false);
        assert_eq!(
            cache.read().delta_dispatch(),
            None,
            "the default carries no config: the legacy lane, no BAL request"
        );
    }

    /// ON carries the development opt-in on the same one owner, and the
    /// session-scoped driver is shared: two fan-outs at one head hold the same
    /// driver, not a copy each.
    #[test]
    fn the_enabled_context_carries_the_opt_in_and_one_shared_driver() {
        let cache = warm_cache_for(true);
        assert_eq!(
            cache.read().delta_dispatch(),
            Some(DeltaDispatchConfig::development()),
            "the enabled owner is the development opt-in"
        );

        let provider = offline_provider();
        let first = cache
            .read()
            .delta_heads(&provider)
            .expect("the opt-in installs a driver");
        let second = cache
            .read()
            .delta_heads(&provider)
            .expect("the installed driver is handed back");
        assert!(
            Arc::ptr_eq(&first, &second),
            "one session-scoped driver: a later fan-out shares it instead of duplicating the BAL work"
        );
    }
}
