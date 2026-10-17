//! The merged walk-verb test suite (ADR-080 D4): directional viability,
//! the lazy tick-range cache, the integer tick-range walks, and the
//! two-stamp monotonic clock guards — the former V3/V4 twin batteries
//! converging at the shared interface, each behavior written once against
//! the [`ClStateFixture`] seam and run on BOTH adapters. The monotonic
//! guards panic, so their probes get per-adapter wrappers (a shared test
//! body would stop at the first adapter's panic).

#![expect(clippy::unwrap_used, clippy::expect_used)]

use alloy::primitives::{U128, U256};
use degenbot_math::concentrated_liquidity::tick_math::{
    get_sqrt_ratio_at_tick_internal, MAX_SQRT_RATIO, MIN_SQRT_RATIO,
};

use super::{
    get_cached_tick_ranges, invalidate_tick_range_cache, swap_is_viable, CachedTickRanges,
};
use crate::families::concentrated_liquidity::state_fixture::ClStateFixture;
use crate::families::concentrated_liquidity::state_journal::JournalHost;
use crate::families::concentrated_liquidity::state_walk::WalkHost;
use crate::v3_state::V3PoolState;
use crate::v4_state::V4PoolState;
use crate::TickInfo;

// ------------------------------------------------------------------
// swap_is_viable — directional viability, the twin of the archived
// Python v3_liquidity_pool.py::swap_is_viable.
// ------------------------------------------------------------------

#[test]
fn empty_tick_data_is_not_viable_in_either_direction_on_both_adapters() {
    fn empty_tick_data_is_not_viable_in_either_direction<F: ClStateFixture>(tag: &str) {
        // What: a pool with NO initialized ticks cannot host a swap in either
        // direction — the solver has no range to walk.
        // Why: the cheapest rejection; mirrors Python `tick_data == {}`.
        let mut state = F::with_position(10_000_000_000_000u128);
        JournalHost::tick_data_mut(&mut state).clear();

        assert!(!swap_is_viable(&state, true), "{tag}: zfo");
        assert!(!swap_is_viable(&state, false), "{tag}: ofz");
    }
    empty_tick_data_is_not_viable_in_either_direction::<V3PoolState>("V3");
    empty_tick_data_is_not_viable_in_either_direction::<V4PoolState>("V4");
}

#[test]
fn uninitialized_price_is_not_viable_in_either_direction_on_both_adapters() {
    fn uninitialized_price_is_not_viable_in_either_direction<F: ClStateFixture>(tag: &str) {
        // What: a pool whose sqrt_price_x96 is zero was never initialized on
        // chain and cannot host a swap.
        // Why: mirrors Python `sqrt_price_x96 == 0` guard.
        let mut state = F::with_position(10_000_000_000_000u128);
        state.set_sqrt_price(U256::ZERO);

        assert!(!swap_is_viable(&state, true), "{tag}: zfo");
        assert!(!swap_is_viable(&state, false), "{tag}: ofz");
    }
    uninitialized_price_is_not_viable_in_either_direction::<V3PoolState>("V3");
    uninitialized_price_is_not_viable_in_either_direction::<V4PoolState>("V4");
}

#[test]
fn straddling_position_is_viable_in_both_directions_on_both_adapters() {
    fn straddling_position_is_viable_in_both_directions<F: ClStateFixture>(tag: &str) {
        // What: the 1:1 pool with [-60,+60] straddling the current tick has
        // initialized ticks BOTH below (-60) and above (+60) the price —
        // viable in both directions.
        let state = F::with_position(10_000_000_000_000u128);

        assert!(swap_is_viable(&state, true), "{tag}: zfo");
        assert!(swap_is_viable(&state, false), "{tag}: ofz");
    }
    straddling_position_is_viable_in_both_directions::<V3PoolState>("V3");
    straddling_position_is_viable_in_both_directions::<V4PoolState>("V4");
}

#[test]
fn one_sided_liquidity_is_direction_dependent_on_both_adapters() {
    fn one_sided_liquidity_is_direction_dependent<F: ClStateFixture>(tag: &str) {
        // What: all initialized ticks ABOVE the current price → a zfo swap
        // (walking DOWN) finds no liquidity ahead and is not viable, while
        // ofz (walking UP) is viable. This is the directional precision the
        // Python check had via min/max tick comparison.
        // Why: this exact population produces the one-sided pools that die
        // as SequenceUnavailable in only one direction.
        let mut state = F::with_position(10_000_000_000_000u128);
        // Move the price far BELOW every initialized tick (tick -100000): a
        // zfo swap walks DOWN from here and finds no position ahead.
        state.set_tick(-100_000);
        state.set_sqrt_price(U256::from(
            get_sqrt_ratio_at_tick_internal(-100_000).unwrap(),
        ));

        assert!(
            !swap_is_viable(&state, true),
            "{tag}: zfo walks into nothing"
        );
        assert!(
            swap_is_viable(&state, false),
            "{tag}: ofz has liquidity ahead"
        );
    }
    one_sided_liquidity_is_direction_dependent::<V3PoolState>("V3");
    one_sided_liquidity_is_direction_dependent::<V4PoolState>("V4");
}

