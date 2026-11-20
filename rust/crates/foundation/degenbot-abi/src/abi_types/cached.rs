use crate::abi_types::type_::AbiType;
use crate::abi_types::value::AbiValue;
use degenbot_core::errors::AbiDecodeError;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};

const CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(10_000).expect("10_000 is non-zero");

/// Interns individual Solidity type strings ("uint256", "address", etc.).
/// ~20 entries, fixed set, no eviction needed.
pub(crate) static TYPE_STR_INTERNER: LazyLock<Mutex<std::collections::HashMap<String, Arc<str>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn intern_type_str(s: &str) -> Arc<str> {
    let mut cache = TYPE_STR_INTERNER.lock();
    if let Some(interned) = cache.get(s) {
        return Arc::clone(interned);
    }
    let arc_str: Arc<str> = Arc::from(s);
    cache.insert(s.to_string(), Arc::clone(&arc_str));
    drop(cache);
    arc_str
}

type TypeCacheKey = Arc<[Arc<str>]>;

/// Keyed by interned type strings, so a hit clones an `Arc` rather than the
/// `DynSolType` tree inside it.
pub(crate) static TYPE_CACHE: LazyLock<Mutex<LruCache<TypeCacheKey, Arc<CachedAbiTypes>>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(CACHE_CAPACITY)));

#[cfg(test)]
pub fn cache_len() -> usize {
    TYPE_CACHE.lock().len()
}

#[cfg(test)]
pub fn cache_clear() {
    TYPE_CACHE.lock().clear();
}

/// Global mutex to serialize tests that depend on the shared type cache.
/// Callers must hold the lock for the duration of cache assertions.
#[cfg(test)]
static CACHE_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[expect(clippy::missing_panics_doc, clippy::unwrap_used)]
pub fn cache_test_lock() -> std::sync::MutexGuard<'static, ()> {
    CACHE_TEST_MUTEX.lock().unwrap()
}

/// The single caching point the string-keyed encode and decode paths share, so a
/// signature is parsed once per process, not once per call.
///
/// Returns `Arc<CachedAbiTypes>`: a hit costs an `Arc` bump, never a deep clone of
/// the `DynSolType` tree.
///
/// # Errors
///
/// Returns `AbiDecodeError` if any type string is invalid.
pub fn get_cached_types(types: &[&str]) -> Result<Arc<CachedAbiTypes>, AbiDecodeError> {
    let key: Arc<[Arc<str>]> = types.iter().map(|&s| intern_type_str(s)).collect();

    let mut cache = TYPE_CACHE.lock();
    if let Some(cached) = cache.get(&key) {
        return Ok(Arc::clone(cached));
    }

    let cached = Arc::new(CachedAbiTypes::new(types)?);
    cache.put(key, Arc::clone(&cached));
    drop(cache);
    Ok(cached)
}

/// Pre-parsed types for batch encode/decode.
///
/// Thousands of values sharing one signature (`Transfer` logs from historical
/// blocks) re-parse the same type strings on every call; this holds the parse
/// result instead.
///
/// # Example
///
/// ```
/// use degenbot_abi::abi_types::cached::CachedAbiTypes;
///
/// // Parse once
/// let cached = CachedAbiTypes::new(&["address", "address", "uint256"])?;
///
/// // Encode values
/// use degenbot_abi::abi_types::value::AbiValue;
/// use alloy::primitives::U256;
/// let values = vec![
///     AbiValue::Address([0u8; 20]),
///     AbiValue::Address([0u8; 20]),
///     AbiValue::Uint(U256::from(100u64), 256),
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
    types: Vec<AbiType>,
    alloy_types: Vec<alloy::dyn_abi::DynSolType>,
    tuple_type: alloy::dyn_abi::DynSolType,
    type_strings: Vec<String>,
}

impl CachedAbiTypes {
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any type string is invalid.
    ///
    /// # Example
    ///
    /// ```
    /// use degenbot_abi::abi_types::cached::CachedAbiTypes;
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

    /// For callers that already hold parsed types (`FunctionSignature::inputs`);
    /// returns `Arc` for consistency with [`get_cached_types`].
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if conversion to alloy types fails.
    pub fn from_abi_types(types: &[AbiType]) -> Result<Arc<Self>, AbiDecodeError> {
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

        Ok(Arc::new(Self {
            types: types.to_vec(),
            alloy_types,
            tuple_type,
            type_strings,
        }))
    }

    /// Significantly faster than `decode_rust()` per value when many values share
    /// one type signature.
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

    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any decode fails. Partial results are
    /// not returned on error.
    ///
    /// # Example
    ///
    /// ```
    /// use degenbot_abi::abi_types::cached::CachedAbiTypes;
    /// use degenbot_abi::abi_types::value::AbiValue;
    /// use alloy::primitives::U256;
    ///
    /// let cached = CachedAbiTypes::new(&["address", "uint256"])?;
    ///
    /// // Encode test data
    /// let values = vec![AbiValue::Address([0u8; 20]), AbiValue::Uint(U256::from(1u64), 256)];
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

