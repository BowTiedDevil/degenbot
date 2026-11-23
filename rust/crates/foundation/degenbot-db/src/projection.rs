//! Per-market exact Projection cursors and the canonical delta journal
//! (ADR-105 D8, D9).
//!
//! A market's completeness claim is an exact chain position, not a block
//! number: [`ProjectionCursor`] names the terminal [`ChainAnchor`] of the last
//! committed projection unit, the transaction and log the unit last advanced
//! to, the journal entry that position belongs to, and the numeric range end
//! the unit's projection covers. [`ProjectionUnit`] is the one atomic write:
//! its [`DeltaJournalEntry`] rows, the chain identities they name, the
//! generation relationships they were reduced under, and the cursor advance
//! all commit together inside the caller's transaction.
//!
//! # What this module owns, and what it does not
//!
//! This module owns the SCHEMA shape of projection persistence — the tables,
//! the constraints, and the atomic storage primitives — and nothing about what
//! a delta MEANS. The delta vocabulary, its versioned encoding, and the mapping
//! from a delta to Aave rows live in `degenbot-aave` (ADR-105 D7); a journal
//! entry arrives here already encoded and already naming its settled identity.
//! `degenbot-db` never depends on `degenbot-aave`.
//!
//! # Refusal is the whole point
//!
//! Every refusal leaves the database byte-identical, because the caller's
//! transaction is dropped: an unexpected parent, a broken parent link, a
//! duplicate branch position, an invalid generation relationship, a range claim
//! that does not advance, or a first anchor with no evidence. The cursor is the
//! per-market writer fence's token (ADR-070): a caller states the exact anchor
//! it expects to find, so a competing advance, a reorg, or a replay is refused
//! rather than applied.
//!
//! # Foreign-key enforcement
//!
//! The declared foreign keys bind each journal row's block, transaction, and
//! generation to the identity rows the same unit writes, and the writer's
//! connection enforces them. The writer ALSO verifies each identity it depends
//! on, so a contradiction is a typed refusal naming the block or the
//! transaction rather than a generic constraint failure.

use alloy::primitives::B256;
use degenbot_core::canonical_head::ChainAnchor;
use rusqlite::{params, Connection, OptionalExtension};

use crate::connection::DegenbotDb;

/// The projection unit writer (the write substrate, ADR-070).
#[cfg(any(test, feature = "writer"))]
mod persist;

/// One exact chain position: the block identity, the transaction identity, and
/// the block-wide log index.
///
/// A position is the journal's identity unit: two entries at one position differ
/// only by their [`DeltaJournalEntry`] ordinal (one log can carry more than one
/// delta), and a same-height peer block is a different position because its
/// block hash is part of the identity (ADR-105 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogCoordinate {
    anchor: ChainAnchor,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
}

impl LogCoordinate {
    /// The position of `log_index` in transaction `tx_hash` (at `tx_index`)
    /// inside `anchor`'s block.
    #[must_use]
    pub const fn new(anchor: ChainAnchor, tx_hash: B256, tx_index: u64, log_index: u64) -> Self {
        Self {
            anchor,
            tx_hash,
            tx_index,
            log_index,
        }
    }

    /// The exact block identity this position sits in.
    #[must_use]
    pub const fn anchor(&self) -> ChainAnchor {
        self.anchor
    }

    /// The transaction's own hash.
    #[must_use]
    pub const fn tx_hash(&self) -> B256 {
        self.tx_hash
    }

    /// The transaction's index within its block.
    #[must_use]
    pub const fn tx_index(&self) -> u64 {
        self.tx_index
    }

    /// The log's block-wide index.
    #[must_use]
    pub const fn log_index(&self) -> u64 {
        self.log_index
    }
}

/// The position-order and branch predicates the writer checks a unit against
/// (the write substrate, ADR-070).
#[cfg(any(test, feature = "writer"))]
impl LogCoordinate {
    /// Whether `self` is strictly forward of `held` on the same branch: a later
    /// block, or the same block (same hash) at a later transaction/log.
    #[must_use]
    pub fn is_forward_of(&self, held: &Self) -> bool {
        if self.anchor.number() > held.anchor.number() {
            return true;
        }
        self.anchor.number() == held.anchor.number()
            && self.anchor.hash() == held.anchor.hash()
            && (self.tx_index, self.log_index) > (held.tx_index, held.log_index)
    }

    /// Whether `self` looks like a continuation of `held`'s block without
    /// naming it: the same height on another branch.
    #[must_use]
    pub fn contradicts_branch_of(&self, held: &Self) -> bool {
        self.anchor.chain_id() == held.anchor.chain_id()
            && self.anchor.number() == held.anchor.number()
            && self.anchor.hash() != held.anchor.hash()
    }
}

/// Where a generation came into being (ADR-105 D6).
///
/// [`Self::Seed`] is the generation the resolution started from — committed
/// state at the unit's first event, with no activation position inside the
/// retained window. [`Self::Event`] names the exact event that activated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationActivation {
    /// Resolved from committed state; no activation position in the journal.
    Seed,
    /// Activated by this exact event.
    Event(LogCoordinate),
}

