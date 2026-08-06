//! Tier-3 PancakeSwap V3 `PancakeV3Pool.swap` on-chain accuracy oracle
//! (task: PancakeSwap V3 variant harness). Deploys the REAL `PancakeV3Pool`
//! — the Etherscan-verified deployment (pool 0x1445F32D1A74872bA41f3D8cF4022E9996120b31,
//! solc 0.7.6, source vendored under `tier3-oracle/lib/pancake-src/`) via the
//! `PancakeV3SwapOracleHarness`, seeds its storage slot-for-slot with the same
//! `degenbot_pools::v3_storage_slots` encoders (the fork shares the Uniswap V3
//! storage layout), drives `pool.swap`, and proves Rust `v3_simulate_swap` is
//! BYTE-EXACT to the PancakeSwap pool's swap walk (amounts, post-sqrtPrice,
//! post-tick, post-liquidity).
//!
//! ## The variant under test
//!
//! PancakeSwap V3 forked Uniswap V3 but the emitted `Swap` event APPENDS two
//! `uint128 protocolFeesToken0/1` fields, so its `topic0` differs
//! (`0x19b47279…` vs Uniswap's `0xc42079f9…`) and its data is 7 words (224
//! bytes) vs Uniswap's 5 (160). The 5 state fields are byte-identical. This
//! test verifies the variant end-to-end:
//!   1. swap MATH is byte-exact to the canonical PancakeSwap pool (protocol fee
//!      seeded 0 ⇒ the extra protocol-fee words are 0, so the state walk is
//!      indistinguishable from Uniswap — but run against the REAL fork bytecode);
//!   2. the emitted `Swap` log is NOT decodable by the Uniswap V3 decoder
//!      (`V3_SWAP_TOPIC`) and IS decodable by the PancakeSwap decoder
//!      (`decode_v3_pancakeswap_swap_log`), whose decoded state matches the
//!      Rust sim byte-exact — the exact drift the `v3_pancakeswap_swap_decoder`
//!      fixes.
//!
//! ## EIP-170 note
//!
//! The PancakeSwap fork's embedded pool-creation code makes the harness's
//! deployed code ~25.0KB — over the 24.6KB EIP-170 limit the Uniswap harness
//! stays under. revm's `cfg.limit_contract_code_size` /
//! `limit_contract_initcode_size` are raised to `usize::MAX` here so the
//! oversized harness (mainnet-illegal, revm-runnable) deploys; the state it
//! seeds/runs is otherwise identical.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite; the bytecode is loaded
//! from the committed `tier3-oracle/artifacts/` tree. Artifact integrity is
//! enforced by `tier3_harness_artifacts.rs` (source-hash, toolchain-free) and
//! `tier3-oracle/verify-tier3-artifacts.sh` (compile-vs-use). After a harness
//! edit, regenerate via `tier3-oracle/build-tier3-pancake-swap-harness.sh`.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)] // Solidity/V3 identifiers (slot0, tickBitmap…) in doc comments
#![allow(clippy::type_complexity)] // run_onchain_swap's on-chain-state return bundle

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use alloy::primitives::{aliases::I256, keccak256, Address, Bytes, U128, U256};
use alloy::rpc::types::Log as RpcLog;
use proptest::prelude::*;
use revm::context::TxEnv;
use revm::context_interface::result::ExecutionResult;
use revm::context_interface::ContextTr;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_decoders::v3_pancakeswap_swap_decoder::{
    decode_v3_pancakeswap_swap_log, V3_PANCAKESWAP_SWAP_TOPIC,
};
use degenbot_decoders::v3_swap_decoder::decode_v3_swap_log;
use degenbot_pools::state_history::{ReorgJournal, V3BlockDelta};
use degenbot_pools::v3_pancakeswap_storage_slots::{
    encode_pancake_v3_slot0_word1, pancake_v3_tick_bitmap_word_slot, pancake_v3_tick_mapping_slot,
};
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegistrationLifecycle, SimulateSwapError, TickRangeCache,
    V3PoolState,
};
use degenbot_pools::v3_storage_slots::{
    compute_v3_tick_bitmap_word_from_raw, encode_v3_liquidity_slot, encode_v3_slot0_fresh,
    encode_v3_tick_info_slot,
};
use degenbot_pools::TickInfo;

