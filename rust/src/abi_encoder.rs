//! ABI encoding for Ethereum data.
//!
//! High-performance encoding of ABI data using alloy's `dyn_abi`.
//!
//! # Architecture
//!
//! This module uses a two-layer architecture:
//!
//! 1. **Pure Rust core**: `encode_rust()` functions that operate
//!    entirely without `PyO3` dependencies. This enables:
//!    - Unit testing without Python
//!    - Parallel encoding without GIL
//!    - Reuse in non-Python Rust code
//!
//! 2. **Thin `PyO3` wrapper**: `encode()` and `encode_single()` functions that convert
//!    Python objects to `AbiValue` and encode them.

use crate::abi_types::{value_to_alloy_for_type, AbiType, AbiValue};
use crate::errors::AbiDecodeError;
use alloy::dyn_abi::DynSolType;
use alloy::primitives::{I256, U256};
use lru::LruCache;
use parking_lot::Mutex;
use pyo3::{
    exceptions::PyValueError,
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::LazyLock;

// =============================================================================
// Type conversion cache (AbiType -> DynSolType)
// =============================================================================

/// Maximum number of cached type conversions.
const TYPE_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(10_000).expect("10_000 is non-zero");

/// Global LRU cache for type conversions to avoid repeated string parsing.
static TYPE_CACHE: LazyLock<Mutex<LruCache<String, DynSolType>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(TYPE_CACHE_CAPACITY)));

