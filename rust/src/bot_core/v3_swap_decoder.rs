//! Uniswap V3 Swap event decoder.
//!
//! Decodes `Swap(address,address,int256,int256,uint160,uint128,int24)` events
//! from Uniswap V3 pool contracts. The Swap event is emitted on every V3 swap
//! and carries the final state (sqrt price, liquidity, tick) after the swap.
//!
//! # Swap event format
//!
//! ```text
//! event Swap(
//!     address indexed sender,
//!     address indexed recipient,
//!     int256 amount0,
//!     int256 amount1,
//!     uint160 sqrtPriceX96,
//!     uint128 liquidity,
//!     int24 tick
//! )
//!
//! topic[0] = 0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67
//! topic[1] = sender (indexed)
//! topic[2] = recipient (indexed)
//! data    = abi.encode(int256, int256, uint160, uint128, int24) = 160 bytes
//!           (5 × 32-byte words, left-padded)
//! ```

use alloy::primitives::{Address, B256, I256, U128, U256};
use alloy::rpc::types::Log;

/// Keccak256 of `Swap(address,address,int256,int256,uint160,uint128,int24)`.
pub const V3_SWAP_TOPIC: B256 = B256::new([
    0xc4, 0x20, 0x79, 0xf9, 0x4a, 0x63, 0x50, 0xd7,
    0xe6, 0x23, 0x5f, 0x29, 0x17, 0x49, 0x24, 0xf9,
    0x28, 0xcc, 0x2a, 0xc8, 0x18, 0xeb, 0x64, 0xfe,
    0xd8, 0x00, 0x4e, 0x11, 0x5f, 0xbc, 0xca, 0x67,
]);

/// Decoded V3 Swap event carrying post-swap state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3SwapEvent {
    /// The pool contract that emitted the event.
    pub pool_address: Address,
    /// The sender (from topic[1]).
    pub sender: Address,
    /// The recipient (from topic[2]).
    pub recipient: Address,
    /// Amount of token0 (signed — negative for exact-in).
    pub amount0: I256,
    /// Amount of token1 (signed — negative for exact-in).
    pub amount1: I256,
    /// Sqrt price after the swap (uint160).
    pub sqrt_price_x96: U256,
    /// Active liquidity after the swap (uint128).
    pub liquidity: U128,
    /// Current tick after the swap (int24).
    pub tick: i32,
}

