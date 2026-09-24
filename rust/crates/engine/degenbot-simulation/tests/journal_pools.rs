//! Journal → typed pool post-state extractors (V2/V3 families; V4 explicit
//! Unsupported) over the frame-replay seam.
//!
//! Engine-level claims (code-only over `CacheDB<EmptyDB>` loopbacks):
//! - The layout table is the one source both directions consume: the PACK
//!   direction produces the word the frame journals, and the DECODE
//!   direction recovers the engine-typed post-state from the settled
//!   journal — `pack==journal word`, `decode==engine state` on fixtures.
//! - A V3 frame's journalled tick slots (keccak preimages) recover to exact
//!   spacing-aligned tick indices from the pre/post current-tick anchors —
//!   and an unmatched slot is never fabricated into a tick.
//! - A V4 `PoolManager` frame returns `Unsupported`, never a guess.
//! - Descriptor-less touched accounts contribute nothing.
//!
//! Live claims (ignored by default, network-gated), following
//! `tests/frame_replay.rs`'s fixture discipline (Eip1559-only, no access
//! list, first-sender-in-block, env-pinnable fixtures):
//! - a replayed V2 pair swap's extracted reserves equal the chain's post-tx
//!   word, decoded through the same layout rows — 0-wei error;
//! - a V3 in-range swap's slot0 (sqrtPriceX96 + tick) + liquidity equal the
//!   chain's post-tx words;
//! - a V3 tick-crossing swap's recovered touched tick words equal the
//!   chain's post-tx `ticks(tick)` words.
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use alloy::primitives::{address, Address, Bytes, B256, U256};
use degenbot_pools::slot_layout::{
    cl_liquidity_tracked_word, cl_slot0_tracked_word, decode_tick_word, decode_v2_reserves_word,
    pack_v2_reserves_word, tick_mapping_slot_at_base, tick_word_of, V2ReservesParts,
};
use degenbot_pools::v3_storage_slots::decode_v3_slot0;
use degenbot_pools::v4_storage_slots::{
    encode_v4_liquidity_slot, encode_v4_slot0, v4_liquidity_slot, v4_pool_state_base_slot,
    v4_slot0_slot, v4_tick_mapping_slot, V4Slot0Parts,
};
use degenbot_pools::ClSlotLayout;
use degenbot_simulation::sim::evm::frame_replay::{
    ReplayStatus, ReplayableTx, ScratchBlock, ScratchEvm,
};
use degenbot_simulation::sim::evm::journal_pools::{
    extract_pool_post_states, PoolFamily, PoolPostKind, PoolPostState, TouchedTickWord,
    TypedPoolPost, V4PoolDescriptor, V4PoolSet,
};
use revm::bytecode::Bytecode;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;

const SENDER: Address = address!("0x1111111111111111111111111111111111111111");
const POOL: Address = address!("0x2222222222222222222222222222222222222222");
const STRANGER: Address = address!("0x3333333333333333333333333333333333333333");

const FIRST_BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;

/// Unrolled `(slot, value)` SSTORE pairs from calldata: for k in 0..PAIRS,
/// writes slot `cd[32k..32k+32]` = value `cd[32(k+PAIRS)..32(k+PAIRS+1)]`
/// — one frame journaling many pool words at once. Three pairs keep every
/// PUSH1 calldata offset inside a byte (32·(2·PAIRS−1) ≤ 255).
const PAIRS: usize = 3;

fn multi_store_code() -> Vec<u8> {
    let mut code = Vec::new();
    for k in 0..PAIRS {
        let s = 32 * k;
        let v = 32 * (k + PAIRS);
        // SSTORE pops the KEY from the stack top: push value first.
        code.extend_from_slice(&[
            0x60,
            u8::try_from(v).unwrap(),
            0x35,
            0x60,
            u8::try_from(s).unwrap(),
            0x35,
            0x55,
        ]);
    }
    code
}

fn block_env() -> ScratchBlock {
    ScratchBlock {
        number: FIRST_BLOCK,
        timestamp: TIMESTAMP,
        base_fee_next: BASE_FEE_GWEI,
    }
}

type TestExt = CacheDB<EmptyDB>;

fn scratch() -> ScratchEvm<TestExt> {
    let mut db = CacheDB::new(EmptyDB::default());
    for (addr, code) in [(POOL, multi_store_code()), (STRANGER, multi_store_code())] {
        db.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                nonce: 0,
                code: Some(Bytecode::new_raw(Bytes::copy_from_slice(&code))),
                ..Default::default()
            },
        );
    }
    db.insert_account_info(
        SENDER,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            nonce: 7,
            ..Default::default()
        },
    );
    ScratchEvm::new(db, block_env())
}

/// A tx whose calldata is `pairs` `(slot, value)` words.
fn tx(nonce: u64, pairs: &[(U256, U256)]) -> ReplayableTx {
    // Block layout (matching the writer): slots 0..PAIRS, then values
    // 0..PAIRS. Padding pairs park at an off-grid slot (far off the tick
    // spacing grid AND every keccak-preimage range) so they never clobber a
    // tracked slot — slot 0 in particular.
    let pad: U256 = U256::from(1u128) << 248;
    let pad_be = pad.to_be_bytes::<32>();
    let mut data = Vec::with_capacity(64 * PAIRS);
    for k in 0..PAIRS {
        match pairs.get(k) {
            Some((slot, _)) => data.extend_from_slice(&slot.to_be_bytes::<32>()),
            None => data.extend_from_slice(&pad_be),
        }
    }
    for k in 0..PAIRS {
        match pairs.get(k) {
            Some((_, value)) => data.extend_from_slice(&value.to_be_bytes::<32>()),
            None => data.extend_from_slice(&pad_be),
        }
    }
    ReplayableTx {
        from: SENDER,
        to: Some(POOL),
        value: U256::ZERO,
        data: Bytes::from(data),
        gas_limit: 300_000,
        max_fee_per_gas: BASE_FEE_GWEI,
        max_priority_fee_per_gas: 0,
        nonce,
    }
}

/// [`tx`] targeting an explicit contract (the mixed-family relay).
fn tx_to(to: Address, nonce: u64, pairs: &[(U256, U256)]) -> ReplayableTx {
    ReplayableTx {
        to: Some(to),
        ..tx(nonce, pairs)
    }
}

