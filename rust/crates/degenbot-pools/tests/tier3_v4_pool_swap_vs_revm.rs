//! Tier-3b V4 `PoolManager.swap` end-to-end oracle (ergo task `2LTKVO`, epic
//! UP5NH6) — the V4 twin of `tier3_v3_pool_swap_vs_revm.rs`. Hardened per epic
//! `CMORFZ` task `5KS2SQ` (H1 rejection-reason airtightness, H3 pinned edge
//! corpus, H4 widened proptest across fee-1/3000 + protocol-fee on/off).
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
//! Runs in the default `cargo test --workspace` suite. The canonical
//! v4-core bytecode is loaded from the committed `tier3-oracle/artifacts/`
//! tree (no solc/forge needed to RUN). Artifact integrity is enforced two
//! ways: `tier3_harness_artifacts.rs` hashes the tracked sources
//! (toolchain-free), and `tier3-oracle/verify-tier3-artifacts.sh` recompiles
//! every harness and byte-compares it to the committed artifact. After a
//! harness-source edit, regenerate + publish via
//! `tier3-oracle/build-tier3-v4-swap-harness.sh`.
//!
//! ## Shared fixture (H5)
//!
//! The Tier-2 dual-driver CL-math fixture `v4_swap.json` is a SINGLE shared
//! file (HRT356) consumed by BOTH consumers — `rust/crates/degenbot/tests/
//! parity_v4_swap.rs` (Rust) and `tests/standalone_parity/test_v4_swap_
//! dual_driver.py` (Python) — so the V4 math constant is never independently
//! redefined on either side. This tier-3 oracle uses its own REAL recorded
//! scalars (the live fee-1 reproduction) rather than duplicating the
//! dual-driver fixture, so there is no cross-file constant to drift.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)] // Solidity/V4 identifiers (PoolManager, slot0…)

// Reuse the CL-family shared driver + the arbitrary-topology position type and
// grid-snapping helpers from the V3/Pancake-V3 oracle's common module.
mod tier3_v3_common;

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
use degenbot_decoders::revert::RevertClass;
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

