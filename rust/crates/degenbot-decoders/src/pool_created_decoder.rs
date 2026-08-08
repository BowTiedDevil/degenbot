//! Uniswap V2/V3/V4 `PoolCreated` event decoders.
//!
//! The chunk-loop fetcher (epic `2SFL6I`) decodes `PoolCreated` events from
//! a block range to discover pools for the DB write seams. This module is
//! the pure-Rust decode leaf — alloy-only, no `pyo3`/`tokio`/`degenbot-core`/
//! `degenbot-abi` (the same constraint as [`crate::v3_mint_burn_decoder`] +
//! [`crate::v4_modify_liquidity_decoder`]). Each decoder hand-slices the EVM
//! log bytes + returns a plain `Option<Event>` struct, independently
//! testable + reusable in non-Python Rust code.
//!
//! The returned structs are decoder-native (NOT the `degenbot-db`
//! `V2/V3/V4PoolRowInput` row-input types — those carry DB-specific concerns
//! like `fee_denominator`/`kind` discriminators; mapping a decoded event to
//! a row-input is the chunk loop's job, not the leaf's). This keeps the leaf
//! unfettered by DB concerns + preserves the alloy-only property.
//!
//! Event signatures + topic constants are reproduced from the Python
//! `src/degenbot/cli/pool.py` event-hash constants (the canonical degenbot
//! source), computed as keccak256 of the canonical event signature.
//!
//! # V2 `PairCreated` (Uniswap V2 family)
//!
//! ```text
//! event PairCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     address pair,
//!     uint
//! )
//!
//! topic[0] = 0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9
//! topic[1] = token0 (indexed address)
//! topic[2] = token1 (indexed address)
//! data    = abi.encode(address pair, uint totalSupply)
//!         = 2 × 32 bytes = 64 bytes
//! ```
//!
//! # Aerodrome V2 `PoolCreated` (the stable-flag variant)
//!
//! ```text
//! event PoolCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     bool indexed stable,
//!     address pool,
//!     uint256 type
//! )
//!
//! topic[0] = 0x2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e
//! topic[1] = token0 (indexed address)
//! topic[2] = token1 (indexed address)
//! topic[3] = stable (indexed bool)
//! data    = abi.encode(address pool, uint256 type)
//!         = 2 × 32 bytes = 64 bytes
//! ```
//!
//! # V3 `PoolCreated`
//!
//! ```text
//! event PoolCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     uint24 indexed fee,
//!     int24 tickSpacing,
//!     address pool
//! )
//!
//! topic[0] = 0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118
//! topic[1] = token0 (indexed address)
//! topic[2] = token1 (indexed address)
//! topic[3] = fee (indexed uint24)
//! data    = abi.encode(int24 tickSpacing, address pool)
//!         = 2 × 32 bytes = 64 bytes
//! ```
//!
//! # V4 `Initialize` (the V4 `PoolCreated`)
//!
//! ```text
//! event Initialize(
//!     V4PoolId indexed id,
//!     address indexed currency0,
//!     address indexed currency1,
//!     uint24 fee,
//!     int24 tickSpacing,
//!     address hooks
//!     // ... trailing fields (sqrtPriceX96, sqrtPriceX96 ...) ignored by degenbot;
//!     // degenbot reads only the first 3 × 32 bytes of data.
//! )
//!
//! topic[0] = 0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438
//! topic[1] = V4PoolId (indexed bytes32)
//! topic[2] = currency0 (indexed address)
//! topic[3] = currency1 (indexed address)
//! data    = abi.encode(uint24 fee, int24 tickSpacing, address hooks, ...)
//!         = ≥ 3 × 32 bytes (degenbot reads only the first 96)
//! ```

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Log;

use crate::uniswap_tick_range::extract_int24_from_word;
use crate::v4_swap_decoder::V4PoolId;

// ── topic constants ───────────────────────────────────────────────────────

