//! The in-process-sim divergence probe — engine side.
//!
//! A pure-observation accessor used by the sim's `BotStateDb.storage_ref`
//! to quantify the gap between the **engine's event-applied typed pool
//! state** (what the solver read to derive `hop_outputs`) and the
//! **RPC-served storage state** (what the in-process revm sim reads during
//! `execute()`). The divergence on the failing V4-V3-V3 paths
//! (`CurrencyNotSettled` — selector `0x5212cba1`) is rooted in this gap;
//! this accessor answers the spike (`docs/architecture/
//! in_process_sim_served_slots.md`) checkpoint question — *do the engine's
//! scalar slots (sqrtPrice/liquidity/reserves) match the RPC at the sim
//! block?* — which picks fix path A (extend engine state) / B (shadow-RPC
//! at sim block) / C (gated serve when caught up).
//!
//! # What this is NOT
//!
//! This is **observation only**. It packs the engine's tracked fields into
//! the on-chain storage word shape (untracked bits zeroed) for comparison
//! against the RPC value; it does NOT serve anything. The naive
//! `storage_ref` serve of these slots was the **reverted bug** (K-invariant /
//! `LOK` reverts — the engine doesn't carry `feeGrowthGlobal`/`tickBitmap`/
//! per-pair token balances, so a partial serve produced intra-sim
//! inconsistency). See `bot_state_db.rs`'s historical note.
//!
//! # Scope — scalar slots only
//!
//! Covers the slots the engine carries authoritatively:
//! - **V2** pair slot 8 (`reserves`: `uint112 reserve0 | uint112 reserve1`,
//!   low 224 bits; the high-32 `blockTimestampLast` is NOT tracked → zeroed +
//!   masked out of the comparison — the divergence that matters is reserves,
//!   not the timestamp the engine doesn't store).
//! - **V3**/V4 `slot0` (`uint160 sqrtPriceX96 | int24 tick`, low 184 bits;
//!   `observationIndex`/`feeProtocol`/`unlocked` are NOT tracked →
//!   zeroed + masked).
//! - **V3**/V4 `liquidity` (`uint128`, low 128 bits; high 128 zero on-chain).
//!
//! Per-tick `ticks(tick)` (`liquidityGross | liquidityNet`, slot+0 word —
//! the engine DOES track gross/net) + the V4 `S_state` derivation are
//! **deferred**: the tick reverse-map requires `keccak256(tick . base)` per
//! tick per cold read. The scalar slots alone answer the checkpoint question
//! (are the engine's price/liquidity/reserves stale vs the sim's RPC?), and
//! are O(1) per address (`pool_addresses`) — acceptably cheap for an
//! env-gated diagnostic. V4 `S_state` derivation (`keccak256(poolId . 6)`)
//! IS computed here because the V4 scalar slots live at non-fixed offsets.
//!
//! # Cost model
//!
//! `BotStateDb.storage_ref` calls [`BotState::probe_tracked_storage_slot`]
//! ONLY when the env gate `DEGENBOT_SIM_DIVERGENCE_LOG=1` is set (checked at
//! the call site, before this method runs). Default runs pay zero. When on:
//! - V2/V3: O(1) `pool_addresses.get(address)`.
//! - V4: O(v4-pools-under-this-PoolManager) `keccak256` on PM cold reads;
//!   revm's `CacheDB` caches `storage_ref`, so each cold slot pays once per
//!   sim. Bounded by the sim's working set of Pool.State slots.

// The tick→u32 bit-pattern cast (two's-complement packing of an `int24`) is
// intentional; clippy's `cast_sign_loss` suggestion (`cast_unsigned`) is not a
// real std method — allow the intentional signed→unsigned bit cast here.
#![expect(clippy::cast_sign_loss)]
#![cfg_attr(
    test,
    allow(clippy::decimal_bitwise_operands, clippy::unreadable_literal)
)]

use alloy::primitives::{keccak256, Address, B256, U256};

use crate::bot_core::{BotState, PoolEntry};

