//! `pools` base row + per-DEX subclass rows + the `kind` discriminator enum.
//!
//! `SQLAlchemy` models the per-DEX pools as single-table-per-subclass joined
//! inheritance: the `pools` table holds the base columns, and a `kind` column
//! names the subclass table (e.g. `uniswap_v3` → `uniswap_v3_pools`). The
//! Rust reader dispatches on `kind` to join the matching subclass table and
//! fill a [`PoolKindRow`]. See `models/pools.py`.

use alloy::primitives::{Address, B256};
use rusqlite::Row;

use crate::error::DbError;
use crate::rows::decode::{decode_address, decode_opt_address};
use crate::schema::table;

/// The `pools` base row (`SQLAlchemy` `LiquidityPoolTable`).
///
/// `kind` is the per-DEX subclass discriminator — join `v2_v3_subclass_table`
/// for the V2/V3 families, or `uniswap_v4_pools` (via the `managed_pools`
/// polymorphic base) for V4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityPoolRow {
    pub id: i64,
    pub address: Address,
    pub chain: i64,
    pub kind: String,
    pub token0_id: i64,
    pub token1_id: i64,
    pub exchange_id: i64,
}

impl LiquidityPoolRow {
    /// Decode from a `SELECT id, address, chain, kind, token0_id, token1_id, exchange_id FROM pools`.
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            address: decode_address(&row.get::<_, String>(1)?)?,
            chain: row.get(2)?,
            kind: row.get(3)?,
            token0_id: row.get(4)?,
            token1_id: row.get(5)?,
            exchange_id: row.get(6)?,
        })
    }
}

/// The per-DEX subclass projection. Built by [`crate::read::DegenbotDb::fetch_pool_kind`]
/// joining the subclass table named by the `kind` discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolKindRow {
    /// A V2-family pool: `(pool_id, fee_token0, fee_token1, fee_denominator)`
    /// plus the optional Aerodrome-only `stable` flag.
    V2(V2PoolRow),
    /// A V3-family pool: fees, `tick_spacing`, and the liquidity-update marker
    /// (`liquidity_update_block`, `liquidity_update_log_index`).
    V3(V3PoolRow),
    /// A Uniswap V4 managed pool: pool hash + hooks + currencies + fees +
    /// `tick_spacing` + the liquidity-update marker.
    V4(V4PoolRow),
}

/// The V2 subclass columns shared by every V2 variant (`UniswapFeeMixin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2PoolRow {
    pub pool_id: i64,
    pub fee_token0: i64,
    pub fee_token1: i64,
    pub fee_denominator: i64,
    /// Present only for `aerodrome_v2` (migration `723fe25c87b8`); `None` for
    /// the other V2 variants which have no `stable` column.
    pub stable: Option<bool>,
}

/// The V3 subclass columns shared by every V3 variant (`UniswapV3PoolTableBase`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PoolRow {
    pub pool_id: i64,
    pub tick_spacing: i64,
    pub liquidity_update_block: Option<i64>,
    pub liquidity_update_log_index: Option<i64>,
    pub fee_token0: i64,
    pub fee_token1: i64,
    pub fee_denominator: i64,
}

/// The V4 subclass columns (`SQLAlchemy` `UniswapV4PoolTable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V4PoolRow {
    pub managed_pool_id: i64,
    /// The V4 `pool_hash` stored as `VARCHAR(66)` 0x-prefixed lowercase hex.
    pub pool_hash: B256,
    pub hooks: Address,
    pub currency0_id: i64,
    pub currency1_id: i64,
    pub fee_currency0: i64,
    pub fee_currency1: i64,
    pub fee_denominator: i64,
    pub tick_spacing: i64,
    pub liquidity_update_block: Option<i64>,
    pub liquidity_update_log_index: Option<i64>,
}

impl V2PoolRow {
    /// Column SELECT list matching the subclass tables (Aerodrome adds `stable`
    /// — callers select `stable` last and may pass it through `Option`).
    pub const SELECT_V2: &'static str = "pool_id, fee_token0, fee_token1, fee_denominator";

    /// Decode from a row laid out as `pool_id, fee_token0, fee_token1,
    /// fee_denominator` (the non-Aerodrome subclass shape).
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            pool_id: row.get(0)?,
            fee_token0: row.get(1)?,
            fee_token1: row.get(2)?,
            fee_denominator: row.get(3)?,
            stable: None,
        })
    }

    /// Decode from a row laid out as `pool_id, fee_token0, fee_token1,
    /// fee_denominator, stable` (the Aerodrome subclass shape).
    pub(crate) fn from_row_with_stable(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            pool_id: row.get(0)?,
            fee_token0: row.get(1)?,
            fee_token1: row.get(2)?,
            fee_denominator: row.get(3)?,
            stable: Some(row.get(4)?),
        })
    }
}

