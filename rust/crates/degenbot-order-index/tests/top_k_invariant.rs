//! RED→GREEN invariant tests for [`EnvelopeIndex`].
//!
//! The central claim: the hot/cold split driven by the hull's upper-bound
//! threshold is **lossless** — `top_k(X, k)` over the hot set reproduces the
//! exact global top-K (by net) under any gas price `X`. We verify this against
//! a brute-force reference over randomized point sets and randomized `X`, and
//! separately check that pruning actually shrinks the hot set (the point of the
//! optimization) on crafted dominated-heavy data.

use degenbot_order_index::{Candidate, EnvelopeIndex};
use proptest::prelude::*;

/// Brute-force reference: full sort of every candidate's net at `X`, desc
/// (ties by ascending id), truncated to `k`. Returns ids.
fn brute_top_k(points: &[Candidate], x: i128, k: usize) -> Vec<u64> {
    let mut ranked: Vec<(i128, u64)> = points.iter().map(|c| (c.gross - c.gas * x, c.id)).collect();
    ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.truncate(k);
    ranked.into_iter().map(|(_, id)| id).collect()
}

/// De-duplicate by id (proptest may emit colliding ids), preserving order.
fn dedup(points: &[Candidate]) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    points
        .iter()
        .filter(|p| seen.insert(p.id))
        .copied()
        .collect()
}

prop_compose! {
    /// A `(gas, gross)` point with realistic magnitudes: gas in the low
    /// millions, gross in wei up to ~1e21, id unique-ish.
    fn point()(id in 0u64..1_000_000u64, gas in 21_000i128..12_000_000i128,
               gross in 0i128..10i128.pow(21)) -> Candidate {
        Candidate { id, gas, gross }
    }
}

proptest! {
    /// The headline invariant: `top_k` over the hot set == brute-force global
    /// top-K over the whole set, for a wide spread of gas prices.
    #[test]
    fn top_k_matches_brute_force(
        points in prop::collection::vec(point(), 1..60),
        x in 0i128..2_000_000_000_000i128, // 0 .. 2000 gwei
        k in 1usize..8,
    ) {
        let unique = dedup(&points);
        let mut idx = EnvelopeIndex::new();
        for p in &unique {
            idx.insert(*p);
        }
        let kk = k.min(unique.len());
        let got: Vec<u64> = idx.top_k(x, kk).into_iter().map(|i| unique[i].id).collect();
        let want = brute_top_k(&unique, x, kk);
        prop_assert_eq!(got, want);
    }

    /// `best(X)` must equal brute-force argmax at `X`.
    #[test]
    fn argmax_matches_brute_force(
        points in prop::collection::vec(point(), 1..60),
        x in 0i128..2_000_000_000_000i128,
    ) {
        let unique = dedup(&points);
        let mut idx = EnvelopeIndex::new();
        for p in &unique {
            idx.insert(*p);
        }
        if let (Some(bi), Some(wi)) = (
            idx.best(x).map(|i| unique[i].id),
            brute_top_k(&unique, x, 1).first().copied(),
        ) {
            // argmax: assert the picked id has >= every candidate's net
            let bn = unique.iter().find(|c| c.id == bi).unwrap();
            let bnet = bn.gross - bn.gas * x;
            for c in &unique {
                let n = c.gross - c.gas * x;
                prop_assert!(bnet >= n, "argmax {} net {} < {} net {}", bi, bnet, c.id, n);
            }
            prop_assert_eq!(bi, wi);
        }
    }

    /// All ids returned by `top_k` are distinct and within bounds.
    #[test]
    fn top_k_ids_distinct(
        points in prop::collection::vec(point(), 1..60),
        x in 0i128..2_000_000_000_000i128,
        k in 1usize..8,
    ) {
        let unique = dedup(&points);
        let mut idx = EnvelopeIndex::new();
        for p in &unique {
            idx.insert(*p);
        }
        let kk = k.min(unique.len());
        let got = idx.top_k(x, kk);
        let ids: Vec<u64> = got.into_iter().map(|i| unique[i].id).collect();
        let mut set = ids.clone();
        set.sort_unstable();
        set.dedup();
        prop_assert_eq!(ids.len(), set.len());
    }
}

