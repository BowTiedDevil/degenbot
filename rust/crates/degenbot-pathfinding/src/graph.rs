//! The pathfinding multigraph and iterative depth-first search.
//!
//! This module is the pure-Rust core of the pathfinding algorithm. It
//! replaces the Python `networkx.MultiGraph` + recursive `_dfs` with a lean
//! adjacency-list graph and an iterative DFS using a `HashSet` visited-set
//! for O(1) cycle detection.

use std::collections::{HashMap, HashSet};

/// Discriminant for the three pool-table families.
///
/// `V2` corresponds to `UniswapV2PoolTableBase` (Uniswap V2 and V2-style
/// forks); `V3` corresponds to `UniswapV3PoolTableBase` (Uniswap V3 and
/// V3-style forks); `V4` corresponds to `UniswapV4PoolTable`.
///
/// All V2/V3 subtypes share the `pools` database table (single ID
/// sequence), so a `pool_id` is unique within V2 and within V3 — no
/// collision between the two. V4 pools use a separate `managed_pools`
/// table, so the `PoolKind` discriminant disambiguates V4 IDs from V2/V3
/// IDs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PoolKind {
    V2,
    V3,
    V4,
}

impl PoolKind {
    /// Convert to the `u8` discriminant used at the `PyO3` boundary.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            PoolKind::V2 => 0,
            PoolKind::V3 => 1,
            PoolKind::V4 => 2,
        }
    }

    /// Convert from the `u8` discriminant used at the `PyO3` boundary.
    ///
    /// Returns `None` for unknown discriminants.
    #[must_use]
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(PoolKind::V2),
            1 => Some(PoolKind::V3),
            2 => Some(PoolKind::V4),
            _ => None,
        }
    }
}

/// A pool edge in the graph: which pool connects a node to its neighbor.
///
/// Stored in the adjacency list of the source node; `neighbor` is the
/// token ID on the other end of the pool.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Edge {
    /// The token ID this edge leads to.
    pub neighbor: u64,
    /// The pool ID providing this liquidity connection.
    pub pool_id: u64,
    /// Which pool-table family this pool belongs to.
    pub pool_kind: PoolKind,
}

/// A key uniquely identifying a pool within a traversal.
pub type EdgeKey = (u64, PoolKind);

/// A multigraph: token IDs are nodes, liquidity pools are edges.
///
/// Stored as an adjacency list (`HashMap<u64, Vec<Edge>>`) that preserves
/// edge insertion order for deterministic traversal. Parallel edges
/// (multiple pools connecting the same token pair) are naturally supported.
pub struct PathGraph {
    adj: HashMap<u64, Vec<Edge>>,
}

impl PathGraph {
    /// Build from a flat list of `(token0, token1, pool_id, pool_kind)` edges.
    ///
    /// Each edge is added in both directions (the graph is undirected, like
    /// the `networkx.MultiGraph` it replaces). Edge insertion order within
    /// each node's adjacency list is preserved for deterministic traversal.
    #[must_use]
    pub fn from_edges(edges: Vec<(u64, u64, u64, PoolKind)>) -> Self {
        let mut adj: HashMap<u64, Vec<Edge>> = HashMap::new();
        for (token0, token1, pool_id, pool_kind) in edges {
            adj.entry(token0).or_default().push(Edge {
                neighbor: token1,
                pool_id,
                pool_kind,
            });
            adj.entry(token1).or_default().push(Edge {
                neighbor: token0,
                pool_id,
                pool_kind,
            });
        }
        Self { adj }
    }

    /// Returns `true` if the node exists in the graph.
    #[must_use]
    pub fn contains_node(&self, node: u64) -> bool {
        self.adj.contains_key(&node)
    }

