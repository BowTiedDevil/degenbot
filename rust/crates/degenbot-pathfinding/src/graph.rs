//! The pathfinding multigraph and iterative depth-first search.
//!
//! This module is the pure-Rust core of the pathfinding algorithm. It
//! replaces the Python `networkx.MultiGraph` + recursive `_dfs` with a lean
//! adjacency-list graph and an iterative DFS using a `Vec<bool>` visited-set
//! for O(1) cycle detection.
//!
//! # Performance design
//!
//! External token IDs (`u64`) are remapped to compact contiguous indices
//! (`u32`) at construction. The adjacency list is a flat `Vec<Vec<CompactEdge>>`
//! indexed by compact token index — a direct array lookup with no hashing.
//! Pools are likewise remapped to compact indices so the visited set is a
//! `Vec<bool>` (indexed by pool index) instead of a `HashSet`, eliminating
//! hashing on every edge explored. Each `CompactEdge` is 8 bytes (two `u32`)
//! versus the 24-byte `Edge`, improving cache density for the hot DFS loop.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
#[non_exhaustive]
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

/// A pool edge in the external (database) form. Retained for API
/// compatibility; the internal adjacency list uses the compact [`CompactEdge`]
/// (8 bytes, two `u32` indices) for cache density.
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

/// Hasher for `u64` token IDs: single multiply-xor round (FxHash-style).
/// Token-ID hashing runs ~1.5M times during graph construction; the default
/// `SipHash` costs several instructions per byte for 8-byte keys with no
/// security benefit here (keys are internal IDs, not adversarial input).
#[derive(Default)]
pub(crate) struct U64Hasher {
    hash: u64,
}

impl Hasher for U64Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.hash = (self.hash.rotate_left(5) ^ n).wrapping_mul(0x517C_C1B7_2722_0A95);
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.hash =
                (self.hash.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x517C_C1B7_2722_0A95);
        }
    }
}

/// Hasher-builder for token-ID maps.
pub(crate) type U64BuildHasher = BuildHasherDefault<U64Hasher>;

/// Compact internal edge: neighbor is a compact token index, `pool_idx`
/// identifies the pool in the graph's `pools` table. 8 bytes, cache-dense.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct CompactEdge {
    /// Compact index of the neighbor token (into `PathGraph::adj`).
    neighbor: u32,
    /// Compact index of the pool (into `PathGraph::pools`).
    pool_idx: u32,
}

/// A multigraph: token IDs are nodes, liquidity pools are edges.
///
/// Stored as a compact adjacency list (`Vec<Vec<CompactEdge>>`) indexed by
/// remapped contiguous token indices. External token IDs (`u64`) are mapped
/// to compact indices (`u32`) via `token_index`, so the hot DFS loop does
/// direct array indexing instead of hashing. Parallel edges (multiple pools
/// connecting the same token pair) are naturally supported and preserve
/// insertion order for deterministic traversal.
///
/// Pools are also remapped to compact indices; the `pools` table maps each
/// compact pool index back to its `(pool_id, PoolKind)` for yielding, and the
/// visited set is an O(1) `Vec<bool>` indexed by pool index.
#[derive(Clone)]
pub struct PathGraph {
    /// CSR adjacency: `adj_flat[adj_offsets[i]..adj_offsets[i + 1]]` is the
    /// list of outgoing `CompactEdge`s of compact token index `i`, in edge
    /// insertion order (identical order to the previous `Vec<Vec<_>>`),
    /// contiguous in memory for cache-dense DFS hops.
    adj_offsets: Vec<u32>,
    adj_flat: Vec<CompactEdge>,
    /// External token ID → compact token index.
    token_index: HashMap<u64, u32, U64BuildHasher>,
    /// Compact pool index → `(pool_id, PoolKind)` for yielding results.
    pools: Vec<(u64, PoolKind)>,
}

impl PathGraph {
    /// Slice of the outgoing edges of a compact node.
    #[must_use]
    pub(crate) fn adj_of(&self, node: u32) -> &[CompactEdge] {
        let start = self.adj_offsets[node as usize] as usize;
        let end = self.adj_offsets[node as usize + 1] as usize;
        &self.adj_flat[start..end]
    }

    /// Compact node count (=`adj_offsets.len() - 1`).
    fn nodes(&self) -> usize {
        self.adj_offsets.len() - 1
    }

