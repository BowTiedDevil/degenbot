//! `exchanges` table row.

use alloy::primitives::Address;
use rusqlite::Row;

use crate::error::DbError;
use crate::rows::decode::{decode_address, decode_opt_address};

/// A typed projection of the `exchanges` table (`SQLAlchemy` `ExchangeTable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeRow {
    pub id: i64,
    pub chain_id: i64,
    pub name: String,
    pub active: bool,
    pub last_update_block: Option<i64>,
    pub factory: Address,
    pub deployer: Option<Address>,
}

impl ExchangeRow {
    /// Decode from a `SELECT id, chain_id, name, active, last_update_block, factory, deployer FROM exchanges`.
    #[allow(dead_code)] // exercised by a future fetch_exchange (spike 4c siblings)
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
        Ok(Self {
            id: row.get(0)?,
            chain_id: row.get(1)?,
            name: row.get(2)?,
            active: row.get(3)?,
            last_update_block: row.get(4)?,
            factory: decode_address(&row.get::<_, String>(5)?)?,
            deployer: decode_opt_address(row.get::<_, Option<String>>(6)?.as_deref())?,
        })
    }
}
