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

use alloy::primitives::{B256, U256};
use degenbot_core::canonical_head::ChainAnchor;
use rusqlite::{params, Connection, OptionalExtension};

use crate::connection::DegenbotDb;
use crate::rows::decode::decode_u256;
use crate::schema::ScaledTokenPosition;

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

/// One position's settled state at a checkpoint's exact cursor.
///
/// The relational position rows are mutable and the end-of-chunk cleanup deletes
/// a position once its balance settles to zero, so a checkpoint stores the pair
/// rather than referencing the row: restoring a checkpoint must read the state at
/// that cursor, not whatever the row says after a later unit (ADR-105 D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPositionState {
    /// The position relation the state belongs to.
    pub position: ScaledTokenPosition,
    /// The row id inside that relation.
    pub position_id: i64,
    /// The settled scaled balance.
    pub balance: U256,
    /// The settled scaled index, when the position carries one.
    pub index: Option<U256>,
}

/// One reserve's identity at a checkpoint's cursor: the three address mappings
/// resolution classifies an operation's tokens by, and the two revisions that
/// select its movement rule (ADR-105 D6, D12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointAsset {
    /// `aave_v3_assets.id`.
    pub asset_id: i64,
    /// The underlying token's address mapping.
    pub underlying_address: String,
    /// The aToken's address mapping.
    pub a_token_address: String,
    /// The vToken's address mapping.
    pub v_token_address: String,
    /// The aToken revision the projection reads under.
    pub a_token_revision: u32,
    /// The vToken revision the projection reads under.
    pub v_token_revision: u32,
}

/// One contract endpoint at a checkpoint's cursor: the role's settled address
/// and revision, so a resume can target the same endpoint and read the same
/// replacement generation (ADR-105 D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointContract {
    /// The role name (`POOL`, `POOL_CONFIGURATOR`, ...) — the stable logical key
    /// ADR-105 D6 qualifies by generation.
    pub name: String,
    /// The checksummed endpoint address the role resolves to.
    pub address: String,
    /// The role's revision fact, where it has one.
    pub revision: Option<u32>,
}

/// The chain's GHO row at a checkpoint's cursor: whether the reserve is linked
/// yet (`v_token_id`), and which discount token/strategy the discount path reads
/// (ADR-105 D8). A resume classifies the GHO discount pre-pass from these alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointGho {
    /// `aave_gho_tokens.id`.
    pub gho_id: i64,
    /// The underlying GHO token id.
    pub token_id: i64,
    /// The GHO variable-debt token id; `None` until the reserve is initialized.
    pub v_token_id: Option<i64>,
    /// The checksummed underlying GHO token address.
    pub gho_token_address: String,
    /// The checksummed GHO variable-debt token address (`None` exactly when
    /// `v_token_id` is `None`).
    pub v_token_address: Option<String>,
    /// The checksummed discount-rate-strategy endpoint, when one is set.
    pub discount_rate_strategy: Option<String>,
    /// The checksummed stkAAVE discount token, when the market has one.
    pub discount_token: Option<String>,
}

/// One user's discount state at a checkpoint's cursor: the transaction-start
/// `gho_discount` a pre-pass reads, and the observed stkAAVE balance that decides
/// whether the balance still needs a chain read (ADR-105 D8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointUser {
    /// `aave_v3_users.id`.
    pub user_id: i64,
    /// The checksummed user address.
    pub address: String,
    /// The settled GHO discount percent.
    pub gho_discount: i64,
    /// The observed stkAAVE balance; `None` until the column is observed.
    pub stk_aave_balance: Option<U256>,
}

/// The resume state a checkpoint carries besides the exact cursor it is
/// identified by: the generations the projection read under and the settled
/// state of the positions it can name (ADR-105 D8, D9).
///
/// The checkpoint states no position or journal sequence of its own: the unit
/// writer derives them from the committed cursor, so a checkpoint cannot be
/// written at a position the cursor does not hold.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProjectionCheckpoint<'a> {
    /// The payload meaning's version (owned by the delta vocabulary).
    pub encoding_version: &'a str,
    /// The generation relationships the projection reads under.
    pub generations: &'a [GenerationRecord],
    /// The positions the reducing view held; the writer fills the rest from the
    /// market's own settled rows.
    pub positions: &'a [CheckpointPositionState],
    /// The reserve identities resolution classifies tokens by.
    pub assets: &'a [CheckpointAsset],
    /// The contract endpoints and revisions resolution targets.
    pub contracts: &'a [CheckpointContract],
    /// The chain's GHO row, when the chain has one.
    pub gho: Option<&'a CheckpointGho>,
}