    /// Build from a flat list of `(token0, token1, pool_id, pool_kind)` edges.
    ///
    /// Each edge is added in both directions (the graph is undirected, like
    /// the `networkx.MultiGraph` it replaces). Edge insertion order within
    /// each node's adjacency list is preserved for deterministic traversal
    /// (per-node cursor fill over the edge list = insertion order). External
    /// token IDs are remapped to compact contiguous indices.
    ///
    /// Two passes over the edge list into a flat CSR array: no per-node
    /// `Vec` allocations, no reallocation churn (~1.5M heap operations on a
    /// 742k-edge graph, down from ~750k edge pushes into growing per-node
    /// vectors plus map-insert adjacency growth).
    ///
    /// # Panics
    ///
    /// Panics if the number of distinct pools or tokens exceeds `u32::MAX`
    /// (compact index overflow). This is an architectural bound of the
    /// compact-index representation and unreachable in practice.
    #[must_use]
    pub fn from_edges(edges: Vec<(u64, u64, u64, PoolKind)>) -> Self {
        let n = edges.len();
        // Upper bound on distinct tokens: 2 per edge. Saves rehashing.
        let mut token_index: HashMap<u64, u32, U64BuildHasher> =
            HashMap::with_capacity_and_hasher(n * 2, U64BuildHasher::default());
        let mut pools: Vec<(u64, PoolKind)> = Vec::with_capacity(n);
        // Pass 1: intern endpoints; arena keeps (compactnode0, node1) per edge.
        let mut arena: Vec<(u32, u32)> = Vec::with_capacity(n);

        for (token0, token1, pool_id, pool_kind) in edges {
            pools.push((pool_id, pool_kind));

            let idx0 = Self::intern_token(&mut token_index, token0);
            let idx1 = Self::intern_token(&mut token_index, token1);

            arena.push((idx0, idx1));
        }

        // Pass 2: degrees → offsets → cursor fill (insertion order per node).
        let node_count = token_index.len();
        #[expect(clippy::expect_used)]
        let node_count_u32 = u32::try_from(node_count).expect("node count exceeds u32::MAX");
        let mut deg = vec![0u32; node_count];
        for (a, b) in &arena {
            deg[*a as usize] += 1;
            deg[*b as usize] += 1;
        }
        let mut adj_offsets: Vec<u32> = Vec::with_capacity(node_count + 1);
        let mut running: u32 = 0;
        adj_offsets.push(0);
        for &d in &deg {
            running += d;
            adj_offsets.push(running);
        }
        let total = running as usize;
        let mut adj_flat: Vec<CompactEdge> = vec![
            CompactEdge {
                neighbor: node_count_u32,
                pool_idx: u32::MAX,
            };
            total
        ];
        let mut cursor: Vec<u32> = adj_offsets[..node_count].to_vec();
        for (pool_idx_usize, (a, b)) in arena.iter().enumerate() {
            #[expect(clippy::expect_used)]
            let pool_idx = u32::try_from(pool_idx_usize).expect("pool index exceeds u32::MAX");
            let ca = &mut cursor[*a as usize];
            adj_flat[*ca as usize] = CompactEdge {
                neighbor: *b,
                pool_idx,
            };
            *ca += 1;
            let cb = &mut cursor[*b as usize];
            adj_flat[*cb as usize] = CompactEdge {
                neighbor: *a,
                pool_idx,
            };
            *cb += 1;
        }

        Self {
            adj_offsets,
            adj_flat,
            token_index,
            pools,
        }
    }

    /// Assign (or look up) the compact index for an external token ID.
    fn intern_token(token_index: &mut HashMap<u64, u32, U64BuildHasher>, token: u64) -> u32 {
        if let Some(&idx) = token_index.get(&token) {
            idx
        } else {
            #[expect(clippy::expect_used)] // u32 compact-index invariant (see `from_edges`)
            let idx = u32::try_from(token_index.len()).expect("token count exceeds u32::MAX");
            token_index.insert(token, idx);
            idx
        }
    }

    /// Map an external token ID to its compact index, if present.
    #[must_use]
    fn compact_index(&self, token: u64) -> Option<u32> {
        self.token_index.get(&token).copied()
    }

    /// Returns `true` if the node exists in the graph.
    #[must_use]
    pub fn contains_node(&self, node: u64) -> bool {
        self.token_index.contains_key(&node)
    }

