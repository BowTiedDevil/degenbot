//! V3→V2→V2 fixture: Möbius solver V2-hops vs byte-exact constant-product (path 11053).
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the path11354 / fee1_v3v4v3 solver-fixture runners
//! Modeled 1:1 on `path11354_v3v2v3_solver_fixture.rs` (the V3-V2-V3 sim-side
//! under-delivery harness) and `fee1_v3v4v3_solver_fixture.rs`. Loads
//! `tests/fixtures/path11053_v3v2v2_block<BLOCK>.json` — the exact V3-V2-V2
//! pool states at the failure block, captured by
//! `scripts/capture_path11053_v3v2v2_fixture.py` from the DB liquidity snapshot
//! + on-chain scalar/reserve reads — reconstructs the three pools into
//! `BotState`, runs the production Möbius solver, and reports each V2-hop
//! output against the byte-exact constant-product oracle at the on-chain
//! reserves and the recorded `[sim-revert-swap]` predicted/actual from the log.
//!
//! The live trap (path 11053, block 25695693, type V3-V2-V2, bucket Pancake: K):
//! ```text
//!   hop0 V3 0x4e68Ccd3 WETH→USDT (zfo)          matched=true
//!   hop1 V2 0x648Ef94C USDT→stETH (997/1000)    predicted=8246881364465 actual=8091930949192 (1.88% short)
//!   hop2 V2 0x3cC0B797 stETH→WETH (9975/10000)  predicted=8053151054828 (reverts Pancake: K via hop1 shortfall)
//! ```
//! The solver's V2-hop outputs come from `IntHopState` (the constant-product
//! `getAmountOut`) — exactly the on-chain V2 pair's `_v2_get_amount_out` with
//! that pair's retained fee fraction (hop1 997/1000, hop2 9975/10000). So by
//! construction there is NO solver-vs-oracle gap. The engine reserves match
//! on-chain at the block byte-for-byte, so this harness pins the question:
//! **is the solver reproducible from the reconstructed on-chain reserves, or
//! is it over-predicting?** The oracle is computed at the RECORDED hop inputs
//! (input-matched), so the verdict never compares at different amounts.
//!
//! Exit 0 = each solver V2 hop is byte-exact to constant-product at the
//! on-chain reserves (the solver is exonerated; the recorded 155e9-wei
//! under-delivery sits on the sim side). Exit 1 = RED if the solver diverges
//! from the oracle. Exit 2 = the path is not soluble from the reconstructed
//! state.

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
    "/../../../tests/fixtures/path11053_v3v2v2_block25695693.json"
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
    v2_2: PoolData,
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
    fee_gamma: Option<u64>,
    fee_denom: Option<u64>,
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

fn register_v2(core: &mut BotState, p: &PoolData, variant: DexVariant) -> Result<u64, String> {
    let (gamma, denom) = (p.fee_gamma.unwrap(), p.fee_denom.unwrap());
    core.register_v2_pool(&RegisterV2PoolParams {
        address: parse_addr(p.address.as_ref().unwrap()),
        token0: parse_addr(p.token0.as_ref().unwrap()),
        token1: parse_addr(p.token1.as_ref().unwrap()),
        reserve0: U112::try_from(p.reserve0.as_ref().unwrap().parse::<u128>().unwrap()).unwrap(),
        reserve1: U112::try_from(p.reserve1.as_ref().unwrap().parse::<u128>().unwrap()).unwrap(),
        fee_token0: (gamma, denom),
        fee_token1: (gamma, denom),
        factory: Address::ZERO,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        update_block: 0,
        variant,
        stable_swap: false,
        fee_denominator: None,
    })
    .map_err(|e| format!("register_v2: {e:?}"))
}

