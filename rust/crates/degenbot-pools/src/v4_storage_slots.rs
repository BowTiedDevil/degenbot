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
//! V4 on-chain storage-slot encoders — engine typed state → PoolManager slots.
//!
//! Pure functions (no revm, no pyo3) that pack the engine's `V4PoolState`
//! typed fields into the EXACT storage layout the canonical V4 `PoolManager`
//! singleton bytecode reads during a real `Pool.swap()` (via `unlock()`).
//! V4 twin of [`crate::v3_storage_slots`]; see that module's docs for the
//! "test oracle can seed the swap-math slots the production seam could not
//! serve" rationale (ergo epic `UP5NH6`, task `NH6NLJ`).
//!
//! ## V4 storage layout (PoolManager singleton, v4-core `Pool.State`)
//!
//! `_pools` mapping at top-level slot **6**. Per-pool `Pool.State` base slot:
//! `S_state = keccak256(abi.encode(poolId, uint256(6)))` where
//! `poolId = keccak256(abi.encode(poolKey))` and
//! `poolKey = (currency0, currency1, fee, tickSpacing, hooks)`.
//!
//! `Pool.State` field order (v4-core `Pool.sol`), each a separate storage slot:
//!
//! | Offset | Field                       | Engine field                |
//! |-------:|-----------------------------|-----------------------------|
//! | +0     | `Slot0 slot0` (packed)      | `sqrt_price_x96`, `tick`, `protocol_fee`, lp `fee` |
//! | +1     | `feeGrowthGlobal0X128`      | NOT tracked (zero-filled)   |
//! | +2     | `feeGrowthGlobal1X128`      | NOT tracked (zero-filled)   |
//! | +3     | `liquidity` (`uint128`)     | `liquidity`                 |
//! | +4     | `ticks` mapping base        | `tick_data` (gross+net); per-tick = `keccak256(abi.encode(int24 tick) . abi.encode(uint256(S_state+4)))` |
//! | +5     | `tickBitmap` mapping base   | synth from `tick_data` keys; per-word = `keccak256(abi.encode(int16 word) . abi.encode(uint256(S_state+5)))` |
//! | +6     | `positions` mapping         | NOT tracked (not read by swap) |
//!
//! `Slot0` bit layout (v4-core `types/Slot0.sol`, LSB-first):
//! `[0..160)` `uint160 sqrtPriceX96` · `[160..184)` `int24 tick` ·
//! `[184..208)` `uint24 protocolFee` (low 12 bits = 0→1 fee, high 12 = 1→0 fee — matches the engine's `protocol_fee: u32` packing) ·
//! `[208..232)` `uint24 lpFee` (the pool's static fee; the swap reads this to charge the LP portion) ·
//! `[232..256)` unused (24 bits).
//!
//! NOTE: V4 has no `unlocked` flag in `Slot0` — the PoolManager's `locked`
//! state is in TRANSIENT storage (EIP-1153 `TSTORE`/`TLOAD`); the Tier-3b
//! end-to-end oracle (task `2LTKVO`) handles the `unlock()` entry + transient
//! seed, NOT these encoders. V4 `swap()` also reads `CurrencyDelta` (transient)
//! during settle — written by the swap itself, no pre-seed needed.

use alloy::primitives::{keccak256, Address, B256, U256};

use crate::v3_storage_slots::{compute_v3_tick_bitmap_word, sign_extend_int16, sign_extend_int24};
use crate::v4_state::V4PoolKey;

/// The PoolManager singleton's top-level `_pools` mapping base slot. Per-pool
/// `Pool.State` base = `keccak256(abi.encode(poolId, uint256(V4_POOLS_MAPPING_SLOT)))`.
pub const V4_POOLS_MAPPING_SLOT: u64 = 6;

