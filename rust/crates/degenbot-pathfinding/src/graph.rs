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

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
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

    /// The single table of persisted `kind` discriminators the graph
    /// vocabulary admits, paired with their tag.
    ///
    /// ADR-059 D1: this is the one enumeration [`Self::from_kind_str`] (and
    /// therefore every schema `is_v*_kind` helper and graph reader) projects
    /// through; `degenbot-db`'s golden test pins its membership so a taxonomy
    /// species cannot be added without a graph tag.
    pub const KNOWN_KINDS: &'static [(&'static str, Self)] = &[
        ("uniswap_v2", Self::V2),
        ("sushiswap_v2", Self::V2),
        ("pancakeswap_v2", Self::V2),
        ("aerodrome_v2", Self::V2),
        ("camelot_v2", Self::V2),
        ("swapbased_v2", Self::V2),
        ("uniswap_v3", Self::V3),
        ("sushiswap_v3", Self::V3),
        ("pancakeswap_v3", Self::V3),
        ("aerodrome_v3", Self::V3),
        ("uniswap_v4", Self::V4),
    ];

    /// Project a persisted pool `kind` discriminator onto the graph vocabulary
    /// (ADR-059 D1).
    ///
    /// The discovery tier receives a pool's taxonomy species as the database
    /// `pools.kind` / `managed_pools.kind` string, not a `degenbot-pools`
    /// `Identity`: this crate is the graph leaf and carries no taxonomy
    /// dependency. The `kind` string is therefore the minimal taxonomy input
    /// the tier can construct from its own data.
    ///
    /// Returns `None` for a kind outside the V2/V3/V4 graph vocabulary; the
    /// caller refuses it loudly rather than dropping the row.
    #[must_use]
    pub fn from_kind_str(kind: &str) -> Option<Self> {
        Self::KNOWN_KINDS
            .iter()
            .find(|(name, _)| *name == kind)
            .map(|(_, pool_kind)| *pool_kind)
    }

    /// `pools.kind` / `managed_pools.kind` discriminators whose family the
    /// taxonomy declares but this graph vocabulary does not admit (ADR-059
    /// D8). These are NOT supported: [`Self::from_kind_str`] returns `None`
    /// for them, and a DB row carrying one flows into the loud
    /// `load_unsupported` roster instead of being unclassifiable.
    ///
    /// `lfj_binned` is the LFJ (Trader Joe) binned-liquidity family — the
    /// first genuinely new pool structure through the kernel, declared here
    /// before any tier admits it.
    pub const DECLARED_UNSUPPORTED_KINDS: &'static [&'static str] = &["lfj_binned"];

    /// `true` if `kind` is a declared-but-unsupported family discriminator
    /// (present in the taxonomy, absent from the supported [`Self::KNOWN_KINDS`]).
    #[must_use]
    pub fn is_declared_unsupported(kind: &str) -> bool {
        Self::DECLARED_UNSUPPORTED_KINDS.contains(&kind)
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
    /// Bundle layer, computed once at construction: parallel pools between
    /// the same unordered token pair collapse into one bundle. Bundle ids
    /// follow first-appearance order in the edge list (deterministic; ties
    /// in per-token sorts resolve back to edge order). Pools of bundle `b`
    /// are `bundle_pools_flat[bundle_offsets[b] .. bundle_offsets[b + 1]]`
    /// in ascending pool index — CSR, so no per-bundle allocations.
    bundle_offsets: Vec<u32>,
    bundle_pools_flat: Vec<u32>,
    /// Per bundle: the unordered compact token pair it spans.
    bundle_pairs: Vec<(u32, u32)>,
    /// Per bundle: bitmask of `PoolKind`s present among its pools.
    bundle_kinds: Vec<u32>,
}

/// Admissible prune data for ONE bounded search (start, end, budget).
///
/// `to_end[x]` is the fewest hops from `x` to the target — a lower bound on
/// the trail hops still needed once `x` is reached, so a step onto `x` is
/// cut only when no within-budget trail can close through it.
///
/// A start-side/ball mask (`d_start + d_end > budget` exclusion) would be
/// redundant here: the DFS only ever reaches `start`-reachable nodes, and a
/// node arrived at length `len` passes the cutoff only when
/// `d_end[x] <= emd - len - 1 <= emd - d_start[x] - 1` — exactly the ball
/// condition, enforced en route at zero extra cost.
struct SearchPrune {
    to_end: Vec<u32>,
}

impl SearchPrune {
    fn build(graph: &PathGraph, end: u32) -> Self {
        Self {
            to_end: graph.hop_distances(&[end]),
        }
    }
}

impl PathGraph {
    /// Slice of the outgoing edges of a compact node.
    #[must_use]
    pub(crate) fn adj_of(&self, node: u32) -> &[CompactEdge] {
        let start = self.adj_offsets[node as usize] as usize;
        let end = self.adj_offsets[node as usize + 1] as usize;
        &self.adj_flat[start..end]
    }

    /// Multi-source BFS hop distances to the target node set (undirected).
    /// `u32::MAX` marks unreachable. On the token multigraph this is a LOWER
    /// bound on the trail-hop count between any two nodes, so it is an
    /// admissible quantity for search cutoffs: skipping a step can only
    /// discard trails that provably exceed the budget.
    #[must_use]
    pub(crate) fn hop_distances(&self, sources: &[u32]) -> Vec<u32> {
        let n = self.nodes();
        let mut dist: Vec<u32> = vec![u32::MAX; n];
        let mut queue: VecDeque<u32> = VecDeque::new();
        for &s in sources {
            if (s as usize) < n {
                dist[s as usize] = 0;
                queue.push_back(s);
            }
        }
        while let Some(x) = queue.pop_front() {
            let next = dist[x as usize].saturating_add(1);
            for edge in self.adj_of(x) {
                if dist[edge.neighbor as usize] > next {
                    dist[edge.neighbor as usize] = next;
                    queue.push_back(edge.neighbor);
                }
            }
        }
        dist
    }

    /// Compact node count (=`adj_offsets.len() - 1`).
    fn nodes(&self) -> usize {
        self.adj_offsets.len() - 1
    }

    /// Member pools of bundle `b` (ascending pool index).
    #[must_use]
    pub(crate) fn bundle_pools(&self, b: u32) -> &[u32] {
        let start = self.bundle_offsets[b as usize] as usize;
        let end = self.bundle_offsets[b as usize + 1] as usize;
        &self.bundle_pools_flat[start..end]
    }

    /// Kind bitmask of bundle `b`.
    #[must_use]
    pub(crate) fn bundle_kind_mask(&self, b: u32) -> u32 {
        self.bundle_kinds[b as usize]
    }