/// Keccak256 of `PairCreated(address,address,address,uint256)` — the
/// Uniswap V2 family sig (shared by `Uniswap`, `PancakeSwap`, `SushiSwap`,
/// Swapbased V2, per `src/degenbot/cli/pool.py` constants).
pub const V2_PAIR_CREATED_TOPIC: B256 = B256::new([
    0x0d, 0x36, 0x48, 0xbd, 0x0f, 0x6b, 0xa8, 0x01, 0x34, 0xa3, 0x3b, 0xa9, 0x27, 0x5a, 0xc5, 0x85,
    0xd9, 0xd3, 0x15, 0xf0, 0xad, 0x83, 0x55, 0xcd, 0xde, 0xfd, 0xe3, 0x1a, 0xfa, 0x28, 0xd0, 0xe9,
]);

/// Keccak256 of `PoolCreated(address,address,bool,address,uint256)` — the
/// Aerodrome V2 family sig (the V2 variant with a `stable` indexed flag).
pub const AERODROME_V2_POOL_CREATED_TOPIC: B256 = B256::new([
    0x21, 0x28, 0xd8, 0x8d, 0x14, 0xc8, 0x0c, 0xb0, 0x81, 0xc1, 0x25, 0x2a, 0x5a, 0xcf, 0xf7, 0xa2,
    0x64, 0x67, 0x1b, 0xf1, 0x99, 0xce, 0x22, 0x6b, 0x53, 0x78, 0x8f, 0xb2, 0x60, 0x65, 0x00, 0x5e,
]);

/// Keccak256 of Aerodrome V3's `PoolCreated` event signature — a distinct
/// topic0 from the canonical Uniswap V3 sig ([`V3_POOL_CREATED_TOPIC`]),
/// though structurally decoded the same way (token0/token1/fee in
/// topics[1..=3], `tickSpacing`/`pool_address` in non-indexed data, per the
/// Python `update_v3_pools` updater that handles both `uniswap_v3` and
/// `aerodrome_v3`). See `src/degenbot/cli/pool.py`'s
/// `AERODROME_V3_POOLCREATED_EVENT_HASH`. The decode leaf that accepts this
/// topic is deferred to task `CKXCOB` (the chunk loop) — the constant is
/// surfaced here so `ExchangeSpec.event_topic` can carry it faithfully.
pub const AERODROME_V3_POOL_CREATED_TOPIC: B256 = B256::new([
    0xab, 0x0d, 0x57, 0xf0, 0xdf, 0x53, 0x7b, 0xb2, 0x5e, 0x80, 0x24, 0x5e, 0xf7, 0x74, 0x8f, 0xa6,
    0x23, 0x53, 0x80, 0x8c, 0x54, 0xd6, 0xe5, 0x28, 0xa9, 0xdd, 0x20, 0x88, 0x7a, 0xed, 0x9a, 0xc2,
]);

/// Keccak256 of `PoolCreated(address,address,uint24,int24,address)` — the
/// Uniswap V3 family sig (`PancakeSwap V3` + `SushiSwap V3` share it).
pub const V3_POOL_CREATED_TOPIC: B256 = B256::new([
    0x78, 0x3c, 0xca, 0x1c, 0x04, 0x12, 0xdd, 0x0d, 0x69, 0x5e, 0x78, 0x45, 0x68, 0xc9, 0x6d, 0xa2,
    0xe9, 0xc2, 0x2f, 0xf9, 0x89, 0x35, 0x7a, 0x2e, 0x8b, 0x1d, 0x9b, 0x2b, 0x4e, 0x6b, 0x71, 0x18,
]);

/// Keccak256 of `Initialize(bytes32,address,address,uint24,int24,address,...)`
/// — the Uniswap V4 `PoolCreated` sig (degenbot treats V4 `Initialize` as the
/// pool-creation event, per the Python `UNISWAP_V4_POOLCREATED_EVENT_HASH`).
pub const V4_INITIALIZE_TOPIC: B256 = B256::new([
    0xdd, 0x46, 0x6e, 0x67, 0x4e, 0xa5, 0x57, 0xf5, 0x62, 0x95, 0xe2, 0xd0, 0x21, 0x8a, 0x12, 0x5e,
    0xa4, 0xb4, 0xf0, 0xf6, 0xf3, 0x30, 0x7b, 0x95, 0xf8, 0x5e, 0x61, 0x10, 0x83, 0x8d, 0x64, 0x38,
]);

// ── decoded structs ───────────────────────────────────────────────────────

