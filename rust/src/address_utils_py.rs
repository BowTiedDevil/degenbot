//! `PyO3` bindings for address utilities.

use crate::address_utils::{to_checksum_address_bytes, to_checksum_address_str};
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
};

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
