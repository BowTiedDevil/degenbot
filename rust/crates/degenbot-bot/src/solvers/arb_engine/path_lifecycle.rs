//! Path solve-eligibility state machine (ergo R522XA).
//!
//! Owns the ONE question the arb engine's solve fan-out needs answered per
//! registered path: "given that a pool just went dirty, should this path be
//! (re)resolved now?"
//!
//! # Why a state machine
//!
//! Before R522XA the truth was scattered: `ResolvedMixedPath.valid` (a bool
//! whose failing hop was discarded), `rebuild_and_solve_affected`'s
//! reverse-index fan-out + invalid-reason histogram + solve-time `valid`
//! filter, and `solve_all_paths` re-resolving the whole registered set every
//! cold start. Each new "skip a path when X" rule added another ad-hoc
//! condition on top.
//!
//! The machine makes the transition explicit: a path is `Invalid` because of
//! a SET of responsible pools; a `pool_dirty(p)` event removes `p` from that
//! set; the path becomes eligible again exactly when the set EMPTIES. Pools
//! clear independently, in any order, across any time gap — no simultaneity,
//! no dependence on whichever hop the old code happened to hit first.
//!
//! # Structural failures are rejected, not stored
//!
//! `MissingHopReason::is_structurally_unroutable` (`TooFewTokens`,
//! `UnknownVariant`, `OutOfRange`) marks a misconfigured pool or path shape
//! that can never self-heal. `register_path` REJECTS those loudly at
//! construction — they never enter this machine as a state that could
//! silently evade detection.

use std::collections::HashSet;

use degenbot_solvers::mixed::HopType;

use crate::bot_core::resolve::HopDeficit;

/// The set of pools a path is invalid because of (family, key). A
/// `pool_dirty((ht, key))` clears its entry; the path un-blocks when the set
/// empties.
pub(crate) type ResponsibleSet = HashSet<(HopType, u64)>;

/// A registered path's solve-eligibility.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum PathSolveStatus {
    /// Registered (or re-activated) but not (re)resolved against current state.
    #[default]
    Unresolved,
    /// Every hop projects; eligible to solve.
    Solvable,
    /// One or more hops cannot project. `responsible` is the SET of pools
    /// whose current state failed projection.
    Invalid {
        /// Pools still responsible for the path being non-solvable.
        responsible: ResponsibleSet,
    },
}

impl PathSolveStatus {
    /// Record a resolve result (the FULL deficit set from `resolve_hops`).
    pub(crate) fn set_resolved(&mut self, deficits: &[HopDeficit]) {
        if deficits.is_empty() {
            *self = Self::Solvable;
        } else {
            *self = Self::Invalid {
                responsible: deficits.iter().map(|d| (d.hop_type, d.pool_key)).collect(),
            };
        }
    }

