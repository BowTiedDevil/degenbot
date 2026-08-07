//! Path-5000 V4-leg seeded-state gas probe (block 25704509).
//!
//! The decisive follow-up to `path5000_executor_payload`: that harness runs the
//! real `cmd_executor` against an **EmptyDB** (no pool code seeded), so a call
//! into the V4 PoolManager hits a non-contract account and the leg cannot be
//! reproduced. This harness instead deploys the **real v4-core `PoolManager`**
//! (via the committed `V4SwapOracleHarness` unlocker wrapper), seeds the
//! path-5000 V4 pool's storage slot-for-slot from the fixture (pool_id
//! `0x929b9b09…`, the single tracked band `[-257352, 35067]`, liquidity
//! `3186539294357237543`, protocol_fee `102425`, fee `100`, spacing `1`), and
//! drives the **recorded V4 swap** — `v4_input=15351327867212777`, `zfo=false`
//! (sell MATIC/currency1 buy UNI/currency0) — through `unlock`→`swap`→settle.
//!
//! ## What it answers
//!
//! The live halt was `[sim-fail] path=5000 … bucket=empty` with the deepest
//! PoolManager frame spending ~4.46M gas under the then-hard-coded
//! `INITIAL_EXECUTE_GAS = 5_000_000` execute ceiling (`degenbot-backrun-strategy/
//! simulator.rs`). The question is whether that halt is (a) a **genuine
//! liquidity / range-exhaustion verdict** on the real pool (v4_simulate_swap
//! fills the band 98% and any excess tips past tick 35067 into zero liquidity)
//! or (b) an **artifact of the 5M ceiling truncating real execution**. This
//! probe re-runs the SAME swap on the SAME seeded on-chain state at 5M vs 30M
//! and compares the verdict + BalanceDelta to the recorded solver output
//! (`v4_predicted_output=460882096151249`).
//!
//! If the swap fills at 30M but not 5M, the 5M ceiling is causal and raising it
//! un-halts the path. If it reverts at both, the halt tracks a real on-chain
//! infeasibility (the engine's solver/twin agrees: see the byte-exact solver
//! fixture, which computes `v4_hop_output == 460882096151249` from the
//! reconstructed state). Exit 0 = probe ran; the verdicts are printed for
//! inspection.
//!
//! Run standalone:
//! ```text
//! cargo run -p degenbot --example path5000_v4_gas_probe
//! ```
//! Optional: `FIXTURE_PATH=…` to point at a different capture, and
//! `GAS_5M=… GAS_30M=…` to override the two budgets (defaults 5_000_000 /
//! 30_000_000).

#![allow(clippy::doc_markdown, clippy::too_many_lines)]
#![allow(clippy::cast_possible_wrap, clippy::match_same_arms)]

use alloy::primitives::{aliases::I256, U160, U256};
use degenbot::investigation::{build_v4_state, real_oracle, PathFixture};
use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO;
use degenbot_pools::v4_state::v4_simulate_swap;
use degenbot_simulation::oracle::{self, Verdict};

const DEFAULT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
);

