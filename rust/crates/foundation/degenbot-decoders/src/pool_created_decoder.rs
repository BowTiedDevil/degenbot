//! Uniswap V2/V3/V4 `PoolCreated` event decoders.
//!
//! The chunk-loop fetcher  decodes `PoolCreated` events from
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
//! Event signatures + topic constants are the keccak256 of the canonical
//! event signature each family's factory declares.
//!
//! Every decoder but V4 `Initialize` reads its log through an Alloy event
//! declaration, and a declaration binds the topic count exactly: too few topics
//! and a surplus topic are both decode failures. V4 `Initialize` is the one
//! deliberate exception — degenbot keeps a *prefix* of that event, so it
//! requires the leading topics and data words and ignores whatever follows.
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
//! # Aerodrome V3 (CL) `PoolCreated`
//!
//! ```text
//! event PoolCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     int24 indexed tickSpacing,
//!     address pool
//! )
//!
//! topic[0] = 0xab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2
//! topic[1] = token0 (indexed address)
//! topic[2] = token1 (indexed address)
//! topic[3] = tickSpacing (indexed int24)
//! data    = abi.encode(address pool)
//!         = 32 bytes
//! ```
//!
//! The CL factory announces no fee: a CL pool's authoritative fee is the
//! factory's `getSwapFee(pool)` answer at read time, and it stays mutable
//! afterwards.
//!
//! # V4 `Initialize` (the V4 `PoolCreated`)
//!
//! ```text
//! event Initialize(
//!     V4PoolId indexed id,
//! address indexed currency0,
//!     address indexed currency1,
//!     uint24 fee,
//!     int24 tickSpacing,
//!     address hooks,
//!     uint160 sqrtPriceX96,
//!     int24 tick
//! )
//!
//! topic[0] = 0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438
//! topic[1] = V4PoolId (indexed bytes32)
//! topic[2] = currency0 (indexed address)
//! topic[3] = currency1 (indexed address)
//! data    = abi.encode(
//!     uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96, int24 tick
//! ) = 5 × 32 bytes = 160 bytes
//!
//! degenbot reads only the leading three words (`fee`, `tickSpacing`, `hooks`).
//! ```

use alloy::primitives::{
    aliases::{I24, U24},
    b256, Address, B256,
};
use alloy::rpc::types::Log;
use alloy::sol_types::{SolEvent, SolValue};

use crate::uniswap_tick_range::validate_tick;
use crate::v4_swap_decoder::V4PoolId;

// Three of the family declarations below name their event `PoolCreated` with a
// different parameter list, so each family's declaration lives in its own
// module: Alloy derives the topic hash from the declared event name and
// parameter types, which is exactly what tells them apart on the wire.

/// The V2 factory's pool-creation event (the canonical Uniswap V2 family sig —
/// `Uniswap`, `PancakeSwap`, `SushiSwap`, Swapbased V2).
mod pair_created_event {
    alloy::sol! {
        event PairCreated(
            address indexed token0,
            address indexed token1,
            address pair,
            uint256 totalSupply,
        );
    }
}

/// Aerodrome V2's pool-creation event — the V2 shape with a `stable` flag
/// indexed into topic[3].
mod aerodrome_v2_pool_created_event {
    alloy::sol! {
        event PoolCreated(
            address indexed token0,
            address indexed token1,
            bool indexed stable,
            address pool,
            uint256 poolType,
        );
    }
}

/// The V3 factory's pool-creation event (`PancakeSwap V3` and `SushiSwap V3`
/// share it).
mod v3_pool_created_event {
    alloy::sol! {
        event PoolCreated(
            address indexed token0,
            address indexed token1,
            uint24 indexed fee,
            int24 tickSpacing,
            address pool,
        );
    }
}

/// Aerodrome V3 (CL) factory's pool-creation event — the canonical V3 shape with
/// the indexed fee tier replaced by the indexed tick spacing.
mod aerodrome_v3_pool_created_event {
    alloy::sol! {
        event PoolCreated(
            address indexed token0,
            address indexed token1,
            int24 indexed tickSpacing,
            address pool,
        );
    }
}

// ── topic constants ───────────────────────────────────────────────────────

/// Keccak256 of `PairCreated(address,address,address,uint256)` — the
/// Uniswap V2 family sig (shared by `Uniswap`, `PancakeSwap`, `SushiSwap`,
/// Swapbased V2 — the forks whose factories emit the same event).
pub const V2_PAIR_CREATED_TOPIC: B256 =
    b256!("0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9");

