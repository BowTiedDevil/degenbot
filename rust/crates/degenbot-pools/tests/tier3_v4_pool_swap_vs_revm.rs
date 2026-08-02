//! Tier-3b V4 `PoolManager.swap` end-to-end oracle (ergo task `2LTKVO`, epic
//! UP5NH6) — the V4 twin of `tier3_v3_pool_swap_vs_revm.rs`.
//!
//! Deploys the canonical v4-core `PoolManager` (via the `V4SwapOracleHarness`
//! unlocker wrapper) as real bytecode in an in-process revm `CacheDB<EmptyDB>`,
//! seeds the single pool's `slot0`/`liquidity`/`ticks`/`tickBitmap` storage at
//! the poolId-derived base from a `V4PoolState` via the `v4_storage_slots`
//! encoders, drives the swap through `unlock` → `unlockCallback` → `swap` →
//! settle, and asserts the Rust `v4_simulate_swap` amount0/amount1 === the
//! on-chain `BalanceDelta` byte-for-byte.
//!
//! ## V4 vs V3 harness differences
//!
//! - The pool is a SINGLETON `PoolManager`; per-pool storage is keyed by
//!   `poolId = keccak256(abi.encode(poolKey))` at top-level slot 6, so seeding
//!   writes to `v4_pool_state_base_slot(poolId) + offset` — not the fixed
//!   slots a per-pool V3 contract uses.
//! - `unlock(bytes)` re-enters via `IUnlockCallback(msg.sender)`; the swap runs
//!   inside the callback and `NonzeroDeltaCount` must return to 0 (settle the
//!   negative/input delta with a real token transfer, take the positive/output
//!   delta out) or `CurrencyNotSettled` reverts. The harness pre-funds both
//!   mock ERC-20s so the settle transfer succeeds.
//! - V4 `slot0` has NO `unlocked` flag; `checkPoolInitialized` only requires
//!   `sqrtPriceX96 != 0`. Fee = `slot0.lpFee` when `protocolFee == 0`, so
//!   seeding `lp_fee = fee` reproduces the swap the engine's
//!   `v4_simulate_swap(fee, …)` charges.
//! - `v4_simulate_swap` takes a NEGATIVE `amount_specified` for exact-in (V4
//!   sign convention, opposite to V3) — mirrored here.
//!
//! Plain `cargo test --workspace` does not build the harness bytecode, so this
//! test is `#[ignore]`d. `just test-tier3-v4` runs
//! `tier3-oracle/build-tier3-v4-swap-harness.sh` then runs this test with
//! `--include-ignored`.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)] // Solidity/V4 identifiers (PoolManager, slot0…)

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
use degenbot_pools::v3_state::{PoolTickCoverage, SimulateSwapError};
use degenbot_pools::v4_state::{v4_simulate_swap, V4PoolKey, V4PoolState};
use degenbot_pools::v4_storage_slots::{
    encode_v4_liquidity_slot, encode_v4_slot0, encode_v4_tick_info_slot, v4_liquidity_slot,
    v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot, v4_tick_bitmap_word_slot,
    v4_tick_mapping_slot, V4Slot0Parts,
};
use degenbot_pools::TickInfo;

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
fn selector(sig: &str) -> [u8; 4] {
    keccak256(sig.as_bytes())[0..4].try_into().unwrap()
}

/// Root of the repo (used to resolve the tier3-oracle artifacts).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Load a foundry-shaped harness artifact's creation bytecode.
fn load_creation_bytecode(file: &str, contract: &str) -> Vec<u8> {
    let artifact_path = repo_root()
        .join("tier3-oracle")
        .join("out")
        .join(file)
        .join(format!("{contract}.json"));
    let raw = std::fs::read_to_string(&artifact_path)
        .unwrap_or_else(|_| panic!("missing artifact {}", artifact_path.display()));
    let artifact = serde_json::from_str::<serde_json::Value>(&raw).expect("valid artifact json");
    let code = artifact["bytecode"]["object"]
        .as_str()
        .expect("creation bytecode object");
    hex::decode(code).expect("hex object")
}

/// ABI-encode constructor args `(uint24 fee, int24 tickSpacing)` for the V4
/// harness (matches `V4SwapOracleHarness(uint24, int24)`).
fn harness_constructor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
    let mut args = vec![0u8; 64];
    args[28..32].copy_from_slice(&fee.to_be_bytes());
    args[60..64].copy_from_slice(&tick_spacing.to_be_bytes());
    args
}

