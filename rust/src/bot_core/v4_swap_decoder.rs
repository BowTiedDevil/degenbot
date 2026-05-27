//! Uniswap V4 Swap event decoder.
//!
//! Decodes `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`
//! events from the Uniswap V4 `PoolManager` contract.
//!
//! # V4 Swap event format
//!
//! ```text
//! event Swap(
//!     PoolId indexed id,       // bytes32
//!     address indexed sender,
//!     int128 amount0,
//!     int128 amount1,
//!     uint160 sqrtPriceX96,
//!     uint128 liquidity,
//!     int24 tick,
//!     uint24 fee
//! )
//!
//! topic[0] = 0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f
//! topic[1] = PoolId (indexed bytes32)
//! topic[2] = sender (indexed address)
//! data    = abi.encode(int128, int128, uint160, uint128, int24, uint24)
//!         = 6 × 32 bytes = 192 bytes
//! ```
//!
//! The V4 Swap event differs from V3 in three ways:
//! 1. `PoolId` (bytes32) replaces the pool contract address — V4 pools
//!    live inside `PoolManager`, not as separate contracts.
//! 2. Amounts are `int128` (not `int256`), and a `fee` field is present.
//! 3. The event is emitted by `PoolManager`, not by individual pool contracts.

use alloy::primitives::{Address, B256, I256, U128, U256};
use alloy::rpc::types::Log;

/// Keccak256 of `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`.
pub const V4_SWAP_TOPIC: B256 = B256::new([
    0x40, 0xe9, 0xce, 0xcb, 0x9f, 0x5f, 0x1f, 0x1c,
    0x5b, 0x9c, 0x97, 0xde, 0xc2, 0x91, 0x7b, 0x7e,
    0xe9, 0x2e, 0x57, 0xba, 0x55, 0x63, 0x70, 0x8d,
    0xac, 0xa9, 0x4d, 0xd8, 0x4a, 0xd7, 0x11, 0x2f,
]);

/// V4 pool identifier — a `bytes32` derived from `keccak256(PoolKey)`.
pub type PoolId = [u8; 32];

/// Decoded V4 Swap event carrying post-swap state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4SwapEvent {
    /// The pool ID (bytes32 from topic[1]).
    pub pool_id: PoolId,
    /// The sender (from topic[2]).
    pub sender: Address,
    /// Amount of currency0 (signed — negative for exact-input in V4).
    pub amount0: I256,
    /// Amount of currency1 (signed — negative for exact-input in V4).
    pub amount1: I256,
    /// Sqrt price after the swap (uint160).
    pub sqrt_price_x96: U256,
    /// Active liquidity after the swap (uint128).
    pub liquidity: U128,
    /// Current tick after the swap (int24).
    pub tick: i32,
    /// Swap fee for this swap (uint24).
    pub fee: u32,
}

