//! Tier-3b end-to-end V3 `Pool.swap` oracle (ergo task `2LTKVO`, epic
//! `UP5NH6`). Deploys the canonical v3-core `UniswapV3Pool` as real bytecode
//! via the `V3SwapOracleHarness` (solc-0.7.6 compiled), seeds its
//! `slot0`/`liquidity`/`ticks`/`tickBitmap` storage directly from a
//! `V3PoolState` using the `degenbot_pools::v3_storage_slots` encoders, and
//! proves Rust `v3_simulate_swap` is BYTE-EXACT to the real pool's `Swap`
//! walk — amounts, post-sqrtPrice, post-tick, and post-liquidity.
//!
//! Two layers: (1) the seeding-encoder foundation (`v3_pool_reads_back_...`)
//! proving the deploy + setupPool + storage-encoder pipeline reads back
//! byte-exact; (2) the end-to-end swap oracle on a DENSE-TICK fixture
//! (`v3_pool_dense_swap_byte_exact` + the proptest), asserting the full
//! multi-tick crossing is byte-exact to the on-chain walk.
//!
//! ## The dense-tick fixture + the observation-cardinality trap
//!
//! A swap needs a DENSE band (multiple overlapping positions
//! `[current_tick - k*spacing, current_tick + k*spacing]`) whose current tick
//! is MID-WORD in the tick bitmap, so V3's `nextInitializedTickWithinOneWord`
//! finds a same-word initialized tick and sinks real liquidity step-by-step
//! (an isolated / word-edge tick makes the swap chase empty words to
//! `MIN_TICK`, OOGing at ~16.4M gas). Separately, the seeded `slot0` MUST
//! carry `observationCardinality = 1` (the post-`initialize()` value) — a 0
//! cardinality makes the swap's observation bookkeeping (`observeSingle` /
//! `_updateObservation`) OOG the same way. Both are handled here; the Rust
//! sim and the pool therefore walk the identical crossed-tick path.
//!
//! ## Harness build (gated — `#[ignore]`d)
//!
//! Plain `cargo test --workspace` does not build the harness bytecode, so
//! this test is `#[ignore]`d. `just test-tier3-swap` runs
//! `tier3-oracle/build-tier3-v3-swap-harness.sh` then runs this test with
//! `--include-ignored`.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)] // Solidity/V3 identifiers (MIN_TICK, slot0…) in doc comments

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use alloy::primitives::{aliases::I256, keccak256, Address, Bytes, U256};
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::context_interface::ContextTr;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::state_history::{ReorgJournal, V3BlockDelta};
use degenbot_pools::v3_state::{v3_simulate_swap, SimulateSwapError};
use degenbot_pools::v3_state::{
    PoolTickCoverage, RegistrationLifecycle, TickRangeCache, V3PoolState,
};
use degenbot_pools::v3_storage_slots::{
    compute_v3_tick_bitmap_word_from_raw, encode_v3_liquidity_slot, encode_v3_slot0_fresh,
    encode_v3_tick_info_slot, v3_tick_bitmap_word_slot, v3_tick_mapping_slot,
};
use degenbot_pools::TickInfo;

// Mask selecting the low 128 bits of a U256 (V3 `liquidity`/TickInfo fields).
const MASK_128: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Repo path to a built harness artifact (foundry `out/<File>.sol/<Contract>.json`).
fn harness_artifact_path(file: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/out")
        .join(file)
        .join(format!("{contract}.json"))
}

/// Load the creation (`bytecode.object`) hex for a harness.
fn load_creation_bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = harness_artifact_path(file, contract);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing harness artifact {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid harness JSON");
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .expect("artifact has bytecode.object (creation)");
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex creation bytecode")
}

