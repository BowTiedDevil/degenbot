//! Invariant + Spike S1/S2 checks for the convex-hull [`EnvelopeIndex`],
//! which must satisfy the same top-K contract as `ScanTopK`.

#![cfg(feature = "envelope")]
use alloy_primitives::U256;
use proptest::prelude::*;

use degenbot_order_index::{EnvelopeIndex, OrderIndex};

mod common;
use common::{assert_invariant, brute_top_k, check_topk_and_best, point};

proptest! {
    /// The headline invariant: `top_k` over the hot set == brute-force global
    /// top-K over the whole set, for a wide spread of gas prices.
    #[test]
    fn topk_matches_brute_force(points in prop::collection::vec(point(), 1..500),
                                x in 0u64..2_000_000_000_000,
                                k in 1usize..12) {
        prop_assert!(check_topk_and_best::<EnvelopeIndex<u64>>(
            &points, U256::from(x), k));
    }

    /// Building the hull by incremental `insert` matches a full `rebuild`
    /// (hull size, argmax, top-3) over a randomized sequence.
    #[test]
    fn incremental_insert_matches_rebuild(points in prop::collection::vec(point(), 1..200)) {
        let unique = common::dedup(&points);
        let mut inc = EnvelopeIndex::<u64>::default();
        for c in &unique {
            inc.insert(c.id, c.gas, c.gross);
        }
        let mut rbd = EnvelopeIndex::<u64>::default();
        for c in &unique {
            rbd.insert(c.id, c.gas, c.gross);
            rbd.rebuild(); // force tightening step after each insert
        }
        prop_assert_eq!(inc.hull_len(), rbd.hull_len(), "incremental hull size must match rebuild");
        for x in [0u64, 50_000_000_000, 200_000_000_000, 2_000_000_000_000] {
            let x = U256::from(x);
            prop_assert_eq!(inc.best(x), rbd.best(x), "argmax mismatch at x={}", x);
            prop_assert_eq!(inc.top_k(x, 3), rbd.top_k(x, 3), "top-3 mismatch at x={}", x);
        }
    }
}

/// Pruning effectiveness: with a large concave frontier and hundreds of
/// interior points tucked below it, the hot set must be much smaller than the
/// total — otherwise the envelope buys nothing.
#[test]
fn pruning_shrinks_hot_set_for_dominated_data() {
    // A concave (strictly decreasing-slope) frontier of 5 vertices. X = 2e11.
    let frontier = [
        (0u64, 21_000u64, 1_000_000_000_000_000_000_000u128),
        (1, 100_000, 1_500_000_000_000_000_000_000),
        (2, 1_000_000, 3_000_000_000_000_000_000_000),
        (3, 3_000_000, 4_000_000_000_000_000_000_000),
        (4, 6_000_000, 4_500_000_000_000_000_000_000),
    ];
    let x = U256::from(200_000_000_000u64); // 200 gwei
    let k = 3usize;

    let mut idx = EnvelopeIndex::<u64>::default();
    for (id, gas, gross) in frontier {
        idx.insert(id, U256::from(gas), U256::from(gross));
    }
    // Hundreds of interior points tucked far below the low-gas edge.
    let mut id = 100u64;
    for gas in (22_000u64..99_000).step_by(150) {
        for _ in 0..4 {
            idx.insert(
                id,
                U256::from(gas),
                U256::from(1_000_000_000_000_000_000u128),
            );
            id += 1;
        }
    }
    let total = idx.len();
    assert!(total > 1000, "expected >1000 points, got {total}");
    assert_eq!(idx.hull_len(), 5, "hull must stay the 5 frontier vertices");

    let hot = idx.hot_len(x, k);
    assert!(
        hot * 10 < total,
        "x={x}: hot={hot} should be << total={total} (dominance pruning)"
    );
}

/// Spike S1 validation: exact `I256` net + no overflow at the realistic extreme
/// (trillion-token gross ~2^100, high gas, high X).
#[test]
fn s1_extreme_magnitudes_no_panic() {
    let gross = U256::from(1u128) << 100;
    let gas = U256::from(1u64) << 26;
    let x = U256::from(1u64) << 60;
    let mut idx = EnvelopeIndex::<u64>::default();
    idx.insert(0, gas, gross);
    let top = idx.top_k(x, 1);
    assert_eq!(top, vec![0]);
    assert_eq!(idx.best(x), Some(0));
    assert_eq!(idx.len(), 1);
}

/// Differential against `ScanTopK`: same points, same top-K from both impls.
#[test]
fn envelope_matches_scan_topk() {
    let points = common::dedup(&[
        common::Cand {
            id: 0,
            gas: U256::from(21_000),
            gross: U256::from(1_234_567_890u64),
        },
        common::Cand {
            id: 1,
            gas: U256::from(1_000_000),
            gross: U256::from(9_876_543_210u64),
        },
        common::Cand {
            id: 2,
            gas: U256::from(500_000),
            gross: U256::from(4_444_444_444u64),
        },
        common::Cand {
            id: 3,
            gas: U256::from(3_000_000),
            gross: U256::from(2_222_222_222u64),
        },
    ]);
    let x = U256::from(100_000_000_000u64);
    let want = brute_top_k(&points, x, 3);
    let mut env = EnvelopeIndex::<u64>::default();
    for c in &points {
        env.insert(c.id, c.gas, c.gross);
    }
    assert_eq!(env.top_k(x, 3), want);
    assert_eq!(env.best(x), Some(want[0]));
    // sanity: brute_top_k is our reference, cross-check directly
    assert_invariant::<EnvelopeIndex<u64>>(&points, x, 3);
}
