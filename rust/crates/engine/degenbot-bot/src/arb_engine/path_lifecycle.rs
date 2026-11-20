//! Path solve-eligibility state machine.
//!
//! Owns the ONE question the arb engine's solve fan-out asks per registered
//! path: "given that a pool just went dirty, should this path be (re)resolved
//! now?" [`PathSolveStatus`] is that answer's only home: the solve dispatch
//! stages a path iff its row is `Solvable`, and only the dirty scan reads a
//! row's blame set.
//!
//! # Why a state machine
//!
//! The engine must not re-resolve a six-figure path set because one pool moved.
//! A path is `Invalid` because of a SET of responsible pools; `pool_dirty(p)`
//! clears `p`'s responsibility; the path wants a fresh resolve exactly when
//! nothing is left to blame. Pools clear independently, in any order, across any
//! time gap — no simultaneity, no dependence on whichever hop an ad-hoc skip
//! rule happened to inspect first.
//!
//! # Two invariants the types carry
//!
//! * **`Invalid` always names a pool.** [`Responsible`] holds at least one pool
//!   by construction, so "the last responsible pool cleared" is a TRANSITION
//!   into `Unresolved`, never a row a later cycle can observe. An empty blame
//!   set is unreadable to the dirty scan — nothing can clear it further, so the
//!   path would sit at `Invalid` forever, never staged and never re-resolved.
//!   `Unresolved` is the honest state for "a resolve is owed", and its dirty
//!   verdict is `NeedsResolve`, so a resolve the cycle deferred (future-price
//!   deferral) or never ran (shed draw) is retried by the next dirty hop or
//!   re-recorded key.
//! * **One eligibility verdict.** The row is written only by [`resolved`],
//!   from the deficit list `resolve_hops` returns, and read only through
//!   [`PathSolveStatus::is_solvable`] (staging) and
//!   [`PathSolveStatus::on_pool_dirty`] (the scan). The solve dispatch asks the
//!   ROW whether to stage a path; the resolved snapshot is the payload it
//!   stages. `ResolvedMixedPath::valid` is the substrate's
//!   own projection flag for the solver (which reads it before any engine code
//!   sees the snapshot) and is derived from that same deficit list, so the two
//!   cannot disagree. The per-hop update-block snapshot stays outside the row:
//!   it records WHICH intake the last resolve saw (the same-state memo and the
//!   merge's freshness stamps), not whether the path may solve.
//!
//! [`resolved`]: PathSolveStatus::resolved
//!
//! # Structural failures are rejected, not stored
//!
//! `MissingHopReason::is_structurally_unroutable` (`TooFewTokens`,
//! `UnknownVariant`, `OutOfRange`) marks a misconfigured pool or path shape that
//! can never self-heal. `register_path` REJECTS those loudly at construction —
//! they never enter this machine as a state that could silently evade
//! detection.

use degenbot_pools::solve::HopType;
use degenbot_substrate::resolve::HopDeficit;
use hashbrown::HashSet;

/// The pools a path is `PathSolveStatus::Invalid` because of (family, key).
///
/// NON-EMPTY BY CONSTRUCTION: every door hands back `Option` or a residual
/// ([`Cleared`]), so the empty container is unrepresentable outside this type's
/// own transitions. "Nothing is responsible any more" is not a blame set — it is
/// `PathSolveStatus::Unresolved`, the state that owes a resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Responsible(HashSet<(HopType, u64)>);

impl Responsible {
    /// The blame set of a resolve's full deficit list, or `None` when nothing
    /// failed projection (the path is `Solvable`).
    fn from_deficits(deficits: &[HopDeficit]) -> Option<Self> {
        let pools: HashSet<(HopType, u64)> =
            deficits.iter().map(|d| (d.hop_type, d.pool_key)).collect();
        (!pools.is_empty()).then_some(Self(pools))
    }

    /// Clear one pool's responsibility, consuming the set: the residual is
    /// handed back inside [`Cleared`], so an empty container never escapes.
    fn cleared(self, pool: (HopType, u64)) -> Cleared {
        let mut pools = self.0;
        if !pools.remove(&pool) {
            return Cleared::Unrelated(Self(pools));
        }
        if pools.is_empty() {
            Cleared::Last
        } else {
            Cleared::Remaining(Self(pools))
        }
    }
}