/// Decoded V2 `PairCreated` event (the canonical Uniswap V2 family —
/// no `stable` flag). `pool_address` is the newly-created pair contract
/// (extracted from non-indexed data; the emitter is the factory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2PairCreatedEvent {
    /// The factory that emitted the event (the log emitter).
    pub factory_address: Address,
    /// Token0 (from topic[1]).
    pub token0: Address,
    /// Token1 (from topic[2]).
    pub token1: Address,
    /// The created pair/pool address (from data word 0).
    pub pool_address: Address,
}

/// Decoded Aerodrome V2 `PoolCreated` event (the `stable`-flag V2 variant).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AerodromeV2PoolCreatedEvent {
    /// The factory that emitted the event (the log emitter).
    pub factory_address: Address,
    /// Token0 (from topic[1]).
    pub token0: Address,
    /// Token1 (from topic[2]).
    pub token1: Address,
    /// The stable flag (from topic[3] — a bool indexed as uint256).
    pub stable: bool,
    /// The created pool address (from data word 0).
    pub pool_address: Address,
}

/// Decoded V3 `PoolCreated` event. `pool_address` is the new V3 pool contract
/// (from data word 1; the emitter is the factory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V3PoolCreatedEvent {
    /// The factory that emitted the event (the log emitter).
    pub factory_address: Address,
    /// Token0 (from topic[1]).
    pub token0: Address,
    /// Token1 (from topic[2]).
    pub token1: Address,
    /// The swap fee, in hundredths of a bip (from topic[3] — a uint24).
    pub fee: u32,
    /// The tick spacing (from data word 0 — an int24).
    pub tick_spacing: i32,
    /// The created pool address (from data word 1 — an address).
    pub pool_address: Address,
}

/// Decoded V4 `Initialize` event (the V4 `PoolCreated`). The pool is
/// identified by its `V4PoolId` (a bytes32, NOT a contract address — V4 pools
/// live inside the singleton `PoolManager`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4InitializeEvent {
    /// The `PoolManager` that emitted the event (the log emitter).
    pub pool_manager_address: Address,
    /// The pool ID (from topic[1] — a bytes32). Typed as the V4 [`V4PoolId`]
    /// alias so every V4 decoded event shares one `pool_id` type, matching
    /// [`crate::v4_swap_decoder::V4SwapEvent`] and
    /// [`crate::v4_modify_liquidity_decoder::V4ModifyLiquidityEvent`].
    pub pool_id: V4PoolId,
    /// Currency0 (from topic[2]).
    pub currency0: Address,
    /// Currency1 (from topic[3]).
    pub currency1: Address,
    /// The swap fee (from data word 0 — a uint24).
    pub fee: u32,
    /// The tick spacing (from data word 1 — an int24).
    pub tick_spacing: i32,
    /// The hooks contract address (from data word 2 — an address).
    pub hooks: Address,
}

// ── decoders ──────────────────────────────────────────────────────────────

/// Decode a V2 `PairCreated` event (the canonical Uniswap V2 family sig).
///
/// Returns `None` if the topic doesn't match `V2_PAIR_CREATED_TOPIC`, there
/// are fewer than 3 topics, or the data is too short (< 64 bytes).
#[must_use]
pub fn decode_v2_pair_created_log(log: &Log) -> Option<V2PairCreatedEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V2_PAIR_CREATED_TOPIC {
        return None;
    }
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }
    let token0 = Address::from_word(topics[1]);
    let token1 = Address::from_word(topics[2]);
    // data = abi.encode(address pair, uint totalSupply) = 2 × 32 bytes = 64.
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let pool_address = Address::from_slice(&data[12..32]);
    Some(V2PairCreatedEvent {
        factory_address: log.address(),
        token0,
        token1,
        pool_address,
    })
}

