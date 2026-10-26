#![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
#![expect(clippy::doc_markdown, clippy::too_many_lines)]
//! Pins that `build_int_v4_sequence` models the leading current-tick segment
//! exactly as `build_int_v3_sequence` does.
//!
//! Discriminator: a V4 pool whose CURRENT tick is initialized and whose price sits
//! STRICTLY INSIDE that tick (so the first PoolManager swap step sweeps
//! `[sqrt(currentTick), sqrtPriceX96]` at the PRE-drain stored liquidity before
//! crossing `currentTick`). Every other V4 fixture places `sqrt_price_x96`
//! exactly at `1 << 96` with `tick = 0`, i.e. on the tick boundary, where the
//! leading segment has zero length and the asymmetry cannot show.
//!
//! Reference: `v4_simulate_swap` (the byte-exact PoolManager twin).
//!
//! Scenario A pins the over-prediction (the `CurrencyNotSettled` /
//! hop-clamp-margin direction); scenario B pins the silent missed opportunity
//! (the walk answers 0 where the contract pays out); the V3 control proves the
//! leading hop is exactly what makes the walk exact.

use hashbrown::HashMap;

use alloy::primitives::{Address, B256, I256, U128, U256};

use degenbot_math::concentrated_liquidity::tick_math::get_sqrt_ratio_at_tick_internal;

use degenbot_pools::solve::concentrated_liquidity::{
    simulate_v3_range_swap, IntV3TickRangeSequence, WalkTally,
};
use degenbot_pools::state::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, V3PoolState, V3SwapOutcome,
};
use degenbot_pools::state::v4_state::{
    v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState,
};
use degenbot_pools::TickInfo;

const TICK_SPACING: i32 = 60;
const LP_FEE: u32 = 3_000;
const CURRENT_TICK: i32 = 30;
const L: u128 = 1_000_000_000_000_000_000; // 1e18 active liquidity

fn sqrt_at(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap())
}

/// Midpoint of `CURRENT_TICK`'s sqrt-price span: strictly above
/// `sqrt(currentTick)`, strictly below `sqrt(currentTick + 1)` — i.e. the
/// pool's `tick` really is `CURRENT_TICK` while the price has room to move
/// inside the leading segment.
fn price_inside_current_tick() -> U256 {
    let lo = sqrt_at(CURRENT_TICK);
    let hi = sqrt_at(CURRENT_TICK + 1);
    lo + (hi - lo) / U256::from(2u64)
}

fn tick_map(pairs: &[(i32, i128, u128)]) -> HashMap<i32, TickInfo> {
    pairs
        .iter()
        .map(|&(tick, net, gross)| {
            (
                tick,
                TickInfo {
                    liquidity_gross: U256::from(gross).to::<U128>(),
                    liquidity_net: net,
                    block: 0,
                },
            )
        })
        .collect()
}

fn build_v4(liquidity: u128, tick_data: HashMap<i32, TickInfo>) -> V4PoolState {
    let params = RegisterV4PoolParams {
        pool_manager: Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::ZERO,
            fee: LP_FEE,
            tick_spacing: TICK_SPACING,
            hooks: Address::ZERO,
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: price_inside_current_tick(),
        liquidity,
        tick: CURRENT_TICK,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    };
    let (_id, state) = V4PoolState::from_params(params, 8);
    state
}

fn build_v3(liquidity: u128, tick_data: HashMap<i32, TickInfo>) -> V3PoolState {
    let params = RegisterV3PoolParams {
        address: Address::ZERO,
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee: LP_FEE,
        tick_spacing: TICK_SPACING,
        factory: Address::ZERO,
        sqrt_price_x96: price_inside_current_tick(),
        liquidity,
        tick: CURRENT_TICK,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        slot_layout: degenbot_pools::ConcentratedLiquiditySlotLayout::UniswapV3,
        known_bitmap_words: Vec::new(),
    };
    let (_id, state) = V3PoolState::from_params(params, 8);
    state
}

/// The solver's concentrated-liquidity-hop output for `amount_in` — the same
/// assembly `int_simulate_mixed_path_n` uses.
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
    let ending =
        simulate_v3_range_swap(remaining, &crossing.ending_range, &mut WalkTally::default());
    Some(crossing.crossing_output.saturating_add(ending.output))
}

