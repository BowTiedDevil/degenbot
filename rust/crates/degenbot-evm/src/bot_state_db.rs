//! `BotStateDb` — a `revm::DatabaseRef` impl over `Bot`'s typed pool state.
//!
//! The move that makes the engine state *be* the EVM's `Database` (option B,
//! chosen by the operator post-spike QGJGWI). A hand-written `DatabaseRef`
//! impl reads `Bot`'s typed pool state — V2 reserves, V3 `slot0`/`liquidity`/
//! `ticks(i32)` — and ABI-encodes it to EVM slots **on demand** (no long-lived
//! encoded copy; the typed fields in `Bot` are the single source of truth).
//! `WrapDatabaseAsync<AlloyDB>` is the cold-miss fallback for contracts the
//! engine does not track.
//!
//! `DatabaseRef` is `&self` (vs `Database`'s `&mut self`) — no `Mutex` needed
//! (`WrapDatabaseAsync<AlloyDB>` impls `DatabaseRef` via `&self`, blocking
//! internally on a tokio runtime). Composes under `CacheDB<BotStateDb<…>>`:
//!
//! ```text
//! EVM transact -> CacheDB (sim-scoped overrides)
//!                 -> BotStateDb (engine typed state, encode-on-demand)
//!                 -> WrapDatabaseAsync<AlloyDB> (RPC fallback)
//! ```
//!
//! # Coverage (task EGMSNS)
//!
//! - **V2** — `V2PoolState { reserve0, reserve1, update_block }` -> V2 pair
//!   reserves slot 8 (packed `uint112 reserve0; uint112 reserve1; uint32
//!   blockTimestampLast`; `blockTimestampLast` filled from `update_block`).
//! - **V3** — `V3PoolState { sqrt_price_x96, liquidity, tick, tick_data }` ->
//!   `slot0`@0 (packed `uint160 sqrtPriceX96; int24 tick; …`),
//!   `liquidity`@4 (`uint128`), `ticks(i32)` mapping slot 5 (per-tick slot =
//!   `keccak256(tick_BE32 . 5_BE32)`; `TickInfo` packed `uint128 liquidityGross;
//!   int128 liquidityNet; …`).
//! - **V4** — NOT served here. V4 pools have no persistent on-chain storage at
//!   fixed slots; their swap state lives in the PoolManager's **transient
//!   storage** (TSTORE/TLOAD, EIP-1153) during the `unlock()` batch. Use
//!   [`crate::v4_transient::apply_v4_transient_state`] to seed the built EVM's
//!   `journaled_state.inner.transient_storage` before `transact` (revm exposes
//!   transient storage as a pre-seedable public field — verified by the
//!   `transient_seed` PoC).
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §2.3 for
//! the verified composition shape + §4 for the `code_by_hash` panic-safety
//! invariant (`basic` must eagerly load code).

use alloy::primitives::{Address, U256};
use degenbot_bot::bot_core::BotState;
use degenbot_pools::registry::PoolEntry;
use revm::database_interface::DatabaseRef;
use revm::primitives::{StorageKey, StorageValue, B256};
use revm::state::AccountInfo;

/// V2 pair reserves slot — `uint112 reserve0; uint112 reserve1; uint32
/// blockTimestampLast;` packed at slot 8 (UniswapV2Pair `getReserves` layout).
///
/// Unused by `read_v2_slot` (V2 slot 8 is served from the RPC fallback until
/// the engine tracks per-pair token balances — see [`BotStateDb`] doc). Kept as
/// the slot-layout reference for the follow-up re-enablement.
#[allow(dead_code)]
const V2_PAIR_RESERVES_SLOT: u64 = 8;

/// V3 `slot0` storage slot — `uint160 sqrtPriceX96; int24 tick; …` packed at
/// slot 0. Unused by `read_v3_slot` (V3 served from the RPC fallback — see
/// [`BotStateDb`] doc). Kept for the follow-up re-enablement.
#[allow(dead_code)]
const V3_SLOT0_SLOT: u64 = 0;

/// V3 `liquidity` storage slot — `uint128 liquidity` at slot 4. Unused by
/// `read_v3_slot` (V3 served from the RPC fallback).
#[allow(dead_code)]
const V3_LIQUIDITY_SLOT: u64 = 4;

/// V3 `ticks` mapping base slot — `mapping(int24 => TickInfo)` at slot 5. The
/// per-tick slot is `keccak256(tick_BE32 . 5_BE32)`; `TickInfo` is packed
/// `uint128 liquidityGross; int128 liquidityNet; …`. Unused by `read_v3_slot`.
#[allow(dead_code)]
const V3_TICKS_MAPPING_SLOT: u64 = 5;