/// Keccak256 of `PoolCreated(address,address,bool,address,uint256)` — the
/// Aerodrome V2 family sig (the V2 variant with a `stable` indexed flag).
pub const AERODROME_V2_POOL_CREATED_TOPIC: B256 =
    b256!("0x2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e");

/// Keccak256 of `PoolCreated(address,address,int24,address)` — the Aerodrome
/// V3 (CL) factory sig. Distinct from the canonical Uniswap V3 sig
/// ([`V3_POOL_CREATED_TOPIC`]): the indexed `int24 tickSpacing` sits where the
/// canonical event indexes its `uint24 fee`, and the pool address is the only
/// data word. The event carries no fee — a CL pool's fee is the factory's
/// `getSwapFee(pool)` answer at read time, mutable afterwards.
pub const AERODROME_V3_POOL_CREATED_TOPIC: B256 =
    b256!("0xab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2");

/// Keccak256 of `PoolCreated(address,address,uint24,int24,address)` — the
/// Uniswap V3 family sig (`PancakeSwap V3` + `SushiSwap V3` share it).
pub const V3_POOL_CREATED_TOPIC: B256 =
    b256!("0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118");

/// Keccak256 of `Initialize(bytes32,address,address,uint24,int24,address,...)`
/// — the Uniswap V4 `PoolCreated` sig (degenbot treats V4 `Initialize` as the
/// pool-creation event, per the Python `UNISWAP_V4_POOLCREATED_EVENT_HASH`).
pub const V4_INITIALIZE_TOPIC: B256 =
    b256!("0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438");

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

/// Decoded Aerodrome V3 (CL) `PoolCreated` event. The pool address is the
/// single data word; `tick_spacing` rides in topic[3], where the canonical V3
/// event indexes its fee tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AerodromeV3PoolCreatedEvent {
    /// The factory that emitted the event (the log emitter).
    pub factory_address: Address,
    /// Token0 (from topic[1]).
    pub token0: Address,
    /// Token1 (from topic[2]).
    pub token1: Address,
    /// The tick spacing (from topic[3] — an int24).
    pub tick_spacing: i32,
    /// The created pool address (from data word 0).
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
/// Returns `None` if the log's topics are not exactly the declared three or the
/// first one doesn't match `V2_PAIR_CREATED_TOPIC` (the declaration binds the
/// topic count, so a surplus topic is a refusal), the data is too short
/// (< 64 bytes), or any address word is not a canonical `address` slot.
#[must_use]
pub fn decode_v2_pair_created_log(log: &Log) -> Option<V2PairCreatedEvent> {
    // The declaration is the bound: topic0, both indexed address words, and the
    // two data words are decoded VALIDATING, so a `token0`/`token1` topic or a
    // `pair` data word with a non-zero 12-byte prefix is a decode failure
    // instead of a truncated address.
    let log = pair_created_event::PairCreated::decode_log_validate(&log.inner).ok()?;
    Some(V2PairCreatedEvent {
        factory_address: log.address,
        token0: log.data.token0,
        token1: log.data.token1,
        pool_address: log.data.pair,
    })
}

/// Decode an Aerodrome V2 `PoolCreated` event (the `stable`-flag variant).
///
/// Returns `None` if the log's topics are not exactly the declared four or the
/// first one doesn't match `AERODROME_V2_POOL_CREATED_TOPIC` (the declaration
/// binds the topic count, so a surplus topic is a refusal), the data is too
/// short (< 64 bytes), the indexed `stable` word is not a canonical `bool`, or
/// any address word is not a canonical `address` slot.
#[must_use]
pub fn decode_aerodrome_v2_pool_created_log(log: &Log) -> Option<AerodromeV2PoolCreatedEvent> {
    // The declaration is the bound here too, plus the fork's indexed `bool`: a
    // `stable` word other than 0 or 1 is a decode failure rather than
    // "non-zero means true".
    let log = aerodrome_v2_pool_created_event::PoolCreated::decode_log_validate(&log.inner).ok()?;
    Some(AerodromeV2PoolCreatedEvent {
        factory_address: log.address,
        token0: log.data.token0,
        token1: log.data.token1,
        stable: log.data.stable,
        pool_address: log.data.pool,
    })
}

