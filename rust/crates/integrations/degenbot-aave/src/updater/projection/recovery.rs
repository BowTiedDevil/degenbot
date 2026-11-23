//! Bounded checkpoint replay and reorg recovery (ADR-105 D9).
//!
//! A head mismatch is not an ordinary forward advance: the market's committed
//! cursor names a position the new branch no longer contains. Recovery restores
//! the newest retained checkpoint at or before the deepest retained position the
//! new head's ancestry still holds (the common ancestor) and applies the retained
//! journal deltas forward to that ancestor, so the new branch's deltas continue
//! from a position both branches share.
//!
//! Three properties make this forward-only and RPC-free once evidence exists:
//!
//! - The ancestry evidence is a validated [`CanonicalSegment`] the caller already
//!   acquired; recovery reads no chain.
//! - The restore state is the checkpoint's own payload — no market row the
//!   abandoned branch wrote is consulted for it.
//! - Replay applies journaled deltas, never inverse arithmetic: the journal is the
//!   forward evidence, and its rows are decoded with the row's own identity.
//!
//! Retention is the session's reorg window. A reorg deeper than the retained
//! evidence is an explicit [`ProjectionError::RetentionExceeded`] refusal naming
//! the retained span and the deepest retained checkpoint, and it moves nothing.

use degenbot_core::canonical_head::ChainAnchor;
use degenbot_db::{
    read_retained_checkpoints_on_conn, read_retained_journal_on_conn, CheckpointBoundary,
    DeltaJournalRow, LogCoordinate, ProjectionCursor,
};
use rusqlite::Connection;

use super::checkpoint::log_ref;
use super::{
    CanonicalDelta, CheckpointState, JournaledDelta, ProjectionError, ProjectionView,
    CHECKPOINT_ENCODING_VERSION,
};

/// The new head's ancestry, as the caller acquired it: one ascending, contiguous,
/// linked chain of exact anchors covering the retained window through the new
/// head.
///
/// A retained position is on the new head's ancestry exactly when this segment
/// holds its block, so the segment is the only chain evidence a recovery needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSegment {
    anchors: Vec<ChainAnchor>,
    lowest: ChainAnchor,
    tip: ChainAnchor,
}

impl CanonicalSegment {
    /// Validate an ascending chain segment: one chain, contiguous heights, and
    /// every anchor building on the one before it.
    ///
    /// # Errors
    ///
    /// [`ProjectionError::SegmentEmpty`] for no anchors,
    /// [`ProjectionError::SegmentChainMismatch`] when the anchors mix chains, and
    /// [`ProjectionError::SegmentUnordered`] when a height repeats, skips, or names
    /// a parent the previous anchor does not hold.
    pub fn new(anchors: Vec<ChainAnchor>) -> Result<Self, ProjectionError> {
        let Some(first) = anchors.first().copied() else {
            return Err(ProjectionError::SegmentEmpty);
        };
        if let Some(other) = anchors
            .iter()
            .find(|anchor| anchor.chain_id() != first.chain_id())
        {
            return Err(ProjectionError::SegmentChainMismatch {
                chain_id: first.chain_id(),
                other: other.chain_id(),
            });
        }
        for pair in anchors.windows(2) {
            let (previous, next) = (pair[0], pair[1]);
            if next.number() != previous.number() + 1 || next.parent_hash() != previous.hash() {
                return Err(ProjectionError::SegmentUnordered {
                    number: next.number(),
                });
            }
        }
        let tip = anchors.last().copied().unwrap_or(first);
        Ok(Self {
            anchors,
            lowest: first,
            tip,
        })
    }

    /// The lowest block the segment covers.
    #[must_use]
    pub const fn lowest(&self) -> ChainAnchor {
        self.lowest
    }

    /// The head the segment ends at.
    #[must_use]
    pub const fn tip(&self) -> ChainAnchor {
        self.tip
    }

    /// The segment's anchors, lowest first.
    #[must_use]
    pub fn anchors(&self) -> &[ChainAnchor] {
        &self.anchors
    }

    /// Whether the segment holds this exact block.
    #[must_use]
    pub fn holds(&self, anchor: ChainAnchor) -> bool {
        self.anchors.iter().any(|candidate| {
            candidate.number() == anchor.number()
                && candidate.hash() == anchor.hash()
                && candidate.chain_id() == anchor.chain_id()
        })
    }
}