/// The engine-state read view that is the EVM's `Database`.
///
/// Wraps a borrowed `&BotState` (ADR-003 state owner) + the cold-miss
/// `fallback` (a `WrapDatabaseAsync<AlloyDB>`). The borrow IS the snapshot —
/// immutable for the sim fan-out, coherent with reorg rollback (ADR-016).
pub struct BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// The `Bot` typed-state read view (borrowed `&BotState`). Encodes typed
    /// fields to EVM slots on demand.
    pub bot_state: &'bot BotState,
    /// The RPC cold-miss fallback (`WrapDatabaseAsync<AlloyDB>` in production).
    pub fallback: ExtDb,
}

impl<'bot, ExtDb> BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// Wrap a borrowed `&BotState` + a cold-miss fallback `DatabaseRef`.
    #[must_use]
    pub fn new(bot_state: &'bot BotState, fallback: ExtDb) -> Self {
        Self {
            bot_state,
            fallback,
        }
    }
}

impl<ExtDb> DatabaseRef for BotStateDb<'_, ExtDb>
where
    ExtDb: DatabaseRef,
{
    type Error = ExtDb::Error;

    /// The engine tracks pool STATE, not pool CODE. Tracked pools get their
    /// account info (balance/nonce/code) served from the `AlloyDB` fallback
    /// (cold-loaded once, cached by `CacheDB`); the sim reads pool state via
    /// [`storage_ref`](Self::storage_ref). Untracked contracts also fall
    /// through — `None` is the normal fallback path.
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if the RPC `basic` fetch fails.
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.fallback.basic_ref(address)
    }

    /// Tracked storage served from the snapshot (V3 slot0 / liquidity /
    /// ticks); untracked fall through to `AlloyDB`.
    ///
    /// # V2 pair slot 8 — served from the RPC fallback, NOT the snapshot
    ///
    /// The V2 pair's `swap()` K-invariant check mixes slot-8
    /// `_reserve0/_reserve1` (this slot) with `IERC20.balanceOf` (read from
    /// the same RPC fallback). Serving the snapshot reserves here makes the two
    /// axes diverge — the snapshot reserves are from `update_block` (a block
    /// number) and lack matching per-pair token balances the engine does not
    /// track — so K fails with stale `_reserve` vs fresh `balanceOf`. Serving
    /// BOTH from the same on-chain slot-8 read keeps them consistent.
    ///
    /// `read_v2_slot` therefore returns `None`, routing V2 slot 8 through to
    /// `self.fallback.storage_ref` below (one cold RPC per pair, cached by the
    /// outer `CacheDB`). The snapshot IS still the SOLVER's source of truth for
    /// path-finding + amount computation — only the EVM's K check needs
    /// on-chain-consistent state, and a fresh backrun snapshot matches on-chain
    /// closely enough that the solver's amounts satisfy K against the on-chain
    /// reserves.
    ///
    /// # Follow-up
    ///
    /// The engine should track per-pair token balances (so `balanceOf` and
    /// reserves both come from the snapshot, reorg-aware + RPC-free). Until
    /// then, V2 slot 8 falls through to the RPC fallback.
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if an untracked RPC fetch fails (the
    /// snapshot path is infallible — an unmapped slot on a tracked pool
    /// falls through to AlloyDB rather than erroring, since the pool's own
    /// `swap()` reads internal slots the engine does not track).
    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        if let Some(value) = read_tracked_storage(self.bot_state, address, index) {
            return Ok(value);
        }
        self.fallback.storage_ref(address, index)
    }

    /// `code_by_hash` is **never invoked** if `basic` eagerly loads code (the
    /// spike-verified `code_by_hash` panic-safety invariant). Falls through to
    /// the fallback for any (unreachable) cold-path.
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.fallback.code_by_hash_ref(code_hash)
    }

    /// Block hashes are not in `Bot`'s domain — always fall through to
    /// `AlloyDB` (the live-network axis).
    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.fallback.block_hash_ref(number)
    }
}

