//! Pathfinding graph-construction read fns — the bulk SQL selects that turn
//! a degenbot `SQLite` DB into a [`PathGraphData`] (flat edge list +
//! address lookups) composable with
//! [`degenbot_pathfinding::PathGraph::from_edges`], without Python.
//!
//! Mirrors `src/degenbot/pathfinding.py` `_prepare_graph` / `_get_tokens_with_min_degree`
//! (the parity oracle — §4.2): same query shapes, same row transforms. The
//! Python path positions pathfinding DISCOVERY reads against the DB
//! (`db=bot.db` in `find_paths`/`find_paths_async`); these fns expose that
//! same substrate to standalone Rust consumers (ADR-005 standalone path:
//! `DB → fetch_path_graph_edges → PathGraph::from_edges`).
//!
//! # Decomposition
//!
//! The Python `_prepare_graph` conflates two concerns: degree filtering
//! (`_get_tokens_with_min_degree`) and edge selection. Here they are split
//! into three composable fns so a standalone consumer can drive graph
//! construction at any granularity:
//!
//! 1. [`DegenbotDb::fetch_tokens_with_min_degree`] — the `GROUP BY token
//!    HAVING count(*) >= degree` candidate-token set (§2.1 warm path:
//!    construction-time graph build per arbitrage-session startup).
//! 2. [`DegenbotDb::fetch_path_graph_edges`] — ALL chain-filtered edges
//!    (unfiltered; the caller applies the degree whitelist just like the
//!    Python `_prepare_graph` does inline).
//! 3. [`DegenbotDb::fetch_token_ids_by_address`] — start/end +
//!    `allowed_intermediate_tokens` resolution.
//!
//! All three are pure row→struct transforms over [`DegenbotDb`] (§2.1 warm
//! path; construction-time graph build per arbitrage-session startup, not
//! per-tx).

use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use degenbot_pathfinding::PoolKind;

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::decode_address;
use crate::schema::table::{
    is_v2_kind, is_v3_kind, MANAGED_POOLS, POOLS, POOL_MANAGERS, UNISWAP_V4_POOLS,
};

/// A pool edge in the pathfinding graph: `(token0_id, token1_id, pool_id, pool_kind)`.
///
/// Composes directly with
/// [`degenbot_pathfinding::PathGraph::from_edges`](degenbot_pathfinding::PathGraph::from_edges).
pub type PathEdge = (u64, u64, u64, PoolKind);

/// Offset applied to V4 pool ids in the pathfinding graph's edge list.
///
/// The V2/V3 `pools.id` counter and the V4 `managed_pools.id` counter are
/// INDEPENDENT, so a bare numeric id is ambiguous: on mainnet, 116k of 125k
/// V4 pool ids collide with a V2/V3 id (measured). A collided pair put TWO
/// edges under one `pool_id`, so the DFS walked e.g. WETH→USDC through one
/// real pool and USDT→WETH through another while believing it used ONE pool
/// — yielding paths that cannot close (148,896 broken paths observed live;
/// surfaced as 85k `direction-fail` registration skips).
///
/// Every emitted V4 id is `managed_pool_id + [`V4_POOL_ID_OFFSET`]`, which
/// places V4 ids strictly above any realistic `pools.id` (mainnet max
/// ~678k). Consumers demangle with [`demangle_v4_pool_id`] / [`is_v4_graph_id`].
pub const V4_POOL_ID_OFFSET: u64 = 1 << 32;

/// Whether a graph-level pool id carries the V4 offset.
#[must_use]
pub const fn is_v4_graph_id(graph_id: u64) -> bool {
    graph_id >= V4_POOL_ID_OFFSET
}

/// Recover the raw `managed_pools.id` from an offset V4 graph id.
#[must_use]
pub const fn demangle_v4_pool_id(graph_id: u64) -> u64 {
    graph_id - V4_POOL_ID_OFFSET
}

