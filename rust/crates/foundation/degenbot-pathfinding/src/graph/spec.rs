//! Caller-supplied search parameters ([`SearchSpec`]) for the lazy DFS.
//!
//! Extracted from the former seven-argument public constructors
//! (`PathGraph::find_paths_iter`, `PathGraph::find_paths`,
//! `OwnedPathFinder::new`): the boundary + depth knobs travel as one value,
//! and the per-depth-filter floor/cap has exactly one enforcement point
//! ([`SearchSpec::validate`]).

use super::expect_u32;
use crate::graph::PoolKind;

/// The widest hop bound a search can carry: a depth bound is a hop count in
/// `0..=MAX_HOP_DEPTH`, and `None` (unlimited) is the only other
/// representation — no in-range value is a sentinel for "unlimited".
///
/// The bound is a `u32` because that is the width the walk's remaining-budget
/// arithmetic works in, and the width every compact index in the graph is
/// bounded by. A caller holding a wider number rejects an out-of-range value at
/// its own boundary; nothing inside a search reinterprets one.
pub const MAX_HOP_DEPTH: u32 = u32::MAX;

/// All caller-supplied parameters of one path search — the value that
/// replaces the puts of the former seven-argument constructors
/// ([`PathGraph::find_paths_iter`](crate::graph::PathGraph::find_paths_iter),
/// [`PathGraph::find_paths`](crate::graph::PathGraph::find_paths), and
/// [`OwnedPathFinder::new`](crate::graph::OwnedPathFinder)).
#[derive(Clone, Debug)]
pub struct SearchSpec {
    /// External token ID where the search begins.
    pub start: u64,
    /// External token ID the path must return to. An ID absent from the
    /// graph admits no path — the search starts exhausted (never a
    /// synthetic fallback index).
    pub end: u64,
    /// Minimum number of hops in a completed path.
    pub min_depth: usize,
    /// Maximum number of hops in `0..=MAX_HOP_DEPTH`, or `None` for no
    /// limit (the only unlimited representation). Zero admits no completed
    /// path: only an empty walk can satisfy it, at `min_depth == 0`.
    pub max_depth: Option<u32>,
    /// If `true`, each found path is yielded again in reverse.
    pub include_reverse: bool,
    /// Optional per-depth allowed pool kinds. A `None` entry allows all
    /// kinds at that depth. A filter of length N also caps the effective
    /// maximum depth at N and floors the effective minimum depth at N
    /// (all-`None` filters included — see [`Self::validate`]).
    pub pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
}

impl SearchSpec {
    /// Assemble a spec from the caller's raw parameters. The depth-bound
    /// normalization happens later, in [`Self::validate`], when the search
    /// consumes the spec.
    #[must_use]
    pub fn new(
        start: u64,
        end: u64,
        min_depth: usize,
        max_depth: Option<u32>,
        include_reverse: bool,
        pool_type_per_depth: Option<Vec<Option<Vec<PoolKind>>>>,
    ) -> Self {
        Self {
            start,
            end,
            min_depth,
            max_depth,
            include_reverse,
            pool_type_per_depth,
        }
    }

    /// Normalize the depth bounds against a present per-depth filter — the
    /// single enforcement point for the filter floor/cap (migrated verbatim
    /// from the former `BundledSearch::with_params`):
    ///
    /// - a per-depth filter of length N pins an exactly-N-hop permutation
    ///   regardless of kind constraints (all-`None` filters included), so
    ///   the effective maximum depth is capped at N (pinned there when the
    ///   caller passed `None`). A filter longer than [`MAX_HOP_DEPTH`] would
    ///   describe a path no graph can hold, so that length converts through
    ///   the graph's architectural-bound helper;
    /// - the effective minimum depth is floored at N. Without the floor,
    ///   the yield condition only checks `>= min_depth`, so a walk shorter
    ///   than the filter that prefix-matches its early depths leaks
    ///   through. plan.rs floors the *reported* effective minimum for its
    ///   callers; re-applying here is an idempotent `max()`.
    ///
    /// # Filter immutability (`node_valid_depths` table invariant)
    ///
    /// `validate` adjusts ONLY `min_depth` / `max_depth` and NEVER mutates
    /// `pool_type_per_depth`: the `node_valid_depths` lookahead table is
    /// computed from the RAW filter and is indexed by filter depth — its
    /// length must stay exactly `pool_type_per_depth.len()` (the walk
    /// looks up `node_valid_depths[node][next_depth]` against the same
    /// filter depths). Mutating the filter here would silently
    /// desynchronize that table.
    pub fn validate(&mut self) {
        if let Some(filter) = &self.pool_type_per_depth {
            let filter_len = filter.len();
            let filter_hops = expect_u32(filter_len, "filter length exceeds u32::MAX");
            self.max_depth = Some(match self.max_depth {
                Some(md) => md.min(filter_hops),
                None => filter_hops,
            });
            self.min_depth = self.min_depth.max(filter_len);
        }
    }
}