        let mut alloy_values = Vec::with_capacity(self.types.len());
        for (alloy_type, value) in self.alloy_types.iter().zip(values.iter()) {
            let alloy_value = value_to_alloy_for_type(value, alloy_type)?;
            alloy_values.push(alloy_value);
        }

        let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
        Ok(tuple_value.abi_encode_params())
    }

    /// Solidity `abi.encodePacked`: each value concatenates in its packed form, with
    /// no word padding and no length prefix for dynamic types. Mirrors
    /// `eth_abi.packed.encode_packed` exactly, coercion path included.
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError` if a value doesn't match its type.
    #[inline]
    pub fn encode_packed(&self, values: &[AbiValue]) -> Result<Vec<u8>, AbiDecodeError> {
        if values.len() != self.types.len() {
            return Err(AbiDecodeError::InvalidLength(format!(
                "Type count {} does not match value count {}",
                self.types.len(),
                values.len()
            )));
        }

        let mut alloy_values = Vec::with_capacity(self.types.len());
        for (alloy_type, value) in self.alloy_types.iter().zip(values.iter()) {
            let alloy_value = value_to_alloy_for_type(value, alloy_type)?;
            alloy_values.push(alloy_value);
        }

        let tuple_value = alloy::dyn_abi::DynSolValue::Tuple(alloy_values);
        Ok(tuple_value.abi_encode_packed())
    }

    /// # Errors
    ///
    /// Returns `AbiDecodeError` if any encode fails.
    #[inline]
    pub fn encode_batch(
        &self,
        values_batch: &[&[AbiValue]],
    ) -> Result<Vec<Vec<u8>>, AbiDecodeError> {
        values_batch
            .iter()
            .map(|values| self.encode(values))
            .collect()
    }

    #[must_use]
    #[inline]
    pub const fn type_strings(&self) -> &Vec<String> {
        &self.type_strings
    }

    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.types.len()
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Coerce `value` into the shape `ty` expects before encoding.
///
/// The cross-type arms mirror `eth_abi`'s permissive encoder, so call sites that
/// worked against the Python implementation keep working.
#[expect(clippy::missing_errors_doc)]
pub fn value_to_alloy_for_type(
    value: &AbiValue,
    ty: &alloy::dyn_abi::DynSolType,
) -> Result<alloy::dyn_abi::DynSolValue, AbiDecodeError> {
    use alloy::dyn_abi::DynSolType;

    match (ty, value) {
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

        // Cross-type coercion: a 20-byte `bytes` value (e.g. `HexBytes` of
        // an address) is accepted as an `address` by `eth_abi`'s permissive
        // encoder. Mirror that here so callers passing `HexBytes(addr)` with
        // type `"address"` don't need to pre-convert.
        (DynSolType::Address, AbiValue::Bytes(bytes)) => {
            if bytes.len() != 20 {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "address requires 20 bytes, got {}",
                    bytes.len()
                )));
            }
            Ok(alloy::dyn_abi::DynSolValue::Address(
                alloy::primitives::Address::from_slice(bytes),
            ))
        }

        (DynSolType::Uint(bits), AbiValue::Uint(n, val_bits)) => {
            // DynSolType constructed from a parsed AbiType should never have
            // bit width 0. If it does, that's a bug in the caller.
            debug_assert!(*bits > 0, "Uint bit width should never be 0");
            let effective_bits = if *bits > 0 { *bits } else { *val_bits };
            Ok(alloy::dyn_abi::DynSolValue::Uint(*n, effective_bits))
        }

        (DynSolType::Int(bits), AbiValue::Int(n, val_bits)) => {
            debug_assert!(*bits > 0, "Int bit width should never be 0");
            let effective_bits = if *bits > 0 { *bits } else { *val_bits };
            Ok(alloy::dyn_abi::DynSolValue::Int(*n, effective_bits))
        }

        // Cross-type coercion: a non-negative Python int is produced by
        // `abi_value_from_python` as `AbiValue::Uint(...)` (it doesn't know
        // the target type). When the requested ABI type is a signed `Int`,
        // widen the value if it fits, so e.g. `encode(["int128"], [0])` works
        // (mirroring `eth_abi`'s permissive coercion).
        (DynSolType::Int(bits), AbiValue::Uint(n, _)) => {
            let signed = alloy::primitives::I256::try_from(*n).map_err(|_| {
                AbiDecodeError::UnsupportedType(format!("value {n} does not fit in int{bits}"))
            })?;
            Ok(alloy::dyn_abi::DynSolValue::Int(signed, *bits))
        }

        // Cross-type coercion: a negative Python int is produced by
        // `abi_value_from_python` as `AbiValue::Int(...)`. Encoding a
        // negative value as a `Uint` ABI type is invalid — reject up front
        // (mirroring `eth_abi`'s behaviour of raising `ValueError`).
        (DynSolType::Uint(bits), AbiValue::Int(n, _)) => {
            if *n < alloy::primitives::I256::ZERO {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "negative value {n} cannot be encoded as uint{bits}"
                )));
            }
            let unsigned = alloy::primitives::U256::try_from(*n).map_err(|_| {
                AbiDecodeError::UnsupportedType(format!("value {n} does not fit in uint{bits}"))
            })?;
            Ok(alloy::dyn_abi::DynSolValue::Uint(unsigned, *bits))
        }

        // Dynamic array: map elements against the item type so that tuples
        // (or dynamic arrays of dynamic arrays) encode to the right
        // DynSolValue shapes.
        (DynSolType::Array(inner_ty), AbiValue::Array(values)) => {
            let alloy_values: Result<Vec<_>, _> = values
                .iter()
                .map(|v| value_to_alloy_for_type(v, inner_ty))
                .collect();
            Ok(alloy::dyn_abi::DynSolValue::Array(alloy_values?))
        }
        // Tuple: a Python list/tuple arrives as AbiValue::Array regardless of
        // the target layout; map component-by-component so tuples (and nested
        // tuples) encode to DynSolValue::Tuple.
        (DynSolType::Tuple(components), AbiValue::Array(values)) => {
            if values.len() != components.len() {
                return Err(AbiDecodeError::UnsupportedType(format!(
                    "Tuple requires exactly {} values, got {}",
                    components.len(),
                    values.len()
                )));
            }
            let alloy_values: Result<Vec<_>, _> = components
                .iter()
                .zip(values.iter())
                .map(|(c, v)| value_to_alloy_for_type(v, c))
                .collect();
            Ok(alloy::dyn_abi::DynSolValue::Tuple(alloy_values?))
        }
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
#[expect(clippy::panic, clippy::unwrap_used)]
mod tests {

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
            AbiValue::Uint(U256::from(1000u64), 256),
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
            AbiValue::Uint(n, bits) => {
                assert_eq!(*n, U256::from(1000u64));
                assert_eq!(*bits, 256);
            }
            _ => panic!("Expected Uint"),
        }
    }

    #[test]
    fn test_cached_abi_types_decode_batch() {
        let cached = CachedAbiTypes::new(&["uint256", "bool"]).unwrap();

        let mut encoded_items: Vec<Vec<u8>> = Vec::new();
        for i in 0..3u64 {
            let values = vec![
                AbiValue::Uint(U256::from(i * 10), 256),
                AbiValue::Bool(i % 2 == 0),
            ];
            encoded_items.push(cached.encode(&values).unwrap());
        }

        let refs: Vec<&[u8]> = encoded_items.iter().map(Vec::as_slice).collect();
        let decoded_batch = cached.decode_batch(&refs).unwrap();
        assert_eq!(decoded_batch.len(), 3);

        for (i, decoded) in decoded_batch.iter().enumerate() {
            match &decoded[0] {
                AbiValue::Uint(n, bits) => {
                    assert_eq!(*n, U256::from(i as u64 * 10));
                    assert_eq!(*bits, 256);
                }
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
                    AbiValue::Uint(U256::from(u64::from(i) * 100), 256),
                ]
            })
            .collect();

        let refs: Vec<&[AbiValue]> = values_batch.iter().map(Vec::as_slice).collect();
        let encoded_batch = cached.encode_batch(&refs).unwrap();
        assert_eq!(encoded_batch.len(), 5);

        for (i, encoded) in encoded_batch.iter().enumerate() {
            let decoded = cached.decode(encoded).unwrap();
            match &decoded[1] {
                AbiValue::Uint(n, bits) => {
                    assert_eq!(*n, U256::from(i as u64 * 100));
                    assert_eq!(*bits, 256);
                }
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
        use crate::decoder::decode_rust;

        let type_strings = ["address", "uint256", "bool"];
        let cached = CachedAbiTypes::new(&type_strings).unwrap();

        let values = vec![
            AbiValue::Address([0xab; 20]),
            AbiValue::Uint(U256::from(999_888_777u64), 256),
            AbiValue::Bool(true),
        ];
        let encoded = cached.encode(&values).unwrap();

        let decoded_cached = cached.decode(&encoded).unwrap();
        let decoded_rust = decode_rust(&type_strings, &encoded).unwrap();

        assert_eq!(decoded_cached, decoded_rust);
    }
}