    /// The `pool_dirty(pool)` transition. Returns `true` iff the path must be
    /// (re)resolved NOW.
    ///
    /// - `Unresolved` / `Solvable`: any hop dirty can flip solvability → `true`.
    /// - `Invalid{responsible}`: `true` only when `pool` was a responsible
    ///   pool AND removing it empties the container (path may now be viable).
    ///   Unrelated dirty pools leave the path untouched.
    #[must_use]
    pub(crate) fn on_pool_dirty(&mut self, pool: (HopType, u64)) -> bool {
        match self {
            Self::Unresolved | Self::Solvable => true,
            Self::Invalid { responsible } => {
                if responsible.remove(&pool) {
                    responsible.is_empty()
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
#[expect(clippy::panic)]
mod tests {
    use super::*;
    use crate::bot_core::resolve::MissingHopReason;

    fn deficit(hop_type: HopType, pool_key: u64) -> HopDeficit {
        HopDeficit {
            hop_type,
            pool_key,
            reason: MissingHopReason::NotViable,
        }
    }

    #[test]
    fn two_faulty_pools_clear_independently_any_order() {
        let mut s = PathSolveStatus::default();
        s.set_resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        match &s {
            PathSolveStatus::Invalid { responsible } => {
                assert_eq!(responsible.len(), 2);
                assert!(responsible.contains(&(HopType::V3, 1)));
                assert!(responsible.contains(&(HopType::V3, 2)));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }

        // Clear B first: still Invalid (A remains), and NOT re-resolved.
        assert!(!s.on_pool_dirty((HopType::V3, 2)));
        match &s {
            PathSolveStatus::Invalid { responsible } => {
                assert!(!responsible.contains(&(HopType::V3, 2)));
                assert!(responsible.contains(&(HopType::V3, 1)));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }

        // Clear A last: container empties -> re-resolve, then a clean resolve.
        assert!(s.on_pool_dirty((HopType::V3, 1)));
        s.set_resolved(&[]);
        assert_eq!(s, PathSolveStatus::Solvable);
    }

    #[test]
    fn clear_order_is_independent() {
        let mut s = PathSolveStatus::default();
        s.set_resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert!(!s.on_pool_dirty((HopType::V3, 1)));
        assert!(s.on_pool_dirty((HopType::V3, 2)));
        s.set_resolved(&[]);
        assert_eq!(s, PathSolveStatus::Solvable);
    }

    #[test]
    fn unrelated_dirty_pool_does_not_clear_nor_resolve() {
        let mut s = PathSolveStatus::default();
        s.set_resolved(&[deficit(HopType::V3, 1)]);
        assert!(!s.on_pool_dirty((HopType::V3, 99)));
        match &s {
            PathSolveStatus::Invalid { responsible } => {
                assert!(responsible.contains(&(HopType::V3, 1)));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn pool_oscillating_in_and_out_of_validity_grows_and_shrinks_container() {
        fn responsible(s: &PathSolveStatus) -> Vec<(HopType, u64)> {
            match s {
                PathSolveStatus::Invalid { responsible } => {
                    let mut v: Vec<(HopType, u64)> = responsible.iter().copied().collect();
                    v.sort_by_key(|&(_, key)| key);
                    v
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }

        let mut s = PathSolveStatus::default();
        // A(1) bad, B(2) good at first.
        s.set_resolved(&[deficit(HopType::V3, 1)]);
        assert_eq!(responsible(&s), vec![(HopType::V3, 1)]);

        // B goes bad too, A still bad: container grows.
        s.set_resolved(&[deficit(HopType::V3, 1), deficit(HopType::V3, 2)]);
        assert_eq!(responsible(&s), vec![(HopType::V3, 1), (HopType::V3, 2)]);

        // A recovers while B still bad: container shrinks to {B}, no re-resolve.
        assert!(!s.on_pool_dirty((HopType::V3, 1)));
        assert_eq!(responsible(&s), vec![(HopType::V3, 2)]);

        // A goes bad AGAIN while B still bad: container regrows to {A, B}.
        let deficits = [deficit(HopType::V3, 1), deficit(HopType::V3, 2)];
        s.set_resolved(&deficits);
        assert_eq!(responsible(&s), vec![(HopType::V3, 1), (HopType::V3, 2)]);

        // Clear B then A: A last empties -> re-resolve -> Solvable.
        assert!(!s.on_pool_dirty((HopType::V3, 2)));
        assert!(s.on_pool_dirty((HopType::V3, 1)));
        s.set_resolved(&[]);
        assert_eq!(s, PathSolveStatus::Solvable);
    }

    #[test]
    fn solvable_and_unresolved_recheck_on_any_hop_dirty() {
        let mut s = PathSolveStatus::Solvable;
        assert!(s.on_pool_dirty((HopType::V2, 7)));
        assert!(s.on_pool_dirty((HopType::V4, 9)));

        let mut u = PathSolveStatus::Unresolved;
        assert!(u.on_pool_dirty((HopType::V2, 1)));
    }
}
