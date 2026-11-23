//! A canonical delta's journal projection: the delta vocabulary mapped onto the
//! generic persistence vocabulary (ADR-105 D7, D8).
//!
//! The delta vocabulary owns meaning, encoding, and the mapping to Aave rows;
//! `degenbot-db` owns the schema, the constraints, and the atomic storage
//! primitives and never learns what a delta means. This module is the seam: it
//! states a delta's exact chain position, the generation relationship its
//! reduction consumed, and its versioned encoding in the terms a journal row
//! stores.

use degenbot_core::canonical_head::LogRef;
use degenbot_db::{
    read_projection_cursor_on_conn, CursorAnchor, DeltaJournalEntry,
    GenerationActivation as PersistedActivation, GenerationRecord, LogCoordinate, ProjectionError,
};
use rusqlite::{Connection, OptionalExtension};

use super::{CanonicalDelta, GenerationActivation, GenerationRef};

/// The encoding version every journaled delta states. A vocabulary change that
/// alters what `[CanonicalDelta::encode]` emits advances this tag, so a reader
/// can tell two encodings apart without guessing (ADR-105 D7).
pub const DELTA_ENCODING_VERSION: &str = "aave.canonical-delta/v1";

/// The market's chain and the anchor its next projection unit must continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketProjection {
    /// The chain the market lives on (`aave_v3_markets.chain_id`).
    pub chain_id: i64,
    /// The token the next unit is checked against.
    pub anchor: CursorAnchor,
}

/// Read `market_id`'s projection state: its exact cursor when one is
/// established, the numeric claim that must be replaced when one is not, and
/// `Unchanged` for a market that has never completed a range.
///
/// # Errors
///
/// [`ProjectionError::MissingMarket`] when no market row matches, and the read
/// path's own refusals for a corrupt cursor row.
pub fn market_projection(
    conn: &Connection,
    market_id: i64,
) -> Result<MarketProjection, ProjectionError> {
    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT chain_id, last_update_block FROM aave_v3_markets WHERE id = ?1",
            rusqlite::params![market_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (chain_id, numeric) = row.ok_or(ProjectionError::MissingMarket(market_id))?;
    let anchor = match read_projection_cursor_on_conn(conn, market_id)? {
        Some(cursor) => CursorAnchor::At(cursor),
        None => CursorAnchor::First {
            replaces_numeric_cursor: numeric
                .map(|numeric| {
                    u64::try_from(numeric).map_err(|_| {
                        ProjectionError::Decode(format!("last_update_block {numeric}"))
                    })
                })
                .transpose()?,
        },
    };
    Ok(MarketProjection { chain_id, anchor })
}

fn log_coordinate(at: LogRef) -> LogCoordinate {
    let tx = at.tx();
    LogCoordinate::new(*tx.anchor(), tx.hash(), tx.index(), at.index())
}

fn generation_record(generation: GenerationRef) -> GenerationRecord {
    GenerationRecord {
        entity: generation.entity().encode(),
        activation: match generation.activation() {
            GenerationActivation::Seed => PersistedActivation::Seed,
            GenerationActivation::Event(at) => PersistedActivation::Event(log_coordinate(at)),
        },
        revision: generation.revision(),
    }
}

/// One delta's journal entry: its exact position, its generation relationship,
/// and its canonical encoding.
#[must_use]
pub fn journal_entry(delta: &CanonicalDelta) -> DeltaJournalEntry {
    DeltaJournalEntry {
        position: log_coordinate(delta.source()),
        generation: delta.generation().map(generation_record),
        encoding: delta.encode(),
    }
}

/// A chunk's deltas in emission order, as the journal entries its projection
/// unit persists.
#[must_use]
pub fn journal_entries(deltas: &[CanonicalDelta]) -> Vec<DeltaJournalEntry> {
    deltas.iter().map(journal_entry).collect()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use degenbot_core::canonical_head::{ChainAnchor, TxRef};
    use degenbot_db::ScaledTokenPosition;

    use crate::projection::{DeltaEffect, GenerationEntity, PositionSubject};

    fn source() -> LogRef {
        let anchor = ChainAnchor::new(
            1,
            26_130_441,
            B256::repeat_byte(0xaa),
            B256::repeat_byte(0x99),
        )
        .unwrap();
        let tx = TxRef::new(anchor, B256::repeat_byte(0xbb), 3).unwrap();
        LogRef::new(tx, 7)
    }

    #[test]
    fn a_delta_maps_to_its_exact_position_encoding_and_generation() {
        let delta = CanonicalDelta::new(
            source(),
            Some(GenerationRef::new(
                GenerationEntity::ScaledToken {
                    asset_id: 2,
                    position: ScaledTokenPosition::Collateral,
                },
                1,
            )),
            DeltaEffect::ScaledBalance {
                subject: PositionSubject {
                    position: ScaledTokenPosition::Collateral,
                    position_id: 9,
                },
                balance_delta: alloy::primitives::I256::try_from(5_i64).unwrap(),
                index: alloy::primitives::U256::from(7_u64),
            },
        );
        let entry = journal_entry(&delta);
        assert_eq!(entry.position.anchor(), *source().tx().anchor());
        assert_eq!(entry.position.tx_hash(), source().tx().hash());
        assert_eq!(entry.position.tx_index(), 3);
        assert_eq!(entry.position.log_index(), 7);
        assert_eq!(entry.encoding, delta.encode());
        let record = entry.generation.unwrap();
        assert_eq!(record.entity, "scaled-token{asset=2,leg=collateral}");
        assert_eq!(record.activation, PersistedActivation::Seed);
        assert_eq!(record.revision, Some(1));
    }

    #[test]
    fn an_event_activated_generation_carries_its_activation_position() {
        let at = source();
        let delta = CanonicalDelta::new(
            at,
            Some(GenerationRef::activated_at(
                GenerationEntity::Contract {
                    market_id: 1,
                    role: crate::projection::ContractRole::Pool,
                },
                Some(2),
                at,
            )),
            DeltaEffect::GhoRefreshDiscount { position_id: 4 },
        );
        let record = journal_entry(&delta).generation.unwrap();
        assert_eq!(record.entity, "contract{market=1,role=pool}");
        assert_eq!(record.revision, Some(2));
        assert_eq!(
            record.activation,
            PersistedActivation::Event(LogCoordinate::new(
                *at.tx().anchor(),
                at.tx().hash(),
                at.tx().index(),
                at.index()
            ))
        );
    }

    #[test]
    fn a_generation_free_delta_journals_no_relationship() {
        let delta = CanonicalDelta::new(
            source(),
            None,
            DeltaEffect::GhoRefreshDiscount { position_id: 1 },
        );
        assert_eq!(journal_entry(&delta).generation, None);
        assert_eq!(journal_entries(std::slice::from_ref(&delta)).len(), 1);
    }
}