/// Which tracked-pool scalar storage slot the probe packed.
///
/// The on-chain packed word layout differs per family+slot, but the
/// **tracked-bit field** is uniform within a kind (the mask of bits the
/// engine carries; untracked bits are zeroed in the packed word AND masked
/// out of the comparison).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackedSlotKind {
    /// V2 `reserves` slot 8 — `uint112 reserve0 | uint112 reserve1` (low 224
    /// bits). `blockTimestampLast` (high 32) NOT tracked → zeroed + masked.
    V2Reserves,
    /// V3 `slot0` — `uint160 sqrtPriceX96 | int24 tick` (low 184 bits).
    /// `observationIndex`/`feeProtocol`/`unlocked` NOT tracked → zeroed.
    ///
    /// NOTE (ergo task `W32CAU`): these V3 tracked slots assume the canonical
    /// Uniswap V3 layout (one-word slot0, liquidity@4, ticks base@5). A
    /// **`PancakeSwap` V3** pool has a divergent layout (two-word slot0,
    /// liquidity@5, ticks base@6); probing/serving one with these Uniswap
    /// indices (or the `v3_storage_slots` encoders) would misread it. Route
    /// pancake V3 through event-based state sync (production already does)
    /// and, for any direct slot probe/serve, use
    /// `degenbot_pools::v3_pancakeswap_storage_slots`.
    V3Slot0,
    /// V3 `liquidity` slot 4 — `uint128` (low 128 bits).
    V3Liquidity,
    /// V4 `Pool.State slot0` — same packed shape as [`V3Slot0`], lives at the
    /// pool's derived `S_state+0`.
    V4Slot0,
    /// V4 `Pool.State liquidity` — same packed shape as [`V3Liquidity`], lives
    /// at `S_state+3`.
    V4Liquidity,
    /// V3 `ticks(tick)` slot+0 — `uint128 liquidityGross | int128 liquidityNet`
    /// (the engine's `TickInfo` DOES carry both; the slot+1/+2 fee-growth
    /// fields are NOT tracked → rpc-fallback). Per-tick slot =
    /// `keccak256(sign_extend_24(tick) . 5)`.
    V3TickInfo,
    /// V4 `ticks(tick)` slot+0 — same packed shape as [`V3TickInfo`], lives at
    /// `keccak256(sign_extend_24(tick) . (S_state+4))`.
    V4TickInfo,
}

impl TrackedSlotKind {
    /// The bit-mask of the on-chain word the engine tracks (1 = tracked).
    /// The probe zeroes untracked bits in the packed engine word AND masks
    /// the RPC word to this range before comparing — so a divergence fires
    /// only when the engine's tracked fields disagree with the RPC, never on
    /// the untracked timestamp/observation/fee-protocol bits.
    #[must_use]
    pub const fn tracked_bit_mask(self) -> B256 {
        match self {
            // reserve0 (low 112) | reserve1 (bits 112..224); high 32 (ts) NOT
            // tracked. Mask = low 224 bits set.
            Self::V2Reserves => word_low_bits(224),
            // sqrtPriceX96 (low 160) | tick (bits 160..184); high 72 NOT
            // tracked. Mask = low 184 bits set.
            Self::V3Slot0 | Self::V4Slot0 => word_low_bits(184),
            // uint128 liquidity (low 128); high 128 NOT tracked (zero on-chain
            // anyway, but mask to be safe against a packed-but-nonzero rpc).
            Self::V3Liquidity | Self::V4Liquidity => word_low_bits(128),
            // ticks(tick) slot+0: uint128 liquidityGross (low 128) |
            // int128 liquidityNet (high 128); the full 256-bit word IS tracked
            // (the engine carries both fields in TickInfo).
            Self::V3TickInfo | Self::V4TickInfo => word_low_bits(256),
        }
    }
}

/// A `B256` with the low `n` bits set (0 < n <= 256), rest zero.
const fn word_low_bits(n: u32) -> B256 {
    // big-endian: the low `n` bits are the LAST `n` bytes set to 0xff when the
    // bit count is a byte multiple; the standard slots here are byte-aligned
    // (184 = 23 bytes, 224 = 28, 128 = 16), so the general non-byte-aligned
    // path is unreachable for the kinds in use — but a full-byte-set helper
    // keeps the const-eval honest for any `n`.
    let mut out = [0u8; 32];
    let full_bytes = (n / 8) as usize;
    let mut i = 0usize;
    while i < 32 {
        // big-endian byte index: low bytes are at the tail.
        if (32 - full_bytes) <= i {
            out[i] = 0xff;
        }
        i += 1;
    }
    B256::new(out)
}

