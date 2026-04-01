//! Smart contract interface with ABI encoding/decoding.
//!
//! Provides high-level contract interaction with automatic ABI encoding
//! for function calls and automatic decoding of return values.

use crate::errors::{ProviderError, ProviderResult};
use crate::provider::AlloyProvider;
use alloy::primitives::{Address, Bytes, U256};
use alloy::primitives::I256;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

/// ABI type information parsed from function signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiType {
    /// Address type (20 bytes)
    Address,
    /// Boolean type
    Bool,
    /// Unsigned integer (uint8-uint256)
    Uint(usize),
    /// Signed integer (int8-int256)
    Int(usize),
    /// Fixed-size bytes (bytes1-bytes32)
    FixedBytes(usize),
    /// Dynamic bytes
    Bytes,
    /// String type
    String,
    /// Dynamic array of a type
    Array(Box<Self>),
    /// Fixed-size array of a type
    FixedArray(Box<Self>, usize),
}

impl AbiType {
    /// Parse an ABI type from a string.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidAbi` if the type string is invalid.
    pub fn parse(s: &str) -> ProviderResult<Self> {
        let s = s.trim();

        // Handle array types
        if let Some(bracket_idx) = s.find('[') {
            let base = &s[..bracket_idx];
            let rest = &s[bracket_idx..];

            if rest.ends_with("[]") {
                // Dynamic array
                let inner = Self::parse(base)?;
                return Ok(Self::Array(Box::new(inner)));
            }

            // Fixed-size array
            let size_str = &rest[1..rest.len() - 1];
            let size = size_str
                .parse::<usize>()
                .map_err(|_| ProviderError::InvalidAbi {
                    message: format!("Invalid array size in type: {s}"),
                })?;
            let inner = Self::parse(base)?;
            return Ok(Self::FixedArray(Box::new(inner), size));
        }

        // Handle basic types
        match s {
            "address" => Ok(Self::Address),
            "bool" => Ok(Self::Bool),
            "bytes" => Ok(Self::Bytes),
            "string" => Ok(Self::String),
            _ => {
                // Handle uint/int/fixed bytes
                if let Some(num_str) = s.strip_prefix("uint") {
                    let bits = if num_str.is_empty() {
                        return Err(ProviderError::InvalidAbi {
                            message: format!("Missing bits in uint type: {s}"),
                        });
                    } else {
                        num_str.parse::<usize>().map_err(|_| ProviderError::InvalidAbi {
                            message: format!("Invalid uint bits: {s}"),
                        })?
                    };
                    if bits % 8 != 0 || !(8..=256).contains(&bits) {
                        return Err(ProviderError::InvalidAbi {
                            message: format!("Invalid uint bits: {bits}"),
                        });
                    }
                    Ok(Self::Uint(bits))
                } else if let Some(num_str) = s.strip_prefix("int") {
                    let bits = if num_str.is_empty() {
                        return Err(ProviderError::InvalidAbi {
                            message: format!("Missing bits in int type: {s}"),
                        });
                    } else {
                        num_str.parse::<usize>().map_err(|_| ProviderError::InvalidAbi {
                            message: format!("Invalid int bits: {s}"),
                        })?
                    };
                    if bits % 8 != 0 || !(8..=256).contains(&bits) {
                        return Err(ProviderError::InvalidAbi {
                            message: format!("Invalid int bits: {bits}"),
                        });
                    }
                    Ok(Self::Int(bits))
                } else if let Some(num_str) = s.strip_prefix("bytes") {
                    if num_str.is_empty() {
                        Ok(Self::Bytes)
                    } else {
                        let size = num_str.parse::<usize>().map_err(|_| ProviderError::InvalidAbi {
                            message: format!("Invalid bytes size: {s}"),
                        })?;
                        if size == 0 || size > 32 {
                            return Err(ProviderError::InvalidAbi {
                                message: format!("Invalid bytes size: {size}"),
                            });
                        }
                        Ok(Self::FixedBytes(size))
                    }
                } else {
                    Err(ProviderError::InvalidAbi {
                        message: format!("Unknown ABI type: {s}"),
                    })
                }
            }
        }
    }

    /// Check if this type is dynamically sized.
    #[must_use]
    pub const fn is_dynamic(&self) -> bool {
        match self {
            Self::Bytes | Self::String | Self::Array(_) => true,
            Self::FixedArray(inner, _) => inner.is_dynamic(),
            _ => false,
        }
    }
}

