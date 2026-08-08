#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
//! Path-13308 (V3-V4-V3) snapshot: Möbius solver vs on-chain reality — driven
//! through the shared `degenbot::investigation` toolkit.
//!
#![allow(clippy::too_many_lines)]
//! Loads `tests/fixtures/path13308_v3v4v3_block25664704.json` (exact pool states
//! for block 25664704 captured by `scripts/capture_path_13308_fixture.py`),
//! reconstructs the V3-V4-V3 pools, runs the production Möbius solver
//! (`ArbitrageEngine::register_and_solve_path`), and reports
//! `optimal_input`/`hop_outputs` against the RECORDED solve, plus a per-hop
//! tier-3 oracle comparison (V3/V4 twins) at the solver's inputs.
//!
//! Insight for the `no-profit` crash: the solver predicted a +1.19e11 wei WETH
//! cycle-profit but executing the same plan on-chain nets -2.58e12 wei (gross).
//! This isolates whether that sign flip is a solver-error vs a sub-threshold
//! unprofitable candidate. The DB-vs-verified-current scalars can be probed via
//! `FIXTURE_V3_2_SQRT`/`FIXTURE_V3_2_TICK`/`FIXTURE_V4_PROTO_FEE` env overrides.

use std::collections::HashMap;

use alloy::primitives::U256;
use degenbot::investigation::{
    build_v3_state, build_v4_state, display_check, register_v3_with, register_v4_with,
    v3_hop_output, v4_hop_output, OracleOutcome, PathFixture,
};
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path13308_v3v4v3_block25664704.json"
);

fn main() {
    let fx = PathFixture::load(FIXTURE_PATH).unwrap_or_else(|e| panic!("{e}"));

    let engine = ArbitrageEngine::new();
    let v3_2_sqrt: Option<U256> = std::env::var("FIXTURE_V3_2_SQRT")
        .ok()
        .map(|s| s.parse().unwrap());
    let v3_2_tick: Option<i32> = std::env::var("FIXTURE_V3_2_TICK")
        .ok()
        .map(|s| s.parse().unwrap());
    let v4_proto: Option<u32> = std::env::var("FIXTURE_V4_PROTO_FEE")
        .ok()
        .map(|s| s.parse().unwrap());
    println!("v3_2 override sqrt={v3_2_sqrt:?} tick={v3_2_tick:?} v4 proto_fee={v4_proto:?}");

    let pid0 = register_v3_with(&mut engine.core().write(), &fx.pools["v3_0"], None, None)
        .unwrap_or_else(|e| panic!("{e}"));
    let pid2 = register_v3_with(
        &mut engine.core().write(),
        &fx.pools["v3_2"],
        v3_2_sqrt,
        v3_2_tick,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let v4id = register_v4_with(&mut engine.core().write(), &fx.pools["v4"], v4_proto)
        .unwrap_or_else(|e| panic!("{e}"));

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
    let path_id = engine
        .register_and_solve_path(hops.clone())
        .expect("solve path");
    let rec = &fx.recorded_solve;
    let recorded_optimal = rec.optimal_input.map(|a| a.0);
    let recorded_hops: Vec<U256> = rec.hop_outputs.iter().map(|a| a.0).collect();

    let (results, _) = engine.latest_results();
    match results.get(&path_id) {
        Some(r) => {
            println!("=== solver result (path {path_id}) ===");
            println!("  optimal_input (recomputed): {}", r.optimal_input);
            println!("  optimal_input (recorded):   {recorded_optimal:?}");
            println!("  hop_outputs (recomputed):   {:?}", r.hop_outputs);
            println!("  hop_outputs (recorded):     {recorded_hops:?}");
            println!("  profit (recomputed):        {}", r.profit);
            let matches =
                Some(r.optimal_input) == recorded_optimal && r.hop_outputs == recorded_hops;
            let verdict = if matches {
                "MATCHES recorded solve"
            } else {
                "DIFFERS from recorded solve"
            };
            println!("  => {verdict}");

            // Per-hop tier-3 oracle comparison (input-matched).
            println!("=== per-hop tier-3 oracle comparison (input-matched) ===");
            for (i, ph) in fx.path.iter().enumerate() {
                let pool = ph.pool.as_str();
                let zfo = ph.zero_for_one;
                let outcome = match pool {
                    "v3_0" | "v3_2" => {
                        let st = build_v3_state(&fx.pools[pool]);
                        v3_hop_output(
                            &st,
                            fx.pools[pool].fee_token0.unwrap(),
                            fx.pools[pool].tick_spacing.unwrap(),
                            zfo,
                            r.consumed_inputs[i],
                        )
                    }
                    "v4" => {
                        let st = build_v4_state(&fx.pools["v4"]);
                        v4_hop_output(
                            &st,
                            fx.pools["v4"].fee_currency0.unwrap(),
                            fx.pools["v4"].tick_spacing.unwrap(),
                            zfo,
                            r.consumed_inputs[i],
                        )
                    }
                    _ => OracleOutcome::NotComputable,
                };
                println!(
                    "  hop[{i}] {pool} in={} {}",
                    r.consumed_inputs[i],
                    display_check(&outcome, r.hop_outputs[i])
                );
            }
        }
        None => println!("solver returned None for path {path_id} (no profitable input)"),
    }
}
