//! Touched-set discovery over the shared pathfinding walker.
//!
//! Each frame's touched pools anchor cycles into
//! [`degenbot_pathfinding`](::degenbot_pathfinding)'s DFS walker: for a
//! touched pool `p` spanning tokens `(a, b)`, the walker enumerates the open
//! paths `b → a` (and `a → b`) up to `max_hops - 1` connectors, and each
//! cycle is the anchor hop followed by one open path. Connector positions may
//! contain OTHER touched pools, so a cycle can carry several touched pools in
//! any rotation; cycles sharing a directed edge sequence are deduplicated
//! across their anchoring rotations. More-touched cycles rank ahead of
//! fewer-touched ones before the cap truncates, so a small cap cannot starve
//! dual-touched cycles. Both drift directions surface. Each walk runs to
//! completion — the walker's cap and hop budget bound the result — and a 1 s
//! `tracing::info!` progress report (elapsed, yields, walker advances)
//! surfaces a hub-rooted, no-yield straggler mid-grind.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V3Edge};
use degenbot_pathfinding::{EdgeKey, PathGraph, PoolKind};

/// One touched pool the frame's cycles anchor on: the connector-index
/// identity (pool id + table family) and the DB token ids it trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorPool {
    /// The connector-index pool id (the graph's edge identity).
    pub pool_id: u64,
    /// The pool-table family (`PoolKind` carries it; V4 has no index lane).
    pub pool_kind: PoolKind,
    /// First token id of the anchor's pair.
    pub token_a_id: u64,
    /// Second token id of the anchor's pair.
    pub token_b_id: u64,
}

/// One walker cycle: pools in traversal order (the first hop is the traversed
/// touched pool for this rotation), plus the DB id of the token the first
/// pool consumes — the cycle's transit currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfsCycle {
    /// `(pool_id, PoolKind)` hops in traversal order. The raw walker emits
    /// the touched anchor first; admission rotates the cycle to its stake
    /// entry, so the first hop is the WETH consumer for an admitted cycle.
    pub pools: Vec<EdgeKey>,
    /// The token the first hop consumes (the cycle's transit currency).
    pub entry_token_id: u64,
}

/// The trace label for an enumerated cycle no rotation of which consumes the
/// settlement token: it cannot be entered with a WETH stake and is refused
/// before the solver ever sees it.
pub const NON_WETH_CYCLE: &str = "non_weth_cycle";

/// The startup-built discovery graph: the connector index's loaded edge set
/// (V2 + V3; `PoolKind` carries the family) as a pathfinding multigraph,
/// built ONCE at startup — frames only traverse it.
#[derive(Clone)]
pub struct AnchoredGraph {
    graph: PathGraph,
    /// Pool identity → its `(token0_id, token1_id)` pair, from the same
    /// startup edge list as the graph; cycle dedup canonicalizes a directed
    /// sequence with it so rotations and drift directions stay distinguishable.
    edge_tokens: HashMap<EdgeKey, (u64, u64)>,
}

impl AnchoredGraph {
    /// Build from the connector index's edge list (one startup pass; no
    /// per-frame graph construction).
    #[must_use]
    pub fn from_connector_index(index: &V2ConnectorIndex) -> Self {
        let edges = index.path_edges();
        let mut edge_tokens = HashMap::with_capacity(edges.len());
        for &(token0_id, token1_id, pool_id, pool_kind) in &edges {
            edge_tokens.insert((pool_id, pool_kind), (token0_id, token1_id));
        }
        Self {
            graph: PathGraph::from_edges(edges),
            edge_tokens,
        }
    }

    /// Touched-set cycle enumeration: every touched pool anchors (both drift
    /// directions); open paths of 1..=`max_hops - 1` connectors close the
    /// cycle, and connector positions may hold OTHER touched pools. Directed
    /// cycles dedup across anchoring rotations; the `cap` strongest by
    /// touched-pool count (then edge sequence) are returned.
    ///
    /// Anchors are walked in two passes (each pool's `token_a` exit first,
    /// then its `token_b` exit), so a directed cycle reachable from some
    /// anchor's `token_a` rotation is first seen in a rotation the solver
    /// lane can consume.
    #[must_use]
    pub fn cycles_through_touched(
        &self,
        touched: &[AnchorPool],
        cap: usize,
        max_hops: usize,
    ) -> Vec<DfsCycle> {
        self.enumerate_cycles(touched, cap, max_hops, Some).0
    }

