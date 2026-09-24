//! The `slot_layout` table — the ONE source both storage directions consume:
//! *pack* (engine typed state → on-chain words, as the divergence probe /
//! sim-anchor projection do) and *decode* (journalled words → typed pool
//! post-state, as the journal extractors do).
//!
//! Engine-level claims:
//! - A packed V2 reserves word is the low-224-bit `reserve0 | reserve1`
//!   layout with the untracked timestamp zeroed, and decodes back exactly;
//!   the timestamp the engine does not carry reads through decode.
//! - The layout-generic CL mapping-slot helpers agree with the per-family
//!   forward fns (Uniswap V3 base 5 / Pancake V3 base 6) and with a
//!   hand-built keccak preimage (negative ticks sign-extend).
//! - A per-tick word decodes `liquidityGross | liquidityNet` (two's-
//!   complement int128 in the high half) for both signs of net.
//! - `recover_cl_tick_from_slot` recovers the tick index of a journalled
//!   ticks(tick) slot by hashing `tick_spacing`-aligned candidates around
//!   the anchor ticks — and returns `None` for slots outside every window.
#![expect(clippy::unwrap_used)]

use alloy::primitives::{aliases::U112, keccak256, B256, U256};
use degenbot_pools::slot_layout::{
    cl_liquidity_tracked_word, cl_slot0_tracked_word, cl_tick_bitmap_word_slot,
    cl_tick_mapping_slot, decode_tick_word, decode_v2_reserves_word, low_bits_mask,
    pack_v2_reserves_word, recover_cl_tick_from_slot, tick_mapping_slot_at_base, tick_word_of,
    CL_LIQUIDITY_TRACKED_MASK, CL_SLOT0_TRACKED_MASK, CL_TICK_WORD_MASK, V2_RESERVES_SLOT,
    V2_RESERVES_TRACKED_MASK,
};
use degenbot_pools::{v3_pancakeswap_storage_slots, v3_storage_slots, ClSlotLayout};

// Independent trusted literals (not computed by any pack helper here): the
// reserves pair from the bot fixture, spelled out as bytes.
const R0: u128 = 0x6a68_0ee7_0000_0000_00fd_e867;
const R1: u128 = 0x7811_7bf0_c05b;

#[test]
fn v2_reserves_word_packs_low_224_bits_with_zeroed_timestamp() {
    let word = pack_v2_reserves_word(U112::from(R0), U112::from(R1));
    // reserve1 lands at bits 112..224, reserve0 in the low 112 (28 hex
    // digits per 112-bit field), high 32 (blockTimestampLast) zero —
    // spelled out independently.
    let expected = U256::from_str_radix(&format!("{R1:028x}{R0:028x}"), 16).unwrap();
    assert_eq!(U256::from_be_bytes(word.0), expected);
    assert_eq!(V2_RESERVES_SLOT, 8, "V2 reserves slot is 8");
}

#[test]
fn v2_reserves_decode_splits_fields_and_reads_timestamp() {
    // Pinned on-chain-shaped word with a NONZERO timestamp high half — the
    // journal (or the chain) carries the ts the engine does not track.
    let ts: u128 = 0x652a_b3c0;
    let word64: U256 = U256::from_str_radix(&format!("{ts:08x}{R1:028x}{R0:028x}"), 16).unwrap();
    let parts = decode_v2_reserves_word(word64);
    assert_eq!(parts.reserve0, U112::from(R0));
    assert_eq!(parts.reserve1, U112::from(R1));
    assert_eq!(parts.block_timestamp_last, u32::try_from(ts).unwrap());
}

