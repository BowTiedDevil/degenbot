//! Uniswap V4 `ModifyLiquidity` event decoder.
//!
//! Decodes `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)`
//! events from the Uniswap V4 `PoolManager` contract.
//!
//! # V4 `ModifyLiquidity` event format
//!
//! ```text
//! event ModifyLiquidity(
//!     V4PoolId indexed id,       // bytes32
//!     address indexed sender,
//!     int24 tickLower,
//!     int24 tickUpper,
//!     int256 liquidityDelta,
//!     bytes32 salt
//! )
//!
//! topic[0] = 0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec
//! topic[1] = V4PoolId (indexed bytes32)
//! topic[2] = sender (indexed address)
//! data    = abi.encode(int24 tickLower, int24 tickUpper, int256 liquidityDelta, bytes32 salt)
//!         = 4 × 32 bytes = 128 bytes
//! ```
//!
//! The V4 `ModifyLiquidity` event is the V4 equivalent of V3's `Mint` and
//! `Burn` events combined. A positive `liquidityDelta` is a liquidity
//! addition (V3 Mint), a negative `liquidityDelta` is a removal (V3 Burn).
//!
//! Key differences from V3 Mint/Burn:
//! - `tickLower` and `tickUpper` are in the data (not indexed)
//! - `liquidityDelta` is `int256` (signed), not `uint128`
//! - V4 uses `V4PoolId` (bytes32) instead of pool contract address
//! - The event is emitted by `PoolManager`, not by individual pool contracts

use alloy::primitives::{b256, Address, B256, I256};
use alloy::rpc::types::Log;

use crate::uniswap_tick_range::extract_int24_from_word;
use crate::v4_swap_decoder::V4PoolId;

/// Keccak256 of `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)`.
pub const V4_MODIFY_LIQUIDITY_TOPIC: B256 =
    b256!("0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec");

/// Decoded V4 `ModifyLiquidity` event carrying liquidity change data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4ModifyLiquidityEvent {
    /// The pool ID (bytes32 from topic[1]).
    pub pool_id: V4PoolId,
    /// The sender (from topic[2]).
    pub sender: Address,
    /// The lower tick of the position (from data).
    pub tick_lower: i32,
    /// The upper tick of the position (from data).
    pub tick_upper: i32,
    /// The liquidity delta (signed — positive for additions, negative for removals).
    pub liquidity_delta: I256,
    /// The salt parameter (from data; used for flash-loan-style positions).
    pub salt: B256,
}

/// Decode a V4 `ModifyLiquidity` event from a log.
///
/// Returns `Some(V4ModifyLiquidityEvent)` if the log is a valid
/// `ModifyLiquidity` event with correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V4_MODIFY_LIQUIDITY_TOPIC`
/// - There are fewer than 3 topics
/// - The data is too short (< 128 bytes)
/// - Tick values are out of the valid int24 range
#[must_use]
pub fn decode_v4_modify_liquidity_log(log: &Log) -> Option<V4ModifyLiquidityEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V4_MODIFY_LIQUIDITY_TOPIC {
        return None;
    }

    // Need 3 topics: event sig + V4PoolId + sender
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }

    // Decode V4PoolId from topic[1] (indexed bytes32)
    let pool_id: V4PoolId = topics[1].0;

    // Decode sender from topic[2] (indexed address)
    let sender = Address::from_word(topics[2]);

    // data = abi.encode(int24 tickLower, int24 tickUpper, int256 liquidityDelta, bytes32 salt)
    // = 4 × 32 bytes = 128 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }

    // Decode tickLower (int24, sign-extended to int256 in the first 32-byte
    // data word; range-checked against the Uniswap V3 tick bounds by the
    // shared helper).
    let tick_lower = extract_int24_from_word(&data[0..32])?;

    // Decode tickUpper (int24, sign-extended to int256 in the second 32-byte
    // data word).
    let tick_upper = extract_int24_from_word(&data[32..64])?;

    // Decode liquidityDelta (int256, bytes 64..96)
    let liquidity_delta = I256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    // Decode salt (bytes32, bytes 96..128)
    let salt = B256::from_slice(&data[96..128]);

    Some(V4ModifyLiquidityEvent {
        pool_id,
        sender,
        tick_lower,
        tick_upper,
        liquidity_delta,
        salt,
    })
}

