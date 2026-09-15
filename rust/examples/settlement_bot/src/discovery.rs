//! Discovery graph construction + batched path finding — parity-ledger
//! row 12 .
//!
//! Mirrors the graph half of `src/degenbot/pathfinding/_pathfinding.py`
//! (`_prepare_graph` / `build_path_graph`: candidate-token degree filter,
//! allowed-intermediate intersection, V4 graph-id namespacing,
//! `prune_dead_ends`) over the G2 discovery rows
//! (`degenbot::db::discovery_read::DiscoveryPoolRow`) instead of
//! re-opening the DB — so the enumeration and the graph share ONE held
//! snapshot transaction (the `build_paths.py` snapshot discipline).
//!
//! The DFS is the umbrella's own `degenbot::pathfinding::OwnedPathFinder`.
//! The batching driver mirrors `find_paths_async`'s `discovery_batch_size`
//! delivery: `batch_size <= 1` degrades to per-path delivery, larger values
//! deliver `batch_size` paths per batch.
//!
//! **Difference (documented):** Python's `find_paths_async` drives a lazy
//! sync generator on a WORKER THREAD with a bounded `queue.Queue`, draining
//! one batch per `asyncio.to_thread` hop so the event loop stays live. The
//! Rust driver owns a lazy `OwnedPathFinder` directly (no GIL, no worker
//! thread); it yields one batch per cooperative async hop
//! (`tokio::task::yield_now`). The delivered batch contents/order are
//! identical; only the liveness mechanism differs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use degenbot::core::address_utils::address_to_checksum_string;
use degenbot::db::discovery_read::DiscoveryPoolRow;
use degenbot::pathfinding::{OwnedPathFinder, PathGraph, PoolKind};

/// V4 graph-id namespace offset (mirrors
/// `degenbot_db::pathfinding::V4_POOL_ID_OFFSET`).
///
/// The V2/V3 `pools.id` counter and the V4 `managed_pools.id` counter are
/// independent, so a bare numeric id can collide (mainnet: 116k of 125k V4
/// ids). `degenbot::db` does not re-export the core constant, so the example
/// mirrors it — see the parity-ledger report note.
pub const V4_POOL_ID_OFFSET: u64 = 1 << 32;

/// The zero address (V4 native currency sentinel).
pub const NATIVE_CURRENCY: &str = "0x0000000000000000000000000000000000000000";

/// One discovered pool, tagged with its graph id + construction identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolNode {
    /// Index of the source row in the caller's `rows` slice (live build
    /// re-reads the typed row for its `Address` values).
    pub row_index: usize,
    /// The pathfinding graph id (V4 ids offset by [`V4_POOL_ID_OFFSET`]).
    pub graph_id: u64,
    /// The raw pool id (V2/V3 `pools.id`; V4 `managed_pools.id`).
    pub raw_id: u64,
    /// The pool family.
    pub kind: PoolKind,
    /// The raw DB `kind` string (e.g. `"uniswap_v3"`).
    pub kind_string: String,
    /// `token0_id` / `currency0_id`.
    pub token0_id: u64,
    /// `token1_id` / `currency1_id`.
    pub token1_id: u64,
    /// `token0` / `currency0` on-chain address (lowercase hex).
    pub token0: String,
    /// `token1` / `currency1` on-chain address (lowercase hex).
    pub token1: String,
    /// V2/V3 pool address (lowercase, `None` for V4).
    pub address: Option<String>,
    /// V4 pool hash (lowercase hex, `None` for V2/V3).
    pub pool_hash: Option<String>,
    /// V4 pool manager address (lowercase, `None` for V2/V3).
    pub manager: Option<String>,
    /// V4 `StateView` address (lowercase, `None` for V2/V3).
    pub state_view: Option<String>,
    /// The duplicate-guard identity (mirrors `policy._pool_identity`).
    pub identity: String,
    /// The negative-memo key (mirrors `_registration_ledger.pool_memo_key`).
    pub memo_key: Option<String>,
}

/// The edges + node index a discovery graph build produces.
pub struct BuiltGraph {
    /// The (dead-end-pruned) pathfinding graph.
    pub graph: PathGraph,
    /// Nodes in edge-insertion order, indexed by [`Self::by_graph_id`].
    pub nodes: Vec<PoolNode>,
    /// graph id → index into [`Self::nodes`].
    pub by_graph_id: HashMap<u64, usize>,
    /// lowercase token address → `erc20_tokens.id`.
    pub token_id_by_lower: HashMap<String, u64>,
    /// The candidate tokens that survived the degree + allowlist filter.
    pub candidate_tokens: BTreeSet<u64>,
}

fn lower_address(address: &str) -> String {
    address.to_lowercase()
}

