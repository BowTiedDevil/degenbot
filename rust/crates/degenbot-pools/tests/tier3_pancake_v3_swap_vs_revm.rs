//! Tier-3 PancakeSwap V3 `PancakeV3Pool.swap` on-chain accuracy oracle
//! (epic `CMORFZ` task `BXIOWT`). Deploys the REAL `PancakeV3Pool` — the
//! Etherscan-verified deployment (pool 0x1445F32D1A74872bA41f3D8cF4022E9996120b31,
//! solc 0.7.6, source vendored under `tier3-oracle/lib/pancake-src/`) via the
//! `PancakeV3SwapOracleHarness`, seeds its storage slot-for-slot, drives
//! `pool.swap`, and proves Rust `v3_simulate_swap` is BYTE-EXACT to the
//! PancakeSwap pool's swap walk (amounts, post-sqrtPrice, post-tick,
//! post-liquidity).
//!
//! The shared deploy → setup → seed → swap → read-back pipeline lives in
//! [`tier3_v3_common`](crate::tier3_v3_common) (created by the V3 task
//! `6DLK7I`); this file is the PancakeSwap fork consumer — it declares its
//! `V3Fork` (EIP-170 override), its fork-specific storage seeder
//! (`v3_pancakeswap_storage_slots` — a 2-word `slot0`, liquidity@5, ticks@6,
//! tickBitmap@7), and its 9-field `Swap`-event-variant assertions. Per epic
//! `CMORFZ` this adds H1 rejection-reason airtightness, an H3 pinned edge
//! corpus (incl. a protocol-fee-on case), and an H4 widened proptest.
//!
//! ## The variant under test
//!
//! PancakeSwap V3 forked Uniswap V3 but the emitted `Swap` event APPENDS two
//! `uint128 protocolFeesToken0/1` fields, so its `topic0` differs
//! (`0x19b47279…` vs Uniswap's `0xc42079f9…`) and its data is 7 words (224
//! bytes) vs Uniswap's 5 (160). The 5 state fields are byte-identical. This
//! test verifies the variant end-to-end:
//!   1. swap MATH is byte-exact to the canonical PancakeSwap pool;
//!   2. the emitted `Swap` log is NOT decodable by the Uniswap V3 decoder
//!      (`V3_SWAP_TOPIC`) and IS decodable by the PancakeSwap decoder
//!      (`decode_v3_pancakeswap_swap_log`), whose decoded state matches the
//!      Rust sim byte-exact — the exact drift the
//!      `v3_pancakeswap_swap_decoder` fixes;
//!   3. with a protocol fee seeded (`feeProtocol != 0`) the event's trailing
//!      protocol-fee word is nonzero (the swap accrued a protocol cut) yet the
//!      state walked is byte-identical.
//!
//! ## EIP-170 note
//!
//! The PancakeSwap fork's embedded pool-creation code makes the harness's
//! deployed code ~25.0KB — over the 24.6KB EIP-170 limit the Uniswap harness
//! stays under. `V3Fork.raise_eip170` is set so the shared driver raises revm's
//! effective code-size limits to `usize::MAX`.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite; the bytecode is loaded
//! from the committed `tier3-oracle/artifacts/` tree. Artifact integrity is
//! enforced by `tier3_harness_artifacts.rs` (source-hash, toolchain-free) and
//! `tier3-oracle/verify-tier3-artifacts.sh` (compile-vs-use).

#![expect(clippy::doc_markdown)] // Solidity/V3 identifiers (slot0, tickBitmap…) in doc comments
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tier3_v3_common;

use std::collections::HashSet;

use alloy::primitives::{aliases::I256, Address, U256};
use alloy::rpc::types::Log as RpcLog;
use proptest::prelude::*;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;

