//! Tier-3b end-to-end V3 `Pool.swap` oracle (ergo task `2LTKVO`, epic
//! `UP5NH6`, hardened per epic `CMORFZ` task `6DLK7I`). Deploys the canonical
//! v3-core `UniswapV3Pool` as real bytecode via the `V3SwapOracleHarness`
//! (solc-0.7.6 compiled), seeds its `slot0`/`liquidity`/`ticks`/`tickBitmap`
//! storage directly from a `V3PoolState` using the `degenbot_pools::v3_storage_slots`
//! encoders, and proves Rust `v3_simulate_swap` is BYTE-EXACT to the real
//! pool's `Swap` walk — amounts, post-sqrtPrice, post-tick, post-liquidity.
//!
//! The shared deploy → setup → seed → swap → read-back pipeline lives in
//! [`tier3_v3_common`](crate::tier3_v3_common) (and is reused by the
//! Pancake-V3 oracle); this file owns the Uniswap fork's storage seeder and
//! the oracle assertions. Hardening (epic `CMORFZ`):
//! - **H1 rejection-reason airtightness**: swaps are probed through
//!   [`ProbeOutcome`](crate::tier3_v3_common::ProbeOutcome), which keeps a
//!   Solidity `Revert` (a verdict) distinct from a verbless `Halt` (the
//!   documented OOG gas trap). A `Revert` MUST be matched by an engine
//!   `NotComputable`; only a `Halt` (no EVM verdict) is a legitimate skip.
//! - **H3**: a pinned deterministic edge corpus (`#[test]`) — 1-wei amounts at
//!   wei-scale liquidity, tiny + large liquidity, both directions, two fee
//!   tiers.
//! - **H4**: the proptest now sweeps tiny/large liquidity, a second fee tier,
//!   a pre-vetted set of mid-word current ticks, and boundary amounts.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The canonical v3-core
//! bytecode is loaded from the committed `tier3-oracle/artifacts/` tree (no
//! solc/forge needed to RUN). Artifact integrity is enforced two ways:
//! `tier3_harness_artifacts.rs` hashes the tracked sources (toolchain-free),
//! and `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)] // Solidity/V3 identifiers (MIN_TICK, slot0…) in doc comments

mod tier3_v3_common;

use std::collections::HashMap;

use alloy::primitives::{aliases::I256, Address, U256};
use proptest::prelude::*;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;

use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::state_history::{ReorgJournal, V3BlockDelta};
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegistrationLifecycle, SimulateSwapError, TickRangeCache,
    V3PoolState,
};
use degenbot_pools::v3_storage_slots::{
    compute_v3_tick_bitmap_word_from_raw, encode_v3_liquidity_slot, encode_v3_slot0_fresh,
    encode_v3_tick_info_slot, v3_tick_bitmap_word_slot, v3_tick_mapping_slot,
};
use degenbot_pools::TickInfo;

use tier3_v3_common::{
    build_arbitrary_v3_state, decode_error_string, run_onchain_swap, ArbV3Position,
    OnChainSwapResult, ProbeOutcome, V3Fork,
};

/// The canonical Uniswap V3 fork descriptor (Uniswap harness is under
/// EIP-170 — no limit raise).
const FORK: V3Fork = V3Fork {
    harness_sol: "V3SwapOracleHarness.sol",
    harness_contract: "V3SwapOracleHarness",
    raise_eip170: false,
};

/// Seed a revm `CacheDB`'s pool account from a `V3PoolState` using the v3-core
/// storage-slot encoders. Writes `slot0` (slot 0), `liquidity` (slot 4), every
/// `ticks(tick)` entry (mapping base slot 5), and every occupied `tickBitmap(word)`
/// (mapping base slot 6) — the full swap-math read set.
fn seed_v3_pool_storage(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
) {
    db.insert_account_storage(
        pool,
        U256::from(0u64),
        encode_v3_slot0_fresh(state.sqrt_price_x96, state.tick),
    )
    .expect("seed slot0");
    db.insert_account_storage(
        pool,
        U256::from(4u64),
        encode_v3_liquidity_slot(state.liquidity),
    )
    .expect("seed liquidity");
    for (tick, info) in &state.tick_data {
        db.insert_account_storage(
            pool,
            v3_tick_mapping_slot(*tick),
            encode_v3_tick_info_slot(info),
        )
        .expect("seed tick info");
    }
    let mut word_positions: std::collections::HashSet<i16> = std::collections::HashSet::new();
    for &tick in state.tick_data.keys() {
        let compressed = tick.div_euclid(tick_spacing);
        let word_pos = i16::try_from(compressed >> 8).unwrap_or(0);
        word_positions.insert(word_pos);
    }
    for word_pos in word_positions {
        let word_value =
            compute_v3_tick_bitmap_word_from_raw(&state.tick_data, tick_spacing, word_pos);
        db.insert_account_storage(pool, v3_tick_bitmap_word_slot(word_pos), word_value)
            .expect("seed tickBitmap word");
    }
}

/// The `PoolSeeder` for the Uniswap fork — pass `seed_v3_pool_storage` where a
/// `PoolSeeder` is expected.
fn uniswap_seeder(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
) {
    seed_v3_pool_storage(db, pool, state, tick_spacing);
}

