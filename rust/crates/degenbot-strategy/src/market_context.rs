//! Process-lifetime shared caches for pending-transaction strategies.
//!
//! A [`MarketContext`] owns the services every pending-transaction strategy
//! reads but none owns: the boot-time [`RouteRegistry`] handle (whose frozen
//! connector index backs the token joins and the discovery graph), the DB
//! handle behind the token id/address joins, the cross-block warm
//! bytecode/account cache, and the startup discovery graph built from that
//! index. Each is expensive to rebuild per transaction and safe to share
//! process-wide; a per-frame refill would re-pay a DB query per pool and
//! forfeit the index's memoized depth rankings.
//!
//! This is NOT strategy identity. A strategy's identity (which pools it
//! reacts to, how it selects candidates, how it prices them) lives in the
//! strategy that reads this context, not here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use alloy::primitives::Address;
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_db::connection::DegenbotDb;
use degenbot_simulation::WarmCodeCacheInner;
use parking_lot::RwLock;

use crate::anchored_dfs::AnchoredGraph;

/// The process-lifetime caches shared by every pending-transaction strategy.
pub struct MarketContext {
    /// The chain the connector index + DB id joins are keyed on.
    pub chain_id: i64,
    /// The boot-time pool world-view (connector index + frozen pool set +
    /// decoder mirror), shared by handle. `None` keeps the discovery fan shut
    /// (frames observe; connectors are never guessed).
    pub registry: Option<Arc<RouteRegistry>>,
    /// The DB handle the index was loaded from (token id/address joins).
    pub db: Option<DegenbotDb>,
    /// The discovery fan-out cap (`strategy.mevblocker_backrun`/`strategy.peer_backrun`).
    pub connector_cap: usize,
    /// The hop-depth cap per discovered cycle: the WETH-entry pin plus up to
    /// `cycle_max_hops - 1` connectors
    /// (`strategy.mevblocker_backrun`/`strategy.peer_backrun`).
    pub cycle_max_hops: usize,
    /// Cross-block warm bytecode/account cache owner, shared into every
    /// per-block replay handle.
    pub warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The startup-built discovery graph over the connector index's edge
    /// set (V2 + V3; `None` exactly when the index is `None` — the walker
    /// lane stays shut with it).
    pub dfs: Option<AnchoredGraph>,
    token_ids: Mutex<HashMap<Address, u64>>,
    token_addrs: Mutex<HashMap<u64, Address>>,
}

impl MarketContext {
    #[must_use]
    pub fn new(
        chain_id: i64,
        registry: Option<Arc<RouteRegistry>>,
        db: Option<DegenbotDb>,
        connector_cap: usize,
        cycle_max_hops: usize,
    ) -> Self {
        Self {
            chain_id,
            // Built from the registry's index BEFORE the handle moves in: one
            // startup graph pass.
            dfs: registry
                .as_ref()
                .map(|r| AnchoredGraph::from_connector_index(r.index())),
            registry,
            db,
            connector_cap,
            cycle_max_hops,
            warm_cache: WarmCodeCacheInner::shared_default(),
            token_ids: Mutex::new(HashMap::new()),
            token_addrs: Mutex::new(HashMap::new()),
        }
    }

    /// The frozen connector index behind the registry handle (`None` when the
    /// boot load failed or the DB was absent).
    #[must_use]
    pub fn index(&self) -> Option<&V2ConnectorIndex> {
        self.registry.as_ref().map(|r| r.index())
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