/// Decode an Aerodrome V2 `PoolCreated` event (the `stable`-flag variant).
///
/// Returns `None` if the topic doesn't match `AERODROME_V2_POOL_CREATED_TOPIC`,
/// there are fewer than 4 topics, or the data is too short (< 64 bytes).
#[must_use]
pub fn decode_aerodrome_v2_pool_created_log(log: &Log) -> Option<AerodromeV2PoolCreatedEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != AERODROME_V2_POOL_CREATED_TOPIC {
        return None;
    }
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let token0 = Address::from_word(topics[1]);
    let token1 = Address::from_word(topics[2]);
    // The stable flag is an indexed bool, sign-extended to uint256 in the
    // topic word. Non-zero word = true; zero word = false.
    let stable = !topics[3].is_zero();
    // data = abi.encode(address pool, uint256 type) = 2 × 32 bytes = 64.
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let pool_address = Address::from_slice(&data[12..32]);
    Some(AerodromeV2PoolCreatedEvent {
        factory_address: log.address(),
        token0,
        token1,
        stable,
        pool_address,
    })
}

/// Decode a V3 `PoolCreated` event.
///
/// Returns `None` if the topic doesn't match `V3_POOL_CREATED_TOPIC`, there
/// are fewer than 4 topics, the data is too short (< 64 bytes), or the
/// tick-spacing is outside the valid int24 range.
#[must_use]
pub fn decode_v3_pool_created_log(log: &Log) -> Option<V3PoolCreatedEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V3_POOL_CREATED_TOPIC {
        return None;
    }
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let token0 = Address::from_word(topics[1]);
    let token1 = Address::from_word(topics[2]);
    // fee = uint24 indexed as uint256 — last 3 bytes of the topic word.
    let fee_word = U256::from_be_bytes::<32>(topics[3].0);
    let fee: u32 = fee_word.to::<u32>() & 0x00FF_FFFF;
    // data = abi.encode(int24 tickSpacing, address pool) = 2 × 32 bytes = 64.
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let tick_spacing = extract_int24_from_word(&data[0..32])?;
    let pool_address = Address::from_slice(&data[44..64]);
    Some(V3PoolCreatedEvent {
        factory_address: log.address(),
        token0,
        token1,
        fee,
        tick_spacing,
        pool_address,
    })
}

/// Decode a V4 `Initialize` event (the V4 `PoolCreated`).
///
/// Returns `None` if the topic doesn't match `V4_INITIALIZE_TOPIC`, there are
/// fewer than 4 topics, the data is too short (< 96 bytes), or the
/// tick-spacing is outside the valid int24 range. Trailing data fields
/// (`sqrtPriceX96`, etc.) are NOT decoded — degenbot's `uniswap_v4_pools`
/// row only needs `(pool_id, currency0, currency1, fee, tick_spacing, hooks)`.
#[must_use]
pub fn decode_v4_initialize_log(log: &Log) -> Option<V4InitializeEvent> {
    let first_topic = log.topics().first()?;
    if *first_topic != V4_INITIALIZE_TOPIC {
        return None;
    }
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let pool_id: V4PoolId = topics[1].0;
    let currency0 = Address::from_word(topics[2]);
    let currency1 = Address::from_word(topics[3]);
    // data = abi.encode(uint24 fee, int24 tickSpacing, address hooks, ...)
    // ≥ 3 × 32 bytes = 96. degenbot reads only the first 96.
    let data = log.data().data.as_ref();
    if data.len() < 96 {
        return None;
    }
    let fee_word = U256::from_be_bytes::<32>(data[0..32].try_into().ok()?);
    let fee: u32 = fee_word.to::<u32>() & 0x00FF_FFFF;
    let tick_spacing = extract_int24_from_word(&data[32..64])?;
    let hooks = Address::from_slice(&data[76..96]);
    Some(V4InitializeEvent {
        pool_manager_address: log.address(),
        pool_id,
        currency0,
        currency1,
        fee,
        tick_spacing,
        hooks,
    })
}

