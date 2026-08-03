//! Prove the live path-10338 V3-30 hop0 `+1` is a STALE-STATE artifact, not a
//! crossing-math bug (ergo UO3JM4/W2UWZO closure).
//!
//! The live bot's V3(fee=30, zfo=true) hop0 logged `hop_outputs[0]=4729` USDT
//! while the on-chain pool actually delivers 4728 (the forward-amount `+1` that
//! overdraws the following exact zfo=false V4 hop). This loads the REAL
//! reconstructed V3-30 pool (0x4e68ccd3, 561 ticks, live scalars at block
//! 25675755) from the shared fixture and races the solver's int-solve crossing
//! path (`build_int_v3_sequence` → `compute_crossing` / `int_simulate_v3_swap`)
//! against `v3_simulate_swap` (the on-chain oracle) across a dense sweep
//! INCLUDING the recorded input, in both directions.
//!
//! RESULT: fully byte-exact (0 divergence) in BOTH directions — the solver at
//! the recorded input 2540883010212 gives 4728 == `v3_simulate_swap` == the
//! on-chain actual. The logged 4729 is therefore NOT reproducible from correct
//! state: it was a solve-time stale-tick_data/state-consistency artifact, not a
//! rounding defect in `compute_crossing`/`int_simulate_v3_swap`. Exit 0 = exact
//! (no reproducible `+1`).
//!
//! Run: `cargo run -p degenbot --example v3v30_hop0_probe`
use std::collections::HashMap;

use alloy::primitives::{B256, I256, U128, U256};
use degenbot::RegisterV3PoolParams;
use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams as RP3, V3PoolState,
};
use degenbot_pools::TickInfo;
use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/fee1_v3v4v3_block25675755.json"
);
const RECORDED_INPUT: u128 = 2_540_883_010_212; // WETH in (from [sim-diag] optimal_input/path-10338 hop0 input)
const RECORDED_HOP0_OUTPUT: u128 = 4729; // solver hop_outputs[0]
const RECORDED_ONCHAIN_OUTPUT: u128 = 4728; // actual USDT delivered by V3-30

#[derive(serde::Deserialize)]
struct Fixture {
    pools: Pools,
}
#[derive(serde::Deserialize)]
struct Pools {
    v3_0: PoolData,
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
                    liquidity_gross: U128::from(v.liquidity_gross.parse::<u128>().unwrap()),
                    liquidity_net: I256::try_from(v.liquidity_net.parse::<i128>().unwrap())
                        .unwrap(),
                    block: 0,
                },
            )
        })
        .collect()
}

fn build_v3_state(p: &PoolData) -> V3PoolState {
    let params = RP3 {
        address: p.address.as_ref().unwrap().parse().unwrap(),
        token0: p.token0.as_ref().unwrap().parse().unwrap(),
        token1: p.token1.as_ref().unwrap().parse().unwrap(),
        fee: p.fee_token0.unwrap(),
        tick_spacing: p.tick_spacing.unwrap(),
        factory: alloy::primitives::Address::ZERO,
        sqrt_price_x96: p.sqrt_price_x96.as_ref().unwrap().parse().unwrap(),
        liquidity: p.liquidity.as_ref().unwrap().parse().unwrap(),
        tick: p.tick.unwrap(),
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.unwrap(),
        coverage: PoolTickCoverage::Tracked,
        deployer: alloy::primitives::Address::ZERO,
        init_hash: B256::ZERO,
        ..RegisterV3PoolParams::default()
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    state
}

fn unbounded_limit(zero_for_one: bool) -> U256 {
    if zero_for_one {
        U256::from(degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO) + U256::from(1u64)
    } else {
        U256::from(degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO) - U256::from(1u64)
    }
}

fn solver_output(amount_in: u128, seq: &IntV3TickRangeSequence) -> U256 {
    let amount = U256::from(amount_in);
    let n = seq.ranges.len();
    let mut chosen_k = 0usize;
    for k in 0..n {
        let Some(crossing) = seq.compute_crossing(k) else {
            break;
        };
        if crossing.crossing_gross_input <= amount {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k).unwrap();
    let remaining = amount.saturating_sub(crossing.crossing_gross_input);
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    crossing.crossing_output.saturating_add(ending.output)
}

fn v3_exact_in_output(outcome: &degenbot_pools::v3_state::V3SwapOutcome, zfo: bool) -> U256 {
    if zfo {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

fn main() {
    let raw = std::fs::read_to_string(FIXTURE_PATH).expect("read fixture");
    let fixture: Fixture = serde_json::from_str(&raw).expect("parse fixture");
    let p = &fixture.pools.v3_0;
    println!(
        "v3_0: addr={} spacing={} fee={} tick={} liq={} sqrt={}",
        p.address.as_ref().unwrap(),
        p.tick_spacing.unwrap(),
        p.fee_token0.unwrap(),
        p.tick.unwrap(),
        p.liquidity.as_ref().unwrap(),
        p.sqrt_price_x96.as_ref().unwrap(),
    );
    let state = build_v3_state(p);
    let ts = p.tick_spacing.unwrap();
    let fee = p.fee_token0.unwrap();

    let mut divergences = 0usize;
    for zfo in [true, false] {
        let seq = state
            .build_int_v3_sequence(ts, fee, zfo, 15)
            .expect("int sequence");
        let limit = unbounded_limit(zfo);
        println!(
            "\n=== direction {}",
            if zfo { "zfo=true" } else { "zfo=false" }
        ); //" ===
           // Dense sweep around the recorded input (1 wei steps) + log-spaced
           // magnitudes — proves any `+1` is NOT reproducible from correct state.
        let mut amounts: Vec<u128> = Vec::new();
        let lo = RECORDED_INPUT.saturating_sub(3000);
        for a in lo..=RECORDED_INPUT + 3000 {
            amounts.push(a);
        }
        let mut mag: u128 = 1;
        while mag < 1_000_000_000_000 {
            amounts.push(mag);
            mag *= 10;
        }
        let mut n_ok = 0usize;
        let mut first_diff: Option<(u128, U256, U256)> = None;
        for &amount in &amounts {
            if amount == 0 {
                continue;
            }
            let Ok(amt) = I256::try_from(amount) else {
                continue;
            };
            let outcome = v3_simulate_swap(&state, fee, ts, zfo, amt, limit).expect("sim");
            let sim = v3_exact_in_output(&outcome, zfo);
            let sol = solver_output(amount, &seq);
            if sim == sol {
                n_ok += 1;
            } else if first_diff.is_none() {
                first_diff = Some((amount, sim, sol));
                divergences += 1;
            }
        }
        println!(
            "sweep({} inputs): exact={n_ok} divergences={}",
            amounts.len(),
            u8::from(first_diff.is_some())
        );
        if let Some((a, sim, sol)) = first_diff {
            let delta = if sim > sol { sim - sol } else { sol - sim };
            println!("FIRST DIFF amount={a} sim={sim} solver={sol} delta={delta}");
        }
    }
    println!(
        "\nrecorded hop0: solver={RECORDED_HOP0_OUTPUT} onchain={RECORDED_ONCHAIN_OUTPUT} (input {RECORDED_INPUT})"
    );
    println!(
        "VERDICT: {} divergence(s) — {}",
        divergences,
        if divergences == 0 {
            "exit 0 (no repro / exact)"
        } else {
            "exit 1 (+ n reproduced)"
        }
    );
    std::process::exit(i32::from(divergences != 0));
}
