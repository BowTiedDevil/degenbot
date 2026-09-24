//! `erc20_tokens` table row.

use alloy::primitives::Address;
use rusqlite::Row;

use crate::error::DbError;
use crate::rows::decode::decode_address;

/// A typed projection of the `erc20_tokens` table (`SQLAlchemy` `Erc20TokenTable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erc20TokenRow {
    pub id: i64,
    pub chain: i64,
    pub address: Address,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<i64>,
}

impl Erc20TokenRow {
    /// Decode from a `SELECT id, chain, address, name, symbol, decimals FROM erc20_tokens`.
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            chain: row.get(1)?,
            address: decode_address(&row.get::<_, String>(2)?)?,
            name: row.get(3)?,
            symbol: row.get(4)?,
            decimals: row.get(5)?,
        })
    }
}
