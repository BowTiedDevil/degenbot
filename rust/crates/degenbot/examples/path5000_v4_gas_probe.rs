//! Path-5000 V4-leg seeded-state gas probe (block 25704509).
//!
//! The decisive follow-up to `path5000_executor_payload`: that harness runs the
//! real `cmd_executor` against an **EmptyDB** (no pool code seeded), so a call
//! into the V4 PoolManager hits a non-contract account and the leg cannot be
//! reproduced. This harness instead deploys the **real v4-core `PoolManager`**
//! (via the committed `V4SwapOracleHarness` unlocker wrapper), seeds the
//! path-5000 V4 pool's storage slot-for-slot from the fixture (pool_id
//! `0x929b9b09…`, the single tracked band `[-257352, 35067]`, liquidity
//! `3186539294357237543`, protocol_fee `102425`, fee `100`, spacing `1`), and
//! drives the **recorded V4 swap** — `v4_input=15351327867212777`, `zfo=false`
//! (sell MATIC/currency1 buy UNI/currency0) — through `unlock`→`swap`→settle.
//!
//! ## What it answers
//!
//! The live halt was `[sim-fail] path=5000 … bucket=empty` with the deepest
//! PoolManager frame spending ~4.46M gas under the hard-coded
//! `INITIAL_EXECUTE_GAS = 5_000_000` ceiling (`degenbot-backrun-strategy/
//! simulator.rs`). The question is whether that halt is (a) a **genuine
//! liquidity / range-exhaustion verdict** on the real pool (v4_simulate_swap
//! fills the band 98% and any excess tips past tick 35067 into zero liquidity)
//! or (b) an **artifact of the 5M ceiling truncating real execution**. This
//! probe re-runs the SAME swap on the SAME seeded on-chain state at 5M vs 30M
//! and compares the verdict + BalanceDelta to the recorded solver output
//! (`v4_predicted_output=460882096151249`).
//!
//! If the swap fills at 30M but not 5M, the 5M ceiling is causal and raising it
//! un-halts the path. If it reverts at both, the halt tracks a real on-chain
//! infeasibility (the engine's solver/twin agrees: see the byte-exact solver
//! fixture, which computes `v4_hop_output == 460882096151249` from the
//! reconstructed state). Exit 0 = probe ran; the verdicts are printed for
//! inspection.
//!
//! Run standalone:
//! ```text
//! cargo run -p degenbot --example path5000_v4_gas_probe
//! ```
//! Optional: `FIXTURE_PATH=…` to point at a different capture, and
//! `GAS_5M=… GAS_30M=…` to override the two budgets (defaults 5_000_000 /
//! 30_000_000).

#![allow(clippy::doc_markdown, clippy::too_many_lines)]
#![allow(clippy::cast_possible_wrap, clippy::match_same_arms)]

use std::collections::HashSet;
use std::path::PathBuf;

use alloy::primitives::{aliases::I256, Address, Bytes, U160, U256};
use degenbot::investigation::{build_v4_state, PathFixture};
use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO;
use degenbot_pools::v4_state::{v4_simulate_swap, V4PoolKey, V4PoolState};
use degenbot_pools::v4_storage_slots::{
    encode_v4_liquidity_slot, encode_v4_slot0, encode_v4_tick_info_slot, v4_liquidity_slot,
    v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot, v4_tick_bitmap_word_slot,
    v4_tick_mapping_slot, V4Slot0Parts,
};
use degenbot_simulation::oracle::{self, TxSpec, Verdict};
use revm::context_interface::result::Output;

const DEFAULT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
);

