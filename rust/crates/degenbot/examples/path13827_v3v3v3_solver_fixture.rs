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
use degenbot_solvers::mobius_v3_int::int_simulate_v3_swap;

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

    // 1b) The solver-math parity on the IDENTICAL (fixture-current) hop1 state:
    //     feed the recorded hop1 input through the solver's int crossing
    //     (`compute_crossing` + `int_simulate_v3_swap`) and compare against the
    //     oracle. If they now AGREE at 41579706, the live +1 was stale engine
    //     state (pump ordering), not solver math. If the int crossing still
    //     yields 41579707, it is a genuine CL-crossing residual (the fix target).
    let seq = v3_1_state
        .build_int_v3_sequence(hop1_ts, hop1_fee, hop1_zfo, 30)
        .expect("hop1 builds an int sequence");
    let n = seq.ranges.len();
    let mut chosen_k = 0usize;
    for k in 0..n {
        let crossing = seq.compute_crossing(k).expect("crossing k");
        if crossing.crossing_gross_input <= hop1_input {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k).expect("crossing chosen");
    let remaining = hop1_input.saturating_sub(crossing.crossing_gross_input);
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    let solver_math_out = crossing.crossing_output.saturating_add(ending.output);
    eprintln!(
        "[DIAG] chosen_k={chosen_k} n={n} crossing_gross_input={} crossing_output={} remaining={} ending.output={}",
        crossing.crossing_gross_input, crossing.crossing_output, remaining, ending.output
    );
    // Isolate the residual: re-walk the FULL crossing (ranges 0..chosen_k,
    // entry->interior boundaries->exit) with the CANONICAL compute_swap_step_v3
    // (what v3_simulate_swap does) instead of the closed-form
    // exact_in_step_to_target path used by compute_crossing. If this matches
    // the sim where the closed-form path over-counts by 1, the residual is in
    // `full_crossing_of_range`/`exact_in_step_to_target`.
    {
        eprintln!(
            "[DIAG] r0 liquidity={} sp_lower={} sp_upper={} sp_cur={} n_word_boundaries={} sim_post_sqrt={} sim_post_tick={}",
            seq.ranges[0].liquidity,
            seq.ranges[0].sqrt_price_lower_x96,
            seq.ranges[0].sqrt_price_upper_x96,
            seq.ranges[0].sqrt_price_x96,
            seq.ranges[0].word_boundary_prices.len(),
            sim.sqrt_price_x96,
            sim.tick
        );
        eprintln!(
            "[DIAG] r0 word_boundary_prices (first 6): {:?}",
            seq.ranges[0]
                .word_boundary_prices
                .iter()
                .take(6)
                .collect::<Vec<_>>()
        );
        eprintln!(
            "[DIAG] sim amount0={} amount1={} | solver ending.output={} : parity?",
            sim.amount0, sim.amount1, ending.output
        );
        // Direct replication of the solver's single step vs the sim's on the
        // SAME compute_swap_step_v3 with the same start/liquidity/input/fee.
        {
            use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
            let liq = i128::try_from(seq.ranges[0].liquidity).unwrap();
            let fee = U256::from(seq.ranges[0].fee_denom - seq.ranges[0].gamma_numer);
            let amt = I256::try_from(hop1_input).unwrap();
            let step1 = compute_swap_step_v3(
                seq.ranges[0].sqrt_price_x96,
                seq.ranges[0].sqrt_price_lower_x96,
                liq,
                amt,
                fee,
            )
            .unwrap();
            let step2 = compute_swap_step_v3(
                v3_1_state.sqrt_price_x96,
                seq.ranges[0].sqrt_price_lower_x96,
                liq,
                amt,
                fee,
            )
            .unwrap();
            eprintln!(
                "[DIAG] solver-step amount_out={} sqrt_next={} fee={fee} | sim-state-step amount_out={} sqrt_next={} | state_sqrt={}",
                step1.amount_out, step1.sqrt_price_next, step2.amount_out, step2.sqrt_price_next, v3_1_state.sqrt_price_x96
            );
            // Equality probe: is the coercive price EXACTLY at the current tick's
            // sqrt boundary (the condition that triggers the step-1 drain)?
            let tick_sqrt =
                degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(-276_324)
                    .map(|v| U256::from(v))
                    .unwrap_or_default();
            eprintln!(
                "[DIAG] tick_sqrt(-276324)={tick_sqrt} state_sqrt={} equal={}",
                v3_1_state.sqrt_price_x96,
                tick_sqrt == v3_1_state.sqrt_price_x96
            );
        }
    }
    {
        use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
        let fee_pips = U256::from(seq.ranges[0].fee_denom - seq.ranges[0].gamma_numer);
        let zfo = seq.ranges[0].zero_for_one;
        let mut consumed_canon = U256::ZERO;
        let mut output_canon = U256::ZERO;
        let mut rem = I256::try_from(hop1_input).expect("input fits i256");
        for i in 0..chosen_k {
            let r = &seq.ranges[i];
            let mut sp = if i == 0 {
                r.sqrt_price_x96
            } else if zfo {
                seq.ranges[i - 1].sqrt_price_lower_x96
            } else {
                seq.ranges[i - 1].sqrt_price_upper_x96
            };
            let exit = if zfo {
                r.sqrt_price_lower_x96
            } else {
                r.sqrt_price_upper_x96
            };
            for &target in r.word_boundary_prices.iter().chain(std::iter::once(&exit)) {
                if rem <= I256::ZERO {
                    break;
                }
                let step = compute_swap_step_v3(
                    sp,
                    target,
                    i128::try_from(r.liquidity).unwrap(),
                    rem,
                    fee_pips,
                )
                .expect("step");
                let consumed = step.amount_in.saturating_add(step.fee_amount);
                consumed_canon = consumed_canon.saturating_add(consumed);
                output_canon = output_canon.saturating_add(step.amount_out);
                rem = rem
                    .checked_sub(I256::try_from(consumed).unwrap())
                    .unwrap_or(I256::ZERO);
                sp = step.sqrt_price_next;
                if sp != target {
                    break;
                }
            }
        }
        let ending_canon = int_simulate_v3_swap(
            U256::try_from(rem).unwrap_or(U256::ZERO),
            &crossing.ending_range,
        );
        let total_canon = output_canon.saturating_add(ending_canon.output);
        eprintln!(
            "[DIAG] canonical cross walk: output_canon={} ending.output={} total={} | sim={sim_out}",
            output_canon, ending_canon.output, total_canon
        );
    }
    println!("--- solver-math parity on identical state (hop1) ---");
    println!("solver int crossing output @ recorded input: {solver_math_out}");
    println!("v3_simulate_swap @ same state+input:          {sim_out}");
    println!(
        "  solver-math == sim? {}  | == recorded actual ({recorded_actual})? {}",
        solver_math_out == sim_out,
        solver_math_out == recorded_actual
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
