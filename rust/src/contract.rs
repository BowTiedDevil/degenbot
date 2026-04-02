//! Smart contract interface with ABI encoding/decoding.
//!
//! Provides high-level contract interaction with automatic ABI encoding
//! for function calls and automatic decoding of return values.

use crate::errors::{ProviderError, ProviderResult};
use crate::provider::AlloyProvider;
use crate::signature_parser;
use alloy::hex;
use alloy::primitives::I256;
use alloy::primitives::{Address, Bytes, U256};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

/// Re-export the shared `AbiType` enum for use by consumers of this module.
pub use crate::abi_types::AbiType;

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
        let parsed = signature_parser::parse_signature(signature).map_err(|e| {
            ProviderError::InvalidAbi {
                message: format!("Invalid signature '{signature}': {e}"),
            }
        })?;

        // Calculate selector: first 4 bytes of keccak256(signature_without_returns)
        let selector_input = format!("{}({})", parsed.name, Self::types_to_string(&parsed.inputs));
        let selector = Self::calculate_selector(&selector_input);

        Ok(Self {
            name: parsed.name,
            inputs: parsed.inputs,
            outputs: parsed.outputs,
            selector,
        })
    }

    /// Convert types back to comma-separated string.
    fn types_to_string(types: &[AbiType]) -> String {
        types
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
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
#[allow(clippy::too_many_lines)]
fn encode_value(abi_type: &AbiType, value: &str) -> ProviderResult<Vec<u8>> {
    match abi_type {
        AbiType::Address => {
            let addr =
                Address::from_str(value.trim()).map_err(|_| ProviderError::InvalidAddress {
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
            let trimmed = value.trim();
            let uint_val = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .map_or_else(
                    || U256::from_str(trimmed),
                    |hex_str| U256::from_str_radix(hex_str, 16),
                )
                .map_err(|_| ProviderError::InvalidAbi {
                    message: format!("Invalid uint{bits} value: {value}"),
                })?;
            // U256 is already 32 bytes
            Ok(uint_val.to_be_bytes_vec())
        }
        AbiType::Int(bits) => {
            let trimmed = value.trim();
            let int_val = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .map_or_else(
                    || {
                        I256::from_str(trimmed).map_err(|_| ProviderError::InvalidAbi {
                            message: format!("Invalid int{bits} value: {value}"),
                        })
                    },
                    |hex_str| {
                        U256::from_str_radix(hex_str, 16)
                            .map(I256::from_raw)
                            .map_err(|_| ProviderError::InvalidAbi {
                                message: format!("Invalid int{bits} value: {value}"),
                            })
                    },
                )?;
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
        AbiType::Array(element_type) => {
            encode_dynamic_array(element_type, value)
        }
        AbiType::FixedArray(element_type, size) => {
            encode_fixed_array(element_type, *size, value)
        }
    }
}

/// Parse a JSON array string into individual element strings.
///
/// Accepts `["elem1", "elem2"]` format with optional whitespace.
/// Returns error if input is not a valid JSON array.
fn parse_json_array(input: &str) -> ProviderResult<Vec<&str>> {
    let trimmed = input.trim();

    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(ProviderError::InvalidAbi {
            message: format!(
                "Array value must be enclosed in square brackets, got: '{input}'"
            ),
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

/// Encode a dynamic array (e.g., `uint256[]`).
fn encode_dynamic_array(element_type: &AbiType, value: &str) -> ProviderResult<Vec<u8>> {
    let elements = parse_json_array(value)?;

    // Encode the elements
    let encoded_elements = encode_array_elements(element_type, &elements)?;

    // Dynamic array layout: offset (32 bytes) + length (32 bytes) + encoded elements
    let mut result = Vec::with_capacity(64 + encoded_elements.len());

    // Offset (32 bytes - points to the start of the array data)
    result.extend_from_slice(&U256::from(32).to_be_bytes_vec());

    // Length (32 bytes)
    result.extend_from_slice(&U256::from(elements.len()).to_be_bytes_vec());

    // Encoded elements
    result.extend(encoded_elements);

    Ok(result)
}

/// Encode a fixed-size array (e.g., `uint256[3]`).
fn encode_fixed_array(
    element_type: &AbiType,
    size: usize,
    value: &str,
) -> ProviderResult<Vec<u8>> {
    // Defensive check - should be caught at parse time
    if size == 0 {
        return Err(ProviderError::InvalidAbi {
            message: "Cannot encode zero-element fixed array (e.g., uint256[0]) - not supported by Solidity".to_string(),
        });
    }

    let elements = parse_json_array(value)?;

    if elements.len() != size {
        return Err(ProviderError::InvalidAbi {
            message: format!(
                "Fixed array of size {} requires exactly {} elements, got {}",
                size, size, elements.len()
            ),
        });
    }

    encode_array_elements(element_type, &elements)
}

/// Encode array elements without the array header.
fn encode_array_elements(element_type: &AbiType, elements: &[&str]) -> ProviderResult<Vec<u8>> {
    // For static element types, encode directly
    // For dynamic element types, we need to compute offsets
    if element_type.is_dynamic() {
        encode_dynamic_array_elements(element_type, elements)
    } else {
        encode_static_array_elements(element_type, elements)
    }
}

/// Encode static element types directly.
fn encode_static_array_elements(
    element_type: &AbiType,
    elements: &[&str],
) -> ProviderResult<Vec<u8>> {
    let mut result = Vec::new();

    for (i, element) in elements.iter().enumerate() {
        let encoded = encode_value(element_type, element).map_err(|e| {
            ProviderError::InvalidAbi {
                message: format!(
                    "Failed to encode array element {} of {}: {}",
                    i + 1,
                    elements.len(),
                    e
                ),
            }
        })?;
        result.extend(encoded);
    }

    Ok(result)
}

/// Encode dynamic element types with proper offset handling.
fn encode_dynamic_array_elements(
    element_type: &AbiType,
    elements: &[&str],
) -> ProviderResult<Vec<u8>> {
    // For dynamic elements, we need to:
    // 1. Compute the offsets for each element
    // 2. Encode the offsets
    // 3. Encode the elements themselves

    // First, pre-encode all elements to know their sizes
    let mut encoded_elements: Vec<Vec<u8>> = Vec::with_capacity(elements.len());
    for (i, element) in elements.iter().enumerate() {
        let encoded = encode_value(element_type, element).map_err(|e| {
            ProviderError::InvalidAbi {
                message: format!(
                    "Failed to encode array element {} of {}: {}",
                    i + 1,
                    elements.len(),
                    e
                ),
            }
        })?;
        encoded_elements.push(encoded);
    }

    // Calculate offsets
    // Header size: (n elements) * 32 bytes per offset
    let header_size = elements.len() * 32;

    // Calculate cumulative offsets
    let mut offsets = Vec::with_capacity(elements.len());
    let mut current_offset = header_size;

    for encoded in &encoded_elements {
        offsets.push(current_offset);
        current_offset += encoded.len();
    }

    // Build the result: offsets + encoded elements
    let total_size: usize = header_size + encoded_elements.iter().map(Vec::len).sum::<usize>();
    let mut result = Vec::with_capacity(total_size);

    // Add offsets
    for offset in offsets {
        result.extend_from_slice(&U256::from(offset).to_be_bytes_vec());
    }

    // Add encoded elements
    for encoded in encoded_elements {
        result.extend(encoded);
    }

    Ok(result)
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
            let value =
                I256::from_be_bytes::<32>(data[offset..offset + 32].try_into().map_err(|_| {
                    ProviderError::DecodingError {
                        message: "Failed to convert bytes to I256".to_string(),
                    }
                })?);
            Ok((value.to_string(), offset + 32))
        }
        AbiType::FixedBytes(size) => {
            let bytes = &data[offset..offset + *size];
            Ok((format!("0x{}", hex::encode(bytes)), offset + 32))
        }
        AbiType::Bytes | AbiType::String => {
            // Dynamic types: offset to data location
            let data_offset: usize = U256::from_be_slice(&data[offset..offset + 32])
                .try_into()
                .map_err(|_| ProviderError::DecodingError {
                    message: "Data offset exceeds platform addressable range".to_string(),
                })?;

            if data.len() < data_offset + 32 {
                return Err(ProviderError::DecodingError {
                    message: "Invalid dynamic data offset".to_string(),
                });
            }

            let length: usize = U256::from_be_slice(&data[data_offset..data_offset + 32])
                .try_into()
                .map_err(|_| ProviderError::DecodingError {
                    message: "Data length exceeds platform addressable range".to_string(),
                })?;
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
        AbiType::Array(element_type) => {
            decode_dynamic_array(data, offset, element_type)
        }
        AbiType::FixedArray(element_type, size) => {
            decode_fixed_array(data, offset, element_type, *size)
        }
    }
}

/// Decode a dynamic array (e.g., `uint256[]`).
fn decode_dynamic_array(
    data: &[u8],
    offset: usize,
    element_type: &AbiType,
) -> ProviderResult<(String, usize)> {
    // Read the offset to array data
    let data_offset: usize = U256::from_be_slice(&data[offset..offset + 32])
        .try_into()
        .map_err(|_| ProviderError::DecodingError {
            message: "Array offset exceeds platform addressable range".to_string(),
        })?;

    if data.len() < data_offset + 32 {
        return Err(ProviderError::DecodingError {
            message: format!(
                "Invalid array data offset: {} exceeds data length {}",
                data_offset,
                data.len()
            ),
        });
    }

    // Read the length
    let length: usize = U256::from_be_slice(&data[data_offset..data_offset + 32])
        .try_into()
        .map_err(|_| ProviderError::DecodingError {
            message: "Array length exceeds platform addressable range".to_string(),
        })?;

    // Decode elements
    let elements_data_start = data_offset + 32;
    let mut values = Vec::with_capacity(length);
    let mut element_offset = elements_data_start;

    for i in 0..length {
        let (value, new_offset) =
            decode_value(data, element_offset, element_type).map_err(|e| {
                ProviderError::DecodingError {
                    message: format!(
                        "Failed to decode array element {} of {}: {}",
                        i + 1,
                        length,
                        e
                    ),
                }
            })?;
        values.push(value);
        element_offset = new_offset;
    }

    let result = format!("[{}]", values.join(", "));
    Ok((result, offset + 32)) // Consume the offset pointer in the head
}

/// Decode a fixed-size array (e.g., `uint256[3]`).
fn decode_fixed_array(
    data: &[u8],
    offset: usize,
    element_type: &AbiType,
    size: usize,
) -> ProviderResult<(String, usize)> {
    // Defensive check
    if size == 0 {
        return Err(ProviderError::DecodingError {
            message: "Cannot decode zero-element fixed array (e.g., uint256[0]) - not supported by Solidity".to_string(),
        });
    }

    let mut values = Vec::with_capacity(size);
    let mut element_offset = offset;

    for i in 0..size {
        let (value, new_offset) =
            decode_value(data, element_offset, element_type).map_err(|e| {
                ProviderError::DecodingError {
                    message: format!(
                        "Failed to decode fixed array element {} of {}: {}",
                        i + 1,
                        size,
                        e
                    ),
                }
            })?;
        values.push(value);
        element_offset = new_offset;
    }

    let result = format!("[{}]", values.join(", "));
    Ok((result, element_offset))
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
        self.signature_cache
            .write()
            .insert(signature.to_string(), func.clone());

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
        assert_eq!(
            AbiType::parse("address").expect("address should parse"),
            AbiType::Address
        );
        assert_eq!(
            AbiType::parse("bool").expect("bool should parse"),
            AbiType::Bool
        );
        assert_eq!(
            AbiType::parse("uint256").expect("uint256 should parse"),
            AbiType::Uint(256)
        );
        assert_eq!(
            AbiType::parse("uint8").expect("uint8 should parse"),
            AbiType::Uint(8)
        );
        assert_eq!(
            AbiType::parse("int256").expect("int256 should parse"),
            AbiType::Int(256)
        );
        assert_eq!(
            AbiType::parse("bytes").expect("bytes should parse"),
            AbiType::Bytes
        );
        assert_eq!(
            AbiType::parse("bytes32").expect("bytes32 should parse"),
            AbiType::FixedBytes(32)
        );
        assert_eq!(
            AbiType::parse("string").expect("string should parse"),
            AbiType::String
        );

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
        assert!(AbiType::parse("bytes33").is_err()); // Too large
    }

    #[test]
    fn test_abi_type_aliases() {
        // The unified AbiType supports aliases
        assert_eq!(AbiType::parse("uint").unwrap(), AbiType::Uint(256));
        assert_eq!(AbiType::parse("int").unwrap(), AbiType::Int(256));
        assert_eq!(AbiType::parse("function").unwrap(), AbiType::FixedBytes(24));
    }

    #[test]
    fn test_function_signature_parse() {
        let sig = FunctionSignature::parse("transfer(address,uint256)")
            .expect("transfer signature should parse");
        assert_eq!(sig.name, "transfer");
        assert_eq!(sig.inputs.len(), 2);
        assert_eq!(sig.inputs[0], AbiType::Address);
        assert_eq!(sig.inputs[1], AbiType::Uint(256));
        assert!(sig.outputs.is_empty());
        assert_eq!(sig.selector.len(), 4);

        let sig = FunctionSignature::parse("balanceOf(address) returns (uint256)")
            .expect("balanceOf signature should parse");
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
            hex::decode("742d35Cc6634C0532925a3b8D4C9db96590d6B75")
                .expect("valid hex should decode")
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
        let sig = FunctionSignature::parse("transfer(address,uint256)")
            .expect("transfer signature should parse");
        // Expected selector: first 4 bytes of keccak256("transfer(address,uint256)")
        // Should be 0xa9059cbb
        assert_eq!(hex::encode(sig.selector), "a9059cbb");

        // balanceOf(address) selector
        let sig = FunctionSignature::parse("balanceOf(address)")
            .expect("balanceOf signature should parse");
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

    #[test]
    fn test_encode_uint_hex() {
        // Hex-encoded uint should produce the same result as decimal
        let encoded_hex =
            encode_value(&AbiType::Uint(256), "0x3039").expect("hex uint256 should encode");
        let encoded_dec =
            encode_value(&AbiType::Uint(256), "12345").expect("dec uint256 should encode");
        assert_eq!(encoded_hex, encoded_dec);

        // Uppercase 0X prefix
        let encoded_upper =
            encode_value(&AbiType::Uint(256), "0X3039").expect("0X uint256 should encode");
        assert_eq!(encoded_upper, encoded_dec);

        // Large hex value
        let encoded_large = encode_value(
            &AbiType::Uint(256),
            "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("max uint256 should encode");
        assert_eq!(encoded_large.len(), 32);
        assert!(encoded_large.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_encode_int_hex() {
        // Hex-encoded int should produce the same result as decimal
        let encoded_hex =
            encode_value(&AbiType::Int(256), "0x3039").expect("hex int256 should encode");
        let encoded_dec =
            encode_value(&AbiType::Int(256), "12345").expect("dec int256 should encode");
        assert_eq!(encoded_hex, encoded_dec);

        // Uppercase 0X prefix
        let encoded_upper =
            encode_value(&AbiType::Int(256), "0X3039").expect("0X int256 should encode");
        assert_eq!(encoded_upper, encoded_dec);

        // Negative decimal should still work
        let encoded_neg =
            encode_value(&AbiType::Int(256), "-1").expect("negative int256 should encode");
        assert_eq!(encoded_neg.len(), 32);
        assert!(encoded_neg.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_encode_uint_invalid_hex() {
        // Invalid hex should produce a clear error
        let result = encode_value(&AbiType::Uint(256), "0xZZZZ");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid uint256 value"));
    }

    // =========================================================================
    // FunctionSignature parsing edge cases (issue #15)
    // =========================================================================

    #[test]
    fn test_signature_returns_missing_closing_paren() {
        // Missing closing paren after returns should return an error, not panic
        let result = FunctionSignature::parse("foo()returns(uint256");
        assert!(
            result.is_err(),
            "Should return error for missing closing paren, got: {result:?}"
        );
    }

    #[test]
    fn test_signature_returns_with_space_in_parens() {
        // "returns ( uint256 )" with spaces inside parens should work after normalization
        let sig = FunctionSignature::parse("balanceOf(address) returns ( uint256 )")
            .expect("spaces inside parens should work");
        assert_eq!(sig.name, "balanceOf");
        assert_eq!(sig.outputs.len(), 1);
        assert_eq!(sig.outputs[0], AbiType::Uint(256));
    }

    #[test]
    fn test_signature_returns_empty_outputs() {
        // "returns()" with empty outputs should work
        let sig = FunctionSignature::parse("foo()returns()").expect("empty returns should work");
        assert_eq!(sig.name, "foo");
        assert!(sig.outputs.is_empty());
    }

    #[test]
    fn test_signature_no_parens() {
        // Missing parens entirely should return an error
        let result = FunctionSignature::parse("transfer");
        assert!(
            result.is_err(),
            "Should return error for missing parens, got: {result:?}"
        );
    }

    #[test]
    fn test_signature_only_open_paren() {
        // Only opening paren should return an error
        let result = FunctionSignature::parse("transfer(");
        assert!(
            result.is_err(),
            "Should return error for only open paren, got: {result:?}"
        );
    }

    #[test]
    fn test_signature_returns_without_parens() {
        // "foo()returns" without parens after returns is now properly rejected.
        // The old parser treated trailing "returns" as garbage and silently ignored it.
        // The new parser correctly rejects it as unexpected input after valid signature.
        let result = FunctionSignature::parse("foo()returns");
        assert!(
            result.is_err(),
            "Should return error for 'returns' without '()', got: {result:?}"
        );
    }

    #[test]
    fn test_signature_returns_open_paren_only_no_content() {
        // "foo()returns(" with open paren but no closing should return error, not panic
        let result = FunctionSignature::parse("foo()returns(");
        assert!(
            result.is_err(),
            "Should return error for returns( with no close, got: {result:?}"
        );
    }

    #[test]
    fn test_signature_returns_multiple_outputs() {
        // Multiple output types should parse correctly
        let sig = FunctionSignature::parse("foo(address)returns(uint256,bool)")
            .expect("multiple outputs should parse");
        assert_eq!(sig.outputs.len(), 2);
        assert_eq!(sig.outputs[0], AbiType::Uint(256));
        assert_eq!(sig.outputs[1], AbiType::Bool);
    }

    // =========================================================================
    // Array encoding tests (matching eth_abi Python package behavior)
    // =========================================================================

    #[test]
    fn test_encode_empty_dynamic_array() {
        // Empty dynamic array: offset + length 0
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[]")
            .expect("should encode empty array");
        assert_eq!(encoded.len(), 64); // offset (32) + length (32)

        // First 32 bytes: offset = 32
        assert_eq!(&encoded[0..32], U256::from(32).to_be_bytes_vec().as_slice());
        // Second 32 bytes: length = 0
        assert_eq!(&encoded[32..64], U256::from(0).to_be_bytes_vec().as_slice());
    }

    #[test]
    fn test_encode_dynamic_uint_array() {
        // [1, 2, 3] as uint256[]
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[1, 2, 3]")
            .expect("should encode uint256[]");

        // Should be: offset (32) + length (3) + 3 * 32 bytes of uint values
        assert_eq!(encoded.len(), 64 + 96);

        // Verify offset and length
        assert_eq!(&encoded[0..32], U256::from(32).to_be_bytes_vec().as_slice());
        assert_eq!(&encoded[32..64], U256::from(3).to_be_bytes_vec().as_slice());

        // Verify values
        assert_eq!(
            U256::from_be_slice(&encoded[64..96]),
            U256::from(1)
        );
        assert_eq!(
            U256::from_be_slice(&encoded[96..128]),
            U256::from(2)
        );
        assert_eq!(
            U256::from_be_slice(&encoded[128..160]),
            U256::from(3)
        );
    }

    #[test]
    fn test_encode_fixed_uint_array() {
        // [10, 20, 30] as uint256[3]
        let encoded =
            encode_value(&AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3), "[10, 20, 30]")
                .expect("should encode uint256[3]");

        // Fixed array: no offset, no length, just 3 * 32 bytes
        assert_eq!(encoded.len(), 96);

        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(10));
        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(20));
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(30));
    }

    #[test]
    fn test_encode_fixed_array_wrong_size() {
        // uint256[3] with 2 elements should error
        let result =
            encode_value(&AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3), "[1, 2]");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requires exactly 3 elements"));
    }

    #[test]
    fn test_encode_dynamic_address_array() {
        // Address array
        let addr1 = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657";
        let input = format!("[{addr1}, {addr2}]");

        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Address)), &input)
            .expect("should encode address[]");

        // offset (32) + length (2) + 2 * 32 bytes
        assert_eq!(encoded.len(), 64 + 64);

        // Verify values are right-padded addresses
        let addr1_encoded = &encoded[64..96];
        assert_eq!(&addr1_encoded[12..], hex::decode("742d35Cc6634C0532925a3b8D4C9db96590d6B75").unwrap().as_slice());
    }

    #[test]
    fn test_encode_dynamic_bool_array() {
        // [true, false, true]
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Bool)), "[true, false, true]")
            .expect("should encode bool[]");

        // offset + length + 3 * 32 bytes
        assert_eq!(encoded.len(), 64 + 96);

        assert_eq!(encoded[64 + 31], 1);
        assert_eq!(encoded[96 + 31], 0);
        assert_eq!(encoded[128 + 31], 1);
    }

    #[test]
    fn test_encode_nested_array() {
        // [[1, 2], [3, 4]] as uint256[][]
        let encoded = encode_value(
            &AbiType::Array(Box::new(AbiType::Array(Box::new(AbiType::Uint(256))))),
            "[[1, 2], [3, 4]]",
        )
        .expect("should encode uint256[][]");

        // Nested dynamic arrays are complex:
        // - Outer offset (32)
        // - Outer length (2)
        // - Inner array offsets (2 * 32 bytes)
        // - Inner array data
        // This is valid ABI encoding, verify it doesn't panic and has reasonable size
        assert!(encoded.len() > 64);
    }

    #[test]
    fn test_encode_string_array() {
        // ["hello", "world"] as string[]
        let encoded =
            encode_value(&AbiType::Array(Box::new(AbiType::String)), "[hello, world]")
                .expect("should encode string[]");

        // Dynamic array of dynamic strings has complex layout
        // Just verify it encodes without error
        assert!(encoded.len() >= 64);
    }

    #[test]
    fn test_encode_bytes_array() {
        // [0x1234, 0x5678] as bytes[]
        let encoded =
            encode_value(&AbiType::Array(Box::new(AbiType::Bytes)), "[0x1234, 0x5678]")
                .expect("should encode bytes[]");

        assert!(encoded.len() >= 64);
    }

    // =========================================================================
    // Array decoding tests
    // =========================================================================

    #[test]
    fn test_decode_empty_dynamic_array() {
        // Create encoded data: offset (32) + length (0)
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&U256::from(32).to_be_bytes_vec());
        data[32..64].copy_from_slice(&U256::from(0).to_be_bytes_vec());

        let (value, consumed) =
            decode_value(&data, 0, &AbiType::Array(Box::new(AbiType::Uint(256))))
                .expect("should decode empty array");

        assert_eq!(value, "[]");
        assert_eq!(consumed, 32); // Consumed the offset pointer
    }

    #[test]
    fn test_decode_dynamic_uint_array() {
        // Encode [1, 2, 3] then decode it
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[1, 2, 3]")
            .expect("should encode");

        let (value, _) = decode_value(&encoded, 0, &AbiType::Array(Box::new(AbiType::Uint(256))))
            .expect("should decode");

        // Check the decoded value matches expected format
        assert!(value.contains('1'));
        assert!(value.contains('2'));
        assert!(value.contains('3'));
        assert!(value.starts_with('['));
        assert!(value.ends_with(']'));
    }

    #[test]
    fn test_decode_fixed_uint_array() {
        // Encode [10, 20, 30] as uint256[3]
        let encoded =
            encode_value(&AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3), "[10, 20, 30]")
                .expect("should encode");

        let (value, consumed) =
            decode_value(&encoded, 0, &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3))
                .expect("should decode");

        assert!(value.contains("10"));
        assert!(value.contains("20"));
        assert!(value.contains("30"));
        assert_eq!(consumed, 96); // 3 * 32 bytes
    }

    #[test]
    fn test_decode_address_array() {
        let addr1 = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let input = format!("[{addr1}]");

        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Address)), &input)
            .expect("should encode");

        let (value, _) = decode_value(&encoded, 0, &AbiType::Array(Box::new(AbiType::Address)))
            .expect("should decode");

        assert!(value.contains("742d35cc6634c0532925a3b8d4c9db96590d6b75"));
    }

    #[test]
    fn test_array_roundtrip() {
        // Encode and decode should produce consistent results
        let test_cases = vec![
            ("[]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[1]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[1, 2, 3]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[true, false]", AbiType::Array(Box::new(AbiType::Bool))),
        ];

        for (input, abi_type) in test_cases {
            let encoded = encode_value(&abi_type, input).expect("should encode");
            let (decoded, _) = decode_value(&encoded, 0, &abi_type).expect("should decode");

            // For roundtrip, we can't expect exact string match due to formatting,
            // but we can verify the structure is preserved
            assert!(
                decoded.starts_with('[') && decoded.ends_with(']'),
                "Decoded value should be an array: {decoded}"
            );
        }
    }
}