    /// The WETH-stake admission of the touched-set enumeration: every
    /// enumerated directed cycle is rotated so its first hop consumes
    /// `weth_token_id`. A cycle no rotation of which consumes WETH cannot be
    /// entered with the settlement asset and is refused
    /// ([`NON_WETH_CYCLE`]); the refusal is counted, not returned.
    ///
    /// Admission runs inside the enumeration loop, so a refused no-WETH cycle
    /// never consumes a `cap` slot ahead of an admissible one.
    #[must_use]
    pub fn weth_entry_cycles(
        &self,
        touched: &[AnchorPool],
        weth_token_id: u64,
        cap: usize,
        max_hops: usize,
    ) -> (Vec<DfsCycle>, usize) {
        self.enumerate_cycles(touched, cap, max_hops, |cycle| {
            self.rotate_to_entry(&cycle, weth_token_id)
        })
    }

    /// Rotate one directed cycle so its traversal starts at the hop that
    /// consumes `entry_token_id`, preserving hop order and closing back on the
    /// same token; `None` when no hop consumes it. The result is the same
    /// directed cycle with a different starting hop.
    #[must_use]
    pub fn rotate_to_entry(&self, cycle: &DfsCycle, entry_token_id: u64) -> Option<DfsCycle> {
        let n = cycle.pools.len();
        if n == 0 {
            return None;
        }
        let mut in_token = cycle.entry_token_id;
        let mut entries = Vec::with_capacity(n);
        for key in &cycle.pools {
            entries.push(in_token);
            let &(token0_id, token1_id) = self.edge_tokens.get(key)?;
            in_token = if token0_id == in_token {
                token1_id
            } else if token1_id == in_token {
                token0_id
            } else {
                return None;
            };
        }
        if in_token != cycle.entry_token_id {
            return None;
        }
        let position = entries.iter().position(|&token| token == entry_token_id)?;
        let pools = (0..n).map(|i| cycle.pools[(position + i) % n]).collect();
        Some(DfsCycle {
            pools,
            entry_token_id,
        })
    }

    /// The shared enumeration loop behind [`Self::cycles_through_touched`] and
    /// [`Self::weth_entry_cycles`]: `transform` maps each freshly
    /// deduplicated directed cycle to the rotation the caller admits, or
    /// `None` to refuse it. A refused cycle is counted so a caller's trace can
    /// name the refusal without enumerating it again.
    fn enumerate_cycles<F>(
        &self,
        touched: &[AnchorPool],
        cap: usize,
        max_hops: usize,
        transform: F,
    ) -> (Vec<DfsCycle>, usize)
    where
        F: Fn(DfsCycle) -> Option<DfsCycle>,
    {
        if cap == 0 || touched.is_empty() || max_hops < 2 {
            return (Vec::new(), 0);
        }
        let touched_keys: HashSet<EdgeKey> =
            touched.iter().map(|p| (p.pool_id, p.pool_kind)).collect();
        let mut seen: HashSet<Vec<(u64, u8, u64)>> = HashSet::new();
        let mut best: Vec<Candidate> = Vec::new();
        let mut refused = 0usize;
        'passes: for pass in 0..2u8 {
            for anchor in touched {
                let anchor_key = (anchor.pool_id, anchor.pool_kind);
                let (entry, exit) = if pass == 0 {
                    (anchor.token_a_id, anchor.token_b_id)
                } else {
                    (anchor.token_b_id, anchor.token_a_id)
                };
                // The 1 s progress report is the only channel that observes a
                // hub-rooted grind BEFORE its first yield: the caller loop is
                // parked inside `next_path()` until one arrives.
                let anchor_pool_id = anchor.pool_id;
                let pass_num = pass;
                let mut finder = self
                    .graph
                    .find_paths_iter(exit, entry, 1, Some(max_hops - 1), false, None, None)
                    .with_progress(Duration::from_secs(1), move |tally| {
                        tracing::info!(
                            anchor_pool = anchor_pool_id,
                            pass = pass_num,
                            entry_token = entry,
                            exit_token = exit,
                            elapsed_ms =
                                u64::try_from(tally.elapsed.as_millis()).unwrap_or(u64::MAX),
                            paths_yielded = tally.paths_yielded,
                            advances_since_yield = tally.advances_since_yield,
                            max_stack_depth = tally.max_stack_depth,
                            "long pathfinding walk"
                        );
                    });
                while let Some(open) = finder.next_path() {
                    let mut pools = Vec::with_capacity(open.len() + 1);
                    pools.push(anchor_key);
                    pools.extend(open);
                    if has_duplicate(&pools) {
                        continue;
                    }
                    let Some(key) = self.directed_key(&pools, entry) else {
                        continue;
                    };
                    if !seen.insert(key.clone()) {
                        continue;
                    }
                    let touched_count = pools.iter().filter(|k| touched_keys.contains(k)).count();
                    let Some(cycle) = transform(DfsCycle {
                        pools,
                        entry_token_id: entry,
                    }) else {
                        refused += 1;
                        continue;
                    };
                    insert_top(
                        &mut best,
                        Candidate {
                            cycle,
                            key,
                            touched_count,
                        },
                        cap,
                    );
                    if best.len() >= cap && best.iter().all(|c| c.touched_count >= touched.len()) {
                        break 'passes;
                    }
                }
            }
        }
        best.sort_by(rank);
        best.truncate(cap);
        (best.into_iter().map(|c| c.cycle).collect(), refused)
    }

    /// Canonical form of a directed cycle for dedup: the minimum rotation of
    /// the `(hop, consumed_token)` sequence. It is rotation invariant, so one
    /// directed cycle seen from any anchor's rotation shares a key, while the
    /// forward and reverse drift carry different keys.
    fn directed_key(&self, pools: &[EdgeKey], entry: u64) -> Option<Vec<(u64, u8, u64)>> {
        let mut in_token = entry;
        let mut seq = Vec::with_capacity(pools.len());
        for &key in pools {
            let &(token0_id, token1_id) = self.edge_tokens.get(&key)?;
            let out_token = if token0_id == in_token {
                token1_id
            } else if token1_id == in_token {
                token0_id
            } else {
                return None;
            };
            seq.push((key.0, key.1.as_u8(), in_token));
            in_token = out_token;
        }
        if in_token != entry {
            return None;
        }
        let n = seq.len();
        let mut best: Option<Vec<(u64, u8, u64)>> = None;
        for offset in 0..n {
            let rotation: Vec<(u64, u8, u64)> = (0..n).map(|i| seq[(offset + i) % n]).collect();
            if best.as_ref().is_none_or(|b| rotation < *b) {
                best = Some(rotation);
            }
        }
        best
    }
}

