//! Market-scoped purge of one Aave V3 market's populated state.
//!
//! [`DegenbotDb::reset_aave_market_on_conn`] clears every row the updater
//! populated for ONE market and rewinds the market's `last_update_block` to a
//! cold-boot block, so the same cold-boot entry the updater takes on an empty
//! database (`bootstrap_pool_contracts` plus the chunk loop) re-populates the
//! market from scratch. Rows keyed to another market, and every table no market
//! keys (`erc20_tokens`, `pools`, `exchanges`, price data), are untouched.
//!
//! # What a purge keeps
//!
//! The purge restores the state `aave activate` seeds — the market row itself,
//! the `POOL_ADDRESS_PROVIDER` contract row, the GHO `erc20_tokens` row, and a
//! bare `aave_gho_tokens` row. Those four are the substrate the cold-boot
//! bootstrap resolves; deleting the address-provider row would leave the
//! bootstrap without its fetch anchor (`bootstrap_pool_contracts` reads the
//! `ProxyCreated` events from that address). Everything else the updater
//! populated — reserves, users, positions, per-asset and per-user configs,
//! e-mode categories, the remaining contract rows — is removed.
//!
//! # Delete order (the foreign-key argument)
//!
//! Children first, so the purge is correct on a connection that enforces
//! foreign keys as well as on this crate's default (unenforced) connections:
//!
//! 1. `aave_v3_collateral_positions` — references `aave_v3_users` and
//!    `aave_v3_assets`.
//! 2. `aave_v3_debt_positions` — same two parents.
//! 3. `aave_v3_user_collateral_configs` — same two parents.
//! 4. `aave_v3_asset_configs` — references `aave_v3_assets`.
//! 5. `aave_v3_users` — references `aave_v3_assets` through
//!    `isolation_mode_collateral_asset_id`.
//! 6. `aave_v3_assets` — references `aave_v3_emode_categories` through
//!    `e_mode_category_id`.
//! 7. `aave_v3_emode_categories` — references only the market row (kept).
//! 8. `aave_v3_contracts` (minus the address provider) — references only the
//!    market row (kept).
//!
//! The GHO token row is not deleted: `aave_gho_tokens` is keyed by the chain's
//! GHO `erc20_tokens` row, not by market, so the purge clears its
//! updater-populated columns back to the bare seed shape instead. The GHO token
//! is chain-unique, so this is the only market-scoped reading available.
//!
//! # Atomicity
//!
//! These functions take a borrowed [`rusqlite::Connection`]: the caller owns
//! ONE `Transaction` and commits it, so a failure part-way through leaves the
//! database byte-identical (see the `_on_conn` seam in this module's parent).

use rusqlite::Connection;

use super::{params, DbError, DegenbotDb};

/// The one relation a purge clears rather than empties.
const GHO_TABLE: &str = "aave_gho_tokens";

/// The market-scoped relations the purge clears, in foreign-key-safe order,
/// each with the predicate that selects its rows for `?1` = the market id.
const PURGE_STEPS: &[PurgeStep] = &[
    PurgeStep {
        table: "aave_v3_collateral_positions",
        predicate: "asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?1) \
                    OR user_id IN (SELECT id FROM aave_v3_users WHERE market_id = ?1)",
    },
    PurgeStep {
        table: "aave_v3_debt_positions",
        predicate: "asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?1) \
                    OR user_id IN (SELECT id FROM aave_v3_users WHERE market_id = ?1)",
    },
    PurgeStep {
        table: "aave_v3_user_collateral_configs",
        predicate: "asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?1) \
                    OR user_id IN (SELECT id FROM aave_v3_users WHERE market_id = ?1)",
    },
    PurgeStep {
        table: "aave_v3_asset_configs",
        predicate: "asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?1)",
    },
    PurgeStep {
        table: "aave_v3_users",
        predicate: "market_id = ?1",
    },
    PurgeStep {
        table: "aave_v3_assets",
        predicate: "market_id = ?1",
    },
    PurgeStep {
        table: "aave_v3_emode_categories",
        predicate: "market_id = ?1",
    },
    PurgeStep {
        table: "aave_v3_contracts",
        predicate: "market_id = ?1 AND name <> 'POOL_ADDRESS_PROVIDER'",
    },
];

/// One market-scoped relation and the predicate that selects its rows.
struct PurgeStep {
    table: &'static str,
    predicate: &'static str,
}

/// One relation's share of a purge: the table name and the row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveMarketPurgeCount {
    /// The relation the count belongs to.
    pub table: &'static str,
    /// The rows the purge removes (or, for `aave_gho_tokens`, clears).
    pub rows: i64,
}

