//! Snapshot-shaped read fns — [`LiquidityMap`] / [`BitmapAtWord`] /
//! [`LiquidityAtTick`] + [`fetch_liquidity_map`] / [`fetch_liquidity_map_v4`] /
//! [`fetch_all_liquidity_maps`]. These mirror Python `DatabaseSnapshot`
//! (`src/degenbot/uniswap/{v3,v4}_snapshot.py`) so the cross-implementation
//! parity fixture asserts byte-identical results, including the
//! `VARCHAR(78)` ↔ [`U256`] boundary.
//!
//! The domain types live HERE (in `degenbot-db`, a leaf) rather than borrowing
//! `degenbot-bot`'s `TickInfo` — that would pull a `degenbot-db → degenbot-bot`
//! dependency edge (BotState/state-machine logic into a persistence leaf). The
//! sibling routing task (slice 14c) converts `LiquidityMap → V3PoolState`'s
//! `tick_data` at the `PyBot`-registration seam (a 1:1 field copy).

use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::read::ExchangeFamily;
use crate::rows::decode::{decode_address, decode_u256};
use crate::schema::table::is_v3_kind;

/// Per-tick (`liquidity_gross`, `liquidity_net`) pair (the value type of a batch
/// read entry).
type TickMap = HashMap<i32, (U256, U256)>;

/// The V3 batch result: pool address → per-tick map.
type V3Batch = HashMap<Address, TickMap>;

/// The V4 batch key (pool-manager address, `pool_hash` hex string) for the result map.
type V4Key = (String, String);

/// The tick-initialization bitmap entry at one word (mirrors Python
/// `BitmapAtWord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapAtWord {
    pub bitmap: U256,
}

/// The liquidity state at one tick (mirrors Python `LiquidityAtTick`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityAtTick {
    pub liquidity_gross: U256,
    pub liquidity_net: U256,
}

/// The liquidity map for one pool — `tick_bitmap` + `tick_data` (mirrors
/// Python `LiquidityMap` `TypedDict`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiquidityMap {
    pub tick_bitmap: HashMap<i64, BitmapAtWord>,
    pub tick_data: HashMap<i32, LiquidityAtTick>,
}

/// The key a per-pool batch-read entry is grouped under. V3 pools are keyed by
/// address alone; V4 pools are keyed by (pool-manager address, pool-hash hex).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolKey {
    /// A V3 pool: keyed by its pool contract address.
    V3(Address),
    /// A V4 pool: keyed by its pool-manager address + the 0x-prefixed lowercase
    /// `pool_hash` hex string (exactly as Python's `get_all_liquidity_maps` V4
    /// keys its result dict).
    V4 {
        pool_manager: Address,
        pool_hash: String,
    },
}

