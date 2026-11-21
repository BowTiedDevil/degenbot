//! Address utilities: the EIP-55 checksum and error-typed parse family the
//! Python shell and the DB seam call.
//!
//! Alloy's `to_checksum` stays valid at call sites that only need the alloy
//! value; these helpers add the `String`-returning checksum form and the
//! `AddressError`-typed parse.

use crate::errors::AddressError;
use alloy::primitives::Address;
use std::str::FromStr;

/// The Curve protocol's native-coin wire spelling — the all-Es address the
/// old Vyper Curve pools spell a native coin with (the Curve protocol's
/// PRIVATE sentinel, ADR-097 D3). This crate is the single authority for the
/// spelling: it is the lowest crate every sentinel consumer (the pools
/// capability, the persistence substrate) already shares, so the constant
/// needs no dependency edge back up the release graph and every copy pins
/// against one definition. It is translated to the conceptual `NativeToken`
/// at Curve construction and coin-projection call sites and is never a token
/// contract address or a generic input; zero remains the coin-enumeration
/// terminator and is not this constant.
pub const NATIVE_SENTINEL_ALL_ES: Address = Address::repeat_byte(0xEE);

/// Format an alloy `Address` as an EIP-55 checksummed string.
#[must_use]
pub fn address_to_checksum_string(address: &Address) -> String {
    address.to_checksum(None)
}

/// Parse a hex address string and return its EIP-55 checksummed form.
///
/// # Errors
///
/// Returns `AddressError::InvalidAddress` if the string is not a valid hex address.
pub fn to_checksum_address_str(addr_str: &str) -> Result<String, AddressError> {
    let addr = parse_address(addr_str)?;
    Ok(address_to_checksum_string(&addr))
}

/// Parse an Ethereum address from a hex string.
///
/// This is the canonical address parsing function used throughout the codebase.
///
/// # Arguments
///
/// * `s` - A hex string representing an Ethereum address (e.g., "0x1234...")
///
/// # Returns
///
/// The parsed `Address`.
///
/// # Errors
///
/// Returns `AddressError::InvalidAddress` if the string is not a valid hex address.
pub fn parse_address(s: &str) -> Result<Address, AddressError> {
    Address::from_str(s).map_err(|e| AddressError::InvalidAddress(e.to_string()))
}

/// Format 20 address bytes as an EIP-55 checksummed string.
///
/// # Errors
///
/// Returns `AddressError::InvalidByteLength` if bytes length is not 20.
pub fn to_checksum_address_bytes(bytes: &[u8]) -> Result<String, AddressError> {
    if bytes.len() != 20 {
        return Err(AddressError::InvalidByteLength(bytes.len()));
    }
    let address = Address::from_slice(bytes);
    Ok(address_to_checksum_string(&address))
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_from_str() {
        let result = to_checksum_address_str("0x66f9664f97f2b50f62d13ea064982f936de76657");
        assert!(result.is_ok());
        let checksummed = result.expect("valid address should checksum successfully");
        assert!(checksummed.contains(|c: char| c.is_ascii_uppercase()));
        assert!(checksummed.contains(|c: char| c.is_ascii_lowercase()));
    }

    #[test]
    fn test_checksum_from_str_invalid() {
        let result = to_checksum_address_str("not-an-address");
        assert!(matches!(result, Err(AddressError::InvalidAddress(_))));
    }

    #[test]
    fn test_checksum_from_bytes() {
        let bytes: [u8; 20] = [
            0x66, 0xf9, 0x66, 0x4f, 0x97, 0xf2, 0xb5, 0x0f, 0x62, 0xd1, 0x3e, 0xa0, 0x64, 0x98,
            0x2f, 0x93, 0x6d, 0xe7, 0x66, 0x57,
        ];
        let result = to_checksum_address_bytes(&bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn test_checksum_from_bytes_wrong_length() {
        let bytes: [u8; 10] = [0x66; 10];
        let result = to_checksum_address_bytes(&bytes);
        assert!(matches!(result, Err(AddressError::InvalidByteLength(10))));
    }

    #[test]
    fn test_parse_address_valid() {
        let result = parse_address("0x66f9664f97f2b50f62d13ea064982f936de76657");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_address_invalid() {
        let result = parse_address("not-an-address");
        assert!(matches!(result, Err(AddressError::InvalidAddress(_))));
    }
}