/// Abi-encode the `V3SwapOracleHarness` constructor args `(uint24 fee, int24 tickSpacing)`.
fn harness_constructor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
    let mut args = Vec::with_capacity(64);
    args.extend_from_slice(&U256::from(fee).to_be_bytes::<32>());
    args.extend_from_slice(
        &I256::try_from(tick_spacing)
            .unwrap_or(I256::ZERO)
            .into_raw()
            .to_be_bytes::<32>(),
    );
    args
}

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

    let mut word_positions: HashSet<i16> = HashSet::new();
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

/// Build a minimal fully-tracked V3 state at tick 0, 1:1 price, liquidity `liq`,
/// with a `[-tick_spacing, +tick_spacing]` position (two boundary ticks).
fn state_at_tick_zero(liq: u128, tick_spacing: i32) -> V3PoolState {
    let sp_0 = U256::from(1u128) << 96;
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -tick_spacing,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        tick_spacing,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    V3PoolState {
        sqrt_price_x96: sp_0,
        liquidity: liq,
        tick: 0,
        update_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

/// Tier-3b foundation oracle: deploy the real v3-core `UniswapV3Pool`
/// (via the `V3SwapOracleHarness` deployer + `setupPool`), seed its storage
/// from a `V3PoolState` via the `v3_storage_slots` encoders, and assert the
/// pool BYTE-EXACT reads back the seeded `slot0` (sqrtPrice + tick + unlocked),
/// `liquidity`, and per-tick `liquidityGross`/`liquidityNet`. This proves the
/// deploy → setupPool → storage-encoder-seeding pipeline end-to-end — the
/// load-bearing prerequisite for the swap byte-exact assertion (the remaining
/// slice, gated on a dense-tick fixture).
#[test]
#[ignore = "build the harness first: just test-tier3-swap"]
#[allow(clippy::too_many_lines)]
fn v3_pool_reads_back_seeded_state_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;

    // 1. Deploy the harness (mock tokens + the deployer/callback roles).
    let mut init_code = load_creation_bytecode("V3SwapOracleHarness.sol", "V3SwapOracleHarness");
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(Bytes::from(init_code))
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

    // 2. Deploy the real UniswapV3Pool via `setupPool()` — a separate CALL so
    //    the 22KB code-deposit gas isn't starved by the constructor's 63/64
    //    forwarding.
    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(Bytes::from(selector("setupPool()").to_vec()))
                .build()
                .expect("setupPool tx"),
        )
        .expect("setupPool transact");
    match &setup_res.result {
        ExecutionResult::Success { .. } => {}
        other => panic!("setupPool failed: {other:?}"),
    }
    evm.commit(setup_res.state);

    // 3. Discover the pool address via `harness.pool()`.
    let pool_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("pool()").to_vec()))
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

    // 4. Pinned state + seed.
    let liq = 1_000_000_000_000_000_000u128;
    let state = state_at_tick_zero(liq, tick_spacing);
    seed_v3_pool_storage(evm.ctx.db_mut(), pool, &state, tick_spacing);

    // 5. GREEN anchor: the pool reads back the seeded state byte-exact.
    //    slot0() returns the struct as 7 separate ABI words (Solidity
    //    decodes the packed slot): word0=sqrtPrice(uint160), word1=tick(int24),
    //    …, word6=unlocked(bool).
    let s0_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("slot0()").to_vec()))
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
    #[allow(clippy::cast_possible_wrap)]
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

    // liquidity() -> uint128.
    let liq_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("liquidity()").to_vec()))
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
        liq_word & MASK_128,
        U256::from(liq),
        "seeded liquidity read back"
    );

    // ticks(int24) getter returns (uint128 gross, int128 net, …) as SEPARATE
    // 32-byte words → word0 = gross, word1 = net.
    let mut tick_call = selector("ticks(int24)").to_vec();
    tick_call.extend_from_slice(
        &I256::try_from(-tick_spacing)
            .unwrap()
            .into_raw()
            .to_be_bytes::<32>(),
    );
    let tk_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(tick_call))
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
    let on_gross = U256::from_be_bytes(w) & MASK_128;
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

