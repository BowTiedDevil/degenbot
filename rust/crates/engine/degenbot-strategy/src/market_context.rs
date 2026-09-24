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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use alloy::primitives::Address;
use degenbot_bot::bot_core::pool_ingress::PoolIngress;
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_db::connection::DegenbotDb;
use degenbot_simulation::WarmCodeCacheInner;
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
            warm_cache: WarmCodeCacheInner::shared_default(),
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
