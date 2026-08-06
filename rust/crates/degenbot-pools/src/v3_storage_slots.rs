#![allow(
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]
#![allow(
    clippy::identity_op,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
//! V3 on-chain storage-slot encoders — engine typed state → EVM slot words.
//!
//! Pure functions (no revm, no pyo3) that pack the engine's `V3PoolState`
//! typed fields into the EXACT `bytes32` storage layout the canonical
//! UniswapV3Pool bytecode reads during a real `swap()` callback. Used by the
//! Tier-3 on-chain accuracy oracle (ergo epic `UP5NH6`, task `NH6NLJ`) to
//! seed an offline revm `CacheDB` from a `V3PoolState` so a `Pool.swap` call
//! against real V3-core bytecode reproduces the engine's swap math — closing
//! the "Rust == Rust" blind spot of Tier 2 (ADR-005 dual-path coverage).
//!
//! ## Why a TEST oracle can seed the swap-math slots the production seam could
//! not serve
//!
//! The production `BotStateDb::storage_ref` (deleted in commit `c4d95424`)
//! served the engine's *partial* state (`sqrtPrice`/`tick`/`liquidity`/
//! tick `gross`+`net`) against RPC-fresh *full* state
//! (`feeGrowthGlobal`, per-tick `feeGrowthOutside`, `tickBitmap` word values,
//! `observation` index/cardinality) → intra-sim `LOK` reverts on every
//! cross-tick swap (see `docs/architecture/in_process_sim_served_slots.md`).
//!
//! A **test oracle** owns the whole state: it seeds EVERY slot the swap reads
//! from a single synthetic but *fully-consistent* `V3PoolState`, so the LOK
//! invariant cannot trip. And crucially, swap MATH (amount0/amount1,
//! sqrtPriceNext, liquidity, tick) reads only `slot0` (sqrtPrice+tick) +
//! `liquidity` + `tickBitmap` word *values* + `ticks[i].liquidityGross`/
//! `liquidityNet` + `feeProtocol`. The `feeGrowthGlobal`/`feeGrowthOutside`/
//! `tickCumulative`/`seconds*` fields are **written** as side-effects during
//! tick crossing but **never read into the output amounts** — so seeding them
//! to zero (a self-consistent fresh-pool initial state) yields a correct
//! swap-math read with no engine-state extension. The encoders here zero-fill
//! every on-chain field the engine does not carry, EXCEPT the observation
//! cardinal/next values in a fresh `slot0`, which are seeded to `1` (the
//! post-`initialize()` value): a 0 cardinality makes the on-chain `swap()`'s
//! observation bookkeeping (`observeSingle`/`_updateObservation`) walk an
//! infinite loop and OOG — the oracle fixture needs the swap to terminate.
//!
//! ## Storage layout reference (UniswapV3Pool, v3-core v1.0.0)
//!
//! | Slot | Field                                         | Engine field           |
//! |-----:|-----------------------------------------------|------------------------|
//! | 0    | `slot0` (packed, see [`V3Slot0Parts`])        | `sqrt_price_x96`, `tick` + zero-filled tail |
//! | 4    | `liquidity` (`uint128`, high 128 bits zero)   | `liquidity`            |
//! | 5    | `ticks` mapping base                          | `tick_data` (gross+net); per-tick = `keccak256(int24(tick) . bytes32(5))` |
//! | 6    | `tickBitmap` mapping base                     | synth from `tick_data` keys; per-word = `keccak256(int16(word) . bytes32(6))` |
//!
//! `slot0` bit layout (LSB-first, per v3-core `UniswapV3Pool.Slot0` struct):
//! `[0..160)` `uint160 sqrtPriceX96` · `[160..184)` `int24 tick` ·
//! `[184..200)` `uint16 observationIndex` · `[200..216)` `uint16 observationCardinality` ·
//! `[216..232)` `uint16 observationCardinalityNext` · `[232..240)` `uint8 feeProtocol` ·
//! `[240]` `bool unlocked` · `[241..256)` unused.