#[cfg(test)]
#[expect(clippy::unreadable_literal, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog, I256};

    /// Build a minimal `alloy::rpc::types::Log` with the given emitter,
    /// topics, + data (mirrors the construction helper in
    /// `v3_mint_burn_decoder`'s tests).
    fn make_log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> Log {
        let inner = AlloyLog::new_unchecked(address, topics, Bytes::from(data));
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

    /// Left-pad an address to a 32-byte word.
    fn addr_word(addr: Address) -> B256 {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_slice());
        B256::new(word)
    }

    /// Encode an int24 tick as a sign-extended 32-byte int256 word.
    fn int24_word(tick: i32) -> B256 {
        let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        B256::from(i.to_be_bytes::<32>())
    }

    /// Encode a uint24 fee as a 32-byte word (left-zero-padded uint256).
    fn uint24_word(fee: u32) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&fee.to_be_bytes()[1..4]); // low 3 bytes of the u32
        word
    }

    // ── V2 PairCreated ──────────────────────────────────────────────────

    #[test]
    fn decode_valid_v2_pair_created() {
        let factory = Address::from([0xff; 20]);
        let token0 = Address::from([0x10; 20]);
        let token1 = Address::from([0x20; 20]);
        let pool = Address::from([0xab; 20]);

        // data = address pool (32) + uint totalSupply (32)
        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(pool.as_slice()); // pool_address
                                                       // totalSupply left as zero — doesn't matter for the decode.

        let log = make_log(
            factory,
            vec![V2_PAIR_CREATED_TOPIC, addr_word(token0), addr_word(token1)],
            data,
        );
        let event = decode_v2_pair_created_log(&log).unwrap();
        assert_eq!(event.factory_address, factory);
        assert_eq!(event.token0, token0);
        assert_eq!(event.token1, token1);
        assert_eq!(event.pool_address, pool);
    }

    #[test]
    fn v2_pair_created_wrong_topic_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 64],
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    #[test]
    fn v2_pair_created_truncated_data_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V2_PAIR_CREATED_TOPIC, B256::ZERO, B256::ZERO],
            vec![0u8; 32], // need 64
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    #[test]
    fn v2_pair_created_insufficient_topics_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V2_PAIR_CREATED_TOPIC, B256::ZERO], // need 3
            vec![0u8; 64],
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    // ── Aerodrome V2 PoolCreated ────────────────────────────────────────

    #[test]
    fn decode_valid_aerodrome_v2_pool_created_stable_true() {
        let factory = Address::from([0xfe; 20]);
        let token0 = Address::from([0x30; 20]);
        let token1 = Address::from([0x40; 20]);
        let pool = Address::from([0xcd; 20]);

        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(pool.as_slice());

        // stable=true → topic[3] = 1
        let mut stable_word = [0u8; 32];
        stable_word[31] = 1;
        let log = make_log(
            factory,
            vec![
                AERODROME_V2_POOL_CREATED_TOPIC,
                addr_word(token0),
                addr_word(token1),
                B256::new(stable_word),
            ],
            data,
        );
        let event = decode_aerodrome_v2_pool_created_log(&log).unwrap();
        assert_eq!(event.factory_address, factory);
        assert_eq!(event.token0, token0);
        assert_eq!(event.token1, token1);
        assert!(event.stable);
        assert_eq!(event.pool_address, pool);
    }

    #[test]
    fn decode_valid_aerodrome_v2_pool_created_stable_false() {
        // stable=false → topic[3] = 0 (zero word)
        let log = make_log(
            Address::from([0xfe; 20]),
            vec![
                AERODROME_V2_POOL_CREATED_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO, // stable=false
            ],
            vec![0u8; 64], // pool_address = zero (doesn't matter here)
        );
        let event = decode_aerodrome_v2_pool_created_log(&log).unwrap();
        assert!(!event.stable);
    }

    #[test]
    fn aerodrome_v2_pool_created_truncated_data_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![
                AERODROME_V2_POOL_CREATED_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 32], // need 64
        );
        assert!(decode_aerodrome_v2_pool_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_topic_not_matched_by_canonical_v2_decoder() {
        // The Aerodrome topic must not be decoded by the canonical V2 decoder.
        let log = make_log(
            Address::ZERO,
            vec![
                AERODROME_V2_POOL_CREATED_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 64],
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    // ── V3 PoolCreated ──────────────────────────────────────────────────

    #[test]
    fn decode_valid_v3_pool_created() {
        let factory = Address::from([0xfa; 20]);
        let token0 = Address::from([0x11; 20]);
        let token1 = Address::from([0x22; 20]);
        let pool = Address::from([0x99; 20]);
        let fee: u32 = 3000;
        let tick_spacing: i32 = 60;

        let mut data = vec![0u8; 64];
        // word 0 = int24 tickSpacing (sign-extended to int256)
        data[0..32].copy_from_slice(&int24_word(tick_spacing).0);
        // word 1 = address pool
        data[12 + 32..32 + 32].copy_from_slice(pool.as_slice());

        let log = make_log(
            factory,
            vec![
                V3_POOL_CREATED_TOPIC,
                addr_word(token0),
                addr_word(token1),
                {
                    let mut word = [0u8; 32];
                    word[29..32].copy_from_slice(&fee.to_be_bytes()[1..4]); // low 3 bytes
                    B256::new(word)
                },
            ],
            data,
        );
        let event = decode_v3_pool_created_log(&log).unwrap();
        assert_eq!(event.factory_address, factory);
        assert_eq!(event.token0, token0);
        assert_eq!(event.token1, token1);
        assert_eq!(event.fee, fee);
        assert_eq!(event.tick_spacing, tick_spacing);
        assert_eq!(event.pool_address, pool);
    }

    #[test]
    fn v3_pool_created_negative_tick_spacing() {
        // tick_spacing is signed (int24); some chains use negatives? No —
        // Uniswap V3 tick spacings are always positive, but the wire format
        // is int24, so a negative must decode correctly. Use -60.
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&int24_word(-60).0);
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );
        let event = decode_v3_pool_created_log(&log).unwrap();
        assert_eq!(event.tick_spacing, -60);
    }

    #[test]
    fn v3_pool_created_tick_out_of_range_returns_none() {
        let mut data = vec![0u8; 64];
        // -887273 is out of int24 range
        data[0..32].copy_from_slice(&int24_word(-887273).0);
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_truncated_data_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 32], // need 64
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_insufficient_topics_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO], // need 4
            vec![0u8; 64],
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    // ── V4 Initialize ───────────────────────────────────────────────────

    #[test]
    fn decode_valid_v4_initialize() {
        let pool_manager = Address::from([0xfc; 20]);
        let currency0 = Address::from([0x55; 20]);
        let currency1 = Address::from([0x66; 20]);
        let hooks = Address::from([0x77; 20]);
        let fee: u32 = 500;
        let tick_spacing: i32 = 10;
        let pool_id = B256::repeat_byte(0x42);

        // data = uint24 fee (32) + int24 tickSpacing (32) + address hooks (32) + trailing
        let mut data = vec![0u8; 128];
        data[0..32].copy_from_slice(&uint24_word(fee));
        data[32..64].copy_from_slice(&int24_word(tick_spacing).0);
        data[64 + 12..64 + 32].copy_from_slice(hooks.as_slice()); // hooks address (3rd word)

        let log = make_log(
            pool_manager,
            vec![
                V4_INITIALIZE_TOPIC,
                pool_id,
                addr_word(currency0),
                addr_word(currency1),
            ],
            data,
        );
        let event = decode_v4_initialize_log(&log).unwrap();
        assert_eq!(event.pool_manager_address, pool_manager);
        assert_eq!(B256::from(event.pool_id), pool_id);
        assert_eq!(event.currency0, currency0);
        assert_eq!(event.currency1, currency1);
        assert_eq!(event.fee, fee);
        assert_eq!(event.tick_spacing, tick_spacing);
        assert_eq!(event.hooks, hooks);
    }

    #[test]
    fn v4_initialize_zero_hooks() {
        // Hooks address = 0 (no hooks) — a legitimate Initialize event.
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(3000));
        data[32..64].copy_from_slice(&int24_word(60).0);
        // hooks left as zero
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );
        let event = decode_v4_initialize_log(&log).unwrap();
        assert_eq!(event.hooks, Address::ZERO);
    }

    #[test]
    fn v4_initialize_truncated_data_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 64], // need 96
        );
        assert!(decode_v4_initialize_log(&log).is_none());
    }

    #[test]
    fn v4_initialize_insufficient_topics_returns_none() {
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO], // need 4
            vec![0u8; 96],
        );
        assert!(decode_v4_initialize_log(&log).is_none());
    }

    // ── cross-family topic discrimination ──────────────────────────────

    #[test]
    fn v3_topic_not_matched_by_v2_decoder() {
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 64],
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    #[test]
    fn v4_topic_not_matched_by_v3_decoder() {
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 96],
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }
}
