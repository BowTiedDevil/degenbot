//! Path-13308 (V3-V4-V3) snapshot: Möbius solver vs on-chain reality.
//!
//! Loads `tests/fixtures/path13308_v3v4v3_block25664704.json` (the exact pool
//! states for block 25664704 captured by `scripts/capture_path_13308_fixture.py`
//! from the DB stale snapshot + verified-current liquidity maps + on-chain
//! scalar reads), reconstructs the three pool states, builds the V3-V4-V3 path,
//! runs the production Möbius solver (`ArbitrageEngine::register_and_solve_path`),
//! and reports `optimal_input`/`hop_outputs` against the RECORDED solve from the
//! trapping [sim-diag] and the ON-CHAIN captured swap amounts.
//!
//! Insight for the `no-profit` crash: the solver predicted a +1.19e11 wei
//! WETH-cycle profit, but executing the same plan on-chain nets -2.58e12 wei
//! (gross). Reproducing the solver output here isolates whether the sign flip is
//! a solver-error vs a genuine sub-threshold unprofitable candidate.

use std::collections::HashMap;

use alloy::primitives::{Address, B256, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot::RegisterV3PoolParams;
use degenbot::RegisterV4PoolParams;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::v4_state::V4PoolKey;
use degenbot_pools::TickInfo;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path13308_v3v4v3_block25664704.json"
);

#[derive(serde::Deserialize)]
struct Fixture {
    recorded_solve: RecordedSolve,
    pools: Pools,
    path: Vec<PathHop>,
}
#[derive(serde::Deserialize)]
struct RecordedSolve {
    optimal_input: u128,
    hop_outputs: Vec<u128>,
}
#[derive(serde::Deserialize)]
struct Pools {
    v3_0: PoolData,
    v4: PoolData,
    v3_2: PoolData,
}
#[derive(serde::Deserialize)]
struct PathHop {
    hop: usize,
    pool: String,
    zero_for_one: bool,
}
#[derive(serde::Deserialize)]
struct PoolData {
    address: Option<String>,
    token0: Option<String>,
    token1: Option<String>,
    pool_manager: Option<String>,
    pool_id: Option<String>,
    currency0: Option<String>,
    currency1: Option<String>,
    tick_spacing: Option<i32>,
    fee_token0: Option<u32>,
    fee_currency0: Option<u32>,
    tick: Option<i32>,
    sqrt_price_x96: Option<String>,
    liquidity: Option<String>,
    protocol_fee: Option<u32>,
    liquidity_update_block: Option<u64>,
    tick_data: HashMap<String, TickJson>,
}
#[derive(serde::Deserialize)]
struct TickJson {
    liquidity_net: String,
    liquidity_gross: String,
}

fn tick_map(data: &HashMap<String, TickJson>) -> HashMap<i32, TickInfo> {
    data.iter()
        .map(|(t, v)| {
            (
                t.parse::<i32>().unwrap(),
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(
                        v.liquidity_gross.parse::<u128>().unwrap(),
                    ),
                    liquidity_net: I256::try_from(v.liquidity_net.parse::<i128>().unwrap())
                        .unwrap(),
                    block: 0,
                },
            )
        })
        .collect()
}

fn parse_addr(s: &str) -> Address {
    s.parse().unwrap()
}

fn register_v3(core: &mut BotState, p: &PoolData) -> u64 {
    let sqrt_override: Option<U256> = std::env::var("FIXTURE_V3_2_SQRT")
        .ok()
        .map(|s| s.parse().unwrap());
    let sqrt_price_x96 =
        sqrt_override.unwrap_or_else(|| p.sqrt_price_x96.as_ref().unwrap().parse().unwrap());
    core.register_v3_pool(&RegisterV3PoolParams {
        address: parse_addr(p.address.as_ref().unwrap()),
        token0: parse_addr(p.token0.as_ref().unwrap()),
        token1: parse_addr(p.token1.as_ref().unwrap()),
        fee: p.fee_token0.unwrap(),
        tick_spacing: p.tick_spacing.unwrap(),
        factory: Address::ZERO,
        sqrt_price_x96,
        liquidity: p.liquidity.as_ref().unwrap().parse().unwrap(),
        tick: p.tick.unwrap(),
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.as_ref().copied().unwrap(),
        coverage: PoolTickCoverage::Tracked,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        ..Default::default()
    })
    .expect("register v3")
}