/// A relay contract: forwards the WHOLE calldata to `pool` (one CALL), then
/// applies the same `(slot, value)` stores to its OWN storage — one frame
/// that journals two pool-family accounts at once.
fn relay_code(pool: Address) -> Vec<u8> {
    let mut code = Vec::new();
    // mem[0..cdsize] = calldata.
    code.extend_from_slice(&[0x36, 0x60, 0x00, 0x60, 0x00, 0x37]); // CALLDATASIZE 0 0 CALLDATACOPY
                                                                   // CALL(gas, pool, 0, 0, cdsize, 0, 0)
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x36, 0x60, 0x00, 0x60, 0x00]);
    code.push(0x73);
    code.extend_from_slice(pool.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1, 0x50]); // GAS CALL POP
    for k in 0..PAIRS {
        let s = 32 * k;
        let v = 32 * (k + PAIRS);
        code.extend_from_slice(&[
            0x60,
            u8::try_from(v).unwrap(),
            0x35,
            0x60,
            u8::try_from(s).unwrap(),
            0x35,
            0x55,
        ]);
    }
    code
}

/// A scratch with a relay target plus the managed contract it forwards to.
fn scratch_relayed(relay: Address, pool: Address) -> ScratchEvm<TestExt> {
    let mut db = CacheDB::new(EmptyDB::default());
    for (addr, code) in [(relay, relay_code(pool)), (pool, multi_store_code())] {
        db.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                nonce: 0,
                code: Some(Bytecode::new_raw(Bytes::copy_from_slice(&code))),
                ..Default::default()
            },
        );
    }
    db.insert_account_info(
        SENDER,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            nonce: 7,
            ..Default::default()
        },
    );
    ScratchEvm::new(db, block_env())
}

/// A known-V4-pool set owning its descriptors.
fn v4_set(pools: Vec<V4PoolDescriptor>) -> V4PoolSet {
    V4PoolSet::new(pools)
}

fn descriptors(
    families: impl IntoIterator<Item = (Address, PoolFamily)>,
) -> hashbrown::HashMap<Address, PoolFamily> {
    families.into_iter().collect()
}

fn post(states: &[PoolPostState], addr: Address) -> &PoolPostState {
    states
        .iter()
        .find(|s| s.address == addr)
        .unwrap_or_else(|| panic!("no PoolPostState for {addr}"))
}

fn one_word(slot: U256, value: U256) -> Vec<(U256, U256)> {
    vec![(slot, value)]
}

const SLOT8: U256 = U256::from_limbs([8, 0, 0, 0]);
const LIQ_SLOT: U256 = U256::from_limbs([4, 0, 0, 0]);

// ─────────────────────────────────────────────────────────────────────────
// V2: pack==journal word, decode==engine state (0-wei)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn v2_pack_is_the_journal_word_and_decode_is_the_engine_state() {
    let reserve0 = alloy::primitives::aliases::U112::from(1_500u64);
    let reserve1 = alloy::primitives::aliases::U112::from(2_500_000u64);
    // The PACK direction: engine typed state → the on-chain word.
    let word = U256::from_be_bytes(pack_v2_reserves_word(reserve0, reserve1).0);

    let mut scratch = scratch();
    let out = scratch
        .replay(&tx(7, &one_word(SLOT8, word)))
        .expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    // pack == journal word: the settled storage word at slot 8 IS the pack.
    assert_eq!(
        out.state
            .get(&POOL)
            .and_then(|acc| acc.storage.get(&U256::from(SLOT8)))
            .map(|s| s.present_value),
        Some(word)
    );

    // decode == engine state: the extractor's typed reserves equal the
    // source reserves exactly (0-wei).
    let states = extract_pool_post_states(&out, &descriptors([(POOL, PoolFamily::V2Pair)]));
    let state = post(&states, POOL);
    assert_eq!(state.family, PoolFamily::V2Pair);
    let expected = V2ReservesParts {
        reserve0,
        reserve1,
        block_timestamp_last: 0,
    };
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V2 { reserves }) => {
            assert_eq!(reserves, &expected, "reserves decoded 0-wei exact");
        }
        other => panic!("V2 frame must decode typed, got {other:?}"),
    }
}

/// A frame that never touches slot 8 surfaces no reserves the engine never
/// had: decode of the absent word is the all-zero reserves (never
/// fabricated from anywhere else).
#[test]
fn v2_slot_untouched_is_not_fabricated() {
    let mut scratch = scratch();
    let out = scratch
        .replay(&tx(
            7,
            &one_word(SLOT8 + U256::from(5u128), U256::from(42u128)),
        ))
        .expect("frame executes");
    let states = extract_pool_post_states(&out, &descriptors([(POOL, PoolFamily::V2Pair)]));
    let state = post(&states, POOL);
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V2 { reserves }) => {
            assert_eq!(
                reserves,
                &V2ReservesParts {
                    reserve0: alloy::primitives::aliases::U112::ZERO,
                    reserve1: alloy::primitives::aliases::U112::ZERO,
                    block_timestamp_last: 0,
                }
            );
        }
        other => panic!("expected V2 typed, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// V3: slot0 + liquidity + preimage tick recovery
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn v3_pack_decode_round_trip_recovers_tick_indices_from_preimages() {
    let sqrt_price_x96 = U256::from(1u128) << 96;
    let tick_post = -5010i32;
    let liquidity: u128 = 0x0000_0000_006b_5d49_e99f_8835;
    let (gross, net) = (7_000u128, -250i128);
    let spacing = 60;

    let word0 = U256::from_be_bytes(cl_slot0_tracked_word(sqrt_price_x96, tick_post).0);
    let word4 = U256::from_be_bytes(cl_liquidity_tracked_word(liquidity).0);
    let tick_slot = tick_mapping_slot_at_base(-2880, U256::from(5u64));
    let word_tick = U256::from_be_bytes(tick_word_of(gross, net).0);

    let mut scratch = scratch();
    let mut pairs = one_word(U256::ZERO, word0);
    pairs.push((U256::from(LIQ_SLOT), word4));
    pairs.push((tick_slot, word_tick));
    let out = scratch.replay(&tx(7, &pairs)).expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    let hint = 1200i32; // the pre-tx current tick (spacing-aligned anchor)
    let states = extract_pool_post_states(
        &out,
        &descriptors([(
            POOL,
            PoolFamily::V3 {
                layout: ClSlotLayout::UniswapV3,
                tick_spacing: spacing,
                current_tick_hint: Some(hint),
            },
        )]),
    );
    let state = post(&states, POOL);
    assert_eq!(
        state.family,
        PoolFamily::V3 {
            layout: ClSlotLayout::UniswapV3,
            tick_spacing: spacing,
            current_tick_hint: Some(hint)
        }
    );
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96: sqrt,
            tick,
            liquidity: liq,
            touched_ticks,
        }) => {
            assert_eq!(*sqrt, Some(sqrt_price_x96), "sqrtPriceX96: 0-wei");
            assert_eq!(*tick, Some(tick_post), "tick decoded from slot0 bits");
            assert_eq!(*liq, Some(liquidity));

            // The journalled tick slot recovered to its EXACT index via the
            // spacing-aligned preimage hunt around pre(hint)/post(-5010).
            assert_eq!(
                touched_ticks,
                &vec![TouchedTickWord {
                    tick: -2880,
                    liquidity_gross: gross,
                    liquidity_net: net,
                }],
                "tick index recovered exactly from the preimage"
            );
        }
        other => panic!("expected V3 typed, got {other:?}"),
    }
}

