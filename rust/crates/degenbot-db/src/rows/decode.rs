//! Boundary decoders — the `VARCHAR(78)` ↔ `U256` and `VARCHAR(42)` ↔ `Address`
//! conversions (Decision 3 of the degenbot-db substrate).
//!
//! Every EVM integer column is stored as a `VARCHAR(78)` decimal string (mirrors
//! Python's `IntMappedToString` `TypeDecorator`); these helpers parse to
//! [`alloy::primitives::U256`] at the row-decode boundary via
//! `U256::from_str_radix(s, 10)` (and re-encode via `u256.to_string()`). Zero
//! schema change vs. the Alembic-head DBs — the round-trip is lossless across
//! the full 32-byte EVM range (`U256::MAX` = 78 decimal digits).

use alloy::primitives::{Address, U256};

use crate::error::DbError;

/// Decode a non-null `VARCHAR(78)` column value to [`U256`].
pub(crate) fn decode_u256(s: &str) -> Result<U256, DbError> {
    U256::from_str_radix(s.trim(), 10)
        .map_err(|e| DbError::Decode(format!("u256 parse of {s:?}: {e}")))
}

/// Decode a signed `VARCHAR(78)` column value to [`I256`]. Used for
/// `liquidity_net` (upper ticks carry a negative net). The DB stores it as a
/// signed decimal string (Python `str(int)`), so `U256::from_str_radix`
/// rejects the leading `-`; `I256::from_dec_str` accepts it.
pub(crate) fn decode_i256(s: &str) -> Result<alloy::primitives::I256, DbError> {
    alloy::primitives::I256::from_dec_str(s.trim())
        .map_err(|e| DbError::Decode(format!("i256 parse of {s:?}: {e}")))
}

/// Decode a nullable `VARCHAR(78)` column value to `Option<U256>`.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the non-null value is not a valid `U256`.
// Reference-free until fetch_aave_* lands (AZGJUN) — keep it that way: a call
// from dead code (e.g. the aave module's `RowGet` impls) left this `expect`
// unfulfilled on older rustc versions, breaking builds under
// `[workspace.lints.rust] warnings = "deny"` (macos-14 wheel CI).
#[expect(dead_code)]
pub(crate) fn decode_opt_u256(s: Option<&str>) -> Result<Option<U256>, DbError> {
    s.map(decode_u256).transpose()
}

/// Decode a non-null `VARCHAR(42)` checksummed address to [`Address`].
///
/// Addresses are stored EIP-55 checksummed (Python `ChecksummedAddress`); this
/// validates the checksum, matching the Python reader's `get_checksum_address`.
pub(crate) fn decode_address(s: &str) -> Result<Address, DbError> {
    Address::parse_checksummed(s, None)
        .map_err(|e| DbError::Decode(format!("address parse of {s:?}: {e}")))
}

/// Decode a nullable `VARCHAR(42)` checksummed address.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the non-null value fails the EIP-55 checksum.
pub(crate) fn decode_opt_address(s: Option<&str>) -> Result<Option<Address>, DbError> {
    s.map(decode_address).transpose()
}

/// Re-encode a [`U256`] to its `VARCHAR(78)` decimal-string form (the
/// inverse of [`decode_u256`]); used at any future write boundary and by the
/// parity fixture to compare against the Python-side decimal strings.
#[must_use]
pub fn encode_u256(v: &U256) -> String {
    v.to_string()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_full_u256_range() {
        let cases = [
            U256::ZERO,
            U256::from(1u64),
            U256::from(i64::MAX as u64) + U256::from(1u64), // first value beyond i64
            U256::from(2u64).pow(U256::from(130u64)),       // comfortably beyond i64
            U256::MAX,
        ];
        for v in cases {
            let s = encode_u256(&v);
            assert!(s.len() <= 78, "{s} exceeds VARCHAR(78)");
            let back = decode_u256(&s).unwrap();
            assert_eq!(v, back, "{s} did not round-trip");
        }
    }

    #[test]
    fn rejects_malformed() {
        assert!(decode_u256("not-a-number").is_err());
        assert!(decode_u256("-1").is_err()); // negatives aren't valid U256 strings
    }

    #[test]
    fn address_round_trip() {
        let s = "0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b"; // checksummed
        let a = decode_address(s).unwrap();
        assert_eq!(a.to_checksum(None), s);
        assert!(decode_address("0x236aa50979d5f3de3bd1eeb40e81137f22ab794b").is_err());
        // bad checksum
    }
}
