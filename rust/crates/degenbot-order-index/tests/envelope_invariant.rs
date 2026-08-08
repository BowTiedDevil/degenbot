//! Invariant + Spike S1/S2 checks for the convex-hull [`EnvelopeIndex`],
//! which must satisfy the same top-K contract as `ScanTopK`.

#![cfg(feature = "envelope")]
use alloy_primitives::{I256, U256};
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

// Differential across random insert/update/remove sequences: the same ops drive
// both `EnvelopeIndex` (incremental) and `ScanTopK` (brute), and they must agree
// after every mutation. Ids pool into a small range so update/remove actually
// hit existing points.
proptest! {
    #[test]
    fn update_remove_matches_scan_topk(
        ops in prop::collection::vec(op_strategy(), 1..150),
        x in 0u64..2_000_000_000_000,
        k in 1usize..8,
    ) {
        use degenbot_order_index::{EnvelopeIndex, ScanTopK};
        let x = U256::from(x);
        let mut env = EnvelopeIndex::<u64>::new();
        let mut scan = ScanTopK::<u64>::new();
        for op in ops {
            match op {
                Op::Insert(id, gas, gross) => {
                    env.insert(id, gas, gross);
                    scan.insert(id, gas, gross);
                }
                Op::Update(id, gas, gross) => {
                    prop_assert_eq!(env.update(id, gas, gross), scan.update(id, gas, gross));
                }
                Op::Remove(id) => {
                    prop_assert_eq!(env.remove(&id), scan.remove(&id));
                }
            }
            prop_assert_eq!(env.len(), scan.len());
            prop_assert_eq!(env.top_k(x, k), scan.top_k(x, k), "top_k diverged after op {:?}", op);
            prop_assert_eq!(env.best(x), scan.best(x), "best diverged after op {:?}", op);
        }
    }
}

/// A random mutation: insert (upsert), update (existing), or remove, with ids in
/// a small pool so the operations frequently hit present points.
fn op_strategy() -> impl Strategy<Value = Op> {
    (
        any::<u8>(),
        any::<u64>(),
        21_000u64..12_000_000,
        0u128..(1u128 << 100),
    )
        .prop_map(|(a, id, gas, gross)| {
            let id = id % 40; // small pool
            let gas = U256::from(gas);
            let gross = U256::from(gross);
            match a % 3 {
                0 => Op::Insert(id, gas, gross),
                1 => Op::Update(id, gas, gross),
                _ => Op::Remove(id),
            }
        })
}

/// A single differential mutation.
#[derive(Debug, Clone)]
enum Op {
    Insert(u64, U256, U256),
    Update(u64, U256, U256),
    Remove(u64),
}

// The per-block profit floor: `top_k_floor(X,k,min_net)` returns exactly the
// floored brute-force reference, on both impls.
proptest! {
    #[test]
    fn topk_floor_matches_brute(
        points in prop::collection::vec(point(), 1..200),
        x in 0u64..2_000_000_000_000,
        k in 1usize..8,
        floor_frac in 0u64..100,
    ) {
        use degenbot_order_index::{EnvelopeIndex, ScanTopK};
        let unique = common::dedup(&points);
        let x = U256::from(x);
        // floor as a fraction of the max net (so the floor is meaningful)
        let max_net = unique.iter().map(|c| common::net(c, x)).max().unwrap_or(I256::ZERO);
        let min_net = max_net * I256::unchecked_from(i128::from(floor_frac.min(99))) / I256::unchecked_from(100);
        let want = brute_top_k_floor(&unique, x, k, min_net);

        let mut env = EnvelopeIndex::<u64>::default();
        let mut scan = ScanTopK::<u64>::default();
        for c in &unique { env.insert(c.id, c.gas, c.gross); scan.insert(c.id, c.gas, c.gross); }
        prop_assert!(env.top_k_floor(x, k, min_net) == want, "envelope floor mismatch");
        prop_assert!(scan.top_k_floor(x, k, min_net) == want, "scan floor mismatch");
    }
}

/// Brute-force reference for the floored top-K.
fn brute_top_k_floor(points: &[common::Cand], x: U256, k: usize, min_net: I256) -> Vec<u64> {
    let mut ranked: Vec<(I256, u64)> = points
        .iter()
        .map(|c| (common::net(c, x), c.id))
        .filter(|(n, _)| *n >= min_net)
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.truncate(k);
    ranked.into_iter().map(|(_, id)| id).collect()
}
