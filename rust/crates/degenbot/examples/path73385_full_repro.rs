#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::panic_in_result_fn
)]
//! Path-73385 full production-shaped reproduction (block 25706469).
//!
//! Re-runs the EXACT failing V3→V4→V3 path (the recurring `bucket=empty
//! kind=halt gas=6269` depth-8 empty-calldata USDT halt) through the
//! PRODUCTION layered-DB simulator (`BlockSimHandle` → `AlloyDB` →
//! `WrapDatabaseAsync` → `BotStateDb` → `WarmCodeCache` → `CacheDB`) against
//! the live archive node at the solve block, with `DEGENBOT_DUMP_CALL_TRACE=1`
//! so the full per-frame `CallTraceInspector` render of `execute()` is dumped.
//!
//! Unlike the `path5000_executor_payload` probe (inject-only-executor over an
//! EmptyDB), this harness does NOT hand-inject pool bytecode: the layered DB
//! cold-miss fetches the REAL `UniswapV3Pool` (0xE0554a47 USDC/WETH,
//! 0xc7bBeC68 WETH/USDT), the REAL v4-core `PoolManager`, and the REAL token
//! code (USDC/WETH/USDT) from the archive node at block 25706469 — so the
//! nested swap + the final USDT settle execute exactly as the live bot saw.
//!
//! Run (needs the archive/full RPC):
//! ```text
//! DEGENBOT_DUMP_CALL_TRACE=1 \
//!   cargo run -p degenbot --example path73385_full_repro
//! ```
//! RPC comes from `DEGENBOT_RPC_HTTP_CHAINID_1` (else `CHAIN_1_HTTP`, else
//! `http://host.containers.internal:8545`).

#![allow(clippy::doc_markdown, clippy::too_many_lines)]
#![allow(clippy::cast_possible_wrap)] // run-once investigation probe

use alloy::primitives::{utils::keccak256, Address, Bytes, B256, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::degenbot_backrun_strategy::{
    simulate_path_on_evm, FailBuckets, SimulateContext, SimulatePath,
};
use degenbot::degenbot_executor::composers::{
    EncodeOptions, HopInfo, PathInfo, V3HopInfo, V4HopInfo,
};
use degenbot::degenbot_executor::compute_simulation_warmup_slots;
use degenbot::degenbot_rpc::provider::AlloyProvider;
use degenbot::degenbot_simulation::sim::evm::BlockSimHandle;
use degenbot::degenbot_simulation::{SimulationOverrideParams, WarmCodeCacheInner};

const EXECUTOR: Address = alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const WETH: Address = alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const PM: Address = alloy::primitives::address!("000000000004444c5dc75cB358380D2e3dE08A90");
const MULTICALL3: Address = alloy::primitives::address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
const OWNER: Address = alloy::primitives::address!("9c56a29c7231974c269e24f9fb3c29203039089e");

const SOLVE_BLOCK: u64 = 25_706_469;
const BLOCK_TS: u64 = 1_786_146_455; // `cast block 25706469 --field timestamp`
const BASE_FEE_NEXT: u128 = 1_000_000_000;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path73385_v4_block25706469.json"
);
const EXECUTOR_RUNTIME: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tier3-oracle/artifacts/executor/cmd_executor.runtime.hex"
);

fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(rel).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let s: String = raw.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.strip_prefix("0x").unwrap_or(&s);
    alloy::primitives::hex::decode(s).expect("hex decodes")
}

fn parse_addr(s: &str) -> Address {
    s.parse().expect("address parses")
}

/// Build the executor runtime bytecode by appending the 5×32-byte immutable
/// tail to the committed `[code_section][CBOR]` runtime (same construction as
/// `path5000_executor_payload` / `contracts/recompile.py`).
fn build_executor_runtime(owner: Address, injected: Address) -> Vec<u8> {
    let mut code = load_hex(EXECUTOR_RUNTIME);
    let code_len = code.len();
    let pad32 = |a: Address| -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..32].copy_from_slice(a.as_slice());
        w
    };
    let weth_delta: B256 = keccak256([pad32(injected).as_slice(), pad32(WETH).as_slice()].concat());
    let native_delta: B256 =
        keccak256([pad32(injected).as_slice(), pad32(Address::ZERO).as_slice()].concat());
    code.extend_from_slice(&pad32(owner));
    code.extend_from_slice(&pad32(WETH));
    code.extend_from_slice(&pad32(PM));
    code.extend_from_slice(weth_delta.as_slice());
    code.extend_from_slice(native_delta.as_slice());
    println!(
        "  executor runtime: {code_len} bytes + 160B immutable tail (owner={owner} injected={injected})"
    );
    code
}

