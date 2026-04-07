//! Smart contract interface with ABI encoding/decoding.
//!
//! Provides high-level contract interaction with automatic ABI encoding
//! for function calls and automatic decoding of return values.

use crate::errors::{ContractError, ContractResult, ProviderError, ProviderResult};
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
    /// Returns `ContractError::InvalidAbi` if the signature is invalid.
    pub fn parse(signature: &str) -> ContractResult<Self> {
        let parsed = signature_parser::parse_signature(signature).map_err(|e| {
            ContractError::InvalidAbi {
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
///
/// Uses proper Solidity ABI head/tail encoding: each parameter gets a slot in
/// the head (32 bytes for dynamic types storing an offset, or the full inline
/// encoding for static types). Dynamic type data is appended in the tail.
pub fn encode_arguments(types: &[AbiType], args: &[String]) -> ContractResult<Bytes> {
    if types.len() != args.len() {
        return Err(ContractError::InvalidAbi {
            message: format!(
                "Argument count mismatch: expected {}, got {}",
                types.len(),
                args.len()
            ),
        });
    }

    let mut encoded_values: Vec<Vec<u8>> = Vec::with_capacity(types.len());
    let mut head_size: usize = 0;

    for (abi_type, arg) in types.iter().zip(args.iter()) {
        let encoded = encode_value(abi_type, arg)?;
        if abi_type.is_dynamic() {
            head_size += 32;
        } else {
            head_size += encoded.len();
        }
        encoded_values.push(encoded);
    }

    let mut head = Vec::with_capacity(head_size);
    let mut tail = Vec::new();

    for (encoded, abi_type) in encoded_values.iter().zip(types.iter()) {
        if abi_type.is_dynamic() {
            let offset = head_size + tail.len();
            head.extend_from_slice(&U256::from(offset).to_be_bytes_vec());
            tail.extend_from_slice(encoded);
        } else {
            head.extend_from_slice(encoded);
        }
    }

    head.extend(tail);
    Ok(Bytes::from(head))
}

/// Encode a single value based on its ABI type.
#[allow(clippy::too_many_lines)]
fn encode_value(abi_type: &AbiType, value: &str) -> ContractResult<Vec<u8>> {
    match abi_type {
        AbiType::Address => {
            let addr =
                Address::from_str(value.trim()).map_err(|_| ContractError::InvalidAddress {
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
                .map_err(|_| ContractError::InvalidAbi {
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
                        I256::from_str(trimmed).map_err(|_| ContractError::InvalidAbi {
                            message: format!("Invalid int{bits} value: {value}"),
                        })
                    },
                    |hex_str| {
                        U256::from_str_radix(hex_str, 16)
                            .map(I256::from_raw)
                            .map_err(|_| ContractError::InvalidAbi {
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
            let bytes = hex::decode(hex_str).map_err(|_| ContractError::InvalidAbi {
                message: format!("Invalid hex value for bytes{size}: {value}"),
            })?;
            if bytes.len() != *size {
                return Err(ContractError::InvalidAbi {
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
            let bytes = hex::decode(hex_str).map_err(|_| ContractError::InvalidAbi {
                message: format!("Invalid hex value for bytes: {value}"),
            })?;

            let mut encoded = Vec::new();

            let length = U256::from(bytes.len());
            encoded.extend_from_slice(&length.to_be_bytes_vec());

            encoded.extend_from_slice(&bytes);
            let padding = (32 - (bytes.len() % 32)) % 32;
            encoded.extend(std::iter::repeat_n(0u8, padding));

            Ok(encoded)
        }
        AbiType::String => {
            let bytes = value.as_bytes();

            let mut encoded = Vec::new();

            let length = U256::from(bytes.len());
            encoded.extend_from_slice(&length.to_be_bytes_vec());

            encoded.extend_from_slice(bytes);
            let padding = (32 - (bytes.len() % 32)) % 32;
            encoded.extend(std::iter::repeat_n(0u8, padding));

            Ok(encoded)
        }
        AbiType::Array(element_type) => encode_dynamic_array(element_type, value),
        AbiType::FixedArray(element_type, size) => encode_fixed_array(element_type, *size, value),
    }
}

/// Parse a JSON array string into individual element strings.
///
/// Accepts `["elem1", "elem2"]` format with optional whitespace.
/// Returns error if input is not a valid JSON array.
fn parse_json_array(input: &str) -> ContractResult<Vec<&str>> {
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

/// Encode a dynamic array (e.g., `uint256[]`).
fn encode_dynamic_array(element_type: &AbiType, value: &str) -> ContractResult<Vec<u8>> {
    let elements = parse_json_array(value)?;

    let encoded_elements = encode_array_elements(element_type, &elements)?;

    let mut result = Vec::with_capacity(32 + encoded_elements.len());

    let length = U256::from(elements.len());
    result.extend_from_slice(&length.to_be_bytes_vec());

    result.extend(encoded_elements);

    Ok(result)
}

/// Encode a fixed-size array (e.g., `uint256[3]`).
fn encode_fixed_array(element_type: &AbiType, size: usize, value: &str) -> ContractResult<Vec<u8>> {
    // Defensive check - should be caught at parse time
    if size == 0 {
        return Err(ContractError::InvalidAbi {
            message: "Cannot encode zero-element fixed array (e.g., uint256[0]) - not supported by Solidity".to_string(),
        });
    }

    let elements = parse_json_array(value)?;

    if elements.len() != size {
        return Err(ContractError::InvalidAbi {
            message: format!(
                "Fixed array of size {} requires exactly {} elements, got {}",
                size,
                size,
                elements.len()
            ),
        });
    }

    encode_array_elements(element_type, &elements)
}

/// Encode array elements without the array header.
fn encode_array_elements(element_type: &AbiType, elements: &[&str]) -> ContractResult<Vec<u8>> {
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
) -> ContractResult<Vec<u8>> {
    let mut result = Vec::new();

    for (i, element) in elements.iter().enumerate() {
        let encoded =
            encode_value(element_type, element).map_err(|e| ContractError::InvalidAbi {
                message: format!(
                    "Failed to encode array element {} of {}: {}",
                    i + 1,
                    elements.len(),
                    e
                ),
            })?;
        result.extend(encoded);
    }

    Ok(result)
}

/// Encode dynamic element types with proper offset handling.
fn encode_dynamic_array_elements(
    element_type: &AbiType,
    elements: &[&str],
) -> ContractResult<Vec<u8>> {
    // For dynamic elements, we need to:
    // 1. Compute the offsets for each element
    // 2. Encode the offsets
    // 3. Encode the elements themselves

    // First, pre-encode all elements to know their sizes
    let mut encoded_elements: Vec<Vec<u8>> = Vec::with_capacity(elements.len());
    for (i, element) in elements.iter().enumerate() {
        let encoded =
            encode_value(element_type, element).map_err(|e| ContractError::InvalidAbi {
                message: format!(
                    "Failed to encode array element {} of {}: {}",
                    i + 1,
                    elements.len(),
                    e
                ),
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
pub fn decode_return_data(data: &[u8], types: &[AbiType]) -> ContractResult<Vec<String>> {
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
fn decode_value(data: &[u8], offset: usize, abi_type: &AbiType) -> ContractResult<(String, usize)> {
    if data.len() < offset + 32 {
        return Err(ContractError::DecodingError {
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
                    ContractError::DecodingError {
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
                .map_err(|_| ContractError::DecodingError {
                    message: "Data offset exceeds platform addressable range".to_string(),
                })?;

            if data.len() < data_offset + 32 {
                return Err(ContractError::DecodingError {
                    message: "Invalid dynamic data offset".to_string(),
                });
            }

            let length: usize = U256::from_be_slice(&data[data_offset..data_offset + 32])
                .try_into()
                .map_err(|_| ContractError::DecodingError {
                    message: "Data length exceeds platform addressable range".to_string(),
                })?;
            let value_start = data_offset + 32;
            let value_end = value_start + length;

            if data.len() < value_end {
                return Err(ContractError::DecodingError {
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
        AbiType::Array(element_type) => decode_dynamic_array(data, offset, element_type),
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
) -> ContractResult<(String, usize)> {
    // Read the offset to array data
    let data_offset: usize = U256::from_be_slice(&data[offset..offset + 32])
        .try_into()
        .map_err(|_| ContractError::DecodingError {
            message: "Array offset exceeds platform addressable range".to_string(),
        })?;

    if data.len() < data_offset + 32 {
        return Err(ContractError::DecodingError {
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
        .map_err(|_| ContractError::DecodingError {
            message: "Array length exceeds platform addressable range".to_string(),
        })?;

    // Validate length against available data to prevent OOM from malformed input
    let elements_data_start = data_offset + 32;
    if length > 0 && elements_data_start >= data.len() {
        return Err(ContractError::DecodingError {
            message: format!(
                "Array length {} with no element data available (data length: {})",
                length,
                data.len()
            ),
        });
    }

    // For static element types, we can calculate exact minimum data required
    if !element_type.is_dynamic() {
        // Each static element requires exactly 32 bytes
        let min_data_needed = elements_data_start + (length * 32);
        if data.len() < min_data_needed {
            return Err(ContractError::DecodingError {
                message: format!(
                    "Array length {} requires at least {} bytes but only {} available",
                    length,
                    min_data_needed,
                    data.len()
                ),
            });
        }
    }

    // Decode elements
    let mut values = Vec::with_capacity(length);
    let mut element_offset = elements_data_start;

    for i in 0..length {
        let (value, new_offset) =
            decode_value(data, element_offset, element_type).map_err(|e| {
                ContractError::DecodingError {
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
) -> ContractResult<(String, usize)> {
    // Defensive check
    if size == 0 {
        return Err(ContractError::DecodingError {
            message: "Cannot decode zero-element fixed array (e.g., uint256[0]) - not supported by Solidity".to_string(),
        });
    }

    let mut values = Vec::with_capacity(size);
    let mut element_offset = offset;

    for i in 0..size {
        let (value, new_offset) =
            decode_value(data, element_offset, element_type).map_err(|e| {
                ContractError::DecodingError {
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
#[derive(Clone)]
pub struct Contract {
    address: Address,
    provider: Arc<AlloyProvider>,
    /// Cache of parsed function signatures by name
    signature_cache: Arc<RwLock<HashMap<String, Arc<FunctionSignature>>>>,
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
    /// Returns `ProviderError` or `ContractError` if the call fails or encoding/decoding fails.
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
        decode_return_data(&result, &func.outputs).map_err(Into::into)
    }

    /// Parse and cache a function signature.
    fn parse_function_signature(&self, signature: &str) -> ProviderResult<Arc<FunctionSignature>> {
        let cache = self.signature_cache.read();
        if let Some(func) = cache.get(signature) {
            return Ok(Arc::clone(func));
        }
        drop(cache);

        let func = Arc::new(FunctionSignature::parse(signature)?);

        self.signature_cache
            .write()
            .insert(signature.to_string(), Arc::clone(&func));

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
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[]")
            .expect("should encode empty array");
        assert_eq!(encoded.len(), 32);

        assert_eq!(&encoded[0..32], U256::from(0).to_be_bytes_vec().as_slice());
    }

    #[test]
    fn test_encode_dynamic_uint_array() {
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[1, 2, 3]")
            .expect("should encode uint256[]");

        assert_eq!(encoded.len(), 32 + 96);

        assert_eq!(&encoded[0..32], U256::from(3).to_be_bytes_vec().as_slice());

        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(1));
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(2));
        assert_eq!(U256::from_be_slice(&encoded[96..128]), U256::from(3));
    }

    #[test]
    fn test_encode_fixed_uint_array() {
        // [10, 20, 30] as uint256[3]
        let encoded = encode_value(
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            "[10, 20, 30]",
        )
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
        let result = encode_value(
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            "[1, 2]",
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requires exactly 3 elements"));
    }

    #[test]
    fn test_encode_dynamic_address_array() {
        let addr1 = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657";
        let input = format!("[{addr1}, {addr2}]");

        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Address)), &input)
            .expect("should encode address[]");

        assert_eq!(encoded.len(), 32 + 64);

        let addr1_encoded = &encoded[32..64];
        assert_eq!(
            &addr1_encoded[12..],
            hex::decode("742d35Cc6634C0532925a3b8D4C9db96590d6B75")
                .unwrap()
                .as_slice()
        );
    }

    #[test]
    fn test_encode_dynamic_bool_array() {
        let encoded = encode_value(
            &AbiType::Array(Box::new(AbiType::Bool)),
            "[true, false, true]",
        )
        .expect("should encode bool[]");

        assert_eq!(encoded.len(), 32 + 96);

        assert_eq!(encoded[32 + 31], 1);
        assert_eq!(encoded[64 + 31], 0);
        assert_eq!(encoded[96 + 31], 1);
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
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::String)), "[hello, world]")
            .expect("should encode string[]");

        // Dynamic array of dynamic strings has complex layout
        // Just verify it encodes without error
        assert!(encoded.len() >= 64);
    }

    #[test]
    fn test_encode_bytes_array() {
        // [0x1234, 0x5678] as bytes[]
        let encoded = encode_value(
            &AbiType::Array(Box::new(AbiType::Bytes)),
            "[0x1234, 0x5678]",
        )
        .expect("should encode bytes[]");

        assert!(encoded.len() >= 64);
    }

    // =========================================================================
    // encode_arguments: correct ABI head/tail offset encoding
    // =========================================================================

    /// Wrap encoded data with a 32-byte offset prefix pointing to the data.
    fn wrap_with_offset(data: &[u8]) -> Vec<u8> {
        let mut wrapped = Vec::with_capacity(32 + data.len());
        wrapped.extend_from_slice(&U256::from(32).to_be_bytes_vec());
        wrapped.extend_from_slice(data);
        wrapped
    }

    #[test]
    fn test_encode_arguments_static_then_dynamic() {
        // f(uint256, bytes) with (1, 0x1234)
        // ABI encoding:
        //   Head: [uint256(1)][offset=64]
        //   Tail: [length=2][0x1234 padded to 32 bytes]
        let types = vec![AbiType::Uint(256), AbiType::Bytes];
        let args = vec!["1".to_string(), "0x1234".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode mixed args");

        assert_eq!(encoded.len(), 128);

        // Head slot 0: uint256 = 1
        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(1));

        // Head slot 1: offset = 64 (2 head slots × 32 bytes)
        let offset = U256::from_be_slice(&encoded[32..64]);
        assert_eq!(offset, U256::from(64), "offset should be 64, got {offset}");

        // Tail: bytes length = 2
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(2));

        // Tail: bytes data
        assert_eq!(encoded[96], 0x12);
        assert_eq!(encoded[97], 0x34);
    }

    #[test]
    fn test_encode_arguments_two_dynamic_params() {
        // f(bytes, bytes) with (0x1234, 0x5678)
        // ABI encoding:
        //   Head: [offset=64][offset=128]
        //   Tail: [length=2][0x1234+pad][length=2][0x5678+pad]
        let types = vec![AbiType::Bytes, AbiType::Bytes];
        let args = vec!["0x1234".to_string(), "0x5678".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        assert_eq!(encoded.len(), 192);

        // Head: offsets
        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(64));
        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(128));

        // First bytes tail
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(2));
        assert_eq!(encoded[96], 0x12);
        assert_eq!(encoded[97], 0x34);

        // Second bytes tail
        assert_eq!(U256::from_be_slice(&encoded[128..160]), U256::from(2));
        assert_eq!(encoded[160], 0x56);
        assert_eq!(encoded[161], 0x78);
    }

    #[test]
    fn test_encode_arguments_static_dynamic_static_dynamic() {
        // f(uint256, string, bool, bytes) with (42, "hello", true, 0xabcd)
        // Head size = 4 × 32 = 128
        // Offsets point into the tail past the 128-byte head
        let types = vec![
            AbiType::Uint(256),
            AbiType::String,
            AbiType::Bool,
            AbiType::Bytes,
        ];
        let args = vec![
            "42".to_string(),
            "hello".to_string(),
            "true".to_string(),
            "0xabcd".to_string(),
        ];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        // Head
        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(42)); // uint256
        let string_offset = U256::from_be_slice(&encoded[32..64]);
        assert_eq!(
            string_offset,
            U256::from(128),
            "string offset should be 128"
        );
        assert_eq!(encoded[64 + 31], 1); // bool true
        let bytes_offset = U256::from_be_slice(&encoded[96..128]);
        assert_eq!(bytes_offset, U256::from(192), "bytes offset should be 192");

        // String tail at offset 128
        assert_eq!(U256::from_be_slice(&encoded[128..160]), U256::from(5)); // "hello" length
        assert_eq!(&encoded[160..165], b"hello");

        // Bytes tail at offset 192
        assert_eq!(U256::from_be_slice(&encoded[192..224]), U256::from(2)); // 0xabcd length
        assert_eq!(encoded[224], 0xab);
        assert_eq!(encoded[225], 0xcd);
    }

    #[test]
    fn test_encode_arguments_static_fixed_array_and_dynamic() {
        // f(uint256[3], bytes) with ([10, 20, 30], 0xff)
        // uint256[3] is static (96 bytes inline in head)
        // Head size = 96 + 32 = 128
        let types = vec![
            AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            AbiType::Bytes,
        ];
        let args = vec!["[10, 20, 30]".to_string(), "0xff".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        // Static uint256[3] inline
        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(10));
        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(20));
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(30));

        // Offset to bytes = 128 (96 static + 32 offset slot)
        let offset = U256::from_be_slice(&encoded[96..128]);
        assert_eq!(
            offset,
            U256::from(128),
            "offset should be 128, got {offset}"
        );

        // Bytes tail
        assert_eq!(U256::from_be_slice(&encoded[128..160]), U256::from(1));
        assert_eq!(encoded[160], 0xff);
    }

    #[test]
    fn test_encode_arguments_dynamic_array_with_static_elements() {
        // f(uint256[], uint256) with ([1,2,3], 42)
        // Head: [offset_to_array][uint256(42)]
        // Tail: [length=3][1][2][3]
        let types = vec![
            AbiType::Array(Box::new(AbiType::Uint(256))),
            AbiType::Uint(256),
        ];
        let args = vec!["[1, 2, 3]".to_string(), "42".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        // Head slot 0: offset to array data = 64
        let offset = U256::from_be_slice(&encoded[0..32]);
        assert_eq!(
            offset,
            U256::from(64),
            "array offset should be 64, got {offset}"
        );

        // Head slot 1: uint256 = 42
        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(42));

        // Tail: array length + elements
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(3));
        assert_eq!(U256::from_be_slice(&encoded[96..128]), U256::from(1));
        assert_eq!(U256::from_be_slice(&encoded[128..160]), U256::from(2));
        assert_eq!(U256::from_be_slice(&encoded[160..192]), U256::from(3));
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
        let encoded = encode_value(&AbiType::Array(Box::new(AbiType::Uint(256))), "[1, 2, 3]")
            .expect("should encode");

        let wrapped = wrap_with_offset(&encoded);
        let (value, _) = decode_value(&wrapped, 0, &AbiType::Array(Box::new(AbiType::Uint(256))))
            .expect("should decode");

        assert!(value.contains('1'));
        assert!(value.contains('2'));
        assert!(value.contains('3'));
        assert!(value.starts_with('['));
        assert!(value.ends_with(']'));
    }

    #[test]
    fn test_decode_fixed_uint_array() {
        // Encode [10, 20, 30] as uint256[3]
        let encoded = encode_value(
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            "[10, 20, 30]",
        )
        .expect("should encode");

        let (value, consumed) = decode_value(
            &encoded,
            0,
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
        )
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

        let wrapped = wrap_with_offset(&encoded);
        let (value, _) = decode_value(&wrapped, 0, &AbiType::Array(Box::new(AbiType::Address)))
            .expect("should decode");

        assert!(value.contains("742d35cc6634c0532925a3b8d4c9db96590d6b75"));
    }

    #[test]
    fn test_array_roundtrip() {
        let test_cases = vec![
            ("[]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[1]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[1, 2, 3]", AbiType::Array(Box::new(AbiType::Uint(256)))),
            ("[true, false]", AbiType::Array(Box::new(AbiType::Bool))),
        ];

        for (input, abi_type) in test_cases {
            let encoded = encode_value(&abi_type, input).expect("should encode");

            let wrapped = wrap_with_offset(&encoded);
            let (decoded, _) = decode_value(&wrapped, 0, &abi_type).expect("should decode");

            assert!(
                decoded.starts_with('[') && decoded.ends_with(']'),
                "Decoded value should be an array: {decoded}"
            );
        }
    }

    // =========================================================================
    // Invalid length validation tests
    // =========================================================================

    #[test]
    fn test_decode_dynamic_array_with_length_exceeding_data() {
        // Craft malformed ABI data where length claims more elements than data can contain
        // Structure:
        // - bytes 0..32: offset = 32 (points to length field)
        // - bytes 32..64: length = 1_000_000 (claims 1 million elements)
        // - bytes 64..96: only 1 element worth of data
        // Total: 96 bytes, but claims 1 million * 32 = 32 million bytes of elements
        let mut data = vec![0u8; 96];

        // Offset to array data (right after this offset field)
        data[0..32].copy_from_slice(&U256::from(32).to_be_bytes_vec());

        // Malicious length: claims 1 million elements
        data[32..64].copy_from_slice(&U256::from(1_000_000).to_be_bytes_vec());

        // Only 1 element of actual data
        data[64..96].copy_from_slice(&U256::from(42).to_be_bytes_vec());

        // Decoding should fail with a clear error, not panic or allocate massive memory
        let result = decode_value(&data, 0, &AbiType::Array(Box::new(AbiType::Uint(256))));

        assert!(
            result.is_err(),
            "Should reject array with length exceeding available data"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("requires at least") && err.to_string().contains("available"),
            "Error should mention bytes required vs available: {err}"
        );
    }

    #[test]
    fn test_decode_dynamic_array_with_valid_length_at_boundary() {
        // Test that valid arrays at the data boundary still work
        // Structure:
        // - bytes 0..32: offset = 32
        // - bytes 32..64: length = 2
        // - bytes 64..128: 2 elements (64 bytes)
        // Total: 128 bytes, length = 2, exactly fits
        let mut data = vec![0u8; 128];

        data[0..32].copy_from_slice(&U256::from(32).to_be_bytes_vec());
        data[32..64].copy_from_slice(&U256::from(2).to_be_bytes_vec());
        data[64..96].copy_from_slice(&U256::from(1).to_be_bytes_vec());
        data[96..128].copy_from_slice(&U256::from(2).to_be_bytes_vec());

        let (value, consumed) =
            decode_value(&data, 0, &AbiType::Array(Box::new(AbiType::Uint(256))))
                .expect("should decode valid array");

        assert!(value.contains('1'));
        assert!(value.contains('2'));
        assert_eq!(consumed, 32);
    }
}
