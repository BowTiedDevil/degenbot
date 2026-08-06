#![allow(
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]
#![allow(clippy::identity_op, clippy::cast_possible_truncation)]
//! PancakeSwap V3 storage-slot encoders — fork-aware engine typed state →
//! EVM slot words.
//!
//! Parallel to [`super::v3_storage_slots`] but for the **PancakeSwap V3 fork**,
//! which has a DIFFERENT storage layout than canonical Uniswap V3 (surfaced by
//! the Tier-3 pancake oracle, ergo task `W32CAU`):
//!
//! ```text
//!                    Uniswap V3   PancakeSwap V3
//!   slot0 (packed)     1 word        2 words
//!   liquidity@         slot 4        slot 5
//!   ticks base@        slot 5        slot 6
//!   tickBitmap base@   slot 6        slot 7
//! ```
//!
//! The fork's `Slot0` struct packs `feeProtocol` as a `uint32` (2× `uint16`,
//! `PROTOCOL_FEE_SP = 65536`) instead of Uniswap's `uint8`. Combined with the
//! price/tick/observation fields (160+24+16+16+16 = 232 bits), the struct
//! exceeds a single word, so it spans TWO storage words: `slot0` word 0 holds
//! `sqrtPrice | tick | observation{Index,Cardinality,CardinalityNext}` (232
//! bits) and `slot0` word 1 holds `feeProtocol | unlocked << 32`. Every
//! following slot shifts by one (liquidity@5, ticks@6, tickBitmap@7).
//!
//! The Uniswap `v3_storage_slots` encoders (liquidity@4, ticks@5, tickBitmap@6,
//! one-word slot0) would therefore MISREAD a PancakeSwap pool.
//!
//! ## Production sync note
//!
//! Production V3 pool state is advanced from on-chain **events** (the Swap
//! events decoded by `degenbot_decoders::v3_pancakeswap_swap_decoder`) and
//! initial seeding uses ABI `slot0()`/`liquidity()` `eth_call`s — neither reads
//! raw storage slots, so live sync is correct for pancake pools regardless of
//! layout. These fork-aware encoders exist for the (a) Tier-3 oracle seeding
//! path, and (b) any direct slot-based seed/serve a standalone consumer or a
//! future serving seam performs on a pancake pool — which MUST use this fork
//! layout, never the Uniswap constants.

use alloy::primitives::{keccak256, U256};

use super::v3_storage_slots::{sign_extend_int16, sign_extend_int24};

/// `slot0` word 0 storage slot number (PancakeSwap — the packed price/tick/
/// observation word; identical field placement to Uniswap's `slot0` low 232
/// bits, but `feeProtocol`/`unlocked` live in **word 1** here).
pub const PANCAKE_V3_SLOT0_WORD0_SLOT: u64 = 0;
/// `slot0` word 1 storage slot number (PancakeSwap — `feeProtocol` (uint32,
/// low 32 bits) | `unlocked` (bool, bit 32)).
pub const PANCAKE_V3_SLOT0_WORD1_SLOT: u64 = 1;
/// `liquidity` storage slot number (`uint128`, high 128 bits zero) —
/// PancakeSwap `@5` (Uniswap `@4`).
pub const PANCAKE_V3_LIQUIDITY_SLOT: u64 = 5;
/// `ticks` mapping base slot (`mapping(int24 => TickInfo)`) — PancakeSwap `@6`.
pub const PANCAKE_V3_TICKS_MAPPING_SLOT: u64 = 6;
/// `tickBitmap` mapping base slot (`mapping(int16 => uint256)`) — PancakeSwap
/// `@7`.
pub const PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT: u64 = 7;

/// Encode the PancakeSwap `slot0` word 1 (the fork-only second word): the
/// `uint32 feeProtocol` in the low 32 bits OR-ed with a `bool unlocked` flag at
/// bit 32.
///
/// This is the field that splits the fork's `Slot0` across two words (Uniswap
/// packs `uint8 feeProtocol` + `unlocked` into word 0 bit 232..241). Callers
/// seed word 0 with `encode_v3_slot0_fresh` (the price/tick/observation fields
/// are byte-identical; the bit-240 `unlocked` that helper sets falls in unused
/// padding, so it is harmless).
#[must_use]
pub fn encode_pancake_v3_slot0_word1(fee_protocol: u32, unlocked: bool) -> U256 {
    let mut word = U256::from(fee_protocol);
    if unlocked {
        word |= U256::from(1u64) << 32;
    }
    word
}

/// Compute the storage slot for a single PancakeSwap V3 `ticks(tick)` entry:
/// `keccak256(abi.encode(int24 tick) . abi.encode(uint256(6)))` (base slot 6,
/// unlike Uniswap's 5).
#[must_use]
pub fn pancake_v3_tick_mapping_slot(tick: i32) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int24(tick));
    preimage[32..64]
        .copy_from_slice(&U256::from(PANCAKE_V3_TICKS_MAPPING_SLOT).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Compute the storage slot for a single PancakeSwap V3 `tickBitmap(word)`
/// entry: `keccak256(abi.encode(int16 word) . abi.encode(uint256(7)))` (base
/// slot 7, unlike Uniswap's 6).
#[must_use]
pub fn pancake_v3_tick_bitmap_word_slot(word_pos: i16) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int16(word_pos));
    preimage[32..64]
        .copy_from_slice(&U256::from(PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v3_storage_slots::{v3_tick_bitmap_word_slot, v3_tick_mapping_slot};

    #[test]
    fn fork_slots_divergence_from_uniswap_layout() {
        // The whole point: the fork's liquidity/ticks/tickBitmap base slots are
        // each +1 vs Uniswap (slot0 spans two words). A caller seeding a pancake
        // pool with the Uniswap encoders would write to the WRONG slots.
        assert_eq!(PANCAKE_V3_LIQUIDITY_SLOT, 5);
        assert_eq!(PANCAKE_V3_TICKS_MAPPING_SLOT, 6);
        assert_eq!(PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT, 7);
        assert_ne!(
            PANCAKE_V3_TICKS_MAPPING_SLOT,
            crate::v3_storage_slots::V3_TICKS_MAPPING_SLOT
        );
        assert_ne!(
            PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT,
            crate::v3_storage_slots::V3_TICK_BITMAP_MAPPING_SLOT
        );

        // The keccak-derived mapping slots must also differ for the same key:
        // the fork uses base 6/7, Uniswap 5/6.
        assert_ne!(pancake_v3_tick_mapping_slot(0), v3_tick_mapping_slot(0));
        assert_ne!(
            pancake_v3_tick_bitmap_word_slot(0),
            v3_tick_bitmap_word_slot(0)
        );
    }

    #[test]
    fn slot0_word1_packs_fee_protocol_and_unlocked() {
        // feeProtocol (32b) low, unlocked bool at bit 32.
        assert_eq!(encode_pancake_v3_slot0_word1(0, false), U256::ZERO);
        assert_eq!(
            encode_pancake_v3_slot0_word1(0, true),
            U256::from(1u64) << 32
        );
        // PROTOCOL_FEE_SP = 65536 = fee in 1/65536ths: 65536 * 1 = 0x10000 kept
        // in the low 32 bits.
        assert_eq!(
            encode_pancake_v3_slot0_word1(65_536, true),
            U256::from(65_536u64) | (U256::from(1u64) << 32)
        );
    }
}
