//! Smart contract interface with ABI encoding/decoding.
//!
//! Provides high-level contract interaction with automatic ABI encoding
//! for function calls and automatic decoding of return values.
//!
//! # Architecture
//!
//! This module uses `AbiType` and `AbiValue` from `abi_types` as the unified
//! type system. Encoding and decoding delegate to `abi_encoder` and
//! `abi_decoder` modules.
//!
//! # Encoding Flow
//!
//! ```text
//! Python strings -> AbiValue::from_str_arg() -> encode_for_types() -> Bytes
//! ```
//!
//! # Decoding Flow
//!
//! ```text
//! Bytes -> decode_for_types() -> Vec<AbiValue> -> Vec<String>
//! ```
//!
//! The `encode_for_types` and `decode_for_types` functions take `&[AbiType]`
//! directly, avoiding string parsing overhead.

use crate::abi_types::{AbiType, AbiValue, CachedAbiTypes};
use crate::errors::{ContractError, ContractResult, ProviderResult};
use crate::provider::AlloyProvider;
use crate::signature_parser;
use alloy::primitives::{Address, Bytes};
use dashmap::DashMap;
use std::sync::Arc;

/// Parsed function signature with pre-built cached types for encoding/decoding.
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
    /// Pre-built cached types for encoding input arguments.
    /// `None` when inputs are empty (no encoding needed).
    input_cache: Option<Arc<CachedAbiTypes>>,
    /// Pre-built cached types for decoding output values.
    /// `None` when outputs are empty (no decoding needed).
    output_cache: Option<Arc<CachedAbiTypes>>,
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

        // Pre-build CachedAbiTypes for encoding/decoding so the hot path
        // (Contract::call) never re-parses types.
        let input_cache = if parsed.inputs.is_empty() {
            None
        } else {
            Some(CachedAbiTypes::from_abi_types(&parsed.inputs)?)
        };
        let output_cache = if parsed.outputs.is_empty() {
            None
        } else {
            Some(CachedAbiTypes::from_abi_types(&parsed.outputs)?)
        };

        Ok(Self {
            name: parsed.name,
            inputs: parsed.inputs,
            outputs: parsed.outputs,
            selector,
            input_cache,
            output_cache,
        })
    }

    /// Convert types back to comma-separated string.
    fn types_to_string(types: &[AbiType]) -> String {
        use std::fmt::Write;

        let mut result = String::new();
        for (i, ty) in types.iter().enumerate() {
            if i > 0 {
                result.push(',');
            }
            #[allow(clippy::expect_used)]
            write!(&mut result, "{ty}").expect("writing to String is infallible");
        }
        result
    }

    /// Calculate function selector (4-byte keccak256 hash).
    fn calculate_selector(input: &str) -> [u8; 4] {
        use alloy::primitives::keccak256;
        let hash = keccak256(input.as_bytes());
        let mut selector = [0u8; 4];
        selector.copy_from_slice(&hash[..4]);
        selector
    }

    /// Get the pre-built input cache for encoding.
    /// Returns `None` when inputs are empty.
    #[must_use]
    pub const fn input_cache(&self) -> Option<&Arc<CachedAbiTypes>> {
        self.input_cache.as_ref()
    }

    /// Get the pre-built output cache for decoding.
    /// Returns `None` when outputs are empty.
    #[must_use]
    pub const fn output_cache(&self) -> Option<&Arc<CachedAbiTypes>> {
        self.output_cache.as_ref()
    }
}

/// Encode arguments for an ABI function call.
///
/// Uses `encode_for_types` to avoid string parsing overhead when we already
/// have `AbiType` instances.
///
/// # Errors
///
/// Returns `ContractError` if argument count mismatch or encoding fails.
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

    // Convert string arguments to AbiValues
    let values: Vec<AbiValue> = types
        .iter()
        .zip(args.iter())
        .map(|(ty, arg)| AbiValue::from_str_arg(ty, arg))
        .collect::<Result<Vec<_>, _>>()?;

    // Use encode_for_types to avoid string parsing
    let encoded = crate::abi_encoder::encode_for_types(types, &values)?;

    Ok(Bytes::from(encoded))
}