    /// The number of nodes (tokens) in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.adj.len()
    }

    /// Remove nodes with degree ≤ 1, repeating until no such nodes remain.
    ///
    /// This mirrors the Python `_prepare_graph` dead-end pruning loop:
    /// `while tokens_to_prune := tuple(t for t, d in graph.degree() if d <= 1):
    ///     graph.remove_nodes_from(tokens_to_prune)`
    ///
    /// Pruning a node removes all its incident edges, which may reduce other
    /// nodes' degrees below 2 — hence the iterative fixpoint.
    pub fn prune_dead_ends(&mut self) {
        loop {
            // Collect nodes whose total degree (count of incident edges) is ≤ 1.
            let to_prune: Vec<u64> = self
                .adj
                .iter()
                .filter_map(
                    |(&node, edges)| {
                        if edges.is_empty() {
                            Some(node)
                        } else {
                            None
                        }
                    },
                )
                .collect();

            // Also prune nodes with exactly one edge (degree 1).
            // Nodes with zero edges shouldn't exist after from_edges (every
            // node has at least one edge), but prune them defensively.
            if to_prune.is_empty() {
                // Check for degree-1 nodes
                let deg_one: Vec<u64> = self
                    .adj
                    .iter()
                    .filter(|(_, edges)| edges.len() == 1)
                    .map(|(&node, _)| node)
                    .collect();
                if deg_one.is_empty() {
                    break;
                }
                self.remove_nodes(&deg_one);
                // After removal, some nodes may have dropped to degree 0 or 1.
            } else {
                self.remove_nodes(&to_prune);
            }
        }
    }

    /// Remove a set of nodes and all their incident edges.
    fn remove_nodes(&mut self, nodes: &[u64]) {
        // Collect all reverse-edge removals first to avoid borrowing self.adj
        // as both immutable (reading edges) and mutable (removing from neighbors).
        let mut reverse_removals: Vec<(u64, u64, PoolKind)> = Vec::new();
        for &node in nodes {
            if let Some(edges) = self.adj.get(&node) {
                for edge in edges {
                    reverse_removals.push((edge.neighbor, edge.pool_id, edge.pool_kind));
                }
            }
        }

        // Apply reverse-edge removals from neighbors.
        for (neighbor, pool_id, pool_kind) in reverse_removals {
            if let Some(neighbor_edges) = self.adj.get_mut(&neighbor) {
                neighbor_edges.retain(|e| !(e.pool_id == pool_id && e.pool_kind == pool_kind));
            }
        }

        // Remove the nodes themselves.
        for &node in nodes {
            self.adj.remove(&node);
        }

        // Remove any nodes that are now empty (lost all their edges to pruned nodes).
        self.adj.retain(|_, edges| !edges.is_empty());
    }

    /// Precompute valid depth positions per node, for lookahead pruning.
    ///
    /// For each node, determine which depth positions its edges satisfy. A
    /// node can appear at depth `d` if it has at least one incident edge whose
    /// `pool_kind` is in the allowed set at depth `d` (or `allowed[d]` is
    /// `None`, meaning all kinds are allowed).
    ///
    /// Returns a map from token ID to a `Vec<bool>` where index `d` is `true`
    /// if the node can participate at depth `d`.
    #[must_use]
    pub fn compute_node_valid_depths(
        &self,
        pool_type_per_depth: &[Option<Vec<PoolKind>>],
    ) -> HashMap<u64, Vec<bool>> {
        let mut result = HashMap::new();
        for (&node, edges) in &self.adj {
            // Collect all pool kinds this node has edges for.
            let node_kinds: HashSet<PoolKind> = edges.iter().map(|e| e.pool_kind).collect();

            let mut valid = vec![false; pool_type_per_depth.len()];
            for (d, allowed) in pool_type_per_depth.iter().enumerate() {
                match allowed {
                    None => valid[d] = true,
                    Some(allowed_kinds) => {
                        valid[d] = node_kinds.iter().any(|k| allowed_kinds.contains(k));
                    }
                }
            }
            result.insert(node, valid);
        }
        result
    }
}

