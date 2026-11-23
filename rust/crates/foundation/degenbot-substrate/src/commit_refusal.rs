//! One freshness refusal for projection commits (ADR-105 D9).
//!
//! Two fences guard the same event. Live work reduced under a rewound Epoch must
//! not publish, and a projection commit whose expected parent storage no longer
//! holds must not publish either: both describe work about a chain view that no
//! longer exists. They share one refusal class, so a caller answers one outcome
//! and a stale live-work rejection and an unexpected-parent persistence rejection
//! cannot drift into different handling.

use degenbot_db::{ProjectionCursor, ProjectionError};

use crate::epoch::{Epoch, StaleEpoch};

/// Why a projection commit cannot publish: the work's chain view was rewound
/// (live side) or storage holds another parent (persistence side).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitRefusal {
    /// The work was reduced under an epoch the session has rewound past.
    #[error(transparent)]
    StaleEpoch(#[from] StaleEpoch),
    /// The database holds another parent than the commit expects.
    #[error("market {market_id} projection commit expects a parent storage does not hold")]
    UnexpectedParent {
        /// The market the commit belongs to.
        market_id: i64,
        /// The cursor the commit states.
        expected: Box<ProjectionCursor>,
        /// The cursor storage holds.
        held: Box<ProjectionCursor>,
    },
}

impl CommitRefusal {
    /// The live-side fence: a work item minted in an epoch the session has
    /// rewound past cannot commit.
    ///
    /// # Errors
    ///
    /// [`CommitRefusal::StaleEpoch`] when the held epoch is no longer current.
    pub fn require_current(held: Epoch, current: Epoch) -> Result<Epoch, Self> {
        held.ensure_current(current).map_err(Self::from)
    }

    /// The persistence face of the same refusal: an expected-parent mismatch maps
    /// into this class, so both fences report one outcome. Any other projection
    /// refusal is not a freshness failure and stays with its own caller.
    #[must_use]
    pub fn from_persistence(error: &ProjectionError) -> Option<Self> {
        match error {
            ProjectionError::UnexpectedParent {
                market_id,
                expected,
                held,
            } => Some(Self::UnexpectedParent {
                market_id: *market_id,
                expected: expected.clone(),
                held: held.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use alloy::primitives::B256;
    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_db::LogCoordinate;

    use super::{CommitRefusal, Epoch, ProjectionCursor, ProjectionError};

    fn cursor(journal_sequence: i64) -> ProjectionCursor {
        let anchor =
            ChainAnchor::new(1, 500, B256::repeat_byte(0xaa), B256::repeat_byte(0x99)).unwrap();
        ProjectionCursor {
            market_id: 1,
            position: LogCoordinate::new(
                anchor,
                B256::repeat_byte(0xbb),
                0,
                u64::try_from(journal_sequence).unwrap(),
            ),
            journal_sequence,
            range_end_block: 500,
        }
    }

    #[test]
    fn a_stale_epoch_commit_is_refused() {
        let current = Epoch::at(500).rewind();
        let held = Epoch::at(500);
        assert!(matches!(
            CommitRefusal::require_current(held, current),
            Err(CommitRefusal::StaleEpoch(_))
        ));
    }

    #[test]
    fn a_current_epoch_commit_passes() {
        assert_eq!(
            CommitRefusal::require_current(Epoch::at(500), Epoch::at(500)),
            Ok(Epoch::at(500))
        );
    }

    #[test]
    fn an_unexpected_parent_commit_is_the_same_refusal_class() {
        let error = ProjectionError::UnexpectedParent {
            market_id: 1,
            expected: Box::new(cursor(2)),
            held: Box::new(cursor(3)),
        };
        let refusal = CommitRefusal::from_persistence(&error)
            .expect("the parent mismatch is a freshness refusal");
        assert!(matches!(refusal, CommitRefusal::UnexpectedParent { .. }));
        assert!(refusal.to_string().contains("market 1"));
    }

    #[test]
    fn a_non_freshness_projection_error_is_not_a_commit_refusal() {
        assert!(CommitRefusal::from_persistence(&ProjectionError::MissingMarket(9)).is_none());
    }

    #[test]
    fn the_two_fences_report_distinct_causes_of_one_outcome() {
        let stale =
            CommitRefusal::require_current(Epoch::at(500), Epoch::at(500).rewind()).unwrap_err();
        let parent = CommitRefusal::from_persistence(&ProjectionError::UnexpectedParent {
            market_id: 1,
            expected: Box::new(cursor(2)),
            held: Box::new(cursor(3)),
        })
        .unwrap();
        assert_ne!(stale, parent);
        assert!(stale.to_string().contains("stale epoch"));
        assert!(parent.to_string().contains("expects a parent"));
    }
}
