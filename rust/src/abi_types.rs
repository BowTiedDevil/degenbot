//! Unified ABI type and value representation.
//!
//! This module provides:
//! - `AbiType` enum for representing ABI type signatures
//! - `AbiValue` enum for representing encoded/decoded ABI values
//!
//! Used by `abi_decoder.rs`, `abi_encoder.rs`, and `contract.rs` to ensure
//! consistent type handling across the codebase.

use crate::errors::{AbiDecodeError, ContractError};
use alloy::dyn_abi::DynSolValue;
use alloy::hex;
use alloy::primitives::{Address, I256, U256};
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

/// Represents an Ethereum ABI type.
///
/// Supports all standard ABI types including nested arrays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiType {
    /// Ethereum address (20 bytes)
    Address,
    /// Boolean value
    Bool,
    /// Unsigned integer with bit width (8-256, multiples of 8)
    Uint(usize),
    /// Signed integer with bit width (8-256, multiples of 8)
    Int(usize),
    /// Fixed-size bytes (bytes1-bytes32)
    FixedBytes(usize),
    /// Dynamic bytes
    Bytes,
    /// Dynamic string
    String,
    /// Dynamic array of a type (e.g., `uint256[]`)
    Array(Box<Self>),
    /// Fixed-size array of a type (e.g., `uint256[3]`)
    FixedArray(Box<Self>, usize),
}

impl AbiType {
    /// Parse an ABI type from a string.
    ///
    /// Supports type aliases: `uint` → `uint256`, `int` → `int256`, `function` → `bytes24`.
    ///
    /// # Errors
    ///
    /// Returns `AbiTypeError` if the type string is invalid.
    pub fn parse(s: &str) -> Result<Self, AbiTypeError> {
        parse_abi_type(s.trim())
    }

    /// Check if this type is dynamically sized.
    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        match self {
            Self::Bytes | Self::String | Self::Array(_) => true,
            Self::FixedArray(inner, _) => inner.is_dynamic(),
            _ => false,
        }
    }

    /// Get the bit width for integer types, byte size for fixed bytes.
    /// Returns `None` for non-sized types.
    #[must_use]
    pub const fn size_bits(&self) -> Option<usize> {
        match self {
            Self::Uint(bits) | Self::Int(bits) => Some(*bits),
            Self::FixedBytes(n) => Some(*n * 8),
            _ => None,
        }
    }

    /// Get the canonical type string without allocating for common types.
    ///
    /// This is more efficient than `to_string()` for types that don't need
    /// formatting (e.g., `address`, `bool`, `bytes`). Sized types like
    /// `uint256` still allocate but this avoids intermediate collections.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ty = AbiType::Address;
    /// assert_eq!(ty.type_str(), "address");
    ///
    /// let ty = AbiType::Uint(256);
    /// assert_eq!(ty.type_str(), "uint256");
    /// ```
    #[must_use]
    pub fn type_str(&self) -> Cow<'static, str> {
        match self {
            Self::Address => Cow::Borrowed("address"),
            Self::Bool => Cow::Borrowed("bool"),
            Self::Bytes => Cow::Borrowed("bytes"),
            Self::String => Cow::Borrowed("string"),
            Self::Uint(bits) => Cow::Owned(format!("uint{bits}")),
            Self::Int(bits) => Cow::Owned(format!("int{bits}")),
            Self::FixedBytes(size) => Cow::Owned(format!("bytes{size}")),
            Self::Array(inner) => Cow::Owned(format!("{}[]", inner.type_str())),
            Self::FixedArray(inner, size) => Cow::Owned(format!("{}[{size}]", inner.type_str())),
        }
    }

    /// Convert this `AbiType` to an alloy `DynSolType`.
    ///
    /// This enables direct encoding/decoding without string parsing.
    ///
    /// # Errors
    ///
    /// Returns `AbiDecodeError::UnsupportedType` if the type cannot be converted
    /// (should not happen for valid `AbiType` instances).
    pub fn to_alloy_type(&self) -> Result<alloy::dyn_abi::DynSolType, AbiDecodeError> {
        use alloy::dyn_abi::DynSolType;

        match self {
            Self::Address => Ok(DynSolType::Address),
            Self::Bool => Ok(DynSolType::Bool),
            Self::Uint(bits) => Ok(DynSolType::Uint(*bits)),
            Self::Int(bits) => Ok(DynSolType::Int(*bits)),
            Self::FixedBytes(size) => Ok(DynSolType::FixedBytes(*size)),
            Self::Bytes => Ok(DynSolType::Bytes),
            Self::String => Ok(DynSolType::String),
            Self::Array(inner) => {
                let inner_type = inner.to_alloy_type()?;
                Ok(DynSolType::Array(Box::new(inner_type)))
            }
            Self::FixedArray(inner, size) => {
                let inner_type = inner.to_alloy_type()?;
                Ok(DynSolType::FixedArray(Box::new(inner_type), *size))
            }
        }
    }
}

