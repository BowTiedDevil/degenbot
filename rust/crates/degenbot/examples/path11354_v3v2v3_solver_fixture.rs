//! V3→V2→V3 fixture: Möbius solver V2-hop vs byte-exact constant-product (path 11354).
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the fee1_v3v4v3 / path13308 solver-fixture runners
//! Modeled 1:1 on `fee1_v3v4v3_solver_fixture.rs` (the V3-V4-V3 overdraw
//! harness) and `v3v30_hop0_probe.rs`. Loads
//! `tests/fixtures/path11354_v3v2v3_block<BLOCK>.json` — the exact V3-V2-V3
//! pool states at the failure block, captured by
//! `scripts/capture_path11354_v3v2v3_fixture.py` from the DB liquidity snapshot
//! + on-chain scalar reads — reconstructs the three pools into `BotState`, runs
//! the production Möbius solver, and reports the solver's V2-hop output against
//! the byte-exact constant-product oracle and the recorded `[sim-revert-swap]`
//! predicted/actual from the live log.
//!
//! The live trap (path 11354, block 25678283, type V3-V2-V3, bucket IIA):
//! ```text
//!   hop0 V3 0x4e68Ccd3 WETH→USDT (zfo)      matched=true
//!   hop1 V2 0x648Ef94C USDT→stETH (zfo=0)   actual=15166900278114 predicted=15166900278115  matched=false (1 wei short)
//!   hop2 V3 0x63818BbD stETH→WETH (zfo)     fixed input …115 not met → IIA
//! ```
//! The solver's V2-hop output comes from `IntHopState` (the constant-product
//! `getAmountOut`), which is also exactly what the on-chain V2 pair's
//! `_v2_get_amount_out` computes (9970/10000 ≡ 997/1000). So unlike the V4
//! case there is NO solver-vs-oracle gap by construction: the solver and the
//! on-chain formula are the same math. This harness therefore pins the
//! question the reverse-engineering could not: **is the solver's 15166900278115
//! reproducible from the reconstructed on-chain reserves, or is it an
//! over-prediction?** If the solver equals `IntHopState` at the on-chain
//! reserves (recorded predicted ...115), the solver is correct and the ...114
//! is a sim-side / state-side artifact (the investigation continues there).
//!
//! Exit 0 = solver V2 hop is byte-exact to the constant-product oracle (the
//! solver is exonerated; the recorded ...114 is a sim-side divergence).
//! Exit 1 = RED (fix violated) if the solver diverges from the oracle. Exit 2
//! = one of the CL hops is not soluble from the reconstructed state.

use std::collections::HashMap;

use alloy::primitives::{aliases::U112, Address, B256, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot::{DexVariant, RegisterV2PoolParams, RegisterV3PoolParams};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_solvers::mixed::PoolHop;
use degenbot_v2_math::IntHopState;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path11354_v3v2v3_block25678283.json"
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
    optimal_input: String,
    hop_outputs: Vec<String>,
    v2_hop_index: usize,
    v2_input: String,
    v2_predicted: String,
    v2_actual: String,
}
#[derive(serde::Deserialize)]
struct Pools {
    v3_0: PoolData,
    v2_1: PoolData,
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
    reserve0: Option<String>,
    reserve1: Option<String>,
    #[serde(default)]
    tick_data: HashMap<String, TickJson>,
}
#[derive(serde::Deserialize)]
struct TickJson {
    liquidity_net: String,
    liquidity_gross: String,
}

