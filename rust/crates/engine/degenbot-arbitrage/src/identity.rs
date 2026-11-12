//! The dispatch identity contract: one [`CandidateId`], one live row.
//!
//! [`degenbot_execution::CandidateId`] is the post-target identity — one solver
//! path bound to the deployment that executes it — and it keys every
//! post-simulation structure: the retry-interval ledger, the order-index
//! ranking, the thin-margin attribution, the result map a batch joins, and the
//! submit row. None of those consumers can tell two rows carrying one identity
//! apart: a map keyed on the identity keeps one entry, the ledger holds one
//! retry interval, the ranking holds one position, and the submit lane holds
//! one nonce slot. The identity is therefore a ROW identity, not a label a set
//! of rows may share:
//!
//! > A candidate set carries AT MOST ONE row per [`CandidateId`].
//!
//! # Where the rule is enforced
//!
//! Every boundary that turns caller rows into execution refuses a repeated
//! identity BEFORE it produces a side effect, and it reports the identity it
//! refused ([`DispatchRefusal::DuplicateCandidateId`]):
//!
//! - the direct raw leaf ([`crate::dispatch_profitable_results`]) — before the
//!   census instrument and before any admission stage, so no suppression retry
//!   stamp and no divergence/fee-on-transfer tally is spent;
//! - the registry fan-out, for BOTH entries
//!   ([`crate::dispatch_across_deployments`] and
//!   [`crate::dispatch_admitted_across_deployments`]) — before the
//!   target-selection walk, so no adapter composes an encoding for a set that
//!   would be refused;
//! - the batch assembly (`degenbot_batch_executor::assembly`), for BOTH of its
//!   row kinds: the raw solver rows it would resolve and simulate (a raw row's
//!   identity is its path id bound to the assembly's deployment) and the payload
//!   rows it would join to a live submit row — refused before the raw checks,
//!   the path resolve, any policy-registry read or stamp, and any record.
//!
//! Refusing, never deduplicating: silently dropping the second row would leave
//! the caller believing it reported a candidate that never ran, and the input
//! is a wiring bug rather than data (the solver emits one result per solved
//! path; the engine emits one payload row per inline sim), so the refusal is
//! the loud, attributable outcome.
//!
//! # What is NOT a violation
//!
//! A caller may hold several rows of PROVENANCE for one candidate — an inline
//! sim's failure row beside the profitable retry of the same sim — because a
//! record is evidence ABOUT a candidate, not a slot in execution. Only the live
//! rows (a simulated candidate, a joined submit row) are identity-unique.

use std::collections::HashMap;

use degenbot_execution::{CandidateId, DeploymentId};

/// Scan `ids` for the first identity that repeats, in input order — the one
/// implementation of the [`CandidateId`] row contract, shared by every
/// boundary that enforces it.
///
/// Returns the repeated identity and how many rows carried it, or `None` when
/// every identity appears exactly once (the input a dispatch entry requires).
#[must_use]
pub fn duplicate_candidate_id<I>(ids: I) -> Option<(CandidateId, usize)>
where
    I: IntoIterator<Item = CandidateId>,
{
    let ids: Vec<CandidateId> = ids.into_iter().collect();
    let mut counts: HashMap<CandidateId, usize> = HashMap::with_capacity(ids.len());
    for id in &ids {
        *counts.entry(*id).or_insert(0) += 1;
    }
    ids.into_iter().find_map(|id| {
        counts
            .get(&id)
            .filter(|count| **count > 1)
            .map(|count| (id, *count))
    })
}

/// Why a dispatch boundary refused a candidate set.
///
/// Both variants are INPUT refusals: the entry validated the set before
/// touching a policy registry, an adapter, an engine, or the submit lane, so a
/// refusal leaves no partial side effect behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchRefusal {
    /// Two or more rows carried one [`CandidateId`] — the identity contract in
    /// this module's header, violated.
    DuplicateCandidateId {
        /// The identity that repeated.
        candidate_id: CandidateId,
        /// How many rows carried it.
        occurrences: usize,
    },
    /// A candidate named a deployment the session never registered: its
    /// [`CandidateId`] cannot be simulated (there is no adapter, target, or
    /// owner for it) and cannot be joined to a submit lane.
    UnregisteredDeployment {
        /// The offending candidate.
        candidate_id: CandidateId,
        /// The identities the registry actually holds.
        registered: Vec<DeploymentId>,
    },
}

impl std::fmt::Display for DispatchRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateCandidateId {
                candidate_id,
                occurrences,
            } => write!(
                f,
                "candidate {candidate_id} arrived on {occurrences} rows: one identity is one row"
            ),
            Self::UnregisteredDeployment {
                candidate_id,
                registered,
            } => {
                write!(
                    f,
                    "candidate {} targets deployment {}, which is not registered (registered: ",
                    candidate_id,
                    candidate_id.deployment_id()
                )?;
                for (index, id) in registered.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{id}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl std::error::Error for DispatchRefusal {}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path_id: u64, deployment: u64) -> CandidateId {
        CandidateId::new(path_id, DeploymentId::new(deployment))
    }

    /// The scan reports the FIRST repeated identity in input order, with the
    /// number of rows that carried it.
    #[test]
    fn the_first_repeated_identity_is_the_ones_reported() {
        let found = duplicate_candidate_id([
            candidate(3, 1),
            candidate(7, 1),
            candidate(3, 1),
            candidate(7, 1),
            candidate(7, 1),
        ]);
        assert_eq!(found, Some((candidate(3, 1), 2)));
    }

    /// One row per identity — including one path at two deployments — is the
    /// legal input: `None`.
    #[test]
    fn one_row_per_identity_is_legal() {
        assert_eq!(
            duplicate_candidate_id([candidate(7, 1), candidate(7, 2), candidate(8, 1)]),
            None
        );
        assert_eq!(duplicate_candidate_id([]), None);
    }

    /// The refusal names the identity and the row count, and reads as one
    /// sentence an operator can act on.
    #[test]
    fn the_refusal_names_the_identity_and_the_row_count() {
        let refusal = DispatchRefusal::DuplicateCandidateId {
            candidate_id: candidate(7, 2),
            occurrences: 3,
        };
        assert_eq!(
            refusal.to_string(),
            "candidate 7@2 arrived on 3 rows: one identity is one row"
        );
    }
}
