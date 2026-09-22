//! Read-only candidate-pool discovery for path enumeration (Gap G2).
//!
//! The Python driver's `build_paths.py` (`src/degenbot/runner/build_paths.py`)
//! discovers its candidate-pool universe from the `SQLAlchemy` ORM and hands
//! each `PathStep` to the Rust `PoolBuilder`. A pure-Rust `cargo add degenbot`
//! consumer needs the same READ-ONLY enumeration without the ORM layer, so
//! this module exposes [`DiscoveryPoolRow`] — a typed row per discovered pool
//! carrying EXACTLY the columns the construction path reads:
//!
//! - the base `pools` row (`id` / `address` / `chain` / `kind` / `token0_id` / `token1_id` / `exchange_id`),
//! - the `token0` / `token1` `erc20_tokens` rows (address + decimals + name / symbol),
//! - the owning `exchanges` row (name / `active` / `last_update_block` / factory / deployer),
//! - per-family detail: V2 fees + Aerodrome `stable`; V3 fees + `tick_spacing` + the liquidity-update marker; V4 `pool_hash` / `hooks` / currencies + fees + `tick_spacing` + the resolving `pool_managers` row (address + `state_view`).
//!
//! # Read-only + held-snapshot discipline
//!
//! The single SELECT surface is [`fetch_discovery_rows_on_conn`], which takes a
//! borrowed [`rusqlite::Connection`] so it composes with the
//! [`crate::snapshot_db::SnapshotDb`] held-deferred-tx handle (the same
//! discipline [`crate::read::fetch_newest_update_block_on_conn`] follows).
//! [`crate::connection::DegenbotDb::fetch_discovery_rows`] is the
//! self-locking wrapper and [`crate::snapshot_db::SnapshotDb::fetch_discovery_rows`]
//! is the frozen-snapshot variant. No writes; the schema stays Rust-DDL-owned
//! (AGENTS.md 0.7 kill list: the Alembic path is untouched).
//!
//! # Family coverage
//!
//! Discovery covers the pool families `build_paths.py` can build: V2, V3, and
//! Uniswap V4. Balancer / Curve pools are NOT part of the candidate graph in
//! `build_paths.py` (its `_POOL_VERSION_MAP` is V2/V3/V4 only) and are
//! therefore not enumerated here.

use alloy::primitives::{Address, B256};
use rusqlite::{Connection, Row};

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::{decode_address, decode_opt_address};
use crate::rows::pool::decode_pool_hash;
use crate::rows::{Erc20TokenRow, ExchangeRow, LiquidityPoolRow, PoolManagerRow};
use crate::schema::table::{
    ERC20_TOKENS, EXCHANGES, MANAGED_POOLS, POOL_MANAGERS, UNISWAP_V4_POOLS,
};

// ── row structs ────────────────────────────────────────────────────────────

/// A discovered V2-family pool (Uniswap / Sushi / Pancake / Camelot /
/// `SwapBased` / Aerodrome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryV2Row {
    /// The base `pools` row.
    pub pool: LiquidityPoolRow,
    /// The owning `exchanges` row.
    pub exchange: ExchangeRow,
    /// The `token0` `erc20_tokens` row.
    pub token0: Erc20TokenRow,
    /// The `token1` `erc20_tokens` row.
    pub token1: Erc20TokenRow,
    /// Fee charged on the `token0` side.
    pub fee_token0: i64,
    /// Fee charged on the `token1` side.
    pub fee_token1: i64,
    /// The fee denominator.
    pub fee_denominator: i64,
    /// Aerodrome-only stable-pool flag; `None` for the other V2 families
    /// (whose subclass table has no `stable` column).
    pub stable: Option<bool>,
}

/// A discovered V3-family pool (Uniswap / Sushi / Pancake / Aerodrome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryV3Row {
    /// The base `pools` row.
    pub pool: LiquidityPoolRow,
    /// The owning `exchanges` row.
    pub exchange: ExchangeRow,
    /// The `token0` `erc20_tokens` row.
    pub token0: Erc20TokenRow,
    /// The `token1` `erc20_tokens` row.
    pub token1: Erc20TokenRow,
    /// Fee charged on the `token0` side.
    pub fee_token0: i64,
    /// Fee charged on the `token1` side.
    pub fee_token1: i64,
    /// The fee denominator.
    pub fee_denominator: i64,
    /// The V3 tick spacing.
    pub tick_spacing: i64,
    /// Liquidity-update marker block (stamped by the liquidity updater).
    pub liquidity_update_block: Option<i64>,
    /// Liquidity-update marker log index.
    pub liquidity_update_log_index: Option<i64>,
}

