#![expect(clippy::unwrap_used, clippy::expect_used)]
//! Word-boundary flooring divergence — the root cause of the residual V4
//! `CurrencyNotSettled` (ergo ON5QMD).
//!
//! ## Hypothesis (RED)
//!
//! The real V4 PoolManager (and V3 pool) `_nextInitializedTickWithinOneWord`
//! returns the next WORD-BOUNDARY tick when no initialized tick exists in the
//! current tick-bitmap word. The swap then floors `computeSwapStep` at that
//! boundary (sqrt_price_target = sqrt_price_at_tick(boundary)) even though
//! liquidity does not change there. This extra flooring introduces an extra
//! `ceil`/`floor` rounding per word-boundary crossed.
//!
//! `v4_simulate_swap` mirrors this exactly — `gen_ticks` yields BOTH
//! initialized ticks AND word-boundary ticks (step = 256 * tick_spacing), and
//! the swap floors `compute_swap_step_v4` at each.
//!
//! But the SOLVER's `compute_tick_ranges` (tick_bitmap.rs) calls `gen_ticks`
//! and then DROPS the boundary ticks:
//!
//!   if tp.is_initialized || tick == current_tick { initialized_ticks.push(tick) }
//!
//! so the solver's `IntV3TickRangeHop` ranges step only at INITIALIZED
//! ticks. When a range spans an uninitialized word-boundary tick, the solver
//! does ONE `compute_swap_step` (single rounding) while real PM does TWO
//! (floor at the boundary, then continue). The extra rounding consumes more
//! input-as-amount_in+fee → real PM produces LESS output → the solver
//! OVER-PREDICTS → `CurrencyNotSettled` at V4 unlock settlement (exact-delta).
//!
//! `v4_crossing_solver_vs_sim_parity.rs` is blind to this because its
//! fixtures place initialized ticks at ±60·i (i ≤ 5) — ALL within tick-bitmap
//! word 0 (word boundary at ±15360 = 60 × 256). No range spans a word
//! boundary, so solver (boundaries dropped) trivially == sim (boundaries
//! floored, but none in-range).
//!
//! ## This test (the minimal RED)
//!
//! Build a V4 state at tick 0 (tick_spacing 60) with a SPARSE initialized
//! tick past the first word boundary — at tick 30720 (= 60 × 512, word-2
//! start), leaving the word boundary at 15360 (= 60 × 256) UNINITIALIZED.
//! A swap of an amount large enough to cross 15360 but land short of 30720
//! must produce IDENTICAL output from both `v4_simulate_swap` (floors at
//! 15360) and the solver's crossing path (single-shot — no floor). It does
//! not: the solver over-predicts. RED.

#![expect(clippy::doc_markdown)]

use hashbrown::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_pools::v3_state::{PoolTickCoverage, V3PoolState, V3SwapOutcome};
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;

use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

const TICK_SPACING: i32 = 60;
const LP_FEE: u32 = 3_000;

/// Word-boundary tick for `TICK_SPACING` (256 ticks per bitmap word).
/// 60 * 256 = 15360.
const WORD_BOUNDARY_TICK: i32 = TICK_SPACING * 256; // 15360

fn unbounded_limit(zero_for_one: bool) -> U256 {
    V3PoolState::default_sqrt_price_limit(zero_for_one)
}