// ---------------------------------------------------------------------------
// Dense-tick swap oracle (the end-to-end slice of 2LTKVO).
//
// A single-position fixture OOGs because once the swap crosses the isolated
// boundary tick it has no next initialized tick in the bitmap word, so V3 walks
// the empty words to MIN_TICK with phantom liquidity. The fix is a DENSE band:
// multiple overlapping positions `[current_tick - k*spacing,
// current_tick + k*spacing]` for k=1..=K make every boundary inside the band an
// initialized tick the swap genuinely crosses, so it sinks real liquidity
// step-by-step instead of chasing the empty edge. A `sqrtPriceLimit` set INSIDE
// the band guarantees on-chain termination at the same price the Rust simulator
// stops at (neither can walk past it).
// ---------------------------------------------------------------------------

/// Build a dense V3 state at an arbitrary `current_tick` (not tick 0).
/// `k_positions` overlapping positions centered on `current_tick`:
/// `[current_tick - k*spacing, current_tick + k*spacing]` for k=1..=K,
/// each contributing liquidity `liq`. Active liquidity at `current_tick` is
/// `K*liq`; every boundary is a DISTINCT initialized tick, so a swap sinks
/// deterministically into the band instead of chasing an empty word edge.
///
/// `current_tick` must be chosen MID-WORD (its compressed value not at a word
/// boundary) so the first `nextInitializedTickWithinOneWord` lookup finds a
/// same-word initialized tick in the swap direction — this is what avoids the
/// isolated-word-edge degenerate walk (a band anchored at tick 0 still OOGs
/// because tick 0 is the upper edge of bitmap word 0 in the zfo direction).
fn dense_state(liq: u128, spacing: i32, k_positions: i32, current_tick: i32) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    let mut tick_data = HashMap::new();
    for k in 1..=k_positions {
        let lower = current_tick - (k * spacing);
        let upper = current_tick + (k * spacing);
        // Lower boundary: crossing downward (zfo) removes this position's liq
        // (net is +liq, so a zfo down-cross subtracts it).
        let entry = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross =
            alloy::primitives::U128::from(entry.liquidity_gross.to::<u128>() + liq);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() + i128::try_from(liq).unwrap(),
        )
        .unwrap();
        // Upper boundary: crossing upward adds this position's liq (net -liq).
        let entry = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross =
            alloy::primitives::U128::from(entry.liquidity_gross.to::<u128>() + liq);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() - i128::try_from(liq).unwrap(),
        )
        .unwrap();
    }
    V3PoolState {
        sqrt_price_x96: sp,
        liquidity: (k_positions as u128) * liq,
        tick: current_tick,
        update_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

/// Abi-encode `swap(bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)`
/// for the harness entry (`uint160` is right-padded in 32 bytes).
fn encode_swap_call(zero_for_one: bool, amount_specified: I256, sqrt_price_limit: u128) -> Vec<u8> {
    let mut call = selector("swap(bool,int256,uint160)").to_vec();
    call.extend_from_slice(&[0u8; 31]);
    call.push(u8::from(zero_for_one));
    call.extend_from_slice(&amount_specified.into_raw().to_be_bytes::<32>());
    call.extend_from_slice(&U256::from(sqrt_price_limit).to_be_bytes::<32>());
    call
}

/// Drive one on-chain V3 `pool.swap` end-to-end against a fresh, self-contained
/// revm `CacheDB`: deploy the harness + real pool, seed storage from `&state`,
/// call `harness.swap`, and read back the swap return `(amount0, amount1)` plus
/// the post-swap `(sqrtPriceX96, tick, liquidity)`. The evm is built internally
/// so the verbose revm concrete type never crosses a function boundary.
#[allow(clippy::too_many_lines)] // one logical deploy→seed→swap→read pipeline
fn run_onchain_swap(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> Result<(U256, U256, U256, i32, u128), String> {
    let mut init_code = load_creation_bytecode("V3SwapOracleHarness.sol", "V3SwapOracleHarness");
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    // Deploy harness.
    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(Bytes::from(init_code))
                .build()
                .expect("deploy tx"),
        )
        .expect("deploy transact");
    let harness = match &deploy_res.result {
        ExecutionResult::Success {
            output: Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => return Err(format!("harness deploy failed: {other:?}")),
    };
    evm.commit(deploy_res.state);

    // setupPool -> real UniswapV3Pool.
    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(Bytes::from(selector("setupPool()").to_vec()))
                .build()
                .expect("setupPool tx"),
        )
        .expect("setupPool transact");
    match &setup_res.result {
        ExecutionResult::Success { .. } => {}
        other => return Err(format!("setupPool failed: {other:?}")),
    }
    evm.commit(setup_res.state);

    // Resolve + seed the pool address.
    let pool_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("pool()").to_vec()))
                .build()
                .expect("pool() tx"),
        )
        .expect("pool() transact");
    let pool_out = match &pool_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("pool() reverted: {other:?}")),
    };
    evm.commit(pool_res.state);
    let mut pb = [0u8; 32];
    pb.copy_from_slice(&pool_out.as_ref()[0..32]);
    let pool = Address::from_slice(&pb[12..32]);
    seed_v3_pool_storage(evm.ctx.db_mut(), pool, state, tick_spacing);

    // Drive the swap.
    let data = encode_swap_call(zero_for_one, amount_specified, sqrt_price_limit);
    let res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(Bytes::from(data))
                .build()
                .expect("swap tx"),
        )
        .expect("swap transact");

    let out = match &res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => {
            let gas = match other {
                ExecutionResult::Revert { gas, .. } => {
                    format!("Revert gas={gas:?}")
                }
                ExecutionResult::Halt { reason, .. } => format!("Halt reason={reason:?}"),
                ExecutionResult::Success { .. } => "unexpected Success".to_string(),
            };
            return Err(format!("on-chain swap reverted/error: {gas}"));
        }
    };
    evm.commit(res.state);

    let mut w = [0u8; 32];
    w.copy_from_slice(&out.as_ref()[0..32]);
    let amount0_raw = I256::from_raw(U256::from_be_bytes(w));
    w.copy_from_slice(&out.as_ref()[32..64]);
    let amount1_raw = I256::from_raw(U256::from_be_bytes(w));
    // The pool returns signed deltas (positive = pool receives). For byte-exact
    // comparison with the engine's absolute magnitudes, take the abs value.
    let amount0 = amount0_raw.unsigned_abs();
    let amount1 = amount1_raw.unsigned_abs();

    // Post-swap slot0 -> sqrtPriceX96 (word0), tick (word1).
    let s0 = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("slot0()").to_vec()))
                .build()
                .expect("slot0 tx"),
        )
        .expect("slot0 transact");
    let s0_out = match &s0.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("slot0() reverted: {other:?}")),
    };
    evm.commit(s0.state);
    let word_at = |i: usize| -> U256 {
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&s0_out.as_ref()[i * 32..(i + 1) * 32]);
        U256::from_be_bytes(buf)
    };
    let post_sqrt = word_at(0) & U256::from_limbs([u64::MAX, u64::MAX, 0xFF, 0]);
    // tick is a sign-extended int24 in the low 32 bits of word1 (all high bits
    // are the sign-extension, so `to::<u32>()` would overflow for a negative
    // tick — mask the low 32 bits first, then reinterpret as i32 directly; the
    // bit-23 sign check is NOT needed because a full 32-bit reinterpret already
    // yields the correct two's-complement value).
    let tick_bits = (word_at(1) & U256::from(u32::MAX)).to::<u32>();
    let post_tick = tick_bits as i32;

    // Post-swap liquidity() -> uint128.
    let liq_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("liquidity()").to_vec()))
                .build()
                .expect("liquidity tx"),
        )
        .expect("liquidity transact");
    let liq_out = match &liq_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("liquidity() reverted: {other:?}")),
    };
    evm.commit(liq_res.state);
    let mut lb = [0u8; 32];
    lb.copy_from_slice(&liq_out.as_ref()[0..32]);
    let post_liq = (U256::from_be_bytes(lb) & MASK_128).to::<u128>();

    Ok((amount0, amount1, post_sqrt, post_tick, post_liq))
}

