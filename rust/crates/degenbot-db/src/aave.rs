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
use rusqlite::{params, Connection};

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

/// The GHO-token row for a market, with the underlying GHO token + vToken
/// addresses resolved via the `erc20_tokens` FK joins (`fetch_aave_gho_asset`).
/// Mirrors the Python `get_gho_asset(market)` shape (the hydrated `AaveGhoToken`
/// ORM object + its `token`/`v_token` relationships).
///
/// `v_gho_discount_token` is the stkAAVE address (the discount-token that
/// stkAAVE-`Transfer`s are fetched against in `fetch_stk_aave_events`);
/// `v_gho_discount_rate_strategy` is the discount-rate-strategy contract (no
/// fetch target itself — only the `DISCOUNT_RATE_STRATEGY_UPDATED` event reads
/// it). `gho_token_address` / `v_token_address` are the resolved checksummed
/// `erc20_tokens.address` strings (the FK table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveGhoAsset {
    /// `aave_gho_tokens.id` (the row id; the chain-unique GHO token row).
    pub id: i64,
    /// The underlying GHO erc20 token id (`aave_gho_tokens.token_id`).
    pub token_id: i64,
    /// The GHO variable-debt token id (`aave_gho_tokens.v_token_id`; `None` if
    /// the GHO token row has no vToken FK — shouldn't happen on a
    /// well-formed DB, but mirrored for parity).
    pub v_token_id: Option<i64>,
    /// The checksummed underlying GHO token address (`erc20_tokens.address`
    /// joined via `token_id`). `None` if the FK is missing.
    pub gho_token_address: Option<String>,
    /// The checksummed GHO variable-debt token address (`erc20_tokens.address`
    /// joined via `v_token_id`). `None` if `v_token_id` is `None` or the FK is
    /// missing.
    pub v_token_address: Option<String>,
    /// The checksummed discount-rate-strategy contract address (or `None` if
    /// not set).
    pub v_gho_discount_rate_strategy: Option<String>,
    /// The checksummed stkAAVE token address (or `None` if not set — the
    /// market has no GHO discount token). `fetch_stk_aave_events` skips when
    /// this is `None`.
    pub v_gho_discount_token: Option<String>,
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
        Self::fetch_aave_oracle_address_on_conn(&conn, market_id)
    }

    /// The `PRICE_ORACLE` contract address for `market_id`, read on a borrowed
    /// [`Connection`] (the chunk loop's `Transaction`) — so it sees the
    /// in-progress chunk's uncommitted writes (read-your-own-writes within the
    /// chunk) + prior chunks' committed writes. Used by the Aave updater's
    /// `ReserveInitialized` dispatch to resolve the oracle FRESH (the spec's
    /// `oracle_address` is cached once before the loop + can be `None` when the
    /// `PRICE_ORACLE` row is registered mid-loop via a `PriceOracleUpdated`
    /// event — I2RHGP Fix 1b).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_oracle_address_on_conn(
        conn: &Connection,
        market_id: i64,
    ) -> Result<Option<String>, DbError> {
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

    // ── 6SWY4R-1: the orchestrator's market-bootstrap substrate ─────────
    //
    // Four read fns the `run_aave_update` orchestrator (sibling `6SWY4R`)
    // calls once at the top of a run to build the per-market `AaveMarketContext`
    // (the pool/configurator/oracle/gho-asset addresses + the scaled-token
    // address set + the `last_update_block` cursor). These mirror the Python
    // `aave/commands.py` bootstrap: `get_contract(market, name)` (the contracts
    // row), `get_gho_asset(market)` (the GHO token), +
    // `_get_all_scaled_token_addresses(chain_id)` (the aToken/vToken set).
    //
    // They follow the same `&self` + `self.lock()` + `rusqlite::params!` shape
    // as `fetch_aave_oracle_address` / `fetch_aave_asset_addresses` above (the
    // established precedent). The orchestrator consumes these + builds the
    // context; no business logic here.

    /// The market row (`aave_v3_markets`) for `market_id` (the `id, chain_id,
    /// name, active, last_update_block` columns). The orchestrator reads
    /// `last_update_block` as the chunk cursor + `chain_id` for the
    /// scaled-token-address fetch.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_market_row(
        &self,
        market_id: i64,
    ) -> Result<Option<crate::rows::AaveV3MarketRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, chain_id, name, active, last_update_block \
             FROM aave_v3_markets WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![market_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(crate::rows::AaveV3MarketRow {
                id: row.get(0)?,
                chain_id: row.get(1)?,
                name: row.get(2)?,
                active: row.get(3)?,
                last_update_block: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// All `aave_v3_contracts` rows for `market_id` (the
    /// `id, market_id, name, address, revision` columns), unfiltered. The
    /// orchestrator indexes these by `name` (the Python `get_contract(market,
    /// name)` shape) to resolve `POOL`, `POOL_CONFIGURATOR`, `PRICE_ORACLE`,
    /// `POOL_ADDRESS_PROVIDER` (the bootstrap contracts). Returns all rows so
    /// one query serves every name lookup (mirrors the Python's per-market
    /// `session.query` over the ORM relationship — a single round-trip).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_contracts(
        &self,
        market_id: i64,
    ) -> Result<Vec<crate::rows::AaveV3ContractRow>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, market_id, name, address, revision \
             FROM aave_v3_contracts WHERE market_id = ?1",
        )?;
        let rows = stmt.query_map(params![market_id], |r| {
            let id: i64 = r.get(0)?;
            let market_id: i64 = r.get(1)?;
            let name: String = r.get(2)?;
            let address: String = r.get(3)?;
            let revision: Option<i64> = r.get(4)?;
            Ok((id, market_id, name, address, revision))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, market_id, name, address, revision) = row?;
            out.push(crate::rows::AaveV3ContractRow {
                id,
                market_id,
                name,
                address: crate::rows::decode::decode_address(&address)?,
                revision,
            });
        }
        Ok(out)
    }

    /// The GHO-token row for `chain_id` (the `aave_gho_tokens` table) with the
    /// underlying GHO token + vToken addresses resolved via the `erc20_tokens`
    /// FK joins. Mirrors the Python `get_gho_asset(market)` shape (which loads
    /// all `AaveGhoToken` rows + filters by `gho.token.chain == market.chain_id` —
    /// the table is tiny, ~1 row per chain, so the JOIN-through-chain filter is
    /// the Rust equivalent). Returns `None` when the chain has no GHO token
    /// (a non-GHO market, e.g. the zkSync or Polygon instances without GHO).
    ///
    /// NB: `aave_gho_tokens` has NO `market_id` column — the linkage to a market
    /// is via `chain` (the `erc20_tokens.chain` FK on the underlying GHO token).
    /// The caller resolves `chain_id` from `fetch_aave_market_row(market_id)`.
    ///
    /// The resolved `gho_token_address` + `v_token_address` are what the
    /// orchestrator passes to `process_transaction` as `gho_token_address` /
    /// `gho_vtoken_address` + what `run_aave_update` uses as the discount-token
    /// filter (`v_gho_discount_token` is the stkAAVE address).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_gho_asset(&self, chain_id: i64) -> Result<Option<AaveGhoAsset>, DbError> {
        Self::fetch_aave_gho_asset_on_conn(&self.lock(), chain_id)
    }

    /// The `&Connection`-bound variant (for the chunk-loop's single
    /// `Transaction` — §3.4 atomicity).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_gho_asset_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
    ) -> Result<Option<AaveGhoAsset>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT g.id, g.token_id, g.v_token_id, \
                    g.v_gho_discount_rate_strategy, g.v_gho_discount_token, \
                    et_gho.address, et_v.address \
             FROM aave_gho_tokens g \
             JOIN erc20_tokens et_gho ON et_gho.id = g.token_id \
             LEFT JOIN erc20_tokens et_v ON et_v.id = g.v_token_id \
             WHERE et_gho.chain = ?1",
        )?;
        let mut rows = stmt.query(params![chain_id])?;
        match rows.next()? {
            Some(row) => {
                let id: i64 = row.get(0)?;
                let token_id: i64 = row.get(1)?;
                let v_token_id: Option<i64> = row.get(2)?;
                let v_gho_discount_rate_strategy: Option<String> = row.get(3)?;
                let v_gho_discount_token: Option<String> = row.get(4)?;
                let gho_token_address: Option<String> = row.get(5)?;
                let v_token_address: Option<String> = row.get(6)?;
                Ok(Some(AaveGhoAsset {
                    id,
                    token_id,
                    v_token_id,
                    gho_token_address,
                    v_token_address,
                    v_gho_discount_rate_strategy,
                    v_gho_discount_token,
                }))
            }
            None => Ok(None),
        }
    }

    /// All aToken + vToken addresses for `chain_id` (mirrors Python
    /// `_get_all_scaled_token_addresses`). Joins `aave_v3_assets` (per-market)
    /// to `aave_v3_markets` (for the chain filter) to `erc20_tokens` (for the
    /// addresses). Deduplicates via `SELECT DISTINCT`.
    ///
    /// Addresses are returned as raw stored [`String`]s — `fetch_pool_events`/
    /// `fetch_scaled_token_events` accept the checksum form; the orchestrator
    /// checksum-normalizes once at the call site (mirrors the Python path
    /// which holds `ChecksumAddress`s).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_aave_scaled_token_addresses(&self, chain_id: i64) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT et.address \
             FROM aave_v3_assets a \
             JOIN aave_v3_markets m ON m.id = a.market_id \
             JOIN erc20_tokens et ON et.id = a.a_token_id \
             WHERE m.chain_id = ?1 \
             UNION \
             SELECT DISTINCT et.address \
             FROM aave_v3_assets a \
             JOIN aave_v3_markets m ON m.id = a.market_id \
             JOIN erc20_tokens et ON et.id = a.v_token_id \
             WHERE m.chain_id = ?1",
        )?;
        let rows = stmt.query_map(params![chain_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Read a user's `gho_discount` by `(market_id, address)` — the path #1
    /// lookup for the discount pre-pass (`build_discount_snapshot` in
    /// `degenbot-aave::config_dispatch`). Returns `None` when the user
    /// doesn't exist (the path #2 RPC trigger — mirrors the Python
    /// `gho_users.get(user_address)` returning `None`). Pure read (does NOT
    /// create a user — `get_or_create_user_on_conn` is the dispatch path's
    /// responsibility). The `_on_conn` bound serves the chunk-loop's single
    /// `Transaction` (§3.4 atomicity).
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] on a `SQLite` query failure.
    pub fn fetch_user_gho_discount_by_address_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        address: &str,
    ) -> Result<Option<i64>, DbError> {
        let res = conn.query_row(
            "SELECT gho_discount FROM aave_v3_users \
             WHERE market_id = ?1 AND address = ?2 LIMIT 1",
            params![market_id, address],
            |r| r.get::<_, i64>(0),
        );
        match res {
            Ok(d) => Ok(Some(d)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::connection::DegenbotDb;
    use rusqlite::params;
    use std::path::Path;

    /// An in-memory writeable DB with the head schema, + a seeded market
    /// (`id=1, chain_id=1`) + 3 erc20 tokens (underlying `0xu1`/aToken `0xa1`/
    /// vToken `0xv1`, ids 1/2/3) + one `aave_v3_assets` row (id 1, market 1,
    /// token ids 1/2/3). Mirrors `write.rs`'s `write_db_with_market`/`seed_asset`.
    fn db_with_market_and_assets() -> DegenbotDb {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let conn = db.lock();
        // market id=1, chain_id=1, last_update_block=NULL.
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'aave-v3-ethereum', 1, NULL)",
            [],
        )
        .unwrap();
        // 3 erc20 tokens (ids 1/2/3, chain 1).
        for (id, addr) in [(1_i64, "0xu1"), (2, "0xa1"), (3, "0xv1")] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, addr],
            )
            .unwrap();
        }
        // aave_v3_assets row: underlying=1, aToken=2, vToken=3.
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '1', '0')",
            [],
        )
        .unwrap();
        // contracts: POOL + POOL_CONFIGURATOR + PRICE_ORACLE + POOL_ADDRESS_PROVIDER.
        for (cid, name, addr, rev) in [
            (
                1_i64,
                "POOL",
                "0x1111111111111111111111111111111111111111",
                Some(1_i64),
            ),
            (
                2,
                "POOL_CONFIGURATOR",
                "0x2222222222222222222222222222222222222222",
                Some(1),
            ),
            (
                3,
                "PRICE_ORACLE",
                "0x3333333333333333333333333333333333333333",
                None,
            ),
            (
                4,
                "POOL_ADDRESS_PROVIDER",
                "0x4444444444444444444444444444444444444444",
                Some(1),
            ),
        ] {
            conn.execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
                 VALUES (?1, 1, ?2, ?3, ?4)",
                params![cid, name, addr, rev],
            )
            .unwrap();
        }
        // aave_gho_tokens row: token_id=1, v_token_id=3, discount_token=“0xstk”.
        conn.execute(
            "INSERT INTO aave_gho_tokens \
                (id, token_id, v_token_id, v_gho_discount_rate_strategy, v_gho_discount_token) \
             VALUES (1, 1, 3, '0xstrate', '0xstk')",
            [],
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn fetch_aave_market_row_reads_seeded_market() {
        let db = db_with_market_and_assets();
        let row = db.fetch_aave_market_row(1).unwrap().unwrap();
        assert_eq!(row.id, 1);
        assert_eq!(row.chain_id, 1);
        assert_eq!(row.name, "aave-v3-ethereum");
        assert!(row.active);
        assert!(
            row.last_update_block.is_none(),
            "seeded with NULL last_update_block"
        );
    }

    #[test]
    fn fetch_aave_market_row_missing_returns_none() {
        let db = db_with_market_and_assets();
        assert!(db.fetch_aave_market_row(9999).unwrap().is_none());
    }

    #[test]
    fn fetch_aave_contracts_returns_all_seeded_rows() {
        let db = db_with_market_and_assets();
        let contracts = db.fetch_aave_contracts(1).unwrap();
        assert_eq!(
            contracts.len(),
            4,
            "POOL + POOL_CONFIGURATOR + PRICE_ORACLE + POOL_ADDRESS_PROVIDER"
        );
        // Find by name — the orchestrator's pattern.
        let pool = contracts
            .iter()
            .find(|c| c.name == "POOL")
            .expect("POOL contract present");
        assert_eq!(
            pool.address.to_string().to_lowercase(),
            "0x1111111111111111111111111111111111111111"
        );
        assert_eq!(pool.revision, Some(1));
        let cfg = contracts
            .iter()
            .find(|c| c.name == "POOL_CONFIGURATOR")
            .expect("POOL_CONFIGURATOR present");
        assert_eq!(cfg.revision, Some(1));
    }

    #[test]
    fn fetch_aave_contracts_empty_market_returns_empty_vec() {
        let db = db_with_market_and_assets();
        // market 2 doesn't exist → zero contracts (mirrors the Python
        // `get_contract(missing_market) → None` path; the orchestrator handles
        // the empty case).
        assert!(db.fetch_aave_contracts(2).unwrap().is_empty());
    }

    #[test]
    fn fetch_aave_gho_asset_resolves_token_addresses() {
        let db = db_with_market_and_assets();
        let gho = db.fetch_aave_gho_asset(1).unwrap().unwrap();
        assert_eq!(gho.id, 1);
        assert_eq!(gho.token_id, 1, "underlying GHO token id");
        assert_eq!(gho.v_token_id, Some(3), "vToken id");
        assert_eq!(gho.gho_token_address.as_deref(), Some("0xu1"));
        assert_eq!(gho.v_token_address.as_deref(), Some("0xv1"));
        assert_eq!(
            gho.v_gho_discount_rate_strategy.as_deref(),
            Some("0xstrate")
        );
        assert_eq!(gho.v_gho_discount_token.as_deref(), Some("0xstk"));
    }

    #[test]
    fn fetch_aave_gho_asset_missing_returns_none() {
        let db = db_with_market_and_assets();
        // chain 2 has no GHO token → None (a non-GHO market).
        assert!(db.fetch_aave_gho_asset(2).unwrap().is_none());
    }

    /// Regression for the coldboot GHO-vToken staleness crash (block 17699253).
    ///
    /// At coldboot `16591070` the GHO reserve isn't initialized yet →
    /// `aave_gho_tokens.v_token_id IS NULL` → `fetch_aave_gho_asset_on_conn`
    /// returns `v_token_address = None`. Mid-drive (`17699249`) a
    /// `ReserveInitialized` event for GHO links `v_token_id` to the new
    /// vToken's erc20 row. The orchestrator's per-tx re-resolution
    /// (`process_chunk_on_conn` step 0) re-calls `fetch_aave_gho_asset_on_conn`
    /// on the SAME chunk `Transaction`'s `&Connection` and MUST see the
    /// in-transaction write (read-your-own-writes) — otherwise the subsequent
    /// GHO borrow's vToken `Mint` classifies as plain `DebtMint` (not
    /// `GhoDebtMint`) → the borrow matcher finds no `DebtMint` → `NoMatch` crash.
    ///
    /// This test pins the substrate seam the fix relies on: a same-
    /// `Connection` `apply_reserve_initialized_on_conn` write is visible to
    /// the next `fetch_aave_gho_asset_on_conn` read.
    #[test]
    fn fetch_aave_gho_asset_on_conn_sees_intra_tx_v_token_link_write() {
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        let gho_row_id = {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
            // The underlying GHO erc20 (id 1). NO vToken erc20 seeded yet
            // (the GHO reserve is not initialized at coldboot).
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xgho')",
                [],
            )
            .unwrap();
            // The GHO token row with v_token_id = NULL (coldboot state).
            DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, "0xgho").unwrap()
        };

        // (1) Pre-init: the per-tx re-resolution sees v_token_address = None.
        {
            let conn = db.lock();
            let gho = DegenbotDb::fetch_aave_gho_asset_on_conn(&conn, 1)
                .unwrap()
                .expect("GHO token row exists");
            assert!(
                gho.v_token_id.is_none(),
                "coldboot: v_token_id must be NULL before ReserveInitialized"
            );
            assert!(
                gho.v_token_address.is_none(),
                "coldboot: v_token_address must resolve to None"
            );
        }

        // (2) Mid-drive: ReserveInitialized links v_token_id to the new
        //     vToken erc20 (id 2 = '0xvtoken') on the SAME connection.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (2, 1, '0xvtoken')",
                [],
            )
            .unwrap();
            DegenbotDb::apply_reserve_initialized_on_conn(
                &conn,
                1,
                /*underlying*/ 1,
                /*a_token*/ 1,
                1,
                /*v_token*/ 2,
                1,
                /*price_source*/ None,
                /*gho_link*/ Some(gho_row_id),
            )
            .unwrap();
        }

        // (3) Post-init same-conn re-resolution: v_token_address = Some.
        {
            let conn = db.lock();
            let gho = DegenbotDb::fetch_aave_gho_asset_on_conn(&conn, 1)
                .unwrap()
                .expect("GHO token row exists");
            assert_eq!(
                gho.v_token_id,
                Some(2),
                "the in-transaction ReserveInitialized write must be visible"
            );
            assert_eq!(
                gho.v_token_address.as_deref(),
                Some("0xvtoken"),
                "the per-tx re-resolution must see the linked vToken address"
            );
        }
    }

    #[test]
    fn fetch_aave_scaled_token_addresses_returns_atoken_and_vtoken() {
        let db = db_with_market_and_assets();
        let addrs = db.fetch_aave_scaled_token_addresses(1).unwrap();
        assert_eq!(addrs.len(), 2, "aToken (0xa1) + vToken (0xv1)");
        assert!(addrs.contains(&"0xa1".to_string()));
        assert!(addrs.contains(&"0xv1".to_string()));
    }

    #[test]
    fn fetch_aave_scaled_token_addresses_no_assets_for_chain_returns_empty() {
        let db = db_with_market_and_assets();
        // chain 2 has no assets → empty (mirrors the Python guard).
        assert!(db.fetch_aave_scaled_token_addresses(2).unwrap().is_empty());
    }
}
