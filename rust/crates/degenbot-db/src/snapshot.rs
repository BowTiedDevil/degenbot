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
use rusqlite::Connection;

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::read::ExchangeFamily;
use crate::rows::decode::{decode_address, decode_i256, decode_u256};
use crate::schema::table::is_v3_kind;

/// Per-tick (`liquidity_gross`, `liquidity_net`) pair (the value type of a batch
/// read entry). `liquidity_net` is `I256` — the DB stores it as a signed
/// decimal string (`VARCHAR(78)` with a leading `-` for upper ticks).
pub type TickMap = HashMap<i32, (U256, alloy::primitives::I256)>;

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
    pub liquidity_net: alloy::primitives::I256,
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
        fetch_liquidity_map_on_conn(&conn, pool_address)
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
        fetch_liquidity_map_v4_on_conn(&conn, pool_manager, pool_id_hash)
    }

    /// The `uniswap_v3_pools.liquidity_update_block` for a pool (the block its
    /// DB liquidity map is EXACT at, i.e. the authoritative liquidity clock of
    /// a DB-seeded `Tracked` pool). Task 4TWM7C: the Rust `PoolBuilder` uses
    /// this to stamp a DB-seeded pool's `tick_data_block` at the DB seed block
    /// rather than the live head, so the seed/post-drain verify anchors at the
    /// block the tick data actually reflects — not the aggregate `S`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_liquidity_update_block(
        &self,
        pool_address: Address,
    ) -> Result<Option<i64>, DbError> {
        let conn = self.lock();
        fetch_liquidity_update_block_on_conn(&conn, pool_address)
    }

    /// V4 twin of [`Self::fetch_liquidity_update_block`].
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_liquidity_update_block_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<i64>, DbError> {
        let conn = self.lock();
        fetch_liquidity_update_block_v4_on_conn(&conn, pool_manager, pool_id_hash)
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
        let mut out: Vec<(PoolKey, TickMap)> = Vec::new();
        self.stream_liquidity_maps(chain_id, family, |key, ticks| {
            out.push((key, ticks.clone()));
        })?;
        Ok(out)
    }

    /// Streaming variant of [`Self::fetch_all_liquidity_maps`]: invokes
    /// `on_pool(key, ticks)` once per pool as rows are consumed, instead of
    /// materializing one `Vec<(PoolKey, TickMap)>`. A single pass under one
    /// `MutexGuard`; pools arrive in the SAME order as the materialized
    /// version (V3: address order; V4: `(pm_address, pool_hash)` order). A
    /// chain with no pools invokes the callback zero times.
    ///
    /// Used by the core `Bot::load_snapshot_from_db` to feed a
    /// `SnapshotStore::insert` one pool at a time, mirroring Python's
    /// `yield_per` streaming shape and avoiding transient ~2× peak RSS
    /// (the materialized `Vec` + the engine `HashMap` coexisting).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`]
    /// on a malformed column.
    pub fn stream_liquidity_maps(
        &self,
        chain_id: i64,
        family: ExchangeFamily,
        mut on_pool: impl FnMut(PoolKey, &TickMap),
    ) -> Result<(), DbError> {
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
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(rusqlite::params![chain_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i32>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?;
                let mut current_addr: Option<Address> = None;
                let mut current_ticks: TickMap = HashMap::new();
                for r in rows {
                    let (addr_s, tick, gross, net) = r?;
                    let addr = decode_address(&addr_s)?;
                    if current_addr.is_some() && current_addr != Some(addr) {
                        if let Some(addr) = current_addr.take() {
                            on_pool(PoolKey::V3(addr), &current_ticks);
                            current_ticks.clear();
                        }
                    }
                    current_addr = Some(addr);
                    current_ticks.insert(tick, (decode_u256(&gross)?, decode_i256(&net)?));
                }
                if let Some(addr) = current_addr {
                    on_pool(PoolKey::V3(addr), &current_ticks);
                }
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
                let mut current: Option<(String, String)> = None;
                let mut current_ticks: TickMap = HashMap::new();
                for r in rows {
                    let (pm_s, hash_s, tick, gross, net) = r?;
                    let key = (pm_s, hash_s);
                    if current.is_some() && current.as_ref() != Some(&key) {
                        if let Some((pm_s, hash_s)) = current.take() {
                            let pm = decode_address(&pm_s)?;
                            on_pool(
                                PoolKey::V4 {
                                    pool_manager: pm,
                                    pool_hash: hash_s,
                                },
                                &current_ticks,
                            );
                            current_ticks.clear();
                        }
                    }
                    current = Some(key);
                    current_ticks.insert(tick, (decode_u256(&gross)?, decode_i256(&net)?));
                }
                if let Some((pm_s, hash_s)) = current {
                    let pm = decode_address(&pm_s)?;
                    on_pool(
                        PoolKey::V4 {
                            pool_manager: pm,
                            pool_hash: hash_s,
                        },
                        &current_ticks,
                    );
                }
            }
        }
        Ok(())
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
    pub fn fetch_v4_pool_hashes(&self, chain_id: i64) -> Result<Vec<(String, Address)>, DbError> {
        let conn = self.lock();
        Self::fetch_v4_pool_hashes_on_conn(&conn, chain_id)
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
    ) -> Result<Vec<(String, Address)>, DbError> {
        // SELECT the PoolManager `address` alongside the pool_hash — the V4
        // full-verify needs it to call `extsload` on the right singleton, and
        // it is cleanly retrievable via the `managed_pools.manager_id` →
        // `pool_managers.address` FK already joined here. Resolving from the DB
        // (not a chunk-local in-memory map) means pools created in any earlier
        // chunk — which have a `pool_hash` row but no liquidity event in the
        // current chunk — are covered. This was the root cause of the
        // "v4 full-verify: no PoolManager address for pool_hash" crash.
        let mut stmt = conn.prepare(
            "SELECT v4.pool_hash, pm.address FROM uniswap_v4_pools v4 \
             JOIN managed_pools mp ON mp.id = v4.managed_pool_id \
             JOIN pool_managers pm ON pm.id = mp.manager_id \
             WHERE pm.chain = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![chain_id], |r| {
            let hash: String = r.get(0)?;
            let addr_str: String = r.get(1)?;
            let addr = decode_address(&addr_str)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            Ok((hash, addr))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

// === _on_conn factored read bodies + TickMapDb trait =========================
//
// Extracted so a held-tx connection (see `SnapshotDb`) reuses the SAME SQL as
// the per-call `DegenbotDb` reads — the tick-map assembly Db arm works against
// either handle. Epic `XEANMB`: the held read transaction (`SnapshotDb`)
// freezes the DB view across `build_paths`, replacing `SnapshotStore`.

/// `fetch_liquidity_map` body taking a borrowed `&Connection` (works on either
/// a freshly-locked `DegenbotDb` connection or a `SnapshotDb`'s held-tx
/// connection).
///
/// # Errors
/// [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
pub fn fetch_liquidity_map_on_conn(
    conn: &Connection,
    pool_address: Address,
) -> Result<Option<LiquidityMap>, DbError> {
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
                    liquidity_net: decode_i256(&net)?,
                },
            );
        }
    }

    Ok(Some(LiquidityMap {
        tick_bitmap,
        tick_data,
    }))
}