impl DegenbotDb {
    /// The V3 [`LiquidityMap`] for `pool_address` (mirrors Python
    /// `DatabaseSnapshot.get_liquidity_map`).
    ///
    /// Selects the pool by address only ( matching the oracle, which does NOT
    /// filter by chain in `get_liquidity_map`); the pool address is taken to
    /// identify the pool within the snapshot's chain. Returns `None` if no
    /// such pool exists or it is not a V3-family kind.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_liquidity_map(
        &self,
        pool_address: Address,
    ) -> Result<Option<LiquidityMap>, DbError> {
        let conn = self.lock();
        // Mirror Python: `select(LiquidityPoolTable).where(address == pool_address)`.
        // LIMIT 1 since (address, chain) is unique but address alone is not
        // cross-chain; the oracle returns the first match.
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, kind FROM pools WHERE address = ?1 LIMIT 1",
                rusqlite::params![pool_address.to_checksum(None)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let Some((pool_id, kind)) = row else {
            return Ok(None);
        };
        if !is_v3_kind(&kind) {
            return Ok(None);
        }

        let mut tick_bitmap: HashMap<i64, BitmapAtWord> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT word, bitmap FROM initialization_maps WHERE pool_id = ?1")?;
            let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (word, bitmap_str) = r?;
                tick_bitmap.insert(
                    word,
                    BitmapAtWord {
                        bitmap: decode_u256(&bitmap_str)?,
                    },
                );
            }
        }

        let mut tick_data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT tick, liquidity_gross, liquidity_net \
                 FROM liquidity_positions WHERE pool_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![pool_id], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (tick, gross, net) = r?;
                tick_data.insert(
                    tick,
                    LiquidityAtTick {
                        liquidity_gross: decode_u256(&gross)?,
                        liquidity_net: decode_u256(&net)?,
                    },
                );
            }
        }

        Ok(Some(LiquidityMap {
            tick_bitmap,
            tick_data,
        }))
    }

    /// The V4 [`LiquidityMap`] for a (pool manager, pool-hash) pair (mirrors
    /// Python `DatabaseSnapshot.get_liquidity_map`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_liquidity_map_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<LiquidityMap>, DbError> {
        let conn = self.lock();
        // Mirror Python: select the UniswapV4PoolTable joined to its manager,
        // matching on pool_hash hex + manager address.
        let hash_hex = format!("{pool_id_hash}"); // B256 Display includes the 0x prefix
        let row: Option<i64> = conn
            .query_row(
                "SELECT v4.managed_pool_id \
                 FROM uniswap_v4_pools v4 \
                 JOIN managed_pools mp ON mp.id = v4.managed_pool_id \
                 JOIN pool_managers pm ON pm.id = mp.manager_id \
                 WHERE v4.pool_hash = ?1 AND pm.address = ?2",
                rusqlite::params![hash_hex, pool_manager.to_checksum(None)],
                |r| r.get::<_, i64>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let Some(managed_pool_id) = row else {
            return Ok(None);
        };

        let mut tick_bitmap: HashMap<i64, BitmapAtWord> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT word, bitmap FROM managed_pool_initialization_maps \
                 WHERE managed_pool_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (word, bitmap_str) = r?;
                tick_bitmap.insert(
                    word,
                    BitmapAtWord {
                        bitmap: decode_u256(&bitmap_str)?,
                    },
                );
            }
        }

        let mut tick_data: HashMap<i32, LiquidityAtTick> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT tick, liquidity_gross, liquidity_net \
                 FROM managed_pool_liquidity_positions WHERE managed_pool_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![managed_pool_id], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (tick, gross, net) = r?;
                tick_data.insert(
                    tick,
                    LiquidityAtTick {
                        liquidity_gross: decode_u256(&gross)?,
                        liquidity_net: decode_u256(&net)?,
                    },
                );
            }
        }

        Ok(Some(LiquidityMap {
            tick_bitmap,
            tick_data,
        }))
    }

    /// All V3 or V4 tick data for a chain as a batch (mirrors Python
    /// `DatabaseSnapshot.get_all_liquidity_maps`). V3 keyed by address; V4
    /// keyed by (pool-manager address, `pool_hash` hex). Returns one entry per
    /// pool that has at least one liquidity position.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    pub fn fetch_all_liquidity_maps(
        &self,
        chain_id: i64,
        family: ExchangeFamily,
    ) -> Result<Vec<(PoolKey, TickMap)>, DbError> {
        let conn = self.lock();
        match family {
            ExchangeFamily::V3 => {
                let sql = "\
                    SELECT p.address, lp.tick, lp.liquidity_gross, lp.liquidity_net \
                    FROM pools p \
                    JOIN liquidity_positions lp ON lp.pool_id = p.id \
                    WHERE p.chain = ?1 \
                      AND p.kind IN ('uniswap_v3', 'sushiswap_v3', 'pancakeswap_v3', 'aerodrome_v3') \
                    ORDER BY p.address, lp.tick";
                let mut out: V3Batch = HashMap::new();
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(rusqlite::params![chain_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i32>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?;
                for r in rows {
                    let (addr_s, tick, gross, net) = r?;
                    let addr = decode_address(&addr_s)?;
                    out.entry(addr)
                        .or_default()
                        .insert(tick, (decode_u256(&gross)?, decode_u256(&net)?));
                }
                // emit in address order to match Python's ORDER BY p.address
                let mut keys: Vec<Address> = out.keys().copied().collect();
                keys.sort_by_key(|a| a.to_checksum(None));
                Ok(keys
                    .into_iter()
                    .map(|k| (PoolKey::V3(k), out.remove(&k).unwrap_or_default()))
                    .collect())
            }
            ExchangeFamily::V4 => {
                let sql = "\
                    SELECT pm.address, v4.pool_hash, lp.tick, lp.liquidity_gross, lp.liquidity_net \
                    FROM pool_managers pm \
                    JOIN managed_pools mp ON mp.manager_id = pm.id \
                    JOIN uniswap_v4_pools v4 ON v4.managed_pool_id = mp.id \
                    JOIN managed_pool_liquidity_positions lp ON lp.managed_pool_id = mp.id \
                    WHERE pm.chain = ?1 AND mp.kind = 'uniswap_v4' \
                    ORDER BY pm.address, v4.pool_hash, lp.tick";
                // key by (pm_address_string, pool_hash_string) so eq matches Python
                let mut out: HashMap<V4Key, TickMap> = HashMap::new();
                let mut key_order: Vec<(String, String)> = Vec::new();
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(rusqlite::params![chain_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i32>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?;
                for r in rows {
                    let (pm_s, hash_s, tick, gross, net) = r?;
                    let key = (pm_s, hash_s);
                    if !out.contains_key(&key) {
                        key_order.push(key.clone());
                    }
                    out.entry(key)
                        .or_default()
                        .insert(tick, (decode_u256(&gross)?, decode_u256(&net)?));
                }
                let mut result: Vec<(PoolKey, TickMap)> = Vec::new();
                for (pm_s, hash_s) in key_order {
                    let pm = decode_address(&pm_s)?;
                    let ticks = out
                        .remove(&(pm_s.clone(), hash_s.clone()))
                        .unwrap_or_default();
                    result.push((
                        PoolKey::V4 {
                            pool_manager: pm,
                            pool_hash: hash_s,
                        },
                        ticks,
                    ));
                }
                Ok(result)
            }
        }
    }

    /// All V3-family pool addresses for a chain (mirrors Python
    /// `DatabaseSnapshot.get_pools` V3 — `select(UniswapV3PoolTableBase.address)`).
    ///
    /// The Python oracle does NOT filter by chain; this Rust port adds the
    /// chain filter (the snapshot is per-chain — chain-scoping is the
    /// standalone-correct shape + tightens the Python oversight). On the
    /// single-chain parity fixture the result is identical.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed address column.
    pub fn fetch_v3_pool_addresses(&self, chain_id: i64) -> Result<Vec<Address>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT address FROM pools WHERE chain = ?1 AND kind IN \
             ('uniswap_v3', 'sushiswap_v3', 'pancakeswap_v3', 'aerodrome_v3')",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for addr_s in rows {
            out.push(decode_address(&addr_s?)?);
        }
        Ok(out)
    }

    /// Same as [`Self::fetch_v3_pool_addresses`] but runs on a caller-provided
    /// `Connection` — used by the pool-updater's pre-commit full verification,
    /// which must read uncommitted writes inside the chunk's `Transaction`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_v3_pool_addresses_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
    ) -> Result<Vec<alloy::primitives::Address>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT address FROM pools WHERE chain = ?1 AND kind IN \
             ('uniswap_v3', 'sushiswap_v3', 'pancakeswap_v3', 'aerodrome_v3')",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for addr_s in rows {
            out.push(decode_address(&addr_s?)?);
        }
        Ok(out)
    }

    /// All V4 `pool_hash` hex strings for a chain (mirrors Python
    /// `DatabaseSnapshot.get_pools` V4 — `select(UniswapV4PoolTable.pool_hash)`).
    ///
    /// As with [`Self::fetch_v3_pool_addresses`], adds the chain filter the
    /// Python oracle omits (per-chain snapshot + standalone-correct).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_v4_pool_hashes(&self, chain_id: i64) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT v4.pool_hash FROM uniswap_v4_pools v4 \
             JOIN managed_pools mp ON mp.id = v4.managed_pool_id \
             JOIN pool_managers pm ON pm.id = mp.manager_id \
             WHERE pm.chain = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for h in rows {
            out.push(h?);
        }
        Ok(out)
    }

    /// Same as [`Self::fetch_v4_pool_hashes`] but runs on a caller-provided
    /// `Connection` — used by the pool-updater's pre-commit full verification.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_v4_pool_hashes_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT v4.pool_hash FROM uniswap_v4_pools v4 \
             JOIN managed_pools mp ON mp.id = v4.managed_pool_id \
             JOIN pool_managers pm ON pm.id = mp.manager_id \
             WHERE pm.chain = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for h in rows {
            out.push(h?);
        }
        Ok(out)
    }
}