use alloy::primitives::{keccak256, U256};

use crate::TickInfo;

/// V3 `slot0` storage slot number (`UniswapV3Pool.slot0`).
pub const V3_SLOT0_SLOT: u64 = 0;
/// V3 `liquidity` storage slot number (`uint128`, high 128 bits zero).
pub const V3_LIQUIDITY_SLOT: u64 = 4;
/// V3 `ticks` mapping base slot (`mapping(int24 => TickInfo)`); per-tick slot =
/// `keccak256(abi.encode(int24 tick) . abi.encode(uint256(5)))`.
pub const V3_TICKS_MAPPING_SLOT: u64 = 5;
/// V3 `tickBitmap` mapping base slot (`mapping(int16 => uint256)`); per-word
/// slot = `keccak256(abi.encode(int16 word) . abi.encode(uint256(6)))`.
pub const V3_TICK_BITMAP_MAPPING_SLOT: u64 = 6;

/// Mask selecting the low 128 bits of a `U256` (the `int128`/`uint128` field
/// width used by V3 `liquidity` and `TickInfo.liquidityNet`).
const MASK_128: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);
/// Mask selecting the low 160 bits of a `U256` (the `uint160 sqrtPriceX96`
/// field width in V3 `slot0`).
const MASK_160: U256 = U256::from_limbs([u64::MAX, u64::MAX, u64::MAX & 0xFF, 0]);

/// The decoded/encodable packed fields of a V3 `slot0` storage word.
///
/// Mirrors the v3-core `UniswapV3Pool.Slot0` struct field order + bit layout
/// (see module docs). Used as both the encode input and the decode output so
/// the mainnet round-trip test can assert `encode(decode(pinned)) == pinned`
/// against a real on-chain `slot0` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct V3Slot0Parts {
    /// `uint160 sqrtPriceX96` at bits `[0..160)` (Q64.96).
    pub sqrt_price_x96: U256,
    /// `int24 tick` at bits `[160..184)`.
    pub tick: i32,
    /// `uint16 observationIndex` at bits `[184..200)`. Zero-filled for the
    /// fresh-pool oracle (swap math does not read it).
    pub observation_index: u16,
    /// `uint16 observationCardinality` at bits `[200..216)`. Zero-filled.
    pub observation_cardinality: u16,
    /// `uint16 observationCardinalityNext` at bits `[216..232)`. Zero-filled.
    pub observation_cardinality_next: u16,
    /// `uint8 feeProtocol` at bits `[232..240)`. Zero-filled (no protocol fee).
    pub fee_protocol: u8,
    /// `bool unlocked` at bit `[240]`. **Must be `true`** for a `swap()` call
    /// to execute (reverts with `LOK` otherwise). The fresh-pool oracle sets
    /// this `true`.
    pub unlocked: bool,
}

/// Sign-extend an `int24` tick value to a 32-byte big-endian ABI-encoded word
/// (the mapping-key form `abi.encode(int24)` produces). Negative ticks get the
/// high 29 bytes set to `0xFF`; the low 3 bytes hold the two's-complement
/// value. Used in the `ticks(tick)` + V4 per-tick mapping-slot keccak preimage.
///
/// Accepts `i32` (the engine's tick type) but masks to the 24-bit field
/// before sign-extending — matches `abi.encode(int24(tick))` for any tick in
/// the V3/V4 valid range `[-887272, 887272]` (fits `int24`).
#[must_use]
pub fn sign_extend_int24(tick: i32) -> [u8; 32] {
    #[allow(clippy::cast_sign_loss)]
    let low24 = (tick as u32) & 0x00FF_FFFF;
    let mut bytes = [0u8; 32];
    // Place the 3 significant bytes at the low end (bytes 29..32).
    bytes[29..32].copy_from_slice(&low24.to_be_bytes()[1..4]);
    if (low24 & 0x800000) != 0 {
        // int24 sign bit set → sign-extend the high 29 bytes.
        for b in &mut bytes[0..29] {
            *b = 0xFF;
        }
    }
    bytes
}