/// Drive one on-chain swap and REQUIRES the pool to accept it (revert / halt
/// here is a test-fixture failure, not parity — the pinned tests only assert
/// byte-exact accepted swaps).
fn probe_accepted(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> OnChainSwapResult {
    match run_onchain_swap(
        &FORK,
        uniswap_seeder,
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        sqrt_price_limit,
    ) {
        ProbeOutcome::Accepted(r) => r,
        ProbeOutcome::Reverted { reason } => panic!(
            "on-chain swap reverted: {}",
            decode_error_string(reason.as_ref())
                .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())))
        ),
        ProbeOutcome::Halted(m) => panic!("on-chain swap halted: {m}"),
    }
}

/// The full byte-exact oracle for one case, with H1 rejection-reason
/// airtightness: an on-chain `Accepted` MUST be matched by an engine `Ok`
/// (then compared byte-for-byte); an on-chain `Revert` (a verdict) MUST be
/// matched by an engine `NotComputable`; only a verbless `Halt` (the documented
/// OOG gas trap / deploy failure — no EVM verdict) is a legitimate skip.
#[allow(clippy::too_many_lines)]
fn assert_byte_exact(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount: I256,
    sqrt_price_limit: u128,
) {
    let sim = v3_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount,
        U256::from(sqrt_price_limit),
    );
    match run_onchain_swap(
        &FORK,
        uniswap_seeder,
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount,
        sqrt_price_limit,
    ) {
        ProbeOutcome::Accepted(res) => match sim {
            Ok(sim) => {
                assert_eq!(res.amount0, sim.amount0, "amount0 byte-exact");
                assert_eq!(res.amount1, sim.amount1, "amount1 byte-exact");
                assert_eq!(
                    res.post_sqrt, sim.sqrt_price_x96,
                    "post sqrtPriceX96 byte-exact"
                );
                assert_eq!(res.post_tick, sim.tick, "post tick byte-exact");
                assert_eq!(res.post_liq, sim.liquidity, "post liquidity byte-exact");
            }
            Err(e) => panic!("on-chain ACCEPTED but engine rejected: {e:?}"),
        },
        ProbeOutcome::Reverted { reason } => {
            let reason_str = decode_error_string(reason.as_ref())
                .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())));
            match sim {
                Err(SimulateSwapError::NotComputable) => {
                    // Parity: both reject. Not silently skipped — the on-chain
                    // verdict was a Solidity revert and the engine agrees.
                }
                Ok(s) => panic!("on-chain REVERTED ({reason_str}) but engine produced {s:?}"),
                Err(SimulateSwapError::MissingTickWord(w)) => {
                    panic!("on-chain REVERTED ({reason_str}) but engine misses tick word {w}")
                }
            }
        }
        ProbeOutcome::Halted(_) => {
            // Verbless halt (OOG gas trap or deploy failure) — no EVM verdict
            // to compare against the engine, so nothing to cross-check. This is
            // the ONLY legitimate skip, and it is not a math divergence.
        }
    }
}

