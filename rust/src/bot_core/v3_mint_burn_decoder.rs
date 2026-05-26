//! Uniswap V3 Mint/Burn event decoders.
//!
//! Decodes `Mint` and `Burn` events from Uniswap V3 pool contracts.
//! These events modify `tick_data` (`liquidity_gross`, `liquidity_net`) at
//! `tick_lower` and `tick_upper`, but do NOT change the pool's scalar
//! state (sqrtPriceX96, liquidity, tick) — those are only in Swap events.
//!
//! # Mint event format
//!
//! ```text
//! event Mint(
//!     address sender,
//!     address indexed owner,
//!     int24 indexed tickLower,
//!     int24 indexed tickUpper,
//!     uint128 amount,
//!     uint256 amount0,
//!     uint256 amount1
//! )
//!
//! topic[0] = 0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde
//! topic[1] = owner (indexed address)
//! topic[2] = tickLower (indexed int24)
//! topic[3] = tickUpper (indexed int24)
//! data    = abi.encode(address sender, uint128 amount, uint256 amount0, uint256 amount1)
//!         = 4 × 32 bytes = 128 bytes
//! ```
//!
//! # Burn event format
//!
//! ```text
//! event Burn(
//!     address indexed owner,
//!     int24 indexed tickLower,
//!     int24 indexed tickUpper,
//!     uint128 amount,
//!     uint256 amount0,
//!     uint256 amount1
//! )
//!
//! topic[0] = 0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c
//! topic[1] = owner (indexed address)
//! topic[2] = tickLower (indexed int24)
//! topic[3] = tickUpper (indexed int24)
//! data    = abi.encode(uint128 amount, uint256 amount0, uint256 amount1)
//!         = 3 × 32 bytes = 96 bytes
//! ```

use alloy::primitives::{Address, B256, U128, U256};
use alloy::rpc::types::Log;

/// Keccak256 of `Mint(address,address,int24,int24,uint128,uint256,uint256)`.
pub const V3_MINT_TOPIC: B256 = B256::new([
    0x7a, 0x53, 0x08, 0x0b, 0xa4, 0x14, 0x15, 0x8b,
    0xe7, 0xec, 0x69, 0xb9, 0x87, 0xb5, 0xfb, 0x7d,
    0x07, 0xde, 0xe1, 0x01, 0xfe, 0x85, 0x48, 0x8f,
    0x08, 0x53, 0xae, 0x16, 0x23, 0x9d, 0x0b, 0xde,
]);

/// Keccak256 of `Burn(address,int24,int24,uint128,uint256,uint256)`.
pub const V3_BURN_TOPIC: B256 = B256::new([
    0x0c, 0x39, 0x6c, 0xd9, 0x89, 0xa3, 0x9f, 0x44,
    0x59, 0xb5, 0xfa, 0x1a, 0xed, 0x6a, 0x9a, 0x8d,
    0xcd, 0xbc, 0x45, 0x90, 0x8a, 0xcf, 0xd6, 0x7e,
    0x02, 0x8c, 0xd5, 0x68, 0xda, 0x98, 0x98, 0x2c,
]);

/// Decoded V3 Mint event carrying liquidity position data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3MintEvent {
    /// The pool contract that emitted the event.
    pub pool_address: Address,
    /// The sender (from non-indexed data; the address that called mint).
    pub sender: Address,
    /// The owner (from topic[1]; the position owner).
    pub owner: Address,
    /// The lower tick of the position (from topic[2]).
    pub tick_lower: i32,
    /// The upper tick of the position (from topic[3]).
    pub tick_upper: i32,
    /// The amount of liquidity minted (uint128).
    pub amount: u128,
    /// Amount of token0 paid (uint256).
    pub amount0: U256,
    /// Amount of token1 paid (uint256).
    pub amount1: U256,
}

