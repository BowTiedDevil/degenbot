//! **Real v4-core `PoolManager` oracle driver** — deploy the canonical
//! PoolManager (via the committed `V4SwapOracleHarness` unlocker artifact), seed
//! a reconstructed pool's storage slot-for-slot, and drive a swap — as a reuseable
//! building block for the path-investigation fixtures (the deep, contract-agnostic
//! revm spine is [`degenbot_simulation::oracle`]; this module layers the V4
//! pool-specific deploy/seed/swap/call-encoding on top of it).
//!
//! **Layering.** Investigation tooling (like the wider [`crate::investigation`]
//! — run-once diagnostic scaffolding, not library surface), so it relaxes the
//! pedantic `doc_markdown`/`cast_possible_wrap` nits the production cores keep
//! (matching the `path5000_v4_gas_probe` example it was extracted from). It
//! still emits no `pyo3` and holds no engine state—it is a thin, contract-level
//! revm driver over the reusable [`degenbot_simulation::oracle`] spine.
#![expect(
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::expect_used,
    clippy::panic
)]
//!
//! Extracted from the `path5000_v4_gas_probe` example so the deploy+seed+drive
//! sequence lives in ONE place — the example and the committed tier-3-path5000
//! regression test (`tests/tier3_path5000_v4_clamp.rs`) share it. See that test
//! for the full RED→GREEN narrative (the CL-hop input clamp turns a 20.7M-gas
//! EMPTY-HALT into a ~190k clean fill under the 5M executor ceiling).

use std::path::PathBuf;

use alloy::primitives::{aliases::I256, Address, Bytes, U160, U256};
use degenbot_pools::v4_state::{V4PoolKey, V4PoolState};
use degenbot_pools::v4_storage_slots::{
    encode_v4_liquidity_slot, encode_v4_slot0, encode_v4_tick_info_slot, v4_liquidity_slot,
    v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot, v4_tick_bitmap_word_slot,
    v4_tick_mapping_slot, V4Slot0Parts,
};
use degenbot_simulation::oracle::{self, TxSpec, Verdict};

/// Resolve the tier3-oracle artifacts root from the repo (the crate's
/// `CARGO_MANIFEST_DIR` is `<repo>/rust/crates/degenbot`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Load a foundry-shaped harness artifact's creation bytecode (committed, so no
/// toolchain is needed at runtime).
#[expect(clippy::missing_panics_doc)]
pub fn load_creation_bytecode(file: &str, contract: &str) -> Bytes {
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
    Bytes::from(alloy::hex::decode(code).expect("hex object"))
}

/// ABI-encode the V4SwapOracleHarness constructor args `(uint24 fee, int24
/// tickSpacing)`.
pub fn harness_constructor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
    let mut args = vec![0u8; 64];
    args[28..32].copy_from_slice(&fee.to_be_bytes());
    args[60..64].copy_from_slice(&tick_spacing.to_be_bytes());
    args
}

/// Bitmap word value for one word from `tick_data` — the V4 bitmask packing is
/// identical to V3, so delegate to the shared V3 helper.
fn compute_v4_word_from_raw(
    tick_data: &std::collections::HashMap<i32, degenbot_pools::TickInfo>,
    tick_spacing: i32,
    word_pos: i16,
) -> U256 {
    degenbot_pools::v3_storage_slots::compute_v3_tick_bitmap_word_from_raw(
        tick_data,
        tick_spacing,
        word_pos,
    )
}

/// Seed the V4 `Pool.State` storage for the single pool at the manager, from a
/// reconstructed `V4PoolState`. The pool key's currencies are the harness's
/// deployed mock token addresses (read back via getters) so the derived poolId
/// matches the one the harness's `swap` will actually touch.
pub fn seed_v4_pool_storage(
    evm: &mut oracle::FixtureEvm,
    manager: Address,
    pool_key: &V4PoolKey,
    state: &V4PoolState,
    fee: u32,
) {
    let pool_id = v4_pool_id(pool_key);
    let base = v4_pool_state_base_slot(pool_id);

    let mut slots = Vec::new();
    slots.push((
        v4_slot0_slot(base),
        encode_v4_slot0(V4Slot0Parts {
            sqrt_price_x96: state.sqrt_price_x96,
            tick: state.tick,
            protocol_fee: state.protocol_fee,
            lp_fee: fee,
        }),
    ));
    slots.push((
        v4_liquidity_slot(base),
        encode_v4_liquidity_slot(state.liquidity),
    ));
    for (tick, info) in &state.tick_data {
        slots.push((
            v4_tick_mapping_slot(*tick, base),
            encode_v4_tick_info_slot(info),
        ));
    }
    let mut word_positions: std::collections::HashSet<i16> = std::collections::HashSet::new();
    for &tick in state.tick_data.keys() {
        let compressed = tick.div_euclid(pool_key.tick_spacing);
        let word_pos = i16::try_from(compressed >> 8).unwrap_or(0);
        word_positions.insert(word_pos);
    }
    for word_pos in word_positions {
        slots.push((
            v4_tick_bitmap_word_slot(word_pos, base),
            compute_v4_word_from_raw(&state.tick_data, pool_key.tick_spacing, word_pos),
        ));
    }
    oracle::seed_slots(evm, manager, &slots);
}

