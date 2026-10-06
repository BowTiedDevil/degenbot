//! Shared write-substrate helpers.

use super::{DbError, OptionalExtension};

/// Parse a decimal-`VARCHAR` `U256` value. Mirrors the Python `int(s)` parse
/// of an `AaveV3*Position.balance` attribute read back from the DB.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the string is not a valid decimal
/// `U256`.
pub(super) fn parse_decimal_u256(s: &str) -> Result<alloy::primitives::U256, DbError> {
    alloy::primitives::U256::from_str_radix(s, 10)
        .map_err(|e| DbError::Decode(format!("bad decimal U256 {s:?}: {e}")))
}

/// The one existing-row probe behind every `existing_*` lookup:
/// `prepare_cached` (the compiled statement is cached across calls — banked
/// once here for every constant-SQL single-row shape) + `query_row` returning
/// the first column as an `Option<i64>` id.
pub(super) fn existing_row_id(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Option<i64>, DbError> {
    let mut s = conn.prepare_cached(sql)?;
    Ok(s.query_row(params, |r| r.get(0)).optional()?)
}
