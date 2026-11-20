use degenbot_core::errors::AbiDecodeError;
use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BitWidth(usize);

impl BitWidth {
    pub const BITS_8: Self = Self(8);
    pub const BITS_80: Self = Self(80);
    pub const BITS_256: Self = Self(256);

    #[must_use]
    pub const fn new(bits: usize) -> Option<Self> {
        if bits >= 8 && bits <= 256 && bits.is_multiple_of(8) {
            Some(Self(bits))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for BitWidth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiType {
    Address,
    Bool,
    Uint(BitWidth),
    Int(BitWidth),
    FixedBytes(usize),
    Bytes,
    String,
    Array(Box<Self>),
    FixedArray(Box<Self>, usize),
    /// Solidity tuple components, parenthesized in signature text.
    Tuple(Vec<Self>),
}

impl AbiType {
    /// # Errors
    ///
    /// Returns `AbiTypeError` if the type string is invalid.
    pub fn parse(s: &str) -> Result<Self, AbiTypeError> {
        parse_abi_type(s.trim())
    }

    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        match self {
            Self::Bytes | Self::String | Self::Array(_) => true,
            Self::FixedArray(inner, _) => inner.is_dynamic(),
            Self::Tuple(comps) => comps.iter().any(AbiType::is_dynamic),
            _ => false,
        }
    }

    /// For `FixedBytes(n)` this is `n * 8`: bit count, not byte count.
    #[must_use]
    pub const fn size_bits(&self) -> Option<usize> {
        match self {
            Self::Uint(bits) | Self::Int(bits) => Some(bits.get()),
            Self::FixedBytes(n) => Some(*n * 8),
            _ => None,
        }
    }

    /// Canonical type string, borrowed for the fixed-width types so the common
    /// cases allocate nothing.
    ///
    /// # Example
    ///
    /// ```
    /// use degenbot_abi::abi_types::type_::{AbiType, BitWidth};
    ///
    /// let ty = AbiType::Address;
    /// assert_eq!(ty.type_str(), "address");
    ///
    /// let ty = AbiType::Uint(BitWidth::BITS_256);
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
            Self::Tuple(comps) => Cow::Owned(format!(
                "({})",
                comps
                    .iter()
                    .map(AbiType::type_str)
                    .collect::<Vec<_>>()
                    .join(",")
            )),
        }
    }

    /// Bridge to alloy's `DynSolType`, so encoding and decoding never re-parse a
    /// type string.
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
            Self::Uint(bits) => Ok(DynSolType::Uint(bits.get())),
            Self::Int(bits) => Ok(DynSolType::Int(bits.get())),
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
            Self::Tuple(comps) => {
                let alloy_types: Result<Vec<_>, _> =
                    comps.iter().map(Self::to_alloy_type).collect();
                Ok(DynSolType::Tuple(alloy_types?))
            }
        }
    }
}

impl fmt::Display for AbiType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.type_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AbiTypeError {
    #[error("Unknown ABI type: {0}")]
    UnknownType(String),
    #[error("Invalid array size: {0}")]
    InvalidArraySize(String),
    #[error("Invalid bit width for {type_name}: {}", if *bits == 0 { "expected number after 'int'/'uint'".to_string() } else { format!("{bits} (must be 8-256, multiple of 8)") })]
    InvalidBitWidth { type_name: String, bits: usize },
    #[error("Invalid byte size for {type_name}: {}", if *size == 0 { "expected number after 'bytes'".to_string() } else { format!("{size} (must be 1-32)") })]
    InvalidByteSize { type_name: String, size: usize },
}

#[inline]
fn normalize_type(abi_type: &str) -> &str {
    match abi_type {
        "uint" => "uint256",
        "int" => "int256",
        "function" => "bytes24",
        other => other,
    }
}

#[expect(clippy::missing_errors_doc)]
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
            _ => {} // ExpectedAbsent: any other char is part of a type token.
        }
    }

    types.push(parse_abi_type(&types_str[start..])?);
    Ok(types)
}

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

    if s.starts_with('(') {
        return parse_tuple_type(s);
    }

    parse_base_type(normalize_type(s))
}

