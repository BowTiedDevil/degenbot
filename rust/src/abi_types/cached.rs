//! Cached ABI types for batch encoding/decoding.
//!
//! Provides `CachedAbiTypes` for pre-parsed type sets and
//! `value_to_alloy_for_type` for type-aware value conversion.
//!
//! This module also provides the global type cache (`get_cached_types`)
//! which is the single caching point for both encoding and decoding.

use crate::abi_types::type_::AbiType;
use crate::abi_types::value::AbiValue;
use crate::errors::AbiDecodeError;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::LazyLock;

// =============================================================================
// Global type cache
// =============================================================================

/// Maximum number of cached type sets.
const CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(10_000).expect("10_000 is non-zero");

/// Global LRU cache for parsed ABI types.
/// Key is the actual type strings (not a hash) to avoid collision risk.
pub(crate) static TYPE_CACHE: LazyLock<Mutex<LruCache<Vec<String>, CachedAbiTypes>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(CACHE_CAPACITY)));

/// Get or create cached types for the given type strings.
///
/// This function checks the global cache first, and only parses
/// if the types haven't been seen before. Uses LRU eviction when cache is full.
///
/// This is the single caching point for both encoding and decoding.
/// The cache is keyed by the actual type strings (not a hash) to avoid collision risk.
///
/// # Errors
///
/// Returns `AbiDecodeError` if any type string is invalid.
pub fn get_cached_types(types: &[&str]) -> Result<CachedAbiTypes, AbiDecodeError> {
    let key: Vec<String> = types.iter().map(std::string::ToString::to_string).collect();

    // Fast path: check cache
    {
        let mut cache = TYPE_CACHE.lock();
        if let Some(cached) = cache.get(&key) {
            return Ok(cached.clone());
        }
    }

    // Slow path: parse and cache
    let cached = CachedAbiTypes::new(types)?;
    {
        let mut cache = TYPE_CACHE.lock();
        cache.put(key, cached.clone());
    }
    Ok(cached)
}

/// Pre-parsed ABI types for high-performance batch encoding/decoding.
///
/// When processing thousands of values with the same type signature
/// (e.g., decoding Transfer events from historical blocks), parsing the
/// type string each time adds significant overhead. This struct caches
/// the parsed types for reuse.
///
/// # Example
///
/// ```
/// use degenbot_rs::abi_types::CachedAbiTypes;
///
/// // Parse once
/// let cached = CachedAbiTypes::new(&["address", "address", "uint256"])?;
///
/// // Encode values
/// use degenbot_rs::abi_types::AbiValue;
/// use alloy::primitives::U256;
/// let values = vec![
///     AbiValue::Address([0u8; 20]),
///     AbiValue::Address([0u8; 20]),
///     AbiValue::Uint(U256::from(100u64)),
/// ];
/// let encoded = cached.encode(&values)?;
///
/// // Decode many times without parsing overhead
/// let decoded = cached.decode(&encoded)?;
/// assert_eq!(decoded.len(), 3);
///
/// Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct CachedAbiTypes {
    /// The individual parsed types
    types: Vec<AbiType>,
    /// The individual cached alloy types (parallel to `types`)
    alloy_types: Vec<alloy::dyn_abi::DynSolType>,
    /// The tuple type for encoding/decoding multiple values
    tuple_type: alloy::dyn_abi::DynSolType,
    /// Cached type strings for debugging/display
    type_strings: Vec<String>,
}

impl CachedAbiTypes {
    /// Create a new cached type set from type strings.
    ///
    /// Parses all types upfront and caches them for reuse.
    ///
    /// # Arguments
    ///
    /// * `types` - Slice of ABI type strings (e.g., `["address", "uint256"]`)
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any type string is invalid.
    ///
    /// # Example
    ///
    /// ```
    /// use degenbot_rs::abi_types::CachedAbiTypes;
    ///
    /// let cached = CachedAbiTypes::new(&["address", "uint256"])?;
    ///
    /// Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(types: &[&str]) -> Result<Self, AbiDecodeError> {
        if types.is_empty() {
            return Err(AbiDecodeError::EmptyTypesList);
        }