/// The market's retained evidence: the committed cursor, the checkpoints that
/// boundary its retention window, and the journal rows they precede.
#[derive(Debug, Clone)]
pub struct RetainedEvidence {
    /// The market's committed exact cursor.
    pub cursor: ProjectionCursor,
    /// Every retained checkpoint, in journal-sequence order.
    pub checkpoints: Vec<CheckpointBoundary>,
    /// Every retained journal row, in journal-sequence order.
    pub journal: Vec<DeltaJournalRow>,
}

/// What the committed cursor means for a new canonical segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The cursor's block is one the segment holds: the head continues this
    /// market's branch and no recovery is due.
    Continues,
    /// The head abandoned the cursor's branch: this is the restore-and-replay
    /// plan that returns the market to a position both branches share.
    Recover(RecoveryPlan),
}

/// Why a recovery could not read its evidence or commit its restore.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// The recovery itself refused (a segment, retention, or replay refusal).
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    /// The market's retained evidence could not be read.
    #[error("market {market_id} retained evidence could not be read: {source}")]
    Evidence {
        /// The market the recovery belongs to.
        market_id: i64,
        /// The read's own refusal.
        source: degenbot_db::ProjectionError,
    },
    /// The restore could not be committed.
    #[error("market {market_id} recovery could not be committed: {source}")]
    Commit {
        /// The market the recovery belongs to.
        market_id: i64,
        /// The writer's own refusal.
        source: degenbot_db::ProjectionError,
    },
}

/// Where a recovery restores, and which retained deltas it replays to get there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    ancestor: LogCoordinate,
    ancestor_sequence: i64,
    checkpoint_sequence: i64,
    replay: Vec<DeltaJournalRow>,
}

impl RecoveryPlan {
    /// The deepest retained position the new head's ancestry holds.
    #[must_use]
    pub const fn ancestor(&self) -> LogCoordinate {
        self.ancestor
    }

    /// The journal entry the ancestor terminates at.
    #[must_use]
    pub const fn ancestor_sequence(&self) -> i64 {
        self.ancestor_sequence
    }

    /// The checkpoint the recovery restores before replaying forward.
    #[must_use]
    pub const fn checkpoint_sequence(&self) -> i64 {
        self.checkpoint_sequence
    }

    /// The retained deltas the recovery applies after the restore, in order.
    #[must_use]
    pub fn replay(&self) -> &[DeltaJournalRow] {
        &self.replay
    }
}

/// Whether a retained position is at or before the ancestor on one branch: a
/// lower block, or the same block at an earlier transaction/log.
fn at_or_before(position: LogCoordinate, ancestor: LogCoordinate) -> bool {
    if position.anchor().number() != ancestor.anchor().number() {
        return position.anchor().number() < ancestor.anchor().number();
    }
    position.anchor().hash() == ancestor.anchor().hash()
        && (position.tx_index(), position.log_index())
            <= (ancestor.tx_index(), ancestor.log_index())
}

/// Read a market's retained evidence: its committed cursor with the checkpoints
/// and journal rows the retention window still holds.
///
/// A market without an exact cursor has no completeness claim to defend, so it
/// needs no recovery.
///
/// # Errors
///
/// [`RecoveryError::Evidence`] when a retained read is refused.
pub fn read_retained_evidence(
    conn: &Connection,
    market_id: i64,
) -> Result<Option<RetainedEvidence>, RecoveryError> {
    let evidence =
        |source: degenbot_db::ProjectionError| RecoveryError::Evidence { market_id, source };
    let Some(cursor) =
        degenbot_db::read_projection_cursor_on_conn(conn, market_id).map_err(evidence)?
    else {
        return Ok(None);
    };
    let checkpoints = read_retained_checkpoints_on_conn(conn, market_id).map_err(evidence)?;
    let journal = read_retained_journal_on_conn(conn, market_id, 0).map_err(evidence)?;
    Ok(Some(RetainedEvidence {
        cursor,
        checkpoints,
        journal,
    }))
}