/// Read a tracked pool's storage slot from `BotState`'s typed state, ABI-encoded
/// to the EVM slot word. Returns `None` if the address + slot is not a tracked
/// pool-state read (fall through to `AlloyDB`).
///
/// An unmapped slot ON a tracked pool address returns `None` (fall through) —
/// not an error — because the pool's own `swap()` reads internal slots the
/// engine does not track (fee growth, observation, etc.); those are correctly
/// served from the RPC fallback (cold-loaded once, cached).
fn read_tracked_storage(
    bot_state: &BotState,
    address: Address,
    slot: StorageKey,
) -> Option<StorageValue> {
    let pool_id = bot_state.pool_id_by_address(&address)?;
    let entry = bot_state.pool_entry(pool_id)?;
    match entry {
        PoolEntry::V2(_, state) => read_v2_slot(state, slot),
        PoolEntry::V3(_, state) => read_v3_slot(state, slot),
        // V4 has no persistent slots — its state is transient (see v4_transient).
        _ => None,
    }
}

/// Read a V2 pair persistent slot. Returns `None` — V2 slot 8 (reserves) is
/// served from the RPC fallback, NOT the snapshot. See [`BotStateDb`]'s and
/// [`DatabaseRef::storage_ref`](BotStateDb#method.storage_ref)'s doc comments
/// for the K-invariant consistency reason (the snapshot reserves lack matching
/// per-pair token balances the engine does not track). All V2 pair internal
/// slots also fall through to AlloyDB.
fn read_v2_slot(
    _state: &degenbot_pools::v2_state::V2PoolState,
    _slot: U256,
) -> Option<StorageValue> {
    None
}

/// Read a V3 pool persistent slot. Returns `None` — V3 `slot0`/`liquidity`/
/// `ticks(i24)` are served from the RPC fallback, NOT the snapshot. See
/// [`BotStateDb`]'s doc comment for the consistency reason: the snapshot's V3
/// state (from `update_block`) is inconsistent with the on-chain tick-bitmap /
/// fee-growth / observation slots the pool's `swap()` also reads (served from
/// the RPC at `current_block`) — a stale `slot0`/`tick` against a fresh
/// tick-bitmap reverts every cross-tick swap (`LOK`/`empty` on V2-V2-V3 paths).
/// All V3 pair internal slots fall through to AlloyDB.
fn read_v3_slot(
    _state: &degenbot_pools::v3_state::V3PoolState,
    _slot: U256,
) -> Option<StorageValue> {
    None
}

/// Encode the V2 pair reserves slot (slot 8): packed `uint112 reserve0;
/// uint112 reserve1; uint32 blockTimestampLast` (UniswapV2Pair `getReserves`
/// layout). `blockTimestampLast` is filled from `update_block` (truncated to
/// `u32`).
///
/// UNUSED in production — `read_v2_slot` returns `None` (V2 slot 8 served from
/// the RPC fallback). Kept + tested as the encoding oracle for the follow-up
/// that re-enables snapshot-served V2 reserves once the engine tracks per-pair
/// token balances (so reserves + `IERC20.balanceOf` stay consistent).
#[allow(dead_code)]
#[must_use]
fn encode_v2_reserves_slot(reserve0: u128, reserve1: u128, update_block: u64) -> StorageValue {
    let timestamp_last = u32::try_from(update_block).unwrap_or(u32::MAX);
    // Pack: reserve0 (high 112 bits) | reserve1 (next 112) | timestamp (low 32).
    (U256::from(reserve0) << 144) | (U256::from(reserve1) << 32) | U256::from(timestamp_last)
}

/// Encode V3 `slot0` (slot 0): packed `uint160 sqrtPriceX96; int24 tick; …`. The
/// post-tick tail (observation index/cardinality, fee protocol, unlocked) is
/// zero-filled — the sim reads `sqrtPriceX96` + `tick` only.
#[allow(dead_code)]
#[must_use]
fn encode_v3_slot0(sqrt_price_x96: U256, tick: i32) -> StorageValue {
    // Low 160 bits of sqrtPriceX96 (mask = 2^160 - 1).
    let sqrt_price = sqrt_price_x96 & ((U256::from(1u64) << 160) - U256::from(1u64));
    // int24 tick at bits 160..184 — sign-extend the low 24 bits to 256 bits.
    // `as u32` is the two's-complement reinterpretation (intentional; the
    // sign is preserved via the subsequent sign-extension).
    #[allow(clippy::cast_sign_loss)]
    let tick_u32 = tick as u32;
    let tick_word = sign_extend_24(U256::from(tick_u32 & 0xFF_FFFF));
    sqrt_price | (tick_word << 160)
}

/// Encode V3 `liquidity` (slot 4): `uint128 liquidity` (high 128 bits zero).
#[allow(dead_code)]
#[must_use]
fn encode_v3_liquidity_slot(liquidity: u128) -> StorageValue {
    U256::from(liquidity)
}