/// Encode arguments using a pre-built `CachedAbiTypes` for maximum performance.
///
/// Use this in the hot path (e.g., `Contract::call`) where the type cache is
/// already resolved from the `FunctionSignature`.
///
/// # Errors
///
/// Returns `ContractError` if argument count mismatch or encoding fails.
pub fn encode_arguments_cached(
    cached: &Arc<CachedAbiTypes>,
    types: &[AbiType],
    args: &[String],
) -> ContractResult<Bytes> {
    if types.len() != args.len() {
        return Err(ContractError::InvalidAbi {
            message: format!(
                "Argument count mismatch: expected {}, got {}",
                types.len(),
                args.len()
            ),
        });
    }

    // Convert string arguments to AbiValues
    let values: Vec<AbiValue> = types
        .iter()
        .zip(args.iter())
        .map(|(ty, arg)| AbiValue::from_str_arg(ty, arg))
        .collect::<Result<Vec<_>, _>>()?;

    let encoded = cached.encode(&values)?;

    Ok(Bytes::from(encoded))
}

/// Decode return data based on expected ABI types.
///
/// Uses `decode_for_types` to avoid string parsing overhead when we already
/// have `AbiType` instances.
///
/// # Errors
///
/// Returns `ContractError` if decoding fails or if data is empty when return
/// types are expected.
pub fn decode_return_data(data: &[u8], types: &[AbiType]) -> ContractResult<Vec<String>> {
    if types.is_empty() {
        return Ok(Vec::new());
    }

    if data.is_empty() {
        return Err(ContractError::DecodingError {
            message: "Expected return data but got empty response".to_string(),
        });
    }

    // Use decode_for_types to avoid string parsing
    let decoded = crate::abi_decoder::decode_for_types(types, data)?;

    // Convert AbiValues back to strings
    Ok(decoded.iter().map(AbiValue::to_contract_string).collect())
}

/// Decode return data using a pre-built `CachedAbiTypes` for maximum performance.
///
/// Use this in the hot path (e.g., `Contract::call`) where the type cache is
/// already resolved from the `FunctionSignature`.
///
/// # Errors
///
/// Returns `ContractError` if decoding fails or if data is empty when return
/// types are expected.
pub fn decode_return_data_cached(
    cached: &Arc<CachedAbiTypes>,
    data: &[u8],
    types: &[AbiType],
) -> ContractResult<Vec<String>> {
    if types.is_empty() {
        return Ok(Vec::new());
    }

    if data.is_empty() {
        return Err(ContractError::DecodingError {
            message: "Expected return data but got empty response".to_string(),
        });
    }

    let decoded = cached.decode(data)?;

    // Convert AbiValues back to strings
    Ok(decoded.iter().map(AbiValue::to_contract_string).collect())
}

/// Decode return data into typed `AbiValue` results.
///
/// Returns the raw `AbiValue` enums so callers can match on specific variants
/// without re-parsing strings back into typed values.
///
/// # Errors
///
/// Returns `ContractError` if decoding fails or if data is empty when return
/// types are expected.
pub fn decode_return_data_typed(types: &[AbiType], data: &[u8]) -> ContractResult<Vec<AbiValue>> {
    if types.is_empty() {
        return Ok(Vec::new());
    }

    if data.is_empty() {
        return Err(ContractError::DecodingError {
            message: "Expected return data but got empty response".to_string(),
        });
    }

    crate::abi_decoder::decode_for_types(types, data).map_err(Into::into)
}

/// Contract interface for calling contract functions.
///
/// Cloned instances share the same signature cache, so parsing a signature
/// in one clone makes it available in all others.
#[derive(Clone)]
pub struct Contract {
    address: Address,
    provider: Arc<AlloyProvider>,
    /// Cache of parsed function signatures by signature string.
    /// Uses `DashMap` for lock-free concurrent reads.
    signature_cache: Arc<DashMap<String, Arc<FunctionSignature>>>,
}