impl V3PoolRow {
    /// Column SELECT list matching the V3 subclass tables in
    /// `pool_id, tick_spacing, liquidity_update_block,
    /// liquidity_update_log_index, fee_token0, fee_token1, fee_denominator`
    /// order (mirrors `models/pools.py` column order).
    pub const SELECT_V3: &'static str =
        "pool_id, tick_spacing, liquidity_update_block, liquidity_update_log_index, \
         fee_token0, fee_token1, fee_denominator";

    /// Decode from a row laid out as [`Self::SELECT_V3`].
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            pool_id: row.get(0)?,
            tick_spacing: row.get(1)?,
            liquidity_update_block: row.get(2)?,
            liquidity_update_log_index: row.get(3)?,
            fee_token0: row.get(4)?,
            fee_token1: row.get(5)?,
            fee_denominator: row.get(6)?,
        })
    }
}

impl V4PoolRow {
    /// Column SELECT list matching `uniswap_v4_pools`.
    pub const SELECT_V4: &'static str =
        "managed_pool_id, pool_hash, hooks, currency0_id, currency1_id, \
         fee_currency0, fee_currency1, fee_denominator, tick_spacing, \
         liquidity_update_block, liquidity_update_log_index";

    /// Decode from a row laid out as [`Self::SELECT_V4`].
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        let pool_hash_str: String = row.get(1)?;
        let pool_hash = decode_pool_hash(&pool_hash_str)?;
        Ok(Self {
            managed_pool_id: row.get(0)?,
            pool_hash,
            hooks: decode_address(&row.get::<_, String>(2)?)?,
            currency0_id: row.get(3)?,
            currency1_id: row.get(4)?,
            fee_currency0: row.get(5)?,
            fee_currency1: row.get(6)?,
            fee_denominator: row.get(7)?,
            tick_spacing: row.get(8)?,
            liquidity_update_block: row.get(9)?,
            liquidity_update_log_index: row.get(10)?,
        })
    }
}

/// `pool_managers` table row (`SQLAlchemy` `PoolManagerTable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolManagerRow {
    pub id: i64,
    pub address: Address,
    pub chain: i64,
    pub kind: String,
    pub state_view: Option<Address>,
    pub exchange_id: i64,
}

impl PoolManagerRow {
    /// Decode from `SELECT id, address, chain, kind, state_view, exchange_id FROM pool_managers`.
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            address: decode_address(&row.get::<_, String>(1)?)?,
            chain: row.get(2)?,
            kind: row.get(3)?,
            state_view: decode_opt_address(row.get::<_, Option<String>>(4)?.as_deref())?,
            exchange_id: row.get(5)?,
        })
    }
}

/// `managed_pools` base row (`SQLAlchemy` `ManagedLiquidityPoolTable`, the
/// polymorphic base under `uniswap_v4_pools`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedLiquidityPoolRow {
    pub id: i64,
    pub kind: String,
    pub manager_id: i64,
}

impl ManagedLiquidityPoolRow {
    /// Decode from `SELECT id, kind, manager_id FROM managed_pools`.
    #[expect(dead_code)] // exercised by a future managed-pool read fn (spike 4c siblings)
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            kind: row.get(1)?,
            manager_id: row.get(2)?,
        })
    }
}

/// Decode a `VARCHAR(66)` 0x-prefixed lowercase-hex pool hash to [`B256`].
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `s` is not valid 32-byte hex.
pub(crate) fn decode_pool_hash(s: &str) -> Result<B256, DbError> {
    let stripped = s.trim().trim_start_matches("0x");
    let bytes = alloy::hex::decode(stripped)
        .map_err(|e| DbError::Decode(format!("pool_hash parse of {s:?}: {e}")))?;
    if bytes.len() != 32 {
        return Err(DbError::Decode(format!(
            "pool_hash parse of {s:?}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(B256::from(arr))
}

/// `true` if `kind` is an Aerodrome V2 (the only V2 subclass with a `stable`
/// column).
#[must_use]
pub fn is_aerodrome_v2_kind(kind: &str) -> bool {
    matches!(kind, "aerodrome_v2")
}

/// `true` if `kind` is a V4 discriminator (only `uniswap_v4` today).
///
/// Re-exported for convenience from [`crate::schema::table::is_v4_kind`].
#[must_use]
pub fn is_v4_kind(kind: &str) -> bool {
    table::is_v4_kind(kind)
}

/// Convenience: the SELECT list for an Aerodrome V2 row (includes `stable`).
pub const SELECT_V2_AERODROME: &str = "pool_id, fee_token0, fee_token1, fee_denominator, stable";