/// The packed engine word + the bookkeeping the divergence log needs.
///
/// `engine_word` has the **tracked fields** packed in the on-chain layout and
/// **untracked bits ZEROED**. The probe caller masks the RPC value to
/// [`TrackedSlotKind::tracked_bit_mask`] before comparing to `engine_word`,
/// so a divergence fires only on a real tracked-field disagreement (never on
/// the timestamp/observation/fee-protocol bits the engine doesn't carry).
#[derive(Clone, Debug)]
pub struct TrackedSlotProbe {
    /// Which scalar slot was packed (drives the mask + the log `kind=`).
    pub kind: TrackedSlotKind,
    /// The on-chain-packed engine word, untracked bits zeroed.
    pub engine_word: B256,
    /// The engine's `update_block` for this pool — the lag signal (does the
    /// engine trail the sim block?).
    pub update_block: u64,
}

impl BotState {
    /// If `address` + `index` (a storage slot the sim just SLOAD'd) maps to a
    /// **tracked pool's scalar storage slot** the engine carries
    /// authoritatively, return the on-chain-packed engine word (untracked
    /// bits zeroed) plus its [`TrackedSlotKind`] + `update_block`. `None` for
    /// untracked pools, non-pool contracts, or slots the engine doesn't carry
    /// (per-tick `ticks(tick)`, `feeGrowthGlobal`, `tickBitmap`, `observations`
    /// — see the module scope note).
    ///
    /// Pure observation — the caller (`BotStateDb.storage_ref`) compares the
    /// returned word against the RPC value WITHOUT changing what the sim reads.
    /// Env-gated at the call site; this method is cheap (`None` fast-paths the
    /// non-pool addresses via the O(1) `pool_addresses` map; V4 pays
    /// O(v4-pools-under-PM) `keccak256` only on a `PoolManager` SLOAD).
    #[must_use]
    pub fn probe_tracked_storage_slot(
        &self,
        address: Address,
        index: U256,
    ) -> Option<TrackedSlotProbe> {
        // V2/V3 path: address is the pool contract; O(1) address→pool_id.
        if let Some(pool_id) = self.pool_id_by_address(&address) {
            return match self.pools.get(&pool_id)? {
                PoolEntry::V2(_, state) => {
                    // V2 reserves slot = 8.
                    if index == U256::from(8u64) {
                        Some(TrackedSlotProbe {
                            kind: TrackedSlotKind::V2Reserves,
                            engine_word: pack_v2_reserves_word(state.reserve0, state.reserve1),
                            update_block: state.update_block,
                        })
                    } else {
                        None
                    }
                }
                PoolEntry::V3(_, state) => {
                    // V3 slot0 = 0; liquidity = 4; ticks base = 5.
                    if index.is_zero() {
                        return Some(TrackedSlotProbe {
                            kind: TrackedSlotKind::V3Slot0,
                            engine_word: pack_cl_slot0_word(state.sqrt_price_x96, state.tick),
                            update_block: state.update_block,
                        });
                    }
                    if index == U256::from(4u64) {
                        return Some(TrackedSlotProbe {
                            kind: TrackedSlotKind::V3Liquidity,
                            engine_word: pack_cl_liquidity_word(state.liquidity),
                            update_block: state.update_block,
                        });
                    }
                    // Per-tick `ticks(tick)` slot = keccak256(sign_extend_24(tick) . 5).
                    probe_tick_slot(
                        state.tick_data.iter(),
                        U256::from(5u64),
                        index,
                        state.update_block,
                        TrackedSlotKind::V3TickInfo,
                    )
                }
                // V2-style Aerodrome pools are NOT V2-slot-8 pools (their
                // reserves live at a different slot — the V2 reserves junction
                // is UniswapV2-pair-specific). Don't probe; the sim reads them
                // from RPC and the engine's V2-style calc reads them too, but
                // the on-chain slot for Aerodrome V2 differs.
                PoolEntry::V4(..)
                | PoolEntry::AerodromeV2(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..) => None,
            };
        }

        // V4 path: `address` is the PoolManager (one PM hosts many pool ids;
        // the address→pool_id map does NOT cover V4 — V4 is keyed by
        // (PM, pool_id), and the per-tick `S_state` base is pool-id-derived).
        // Reverse-map: iterate this PM's V4 pools, derive each `S_state`, check
        // `index == S_state` (slot0) / `S_state + 3` (liquidity).
        for ((pm, pool_id_bytes), internal_pool_id) in &self.v4_pool_ids {
            if *pm != address {
                continue;
            }
            let PoolEntry::V4(_identity, state) = self.pools.get(internal_pool_id)? else {
                continue;
            };
            let s_state = derive_v4_pool_state_base(pool_id_bytes);
            if index == s_state {
                return Some(TrackedSlotProbe {
                    kind: TrackedSlotKind::V4Slot0,
                    engine_word: pack_cl_slot0_word(state.sqrt_price_x96, state.tick),
                    update_block: state.update_block,
                });
            }
            let liquidity_slot = s_state.checked_add(U256::from(3u64)).unwrap_or(U256::MAX);
            if index == liquidity_slot {
                return Some(TrackedSlotProbe {
                    kind: TrackedSlotKind::V4Liquidity,
                    engine_word: pack_cl_liquidity_word(state.liquidity),
                    update_block: state.update_block,
                });
            }
            // Per-tick V4 `ticks(tick)` slot = keccak256(sign_extend_24(tick) . (S_state+4)).
            let ticks_base = s_state.checked_add(U256::from(4u64)).unwrap_or(U256::MAX);
            if let Some(probe) = probe_tick_slot(
                state.tick_data.iter(),
                ticks_base,
                index,
                state.update_block,
                TrackedSlotKind::V4TickInfo,
            ) {
                return Some(probe);
            }
        }
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The on-chain word encoders (re-derived from the Solidity storage layout).
// ─────────────────────────────────────────────────────────────────────────

/// Pack V2 `reserves` slot 8: `uint112 reserve0 | uint112 reserve1 << 112 |
/// uint32 blockTimestampLast << 224`. The timestamp (high 32) is NOT tracked
/// by the engine → zeroed (the probe masks the high 32 out of the comparison
/// so a divergence fires only on reserves).
fn pack_v2_reserves_word(
    reserve0: alloy::primitives::aliases::U112,
    reserve1: alloy::primitives::aliases::U112,
) -> B256 {
    let r0 = U256::from(reserve0);
    let r1 = U256::from(reserve1);
    // reserve1 shifted left 112; ts = 0 (engine doesn't track it).
    let word: U256 = r0 | (r1 << 112u32);
    word.into()
}

/// Pack a concentrated-liquidity `slot0`:
/// `uint160 sqrtPriceX96 | int24 tick << 160`. The remaining high bits
/// (`observationIndex`/`slow`, `feeProtocol`, `unlocked`) are NOT tracked →
/// zeroed + masked out (low 184 bits).
fn pack_cl_slot0_word(sqrt_price_x96: U256, tick: i32) -> B256 {
    // sqrtPriceX96 occupies the low 160 bits of the packed slot0 word.
    // tick is a SIGNED int24 — on-chain it's the two's-complement 24-bit
    // pattern placed at bits 160..184. Mask sqrtPrice to 160 bits so a
    // stray high bit can't bleed into the tick field.
    let sqrt_masked = sqrt_price_x96 & be_uint160_mask();
    let tick_u = (tick as u32) & 0x00ff_ffff; // low 24 bits two's-complement.
    let word: U256 = sqrt_masked | (U256::from(tick_u) << 160u32);
    word.into()
}

/// Pack a concentrated-liquidity `liquidity` slot: `uint128` (low 128 bits;
/// high 128 zero on-chain).
fn pack_cl_liquidity_word(liquidity: u128) -> B256 {
    U256::from(liquidity).into()
}

/// `0xffffffffffffffffffffffffffffffffffff` (low 160 bits set — 2 full
/// 64-bit limbs + 32 bits in the third limb) — the sqrtPriceX96 field mask
/// inside `slot0` (160 bits = uint160).
const fn be_uint160_mask() -> U256 {
    U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0])
}

