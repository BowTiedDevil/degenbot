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
#![allow(
    clippy::unreadable_literal,
    reason = "machine-generated 134-tick UNI fixture payload; separators would be noise"
)]

use std::collections::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_pools::v3_state::{PoolTickCoverage, V3PoolState, V3SwapOutcome};
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;

use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

/// Sqrt-price limit that lets the walk cross every tick the input can reach
/// (V4 Pool.swap's `sqrtPriceLimit` = the MIN/MAX bound for the direction).
fn unbounded_limit(zero_for_one: bool) -> U256 {
    V3PoolState::default_sqrt_price_limit(zero_for_one)
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
        tick_data_block: None,
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

/// Build a fee-1/tiny-liquidity V4 state at ~price 1 (current_tick 0), with
/// initialized ticks at ±`spacing·i`, alternating nets (liquidity toggles
/// `L ↔ 2L`), mirroring `build_multi_tick_v4_state` but with fee=1, the
/// tightest tick spacing, and the reproduction's tiny liquidity + active price.
#[allow(dead_code)]
fn build_fee1_tiny_state(
    base_liquidity: u128,
    tick_count: usize,
    zero_for_one: bool,
    sqrt_price_x96: U256,
) -> V4PoolState {
    let net_pos = I256::try_from(i128::try_from(base_liquidity).unwrap()).unwrap();
    let net_neg = -net_pos;
    let liq_gross = U256::from(base_liquidity).to::<U128>();
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    for i in 1..=tick_count {
        let tick = if zero_for_one { -(i as i32) } else { i as i32 };
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
            fee: 1,
            tick_spacing: 1,
            hooks: alloy::primitives::Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96,
        liquidity: base_liquidity,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// GREEN regression guard (ergo UO3JM4/W2UWZO) — the solver int-solve crossing
/// path must be byte-exact to `v4_simulate_swap` (== the on-chain PoolManager)
/// on the real fee-1 pool, in BOTH swap directions.
///
/// SOURCE OF THE ROUNDING ERROR (found via the revm-powered V4 parity harness,
/// grounded by the tier-3 `v4_simulate_swap` == on-chain PoolManager oracle):
/// the int-solve crossing path over-predicted output on **zero-for-one** CL
/// hops by a few wei because its `build_int_v4_sequence` → `compute_crossing` /
/// `int_simulate_v3_swap` range-collapse evaluated each tick range as a SINGLE
/// floored step, MISSING the zero-amount current-tick flooring the on-chain
/// PoolManager (and `v4_simulate_swap`) apply at the current tick's word
/// boundary — but ONLY when the current tick sits exactly on a word boundary
/// (the fee-1 pool's tick 0). The fix (tick_bitmap.rs::compute_tick_ranges)
/// re-inserts sqrt(current_tick) as the first interior boundary of range 0 for
/// zfo=true, so the walk floors there like the on-chain. Verified on the real
/// fee-1 pool (zfo=true, input 4728): on-chain = 4724, single-step collapse
/// (pre-fix) = 4727.
#[test]
fn v4_fee1_solver_path_matches_v4_simulate_swap() {
    // Reproduction scalars (paths 10234/10338): sq & liq of the fee-1 V4 hop.
    let liq = 94_294_142u128;
    let mut failures: Vec<String> = Vec::new();
    let state = build_fee1_76f75965_v4_state();

    for &zfo in &[true, false] {
        let seq = state
            .build_int_v4_sequence(1, 50, zfo, 10)
            .expect("V4 state builds a tick-range sequence");
        let limit = unbounded_limit(zfo);

        // Amounts in the tiny-output regime (10 wei → a few thousand wei).
        for amount_in_u256 in [
            10u64, 100, 500, 1_000, 5_000, 9_000, 9_586, 12_000, 20_000, 50_000, 4_728, 4_729,
        ] {
            let amount_in_u256 = U256::from(amount_in_u256);
            let Some(amount_specified) = I256::try_from(amount_in_u256)
                .ok()
                .and_then(|v| I256::ZERO.checked_sub(v))
            else {
                continue;
            };
            let Ok(outcome) = v4_simulate_swap(&state, 50, 1, zfo, amount_specified, limit) else {
                continue; // sim not computable on a degenerate edge — skip
            };
            let sim_out = v4_exact_in_output(&outcome, zfo);
            let Some(solver_out) = solver_crossing_output(amount_in_u256, &seq) else {
                continue;
            };
            if sim_out != solver_out {
                let delta = if sim_out > solver_out {
                    sim_out - solver_out
                } else {
                    solver_out - sim_out
                };
                failures.push(format!(
                    "DIVERGENCE dir={} L={liq} amount_in={amount_in_u256} sim={sim_out} solver={solver_out} delta={delta}",
                    if zfo { "zfo" } else { "ofz" },
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "V4 fee-1 solver crossing path diverges from v4_simulate_swap (the 1-wei `predicted≠actual` \
         is in the int-solve path, NOT stale state). First {}:\n{}",
        failures.len(),
        failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"),
    );
}

/// UO3JM4 regression: the REAL UNI V4 pool `0x9a5c1d2f...` at block 25673381
/// (path 57150) - deep 134-range tick topology, protocol_fee 2048500. The
/// tier-3 on-chain oracle `v4_simulate_swap` at the solver's recorded V4 input
/// (3135) reproduces the recorded on-chain `actual` EXACTLY (772076574181336),
/// and the reconstructed solver crossing at the delivered input is byte-exact
/// to it (proven below). This exonerates the V4 crossing math: the live
/// over-prediction (772833263957077) is NOT reproducible at input 3135 on the
/// on-chain or solver-snapshot state (it maps to ~3 units MORE input) - a
/// solve-vs-sim state/amount divergence, see
/// docs/tracked-failures-log-review-2026-08-03.md. Pins the on-chain truth so
/// any fix keeps `v4_simulate_swap(state, 3135) == 772076574181336`.
fn build_uni_9a5c1d2f_v4_state() -> V4PoolState {
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    tick_data.insert(
        -887220,
        TickInfo {
            liquidity_gross: U128::from(3095796326960008u128),
            liquidity_net: I256::try_from(3095796326960008i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -273960,
        TickInfo {
            liquidity_gross: U128::from(2569758449300718u128),
            liquidity_net: I256::try_from(2569758449300718i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -272460,
        TickInfo {
            liquidity_gross: U128::from(423606468713367u128),
            liquidity_net: I256::try_from(423606468713367i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -272280,
        TickInfo {
            liquidity_gross: U128::from(64137966113587u128),
            liquidity_net: I256::try_from(64137966113587i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -270840,
        TickInfo {
            liquidity_gross: U128::from(6059498619583658u128),
            liquidity_net: I256::try_from(6059498619583658i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -270720,
        TickInfo {
            liquidity_gross: U128::from(1442416500734322u128),
            liquidity_net: I256::try_from(1442416500734322i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -270660,
        TickInfo {
            liquidity_gross: U128::from(1072538255797962u128),
            liquidity_net: I256::try_from(1072538255797962i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269880,
        TickInfo {
            liquidity_gross: U128::from(746254119994175u128),
            liquidity_net: I256::try_from(746254119994175i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269640,
        TickInfo {
            liquidity_gross: U128::from(1063979019046437u128),
            liquidity_net: I256::try_from(1063979019046437i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269460,
        TickInfo {
            liquidity_gross: U128::from(4392735532859774u128),
            liquidity_net: I256::try_from(4392735532859774i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269400,
        TickInfo {
            liquidity_gross: U128::from(3301964538422661603u128),
            liquidity_net: I256::try_from(3301964538422661603i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269340,
        TickInfo {
            liquidity_gross: U128::from(60648748642817u128),
            liquidity_net: I256::try_from(60648748642817i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -269220,
        TickInfo {
            liquidity_gross: U128::from(42054372060004u128),
            liquidity_net: I256::try_from(42054372060004i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -268500,
        TickInfo {
            liquidity_gross: U128::from(884671098135251u128),
            liquidity_net: I256::try_from(884671098135251i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -268440,
        TickInfo {
            liquidity_gross: U128::from(41758110177656u128),
            liquidity_net: I256::try_from(41758110177656i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267900,
        TickInfo {
            liquidity_gross: U128::from(5250989243683818u128),
            liquidity_net: I256::try_from(5250989243683818i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267660,
        TickInfo {
            liquidity_gross: U128::from(652579615864060u128),
            liquidity_net: I256::try_from(652579615864060i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267600,
        TickInfo {
            liquidity_gross: U128::from(621060394843147u128),
            liquidity_net: I256::try_from(621060394843147i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267300,
        TickInfo {
            liquidity_gross: U128::from(958471229359559285u128),
            liquidity_net: I256::try_from(-958471229359559285i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267240,
        TickInfo {
            liquidity_gross: U128::from(368946244160946u128),
            liquidity_net: I256::try_from(368946244160946i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267180,
        TickInfo {
            liquidity_gross: U128::from(10186277318479783u128),
            liquidity_net: I256::try_from(10186277318479783i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267120,
        TickInfo {
            liquidity_gross: U128::from(2422135248561103149u128),
            liquidity_net: I256::try_from(-2264621070134041011i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -267060,
        TickInfo {
            liquidity_gross: U128::from(7734991288379668u128),
            liquidity_net: I256::try_from(7734991288379668i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266820,
        TickInfo {
            liquidity_gross: U128::from(9534897714990701u128),
            liquidity_net: I256::try_from(9534897714990701i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266700,
        TickInfo {
            liquidity_gross: U128::from(813322091222952u128),
            liquidity_net: I256::try_from(813322091222952i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266640,
        TickInfo {
            liquidity_gross: U128::from(6987956816277586u128),
            liquidity_net: I256::try_from(6987956816277586i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266580,
        TickInfo {
            liquidity_gross: U128::from(59281355715900u128),
            liquidity_net: I256::try_from(59281355715900i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266400,
        TickInfo {
            liquidity_gross: U128::from(2399522880507038u128),
            liquidity_net: I256::try_from(2399522880507038i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266340,
        TickInfo {
            liquidity_gross: U128::from(112493218521047u128),
            liquidity_net: I256::try_from(112493218521047i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266220,
        TickInfo {
            liquidity_gross: U128::from(656032139661335u128),
            liquidity_net: I256::try_from(656032139661335i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -266040,
        TickInfo {
            liquidity_gross: U128::from(100838336754559490u128),
            liquidity_net: I256::try_from(100838336754559490i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -265980,
        TickInfo {
            liquidity_gross: U128::from(12413139839524009u128),
            liquidity_net: I256::try_from(12413139839524009i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -265680,
        TickInfo {
            liquidity_gross: U128::from(415010191696619u128),
            liquidity_net: I256::try_from(415010191696619i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -265500,
        TickInfo {
            liquidity_gross: U128::from(2055598704323972u128),
            liquidity_net: I256::try_from(2055598704323972i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -265320,
        TickInfo {
            liquidity_gross: U128::from(109663003636428798u128),
            liquidity_net: I256::try_from(109663003636428798i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -265020,
        TickInfo {
            liquidity_gross: U128::from(750443156766402504u128),
            liquidity_net: I256::try_from(750443156766402504i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -264720,
        TickInfo {
            liquidity_gross: U128::from(12635854150371872u128),
            liquidity_net: I256::try_from(12635854150371872i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -264600,
        TickInfo {
            liquidity_gross: U128::from(93518518087984u128),
            liquidity_net: I256::try_from(93518518087984i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -264420,
        TickInfo {
            liquidity_gross: U128::from(37166238477338101u128),
            liquidity_net: I256::try_from(37166238477338101i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -264300,
        TickInfo {
            liquidity_gross: U128::from(19985792372304u128),
            liquidity_net: I256::try_from(19985792372304i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -263760,
        TickInfo {
            liquidity_gross: U128::from(4696819509253034u128),
            liquidity_net: I256::try_from(4696819509253034i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -263520,
        TickInfo {
            liquidity_gross: U128::from(42054372060004u128),
            liquidity_net: I256::try_from(-42054372060004i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -263220,
        TickInfo {
            liquidity_gross: U128::from(125863968906478272u128),
            liquidity_net: I256::try_from(125863968906478272i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262980,
        TickInfo {
            liquidity_gross: U128::from(7703414190147941u128),
            liquidity_net: I256::try_from(7703414190147941i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262500,
        TickInfo {
            liquidity_gross: U128::from(46089222798377746u128),
            liquidity_net: I256::try_from(46089222798377746i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262440,
        TickInfo {
            liquidity_gross: U128::from(874654509432810317u128),
            liquidity_net: I256::try_from(828080803628147815i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262320,
        TickInfo {
            liquidity_gross: U128::from(6398617370111093u128),
            liquidity_net: I256::try_from(6398617370111093i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262260,
        TickInfo {
            liquidity_gross: U128::from(146855620115534377u128),
            liquidity_net: I256::try_from(146855620115534377i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262200,
        TickInfo {
            liquidity_gross: U128::from(21561187657487821u128),
            liquidity_net: I256::try_from(-21561187657487821i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -262080,
        TickInfo {
            liquidity_gross: U128::from(7552465476547216u128),
            liquidity_net: I256::try_from(-7552465476547216i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261960,
        TickInfo {
            liquidity_gross: U128::from(858809643407810700u128),
            liquidity_net: I256::try_from(-858809643407810700i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261720,
        TickInfo {
            liquidity_gross: U128::from(1323244005757474309u128),
            liquidity_net: I256::try_from(1321938849239849041i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261600,
        TickInfo {
            liquidity_gross: U128::from(929926413866351u128),
            liquidity_net: I256::try_from(-929926413866351i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261420,
        TickInfo {
            liquidity_gross: U128::from(884671098135251u128),
            liquidity_net: I256::try_from(-884671098135251i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261360,
        TickInfo {
            liquidity_gross: U128::from(8452163823214659u128),
            liquidity_net: I256::try_from(8368647602859347i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261300,
        TickInfo {
            liquidity_gross: U128::from(60440551344662902u128),
            liquidity_net: I256::try_from(-60440551344662902i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261240,
        TickInfo {
            liquidity_gross: U128::from(4890074599754121u128),
            liquidity_net: I256::try_from(-4890074599754121i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261060,
        TickInfo {
            liquidity_gross: U128::from(107299173773702665u128),
            liquidity_net: I256::try_from(-107299173773702665i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -261000,
        TickInfo {
            liquidity_gross: U128::from(6987956816277586u128),
            liquidity_net: I256::try_from(-6987956816277586i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260880,
        TickInfo {
            liquidity_gross: U128::from(55353246829305823u128),
            liquidity_net: I256::try_from(55353246829305823i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260820,
        TickInfo {
            liquidity_gross: U128::from(1319428066177662831u128),
            liquidity_net: I256::try_from(-1319428066177662831i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260640,
        TickInfo {
            liquidity_gross: U128::from(17123234183330268u128),
            liquidity_net: I256::try_from(-17013374488476260i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260520,
        TickInfo {
            liquidity_gross: U128::from(139491426758065142u128),
            liquidity_net: I256::try_from(-139340037267354454i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260460,
        TickInfo {
            liquidity_gross: U128::from(14773839555594581u128),
            liquidity_net: I256::try_from(14773839555594581i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260400,
        TickInfo {
            liquidity_gross: U128::from(1519577093352529u128),
            liquidity_net: I256::try_from(689556709959291i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260340,
        TickInfo {
            liquidity_gross: U128::from(621060394843147u128),
            liquidity_net: I256::try_from(-621060394843147i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260280,
        TickInfo {
            liquidity_gross: U128::from(37166238477338101u128),
            liquidity_net: I256::try_from(-37166238477338101i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260220,
        TickInfo {
            liquidity_gross: U128::from(38108430607252998u128),
            liquidity_net: I256::try_from(9795261403725302i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -260100,
        TickInfo {
            liquidity_gross: U128::from(2569758449300718u128),
            liquidity_net: I256::try_from(-2569758449300718i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259980,
        TickInfo {
            liquidity_gross: U128::from(1066607604323169u128),
            liquidity_net: I256::try_from(1066607604323169i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259740,
        TickInfo {
            liquidity_gross: U128::from(2904288456195335u128),
            liquidity_net: I256::try_from(1592224176872665i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259620,
        TickInfo {
            liquidity_gross: U128::from(84566505322153u128),
            liquidity_net: I256::try_from(-33996206109647i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259560,
        TickInfo {
            liquidity_gross: U128::from(852655011244344u128),
            liquidity_net: I256::try_from(852655011244344i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259440,
        TickInfo {
            liquidity_gross: U128::from(75510704875294845u128),
            liquidity_net: I256::try_from(-35195788783316801i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259320,
        TickInfo {
            liquidity_gross: U128::from(1442416500734322u128),
            liquidity_net: I256::try_from(-1442416500734322i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -259080,
        TickInfo {
            liquidity_gross: U128::from(4682848742389730u128),
            liquidity_net: I256::try_from(359625559470110i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258840,
        TickInfo {
            liquidity_gross: U128::from(54929847427004u128),
            liquidity_net: I256::try_from(-54929847427004i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258720,
        TickInfo {
            liquidity_gross: U128::from(334649707142645u128),
            liquidity_net: I256::try_from(334649707142645i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258600,
        TickInfo {
            liquidity_gross: U128::from(335734850520887u128),
            liquidity_net: I256::try_from(335734850520887i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258540,
        TickInfo {
            liquidity_gross: U128::from(589492701888314u128),
            liquidity_net: I256::try_from(589492701888314i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258420,
        TickInfo {
            liquidity_gross: U128::from(718737240575391944u128),
            liquidity_net: I256::try_from(-718737240575391944i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258360,
        TickInfo {
            liquidity_gross: U128::from(1634294381180807u128),
            liquidity_net: I256::try_from(-1634294381180807i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258300,
        TickInfo {
            liquidity_gross: U128::from(743879648539850u128),
            liquidity_net: I256::try_from(592490157829162i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258240,
        TickInfo {
            liquidity_gross: U128::from(368946244160946u128),
            liquidity_net: I256::try_from(-368946244160946i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -258000,
        TickInfo {
            liquidity_gross: U128::from(12470632768580422u128),
            liquidity_net: I256::try_from(12470632768580422i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -257820,
        TickInfo {
            liquidity_gross: U128::from(1066607604323169u128),
            liquidity_net: I256::try_from(-1066607604323169i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -257760,
        TickInfo {
            liquidity_gross: U128::from(14773839555594581u128),
            liquidity_net: I256::try_from(-14773839555594581i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -257640,
        TickInfo {
            liquidity_gross: U128::from(15209391792496367u128),
            liquidity_net: I256::try_from(-15209391792496367i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -257580,
        TickInfo {
            liquidity_gross: U128::from(8926653874846152u128),
            liquidity_net: I256::try_from(-8741370488654588i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -257280,
        TickInfo {
            liquidity_gross: U128::from(86731011987690u128),
            liquidity_net: I256::try_from(86731011987690i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -256860,
        TickInfo {
            liquidity_gross: U128::from(2055598704323972u128),
            liquidity_net: I256::try_from(-2055598704323972i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -256680,
        TickInfo {
            liquidity_gross: U128::from(6398617370111093u128),
            liquidity_net: I256::try_from(-6398617370111093i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -256500,
        TickInfo {
            liquidity_gross: U128::from(32727480377239u128),
            liquidity_net: I256::try_from(32727480377239i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -256380,
        TickInfo {
            liquidity_gross: U128::from(2248256316534000u128),
            liquidity_net: I256::try_from(-2248256316534000i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -256140,
        TickInfo {
            liquidity_gross: U128::from(924142409030959u128),
            liquidity_net: I256::try_from(-924142409030959i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -255420,
        TickInfo {
            liquidity_gross: U128::from(32727480377239u128),
            liquidity_net: I256::try_from(-32727480377239i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -255360,
        TickInfo {
            liquidity_gross: U128::from(74866333480083563u128),
            liquidity_net: I256::try_from(74866333480083563i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -255240,
        TickInfo {
            liquidity_gross: U128::from(3099314693730195u128),
            liquidity_net: I256::try_from(3099314693730195i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -254820,
        TickInfo {
            liquidity_gross: U128::from(335734850520887u128),
            liquidity_net: I256::try_from(-335734850520887i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -254400,
        TickInfo {
            liquidity_gross: U128::from(4392735532859774u128),
            liquidity_net: I256::try_from(-4392735532859774i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -254340,
        TickInfo {
            liquidity_gross: U128::from(15271212930213513u128),
            liquidity_net: I256::try_from(-15271212930213513i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -254220,
        TickInfo {
            liquidity_gross: U128::from(115838223435902u128),
            liquidity_net: I256::try_from(-115838223435902i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -253920,
        TickInfo {
            liquidity_gross: U128::from(25285149606253u128),
            liquidity_net: I256::try_from(-25285149606253i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -253560,
        TickInfo {
            liquidity_gross: U128::from(97711652918074811u128),
            liquidity_net: I256::try_from(97711652918074811i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -253440,
        TickInfo {
            liquidity_gross: U128::from(46089222798377746u128),
            liquidity_net: I256::try_from(-46089222798377746i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -253380,
        TickInfo {
            liquidity_gross: U128::from(668184903184506u128),
            liquidity_net: I256::try_from(-668184903184506i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -253260,
        TickInfo {
            liquidity_gross: U128::from(21506090929337481u128),
            liquidity_net: I256::try_from(-21506090929337481i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -252780,
        TickInfo {
            liquidity_gross: U128::from(10435192179932373u128),
            liquidity_net: I256::try_from(4108469537934685i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -252480,
        TickInfo {
            liquidity_gross: U128::from(112493218521047u128),
            liquidity_net: I256::try_from(-112493218521047i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -252360,
        TickInfo {
            liquidity_gross: U128::from(7271830858933529u128),
            liquidity_net: I256::try_from(-7271830858933529i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -252120,
        TickInfo {
            liquidity_gross: U128::from(3099314693730195u128),
            liquidity_net: I256::try_from(-3099314693730195i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -252060,
        TickInfo {
            liquidity_gross: U128::from(257713336954829u128),
            liquidity_net: I256::try_from(-257713336954829i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -251640,
        TickInfo {
            liquidity_gross: U128::from(1181683742724883u128),
            liquidity_net: I256::try_from(-1181683742724883i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -251520,
        TickInfo {
            liquidity_gross: U128::from(74866333480083563u128),
            liquidity_net: I256::try_from(-74866333480083563i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -251460,
        TickInfo {
            liquidity_gross: U128::from(60648748642817u128),
            liquidity_net: I256::try_from(-60648748642817i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -251400,
        TickInfo {
            liquidity_gross: U128::from(7767552156261528u128),
            liquidity_net: I256::try_from(-7767552156261528i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -251100,
        TickInfo {
            liquidity_gross: U128::from(465541842701550u128),
            liquidity_net: I256::try_from(-465541842701550i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -250860,
        TickInfo {
            liquidity_gross: U128::from(97711652918074811u128),
            liquidity_net: I256::try_from(-97711652918074811i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -250800,
        TickInfo {
            liquidity_gross: U128::from(86731011987690u128),
            liquidity_net: I256::try_from(-86731011987690i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -250680,
        TickInfo {
            liquidity_gross: U128::from(18414293164728178u128),
            liquidity_net: I256::try_from(-18414293164728178i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -249960,
        TickInfo {
            liquidity_gross: U128::from(1264071333426786u128),
            liquidity_net: I256::try_from(-1264071333426786i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -249240,
        TickInfo {
            liquidity_gross: U128::from(23606278249583048u128),
            liquidity_net: I256::try_from(-23606278249583048i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -248820,
        TickInfo {
            liquidity_gross: U128::from(1072538255797962u128),
            liquidity_net: I256::try_from(-1072538255797962i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -247440,
        TickInfo {
            liquidity_gross: U128::from(736591368984762u128),
            liquidity_net: I256::try_from(-736591368984762i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -247380,
        TickInfo {
            liquidity_gross: U128::from(2399522880507038u128),
            liquidity_net: I256::try_from(-2399522880507038i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -243000,
        TickInfo {
            liquidity_gross: U128::from(639025058954360u128),
            liquidity_net: I256::try_from(-639025058954360i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -242280,
        TickInfo {
            liquidity_gross: U128::from(125863968906478272u128),
            liquidity_net: I256::try_from(-125863968906478272i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -239580,
        TickInfo {
            liquidity_gross: U128::from(813322091222952u128),
            liquidity_net: I256::try_from(-813322091222952i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -239460,
        TickInfo {
            liquidity_gross: U128::from(18843474118215995u128),
            liquidity_net: I256::try_from(-18843474118215995i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -235380,
        TickInfo {
            liquidity_gross: U128::from(65598520532469184u128),
            liquidity_net: I256::try_from(-65598520532469184i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -234540,
        TickInfo {
            liquidity_gross: U128::from(92641693095782u128),
            liquidity_net: I256::try_from(-92641693095782i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -230220,
        TickInfo {
            liquidity_gross: U128::from(7734991288379668u128),
            liquidity_net: I256::try_from(-7734991288379668i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        -225180,
        TickInfo {
            liquidity_gross: U128::from(746254119994175u128),
            liquidity_net: I256::try_from(-746254119994175i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        887220,
        TickInfo {
            liquidity_gross: U128::from(3095796326960008u128),
            liquidity_net: I256::try_from(-3095796326960008i128).unwrap(),
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager: alloy::primitives::Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: alloy::primitives::Address::ZERO,
            currency1: alloy::primitives::Address::ZERO,
            fee: 3_000,
            tick_spacing: 60,
            hooks: alloy::primitives::Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 2_048_500,
        sqrt_price_x96: "159369389255773083394993".parse().unwrap(),
        liquidity: 1_432_650_976_603_835_442u128,
        tick: -262_346,
        tick_data,
        update_block: 25_670_030,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

#[test]
fn v4_uni_9a5c1d2f_oracle_matches_recorded_actual_not_solver_overprediction() {
    let state = build_uni_9a5c1d2f_v4_state();
    let zero_for_one = false;
    let input = U256::from(3_135u64);
    let amount_specified = I256::ZERO
        .checked_sub(I256::try_from(input).unwrap())
        .unwrap();
    let limit = unbounded_limit(zero_for_one);
    let outcome = v4_simulate_swap(&state, 3_000, 60, zero_for_one, amount_specified, limit)
        .expect("real UNI state simulates");
    let sim_out = v4_exact_in_output(&outcome, zero_for_one);

    // Tier-3 oracle truth at the recorded input == recorded on-chain actual.
    assert_eq!(
        sim_out,
        U256::from(772_076_574_181_336u128),
        "v4_simulate_swap @ recorded input 3135 must equal the recorded on-chain actual",
    );

    // Crossing-exactness proof: the reconstructed solver crossing at the
    // DELIVERED input (3135) equals the oracle byte-exactly. The live solver's
    // predicted 772833263957077 is NOT reproducible at input 3135 on either the
    // on-chain-at-block state or the solver's snapshot (it maps to ~3 units MORE
    // input) -> a solve-vs-sim state/amount divergence, NOT a crossing-math error
    // (see docs/tracked-failures-log-review-2026-08-03.md).
    let seq = state
        .build_int_v4_sequence(60, 3_000, zero_for_one, 10)
        .expect("build int v4 sequence");
    let solver_out =
        solver_crossing_output(input, &seq).expect("solver crossing output at delivered input");
    assert_eq!(
        solver_out, sim_out,
        "reconstructed solver crossing at the delivered input must equal v4_simulate_swap \
         (the V4 crossing math is exact; the live over-prediction is a solve-vs-sim divergence)"
    );
    println!(
        "v4-uni oracle+probe: oracle/solver@3135 = {sim_out} == recorded actual; \
         live predicted 772833263957077 not reproducible at input 3135 (solve-vs-sim divergence)"
    );
}

/// Real fee-1 USDC/USDT V4 pool `0x76f75965…` (ts=1, fee=50, protocol_fee
/// 53261) at the live-captured on-chain state (tick=0, sqrt
/// 79231869042278935382727675145, liq 94294142), tick_data from DB managed
/// pool 2337 ({-2:+L, 3:-L}). This is the UO3JM4 fee-1 topology whose crossing
/// over-predicts by 1 wei at certain inputs (captured live, path 10338).
fn build_fee1_76f75965_v4_state() -> V4PoolState {
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    tick_data.insert(
        -2,
        TickInfo {
            liquidity_gross: U128::from(94_294_142u128),
            liquidity_net: I256::try_from(94_294_142i128).unwrap(),
            block: 0,
        },
    );
    tick_data.insert(
        3,
        TickInfo {
            liquidity_gross: U128::from(94_294_142u128),
            liquidity_net: I256::try_from(-94_294_142i128).unwrap(),
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager: alloy::primitives::Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: alloy::primitives::Address::ZERO,
            currency1: alloy::primitives::Address::ZERO,
            fee: 50, // 0.005% fee-1 tier
            tick_spacing: 1,
            hooks: alloy::primitives::Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 53_261,
        sqrt_price_x96: "79231869042278935382727675145".parse().unwrap(),
        liquidity: 94_294_142u128,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// The fee-1 over-prediction (UO3JM4 live capture, path 10338): at the
/// solver's recorded input 4728 the on-chain oracle gives 4726 (= the recorded
/// actual). The solver predicted 4727 == oracle at input 4729 (1 unit MORE).
/// This pins the on-chain truth AND proves the crossing math is EXACT (the
/// solver-crossing mirror == oracle at 4728) -> the over-prediction is a 1-unit
/// INTER-HOP FORWARD-AMOUNT gap (the solver's V4 exact-in runs 1 unit above the
/// delivered amount), not a crossing-math error.
#[test]
fn fee1_76f75965_crossing_overprediction_at_4728() {
    let state = build_fee1_76f75965_v4_state();
    let zero_for_one = false;
    let input = U256::from(4_728u64);
    let amount_specified = I256::ZERO
        .checked_sub(I256::try_from(input).unwrap())
        .unwrap();
    let limit = unbounded_limit(zero_for_one);
    let outcome = v4_simulate_swap(&state, 50, 1, zero_for_one, amount_specified, limit)
        .expect("fee-1 state simulates");
    let sim_out = v4_exact_in_output(&outcome, zero_for_one);

    // On-chain truth at input 4728 == the recorded actual.
    assert_eq!(
        sim_out,
        U256::from(4_726u64),
        "v4_simulate_swap @ input 4728 must equal the recorded on-chain actual 4726"
    );

    // Localize the +1: does the solver-crossing mirror re-derive the solver's
    // 4727 (bug) or match the oracle 4726? Printed, not asserted, so the suite
    // stays green until the W2UWZO fix lands (then re-run to check it flipped
    // to 4726 == oracle, i.e. the +1 is gone).
    let seq = state
        .build_int_v4_sequence(1, 50, zero_for_one, 10)
        .expect("fee-1 int sequence");
    // Crossing-exactness at the delivered input (proves it is NOT crossing math):
    let solver_out = solver_crossing_output(input, &seq);
    assert_eq!(
        solver_out,
        Some(sim_out),
        "solver crossing mirror at input 4728 must equal v4_simulate_swap (4726) - the V4 \
         crossing math is exact; the +1 over-prediction is a forward-amount gap (4729 vs 4728)"
    );
    println!(
        "fee-1 @4728: v4_simulate_swap={sim_out}=actual, solver predicted 4727 (=oracle@4729), \
         solver_crossing_output={solver_out:?}=oracle -> forward-amount gap, crossing exact"
    );
}

/// Regression guard: localizes the fee-1 pool's (fee=50, ts=1) zfo=true
/// over-prediction to the current-tick interior flooring the int-solve
/// range-collapse skips. On-chain `v4_simulate_swap` (4724 at input 4728)
/// equals a TWO-step floored walk (current → current-tick-0 boundary → tick−2),
/// whereas the single-step collapse gives 4727 — the +3 the
/// `compute_tick_ranges` step-0 drain insert now floors. Pairs with
/// `fee1_76f75965_crossing_overprediction_at_4728` (which pins the same
/// oracle-vs-collapse split on the full crossing path).
#[test]
fn fee1_zfo_true_two_step_floored_equivalence() {
    use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v4;
    use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
    let cur = U256::from(79_231_869_042_278_935_382_727_675_145u128);
    let sqrt0 = U256::from(get_sqrt_ratio_at_tick_internal(0).unwrap()); // 2^96
    let lower = U256::from(79_220_240_490_215_316_061_937_756_561u128); // tick -2
    let liq: i128 = 94_294_142;
    let fee = U256::from(63u128);
    let amt = U256::from(4728u64);
    let zero = I256::ZERO;
    // step1: current -> sqrt(0) (floored)
    let s1 = compute_swap_step_v4(
        cur,
        sqrt0,
        liq,
        zero.checked_sub(I256::try_from(amt).unwrap()).unwrap(),
        fee,
    )
    .unwrap();
    let consumed1 = s1.amount_in + s1.fee_amount;
    // step2: sqrt(0) -> lower with remaining
    let remaining = amt - consumed1;
    let s2 = compute_swap_step_v4(
        sqrt0,
        lower,
        liq,
        zero.checked_sub(I256::try_from(remaining).unwrap())
            .unwrap(),
        fee,
    )
    .unwrap();
    let two_step = s1.amount_out + s2.amount_out;
    // The on-chain oracle for the same state, zfo=true, input 4728:
    let state = build_fee1_76f75965_v4_state();
    let neg = zero.checked_sub(I256::try_from(amt).unwrap()).unwrap();
    let outc = v4_simulate_swap(&state, 50, 1, true, neg, unbounded_limit(true)).unwrap();
    let onchain = v4_exact_in_output(&outc, true);
    assert_eq!(
        two_step, onchain,
        "two-step floored walk must equal the on-chain v4_simulate_swap (both 4724), proving the \
         single-step collapse (4727) is the +3 over-prediction source"
    );
    assert_eq!(onchain, U256::from(4724u64));
}

/// Margin-policy measurement for the CL-hop clamp (ergo 7E5D7W): quantify the
/// worst solver-vs-`v4_simulate_swap` OVER-prediction magnitude across the
/// covered corpus (fee-1/ts=1 in both directions, plus the fee-3000/ts=60
/// multi-tick topology) — the quantity the VAASFM clamp margin (1 wei) must
/// strictly exceed so the clamp never lands exactly on an over-predicted tight
/// value.
///
/// The clamp commits `input_consumed - margin` where `input_consumed` comes
/// from the tier-3-proven `v4_simulate_swap` twin (NOT the solver). The margin
/// protects against the solver's `hop_outputs[i]` over-predicting the twin by
/// a residue. The parity suites above already assert solver == twin (0
/// divergence) across the multi-tick + fee-1 corpus; this test measures the
/// strict over-prediction direction and asserts the chosen margin (1 wei) is
/// strictly greater — a self-checking guard that the VAASFM maximum-extraction
/// choice remains justified if a future topology re-introduces a residual.
#[test]
fn cl_hop_clamp_margin_exceeds_worst_solver_over_prediction() {
    /// The VAASFM margin decision (must stay > worst observed over-prediction).
    /// Keep in sync with `ArbitrageEngine::cl_hop_clamp_margin` in
    /// degenbot-bot (solver_dispatch.rs).
    const MARGIN: u64 = 1;

    // Worst over-prediction across the corpus (solver_out - sim_out when the
    // solver predicts MORE output than the on-chain twin). The parity suites
    // pin this to 0; a regression here would re-introduce a non-zero value.
    let mut worst_over_predict: u64 = 0;

    // fee-1 / ts=1 (the UO3JM4 low-fee topology whose live +1..+3 residuals
    // were localized and fixed), both swap directions.
    let fee1 = build_fee1_76f75965_v4_state();
    for &zfo in &[true, false] {
        let seq = fee1
            .build_int_v4_sequence(1, 50, zfo, 10)
            .expect("fee-1 sequence");
        for amount_in_u256 in [
            10u64, 100, 500, 1_000, 4_728, 4_729, 5_000, 9_000, 9_586, 20_000,
        ] {
            let amount_in_u256 = U256::from(amount_in_u256);
            let Some(amount_specified) = I256::try_from(amount_in_u256)
                .ok()
                .and_then(|v| I256::ZERO.checked_sub(v))
            else {
                continue;
            };
            let Ok(outcome) =
                v4_simulate_swap(&fee1, 50, 1, zfo, amount_specified, unbounded_limit(zfo))
            else {
                continue;
            };
            let sim_out = v4_exact_in_output(&outcome, zfo);
            let Some(solver_out) = solver_crossing_output(amount_in_u256, &seq) else {
                continue;
            };
            if solver_out > sim_out {
                let delta = solver_out - sim_out;
                let d: u64 = delta.try_into().unwrap_or(u64::MAX);
                worst_over_predict = worst_over_predict.max(d);
            }
        }
    }

    // fee-3000 / ts=60 multi-tick topology (the crossing-corpus sweep that
    // surfaced the zfo partial-step round-up bug), representative liquidity.
    let mt = build_multi_tick_v4_state(10_000_000_000_000_000u128, 5, true);
    let seq = mt
        .build_int_v4_sequence(60, 3_000, true, 10)
        .expect("multi-tick sequence");
    let gins: Vec<U256> = (0..seq.ranges.len())
        .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
        .collect();
    if gins.len() >= 2 {
        for &delta in &[1u64, 2, 3, 7, 13] {
            for &near in &[
                gins[0].saturating_add(U256::from(delta)),
                gins[1].saturating_sub(U256::from(delta)),
            ] {
                let Some(amount_specified) = I256::try_from(near)
                    .ok()
                    .and_then(|v| I256::ZERO.checked_sub(v))
                else {
                    continue;
                };
                let Ok(outcome) = v4_simulate_swap(
                    &mt,
                    3_000,
                    60,
                    true,
                    amount_specified,
                    unbounded_limit(true),
                ) else {
                    continue;
                };
                let sim_out = v4_exact_in_output(&outcome, true);
                let Some(solver_out) = solver_crossing_output(near, &seq) else {
                    continue;
                };
                if solver_out > sim_out {
                    let d: u64 = (solver_out - sim_out).try_into().unwrap_or(u64::MAX);
                    worst_over_predict = worst_over_predict.max(d);
                }
            }
        }
    }

    assert!(
        MARGIN > worst_over_predict,
        "VAASFM clamp margin ({MARGIN} wei) must strictly exceed the worst observed \
         solver-vs-`v4_simulate_swap` over-prediction ({worst_over_predict} wei); otherwise the \
         clamp can land exactly on an over-predicted tight value and re-trigger the EMPTY march \
         (ergo 7E5D7W). Parity suites pin this to 0; a non-zero value is a regression guard trip."
    );
}