#[test]
fn cl_slot0_and_liquidity_pack_zeroed_untracked_bits() {
    // slot0: sqrtP in the low 160 bits, tick's 24-bit two's-complement
    // pattern at bits 160..184, everything else zero (negative tick).
    let sqrt = U256::from(1u128) << 96;
    let word = cl_slot0_tracked_word(sqrt, -5010);
    let as_u: U256 = U256::from_be_bytes(word.0);
    let tick_u = (as_u >> 160) & U256::from(0x00ff_ffffu32);
    assert_eq!(tick_u, U256::from((-5010i32).cast_unsigned() & 0x00ff_ffff));
    let mask160 = (U256::from(1u128) << 160u32) - U256::from(1u128);
    assert_eq!(as_u & mask160, sqrt);
    assert_eq!(as_u >> 184, U256::ZERO, "untracked high bits zeroed");

    // liquidity: uint128, high half zero.
    let liq = 0x0000_0000_006b_5d49_e99f_8835u128;
    let wl = U256::from_be_bytes(cl_liquidity_tracked_word(liq).0);
    assert_eq!(wl, U256::from(liq));
    assert_eq!(wl >> 128, U256::ZERO);
}

#[test]
fn cl_mapping_slot_helpers_agree_with_family_forward_fns_and_preimage() {
    const T: i32 = -100;
    assert_eq!(
        cl_tick_mapping_slot(ClSlotLayout::UniswapV3, T),
        v3_storage_slots::v3_tick_mapping_slot(T),
        "Uniswap V3 ticks base 5"
    );
    assert_eq!(
        cl_tick_mapping_slot(ClSlotLayout::PancakeV3, T),
        v3_pancakeswap_storage_slots::pancake_v3_tick_mapping_slot(T),
        "Pancake V3 ticks base 6 — distinct from the Uni slot"
    );
    assert_ne!(
        cl_tick_mapping_slot(ClSlotLayout::UniswapV3, T),
        cl_tick_mapping_slot(ClSlotLayout::PancakeV3, T)
    );

    // The base-generic forward fn against a hand-built abi.encode(int24,
    // uint256 base) preimage: negative tick sign-extends to 32 bytes.
    let base = U256::from(5u64);
    let mut input = [0u8; 64];
    input[..29].fill(0xff);
    input[29..32].copy_from_slice(&[0xff, 0xff, 0x9c]); // -100 low 24 bits
    input[32..64].copy_from_slice(&base.to_be_bytes::<32>());
    let expected = U256::from_be_bytes(keccak256(input).0);
    assert_eq!(tick_mapping_slot_at_base(T, base), expected);
}

#[test]
fn cl_tick_bitmap_word_slot_pins_bitmap_bases_6_and_7() {
    // Independent keccak preimage per the `mapping(int16 => uint256)` layout:
    // the sign-extended word then the mapping base. V3's `_tickBitmap` is
    // base 6; Pancake's is base 7. Pinning the exact expected slots catches a
    // wrong base (hashing the ticks base 5 would read a zero slot on-chain).
    let expected = |word_pos: i16, base: u64| {
        let mut input = [0u8; 64];
        if word_pos < 0 {
            input[..30].fill(0xff);
            input[30..32].copy_from_slice(&word_pos.to_be_bytes());
        } else {
            input[30..32].copy_from_slice(&word_pos.to_be_bytes());
        }
        input[32..64].copy_from_slice(&U256::from(base).to_be_bytes::<32>());
        U256::from_be_bytes(keccak256(input).0)
    };
    assert_eq!(
        cl_tick_bitmap_word_slot(ClSlotLayout::UniswapV3, -8),
        expected(-8, 6)
    );
    assert_eq!(
        cl_tick_bitmap_word_slot(ClSlotLayout::PancakeV3, 33),
        expected(33, 7)
    );
    // The bitmap mapping is distinct from the ticks mapping (bases 5/6).
    assert_ne!(
        cl_tick_bitmap_word_slot(ClSlotLayout::UniswapV3, 0),
        tick_mapping_slot_at_base(0, U256::from(5))
    );
}

