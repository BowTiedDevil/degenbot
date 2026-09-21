//! Aave position get-or-create + scaled-token applies + position lookups.

use super::util::parse_decimal_u256;
use super::{params, DbError, DegenbotDb, OptionalExtension, U256};

// ── the upsert substrate (`get_or_create_*`) ──────────────────────────────

/// C3.3 (the (C) discount-refresh pass) — the post-apply context the refresh
/// reads: the GHO debt position's `scaled_balance`/`last_index` (POST-apply —
/// the `ScaledTokenMint`/`Burn` apply landed them) + the user's `address` +
/// `stk_aave_balance`. Built by [`DegenbotDb::lookup_debt_position_refresh_context_on_conn`].
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

/// Which position table a `ScaledToken` event applies to (mirrors the
/// `CollateralMintEvent`/`DebtMintEvent` dispatch in the Python `_process_
/// scaled_token_operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaledTokenPosition {
    /// `aave_v3_collateral_positions` (aToken Mint/Burn/BalanceTransfer).
    Collateral,
    /// `aave_v3_debt_positions` (vToken Mint/Burn).
    Debt,
}

impl ScaledTokenPosition {
    /// The literal table name (compile-time-dispatched; no injection surface).
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Collateral => "aave_v3_collateral_positions",
            Self::Debt => "aave_v3_debt_positions",
        }
    }

    /// The literal `balance`/`last_index` column pair (identical across both
    /// tables — `(balance VARCHAR(78) NOT NULL, last_index VARCHAR(78))`).
    #[must_use]
    pub const fn balance_col(self) -> &'static str {
        "balance"
    }

    #[must_use]
    pub const fn last_index_col(self) -> &'static str {
        "last_index"
    }
}