/// Convert one discovery row into a [`PoolNode`].
fn node_from_row(row: &DiscoveryPoolRow, row_index: usize) -> PoolNode {
    match row {
        DiscoveryPoolRow::V2(r) => {
            let raw_id = u64::try_from(r.pool.id).unwrap_or(0);
            let address = lower_address(&address_to_checksum_string(&r.pool.address));
            PoolNode {
                row_index,
                graph_id: raw_id,
                raw_id,
                kind: PoolKind::V2,
                kind_string: r.pool.kind.clone(),
                token0_id: u64::try_from(r.pool.token0_id).unwrap_or(0),
                token1_id: u64::try_from(r.pool.token1_id).unwrap_or(0),
                token0: lower_address(&address_to_checksum_string(&r.token0.address)),
                token1: lower_address(&address_to_checksum_string(&r.token1.address)),
                address: Some(address.clone()),
                pool_hash: None,
                manager: None,
                state_view: None,
                identity: address.clone(),
                memo_key: Some(format!("p:{address}")),
            }
        }
        DiscoveryPoolRow::V3(r) => {
            let raw_id = u64::try_from(r.pool.id).unwrap_or(0);
            let address = lower_address(&address_to_checksum_string(&r.pool.address));
            PoolNode {
                row_index,
                graph_id: raw_id,
                raw_id,
                kind: PoolKind::V3,
                kind_string: r.pool.kind.clone(),
                token0_id: u64::try_from(r.pool.token0_id).unwrap_or(0),
                token1_id: u64::try_from(r.pool.token1_id).unwrap_or(0),
                token0: lower_address(&address_to_checksum_string(&r.token0.address)),
                token1: lower_address(&address_to_checksum_string(&r.token1.address)),
                address: Some(address.clone()),
                pool_hash: None,
                manager: None,
                state_view: None,
                identity: address.clone(),
                memo_key: Some(format!("p:{address}")),
            }
        }
        DiscoveryPoolRow::V4(r) => {
            let raw_id = u64::try_from(r.managed_pool_id).unwrap_or(0);
            let graph_id = raw_id + V4_POOL_ID_OFFSET;
            let hash_hex = format!("{:#x}", r.pool_hash);
            let manager = lower_address(&address_to_checksum_string(&r.manager.address));
            let state_view = r
                .manager
                .state_view
                .map(|address| lower_address(&address_to_checksum_string(&address)));
            PoolNode {
                row_index,
                graph_id,
                raw_id,
                kind: PoolKind::V4,
                kind_string: r.manager.kind.clone(),
                token0_id: u64::try_from(r.token0.id).unwrap_or(0),
                token1_id: u64::try_from(r.token1.id).unwrap_or(0),
                token0: lower_address(&address_to_checksum_string(&r.token0.address)),
                token1: lower_address(&address_to_checksum_string(&r.token1.address)),
                address: None,
                pool_hash: Some(hash_hex.clone()),
                manager: Some(manager),
                state_view,
                identity: hash_hex.clone(),
                memo_key: Some(format!("v4id:{hash_hex}")),
            }
        }
    }
}

/// Build the pathfinding graph from the G2 discovery rows.
///
/// Mirror of `fetch_graph_data` + `PathGraph::from_edges` +
/// `prune_dead_ends` over already-read rows:
/// 1. keep rows whose family is in `requested_kinds`;
/// 2. candidate tokens = tokens incident to >= 2 kept edges;
/// 3. when `allowed_intermediate_lower` is set, retain only those tokens
///    (mirrors the Python whitelist intersection — boundary tokens must be in
///    the set too, exactly as `build_path_graph` does);
/// 4. keep edges whose BOTH endpoints survive;
/// 5. build + prune.
#[must_use]
pub fn build_graph(
    rows: &[DiscoveryPoolRow],
    requested_kinds: &[PoolKind],
    allowed_intermediate_lower: Option<&BTreeSet<String>>,
) -> BuiltGraph {
    let mut nodes: Vec<PoolNode> = Vec::with_capacity(rows.len());
    let mut raw_edges: Vec<(u64, u64, u64, PoolKind)> = Vec::with_capacity(rows.len());
    let mut token_id_by_lower: HashMap<String, u64> = HashMap::new();

    for (row_index, row) in rows.iter().enumerate() {
        let node = node_from_row(row, row_index);
        if !requested_kinds.contains(&node.kind) {
            continue;
        }
        token_id_by_lower.insert(node.token0.clone(), node.token0_id);
        token_id_by_lower.insert(node.token1.clone(), node.token1_id);
        raw_edges.push((node.token0_id, node.token1_id, node.graph_id, node.kind));
        nodes.push(node);
    }

    // Candidate tokens: incident to >= 2 edges (mirrors degree=2).
    let mut degree: BTreeMap<u64, usize> = BTreeMap::new();
    for (t0, t1, _, _) in &raw_edges {
        *degree.entry(*t0).or_insert(0) += 1;
        *degree.entry(*t1).or_insert(0) += 1;
    }
    let mut candidate_tokens: BTreeSet<u64> = degree
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(token, _)| token)
        .collect();

    if let Some(allowed) = allowed_intermediate_lower {
        let allowed_ids: BTreeSet<u64> = token_id_by_lower
            .iter()
            .filter(|(addr, _)| allowed.contains(*addr))
            .map(|(_, id)| *id)
            .collect();
        candidate_tokens.retain(|token| allowed_ids.contains(token));
    }

    let edges: Vec<(u64, u64, u64, PoolKind)> = raw_edges
        .into_iter()
        .filter(|(t0, t1, _, _)| candidate_tokens.contains(t0) && candidate_tokens.contains(t1))
        .collect();

    let by_graph_id: HashMap<u64, usize> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (node.graph_id, idx))
        .collect();

    let mut graph = PathGraph::from_edges(edges);
    graph.prune_dead_ends();

    BuiltGraph {
        graph,
        nodes,
        by_graph_id,
        token_id_by_lower,
        candidate_tokens,
    }
}