/// A discovered Uniswap V4 managed pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryV4Row {
    /// The `managed_pools.id` (the V4 pool's polymorphic primary key).
    pub managed_pool_id: i64,
    /// The V4 `pool_hash` (`bytes32`) unique pool key.
    pub pool_hash: B256,
    /// The hooks contract address.
    pub hooks: Address,
    /// The resolving `pool_managers` row (address + `state_view`).
    pub manager: PoolManagerRow,
    /// The `currency0` `erc20_tokens` row (V4's `token0`).
    pub token0: Erc20TokenRow,
    /// The `currency1` `erc20_tokens` row (V4's `token1`).
    pub token1: Erc20TokenRow,
    /// The owning `exchanges` row (via `pool_managers.exchange_id`).
    pub exchange: ExchangeRow,
    /// Fee charged on the `currency0` side.
    pub fee_currency0: i64,
    /// Fee charged on the `currency1` side.
    pub fee_currency1: i64,
    /// The fee denominator.
    pub fee_denominator: i64,
    /// The V4 tick spacing.
    pub tick_spacing: i64,
    /// Liquidity-update marker block (stamped by the liquidity updater).
    pub liquidity_update_block: Option<i64>,
    /// Liquidity-update marker log index.
    pub liquidity_update_log_index: Option<i64>,
}

/// A discovered candidate pool, one variant per buildable family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryPoolRow {
    /// A V2-family pool.
    V2(DiscoveryV2Row),
    /// A V3-family pool.
    V3(DiscoveryV3Row),
    /// A Uniswap V4 managed pool.
    V4(DiscoveryV4Row),
}

impl DiscoveryPoolRow {
    /// The raw `kind` discriminator string (`"uniswap_v3"`, `"aerodrome_v2"`,
    /// `"uniswap_v4"`, ...) — the single-table-inheritance polymorphic
    /// identity used to recover the concrete Python pool class.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::V2(r) => &r.pool.kind,
            Self::V3(r) => &r.pool.kind,
            Self::V4(r) => &r.manager.kind,
        }
    }

    /// The `token0` / `currency0` token row.
    #[must_use]
    pub fn token0(&self) -> &Erc20TokenRow {
        match self {
            Self::V2(r) => &r.token0,
            Self::V3(r) => &r.token0,
            Self::V4(r) => &r.token0,
        }
    }

    /// The `token1` / `currency1` token row.
    #[must_use]
    pub fn token1(&self) -> &Erc20TokenRow {
        match self {
            Self::V2(r) => &r.token1,
            Self::V3(r) => &r.token1,
            Self::V4(r) => &r.token1,
        }
    }

    /// The owning exchange row.
    #[must_use]
    pub fn exchange(&self) -> &ExchangeRow {
        match self {
            Self::V2(r) => &r.exchange,
            Self::V3(r) => &r.exchange,
            Self::V4(r) => &r.exchange,
        }
    }
}

// ── SELECT construction ────────────────────────────────────────────────────

/// `(kind, subclass table, is_aerodrome_stable)` for every V2 family.
///
/// Mirrors [`crate::schema::table::v2_v3_subclass_table`]; kept as an ordered
/// const so one `UNION ALL` covers every V2 subclass table.
const V2_FAMILIES: &[(&str, &str, bool)] = &[
    ("uniswap_v2", "uniswap_v2_pools", false),
    ("sushiswap_v2", "sushiswap_v2_pools", false),
    ("pancakeswap_v2", "pancakeswap_v2_pools", false),
    ("camelot_v2", "camelot_v2_pools", false),
    ("swapbased_v2", "swapbased_v2_pools", false),
    ("aerodrome_v2", "aerodrome_v2_pools", true),
];

