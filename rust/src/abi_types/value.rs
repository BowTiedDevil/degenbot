//! ABI value representation and conversions.
//!
//! Provides `AbiValue` enum for representing encoded/decoded ABI values,
//! plus integer parsing helpers and string-to-value conversion.

use crate::errors::{AbiDecodeError, ContractError};
use crate::abi_types::type_::AbiType;
use alloy::dyn_abi::DynSolValue;
use alloy::hex;
use alloy::primitives::{Address, I256, U256};
use std::fmt;
use std::str::FromStr;

/// Decode a hex string (with optional "0x" prefix) to bytes.
///
/// Handles odd-length strings by padding with a leading zero,
/// which is the canonical behavior for Ethereum hex strings.
///
/// # Errors
///
/// Returns `Err` if the string contains invalid hex characters.
pub(crate) fn decode_hex(hex_str: &str) -> Result<Vec<u8>, hex::FromHexError> {
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
    alloy::hex::decode(&padded)
}

/// Represents an ABI value for encoding/decoding.
///
/// This enum captures all possible ABI types without any Python dependencies,
/// enabling pure Rust testing and GIL-free processing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiValue {
    /// Ethereum address (20 bytes)
    Address([u8; 20]),
    /// Boolean value
    Bool(bool),
    /// Fixed-size bytes (bytes1-bytes32)
    FixedBytes(Vec<u8>),
    /// Dynamic bytes
    Bytes(Vec<u8>),
    /// Unsigned integer (up to 256 bits)
    Uint(U256),
    /// Signed integer (up to 256 bits)
    Int(I256),
    /// String
    String(String),
    /// Array of values
    Array(Vec<Self>),
}

impl AbiValue {
    /// Convert an `AbiValue` to a `DynSolValue` for encoding.
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if conversion fails (e.g., value too large).
    pub fn to_alloy(&self) -> Result<DynSolValue, AbiDecodeError> {
        match self {
            Self::Address(addr) => {
                let addr = Address::from_slice(addr);
                Ok(DynSolValue::Address(addr))
            }
            Self::Bool(b) => Ok(DynSolValue::Bool(*b)),
            Self::FixedBytes(bytes) => {
                let size = bytes.len();
                if size > 32 {
                    return Err(AbiDecodeError::UnsupportedType(format!(
                        "bytes{size} requires at most 32 bytes, got {size}"
                    )));
                }
                let mut arr = [0u8; 32];
                arr[..size].copy_from_slice(bytes);
                Ok(DynSolValue::FixedBytes(
                    alloy::primitives::FixedBytes::<32>::new(arr),
                    size,
                ))
            }
            Self::Bytes(bytes) => Ok(DynSolValue::Bytes(bytes.clone())),
            Self::String(s) => Ok(DynSolValue::String(s.clone())),
            Self::Uint(n) => Ok(DynSolValue::Uint(*n, 256)),
            Self::Int(n) => Ok(DynSolValue::Int(*n, 256)),
            Self::Array(values) => {
                let alloy_values: Result<Vec<_>, _> = values.iter().map(Self::to_alloy).collect();
                Ok(DynSolValue::Array(alloy_values?))
            }
        }
    }

    /// Convert a `DynSolValue` from alloy to `AbiValue`.
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if the alloy type is unsupported.
    pub fn from_alloy(value: DynSolValue) -> Result<Self, AbiDecodeError> {
        match value {
            DynSolValue::Address(addr) => Ok(Self::Address(addr.into())),
            DynSolValue::Bool(b) => Ok(Self::Bool(b)),
            DynSolValue::Uint(u, _bits) => Ok(Self::Uint(u)),
            DynSolValue::Int(i, _bits) => Ok(Self::Int(i)),
            DynSolValue::FixedBytes(fb, size) => Ok(Self::FixedBytes(fb[..size].to_vec())),
            DynSolValue::Bytes(b) => Ok(Self::Bytes(b)),
            DynSolValue::String(s) => Ok(Self::String(s)),
            DynSolValue::Array(arr) | DynSolValue::FixedArray(arr) => {
                let values: Result<Vec<Self>, _> = arr.into_iter().map(Self::from_alloy).collect();
                Ok(Self::Array(values?))
            }
            DynSolValue::Tuple(vals) => {
                let values: Result<Vec<Self>, _> = vals.into_iter().map(Self::from_alloy).collect();
                Ok(Self::Array(values?))
            }
            DynSolValue::Function(_) => Err(AbiDecodeError::UnsupportedType(
                "function type not supported".to_string(),
            )),
        }
    }