/// The reads-back foundation test unchanged: proves the deploy → setupPool →
/// storage-encoder-seeding pipeline reads back byte-exact (the prerequisite
/// for the swap byte-exact assertion).
#[test]
#[allow(clippy::too_many_lines)]
fn v3_pool_reads_back_seeded_state_byte_exact() {
    use revm::context_interface::ContextTr;
    use revm::primitives::TxKind;
    use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

    let fee = 3000u32;
    let tick_spacing = 60i32;

    let mut init_code =
        tier3_v3_common::load_creation_bytecode("V3SwapOracleHarness.sol", "V3SwapOracleHarness");
    init_code.extend_from_slice(&tier3_v3_common::harness_constructor_args(
        fee,
        tick_spacing,
    ));
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    let deploy_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(revm::primitives::Bytes::from(init_code))
                .build()
                .expect("deploy tx"),
        )
        .expect("deploy transact");
    let harness = match &deploy_res.result {
        ExecutionResult::Success {
            output: Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => panic!("deploy did not create a contract: {other:?}"),
    };
    evm.commit(deploy_res.state);

    let setup_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(revm::primitives::Bytes::from(
                    tier3_v3_common::selector("setupPool()").to_vec(),
                ))
                .build()
                .expect("setupPool tx"),
        )
        .expect("setupPool transact");
    match &setup_res.result {
        ExecutionResult::Success { .. } => {}
        other => panic!("setupPool failed: {other:?}"),
    }
    evm.commit(setup_res.state);

    let pool_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(2_000_000)
                .data(revm::primitives::Bytes::from(
                    tier3_v3_common::selector("pool()").to_vec(),
                ))
                .build()
                .expect("pool() tx"),
        )
        .expect("pool() transact");
    let pool_out = match &pool_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => panic!("pool() reverted: {other:?}"),
    };
    evm.commit(pool_res.state);
    let mut pool_buf = [0u8; 32];
    pool_buf.copy_from_slice(&pool_out.as_ref()[0..32]);
    let pool = Address::from_slice(&pool_buf[12..32]);

    let liq = 1_000_000_000_000_000_000u128;
    let state = state_at_tick_zero(liq, tick_spacing);
    seed_v3_pool_storage(evm.ctx.db_mut(), pool, &state, tick_spacing);

    let s0_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(revm::primitives::Bytes::from(
                    tier3_v3_common::selector("slot0()").to_vec(),
                ))
                .build()
                .expect("slot0 tx"),
        )
        .expect("slot0 transact");
    let s0_out = match &s0_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => panic!("slot0() reverted: {other:?}"),
    };
    evm.commit(s0_res.state);
    let word = |i: usize| -> U256 {
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&s0_out.as_ref()[i * 32..(i + 1) * 32]);
        U256::from_be_bytes(buf)
    };
    let on_sqrt = word(0) & U256::from_limbs([u64::MAX, u64::MAX, 0xFF, 0]);
    let on_tick_u = word(1).to::<u32>();
    let on_tick = if on_tick_u & 0x0080_0000 != 0 {
        on_tick_u as i32 - (1 << 24)
    } else {
        on_tick_u as i32
    };
    assert_eq!(
        on_sqrt, state.sqrt_price_x96,
        "seeded sqrtPriceX96 read back"
    );
    assert_eq!(on_tick, state.tick, "seeded tick read back");
    assert!(!word(6).is_zero(), "seeded slot0 unlocked (LOK-safe)");

    let liq_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(revm::primitives::Bytes::from(
                    tier3_v3_common::selector("liquidity()").to_vec(),
                ))
                .build()
                .expect("liquidity tx"),
        )
        .expect("liquidity transact");
    let liq_out = match &liq_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => panic!("liquidity() reverted: {other:?}"),
    };
    evm.commit(liq_res.state);
    let mut liq_buf = [0u8; 32];
    liq_buf.copy_from_slice(&liq_out.as_ref()[0..32]);
    let liq_word = U256::from_be_bytes(liq_buf);
    assert_eq!(
        liq_word & tier3_v3_common::MASK_128,
        U256::from(liq),
        "seeded liquidity read back"
    );

    let mut tick_call = tier3_v3_common::selector("ticks(int24)").to_vec();
    tick_call.extend_from_slice(
        &I256::try_from(-tick_spacing)
            .unwrap()
            .into_raw()
            .to_be_bytes::<32>(),
    );
    let tk_res = evm
        .transact(
            revm::context::TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(revm::primitives::Bytes::from(tick_call))
                .build()
                .expect("ticks tx"),
        )
        .expect("ticks transact");
    let tk_out = match &tk_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => panic!("ticks() reverted: {other:?}"),
    };
    evm.commit(tk_res.state);
    let mut w = [0u8; 32];
    w.copy_from_slice(&tk_out.as_ref()[0..32]);
    let on_gross = U256::from_be_bytes(w) & tier3_v3_common::MASK_128;
    w.copy_from_slice(&tk_out.as_ref()[32..64]);
    let on_net = I256::from_raw(U256::from_be_bytes(w));
    assert_eq!(
        on_gross.to::<u128>(),
        state.tick_data[&-tick_spacing].liquidity_gross.to::<u128>(),
        "seeded tick gross read back"
    );
    assert_eq!(
        on_net, state.tick_data[&-tick_spacing].liquidity_net,
        "seeded tick net read back"
    );
}