/// Sign-extend an `int16` tick-bitmap word position to a 32-byte big-endian
/// ABI-encoded word (`abi.encode(int16)`). Negative word positions (sparse
/// pools below tick 0) get the high 30 bytes set to `0xFF`. Used in the V3
/// `tickBitmap(word)` + V4 per-bitmap-word mapping-slot keccak preimage.
#[must_use]
pub fn sign_extend_int16(word_pos: i16) -> [u8; 32] {
    #[allow(clippy::cast_sign_loss)]
    let low16 = (word_pos as u16) & 0xFFFF;
    let mut bytes = [0u8; 32];
    bytes[30..32].copy_from_slice(&low16.to_be_bytes());
    if (low16 & 0x8000) != 0 {
        for b in &mut bytes[0..30] {
            *b = 0xFF;
        }
    }
    bytes
}

/// Encode the V3 `slot0` storage word (slot 0) from its packed fields.
///
/// Bit layout per v3-core `Slot0` (see module docs). The `int24 tick` is
/// placed as exactly 24 bits at `[160..184)` (NOT sign-extended across the
/// tail) so the observation/fee/unlocked fields keep their correct positions
/// for negative ticks — required for a byte-exact round-trip of a real
/// on-chain `slot0` value.
#[must_use]
pub fn encode_v3_slot0(parts: V3Slot0Parts) -> U256 {
    let sqrt_price = parts.sqrt_price_x96 & MASK_160;
    // int24 tick as an unsigned 24-bit field (two's-complement reinterpretation
    // masked to 24 bits) — occupies exactly bits [160..184), leaving the tail
    // fields intact even for negative ticks.
    #[allow(clippy::cast_sign_loss)]
    let tick_field = U256::from(parts.tick as u32 & 0x00FF_FFFF);
    let obs_index = U256::from(parts.observation_index);
    let obs_card = U256::from(parts.observation_cardinality);
    let obs_card_next = U256::from(parts.observation_cardinality_next);
    let fee_protocol = U256::from(parts.fee_protocol);
    let unlocked = U256::from(u8::from(parts.unlocked));
    sqrt_price
        | (tick_field << 160)
        | (obs_index << 184)
        | (obs_card << 200)
        | (obs_card_next << 216)
        | (fee_protocol << 232)
        | (unlocked << 240)
}

/// Decode a V3 `slot0` storage word into its packed fields (the inverse of
/// [`encode_v3_slot0`]). Used by the mainnet round-trip test
/// (`encode_v3_slot0(decode_v3_slot0(pinned)) == pinned`) and by the Tier-3b
/// seeding layer to read a pinned on-chain `slot0` for fixture derivation.
#[must_use]
pub fn decode_v3_slot0(word: U256) -> V3Slot0Parts {
    let sqrt_price_x96 = word & MASK_160;
    let tick_field = (word >> 160u32) & U256::from(0x00FF_FFFFu32);
    let observation_index = ((word >> 184u32) & U256::from(0xFFFFu32)).to::<u16>();
    let observation_cardinality = ((word >> 200u32) & U256::from(0xFFFFu32)).to::<u16>();
    let observation_cardinality_next = ((word >> 216u32) & U256::from(0xFFFFu32)).to::<u16>();
    let fee_protocol = ((word >> 232u32) & U256::from(0xFFu32)).to::<u8>();
    let unlocked = !((word >> 240u32) & U256::from(1u32)).is_zero();
    // Sign-extend the 24-bit tick field back to i32 (bit 23 is the sign bit).
    #[allow(clippy::cast_possible_wrap)]
    let tick_u32 = tick_field.to::<u32>();
    let tick = if (tick_u32 & 0x800000) != 0 {
        (tick_u32 as i32) - (1 << 24)
    } else {
        tick_u32 as i32
    };
    V3Slot0Parts {
        sqrt_price_x96,
        tick,
        observation_index,
        observation_cardinality,
        observation_cardinality_next,
        fee_protocol,
        unlocked,
    }
}