/// Discovery search parameters (mirrors `discovery_sweep`'s call shape).
#[derive(Clone, Debug)]
pub struct DiscoveryParams {
    /// Start token ids.
    pub start_tokens: Vec<u64>,
    /// End token ids.
    pub end_tokens: Vec<u64>,
    /// Minimum hops (Python `find_paths` default: 2).
    pub min_depth: usize,
    /// Maximum hops (`discovery_sweep`: 3).
    pub max_depth: Option<usize>,
    /// Per-depth pool-kind filter (from the permutation filter).
    pub pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    /// Paths per delivery batch (`discovery_batch_size`; `<= 1` = per-path).
    pub batch_size: usize,
}

impl Default for DiscoveryParams {
    fn default() -> Self {
        Self {
            start_tokens: Vec::new(),
            end_tokens: Vec::new(),
            min_depth: 2,
            max_depth: Some(3),
            pool_type_per_depth: None,
            batch_size: 1000,
        }
    }
}

/// The traversal plan (mirrors `_prepare_traversal_plan`).
///
/// Returns `(start, end, include_reverse)` entries: the Cartesian product is
/// consolidated so a forward path between two shared tokens also yields the
/// reverse without a second DFS.
#[must_use]
pub fn traversal_plan(start_tokens: &[u64], end_tokens: &[u64]) -> Vec<(u64, u64, bool)> {
    let starts: BTreeSet<u64> = start_tokens.iter().copied().collect();
    let ends: BTreeSet<u64> = end_tokens.iter().copied().collect();
    let mut plan: BTreeMap<(u64, u64), bool> = BTreeMap::new();
    for start in &starts {
        for end in &ends {
            plan.insert((*start, *end), false);
        }
    }
    let shared: Vec<u64> = starts.intersection(&ends).copied().collect();
    if shared.len() > 1 {
        for i in 0..shared.len() {
            for j in (i + 1)..shared.len() {
                plan.insert((shared[i], shared[j]), true);
                plan.remove(&(shared[j], shared[i]));
            }
        }
    }
    plan.into_iter()
        .map(|((start, end), reverse)| (start, end, reverse))
        .collect()
}

/// The effective minimum depth: a permutation filter implies an exact hop
/// count, so shorter prefix cycles must not leak (mirrors
/// `_pathfinding.find_paths`).
#[must_use]
pub fn effective_min_depth(min_depth: usize, filter: Option<&[Option<Vec<PoolKind>>]>) -> usize {
    filter.map_or(min_depth, |f| min_depth.max(f.len()))
}

/// A lazy, batch-delivering path finder over the traversal plan.
///
/// The graph is cloned per plan entry (each `OwnedPathFinder` owns its
/// graph, matching the `PyO3` iterator's ownership shape).
pub struct BatchedPathFinder {
    finders: Vec<OwnedPathFinder>,
    active: usize,
    batch_size: usize,
}

impl BatchedPathFinder {
    /// Build the finder from a graph + discovery parameters.
    #[must_use]
    pub fn new(graph: &PathGraph, params: &DiscoveryParams) -> Self {
        let min_depth =
            effective_min_depth(params.min_depth, params.pool_type_per_depth.as_deref());
        let finders: Vec<OwnedPathFinder> =
            traversal_plan(&params.start_tokens, &params.end_tokens)
                .into_iter()
                .map(|(start, end, reverse)| {
                    OwnedPathFinder::new(
                        graph.clone(),
                        start,
                        end,
                        min_depth,
                        params.max_depth,
                        reverse,
                        params.pool_type_per_depth.clone(),
                    )
                })
                .collect();
        Self {
            finders,
            active: 0,
            batch_size: params.batch_size,
        }
    }