impl fmt::Display for AbiType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", type_to_string(self))
    }
}

/// Errors that can occur when parsing an ABI type string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiTypeError {
    /// Unknown or unsupported ABI type.
    UnknownType(String),
    /// Invalid array size specification.
    InvalidArraySize(String),
    /// Invalid bit width for integer type.
    InvalidBitWidth { type_name: String, bits: usize },
    /// Invalid byte size for fixed bytes type.
    InvalidByteSize { type_name: String, size: usize },
}

impl fmt::Display for AbiTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType(t) => write!(f, "Unknown ABI type: {t}"),
            Self::InvalidArraySize(s) => write!(f, "Invalid array size: {s}"),
            Self::InvalidBitWidth { type_name, bits } => {
                if *bits == 0 {
                    write!(
                        f,
                        "Invalid bit width for {type_name}: expected number after 'int'/'uint'"
                    )
                } else {
                    write!(
                        f,
                        "Invalid bit width for {type_name}: {bits} (must be 8-256, multiple of 8)"
                    )
                }
            }
            Self::InvalidByteSize { type_name, size } => {
                if *size == 0 {
                    write!(
                        f,
                        "Invalid byte size for {type_name}: expected number after 'bytes'"
                    )
                } else {
                    write!(
                        f,
                        "Invalid byte size for {type_name}: {size} (must be 1-32)"
                    )
                }
            }
        }
    }
}

impl std::error::Error for AbiTypeError {}

// =============================================================================
// Internal parsing implementation
// =============================================================================

/// Normalize a type string by applying aliases.
#[inline]
fn normalize_type(abi_type: &str) -> &str {
    match abi_type {
        "uint" => "uint256",
        "int" => "int256",
        "function" => "bytes24",
        other => other,
    }
}

/// Parse a comma-separated list of types.
pub fn parse_type_list(types_str: &str) -> Result<Vec<AbiType>, AbiTypeError> {
    if types_str.is_empty() {
        return Ok(Vec::new());
    }

    let mut types = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, c) in types_str.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                types.push(parse_abi_type(&types_str[start..i])?);
                start = i + 1;
            }
            _ => {}
        }
    }

    types.push(parse_abi_type(&types_str[start..])?);
    Ok(types)
}

/// Core recursive parser that handles arrays and base types.
fn parse_abi_type(s: &str) -> Result<AbiType, AbiTypeError> {
    // Handle array types - find the last '[' to support nested arrays
    if let Some(bracket_idx) = s.rfind('[') {
        let base = &s[..bracket_idx];
        let rest = &s[bracket_idx..];

        if rest == "[]" {
            let inner = parse_abi_type(base)?;
            return Ok(AbiType::Array(Box::new(inner)));
        }

        if rest.ends_with(']') && rest.starts_with('[') {
            let size_str = &rest[1..rest.len() - 1];
            let size = size_str
                .parse::<usize>()
                .map_err(|_| AbiTypeError::InvalidArraySize(s.to_string()))?;
            if size == 0 {
                return Err(AbiTypeError::InvalidArraySize(
                    "Fixed array size must be >= 1 (uint256[0] is not supported by Solidity)"
                        .to_string(),
                ));
            }
            let inner = parse_abi_type(base)?;
            return Ok(AbiType::FixedArray(Box::new(inner), size));
        }
    }

    // Parse base type
    parse_base_type(normalize_type(s))
}

/// Parse a normalized base type string (no arrays).
fn parse_base_type(normalized: &str) -> Result<AbiType, AbiTypeError> {
    match normalized {
        "address" => Ok(AbiType::Address),
        "bool" => Ok(AbiType::Bool),
        "bytes" => Ok(AbiType::Bytes),
        "string" => Ok(AbiType::String),
        t => parse_sized_type(t),
    }
}