use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_decoders::v3_pancakeswap_swap_decoder::{
    decode_v3_pancakeswap_swap_log, V3_PANCAKESWAP_SWAP_TOPIC,
};
use degenbot_decoders::v3_swap_decoder::{decode_v3_swap_log, V3_SWAP_TOPIC};
use degenbot_pools::v3_pancakeswap_storage_slots::{
    encode_pancake_v3_slot0_word1, pancake_v3_tick_bitmap_word_slot, pancake_v3_tick_mapping_slot,
};
use degenbot_pools::v3_state::{v3_simulate_swap, SimulateSwapError, V3PoolState, V3SwapOutcome};
use degenbot_pools::v3_storage_slots::{
    compute_v3_tick_bitmap_word_from_raw, encode_v3_liquidity_slot, encode_v3_slot0_fresh,
    encode_v3_tick_info_slot,
};

use tier3_v3_common::{decode_error_string, dense_state, run_onchain_swap, ProbeOutcome, V3Fork};

/// The PancakeSwap V3 fork descriptor. Its harness exceeds EIP-170 (~25KB), so
/// `raise_eip170` is set and the shared driver raises revm's code-size limits.
const FORK: V3Fork = V3Fork {
    harness_sol: "PancakeV3SwapOracleHarness.sol",
    harness_contract: "PancakeV3SwapOracleHarness",
    raise_eip170: true,
};

/// Protocol-fee share (in `PROTOCOL_FEE_DENOMINATOR`=10000 units) applied to
/// token0 when the protocol-fee-on seeding variant is used (`1000` = 10% of the
/// LP fee).
const PANCAKE_PROTOCOL_FEE0: u32 = 1000;

/// PancakeSwap V3 storage layout (a real divergence from Uniswap V3, surfaced
/// by this oracle): `slot0.feeProtocol` is `uint32` (2× uint16) instead of
/// Uniswap's `uint8`, so the packed `Slot0` struct spans TWO storage words —
/// `unlocked` lives at slot 1 bit 32 — and every following slot shifts by one:
/// `liquidity`@5, `ticks`@6, `tickBitmap`@7. The Uniswap `v3_storage_slots`
/// encoders (liquidity@4, ticks@5, tickBitmap@6) therefore WOULD misread a
/// PancakeSwap pool; the engine must use these fork-aware slot indices when
/// syncing/seeding pancake pools directly.
///
/// `slot0` word 0 reuses `encode_v3_slot0_fresh` (price/tick/observations are
/// identical; the bit-240 `unlocked` it sets is unused padding here); word 1 =
/// `encode_pancake_v3_slot0_word1(feeProtocol, unlocked)`. Protocol fee is
/// OFF by default; use [`seed_pancake_pool_storage_with_protocol_fee`] to
/// accrue a token0 protocol cut (exercises the 9-field event's trailing words).
fn seed_pancake_pool_storage(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
) {
    seed_pancake_pool_storage_impl(db, pool, state, tick_spacing, 0);
}

/// Same seeder but with a nonzero token0 protocol fee (a real divergence the
/// byte-exact oracle exercises): the swap's LP fee is split with the protocol,
/// so the `Swap` event's trailing `protocolFeesToken0` word is nonzero while
/// the state walk is byte-identical.
fn seed_pancake_pool_storage_with_protocol_fee(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
) {
    seed_pancake_pool_storage_impl(db, pool, state, tick_spacing, PANCAKE_PROTOCOL_FEE0);
}