/// The verdict of one on-chain V4 swap probe — distinguishes a real EVM
/// verdict (Accepted vs Reverted) from a broken/aborted pipeline (Halted:
/// deploy/getter failure or the OOG empty-word-walk gas trap, which emits no
/// verdict). H1 rejection-parity matches on this shape instead of a blanket
/// error skip.
enum ProbeOutcome {
    /// The swap succeeded; `amount0/amount1` are the ABSOLUTE BalanceDelta.
    Accepted { amount0: U256, amount1: U256 },
    /// The swap reverted with the raw revert return-data (a V4 custom error
    /// selector like `CurrencyNotSettled`, or an `Error(string)`).
    Reverted { reason: Bytes },
    /// The pipeline itself broke (deploy/getter failure or the documented
    /// empty-word-walk OOG halt) — no EVM verdict to compare against.
    Halted(String),
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
        .join("artifacts")
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
            // Seed the pool's OWN protocol fee from the `V4PoolState` — NOT a
            // hardcoded 0. A pool with `protocol_fee > 0` exercises the
            // on-chain `Pool.swap` `calculateSwapFee(direction_fee, lp_fee)`
            // path; omitting it (the original oracle, protocol_fee=0 everywhere)
            // would make the revm oracle blind to the protocol-fee fee-combination
            // rounding — the exact gap the fee-1/tiny over-prediction (UO3JM4)
            // probes. `state.protocol_fee` uses the same 24-bit packing as
            // on-chain `slot0.protocolFee` (low 12 bits = 0→1, high 12 = 1→0), so
            // seeding it verbatim reproduces the on-chain fee the swap charges.
            protocol_fee: state.protocol_fee,
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
/// istically instead of chasing an empty word edge. `fee`/`protocol_fee` are
/// threaded into the state (identity + the on-chain `calculateSwapFee`
/// combination) so the same builder reproduces fee-3000, fee-1, and
/// protocol-fee-on pools.
fn dense_v4_state(
    liq: u128,
    tick_spacing: i32,
    k_positions: i32,
    current_tick: i32,
    fee: u32,
    protocol_fee: u32,
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
            fee,
            tick_spacing,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee,
        sqrt_price_x96: sp,
        liquidity: u128::try_from(i128::try_from(liq).unwrap() * i128::from(k_positions)).unwrap(),
        tick: current_tick,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// Build a fully-tracked V4 state from an **arbitrary** liquidity distribution
/// — the V4 twin of `tier3_v3_common::build_arbitrary_v3_state`. Folds the
/// same `ArbV3Position` layout into `tick_data` (each `lower` +liq gross/net,
/// each `upper` +gross/−net), derives active liquidity as the sum covering
/// `current_tick`, and snaps every boundary to the `tick_spacing` grid via the
/// shared helpers (an off-grid boundary floors in the on-chain tickBitmap —
/// the divergence the V3 fuzz surfaced). `fee`/`protocol_fee` thread through
/// `from_params` so the same builder reproduces fee-3000, fee-1, and
/// protocol-fee-on pools.
fn build_arbitrary_v4_state(
    current_tick: i32,
    tick_spacing: i32,
    fee: u32,
    protocol_fee: u32,
    positions: &[tier3_v3_common::ArbV3Position],
) -> V4PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    let mut active: u128 = 0;
    for p in positions {
        let lower = tier3_v3_common::snap_tick_floor(p.lower, tick_spacing);
        let upper = tier3_v3_common::snap_tick_ceil(p.upper, tick_spacing);
        if lower >= upper {
            // Too thin to survive snapping — drop (contributes no real range).
            continue;
        }
        if lower <= current_tick && current_tick < upper {
            active = active.saturating_add(p.liquidity);
        }
        let lo = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        lo.liquidity_gross = alloy::primitives::U128::from(
            lo.liquidity_gross.to::<u128>().saturating_add(p.liquidity),
        );
        lo.liquidity_net = I256::try_from(
            i128::try_from(lo.liquidity_net)
                .unwrap()
                .saturating_add(i128::try_from(p.liquidity).unwrap()),
        )
        .unwrap();
        let hi = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: alloy::primitives::U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        hi.liquidity_gross = alloy::primitives::U128::from(
            hi.liquidity_gross.to::<u128>().saturating_add(p.liquidity),
        );
        hi.liquidity_net = I256::try_from(
            i128::try_from(hi.liquidity_net)
                .unwrap()
                .saturating_sub(i128::try_from(p.liquidity).unwrap()),
        )
        .unwrap();
    }
    let params = degenbot_pools::v4_state::RegisterV4PoolParams {
        pool_manager: Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::ZERO,
            fee,
            tick_spacing,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee,
        sqrt_price_x96: sp,
        liquidity: active,
        tick: current_tick,
        tick_data,
        update_block: 0,
        tick_data_block: None,
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

/// Probe ONE on-chain V4 swap (deploy PoolManager → seed → unlock → swap →
/// settle) and classify the verdict. Returns [`ProbeOutcome::Accepted`] with
/// the ABSOLUTE (amount0, amount1) from the pool's BalanceDelta on success,
/// [`ProbeOutcome::Reverted`] with the raw revert data on a Solidity/V4-error
/// revert, or [`ProbeOutcome::Halted`] when the pipeline itself broke (deploy/
/// getter failure or the OOG gas trap — no EVM verdict).
#[allow(clippy::too_many_lines)] // one logical deploy → seed → unlock → swap → verdict pipeline
fn probe_v4(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> ProbeOutcome {
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
        other => return ProbeOutcome::Halted(format!("V4 harness deploy failed: {other:?}")),
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
    let manager = match get_addr("manager()") {
        Ok(a) => a,
        Err(e) => return ProbeOutcome::Halted(e),
    };
    let cur0 = match get_addr("currency0()") {
        Ok(a) => a,
        Err(e) => return ProbeOutcome::Halted(e),
    };
    let cur1 = match get_addr("currency1()") {
        Ok(a) => a,
        Err(e) => return ProbeOutcome::Halted(e),
    };
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

    match &res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => {
            let out = b.clone();
            // BalanceDelta is ONE packed word: amount0 in the HIGH 128 bits,
            // amount1 in the LOW 128 bits (v4-core `types/BalanceDelta.sol`).
            // Sign-extend each 128-bit field to a proper signed value.
            let mut w32 = [0u8; 32];
            w32.copy_from_slice(&out.as_ref()[0..32]);
            let packed = U256::from_be_bytes(w32);
            let low_mask = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);
            let hi_u128: u128 = ((packed >> 128u32) & low_mask).to::<u128>();
            let lo_u128: u128 = (packed & low_mask).to::<u128>();
            // Build a 256-bit two's-complement word from a 128-bit int
            // (sign-extend the 16-byte representation) for a panic-free I256
            // read-back.
            let i128_to_u256 = |v: i128| -> U256 {
                let be = v.to_be_bytes(); // [u8; 16]
                let mut arr = [0u8; 32];
                arr[0..16].fill(if v < 0 { 0xFF } else { 0x00 });
                arr[16..32].copy_from_slice(&be);
                U256::from_be_bytes(arr)
            };
            let d0 = I256::from_raw(i128_to_u256(hi_u128 as i128));
            let d1 = I256::from_raw(i128_to_u256(lo_u128 as i128));
            // Absolute magnitudes match `v4_simulate_swap`'s unsigned amounts.
            ProbeOutcome::Accepted {
                amount0: d0.unsigned_abs(),
                amount1: d1.unsigned_abs(),
            }
        }
        ExecutionResult::Revert { output, .. } => ProbeOutcome::Reverted {
            reason: output.clone(),
        },
        other => ProbeOutcome::Halted(format!("on-chain v4 swap halted: {other:?}")),
    }
}

/// Unwrap a probe to its absolute BalanceDelta, panicking on any non-accept so
/// pinned tests can assert known-good swaps fail loudly.
fn probe_accepted(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> (U256, U256) {
    match probe_v4(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        sqrt_price_limit,
    ) {
        ProbeOutcome::Accepted { amount0, amount1 } => (amount0, amount1),
        ProbeOutcome::Reverted { reason } => {
            panic!(
                "on-chain v4 swap reverted: {}",
                RevertClass::classify(reason.as_ref()).label()
            )
        }
        ProbeOutcome::Halted(m) => panic!("on-chain v4 swap halted: {m}"),
    }
}

/// The full V4 byte-exact oracle for one case, with H1 rejection-reason
/// airtightness: on-chain Accepted ⇒ engine Ok (compared byte-for-byte via the
/// BalanceDelta, the on-chain V4-error label surfaced on divergence); on-chain
/// Revert (a verdict — e.g. `CurrencyNotSettled`, decoded via
/// `degenbot_decoders::revert`) ⇒ engine `NotComputable`; only a verbless
/// Halt (pipeline break / OOG) is a legitimate skip.
#[allow(clippy::match_same_arms)] // two parity arms legitimately share an empty body
#[allow(clippy::too_many_lines)]
fn assert_v4_byte_exact(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) {
    let sim = v4_simulate_swap(
        state,
        fee,
        tick_spacing,
        zero_for_one,
        amount_specified,
        U256::from(sqrt_price_limit),
    );
    match (
        probe_v4(
            state,
            fee,
            tick_spacing,
            zero_for_one,
            amount_specified,
            sqrt_price_limit,
        ),
        &sim,
    ) {
        (ProbeOutcome::Accepted { amount0, amount1 }, Ok(s)) => {
            assert_eq!(amount0, s.amount0, "v4 amount0 byte-exact");
            assert_eq!(amount1, s.amount1, "v4 amount1 byte-exact");
        }
        (ProbeOutcome::Accepted { .. }, Err(e)) => {
            panic!("on-chain ACCEPTED but engine rejected: {e:?}")
        }
        (ProbeOutcome::Reverted { .. }, Err(SimulateSwapError::NotComputable)) => {
            // Parity: both reject — no silent skip; the on-chain verdict was a
            // real Solidity/V4 revert and the engine agrees.
        }
        (ProbeOutcome::Reverted { reason }, _) => {
            let label = RevertClass::classify(reason.as_ref()).label();
            match sim {
                Ok(s) => panic!("on-chain REVERTED ({label}) but engine produced {s:?}"),
                Err(SimulateSwapError::MissingTickWord(w)) => {
                    panic!("on-chain REVERTED ({label}) but engine misses word {w}")
                }
                Err(SimulateSwapError::NotComputable) => unreachable!(),
            }
        }
        (ProbeOutcome::Halted(_), _) => {
            // Verbless halt (pipeline break or OOG gas trap) — no EVM verdict
            // to compare against the engine. The only legitimate skip.
        }
    }
}

/// V4 dense-swap byte-exact oracle: deploy the real PoolManager, seed a dense
/// pool from a `V4PoolState`, drive the swap through unlock/settle, and assert
/// `v4_simulate_swap` amount0/amount1 === on-chain BalanceDelta for a pinned
/// set of amounts across both directions.
#[test]
fn v4_pool_dense_swap_matches_sim_byte_exact() {
    let fee = 3000u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;
    let liq = 1_000_000_000_000_000_000u128;

    let state = dense_v4_state(liq, tick_spacing, k_positions, current_tick, fee, 0);

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

        let (on_am0, on_am1) = probe_accepted(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
        );

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

/// H3 — pinned deterministic edge corpus: 1-wei at wei-scale liquidity, tiny +
/// large liquidity, boundary amounts, fee-3000 + fee-1, protocol-fee on/off,
/// both directions. Each case runs the full H1 oracle so a byte-exactness or
/// rejection-parity drift fails loudly.
#[test]
fn v4_pool_edge_corpus_is_byte_exact() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    // (liq, amount, fee, zfo, protocol_fee).
    let cases: &[(u128, u128, u32, bool, u32)] = &[
        // 1-wei amount at wei-scale liquidity.
        (2, 1, 3000, true, 0),
        (2, 1, 3000, false, 0),
        // Tiny liquidity, wei-scale amounts (floor-division-sensitive region).
        (1_000, 5, 3000, true, 0),
        (1_000, 5, 3000, false, 0),
        // Fee-1 tiny.
        (100_000, 100, 1, true, 0),
        (100_000, 100, 1, false, 0),
        // Boundary amount deep into the band (amount == active liquidity).
        (
            1_000_000_000_000_000_000u128,
            8_000_000_000_000_000_000u128,
            3000,
            true,
            0,
        ),
        (
            1_000_000_000_000_000_000u128,
            8_000_000_000_000_000_000u128,
            3000,
            false,
            0,
        ),
        // Large liquidity, proportionally large amount.
        (
            1_000_000_000_000_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000_000_000u128,
            3000,
            true,
            0,
        ),
        // Protocol-fee-on (low-12 = 13 pips 0→1, high-12 = 13 pips 1→0).
        (
            1_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000u128,
            3000,
            true,
            0x0000_d00d,
        ),
        (
            1_000_000_000_000_000_000u128,
            1_000_000_000_000_000_000u128,
            3000,
            false,
            0x0000_d00d,
        ),
    ];

    for &(liq, amount, fee, zfo, protocol_fee) in cases {
        let state = dense_v4_state(
            liq,
            tick_spacing,
            k_positions,
            current_tick,
            fee,
            protocol_fee,
        );
        let amount_specified = I256::ZERO
            .checked_sub(I256::try_from(U256::from(amount)).unwrap())
            .unwrap();
        let dir = if zfo { -1 } else { 1 };
        let limit_tick = current_tick + dir * 3 * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();
        assert_v4_byte_exact(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
        );
    }
}

/// H4 — the widened V4 proptest strategy: `prop_oneof!` self-consistent arms
/// (nominal wide dynamic range, tiny/floor-division, large, and a
/// protocol-fee-on arm) producing `(liq, amount, zfo, sink_ticks, fee,
/// protocol_fee)` tuples where `amount` is coupled to `liq` so each walk
/// terminates inside the band (no empty-word OOG = no verbless skip).
fn v4_case_strategy() -> impl Strategy<Value = (u128, U256, i32, i32, u32, u32)> {
    // Nominal wide dynamic range, fee-3000, protocol fee off.
    let nominal = (1u32..22u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        (
            Just(liq),
            1u32..200u32,
            0i32..2,
            1i32..4,
            Just(3000u32),
            Just(0u32),
        )
            .prop_map(move |(_, frac, zfo, sink, fee, pf)| {
                (
                    liq,
                    U256::from(liq) / U256::from(1_000_000u64) * U256::from(frac),
                    zfo,
                    sink,
                    fee,
                    pf,
                )
            })
    });
    // Tiny liquidity + wei-scale amounts (floor-division region), fee 1 or 3000.
    let tiny = (0u32..7u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        let active = liq * 8;
        (
            Just(liq),
            1u128..(active.min(1_000_000u128) + 1),
            0i32..2,
            1i32..3,
            prop_oneof![Just(1u32), Just(3000u32)],
            Just(0u32),
        )
            .prop_map(move |(_, amount, zfo, sink, fee, pf)| {
                (liq, U256::from(amount), zfo, sink, fee, pf)
            })
    });
    // Large liquidity + proportionally large amounts, fee-3000.
    let large = (23u32..30u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        (
            Just(liq),
            100u32..2000u32,
            0i32..2,
            1i32..4,
            Just(3000u32),
            Just(0u32),
        )
            .prop_map(move |(_, frac, zfo, sink, fee, pf)| {
                (
                    liq,
                    U256::from(liq) / U256::from(1_000_000u64) * U256::from(frac),
                    zfo,
                    sink,
                    fee,
                    pf,
                )
            })
    });
    // Protocol-fee on/off over a mid liquidity band (exercises the on-chain
    // calculateSwapFee combination + the fee-combination rounding).
    let proto = (10u32..20u32).prop_flat_map(|liq_exp| {
        let liq = 10u128.pow(liq_exp);
        (
            Just(liq),
            2u32..50u32,
            0i32..2,
            1i32..3,
            Just(3000u32),
            prop_oneof![Just(0u32), Just(0x0000_d00du32), Just(0x0001u32)],
        )
            .prop_map(move |(_, frac, zfo, sink, fee, pf)| {
                (
                    liq,
                    U256::from(liq) / U256::from(1_000_000u64) * U256::from(frac),
                    zfo,
                    sink,
                    fee,
                    pf,
                )
            })
    });
    prop_oneof![nominal, tiny, large, proto]
}