// The per-user family is NOT part of the payload: the checkpoint must carry the
// market's COMPLETE user set, and only the market's own rows can enumerate it
// (a resolution cache holds the chunk's candidate addresses). The unit writer
// reads it inside the same transaction, so the checkpoint states the users'
// settled discount state at the cursor exactly as it states their positions.

/// A market's committed checkpoint, read back for recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCheckpointRow {
    /// The market the checkpoint belongs to.
    pub market_id: i64,
    /// The journal entry the checkpointed cursor terminates at.
    pub journal_sequence: i64,
    /// The exact chain position the checkpoint restores to.
    pub position: LogCoordinate,
    /// The numeric range end the projection covered at that cursor.
    pub range_end_block: u64,
    /// The payload meaning's version the checkpoint states.
    pub encoding_version: String,
    /// The generation relationships the projection read under.
    pub generations: Vec<GenerationRecord>,
    /// The settled per-position state.
    pub positions: Vec<CheckpointPositionState>,
    /// The reserve identities resolution classifies tokens by.
    pub assets: Vec<CheckpointAsset>,
    /// The contract endpoints and revisions resolution targets.
    pub contracts: Vec<CheckpointContract>,
    /// The chain's GHO row, when the chain has one.
    pub gho: Option<CheckpointGho>,
    /// The per-user discount state resolution reads at tx start.
    pub users: Vec<CheckpointUser>,
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
    /// The checkpoint to commit with the unit's journal entries and cursor
    /// advance, if the cadence scheduled one. The checkpoint's own cursor
    /// identity is the unit's, so it cannot disagree with what this unit
    /// commits.
    pub checkpoint: Option<ProjectionCheckpoint<'a>>,
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
    /// A checkpoint was scheduled for a unit that does not advance the market's
    /// exact cursor, so there is no position or journal entry to capture.
    #[error(
        "market {market_id} scheduled a checkpoint for a unit that journals no delta: \
         a checkpoint captures an advancing cursor"
    )]
    CheckpointWithoutCursor {
        /// The market the unit belongs to.
        market_id: i64,
    },
    /// A checkpoint named a position state with a non-zero balance that the
    /// market's own position rows do not hold: a foreign or invented position.
    #[error(
        "market {market_id} checkpointed a non-zero balance for unknown position \
         {position:?} id={position_id}"
    )]
    CheckpointPositionNotInMarket {
        /// The market the unit belongs to.
        market_id: i64,
        /// The position relation the state named.
        position: ScaledTokenPosition,
        /// The row id the state named.
        position_id: i64,
    },
    /// A checkpoint's payload version is empty: a checkpoint must state which
    /// encoding its state means.
    #[error("market {market_id} checkpointed state with an empty encoding version")]
    CheckpointWithoutVersion {
        /// The market the unit belongs to.
        market_id: i64,
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

/// A checkpoint's stored revision column: a non-negative `u32` fact.
fn decode_revision(value: i64) -> Result<u32, ProjectionError> {
    u32::try_from(value)
        .map_err(|_| ProjectionError::Decode(format!("checkpoint revision {value}")))
}

/// The generation relationship a journal or checkpoint row carries, decoded from
/// its raw columns (both readers select the same ten values in the same order).
#[expect(clippy::too_many_arguments)]
fn decode_generation_columns(
    entity: String,
    kind: &str,
    revision: Option<i64>,
    chain_id: Option<i64>,
    block_number: Option<i64>,
    block_hash: Option<&str>,
    parent_hash: Option<&str>,
    tx_hash: Option<&str>,
    tx_index: Option<i64>,
    log_index: Option<i64>,
) -> Result<GenerationRecord, ProjectionError> {
    let activation = match kind {
        "seed" => GenerationActivation::Seed,
        "event" => {
            let (
                Some(chain_id),
                Some(block_number),
                Some(block_hash),
                Some(parent_hash),
                Some(tx_hash),
                Some(tx_index),
                Some(log_index),
            ) = (
                chain_id,
                block_number,
                block_hash,
                parent_hash,
                tx_hash,
                tx_index,
                log_index,
            )
            else {
                return Err(ProjectionError::Decode(format!(
                    "event-activated generation {entity:?} is missing its activation position"
                )));
            };
            let anchor = decode_anchor(chain_id, block_number, block_hash, parent_hash)?;
            GenerationActivation::Event(LogCoordinate::new(
                anchor,
                decode_hash(tx_hash)?,
                u64::try_from(tx_index)
                    .map_err(|_| ProjectionError::Decode("tx index".to_string()))?,
                u64::try_from(log_index)
                    .map_err(|_| ProjectionError::Decode("log index".to_string()))?,
            ))
        }
        other => {
            return Err(ProjectionError::Decode(format!(
                "generation activation kind {other:?}"
            )))
        }
    };
    Ok(GenerationRecord {
        entity,
        activation,
        revision: revision
            .map(|revision| {
                u32::try_from(revision)
                    .map_err(|_| ProjectionError::Decode(format!("revision {revision}")))
            })
            .transpose()?,
    })
}

/// One checkpoint row's eleven columns, in SELECT order.
type CheckpointColumns = (
    i64,
    i64,
    i64,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    String,
);

fn checkpoint_coordinate_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointColumns> {
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
        row.get(10)?,
    ))
}

