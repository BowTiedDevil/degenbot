//! On-demand backrun pair resolution + staged path planning (task B6H2KO).
//!
//! Seam: `degenbot_bot::bot_core::backrun_resolver::{
//!   PoolQuery, ResolvedPool, PairDiscovery, StagedPlan, StagedEdge,
//!   OnDemandPathPlanner, apply_staged, PlannerConfig
//! }`.
//!
//! Single-writer discipline: the planner NEVER mutates the shared graph; it
//! stages edges the dispatcher merges under its write lock via `apply_staged`,
//! then rebuilds `PathGraph` from the authoritative list.

#![expect(clippy::unwrap_used)]

use std::collections::HashSet;

use alloy::primitives::Address;
use degenbot_bot::bot_core::backrun_resolver::{
    apply_staged, OnDemandPathPlanner, PairDiscovery, PlannerConfig, PoolKind, PoolQuery,
    ResolvedPool, StagedEdge,
};
use degenbot_pathfinding::PathGraph;

fn id(n: u8) -> u64 {
    100 + u64::from(n)
}

/// Mock discovery: positive for a configured table, None elsewhere; records
/// every query it was asked.
struct MockDiscovery {
    table: Vec<(PoolQuery, ResolvedPool)>,
    asked: std::sync::Mutex<Vec<PoolQuery>>,
}

impl PairDiscovery for MockDiscovery {
    #[expect(clippy::unwrap_used)]
    async fn resolve(&self, query: &PoolQuery) -> Option<ResolvedPool> {
        self.asked.lock().unwrap().push(query.clone());
        // Genuine await point: the mock exercises the port asynchrony.
        tokio::task::yield_now().await;
        self.table.iter().find(|(q, _)| q == query).map(|(_, r)| *r)
    }
}

fn resolved_v2(pool_id: u64) -> ResolvedPool {
    ResolvedPool {
        pool_id,
        pool_kind: PoolKind::V2,
    }
}

#[expect(dead_code)]
fn resolved_v3(pool_id: u64) -> ResolvedPool {
    ResolvedPool {
        pool_id,
        pool_kind: PoolKind::V3,
    }
}

/// Boot graph: pairs (10,11), (11,12), (12,13).
fn boot_edges() -> Vec<(u64, u64, u64, PoolKind)> {
    vec![
        (10, 11, 1, PoolKind::V2),
        (11, 12, 2, PoolKind::V3),
        (12, 13, 3, PoolKind::V2),
    ]
}

fn warm_from(edges: &[(u64, u64, u64, PoolKind)]) -> HashSet<(u64, u64)> {
    edges
        .iter()
        .map(|(a, b, _, _)| (*a.min(b), *a.max(b)))
        .collect()
}

fn planner_cfg() -> PlannerConfig {
    PlannerConfig {
        max_new_edges_per_event: 4,
        token_is_fot: Box::new(|_| false),
    }
}