/// Mask selecting the low 160 bits of a `U256` (the V4 `Slot0` `uint160
/// sqrtPriceX96` field width — identical to V3).
const MASK_160: U256 = U256::from_limbs([u64::MAX, u64::MAX, u64::MAX & 0xFF, 0]);
/// Mask selecting the low 128 bits of a `U256` (the V4 `liquidity uint128` +
/// `int128 liquidityNet` field width — identical to V3).
const MASK_128: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);

/// The decoded/encodable packed fields of a V4 `Slot0` storage word.
///
/// Mirrors the v4-core `types/Slot0.sol` bit layout (see module docs). The
/// 24-bit `protocol_fee` packs the two 12-bit direction fees as
/// `get_zero_for_one_fee | (get_one_for_zero_fee << 12)` — matching the engine's
/// `V4PoolState.protocol_fee: u32` packing exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct V4Slot0Parts {
    /// `uint160 sqrtPriceX96` at bits `[0..160)` (Q64.96).
    pub sqrt_price_x96: U256,
    /// `int24 tick` at bits `[160..184)`.
    pub tick: i32,
    /// `uint24 protocolFee` at bits `[184..208)` (low 12 = 0→1, high 12 = 1→0).
    pub protocol_fee: u32,
    /// `uint24 lpFee` at bits `[208..232)` (the pool's static fee; V4 `swap()`
    /// reads this to charge the LP portion of the swap fee).
    pub lp_fee: u32,
}

/// Compute the V4 `poolId` for a pool key: `keccak256(abi.encode(poolKey))`
/// where `poolKey = (address currency0, address currency1, uint24 fee,
/// int24 tickSpacing, address hooks)` (each ABI-padded to 32 bytes).
///
/// `poolId` is the per-pool identity the PoolManager indexes `Pool.State` by;
/// feed it to [`v4_pool_state_base_slot`] to derive the per-pool storage base.
#[must_use]
pub fn v4_pool_id(pool_key: &V4PoolKey) -> B256 {
    // abi.encode(address, address, uint24, int24, address) = 5 * 32 bytes.
    // Address → left-padded (low 20 bytes hold the address).
    // uint24 → right-aligned (low 3 bytes hold the fee).
    // int24 → sign-extended (24 bits → 256-bit two's complement; sign bit at 23).
    let mut preimage = [0u8; 160];
    write_address_padded(&mut preimage[0..32], pool_key.currency0);
    write_address_padded(&mut preimage[32..64], pool_key.currency1);
    write_uint24_padded(&mut preimage[64..96], pool_key.fee);
    preimage[96..128].copy_from_slice(&sign_extend_int24(pool_key.tick_spacing));
    write_address_padded(&mut preimage[128..160], pool_key.hooks);
    keccak256(preimage)
}