        let mut parsed_types = Vec::with_capacity(types.len());
        let mut alloy_types = Vec::with_capacity(types.len());
        let mut type_strings = Vec::with_capacity(types.len());

        for ty in types {
            let abi_type = AbiType::parse(ty).map_err(|e| {
                AbiDecodeError::UnsupportedType(format!("Invalid type '{ty}': {e}"))
            })?;
            let alloy_type = abi_type.to_alloy_type()?;
            parsed_types.push(abi_type);
            alloy_types.push(alloy_type);
            type_strings.push(ty.to_string());
        }

        let tuple_type = alloy::dyn_abi::DynSolType::Tuple(alloy_types.clone());

        Ok(Self {
            types: parsed_types,
            alloy_types,
            tuple_type,
            type_strings,
        })
    }

    /// Create from already-parsed `AbiType` values.
    ///
    /// Use this when you already have `AbiType` instances
    /// (e.g., from `FunctionSignature::inputs`).
    ///
    /// # Arguments
    ///
    /// * `types` - Slice of `AbiType` values
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if conversion to alloy types fails.
    pub fn from_abi_types(types: &[AbiType]) -> Result<Self, AbiDecodeError> {
        if types.is_empty() {
            return Err(AbiDecodeError::EmptyTypesList);
        }

        let mut alloy_types = Vec::with_capacity(types.len());
        let type_strings: Vec<String> = types.iter().map(ToString::to_string).collect();

        for ty in types {
            let alloy_type = ty.to_alloy_type()?;
            alloy_types.push(alloy_type);
        }

        let tuple_type = alloy::dyn_abi::DynSolType::Tuple(alloy_types.clone());

        Ok(Self {
            types: types.to_vec(),
            alloy_types,
            tuple_type,
            type_strings,
        })
    }

    /// Decode ABI-encoded data using cached types.
    ///
    /// This is significantly faster than calling `decode_rust()` for each
    /// decode when processing many values with the same type signature.
    ///
    /// # Arguments
    ///
    /// * `data` - ABI-encoded bytes
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if decoding fails.
    #[inline]
    pub fn decode(&self, data: &[u8]) -> Result<Vec<AbiValue>, AbiDecodeError> {
        if data.is_empty() {
            return Err(AbiDecodeError::EmptyData);
        }

        let decoded = self
            .tuple_type
            .abi_decode_params(data)
            .map_err(|e| AbiDecodeError::InvalidOffset(format!("Decoding failed: {e}")))?;

        let values = match decoded {
            alloy::dyn_abi::DynSolValue::Tuple(vals) => vals,
            other => vec![other],
        };

        values.into_iter().map(AbiValue::from_alloy).collect()
    }

    /// Decode multiple ABI-encoded values using cached types.
    ///
    /// This is the batch version of `decode()`, optimized for processing
    /// thousands of values with the same type signature.
    ///
    /// # Arguments
    ///
    /// * `data_items` - Slice of ABI-encoded byte slices
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any decode fails. Partial results are
    /// not returned on error.
    ///
    /// # Example
    ///
    /// ```
    /// use degenbot_rs::abi_types::CachedAbiTypes;
    /// use degenbot_rs::abi_types::AbiValue;
    /// use alloy::primitives::U256;
    ///
    /// let cached = CachedAbiTypes::new(&["address", "uint256"])?;
    ///
    /// // Encode test data
    /// let values = vec![AbiValue::Address([0u8; 20]), AbiValue::Uint(U256::from(1u64))];
    /// let encoded = cached.encode(&values)?;
    ///
    /// // Decode batch
    /// let encoded_ref: &[u8] = &encoded;
    /// let decoded_batch = cached.decode_batch(&[encoded_ref])?;
    /// assert_eq!(decoded_batch.len(), 1);
    ///
    /// Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn decode_batch(&self, data_items: &[&[u8]]) -> Result<Vec<Vec<AbiValue>>, AbiDecodeError> {
        data_items.iter().map(|data| self.decode(data)).collect()
    }

    /// Encode values using cached types.
    ///
    /// # Arguments
    ///
    /// * `values` - Slice of `AbiValue` to encode
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if encoding fails or value count doesn't match type count.
    #[inline]
    pub fn encode(&self, values: &[AbiValue]) -> Result<Vec<u8>, AbiDecodeError> {
        if values.len() != self.types.len() {
            return Err(AbiDecodeError::InvalidLength(format!(
                "Type count {} does not match value count {}",
                self.types.len(),
                values.len()
            )));
        }

        // Use cached alloy types instead of re-deriving from AbiType on every call
        let mut alloy_values = Vec::with_capacity(self.types.len());
        for (alloy_type, value) in self.alloy_types.iter().zip(values.iter()) {
            let alloy_value = value_to_alloy_for_type(value, alloy_type)?;
            alloy_values.push(alloy_value);
        }

        let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
        Ok(tuple_value.abi_encode_params())
    }

    /// Encode multiple value sets using cached types.
    ///
    /// # Arguments
    ///
    /// * `values_batch` - Slice of value slices to encode
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any encode fails.
    #[inline]
    pub fn encode_batch(&self, values_batch: &[&[AbiValue]]) -> Result<Vec<Vec<u8>>, AbiDecodeError> {
        values_batch.iter().map(|values| self.encode(values)).collect()
    }

    /// Get the cached type strings.
    #[must_use]
    #[inline]
    pub const fn type_strings(&self) -> &Vec<String> {
        &self.type_strings
    }

    /// Get the number of types.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.types.len()
    }

    /// Check if there are no types.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Convert an `AbiValue` to a `DynSolValue` for a specific expected type.