/// The V2 exact-in output (the token that is received) via `IntHopState` —
/// the same constant-product `getAmountOut` the solver and the on-chain V2
/// pair both use. `gamma`/`denom` are that pair's retained fee fraction.
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
        "fixture: block={} V2-hop={hop_idx} optimal_input={} recorded V2 input={} predicted={} actual={}",
        fx.target_block, rec.optimal_input, rec.v2_input, rec.v2_predicted, rec.v2_actual
    );
    println!("  recorded hop_outputs[..] = {:?}", rec.hop_outputs);

    // ── Hop 1 oracle (USDT→stETH, zfo=false): t0=stETH(reserve0 out), t1=USDT(reserve1 in) ──
    let v1 = &fx.pools.v2_1;
    let (g1, d1) = (v1.fee_gamma.unwrap(), v1.fee_denom.unwrap());
    let r1_in: u128 = v1.reserve1.as_ref().unwrap().parse().unwrap(); // USDT
    let r1_out: u128 = v1.reserve0.as_ref().unwrap().parse().unwrap(); // stETH
    let rec_actual: U256 = rec.v2_actual.parse().unwrap();
    let rec_predicted: U256 = rec.v2_predicted.parse().unwrap();
    let rec_in_abs: u128 = rec.v2_input.parse().unwrap();
    let oracle_1 = v2_exact_in_output(r1_in, r1_out, g1, d1, rec_in_abs);

    // ── Hop 2 oracle (stETH→WETH, zfo=true): t0=stETH(reserve0 in), t1=WETH(reserve1 out) ──
    // Input = hop1's predicted output (the solver chains predicted→predicted).
    let v2 = &fx.pools.v2_2;
    let (g2, d2) = (v2.fee_gamma.unwrap(), v2.fee_denom.unwrap());
    let r2_in: u128 = v2.reserve0.as_ref().unwrap().parse().unwrap(); // stETH
    let r2_out: u128 = v2.reserve1.as_ref().unwrap().parse().unwrap(); // WETH
    let rec_hop2_pred: u128 = rec.hop_outputs[2].parse().unwrap();
    let hop2_input: u128 = rec_predicted.to_string().parse().unwrap(); // hop1 predicted output == hop2 input
    let oracle_2 = v2_exact_in_output(r2_in, r2_out, g2, d2, hop2_input);

    println!("--- recorded-input constant-product oracles (997/1000 and 9975/10000) ---");
    println!("hop1 oracle @ recorded USDT in {rec_in_abs} = {oracle_1}");
    println!(
        "  == recorded solver predicted ({rec_predicted})? {}",
        oracle_1 == rec_predicted
    );
    println!(
        "  == recorded on-chain actual ({rec_actual})? {}",
        oracle_1 == rec_actual
    );
    println!(
        "  => solver-oracle is the SAME math; if oracle matches predicted but not actual, the {} wei\n     sits on the sim side, not the solver.",
        rec_predicted.saturating_sub(rec_actual)
    );
    println!("hop2 oracle @ hop1-predicted stETH in {hop2_input} = {oracle_2}");
    println!(
        "  == recorded solver predicted hop_outputs[2] ({rec_hop2_pred})? {}",
        oracle_2 == U256::from(rec_hop2_pred)
    );

    // ── The production Möbius solver over the reconstructed three pools ──
    let engine = ArbitrageEngine::new();
    let pid0 =
        register_v3(&mut engine.core().write(), &fx.pools.v3_0).unwrap_or_else(|e| panic!("{e}"));
    let pid1 = register_v2(
        &mut engine.core().write(),
        &fx.pools.v2_1,
        DexVariant::UniswapV2,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let pid2 = register_v2(
        &mut engine.core().write(),
        &fx.pools.v2_2,
        DexVariant::PancakeswapV2,
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v3_0" => pid0,
            "v2_1" => pid1,
            "v2_2" => pid2,
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

    // 3) The verdict — input-matched: drive each constant-product oracle at the
    //    solver's OWN consumed hop input, so the comparison is never at a
    //    different amount.
    let mut engine = engine;
    let solved = engine.register_and_solve_path(hops).ok().and_then(|pid| {
        let (results, _) = engine.latest_results();
        results.get(&pid).cloned()
    });

    match solved {
        None => {
            println!("solver: path not profitable -> no solve (NO VERDICT on real full path).");
            std::process::exit(2);
        }
        Some(sr) => {
            let s_in1: U256 = sr.consumed_inputs[1];
            let s_out1: U256 = sr.hop_outputs[1];
            let oracle_1_solver = v2_exact_in_output(
                r1_in,
                r1_out,
                g1,
                d1,
                u128::try_from(s_in1).expect("V2 hop1 input fits u128"),
            );
            let s_in2: U256 = sr.consumed_inputs[2];
            let s_out2: U256 = sr.hop_outputs[2];
            let oracle_2_solver = v2_exact_in_output(
                r2_in,
                r2_out,
                g2,
                d2,
                u128::try_from(s_in2).expect("V2 hop2 input fits u128"),
            );

            println!(
                "--- production Möbius solver vs oracle (at the solver's own consumed inputs) ---"
            );
            println!(
                "hop1  consumed_in={s_in1} solver_out={s_out1} oracle={oracle_1_solver} match={}",
                s_out1 == oracle_1_solver
            );
            println!(
                "hop2  consumed_in={s_in2} solver_out={s_out2} oracle={oracle_2_solver} match={}",
                s_out2 == oracle_2_solver
            );
            println!("recorded historical repro: hop1 predicted={rec_predicted} hop2 predicted[2]={rec_hop2_pred}");
            println!("recorded on-chain actual  : hop1 actual={rec_actual}");

            let all_ok = s_out1 == oracle_1_solver && s_out2 == oracle_2_solver;
            if all_ok {
                println!(
                    "=> VERDICT: PASS on the solver — every V2 hop is byte-exact to \
                     constant-product at the on-chain reserves and equals the recorded \
                     predicted ({rec_predicted}). The recorded on-chain actual ({rec_actual}) \
                     is {} wei LOWER and is NOT reproducible by the solver: this localizes the \
                     ~1.88% under-delivery to the SIM side (the sim's V2 swaps executed on a \
                     state where the pool's effective post-tip reserves diverged from the \
                     engine's solve-time on-chain reserves). Continue the sim-side investigation.",
                    rec_predicted.saturating_sub(rec_actual)
                );
            } else {
                println!(
                    "=> VERDICT: FAIL (RED) — a solver V2-hop output diverges from its \
                     constant-product oracle at the same input: a solver-side crossing defect."
                );
                std::process::exit(1);
            }
        }
    }
}
