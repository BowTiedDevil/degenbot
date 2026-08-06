//! Path-13827 V3-V3-V3 solver fixture (thin-tick-spacing over-prediction family).
//!
//! Loads `tests/fixtures/path13827_v3v3v3_block25696329.json` — the exact
//! V3-V3-V3 pool states at the failure block, captured by
//! `scripts/capture_path13822_v3v3v3_fixture.py` from the DB liquidity snapshot
//! (hop1 verified current at TARGET) + on-chain scalar reads — reconstructs the
//! three V3 pools, runs the production Möbius solver, and reports the solver's
//! hop-1 output against the byte-exact `v3_simulate_swap` oracle and the
//! recorded on-chain `[sim-revert-swap]` actual.
//!
//! The bug (same family as UO3JM4 but the failing hop is a **V3** DAI/USDC
//! `tick_spacing=1` pool `0x5777d92f…`, not V4): the solver's V3-hop output
//! `hop_outputs[1]` over-predicts `v3_simulate_swap` by 1 wei →
//! `V3_TAKE(predicted)` overdrafts → "IIA" revert in sim → fail-fast. This harp
//! asserts the fix target: **solver hop-1 output == `v3_simulate_swap` ==
//! recorded on-chain actual — all three byte-equal**, alongside the recorded
//! [`sim-revert-swap`] `hop=1 matched=true` acceptance.
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::struct_field_names,
    clippy::similar_names
)]

use std::collections::HashMap;

use alloy::primitives::{Address, B256, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot::RegisterV3PoolParams;
use degenbot_pools::v3_state::{v3_simulate_swap, PoolTickCoverage, V3PoolState};
use degenbot_pools::TickInfo;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path13827_v3v3v3_block25696329.json"
);

