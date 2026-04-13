//! Address utility functions.
//!
//! Provides functions for Ethereum address manipulation.
//!
//! # Architecture
//!
//! This module is the single source of truth for address formatting.
//! All address-to-string conversions should use `address_to_checksum_string()`
//! or the higher-level `to_checksum_address_*` functions.

use crate::errors::AddressError;
use alloy::primitives::Address;
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
};
use std::str::FromStr;

/// Convert an Alloy `Address` to a checksummed string.
///
/// This is the canonical implementation used throughout the codebase.
/// All address-to-string conversions should use this function.
///
/// # Arguments
///
/// * `address` - The Alloy Address to format
///
/// # Returns
///
/// The EIP-55 checksummed address string.
#[must_use]
pub fn address_to_checksum_string(address: &Address) -> String {
    address.to_checksum(None)
}

/// Internal implementation for checksumming a hex address string.
///
/// # Arguments
///
/// * `addr_str` - A hex string representing an Ethereum address
///
/// # Returns
///
/// The checksummed address string
///
/// # Errors
///
/// Returns `AddressError::InvalidAddress` if the string is not a valid hex address.
pub fn to_checksum_address_str(addr_str: &str) -> Result<String, AddressError> {
    let addr =
        parse_address(addr_str)?;
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

/// Internal implementation for checksumming address bytes.
///
/// # Arguments
///
/// * `bytes` - A 20-byte slice representing an Ethereum address
///
/// # Returns
///
/// The checksummed address string
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

/// Generates an EIP-55 checksummed address from the input.
///
/// Accepts either a hex string or a 20-byte sequence and returns
/// a checksummed Ethereum address.
///
/// # Arguments
///
/// * `address` - A Python `str` (hex) or `bytes` (20 bytes) representing an address
///
/// # Returns
///
/// A checksummed address string with uppercase/lowercase letters
///
/// # Errors
///
/// Returns `PyValueError` if:
/// - The string is not a valid hex address
/// - The bytes are not exactly 20 bytes long
///
/// Returns `PyTypeError` if the input is not a string or bytes
///
/// # Architecture
///
/// This PyO3-exposed function is a thin wrapper around the internal implementations
/// `to_checksum_address_str` and `to_checksum_address_bytes`. This separation enables:
/// - Unit testing without `PyO3` dependencies
/// - Reuse in non-Python Rust code
/// - Cleaner error types (`AddressError` vs `PyErr`)
///
/// # Example
///
/// ```
/// use degenbot_rs::address_utils::to_checksum_address_str;
///
/// let result = to_checksum_address_str("0x66f9664f97f2b50f62d13ea064982f936de76657");
/// match result {
///     Ok(checksummed) => println!("Checksummed: {}", checksummed),
///     Err(e) => eprintln!("Error: {}", e),
/// }
/// ```
#[pyfunction(signature = (address))]
pub fn to_checksum_address(address: &Bound<'_, PyAny>) -> PyResult<String> {
    // GIL detachment is of minor benefit here: the checksum computation is
    // trivial (Address::from_str + EIP-55 hashing, microseconds), so the
    // overhead of releasing and reacquiring the GIL would exceed the cost
    // of simply computing the result while holding it.
    if let Ok(s) = address.extract::<&str>() {
        return to_checksum_address_str(s)
            .map_err(|e| PyErr::new::<PyValueError, _>(e.to_string()));
    }

    if let Ok(bytes) = address.extract::<&[u8]>() {
        return to_checksum_address_bytes(bytes)
            .map_err(|e| PyErr::new::<PyValueError, _>(e.to_string()));
    }

    Err(PyErr::new::<PyTypeError, _>(
        "Address must be string or bytes",
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_from_str() {
        let result = to_checksum_address_str("0x66f9664f97f2b50f62d13ea064982f936de76657");
        assert!(result.is_ok());
        let checksummed = result.expect("valid address should checksum successfully");
        // Verify it's properly checksummed (has mixed case)
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
