//! The driver's reorg-recovery seam (ADR-105 D9).
//!
//! A head mismatch is not an ordinary forward advance: the market's committed
//! cursor names a position the new branch no longer contains. This module is the
//! one place the driver asks that question, at startup (before the range is
//! chosen, so a head that fell below the cursor is not mistaken for "up to date")
//! and at every subsequent chunk boundary (a reorg between two chunks).
//!
//! The chain evidence is acquired here, OUTSIDE the market's write transaction:
//! one header read at the cursor's own height answers whether the cursor's block
//! is still canonical, and only a mismatch pays for the session reorg window's
//! header chain. Recovery itself is RPC-free once that evidence exists.

use degenbot_db::{DegenbotDb, ProjectionCursor};
use degenbot_rpc::chain_evidence::acquire_header_chain;
use degenbot_rpc::provider::AlloyProvider;

use super::{DbError, RunError};
use crate::projection::{recover_on_conn, CanonicalSegment, Recovery};

/// Recover `market_id` when its committed cursor is not on the canonical branch
/// that ends at `head`, returning the market's cursor after the rewind.
///
/// `Ok(None)` means the cursor is still canonical (or the market has no exact
/// cursor yet): nothing was read beyond one header and nothing was written.
///
/// # Errors
///
/// [`RunError::TerminalAnchor`] when a header read is refused,
/// [`RunError::Projection`] for a segment that is not one linked chain, and
/// [`RunError::Recovery`] for a refused plan or replay — a reorg deeper than the
/// retained window names the market-reset path instead of moving the cursor.
pub(super) async fn recover_market_if_reorged(
    db: &DegenbotDb,
    provider: &AlloyProvider,
    market_id: i64,
    head: u64,
) -> Result<Option<ProjectionCursor>, RunError> {
    let Some(cursor) = db.fetch_projection_cursor(market_id)? else {
        return Ok(None);
    };
    let at = cursor.position.anchor();
    let served = if at.number() > head {
        None
    } else {
        provider.get_block_lineage_at(at.number()).await?
    };
    if served.is_some_and(|served| served.hash() == at.hash()) {
        return Ok(None);
    }
    let window_from = at.number().saturating_sub(degenbot_db::REORG_WINDOW_BLOCKS);
    let chain = acquire_header_chain(provider, window_from, head).await?;
    let segment = CanonicalSegment::new(chain.anchors().to_vec())
        .map_err(crate::projection::RecoveryError::from)?;
    recover_with_segment(db, market_id, &segment)
}