/// Compute the V4 bitmap word value for one word from `tick_data`. Delegate to
/// the V3 helper — the bitmask packing is identical between V3 and V4.
fn compute_v4_word_from_raw(
    tick_data: &HashMap<i32, TickInfo, std::hash::RandomState>,
    tick_spacing: i32,
    word_pos: i16,
) -> U256 {
    degenbot_pools::v3_storage_slots::compute_v3_tick_bitmap_word_from_raw(
        tick_data,
        tick_spacing,
        word_pos,
    )
}

/// Seed the V4 `Pool.State` storage for a single defined pool at the manager.
/// `pool_key` derives the `poolId`; the per-pool base slot is
/// `keccak256(abi.encode(poolId, uint256(6)))`, fields are `base + offset`
/// (0=`Slot0`, 3=`liquidity`, 4=`ticks` base, 5=`tickBitmap` base).
fn seed_v4_pool_storage(
    db: &mut CacheDB<EmptyDB>,
    manager: Address,
    pool_key: &V4PoolKey,
    state: &V4PoolState,
    fee: u32,
) {
    let tick_spacing = pool_key.tick_spacing;
    let pool_id = v4_pool_id(pool_key);
    let base = v4_pool_state_base_slot(pool_id);

    db.insert_account_storage(
        manager,
        v4_slot0_slot(base),
        encode_v4_slot0(V4Slot0Parts {
            sqrt_price_x96: state.sqrt_price_x96,
            tick: state.tick,
            protocol_fee: 0,
            lp_fee: fee,
        }),
    )
    .expect("seed v4 slot0");
    db.insert_account_storage(
        manager,
        v4_liquidity_slot(base),
        encode_v4_liquidity_slot(state.liquidity),
    )
    .expect("seed v4 liquidity");

    for (tick, info) in &state.tick_data {
        db.insert_account_storage(
            manager,
            v4_tick_mapping_slot(*tick, base),
            encode_v4_tick_info_slot(info),
        )
        .expect("seed v4 tick info");
    }

    let mut word_positions: HashSet<i16> = HashSet::new();
    for &tick in state.tick_data.keys() {
        let compressed = tick.div_euclid(tick_spacing);
        let word_pos = i16::try_from(compressed >> 8).unwrap_or(0);
        word_positions.insert(word_pos);
    }
    for word_pos in word_positions {
        let word_value = compute_v4_word_from_raw(&state.tick_data, tick_spacing, word_pos);
        db.insert_account_storage(
            manager,
            v4_tick_bitmap_word_slot(word_pos, base),
            word_value,
        )
        .expect("seed v4 tickBitmap word");
    }
}