/// The shared `mint`/`burn` apply body (both are signed deltas). Reads the
/// position's current `balance` + `last_index`, adds the signed
/// `balance_delta`, conditionally advances `last_index`, writes both back.
///
/// # `last_index` reconciliation
///
/// Advances `last_index` to `new_index` ONLY when `new_index >
/// COALESCE(current_last_index, 0)` — mirrors the Python
/// `if mint_result.new_index > (position.last_index or 0) { position.last_index
/// = mint_result.new_index }`. The max-with-prev guard prevents an out-of-log-
/// order event from clobbering a newer event's index.
///
/// # Errors
///
/// Returns [`DbError::MissingRow`] if no position matches `position_id`.
fn apply_scaled_token_balance_delta_on_conn(
    conn: &rusqlite::Connection,
    position: ScaledTokenPosition,
    position_id: i64,
    balance_delta: alloy::primitives::I256,
    new_index: alloy::primitives::U256,
) -> Result<(), DbError> {
    let table = position.table();
    let sql = match table {
        "aave_v3_collateral_positions" => {
            "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
        }
        "aave_v3_debt_positions" => {
            "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
        }
        _ => unreachable!("ScaledTokenPosition: bad table {table:?}"),
    };
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(sql, params![position_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    let Some((balance_str, last_index_str)) = row else {
        return Err(DbError::MissingRow(format!(
            "{table} id={position_id} (ScaledToken apply target)"
        )));
    };
    let current_balance =
        parse_decimal_u256(&balance_str.ok_or_else(|| {
            DbError::Decode(format!("{table} id={position_id}: balance is NULL"))
        })?)?;
    let current_index = match last_index_str {
        Some(s) => Some(parse_decimal_u256(&s)?),
        None => None,
    };
    // Compute new balance (I256 arithmetic — delta may be negative).
    let current_balance_i =
        alloy::primitives::I256::try_from(current_balance).unwrap_or(alloy::primitives::I256::MAX);
    let new_balance_i = current_balance_i + balance_delta;
    // Aave V3 `_burnScaled` cap-to-scaledBalance clamp (crash #11 family).
    // The on-chain `_burnScaled` computes `amountScaled =
    // amountTotal.rayDiv(index)` then `if (amountScaled > scaledBalance)
    // amountScaled = scaledBalance;` before burning — the contract NEVER
    // decrements the position below zero. When Rust's `scaled_amount`
    // (carried as the burn's `balance_delta`) exceeds the live `current_balance`
    // (because of accumulated 1-2 wei drift between Rust's stored balance and
    // the on-chain `scaledBalance`), the unguarded `current_balance_i +
    // balance_delta` goes negative and would crash the chunk with
    // `balance would go negative`. Mirroring the contract, clamp the new
    // balance to ZERO instead of erroring. Symmetric with the GHO-side fix
    // in `process_gho_debt_burn`'s `>=` branch (commit a419f87f — crash #11).
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
    // last_index reconciliation — max-with-prev (mirrors the Python guard).
    let new_last_index = match current_index {
        Some(cur) if new_index > cur => Some(new_index),
        Some(_) => current_index, // keep the prior higher index
        None => Some(new_index),  // first event: set it
    };
    let update_sql = match table {
        "aave_v3_collateral_positions" => {
            "UPDATE aave_v3_collateral_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        "aave_v3_debt_positions" => {
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        _ => unreachable!(),
    };
    conn.execute(
        update_sql,
        params![
            new_balance.to_string(),
            new_last_index.map(|i| i.to_string()),
            position_id,
        ],
    )?;
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
        .query_row::<i64, _, _>(sql, params![user_id, asset_id], |r| r.get(0))
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
    conn.execute(insert_sql, params![user_id, asset_id])?;
    Ok(conn.last_insert_rowid())
}
impl DegenbotDb {
    /// Get-or-create an `aave_v3_collateral_positions` row by `(user_id,
    /// asset_id)`. Port of `db_positions.py::get_or_create_collateral_position`
    /// (L51–…). On create, inserts `balance='0'`, `last_index=NULL` (the
    /// Python `AaveV3CollateralPosition` defaults). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_collateral_position(
        &self,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_collateral_position_on_conn(&conn, user_id, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_collateral_position`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_collateral_position_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_collateral_positions")
    }

    /// Get-or-create an `aave_v3_debt_positions` row by `(user_id, asset_id)`.
    /// Port of `db_positions.py::get_or_create_debt_position` (L73–…). On
    /// create, inserts `balance='0'`, `last_index=NULL`. Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_debt_position(&self, user_id: i64, asset_id: i64) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_debt_position_on_conn(&conn, user_id, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_debt_position`] (the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_debt_position_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_debt_positions")
    }

    // ── ScaledToken position-state mutations (5Z3QQ2 — SCALEAPPLY) ────────
    //
    // The aToken/vToken `Mint`/`Burn`/`BalanceTransfer` events drive the
    // `balance` + `last_index` columns on `aave_v3_collateral_positions` /
    // `aave_v3_debt_positions`. These apply fns are the pure substrate: they
    // take a PRE-COMPUTED signed `balance_delta` (the revision-aware
    // `process_mint_event`/`process_burn_event` math in
    // `degenbot-aave::processors` computed it from the event's
    // `value`/`balance_increase`/`index`/`scaled_amount` + the token
    // revision's rounding strategy) + the new index, then mutate the row.
    //
    // The `last_index` reconciliation mirrors the Python
    // `token_processor._process_scaled_token_operation` L91/L109/L131/L148:
    // `position.last_index = max(position.last_index or 0, new_index)` — only
    // advance last_index when the event's index is strictly greater (so
    // out-of-log-order events don't clobber a later event's index).

    /// Apply a `ScaledToken` `Mint` event's pre-computed `balance_delta` +
    /// `new_index` to a collateral or debt position. Port of
    /// `token_processor._process_scaled_token_operation`'s
    /// `CollateralMintEvent`/`DebtMintEvent` arm (the `position.balance +=
    /// mint_result.balance_delta` + the `last_index` reconciliation).
    ///
    /// `balance_delta` is signed: positive for a true mint (deposit/borrow),
    /// negative for the interest-exceeds-withdrawal edge case (the Python
    /// `process_mint_event` returns a negative delta when `balance_increase >
    /// value`, treating the Mint as an effective burn). The apply fn is the
    /// same shape as [`Self::apply_scaled_token_burn_on_conn`] — the only
    /// difference is the caller's semantic (mint vs burn); the math (read
    /// balance, add delta, write back, conditionally advance `last_index`) is
    /// identical.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::Decode`] if the persisted `balance`/`last_index` string is
    /// malformed, or [`DbError::MissingRow`] if no position matches
    /// `position_id`.
    pub fn apply_scaled_token_mint_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        apply_scaled_token_balance_delta_on_conn(
            conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// Apply a `ScaledToken` `Burn` event's pre-computed `balance_delta` +
    /// `new_index` to a collateral or debt position. Port of
    /// `token_processor._process_scaled_token_operation`'s
    /// `CollateralBurnEvent`/`DebtBurnEvent` arm. `balance_delta` is negative
    /// (a burn decrements the scaled balance); the apply math is shared with
    /// [`Self::apply_scaled_token_mint_on_conn`] (both are signed deltas).
    ///
    /// # Errors
    ///
    /// Same as [`Self::apply_scaled_token_mint_on_conn`].
    pub fn apply_scaled_token_burn_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        apply_scaled_token_balance_delta_on_conn(
            conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// Apply a `ScaledToken` `BalanceTransfer` event: debit `from_position_id`'s
    /// balance by `scaled_amount`, credit `to_position_id` by the same, +
    /// advance both positions' `last_index` to `transfer_index` (the transfer
    /// carries its own index; both positions reconcile to it). Port of
    /// `transfers._process_collateral_transfer` (the sender's `last_index =
    /// max(prev, transfer_index)` + the recipient's `last_index =
    /// transfer_index` unconditional set).
    ///
    /// `BalanceTransfer` is aToken-only (collateral positions); there's no
    /// vToken `BalanceTransfer` in Aave V3 (debt transfers are ERC20 Transfer +
    /// paired Burn, not `BalanceTransfer`). So both positions are collateral.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (either position's
    /// SELECT/UPDATE), or [`DbError::MissingRow`] if either position id has no
    /// row, or [`DbError::Decode`] on a malformed persisted balance string.
    pub fn apply_scaled_token_transfer_on_conn(
        conn: &rusqlite::Connection,
        from_position_id: i64,
        to_position_id: Option<i64>,
        scaled_amount: alloy::primitives::U256,
        transfer_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        // Debit the sender (negative delta). On existing positions, the
        // Python mutates `sender_position.balance -= scaled_amount` then
        // `sender_position.last_index = max(prev, transfer_index)`.
        apply_scaled_token_balance_delta_on_conn(
            conn,
            ScaledTokenPosition::Collateral,
            from_position_id,
            -alloy::primitives::I256::try_from(scaled_amount)
                .unwrap_or(alloy::primitives::I256::MIN),
            transfer_index,
        )?;
        // Credit the recipient (positive delta) — SKIPPED when the recipient
        // address is `ZERO_ADDRESS` (the `Transfer(from=user, to=0x0, amount)`
        // burn-to-zero leg). Mirrors Python's `transfers.py:
        // _process_collateral_transfer` recipient guard (`if
        // scaled_event.target_address != ZERO_ADDRESS:` — the recipient
        // block is gated on !=0x0; delete dial-back was landing
        // extra 0-address collateral rows in Rust otherwise, surfacing as
        // crash #8 on the 21.9M→22M march checkpoint). The dispatcher passes
        // `None` for the position id when the recipient is 0x0.
        if let Some(to_position_id) = to_position_id {
            // The Python sets `recipient_position.last_index = transfer_index`
            // UNCONDITIONALLY (not max-with-prev) — the recipient's index
            // becomes the transfer's index. Reuse the shared helper which
            // does max-with-prev; the difference is benign because a fresh
            // recipient position's `last_index` is NULL (treated as 0), so
            // max(0, transfer_index) = transfer_index matches the
            // unconditional set. For an existing recipient (a re-transfer
            // into a non-empty position), max-with-prev is SAFER (avoids
            // clobbering a higher index from a prior transfer); the
            // Python's unconditional set is a latent bug only visible if a
            // prior transfer into the same recipient had a higher index,
            // which can't happen within a single chunk's monotonic block order.
            apply_scaled_token_balance_delta_on_conn(
                conn,
                ScaledTokenPosition::Collateral,
                to_position_id,
                alloy::primitives::I256::try_from(scaled_amount)
                    .unwrap_or(alloy::primitives::I256::MAX),
                transfer_index,
            )?;
        }
        Ok(())
    }

    // ── `&self` wrappers (2QPBUJ path: thin lock-and-delegate) ─────────────

    /// `&self` wrapper for [`Self::apply_scaled_token_mint_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_mint(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_mint_on_conn(
            &conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// `&self` wrapper for [`Self::apply_scaled_token_burn_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_burn(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_burn_on_conn(
            &conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// `&self` wrapper for [`Self::apply_scaled_token_transfer_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_transfer(
        &self,
        from_position_id: i64,
        to_position_id: Option<i64>,
        scaled_amount: alloy::primitives::U256,
        transfer_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_transfer_on_conn(
            &conn,
            from_position_id,
            to_position_id,
            scaled_amount,
            transfer_index,
        )
    }

    /// Resolve the collateral (aToken) or debt (vToken) position-row id by
    /// `(user_id, asset_id, position_type)`. Mirrors the parser's need to
    /// look up the `BalanceTransfer` recipient's collateral position for the
    /// `DEFICIT_COVERAGE` triplet. `position_type` is `'collateral'` or `'debt'`
    /// (the parser / dispatch glue selects the table).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_position_by_user_asset_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
        position_type: &str,
    ) -> Result<Option<i64>, DbError> {
        let (table, fk) = match position_type {
            "collateral" => ("aave_v3_collateral_positions", "asset_id"),
            "debt" => ("aave_v3_debt_positions", "asset_id"),
            _ => {
                return Err(DbError::Decode(format!(
                    "unexpected position_type '{position_type}' (expected 'collateral' or 'debt')"
                )));
            }
        };
        let sql = format!("SELECT id FROM {table} WHERE user_id = ?1 AND {fk} = ?2");
        Ok(conn
            .query_row(&sql, rusqlite::params![user_id, asset_id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?)
    }

    /// Read a position's current `balance` + `last_index`. Used by the GHO
    /// apply dispatch (C3 — `UnifiedGhoProcessor::accrue_debt_on_action` +
    /// `get_discounted_balance` need the position's prev balance/index to
    /// compute the discount-scaled amount). Mirrors the Python's read of
    /// `debt_position.balance` / `.last_index` on the ORM-tracked position.
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
            .query_row(sql, rusqlite::params![position_id], |r| {
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

    /// C3.3 (the (C) discount-refresh post-apply pass) — fetch a debt
    /// position's refresh context (`user_id`, `user_address`, `scaled_balance`,
    /// `last_index`, `stk_aave_balance`) in one joined read. The refresh reads the
    /// post-apply `balance`/`last_index` (the `ScaledTokenMint`/`Burn` apply
    /// landed them just before this call). Mirrors Python's
    /// `_refresh_discount_rate` reading `debt_position.balance` / `.last_index`
    /// + `user.address` / `.stk_aave_balance`.
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
    /// bad-debt liquidation path — C3). Mirrors the Python's
    /// `debt_position.balance = 0` + `if index > current_index: last_index =
    /// index` guard in `_process_debt_burn_with_match`'s bad-debt arm. The
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
            .query_row(
                "SELECT last_index FROM aave_v3_debt_positions WHERE id = ?1",
                rusqlite::params![position_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::MissingRow(format!(
                    "aave_v3_debt_positions id={position_id} (reset target)"
                )),
                e => DbError::Sqlite(e),
            })?;
        let current_index_u256 = current_index
            .as_deref()
            .and_then(|s| parse_decimal_u256(s).ok());
        let new_last_index = match current_index_u256 {
            Some(cur) if new_index > cur => Some(new_index),
            Some(_) => current_index_u256, // keep the prior higher index
            None => Some(new_index),       // first event: set it
        };
        conn.execute(
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3",
            rusqlite::params!["0", new_last_index.map(|i| i.to_string()), position_id],
        )?;
        Ok(())
    }
}