#[derive(serde::Deserialize)]
struct Fixture {
    target_block: u64,
    recorded_solve: RecordedSolve,
    pools: Pools,
    path: Vec<PathHop>,
}
#[derive(serde::Deserialize)]
struct RecordedSolve {
    optimal_input: u128,
    hop_outputs: Vec<u128>,
    hop1_actual: u128,
}
#[derive(serde::Deserialize)]
struct Pools {
    v3_0: PoolData,
    v3_1: PoolData,
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
    tick_spacing: Option<i32>,
    fee_token0: Option<u32>,
    tick: Option<i32>,
    sqrt_price_x96: Option<String>,
    liquidity: Option<String>,
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

fn build_v3_state(p: &PoolData) -> V3PoolState {
    let (_identity, state) = V3PoolState::from_params(
        RegisterV3PoolParams {
            address: parse_addr(p.address.as_ref().unwrap()),
            token0: parse_addr(p.token0.as_ref().unwrap()),
            token1: parse_addr(p.token1.as_ref().unwrap()),
            fee: p.fee_token0.unwrap(),
            tick_spacing: p.tick_spacing.unwrap(),
            factory: Address::ZERO,
            sqrt_price_x96: p.sqrt_price_x96.as_ref().unwrap().parse().unwrap(),
            liquidity: p.liquidity.as_ref().unwrap().parse().unwrap(),
            tick: p.tick.unwrap(),
            tick_data: tick_map(&p.tick_data),
            update_block: p.liquidity_update_block.as_ref().copied().unwrap(),
            coverage: PoolTickCoverage::Tracked,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
            ..Default::default()
        },
        8,
    );
    state
}

fn register_v3(core: &mut BotState, p: &PoolData) -> u64 {
    core.register_v3_pool(&RegisterV3PoolParams {
        address: parse_addr(p.address.as_ref().unwrap()),
        token0: parse_addr(p.token0.as_ref().unwrap()),
        token1: parse_addr(p.token1.as_ref().unwrap()),
        fee: p.fee_token0.unwrap(),
        tick_spacing: p.tick_spacing.unwrap(),
        factory: Address::ZERO,
        sqrt_price_x96: p.sqrt_price_x96.as_ref().unwrap().parse().unwrap(),
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
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let text = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("read fixture {fixture_path}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");

    let hop1_zfo = fx.path.iter().find(|h| h.hop == 1).unwrap().zero_for_one;
    let hop1_fee = fx.pools.v3_1.fee_token0.unwrap();
    let hop1_ts = fx.pools.v3_1.tick_spacing.unwrap();
    println!(
        "fixture: block={} hop1={} (fee={hop1_fee} spacing={hop1_ts} zfo={hop1_zfo}) recorded_optimal={}",
        fx.target_block, fx.pools.v3_1.address.as_ref().unwrap(), fx.recorded_solve.optimal_input
    );
    let recorded_actual: U256 = U256::from(fx.recorded_solve.hop1_actual);
    let recorded_predicted: U256 = U256::from(fx.recorded_solve.hop_outputs[1]);

    // 1) Reconstruct hop1 (the failing DAI/USDC pool) and run the byte-exact
    //    v3_simulate_swap oracle at the RECORDED hop1 input (= hop_outputs[0],
    //    the prior hop's output that was fed into the V3 pool).
    let v3_1_state = build_v3_state(&fx.pools.v3_1);
    let hop1_input: U256 = U256::from(fx.recorded_solve.hop_outputs[0]);
    let sim = match v3_simulate_swap(
        &v3_1_state,
        hop1_fee,
        hop1_ts,
        hop1_zfo,
        I256::try_from(hop1_input).expect("hop1 input fits i256"),
        V3PoolState::default_sqrt_price_limit(hop1_zfo),
    ) {
        Ok(s) => s,
        Err(e) => {
            println!("recorded-input v3_simulate_swap: {e:?}");
            std::process::exit(2);
        }
    };
    let sim_out = if hop1_zfo { sim.amount1 } else { sim.amount0 };
    println!("--- recorded-input oracle check (hop1) ---");
    println!(
        "recorded-input oracle: v3_simulate_swap @ recorded hop1 input {hop1_input} = {sim_out}"
    );
    println!(
        "  == recorded on-chain actual ({recorded_actual})?  {}",
        sim_out == recorded_actual
    );
    println!(
        "  != recorded solver predicted ({recorded_predicted})? {}",
        sim_out != recorded_predicted
    );

    // 2) The production Möbius solver over the reconstructed three V3 pools.
    let engine = ArbitrageEngine::new();
    let pid0 = register_v3(&mut engine.core().write(), &fx.pools.v3_0);
    let pid1 = register_v3(&mut engine.core().write(), &fx.pools.v3_1);
    let pid2 = register_v3(&mut engine.core().write(), &fx.pools.v3_2);

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v3_0" => pid0,
            "v3_1" => pid1,
            "v3_2" => pid2,
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
    match results.get(&path_id) {
        None => {
            println!("solver: path not profitable -> no solve (NO VERDICT on real full path).");
        }
        Some(r) => {
            let solver_in: U256 = r.consumed_inputs[1];
            let solver_out: U256 = r.hop_outputs[1];
            let sim2 = match v3_simulate_swap(
                &v3_1_state,
                hop1_fee,
                hop1_ts,
                hop1_zfo,
                I256::try_from(solver_in).expect("solver input fits i256"),
                V3PoolState::default_sqrt_price_limit(hop1_zfo),
            ) {
                Ok(s) => s,
                Err(e) => {
                    println!("v3_simulate_swap @ solver input: {e:?}");
                    std::process::exit(2);
                }
            };
            let sim2_out = if hop1_zfo { sim2.amount1 } else { sim2.amount0 };
            println!("solver hop-1 input  (consumed_inputs[1]): {solver_in}");
            println!("solver hop-1 output (hop_outputs[1]):     {solver_out}");
            println!("v3_simulate_swap @ same input:            {sim2_out}");
            println!("recorded on-chain actual:                 {recorded_actual}");

            let solver_ok = solver_out == sim2_out;
            println!("solver hop-1 == v3_simulate_swap:          {solver_ok}");
            if solver_ok {
                println!("=> VERDICT: PASS — solver hop-1 matches on-chain (matched=true).");
            } else {
                println!(
                    "=> VERDICT: FAIL (RED) — solver hop-1 output ({solver_out}) differs from v3_simulate_swap ({sim2_out}) at the same input: the thin-spacing V3 crossing over-prediction."
                );
                std::process::exit(1);
            }
        }
    }
}