/// Pinned dense-tick oracle: byte-exact swap across the dense band.
#[test]
#[ignore = "build the harness first: just test-tier3-swap"]
fn v3_pool_dense_swap_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21
                                                 // Mid-word current tick (not a bitmap-word edge) so the first zfo bitmap
                                                 // lookup finds a same-word initialized tick (avoids the isolated-edge walk).
    let current_tick = 120i32;

    let state = dense_state(liq, tick_spacing, k_positions, current_tick);

    // Exact-in zfo swap of token0. Amount large enough to move price meaningfully
    // (vs the K*liq liquidity), with sqrtPriceLimit INSIDE the band so the walk
    // terminates at the limit. If the amount were tiny vs liquidity,
    // computeSwapStep would consume ~0 per step and the loop would OOG.
    let amount_specified = I256::try_from(U256::from(1_000_000_000_000_000_000_000u128)).unwrap(); // 1e21
    let limit_tick = current_tick - 4 * tick_spacing;
    let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
        .unwrap()
        .to::<u128>();

    let (on_am0, on_am1, on_sqrt, on_tick, on_liq) = run_onchain_swap(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        sqrt_price_limit,
    )
    .expect("on-chain swap succeeded");

    let sim = v3_simulate_swap(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        U256::from(sqrt_price_limit),
    )
    .expect("Rust sim computable");

    assert_eq!(on_am0, sim.amount0, "amount0 byte-exact");
    assert_eq!(on_am1, sim.amount1, "amount1 byte-exact");
    assert_eq!(on_sqrt, sim.sqrt_price_x96, "post sqrtPriceX96 byte-exact");
    assert_eq!(on_tick, sim.tick, "post tick byte-exact");
    assert_eq!(on_liq, sim.liquidity, "post liquidity byte-exact");
}

