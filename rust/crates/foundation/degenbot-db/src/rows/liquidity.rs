//! `liquidity_positions` rows + their V4 managed-pool twins. Each carries the
//! EVM big-int columns as
//! [`alloy::primitives::U256`] (decoded from `VARCHAR(78)` decimal strings —
//! Decision 3).

use alloy::primitives::U256;
use rusqlite::Row;

use crate::error::DbError;
use crate::rows::decode::decode_u256;

/// `liquidity_positions` row (V3 — `LiquidityPositionTable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityPositionRow {
    pub id: i64,
    pub pool_id: i64,
    pub tick: i32,
    pub liquidity_net: U256,
    pub liquidity_gross: U256,
}

impl LiquidityPositionRow {
    /// Decode from `SELECT id, pool_id, tick, liquidity_net, liquidity_gross FROM liquidity_positions`.
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            pool_id: row.get(1)?,
            tick: row.get(2)?,
            liquidity_net: decode_u256(&row.get::<_, String>(3)?)?,
            liquidity_gross: decode_u256(&row.get::<_, String>(4)?)?,
        })
    }
}

/// `managed_pool_liquidity_positions` row (V4 twin of [`LiquidityPositionRow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPoolLiquidityPositionRow {
    pub id: i64,
    pub managed_pool_id: i64,
    pub tick: i32,
    pub liquidity_net: U256,
    pub liquidity_gross: U256,
}

impl ManagedPoolLiquidityPositionRow {
    /// Decode from `SELECT id, managed_pool_id, tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions`.
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            managed_pool_id: row.get(1)?,
            tick: row.get(2)?,
            liquidity_net: decode_u256(&row.get::<_, String>(3)?)?,
            liquidity_gross: decode_u256(&row.get::<_, String>(4)?)?,
        })
    }
}
