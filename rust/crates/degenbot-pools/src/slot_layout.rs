//! The slot-layout table — ONE source both storage directions consume.
//!
//! Every piece of per-family slot knowledge (which storage index, which
//! fields, which packing) lives here as a pack + decode pair:
//!
//! - the PACK direction packs the engine's typed state into on-chain words
//!   (the divergence probe + the sim-anchor projection consume this);
//! - the DECODE direction recovers typed fields from journalled or chain
//!   words (the journal post-state extractors consume this).
//!
//! The directions agree by construction because they call the same rows:
//! `decode(pack(typed))` reproduces the engine's fields with untracked bits
//! zeroed, and the round-trip tests pin pack==word/decode==engine-state
//! against independent literals. Re-deriving slot indices at a call site
//! is how layout drift returns (the pancake classification incident — a
//! fork pool read through canonical Uniswap indices misreads every field).
//! CL-family mapping slots delegate to [`crate::v3_storage_slots`] /
//! [`crate::v3_pancakeswap_storage_slots`] through the [`ClSlotLayout`]
//! selector.
// Solidity/EVM identifiers (uint112, slot0, liquidity, uint256, SSTORE,
// keccak256) are ubiquitous — match the crate's storage-slot modules.

use alloy::primitives::{aliases::U112, keccak256, B256, U256};

use crate::v3_storage_slots::{sign_extend_int16, sign_extend_int24};
use crate::ClSlotLayout;

/// Mask selecting the low 160 bits of a `U256` (the `uint160 sqrtPriceX96`
/// field width inside CL `slot0`).
const MASK_160: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0]);

// ─────────────────────────────────────────────────────────────────────────
// V2 pair — `reserves` slot 8
// ─────────────────────────────────────────────────────────────────────────

/// The V2 pair's packed `reserves` slot:
/// `uint112 reserve0 | uint112 reserve1 | uint32 blockTimestampLast`.
pub const V2_RESERVES_SLOT: u64 = 8;

/// Tracked (engine-carried) bits of a V2 `reserves` word — the low 224
/// bits. The high-32 `blockTimestampLast` is NOT tracked: packed as zero,
/// and masked out of any engine-vs-chain comparison.
pub const V2_RESERVES_TRACKED_MASK: B256 = low_bits_mask(224);

/// The decoded/encodable fields of a V2 `reserves` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2ReservesParts {
    /// `uint112 reserve0` (the low 112 bits).
    pub reserve0: U112,
    /// `uint112 reserve1` (bits 112..224).
    pub reserve1: U112,
    /// `uint32 blockTimestampLast` (bits 224..256). The pack direction
    /// zeroes this (the engine does not track it); decode reads it through.
    pub block_timestamp_last: u32,
}

/// Pack the engine's V2 reserves into the on-chain `reserves` word
/// (untracked `blockTimestampLast` zeroed).
#[must_use]
pub fn pack_v2_reserves_word(reserve0: U112, reserve1: U112) -> B256 {
    let r0 = U256::from(reserve0);
    let r1 = U256::from(reserve1);
    (r0 | (r1 << 112u32)).to_be_bytes::<32>().into()
}

/// Decode an on-chain or journalled `reserves` word into its fields —
/// including the timestamp the pack direction does not carry.
#[must_use]
pub fn decode_v2_reserves_word(word: U256) -> V2ReservesParts {
    let mask112 = (U256::from(1u128) << 112) - U256::from(1u128);
    V2ReservesParts {
        reserve0: U112::from(word & mask112),
        reserve1: U112::from((word >> 112) & mask112),
        block_timestamp_last: u32::try_from(word >> 224).unwrap_or(u32::MAX),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// CL (V3/V4) — slot0, liquidity, per-tick words, mapping slots, masks
// ─────────────────────────────────────────────────────────────────────────

/// Tracked bits of a CL `slot0` word: `uint160 sqrtPriceX96 | int24 tick`
/// (low 184 bits). The observation/fee-protocol/unlocked bits are NOT
/// tracked — packed zero, masked out of comparisons.
pub const CL_SLOT0_TRACKED_MASK: B256 = low_bits_mask(184);

/// Tracked bits of a CL `liquidity` word — the low-128 `uint128`.
pub const CL_LIQUIDITY_TRACKED_MASK: B256 = low_bits_mask(128);

/// Tracked bits of a per-tick `ticks(tick)` slot+0 word — the FULL 256 bits
/// (`uint128 liquidityGross | int128 liquidityNet`; the engine's `TickInfo`
/// carries both).
pub const CL_TICK_WORD_MASK: B256 = low_bits_mask(256);

/// Pack a CL `slot0` word with untracked bits zeroed:
/// `uint160 sqrtPriceX96 | int24 tick << 160`. sqrtPriceX96 is masked to
/// 160 bits first so a stray high bit cannot bleed into the tick field.
///
/// The tick is placed as its 24-bit two's-complement pattern: the signed→
/// unsigned bit pattern cast is intentional (the on-chain int24 IS the low
/// 24 bits of the two's-complement i32).
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    reason = "the on-chain int24 IS the low 24 bits of the two's-complement i32 pattern"
)]
pub fn cl_slot0_tracked_word(sqrt_price_x96: U256, tick: i32) -> B256 {
    let sqrt_masked = sqrt_price_x96 & MASK_160;
    let tick_u = (tick as u32) & 0x00ff_ffff;
    (sqrt_masked | (U256::from(tick_u) << 160u32))
        .to_be_bytes::<32>()
        .into()
}

