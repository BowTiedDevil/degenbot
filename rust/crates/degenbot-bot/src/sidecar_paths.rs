//! DB-backed V2 connector index for the sidecar frame solver (epic DFYDYI,
//! task B3): one startup load of the unified `pools` table's V2 edges, an
//! adjacency map by token id, and the two-hop candidate expansion the solver
//! needs: "other pools trading TOKEN against WETH".
//!
//! This is the Rust-native answer to the discovery prototype's adjacency
//! join (610k pools -> sub-ms connector lookups) — the DB read happens ONCE
//! at sidecar startup, never per frame.

use std::collections::HashMap;

use alloy::primitives::Address;
use degenbot_db::connection::DegenbotDb;
use degenbot_db::error::DbError;
use degenbot_pathfinding::PoolKind;

/// One V2 edge of the connector index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Edge {
    pub pool_id: u64,
    pub token0_id: u64,
    pub token1_id: u64,
    pub address: Address,
}

/// Startup-loaded V2 adjacency (token id -> edges).
#[derive(Debug, Default)]
pub struct V2ConnectorIndex {
    edges: Vec<V2Edge>,
    by_pool: HashMap<u64, usize>,
    by_address: HashMap<Address, usize>,
    by_token: HashMap<u64, Vec<usize>>,
}

impl V2ConnectorIndex {
    /// Load the V2 edges for `chain_id` from the DB (one scan, no per-frame
    /// queries).
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`).
    pub fn load(db: &DegenbotDb, chain_id: i64) -> Result<Self, DbError> {
        let data = db.fetch_path_graph_edges(chain_id, &[PoolKind::V2])?;
        let mut index = Self::default();
        index.edges.reserve(data.edges.len());
        // PathEdge is (token0_id, token1_id, pool_id, kind) -- the pool id
        // rides THIRD (see fetch_path_graph_edges' push order).
        for (t0, t1, pool_id, kind) in &data.edges {
            if *kind != PoolKind::V2 {
                continue;
            }
            let Some(&address) = data.v2v3_addresses.get(pool_id) else {
                continue;
            };
            index.push_edge(V2Edge {
                pool_id: *pool_id,
                token0_id: *t0,
                token1_id: *t1,
                address,
            });
        }
        Ok(index)
    }

    fn push_edge(&mut self, edge: V2Edge) {
        let idx = self.edges.len();
        self.by_pool.insert(edge.pool_id, idx);
        self.by_address.insert(edge.address, idx);
        self.by_token.entry(edge.token0_id).or_default().push(idx);
        self.by_token.entry(edge.token1_id).or_default().push(idx);
        self.edges.push(edge);
    }

    /// The edge of a known pool id.
    #[must_use]
    pub fn pool_edge(&self, pool_id: u64) -> Option<&V2Edge> {
        self.by_pool.get(&pool_id).map(|&i| &self.edges[i])
    }

    /// The edge of a pool by on-chain address (the feed frames name pools by
    /// address; the index id map resolves them).
    #[must_use]
    pub fn edge_by_address(&self, address: Address) -> Option<&V2Edge> {
        self.by_address.get(&address).map(|&i| &self.edges[i])
    }

    /// Connector candidates for a two-hop WETH-denominated backrun: pools
    /// trading `token_id` against `quote_id` (usually WETH), excluding
    /// `exclude_pool` (the affected pool). Returns `(pool edge, token is
    /// token0)` so the caller can orient hops without re-deriving. Bounded
    /// by `limit` (insertion order — the caller ranks by live liquidity).
    #[must_use]
    pub fn connectors(
        &self,
        token_id: u64,
        quote_id: u64,
        exclude_pool: u64,
        limit: usize,
    ) -> Vec<(&V2Edge, bool)> {
        let mut out = Vec::new();
        let Some(idxs) = self.by_token.get(&token_id) else {
            return out;
        };
        for &i in idxs {
            let e = &self.edges[i];
            if e.pool_id == exclude_pool {
                continue;
            }
            if e.token0_id == token_id && e.token1_id == quote_id {
                out.push((e, true));
            } else if e.token1_id == token_id && e.token0_id == quote_id {
                out.push((e, false));
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(pool_id: u64, t0: u64, t1: u64) -> V2Edge {
        V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: Address::new([pool_id as u8; 20]),
        }
    }

    #[test]
    fn adjacency_and_orientation() {
        let mut ix = V2ConnectorIndex::default();
        ix.push_edge(edge(1, 20, 10));
        ix.push_edge(edge(2, 10, 20));
        ix.push_edge(edge(3, 20, 30));

        // Connectors for TOK=20 against quote=10: pools 1 (tok is t0) and
        // 2 (tok is t1); pool 3 doesn't touch 10.
        let c = ix.connectors(20, 10, 0, 10);
        assert_eq!(c.len(), 2, "both V2 edges trading (20,10) surface");
        assert_eq!(c[0].0.pool_id, 1);
        assert!(c[0].1, "pool 1 has TOK as token0");
        assert_eq!(c[1].0.pool_id, 2);
        assert!(!c[1].1, "pool 2 has TOK as token1");

        // Exclusion drops the affected pool.
        let c = ix.connectors(20, 10, 2, 10);
        assert_eq!(c.len(), 1, "only pool 1 after excluding 2");

        // Limit bounds the fan-out.
        let c = ix.connectors(20, 10, 0, 1);
        assert_eq!(c.len(), 1);

        assert!(ix.pool_edge(3).is_some());
        assert!(ix.pool_edge(99).is_none());
    }
}