/// `fetch_liquidity_map_v4` body taking a borrowed `&Connection`.
///
/// # Errors
/// [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
pub fn fetch_liquidity_map_v4_on_conn(
    conn: &Connection,
    pool_manager: Address,
    pool_id_hash: B256,
) -> Result<Option<LiquidityMap>, DbError> {
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
                    liquidity_net: decode_i256(&net)?,
                },
            );
        }
    }

    Ok(Some(LiquidityMap {
        tick_bitmap,
        tick_data,
    }))
}

/// The `uniswap_v3_pools.liquidity_update_block` for a pool, by address — the
/// block the DB liquidity map is exact at (the authoritative liquidity clock of
/// a DB-seeded `Tracked` pool; task 4TWM7C). Resolves the pool id by address
/// then reads the V3 row's `liquidity_update_block`.
///
/// Returns `None` if no such pool exists or it is not registered as a
/// V3-family kind (mirrors [`fetch_liquidity_map_on_conn`]'s miss semantics).
///
/// # Errors
/// [`DbError::Sqlite`] on a query failure.
/// Map a V3-family `kind` to its per-dex liquidity table. All four share the
/// same `(pool_id, tick_spacing, liquidity_update_block, ...)` schema; the
/// liquidity clock of a `Tracked` pool lives in its OWN table, not a single
/// one (task 4TWM7C follow-up — pool 0x1ac1 was a `pancakeswap_v3` and a
/// uniswap-only lookup returned `None`, so the seed verify fell back to head
/// and re-tripped the wrong-block bug). Returns `None` for non-V3 kinds.
fn v3_kind_liquidity_table(kind: &str) -> Option<&'static str> {
    match kind {
        "uniswap_v3" => Some("uniswap_v3_pools"),
        "pancakeswap_v3" => Some("pancakeswap_v3_pools"),
        "sushiswap_v3" => Some("sushiswap_v3_pools"),
        "aerodrome_v3" => Some("aerodrome_v3_pools"),
        _ => None,
    }
}

