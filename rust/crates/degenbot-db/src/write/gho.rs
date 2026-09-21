//! GHO discount-token writes + stkAAVE / rewards event applies.

use super::util::parse_decimal_u256;
use super::{params, DbError, DegenbotDb, OptionalExtension};

/// Look up an `aave_gho_tokens.id` by the underlying GHO token's `(chain, address)`
/// joins through `erc20_tokens` (the `aave_gho_tokens` table is keyed by
/// `token_id`, not by address).
fn existing_gho_token(
    conn: &rusqlite::Connection,
    chain_id: i64,
    token_address: &str,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls.
    let mut s = conn.prepare_cached(
        "SELECT g.id FROM aave_gho_tokens g
         JOIN erc20_tokens t ON t.id = g.token_id
         WHERE t.chain = ?1 AND t.address = ?2",
    )?;
    Ok(s.query_row(params![chain_id, token_address], |r| r.get(0))
        .optional()?)
}
impl DegenbotDb {
    // ── GHO / stkAAVE / Rewards apply fns ──────────────────────────

    /// Apply a GHO `DiscountPercentUpdated` event: set the user's
    /// `gho_discount` column (an `INTEGER` percentage; the Python path stores
    /// the raw uint256 but the Aave protocol caps discount at 100% so `i64` is
    /// the correct Rust type). Port of
    /// `event_handlers._process_discount_percent_updated_event` (L1167).
    ///
    /// `user_id` is pre-resolved by the orchestrator/parser (the Python calls
    /// `get_or_create_user` upstream; the apply fn takes the resolved id — the
    /// precedent).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no user matches `user_id`.
    pub fn apply_gho_discount_percent_updated_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        new_discount_percent: i64,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_v3_users SET gho_discount = ?1 WHERE id = ?2",
            params![new_discount_percent, user_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (DiscountPercentUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_percent_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_percent_updated(
        &self,
        user_id: i64,
        new_discount_percent: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_percent_updated_on_conn(&conn, user_id, new_discount_percent)
    }

    /// Apply a GHO `DiscountRateStrategyUpdated` event: set the GHO token's
    /// `v_gho_discount_rate_strategy` column (a checksummed address or
    /// `NULL`). Port of
    /// `event_handlers._process_discount_rate_strategy_updated_event` (L752).
    ///
    /// `gho_token_id` is the `aave_gho_tokens.id` (NOT the erc20 token id —
    /// the GHO token row is chain-unique, identified by its primary key).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
    /// `gho_token_id`.
    pub fn apply_gho_discount_rate_strategy_updated_on_conn(
        conn: &rusqlite::Connection,
        gho_token_id: i64,
        new_strategy: Option<&str>,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_gho_tokens SET v_gho_discount_rate_strategy = ?1 WHERE id = ?2",
            params![new_strategy, gho_token_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_gho_tokens id={gho_token_id} (DiscountRateStrategyUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_rate_strategy_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_rate_strategy_updated(
        &self,
        gho_token_id: i64,
        new_strategy: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_rate_strategy_updated_on_conn(&conn, gho_token_id, new_strategy)
    }

    /// Apply a GHO `DiscountTokenUpdated` event: set the GHO token's
    /// `v_gho_discount_token` column (a checksummed address or `NULL`). Port
    /// of `event_handlers._process_discount_token_updated_event` (L717).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
    /// `gho_token_id`.
    pub fn apply_gho_discount_token_updated_on_conn(
        conn: &rusqlite::Connection,
        gho_token_id: i64,
        new_discount_token: Option<&str>,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_gho_tokens SET v_gho_discount_token = ?1 WHERE id = ?2",
            params![new_discount_token, gho_token_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_gho_tokens id={gho_token_id} (DiscountTokenUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_token_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_token_updated(
        &self,
        gho_token_id: i64,
        new_discount_token: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_token_updated_on_conn(&conn, gho_token_id, new_discount_token)
    }

    /// Apply a stkAAVE `Transfer(from, to, value)` event. Port of
    /// `stkaave.process_stk_aave_transfer_event` — processes EVERY Transfer
    /// leg (including both zero-leg arms: `Transfer(0→X)` mint + `Transfer(X→0)`
    /// burn) via `Option<i64>` `user_ids`. The `None` side (the `ZERO_ADDRESS` leg)
    /// is skipped entirely; only the `Some` side is mutated. The degenerate
    /// `Transfer(0→0)` case lands both `None` and applies nothing.
    ///
    /// YMWN5V retirement (crash #3): the prior design dedupe-skipped the
    /// zero-leg here + processed the paired Staked/Redeem via separate apply
    /// fns. The empirical reality (verified via cast logs) is that some
    /// actions emit ONLY the `Transfer(X→0)` event with NO paired Redeem;
    /// skipping the zero-leg left the sender's `stk_aave_balance` stuck at
    /// the pre-burn cache value → wrong `calculate_gho_discount_rate` →
    /// GHO-burn delta overshoots the scaled balance → crash (byte-exact
    /// match: overshoot == `discount_scaled` at stale `prev_discount`).
    ///
    /// The `from`-leg underflow guard (value > from's balance) errors —
    /// mirrors the Python `assert from_user.stk_aave_balance >= 0` (the Python
    /// asserts before applying; the Rust returns [`DbError::Decode`] instead
    /// of panicking). The `to`-leg overflow guard errors (Python's
    /// `checked_add` semantics — the stdlib `+=` would panic on overflow, the
    /// Rust guards it explicitly). Both writes happen in the caller's
    /// `Transaction` (atomic with the chunk).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, [`DbError::MissingRow`]
    /// if a `Some(user_id)` matches no row, or [`DbError::Decode`] if a
    /// persisted string is malformed, the `from`-leg underflows, or the
    /// `to`-leg overflows.
    pub fn apply_stk_aave_transfer_on_conn(
        conn: &rusqlite::Connection,
        from_user_id: Option<i64>,
        to_user_id: Option<i64>,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        // The from-leg (decrement) — skipped when None (the mint-from-zero arm).
        if let Some(uid) = from_user_id {
            let from_str: Option<Option<String>> = conn
                .query_row(
                    "SELECT stk_aave_balance FROM aave_v3_users WHERE id = ?1",
                    [uid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?;
            let Some(balance_str) = from_str else {
                return Err(DbError::MissingRow(format!(
                    "aave_v3_users id={uid} (StkAaveTransfer from-leg target)"
                )));
            };
            let balance = balance_str
                .as_deref()
                .map(parse_decimal_u256)
                .transpose()?
                .unwrap_or(alloy::primitives::U256::ZERO);
            let new_balance = balance.checked_sub(amount).ok_or_else(|| {
                DbError::Decode(format!(
                    "stk_aave_balance underflow on Transfer from-leg: {balance} - {amount} (user_id={uid})"
                ))
            })?;
            let updated = conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = ?1 WHERE id = ?2",
                params![new_balance.to_string(), uid],
            )?;
            if updated == 0 {
                return Err(DbError::MissingRow(format!(
                    "aave_v3_users id={uid} (StkAaveTransfer from-leg — row vanished mid-tx)"
                )));
            }
        }

        // The to-leg (increment) — skipped when None (the burn-to-zero arm).
        if let Some(uid) = to_user_id {
            let to_str: Option<Option<String>> = conn
                .query_row(
                    "SELECT stk_aave_balance FROM aave_v3_users WHERE id = ?1",
                    [uid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?;
            let Some(balance_str) = to_str else {
                return Err(DbError::MissingRow(format!(
                    "aave_v3_users id={uid} (StkAaveTransfer to-leg target)"
                )));
            };
            let balance = balance_str
                .as_deref()
                .map(parse_decimal_u256)
                .transpose()?
                .unwrap_or(alloy::primitives::U256::ZERO);
            let new_balance = balance.checked_add(amount).ok_or_else(|| {
                DbError::Decode(format!(
                    "stk_aave_balance overflow on Transfer to-leg: {balance} + {amount} (user_id={uid})"
                ))
            })?;
            let updated = conn.execute(
                "UPDATE aave_v3_users SET stk_aave_balance = ?1 WHERE id = ?2",
                params![new_balance.to_string(), uid],
            )?;
            if updated == 0 {
                return Err(DbError::MissingRow(format!(
                    "aave_v3_users id={uid} (StkAaveTransfer to-leg — row vanished mid-tx)"
                )));
            }
        }

        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_stk_aave_transfer_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_stk_aave_transfer(
        &self,
        from_user_id: Option<i64>,
        to_user_id: Option<i64>,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_stk_aave_transfer_on_conn(&conn, from_user_id, to_user_id, amount)
    }

    /// Apply a `RewardsController` `RewardsClaimed` event. **No-op** —
    /// investigation (RYKCC4, 2026-07-04) confirmed the Python `event_handlers.py`
    /// defines `AaveV3RewardsControllerEvent.REWARDS_CLAIMED` in
    /// `src/degenbot/aave/events.py` but has NO handler for it: rewards claims
    /// surface only via the stkAAVE token's `Transfer` events (see
    /// `transaction_processor.py:249` — "This handles cases where stkAAVE
    /// transfers (e.g., rewards claims) occur"). The DB has no rewards table.
    /// The variant exists in `AaveChunkEvent` so the parser can route the
    /// event through the chunk pipeline (for operation-classification + log
    /// accounting), but the apply dispatch records the count + writes nothing.
    ///
    /// # Errors
    ///
    /// Never errors — the no-op always returns `Ok(())`.
    pub fn apply_rewards_claimed_on_conn(
        _conn: &rusqlite::Connection,
        _user_id: i64,
        _reward_token_id: i64,
        _claimer_id: i64,
        _claimed_amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_rewards_claimed_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant (never errors).
    pub fn apply_rewards_claimed(
        &self,
        user_id: i64,
        reward_token_id: i64,
        claimer_id: i64,
        claimed_amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_rewards_claimed_on_conn(
            &conn,
            user_id,
            reward_token_id,
            claimer_id,
            claimed_amount,
        )
    }

    // ── HQF5NQ-A substrate lookups (the parser's address→id resolution) ──
    //
    // Four lookups the parser needs (each verified non-existent via
    // `grep "pub fn get_or_create\|pub fn lookup" write.rs` before being
    // added — Finding 2 of HQF5NQ's BLOCKED-FOR-SPLIT-DECISION). Each mirrors
    // the Python `operations_parser.py::_get_*` helpers in shape — `&Connection`
    // for the chunk-tx §3.4 invariant (one `Transaction` per chunk).

    /// Get-or-create an `aave_gho_tokens` row by `(chain_id, token_address)`.
    /// Port of the GHO-token resolution the parser + apply glue uses to
    /// resolve the `gho_token_id` parameter the GHO apply fns take (RYKCC4
    /// flag #6 follow-up; the apply fns `apply_gho_discount_rate_strategy_updated_on_conn`
    /// / `apply_gho_discount_token_updated_on_conn` require a pre-resolved
    /// `gho_token_id`).
    ///
    /// On create, inserts a bare row (`token_id` resolved via
    /// [`Self::get_or_create_erc20_token_on_conn`]; `v_token_id` left `NULL`; the
    /// `v_gho_discount_rate_strategy` / `v_gho_discount_token` columns are
    /// filled later by their apply fns). On an existing row, returns the
    /// `id` (no UPSERT — the columns are managed by the GHO-config apply
    /// fns).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_gho_token_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        token_address: &str,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_gho_token(conn, chain_id, token_address)? {
            return Ok(id);
        }
        // Resolve the erc20_tokens row id (the FK) — the caller passes the
        // address; we look up / insert the token-row first.
        let token_id = Self::get_or_create_erc20_token_on_conn(
            conn,
            chain_id,
            token_address,
            None,
            None,
            None,
        )?;
        conn.execute(
            "INSERT INTO aave_gho_tokens (token_id) VALUES (?1)",
            params![token_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// The `&self` wrapper for [`Self::get_or_create_gho_token_on_conn`].
    /// # Errors
    ///
    /// Same error conditions as the `_on_conn` variant.
    pub fn get_or_create_gho_token(
        &self,
        chain_id: i64,
        token_address: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_gho_token_on_conn(&conn, chain_id, token_address)
    }

    /// C3.3 (the (C) refresh) — SET (not increment) the user's
    /// `stk_aave_balance` to the `balanceOf` RPC result. Used when
    /// `stk_aave_balance` is None (the user was never touched by a stkAAVE
    /// `Staked`/`Transfer` event yet — the 890 None + the 1042 missing —
    /// `get_or_init_stk_aave_balance`). Mirrors Python's
    /// `user.stk_aave_balance = balance`.
    #[expect(clippy::missing_errors_doc)]
    pub fn set_user_stk_aave_balance_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        balance: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        conn.execute(
            "UPDATE aave_v3_users SET stk_aave_balance = ?1 WHERE id = ?2",
            rusqlite::params![balance.to_string(), user_id],
        )
        .map_err(DbError::Sqlite)?;
        Ok(())
    }
}
