//! Decisive offline experiment for ergo task `W2UWZO` — resolves the
//! "stale active state vs. compute_crossing residual" fork for the V4
//! CurrencyNotSettled `+1` divergence WITHOUT a live mainnet run.
//!
//! ## What this proves
//!
//! `docs/architecture/sim_v4_swap_step_rounding.md` (addendum) left two
//! residual suspects for the post-round-up-fix `+1` V4 hop over-prediction:
//!
//! 1. **Stale active state** — engine `V4PoolState` lags the solve-block RPC
//!    state the sim reads.
//! 2. **A residual in `compute_crossing` / `int_simulate_v3_swap`** — the
//!    solver's hand-rolled 2-range→N-range CL model diverges from V4's
//!    stepwise `compute_swap_step_v4` walk on a multi-tick crossing.
//!
//! This test feeds the **IDENTICAL** synthetic `V4PoolState` to BOTH:
//! - `degenbot_pools::v4_simulate_swap` — the byte-exact full-tick-walk the
//!   in-process revm sim's `actual` amount mirrors (it runs the real V4
//!   PoolManager bytecode; this Rust twin matches it, proven by the V3/V4
//!   Python `compute_swap_step` oracle suite).
//! - the solver's crossing path: `IntV3TickRangeSequence::compute_crossing`
//!   + `int_simulate_v3_swap` for the ending partial step — exactly what
//!   `int_simulate_mixed_path_n` assembles for a CL hop's `hop_outputs[i]`.
//!
//! Because the input state is byte-identical, ANY divergence is pure solver
//! math — hypothesis (2) — and stale state (1) is exonerated by construction
//! (there is no "freshness" to differ when both sides read the same struct).
//! Conversely, byte-exact parity across a liquidity×amount sweep means the
//! on-chain `+1` CANNOT originate in the solver math and must be stale state.
//!
//! The sequence is built via `V4PoolState::build_int_v4_sequence` (the
//! production solver intake path), so the experiment exercises the real
//! tick-bitmap → `IntV3TickRangeSequence` transformation, not a hand-built
//! fixture.

#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "60·i (i ≤ tick_count ≤ 8) fits i32"
)]

use std::collections::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_cl_math::cl_lib::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use degenbot_pools::v3_state::{PoolTickCoverage, V3SwapOutcome};
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;

use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

/// Sqrt-price limit that lets the walk cross every tick the input can reach
/// (V4 Pool.swap's `sqrtPriceLimit` = the MIN/MAX bound for the direction).
fn unbounded_limit(zero_for_one: bool) -> U256 {
    if zero_for_one {
        U256::from(MIN_SQRT_RATIO)
    } else {
        U256::from(MAX_SQRT_RATIO)
    }
}