fn seed_pancake_pool_storage_impl(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
    fee_protocol: u32,
) {
    // slot0 word 0: sqrtPrice | tick | observation{Index,Cardinality,CardinalityNext}.
    db.insert_account_storage(
        pool,
        U256::from(0u64),
        encode_v3_slot0_fresh(state.sqrt_price_x96, state.tick),
    )
    .expect("seed slot0 word0");
    // slot0 word 1: feeProtocol (32b) | unlocked (bit 32, =true).
    db.insert_account_storage(
        pool,
        U256::from(1u64),
        encode_pancake_v3_slot0_word1(fee_protocol, true),
    )
    .expect("seed slot0 word1 (unlocked)");
    // liquidity @ slot 5 (after the 2-word slot0 + feeGrowth×2 + protocolFees).
    db.insert_account_storage(
        pool,
        U256::from(5u64),
        encode_v3_liquidity_slot(state.liquidity),
    )
    .expect("seed liquidity");
    for (tick, info) in &state.tick_data {
        db.insert_account_storage(
            pool,
            pancake_v3_tick_mapping_slot(*tick),
            encode_v3_tick_info_slot(info),
        )
        .expect("seed tick info");
    }
    let mut word_positions: HashSet<i16> = HashSet::new();
    for &tick in state.tick_data.keys() {
        let compressed = tick.div_euclid(tick_spacing);
        let word_pos = i16::try_from(compressed >> 8).unwrap_or(0);
        word_positions.insert(word_pos);
    }
    for word_pos in word_positions {
        let word_value =
            compute_v3_tick_bitmap_word_from_raw(&state.tick_data, tick_spacing, word_pos);
        db.insert_account_storage(pool, pancake_v3_tick_bitmap_word_slot(word_pos), word_value)
            .expect("seed tickBitmap word");
    }
}

/// Assert the swap-tx logs carry exactly-one PancakeSwap `Swap` event that is
/// NOT decodable by the Uniswap V3 decoder, is decodable by the PancakeSwap
/// decoder, and whose 5 state fields match the sim byte-exact. Optionally
/// asserts the trailing `protocolFeesToken0` word is nonzero (`protocol_fee_on`).
fn assert_event_variant(logs: &[RpcLog], sim: &V3SwapOutcome, protocol_fee_on: bool) {
    let swap_log = logs
        .iter()
        .find(|l| l.topics().first() == Some(&V3_PANCAKESWAP_SWAP_TOPIC))
        .unwrap_or_else(|| panic!("no PancakeSwap Swap event emitted in {} log(s)", logs.len()));

    // 2. THE SWAP EVENT VARIANT: 9-field, distinct topic0, 224-byte data.
    assert_eq!(
        swap_log.topics().first(),
        Some(&V3_PANCAKESWAP_SWAP_TOPIC),
        "PancakeSwap Swap topic0"
    );
    assert_ne!(
        swap_log.topics().first(),
        Some(&V3_SWAP_TOPIC),
        "must differ from Uniswap V3 Swap topic0"
    );
    assert_eq!(
        swap_log.data().data.len(),
        224,
        "9-field (7-word) event data"
    );
    // The Uniswap V3 decoder must NOT claim this event (variant is distinct).
    assert!(
        !logs.iter().any(|l| decode_v3_swap_log(l).is_some()),
        "Uniswap V3 decoder must not match the PancakeSwap Swap"
    );

    // 3. The PancakeSwap decoder decodes it and matches the sim byte-exact.
    let decoded = decode_v3_pancakeswap_swap_log(swap_log).expect("PancakeSwap decoder");
    assert_eq!(decoded.amount0.unsigned_abs(), sim.amount0, "event amount0");
    assert_eq!(decoded.amount1.unsigned_abs(), sim.amount1, "event amount1");
    assert_eq!(
        decoded.sqrt_price_x96, sim.sqrt_price_x96,
        "event sqrtPriceX96"
    );
    assert_eq!(
        decoded.liquidity.to::<u128>(),
        sim.liquidity,
        "event liquidity"
    );
    assert_eq!(decoded.tick, sim.tick, "event tick");

    // 4. protocol-fee trail: when seeded on, the trailing protocolFeesToken0
    //    word (bytes 160..192 of the 224-byte data) must be nonzero (a real
    //    protocol cut accrued) — but only for a token0-in (zeroForOne) swap.
    if protocol_fee_on {
        let data = swap_log.data().data.as_ref();
        let proto0 = U256::from_be_bytes::<32>(data[160..192].try_into().unwrap());
        assert!(
            !proto0.is_zero(),
            "protocolFeesToken0 must be nonzero when protocol fee is on"
        );
    }
}