/// Decoded V3 Burn event carrying liquidity removal data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3BurnEvent {
    /// The pool contract that emitted the event.
    pub pool_address: Address,
    /// The owner (from topic[1]; the position owner).
    pub owner: Address,
    /// The lower tick of the position (from topic[2]).
    pub tick_lower: i32,
    /// The upper tick of the position (from topic[3]).
    pub tick_upper: i32,
    /// The amount of liquidity burned (uint128).
    pub amount: u128,
    /// Amount of token0 received (uint256).
    pub amount0: U256,
    /// Amount of token1 received (uint256).
    pub amount1: U256,
}

/// Decode a V3 Mint event from a log.
///
/// Returns `Some(V3MintEvent)` if the log is a valid Mint event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V3_MINT_TOPIC`
/// - There are fewer than 4 topics
/// - The data is too short (< 128 bytes)
/// - Tick values are out of the valid int24 range
#[must_use]
pub fn decode_v3_mint_log(log: &Log) -> Option<V3MintEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V3_MINT_TOPIC {
        return None;
    }

    // Need 4 topics: event sig + owner + tickLower + tickUpper
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }

    let owner = Address::from_word(topics[1]);
    let tick_lower = extract_tick_from_topic(&topics[2])?;
    let tick_upper = extract_tick_from_topic(&topics[3])?;

    // data = abi.encode(address sender, uint128 amount, uint256 amount0, uint256 amount1)
    // = 4 × 32 bytes = 128 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }

    // Decode sender (address, bytes 0..32)
    let sender = Address::from_slice(&data[12..32]);

    // Decode amount (uint128, bytes 32..64)
    let amount = U128::from_be_bytes::<16>(data[48..64].try_into().ok()?);

    // Decode amount0 (uint256, bytes 64..96)
    let amount0 = U256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    // Decode amount1 (uint256, bytes 96..128)
    let amount1 = U256::from_be_bytes::<32>(data[96..128].try_into().ok()?);

    Some(V3MintEvent {
        pool_address: log.address(),
        sender,
        owner,
        tick_lower,
        tick_upper,
        amount: amount.to::<u128>(),
        amount0,
        amount1,
    })
}

/// Decode a V3 Burn event from a log.
///
/// Returns `Some(V3BurnEvent)` if the log is a valid Burn event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V3_BURN_TOPIC`
/// - There are fewer than 4 topics
/// - The data is too short (< 96 bytes)
/// - Tick values are out of the valid int24 range
#[must_use]
pub fn decode_v3_burn_log(log: &Log) -> Option<V3BurnEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V3_BURN_TOPIC {
        return None;
    }

    // Need 4 topics: event sig + owner + tickLower + tickUpper
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }

    let owner = Address::from_word(topics[1]);
    let tick_lower = extract_tick_from_topic(&topics[2])?;
    let tick_upper = extract_tick_from_topic(&topics[3])?;

    // data = abi.encode(uint128 amount, uint256 amount0, uint256 amount1)
    // = 3 × 32 bytes = 96 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 96 {
        return None;
    }

    // Decode amount (uint128, bytes 0..32)
    let amount = U128::from_be_bytes::<16>(data[16..32].try_into().ok()?);

    // Decode amount0 (uint256, bytes 32..64)
    let amount0 = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?);

    // Decode amount1 (uint256, bytes 64..96)
    let amount1 = U256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    Some(V3BurnEvent {
        pool_address: log.address(),
        owner,
        tick_lower,
        tick_upper,
        amount: amount.to::<u128>(),
        amount0,
        amount1,
    })
}

/// Extract an int24 tick value from an indexed topic.
///
/// Indexed int24 values are stored as a full 32-byte word (sign-extended
/// to uint256 / int256 in the ABI). The actual value is in the last 4 bytes.
///
/// Returns `None` if the tick is outside the valid int24 range [-887272, 887272].
fn extract_tick_from_topic(topic: &B256) -> Option<i32> {
    // The int24 value is in the last 4 bytes of the 32-byte word
    let tick_bytes: [u8; 4] = topic[28..32].try_into().ok()?;
    let tick = i32::from_be_bytes(tick_bytes);
    if !(-887_272..=887_272).contains(&tick) {
        return None;
    }
    Some(tick)
}