/// Build a multi-tick V4 pool state at tick 0 (1:1 price), tick_spacing 60,
/// with initialized ticks every ±60 out to ±(tick_count) boundaries.
///
/// `liquidity_net` alternates `+L`/`-L` at successive initialized ticks so
/// the active liquidity toggles between `L` and `2L` across ranges — a
/// realistic multi-tick-crossing topology that exercises the
/// `compute_crossing` accumulator across range boundaries.
fn build_multi_tick_v4_state(
    base_liquidity: u128,
    tick_count: usize,
    zero_for_one: bool,
) -> V4PoolState {
    let sp_0 = U256::from(1u128) << 96;
    let liq_gross = U256::from(base_liquidity).to::<U128>();
    let net_pos = I256::try_from(i128::try_from(base_liquidity).unwrap()).unwrap();
    let net_neg = -net_pos;

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    // Place initialized ticks at ±60·i for i in 1..=tick_count.
    // Alternate the net so liquidity toggles L ↔ 2L across ranges.
    for i in 1..=tick_count {
        let tick = if zero_for_one {
            -60 * i as i32
        } else {
            60 * i as i32
        };
        let net = if i % 2 == 1 { net_pos } else { net_neg };
        tick_data.insert(
            tick,
            TickInfo {
                liquidity_gross: liq_gross,
                liquidity_net: net,
                block: 0,
            },
        );
    }

    let params = RegisterV4PoolParams {
        pool_manager: alloy::primitives::Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: alloy::primitives::Address::ZERO,
            currency1: alloy::primitives::Address::ZERO,
            fee: 3_000, // 0.3%
            tick_spacing: 60,
            hooks: alloy::primitives::Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: sp_0,
        liquidity: base_liquidity,
        tick: 0,
        tick_data,
        update_block: 0,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    // `from_params` is the production registration constructor (seeds
    // `known_bitmap_words` from tick_data keys, pins the snapshot seed).
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// The solver's CL-hop output for `amount_in` against a pre-built sequence —
/// the exact assembly `int_simulate_mixed_path_n` uses for a CL hop's
/// `hop_outputs[i]`: find the range `k` the input lands in (largest k with
/// `crossing_gross_input(k) ≤ amount_in`), then sum the crossing output +
/// the ending-range partial-step output.
fn solver_crossing_output(amount_in: U256, seq: &IntV3TickRangeSequence) -> Option<U256> {
    let n = seq.ranges.len();
    // Find k = the range index the swap terminates in.
    let mut chosen_k = 0usize;
    for k in 0..n {
        let crossing = seq.compute_crossing(k)?;
        if crossing.crossing_gross_input <= amount_in {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k)?;
    if amount_in < crossing.crossing_gross_input {
        // Input can't even reach range `chosen_k` — path exhausted (shouldn't
        // happen given the search above, but mirror the production guard).
        return Some(U256::ZERO);
    }
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    Some(crossing.crossing_output.saturating_add(ending.output))
}

/// Extract the exact-in token-OUT amount from a V4 swap outcome.
/// zfo exact-in → amount1 out; ofz exact-in → amount0 out.
fn v4_exact_in_output(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

/// Run the parity sweep for one (direction, liquidity, tick_count) config.
/// Asserts `v4_simulate_swap == solver_crossing_output` for every amount_in
/// whose value lands strictly INSIDE a known range interior (derived from the
/// sequence's own `crossing_gross_input(k)` so the input never exhausts the
/// initialized ticks into the unbounded tail liquidity the solver's bounded
/// sequence cannot model). Returns the first divergence (if any) for
/// diagnostics.
fn run_parity_sweep(zero_for_one: bool, base_liquidity: u128, tick_count: usize) -> Vec<String> {
    let state = build_multi_tick_v4_state(base_liquidity, tick_count, zero_for_one);
    let seq = state
        .build_int_v4_sequence(60, 3_000, zero_for_one, 10)
        .expect("V4 state builds a tick-range sequence");
    let limit = unbounded_limit(zero_for_one);

    // crossing_gross_input(k) = gross input to fully cross k boundaries into
    // range k's entry (k=0 is identity, gross_input=0).
    let n = seq.ranges.len();
    if n < 3 {
        return Vec::new();
    }
    let gins: Vec<U256> = (0..n)
        .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
        .collect();

    // Build amounts landing strictly inside each range interior:
    //   range 0:        gins[1] / U256::from(2u64)                     (before the first boundary)
    //   range k (1..n-1): gins[k] + (gins[k+1] - gins[k]) / 2
    //   last range (n-1): gins[n-1] + (gins[n-1] - gins[n-2]) / 2  (mirrors the
    //                    prior step's span — safely interior, never reaching
    //                    the final boundary's unbounded tail).
    let mut amounts: Vec<U256> = Vec::new();
    amounts.push(gins[1] / U256::from(2u64)); // range 0 interior
    for k in 1..n {
        let span = if k < n - 1 {
            gins[k + 1].saturating_sub(gins[k])
        } else {
            // last range: mirror the prior span
            gins[k].saturating_sub(gins[k - 1])
        };
        amounts.push(gins[k] + span / U256::from(2u64));
        // Boundary-adjacent + dust amounts — the rounding regime where the
        // zfo partial-step round-up bug surfaced. `gin(k) + δ` lands just
        // past the k-th boundary (tiny ending partial step); `gin(k+1) - δ`
        // lands just short of the next boundary (near-saturating ending
        // step). Both stress the per-step ceil/floor rounding that a `+1`
        // residual would live in.
        if k < n - 1 {
            for delta in [1u64, 2, 3, 7, 13] {
                amounts.push(gins[k].saturating_add(U256::from(delta)));
                amounts.push(gins[k + 1].saturating_sub(U256::from(delta)));
            }
        }
    }

    let mut failures = Vec::new();
    for amount_in_u256 in amounts {
        if amount_in_u256.is_zero() {
            continue;
        }
        // V4 exact-in: amount_specified < 0.
        let Some(amount_specified) = I256::try_from(amount_in_u256)
            .ok()
            .and_then(|v| I256::ZERO.checked_sub(v))
        else {
            continue;
        };
        let outcome = v4_simulate_swap(&state, 3_000, 60, zero_for_one, amount_specified, limit)
            .expect("v4_simulate_swap succeeds on a well-formed multi-tick state");

        let sim_out = v4_exact_in_output(&outcome, zero_for_one);
        let solver_out = solver_crossing_output(amount_in_u256, &seq)
            .expect("solver produces a crossing output");

        if sim_out != solver_out {
            let n_ranges = seq.ranges.len();
            let delta = if sim_out > solver_out {
                sim_out - solver_out
            } else {
                solver_out - sim_out
            };
            failures.push(format!(
                "DIVERGENCE dir={} L={} ticks={} amount_in={} sim={sim_out} solver={solver_out} \
                 delta={delta} n_ranges={n_ranges}",
                if zero_for_one { "zfo" } else { "ofz" },
                base_liquidity,
                tick_count,
                amount_in_u256,
            ));
        }
    }
    failures
}

#[test]
fn v4_crossing_solver_matches_v4_simulate_swap_across_liquidity_and_amounts() {
    // Realistic liquidity magnitudes for mature V3/V4 pools (L ≈ 1e13 to
    // 1e21 — the sweep that surfaced the zfo partial-step round-up bug in
    // `int_simulate_v3_swap_partial_step_zfo_matches_onchain_compute_swap_step_v3_sweep`).
    let liquidities: [u128; 5] = [
        10_000_000_000_000,            // 1e13
        1_000_000_000_000_000,         // 1e15
        100_000_000_000_000_000,       // 1e17
        10_000_000_000_000_000_000,    // 1e19
        1_000_000_000_000_000_000_000, // 1e21
    ];

    let mut failures = Vec::new();

    for &liq in &liquidities {
        // tick_count=5 → 5 ranges; amounts derived per-range stay strictly
        // inside the initialized ticks, so the solver's bounded sequence
        // covers the same territory v4_simulate_swap walks.
        for &ticks in &[5usize] {
            for &zfo in &[true, false] {
                failures.extend(run_parity_sweep(zfo, liq, ticks));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "V4 crossing solver diverges from v4_simulate_swap (hypothesis 2 confirmed — residual is \
         in compute_crossing/int_simulate_v3_swap, NOT stale state). First {} divergence(s):\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn v4_crossing_solver_matches_v4_simulate_swap_single_high_liquidity_corner() {
    // A targeted high-liquidity corner (the regime where the zfo partial-step
    // round-up surfaced) with a denser tick topology.
    let liq = 100_000_000_000_000_000_000u128; // 1e20
    let mut failures = Vec::new();
    for &zfo in &[true, false] {
        failures.extend(run_parity_sweep(zfo, liq, 5));
    }
    assert!(
        failures.is_empty(),
        "high-liquidity corner diverged:\n{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
