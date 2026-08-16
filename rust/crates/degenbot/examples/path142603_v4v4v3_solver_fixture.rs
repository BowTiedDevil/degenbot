#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
//! Path-142603 (V4->V4->V3) snapshot: Möbius solver vs on-chain reality — the
//! live `DEGENBOT_SIM_EXIT_ON_FAIL=1` no-profit trap at block 25723658.
//!
#![expect(clippy::too_many_lines)]
//! Loads `tests/fixtures/path142603_v4v4v3_block25723658.json` (exact pool
//! states captured by `scripts/capture_path142603_v4v4v3_fixture.py`),
//! reconstructs the two V4 pools + one V3 pool, runs the production Möbius
//! solver (`ArbitrageEngine::register_and_solve_path`), and reports
//! `optimal_input`/`hop_outputs` against the RECORDED solve.
//!
//! ## The incident & the discriminating experiment
//! The solver selected this path and SIM-EXECUTED it to net `-173150825` wei
//! WETH (`gross_profit == 0` → the `no-profit` bucket → trap aborts the
//! process). On-chain ground truth at 25723658 (verified from archive RPC):
//!   - pool A (V4 USDC/WETH) `(sqrt, liq)` == solver   — honest
//!   - pool C (V3 WETH/USDT) `(sqrt, liq)` == solver   — honest
//!   - pool B (V4 USDC/USDT) `(sqrt, tick)` == solver (current) BUT solver
//!     `liq = 1_018_741_430_873` while on-chain = `718_152_690_765` — a
//!     ~3.05e11 `ModifyLiquidity` removal at ~block 25720300 was NOT applied;
//!     solver liq is frozen at the pre-removal value, ~3,300 blocks stale.
//!
//! This is a **staged-clock desync** (two-stamp OB7UNY): the price clock
//! (`update_block`) reached the solve block via swap incorporation while the
//! **liquidity clock** (`tick_data_block`, observed 25722568) did not — and
//! the gate's `skip_in_progress_hop` short-circuits verification for a hop
//! whose price clock reached the solve block, so the stale liquidity sails
//! through.
//!
//! Test by overriding pool B's liquidity:
//!   # Reproduce the PHANTOM solve (solver liq, stale):
//!   `FIXTURE_V4_B_LIQ=1018741430873` cargo run -p degenbot --example `path142603_v4v4v3_solver_fixture`
//!   # True on-chain state (fixture default 718152690765) should return None:
//!   cargo run -p degenbot --example `path142603_v4v4v3_solver_fixture`
//!
//! Other overrides: `FIXTURE_V4_A_PROTO_FEE` / `FIXTURE_V4_B_PROTO_FEE`
//! (protocol fee), `FIXTURE_V3_C_SQRT` / `FIXTURE_V3_C_TICK`.

use std::collections::HashMap;

use alloy::primitives::U256;
use degenbot::investigation::fixture::Amount;
use degenbot::investigation::{
    build_v3_state, build_v4_state, display_check, register_v3_with, register_v4_with,
    v3_hop_output, v4_hop_output, OracleOutcome, PathFixture, PoolData,
};
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path142603_v4v4v3_block25723658.json"
);

/// Override a single scalar field on a cloned `PoolData` (the fixture records
/// on-chain truth; env overrides probe hypotheses without editing the JSON).
fn with_liq(mut p: PoolData, liq: &str) -> PoolData {
    p.liquidity = Some(Amount(U256::from_str_radix(liq, 10).unwrap()));
    p
}

fn main() {
    let fx = PathFixture::load(FIXTURE_PATH).unwrap_or_else(|e| panic!("{e}"));

    // Env overrides.
    let v4_b_liq: Option<String> = std::env::var("FIXTURE_V4_B_LIQ").ok();
    let v4_a_proto: Option<u32> = std::env::var("FIXTURE_V4_A_PROTO_FEE")
        .ok()
        .map(|s| s.parse().unwrap());
    let v4_b_proto_fee: Option<u32> = std::env::var("FIXTURE_V4_B_PROTO_FEE")
        .ok()
        .map(|s| s.parse().unwrap());
    let v3_c_sqrt: Option<U256> = std::env::var("FIXTURE_V3_C_SQRT")
        .ok()
        .map(|s| s.parse().unwrap());
    let v3_c_tick: Option<i32> = std::env::var("FIXTURE_V3_C_TICK")
        .ok()
        .map(|s| s.parse().unwrap());

    let p_a = fx.pools["v4_a"].clone();
    let p_b = match &v4_b_liq {
        Some(l) => with_liq(fx.pools["v4_b"].clone(), l),
        None => fx.pools["v4_b"].clone(),
    };
    let p_c = fx.pools["v3_c"].clone();

    // On-chain scalars used by the fixture (before any override), for the header.
    println!(
        "=== path-142603 pool scalars (fixture @ block {:?}) ===",
        fx.target_block
    );
    for (n, p) in [("v4_a", &p_a), ("v4_b", &p_b), ("v3_c", &p_c)] {
        println!(
            "  {} sqrt={} liq={} tick={:?} proto_fee={:?}",
            n,
            p.sqrt_price_x96
                .map(|a| a.0.to_string())
                .unwrap_or_default(),
            p.liquidity.map(|a| a.0.to_string()).unwrap_or_default(),
            p.tick,
            p.protocol_fee,
        );
    }
    println!(
        "  FIXTURE_V4_B_LIQ={v4_b_liq:?} (None = on-chain 718152690765; 1018741430873 = solver stale)"
    );

    let engine = ArbitrageEngine::new();
    let a_id = register_v4_with(&mut engine.core().write(), &p_a, v4_a_proto)
        .unwrap_or_else(|e| panic!("{e}"));
    let b_id = register_v4_with(&mut engine.core().write(), &p_b, v4_b_proto_fee)
        .unwrap_or_else(|e| panic!("{e}"));
    let c_id = register_v3_with(&mut engine.core().write(), &p_c, v3_c_sqrt, v3_c_tick)
        .unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v4_a" => a_id,
            "v4_b" => b_id,
            "v3_c" => c_id,
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
            let matches = Some(r.optimal_input) == recorded_optimal && r.hop_outputs == recorded_hops;
            println!("  => {}", if matches { "MATCHES recorded solve (phantom reproduced)" } else { "DIFFERS from recorded solve" });

            println!("=== per-hop tier-3 oracle comparison (input-matched) ===");
            for (i, ph) in fx.path.iter().enumerate() {
                let pool = ph.pool.as_str();
                let zfo = ph.zero_for_one;
                let outcome = match pool {
                    "v3_c" => {
                        let st = build_v3_state(&p_c);
                        v3_hop_output(
                            &st,
                            p_c.fee_token0.unwrap(),
                            p_c.tick_spacing.unwrap(),
                            zfo,
                            r.consumed_inputs[i],
                        )
                    }
                    "v4_a" => {
                        let st = build_v4_state(&p_a);
                        v4_hop_output(
                            &st,
                            p_a.fee_currency0.unwrap(),
                            p_a.tick_spacing.unwrap(),
                            zfo,
                            r.consumed_inputs[i],
                        )
                    }
                    "v4_b" => {
                        let st = build_v4_state(&p_b);
                        v4_hop_output(
                            &st,
                            p_b.fee_currency0.unwrap(),
                            p_b.tick_spacing.unwrap(),
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
        None => println!("=== solver returned None for path {path_id} (no profitable input) — matches the TRUE on-chain state"),
    }
}
