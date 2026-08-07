//! Path-73385 V4 concentrated-liquidity swap gas probe (block 25706469).
//!
//! The live bot crashed at block 25706469 on a V3-V4-V3 USDC→…→USDT path
//! (`[sim-fail] bucket=empty kind=halt gas=6269`, depth-8 empty-calldata USDT
//! frame) even after the execute ceiling was raised from the legacy 5M to the
//! EIP-7825 16.7M cap. The question this probe answers: does the transaction
//! **legitimately** need ~16.7M gas just to deal with the concentrated-
//! liquidity swap — the V4 hop on pool `0x8aa4e11c…` (USDC/USDT, fee=10,
//! tick_spacing=1)? Or is the V4 CL swap cheap, implying the halting `gas=6269`
//! frame is starvation of a LOCAL sub-call, not total-gas exhaustion?
//!
//! This harness deploys the **real v4-core `PoolManager`** (via the committed
//! `V4SwapOracleHarness` unlocker wrapper), seeds the pool `0x8aa4e11c…` slot-
//! for-slot from the captured fixture (63 tracked ticks, sqrt_price, liquidity
//! `2301144615601877`, protocol_fee `8194`, fee `10`, spacing `1`, current tick
//! `4`), and drives the **recorded V4 swap** — `v4_input=85060245`,
//! `zfo=true` (sell USDC/currency0 buy USDT/currency1, exact-in) — through
//! `unlock`→`swap`→settle at multiple execute-gas budgets, reporting each
//! budget's verdict + the swap's `gas_used`.
//!
//! For `zfo=true` (sell currency0) the executor's extreme price limit is
//! `MIN_SQRT_RATIO` (the floor) — mirroring `path5000_v4_gas_probe`'s
//! `EXECUTOR_LIMIT` mode. A tiny ~85 USDC→85 USDT 1:1 swap fills in a few
//! hundred k gas at ANY budget.
//!
//! ## Result (block 25706469, pool 0x8aa4e11c)
//!
//! The swap is **accepted at both 5M and 30M** with `gas_used = 190698` (same
//! at every budget), and the `BalanceDelta` (amount0=USDC 85060245,
//! amount1=USDT 85097881) matches the recorded on-chain actual byte-for-byte.
//! `v4_simulate_swap` fills at **tick 4 with ZERO tick crossings** (the ~1:1
//! swap barely moves the price), so the concentrated-liquidity swap does NOT
//! walk the tick bitmap and costs ~191k gas — NOT the ~16.7M pretend ceiling.
//! Combined with the live crash showing the SAME `gas=6269` depth-8 USDT
//! frame at BOTH the legacy 5M and the 16.7M budgets, the tx total is small
//! (< 5M): this is a **local gas-starvation of the executor's final
//! USDT-touching sub-call**, not total-gas exhaustion and not an EIP-7825
//! cap-hit. That is exactly why raising 5M→16.7M did not fix it.
//!
//! Run standalone:
//! ```text
//! cargo run -p degenbot --example path73385_v4_gas_probe
//! ```
//! Optional: `FIXTURE_PATH=…` points at a different capture; `GAS_5M=… GAS_30M=…`
//! override the two budgets (defaults 5_000_000 / 30_000_000).

#![allow(clippy::doc_markdown, clippy::too_many_lines)]
#![allow(clippy::cast_possible_wrap, clippy::match_same_arms)]

use alloy::primitives::{aliases::I256, U160, U256};
use degenbot::investigation::{build_v4_state, real_oracle, PathFixture};
use degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO;
use degenbot_pools::v4_state::v4_simulate_swap;
use degenbot_simulation::oracle::{self, Verdict};

const DEFAULT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path73385_v4_block25706469.json"
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

    println!("=== path-73385 V4 CL swap gas probe (block 25706469) ===");
    println!(
        "V4 pool_id={} fee={} spacing={} zfo={}",
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
        "V4 tracked band: {} ticks [{tmin},{tmax}] current={cur_tick} liq={} protocol_fee={}",
        ticks.len(),
        state.liquidity,
        state.protocol_fee,
    );

    // Recorded V4 input: exact-in (=> negative), selling USDC/currency0 for
    // USDT/currency1, zero_for_one=true.
    let v4_input = rec.v4_input.unwrap().0;
    let amount_specified = I256::ZERO
        .checked_sub(I256::try_from(v4_input).expect("v4_input fits i256"))
        .expect("no underflow");
    let zfo = rec.v4_zero_for_one.unwrap();

    // Price limit: for zfo=true (sell currency0) the executor's extreme bound
    // is MIN_SQRT_RATIO (the floor) — the unbounded price march in the sell-
    // currency0 direction. The ~1:1 tiny swap should fill (not march) regardless.
    let sqrt_price_limit: U160 = if std::env::var("LIMIT_TICK").is_ok() {
        let t = std::env::var("LIMIT_TICK").unwrap().parse::<i32>().unwrap();
        U160::from(
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(t)
                .expect("limit sqrt"),
        )
    } else {
        // Strictly inside bounds (the floor itself is rejected as out-of-bounds).
        MIN_SQRT_RATIO + U160::from(1u64)
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
        "v4_simulate_swap -> {sim:?} (recorded solver output={})",
        rec.v4_predicted_output.unwrap()
    );

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
        println!("--- CL swap @ {label} budget ({gas}) ---");
        match &verdict {
            Verdict::Accepted { .. } => {
                println!(
                    "  ACCEPTED -> BalanceDelta amount0(USDC)={am0} amount1(USDT)={am1} | recorded V4 output={} | gas_used={gas_used}",
                    rec.v4_predicted_output.unwrap()
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