/// A touched pool word that is NOT a recoverable preimage (nor a tracked
/// scalar) never fabricates a tick index or a price.
#[test]
fn v3_unmatched_slots_never_fabricate_typed_fields() {
    // A keccak-shaped slot that is not on the anchor's spacing grid.
    let stray_slot = U256::from_be_bytes(alloy::primitives::keccak256([9u8; 64]).0);
    let mut scratch = scratch();
    let out = scratch
        .replay(&tx(7, &one_word(stray_slot, U256::from(u128::MAX))))
        .expect("frame executes");

    let states = extract_pool_post_states(
        &out,
        &descriptors([(
            POOL,
            PoolFamily::V3 {
                layout: ClSlotLayout::UniswapV3,
                tick_spacing: 60,
                current_tick_hint: Some(0),
            },
        )]),
    );
    let state = post(&states, POOL);
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96,
            tick,
            liquidity,
            touched_ticks,
        }) => {
            assert_eq!(*sqrt_price_x96, None);
            assert_eq!(*tick, None);
            assert_eq!(*liquidity, None);
            assert!(touched_ticks.is_empty(), "no fabricated tick indices");
        }
        other => panic!("expected V3 typed, got {other:?}"),
    }
}

/// The post-tx current tick (from the journal's own slot0) anchors the
/// preimage hunt too: a tick word FAR from the pre-tx hint but near the
/// post tick still recovers.
#[test]
fn v3_post_tick_anchors_the_preimage_hunt() {
    let word0 = U256::from_be_bytes(cl_slot0_tracked_word(U256::from(1u128) << 96, 88_000).0);
    let tick_slot = tick_mapping_slot_at_base(88_320, U256::from(5u64)); // 60*1472
    let word_tick = U256::from_be_bytes(tick_word_of(3_000, 400).0);

    let mut scratch = scratch();
    let mut pairs = one_word(U256::ZERO, word0);
    pairs.push((tick_slot, word_tick));
    let out = scratch.replay(&tx(7, &pairs)).expect("frame executes");

    let states = extract_pool_post_states(
        &out,
        &descriptors([(
            POOL,
            PoolFamily::V3 {
                layout: ClSlotLayout::UniswapV3,
                tick_spacing: 60,
                // Pre-tx hint 0; the touched tick sits near the journal's
                // OWN post tick (88000) — only the post anchor finds it.
                current_tick_hint: Some(0),
            },
        )]),
    );
    let state = post(&states, POOL);
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V3 { touched_ticks, .. }) => {
            assert_eq!(
                touched_ticks,
                &vec![TouchedTickWord {
                    tick: 88_320,
                    liquidity_gross: 3_000,
                    liquidity_net: 400,
                }]
            );
        }
        other => panic!("expected V3 typed, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// V4: per-known-poolId decode; explicit Unsupported otherwise
// ─────────────────────────────────────────────────────────────────────────

/// The descriptor default (empty known set) keeps a V4 `PoolManager` frame
/// explicitly `Unsupported` — the pre-wiring behavior, never a guess.
#[test]
fn v4_poolmanager_with_no_known_poolids_returns_unsupported() {
    let state_base = U256::from_be_bytes(alloy::primitives::keccak256([2u8; 32]).0);
    let mut scratch = scratch();
    let mut pairs = one_word(state_base, U256::from(1u128) << 96);
    pairs.push((state_base + U256::from(3u64), U256::from(1_000u64)));
    pairs.push((U256::from(LIQ_SLOT), U256::from(9u64)));
    let out = scratch.replay(&tx(7, &pairs)).expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    let family = PoolFamily::V4PoolManager {
        pools: V4PoolSet::default(),
    };
    let states = extract_pool_post_states(&out, &descriptors([(POOL, family.clone())]));
    let state = post(&states, POOL);
    assert_eq!(state.family, family);
    assert!(
        matches!(state.kind, PoolPostKind::Unsupported),
        "V4 PoolManager extraction is explicit Unsupported, got {:?}",
        state.kind
    );
}

/// A KNOWN V4 poolId's journalled `PoolManager` slots decode to the typed
/// post-state through the pinned layout: slot0 at `S_state`, liquidity at
/// `S_state+3`, and a tick word recovered from its `S_state+4` preimage —
/// 0-wei.
#[test]
fn v4_known_poolid_slots_decode_to_the_typed_post_state() {
    let pool_id = B256::new([0x21; 32]);
    let state_base = v4_pool_state_base_slot(pool_id);
    let sqrt_price_x96 = U256::from(1u128) << 96;
    let tick_post = -5010i32;
    let liquidity: u128 = 0x0000_0000_006b_5d49_e99f_8835;
    let (gross, net) = (7_000u128, -250i128);
    let tick = -5040i32;
    let spacing = 60;

    let word0 = encode_v4_slot0(V4Slot0Parts {
        sqrt_price_x96,
        tick: tick_post,
        protocol_fee: 0,
        lp_fee: 3000,
    });
    let tick_slot = v4_tick_mapping_slot(tick, state_base);
    let word_tick = U256::from_be_bytes(tick_word_of(gross, net).0);

    let mut scratch = scratch();
    let mut pairs = one_word(v4_slot0_slot(state_base), word0);
    pairs.push((
        v4_liquidity_slot(state_base),
        encode_v4_liquidity_slot(liquidity),
    ));
    pairs.push((tick_slot, word_tick));
    let out = scratch.replay(&tx(7, &pairs)).expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    let set = v4_set(vec![V4PoolDescriptor {
        pool_id,
        tick_spacing: spacing,
    }]);
    let family = PoolFamily::V4PoolManager { pools: set };
    let states = extract_pool_post_states(&out, &descriptors([(POOL, family.clone())]));
    let state = post(&states, POOL);
    assert_eq!(state.family, family);
    match &state.kind {
        PoolPostKind::Typed(TypedPoolPost::V4 {
            pool_id: got_id,
            sqrt_price_x96: sqrt,
            tick: got_tick,
            liquidity: liq,
            touched_ticks,
        }) => {
            assert_eq!(*got_id, pool_id, "the decoded pool is the known poolId");
            assert_eq!(*sqrt, Some(sqrt_price_x96), "slot0 sqrtPriceX96: 0-wei");
            assert_eq!(*got_tick, Some(tick_post), "slot0 tick decoded");
            assert_eq!(*liq, Some(liquidity), "liquidity: 0-wei");
            assert_eq!(
                touched_ticks,
                &vec![TouchedTickWord {
                    tick,
                    liquidity_gross: gross,
                    liquidity_net: net,
                }],
                "tick index recovered exactly from the S_state+4 preimage"
            );
        }
        other => panic!("expected V4 typed post-state, got {other:?}"),
    }
}

/// Slots that match NO known poolId stay `Unsupported`: the extractor never
/// guesses a pool identity from an unclaimed derived slot.
#[test]
fn v4_unknown_poolid_slots_stay_unsupported_never_guessed() {
    let known = B256::new([0xA1; 32]);
    let unknown_base = v4_pool_state_base_slot(B256::new([0xB2; 32]));
    let mut scratch = scratch();
    let mut pairs = one_word(
        v4_slot0_slot(unknown_base),
        encode_v4_slot0(V4Slot0Parts {
            sqrt_price_x96: U256::from(1u128) << 96,
            tick: 1234,
            ..Default::default()
        }),
    );
    pairs.push((
        v4_liquidity_slot(unknown_base),
        encode_v4_liquidity_slot(999),
    ));
    pairs.push((
        v4_tick_mapping_slot(1200, unknown_base),
        U256::from_be_bytes(tick_word_of(1, 2).0),
    ));
    let out = scratch.replay(&tx(7, &pairs)).expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    let set = v4_set(vec![V4PoolDescriptor {
        pool_id: known,
        tick_spacing: 60,
    }]);
    let states = extract_pool_post_states(
        &out,
        &descriptors([(POOL, PoolFamily::V4PoolManager { pools: set })]),
    );
    let state = post(&states, POOL);
    assert!(
        matches!(state.kind, PoolPostKind::Unsupported),
        "a slot set matching no known poolId must stay Unsupported, got {:?}",
        state.kind
    );
}

/// One frame that journals a V2 pair's reserves and a known V4 pool's
/// `Pool.State` extracts BOTH typed families — the family arms do not
/// collapse.
#[test]
fn mixed_v2_and_v4_frame_extracts_both_families() {
    let relay = address!("0x4444444444444444444444444444444444444444");
    let pool_id = B256::new([0x44; 32]);
    let state_base = v4_pool_state_base_slot(pool_id);
    let reserve0 = alloy::primitives::aliases::U112::from(1_500u64);
    let reserve1 = alloy::primitives::aliases::U112::from(2_500_000u64);
    let sqrt_price_x96 = U256::from(1u128) << 96;
    let tick_post = 1234i32;
    let liquidity: u128 = 42;

    let pairs = vec![
        (
            SLOT8,
            U256::from_be_bytes(pack_v2_reserves_word(reserve0, reserve1).0),
        ),
        (
            v4_slot0_slot(state_base),
            encode_v4_slot0(V4Slot0Parts {
                sqrt_price_x96,
                tick: tick_post,
                protocol_fee: 0,
                lp_fee: 3000,
            }),
        ),
        (
            v4_liquidity_slot(state_base),
            encode_v4_liquidity_slot(liquidity),
        ),
    ];
    let mut scratch = scratch_relayed(relay, POOL);
    let out = scratch
        .replay(&tx_to(relay, 7, &pairs))
        .expect("frame executes");
    assert!(matches!(out.status, ReplayStatus::Success));

    let set = v4_set(vec![V4PoolDescriptor {
        pool_id,
        tick_spacing: 60,
    }]);
    let states = extract_pool_post_states(
        &out,
        &descriptors([
            (relay, PoolFamily::V2Pair),
            (POOL, PoolFamily::V4PoolManager { pools: set }),
        ]),
    );

    match &post(&states, relay).kind {
        PoolPostKind::Typed(TypedPoolPost::V2 { reserves }) => {
            assert_eq!(
                reserves,
                &V2ReservesParts {
                    reserve0,
                    reserve1,
                    block_timestamp_last: 0,
                },
                "the relay's V2 slot decodes 0-wei"
            );
        }
        other => panic!("expected V2 typed for the relay, got {other:?}"),
    }
    match &post(&states, POOL).kind {
        PoolPostKind::Typed(TypedPoolPost::V4 {
            pool_id: got,
            sqrt_price_x96: sqrt,
            tick: got_tick,
            liquidity: liq,
            ..
        }) => {
            assert_eq!(*got, pool_id);
            assert_eq!(*sqrt, Some(sqrt_price_x96));
            assert_eq!(*got_tick, Some(tick_post));
            assert_eq!(*liq, Some(liquidity));
        }
        other => panic!("expected V4 typed for the managed pool, got {other:?}"),
    }
}

#[test]
fn descriptor_less_touched_accounts_contribute_nothing() {
    let mut scratch = scratch();
    let out = scratch
        .replay(&tx(7, &one_word(SLOT8, U256::from(1u128) << 112)))
        .expect("frame executes");
    // The sender + the pool contract are both touched; only STRANGER is a
    // tracked descriptor here.
    assert!(!out.touched.is_empty());
    let states = extract_pool_post_states(&out, &descriptors([(STRANGER, PoolFamily::V2Pair)]));
    assert!(
        !states.iter().any(|s| s.address == POOL),
        "untracked POOL must not surface"
    );
    assert!(
        !states.iter().any(|s| s.address == SENDER),
        "the sender is never a pool"
    );
    // STRANGER is tracked but NEVER touched by this frame (the tx targets
    // POOL) — it must not surface either. No tracked touched pool is left.
    assert!(
        states.is_empty(),
        "only touched+tracked pools surface: {states:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Live fork fixtures (network-gated) — parity vs the node's state diffs
// ─────────────────────────────────────────────────────────────────────────

// Fixture discovery mirrors `swap_capture_correctness`: candidates come
// from the chain's own activity (router fan-outs for V2, V3 `Swap` logs for
// V3), the node's per-tx storage diff (`prestateTracer` diffMode, immune to
// later-in-block interference) is the truth, and a fixture is only accepted
// when the frame both REPLAYS clean AND its pool was untouched by earlier
// same-block transactions (a parent-block-state replay would otherwise
// diverge from mid-block execution — the moving-head stability constraint).

/// Uniswap V2 Router 02.
const V2_ROUTER: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
/// Uniswap `SwapRouter02`.
const SWAP_ROUTER_02: Address = address!("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
/// Uniswap `SwapRouter` (02 predecessor, still fanned by aggregators).
const SWAP_ROUTER_V1: Address = address!("0xE592427A0AEce92De3Edee1F18E0157C05861564");
/// Uniswap Universal Router (both mainnet deployments).
const UNIVERSAL_ROUTER: Address = address!("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD");
const UNIVERSAL_ROUTER_2: Address = address!("0x66a9893cc07d91d95644aedd05d03f95e1dba8af");

const ROUTER_TARGETS: [Address; 5] = [
    V2_ROUTER,
    SWAP_ROUTER_02,
    SWAP_ROUTER_V1,
    UNIVERSAL_ROUTER,
    UNIVERSAL_ROUTER_2,
];

/// The canonical USDC/WETH V2 pair — used ONLY to learn the V2 family
/// runtime bytecode (`eth_getCode`) the fixture discriminates with:
/// keccak256 of it is the per-family identity (one runtime = one layout).
const V2_PAIR_USDC_WETH: Address = address!("0xB4e16d0168e52d35CACD2c6185b44281Ec28C9dc");

/// Cap on prestateTracer/replay attempts per scan (the fixture search must
/// stay bounded when a window won't yield a clean frame).
const MAX_TRIED_FRAMES: usize = 64;

/// The scratch stack never applies the strategy's state overrides, so the
/// live tests only need a well-formed (all-zero) warmup set to pass through
/// `BlockSimHandle::build`.
fn zero_warmup() -> degenbot_executor::WarmupSlots {
    degenbot_executor::WarmupSlots {
        weth_balance: U256::ZERO,
        erc6909_weth: U256::ZERO,
        erc6909_native: U256::ZERO,
    }
}

fn live_provider() -> alloy::providers::RootProvider {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    alloy::providers::ProviderBuilder::default().connect_http(rpc_url.parse().unwrap())
}

/// The node's per-tx storage diff (prestateTracer diffMode): post-tx values
/// per account (the truth — immune to later-in-block interference) together
/// with the pre-tx values.
async fn node_post_diff(
    provider: &alloy::providers::RootProvider,
    tx_hash: alloy::primitives::TxHash,
) -> (
    hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>>,
    hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>>,
) {
    use alloy::providers::Provider as _;
    let diff: serde_json::Value = provider
        .client()
        .request(
            "debug_traceTransaction",
            (
                tx_hash,
                serde_json::json!({"tracer": "prestateTracer", "tracerConfig": {"diffMode": true}}),
            ),
        )
        .await
        .unwrap();

    let hex_u256 = |s: &str| {
        let t = s.trim_start_matches("0x");
        if t.is_empty() {
            U256::ZERO
        } else {
            U256::from_str_radix(t, 16).unwrap()
        }
    };
    // reth trims leading hex zeros in diff keys/values — left-pad to width.
    let pad = |s: &str, width: usize| format!("{s:0>width$}");

    let grab = |side: &str| -> hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>> {
        let mut out = hashbrown::HashMap::new();
        let Some(accounts) = diff
            .pointer(&format!("/{side}"))
            .and_then(|p| p.as_object())
        else {
            return out;
        };
        for (addr_key, body) in accounts {
            let addr: Address = pad(addr_key, 40).parse().unwrap();
            let Some(storage) = body.get("storage").and_then(|s| s.as_object()) else {
                continue;
            };
            for (slot_key, value) in storage {
                let slot = hex_u256(&pad(slot_key, 64));
                let val = hex_u256(value.as_str().expect("storage value is hex"));
                out.entry(addr)
                    .or_insert_with(hashbrown::HashMap::new)
                    .insert(slot, val);
            }
        }
        out
    };
    (grab("post"), grab("pre"))
}

/// True when `pool` was already touched by a transaction EARLIER in its
/// block (any event from the pool address before `fixture_tx_index`) — such
/// a frame would replay against stale parent state and diverge mid-block.
async fn pool_touched_earlier_in_block(
    provider: &alloy::providers::RootProvider,
    pool: Address,
    pin: u64,
    fixture_tx_hash: alloy::primitives::TxHash,
    fixture_tx_index: u64,
) -> bool {
    use alloy::eips::BlockNumberOrTag;
    use alloy::providers::Provider as _;
    let logs = provider
        .get_logs(
            &alloy::rpc::types::Filter::new()
                .address(pool)
                .from_block(BlockNumberOrTag::Number(pin))
                .to_block(BlockNumberOrTag::Number(pin)),
        )
        .await
        .unwrap();
    logs.iter().any(|log| {
        log.transaction_hash.is_some_and(|h| h != fixture_tx_hash)
            && log.transaction_index.is_some_and(|i| i < fixture_tx_index)
    })
}

/// The packed tick (bits 160..184 of a slot0 word), two's-complement.
fn slot0_tick_of(word: U256) -> i32 {
    let tick_u = ((word >> 160u32) & U256::from(0x00ff_ffffu32)).to::<u128>();
    let tick_u = u32::try_from(tick_u).unwrap();
    if tick_u & 0x0080_0000 != 0 {
        (tick_u.cast_signed()) - (1 << 24)
    } else {
        tick_u.cast_signed()
    }
}

/// Parent block base fee (wei/gas) — the handle's projection input.
async fn parent_base_fee(provider: &alloy::providers::RootProvider, pin: u64) -> u128 {
    use alloy::eips::BlockNumberOrTag;
    use alloy::providers::Provider as _;
    let parent = provider
        .get_block_by_number(BlockNumberOrTag::Number(pin.saturating_sub(1)))
        .await
        .unwrap()
        .expect("parent block exists");
    u128::from(parent.header.base_fee_per_gas.unwrap_or(0))
}

/// Build a live handle + scratch over the fixture frame's parent block —
/// the `BlockSimHandle::build` shape `tests/frame_replay.rs` uses. The
/// handle borrows the override/anchor/cache locals, so the whole pairing
/// expands in the CALLER's scope: pass idents for the handle AND the
/// scratch (the scratch borrows the handle mutably).
macro_rules! live_scratch {
    ($scratch:ident, $handle:ident, $provider:expr, $pin:expr, $timestamp:expr, $base_fee:expr) => {
        let alloy_provider =
            degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new($provider.clone()));
        let override_params = degenbot_simulation::SimulationOverrideParams {
            owner: Address::ZERO,
            inject_code: false,
            injected_address: None,
            runtime_bytecode: Bytes::new(),
            warmup: zero_warmup(),
            weth_address: Address::ZERO,
            pool_manager_address: Address::ZERO,
        };
        let anchor = degenbot_bot::bot_core::SimAnchorState::default();
        let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
        let mut $handle = degenbot_simulation::BlockSimHandle::build(
            &alloy_provider,
            $base_fee.max(1),
            $pin.saturating_sub(1),
            $timestamp,
            &override_params,
            &anchor,
            &warm_cache,
            None,
            false,
        )
        .expect("live handle builds");
        #[expect(unused_mut)]
        let mut $scratch = $handle.scratch_evm().expect("scratch evm stacks");
    };
}

fn replayable_of(t: &alloy::rpc::types::Transaction) -> ReplayableTx {
    use alloy::consensus::Transaction as ConsensusTx;
    ReplayableTx {
        from: t.inner.signer(),
        to: t.to(),
        value: t.value(),
        data: t.input().clone(),
        gas_limit: t.gas_limit(),
        max_fee_per_gas: t.max_fee_per_gas(),
        max_priority_fee_per_gas: t.max_priority_fee_per_gas().unwrap_or(0),
        nonce: t.nonce(),
    }
}

/// keccak256 of a runtime code blob — the per-family identity the fixture
/// discriminates touched pool accounts with (one runtime = one layout, the
/// property the shared layout table leans on).
fn code_id(code: &alloy::primitives::Bytes) -> alloy::primitives::B256 {
    alloy::primitives::keccak256(code.as_ref())
}

/// A candidate transaction: first-sender-in-block, EIP-1559, no access
/// list — the seam's replayable input domain.
fn is_replayable_domain(t: &alloy::rpc::types::Transaction, parent_nonce: u64) -> bool {
    use alloy::consensus::Transaction as ConsensusTx;
    matches!(t.inner.tx_type(), alloy::consensus::TxType::Eip1559)
        && t.inner.access_list().is_none_or(|al| al.is_empty())
        && parent_nonce == t.nonce()
}

/// The V2 pair accounts the node's diff touched whose runtime bytecode
/// matches `family` — fixture pool discovery.
async fn v2_pair_hits(
    provider: &alloy::providers::RootProvider,
    post: &hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>>,
    family: alloy::primitives::B256,
    code_cache: &mut hashbrown::HashMap<Address, Option<alloy::primitives::B256>>,
) -> Vec<Address> {
    use alloy::providers::Provider as _;

    let mut hits = Vec::new();
    for (addr, storage) in post {
        if storage.is_empty() || !storage.contains_key(&U256::from(8u64)) {
            continue;
        }
        let id = if let Some(cached) = code_cache.get(addr) {
            *cached
        } else {
            let fetched: Option<alloy::primitives::B256> = provider
                .client()
                .request("eth_getCode", (*addr, "latest"))
                .await
                .ok()
                .map(|code: alloy::primitives::Bytes| code_id(&code));
            code_cache.insert(*addr, fetched);
            fetched
        };
        if id == Some(family) && !hits.contains(addr) {
            hits.push(*addr);
        }
    }
    hits
}

/// Live parity: a replayed V2 pair swap's extracted reserves equal the
/// chain's post-tx `reserves` word decoded through the same layout rows —
/// 0-wei error — for a clean-frame first-sender-in-block router swap.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
#[expect(clippy::too_many_lines)]
async fn live_v2_pair_swap_post_state_matches_the_node() {
    use alloy::consensus::Transaction as _;
    use alloy::eips::{BlockId, BlockNumberOrTag};
    use alloy::providers::Provider;

    let provider = live_provider();
    let head = provider.get_block_number().await.unwrap();
    let scan = match std::env::var("DEGENBOT_V2_FIXTURE_BLOCK") {
        Ok(b) => vec![b.parse::<u64>().unwrap()],
        Err(_) => (1..=300u64).rev().map(|i| head.saturating_sub(i)).collect(),
    };
    let pair_code: alloy::primitives::Bytes = provider
        .client()
        .request("eth_getCode", (V2_PAIR_USDC_WETH, "latest"))
        .await
        .unwrap();
    let pair_family = code_id(&pair_code);

    let mut code_cache = hashbrown::HashMap::new();
    let mut tried = 0usize;
    let mut verified = false;
    'blocks: for pin in scan {
        let Some(block) = provider
            .get_block_by_number(BlockNumberOrTag::Number(pin))
            .full()
            .await
            .unwrap()
        else {
            continue;
        };
        for t in block.transactions.txns() {
            if tried >= MAX_TRIED_FRAMES {
                break 'blocks;
            }
            // Cheap filters first: known router + replayable input domain.
            if !ROUTER_TARGETS.contains(&t.to().unwrap_or_default()) {
                continue;
            }
            let parent_nonce = provider
                .get_transaction_count(t.inner.signer())
                .block_id(BlockId::number(pin.saturating_sub(1)))
                .await
                .unwrap();
            if !is_replayable_domain(t, parent_nonce) {
                continue;
            }
            let (node_post, _) = node_post_diff(&provider, *t.inner.hash()).await;
            let pairs = v2_pair_hits(&provider, &node_post, pair_family, &mut code_cache).await;
            if pairs.is_empty() {
                continue;
            }
            // Same-block interference would diverge a parent-state replay.
            let tx_index = t
                .transaction_index
                .expect("rpc transaction carries its index");
            let mut clean = false;
            for &pair in &pairs {
                if pool_touched_earlier_in_block(&provider, pair, pin, *t.inner.hash(), tx_index)
                    .await
                {
                    clean = true;
                    break;
                }
            }
            if clean {
                continue;
            }

            tried += 1;
            let descriptors: hashbrown::HashMap<Address, PoolFamily> =
                pairs.iter().map(|a| (*a, PoolFamily::V2Pair)).collect();
            live_scratch!(
                scratch,
                handle,
                &provider,
                pin,
                block.header.timestamp,
                parent_base_fee(&provider, pin).await
            );
            let Ok(out) = scratch.replay(&replayable_of(t)) else {
                continue 'blocks; // not a settleable frame — skip the fixture
            };
            assert!(
                matches!(out.status, ReplayStatus::Success),
                "status {:?}",
                out.status
            );

            let states = extract_pool_post_states(&out, &descriptors);
            for pair in &pairs {
                let state = post(&states, *pair);
                assert_eq!(state.family, PoolFamily::V2Pair);
                let expected = decode_v2_reserves_word(
                    *node_post
                        .get(pair)
                        .and_then(|s| s.get(&U256::from(8u64)))
                        .expect("the node diff reported slot 8 for the pair"),
                );
                match &state.kind {
                    PoolPostKind::Typed(TypedPoolPost::V2 { reserves }) => {
                        assert_eq!(reserves, &expected, "0-wei reserves parity vs the node");
                    }
                    other => panic!("expected V2 typed post-state for {pair}, got {other:?}"),
                }
            }
            verified = true;
            break 'blocks;
        }
    }
    assert!(
        verified,
        "no clean V2 pair swap fixture found in the scanned blocks \
         (pin one via DEGENBOT_V2_FIXTURE_BLOCK)"
    );
}

/// Live parity: a replayed mainnet V3 swap whose qualifying pool moved
/// IN RANGE (no tick crossing) extracts slot0 (sqrtPriceX96 + tick) +
/// liquidity (+ any touched tick words) matching the node's post-tx words.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_v3_in_range_swap_post_state_matches_the_node() {
    v3_live_parity(false).await;
}

/// Live parity for a TICK-CROSSING swap: the recovered touched tick words
/// (indices recovered from keccak preimages via the spacing-aligned grid)
/// match the node's post-tx `ticks(tick)` words exactly.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_v3_tick_crossing_swap_touched_ticks_match_the_node() {
    v3_live_parity(true).await;
}

