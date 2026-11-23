//! Aave position get-or-create + scaled-token applies + position lookups.

use super::util::parse_decimal_u256;
use super::{params, DbError, DegenbotDb, OptionalExtension, U256};
use crate::schema::ScaledTokenPosition;

/// The post-apply context the discount refresh reads: the GHO debt position's
/// `scaled_balance`/`last_index` (POST-apply — the `ScaledTokenMint`/`Burn` apply
/// landed them) + the user's `address` + `stk_aave_balance`. Built by
/// [`DegenbotDb::lookup_debt_position_refresh_context_on_conn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebtPositionRefreshContext {
    /// `aave_v3_users.id` — the SET-stk-aave-balance + the
    /// write-`gho_discount` target.
    pub user_id: i64,
    /// The user's checksummed address (`VARCHAR(42)`) — the `balanceOf`
    /// `eth_call` argument.
    pub user_address: String,
    /// `aave_v3_debt_positions.balance` (POST-apply, scaled).
    pub scaled_balance: U256,
    /// `aave_v3_debt_positions.last_index` (POST-apply) — the `ray_mul`
    /// multiplier. `None` falls back to the event's index (Python: the
    /// position has no prior index).
    pub last_index: Option<U256>,
    /// `aave_v3_users.stk_aave_balance` — the `balanceOf` cache. `None`
    /// triggers `get_or_init_stk_aave_balance`.
    pub stk_aave_balance: Option<U256>,
}

/// The pure balance-state computation behind the scaled-token apply (shared
/// by the read-then-write shell and the chunk substrate's with-current
/// variant): the signed-delta add, the `_burnScaled` clamp, and the
/// `last_index` max-with-prev reconciliation.
///
/// # `last_index` reconciliation
///
/// Advances `last_index` to `new_index` ONLY when `new_index >
/// COALESCE(current_last_index, 0)` — advance `last_index` only to a strictly
/// greater index. The max-with-prev guard prevents an out-of-log-
/// order event from clobbering a newer event's index.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the computed new balance overflows `U256`.
fn finish_scaled_token_balance_state(
    table: &str,
    position_id: i64,
    current_balance: alloy::primitives::U256,
    current_index: Option<alloy::primitives::U256>,
    balance_delta: alloy::primitives::I256,
    new_index: alloy::primitives::U256,
) -> Result<(alloy::primitives::U256, Option<alloy::primitives::U256>), DbError> {
    // Compute new balance (I256 arithmetic — delta may be negative).
    let current_balance_i =
        alloy::primitives::I256::try_from(current_balance).unwrap_or(alloy::primitives::I256::MAX);
    let new_balance_i = current_balance_i + balance_delta;
    // Aave V3 `_burnScaled` cap-to-scaledBalance clamp.
    // The on-chain `_burnScaled` computes `amountScaled =
    // amountTotal.rayDiv(index)` then `if (amountScaled > scaledBalance)
    // amountScaled = scaledBalance;` before burning — the contract NEVER
    // decrements the position below zero. When Rust's `scaled_amount`
    // (carried as the burn's `balance_delta`) exceeds the live `current_balance`
    // (because of accumulated 1-2 wei drift between Rust's stored balance and
    // the on-chain `scaledBalance`), the unguarded `current_balance_i +
    // balance_delta` goes negative and would crash the chunk with
    // `balance would go negative`. Mirroring the contract, clamp the new
    // balance to ZERO instead of erroring. Symmetric with the GHO-side clamp
    // in `process_gho_debt_burn`'s `>=` branch.
    //
    // NB: this is the ACTUAL on-chain rule for BOTH aToken collateral burns
    // AND vToken debt burns (`_burnScaled` is defined on both). Applied
    // here so it consults the live running balance inside the chunk's apply
    // loop (NOT the build-time snapshot — chunk events are applied at
    // end-of-tx, AFTER the per-event `process_*_burn` builder ran; the
    // per-tx running state is at apply time, not at build time).
    let new_balance = if new_balance_i < alloy::primitives::I256::ZERO
        && balance_delta.is_negative()
    {
        // Clamp to zero (= Aave's `min(amountScaled, scaledBalance) == scaledBalance`)
        // and emit a stderr diagnostic so the surge is auditable — the drift
        // surface this masks is real (Rust's stored balance diverged from the
        // chain) but it's preferable to crashing every chunk where a
        // long-running position drifts by a single wei at full-withdraw.
        // Deliberate stderr audit line (kept off the tracing path intentionally).
        #[expect(clippy::print_stderr)]
        {
            eprintln!(
                "AAVE-CLAMP table={table} pos={position_id} current={current_balance} delta={balance_delta} reason=aave-v3-_burnScaled-cap-to-scaledBalance"
            );
        }
        alloy::primitives::U256::ZERO
    } else {
        let new_balance: alloy::primitives::U256 = new_balance_i.try_into().map_err(|_| {
            DbError::Decode(format!(
                "{table} id={position_id}: new balance overflows U256 (={new_balance_i})"
            ))
        })?;
        new_balance
    };
    // last_index reconciliation — max-with-prev.
    let new_last_index = match current_index {
        Some(cur) if new_index > cur => Some(new_index),
        Some(_) => current_index, // keep the prior higher index
        None => Some(new_index),  // first event: set it
    };
    Ok((new_balance, new_last_index))
}