/// The full byte-exact oracle for one case, with H1 rejection-reason
/// airtightness: on-chain Accepted ⇒ engine Ok (compared byte-for-byte + event
/// variant); on-chain Revert (a verdict) ⇒ engine `NotComputable`; only a
/// verbless Halt (the OOG gas trap) is a legitimate skip. `protocol_fee_on`
/// selects the parallel seeder that splits the LP fee with the protocol.
#[expect(clippy::match_same_arms)] // two parity arms legitimately share an empty body
fn assert_byte_exact_and_variant(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount: I256,
    sqrt_price_limit: u128,
    protocol_fee_on: bool,
) {
    let sim = v3_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount,
        U256::from(sqrt_price_limit),
    );
    let outcome = run_onchain_swap(
        &FORK,
        if protocol_fee_on {
            seed_pancake_pool_storage_with_protocol_fee
        } else {
            seed_pancake_pool_storage
        },
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount,
        sqrt_price_limit,
    );

    match (outcome, &sim) {
        (ProbeOutcome::Accepted(res), Ok(sim)) => {
            assert_eq!(res.amount0, sim.amount0, "amount0 byte-exact");
            assert_eq!(res.amount1, sim.amount1, "amount1 byte-exact");
            assert_eq!(
                res.post_sqrt, sim.sqrt_price_x96,
                "post sqrtPriceX96 byte-exact"
            );
            assert_eq!(res.post_tick, sim.tick, "post tick byte-exact");
            assert_eq!(res.post_liq, sim.liquidity, "post liquidity byte-exact");
            assert_event_variant(&res.logs, sim, protocol_fee_on);
        }
        (ProbeOutcome::Accepted(_), Err(e)) => {
            panic!("on-chain ACCEPTED but engine rejected: {e:?}")
        }
        (ProbeOutcome::Reverted { .. }, Err(SimulateSwapError::NotComputable)) => {
            // Parity: both reject — no silent skip (the on-chain verdict was a
            // Solidity revert and the engine agrees).
        }
        (ProbeOutcome::Reverted { reason }, _) => {
            let reason_str = decode_error_string(reason.as_ref())
                .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())));
            match sim {
                Ok(s) => panic!("on-chain REVERTED ({reason_str}) but engine produced {s:?}"),
                Err(SimulateSwapError::MissingTickWord(w)) => {
                    panic!("on-chain REVERTED ({reason_str}) but engine misses word {w}")
                }
                Err(SimulateSwapError::NotComputable) => unreachable!(),
            }
        }
        (ProbeOutcome::Halted(_), _) => {
            // Verbless halt (OOG gas trap or deploy failure) — no EVM verdict
            // to compare against the engine. The only legitimate skip.
        }
    }
}

/// Pinned dense-band oracle: byte-exact swap across the dense band + the Swap
/// event variant decodes only via the PancakeSwap decoder, matching the sim.
#[test]
fn pancake_v3_pool_swap_byte_exact_and_event_variant() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21
    let current_tick = 120i32; // mid-word

    let state = dense_state(liq, tick_spacing, k_positions, current_tick);
    let amount_specified = I256::try_from(U256::from(1_000_000_000_000_000_000_000u128)).unwrap(); // 1e21
    let limit_tick = current_tick - 4 * tick_spacing;
    let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
        .unwrap()
        .to::<u128>();

    assert_byte_exact_and_variant(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        sqrt_price_limit,
        false,
    );
}

