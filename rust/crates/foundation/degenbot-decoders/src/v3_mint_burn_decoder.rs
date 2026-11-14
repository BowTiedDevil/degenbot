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

use crate::uniswap_tick_range::validate_tick;
use alloy::primitives::{b256, Address, B256, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;

alloy::sol! {
    /// A V3 pool's liquidity-mint event: the position's tick range plus the
    /// liquidity and token amounts added.
    event Mint(
        address sender,
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1,
    );

    /// A V3 pool's liquidity-burn event: the position's tick range plus the
    /// liquidity and token amounts removed.
    event Burn(
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1,
    );
}

/// Keccak256 of `Mint(address,address,int24,int24,uint128,uint256,uint256)`.
pub const V3_MINT_TOPIC: B256 =
    b256!("0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde");

/// Keccak256 of `Burn(address,int24,int24,uint128,uint256,uint256)`.
pub const V3_BURN_TOPIC: B256 =
    b256!("0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c");

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
/// Returns `Some(V3MintEvent)` if the log is a valid Mint event with correctly
/// formatted data. Returns `None` if:
/// - The log's topics are not exactly the declared four (signature + owner +
///   tickLower + tickUpper) or the first one doesn't match `V3_MINT_TOPIC` —
///   the declaration binds the topic count, so a surplus topic is a refusal,
///   not an ignored word
/// - An indexed word is not a canonical `address` / sign-extended `int24` slot
/// - The data is too short (< 128 bytes) or a data word is not canonical for
///   its declared width (`address` sender, `uint128` amount)
/// - Tick values are outside the Uniswap V3 tick range
#[must_use]
pub fn decode_v3_mint_log(log: &Log) -> Option<V3MintEvent> {
    // The declaration is the bound: topic0, the indexed owner + tick words, and
    // the four data words are decoded VALIDATING against the widths the pool
    // declares, so a data sender with a non-zero 12-byte prefix or an amount
    // word above 128 bits is a decode failure instead of a truncated value.
    let log = Mint::decode_log_validate(&log.inner).ok()?;

    Some(V3MintEvent {
        pool_address: log.address,
        sender: log.data.sender,
        owner: log.data.owner,
        tick_lower: validate_tick(log.data.tickLower)?,
        tick_upper: validate_tick(log.data.tickUpper)?,
        amount: log.data.amount,
        amount0: log.data.amount0,
        amount1: log.data.amount1,
    })
}

/// Decode a V3 Burn event from a log.
///
/// Returns `Some(V3BurnEvent)` if the log is a valid Burn event with correctly
/// formatted data. Returns `None` if:
/// - The log's topics are not exactly the declared four (signature + owner +
///   tickLower + tickUpper) or the first one doesn't match `V3_BURN_TOPIC` —
///   the declaration binds the topic count, so a surplus topic is a refusal,
///   not an ignored word
/// - An indexed word is not a canonical `address` / sign-extended `int24` slot
/// - The data is too short (< 96 bytes) or the `uint128` amount word carries
///   bits above 128
/// - Tick values are outside the Uniswap V3 tick range
#[must_use]
pub fn decode_v3_burn_log(log: &Log) -> Option<V3BurnEvent> {
    // The declaration is the bound here too: every word of the burn log is
    // decoded VALIDATING against the width the pool declares.
    let log = Burn::decode_log_validate(&log.inner).ok()?;

    Some(V3BurnEvent {
        pool_address: log.address,
        owner: log.data.owner,
        tick_lower: validate_tick(log.data.tickLower)?,
        tick_upper: validate_tick(log.data.tickUpper)?,
        amount: log.data.amount,
        amount0: log.data.amount0,
        amount1: log.data.amount1,
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unreadable_literal, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, I256, U128};

    #[expect(clippy::too_many_arguments)]
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
            vec![
                V3_MINT_TOPIC,
                owner.into_word(),
                tick_lower_topic,
                tick_upper_topic,
            ],
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
            vec![
                V3_BURN_TOPIC,
                owner.into_word(),
                tick_lower_topic,
                tick_upper_topic,
            ],
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

        let log = make_v3_mint_log(
            pool, sender, owner, tick_lower, tick_upper, amount, amount0, amount1,
        );

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

        // 2 topics — the event declares 4 (sig + owner + tickLower + tickUpper)
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
    fn decode_v3_mint_tick_at_protocol_bounds() {
        // Both protocol bounds are valid ticks, so both decode with the bound
        // intact; only values outside them are refused.
        let pool = Address::ZERO;
        let sender = Address::ZERO;
        let owner = Address::ZERO;

        let log_min = make_v3_mint_log(
            pool,
            sender,
            owner,
            -887_272,
            0,
            100,
            U256::ZERO,
            U256::ZERO,
        );
        let event = decode_v3_mint_log(&log_min).expect("min tick must decode");
        assert_eq!(event.tick_lower, -887_272);

        let log_max =
            make_v3_mint_log(pool, sender, owner, 0, 887_272, 100, U256::ZERO, U256::ZERO);
        let event = decode_v3_mint_log(&log_max).expect("max tick must decode");
        assert_eq!(event.tick_upper, 887_272);
    }

    #[test]
    fn decode_v3_mint_positive_ticks() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let owner = Address::from([0xcc; 20]);

        let log = make_v3_mint_log(
            pool,
            sender,
            owner,
            100,
            200,
            50,
            U256::from(10u64),
            U256::from(20u64),
        );
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

        let log = make_v3_burn_log(
            pool, owner, tick_lower, tick_upper, amount, amount0, amount1,
        );

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
        let event = decode_v3_burn_log(&log_min).expect("min tick must decode");
        assert_eq!(event.tick_lower, -887_272);

        // max tick: 887272
        let log_max = make_v3_burn_log(pool, owner, 0, 887272, 100, U256::ZERO, U256::ZERO);
        let event = decode_v3_burn_log(&log_max).expect("max tick must decode");
        assert_eq!(event.tick_upper, 887_272);

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
                crate::v3_swap_decoder::V3_SWAP_TOPIC,
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

    fn valid_mint_log() -> Log {
        make_v3_mint_log(
            Address::from([0xaa; 20]),
            Address::from([0xbb; 20]),
            Address::from([0xcc; 20]),
            -60,
            60,
            1_000_000,
            U256::from(500u64),
            U256::from(1000u64),
        )
    }

    fn valid_burn_log() -> Log {
        make_v3_burn_log(
            Address::from([0xaa; 20]),
            Address::from([0xcc; 20]),
            -60,
            60,
            500_000,
            U256::from(200u64),
            U256::from(400u64),
        )
    }

    #[test]
    fn decode_v3_mint_rejects_dirty_sender_address() {
        // The non-indexed `sender` data word is an `address` slot; a non-zero
        // 12-byte prefix is not an address.
        let mut log = valid_mint_log();
        corrupt_data_byte(&mut log, 0, 0, 0x01);
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_rejects_over_wide_amount() {
        // The `uint128` amount word's top half is not a fixed-width tail: a set
        // bit above 128 must be a decode failure, not a truncated amount.
        let mut log = valid_mint_log();
        corrupt_data_byte(&mut log, 1, 0, 0x01);
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_rejects_non_sign_extended_indexed_tick() {
        // An indexed `int24` word that is not sign-extended is malformed; a
        // tail-only read would have reported the same tick.
        let mut log = valid_mint_log();
        let mut topics = log.topics().to_vec();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        topics[2].0 = word;
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_rejects_dirty_owner_topic() {
        let mut log = valid_mint_log();
        corrupt_topic_byte(&mut log, 1, 0, 0x01);
        assert!(decode_v3_mint_log(&log).is_none());
    }

    #[test]
    fn decode_v3_burn_rejects_over_wide_amount() {
        let mut log = valid_burn_log();
        corrupt_data_byte(&mut log, 0, 0, 0x01);
        assert!(decode_v3_burn_log(&log).is_none());
    }

    #[test]
    fn decode_v3_burn_rejects_non_sign_extended_indexed_tick() {
        let mut log = valid_burn_log();
        let mut topics = log.topics().to_vec();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(60_i32).to_be_bytes()[1..]);
        word[0] = 0xff;
        topics[3].0 = word;
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
        assert!(decode_v3_burn_log(&log).is_none());
    }

    #[test]
    fn decode_v3_mint_accepts_uint128_max_amount() {
        let log = make_v3_mint_log(
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            -60,
            60,
            u128::MAX,
            U256::MAX,
            U256::MAX,
        );

        let event = decode_v3_mint_log(&log).expect("uint128::MAX must decode cleanly");
        assert_eq!(event.amount, u128::MAX);
        assert_eq!(event.amount0, U256::MAX);
        assert_eq!(event.amount1, U256::MAX);
    }
}