/// Decode a V3 `PoolCreated` event.
///
/// Returns `None` if the log's topics are not exactly the declared four or the
/// first one doesn't match `V3_POOL_CREATED_TOPIC` (the declaration binds the
/// topic count, so a surplus topic is a refusal), the data is too short
/// (< 64 bytes), an address/tick word is not canonical for its declared width
/// (including the indexed `uint24` fee), or the tick-spacing is outside the
/// Uniswap V3 tick range.
#[must_use]
pub fn decode_v3_pool_created_log(log: &Log) -> Option<V3PoolCreatedEvent> {
    // The declaration is the bound, and it is what makes the indexed `uint24`
    // fee safe: an over-wide fee topic is a decode failure instead of a masked
    // low-24-bits read.
    let log = v3_pool_created_event::PoolCreated::decode_log_validate(&log.inner).ok()?;
    Some(V3PoolCreatedEvent {
        factory_address: log.address,
        token0: log.data.token0,
        token1: log.data.token1,
        // A validated `uint24` fee widens into the event struct's `u32`
        // without a failure path.
        fee: log.data.fee.to::<u32>(),
        tick_spacing: validate_tick(log.data.tickSpacing)?,
        pool_address: log.data.pool,
    })
}

/// Decode an Aerodrome V3 (CL) `PoolCreated` event.
///
/// The declaration binds topic0 to the CL signature, so a canonical V3 log's
/// indexed `uint24` fee can never arrive as a tick spacing here.
///
/// Returns `None` if the log's topics are not exactly the declared four or the
/// first one doesn't match `AERODROME_V3_POOL_CREATED_TOPIC` (the declaration
/// binds the topic count, so a surplus topic is a refusal), the data is shorter
/// than its single address word, the indexed `tickSpacing` is not a
/// sign-extended `int24`, the pool word is not a canonical `address` slot, or
/// the tick spacing is outside the Uniswap V3 tick range.
#[must_use]
pub fn decode_aerodrome_v3_pool_created_log(log: &Log) -> Option<AerodromeV3PoolCreatedEvent> {
    let log = aerodrome_v3_pool_created_event::PoolCreated::decode_log_validate(&log.inner).ok()?;
    Some(AerodromeV3PoolCreatedEvent {
        factory_address: log.address,
        token0: log.data.token0,
        token1: log.data.token1,
        tick_spacing: validate_tick(log.data.tickSpacing)?,
        pool_address: log.data.pool,
    })
}