/// `(kind, subclass table)` for every V3 family.
const V3_FAMILIES: &[(&str, &str)] = &[
    ("uniswap_v3", "uniswap_v3_pools"),
    ("sushiswap_v3", "sushiswap_v3_pools"),
    ("pancakeswap_v3", "pancakeswap_v3_pools"),
    ("aerodrome_v3", "aerodrome_v3_pools"),
];

/// Column-list prefix shared by every V2/V3 union branch: the base `pools`
/// row, then the two token joins, then the exchange row.
const V2V3_SELECT_PREFIX: &str = "p.id, p.address, p.chain, p.kind, p.token0_id, \
     p.token1_id, p.exchange_id, \
     t0.id, t0.chain, t0.address, t0.name, t0.symbol, t0.decimals, \
     t1.id, t1.chain, t1.address, t1.name, t1.symbol, t1.decimals, \
     e.id, e.chain_id, e.name, e.active, e.last_update_block, e.factory, e.deployer";

/// Join clause shared by every V2/V3 union branch.
const V2V3_JOINS: &str = "FROM pools p \
     JOIN erc20_tokens t0 ON t0.id = p.token0_id \
     JOIN erc20_tokens t1 ON t1.id = p.token1_id \
     JOIN exchanges e ON e.id = p.exchange_id";

/// Build the one-statement V2/V3 discovery SELECT (`UNION ALL` over every
/// subclass table so a single query covers all V2 + V3 families).
fn v2v3_select() -> String {
    let mut branches: Vec<String> = Vec::with_capacity(V2_FAMILIES.len() + V3_FAMILIES.len());
    for (kind, table, aerodrome) in V2_FAMILIES {
        let stable = if *aerodrome { "s.stable" } else { "NULL" };
        branches.push(format!(
            "SELECT {V2V3_SELECT_PREFIX}, \
             s.fee_token0, s.fee_token1, s.fee_denominator, \
             {stable}, NULL, NULL, NULL \
             {V2V3_JOINS} \
             JOIN {table} s ON s.pool_id = p.id \
             WHERE p.chain = ?1 AND p.kind = '{kind}'"
        ));
    }
    for (kind, table) in V3_FAMILIES {
        branches.push(format!(
            "SELECT {V2V3_SELECT_PREFIX}, \
             s.fee_token0, s.fee_token1, s.fee_denominator, \
             NULL, s.tick_spacing, s.liquidity_update_block, \
             s.liquidity_update_log_index \
             {V2V3_JOINS} \
             JOIN {table} s ON s.pool_id = p.id \
             WHERE p.chain = ?1 AND p.kind = '{kind}'"
        ));
    }
    format!("{union} ORDER BY 1", union = branches.join(" UNION ALL "))
}

/// The one-statement V4 discovery SELECT.
fn v4_select() -> String {
    format!(
        "SELECT mp.id, u.pool_hash, u.hooks, u.currency0_id, u.currency1_id, \
         u.fee_currency0, u.fee_currency1, u.fee_denominator, u.tick_spacing, \
         u.liquidity_update_block, u.liquidity_update_log_index, \
         pm.id, pm.address, pm.chain, pm.kind, pm.state_view, pm.exchange_id, \
         t0.id, t0.chain, t0.address, t0.name, t0.symbol, t0.decimals, \
         t1.id, t1.chain, t1.address, t1.name, t1.symbol, t1.decimals, \
         e.id, e.chain_id, e.name, e.active, e.last_update_block, e.factory, e.deployer \
         FROM {UNISWAP_V4_POOLS} u \
         JOIN {MANAGED_POOLS} mp ON mp.id = u.managed_pool_id \
         JOIN {POOL_MANAGERS} pm ON pm.id = mp.manager_id \
         JOIN {ERC20_TOKENS} t0 ON t0.id = u.currency0_id \
         JOIN {ERC20_TOKENS} t1 ON t1.id = u.currency1_id \
         JOIN {EXCHANGES} e ON e.id = pm.exchange_id \
         WHERE pm.chain = ?1 \
         ORDER BY mp.id",
    )
}

// ── row decoding ───────────────────────────────────────────────────────────

/// Decode the base `pools` row from columns `[0, 7)` of a V2/V3 branch.
fn decode_pool_base(row: &Row<'_>) -> Result<LiquidityPoolRow, DbError> {
    Ok(LiquidityPoolRow {
        id: row.get(0)?,
        address: decode_address(&row.get::<_, String>(1)?)?,
        chain: row.get(2)?,
        kind: row.get(3)?,
        token0_id: row.get(4)?,
        token1_id: row.get(5)?,
        exchange_id: row.get(6)?,
    })
}