/// Decide what a new canonical segment means for the market's committed cursor:
/// the head continues its branch, or the market must be restored to the common
/// ancestor and replayed forward.
///
/// The common ancestor is the deepest retained position — the cursor, a journal
/// row, or a checkpoint boundary — whose block the segment holds. Everything
/// below an ancestor belongs to both branches by construction of a reorg, so the
/// newest retained checkpoint at or before it is a sound restore point.
///
/// # Errors
///
/// [`ProjectionError::RetentionExceeded`] when no retained position is on the new
/// head's ancestry, or when no retained checkpoint precedes the ancestor, and
/// [`ProjectionError::JournalGap`] when the retained journal is not contiguous from
/// the restore point.
pub fn plan_recovery(
    evidence: &RetainedEvidence,
    segment: &CanonicalSegment,
) -> Result<RecoveryOutcome, ProjectionError> {
    let cursor = evidence.cursor;
    if segment.holds(cursor.position.anchor()) {
        return Ok(RecoveryOutcome::Continues);
    }
    let mut candidates: Vec<(LogCoordinate, i64)> =
        Vec::with_capacity(evidence.journal.len() + evidence.checkpoints.len() + 1);
    candidates.push((cursor.position, cursor.journal_sequence));
    candidates.extend(
        evidence
            .journal
            .iter()
            .map(|row| (row.position, row.sequence)),
    );
    candidates.extend(
        evidence
            .checkpoints
            .iter()
            .map(|boundary| (boundary.position, boundary.journal_sequence)),
    );
    let retained_from = candidates
        .iter()
        .map(|(position, _)| position.anchor().number())
        .min()
        .unwrap_or_else(|| cursor.position.anchor().number());
    let deepest_checkpoint = evidence
        .checkpoints
        .iter()
        .map(|boundary| boundary.journal_sequence)
        .max();
    let retention_exceeded = || ProjectionError::RetentionExceeded {
        market_id: cursor.market_id,
        head: segment.tip().number(),
        covered_from: segment.lowest().number(),
        retained_from,
        deepest_checkpoint,
    };
    let ancestor = candidates
        .iter()
        .copied()
        .filter(|(position, _)| segment.holds(position.anchor()))
        .max_by_key(|(position, sequence)| {
            (
                position.anchor().number(),
                position.tx_index(),
                position.log_index(),
                *sequence,
            )
        });
    let Some((ancestor, ancestor_sequence)) = ancestor else {
        return Err(retention_exceeded());
    };
    let checkpoint = evidence
        .checkpoints
        .iter()
        .copied()
        .filter(|boundary| boundary.journal_sequence <= ancestor_sequence)
        .filter(|boundary| at_or_before(boundary.position, ancestor))
        .max_by_key(|boundary| boundary.journal_sequence);
    let Some(checkpoint) = checkpoint else {
        return Err(retention_exceeded());
    };
    let replay: Vec<DeltaJournalRow> = evidence
        .journal
        .iter()
        .filter(|row| {
            row.sequence > checkpoint.journal_sequence && row.sequence <= ancestor_sequence
        })
        .cloned()
        .collect();
    for (expected, row) in (checkpoint.journal_sequence + 1..).zip(replay.iter()) {
        if row.sequence != expected {
            return Err(ProjectionError::JournalGap {
                market_id: cursor.market_id,
                held: expected.saturating_sub(1),
                arrived: row.sequence,
            });
        }
    }
    Ok(RecoveryOutcome::Recover(RecoveryPlan {
        ancestor,
        ancestor_sequence,
        checkpoint_sequence: checkpoint.journal_sequence,
        replay,
    }))
}

/// The projection a recovery restores: the checkpoint's resume state with the
/// retained deltas applied forward to the common ancestor.
pub struct RestoredProjection {
    /// The checkpoint's restored resume state, read from the payload alone.
    pub state: CheckpointState,
    /// The forward read position replay left at the common ancestor.
    pub view: ProjectionView,
}