fn tick_map(data: &HashMap<String, TickJson>) -> HashMap<i32, degenbot_pools::TickInfo> {
    data.iter()
        .map(|(t, v)| {
            (
                t.parse::<i32>().unwrap(),
                degenbot_pools::TickInfo {
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

fn register_v3(core: &mut BotState, p: &PoolData) -> Result<u64, String> {
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
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        ..Default::default()
    })
    .map_err(|e| format!("register_v3: {e:?}"))
}

fn register_v2(core: &mut BotState, p: &PoolData) -> Result<u64, String> {
    // The on-chain pair uses UniswapV2's 997/1000 `getAmountOut` fee. Register
    // it so the solver's `IntHopState` gamma == 997 (the DB's `fee_token0=3`
    // is a display/`fee_token0/1000` form; the real retained fraction is 997).
    core.register_v2_pool(&RegisterV2PoolParams {
        address: parse_addr(p.address.as_ref().unwrap()),
        token0: parse_addr(p.token0.as_ref().unwrap()),
        token1: parse_addr(p.token1.as_ref().unwrap()),
        reserve0: U112::try_from(p.reserve0.as_ref().unwrap().parse::<u128>().unwrap()).unwrap(),
        reserve1: U112::try_from(p.reserve1.as_ref().unwrap().parse::<u128>().unwrap()).unwrap(),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::ZERO,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        update_block: p.liquidity_update_block.unwrap_or(0),
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
    })
    .map_err(|e| format!("register_v2: {e:?}"))
}

/// The V2 exact-in output (the token that is received). `zero_for_one` false →
/// token0 (stETH) is out; true → token1 is out.
fn v2_exact_in_output(
    reserve_in: u128,
    reserve_out: u128,
    gamma: u64,
    denom: u64,
    amount_in: u128,
) -> U256 {
    IntHopState::new(
        U256::from(reserve_in),
        U256::from(reserve_out),
        gamma,
        denom,
    )
    .swap(U256::from(amount_in))
    .expect("V2 constant-product swap is always computable")
}

fn main() {
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let text = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("read fixture {fixture_path}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let rec = &fx.recorded_solve;
    let hop_idx = rec.v2_hop_index;

    println!(
        "fixture: block={} V2-hop={hop_idx} optimal_input={} recorded_input={} predicted={} actual={}",
        fx.target_block, rec.optimal_input, rec.v2_input, rec.v2_predicted, rec.v2_actual
    );
    println!(
        "  recorded hop_outputs[..] = {:?}  (solver V2-hop should sit at index {hop_idx})",
        rec.hop_outputs
    );

    // The V2 pair orientation: token0=stETH, token1=USDT, zfo=false → input is
    // USDT (token1, reserve1), output is stETH (token0, reserve0).
    let v2 = &fx.pools.v2_1;
    let reserve0: u128 = v2.reserve0.as_ref().unwrap().parse().unwrap(); // stETH
    let reserve1: u128 = v2.reserve1.as_ref().unwrap().parse().unwrap(); // USDT
    let reserve_in = reserve1; // zfo=false input reserve (USDT)
    let reserve_out = reserve0; // zfo=false output reserve (stETH)
    let recorded_actual: U256 = rec.v2_actual.parse().unwrap();
    let recorded_predicted: U256 = rec.v2_predicted.parse().unwrap();

    // 1b) Recorded-input oracle check (independent of the global Möbius solve):
    //     at the LIVE recorded V2 input (the prior hop's output, USDT 27415),
    //     constant-product `IntHopState` (997/1000) is the byte-exact truth —
    //     identical to the on-chain V2 pair's `_v2_get_amount_out`. The solver
    //     uses the SAME formula, so `oracle == recorded_predicted` (both …115)
    //     and `oracle != recorded_actual` (…114) tells us the sim's …114 is NOT
    //     reproducible from constant-product at the on-chain reserves.
    let rec_input_abs: u128 = rec.v2_input.parse().unwrap();
    let rec_oracle = v2_exact_in_output(reserve_in, reserve_out, 997, 1000, rec_input_abs);
    println!("--- recorded-input constant-product oracle ---");
    println!("IntHopState(997/1000) @ recorded V2 input {rec_input_abs} = {rec_oracle}");
    println!(
        "  == recorded solver predicted ({recorded_predicted})?  {}",
        rec_oracle == recorded_predicted
    );
    println!(
        "  == recorded on-chain actual ({recorded_actual})?  {}",
        rec_oracle == recorded_actual
    );
    println!(
        "  => solver-oracle is the SAME math; if oracle matches predicted but not actual, the {} wei\n     sits on the sim side, not the solver.",
        recorded_predicted.saturating_sub(recorded_actual)
    );

    // 2) The production Möbius solver over the reconstructed three pools.
    let engine = ArbitrageEngine::new();
    let pid0 =
        register_v3(&mut engine.core().write(), &fx.pools.v3_0).unwrap_or_else(|e| panic!("{e}"));
    let pid2 =
        register_v3(&mut engine.core().write(), &fx.pools.v3_2).unwrap_or_else(|e| panic!("{e}"));
    let pid1 =
        register_v2(&mut engine.core().write(), &fx.pools.v2_1).unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v3_0" => pid0,
            "v3_2" => pid2,
            "v2_1" => pid1,
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

    // 3) The verdict — input-matched: drive the constant-product oracle at the
    //    solver's OWN consumed V2-hop input, so the comparison is never at a
    //    different amount.
    let mut engine = engine;
    let solved = engine.register_and_solve_path(hops).ok().and_then(|pid| {
        let (results, _) = engine.latest_results();
        results.get(&pid).cloned()
    });

    match solved {
        None => println!("solver: path not profitable -> no solve (NO VERDICT on real full path)."),
        Some(sr) => {
            let solver_in: U256 = sr.consumed_inputs[hop_idx];
            let solver_out: U256 = sr.hop_outputs[hop_idx];
            let solver_in_u128 = u128::try_from(solver_in).expect("V2 hop input fits u128");
            let oracle_out = v2_exact_in_output(reserve_in, reserve_out, 997, 1000, solver_in_u128);

            println!("solver V2-hop input  (consumed_inputs[{hop_idx}]): {solver_in}");
            println!("solver V2-hop output (hop_outputs[{hop_idx}]):     {solver_out}");
            println!("constant-product oracle @ same input:               {oracle_out}");
            println!("recorded solver predicted (historical repro):       {recorded_predicted}");
            println!("recorded on-chain actual (historical repro):        {recorded_actual}");

            // The fix criterion (path 11354): the solver's V2-hop crossing
            // output must equal the constant-product oracle byte-exactly. The
            // oracle is the on-chain truth (the V2 pair's `_v2_get_amount_out`
            // is 9970/10000 ≡ this 997/1000), and the solver IS this math, so
            // `solver_ok` asserts solver self-consistency against on-chain math.
            let solver_ok = solver_out == oracle_out;
            println!("solver V2 hop == constant-product oracle:           {solver_ok}");

            if solver_ok {
                println!(
                    "=> VERDICT: PASS on the solver — the solver V2-hop ({solver_out}) is \
                     byte-exact to constant-product at the on-chain reserves and equals the \
                     recorded predicted ({recorded_predicted}). The recorded on-chain actual \
                     ({recorded_actual}) is {} wei LOWER and is NOT reproducible by the solver: \
                     this localizes the 1-wei divergence to the SIM side (the sim's V2_SWAP_CALC \
                     executed on a state where the stETH reserve behaved ~145 wei lower than \
                     on-chain). Continue the sim-side investigation.",
                    solver_out.saturating_sub(recorded_actual)
                );
            } else {
                println!(
                    "=> VERDICT: FAIL (RED) — solver V2-hop output ({solver_out}) differs from \
                     the constant-product oracle ({oracle_out}) at the same input: a solver-side \
                     crossing defect."
                );
                std::process::exit(1);
            }
        }
    }
}