fn read_checkpoint_generations(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Vec<GenerationRecord>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT g.entity, g.activation_kind, g.revision, g.chain_id, g.block_number, \
                g.block_hash, g.parent_hash, g.tx_hash, g.tx_index, g.log_index \
         FROM aave_v3_checkpoint_generations cg \
         JOIN aave_v3_generations g ON g.id = cg.generation_id AND g.market_id = cg.market_id \
         WHERE cg.checkpoint_id = ?1 \
         ORDER BY g.entity, g.activation_kind, g.revision, g.chain_id, g.block_number, \
                  g.tx_hash, g.log_index",
    )?;
    let mut rows = statement.query(params![checkpoint_id])?;
    let mut generations = Vec::new();
    while let Some(row) = rows.next()? {
        let entity: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let revision: Option<i64> = row.get(2)?;
        let chain: Option<i64> = row.get(3)?;
        let number: Option<i64> = row.get(4)?;
        let block_hash: Option<String> = row.get(5)?;
        let parent_hash: Option<String> = row.get(6)?;
        let tx_hash: Option<String> = row.get(7)?;
        let tx_index: Option<i64> = row.get(8)?;
        let log_index: Option<i64> = row.get(9)?;
        generations.push(decode_generation_columns(
            entity,
            &kind,
            revision,
            chain,
            number,
            block_hash.as_deref(),
            parent_hash.as_deref(),
            tx_hash.as_deref(),
            tx_index,
            log_index,
        )?);
    }
    drop(rows);
    drop(statement);

    Ok(generations)
}

fn read_checkpoint_positions(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Vec<CheckpointPositionState>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT position, position_id, balance, last_index \
         FROM aave_v3_checkpoint_positions WHERE checkpoint_id = ?1 \
         ORDER BY position, position_id",
    )?;
    let mut rows = statement.query(params![checkpoint_id])?;
    let mut positions = Vec::new();
    while let Some(row) = rows.next()? {
        let code: String = row.get(0)?;
        let position = ScaledTokenPosition::from_code(&code)
            .ok_or_else(|| ProjectionError::Decode(format!("position relation {code:?}")))?;
        let position_id: i64 = row.get(1)?;
        let balance: String = row.get(2)?;
        let index: Option<String> = row.get(3)?;
        positions.push(CheckpointPositionState {
            position,
            position_id,
            balance: decode_u256(&balance).map_err(|error| {
                ProjectionError::Decode(format!("checkpoint balance {balance:?}: {error}"))
            })?,
            index: index
                .map(|index| {
                    decode_u256(&index).map_err(|error| {
                        ProjectionError::Decode(format!("checkpoint index {index:?}: {error}"))
                    })
                })
                .transpose()?,
        });
    }
    drop(rows);
    drop(statement);

    Ok(positions)
}

fn read_checkpoint_assets(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Vec<CheckpointAsset>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT asset_id, underlying_address, a_token_address, v_token_address, \
                a_token_revision, v_token_revision \
         FROM aave_v3_checkpoint_assets WHERE checkpoint_id = ?1 ORDER BY asset_id",
    )?;
    let mut rows = statement.query(params![checkpoint_id])?;
    let mut assets = Vec::new();
    while let Some(row) = rows.next()? {
        assets.push(CheckpointAsset {
            asset_id: row.get(0)?,
            underlying_address: row.get(1)?,
            a_token_address: row.get(2)?,
            v_token_address: row.get(3)?,
            a_token_revision: decode_revision(row.get(4)?)?,
            v_token_revision: decode_revision(row.get(5)?)?,
        });
    }
    Ok(assets)
}

fn read_checkpoint_contracts(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Vec<CheckpointContract>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT name, address, revision \
         FROM aave_v3_checkpoint_contracts WHERE checkpoint_id = ?1 ORDER BY name",
    )?;
    let mut rows = statement.query(params![checkpoint_id])?;
    let mut contracts = Vec::new();
    while let Some(row) = rows.next()? {
        let revision: Option<i64> = row.get(2)?;
        contracts.push(CheckpointContract {
            name: row.get(0)?,
            address: row.get(1)?,
            revision: revision.map(decode_revision).transpose()?,
        });
    }
    Ok(contracts)
}