    /// Per-token bundle incidence as CSR: `(other token, bundle)` pairs, in
    /// bundle first-appearance order. The search clones the flat array
    /// (memcpy) and stable-sorts each token's slice by prune distance — the
    /// only search-dependent ordering.
    #[must_use]
    pub(crate) fn token_bundles_csr(&self) -> (Vec<u32>, Vec<(u32, u32)>) {
        let n = self.nodes();
        let mut offsets = vec![0u32; n + 1];
        for &(a, t) in &self.bundle_pairs {
            offsets[a as usize + 1] += 1;
            if t != a {
                offsets[t as usize + 1] += 1;
            }
        }
        for w in 1..=n {
            offsets[w] += offsets[w - 1];
        }
        let mut flat = vec![(0u32, 0u32); offsets[n] as usize];
        let mut cursor: Vec<u32> = offsets[..n].to_vec();
        for (b, &(a, t)) in self.bundle_pairs.iter().enumerate() {
            let bundle = u32::try_from(b).unwrap_or(u32::MAX);
            flat[cursor[a as usize] as usize] = (t, bundle);
            cursor[a as usize] += 1;
            if t != a {
                flat[cursor[t as usize] as usize] = (a, bundle);
                cursor[t as usize] += 1;
            }
        }
        (offsets, flat)
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
        // Pass 1: intern endpoints; arena keeps (compactnode0, node1) per
        // edge, and parallel pools intern into one bundle in first-
        // appearance order (packed-pair keys keep lookups off SipHash).
        let mut arena: Vec<(u32, u32)> = Vec::with_capacity(n);
        let mut bundle_id: HashMap<u64, u32, U64BuildHasher> =
            HashMap::with_capacity_and_hasher(n, U64BuildHasher::default());
        let mut bundle_pairs: Vec<(u32, u32)> = Vec::with_capacity(n);
        let mut bundle_kinds: Vec<u32> = Vec::with_capacity(n);
        let mut pool_bundle: Vec<u32> = Vec::with_capacity(n);

        for (token0, token1, pool_id, pool_kind) in edges {
            pools.push((pool_id, pool_kind));

            let idx0 = Self::intern_token(&mut token_index, token0);
            let idx1 = Self::intern_token(&mut token_index, token1);

            let packed = pack_token_pair(idx0, idx1);
            let kind_bit = 1u32 << pool_kind.as_u8();
            let bundle = if let Some(&b) = bundle_id.get(&packed) {
                bundle_kinds[b as usize] |= kind_bit;
                b
            } else {
                let b = u32::try_from(bundle_pairs.len()).unwrap_or(u32::MAX);
                bundle_id.insert(packed, b);
                bundle_pairs.push((idx0.min(idx1), idx0.max(idx1)));
                bundle_kinds.push(kind_bit);
                b
            };
            pool_bundle.push(bundle);

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

        // Bundle pools as CSR (count + cursor fill): pools land grouped by
        // bundle, ascending in pool index within a bundle, with zero
        // per-bundle allocations.
        let bundle_count = bundle_pairs.len();
        let mut bundle_offsets: Vec<u32> = vec![0; bundle_count + 1];
        for &b in &pool_bundle {
            bundle_offsets[b as usize + 1] += 1;
        }
        for w in 1..=bundle_count {
            bundle_offsets[w] += bundle_offsets[w - 1];
        }
        let mut bundle_pools_flat: Vec<u32> = vec![0; pool_bundle.len()];
        let mut bcursor: Vec<u32> = bundle_offsets[..bundle_count].to_vec();
        for (pool_idx_usize, &b) in pool_bundle.iter().enumerate() {
            #[expect(clippy::expect_used)]
            let pool_idx = u32::try_from(pool_idx_usize).expect("pool index exceeds u32::MAX");
            bundle_pools_flat[bcursor[b as usize] as usize] = pool_idx;
            bcursor[b as usize] += 1;
        }

        Self {
            adj_offsets,
            adj_flat,
            token_index,
            pools,
            bundle_offsets,
            bundle_pools_flat,
            bundle_pairs,
            bundle_kinds,
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

/// A long-running walk's live progress, delivered to a caller-installed
/// observation hook ([`BundledSearch::with_progress`]) on the heartbeat
/// checkpoint. A pure snapshot: reading it never perturbs the enumeration.
#[derive(Clone, Copy, Debug)]
pub struct WalkerTally {
    /// Wall clock since the search's first advance.
    pub elapsed: Duration,
    /// Completed paths yielded so far.
    pub paths_yielded: u64,
    /// Stack-frame advances since the most recent yield: a large value with a
    /// small `paths_yielded` is the no-yield straggler shape.
    pub advances_since_yield: u64,
    /// Peak DFS stack depth observed — distinguishes "stuck shallow" from
    /// "grinding deep" on a real run.
    pub max_stack_depth: usize,
}

/// One pending yield's expansion state: the walk's per-step candidate pool
/// lists (kind-filtered), an odometer over those lists, and same-bundle
/// injectivity validation. Each valid odometer state is one concrete pool
/// assignment of the walk — the bundle-level equivalent of the per-pool
/// DFS branching the old engine performed eagerly.
struct WalkExpansion {
    /// Step `i` of the walk used `slot_bundle[i]`'s bundle: assignments must
    /// choose distinct pools across all slots of one bundle (the trail's
    /// pools are distinct; different bundles share no pool by construction).
    slot_bundle: Vec<u32>,
    /// Per step: candidate pool indices, limited to pools whose kind passes
    /// the depth's filter (full bundle list when unfiltered).
    slots: Vec<Vec<u32>>,
    /// Odometer position per slot; `chosen[i] = slots[i][cursor[i]]`.
    cursor: Vec<usize>,
    chosen: Vec<u32>,
    started: bool,
    finished: bool,
}

impl WalkExpansion {
    /// Advance to the next concrete assignment (mixed-radix over the slot
    /// candidate lists, skimming invalid same-bundle duplicates). On success
    /// the step-ordered pool list is written into `out` (reusing its
    /// capacity — the hot path allocates nothing per assignment).
    fn next_assignment_into(&mut self, out: &mut Vec<u32>) -> bool {
        let n = self.slots.len();
        if n == 0 {
            // An empty walk (min_depth == 0) has exactly one assignment.
            if self.started {
                return false;
            }
            self.started = true;
            out.clear();
            return true;
        }
        loop {
            if self.finished {
                return false;
            }
            if self.started {
                // Increment the odometer from the last slot, carrying left;
                // on a carry, higher slots reset to their first candidate.
                let mut j = n;
                loop {
                    if j == 0 {
                        self.finished = true;
                        return false;
                    }
                    j -= 1;
                    self.cursor[j] += 1;
                    if self.cursor[j] < self.slots[j].len() {
                        self.chosen[j] = self.slots[j][self.cursor[j]];
                        for k in j + 1..n {
                            self.cursor[k] = 0;
                            self.chosen[k] = self.slots[k][0];
                        }
                        break;
                    }
                }
            } else {
                self.started = true;
                for i in 0..n {
                    self.cursor[i] = 0;
                    self.chosen[i] = self.slots[i][0];
                }
            }
            if self.same_bundle_distinct() {
                out.clear();
                out.extend_from_slice(&self.chosen);
                return true;
            }
            // Invalid (two slots of one bundle chose the same pool): keep
            // advancing. The walk-level kind check only proved a necessary
            // condition; this is where exact feasibility is decided.
        }
    }

    fn same_bundle_distinct(&self) -> bool {
        for i in 0..self.slots.len() {
            for j in i + 1..self.slots.len() {
                if self.slot_bundle[i] == self.slot_bundle[j] && self.chosen[i] == self.chosen[j] {
                    return false;
                }
            }
        }
        true
    }
}

/// Outcome of one DFS advance: which path form (if any) is ready to yield.
#[derive(PartialEq, Eq)]
enum AdvanceOutcome {
    /// The search is exhausted; no more paths.
    Exhausted,
    /// A complete walk expansion is ready (pools in `emitted`).
    Forward,
    /// The reversed form of the previous yield is ready.
    Reversed,
}

/// Depth-first trail search over the **bundle graph**: all pools between the
/// same unordered token pair form one bundle with multiplicity `m`, and the
/// DFS walks token-pair BUNDLES (a per-bundle use counter instead of a
/// per-pool visited mask), expanding each walk to its concrete pool
/// assignments lazily at yield time.
///
/// Why: a hub token with `m` parallel pools to the same neighbor used to
/// branch the entire remaining DFS subtree `m` times — the subtrees are
/// token-wise identical, differing only in which pool each step consumed.
/// The bundled walk explores the token shape once and enumerates the
/// `m·(m-1)·…·(m-u+1)` ordered distinct-pool assignments (`u` = visits of
/// that bundle) during expansion: exactly the multiset the per-pool DFS
/// produced, in deterministic lexicographic pool order.
///
/// Correctness invariants (differential parity tests against the naive
/// reference enumerator):
/// - a trail's constraint is DISTINCT POOLS, not distinct tokens or
///   bundles: a walk may revisit the same pair, consuming another pool;
/// - the walk-level kind/nvd checks are necessary conditions only; exact
///   per-step pool-kind feasibility is decided in the expansion;
/// - hop-distance cutoffs, the sorted prefix break, `min_depth`, the
///   `include_reverse` interleave, cancellation, and the discovery
///   heartbeat all behave as before.
pub struct BundledSearch<B: Borrow<PathGraph>> {
    graph: B,
    end: u32,
    min_depth: usize,
    effective_max_depth: Option<usize>,
    include_reverse: bool,
    pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    node_valid_depths: Option<Vec<Vec<bool>>>,
    filter_len: usize,
    /// Per-depth allowed-kind bitmask (a depth with no qualifying pool kind
    /// prunes every step into it). Empty when no filter was supplied.
    allowed_masks: Vec<u32>,
    /// Bundle incidence per token — CSR over `(other token, bundle)` pairs,
    /// stable-sorted by hop distance to `end` so the stop-admissible cutoff
    /// is a prefix break. Member pools and kind masks live on the graph's
    /// bundle layer (see `PathGraph::bundle_pools`).
    tok_bundle_offsets: Vec<u32>,
    tok_bundle_flat: Vec<(u32, u32)>,
    /// Admissible cutoff data for the bounded search (None when unbounded).
    prune: Option<SearchPrune>,
    stack: Vec<(u32, usize, bool)>,
    /// Bundle chosen at each walk step, parallel to the DFS path.
    walk_bundles: Vec<u32>,
    /// Active-walk use count per bundle: the `u`-th visit of a bundle
    /// consumes the `(u+1)`-th distinct member pool, so a visit is only
    /// possible while `use < multiplicity`.
    bundle_use: Vec<u32>,
    /// Flat per-bundle multiplicity (member pool count), copied from the
    /// graph's bundle offsets so the hot capacity check is two flat reads.
    bundle_mult: Vec<u32>,
    /// Live expansion of the walk currently parked at the `end` token.
    expansion: Option<WalkExpansion>,
    /// Whether the reversed form of the most recent yield is still pending
    /// emission (used only when `include_reverse` is set).
    pending_reverse: bool,
    /// Concrete pool indices of the most recent yield, in walk order.
    emitted: Vec<u32>,
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
    /// Caller-installed long-run observation hook ([`WalkerTally`]); when
    /// present it replaces the default stderr heartbeat as the emit channel.
    progress: Option<Box<dyn FnMut(&WalkerTally) + Send + Sync>>,
    /// Wall clock between progress reports. The hook's interval when a hook
    /// is installed; [`DISCOVERY_HEARTBEAT`] for the default stderr line.
    progress_every: Duration,
}

/// Unordered compact token pair packed into one u64 (identity-hashed keys).
#[inline]
fn pack_token_pair(a: u32, b: u32) -> u64 {
    (u64::from(a.min(b)) << 32) | u64::from(a.max(b))
}

impl<B: Borrow<PathGraph>> BundledSearch<B> {
    /// Assemble from explicit parameters (both public constructors funnel
    /// here). The filter's `node_valid_depths` lookahead table is passed in
    /// precomputed so the borrowing and owning constructors can differ in
    /// where it comes from.
    #[expect(clippy::too_many_arguments)]
    fn with_params(
        graph: B,
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
        include_reverse: bool,
        pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
        node_valid_depths: Option<Vec<Vec<bool>>>,
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

        let src = graph.borrow();

        // Remap external start/end token IDs to compact indices. If EITHER
        // boundary token is absent from the graph, no start->end path exists
        // - yield nothing. `end` must NOT fall back to a synthetic index:
        // remapping an absent end to compact index 0 made the DFS search for
        // cycles ending at an unrelated token (compact 0), yielding
        // non-closing paths that tripped the direction-resolution fail-stop.
        let start_idx = src.compact_index(start);
        let end_idx = src.compact_index(end);
        let boundaries_present = matches!((start_idx, end_idx), (Some(_), Some(_)));
        let end_idx = end_idx.unwrap_or(0);

        // Admissible prune data (single BFS distance table). Only meaningful
        // for a bounded search; an unbounded one has no budget to cut against.
        let prune = match effective_max_depth {
            Some(_) if boundaries_present => Some(SearchPrune::build(src, end_idx)),
            _ => None,
        };

        // Per-depth allowed-kind bitmasks (0 bits = no pool qualifies there).
        let allowed_masks: Vec<u32> = pool_type_per_depth
            .as_ref()
            .map(|filter| {
                filter
                    .iter()
                    .map(|allowed| match allowed {
                        None => u32::MAX,
                        Some(kinds) => kinds.iter().fold(0u32, |acc, k| acc | (1u32 << k.as_u8())),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Per-token bundle incidence, stable-sorted by hop distance to the
        // target (ties keep bundle first-appearance order), so the
        // stop-admissible cutoff reduces to a prefix break: at a hub with
        // 10^4+ parallel pools and 1 hop of budget left, the scan stops
        // after the handful of target-adjacent bundles. The CSR comes from
        // the graph (built once at construction); only the memcpy'd flat
        // array gets re-ordered per search.
        let (tok_bundle_offsets, mut tok_bundle_flat) = src.token_bundles_csr();
        if let Some(prune_data) = prune.as_ref() {
            for w in 0..src.nodes() {
                let start = tok_bundle_offsets[w] as usize;
                let stop = tok_bundle_offsets[w + 1] as usize;
                tok_bundle_flat[start..stop]
                    .sort_by_key(|(nbr, _)| prune_data.to_end[*nbr as usize]);
            }
        }

        let n_bundles = src.bundle_pairs.len();
        let bundle_mult: Vec<u32> = src.bundle_offsets.windows(2).map(|w| w[1] - w[0]).collect();
        let now = Instant::now();
        let (stack, done) = if boundaries_present {
            (vec![(start_idx.unwrap_or(0), 0, false)], false)
        } else {
            (Vec::new(), true)
        };

        Self {
            graph,
            end: end_idx,
            min_depth,
            effective_max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
            filter_len,
            allowed_masks,
            tok_bundle_offsets,
            tok_bundle_flat,
            prune,
            stack,
            walk_bundles: Vec::with_capacity(16),
            bundle_mult,
            bundle_use: vec![0; n_bundles],
            expansion: None,
            pending_reverse: false,
            emitted: Vec::new(),
            done,
            cancel: None,
            search_started: now,
            paths_yielded: 0,
            advances_since_yield: 0,
            last_heartbeat: now,
            max_stack_depth: 0,
            progress: None,
            progress_every: DISCOVERY_HEARTBEAT,
        }
    }

    /// Install a long-run observation hook on top of the default heartbeat.
    ///
    /// The hook replaces the stderr line as the emit channel: once `every` of
    /// wall clock has passed on a heartbeat checkpoint (every
    /// [`HEARTBEAT_CHECK_EVERY`] stack-frame advances), the walk's
    /// [`WalkerTally`] snapshot is handed to `sink`. Callers that run walks
    /// to completion (no cancellation) use this to surface no-yield
    /// stragglers — the tally's `advances_since_yield` is large precisely
    /// when the DFS is grinding with nothing ready to return.
    #[must_use]
    pub fn with_progress(
        mut self,
        every: Duration,
        sink: impl FnMut(&WalkerTally) + Send + Sync + 'static,
    ) -> Self {
        self.progress = Some(Box::new(sink));
        self.progress_every = every;
        self
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
    /// (read it from `emitted`), [`AdvanceOutcome::Reversed`] when the
    /// pending reverse of the previous yield is ready, or
    /// [`AdvanceOutcome::Exhausted`] when the search is done. Shared DFS
    /// core: `next_path` (materializing `EdgeKey`s) and
    /// `next_path_indices_into` (appending pool indices) dispatch through it.
    #[expect(clippy::too_many_lines)]
    fn advance(&mut self) -> AdvanceOutcome {
        if self.done || self.cancelled() {
            self.done = true;
            return AdvanceOutcome::Exhausted;
        }

        let filter_slice = self.pool_type_per_depth.as_deref();
        let nvd_ref = self.node_valid_depths.as_deref();
        let masks: &[u32] = &self.allowed_masks;
        let prune_ref = self.prune.as_ref();
        let src: &PathGraph = self.graph.borrow();

        loop {
            // 1. Emit from a live expansion before doing any further DFS
            //    work: forward and reversed assignments of the parked walk
            //    interleave exactly as the old per-pool engine interleaved
            //    forwards and reverses.
            if self.expansion.is_some() {
                if self.pending_reverse {
                    self.pending_reverse = false;
                    self.paths_yielded += 1;
                    self.advances_since_yield = 0;
                    return AdvanceOutcome::Reversed;
                }
                let next_ready = match self.expansion.as_mut() {
                    Some(exp) => exp.next_assignment_into(&mut self.emitted),
                    None => false,
                };
                if next_ready {
                    self.pending_reverse = self.include_reverse;
                    self.paths_yielded += 1;
                    self.advances_since_yield = 0;
                    return AdvanceOutcome::Forward;
                }
                // Expansion exhausted: resume walking from the end frame
                // (its yield flag is consumed; scanning continues).
                self.expansion = None;
            }

            // 2. Walk the bundle DFS until the next expansion begins.
            loop {
                // Cooperative cancellation: a set flag stops the search at
                // the next loop iteration.
                if self
                    .cancel
                    .as_ref()
                    .is_some_and(|c| c.load(Ordering::Relaxed))
                {
                    break;
                }

                let stack_len = self.stack.len();
                if stack_len == 0 {
                    break;
                }

                // Discovery heartbeat: amortized (checked every
                // `HEARTBEAT_CHECK_EVERY` stack-frame iterations, not per
                // edge) so `Instant::now` stays out of the hot inner loop.
                self.advances_since_yield = self.advances_since_yield.wrapping_add(1);
                if stack_len > self.max_stack_depth {
                    self.max_stack_depth = stack_len;
                }
                if self
                    .advances_since_yield
                    .is_multiple_of(HEARTBEAT_CHECK_EVERY)
                {
                    let now = Instant::now();
                    if now.duration_since(self.last_heartbeat) >= self.progress_every {
                        self.last_heartbeat = now;
                        let tally = WalkerTally {
                            elapsed: now.duration_since(self.search_started),
                            paths_yielded: self.paths_yielded,
                            advances_since_yield: self.advances_since_yield,
                            max_stack_depth: self.max_stack_depth,
                        };
                        match self.progress.as_mut() {
                            Some(sink) => sink(&tally),
                            // Default diagnostic on a zero-dependency leaf; no
                            // logging crate is available and this runs off the
                            // hot path.
                            None => {
                                #[expect(clippy::print_stderr)]
                                {
                                    eprintln!(
                                        "discovery heartbeat: elapsed={:?} \
                                         paths_yielded={} advances_since_yield={} max_stack_depth={}",
                                        tally.elapsed, tally.paths_yielded,
                                        tally.advances_since_yield, tally.max_stack_depth
                                    );
                                }
                            }
                        }
                    }
                }

                let frame = &mut self.stack[stack_len - 1];
                let (node, edge_idx, yield_checked) = frame;

                // Check yield condition (once per frame arrival): the walk
                // reached the end token with enough pools. The expansion
                // emits canonical forward assignment(s) — one concrete path
                // per call — starting on the next outer-loop iteration.
                if !*yield_checked {
                    *yield_checked = true;
                    if *node == self.end && self.walk_bundles.len() >= self.min_depth {
                        let expansion = self.build_expansion();
                        self.expansion = Some(expansion);
                        break;
                    }
                }

                // Stop recursion if the walk has reached the maximum depth.
                if let Some(emd) = self.effective_max_depth {
                    if self.walk_bundles.len() >= emd {
                        // Backtrack.
                        self.stack.pop();
                        if let Some(bundle) = self.walk_bundles.pop() {
                            self.bundle_use[bundle as usize] -= 1;
                        }
                        continue;
                    }
                }

                // Remaining hop budget for the step chosen at THIS node
                // visit. A trail extended by one bundle must still reach the
                // target within `emd - len - 1` hops; the BFS hop distance is
                // a lower bound on how many hops that takes, so the cutoff
                // never discards a yieldable trail. Len < emd is guaranteed
                // by the max-depth backtrack above, so the subtraction cannot
                // underflow. (At the closing depth the remaining budget is 0,
                // and since `end` is the unique token at hop distance 0, the
                // prefix break admits only target-adjacent bundles — the
                // old engine's separate final-hop end-edge list is implicit.)
                let remaining_budget = self.effective_max_depth.map(|emd| {
                    u32::try_from(emd - self.walk_bundles.len() - 1).unwrap_or(u32::MAX)
                });
                let entry_base = self.tok_bundle_offsets[*node as usize] as usize;
                let entries_len = self.tok_bundle_offsets[*node as usize + 1] as usize - entry_base;
                let mut found_bundle = false;
                while *edge_idx < entries_len {
                    let (nbr, bundle) = self.tok_bundle_flat[entry_base + *edge_idx];

                    // Sorted-prefix break: adjacency is ordered by hop
                    // distance, so the first neighbor beyond the remaining
                    // budget terminates the scan: every later neighbor is at
                    // least as far, and none of them can be on a yieldable
                    // trail.
                    if let Some(prune_data) = prune_ref {
                        if prune_data.to_end[nbr as usize] > remaining_budget.unwrap_or(u32::MAX) {
                            break;
                        }
                    }

                    *edge_idx += 1;

                    // Capacity: the `u`-th visit of a bundle consumes another
                    // distinct member pool, so a visit is only possible while
                    // the count is below the multiplicity.
                    if self.bundle_use[bundle as usize] >= self.bundle_mult[bundle as usize] {
                        continue;
                    }

                    // Per-depth pool-type filter (walk-level necessary
                    // condition; the expansion enforces the exact sets).
                    if filter_slice.is_some() {
                        let depth = self.walk_bundles.len();
                        if depth >= self.filter_len {
                            continue;
                        }
                        if src.bundle_kind_mask(bundle) & masks.get(depth).copied().unwrap_or(0)
                            == 0
                        {
                            continue;
                        }

                        // Lookahead pruning: skip if the neighbor token
                        // can't continue at the next depth.
                        let next_depth = depth + 1;
                        if next_depth < self.filter_len {
                            if let Some(valid) = nvd_ref.and_then(|nvd| nvd.get(nbr as usize)) {
                                if !valid[next_depth] {
                                    continue;
                                }
                            }
                        }
                    }

                    // Found a bundle — extend the walk and descend.
                    self.stack.push((nbr, 0, false));
                    self.walk_bundles.push(bundle);
                    self.bundle_use[bundle as usize] += 1;
                    found_bundle = true;
                    break;
                }

                if !found_bundle {
                    // No more bundles to explore from this token — backtrack.
                    self.stack.pop();
                    if let Some(bundle) = self.walk_bundles.pop() {
                        self.bundle_use[bundle as usize] -= 1;
                    }
                }
            }

            // A yield fired mid-walk and parked an expansion on the finder:
            // return to the emission dispatcher at the top of the outer loop
            // instead of exhausting.
            if self.expansion.is_some() {
                continue;
            }

            // Search exhausted (or cancelled) — emit the final status line.
            self.emit_discovery_complete();
            self.done = true;
            return AdvanceOutcome::Exhausted;
        }
    }

    /// Build the expansion for a walk parked at `end`: per-step candidate
    /// pools limited to the depth's allowed kinds (the full bundle when
    /// unfiltered).
    fn build_expansion(&self) -> WalkExpansion {
        let graph = self.graph.borrow();
        let filtered = self.pool_type_per_depth.is_some();
        let slots: Vec<Vec<u32>> = self
            .walk_bundles
            .iter()
            .enumerate()
            .map(|(depth, &bundle)| {
                if !filtered {
                    return graph.bundle_pools(bundle).to_vec();
                }
                let mask = self.allowed_masks.get(depth).copied().unwrap_or(0);
                graph
                    .bundle_pools(bundle)
                    .iter()
                    .copied()
                    .filter(|&pool_idx| {
                        (1u32 << graph.pools[pool_idx as usize].1.as_u8()) & mask != 0
                    })
                    .collect()
            })
            .collect();
        let n = slots.len();
        WalkExpansion {
            slot_bundle: self.walk_bundles.clone(),
            slots,
            cursor: vec![0; n],
            chosen: vec![0; n],
            started: false,
            finished: false,
        }
    }

    /// Emit a final discovery-complete line so the operator sees the total at
    /// search end (cheap; covers the common fast-search case that never
    /// tripped the throttled heartbeat).
    fn emit_discovery_complete(&self) {
        let elapsed = self.search_started.elapsed();
        // Low-frequency stderr diagnostic on a zero-dependency leaf (no
        // logging crate available); one line at discovery completion.
        #[expect(clippy::print_stderr)]
        {
            eprintln!(
                "discovery complete: elapsed={elapsed:?} paths_yielded={} max_stack_depth={}",
                self.paths_yielded, self.max_stack_depth
            );
        }
    }

    /// Advance the DFS and return the next complete path, or `None` if the
    /// search is exhausted.
    ///
    /// If `include_reverse` is set, each found cycle yields the forward path
    /// first, then the reversed path on the next call.
    #[must_use]
    pub fn next_path(&mut self) -> Option<Vec<EdgeKey>> {
        match self.advance() {
            AdvanceOutcome::Exhausted => None,
            AdvanceOutcome::Forward => Some(
                self.emitted
                    .iter()
                    .map(|&idx| self.graph.borrow().pools[idx as usize])
                    .collect(),
            ),
            AdvanceOutcome::Reversed => Some(
                self.emitted
                    .iter()
                    .rev()
                    .map(|&idx| self.graph.borrow().pools[idx as usize])
                    .collect(),
            ),
        }
    }

    /// Advance the DFS and append the next path's **pool indices** into
    /// `out`, returning the number of indices appended (the path length), or
    /// `None` if the search is exhausted.
    ///
    /// This is the allocation-free hot path used by the `PyO3` iterator:
    /// instead of materializing a `Vec<EdgeKey>` per yielded path, it
    /// appends the compact `u32` pool indices into a caller-owned flat
    /// buffer. The FFI layer converts indices → `(pool_id, kind_u8)` lazily
    /// while building Python objects.
    #[must_use]
    pub fn next_path_indices_into(&mut self, out: &mut Vec<u32>) -> Option<usize> {
        match self.advance() {
            AdvanceOutcome::Exhausted => None,
            AdvanceOutcome::Forward => {
                let len = self.emitted.len();
                out.extend(self.emitted.iter().copied());
                Some(len)
            }
            AdvanceOutcome::Reversed => {
                let len = self.emitted.len();
                out.extend(self.emitted.iter().rev().copied());
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
        self.graph.borrow().pools[pool_idx as usize]
    }
}

impl<B: Borrow<PathGraph>> Iterator for BundledSearch<B> {
    type Item = Vec<EdgeKey>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_path()
    }
}

/// Borrowing lazy DFS over a shared [`PathGraph`] (the bundled core; see
/// [`BundledSearch`] for the invariants and the bundling rationale).
pub type PathFinder<'a> = BundledSearch<&'a PathGraph>;

/// Owning lazy DFS — the PyO3-friendly form (no lifetime parameters, so it
/// can be stored in a `#[pyclass]` and iterated from Python one path at a
/// time). The graph, filter, and node-valid-depths are owned by the finder.
pub type OwnedPathFinder = BundledSearch<Box<PathGraph>>;

impl BundledSearch<Box<PathGraph>> {
    /// Create from an owned graph plus search parameters. Computes the
    /// lookahead `node_valid_depths` table from the filter.
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
        let node_valid_depths = pool_type_per_depth
            .as_ref()
            .map(|filter| graph.compute_node_valid_depths(filter));
        Self::with_params(
            Box::new(graph),
            start,
            end,
            min_depth,
            max_depth,
            include_reverse,
            pool_type_per_depth,
            node_valid_depths,
        )
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
        let filter = pool_type_per_depth.map(<[Option<Vec<PoolKind>>]>::to_vec);
        let nvd = node_valid_depths.map(<[Vec<bool>]>::to_vec);
        BundledSearch::with_params(
            self,
            start,
            end,
            min_depth,
            max_depth,
            include_reverse,
            filter,
            nvd,
        )
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
    use std::collections::{BTreeSet, HashSet};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// A walk that grinds past the heartbeat checkpoint BEFORE its first
    /// yield reports through the caller's hook: hub-rooted walks (the
    /// production frame that anchored 0x1d8168b6…) run 9000 spokes before
    /// reaching the closing leaf, so a zero-interval hook observes un-yielded
    /// DFS work mid-walk with the running tally.
    #[test]
    fn test_progress_hook_fires_mid_walk_with_the_tally() {
        const SPOKES: u64 = 9000;
        const HUB: u64 = 10_000;
        const END: u64 = 11_000;
        let mut edges: Vec<(u64, u64, u64, PoolKind)> = Vec::with_capacity(SPOKES as usize + 1);
        let mut pool = 1000u64;
        for leaf in 1..=SPOKES {
            edges.push((HUB, HUB + leaf, pool, PoolKind::V2));
            pool += 1;
        }
        // The lone closing edge hangs off the last spoke; unbounded depth (no
        // prune sort) keeps the closing path at the END of the scan order.
        edges.push((HUB + SPOKES, END, pool, PoolKind::V2));
        let graph = PathGraph::from_edges(edges);

        let calls = Arc::new(AtomicUsize::new(0));
        let saw_mid_walk = Arc::new(AtomicBool::new(false));
        {
            let calls = Arc::clone(&calls);
            let saw_mid_walk = Arc::clone(&saw_mid_walk);
            let mut finder = graph
                .find_paths_iter(HUB, END, 2, None, false, None, None)
                .with_progress(Duration::ZERO, move |t| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    if t.advances_since_yield > 0 {
                        saw_mid_walk.store(true, Ordering::Relaxed);
                    }
                });
            assert!(finder.next_path().is_some(), "the lone closing path yields");
        }
        assert!(calls.load(Ordering::Relaxed) >= 1, "the hook fired");
        assert!(
            saw_mid_walk.load(Ordering::Relaxed),
            "a tally arrived while the DFS was mid-grind (advances > 0, no yield yet)"
        );
    }

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

    /// Token id -> compact index (tests live in-module, so the private
    /// `compact_index` seam is directly reachable).
    #[expect(clippy::expect_used)]
    fn graph_compact(graph: &PathGraph, token: u64) -> u32 {
        graph
            .compact_index(token)
            .expect("test fixture token is interned")
    }

    /// Token id -> compact index for hand-built graphs without fixtures.
    #[expect(clippy::cast_possible_truncation)]
    fn intern_ref(map: &mut HashMap<u64, u32>, adj: &mut Vec<Vec<(u32, u32)>>, t: u64) -> u32 {
        if let Some(&i) = map.get(&t) {
            i
        } else {
            let i = map.len() as u32;
            map.insert(t, i);
            adj.push(Vec::new());
            i
        }
    }

    /// A big parallel bundle on A<->B plus single-pool B<->C, C<->A: forces
    /// walks that traverse the SAME bundle two and four times, and 3-hop
    /// triangles closing before the max budget.
    fn battery_parallel_hub_trio() -> Vec<(u64, u64, u64, PoolKind)> {
        let mut edges: Vec<(u64, u64, u64, PoolKind)> = Vec::new();
        for i in 0..40u64 {
            edges.push((1, 2, 700_000 + i, PoolKind::V2));
        }
        edges.push((2, 3, 799_001, PoolKind::V2));
        edges.push((3, 1, 799_002, PoolKind::V2));
        edges
    }

    /// Reference enumerator: ALL edge-trails from `start` to `end` with
    /// `min_depth <= len <= max_depth`, by naive full backtracking. This is
    /// the independent oracle every pruned search must reproduce.
    fn reference_trails(
        edges: &[(u64, u64, u64, PoolKind)],
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<usize>,
    ) -> BTreeSet<Vec<u64>> {
        let mut token_index: HashMap<u64, u32> = HashMap::new();
        let mut adj: Vec<Vec<(u32, u32)>> = Vec::new();
        let mut pool_ids: Vec<u64> = Vec::new();
        for (t0, t1, pid, _kind) in edges {
            let a = intern_ref(&mut token_index, &mut adj, *t0);
            let b = intern_ref(&mut token_index, &mut adj, *t1);
            let fresh_pool = u32::try_from(pool_ids.len()).unwrap_or(u32::MAX);
            pool_ids.push(*pid);
            adj[a as usize].push((b, fresh_pool));
            adj[b as usize].push((a, fresh_pool));
        }
        let s = intern_ref(&mut token_index, &mut adj, start);

        let mut out: BTreeSet<Vec<u64>> = BTreeSet::new();
        let mut visited: Vec<bool> = vec![false; pool_ids.len()];
        let mut path: Vec<u32> = Vec::new();

        #[expect(clippy::too_many_arguments, clippy::items_after_statements)]
        fn dfs(
            node: u32,
            end: u32,
            min_depth: usize,
            max_depth: Option<usize>,
            adj: &[Vec<(u32, u32)>],
            pool_ids: &[u64],
            visited: &mut [bool],
            path: &mut Vec<u32>,
            out: &mut BTreeSet<Vec<u64>>,
        ) {
            if node == end && path.len() >= min_depth {
                out.insert(path.iter().map(|&i| pool_ids[i as usize]).collect());
            }
            if max_depth.is_some_and(|md| path.len() >= md) {
                return;
            }
            for (nbr, pool_idx) in &adj[node as usize] {
                if visited[*pool_idx as usize] {
                    continue;
                }
                visited[*pool_idx as usize] = true;
                path.push(*pool_idx);
                dfs(
                    *nbr, end, min_depth, max_depth, adj, pool_ids, visited, path, out,
                );
                path.pop();
                visited[*pool_idx as usize] = false;
            }
        }
        dfs(
            s,
            intern_ref(&mut token_index, &mut adj, end),
            min_depth,
            max_depth,
            &adj,
            &pool_ids,
            &mut visited,
            &mut path,
            &mut out,
        );
        out
    }

    /// A cycle that must walk AWAY from the target before closing, plus the
    /// shortcut 2-cycle, dead-end chains, a chain-through tail, and a pendant
    /// far tail. Classic trap for overly tight admissible cutoffs.
    fn battery_go_around() -> Vec<(u64, u64, u64, PoolKind)> {
        vec![
            (1, 2, 101, PoolKind::V2),   // WETH-A (2-cycle shortcut)
            (1, 2, 102, PoolKind::V2),   // parallel
            (2, 3, 103, PoolKind::V2),   // A-B
            (3, 4, 104, PoolKind::V2),   // B-C
            (4, 1, 105, PoolKind::V2),   // C-WETH closes the 4-cycle
            (2, 99, 106, PoolKind::V2),  // dead-end chain A-X
            (99, 98, 107, PoolKind::V2), // X chain-through, no other exit
            (1, 5, 108, PoolKind::V2),   // pendant far tail WETH-E
            (5, 6, 109, PoolKind::V2),   // E-F
        ]
    }

    /// Two triangles sharing the hub WETH, joined by a single-pool bridge
    /// (A-C) that sits inside the 2-core, with parallel bundles.
    fn battery_bridge_in_2core() -> Vec<(u64, u64, u64, PoolKind)> {
        vec![
            (1, 2, 201, PoolKind::V2),
            (1, 2, 202, PoolKind::V3),
            (2, 3, 203, PoolKind::V2),
            (3, 1, 204, PoolKind::V2),
            (1, 4, 205, PoolKind::V2),
            (4, 5, 206, PoolKind::V2),
            (5, 1, 207, PoolKind::V3),
            (2, 4, 208, PoolKind::V2), // the bridge between the triangles
        ]
    }

    fn hub_node(s: u64) -> u64 {
        100 * s + s
    }

    /// Hub WETH with 2 parallel pools to each of 5 spokes, plus a spoke ring.
    /// The multiplicity bundle a per-step cutoff must survive.
    fn battery_hub_parallel() -> Vec<(u64, u64, u64, PoolKind)> {
        let mut edges = Vec::new();
        let mut pid = 300u64;
        for spoke in 11..16u64 {
            for _ in 0..2 {
                pid += 1;
                edges.push((1, hub_node(spoke), pid, PoolKind::V2));
            }
        }
        for s in 11..15u64 {
            pid += 1;
            edges.push((hub_node(s), hub_node(s + 1), pid, PoolKind::V2));
        }
        edges
    }

    fn assert_search_parity(
        edges: &[(u64, u64, u64, PoolKind)],
        start: u64,
        end: u64,
        label: &str,
    ) {
        for min_depth in [1usize, 2, 3] {
            for max_depth in [Some(min_depth), Some(min_depth + 1), Some(min_depth + 2)] {
                let mut found: BTreeSet<Vec<u64>> = BTreeSet::new();
                let mut finder = OwnedPathFinder::new(
                    PathGraph::from_edges(edges.to_vec()),
                    start,
                    end,
                    min_depth,
                    max_depth,
                    false,
                    None,
                );
                while let Some(path) = finder.next_path() {
                    found.insert(path.into_iter().map(|(pid, _)| pid).collect());
                }
                let reference = reference_trails(edges, start, end, min_depth, max_depth);
                assert_eq!(
                    found, reference,
                    "{label} min={min_depth} max={max_depth:?}: pruned search diverged"
                );
            }
        }
    }

    /// Core DFS throughput (no FFI/yield conversion): 2-connected grid with
    /// yields only near the start, so dead-branch churn dominates.
    /// `#[ignore]`d — run explicitly with
    /// `cargo test -p degenbot-pathfinding --release perf_core -- --ignored --nocapture`.
    #[test]
    #[ignore = "manual perf harness: cargo test -p degenbot-pathfinding --release perf_core -- --ignored --nocapture"]
    #[expect(clippy::print_stderr)]
    fn perf_core_grid_search() {
        let w = 60usize;
        let gid = |r: usize, c: usize| 500_000u64 + (r * w + c) as u64;
        let mut edges: Vec<(u64, u64, u64, PoolKind)> = Vec::new();
        let mut pid = 900_000u64;
        for r in 0..w {
            for c in 0..w {
                if c + 1 < w {
                    pid += 1;
                    edges.push((gid(r, c), gid(r, c + 1), pid, PoolKind::V2));
                }
                if r + 1 < w {
                    pid += 1;
                    edges.push((gid(r, c), gid(r + 1, c), pid, PoolKind::V2));
                }
                if r + 1 < w && c + 1 < w {
                    pid += 1;
                    edges.push((gid(r, c + 1), gid(r + 1, c), pid, PoolKind::V2));
                }
            }
        }
        let n_runs = 20;
        let mut best = f64::MAX;
        let mut best_build = f64::MAX;
        for _ in 0..n_runs {
            let d_start = std::time::Instant::now();
            let graph = PathGraph::from_edges(edges.clone());
            let build_start = std::time::Instant::now();
            let mut finder = OwnedPathFinder::new(graph, 500_000, 500_000, 3, Some(5), false, None);
            let build_dt = build_start.elapsed().as_secs_f64() * 1e3;
            let mut count = 0u64;
            while finder.next_path().is_some() {
                count += 1;
            }
            let dt = d_start.elapsed().as_secs_f64() * 1e3;
            if dt < best {
                best = dt;
            }
            if build_dt < best_build {
                best_build = build_dt;
            }
            assert_eq!(count, 8); // 8 unique cycles, forward only here
        }
        eprintln!("perf_core_grid_search best={best:.3} ms (finder build best={best_build:.3} ms)");
    }

    /// Bundle-heavy core throughput: one big parallel bundle on A<->B plus
    /// single-pool B<->C, C<->A. Depth-4 sweeps force the walk to traverse
    /// the bundle four times (40P4 concrete expansions). The per-pool engine
    /// multiplies the whole subtree by the multiplicity at each level; the
    /// bundled walk enumerates the token shape once.
    #[test]
    #[ignore = "manual perf harness: cargo test -p degenbot-pathfinding --release perf_core -- --ignored --nocapture"]
    #[expect(clippy::print_stderr)]
    fn perf_core_parallel_hub() {
        let m = 40u64;
        let mut edges: Vec<(u64, u64, u64, PoolKind)> = Vec::new();
        for i in 0..m {
            edges.push((1, 2, 700_000 + i, PoolKind::V2));
        }
        edges.push((2, 3, 799_001, PoolKind::V2));
        edges.push((3, 1, 799_002, PoolKind::V2));

        let n_runs = 5;
        let mut best = f64::MAX;
        for _ in 0..n_runs {
            let d_start = std::time::Instant::now();
            let graph = PathGraph::from_edges(edges.clone());
            let mut finder = OwnedPathFinder::new(graph, 1, 1, 2, Some(4), false, None);
            let mut count = 0u64;
            while finder.next_path().is_some() {
                count += 1;
            }
            let dt = d_start.elapsed().as_secs_f64() * 1e3;
            if dt < best {
                best = dt;
            }
            // len-2: 40P2; len-3 via C (both directions): 2m;
            // len-4 (two round trips): 40P4.
            let p2 = m * (m - 1);
            let p4 = p2 * (m - 2) * (m - 3);
            assert_eq!(
                count,
                p2 + 2 * m + p4,
                "falling-factorial expansion contract"
            );
        }
        eprintln!("perf_core_parallel_hub best={best:.3} ms");
    }

    #[test]
    fn test_hop_distances_basics() {
        let graph = build_fixture_graph();
        // Fixture: WETH-A (2 parallel pools), A-B, B-WETH — B connects to
        // WETH directly, so every fixture node sits within 1 hop of WETH.
        let d = graph.hop_distances(&[graph_compact(&graph, WETH)]);
        assert_eq!(d[graph_compact(&graph, WETH) as usize], 0);
        assert_eq!(d[graph_compact(&graph, A) as usize], 1);
        assert_eq!(d[graph_compact(&graph, B) as usize], 1);
        let d_from_b = graph.hop_distances(&[graph_compact(&graph, B)]);
        assert_eq!(d_from_b[graph_compact(&graph, WETH) as usize], 1);
        assert_eq!(d_from_b[graph_compact(&graph, A) as usize], 1);
    }

    #[test]
    fn test_hop_distances_unreachable_is_max() {
        let graph = PathGraph::from_edges(vec![
            (1, 2, 1, PoolKind::V2),
            (8, 9, 2, PoolKind::V2), // separate component
        ]);
        let d = graph.hop_distances(&[graph_compact(&graph, 1)]);
        for token in [8, 9] {
            assert_eq!(
                d[graph_compact(&graph, token) as usize],
                u32::MAX,
                "token {token} unreachable"
            );
        }
    }

    /// Trails that close on the target EARLIER than the max budget must
    /// still be yielded whenever `min_depth` allows them. Regression: the
    /// former penultimate-hop reachability prune checked
    /// `end_edges[neighbor]` on the closing step's predecessor — but when the
    /// neighbor IS the target, `end_edges[end]` is empty (no self-loops), so
    /// every shorter-than-budget close was silently amputated (a depth-3
    /// sweep yielded no 2-pool cycles at all; 360/360 parallel-pool 2-hop
    /// cycles vanished on the 60-spoke hub fixture). The hop-distance cutoff
    /// admits the target itself (`d_end[end] = 0 <= remaining`), restoring
    /// them. The differential parity batteries also cover this, but this
    /// test names the exact shape.
    #[test]
    fn test_regress_trail_closing_earlier_than_budget_is_yielded() {
        // 3 parallel hubs pools WETH<->A, nothing else.
        let edges = {
            let mut e = Vec::new();
            for pool in 100..103u64 {
                e.push((1u64, 2u64, pool, PoolKind::V2));
            }
            e
        };
        // min_depth=2, max_depth=3 (production's floor/limit relationship):
        // the 2-hop cycles are within budget and must be yielded.
        let mut two_hop: Vec<Vec<u64>> = Vec::new();
        let mut finder = OwnedPathFinder::new(
            PathGraph::from_edges(edges.clone()),
            1,
            1,
            2,
            Some(3),
            false,
            None,
        );
        while let Some(path) = finder.next_path() {
            if path.len() == 2 {
                two_hop.push(path.iter().map(|(pid, _)| *pid).collect());
            }
        }
        // 6 distinct directed trails (3 pools * 2 orderings, distinct pools
        // per trail). Zero would be the regression shape.
        assert_eq!(
            two_hop.len(),
            6,
            "2-pool cycles closing before the max budget must be yielded: {two_hop:?}"
        );
    }

    #[test]
    fn pruned_search_parity_go_around() {
        assert_search_parity(&battery_go_around(), 1, 1, "go_around");
    }

    #[test]
    fn pruned_search_parity_bridge_in_2core() {
        assert_search_parity(&battery_bridge_in_2core(), 1, 1, "bridge_in_2core");
    }

    #[test]
    fn pruned_search_parity_hub_parallel() {
        assert_search_parity(&battery_hub_parallel(), 1, 1, "hub_parallel");
    }

    #[test]
    fn pruned_search_parity_parallel_hub_trio() {
        assert_search_parity(&battery_parallel_hub_trio(), 1, 1, "parallel_hub_trio");
    }

    #[test]
    fn pruned_search_parity_open_paths() {
        // start != end: the pruned DFS must serve open traversals too.
        assert_search_parity(&battery_go_around(), 1, 5, "go_around_open");
        assert_search_parity(&battery_bridge_in_2core(), 1, 4, "bridge_open");
    }

    /// A token walk may traverse the SAME unordered pool pair more than
    /// once, consuming a DISTINCT pool per visit. Counting contract for the
    /// bundled engine: `u` visits of a bundle with multiplicity `m` expand
    /// to the ordered selections `m·(m-1)·...·(m-u+1)`.
    ///
    /// Shape: 4 parallel pools WETH<->A, nothing else. Walks from WETH back
    /// to WETH with `min=2, max=4`: len-2 cycles use the bundle twice
    /// (4·3 = 12) and len-4 cycles use it four times (4·3·2·1 = 24).
    #[test]
    fn test_parity_parallel_bundle_revisited_pair() {
        let mut edges = Vec::new();
        for pool in 400..404u64 {
            edges.push((1u64, 2u64, pool, PoolKind::V2));
        }
        let mut finder = OwnedPathFinder::new(
            PathGraph::from_edges(edges.clone()),
            1,
            1,
            2,
            Some(4),
            false,
            None,
        );
        let mut count = 0;
        while finder.next_path().is_some() {
            count += 1;
        }
        assert_eq!(count, 12 + 24, "falling-factorial expansion: {count}");
        assert_search_parity(&edges, 1, 1, "revisit_pair");
    }

    /// Mixed-kind parallel bundle + per-depth kind filter: the walk-level
    /// kind check is a NECESSARY condition (any kind-allowed pool exists);
    /// the expansion enforces the exact per-step kind sets and the
    /// same-bundle distinctness across the walk's visits.
    ///
    /// Bundle WETH<->A: p1=V2, p2=V3, p3=V2. 2-hop cycles WETH-A-WETH.
    #[test]
    fn test_parity_mixed_kinds_parallel_bundle_with_filter() {
        fn collect(
            edges: &[(u64, u64, u64, PoolKind)],
            filter: Option<Vec<Option<Vec<PoolKind>>>>,
        ) -> BTreeSet<Vec<u64>> {
            let mut finder = OwnedPathFinder::new(
                PathGraph::from_edges(edges.to_vec()),
                1,
                1,
                2,
                Some(2),
                false,
                filter,
            );
            let mut out = BTreeSet::new();
            while let Some(path) = finder.next_path() {
                out.insert(path.into_iter().map(|(pid, _)| pid).collect());
            }
            out
        }

        let edges = vec![
            (1u64, 2u64, 500, PoolKind::V2),
            (1, 2, 501, PoolKind::V3),
            (1, 2, 502, PoolKind::V2),
        ];

        // No filter: ordered pairs over the full bundle: 3·2 = 6.
        let out = collect(&edges, None);
        assert_eq!(out.len(), 6, "unfiltered parallel bundle: {out:?}");

        // [V3, None]: slot 0 must be p(501); slot 1 any other pool: 2 paths.
        let out = collect(&edges, Some(vec![Some(vec![PoolKind::V3]), None]));
        let expected: BTreeSet<Vec<u64>> = BTreeSet::from([vec![501, 500], vec![501, 502]]);
        assert_eq!(out, expected, "V3-first filter expansion: {out:?}");

        // [V3, V3]: both slots need p(501) — impossible (distinct pools):
        // the walk passes the kind-necessity check but expansion yields 0.
        let out = collect(
            &edges,
            Some(vec![Some(vec![PoolKind::V3]), Some(vec![PoolKind::V3])]),
        );
        assert!(
            out.is_empty(),
            "kind-infeasible re-visit must yield nothing: {out:?}"
        );

        // Oracle parity on the same shape with reverse doubling.
        assert_search_parity(&edges, 1, 1, "mixed_kinds_bundle");
    }
}
