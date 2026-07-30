//! Tier-3b end-to-end V3 `Pool.swap` oracle (ergo task `2LTKVO`, epic
//! `UP5NH6`). Deploys the canonical v3-core `UniswapV3Pool` as real bytecode
//! via the `V3SwapOracleHarness` (solc-0.7.6 compiled), seeds its
//! `slot0`/`liquidity`/`ticks`/`tickBitmap` storage directly from a
//! `V3PoolState` using the `degenbot_pools::v3_storage_slots` encoders, and
//! proves the pool BYTE-EXACT reads back the seeded state — the foundation of
//! the on-chain accuracy oracle.
//!
//! The end-to-end swap byte-exact assertion (Rust `v3_simulate_swap` === the
//! real pool's `Swap` event) is the remaining slice of this task: it requires
//! a DENSE-tick fixture (multiple overlapping positions) so a swap can cross
//! ticks without triggering V3's isolated-tick degenerate walk (a single
//! `[-spacing,+spacing]` position OOGs once the swap reaches the isolated
//! boundary). The deploy + setupPool + seeding-encoder pipeline proven GREEN
//! here is the load-bearing prerequisite for that fixture.
//!
//! ## Harness build (gated — `#[ignore]`d)
//!
//! Plain `cargo test --workspace` does not build the harness bytecode, so
//! this test is `#[ignore]`d. `just test-tier3-swap` runs
//! `tier3-oracle/build-tier3-v3-swap-harness.sh` then runs this test with
//! `--include-ignored`.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use alloy::primitives::{aliases::I256, keccak256, Address, Bytes, U256};
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::context_interface::ContextTr;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_pools::state_history::{ReorgJournal, V3BlockDelta};
use degenbot_pools::v3_state::{PoolTickCoverage, TickRangeCache, V3PoolState};
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