    /// Parse a string argument into an `AbiValue` based on the expected type.
    ///
    /// Used by `contract.rs` to convert string arguments from Python into
    /// typed values for encoding.
    ///
    /// # Errors
    ///
    /// Returns `ContractError` if the string cannot be parsed for the given type.
    pub fn from_str_arg(abi_type: &AbiType, arg: &str) -> Result<Self, ContractError> {
        let trimmed = arg.trim();
        match abi_type {
            AbiType::Address => {
                let addr =
                    Address::from_str(trimmed).map_err(|_| ContractError::InvalidAddress {
                        address: trimmed.to_string(),
                        reason: "Invalid address format".to_string(),
                    })?;
                Ok(Self::Address(addr.into()))
            }
            AbiType::Bool => {
                let value = trimmed.to_lowercase() == "true" || trimmed == "1";
                Ok(Self::Bool(value))
            }
            AbiType::Uint(_) => {
                let uint_val = parse_uint256_with_hex_prefix(trimmed).map_err(|e| {
                    ContractError::InvalidAbi {
                        message: format!("Invalid uint value '{arg}': {e}"),
                    }
                })?;
                Ok(Self::Uint(uint_val))
            }
            AbiType::Int(_) => {
                let int_val = parse_int256_with_hex_prefix(trimmed).map_err(|e| {
                    ContractError::InvalidAbi {
                        message: format!("Invalid int value '{arg}': {e}"),
                    }
                })?;
                Ok(Self::Int(int_val))
            }
            AbiType::FixedBytes(size) => {
                let bytes = decode_hex(trimmed).map_err(|_| ContractError::InvalidAbi {
                    message: format!("Invalid hex value for bytes{size}: {arg}"),
                })?;
                if bytes.len() != *size {
                    return Err(ContractError::InvalidAbi {
                        message: format!(
                            "bytes{size} requires exactly {size} bytes, got {}",
                            bytes.len()
                        ),
                    });
                }
                Ok(Self::FixedBytes(bytes))
            }
            AbiType::Bytes => {
                let bytes = decode_hex(trimmed).map_err(|_| ContractError::InvalidAbi {
                    message: format!("Invalid hex value for bytes: {arg}"),
                })?;
                Ok(Self::Bytes(bytes))
            }
            AbiType::String => Ok(Self::String(trimmed.to_string())),
            AbiType::Array(element_type) => {
                let elements = parse_json_array(trimmed)?;
                let values: Result<Vec<_>, _> = elements
                    .iter()
                    .map(|elem| Self::from_str_arg(element_type, elem))
                    .collect();
                Ok(Self::Array(values?))
            }
            AbiType::FixedArray(element_type, size) => {
                let elements = parse_json_array(trimmed)?;
                if elements.len() != *size {
                    return Err(ContractError::InvalidAbi {
                        message: format!(
                            "Fixed array of size {size} requires exactly {size} elements, got {}",
                            elements.len()
                        ),
                    });
                }
                let values: Result<Vec<_>, _> = elements
                    .iter()
                    .map(|elem| Self::from_str_arg(element_type, elem))
                    .collect();
                Ok(Self::Array(values?))
            }
        }
    }

