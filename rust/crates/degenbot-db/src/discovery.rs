//! Pool discovery DB writers — the PoolCreated-event apply fns.
//!
//! Port of `cli/pool_updater_configs.py`'s `update_v2_pools` / `update_v3_pools`
//! / `update_v4_pools` `SQLAlchemy`
//! upsert loops. The Python event fetch + ABI decode + RPC fee lookup stay as
//! the shell (`stays-python`); the Rust core owns the get-or-create-token
//! escalate + the polymorphic pool-row insert + the
//! `ExchangeTable.last_update_block` stamp.
//!
//! # The polymorphic two-step insert
//!
//! V2/V3 pools use joined-table inheritance: a base `pools` row (carrying
//! `kind` + `token0_id`/`token1_id` + `exchange_id`) + a per-subclass detail
//! row (`pool_id` = the base `pools.id`, carrying the fee / tick-spacing
//! columns). [`DegenbotDb::upsert_v2_pools`] / [`upsert_v3_pools`] replicate
//! the `SQLAlchemy` `session.add(database_type(**kwargs))` trajectory as an
//! `INSERT INTO pools` → take `last_insert_rowid` → `INSERT INTO <subclass> …` pair.
//!
//! V4 pools use a separate polymorphic base (`managed_pools`, discriminator
//! `kind='uniswap_v4'`, `manager_id`) + the `uniswap_v4_pools` detail row
//! (`managed_pool_id` = the base id). [`DegenbotDb::upsert_v4_pools`]
//! resolves the `PoolManagerTable` id from the manager address (one `SELECT`)
//! then per-row: `INSERT INTO managed_pools …` → `INSERT INTO uniswap_v4_pools …`.
//!
//! # Address storage
//!
//! Addresses are stored EIP-55-checksummed (`Address::to_checksum(None)`),
//! matching the Python `get_checksum_address` + the read seams
//! ([`crate::read::DegenbotDb::fetch_pool_by_address`]).
//!
//! # Auto-commit (matches `liquidity_updater.rs`)
//!
//! Each `INSERT`/`UPDATE` is auto-committed by `rusqlite` on the
//! write-capable connection (the established pattern from
//! [`crate::liquidity_updater`]); no explicit `BEGIN`/`COMMIT` wrapper.

use alloy::primitives::Address;
use rusqlite::{params, OptionalExtension};

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::schema::table;

// ── row-input structs (the PyO3 seam extracts Python args into these) ──────

/// A V2 pool-row to upsert. `stable` is honored only when `kind ==
/// "aerodrome_v2"` (the sole V2 subclass with a `stable` column); `None`
/// for the other V2 families.
#[derive(Debug, Clone)]
pub struct V2PoolRowInput {
    /// The pool contract address (stored checksummed).
    pub address: Address,
    /// Token0 address (get-or-create'd into `erc20_tokens`).
    pub token0_address: Address,
    /// Token1 address.
    pub token1_address: Address,
    /// Fee for token0 swaps (the V2 `fee_token0` column).
    pub fee_token0: i64,
    /// Fee for token1 swaps (`fee_token1`).
    pub fee_token1: i64,
    /// Stable-pool flag — `Some` for Aerodrome, `None` for the rest.
    pub stable: Option<bool>,
}

/// A V3 pool-row to upsert.
#[derive(Debug, Clone)]
pub struct V3PoolRowInput {
    /// The pool contract address.
    pub address: Address,
    pub token0_address: Address,
    pub token1_address: Address,
    /// Swap fee (stored in both `fee_token0` + `fee_token1`, matching the
    /// Python `fee_token0=fee, fee_token1=fee` trajectory).
    pub fee: i64,
    /// V3 tick spacing.
    pub tick_spacing: i64,
}

/// A V4 pool-row to upsert. The `manager_id` is resolved inside
/// [`DegenbotDb::upsert_v4_pools`] from the passed `manager_address` (one
/// `SELECT` per batch), matching the Python `session.scalar(select(
/// PoolManagerTable).where(address == exchange.factory))` lookup.
#[derive(Debug, Clone)]
pub struct V4PoolRowInput {
    /// The V4 pool id as a 0x-prefixed 66-char hex string.
    pub pool_hash: String,
    /// The hooks contract address.
    pub hooks: Address,
    pub currency0_address: Address,
    pub currency1_address: Address,
    /// Swap fee (stored in both `fee_currency0` + `fee_currency1`).
    pub fee: i64,
    pub tick_spacing: i64,
}