/// Parse sized types like bytesN, uintN, intN.
fn parse_sized_type(t: &str) -> Result<AbiType, AbiTypeError> {
    // Try bytesN first
    if let Some(n_str) = t.strip_prefix("bytes") {
        return parse_byte_size(t, n_str);
    }

    // Try uintN
    if let Some(n_str) = t.strip_prefix("uint") {
        return parse_uint_bits(t, n_str);
    }

    // Try intN
    if let Some(n_str) = t.strip_prefix("int") {
        return parse_int_bits(t, n_str);
    }

    Err(AbiTypeError::UnknownType(t.to_string()))
}

/// Parse bytes size (1-32).
fn parse_byte_size(type_name: &str, n_str: &str) -> Result<AbiType, AbiTypeError> {
    match n_str.parse::<usize>() {
        Ok(n) if n > 0 && n <= 32 => Ok(AbiType::FixedBytes(n)),
        Ok(n) => Err(AbiTypeError::InvalidByteSize {
            type_name: type_name.to_string(),
            size: n,
        }),
        Err(_) => Err(AbiTypeError::InvalidByteSize {
            type_name: type_name.to_string(),
            size: 0,
        }),
    }
}

/// Parse uint bits (8-256, multiple of 8).
fn parse_uint_bits(type_name: &str, n_str: &str) -> Result<AbiType, AbiTypeError> {
    match n_str.parse::<usize>() {
        Ok(bits) if bits > 0 && bits <= 256 && bits % 8 == 0 => Ok(AbiType::Uint(bits)),
        Ok(bits) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits,
        }),
        Err(_) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits: 0,
        }),
    }
}

/// Parse int bits (8-256, multiple of 8).
fn parse_int_bits(type_name: &str, n_str: &str) -> Result<AbiType, AbiTypeError> {
    match n_str.parse::<usize>() {
        Ok(bits) if bits > 0 && bits <= 256 && bits % 8 == 0 => Ok(AbiType::Int(bits)),
        Ok(bits) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits,
        }),
        Err(_) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits: 0,
        }),
    }
}

/// Convert an `AbiType` back to its canonical string representation.
fn type_to_string(abi_type: &AbiType) -> String {
    match abi_type {
        AbiType::Address => "address".to_string(),
        AbiType::Bool => "bool".to_string(),
        AbiType::Uint(bits) => format!("uint{bits}"),
        AbiType::Int(bits) => format!("int{bits}"),
        AbiType::FixedBytes(size) => format!("bytes{size}"),
        AbiType::Bytes => "bytes".to_string(),
        AbiType::String => "string".to_string(),
        AbiType::Array(inner) => format!("{}[]", type_to_string(inner)),
        AbiType::FixedArray(inner, size) => format!("{}[{size}]", type_to_string(inner)),
    }
}

// =============================================================================
// AbiValue - Unified representation of ABI values
// =============================================================================

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
            Self::Uint(n) => {
                // U256 is already in the correct format
                Ok(DynSolValue::Uint(*n, 256))
            }
            Self::Int(n) => {
                // I256 is already in the correct format
                Ok(DynSolValue::Int(*n, 256))
            }
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
                // Treat tuples as arrays
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
                let hex_str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
                let bytes = hex::decode(hex_str).map_err(|_| ContractError::InvalidAbi {
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
                let hex_str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
                let bytes = hex::decode(hex_str).map_err(|_| ContractError::InvalidAbi {
                    message: format!("Invalid hex value for bytes: {arg}"),
                })?;
                Ok(Self::Bytes(bytes))
            }
            AbiType::String => Ok(Self::String(trimmed.to_string())),
            AbiType::Array(element_type) => {
                // Parse JSON array format
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
fn parse_uint256_with_hex_prefix(s: &str) -> Result<U256, ParseU256Error> {
    let s = s.trim();
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .map_or_else(
            || U256::from_str_radix(s, 10).map_err(|_| ParseU256Error::InvalidDecimal),
            |hex_str| U256::from_str_radix(hex_str, 16).map_err(|_| ParseU256Error::InvalidHex),
        )
}

/// Parse an int256 value from a string, handling hex prefix.
fn parse_int256_with_hex_prefix(s: &str) -> Result<I256, ParseI256Error> {
    let s = s.trim();
    // Handle negative sign separately for hex
    if let Some(abs_str) = s.strip_prefix('-') {
        if let Some(hex_str) = abs_str.strip_prefix("0x").or_else(|| abs_str.strip_prefix("0X")) {
            let abs_val = U256::from_str_radix(hex_str, 16)
                .map_err(|_| ParseI256Error::InvalidHex)?;
            let neg = I256::from_raw(abs_val).wrapping_neg();
            Ok(neg)
        } else {
            // Parse as decimal using FromStr
            I256::from_str(s).map_err(|_| ParseI256Error::InvalidDecimal)
        }
    } else if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        // For positive hex values, parse as U256 and convert
        let uval = U256::from_str_radix(hex_str, 16).map_err(|_| ParseI256Error::InvalidHex)?;
        Ok(I256::from_raw(uval))
    } else {
        // Parse as decimal using FromStr
        I256::from_str(s).map_err(|_| ParseI256Error::InvalidDecimal)
    }
}

/// Error parsing U256 from string.
#[derive(Debug, Clone)]
pub enum ParseU256Error {
    /// Invalid hex format
    InvalidHex,
    /// Invalid decimal format
    InvalidDecimal,
}

impl fmt::Display for ParseU256Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHex => write!(f, "invalid hex format"),
            Self::InvalidDecimal => write!(f, "invalid decimal format"),
        }
    }
}

