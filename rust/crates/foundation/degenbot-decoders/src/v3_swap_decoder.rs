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

use crate::uniswap_tick_range::validate_tick;
use alloy::primitives::{b256, Address, B256, I256, U128, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;

alloy::sol! {
    /// The V3 pool's post-swap state event.
    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick,
    );
}

/// Keccak256 of `Swap(address,address,int256,int256,uint160,uint128,int24)`.
pub const V3_SWAP_TOPIC: B256 =
    b256!("0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

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
/// Returns `Some(V3SwapEvent)` if the log is a valid Swap event with correctly
/// formatted data. Returns `None` if:
/// - The log's topics are not exactly the declared three (signature + sender +
///   recipient) or the first one doesn't match `V3_SWAP_TOPIC` — the
///   declaration binds the topic count, so a surplus topic is a refusal, not an
///   ignored word
/// - The data is too short (< 160 bytes) or a state word is not canonical for
///   its declared width (`uint160` price, `uint128` liquidity, sign-extended
///   `int24` tick), or an indexed word is not a canonical `address` slot
/// - The tick is outside the Uniswap V3 tick range
///
/// Words past the tick are unread, so a fork's trailing data still decodes.
#[must_use]
pub fn decode_v3_swap_log(log: &Log) -> Option<V3SwapEvent> {
    // The declaration is the bound: topic0, both indexed `address` words, and
    // the five state words are decoded VALIDATING against the widths the pool
    // declares, so an over-wide price or liquidity word is a decode failure
    // instead of a narrowed value, and a tick word that is not sign-extended is
    // a failure instead of a flipped tick. Words past the tick are unread.
    let log = Swap::decode_log_validate(&log.inner).ok()?;

    Some(V3SwapEvent {
        pool_address: log.address,
        sender: log.data.sender,
        recipient: log.data.recipient,
        amount0: log.data.amount0,
        amount1: log.data.amount1,
        sqrt_price_x96: U256::from(log.data.sqrtPriceX96),
        liquidity: U128::from(log.data.liquidity),
        tick: validate_tick(log.data.tick)?,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, U160};

    #[expect(clippy::too_many_arguments)]
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
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
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
        let sqrt_price = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // ~1.0 price
        let liquidity = U128::from(1_000_000_u64);
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
        let sqrt_price = U256::from(77_375_349_197_886_179_843_141_176_482_u128); // below 1.0
        let liquidity = U128::from(5_000_000_u64);
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
            pool,
            sender,
            recipient,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            -887_272,
        );
        assert!(decode_v3_swap_log(&log_min).is_some());

        // max tick: 887272
        let log_max = make_v3_swap_log(
            pool,
            sender,
            recipient,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            887_272,
        );
        assert!(decode_v3_swap_log(&log_max).is_some());

        // out of range: -887273
        let log_under = make_v3_swap_log(
            pool,
            sender,
            recipient,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            -887_273,
        );
        assert!(decode_v3_swap_log(&log_under).is_none());

        // out of range: 887273
        let log_over = make_v3_swap_log(
            pool,
            sender,
            recipient,
            I256::ZERO,
            I256::ZERO,
            sqrt_price,
            liquidity,
            887_273,
        );
        assert!(decode_v3_swap_log(&log_over).is_none());
    }

    #[test]
    fn decode_v3_swap_no_topics_returns_none() {
        let pool = Address::ZERO;
        let inner =
            alloy::primitives::Log::new_unchecked(pool, vec![], Bytes::from(vec![0u8; 160]));
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

    /// Set one byte of indexed topic word `topic`.
    fn corrupt_topic_byte(log: &mut Log, topic: usize, byte: usize, value: u8) {
        let mut topics = log.topics().to_vec();
        topics[topic].0[byte] = value;
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
    }

    fn valid_swap_log() -> Log {
        make_v3_swap_log(
            Address::from([0xaa; 20]),
            Address::from([0xbb; 20]),
            Address::from([0xcc; 20]),
            I256::try_from(-1_000_i128).unwrap_or(I256::ZERO),
            I256::try_from(500_i128).unwrap_or(I256::ZERO),
            U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            U128::from(1_000_000_u64),
            -60,
        )
    }

    #[test]
    fn decode_v3_swap_rejects_over_wide_sqrt_price() {
        // A `uint160` slot whose bits above 160 are set is not a price — the
        // decoder must refuse the log, not carry the junk bits in the U256.
        let mut log = valid_swap_log();
        corrupt_data_byte(&mut log, 2, 0, 0x01);
        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_rejects_over_wide_liquidity() {
        // `uint128` liquidity: a set bit in the word's top half would be
        // silently dropped by a fixed 16-byte tail read.
        let mut log = valid_swap_log();
        corrupt_data_byte(&mut log, 3, 0, 0x01);
        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_rejects_non_sign_extended_tick() {
        // The ABI sign-extends `int24` to 32 bytes. A word carrying the -60
        // value zero-extended instead is malformed, and a tail-only read would
        // report -60 as if the log were canonical.
        let mut log = valid_swap_log();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        let mut data = log.inner.data.data.to_vec();
        data[128..160].copy_from_slice(&word);
        log.inner.data.data = Bytes::from(data);
        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_rejects_dirty_sender_topic() {
        // An indexed `address` word with a non-zero 12-byte prefix is not an
        // address slot.
        let mut log = valid_swap_log();
        corrupt_topic_byte(&mut log, 1, 0, 0x01);
        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_rejects_dirty_recipient_topic() {
        let mut log = valid_swap_log();
        corrupt_topic_byte(&mut log, 2, 11, 0xff);
        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_surplus_topic_returns_none() {
        // The declaration binds the topic count: a fourth topic is not a V3
        // Swap log, so it is refused instead of ignored.
        let mut log = valid_swap_log();
        let mut topics = log.topics().to_vec();
        topics.push(B256::repeat_byte(0x01));
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());

        assert!(decode_v3_swap_log(&log).is_none());
    }

    #[test]
    fn decode_v3_swap_ignores_trailing_data() {
        // A fork appending words past `tick` still decodes: the declaration
        // reads the five declared words and leaves the tail unread.
        let mut log = valid_swap_log();
        let mut data = log.data().data.to_vec();
        data.extend_from_slice(&[0xff; 32]);
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );

        let event = decode_v3_swap_log(&log).expect("trailing data must not fail the decode");
        assert_eq!(event.tick, -60);
        assert_eq!(
            event.amount0,
            I256::try_from(-1_000_i128).unwrap_or(I256::ZERO)
        );
    }

    #[test]
    fn decode_v3_swap_accepts_width_maxima() {
        // The highest canonical value each narrow slot can carry still decodes.
        let log = make_v3_swap_log(
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            I256::ZERO,
            I256::ZERO,
            U256::from(U160::MAX),
            U128::MAX,
            -60,
        );

        let event = decode_v3_swap_log(&log).expect("width maxima must decode cleanly");
        assert_eq!(event.sqrt_price_x96, U256::from(U160::MAX));
        assert_eq!(event.liquidity, U128::MAX);
    }
}