fn parse_tuple_type(maybe_tuple: &str) -> Result<AbiType, AbiTypeError> {
    // Parse a tuple type string `(t1,t2,...)` (no array suffix - those are
    // stripped by `parse_abi_type` before this is reached).
    let mut depth = 0;
    let mut close = None;
    for (i, c) in maybe_tuple.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {} // ExpectedAbsent: any other char is part of the tuple spec.
        }
    }
    let close = close.ok_or_else(|| AbiTypeError::UnknownType(maybe_tuple.to_string()))?;
    if !maybe_tuple[close + 1..].trim().is_empty() {
        return Err(AbiTypeError::UnknownType(maybe_tuple.to_string()));
    }
    let inner = &maybe_tuple[1..close];
    if inner.trim().is_empty() {
        return Err(AbiTypeError::UnknownType(maybe_tuple.to_string()));
    }
    let components = parse_type_list(inner)?;
    if components.is_empty() {
        return Err(AbiTypeError::UnknownType(maybe_tuple.to_string()));
    }
    Ok(AbiType::Tuple(components))
}

fn parse_base_type(normalized: &str) -> Result<AbiType, AbiTypeError> {
    match normalized {
        "address" => Ok(AbiType::Address),
        "bool" => Ok(AbiType::Bool),
        "bytes" => Ok(AbiType::Bytes),
        "string" => Ok(AbiType::String),
        t => parse_sized_type(t),
    }
}

fn parse_sized_type(t: &str) -> Result<AbiType, AbiTypeError> {
    if let Some(n_str) = t.strip_prefix("bytes") {
        return parse_byte_size(t, n_str);
    }

    if let Some(n_str) = t.strip_prefix("uint") {
        return parse_uint_bits(t, n_str);
    }

    if let Some(n_str) = t.strip_prefix("int") {
        return parse_int_bits(t, n_str);
    }

    Err(AbiTypeError::UnknownType(t.to_string()))
}

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

fn parse_uint_bits(type_name: &str, n_str: &str) -> Result<AbiType, AbiTypeError> {
    match n_str.parse::<usize>() {
        Ok(bits) => {
            BitWidth::new(bits)
                .map(AbiType::Uint)
                .ok_or_else(|| AbiTypeError::InvalidBitWidth {
                    type_name: type_name.to_string(),
                    bits,
                })
        }
        Err(_) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits: 0,
        }),
    }
}

