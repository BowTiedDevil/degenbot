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
use crate::state::v3_state::V3PoolState;
use crate::state::v4_state::V4PoolState;
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
// tick-range walks (identical concentrated liquidity math; V4's packed `slot0.protocolFee`
// rides its adapter call as the fee-combine input).
// ------------------------------------------------------------------

/// The pool's OWN swap-loop census, independent of the solver's range
/// construction: walk `gen_ticks_iter` exactly as `v3_simulate_swap` does and
/// count what the loop reads — one `tickBitmap` word per distinct word probed
/// (the word the price sits in, then one per word the walk enters) and one
/// `Tick.cross` per initialized tick the price reaches.
fn pool_swap_loop_census<F: ClStateFixture>(
    state: &F,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
    amount_in: U256,
) -> (usize, usize) {
    use alloy::primitives::I256;
    use degenbot_math::concentrated_liquidity::swap_math::compute_swap_step_v3;
    use degenbot_math::concentrated_liquidity::tick_math::get_tick_at_sqrt_ratio_internal;
    use std::collections::BTreeSet;

    use crate::sim::tick_fetch::word_of;
    use crate::state::tick_bitmap::gen_ticks_iter;

    let mut remaining = I256::try_from(amount_in).unwrap();
    let mut sp = WalkHost::sqrt_price_x96(state);
    let mut tick = WalkHost::tick(state);
    let mut liquidity = i128::try_from(WalkHost::liquidity(state)).unwrap();
    let fee_pips = U256::from(fee);
    let limit = if zero_for_one {
        U256::from(MIN_SQRT_RATIO) + U256::ONE
    } else {
        U256::from(MAX_SQRT_RATIO) - U256::ONE
    };

    let mut words: BTreeSet<i32> = BTreeSet::new();
    let mut crossings = 0usize;
    let ticks = gen_ticks_iter(
        WalkHost::tick_data(state),
        tick,
        tick_spacing,
        zero_for_one,
        30_000,
    )
    .unwrap();
    for along in ticks {
        if remaining.is_zero() || sp == limit {
            break;
        }
        // `nextInitializedTickWithinOneWord` probes the price's own word going
        // down, and the word of the NEXT tick going up (`gt` is exclusive).
        words.insert(word_of(
            if zero_for_one { tick } else { tick + 1 },
            tick_spacing,
        ));
        let tick_next = if zero_for_one {
            along.tick.max(-887_272)
        } else {
            along.tick.min(887_272)
        };
        let sp_next = U256::from(get_sqrt_ratio_at_tick_internal(tick_next).unwrap());
        let target = if (zero_for_one && sp_next < limit) || (!zero_for_one && sp_next > limit) {
            limit
        } else {
            sp_next
        };
        let step = compute_swap_step_v3(sp, target, liquidity, remaining, fee_pips).unwrap();
        remaining -= I256::try_from(step.amount_in.saturating_add(step.fee_amount)).unwrap();
        sp = step.sqrt_price_next;
        if sp == sp_next {
            if along.is_initialized {
                crossings += 1;
                let net = WalkHost::tick_data(state)
                    .get(&tick_next)
                    .map_or(0, |info| info.liquidity_net);
                liquidity += if zero_for_one { -net } else { net };
            }
            tick = if zero_for_one {
                tick_next - 1
            } else {
                tick_next
            };
        } else {
            tick = get_tick_at_sqrt_ratio_internal(sp.to::<alloy::primitives::U160>())
                .unwrap()
                .as_i32();
        }
    }
    (words.len(), crossings)
}

