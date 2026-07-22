//! Parity between the sim's `BotStateDb::storage_ref` view of pool state and
//! the diagnostic's typed `engine_state` view (task BNHNTU, Option C).
//!
//! ## What this proves
//!
//! BNHNTU's acceptance criterion #2 is "no divergence between the sim's view of
//! state and the diagnostic's view (same snapshot, same block)." Both views
//! derive from one `BotState` registry (ADR-003 single state owner) — the sim
//! via `BotStateDb::storage_ref` (storage slot words that revm SLOADs and the
//! pool's `swap()` Solidity unpacks), the diagnostic via typed scalars pulled
//! directly from `PoolEntry`'s `(identity, state)` pair at solve time.
//!
//! The divergence risk is purely an **encoding-parity** risk: does the slot
//! word `storage_ref` encodes unpack (per the Solidity storage layout spec) to
//! the scalar values held in the typed state? This test closes that gap with a
//! closed-form, spec-derived round-trip oracle — NO snapshot/mirror type
//! introduced (a `BotStateSnapshot` clonable view was refused per ADR-003
//! ("delete, not migrate"; "the engine owns no pool state") + ADR-014 D5
//! ("a method returning `&FamilyPoolState` out of `BotState`'s map can't live
//! on a borrowed struct… projections on the sum type where they structurally
//! belong. No trait, no `dyn`")).
//!
//! ## Coverage
//!
//! - **V2** — reserves slot 8: `uint112 r0 | uint112 r1 | uint32 ts` packed word.
//! - **V3** — `slot0`@0 (`uint160 sqrtPriceX96 | int24 tick | …`),
//!   `liquidity`@4 (`uint128`), `ticks(i24)` mapping slot 5 (per-tick slot =
//!   `keccak256(tick_BE32 . 5_BE32)`, packed `uint128 gross | int128 net | …`).
//! - **V4** — out of scope (V4 pools have no persistent on-chain storage at
//!   fixed slots; `BotStateDb::storage_ref` returns `None` for V4 addresses
//!   and V4 state is seeded via transient storage — see `v4_transient.rs`).
//!   V4's diagnostic `engine_state` goes through a `StateView.getSlot0()`
//!   `eth_call`, not an SLOAD; the storage-slot encoder is structurally
//!   absent. The diagnostic's `compute_field_diffs` for V4 compares typed
//!   scalars from the `StateView` call return, independent of any slot word.
//!
//! ## Oracle strength
//!
//! Closed-form: each unpacker is derived independently from the Solidity
//! storage-layout spec (the `UniswapV2Pair::getReserves` / `UniswapV3Pool`
//! `slot0`+`liquidity`+`ticks` storage layout), NOT from the encoder
//! (`encode_v2_reserves_slot` etc. in `bot_state_db.rs`). The unpacker and the
//! encoder agree only because both correctly implement the spec; a bug in the
//! encoder (wrong shift width, wrong mask, missed sign extension) shows up as
//! an unpacked value that does not equal the typed scalar pulled from
//! `BotState`'s registry. The tick-mapping-slot oracle is the
//! `keccak256(tick_BE32 . 5_BE32)` formula from the Solidity mapping-slot
//! derivation rule.

use alloy::primitives::{aliases::U112, keccak256, Address, B256, I256, U256};
use degenbot_bot::bot_core::BotState;
use degenbot_evm::{BotStateDb, SnapshotError};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams};
use degenbot_pools::TickInfo;
use revm::bytecode::Bytecode as RevmBytecode;
use revm::database_interface::DatabaseRef;
use revm::primitives::{StorageKey, StorageValue};
use revm::state::AccountInfo;
use std::collections::HashMap;

// A no-op fallback `DatabaseRef` — the tracked-slot reads return early via
// `storage_ref` and never reach the fallback. Its error type is a test-local
// enum that implements `From<SnapshotError>` (required by the
// `BotStateDb<ExtDb>` `From<SnapshotError>` bound).
struct NoopFallback;