/// Decode an `erc20_tokens` row starting at `base`.
fn decode_token(row: &Row<'_>, base: usize) -> Result<Erc20TokenRow, DbError> {
    Ok(Erc20TokenRow {
        id: row.get(base)?,
        chain: row.get(base + 1)?,
        address: decode_address(&row.get::<_, String>(base + 2)?)?,
        name: row.get(base + 3)?,
        symbol: row.get(base + 4)?,
        decimals: row.get(base + 5)?,
    })
}

/// Decode an `exchanges` row starting at `base`.
fn decode_exchange(row: &Row<'_>, base: usize) -> Result<ExchangeRow, DbError> {
    Ok(ExchangeRow {
        id: row.get(base)?,
        chain_id: row.get(base + 1)?,
        name: row.get(base + 2)?,
        active: row.get(base + 3)?,
        last_update_block: row.get(base + 4)?,
        factory: decode_address(&row.get::<_, String>(base + 5)?)?,
        deployer: decode_opt_address(row.get::<_, Option<String>>(base + 6)?.as_deref())?,
    })
}

/// Decode a [`DiscoveryV4Row`] from the [`v4_select`] column layout.
fn decode_v4_row(row: &Row<'_>) -> Result<DiscoveryV4Row, DbError> {
    Ok(DiscoveryV4Row {
        managed_pool_id: row.get(0)?,
        pool_hash: decode_pool_hash(&row.get::<_, String>(1)?)?,
        hooks: decode_address(&row.get::<_, String>(2)?)?,
        manager: PoolManagerRow {
            id: row.get(11)?,
            address: decode_address(&row.get::<_, String>(12)?)?,
            chain: row.get(13)?,
            kind: row.get(14)?,
            state_view: decode_opt_address(row.get::<_, Option<String>>(15)?.as_deref())?,
            exchange_id: row.get(16)?,
        },
        token0: decode_token(row, 17)?,
        token1: decode_token(row, 23)?,
        exchange: decode_exchange(row, 29)?,
        fee_currency0: row.get(5)?,
        fee_currency1: row.get(6)?,
        fee_denominator: row.get(7)?,
        tick_spacing: row.get(8)?,
        liquidity_update_block: row.get(9)?,
        liquidity_update_log_index: row.get(10)?,
    })
}

// ── the read surface ───────────────────────────────────────────────────────

/// Read every candidate pool for `chain_id` on a borrowed connection.
///
/// The single read surface: [`DegenbotDb::fetch_discovery_rows`] wraps it with
/// the handle's connection lock, and
/// [`crate::snapshot_db::SnapshotDb::fetch_discovery_rows`] runs it inside the
/// held deferred read transaction (so discovery sees one frozen DB cut — the
/// `build_paths.py` snapshot discipline). Read-only by construction.
///
/// Result order: all V2/V3 rows ascending by `pools.id`, then all V4 rows
/// ascending by `managed_pools.id` (each branch is `ORDER BY`-pinned).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a
/// malformed address / pool-hash / integer column.
pub fn fetch_discovery_rows_on_conn(
    conn: &Connection,
    chain_id: i64,
) -> Result<Vec<DiscoveryPoolRow>, DbError> {
    let mut out: Vec<DiscoveryPoolRow> = Vec::new();

    {
        let sql = v2v3_select();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![chain_id])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(3)?;
            let pool = decode_pool_base(row)?;
            let token0 = decode_token(row, 7)?;
            let token1 = decode_token(row, 13)?;
            let exchange = decode_exchange(row, 19)?;
            let fee_token0: i64 = row.get(26)?;
            let fee_token1: i64 = row.get(27)?;
            let fee_denominator: i64 = row.get(28)?;
            if crate::schema::table::is_v2_kind(&kind) {
                out.push(DiscoveryPoolRow::V2(DiscoveryV2Row {
                    pool,
                    exchange,
                    token0,
                    token1,
                    fee_token0,
                    fee_token1,
                    fee_denominator,
                    stable: row.get(29)?,
                }));
            } else if crate::schema::table::is_v3_kind(&kind) {
                out.push(DiscoveryPoolRow::V3(DiscoveryV3Row {
                    pool,
                    exchange,
                    token0,
                    token1,
                    fee_token0,
                    fee_token1,
                    fee_denominator,
                    tick_spacing: row.get(30)?,
                    liquidity_update_block: row.get(31)?,
                    liquidity_update_log_index: row.get(32)?,
                }));
            } else {
                return Err(DbError::Decode(format!(
                    "discovery select returned unrecognized pool kind {kind:?}"
                )));
            }
        }
    }

    {
        let sql = v4_select();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![chain_id])?;
        while let Some(row) = rows.next()? {
            out.push(DiscoveryPoolRow::V4(decode_v4_row(row)?));
        }
    }

    Ok(out)
}