/// Build a sparse V4 state at tick 0, liquidity `base_liquidity`, with the
/// given `(tick, liquidity_net)` pairs as initialized ticks. Used to place a
/// SPARSE initialized tick past the first word boundary so the solver's
/// range [0, far_tick] spans the uninitialized boundary tick at ±15360.
fn build_sparse_v4_state(base_liquidity: u128, ticks: &[(i32, i128)]) -> V4PoolState {
    let sp_0 = U256::from(1u128) << 96;
    let liq_gross = U256::from(base_liquidity).to::<U128>();

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    for &(tick, net) in ticks {
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
            fee: LP_FEE,
            tick_spacing: TICK_SPACING,
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
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// The solver's CL-hop output for `amount_in` (exact-in) — same assembly
/// `int_simulate_mixed_path_n` uses for a CL hop's `hop_outputs[i]`.
fn solver_crossing_output(amount_in: U256, seq: &IntV3TickRangeSequence) -> Option<U256> {
    let n = seq.ranges.len();
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
        return Some(U256::ZERO);
    }
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    Some(crossing.crossing_output.saturating_add(ending.output))
}

fn v4_exact_in_output(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

#[test]
fn solver_over_predicts_when_range_spans_uninitialized_word_boundary_ofz() {
    let base_liquidity = 1_000_000_000_000_000_000u128; // 1e18
                                                        // Two initialized ticks: at 30720 (= 60*512, word-2 start) and 46080
                                                        // (= 60*768, word-3 start). Range 0 = [0, 30720] spans the uninitialized
                                                        // word boundary at 15360 (= 60*256). The second tick bounds range 0's
                                                        // crossing so `compute_crossing(1).crossing_gross_input` = the input to
                                                        // reach tick 30720.
    let net = i128::try_from(base_liquidity).unwrap();
    let far_tick = TICK_SPACING * 512; // 30720
    let state = build_sparse_v4_state(
        base_liquidity,
        &[(far_tick, net), (TICK_SPACING * 768, -net)],
    );
    let seq = state
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, false)
        .expect("V4 state builds a tick-range sequence");
    let limit = unbounded_limit(false);

    // The solver's gross input to fully cross into range 1 (reach tick 30720).
    // Sweep fractions that land the final sqrt-price PAST the 15360 boundary
    // but SHORT of 30720 — both sides terminate inside range 0; the only
    // difference is whether 15360 is floored at (sim: yes, solver: no).
    let gins: Vec<U256> = (0..seq.ranges.len())
        .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
        .collect();
    assert!(
        gins.len() >= 2,
        "need at least 2 ranges for a crossing input, got {}: {:?}",
        gins.len(),
        gins
    );
    let input_to_reach_far_tick = gins[1];
    assert!(
        !input_to_reach_far_tick.is_zero(),
        "gross input to reach the far initialized tick must be nonzero"
    );

    let mut divergences: Vec<String> = Vec::new();
    for numer in [50u64, 55, 60, 65, 70, 75, 80, 85, 90, 95, 99] {
        let amount_in = input_to_reach_far_tick * U256::from(numer) / U256::from(100u64);
        if amount_in.is_zero() {
            continue;
        }
        let amount_specified = I256::ZERO
            .checked_sub(I256::try_from(amount_in).unwrap())
            .unwrap();
        let outcome =
            v4_simulate_swap(&state, LP_FEE, TICK_SPACING, false, amount_specified, limit)
                .expect("v4_simulate_swap succeeds on a well-formed sparse state");
        let sim_out = v4_exact_in_output(&outcome, false);
        let solver_out =
            solver_crossing_output(amount_in, &seq).expect("solver produces a crossing output");
        if sim_out != solver_out {
            let delta = if solver_out > sim_out {
                solver_out - sim_out
            } else {
                sim_out - solver_out
            };
            divergences.push(format!(
                "numer={numer}% amount_in={amount_in} sim={sim_out} solver={solver_out} \
                 delta={delta} (solver {})",
                if solver_out > sim_out {
                    "OVER-PREDICTS"
                } else {
                    "under-predicts"
                },
            ));
        }
    }

    assert!(
        divergences.is_empty(),
        "solver diverges from v4_simulate_swap when range [0, {far_tick}] spans the uninitialized \
         word boundary at tick {WORD_BOUNDARY_TICK} (ofz). The solver drops the boundary tick \
         (single-shot, no floor) while v4_simulate_swap floors compute_swap_step at it. \
         {} divergence(s):
{}",
        divergences.len(),
        divergences.join("\n"),
    );
}

#[test]
fn solver_over_predicts_when_range_spans_uninitialized_word_boundary_zfo() {
    let base_liquidity = 1_000_000_000_000_000_000u128; // 1e18
                                                        // Descending mirror: two initialized ticks at -30720 and -46080. Range 0
                                                        // = [0, -30720] spans the uninitialized word boundary at -15360.
    let net = i128::try_from(base_liquidity).unwrap();
    let far_tick = -(TICK_SPACING * 512); // -30720
    let state = build_sparse_v4_state(
        base_liquidity,
        &[(far_tick, -net), (-(TICK_SPACING * 768), net)],
    );

    let seq = state
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, true)
        .expect("V4 zfo state builds a tick-range sequence");
    let limit = unbounded_limit(true);

    let gins: Vec<U256> = (0..seq.ranges.len())
        .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
        .collect();
    assert!(gins.len() >= 2, "need ≥2 ranges, got {}", gins.len());
    let input_to_reach_far_tick = gins[1];
    assert!(
        !input_to_reach_far_tick.is_zero(),
        "gross input to reach the far initialized tick must be nonzero"
    );

    let mut divergences: Vec<String> = Vec::new();
    for numer in [50u64, 55, 60, 65, 70, 75, 80, 85, 90, 95, 99] {
        let amount_in = input_to_reach_far_tick * U256::from(numer) / U256::from(100u64);
        if amount_in.is_zero() {
            continue;
        }
        let amount_specified = I256::ZERO
            .checked_sub(I256::try_from(amount_in).unwrap())
            .unwrap();
        let outcome = v4_simulate_swap(&state, LP_FEE, TICK_SPACING, true, amount_specified, limit)
            .expect("v4_simulate_swap succeeds on a well-formed sparse zfo state");
        let sim_out = v4_exact_in_output(&outcome, true);
        let solver_out =
            solver_crossing_output(amount_in, &seq).expect("solver produces a zfo crossing output");
        if sim_out != solver_out {
            let delta = if solver_out > sim_out {
                solver_out - sim_out
            } else {
                sim_out - solver_out
            };
            divergences.push(format!(
                "numer={numer}% amount_in={amount_in} sim={sim_out} solver={solver_out} \
                 delta={delta} (solver {})",
                if solver_out > sim_out {
                    "OVER-PREDICTS"
                } else {
                    "under-predicts"
                },
            ));
        }
    }

    assert!(
        divergences.is_empty(),
        "solver diverges from v4_simulate_swap when range [0, {far_tick}] spans the uninitialized \
         word boundary at tick -{WORD_BOUNDARY_TICK} (zfo). The solver drops the boundary tick \
         (single-shot, no floor) while v4_simulate_swap floors compute_swap_step at it. \
         {} divergence(s):
{}",
        divergences.len(),
        divergences.join("\n"),
    );
}
