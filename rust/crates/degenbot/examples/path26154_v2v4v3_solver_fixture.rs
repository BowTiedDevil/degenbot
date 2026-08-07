//! V2→V4→V3 fixture: Möbius solver replay of the path-26154 empty-Halt
//! (block 25700805), driven through the shared `degenbot::investigation`
//! toolkit instead of a re-derived per-example preamble.
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the fee1_v3v4v3 / path11354 solver-fixture runners
//! Loads `tests/fixtures/path26154_v2v4v3_block25700805.json` and reconstructs
//! the V2-V4-V3 pools into `BotState`, runs the production Möbius solver, and —
//! per hop — feeds the solver's input into the tier-3-validated oracle twin
//! (V2 `getAmountOut` / `v3_simulate_swap` / `v4_simulate_swap`) to assert each
//! hop's input→output is correct at the solution.
//!
//! Context (live `[sim-fail] path=26154 … bucket=empty`): the V4 hop UNI/MATIC
//! reverts EMPTY in sim; all three hops here check oracle-consistent
//! individually, so the empty-Halt is a full-path execution artifact (the
//! recorded input fills ~98% of the tracked band, and any real excess tips past
//! tick 35067 into zero liquidity).

use std::collections::HashMap;

use alloy::primitives::U256;
use degenbot::investigation::{
    build_v3_state, build_v4_state, display_check, register_v2, register_v3, register_v4,
    v2_get_amount_out, v3_hop_output, v4_hop_output, OracleOutcome, PathFixture, V2_DEFAULT_FEE,
};
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path26154_v2v4v3_block25700805.json"
);

fn main() {
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let fx = PathFixture::load(&fixture_path).unwrap_or_else(|e| panic!("{e}"));

    let rec = &fx.recorded_solve;
    let hop_idx = rec.v4_hop_index.unwrap();
    let fee = fx.pools["v4"].fee_currency0.unwrap();
    let spacing = fx.pools["v4"].tick_spacing.unwrap();
    let zfo = rec.v4_zero_for_one.unwrap();

    println!(
        "fixture: block={:?} V4-hop={hop_idx} fee={fee} spacing={spacing} zfo={zfo} bucket={}",
        fx.target_block,
        rec.sim_bucket.as_deref().unwrap_or("")
    );
    println!(
        "  recorded optimal_input={:?} hop_outputs={:?}",
        rec.optimal_input, rec.hop_outputs
    );
    println!(
        "  recorded V4 input={:?} predicted_out={:?} onchain={:?}",
        rec.v4_input, rec.v4_predicted_output, rec.v4_onchain
    );

    // The V4 tracked band: [min tick, max tick].
    let v4 = build_v4_state(&fx.pools["v4"]);
    let ticks: Vec<i32> = fx.pools["v4"]
        .tick_data
        .keys()
        .map(|t| t.parse().unwrap())
        .collect();
    let tmin = *ticks.iter().min().unwrap();
    let tmax = *ticks.iter().max().unwrap();
    let cur_tick = fx.pools["v4"].tick.unwrap();
    println!(
        "--- V4 tracked band ---\n  ticks used: {:?} -> range [{tmin},{tmax}] current tick {cur_tick} headroom-above {}",
        ticks.len(),
        tmax - cur_tick
    );

    // Register the three pools + resolve the hop list.
    let engine = ArbitrageEngine::new();
    let pid0 = register_v2(&mut engine.core().write(), &fx.pools["v2_0"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid2 = register_v3(&mut engine.core().write(), &fx.pools["v3_2"])
        .unwrap_or_else(|e| panic!("{e}"));
    let v4id =
        register_v4(&mut engine.core().write(), &fx.pools["v4"]).unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v2_0" => pid0,
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
    let solved = engine
        .register_and_solve_path(hops.clone())
        .ok()
        .and_then(|pid| {
            let (results, _) = engine.latest_results();
            results.get(&pid).cloned()
        });

    let recorded_ho: Vec<U256> = rec.hop_outputs.iter().map(|a| a.0).collect();
    match solved {
        None => {
            println!("solver: path NOT profitable -> production Möbius solver produced NO solve.");
            println!(
                "  => the solver did not emit the recorded hop_outputs {recorded_ho:?} from the reconstructed state."
            );
        }
        Some(sr) => {
            let solver_ins: Vec<U256> = sr.consumed_inputs.clone();
            let solver_outs: Vec<U256> = sr.hop_outputs.clone();
            println!("--- production Möbius solver result ---");
            for i in 0..3 {
                println!(
                    "  hop[{i}] zfo={} input(consumed)={} output(hop_outputs)={}",
                    hops[i].zero_for_one, solver_ins[i], solver_outs[i]
                );
            }
            println!("  recorded hop_outputs = {recorded_ho:?}");
            println!(
                "  solver hop_outputs == recorded hop_outputs? {}",
                solver_outs == recorded_ho
            );

            // ── Per-hop tier-3 oracle comparison (input-matched). ──
            println!("--- per-hop tier-3 oracle comparison (input-matched) ---");

            // hop[0] V2: getAmountOut from reserves + fee. zfo=false -> input
            // token1(WETH), output token0(USDT).
            let v2 = &fx.pools["v2_0"];
            let reserve_in = if hops[0].zero_for_one {
                v2.reserve0.unwrap().0
            } else {
                v2.reserve1.unwrap().0
            };
            let reserve_out = if hops[0].zero_for_one {
                v2.reserve1.unwrap().0
            } else {
                v2.reserve0.unwrap().0
            };
            let v2_out = v2_get_amount_out(solver_ins[0], reserve_in, reserve_out, V2_DEFAULT_FEE);
            println!(
                "  hop[0] V2 in={} {}",
                solver_ins[0],
                display_check(&OracleOutcome::Ok(v2_out), solver_outs[0])
            );

            // hop[1] V4: v4_simulate_swap (+ optional explicit note).
            let v3 = build_v3_state(&fx.pools["v3_2"]);
            let v3_oc = v3_hop_output(
                &v3,
                fx.pools["v3_2"].fee_token0.unwrap(),
                fx.pools["v3_2"].tick_spacing.unwrap(),
                hops[2].zero_for_one,
                solver_ins[2],
            );
            println!(
                "  hop[2] V3 in={} {}",
                solver_ins[2],
                display_check(&v3_oc, solver_outs[2])
            );

            let v4_oc = v4_hop_output(&v4, fee, spacing, zfo, solver_ins[hop_idx]);
            println!(
                "  hop[1] V4 in={} {}",
                solver_ins[hop_idx],
                display_check(&v4_oc, solver_outs[hop_idx])
            );
            println!(
                "  recorded V4 predicted (sim-diag) = {:?} onchain = {:?}",
                rec.v4_predicted_output, rec.v4_onchain
            );
        }
    }
    println!(
        "note: empty-Halt is a FULL-path revm-execution outcome (the V4 swap was attempted past the \
         band); the numbers above are the solver's/sim's pre-revert view at the recorded block."
    );
}
