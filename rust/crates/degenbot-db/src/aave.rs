//! Aave V3 position read-back fns (read-only). Rehydrates the very rows the
//! `AZGJUN` writers persist: `aave_v3_users` (+ isolation-debt-ceiling join),
//! `aave_v3_collateral_positions` / `aave_v3_debt_positions` (+ asset /
//! `asset_config` / `e_mode_category` / `underlying_token` joins),
//! `aave_v3_user_collateral_configs`, `aave_v3_contracts` (`PRICE_ORACLE`),
//! `aave_v3_assets` underlying addresses.
//!
//! These mirror Python `aave/analysis/orchestrator.py::DatabasePositionQuery`
//! (`select(...).where(...).joinedload(...)` chains) so the cross-implementation
//! parity fixture asserts identical user/position/asset graphs, including the
//! `VARCHAR(78)` to [`alloy::primitives::U256`] boundary (debt/balance/index
//! values) and `VARCHAR(42)` to raw stored address strings.
//!
//! The domain shape mirrors Python `aave/analysis/core.py` `UserRecord`,
//! `CollateralPositionRecord`, `DebtPositionRecord` — flat records the pure
//! core consumes. Addresses are returned as raw stored [`String`]s (verbatim,
//! matching the `SQLAlchemy` path which returns the stored `String(42)` as-is —
//! no checksum normalization, so byte-exact parity regardless of stored case);
//! the pure core keys `price_map` by the same stored strings (all sourced from
//! the single `erc20_tokens.address` column), so internal consistency holds.

use std::collections::HashMap;

use alloy::primitives::U256;
use rusqlite::params;

use crate::connection::DegenbotDb;
use crate::error::DbError;
use crate::rows::decode::decode_u256;

/// The EIP-55 checksummed zero address (the Python fallback when an asset's
/// underlying token relationship is absent — `underlying_asset_id` is `NOT
/// NULL` with a `FK` so this never triggers on a well-formed DB, but the
/// Python `_convert_*` path emits it defensively; mirrored here for parity).
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// Flat user record (mirrors Python `UserRecord`).
/// `isolation_debt_ceiling` is materialized from the LEFT JOIN
/// `users.isolation_mode_collateral_asset_id` to `assets.id` to
/// `asset_configs.asset_id` to `debt_ceiling` (the Python
/// `user.isolation_collateral_asset.asset_config` relationship chain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveUserRecord {
    pub id: i64,
    pub address: String,
    pub market_id: i64,
    pub e_mode: i64,
    pub is_isolation_mode: bool,
    pub isolation_mode_debt: U256,
    pub isolation_debt_ceiling: Option<U256>,
}

/// Flat collateral-position record (mirrors Python
/// `CollateralPositionRecord`). Joins `collateral_positions.asset_id` to
/// `assets.id` (+ `asset_config`, `e_mode_category`, `underlying_token`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveCollateralPositionRecord {
    pub asset_id: i64,
    pub balance: U256,
    pub underlying_address: String,
    pub underlying_symbol: Option<String>,
    pub liquidity_index: U256,
    pub e_mode_category_id: Option<i64>,
    pub asset_lt: i64,
    pub asset_ltv: i64,
    pub emode_lt: Option<i64>,
    pub emode_ltv: Option<i64>,
}

/// Flat debt-position record (mirrors Python `DebtPositionRecord`). Joins
/// `debt_positions.asset_id` to `assets.id` (+ `e_mode_category`,
/// `underlying_token`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveDebtPositionRecord {
    pub asset_id: i64,
    pub balance: U256,
    pub underlying_address: String,
    pub underlying_symbol: Option<String>,
    pub borrow_index: U256,
    pub e_mode_category_id: Option<i64>,
}