fn main() {
    let text = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|e| panic!("read fixture {FIXTURE_PATH}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");

    let engine = ArbitrageEngine::new();
    let pid0 = register_v3(&mut engine.core.write(), &fx.pools.v3_0);
    let pid2 = register_v3(&mut engine.core.write(), &fx.pools.v3_2);

    let pv = &fx.pools.v4;
    let pid_hex = pv.pool_id.as_ref().unwrap();
    let pool_id: V4PoolId = pid_hex
        .trim_start_matches("0x")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect::<Vec<u8>>()
        .try_into()
        .unwrap();
    let pool_key = V4PoolKey {
        currency0: parse_addr(pv.currency0.as_ref().unwrap()),
        currency1: parse_addr(pv.currency1.as_ref().unwrap()),
        fee: pv.fee_currency0.unwrap(),
        tick_spacing: pv.tick_spacing.unwrap(),
        hooks: Address::ZERO,
    };
    let v4_fee_override: Option<u32> = std::env::var("FIXTURE_V4_PROTO_FEE")
        .ok()
        .map(|s| s.parse().unwrap());
    println!("v4 protocol_fee: {v4_fee_override:?}");
    let v4id = engine
        .core
        .write()
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: parse_addr(pv.pool_manager.as_ref().unwrap()),
            pool_id,
            pool_key,
            hook_flags: 0,
            protocol_fee: v4_fee_override.unwrap_or(pv.protocol_fee.unwrap_or(0)),
            sqrt_price_x96: pv.sqrt_price_x96.as_ref().unwrap().parse().unwrap(),
            liquidity: pv.liquidity.as_ref().unwrap().parse().unwrap(),
            tick: pv.tick.unwrap(),
            tick_data: tick_map(&pv.tick_data),
            update_block: pv.liquidity_update_block.as_ref().copied().unwrap(),
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .map_err(|e| format!("register_v4: {e:?}"))
        .expect("register v4");

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v3_0" => pid0,
            "v3_2" => pid2,
            "v4" => v4id,
            o => panic!("unknown pool {o}"),
        };
        by_idx.insert(h.hop, (pid, h.zero_for_one));
    }
    let hops = (0..fx.path.len())
        .map(|i| {
            let (pid, zfo) = by_idx[&i];
            PoolHop {
                pool_id: pid,
                zero_for_one: zfo,
            }
        })
        .collect::<Vec<_>>();

    let mut engine = engine;
    let path_id = engine.register_and_solve_path(hops).expect("solve path");

    let (results, _) = engine.latest_results();
    let recorded_optimal: U256 = U256::from(fx.recorded_solve.optimal_input);
    let recorded_hops: Vec<U256> = fx
        .recorded_solve
        .hop_outputs
        .iter()
        .map(|&s| U256::from(s))
        .collect();

    match results.get(&path_id) {
        Some(r) => {
            println!("=== solver result (path {path_id}) ===");
            println!("  optimal_input (recomputed): {v}", v = r.optimal_input);
            println!("  optimal_input (recorded):   {recorded_optimal}");
            println!("  hop_outputs (recomputed):   {:?}", r.hop_outputs);
            println!("  hop_outputs (recorded):     {recorded_hops:?}");
            println!("  profit (recomputed):        {v}", v = r.profit);
            let matches = r.optimal_input == recorded_optimal && r.hop_outputs == recorded_hops;
            let verdict = if matches {
                "MATCHES recorded solve"
            } else {
                "DIFFERS from recorded solve"
            };
            println!("  => {verdict}");
        }
        None => println!("solver returned None for path {path_id} (no profitable input)"),
    }
}