#[cfg(test)]
impl Responsible {
    /// The pools still responsible, for the tests that pin the container's
    /// growth and shrinkage.
    pub(crate) fn pools(&self) -> impl Iterator<Item = (HopType, u64)> + '_ {
        self.0.iter().copied()
    }
}

/// What clearing one responsible pool leaves behind.
enum Cleared {
    /// `pool` was not responsible: the blame set is unchanged.
    Unrelated(Responsible),
    /// The pool cleared; others are still responsible.
    Remaining(Responsible),
    /// The pool cleared and nothing is left to blame, so the row owes a
    /// resolve.
    Last,
}

/// The dirty scan's ONE question per touched path, answered by the row's
/// transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirtyVerdict {
    /// The path must be (re)resolved now.
    NeedsResolve,
    /// No resolve: other pools remain responsible, or this pool was never one.
    Skip,
}

/// A registered path's solve-eligibility — the ONE authority the solve cycle
/// consults before staging a path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum PathSolveStatus {
    /// Registered (or re-activated) but not (re)resolved against current state:
    /// the row owes a resolve, and any dirty hop asks for it.
    #[default]
    Unresolved,
    /// Every hop projects at the last resolve; eligible to solve.
    Solvable,
    /// One or more hops could not project at the last resolve.
    Invalid {
        /// The pools whose current state failed projection — never empty.
        responsible: Responsible,
    },
}

impl PathSolveStatus {
    /// Record a resolve result (the FULL deficit set from `resolve_hops`). This
    /// is the only writer that can produce `Invalid`, so the blame set is never
    /// empty and the row can never claim a solve the resolve did not support.
    #[must_use]
    pub(crate) fn resolved(deficits: &[HopDeficit]) -> Self {
        match Responsible::from_deficits(deficits) {
            Some(responsible) => Self::Invalid { responsible },
            None => Self::Solvable,
        }
    }

    /// Whether the solve dispatch may stage this path.
    #[must_use]
    pub(crate) const fn is_solvable(&self) -> bool {
        matches!(self, Self::Solvable)
    }

    /// The `pool_dirty(pool)` transition, in place, with its fan-out verdict.
    ///
    /// - `Unresolved` / `Solvable`: any hop dirty can flip solvability →
    ///   `NeedsResolve`, and the row returns to `Unresolved` because the dirty
    ///   hop invalidated the verdict it held.
    /// - `Invalid{responsible}`: `NeedsResolve` only when `pool` was a
    ///   responsible pool AND removing it empties the set. Unrelated dirty
    ///   pools, and co-responsible pools that leave others to blame, leave the
    ///   path untouched — no re-resolve churn.
    ///
    /// A `NeedsResolve` verdict always leaves the row `Unresolved`, so a cycle
    /// that sheds the draw or defers the path cannot lose the owed resolve: the
    /// next dirty hop (or the ledger's re-recorded key) asks again.
    pub(crate) fn on_pool_dirty(&mut self, pool: (HopType, u64)) -> DirtyVerdict {
        let (next, verdict) = std::mem::take(self).cleared_of(pool);
        *self = next;
        verdict
    }

    /// The transition's pure core: the row it lands on and the verdict.
    fn cleared_of(self, pool: (HopType, u64)) -> (Self, DirtyVerdict) {
        match self {
            Self::Unresolved | Self::Solvable => (Self::Unresolved, DirtyVerdict::NeedsResolve),
            Self::Invalid { responsible } => match responsible.cleared(pool) {
                Cleared::Last => (Self::Unresolved, DirtyVerdict::NeedsResolve),
                Cleared::Unrelated(rest) | Cleared::Remaining(rest) => {
                    (Self::Invalid { responsible: rest }, DirtyVerdict::Skip)
                }
            },
        }
    }
}

#[cfg(test)]
#[expect(clippy::panic)]
mod tests {
    use super::*;
    use degenbot_substrate::resolve::MissingHopReason;

    fn deficit(hop_type: HopType, pool_key: u64) -> HopDeficit {
        HopDeficit {
            hop_type,
            pool_key,
            reason: MissingHopReason::NotViable,
        }
    }

    /// Drive one dirty hop through the row, as the dirty scan does.
    fn dirty(row: &mut PathSolveStatus, pool: (HopType, u64)) -> bool {
        row.on_pool_dirty(pool) == DirtyVerdict::NeedsResolve
    }

