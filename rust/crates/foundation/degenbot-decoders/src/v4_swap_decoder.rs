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
//!
//! # `PoolManager.swap` calldata
//!
//! [`decode_v4_pool_manager_swap_pool_id`] reads the pool identity out of a
//! `swap` CALL rather than a log: `swap(PoolKey,SwapParams,bytes)`, whose
//! five-word `PoolKey` head is precisely the `abi.encode(PoolKey)` preimage, so
//! `keccak256` over those words is the same `V4PoolId` the event's `topic[1]`
//! carries. Reverting-frame attribution needs it because every V4 pool lives
//! inside one shared `PoolManager`: a failing frame's target address names the
//! manager, and only the calldata names the pool.

use crate::uniswap_tick_range::validate_tick;
use alloy::primitives::{b256, keccak256, Address, B256, I256, U128, U256};
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

// ── `PoolManager.swap` calldata ───────────────────────────────────────────

/// The `PoolManager.swap` selector —
/// `swap((address,address,uint24,int24,address),(bool,int256,uint160),bytes)`.
pub const V4_POOL_MANAGER_SWAP_SELECTOR: [u8; 4] = [0xf3, 0xcd, 0x91, 0x4c];

/// The static head of a canonical `swap` call: the five-word `PoolKey`, the
/// three-word `SwapParams`, and the `bytes hookData` offset word.
const V4_SWAP_HEAD_BYTES: usize = 9 * 32;

/// The width of `abi.encode(PoolKey)` — five static words, which are exactly
/// the call's first five head words.
const V4_POOL_KEY_BYTES: usize = 5 * 32;

/// Decode the pool identity a canonical `PoolManager.swap` call names: the
/// `poolId` = `keccak256(abi.encode(PoolKey))` the `PoolManager` indexes the
/// pool under, and the key [`V4SwapEvent::pool_id`] carries — the call's first
/// five head words ARE the `abi.encode(PoolKey)` preimage.
///
/// Returns `None` unless the calldata is a canonical `swap` call:
///
/// - The selector is [`V4_POOL_MANAGER_SWAP_SELECTOR`]. Another `PoolManager`
///   entry point (`unlock`, `settle`, `take`, …) carries no `PoolKey`, so the
///   five words in its argument list are some other call's numbers.
/// - The nine-word static head is present and every word is canonical for its
///   declared width (`address` / `uint160` / `uint24` zero-padded above their
///   width, `int24` sign-extended, `bool` exactly `0` or `1`). A dirty word is
///   refused instead of masked into a key the pool never had.
/// - The `hookData` offset is the canonical head offset and its tail is exactly
///   the declared length, so a truncated call or a surplus trailing byte cannot
///   ride along as an attributable swap.
///
/// The identity is the attribution seam: every V4 pool lives inside one shared
/// `PoolManager`, so the failing frame's target address names the manager, not
/// the pool. This is what names the pool.
#[must_use]
pub fn decode_v4_pool_manager_swap_pool_id(calldata: &[u8]) -> Option<V4PoolId> {
    let body = calldata.strip_prefix(V4_POOL_MANAGER_SWAP_SELECTOR.as_slice())?;
    let head = body.get(..V4_SWAP_HEAD_BYTES)?;
    let pool_key = head.get(..V4_POOL_KEY_BYTES)?;
    let (words, _) = head.as_chunks::<32>();
    let word = |index: usize| -> Option<&[u8]> { words.get(index).map(<[u8; 32]>::as_slice) };

    // `PoolKey`: Currency (address), Currency (address), uint24 fee,
    // int24 tickSpacing, IHooks (address).
    if !is_canonical_address(word(0)?)
        || !is_canonical_address(word(1)?)
        || !is_canonical_uint(word(2)?, 3)
        || !is_canonical_int24(word(3)?)
        || !is_canonical_address(word(4)?)
    {
        return None;
    }

    // `SwapParams`: bool zeroForOne, int256 amountSpecified, uint160
    // sqrtPriceLimitX96. Every `int256` word is canonical at its full width, so
    // the amount needs no check; the other two do.
    if !is_canonical_bool(word(5)?) || !is_canonical_uint(word(7)?, 20) {
        return None;
    }

    // `bytes hookData`: an offset naming the head's end, then a length word and
    // that many padded bytes.
    if U256::from_be_slice(word(8)?) != U256::from(V4_SWAP_HEAD_BYTES) {
        return None;
    }
    let tail = body.get(V4_SWAP_HEAD_BYTES..)?;
    let declared = usize::try_from(U256::from_be_slice(tail.get(..32)?)).ok()?;
    if tail.len() != 32 + declared.checked_next_multiple_of(32)? {
        return None;
    }

    Some(keccak256(pool_key).0)
}

/// Whether `word` is the canonical ABI word of an `address`: 32 bytes whose
/// high 12 are zero.
fn is_canonical_address(word: &[u8]) -> bool {
    word.get(..12)
        .is_some_and(|high| high.iter().all(|byte| *byte == 0))
}

/// Whether `word` is the canonical ABI word of an unsigned integer
/// `width_bytes` wide — zero above the value's width.
fn is_canonical_uint(word: &[u8], width_bytes: usize) -> bool {
    word.get(..32_usize.saturating_sub(width_bytes))
        .is_some_and(|high| high.iter().all(|byte| *byte == 0))
}

/// Whether `word` is the canonical ABI word of an `int24`: sign-extended, so
/// every byte above the value's three bytes repeats its sign bit.
fn is_canonical_int24(word: &[u8]) -> bool {
    let Some(low) = word.get(29) else {
        return false;
    };
    let fill = if low & 0x80 == 0 { 0x00 } else { 0xff };
    word.get(..29)
        .is_some_and(|high| high.iter().all(|byte| *byte == fill))
}

