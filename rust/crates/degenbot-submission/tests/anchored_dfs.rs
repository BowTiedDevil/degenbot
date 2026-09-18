//! Acceptance tests for the anchored touched-set discovery engine
//! (`degenbot_submission::anchored_dfs`): the frame's touched pools anchor
//! cycles into the shared pathfinding walker, replacing the hand-rolled
//! 2-hop star.
//!
//! 1. Star parity: every cycle the star (connector fan + drift-cycle
//!    pairing) finds on a synthetic frame is found by the walker.
//! 2. Depth-3 gain: a cycle requiring three hops the star can't express.
//! 3. Soundness property: over generated graphs, returned cycles ALWAYS
//!    intersect the touched pool set.
//! 4. Cancel budget: a per-frame time slice fires on a hostile fixture
//!    without hanging.

#![expect(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_bot::sidecar_paths::{V2ConnectorIndex, V2Edge, V3Edge};
use degenbot_pathfinding::PoolKind;
use degenbot_submission::anchored_dfs::{
    resolve_hop, AnchorPool, AnchoredGraph, DfsCycle, DiscoveryBudget, UnsupportedHop,
};
use proptest::prelude::*;

const QUOTE_ID: u64 = 20; // the WETH DB id in every fixture
const TOK_ID: u64 = 10;

fn edge(pool_id: u64, t0: u64, t1: u64, addr_seed: u8) -> V2Edge {
    V2Edge {
        pool_id,
        token0_id: t0,
        token1_id: t1,
        address: Address::new([addr_seed; 20]),
    }
}

fn v3_edge(pool_id: u64, t0: u64, t1: u64, addr_seed: u8) -> V3Edge {
    V3Edge {
        pool_id,
        token0_id: t0,
        token1_id: t1,
        address: Address::new([addr_seed; 20]),
        fee: 500,
        tick_spacing: 10,
    }
}

fn anchor(pool_id: u64, a: u64, b: u64, kind: PoolKind) -> AnchorPool {
    AnchorPool {
        pool_id,
        pool_kind: kind,
        token_a_id: a,
        token_b_id: b,
    }
}

/// A budget that never expires in-test.
fn open_budget() -> DiscoveryBudget {
    DiscoveryBudget::after(Duration::from_secs(3600))
}

/// Cycle pool ids, normalized for comparison against the star's
/// (anchor, connector) pairing: anchor first, then the connector hops.
fn cycle_pool_ids(c: &DfsCycle) -> Vec<u64> {
    c.pools.iter().map(|(pid, _)| *pid).collect()
}

// ─────────────── 1. star parity ───────────────