/// Fuzz the V4 dense-swap oracle across the widened (liq, amount, direction,
/// band-depth, fee, protocol-fee) domain: assert `v4_simulate_swap`
/// amount0/amount1 are byte-exact to the on-chain `PoolManager.swap`
/// BalanceDelta (via unlock/settle), with H1 rejection-reason airtightness.
#[test]
fn v4_pool_dense_swap_matches_sim_proptest() {
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;

    proptest!(|(case in v4_case_strategy())| {
        let (liq, amount, zfo, sink_ticks, fee, protocol_fee) = case;
        if amount > U256::from(i128::MAX) {
            return Ok(());
        }
        let mag = I256::try_from(amount).unwrap();
        if mag.is_zero() {
            return Ok(());
        }
        // V4 exact-in = NEGATIVE for both directions.
        let amount_specified = I256::ZERO.checked_sub(mag).unwrap();
        let state = dense_v4_state(liq, tick_spacing, k_positions, current_tick, fee, protocol_fee);
        let dir = if zfo == 0 { -1 } else { 1 };
        let limit_tick = current_tick + dir * sink_ticks * tick_spacing;
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        assert_v4_byte_exact(
            &state,
            fee,
            tick_spacing,
            zfo == 0,
            amount_specified,
            sqrt_price_limit,
        );
    });
}