/// Recover `market_id` from an already acquired segment: the write half of
/// [`recover_market_if_reorged`], kept callable from a test with recorded header
/// evidence and by the driver with acquired evidence.
///
/// # Errors
///
/// [`RunError::Recovery`] for a refused plan or replay,
/// [`RunError::Projection`] for the persistence refusal, and `Db` for a read or
/// commit failure.
pub(super) fn recover_with_segment(
    db: &DegenbotDb,
    market_id: i64,
    segment: &CanonicalSegment,
) -> Result<Option<ProjectionCursor>, RunError> {
    let mut guard = db.lock();
    let tx = guard.transaction().map_err(DbError::from)?;
    let recovered = recover_on_conn(&tx, market_id, segment)?;
    let cursor = match recovered {
        Recovery::Continues => None,
        Recovery::Restored { cursor, .. } => Some(cursor),
    };
    tx.commit().map_err(DbError::from)?;
    Ok(cursor)
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use alloy::primitives::{B256, U256};
    use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};
    use degenbot_db::{
        read_projection_cursor_on_conn, write_projection_unit_on_conn, CursorAnchor,
        DeltaJournalEntry, LogCoordinate, ProjectionCheckpoint, ProjectionUnit,
    };
    use rusqlite::params;

    use super::recover_with_segment;
    use crate::projection::{CanonicalSegment, DELTA_ENCODING_VERSION};

    const MARKET: i64 = 1;
    const SHARED_FROM: u64 = 98;

    fn block_hash(branch: u8, number: u64) -> B256 {
        B256::from(U256::from(number | (u64::from(branch) << 40)).to_be_bytes::<32>())
    }

    fn linked(branch: u8, numbers: std::ops::RangeInclusive<u64>) -> Vec<ChainAnchor> {
        let mut anchors: Vec<ChainAnchor> = Vec::new();
        for number in numbers {
            let parent = anchors
                .last()
                .map_or_else(|| block_hash(branch, number - 1), ChainAnchor::hash);
            anchors.push(ChainAnchor::new(1, number, block_hash(branch, number), parent).unwrap());
        }
        anchors
    }

    fn log_ref(anchor: ChainAnchor) -> LogRef {
        let mut material = anchor.hash().to_vec();
        material.extend_from_slice(&(0x2000_u64 + anchor.number()).to_be_bytes());
        let tx_hash = alloy::primitives::keccak256(&material);
        LogRef::new(TxRef::new(anchor, tx_hash, 0).unwrap(), 1)
    }

    fn reorged(shared_until: u64, branch: u8, replaced_at: u64) -> CanonicalSegment {
        let mut anchors = linked(0xaa, SHARED_FROM..=shared_until);
        let parent = anchors.last().unwrap().hash();
        anchors.push(
            ChainAnchor::new(1, replaced_at, block_hash(branch, replaced_at), parent).unwrap(),
        );
        CanonicalSegment::new(anchors).unwrap()
    }

    fn seeded(head: u64) -> degenbot_db::DegenbotDb {
        let (db, _) = degenbot_db::DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (?1, 1, 'market', 1, ?2)",
                params![MARKET, i64::try_from(SHARED_FROM - 1).unwrap()],
            )
            .unwrap();
        }
        let mut cursor = None;
        for anchor in linked(0xaa, SHARED_FROM..=head) {
            let entries = [DeltaJournalEntry {
                position: LogCoordinate::new(anchor, log_ref(anchor).tx().hash(), 0, 1),
                generation: None,
                encoding: format!("test-delta@{}", anchor.number()),
            }];
            let cursor_anchor = cursor.map_or(
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(SHARED_FROM - 1),
                },
                CursorAnchor::At,
            );
            let conn = db.lock();
            let tx = conn.unchecked_transaction().unwrap();
            cursor = write_projection_unit_on_conn(
                &tx,
                &ProjectionUnit {
                    market_id: MARKET,
                    chain_id: 1,
                    encoding_version: DELTA_ENCODING_VERSION,
                    entries: &entries,
                    range_end_block: anchor.number(),
                    anchor: cursor_anchor,
                    checkpoint: Some(ProjectionCheckpoint {
                        encoding_version: crate::projection::CHECKPOINT_ENCODING_VERSION,
                        generations: &[],
                        positions: &[],
                        contracts: &[],
                        gho: None,
                    }),
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        db
    }

    fn cursor(db: &degenbot_db::DegenbotDb) -> Option<degenbot_db::ProjectionCursor> {
        read_projection_cursor_on_conn(&db.lock(), MARKET).unwrap()
    }

    fn commit_after(
        db: &degenbot_db::DegenbotDb,
        cursor: degenbot_db::ProjectionCursor,
        anchor: ChainAnchor,
    ) -> degenbot_db::ProjectionCursor {
        let entries = [DeltaJournalEntry {
            position: LogCoordinate::new(anchor, log_ref(anchor).tx().hash(), 0, 1),
            generation: None,
            encoding: format!("test-delta@{}", anchor.number()),
        }];
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let out = write_projection_unit_on_conn(
            &tx,
            &ProjectionUnit {
                market_id: MARKET,
                chain_id: 1,
                encoding_version: DELTA_ENCODING_VERSION,
                entries: &entries,
                range_end_block: anchor.number(),
                anchor: CursorAnchor::At(cursor),
                checkpoint: None,
            },
        )
        .expect("the recovered cursor is the expected parent");
        tx.commit().unwrap();
        out.expect("the unit advances")
    }

    #[test]
    fn a_startup_mismatch_recovers_and_the_next_unit_commits() {
        let db = seeded(105);
        let abandoned = cursor(&db).expect("the seeded market has a cursor");
        let segment = reorged(104, 0xbb, 105);

        let recovered = recover_with_segment(&db, MARKET, &segment)
            .unwrap()
            .expect("the abandoned cursor is recovered");
        assert_eq!(recovered.position.anchor().number(), 104);
        assert_eq!(recovered.position.anchor().hash(), block_hash(0xaa, 104));
        assert_ne!(
            recovered.position.anchor().hash(),
            abandoned.position.anchor().hash()
        );

        let replacement = *segment
            .anchors()
            .iter()
            .find(|anchor| anchor.number() == 105)
            .expect("the segment holds the reorged head");
        let next = commit_after(&db, recovered, replacement);
        assert_eq!(next.position.anchor().hash(), block_hash(0xbb, 105));
        assert_eq!(
            next.position.anchor().parent_hash(),
            recovered.position.anchor().hash(),
            "the run continues from the restored ancestor"
        );
    }

    #[test]
    fn a_mid_run_mismatch_recovers_the_same_way() {
        let db = seeded(105);
        let segment = reorged(103, 0xbb, 104);
        let recovered = recover_with_segment(&db, MARKET, &segment)
            .unwrap()
            .expect("a reorg between chunks recovers");
        assert_eq!(recovered.position.anchor().number(), 103);
        assert_eq!(
            recovered.journal_sequence, 6,
            "the retained journal up to the shared ancestor is the resumed position"
        );
    }

    #[test]
    fn a_mismatch_without_a_retained_ancestor_refuses_and_moves_nothing() {
        let db = seeded(105);
        let before = cursor(&db);
        let segment = CanonicalSegment::new(linked(0xbb, 200..=201)).unwrap();
        let error = recover_with_segment(&db, MARKET, &segment).unwrap_err();
        assert!(
            error.to_string().contains("market reset"),
            "the refusal names the recovery path: {error}"
        );
        assert_eq!(cursor(&db), before, "a refused recovery moves nothing");
    }
}
