//! Full-sweep path enumeration on the mainnet snapshot graph (chain 1).
//!
//! Production-shaped pure-Rust consumer path: open the snapshot SQLite DB,
//! load the flat edge list with `fetch_path_graph_edges`, build a
//! `PathGraph`, precompute dead-end pruning + valid-depth tables, then time
//! full `find_paths_iter` sweeps with per-path `Vec<EdgeKey>` materialization
//! — the shape `bot_core::route_registry` / `backrun_resolver` consume.
//!
//! Requires the fixture generated from the populated database:
//!   `uv run python tests/pathfinding/fixtures/generate_mainnet_snapshot.py`
//! Override its location with `DEGENBOT_SNAPSHOT_DB`.

#![expect(clippy::panic)] // bench harness: criterion setup may fail loudly

use std::path::PathBuf;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use alloy::primitives::{address, Address};
use degenbot_db::DegenbotDb;
use degenbot_pathfinding::{PathGraph, PoolKind};

fn snapshot_db_path() -> PathBuf {
    // Read-only fixture: never auto-heal it (mirrors degenbot-db's parity harness).
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var(degenbot_db::AUTO_HEAL_ENV, "0"));
    match std::env::var_os("DEGENBOT_SNAPSHOT_DB") {
        Some(override_path) => PathBuf::from(override_path),
        None => PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/pathfinding/fixtures/mainnet_snapshot.db"
        )),
    }
}

fn load_graph() -> (DegenbotDb, PathGraph, u64, u64) {
    let path = snapshot_db_path();
    let (db, _state) =
        DegenbotDb::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let data = db
        .fetch_path_graph_edges(1, &[PoolKind::V2, PoolKind::V3, PoolKind::V4])
        .unwrap_or_else(|e| panic!("fetch edges: {e}"));
    let mut graph = PathGraph::from_edges(data.edges);
    graph.prune_dead_ends();
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let native = Address::ZERO;
    let ids = db
        .fetch_token_ids_by_address(1, &[weth, native])
        .unwrap_or_else(|e| panic!("token ids: {e}"));
    (
        db,
        graph,
        *ids.get(&weth).unwrap_or_else(|| panic!("WETH id missing")),
        *ids.get(&native)
            .unwrap_or_else(|| panic!("native id missing")),
    )
}

fn sweep_paths(
    graph: &PathGraph,
    depths: Option<&[Option<Vec<PoolKind>>]>,
    nvd: Option<&[Vec<bool>]>,
    start: u64,
    end: u64,
    min_depth: usize,
    max_depth: Option<usize>,
) -> usize {
    let mut hops = 0usize;
    let filter = depths.map(<[Option<Vec<PoolKind>>]>::to_vec);
    let mut iter = graph.find_paths_iter(
        start,
        end,
        min_depth,
        max_depth,
        true,
        filter.as_deref(),
        nvd,
    );
    while let Some(path) = iter.next_path() {
        hops += path.len();
    }
    hops
}

/// One production sweep-setup cycle: DB fetch → graph build → dead-end
/// prune. The route registry amortizes this once per sweep.
fn fetch_build_prune(db: &DegenbotDb) -> PathGraph {
    let data = db
        .fetch_path_graph_edges(1, &[PoolKind::V2, PoolKind::V3, PoolKind::V4])
        .unwrap_or_else(|e| panic!("fetch edges: {e}"));
    let mut graph = PathGraph::from_edges(data.edges);
    graph.prune_dead_ends();
    graph
}

fn bench_snapshot_sweeps(c: &mut Criterion) {
    let (db, graph, weth, native) = load_graph();

    let depths_all: [Option<Vec<PoolKind>>; 2] = [None, None];
    let nvd_all = graph.compute_node_valid_depths(&depths_all);

    let depths_v3v4v3: [Option<Vec<PoolKind>>; 3] = [
        Some(vec![PoolKind::V3]),
        Some(vec![PoolKind::V4]),
        Some(vec![PoolKind::V3]),
    ];
    let nvd_v3v4v3 = graph.compute_node_valid_depths(&depths_v3v4v3);

    let depths_native: [Option<Vec<PoolKind>>; 3] = [None, None, None];
    let nvd_native = graph.compute_node_valid_depths(&depths_native);

    let mut g = c.benchmark_group("pathfinding_snapshot");
    g.measurement_time(Duration::from_secs(3));
    g.sample_size(15);

    g.bench_function("setup_fetch_build_prune", |b| {
        b.iter(|| black_box(fetch_build_prune(&db)));
    });

    g.bench_function("d2_all_kinds", |b| {
        b.iter(|| {
            black_box(sweep_paths(
                &graph,
                Some(&depths_all),
                Some(&nvd_all),
                weth,
                weth,
                2,
                Some(2),
            ));
        });
    });
    g.bench_function("d3_v3_v4_v3", |b| {
        b.iter(|| {
            black_box(sweep_paths(
                &graph,
                Some(&depths_v3v4v3),
                Some(&nvd_v3v4v3),
                weth,
                weth,
                3,
                Some(3),
            ));
        });
    });
    g.bench_function("native_min2_emd3", |b| {
        b.iter(|| {
            black_box(sweep_paths(
                &graph,
                Some(&depths_native),
                Some(&nvd_native),
                native,
                native,
                2,
                Some(3),
            ));
        });
    });

    g.finish();
}

criterion_group!(benches, bench_snapshot_sweeps);
criterion_main!(benches);