/// A lazy, stateful depth-first search iterator over valid cycles.
///
/// This struct holds the DFS stack, working path, and visited set,
/// yielding one path at a time via [`PathFinder::next_path`]. This avoids
/// collecting all results into memory at once — essential for large
/// graphs that produce millions of paths.
///
/// Created by [`PathGraph::find_paths_iter`].
pub struct PathFinder<'a> {
    graph: &'a PathGraph,
    end: u64,
    min_depth: usize,
    effective_max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<&'a [Option<Vec<PoolKind>>]>,
    node_valid_depths: Option<&'a HashMap<u64, Vec<bool>>>,
    filter_len: usize,
    stack: Vec<(u64, usize, bool)>,
    working_path: Vec<EdgeKey>,
    visited: HashSet<EdgeKey>,
    pending_reverse: Option<Vec<EdgeKey>>,
    done: bool,
}

impl PathFinder<'_> {
    /// Advance the DFS and return the next complete path, or `None` if
    /// the search is exhausted.
    ///
    /// If `include_reverse` is set, each found cycle yields the forward path
    /// first, then the reversed path on the next call.
    #[must_use]
    pub fn next_path(&mut self) -> Option<Vec<EdgeKey>> {
        if self.done {
            return None;
        }

        // If a reversed path is pending from the last yield, emit it now.
        if let Some(rev) = self.pending_reverse.take() {
            return Some(rev);
        }

        while let Some(frame) = self.stack.last_mut() {
            let (node, edge_idx, yield_checked) = frame;

            // Check yield condition (once per frame arrival).
            if !*yield_checked {
                *yield_checked = true;
                if *node == self.end && self.working_path.len() >= self.min_depth {
                    let path = self.working_path.clone();
                    if self.include_reverse {
                        let mut rev = self.working_path.clone();
                        rev.reverse();
                        self.pending_reverse = Some(rev);
                    }
                    return Some(path);
                }
            }

            // Stop recursion if the working path has reached the maximum depth.
            if let Some(emd) = self.effective_max_depth {
                if self.working_path.len() >= emd {
                    // Backtrack.
                    self.stack.pop();
                    if let Some(popped) = self.working_path.pop() {
                        self.visited.remove(&popped);
                    }
                    continue;
                }
            }

            // Find the next valid edge to explore from this node.
            let neighbors = if let Some(n) = self.graph.adj.get(node) {
                n.as_slice()
            } else {
                // No edges from this node — backtrack.
                self.stack.pop();
                continue;
            };

            let mut found_edge = false;
            while *edge_idx < neighbors.len() {
                let edge = &neighbors[*edge_idx];
                *edge_idx += 1;
                let edge_key = (edge.pool_id, edge.pool_kind);

                // Cycle detection: skip edges already on the working path.
                if self.visited.contains(&edge_key) {
                    continue;
                }

                // Per-depth pool-type filter.
                if let Some(filter) = self.pool_type_per_depth {
                    let depth = self.working_path.len();
                    // depth < filter_len is guaranteed by effective_max_depth,
                    // but guard defensively.
                    if depth >= self.filter_len {
                        continue;
                    }
                    if let Some(allowed_kinds) = &filter[depth] {
                        if !allowed_kinds.contains(&edge.pool_kind) {
                            continue;
                        }
                    }

                    // Lookahead pruning: skip if the neighbor can't continue
                    // at the next depth.
                    let next_depth = depth + 1;
                    if next_depth < self.filter_len {
                        if let Some(nvd) = self.node_valid_depths {
                            if let Some(valid) = nvd.get(&edge.neighbor) {
                                if !valid[next_depth] {
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Found a valid edge — extend the path and push the neighbor.
                self.working_path.push(edge_key);
                self.visited.insert(edge_key);
                self.stack.push((edge.neighbor, 0, false));
                found_edge = true;
                break;
            }

            if !found_edge {
                // No more edges to explore from this node — backtrack.
                self.stack.pop();
                if let Some(popped) = self.working_path.pop() {
                    self.visited.remove(&popped);
                }
            }
        }

        // Search exhausted.
        self.done = true;
        None
    }
}

impl Iterator for PathFinder<'_> {
    type Item = Vec<EdgeKey>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_path()
    }
}

/// An owning, lazy DFS iterator that owns the graph and filter data.
///
/// This is the PyO3-friendly version of [`PathFinder`] — it has no lifetime
/// parameters, so it can be stored in a `#[pyclass]` and iterated from
/// Python one path at a time. The graph, filter, and node-valid-depths are
/// all owned, eliminating self-referential borrow issues.
pub struct OwnedPathFinder {
    graph: PathGraph,
    end: u64,
    min_depth: usize,
    effective_max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    node_valid_depths: Option<HashMap<u64, Vec<bool>>>,
    filter_len: usize,
    stack: Vec<(u64, usize, bool)>,
    working_path: Vec<EdgeKey>,
    visited: HashSet<EdgeKey>,
    pending_reverse: Option<Vec<EdgeKey>>,
    done: bool,
}

impl OwnedPathFinder {
    /// Create from owned graph + search parameters.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        graph: PathGraph,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    ) -> Self {
        let effective_max_depth: Option<usize> = match &pool_type_per_depth {
            Some(filter) => {
                let filter_len = filter.len();
                match max_depth {
                    Some(md) => Some(md.min(filter_len)),
                    None => Some(filter_len),
                }
            }
            None => max_depth,
        };

        let filter_len = pool_type_per_depth.as_ref().map_or(0, Vec::len);

        let node_valid_depths = pool_type_per_depth
            .as_ref()
            .map(|filter| graph.compute_node_valid_depths(filter));

        let stack = if graph.contains_node(start) {
            vec![(start, 0, false)]
        } else {
            Vec::new()
        };
        let done = stack.is_empty();

        Self {
            graph,
            end,
            min_depth,
            effective_max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
            filter_len,
            stack,
            working_path: Vec::new(),
            visited: HashSet::new(),
            pending_reverse: None,
            done,
        }
    }

    /// Advance the DFS and return the next complete path, or `None` if
    /// the search is exhausted.
    ///
    /// If `include_reverse` is set, each found cycle yields the forward path
    /// first, then the reversed path on the next call.
    #[must_use]
    pub fn next_path(&mut self) -> Option<Vec<EdgeKey>> {
        if self.done {
            return None;
        }

        // If a reversed path is pending from the last yield, emit it now.
        if let Some(rev) = self.pending_reverse.take() {
            return Some(rev);
        }

        let filter_slice = self.pool_type_per_depth.as_deref();
        let nvd_ref = self.node_valid_depths.as_ref();

        while let Some(frame) = self.stack.last_mut() {
            let (node, edge_idx, yield_checked) = frame;

            // Check yield condition (once per frame arrival).
            if !*yield_checked {
                *yield_checked = true;
                if *node == self.end && self.working_path.len() >= self.min_depth {
                    let path = self.working_path.clone();
                    if self.include_reverse {
                        let mut rev = self.working_path.clone();
                        rev.reverse();
                        self.pending_reverse = Some(rev);
                    }
                    return Some(path);
                }
            }

            // Stop recursion if the working path has reached the maximum depth.
            if let Some(emd) = self.effective_max_depth {
                if self.working_path.len() >= emd {
                    // Backtrack.
                    self.stack.pop();
                    if let Some(popped) = self.working_path.pop() {
                        self.visited.remove(&popped);
                    }
                    continue;
                }
            }

            // Find the next valid edge to explore from this node.
            let neighbors = if let Some(n) = self.graph.adj.get(node) {
                n.as_slice()
            } else {
                // No edges from this node — backtrack.
                self.stack.pop();
                continue;
            };

            let mut found_edge = false;
            while *edge_idx < neighbors.len() {
                let edge = &neighbors[*edge_idx];
                *edge_idx += 1;
                let edge_key = (edge.pool_id, edge.pool_kind);

                // Cycle detection: skip edges already on the working path.
                if self.visited.contains(&edge_key) {
                    continue;
                }

                // Per-depth pool-type filter.
                if let Some(filter) = filter_slice {
                    let depth = self.working_path.len();
                    if depth >= self.filter_len {
                        continue;
                    }
                    if let Some(allowed_kinds) = &filter[depth] {
                        if !allowed_kinds.contains(&edge.pool_kind) {
                            continue;
                        }
                    }

                    // Lookahead pruning.
                    let next_depth = depth + 1;
                    if next_depth < self.filter_len {
                        if let Some(nvd) = nvd_ref {
                            if let Some(valid) = nvd.get(&edge.neighbor) {
                                if !valid[next_depth] {
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Found a valid edge — extend the path and push the neighbor.
                self.working_path.push(edge_key);
                self.visited.insert(edge_key);
                self.stack.push((edge.neighbor, 0, false));
                found_edge = true;
                break;
            }

            if !found_edge {
                // No more edges to explore from this node — backtrack.
                self.stack.pop();
                if let Some(popped) = self.working_path.pop() {
                    self.visited.remove(&popped);
                }
            }
        }

        // Search exhausted.
        self.done = true;
        None
    }
}

impl Iterator for OwnedPathFinder {
    type Item = Vec<EdgeKey>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_path()
    }
}

impl PathGraph {
    /// Create a lazy iterator over all valid paths from `start` back to `end`.
    ///
    /// This is a stateful, resumable version of the DFS. The iterator yields
    /// one path at a time, avoiding the memory cost of collecting all results
    /// into a `Vec`. Use this when the graph may produce a large number of
    /// paths.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn find_paths_iter<'a>(
        &'a self,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<&'a [Option<Vec<PoolKind>>]>,
        node_valid_depths: Option<&'a HashMap<u64, Vec<bool>>>,
    ) -> PathFinder<'a> {
        let effective_max_depth: Option<usize> = match pool_type_per_depth {
            Some(filter) => {
                let filter_len = filter.len();
                match max_depth {
                    Some(md) => Some(md.min(filter_len)),
                    None => Some(filter_len),
                }
            }
            None => max_depth,
        };

        let filter_len = pool_type_per_depth.map_or(0, <[Option<Vec<PoolKind>>]>::len);

        let stack = if self.adj.contains_key(&start) {
            vec![(start, 0, false)]
        } else {
            Vec::new()
        };
        let done = stack.is_empty();

        PathFinder {
            graph: self,
            end,
            min_depth,
            effective_max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
            filter_len,
            stack,
            working_path: Vec::new(),
            visited: HashSet::new(),
            pending_reverse: None,
            done,
        }
    }

    /// Depth-first search for all valid paths from `start` back to `end`.
    ///
    /// This is an eager version that collects all results. For large graphs
    /// that may produce millions of paths, use [`PathGraph::find_paths_iter`]
    /// instead to avoid excessive memory usage.
    ///
    /// # Arguments
    /// * `start` — The token ID where the search begins.
    /// * `end` — The token ID the path must return to.
    /// * `min_depth` — Minimum number of hops in a completed path.
    /// * `max_depth` — Maximum number of hops, or `None` for no limit.
    /// * `include_reverse` — If `true`, yield each found path again reversed.
    /// * `pool_type_per_depth` — Optional per-depth allowed pool kinds. A
    ///   `None` entry allows all kinds at that depth. Implicitly caps max
    ///   depth at its length.
    /// * `node_valid_depths` — Optional precomputed valid-depth sets (from
    ///   `compute_node_valid_depths`) for lookahead pruning.
    ///
    /// # Returns
    /// A `Vec` of paths, each a `Vec` of `(pool_id, PoolKind)` hops.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn find_paths(
        &self,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<&[Option<Vec<PoolKind>>]>,
        node_valid_depths: Option<&HashMap<u64, Vec<bool>>>,
    ) -> Vec<Vec<EdgeKey>> {
        self.find_paths_iter(
            start,
            end,
            min_depth,
            max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
        )
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Token IDs for the synthetic 4-pool V2 fixture (mirrors the in-memory
    // DB fixture from test_permutation_filter_min_depth.py).
    // Graph:
    //     WETH ===pool1=== A
    //     WETH ===pool2=== A      (parallel edge -> 2-hop cycle WETH-A-WETH)
    //     A     ===pool3=== B
    //     B     ===pool4=== WETH   (completes 3-hop cycle WETH-A-B-WETH)
    const WETH: u64 = 1;
    const A: u64 = 2;
    const B: u64 = 3;
    const POOL_WETH_A_1: u64 = 100;
    const POOL_WETH_A_2: u64 = 101;
    const POOL_A_B: u64 = 102;
    const POOL_B_WETH: u64 = 103;

    fn build_fixture_graph() -> PathGraph {
        PathGraph::from_edges(vec![
            (WETH, A, POOL_WETH_A_1, PoolKind::V2),
            (WETH, A, POOL_WETH_A_2, PoolKind::V2),
            (A, B, POOL_A_B, PoolKind::V2),
            (B, WETH, POOL_B_WETH, PoolKind::V2),
        ])
    }

    fn edges_to_pool_ids(path: &[EdgeKey]) -> Vec<u64> {
        path.iter().map(|(pid, _)| *pid).collect()
    }

    #[test]
    fn test_from_edges_builds_adjacency() {
        let graph = build_fixture_graph();
        assert_eq!(graph.node_count(), 3); // WETH, A, B
        assert!(graph.contains_node(WETH));
        assert!(graph.contains_node(A));
        assert!(graph.contains_node(B));
    }

    #[test]
    fn test_parallel_edges_preserved() {
        let graph = build_fixture_graph();
        // WETH has 3 edges: pool1->A, pool2->A, pool4->B
        let weth_edges = &graph.adj[&WETH];
        assert_eq!(weth_edges.len(), 3);
        // A has 3 edges: pool1->WETH, pool2->WETH, pool3->B
        let a_edges = &graph.adj[&A];
        assert_eq!(a_edges.len(), 3);
    }

    #[test]
    fn test_prune_dead_ends() {
        // Build a graph with a dead-end chain: A-B-C where C only connects to B.
        // After pruning, C (degree 1) is removed, then B (now degree 1) is removed.
        let mut graph = PathGraph::from_edges(vec![
            (A, B, 1, PoolKind::V2),
            (B, 99, 2, PoolKind::V2), // 99 is a dead end (degree 1)
        ]);
        graph.prune_dead_ends();
        // 99 is removed (degree 1). Then B has degree 1 (only edge to A).
        // Wait — A-B is bidirectional, so A has 1 edge (to B) and B has 1 edge
        // (to A) after 99 is removed. Both get pruned.
        // Actually: A has edges [B], B has edges [A, 99]. 99 has edges [B].
        // 99 (degree 1) pruned. Now B has edges [A] (degree 1) pruned.
        // Now A has edges [B] but B is removed, so A has 0 edges pruned.
        assert!(!graph.contains_node(99));
        assert!(!graph.contains_node(B));
        assert!(!graph.contains_node(A));
    }

    #[test]
    fn test_prune_preserves_cycle() {
        // The fixture graph has cycles; pruning should not remove WETH, A, B.
        let mut graph = build_fixture_graph();
        graph.prune_dead_ends();
        assert!(graph.contains_node(WETH));
        assert!(graph.contains_node(A));
        assert!(graph.contains_node(B));
    }

    #[test]
    fn test_two_hop_pathfinding() {
        // WETH -> A -> WETH (2-hop cycle via parallel edges)
        let graph = build_fixture_graph();
        let paths = graph.find_paths(WETH, WETH, 2, Some(2), false, None, None);
        assert!(!paths.is_empty(), "Should find 2-hop WETH cycles");
        for path in &paths {
            assert_eq!(path.len(), 2, "Each path should be exactly 2 hops");
        }
    }

    #[test]
    fn test_three_hop_pathfinding() {
        // WETH -> A -> B -> WETH (3-hop cycle)
        let graph = build_fixture_graph();
        let paths = graph.find_paths(WETH, WETH, 3, Some(3), false, None, None);
        assert!(!paths.is_empty(), "Should find 3-hop WETH cycles");
        for path in &paths {
            assert_eq!(path.len(), 3, "Each path should be exactly 3 hops");
        }
    }

    #[test]
    fn test_min_depth_excludes_shorter() {
        // With min_depth=3, no 2-hop paths should be yielded.
        let graph = build_fixture_graph();
        let paths = graph.find_paths(WETH, WETH, 3, Some(3), false, None, None);
        for path in &paths {
            assert_eq!(path.len(), 3, "min_depth=3 should exclude shorter paths");
        }
    }

    #[test]
    fn test_max_depth_caps() {
        // With max_depth=2, no 3-hop paths.
        let graph = build_fixture_graph();
        let paths = graph.find_paths(WETH, WETH, 2, Some(2), false, None, None);
        for path in &paths {
            assert!(path.len() <= 2, "max_depth=2 should cap path length");
        }
    }

    #[test]
    fn test_include_reverse_doubles_output() {
        let graph = build_fixture_graph();
        let forward = graph.find_paths(WETH, WETH, 2, Some(2), false, None, None);
        let with_reverse = graph.find_paths(WETH, WETH, 2, Some(2), true, None, None);
        assert_eq!(
            with_reverse.len(),
            forward.len() * 2,
            "include_reverse should double the output count"
        );
    }

    #[test]
    fn test_three_hop_filter_yields_no_two_hop_cycles() {
        // A 3-depth V2-V2-V2 filter must yield only 3-hop paths.
        // The synthetic graph contains both a 2-hop cycle (WETH-A-WETH via
        // parallel pools) and a 3-hop cycle (WETH-A-B-WETH). The 2-hop cycle
        // matches the filter's depths 0 and 1, so without the implicit
        // min_depth floor from the filter length, it would leak through.
        let graph = build_fixture_graph();
        let filter = vec![
            Some(vec![PoolKind::V2]),
            Some(vec![PoolKind::V2]),
            Some(vec![PoolKind::V2]),
        ];
        let nvd = graph.compute_node_valid_depths(&filter);
        let _paths = graph.find_paths(
            WETH,
            WETH,
            2,       // caller min_depth
            Some(3), // caller max_depth
            false,
            Some(&filter),
            Some(&nvd),
        );

        // The filter caps max_depth at 3 and the effective min_depth should
        // be max(2, 3) = 3 (floor applied by the Python caller). But the Rust
        // core does NOT apply the floor — the caller does. Here we test with
        // min_depth=2 to verify that the filter alone does not leak 2-hop
        // paths... actually, it CAN leak 2-hop paths if min_depth=2.
        //
        // The Python find_paths applies: effective_min_depth = max(min_depth,
        // len(pool_type_per_depth)). So the caller would pass min_depth=3.
        // Let's test that explicitly:
        let paths_floored = graph.find_paths(
            WETH,
            WETH,
            3, // effective min_depth = max(2, 3) = 3
            Some(3),
            false,
            Some(&filter),
            Some(&nvd),
        );
        for path in &paths_floored {
            assert_eq!(
                path.len(),
                3,
                "3-depth filter with min_depth=3 should yield only 3-hop paths"
            );
        }
        assert!(
            !paths_floored.is_empty(),
            "3-depth filter should yield at least one 3-hop path"
        );
    }

    #[test]
    fn test_pool_type_per_depth_caps_max_depth() {
        // A 2-depth filter with max_depth=3 must not IndexError and must
        // cap at 2-hop paths.
        let graph = build_fixture_graph();
        let filter = vec![Some(vec![PoolKind::V2]), Some(vec![PoolKind::V2])];
        let nvd = graph.compute_node_valid_depths(&filter);
        let paths = graph.find_paths(
            WETH,
            WETH,
            2,
            Some(3), // exceeds filter length
            false,
            Some(&filter),
            Some(&nvd),
        );
        for path in &paths {
            assert_eq!(
                path.len(),
                2,
                "2-depth filter should cap at 2-hop paths even with max_depth=3"
            );
        }
    }

    #[test]
    fn test_pool_type_per_depth_with_max_depth_none() {
        // A 2-depth filter with max_depth=None must cap at 2-hop paths.
        let graph = build_fixture_graph();
        let filter = vec![Some(vec![PoolKind::V2]), Some(vec![PoolKind::V2])];
        let nvd = graph.compute_node_valid_depths(&filter);
        let paths = graph.find_paths(
            WETH,
            WETH,
            2,
            None, // no explicit max
            false,
            Some(&filter),
            Some(&nvd),
        );
        for path in &paths {
            assert_eq!(path.len(), 2);
        }
    }

    #[test]
    fn test_none_entry_allows_all_kinds() {
        // A filter with None at depth 0 allows all pool kinds.
        let graph = build_fixture_graph();
        let filter = vec![None, Some(vec![PoolKind::V4])];
        let nvd = graph.compute_node_valid_depths(&filter);
        // The fixture has only V2 pools, and depth 1 requires V4.
        // So no V4 paths should be found (node_valid_depths will show A and B
        // are invalid at depth 1).
        let paths = graph.find_paths(WETH, WETH, 2, Some(2), false, Some(&filter), Some(&nvd));
        // No V4 pools exist, so no paths match the filter.
        assert!(
            paths.is_empty(),
            "V4 filter on V2-only graph should yield nothing"
        );
    }

    #[test]
    fn test_cycle_detection_prevents_reusing_pools() {
        // A path must not visit the same pool twice.
        let graph = build_fixture_graph();
        let paths = graph.find_paths(WETH, WETH, 2, Some(3), false, None, None);
        for path in &paths {
            let pool_ids = edges_to_pool_ids(path);
            let unique: HashSet<u64> = pool_ids.iter().copied().collect();
            assert_eq!(
                pool_ids.len(),
                unique.len(),
                "Path should not reuse a pool: {pool_ids:?}"
            );
        }
    }

    #[test]
    fn test_node_not_in_graph_returns_empty() {
        let graph = build_fixture_graph();
        let paths = graph.find_paths(999, 999, 2, Some(2), false, None, None);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_poolkind_roundtrip() {
        assert_eq!(PoolKind::V2.as_u8(), 0);
        assert_eq!(PoolKind::V3.as_u8(), 1);
        assert_eq!(PoolKind::V4.as_u8(), 2);
        assert_eq!(PoolKind::from_u8(0), Some(PoolKind::V2));
        assert_eq!(PoolKind::from_u8(1), Some(PoolKind::V3));
        assert_eq!(PoolKind::from_u8(2), Some(PoolKind::V4));
        assert_eq!(PoolKind::from_u8(3), None);
    }

    #[test]
    fn test_mixed_pool_kinds() {
        // Build a graph with both V2 and V4 pools.
        // WETH --V2-- A --V4-- B --V2-- WETH (3-hop mixed cycle)
        let graph = PathGraph::from_edges(vec![
            (WETH, A, 1, PoolKind::V2),
            (A, B, 2, PoolKind::V4),
            (B, WETH, 3, PoolKind::V2),
        ]);
        let filter = vec![
            Some(vec![PoolKind::V2]),
            Some(vec![PoolKind::V4]),
            Some(vec![PoolKind::V2]),
        ];
        let nvd = graph.compute_node_valid_depths(&filter);
        let paths = graph.find_paths(WETH, WETH, 3, Some(3), false, Some(&filter), Some(&nvd));
        assert!(!paths.is_empty(), "Should find a V2-V4-V2 path");
        for path in &paths {
            assert_eq!(path.len(), 3);
            assert_eq!(path[0].1, PoolKind::V2);
            assert_eq!(path[1].1, PoolKind::V4);
            assert_eq!(path[2].1, PoolKind::V2);
        }
    }
}