/// Build a minimal fully-tracked V3 state at tick 0, 1:1 price, liquidity
/// `liq`, with a `[-tick_spacing, +tick_spacing]` position (two boundary ticks).
fn state_at_tick_zero(liq: u128, tick_spacing: i32) -> V3PoolState {
    use alloy::primitives::U128;
    let sp_0 = U256::from(1u128) << 96;
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -tick_spacing,
        TickInfo {
            liquidity_gross: U128::from(liq),
            liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        tick_spacing,
        TickInfo {
            liquidity_gross: U128::from(liq),
            liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    V3PoolState {
        sqrt_price_x96: sp_0,
        liquidity: liq,
        tick: 0,
        update_block: 0,
        tick_data_block: 0,
        initial_state_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: std::collections::HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

// ---------------------------------------------------------------------------
// Dense-tick swap oracle (the end-to-end slice of 2LTKVO), hardened per
// CMORFZ/6DLK7I. See `tier3_v3_common::dense_state` for the fixture rationale
// (dense band + mid-word current tick + `sqrtPriceLimit` inside the band so the
// on-chain walk terminates at the same price the Rust simulator stops at).
// ---------------------------------------------------------------------------

/// Pinned dense-tick oracle: byte-exact swap across the dense band.
#[test]
fn v3_pool_dense_swap_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21
                                                 // Mid-word current tick (not a bitmap-word edge) so the first zfo bitmap
                                                 // lookup finds a same-word initialized tick (avoids the isolated-edge walk).
    let current_tick = 120i32;

    let state = tier3_v3_common::dense_state(liq, tick_spacing, k_positions, current_tick);

    let amount_specified = I256::try_from(U256::from(1_000_000_000_000_000_000_000u128)).unwrap(); // 1e21
    let limit_tick = current_tick - 4 * tick_spacing;
    let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
        .unwrap()
        .to::<u128>();

    let res = probe_accepted(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        sqrt_price_limit,
    );

    let sim = v3_simulate_swap(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        U256::from(sqrt_price_limit),
    )
    .expect("Rust sim computable");

    assert_eq!(res.amount0, sim.amount0, "amount0 byte-exact");
    assert_eq!(res.amount1, sim.amount1, "amount1 byte-exact");
    assert_eq!(
        res.post_sqrt, sim.sqrt_price_x96,
        "post sqrtPriceX96 byte-exact"
    );
    assert_eq!(res.post_tick, sim.tick, "post tick byte-exact");
    assert_eq!(res.post_liq, sim.liquidity, "post liquidity byte-exact");
}

/// H3 — pinned deterministic edge corpus (not proptest): 1-wei amounts at
/// wei-scale liquidity, tiny + large liquidity, both direction, two fee tiers.
/// Each case runs the full H1 byte-exact oracle and MUST terminate (no OOG:
/// the amount is coupled to liquidity so `computeSwapStep` actually moves
/// price each step).
#[test]
fn v3_pool_edge_corpus_is_byte_exact() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    // (liq, amount, fee, zfo). Amounts are explicit (not `liq/1e6*frac`) so a
    // 1-wei amount at wei-scale liquidity is representable (a 1-wei amount at
    // 1e21 liq would OOG — `computeSwapStep` consumes ~0 per step).
    let cases: &[(u128, u128, u32, bool)] = &[
        // 1-wei amount at wei-scale liquidity.
        (2, 1, 3000, true),
        (2, 1, 3000, false),
        // Tiny liquidity, wei-scale amounts (floor-division-sensitive region).
        (1_000, 5, 3000, true),
        (1_000, 5, 3000, false),
        (100_000, 100, 500, true),
        (100_000, 100, 500, false),
        // Large liquidity, proportionally large amount.
        (
            1_000_000_000_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
            3000,
            true,
        ), // 1e30 liq / 1e24 in
        (
            1_000_000_000_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
            500,
            false,
        ),
        // Boundary amount pushing deep into the band (1.5x the per-position liq).
        (
            1_000_000_000_000_000_000_000u128,
            1_500_000_000_000_000_000_000u128,
            3000,
            true,
        ),
        (
            1_000_000_000_000_000_000_000u128,
            1_500_000_000_000_000_000_000u128,
            3000,
            false,
        ),
    ];

    for &(liq, amount, fee, zfo) in cases {
        let state = tier3_v3_common::dense_state(liq, tick_spacing, k_positions, current_tick);
        let amount_in = I256::try_from(U256::from(amount)).unwrap();
        let dir = if zfo { -1 } else { 1 };
        let limit_tick = current_tick + dir * 3 * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        assert_byte_exact(&state, fee, tick_spacing, zfo, amount_in, sqrt_price_limit);
    }
}

/// Proptest: dense-band swap byte-exactness across a widened (state, amount,
/// direction, fee, topology) domain (H4). Amount is coupled to liquidity so
/// the walk terminates with real per-step price movement (the OOG trap).
#[allow(clippy::items_after_statements)] // `fn strategy` is local to the test
#[allow(clippy::too_many_lines)]
#[test]
fn v3_pool_dense_swap_matches_sim_proptest() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    // (liq, amount, zfo, sink_ticks, fee) — each arm couples amount to
    // liquidity so the walk terminates with real per-step price movement.
    fn strategy() -> impl Strategy<Value = (u128, U256, i32, i32, u32)> {
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
                .prop_map(move |(_, amount, zfo, sink, fee)| {
                    (liq, U256::from(amount), zfo, sink, fee)
                })
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

    proptest!(|(case in strategy())| {
        let (liq, amount, zfo, sink_ticks, fee) = case;
        // Amounts must stay inside the engine's i128 domain (or the pools' liq).
        if amount > U256::from(i128::MAX) {
            return Ok(());
        }
        let amount_in = I256::try_from(amount).unwrap();
        if amount_in.is_zero() {
            return Ok(());
        }
        let state = tier3_v3_common::dense_state(liq, tick_spacing, k_positions, current_tick);
        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * sink_ticks * tick_spacing;
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        // The H1 byte-exact oracle: an on-chain Accepted must be matched by an
        // engine Ok (then compared byte-for-byte); an on-chain Revert (a
        // verdict) must be matched by an engine NotComputable; only a verbless
        // Halt (OOG gas trap) is a legitimate skip.
        let sim = v3_simulate_swap(
            &state, fee, tick_spacing, zfo == 0, amount_in, U256::from(sqrt_price_limit),
        );
        match run_onchain_swap(
            &FORK,
            uniswap_seeder,
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount_in,
            sqrt_price_limit,
        ) {
            ProbeOutcome::Accepted(res) => {
                let sim = sim.unwrap_or_else(|e| {
                    panic!("on-chain ACCEPTED but engine rejected: {e:?}")
                });
                prop_assert_eq!(res.amount0, sim.amount0, "amount0");
                prop_assert_eq!(res.amount1, sim.amount1, "amount1");
                prop_assert_eq!(res.post_sqrt, sim.sqrt_price_x96, "post sqrtPriceX96");
                prop_assert_eq!(res.post_tick, sim.tick, "post tick");
                prop_assert_eq!(res.post_liq, sim.liquidity, "post liquidity");
            }
            ProbeOutcome::Reverted { reason } => {
                let reason_str = decode_error_string(reason.as_ref())
                    .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())));
                match sim {
                    Err(SimulateSwapError::NotComputable) => {}
                    Ok(s) => prop_assert!(
                        false,
                        "on-chain REVERTED ({reason_str}) but engine produced {s:?}"
                    ),
                    Err(SimulateSwapError::MissingTickWord(w)) => prop_assert!(
                        false,
                        "on-chain REVERTED ({reason_str}) but engine misses word {w}"
                    ),
                }
            }
            ProbeOutcome::Halted(_) => {
                // Verbless halt (OOG fixture trap) — no verdict to compare.
            }
        }
    });
}