#[cfg(test)]
#[allow(clippy::unreadable_literal, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, I256};

    #[allow(clippy::too_many_arguments)]
    fn make_v3_mint_log(
        pool_address: Address,
        sender: Address,
        owner: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
        amount0: U256,
        amount1: U256,
    ) -> Log {
        // data = abi.encode(address sender, uint128 amount, uint256 amount0, uint256 amount1)
        let mut data = Vec::with_capacity(128);
        // sender (address, left-padded to 32 bytes)
        let mut sender_word = [0u8; 32];
        sender_word[12..32].copy_from_slice(sender.as_slice());
        data.extend_from_slice(&sender_word);
        // amount (uint128, left-padded to 32 bytes)
        let amount_u128 = U128::from(amount);
        let amount_bytes = amount_u128.to_be_bytes::<16>();
        let mut amount_word = [0u8; 32];
        amount_word[16..32].copy_from_slice(&amount_bytes);
        data.extend_from_slice(&amount_word);
        // amount0 (uint256)
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        // amount1 (uint256)
        data.extend_from_slice(&amount1.to_be_bytes::<32>());

        let tick_lower_topic = tick_to_topic(tick_lower);
        let tick_upper_topic = tick_to_topic(tick_upper);

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V3_MINT_TOPIC, owner.into_word(), tick_lower_topic, tick_upper_topic],
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

    fn make_v3_burn_log(
        pool_address: Address,
        owner: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
        amount0: U256,
        amount1: U256,
    ) -> Log {
        // data = abi.encode(uint128 amount, uint256 amount0, uint256 amount1)
        let mut data = Vec::with_capacity(96);
        // amount (uint128, left-padded to 32 bytes)
        let amount_u128 = U128::from(amount);
        let amount_bytes = amount_u128.to_be_bytes::<16>();
        let mut amount_word = [0u8; 32];
        amount_word[16..32].copy_from_slice(&amount_bytes);
        data.extend_from_slice(&amount_word);
        // amount0 (uint256)
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        // amount1 (uint256)
        data.extend_from_slice(&amount1.to_be_bytes::<32>());

        let tick_lower_topic = tick_to_topic(tick_lower);
        let tick_upper_topic = tick_to_topic(tick_upper);

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V3_BURN_TOPIC, owner.into_word(), tick_lower_topic, tick_upper_topic],
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

    /// Encode an int24 tick value as a 32-byte topic (sign-extended to int256).
    fn tick_to_topic(tick: i32) -> B256 {
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        B256::from(tick_i256.to_be_bytes::<32>())
    }

    // ── Mint tests ──────────────────────────────────────────────

    #[test]
    fn decode_valid_v3_mint() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let owner = Address::from([0xcc; 20]);
        let tick_lower = -887220i32;
        let tick_upper = -887160i32;
        let amount = 1000000u128;
        let amount0 = U256::from(500u64);
        let amount1 = U256::from(1000u64);

        let log = make_v3_mint_log(pool, sender, owner, tick_lower, tick_upper, amount, amount0, amount1);

        let result = decode_v3_mint_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.sender, sender);
        assert_eq!(event.owner, owner);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.amount, amount);
        assert_eq!(event.amount0, amount0);
        assert_eq!(event.amount1, amount1);
    }

    #[test]
    fn decode_v3_mint_wrong_topic_returns_none() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;
        let data = vec![0u8; 128];

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![B256::ZERO, owner.into_word(), B256::ZERO, B256::ZERO],
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

        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_truncated_data_returns_none() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V3_MINT_TOPIC, owner.into_word(), B256::ZERO, B256::ZERO],
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

        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_insufficient_topics_returns_none() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;

        // Only 3 topics — need 4 (sig + owner + tickLower + tickUpper)
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V3_MINT_TOPIC, owner.into_word()],
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

        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_tick_out_of_range_returns_none() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let owner = Address::ZERO;

        // tick_lower = -887273 (out of range)
        let log = make_v3_mint_log(pool, sender, owner, -887273, 0, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_mint_log(&log).is_none());

        // tick_upper = 887273 (out of range)
        let log = make_v3_mint_log(pool, sender, owner, 0, 887273, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_positive_ticks() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let owner = Address::from([0xcc; 20]);

        let log = make_v3_mint_log(pool, sender, owner, 100, 200, 50, U256::from(10u64), U256::from(20u64));
        let event = decode_v3_mint_log(&log).unwrap();

        assert_eq!(event.tick_lower, 100);
        assert_eq!(event.tick_upper, 200);
        assert_eq!(event.amount, 50);
    }

    #[test]
    fn decode_v3_mint_no_topics_returns_none() {
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

        assert!(decode_v3_mint_log(&log).is_none());
    }

    // ── Burn tests ──────────────────────────────────────────────

    #[test]
    fn decode_valid_v3_burn() {
        let pool = Address::from([0xaa; 20]);
        let owner = Address::from([0xcc; 20]);
        let tick_lower = -60i32;
        let tick_upper = 60i32;
        let amount = 500000u128;
        let amount0 = U256::from(200u64);
        let amount1 = U256::from(400u64);

        let log = make_v3_burn_log(pool, owner, tick_lower, tick_upper, amount, amount0, amount1);

        let result = decode_v3_burn_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.owner, owner);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.amount, amount);
        assert_eq!(event.amount0, amount0);
        assert_eq!(event.amount1, amount1);
    }

    #[test]
    fn decode_v3_burn_wrong_topic_returns_none() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;
        let data = vec![0u8; 96];

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![B256::ZERO, owner.into_word(), B256::ZERO, B256::ZERO],
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

        assert!(decode_v3_burn_log(&log).is_none());
    }

    #[test]
    fn decode_v3_burn_truncated_data_returns_none() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V3_BURN_TOPIC, owner.into_word(), B256::ZERO, B256::ZERO],
            Bytes::from(vec![0u8; 64]), // too short — need 96
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

        assert!(decode_v3_burn_log(&log).is_none());
    }

    #[test]
    fn decode_v3_burn_tick_at_boundary() {
        let pool = Address::ZERO;
        let owner = Address::ZERO;

        // min tick: -887272
        let log_min = make_v3_burn_log(pool, owner, -887272, 0, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_burn_log(&log_min).is_some());

        // max tick: 887272
        let log_max = make_v3_burn_log(pool, owner, 0, 887272, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_burn_log(&log_max).is_some());

        // out of range: -887273
        let log_under = make_v3_burn_log(pool, owner, -887273, 0, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_burn_log(&log_under).is_none());

        // out of range: 887273
        let log_over = make_v3_burn_log(pool, owner, 0, 887273, 100, U256::ZERO, U256::ZERO);
        assert!(decode_v3_burn_log(&log_over).is_none());
    }

    #[test]
    fn decode_v3_burn_no_topics_returns_none() {
        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![],
            Bytes::from(vec![0u8; 96]),
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

        assert!(decode_v3_burn_log(&log).is_none());
    }

    // ── Topic discrimination tests ─────────────────────────────

    #[test]
    fn mint_topic_not_matched_by_burn_decoder() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::ZERO;
        let owner = Address::ZERO;
        let log = make_v3_mint_log(pool, sender, owner, -60, 60, 100, U256::ZERO, U256::ZERO);

        // A Mint event should NOT be decoded as a Burn
        assert!(decode_v3_burn_log(&log).is_none());
    }

    #[test]
    fn burn_topic_not_matched_by_mint_decoder() {
        let pool = Address::from([0xaa; 20]);
        let owner = Address::ZERO;
        let log = make_v3_burn_log(pool, owner, -60, 60, 100, U256::ZERO, U256::ZERO);

        // A Burn event should NOT be decoded as a Mint
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn swap_topic_not_matched_by_mint_or_burn_decoder() {
        // V3_SWAP_TOPIC should not match either decoder
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let recipient = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![
                crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC,
                sender.into_word(),
                recipient.into_word(),
            ],
            Bytes::from(vec![0u8; 160]),
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

        assert!(decode_v3_mint_log(&log).is_none());
        assert!(decode_v3_burn_log(&log).is_none());
    }
}
