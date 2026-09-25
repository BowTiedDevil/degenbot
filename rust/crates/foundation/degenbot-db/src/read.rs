//! Low-level read fns — typed row lookups mirroring the `SQLAlchemy`
//! `DatabaseSnapshot` reader (the parity oracle). Each takes the
//! [`DegenbotDb`] handle, locks the [`Connection`], prepares a statement, and
//! decodes rows via the [`crate::rows`] `FromRow` impls.
//!
//! See `src/degenbot/uniswap/{v3,v4}_snapshot.py` `DatabaseSnapshot` for the
//! canonical query shapes.

use alloy::primitives::Address;

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::decode_address;
use crate::rows::{
    InitializationMapRow, LfjPoolRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolInitializationMapRow, ManagedPoolLiquidityPositionRow, PoolKindRow, V2PoolRow,
    V3PoolRow, V4PoolRow,
};
use crate::schema::table::{
    is_lfj_kind, is_v2_kind, is_v3_kind, is_v4_kind, v2_v3_subclass_table, EXCHANGES, LFJ_POOLS,
    MANAGED_POOLS, POOLS, POOL_MANAGERS,
};

/// Whether to filter exchange `last_update_block` rows by V3 or V4 family name
/// (mirrors Python's `name.like('%!_v3'/'%!_v4')` — names ending in `_v3` or
/// `_v4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeFamily {
    /// Exchange names ending in `_v3` (`uniswap_v3`, `aerodrome_v3`, ...).
    V3,
    /// Exchange names ending in `_v4` (`uniswap_v4`, ...).
    V4,
}

impl ExchangeFamily {
    /// The SQL SUFFIX pattern (used with an explicit ESCAPE so the `_` is
    /// treated as a literal underscore, not a wildcard — mirrors the Python
    /// `escape="!"` `LIKE '%!_v3'`).
    fn suffix(self) -> &'static str {
        match self {
            ExchangeFamily::V3 => "%\\_v3",
            ExchangeFamily::V4 => "%\\_v4",
        }
    }
}

/// One chain-scoped pool family the backrun arm cannot type: the on-chain
/// address a frame would touch and the DB `kind` discriminator.
///
/// A unified `pools` row's address is the pool contract; a `managed_pools`
/// row's address is the pool MANAGER (`pool_managers.address`, joined via
/// `manager_id`) — that is the account a frame touches for a managed family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPoolAddress {
    pub address: Address,
    pub kind: String,
}

/// One `pools` row as the connector index sees it: identity, the raw on-chain
/// address string, and the polymorphic `kind` discriminator (deliberately
/// unclassified — the connector index routes V2 rows to edges and leaves every
/// other family to [`UnsupportedPoolAddress`]).
///
/// The address stays the DB string so a row the index will not use (a
/// non-checksummed V3 address from a fixture) never fails the scan: the
/// consumer parses only the rows it routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorPoolRow {
    pub pool_id: u64,
    pub token0_id: u64,
    pub token1_id: u64,
    pub address: String,
    pub kind: String,
}

/// Structural fingerprint of the discovery graph for one chain.
///
/// The first pair describes the unified V2/V3 `pools` table and the second
/// pair describes `managed_pools` scoped through its pool manager's chain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphEdition {
    pub v2v3_count: i64,
    pub v2v3_max_id: i64,
    pub v4_count: i64,
    pub v4_max_id: i64,
}