/// Encode a fresh-pool V3 `slot0` from just the swap-math-relevant fields:
/// `unlocked = true` (a `swap()` reverts with `LOK` when locked), and the
/// observation cardinality set to 1 (the post-`initialize()` value — a swap's
/// `observeSingle`/`_updateObservation` bookkeeping path expects ≥1, and a 0
/// cardinality sends the observation-grow walk into an infinite loop that OOGs
/// the swap). This is the form the Tier-3b seeding layer uses to seed a
/// `CacheDB` from a `V3PoolState`.
#[must_use]
pub fn encode_v3_slot0_fresh(sqrt_price_x96: U256, tick: i32) -> U256 {
    encode_v3_slot0(V3Slot0Parts {
        sqrt_price_x96,
        tick,
        observation_index: 0,
        observation_cardinality: 1,
        observation_cardinality_next: 1,
        fee_protocol: 0,
        unlocked: true,
    })
}

/// Encode the V3 `liquidity` storage word (slot 4): `uint128` in the low 128
/// bits, high 128 bits zero.
#[must_use]
pub fn encode_v3_liquidity_slot(liquidity: u128) -> U256 {
    U256::from(liquidity) & MASK_128
}

/// Encode the V3 `TickInfo` packed storage word (the `ticks(tick)` slot+0
/// value): `uint128 liquidityGross` in the LOW 128 bits, `int128 liquidityNet`
/// in the HIGH 128 bits. This matches canonical v3-core `Tick.Info`, which
/// declares `liquidityGross` FIRST (so Solidity packs it at the lowest bits)
/// and `liquidityNet` SECOND (the high 128 bits). The `feeGrowthOutside0/1`
/// tail slots (slot+1/+2) are NOT encoded here — they are zero-filled by the
/// seeding layer (the swap math never reads them into the amounts).
///
/// `liquidity_net` is an `i128`; the `int128` field is the value's 128-bit
/// two's complement. A negative net therefore lives in the high 128 bits with
/// the sign-extension masked away, leaving the low-128 `liquidity_gross` half
/// untouched.
///
/// # On-chain ground truth
///
/// This is a Tier-3 verified layout: the up-direction (oneForZero) byte-exact
/// oracle (`tier3_v3_pool_swap_vs_revm.rs`) previously seeded the pool with
/// gross/net SWAPPED, and a swap crossing an upper boundary read `net = +
/// gross` (liquidity grew instead of shrinking). Fixing the halves to match the
/// canonical pool made the up-direction walk byte-exact (epic CMORFZ/6DLK7I).
#[must_use]
pub fn encode_v3_tick_info_slot(tick_info: &TickInfo) -> U256 {
    let gross = U256::from(tick_info.liquidity_gross.to::<u128>());
    // `I256::into_raw()` yields the full 256-bit two's-complement bit pattern
    // (negative values have the high 128 bits set). Mask to the LOW 128 bits —
    // the `int128` field width — then shift into the HIGH half so the low-128
    // `gross` field is not corrupted by a negative net's sign extension.
    let net_high128 = (tick_info.liquidity_net.into_raw() & MASK_128) << 128;
    gross | net_high128
}