/// ABI-encode the V4 harness `swap(bool,int256,uint160)` call.
pub fn encode_v4_swap_call(
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: U160,
) -> Vec<u8> {
    let mut data = oracle::selector("swap(bool,int256,uint160)").to_vec();
    let mut buf = vec![0u8; 32];
    buf[31] = u8::from(zero_for_one);
    data.extend_from_slice(&buf);
    data.extend_from_slice(&amount_specified.into_raw().to_be_bytes::<32>());
    // uint160: pad the low 20 bytes.
    let mut lim = [0u8; 32];
    lim[12..32].copy_from_slice(&sqrt_price_limit.to_be_bytes::<20>());
    data.extend_from_slice(&lim);
    data
}

/// Decode the packed `BalanceDelta` (amount0 in the high 128 bits, amount1 in
/// the low 128 bits) into (amount0, amount1) absolute magnitudes.
pub fn decode_balance_delta(out: &[u8]) -> (U256, U256) {
    let mut w32 = [0u8; 32];
    w32.copy_from_slice(&out[0..32]);
    let packed = U256::from_be_bytes(w32);
    let low_mask = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);
    let hi_u128: u128 = ((packed >> 128u32) & low_mask).to::<u128>();
    let lo_u128: u128 = (packed & low_mask).to::<u128>();
    // Rebuild a 256-bit two's-complement word from a 128-bit int
    // (sign-extend the 16-byte representation).
    let i128_to_u256 = |v: i128| -> U256 {
        let be = v.to_be_bytes(); // [u8; 16]
        let mut arr = [0u8; 32];
        arr[0..16].fill(if v < 0 { 0xFF } else { 0x00 });
        arr[16..32].copy_from_slice(&be);
        U256::from_be_bytes(arr)
    };
    (
        I256::from_raw(i128_to_u256(hi_u128 as i128)).unsigned_abs(),
        I256::from_raw(i128_to_u256(lo_u128 as i128)).unsigned_abs(),
    )
}

/// Result of driving a swap through the real seeded PoolManager.
pub struct V4RealSwap {
    pub verdict: Verdict,
    /// `(amount0, amount1)` absolute magnitudes from the BalanceDelta (only
    /// meaningful when `verdict` is `Accepted`).
    pub delta: (U256, U256),
    pub gas_used: u64,
}

/// Deploy the real v4-core `PoolManager` via the `V4SwapOracleHarness`, seed
/// `state` slot-for-slot, and drive an exact-in swap at `amount_specified`
/// (NEGATIVE for V4 exact-in) with the given price limit and gas budget.
pub fn drive_real_v4_swap(
    state: &V4PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: U160,
    gas: u64,
) -> V4RealSwap {
    let mut evm = oracle::new_fixture_evm();
    oracle::set_disable_nonce_check(&mut evm, true);
    oracle::set_code_size_limits(&mut evm, None);
    oracle::set_block_gas_limit(&mut evm, std::cmp::max(gas, 30_000_000));
    oracle::set_tx_gas_limit_cap(&mut evm, u64::MAX);

    // Deploy V4SwapOracleHarness (constructs the canonical PoolManager).
    let mut init_code =
        load_creation_bytecode("V4SwapOracleHarness.sol", "V4SwapOracleHarness").to_vec();
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let harness = match oracle::deploy(&mut evm, Bytes::from(init_code), 16_000_000) {
        Ok(a) => a,
        Err(e) => {
            return V4RealSwap {
                verdict: Verdict::Halted(format!("harness deploy failed: {e}")),
                delta: (U256::ZERO, U256::ZERO),
                gas_used: 0,
            }
        }
    };

    // Read back the harness-deployed currency addresses.
    let cur0 = oracle::read_address(
        &mut evm,
        harness,
        Bytes::from(oracle::selector("currency0()").to_vec()),
        2_000_000,
    )
    .expect("read currency0");
    let cur1 = oracle::read_address(
        &mut evm,
        harness,
        Bytes::from(oracle::selector("currency1()").to_vec()),
        2_000_000,
    )
    .expect("read currency1");
    let manager = oracle::read_address(
        &mut evm,
        harness,
        Bytes::from(oracle::selector("manager()").to_vec()),
        2_000_000,
    )
    .expect("read manager");

    let pool_key = V4PoolKey {
        currency0: cur0,
        currency1: cur1,
        fee,
        tick_spacing,
        hooks: Address::ZERO,
    };
    seed_v4_pool_storage(&mut evm, manager, &pool_key, state, fee);

    // Drive the swap at the given gas budget.
    let data = Bytes::from(encode_v4_swap_call(
        zero_for_one,
        amount_specified,
        sqrt_price_limit,
    ));
    let (verdict, gas_used) = oracle::transact_with_gas(
        &mut evm,
        TxSpec::Call {
            to: harness,
            data,
            gas,
        },
    );

    let delta = match &verdict {
        Verdict::Accepted {
            output: oracle::Output::Call(b),
            ..
        } => decode_balance_delta(b.as_ref()),
        _ => (U256::ZERO, U256::ZERO),
    };
    V4RealSwap {
        verdict,
        delta,
        gas_used,
    }
}
