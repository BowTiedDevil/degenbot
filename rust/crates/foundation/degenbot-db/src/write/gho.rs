//! GHO discount-token writes + stkAAVE / rewards event applies.

use super::util::{existing_row_id, parse_decimal_u256};
use super::{params, DbError, DegenbotDb, OptionalExtension};

/// Look up an `aave_gho_tokens.id` by the underlying GHO token's `(chain, address)`
/// joins through `erc20_tokens` (the `aave_gho_tokens` table is keyed by
/// `token_id`, not by address).
fn existing_gho_token(
    conn: &rusqlite::Connection,
    chain_id: i64,
    token_address: &str,
) -> Result<Option<i64>, DbError> {
    existing_row_id(
        conn,
        "SELECT g.id FROM aave_gho_tokens g
         JOIN erc20_tokens t ON t.id = g.token_id
         WHERE t.chain = ?1 AND t.address = ?2",
        params![chain_id, token_address],
    )
}
impl DegenbotDb {
    // ── GHO / stkAAVE / Rewards apply fns ──────────────────────────

    conn_writer! {
        /// Apply a GHO `DiscountPercentUpdated` event: set the user's
        /// `gho_discount` column (an `INTEGER` percentage; the Python path stores
        /// the raw uint256 but the Aave protocol caps discount at 100% so `i64` is
        /// the correct Rust type).
        ///
        /// `user_id` is pre-resolved by the orchestrator/parser (the Python calls
        /// `get_or_create_user` upstream; the apply fn takes the resolved id — the
        /// precedent).
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure, or
        /// [`DbError::MissingRow`] if no user matches `user_id`.
        fn apply_gho_discount_percent_updated, apply_gho_discount_percent_updated_on_conn(
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
    }

    conn_writer! {
        /// Apply a GHO `DiscountRateStrategyUpdated` event: set the GHO token's
        /// `v_gho_discount_rate_strategy` column (a checksummed address or
        /// `NULL`).
        ///
        /// `gho_token_id` is the `aave_gho_tokens.id` (NOT the erc20 token id —
        /// the GHO token row is chain-unique, identified by its primary key).
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure, or
        /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
        /// `gho_token_id`.
        fn apply_gho_discount_rate_strategy_updated,
            apply_gho_discount_rate_strategy_updated_on_conn(
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
    }

    conn_writer! {
        /// Apply a GHO `DiscountTokenUpdated` event: set the GHO token's
        /// `v_gho_discount_token` column (a checksummed address or `NULL`).
        ///
        /// An ACTUAL generation change (`Some(T0)` -> `Some(T1)`, `T0 != T1`)
        /// additionally invalidates every cached `stk_aave_balance` on the
        /// GHO row's chain — the cached values are T0-denominated (the old
        /// contract's ledger), so carrying them across the rotation would let
        /// a balance minted on one token be read as another's. The
        /// invalidation runs on the caller's connection (the chunk's own
        /// `Transaction`), so the rotation + the invalidation commit or roll
        /// back atomically and the next read/backfill
        /// (`get_or_init_stk_aave_balance`) reseeds from the NEW token. The
        /// initial `None` -> `Some` set and an unchanged token invalidate
        /// nothing.
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure, or
        /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
        /// `gho_token_id`.
        fn apply_gho_discount_token_updated, apply_gho_discount_token_updated_on_conn(
            conn: &rusqlite::Connection,
            gho_token_id: i64,
            new_discount_token: Option<&str>,
        ) -> Result<(), DbError> {
            let old: Option<String> = conn
                .query_row(
                    "SELECT v_gho_discount_token FROM aave_gho_tokens WHERE id = ?1",
                    [gho_token_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    DbError::MissingRow(format!(
                        "aave_gho_tokens id={gho_token_id} (DiscountTokenUpdated apply target)"
                    ))
                })?;
            let updated = conn.execute(
                "UPDATE aave_gho_tokens SET v_gho_discount_token = ?1 WHERE id = ?2",
                params![new_discount_token, gho_token_id],
            )?;
            if updated == 0 {
                return Err(DbError::MissingRow(format!(
                    "aave_gho_tokens id={gho_token_id} (DiscountTokenUpdated apply target)"
                )));
            }
            let generation_changed = matches!(
                (&old, new_discount_token),
                (Some(old), Some(new)) if !old.eq_ignore_ascii_case(new)
            );
            if generation_changed {
                // The GHO row is chain-unique (its chain rides the underlying
                // erc20_tokens row) and the discount token is a chain-wide
                // property — every user row on that chain holding a cached
                // balance carries an old-generation value.
                let chain_id: i64 = conn.query_row(
                    "SELECT t.chain FROM aave_gho_tokens g \
                     JOIN erc20_tokens t ON t.id = g.token_id \
                     WHERE g.id = ?1",
                    [gho_token_id],
                    |r| r.get(0),
                )?;
                conn.execute(
                    "UPDATE aave_v3_users SET stk_aave_balance = NULL \
                     WHERE stk_aave_balance IS NOT NULL \
                     AND market_id IN (SELECT id FROM aave_v3_markets WHERE chain_id = ?1)",
                    params![chain_id],
                )?;
            }
            Ok(())
        }
    }

    conn_writer! {
        /// Apply a stkAAVE `Transfer(from, to, value)` event. Port of
        /// `stkaave.process_stk_aave_transfer_event` — processes EVERY Transfer
        /// leg (including both zero-leg arms: `Transfer(0→X)` mint + `Transfer(X→0)`
        /// burn) via `Option<i64>` `user_ids`. The `None` side (the `ZERO_ADDRESS` leg)
        /// is skipped entirely; only the `Some` side is mutated. The degenerate
        /// `Transfer(0→0)` case lands both `None` and applies nothing.
        ///
        /// The zero-leg is applied, never dedupe-skipped: some actions emit
        /// ONLY the `Transfer(X→0)` event with no paired Redeem, and skipping
        /// it leaves the sender's `stk_aave_balance` at the stale pre-burn
        /// value → a wrong `calculate_gho_discount_rate` → a GHO-burn delta that
        /// overshoots the scaled balance (the overshoot equals
        /// `discount_scaled` at the stale `prev_discount`).
        ///
        /// The `from`-leg underflow guard (value > from's balance) errors as
        /// [`DbError::Decode`] rather than panicking: a chain that could produce
        /// it is a decode fault, and the caller must see it. The `to`-leg
        /// overflow guard is explicit for the same reason — an unchecked `+=`
        /// would panic on overflow. Both writes happen in the caller's
        /// `Transaction` (atomic with the chunk).
        ///
        /// # Errors
        ///
        /// Returns [`DbError::Sqlite`] on a query failure, [`DbError::MissingRow`]
        /// if a `Some(user_id)` matches no row, or [`DbError::Decode`] if a
        /// persisted string is malformed, the `from`-leg underflows, or the
        /// `to`-leg overflows.
        fn apply_stk_aave_transfer, apply_stk_aave_transfer_on_conn(
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
    }

    conn_writer! {
        /// Apply a `RewardsController` `RewardsClaimed` event. **No-op** — the
        /// event has NO handler: rewards claims surface only via the stkAAVE
        /// token's `Transfer` events, and the DB has no rewards table. The
        /// receipt settles the log as a rewards-accounting role, so the parser
        /// still accounts for it; the write side records nothing.
        ///
        /// # Errors
        ///
        /// Never errors — the no-op always returns `Ok(())`.
        fn apply_rewards_claimed, apply_rewards_claimed_on_conn(
            _conn: &rusqlite::Connection,
            _user_id: i64,
            _reward_token_id: i64,
            _claimer_id: i64,
            _claimed_amount: alloy::primitives::U256,
        ) -> Result<(), DbError> {
            Ok(())
        }
    }

    // ── Substrate lookups (the parser's address→id resolution) ──
    //
    // The GHO-token lookup the parser needs. It mirrors the Python
    // `operations_parser.py::_get_*` helpers in shape — `&Connection` so a
    // chunk's lookups run inside its one `Transaction` (the write-module
    // connection seam).

    conn_writer! {
        /// Get-or-create an `aave_gho_tokens` row by `(chain_id, token_address)`.
        /// Port of the GHO-token resolution the parser + apply glue uses to
        /// resolve the `gho_token_id` parameter the GHO apply fns take (the apply
        /// fns `apply_gho_discount_rate_strategy_updated_on_conn` /
        /// `apply_gho_discount_token_updated_on_conn` require a pre-resolved
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
        fn get_or_create_gho_token, get_or_create_gho_token_on_conn(
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
    }

    /// SET (not increment) the user's `stk_aave_balance` to the `balanceOf` RPC
    /// result. Used when `stk_aave_balance` is None (a user never touched by a
    /// stkAAVE `Staked`/`Transfer` event yet — `get_or_init_stk_aave_balance`).
    /// Mirrors Python's `user.stk_aave_balance = balance`.
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