/// Compute the storage slot for a single V3 `ticks(tick)` entry:
/// `keccak256(abi.encode(int24 tick) . abi.encode(uint256(5)))`.
///
/// The slot is a pure function of the tick (the V3 contract's storage layout
/// is identical across all V3 pools); the pool ADDRESS selects the
/// `CacheDB` account at seed time, NOT part of the slot derivation. Returns
/// `U256` (the revm `CacheDB` slot-key type).
///
/// The per-tick `TickInfo` occupies THREE consecutive slots starting here:
/// slot+0 = `liquidityGross | liquidityNet` (see [`encode_v3_tick_info_slot`]),
/// slot+1 = `feeGrowthOutside0`, slot+2 = `feeGrowthOutside1`. The seeding
/// layer writes slot+0 from the engine + zero-fills slot+1/+2 (never read
/// by the swap math).
#[must_use]
pub fn v3_tick_mapping_slot(tick: i32) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int24(tick));
    preimage[32..64].copy_from_slice(&U256::from(V3_TICKS_MAPPING_SLOT).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Compute the storage slot for a single V3 `tickBitmap(word)` entry:
/// `keccak256(abi.encode(int16 word) . abi.encode(uint256(6)))`.
///
/// As with [`v3_tick_mapping_slot`], the pool address is the `CacheDB` account
/// (applied at seed time), not part of the slot derivation.
#[must_use]
pub fn v3_tick_bitmap_word_slot(word_pos: i16) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int16(word_pos));
    preimage[32..64].copy_from_slice(&U256::from(V3_TICK_BITMAP_MAPPING_SLOT).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Compute the V3 `tickBitmap` word VALUE (a `uint256` bitmask) for a single
/// bitmap word from the COMPRESSED tick indices that fall within it.
///
/// Each compressed tick `c` sets bit `c & 0xFF` (bit 0 = LSB = tick
/// `word_pos * 256 + 0`). Negative compressed ticks are handled correctly:
/// `-1 & 0xFF == 255` (matches `rem_euclid(256)`), matching Solidity's
/// `tick & 0xFF` bit-position convention.
///
/// `compressed_ticks_in_word` must already be the `tick.div_euclid(tick_spacing)`
/// values filtered to one `word_pos` (see [`compute_v3_tick_bitmap_word_from_raw`]
/// for the raw-tick + spacing + word-pos convenience).
#[must_use]
pub fn compute_v3_tick_bitmap_word(compressed_ticks_in_word: &[i32]) -> U256 {
    let mut word = U256::ZERO;
    for &c in compressed_ticks_in_word {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let bit = (c & 0xFF) as u8;
        word |= U256::from(1u64) << bit;
    }
    word
}

/// Compute the V3 `tickBitmap` word value for a single `word_pos` directly from
/// the engine's RAW `tick_data` keys + the pool's `tick_spacing`. Filters the
/// raw ticks to those whose compressed word position equals `word_pos`,
/// compresses them, and delegates to [`compute_v3_tick_bitmap_word`].
///
/// This is the high-level helper the Tier-3b seeding layer calls for each
/// occupied `word_pos` to write `compute_v3_tick_bitmap_word_from_raw(...)`
/// at slot [`v3_tick_bitmap_word_slot(word_pos)`].
#[must_use]
pub fn compute_v3_tick_bitmap_word_from_raw<S: std::hash::BuildHasher>(
    tick_data: &std::collections::HashMap<i32, TickInfo, S>,
    tick_spacing: i32,
    word_pos: i16,
) -> U256 {
    let mut compressed: Vec<i32> = Vec::new();
    for &tick in tick_data.keys() {
        let c = tick.div_euclid(tick_spacing);
        if i16::try_from(c >> 8).unwrap_or(0) == word_pos {
            compressed.push(c);
        }
    }
    compute_v3_tick_bitmap_word(&compressed)
}

// ─────────────────────────────────────────────────────────────────────────
// Tests — mainnet-pinned (cast storage) + cast-keccak oracles. No live RPC.
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    // Pinned mainnet triple (UniswapV3Pool 0xCBCdF9626bC03E24f779434178A73a0B4bad62eD),
    // from docs/architecture/in_process_sim_served_slots.md.
    const V3_PINNED_SLOT0_HEX: &str =
        "0x00016601680168000a040cef000000000008dca6028b78f02240aced87bb387d";
    const V3_PINNED_SQRT_PRICE_X96: &str = "46013657643641178635361266647644285"; // decimal
    const V3_PINNED_TICK: i32 = 265455;
    const V3_PINNED_LIQUIDITY_HEX: &str =
        "0x000000000000000000000000000000000000000000000000006b5d49e99f8835";
    const V3_PINNED_LIQUIDITY: u128 = 30_220_394_541_582_389;

    // Cast-keccak oracle (computed offline via `cast keccak`, NOT RPC): the V3
    // ticks(tick) mapping slot for tick=-887270 at base slot 5.
    const V3_TICK_SLOT_CAST: &str =
        "0xeb932033740564f24d9a56736a77e2fd5f9837a58122fe9a7f61e1e4dac5c15b";
    // Cast-keccak oracle: the V3 tickBitmap(word) mapping slot for word=-1 at
    // base slot 6.
    const V3_BITMAP_SLOT_CAST: &str =
        "0x63187d71e139eee983a88d0737447c7451979b3dbb75903c76b5fe430d36588e";

    fn u256_from_dec(s: &str) -> U256 {
        s.parse::<U256>().expect("valid decimal U256")
    }

    fn u256_from_hex(s: &str) -> U256 {
        U256::from_str_radix(s.trim_start_matches("0x"), 16).expect("valid hex U256")
    }

    /// Decode → re-encode round-trips the pinned mainnet `slot0` byte-exact,
    /// AND the extracted `sqrtPriceX96` + `tick` match the recorded decimals.
    /// This proves the bit layout against a real on-chain slot0 value.
    #[test]
    fn v3_slot0_round_trips_pinned_mainnet_slot0_and_extracts_fields() {
        let pinned = u256_from_hex(V3_PINNED_SLOT0_HEX);
        let parts = decode_v3_slot0(pinned);
        // Field extraction against the recorded mainnet decimals.
        assert_eq!(
            parts.sqrt_price_x96,
            u256_from_dec(V3_PINNED_SQRT_PRICE_X96),
            "decoded sqrtPriceX96 must match the pinned mainnet decimal"
        );
        assert_eq!(
            parts.tick, V3_PINNED_TICK,
            "decoded tick must match the pinned mainnet tick"
        );
        // Round-trip: re-encoding the decoded parts reproduces the pinned word
        // byte-exact (proves the tail fields — observation/fee/unlocked — are
        // placed at the correct positions).
        assert_eq!(
            encode_v3_slot0(parts),
            pinned,
            "round-trip encode(decode(pinned)) must equal pinned byte-exact"
        );
    }

    /// Fresh-pool encoder zero-fills the tail + sets `unlocked=true`; the
    /// sqrtPrice+tick fields still land at their documented positions.
    #[test]
    fn v3_slot0_fresh_sets_unlocked_and_zero_fills_tail() {
        let word = encode_v3_slot0_fresh(u256_from_dec(V3_PINNED_SQRT_PRICE_X96), V3_PINNED_TICK);
        let parts = decode_v3_slot0(word);
        assert!(parts.unlocked, "fresh slot0 must be unlocked");
        assert_eq!(parts.observation_index, 0);
        // Cardinality = 1 is the post-`initialize()` value (the ORACLE path
        // needs the swap's observation bookkeeping to terminate; see the
        // `v3_slot0_fresh` doc).
        assert_eq!(parts.observation_cardinality, 1);
        assert_eq!(parts.observation_cardinality_next, 1);
        assert_eq!(parts.fee_protocol, 0);
        assert_eq!(
            parts.sqrt_price_x96,
            u256_from_dec(V3_PINNED_SQRT_PRICE_X96)
        );
        assert_eq!(parts.tick, V3_PINNED_TICK);
    }

    /// Negative-tick placement: the int24 occupies exactly bits [160..184)
    /// (no sign-extension across the tail), so the observation fields stay
    /// readable. This is the historical deletion-cause regression guard
    /// (`sign_extend_24` in the deleted `bot_state_db.rs` filled the tail with
    /// the sign extension — see task body).
    #[test]
    fn v3_slot0_negative_tick_does_not_corrupt_tail() {
        let word = encode_v3_slot0(V3Slot0Parts {
            sqrt_price_x96: U256::from(0xABCDEFu32),
            tick: -887_270, // negative
            observation_index: 7,
            observation_cardinality: 9,
            observation_cardinality_next: 11,
            fee_protocol: 4,
            unlocked: true,
        });
        let parts = decode_v3_slot0(word);
        assert_eq!(parts.tick, -887_270, "negative tick round-trips");
        assert_eq!(parts.observation_index, 7);
        assert_eq!(parts.observation_cardinality, 9);
        assert_eq!(parts.observation_cardinality_next, 11);
        assert_eq!(parts.fee_protocol, 4);
        assert!(parts.unlocked);
        // Round-trip byte-exact.
        assert_eq!(encode_v3_slot0(parts), word);
    }

    /// The pinned mainnet `liquidity` slot (slot 4) round-trips + decodes to
    /// the recorded decimal.
    #[test]
    fn v3_liquidity_slot_round_trips_pinned_mainnet() {
        let pinned = u256_from_hex(V3_PINNED_LIQUIDITY_HEX);
        assert_eq!(
            encode_v3_liquidity_slot(V3_PINNED_LIQUIDITY),
            pinned,
            "encoded liquidity must equal the pinned mainnet slot-4 word"
        );
        assert_eq!(
            pinned & MASK_128,
            U256::from(V3_PINNED_LIQUIDITY),
            "low 128 bits hold the liquidity"
        );
    }

    /// `sign_extend_int24` matches `abi.encode(int24(tick))` (negative sign
    /// extension into the high bytes).
    #[test]
    fn sign_extend_int24_negative_tick_is_two_complement() {
        // tick = -1 → abi.encode(int24(-1)) = bytes32(0xFF…FF) (sign-extended).
        assert_eq!(sign_extend_int24(-1), [0xFF; 32]);
        // tick = -887270 → low 3 bytes are the two's complement, high 29 0xFF.
        let bytes = sign_extend_int24(-887_270);
        assert!(
            bytes[..29].iter().all(|&b| b == 0xFF),
            "high 29 bytes sign-set"
        );
        // The int24 value of -887270 as 3 bytes = 0xF2761A (two's complement).
        assert_eq!(&bytes[29..32], &[0xF2, 0x76, 0x1A]);
        // Positive tick: high bytes zero, low 3 = big-endian value.
        let pos = sign_extend_int24(265_455);
        assert!(pos[..29].iter().all(|&b| b == 0x00));
        assert_eq!(&pos[29..32], &[0x04, 0x0C, 0xEF]);
    }

    /// `sign_extend_int16` matches `abi.encode(int16(word))`.
    #[test]
    fn sign_extend_int16_negative_word_is_two_complement() {
        assert_eq!(sign_extend_int16(-1), [0xFF; 32]);
        let bytes = sign_extend_int16(-2);
        assert!(bytes[..30].iter().all(|&b| b == 0xFF));
        assert_eq!(&bytes[30..32], &[0xFF, 0xFE]);
    }

    /// The V3 `ticks(tick)` mapping slot derivation matches the offline cast
    /// keccak oracle for tick=-887270 at base slot 5.
    #[test]
    fn v3_tick_mapping_slot_matches_cast_keccak_oracle() {
        assert_eq!(
            v3_tick_mapping_slot(-887_270),
            u256_from_hex(V3_TICK_SLOT_CAST),
            "ticks(tick) slot must equal keccak(abi.encode(int24 tick) . abi.encode(uint256(5)))"
        );
    }

    /// The V3 `tickBitmap(word)` mapping slot derivation matches the offline
    /// cast keccak oracle for word=-1 at base slot 6.
    #[test]
    fn v3_tick_bitmap_word_slot_matches_cast_keccak_oracle() {
        assert_eq!(
            v3_tick_bitmap_word_slot(-1),
            u256_from_hex(V3_BITMAP_SLOT_CAST),
            "tickBitmap(word) slot must equal keccak(abi.encode(int16 word) . abi.encode(uint256(6)))"
        );
    }

    /// The `ticks(tick)` mapping slot for a POSITIVE tick differs from the
    /// negative-tick slot (regression: a wrong sign-extension would collide).
    #[test]
    fn v3_tick_mapping_slot_positive_distinct_from_negative() {
        assert_ne!(
            v3_tick_mapping_slot(887_270),
            v3_tick_mapping_slot(-887_270),
            "positive + negative tick slots must not collide"
        );
    }

    /// `compute_v3_tick_bitmap_word` sets bit `(compressed & 0xFF)` per tick,
    /// with negative compressed ticks mapping to their `rem_euclid(256)` bit.
    #[test]
    fn v3_tick_bitmap_word_sets_correct_bits() {
        // compressed ticks [0, 1, 255, -1] → bits 0, 1, 255, 255 (last two
        // collide to bit 255, the OR is idempotent).
        let word = compute_v3_tick_bitmap_word(&[0, 1, 255, -1]);
        assert!(!(word & (U256::from(1u64) << 0u32)).is_zero(), "bit 0 set");
        assert!(!(word & (U256::from(1u64) << 1u32)).is_zero(), "bit 1 set");
        assert!(
            !(word & (U256::from(1u64) << 255u32)).is_zero(),
            "bit 255 set (compressed 255 AND -1 both map here)"
        );
        assert!((word & (U256::from(1u64) << 2u32)).is_zero(), "bit 2 unset");
    }

    /// High-level raw-tick convenience: compresses + filters to one word and
    /// delegates to the compressed-tick builder. With tick_spacing=60, raw
    /// tick 0 → compressed 0 (word 0, bit 0); raw tick 60 → compressed 1
    /// (word 0, bit 1); raw tick 15360 → compressed 256 (word 1, bit 0).
    #[test]
    fn v3_tick_bitmap_word_from_raw_splits_across_words() {
        let mut tick_data = std::collections::HashMap::new();
        tick_data.insert(0, make_tick_info(100, 50));
        tick_data.insert(60, make_tick_info(100, 50));
        tick_data.insert(15_360, make_tick_info(100, 50)); // word_pos 1
        let word0 = compute_v3_tick_bitmap_word_from_raw(&tick_data, 60, 0);
        assert_eq!(
            word0,
            (U256::from(1u64) << 0u32) | (U256::from(1u64) << 1u32)
        );
        let word1 = compute_v3_tick_bitmap_word_from_raw(&tick_data, 60, 1);
        assert_eq!(word1, U256::from(1u64));
    }

    /// `encode_v3_tick_info_slot`: gross in LOW 128 (declaration order of
    /// canonical `Tick.Info`), net as int128 two's complement in HIGH 128.
    /// A NEGATIVE net must NOT corrupt the gross half (the on-chain-layout
    /// regression fixed when the up-direction Tier-3 oracle caught gross/net
    /// swapped).
    #[test]
    fn v3_tick_info_slot_packs_gross_low_net_high_negative_preserves_gross() {
        let tick_info = make_tick_info(5_000_000, -3_000_000);
        let word = encode_v3_tick_info_slot(&tick_info);
        let gross = word & MASK_128;
        assert_eq!(gross, U256::from(5_000_000u128), "gross in low 128");
        let net_high = (word >> 128) & MASK_128;
        // int128 of -3_000_000 == 2^128 - 3_000_000 (two's complement).
        let expected_net = (U256::from(1u64) << 128) - U256::from(3_000_000u64);
        assert_eq!(
            net_high, expected_net,
            "net = -3M as int128 two's complement"
        );
    }

    /// `encode_v3_tick_info_slot` positive net round-trips (gross low, net high).
    #[test]
    fn v3_tick_info_slot_positive_net() {
        let tick_info = make_tick_info(1_000, 2_500);
        let word = encode_v3_tick_info_slot(&tick_info);
        assert_eq!(word & MASK_128, U256::from(1_000u128));
        assert_eq!((word >> 128) & MASK_128, U256::from(2_500u128));
    }

    fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> TickInfo {
        use alloy::primitives::{I256, U128};
        TickInfo {
            liquidity_gross: U128::from(liquidity_gross),
            liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
            block: 0,
        }
    }
}