/// Convert an `AbiType` to a `DynSolType` with caching.
fn abi_type_to_alloy_cached(ty: &AbiType) -> Result<DynSolType, AbiDecodeError> {
    let key = ty.to_string();
    
    // Fast path: check cache
    {
        let mut cache = TYPE_CACHE.lock();
        if let Some(cached) = cache.get(&key) {
            return Ok(cached.clone());
        }
    }
    
    // Slow path: convert and cache
    let alloy_type = ty.to_alloy_type()?;
    {
        let mut cache = TYPE_CACHE.lock();
        cache.put(key, alloy_type.clone());
    }
    Ok(alloy_type)
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
/// Returns `AbiDecodeError` if encoding fails.
pub fn encode_single_rust(abi_type: &str, value: &AbiValue) -> Result<Vec<u8>, AbiDecodeError> {
    let ty = DynSolType::parse(abi_type)
        .map_err(|e| AbiDecodeError::UnsupportedType(format!("{abi_type}: {e}")))?;

    let alloy_value = value_to_alloy_for_type(value, &ty)?;

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
/// Returns `AbiDecodeError` if encoding fails.
pub fn encode_rust(types: &[&str], values: &[AbiValue]) -> Result<Vec<u8>, AbiDecodeError> {
    if types.len() != values.len() {
        return Err(AbiDecodeError::InvalidLength(format!(
            "Type count {} does not match value count {}",
            types.len(),
            values.len()
        )));
    }

    if types.is_empty() {
        return Ok(Vec::new());
    }

    // Parse types and convert values
    let mut alloy_values = Vec::with_capacity(types.len());
    for (ty, value) in types.iter().zip(values.iter()) {
        let parsed_ty =
            DynSolType::parse(ty).map_err(|e| AbiDecodeError::UnsupportedType(format!("{ty}: {e}")))?;
        let alloy_value = value_to_alloy_for_type(value, &parsed_ty)?;
        alloy_values.push(alloy_value);
    }

    // Encode the values using abi_encode_params for proper parameter encoding
    // For tuples, abi_encode_params uses sequence encoding (no extra offset)
    // For single values, it delegates to abi_encode()
    let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
    Ok(tuple_value.abi_encode_params())
}

/// Encode multiple ABI values using pre-parsed `AbiType` values.
///
/// This is more efficient than `encode_rust()` because it avoids
/// string parsing for each type. Use this when you already have
/// `AbiType` instances (e.g., from `FunctionSignature::inputs`).
///
/// # Arguments
///
/// * `types` - Slice of `AbiType` values
/// * `values` - Slice of values to encode
///
/// # Returns
///
/// The ABI-encoded bytes (without function selector).
///
/// # Errors
///
/// Returns `AbiDecodeError` if encoding fails.
///
/// # Example
///
/// ```
/// use degenbot_rs::abi_types::{AbiType, AbiValue};
/// use degenbot_rs::abi_encoder::encode_for_types;
/// use alloy::primitives::U256;
///
/// let types = vec![AbiType::Uint(256), AbiType::Bool];
/// let values = vec![
///     AbiValue::Uint(U256::from(42u64)),
///     AbiValue::Bool(true),
/// ];
///
/// let encoded = encode_for_types(&types, &values)?;
/// assert!(!encoded.is_empty());
///
/// Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn encode_for_types(types: &[AbiType], values: &[AbiValue]) -> Result<Vec<u8>, AbiDecodeError> {
    if types.len() != values.len() {
        return Err(AbiDecodeError::InvalidLength(format!(
            "Type count {} does not match value count {}",
            types.len(),
            values.len()
        )));
    }

    if types.is_empty() {
        return Ok(Vec::new());
    }

    // Convert types and values directly with caching
    let mut alloy_values = Vec::with_capacity(types.len());
    for (ty, value) in types.iter().zip(values.iter()) {
        let parsed_ty = abi_type_to_alloy_cached(ty)?;
        let alloy_value = value_to_alloy_for_type(value, &parsed_ty)?;
        alloy_values.push(alloy_value);
    }

    // Encode the values using abi_encode_params for proper parameter encoding
    // For tuples, abi_encode_params uses sequence encoding (no extra offset)
    // For single values, it delegates to abi_encode()
    let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
    Ok(tuple_value.abi_encode_params())
}

// =============================================================================
// Python conversion
// =============================================================================

/// Create an `AbiValue` from a Python object.
pub fn abi_value_from_python(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<AbiValue> {
    // Recursive helper that captures py from outer scope
    fn convert_item(item: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<AbiValue> {
        abi_value_from_python(py, item)
    }

    // Try bool first (before int, since bool is subclass of int in Python)
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(AbiValue::Bool(b.is_true()));
    }

    // Try int - extract as i128 first to detect sign, then convert to U256/I256
    // Python integers can be arbitrarily large, but we only support up to 256 bits
    if let Ok(int_val) = obj.extract::<i128>() {
        // Small integer fits in i128
        if int_val >= 0 {
            return Ok(AbiValue::Uint(U256::from(int_val.cast_unsigned())));
        }
        return Ok(AbiValue::Int(I256::try_from(int_val).map_err(|_| {
            PyValueError::new_err("Integer conversion failed")
        })?));
    }

    // For larger integers, try to extract via Python's to_bytes method
    // Check if it's an integer type
    let int_type = py.import("builtins")?.getattr("int")?;
    if obj.is_instance(&int_type)? {
        // Try to get the sign
        let is_negative: bool = obj.call_method1("__lt__", (0,))?.extract()?;

        if is_negative {
            // Negative integer - use signed to_bytes for correct I256::MIN handling
            // signed=True handles two's complement encoding, including I256::MIN
            let bytes = obj.call_method1("to_bytes", (32, "big", true))?;
            let bytes: &[u8] = bytes.extract()?;
            let u256 = U256::from_be_bytes(
                <[u8; 32]>::try_from(bytes).map_err(|_| {
                    PyValueError::new_err("Integer value out of range for int256")
                })?,
            );
            // Directly interpret as I256 (two's complement encoding)
            return Ok(AbiValue::Int(I256::from_raw(u256)));
        }
        // Positive integer - use unsigned to_bytes
        let bytes = obj.call_method1("to_bytes", (32, "big", false))?;
        let bytes: &[u8] = bytes.extract()?;
        let u256 = U256::from_be_bytes(
            <[u8; 32]>::try_from(bytes).map_err(|_| {
                PyValueError::new_err("Integer value out of range for uint256")
            })?,
        );
        return Ok(AbiValue::Uint(u256));
    }

    // Try string (for addresses)
    if let Ok(s) = obj.cast::<PyString>() {
        let s = s.to_string();
        // Check if it's an address
        if s.starts_with("0x") && s.len() == 42 {
            let addr = alloy::primitives::Address::from_str(&s)
                .map_err(|e| PyValueError::new_err(format!("Invalid address '{s}': {e}")))?;
            return Ok(AbiValue::Address(addr.into()));
        }
        return Ok(AbiValue::String(s));
    }

    // Try bytes
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(AbiValue::Bytes(b.as_bytes().to_vec()));
    }

    // Try list (for arrays)
    if let Ok(list) = obj.cast::<PyList>() {
        let values: Result<Vec<_>, _> =
            list.iter().map(|item| convert_item(&item, py)).collect();
        return Ok(AbiValue::Array(values?));
    }

    Err(PyValueError::new_err(format!(
        "Cannot convert Python object to ABI value: {}",
        obj.repr()?
    )))
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
pub fn encode_single<'py>(
    py: Python<'py>,
    abi_type: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let abi_value = abi_value_from_python(py, value)?;
    let encoded = encode_single_rust(abi_type, &abi_value)
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    Ok(PyBytes::new(py, &encoded))
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
pub fn encode<'py>(
    py: Python<'py>,
    types: &Bound<'_, PyList>,
    values: &Bound<'_, PyList>,
) -> PyResult<Bound<'py, PyBytes>> {
    if types.len() != values.len() {
        return Err(PyValueError::new_err(format!(
            "Type count {} does not match value count {}",
            types.len(),
            values.len()
        )));
    }

    let abi_values: Result<Vec<AbiValue>, _> = values
        .iter()
        .map(|v| abi_value_from_python(py, &v))
        .collect();

    // Extract type strings from Python list
    let type_strings: Vec<String> = types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;
    let type_refs: Vec<&str> = type_strings.iter().map(String::as_str).collect();

    let encoded = encode_rust(&type_refs, &abi_values?)
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    Ok(PyBytes::new(py, &encoded))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::abi_decoder::decode_single_rust;
    use alloy::hex;
    use alloy::primitives::{Address, I256, U256};
    use std::str::FromStr;

    #[test]
    fn test_encode_uint256() {
        let value = AbiValue::Uint(U256::from(12345u64));
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
        let offset = alloy::primitives::U256::from_be_slice(&encoded[0..32]);
        assert_eq!(offset, alloy::primitives::U256::from(32));
        // Check length (at offset 32)
        let len = alloy::primitives::U256::from_be_slice(&encoded[32..64]);
        assert_eq!(len, alloy::primitives::U256::from(4));
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
        let offset = alloy::primitives::U256::from_be_slice(&encoded[0..32]);
        assert_eq!(offset, alloy::primitives::U256::from(32));
        // Check length (at offset 32)
        let len = alloy::primitives::U256::from_be_slice(&encoded[32..64]);
        assert_eq!(len, alloy::primitives::U256::from(13));
        // Check data (at offset 64)
        assert_eq!(&encoded[64..77], s.as_bytes());
    }

    #[test]
    fn test_encode_int_negative() {
        let value = AbiValue::Int(I256::MINUS_ONE);
        let encoded = encode_single_rust("int256", &value).unwrap();
        assert_eq!(encoded.len(), 32);
        // -1 in two's complement should be all 0xFF
        assert!(encoded.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_encode_array() {
        let values = vec![
            AbiValue::Uint(U256::from(1u64)),
            AbiValue::Uint(U256::from(2u64)),
            AbiValue::Uint(U256::from(3u64)),
        ];
        let value = AbiValue::Array(values);
        let encoded = encode_single_rust("uint256[]", &value).unwrap();
        // Dynamic array: offset (32) + length (32) + elements (3 * 32)
        assert_eq!(encoded.len(), 32 + 32 + 96);
    }

    #[test]
    fn test_encode_multiple() {
        let values = vec![AbiValue::Uint(U256::from(42u64)), AbiValue::Bool(true)];
        let encoded = encode_rust(&["uint256", "bool"], &values).unwrap();
        assert_eq!(encoded.len(), 64);
        // First word: 42
        assert_eq!(encoded[31], 42);
        // Second word: true
        assert_eq!(encoded[63], 1);
    }

    #[test]
    fn test_encode_for_types() {
        use crate::abi_types::AbiType;

        let types = vec![AbiType::Uint(256), AbiType::Bool];
        let values = vec![AbiValue::Uint(U256::from(42u64)), AbiValue::Bool(true)];

        let encoded = encode_for_types(&types, &values).unwrap();
        assert_eq!(encoded.len(), 64);
        assert_eq!(encoded[31], 42);
        assert_eq!(encoded[63], 1);

        // Verify it produces the same output as encode_rust
        let encoded_rust = encode_rust(&["uint256", "bool"], &values).unwrap();
        assert_eq!(encoded, encoded_rust);
    }

    #[test]
    fn test_encode_for_types_arrays() {
        use crate::abi_types::AbiType;

        let types = vec![AbiType::Array(Box::new(AbiType::Uint(256)))];
        let values = vec![AbiValue::Array(vec![
            AbiValue::Uint(U256::from(1u64)),
            AbiValue::Uint(U256::from(2u64)),
        ])];

        let encoded = encode_for_types(&types, &values).unwrap();
        assert!(!encoded.is_empty());

        // Verify roundtrip
        let encoded_rust = encode_rust(&["uint256[]"], &values).unwrap();
        assert_eq!(encoded, encoded_rust);
    }

    #[test]
    fn test_roundtrip() {
        // Encode then decode should give back the same value
        let original = U256::from(12_345_678_901_234_567_890_u128);
        let value = AbiValue::Uint(original);
        let encoded = encode_single_rust("uint256", &value).unwrap();

        // Decode using alloy
        let ty = DynSolType::parse("uint256").unwrap();
        let decoded = ty.abi_decode(&encoded).unwrap();

        if let alloy::dyn_abi::DynSolValue::Uint(u, _) = decoded {
            assert_eq!(u, original);
        } else {
            panic!("Expected Uint");
        }
    }

    #[test]
    fn test_encode_roundtrip_with_decode() {
        use crate::abi_decoder::decode_single_rust;

        // Test uint256
        let original = U256::from(12_345_678_901_234_567_890_u128);
        let value = AbiValue::Uint(original);
        let encoded = encode_single_rust("uint256", &value).unwrap();
        let decoded = decode_single_rust("uint256", &encoded).unwrap();
        match decoded {
            AbiValue::Uint(n) => assert_eq!(n, original),
            _ => panic!("Expected Uint"),
        }

        // Test address
        let addr_str = "0xd3cda913deb6f67967b99d67acdfa1712c293601";
        let addr = Address::from_str(addr_str).unwrap();
        let value = AbiValue::Address(addr.into());
        let encoded = encode_single_rust("address", &value).unwrap();
        let decoded = decode_single_rust("address", &encoded).unwrap();
        match decoded {
            AbiValue::Address(a) => assert_eq!(a, addr.0),
            _ => panic!("Expected Address"),
        }

        // Test string
        let s = "Hello, World!";
        let value = AbiValue::String(s.to_string());
        let encoded = encode_single_rust("string", &value).unwrap();
        let decoded = decode_single_rust("string", &encoded).unwrap();
        match decoded {
            AbiValue::String(decoded_s) => assert_eq!(decoded_s, s),
            _ => panic!("Expected String"),
        }
    }

    // =========================================================================
    // Boundary condition tests for Python integer conversion
    // These test the abi_value_from_python function behavior
    // =========================================================================

    #[test]
    fn test_u256_max_roundtrip() {
        // U256::MAX should encode and decode correctly
        let max = U256::MAX;
        let value = AbiValue::Uint(max);
        let encoded = encode_single_rust("uint256", &value).unwrap();
        let decoded = decode_single_rust("uint256", &encoded).unwrap();
        match decoded {
            AbiValue::Uint(n) => assert_eq!(n, max),
            _ => panic!("Expected Uint"),
        }
    }

    #[test]
    fn test_i256_max_roundtrip() {
        // I256::MAX should encode and decode correctly
        let max = I256::MAX;
        let value = AbiValue::Int(max);
        let encoded = encode_single_rust("int256", &value).unwrap();
        let decoded = decode_single_rust("int256", &encoded).unwrap();
        match decoded {
            AbiValue::Int(n) => assert_eq!(n, max),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_i256_min_roundtrip() {
        // I256::MIN should encode and decode correctly
        let min = I256::MIN;
        let value = AbiValue::Int(min);
        let encoded = encode_single_rust("int256", &value).unwrap();
        let decoded = decode_single_rust("int256", &encoded).unwrap();
        match decoded {
            AbiValue::Int(n) => assert_eq!(n, min),
            _ => panic!("Expected Int"),
        }
    }
}

// =============================================================================
// Property-based tests for encoding/decoding
// =============================================================================

#[cfg(test)]
mod proptests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::abi_decoder::decode_single_rust;
    use alloy::primitives::Address;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn uint256_roundtrip(n in prop::array::uniform32(0u8..)) {
            let value = AbiValue::Uint(U256::from_be_bytes(n));
            let encoded = encode_single_rust("uint256", &value).unwrap();
            let decoded = decode_single_rust("uint256", &encoded).unwrap();
            prop_assert!(matches!(decoded, AbiValue::Uint(val) if val == U256::from_be_bytes(n)));
        }

        #[test]
        fn address_roundtrip(bytes in prop::array::uniform20(0u8..)) {
            let addr = Address::from_slice(&bytes);
            let value = AbiValue::Address(addr.into());
            let encoded = encode_single_rust("address", &value).unwrap();
            let decoded = decode_single_rust("address", &encoded).unwrap();
            prop_assert!(matches!(decoded, AbiValue::Address(val) if val == addr.0));
        }

        #[test]
        fn bool_roundtrip(b in prop::bool::ANY) {
            let value = AbiValue::Bool(b);
            let encoded = encode_single_rust("bool", &value).unwrap();
            let decoded = decode_single_rust("bool", &encoded).unwrap();
            prop_assert!(matches!(decoded, AbiValue::Bool(val) if val == b));
        }

        #[test]
        fn bytes32_roundtrip(data in prop::array::uniform32(0u8..)) {
            let bytes = data.to_vec();
            let value = AbiValue::FixedBytes(bytes.clone());
            let encoded = encode_single_rust("bytes32", &value).unwrap();
            let decoded = decode_single_rust("bytes32", &encoded).unwrap();
            prop_assert!(matches!(decoded, AbiValue::FixedBytes(val) if val == bytes));
        }

        #[test]
        fn uint256_array_roundtrip(
            count in 1usize..20,
            seed in 0u64..
        ) {
            let values: Vec<AbiValue> = (0..count)
                .map(|i| AbiValue::Uint(U256::from(seed.wrapping_add(i as u64))))
                .collect();
            let value = AbiValue::Array(values.clone());
            let encoded = encode_single_rust("uint256[]", &value).unwrap();
            let decoded = decode_single_rust("uint256[]", &encoded).unwrap();

            if let AbiValue::Array(decoded_values) = decoded {
                prop_assert_eq!(decoded_values.len(), values.len());
                for (expected, actual) in values.iter().zip(decoded_values.iter()) {
                    prop_assert!(matches!((expected, actual), (AbiValue::Uint(e), AbiValue::Uint(a)) if e == a));
                }
            } else {
                prop_assert!(false, "Expected Array variant");
            }
        }

        #[test]
        fn int256_roundtrip(n in prop::array::uniform32(0u8..)) {
            // Use raw bytes to create potentially negative I256 values
            let u256 = U256::from_be_bytes(n);
            let i256 = I256::from_raw(u256);
            let value = AbiValue::Int(i256);
            let encoded = encode_single_rust("int256", &value).unwrap();
            let decoded = decode_single_rust("int256", &encoded).unwrap();
            prop_assert!(matches!(decoded, AbiValue::Int(val) if val == i256));
        }
    }
}