/// H4 — topology fuzz (V4 twin): the byte-exact oracle driven over an
/// **arbitrary** liquidity distribution through [`build_arbitrary_v4_state`].
/// Randomizes the whole `tick_data` layout so the PoolManager walk must handle
/// initialized ticks crossing the current tick, empty bitmap-word regions
/// between far-apart boundaries, and one-sided ranges that run to a price
/// limit / the bitmap end — same axes as the V3 topology fuzz, plus the
/// fee/protocol-fee combination. Reuses the shared H1 verdict protocol via
/// [`assert_v4_byte_exact`].
#[allow(clippy::items_after_statements)] // `fn strategy` is local to the test
#[allow(clippy::too_many_lines)]
#[test]
fn v4_pool_arbitrary_liquidity_matches_sim_proptest() {
    fn strategy() -> impl Strategy<
        Value = (
            i32,
            i32,
            Vec<tier3_v3_common::ArbV3Position>,
            u32,
            u32,
            i32,
            u32,
            i32,
        ),
    > {
        // (spacing, current_tick, positions, fee, protocol_fee, zfo, frac, limit_words)
        prop_oneof![Just(10i32), Just(60i32)].prop_flat_map(|spacing| {
            let words = 256 * spacing; // one tick-bitmap word in ticks
            (-60_000i32..60_000i32).prop_flat_map(move |cur| {
                // Random position liquidity magnitude: wei-scale to ~1e23.
                let liq = prop_oneof![
                    1_000u128..1_000_000u128,
                    (1u32..24u32).prop_map(|e| 10u128.pow(e)),
                ];

                // --- Arm 1: a contiguous band anchored covering `cur` ---
                let band_positions = {
                    let pos = ((-4 * words..-1i32), (1i32..4 * words), liq.clone()).prop_map(
                        move |(lo, hi, l)| tier3_v3_common::ArbV3Position {
                            lower: cur + lo,
                            upper: cur + hi,
                            liquidity: l,
                        },
                    );
                    // Anchor ALWAYS covers `cur` so arm-1 seed liquidity > 0.
                    let anchor = tier3_v3_common::ArbV3Position {
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
                // position covers the current tick and the seed liquidity is 0
                // — the real mainnet pattern (liquidity withdrawn, no swap): a
                // swap must walk the empty region back into remaining liquidity.
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
                            tier3_v3_common::ArbV3Position {
                                lower: near.min(far),
                                upper: near.max(far),
                                liquidity: l,
                            }
                        });
                    prop::collection::vec(band, 1usize..=2usize)
                };

                let rest = (
                    prop_oneof![Just(500u32), Just(3000u32)],
                    prop_oneof![Just(0u32), Just(0x0000_d00du32)],
                    0i32..2,
                    1u32..=300,
                    1i32..=12,
                );
                prop_oneof![band_positions, empty_positions].prop_flat_map(move |positions| {
                    rest.clone()
                        .prop_map(move |(fee, pf, zfo, frac, limit_words)| {
                            (
                                spacing,
                                cur,
                                positions.clone(),
                                fee,
                                pf,
                                zfo,
                                frac,
                                limit_words,
                            )
                        })
                })
            })
        })
    }

    proptest!(|(case in strategy())| {
        let (spacing, cur, positions, fee, pf, zfo, frac, limit_words) = case;
        let state = build_arbitrary_v4_state(cur, spacing, fee, pf, &positions);
        let active = state.liquidity;
        // Couple amount to active liquidity so `computeSwapStep` moves price
        // (the OOG-trap guard) while spanning tiny → deep pushes. When the
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
        let mag = I256::try_from(amount).unwrap();
        // V4 exact-in is NEGATIVE for both directions.
        let amount_specified = I256::ZERO.checked_sub(mag).unwrap();
        let zero_for_one = zfo == 0;
        let dir = if zero_for_one { -1i32 } else { 1i32 };
        let limit_tick = (cur + dir * limit_words * spacing * 256).clamp(-887_272, 887_272);
        let sqrt_price_limit: u128 =
            get_sqrt_ratio_at_tick_internal(limit_tick).unwrap().to::<u128>();

        assert_v4_byte_exact(
            &state,
            fee,
            spacing,
            zero_for_one,
            amount_specified,
            sqrt_price_limit,
        );
    });
}