impl std::fmt::Debug for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contract")
            .field("address", &self.address)
            .field("signature_cache_len", &self.signature_cache.len())
            .finish_non_exhaustive()
    }
}

impl Contract {
    /// Create a new contract instance.
    ///
    /// # Errors
    ///
    /// Returns `AddressError` if the address is invalid.
    pub fn new(
        address: &str,
        provider: Arc<AlloyProvider>,
    ) -> Result<Self, crate::errors::AddressError> {
        let addr = crate::address_utils::parse_address(address)?;

        Ok(Self {
            address: addr,
            provider,
            signature_cache: Arc::new(DashMap::new()),
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

        // Encode arguments (use cached types when available)
        let encoded_args = if let Some(cached) = func.input_cache() {
            encode_arguments_cached(cached, &func.inputs, args)?
        } else {
            encode_arguments(&func.inputs, args)?
        };

        // Build calldata: selector + encoded_args
        let mut calldata = Vec::with_capacity(4 + encoded_args.len());
        calldata.extend_from_slice(&func.selector);
        calldata.extend_from_slice(&encoded_args);

        // Execute eth_call
        let result = self
            .provider
            .eth_call(&self.address, Bytes::from(calldata), block_number)
            .await?;

        // Decode return values (use cached types when available)
        func.output_cache().map_or_else(
            || decode_return_data(&result, &func.outputs).map_err(Into::into),
            |cached| decode_return_data_cached(cached, &result, &func.outputs).map_err(Into::into),
        )
    }

    /// Call a contract function and return typed `AbiValue` results.
    ///
    /// This avoids the string round-trip of [`call`](Self::call), which
    /// is useful when the caller needs to inspect specific value variants
    /// (e.g., extracting `U256` from `AbiValue::Uint` without re-parsing).
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` or `ContractError` if the call fails or encoding/decoding fails.
    pub async fn call_typed(
        &self,
        function_signature: &str,
        args: &[String],
        block_number: Option<u64>,
    ) -> ProviderResult<Vec<AbiValue>> {
        // Parse function signature
        let func = self.parse_function_signature(function_signature)?;

        // Encode arguments (use cached types when available)
        let encoded_args = if let Some(cached) = func.input_cache() {
            encode_arguments_cached(cached, &func.inputs, args)?
        } else {
            encode_arguments(&func.inputs, args)?
        };

        // Build calldata: selector + encoded_args
        let mut calldata = Vec::with_capacity(4 + encoded_args.len());
        calldata.extend_from_slice(&func.selector);
        calldata.extend_from_slice(&encoded_args);

        // Execute eth_call
        let result = self
            .provider
            .eth_call(&self.address, Bytes::from(calldata), block_number)
            .await?;

        // Decode into typed values
        #[allow(clippy::option_if_let_else)]
        func.output_cache().map_or_else(
            || decode_return_data_typed(&func.outputs, &result).map_err(Into::into),
            |cached| {
                if func.outputs.is_empty() {
                    return Ok(Vec::new());
                }
                if result.is_empty() {
                    return Err(ContractError::DecodingError {
                        message: "Expected return data but got empty response".to_string(),
                    }
                    .into());
                }
                cached
                    .decode(&result)
                    .map_err(|e| ContractError::DecodingError {
                        message: e.to_string(),
                    })
                    .map_err(Into::into)
            },
        )
    }

    /// Parse and cache a function signature.
    ///
    /// Uses `DashMap` so there is no read/write lock toggling. A concurrent
    /// double-miss on the same key is harmless: both threads insert the same
    /// value and the second insert simply overwrites the first.
    fn parse_function_signature(&self, signature: &str) -> ProviderResult<Arc<FunctionSignature>> {
        // Fast path: check cache
        if let Some(func) = self.signature_cache.get(signature) {
            return Ok(Arc::clone(func.value()));
        }

        let func = Arc::new(FunctionSignature::parse(signature)?);

        // Insert (harmless if another thread already inserted)
        self.signature_cache
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
    use alloy::primitives::I256;
    use crate::runtime::get_runtime;

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
    fn test_function_signature_caches_are_built() {
        // Signature with inputs: should have input_cache
        let sig = FunctionSignature::parse("transfer(address,uint256)").unwrap();
        assert!(sig.input_cache.is_some(), "input_cache should be built");
        assert!(sig.output_cache.is_none(), "output_cache should be None for no outputs");

        // Signature with outputs: should have output_cache
        let sig = FunctionSignature::parse("balanceOf(address) returns (uint256)").unwrap();
        assert!(sig.input_cache.is_some());
        assert!(sig.output_cache.is_some());

        // Empty inputs: no input_cache
        let sig = FunctionSignature::parse("foo() returns (uint256)").unwrap();
        assert!(sig.input_cache.is_none(), "input_cache should be None for empty inputs");
        assert!(sig.output_cache.is_some());
    }

    #[test]
    fn test_encode_address() {
        let addr = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let encoded =
            AbiValue::from_str_arg(&AbiType::Address, addr).expect("address should parse");
        match encoded {
            AbiValue::Address(bytes) => {
                assert_eq!(bytes.len(), 20);
            }
            _ => panic!("Expected Address variant"),
        }
    }

    #[test]
    fn test_encode_bool() {
        let encoded =
            AbiValue::from_str_arg(&AbiType::Bool, "true").expect("bool true should parse");
        match encoded {
            AbiValue::Bool(true) => {}
            _ => panic!("Expected Bool(true)"),
        }

        let encoded =
            AbiValue::from_str_arg(&AbiType::Bool, "false").expect("bool false should parse");
        match encoded {
            AbiValue::Bool(false) => {}
            _ => panic!("Expected Bool(false)"),
        }
    }

    #[test]
    fn test_encode_uint() {
        let encoded =
            AbiValue::from_str_arg(&AbiType::Uint(256), "12345").expect("uint256 should parse");
        match encoded {
            AbiValue::Uint(n, bits) => {
                assert_eq!(n.to_string(), "12345");
                assert_eq!(bits, 256);
            }
            _ => panic!("Expected Uint variant"),
        }
    }

    #[test]
    fn test_function_selector() {
        // transfer(address,uint256) selector
        let sig = FunctionSignature::parse("transfer(address,uint256)")
            .expect("transfer signature should parse");
        // Expected selector: first 4 bytes of keccak256("transfer(address,uint256)")
        // Should be 0xa9059cbb
        assert_eq!(alloy::hex::encode(sig.selector), "a9059cbb");

        // balanceOf(address) selector
        let sig = FunctionSignature::parse("balanceOf(address)")
            .expect("balanceOf signature should parse");
        // Expected: 0x70a08231
        assert_eq!(alloy::hex::encode(sig.selector), "70a08231");
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
        let encoded_hex = AbiValue::from_str_arg(&AbiType::Uint(256), "0x3039").unwrap();
        let encoded_dec = AbiValue::from_str_arg(&AbiType::Uint(256), "12345").unwrap();
        assert_eq!(encoded_hex, encoded_dec);

        // Uppercase 0X prefix
        let encoded_upper = AbiValue::from_str_arg(&AbiType::Uint(256), "0X3039").unwrap();
        assert_eq!(encoded_upper, encoded_dec);
    }

    #[test]
    fn test_encode_int_hex() {
        // Hex-encoded int should produce the same result as decimal
        let encoded_hex = AbiValue::from_str_arg(&AbiType::Int(256), "0x3039").unwrap();
        let encoded_dec = AbiValue::from_str_arg(&AbiType::Int(256), "12345").unwrap();
        assert_eq!(encoded_hex, encoded_dec);

        // Uppercase 0X prefix
        let encoded_upper = AbiValue::from_str_arg(&AbiType::Int(256), "0X3039").unwrap();
        assert_eq!(encoded_upper, encoded_dec);

        // Negative decimal should still work
        let encoded_neg = AbiValue::from_str_arg(&AbiType::Int(256), "-1").unwrap();
        match encoded_neg {
            AbiValue::Int(n, bits) => {
                assert_eq!(n, I256::MINUS_ONE);
                assert_eq!(bits, 256);
            }
            _ => panic!("Expected Int variant"),
        }
    }

    #[test]
    fn test_encode_uint_invalid_hex() {
        // Invalid hex should produce a clear error
        let result = AbiValue::from_str_arg(&AbiType::Uint(256), "0xZZZZ");
        assert!(result.is_err());
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
    // Array encoding tests (via abi_encoder)
    // =========================================================================

    #[test]
    fn test_encode_empty_dynamic_array() {
        let encoded = AbiValue::from_str_arg(&AbiType::Array(Box::new(AbiType::Uint(256))), "[]")
            .expect("should encode empty array");
        match encoded {
            AbiValue::Array(arr) => assert!(arr.is_empty()),
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_encode_dynamic_uint_array() {
        let value =
            AbiValue::from_str_arg(&AbiType::Array(Box::new(AbiType::Uint(256))), "[1, 2, 3]")
                .expect("should encode uint256[]");
        match value {
            AbiValue::Array(arr) => {
                assert_eq!(arr.len(), 3);
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_encode_fixed_uint_array() {
        // [10, 20, 30] as uint256[3]
        let value = AbiValue::from_str_arg(
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            "[10, 20, 30]",
        )
        .expect("should encode uint256[3]");
        match value {
            AbiValue::Array(arr) => {
                assert_eq!(arr.len(), 3);
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_encode_fixed_array_wrong_size() {
        // uint256[3] with 2 elements should error
        let result = AbiValue::from_str_arg(
            &AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            "[1, 2]",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_dynamic_address_array() {
        let addr1 = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657";
        let input = format!("[{addr1}, {addr2}]");

        let value = AbiValue::from_str_arg(&AbiType::Array(Box::new(AbiType::Address)), &input)
            .expect("should encode address[]");
        match value {
            AbiValue::Array(arr) => {
                assert_eq!(arr.len(), 2);
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_encode_dynamic_bool_array() {
        let value = AbiValue::from_str_arg(
            &AbiType::Array(Box::new(AbiType::Bool)),
            "[true, false, true]",
        )
        .expect("should encode bool[]");
        match value {
            AbiValue::Array(arr) => {
                assert_eq!(arr.len(), 3);
                match &arr[0] {
                    AbiValue::Bool(true) => {}
                    _ => panic!("Expected true"),
                }
                match &arr[1] {
                    AbiValue::Bool(false) => {}
                    _ => panic!("Expected false"),
                }
                match &arr[2] {
                    AbiValue::Bool(true) => {}
                    _ => panic!("Expected true"),
                }
            }
            _ => panic!("Expected Array variant"),
        }
    }

    // =========================================================================
    // encode_arguments: integration with abi_encoder
    // =========================================================================

    #[test]
    fn test_encode_arguments_static_then_dynamic() {
        // f(uint256, bytes) with (1, 0x1234)
        let types = vec![AbiType::Uint(256), AbiType::Bytes];
        let args = vec!["1".to_string(), "0x1234".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode mixed args");

        // Verify we got some bytes back (actual encoding verified in abi_encoder tests)
        assert!(!encoded.is_empty());
        assert!(encoded.len() >= 128); // At least head + tail
    }

    #[test]
    fn test_encode_arguments_two_dynamic_params() {
        // f(bytes, bytes) with (0x1234, 0x5678)
        let types = vec![AbiType::Bytes, AbiType::Bytes];
        let args = vec!["0x1234".to_string(), "0x5678".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        assert!(!encoded.is_empty());
        assert!(encoded.len() >= 192); // Two dynamic params
    }

    #[test]
    fn test_encode_arguments_static_dynamic_static_dynamic() {
        // f(uint256, string, bool, bytes) with (42, "hello", true, 0xabcd)
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
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_arguments_static_fixed_array_and_dynamic() {
        // f(uint256[3], bytes) with ([10, 20, 30], 0xff)
        let types = vec![
            AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3),
            AbiType::Bytes,
        ];
        let args = vec!["[10, 20, 30]".to_string(), "0xff".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_arguments_dynamic_array_with_static_elements() {
        // f(uint256[], uint256) with ([1,2,3], 42)
        let types = vec![
            AbiType::Array(Box::new(AbiType::Uint(256))),
            AbiType::Uint(256),
        ];
        let args = vec!["[1, 2, 3]".to_string(), "42".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");
        assert!(!encoded.is_empty());
    }

    // =========================================================================
    // Array decoding tests (via abi_decoder)
    // =========================================================================

    #[test]
    fn test_decode_empty_dynamic_array() {
        // Create encoded data: offset (32) + length (0)
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&alloy::primitives::U256::from(32).to_be_bytes_vec());
        data[32..64].copy_from_slice(&alloy::primitives::U256::from(0).to_be_bytes_vec());

        let values = decode_return_data(&data, &[AbiType::Array(Box::new(AbiType::Uint(256)))])
            .expect("should decode empty array");

        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "[]");
    }

    #[test]
    fn test_decode_dynamic_uint_array() {
        // Use abi_encoder to create test data, then decode it
        let types = vec![AbiType::Array(Box::new(AbiType::Uint(256)))];
        let args = vec!["[1, 2, 3]".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");

        // Decode should give back the original values (exact match)
        let decoded = decode_return_data(&encoded, &types).expect("should decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], "[1, 2, 3]");
    }

    #[test]
    fn test_decode_fixed_uint_array() {
        // Encode [10, 20, 30] as uint256[3]
        let types = vec![AbiType::FixedArray(Box::new(AbiType::Uint(256)), 3)];
        let args = vec!["[10, 20, 30]".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");
        let decoded = decode_return_data(&encoded, &types).expect("should decode");

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], "[10, 20, 30]");
    }

    #[test]
    fn test_decode_address_array() {
        let addr1 = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75";
        let types = vec![AbiType::Array(Box::new(AbiType::Address))];
        let args = vec![format!("[{addr1}]")];

        let encoded = encode_arguments(&types, &args).expect("should encode");
        let decoded = decode_return_data(&encoded, &types).expect("should decode");

        assert_eq!(decoded.len(), 1);
        // Use exact match instead of case-insensitive contains
        assert_eq!(decoded[0], "[0x742d35cc6634c0532925a3b8d4c9db96590d6b75]");
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
            let types = vec![abi_type.clone()];
            let args = vec![input.to_string()];

            let encoded = encode_arguments(&types, &args).expect("should encode");
            let decoded = decode_return_data(&encoded, &types).expect("should decode");

            assert!(
                decoded[0].starts_with('[') && decoded[0].ends_with(']'),
                "Decoded value should be an array: {}",
                decoded[0]
            );
        }
    }

    // =========================================================================
    // Invalid length validation tests
    // =========================================================================

    #[test]
    fn test_decode_dynamic_array_with_length_exceeding_data() {
        // Craft malformed ABI data where length claims more elements than data can contain
        let mut data = vec![0u8; 96];

        // Offset to array data (right after this offset field)
        data[0..32].copy_from_slice(&alloy::primitives::U256::from(32).to_be_bytes_vec());

        // Malicious length: claims 1 million elements
        data[32..64].copy_from_slice(&alloy::primitives::U256::from(1_000_000).to_be_bytes_vec());

        // Only 1 element of actual data
        data[64..96].copy_from_slice(&alloy::primitives::U256::from(42).to_be_bytes_vec());

        // Decoding should fail with a clear error
        let result = decode_return_data(&data, &[AbiType::Array(Box::new(AbiType::Uint(256)))])
            .map_err(|e| e.to_string());

        assert!(
            result.is_err(),
            "Should reject array with length exceeding available data"
        );
    }

    // =========================================================================
    // decode_return_data: empty data with expected types
    // =========================================================================

    #[test]
    fn test_decode_return_data_empty_with_types_errors() {
        // When return types are expected but data is empty, this is an error
        let result = decode_return_data(&[], &[AbiType::Uint(256)]);
        assert!(
            result.is_err(),
            "Should return error when data is empty but types are expected"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty response"),
            "Error message should mention empty response: {err_msg}"
        );
    }

    #[test]
    fn test_decode_return_data_empty_types_returns_empty() {
        // When no types are expected, empty data is fine
        let result = decode_return_data(&[], &[]);
        assert_eq!(result.unwrap(), Vec::<String>::new());
    }

    // =========================================================================
    // Decode-typed (returns AbiValue instead of String)
    // =========================================================================

    #[test]
    fn test_decode_return_data_typed() {
        let types = vec![AbiType::Uint(256), AbiType::Bool];
        let args = vec!["42".to_string(), "true".to_string()];

        let encoded = encode_arguments(&types, &args).expect("should encode");
        let decoded = decode_return_data_typed(&types, &encoded).expect("should decode typed");

        assert_eq!(decoded.len(), 2);
        match &decoded[0] {
            AbiValue::Uint(n, bits) => {
                assert_eq!(*n, alloy::primitives::U256::from(42u64));
                assert_eq!(*bits, 256);
            }
            _ => panic!("Expected Uint variant"),
        }
        match &decoded[1] {
            AbiValue::Bool(true) => {}
            _ => panic!("Expected Bool(true)"),
        }
    }

    // =========================================================================
    // encode_arguments_cached / decode_return_data_cached
    // =========================================================================

    #[test]
    fn test_encode_arguments_cached_matches_uncached() {
        let types = vec![AbiType::Uint(256), AbiType::Bool];
        let args = vec!["42".to_string(), "true".to_string()];

        let uncached = encode_arguments(&types, &args).expect("uncached should encode");

        let cached_types = CachedAbiTypes::from_abi_types(&types).expect("should build cache");
        let with_cache =
            encode_arguments_cached(&cached_types, &types, &args).expect("cached should encode");

        assert_eq!(uncached, with_cache, "Cached and uncached encoding must match");
    }

    #[test]
    fn test_decode_return_data_cached_matches_uncached() {
        let types = vec![AbiType::Uint(256), AbiType::Bool];
        let args = vec!["42".to_string(), "true".to_string()];
        let encoded = encode_arguments(&types, &args).expect("should encode");

        let uncached = decode_return_data(&encoded, &types).expect("uncached should decode");

        let cached_types = CachedAbiTypes::from_abi_types(&types).expect("should build cache");
        let with_cache = decode_return_data_cached(&cached_types, &encoded, &types)
            .expect("cached should decode");

        assert_eq!(uncached, with_cache, "Cached and uncached decoding must match");
    }

    // =========================================================================
    // Contract struct: Debug and Clone
    // =========================================================================

    #[test]
    fn test_contract_debug_impl() {
        let provider = Arc::new(
            get_runtime()
                .block_on(async {
                    AlloyProvider::new("http://localhost:8545", 1).await
                })
                .expect("should create provider"),
        );
        let contract =
            Contract::new("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", provider)
                .expect("should create contract");

        let debug_str = format!("{contract:?}");
        assert!(
            debug_str.contains("Contract"),
            "Debug output should contain Contract: {debug_str}"
        );
    }

    #[test]
    fn test_contract_clone_shares_cache() {
        let provider = Arc::new(
            get_runtime()
                .block_on(async {
                    AlloyProvider::new("http://localhost:8545", 1).await
                })
                .expect("should create provider"),
        );
        let contract =
            Contract::new("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", provider)
                .expect("should create contract");

        let cloned = contract.clone();

        // Parse a signature on the original
        let func = contract
            .parse_function_signature("transfer(address,uint256)")
            .expect("should parse");

        // The cloned contract should also have the signature in its cache
        // (shared Arc<DashMap>)
        let cached_func = cloned
            .parse_function_signature("transfer(address,uint256)")
            .expect("should find in shared cache");

        assert_eq!(func.selector, cached_func.selector);
    }
}
