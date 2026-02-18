//! Address utility functions.
//!
//! Provides functions for Ethereum address manipulation.

use alloy_primitives::Address;
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
};
use std::str::FromStr;

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
/// # Example
///
/// ```
/// use alloy_primitives::Address;
/// use std::str::FromStr;
///
/// let addr = Address::from_str("0x66f9664f97f2b50f62d13ea064982f936de76657").unwrap();
/// let checksummed = addr.to_checksum(None);
/// println!("Checksummed: {}", checksummed);
/// ```
#[pyfunction(signature = (address))]
pub fn to_checksum_address(py: Python<'_>, address: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = address.extract::<&str>() {
        let addr = Address::from_str(s)
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("Invalid address: {e}")))?;
        return Ok(py.detach(|| addr.to_checksum(None)));
    }

    if let Ok(bytes) = address.extract::<&[u8]>() {
        if bytes.len() != 20 {
            return Err(PyErr::new::<PyValueError, _>("Address must be 20 bytes"));
        }
        let address = Address::from_slice(bytes);
        return Ok(py.detach(|| address.to_checksum(None)));
    }

    Err(PyErr::new::<PyTypeError, _>(
        "Address must be string or bytes",
    ))
}
