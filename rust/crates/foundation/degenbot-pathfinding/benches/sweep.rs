//! Full-sweep path enumeration latency on synthetic production-shaped graphs.
//!
//! Times the exact consumption shape used by the Rust strategies
//! (`bot_core::route_registry` / `backrun_resolver`): a prebuilt graph +
//! precomputed prune tables, then one full `find_paths_iter` sweep whose
//! yielded paths are materialized as `Vec<EdgeKey>` for registration.
//!
//! Graph construction and preprocessing run once in setup (they are
//! per-sweep amortized in production), so each bench isolates the
//! enumeration engine: fixpoint pruning + DFS + per-path `EdgeKey` Vecs.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use degenbot_pathfinding::{PathGraph, PoolKind};

/// `find_paths_iter` filter arg for a per-depth allowed-kind list.
fn kind_filter(depths: &[Option<Vec<PoolKind>>]) -> Vec<Option<Vec<PoolKind>>> {
    depths.to_vec()
}

/// Enumerate every path, materializing each as `Vec<EdgeKey>` the way the
/// route registry does, and return the total edge count across paths (keeps
/// the allocator work honest without holding millions of paths live).
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
    let filter = depths.map(kind_filter);
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

/// Hub-and-spoke multigraph: 12 parallel pools per adjacent token pair in a
/// triangle (WETH-USDC-DAI). Probes the bundled-parallel-pool layer and the
/// only-works-within-depth cut on a hub-heavy shape.
fn hub_triangle() -> PathGraph {
    let mut edges = Vec::with_capacity(36);
    for i in 0..12u64 {
        edges.push((1, 2, 100 + i, PoolKind::V2));
        edges.push((2, 3, 200 + i, PoolKind::V3));
        edges.push((3, 1, 300 + i, PoolKind::V4));
    }
    PathGraph::from_edges(edges)
}

/// 4x4 token grid (501-column layout mirrors the WETH-hub production grid):
/// chain cycles of length 4/8/12 over 24 undirected pool rows.
fn grid_4x4() -> PathGraph {
    const W: u64 = 4;
    let gid = |r: u64, c: u64| 500_000 + r * W + c;
    let mut edges = Vec::new();
    let mut pool = 9000u64;
    for r in 0..W {
        for c in 0..W {
            if c + 1 < W {
                edges.push((gid(r, c), gid(r, c + 1), pool, PoolKind::V2));
                pool += 1;
            }
            if r + 1 < W {
                edges.push((gid(r, c), gid(r + 1, c), pool, PoolKind::V2));
                pool += 1;
            }
        }
    }
    PathGraph::from_edges(edges)
}

fn bench_sweep(c: &mut Criterion) {
    let mut g = c.benchmark_group("pathfinding_sweep");
    g.measurement_time(Duration::from_secs(3));
    g.sample_size(20);

    let hub = hub_triangle();
    g.bench_function("hub_triangle_d3", |b| {
        b.iter(|| black_box(sweep_paths(&hub, None, None, 1, 1, 3, Some(3))));
    });

    let grid = grid_4x4();
    let grid_depths = Some(kind_filter(&[None, None, None, None, None]));
    let grid_nvd = grid.compute_node_valid_depths(&kind_filter(&[None, None, None, None, None]));
    g.bench_function("grid_4x4_d5", |b| {
        b.iter(|| {
            black_box(sweep_paths(
                &grid,
                grid_depths.as_deref(),
                Some(&grid_nvd),
                gid_of(0, 0),
                gid_of(0, 0),
                3,
                Some(5),
            ));
        });
    });

    g.finish();
}

fn gid_of(r: u64, c: u64) -> u64 {
    500_000 + r * 4 + c
}

criterion_group!(benches, bench_sweep);
criterion_main!(benches);
