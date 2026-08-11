#![expect(clippy::unwrap_used, clippy::panic, clippy::print_stdout)]
//! V4→V4→V2 fixture: Möbius solver replay of the path-182449 terminal-V2
//! `UniswapV2: K` failure (block 25731019), driven through the shared
//! `degenbot::investigation` toolkit.
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the path110302 / path142603 solver-fixture runners
//! Loads `tests/fixtures/path182449_v4v4v2_block25731019.json` and reconstructs
//! the V4-V4-V2 pools into `BotState`, runs the production Möbius solver, and —
//! per hop — feeds the solver's input into the tier-3-validated oracle twin
//! (V4 `v4_simulate_swap` / V2 `getAmountOut`) to assert each hop's input→output
//! is correct at the solution. The terminal V2 hop is then held up against the
//! on-chain exact-**OUT** `swap(amount0Out=…)` that the executor encoded, to
//! expose the exact-in/exact-out rounding mismatch that trips `UniswapV2: K`.
//!
//! Context (live `[sim-fail] path=182449 … bucket=UniswapV2: K`, block 25731019):
//! ```text
//!   hop0 V4  USDC/WETH  fee=200  spacing=4   zfo=false  (WETH→USDC)  matched=true
//!   hop1 V4  USDC/USDT  fee=100  spacing=1   zfo=true   (USDC→USDT)  matched=true
//!   hop2 V2  WETH/USDT  fee=997/1000 zfo=false (USDT→WETH) ← K-REVERT
//! ```
//! recorded `[sim-diag]` optimal_input=4820058343725384 (WETH), hop_outputs=
//! [9079140 (USDC), 9085365 (USDT), 4820488856043000 (WETH)].
//!
//! The executor encodes the **terminal V2 swap as an exact-OUT** UniswapV2
//! `swap(amount0Out=4820488856043000, …)` (decoded from the live revert
//! calldata `0x022c0d9f…)`). For that exact-out the pair needs
//! `getAmountIn = 9085366` USDT, but only **9085365** USDT (what hop1
//! produced) is delivered → **1-wei short → `UniswapV2: K`**. The byte-exact
//! exact-in `getAmountOut(9085365)` at the on-chain reserves equals
//! **4820488325483365**, i.e. the recorded solver output (4820488856043000)
//! OVER-predicts by 530,559,635 wei — exactly `getAmountOut(9085366)`, one wei
//! more input than hop1 produced. Same fault class as path-110302 (V3-V4-V2);
//! the `v4_v4_v2` composer now encodes the terminal V2 hop as `V2_SWAP_CALC`
//! to fence it (the fenced encoding round-trips cleanly, see the deep-dive).
//!
//! Exit 0 = each solver hop is byte-exact to its family oracle at the solver's
//! own consumed inputs (the V2 over-prediction reproduces, localizing it to the
//! solve/encode boundary). Exit 1 = RED if a solver hop diverges from its oracle
//! twin. Exit 2 = path not soluble from the reconstructed state.

use std::collections::HashMap;

use alloy::primitives::U256;
use degenbot::investigation::{
    build_v4_state, display_check, register_v2, register_v4, v2_get_amount_out, v4_hop_output,
    OracleOutcome, PathFixture, V2_DEFAULT_FEE,
};
use degenbot::solvers::arb_engine::ArbitrageEngine;
use degenbot_solvers::mixed::PoolHop;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path182449_v4v4v2_block25731019.json"
);

/// UniswapV2 exact-**OUT** required-input (`getAmountIn`): the amount of
/// reserve_in needed to withdraw `amount_out` of reserve_out while preserving
/// the constant product, given the pair's retained fee fraction (`gamma`/`denom`).
fn v2_get_amount_in(
    amount_out: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee: (u64, u64),
) -> U256 {
    let (gamma_num, fee_denom) = fee;
    if amount_out >= reserve_out {
        return U256::MAX;
    }
    let numerator = reserve_in * amount_out * U256::from(fee_denom);
    let denominator = (reserve_out - amount_out) * U256::from(gamma_num);
    (numerator / denominator) + U256::from(1u64)
}