/// One generation-qualified relationship a delta was reduced under.
///
/// Identity is the entity key plus the activation (ADR-105 D6): a same-height
/// replacement activates a different generation rather than mutating one, and a
/// role or link entity with no revision contract carries `revision: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRecord {
    /// The stable logical entity key ("`scaled-token{asset=2,leg=collateral}`").
    pub entity: String,
    /// Where the generation came into being.
    pub activation: GenerationActivation,
    /// The entity's revision fact, where it has one.
    pub revision: Option<u32>,
}

/// One canonical delta's persisted evidence: the exact position it was emitted
/// at, the generation it was reduced under, and its versioned encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaJournalEntry {
    /// The delta's exact chain position.
    pub position: LogCoordinate,
    /// The generation relationship, when the delta consumed one.
    pub generation: Option<GenerationRecord>,
    /// The delta's canonical encoding (owned by the delta's vocabulary).
    pub encoding: String,
}

/// A market's committed exact projection position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionCursor {
    /// The market this cursor belongs to.
    pub market_id: i64,
    /// The exact position the market's journal last advanced to.
    pub position: LogCoordinate,
    /// The journal entry that position belongs to (per-market, monotone).
    pub journal_sequence: i64,
    /// The numeric range end the market's projection covers.
    pub range_end_block: u64,
}

/// What the caller expects the market's cursor to be (the writer-fence token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAnchor {
    /// The unit journals no delta, so it neither establishes nor moves the
    /// cursor: only the numeric range claim advances.
    Unchanged,
    /// The market has no exact cursor yet. This unit establishes the first
    /// anchor from the deltas' own resolved chain evidence, replacing (and
    /// proving forward of) the numeric cursor the market row still carries —
    /// `None` for a market that never completed a range.
    First {
        /// The still-numeric `last_update_block` claim this unit replaces.
        replaces_numeric_cursor: Option<u64>,
    },
    /// The market's exact cursor terminates at this row.
    At(ProjectionCursor),
}

/// One atomic projection unit: the journal entries, their identities, and the
/// cursor advance they justify.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionUnit<'a> {
    /// The market the unit belongs to.
    pub market_id: i64,
    /// The chain the market lives on.
    pub chain_id: i64,
    /// The encoding version every journal entry states.
    pub encoding_version: &'a str,
    /// The deltas the unit journals, in emission order.
    pub entries: &'a [DeltaJournalEntry],
    /// The numeric range end this unit's projection covers.
    pub range_end_block: u64,
    /// What the unit expects the cursor to be.
    pub anchor: CursorAnchor,
}

/// One journaled delta, read back for recovery and replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaJournalRow {
    /// The entry's per-market sequence.
    pub sequence: i64,
    /// The delta's ordinal among the deltas emitted at the same position.
    pub position_ordinal: i64,
    /// The delta's exact chain position.
    pub position: LogCoordinate,
    /// The generation relationship, when the delta consumed one.
    pub generation: Option<GenerationRecord>,
    /// The encoding version the entry states.
    pub encoding_version: String,
    /// The delta's canonical encoding.
    pub encoding: String,
}