/// Decode a V4 `Initialize` event (the V4 `PoolCreated`).
///
/// This is the one decoder that reads a *prefix* of its event rather than the
/// whole declaration, and it therefore tolerates a surplus topic or trailing
/// data word that the `sol!` decoders refuse: it requires the four leading
/// topics and the three-word data prefix degenbot consumes, and ignores
/// whatever the event carries past them.
///
/// Returns `None` if the first topic doesn't match `V4_INITIALIZE_TOPIC`, there
/// are fewer than 4 topics, the data is shorter than the 96-byte prefix, an
/// indexed or kept data word is not canonical for its declared width (`uint24`
/// fee, sign-extended `int24` tickSpacing, `address` hooks), or the
/// tick-spacing is outside the Uniswap V3 tick range. The declared tail —
/// `sqrtPriceX96` and `tick` — is deliberately not decoded, since degenbot's
/// `uniswap_v4_pools` row only needs
/// `(pool_id, currency0, currency1, fee, tick_spacing, hooks)`.
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
    let currency0 = <Address as SolValue>::abi_decode_validate(topics[2].as_slice()).ok()?;
    let currency1 = <Address as SolValue>::abi_decode_validate(topics[3].as_slice()).ok()?;
    // The event declares five data words and degenbot keeps the leading three,
    // so the kept prefix is declared as its own tuple rather than as the whole
    // event: Alloy's validating decode binds `fee` to `uint24`, `tickSpacing` to
    // a sign-extended `int24`, and `hooks` to `address`, and the `sqrtPriceX96`
    // and `tick` words past `hooks` are left unread.
    let (fee, tick_spacing, hooks) =
        <(U24, I24, Address) as SolValue>::abi_decode_validate(log.data().data.as_ref()).ok()?;
    Some(V4InitializeEvent {
        pool_manager_address: log.address(),
        pool_id,
        currency0,
        currency1,
        fee: fee.to::<u32>(),
        tick_spacing: validate_tick(tick_spacing)?,
        hooks,
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unreadable_literal, clippy::unwrap_used)]
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

    // ── Aerodrome V3 (CL) PoolCreated ───────────────────────────────────

    /// A CL `PoolCreated` log: `token0`/`token1`/`tickSpacing` indexed, the pool
    /// address the single data word.
    fn aerodrome_v3_pool_created_log(tick_spacing: i32, pool: Address, data_len: usize) -> Log {
        let mut data = vec![0u8; data_len];
        if data_len >= 32 {
            data[12..32].copy_from_slice(pool.as_slice());
        }
        make_log(
            Address::from([0x5e; 20]),
            vec![
                AERODROME_V3_POOL_CREATED_TOPIC,
                addr_word(Address::from([0x11; 20])),
                addr_word(Address::from([0x22; 20])),
                int24_word(tick_spacing),
            ],
            data,
        )
    }

    #[test]
    fn decode_aerodrome_v3_pool_created_reads_the_tick_spacing_topic_and_pool_data() {
        let pool = Address::from([0x99; 20]);
        let log = aerodrome_v3_pool_created_log(200, pool, 32);

        let event = decode_aerodrome_v3_pool_created_log(&log).expect("the CL log decodes");
        assert_eq!(event.factory_address, Address::from([0x5e; 20]));
        assert_eq!(event.token0, Address::from([0x11; 20]));
        assert_eq!(event.token1, Address::from([0x22; 20]));
        assert_eq!(event.tick_spacing, 200);
        assert_eq!(event.pool_address, pool);
    }

    #[test]
    fn aerodrome_v3_pool_created_decodes_a_negative_tick_spacing() {
        let log = aerodrome_v3_pool_created_log(-60, Address::from([0x99; 20]), 32);
        let event = decode_aerodrome_v3_pool_created_log(&log).unwrap();
        assert_eq!(event.tick_spacing, -60);
    }

    #[test]
    fn aerodrome_v3_pool_created_rejects_truncated_data() {
        let log = aerodrome_v3_pool_created_log(200, Address::from([0x99; 20]), 16);
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_v3_pool_created_rejects_a_surplus_topic() {
        let mut log = aerodrome_v3_pool_created_log(200, Address::from([0x99; 20]), 32);
        let mut topics = log.topics().to_vec();
        topics.push(B256::repeat_byte(0x01));
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_v3_pool_created_rejects_a_non_sign_extended_tick_spacing() {
        // The zero-extended form of -60 is not a canonical int24 slot; a
        // fixed-width read would have reported -60 as if the log were sound.
        let mut log = aerodrome_v3_pool_created_log(-60, Address::from([0x99; 20]), 32);
        let mut topics = log.topics().to_vec();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        topics[3] = B256::new(word);
        log.inner =
            alloy::primitives::Log::new_unchecked(log.address(), topics, log.data().data.clone());
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_v3_pool_created_rejects_a_dirty_pool_address() {
        let mut log = aerodrome_v3_pool_created_log(200, Address::from([0x99; 20]), 32);
        let mut data = log.data().data.to_vec();
        data[0] = 0x01;
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_v3_pool_created_rejects_a_tick_spacing_outside_the_protocol_range() {
        let log = aerodrome_v3_pool_created_log(887_273, Address::from([0x99; 20]), 32);
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
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

    // ── malformed narrow-field words ──────────────────────────────────────

    /// A V3 `PoolCreated` log carrying `fee` in topic[3] and
    /// `tickSpacing`/`pool` in the two data words.
    fn v3_pool_created_log(fee: u32, tick_spacing: i32, pool: Address) -> Log {
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&int24_word(tick_spacing).0);
        data[44..64].copy_from_slice(pool.as_slice());

        make_log(
            Address::from([0xfa; 20]),
            vec![
                V3_POOL_CREATED_TOPIC,
                addr_word(Address::from([0x11; 20])),
                addr_word(Address::from([0x22; 20])),
                B256::new(uint24_word(fee)),
            ],
            data,
        )
    }

    #[test]
    fn v2_pair_created_rejects_dirty_pool_address() {
        // The non-indexed `pair` data word is an `address` slot.
        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(Address::from([0xab; 20]).as_slice());
        data[0] = 0x01;
        let log = make_log(
            Address::ZERO,
            vec![V2_PAIR_CREATED_TOPIC, B256::ZERO, B256::ZERO],
            data,
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    #[test]
    fn v2_pair_created_rejects_dirty_token_topic() {
        let mut token0_word = [0u8; 32];
        token0_word[12..32].copy_from_slice(Address::from([0x10; 20]).as_slice());
        token0_word[0] = 0x01;
        let log = make_log(
            Address::ZERO,
            vec![
                V2_PAIR_CREATED_TOPIC,
                B256::new(token0_word),
                addr_word(Address::from([0x20; 20])),
            ],
            vec![0u8; 64],
        );
        assert!(decode_v2_pair_created_log(&log).is_none());
    }

    #[test]
    fn aerodrome_pool_created_rejects_non_bool_stable() {
        // An indexed `bool` is 0 or 1; a 2 word is malformed, not `true`.
        let mut stable_word = [0u8; 32];
        stable_word[31] = 2;
        let log = make_log(
            Address::ZERO,
            vec![
                AERODROME_V2_POOL_CREATED_TOPIC,
                addr_word(Address::from([0x30; 20])),
                addr_word(Address::from([0x40; 20])),
                B256::new(stable_word),
            ],
            vec![0u8; 64],
        );
        assert!(decode_aerodrome_v2_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_rejects_over_wide_fee_topic() {
        // An indexed `uint24` fee whose topic word carries a set bit above 24 is
        // malformed; masking the low three bytes would have reported 3000.
        let mut fee_word = uint24_word(3000);
        fee_word[28] = 0x01;
        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&int24_word(60).0);
        data[44..64].copy_from_slice(Address::from([0x99; 20]).as_slice());
        let log = make_log(
            Address::ZERO,
            vec![
                V3_POOL_CREATED_TOPIC,
                addr_word(Address::from([0x11; 20])),
                addr_word(Address::from([0x22; 20])),
                B256::new(fee_word),
            ],
            data,
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_rejects_non_sign_extended_tick_spacing() {
        let mut log = v3_pool_created_log(3000, -60, Address::from([0x99; 20]));
        let mut data = log.data().data.to_vec();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        data[..32].copy_from_slice(&word);
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_rejects_dirty_pool_address() {
        let mut log = v3_pool_created_log(3000, 60, Address::from([0x99; 20]));
        let mut data = log.data().data.to_vec();
        data[32] = 0x01;
        log.inner = alloy::primitives::Log::new_unchecked(
            log.address(),
            log.topics().to_vec(),
            Bytes::from(data),
        );
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_pool_created_accepts_uint24_max_fee() {
        let pool = Address::from([0x99; 20]);
        let log = v3_pool_created_log(0x00FF_FFFF, 60, pool);

        let event = decode_v3_pool_created_log(&log).expect("uint24::MAX fee must decode cleanly");
        assert_eq!(event.fee, 0x00FF_FFFF);
        assert_eq!(event.pool_address, pool);
    }

    #[test]
    fn v4_initialize_rejects_over_wide_fee() {
        // The `uint24` fee data word's bits above 24 are not a fee tier; masking
        // would have reported 500.
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(500));
        data[0] = 0x01;
        data[32..64].copy_from_slice(&int24_word(10).0);
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );
        assert!(decode_v4_initialize_log(&log).is_none());
    }

    #[test]
    fn v4_initialize_rejects_dirty_hooks_address() {
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(500));
        data[32..64].copy_from_slice(&int24_word(10).0);
        data[64..96].copy_from_slice(addr_word(Address::from([0x77; 20])).as_slice());
        data[64] = 0x01;
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );
        assert!(decode_v4_initialize_log(&log).is_none());
    }

    #[test]
    fn v4_initialize_rejects_dirty_currency_topic() {
        let mut currency0_word = [0u8; 32];
        currency0_word[12..32].copy_from_slice(Address::from([0x55; 20]).as_slice());
        currency0_word[0] = 0x01;
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(500));
        data[32..64].copy_from_slice(&int24_word(10).0);
        let log = make_log(
            Address::ZERO,
            vec![
                V4_INITIALIZE_TOPIC,
                B256::ZERO,
                B256::new(currency0_word),
                addr_word(Address::from([0x66; 20])),
            ],
            data,
        );
        assert!(decode_v4_initialize_log(&log).is_none());
    }

    #[test]
    fn v4_initialize_decodes_five_word_body_and_ignores_dirty_price_tail() {
        // The live `Initialize` body is five words / 160 bytes: `fee`,
        // `tickSpacing`, `hooks`, `sqrtPriceX96`, `tick`. The decoder keeps the
        // leading three and never reads the price/tick tail.
        let hooks = Address::from([0x77; 20]);
        let mut data = vec![0u8; 160];
        data[0..32].copy_from_slice(&uint24_word(3000));
        data[32..64].copy_from_slice(&int24_word(-60).0);
        data[64..96].copy_from_slice(addr_word(hooks).as_slice());
        // `sqrtPriceX96`: deliberately not a canonical `uint160` (every bit
        // set), so a decoder that reached into the tail would have to refuse
        // this log. The prefix decode must carry on.
        data[96..128].copy_from_slice(&[0xff; 32]);
        // `tick`: junk for the same reason.
        data[128..160].copy_from_slice(&[0xff; 32]);

        let log = make_log(
            Address::from([0xfc; 20]),
            vec![
                V4_INITIALIZE_TOPIC,
                B256::ZERO,
                addr_word(Address::from([0x55; 20])),
                addr_word(Address::from([0x66; 20])),
            ],
            data,
        );

        let event = decode_v4_initialize_log(&log).expect("the five-word body must decode");
        assert_eq!(event.fee, 3000);
        assert_eq!(event.tick_spacing, -60);
        assert_eq!(event.hooks, hooks);
    }

    #[test]
    fn v4_initialize_rejects_non_sign_extended_tick_spacing() {
        // `tickSpacing` is an `int24` sign-extended to the 32-byte slot; the
        // zero-extended -60 below is malformed, and a fixed-width tail read
        // would have reported -60 as if the log were canonical.
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(3000));
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        data[32..64].copy_from_slice(&word);
        let log = make_log(
            Address::ZERO,
            vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            data,
        );

        assert!(decode_v4_initialize_log(&log).is_none());
    }

    #[test]
    fn v4_initialize_tick_spacing_at_protocol_bounds() {
        // The decoder applies the Uniswap tick range to `tickSpacing`: both
        // bounds decode with the bound intact, and one step past either end is
        // refused.
        for tick_spacing in [-887_272, 887_272] {
            let mut data = vec![0u8; 96];
            data[0..32].copy_from_slice(&uint24_word(3000));
            data[32..64].copy_from_slice(&int24_word(tick_spacing).0);
            let log = make_log(
                Address::ZERO,
                vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
                data,
            );

            let event = decode_v4_initialize_log(&log);
            assert!(event.is_some(), "tick spacing {tick_spacing} must decode");
            assert_eq!(event.map(|e| e.tick_spacing), Some(tick_spacing));
        }

        for tick_spacing in [-887_273, 887_273] {
            let mut data = vec![0u8; 96];
            data[0..32].copy_from_slice(&uint24_word(3000));
            data[32..64].copy_from_slice(&int24_word(tick_spacing).0);
            let log = make_log(
                Address::ZERO,
                vec![V4_INITIALIZE_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
                data,
            );

            assert!(
                decode_v4_initialize_log(&log).is_none(),
                "tick spacing {tick_spacing} is outside the protocol range"
            );
        }
    }

    #[test]
    fn v4_initialize_tolerates_surplus_topic() {
        // The prefix policy: `Initialize` keeps its leading four topics and
        // three data words, so an extra topic is ignored here — unlike the
        // `sol!` decoders, whose declaration binds the topic count exactly.
        let mut data = vec![0u8; 96];
        data[0..32].copy_from_slice(&uint24_word(500));
        data[32..64].copy_from_slice(&int24_word(10).0);
        let log = make_log(
            Address::ZERO,
            vec![
                V4_INITIALIZE_TOPIC,
                B256::ZERO,
                B256::ZERO,
                B256::ZERO,
                B256::repeat_byte(0x01),
            ],
            data,
        );

        let event = decode_v4_initialize_log(&log).expect("surplus topic must not fail the decode");
        assert_eq!(event.fee, 500);
        assert_eq!(event.tick_spacing, 10);
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
    fn aerodrome_v3_topic_not_matched_by_v3_decoder() {
        // The CL event has the same topic count and word count as the canonical
        // V3 event: only the declared topic0 tells the fee tier apart from the
        // tick spacing, so the canonical leaf must refuse the CL topic.
        let log = aerodrome_v3_pool_created_log(200, Address::from([0x99; 20]), 32);
        assert!(decode_v3_pool_created_log(&log).is_none());
    }

    #[test]
    fn v3_topic_not_matched_by_aerodrome_v3_decoder() {
        let log = v3_pool_created_log(3000, 60, Address::from([0x99; 20]));
        assert!(decode_aerodrome_v3_pool_created_log(&log).is_none());
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