impl DegenbotDb {
    /// Users with at least one debt position in `market_id` (mirrors Python
    /// `get_users_with_debt`: `select(AaveV3User).where(market_id)`
    /// `.joinedload(debt_positions)` then `[u for u in users if
    /// len(debt_positions) > 0]`).
    ///
    /// `isolation_debt_ceiling` comes from the LEFT JOIN through the isolation
    /// collateral asset's `asset_config.debt_ceiling` (`None` when the user is
    /// not in isolation mode or the asset/config is absent). Rows are returned
    /// in the table's natural (rowid) scan order (matching the `SQLAlchemy`
    /// default); `limit` slices after the debt-presence filter (matching
    /// Python's `users_with_debt[:limit]`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query / `U256` decode failure.
    pub fn fetch_aave_users_with_debt(
        &self,
        market_id: i64,
        limit: Option<i64>,
    ) -> Result<Vec<AaveUserRecord>, DbError> {
        let conn = self.lock();
        // EXISTS semi-join = "users with len(debt_positions) > 0".
        // LEFT JOIN users.isolation_mode_collateral_asset_id → assets.id →
        // asset_configs.asset_id → debt_ceiling (None when not in isolation mode).
        let mut stmt = conn.prepare(
            "SELECT u.id, u.address, u.market_id, u.e_mode, \
                    (u.isolation_mode_collateral_asset_id IS NOT NULL) AS is_isolation_mode, \
                    u.isolation_mode_debt, ac.debt_ceiling \
             FROM aave_v3_users u \
             LEFT JOIN aave_v3_assets iso ON iso.id = u.isolation_mode_collateral_asset_id \
             LEFT JOIN aave_v3_asset_configs ac ON ac.asset_id = iso.id \
             WHERE u.market_id = ?1 \
               AND EXISTS (SELECT 1 FROM aave_v3_debt_positions dp WHERE dp.user_id = u.id)",
        )?;
        let rows = stmt.query_map(params![market_id], |r| {
            let id: i64 = r.get(0)?;
            let address: String = r.get(1)?;
            let mid: i64 = r.get(2)?;
            let emode: i64 = r.get(3)?;
            let is_iso: bool = r.get(4)?;
            let debt_str: String = r.get(5)?;
            let ceiling_str: Option<String> = r.get(6)?;
            Ok((id, address, mid, emode, is_iso, debt_str, ceiling_str))
        })?;
        let mut out: Vec<AaveUserRecord> = Vec::new();
        for row in rows {
            let (id, address, mid, emode, is_iso, debt_str, ceiling_str) = row?;
            out.push(AaveUserRecord {
                id,
                address,
                market_id: mid,
                e_mode: emode,
                is_isolation_mode: is_iso,
                isolation_mode_debt: decode_u256(&debt_str)?,
                isolation_debt_ceiling: ceiling_str.as_deref().map(decode_u256).transpose()?,
            });
        }
        if let Some(n) = limit {
            if n >= 0 {
                out.truncate(usize::try_from(n).unwrap_or(usize::MAX));
            }
        }
        Ok(out)
    }