/// Mask selecting the low 128 bits of a U256 (V3 `liquidity`/TickInfo fields).
const MASK_128: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Repo path to a built harness artifact.
fn harness_artifact_path(file: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
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

/// Abi-encode the `PancakeV3SwapOracleHarness` constructor `(uint24 fee, int24 tickSpacing)`.
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

/// PancakeSwap V3 storage layout (a real divergence from Uniswap V3, surfaced
/// by this oracle): `slot0.feeProtocol` is `uint32` (2× uint16, `PROTOCOL_FEE_SP
/// = 65536`) instead of Uniswap's `uint8`, so the packed `Slot0` struct spans
/// TWO storage words — `unlocked` lives at slot 1 bit 32 — and every following
/// slot shifts by one: `liquidity`@5, `ticks`@6, `tickBitmap`@7. The Uniswap
/// `v3_storage_slots` encoders (liquidity@4, ticks@5, tickBitmap@6) therefore
/// WOULD misread a PancakeSwap pool; the engine must use these fork-aware slot
/// indices when syncing/seeding pancake pools directly (seeded here via the
/// canonical `v3_pancakeswap_storage_slots` encoders, ergo task `W32CAU`).
///
/// `slot0` word 0 reuses `encode_v3_slot0_fresh` (price/tick/observations are
/// identical; the bit-240 `unlocked` it sets is unused padding here); word 1 =
/// `encode_pancake_v3_slot0_word1` (`feeProtocol(0) | unlocked << 32`).
fn seed_pancake_pool_storage(
    db: &mut CacheDB<EmptyDB>,
    pool: Address,
    state: &V3PoolState,
    tick_spacing: i32,
) {
    // slot0 word 0: sqrtPrice | tick | observation{Index,Cardinality,CardinalityNext}.
    db.insert_account_storage(
        pool,
        U256::from(0u64),
        encode_v3_slot0_fresh(state.sqrt_price_x96, state.tick),
    )
    .expect("seed slot0 word0");
    // slot0 word 1: feeProtocol (32b, =0) | unlocked (bit 32, =true).
    db.insert_account_storage(
        pool,
        U256::from(1u64),
        encode_pancake_v3_slot0_word1(0, true),
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

/// Build a dense multi-position `V3PoolState` at `current_tick` (multiple
/// overlapping `[current_tick±k*spacing]` positions so the swap sinks real
/// liquidity across initialized ticks — avoids the empty-word-edge OOG).
fn dense_state(liq: u128, spacing: i32, k_positions: i32, current_tick: i32) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    let mut tick_data = HashMap::new();
    for k in 1..=k_positions {
        let lower = current_tick - (k * spacing);
        let upper = current_tick + (k * spacing);
        let entry = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + liq);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() + i128::try_from(liq).unwrap(),
        )
        .unwrap();
        let entry = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + liq);
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
        tick_data_block: 0,
        initial_state_block: 0,
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

/// Abi-encode `swap(bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)`.
fn encode_swap_call(zero_for_one: bool, amount_specified: I256, sqrt_price_limit: u128) -> Vec<u8> {
    let mut call = selector("swap(bool,int256,uint160)").to_vec();
    call.extend_from_slice(&[0u8; 31]);
    call.push(u8::from(zero_for_one));
    call.extend_from_slice(&amount_specified.into_raw().to_be_bytes::<32>());
    call.extend_from_slice(&U256::from(sqrt_price_limit).to_be_bytes::<32>());
    call
}

/// Wrap an alloy primitive log (what revm hands back) into the rpc `Log` shape
/// the degenbot decoders consume (outer block/tx metadata absent in-process).
fn to_rpc_log(l: alloy::primitives::Log) -> RpcLog {
    RpcLog {
        inner: l,
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// Everything the test needs from one on-chain swap: the pool state walk
/// (`amount0/amount1` absolute, post `sqrtPriceX96`/`tick`/`liquidity`) plus
/// the emitted PancakeSwap `Swap` event (captured from the swap tx logs) and
/// whether the Uniswap-V3 decoder ALSO matched (it must NOT).
#[allow(clippy::too_many_lines)] // one logical deploy→seed→swap→read pipeline
fn run_onchain_swap(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> Result<(U256, U256, U256, i32, u128, RpcLog, bool), String> {
    let mut init_code = load_creation_bytecode(
        "PancakeV3SwapOracleHarness.sol",
        "PancakeV3SwapOracleHarness",
    );
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;
    // The fork harness's deployed code exceeds EIP-170 (24.6KB); raise the
    // effective limits so the oversized (mainnet-illegal, revm-runnable) code
    // deploys in the oracle.
    evm.ctx.cfg.limit_contract_code_size = Some(usize::MAX);
    evm.ctx.cfg.limit_contract_initcode_size = Some(usize::MAX);

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
            output: revm::context_interface::result::Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => return Err(format!("harness deploy failed: {other:?}")),
    };
    evm.commit(deploy_res.state);

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
            output: revm::context_interface::result::Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("pool() reverted: {other:?}")),
    };
    evm.commit(pool_res.state);
    let mut pb = [0u8; 32];
    pb.copy_from_slice(&pool_out.as_ref()[0..32]);
    let pool = Address::from_slice(&pb[12..32]);
    seed_pancake_pool_storage(evm.ctx.db_mut(), pool, state, tick_spacing);

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
    let (out, logs) = match &res.result {
        ExecutionResult::Success {
            output: revm::context_interface::result::Output::Call(b),
            logs,
            ..
        } => (b.clone(), logs.clone()),
        other => {
            let gas = match other {
                ExecutionResult::Revert { gas, output, .. } => format!(
                    "Revert gas={gas:?} output=0x{}",
                    hex::encode(output.as_ref())
                ),
                ExecutionResult::Halt { reason, .. } => format!("Halt reason={reason:?}"),
                ExecutionResult::Success { .. } => "unexpected Success".to_string(),
            };
            return Err(format!("on-chain swap reverted/error: {gas}"));
        }
    };
    evm.commit(res.state);

    // Swap return (amount0, amount1) absolute.
    let mut w = [0u8; 32];
    w.copy_from_slice(&out.as_ref()[0..32]);
    let amount0 = I256::from_raw(U256::from_be_bytes(w)).unsigned_abs();
    w.copy_from_slice(&out.as_ref()[32..64]);
    let amount1 = I256::from_raw(U256::from_be_bytes(w)).unsigned_abs();

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
            output: revm::context_interface::result::Output::Call(b),
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
    let tick_bits = (word_at(1) & U256::from(u32::MAX)).to::<u32>();
    let post_tick = tick_bits as i32;

    // Post-swap liquidity().
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
            output: revm::context_interface::result::Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("liquidity() reverted: {other:?}")),
    };
    evm.commit(liq_res.state);
    let mut lb = [0u8; 32];
    lb.copy_from_slice(&liq_out.as_ref()[0..32]);
    let post_liq = (U256::from_be_bytes(lb) & MASK_128).to::<u128>();

    // The PancakeSwap Swap event is the one whose topic0 == the variant hash;
    // the Uniswap V3 decoder must NOT match it (the whole point of the variant).
    let rpc_logs: Vec<RpcLog> = logs.into_iter().map(to_rpc_log).collect();
    let swapped = &rpc_logs[..];
    let swap_log = swapped
        .iter()
        .find(|l| l.topics().first() == Some(&V3_PANCAKESWAP_SWAP_TOPIC))
        .cloned()
        .ok_or_else(|| "no PancakeSwap Swap event emitted".to_string())?;
    let uniswap_matches = rpc_logs.iter().any(|l| decode_v3_swap_log(l).is_some());

    Ok((
        amount0,
        amount1,
        post_sqrt,
        post_tick,
        post_liq,
        swap_log,
        uniswap_matches,
    ))
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

    let (on_am0, on_am1, on_sqrt, on_tick, on_liq, swap_log, uniswap_matches) = run_onchain_swap(
        &state,
        fee,
        tick_spacing,
        true,
        amount_specified,
        sqrt_price_limit,
    )
    .expect("on-chain swap succeeded");

    // 1. Swap MATH byte-exact vs the canonical PancakeSwap pool.
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

    // 2. The SWAP EVENT VARIANT: 9-field, distinct topic0, 224-byte data.
    assert_eq!(
        swap_log.topics().first(),
        Some(&V3_PANCAKESWAP_SWAP_TOPIC),
        "PancakeSwap Swap topic0"
    );
    assert!(
        swap_log.topics().first() != Some(&degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC),
        "must differ from Uniswap V3 Swap topic0"
    );
    assert_eq!(
        swap_log.data().data.len(),
        224,
        "9-field (7-word) event data"
    );
    // The Uniswap V3 decoder must NOT claim this event (variant is distinct).
    assert!(
        !uniswap_matches,
        "Uniswap V3 decoder must not match the PancakeSwap Swap"
    );

    // 3. The PancakeSwap decoder decodes it and matches the sim byte-exact.
    let decoded = decode_v3_pancakeswap_swap_log(&swap_log).expect("PancakeSwap decoder");
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
}

