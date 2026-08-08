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
use crate::rows::{ExchangeRow, PoolManagerRow};
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
        Self::fetch_pool_manager_id_by_address_on_conn(&conn, chain, address)
    }

    /// The single-transaction-bound variant of
    /// [`Self::fetch_pool_manager_id_by_address`] (CKXCOB 3a).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn fetch_pool_manager_id_by_address_on_conn(
        conn: &rusqlite::Connection,
        chain: i64,
        address: &str,
    ) -> Result<Option<i64>, DbError> {
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
        let conn = self.lock();
        Self::upsert_v2_pools_on_conn(&conn, chain, kind, exchange_id, fee_denominator, rows)
    }

    /// The single-transaction-bound variant of [`Self::upsert_v2_pools`]
    /// (CKXCOB 3a). Accepts a borrowed [`rusqlite::Connection`] (a chunk-loop
    /// `Transaction` derefs to one) so the pool-updater chunk loop can write a
    /// batch under ONE connection + ONE transaction. Retires the per-row
    /// `self.lock()` cycle the `&self` path previously needed (the token
    /// escalate now uses [`Self::get_or_create_erc20_token_on_conn`] on the
    /// SAME borrowed conn — no re-lock → no `parking_lot` non-reentrant deadlock).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn upsert_v2_pools_on_conn(
        conn: &rusqlite::Connection,
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
            let token0_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.token0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let token1_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.token1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
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
        let conn = self.lock();
        Self::upsert_v3_pools_on_conn(&conn, chain, kind, exchange_id, fee_denominator, rows)
    }

    /// The single-transaction-bound variant of [`Self::upsert_v3_pools`]
    /// (CKXCOB 3a). See [`Self::upsert_v2_pools_on_conn`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn upsert_v3_pools_on_conn(
        conn: &rusqlite::Connection,
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
            let token0_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.token0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let token1_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.token1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
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
        let conn = self.lock();
        Self::upsert_v4_pools_on_conn(&conn, chain, manager_address, fee_denominator, rows)
    }

    /// The single-transaction-bound variant of [`Self::upsert_v4_pools`]
    /// (CKXCOB 3a). See [`Self::upsert_v2_pools_on_conn`].
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn upsert_v4_pools_on_conn(
        conn: &rusqlite::Connection,
        chain: i64,
        manager_address: &str,
        fee_denominator: i64,
        rows: &[V4PoolRowInput],
    ) -> Result<(), DbError> {
        let manager_id =
            Self::fetch_pool_manager_id_by_address_on_conn(conn, chain, manager_address)?
                .ok_or_else(|| {
                    DbError::Decode(format!(
                        "no pool_manager for chain {chain} address {manager_address:?}"
                    ))
                })?;
        for r in rows {
            let currency0_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.currency0_address.to_checksum(None),
                None,
                None,
                None,
            )?;
            let currency1_id = Self::get_or_create_erc20_token_on_conn(
                conn,
                chain,
                &r.currency1_address.to_checksum(None),
                None,
                None,
                None,
            )?;
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
        Self::set_exchange_last_update_block_on_conn(&conn, chain_id, exchange_id, block)
    }

    /// The single-transaction-bound variant of
    /// [`Self::set_exchange_last_update_block`] (CKXCOB 3a) — the chunk loop's
    /// end-of-chunk stamp, callable on the chunk's `Transaction` so the stamp
    /// commits atomically with the chunk's pool + liquidity writes (the §1
    /// atomicity invariant's structural fix).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn set_exchange_last_update_block_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        exchange_id: i64,
        block: i64,
    ) -> Result<(), DbError> {
        conn.execute(
            "UPDATE exchanges SET last_update_block = ?1 \
             WHERE chain_id = ?2 AND id = ?3",
            params![block, chain_id, exchange_id],
        )?;
        Ok(())
    }

    /// Resolve an `exchanges` row by `(chain_id, name)`, inserting a new
    /// `active = false`, `last_update_block = NULL` row if none exists. This is
    /// the substrate the `exchange activate/deactivate` CLI delegates to for
    /// the get-or-create step (the Python `cli/exchange.py` trajectory:
    /// `session.scalar(select(ExchangeTable).where(chain_id, name))` →
    /// `session.add(ExchangeTable(active=True, factory=...))`).
    ///
    /// # Active flag is NOT touched here
    ///
    /// If a row already exists, it is returned **unchanged** — flipping
    /// `active` is a separate concern owned by [`Self::set_exchange_active`].
    /// The insert path stamps `active = false` (the CLI flips it true for
    /// activate; the row is the get-or-create substrate, not the
    /// activate/deactivate primitive).
    ///
    /// # No unique index → explicit SELECT-then-INSERT dedup
    ///
    /// `exchanges` has NO unique index on `(chain_id, name)` (see
    /// [`crate::schema_head`]), and this epic MUST NOT add one (a new index
    /// would force a new Alembic migration = out of scope per ADR-010). So this
    /// uses the explicit `SELECT … WHERE chain_id, name` → `INSERT` dedup the
    /// Python code uses, NOT `INSERT … ON CONFLICT(chain_id, name)`. The
    /// inherent SELECT-then-INSERT race matches the Python trajectory's own
    /// race; the CLI holds the connection mutex via [`Self::lock`] so a
    /// single-process caller is race-free.
    ///
    /// # Address storage
    ///
    /// `factory` is always stored EIP-55-checksummed
    /// (`Address::to_checksum(None)`); `deployer` is likewise checksummed when
    /// `Some`, `NULL` when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] if
    /// the inserted/selected row fails to decode (the `factory`/`deployer`
    /// checksum round-trip is round-trip-safe by construction).
    pub fn upsert_exchange(
        &self,
        chain_id: i64,
        name: &str,
        factory: Address,
        deployer: Option<Address>,
    ) -> Result<ExchangeRow, DbError> {
        let conn = self.lock();
        // 1. resolve by (chain_id, name) — the Python `session.scalar(select(
        //    ExchangeTable).where(chain_id, name))` read.
        let existing = conn
            .query_row(
                "SELECT id, chain_id, name, active, last_update_block, factory, deployer \
                 FROM exchanges WHERE chain_id = ?1 AND name = ?2",
                params![chain_id, name],
                |row| ExchangeRow::from_row(row).map_err(rusqlite::Error::from),
            )
            .optional()?;
        if let Some(row) = existing {
            // Found → return UNCHANGED. Do NOT touch `active`.
            return Ok(row);
        }
        // 2. absent → INSERT with active=false, last_update_block=NULL,
        //    factory/deployer checksummed. RETURNING avoids a re-SELECT round
        //    trip + mirrors the fetch read's column order for `from_row`.
        let mut stmt = conn.prepare(
            "INSERT INTO exchanges (chain_id, name, active, last_update_block, factory, deployer) \
             VALUES (?1, ?2, 0, NULL, ?3, ?4) \
             RETURNING id, chain_id, name, active, last_update_block, factory, deployer",
        )?;
        let mut rows = stmt.query(params![
            chain_id,
            name,
            factory.to_checksum(None),
            deployer.map(|a| a.to_checksum(None)),
        ])?;
        let row = rows
            .next()?
            .ok_or_else(|| DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        ExchangeRow::from_row(row)
    }

    /// Flip an `exchanges` row's `active` flag. The activate/deactivate
    /// primitive the `exchange activate` / `exchange deactivate` CLI commands
    /// delegate to (the Python trajectory's `exchange.active = True/False;
    /// session.commit()`).
    ///
    /// # Not-found
    ///
    /// If no row matches `id`, `rows_affected() == 0` and this returns
    /// [`DbError::MissingRow`] — the crate's dedicated not-found variant (the
    /// existing `MissingRow(String)` carries the missing key in its message).
    /// The CLI maps this to the user-facing "no entry" message the Python
    /// commands print.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::MissingRow`]
    /// when no `exchanges` row matches `exchange_id`.
    pub fn set_exchange_active(&self, exchange_id: i64, active: bool) -> Result<(), DbError> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE exchanges SET active = ?1 WHERE id = ?2",
            params![active, exchange_id],
        )?;
        if n == 0 {
            return Err(DbError::MissingRow(format!(
                "exchange id {exchange_id} not found"
            )));
        }
        Ok(())
    }

    /// Upsert a `pool_managers` row by `(address, chain)`. The
    /// get-or-create substrate for the V4 manager row the `exchange activate
    /// uniswap_v4` CLI creates (the Python trajectory: `session.add(
    /// PoolManagerTable(address, chain, kind, exchange_id, state_view))`).
    ///
    /// # Unique index → `ON CONFLICT` upsert
    ///
    /// Unlike `exchanges`, `pool_managers` HAS the `ix_pool_manager_address_chain`
    /// unique index on `(address, chain)` (see [`crate::schema_head`]), so this
    /// uses `INSERT … ON CONFLICT(address, chain) DO UPDATE SET
    /// kind=excluded.kind, state_view=excluded.state_view,
    /// exchange_id=excluded.exchange_id RETURNING …` — idempotent: re-calling
    /// with a changed `state_view` (or `kind`/`exchange_id`) updates the row in
    /// place (same `id`), re-calling identical leaves it unchanged.
    ///
    /// # Address storage
    ///
    /// `address` is stored EIP-55-checksummed; `state_view` likewise when
    /// `Some`, `NULL` when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure or [`DbError::Decode`] if
    /// the returned row fails to decode.
    pub fn upsert_pool_manager(
        &self,
        address: Address,
        chain: i64,
        kind: &str,
        state_view: Option<Address>,
        exchange_id: i64,
    ) -> Result<PoolManagerRow, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "INSERT INTO pool_managers (address, chain, kind, state_view, exchange_id) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(address, chain) DO UPDATE SET \
               kind = excluded.kind, \
               state_view = excluded.state_view, \
               exchange_id = excluded.exchange_id \
             RETURNING id, address, chain, kind, state_view, exchange_id",
        )?;
        let mut rows = stmt.query(params![
            address.to_checksum(None),
            chain,
            kind,
            state_view.map(|a| a.to_checksum(None)),
            exchange_id,
        ])?;
        let row = rows
            .next()?
            .ok_or_else(|| DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        PoolManagerRow::from_row(row)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::migrate::SchemaState;
    use alloy::primitives::address;

    /// A fresh in-memory **write-capable** DB (the writer methods require the
    /// `open_in_memory_for_writes` handle — `query_only=on` on the read handle
    /// would refuse the `INSERT`/`UPDATE`).
    fn write_db() -> DegenbotDb {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        db
    }

    // ── upsert_exchange ──────────────────────────────────────────────────

    #[test]
    fn upsert_exchange_inserts_new_row_active_false_block_null() {
        let db = write_db();
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        let deployer = address!("0x02276A281287Fd86c88DD4a0B5F62Fde4Be243C6");

        let row = db
            .upsert_exchange(8453, "uniswap_v3", factory, Some(deployer))
            .unwrap();

        assert_eq!(row.chain_id, 8453);
        assert_eq!(row.name, "uniswap_v3");
        assert!(!row.active, "new exchange must be inserted active=false");
        assert_eq!(row.last_update_block, None);
        assert_eq!(row.factory, factory);
        assert_eq!(row.deployer, Some(deployer));
    }

    #[test]
    fn upsert_exchange_returns_same_row_unchanged_on_second_call_no_new_insert() {
        let db = write_db();
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        let deployer = address!("0x02276A281287Fd86c88DD4a0B5F62Fde4Be243C6");
        let name = "uniswap_v3";

        let first = db
            .upsert_exchange(8453, name, factory, Some(deployer))
            .unwrap();
        // Second call with the SAME (chain_id, name) — even with different
        // factory/deployer args — must return the SAME row, no new insert.
        let second = db
            .upsert_exchange(
                8453,
                name,
                address!("0x0000000000000000000000000000000000000001"),
                None,
            )
            .unwrap();

        assert_eq!(first.id, second.id, "second call must return same id");
        assert_eq!(
            second.factory, factory,
            "existing factory must be unchanged"
        );
        assert_eq!(
            second.deployer,
            Some(deployer),
            "existing deployer unchanged"
        );
        assert_eq!(second.active, first.active);
        // exactly one row in exchanges.
        let conn = db.lock();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn upsert_exchange_factory_round_trips_and_deploys_none_when_none() {
        let db = write_db();
        let factory = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
        let row = db.upsert_exchange(1, "uniswap_v2", factory, None).unwrap();
        assert_eq!(row.factory, factory);
        assert_eq!(row.deployer, None);
        // verify EIP-55 checksum was stored (uppercase-mixed, not all-lowercase).
        let conn = db.lock();
        let stored: String = conn
            .query_row(
                "SELECT factory FROM exchanges WHERE id = ?1",
                params![row.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, factory.to_checksum(None));
    }

    // ── set_exchange_active ──────────────────────────────────────────────

    #[test]
    fn set_exchange_active_flips_false_to_true_and_back() {
        let db = write_db();
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        let row = db
            .upsert_exchange(8453, "uniswap_v3", factory, None)
            .unwrap();
        assert!(!row.active, "upsert starts inactive");

        db.set_exchange_active(row.id, true).unwrap();
        let after = db.fetch_exchange(row.id).unwrap().unwrap();
        assert!(after.active);
        assert_eq!(after.id, row.id);

        db.set_exchange_active(row.id, false).unwrap();
        let after = db.fetch_exchange(row.id).unwrap().unwrap();
        assert!(!after.active);
    }

    #[test]
    fn set_exchange_active_missing_row_returns_not_found_error() {
        let db = write_db();
        let err = db.set_exchange_active(9999, true).unwrap_err();
        assert!(
            matches!(err, DbError::MissingRow(ref m) if m.contains("9999")),
            "expected MissingRow for nonexistent id, got {err:?}"
        );
    }

    // ── upsert_pool_manager ──────────────────────────────────────────────

    #[test]
    fn upsert_pool_manager_inserts_new_row() {
        let db = write_db();
        let exchange = db
            .upsert_exchange(
                8453,
                "uniswap_v4",
                address!("0x00000000000444F6DC9C6A8FD8aB7C0B84D6Bf27"),
                None,
            )
            .unwrap();
        let manager = address!("0x00000000000444F6DC9C6A6Fc78Fa31c6a7C8e1d");
        let state_view = address!("0x49aab6879414105c7D7C09d2Cb01414e2e9dE828");

        let row = db
            .upsert_pool_manager(manager, 8453, "uniswap_v4", Some(state_view), exchange.id)
            .unwrap();

        assert_eq!(row.address, manager);
        assert_eq!(row.chain, 8453);
        assert_eq!(row.kind, "uniswap_v4");
        assert_eq!(row.state_view, Some(state_view));
        assert_eq!(row.exchange_id, exchange.id);
    }

    #[test]
    fn upsert_pool_manager_updates_state_view_in_place_same_id() {
        let db = write_db();
        let exchange = db
            .upsert_exchange(
                8453,
                "uniswap_v4",
                address!("0x00000000000444F6DC9C6A8FD8aB7C0B84D6Bf27"),
                None,
            )
            .unwrap();
        let manager = address!("0x00000000000444F6DC9C6A6Fc78Fa31c6a7C8e1d");
        let sv1 = address!("0x0000000000000000000000000000000000000001");
        let sv2 = address!("0x0000000000000000000000000000000000000002");

        let first = db
            .upsert_pool_manager(manager, 8453, "uniswap_v4", Some(sv1), exchange.id)
            .unwrap();
        let second = db
            .upsert_pool_manager(manager, 8453, "uniswap_v4", Some(sv2), exchange.id)
            .unwrap();

        assert_eq!(first.id, second.id, "upsert must keep same id");
        assert_eq!(second.state_view, Some(sv2), "state_view updated in place");

        // exactly one row in pool_managers for this (address, chain).
        let conn = db.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pool_managers WHERE address = ?1 AND chain = ?2",
                params![manager.to_checksum(None), 8453],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn upsert_pool_manager_identical_recall_is_unchanged() {
        let db = write_db();
        let exchange = db
            .upsert_exchange(
                1,
                "uniswap_v4",
                address!("0x00000000000444F6DC9C6A8FD8aB7C0B84D6Bf27"),
                None,
            )
            .unwrap();
        let manager = address!("0x00000000000444F6DC9C6A6Fc78Fa31c6a7C8e1d");
        let state_view = address!("0x49aab6879414105c7D7C09d2Cb01414e2e9dE828");

        let first = db
            .upsert_pool_manager(manager, 1, "uniswap_v4", Some(state_view), exchange.id)
            .unwrap();
        let second = db
            .upsert_pool_manager(manager, 1, "uniswap_v4", Some(state_view), exchange.id)
            .unwrap();

        assert_eq!(first, second, "identical recall must be a no-op");
    }

    // ── fetch_exchange_by_name (the read-by-name companion) ───────────────

    #[test]
    fn fetch_exchange_by_name_returns_inserted_row_and_none_when_missing() {
        let db = write_db();
        let factory = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
        let inserted = db.upsert_exchange(1, "uniswap_v2", factory, None).unwrap();

        let fetched = db.fetch_exchange_by_name(1, "uniswap_v2").unwrap();
        assert_eq!(fetched, Some(inserted));

        // a missing name returns None.
        let missing = db.fetch_exchange_by_name(1, "uniswap_v3").unwrap();
        assert!(missing.is_none());

        // the lookup is scoped by chain_id — a different chain returns None
        // even for the same name.
        let cross_chain = db.fetch_exchange_by_name(8453, "uniswap_v2").unwrap();
        assert!(cross_chain.is_none());
    }

    // ── CKXCOB 3a: single-transaction chunk atomicity ──────────────────
    //
    // The §1 atomicity invariant's structural proof: the `*_on_conn` write
    // variants run on ONE borrowed `Connection` (the chunk's `Transaction`),
    // so the pool writes + the `last_update_block` stamp commit together OR
    // roll back together. This test exercises both outcomes.

    #[test]
    fn on_conn_writes_commit_atomically_with_the_stamp() {
        let db = write_db();
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        let exchange = db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
        assert!(exchange.last_update_block.is_none());

        let pool = V3PoolRowInput {
            address: address!("0x1111111111111111111111111111111111111111"),
            token0_address: address!("0x2222222222222222222222222222222222222222"),
            token1_address: address!("0x3333333333333333333333333333333333333333"),
            fee: 500,
            tick_spacing: 10,
        };

        // ONE transaction wraps the pool write + the stamp.
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::upsert_v3_pools_on_conn(
            &tx,
            1,
            "uniswap_v3",
            exchange.id,
            1_000_000,
            std::slice::from_ref(&pool),
        )
        .unwrap();
        DegenbotDb::set_exchange_last_update_block_on_conn(&tx, 1, exchange.id, 100).unwrap();
        tx.commit().unwrap();
        drop(guard);

        // Committed → both the pool row + the stamp landed.
        let fetched_pool = db.fetch_pool_by_address(pool.address, 1).unwrap();
        assert!(fetched_pool.is_some(), "pool row committed with the stamp");
        assert_eq!(fetched_pool.unwrap().exchange_id, exchange.id);
        let after = db.fetch_exchange(exchange.id).unwrap().unwrap();
        assert_eq!(
            after.last_update_block,
            Some(100),
            "stamp committed atomically"
        );
    }

    #[test]
    fn on_conn_writes_roll_back_together_when_the_transaction_is_dropped() {
        let db = write_db();
        let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
        let exchange = db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();

        // First chunk: commit a pool + stamp at block 100.
        let pool1 = V3PoolRowInput {
            address: address!("0x1111111111111111111111111111111111111111"),
            token0_address: address!("0x2222222222222222222222222222222222222222"),
            token1_address: address!("0x3333333333333333333333333333333333333333"),
            fee: 500,
            tick_spacing: 10,
        };
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            DegenbotDb::upsert_v3_pools_on_conn(
                &tx,
                1,
                "uniswap_v3",
                exchange.id,
                1_000_000,
                std::slice::from_ref(&pool1),
            )
            .unwrap();
            DegenbotDb::set_exchange_last_update_block_on_conn(&tx, 1, exchange.id, 100).unwrap();
            tx.commit().unwrap();
        }

        // Second chunk: write a SECOND pool + advance the stamp to 200, then
        // DROP the transaction (simulating an interrupt / error before commit).
        let pool2 = V3PoolRowInput {
            address: address!("0x4444444444444444444444444444444444444444"),
            token0_address: address!("0x5555555555555555555555555555555555555555"),
            token1_address: address!("0x6666666666666666666666666666666666666666"),
            fee: 3000,
            tick_spacing: 60,
        };
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            DegenbotDb::upsert_v3_pools_on_conn(
                &tx,
                1,
                "uniswap_v3",
                exchange.id,
                1_000_000,
                std::slice::from_ref(&pool2),
            )
            .unwrap();
            DegenbotDb::set_exchange_last_update_block_on_conn(&tx, 1, exchange.id, 200).unwrap();
            // Drop `tx` WITHOUT committing → rollback (the §1 atomicity invariant).
            drop(tx);
        }

        // Rolled back → BOTH the second pool AND the stamp advance reverted.
        assert!(
            db.fetch_pool_by_address(pool2.address, 1)
                .unwrap()
                .is_none(),
            "rolled-back chunk's pool write must not be durable",
        );
        let after = db.fetch_exchange(exchange.id).unwrap().unwrap();
        assert_eq!(
            after.last_update_block,
            Some(100),
            "rolled-back chunk's stamp advance must not be durable (restart-safe)",
        );
        // The first chunk's pool survived.
        assert!(db
            .fetch_pool_by_address(pool1.address, 1)
            .unwrap()
            .is_some());
    }
}