    /// Convert this value to a string for contract return values.
    ///
    /// Used by `contract.rs` to format decoded values as strings for Python.
    #[must_use]
    pub fn to_contract_string(&self) -> String {
        match self {
            Self::Address(addr) => format!("0x{}", hex::encode(addr)),
            Self::Bool(b) => b.to_string(),
            Self::FixedBytes(bytes) | Self::Bytes(bytes) => format!("0x{}", hex::encode(bytes)),
            Self::Uint(n) => n.to_string(),
            Self::Int(n) => n.to_string(),
            Self::String(s) => s.clone(),
            Self::Array(values) => {
                let elements: Vec<String> = values.iter().map(Self::to_contract_string).collect();
                format!("[{}]", elements.join(", "))
            }
        }
    }
}

/// Parse a uint256 value from a string, handling hex prefix.
pub(crate) fn parse_uint256_with_hex_prefix(s: &str) -> Result<U256, ParseIntError> {
    let s = s.trim();
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .map_or_else(
            || U256::from_str_radix(s, 10).map_err(|_| ParseIntError::InvalidDecimal),
            |hex_str| U256::from_str_radix(hex_str, 16).map_err(|_| ParseIntError::InvalidHex),
        )
}

/// Parse an int256 value from a string, handling hex prefix.
///
/// Handles the tricky boundary cases around `I256::MAX` (2^255 - 1) and `I256::MIN` (-2^255).
/// For positive hex values, any value >= 2^255 is rejected as out of range.
pub(crate) fn parse_int256_with_hex_prefix(s: &str) -> Result<I256, ParseIntError> {
    let s = s.trim();

    let max_positive = (U256::from(1u8) << 255) - U256::from(1u8);

    if let Some(abs_str) = s.strip_prefix('-') {
        if let Some(hex_str) = abs_str.strip_prefix("0x").or_else(|| abs_str.strip_prefix("0X")) {
            let abs_val = U256::from_str_radix(hex_str, 16)
                .map_err(|_| ParseIntError::InvalidHex)?;

            let min_abs = U256::from(1u8) << 255;
            if abs_val == min_abs {
                return Ok(I256::MIN);
            }

            if abs_val > max_positive + U256::from(1u8) {
                return Err(ParseIntError::InvalidHex);
            }

            Ok(I256::from_raw(abs_val).wrapping_neg())
        } else {
            I256::from_str(s).map_err(|_| ParseIntError::InvalidDecimal)
        }
    } else if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let uval = U256::from_str_radix(hex_str, 16).map_err(|_| ParseIntError::InvalidHex)?;

        if uval > max_positive {
            return Err(ParseIntError::InvalidHex);
        }

        Ok(I256::from_raw(uval))
    } else {
        I256::from_str(s).map_err(|_| ParseIntError::InvalidDecimal)
    }
}

/// Error parsing an integer from string.
#[derive(Debug, Clone)]
pub enum ParseIntError {
    /// Invalid hex format
    InvalidHex,
    /// Invalid decimal format
    InvalidDecimal,
}

impl fmt::Display for ParseIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHex => write!(f, "invalid hex format"),
            Self::InvalidDecimal => write!(f, "invalid decimal format"),
        }
    }
}

impl std::error::Error for ParseIntError {}

/// Deprecated alias for `ParseIntError`.
#[deprecated(note = "Use `ParseIntError` instead")]
pub type ParseU256Error = ParseIntError;

/// Deprecated alias for `ParseIntError`.
#[deprecated(note = "Use `ParseIntError` instead")]
pub type ParseI256Error = ParseIntError;

