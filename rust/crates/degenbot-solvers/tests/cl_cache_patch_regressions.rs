//! Regression suite for the segment-patch clamps of S3/S4/S7 (epic KIMRKS).
//!
//! Strategy-level through the public API so it never collides with the
//! in-module unit tests while two sessions work cl_cache.rs.
//!
//! Pinned findings:
//! - Trailing-range Liquidity event (range == n-1): segs holds n-1 slots for
//!   CROSSING ranges 0..n-2 only; such an event cannot move any crossing-amount
//!   prefix, so the patch must keep every segment (no re-segmentation panics)
//!   and only refresh the final ending_range/profile.
//! - Stale-cache carry-over (cache-lab path 4 t=0): a cached seg[0] from a
//!   moved price offsets every reassembled prefix; a Liquidity patch must
//!   rebuild when the cached shape no longer matches the current sequence.
//! - Single-range PriceMove: segs is EMPTY; the old patch wrote segs[0]
//!   unconditionally (index out of bounds for n == 1).

#![expect(clippy::doc_markdown, clippy::expect_used)]

use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::cl_cache::{strategy_catalog, CacheEvent};
use degenbot_solvers::mobius_v3_int::{build_cl_crossing_table, ClCrossingTable};

fn single_range_seq() -> IntV3TickRangeSequence {
    let mk = |liq: u128, lo: u64, hi: u64, price: u64| IntV3TickRangeHop {
        liquidity: liq,
        sqrt_price_x96: U256::from(price),
        sqrt_price_lower_x96: U256::from(lo),
        sqrt_price_upper_x96: U256::from(hi),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };
    IntV3TickRangeSequence::new(vec![mk(1_000, 90, 110, 100)]).expect("single-range seq")
}

fn two_range_seq() -> IntV3TickRangeSequence {
    let mk = |liq: u128, lo: u64, hi: u64, price: u64| IntV3TickRangeHop {
        liquidity: liq,
        sqrt_price_x96: U256::from(price),
        sqrt_price_lower_x96: U256::from(lo),
        sqrt_price_upper_x96: U256::from(hi),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };
    IntV3TickRangeSequence::new(vec![mk(1_000, 90, 110, 100), mk(1_100, 110, 130, 105)])
        .expect("two-range seq")
}

fn three_range_seq() -> IntV3TickRangeSequence {
    let mk = |liq: u128, lo: u64, hi: u64, price: u64| IntV3TickRangeHop {
        liquidity: liq,
        sqrt_price_x96: U256::from(price),
        sqrt_price_lower_x96: U256::from(lo),
        sqrt_price_upper_x96: U256::from(hi),
        gamma_numer: 997_000,
        fee_denom: 1_000_000,
        zero_for_one: false,
        word_boundary_prices: Vec::new(),
    };
    IntV3TickRangeSequence::new(vec![
        mk(1_000, 90, 110, 100),
        mk(1_100, 110, 145, 115),
        mk(1_200, 145, 180, 155),
    ])
    .expect("three-range seq")
}

/// Lab-faithful PriceMove: move range-0's price to the midpoint of the
/// current/exiting pair (strictly inside the range).
fn move_price(seq: &mut IntV3TickRangeSequence) {
    let r = &mut seq.ranges[0];
    let price = r.sqrt_price_x96;
    let exit = if r.zero_for_one {
        r.sqrt_price_lower_x96
    } else {
        r.sqrt_price_upper_x96
    };
    let mut target = price.saturating_add(exit) / U256::from(2u64);
    if target <= price || target >= exit {
        target = price.saturating_add(U256::from(1u64));
    }
    r.sqrt_price_x96 = target;
}