/// A ranked enumeration candidate: the cycle, its canonical dedup key, and
/// how many touched pools it carries.
struct Candidate {
    cycle: DfsCycle,
    key: Vec<(u64, u8, u64)>,
    touched_count: usize,
}

/// Cycles carrying more touched pools rank first; ties break on the canonical
/// edge sequence (ascending) so ranking is deterministic.
fn rank(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.touched_count
        .cmp(&a.touched_count)
        .then_with(|| a.key.cmp(&b.key))
}

/// Keep the top `cap` candidates, replacing the current worst when a stronger
/// one arrives.
fn insert_top(best: &mut Vec<Candidate>, candidate: Candidate, cap: usize) {
    if best.len() < cap {
        best.push(candidate);
        return;
    }
    let worst_idx = best
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| rank(a, b))
        .map(|(idx, _)| idx);
    if let Some(worst_idx) = worst_idx {
        if rank(&candidate, &best[worst_idx]) == std::cmp::Ordering::Less {
            best[worst_idx] = candidate;
        }
    }
}

/// Whether a pool appears more than once in a cycle.
fn has_duplicate(pools: &[EdgeKey]) -> bool {
    for (i, a) in pools.iter().enumerate() {
        for b in &pools[i + 1..] {
            if a == b {
                return true;
            }
        }
    }
    false
}

/// One walker cycle hop resolved to its connector-index edge (identity
/// only — admission happens against the frame's chain view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedHop {
    /// A V2 pair edge.
    V2(V2Edge),
    /// A V3 pool edge (fee in the 1e6 convention).
    V3(V3Edge),
}

impl ResolvedHop {
    /// The pool's on-chain address.
    #[must_use]
    pub const fn address(&self) -> &alloy::primitives::Address {
        match self {
            Self::V2(e) => &e.address,
            Self::V3(e) => &e.address,
        }
    }

    /// The pool's token0 DB id.
    #[must_use]
    pub const fn token0_id(&self) -> u64 {
        match self {
            Self::V2(e) => e.token0_id,
            Self::V3(e) => e.token0_id,
        }
    }

    /// The pool's token1 DB id.
    #[must_use]
    pub const fn token1_id(&self) -> u64 {
        match self {
            Self::V2(e) => e.token1_id,
            Self::V3(e) => e.token1_id,
        }
    }
}