/// The star's fan for the affected pool P against the WETH quote is
/// `connectors(tok, quote, exclude = P)`; its cycles pair P with each
/// connector (both drift directions, expressed downstream from the pair).
/// The walker must surface the same `(P, connector)` pair sets — V2 AND V3
/// connectors — and nothing that re-pairs P or leaves the pair.
#[tokio::test]
async fn walker_finds_every_star_cycle_on_synthetic_frame() {
    let mut index = V2ConnectorIndex::default();
    // P: the affected (touched) pool.
    index.push_edge(edge(101, TOK_ID, QUOTE_ID, 0xB1));
    // Star-reachable connectors on (TOK, WETH): V2 x2, V3 x1 (canonical
    // order and orientation varied — the star matches either side).
    index.push_edge(edge(102, TOK_ID, QUOTE_ID, 0xC1));
    index.push_edge(edge(103, QUOTE_ID, TOK_ID, 0xC2));
    index.push_v3_edge(v3_edge(104, TOK_ID, QUOTE_ID, 0xC3));
    // Off-pair noise neither fan may see at the 2-hop surface.
    index.push_edge(edge(105, TOK_ID, 30, 0xC4));

    let star_connectors: Vec<u64> = {
        let v2 = index.connectors(TOK_ID, QUOTE_ID, 101, 16).await;
        let v3 = index.v3_connectors(TOK_ID, QUOTE_ID, 101, 16).await;
        v2.into_iter()
            .map(|(e, _)| e.pool_id)
            .chain(v3.into_iter().map(|(e, _)| e.pool_id))
            .collect::<Vec<_>>()
    };

    let graph = AnchoredGraph::from_connector_index(&index);
    let cycles = graph.cycles_through_pool(
        anchor(101, TOK_ID, QUOTE_ID, PoolKind::V2),
        &open_budget(),
        64,
    );

    // Walker 2-hop pairs = (anchor, connector) sets, regardless of which
    // side the connector indexes the pair on.
    let walker_pairs: Vec<(u64, u64)> = cycles
        .iter()
        .filter(|c| c.pools.len() == 2)
        .map(|c| {
            let ids = cycle_pool_ids(c);
            (ids[0], ids[1])
        })
        .collect();

    for c in &star_connectors {
        let pair = (101, *c);
        assert!(
            walker_pairs.contains(&pair),
            "star cycle {pair:?} missing from the walker: {walker_pairs:?}"
        );
    }
    // Both drift directions surface (the star proposes two cycles per
    // pairing): each connector pairs with the anchor under BOTH entry
    // tokens.
    for c in &star_connectors {
        let directions = cycles
            .iter()
            .filter(|c2| {
                let ids = cycle_pool_ids(c2);
                ids.len() == 2 && ids.contains(c)
            })
            .map(|c2| c2.entry_token_id)
            .collect::<Vec<_>>();
        let mut sorted = directions.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![TOK_ID, QUOTE_ID],
            "connector {c} must surface under both drift entries"
        );
    }
    // The walker never loops the anchor through itself, and nothing at the
    // 2-hop surface involves the off-pair pool.
    assert!(
        walker_pairs
            .iter()
            .all(|(a, b)| *a == 101 && *b != 101 && *b != 105),
        "walker 2-hop pairs must be exactly (anchor, same-pair connector): {walker_pairs:?}"
    );
}

// ─────────────── 2. depth-3 gain ───────────────

/// A 3-hop cycle through an intermediate token X is structurally invisible
/// to the star (its connector fan only matches pools trading the anchor's
/// exact pair), but the walker finds it.
#[tokio::test]
async fn walker_finds_depth_three_cycles_the_star_cannot_express() {
    let mut index = V2ConnectorIndex::default();
    index.push_edge(edge(101, TOK_ID, QUOTE_ID, 0xB1)); // P: touched anchor
    index.push_edge(edge(111, TOK_ID, 30, 0xCF)); // P -- X
    index.push_edge(edge(112, 30, QUOTE_ID, 0xCE)); // X -- quote

    // The star's surface for this frame is EMPTY: no pool trades (TOK, WETH)
    // besides the excluded anchor.
    let star: Vec<_> = index
        .connectors(TOK_ID, QUOTE_ID, 101, 16)
        .await
        .iter()
        .map(|(e, _)| e.pool_id)
        .collect();
    assert!(star.is_empty(), "star fan must be empty on this fixture");

    let graph = AnchoredGraph::from_connector_index(&index);
    let cycles = graph.cycles_through_pool(
        anchor(101, TOK_ID, QUOTE_ID, PoolKind::V2),
        &open_budget(),
        64,
    );
    let threes: Vec<Vec<u64>> = cycles
        .iter()
        .filter(|c| c.pools.len() == 3)
        .map(cycle_pool_ids)
        .collect();
    assert_eq!(
        threes.len(),
        2,
        "both drift directions of the 3-hop cycle surface: {threes:?}"
    );
    for ids in &threes {
        assert_eq!(ids[0], 101, "anchor hop first");
        assert_eq!(
            {
                let mut v = ids[1..].to_vec();
                v.sort_unstable();
                v
            },
            vec![111, 112],
            "the bridge pools ride the cycle: {ids:?}"
        );
    }
    assert_ne!(
        threes[0], threes[1],
        "the two drift directions traverse the bridge in opposite order"
    );
}