fn out_of(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

fn exact_in(amount: U256, v4_sign: bool) -> Option<I256> {
    let v = I256::try_from(amount).ok()?;
    if v4_sign {
        I256::ZERO.checked_sub(v)
    } else {
        Some(v)
    }
}

/// Scenario A — `net(currentTick) = -L`, so the pre-drain stored liquidity is `L`
/// and the post-drain (below) liquidity is `2L`. The V4 walk enters its range 0 at
/// `2L`; the contract sweeps the leading segment at `L`.
fn scenario_a_ticks() -> HashMap<i32, TickInfo> {
    let net = i128::try_from(L).unwrap();
    // P1 [CURRENT_TICK, 600] = L, P2 [-60, CURRENT_TICK] = 2L.
    tick_map(&[
        (-60, 2 * net, 2 * L),
        (CURRENT_TICK, net - 2 * net, 3 * L),
        (600, -net, L),
    ])
}

/// Scenario B — `net(currentTick) = +L` (the DAI/WETH shape: the current tick is a
/// position's LOWER bound), so the drain zeroes the range below and the V4 walk's
/// range 0 carries ZERO liquidity.
fn scenario_b_ticks() -> HashMap<i32, TickInfo> {
    let net = i128::try_from(L).unwrap();
    tick_map(&[(CURRENT_TICK, net, L), (600, -net, L), (-60, 0, 2 * L)])
}

#[test]
fn v4_walk_models_the_leading_current_tick_segment() {
    // ---- Scenario A: numerically meaningful over-prediction -----------------
    let v4a = build_v4(L, scenario_a_ticks());
    let seq_v4 = v4a
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, true)
        .expect("V4 sequence");
    let v3a = build_v3(L, scenario_a_ticks());
    let seq_v3 = v3a
        .build_int_v3_sequence(TICK_SPACING, LP_FEE, true)
        .expect("V3 sequence");

    println!(
        "scenario A current sqrt price = {}",
        price_inside_current_tick()
    );
    println!("scenario A sqrt(currentTick) = {}", sqrt_at(CURRENT_TICK));
    println!(
        "V4 range 0: liq={} entry={} lower={} upper={}",
        seq_v4.ranges[0].liquidity,
        seq_v4.ranges[0].sqrt_price_x96,
        seq_v4.ranges[0].sqrt_price_lower_x96,
        seq_v4.ranges[0].sqrt_price_upper_x96,
    );
    println!(
        "V3 range 0 (leading): liq={} entry={} lower={} upper={}",
        seq_v3.ranges[0].liquidity,
        seq_v3.ranges[0].sqrt_price_x96,
        seq_v3.ranges[0].sqrt_price_lower_x96,
        seq_v3.ranges[0].sqrt_price_upper_x96,
    );
    println!(
        "V3 range 1: liq={} entry={}",
        seq_v3.ranges[1].liquidity, seq_v3.ranges[1].sqrt_price_x96
    );

    // The V3 walk's leading hop is exactly the missing V4 segment; its crossing
    // input is the input that reaches sqrt(currentTick).
    let input_to_current_tick = seq_v3
        .compute_crossing(1)
        .expect("V3 crossing to range 1")
        .crossing_gross_input;
    println!("input to reach sqrt(currentTick) = {input_to_current_tick}");

    let mut divergences = Vec::new();
    for pct in [10u64, 25, 50, 75, 90, 99] {
        let amount_in = input_to_current_tick * U256::from(pct) / U256::from(100u64);
        let Some(specified) = exact_in(amount_in, true) else {
            continue;
        };
        let sim = v4_simulate_swap(
            &v4a,
            LP_FEE,
            TICK_SPACING,
            true,
            specified,
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("v4_simulate_swap");
        let sim_out = out_of(&sim, true);
        let solver_out = solver_crossing_output(amount_in, &seq_v4).expect("solver output");
        println!(
            "A pct={pct} amount_in={amount_in} sim={sim_out} solver={solver_out} sim_final_sp={} (inside leading segment: {})",
            sim.sqrt_price_x96,
            sim.sqrt_price_x96 > sqrt_at(CURRENT_TICK)
        );
        if sim_out != solver_out {
            divergences.push(format!(
                "pct={pct} amount_in={amount_in} sim={sim_out} solver={solver_out} delta={}",
                if solver_out > sim_out {
                    solver_out - sim_out
                } else {
                    sim_out - solver_out
                }
            ));
        }
    }

    // V3 control on identical scalars: the leading hop makes the V3 walk agree.
    let v3_divergences: Vec<String> = [10u64, 50, 99]
        .iter()
        .filter_map(|&pct| {
            let amount_in = input_to_current_tick * U256::from(pct) / U256::from(100u64);
            let sim = v3_simulate_swap(
                &v3a,
                LP_FEE,
                TICK_SPACING,
                true,
                exact_in(amount_in, false)?,
                V3PoolState::default_sqrt_price_limit(true),
            )
            .ok()?;
            let sim_out = out_of(&sim, true);
            let solver_out = solver_crossing_output(amount_in, &seq_v3)?;
            (sim_out != solver_out).then(|| {
                format!(
                    "V3-CONTROL pct={pct} amount_in={amount_in} sim={sim_out} solver={solver_out}"
                )
            })
        })
        .collect();
    println!("V3 control divergences: {v3_divergences:?}");

    // ---- Scenario B: the drain zeroes the V4 walk's range 0 ----------------
    let v4b = build_v4(L, scenario_b_ticks());
    let seq_v4b = v4b
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, true)
        .expect("V4 scenario B sequence");
    let v3b = build_v3(L, scenario_b_ticks());
    let seq_v3b = v3b
        .build_int_v3_sequence(TICK_SPACING, LP_FEE, true)
        .expect("V3 scenario B sequence");
    let input_to_current_tick_b = seq_v3b
        .compute_crossing(1)
        .expect("V3 scenario B crossing")
        .crossing_gross_input;
    println!(
        "scenario B: V4 range0 liq={} V3 lead liq={} V3 r1 liq={}",
        seq_v4b.ranges[0].liquidity, seq_v3b.ranges[0].liquidity, seq_v3b.ranges[1].liquidity
    );
    let amount_b = input_to_current_tick_b * U256::from(50u64) / U256::from(100u64);
    let sim_b = v4_simulate_swap(
        &v4b,
        LP_FEE,
        TICK_SPACING,
        true,
        exact_in(amount_b, true).unwrap(),
        V3PoolState::default_sqrt_price_limit(true),
    )
    .expect("v4_simulate_swap scenario B");
    let sim_out_b = out_of(&sim_b, true);
    let solver_out_b = solver_crossing_output(amount_b, &seq_v4b).expect("solver scenario B");
    println!("scenario B: amount_in={amount_b} sim={sim_out_b} solver={solver_out_b}");

    // ---- ofz mirror: `gt` is exclusive, so NO leading segment exists -------
    // The current tick is only re-crossed by a zfo walk; an ofz walk starts
    // strictly above it, so neither verb may prepend a hop there.
    let seq_v4_ofz = v4a
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, false)
        .expect("V4 ofz sequence");
    let seq_v3_ofz = v3a
        .build_int_v3_sequence(TICK_SPACING, LP_FEE, false)
        .expect("V3 ofz sequence");
    assert_eq!(
        seq_v4_ofz.ranges.len(),
        seq_v3_ofz.ranges.len(),
        "ofz prepends no leading hop on either verb"
    );
    assert_eq!(
        seq_v4_ofz.ranges[0].sqrt_price_x96,
        price_inside_current_tick(),
        "ofz range 0 enters at the current price (no leading segment)"
    );
    assert_eq!(
        seq_v4_ofz.ranges[0].liquidity, seq_v3_ofz.ranges[0].liquidity,
        "ofz range 0 carries the same liquidity on both verbs"
    );
    // A shallow ofz fill that stays inside range 0 (the next initialized tick
    // above the current tick is 600, ~1.9e19 of net input away).
    let amount_ofz = U256::from(10_000_000_000_000_000u128);
    let sim_ofz = v4_simulate_swap(
        &v4a,
        LP_FEE,
        TICK_SPACING,
        false,
        exact_in(amount_ofz, true).expect("ofz exact-in amount"),
        V3PoolState::default_sqrt_price_limit(false),
    )
    .expect("v4_simulate_swap ofz");
    let sim_out_ofz = out_of(&sim_ofz, false);
    let solver_out_ofz = solver_crossing_output(amount_ofz, &seq_v4_ofz).expect("solver ofz");
    println!("ofz: amount_in={amount_ofz} sim={sim_out_ofz} solver={solver_out_ofz}");

    // Both walks model the leading segment as hop 0 at the stored liquidity,
    // spanning [current price, sqrt(currentTick)]; the V4 hop differs only in
    // its fee (protocol fee 0 here, so the combined fee equals the lp fee).
    assert_eq!(
        seq_v4.ranges[0].liquidity, seq_v3.ranges[0].liquidity,
        "V4 leading hop must sweep at the PRE-drain stored liquidity, like the V3 verb"
    );
    assert_eq!(
        seq_v4.ranges[0].sqrt_price_x96,
        price_inside_current_tick(),
        "V4 leading hop must enter at the current price"
    );
    assert_eq!(
        (
            seq_v4.ranges[0].sqrt_price_lower_x96,
            seq_v4.ranges[0].sqrt_price_upper_x96
        ),
        (sqrt_at(CURRENT_TICK), sqrt_at(CURRENT_TICK)),
        "V4 leading hop must span [current price, sqrt(currentTick)]"
    );
    assert!(
        divergences.is_empty()
            && v3_divergences.is_empty()
            && sim_out_b == solver_out_b
            && sim_out_ofz == solver_out_ofz,
        "V4 walk diverges from v4_simulate_swap when the current tick is initialized and the \
         price sits inside it (leading segment). V3 control divergences={v3_divergences:?}, \
         ofz sim={sim_out_ofz} solver={solver_out_ofz}, scenario B sim={sim_out_b} \
         solver={solver_out_b}, scenario A divergences={}:\n{}",
        divergences.len(),
        divergences.join("\n"),
    );
}