/// Decode a V4 Swap event from a log.
///
/// Returns `Some(V4SwapEvent)` if the log is a valid V4 Swap event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V4_SWAP_TOPIC`
/// - The data is too short (< 192 bytes)
/// - The tick is out of the valid int24 range
#[must_use]
pub fn decode_v4_swap_log(log: &Log) -> Option<V4SwapEvent> {
    // topic[0] must match V4_SWAP_TOPIC
    let first_topic = log.topics().first()?;
    if *first_topic != V4_SWAP_TOPIC {
        return None;
    }

    // We need at least 3 topics (event sig + poolId + sender)
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }

    // Decode PoolId from topic[1] (indexed bytes32)
    let pool_id: PoolId = topics[1].0;

    // Decode sender from topic[2] (indexed address)
    let sender = Address::from_word(topics[2]);

    // data = abi.encode(int128, int128, uint160, uint128, int24, uint24)
    // = 6 × 32 bytes = 192 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 192 {
        return None;
    }

    // Decode amount0 (int128, sign-extended to int256 in 32 bytes)
    // V4 amounts are int128 but ABI-encodec as int256. We decode as I256
    // and don't range-check — amounts aren't used for solving.
    let amount0_raw = I256::from_be_bytes::<32>(data[..32].try_into().ok()?);

    // Decode amount1 (int128, sign-extended to int256 in 32 bytes)
    let amount1_raw = I256::from_be_bytes::<32>(data[32..64].try_into().ok()?);

    // Decode sqrtPriceX96 (uint160, bytes 64..96)
    let sqrt_price_x96 = U256::from_be_bytes::<32>(data[64..96].try_into().ok()?);

    // Decode liquidity (uint128, bytes 96..128)
    let liquidity = U128::from_be_bytes::<16>(data[112..128].try_into().ok()?);

    // Decode tick (int24, bytes 128..160 — sign-extended to 256 bits in ABI)
    let tick_bytes: [u8; 4] = data[156..160].try_into().ok()?;
    let tick = i32::from_be_bytes(tick_bytes);
    if !(-887_272..=887_272).contains(&tick) {
        return None;
    }

    // Decode fee (uint24, bytes 160..192)
    let fee_bytes: [u8; 4] = data[188..192].try_into().ok()?;
    let fee = u32::from_be_bytes(fee_bytes);
    if fee > 0xFFFFFF {
        return None;
    }

    Some(V4SwapEvent {
        pool_id,
        sender,
        amount0: amount0_raw,
        amount1: amount1_raw,
        sqrt_price_x96,
        liquidity,
        tick,
        fee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;

    fn make_v4_swap_log(
        pool_id: PoolId,
        sender: Address,
        amount0: I256,
        amount1: I256,
        sqrt_price_x96: U256,
        liquidity: U128,
        tick: i32,
        fee: u32,
    ) -> Log {
        let mut data = Vec::with_capacity(192);
        // amount0 (int128, sign-extended to int256)
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        // amount1 (int128, sign-extended to int256)
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
        // fee (uint24, left-padded to 32 bytes)
        let mut fee_word = [0u8; 32];
        fee_word[28..32].copy_from_slice(&fee.to_be_bytes());
        data.extend_from_slice(&fee_word);

        let pool_id_topic = B256::from(pool_id);

        let inner = alloy::primitives::Log::new_unchecked(
            Address::from([0x00u8; 20]), // PoolManager address (placeholder)
            vec![V4_SWAP_TOPIC, pool_id_topic, sender.into_word()],
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
    fn decode_valid_v4_swap() {
        let pool_id = [0xABu8; 32];
        let sender = Address::from([0xBBu8; 20]);
        let sqrt_price = U256::from(79228162514264337593543950336u128);
        let liquidity = U128::from(1000000u64);
        let tick = 0i32;
        let fee = 3000u32;

        let log = make_v4_swap_log(
            pool_id,
            sender,
            I256::try_from(-1000_i128).unwrap_or(I256::ZERO),
            I256::try_from(500_i128).unwrap_or(I256::ZERO),
            sqrt_price,
            liquidity,
            tick,
            fee,
        );

        let result = decode_v4_swap_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_id, pool_id);
        assert_eq!(event.sender, sender);
        assert_eq!(event.amount0, I256::try_from(-1000_i128).unwrap_or(I256::ZERO));
        assert_eq!(event.amount1, I256::try_from(500_i128).unwrap_or(I256::ZERO));
        assert_eq!(event.sqrt_price_x96, sqrt_price);
        assert_eq!(event.liquidity, liquidity);
        assert_eq!(event.tick, 0);
        assert_eq!(event.fee, 3000);
    }

    #[test]
    fn decode_v4_swap_wrong_topic_returns_none() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;
        let data = vec![0u8; 192];

        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![B256::ZERO, B256::from(pool_id), sender.into_word()],
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

        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_truncated_data_returns_none() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;

        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![V4_SWAP_TOPIC, B256::from(pool_id), sender.into_word()],
            Bytes::from(vec![0u8; 64]), // too short — need 192
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

        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_negative_tick() {
        let pool_id = [0xCDu8; 32];
        let sender = Address::from([0xEEu8; 20]);
        let sqrt_price = U256::from(77375349197886179843141176482u128);
        let liquidity = U128::from(5000000u64);
        let tick = -100i32;
        let fee = 500u32;

        let log = make_v4_swap_log(
            pool_id,
            sender,
            I256::try_from(200_i128).unwrap_or(I256::ZERO),
            I256::try_from(-100_i128).unwrap_or(I256::ZERO),
            sqrt_price,
            liquidity,
            tick,
            fee,
        );

        let result = decode_v4_swap_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.tick, -100);
        assert_eq!(event.fee, 500);
        assert_eq!(event.pool_id, pool_id);
    }

    #[test]
    fn decode_v4_swap_tick_at_boundary() {
        let pool_id = [0u8; 32];
        let sender = Address::ZERO;
        let sqrt_price = U256::from(1u64);
        let liquidity = U128::ZERO;
        let fee = 0u32;

        // min tick: -887272
        let log_min = make_v4_swap_log(
            pool_id, sender,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, -887272, fee,
        );
        assert!(decode_v4_swap_log(&log_min).is_some());

        // max tick: 887272
        let log_max = make_v4_swap_log(
            pool_id, sender,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, 887272, fee,
        );
        assert!(decode_v4_swap_log(&log_max).is_some());

        // out of range: -887273
        let log_under = make_v4_swap_log(
            pool_id, sender,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, -887273, fee,
        );
        assert!(decode_v4_swap_log(&log_under).is_none());

        // out of range: 887273
        let log_over = make_v4_swap_log(
            pool_id, sender,
            I256::ZERO, I256::ZERO, sqrt_price, liquidity, 887273, fee,
        );
        assert!(decode_v4_swap_log(&log_over).is_none());
    }

    #[test]
    fn decode_v4_swap_no_topics_returns_none() {
        let inner = alloy::primitives::Log::new_unchecked(
            Address::ZERO,
            vec![],
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

        assert!(decode_v4_swap_log(&log).is_none());
    }
}