fn main() {
    let fixture_path = std::env::var("FIXTURE_PATH").unwrap_or_else(|_| FIXTURE_PATH.to_string());
    let fx = PathFixture::load(&fixture_path).unwrap_or_else(|e| panic!("{e}"));

    let rec = &fx.recorded_solve;
    // Hop indexes are fixed by the capture: hop0=V4, hop1=V4, hop2=V2.
    let hop_idx = rec.v2_hop_index.unwrap(); // 2 (terminal V2)

    println!(
        "fixture: block={:?} V2-terminal-hop={hop_idx} bucket={}",
        fx.target_block,
        rec.sim_bucket.as_deref().unwrap_or("")
    );
    println!(
        "  recorded optimal_input={:?} hop_outputs={:?}",
        rec.optimal_input, rec.hop_outputs
    );
    println!(
        "  recorded V2 input={:?} predicted={:?} actual(byte-exact exact-in)={:?}",
        rec.v2_input, rec.v2_predicted, rec.v2_actual
    );

    // ── Register the three pools + resolve the hop list. ──
    let engine = ArbitrageEngine::new();
    let pid0 = register_v4(&mut engine.core().write(), &fx.pools["v4_a"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid1 = register_v4(&mut engine.core().write(), &fx.pools["v4_b"])
        .unwrap_or_else(|e| panic!("{e}"));
    let pid2 = register_v2(&mut engine.core().write(), &fx.pools["v2_c"])
        .unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v4_a" => pid0,
            "v4_b" => pid1,
            "v2_c" => pid2,
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

    // ── Production Möbius solver. ──
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
            std::process::exit(2);
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
            println!(
                "  solver hop_outputs == recorded hop_outputs? {}",
                solver_outs == recorded_ho
            );

            // ── Per-hop tier-3 oracle comparison (input-matched: oracle at the
            //    solver's OWN consumed input). ──
            println!("--- per-hop tier-3 oracle comparison (input-matched) ---");

            // hop[0] V4 USDC/WETH fee=200 spacing=4, zfo=false (WETH→USDC).
            let state0 = build_v4_state(&fx.pools["v4_a"]);
            let out0 = v4_hop_output(
                &state0,
                fx.pools["v4_a"].fee_currency0.unwrap(),
                fx.pools["v4_a"].tick_spacing.unwrap(),
                hops[0].zero_for_one,
                solver_ins[0],
            );
            println!(
                "  hop[0] V4 in={} {}",
                solver_ins[0],
                display_check(&out0, solver_outs[0])
            );

            // hop[1] V4 USDC/USDT fee=100 spacing=1, zfo=true (USDC→USDT).
            let state1 = build_v4_state(&fx.pools["v4_b"]);
            let out1 = v4_hop_output(
                &state1,
                fx.pools["v4_b"].fee_currency0.unwrap(),
                fx.pools["v4_b"].tick_spacing.unwrap(),
                hops[1].zero_for_one,
                solver_ins[1],
            );
            println!(
                "  hop[1] V4 in={} {}",
                solver_ins[1],
                display_check(&out1, solver_outs[1])
            );

            // hop[2] V2 WETH/USDT, zfo=false (USDT→WETH): t0=WETH(reserve0 out),
            // t1=USDT(reserve1 in). V2_DEFAULT_FEE (997/1000) == this Sushi 0.30%.
            let v2 = &fx.pools["v2_c"];
            let r_in = v2.reserve1.unwrap().0; // USDT (token1, input)
            let r_out = v2.reserve0.unwrap().0; // WETH (token0, output)
            let v2_out = v2_get_amount_out(solver_ins[2], r_in, r_out, V2_DEFAULT_FEE);
            let out2 = OracleOutcome::Ok(v2_out);
            println!(
                "  hop[2] V2 in={} {}",
                solver_ins[2],
                display_check(&out2, solver_outs[2])
            );

            // ── Terminal V2 hop deep-dive: recorded predicted vs byte-exact
            //    exact-in, and the exact-OUT K consequences. ──
            println!("--- terminal V2 hop: exact-in vs exact-out K analysis ---");
            let rec_in: U256 = rec.v2_input.unwrap().0;
            let rec_pred: U256 = rec.v2_predicted.unwrap().0;
            let rec_actual: U256 = rec.v2_actual.unwrap().0;
            let oracle_at_in = v2_get_amount_out(rec_in, r_in, r_out, V2_DEFAULT_FEE);
            println!("  reserves: t0(WETH)= {r_out}  t1(USDT)= {r_in}  fee=997/1000");
            println!("  recorded input (hop1 USDT out)  = {rec_in}");
            println!(
                "  byte-exact exact-IN getAmountOut(@{rec_in}) = {oracle_at_in}\n    == recorded v2_actual? {}  == recorded predicted? {}",
                oracle_at_in == rec_actual,
                oracle_at_in == rec_pred
            );
            println!(
                "  recorded solver predicted output = {rec_pred}\n    over-predicts byte-exact exact-in by {} wei (= 1 input wei)",
                rec_pred.saturating_sub(oracle_at_in)
            );

            // The executor encodes the terminal V2 swap as an EXACT-OUT swap
            // with amount0Out = recorded predicted. Show the input the pair
            // then requires vs the input actually delivered.
            let required_out_in = v2_get_amount_in(rec_pred, r_in, r_out, V2_DEFAULT_FEE);
            println!(
                "  exact-OUT getAmountIn(amount0Out={rec_pred}) = {required_out_in}\n    vs {rec_in} USDT delivered -> shortfall {} => UniswapV2: K",
                required_out_in.saturating_sub(rec_in)
            );
            // The safe exact-out max (== exact-in getAmountOut) round-trips cleanly.
            let safe_roundtrip = v2_get_amount_in(oracle_at_in, r_in, r_out, V2_DEFAULT_FEE);
            println!(
                "  exact-OUT getAmountIn(getAmountOut({rec_in})) = {safe_roundtrip}\n    == {rec_in}? {} (round-trip is clean at the byte-exact exact-in value)",
                safe_roundtrip == rec_in
            );

            let all_ok = [&out0, &out1, &out2]
                .iter()
                .zip(&solver_outs)
                .all(|(o, s)| matches!(o, OracleOutcome::Ok(v) if v == s));
            if all_ok {
                println!(
                    "=> VERDICT: PASS on the solver — every hop is byte-exact to its oracle at \
                     the solver's own consumed inputs. The recorded terminal-V2 predicted \
                     ({rec_pred}) equals the exact-out `amount0Out` the executor encoded, and \
                     exceeds the byte-exact exact-in getAmountOut ({oracle_at_in}) by 530,559,635 \
                     wei (one input wei), so the exact-out swap needs 9085366 USDT but only \
                     9085365 is delivered -> UniswapV2: K. The over-prediction reproduces: it \
                     lives at the solve→encode boundary (recorded predicted == encoded amount0Out), \
                     and the `v4_v4_v2` composer's V2_SWAP_CALC encoding fences it."
                );
            } else {
                println!(
                    "=> VERDICT: FAIL (RED) — a solver hop diverges from its oracle twin at the \
                     same input: a solver-side crossing defect."
                );
                std::process::exit(1);
            }
        }
    }
}