/// H3 — pinned deterministic edge corpus (V4 twin of the V3 arbitrary-topology
/// corpus): explicit topologies that cross initialized ticks, span empty
/// words, and run to a bitmap end, each driven through the byte-exact oracle.
#[test]
#[allow(clippy::too_many_lines)]
fn v4_pool_arbitrary_topology_edge_corpus() {
    let fee = 3000u32;
    let protocol_fee = 0u32;
    let tick_spacing = 60i32;
    let cur = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    let cases: Vec<(Vec<tier3_v3_common::ArbV3Position>, bool)> = vec![
        // (1) Crossing: overlapping dense bands around the current tick — the
        //     upward walk crosses several distinct initialized boundaries.
        (
            vec![
                tier3_v3_common::ArbV3Position {
                    lower: cur - 4 * tick_spacing,
                    upper: cur + 4 * tick_spacing,
                    liquidity: liq,
                },
                tier3_v3_common::ArbV3Position {
                    lower: cur - 2 * tick_spacing,
                    upper: cur + 6 * tick_spacing,
                    liquidity: liq,
                },
                tier3_v3_common::ArbV3Position {
                    lower: cur,
                    upper: cur + 8 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false, // upward, crosses 4+ boundaries
        ),
        // (2) Empty-word crossing downward: a dense band far below (2 words),
        //     so the downward walk crosses empty bitmap words to reach it.
        (
            vec![
                tier3_v3_common::ArbV3Position {
                    lower: cur - 2 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
                tier3_v3_common::ArbV3Position {
                    lower: cur - 2 * 256 * tick_spacing,
                    upper: cur - 2 * 256 * tick_spacing + 4 * tick_spacing,
                    liquidity: liq,
                },
            ],
            true, // downward, ~2 empty words then the far band
        ),
        // (3) Run to the bitmap end: the current tick sits inside a range whose
        //     UPWARD exit leaves all liquidity behind, so the upward swap
        //     crosses out of it and runs (empty) to the far price limit.
        (
            vec![
                tier3_v3_common::ArbV3Position {
                    lower: cur - 10 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
                tier3_v3_common::ArbV3Position {
                    lower: cur - 2 * 256 * tick_spacing,
                    upper: cur + 2 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false, // upward, out of the range then empty to the limit
        ),
    ];

    for (positions, zfo) in cases {
        let state = build_arbitrary_v4_state(cur, tick_spacing, fee, protocol_fee, &positions);
        let active = state.liquidity;
        let amount = U256::from(active) * U256::from(8u64);
        if amount.is_zero() || amount > U256::from(i128::MAX) {
            continue;
        }
        let mag = I256::try_from(amount).unwrap();
        let amount_specified = I256::ZERO.checked_sub(mag).unwrap();
        let dir = if zfo { -1i32 } else { 1i32 };
        let limit_tick = (cur + dir * 6 * tick_spacing * 256).clamp(-887_272, 887_272);
        let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
            .unwrap()
            .to::<u128>();
        assert_v4_byte_exact(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
        );
    }
}

/// A real mainnet pattern (the user-raised case, V4 twin): the current price
/// sits in an EMPTY region — liquidity withdrawn, no swap yet — with remaining
/// liquidity on one side (or both), and a swap must walk the empty region back
/// into that liquidity. Starts with seed `liquidity == 0` and asserts the Rust
/// walk matches the on-chain PoolManager byte-exact, strictly (must be
/// Accepted). Covers liquidity only above, only below, and on both sides, at
/// several amounts.
#[test]
fn v4_pool_start_in_empty_region_crosses_to_liquidity() {
    let fee = 3000u32;
    let protocol_fee = 0u32;
    let tick_spacing = 60i32;
    let cur = 30_000i32;
    let liq = 1_000_000_000_000_000_000_000u128; // 1e21

    let cases: Vec<(Vec<tier3_v3_common::ArbV3Position>, bool)> = vec![
        // Liquidity only ABOVE; price sits in the empty region below, crossing up.
        (
            vec![tier3_v3_common::ArbV3Position {
                lower: cur + 100 * tick_spacing,
                upper: cur + 500 * tick_spacing,
                liquidity: liq,
            }],
            false,
        ),
        // Liquidity only BELOW; price sits in the empty region above, crossing down.
        (
            vec![tier3_v3_common::ArbV3Position {
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
                tier3_v3_common::ArbV3Position {
                    lower: cur + 100 * tick_spacing,
                    upper: cur + 500 * tick_spacing,
                    liquidity: liq,
                },
                tier3_v3_common::ArbV3Position {
                    lower: cur - 500 * tick_spacing,
                    upper: cur - 100 * tick_spacing,
                    liquidity: liq,
                },
            ],
            false,
        ),
    ];

    for (positions, zfo) in cases {
        let state = build_arbitrary_v4_state(cur, tick_spacing, fee, protocol_fee, &positions);
        // The price must genuinely start in an empty region (zero active liq).
        assert_eq!(state.liquidity, 0, "price must start in an empty region");
        for frac in [1u64, 10u64, 100u64] {
            let mag = I256::try_from(U256::from(liq) / U256::from(frac)).unwrap();
            // V4 exact-in is NEGATIVE for both directions.
            let amount_specified = I256::ZERO.checked_sub(mag).unwrap();
            let dir = if zfo { -1i32 } else { 1i32 };
            let limit_tick = (cur + dir * 6 * tick_spacing * 256).clamp(-887_272, 887_272);
            let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
                .unwrap()
                .to::<u128>();
            // Strict: must be Accepted on-chain (byte-exact); a Revert or the
            // OOG gas-trap Halt here is a fixture failure, not a skip.
            let (amount0, amount1) = probe_accepted(
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_specified,
                sqrt_price_limit,
            );
            let sim = v4_simulate_swap(
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_specified,
                U256::from(sqrt_price_limit),
            )
            .expect("engine must accept an on-chain-accepted empty-region start");
            assert_eq!(amount0, sim.amount0, "amount0 byte-exact");
            assert_eq!(amount1, sim.amount1, "amount1 byte-exact");
        }
    }
}

// ---------------------------------------------------------------------------
// Fee-1 / tiny-liquidity discriminator (ergo UO3JM4 — the V3→V4→V3 1-wei
// take-overdraw observed on a fee-1 V4 pool at ~1:1 price).
//
// The live reproduction (paths 10234/10338): V4(fee=1, zfo=false),
// `sq=79_231_869_042_278_935_382_727_675_145, liq=94294142` — a fee-1 stable pool at
// essentially price 1 (current_tick 0). On-chain PoolManager yielded
// `actual_out=9585` vs the solver's `predicted=9586` (1 wei over-prediction).
//
// This is the TDD-first discriminator: feed the IDENTICAL `V4PoolState` to
// BOTH `v4_simulate_swap` (the Rust twin) and the on-chain PoolManager via
// this revm harness and assert byte-exact. If it matches, the 1-wei cannot be
// in `v4_simulate_swap`'s math and must be solver stale-state; if it diverges,
// a genuine fee-1/tiny-amount rounding bug lives in `compute_swap_step_v4` /
// `v4_simulate_swap`.
// ---------------------------------------------------------------------------

/// Reproduction scalars — the fee-1 V4 hop that over-drew by 1 wei
/// (paths 10234/10338, observed 2026-08-02).
const FEE1_REPRO_SQ_X96: u128 = 79_231_869_042_278_935_382_727_675_145;
const FEE1_REPRO_LIQ: u128 = 94_294_142;
const FEE1_REPRO_FEE: u32 = 1;

/// Clean fee-1 byte-exact discriminator (ergo UO3JM4): a PHYSICALLY VALID
/// single-position state (lower tick +liq, upper tick -liq; constant active
/// liquidity L across the whole walk), seeded at the exact reproduction
/// scalars (`sq=79_231_869_042_278_935_382_727_675_145`, `liq=94294142`, fee=1).
/// Asserts `v4_simulate_swap` amount0/amount1 === on-chain PoolManager
/// BalanceDelta byte-for-byte.
#[test]
fn v4_pool_fee1_valid_single_position_matches_sim() {
    let liq = FEE1_REPRO_LIQ;
    let fee = FEE1_REPRO_FEE;
    let tick_spacing = 1i32;
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    tick_data.insert(
        -100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    let params = degenbot_pools::v4_state::RegisterV4PoolParams {
        pool_manager: Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::ZERO,
            fee,
            tick_spacing,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(FEE1_REPRO_SQ_X96),
        liquidity: liq,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);

    for zfo in [false, true] {
        for amount in [9_000u64, 9_586, 10_000, 20_000, 50_000] {
            let amount_specified = I256::ZERO
                .checked_sub(I256::try_from(U256::from(amount)).unwrap())
                .unwrap();
            let dir = if zfo { -1i32 } else { 1i32 };
            let limit_tick = dir * 10 * tick_spacing;
            let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
                .unwrap()
                .to::<u128>();

            let (on_am0, on_am1) = probe_accepted(
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_specified,
                sqrt_price_limit,
            );
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
                "zfo={zfo} amount={amount} on_am0={on_am0} on_am1={on_am1} sim0={} sim1={}",
                sim.amount0, sim.amount1
            );
            assert_eq!(on_am0, sim.amount0, "v4 amount0 {err}");
            assert_eq!(on_am1, sim.amount1, "v4 amount1 {err}");
        }
    }
}

/// Build a physically-valid single-position V4 state at the fee-1 repro
/// scalars, with a caller-chosen protocol fee (the on-chain `slot0.protocolFee`
/// packing). Constant active liquidity `liq` across the whole walk (lower -100
/// +liq, upper +100 -liq) at price 1 (tick 0), mirroring
/// `v4_pool_fee1_valid_single_position_matches_sim` but parameterizing the
/// static LP fee + protocol fee so the fee-combination path is exercised.
///
/// The fixture's divergence pool carries lp_fee **50**/1e6 (0.005% — the
/// "fee-1" display label is the solver's print rounding artifact, NOT the
/// on-chain fee; see AGENTS.md‘s V4 Fee-1 section) + protocol override
/// `protocol_fee`. `fee` is the static lp fee (the `PoolKey.fee`), so the
/// caller must pass 50 to reproduce the true fee.
fn fee1_protocol_fee_state(protocol_fee: u32, lp_fee: u32) -> V4PoolState {
    let liq = FEE1_REPRO_LIQ;
    let tick_data_fee = lp_fee;
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    tick_data.insert(
        -100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        100,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liq),
            liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
            block: 0,
        },
    );
    let params = degenbot_pools::v4_state::RegisterV4PoolParams {
        pool_manager: Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::ZERO,
            fee: tick_data_fee,
            tick_spacing: 1,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee,
        sqrt_price_x96: U256::from(FEE1_REPRO_SQ_X96),
        liquidity: liq,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// **UO3JM4 protocol-fee discriminator** — the exact gap the fee-1 live record
/// leaves open. The fixture's divergence pool carried BOTH a tiny static fee
/// (`lp_fee=50`) AND a non-zero protocol override (`protocol_fee=0xd00d` =
/// 13 pips each direction). Every pre-existing tier-3b V4 oracle seeded
/// `protocol_fee: 0`, so the on-chain `calculateSwapFee(proto, lp)` fee-
/// combination path was NEVER byte-exactly cross-checked — yet that is the
/// exact path `v4_simulate_swap` charges on the divider state
/// (`calculate_swap_fee(protocol_fee_dir, fee)`, see `v4_state.rs`).
///
/// Seeds the canonical PoolManager with the SAME `V4PoolState` (via the now
/// `state.protocol_fee`-threaded `seed_v4_pool_storage`) and asserts
/// `v4_simulate_swap` amounts === on-chain BalanceDelta byte-for-byte. This
/// isolates "is `v4_simulate_swap`'s protocol-fee math byte-exact?" from any
/// fixture-state-reconstruction error.
#[test]
fn v4_pool_fee1_protocol_fee_override_matches_sim() {
    // The fixture's protocol override: 0xd00d -> low12 (0->1) = 13 pips,
    // high12 (1->0) = 13 pips. lp_fee=50 -> combined swap_fee = 63 pips
    // (13 + 50 - 13*50/1_000_000).
    let protocol_fee: u32 = 0x0000_d00d;
    let lp_fee = 50u32; // the fixture's real on-chain fee (not the "fee-1" label)
    let state = fee1_protocol_fee_state(protocol_fee, lp_fee);
    let fee = lp_fee;
    let tick_spacing = 1i32;

    // The exact recorded amounts from the live fee-1 repro (paths 10234/10338),
    // plus a spread around them, in BOTH directions.
    for zfo in [false, true] {
        for amount in [9_000u64, 9_583, 9_585, 9_586, 10_000, 50_000] {
            let amount_specified = I256::ZERO
                .checked_sub(I256::try_from(U256::from(amount)).unwrap())
                .unwrap();
            let dir = if zfo { -1i32 } else { 1i32 };
            let limit_tick = dir * 10 * tick_spacing;
            let sqrt_price_limit: u128 = get_sqrt_ratio_at_tick_internal(limit_tick)
                .unwrap()
                .to::<u128>();

            let (on_am0, on_am1) = probe_accepted(
                &state,
                fee,
                tick_spacing,
                zfo,
                amount_specified,
                sqrt_price_limit,
            );
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
                "zfo={zfo} amount={amount} proto_fee={protocol_fee} lp_fee={fee} on_am0={on_am0} on_am1={on_am1} sim0={} sim1={}",
                sim.amount0, sim.amount1
            );
            assert_eq!(on_am0, sim.amount0, "v4 amount0 {err}");
            assert_eq!(on_am1, sim.amount1, "v4 amount1 {err}");
        }
    }
}

/// Clean multi-tick fee-1 byte-exact oracle (ergo UO3JM4): mirrors the proven
/// fee-3000 dense test (`v4_pool_dense_swap_matches_sim_byte_exact`) but with
/// fee=1 and a smaller per-position liquidity, crossing 4 of 8 tick boundaries
/// (liquidity stays healthy — never drains to the degenerate tail-0 regime).
/// Asserts `v4_simulate_swap` amount0/amount1 === on-chain BalanceDelta.
#[test]
fn v4_pool_fee1_dense_matches_sim_byte_exact() {
    let fee = 1u32;
    let tick_spacing = 60i32;
    let k_positions = 8i32;
    let current_tick = 120i32;
    let liq = 10_000_000_000u128;
    let state = dense_v4_state(liq, tick_spacing, k_positions, current_tick, fee, 0);

    for zfo in [true, false] {
        // V4 exact-in (negative) — magnitude scaled to the tiny liquidity.
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

        let (on_am0, on_am1) = probe_accepted(
            &state,
            fee,
            tick_spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
        );
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