/// The V3-V4-V3 path 73385 (unchanged from the live recording).
fn build_path(path_id: u64) -> SimulatePath {
    let (optimal_input, hop_outputs) = (
        44_421_383_036_608_956u128,
        vec![85_060_245u128, 85_097_884, 44_421_879_564_949_974],
    );
    // consumed_inputs = [optimal_input] + hop_outputs[..n-1], THEN the solver's
    // pool-state reconciliation (`clamp_cl_hop_capacity` OUTPUT clamp) caps
    // consumed_inputs[2] (= the V4->v3c forward) at the V4 pool's byte-exact
    // twin output (85097881), so the composer's take (which now uses
    // consumed_inputs[2]) can never over-take the pool's actual USDT output.
    let mut consumed_inputs = vec![optimal_input, hop_outputs[0], hop_outputs[1]];
    consumed_inputs[2] = 85_097_881; // v4_simulate_swap actual output (twin)
    let path_info = PathInfo::new(vec![
        HopInfo::V3(V3HopInfo {
            pool_address: parse_addr("0xE0554a476A092703abdB3Ef35c80e0D76d32939F"),
            token0_address: parse_addr("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
            token1_address: WETH,
            fee: 100,
            zfo: false,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x8aa4e11cbdf30eedc92100f4c8a31ff748e201d44712cc8c90d189edaa8e4e47"
                .to_string(),
            currency0_address: parse_addr("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
            currency1_address: parse_addr("0xdAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
            fee: 10,
            tick_spacing: 1,
            hook_address: Address::ZERO,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: parse_addr("0xc7bBeC68d12a0d1830360F8Ec58fA599bA1b0e9b"),
            token0_address: WETH,
            token1_address: parse_addr("0xdAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
            fee: 100,
            zfo: false,
        }),
    ]);
    SimulatePath {
        path_id,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        path_info,
        solve_block: SOLVE_BLOCK,
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..Default::default()
        },
        state_nonces: vec![],
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Force the full call-trace dump on the execute() failure.
    std::env::set_var("DEGENBOT_DUMP_CALL_TRACE", "1");

    let http = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("CHAIN_1_HTTP"))
        .unwrap_or_else(|_| "http://host.containers.internal:8545".to_string());
    println!("=== path-73385 full production-shaped repro (block {SOLVE_BLOCK}) ===");
    println!("RPC: {http}");

    let provider = AlloyProvider::new(&http, 5).await?;
    let runtime_bytecode = Bytes::copy_from_slice(&build_executor_runtime(OWNER, EXECUTOR));

    let bot_state = BotState::new();
    let warmup = compute_simulation_warmup_slots(EXECUTOR, WETH, PM);
    let overrides = SimulationOverrideParams {
        owner: OWNER,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: runtime_bytecode.clone(),
        warmup,
        weth_address: WETH,
        pool_manager_address: PM,
    };
    let warm_cache = WarmCodeCacheInner::shared_default();

    let mut handle = BlockSimHandle::build(
        &provider,
        BASE_FEE_NEXT,
        SOLVE_BLOCK,
        BLOCK_TS,
        &overrides,
        &bot_state,
        &warm_cache,
    )
    .expect("BlockSimHandle build (needs multi-thread tokio runtime)");

    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: OWNER,
        executor_address: EXECUTOR,
        weth_address: WETH,
        pool_manager_address: PM,
        multicall3_address: MULTICALL3,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode,
        warmup,
        base_fee_next: BASE_FEE_NEXT,
        current_block: SOLVE_BLOCK,
        block_timestamp: BLOCK_TS,
        block_priority_fees: None,
    };

    // Solver-side analysis: reconstruct each CL pool and run the simulation
    // twins to see the ACTUAL convertible amounts vs the solver's predictions.
    {
        use degenbot::investigation::build_v4_state;
        use degenbot::investigation::reconstruct::build_v3_state as bv3;
        println!("--- solver-side CL analysis ---");
        let fx2 = degenbot::investigation::PathFixture::load(FIXTURE_PATH)
            .unwrap_or_else(|e| panic!("{e}"));
        // V4 (hop1): sell USDC/currency0 (zfo=true), exact-in 85060245.
        let v4 = build_v4_state(&fx2.pools["v4"]);
        let v4_amount = I256::ZERO
            .checked_sub(I256::try_from(85_060_245u128).unwrap())
            .unwrap();
        let v4_sim = degenbot::degenbot_pools::v4_state::v4_simulate_swap(
            &v4,
            10u32,
            1i32,
            true,
            v4_amount,
            U256::from(degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO)
                .checked_add(U256::from(1u64))
                .unwrap(),
        );
        println!("V4(v)\t sim={v4_sim:?}");
        // V3c (hop2): sell USDT/token1 (zfo=false), exact-in 85097884 (predicted
        // forward) — the solver's requested consumed_inputs[2].
        let v3c = bv3(&fx2.pools["v3_2"]);
        let req = I256::try_from(85_097_884u128).unwrap();
        let limit = U256::from(
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(820_000)
                .expect("limit"),
        );
        let v3c_sim = degenbot::degenbot_pools::v3_state::v3_simulate_swap(
            &v3c, 100u32, 1i32, false, req, limit,
        );
        if let Ok(o) = &v3c_sim {
            let clamp = o.exact_input_clamp_bound(U256::from(85_097_884u128), U256::from(1));
            println!("V3c\t sim={o:?}\nV3c\t consumed=[req=85097884] clamp={clamp:?} (None = not over-fed)");
        } else {
            println!("V3c\t sim Err={v3c_sim:?}");
        }
    }

    let path = build_path(73385);
    let mut buckets = FailBuckets::new();
    println!("--- simulating path 73385 (V3-V4-V3 USDC->...->USDT) ---");
    let evm = handle.evm_mut();
    let result = simulate_path_on_evm(evm, &ctx, &path, &mut buckets)
        .map_err(|e| format!("sim err: {e:?}"))?;

    match result {
        Some(r) => {
            println!(
                "RESULT: SUCCESS gross_profit={} net_profit={} gas_used={} captured_swaps={}",
                r.gross_profit,
                r.net_profit,
                r.gas_used,
                r.captured_swaps.len()
            );
        }
        None => {
            println!("RESULT: REVERT/HALT — see the [sim-trace] dump above for the failing frame.");
        }
    }
    println!("buckets: {:?}", buckets.buckets());

    let _ = FIXTURE_PATH; // keep the const referenced (documented fixture location)
    Ok(())
}