#[test]
fn price_at_sqrt_ratio_boundary_is_not_viable_in_the_exiting_direction_on_both_adapters() {
    fn price_at_sqrt_ratio_boundary_is_not_viable_in_the_exiting_direction<F: ClStateFixture>(
        tag: &str,
    ) {
        // What: a price at/below MIN_SQRT_RATIO+1 cannot go further DOWN
        // (zfo); a price at/above MAX_SQRT_RATIO-1 cannot go further UP (ofz).
        // Why: protocol limit guard from the Python port.
        let mut state = F::with_position(10_000_000_000_000u128);
        state.set_sqrt_price(U256::from(MIN_SQRT_RATIO) + U256::from(1u64));

        assert!(
            !swap_is_viable(&state, true),
            "{tag}: zfo at the lower boundary"
        );
        assert!(
            swap_is_viable(&state, false),
            "{tag}: ofz at the lower boundary"
        );

        state.set_sqrt_price(U256::from(MAX_SQRT_RATIO) - U256::from(1u64));
        assert!(
            swap_is_viable(&state, true),
            "{tag}: zfo at the upper boundary"
        );
        assert!(
            !swap_is_viable(&state, false),
            "{tag}: ofz at the upper boundary"
        );
    }
    price_at_sqrt_ratio_boundary_is_not_viable_in_the_exiting_direction::<V3PoolState>("V3");
    price_at_sqrt_ratio_boundary_is_not_viable_in_the_exiting_direction::<V4PoolState>("V4");
}

// ------------------------------------------------------------------
// The two-stamp monotonic clock guards. The rollback is the ONLY
// sanctioned rewind: a backward stamp applied outside a reorg still
// panics on both clocks. Per-adapter probes — the guard panics.
// ------------------------------------------------------------------

#[test]
#[should_panic(expected = "monotonicity violated: update_block")]
fn backward_price_clock_stamp_outside_reorg_still_panics_v3() {
    let mut state = V3PoolState::seeded_state();
    WalkHost::set_update_block(&mut state, 10);
    state.apply_swap(U256::from(1u128) << 96, 1_000_000, 0, 5, &[]);
}

#[test]
#[should_panic(expected = "monotonicity violated: update_block")]
fn backward_price_clock_stamp_outside_reorg_still_panics_v4() {
    let mut state = V4PoolState::seeded_state();
    WalkHost::set_update_block(&mut state, 10);
    state.apply_swap(U256::from(1u128) << 96, 1_000_000, 0, 5, &[]);
}

#[test]
#[should_panic(expected = "monotonicity violated: tick_data_block")]
fn backward_liquidity_clock_stamp_outside_reorg_still_panics_v3() {
    let mut state = V3PoolState::seeded_state();
    WalkHost::set_tick_data_block(&mut state, 10);
    state.apply_liquidity_update(-60, 60, 1_000, 5);
}

#[test]
#[should_panic(expected = "monotonicity violated: tick_data_block")]
fn backward_liquidity_clock_stamp_outside_reorg_still_panics_v4() {
    let mut state = V4PoolState::seeded_state();
    WalkHost::set_tick_data_block(&mut state, 10);
    state.apply_liquidity_update(-60, 60, 1_000, 5);
}

// ------------------------------------------------------------------
// get_cached_tick_ranges + the lazy per-direction cache (caching Miss
// prevents re-walking pools whose state hasn't changed — the dominant
// cost in live solve cycles: 1300-9600 `SequenceUnavailable` rejections
// per cycle, each re-walking O(tick-walk) from scratch). Invalidation is
// already wired to every V3/V4 state mutation (Swap/Mint/Burn), so a pool
// that was unviable and later receives liquidity re-walks fresh.
// ------------------------------------------------------------------

