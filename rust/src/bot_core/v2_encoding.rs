//! V2 swap encoding — produces pre-encoded calldata for Uniswap V2 `swap()` calls.
//!
//! The V2 swap function signature is `swap(uint256,uint256,address,bytes)`.
//! The selector is the first 4 bytes of `keccak256("swap(uint256,uint256,address,bytes)")`.
//!
//! For a `zero_for_one` swap (token0 → token1):
//! - `amount0Out = 0`, `amount1Out = amount_out`
//!
//! For a `one_for_zero` swap (token1 → token0):
//! - `amount0Out = amount_out`, `amount1Out = 0`

use alloy::primitives::{Address, U256};

use crate::abi_encoder::encode_rust;
use crate::abi_types::AbiValue;
use crate::errors::AbiDecodeError;

/// The V2 swap function selector: keccak256("swap(uint256,uint256,address,bytes)")[:4]
pub const V2_SWAP_SELECTOR: [u8; 4] = [0x02, 0x2c, 0x0d, 0x9f];

/// A single EVM call ready for on-chain submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedCall {
    /// Target contract address.
    pub to: Address,
    /// ABI-encoded calldata (selector + parameters).
    pub data: Vec<u8>,
    /// ETH value to send with the call.
    pub value: U256,
}

/// Encode a Uniswap V2 `swap(uint256,uint256,address,bytes)` call.
///
/// # Arguments
///
/// * `pool_address` - The V2 pool contract address
/// * `zero_for_one` - If true, token0→token1 (`amount1_out` nonzero); if false, token1→token0 (`amount0_out` nonzero)
/// * `amount_out` - The output token amount
/// * `recipient` - The address to receive the output tokens
///
/// # Errors
///
/// Returns `AbiDecodeError` if ABI encoding fails (should not happen with valid inputs).
pub fn encode_v2_swap(
    pool_address: Address,
    zero_for_one: bool,
    amount_out: U256,
    recipient: Address,
) -> Result<EncodedCall, AbiDecodeError> {
    let (a0_out, a1_out) = if zero_for_one {
        (U256::ZERO, amount_out)
    } else {
        (amount_out, U256::ZERO)
    };

    let recipient_bytes: [u8; 20] = recipient.into();

    let values = [
        AbiValue::Uint(a0_out, 256),
        AbiValue::Uint(a1_out, 256),
        AbiValue::Address(recipient_bytes),
        AbiValue::Bytes(Vec::new()),
    ];

    let types = ["uint256", "uint256", "address", "bytes"];

    let encoded = encode_rust(&types, &values)?;

    let mut data = Vec::with_capacity(4 + encoded.len());
    data.extend_from_slice(&V2_SWAP_SELECTOR);
    data.extend_from_slice(&encoded);

    Ok(EncodedCall {
        to: pool_address,
        data,
        value: U256::ZERO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_v2_swap_zero_for_one() {
        // Python reference calldata for swap(0, 181, 0xbbbb..., b"")
        let pool = Address::from([0xaa; 20]);
        let recipient = Address::from([0xbb; 20]);

        let call = encode_v2_swap(pool, true, U256::from(181), recipient).unwrap();

        assert_eq!(call.to, pool);
        assert_eq!(call.value, U256::ZERO);
        // Selector is first 4 bytes
        assert_eq!(&call.data[..4], &[0x02, 0x2c, 0x0d, 0x9f]);
        // Total length: 4 (selector) + 160 (4 x 32-byte params) = 164
        assert_eq!(call.data.len(), 164);
    }

    #[test]
    fn encode_v2_swap_matches_python_reference() {
        // Full byte-level comparison against Python eth_abi output
        let pool = Address::from([0xaa; 20]);
        let recipient = Address::from([0xbb; 20]);

        let call = encode_v2_swap(pool, true, U256::from(181), recipient).unwrap();

        let expected_hex = "022c0d9f\
            0000000000000000000000000000000000000000000000000000000000000000\
            00000000000000000000000000000000000000000000000000000000000000b5\
            000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
            0000000000000000000000000000000000000000000000000000000000000080\
            0000000000000000000000000000000000000000000000000000000000000000";
        let expected = hex_decode(expected_hex);
        assert_eq!(call.data, expected);
    }

    #[test]
    fn encode_v2_swap_one_for_zero() {
        // Reverse direction: amount0_out is nonzero, amount1_out is 0
        let pool = Address::from([0xaa; 20]);
        let recipient = Address::from([0xbb; 20]);

        let call = encode_v2_swap(pool, false, U256::from(181), recipient).unwrap();

        // amount0_out=181, amount1_out=0
        let expected_hex = "022c0d9f\
            00000000000000000000000000000000000000000000000000000000000000b5\
            0000000000000000000000000000000000000000000000000000000000000000\
            000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
            0000000000000000000000000000000000000000000000000000000000000080\
            0000000000000000000000000000000000000000000000000000000000000000";
        let expected = hex_decode(expected_hex);
        assert_eq!(call.data, expected);
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        let clean: String = s.chars().filter(|c| *c != '\\' && *c != '\n').collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
            .collect()
    }
}
