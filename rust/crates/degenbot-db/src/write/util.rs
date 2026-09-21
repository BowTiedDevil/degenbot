//! Shared write-substrate helpers.

use super::DbError;

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