/// Why a projection unit was refused.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    /// The `SQLite` layer refused a statement.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored coordinate could not be decoded into an exact identity.
    #[error("projection decode error: {0}")]
    Decode(String),
    /// No `aave_v3_markets` row matches the unit's market.
    #[error("no aave_v3_markets row for market {0}")]
    MissingMarket(i64),
    /// The market's `chain_id` is not a chain identifier.
    #[error("market {market_id} carries chain id {chain_id}, which is not a chain")]
    InvalidMarketChain {
        /// The market row's id.
        market_id: i64,
        /// The market row's chain id.
        chain_id: i64,
    },
    /// A journal entry names another chain than the market's.
    #[error("entry on chain {entry} cannot be journaled for market {market_id} on chain {market}")]
    ChainMismatch {
        /// The market row's id.
        market_id: i64,
        /// The market's chain.
        market: u64,
        /// The entry's chain.
        entry: u64,
    },
    /// The unit journals deltas without stating the anchor they continue.
    #[error("market {market_id} journaled deltas without a cursor anchor claim")]
    AnchorRequired {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// A `First` unit was offered for a market whose exact cursor already
    /// exists: a first anchor is never re-established, only extended.
    #[error("market {market_id} already has an exact projection cursor")]
    CursorAlreadyEstablished {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// An `At` unit was offered for a market with no exact cursor: the caller's
    /// token names a position the database never committed.
    #[error("market {market_id} has no exact projection cursor to extend")]
    CursorMissing {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// The cursor row differs from the exact anchor the caller expected: a
    /// competing advance, a reorg, or a replay.
    #[error("cursor for market {market_id} is not the expected parent")]
    UnexpectedParent {
        /// The market the unit belongs to.
        market_id: i64,
        /// The anchor the caller stated.
        expected: Box<ProjectionCursor>,
        /// The anchor the database holds.
        held: Box<ProjectionCursor>,
    },
    /// A `First` unit named no numeric claim while the market still carries one
    /// (or named one the market does not carry): the migration cannot say what
    /// it replaces, so its evidence is unproven.
    #[error(
        "market {market_id} cannot establish its first anchor: claimed numeric cursor {named:?}, \
         market carries {held:?}"
    )]
    FirstAnchorUnproven {
        /// The market the unit belongs to.
        market_id: i64,
        /// The numeric claim the unit named, if any.
        named: Option<u64>,
        /// The numeric claim the market row holds, if any.
        held: Option<u64>,
    },
    /// The unit's exact evidence does not reach past the numeric claim it
    /// replaces: a first anchor must cover blocks the numeric cursor did not.
    #[error("market {market_id} first anchor at {arrived} is not forward of the numeric cursor {replaces}")]
    FirstAnchorNotForward {
        /// The market the unit belongs to.
        market_id: i64,
        /// The numeric cursor the unit replaces.
        replaces: u64,
        /// The block the unit's first journaled position names.
        arrived: u64,
    },
    /// A unit with no deltas claimed a first anchor: there is no exact
    /// evidence to establish one from.
    #[error("market {market_id} claims a first anchor without any journaled delta")]
    FirstAnchorWithoutEvidence {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// The unit's range claim does not advance past the committed one.
    #[error("market {market_id} range end {arrived} is not forward of the committed {held}")]
    RangeNotForward {
        /// The market the unit belongs to.
        market_id: i64,
        /// The committed range end.
        held: u64,
        /// The unit's range end.
        arrived: u64,
    },
    /// The unit's first position is not forward of the committed one.
    #[error(
        "market {market_id} entry at block {arrived} is not forward of the cursor at block {held}"
    )]
    NotForward {
        /// The market the unit belongs to.
        market_id: i64,
        /// The committed cursor's block number.
        held: u64,
        /// The arriving entry's block number.
        arrived: u64,
    },
    /// The unit's first position continues the cursor's block on another branch.
    #[error("market {market_id} entry at block {number} names another branch than the cursor")]
    BranchMismatch {
        /// The market the unit belongs to.
        market_id: i64,
        /// The shared height.
        number: u64,
    },
    /// The unit's first position is the cursor's next height but builds on a
    /// different block than the cursor's own hash.
    #[error(
        "market {market_id} entry at block {number} builds on {parent} rather than the cursor's {expected}"
    )]
    BrokenParentLink {
        /// The market the unit belongs to.
        market_id: i64,
        /// The arriving height.
        number: u64,
        /// The parent hash the entry claims.
        parent: B256,
        /// The cursor's own block hash.
        expected: B256,
    },
    /// One chain position already carries this ordinal in the market's journal.
    #[error(
        "market {market_id} already journals ordinal {position_ordinal} at {tx_hash}/{log_index}"
    )]
    DuplicateBranchPosition {
        /// The market the unit belongs to.
        market_id: i64,
        /// The transaction hash.
        tx_hash: B256,
        /// The block-wide log index.
        log_index: u64,
        /// The repeating ordinal.
        position_ordinal: i64,
    },
    /// A block hash is recorded as one identity and offered as another (a
    /// different height or parent).
    #[error("block {block_hash} is already recorded with another identity")]
    BlockIdentityConflict {
        /// The contradicting block hash.
        block_hash: B256,
        /// The height the caller offered.
        number: u64,
    },
    /// A transaction hash is recorded at one position and offered at another.
    #[error("transaction {tx_hash} is already recorded at another position")]
    TransactionIdentityConflict {
        /// The contradicting transaction hash.
        tx_hash: B256,
        /// The block hash the caller offered.
        block_hash: B256,
        /// The transaction index the caller offered.
        tx_index: u64,
    },
    /// A generation record names no entity.
    #[error("market {market_id} names a generation with no entity")]
    InvalidGenerationEntity {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// A generation's activation is later than the delta that consumed it, or
    /// sits on another branch: the delta cannot have read it.
    #[error("market {market_id} name generation {entity:?} activated at a position the delta cannot read")]
    GenerationNotForward {
        /// The market the unit belongs to.
        market_id: i64,
        /// The generation's entity key.
        entity: String,
    },
    /// A journal row references a generation the market does not own.
    #[error("market {market_id} references generation {generation_id}, which it does not own")]
    ForeignGeneration {
        /// The market the unit belongs to.
        market_id: i64,
        /// The generation row id.
        generation_id: i64,
    },
    /// A generation row could not be resolved after it was recorded.
    #[error("market {market_id} recorded generation {entity:?} but cannot resolve it")]
    MissingGeneration {
        /// The market the unit belongs to.
        market_id: i64,
        /// The generation's entity key.
        entity: String,
    },
}

/// The canonical hex form of a hash column (`0x`-prefixed, lowercase).
#[cfg(any(test, feature = "writer"))]
fn encode_hash(hash: B256) -> String {
    alloy::hex::encode_prefixed(hash)
}

fn decode_hash(value: &str) -> Result<B256, ProjectionError> {
    let bytes = alloy::hex::decode(value)
        .map_err(|error| ProjectionError::Decode(format!("hash {value:?}: {error}")))?;
    if bytes.len() != 32 {
        return Err(ProjectionError::Decode(format!(
            "hash {value:?} is not 32 bytes"
        )));
    }
    Ok(B256::from_slice(&bytes))
}

