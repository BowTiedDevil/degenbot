#![expect(clippy::unwrap_used, clippy::panic, clippy::print_stdout)]
//! V3→V4→V2 fixture: Möbius solver replay of the path-110302 terminal-V2
//! `UniswapV2: K` failure (block 25711761), driven through the shared
//! `degenbot::investigation` toolkit.
//!
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)] // faithful mirror of the path5000 / fee1_v3v4v3 solver-fixture runners
//! Loads `tests/fixtures/path110302_v3v4v2_block25711761.json` and reconstructs
//! the V3-V4-V2 pools into `BotState`, runs the production Möbius solver, and —
//! per hop — feeds the solver's input into the tier-3-validated oracle twin
//! (V3 `v3_simulate_swap` / V4 `v4_simulate_swap` / V2 `getAmountOut`) to assert
//! each hop's input→output is correct at the solution. The terminal V2 hop is
//! then held up against the on-chain exact-**OUT** `swap(amount0Out=…)` that the
//! executor encoded, to expose the exact-in/exact-out rounding mismatch that
//! trips `UniswapV2: K`.
//!
//! Context (live `[sim-fail] path=110302 … bucket=UniswapV2: K`):
//! ```text
//!   hop0 V3 PancakeSwap  USDC/WETH  fee=100  zfo=false  (WETH→USDC)  matched
//!   hop1 V4              USDC/USDT  fee=8    zfo=true   (USDC→USDT)  matched=true
//!   hop2 V2 UniV2        WETH/USDT  fee=997/1000 zfo=false (USDT→WETH) ← K-REVERT
//! ```
//! recorded `[sim-diag]` optimal_input=30261840128124434 (WETH), hop_outputs=
//! [58199277 (USDC), 58233015 (USDT), 30263206881291235 (WETH)].
//!
//! The executor encodes the **terminal V2 swap as an exact-OUT** UniswapV2
//! `swap(amount0Out=30263206881291235, …)` (decoded from the live revert
//! calldata `0x022c0d9f…)`). For that exact-out the pair needs
//! `getAmountIn = 58233016` USDT, but only **58233015** USDT (what hop1
//! produced) is delivered → **1-wei short → `UniswapV2: K`**. The byte-exact
//! exact-in `getAmountOut(58233015)` at the on-chain reserves equals
//! **30263206361603722**, i.e. the recorded solver output (30263206881291235)
//! OVER-predicts by 519,687,513 wei — exactly `getAmountOut(58233016)`, one wei
//! more input than hop1 produced. So the solver's V2 output, fed straight into
//! the exact-out `amount0Out`, over-draws by the amount of 1 input wei.
//!
//! Exit 0 = each solver hop is byte-exact to its family oracle at the solver's
//! own consumed inputs (the V2 over-prediction reproduces, localizing it to the
//! solve/encode boundary). Exit 1 = RED if a solver hop diverges from its oracle
//! twin. Exit 2 = path not soluble from the reconstructed state.

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
    "/../../../tests/fixtures/path110302_v3v4v2_block25711761.json"
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
    // Hop indexes are fixed by the capture: hop0=V3, hop1=V4, hop2=V2.
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
    let pid0 = register_v3(&mut engine.core().write(), &fx.pools["v3_0"])
        .unwrap_or_else(|e| panic!("{e}"));
    let v4id =
        register_v4(&mut engine.core().write(), &fx.pools["v4"]).unwrap_or_else(|e| panic!("{e}"));
    let pid2 = register_v2(&mut engine.core().write(), &fx.pools["v2_2"])
        .unwrap_or_else(|e| panic!("{e}"));

    let mut by_idx = HashMap::new();
    for h in &fx.path {
        let pid = match h.pool.as_str() {
            "v3_0" => pid0,
            "v4" => v4id,
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

            // hop[0] V3 PancakeSwap USDC/WETH fee=100 spacing=1, zfo=false (WETH→USDC).
            let v3 = build_v3_state(&fx.pools["v3_0"]);
            let v3_oc = v3_hop_output(
                &v3,
                fx.pools["v3_0"].fee_token0.unwrap(),
                fx.pools["v3_0"].tick_spacing.unwrap(),
                hops[0].zero_for_one,
                solver_ins[0],
            );
            println!(
                "  hop[0] V3 in={} {}",
                solver_ins[0],
                display_check(&v3_oc, solver_outs[0])
            );

            // hop[1] V4 USDC/USDT fee=8 spacing=1, zfo=true (USDC→USDT).
            let v4 = build_v4_state(&fx.pools["v4"]);
            let fee = fx.pools["v4"].fee_currency0.unwrap();
            let spacing = fx.pools["v4"].tick_spacing.unwrap();
            let v4_oc = v4_hop_output(&v4, fee, spacing, hops[1].zero_for_one, solver_ins[1]);
            println!(
                "  hop[1] V4 in={} {}",
                solver_ins[1],
                display_check(&v4_oc, solver_outs[1])
            );

            // hop[2] V2 WETH/USDT, zfo=false (USDT→WETH): t0=WETH(reserve0 out),
            // t1=USDT(reserve1 in). V2_DEFAULT_FEE (997/1000) == this UniV2 0.30%.
            let v2 = &fx.pools["v2_2"];
            let r_in = v2.reserve1.unwrap().0; // USDT (token1, input)
            let r_out = v2.reserve0.unwrap().0; // WETH (token0, output)
            let v2_out = v2_get_amount_out(solver_ins[2], r_in, r_out, V2_DEFAULT_FEE);
            println!(
                "  hop[2] V2 in={} {}",
                solver_ins[2],
                display_check(&OracleOutcome::Ok(v2_out), solver_outs[2])
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

            let all_ok = [&v3_oc, &v4_oc, &OracleOutcome::Ok(v2_out)]
                .iter()
                .zip(&solver_outs)
                .all(|(o, s)| matches!(o, OracleOutcome::Ok(v) if v == s));
            if all_ok {
                println!(
                    "=> VERDICT: PASS on the solver — every hop is byte-exact to its oracle at \
                     the solver's own consumed inputs. The recorded terminal-V2 predicted \
                     ({rec_pred}) equals the exact-out `amount0Out` the executor encoded, and \
                     exceeds the byte-exact exact-in getAmountOut ({oracle_at_in}) by 519,687,513 \
                     wei (one input wei), so the exact-out swap needs 58233016 USDT but only \
                     58233015 is delivered -> UniswapV2: K. The over-prediction reproduces: it \
                     lives at the solve→encode boundary (recorder predicted == encoded amount0Out)."
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