fn parse_int_bits(type_name: &str, n_str: &str) -> Result<AbiType, AbiTypeError> {
    match n_str.parse::<usize>() {
        Ok(bits) => {
            BitWidth::new(bits)
                .map(AbiType::Int)
                .ok_or_else(|| AbiTypeError::InvalidBitWidth {
                    type_name: type_name.to_string(),
                    bits,
                })
        }
        Err(_) => Err(AbiTypeError::InvalidBitWidth {
            type_name: type_name.to_string(),
            bits: 0,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn bit_width_accepts_only_solidity_integer_widths() {
        for bits in (8..=256).step_by(8) {
            let width = BitWidth::new(bits).unwrap();
            assert_eq!(width.get(), bits);
            assert_eq!(width.to_string(), bits.to_string());
        }

        for bits in [0, 1, 7, 9, 255, 257, 264, usize::MAX] {
            assert_eq!(BitWidth::new(bits), None);
        }
    }

    #[test]
    fn parsed_integer_types_carry_bit_width() {
        assert_eq!(
            AbiType::parse("uint80").unwrap(),
            AbiType::Uint(BitWidth::BITS_80)
        );
        assert_eq!(
            AbiType::parse("int256").unwrap(),
            AbiType::Int(BitWidth::BITS_256)
        );
    }

    #[test]
    fn test_parse_basic_types() {
        assert_eq!(AbiType::parse("address").unwrap(), AbiType::Address);
        assert_eq!(AbiType::parse("bool").unwrap(), AbiType::Bool);
        assert_eq!(
            AbiType::parse("uint256").unwrap(),
            AbiType::Uint(BitWidth::BITS_256)
        );
        assert_eq!(
            AbiType::parse("uint8").unwrap(),
            AbiType::Uint(BitWidth::BITS_8)
        );
        assert_eq!(
            AbiType::parse("int256").unwrap(),
            AbiType::Int(BitWidth::BITS_256)
        );
        assert_eq!(
            AbiType::parse("int8").unwrap(),
            AbiType::Int(BitWidth::BITS_8)
        );
        assert_eq!(AbiType::parse("bytes").unwrap(), AbiType::Bytes);
        assert_eq!(AbiType::parse("bytes32").unwrap(), AbiType::FixedBytes(32));
        assert_eq!(AbiType::parse("bytes1").unwrap(), AbiType::FixedBytes(1));
        assert_eq!(AbiType::parse("string").unwrap(), AbiType::String);
    }

    #[test]
    fn test_parse_aliases() {
        assert_eq!(
            AbiType::parse("uint").unwrap(),
            AbiType::Uint(BitWidth::BITS_256)
        );
        assert_eq!(
            AbiType::parse("int").unwrap(),
            AbiType::Int(BitWidth::BITS_256)
        );
        assert_eq!(AbiType::parse("function").unwrap(), AbiType::FixedBytes(24));
    }

    #[test]
    fn test_parse_tuple_types() {
        assert_eq!(
            AbiType::parse("(address,bool,bytes)").unwrap(),
            AbiType::Tuple(vec![AbiType::Address, AbiType::Bool, AbiType::Bytes])
        );
        assert_eq!(
            AbiType::parse("(bool,bytes)[]").unwrap(),
            AbiType::Array(Box::new(AbiType::Tuple(vec![
                AbiType::Bool,
                AbiType::Bytes
            ])))
        );
        assert_eq!(
            AbiType::parse("((uint256,address),bool)").unwrap(),
            AbiType::Tuple(vec![
                AbiType::Tuple(vec![AbiType::Uint(BitWidth::BITS_256), AbiType::Address]),
                AbiType::Bool,
            ])
        );
        assert_eq!(
            AbiType::parse("(uint256,bool)[2]").unwrap(),
            AbiType::FixedArray(
                Box::new(AbiType::Tuple(vec![
                    AbiType::Uint(BitWidth::BITS_256),
                    AbiType::Bool
                ])),
                2
            )
        );
    }

    #[test]
    fn test_tuple_type_str_roundtrip() {
        for s in [
            "(address,bool,bytes)",
            "(bool,bytes)[]",
            "((uint256,address),bool)",
            "(uint256,bool)[2]",
            "(uint8,(uint256,bool))[]",
        ] {
            let ty = AbiType::parse(s).unwrap();
            assert_eq!(ty.type_str(), s, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_tuple_is_dynamic() {
        assert!(!AbiType::parse("(bool,uint8)").unwrap().is_dynamic());
        assert!(AbiType::parse("(uint256,bytes)").unwrap().is_dynamic());
        assert!(AbiType::parse("(bytes[])[]").unwrap().is_dynamic());
        assert!(AbiType::parse("((uint256,string),bool)")
            .unwrap()
            .is_dynamic());
    }

    #[test]
    fn test_parse_tuple_errors() {
        assert!(AbiType::parse("()").is_err());
        assert!(AbiType::parse("(uint256))").is_err());
        assert!(AbiType::parse("((uint256)").is_err());
        assert!(AbiType::parse("(,bool)").is_err());
    }

    #[test]
    fn test_parse_array_types() {
        assert_eq!(
            AbiType::parse("address[]").unwrap(),
            AbiType::Array(Box::new(AbiType::Address))
        );
        assert_eq!(
            AbiType::parse("uint256[5]").unwrap(),
            AbiType::FixedArray(Box::new(AbiType::Uint(BitWidth::BITS_256)), 5)
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
        let err = AbiType::parse("bytesfoo").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

        let err = AbiType::parse("uintbar").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

        let err = AbiType::parse("intbaz").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected number"), "Error message: {msg}");

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
        assert!(!AbiType::Uint(BitWidth::BITS_256).is_dynamic());
        assert!(!AbiType::FixedBytes(32).is_dynamic());
        assert!(AbiType::Bytes.is_dynamic());
        assert!(AbiType::String.is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::String)).is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::Uint(BitWidth::BITS_256))).is_dynamic());
        assert!(AbiType::FixedArray(Box::new(AbiType::String), 3).is_dynamic());
        assert!(!AbiType::FixedArray(Box::new(AbiType::Uint(BitWidth::BITS_256)), 3).is_dynamic());
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
        assert_eq!(AbiType::Uint(BitWidth::BITS_256).size_bits(), Some(256));
        assert_eq!(
            AbiType::Int(BitWidth::new(128).unwrap()).size_bits(),
            Some(128)
        );
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
        assert_eq!(types[1], AbiType::Uint(BitWidth::BITS_256));

        let types = parse_type_list("").unwrap();
        assert!(types.is_empty());

        let types = parse_type_list("uint256").unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], AbiType::Uint(BitWidth::BITS_256));
    }

    #[test]
    fn test_whitespace_handling() {
        assert_eq!(
            AbiType::parse("  uint256  ").unwrap(),
            AbiType::Uint(BitWidth::BITS_256)
        );
        assert_eq!(
            AbiType::parse("  address[]  ").unwrap(),
            AbiType::Array(Box::new(AbiType::Address))
        );
    }
}