#[test]
fn walk_census_matches_the_pool_swap_loop_on_both_adapters() {
    fn walk_census_matches_the_pool_swap_loop<F: ClStateFixture>(tag: &str) {
        // What: the walk's projected census (`landed` + the landing fill's word
        // steps, read through the range exit kinds) must equal the census the
        // pool's own swap loop produces — the initialized ticks it ran
        // `Tick.cross` on and the distinct bitmap words it read.
        // Why: the two axes a leg's gas decomposes into are loop facts. The
        // range index alone is NOT the crossing count (a range may exit at a
        // bitmap-word edge) and the landing range's whole word list is not the
        // word count (a partial fill walks only part of it).
        use crate::solve::concentrated_liquidity::ConcentratedLiquidityPathSim;

        let tick_spacing = 1;
        let fee = 3_000;
        let liquidity: u128 = 1_000_000_000_000_000;
        // Two overlapping positions straddling the price: active liquidity is
        // `liquidity` at tick 0, and the walk loses a slice at each initialized
        // tick it crosses so both directions have interior crossings. The ticks
        // are deliberately NOT on bitmap-word edges (word = tick / 256 `spacing`).
        let mut tick_data = hashbrown::HashMap::new();
        for (tick, net) in [
            (-700i32, 400_000_000_000_000i128),
            (-300, 600_000_000_000_000),
            (500, -600_000_000_000_000),
            (1200, -400_000_000_000_000),
        ] {
            tick_data.insert(
                tick,
                TickInfo {
                    liquidity_gross: U128::from(net.unsigned_abs()),
                    liquidity_net: net,
                    block: 0,
                },
            );
        }
        for (current_tick, label) in [
            (0i32, "price on the word minimum"),
            (37, "price inside a word"),
        ] {
            let mut state = F::with_position(liquidity);
            state.replace_tick_data(tick_data.clone(), 0, tick_spacing);
            state.set_tick(current_tick);
            state.set_sqrt_price(U256::from(
                get_sqrt_ratio_at_tick_internal(current_tick).unwrap(),
            ));

            for zero_for_one in [true, false] {
                let seq = state
                    .build_int_sequence(tick_spacing, fee, zero_for_one)
                    .expect("the fixture's tick map builds a sequence");
                let sim = ConcentratedLiquidityPathSim::new(&[&seq], None);
                assert!(
                    seq.ranges.len() > 2,
                    "{tag} {label}: multi-range probe, got {}",
                    seq.ranges.len()
                );
                // A geometric sweep from dust past the in-range reserve, so the
                // landings cover every range (including partial fills inside a
                // multi-word span) and the walk's bounded tail.
                let mut tails = 0;
                for step in 0..=28u32 {
                    let amount = U256::from(1_000_000u64) * U256::from(4u64).pow(U256::from(step));
                    let outcome = sim.output(amount);
                    let census = outcome
                        .census
                        .first()
                        .copied()
                        .flatten()
                        .expect("a state-built sequence carries exit kinds");
                    let (words, crossings) =
                        pool_swap_loop_census(&state, tick_spacing, fee, zero_for_one, amount);
                    if census.bounded_tail {
                        // The sequence stops at the last initialized tick; past
                        // it the pool marches on and the walk cannot see it.
                        tails += 1;
                        assert!(
                            census.initialized_ticks_crossed <= crossings
                                && census.bitmap_words_walked <= words,
                            "{tag} {label} zfo={zero_for_one} at {amount}: a bounded tail still                              only under-counts (walk {census:?} vs loop {crossings} xing / {words} words)"
                        );
                        continue;
                    }
                    assert_eq!(
                        (census.bitmap_words_walked, census.initialized_ticks_crossed),
                        (words, crossings),
                        "{tag} {label} zfo={zero_for_one} at {amount} (landed {}): the walk's                          census must be the swap loop's",
                        outcome.landed[0]
                    );
                }
                assert!(
                    tails < 29,
                    "{tag} {label}: the sweep must reach a modeled landing"
                );
            }
        }
    }
    walk_census_matches_the_pool_swap_loop::<V3PoolState>("V3");
    walk_census_matches_the_pool_swap_loop::<V4PoolState>("V4");
}

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

#[test]
fn range_entry_price_is_the_previous_ranges_exit_boundary_on_both_adapters() {
    fn range_entry_price_is_the_previous_ranges_exit_boundary<F: ClStateFixture>(tag: &str) {
        // What: every range after range 0 enters at the price the previous
        // range EXITS at — its own near bound (the lower bound walking down,
        // the upper walking up), which is what `compute_crossing` hands the
        // ending range.
        // Why: the hop's stored `sqrt_price_x96` is the entry the per-range
        // verbs (`max_gross_input_in_range`, `to_int_hop_state`) start from.
        // Reading the OTHER neighbour stores the previous range's entry, so
        // those verbs would span two ranges.
        let mut state = F::with_position(1_000_000_000_000u128);
        {
            let ticks = JournalHost::tick_data_mut(&mut state);
            ticks.clear();
            for i in 1..=6 {
                let net: i128 = if i % 2 == 0 {
                    -100_000_000_000
                } else {
                    100_000_000_000
                };
                for signed in [60 * i, -60 * i] {
                    ticks.insert(
                        signed,
                        TickInfo {
                            liquidity_gross: U128::from(200_000_000_000u128),
                            liquidity_net: net,
                            block: 0,
                        },
                    );
                }
            }
        }

        for zero_for_one in [true, false] {
            let seq = state
                .build_int_sequence(60, 3_000, zero_for_one)
                .expect("six initialized ticks either side build a sequence");
            assert!(seq.ranges.len() > 2, "{tag}: a multi-range probe");
            for i in 1..seq.ranges.len() {
                let want = if zero_for_one {
                    seq.ranges[i - 1].sqrt_price_lower_x96
                } else {
                    seq.ranges[i - 1].sqrt_price_upper_x96
                };
                assert_eq!(
                    seq.ranges[i].sqrt_price_x96,
                    want,
                    "{tag}: zfo={zero_for_one} range {i} must enter at range {}'s exit boundary",
                    i - 1
                );
                // The crossing table is the authority the solver reads.
                assert_eq!(
                    seq.compute_crossing(i)
                        .expect("index in range")
                        .ending_range
                        .sqrt_price_x96,
                    want,
                    "{tag}: zfo={zero_for_one} range {i} disagrees with compute_crossing"
                );
            }
        }
    }
    range_entry_price_is_the_previous_ranges_exit_boundary::<V3PoolState>("V3");
    range_entry_price_is_the_previous_ranges_exit_boundary::<V4PoolState>("V4");
}