// ─────────────── 3. soundness property ───────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Over generated graphs, every cycle the anchored walker returns
    /// contains its touched anchor pool (the touched-set intersection
    /// property), never repeats a pool, and never exceeds 3 hops.
    #[test]
    fn returned_cycles_always_intersect_touched_pools(
        edges in prop::collection::vec((0u16..7, 0u16..7), 2..15)
    ) {
        // Distinct pool ids 1001.., self-edges dropped, anchor = first edge.
        let used: Vec<(u16, u16)> = edges
            .iter()
            .filter(|(a, b)| a != b)
            .copied()
            .collect();
        prop_assume!(!used.is_empty(), "graph needs at least one edge");
        let anchor_pair = used[0];

        let mut index = V2ConnectorIndex::default();
        for (i, (a, b)) in used.iter().enumerate() {
            index.push_edge(edge(
                1001 + i as u64,
                u64::from(*a),
                u64::from(*b),
                u8::try_from(i % 254).unwrap(),
            ));
        }
        let anchor_pool = 1001_u64;
        let graph = AnchoredGraph::from_connector_index(&index);
        let cycles = graph.cycles_through_pool(
            anchor(
                anchor_pool,
                u64::from(anchor_pair.0),
                u64::from(anchor_pair.1),
                PoolKind::V2,
            ),
            &open_budget(),
            128,
        );
        for c in &cycles {
            let ids = cycle_pool_ids(c);
            prop_assert!(ids.contains(&anchor_pool), "cycle misses the touched pool");
            prop_assert!(ids.len() <= 3, "cycle exceeds the depth cap");
            let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
            prop_assert_eq!(ids.len(), unique.len(), "pool repeated in cycle");
        }
    }
}

// ─────────────── 4. cancel budget on a hostile fixture ───────────────

/// A dense clique graph whose full depth-2 enumeration runs well past the
/// budget: the walker must stop promptly (no hang), return only sound
/// cycles, and report the expiry.
#[test]
fn cancel_budget_fires_on_hostile_fixture() {
    let mut index = V2ConnectorIndex::default();
    // 400-token clique, 4 parallel pools per pair.
    let mut pool: u64 = 5000;
    for a in 0u64..400 {
        for b in (a + 1)..400 {
            for k in 0u64..4 {
                index.push_edge(edge(
                    pool,
                    a,
                    b,
                    u8::try_from((pool + k) % 254).unwrap_or(0xFE),
                ));
                pool += 1;
            }
        }
    }
    let graph = AnchoredGraph::from_connector_index(&index);
    let budget = DiscoveryBudget::after(Duration::from_millis(1));
    let started = Instant::now();
    let cycles = graph.cycles_through_pool(anchor(5000, 0, 1, PoolKind::V2), &budget, usize::MAX);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "the budget must stop the walker promptly, took {elapsed:?}"
    );
    assert!(
        !cycles.is_empty(),
        "the first yields land well inside the slice"
    );
    assert!(
        budget.expired(),
        "a hostile fixture must exhaust the 1ms slice"
    );
    for c in &cycles {
        let ids = cycle_pool_ids(c);
        assert!(ids.contains(&5000), "soundness holds under cancellation");
        assert!(ids.len() <= 3);
    }
}

// ─────────────── 5. unsupported-family witness ───────────────

/// A hop whose family has no connector-index lane is a typed `Err`, distinct
/// from a lane miss (`Ok(None)`) — a new `PoolKind` cannot silently vanish.
#[test]
fn resolve_hop_refuses_family_without_index_lane() {
    let index = V2ConnectorIndex::default();

    let err = resolve_hop(&index, (7, PoolKind::V4)).unwrap_err();
    assert_eq!(
        err,
        UnsupportedHop {
            pool_id: 7,
            kind: PoolKind::V4,
        }
    );

    // A V2 edge absent from the index is a data gap, not a family gap.
    assert_eq!(resolve_hop(&index, (8, PoolKind::V2)).unwrap(), None);
}