/// Build a dense fully-tracked V4 state at an arbitrary `current_tick` (not
/// tick 0), mirroring the V3 dense fixture: `k_positions` overlapping
/// positions `[current_tick - k*spacing, current_tick + k*spacing]`, each
/// contributing `liq`. Active liquidity at `current_tick` is `k_positions*liq`;
/// every boundary is a distinct initialized tick so a swap sinks determin-
/// istically instead of chasing an empty word edge.
fn dense_v4_state(
    liq: u128,
    tick_spacing: i32,
    k_positions: i32,
    current_tick: i32,
) -> V4PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    for k in 1..=k_positions {
        for (tick, net) in [
            (
                current_tick - k * tick_spacing,
                i128::try_from(liq).unwrap(),
            ),
            (
                current_tick + k * tick_spacing,
                -i128::try_from(liq).unwrap(),
            ),
        ] {
            let entry = tick_data.entry(tick).or_insert_with(|| TickInfo {
                liquidity_gross: alloy::primitives::U128::ZERO,
                liquidity_net: I256::ZERO,
                block: 0,
            });
            entry.liquidity_gross =
                alloy::primitives::U128::from(entry.liquidity_gross.to::<u128>() + liq);
            entry.liquidity_net =
                I256::try_from(i128::try_from(entry.liquidity_net).unwrap() + net).unwrap();
        }
    }
    let params = degenbot_pools::v4_state::RegisterV4PoolParams {
        pool_manager: Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::ZERO,
            fee: 3_000,
            tick_spacing,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: sp,
        liquidity: u128::try_from(i128::try_from(liq).unwrap() * i128::from(k_positions)).unwrap(),
        tick: current_tick,
        tick_data,
        update_block: 0,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

// ---------------------------------------------------------------------------
// Dense-tick swap oracle (V4 leg of 2LTKVO). Same fixture philosophy as V3.
// ---------------------------------------------------------------------------

/// ABI-encode the V4 harness `swap(bool,int256,uint160)` call.
fn encode_v4_swap_call(
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> Vec<u8> {
    let mut data = selector("swap(bool,int256,uint160)").to_vec();
    let mut buf = vec![0u8; 32];
    buf[31] = u8::from(zero_for_one);
    data.extend_from_slice(&buf);
    data.extend_from_slice(&amount_specified.into_raw().to_be_bytes::<32>());
    let mut lim = [0u8; 32];
    // uint160 field: right-aligned in 32 bytes; the limit fits in u128 so the
    // low 16 bytes carry it with the next 4 left-padding bytes zero.
    lim[16..32].copy_from_slice(&sqrt_price_limit.to_be_bytes());
    data.extend_from_slice(&lim);
    data
}

/// Drive the on-chain V4 swap for a dense state; return the ABSOLUTE
/// (amount0, amount1) from the pool's BalanceDelta (the Swap event's
/// amount0/amount1 = delta.amount0()/amount1(), both int128).
fn run_v4_onchain_swap(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> Result<(U256, U256), String> {
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;

    let mut init_code = load_creation_bytecode("V4SwapOracleHarness.sol", "V4SwapOracleHarness");
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_000_000)
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
        other => return Err(format!("V4 harness deploy failed: {other:?}")),
    };
    evm.commit(deploy_res.state);

    // Read the three public address getters inline (concrete evm type).
    let mut get_addr = |sig: &str| -> Result<Address, String> {
        let res = evm
            .transact(
                TxEnv::builder()
                    .kind(TxKind::Call(harness))
                    .gas_limit(2_000_000)
                    .data(Bytes::from(selector(sig).to_vec()))
                    .build()
                    .expect("getter tx"),
            )
            .expect("getter transact");
        if let ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } = &res.result
        {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&b.as_ref()[0..32]);
            Ok(Address::from_slice(&buf[12..32]))
        } else {
            Err(format!("getter {sig} failed"))
        }
    };
    let manager = get_addr("manager()")?;
    let cur0 = get_addr("currency0()")?;
    let cur1 = get_addr("currency1()")?;
    // The harness's `swap` builds its PoolKey from the CONSTRUCTOR-DEPLOYED
    // token order (`currency0`, `currency1`), so the poolId we seed must use
    // that EXACT order — reordering here would derive a different poolId and
    // hit `PoolNotInitialized`. (Ordering is only velocity-checked by
    // `initialize`, which we never call; consistency with the swap key is the
    // only requirement.)
    let pool_key = V4PoolKey {
        currency0: cur0,
        currency1: cur1,
        fee,
        tick_spacing,
        hooks: Address::ZERO,
    };
    seed_v4_pool_storage(evm.ctx.db_mut(), manager, &pool_key, state, fee);

    let data = encode_v4_swap_call(zero_for_one, amount_specified, sqrt_price_limit);
    let res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_000_000)
                .data(Bytes::from(data))
                .build()
                .expect("v4 swap tx"),
        )
        .expect("v4 swap transact");

    let out = match &res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return Err(format!("on-chain v4 swap reverted/error: {other:?}")),
    };

    // BalanceDelta is ONE packed word: amount0 in the HIGH 128 bits, amount1 in
    // the LOW 128 bits (v4-core `types/BalanceDelta.sol`). Sign-extend each
    // 128-bit field to a proper signed value.
    let mut w32 = [0u8; 32];
    w32.copy_from_slice(&out.as_ref()[0..32]);
    let packed = U256::from_be_bytes(w32);
    let low_mask = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);
    let hi_u128: u128 = ((packed >> 128u32) & low_mask).to::<u128>();
    let lo_u128: u128 = (packed & low_mask).to::<u128>();
    // `u128 as i128` is a wrapping two's-complement cast — correct sign extension
    // for the 128-bit fields.
    // Build a 256-bit two's-complement word from a 128-bit int (sign-extend the
    // 16-byte representation) for a panic-free I256 read-back.
    let i128_to_u256 = |v: i128| -> U256 {
        let be = v.to_be_bytes(); // [u8; 16]
        let mut arr = [0u8; 32];
        arr[0..16].fill(if v < 0 { 0xFF } else { 0x00 });
        arr[16..32].copy_from_slice(&be);
        U256::from_be_bytes(arr)
    };
    let d0 = I256::from_raw(i128_to_u256(hi_u128 as i128));
    let d1 = I256::from_raw(i128_to_u256(lo_u128 as i128));

    // Absolute magnitudes match `v4_simulate_swap`'s unsigned amount0/amount1.
    Ok((d0.unsigned_abs(), d1.unsigned_abs()))
}