/// The flat edge list + address lookup maps produced by
/// [`DegenbotDb::fetch_path_graph_edges`] — the Rust analogue of the Python
/// `_PreparedGraph`.
///
/// # Fields
///
/// - `edges`: flat `(token0_id, token1_id, pool_id, pool_kind)` tuples. V2/V3
///   `pool_id`s come from the `pools` table; V4 `pool_id`s come from
///   `managed_pools.id` (= `uniswap_v4_pools.managed_pool_id`). The V2/V3 ID
///   space and the V4 ID space are disjoint (separate tables), so a
///   `pool_kind` discriminant disambiguates V4 from V2/V3.
/// - `v2v3_addresses`: V2/V3 `pool_id` → on-chain `address` (from `pools`).
/// - `v4_lookups`: V4 `pool_id` → `(manager_address, pool_hash)` (from the
///   `uniswap_v4_pools` × `pool_managers` join).
/// - `pool_id_to_kind`: `pool_id` → `pool_kind` for path reconstruction.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathGraphData {
    /// Flat edge tuples for `PathGraph::from_edges`.
    pub edges: Vec<PathEdge>,
    /// V2/V3 `pool_id` → on-chain address.
    pub v2v3_addresses: HashMap<u64, Address>,
    /// V4 `pool_id` → `(manager_address, pool_hash)`.
    pub v4_lookups: HashMap<u64, (Address, String)>,
    /// `pool_id` → `pool_kind` (for path reconstruction).
    pub pool_id_to_kind: HashMap<u64, PoolKind>,
    /// `pool_id` → the raw `kind` STRING from the DB (e.g. `"uniswap_v3"`,
    /// `"sushiswap_v4"`) — the single-table-inheritance polymorphic identity.
    /// Preserved so the `PyO3` seam can reconstruct the exact concrete
    /// `PathStep.type` class (Python maps `kind_string → pool_type` via
    /// `pool_type.__mapper__.polymorphic_identity`), avoiding the V2/V3
    /// collapse that `pool_id_to_kind` performs (AF7OEL strict parity).
    pub pool_id_to_kind_string: HashMap<u64, String>,
}

impl DegenbotDb {
    /// Bulk-select the flat pathfinding edge list + address lookups for a
    /// chain, mirroring the `select(...)` inside Python's `_prepare_graph`
    /// (the V2/V3 branch selects `id, token0_id, token1_id, address, kind`
    /// from `pools WHERE chain=?`; the V4 branch joins `uniswap_v4_pools` ×
    /// `pool_managers WHERE pool_managers.chain=?`).
    ///
    /// Returns ALL chain-filtered edges (both pool kinds requested);
    /// degree-based candidate filtering is delegated to the caller via
    /// [`Self::fetch_tokens_with_min_degree`] (matching the Python
    /// `_prepare_graph`'s inline `candidate_tokens` intersection, but split
    /// out for composable standalone use).
    ///
    /// # Pool-kind mapping (mirrors Python `_pool_kind_for_type`)
    ///
    /// The unified `pools` table holds V2 + V3 rows (V4 pools use the
    /// `managed_pools` polymorphic base, not `pools`); each row's `kind`
    /// discriminator is classified via [`is_v2_kind`] / [`is_v3_kind`] into
    /// [`PoolKind::V2`] / [`PoolKind::V3`]. Only rows whose family is in
    /// `pool_kinds` are emitted.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed address column.
    pub fn fetch_path_graph_edges(
        &self,
        chain_id: i64,
        pool_kinds: &[PoolKind],
    ) -> Result<PathGraphData, DbError> {
        let want_v2 = pool_kinds.contains(&PoolKind::V2);
        let want_v3 = pool_kinds.contains(&PoolKind::V3);
        let want_v4 = pool_kinds.contains(&PoolKind::V4);

        let mut data = PathGraphData::default();
        let conn = self.lock();

        // ── V2/V3 edges: the unified `pools` table ──────────────────────
        // token0_id / token1_id / id are INTEGER columns (SQLAlchemy
        // ForeignKey / PrimaryKeyInt); address is VARCHAR(42) checksum.
        if want_v2 || want_v3 {
            let mut stmt = conn.prepare(&format!(
                "SELECT id, token0_id, token1_id, address, kind \
                 FROM {POOLS} WHERE chain = ?1"
            ))?;
            let mut rows = stmt.query(rusqlite::params![chain_id])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let token0_id: i64 = row.get(1)?;
                let token1_id: i64 = row.get(2)?;
                let address: String = row.get(3)?;
                let kind: String = row.get(4)?;

                let pool_kind = if is_v2_kind(&kind) {
                    PoolKind::V2
                } else if is_v3_kind(&kind) {
                    PoolKind::V3
                } else {
                    // Skip non-V2/V3 rows (base/unknown kinds) — mirrors the
                    // Python `_pool_kind_for_type` raise, but a silent skip
                    // keeps the bulk select resiliente to forward-kind rows.
                    continue;
                };

                let included = match pool_kind {
                    PoolKind::V2 => want_v2,
                    PoolKind::V3 => want_v3,
                    _ => false, // V4 not in the `pools` table
                };
                if !included {
                    continue;
                }

                let pool_id = u64::try_from(id)
                    .map_err(|e| DbError::Decode(format!("pool id {id} out of u64 range: {e}")))?;
                let t0 = u64::try_from(token0_id).map_err(|e| {
                    DbError::Decode(format!("token0_id {token0_id} out of u64 range: {e}"))
                })?;
                let t1 = u64::try_from(token1_id).map_err(|e| {
                    DbError::Decode(format!("token1_id {token1_id} out of u64 range: {e}"))
                })?;
                data.edges.push((t0, t1, pool_id, pool_kind));
                data.v2v3_addresses
                    .insert(pool_id, decode_address(&address)?);
                data.pool_id_to_kind.insert(pool_id, pool_kind);
                data.pool_id_to_kind_string.insert(pool_id, kind.clone());
            }
        }