fn read_checkpoint_gho(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Option<CheckpointGho>, ProjectionError> {
    conn.query_row(
        "SELECT gho_id, token_id, v_token_id, gho_token_address, v_token_address, \
                discount_rate_strategy, discount_token \
         FROM aave_v3_checkpoint_gho WHERE checkpoint_id = ?1",
        params![checkpoint_id],
        |row| {
            Ok(CheckpointGho {
                gho_id: row.get(0)?,
                token_id: row.get(1)?,
                v_token_id: row.get(2)?,
                gho_token_address: row.get(3)?,
                v_token_address: row.get(4)?,
                discount_rate_strategy: row.get(5)?,
                discount_token: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(ProjectionError::from)
}

fn read_checkpoint_users(
    conn: &Connection,
    checkpoint_id: i64,
) -> Result<Vec<CheckpointUser>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT user_id, address, gho_discount, stk_aave_balance \
         FROM aave_v3_checkpoint_users WHERE checkpoint_id = ?1 ORDER BY user_id",
    )?;
    let mut rows = statement.query(params![checkpoint_id])?;
    let mut users = Vec::new();
    while let Some(row) = rows.next()? {
        let balance: Option<String> = row.get(3)?;
        users.push(CheckpointUser {
            user_id: row.get(0)?,
            address: row.get(1)?,
            gho_discount: row.get(2)?,
            stk_aave_balance: balance
                .map(|balance| {
                    decode_u256(&balance).map_err(|error| {
                        ProjectionError::Decode(format!("checkpoint stkAAVE balance: {error}"))
                    })
                })
                .transpose()?,
        });
    }
    Ok(users)
}

fn read_checkpoint_row_on_conn(
    conn: &Connection,
    market_id: i64,
    journal_sequence: Option<i64>,
) -> Result<Option<ProjectionCheckpointRow>, ProjectionError> {
    let mut sql = String::from(
        "SELECT id, chain_id, block_number, block_hash, parent_hash, tx_hash, tx_index, \
         log_index, journal_sequence, range_end_block, encoding_version \
         FROM aave_v3_projection_checkpoints WHERE market_id = ?1",
    );
    match journal_sequence {
        Some(_) => sql.push_str(" AND journal_sequence = ?2"),
        None => sql.push_str(" ORDER BY journal_sequence DESC LIMIT 1"),
    }
    let row: Option<CheckpointColumns> = match journal_sequence {
        Some(sequence) => conn
            .query_row(
                &sql,
                params![market_id, sequence],
                checkpoint_coordinate_columns,
            )
            .optional()?,
        None => conn
            .query_row(&sql, params![market_id], checkpoint_coordinate_columns)
            .optional()?,
    };
    let Some((
        id,
        chain_id,
        block_number,
        block_hash,
        parent_hash,
        tx_hash,
        tx_index,
        log_index,
        journal_sequence,
        range_end_block,
        encoding_version,
    )) = row
    else {
        return Ok(None);
    };
    let anchor = decode_anchor(chain_id, block_number, &block_hash, &parent_hash)?;
    let position = LogCoordinate::new(
        anchor,
        decode_hash(&tx_hash)?,
        u64::try_from(tx_index)
            .map_err(|_| ProjectionError::Decode(format!("tx index {tx_index}")))?,
        u64::try_from(log_index)
            .map_err(|_| ProjectionError::Decode(format!("log index {log_index}")))?,
    );

    let generations = read_checkpoint_generations(conn, id)?;
    let positions = read_checkpoint_positions(conn, id)?;
    let assets = read_checkpoint_assets(conn, id)?;
    let contracts = read_checkpoint_contracts(conn, id)?;
    let gho = read_checkpoint_gho(conn, id)?;
    let users = read_checkpoint_users(conn, id)?;

    Ok(Some(ProjectionCheckpointRow {
        market_id,
        journal_sequence,
        position,
        range_end_block: u64::try_from(range_end_block)
            .map_err(|_| ProjectionError::Decode(format!("range end {range_end_block}")))?,
        encoding_version,
        generations,
        positions,
        assets,
        contracts,
        gho,
        users,
    }))
}

/// The checkpoint the market committed at `journal_sequence`, if any.
///
/// # Errors
///
/// [`ProjectionError::Sqlite`] on a query failure, and
/// [`ProjectionError::Decode`] when a stored coordinate, generation, or balance
/// is not a value the schema's own writer could have written.
pub fn read_checkpoint_on_conn(
    conn: &Connection,
    market_id: i64,
    journal_sequence: i64,
) -> Result<Option<ProjectionCheckpointRow>, ProjectionError> {
    read_checkpoint_row_on_conn(conn, market_id, Some(journal_sequence))
}

/// The most recent checkpoint the market committed, if any - the entry point a
/// recovery reads before replaying retained deltas forward.
///
/// # Errors
///
/// See [`read_checkpoint_on_conn`].
pub fn read_latest_checkpoint_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<Option<ProjectionCheckpointRow>, ProjectionError> {
    read_checkpoint_row_on_conn(conn, market_id, None)
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
            checkpoint: None,
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
    fn seed_position_tables(db: &DegenbotDb) -> (i64, i64) {
        let conn = db.lock();
        for (id, address) in [(1_i64, "0xu1"), (2, "0xa1"), (3, "0xv1")] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, address],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
                 a_token_revision, v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '1', '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
                 stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (1, 1, '0xuser', 0, 0, NULL, NULL, '0')",
            [],
        )
        .unwrap();
        let collateral =
            DegenbotDb::get_or_create_collateral_position_on_conn(&conn, 1, 1).unwrap();
        let debt = DegenbotDb::get_or_create_debt_position_on_conn(&conn, 1, 1).unwrap();
        conn.execute(
            "UPDATE aave_v3_collateral_positions SET balance = '1000', last_index = '2' \
             WHERE id = ?1",
            params![collateral],
        )
        .unwrap();
        conn.execute(
            "UPDATE aave_v3_debt_positions SET balance = '500', last_index = '3' WHERE id = ?1",
            params![debt],
        )
        .unwrap();
        (collateral, debt)
    }

    fn checkpoint<'a>(
        generations: &'a [GenerationRecord],
        positions: &'a [CheckpointPositionState],
    ) -> ProjectionCheckpoint<'a> {
        ProjectionCheckpoint {
            encoding_version: "checkpoint/v1",
            generations,
            positions,
            assets: &[],
            contracts: &[],
            gho: None,
        }
    }

    fn checkpoint_count(db: &DegenbotDb) -> i64 {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_projection_checkpoints",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn latest_checkpoint(db: &DegenbotDb, market_id: i64) -> Option<ProjectionCheckpointRow> {
        read_latest_checkpoint_on_conn(&db.lock(), market_id).unwrap()
    }

    fn collateral_state(position_id: i64, balance: u64, index: u64) -> CheckpointPositionState {
        CheckpointPositionState {
            position: ScaledTokenPosition::Collateral,
            position_id,
            balance: U256::from(balance),
            index: Some(U256::from(index)),
        }
    }

    #[test]
    fn a_checkpoint_commits_with_its_unit_and_reads_back_at_the_same_cursor() {
        let db = seeded_market();
        let (collateral, debt) = seed_position_tables(&db);
        let generations = vec![GenerationRecord {
            entity: "scaled-token{asset=1,leg=collateral}".to_string(),
            activation: GenerationActivation::Seed,
            revision: Some(1),
        }];
        // The debt row is gone (the end-of-chunk cleanup deletes a settled-zero
        // position), so only the reducing view still holds its state.
        db.lock()
            .execute(
                "DELETE FROM aave_v3_debt_positions WHERE id = ?1",
                params![debt],
            )
            .unwrap();
        let positions = vec![
            collateral_state(collateral, 1_000, 2),
            CheckpointPositionState {
                position: ScaledTokenPosition::Debt,
                position_id: debt,
                balance: U256::ZERO,
                index: Some(U256::from(3)),
            },
        ];
        let entries = vec![entry(position(100, 1, 3, 7)), entry(position(100, 1, 3, 9))];
        let committed = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&generations, &positions)),
                ..unit(
                    &entries,
                    120,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();

        let row = latest_checkpoint(&db, 1).unwrap();
        assert_eq!(row.journal_sequence, committed.journal_sequence);
        assert_eq!(row.position, committed.position);
        assert_eq!(row.range_end_block, 120);
        assert_eq!(row.encoding_version, "checkpoint/v1");
        assert_eq!(row.generations, generations);
        assert_eq!(
            row.positions, positions,
            "rows plus the view's zeroed state"
        );
        assert_eq!(checkpoint_count(&db), 1);
        assert_eq!(
            read_checkpoint_on_conn(&db.lock(), 1, committed.journal_sequence).unwrap(),
            Some(row.clone()),
            "the checkpoint reads back by its own journal sequence"
        );
        assert_eq!(
            read_checkpoint_on_conn(&db.lock(), 1, committed.journal_sequence + 1).unwrap(),
            None
        );
    }

    #[test]
    fn a_checkpointed_unit_records_the_generations_the_projection_reads_under() {
        let db = seeded_market();
        let (collateral, _debt) = seed_position_tables(&db);
        let generation = GenerationRecord {
            entity: "contract{market=1,role=pool}".to_string(),
            activation: GenerationActivation::Seed,
            revision: Some(11),
        };
        let entries = vec![entry(position(100, 1, 3, 7))];
        commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(std::slice::from_ref(&generation), &[])),
                ..unit(
                    &entries,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();

        let row = latest_checkpoint(&db, 1).unwrap();
        assert_eq!(row.generations, vec![generation]);
        assert_eq!(
            row.positions,
            vec![
                collateral_state(collateral, 1_000, 2),
                CheckpointPositionState {
                    position: ScaledTokenPosition::Debt,
                    position_id: 1,
                    balance: U256::from(500),
                    index: Some(U256::from(3)),
                }
            ],
            "the market's own settled rows fill the checkpoint"
        );
    }

    #[test]
    fn a_unit_without_a_checkpoint_still_journals_every_entry() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7)), entry(position(100, 1, 3, 9))];
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

        assert_eq!(
            journal_count(&db),
            2,
            "the journal never depends on the cadence"
        );
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(checkpoint_count(&db), 0);
        assert_eq!(latest_checkpoint(&db, 1), None);
    }

    #[test]
    fn a_checkpoint_and_the_journal_agree_at_the_same_cursor() {
        let db = seeded_market();
        let (_collateral, debt) = seed_position_tables(&db);
        let at = position(100, 1, 3, 7);
        let activation = GenerationRecord {
            entity: "contract{market=1,role=pool}".to_string(),
            activation: GenerationActivation::Event(position(100, 2, 2, 3)),
            revision: Some(2),
        };
        let first = vec![DeltaJournalEntry {
            position: at,
            generation: Some(activation.clone()),
            encoding: "a".to_string(),
        }];
        let committed = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(std::slice::from_ref(&activation), &[])),
                ..unit(
                    &first,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();

        // A later unit advances the journal without a scheduled checkpoint.
        let second = vec![entry(position(101, 3, 0, 1))];
        commit(&db, &unit(&second, 101, CursorAnchor::At(committed))).unwrap();

        let checkpointed = latest_checkpoint(&db, 1).unwrap();
        assert_eq!(
            checkpointed.journal_sequence, 1,
            "the last scheduled checkpoint"
        );
        let journal = read_journal_on_conn(&db.lock(), 1).unwrap();
        assert_eq!(journal.len(), 2, "every unit journaled its entries");
        let anchored = &journal[usize::try_from(checkpointed.journal_sequence).unwrap() - 1];
        assert_eq!(anchored.sequence, checkpointed.journal_sequence);
        assert_eq!(
            anchored.position, checkpointed.position,
            "one cursor, one position"
        );
        assert_eq!(
            anchored.generation.as_ref(),
            checkpointed.generations.first(),
            "the checkpoint names a generation the journal at its cursor names"
        );
        assert!(
            checkpointed
                .positions
                .iter()
                .any(|state| state.position_id == debt),
            "the checkpoint carries the market's settled position state"
        );
    }

    #[test]
    fn two_markets_checkpoint_independently() {
        let db = seeded_market();
        add_market(&db, 2, 1, 199);
        let generations = vec![GenerationRecord {
            entity: "scaled-token{asset=1,leg=collateral}".to_string(),
            activation: GenerationActivation::Seed,
            revision: Some(1),
        }];
        let one = vec![entry(position(100, 1, 3, 7))];
        commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&generations, &[])),
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
        let two = vec![entry(position(200, 5, 1, 2))];
        commit(
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

        assert!(latest_checkpoint(&db, 1).is_some());
        assert_eq!(
            latest_checkpoint(&db, 2),
            None,
            "a market without a scheduled checkpoint journals alone"
        );
        assert_eq!(checkpoint_count(&db), 1);
        assert_eq!(read_journal_on_conn(&db.lock(), 2).unwrap().len(), 1);
    }

    #[test]
    fn a_checkpoint_failure_rolls_back_the_journal_and_the_cursor() {
        let db = seeded_market();
        let (_collateral, _debt) = seed_position_tables(&db);
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();

        let entries = vec![entry(position(100, 1, 3, 7))];
        let foreign = vec![collateral_state(999, 5, 1)];
        let err = write_projection_unit_on_conn(
            &tx,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&[], &foreign)),
                ..unit(
                    &entries,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                ProjectionError::CheckpointPositionNotInMarket {
                    position_id: 999,
                    ..
                }
            ),
            "the refusal names the foreign position, got {err:?}"
        );
        assert_eq!(
            read_journal_on_conn(&tx, 1).unwrap().len(),
            1,
            "the journal landed inside the transaction before the checkpoint refused"
        );

        drop(tx);
        drop(conn);
        assert_eq!(journal_count(&db), 0, "the whole unit rolled back");
        assert_eq!(cursor(&db, 1), None);
        assert_eq!(checkpoint_count(&db), 0);
        assert_eq!(anchor_count(&db), 0);
        assert_eq!(generation_count(&db), 0);
    }

    #[test]
    fn a_refused_cursor_rolls_back_the_journal_and_the_checkpoint() {
        let db = seeded_market();
        let (_collateral, _debt) = seed_position_tables(&db);
        let first = vec![entry(position(100, 1, 3, 7))];
        let committed = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&[], &[])),
                ..unit(
                    &first,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap()
        .unwrap();

        // The range claim does not advance, so the cursor refuses AFTER the
        // journal and the checkpoint rows were written inside the transaction.
        let next = vec![entry(position(101, 2, 0, 1))];
        let err = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&[], &[])),
                ..unit(&next, 100, CursorAnchor::At(committed))
            },
        )
        .unwrap_err();
        assert!(matches!(err, ProjectionError::RangeNotForward { .. }));

        assert_eq!(
            journal_count(&db),
            1,
            "only the committed unit's entry remains"
        );
        assert_eq!(checkpoint_count(&db), 1);
        assert_eq!(cursor(&db, 1), Some(committed));
        assert_eq!(
            latest_checkpoint(&db, 1).unwrap().journal_sequence,
            committed.journal_sequence
        );
    }

    #[test]
    fn a_checkpoint_without_an_advancing_unit_refuses() {
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

        let err = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&[], &[])),
                ..unit(&[], 150, CursorAnchor::At(committed))
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::CheckpointWithoutCursor { .. }
        ));
        assert_eq!(checkpoint_count(&db), 0);
        assert_eq!(cursor(&db, 1), Some(committed));
    }

    #[test]
    fn a_checkpoint_without_a_payload_version_refuses() {
        let db = seeded_market();
        let entries = vec![entry(position(100, 1, 3, 7))];
        let err = commit(
            &db,
            &ProjectionUnit {
                checkpoint: Some(ProjectionCheckpoint {
                    encoding_version: "",
                    ..checkpoint(&[], &[])
                }),
                ..unit(
                    &entries,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::CheckpointWithoutVersion { .. }
        ));
        assert_eq!(journal_count(&db), 0);
        assert_eq!(checkpoint_count(&db), 0);
    }

    /// The measured storage and write amplification of the initial cadence, and
    /// the bound every future cadence has to meet: a checkpoint costs one row per
    /// market position plus one row per live generation plus the checkpoint row,
    /// never one row per delta.
    ///
    /// Two identical markets (`USERS` positions each) commit the SAME journal in
    /// the same units; only one of them is checkpointed. The comparison is the
    /// measured baseline any cadence tuning re-runs (`--no-capture` prints it):
    /// the checkpoint's row and byte cost against the journal it accompanies.
    /// The measurement's fixed workload: a two-position user set, four
    /// checkpointed units, eight deltas each.
    const USERS: i64 = 200;
    const UNITS: u64 = 4;
    const ENTRIES_PER_UNIT: u64 = 8;

    fn measure_seed(db: &DegenbotDb) {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'market', 1, 99)",
            [],
        )
        .unwrap();
        for (id, address) in [(1_i64, "0xu1"), (2, "0xa1"), (3, "0xv1")] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, address],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
                 a_token_revision, v_token_id, v_token_revision, liquidity_index, \
                 liquidity_rate, borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '1', '0')",
            [],
        )
        .unwrap();
        for user in 1..=USERS {
            conn.execute(
                "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
                     stk_aave_balance, isolation_mode_collateral_asset_id, \
                     isolation_mode_debt) VALUES (?1, 1, ?2, 0, 0, NULL, NULL, '0')",
                params![user, format!("0xuser{user}")],
            )
            .unwrap();
            DegenbotDb::get_or_create_collateral_position_on_conn(&conn, user, 1).unwrap();
            DegenbotDb::get_or_create_debt_position_on_conn(&conn, user, 1).unwrap();
        }
        drop(conn);
    }

    fn measure_commit_units(db: &DegenbotDb, checkpointed: bool) {
        let generations = vec![GenerationRecord {
            entity: "scaled-token{asset=1,leg=collateral}".to_string(),
            activation: GenerationActivation::Seed,
            revision: Some(1),
        }];
        let mut anchor = None;
        for step in 0..UNITS {
            let number = 100 + step;
            let entries: Vec<DeltaJournalEntry> = (1..=ENTRIES_PER_UNIT)
                .map(|log_index| entry(position(number, step + 1, 0, log_index)))
                .collect();
            let cursor_anchor = match anchor {
                None => CursorAnchor::First {
                    replaces_numeric_cursor: Some(99),
                },
                Some(cursor) => CursorAnchor::At(cursor),
            };
            let committed = commit(
                db,
                &ProjectionUnit {
                    checkpoint: checkpointed.then(|| checkpoint(&generations, &[])),
                    ..unit(&entries, number + 100, cursor_anchor)
                },
            )
            .unwrap()
            .unwrap();
            anchor = Some(committed);
        }
    }

    fn measure_pages(db: &DegenbotDb) -> i64 {
        let conn = db.lock();
        let count: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        let size: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        drop(conn);
        count * size
    }

    fn measure_rows(db: &DegenbotDb, table: &str) -> i64 {
        db.lock()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    #[expect(clippy::print_stderr)]
    fn checkpoint_storage_and_write_amplification() {
        let dir = tempfile::tempdir().unwrap();
        let mut measured = Vec::new();
        for checkpointed in [false, true] {
            let path = dir.path().join(if checkpointed {
                "checkpointed.db"
            } else {
                "plain.db"
            });
            let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
            measure_seed(&db);
            measure_commit_units(&db, checkpointed);
            let journal = measure_rows(&db, "aave_v3_delta_journal");
            let checkpoints = measure_rows(&db, "aave_v3_projection_checkpoints");
            let checkpoint_positions = measure_rows(&db, "aave_v3_checkpoint_positions");
            let positions = measure_rows(&db, "aave_v3_collateral_positions")
                + measure_rows(&db, "aave_v3_debt_positions");
            let bytes = measure_pages(&db);
            measured.push((
                checkpointed,
                journal,
                checkpoints,
                checkpoint_positions,
                positions,
                bytes,
            ));
        }
        let (_, plain_journal, plain_checkpoints, plain_cp_positions, positions, plain_bytes) =
            measured[0];
        let (_, journal, checkpoints, checkpoint_positions, _, bytes) = measured[1];

        let basis_points = (bytes - plain_bytes) * 10_000 / plain_bytes;
        eprintln!(
            "checkpoint amplification (USERS={USERS}, UNITS={UNITS}, ENTRIES_PER_UNIT={ENTRIES_PER_UNIT}): journal {plain_journal} rows both arms; checkpoint rows {plain_checkpoints} -> {checkpoints}, checkpoint position rows {plain_cp_positions} -> {checkpoint_positions} for {positions} settled positions; file {plain_bytes} -> {bytes} bytes (+{} bytes, {basis_points} bp over the un-checkpointed arm)",
            bytes - plain_bytes
        );

        assert_eq!(
            journal, plain_journal,
            "the journal is identical with and without the cadence"
        );
        assert_eq!(journal, i64::try_from(UNITS * ENTRIES_PER_UNIT).unwrap());
        assert_eq!(plain_checkpoints, 0);
        assert_eq!(plain_cp_positions, 0);
        assert_eq!(
            checkpoints,
            i64::try_from(UNITS).unwrap(),
            "the initial cadence checkpoints every advancing unit"
        );
        assert_eq!(
            checkpoint_positions,
            positions * checkpoints,
            "a checkpoint costs one position row per settled position, never per delta"
        );
        assert!(
            bytes > plain_bytes,
            "the checkpoint is storage the journal does not pay"
        );
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
            &ProjectionUnit {
                checkpoint: Some(checkpoint(&[], &[])),
                ..unit(
                    &entries,
                    100,
                    CursorAnchor::First {
                        replaces_numeric_cursor: Some(99),
                    },
                )
            },
        )
        .unwrap();
        assert_eq!(
            checkpoint_count(&db),
            1,
            "the reset clears a committed checkpoint"
        );

        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, 1, 99).unwrap();
        tx.commit().unwrap();
        drop(conn);

        assert_eq!(cursor(&db, 1), None);
        assert_eq!(journal_count(&db), 0);
        assert_eq!(generation_count(&db), 0);
        assert_eq!(checkpoint_count(&db), 0);
        assert_eq!(latest_checkpoint(&db, 1), None);
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