/// Derive the V4 `Pool.State` storage base for a poolId:
/// `S_state = keccak256(abi.encode(poolId, uint256(6)))` — the
/// `_pools` mapping lives at top-level slot 6 (per
/// `docs/architecture/v4_poolmanager_storage_layout.md`). `poolId` is the
/// already-computed `keccak256(PoolKey)` bytes (the engine stores it as
/// `V4PoolIdentity.pool_id`). `abi.encode(bytes32,uint256)` = 32 + 32 bytes
/// (the uint256 6 big-endian-padded).
fn derive_v4_pool_state_base(pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId) -> U256 {
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(pool_id);
    // uint256(6) big-endian: 31 zero bytes + 0x06.
    input[63] = 6;
    let hash: [u8; 32] = keccak256(input).into();
    U256::from_be_bytes(hash)
}

/// Reverse-map a per-tick `ticks(tick)` storage slot for a CL pool: for
/// each tick the engine tracks, compute `keccak256(sign_extend_24(tick) .
/// base)` + return the probe if it equals `index`. O(`tick_count`) per cold
/// tick-slot read — cached by the outer `CacheDB` after the first read, so
/// once per tick per sim. Acceptable for an env-gated diagnostic.
///
/// `base` is the mapping base (V3 slot 5; V4 `S_state+4`). The packed
/// slot+0 word is `uint128 liquidityGross | int128 liquidityNet` (the
/// engine's `TickInfo` carries both → full 256-bit comparison).
fn probe_tick_slot<'a, I>(
    ticks: I,
    base: U256,
    index: U256,
    update_block: u64,
    kind: TrackedSlotKind,
) -> Option<TrackedSlotProbe>
where
    I: Iterator<Item = (&'a i32, &'a degenbot_pools::TickInfo)>,
{
    for (&tick, info) in ticks {
        let tick_slot = derive_tick_storage_slot(tick, base);
        if tick_slot == index {
            return Some(TrackedSlotProbe {
                kind,
                engine_word: pack_tick_info_word(info),
                update_block,
            });
        }
    }
    None
}