    /// The number of nodes (tokens) in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes()
    }

    /// The number of pool edges incident to a token (its degree).
    #[must_use]
    pub fn degree(&self, token: u64) -> Option<usize> {
        self.compact_index(token).map(|i| self.adj_of(i).len())
    }

    /// Remove nodes with degree ≤ 1, repeating until no such nodes remain.
    ///
    /// Mirrors Python `_prepare_graph`'s iterative dead-end pruning: pruning a
    /// node may drop another node's live degree below 2, so peeling continues
    /// to a fixpoint. Nodes on cycles always retain degree ≥ 2, so the
    /// surviving subgraph (the 2-core) is identical regardless of peel order.
    ///
    /// Complexity: O(V + E) — a degree-array work queue visits each edge a
    /// constant number of times, then one ordered CSR rebuild pass
    /// (preserving edge insertion order, so DFS enumeration order is
    /// unchanged).
    ///
    /// # Panics
    ///
    /// Panics if the number of surviving edges exceeds `u32::MAX`
    /// (architectural bound of the CSR offset type; unreachable in practice).
    pub fn prune_dead_ends(&mut self) {
        let n = self.nodes();
        // 2-core peel (degree array + work queue): a node is on a pruning
        // path iff iteratively reducing it drops its live degree to <= 1.
        // This is the SAME fixpoint the previous round-based implementation
        // computed (nodes on cycles always retain degree >= 2; a removed
        // node's edges cannot re-connect anything), but it visits each edge
        // O(1) times instead of rescanning the whole graph per round
        // (O(V+E) total; the round-based version was O(rounds * (V+E)) and
        // took ~55s on a 742k-edge mainnet graph).
        let node_count_u32 = u32::try_from(self.nodes()).unwrap_or(u32::MAX);
        let mut degree: Vec<usize> = (0..node_count_u32).map(|i| self.adj_of(i).len()).collect();
        let mut removed = vec![false; n];

        // Only degree-1 nodes /* and isolated degree-0 leftovers */ can start
        let mut queue: Vec<usize> = Vec::with_capacity(n / 8);
        for (i, &d) in degree.iter().enumerate() {
            if d <= 1 {
                queue.push(i);
            }
        }
        let mut head = 0usize;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            if removed[i] {
                continue;
            }
            removed[i] = true;
            for e in self.adj_of(u32::try_from(i).unwrap_or(u32::MAX)) {
                let j = e.neighbor as usize;
                if removed[j] {
                    continue;
                }
                degree[j] -= 1;
                if degree[j] <= 1 {
                    queue.push(j);
                }
            }
        }
        // Single-pass rebuild of the surviving adjacencies (order kept):
        // walk the old CSR, writing kept edges into a fresh flat array.
        let mut new_flat: Vec<CompactEdge> = Vec::with_capacity(self.adj_flat.len());
        let mut new_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        new_offsets.push(0);
        for (i, &start) in self.adj_offsets.iter().enumerate().take(n) {
            if !removed[i] {
                let end = self.adj_offsets[i + 1] as usize;
                for e in &self.adj_flat[start as usize..end] {
                    if !removed[e.neighbor as usize] {
                        new_flat.push(*e);
                    }
                }
            }
            #[expect(clippy::expect_used)]
            new_offsets.push(u32::try_from(new_flat.len()).expect("edge count exceeds u32::MAX"));
        }
        self.adj_flat = new_flat;
        self.adj_offsets = new_offsets;
        // Drop the external token IDs of all pruned nodes so contains_node
        // reflects the post-prune state.
        self.token_index
            .retain(|_, &mut idx| !removed[idx as usize]);
    }

    /// Precompute valid depth positions per node, for lookahead pruning.
    ///
    /// For each node, determine which depth positions its edges satisfy. A
    /// node can appear at depth `d` if it has at least one incident edge whose
    /// `pool_kind` is in the allowed set at depth `d` (or `allowed[d]` is
    /// `None`, meaning all kinds are allowed).
    ///
    /// Returns a `Vec` indexed by compact token index, where entry `i` is a
    /// `Vec<bool>` whose index `d` is `true` if token `i` can participate at
    /// depth `d`.
    #[must_use]
    pub fn compute_node_valid_depths(
        &self,
        pool_type_per_depth: &[Option<Vec<PoolKind>>],
    ) -> Vec<Vec<bool>> {
        let mut result = Vec::with_capacity(self.nodes());
        for i in 0..self.nodes() {
            let edges = self.adj_of(u32::try_from(i).unwrap_or(u32::MAX));
            // Collect all pool kinds this node has edges for (a node can use
            // any pool it touches at any depth).
            let mut kinds = [false; 3];
            for e in edges {
                let kind = self.pools[e.pool_idx as usize].1;
                kinds[kind.as_u8() as usize] = true;
            }
            let mut valid = vec![false; pool_type_per_depth.len()];
            for (d, allowed) in pool_type_per_depth.iter().enumerate() {
                match allowed {
                    None => valid[d] = true,
                    Some(allowed_kinds) => {
                        valid[d] = allowed_kinds.iter().any(|k| kinds[k.as_u8() as usize]);
                    }
                }
            }
            result.push(valid);
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
    end: u32,
    min_depth: usize,
    effective_max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<&'a [Option<Vec<PoolKind>>]>,
    node_valid_depths: Option<&'a [Vec<bool>]>,
    filter_len: usize,
    stack: Vec<(u32, usize, bool)>,
    working_path: Vec<u32>,
    visited: Vec<bool>,
    pending_reverse: Option<Vec<EdgeKey>>,
    done: bool,
    /// Cooperative cancellation flag, shared with [`OwnedPathFinder`] via the
    /// same `with_cancel` contract: once set, the search exhausts at its next
    /// loop iteration instead of grinding to a natural end.
    cancel: Option<Arc<AtomicBool>>,
}

impl PathFinder<'_> {
    /// Attach a cooperative cancellation flag checked on every DFS advance —
    /// the same contract as [`OwnedPathFinder::with_cancel`]. A caller burning
    /// a per-frame time budget flips the flag between yields; the search
    /// reports exhaustion at its next loop iteration.
    #[must_use]
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Whether the attached cancellation flag has been set.
    #[must_use]
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Advance the DFS and return the next complete path, or `None` if
    /// the search is exhausted.
    ///
    /// If `include_reverse` is set, each found cycle yields the forward path
    /// first, then the reversed path on the next call.
    #[must_use]
    pub fn next_path(&mut self) -> Option<Vec<EdgeKey>> {
        if self.done || self.cancelled() {
            self.done = true;
            return None;
        }

        // If a reversed path is pending from the last yield, emit it now.
        if let Some(rev) = self.pending_reverse.take() {
            return Some(rev);
        }

        while let Some(frame) = self.stack.last_mut() {
            // Cooperative cancellation (borrowed walker): a set flag stops the
            // search at the next loop iteration. Field access mirrors
            // OwnedPathFinder::advance's in-loop check; the direct field read
            // keeps the `frame` borrow disjoint.
            if self
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                self.done = true;
                break;
            }
            let (node, edge_idx, yield_checked) = frame;

            // Check yield condition (once per frame arrival).
            if !*yield_checked {
                *yield_checked = true;
                if *node == self.end && self.working_path.len() >= self.min_depth {
                    let path = self.path_to_edge_keys();
                    if self.include_reverse {
                        let rev = self.reversed_path_to_edge_keys();
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
                        self.visited[popped as usize] = false;
                    }
                    continue;
                }
            }

            // Find the next valid edge to explore from this node.
            let neighbors: &[CompactEdge] = self.graph.adj_of(*node);

            // If the next hop reaches the maximum depth, only edges that close
            // the cycle (reach `end`) can possibly yield — skip the rest
            // without pushing a dead frame that would just backtrack. This is
            // the single biggest DFS cost saver: at the closing depth, every
            // non-`end` neighbor is pure waste.
            let final_hop = matches!(
                self.effective_max_depth,
                Some(emd) if self.working_path.len() + 1 == emd
            );

            let mut found_edge = false;
            while *edge_idx < neighbors.len() {
                let edge = &neighbors[*edge_idx];
                *edge_idx += 1;
                let pool_idx = edge.pool_idx;

                // Cycle detection: skip pools already on the working path.
                if self.visited[pool_idx as usize] {
                    continue;
                }

                // Final-hop restriction: the closing hop must reach `end`.
                if final_hop && edge.neighbor != self.end {
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
                        let kind = self.graph.pools[pool_idx as usize].1;
                        if !allowed_kinds.contains(&kind) {
                            continue;
                        }
                    }

                    // Lookahead pruning: skip if the neighbor can't continue
                    // at the next depth.
                    let next_depth = depth + 1;
                    if next_depth < self.filter_len {
                        if let Some(nvd) = self.node_valid_depths {
                            if let Some(valid) = nvd.get(edge.neighbor as usize) {
                                if !valid[next_depth] {
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Found a valid edge — extend the path and push the neighbor.
                self.working_path.push(pool_idx);
                self.visited[pool_idx as usize] = true;
                self.stack.push((edge.neighbor, 0, false));
                found_edge = true;
                break;
            }

            if !found_edge {
                // No more edges to explore from this node — backtrack.
                self.stack.pop();
                if let Some(popped) = self.working_path.pop() {
                    self.visited[popped as usize] = false;
                }
            }
        }

        // Search exhausted.
        self.done = true;
        None
    }

    /// Convert the current working path (pool indices) to `EdgeKey`s for yielding.
    fn path_to_edge_keys(&self) -> Vec<EdgeKey> {
        self.working_path
            .iter()
            .map(|&idx| self.graph.pools[idx as usize])
            .collect()
    }

    /// Convert the reversed working path to `EdgeKey`s.
    fn reversed_path_to_edge_keys(&self) -> Vec<EdgeKey> {
        self.working_path
            .iter()
            .rev()
            .map(|&idx| self.graph.pools[idx as usize])
            .collect()
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
    end: u32,
    min_depth: usize,
    effective_max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    node_valid_depths: Option<Vec<Vec<bool>>>,
    filter_len: usize,
    /// Per-node list of pool indices whose edge reaches `end` (the cycle's
    /// closing token). Built once per search in [`OwnedPathFinder::new`]: the
    /// closing hop can only traverse an edge to `end`, so iterating this
    /// compact list instead of the full adjacency avoids scanning (and
    /// skipping) every non-`end` neighbor of each penultimate node.
    end_edges: Vec<Vec<u32>>,
    stack: Vec<(u32, usize, bool)>,
    working_path: Vec<u32>,
    visited: Vec<bool>,
    /// Whether the reversed form of the most recently yielded cycle is still
    /// pending emission (used only when `include_reverse` is set). A flag
    /// rather than an owned `Vec<EdgeKey>` because the working path is still
    /// intact between yielding a cycle and emitting its reverse — the reversed
    /// path can be read directly from `working_path` on demand.
    pending_reverse: bool,
    done: bool,
    /// Cooperative cancellation flag (4IOEVT). When set, [`Self::advance`]
    /// stops the search at its next loop iteration and reports exhaustion.
    /// The async batch iterator's `Drop` impl sets it so a consumer that
    /// abandons a sweep (aclose / GC) releases a mid-grind DFS promptly
    /// instead of pinning a tokio worker until the search finishes.
    cancel: Option<Arc<AtomicBool>>,
    // --- discovery-phase heartbeat diagnostics ---
    // A silently-stalled DFS grinds here with the GIL released. On the async
    // path (4IOEVT) that grind runs on a tokio worker, so the Python event
    // loop keeps turning and a Python-side progress log cannot reflect the
    // DFS's internal progress. This heartbeat emits to stderr (GIL-free, zero
    // deps) so a future zero-yield hang is visible at a glance, not just
    // "78% CPU, no logs". Purely diagnostic — never alters `advance()`'s
    // return values or enumeration order.
    search_started: Instant,
    paths_yielded: u64,
    advances_since_yield: u64,
    last_heartbeat: Instant,
    /// Peak DFS stack depth observed — distinguishes "stuck shallow" (ordering
    /// gap) from "grinding deep" (graph-size variance) on a real run.
    max_stack_depth: usize,
}

/// Minimum elapsed wall-clock between discovery heartbeat emissions.
///
/// ~10s keeps a long search quiet but surfaces a hang within the ~5-min
/// bounded-time target. Tuned so small synthetic test fixtures
/// (which complete in µs) never emit.
const DISCOVERY_HEARTBEAT: Duration = Duration::from_secs(10);

/// Check the heartbeat clock every this many stack-frame iterations (amortizes
/// `Instant::now` out of the hot per-edge DFS loop). Power-of-two so the modulo
/// is a bitmask.
const HEARTBEAT_CHECK_EVERY: u64 = 4096;

// Discovery-heartbeat output interpretation on a live hang (NY4EFN root-cause
// mapping):
// - `paths_yielded` climbing slowly → graph-size variance (cause c);
//   discovery is progressing, just slow.
// - `paths_yielded` frozen at 0 with `advances_since_yield` climbing + low
//   `max_stack_depth` → DFS not yielding a first valid cycle (cause a: an
//   ordering/pruning gap, or no valid cycle exists for this filter).
// - `paths_yielded` climbing while the example's `[build_paths]` registered
//   count stays flat → the stall is per-path `build_pool` in the example
//   (cause b), NOT the DFS.

/// Outcome of one DFS advance: which path form (if any) is ready to yield.
#[derive(PartialEq, Eq)]
enum AdvanceOutcome {
    /// The search is exhausted; no more paths.
    Exhausted,
    /// The current `working_path` is a complete cycle ready to yield forward.
    Forward,
    /// A pending reverse of the previous cycle is ready to yield.
    Reversed,
}

impl OwnedPathFinder {
    /// Create from owned graph + search parameters.
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

        // Remap external start/end token IDs to compact indices. If EITHER
        // boundary token is absent from the (filtered) graph, no start->end path
        // exists - yield nothing. `end` must NOT fall back to a synthetic index:
        // remapping an absent end to compact index 0 made the DFS search for
        // cycles ending at an unrelated token (compact 0), yielding non-closing
        // paths that tripped the direction-resolution fail-stop.
        let start_idx = graph.compact_index(start);
        let end_idx = graph.compact_index(end);

        let (stack, done) = match (start_idx, end_idx) {
            (Some(s), Some(_e)) => (vec![(s, 0, false)], false),
            _ => (Vec::new(), true),
        };
        let end_idx = end_idx.unwrap_or(0);
        let n_pools = graph.pools.len();

        // Precompute, per node, the pool indices of edges that reach `end`.
        // The closing hop only traverses `end`-reaching edges, so iterating
        // this compact list avoids scanning/skipping every non-`end` neighbor
        // of each penultimate node. Insertion order is preserved so traversal
        // order (hence enumeration order) is unchanged.
        let mut end_edges: Vec<Vec<u32>> = vec![Vec::new(); graph.nodes()];
        for (node_idx, e_list) in graph.adj_offsets.windows(2).enumerate() {
            let seg = &graph.adj_flat[e_list[0] as usize..e_list[1] as usize];
            for e in seg {
                if e.neighbor == end_idx {
                    end_edges[node_idx].push(e.pool_idx);
                }
            }
        }

        let now = Instant::now();
        Self {
            graph,
            end: end_idx,
            min_depth,
            effective_max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
            filter_len,
            end_edges,
            stack,
            working_path: Vec::with_capacity(16),
            visited: vec![false; n_pools],
            pending_reverse: false,
            done,
            cancel: None,
            search_started: now,
            paths_yielded: 0,
            advances_since_yield: 0,
            last_heartbeat: now,
            max_stack_depth: 0,
        }
    }

    /// Attach a cooperative cancellation flag checked on every DFS advance.
    ///
    /// The flag is owned by the caller (the async batch iterator's `Drop`
    /// sets it), so a consumer that drops the iterator mid-search stops the
    /// DFS at its next loop iteration instead of running to completion.
    #[must_use]
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Whether the attached cancellation flag has been set.
    #[must_use]
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Advance the DFS by one yield without materializing the path.
    ///
    /// Returns [`AdvanceOutcome::Forward`] when a complete cycle is ready
    /// (read it from `working_path`), [`AdvanceOutcome::Reversed`] when a
    /// pending reverse of the previous cycle is ready (read reversed
    /// `working_path`), or [`AdvanceOutcome::Exhausted`] when the search is
    /// done. This is the shared DFS core — both [`Self::next_path`] (which
    /// materializes `EdgeKey`s) and [`Self::next_path_indices_into`] (which
    /// appends pool indices, avoiding allocation) dispatch through it.
    #[expect(clippy::too_many_lines)]
    fn advance(&mut self) -> AdvanceOutcome {
        if self.done || self.cancelled() {
            self.done = true;
            return AdvanceOutcome::Exhausted;
        }

        // Emit a pending reversed cycle before doing any further DFS work.
        if self.pending_reverse {
            self.pending_reverse = false;
            self.paths_yielded += 1;
            self.advances_since_yield = 0;
            return AdvanceOutcome::Reversed;
        }

        let filter_slice = self.pool_type_per_depth.as_deref();
        let nvd_ref = self.node_valid_depths.as_deref();

        loop {
            if self.cancelled() {
                self.done = true;
                break;
            }
            let stack_len = self.stack.len();
            if stack_len == 0 {
                break;
            }
            // Discovery heartbeat: amortized (checked every `HEARTBEAT_CHECK_EVERY`
            // stack-frame iterations, not per edge) — `Instant::now` is ~10ns
            // but the hot DFS loop runs millions of iterations, so the modulo
            // keeps it out of the inner per-edge path. Fires a GIL-free stderr
            // line every `DISCOVERY_HEARTBEAT` while grinding, so a zero-yield
            // hang surfaces immediately. Touches only disjoint
            // heartbeat fields + the `stack_len` copy, so it cannot borrow
            // `self.stack` while the mutable `frame` below is live.
            self.advances_since_yield = self.advances_since_yield.wrapping_add(1);
            if stack_len > self.max_stack_depth {
                self.max_stack_depth = stack_len;
            }
            if self
                .advances_since_yield
                .is_multiple_of(HEARTBEAT_CHECK_EVERY)
            {
                // Inlined (not a `&mut self` method) so the heartbeat touches
                // only disjoint fields — `pool_type_per_depth` is borrowed
                // immutably for the whole loop body via `filter_slice`.
                let now = Instant::now();
                if now.duration_since(self.last_heartbeat) >= DISCOVERY_HEARTBEAT {
                    self.last_heartbeat = now;
                    let elapsed = now.duration_since(self.search_started);
                    // Low-frequency stderr diagnostic on a zero-dependency leaf; no
                    // logging crate is available and this runs off the hot path.
                    #[expect(clippy::print_stderr)]
                    {
                        eprintln!(
                            "discovery heartbeat: elapsed={elapsed:?} \
                             paths_yielded={} advances_since_yield={} max_stack_depth={}",
                            self.paths_yielded, self.advances_since_yield, self.max_stack_depth
                        );
                    }
                }
            }
            let frame = &mut self.stack[stack_len - 1];
            let (node, edge_idx, yield_checked) = frame;

            // Check yield condition (once per frame arrival).
            if !*yield_checked {
                *yield_checked = true;
                if *node == self.end && self.working_path.len() >= self.min_depth {
                    if self.include_reverse {
                        // working_path stays intact until the reverse is
                        // emitted on the next advance(), so we only need a
                        // flag — no owned Vec to carry over.
                        self.pending_reverse = true;
                    }
                    self.paths_yielded += 1;
                    self.advances_since_yield = 0;
                    return AdvanceOutcome::Forward;
                }
            }

            // Stop recursion if the working path has reached the maximum depth.
            if let Some(emd) = self.effective_max_depth {
                if self.working_path.len() >= emd {
                    // Backtrack.
                    self.stack.pop();
                    if let Some(popped) = self.working_path.pop() {
                        self.visited[popped as usize] = false;
                    }
                    continue;
                }
            }

            // If the next hop reaches the maximum depth, only edges that close
            // the cycle (reach `end`) can possibly yield — skip the rest
            // without pushing a dead frame that would just backtrack. This is
            // the single biggest DFS cost saver: at the closing depth, every
            // non-`end` neighbor is pure waste.
            let final_hop = matches!(
                self.effective_max_depth,
                Some(emd) if self.working_path.len() + 1 == emd
            );
            // Penultimate hop (next iteration after this push is the closing
            // one). Loop-invariant within the inner edge-scan loop below —
            // working_path.len() only changes on push-after-break — so hoist
            // the depth comparison out of the per-edge loop.
            let penultimate_hop = matches!(
                self.effective_max_depth,
                Some(emd) if self.working_path.len() + 2 == emd
            );

            let mut found_edge = false;

            if final_hop {
                // Closing hop: only `end`-reaching edges can complete the
                // cycle. Iterate the precomputed compact end-edge list
                // (instead of scanning + skipping the full adjacency) — this
                // avoids touching every non-`end` neighbor of each penultimate
                // node. All edges here reach `end`, so the pushed neighbor is
                // always `end` and the final-hop skip check is unnecessary.
                let end_list: &[u32] = self
                    .end_edges
                    .get(*node as usize)
                    .map_or([].as_slice(), Vec::as_slice);
                while *edge_idx < end_list.len() {
                    let pool_idx = end_list[*edge_idx];
                    *edge_idx += 1;

                    // Cycle detection: skip pools already on the working path.
                    if self.visited[pool_idx as usize] {
                        continue;
                    }

                    // Per-depth pool-type filter (lookahead never applies at
                    // the closing hop: next_depth == effective_max_depth is
                    // never < filter_len).
                    if let Some(filter) = filter_slice {
                        let depth = self.working_path.len();
                        if depth < self.filter_len {
                            if let Some(allowed_kinds) = &filter[depth] {
                                let kind = self.graph.pools[pool_idx as usize].1;
                                if !allowed_kinds.contains(&kind) {
                                    continue;
                                }
                            }
                        }
                    }

                    self.working_path.push(pool_idx);
                    self.visited[pool_idx as usize] = true;
                    self.stack.push((self.end, 0, false));
                    found_edge = true;
                    break;
                }
            } else {
                // Find the next valid edge to explore from this node.
                let neighbors: &[CompactEdge] = self.graph.adj_of(*node);

                while *edge_idx < neighbors.len() {
                    let edge = &neighbors[*edge_idx];
                    *edge_idx += 1;
                    let pool_idx = edge.pool_idx;

                    // Cycle detection: skip pools already on the working path.
                    if self.visited[pool_idx as usize] {
                        continue;
                    }

                    // Penultimate-hop reachability prune (analog of the
                    // final-hop restriction, one level up): the hop being
                    // chosen now leads to a node that must make the closing
                    // hop on the next iteration. If that neighbor has no
                    // edge to `end`, no cycle through it can close, so skip
                    // it without pushing a dead frame. Sound (never skips a
                    // valid cycle); conservative (a node whose only end-edges
                    // are visited still passes this check and is pruned only
                    // when the closing loop finds nothing).
                    if penultimate_hop {
                        let neighbor_can_close = self
                            .end_edges
                            .get(edge.neighbor as usize)
                            .is_some_and(|l| !l.is_empty());
                        if !neighbor_can_close {
                            continue;
                        }
                    }

                    // Per-depth pool-type filter.
                    if let Some(filter) = filter_slice {
                        let depth = self.working_path.len();
                        if depth >= self.filter_len {
                            continue;
                        }
                        if let Some(allowed_kinds) = &filter[depth] {
                            let kind = self.graph.pools[pool_idx as usize].1;
                            if !allowed_kinds.contains(&kind) {
                                continue;
                            }
                        }

                        // Lookahead pruning.
                        let next_depth = depth + 1;
                        if next_depth < self.filter_len {
                            if let Some(nvd) = nvd_ref {
                                if let Some(valid) = nvd.get(edge.neighbor as usize) {
                                    if !valid[next_depth] {
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    // Found a valid edge — extend the path and push the neighbor.
                    self.working_path.push(pool_idx);
                    self.visited[pool_idx as usize] = true;
                    self.stack.push((edge.neighbor, 0, false));
                    found_edge = true;
                    break;
                }
            }

            if !found_edge {
                // No more edges to explore from this node — backtrack.
                self.stack.pop();
                if let Some(popped) = self.working_path.pop() {
                    self.visited[popped as usize] = false;
                }
            }
        }

        // Search exhausted — emit a final heartbeat so the operator sees
        // the total when discovery completes (even if it ran fast).
        self.emit_discovery_complete();
        self.done = true;
        AdvanceOutcome::Exhausted
    }

    /// Emit a final discovery-complete line so the operator sees the total at
    /// search end (cheap; covers the common fast-search case that never tripped
    /// the throttled heartbeat).
    fn emit_discovery_complete(&self) {
        let elapsed = self.search_started.elapsed();
        // Low-frequency stderr diagnostic on a zero-dependency leaf (no logging
        // crate available); one line at discovery completion.
        #[expect(clippy::print_stderr)]
        {
            eprintln!(
                "discovery complete: elapsed={elapsed:?} paths_yielded={} max_stack_depth={}",
                self.paths_yielded, self.max_stack_depth
            );
        }
    }

    /// Advance the DFS and return the next complete path, or `None` if
    /// the search is exhausted.
    ///
    /// If `include_reverse` is set, each found cycle yields the forward path
    /// first, then the reversed path on the next call.
    #[must_use]
    pub fn next_path(&mut self) -> Option<Vec<EdgeKey>> {
        match self.advance() {
            AdvanceOutcome::Exhausted => None,
            AdvanceOutcome::Forward => Some(self.path_to_edge_keys()),
            AdvanceOutcome::Reversed => Some(self.reversed_path_to_edge_keys()),
        }
    }

    /// Advance the DFS and append the next path's **pool indices** into `out`,
    /// returning the number of indices appended (the path length), or `None`
    /// if the search is exhausted.
    ///
    /// This is the allocation-free hot path used by the `PyO3` iterator: instead
    /// of materializing a `Vec<EdgeKey>` per yielded path (96k small
    /// allocations for a typical search), it appends the compact `u32` pool
    /// indices into a caller-owned flat buffer. The FFI layer converts
    /// indices → `(pool_id, kind_u8)` lazily while building Python objects.
    #[must_use]
    pub fn next_path_indices_into(&mut self, out: &mut Vec<u32>) -> Option<usize> {
        match self.advance() {
            AdvanceOutcome::Exhausted => None,
            AdvanceOutcome::Forward => {
                let len = self.working_path.len();
                out.extend(self.working_path.iter().copied());
                Some(len)
            }
            AdvanceOutcome::Reversed => {
                let len = self.working_path.len();
                out.extend(self.working_path.iter().rev().copied());
                Some(len)
            }
        }
    }

    /// Resolve a compact pool index to its `EdgeKey` `(pool_id, PoolKind)`.
    ///
    /// Lets the `PyO3` layer convert buffered pool indices to Python tuples
    /// without exposing the graph's internal `pools` field.
    #[must_use]
    pub fn pool_edge_key(&self, pool_idx: u32) -> EdgeKey {
        self.graph.pools[pool_idx as usize]
    }

    /// Convert the current working path (pool indices) to `EdgeKey`s for yielding.
    fn path_to_edge_keys(&self) -> Vec<EdgeKey> {
        self.working_path
            .iter()
            .map(|&idx| self.graph.pools[idx as usize])
            .collect()
    }

    /// Convert the reversed working path to `EdgeKey`s.
    fn reversed_path_to_edge_keys(&self) -> Vec<EdgeKey> {
        self.working_path
            .iter()
            .rev()
            .map(|&idx| self.graph.pools[idx as usize])
            .collect()
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
    #[expect(clippy::too_many_arguments)]
    #[must_use]
    pub fn find_paths_iter<'a>(
        &'a self,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<&'a [Option<Vec<PoolKind>>]>,
        node_valid_depths: Option<&'a [Vec<bool>]>,
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

        let start_idx = self.compact_index(start);
        let end_idx = self.compact_index(end);

        // Match OwnedPathFinder::new: a boundary token absent from the graph
        // must yield NO paths (no `end` fallback to a synthetic index 0).
        let (stack, done) = match (start_idx, end_idx) {
            (Some(s), Some(_e)) => (vec![(s, 0, false)], false),
            _ => (Vec::new(), true),
        };
        let end_idx = end_idx.unwrap_or(0);

        PathFinder {
            graph: self,
            end: end_idx,
            min_depth,
            effective_max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
            filter_len,
            stack,
            working_path: Vec::with_capacity(16),
            visited: vec![false; self.pools.len()],
            pending_reverse: None,
            done,
            cancel: None,
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
    #[expect(clippy::too_many_arguments)]
    #[must_use]
    pub fn find_paths(
        &self,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<&[Option<Vec<PoolKind>>]>,
        node_valid_depths: Option<&[Vec<bool>]>,
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
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

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
        assert_eq!(graph.degree(WETH), Some(3));
        // A has 3 edges: pool1->WETH, pool2->WETH, pool3->B
        assert_eq!(graph.degree(A), Some(3));
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
    fn test_absent_end_token_yields_no_paths() {
        // A boundary token that is NOT a node in the (filtered) graph - e.g. a
        // NATIVE/0x0 token with no connecting pool among the candidate edges -
        // must yield NO paths. Regression: the old
        // `compact_index(end).unwrap_or(0)` silently remapped an absent end to
        // compact index 0, so a "WETH -> <absent-token>" search actually ran as
        // a "<index-0> -> ..." search and emitted non-closing cycles that
        // tripped the direction-resolution fail-stop on the live bot.
        let graph = build_fixture_graph();
        // start present, end absent -> no paths.
        let paths = graph.find_paths(WETH, 9999, 3, Some(3), true, None, None);
        assert!(paths.is_empty(), "absent end token must yield no paths");
        // start absent -> no paths.
        let paths_start = graph.find_paths(9999, WETH, 3, Some(3), true, None, None);
        assert!(
            paths_start.is_empty(),
            "absent start token must yield no paths"
        );
        // both absent -> no paths.
        let paths_both = graph.find_paths(9999, 9998, 3, Some(3), true, None, None);
        assert!(
            paths_both.is_empty(),
            "absent start+end must yield no paths"
        );
        // Sanity: a present end still yields its cycles.
        let ok = graph.find_paths(WETH, WETH, 3, Some(3), false, None, None);
        assert!(!ok.is_empty(), "present end (WETH) must still yield cycles");
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

    #[test]
    fn test_cancel_flag_exhausts_search() {
        let graph = build_fixture_graph();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut finder = OwnedPathFinder::new(graph, WETH, WETH, 2, Some(3), true, None)
            .with_cancel(Arc::clone(&cancel));
        assert!(finder.next_path().is_some(), "fixture must yield a path");
        cancel.store(true, Ordering::Release);
        assert!(
            finder.next_path().is_none(),
            "a set cancel flag must exhaust the search immediately"
        );
        assert!(finder.next_path().is_none());
    }

    /// The borrowed walker (`find_paths_iter`) must support the same
    /// cooperative cancellation the owned one does: an armed flag before the
    /// first advance exhausts immediately, and a flag set mid-stream stops
    /// the search at its next loop iteration.
    #[test]
    fn test_borrowed_finder_cancel_before_first_advance() {
        let graph = build_fixture_graph();
        let cancel = Arc::new(AtomicBool::new(true));
        let mut finder = graph
            .find_paths_iter(WETH, WETH, 2, Some(3), true, None, None)
            .with_cancel(cancel);
        assert!(
            finder.next_path().is_none(),
            "an armed cancel flag must yield nothing"
        );
        assert!(finder.next_path().is_none(), "exhaustion is sticky");
    }

    #[test]
    fn test_borrowed_finder_cancel_mid_stream() {
        let graph = build_fixture_graph();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut finder = graph
            .find_paths_iter(WETH, WETH, 2, Some(3), true, None, None)
            .with_cancel(Arc::clone(&cancel));
        assert!(
            finder.next_path().is_some(),
            "the first cycle yields before any cancel"
        );
        cancel.store(true, Ordering::Release);
        assert!(
            finder.next_path().is_none(),
            "a flag set between advances stops the search promptly"
        );
    }

    /// The discovery-heartbeat diagnostics + the `while let` → `loop`
    /// refactor of `OwnedPathFinder::advance` must not alter enumeration order
    /// or yield count. Two independent searches on the same graph must produce
    /// identical, stable output — the heartbeat is purely diagnostic stderr.
    #[test]
    fn test_heartbeat_diagnostics_do_not_alter_enumeration() {
        let graph = build_fixture_graph();
        let run_one: Vec<Vec<u64>> = graph
            .find_paths(WETH, WETH, 2, Some(3), true, None, None)
            .into_iter()
            .map(|p| edges_to_pool_ids(&p))
            .collect();
        // Re-run on a fresh graph instance — determinism + no heartbeat side
        // effects across runs.
        let graph2 = build_fixture_graph();
        let run_two: Vec<Vec<u64>> = graph2
            .find_paths(WETH, WETH, 2, Some(3), true, None, None)
            .into_iter()
            .map(|p| edges_to_pool_ids(&p))
            .collect();
        assert!(!run_one.is_empty(), "fixture must yield paths");
        assert_eq!(
            run_one, run_two,
            "enumeration must be stable + unaffected by heartbeat wiring"
        );
    }
}