fn main() {
    let fixture_path =
        std::env::var("FIXTURE_PATH").unwrap_or_else(|_| DEFAULT_FIXTURE.to_string());
    let gas_5m = std::env::var("GAS_5M")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5_000_000u64);
    let gas_30m = std::env::var("GAS_30M")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000_000u64);

    let fx = PathFixture::load(&fixture_path).unwrap_or_else(|e| panic!("{e}"));
    let rec = &fx.recorded_solve;

    println!("=== path-5000 V4 seeded-state gas probe ===");
    println!(
        "block={:?} V4 pool_id={} fee={} spacing={} zfo={}",
        fx.target_block,
        fx.pools["v4"].pool_id.as_deref().unwrap_or(""),
        fx.pools["v4"].fee_currency0.unwrap(),
        fx.pools["v4"].tick_spacing.unwrap(),
        rec.v4_zero_for_one.unwrap(),
    );
    println!(
        "recorded: v4_input={} v4_predicted_output={} onchain={}",
        rec.v4_input.unwrap(),
        rec.v4_predicted_output.unwrap(),
        rec.v4_onchain.as_deref().unwrap_or("")
    );

    // Reconstruct the V4 state the solver/engine used.
    let state = build_v4_state(&fx.pools["v4"]);
    let ticks: Vec<i32> = fx.pools["v4"]
        .tick_data
        .keys()
        .map(|t| t.parse().unwrap())
        .collect();
    let tmin = *ticks.iter().min().unwrap();
    let tmax = *ticks.iter().max().unwrap();
    let cur_tick = fx.pools["v4"].tick.unwrap();
    println!(
        "V4 tracked band: {} ticks [{tmin},{tmax}] current={cur_tick} headroom-above={} liq={} protocol_fee={}",
        ticks.len(),
        tmax - cur_tick,
        state.liquidity,
        state.protocol_fee,
    );

    // The recorded V4 input (exact-in => negative amount), selling currency1
    // (MATIC) for currency0 (UNI), zfo=false.
    let v4_input = rec.v4_input.unwrap().0;
    // Optional solver-clamp: reduce the input by `CLAMP_INPUT` wei so the swap
    // is fed exactly (or below) the pool's max-convertible amount — proving the
    // leftover-input hypothesis: when input == capacity, the exact-in loop
    // terminates on amountRemaining==0 at the band boundary (no march, ~215k
    // gas) even with a MAX price limit.
    let clamp = std::env::var("CLAMP_INPUT")
        .ok()
        .and_then(|s| s.parse::<u128>().ok())
        .unwrap_or(0);
    let v4_input = v4_input.saturating_sub(U256::from(clamp));
    let amount_specified = I256::ZERO
        .checked_sub(I256::try_from(v4_input).expect("v4_input fits i256"))
        .expect("no underflow");
    let zfo = false;

    // Price limit mode: env-driven so we can compare band-top-bound vs a MAX
    // (uncapped) limit, which is what the live executor most plausibly passes
    // to PoolManager.swap (MIN/MAX_SQRT_PRICE per exploration-no-profit-crash.md
    // L308) — a MAX limit forces the swap loop to walk the tick-bitmap
    // word-by-word in the price direction (an SLOAD per word) rather than
    // stopping at the tracked band top.
    let sqrt_price_limit: U160 = if std::env::var("EXECUTOR_LIMIT").is_ok() {
        // EXACTLY what the live executor passes for zfo=false: `MAX_SQRT_RATIO - 1`
        // (the extreme bound — an unbounded price march in the buy direction).
        MAX_SQRT_RATIO - U160::from(1u64)
    } else if let Ok(lt) = std::env::var("LIMIT_TICK") {
        // Explicit numeric price-limit tick (int). Sweep this to characterize
        // the swap-loop's gas-vs-price-distance walk.
        let t = lt.parse::<i32>().expect("LIMIT_TICK must be an int");
        U160::from(get_sqrt_ratio_at_tick_internal(t).expect("limit sqrt"))
    } else if std::env::var("CAP_AT_BAND_TOP").is_ok() {
        // Decisive probe #1: cap the price limit AT the band top (tick 35067)
        // — where on-chain liquidity goes to zero.
        U160::from(get_sqrt_ratio_at_tick_internal(tmax).expect("limit sqrt"))
    } else if std::env::var("BAND_TOP_PLUS").is_ok() {
        // Decisive probe #2: cap the price limit just PAST the band top into
        // the first empty word, to see the loop walk a few empty words.
        U160::from(get_sqrt_ratio_at_tick_internal(tmax + 5_000).expect("limit sqrt"))
    } else {
        // Default: past the band with margin, well inside u160.
        U160::from(get_sqrt_ratio_at_tick_internal((tmax + 5000).min(800_000)).expect("limit sqrt"))
    };

    // Rust twin (what v4_simulate_swap says from the reconstructed state).
    let sim = v4_simulate_swap(
        &state,
        fx.pools["v4"].fee_currency0.unwrap(),
        fx.pools["v4"].tick_spacing.unwrap(),
        zfo,
        amount_specified,
        U256::from(sqrt_price_limit),
    );
    println!(
        "v4_simulate_swap -> {:?} (recorded solver output={})",
        sim,
        rec.v4_predicted_output.unwrap()
    );

    // Build a fresh EVM, deploy the real PoolManager via the harness wrapper.
    let probe = |gas: u64| -> (Verdict, (U256, U256), u64) {
        let fee = fx.pools["v4"].fee_currency0.unwrap();
        let spacing = fx.pools["v4"].tick_spacing.unwrap();
        let swap = real_oracle::drive_real_v4_swap(
            &state,
            fee,
            spacing,
            zfo,
            amount_specified,
            sqrt_price_limit,
            gas,
        );
        (swap.verdict, swap.delta, swap.gas_used)
    };

    for (label, gas) in [("5M", gas_5m), ("30M", gas_30m)] {
        let (verdict, (am0, am1), gas_used) = probe(gas);
        println!("--- swap @ {label} gas ({gas}) ---");
        match &verdict {
            Verdict::Accepted { .. } => {
                println!(
                    "  ACCEPTED -> BalanceDelta amount0(UNI)={} amount1(MATIC)={} | recorded V4 output={} | gas_used={}",
                    am0, am1, rec.v4_predicted_output.unwrap(), gas_used
                );
            }
            Verdict::Reverted(r) => {
                println!("  REVERTED (gas_used={gas_used}) -> {r:?}");
                if let Some(msg) = oracle::decode_error_string(r.as_ref()) {
                    println!("    decoded: {msg}");
                }
            }
            Verdict::Halted(h) => {
                println!("  HALTED (gas_used={gas_used}) -> {h}");
            }
        }
    }

    println!("\ndone.");
}