/// H4 — topology fuzz: the byte-exact oracle driven over an **arbitrary**
/// liquidity distribution (the user-requested capability). Unlike the dense /
/// sparse canned shapes, this generator randomizes the whole `tick_data`
/// layout through [`build_arbitrary_v3_state`] so the walk must handle
/// initialized ticks crossing the current tick, empty bitmap-word regions
/// between far-apart boundaries, and one-sided ranges that run to a price
/// limit / the bitmap end. The H1 verdict protocol (Accepted↔Ok byte-exact,
/// Revert↔NotComputable, verbless Halt = the OOG gas trap = skip) is kept.
#[allow(clippy::items_after_statements)] // `fn strategy` is local to the test
#[allow(clippy::too_many_lines)]
#[test]
fn v3_pool_arbitrary_liquidity_matches_sim_proptest() {
    fn strategy() -> impl Strategy<Value = (i32, i32, Vec<ArbV3Position>, u32, i32, u32, i32)> {
        // (spacing, current_tick, positions, fee, zfo, frac, limit_words)
        prop_oneof![Just(10i32), Just(60i32)].prop_flat_map(|spacing| {
            let words = 256 * spacing; // one tick-bitmap word in ticks
            (-60_000i32..60_000i32).prop_flat_map(move |cur| {
                // Random position liquidity magnitude: wei-scale to ~1e23.
                let liq = prop_oneof![
                    1_000u128..1_000_000u128,
                    (1u32..24u32).prop_map(|e| 10u128.pow(e)),
                ];

                // --- Arm 1: a contiguous band anchored covering `cur` ---
                // Every position straddles `cur`, so the union is ONE contiguous
                // occupied band; the walk crosses its many boundaries and exits
                // into zero-liquidity space toward the price limit.
                let band_positions = {
                    let pos = ((-4 * words..-1i32), (1i32..4 * words), liq.clone()).prop_map(
                        move |(lo, hi, l)| ArbV3Position {
                            lower: cur + lo,
                            upper: cur + hi,
                            liquidity: l,
                        },
                    );
                    // Anchor ALWAYS covers `cur` so arm-1 seed liquidity > 0.
                    let anchor = ArbV3Position {
                        lower: cur - 2 * spacing,
                        upper: cur + 2 * spacing,
                        liquidity: 1_000_000_000u128,
                    };
                    prop::collection::vec(pos, 0usize..=3usize).prop_flat_map(move |mut v| {
                        v.insert(0, anchor.clone());
                        Just(v)
                    })
                };

                // --- Arm 2: price starts in an EMPTY region ---
                // Positions sit entirely above OR entirely below `cur` (a sign
                // picks the side and `min`/`max` keep lower < upper), so NO
                // position covers the current tick and the seed liquidity is 0.
                // This is the real mainnet pattern (liquidity withdrawn, no swap
                // yet): a swap must walk the empty region back into the remaining
                // liquidity — and, when it goes the other way, run empty to the
                // price limit.
                let empty_positions = {
                    let band = (
                        prop_oneof![Just(1i32), Just(-1i32)], // side of `cur`
                        (1i32..(4 * words)),                  // gap to the near edge
                        (2 * spacing..6 * spacing),           // band width
                        liq.clone(),
                    )
                        .prop_map(move |(side, gap, width, l)| {
                            let near = cur + side * gap;
                            let far = near + side * width;
                            ArbV3Position {
                                lower: near.min(far),
                                upper: near.max(far),
                                liquidity: l,
                            }
                        });
                    prop::collection::vec(band, 1usize..=2usize)
                };

                let rest = (
                    prop_oneof![Just(500u32), Just(3000u32)],
                    0i32..2,
                    1u32..=300,
                    1i32..=12,
                );
                prop_oneof![band_positions, empty_positions].prop_flat_map(move |positions| {
                    rest.clone().prop_map(move |(fee, zfo, frac, limit_words)| {
                        (spacing, cur, positions.clone(), fee, zfo, frac, limit_words)
                    })
                })
            })
        })
    }

    proptest!(|(case in strategy())| {
        let (spacing, cur, positions, fee, zfo, frac, limit_words) = case;
        let state = build_arbitrary_v3_state(cur, spacing, &positions);
        let active = state.liquidity;
        // Couple amount to the active liquidity so `computeSwapStep` actually
        // moves price (the OOG-trap guard) while still spanning tiny → deep
        // pushes (deep pushes reach far boundaries / the bitmap end). When the
        // price STARTS in an empty region (`active == 0`), size the amount from
        // the largest position liquidity instead, so the empty-region walk is
        // still exercised.
        let nominal = positions.iter().map(|p| p.liquidity).max().unwrap_or(0);
        let amount = if active > 0 {
            U256::from(active) / U256::from(100u64) * U256::from(frac)
        } else {
            U256::from(nominal) / U256::from(100u64) * U256::from(frac)
        };
        if amount.is_zero() || amount > U256::from(i128::MAX) {
            return Ok(());
        }
        let amount_in = I256::try_from(amount).unwrap();
        let zero_for_one = zfo == 0;
        let dir = if zero_for_one { -1i32 } else { 1i32 };
        // Push the price limit `limit_words` words in the swap direction,
        // clamped to the bitmap ends — exercises running out of liquidity to
        // the far limit as well as stopping mid-band.
        let limit_tick = (cur + dir * limit_words * spacing * 256).clamp(-887_272, 887_272);
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        let sim = v3_simulate_swap(
            &state,
            fee,
            spacing,
            zero_for_one,
            amount_in,
            U256::from(sqrt_price_limit),
        );
        match run_onchain_swap(
            &FORK,
            uniswap_seeder,
            &state,
            fee,
            spacing,
            zero_for_one,
            amount_in,
            sqrt_price_limit,
        ) {
            ProbeOutcome::Accepted(res) => {
                let sim = sim
                    .unwrap_or_else(|e| panic!("on-chain ACCEPTED but engine rejected: {e:?}"));
                prop_assert_eq!(res.amount0, sim.amount0, "amount0");
                prop_assert_eq!(res.amount1, sim.amount1, "amount1");
                prop_assert_eq!(res.post_sqrt, sim.sqrt_price_x96, "post sqrtPriceX96");
                prop_assert_eq!(res.post_tick, sim.tick, "post tick");
                prop_assert_eq!(res.post_liq, sim.liquidity, "post liquidity");
            }
            ProbeOutcome::Reverted { reason } => {
                let reason_str = decode_error_string(reason.as_ref())
                    .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())));
                match sim {
                    Err(SimulateSwapError::NotComputable) => {}
                    Ok(s) => prop_assert!(
                        false,
                        "on-chain REVERTED ({reason_str}) but engine produced {s:?}"
                    ),
                    Err(SimulateSwapError::MissingTickWord(w)) => prop_assert!(
                        false,
                        "on-chain REVERTED ({reason_str}) but engine misses word {w}"
                    ),
                }
            }
            ProbeOutcome::Halted(_) => {
                // Verbless halt (OOG fixture trap) — no verdict to compare.
            }
        }
    });
}