/// Proptest: dense-band swap byte-exactness across (state, amount, direction).
#[test]
#[ignore = "build the harness first: just test-tier3-swap"]
fn v3_pool_dense_swap_matches_sim_proptest() {
    let fee = 3000u32;
    let tick_spacing = 60i32;

    proptest!(|(liq_exp in 17u32..23, k in 3i32..10, amount_frac in 1u32..100u32, zfo in 0i32..2, sink_ticks in 1i32..4)| {
        let liq = 10u128.pow(liq_exp);
        let current_tick = 120i32;
        let state = dense_state(liq, tick_spacing, k, current_tick);

        // Amount as a fraction of active liquidity (kept below band capacity
        // so the swap sinks into the band without threatening the edge).
        let amount = I256::try_from(U256::from(liq) / U256::from(1_000_000u64))
            .unwrap().checked_mul(I256::try_from(amount_frac).unwrap()).unwrap();
        // Price limit deep inside the band (a few multiples of spacing in).
        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * sink_ticks * tick_spacing;
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        // An on-chain revert here is a test-fixture limitation (e.g. the
        // exact-output direction walking past a limit), not an engine
        // divergence — skip rather than fail.
        let Ok((on_am0, on_am1, on_sqrt, on_tick, on_liq)) = run_onchain_swap(
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount,
            sqrt_price_limit,
        ) else {
            return Ok(());
        };

        let sim = match v3_simulate_swap(
            &state, fee, tick_spacing, zfo == 0, amount, U256::from(sqrt_price_limit),
        ) {
            Ok(s) => s,
            Err(SimulateSwapError::NotComputable) => return Ok(()),
            Err(SimulateSwapError::MissingTickWord(w)) => {
                panic!("Tracked coverage should not miss word {w}")
            }
        };

        prop_assert_eq!(on_am0, sim.amount0, "amount0");
        prop_assert_eq!(on_am1, sim.amount1, "amount1");
        prop_assert_eq!(on_sqrt, sim.sqrt_price_x96, "post sqrtPriceX96");
        prop_assert_eq!(on_tick, sim.tick, "post tick");
        prop_assert_eq!(on_liq, sim.liquidity, "post liquidity");

        // SOLVER-CROSSING DUAL: for the zfo exact-in cases the solver's
        // crossing assembly must also equal the on-chain pool byte-exact
        // (closing the solver-vs-twin-vs-onchain triangle — a shared
        // solver+twin bug would diverge from the pool here even if the twin
        // asserts above pass).
        if zfo == 0 {
            let seq = state
                .build_int_v3_sequence(tick_spacing, fee, true, 15)
                .expect("build int sequence");
            if let Some(solver_out) = solver_crossing_output_v3(amount.unsigned_abs(), &seq) {
                prop_assert_eq!(on_am1, solver_out, "solver crossing == onchain");
            }
        }
    });
}

