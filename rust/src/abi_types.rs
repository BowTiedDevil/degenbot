//! Unified ABI type representation.
//!
//! This module provides a single `AbiType` enum used by both the ABI decoder
//! (`abi_decoder.rs`) and the contract interface (`contract.rs`), eliminating
//! duplicated type parsing logic.

use std::fmt;

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
                    write!(f, "Invalid bit width for {type_name}: expected number after 'int'/'uint'")
                } else {
                    write!(f, "Invalid bit width for {type_name}: {bits} (must be 8-256, multiple of 8)")
                }
            }
            Self::InvalidByteSize { type_name, size } => {
                if *size == 0 {
                    write!(f, "Invalid byte size for {type_name}: expected number after 'bytes'")
                } else {
                    write!(f, "Invalid byte size for {type_name}: {size} (must be 1-32)")
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
#[allow(clippy::option_if_let_else)]
fn parse_base_type(normalized: &str) -> Result<AbiType, AbiTypeError> {
    match normalized {
        "address" => Ok(AbiType::Address),
        "bool" => Ok(AbiType::Bool),
        "bytes" => Ok(AbiType::Bytes),
        "string" => Ok(AbiType::String),
        t => {
            if let Some(n_str) = t.strip_prefix("bytes") {
                match n_str.parse::<usize>() {
                    Ok(n) if n > 0 && n <= 32 => Ok(AbiType::FixedBytes(n)),
                    Ok(n) => Err(AbiTypeError::InvalidByteSize {
                        type_name: t.to_string(),
                        size: n,
                    }),
                    Err(_) => Err(AbiTypeError::InvalidByteSize {
                        type_name: t.to_string(),
                        size: 0,
                    }),
                }
            } else if let Some(n_str) = t.strip_prefix("uint") {
                match n_str.parse::<usize>() {
                    Ok(bits) if bits > 0 && bits <= 256 && bits % 8 == 0 => Ok(AbiType::Uint(bits)),
                    Ok(bits) => Err(AbiTypeError::InvalidBitWidth {
                        type_name: t.to_string(),
                        bits,
                    }),
                    Err(_) => Err(AbiTypeError::InvalidBitWidth {
                        type_name: t.to_string(),
                        bits: 0,
                    }),
                }
            } else if let Some(n_str) = t.strip_prefix("int") {
                match n_str.parse::<usize>() {
                    Ok(bits) if bits > 0 && bits <= 256 && bits % 8 == 0 => Ok(AbiType::Int(bits)),
                    Ok(bits) => Err(AbiTypeError::InvalidBitWidth {
                        type_name: t.to_string(),
                        bits,
                    }),
                    Err(_) => Err(AbiTypeError::InvalidBitWidth {
                        type_name: t.to_string(),
                        bits: 0,
                    }),
                }
            } else {
                Err(AbiTypeError::UnknownType(t.to_string()))
            }
        }
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
        assert!(msg.contains('3') && msg.contains("must be 1-32"), "Error message: {msg}");

        let err = AbiType::parse("uint7").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains('7') && msg.contains("multiple of 8"), "Error message: {msg}");

        let err = AbiType::parse("uint257").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("257") && msg.contains("8-256"), "Error message: {msg}");
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
}
