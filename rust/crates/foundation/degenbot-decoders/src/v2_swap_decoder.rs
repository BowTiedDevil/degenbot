//! Uniswap V2 Swap event decoder.
//!
//! Decodes `Swap(address,uint256,uint256,uint256,uint256,address)` events
//! from Uniswap V2 (+ V2-compatible: `SushiSwap`, etc.) pair contracts. The Swap
//! event is emitted on every V2 swap and carries the in/out amounts for both
//! tokens — the direct ground-truth the swap-event-capture inspector uses to
//! retire `diagnostic.rs::recompute_v2_amount_out` (no `getAmountOut`
//! recompute, no Multicall3 reserves re-fetch).
//!
//! # Swap event format
//!
//! ```text
//! event Swap(
//!     address indexed sender,
//!     uint256 amount0In,
//!     uint256 amount1In,
//!     uint256 amount0Out,
//!     uint256 amount1Out,
//!     address indexed to
//! )
//!
//! topic[0] = 0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822
//! topic[1] = sender (indexed)
//! topic[2] = to (indexed)
//! data    = abi.encode(uint256, uint256, uint256, uint256) = 128 bytes
//!           (4 × 32-byte words, left-padded)
//! ```

use alloy::primitives::{b256, Address, B256, U256};
use alloy::rpc::types::Log;

/// Keccak256 of `Swap(address,uint256,uint256,uint256,uint256,address)`.
pub const V2_SWAP_TOPIC: B256 =
    b256!("0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

/// Decoded V2 Swap event carrying the raw in/out amounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2SwapEvent {
    /// The pair contract that emitted the event.
    pub pool_address: Address,
    /// The sender (from topic[1]).
    pub sender: Address,
    /// The recipient (from topic[2]).
    pub to: Address,
    /// Amount of token0 paid IN to the pair (uint256).
    pub amount0_in: U256,
    /// Amount of token1 paid IN to the pair (uint256).
    pub amount1_in: U256,
    /// Amount of token0 paid OUT of the pair (uint256).
    pub amount0_out: U256,
    /// Amount of token1 paid OUT of the pair (uint256).
    pub amount1_out: U256,
}

