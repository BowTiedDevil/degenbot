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
use crate::rows::{
    InitializationMapRow, LiquidityPoolRow, LiquidityPositionRow, ManagedPoolInitializationMapRow,
    ManagedPoolLiquidityPositionRow, PoolKindRow, V2PoolRow, V3PoolRow, V4PoolRow,
};
use crate::schema::table::{is_v4_kind, v2_v3_subclass_table, EXCHANGES};

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
    pub fn fetch_token_by_id(&self, token_id: i64) -> Result<Option<crate::rows::Erc20TokenRow>, DbError> {
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
    pub fn fetch_exchange(&self, exchange_id: i64) -> Result<Option<crate::rows::ExchangeRow>, DbError> {
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
        if let Some(sub) = v2_v3_subclass_table(kind) {
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
                Ok::<_, rusqlite::Error>((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            })?;
        match (mx, mn) {
            // Python returns None if the set is empty OR any row has NULL
            // last_update_block (an empty set → MAX/MIN are both NULL).
            (_, None) => Ok(None),
            (mx, _) => Ok(mx),
        }
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
}