/// Shared V3 live parity. Fixture discovery follows the
/// `swap_capture_correctness` probe's shape: scan logs for V3 `Swap`
/// events, take the first replay-clean tx whose emitter pool of the
/// requested crossing class was untouched earlier in its block, then replay
/// through the scratch seam over the parent block and compare the extracted
/// typed post against the node's post-tx words — both sides decode through
/// the same layout rows.
#[expect(clippy::too_many_lines)]
async fn v3_live_parity(want_crossing: bool) {
    use alloy::eips::{BlockId, BlockNumberOrTag};
    use alloy::providers::Provider;

    let v3_swap_topic = degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
    let provider = live_provider();
    let head = provider.get_block_number().await.unwrap();
    let scan = match std::env::var("DEGENBOT_V3_FIXTURE_BLOCK") {
        Ok(b) => vec![b.parse::<u64>().unwrap()],
        Err(_) => (1..=300u64).rev().map(|i| head.saturating_sub(i)).collect(),
    };
    // A crossed frame touches keccak-derived slots (tick word / bitmap /
    // fee-growth-outside); an in-range frame only moves the scalar slots.
    let keccakish = |slot: &U256| *slot >> 128u32 != U256::ZERO;

    let mut tried = 0usize;
    let mut verified = false;
    'blocks: for pin in scan {
        if tried >= MAX_TRIED_FRAMES {
            break 'blocks;
        }
        let Some(block) = provider
            .get_block_by_number(BlockNumberOrTag::Number(pin))
            .full()
            .await
            .unwrap()
        else {
            continue;
        };
        // The V3 Swap events of this block: emitter pools per tx hash.
        let logs = provider
            .get_logs(
                &alloy::rpc::types::Filter::new()
                    .event_signature(v3_swap_topic)
                    .from_block(BlockNumberOrTag::Number(pin))
                    .to_block(BlockNumberOrTag::Number(pin)),
            )
            .await
            .unwrap();
        if logs.is_empty() {
            continue;
        }
        let mut pools_by_tx: hashbrown::HashMap<alloy::primitives::TxHash, Vec<Address>> =
            hashbrown::HashMap::new();
        for log in &logs {
            if log.block_number != Some(pin) {
                continue;
            }
            pools_by_tx
                .entry(log.transaction_hash.unwrap_or_default())
                .or_default()
                .push(log.address());
        }

        for t in block.transactions.txns() {
            let Some(pools) = pools_by_tx.get(t.inner.hash()) else {
                continue;
            };
            let parent_nonce = provider
                .get_transaction_count(t.inner.signer())
                .block_id(BlockId::number(pin.saturating_sub(1)))
                .await
                .unwrap();
            if !is_replayable_domain(t, parent_nonce) {
                continue;
            }
            let (node_post, node_pre) = node_post_diff(&provider, *t.inner.hash()).await;
            // Descriptor per emitter pool of the requested crossing class,
            // excluding pools an earlier same-block tx already touched.
            let tx_index = t
                .transaction_index
                .expect("rpc transaction carries its index");
            let mut descriptors = hashbrown::HashMap::new();
            for pool in pools {
                let Some(pool_post) = node_post.get(pool) else {
                    continue;
                };
                if pool_post.keys().any(keccakish) != want_crossing {
                    continue;
                }
                if pool_touched_earlier_in_block(&provider, *pool, pin, *t.inner.hash(), tx_index)
                    .await
                {
                    continue;
                }
                let spacing = tick_spacing_of(&provider, *pool).await;
                let hint = pool_pre_tx_tick(&provider, &node_pre, &node_post, *pool, pin).await;
                descriptors.insert(
                    *pool,
                    PoolFamily::V3 {
                        layout: ClSlotLayout::UniswapV3,
                        tick_spacing: spacing,
                        current_tick_hint: Some(hint),
                    },
                );
            }
            if descriptors.is_empty() {
                continue;
            }

            tried += 1;
            live_scratch!(
                scratch,
                handle,
                &provider,
                pin,
                block.header.timestamp,
                parent_base_fee(&provider, pin).await
            );
            let Ok(out) = scratch.replay(&replayable_of(t)) else {
                continue 'blocks; // not a settleable frame — skip the fixture
            };
            assert!(
                matches!(out.status, ReplayStatus::Success),
                "status {:?}",
                out.status
            );

            let states = extract_pool_post_states(&out, &descriptors);
            for (&pool, family) in &descriptors {
                let PoolFamily::V3 {
                    layout,
                    tick_spacing: _,
                    current_tick_hint: _,
                } = family
                else {
                    panic!("v3 parity only handles V3 descriptors");
                };
                let state = post(&states, pool);
                assert_eq!(&state.family, family);
                let pool_post = node_post.get(&pool).expect("diff reported this pool");
                let liquidity_slot = layout.liquidity_slot();
                match &state.kind {
                    PoolPostKind::Typed(TypedPoolPost::V3 {
                        sqrt_price_x96,
                        tick,
                        liquidity,
                        touched_ticks,
                    }) => {
                        // slot0 parity: the node post word carries the full
                        // packed shape when the frame moved it; a frame-read
                        // unchanged slot0 falls back to the parent-block
                        // word — both sides decode through the same rows.
                        if sqrt_price_x96.is_some() || tick.is_some() {
                            let expected0 = match pool_post.get(&U256::ZERO) {
                                Some(w) => *w,
                                None => chain_word(&provider, pool, U256::ZERO, pin).await,
                            };
                            let parts = decode_v3_slot0(expected0);
                            assert_eq!(*sqrt_price_x96, Some(parts.sqrt_price_x96), "sqrtP parity");
                            assert_eq!(*tick, Some(parts.tick), "tick parity");
                        }
                        // liquidity parity: the node diff reports the slot
                        // only when the frame changed it; an in-frame READ
                        // (journal-surfaced, unchanged value) falls back to
                        // the parent-block word — both are chain truth.
                        let expected_liq = match pool_post.get(&U256::from(liquidity_slot)) {
                            Some(w) => *w,
                            None => {
                                chain_word(&provider, pool, U256::from(liquidity_slot), pin).await
                            }
                        };
                        assert_eq!(
                            *liquidity,
                            Some((expected_liq & U256::from(u128::MAX)).to::<u128>()),
                            "liquidity parity"
                        );
                        // tick words: every recovered tick word matches the
                        // node's post-tx `ticks(tick)` word exactly; a
                        // crossing must recover at least one.
                        if want_crossing {
                            assert!(!touched_ticks.is_empty(), "a crossing touches tick words");
                        }
                        for tt in touched_ticks {
                            let slot = tick_mapping_slot_at_base(
                                tt.tick,
                                U256::from(layout.ticks_mapping_slot()),
                            );
                            // Crossed tick words land in the node's diff;
                            // in-word unchanged reads fall back to the
                            // parent-block word — both are chain truth.
                            let word = match pool_post.get(&slot) {
                                Some(w) => *w,
                                None => chain_word(&provider, pool, slot, pin).await,
                            };
                            let (gross, net) = decode_tick_word(word);
                            assert_eq!(
                                (tt.liquidity_gross, tt.liquidity_net),
                                (gross, net),
                                "tick word parity at tick {}",
                                tt.tick
                            );
                        }
                    }
                    other => panic!("expected V3 typed post-state for {pool}, got {other:?}"),
                }
            }
            verified = true;
            break 'blocks;
        }
    }
    assert!(
        verified,
        "no matching V3 swap fixture found in the scanned blocks \
         (pin one via DEGENBOT_V3_FIXTURE_BLOCK)"
    );
}