/// H3 — pinned deterministic edge corpus (not proptest): 1-wei at wei-scale
/// liquidity, tiny + large liquidity, both directions, two fee tiers, plus a
/// protocol-fee-on case exercising the 9-field event's trailing word. Each case
/// runs the full H1 byte-exact oracle + event-variant assertion.
#[test]
fn pancake_v3_pool_edge_corpus_is_byte_exact() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    // (liq, amount, fee, zfo, protocol_fee_on).
    let cases: &[(u128, u128, u32, bool, bool)] = &[
        // 1-wei amount at wei-scale liquidity.
        (2, 1, 3000, true, false),
        (2, 1, 3000, false, false),
        // Tiny liquidity, wei-scale amounts (floor-division-sensitive region).
        (1_000, 5, 3000, true, false),
        (1_000, 5, 3000, false, false),
        (100_000, 100, 500, true, false),
        (100_000, 100, 500, false, false),
        // Large liquidity, proportionally large amount.
        (
            1_000_000_000_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
            3000,
            true,
            false,
        ), // 1e30 liq / 1e24 in
        (
            1_000_000_000_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
            500,
            false,
            false,
        ),
        // Boundary amount pushing deep into the band.
        (
            1_000_000_000_000_000_000_000u128,
            1_500_000_000_000_000_000_000u128,
            3000,
            true,
            false,
        ),
        (
            1_000_000_000_000_000_000_000u128,
            1_500_000_000_000_000_000_000u128,
            3000,
            false,
            false,
        ),
        // Protocol-fee-on: token0-in swap accruing a protocol cut.
        (
            1_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000u128,
            3000,
            true,
            true,
        ),
    ];

    for &(liq, amount, fee, zfo, protocol_fee_on) in cases {
        let state = dense_state(liq, tick_spacing, k_positions, current_tick);
        let amount_in = I256::try_from(U256::from(amount)).unwrap();
        let dir = if zfo { -1 } else { 1 };
        let limit_tick = current_tick + dir * 3 * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();
        assert_byte_exact_and_variant(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_in,
            sqrt_price_limit,
            protocol_fee_on,
        );
    }
}

/// H4 — widened proptest domain (`prop_oneof!` self-consistent arms), the
/// PancakeSwap fork analog of the V3 strategy: nominal/tiny/large liquidity,
/// fee tiers 500 + 3000, both directions, band depth; amounts coupled to
/// liquidity so each walk terminates (the OOG trap). Each case runs the H1
/// byte-exact oracle + the 9-field event-variant assertion.
fn pancake_case_strategy() -> impl Strategy<Value = (u128, U256, i32, i32, u32)> {
    // Nominal wide dynamic range.
    let nominal = (1u32..23u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        (
            Just(liq),
            1u32..200u32,
            0i32..2,
            1i32..4,
            prop_oneof![Just(500u32), Just(3000u32)],
        )
            .prop_map(move |(_, frac, zfo, sink, fee)| {
                (
                    liq,
                    U256::from(liq) / U256::from(1_000_000u64) * U256::from(frac),
                    zfo,
                    sink,
                    fee,
                )
            })
    });
    // Tiny liquidity + wei-scale amounts (floor-division region).
    let tiny = (0u32..7u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        let active = liq * 8;
        (
            Just(liq),
            1u128..(active.min(1_000_000u128) + 1),
            0i32..2,
            1i32..3,
            prop_oneof![Just(500u32), Just(3000u32)],
        )
            .prop_map(move |(_, amount, zfo, sink, fee)| (liq, U256::from(amount), zfo, sink, fee))
    });
    // Large liquidity + proportionally large amounts.
    let large = (23u32..31u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        (
            Just(liq),
            100u32..2000u32,
            0i32..2,
            1i32..4,
            prop_oneof![Just(500u32), Just(3000u32)],
        )
            .prop_map(move |(_, frac, zfo, sink, fee)| {
                (
                    liq,
                    U256::from(liq) / U256::from(1_000_000u64) * U256::from(frac),
                    zfo,
                    sink,
                    fee,
                )
            })
    });
    prop_oneof![nominal, tiny, large]
}

/// Proptest: dense-band swap byte-exactness + event-variant decode across the
/// widened (state, amount, direction, fee) domain.
#[test]
fn pancake_v3_pool_swap_matches_sim_proptest() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    proptest!(|(case in pancake_case_strategy())| {
        let (liq, amount, zfo, sink_ticks, fee) = case;
        if amount > U256::from(i128::MAX) {
            return Ok(());
        }
        let amount_in = I256::try_from(amount).unwrap();
        if amount_in.is_zero() {
            return Ok(());
        }
        let state = dense_state(liq, tick_spacing, k_positions, current_tick);
        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * sink_ticks * tick_spacing;
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        assert_byte_exact_and_variant(
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount_in,
            sqrt_price_limit,
            false,
        );
    });
}