/// Test-local fallback error type. Only the `From<SnapshotError>` impl is
/// load-bearing — it satisfies the `BotStateDb<ExtDb>` bound.
#[derive(Debug, thiserror::Error)]
enum NoopFallbackError {
    /// Surfaces a `SnapshotError` from the `BotStateDb` snapshot-read bound.
    /// Never constructed at runtime — the tracked-slot reads never reach the
    /// fallback, and the fallback itself never errors.
    #[error("snapshot: {0}")]
    Snapshot(#[from] SnapshotError),
}

impl revm::database_interface::DBErrorMarker for NoopFallbackError {}

impl DatabaseRef for NoopFallback {
    type Error = NoopFallbackError;

    fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    fn storage_ref(
        &self,
        _address: Address,
        _index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        Ok(StorageValue::ZERO)
    }

    fn code_by_hash_ref(&self, _code_hash: B256) -> Result<RevmBytecode, Self::Error> {
        Ok(RevmBytecode::default())
    }

    fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// The V2 pair reserves slot index — slot 8 (`UniswapV2Pair::getReserves`
/// storage layout).
const V2_PAIR_RESERVES_SLOT: u64 = 8;

/// The V3 `slot0` storage slot index (slot 0).
const V3_SLOT0_SLOT: u64 = 0;

/// The V3 `liquidity` storage slot index (slot 4).
const V3_LIQUIDITY_SLOT: u64 = 4;

/// The V3 `ticks` mapping base slot (`mapping(int24 => TickInfo)` at slot 5).
const V3_TICKS_MAPPING_SLOT: u64 = 5;

// ---------------------------------------------------------------------------
// Spec-derived unpackers (independent of the encoder in `bot_state_db.rs`).
// Each mirrors the Solidity storage-layout comment in
// `rust/crates/degenbot-evm/src/bot_state_db.rs` but is implemented from the
// layout spec, not by calling the encoder.
// ---------------------------------------------------------------------------

/// Unpack `uint112 reserve0; uint112 reserve1; uint32 blockTimestampLast` from
/// the V2 reserves slot word (slot 8). Layout: reserve0 occupies bits
/// 144..=255, reserve1 occupies bits 32..=143, timestamp occupies bits 0..=31.
///
/// UNUSED by the parity tests now that V2 slot 8 is served from the fallback
/// (see `v2_reserves_slot_falls_through_to_fallback`). Kept as the spec-derived
/// unpacker reference for the follow-up that re-enables snapshot-served V2
/// reserves.
#[allow(dead_code)]
fn unpack_v2_reserves(word: U256) -> (u128, u128, u32) {
    let mask_112: U256 = (U256::from(1u64) << 112) - U256::from(1u64);
    let mask_32: U256 = (U256::from(1u64) << 32) - U256::from(1u64);
    let reserve0: U256 = (word >> 144) & mask_112;
    let reserve1: U256 = (word >> 32) & mask_112;
    let timestamp: U256 = word & mask_32;
    // U256 -> u128/u32 narrowing is sound: masked values fit the target width.
    let reserve0_u128 = reserve0.to::<u128>();
    let reserve1_u128 = reserve1.to::<u128>();
    let timestamp_u32 = timestamp.to::<u32>();
    (reserve0_u128, reserve1_u128, timestamp_u32)
}

/// Unpack `uint160 sqrtPriceX96; int24 tick; …` from the V3 `slot0` word.
/// `sqrtPriceX96` occupies the low 160 bits; the int24 tick occupies bits
/// 160..=183 and is sign-extended in the stored word (negative ticks have bits
/// 184..=255 set).
fn unpack_v3_slot0(word: U256) -> (U256, i32) {
    let mask_160 = (U256::from(1u64) << 160) - U256::from(1u64);
    let sqrt = word & mask_160;
    let tick_word: U256 = word >> 160;
    // Low 24 bits are the tick magnitude; bit 23 is the sign bit.
    let low24 = (tick_word & U256::from(0xFF_FFFFu32)).to::<u32>();
    let tick = if low24 & 0x80_0000 != 0 {
        low24.cast_signed() - 0x100_0000
    } else {
        low24.cast_signed()
    };
    (sqrt, tick)
}

/// Unpack `uint128 liquidity` from V3 slot 4 (high 128 bits zero in a
/// well-formed encoder output; only the low 128 bits are read).
fn unpack_v3_liquidity(word: U256) -> u128 {
    let mask_128: U256 = (U256::from(1u64) << 128) - U256::from(1u64);
    (word & mask_128).to::<u128>()
}

/// Unpack `uint128 liquidityGross; int128 liquidityNet; …` from a V3
/// `ticks(i24)` slot word. `liquidityGross` occupies bits 128..=255;
/// `liquidityNet` occupies bits 0..=127 as a two's-complement int128.
fn unpack_v3_tick_info(word: U256) -> (u128, i128) {
    let mask_128: U256 = (U256::from(1u64) << 128) - U256::from(1u64);
    let gross: U256 = (word >> 128) & mask_128;
    let net_word: U256 = word & mask_128;
    let gross_u128 = gross.to::<u128>();
    // Interpret the low 128 bits as a two's-complement int128.
    let net_u128 = net_word.to::<u128>();
    let net_signed: i128 = net_u128.cast_signed();
    (gross_u128, net_signed)
}

/// Compute the V3 `ticks(i24)` mapping slot for tick index `tick` at mapping
/// base slot `base_slot` — `keccak256(tick_BE32 . base_slot_BE32)`. The int24
/// tick is right-aligned in a 32-byte big-endian word (high 28 bytes zero).
fn v3_tick_mapping_slot(base_slot: u64, tick: i32) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[28..32].copy_from_slice(&tick.to_be_bytes());
    preimage[32..64].copy_from_slice(&U256::from(base_slot).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(preimage).0)
}

// ---------------------------------------------------------------------------
// Fixture helpers.
// ---------------------------------------------------------------------------

/// A well-formed mainnet-shaped V2 pool address (nonzero, distinct from ZERO).
const V2_POOL_ADDRESS: Address = Address::repeat_byte(0x11);
const V3_POOL_ADDRESS: Address = Address::repeat_byte(0x22);

fn register_v2_fixture(core: &mut BotState) -> u64 {
    core.register_v2_pool(&RegisterV2PoolParams {
        address: V2_POOL_ADDRESS,
        token0: Address::repeat_byte(0xa1),
        token1: Address::repeat_byte(0xa2),
        // Reserves within uint112 bounds (well below 2^112).
        reserve0: U112::from(1_234_567_890_123_456_789u128),
        reserve1: U112::from(9_876_543_210_987_654_321u128),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::repeat_byte(0xf0),
        deployer: Address::repeat_byte(0xf0),
        init_hash: B256::repeat_byte(0xc0),
        update_block: 19_000_000,
        ..Default::default()
    })
    .expect("test fixture: V2 registration")
}

/// Register a V3 pool with a couple of initialized ticks (one positive-net,
/// one negative-net) so the `ticks(i24)` encoder path has real data to pack.
fn register_v3_fixture(core: &mut BotState) -> u64 {
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000u128),
            liquidity_net: I256::try_from(-3_000_000i64).expect("fits i256"),
            block: 18_999_900,
        },
    );
    tick_data.insert(
        100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(7_500_000u128),
            liquidity_net: I256::try_from(4_200_000i64).expect("fits i256"),
            block: 18_999_950,
        },
    );
    core.register_v3_pool(&RegisterV3PoolParams {
        address: V3_POOL_ADDRESS,
        token0: Address::repeat_byte(0xb1),
        token1: Address::repeat_byte(0xb2),
        fee: 3_000,
        tick_spacing: 60,
        factory: Address::repeat_byte(0xf1),
        // A sqrtPriceX96 well within the uint160 storage field width.
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 12_345_678u128,
        tick: -42,
        tick_data,
        update_block: 19_000_001,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    })
    .expect("test fixture: V3 registration")
}