/// Derive the per-tick storage slot for a CL `ticks` mapping at `base`:
/// `keccak256(abi.encode(int24 tick, uint256 base))` — the int24 is
/// sign-extended to 32 bytes (two's-complement), the base is BE-padded.
fn derive_tick_storage_slot(tick: i32, base: U256) -> U256 {
    let mut input = [0u8; 64];
    // abi.encode(int24): sign-extend the int24 to 32 bytes (bytes 29..31 hold
    // the 24-bit two's-complement pattern, bytes 0..28 hold the sign fill).
    let tick_u = tick as u32; // two's-complement bit pattern (i32 has the sign on the top bit).
                              // The int24 is the low 24 bits of the i32 cast; sign-extend to 32 bytes.
    let tick_24 = tick_u & 0x00ff_ffff;
    let sign_fill = if (tick_u & 0x0080_0000) != 0 {
        0xff
    } else {
        0x00
    };
    input[..29].fill(sign_fill);
    // The int24 occupies the low 24 bits of `tick_24`; its 3 big-endian bytes
    // land at input[29..32] (bytes 29, 30, 31 of the 32-byte sign-extended key).
    let tick_be = tick_24.to_be_bytes();
    input[29..32].copy_from_slice(&tick_be[1..4]);
    // abi.encode(uint256 base): big-endian 32 bytes.
    input[32..64].copy_from_slice(&base.to_be_bytes::<32>());
    let hash: [u8; 32] = keccak256(input).into();
    U256::from_be_bytes(hash)
}