#[test]
fn tick_word_round_trips_gross_and_net_for_both_signs() {
    // Independent pinned words: net=-500 as int128 two's-complement
    // (u128::MAX - 499) in the high half, net=+300 plain.
    let gross = U256::from(1_000u64);
    let neg = gross | (U256::from(u128::MAX - 499) << 128);
    let (g, n) = decode_tick_word(neg);
    assert_eq!(g, 1_000u128);
    assert_eq!(n, -500i128);
    assert_eq!(
        tick_word_of(1_000, -500),
        B256::from(neg.to_be_bytes::<32>())
    );
    assert_eq!(tick_word_of(1_000, -500), tick_word_of(1_000, -500));

    let pos = gross | (U256::from(300u128) << 128);
    let (g2, n2) = decode_tick_word(pos);
    assert_eq!(g2, 1_000u128);
    assert_eq!(n2, 300i128);
    assert!(
        CL_TICK_WORD_MASK == low_bits_mask(256),
        "tick word fully tracked"
    );
}

#[test]
fn recover_cl_tick_from_slot_hashes_spacing_aligned_candidates() {
    let spacing = 60;
    // A touched tick at +600, anchors known to the engine's pre-tx tick (0)
    // and post-tx tick (600) — recovered exactly, from the canonical grid.
    let slot = cl_tick_mapping_slot(ClSlotLayout::UniswapV3, 600);
    let got = recover_cl_tick_from_slot(
        slot,
        ClSlotLayout::UniswapV3,
        spacing,
        &[0, 600],
        spacing * 64,
    );
    assert_eq!(got, Some(600), "tick index recovered from preimage");

    // A NEGATIVE touched tick across the zero boundary.
    let slot = cl_tick_mapping_slot(ClSlotLayout::UniswapV3, -2880);
    let got = recover_cl_tick_from_slot(
        slot,
        ClSlotLayout::UniswapV3,
        spacing,
        &[-2880, 0],
        spacing * 64,
    );
    assert_eq!(got, Some(-2880));

    // A slot outside every candidate window (or not on the grid at all)
    // must NOT be guessed.
    let stray = U256::from_be_bytes(keccak256([7u8; 64]).0);
    let got = recover_cl_tick_from_slot(stray, ClSlotLayout::UniswapV3, spacing, &[0], spacing * 2);
    assert_eq!(got, None, "unmatched slots are never fabricated into ticks");
}

#[test]
fn recover_cl_tick_uses_the_layouts_ticks_base() {
    // Same tick indexes to a different slot per fork layout.
    let uni = cl_tick_mapping_slot(ClSlotLayout::UniswapV3, -180);
    let pk = cl_tick_mapping_slot(ClSlotLayout::PancakeV3, -180);
    assert_eq!(
        recover_cl_tick_from_slot(pk, ClSlotLayout::PancakeV3, 60, &[0], 60 * 8),
        Some(-180)
    );
    assert_ne!(uni, pk);
    // The Uni slot decoded through pancake would misrecover (wrong base).
    assert_eq!(
        recover_cl_tick_from_slot(pk, ClSlotLayout::UniswapV3, 60, &[0], 60 * 8),
        None
    );
}

#[test]
fn tracked_masks_cover_declared_field_widths() {
    let masked = |m: B256| U256::from_be_bytes(m.0);
    // low n bits set: bit n-1 is the top set bit and nothing above.
    let width = |m: B256, n: u32| {
        assert_eq!(masked(m) >> n, U256::ZERO, "nothing above bit {n}");
        assert_eq!(
            masked(m) >> (n - 1) & U256::from(1u64),
            U256::from(1u64),
            "top bit set"
        );
    };
    width(V2_RESERVES_TRACKED_MASK, 224);
    width(CL_SLOT0_TRACKED_MASK, 184);
    width(CL_LIQUIDITY_TRACKED_MASK, 128);
    assert_eq!(masked(CL_TICK_WORD_MASK), U256::MAX);
    // the shared low-bits helper reproduces each declared width mask
    for (m, n) in [
        (V2_RESERVES_TRACKED_MASK, 224u32),
        (CL_SLOT0_TRACKED_MASK, 184),
        (CL_LIQUIDITY_TRACKED_MASK, 128),
    ] {
        assert_eq!(U256::from_be_bytes(m.0), masked(low_bits_mask(n)));
    }
}