/// Parsed function signature.
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    /// Function name
    pub name: String,
    /// Input parameter types
    pub inputs: Vec<AbiType>,
    /// Output return types
    pub outputs: Vec<AbiType>,
    /// Function selector (4-byte keccak256 hash of signature)
    pub selector: [u8; 4],
}

impl FunctionSignature {
    /// Parse a function signature like "transfer(address,uint256)" or "balanceOf(address) returns (uint256)".
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidAbi` if the signature is invalid.
    pub fn parse(signature: &str) -> ProviderResult<Self> {
        // Normalize whitespace
        let sig = signature.replace(' ', "");

        // Extract output types if present
        let (sig_part, output_part) = sig.find("returns(").map_or((sig.as_str(), None), |returns_idx| {
            let outputs = &sig[returns_idx + 8..sig.len() - 1];
            (&sig[..returns_idx], Some(outputs.to_string()))
        });

        // Parse function name and inputs
        let open_paren = sig_part.find('(').ok_or_else(|| ProviderError::InvalidAbi {
            message: format!("Missing opening parenthesis in signature: {signature}"),
        })?;

        let close_paren = sig_part.find(')').ok_or_else(|| ProviderError::InvalidAbi {
            message: format!("Missing closing parenthesis in signature: {signature}"),
        })?;

        let name = sig_part[..open_paren].to_string();

        if name.is_empty() {
            return Err(ProviderError::InvalidAbi {
                message: format!("Empty function name in signature: {signature}"),
            });
        }

        // Parse input types
        let inputs_str = &sig_part[open_paren + 1..close_paren];
        let inputs = Self::parse_types(inputs_str)?;

        // Parse output types
        let outputs = if let Some(out_str) = output_part {
            Self::parse_types(&out_str)?
        } else {
            Vec::new()
        };

        // Calculate selector: first 4 bytes of keccak256(signature_without_returns)
        let selector_input = format!("{name}({})", Self::types_to_string(&inputs));
        let selector = Self::calculate_selector(&selector_input);

        Ok(Self {
            name,
            inputs,
            outputs,
            selector,
        })
    }

    /// Parse a comma-separated list of types.
    fn parse_types(types_str: &str) -> ProviderResult<Vec<AbiType>> {
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
                    let abi_type_str = &types_str[start..i];
                    types.push(AbiType::parse(abi_type_str)?);
                    start = i + 1;
                }
                _ => {}
            }
        }

        // Add last type
        let abi_type_str = &types_str[start..];
        types.push(AbiType::parse(abi_type_str)?);

        Ok(types)
    }

    /// Convert types back to comma-separated string.
    fn types_to_string(types: &[AbiType]) -> String {
        types
            .iter()
            .map(Self::type_to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Convert a single type to string.
    fn type_to_string(abi_type: &AbiType) -> String {
        match abi_type {
            AbiType::Address => "address".to_string(),
            AbiType::Bool => "bool".to_string(),
            AbiType::Uint(bits) => format!("uint{bits}"),
            AbiType::Int(bits) => format!("int{bits}"),
            AbiType::FixedBytes(size) => format!("bytes{size}"),
            AbiType::Bytes => "bytes".to_string(),
            AbiType::String => "string".to_string(),
            AbiType::Array(inner) => format!("{}[]", Self::type_to_string(inner)),
            AbiType::FixedArray(inner, size) => format!("{}[{size}]", Self::type_to_string(inner)),
        }
    }

    /// Calculate function selector (4-byte keccak256 hash).
    fn calculate_selector(input: &str) -> [u8; 4] {
        use alloy::primitives::keccak256;
        let hash = keccak256(input.as_bytes());
        let mut selector = [0u8; 4];
        selector.copy_from_slice(&hash[..4]);
        selector
    }
}

/// Encode arguments for an ABI function call.
pub fn encode_arguments(types: &[AbiType], args: &[String]) -> ProviderResult<Bytes> {
    if types.len() != args.len() {
        return Err(ProviderError::InvalidAbi {
            message: format!(
                "Argument count mismatch: expected {}, got {}",
                types.len(),
                args.len()
            ),
        });
    }

    let mut encoded = Vec::new();

    for (abi_type, arg) in types.iter().zip(args.iter()) {
        let encoded_arg = encode_value(abi_type, arg)?;
        encoded.extend_from_slice(&encoded_arg);
    }

    Ok(Bytes::from(encoded))
}

/// Encode a single value based on its ABI type.
fn encode_value(abi_type: &AbiType, value: &str) -> ProviderResult<Vec<u8>> {
    match abi_type {
        AbiType::Address => {
            let addr = Address::from_str(value.trim()).map_err(|_| ProviderError::InvalidAddress {
                address: value.to_string(),
                reason: "Invalid address format".to_string(),
            })?;
            // Address is right-padded to 32 bytes
            let mut encoded = vec![0u8; 32];
            encoded[12..32].copy_from_slice(&addr[..]);
            Ok(encoded)
        }
        AbiType::Bool => {
            let mut encoded = vec![0u8; 32];
            if value.trim().to_lowercase() == "true" || value.trim() == "1" {
                encoded[31] = 1;
            }
            Ok(encoded)
        }
        AbiType::Uint(bits) => {
            let uint_val = U256::from_str(value.trim()).map_err(|_| ProviderError::InvalidAbi {
                message: format!("Invalid uint{bits} value: {value}"),
            })?;
            // U256 is already 32 bytes
            Ok(uint_val.to_be_bytes_vec())
        }
        AbiType::Int(bits) => {
            let int_val = I256::from_str(value.trim()).map_err(|_| ProviderError::InvalidAbi {
                message: format!("Invalid int{bits} value: {value}"),
            })?;
            // I256 to 32 bytes (signed two's complement)
            let mut encoded = vec![0u8; 32];
            let bytes = int_val.to_be_bytes::<32>();
            encoded.copy_from_slice(&bytes);
            Ok(encoded)
        }
        AbiType::FixedBytes(size) => {
            let hex_str = value.strip_prefix("0x").map_or(value, |stripped| stripped);
            let bytes = hex::decode(hex_str).map_err(|_| ProviderError::InvalidAbi {
                message: format!("Invalid hex value for bytes{size}: {value}"),
            })?;
            if bytes.len() != *size {
                return Err(ProviderError::InvalidAbi {
                    message: format!(
                        "bytes{size} requires exactly {size} bytes, got {}",
                        bytes.len()
                    ),
                });
            }
            // Right-pad to 32 bytes
            let mut encoded = vec![0u8; 32];
            encoded[..*size].copy_from_slice(&bytes);
            Ok(encoded)
        }
        AbiType::Bytes => {
            let hex_str = value.strip_prefix("0x").map_or(value, |stripped| stripped);
            let bytes = hex::decode(hex_str).map_err(|_| ProviderError::InvalidAbi {
                message: format!("Invalid hex value for bytes: {value}"),
            })?;

            // Dynamic type: offset to data location (for single value, this is 32)
            // followed by length and data
            let mut encoded = Vec::new();

            // Offset (32 bytes for this single dynamic value)
            let offset = U256::from(32);
            encoded.extend_from_slice(&offset.to_be_bytes_vec());

            // Length
            let length = U256::from(bytes.len());
            encoded.extend_from_slice(&length.to_be_bytes_vec());

            // Data (padded to 32-byte boundary)
            encoded.extend_from_slice(&bytes);
            let padding = (32 - (bytes.len() % 32)) % 32;
            encoded.extend(std::iter::repeat_n(0u8, padding));

            Ok(encoded)
        }
        AbiType::String => {
            let bytes = value.as_bytes();

            let mut encoded = Vec::new();

            // Offset
            let offset = U256::from(32);
            encoded.extend_from_slice(&offset.to_be_bytes_vec());

            // Length
            let length = U256::from(bytes.len());
            encoded.extend_from_slice(&length.to_be_bytes_vec());

            // Data (padded to 32-byte boundary)
            encoded.extend_from_slice(bytes);
            let padding = (32 - (bytes.len() % 32)) % 32;
            encoded.extend(std::iter::repeat_n(0u8, padding));

            Ok(encoded)
        }
        AbiType::Array(_) | AbiType::FixedArray(_, _) => {
            // For now, arrays require hex-encoded pre-encoded data
            let hex_str = value.strip_prefix("0x").map_or(value, |stripped| stripped);
            let bytes = hex::decode(hex_str).map_err(|_| ProviderError::InvalidAbi {
                message: format!("Invalid hex value for array: {value}"),
            })?;
            Ok(bytes)
        }
    }
}

/// Decode return data based on expected ABI types.
pub fn decode_return_data(data: &[u8], types: &[AbiType]) -> ProviderResult<Vec<String>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    let mut offset = 0;

    for abi_type in types {
        let (value, new_offset) = decode_value(data, offset, abi_type)?;
        values.push(value);
        offset = new_offset;
    }

    Ok(values)
}

/// Decode a single value from the data.
fn decode_value(data: &[u8], offset: usize, abi_type: &AbiType) -> ProviderResult<(String, usize)> {
    if data.len() < offset + 32 {
        return Err(ProviderError::DecodingError {
            message: "Insufficient data for decoding".to_string(),
        });
    }

    match abi_type {
        AbiType::Address => {
            // Address is in the last 20 bytes of the 32-byte word
            let addr_bytes = &data[offset + 12..offset + 32];
            let addr = Address::from_slice(addr_bytes);
            Ok((format!("{addr:#x}"), offset + 32))
        }
        AbiType::Bool => {
            let value = data[offset + 31] != 0;
            Ok((value.to_string(), offset + 32))
        }
        AbiType::Uint(_) => {
            let value = U256::from_be_slice(&data[offset..offset + 32]);
            Ok((value.to_string(), offset + 32))
        }
        AbiType::Int(_) => {
            let value = I256::from_be_bytes::<32>(data[offset..offset + 32].try_into().map_err(|_| ProviderError::DecodingError {
                message: "Failed to convert bytes to I256".to_string(),
            })?);
            Ok((value.to_string(), offset + 32))
        }
        AbiType::FixedBytes(size) => {
            let bytes = &data[offset..offset + *size];
            Ok((format!("0x{}", hex::encode(bytes)), offset + 32))
        }
        AbiType::Bytes | AbiType::String => {
            // Dynamic types: offset to data location
            let data_offset = U256::from_be_slice(&data[offset..offset + 32]).to::<usize>();

            if data.len() < data_offset + 32 {
                return Err(ProviderError::DecodingError {
                    message: "Invalid dynamic data offset".to_string(),
                });
            }

            let length = U256::from_be_slice(&data[data_offset..data_offset + 32]).to::<usize>();
            let value_start = data_offset + 32;
            let value_end = value_start + length;

            if data.len() < value_end {
                return Err(ProviderError::DecodingError {
                    message: "Insufficient data for dynamic value".to_string(),
                });
            }

            let value_bytes = &data[value_start..value_end];

            if matches!(abi_type, AbiType::String) {
                let value = String::from_utf8_lossy(value_bytes).to_string();
                Ok((value, offset + 32))
            } else {
                Ok((format!("0x{}", hex::encode(value_bytes)), offset + 32))
            }
        }
        AbiType::Array(_) | AbiType::FixedArray(_, _) => {
            // For now, return hex-encoded data
            // Full array decoding would be more complex
            let remaining = &data[offset..];
            Ok((format!("0x{}", hex::encode(remaining)), data.len()))
        }
    }
}

/// Contract interface for calling contract functions.
pub struct Contract {
    address: Address,
    provider: Arc<AlloyProvider>,
    /// Cache of parsed function signatures by name
    signature_cache: Arc<RwLock<HashMap<String, FunctionSignature>>>,
}

impl Contract {
    /// Create a new contract instance.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidAddress` if the address is invalid.
    pub fn new(address: &str, provider: Arc<AlloyProvider>) -> ProviderResult<Self> {
        let addr = Address::from_str(address).map_err(|_| ProviderError::InvalidAddress {
            address: address.to_string(),
            reason: "Invalid address format".to_string(),
        })?;

        Ok(Self {
            address: addr,
            provider,
            signature_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Call a contract function.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the call fails or encoding/decoding fails.
    pub async fn call(
        &self,
        function_signature: &str,
        args: &[String],
        block_number: Option<u64>,
    ) -> ProviderResult<Vec<String>> {
        // Parse function signature
        let func = self.parse_function_signature(function_signature)?;

        // Encode arguments
        let encoded_args = encode_arguments(&func.inputs, args)?;

        // Build calldata: selector + encoded_args
        let mut calldata = Vec::with_capacity(4 + encoded_args.len());
        calldata.extend_from_slice(&func.selector);
        calldata.extend_from_slice(&encoded_args);

        // Execute eth_call
        let result = self
            .provider
            .eth_call(&self.address, Bytes::from(calldata), block_number)
            .await?;

        // Decode return values
        decode_return_data(&result, &func.outputs)
    }

    /// Parse and cache a function signature.
    fn parse_function_signature(&self, signature: &str) -> ProviderResult<FunctionSignature> {
        // Try to get from cache first
        let cache = self.signature_cache.read();
        if let Some(func) = cache.get(signature) {
            return Ok(func.clone());
        }
        drop(cache);

        // Parse the signature
        let func = FunctionSignature::parse(signature)?;

        // Cache it
        self.signature_cache.write().insert(signature.to_string(), func.clone());

        Ok(func)
    }

    /// Get the contract address.
    #[must_use]
    pub const fn address(&self) -> Address {
        self.address
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_abi_type_parse() {
        assert_eq!(AbiType::parse("address").expect("address should parse"), AbiType::Address);
        assert_eq!(AbiType::parse("bool").expect("bool should parse"), AbiType::Bool);
        assert_eq!(AbiType::parse("uint256").expect("uint256 should parse"), AbiType::Uint(256));
        assert_eq!(AbiType::parse("uint8").expect("uint8 should parse"), AbiType::Uint(8));
        assert_eq!(AbiType::parse("int256").expect("int256 should parse"), AbiType::Int(256));
        assert_eq!(AbiType::parse("bytes").expect("bytes should parse"), AbiType::Bytes);
        assert_eq!(AbiType::parse("bytes32").expect("bytes32 should parse"), AbiType::FixedBytes(32));
        assert_eq!(AbiType::parse("string").expect("string should parse"), AbiType::String);

        // Array types
        assert_eq!(
            AbiType::parse("address[]").expect("address[] should parse"),
            AbiType::Array(Box::new(AbiType::Address))
        );
        assert_eq!(
            AbiType::parse("uint256[5]").expect("uint256[5] should parse"),
            AbiType::FixedArray(Box::new(AbiType::Uint(256)), 5)
        );
    }

    #[test]
    fn test_abi_type_parse_invalid() {
        assert!(AbiType::parse("invalid").is_err());
        assert!(AbiType::parse("uint").is_err()); // Missing bits
        assert!(AbiType::parse("bytes33").is_err()); // Too large
    }

    #[test]
    fn test_function_signature_parse() {
        let sig = FunctionSignature::parse("transfer(address,uint256)").expect("transfer signature should parse");
        assert_eq!(sig.name, "transfer");
        assert_eq!(sig.inputs.len(), 2);
        assert_eq!(sig.inputs[0], AbiType::Address);
        assert_eq!(sig.inputs[1], AbiType::Uint(256));
        assert!(sig.outputs.is_empty());
        assert_eq!(sig.selector.len(), 4);

        let sig = FunctionSignature::parse("balanceOf(address) returns (uint256)").expect("balanceOf signature should parse");
        assert_eq!(sig.name, "balanceOf");
        assert_eq!(sig.inputs.len(), 1);
        assert_eq!(sig.outputs.len(), 1);
        assert_eq!(sig.outputs[0], AbiType::Uint(256));
    }

    #[test]
    fn test_encode_address() {
        let addr = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let encoded = encode_value(&AbiType::Address, addr).expect("address should encode");
        assert_eq!(encoded.len(), 32);
        assert_eq!(
            &encoded[12..],
            hex::decode("742d35Cc6634C0532925a3b8D4C9db96590d6B75").expect("valid hex should decode")
        );
    }

    #[test]
    fn test_encode_bool() {
        let encoded = encode_value(&AbiType::Bool, "true").expect("bool true should encode");
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded[31], 1);

        let encoded = encode_value(&AbiType::Bool, "false").expect("bool false should encode");
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded[31], 0);
    }

    #[test]
    fn test_encode_uint() {
        let encoded = encode_value(&AbiType::Uint(256), "12345").expect("uint256 should encode");
        assert_eq!(encoded.len(), 32);
        // Last bytes should be 0x3039 (12345 in hex)
        assert_eq!(encoded[28], 0);
        assert_eq!(encoded[29], 0);
        assert_eq!(encoded[30], 0x30);
        assert_eq!(encoded[31], 0x39);
    }

    #[test]
    fn test_function_selector() {
        // transfer(address,uint256) selector
        let sig = FunctionSignature::parse("transfer(address,uint256)").expect("transfer signature should parse");
        // Expected selector: first 4 bytes of keccak256("transfer(address,uint256)")
        // Should be 0xa9059cbb
        assert_eq!(hex::encode(sig.selector), "a9059cbb");

        // balanceOf(address) selector
        let sig = FunctionSignature::parse("balanceOf(address)").expect("balanceOf signature should parse");
        // Expected: 0x70a08231
        assert_eq!(hex::encode(sig.selector), "70a08231");
    }

    #[test]
    fn test_abi_type_is_dynamic() {
        assert!(!AbiType::Address.is_dynamic());
        assert!(!AbiType::Bool.is_dynamic());
        assert!(!AbiType::Uint(256).is_dynamic());
        assert!(!AbiType::FixedBytes(32).is_dynamic());
        assert!(AbiType::Bytes.is_dynamic());
        assert!(AbiType::String.is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::String)).is_dynamic());
        assert!(AbiType::Array(Box::new(AbiType::Uint(256))).is_dynamic()); // Arrays are dynamic
    }
}