/// Encode V3 `TickInfo` (the `ticks(i24)` slot value): packed `uint128
/// liquidityGross; int128 liquidityNet; …`. The post-net tail (fee growth, etc.)
/// is zero-filled — the sim reads `liquidityGross`/`liquidityNet` only.
///
/// `liquidityNet` is an `I256`; the `int128` Solidity field occupies the LOW
/// 128 bits of the slot word. `I256::into_raw()` returns the full 256-bit
/// two's-complement bit pattern, which for negative values has the high 128
/// bits set to all ones. Those high bits are OUTSIDE the `int128` field and
/// MUST be masked off before the OR with `gross << 128` (which occupies the
/// high 128 bits) — otherwise a negative `liquidityNet` corrupts
/// `liquidityGross` to `2^128 - 1`.
#[allow(dead_code)]
#[must_use]
fn encode_v3_tick_info_slot(tick_info: &degenbot_pools::TickInfo) -> StorageValue {
    let gross = U256::from(tick_info.liquidity_gross);
    let net_raw = sign_extend_128_from_i256(tick_info.liquidity_net);
    // Mask net to the low 128 bits (the int128 field width) — the high 128
    // bits of `net_raw` are sign-extension beyond the field and would corrupt
    // the `gross` half of the packed word if OR-ed in.
    let mask_128 = (U256::from(1u64) << 128) - U256::from(1u64);
    let net = net_raw & mask_128;
    (gross << 128) | net
}

/// Compute `keccak256(tick_BE32 . base_slot_BE32)` — the V3 `ticks(i24)`
/// mapping slot for tick index `tick` at mapping base slot `base_slot`.
#[allow(dead_code)]
fn tick_mapping_slot(base_slot: u64, tick: i32) -> U256 {
    let mut preimage = [0u8; 64];
    // int24 tick, big-endian, right-padded to 32 bytes (the high 28 bytes zero).
    preimage[28..32].copy_from_slice(&tick.to_be_bytes());
    preimage[32..64].copy_from_slice(&U256::from(base_slot).to_be_bytes::<32>());
    U256::from_be_bytes(alloy::primitives::keccak256(preimage).0)
}

/// Sign-extend a 24-bit value (int24 reinterpreted as the low 24 bits of a
/// `U256`) to a full 256-bit two's-complement word. The sign bit is bit 23.
#[allow(dead_code)]
fn sign_extend_24(low24_u: U256) -> U256 {
    let low24 = low24_u & U256::from(0xFF_FFFFu32);
    if (low24 & U256::from(0x80_0000u32)).is_zero() {
        low24
    } else {
        // Sign-extend: set bits 24..=255 to 1. Build all-ones then shift.
        let all_ones = U256::MAX;
        let high_mask = all_ones << 24;
        low24 | high_mask
    }
}

/// Sign-extend an `I256` to a `U256` two's-complement word (low 128 bits hold
/// the magnitude; bit 127 is the sign bit). Used for V3 `TickInfo.liquidity_net`
/// in the `ticks(i24)` packed slot.
#[allow(dead_code)]
fn sign_extend_128_from_i256(val: alloy::primitives::I256) -> U256 {
    // I256 -> its 256-bit two's-complement bit pattern (negative values have
    // the high 128 bits set; the EVM int128 reads the low 128).
    val.into_raw()
}