impl DegenbotDb {
    /// `SELECT * FROM erc20_tokens WHERE address=? AND chain=?`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_token_by_address(
        &self,
        address: Address,
        chain: i64,
    ) -> Result<Option<crate::rows::Erc20TokenRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain, address, name, symbol, decimals \
             FROM erc20_tokens WHERE address = ?1 AND chain = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![address.to_checksum(None), chain])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::Erc20TokenRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT id, chain, address, name, symbol, decimals FROM erc20_tokens WHERE id = ?`.
    ///
    /// The FK-id companion to [`Self::fetch_token_by_address`]. QVMWQC: the pool
    /// builders hydrate `pool.token0` / `pool.token1` ORM relationships by their
    /// FK id columns (`token0_id` / `token1_id`); this is the single-table read
    /// that replaces the `SQLAlchemy` lazy-load.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_token_by_id(
        &self,
        token_id: i64,
    ) -> Result<Option<crate::rows::Erc20TokenRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain, address, name, symbol, decimals \
             FROM erc20_tokens WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![token_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::Erc20TokenRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT id, chain_id, name, active, last_update_block, factory, deployer FROM exchanges WHERE id = ?`.
    ///
    /// QVMWQC: the pool builders hydrate the `pool.exchange` ORM relationship by
    /// its FK id column (`exchange_id`); this read replaces the `SQLAlchemy`
    /// lazy-load. `factory` / `deployer` are the fields the builders read
    /// (`exchange.factory`, `exchange.deployer`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_exchange(
        &self,
        exchange_id: i64,
    ) -> Result<Option<crate::rows::ExchangeRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain_id, name, active, last_update_block, factory, deployer \
             FROM exchanges WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![exchange_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::ExchangeRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT id, chain_id, name, active, last_update_block, factory, deployer FROM exchanges WHERE chain_id = ? AND name = ?`.
    ///
    /// The by-name companion to [`Self::fetch_exchange`] (the `(chain_id,
    /// name)` lookup the `cli/exchange.py` `activate`/`deactivate` commands
    /// use). The exchange CLI shells now resolve by name + then flip `active`
    /// via [`crate::discovery::DegenbotDb::set_exchange_active`]; this read is
    /// the deactivate-command resolution step.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_exchange_by_name(
        &self,
        chain_id: i64,
        name: &str,
    ) -> Result<Option<crate::rows::ExchangeRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain_id, name, active, last_update_block, factory, deployer \
             FROM exchanges WHERE chain_id = ?1 AND name = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![chain_id, name])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::ExchangeRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT id, address, chain, kind, state_view, exchange_id FROM pool_managers WHERE address = ? AND chain = ?`.
    ///
    /// QVMWQC: the V4 builder resolves its `pool_manager` row by `(address,
    /// chain)` to obtain the `id` (for the V4 pool join) + the `state_view`
    /// contract address. Replaces the `SQLAlchemy`
    /// `session.scalar(select(PoolManagerTable).where(...))` read.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_pool_manager(
        &self,
        address: Address,
        chain: i64,
    ) -> Result<Option<crate::rows::PoolManagerRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, address, chain, kind, state_view, exchange_id \
             FROM pool_managers WHERE address = ?1 AND chain = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![address.to_checksum(None), chain])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::PoolManagerRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT <V4 subclass cols> FROM uniswap_v4_pools WHERE pool_hash = ?`.
    ///
    /// QVMWQC: the V4 builder resolves its pool row by the `pool_hash`
    /// (`bytes32,` 0x-prefixed lowercase-hex unique key). Returns the V4 subclass
    /// row (`managed_pool_id` / `hooks` / currencies / fees / `tick_spacing` /
    /// liquidity-update marker) — the same shape as [`Self::fetch_pool_kind`]
    /// for `kind = "uniswap_v4"`, keyed by `pool_hash` instead of the numeric id.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_v4_pool_by_pool_hash(
        &self,
        pool_hash_hex: &str,
    ) -> Result<Option<crate::rows::V4PoolRow>, DbError> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {} FROM uniswap_v4_pools WHERE pool_hash = ?1",
            crate::rows::pool::V4PoolRow::SELECT_V4
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![pool_hash_hex])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::V4PoolRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// `SELECT * FROM pools WHERE address=? AND chain=?`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_pool_by_address(
        &self,
        address: Address,
        chain: i64,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        let conn = self.lock();
        Self::fetch_pool_by_address_on_conn(&conn, address, chain)
    }

    /// The single-transaction-bound variant of [`Self::fetch_pool_by_address`]
    /// the chunk loop's per-pool in-scope lookup (read the pool
    /// row to resolve its `exchange_id` for the in-scope filter, on the chunk's
    /// single owned connection so the read + the apply share one transaction).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn fetch_pool_by_address_on_conn(
        conn: &rusqlite::Connection,
        address: Address,
        chain: i64,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT id, address, chain, kind, token0_id, token1_id, exchange_id \
             FROM pools WHERE address = ?1 AND chain = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![address.to_checksum(None), chain])?;
        match rows.next()? {
            Some(row) => Ok(Some(LiquidityPoolRow::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Join the per-DEX subclass row for a `kind` discriminator + `pool_id`.
    ///
    /// For V2/V3 families this selects from the subclass table named by
    /// [`v2_v3_subclass_table`]; for V4 it selects from `uniswap_v4_pools`
    /// via the `managed_pool_id` polymorphic key (which equals the V4
    /// `managed_pools.id` — here we accept the same numeric id and join
    /// `uniswap_v4_pools.managed_pool_id = ?`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_pool_kind(
        &self,
        kind: &str,
        pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError> {
        let conn = self.lock();
        if is_lfj_kind(kind) {
            // A declared-but-unsupported LFJ binned pair (D8): typed so the
            // persisted identity is reachable; no tier admits it.
            let sql = format!(
                "SELECT {} FROM {} WHERE pool_id = ?1",
                LfjPoolRow::SELECT_LFJ,
                LFJ_POOLS
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params![pool_id])?;
            if let Some(row) = rows.next()? {
                return Ok(Some(PoolKindRow::Lfj(LfjPoolRow::from_row(row)?)));
            }
            Ok(None)
        } else if let Some(sub) = v2_v3_subclass_table(kind) {
            // V2/V3 subclass table joins on pool_id.
            if kind == "aerodrome_v2" {
                // the only V2 subclass with a `stable` column
                let sql = format!(
                    "SELECT {} FROM {} WHERE pool_id = ?1",
                    crate::rows::pool::SELECT_V2_AERODROME,
                    sub
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params![pool_id])?;
                if let Some(row) = rows.next()? {
                    return Ok(Some(PoolKindRow::V2(V2PoolRow::from_row_with_stable(row)?)));
                }
            } else if crate::schema::table::is_v2_kind(kind) {
                let sql = format!(
                    "SELECT {} FROM {} WHERE pool_id = ?1",
                    V2PoolRow::SELECT_V2,
                    sub
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params![pool_id])?;
                if let Some(row) = rows.next()? {
                    return Ok(Some(PoolKindRow::V2(V2PoolRow::from_row(row)?)));
                }
            } else {
                // V3 family
                let sql = format!(
                    "SELECT {} FROM {} WHERE pool_id = ?1",
                    V3PoolRow::SELECT_V3,
                    sub
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params![pool_id])?;
                if let Some(row) = rows.next()? {
                    return Ok(Some(PoolKindRow::V3(V3PoolRow::from_row(row)?)));
                }
            }
            Ok(None)
        } else if is_v4_kind(kind) {
            let sql = format!(
                "SELECT {} FROM uniswap_v4_pools WHERE managed_pool_id = ?1",
                V4PoolRow::SELECT_V4
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params![pool_id])?;
            if let Some(row) = rows.next()? {
                return Ok(Some(PoolKindRow::V4(V4PoolRow::from_row(row)?)));
            }
            Ok(None)
        } else {
            Err(DbError::Decode(format!("unrecognized pool kind: {kind:?}")))
        }
    }

    /// All `pools` rows for a chain.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_pools_for_chain(&self, chain: i64) -> Result<Vec<LiquidityPoolRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, address, chain, kind, token0_id, token1_id, exchange_id \
             FROM pools WHERE chain = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain], |r| {
            LiquidityPoolRow::from_row(r).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// Newest `exchanges.last_update_block` for the chain + family.
    ///
    /// Mirrors Python `DatabaseSnapshot.get_newest_block`: returns `None` if
    /// no rows match OR any matching row has a NULL `last_update_block`,
    /// else the maximum.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_newest_update_block(
        &self,
        chain: i64,
        family: ExchangeFamily,
    ) -> Result<Option<i64>, DbError> {
        let conn = self.lock();
        fetch_newest_update_block_on_conn(&conn, chain, family)
    }

    /// All `liquidity_positions` for a V3 pool (ordered by tick).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_liquidity_positions(
        &self,
        pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, pool_id, tick, liquidity_net, liquidity_gross \
             FROM liquidity_positions WHERE pool_id = ?1 ORDER BY tick",
        )?;
        let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
            LiquidityPositionRow::from_row(r).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// All `initialization_maps` for a V3 pool (ordered by word).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_initialization_map(
        &self,
        pool_id: i64,
    ) -> Result<Vec<InitializationMapRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, pool_id, word, bitmap \
             FROM initialization_maps WHERE pool_id = ?1 ORDER BY word",
        )?;
        let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
            InitializationMapRow::from_row(r).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// All `managed_pool_liquidity_positions` for a V4 managed pool (by tick).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_managed_liquidity_positions(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, managed_pool_id, tick, liquidity_net, liquidity_gross \
             FROM managed_pool_liquidity_positions WHERE managed_pool_id = ?1 ORDER BY tick",
        )?;
        let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
            ManagedPoolLiquidityPositionRow::from_row(r).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// All `managed_pool_initialization_maps` for a V4 managed pool (by word).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_managed_initialization_map(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolInitializationMapRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, managed_pool_id, word, bitmap \
             FROM managed_pool_initialization_maps WHERE managed_pool_id = ?1 ORDER BY word",
        )?;
        let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
            ManagedPoolInitializationMapRow::from_row(r).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// `SELECT id, chain_id, name, active, last_update_block, factory, deployer
    /// FROM exchanges WHERE chain_id = ? AND active = 1 ORDER BY id`.
    ///
    /// The pool-updater chunk loop's run-start discovery read (the Rust-owned
    /// replacement for the Python `active_exchanges` query in
    /// `src/degenbot/cli/pool.py`). Returns every active exchange row for a
    /// chain, ordered by `id` for deterministic dispatch order. The
    /// chunk-loop crate (`degenbot-pool-updater`) joins these rows with the
    /// static event-config map (family, event-topic, fee denominator) to build
    /// typed `ExchangeSpec`s via its `load_active_exchange_specs` — the join
    /// lives there, not here, because the family/event-topic resolution is a
    /// chunk-loop concern (the DB crate owns the row; the chunk-loop crate
    /// owns the resolved spec).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed column.
    pub fn fetch_active_exchanges_by_chain(
        &self,
        chain_id: i64,
    ) -> Result<Vec<crate::rows::ExchangeRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain_id, name, active, last_update_block, factory, deployer \
             FROM exchanges WHERE chain_id = ?1 AND active = 1 ORDER BY id",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |row| {
            crate::rows::ExchangeRow::from_row(row).map_err(rusqlite::Error::from)
        })?;
        Ok(rows.collect::<Result<Vec<_>, rusqlite::Error>>()?)
    }

    /// Return the structural discovery-graph fingerprint for `chain_id`.
    ///
    /// This is the Rust-owned equivalent of the runner's two count/max
    /// probes. The unified `pools` table is not filtered by `kind`, and the
    /// managed-pool table is scoped through `pool_managers.chain`, preserving
    /// the Python query's behavior for future or malformed family strings.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] when either aggregate query fails.
    pub fn fetch_graph_edition(&self, chain_id: i64) -> Result<GraphEdition, DbError> {
        let conn = self.lock();
        let (v2v3_count, v2v3_max_id) = conn.query_row(
            &format!("SELECT count(*), COALESCE(max(id), 0) FROM {POOLS} WHERE chain = ?1"),
            rusqlite::params![chain_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (v4_count, v4_max_id) = conn.query_row(
            &format!(
                "SELECT count(*), COALESCE(max({MANAGED_POOLS}.id), 0) \
                 FROM {MANAGED_POOLS} \
                 JOIN {POOL_MANAGERS} ON {POOL_MANAGERS}.id = {MANAGED_POOLS}.manager_id \
                 WHERE {POOL_MANAGERS}.chain = ?1"
            ),
            rusqlite::params![chain_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        Ok(GraphEdition {
            v2v3_count,
            v2v3_max_id,
            v4_count,
            v4_max_id,
        })
    }

    /// Every `pools` row on `chain_id` with its polymorphic `kind`, for the
    /// connector index's V2 edge load.
    ///
    /// Unlike [`Self::fetch_path_graph_edges`], an unrecognized `kind` is
    /// returned rather than refused: the connector index routes V2 rows to
    /// edges and leaves every other family to
    /// [`Self::fetch_unsupported_pool_addresses`], so a DB row for an
    /// unsupported family cannot disable the whole lane at boot.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed address column.
    pub fn fetch_connector_pool_rows(
        &self,
        chain_id: i64,
    ) -> Result<Vec<ConnectorPoolRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, token0_id, token1_id, address, kind FROM {POOLS} WHERE chain = ?1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![chain_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let token0_id: i64 = row.get(1)?;
            let token1_id: i64 = row.get(2)?;
            let address: String = row.get(3)?;
            let kind: String = row.get(4)?;
            let (Ok(pool_id), Ok(token0_id), Ok(token1_id)) = (
                u64::try_from(id),
                u64::try_from(token0_id),
                u64::try_from(token1_id),
            ) else {
                continue;
            };
            out.push(ConnectorPoolRow {
                pool_id,
                token0_id,
                token1_id,
                address,
                kind,
            });
        }
        Ok(out)
    }

    /// Every chain-scoped pool row whose family the backrun descriptor seam
    /// cannot type, as `(touched address, kind)`.
    ///
    /// Two scans: the unified `pools` table (address = the pool contract) and
    /// the `managed_pools` polymorphic base joined to `pool_managers`
    /// (address = the pool MANAGER, the account a frame touches for a managed
    /// family). A row whose `kind` is V2/V3-family or `uniswap_v4` is a
    /// supported family and does not appear.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed address column.
    pub fn fetch_unsupported_pool_addresses(
        &self,
        chain_id: i64,
    ) -> Result<Vec<UnsupportedPoolAddress>, DbError> {
        let conn = self.lock();
        let mut out = Vec::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT address, kind FROM {POOLS} WHERE chain = ?1"
            ))?;
            let mut rows = stmt.query(rusqlite::params![chain_id])?;
            while let Some(row) = rows.next()? {
                let address: String = row.get(0)?;
                let kind: String = row.get(1)?;
                if is_v2_kind(&kind) || is_v3_kind(&kind) {
                    continue;
                }
                out.push(UnsupportedPoolAddress {
                    address: decode_address(&address)?,
                    kind,
                });
            }
        }
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT m.address, mp.kind FROM {MANAGED_POOLS} mp \
                 JOIN {POOL_MANAGERS} m ON m.id = mp.manager_id WHERE m.chain = ?1"
            ))?;
            let mut rows = stmt.query(rusqlite::params![chain_id])?;
            while let Some(row) = rows.next()? {
                let address: String = row.get(0)?;
                let kind: String = row.get(1)?;
                if is_v4_kind(&kind) {
                    continue;
                }
                out.push(UnsupportedPoolAddress {
                    address: decode_address(&address)?,
                    kind,
                });
            }
        }
        Ok(out)
    }
}