fn decode_anchor(
    chain_id: i64,
    number: i64,
    hash: &str,
    parent_hash: &str,
) -> Result<ChainAnchor, ProjectionError> {
    let chain = u64::try_from(chain_id)
        .map_err(|_| ProjectionError::Decode(format!("chain id {chain_id}")))?;
    let height = u64::try_from(number)
        .map_err(|_| ProjectionError::Decode(format!("block number {number}")))?;
    ChainAnchor::new(chain, height, decode_hash(hash)?, decode_hash(parent_hash)?)
        .map_err(|error| ProjectionError::Decode(error.to_string()))
}

/// The ten columns a cursor row selects, in `SELECT` order.
type CursorColumns = (i64, i64, String, String, String, i64, i64, i64, i64, i64);

/// The exact projection cursor of `market_id`, if the market has one.
///
/// # Errors
///
/// [`ProjectionError::Sqlite`] on a query failure, and
/// [`ProjectionError::Decode`] when a stored coordinate is not an exact
/// identity.
pub fn read_projection_cursor_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<Option<ProjectionCursor>, ProjectionError> {
    let row: Option<CursorColumns> = conn
        .query_row(
            "SELECT chain_id, block_number, block_hash, parent_hash, tx_hash, tx_index, \
                    log_index, journal_sequence, range_end_block, market_id \
             FROM aave_v3_projection_cursors WHERE market_id = ?1",
            params![market_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        chain_id,
        number,
        hash,
        parent_hash,
        tx_hash,
        tx_index,
        log_index,
        journal_sequence,
        range_end_block,
        market_id_read,
    )) = row
    else {
        return Ok(None);
    };
    let anchor = decode_anchor(chain_id, number, &hash, &parent_hash)?;
    Ok(Some(ProjectionCursor {
        market_id: market_id_read,
        position: LogCoordinate::new(
            anchor,
            decode_hash(&tx_hash)?,
            u64::try_from(tx_index)
                .map_err(|_| ProjectionError::Decode(format!("tx index {tx_index}")))?,
            u64::try_from(log_index)
                .map_err(|_| ProjectionError::Decode(format!("log index {log_index}")))?,
        ),
        journal_sequence,
        range_end_block: u64::try_from(range_end_block)
            .map_err(|_| ProjectionError::Decode(format!("range end {range_end_block}")))?,
    }))
}

impl DegenbotDb {
    /// Read `market_id`'s exact projection cursor through the shared read
    /// handle.
    ///
    /// # Errors
    ///
    /// See [`read_projection_cursor_on_conn`].
    pub fn fetch_projection_cursor(
        &self,
        market_id: i64,
    ) -> Result<Option<ProjectionCursor>, ProjectionError> {
        let conn = self.lock();
        read_projection_cursor_on_conn(&conn, market_id)
    }
}

/// Every journaled delta of `market_id`, in per-market sequence order.
///
/// # Errors
///
/// [`ProjectionError::Sqlite`] on a query failure, and
/// [`ProjectionError::Decode`] when a stored coordinate is not an exact
/// identity.
pub fn read_journal_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<Vec<DeltaJournalRow>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT j.sequence, j.position_ordinal, j.chain_id, j.block_number, j.block_hash, \
                j.parent_hash, j.tx_hash, j.tx_index, j.log_index, j.encoding_version, \
                j.delta_encoding, g.entity, g.activation_kind, g.revision, g.chain_id, \
                g.block_number, g.block_hash, g.parent_hash, g.tx_hash, g.tx_index, g.log_index \
         FROM aave_v3_delta_journal j \
         LEFT JOIN aave_v3_generations g \
           ON g.id = j.generation_id AND g.market_id = j.market_id \
         WHERE j.market_id = ?1 ORDER BY j.sequence",
    )?;
    let mut rows = statement.query(params![market_id])?;
    let mut journal = Vec::new();
    while let Some(row) = rows.next()? {
        let anchor = decode_anchor(
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            &row.get::<_, String>(4)?,
            &row.get::<_, String>(5)?,
        )?;
        let position = LogCoordinate::new(
            anchor,
            decode_hash(&row.get::<_, String>(6)?)?,
            u64::try_from(row.get::<_, i64>(7)?)
                .map_err(|_| ProjectionError::Decode("tx index".to_string()))?,
            u64::try_from(row.get::<_, i64>(8)?)
                .map_err(|_| ProjectionError::Decode("log index".to_string()))?,
        );
        let entity: Option<String> = row.get(11)?;
        let generation = match entity {
            None => None,
            Some(entity) => {
                let kind: String = row.get(12)?;
                let revision: Option<i64> = row.get(13)?;
                let activation = match kind.as_str() {
                    "seed" => GenerationActivation::Seed,
                    "event" => {
                        let activation_anchor = decode_anchor(
                            row.get::<_, i64>(14)?,
                            row.get::<_, i64>(15)?,
                            &row.get::<_, String>(16)?,
                            &row.get::<_, String>(17)?,
                        )?;
                        GenerationActivation::Event(LogCoordinate::new(
                            activation_anchor,
                            decode_hash(&row.get::<_, String>(18)?)?,
                            u64::try_from(row.get::<_, i64>(19)?)
                                .map_err(|_| ProjectionError::Decode("tx index".to_string()))?,
                            u64::try_from(row.get::<_, i64>(20)?)
                                .map_err(|_| ProjectionError::Decode("log index".to_string()))?,
                        ))
                    }
                    other => {
                        return Err(ProjectionError::Decode(format!(
                            "generation activation kind {other:?}"
                        )))
                    }
                };
                Some(GenerationRecord {
                    entity,
                    activation,
                    revision: revision
                        .map(|revision| {
                            u32::try_from(revision).map_err(|_| {
                                ProjectionError::Decode(format!("revision {revision}"))
                            })
                        })
                        .transpose()?,
                })
            }
        };
        journal.push(DeltaJournalRow {
            sequence: row.get(0)?,
            position_ordinal: row.get(1)?,
            position,
            generation,
            encoding_version: row.get(9)?,
            encoding: row.get(10)?,
        });
    }
    Ok(journal)
}