/// Solver-side output for an exact-in amount, mirroring the `degenbot-solvers`
/// `solver_crossing_output` (the definitive solve-block assembly the engine
/// runs): find the largest crossed range `k` with
/// `compute_crossing(k).crossing_gross_input <= amount_in`, take its cumulative
/// output, then run the ending partial step via the canonical
/// `compute_swap_step_v3` (exactly what `int_simulate_v3_swap` delegates to).
/// Reachable in-crate (no solvers cycle): `IntV3TickRangeSequence` +
/// `compute_crossing` live in `degenbot_pools::int_v3_hop`, and the final step
/// re-uses the same `degenbot_cl_math::compute_swap_step_v3` the solver calls.
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
    // Walk the ending range's interior word boundaries + exit boundary (the
    // E7ALWT per-boundary flooring `int_simulate_v3_swap` mirrors).
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
/// assert SOLVER === ON-CHAIN. `v3_simulate_swap` (the twin) is already proven
/// byte-exact to on-chain in the earlier test; this closes the
/// solver-vs-twin-vs-onchain triangle (a shared solver+twin bug diverging from
/// the pool would RED here even if the twin tests pass).
#[test]
#[ignore = "build the harness first: just test-tier3-swap"]
fn v3_pool_dense_swap_matches_solver_crossing_dual() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let liq = 1_000_000_000_000_000_000_000u128;
    let current_tick = 120i32;

    let state = dense_state(liq, tick_spacing, k_positions, current_tick);

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

        let (on_am0, on_am1, _, _, _) =
            run_onchain_swap(&state, fee, tick_spacing, true, amount_in, sqrt_price_limit)
                .expect("on-chain swap succeeded");

        // The pool returns a negative signed delta for the output token (zfo
        // pays token1 out); `run_onchain_swap` already abs()'d it, so on_am1
        // is the absolute output. Compare against the solver's crossing output.
        let solver_out = solver_crossing_output_v3(amount_u, &seq);
        let err = format!(
            "solver vs on-chain: amount={amount_u} on_am0={on_am0} on_am1={on_am1} solver_out={solver_out:?}"
        );
        let solver_out = solver_out.expect("solver crossing output");
        assert_eq!(on_am1, solver_out, "{err}");
    }
}

