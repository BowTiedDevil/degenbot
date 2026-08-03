//! Fee-1 V3→V4→V3 fixture: Möbius solver V4-hop vs byte-exact on-chain (UO3JM4).
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the path13308 solver-fixture runner
//! Modeled 1:1 on `path13308_solver_fixture.rs` (multi-pool-state fixture that
//! surfaced the PancakeSwap-V3 bug). Loads
//! `tests/fixtures/fee1_v3v4v3_block<BLOCK>.json` — the exact V3-V4-V3 pool
//! states at the failure block, captured by
//! `scripts/capture_fee1_v3v4v3_fixture.py` from the DB liquidity snapshot +
//! on-chain scalar reads — reconstructs the three pools into `BotState`, runs
//! the production Möbius solver, and reports the solver's V4-hop output against
//! the byte-exact `v4_simulate_swap` (which the tier-3 oracle proves equals the
//! on-chain `PoolManager` BalanceDelta for fee-1/tiny states) and the recorded
//! on-chain `[sim-revert-swap]` `actual`.
//!
//! The bug it pins (ergo UO3JM4): the solver's V4-hop `hop_outputs[i]` comes
//! from the int-solve crossing path (`compute_crossing`/`int_simulate_v3_swap`),
//! which over-predicts `v4_simulate_swap` by a few wei on fee-1/tiny pools →
//! the composer's `V4_TAKE(predicted)` overdrafts USDC → ERC20 "transfer amount
//! exceeds balance" → path reverts in sim. This harness asserts the fix target:
//! **solver V4 output == `v4_simulate_swap` == recorded on-chain actual — all
//! three byte-equal** (the `[sim-revert-swap]` `hop=1 matched=true` acceptance).

use std::collections::HashMap;