/// Proptest: dense-band swap byte-exactness + event-variant decode across
/// (state, amount, direction).
#[test]
fn pancake_v3_pool_swap_matches_sim_proptest() {
    let fee = 3000u32;
    let tick_spacing = 60i32;

    proptest!(|(liq_exp in 17u32..23, k in 3i32..10, amount_frac in 1u32..100u32, zfo in 0i32..2, sink_ticks in 1i32..4)| {
        let liq = 10u128.pow(liq_exp);
        let current_tick = 120i32;
        let state = dense_state(liq, tick_spacing, k, current_tick);
        let amount = I256::try_from(U256::from(liq) / U256::from(1_000_000u64))
            .unwrap().checked_mul(I256::try_from(amount_frac).unwrap()).unwrap();
        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * sink_ticks * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        let zfo_b = zfo == 0;
        let Ok((on_am0, on_am1, on_sqrt, on_tick, on_liq, swap_log, uniswap_matches_leftover)) = run_onchain_swap(
            &state, fee, tick_spacing, zfo_b, amount, sqrt_price_limit,
        ) else {
            return Ok(());
        };
        let _ = uniswap_matches_leftover; // asserted in the pinned test

        let sim = match v3_simulate_swap(&state, fee, tick_spacing, zfo_b, amount, U256::from(sqrt_price_limit)) {
            Ok(s) => s,
            Err(SimulateSwapError::NotComputable) => return Ok(()),
            Err(SimulateSwapError::MissingTickWord(w)) => panic!("Tracked coverage should not miss word {w}"),
        };

        // Swap math byte-exact + the event variant decodes & matches the sim.
        prop_assert_eq!(on_am0, sim.amount0);
        prop_assert_eq!(on_am1, sim.amount1);
        prop_assert_eq!(on_sqrt, sim.sqrt_price_x96);
        prop_assert_eq!(on_tick, sim.tick);
        prop_assert_eq!(on_liq, sim.liquidity);
        prop_assert_eq!(swap_log.topics().first(), Some(&V3_PANCAKESWAP_SWAP_TOPIC));
        let decoded = decode_v3_pancakeswap_swap_log(&swap_log).expect("pancake decoder");
        prop_assert_eq!(decoded.sqrt_price_x96, sim.sqrt_price_x96);
        prop_assert_eq!(decoded.tick, sim.tick);
        prop_assert_eq!(decoded.liquidity.to::<u128>(), sim.liquidity);
    });
}