/// Whether `word` is the canonical ABI word of a `bool`: zero except for a low
/// byte that is exactly `0` or `1`.
fn is_canonical_bool(word: &[u8]) -> bool {
    word.get(..31)
        .is_some_and(|high| high.iter().all(|byte| *byte == 0))
        && word.get(31).is_some_and(|low| *low <= 1)
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

    // ── `PoolManager.swap` calldata → poolId ──────────────────────────────

    /// `cast calldata 'swap((address,address,uint24,int24,address),(bool,int256,uint160),bytes)'`
    /// with the ETH/USDC 0.05 % key, `(true,-1000,2^96)`, empty hook data — a
    /// canonical `swap` call produced by an independent encoder (foundry), so
    /// the decoder is pinned against the real ABI layout instead of its own
    /// encoder.
    const CANONICAL_SWAP_CALLDATA: &str = "0xf3cd914c\
        0000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48\
        00000000000000000000000000000000000000000000000000000000000001f4\
        000000000000000000000000000000000000000000000000000000000000000a\
        0000000000000000000000000000000000000000000000000000000000000000\
        0000000000000000000000000000000000000000000000000000000000000001\
        fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffc18\
        0000000000000000000000000000000000000001000000000000000000000000\
        0000000000000000000000000000000000000000000000000000000000000120\
        0000000000000000000000000000000000000000000000000000000000000000";

    /// `cast keccak $(cast abi-encode 'f((address,address,uint24,int24,address))' …)`
    /// over the same key — the mainnet ETH/USDC 0.05 % `poolId`.
    const CANONICAL_POOL_ID: &str =
        "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27";

    fn canonical_swap_calldata() -> Vec<u8> {
        alloy::hex::decode(CANONICAL_SWAP_CALLDATA).expect("pinned calldata is hex")
    }

    /// The first byte of the 32-byte head word `word` (word 0 begins right
    /// after the selector).
    fn word_offset(word: usize) -> usize {
        4 + word * 32
    }

    fn set_calldata_byte(calldata: &mut [u8], offset: usize, value: u8) {
        calldata[offset] = value;
    }

    #[test]
    fn decode_v4_pool_manager_swap_names_the_pool_id() {
        let id = decode_v4_pool_manager_swap_pool_id(&canonical_swap_calldata())
            .expect("a canonical swap call names its pool");
        let expected: B256 = CANONICAL_POOL_ID.parse().expect("the pinned poolId is hex");
        assert_eq!(B256::from(id), expected);
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_wrong_selector() {
        // A different `PoolManager` entry point (`settle`, `take`, `unlock`)
        // carries no `PoolKey` at all: the selector is what distinguishes a
        // swap call, so its absence is a refusal rather than a best-effort read
        // of five words that mean something else.
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, 3, 0x4d);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_truncated_selector() {
        assert!(decode_v4_pool_manager_swap_pool_id(&[]).is_none());
        assert!(decode_v4_pool_manager_swap_pool_id(&[0xf3, 0xcd, 0x91]).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_truncated_head() {
        // One byte short of the 9-word static head: the `hookData` offset word
        // is absent, so no `PoolKey` can be claimed.
        let calldata = canonical_swap_calldata();
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata[..4 + 9 * 32 - 1]).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_truncated_hook_data() {
        // The `hookData` tail owns a 32-byte length word even when empty; a
        // call that stops at the offset word is not the ABI encoding of it.
        let calldata = canonical_swap_calldata();
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata[..4 + 9 * 32]).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_trailing_bytes() {
        // The `bytes` tail is length-bounded: a surplus byte is a refusal, not
        // an ignored word (the encoder never emits one).
        let mut calldata = canonical_swap_calldata();
        calldata.push(0x00);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_dirty_currency() {
        // `Currency` is an `address`: the high 12 bytes of the word are zero.
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(1), 0x01);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_over_wide_fee() {
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(2), 0x01);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_non_sign_extended_tick_spacing() {
        // `tickSpacing` is an `int24`: a zero-extended `-1` (0xff in the low
        // three bytes, zeroes above) is a different key, not a maskable value.
        let mut calldata = canonical_swap_calldata();
        for byte in 29..32 {
            set_calldata_byte(&mut calldata, word_offset(3) + byte, 0xff);
        }
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_dirty_hooks() {
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(4), 0x01);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_non_bool_zero_for_one() {
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(5) + 31, 0x02);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_over_wide_price_limit() {
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(7), 0x01);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_rejects_non_canonical_hook_data_offset() {
        // The offset word must name the canonical tail start; a shifted offset
        // reads the length word from inside the head and is a refusal.
        let mut calldata = canonical_swap_calldata();
        set_calldata_byte(&mut calldata, word_offset(8) + 31, 0x40);
        assert!(decode_v4_pool_manager_swap_pool_id(&calldata).is_none());
    }

    #[test]
    fn decode_v4_pool_manager_swap_distinguishes_pools_sharing_a_manager() {
        // Two keys through ONE PoolManager — the shape two V4 hops take. Their
        // ids must differ, which is what lets a failing frame name its pool
        // rather than only the shared manager.
        let first = canonical_swap_calldata();
        let mut second = canonical_swap_calldata();
        set_calldata_byte(&mut second, word_offset(2) + 31, 0xb8); // fee 3000
        set_calldata_byte(&mut second, word_offset(2) + 30, 0x0b);

        let first_id = decode_v4_pool_manager_swap_pool_id(&first).expect("first key decodes");
        let second_id = decode_v4_pool_manager_swap_pool_id(&second).expect("second key decodes");
        assert_ne!(first_id, second_id);
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