/// Pack a CL `liquidity` word (`uint128`, high half zero on-chain).
#[must_use]
pub fn cl_liquidity_tracked_word(liquidity: u128) -> B256 {
    U256::from(liquidity).to_be_bytes::<32>().into()
}

/// Pack the per-tick `ticks(tick)` slot+0 word:
/// `uint128 liquidityGross | int128 liquidityNet` (net as the two's-
/// complement int128 bit pattern in the high half).
#[must_use]
pub fn tick_word_of(liquidity_gross: u128, liquidity_net: i128) -> B256 {
    (U256::from(liquidity_gross) | (U256::from(liquidity_net.cast_unsigned()) << 128u32))
        .to_be_bytes::<32>()
        .into()
}

/// Decode a per-tick `ticks(tick)` slot+0 word into `(liquidityGross,
/// liquidityNet)` — the inverse of [`tick_word_of`].
#[must_use]
pub fn decode_tick_word(word: U256) -> (u128, i128) {
    let gross = (word & U256::from(u128::MAX)).to::<u128>();
    let high = (word >> 128u32).to::<u128>();
    // Two's-complement int128: reinterpret the bit pattern width-preservingly.
    let net = i128::from_le_bytes(high.to_le_bytes());
    (gross, net)
}

/// The `ticks(tick)` mapping slot for a per-layout CL pool: ticks live at
/// base 5 (Uniswap V3) or base 6 (Pancake V3 fork).
#[must_use]
pub fn cl_tick_mapping_slot(layout: ClSlotLayout, tick: i32) -> U256 {
    tick_mapping_slot_at_base(tick, U256::from(layout.ticks_mapping_slot()))
}

/// The `tickBitmap` per-word slot for a per-layout CL pool: base 6
/// (Uniswap V3) or base 7 (Pancake V3 fork).
#[must_use]
pub fn cl_tick_bitmap_word_slot(layout: ClSlotLayout, word_pos: i16) -> U256 {
    tick_bitmap_word_slot_at_base(word_pos, U256::from(layout.tick_bitmap_mapping_slot()))
}

/// The `tickBitmap` per-word slot against an explicit mapping base
/// (`mapping(int16 => uint256)` at `base`) — V4 pools pass the pool's
/// `S_state+5` here.
#[must_use]
pub fn tick_bitmap_word_slot_at_base(word_pos: i16, base: U256) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int16(word_pos));
    preimage[32..64].copy_from_slice(&base.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// The `ticks(tick)` mapping slot against an explicit mapping base
/// (`mapping(int24 => TickInfo)` at `base`) — V4 pools pass the pool's
/// `S_state+4` here.
///
/// `keccak256(abi.encode(int24 tick, uint256 base))`: the int24 is
/// sign-extended to 32 bytes (two's-complement), the base is BE-padded.
#[must_use]
pub fn tick_mapping_slot_at_base(tick: i32, base: U256) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int24(tick));
    preimage[32..64].copy_from_slice(&base.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Recover the tick index whose `ticks(tick)` slot equals `slot` — the
/// preimage side of [`cl_tick_mapping_slot`].
///
/// A journalled tick slot IS a keccak preimage, so the index is recovered by
/// hashing `tick_spacing`-aligned candidates (initialized ticks are always
/// on the spacing grid) inside `window` raw ticks of each anchor. Bounded
/// (≈ `2·window / tick_spacing` keccaks over the anchors) and EXACT — tick
/// indices are discrete, so a preimage hit IS the index, and an unmatched
/// slot returns `None` rather than fabricating a tick.
///
/// `anchors` are the pool's known current ticks (typically the pre-tx and
/// post-tx current tick); `window` is a raw-tick half-width.
#[must_use]
pub fn recover_cl_tick_from_slot(
    slot: U256,
    layout: ClSlotLayout,
    tick_spacing: i32,
    anchors: &[i32],
    window: i32,
) -> Option<i32> {
    debug_assert!(tick_spacing > 0, "V3 tick_spacing is positive");
    let spacing = i64::from(tick_spacing.max(1));
    let window = i64::from(window);
    for &anchor in anchors {
        let anchor = i64::from(anchor);
        let low = (anchor - window).div_euclid(spacing);
        let high = (anchor + window).div_euclid(spacing);
        for grid in low..=high {
            let Ok(tick) = i32::try_from(grid * spacing) else {
                continue;
            };
            if tick_mapping_slot_at_base(tick, U256::from(layout.ticks_mapping_slot())) == slot {
                return Some(tick);
            }
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────
// Shared bit-width helpers
// ─────────────────────────────────────────────────────────────────────────

/// A word with the low `n` bits set (0 < n <= 256), rest zero. All tracked
/// field widths in use are byte-aligned (128/184/224/256); the general path
/// keeps const-eval honest for any `n`.
#[must_use]
pub const fn low_bits_mask(n: u32) -> B256 {
    let mut out = [0u8; 32];
    let full_bytes = (n / 8) as usize;
    let mut i = 0usize;
    while i < 32 {
        // big-endian: the low bytes sit at the tail of the array.
        if (32 - full_bytes) <= i {
            out[i] = 0xff;
        }
        i += 1;
    }
    B256::new(out)
}