/// `fetch_newest_update_block` body taking a borrowed `&Connection` (works on
/// either a freshly-locked `DegenbotDb` connection or a `SnapshotDb`'s
/// held-tx connection — see `crate::snapshot::TickMapDb`).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query failure.
pub fn fetch_newest_update_block_on_conn(
    conn: &rusqlite::Connection,
    chain: i64,
    family: ExchangeFamily,
) -> Result<Option<i64>, DbError> {
    // NULLIF trick: if any matching row has NULL last_update_block, the
    // `MIN(last_update_block)` over the set is NULL, so we return None
    // (matching Python's `if None in last_update_blocks: return None`).
    let mut stmt = conn.prepare(&format!(
        "SELECT MAX(last_update_block), MIN(last_update_block) \
         FROM {EXCHANGES} \
         WHERE chain_id = ?1 AND name LIKE ?2 ESCAPE '\\'"
    ))?;
    let (mx, mn): (Option<i64>, Option<i64>) =
        stmt.query_row(rusqlite::params![chain, family.suffix()], |row| {
            Ok::<_, rusqlite::Error>((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?))
        })?;
    match (mx, mn) {
        // Python returns None if the set is empty OR any row has NULL
        // last_update_block (an empty set → MAX/MIN are both NULL).
        (_, None) => Ok(None),
        (mx, _) => Ok(mx),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::migrate::SchemaState;
    use alloy::primitives::address;

    /// A fresh in-memory **write-capable** DB (mirrors the `discovery` test
    /// helper — `set_exchange_active`/`upsert_exchange` need the writable
    /// handle).
    fn write_db() -> DegenbotDb {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        db
    }

    #[test]
    fn fetch_active_exchanges_by_chain_returns_only_active_rows_for_chain() {
        let db = write_db();
        let v2_factory = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
        let v3_factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");

        let v2 = db
            .upsert_exchange(1, "uniswap_v2", v2_factory, None)
            .unwrap();
        let v3 = db
            .upsert_exchange(1, "uniswap_v3", v3_factory, None)
            .unwrap();
        // A second-chain exchange — must NOT appear in the chain=1 result.
        let _other = db
            .upsert_exchange(8453, "uniswap_v3", v3_factory, None)
            .unwrap();

        // Initially both chain-1 exchanges are inactive → empty.
        let empty = db.fetch_active_exchanges_by_chain(1).unwrap();
        assert!(empty.is_empty(), "no exchanges are active yet");

        // Activate only the V3 one.
        db.set_exchange_active(v3.id, true).unwrap();

        let only_v3 = db.fetch_active_exchanges_by_chain(1).unwrap();
        assert_eq!(only_v3.len(), 1);
        assert_eq!(only_v3[0].id, v3.id);
        assert_eq!(only_v3[0].name, "uniswap_v3");
        assert_eq!(only_v3[0].factory, v3_factory);
        assert!(only_v3[0].active);

        // Activate the V2 one too — result ordered by id (V2 inserted first).
        db.set_exchange_active(v2.id, true).unwrap();
        let both = db.fetch_active_exchanges_by_chain(1).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].id, v2.id, "ordered by id — v2 first");
        assert_eq!(both[1].id, v3.id);
    }

    #[test]
    fn fetch_graph_edition_is_empty_and_chain_scoped() {
        let db = write_db();
        assert_eq!(db.fetch_graph_edition(1).unwrap(), GraphEdition::default());

        let pool_1 = Address::new([0x11; 20]).to_checksum(None);
        let pool_2 = Address::new([0x12; 20]).to_checksum(None);
        let manager = Address::new([0x21; 20]).to_checksum(None);
        let other_pool = Address::new([0x13; 20]).to_checksum(None);
        let other_manager = Address::new([0x22; 20]).to_checksum(None);
        {
            let conn = db.lock();
            conn.execute_batch(&format!(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES
                   (1, 1, '{pool_1}'), (2, 1, '{pool_2}');
                 INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
                   (1, 1, 'chain-one', 1, '{manager}'),
                   (2, 10, 'chain-ten', 1, '{other_manager}');
                 INSERT INTO pools
                   (id, address, chain, kind, token0_id, token1_id, exchange_id) VALUES
                   (2, '{pool_1}', 1, 'uniswap_v2', 1, 2, 1),
                   (7, '{pool_2}', 1, 'uniswap_v3', 1, 2, 1),
                   (100, '{other_pool}', 10, 'uniswap_v2', 1, 2, 2);
                 INSERT INTO pool_managers
                   (id, address, chain, kind, exchange_id) VALUES
                   (20, '{manager}', 1, 'uniswap_v4', 1),
                   (21, '{other_manager}', 10, 'uniswap_v4', 2);
                 INSERT INTO managed_pools (id, kind, manager_id) VALUES
                   (4, 'uniswap_v4', 20), (9, 'uniswap_v4', 20),
                   (100, 'uniswap_v4', 21);"
            ))
            .unwrap();
        }

        assert_eq!(
            db.fetch_graph_edition(1).unwrap(),
            GraphEdition {
                v2v3_count: 2,
                v2v3_max_id: 7,
                v4_count: 2,
                v4_max_id: 9,
            }
        );
        assert_eq!(
            db.fetch_graph_edition(10).unwrap(),
            GraphEdition {
                v2v3_count: 1,
                v2v3_max_id: 100,
                v4_count: 1,
                v4_max_id: 100,
            }
        );
        assert_eq!(
            db.fetch_graph_edition(999).unwrap(),
            GraphEdition::default()
        );
    }

    #[test]
    fn fetch_graph_edition_counts_foreign_kinds_like_python_probe() {
        let db = write_db();
        let token_1 = Address::new([0x11; 20]).to_checksum(None);
        let token_2 = Address::new([0x13; 20]).to_checksum(None);
        let pool = Address::new([0x12; 20]).to_checksum(None);
        let manager = Address::new([0x21; 20]).to_checksum(None);
        {
            let conn = db.lock();
            conn.execute_batch(&format!(
                "INSERT INTO erc20_tokens (id, chain, address)
                   VALUES (1, 1, '{token_1}'), (2, 1, '{token_2}');
                 INSERT INTO exchanges (id, chain_id, name, active, factory)
                   VALUES (1, 1, 'future', 1, '{manager}');
                 INSERT INTO pool_managers (id, address, chain, kind, exchange_id)
                   VALUES (1, '{manager}', 1, 'future_managed', 1);
                 INSERT INTO pools
                   (id, address, chain, kind, token0_id, token1_id, exchange_id)
                   VALUES (8, '{pool}', 1, 'future_unified', 1, 2, 1);
                 INSERT INTO managed_pools (id, kind, manager_id)
                   VALUES (9, 'future_managed', 1);"
            ))
            .unwrap();
        }

        assert_eq!(
            db.fetch_graph_edition(1).unwrap(),
            GraphEdition {
                v2v3_count: 1,
                v2v3_max_id: 8,
                v4_count: 1,
                v4_max_id: 9,
            }
        );
    }

    #[test]
    fn fetch_unsupported_pool_addresses_keeps_only_unsupported_families() {
        let db = write_db();
        let pool = Address::new([0xc1; 20]);
        let supported = Address::new([0xc2; 20]);
        let manager = Address::new([0xc3; 20]);
        let v4_manager = Address::new([0xc4; 20]);
        let t0 = Address::new([0x11; 20]).to_checksum(None);
        let t1 = Address::new([0x22; 20]).to_checksum(None);
        {
            let conn = db.lock();
            conn.execute_batch(&format!(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) VALUES
                   (1, 1, '{t0}', 'T0', 'T0', 18), (2, 1, '{t1}', 'T1', 'T1', 18);
                 INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
                   (1, 1, 'test', 1, '0x0000000000000000000000000000000000000001');
                 INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) VALUES
                   (901, '{pool}', 1, 'lfj_binned', 1, 2, 1),
                   (902, '{supported}', 1, 'uniswap_v2', 1, 2, 1);
                 INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) VALUES
                   (901, '{manager}', 1, 'lfj_binned', NULL, 1),
                   (902, '{v4_manager}', 1, 'uniswap_v4', NULL, 1);
                 INSERT INTO managed_pools (id, kind, manager_id) VALUES
                   (901, 'lfj_binned', 901), (902, 'uniswap_v4', 902);",
                pool = pool.to_checksum(None),
                supported = supported.to_checksum(None),
                manager = manager.to_checksum(None),
                v4_manager = v4_manager.to_checksum(None),
            ))
            .unwrap();
        }
        let rows = db.fetch_unsupported_pool_addresses(1).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|r| r.address == pool && r.kind == "lfj_binned"),
            "a `pools`-table unsupported kind is keyed by the pool address"
        );
        assert!(
            rows.iter()
                .any(|r| r.address == manager && r.kind == "lfj_binned"),
            "a managed unsupported family is keyed by its MANAGER address"
        );
        assert!(
            !rows.iter().any(|r| r.address == supported),
            "a supported V2 pool is excluded"
        );
        assert!(
            !rows.iter().any(|r| r.address == v4_manager),
            "uniswap_v4 is excluded"
        );
        assert!(
            db.fetch_unsupported_pool_addresses(8453)
                .unwrap()
                .is_empty(),
            "the scan is chain-scoped"
        );
    }
}