    /// The pools a row blames, sorted by pool key.
    fn blamed(row: &PathSolveStatus) -> Vec<(HopType, u64)> {
        match row {
            PathSolveStatus::Invalid { responsible } => {
                let mut pools: Vec<(HopType, u64)> = responsible.pools().collect();
                pools.sort_by_key(|&(_, key)| key);
                pools
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn resolve_outcome_maps_to_the_row() {
        assert_eq!(PathSolveStatus::resolved(&[]), PathSolveStatus::Solvable);
        assert!(PathSolveStatus::resolved(&[]).is_solvable());
        let single = PathSolveStatus::resolved(&[deficit(HopType::V3, 1)]);
        assert_eq!(blamed(&single), vec![(HopType::V3, 1)]);
        assert!(!single.is_solvable());
        // A repeated (family, key) is ONE responsible pool, not two.
        let repeated =
            PathSolveStatus::resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 1)]);
        assert_eq!(blamed(&repeated), vec![(HopType::V3, 1)]);
    }

    #[test]
    fn two_faulty_pools_clear_independently_any_order() {
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert_eq!(blamed(&s), vec![(HopType::V3, 1), (HopType::V3, 2)]);

        // Clear B first: still Invalid (A remains), and NOT re-resolved.
        assert!(!dirty(&mut s, (HopType::V3, 2)));
        assert_eq!(blamed(&s), vec![(HopType::V3, 1)]);

        // Clear A last: the blame set empties -> re-resolve, then a clean
        // resolve.
        assert!(dirty(&mut s, (HopType::V3, 1)));
        assert_eq!(s, PathSolveStatus::Unresolved);
        assert_eq!(PathSolveStatus::resolved(&[]), PathSolveStatus::Solvable);
    }

    #[test]
    fn clear_order_is_independent() {
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert!(!dirty(&mut s, (HopType::V3, 1)));
        assert!(dirty(&mut s, (HopType::V3, 2)));
        assert_eq!(s, PathSolveStatus::Unresolved);
    }