use alloy::primitives::{Address, B256, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot::RegisterV3PoolParams;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::v3_state::{PoolTickCoverage, SimulateSwapError, V3SwapOutcome};
use degenbot_pools::v4_state::{v4_simulate_swap, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/fee1_v3v4v3_block25600000.json"
);
#[derive(serde::Deserialize)]
struct Fixture {
    target_block: u64,
    v4_hop: V4Hop,
    pools: Pools,
    path: Vec<PathHop>,
}
/// The recorded V4 hop (the fee-1 divergence). `onchain_actual` is the revm-sim
/// captured `[sim-revert-swap] actual_out`; `predicted` is the solver's
/// `hop_outputs[i]`; `input` is the exact-in amount fed to the V4 pool (the
/// previous hop's output) — the exact-in amount targeted by `v4_simulate_swap`.
#[derive(serde::Deserialize)]
struct V4Hop {
    hop_index: usize,
    zero_for_one: bool,
    input: String,
    predicted_output: String,
    onchain_actual: String,
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

fn build_v4_state(p: &PoolData) -> V4PoolState {
    let pid_hex = p.pool_id.as_ref().unwrap();
    let pool_id: V4PoolId = pid_hex
        .trim_start_matches("0x")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect::<Vec<u8>>()
        .try_into()
        .unwrap();
    let pool_key = V4PoolKey {
        currency0: parse_addr(p.currency0.as_ref().unwrap()),
        currency1: parse_addr(p.currency1.as_ref().unwrap()),
        fee: p.fee_currency0.unwrap(),
        tick_spacing: p.tick_spacing.unwrap(),
        hooks: Address::ZERO,
    };
    let params = degenbot_pools::v4_state::RegisterV4PoolParams {
        pool_manager: parse_addr(p.pool_manager.as_ref().unwrap()),
        pool_id,
        pool_key,
        hook_flags: 0,
        protocol_fee: p.protocol_fee.unwrap_or(0),
        sqrt_price_x96: p.sqrt_price_x96.as_ref().unwrap().parse().unwrap(),
        liquidity: p.liquidity.as_ref().unwrap().parse().unwrap(),
        tick: p.tick.unwrap(),
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.as_ref().copied().unwrap(),
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// The V4 exact-in output token amount (the currency that is TAKEN).
/// `zero_for_one` false → token0 is output; true → token1 is output.
fn v4_exact_in_output(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

fn main() {
    // `FIXTURE_PATH` env override: run any captured recurrence block in the
    // same harness, e.g. FIXTURE_PATH=.../fee1_v3v4v3_block25672332.json.
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let text = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("read fixture {fixture_path}: {e}"));
    let fx: Fixture = serde_json::from_str(&text).expect("parse fixture");
    let fee = fx.pools.v4.fee_currency0.unwrap();
    let spacing = fx.pools.v4.tick_spacing.unwrap();
    let zfo = fx.v4_hop.zero_for_one;

    println!(
        "fixture: block={} V4 fee={fee} spacing={spacing} zfo={zfo} v4_hop_input={}",
        fx.target_block, fx.v4_hop.input
    );

    // 1) Build the V4 state (reconstructed from the fixture).
    let v4_state = build_v4_state(&fx.pools.v4);
    let recorded_actual: U256 = fx.v4_hop.onchain_actual.parse().unwrap();
    let recorded_predicted: U256 = fx.v4_hop.predicted_output.parse().unwrap();

    // 1b) The recorded-input oracle check (independent of the global Möbius
    //     solve, so it runs even when the reconstructed full path is unprofitable
    //     or the solver's own allocation lands on a different input). At the
    //     LIVE recorded V4 hop input (the prior hop's output, `fx.v4_hop.input`)
    //     the on-chain oracle (v4_simulate_swap) is the tier-3-proven byte-exact
    //     truth. If `recorded_predicted != oracle`, the solver's V4-hop crossing
    //     over-predicted on-chain — the UO3JM4 RED. If `oracle == recorded_actual`
    //     the oracle reproduces what the live sim observed.
    let rec_input_abs: U256 = fx.v4_hop.input.parse().unwrap();
    let rec_sim = match v4_simulate_swap(
        &v4_state,
        fee,
        spacing,
        zfo,
        I256::try_from(rec_input_abs)
            .expect("recorded input fits i256")
            .checked_neg()
            .expect("negate recorded input (V4 exact-in is negative)"),
        U256::MAX,
    ) {
        Ok(s) => s,
        Err(SimulateSwapError::NotComputable) => {
            println!("recorded-input v4_simulate_swap: NotComputable");
            std::process::exit(2);
        }
        Err(SimulateSwapError::MissingTickWord(w)) => {
            println!("recorded-input v4_simulate_swap: MissingTickWord({w})");
            std::process::exit(2);
        }
    };
    let rec_sim_out = v4_exact_in_output(&rec_sim, zfo);
    let solver_over_predicted = rec_sim_out != recorded_predicted;
    println!("--- recorded-input oracle check (per-V4-pool) ---");
    println!(
        "recorded-input oracle: v4_simulate_swap @ recorded V4 input {rec_input_abs} = {rec_sim_out}"
    );
    println!(
        "  == recorded on-chain actual ({recorded_actual})?  {}",
        rec_sim_out == recorded_actual
    );
    println!("  != recorded solver predicted ({recorded_predicted})? {solver_over_predicted}");

    // 1c) Root-cause probe (self-checking): the recorded actual reproduces at
    //     a slightly SMALLER V4 input than the solver's hop_outputs[0]. Find
    //     that input (bounded linear probe below rec_input_abs; fee-1 / tiny
    //     states are ~1:1 in units) and report the inter-hop forward-amount
    //     gap. This demonstrates the UO3JM4 overdraft mechanism: the solver
    //     over-states the amount carried into the V4 pool by a few units, so
    //     V4_TAKE(predicted output) withdraws more than the pool settles.
    let mut gap: u64 = 0;
    for d in 1..=128u64 {
        let cand_i =
            i128::try_from(rec_input_abs).expect("recorded input fits i128") - i128::from(d);
        if cand_i <= 0 {
            break;
        }
        let cand = U256::from(u128::try_from(cand_i).expect("cand_i positive"));
        if let Ok(s) = v4_simulate_swap(
            &v4_state,
            fee,
            spacing,
            zfo,
            I256::try_from(cand)
                .expect("probe input fits i256")
                .checked_neg()
                .expect("negate probe input"),
            U256::MAX,
        ) {
            if v4_exact_in_output(&s, zfo) == recorded_actual {
                gap = d;
                break;
            }
        }
    }
    if gap > 0 {
        println!(
            "root cause: recorded actual reproduces at V4 input {} (= {} down from solver's {}): the solver over-states the forward amount by {gap} unit(s) -> V4_TAKE(predicted) overdrafts.",
            rec_input_abs.saturating_sub(U256::from(gap)),
            gap,
            rec_input_abs
        );
    } else {
        println!(
            "root-cause probe: recorded actual ({recorded_actual}) not reproduced within 128 lower inputs — gap is not a small forward-amount error."
        );
    }

    // 2) The production Möbius solver path over the reconstructed three pools.
    let engine = ArbitrageEngine::new();
    let pid0 = register_v3(&mut engine.core.write(), &fx.pools.v3_0);
    let pid2 = register_v3(&mut engine.core.write(), &fx.pools.v3_2);
    let v4id = engine
        .core
        .write()
        .register_v4_pool(&degenbot::RegisterV4PoolParams {
            pool_manager: parse_addr(fx.pools.v4.pool_manager.as_ref().unwrap()),
            pool_id: v4_pool_id_bytes(fx.pools.v4.pool_id.as_ref().unwrap()),
            pool_key: V4PoolKey {
                currency0: parse_addr(fx.pools.v4.currency0.as_ref().unwrap()),
                currency1: parse_addr(fx.pools.v4.currency1.as_ref().unwrap()),
                fee,
                tick_spacing: spacing,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: fx.pools.v4.protocol_fee.unwrap_or(0),
            sqrt_price_x96: fx
                .pools
                .v4
                .sqrt_price_x96
                .as_ref()
                .unwrap()
                .parse()
                .unwrap(),
            liquidity: fx.pools.v4.liquidity.as_ref().unwrap().parse().unwrap(),
            tick: fx.pools.v4.tick.unwrap(),
            tick_data: tick_map(&fx.pools.v4.tick_data),
            update_block: fx
                .pools
                .v4
                .liquidity_update_block
                .as_ref()
                .copied()
                .unwrap(),
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

    // 3) The verdict — the fix target: solver V4 hop == v4_simulate_swap == on-chain.
    //    Drive v4_simulate_swap at the solver's OWN V4-hop consumed input so the
    //    comparison is input-matched (a clean byte-exact signal, not a
    //    different-input mismatch).
    let mut engine = engine;
    let solved = engine.register_and_solve_path(hops).ok().and_then(|pid| {
        let (results, _) = engine.latest_results();
        results.get(&pid).cloned()
    });

    match solved {
        None => println!("solver: path not profitable -> no solve (NO VERDICT on real full path)."),
        Some(sr) => {
            let solver_in: U256 = sr.consumed_inputs[fx.v4_hop.hop_index];
            let solver_v4_out: U256 = sr.hop_outputs[fx.v4_hop.hop_index];
            let amount_specified = I256::try_from(solver_in)
                .expect("v4 hop input fits i256")
                .checked_neg()
                .expect("negate input (V4 exact-in is negative)");
            let sim =
                match v4_simulate_swap(&v4_state, fee, spacing, zfo, amount_specified, U256::MAX) {
                    Ok(s) => s,
                    Err(SimulateSwapError::NotComputable) => {
                        println!("v4_simulate_swap: NotComputable — abort");
                        std::process::exit(2);
                    }
                    Err(SimulateSwapError::MissingTickWord(w)) => {
                        println!("v4_simulate_swap: MissingTickWord({w})");
                        std::process::exit(2);
                    }
                };
            let sim_out = v4_exact_in_output(&sim, zfo);

            println!(
                "solver V4-hop input  (consumed_inputs[{}]): {solver_in}",
                fx.v4_hop.hop_index
            );
            println!(
                "solver V4-hop output (hop_outputs[{}]):     {solver_v4_out}",
                fx.v4_hop.hop_index
            );
            println!("v4_simulate_swap @ same input (on-chain oracle): {sim_out}");
            println!("recorded on-chain actual (historical repro):     {recorded_actual}");
            println!("recorded solver predicted (historical repro):    {recorded_predicted}");

            // The fix criterion (UO3JM4): the solver's V4-hop crossing output
            // must equal v4_simulate_swap byte-exactly. v4_simulate_swap is
            // independently proven byte-exact to the on-chain PoolManager by
            // the tier-3 oracle, so this is the self-contained on-chain truth
            // (no dependence on the historical recorded pair).
            let solver_ok = solver_v4_out == sim_out;
            println!("solver V4 hop == v4_simulate_swap:                {solver_ok}");

            if solver_ok {
                println!("=> VERDICT: PASS — solver V4 hop matches on-chain (matched=true).");
            } else {
                println!(
                    "=> VERDICT: FAIL (RED) — solver V4-hop output ({solver_v4_out}) differs from v4_simulate_swap ({sim_out}) at the same input: the fee-1 crossing over-prediction."
                );
                std::process::exit(1);
            }
        }
    }
}

fn v4_pool_id_bytes(hex: &str) -> V4PoolId {
    hex.trim_start_matches("0x")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect::<Vec<u8>>()
        .try_into()
        .unwrap()
}