/// Read every V4 managed pool for `chain_id` on a borrowed connection.
///
/// The V4 half of [`fetch_discovery_rows_on_conn`], sharing the SAME
/// [`v4_select`] statement so the roster and the full discovery read can never
/// drift. Result order follows the statement: ascending `managed_pools.id`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] on a
/// malformed address / pool-hash / integer column.
pub fn fetch_v4_discovery_rows_on_conn(
    conn: &Connection,
    chain_id: i64,
) -> Result<Vec<DiscoveryV4Row>, DbError> {
    let sql = v4_select();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params![chain_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(decode_v4_row(row)?);
    }
    Ok(out)
}

impl DegenbotDb {
    /// Read every candidate pool for `chain_id` — the `build_paths.py`
    /// enumeration surface, self-locking on this handle's connection.
    ///
    /// For the frozen-startup-cut discipline use
    /// [`crate::snapshot_db::SnapshotDb::fetch_discovery_rows`] instead.
    ///
    /// # Errors
    ///
    /// Same error conditions as [`fetch_discovery_rows_on_conn`].
    pub fn fetch_discovery_rows(&self, chain_id: i64) -> Result<Vec<DiscoveryPoolRow>, DbError> {
        let conn = self.lock();
        fetch_discovery_rows_on_conn(&conn, chain_id)
    }

    /// Read every V4 managed pool for `chain_id` — the connector-index roster
    /// surface, the V4 half of [`Self::fetch_discovery_rows`], self-locking on
    /// this handle's connection.
    ///
    /// # Errors
    ///
    /// Same error conditions as [`fetch_v4_discovery_rows_on_conn`].
    pub fn fetch_v4_discovery_rows(&self, chain_id: i64) -> Result<Vec<DiscoveryV4Row>, DbError> {
        let conn = self.lock();
        fetch_v4_discovery_rows_on_conn(&conn, chain_id)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::migrate::SchemaState;

    /// Seed an in-memory write-capable DB with one exchange + two V2 pools
    /// (an Aerodrome stable pool + a plain Uniswap V2 pool) sharing a token
    /// pair. Exercises the V2 `stable` decode + the non-Aerodrome `NULL`
    /// branch the frozen `parity.db` (V3 + V4 only) does not cover.
    fn seed_v2_db() -> DegenbotDb {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        {
            let conn = db.lock();
            conn.execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) VALUES
                   (1, 1, '0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b', 'A', 'A', 18),
                   (2, 1, '0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA', 'B', 'B', 18);
                 INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory) VALUES
                   (1, 1, 'aerodrome_v2', 1, 100, '0x1AE92F98d07afFD821725Cd463C223c1E8C5A6d2');
                 INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) VALUES
                   (1, '0x7b8c1d2E3f4a5b6c7d8E9f0A1b2C3D4E5f6a7b8c', 1, 'aerodrome_v2', 1, 2, 1),
                   (2, '0x498581fF718922c3f8e6A244956aF099B2652b2b', 1, 'uniswap_v2', 1, 2, 1);
                 INSERT INTO aerodrome_v2_pools (pool_id, fee_token0, fee_token1, fee_denominator, stable)
                   VALUES (1, 0, 0, 10000, 1);
                 INSERT INTO uniswap_v2_pools (pool_id, fee_token0, fee_token1, fee_denominator)
                   VALUES (2, 3, 3, 1000);",
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn discovers_aerodrome_stable_and_plain_v2() {
        let db = seed_v2_db();
        let rows = db.fetch_discovery_rows(1).unwrap();
        assert_eq!(rows.len(), 2, "two V2 pools");

        let DiscoveryPoolRow::V2(stable) = &rows[0] else {
            panic!("expected V2 row, got {:?}", rows[0]);
        };
        assert_eq!(stable.pool.kind, "aerodrome_v2");
        assert_eq!(stable.stable, Some(true), "aerodrome stable flag decoded");
        assert_eq!(stable.fee_denominator, 10_000);
        assert_eq!(stable.exchange.last_update_block, Some(100));
        assert_eq!(stable.token0.decimals, Some(18));

        let DiscoveryPoolRow::V2(plain) = &rows[1] else {
            panic!("expected V2 row, got {:?}", rows[1]);
        };
        assert_eq!(plain.pool.kind, "uniswap_v2");
        assert_eq!(plain.stable, None, "non-aerodrome V2 has no stable column");
        assert_eq!((plain.fee_token0, plain.fee_denominator), (3, 1000));
        // ordered by pools.id
        assert_eq!(rows[0].kind(), "aerodrome_v2");
        assert_eq!(rows[1].kind(), "uniswap_v2");
    }