/// A parent-block storage word straight off the chain — the pre-tx truth
/// for journal-surfaced words the node's diff does not report (in-frame
/// reads of unchanged values).
async fn chain_word(
    provider: &alloy::providers::RootProvider,
    pool: Address,
    slot: U256,
    pin: u64,
) -> U256 {
    use alloy::providers::Provider as _;
    let raw: alloy::primitives::Bytes = provider
        .client()
        .request(
            "eth_getStorageAt",
            (pool, slot, format!("0x{:x}", pin.saturating_sub(1))),
        )
        .await
        .unwrap();
    U256::from_be_slice(&raw)
}

/// The pool's pre-tx current tick: the node diff's pre map slot0 (the
/// pool's state just before this frame), falling back to the post map
/// (slot0 untouched by this frame), falling back to the parent-block
/// storage word — the preimage anchor the engine would hold.
async fn pool_pre_tx_tick(
    provider: &alloy::providers::RootProvider,
    node_pre: &hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>>,
    node_post: &hashbrown::HashMap<Address, hashbrown::HashMap<U256, U256>>,
    pool: Address,
    pin: u64,
) -> i32 {
    use alloy::providers::Provider as _;
    if let Some(w) = node_pre
        .get(&pool)
        .and_then(|s| s.get(&U256::ZERO))
        .or_else(|| node_post.get(&pool).and_then(|s| s.get(&U256::ZERO)))
    {
        return slot0_tick_of(*w);
    }
    let raw: alloy::primitives::Bytes = provider
        .client()
        .request(
            "eth_getStorageAt",
            (pool, "0x0", format!("0x{:x}", pin.saturating_sub(1))),
        )
        .await
        .unwrap();
    slot0_tick_of(U256::from_be_slice(&raw))
}

/// The V3 pool's `tickSpacing()` via `eth_call` (int24 → sign-reconstructed).
async fn tick_spacing_of(provider: &alloy::providers::RootProvider, pool: Address) -> i32 {
    use alloy::providers::Provider as _;
    let raw: alloy::primitives::Bytes = provider
        .client()
        .request(
            "eth_call",
            (
                serde_json::json!({"to": pool, "input": "0xd0c93a7c"}),
                "latest",
            ),
        )
        .await
        .unwrap();
    let bits = U256::from_be_slice(&raw).to::<u128>();
    let bits = u32::try_from(bits).unwrap_or(60);
    if bits & 0x0080_0000 != 0 {
        (bits.cast_signed()) - (1 << 24)
    } else {
        bits.cast_signed()
    }
}