/// Decode a V2 Swap event from a log.
///
/// Returns `Some(V2SwapEvent)` if the log is a valid Swap event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V2_SWAP_TOPIC`
/// - The data is too short (< 128 bytes)
/// - There are fewer than 3 topics (signature + sender + to)
#[must_use]
pub fn decode_v2_swap_log(log: &Log) -> Option<V2SwapEvent> {
    // topic[0] must match V2_SWAP_TOPIC
    let first_topic = log.topics().first()?;
    if *first_topic != V2_SWAP_TOPIC {
        return None;
    }

    // We need at least 3 topics (event sig + sender + to)
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }

    // Decode sender from topic[1] (indexed address)
    let sender = Address::from_word(topics[1]);
    // Decode `to` from topic[2] (indexed address)
    let to = Address::from_word(topics[2]);

    // data = abi.encode(uint256 amount0In, uint256 amount1In,
    //                   uint256 amount0Out, uint256 amount1Out)
    // = 4 × 32 bytes = 128 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }

    // Decode amount0In (uint256, bytes 0..32)
    let amount0_in = U256::from_be_bytes::<32>(data[..32].try_into().ok()?);

    // Decode amount1In (uint256, bytes 32..64)
    let amount1_in = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?);

    // Decode amount0Out (uint256, bytes 64..96)
    let amount0_out = U256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    // Decode amount1Out (uint256, bytes 96..128)
    let amount1_out = U256::from_be_bytes::<32>(data[96..128].try_into().ok()?);

    Some(V2SwapEvent {
        pool_address: log.address(),
        sender,
        to,
        amount0_in,
        amount1_in,
        amount0_out,
        amount1_out,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;

    fn make_v2_swap_log(
        pool_address: Address,
        sender: Address,
        to: Address,
        amount0_in: U256,
        amount1_in: U256,
        amount0_out: U256,
        amount1_out: U256,
    ) -> Log {
        let mut data = Vec::with_capacity(128);
        // amount0In (uint256)
        data.extend_from_slice(&amount0_in.to_be_bytes::<32>());
        // amount1In (uint256)
        data.extend_from_slice(&amount1_in.to_be_bytes::<32>());
        // amount0Out (uint256)
        data.extend_from_slice(&amount0_out.to_be_bytes::<32>());
        // amount1Out (uint256)
        data.extend_from_slice(&amount1_out.to_be_bytes::<32>());

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V2_SWAP_TOPIC, sender.into_word(), to.into_word()],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    #[test]
    fn decode_valid_v2_swap_exact_in_token0() {
        // A swap paying 1.0 token0 in, receiving ~3000 token1 out (token0 ≈ WETH,
        // token1 ≈ USDC). amount0In=1e18, amount1Out=3_000e6.
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let to = Address::from([0xcc; 20]);
        let amount0_in = U256::from(1_000_000_000_000_000_000_u64);
        let amount1_in = U256::ZERO;
        let amount0_out = U256::ZERO;
        let amount1_out = U256::from(3_000_000_000_u64);

        let log = make_v2_swap_log(
            pool,
            sender,
            to,
            amount0_in,
            amount1_in,
            amount0_out,
            amount1_out,
        );

        let result = decode_v2_swap_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.sender, sender);
        assert_eq!(event.to, to);
        assert_eq!(event.amount0_in, amount0_in);
        assert_eq!(event.amount1_in, amount1_in);
        assert_eq!(event.amount0_out, amount0_out);
        assert_eq!(event.amount1_out, amount1_out);
    }

    #[test]
    fn decode_valid_v2_swap_exact_in_token1() {
        // Reverse direction: paying token1 in, receiving token0 out.
        let pool = Address::from([0x11; 20]);
        let sender = Address::from([0x22; 20]);
        let to = Address::from([0x33; 20]);
        let amount0_in = U256::ZERO;
        let amount1_in = U256::from(3_000_000_000_u64);
        let amount0_out = U256::from(1_000_000_000_000_000_000_u64);
        let amount1_out = U256::ZERO;

        let log = make_v2_swap_log(
            pool,
            sender,
            to,
            amount0_in,
            amount1_in,
            amount0_out,
            amount1_out,
        );

        let event = decode_v2_swap_log(&log).expect("valid V2 swap");
        assert_eq!(event.amount0_in, U256::ZERO);
        assert_eq!(event.amount1_in, amount1_in);
        assert_eq!(event.amount0_out, amount0_out);
        assert_eq!(event.amount1_out, U256::ZERO);
    }

    #[test]
    fn decode_v2_swap_wrong_topic_returns_none() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let to = Address::ZERO;
        let data = vec![0u8; 128];

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![B256::ZERO, sender.into_word(), to.into_word()],
            Bytes::from(data),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_v2_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v2_swap_truncated_data_returns_none() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let to = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V2_SWAP_TOPIC, sender.into_word(), to.into_word()],
            Bytes::from(vec![0u8; 64]), // too short (need 128)
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_v2_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v2_swap_no_topics_returns_none() {
        let pool = Address::ZERO;
        let inner =
            alloy::primitives::Log::new_unchecked(pool, vec![], Bytes::from(vec![0u8; 128]));
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_v2_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v2_swap_two_topics_returns_none() {
        // signature + sender, but missing `to` → not a full V2 swap log.
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V2_SWAP_TOPIC, sender.into_word()],
            Bytes::from(vec![0u8; 128]),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_v2_swap_log(&log).is_none());
    }

    #[test]
    fn v2_swap_topic_is_known_constant() {
        // Sanity: the topic0 matches the well-known Uniswap V2 Swap signature
        // hash (verified against `cast keccak "Swap(address,uint256,uint256,uint256,uint256,address)"`).
        assert_eq!(
            V2_SWAP_TOPIC,
            alloy::primitives::b256!(
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
            )
        );
    }
}