        // ── V4 edges: `uniswap_v4_pools` × `managed_pools` × `pool_managers`
        // The V4 `pool_id` is `managed_pools.id` (= the polymorphic base PK,
        // = `uniswap_v4_pools.managed_pool_id`). The `pool_hash` is stored
        // as a `0x`-prefixed lowercase hex `VARCHAR`. `manager.address` is
        // VARCHAR(42) checksum.
        if want_v4 {
            let mut stmt = conn.prepare(&format!(
                "SELECT {MANAGED_POOLS}.id, \
                 {UNISWAP_V4_POOLS}.currency0_id, {UNISWAP_V4_POOLS}.currency1_id, \
                 {POOL_MANAGERS}.address, {UNISWAP_V4_POOLS}.pool_hash \
                 FROM {UNISWAP_V4_POOLS} \
                 JOIN {MANAGED_POOLS} ON {MANAGED_POOLS}.id = {UNISWAP_V4_POOLS}.managed_pool_id \
                 JOIN {POOL_MANAGERS} ON {POOL_MANAGERS}.id = {MANAGED_POOLS}.manager_id \
                 WHERE {POOL_MANAGERS}.chain = ?1"
            ))?;
            let mut rows = stmt.query(rusqlite::params![chain_id])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let currency0_id: i64 = row.get(1)?;
                let currency1_id: i64 = row.get(2)?;
                let manager_address: String = row.get(3)?;
                let pool_hash: String = row.get(4)?;

                let pool_id = u64::try_from(id).map_err(|e| {
                    DbError::Decode(format!("v4 pool id {id} out of u64 range: {e}"))
                })?;
                let c0 = u64::try_from(currency0_id).map_err(|e| {
                    DbError::Decode(format!("currency0_id {currency0_id} out of u64 range: {e}"))
                })?;
                let c1 = u64::try_from(currency1_id).map_err(|e| {
                    DbError::Decode(format!("currency1_id {currency1_id} out of u64 range: {e}"))
                })?;
                // Namespace the V4 id above every possible `pools.id` so a
                // collided V2/V3 row cannot alias this pool's edges or maps
                // (see [`V4_POOL_ID_OFFSET`]).
                let graph_id = pool_id + V4_POOL_ID_OFFSET;
                data.edges.push((c0, c1, graph_id, PoolKind::V4));
                data.v4_lookups
                    .insert(graph_id, (decode_address(&manager_address)?, pool_hash));
                data.pool_id_to_kind.insert(graph_id, PoolKind::V4);
                data.pool_id_to_kind_string
                    .insert(graph_id, "uniswap_v4".to_string());
            }
        }

        Ok(data)
    }

    /// The `GROUP BY token HAVING count(*) >= degree` candidate-token set.
    ///
    /// Mirrors Python's `_get_tokens_with_min_degree`: `UNION ALL` of the
    /// token0/token1 (V2/V3) and currency0/currency1 (V4) columns across the
    /// requested pool kinds, grouped by token id, filtered to tokens
    /// appearing in `>= degree` pools.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_tokens_with_min_degree(
        &self,
        chain_id: i64,
        degree: u32,
        pool_kinds: &[PoolKind],
    ) -> Result<HashSet<u64>, DbError> {
        if pool_kinds.is_empty() {
            return Ok(HashSet::new());
        }
        let want_v2 = pool_kinds.contains(&PoolKind::V2);
        let want_v3 = pool_kinds.contains(&PoolKind::V3);
        let want_vu4 = pool_kinds.contains(&PoolKind::V4);
        let want_v2v3 = want_v2 || want_v3;

        if !want_v2v3 && !want_vu4 {
            return Ok(HashSet::new());
        }

        // Build the UNION ALL of token columns. Each sub-select yields one
        // `token_id` column. The outer query groups + counts.
        let mut sub_selects: Vec<String> = Vec::new();
        if want_v2v3 {
            sub_selects.push(format!(
                "SELECT token0_id AS token_id FROM {POOLS} WHERE chain = ?1"
            ));
            sub_selects.push(format!(
                "SELECT token1_id AS token_id FROM {POOLS} WHERE chain = ?1"
            ));
        }
        if want_vu4 {
            sub_selects.push(format!(
                "SELECT currency0_id AS token_id \
                 FROM {UNISWAP_V4_POOLS} \
                 JOIN {MANAGED_POOLS} ON {MANAGED_POOLS}.id = {UNISWAP_V4_POOLS}.managed_pool_id \
                 JOIN {POOL_MANAGERS} ON {POOL_MANAGERS}.id = {MANAGED_POOLS}.manager_id \
                 WHERE {POOL_MANAGERS}.chain = ?1"
            ));
            sub_selects.push(format!(
                "SELECT currency1_id AS token_id \
                 FROM {UNISWAP_V4_POOLS} \
                 JOIN {MANAGED_POOLS} ON {MANAGED_POOLS}.id = {UNISWAP_V4_POOLS}.managed_pool_id \
                 JOIN {POOL_MANAGERS} ON {POOL_MANAGERS}.id = {MANAGED_POOLS}.manager_id \
                 WHERE {POOL_MANAGERS}.chain = ?1"
            ));
        }

        let union = sub_selects.join(" UNION ALL ");
        let sql = format!(
            "SELECT token_id, COUNT(*) AS c FROM ({union}) \
             GROUP BY token_id HAVING c >= ?2"
        );

        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let degree_val = i64::from(degree);
        let rows = stmt.query_map(rusqlite::params![chain_id, degree_val], |row| {
            let id: i64 = row.get(0)?;
            u64::try_from(id).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?;
        let mut out = HashSet::new();
        for tid in rows {
            out.insert(tid?);
        }
        Ok(out)
    }

    /// Resolve `Address` → `TokenId` (`erc20_tokens.id`) for a batch of
    /// addresses on a chain.
    ///
    /// Mirrors Python's `select(Erc20TokenTable.id).where(address.in_(...),
    /// chain==chain_id)` — the start/end + `allowed_intermediate_tokens`
    /// resolver used by `find_paths` before the DFS.
    ///
    /// Addresses are stored EIP-55 checksummed; the lookup matches the
    /// checksummed form, so callers must provide checksummed addresses
    /// (mirrors the Python `get_checksum_address` pre-normalization).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed address column.
    pub fn fetch_token_ids_by_address(
        &self,
        chain_id: i64,
        addresses: &[Address],
    ) -> Result<HashMap<Address, u64>, DbError> {
        if addresses.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (0..addresses.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, address FROM erc20_tokens \
             WHERE chain = ?1 AND address IN ({placeholders})"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<String> = std::iter::once(chain_id.to_string())
            .chain(addresses.iter().map(|a| a.to_checksum(None)))
            .collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let mut out = HashMap::with_capacity(addresses.len());
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let addr: String = row.get(1)?;
            let token_id = u64::try_from(id)
                .map_err(|e| DbError::Decode(format!("token id {id} out of u64 range: {e}")))?;
            out.insert(decode_address(&addr)?, token_id);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    // The §4.2 parity tests live in `tests/pathfinding_parity.rs` — they open a
    // fixture DB and compare against the Python `_prepare_graph` /
    // `_get_tokens_with_min_degree` oracle output dumped to JSON.
}
