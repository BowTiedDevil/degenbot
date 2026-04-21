//! Pure-Rust hex encoding and decoding utilities.
//!
//! These functions have no `PyO3` dependency and can be used from the Rust core
//! without pulling in Python bindings.

/// Error type for hex decoding failures.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum HexError {
    /// Invalid hex string.
    #[error("Invalid hex string: {0}")]
    InvalidHex(String),
}

/// Decode a hex string (with optional "0x" prefix) to bytes.
///
/// Handles odd-length strings by padding with a leading zero.
///
/// # Arguments
///
/// * `hex_str` - Hex string, with or without "0x"/"0X" prefix
///
/// # Returns
///
/// The decoded bytes, or an error if the string is not valid hex.
///
/// # Errors
///
/// Returns `HexError::InvalidHex` if the hex string is invalid.
///
/// # Examples
///
/// ```
/// use degenbot_rs::hex_utils::decode_hex;
///
/// let bytes = decode_hex("0xdeadbeef").unwrap();
/// assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
///
/// // Odd length is padded with a leading zero
/// let bytes = decode_hex("0x123").unwrap();
/// assert_eq!(bytes, vec![0x01, 0x23]);
/// ```
pub fn decode_hex(hex_str: &str) -> Result<Vec<u8>, HexError> {
    let stripped = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let stripped = stripped.strip_prefix("0X").unwrap_or(stripped);
    let padded = if stripped.len() % 2 == 1 {
        let mut s = String::with_capacity(stripped.len() + 1);
        s.push('0');
        s.push_str(stripped);
        s
    } else {
        stripped.to_string()
    };
    alloy::hex::decode(&padded).map_err(|e| HexError::InvalidHex(e.to_string()))
}

/// Encode bytes as a hex string with "0x" prefix.
///
/// # Arguments
///
/// * `bytes` - The bytes to encode
///
/// # Returns
///
/// A hex-encoded string with "0x" prefix.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    let hex = alloy::hex::encode(bytes);
    let mut result = String::with_capacity(2 + hex.len());
    result.push_str("0x");
    result.push_str(&hex);
    result
}