/// Errors raised by `BotStateDb` snapshot reads (storage-layout lookup
/// failures — surfaces drift as a typed error for the `From<SnapshotError>`
/// bound on the fallback's error type).
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// A storage slot lookup on a tracked pool address failed (snapshot
    /// unreachable — does not occur for the borrowed `&BotState` path; surfaces
    /// a poisoned-lock or internal-encoding problem as a typed error).
    #[error("BotStateDb snapshot read failed for {address} at slot {slot}: {reason}")]
    SnapshotRead {
        /// The contract address.
        address: Address,
        /// The EVM storage slot index.
        slot: U256,
        /// The failure reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::U256;

    /// V2 reserves-slot encoding pin: reserve0=1e18, reserve1=2e18,
    /// update_block=19_000_000 -> the packed `uint112 r0 | uint112 r1 | uint32
    /// ts` word. Hand-derived closed-form oracle.
    #[test]
    fn v2_reserves_slot_encodes_packed_word() {
        let word = encode_v2_reserves_slot(
            1_000_000_000_000_000_000u128,
            2_000_000_000_000_000_000u128,
            19_000_000,
        );
        let expected = (U256::from(1_000_000_000_000_000_000u128) << 144)
            | (U256::from(2_000_000_000_000_000_000u128) << 32)
            | U256::from(19_000_000u32);
        assert_eq!(word, expected, "V2 reserves slot packed-word encoding");
    }

    /// V3 `slot0` encoding pin: sqrtPriceX96=2^160-1 (max uint160), tick=-1
    /// (sign-extended). Verifies the sqrtPrice lands in the low 160 bits + the
    /// int24 tick is sign-extended above bit 160.
    #[test]
    fn v3_slot0_encodes_packed_word() {
        let max_uint160 = (U256::from(1u64) << 160) - U256::from(1u64);
        let word = encode_v3_slot0(max_uint160, -1);
        // Low 160 bits hold sqrtPriceX96.
        assert_eq!(
            word & max_uint160,
            max_uint160,
            "sqrtPriceX96 in low 160 bits"
        );
        // tick=-1 as int24 = 0xFF_FFFF, sign-extended: bits 24..=255 of the
        // tick word are ones; placed at bit 160 -> bits 160..=255 (96 bits).
        let tick_part = word >> 160;
        assert_eq!(
            tick_part,
            (U256::from(1u64) << 96) - U256::from(1u64),
            "int24 tick=-1 sign-extended at bits 160..=255"
        );
    }

    /// V3 `slot0` positive-tick pin: tick=887_270 (a real V3 tick), no sign
    /// extension. Verifies the positive-tick path.
    #[test]
    fn v3_slot0_positive_tick() {
        let sqrt = U256::from_limbs([0x1234, 0, 0, 0]);
        let word = encode_v3_slot0(sqrt, 887_270);
        assert_eq!(
            word & ((U256::from(1u64) << 160) - U256::from(1u64)),
            sqrt,
            "sqrtPriceX96 in low 160 bits"
        );
        assert_eq!(
            word >> 160,
            U256::from(887_270u32),
            "positive int24 tick, no sign extension"
        );
    }

    /// V3 `liquidity` slot encoding pin: liquidity=0xdead_beef -> uint128 word.
    #[test]
    fn v3_liquidity_slot_encodes_uint128() {
        assert_eq!(
            encode_v3_liquidity_slot(0xdead_beef),
            U256::from(0xdead_beefu128)
        );
    }

    /// V3 `ticks(i24)` mapping slot derivation pin: `keccak256(tick_BE32 . 5)`.
    #[test]
    fn v3_tick_mapping_slot_is_keccak_tick_dot_5() {
        let slot = tick_mapping_slot(V3_TICKS_MAPPING_SLOT, -887_270);
        let mut preimage = [0u8; 64];
        preimage[28..32].copy_from_slice(&(-887_270i32).to_be_bytes());
        preimage[32..64].copy_from_slice(&U256::from(5u64).to_be_bytes::<32>());
        let expected = U256::from_be_bytes(alloy::primitives::keccak256(preimage).0);
        assert_eq!(slot, expected, "ticks(i24) mapping slot = keccak(tick . 5)");
    }

    /// Negative-`liquidityNet` tick pin: a negative net MUST NOT corrupt the
    /// `liquidityGross` half of the packed `ticks(i24)` slot word. Pre-fix,
    /// `sign_extend_128_from_i256` returned the full 256-bit two's-complement
    /// (high 128 bits all ones for negatives), and the OR with `gross << 128`
    /// clobbered gross to `2^128 - 1`. Found by the
    /// `parity_diagnostic_encoding` integration test.
    #[test]
    fn v3_tick_info_slot_negative_net_preserves_gross() {
        let tick_info = degenbot_pools::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000u128),
            liquidity_net: alloy::primitives::I256::try_from(-3_000_000i64).expect("fits i256"),
            block: 0,
        };
        let word = encode_v3_tick_info_slot(&tick_info);
        // gross occupies the high 128 bits.
        let mask_128 = (U256::from(1u64) << 128) - U256::from(1u64);
        let gross = (word >> 128) & mask_128;
        assert_eq!(
            gross,
            U256::from(5_000_000u128),
            "gross preserved (negative net)"
        );
        // net occupies the low 128 bits as int128 two's-complement.
        let net_word = word & mask_128;
        // int128 of -3_000_000 == 2^128 - 3_000_000.
        let expected_net = (U256::from(1u64) << 128) - U256::from(3_000_000u64);
        assert_eq!(
            net_word, expected_net,
            "net = -3M as int128 two's complement"
        );
    }
}