/// Decode a V3 Swap event from a log.
///
/// Returns `Some(V3SwapEvent)` if the log is a valid Swap event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V3_SWAP_TOPIC`
/// - The data is too short (< 160 bytes)
/// - The tick is out of the valid int24 range
#[must_use]
pub fn decode_v3_swap_log(log: &Log) -> Option<V3SwapEvent> {
    // topic[0] must match V3_SWAP_TOPIC
    let first_topic = log.topics().first()?;
    if *first_topic != V3_SWAP_TOPIC {
        return None;
    }

    // We need at least 3 topics (event sig + sender + recipient)
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }

    // Decode sender from topic[1] (indexed address)
    let sender = Address::from_word(topics[1]);
    // Decode recipient from topic[2] (indexed address)
    let recipient = Address::from_word(topics[2]);

    // data = abi.encode(int256, int256, uint160, uint128, int24)
    // = 5 × 32 bytes = 160 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 160 {
        return None;
    }

    // Decode amount0 (int256, bytes 0..32)
    let amount0 = I256::from_be_bytes::<32>(data[..32].try_into().ok()?);

    // Decode amount1 (int256, bytes 32..64)
    let amount1 = I256::from_be_bytes::<32>(data[32..64].try_into().ok()?);

    // Decode sqrtPriceX96 (uint160, bytes 64..96)
    let sqrt_price_x96 = U256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    // Decode liquidity (uint128, bytes 96..128)
    let liquidity = U128::from_be_bytes::<16>(data[112..128].try_into().ok()?);

    // Decode tick (int24, bytes 128..160 — sign-extended to 256 bits in ABI)
    // The actual value fits in the last 4 bytes; read as i32 directly.
    // int24 range: [-887272, 887272]
    let tick_bytes: [u8; 4] = data[156..160].try_into().ok()?;
    let tick = i32::from_be_bytes(tick_bytes);
    if !(-887_272..=887_272).contains(&tick) {
        return None;
    }

    Some(V3SwapEvent {
        pool_address: log.address(),
        sender,
        recipient,
        amount0,
        amount1,
        sqrt_price_x96,
        liquidity,
        tick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;

    #[allow(clippy::too_many_arguments)]
    fn make_v3_swap_log(
        pool_address: Address,
        sender: Address,
        recipient: Address,
        amount0: I256,
        amount1: I256,
        sqrt_price_x96: U256,
        liquidity: U128,
        tick: i32,
    ) -> Log {
        let mut data = Vec::with_capacity(160);
        // amount0 (int256)
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        // amount1 (int256)
        data.extend_from_slice(&amount1.to_be_bytes::<32>());
        // sqrtPriceX96 (uint160, left-padded to 32 bytes)
        data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
        // liquidity (uint128, left-padded to 32 bytes)
        let liq_bytes = liquidity.to_be_bytes::<16>();
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liq_bytes);
        data.extend_from_slice(&liq_word);
        // tick (int24, sign-extended to int256 → 32 bytes)
        let tick_i256 = I256::try_from(i128::from(tick))
            .unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_i256.to_be_bytes::<32>());

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V3_SWAP_TOPIC, sender.into_word(), recipient.into_word()],
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
    fn decode_valid_v3_swap() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let recipient = Address::from([0xcc; 20]);
        let sqrt_price = U256::from(79228162514264337593543950336u128); // ~1.0 price
        let liquidity = U128::from(1000000u64);
        let tick = 0i32;

        let log = make_v3_swap_log(
            pool,
            sender,
            recipient,
            I256::try_from(-1000_i128).unwrap_or(I256::ZERO),
            I256::try_from(500_i128).unwrap_or(I256::ZERO),
            sqrt_price,
            liquidity,
            tick,
        );

        let result = decode_v3_swap_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.sender, sender);
        assert_eq!(event.recipient, recipient);
        assert_eq!(event.amount0, I256::try_from(-1000_i128).unwrap_or(I256::ZERO));
        assert_eq!(event.amount1, I256::try_from(500_i128).unwrap_or(I256::ZERO));
        assert_eq!(event.sqrt_price_x96, sqrt_price);
        assert_eq!(event.liquidity, liquidity);
        assert_eq!(event.tick, 0);
    }

    #[test]
    fn decode_v3_swap_wrong_topic_returns_none() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let recipient = Address::ZERO;
        let data = vec![0u8; 160];

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![B256::ZERO, sender.into_word(), recipient.into_word()],
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

        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_truncated_data_returns_none() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let recipient = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V3_SWAP_TOPIC, sender.into_word(), recipient.into_word()],
            Bytes::from(vec![0u8; 64]), // too short
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

        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_negative_tick() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let recipient = Address::ZERO;
        let sqrt_price = U256::from(77375349197886179843141176482u128); // below 1.0
        let liquidity = U128::from(5000000u64);
        let tick = -100i32;

        let log = make_v3_swap_log(
            pool,
            sender,
            recipient,
            I256::try_from(200_i128).unwrap_or(I256::ZERO),
            I256::try_from(-100_i128).unwrap_or(I256::ZERO),
            sqrt_price,
            liquidity,
            tick,
        );

        let result = decode_v3_swap_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.tick, -100);
    }

    #[test]
    fn decode_v3_swap_tick_at_boundary() {
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let recipient = Address::ZERO;
        let sqrt_price = U256::from(1u64); // placeholder
        let liquidity = U128::ZERO;

        // min tick: -887272
        let log_min = make_v3_swap_log(
            pool, sender, recipient,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, -887272,
        );
        assert!(decode_v3_swap_log(&log_min).is_some());

        // max tick: 887272
        let log_max = make_v3_swap_log(
            pool, sender, recipient,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, 887272,
        );
        assert!(decode_v3_swap_log(&log_max).is_some());

        // out of range: -887273
        let log_under = make_v3_swap_log(
            pool, sender, recipient,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, -887273,
        );
        assert!(decode_v3_swap_log(&log_under).is_none());

        // out of range: 887273
        let log_over = make_v3_swap_log(
            pool, sender, recipient,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, 887273,
        );
        assert!(decode_v3_swap_log(&log_over).is_none());
    }

    #[test]
    fn decode_v3_swap_no_topics_returns_none() {
        let pool = Address::ZERO;
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![],
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

        assert!(decode_v3_swap_log(&log).is_none());
    }
}
