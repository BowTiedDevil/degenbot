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
/// use degenbot_core::hex_utils::decode_hex;
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
    // Avoid allocating an intermediate String for the common case (even length).
    if stripped.len() % 2 == 1 {
        let mut s = String::with_capacity(stripped.len() + 1);
        s.push('0');
        s.push_str(stripped);
        alloy::hex::decode(&s).map_err(|e| HexError::InvalidHex(e.to_string()))
    } else {
        alloy::hex::decode(stripped).map_err(|e| HexError::InvalidHex(e.to_string()))
    }
}

// NOTE (TD2 / B1): encoding was a hand-wheel on alloy::hex::encode_prefixed;
// call sites now go through alloy directly. decode_32byte_hex (a length check
// over hex::decode) had no in-workspace callers and is deleted. What remains
// is the semantics alloy rejects: optional "0x"/"0X" prefix + odd-length
// leading-zero padding.