/// Persist one projection unit: its journal entries, the chain identities they
/// name, their generation relationships, and the cursor advance — all inside
/// the caller's transaction (ADR-105 D8).
///
/// Returns the market's cursor after the unit: `None` when the market still has
/// no exact anchor (a unit that journals nothing cannot establish one).
///
/// # Errors
///
/// Every `ProjectionError` refusal leaves the caller's transaction uncommitted,
/// so the unit is atomic: see `CursorAnchor` for the token each advance is
/// checked against.
#[cfg(any(test, feature = "writer"))]
pub use persist::write_projection_unit_on_conn;

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use alloy::primitives::U256;

    use super::*;

    fn hash_of(number: u64) -> B256 {
        B256::from(U256::from(number).to_be_bytes::<32>())
    }

    fn chain_anchor() -> ChainAnchor {
        ChainAnchor::new(1, 99, hash_of(99), hash_of(98)).unwrap()
    }

    fn anchor_at(number: u64) -> ChainAnchor {
        ChainAnchor::new(1, number, hash_of(number), hash_of(number - 1)).unwrap()
    }

    fn tx_hash(seed: u64) -> B256 {
        B256::from(U256::from(0x1000 + seed).to_be_bytes::<32>())
    }

    fn position(number: u64, tx_seed: u64, tx_index: u64, log_index: u64) -> LogCoordinate {
        LogCoordinate::new(anchor_at(number), tx_hash(tx_seed), tx_index, log_index)
    }

    fn entry(position: LogCoordinate) -> DeltaJournalEntry {
        DeltaJournalEntry {
            position,
            generation: None,
            encoding: format!("delta@{:?}/{}", position.tx_hash(), position.log_index()),
        }
    }

    fn unit(
        entries: &[DeltaJournalEntry],
        range_end_block: u64,
        anchor: CursorAnchor,
    ) -> ProjectionUnit<'_> {
        ProjectionUnit {
            market_id: 1,
            chain_id: 1,
            encoding_version: "test/v1",
            entries,
            range_end_block,
            anchor,
        }
    }

    fn seeded_market() -> DegenbotDb {
        let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
        db.lock()
            .execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'market', 1, 99)",
                [],
            )
            .unwrap();
        db
    }

    fn add_market(db: &DegenbotDb, id: i64, chain_id: i64, numeric: i64) {
        db.lock()
            .execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (?1, ?2, 'market', 1, ?3)",
                params![id, chain_id, numeric],
            )
            .unwrap();
    }

    fn commit(
        db: &DegenbotDb,
        unit: &ProjectionUnit<'_>,
    ) -> Result<Option<ProjectionCursor>, ProjectionError> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let outcome = write_projection_unit_on_conn(&tx, unit)?;
        tx.commit().unwrap();
        Ok(outcome)
    }

    fn journal_count(db: &DegenbotDb) -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM aave_v3_delta_journal", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn anchor_count(db: &DegenbotDb) -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM aave_v3_block_anchors", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn generation_count(db: &DegenbotDb) -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM aave_v3_generations", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn cursor(db: &DegenbotDb, market_id: i64) -> Option<ProjectionCursor> {
        db.fetch_projection_cursor(market_id).unwrap()
    }

    #[test]
    fn first_anchor_establishes_the_cursor_and_journals_the_delta() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7)), entry(position(100, 1, 3, 9))];
        let committed = commit(
            &db,
            &unit(
                &entries,
                120,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        assert_eq!(committed.position, position(100, 1, 3, 9));
        assert_eq!(committed.journal_sequence, 2);
        assert_eq!(committed.range_end_block, 120);
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(journal_count(&db), 2);
        assert_eq!(anchor_count(&db), 1, "one block identity for the unit");

        let journal = read_journal_on_conn(&db.lock(), 1).unwrap();
        assert_eq!(journal.len(), 2);
        assert_eq!(journal[0].sequence, 1);
        assert_eq!(journal[0].position_ordinal, 1);
        assert_eq!(journal[1].position_ordinal, 1);
        assert_eq!(journal[1].encoding_version, "test/v1");
        assert_eq!(journal[1].encoding, entries[1].encoding);
    }

    #[test]
    fn one_log_carrying_two_deltas_is_two_journal_entries() {
        let db = seeded_market();
        let at = position(100, 1, 3, 7);
        let entries = vec![entry(at), entry(at)];
        commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();
        let journal = read_journal_on_conn(&db.lock(), 1).unwrap();
        assert_eq!(journal.len(), 2);
        assert_eq!(journal[0].position_ordinal, 1);
        assert_eq!(journal[1].position_ordinal, 2);
    }

    #[test]
    fn first_anchor_refuses_when_an_exact_cursor_exists() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap();

        let next = vec![entry(position(101, 2, 0, 0))];
        let err = commit(
            &db,
            &unit(
                &next,
                101,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::CursorAlreadyEstablished { .. }
        ));
        assert_eq!(journal_count(&db), 1);
        assert_eq!(cursor(&db, 1).unwrap().range_end_block, 100);
    }

    #[test]
    fn first_anchor_without_exact_evidence_refuses() {
        let db = seeded_market();
        let err = commit(
            &db,
            &unit(
                &[],
                120,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::FirstAnchorWithoutEvidence { .. }
        ));
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
    }

    #[test]
    fn first_anchor_must_name_the_numeric_claim_it_replaces() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let err = commit(
            &db,
            &unit(
                &entries,
                120,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(50),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::FirstAnchorUnproven { .. }));
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
        assert_eq!(anchor_count(&db), 0, "a refusal writes no identity");
    }

    #[test]
    fn a_first_anchor_must_not_ignore_the_market_numeric_claim() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let err = commit(
            &db,
            &unit(
                &entries,
                120,
                CursorAnchor::First {
                    replaces_numeric_cursor: None,
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::FirstAnchorUnproven { .. }));
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
    }

    #[test]
    fn first_anchor_must_be_forward_of_the_numeric_claim() {
        let db = seeded_market();
        let entries = vec![entry(position(99, 1, 3, 7))];
        let err = commit(
            &db,
            &unit(
                &entries,
                120,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::FirstAnchorNotForward { .. }));
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
    }

    #[test]
    fn unexpected_parent_leaves_state_and_cursor_unchanged() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let mut tampered = committed;
        tampered.position = position(100, 2, 3, 7);
        let next = vec![entry(position(101, 3, 1, 1))];
        let err = commit(&db, &unit(&next, 101, CursorAnchor::At(tampered))).unwrap_err();
        assert!(matches!(err, ProjectionError::UnexpectedParent { .. }));
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(journal_count(&db), 1);
        assert_eq!(anchor_count(&db), 1);
    }

    #[test]
    fn cursor_missing_refuses_an_extend() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let claimed = ProjectionCursor {
            market_id: 1,
            position: position(100, 1, 3, 7),
            journal_sequence: 1,
            range_end_block: 100,
        };
        let err = commit(&db, &unit(&entries, 101, CursorAnchor::At(claimed))).unwrap_err();
        assert!(matches!(err, ProjectionError::CursorMissing { .. }));
        assert_eq!(journal_count(&db), 0);
    }

    #[test]
    fn broken_parent_link_refuses_the_advance() {
        let db = seeded_market();
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &first,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let orphan = ChainAnchor::new(1, 101, hash_of(101), hash_of(50)).unwrap();
        let next = vec![entry(LogCoordinate::new(orphan, tx_hash(9), 0, 0))];
        let err = commit(&db, &unit(&next, 101, CursorAnchor::At(committed))).unwrap_err();
        assert!(matches!(err, ProjectionError::BrokenParentLink { .. }));
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(journal_count(&db), 1);
    }

    #[test]
    fn a_repeated_position_is_refused_with_no_writes() {
        let db = seeded_market();
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &first,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let repeat = vec![entry(position(100, 1, 3, 7))];
        let err = commit(&db, &unit(&repeat, 101, CursorAnchor::At(committed))).unwrap_err();
        assert!(matches!(err, ProjectionError::NotForward { .. }));
        assert_eq!(journal_count(&db), 1);
        assert_eq!(cursor(&db, 1), Some(committed));
    }

    #[test]
    fn invalid_generation_relationship_refuses_the_unit() {
        let db = seeded_market();
        let at = position(100, 1, 3, 7);
        let later = GenerationRecord {
            entity: "contract{market=1,role=pool}".to_string(),
            activation: GenerationActivation::Event(position(101, 1, 3, 7)),
            revision: Some(1),
        };
        let entries = vec![DeltaJournalEntry {
            position: at,
            generation: Some(later),
            encoding: "delta".to_string(),
        }];
        let err = commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::GenerationNotForward { .. }));
        assert_eq!(journal_count(&db), 0);
        assert_eq!(generation_count(&db), 0);
    }

    #[test]
    fn a_seeded_generation_is_journaled_once_and_read_back() {
        let db = seeded_market();
        let generation = GenerationRecord {
            entity: "scaled-token{asset=2,leg=collateral}".to_string(),
            activation: GenerationActivation::Seed,
            revision: Some(1),
        };
        let entries = vec![
            DeltaJournalEntry {
                position: position(100, 1, 3, 7),
                generation: Some(generation.clone()),
                encoding: "a".to_string(),
            },
            DeltaJournalEntry {
                position: position(100, 1, 3, 9),
                generation: Some(generation.clone()),
                encoding: "b".to_string(),
            },
        ];
        commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap();
        assert_eq!(generation_count(&db), 1);
        let journal = read_journal_on_conn(&db.lock(), 1).unwrap();
        assert_eq!(journal[0].generation.as_ref(), Some(&generation));
        assert_eq!(journal[1].generation.as_ref(), Some(&generation));
    }

    #[test]
    fn an_event_activated_generation_must_be_readable_at_its_delta() {
        let db = seeded_market();
        let generation = GenerationRecord {
            entity: "contract{market=1,role=pool}".to_string(),
            activation: GenerationActivation::Event(position(100, 2, 2, 3)),
            revision: Some(2),
        };
        let entries = vec![DeltaJournalEntry {
            position: position(100, 1, 3, 7),
            generation: Some(generation.clone()),
            encoding: "a".to_string(),
        }];
        commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap();
        let journal = read_journal_on_conn(&db.lock(), 1).unwrap();
        assert_eq!(journal[0].generation.as_ref(), Some(&generation));
    }

    #[test]
    fn block_identity_conflict_refuses_the_unit() {
        let db = seeded_market();
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &first,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let renumbered = ChainAnchor::new(1, 102, hash_of(100), hash_of(101)).unwrap();
        let next = vec![entry(LogCoordinate::new(renumbered, tx_hash(9), 0, 0))];
        let err = commit(&db, &unit(&next, 101, CursorAnchor::At(committed))).unwrap_err();
        assert!(matches!(err, ProjectionError::BlockIdentityConflict { .. }));
        assert_eq!(journal_count(&db), 1);
    }

    #[test]
    fn a_range_claim_that_does_not_advance_rolls_the_unit_back() {
        let db = seeded_market();
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &first,
                200,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let next = vec![entry(position(101, 2, 0, 0))];
        let err = commit(&db, &unit(&next, 150, CursorAnchor::At(committed))).unwrap_err();
        assert!(matches!(err, ProjectionError::RangeNotForward { .. }));
        assert_eq!(journal_count(&db), 1, "the refused unit journaled nothing");
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(
            anchor_count(&db),
            1,
            "the refused unit's identities rolled back"
        );
    }

    #[test]
    fn journal_and_cursor_roll_back_together() {
        let db = seeded_market();
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();

        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = write_projection_unit_on_conn(
            &tx,
            &unit(
                &first,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            read_journal_on_conn(&tx, 1).unwrap().len(),
            1,
            "the unit's journal landed inside the transaction"
        );

        let stale = vec![entry(position(101, 2, 0, 0))];
        let err =
            write_projection_unit_on_conn(&tx, &unit(&stale, 100, CursorAnchor::At(committed)))
                .unwrap_err();
        assert!(matches!(err, ProjectionError::RangeNotForward { .. }));

        drop(tx);
        drop(conn);
        assert_eq!(journal_count(&db), 0, "the whole unit rolled back");
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(anchor_count(&db), 0);
    }

    #[test]
    fn two_markets_diverge_and_recover_independently() {
        let db = seeded_market();
        add_market(&db, 2, 1, 199);

        let one = vec![entry(position(100, 1, 3, 7))];
        let two = vec![entry(position(200, 5, 1, 2))];
        let committed_one = commit(
            &db,
            &ProjectionUnit {
                market_id: 1,
                ..unit(
                    &one,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();
        let committed_two = commit(
            &db,
            &ProjectionUnit {
                market_id: 2,
                ..unit(
                    &two,
                    200,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(199),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(cursor(&db, 1), Some(committed_one));
        assert_eq!(cursor(&db, 2), Some(committed_two));
        assert_eq!(read_journal_on_conn(&db.lock(), 1).unwrap().len(), 1);
        assert_eq!(read_journal_on_conn(&db.lock(), 2).unwrap().len(), 1);

        let stale = vec![entry(position(201, 6, 0, 0))];
        let mut tampered = committed_two;
        tampered.position = position(200, 6, 1, 2);
        let err = commit(
            &db,
            &ProjectionUnit {
                market_id: 2,
                ..unit(&stale, 201, CursorAnchor::At(tampered))
            },
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::UnexpectedParent { .. }));
        assert_eq!(cursor(&db, 1), Some(committed_one));
        assert_eq!(read_journal_on_conn(&db.lock(), 1).unwrap().len(), 1);
        assert_eq!(read_journal_on_conn(&db.lock(), 2).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_unit_advances_only_the_range_claim() {
        let db = seeded_market();
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &unit(
                &first,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap()
        .unwrap();

        let advanced = commit(&db, &unit(&[], 150, CursorAnchor::At(committed)))
            .unwrap()
            .unwrap();
        assert_eq!(advanced.position, committed.position);
        assert_eq!(advanced.journal_sequence, committed.journal_sequence);
        assert_eq!(advanced.range_end_block, 150);
        assert_eq!(journal_count(&db), 1);

        let unproven = ProjectionUnit {
            market_id: 3,
            ..unit(&[], 400, CursorAnchor::Unchanged)
        };
        add_market(&db, 3, 1, 399);
        assert_eq!(commit(&db, &unproven).unwrap(), None);
        assert_eq!(cursor(&db, 3), None);
    }

    #[test]
    fn a_market_reset_clears_its_cursor_and_journal() {
        let db = seeded_market();
        let entries = vec![DeltaJournalEntry {
            position: position(100, 1, 3, 7),
            generation: Some(GenerationRecord {
                entity: "scaled-token{asset=2,leg=collateral}".to_string(),
                activation: GenerationActivation::Seed,
                revision: Some(1),
            }),
            encoding: "a".to_string(),
        }];
        commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap();

        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, 1, 99).unwrap();
        tx.commit().unwrap();
        drop(conn);

        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
        assert_eq!(generation_count(&db), 0);
    }

    #[test]
    fn identity_references_are_enforced_by_the_schema() {
        let db = seeded_market();
        let conn = db.lock();
        let enforced: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enforced, 1, "the writer connection enforces declared keys");
        let orphan = conn.execute(
            "INSERT INTO aave_v3_delta_journal (market_id, sequence, position_ordinal, \
                 chain_id, block_number, block_hash, parent_hash, tx_hash, tx_index, log_index, \
                 encoding_version, delta_encoding) \
             VALUES (1, 1, 1, 1, 100, ?1, ?2, ?3, 3, 7, 'test/v1', 'delta')",
            params![
                encode_hash(hash_of(100)),
                encode_hash(hash_of(99)),
                encode_hash(tx_hash(1)),
            ],
        );
        assert!(
            orphan.is_err(),
            "a journal row cannot name a block identity the projection never recorded"
        );
    }

    #[test]
    fn the_journal_refuses_a_second_entry_at_one_position() {
        let db = seeded_market();
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_block_anchors (chain_id, block_number, block_hash, parent_hash) \
             VALUES (1, 100, ?1, ?2)",
            params![encode_hash(hash_of(100)), encode_hash(hash_of(99))],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_transaction_anchors (chain_id, tx_hash, block_hash, tx_index) \
             VALUES (1, ?1, ?2, 3)",
            params![encode_hash(tx_hash(1)), encode_hash(hash_of(100))],
        )
        .unwrap();
        let insert = |sequence: i64, ordinal: i64| {
            conn.execute(
                "INSERT INTO aave_v3_delta_journal (market_id, sequence, position_ordinal, \
                     chain_id, block_number, block_hash, parent_hash, tx_hash, tx_index, \
                     log_index, encoding_version, delta_encoding) \
                 VALUES (1, ?1, ?2, 1, 100, ?3, ?4, ?5, 3, 7, 'test/v1', 'delta')",
                params![
                    sequence,
                    ordinal,
                    encode_hash(hash_of(100)),
                    encode_hash(hash_of(99)),
                    encode_hash(tx_hash(1)),
                ],
            )
        };
        insert(1, 1).unwrap();
        assert!(
            insert(2, 1).is_err(),
            "one position ordinal is journaled once per market"
        );
        insert(2, 2).unwrap();
        drop(conn);
        assert_eq!(journal_count(&db), 2);
    }

    #[test]
    fn an_entry_on_another_chain_refuses_the_unit() {
        let db = seeded_market();
        let foreign = ChainAnchor::new(2, 100, hash_of(100), hash_of(99)).unwrap();
        let entries = vec![entry(LogCoordinate::new(foreign, tx_hash(1), 0, 0))];
        let err = commit(
            &db,
            &unit(
                &entries,
                100,
                CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
            ),
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::ChainMismatch { .. }));
        assert_eq!(cursor(&db, 1), None);
    }

    #[test]
    fn a_unit_with_deltas_must_state_its_anchor() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let err = commit(&db, &unit(&entries, 100, CursorAnchor::Unchanged)).unwrap_err();
        assert!(matches!(err, ProjectionError::AnchorRequired { .. }));
        assert_eq!(journal_count(&db), 0);
        assert_eq!(anchor_count(&db), 0);
    }

    #[test]
    fn a_chain_anchor_requires_a_real_parent() {
        let hash = hash_of(5);
        assert!(decode_anchor(1, 5, &encode_hash(hash), "0x00").is_err());
        assert!(decode_anchor(0, 5, &encode_hash(hash), &encode_hash(hash)).is_err());
        let good = decode_anchor(1, 5, &encode_hash(hash), &encode_hash(hash_of(4))).unwrap();
        assert_eq!(good, ChainAnchor::new(1, 5, hash, hash_of(4)).unwrap());
        assert_eq!(good.chain_id(), 1, "the seeded anchor is a real identity");
        assert_eq!(chain_anchor().number(), 99);
    }
}
