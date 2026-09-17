//! Anchored touched-set discovery over the shared pathfinding walker.
//!
//! The frame's touched pools anchor cycles into
//! [`degenbot_pathfinding`](::degenbot_pathfinding)'s DFS walker: for each
//! touched anchor pool `p` spanning tokens `(a, b)`, the walker enumerates
//! the open paths `b → a` (and `a → b`) up to two connector hops, and each
//! cycle is the anchor hop followed by one open path. Every returned cycle
//! therefore contains its touched anchor pool by construction, and the
//! depth cap stays at 3 hops (anchor + 2 connectors) regardless of graph
//! size. Both drift directions surface — the same two-cycles-per-pairing
//! shape the 2-hop star expresses, plus the 3-hop cycles the star can't
//! represent at all. A per-frame [`DiscoveryBudget`] arms the walker's
//! cancel flag between yields, so a hostile graph bounds one anchor's
//! enumeration without hanging the frame loop.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use degenbot_bot::sidecar_paths::{V2ConnectorIndex, V2Edge, V3Edge};
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

/// One walker cycle: pools in traversal order (anchor hop FIRST,
/// traversed `token_a → token_b`), plus the DB id of the token the first
/// pool consumes — the cycle's transit currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfsCycle {
    /// `(pool_id, PoolKind)` hops in traversal order, anchor first.
    pub pools: Vec<EdgeKey>,
    /// The token the anchor hop consumes (the cycle's transit currency).
    pub entry_token_id: u64,
}

/// The per-frame discovery slice: a wall-clock deadline + the walker's
/// cooperative cancel flag. The finding loop checks the clock between
/// yields (the only points it controls) and arms the flag; the walker
/// stops at its next internal loop iteration, so a hostile graph can
/// never out-run the slice by more than one yield's work.
#[derive(Debug)]
pub struct DiscoveryBudget {
    deadline: Instant,
    cancel: Arc<AtomicBool>,
}

impl DiscoveryBudget {
    /// A budget expiring `slice` from now.
    #[must_use]
    pub fn after(slice: Duration) -> Self {
        Self {
            deadline: Instant::now() + slice,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the slice has run out (checked between walker yields).
    #[must_use]
    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// The startup-built discovery graph: the connector index's loaded edge set
/// (V2 + V3; `PoolKind` carries the family) as a pathfinding multigraph,
/// built ONCE at startup — frames only traverse it.
#[derive(Clone)]
pub struct AnchoredGraph {
    graph: PathGraph,
}

impl AnchoredGraph {
    /// Build from the connector index's edge list (one startup pass; no
    /// per-frame graph construction).
    #[must_use]
    pub fn from_connector_index(index: &V2ConnectorIndex) -> Self {
        Self {
            graph: PathGraph::from_edges(index.path_edges()),
        }
    }

    /// Every cycle (≤ 3 hops) through `anchor`, anchor hop first, in BOTH
    /// drift directions (anchor traversed `token_a → token_b` and the
    /// reverse). Cycles stop at `cap` entries or when the budget expires;
    /// the anchor pool itself never re-appears as a connector.
    #[must_use]
    pub fn cycles_through_pool(
        &self,
        anchor: AnchorPool,
        budget: &DiscoveryBudget,
        cap: usize,
    ) -> Vec<DfsCycle> {
        let mut out: Vec<DfsCycle> = Vec::new();
        let mut seen: HashSet<(u64, Vec<u64>)> = HashSet::new();
        let anchor_key = (anchor.pool_id, anchor.pool_kind);
        for (entry, exit) in [
            (anchor.token_a_id, anchor.token_b_id),
            (anchor.token_b_id, anchor.token_a_id),
        ] {
            if budget.expired() || out.len() >= cap {
                break;
            }
            // Open paths exit → entry at depth 1..=2 close the cycle through
            // the anchor: [anchor, open] is always ≤ 3 hops.
            let mut finder = self
                .graph
                .find_paths_iter(exit, entry, 1, Some(2), false, None, None)
                .with_cancel(Arc::clone(&budget.cancel));
            // The budget's clock is checked between yields (the only points
            // this loop controls); expiring arms the flag so the walker's
            // next internal iteration reports exhaustion.
            while let Some(open) = finder.next_path() {
                if budget.expired() {
                    budget.cancel.store(true, Ordering::Relaxed);
                }
                if out.len() >= cap {
                    break;
                }
                // A 1-hop open path may itself ride the anchor's edge — the
                // anchor hop never re-runs as a connector.
                if open.contains(&anchor_key) {
                    continue;
                }
                let dedup = (entry, open.iter().map(|(pid, _)| *pid).collect::<Vec<_>>());
                if !seen.insert(dedup) {
                    continue;
                }
                let mut pools = Vec::with_capacity(open.len() + 1);
                pools.push(anchor_key);
                pools.extend(open);
                out.push(DfsCycle {
                    pools,
                    entry_token_id: entry,
                });
            }
        }
        out
    }
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

/// Resolve one walker hop to its index edge; V4 has no index lane and
/// resolves to `None`.
#[must_use]
pub fn resolve_hop(index: &V2ConnectorIndex, key: EdgeKey) -> Option<ResolvedHop> {
    match key.1 {
        PoolKind::V2 => index.pool_edge(key.0).copied().map(ResolvedHop::V2),
        PoolKind::V3 => index.v3_pool_edge(key.0).copied().map(ResolvedHop::V3),
        // `PoolKind` is `#[non_exhaustive]`: any future family has no index
        // lane and resolves to `None` like V4.
        _ => None,
    }
}
