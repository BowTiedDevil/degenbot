//! ABI encoding for Ethereum data.
//!
//! High-performance encoding of ABI data using alloy's `dyn_abi`.
//!
//! # Architecture
//!
//! This module uses a two-layer architecture:
//!
//! 1. **Pure Rust core**: `AbiValue` enum and `encode_rust()` functions that operate
//!    entirely without `PyO3` dependencies. This enables:
//!    - Unit testing without Python
//!    - Parallel encoding without GIL
//!    - Reuse in non-Python Rust code
//!
//! 2. **Thin `PyO3` wrapper**: `encode()` and `encode_single()` functions that convert
//!    Python objects to `AbiValue` and encode them.

use alloy::dyn_abi::DynSolType;
use alloy::hex;
use alloy::primitives::{Address, I256, U256};
use num_bigint::{BigInt, BigUint};
use num_traits::{Signed, Zero};
use pyo3::{
    exceptions::PyValueError,
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};
use std::str::FromStr;

// =============================================================================
// Errors
// =============================================================================

/// Errors that can occur during ABI encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiEncodeError {
    /// Invalid type string.
    InvalidType(String),
    /// Invalid value for the given type.
    InvalidValue {
        abi_type: String,
        value: String,
        reason: String,
    },
    /// Argument count mismatch.
    ArgumentMismatch { expected: usize, actual: usize },
    /// Unsupported type.
    UnsupportedType(String),
}

impl std::fmt::Display for AbiEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidType(t) => write!(f, "Invalid ABI type: {t}"),
            Self::InvalidValue {
                abi_type,
                value,
                reason,
            } => {
                write!(f, "Invalid value for type '{abi_type}': {value} - {reason}")
            }
            Self::ArgumentMismatch { expected, actual } => {
                write!(
                    f,
                    "Argument count mismatch: expected {expected}, got {actual}"
                )
            }
            Self::UnsupportedType(t) => write!(f, "Unsupported type: {t}"),
        }
    }
}

impl std::error::Error for AbiEncodeError {}

// =============================================================================
// AbiValue - Pure Rust representation of ABI values for encoding
// =============================================================================

/// Represents an ABI value for encoding.
///
/// This enum captures all possible ABI types without any Python dependencies.
#[derive(Clone, Debug, PartialEq)]
pub enum AbiValue {
    /// Ethereum address (20 bytes)
    Address([u8; 20]),
    /// Boolean value
    Bool(bool),
    /// Fixed-size bytes (bytes1-bytes32)
    FixedBytes(Vec<u8>),
    /// Dynamic bytes
    Bytes(Vec<u8>),
    /// Unsigned integer
    Uint(BigUint),
    /// Signed integer
    Int(BigInt),
    /// String
    String(String),
    /// Array of values
    Array(Vec<Self>),
}