/// Parse a JSON array string into individual element strings.
///
/// Accepts `["elem1", "elem2"]` format with optional whitespace.
fn parse_json_array(input: &str) -> Result<Vec<&str>, ContractError> {
    let trimmed = input.trim();

    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(ContractError::InvalidAbi {
            message: format!("Array value must be enclosed in square brackets, got: '{input}'"),
        });
    }

    let content = &trimmed[1..trimmed.len() - 1];
    let content = content.trim();

    if content.is_empty() {
        return Ok(Vec::new());
    }

    let mut elements = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, c) in content.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                let element = &content[start..i].trim();
                if !element.is_empty() || i > start {
                    elements.push(*element);
                }
                start = i + 1;
            }
            _ => {}
        }
    }

    let last_element = &content[start..].trim();
    if !last_element.is_empty() || start < content.len() {
        elements.push(*last_element);
    }

    Ok(elements)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // Test for P0.2: I256 hex parsing bug with large positive values
    #[test]
    fn test_parse_int256_large_positive_hex() {
        let max_hex = "0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let result = parse_int256_with_hex_prefix(max_hex);
        
        assert!(result.is_ok(), "Should parse successfully: {result:?}");
        let parsed = result.unwrap();
        
        assert!(
            parsed > I256::ZERO,
            "I256::MAX should be positive, but got {parsed}"
        );
        assert_eq!(parsed, I256::MAX);
    }

    #[test]
    fn test_parse_int256_edge_case_near_sign_bit() {
        let val_hex = "0x4000000000000000000000000000000000000000000000000000000000000000";
        let result = parse_int256_with_hex_prefix(val_hex);
        
        assert!(result.is_ok(), "Should parse successfully");
        let parsed = result.unwrap();
        
        assert!(
            parsed > I256::ZERO,
            "Value 2^254 should be positive, but got {parsed}"
        );
        
        let expected = I256::try_from(U256::from(1u8) << 254).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn test_parse_int256_hex_out_of_range_positive_fails() {
        let too_large = "0x8000000000000000000000000000000000000000000000000000000000000000";
        let result = parse_int256_with_hex_prefix(too_large);
        
        assert!(result.is_err(), "Hex value 2^255 should be out of range for positive int256");
    }

    #[test]
    fn test_parse_int256_negative_hex_roundtrip() {
        let negative_hex = "-0x1";
        let result = parse_int256_with_hex_prefix(negative_hex).unwrap();
        assert_eq!(result, I256::MINUS_ONE);
    }

    #[test]
    fn test_parse_int256_max_hex() {
        let max_hex = "0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let result = parse_int256_with_hex_prefix(max_hex).unwrap();
        assert_eq!(result, I256::MAX);
    }

    #[test]
    fn test_parse_int256_min_hex() {
        let min_hex = "-0x8000000000000000000000000000000000000000000000000000000000000000";
        let result = parse_int256_with_hex_prefix(min_hex).unwrap();
        assert_eq!(result, I256::MIN);
    }

    #[test]
    fn test_parse_uint256_max() {
        let max_hex = "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let result = parse_uint256_with_hex_prefix(max_hex).unwrap();
        assert_eq!(result, U256::MAX);
    }

    #[test]
    fn test_parse_uint256_overflow_fails() {
        let overflow = "0x10000000000000000000000000000000000000000000000000000000000000000";
        let result = parse_uint256_with_hex_prefix(overflow);
        assert!(result.is_err(), "U256 overflow should be rejected");
    }

    #[test]
    fn test_parse_uint256_max_decimal() {
        let max_decimal = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        let result = parse_uint256_with_hex_prefix(max_decimal).unwrap();
        assert_eq!(result, U256::MAX);
    }

    #[test]
    fn test_int256_min_from_str_arg() {
        let min_val = "-57896044618658097711785492504343953926634992332820282019728792003956564819968";
        let result = AbiValue::from_str_arg(&AbiType::Int(256), min_val).unwrap();
        match result {
            AbiValue::Int(n) => assert_eq!(n, I256::MIN),
            _ => panic!("Expected Int variant"),
        }
    }

    #[test]
    fn test_int256_min_hex_from_str_arg() {
        let min_hex = "-0x8000000000000000000000000000000000000000000000000000000000000000";
        let result = AbiValue::from_str_arg(&AbiType::Int(256), min_hex).unwrap();
        match result {
            AbiValue::Int(n) => assert_eq!(n, I256::MIN),
            _ => panic!("Expected Int variant"),
        }
    }

    #[test]
    fn test_uint256_max_from_str_arg() {
        let max_hex = "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let result = AbiValue::from_str_arg(&AbiType::Uint(256), max_hex).unwrap();
        match result {
            AbiValue::Uint(n) => assert_eq!(n, U256::MAX),
            _ => panic!("Expected Uint variant"),
        }
    }
}