    /// Collateral positions for `user_id` (mirrors Python
    /// `get_collateral_positions`: joinedloads `asset.underlying_token`,
    /// `asset.asset_config`, `asset.e_mode_category`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query / `U256` decode failure.
    pub fn fetch_aave_collateral_positions(
        &self,
        user_id: i64,
    ) -> Result<Vec<AaveCollateralPositionRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            // erc20_tokens joined INNER (underlying_asset_id NOT NULL + FK) —
            // always present; COALESCE to the zero addr matches the Python
            // defensive fallback for parity.
            "SELECT cp.asset_id, cp.balance, \
                    COALESCE(et.address, ?2) AS underlying_address, \
                    et.symbol AS underlying_symbol, \
                    a.liquidity_index, a.e_mode_category_id, \
                    COALESCE(ac.liquidation_threshold, 0) AS asset_lt, \
                    COALESCE(ac.ltv, 0) AS asset_ltv, \
                    emc.liquidation_threshold AS emode_lt, \
                    emc.ltv AS emode_ltv \
             FROM aave_v3_collateral_positions cp \
             JOIN aave_v3_assets a ON a.id = cp.asset_id \
             LEFT JOIN erc20_tokens et ON et.id = a.underlying_asset_id \
             LEFT JOIN aave_v3_asset_configs ac ON ac.asset_id = a.id \
             LEFT JOIN aave_v3_emode_categories emc ON emc.id = a.e_mode_category_id \
             WHERE cp.user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id, ZERO_ADDRESS], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                asset_id,
                balance_str,
                underlying_address,
                underlying_symbol,
                liq_idx_str,
                emode_cat_id,
                asset_lt,
                asset_ltv,
                emode_lt,
                emode_ltv,
            ) = row?;
            out.push(AaveCollateralPositionRecord {
                asset_id,
                balance: decode_u256(&balance_str)?,
                underlying_address,
                underlying_symbol,
                liquidity_index: decode_u256(&liq_idx_str)?,
                e_mode_category_id: emode_cat_id,
                asset_lt,
                asset_ltv,
                emode_lt,
                emode_ltv,
            });
        }
        Ok(out)
    }

    /// Debt positions for `user_id` (mirrors Python `get_debt_positions`:
    /// joinedloads `asset.underlying_token`, `asset.e_mode_category`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query / `U256` decode failure.
    pub fn fetch_aave_debt_positions(
        &self,
        user_id: i64,
    ) -> Result<Vec<AaveDebtPositionRecord>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT dp.asset_id, dp.balance, \
                    COALESCE(et.address, ?2) AS underlying_address, \
                    et.symbol AS underlying_symbol, \
                    a.borrow_index, a.e_mode_category_id \
             FROM aave_v3_debt_positions dp \
             JOIN aave_v3_assets a ON a.id = dp.asset_id \
             LEFT JOIN erc20_tokens et ON et.id = a.underlying_asset_id \
             WHERE dp.user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id, ZERO_ADDRESS], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                asset_id,
                balance_str,
                underlying_address,
                underlying_symbol,
                borrow_idx_str,
                emode_cat_id,
            ) = row?;
            out.push(AaveDebtPositionRecord {
                asset_id,
                balance: decode_u256(&balance_str)?,
                underlying_address,
                underlying_symbol,
                borrow_index: decode_u256(&borrow_idx_str)?,
                e_mode_category_id: emode_cat_id,
            });
        }
        Ok(out)
    }

    /// Map of `asset_id` to `enabled` for `user_id` (mirrors Python
    /// `get_collateral_config_map`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_collateral_config_map(
        &self,
        user_id: i64,
    ) -> Result<HashMap<i64, bool>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT asset_id, enabled FROM aave_v3_user_collateral_configs WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (asset_id, enabled) = row?;
            out.insert(asset_id, enabled);
        }
        Ok(out)
    }

    /// The `PRICE_ORACLE` contract address for `market_id` (mirrors Python
    /// `get_oracle_address`). Returns `None` when no such contract row exists.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_oracle_address(&self, market_id: i64) -> Result<Option<String>, DbError> {
        let conn = self.lock();
        let res = conn.query_row(
            "SELECT address FROM aave_v3_contracts WHERE market_id = ?1 AND name = 'PRICE_ORACLE' LIMIT 1",
            params![market_id],
            |r| r.get::<_, String>(0),
        );
        match res {
            Ok(addr) => Ok(Some(addr)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The set of distinct underlying-token addresses for `market_id` (mirrors
    /// Python `get_asset_addresses`). Returns a `Vec` (deduplicated via
    /// `SELECT DISTINCT`); the seam wraps into a Python `set`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_asset_addresses(&self, market_id: i64) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT et.address \
             FROM aave_v3_assets a \
             JOIN erc20_tokens et ON et.id = a.underlying_asset_id \
             WHERE a.market_id = ?1",
        )?;
        let rows = stmt.query_map(params![market_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
