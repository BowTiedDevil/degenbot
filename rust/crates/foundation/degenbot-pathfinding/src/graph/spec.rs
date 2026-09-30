//! Caller-supplied search parameters ([`SearchSpec`]) for the lazy DFS.
//!
//! Extracted from the former seven-argument public constructors
//! (`PathGraph::find_paths_iter`, `PathGraph::find_paths`,
//! `OwnedPathFinder::new`): the boundary + depth knobs travel as one value,
//! and the per-depth-filter floor/cap has exactly one enforcement point
//! ([`SearchSpec::validate`]).

use crate::graph::PoolKind;

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
    /// Maximum number of hops, or `None` for no limit.
    pub max_depth: Option<usize>,
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
        max_depth: Option<usize>,
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
    ///   caller passed `None`);
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
            self.max_depth = Some(match self.max_depth {
                Some(md) => md.min(filter_len),
                None => filter_len,
            });
            self.min_depth = self.min_depth.max(filter_len);
        }
    }
}