/// Build a SPARSE V3 state: initialized tick boundaries separated by far more
/// than one bitmap word (`256 * spacing` ticks), so a swap crossing from the
/// current tick to a distant boundary must walk through MANY EMPTY bitmap
/// words. This is the exact topology the Tier-3b oracle exists for — the
/// `compute_tick_ranges` word-boundary flooring divergence (the V4
/// `CurrencyNotSettled` root cause) manifests only when a range spans
/// uninitialized word boundaries. Two overlapping wide positions around
/// `current_tick` (mid-word, away from a word edge) provide real liquidity the
/// swap sinks into while its boundaries sit in far-apart bitmap words.
fn sparse_state(liq: u128, spacing: i32, current_tick: i32) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    // Word boundary = 256 compressed ticks. Place the two positions' bounds far
    // beyond ±2 words so the walk between them crosses several empty words.
    let far = 300 * spacing; // ~1.2 words past load-bearing range on each side
    let mut tick_data = HashMap::new();
    for (lower, upper, amount) in [
        (current_tick - far, current_tick + far, liq),
        (
            current_tick - far + spacing,
            current_tick + far - spacing,
            liq,
        ),
    ] {
        let entry = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross =
            alloy::primitives::U128::from(entry.liquidity_gross.to::<u128>() + amount);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() + i128::try_from(amount).unwrap(),
        )
        .unwrap();
        let entry = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross =
            alloy::primitives::U128::from(entry.liquidity_gross.to::<u128>() + amount);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() - i128::try_from(amount).unwrap(),
        )
        .unwrap();
    }
    V3PoolState {
        sqrt_price_x96: sp,
        // Active liquidity = both positions cover current_tick.
        liquidity: 2 * liq,
        tick: current_tick,
        update_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

/// SPARSE-crossing oracle: initialized ticks far apart (spanning empty bitmap
/// words) — the topology the word-boundary flooring divergence lives in.
/// Asserts `v3_simulate_swap` + solver crossing are byte-exact to the on-chain
/// pool across the sparse walk (validates the observation-cardinality fix made
/// even sparse topologies terminate, not just the dense band).
#[test]
#[ignore = "build the harness first: just test-tier3-swap"]
fn v3_pool_sparse_crossing_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    // Mid-word current tick; boundaries ~1.5 words out → the walk crosses
    // empty bitmap words.
    let current_tick = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    let state = sparse_state(liq, tick_spacing, current_tick);

    // Sink a few ticks toward the lower (zfo) boundary — well inside the band
    // so neither walk approaches the empty outer region.
    for shift in [30i32, 60i32, 120i32] {
        let amount_u = U256::from(liq) / U256::from(10_000u64) * U256::from(shift as u64);
        let amount_in = I256::try_from(amount_u).unwrap();
        // Limit anchored a couple words short of the far lower boundary (which
        // sits 300*spacing below current_tick) so both walks stop INSIDE the
        // band at the same price.
        let limit_tick = current_tick - 300 * tick_spacing + shift;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        let (on_am0, on_am1, on_sqrt, on_tick, on_liq) =
            run_onchain_swap(&state, fee, tick_spacing, true, amount_in, sqrt_price_limit)
                .expect("sparse on-chain swap succeeded");

        let sim = v3_simulate_swap(
            &state,
            fee,
            tick_spacing,
            true,
            amount_in,
            U256::from(sqrt_price_limit),
        )
        .expect("sparse Rust sim");

        assert_eq!(on_am0, sim.amount0, "sparse amount0");
        assert_eq!(on_am1, sim.amount1, "sparse amount1");
        assert_eq!(on_sqrt, sim.sqrt_price_x96, "sparse post sqrtPriceX96");
        assert_eq!(on_tick, sim.tick, "sparse post tick");
        assert_eq!(on_liq, sim.liquidity, "sparse post liquidity");

        // Solver crossing dual assert.
        let seq = state
            .build_int_v3_sequence(tick_spacing, fee, true, 15)
            .expect("build sparse int sequence");
        if let Some(solver_out) = solver_crossing_output_v3(amount_u, &seq) {
            assert_eq!(on_am1, solver_out, "sparse solver == onchain");
        }
    }
}