///
/// Handles special cases like `FixedBytes` and `FixedArray`.
pub fn value_to_alloy_for_type(
    value: &AbiValue,
    ty: &alloy::dyn_abi::DynSolType,
) -> Result<alloy::dyn_abi::DynSolValue, AbiDecodeError> {
    use alloy::dyn_abi::DynSolType;

    match (ty, value) {
        // Handle FixedBytes conversion from Bytes
        (DynSolType::FixedBytes(size), AbiValue::Bytes(bytes)) => {
            if bytes.len() != *size {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "bytes{size} requires exactly {size} bytes, got {}",
                    bytes.len()
                )));
            }
            let mut arr = [0u8; 32];
            arr[..*size].copy_from_slice(bytes);
            Ok(alloy::dyn_abi::DynSolValue::FixedBytes(
                alloy::primitives::FixedBytes::<32>::new(arr),
                *size,
            ))
        }

        // Handle FixedArray conversion
        (DynSolType::FixedArray(inner_ty, expected_size), AbiValue::Array(values)) => {
            if values.len() != *expected_size {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "Fixed array of size {expected_size} requires exactly {expected_size} elements, got {}",
                    values.len()
                )));
            }
            let alloy_values: Result<Vec<_>, _> = values
                .iter()
                .map(|v| value_to_alloy_for_type(v, inner_ty))
                .collect();
            Ok(alloy::dyn_abi::DynSolValue::FixedArray(alloy_values?))
        }

        // For all other cases, use the standard conversion
        _ => {
            let alloy_value = value.to_alloy()?;
            if !ty.matches(&alloy_value) {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "Type mismatch: {ty} does not match {value:?}"
                )));
            }
            Ok(alloy_value)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use alloy::primitives::U256;

    #[test]
    fn test_cached_abi_types_new() {
        let cached = CachedAbiTypes::new(&["address", "uint256", "bool"]).unwrap();
        assert_eq!(cached.len(), 3);
        assert!(!cached.is_empty());
        assert_eq!(cached.type_strings(), &["address", "uint256", "bool"]);
    }

    #[test]
    fn test_cached_abi_types_empty() {
        let result = CachedAbiTypes::new(&[]);
        assert!(matches!(result, Err(AbiDecodeError::EmptyTypesList)));
    }

    #[test]
    fn test_cached_abi_types_invalid_type() {
        let result = CachedAbiTypes::new(&["address", "invalid_type"]);
        assert!(matches!(result, Err(AbiDecodeError::UnsupportedType(_))));
    }

    #[test]
    fn test_cached_abi_types_from_abi_types() {
        let types = vec![AbiType::Address, AbiType::Uint(256), AbiType::Bool];
        let cached = CachedAbiTypes::from_abi_types(&types).unwrap();
        assert_eq!(cached.len(), 3);
    }

    #[test]
    fn test_cached_abi_types_decode_encode_roundtrip() {
        let cached = CachedAbiTypes::new(&["address", "address", "uint256"]).unwrap();

        let values = vec![
            AbiValue::Address([0x11; 20]),
            AbiValue::Address([0x22; 20]),
            AbiValue::Uint(U256::from(1000u64)),
        ];

        let encoded = cached.encode(&values).unwrap();
        let decoded = cached.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 3);

        match &decoded[0] {
            AbiValue::Address(addr) => assert_eq!(addr, &[0x11; 20]),
            _ => panic!("Expected Address"),
        }
        match &decoded[1] {
            AbiValue::Address(addr) => assert_eq!(addr, &[0x22; 20]),
            _ => panic!("Expected Address"),
        }
        match &decoded[2] {
            AbiValue::Uint(n) => assert_eq!(*n, U256::from(1000u64)),
            _ => panic!("Expected Uint"),
        }
    }

    #[test]
    fn test_cached_abi_types_decode_batch() {
        let cached = CachedAbiTypes::new(&["uint256", "bool"]).unwrap();

        let mut encoded_items: Vec<Vec<u8>> = Vec::new();
        for i in 0..3u64 {
            let values = vec![AbiValue::Uint(U256::from(i * 10)), AbiValue::Bool(i % 2 == 0)];
            encoded_items.push(cached.encode(&values).unwrap());
        }

        let refs: Vec<&[u8]> = encoded_items.iter().map(Vec::as_slice).collect();
        let decoded_batch = cached.decode_batch(&refs).unwrap();
        assert_eq!(decoded_batch.len(), 3);

        for (i, decoded) in decoded_batch.iter().enumerate() {
            match &decoded[0] {
                AbiValue::Uint(n) => assert_eq!(*n, U256::from(i as u64 * 10)),
                _ => panic!("Expected Uint"),
            }
            match &decoded[1] {
                AbiValue::Bool(b) => assert_eq!(*b, i % 2 == 0),
                _ => panic!("Expected Bool"),
            }
        }
    }

    #[test]
    fn test_cached_abi_types_encode_batch() {
        let cached = CachedAbiTypes::new(&["address", "uint256"]).unwrap();

        let values_batch: Vec<Vec<AbiValue>> = (0u8..5)
            .map(|i| {
                vec![
                    AbiValue::Address([i; 20]),
                    AbiValue::Uint(U256::from(u64::from(i) * 100)),
                ]
            })
            .collect();

        let refs: Vec<&[AbiValue]> = values_batch.iter().map(Vec::as_slice).collect();
        let encoded_batch = cached.encode_batch(&refs).unwrap();
        assert_eq!(encoded_batch.len(), 5);

        for (i, encoded) in encoded_batch.iter().enumerate() {
            let decoded = cached.decode(encoded).unwrap();
            match &decoded[1] {
                AbiValue::Uint(n) => assert_eq!(*n, U256::from(i as u64 * 100)),
                _ => panic!("Expected Uint"),
            }
        }
    }

    #[test]
    fn test_cached_abi_types_wrong_value_count() {
        let cached = CachedAbiTypes::new(&["address", "uint256"]).unwrap();

        let values = vec![AbiValue::Address([0u8; 20])];
        let result = cached.encode(&values);
        assert!(matches!(result, Err(AbiDecodeError::InvalidLength(_))));
    }

    #[test]
    fn test_cached_abi_types_matches_decode_rust() {
        use crate::abi_decoder::decode_rust;

        let type_strings = ["address", "uint256", "bool"];
        let cached = CachedAbiTypes::new(&type_strings).unwrap();

        let values = vec![
            AbiValue::Address([0xab; 20]),
            AbiValue::Uint(U256::from(999_888_777u64)),
            AbiValue::Bool(true),
        ];
        let encoded = cached.encode(&values).unwrap();

        let decoded_cached = cached.decode(&encoded).unwrap();
        let decoded_rust = decode_rust(&type_strings, &encoded).unwrap();

        assert_eq!(decoded_cached, decoded_rust);
    }
}