// ── the writer substrate ───────────────────────────────────────────────────

impl DegenbotDb {
    /// Resolve a `PoolManagerTable` row id by `(chain, address)`. Read seam
    /// for the V4 discovery upsert (the manager lookup the Python path did
    /// inline via `session.scalar(select(PoolManagerTable)…)`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn fetch_pool_manager_id_by_address(
        &self,
        chain: i64,
        address: &str,
    ) -> Result<Option<i64>, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT id FROM pool_managers WHERE chain = ?1 AND address = ?2",
                params![chain, address],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Insert a batch of V2 pool rows (polymorphic base + subclass detail).
    ///
    /// For each [`V2PoolRowInput`]: get-or-create the two `erc20_tokens` rows,
    /// `INSERT INTO pools (address, chain, kind, token0_id, token1_id,
    /// exchange_id)` → take `last_insert_rowid` → `INSERT INTO
    /// <subclass_table> (pool_id, fee_token0, fee_token1, fee_denominator
    /// [, stable])`. The subclass table is resolved from `kind` via
    /// [`table::v2_v3_subclass_table`]; the `stable` column is included only
    /// for `kind == "aerodrome_v2"` (+ then only when the row's `stable` is
    /// `Some`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on any insert failure (e.g. a duplicate
    /// `(address, chain)` raising `SQLITE_CONSTRAINT` — matching the Python
    /// `session.add` trajectory's `IntegrityError`), or
    /// [`DbError::Decode`] if `kind` is not a known V2 family discriminator.
    pub fn upsert_v2_pools(
        &self,
        chain: i64,
        kind: &str,
        exchange_id: i64,
        fee_denominator: i64,
        rows: &[V2PoolRowInput],
    ) -> Result<(), DbError> {
        let sub = table::v2_v3_subclass_table(kind)
            .ok_or_else(|| DbError::Decode(format!("not a V2/V3 kind: {kind:?}")))?;
        if !table::is_v2_kind(kind) {
            return Err(DbError::Decode(format!("not a V2 kind: {kind:?}")));
        }
        let is_aerodrome = kind == "aerodrome_v2";
        for r in rows {
            // Token escalate first — `get_or_create_erc20_token` locks
            // `self.conn` internally, so it MUST NOT be called while we hold
            // the connection guard (parking_lot is non-reentrant → deadlock).
            let token0_id = self.get_or_create_erc20_token(
                chain,
                &r.token0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let token1_id = self.get_or_create_erc20_token(
                chain,
                &r.token1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let conn = self.lock();
            conn.execute(
                "INSERT INTO pools (address, chain, kind, token0_id, token1_id, exchange_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    r.address.to_checksum(None),
                    chain,
                    kind,
                    token0_id,
                    token1_id,
                    exchange_id,
                ],
            )?;
            let pool_id = conn.last_insert_rowid();
            if is_aerodrome {
                conn.execute(
                    &format!(
                        "INSERT INTO {sub} (pool_id, fee_token0, fee_token1, fee_denominator, \
                         stable) VALUES (?1, ?2, ?3, ?4, ?5)"
                    ),
                    params![
                        pool_id,
                        r.fee_token0,
                        r.fee_token1,
                        fee_denominator,
                        r.stable
                    ],
                )?;
            } else {
                conn.execute(
                    &format!(
                        "INSERT INTO {sub} (pool_id, fee_token0, fee_token1, fee_denominator) \
                         VALUES (?1, ?2, ?3, ?4)"
                    ),
                    params![pool_id, r.fee_token0, r.fee_token1, fee_denominator],
                )?;
            }
        }
        Ok(())
    }

    /// Insert a batch of V3 pool rows (polymorphic base + subclass detail).
    ///
    /// For each row: get-or-create the two tokens, `INSERT INTO pools`,
    /// then `INSERT INTO <subclass_table> (pool_id, tick_spacing,
    /// fee_token0, fee_token1, fee_denominator)` — the `liquidity_update_block`
    /// / `liquidity_update_log_index` columns are left `NULL` (the defaults,
    /// matching the Python ORM `None` until the liquidity updater stamps them).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on an insert failure or
    /// [`DbError::Decode`] if `kind` is not a V3 family discriminator.
    pub fn upsert_v3_pools(
        &self,
        chain: i64,
        kind: &str,
        exchange_id: i64,
        fee_denominator: i64,
        rows: &[V3PoolRowInput],
    ) -> Result<(), DbError> {
        let sub = table::v2_v3_subclass_table(kind)
            .ok_or_else(|| DbError::Decode(format!("not a V2/V3 kind: {kind:?}")))?;
        if !table::is_v3_kind(kind) {
            return Err(DbError::Decode(format!("not a V3 kind: {kind:?}")));
        }
        for r in rows {
            // Token escalate first (see `upsert_v2_pools` re-entrancy note).
            let token0_id = self.get_or_create_erc20_token(
                chain,
                &r.token0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let token1_id = self.get_or_create_erc20_token(
                chain,
                &r.token1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let conn = self.lock();
            conn.execute(
                "INSERT INTO pools (address, chain, kind, token0_id, token1_id, exchange_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    r.address.to_checksum(None),
                    chain,
                    kind,
                    token0_id,
                    token1_id,
                    exchange_id,
                ],
            )?;
            let pool_id = conn.last_insert_rowid();
            conn.execute(
                &format!(
                    "INSERT INTO {sub} (pool_id, tick_spacing, fee_token0, fee_token1, \
                     fee_denominator) VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![pool_id, r.tick_spacing, r.fee, r.fee, fee_denominator],
            )?;
        }
        Ok(())
    }

    /// Insert a batch of V4 pool rows (separate `managed_pools` polymorphic
    /// base + `uniswap_v4_pools` detail).
    ///
    /// Resolves the `PoolManagerTable` id from `manager_address` (one
    /// `SELECT`); returns [`DbError::Decode`] if no manager row matches (the
    /// Python path `assert manager_in_db is not None`). For each row:
    /// get-or-create the two currency tokens, `INSERT INTO managed_pools
    /// (kind, manager_id)` → `INSERT INTO uniswap_v4_pools (managed_pool_id,
    /// pool_hash, hooks, currency0_id, currency1_id, fee_currency0,
    /// fee_currency1, fee_denominator, tick_spacing)` (the
    /// `liquidity_update_block`/`log_index` left `NULL`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on an insert failure or
    /// [`DbError::Decode`] if the manager is not found.
    pub fn upsert_v4_pools(
        &self,
        chain: i64,
        manager_address: &str,
        fee_denominator: i64,
        rows: &[V4PoolRowInput],
    ) -> Result<(), DbError> {
        let manager_id = self
            .fetch_pool_manager_id_by_address(chain, manager_address)?
            .ok_or_else(|| {
                DbError::Decode(format!(
                    "no pool_manager for chain {chain} address {manager_address:?}"
                ))
            })?;
        for r in rows {
            // Currency-token escalate first (see `upsert_v2_pools` re-entrancy note).
            let currency0_id = self.get_or_create_erc20_token(
                chain,
                &r.currency0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let currency1_id = self.get_or_create_erc20_token(
                chain,
                &r.currency1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let conn = self.lock();
            conn.execute(
                "INSERT INTO managed_pools (kind, manager_id) VALUES (?1, ?2)",
                params!["uniswap_v4", manager_id],
            )?;
            let managed_pool_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO uniswap_v4_pools (managed_pool_id, pool_hash, hooks, \
                 currency0_id, currency1_id, fee_currency0, fee_currency1, fee_denominator, \
                 tick_spacing) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    managed_pool_id,
                    &r.pool_hash,
                    r.hooks.to_checksum(None),
                    currency0_id,
                    currency1_id,
                    r.fee,
                    r.fee,
                    fee_denominator,
                    r.tick_spacing,
                ],
            )?;
        }
        Ok(())
    }

    /// Stamp an `ExchangeTable.last_update_block`. Port of the
    /// `exchange.last_update_block = working_end_block` trajectory in
    /// `cli/pool.py::pool_update` (the discovery-writer close-out stamp).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn set_exchange_last_update_block(
        &self,
        chain_id: i64,
        exchange_id: i64,
        block: i64,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE exchanges SET last_update_block = ?1 \
             WHERE chain_id = ?2 AND id = ?3",
            params![block, chain_id, exchange_id],
        )?;
        Ok(())
    }
}