impl std::error::Error for ParseU256Error {}

/// Error parsing I256 from string.
#[derive(Debug, Clone)]
pub enum ParseI256Error {
    /// Invalid hex format
    InvalidHex,
    /// Invalid decimal format
    InvalidDecimal,
}

impl fmt::Display for ParseI256Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHex => write!(f, "invalid hex format"),
            Self::InvalidDecimal => write!(f, "invalid decimal format"),
        }
    }
}

impl std::error::Error for ParseI256Error {}

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

    // Split by comma, respecting nested brackets
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

    // Add the last element
    let last_element = &content[start..].trim();
    if !last_element.is_empty() || start < content.len() {
        elements.push(*last_element);
    }

    Ok(elements)
}

// =============================================================================
// CachedAbiTypes - Pre-parsed types for batch operations
// =============================================================================

/// Pre-parsed ABI types for high-performance batch encoding/decoding.
///
/// When processing thousands of values with the same type signature
/// (e.g., decoding Transfer events from historical blocks), parsing the
/// type string each time adds significant overhead. This struct caches
/// the parsed types for reuse.
///
/// # Example
///
/// ```ignore
/// use degenbot_rs::abi_types::CachedAbiTypes;
///
/// // Parse once
/// let cached = CachedAbiTypes::new(&["address", "address", "uint256"])?;
///
/// // Decode many times without parsing overhead
/// for log_data in &logs {
///     let values = cached.decode(log_data)?;
/// }
/// ```
#[derive(Clone, Debug)]
pub struct CachedAbiTypes {
    /// The individual parsed types
    types: Vec<AbiType>,
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
    /// ```ignore
    /// let cached = CachedAbiTypes::new(&["address", "uint256"])?;
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

        let tuple_type = alloy::dyn_abi::DynSolType::Tuple(alloy_types);