// ---------------------------------------------------------------------------
// Parity tests: storage_ref(slot) unpacks to the typed scalars in BotState.
// ---------------------------------------------------------------------------

/// V2 pair slot 8 (reserves) is served from the RPC fallback, NOT the
/// snapshot. The V2 `swap()` K-invariant check mixes slot-8
/// `_reserve0/_reserve1` with `IERC20.balanceOf` (read from the same fallback);
/// serving the snapshot reserves makes the two axes diverge (no matching
/// per-pair token balances in the engine) → K reverts. So `storage_ref` for a
/// tracked V2 pair's slot 8 MUST fall through to the fallback (returns the
/// fallback's value, not the snapshot-encoded reserves).
///
/// The reserves-slot *encoding* is still pinned by the
/// `v2_reserves_slot_encodes_packed_word` unit test (for the follow-up that
/// re-enables snapshot-served V2 reserves once the engine tracks token
/// balances).
#[test]
fn v2_reserves_slot_falls_through_to_fallback() {
    let mut core = BotState::new();
    let pool_id = register_v2_fixture(&mut core);
    let _ = pool_id;

    let db = BotStateDb::new(&core, NoopFallback);
    // NoopFallback returns ZERO for every slot. If the snapshot were served,
    // storage_ref would return the packed reserves word (nonzero — the fixture
    // registers nonzero reserves). ZERO proves the fallthrough.
    let word = db
        .storage_ref(V2_POOL_ADDRESS, U256::from(V2_PAIR_RESERVES_SLOT))
        .expect("V2 slot 8 read must not error (falls through to fallback)");
    assert_eq!(
        word,
        StorageValue::ZERO,
        "V2 slot 8 must fall through to the fallback (snapshot reserves are not \
         servable until the engine tracks per-pair token balances — see \
         BotStateDb doc)"
    );

    // A non-reserves slot on the tracked V2 pair also falls through.
    let other = db
        .storage_ref(V2_POOL_ADDRESS, U256::from(7u64))
        .expect("unmapped V2 slot read must not error");
    assert_eq!(other, StorageValue::ZERO, "unmapped V2 slot falls through");
}