/// A walker hop whose pool family has no connector-index lane (V4 today; any
/// future `PoolKind` variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedHop {
    /// The graph-level pool id carried by the walker edge.
    pub pool_id: u64,
    /// The family with no index lane.
    pub kind: PoolKind,
}

/// Resolve one walker hop to its index edge.
///
/// Returns `Ok(None)` when the family has a lane but the pool id is absent
/// from the index (a data gap), and `Err` when the family has no lane at all.
///
/// # Errors
///
/// [`UnsupportedHop`] when the hop's family has no connector-index lane:
/// `PoolKind` is `#[non_exhaustive]`, so a new family must surface as a typed
/// refusal the caller can count and trace, never a silent `None`.
pub fn resolve_hop(
    index: &V2ConnectorIndex,
    key: EdgeKey,
) -> Result<Option<ResolvedHop>, UnsupportedHop> {
    match key.1 {
        PoolKind::V2 => Ok(index.pool_edge(key.0).copied().map(ResolvedHop::V2)),
        PoolKind::V3 => Ok(index.v3_pool_edge(key.0).copied().map(ResolvedHop::V3)),
        _ => Err(UnsupportedHop {
            pool_id: key.0,
            kind: key.1,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn v2_edge(pool_id: u64, token0_id: u64, token1_id: u64) -> V2Edge {
        V2Edge {
            pool_id,
            token0_id,
            token1_id,
            address: Address::new([u8::try_from(pool_id % 254).unwrap_or(0); 20]),
        }
    }

    fn build_graph(edges: &[(u64, u64, u64)]) -> AnchoredGraph {
        let mut index = V2ConnectorIndex::default();
        for &(pool_id, token0_id, token1_id) in edges {
            index.push_edge(v2_edge(pool_id, token0_id, token1_id));
        }
        AnchoredGraph::from_connector_index(&index)
    }

    fn anchor_of(pool_id: u64, from_token: u64, to_token: u64) -> AnchorPool {
        AnchorPool {
            pool_id,
            pool_kind: PoolKind::V2,
            token_a_id: from_token,
            token_b_id: to_token,
        }
    }

    fn pool_ids(cycle: &DfsCycle) -> Vec<u64> {
        cycle.pools.iter().map(|(pool_id, _)| *pool_id).collect()
    }

    fn carries(cycle: &DfsCycle, ids: &[u64]) -> bool {
        let pools = pool_ids(cycle);
        ids.iter().all(|id| pools.contains(id))
    }

    #[test]
    fn touched_pool_mid_cycle_is_found_at_four_hops() {
        // P:1-2, Q:2-3, R:3-4, S:4-1. Q is touched but never anchors a
        // connector of P under the old two-connector cap; the only cycle it
        // rides is four hops long.
        let graph = build_graph(&[(101, 1, 2), (102, 2, 3), (103, 3, 4), (104, 4, 1)]);
        let cycles =
            graph.cycles_through_touched(&[anchor_of(101, 1, 2), anchor_of(102, 2, 3)], 8, 4);
        assert!(
            cycles
                .iter()
                .any(|c| pool_ids(c) == vec![101, 102, 103, 104]),
            "the pin-first 4-hop rotation must expose the touched mid pool: {cycles:?}"
        );
    }

    #[test]
    fn re_riding_the_touched_edge_is_rejected() {
        // Only P connects 1-2, so the sole open path back is P itself.
        let graph = build_graph(&[(101, 1, 2)]);
        let cycles = graph.cycles_through_touched(&[anchor_of(101, 1, 2)], 8, 4);
        assert!(
            cycles.is_empty(),
            "a cycle that re-rides the pin must be rejected: {cycles:?}"
        );
    }

    #[test]
    fn dual_touched_cycles_outrank_single_touched_under_cap() {
        // P:1-2 is touched; three parallel partners close single-touched
        // 2-cycles with it, and Q:2-3 + R:3-1 close a dual-touched cycle
        // through P and Q. Enumerated first, the singles must not fill the cap.
        let graph = build_graph(&[
            (101, 1, 2),
            (105, 1, 2),
            (106, 1, 2),
            (107, 1, 2),
            (102, 2, 3),
            (103, 3, 1),
        ]);
        let cycles =
            graph.cycles_through_touched(&[anchor_of(101, 1, 2), anchor_of(102, 2, 3)], 2, 4);
        assert_eq!(cycles.len(), 2, "the cap bounds the result: {cycles:?}");
        assert!(
            cycles.iter().all(|c| carries(c, &[101, 102])),
            "dual-touched cycles must outrank the singles under the cap: {cycles:?}"
        );
    }
}