    /// Seed an in-memory DB with one chain-1 V4 managed pool (non-hooked) so
    /// the V4 roster read has a graph to decode.
    fn seed_v4_db() -> DegenbotDb {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        let t0 = Address::new([0x11; 20]);
        let t1 = Address::new([0x22; 20]);
        let manager = Address::new([0xaa; 20]);
        let factory = Address::new([0x99; 20]);
        let state_view = Address::new([0x77; 20]);
        let hash = B256::from([0x11; 32]);
        {
            let conn = db.lock();
            conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) \
                 VALUES (1, 1, ?1, 'T0', 'T0', 18), (2, 1, ?2, 'T1', 'T1', 6)",
                rusqlite::params![t0.to_checksum(None), t1.to_checksum(None)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO exchanges (id, chain_id, name, active, factory) \
                 VALUES (1, 1, 'uniswap_v4', 1, ?1)",
                [factory.to_checksum(None)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) \
                 VALUES (1, ?1, 1, 'uniswap_v4', ?2, 1)",
                rusqlite::params![manager.to_checksum(None), state_view.to_checksum(None)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO managed_pools (id, kind, manager_id) VALUES (7, 'uniswap_v4', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO uniswap_v4_pools (managed_pool_id, pool_hash, hooks, currency0_id, \
                 currency1_id, fee_currency0, fee_currency1, fee_denominator, tick_spacing) \
                 VALUES (7, ?1, ?2, 1, 2, 500, 500, 1000000, 10)",
                rusqlite::params![format!("{hash:#x}"), Address::ZERO.to_checksum(None)],
            )
            .unwrap();
        }
        db
    }

    /// The V4 roster read decodes the same row the full discovery read's V4
    /// half yields, chain-scoped.
    #[test]
    fn v4_roster_read_matches_the_v4_half_of_discovery() {
        let db = seed_v4_db();
        let roster = db.fetch_v4_discovery_rows(1).unwrap();
        assert_eq!(roster.len(), 1, "one chain-1 V4 pool");
        let row = &roster[0];
        assert_eq!(row.managed_pool_id, 7);
        assert_eq!(row.pool_hash, B256::from([0x11; 32]));
        assert_eq!(row.hooks, Address::ZERO);
        assert_eq!(row.manager.address, Address::new([0xaa; 20]));
        assert_eq!(row.token0.address, Address::new([0x11; 20]));
        assert_eq!(row.token1.address, Address::new([0x22; 20]));
        assert_eq!(
            (row.fee_currency0, row.fee_currency1, row.fee_denominator),
            (500, 500, 1_000_000)
        );
        assert_eq!(row.tick_spacing, 10);

        // The roster is exactly the V4 subset of the full discovery read.
        let all = db.fetch_discovery_rows(1).unwrap();
        let v4: Vec<_> = all
            .into_iter()
            .filter_map(|r| match r {
                DiscoveryPoolRow::V4(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(roster, v4);

        assert!(db.fetch_v4_discovery_rows(8453).unwrap().is_empty());
    }
}