#[test]
fn none_tick_ranges_cached_to_avoid_rewalk_on_both_adapters() {
    fn none_tick_ranges_cached_to_avoid_rewalk<F: ClStateFixture>(tag: &str) {
        let mut state = F::with_position(10_000_000_000_000u128);
        // Clear tick_data so compute_tick_ranges returns None (no
        // initialized ticks ahead in either direction).
        JournalHost::tick_data_mut(&mut state).clear();

        // First call: walks, returns None.
        let first = get_cached_tick_ranges(&state, 60, true);
        assert!(first.is_none(), "{tag}: empty tick data should yield None");

        // The cache slot must now reflect a computed Miss — NOT StillEmpty.
        // Before the fix, the slot is StillEmpty because None was not
        // cached, meaning every subsequent call re-walks from scratch.
        assert!(
            matches!(state.cached_slot(true), CachedTickRanges::Miss),
            "{tag}: zfo slot must be cached (Miss) after the first None result, \
             not StillEmpty (which causes a re-walk every cycle)"
        );
        assert!(
            matches!(state.cached_slot(false), CachedTickRanges::StillEmpty),
            "{tag}: ofz slot should still be StillEmpty (not called)"
        );

        // After invalidation, both slots reset to StillEmpty.
        invalidate_tick_range_cache(&state);
        assert!(
            matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
            "{tag}: zfo slot reset"
        );
        assert!(
            matches!(state.cached_slot(false), CachedTickRanges::StillEmpty),
            "{tag}: ofz slot reset"
        );

        // After adding liquidity + invalidation, should return Some.
        let liq = U256::from(1_000_000u64).to::<U128>();
        let ticks = JournalHost::tick_data_mut(&mut state);
        ticks.insert(
            -60,
            TickInfo {
                liquidity_gross: liq,
                liquidity_net: 1_000_000i128,
                block: 0,
            },
        );
        ticks.insert(
            60,
            TickInfo {
                liquidity_gross: liq,
                liquidity_net: -1_000_000i128,
                block: 0,
            },
        );
        let third = get_cached_tick_ranges(&state, 60, true);
        assert!(
            third.is_some(),
            "{tag}: after adding liquidity + invalidation, should be Some"
        );
        assert!(
            matches!(state.cached_slot(true), CachedTickRanges::Hit(_)),
            "{tag}: zfo slot should be Hit after computing Some"
        );
    }
    none_tick_ranges_cached_to_avoid_rewalk::<V3PoolState>("V3");
    none_tick_ranges_cached_to_avoid_rewalk::<V4PoolState>("V4");
}

// ------------------------------------------------------------------
// build_int_v3_sequence / build_int_v4_sequence — the integer
// tick-range walks (identical CL math; V4's packed `slot0.protocolFee`
// rides its adapter call as the fee-combine input).
// ------------------------------------------------------------------

#[test]
fn single_straddling_position_builds_one_range_on_both_adapters() {
    fn single_straddling_position_builds_one_range<F: ClStateFixture>(tag: &str) {
        // Probe: a single reachable position is enough for range building —
        // the solver does not require multiple positions.
        let state = F::with_position(10_000_000_000_000u128);
        let seq = state.build_int_sequence(60, 3_000, true);
        assert_eq!(seq.expect("built").ranges.len(), 1, "{tag}: one range");
    }
    single_straddling_position_builds_one_range::<V3PoolState>("V3");
    single_straddling_position_builds_one_range::<V4PoolState>("V4");
}

#[test]
fn int_sequence_is_not_truncated_at_twentyfour_ranges_on_both_adapters() {
    fn int_sequence_is_not_truncated_at_twentyfour_ranges<F: ClStateFixture>(tag: &str) {
        // Pin 4b05cb17e (max_ranges / WALK_CEILING removal): a pool with 30
        // initialized ticks in one swap direction must yield MORE than 24
        // ranges. Pre-removal the feed was truncated at 24, which is exactly
        // the shape the pre-removal golden captures (369 corpus) froze in.
        let mut state = F::with_position(1_000_000_000_000u128);
        let ticks = JournalHost::tick_data_mut(&mut state);
        ticks.clear();
        for i in 1..=30 {
            let net: i128 = if i % 2 == 0 {
                -100_000_000_000
            } else {
                100_000_000_000
            };
            ticks.insert(
                60 * i,
                TickInfo {
                    liquidity_gross: U128::from(200_000_000_000u128),
                    liquidity_net: net,
                    block: 0,
                },
            );
        }

        // ofz walks UP from tick 0 through every initialized tick above it.
        let seq = state
            .build_int_sequence(60, 3_000, false)
            .expect("30 initialized ticks above the current tick build a sequence");
        assert!(
            seq.ranges.len() > 24,
            "{tag}: sequence must not be truncated at 24 ranges; got {}",
            seq.ranges.len()
        );
    }
    int_sequence_is_not_truncated_at_twentyfour_ranges::<V3PoolState>("V3");
    int_sequence_is_not_truncated_at_twentyfour_ranges::<V4PoolState>("V4");
}