/// Restore a market's checkpoint and replay the retained deltas forward to the
/// planned ancestor, without writing anything.
///
/// The view is the reducing read position a resumed chunk continues from, and the
/// state is the relational resume state the caller writes back — both read from
/// the checkpoint's own payload, so no abandoned-branch row participates.
///
/// # Errors
///
/// [`ProjectionError::RecoveryCheckpointMissing`] when the planned checkpoint is no
/// longer retained, [`ProjectionError::CheckpointEncodingVersion`] when its payload
/// states another version, a decode refusal from [`CanonicalDelta::decode`], an
/// apply refusal from [`ProjectionView`], and
/// [`ProjectionError::RecoveryReplayIncomplete`] when replay does not end at the
/// ancestor.
pub fn restore_projection(
    conn: &Connection,
    market_id: i64,
    plan: &RecoveryPlan,
) -> Result<RestoredProjection, RecoveryError> {
    let evidence =
        |source: degenbot_db::ProjectionError| RecoveryError::Evidence { market_id, source };
    let row = degenbot_db::read_checkpoint_on_conn(conn, market_id, plan.checkpoint_sequence())
        .map_err(evidence)?
        .ok_or(ProjectionError::RecoveryCheckpointMissing {
            market_id,
            sequence: plan.checkpoint_sequence(),
        })?;
    if row.encoding_version != CHECKPOINT_ENCODING_VERSION {
        return Err(ProjectionError::CheckpointEncodingVersion {
            version: row.encoding_version,
            expected: CHECKPOINT_ENCODING_VERSION.to_string(),
        }
        .into());
    }
    let state = CheckpointState::restore(&row);
    let mut view = state.restore_view()?;
    for entry in plan.replay() {
        let journaled = JournaledDelta {
            source: log_ref(entry.position)?,
            generation: entry.generation.clone(),
            market_id,
            encoding: &entry.encoding,
            encoding_version: &entry.encoding_version,
        };
        view.apply(&CanonicalDelta::decode(&journaled)?)?;
    }
    let expected = log_ref(plan.ancestor())?;
    if view.cursor() != Some(expected) {
        return Err(ProjectionError::RecoveryReplayIncomplete {
            market_id,
            held: Box::new(view.cursor().unwrap_or(expected)),
            expected: Box::new(expected),
        }
        .into());
    }
    Ok(RestoredProjection { state, view })
}

/// What a recovery did to one market.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the restored variant carries the projection a resumed chunk continues from"
)]
pub enum Recovery {
    /// The committed cursor is still on the head's ancestry: nothing was written.
    Continues,
    /// The market is restored to its common ancestor and ready for the new
    /// branch's forward deltas.
    Restored {
        /// The market's cursor after the rewind.
        cursor: ProjectionCursor,
        /// The forward read position a resumed chunk continues from.
        view: ProjectionView,
        /// The common ancestor both branches share.
        ancestor: LogCoordinate,
        /// How many retained deltas replay applied.
        replayed: usize,
    },
}

/// Recover one market against a validated canonical segment, inside the caller's
/// transaction: plan from the retained evidence, restore and replay in memory, then
/// rewind the cursor and write the restored state.
///
/// A refusal moves nothing: the plan and the replay both run before the first
/// write, and the caller's transaction covers the rest.
///
/// # Errors
///
/// [`RecoveryError`] for a refused plan or replay, for its own reads, and for the
/// writer's expected-parent refusal.
pub fn recover_on_conn(
    conn: &Connection,
    market_id: i64,
    segment: &CanonicalSegment,
) -> Result<Recovery, RecoveryError> {
    let Some(evidence) = read_retained_evidence(conn, market_id)? else {
        return Ok(Recovery::Continues);
    };
    let RecoveryOutcome::Recover(plan) = plan_recovery(&evidence, segment)? else {
        return Ok(Recovery::Continues);
    };
    let restored = restore_projection(conn, market_id, &plan)?;
    let recovery = degenbot_db::ProjectionRecovery {
        market_id,
        expected: evidence.cursor,
        ancestor: plan.ancestor(),
        ancestor_sequence: plan.ancestor_sequence(),
        range_end_block: plan.ancestor().anchor().number(),
        restore: degenbot_db::RecoveryRestore {
            positions: restored.state.positions(),
            assets: restored.state.assets(),
            asset_configs: restored.state.asset_configs(),
            contracts: restored.state.contracts(),
            gho: restored.state.gho(),
            users: restored.state.users(),
            user_collaterals: restored.state.user_collaterals(),
            emode_categories: restored.state.emode_categories(),
        },
    };
    let cursor = degenbot_db::restore_projection_on_conn(conn, &recovery)
        .map_err(|source| RecoveryError::Commit { market_id, source })?;
    Ok(Recovery::Restored {
        cursor,
        view: restored.view,
        ancestor: plan.ancestor(),
        replayed: plan.replay().len(),
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests;