    /// The next delivery batch, or `None` when the search is exhausted.
    ///
    /// `batch_size <= 1` delivers one path per call (the legacy per-path
    /// mode); larger values deliver up to `batch_size` paths.
    pub fn next_batch(&mut self) -> Option<Vec<Vec<(u64, PoolKind)>>> {
        let batch_size = self.batch_size.max(1);
        let mut batch: Vec<Vec<(u64, PoolKind)>> = Vec::with_capacity(batch_size);
        while self.active < self.finders.len() {
            let finder = &mut self.finders[self.active];
            match finder.next_path() {
                Some(path) => {
                    batch.push(path);
                    if batch.len() >= batch_size {
                        return Some(batch);
                    }
                }
                None => {
                    self.active += 1;
                }
            }
        }
        if batch.is_empty() {
            None
        } else {
            Some(batch)
        }
    }
}

/// Drive a [`BatchedPathFinder`] as one async loop, yielding the event loop
/// once per batch and folding every path into `on_path`.
///
/// Returns the number of paths delivered. Mirrors `find_paths_async`'s
/// batch cadence (one loop hop per batch) without its worker thread/queue —
/// see the module docs for the documented difference.
pub async fn drive_batched<F>(finder: &mut BatchedPathFinder, mut on_path: F) -> usize
where
    F: FnMut(&[(u64, PoolKind)]),
{
    let mut total = 0_usize;
    while let Some(batch) = finder.next_batch() {
        for path in &batch {
            on_path(path);
            total += 1;
        }
        tokio::task::yield_now().await;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-hop cycle with a parallel pool: 1-2 via pools 1 and 2 yields
    /// more than one distinct 2-hop cycle.
    fn parallel_graph() -> PathGraph {
        let edges: Vec<(u64, u64, u64, PoolKind)> =
            vec![(1, 2, 1, PoolKind::V3), (1, 2, 2, PoolKind::V3)];
        let mut graph = PathGraph::from_edges(edges);
        graph.prune_dead_ends();
        graph
    }

    fn params(batch_size: usize) -> DiscoveryParams {
        DiscoveryParams {
            start_tokens: vec![1],
            end_tokens: vec![1],
            min_depth: 2,
            max_depth: Some(2),
            pool_type_per_depth: None,
            batch_size,
        }
    }

    #[test]
    fn v4_graph_ids_are_namespaced_above_v2v3() {
        assert_eq!(V4_POOL_ID_OFFSET, 1 << 32);
        assert_eq!(lower_address(NATIVE_CURRENCY), NATIVE_CURRENCY);
    }

    #[test]
    fn traversal_plan_consolidates_forward_and_reverse() {
        let plan = traversal_plan(&[1, 2], &[1, 2]);
        assert!(plan.contains(&(1, 1, false)));
        assert!(plan.contains(&(2, 2, false)));
        assert!(plan.contains(&(1, 2, true)));
        assert!(!plan.contains(&(2, 1, false)));
    }

    #[test]
    fn effective_min_depth_honors_the_filter_length() {
        let filter: Vec<Option<Vec<PoolKind>>> = vec![
            Some(vec![PoolKind::V3]),
            Some(vec![PoolKind::V4]),
            Some(vec![PoolKind::V3]),
        ];
        assert_eq!(effective_min_depth(2, Some(&filter)), 3);
        assert_eq!(effective_min_depth(2, None), 2);
    }

    #[test]
    fn batches_respect_batch_size_and_cover_the_same_paths() {
        let graph = parallel_graph();
        let mut per_path = BatchedPathFinder::new(&graph, &params(1));
        let mut per_path_total = 0;
        let mut per_path_batch_sizes = Vec::new();
        while let Some(batch) = per_path.next_batch() {
            per_path_batch_sizes.push(batch.len());
            per_path_total += batch.len();
        }
        assert!(
            per_path_total > 0,
            "parallel pools yield at least one cycle"
        );
        assert!(per_path_batch_sizes.iter().all(|n| *n == 1));

        let mut bulk = BatchedPathFinder::new(&graph, &params(10));
        let mut bulk_total = 0;
        while let Some(batch) = bulk.next_batch() {
            assert!(batch.len() <= 10);
            bulk_total += batch.len();
        }
        assert_eq!(
            per_path_total, bulk_total,
            "batching never changes the path set"
        );
    }

    #[tokio::test]
    async fn drive_batched_delivers_every_path() {
        let graph = parallel_graph();
        let mut finder = BatchedPathFinder::new(&graph, &params(2));
        let mut count = 0;
        let total = drive_batched(&mut finder, |_path| count += 1).await;
        assert_eq!(total, count);
        assert!(total > 0);
    }
}