#[tokio::test]
async fn t01_new_pair_resolves_and_stages_edge() {
    let disc = MockDiscovery {
        table: vec![(PoolQuery::V2Pair(id(20), id(21)), resolved_v2(77))],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let planner = OnDemandPathPlanner::new(warm_from(&boot_edges()), planner_cfg());
    let plan = planner.stage_for_legs(&[(id(20), id(21))], &disc).await;
    assert_eq!(plan.new_edges.len(), 1);
    assert_eq!(plan.new_edges[0].token_a, id(20));
    assert_eq!(plan.new_edges[0].token_b, id(21));
    assert_eq!(plan.new_edges[0].pool_id, 77);
    assert_eq!(plan.new_edges[0].pool_kind, PoolKind::V2);
    assert!(plan.owed_queries.is_empty(), "discovery confirmed the pair");
    assert_eq!(plan.churn.new_pairs, 1);

    // Applying then rebuilding grows the graph without disturbing boot edges.
    let mut authoritative = boot_edges();
    let added = apply_staged(&mut authoritative, &plan.new_edges);
    assert_eq!(added, 1);
    assert_eq!(authoritative.len(), 4);
    let g = PathGraph::from_edges(authoritative.clone());
    assert_eq!(g.node_count(), 6, "boot 4 tokens + the pair's two");
}

#[tokio::test]
async fn t02_warm_pair_hits_never_re_query_never_stage() {
    let disc = MockDiscovery {
        table: vec![],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let planner = OnDemandPathPlanner::new(warm_from(&boot_edges()), planner_cfg());
    let plan = planner.stage_for_legs(&[(10, 11), (11, 12)], &disc).await;
    assert!(plan.new_edges.is_empty(), "warm pairs stage nothing");
    assert!(plan.owed_queries.is_empty());
    assert!(
        disc.asked.lock().unwrap().is_empty(),
        "warm hit skips discovery"
    );
    assert_eq!(plan.churn.new_pairs, 0);
}

#[tokio::test]
async fn t03_failed_discovery_becomes_owed_query_not_edge() {
    let disc = MockDiscovery {
        table: vec![],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let planner = OnDemandPathPlanner::new(warm_from(&boot_edges()), planner_cfg());
    let plan = planner.stage_for_legs(&[(id(30), id(31))], &disc).await;
    assert!(plan.new_edges.is_empty());
    assert_eq!(plan.owed_queries.len(), 1);
    assert_eq!(plan.owed_queries[0], PoolQuery::V2Pair(id(30), id(31)));
    assert_eq!(plan.churn.discovery_misses, 1);
}

#[tokio::test]
async fn t04_fot_token_is_never_staged() {
    let disc = MockDiscovery {
        table: vec![(PoolQuery::V2Pair(id(40), id(41)), resolved_v2(88))],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let cfg = PlannerConfig {
        max_new_edges_per_event: 4,
        token_is_fot: Box::new(|tid| tid == id(41)),
    };
    let planner = OnDemandPathPlanner::new(HashSet::new(), cfg);
    let plan = planner.stage_for_legs(&[(id(40), id(41))], &disc).await;
    assert!(plan.new_edges.is_empty(), "FoT tokens never stage edges");
    assert_eq!(plan.churn.rejected_fot, 1);
}

#[tokio::test]
async fn t05_budget_caps_new_edges_per_event_and_counts_overflow() {
    let disc = MockDiscovery {
        table: vec![
            (PoolQuery::V2Pair(id(50), id(51)), resolved_v2(901)),
            (PoolQuery::V2Pair(id(51), id(52)), resolved_v2(902)),
            (PoolQuery::V2Pair(id(52), id(53)), resolved_v2(903)),
            (PoolQuery::V2Pair(id(53), id(54)), resolved_v2(904)),
            (PoolQuery::V2Pair(id(54), id(55)), resolved_v2(905)),
        ],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let cfg = PlannerConfig {
        max_new_edges_per_event: 3,
        token_is_fot: Box::new(|_| false),
    };
    let planner = OnDemandPathPlanner::new(HashSet::new(), cfg);
    let plan = planner
        .stage_for_legs(
            &[
                (id(50), id(51)),
                (id(51), id(52)),
                (id(52), id(53)),
                (id(53), id(54)),
                (id(54), id(55)),
            ],
            &disc,
        )
        .await;
    assert_eq!(plan.new_edges.len(), 3, "budget caps staging");
    assert_eq!(plan.churn.rejected_budget, 2, "overflow counted");
}

#[tokio::test]
async fn t06_duplicate_legs_within_event_dedupe() {
    let disc = MockDiscovery {
        table: vec![(PoolQuery::V2Pair(id(60), id(61)), resolved_v2(910))],
        asked: std::sync::Mutex::new(Vec::new()),
    };
    let planner = OnDemandPathPlanner::new(HashSet::new(), planner_cfg());
    let plan = planner
        .stage_for_legs(&[(id(60), id(61)), (id(61), id(60))], &disc)
        .await;
    assert_eq!(plan.new_edges.len(), 1, "mirrored pair stages once");
    assert_eq!(plan.churn.new_pairs, 1);
}

#[test]
fn t07_apply_staged_dedupes_and_preserves_boot_edges() {
    let mut authoritative = boot_edges();
    let staged = vec![
        StagedEdge {
            token_a: id(20),
            token_b: id(21),
            pool_id: 77,
            pool_kind: PoolKind::V2,
        },
        // Parallel pool on an EXISTING pair (allowed: fee-tier multigraph)...
        StagedEdge {
            token_a: 10,
            token_b: 11,
            pool_id: 999,
            pool_kind: PoolKind::V2,
        },
    ];
    assert_eq!(apply_staged(&mut authoritative, &staged), 2);
    // Exact re-staging is a no-op.
    let again = vec![staged[0]];
    assert_eq!(apply_staged(&mut authoritative, &again), 0);
    assert_eq!(authoritative[0], boot_edges()[0], "order preserved");
    assert_eq!(authoritative.len(), 5);
}

#[test]
fn t08_v3_pool_queries_carry_fee() {
    assert_ne!(PoolQuery::V3Pool(1, 2, 500), PoolQuery::V2Pair(1, 2));
    assert_ne!(PoolQuery::V3Pool(1, 2, 500), PoolQuery::V3Pool(1, 2, 3000));
}

#[test]
fn t09_address_intern_kv_is_stable() {
    let a = Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
    ]);
    let b = Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02,
    ]);
    let m = degenbot_bot::bot_core::backrun_resolver::OnDemandAddressMap::new();
    let i1 = m.intern(a);
    assert_eq!(m.intern(a), i1, "same address same id");
    let i2 = m.intern(b);
    assert_ne!(i1, i2);
    assert_eq!(m.intern(a), i1, "stable after growth");
}