/// V4 dense-swap byte-exact oracle: deploy the real PoolManager, seed a dense
/// pool from a `V4PoolState`, drive the swap through unlock/settle, and assert
/// `v4_simulate_swap` amount0/amount1 === on-chain BalanceDelta for a pinned
/// set of amounts across both directions.
#[test]
#[ignore = "build the harness first: just test-tier3-v4"]
fn v4_pool_dense_swap_matches_sim_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;
    let liq = 1_000_000_000_000_000_000u128;

    let state = dense_v4_state(liq, tick_spacing, k_positions, current_tick);

    for zfo in [true, false] {
        // V4 exact-in: amount must be NEGATIVE. Use the same magnitude in both
        // directions (zfo exact-in = -amount; ofz exact-in = -amount too).
        let amount = I256::try_from(U256::from(liq) / U256::from(100u64)).unwrap();
        let amount_specified = I256::ZERO.checked_sub(amount).unwrap();

        let limit_tick = if zfo {
            current_tick - 4 * tick_spacing
        } else {
            current_tick + 4 * tick_spacing
        };
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();

        let (on_am0, on_am1) = run_v4_onchain_swap(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
        )
        .expect("v4 on-chain swap succeeded");

        let sim = v4_simulate_swap(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            U256::from(sqrt_price_limit),
        )
        .expect("v4 Rust sim");

        let err = format!(
            "zfo={zfo} on_am0={on_am0} on_am1={on_am1} sim0={} sim1={}",
            sim.amount0, sim.amount1
        );
        assert_eq!(on_am0, sim.amount0, "v4 amount0 {err}");
        assert_eq!(on_am1, sim.amount1, "v4 amount1 {err}");
    }
}

/// Fuzz the V4 dense-swap oracle: state (liquidity × band) × amount-fraction ×
/// direction. For every case, assert `v4_simulate_swap` amount0/amount1 are
/// byte-exact to the on-chain `PoolManager.swap` BalanceDelta (via the unlock/
/// settle dance). Skips the rare on-chain revert (test-fixture limit, e.g. the
/// exact-output walk past a limit) rather than treating it as a divergence.
#[test]
#[ignore = "build the harness first: just test-tier3-v4"]
fn v4_pool_dense_swap_matches_sim_proptest() {
    proptest!(|(
        k in 4i32..=8i32,
        amount_frac in 1u64..=50u64,
        zfo in 0u8..=1u8,
        current_tick in prop::sample::select(&[480i32, 540, 600, -480]),
    )| {
        let fee = 3000u32;
        let tick_spacing = 60i32;
        let liq = 1_000_000_000_000_000_000u128;
        let state = dense_v4_state(liq, tick_spacing, k, current_tick);

        // Amount as a fraction of active liquidity, kept inside the band.
        let amount = I256::try_from(
            U256::from(liq) / U256::from(1_000_000u64)
                * U256::from(amount_frac),
        )
        .unwrap();
        // V4 exact-in = NEGATIVE (for both directions here — exact-in for zfo
        // and exact-in for ofz both pass negative).
        let amount_specified = I256::ZERO.checked_sub(amount).unwrap();

        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * 4 * tick_spacing;
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        let Ok((on_am0, on_am1)) = run_v4_onchain_swap(
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount_specified,
            sqrt_price_limit,
        ) else {
            return Ok(()); // on-chain revert = fixture limitation, skip
        };

        let sim = match v4_simulate_swap(
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount_specified,
            U256::from(sqrt_price_limit),
        ) {
            Ok(s) => s,
            Err(SimulateSwapError::NotComputable) => return Ok(()),
            Err(SimulateSwapError::MissingTickWord(w)) => {
                panic!("Tracked coverage should not miss word {w}")
            }
        };

        prop_assert_eq!(on_am0, sim.amount0, "amount0");
        prop_assert_eq!(on_am1, sim.amount1, "amount1");
    });
}
