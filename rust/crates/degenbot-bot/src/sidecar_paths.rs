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
    /// V3 edges + their adjacency (the CL connector lane, B2-CL).
    v3_edges: Vec<V3Edge>,
    v3_by_pool: HashMap<u64, usize>,
    v3_by_address: HashMap<Address, usize>,
    v3_by_token: HashMap<u64, Vec<usize>>,
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

    /// The loaded V3 edge count (the B2-CL lane surface).
    #[must_use]
    pub fn v3_len(&self) -> usize {
        self.v3_edges.len()
    }
}

// ───────────────────────── V3 edges (DFYDYI B2-CL) ─────────────────────────

/// One V3 edge: identity + the seed facts admission needs (fee in the 1e6
/// convention + tick spacing) without a second scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Edge {
    pub pool_id: u64,
    pub token0_id: u64,
    pub token1_id: u64,
    pub address: Address,
    /// The pool's fee, `bips` of 1e6 (3000 = 0.3%) -- the composer's unit.
    pub fee: u32,
    pub tick_spacing: i32,
}

impl V2ConnectorIndex {
    /// Load the V3 edges (all v3-kind pools joined to their family table for
    /// fee + tick spacing). One scan; call once at startup after `load`.
    ///
    /// # Errors
    ///
    /// DB failures propagate (`DbError`).
    pub fn load_v3(&mut self, db: &DegenbotDb, chain_id: i64) -> Result<(), DbError> {
        // The DB emits `0x`-prefixed checksum addresses; alloy parses those.
        // (rows::decode::decode_address is pub(crate) to degenbot-db.)
        let conn = db.lock();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.token0_id, p.token1_id, p.address, v.fee_token0, v.tick_spacing \
             FROM pools p JOIN uniswap_v3_pools v ON v.pool_id = p.id WHERE p.chain = ?1",
        )?;
        let mut rows = stmt.query([chain_id])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let t0: i64 = row.get(1)?;
            let t1: i64 = row.get(2)?;
            let address: String = row.get(3)?;
            let fee: i64 = row.get(4)?;
            let tick_spacing: i64 = row.get(5)?;
            let (Ok(pool_id), Ok(token0_id), Ok(token1_id)) =
                (u64::try_from(id), u64::try_from(t0), u64::try_from(t1))
            else {
                continue;
            };
            let (Ok(fee), Ok(tick_spacing)) = (u32::try_from(fee), i32::try_from(tick_spacing))
            else {
                continue;
            };
            // The DB emits `0x`-prefixed checksum addresses; alloy parses
            // those directly (rows::decode::decode_address is pub(crate)).
            let address: Address = match address.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let edge = V3Edge {
                pool_id,
                token0_id,
                token1_id,
                address,
                fee,
                tick_spacing,
            };
            let idx = self.v3_edges.len();
            self.v3_by_pool.insert(edge.pool_id, idx);
            self.v3_by_address.insert(edge.address, idx);
            self.v3_by_token
                .entry(edge.token0_id)
                .or_default()
                .push(idx);
            self.v3_by_token
                .entry(edge.token1_id)
                .or_default()
                .push(idx);
            self.v3_edges.push(edge);
        }
        Ok(())
    }

    /// V3 connector candidates trading `token_id` against `quote_id`, same
    /// contract as [`Self::connectors`]: `(edge, token is token0)`.
    #[must_use]
    pub fn v3_connectors(
        &self,
        token_id: u64,
        quote_id: u64,
        exclude_pool: u64,
        limit: usize,
    ) -> Vec<(&V3Edge, bool)> {
        let mut out = Vec::new();
        let Some(idxs) = self.v3_by_token.get(&token_id) else {
            return out;
        };
        for &i in idxs {
            let e = &self.v3_edges[i];
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

    /// A V3 pool's edge by on-chain address.
    #[must_use]
    pub fn v3_edge_by_address(&self, address: Address) -> Option<&V3Edge> {
        self.v3_by_address.get(&address).map(|&i| &self.v3_edges[i])
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
            address: Address::new([u8::try_from(pool_id).unwrap_or(0); 20]),
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