/// H3 — pinned deterministic edge corpus over the arbitrary-topology builder:
/// explicit topologies that cross initialized ticks, span empty words, and run
/// to a bitmap end, each driven through the full byte-exact oracle. These are
/// the concrete shapes the proptest randomizes, pinned so a regression can't
/// hide behind a changed seed.
#[test]
#[allow(clippy::too_many_lines)]
fn v3_pool_arbitrary_topology_edge_corpus() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let cur = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    // Each topology is a `Vec<ArbV3Position>` + a swap direction.
    let cases: Vec<(Vec<ArbV3Position>, bool)> = vec![
        // (1) Crossing: overlapping dense bands around the current tick — the
        //     upward walk crosses several distinct initialized boundaries.
        (
            vec![
                ArbV3Position {
                    lower: cur - 4 * tick_spacing,
                    upper: cur + 4 * tick_spacing,
                    liquidity: liq,
                },
                ArbV3Position {
                    lower: cur - 2 * tick_spacing,
                    upper: cur + 6 * tick_spacing,
                    liquidity: liq,
                },
                ArbV3Position {
                    lower: cur,
                    upper: cur + 8 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false, // zfo=false: upward, crosses 4+ boundaries
        ),
        // (2) Empty-word crossing downward: a dense band far below (2 words),
        //     so the downward walk crosses empty bitmap words to reach it.
        (
            vec![
                ArbV3Position {
                    lower: cur - 2 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
                ArbV3Position {
                    lower: cur - 2 * 256 * tick_spacing,
                    upper: cur - 2 * 256 * tick_spacing + 4 * tick_spacing,
                    liquidity: liq,
                },
            ],
            true, // zfo=true: downward, ~2 empty words then the far band
        ),
        // (3) Run to the bitmap end: the current tick sits inside a range whose
        //     UPWARD exit leaves all liquidity behind, so the upward swap
        //     crosses out of it and runs (empty) to the far price limit.
        (
            vec![
                ArbV3Position {
                    lower: cur - 10 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
                ArbV3Position {
                    lower: cur - 2 * 256 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false, // zfo=false: upward, out of the range then empty to the limit
        ),
    ];

    for (k, (positions, zfo)) in cases.into_iter().enumerate() {
        let state = build_arbitrary_v3_state(cur, tick_spacing, &positions);
        let active = state.liquidity;
        // A push deep enough to cross / reach the far region or the limit.
        let amount = U256::from(active) * U256::from(8u64);
        if amount > U256::from(i128::MAX) {
            continue;
        }
        let amount_in = I256::try_from(amount).unwrap();
        let dir = if zfo { -1i32 } else { 1i32 };
        let limit_tick = (cur + dir * 6 * tick_spacing * 256).clamp(-887_272, 887_272);
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        let sim = v3_simulate_swap(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_in,
            U256::from(sqrt_price_limit),
        );
        let res = match run_onchain_swap(
            &FORK,
            uniswap_seeder,
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_in,
            sqrt_price_limit,
        ) {
            ProbeOutcome::Accepted(r) => r,
            ProbeOutcome::Reverted { reason } => panic!(
                "case {k} reverted: {}",
                decode_error_string(reason.as_ref())
                    .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())))
            ),
            ProbeOutcome::Halted(m) => panic!("case {k} halted: {m}"),
        };
        let sim = sim.expect("engine must accept a pinned on-chain-accepted topology");
        assert_eq!(res.amount0, sim.amount0, "case {k} amount0 byte-exact");
        assert_eq!(res.amount1, sim.amount1, "case {k} amount1 byte-exact");
        assert_eq!(
            res.post_sqrt, sim.sqrt_price_x96,
            "case {k} post sqrtPriceX96"
        );
        assert_eq!(res.post_tick, sim.tick, "case {k} post tick byte-exact");
        assert_eq!(
            res.post_liq, sim.liquidity,
            "case {k} post liquidity byte-exact"
        );
    }
}

/// A real mainnet pattern (the user-raised case): the current price sits in an
/// EMPTY region — liquidity withdrawn, no swap yet — with the remaining
/// liquidity on one side (or both), and a swap must walk the empty region back
/// into that liquidity. Starts with seed `liquidity == 0` and asserts the Rust
/// walk matches the on-chain pool byte-exact. Covers liquidity only above,
/// only below, and on both sides, at several amounts.
#[test]
fn v3_pool_start_in_empty_region_crosses_to_liquidity() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let cur = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    let cases: Vec<(Vec<ArbV3Position>, bool)> = vec![
        // Liquidity only ABOVE; price sits in the empty region below, crossing up.
        (
            vec![ArbV3Position {
                lower: cur + 100 * tick_spacing,
                upper: cur + 500 * tick_spacing,
                liquidity: liq,
            }],
            false,
        ),
        // Liquidity only BELOW; price sits in the empty region above, crossing down.
        (
            vec![ArbV3Position {
                lower: cur - 500 * tick_spacing,
                upper: cur - 100 * tick_spacing,
                liquidity: liq,
            }],
            true,
        ),
        // Two separated bands, price in the empty middle, crossing up into the
        // top band (also spanning the interior gap between the bands).
        (
            vec![
                ArbV3Position {
                    lower: cur + 100 * tick_spacing,
                    upper: cur + 500 * tick_spacing,
                    liquidity: liq,
                },
                ArbV3Position {
                    lower: cur - 500 * tick_spacing,
                    upper: cur - 100 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false,
        ),
    ];

    for (positions, zfo) in cases {
        let state = build_arbitrary_v3_state(cur, tick_spacing, &positions);
        // The price must genuinely start in an empty region (zero active liq).
        assert_eq!(state.liquidity, 0, "price must start in an empty region");
        for frac in [1u64, 10u64, 100u64] {
            let amount_in = I256::try_from(U256::from(liq) / U256::from(frac)).unwrap();
            let dir = if zfo { -1i32 } else { 1i32 };
            let limit_tick = (cur + dir * 6 * tick_spacing * 256).clamp(-887_272, 887_272);
            let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
                .unwrap()
                .to::<u128>();
            // Strict: the swap MUST be Accepted on-chain (byte-exact); a Revert
            // or the OOG gas-trap Halt here is a fixture failure, not a skip.
            let res = match run_onchain_swap(
                &FORK,
                uniswap_seeder,
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_in,
                sqrt_price_limit,
            ) {
                ProbeOutcome::Accepted(r) => r,
                ProbeOutcome::Reverted { reason } => panic!(
                    "empty-region-start swap reverted: {}",
                    decode_error_string(reason.as_ref())
                        .unwrap_or_else(|| format!("0x{}", hex::encode(reason.as_ref())))
                ),
                ProbeOutcome::Halted(m) => panic!("empty-region-start swap halted: {m}"),
            };
            let sim = v3_simulate_swap(
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_in,
                U256::from(sqrt_price_limit),
            )
            .expect("engine must accept an on-chain-accepted empty-region start");
            assert_eq!(res.amount0, sim.amount0, "amount0 byte-exact");
            assert_eq!(res.amount1, sim.amount1, "amount1 byte-exact");
            assert_eq!(res.post_sqrt, sim.sqrt_price_x96, "post sqrtPriceX96");
            assert_eq!(res.post_tick, sim.tick, "post tick byte-exact");
            assert_eq!(res.post_liq, sim.liquidity, "post liquidity byte-exact");
        }
    }
}

/// Solver-side output for an exact-in amount, mirroring the `degenbot-solvers`
/// `solver_crossing_output` (the definitive solve-block assembly the engine
/// runs): find the largest crossed range `k` with
/// `compute_crossing(k).crossing_gross_input <= amount_in`, take its cumulative
/// output, then run the ending partial step via the canonical
/// `compute_swap_step_v3` (exactly what `int_simulate_v3_swap` delegates to).
fn solver_crossing_output_v3(
    amount_in: U256,
    seq: &degenbot_pools::int_v3_hop::IntV3TickRangeSequence,
) -> Option<U256> {
    use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;

    if amount_in.is_zero() {
        return Some(U256::ZERO);
    }
    let mut chosen_k = 0usize;
    for k in 0..seq.ranges.len() {
        let crossing = seq.compute_crossing(k)?;
        if crossing.crossing_gross_input <= amount_in {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k)?;
    if amount_in < crossing.crossing_gross_input {
        // Amount insufficient to cross into the chosen range — land in range 0.
        let hop = &seq.ranges[0];
        let fee_pips = U256::from(hop.fee_denom - hop.gamma_numer);
        let exit = if hop.zero_for_one {
            hop.sqrt_price_lower_x96
        } else {
            hop.sqrt_price_upper_x96
        };
        let step = compute_swap_step_v3(
            hop.sqrt_price_x96,
            exit,
            i128::try_from(hop.liquidity).ok()?,
            I256::try_from(amount_in).ok()?,
            fee_pips,
        )
        .ok()?;
        return Some(step.amount_out);
    }
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = crossing.ending_range;
    let fee_pips = U256::from(ending.fee_denom - ending.gamma_numer);
    // Walk the ending range's interior word boundaries + exit boundary.
    let mut sp = ending.sqrt_price_x96;
    let mut out = crossing.crossing_output;
    let mut remaining_in = I256::try_from(remaining).ok()?;
    let exit_price = if ending.zero_for_one {
        ending.sqrt_price_lower_x96
    } else {
        ending.sqrt_price_upper_x96
    };
    for target in ending
        .word_boundary_prices
        .iter()
        .chain(std::iter::once(&exit_price))
    {
        if remaining_in <= I256::ZERO {
            break;
        }
        let step = compute_swap_step_v3(
            sp,
            *target,
            i128::try_from(ending.liquidity).ok()?,
            remaining_in,
            fee_pips,
        )
        .ok()?;
        let consumed = step.amount_in.saturating_add(step.fee_amount);
        remaining_in = remaining_in.checked_sub(I256::try_from(consumed).ok()?)?;
        out = out.saturating_add(step.amount_out);
        sp = step.sqrt_price_next;
    }
    Some(out)
}

/// The dual-driver assertion the tier exists for: drive the SAME dense state +
/// amount through the on-chain pool AND through the solver's crossing assembly
/// (`build_int_v3_sequence` + `compute_crossing` + canonical final step), and
/// assert SOLVER === ON-CHAIN.
#[test]
fn v3_pool_dense_swap_matches_solver_crossing_dual() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let liq = 1_000_000_000_000_000_000_000u128;
    let current_tick = 120i32;

    let state = tier3_v3_common::dense_state(liq, tick_spacing, k_positions, current_tick);

    let seq = state
        .build_int_v3_sequence(tick_spacing, fee, true, 15)
        .expect("build int sequence");

    for amount in 1u64..=5u64 {
        let amount_u = U256::from(liq) / U256::from(1_000u64) * U256::from(amount);
        let amount_in = I256::try_from(amount_u).unwrap();
        let limit_tick = current_tick - 4 * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        let res = probe_accepted(&state, fee, tick_spacing, true, amount_in, sqrt_price_limit);

        let solver_out = solver_crossing_output_v3(amount_u, &seq);
        let solver_out = solver_out.expect("solver crossing output");
        assert_eq!(
            res.amount1, solver_out,
            "solver vs on-chain: amount={amount_u} on_am0={} on_am1={} solver_out={solver_out}",
            res.amount0, res.amount1
        );
    }
}

/// SPARSE-crossing oracle: initialized ticks far apart (spanning empty bitmap
/// words) — the topology the word-boundary flooring divergence lives in.
#[test]
fn v3_pool_sparse_crossing_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let current_tick = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    let state = tier3_v3_common::sparse_state(liq, tick_spacing, current_tick);

    for shift in [30i32, 60i32, 120i32] {
        let amount_u = U256::from(liq) / U256::from(10_000u64) * U256::from(shift as u64);
        let amount_in = I256::try_from(amount_u).unwrap();
        let limit_tick = current_tick - 300 * tick_spacing + shift;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        let res = probe_accepted(&state, fee, tick_spacing, true, amount_in, sqrt_price_limit);

        let sim = v3_simulate_swap(
            &state,
            fee,
            tick_spacing,
            true,
            amount_in,
            U256::from(sqrt_price_limit),
        )
        .expect("sparse Rust sim");

        assert_eq!(res.amount0, sim.amount0, "sparse amount0");
        assert_eq!(res.amount1, sim.amount1, "sparse amount1");
        assert_eq!(
            res.post_sqrt, sim.sqrt_price_x96,
            "sparse post sqrtPriceX96"
        );
        assert_eq!(res.post_tick, sim.tick, "sparse post tick");
        assert_eq!(res.post_liq, sim.liquidity, "sparse post liquidity");

        let seq = state
            .build_int_v3_sequence(tick_spacing, fee, true, 15)
            .expect("build sparse int sequence");
        if let Some(solver_out) = solver_crossing_output_v3(amount_u, &seq) {
            assert_eq!(res.amount1, solver_out, "sparse solver == onchain");
        }
    }
}