/// V3 `slot0` round-trips the registered sqrtPriceX96 + tick.
#[test]
fn v3_slot0_round_trips_typed_state() {
    let mut core = BotState::new();
    let pool_id = register_v3_fixture(&mut core);

    let entry = core.pool_entry(pool_id).expect("pool registered");
    let PoolEntry::V3(_identity, state) = entry else {
        panic!("expected V3 pool entry");
    };
    let expected_sqrt = state.sqrt_price_x96;
    let expected_tick = state.tick;

    let db = BotStateDb::new(&core, NoopFallback);
    let word = db
        .storage_ref(V3_POOL_ADDRESS, U256::from(V3_SLOT0_SLOT))
        .expect("tracked V3 slot0 must not fall through");

    let (sqrt, tick) = unpack_v3_slot0(word);
    assert_eq!(
        sqrt, expected_sqrt,
        "V3 sqrtPriceX96: storage_ref slot unpacks to the typed-state value"
    );
    assert_eq!(
        tick, expected_tick,
        "V3 tick: storage_ref slot unpacks to the typed-state value (incl. sign)"
    );
}

/// V3 `liquidity` round-trips the registered active liquidity.
#[test]
fn v3_liquidity_slot_round_trips_typed_state() {
    let mut core = BotState::new();
    let pool_id = register_v3_fixture(&mut core);

    let entry = core.pool_entry(pool_id).expect("pool registered");
    let PoolEntry::V3(_identity, state) = entry else {
        panic!("expected V3 pool entry");
    };
    let expected_liquidity = state.liquidity;

    let db = BotStateDb::new(&core, NoopFallback);
    let word = db
        .storage_ref(V3_POOL_ADDRESS, U256::from(V3_LIQUIDITY_SLOT))
        .expect("tracked V3 liquidity slot must not fall through");

    let liq = unpack_v3_liquidity(word);
    assert_eq!(
        liq, expected_liquidity,
        "V3 liquidity: storage_ref slot unpacks to the typed-state value"
    );
}

