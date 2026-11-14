//! Uniswap V4 Swap event decoder.
//!
//! Decodes `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`
//! events from the Uniswap V4 `PoolManager` contract.
//!
//! # V4 Swap event format
//!
//! ```text
//! event Swap(
//!     V4PoolId indexed id,       // bytes32
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
//! topic[1] = V4PoolId (indexed bytes32)
//! topic[2] = sender (indexed address)
//! data    = abi.encode(int128, int128, uint160, uint128, int24, uint24)
//!         = 6 × 32 bytes = 192 bytes
//! ```
//!
//! The V4 Swap event differs from V3 in three ways:
//! 1. `V4PoolId` (bytes32) replaces the pool contract address — V4 pools
//!    live inside `PoolManager`, not as separate contracts.
//! 2. Amounts are `int128` (not `int256`), and a `fee` field is present.
//! 3. The event is emitted by `PoolManager`, not by individual pool contracts.

use crate::uniswap_tick_range::validate_tick;
use alloy::primitives::{b256, Address, B256, I256, U128, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;

alloy::sol! {
    /// The V4 `PoolManager`'s post-swap state event.
    event Swap(
        bytes32 indexed id,
        address indexed sender,
        int128 amount0,
        int128 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick,
        uint24 fee,
    );
}

/// Keccak256 of `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`.
pub const V4_SWAP_TOPIC: B256 =
    b256!("0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f");

/// V4 pool identifier — a `bytes32` derived from `keccak256(PoolKey)`.
pub type V4PoolId = [u8; 32];

/// Decoded V4 Swap event carrying post-swap state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4SwapEvent {
    /// The pool ID (bytes32 from topic[1]).
    pub pool_id: V4PoolId,
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
/// - The log's topics are not exactly the declared three (signature + id +
///   sender) or the first one doesn't match `V4_SWAP_TOPIC` — the declaration
///   binds the topic count, so a surplus topic is a refusal, not an ignored word
/// - The data is too short (< 192 bytes) or a field is not canonical for its
///   declared width (`int128` amounts, `uint160` price, `uint128` liquidity,
///   sign-extended `int24` tick, `uint24` fee), or an indexed word is not a
///   canonical `address` slot
/// - The tick is outside the Uniswap V3 tick range
#[must_use]
pub fn decode_v4_swap_log(log: &Log) -> Option<V4SwapEvent> {
    // The declaration is the bound: topic0, the indexed `bytes32` id, the
    // indexed `address` sender, and the six data words are decoded VALIDATING
    // against the widths the `PoolManager` declares, so an over-wide fee or
    // price word is a decode failure instead of a masked value, and a tick word
    // that is not sign-extended is a failure instead of a flipped tick.
    let log = Swap::decode_log_validate(&log.inner).ok()?;

    Some(V4SwapEvent {
        pool_id: log.data.id.0,
        sender: log.data.sender,
        // The event carries `int128` amounts and a `uint24` fee; the decoded
        // event keeps them at the struct's `I256`/`u32` width. Both are
        // widenings of a validated slot — an `int128` always fits `int256` and
        // a `uint24` always fits `u32` — so neither carries a failure path.
        amount0: I256::unchecked_from(log.data.amount0),
        amount1: I256::unchecked_from(log.data.amount1),
        sqrt_price_x96: U256::from(log.data.sqrtPriceX96),
        liquidity: U128::from(log.data.liquidity),
        tick: validate_tick(log.data.tick)?,
        fee: log.data.fee.to::<u32>(),
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, U160};

    #[expect(clippy::too_many_arguments)]
    fn make_v4_swap_log(
        pool_id: V4PoolId,
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
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
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
        let sqrt_price = U256::from(79_228_162_514_264_337_593_543_950_336_u128);
        let liquidity = U128::from(1_000_000_u64);
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
        assert_eq!(
            event.amount0,
            I256::try_from(-1000_i128).unwrap_or(I256::ZERO)
        );
        assert_eq!(
            event.amount1,
            I256::try_from(500_i128).unwrap_or(I256::ZERO)
        );
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
        let sqrt_price = U256::from(77_375_349_197_886_179_843_141_176_482_u128);
        let liquidity = U128::from(5_000_000_u64);
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
            pool_id,
            sender,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            -887_272,
            fee,
        );
        assert!(decode_v4_swap_log(&log_min).is_some());

        // max tick: 887272
        let log_max = make_v4_swap_log(
            pool_id,
            sender,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            887_272,
            fee,
        );
        assert!(decode_v4_swap_log(&log_max).is_some());

        // out of range: -887273
        let log_under = make_v4_swap_log(
            pool_id,
            sender,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            -887_273,
            fee,
        );
        assert!(decode_v4_swap_log(&log_under).is_none());

        // out of range: 887273
        let log_over = make_v4_swap_log(
            pool_id,
            sender,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            887_273,
            fee,
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

    // ── malformed narrow-field words ──────────────────────────────────────

    /// Set one byte of the log's data body (`word * 32 + byte`).
    fn corrupt_data_byte(log: &mut Log, word: usize, byte: usize, value: u8) {
        let mut data = log.data().data.to_vec();
        data[word * 32 + byte] = value;
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );
    }

    /// Replace the log's data body.
    fn set_data(log: &mut Log, data: Vec<u8>) {
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );
    }

    /// Set one byte of indexed topic word `topic`.
    fn corrupt_topic_byte(log: &mut Log, topic: usize, byte: usize, value: u8) {
        let mut topics = log.topics().to_vec();
        topics[topic].0[byte] = value;
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
    }

    fn valid_swap_log() -> Log {
        make_v4_swap_log(
            [0xAB; 32],
            Address::from([0xBB; 20]),
            I256::try_from(-1_000_i128).unwrap_or(I256::ZERO),
            I256::try_from(500_i128).unwrap_or(I256::ZERO),
            U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            U128::from(1_000_000_u64),
            -60,
            500,
        )
    }

    #[test]
    fn decode_v4_swap_rejects_over_wide_fee() {
        // A `uint24` fee slot whose word carries a set bit above 24 is not a fee
        // tier — the low-four-byte read would have reported 500 here.
        let mut log = valid_swap_log();
        corrupt_data_byte(&mut log, 5, 0, 0x01);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_rejects_over_wide_sqrt_price() {
        let mut log = valid_swap_log();
        corrupt_data_byte(&mut log, 2, 0, 0x01);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_rejects_over_wide_liquidity() {
        let mut log = valid_swap_log();
        corrupt_data_byte(&mut log, 3, 0, 0x01);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_rejects_non_sign_extended_amount() {
        // A `-1` amount zero-extended to `int128` instead of sign-extended
        // reads as `2^128 - 1` under a full-width `int256` decode — the sign
        // flip this validation refuses.
        let mut log = valid_swap_log();
        let mut word = [0u8; 32];
        word[16..32].copy_from_slice(&[0xff; 16]);
        let mut data = log.data().data.to_vec();
        data[..32].copy_from_slice(&word);
        set_data(&mut log, data);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_rejects_non_sign_extended_tick() {
        let mut log = valid_swap_log();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        let mut data = log.data().data.to_vec();
        data[128..160].copy_from_slice(&word);
        set_data(&mut log, data);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_rejects_dirty_sender_topic() {
        let mut log = valid_swap_log();
        corrupt_topic_byte(&mut log, 2, 0, 0x01);
        assert!(decode_v4_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v4_swap_accepts_width_maxima() {
        let log = make_v4_swap_log(
            [0xAB; 32],
            Address::from([0xBB; 20]),
            I256::try_from(i128::MAX).unwrap_or(I256::ZERO),
            I256::try_from(i128::MIN).unwrap_or(I256::ZERO),
            U256::from(U160::MAX),
            U128::MAX,
            887_272,
            0x00FF_FFFF,
        );

        let event = decode_v4_swap_log(&log).expect("width maxima must decode cleanly");
        assert_eq!(
            event.amount0,
            I256::try_from(i128::MAX).unwrap_or(I256::ZERO)
        );
        assert_eq!(
            event.amount1,
            I256::try_from(i128::MIN).unwrap_or(I256::ZERO)
        );
        assert_eq!(event.sqrt_price_x96, U256::from(U160::MAX));
        assert_eq!(event.liquidity, U128::MAX);
        assert_eq!(event.tick, 887_272);
        assert_eq!(event.fee, 0x00FF_FFFF);
    }
}
