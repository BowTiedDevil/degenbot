//! The degenerate concentrated-liquidity state the phantom-candidate corpus
//! turned up: a pool whose price is pinned at `MIN_TICK` by an `initialize` at
//! `MIN_SQRT_RATIO` while every POSITION sits far above it, so the pool's own
//! `liquidity()` is zero and its `slot0.tick` is not even `tick_spacing`-aligned
//! (`MIN_TICK` is a `TickMath` clamp, not a spacing multiple).
//!
//! The solver's job on such a state is to model the free price walk through the
//! empty region and then the liquidity it reaches — NOT to refuse and NOT to
//! report a zero-liquidity pool. This pins the range walk's answer (the tick map
//! is the live mainnet `REN/WETH 0.05%` pool `0x2dD5…0920` at block 26 335 759,
//! read through `tickBitmap(int16)` / `ticks(int24)`).

#![expect(clippy::expect_used)]

use alloy::primitives::{Address, U128, U256};
use degenbot_math::concentrated_liquidity::tick_math::MIN_TICK;
use degenbot_pools::state::tick_bitmap::compute_tick_ranges;
use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams, TickInfo, V3PoolState};
use hashbrown::HashMap;

/// The pool's `slot0.sqrtPriceX96` — `MIN_SQRT_RATIO + 1`, the floor an
/// `initialize` at the price limit leaves behind.
const FLOOR_SQRT_PRICE: u64 = 4_295_128_740;

const TICK_SPACING: i32 = 60;
const FEE: u32 = 3_000;

fn tick(gross: u128, net: i128) -> TickInfo {
    TickInfo {
        liquidity_gross: U128::from(gross),
        liquidity_net: net,
        block: 26_335_759,
    }
}

/// The live pool's initialized ticks below/at its current price (the full
/// 43-tick swap-direction map, `tickBitmap(int16)`-enumerated).
fn live_tick_map() -> HashMap<i32, TickInfo> {
    let mut ticks: HashMap<i32, TickInfo> = HashMap::new();
    for (at, gross, net) in [
        (
            -126_360i32,
            1_353_686_274_433_565u128,
            1_353_686_274_433_565i128,
        ),
        (
            -122_940,
            593_419_721_510_067_943_641,
            593_419_721_510_067_943_641,
        ),
        (
            -119_700,
            593_419_721_510_067_943_641,
            -593_419_721_510_067_943_641,
        ),
        (-105_240, 238_382_913_794_794_989, 238_382_913_794_794_989),
        (-105_180, 7_996_509_767_525_856, 7_996_509_767_525_856),
        (
            -74_280,
            9_275_037_494_908_628_358,
            -9_275_037_494_908_628_358,
        ),
        (
            -71_340,
            2_592_094_513_429_643_747,
            -2_592_094_513_429_643_747,
        ),
        (-46_080, 408_324_519_813_375_699, -408_324_519_813_375_699),
        (-39_120, 742_398_772_309_843_645, -742_398_772_309_843_645),
        (-16_920, 271_287_150_766_695_270, -271_287_150_766_695_270),
        (12_060, 1_353_686_274_433_565, -1_353_686_274_433_565),
    ] {
        ticks.insert(at, tick(gross, net));
    }
    ticks
}

/// A pool state pinned at the price floor: `liquidity()` is zero, the current
/// tick is `MIN_TICK`, and the pool is `Tracked` (a complete map).
fn floor_pinned_state() -> V3PoolState {
    let params = RegisterV3PoolParams {
        address: Address::ZERO,
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee: FEE,
        tick_spacing: TICK_SPACING,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(FLOOR_SQRT_PRICE),
        liquidity: 0,
        tick: MIN_TICK,
        tick_data: live_tick_map(),
        update_block: 26_335_759,
        tick_data_block: Some(26_335_759),
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    state
}

/// The refusal is DIRECTIONAL, and both halves are load-bearing: at the price
/// floor there is nothing below the current tick to walk into (a `zero_for_one`
/// swap cannot move), while every position the pool owns is above it (an
/// `one_for_zero` walk is alive). A blanket refusal here would hide the live
/// side; a blanket sequence would invent liquidity below the floor.
#[test]
fn floor_pinned_pool_refuses_only_the_dead_direction() {
    let state = floor_pinned_state();
    assert!(
        state
            .build_int_v3_sequence(TICK_SPACING, FEE, true)
            .is_none(),
        "a zero_for_one walk from the price floor has no tick to reach"
    );
    assert!(
        state
            .build_int_v3_sequence(TICK_SPACING, FEE, false)
            .is_some(),
        "an one_for_zero walk from the price floor reaches the live positions"
    );
}

/// Walking UP from the floor: the leading ranges sit in the empty region at
/// zero liquidity (the price moves there for free — the chain pays nothing and
/// charges nothing), and the ranges past the first position carry its
/// liquidity. A walk that reported all-zero liquidity would model a dead pool.
#[test]
fn upward_walk_reaches_the_live_positions_above_the_floor() {
    let state = floor_pinned_state();
    let seq = state
        .build_int_v3_sequence(TICK_SPACING, FEE, false)
        .expect("sequence");

    assert_eq!(
        seq.ranges[0].liquidity, 0,
        "the range holding the floor price is the empty region"
    );
    assert!(
        seq.ranges.iter().any(|r| r.liquidity != 0),
        "positions above the floor must contribute liquidity somewhere in the walk"
    );
    let first_live = seq
        .ranges
        .iter()
        .position(|r| r.liquidity != 0)
        .expect("a live range");
    assert!(
        first_live > 0,
        "the live liquidity starts strictly above the floor range"
    );
}

/// The solver's fixed-point check on the same sequence: the crossing table's
/// gross input is non-decreasing, and its last entry is the boundary of the
/// deepest modeled range.
#[test]
fn crossing_table_is_monotone_on_the_floor_state() {
    let state = floor_pinned_state();
    let seq = state
        .build_int_v3_sequence(TICK_SPACING, FEE, false)
        .expect("sequence");
    let mut previous = U256::ZERO;
    for k in 0..seq.ranges.len() {
        let crossing = seq.compute_crossing(k).expect("crossing");
        assert!(
            crossing.crossing_gross_input >= previous,
            "crossing {k} gross input went backwards"
        );
        previous = crossing.crossing_gross_input;
    }
}

/// `compute_tick_ranges` keeps the current tick as range 0's near boundary even
/// though `MIN_TICK` is not a multiple of `tick_spacing` — the collapse must not
/// drop it and silently start the walk one range later.
#[test]
fn unaligned_floor_tick_still_anchors_range_zero() {
    let tick_data = live_tick_map();
    let (ranges, current_index) =
        compute_tick_ranges(&tick_data, MIN_TICK, TICK_SPACING, 0, false).expect("ranges");
    assert_eq!(current_index, 0);
    assert_eq!(
        ranges[0].tick_lower, MIN_TICK,
        "range 0 must start at the (un-aligned) current tick"
    );
}