/// The scaled-token apply's write arm (the shell's + the chunk substrate's
/// with-current variant share it): the `prepare_cached` position UPDATE the
/// ledger golden replays, parameterized `(balance, last_index, id)` in the
/// single-row shape's order.
fn write_scaled_token_balance_state(
    conn: &rusqlite::Connection,
    table: &str,
    position_id: i64,
    new_balance: alloy::primitives::U256,
    new_last_index: Option<alloy::primitives::U256>,
) -> Result<(), DbError> {
    let update_sql = match table {
        "aave_v3_collateral_positions" => {
            "UPDATE aave_v3_collateral_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        "aave_v3_debt_positions" => {
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        _ => unreachable!(),
    };
    conn.prepare_cached(update_sql)?.execute(params![
        new_balance.to_string(),
        new_last_index.map(|i| i.to_string()),
        position_id,
    ])?;
    Ok(())
}

/// The shared `get_or_create_position` body for collateral + debt (the Python
/// `get_or_create_position[T]` is generic over both table types; the only
/// difference is the table name — both have identical `(user_id, asset_id,
/// balance, last_index)` columns). `table` is the literal `aave_v3_*` name.
fn get_or_create_position_on_conn(
    conn: &rusqlite::Connection,
    user_id: i64,
    asset_id: i64,
    table: &str,
) -> Result<i64, DbError> {
    // the existing-row lookup is parameterized by table (constant string, no
    // injection surface — both table names are compile-time literals below).
    let sql = match table {
        "aave_v3_collateral_positions" => {
            "SELECT id FROM aave_v3_collateral_positions \
             WHERE user_id = ?1 AND asset_id = ?2"
        }
        "aave_v3_debt_positions" => {
            "SELECT id FROM aave_v3_debt_positions WHERE user_id = ?1 AND asset_id = ?2"
        }
        _ => unreachable!("get_or_create_position: bad table {table:?}"),
    };
    if let Some(id) = conn
        .prepare_cached(sql)?
        .query_row::<i64, _, _>(params![user_id, asset_id], |r| r.get(0))
        .optional()?
    {
        return Ok(id);
    }
    let insert_sql = match table {
        "aave_v3_collateral_positions" => {
            "INSERT INTO aave_v3_collateral_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '0', NULL)"
        }
        "aave_v3_debt_positions" => {
            "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '0', NULL)"
        }
        _ => unreachable!(),
    };
    conn.prepare_cached(insert_sql)?
        .execute(params![user_id, asset_id])?;
    Ok(conn.last_insert_rowid())
}
impl DegenbotDb {
    /// The scaled-token apply's compute+write arm with the CURRENT balance
    /// state SUPPLIED by the caller (the chunk substrate cache serves the
    /// read — the cache overlay guarantees the supplied state is exactly what
    /// the SQL probe would have returned). Runs the shared clamp/max-with-prev
    /// math, writes the row, and returns the new `(balance, last_index)` so
    /// the caller can record it (the overlay's read-your-own-writes
    /// backbone).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on the UPDATE failure, or
    /// [`DbError::Decode`] if the computed new balance overflows `U256`.
    pub fn apply_scaled_token_balance_delta_with_current_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        current_balance: alloy::primitives::U256,
        current_index: Option<alloy::primitives::U256>,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(alloy::primitives::U256, Option<alloy::primitives::U256>), DbError> {
        let table = position.table();
        let (new_balance, new_last_index) = finish_scaled_token_balance_state(
            table,
            position_id,
            current_balance,
            current_index,
            balance_delta,
            new_index,
        )?;
        write_scaled_token_balance_state(conn, table, position_id, new_balance, new_last_index)?;
        Ok((new_balance, new_last_index))
    }

    conn_writer! {
        /// Get-or-create an `aave_v3_collateral_positions` row by `(user_id,
        /// asset_id)`. Port of `db_positions.py::get_or_create_collateral_position`.
        /// On create, inserts `balance='0'`, `last_index=NULL` (the Python
        /// `AaveV3CollateralPosition` defaults). Returns the row `id`.
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure.
        fn get_or_create_collateral_position, get_or_create_collateral_position_on_conn(
            conn: &rusqlite::Connection,
            user_id: i64,
            asset_id: i64,
        ) -> Result<i64, DbError> {
            get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_collateral_positions")
        }
    }

    conn_writer! {
        /// Get-or-create an `aave_v3_debt_positions` row by `(user_id, asset_id)`.
        /// Port of `db_positions.py::get_or_create_debt_position`. On create,
        /// inserts `balance='0'`, `last_index=NULL`. Returns the row `id`.
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure.
        fn get_or_create_debt_position, get_or_create_debt_position_on_conn(
            conn: &rusqlite::Connection,
            user_id: i64,
            asset_id: i64,
        ) -> Result<i64, DbError> {
            get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_debt_positions")
        }
    }

    // ── position-row lookups (`&Connection`-bound) ─────────────

    /// Read a position's current `balance` + `last_index`. Used by the GHO
    /// apply dispatch (`UnifiedGhoProcessor::accrue_debt_on_action` +
    /// `get_discounted_balance` need the position's prev balance/index to
    /// compute the discount-scaled amount): the position's stored `balance` and
    /// `last_index`, read in one round-trip.
    ///
    /// Returns `(balance, None)` for a position whose `last_index` is NULL
    /// (a fresh position). Returns [`DbError::MissingRow`] if the position id
    /// has no row.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] if no position matches `position_id`,
    /// or [`DbError::Decode`] on a malformed persisted balance string.
    pub fn lookup_position_balance_index_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<(alloy::primitives::U256, Option<alloy::primitives::U256>), DbError> {
        let sql = match position.table() {
            "aave_v3_collateral_positions" => {
                "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
            }
            "aave_v3_debt_positions" => {
                "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
            }
            _ => unreachable!("bad table"),
        };
        let row: (String, Option<String>) = conn
            .prepare_cached(sql)?
            .query_row(rusqlite::params![position_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    DbError::MissingRow(format!("{} id={position_id}", position.table()))
                }
                e => DbError::Sqlite(e),
            })?;
        let balance = parse_decimal_u256(&row.0)?;
        let last_index = row.1.as_deref().and_then(|s| parse_decimal_u256(s).ok());
        Ok((balance, last_index))
    }

    /// Fetch a debt position's discount-refresh context (`user_id`,
    /// `user_address`, `scaled_balance`, `last_index`, `stk_aave_balance`) in one
    /// joined read. The refresh reads the post-apply `balance`/`last_index` (the
    /// `ScaledTokenMint`/`Burn` apply landed them just before this call). Mirrors
    /// Python's `_refresh_discount_rate` reading `debt_position.balance` /
    /// `.last_index` + `user.address` / `.stk_aave_balance`.
    #[expect(clippy::missing_errors_doc)]
    pub fn lookup_debt_position_refresh_context_on_conn(
        conn: &rusqlite::Connection,
        position_id: i64,
    ) -> Result<DebtPositionRefreshContext, DbError> {
        let row: (String, Option<String>, i64, String, Option<String>) = conn
            .query_row(
                "SELECT dp.balance, dp.last_index, dp.user_id, u.address, \
                 u.stk_aave_balance \
                 FROM aave_v3_debt_positions dp \
                 JOIN aave_v3_users u ON u.id = dp.user_id \
                 WHERE dp.id = ?1",
                rusqlite::params![position_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    DbError::MissingRow(format!("aave_v3_debt_positions id={position_id}"))
                }
                e => DbError::Sqlite(e),
            })?;
        Ok(DebtPositionRefreshContext {
            user_id: row.2,
            user_address: row.3,
            scaled_balance: parse_decimal_u256(&row.0)?,
            last_index: row.1.as_deref().and_then(|s| parse_decimal_u256(s).ok()),
            stk_aave_balance: row.4.as_deref().and_then(|s| parse_decimal_u256(s).ok()),
        })
    }

    /// Delete every zero-balance collateral + debt position owned by the
    /// market's users (the ported Python `cleanup_zero_balance_positions`
    /// from `cli/aave/verification.py`). The aave updater runs this at the
    /// END of each chunk's transaction, before the `last_update_block` stamp,
    /// so burned-down positions do not accumulate as permanent `'0'` rows.
    /// The table names are compile-time literals (the same two tables every
    /// position fn above names) - no injection surface.
    ///
    /// Returns the number of rows deleted (collateral + debt combined).
    ///
    /// # Errors
    ///
    /// [`DbError::Sqlite`] on a DELETE failure.
    pub fn delete_zero_balance_positions_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
    ) -> Result<usize, DbError> {
        let mut deleted = 0;
        for table in ["aave_v3_collateral_positions", "aave_v3_debt_positions"] {
            let sql = format!(
                "DELETE FROM {table} \
                 WHERE id IN ( \
                     SELECT p.id FROM {table} p \
                     JOIN aave_v3_users u ON u.id = p.user_id \
                     WHERE u.market_id = ?1 \
                       AND (p.balance = '0' OR p.balance = 0) \
                 )"
            );
            deleted += conn.execute(&sql, params![market_id])?;
        }
        Ok(deleted)
    }

    /// Reset a debt position's balance to 0 + advance `last_index` (the
    /// bad-debt liquidation path): zero the balance and advance `last_index`
    /// only when the event's index is greater. The
    /// contract burns the ENTIRE remaining debt when a `DeficitCreated`
    /// accompanies a `LiquidationCall`; the delta-based apply may be off by 1
    /// wei, so this reset is the faithful path.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] if `position_id` has no row, or
    /// [`DbError::Sqlite`] on an UPDATE failure.
    pub fn reset_debt_position_to_zero_on_conn(
        conn: &rusqlite::Connection,
        position_id: i64,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        // Read the current last_index (max-with-prev reconciliation — mirrors
        // the Python's `if scaled_event.index > current_index` guard).
        let current_index: Option<String> = conn
            .prepare_cached("SELECT last_index FROM aave_v3_debt_positions WHERE id = ?1")?
            .query_row(rusqlite::params![position_id], |r| r.get(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::MissingRow(format!(
                    "aave_v3_debt_positions id={position_id} (reset target)"
                )),
                e => DbError::Sqlite(e),
            })?;
        let current_index_u256 = current_index
            .as_deref()
            .and_then(|s| parse_decimal_u256(s).ok());
        Self::reset_debt_position_to_zero_with_current_on_conn(
            conn,
            position_id,
            current_index_u256,
            new_index,
        )?;
        Ok(())
    }

    /// The bad-debt reset's compute+write arm with the CURRENT `last_index`
    /// SUPPLIED by the caller (the chunk substrate cache serves the read —
    /// the overlay guarantees the supplied state is exactly what the SQL
    /// probe would have returned). Returns the written `last_index` so the
    /// caller can record it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on the UPDATE failure.
    pub fn reset_debt_position_to_zero_with_current_on_conn(
        conn: &rusqlite::Connection,
        position_id: i64,
        current_index: Option<alloy::primitives::U256>,
        new_index: alloy::primitives::U256,
    ) -> Result<Option<alloy::primitives::U256>, DbError> {
        let new_last_index = match current_index {
            Some(cur) if new_index > cur => Some(new_index),
            Some(_) => current_index, // keep the prior higher index
            None => Some(new_index),  // first event: set it
        };
        conn.prepare_cached(
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3",
        )?
        .execute(rusqlite::params![
            "0",
            new_last_index.map(|i| i.to_string()),
            position_id,
        ])?;
        Ok(new_last_index)
    }
}