#[track_caller]
fn assert_crossings_equal(got: &ClCrossingTable, want: &[IntTickRangeCrossing]) {
    assert_eq!(got.len(), want.len(), "crossing table length");
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(
            g.crossing_gross_input, w.crossing_gross_input,
            "k={k} gross"
        );
        assert_eq!(g.crossing_output, w.crossing_output, "k={k} output");
        assert_eq!(
            g.ending_range.liquidity, w.ending_range.liquidity,
            "k={k} liquidity"
        );
        assert_eq!(
            g.ending_range.sqrt_price_x96, w.ending_range.sqrt_price_x96,
            "k={k} sqrt entry"
        );
        assert_eq!(
            g.ending_range.sqrt_price_lower_x96, w.ending_range.sqrt_price_lower_x96,
            "k={k} lower"
        );
        assert_eq!(
            g.ending_range.sqrt_price_upper_x96, w.ending_range.sqrt_price_upper_x96,
            "k={k} upper"
        );
        assert_eq!(
            g.ending_range.gamma_numer, w.ending_range.gamma_numer,
            "k={k} gamma"
        );
    }
}

#[test]
fn trailing_range_liquidity_event_patches_without_resegmentation() {
    // Trailing-range liquidity event on a 2-range sequence: the segment set
    // holds n-1 = 1 slot (crossing range 0 only). A trailing-range liquidity
    // change cannot move any prefix of crossing amounts: every segment stays,
    // only the final ending_range (and its profile) adopt the new liquidity.
    let mut catalog = strategy_catalog();
    for i in [4usize, 7] {
        let strategy = catalog[i].as_mut();
        let seq0 = two_range_seq();
        let mut seq1 = seq0.clone();
        seq1.ranges[1].liquidity += 100;
        let _ = strategy.refill(std::slice::from_ref(&seq0), &CacheEvent::Fresh);
        let prepared = strategy.refill(
            std::slice::from_ref(&seq1),
            &CacheEvent::Liquidity { hop: 0, range: 1 },
        );
        assert_eq!(prepared.len(), 1);
        assert_crossings_equal(&prepared[0].0, &build_cl_crossing_table(&seq1));
        assert_eq!(
            strategy.counters().crossing_tables,
            1,
            "trailing-range liquidity event must patch, not rebuild"
        );
        assert_eq!(
            strategy.counters().partial_rebuilds,
            1,
            "trailing-range liquidity event is a partial rebuild"
        );
    }
}

#[test]
fn liquidity_patch_rejects_stale_cached_segments() {
    // Regression for cache-lab path 4 t=0: the per-hop segment cache outlives
    // differently-mutated epochs (the lab replays several paths through one
    // catalog). A cached seg[0] encoding a moved price would offset EVERY
    // reassembled prefix on a later Liquidity event, so a shape mismatch
    // (range-0 current price) must force an exact rebuild.
    let mut catalog = strategy_catalog();
    for i in [4usize, 7] {
        let strategy = catalog[i].as_mut();
        let baseline = three_range_seq();
        let _ = strategy.refill(std::slice::from_ref(&baseline), &CacheEvent::Fresh);
        let mut moved = baseline.clone();
        move_price(&mut moved);
        let _ = strategy.refill(
            std::slice::from_ref(&moved),
            &CacheEvent::PriceMove { hop: 0 },
        );
        let mut next = baseline.clone();
        next.ranges[1].liquidity += 250;
        let prepared = strategy.refill(
            std::slice::from_ref(&next),
            &CacheEvent::Liquidity { hop: 0, range: 1 },
        );
        assert_eq!(prepared.len(), 1);
        assert_crossings_equal(&prepared[0].0, &build_cl_crossing_table(&next));
    }
}

#[test]
fn price_patch_on_single_range_sequence_stays_exact() {
    // A single-range sequence has an empty segment set; the old PriceMove
    // patch reassembled after unconditional segs[0] — out of bounds for n == 1.
    let mut catalog = strategy_catalog();
    for i in [2usize, 3, 4, 7] {
        let strategy = catalog[i].as_mut();
        let base = single_range_seq();
        let _ = strategy.refill(std::slice::from_ref(&base), &CacheEvent::Fresh);
        let mut moved = base.clone();
        move_price(&mut moved);
        let prepared = strategy.refill(
            std::slice::from_ref(&moved),
            &CacheEvent::PriceMove { hop: 0 },
        );
        assert_eq!(prepared.len(), 1);
        assert_crossings_equal(&prepared[0].0, &build_cl_crossing_table(&moved));
    }
}