        Ok(Self {
            types: parsed_types,
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

        let tuple_type = alloy::dyn_abi::DynSolType::Tuple(alloy_types);

        Ok(Self {
            types: types.to_vec(),
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
    /// ```ignore
    /// let cached = CachedAbiTypes::new(&["address", "uint256"])?;
    /// let encoded_events: Vec<&[u8]> = vec![/* ... */];
    /// let decoded_batch = cached.decode_batch(&encoded_events)?;
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

        let mut alloy_values = Vec::with_capacity(self.types.len());
        for (ty, value) in self.types.iter().zip(values.iter()) {
            let alloy_type = ty.to_alloy_type()?;
            let alloy_value = value_to_alloy_for_type(value, &alloy_type)?;
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
fn value_to_alloy_for_type(
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_parse_basic_types() {
        assert_eq!(AbiType::parse("address").unwrap(), AbiType::Address);
        assert_eq!(AbiType::parse("bool").unwrap(), AbiType::Bool);
        assert_eq!(AbiType::parse("uint256").unwrap(), AbiType::Uint(256));
        assert_eq!(AbiType::parse("uint8").unwrap(), AbiType::Uint(8));
        assert_eq!(AbiType::parse("int256").unwrap(), AbiType::Int(256));
        assert_eq!(AbiType::parse("int8").unwrap(), AbiType::Int(8));
        assert_eq!(AbiType::parse("bytes").unwrap(), AbiType::Bytes);
        assert_eq!(AbiType::parse("bytes32").unwrap(), AbiType::FixedBytes(32));
        assert_eq!(AbiType::parse("bytes1").unwrap(), AbiType::FixedBytes(1));
        assert_eq!(AbiType::parse("string").unwrap(), AbiType::String);
    }

    #[test]
    fn test_parse_aliases() {
        assert_eq!(AbiType::parse("uint").unwrap(), AbiType::Uint(256));
        assert_eq!(AbiType::parse("int").unwrap(), AbiType::Int(256));
        assert_eq!(AbiType::parse("function").unwrap(), AbiType::FixedBytes(24));
    }

    #[test]
    fn test_parse_array_types() {
        assert_eq!(
            AbiType::parse("address[]").unwrap(),
            AbiType::Array(Box::new(AbiType::Address))
        );
        assert_eq!(
            AbiType::parse("uint256[5]").unwrap(),
            AbiType::FixedArray(Box::new(AbiType::Uint(256)), 5)
        );
        assert_eq!(
            AbiType::parse("bytes32[]").unwrap(),
            AbiType::Array(Box::new(AbiType::FixedBytes(32)))
        );
        assert_eq!(
            AbiType::parse("address[][3]").unwrap(),
            AbiType::FixedArray(Box::new(AbiType::Array(Box::new(AbiType::Address))), 3)
        );
    }

    #[test]
    fn test_parse_invalid_types() {
        assert!(matches!(
            AbiType::parse("invalid"),
            Err(AbiTypeError::UnknownType(_))
        ));
        assert!(matches!(
            AbiType::parse("bytes33"),
            Err(AbiTypeError::InvalidByteSize { .. })
        ));
        assert!(matches!(
            AbiType::parse("bytes0"),
            Err(AbiTypeError::InvalidByteSize { .. })
        ));
        assert!(matches!(
            AbiType::parse("uint7"),
            Err(AbiTypeError::InvalidBitWidth { .. })
        ));
        assert!(matches!(
            AbiType::parse("uint257"),
            Err(AbiTypeError::InvalidBitWidth { .. })
        ));
        assert!(matches!(
            AbiType::parse("uint256[0]"),
            Err(AbiTypeError::InvalidArraySize(ref msg)) if msg.contains("uint256[0]") || msg.contains("must be >= 1")
        ));
        assert!(matches!(
            AbiType::parse("uint256[invalid]"),
            Err(AbiTypeError::InvalidArraySize(_))
        ));
    }

    #[test]
    fn test_parse_invalid_types_error_messages() {
        // Non-numeric suffix should give clear "expected number" message
        let err = AbiType::parse("bytesfoo").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

        let err = AbiType::parse("uintbar").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

        let err = AbiType::parse("intbaz").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

        // Out-of-range values should show the actual value and constraints
        let err = AbiType::parse("bytes33").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains('3') && msg.contains("must be 1-32"),
            "Error message: {msg}"
        );

        let err = AbiType::parse("uint7").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains('7') && msg.contains("multiple of 8"),
            "Error message: {msg}"
        );

        let err = AbiType::parse("uint257").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("257") && msg.contains("8-256"),
            "Error message: {msg}"
        );
    }

    #[test]
    fn test_is_dynamic() {
        assert!(!AbiType::Address.is_dynamic());
        assert!(!AbiType::Bool.is_dynamic());
        assert!(!AbiType::Uint(256).is_dynamic());
        assert!(!AbiType::FixedBytes(32).is_dynamic());
        assert!(AbiType::Bytes.is_dynamic());
        assert!(AbiType::String.is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::String)).is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::Uint(256))).is_dynamic());
        assert!(AbiType::FixedArray(Box::new(AbiType::String), 3).is_dynamic());
        assert!(!AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3).is_dynamic());
    }

    #[test]
    fn test_to_string_roundtrip() {
        let types = [
            "address",
            "bool",
            "uint256",
            "uint8",
            "int256",
            "int128",
            "bytes",
            "bytes32",
            "bytes1",
            "string",
            "address[]",
            "uint256[5]",
            "bytes32[]",
            "address[][3]",
        ];

        for t in types {
            let parsed = AbiType::parse(t).unwrap();
            assert_eq!(parsed.to_string(), t);
        }
    }

    #[test]
    fn test_size_bits() {
        assert_eq!(AbiType::Uint(256).size_bits(), Some(256));
        assert_eq!(AbiType::Int(128).size_bits(), Some(128));
        assert_eq!(AbiType::FixedBytes(32).size_bits(), Some(256));
        assert_eq!(AbiType::FixedBytes(1).size_bits(), Some(8));
        assert!(AbiType::Address.size_bits().is_none());
        assert!(AbiType::Bytes.size_bits().is_none());
        assert!(AbiType::String.size_bits().is_none());
    }

    #[test]
    fn test_parse_type_list() {
        let types = parse_type_list("address,uint256").unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0], AbiType::Address);
        assert_eq!(types[1], AbiType::Uint(256));

        let types = parse_type_list("").unwrap();
        assert!(types.is_empty());

        let types = parse_type_list("uint256").unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], AbiType::Uint(256));
    }

    #[test]
    fn test_whitespace_handling() {
        assert_eq!(AbiType::parse("  uint256  ").unwrap(), AbiType::Uint(256));
        assert_eq!(
            AbiType::parse("  address[]  ").unwrap(),
            AbiType::Array(Box::new(AbiType::Address))
        );
    }

    // =========================================================================
    // CachedAbiTypes tests
    // =========================================================================

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
        // Create cached types for Transfer event: (address, address, uint256)
        let cached = CachedAbiTypes::new(&["address", "address", "uint256"]).unwrap();

        // Create test values
        let values = vec![
            AbiValue::Address([0x11; 20]),
            AbiValue::Address([0x22; 20]),
            AbiValue::Uint(alloy::primitives::U256::from(1000u64)),
        ];

        // Encode
        let encoded = cached.encode(&values).unwrap();

        // Decode
        let decoded = cached.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 3);

        // Verify values match
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

        // Create test data: 3 encoded items
        let mut encoded_items: Vec<Vec<u8>> = Vec::new();
        for i in 0..3u64 {
            let values = vec![AbiValue::Uint(alloy::primitives::U256::from(i * 10)), AbiValue::Bool(i % 2 == 0)];
            encoded_items.push(cached.encode(&values).unwrap());
        }

        // Decode batch
        let refs: Vec<&[u8]> = encoded_items.iter().map(|v| v.as_slice()).collect();
        let decoded_batch = cached.decode_batch(&refs).unwrap();
        assert_eq!(decoded_batch.len(), 3);

        // Verify values
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

        // Create multiple value sets
        let values_batch: Vec<Vec<AbiValue>> = (0..5)
            .map(|i| {
                vec![
                    AbiValue::Address([i as u8; 20]),
                    AbiValue::Uint(alloy::primitives::U256::from(i * 100)),
                ]
            })
            .collect();

        // Encode batch
        let refs: Vec<&[AbiValue]> = values_batch.iter().map(|v| v.as_slice()).collect();
        let encoded_batch = cached.encode_batch(&refs).unwrap();
        assert_eq!(encoded_batch.len(), 5);

        // Verify each encoded value decodes correctly
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

        // Provide wrong number of values
        let values = vec![AbiValue::Address([0u8; 20])];
        let result = cached.encode(&values);
        assert!(matches!(result, Err(AbiDecodeError::InvalidLength(_))));
    }

    #[test]
    fn test_cached_abi_types_matches_decode_rust() {
        use crate::abi_decoder::decode_rust;

        let type_strings = ["address", "uint256", "bool"];
        let cached = CachedAbiTypes::new(&type_strings).unwrap();

        // Create test data
        let values = vec![
            AbiValue::Address([0xab; 20]),
            AbiValue::Uint(alloy::primitives::U256::from(999_888_777u64)),
            AbiValue::Bool(true),
        ];
        let encoded = cached.encode(&values).unwrap();

        // Compare with decode_rust
        let decoded_cached = cached.decode(&encoded).unwrap();
        let decoded_rust = decode_rust(&type_strings, &encoded).unwrap();

        assert_eq!(decoded_cached, decoded_rust);
    }
}