/// The chain row a market belongs to.
///
/// # Errors
///
/// [`DbError::MissingRow`] when no market row matches `market_id`.
fn market_chain_id(conn: &Connection, market_id: i64) -> Result<i64, DbError> {
    conn.query_row(
        "SELECT chain_id FROM aave_v3_markets WHERE id = ?1",
        params![market_id],
        |row| row.get(0),
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => {
            DbError::MissingRow(format!("aave_v3_markets id={market_id} (purge target)"))
        }
        other => DbError::Sqlite(other),
    })
}

/// The chain's GHO token rows whose updater-populated columns a purge clears.
const GHO_CLEAR_PREDICATE: &str = "token_id IN (SELECT id FROM erc20_tokens WHERE chain = ?1) \
     AND (v_token_id IS NOT NULL OR v_gho_discount_rate_strategy IS NOT NULL \
          OR v_gho_discount_token IS NOT NULL)";

impl DegenbotDb {
    /// The per-relation row counts a purge of `market_id` would remove, in
    /// [`PURGE_STEPS`] order with the GHO row last. Reads only — the `--dry-run`
    /// preview.
    ///
    /// # Errors
    ///
    /// [`DbError::MissingRow`] when no market row matches `market_id`, or
    /// [`DbError::Sqlite`] on a query failure.
    pub fn count_aave_market_rows(
        &self,
        market_id: i64,
    ) -> Result<Vec<AaveMarketPurgeCount>, DbError> {
        Self::count_aave_market_rows_on_conn(&self.lock(), market_id)
    }

    /// The `&Connection`-bound variant of [`Self::count_aave_market_rows`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::count_aave_market_rows`].
    pub fn count_aave_market_rows_on_conn(
        conn: &Connection,
        market_id: i64,
    ) -> Result<Vec<AaveMarketPurgeCount>, DbError> {
        let chain_id = market_chain_id(conn, market_id)?;
        let mut counts = Vec::with_capacity(PURGE_STEPS.len() + 1);
        for step in PURGE_STEPS {
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE {}",
                step.table, step.predicate
            );
            let rows: i64 = conn.query_row(&sql, params![market_id], |row| row.get(0))?;
            counts.push(AaveMarketPurgeCount {
                table: step.table,
                rows,
            });
        }
        let sql = format!("SELECT COUNT(*) FROM {GHO_TABLE} WHERE {GHO_CLEAR_PREDICATE}");
        let rows: i64 = conn.query_row(&sql, params![chain_id], |row| row.get(0))?;
        counts.push(AaveMarketPurgeCount {
            table: GHO_TABLE,
            rows,
        });
        Ok(counts)
    }

    /// Remove every row `market_id` populated, returning the per-relation
    /// counts removed. The market row and its `aave activate` substrate stay.
    ///
    /// The caller owns the `Transaction`: on a failure part-way through, the
    /// dropped transaction reverts every statement.
    ///
    /// # Errors
    ///
    /// [`DbError::MissingRow`] when no market row matches `market_id`, or
    /// [`DbError::Sqlite`] on a statement failure.
    pub fn purge_aave_market_on_conn(
        conn: &Connection,
        market_id: i64,
    ) -> Result<Vec<AaveMarketPurgeCount>, DbError> {
        let chain_id = market_chain_id(conn, market_id)?;
        let mut counts = Vec::with_capacity(PURGE_STEPS.len() + 1);
        for step in PURGE_STEPS {
            let sql = format!("DELETE FROM {} WHERE {}", step.table, step.predicate);
            let removed = conn.execute(&sql, params![market_id])?;
            counts.push(AaveMarketPurgeCount {
                table: step.table,
                rows: i64::try_from(removed).unwrap_or(i64::MAX),
            });
        }
        let sql = format!(
            "UPDATE {GHO_TABLE} SET v_token_id = NULL, \
             v_gho_discount_rate_strategy = NULL, v_gho_discount_token = NULL \
             WHERE {GHO_CLEAR_PREDICATE}"
        );
        let cleared = conn.execute(&sql, params![chain_id])?;
        counts.push(AaveMarketPurgeCount {
            table: GHO_TABLE,
            rows: i64::try_from(cleared).unwrap_or(i64::MAX),
        });
        Ok(counts)
    }

    /// Purge `market_id` and rewind its `last_update_block` to
    /// `cold_boot_block`, so the market's next update run re-enters the
    /// cold-boot path (bootstrap plus the first chunk) from that block.
    ///
    /// The rewind rides the caller's purge transaction: a failure between the
    /// two steps must not leave a purged market pointing at a stale cursor.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::purge_aave_market_on_conn`].
    pub fn reset_aave_market_on_conn(
        conn: &Connection,
        market_id: i64,
        cold_boot_block: i64,
    ) -> Result<Vec<AaveMarketPurgeCount>, DbError> {
        let counts = Self::purge_aave_market_on_conn(conn, market_id)?;
        Self::set_market_last_update_block_on_conn(conn, market_id, cold_boot_block)?;
        Ok(counts)
    }
}