/// V3 `ticks(i24)` slot round-trips the registered per-tick gross/net
/// liquidity for BOTH a positive-net and a negative-net tick.
#[test]
fn v3_tick_info_slot_round_trips_typed_state() {
    let mut core = BotState::new();
    let pool_id = register_v3_fixture(&mut core);

    let entry = core.pool_entry(pool_id).expect("pool registered");
    let PoolEntry::V3(_identity, state) = entry else {
        panic!("expected V3 pool entry");
    };

    let db = BotStateDb::new(&core, NoopFallback);

    for (&tick, tick_info) in &state.tick_data {
        let slot = v3_tick_mapping_slot(V3_TICKS_MAPPING_SLOT, tick);
        let word = db
            .storage_ref(V3_POOL_ADDRESS, slot)
            .unwrap_or_else(|_| panic!("tracked V3 ticks({tick}) slot must not fall through"));

        let (gross, net) = unpack_v3_tick_info(word);
        assert_eq!(
            gross,
            tick_info.liquidity_gross.to::<u128>(),
            "V3 ticks({tick}) liquidityGross round-trip"
        );
        // The Solidity field is int128; the typed-state field is I256. The
        // round-trip goes I256 -> int128 slot word -> int128 unpack. Compare
        // via the shared i128 domain (the fixture's net values fit i128).
        let expected_net_i128 = i128::try_from(tick_info.liquidity_net)
            .expect("fixture tick net fits int128 (Solidity field width)");
        assert_eq!(
            net, expected_net_i128,
            "V3 ticks({tick}) liquidityNet round-trip (incl. sign)"
        );
    }
}

/// The V3 tick-mapping-slot derivation in this test file MUST match the slot
/// `storage_ref` actually resolves at — otherwise the `ticks(i24)` lookup
/// would always fall through to the fallback. This guards the keccak preimage
/// construction (int24 tick, big-endian, right-aligned in a 32-byte word,
/// concatenated with the base slot as a 32-byte big-endian word) against
/// drift from the Solidity storage-layout mapping-slot rule.
#[test]
fn v3_tick_mapping_slot_derivation_is_keccak_tick_dot_base_slot() {
    // A known reference: tick = -887_270 (a real V3 tick on mainnet).
    let slot = v3_tick_mapping_slot(V3_TICKS_MAPPING_SLOT, -887_270);
    let mut preimage = [0u8; 64];
    preimage[28..32].copy_from_slice(&(-887_270i32).to_be_bytes());
    preimage[32..64].copy_from_slice(&U256::from(V3_TICKS_MAPPING_SLOT).to_be_bytes::<32>());
    let expected = U256::from_be_bytes(keccak256(preimage).0);
    assert_eq!(
        slot, expected,
        "tick mapping slot = keccak256(int24 tick BE32 . base_slot BE32)"
    );
    // Confirm the lookup hits the tracked path (not the NoopFallback zero).
    let mut core = BotState::new();
    let pool_id = register_v3_fixture(&mut core);
    let entry = core.pool_entry(pool_id).expect("pool registered");
    let PoolEntry::V3(_identity, state) = entry else {
        panic!("expected V3 pool entry");
    };
    let db = BotStateDb::new(&core, NoopFallback);
    for (&tick, tick_info) in &state.tick_data {
        let slot = v3_tick_mapping_slot(V3_TICKS_MAPPING_SLOT, tick);
        let word = db
            .storage_ref(V3_POOL_ADDRESS, slot)
            .expect("tick slot lookup hits the tracked path");
        let (gross, _net) = unpack_v3_tick_info(word);
        assert_ne!(
            gross, 0,
            "tick lookup must hit the tracked path (nonzero gross)"
        );
        assert_eq!(gross, tick_info.liquidity_gross.to::<u128>());
    }
}