#[cfg(test)]
#[expect(clippy::unreadable_literal, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;

    fn make_v4_modify_liquidity_log(
        pool_id: V4PoolId,
        sender: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
        salt: B256,
    ) -> Log {
        // data = abi.encode(int24 tickLower, int24 tickUpper, int256 liquidityDelta, bytes32 salt)
        let mut data = Vec::with_capacity(128);
        // tickLower (int24, sign-extended to int256 → 32 bytes)
        let tick_lower_i256 = I256::try_from(i128::from(tick_lower)).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_lower_i256.to_be_bytes::<32>());
        // tickUpper (int24, sign-extended to int256 → 32 bytes)
        let tick_upper_i256 = I256::try_from(i128::from(tick_upper)).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_upper_i256.to_be_bytes::<32>());
        // liquidityDelta (int256)
        data.extend_from_slice(&liquidity_delta.to_be_bytes::<32>());
        // salt (bytes32)
        data.extend_from_slice(salt.as_slice());

        let pool_id_topic = B256::from(pool_id);

        let inner = alloy::primitives::Log::new_unchecked(
            Address::from([0x00u8; 20]), // PoolManager address (placeholder)
            vec![V4_MODIFY_LIQUIDITY_TOPIC, pool_id_topic, sender.into_word()],
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
    fn decode_valid_v4_modify_liquidity_positive_delta() {
        let pool_id = [0xABu8; 32];
        let sender = Address::from([0xBBu8; 20]);
        let tick_lower = -887220i32;
        let tick_upper = 887220i32;
        let liquidity_delta = I256::try_from(1_000_000_i128).unwrap();
        let salt = B256::ZERO;

        let log = make_v4_modify_liquidity_log(
            pool_id,
            sender,
            tick_lower,
            tick_upper,
            liquidity_delta,
            salt,
        );

        let result = decode_v4_modify_liquidity_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_id, pool_id);
        assert_eq!(event.sender, sender);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.liquidity_delta, liquidity_delta);
        assert_eq!(event.salt, salt);
    }

    #[test]
    fn decode_valid_v4_modify_liquidity_negative_delta() {
        let pool_id = [0xCDu8; 32];
        let sender = Address::from([0xEEu8; 20]);
        let tick_lower = -60i32;
        let tick_upper = 60i32;
        // Burn: negative delta
        let liquidity_delta = I256::try_from(-500_000_i128).unwrap();
        let salt = B256::from([0xFFu8; 32]);

        let log = make_v4_modify_liquidity_log(
            pool_id,
            sender,
            tick_lower,
            tick_upper,
            liquidity_delta,
            salt,
        );

        let result = decode_v4_modify_liquidity_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_id, pool_id);
        assert_eq!(event.sender, sender);
        assert_eq!(event.tick_lower, -60);
        assert_eq!(event.tick_upper, 60);
        assert_eq!(event.liquidity_delta, liquidity_delta);
        assert_eq!(event.salt, B256::from([0xFFu8; 32]));
    }

    #[test]
    fn decode_v4_modify_liquidity_wrong_topic_returns_none() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![B256::ZERO, B256::from(pool_id), sender.into_word()],
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

        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v4_modify_liquidity_truncated_data_returns_none() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![
                V4_MODIFY_LIQUIDITY_TOPIC,
                B256::from(pool_id),
                sender.into_word(),
            ],
            Bytes::from(vec![0u8; 64]), // too short — need 128
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

        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v4_modify_liquidity_insufficient_topics_returns_none() {
        let pool_id = [0u8; 32];
        // sender not needed for insufficient-topics test

        // Only 2 topics — need 3 (sig + V4PoolId + sender)
        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![V4_MODIFY_LIQUIDITY_TOPIC, B256::from(pool_id)],
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

        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v4_modify_liquidity_tick_out_of_range_returns_none() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;
        let liquidity_delta = I256::try_from(100_i128).unwrap();

        // tick_lower = -887273 (out of range)
        let log =
            make_v4_modify_liquidity_log(pool_id, sender, -887273, 0, liquidity_delta, B256::ZERO);
        assert!(decode_v4_modify_liquidity_log(&log).is_none());

        // tick_upper = 887273 (out of range)
        let log =
            make_v4_modify_liquidity_log(pool_id, sender, 0, 887273, liquidity_delta, B256::ZERO);
        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v4_modify_liquidity_tick_at_boundary() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;
        let liquidity_delta = I256::try_from(100_i128).unwrap();

        // min tick: -887272
        let log_min =
            make_v4_modify_liquidity_log(pool_id, sender, -887272, 0, liquidity_delta, B256::ZERO);
        assert!(decode_v4_modify_liquidity_log(&log_min).is_some());

        // max tick: 887272
        let log_max =
            make_v4_modify_liquidity_log(pool_id, sender, 0, 887272, liquidity_delta, B256::ZERO);
        assert!(decode_v4_modify_liquidity_log(&log_max).is_some());
    }

    #[test]
    fn decode_v4_modify_liquidity_no_topics_returns_none() {
        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![],
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

        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn v4_swap_topic_not_matched_by_modify_liquidity_decoder() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![
                crate::v4_swap_decoder::V4_SWAP_TOPIC,
                B256::from(pool_id),
                sender.into_word(),
            ],
            Bytes::from(vec![0u8; 192]),
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

        assert!(decode_v4_modify_liquidity_log(&log).is_none());
    }

    #[test]
    fn modify_liquidity_topic_not_matched_by_swap_decoder() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;

        // Construct a log with MODIFY_LIQUIDITY_TOPIC but Swap-like data
        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![
                V4_MODIFY_LIQUIDITY_TOPIC,
                B256::from(pool_id),
                sender.into_word(),
            ],
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

        assert!(crate::v4_swap_decoder::decode_v4_swap_log(&log).is_none());
    }
}