/// Compute the V4 `Pool.State` base storage slot for a pool:
/// `S_state = keccak256(abi.encode(poolId, uint256(6)))`.
///
/// All per-pool slots are `S_state + offset` (offset 0=`Slot0`, 3=`liquidity`,
/// 4=`ticks` mapping base, 5=`tickBitmap` mapping base — see module docs).
#[must_use]
pub fn v4_pool_state_base_slot(pool_id: B256) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(pool_id.as_slice());
    preimage[32..64].copy_from_slice(&U256::from(V4_POOLS_MAPPING_SLOT).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// The V4 `Slot0` storage slot (`Pool.State` offset +0): `S_state + 0`.
#[must_use]
pub fn v4_slot0_slot(pool_state_base: U256) -> U256 {
    pool_state_base
}

/// The V4 `liquidity` storage slot (`Pool.State` offset +3): `S_state + 3`.
#[must_use]
pub fn v4_liquidity_slot(pool_state_base: U256) -> U256 {
    pool_state_base + U256::from(3u64)
}

/// The V4 `ticks` mapping base slot (`Pool.State` offset +4): `S_state + 4`.
#[must_use]
pub fn v4_ticks_mapping_base_slot(pool_state_base: U256) -> U256 {
    pool_state_base + U256::from(4u64)
}

/// The V4 `tickBitmap` mapping base slot (`Pool.State` offset +5): `S_state + 5`.
#[must_use]
pub fn v4_tick_bitmap_mapping_base_slot(pool_state_base: U256) -> U256 {
    pool_state_base + U256::from(5u64)
}

/// Compute the V4 `ticks(tick)` storage slot:
/// `keccak256(abi.encode(int24 tick) . abi.encode(uint256(S_state+4)))`.
#[must_use]
pub fn v4_tick_mapping_slot(tick: i32, pool_state_base: U256) -> U256 {
    let ticks_base = v4_ticks_mapping_base_slot(pool_state_base);
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int24(tick));
    preimage[32..64].copy_from_slice(&ticks_base.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Compute the V4 `tickBitmap(word)` storage slot:
/// `keccak256(abi.encode(int16 word) . abi.encode(uint256(S_state+5)))`.
#[must_use]
pub fn v4_tick_bitmap_word_slot(word_pos: i16, pool_state_base: U256) -> U256 {
    let bitmap_base = v4_tick_bitmap_mapping_base_slot(pool_state_base);
    let mut preimage = [0u8; 64];
    preimage[0..32].copy_from_slice(&sign_extend_int16(word_pos));
    preimage[32..64].copy_from_slice(&bitmap_base.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

/// Encode the V4 `Slot0` storage word (offset +0) from its packed fields.
///
/// Bit layout per v4-core `types/Slot0.sol` (see module docs). The `int24
/// tick` is placed as exactly 24 bits at `[160..184)` (no sign-extension
/// across the tail), so `protocol_fee` + `lp_fee` keep their positions for
/// negative ticks — required for a byte-exact round-trip of a real on-chain
/// `Slot0` value.
#[must_use]
pub fn encode_v4_slot0(parts: V4Slot0Parts) -> U256 {
    let sqrt_price = parts.sqrt_price_x96 & MASK_160;
    #[allow(clippy::cast_sign_loss)]
    let tick_field = U256::from(parts.tick as u32 & 0x00FF_FFFF);
    let protocol_fee_field = U256::from(parts.protocol_fee & 0x00FF_FFFF);
    let lp_fee_field = U256::from(parts.lp_fee & 0x00FF_FFFF);
    sqrt_price | (tick_field << 160) | (protocol_fee_field << 184) | (lp_fee_field << 208)
}

/// Decode a V4 `Slot0` storage word into its packed fields (inverse of
/// [`encode_v4_slot0`]). Used by the mainnet round-trip test + the Tier-3b
/// seeding layer to read a pinned on-chain `Slot0` for fixture derivation.
#[must_use]
pub fn decode_v4_slot0(word: U256) -> V4Slot0Parts {
    let sqrt_price_x96 = word & MASK_160;
    let tick_field = (word >> 160u32) & U256::from(0x00FF_FFFFu32);
    let protocol_fee = ((word >> 184u32) & U256::from(0x00FF_FFFFu32)).to::<u32>();
    let lp_fee = ((word >> 208u32) & U256::from(0x00FF_FFFFu32)).to::<u32>();
    #[allow(clippy::cast_possible_wrap)]
    let tick_u32 = tick_field.to::<u32>();
    let tick = if (tick_u32 & 0x800000) != 0 {
        (tick_u32 as i32) - (1 << 24)
    } else {
        tick_u32 as i32
    };
    V4Slot0Parts {
        sqrt_price_x96,
        tick,
        protocol_fee,
        lp_fee,
    }
}

/// Encode the V4 `liquidity` storage word (offset +3): `uint128` in the low
/// 128 bits, high 128 bits zero (identical layout to V3).
#[must_use]
pub fn encode_v4_liquidity_slot(liquidity: u128) -> U256 {
    U256::from(liquidity) & MASK_128
}

/// Encode the V4 `TickInfo` packed storage word (the `ticks(tick)` slot value):
/// `uint128 liquidityGross` LOW 128 | `int128 liquidityNet` HIGH 128 (identical
/// layout to V3 — V4's `Pool.TickInfo` has the same gross/net packing (gross
/// declared first → low bits); the `feeGrowthOutside` tail occupies slot+1/+2
/// and is zero-filled by the seeding layer, never read by the swap math).
#[must_use]
pub fn encode_v4_tick_info_slot(tick_info: &crate::TickInfo) -> U256 {
    crate::v3_storage_slots::encode_v3_tick_info_slot(tick_info)
}

/// Compute the V4 `tickBitmap` word value for one word. Delegates to the V3
/// implementation — the bitmask packing (`bit (compressed & 0xFF)` per
/// initialized tick) is identical between V3 and V4.
#[must_use]
pub fn compute_v4_tick_bitmap_word(compressed_ticks_in_word: &[i32]) -> U256 {
    compute_v3_tick_bitmap_word(compressed_ticks_in_word)
}

// ───────── internal helpers ─────────

/// Write an `address` into a 32-byte ABI-encoded slot (left-padded: low 20
/// bytes hold the address, high 12 bytes zero — `abi.encode(address)`).
fn write_address_padded(slot: &mut [u8], addr: Address) {
    debug_assert_eq!(slot.len(), 32);
    slot.fill(0);
    slot[12..32].copy_from_slice(addr.as_slice());
}

/// Write a `uint24` into a 32-byte ABI-encoded slot (right-aligned: low 3
/// bytes hold the value, high 29 bytes zero — `abi.encode(uint24)`).
fn write_uint24_padded(slot: &mut [u8], value: u32) {
    debug_assert_eq!(slot.len(), 32);
    slot.fill(0);
    let be = value.to_be_bytes(); // [b0, b1, b2, b3]
    slot[29..32].copy_from_slice(&be[1..4]);
}

// ─────────────────────────────────────────────────────────────────────────
// Tests — mainnet-pinned (cast storage) + cast-keccak/abi-encode oracles.
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;
    use std::str::FromStr;

    // Pinned mainnet triple (PoolManager singleton 0x000000000004444c5dc75cB358380D2e3De08A90),
    // from docs/architecture/in_process_sim_served_slots.md.
    const V4_PINNED_POOL_ID: &str =
        "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27";
    const V4_PINNED_S_STATE: &str =
        "0xda8cac368d67cd2f2d8aaa5cc531768e0fa3b1d205c5c5de60da078e1f59bdfc";
    const V4_PINNED_SLOT0_HEX: &str =
        "0x0000000001f407d07dfcef1000000000000000000002d6f3af955d0e737f29c9";
    const V4_PINNED_LIQUIDITY_HEX: &str =
        "0x0000000000000000000000000000000000000000000000000130470b58738b1e";

    // Cast-keccak oracle (offline `cast keccak`, NOT RPC) for S_state from the
    // pinned poolId:
    //   keccak256(abi.encode(bytes32 poolId, uint256(6)))
    // cast computed 0xda8cac…bdfc — matches the pinned mainnet S_state.
    const V4_S_STATE_CAST: &str = V4_PINNED_S_STATE;

    // Cast-keccak oracle: V4 per-tick slot for tick=-887270 at the pinned
    // pool's ticks base (S_state + 4):
    //   keccak256(abi.encode(int24(-887270), uint256(S_state+4)))
    //   where S_state+4 = 0xda8cac…1f59be00.
    const V4_TICK_SLOT_CAST: &str =
        "0xcc9c250da8f54017fe4b8d73e8fb283f749b91c6e2c874540fb2cf552132d564";
    // Cast-keccak oracle: V4 per-bitmap-word slot for word=-1 at the pinned
    // pool's tickBitmap base (S_state + 5):
    //   keccak256(abi.encode(int16(-1), uint256(S_state+5)))
    const V4_BITMAP_SLOT_CAST: &str =
        "0xed2e3abc3e206b9536ccabca3cd74488c99fd2158d490e18f1c19e4b090ec726";

    // Cast-keccak oracle for `pool_id` of a self-constructed key
    // (currency0=0x..0, currency1=0x..1, fee=3000, tickSpacing=60, hooks=0x..0):
    //   keccak256(abi.encode(address,address,uint24,int24,address))
    const V4_POOL_ID_CONSTRUCTED_KEY_CAST: &str =
        "0x9e4ee04ba77ddedb315b9ed859fa005bace0c37b85576fd5f1015e0941519077";

    fn b256(hex: &str) -> B256 {
        B256::from_str(hex).expect("valid 0x-prefixed hex B256")
    }
    fn u256(hex: &str) -> U256 {
        U256::from_str_radix(hex.trim_start_matches("0x"), 16).expect("valid hex U256")
    }

    /// `v4_pool_state_base_slot(pinned poolId) == pinned mainnet S_state`,
    /// verified independently against the offline `cast keccak` oracle. This is
    /// the S_state derivation — the keccak preimage is `abi.encode(poolId, 6)`.
    #[test]
    fn v4_pool_state_base_slot_matches_pinned_mainnet_and_cast_oracle() {
        let pool_id = b256(V4_PINNED_POOL_ID);
        let s_state = v4_pool_state_base_slot(pool_id);
        assert_eq!(
            s_state,
            u256(V4_PINNED_S_STATE),
            "S_state must match the pinned mainnet value"
        );
        assert_eq!(
            s_state,
            u256(V4_S_STATE_CAST),
            "S_state must match the offline cast keccak oracle"
        );
    }

    /// `v4_pool_id` of a constructed key matches the offline cast keccak
    /// oracle — proves the ABI-encoded poolKey preimage (address×2 + uint24 +
    /// int24 + address, each padded to 32 bytes) is byte-exact.
    #[test]
    fn v4_pool_id_matches_cast_keccak_for_constructed_key() {
        let pool_key = V4PoolKey {
            currency0: alloy::primitives::address!("0x0000000000000000000000000000000000000000"),
            currency1: alloy::primitives::address!("0x0000000000000000000000000000000000000001"),
            fee: 3000,
            tick_spacing: 60,
            hooks: alloy::primitives::address!("0x0000000000000000000000000000000000000000"),
        };
        assert_eq!(
            v4_pool_id(&pool_key),
            b256(V4_POOL_ID_CONSTRUCTED_KEY_CAST),
            "poolId must match keccak(abi.encode(poolKey)) via offline cast"
        );
    }

    /// Decode → re-encode round-trips the pinned mainnet V4 `Slot0` byte-exact
    /// (proves the V4 `Slot0` bit layout — sqrtPrice+tick+protocolFee+lpFee —
    /// against a real on-chain value).
    #[test]
    fn v4_slot0_round_trips_pinned_mainnet_slot0() {
        let pinned = u256(V4_PINNED_SLOT0_HEX);
        let parts = decode_v4_slot0(pinned);
        assert_eq!(
            encode_v4_slot0(parts),
            pinned,
            "round-trip encode(decode(pinned)) must equal pinned byte-exact"
        );
        // The V4 `Slot0` interface guarantees the sqrtPrice lives in the low
        // 160 bits (the swap-math entry point reads exactly those).
        assert_eq!(parts.sqrt_price_x96, pinned & MASK_160);
    }

    /// V4 `Slot0` negative-tick placement: the int24 occupies exactly bits
    /// [160..184), so `protocol_fee` + `lp_fee` keep their positions for
    /// negative ticks (no tail corruption from sign extension).
    #[test]
    fn v4_slot0_negative_tick_preserves_protocol_and_lp_fee() {
        let parts = V4Slot0Parts {
            sqrt_price_x96: U256::from(0x123u32),
            tick: -887_270,
            protocol_fee: 0x00A_B000, // arbitrary non-zero (low12 + high12)
            lp_fee: 3000,
        };
        let word = encode_v4_slot0(parts);
        let decoded = decode_v4_slot0(word);
        assert_eq!(decoded.tick, -887_270);
        assert_eq!(decoded.protocol_fee, 0x00A_B000);
        assert_eq!(decoded.lp_fee, 3000);
        assert_eq!(encode_v4_slot0(decoded), word, "byte-exact round-trip");
    }

    /// The pinned mainnet V4 `liquidity` slot (offset +3) round-trips.
    #[test]
    fn v4_liquidity_slot_round_trips_pinned_mainnet() {
        let pinned = u256(V4_PINNED_LIQUIDITY_HEX);
        let liquidity_u128 = u128::try_from(pinned & MASK_128).expect("low 128 bits fit u128");
        assert_eq!(
            encode_v4_liquidity_slot(liquidity_u128),
            pinned,
            "encoded V4 liquidity must equal the pinned mainnet offset+3 word"
        );
    }

    /// V4 `Pool.State` offset arithmetic: slot0=+0, liquidity=+3,
    /// ticks-base=+4, tickBitmap-base=+5.
    #[test]
    fn v4_pool_state_offsets_are_correct() {
        let base = v4_pool_state_base_slot(b256(V4_PINNED_POOL_ID));
        assert_eq!(v4_slot0_slot(base), base);
        assert_eq!(v4_liquidity_slot(base), base + U256::from(3u64));
        assert_eq!(v4_ticks_mapping_base_slot(base), base + U256::from(4u64));
        assert_eq!(
            v4_tick_bitmap_mapping_base_slot(base),
            base + U256::from(5u64)
        );
    }

    /// The V4 `ticks(tick)` slot derivation matches the offline cast keccak
    /// oracle for tick=-887270 at the pinned pool's ticks base (S_state+4).
    #[test]
    fn v4_tick_mapping_slot_matches_cast_keccak_oracle() {
        let base = v4_pool_state_base_slot(b256(V4_PINNED_POOL_ID));
        assert_eq!(
            v4_tick_mapping_slot(-887_270, base),
            u256(V4_TICK_SLOT_CAST),
            "V4 ticks(tick) slot must equal keccak(abi.encode(int24 tick, uint256(S_state+4)))"
        );
    }

    /// The V4 `tickBitmap(word)` slot derivation matches the offline cast
    /// keccak oracle for word=-1 at the pinned pool's tickBitmap base.
    #[test]
    fn v4_tick_bitmap_word_slot_matches_cast_keccak_oracle() {
        let base = v4_pool_state_base_slot(b256(V4_PINNED_POOL_ID));
        assert_eq!(
            v4_tick_bitmap_word_slot(-1, base),
            u256(V4_BITMAP_SLOT_CAST),
            "V4 tickBitmap(word) slot must equal keccak(abi.encode(int16 word, uint256(S_state+5)))"
        );
    }

    /// V4 `TickInfo` packing is identical to V3 (gross LOW 128, net HIGH 128).
    #[test]
    fn v4_tick_info_slot_delegates_to_v3_layout() {
        use alloy::primitives::{I256, U128};
        let tick_info = crate::TickInfo {
            liquidity_gross: U128::from(7u128),
            liquidity_net: I256::try_from(-1i64).unwrap(),
            block: 0,
        };
        let word = encode_v4_tick_info_slot(&tick_info);
        assert_eq!(word & MASK_128, U256::from(7u64), "gross in low 128");
        // int128 of -1 == 2^128 - 1 (all ones in the high 128).
        assert_eq!(
            (word >> 128) & MASK_128,
            MASK_128,
            "net = -1 as int128 two's complement"
        );
    }
}