/// Resolve the tier3-oracle artifacts root from the repo.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Load a foundry-shaped harness artifact's creation bytecode.
fn load_creation_bytecode(file: &str, contract: &str) -> Bytes {
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

/// ABI-encode the V2SwapOracleHarness constructor args `(uint24 fee, int24
/// tickSpacing)` for the V4 harness.
fn harness_constructor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
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

/// Seed the V4 `Pool.State` storage for the single defined pool at the manager,
/// from a reconstructed `V4PoolState`. The pool key's currencies are the
/// harness's deployed mock token addresses (read back via getters) so the
/// derived poolId matches the one the harness's `swap` will actually touch.
fn seed_v4_pool_storage(
    evm: &mut oracle::FixtureEvm,
    manager: Address,
    pool_key: &V4PoolKey,
    state: &V4PoolState,
    fee: u32,
) {
    let pool_id = v4_pool_id(pool_key);
    let base = v4_pool_state_base_slot(pool_id);
    println!("    seeding V4 pool, pool_id={pool_id:#x} base={base:#x}");

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
    let mut word_positions: HashSet<i16> = HashSet::new();
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
fn encode_v4_swap_call(
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
fn decode_balance_delta(out: &[u8]) -> (U256, U256) {
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

fn main() {
    let fixture_path =
        std::env::var("FIXTURE_PATH").unwrap_or_else(|_| DEFAULT_FIXTURE.to_string());
    let gas_5m = std::env::var("GAS_5M")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5_000_000u64);
    let gas_30m = std::env::var("GAS_30M")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000_000u64);

    let fx = PathFixture::load(&fixture_path).unwrap_or_else(|e| panic!("{e}"));
    let rec = &fx.recorded_solve;

    println!("=== path-5000 V4 seeded-state gas probe ===");
    println!(
        "block={:?} V4 pool_id={} fee={} spacing={} zfo={}",
        fx.target_block,
        fx.pools["v4"].pool_id.as_deref().unwrap_or(""),
        fx.pools["v4"].fee_currency0.unwrap(),
        fx.pools["v4"].tick_spacing.unwrap(),
        rec.v4_zero_for_one.unwrap(),
    );
    println!(
        "recorded: v4_input={} v4_predicted_output={} onchain={}",
        rec.v4_input.unwrap(),
        rec.v4_predicted_output.unwrap(),
        rec.v4_onchain.as_deref().unwrap_or("")
    );

    // Reconstruct the V4 state the solver/engine used.
    let state = build_v4_state(&fx.pools["v4"]);
    let ticks: Vec<i32> = fx.pools["v4"]
        .tick_data
        .keys()
        .map(|t| t.parse().unwrap())
        .collect();
    let tmin = *ticks.iter().min().unwrap();
    let tmax = *ticks.iter().max().unwrap();
    let cur_tick = fx.pools["v4"].tick.unwrap();
    println!(
        "V4 tracked band: {} ticks [{tmin},{tmax}] current={cur_tick} headroom-above={} liq={} protocol_fee={}",
        ticks.len(),
        tmax - cur_tick,
        state.liquidity,
        state.protocol_fee,
    );

    // The recorded V4 input (exact-in => negative amount), selling currency1
    // (MATIC) for currency0 (UNI), zfo=false.
    let v4_input = rec.v4_input.unwrap().0;
    // Optional solver-clamp: reduce the input by `CLAMP_INPUT` wei so the swap
    // is fed exactly (or below) the pool's max-convertible amount — proving the
    // leftover-input hypothesis: when input == capacity, the exact-in loop
    // terminates on amountRemaining==0 at the band boundary (no march, ~215k
    // gas) even with a MAX price limit.
    let clamp = std::env::var("CLAMP_INPUT")
        .ok()
        .and_then(|s| s.parse::<u128>().ok())
        .unwrap_or(0);
    let v4_input = v4_input.saturating_sub(U256::from(clamp));
    let amount_specified = I256::ZERO
        .checked_sub(I256::try_from(v4_input).expect("v4_input fits i256"))
        .expect("no underflow");
    let zfo = false;

    // Price limit mode: env-driven so we can compare band-top-bound vs a MAX
    // (uncapped) limit, which is what the live executor most plausibly passes
    // to PoolManager.swap (MIN/MAX_SQRT_PRICE per exploration-no-profit-crash.md
    // L308) — a MAX limit forces the swap loop to walk the tick-bitmap
    // word-by-word in the price direction (an SLOAD per word) rather than
    // stopping at the tracked band top.
    let sqrt_price_limit: U160 = if std::env::var("EXECUTOR_LIMIT").is_ok() {
        // EXACTLY what the live executor passes for zfo=false: `MAX_SQRT_RATIO - 1`
        // (the extreme bound — an unbounded price march in the buy direction).
        MAX_SQRT_RATIO - U160::from(1u64)
    } else if let Ok(lt) = std::env::var("LIMIT_TICK") {
        // Explicit numeric price-limit tick (int). Sweep this to characterize
        // the swap-loop's gas-vs-price-distance walk.
        let t = lt.parse::<i32>().expect("LIMIT_TICK must be an int");
        U160::from(get_sqrt_ratio_at_tick_internal(t).expect("limit sqrt"))
    } else if std::env::var("CAP_AT_BAND_TOP").is_ok() {
        // Decisive probe #1: cap the price limit AT the band top (tick 35067)
        // — where on-chain liquidity goes to zero.
        U160::from(get_sqrt_ratio_at_tick_internal(tmax).expect("limit sqrt"))
    } else if std::env::var("BAND_TOP_PLUS").is_ok() {
        // Decisive probe #2: cap the price limit just PAST the band top into
        // the first empty word, to see the loop walk a few empty words.
        U160::from(get_sqrt_ratio_at_tick_internal(tmax + 5_000).expect("limit sqrt"))
    } else {
        // Default: past the band with margin, well inside u160.
        U160::from(get_sqrt_ratio_at_tick_internal((tmax + 5000).min(800_000)).expect("limit sqrt"))
    };

    // Rust twin (what v4_simulate_swap says from the reconstructed state).
    let sim = v4_simulate_swap(
        &state,
        fx.pools["v4"].fee_currency0.unwrap(),
        fx.pools["v4"].tick_spacing.unwrap(),
        zfo,
        amount_specified,
        U256::from(sqrt_price_limit),
    );
    println!(
        "v4_simulate_swap -> {:?} (recorded solver output={})",
        sim,
        rec.v4_predicted_output.unwrap()
    );

    // Build a fresh EVM, deploy the real PoolManager via the harness wrapper.
    let probe = |gas: u64| -> (Verdict, (U256, U256), u64) {
        let mut evm = oracle::new_fixture_evm();
        oracle::set_disable_nonce_check(&mut evm, true);
        oracle::set_code_size_limits(&mut evm, None);
        // Allow tx gas above revm's default block-gas-limit + EIP-7825 caps
        // (both 16,777,216): with the 5M ceiling the harness only needs the 5M
        // tx to pass, but for the raised-budget probe the tx would otherwise be
        // rejected by tx-vs-block / EIP-7825 validation.
        oracle::set_block_gas_limit(&mut evm, std::cmp::max(gas, 30_000_000));
        oracle::set_tx_gas_limit_cap(&mut evm, u64::MAX);

        // Deploy V4SwapOracleHarness (constructs the canonical PoolManager).
        let mut init_code =
            load_creation_bytecode("V4SwapOracleHarness.sol", "V4SwapOracleHarness").to_vec();
        init_code.extend_from_slice(&harness_constructor_args(
            fx.pools["v4"].fee_currency0.unwrap(),
            fx.pools["v4"].tick_spacing.unwrap(),
        ));
        let harness = match oracle::deploy(&mut evm, Bytes::from(init_code), 16_000_000) {
            Ok(a) => a,
            Err(e) => {
                return (
                    Verdict::Halted(format!("harness deploy failed: {e}")),
                    (U256::ZERO, U256::ZERO),
                    0,
                );
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
            fee: fx.pools["v4"].fee_currency0.unwrap(),
            tick_spacing: fx.pools["v4"].tick_spacing.unwrap(),
            hooks: Address::ZERO,
        };
        seed_v4_pool_storage(
            &mut evm,
            manager,
            &pool_key,
            &state,
            fx.pools["v4"].fee_currency0.unwrap(),
        );

        // Drive the recorded swap at the given gas budget.
        let data = Bytes::from(encode_v4_swap_call(zfo, amount_specified, sqrt_price_limit));
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
                output: Output::Call(b),
                ..
            } => decode_balance_delta(b.as_ref()),
            _ => (U256::ZERO, U256::ZERO),
        };
        (verdict, delta, gas_used)
    };

    for (label, gas) in [("5M", gas_5m), ("30M", gas_30m)] {
        let (verdict, (am0, am1), gas_used) = probe(gas);
        println!("--- swap @ {label} gas ({gas}) ---");
        match &verdict {
            Verdict::Accepted { .. } => {
                println!(
                    "  ACCEPTED -> BalanceDelta amount0(UNI)={} amount1(MATIC)={} | recorded V4 output={} | gas_used={}",
                    am0, am1, rec.v4_predicted_output.unwrap(), gas_used
                );
            }
            Verdict::Reverted(r) => {
                println!("  REVERTED (gas_used={gas_used}) -> {r:?}");
                if let Some(msg) = oracle::decode_error_string(r.as_ref()) {
                    println!("    decoded: {msg}");
                }
            }
            Verdict::Halted(h) => {
                println!("  HALTED (gas_used={gas_used}) -> {h}");
            }
        }
    }

    println!("\ndone.");
}