/// The V3 pool's `liquidity_update_block`, by address — the block its DB
/// liquidity map is exact at (the authoritative liquidity clock of a
/// DB-seeded `Tracked` pool; task 4TWM7C). Resolves the pool id + kind from
/// `pools`, then reads the block from the pool's OWN per-dex V3 table
/// (`uniswap_v3_pools` / `pancakeswap_v3_pools` / `sushiswap_v3_pools` /
/// `aerodrome_v3_pools`).
///
/// Returns `None` if no such pool exists or it is not a V3-family kind
/// (mirrors [`fetch_liquidity_map_on_conn`]'s miss semantics).
///
/// # Errors
/// [`DbError::Sqlite`] on a query failure.
pub fn fetch_liquidity_update_block_on_conn(
    conn: &Connection,
    pool_address: Address,
) -> Result<Option<i64>, DbError> {
    let room: Option<(i64, String)> = conn
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
    let Some((pool_id, kind)) = room else {
        return Ok(None);
    };
    let Some(table) = v3_kind_liquidity_table(&kind) else {
        // Not a V3-family pool (e.g. a V2 kind) → no V3 liquidity clock.
        return Ok(None);
    };
    // `table` is a hardcoded constant produced by the kind match — never user
    // input, so interpolating it into the SQL is safe.
    let sql = format!("SELECT liquidity_update_block FROM {table} WHERE pool_id = ?1");
    let block: Option<i64> = match conn.query_row(&sql, rusqlite::params![pool_id], |r| r.get(0)) {
        Ok(b) => b,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(block)
}

/// V4 twin of [`fetch_liquidity_update_block_on_conn`] — reads the V4 row's
/// `liquidity_update_block` resolved through the pool-manager join.
///
/// # Errors
/// [`DbError::Sqlite`] on a query failure.
pub fn fetch_liquidity_update_block_v4_on_conn(
    conn: &Connection,
    pool_manager: Address,
    pool_id_hash: B256,
) -> Result<Option<i64>, DbError> {
    let hash_hex = format!("{pool_id_hash}"); // B256 Display includes the 0x prefix
    let block: Option<i64> = match conn.query_row(
        "SELECT v4.liquidity_update_block \
         FROM uniswap_v4_pools v4 \
         JOIN managed_pools mp ON mp.id = v4.managed_pool_id \
         JOIN pool_managers pm ON pm.id = mp.manager_id \
         WHERE v4.pool_hash = ?1 AND pm.address = ?2",
        rusqlite::params![hash_hex, pool_manager.to_checksum(None)],
        |r| r.get(0),
    ) {
        Ok(b) => b,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(block)
}

/// The tick-map-DB surface the `assemble_*_tick_map` Db arm reads against.
/// Implemented by [`DegenbotDb`] (per-call `lock()` — each read its own
/// snapshot) AND [`crate::snapshot_db::SnapshotDb`] (held read transaction —
/// every read shares one frozen DB view across `build_paths`, the consistency
/// replacement for the retired `SnapshotStore`, epic `XEANMB`).
///
/// `fetch_newest_update_block` is included so the snapshot-seed block `S` can
/// be read on the SAME held tx as the per-pool data (so `S` matches the data).
pub trait TickMapDb: Send + Sync {
    /// The V3 [`LiquidityMap`] for `pool_address` (mirrors
    /// `DatabaseSnapshot.get_liquidity_map`).
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    fn fetch_liquidity_map(&self, pool_address: Address) -> Result<Option<LiquidityMap>, DbError>;

    /// The V4 [`LiquidityMap`] for `(pool_manager, pool_id_hash)`.
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a malformed column.
    fn fetch_liquidity_map_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<LiquidityMap>, DbError>;

    /// Newest `exchanges.last_update_block` for the chain + family.
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a query failure.
    fn fetch_newest_update_block(
        &self,
        chain: i64,
        family: ExchangeFamily,
    ) -> Result<Option<i64>, DbError>;

    /// The V3 pool's `liquidity_update_block` (the block its DB liquidity map
    /// is exact at). Used as the authoritative liquidity clock of a DB-seeded
    /// `Tracked` pool so seed verification anchors at the pool's OWN block
    /// rather than the aggregate `S` (task 4TWM7C).
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a query failure.
    fn fetch_liquidity_update_block(&self, pool_address: Address) -> Result<Option<i64>, DbError>;

    /// V4 twin of [`Self::fetch_liquidity_update_block`].
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a query failure.
    fn fetch_liquidity_update_block_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<i64>, DbError>;
}

impl TickMapDb for DegenbotDb {
    fn fetch_liquidity_map(&self, pool_address: Address) -> Result<Option<LiquidityMap>, DbError> {
        DegenbotDb::fetch_liquidity_map(self, pool_address)
    }
    fn fetch_liquidity_map_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<LiquidityMap>, DbError> {
        DegenbotDb::fetch_liquidity_map_v4(self, pool_manager, pool_id_hash)
    }
    fn fetch_newest_update_block(
        &self,
        chain: i64,
        family: ExchangeFamily,
    ) -> Result<Option<i64>, DbError> {
        DegenbotDb::fetch_newest_update_block(self, chain, family)
    }
    fn fetch_liquidity_update_block(&self, pool_address: Address) -> Result<Option<i64>, DbError> {
        DegenbotDb::fetch_liquidity_update_block(self, pool_address)
    }
    fn fetch_liquidity_update_block_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<i64>, DbError> {
        DegenbotDb::fetch_liquidity_update_block_v4(self, pool_manager, pool_id_hash)
    }
}