/// Pruning effectiveness: with a large concave frontier and hundreds of
/// interior points tucked well below it, the hot set must be much smaller than
/// the total — otherwise the envelope buys nothing. Interior points bracketed
/// by a frontier edge whose max-endpoint net is below the K-th threshold are
/// provably not top-K (crate doc completeness argument) and must fall cold.
#[test]
fn pruning_shrinks_hot_set_for_dominated_data() {
    // A concave (strictly decreasing-slope) frontier of 5 hull vertices.
    // Units: gas in gas-units, gross in wei. X = 2e11 = 200 gwei.
    let frontier = [
        Candidate {
            id: 0,
            gas: 21_000,
            gross: 1_000_000_000_000_000_000_000,
        }, // ~1e21
        Candidate {
            id: 1,
            gas: 100_000,
            gross: 1_500_000_000_000_000_000_000,
        },
        Candidate {
            id: 2,
            gas: 1_000_000,
            gross: 3_000_000_000_000_000_000_000,
        },
        Candidate {
            id: 3,
            gas: 3_000_000,
            gross: 4_000_000_000_000_000_000_000,
        },
        Candidate {
            id: 4,
            gas: 6_000_000,
            gross: 4_500_000_000_000_000_000_000,
        },
    ];
    let mut idx = EnvelopeIndex::new();
    for f in &frontier {
        idx.insert(*f);
    }
    // Hundreds of interior points tucked far below the frontier's low-gas edge:
    // bracketed by (gas 21k, ~1e21) -- (gas 100k, ~1.5e21), whose max-endpoint
    // net at X=2e11 is ~1.5e21, far below the K-th (k=3) hull net (~3e21).
    // Each is provably not top-3 at that X -> must be cold.
    let x = 200_000_000_000i128; // 200 gwei
    let k = 3usize;
    let mut id = 100u64;
    for gas in (22_000i128..99_000).step_by(150) {
        for _ in 0..4 {
            idx.insert(Candidate {
                id,
                gas,
                gross: 1_000_000_000_000_000_000,
            }); // ~1e18
            id += 1;
        }
    }
    let total = idx.len();
    assert!(total > 1000, "expected >1000 points, got {total}");

    // Sanity: hull is still just the 5 frontier vertices (interior all below).
    assert_eq!(idx.hull_len(), 5);

    let hot = idx.hot_len(x, k);
    assert!(
        hot * 10 < total,
        "x={x}: hot={hot} should be << total={total} (dominance pruning)"
    );
}

/// Spike S1 validation: at the worst-case realistic magnitudes the `i128`
/// envelope holds and no `net`/`upper_bound`/hull op panics — a trillion-token
/// asset (gross ~2^100 wei), an extreme gas use (~2^26), and an extreme gas
/// price (~2^60 wei/gas). See docs/ergo-results/order-index-integer-repr-spike.md.
#[test]
fn s1_extreme_magnitudes_no_panic_and_hold_i128() {
    let gross = 1i128 << 100; // ~1.3e30 wei (trillion 18-decimal tokens)
    let gas = 1i128 << 26; // ~6.7e7 gas
    let x = 1i128 << 60; // ~1.15e18 wei/gas
    assert!(gross < i128::MAX, "gross must stay under i128::MAX");
    let mut idx = EnvelopeIndex::new();
    idx.insert(Candidate { id: 0, gas, gross });
    // net = gross - gas*X; exact, no panic. Here 2^100 - 2^86 > 0.
    let net = idx.net(0, x);
    assert_eq!(net, gross - gas * x, "net must be exact (no i128 overflow)");
    // A hull op must not panic (cross handled in i256).
    let top = idx.top_k(x, 1);
    assert_eq!(top.len(), 1);
    assert_eq!(idx.best(x), Some(0));
}

// Spike S2 validation: building the hull by *incremental* `insert` produces the
// exact same hull as a single full `rebuild`, over a randomized sequence.
proptest! {
    #[test]
    fn incremental_insert_matches_rebuild(points in prop::collection::vec(point(), 1..200)) {
        let unique = dedup(&points);
        let mut inc = EnvelopeIndex::new();
        for p in &unique {
            inc.insert(*p);
        }
        let mut rbd = EnvelopeIndex::new();
        rbd.extend(unique.iter().copied());
        prop_assert_eq!(inc.hull_len(), rbd.hull_len(), "incremental hull size must match rebuild");
        for x in [0i128, 50_000_000_000, 200_000_000_000, 2_000_000_000_000] {
            prop_assert_eq!(inc.best(x), rbd.best(x), "argmax mismatch at x={}", x);
            prop_assert_eq!(inc.top_k(x, 3), rbd.top_k(x, 3), "top-3 mismatch at x={}", x);
        }
    }
}