/// Pack the per-tick `ticks(tick)` slot+0 word:/// `uint128 liquidityGross | int128 liquidityNet` (gross in the low 128 bits,
/// net in the high 128 bits as a two's-complement int128).
fn pack_tick_info_word(info: &degenbot_pools::TickInfo) -> B256 {
    let gross = U256::from(info.liquidity_gross.to::<u128>());
    // The int128 liquidity_net: the on-chain slot holds the LOW 128 bits of
    // the two's-complement. The shared low-16-byte projection
    // (`TickInfo::liquidity_net_i128`), re-unsigned into the slot's high
    // half via the width-preserving `cast_unsigned()` bit-pattern cast.
    let net = U256::from(info.liquidity_net_i128().cast_unsigned()) << 128u32;
    (gross | net).into()
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{
        BotState, RegisterV2PoolParams, RegisterV3PoolParams, RegisterV4PoolParams, V4PoolKey,
    };
    use crate::solvers::arb_engine::PoolTickCoverage;
    use alloy::primitives::{address, aliases::U112, keccak256, Address, B256, U256};
    use degenbot_pools::TickInfo;

    const V3_ADDR: Address = address!("777777775ce34e0b60a4a79bb5bc5d34b7e5fab4");
    const V4_PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const V2_ADDR: Address = address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");

    fn v3_pool_params() -> RegisterV3PoolParams {
        RegisterV3PoolParams {
            address: V3_ADDR,
            token0: Address::ZERO,
            token1: Address::from([0xa0; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from_limbs([0x1234_5678, 0x9abc_def0, 0x1111, 0])
                & be_uint160_mask(),
            liquidity: 0x0000_0000_006b_5d49_e99f_8835,
            tick: -5010,
            tick_data: std::collections::HashMap::new(),
            update_block: 18_012_345,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        }
    }

    fn v2_pool_params() -> RegisterV2PoolParams {
        RegisterV2PoolParams {
            address: V2_ADDR,
            token0: Address::ZERO,
            token1: Address::from([0xa0; 20]),
            reserve0: U112::from(0x6a68_0ee7_0000_0000_00fd_e867u128),
            reserve1: U112::from(0x7811_7bf0_c05bu128),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::ZERO,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            update_block: 18_012_345,
        }
    }

    fn v4_pool_params() -> RegisterV4PoolParams {
        RegisterV4PoolParams {
            pool_manager: V4_PM,
            pool_id: [0xeeu8; 32],
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: std::collections::HashMap::new(),
            update_block: 17_999_999,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        }
    }

    // ── V2 reserves slot 8 ───────────────────────────────────────────────

    #[test]
    fn v2_reserves_slot_packs_reserve0_and_reserve1_low_224_bits() {
        let mut core = BotState::new();
        let _id = core
            .register_v2_pool(&v2_pool_params())
            .expect("V2 registration");

        let probe = core
            .probe_tracked_storage_slot(V2_ADDR, U256::from(8u64))
            .expect("V2 slot 8 must probe");
        assert_eq!(probe.kind, TrackedSlotKind::V2Reserves);
        assert_eq!(probe.update_block, 18_012_345);

        // Decode: reserve0 = low 112 bits, reserve1 = bits 112..224, ts(high 32) = 0.
        let word = U256::from_be_bytes(probe.engine_word.0);
        let r0 = word & U256::from((1u128 << 112) - 1);
        let r1 = (word >> 112u32) & U256::from((1u128 << 112) - 1);
        let ts = word >> 224u32;
        assert_eq!(r0, U256::from(0x6a68_0ee7_0000_0000_00fd_e867u128));
        assert_eq!(r1, U256::from(0x7811_7bf0_c05bu128));
        assert_eq!(
            ts,
            U256::ZERO,
            "timestamp bits must be zero (engine doesn't track ts)"
        );

        // The masked mask covers low 224 bits, NOT the ts.
        let mask = U256::from_be_bytes(TrackedSlotKind::V2Reserves.tracked_bit_mask().0);
        assert_eq!(
            mask & (U256::from(1u128) << 224u32),
            U256::ZERO,
            "ts bit must be masked OUT"
        );
        assert_eq!(mask >> 224u32, U256::ZERO);
        assert_eq!(mask & r0, r0, "low 224 bits set");
    }

    #[test]
    fn v2_non_reserves_slot_returns_none() {
        let mut core = BotState::new();
        let _id = core.register_v2_pool(&v2_pool_params()).unwrap();
        assert!(core
            .probe_tracked_storage_slot(V2_ADDR, U256::from(6u64))
            .is_none());
        assert!(core
            .probe_tracked_storage_slot(V2_ADDR, U256::ZERO)
            .is_none());
    }

    // ── V3 slot0 + liquidity ────────────────────────────────────────────

    #[test]
    fn v3_slot0_packs_sqrt_price_and_tick_low_184_bits() {
        let mut core = BotState::new();
        let _id = core
            .register_v3_pool(&v3_pool_params())
            .expect("V3 registration");

        let probe = core
            .probe_tracked_storage_slot(V3_ADDR, U256::ZERO)
            .expect("V3 slot0 must probe");
        assert_eq!(probe.kind, TrackedSlotKind::V3Slot0);
        assert_eq!(probe.update_block, 18_012_345);

        let word = U256::from_be_bytes(probe.engine_word.0);
        let sqrt = word & be_uint160_mask();
        let tick_u = (word >> 160u32) & U256::from(0x00ff_ffffu32);
        // The two's-complement 24-bit pattern of -5010 (computed, not
        // hardcoded, to avoid a stale hex constant drifting from the value).
        let expected_tick_word = U256::from((-5010i32) as u32 & 0x00ff_ffff);
        assert_eq!(tick_u, expected_tick_word);
        assert_eq!(
            sqrt,
            v3_pool_params().sqrt_price_x96 & be_uint160_mask(),
            "sqrtPriceX96 masked to 160 bits"
        );
        // High bits beyond tick (observationIndex etc.) must be zero.
        assert_eq!(word >> 184u32, U256::ZERO);

        // Tick round-trips through reconstruction (24-bit two's complement: if
        // the sign bit (bit 23) is set, subtract 2^24 in signed arithmetic).
        let tick_u_i = tick_u.to::<i128>();
        let reconstructed_tick = if tick_u_i >= (1i128 << 23) {
            tick_u_i - (1i128 << 24)
        } else {
            tick_u_i
        };
        assert_eq!(reconstructed_tick, -5010i128);
    }

    #[test]
    fn v3_liquidity_slot_packs_low_128_bits() {
        let mut core = BotState::new();
        let _id = core.register_v3_pool(&v3_pool_params()).unwrap();

        let probe = core
            .probe_tracked_storage_slot(V3_ADDR, U256::from(4u64))
            .expect("V3 liquidity must probe");
        assert_eq!(probe.kind, TrackedSlotKind::V3Liquidity);
        let word = U256::from_be_bytes(probe.engine_word.0);
        assert_eq!(word, U256::from(0x0000_0000_006b_5d49_e99f_8835u128));
        assert_eq!(word >> 128, U256::ZERO, "high 128 bits zero");
    }

    #[test]
    fn v3_fee_growth_global_slot_returns_none() {
        // feeGrowthGlobal0X128 (slot 1) is NOT tracked by the engine → the
        // probe returns None (this is the slot the V3 swap callback reads that
        // the engine can't serve — the root of the historical LOK reverts).
        let mut core = BotState::new();
        let _id = core.register_v3_pool(&v3_pool_params()).unwrap();
        assert!(core
            .probe_tracked_storage_slot(V3_ADDR, U256::from(1u64))
            .is_none());
        assert!(core
            .probe_tracked_storage_slot(V3_ADDR, U256::from(2u64))
            .is_none());
    }

    #[test]
    fn unregistered_address_returns_none() {
        let core = BotState::new();
        let nowhere = address!("1111111111111111111111111111111111111111");
        assert!(core
            .probe_tracked_storage_slot(nowhere, U256::ZERO)
            .is_none());
    }

    // ── V4 slot0 + liquidity at the derived S_state ────────────────────

    #[test]
    fn v4_state_base_derived_via_keccak_of_pool_id_and_slot_6() {
        // Matches the spike doc: S_state = keccak256(abi.encode(poolId, uint256(6))).
        let pool_id = [0xeeu8; 32];
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(&pool_id);
        input[63] = 6;
        let expected = U256::from_be_bytes(<[u8; 32]>::from(keccak256(input)));
        assert_eq!(derive_v4_pool_state_base(&pool_id), expected);
    }

    #[test]
    fn v4_slot0_and_liquidity_probe_at_derived_state_offsets() {
        let mut core = BotState::new();
        let _id = core
            .register_v4_pool(&v4_pool_params())
            .expect("V4 registration");
        let s_state = derive_v4_pool_state_base(&[0xeeu8; 32]);

        let slot0 = core
            .probe_tracked_storage_slot(V4_PM, s_state)
            .expect("V4 slot0 must probe at S_state+0");
        assert_eq!(slot0.kind, TrackedSlotKind::V4Slot0);
        assert_eq!(slot0.update_block, 17_999_999);

        let liq = core
            .probe_tracked_storage_slot(V4_PM, s_state + U256::from(3u64))
            .expect("V4 liquidity must probe at S_state+3");
        assert_eq!(liq.kind, TrackedSlotKind::V4Liquidity);
    }

    #[test]
    fn v4_fee_growth_slot_at_state_plus_1_returns_none() {
        // S_state+1 (feeGrowthGlobal0X128) is NOT tracked → None (the root of
        // the intra-sim inconsistency: serve slot0 from the engine against
        // RPC fee-growth → LOK).
        let mut core = BotState::new();
        let _id = core.register_v4_pool(&v4_pool_params()).unwrap();
        let s_state = derive_v4_pool_state_base(&[0xeeu8; 32]);
        assert!(core
            .probe_tracked_storage_slot(V4_PM, s_state + U256::from(1u64))
            .is_none());
        assert!(core
            .probe_tracked_storage_slot(V4_PM, s_state + U256::from(2u64))
            .is_none());
    }

    // ── the per-tick `ticks(tick)` reverse-map (V3 + V4) ──────────────

    fn v3_pool_with_tick(params: &mut RegisterV3PoolParams, tick: i32, gross: u128, net: i128) {
        params.tick_data.insert(
            tick,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(gross),
                liquidity_net: alloy::primitives::I256::try_from(net).unwrap(),
                block: 0,
            },
        );
    }

    #[test]
    fn v3_tick_slot_reverse_maps_via_keccak_of_tick_and_base_5() {
        let mut params = v3_pool_params();
        v3_pool_with_tick(&mut params, -100, 1_000, -500);
        v3_pool_with_tick(&mut params, 200, 2_000, 300);
        let mut core = BotState::new();
        let _id = core.register_v3_pool(&params).expect("V3 registration");

        // The on-chain slot for ticks(-100) at mapping base 5.
        let tick_slot = derive_tick_storage_slot(-100, U256::from(5u64));
        let probe = core
            .probe_tracked_storage_slot(V3_ADDR, tick_slot)
            .expect("V3 ticks(tick) slot must probe");
        assert_eq!(probe.kind, TrackedSlotKind::V3TickInfo);
        assert_eq!(probe.update_block, 18_012_345);

        // Decode: gross in low 128, net in high 128 (two's-complement).
        let word = U256::from_be_bytes(probe.engine_word.0);
        let gross = word & U256::from(u128::MAX);
        let net_high = word >> 128u32;
        assert_eq!(gross, U256::from(1_000u64));
        // net = -500: two's-complement int128 = u128::MAX - 499.
        assert_eq!(net_high, U256::from(u128::MAX - 499));

        // The +200 tick maps to a DIFFERENT slot + a different gross.
        let tick_slot_200 = derive_tick_storage_slot(200, U256::from(5u64));
        assert_ne!(tick_slot_200, tick_slot, "distinct ticks → distinct slots");
        let probe_200 = core
            .probe_tracked_storage_slot(V3_ADDR, tick_slot_200)
            .expect("V3 ticks(200) probes");
        let word_200 = U256::from_be_bytes(probe_200.engine_word.0);
        assert_eq!(
            word_200 & U256::from(u128::MAX),
            U256::from(2_000u64),
            "gross for +200 tick"
        );

        // An unknown tick slot (not in tick_data) returns None.
        let unknown_slot = derive_tick_storage_slot(999, U256::from(5u64));
        assert!(core
            .probe_tracked_storage_slot(V3_ADDR, unknown_slot)
            .is_none());
    }

    #[test]
    fn v4_tick_slot_reverse_maps_via_keccak_of_tick_and_state_plus_4() {
        let mut params = v4_pool_params();
        params.tick_data.insert(
            -50,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(7_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-250i64).unwrap(),
                block: 0,
            },
        );
        let mut core = BotState::new();
        let _id = core.register_v4_pool(&params).expect("V4 registration");
        let s_state = derive_v4_pool_state_base(&[0xeeu8; 32]);
        let ticks_base = s_state + U256::from(4u64);

        let tick_slot = derive_tick_storage_slot(-50, ticks_base);
        let probe = core
            .probe_tracked_storage_slot(V4_PM, tick_slot)
            .expect("V4 ticks(tick) slot must probe");
        assert_eq!(probe.kind, TrackedSlotKind::V4TickInfo);
        let word = U256::from_be_bytes(probe.engine_word.0);
        assert_eq!(
            word & U256::from(u128::MAX),
            U256::from(7_000u64),
            "gross for V4 -50 tick"
        );
    }

    #[test]
    fn tick_slot_sign_extension_matches_negative_tick_keccak() {
        // A negative tick's int24 sign-extension: bytes 0..28 are 0xff.
        let slot = derive_tick_storage_slot(-1, U256::from(5u64));
        // Recompute independently to cross-check the sign-extend.
        let mut input = [0u8; 64];
        input[..29].fill(0xff);
        // -1 as int24: low 24 bits all set.
        input[29] = 0xff;
        input[30] = 0xff;
        input[31] = 0xff;
        input[32..64].copy_from_slice(&U256::from(5u64).to_be_bytes::<32>());
        let expected = U256::from_be_bytes(<[u8; 32]>::from(keccak256(input)));
        assert_eq!(
            slot, expected,
            "negative tick sign-extend + keccak round-trips"
        );
    }

    // ── the tick (placeholder — per-tick slot reverse-map is deferred) ──

    #[test]
    fn tick_info_gross_net_pair_shape_kept_for_future_tick_reverse_map() {
        // The per-tick slot+0 word packs `uint128 liquidityGross | int128
        // liquidityNet` — the engine's TickInfo DOES carry both. The
        // reverse-map (keccak(tick . base)) is deferred (see module scope),
        // but prove the field types are reachable so the encodes-the-tick-word
        // extension lands here.
        let info = TickInfo {
            liquidity_gross: alloy::primitives::U128::from(123u128),
            liquidity_net: alloy::primitives::I256::try_from(-456i64).unwrap(),
            block: 0,
        };
        let gross = U256::from(info.liquidity_gross.to::<u128>());
        assert_eq!(gross, U256::from(123u64));
        assert_eq!(
            info.liquidity_net,
            alloy::primitives::I256::try_from(-456i64).unwrap()
        );
    }
}