    #[test]
    fn unrelated_dirty_pool_leaves_the_blame_set_untouched() {
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1)]);
        assert!(!dirty(&mut s, (HopType::V3, 99)));
        assert_eq!(blamed(&s), vec![(HopType::V3, 1)]);
    }

    #[test]
    fn pool_oscillating_in_and_out_of_validity_grows_and_shrinks_the_blame_set() {
        // A(1) bad, B(2) good at first.
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1)]);
        assert_eq!(blamed(&s), vec![(HopType::V3, 1)]);
        // B goes bad too, A still bad: the blame set grows.
        s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert_eq!(blamed(&s), vec![(HopType::V3, 1), (HopType::V3, 2)]);
        // A recovers while B still bad: the set shrinks to {B}, no re-resolve.
        assert!(!dirty(&mut s, (HopType::V3, 1)));
        assert_eq!(blamed(&s), vec![(HopType::V3, 2)]);
        // A goes bad AGAIN while B still bad: the set regrows to {A, B}.
        s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert_eq!(blamed(&s), vec![(HopType::V3, 1), (HopType::V3, 2)]);
        // Clear B then A: A last empties the set -> re-resolve -> Solvable.
        assert!(!dirty(&mut s, (HopType::V3, 2)));
        assert!(dirty(&mut s, (HopType::V3, 1)));
        s = PathSolveStatus::resolved(&[]);
        assert_eq!(s, PathSolveStatus::Solvable);
    }

    /// The last responsible pool's dirt empties the blame set: the path wants a
    /// resolve NOW. A shed cycle draws no keys, so it never runs that resolve —
    /// the retained key comes back on the next cycle's draw, and the scan must
    /// ask for the resolve again rather than find a row it can no longer
    /// reactivate.
    #[test]
    fn shed_between_the_last_responsible_dirty_and_its_resolve_keeps_the_path_retryable() {
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1)]);
        assert!(
            dirty(&mut s, (HopType::V3, 1)),
            "clearing the last responsible pool asks for a resolve"
        );
        assert_eq!(s, PathSolveStatus::Unresolved);
        // The shed cycle ran no resolve, so the row still owes one.
        assert!(
            dirty(&mut s, (HopType::V3, 1)),
            "the carried-over key must ask for the owed resolve"
        );
    }

    /// Same shape, other non-resolving cycle: the future-price gate defers the
    /// path after the scan activated it, and the deferral re-records the hop
    /// keys for the next draw.
    #[test]
    fn deferral_between_the_last_responsible_dirty_and_its_resolve_keeps_the_path_retryable() {
        let mut s = PathSolveStatus::resolved(&[deficit(HopType::V3, 1)]);
        assert!(
            dirty(&mut s, (HopType::V3, 1)),
            "clearing the last responsible pool asks for a resolve"
        );
        // The deferred cycle ran no resolve; the re-recorded key returns.
        assert!(
            dirty(&mut s, (HopType::V3, 1)),
            "the re-recorded key must ask for the deferred resolve"
        );
        assert!(
            dirty(&mut s, (HopType::V2, 7)),
            "any hop of a path owing a resolve reactivates it"
        );
    }

    fn push_unique(rows: &mut Vec<PathSolveStatus>, row: PathSolveStatus) {
        if !rows.contains(&row) {
            rows.push(row);
        }
    }

    /// Whether SOME sequence of dirty hops asks for a resolve from `row` (a
    /// blame set drains pool by pool, so a reachable row is never absorbing).
    fn some_dirty_sequence_asks_for_a_resolve(
        row: &PathSolveStatus,
        pools: &[(HopType, u64)],
    ) -> bool {
        let mut seen = vec![row.clone()];
        let mut cursor = 0;
        while cursor < seen.len() {
            let current = seen[cursor].clone();
            cursor += 1;
            for &pool in pools {
                let mut next = current.clone();
                if next.on_pool_dirty(pool) == DirtyVerdict::NeedsResolve {
                    return true;
                }
                if !seen.contains(&next) {
                    seen.push(next);
                }
            }
        }
        false
    }

    /// The transition graph's invariants: an `Invalid` row always names a
    /// responsible pool, and no row is absorbing — some hop's dirt always asks
    /// for a resolve. Empty blame violates both, and a `NeedsResolve` verdict
    /// always leaves the row that owes the resolve.
    #[test]
    fn reachable_rows_keep_a_blame_set_and_stay_reactivatable() {
        const POOLS: [(HopType, u64); 3] = [(HopType::V2, 1), (HopType::V3, 2), (HopType::V4, 3)];
        let outcomes: [&[HopDeficit]; 4] = [
            &[],
            &[deficit(POOLS[0].0, POOLS[0].1)],
            &[deficit(POOLS[1].0, POOLS[1].1)],
            &[
                deficit(POOLS[0].0, POOLS[0].1),
                deficit(POOLS[1].0, POOLS[1].1),
            ],
        ];
        let mut reachable = vec![PathSolveStatus::default()];
        let mut cursor = 0;
        while cursor < reachable.len() {
            let row = reachable[cursor].clone();
            cursor += 1;
            for outcome in outcomes {
                push_unique(&mut reachable, PathSolveStatus::resolved(outcome));
            }
            for &pool in &POOLS {
                let mut next = row.clone();
                let verdict = next.on_pool_dirty(pool);
                if verdict == DirtyVerdict::NeedsResolve {
                    assert_eq!(
                        next,
                        PathSolveStatus::Unresolved,
                        "a resolve is owed, so the row may not claim a verdict"
                    );
                }
                push_unique(&mut reachable, next);
            }
        }
        for row in &reachable {
            if let PathSolveStatus::Invalid { responsible } = row {
                assert!(
                    responsible.pools().next().is_some(),
                    "an Invalid row must name a responsible pool: {row:?}"
                );
            }
            assert!(
                some_dirty_sequence_asks_for_a_resolve(row, &POOLS),
                "no row may be absorbing — a dirty hop must be able to ask for a resolve: {row:?}"
            );
        }
    }

    #[test]
    fn solvable_and_unresolved_recheck_on_any_hop_dirty() {
        let mut s = PathSolveStatus::Solvable;
        assert!(dirty(&mut s, (HopType::V2, 7)));
        assert_eq!(s, PathSolveStatus::Unresolved);
        assert!(dirty(&mut s, (HopType::V4, 9)));
        let mut u = PathSolveStatus::Unresolved;
        assert!(dirty(&mut u, (HopType::V2, 1)));
    }
}