impl AbiValue {
    /// Convert an `AbiValue` to a `DynSolValue` for encoding.
    fn to_alloy(&self) -> Result<alloy::dyn_abi::DynSolValue, AbiEncodeError> {
        use alloy::dyn_abi::DynSolValue;

        match self {
            Self::Address(addr) => {
                let addr = Address::from_slice(addr);
                Ok(DynSolValue::Address(addr))
            }
            Self::Bool(b) => Ok(DynSolValue::Bool(*b)),
            Self::FixedBytes(bytes) => {
                let size = bytes.len();
                if size > 32 {
                    return Err(AbiEncodeError::InvalidValue {
                        abi_type: format!("bytes{size}"),
                        value: hex::encode(bytes),
                        reason: "bytesN requires at most 32 bytes".to_string(),
                    });
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
            Self::Uint(n) => {
                let bytes = n.to_bytes_be();
                if bytes.len() > 32 {
                    return Err(AbiEncodeError::InvalidValue {
                        abi_type: "uint256".to_string(),
                        value: n.to_string(),
                        reason: "Value exceeds uint256 max".to_string(),
                    });
                }
                // Pad to 32 bytes
                let mut arr = [0u8; 32];
                arr[32 - bytes.len()..].copy_from_slice(&bytes);
                let u256 = U256::from_be_bytes(arr);
                Ok(DynSolValue::Uint(u256, 256))
            }
            Self::Int(n) => {
                let (sign, bytes) = n.to_bytes_be();
                if bytes.len() > 32 {
                    return Err(AbiEncodeError::InvalidValue {
                        abi_type: "int256".to_string(),
                        value: n.to_string(),
                        reason: "Value exceeds int256 range".to_string(),
                    });
                }
                // Convert to I256 with proper sign extension
                let i256 = if sign == num_bigint::Sign::Minus && !n.is_zero() {
                    // For negative numbers, we need two's complement
                    let abs_val = n.abs();
                    // to_bytes_be on positive BigInt returns (Plus, bytes)
                    let (_, abs_bytes) = abs_val.to_bytes_be();
                    let mut arr = [0u8; 32];
                    if abs_bytes.len() <= 32 {
                        arr[32 - abs_bytes.len()..].copy_from_slice(&abs_bytes);
                    }
                    let abs_u256 = U256::from_be_bytes(arr);
                    // Two's complement: invert and add 1
                    let inverted = !abs_u256;
                    let neg_u256 = inverted + U256::from(1);
                    I256::from_raw(neg_u256)
                } else {
                    let mut arr = [0u8; 32];
                    arr[32 - bytes.len()..].copy_from_slice(&bytes);
                    I256::from_raw(U256::from_be_bytes(arr))
                };
                Ok(DynSolValue::Int(i256, 256))
            }
            Self::Array(values) => {
                let alloy_values: Result<Vec<_>, _> = values.iter().map(Self::to_alloy).collect();
                Ok(DynSolValue::Array(alloy_values?))
            }
        }
    }

    /// Create from a Python object.
    fn from_python(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        // Use a recursive helper that captures py from outer scope
        fn convert_item(item: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<AbiValue> {
            AbiValue::from_python(py, item)
        }

        // Try bool first (before int, since bool is subclass of int in Python)
        if let Ok(b) = obj.cast::<PyBool>() {
            return Ok(Self::Bool(b.is_true()));
        }

        // Try int - try BigUint first for positive values, then BigInt
        if let Ok(big_uint) = obj.extract::<BigUint>() {
            return Ok(Self::Uint(big_uint));
        }
        if let Ok(big_int) = obj.extract::<BigInt>() {
            // BigInt succeeded but BigUint failed, so it's negative
            return Ok(Self::Int(big_int));
        }

        // Try string (for addresses)
        if let Ok(s) = obj.cast::<PyString>() {
            let s = s.to_string();
            // Check if it's an address
            if s.starts_with("0x") && s.len() == 42 {
                let addr = Address::from_str(&s)
                    .map_err(|e| PyValueError::new_err(format!("Invalid address '{s}': {e}")))?;
                return Ok(Self::Address(addr.into()));
            }
            return Ok(Self::String(s));
        }

        // Try bytes
        if let Ok(b) = obj.cast::<PyBytes>() {
            return Ok(Self::Bytes(b.as_bytes().to_vec()));
        }

        // Try list (for arrays)
        if let Ok(list) = obj.cast::<PyList>() {
            let values: Result<Vec<Self>, _> =
                list.iter().map(|item| convert_item(&item, py)).collect();
            return Ok(Self::Array(values?));
        }

        Err(PyValueError::new_err(format!(
            "Cannot convert Python object to ABI value: {}",
            obj.repr()?
        )))
    }
}

// =============================================================================
// Pure Rust encoding functions
// =============================================================================

/// Encode a single ABI value (pure Rust).
///
/// # Arguments
///
/// * `abi_type` - ABI type string (e.g., "uint256", "address", "bytes")
/// * `value` - The value to encode
///
/// # Returns
///
/// The ABI-encoded bytes.
///
/// # Errors
///
/// Returns `AbiEncodeError` if encoding fails.
pub fn encode_single_rust(abi_type: &str, value: &AbiValue) -> Result<Vec<u8>, AbiEncodeError> {
    let ty = DynSolType::parse(abi_type)
        .map_err(|e| AbiEncodeError::InvalidType(format!("{abi_type}: {e}")))?;

    let mut alloy_value = value.to_alloy()?;

    // Handle Bytes -> FixedBytes conversion for bytesN types
    if let DynSolType::FixedBytes(size) = &ty {
        if let AbiValue::Bytes(bytes) = value {
            // Convert dynamic bytes to fixed bytes
            if bytes.len() != *size {
                return Err(AbiEncodeError::InvalidValue {
                    abi_type: abi_type.to_string(),
                    value: hex::encode(bytes),
                    reason: format!(
                        "bytes{size} requires exactly {size} bytes, got {}",
                        bytes.len()
                    ),
                });
            }
            let mut arr = [0u8; 32];
            arr[..*size].copy_from_slice(bytes);
            alloy_value = alloy::dyn_abi::DynSolValue::FixedBytes(
                alloy::primitives::FixedBytes::<32>::new(arr),
                *size,
            );
        }
    }

    // Verify type compatibility
    if !ty.matches(&alloy_value) {
        return Err(AbiEncodeError::InvalidValue {
            abi_type: abi_type.to_string(),
            value: format!("{value:?}"),
            reason: "Type mismatch".to_string(),
        });
    }

    // Encode the value using abi_encode()
    Ok(alloy_value.abi_encode())
}

/// Encode multiple ABI values (pure Rust).
///
/// # Arguments
///
/// * `types` - Slice of ABI type strings
/// * `values` - Slice of values to encode
///
/// # Returns
///
/// The ABI-encoded bytes (without function selector).
///
/// # Errors
///
/// Returns `AbiEncodeError` if encoding fails.
pub fn encode_rust(types: &[&str], values: &[AbiValue]) -> Result<Vec<u8>, AbiEncodeError> {
    if types.len() != values.len() {
        return Err(AbiEncodeError::ArgumentMismatch {
            expected: types.len(),
            actual: values.len(),
        });
    }

    if types.is_empty() {
        return Ok(Vec::new());
    }

    // Parse types and convert values
    let mut alloy_values = Vec::with_capacity(types.len());
    for (ty, value) in types.iter().zip(values.iter()) {
        let parsed_ty =
            DynSolType::parse(ty).map_err(|e| AbiEncodeError::InvalidType(format!("{ty}: {e}")))?;
        let alloy_value = value.to_alloy()?;

        // Verify type compatibility
        if !parsed_ty.matches(&alloy_value) {
            return Err(AbiEncodeError::InvalidValue {
                abi_type: ty.to_string(),
                value: format!("{value:?}"),
                reason: "Type mismatch".to_string(),
            });
        }

        alloy_values.push(alloy_value);
    }

    // Encode as a tuple (multiple parameters are encoded as a tuple)
    let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
    Ok(tuple_value.abi_encode())
}

// =============================================================================
// PyO3-exposed functions
// =============================================================================

/// Encode a single ABI value.
///
/// # Arguments
///
/// * `abi_type` - ABI type string (e.g., "uint256", "address", "bytes")
/// * `value` - Python value to encode (int, bool, str, bytes, or list)
///
/// # Returns
///
/// The ABI-encoded bytes.
#[pyfunction]
pub fn encode_single(
    py: Python<'_>,
    abi_type: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<Vec<u8>> {
    let abi_value = AbiValue::from_python(py, value)?;
    encode_single_rust(abi_type, &abi_value).map_err(|e| PyValueError::new_err(format!("{e}")))
}

/// Encode multiple ABI values.
///
/// # Arguments
///
/// * `types` - List of ABI type strings
/// * `values` - List of Python values to encode
///
/// # Returns
///
/// The ABI-encoded bytes.
#[pyfunction]
#[pyo3(signature = (types, values))]
pub fn encode(
    py: Python<'_>,
    types: &Bound<'_, PyList>,
    values: &Bound<'_, PyList>,
) -> PyResult<Vec<u8>> {
    if types.len() != values.len() {
        return Err(PyValueError::new_err(format!(
            "Type count {} does not match value count {}",
            types.len(),
            values.len()
        )));
    }

    let abi_values: Result<Vec<AbiValue>, _> = values
        .iter()
        .map(|v| AbiValue::from_python(py, &v))
        .collect();

    // Extract type strings from Python list
    let type_strings: Vec<String> = types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;
    let type_refs: Vec<&str> = type_strings.iter().map(String::as_str).collect();

    encode_rust(&type_refs, &abi_values?).map_err(|e| PyValueError::new_err(format!("{e}")))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn test_encode_uint256() {
        let value = AbiValue::Uint(BigUint::from(12345u64));
        let encoded = encode_single_rust("uint256", &value).unwrap();
        assert_eq!(encoded.len(), 32);
        // Value should be in the last bytes
        assert_eq!(encoded[30], 0x30);
        assert_eq!(encoded[31], 0x39);
    }

    #[test]
    fn test_encode_address() {
        let addr_str = "0xd3cda913deb6f67967b99d67acdfa1712c293601";
        let addr = Address::from_str(addr_str).unwrap();
        let value = AbiValue::Address(addr.into());
        let encoded = encode_single_rust("address", &value).unwrap();
        assert_eq!(encoded.len(), 32);
        // Address should be in the last 20 bytes
        assert_eq!(
            &encoded[12..],
            hex::decode("d3cda913deb6f67967b99d67acdfa1712c293601").unwrap()
        );
    }

    #[test]
    fn test_encode_bool() {
        let value_true = AbiValue::Bool(true);
        let encoded = encode_single_rust("bool", &value_true).unwrap();
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded[31], 1);

        let value_false = AbiValue::Bool(false);
        let encoded = encode_single_rust("bool", &value_false).unwrap();
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded[31], 0);
    }

    #[test]
    fn test_encode_bytes32() {
        let bytes: Vec<u8> = (0..32).collect();
        let value = AbiValue::FixedBytes(bytes.clone());
        let encoded = encode_single_rust("bytes32", &value).unwrap();
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded, bytes);
    }

    #[test]
    fn test_encode_dynamic_bytes() {
        let bytes = hex::decode("deadbeef").unwrap();
        let value = AbiValue::Bytes(bytes.clone());
        let encoded = encode_single_rust("bytes", &value).unwrap();
        // Dynamic bytes as single value: offset (32) + length (32) + data + padding
        // Total: 32 + 32 + 32 = 96 bytes for 4 bytes of data
        assert_eq!(encoded.len(), 96);
        // Check offset (should point to the length field, which is at byte 32)
        let offset = U256::from_be_slice(&encoded[0..32]);
        assert_eq!(offset, U256::from(32));
        // Check length (at offset 32)
        let len = U256::from_be_slice(&encoded[32..64]);
        assert_eq!(len, U256::from(4));
        // Check data (at offset 64)
        assert_eq!(&encoded[64..68], &bytes);
    }

    #[test]
    fn test_encode_string() {
        let s = "Hello, World!";
        let value = AbiValue::String(s.to_string());
        let encoded = encode_single_rust("string", &value).unwrap();
        // Dynamic string as single value: offset (32) + length (32) + data + padding
        // Total: 32 + 32 + 32 = 96 bytes for 13 bytes of data
        assert_eq!(encoded.len(), 96);
        // Check offset (should point to the length field, which is at byte 32)
        let offset = U256::from_be_slice(&encoded[0..32]);
        assert_eq!(offset, U256::from(32));
        // Check length (at offset 32)
        let len = U256::from_be_slice(&encoded[32..64]);
        assert_eq!(len, U256::from(13));
        // Check data (at offset 64)
        assert_eq!(&encoded[64..77], s.as_bytes());
    }

    #[test]
    fn test_encode_int_negative() {
        let value = AbiValue::Int(BigInt::from(-1));
        let encoded = encode_single_rust("int256", &value).unwrap();
        assert_eq!(encoded.len(), 32);
        // -1 in two's complement should be all 0xFF
        assert!(encoded.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_encode_array() {
        let values = vec![
            AbiValue::Uint(BigUint::from(1u64)),
            AbiValue::Uint(BigUint::from(2u64)),
            AbiValue::Uint(BigUint::from(3u64)),
        ];
        let value = AbiValue::Array(values);
        let encoded = encode_single_rust("uint256[]", &value).unwrap();
        // Dynamic array: offset (32) + length (32) + elements (3 * 32)
        assert_eq!(encoded.len(), 32 + 32 + 96);
    }

    #[test]
    fn test_encode_multiple() {
        let values = vec![AbiValue::Uint(BigUint::from(42u64)), AbiValue::Bool(true)];
        let encoded = encode_rust(&["uint256", "bool"], &values).unwrap();
        assert_eq!(encoded.len(), 64);
        // First word: 42
        assert_eq!(encoded[31], 42);
        // Second word: true
        assert_eq!(encoded[63], 1);
    }

    #[test]
    fn test_roundtrip() {
        // Encode then decode should give back the same value
        let original = BigUint::from(12_345_678_901_234_567_890_u128);
        let value = AbiValue::Uint(original.clone());
        let encoded = encode_single_rust("uint256", &value).unwrap();

        // Decode using alloy
        let ty = DynSolType::parse("uint256").unwrap();
        let decoded = ty.abi_decode(&encoded).unwrap();

        if let alloy::dyn_abi::DynSolValue::Uint(u, _) = decoded {
            let decoded_uint = BigUint::from_bytes_be(&u.to_be_bytes_vec());
            assert_eq!(decoded_uint, original);
        } else {
            panic!("Expected Uint");
        }
    }

    #[test]
    fn test_encode_roundtrip_with_decode() {
        use crate::abi_decoder::{decode_single_rust, DecodedValue};

        // Test uint256
        let original = BigUint::from(12_345_678_901_234_567_890_u128);
        let value = AbiValue::Uint(original.clone());
        let encoded = encode_single_rust("uint256", &value).unwrap();
        let decoded = decode_single_rust("uint256", &encoded).unwrap();
        match decoded {
            DecodedValue::Uint(n) => assert_eq!(n, original),
            _ => panic!("Expected Uint"),
        }

        // Test address
        let addr_str = "0xd3cda913deb6f67967b99d67acdfa1712c293601";
        let addr = Address::from_str(addr_str).unwrap();
        let value = AbiValue::Address(addr.into());
        let encoded = encode_single_rust("address", &value).unwrap();
        let decoded = decode_single_rust("address", &encoded).unwrap();
        match decoded {
            DecodedValue::Address(a) => assert_eq!(a, addr.0),
            _ => panic!("Expected Address"),
        }

        // Test string
        let s = "Hello, World!";
        let value = AbiValue::String(s.to_string());
        let encoded = encode_single_rust("string", &value).unwrap();
        let decoded = decode_single_rust("string", &encoded).unwrap();
        match decoded {
            DecodedValue::String(decoded_s) => assert_eq!(decoded_s, s),
            _ => panic!("Expected String"),
        }
    }
}
